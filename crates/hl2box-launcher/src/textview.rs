//! Read-only text viewer: line numbers, search with hit navigation, optional
//! word wrap, copy-all, and Lua syntax colouring.
//!
//! Lua is tokenised line by line. Long comments and long strings (`--[[ ]]`,
//! `[==[ ]==]`) span lines, so each line's starting state is computed once on
//! load and carried forward; rendering a visible row never re-scans the file.

use std::ops::Range;

use eframe::egui::{
    self,
    text::{LayoutJob, TextFormat},
    Align, Color32, FontId, Label, RichText, ScrollArea, Sense, Shape, TextStyle,
    TextWrapMode,
};

use crate::theme::{alpha, Palette};

// --------------------------------------------------------------- lua lexer

/// Lexer state carried from one line to the next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LuaState {
    Code,
    /// Inside `--[==[ ... ]==]` (comment) or `[==[ ... ]==]` (string).
    Block { comment: bool, level: usize },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    Plain,
    Keyword,
    Str,
    Comment,
    Number,
    /// Engine globals every GMod script touches (hook, net, util, ...).
    Engine,
    /// An identifier directly followed by `(`, `{` or a string.
    Call,
    Op,
}

const KEYWORDS: [&str; 22] = [
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

const ENGINE: [&str; 28] = [
    "hook", "net", "util", "player", "ents", "vgui", "surface", "draw", "file", "timer",
    "concommand", "cvars", "Color", "Vector", "Angle", "IsValid", "CLIENT", "SERVER", "MENU_DLL",
    "include", "AddCSLuaFile", "print", "Msg", "MsgN", "GAMEMODE", "game", "physenv", "language",
];

fn long_open_level(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i + 1;
    let mut level = 0;
    while b.get(j) == Some(&b'=') {
        level += 1;
        j += 1;
    }
    (b.get(j) == Some(&b'[')).then_some(level)
}

fn closer(level: usize) -> String {
    format!("]{}]", "=".repeat(level))
}

/// Tokenise one line starting in `state`. Returns the tokens (byte ranges into
/// `line`, always on char boundaries) and the state at the end of the line.
pub fn tokenize_line(line: &str, mut state: LuaState) -> (Vec<(Tok, Range<usize>)>, LuaState) {
    let b = line.as_bytes();
    let mut out: Vec<(Tok, Range<usize>)> = Vec::new();
    let mut i = 0usize;

    if let LuaState::Block { comment, level } = state {
        let kind = if comment { Tok::Comment } else { Tok::Str };
        match line.find(&closer(level)) {
            Some(pos) => {
                let end = pos + closer(level).len();
                out.push((kind, 0..end));
                i = end;
                state = LuaState::Code;
            }
            None => {
                if !line.is_empty() {
                    out.push((kind, 0..line.len()));
                }
                return (out, state);
            }
        }
    }

    while i < b.len() {
        let c = b[i];

        // comments: -- ... or --[==[ ... ]==]
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            if let Some(level) = long_open_level(b, i + 2) {
                let open = i + 2 + level + 2;
                let close = closer(level);
                if let Some(pos) = line.get(open..).and_then(|rest| rest.find(&close)) {
                    let end = open + pos + close.len();
                    out.push((Tok::Comment, i..end));
                    i = end;
                    continue;
                }
                out.push((Tok::Comment, i..b.len()));
                return (out, LuaState::Block { comment: true, level });
            }
            out.push((Tok::Comment, i..b.len()));
            break;
        }

        // long strings: [[ ... ]] / [==[ ... ]==]
        if c == b'[' {
            if let Some(level) = long_open_level(b, i) {
                let open = i + level + 2;
                let close = closer(level);
                if let Some(pos) = line.get(open..).and_then(|rest| rest.find(&close)) {
                    let end = open + pos + close.len();
                    out.push((Tok::Str, i..end));
                    i = end;
                    continue;
                }
                out.push((Tok::Str, i..b.len()));
                return (out, LuaState::Block { comment: false, level });
            }
        }

        // quoted strings, single line, honouring backslash escapes
        if c == b'"' || c == b'\'' {
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == c {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let end = i.min(b.len());
            out.push((Tok::Str, start..end));
            continue;
        }

        // numbers: 42, 0xFF, 1.5e3
        if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
            out.push((Tok::Number, start..i));
            continue;
        }

        // identifiers and keywords
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            let next = b.get(i).copied().unwrap_or(b' ');
            let kind = if KEYWORDS.contains(&word) {
                Tok::Keyword
            } else if ENGINE.contains(&word) {
                Tok::Engine
            } else if next == b'(' || next == b'{' || next == b'"' || next == b'\'' {
                Tok::Call
            } else {
                Tok::Plain
            };
            out.push((kind, start..i));
            continue;
        }

        // whitespace and punctuation, one char at a time (UTF-8 aware)
        let ch_len = line[i..].chars().next().map(|ch| ch.len_utf8()).unwrap_or(1);
        let kind = if c.is_ascii_punctuation() { Tok::Op } else { Tok::Plain };
        out.push((kind, i..i + ch_len));
        i += ch_len;
    }

    (out, state)
}

/// Starting lexer state of every line: `states[n]` is the state entering line n.
pub fn lua_states(lines: &[String]) -> Vec<LuaState> {
    let mut states = Vec::with_capacity(lines.len());
    let mut st = LuaState::Code;
    for l in lines {
        states.push(st);
        st = tokenize_line(l, st).1;
    }
    states
}

fn tok_color(tok: Tok, pal: &Palette) -> Color32 {
    match tok {
        Tok::Plain => pal.text,
        Tok::Keyword => if pal.dark { Color32::from_rgb(199, 146, 234) } else { Color32::from_rgb(120, 60, 180) },
        Tok::Str => if pal.dark { Color32::from_rgb(195, 232, 141) } else { Color32::from_rgb(60, 120, 40) },
        Tok::Comment => pal.weak,
        Tok::Number => if pal.dark { Color32::from_rgb(247, 140, 108) } else { Color32::from_rgb(180, 80, 30) },
        Tok::Engine => pal.blue,
        Tok::Call => if pal.dark { Color32::from_rgb(130, 170, 255) } else { Color32::from_rgb(30, 90, 170) },
        Tok::Op => pal.weak,
    }
}

// ------------------------------------------------------------------ viewer

/// Strings the viewer shows. Built by the caller so the viewer never holds the
/// whole app (and so translations stay out of this module).
pub struct ViewLabels {
    pub search_hint: String,
    pub prev: String,
    pub next: String,
    pub wrap: String,
    pub copy: String,
    pub lines: String,
    pub no_hits: String,
    pub lua: String,
}

pub struct TextView {
    pub title: String,
    pub source: String,
    pub lines: Vec<String>,
    pub lua: bool,
    states: Vec<LuaState>,
    pub wrap: bool,
    pub search: String,
    hits: Vec<usize>,
    hit_pos: usize,
    hits_for: String,
    pending_scroll: Option<usize>,
}

impl TextView {
    pub fn new(title: impl Into<String>, source: impl Into<String>, text: &str, lua: bool) -> Self {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let states = if lua { lua_states(&lines) } else { Vec::new() };
        TextView {
            title: title.into(),
            source: source.into(),
            lines,
            lua,
            states,
            wrap: false,
            search: String::new(),
            hits: Vec::new(),
            hit_pos: 0,
            hits_for: String::new(),
            pending_scroll: None,
        }
    }

    fn refresh_hits(&mut self) {
        if self.hits_for == self.search {
            return;
        }
        self.hits_for = self.search.clone();
        let q = self.search.trim().to_lowercase();
        self.hits = if q.is_empty() {
            Vec::new()
        } else {
            self.lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect()
        };
        self.hit_pos = 0;
        if self.pending_scroll.is_none() {
            if let Some(&first) = self.hits.first() {
                self.pending_scroll = Some(first);
            }
        }
    }

    /// Scroll so that 0-based `line0` is visible on the next frame.
    pub fn jump_to(&mut self, line0: usize) {
        self.pending_scroll = Some(line0);
    }

    fn step_hit(&mut self, forward: bool) {
        if self.hits.is_empty() {
            return;
        }
        let n = self.hits.len();
        self.hit_pos = if forward {
            (self.hit_pos + 1) % n
        } else {
            (self.hit_pos + n - 1) % n
        };
        self.pending_scroll = Some(self.hits[self.hit_pos]);
    }

    fn line_job(&self, idx: usize, pal: &Palette, font: &FontId) -> LayoutJob {
        let line = &self.lines[idx];
        let mut job = LayoutJob::default();
        if !self.lua {
            job.append(line, 0.0, TextFormat::simple(font.clone(), pal.text));
            return job;
        }
        let state = self.states.get(idx).copied().unwrap_or(LuaState::Code);
        let (toks, _) = tokenize_line(line, state);
        let mut cursor = 0usize;
        for (tok, range) in toks {
            if range.start > cursor {
                job.append(&line[cursor..range.start], 0.0, TextFormat::simple(font.clone(), pal.text));
            }
            job.append(&line[range.clone()], 0.0, TextFormat::simple(font.clone(), tok_color(tok, pal)));
            cursor = range.end;
        }
        if cursor < line.len() {
            job.append(&line[cursor..], 0.0, TextFormat::simple(font.clone(), pal.text));
        }
        job
    }

    /// Draw the viewer. Returns the text when the user asked to copy it.
    pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette, labels: &ViewLabels) {
        self.refresh_hits();

        // toolbar
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.title.as_str()).strong());
            if self.lua {
                crate::widgets::badge(ui, labels.lua.clone(), pal.blue);
            }
            ui.weak(format!("{} {}", self.lines.len(), labels.lines));
            if !self.source.is_empty() {
                ui.weak(self.source.as_str());
            }
        });
        ui.horizontal(|ui| {
            let edit = egui::TextEdit::singleline(&mut self.search)
                .hint_text(&labels.search_hint)
                .desired_width(220.0);
            ui.add(edit);
            let have = !self.hits.is_empty();
            if ui.add_enabled(have, egui::Button::new(&labels.prev)).clicked() {
                self.step_hit(false);
            }
            if ui.add_enabled(have, egui::Button::new(&labels.next)).clicked() {
                self.step_hit(true);
            }
            if !self.search.trim().is_empty() {
                if self.hits.is_empty() {
                    ui.colored_label(pal.err, labels.no_hits.as_str());
                } else {
                    ui.label(format!("{}/{}", self.hit_pos + 1, self.hits.len()));
                }
            }
            ui.separator();
            ui.checkbox(&mut self.wrap, &labels.wrap);
            if ui.button(&labels.copy).clicked() {
                ui.ctx().copy_text(self.lines.join("\n"));
            }
        });
        ui.add_space(4.0);

        let font = TextStyle::Monospace.resolve(ui.style());
        let row_h = ui.fonts(|f| f.row_height(&font)) + 2.0;
        let digits = self.lines.len().max(1).to_string().len();
        let gutter_w = digits as f32 * 8.0 + 14.0;
        let hit_set: std::collections::HashSet<usize> = self.hits.iter().copied().collect();
        let current_hit = self.hits.get(self.hit_pos).copied();
        let scroll = self.pending_scroll.take();
        let n = self.lines.len();

        let draw_row = |this: &Self, ui: &mut egui::Ui, idx: usize, row_w: f32| {
            let is_current = current_hit == Some(idx);
            let is_hit = hit_set.contains(&idx);
            let (row_rect, _) = ui.allocate_exact_size(egui::vec2(row_w, row_h), Sense::hover());
            let bg = ui.painter().add(Shape::Noop);
            if is_current {
                ui.painter().set(bg, Shape::rect_filled(row_rect, 0.0, alpha(pal.accent, 70)));
            } else if is_hit {
                ui.painter().set(bg, Shape::rect_filled(row_rect, 0.0, alpha(pal.accent, 30)));
            }
            let num_rect = egui::Rect::from_min_size(row_rect.left_top(), egui::vec2(gutter_w, row_h));
            ui.painter().text(
                egui::pos2(num_rect.right() - 8.0, num_rect.center().y),
                egui::Align2::RIGHT_CENTER,
                format!("{}", idx + 1),
                font.clone(),
                pal.weak,
            );
            let text_rect = egui::Rect::from_min_max(
                egui::pos2(row_rect.left() + gutter_w + 6.0, row_rect.top()),
                row_rect.right_bottom(),
            );
            let job = this.line_job(idx, pal, &font);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect).layout(egui::Layout::left_to_right(Align::Center)));
            child.add(Label::new(job).wrap_mode(TextWrapMode::Extend).selectable(true));
        };

        if self.wrap {
            ScrollArea::vertical()
                .id_salt("textview_wrap")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for idx in 0..n {
                        let job = self.line_job(idx, pal, &font);
                        ui.horizontal_top(|ui| {
                            ui.add_sized(
                                [gutter_w, row_h],
                                Label::new(RichText::new(format!("{}", idx + 1)).color(pal.weak).monospace()),
                            );
                            ui.add(Label::new(job).wrap().selectable(true));
                        });
                    }
                });
            return;
        }

        let mut area = ScrollArea::both()
            .id_salt("textview_body")
            .auto_shrink([false, false]);
        if let Some(idx) = scroll {
            let viewport = ui.available_height();
            let target = (idx as f32 * row_h - viewport * 0.4).max(0.0);
            area = area.vertical_scroll_offset(target);
        }
        area.show_rows(ui, row_h, n, |ui, range| {
            let row_w = ui.available_width().max(800.0);
            for idx in range {
                draw_row(self, ui, idx, row_w);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str, st: LuaState) -> (Vec<Tok>, LuaState) {
        let (t, s) = tokenize_line(line, st);
        (t.into_iter().map(|(k, _)| k).collect(), s)
    }

    #[test]
    fn keywords_strings_numbers() {
        let (toks, _) = kinds("local x = 42", LuaState::Code);
        assert!(toks.contains(&Tok::Keyword));
        assert!(toks.contains(&Tok::Number));
        let (toks, _) = kinds("print('hi')", LuaState::Code);
        assert!(toks.contains(&Tok::Engine));
        assert!(toks.contains(&Tok::Str));
    }

    #[test]
    fn line_comment_to_eol() {
        let (t, st) = tokenize_line("x = 1 -- note", LuaState::Code);
        assert_eq!(st, LuaState::Code);
        let last = t.last().unwrap();
        assert_eq!(last.0, Tok::Comment);
        assert_eq!(&"x = 1 -- note"[last.1.clone()], "-- note");
    }

    #[test]
    fn block_comment_spans_lines() {
        let (_, st) = tokenize_line("--[==[ start", LuaState::Code);
        assert_eq!(st, LuaState::Block { comment: true, level: 2 });
        let (t, st2) = tokenize_line("still inside", st);
        assert_eq!(st2, LuaState::Block { comment: true, level: 2 });
        assert_eq!(t[0].0, Tok::Comment);
        let (_, st3) = tokenize_line("end ]==] local", st2);
        assert_eq!(st3, LuaState::Code);
    }

    #[test]
    fn long_string_spans_lines() {
        let lines = vec!["x = [[abc".to_string(), "def]] y = 1".to_string(), "z".to_string()];
        let states = lua_states(&lines);
        assert_eq!(states[0], LuaState::Code);
        assert_eq!(states[1], LuaState::Block { comment: false, level: 0 });
        assert_eq!(states[2], LuaState::Code);
    }

    #[test]
    fn ranges_are_char_boundaries_for_utf8() {
        let line = "print('中文') -- 注释";
        let (toks, _) = tokenize_line(line, LuaState::Code);
        let mut rebuilt = String::new();
        for (_, r) in &toks {
            rebuilt.push_str(&line[r.clone()]);
        }
        assert_eq!(rebuilt, line);
    }

    #[test]
    fn view_search_hits() {
        let mut v = TextView::new("t", "", "one\ntwo two\nthree", true);
        v.search = "TWO".into();
        v.refresh_hits();
        assert_eq!(v.hits, vec![1]);
        v.search = "".into();
        v.refresh_hits();
        assert!(v.hits.is_empty());
    }
}
