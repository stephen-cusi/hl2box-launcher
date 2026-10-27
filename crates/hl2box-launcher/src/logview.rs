//! Log viewer.
//!
//! The page is a table: time, level, source tag and message, one row per
//! line (or per run of identical lines when repeats are folded). Filters are
//! level chips, source chips built from the `[Tag]`s actually present, and a
//! text box. Rows are painted directly rather than laid out as widgets so a
//! 20k-line log scrolls smoothly.

use std::collections::BTreeSet;
use std::path::PathBuf;

use eframe::egui::{self, Color32, FontId, Sense};
use hl2box_core::logs::{self, Level, LogSource, Run, SourceKind};

use crate::theme::{alpha, mix, Palette};
use crate::widgets;

/// Everything the page remembers between frames.
pub struct LogView {
    pub query: String,
    /// Visible levels, indexed like [`Level::ALL`].
    pub shown: [bool; 4],
    /// Source tags the user has switched off.
    pub hidden_tags: BTreeSet<String>,
    pub collapse: bool,
    pub follow: bool,
    pub auto_refresh: bool,
    pub lines: Vec<String>,
    pub file_len: u64,
    pub modified: Option<String>,
    /// The file is a binary dump; `lines` holds its printable strings.
    pub binary: bool,
    /// Tags present in the file with their line counts, most frequent first.
    pub tags: Vec<(String, usize)>,
    runs: Vec<Run>,
    /// Row cache, rebuilt only when a filter input changes.
    cache: Option<(CacheKey, Vec<Run>)>,
    /// Line index of the row the user last clicked.
    pub selected: Option<usize>,
    /// Row position to scroll to on the next frame (error navigation).
    scroll_to: Option<usize>,
    nav_row: Option<usize>,
    /// Crash analysis of a minidump source, when the source is one.
    pub report: Option<hl2box_core::crashdump::Report>,
    pub report_error: Option<String>,
    /// Show the printable-strings view instead of the crash report.
    pub show_strings: bool,
    /// Printable strings of the current dump, kept so toggling is instant.
    strings: Vec<String>,
    /// File the analysis belongs to; re-analysed only when it changes.
    report_key: Option<(PathBuf, std::time::SystemTime)>,
    /// Analysis running on a worker thread, if any.
    pending: Option<std::sync::mpsc::Receiver<Result<hl2box_core::crashdump::Report, String>>>,
}

#[derive(PartialEq, Eq)]
struct CacheKey {
    query: String,
    shown: [bool; 4],
    hidden: BTreeSet<String>,
    collapse: bool,
    lines: usize,
}

impl Default for LogView {
    fn default() -> Self {
        LogView {
            query: String::new(),
            shown: [true; 4],
            hidden_tags: BTreeSet::new(),
            collapse: true,
            follow: true,
            auto_refresh: true,
            lines: Vec::new(),
            file_len: 0,
            modified: None,
            binary: false,
            tags: Vec::new(),
            runs: Vec::new(),
            cache: None,
            selected: None,
            scroll_to: None,
            nav_row: None,
            report: None,
            report_error: None,
            show_strings: false,
            strings: Vec::new(),
            report_key: None,
            pending: None,
        }
    }
}

impl LogView {
    /// Recompute the derived data after `lines` changed.
    fn rebuild(&mut self) {
        self.runs = logs::collapse_runs(&self.lines);
        let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for l in &self.lines {
            let tag = logs::parse(l).tag.unwrap_or_else(|| "-".into());
            *counts.entry(tag).or_default() += 1;
        }
        let mut tags: Vec<(String, usize)> = counts.into_iter().collect();
        tags.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        self.tags = tags;
        self.cache = None;
        self.selected = None;
        self.nav_row = None;
    }

    /// Rows that pass every filter, in order.
    fn rows(&mut self) -> Vec<Run> {
        let key = CacheKey {
            query: self.query.clone(),
            shown: self.shown,
            hidden: self.hidden_tags.clone(),
            collapse: self.collapse,
            lines: self.lines.len(),
        };
        if let Some((k, rows)) = &self.cache {
            if *k == key {
                return rows.clone();
            }
        }
        let base: Vec<Run> = if self.collapse {
            self.runs.clone()
        } else {
            (0..self.lines.len()).map(|i| Run { first: i, count: 1 }).collect()
        };
        let rows: Vec<Run> = base
            .into_iter()
            .filter(|r| {
                let line = &self.lines[r.first];
                let level_ok = self.shown[Level::ALL.iter().position(|x| *x == logs::classify(line)).unwrap_or(3)];
                let tag = logs::parse(line).tag.unwrap_or_else(|| "-".into());
                level_ok && !self.hidden_tags.contains(&tag) && logs::matches(line, &self.query)
            })
            .collect();
        self.cache = Some((key, rows.clone()));
        rows
    }

    fn time_span(&self) -> Option<(String, String)> {
        let mut times = self.lines.iter().filter_map(|l| logs::parse(l).time);
        let first = times.next()?;
        let last = times.last().unwrap_or_else(|| first.clone());
        Some((first, last))
    }
}

/// Strings for the crash report view.
pub struct CrashLabels {
    pub thread: String,
    pub address: String,
    pub params: String,
    pub modules: String,
    pub pdb_dirs: String,
    pub pdb_loaded: String,
    pub pdb_missing: String,
    pub pdb_mismatch: String,
    pub pdb_unreadable: String,
    pub pdb_noid: String,
    pub pdb_not_loaded: String,
    pub loading: String,
    pub fault: String,
    pub registers: String,
    pub frames: String,
    pub no_frames: String,
    pub notes: String,
    pub toggle_report: String,
    pub toggle_strings: String,
    pub error: String,
    pub no_exception: String,
    pub dir_list: String,
    pub note_av_no_target: String,
    pub note_no_registers: String,
    pub note_no_fault_code: String,
    pub note_no_exception_stream: String,
    pub note_no_verified_frames: String,
}

impl CrashLabels {
    fn note_labels(&self) -> crate::dumpview::DumpLabels {
        crate::dumpview::DumpLabels {
            title: String::new(),
            subtitle: String::new(),
            none: String::new(),
            src_handler: String::new(),
            src_wer: String::new(),
            src_custom: String::new(),
            pick_folder: String::new(),
            none_wer: String::new(),
            none_custom: String::new(),
            open_folder: String::new(),
            refresh: String::new(),
            analysing: String::new(),
            error: String::new(),
            exception: String::new(),
            fault: String::new(),
            registers: String::new(),
            frames: String::new(),
            no_frames: String::new(),
            modules: String::new(),
            notes: String::new(),
            explorer: String::new(),
            explorer_hint: String::new(),
            go: String::new(),
            no_code: String::new(),
            click_jump: String::new(),
            export: String::new(),
            exported: String::new(),
            thread: String::new(),
            params: String::new(),
            unverified: String::new(),
            outside: String::new(),
            no_exc: String::new(),
            pdb_ok: String::new(),
            pdb_missing: String::new(),
            pdb_mismatch: String::new(),
            pdb_unreadable: String::new(),
            pdb_noid: String::new(),
            pdb_not_loaded: String::new(),
            pick_pdb: String::new(),
            pdb_forced_ok: String::new(),
            pdb_forced_mismatch: String::new(),
            symbols: String::new(),
            note_av_no_target: self.note_av_no_target.clone(),
            note_no_registers: self.note_no_registers.clone(),
            note_no_fault_code: self.note_no_fault_code.clone(),
            note_no_exception_stream: self.note_no_exception_stream.clone(),
            note_no_verified_frames: self.note_no_verified_frames.clone(),
        }
    }
}

pub struct LogLabels {
    pub crash: CrashLabels,
    pub title: String,
    pub auto: String,
    pub refresh: String,
    pub follow: String,
    pub collapse: String,
    pub search_hint: String,
    pub open_folder: String,
    pub copy_visible: String,
    pub export: String,
    pub none: String,
    pub missing: String,
    pub showing: String,
    pub summary: String,
    pub span: String,
    pub level: [String; 4],
    pub all: String,
    pub sources: String,
    pub prev_error: String,
    pub next_error: String,
    pub size: String,
    pub modified: String,
    pub binary_banner: String,
    pub col_time: String,
    pub col_level: String,
    pub col_source: String,
    pub col_message: String,
    pub no_rows: String,
    pub copied: String,
    pub exported: String,
}

/// What the page asks the app to do.
pub enum LogAction {
    Nothing,
    Pick(usize),
    Reload,
    /// A short message for the status bar; `true` means success.
    Note(String, bool),
}

fn is_minidump(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mdmp") || e.eq_ignore_ascii_case("dmp"))
        .unwrap_or(false)
}

/// Re-read the selected source. Minidumps get a crash analysis (re-run only
/// when the file changes, since the log page refreshes every second); other
/// binary files fall back to their printable strings.
pub fn load(view: &mut LogView, src: Option<&LogSource>, engine_root: &std::path::Path) {
    let Some(src) = src else {
        view.lines.clear();
        view.file_len = 0;
        view.modified = None;
        view.binary = false;
        view.report = None;
        view.report_error = None;
        view.report_key = None;
        view.rebuild();
        return;
    };
    view.binary = src.kind == SourceKind::BinaryDump;
    if view.binary && is_minidump(&src.path) {
        let mtime = std::fs::metadata(&src.path).and_then(|m| m.modified()).ok();
        if let Some(mtime) = mtime {
            let key = (src.path.clone(), mtime);
            if view.report_key.as_ref() != Some(&key) {
                // Analysis can take a while (PDB reads), so it runs on a worker
                // thread; the page shows "analysing" until the result arrives.
                let (tx, rx) = std::sync::mpsc::channel();
                let path = src.path.clone();
                let engine = engine_root.to_path_buf();
                std::thread::spawn(move || {
                    let _ = tx.send(hl2box_core::crashdump::analyze(&path, &engine));
                });
                view.report = None;
                view.report_error = None;
                view.pending = Some(rx);
                view.strings = logs::read_dump_strings(&src.path, 8 << 20).unwrap_or_default();
                view.report_key = Some(key);
            }
        }
    }
    view.lines = if view.binary {
        if view.report.is_some() && !view.show_strings {
            Vec::new()
        } else if view.binary && !view.strings.is_empty() {
            view.strings.clone()
        } else {
            logs::read_dump_strings(&src.path, 8 << 20).unwrap_or_default()
        }
    } else {
        logs::tail(&src.path, 1 << 20)
    };
    match std::fs::metadata(&src.path) {
        Ok(m) => {
            view.file_len = m.len();
            view.modified = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| crate::state::format_unix(d.as_secs() as i64));
        }
        Err(_) => {
            view.file_len = 0;
            view.modified = None;
        }
    }
    view.rebuild();
}

fn level_color(pal: &Palette, l: Level) -> Color32 {
    match l {
        Level::Error => pal.err,
        Level::Warn => pal.warn,
        Level::Info => pal.blue,
        Level::Plain => pal.text,
    }
}

fn level_tag(l: Level) -> &'static str {
    match l {
        Level::Error => "ERR",
        Level::Warn => "WARN",
        Level::Info => "INFO",
        Level::Plain => "",
    }
}

/// Stable colour per source tag, so `[lua]` and `[HL2SB]` stay recognisable.
fn tag_color(pal: &Palette, tag: &str) -> Color32 {
    let palette = [
        pal.blue,
        pal.ok,
        pal.accent,
        mix(pal.blue, pal.ok, 0.5),
        mix(pal.accent, pal.err, 0.5),
        mix(pal.blue, pal.err, 0.5),
        mix(pal.ok, pal.warn, 0.5),
    ];
    let h: usize = tag.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
    palette[h % palette.len()]
}

/// Column x-offsets from the row's left edge.
const COL_TIME: f32 = 0.0;
const COL_LEVEL: f32 = 92.0;
const COL_TAG: f32 = 160.0;
const COL_MSG: f32 = 300.0;

pub fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    view: &mut LogView,
    sources: &[(String, PathBuf, bool)],
    pick: usize,
    labels: &LogLabels,
) -> LogAction {
    let mut action = LogAction::Nothing;
    widgets::page_header(ui, pal, &labels.title, None);

    // collect a finished crash analysis
    if let Some(rx) = &view.pending {
        match rx.try_recv() {
            Ok(Ok(report)) => {
                view.report = Some(report);
                view.report_error = None;
                view.pending = None;
            }
            Ok(Err(e)) => {
                view.report = None;
                view.report_error = Some(e);
                view.pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                view.pending = None;
                view.report_error = Some("analysis worker stopped".into());
            }
        }
    }
    // A crash report has no lines to filter, so the table controls are hidden.
    let report_mode = view.binary
        && !view.show_strings
        && (view.report.is_some() || view.report_error.is_some() || view.pending.is_some());

    // ---- toolbar: source, refresh, export
    widgets::card(ui, pal, |ui| {
        ui.horizontal_wrapped(|ui| {
            let current = sources.get(pick).map(|s| s.0.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("log_source")
                .width(300.0)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (i, (title, _, exists)) in sources.iter().enumerate() {
                        let label = if *exists { title.clone() } else { format!("{title}  -  {}", labels.missing) };
                        if ui.add_enabled(*exists, egui::SelectableLabel::new(i == pick, label)).clicked() {
                            action = LogAction::Pick(i);
                        }
                    }
                });
            if ui.button(&labels.refresh).clicked() {
                action = LogAction::Reload;
            }
            ui.checkbox(&mut view.auto_refresh, &labels.auto);
            ui.checkbox(&mut view.follow, &labels.follow);
            ui.checkbox(&mut view.collapse, &labels.collapse);
            if ui.button(&labels.open_folder).clicked() {
                if let Some(src) = sources.get(pick) {
                    if let Some(dir) = src.1.parent() {
                        hl2box_core::addons::open_in_file_manager(dir);
                    }
                }
            }
            if !report_mode && ui.button(&labels.export).clicked() {
                action = export(view, sources.get(pick), labels);
            }
            if view.report.is_some() || view.report_error.is_some() {
                let text = if view.show_strings { &labels.crash.toggle_report } else { &labels.crash.toggle_strings };
                if ui.button(text).clicked() {
                    view.show_strings = !view.show_strings;
                    view.lines = if view.show_strings { view.strings.clone() } else { Vec::new() };
                    view.rebuild();
                }
            }
        });
    });
    ui.add_space(6.0);

    // ---- filters: levels, sources, text, error navigation
    if !report_mode {
    widgets::card(ui, pal, |ui| {
        ui.horizontal_wrapped(|ui| {
            let counts = logs::level_counts(&view.lines);
            for (i, level) in Level::ALL.iter().enumerate() {
                let color = level_color(pal, *level);
                let on = view.shown[i];
                let fill = if on { alpha(color, 90) } else { pal.card };
                let text = format!("{}  {}", labels.level[i], counts[i]);
                let resp = ui.add(egui::Button::new(egui::RichText::new(text).color(pal.text)).fill(fill));
                if resp.clicked() {
                    view.shown[i] = !view.shown[i];
                }
            }
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut view.query)
                    .hint_text(&labels.search_hint)
                    .desired_width(220.0),
            );
            if ui.button(&labels.prev_error).clicked() {
                navigate(view, false);
            }
            if ui.button(&labels.next_error).clicked() {
                navigate(view, true);
            }
            if ui.button(&labels.copy_visible).clicked() {
                let text = view
                    .rows()
                    .iter()
                    .map(|r| view.lines[r.first].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                ui.ctx().copy_text(text);
            }
        });
        if !view.tags.is_empty() {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.weak(&labels.sources);
                if ui.add(egui::SelectableLabel::new(view.hidden_tags.is_empty(), &labels.all)).clicked() {
                    view.hidden_tags.clear();
                }
                let tags = view.tags.clone();
                for (tag, n) in tags.iter().take(12) {
                    let on = !view.hidden_tags.contains(tag);
                    let color = if tag == "-" { pal.weak } else { tag_color(pal, tag) };
                    let fill = if on { alpha(color, 70) } else { pal.card };
                    let text = format!("{tag}  {n}");
                    if ui.add(egui::Button::new(egui::RichText::new(text).color(pal.text).small()).fill(fill)).clicked() {
                        if on {
                            view.hidden_tags.insert(tag.clone());
                        } else {
                            view.hidden_tags.remove(tag);
                        }
                    }
                }
            });
        }
    });
    ui.add_space(6.0);
    }

    if report_mode {
        // the report is taller than the page: it needs its own scroll area
        egui::ScrollArea::vertical()
            .id_salt("crash_report")
            .auto_shrink([false, false])
            .show(ui, |ui| crash_view(ui, pal, view, &labels.crash));
        return action;
    }

    // ---- summary line
    let rows = view.rows();
    let (err, warn) = {
        let c = logs::level_counts(&view.lines);
        (c[0], c[1])
    };
    let sessions = view.lines.iter().filter(|l| logs::is_session(l)).count();
    let summary = labels
        .summary
        .replace("{{total}}", &view.lines.len().to_string())
        .replace("{{err}}", &err.to_string())
        .replace("{{warn}}", &warn.to_string())
        .replace("{{sessions}}", &sessions.to_string());
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(summary).strong());
        if let Some((a, b)) = view.time_span() {
            ui.weak(labels.span.replace("{{a}}", &a).replace("{{b}}", &b));
        }
        ui.weak(labels.showing.replace("{{shown}}", &rows.len().to_string()));
        ui.weak(format!("{} {}", labels.size, crate::explorer::human_size(view.file_len)));
        if let Some(m) = &view.modified {
            ui.weak(format!("{} {m}", labels.modified));
        }
    });
    if view.binary {
        ui.add_space(4.0);
        widgets::status_text(ui, pal.warn, &labels.binary_banner);
    }
    ui.add_space(4.0);

    if view.lines.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| ui.weak(&labels.none));
        return action;
    }

    // ---- column header
    let font = FontId::monospace(12.5);
    let row_h = ui.fonts(|f| f.row_height(&font)) + 6.0;
    let head = [
        (COL_TIME, &labels.col_time),
        (COL_LEVEL, &labels.col_level),
        (COL_TAG, &labels.col_source),
        (COL_MSG, &labels.col_message),
    ];
    let (hdr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), Sense::hover());
    ui.painter().rect_filled(hdr, 4.0, pal.card);
    for (x, text) in head {
        ui.painter().text(
            egui::pos2(hdr.left() + 8.0 + x, hdr.center().y),
            egui::Align2::LEFT_CENTER,
            text.as_str(),
            FontId::proportional(12.0),
            pal.weak,
        );
    }

    if rows.is_empty() {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| ui.weak(&labels.no_rows));
        return action;
    }

    // ---- body
    let follow = view.follow && view.scroll_to.is_none();
    let mut area = egui::ScrollArea::vertical()
        .id_salt("log_rows")
        .auto_shrink([false, false])
        .stick_to_bottom(follow);
    if let Some(idx) = view.scroll_to.take() {
        let viewport = ui.available_height();
        area = area.vertical_scroll_offset((idx as f32 * row_h - viewport * 0.35).max(0.0));
        view.follow = false;
    }
    let selected = view.selected;
    let mut clicked_line: Option<usize> = None;
    area.show_rows(ui, row_h, rows.len(), |ui, range| {
        for r in range {
            let run = &rows[r];
            let line = &view.lines[run.first];
            let parsed = logs::parse(line);
            let level = logs::classify(line);
            let session = logs::is_session(line);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), Sense::click());
            let p = ui.painter();
            if selected == Some(run.first) {
                p.rect_filled(rect, 3.0, alpha(pal.accent, 70));
            } else if session {
                p.rect_filled(rect, 3.0, alpha(pal.accent, 28));
            } else if resp.hovered() {
                p.rect_filled(rect, 3.0, pal.card_hi);
            }
            if level == Level::Error {
                p.rect_filled(
                    egui::Rect::from_min_size(rect.left_top(), egui::vec2(3.0, row_h)),
                    1.0,
                    pal.err,
                );
            }
            let y = rect.center().y;
            let x0 = rect.left() + 8.0;
            if let Some(t) = &parsed.time {
                p.text(egui::pos2(x0 + COL_TIME, y), egui::Align2::LEFT_CENTER, t, font.clone(), pal.weak);
            }
            let tag_txt = level_tag(level);
            if !tag_txt.is_empty() {
                let c = level_color(pal, level);
                let g = ui.fonts(|f| f.layout_no_wrap(tag_txt.to_string(), FontId::proportional(11.0), c));
                let w = g.size().x + 10.0;
                let badge = egui::Rect::from_min_size(
                    egui::pos2(x0 + COL_LEVEL, y - 8.0),
                    egui::vec2(w, 16.0),
                );
                p.rect_filled(badge, 4.0, alpha(c, 55));
                p.text(badge.center(), egui::Align2::CENTER_CENTER, tag_txt, FontId::proportional(11.0), c);
            }
            if let Some(tag) = &parsed.tag {
                let c = tag_color(pal, tag);
                p.text(egui::pos2(x0 + COL_TAG, y), egui::Align2::LEFT_CENTER, tag, font.clone(), c);
            }
            let msg_rect = egui::Rect::from_min_max(
                egui::pos2(x0 + COL_MSG, rect.top()),
                egui::pos2(rect.right() - 60.0, rect.bottom()),
            );
            let msg_color = if session { pal.accent } else { level_color(pal, level) };
            let clipped = p.with_clip_rect(msg_rect);
            clipped.text(
                egui::pos2(msg_rect.left(), y),
                egui::Align2::LEFT_CENTER,
                parsed.message,
                font.clone(),
                msg_color,
            );
            if run.count > 1 {
                p.text(
                    egui::pos2(rect.right() - 8.0, y),
                    egui::Align2::RIGHT_CENTER,
                    format!("x{}", run.count),
                    FontId::monospace(12.0),
                    pal.accent,
                );
            }
            let resp = resp.on_hover_text(line.as_str());
            if resp.clicked() {
                clicked_line = Some(run.first);
            }
        }
    });

    if let Some(i) = clicked_line {
        view.selected = Some(i);
        ui.ctx().copy_text(view.lines[i].clone());
        action = LogAction::Note(labels.copied.clone(), true);
    }
    action
}

/// The crash analysis of a minidump: exception, faulting code, modules and
/// their symbol status, registers, verified stack frames and notes.
fn crash_view(ui: &mut egui::Ui, pal: &Palette, view: &LogView, c: &CrashLabels) {
    if view.pending.is_some() && view.report.is_none() {
        widgets::card(ui, pal, |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(&c.loading).color(pal.weak));
            });
        });
        return;
    }
    if let Some(err) = &view.report_error {
        widgets::card(ui, pal, |ui| {
            widgets::status_text(ui, pal.err, &c.error.replace("{{e}}", err));
        });
        return;
    }
    let Some(r) = &view.report else { return };

    widgets::card(ui, pal, |ui| {
        match &r.exception {
            Some(e) => {
                ui.label(
                    egui::RichText::new(format!("{}  (0x{:08X})", e.code_name, e.code))
                        .strong()
                        .size(17.0)
                        .color(pal.err),
                );
                let loc = e
                    .location
                    .as_ref()
                    .map(|l| l.describe())
                    .unwrap_or_else(|| "outside every loaded module".into());
                let params = if e.params.is_empty() {
                    "-".to_string()
                } else {
                    e.params.iter().map(|p| format!("0x{p:X}")).collect::<Vec<_>>().join(", ")
                };
                let rows = vec![
                    (c.thread.clone(), e.thread_id.to_string(), true),
                    (c.address.clone(), format!("0x{:016X}   {loc}", e.address), true),
                    (c.params.clone(), params, true),
                ];
                widgets::kv_grid(ui, pal, "crash_exc", &rows);
            }
            None => {
                ui.label(egui::RichText::new(&c.no_exception).color(pal.weak));
            }
        }
    });
    ui.add_space(8.0);

    if !r.fault_asm.is_empty() {
        widgets::card(ui, pal, |ui| {
            widgets::section(ui, pal, &c.fault);
            egui::ScrollArea::vertical()
                .id_salt("crash_asm")
                .max_height(260.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for row in &r.fault_asm {
                        let color = if row.fault { pal.err } else { pal.text };
                        let text = format!("{:016X}  {}", row.address, row.text);
                        let rich = egui::RichText::new(text).monospace().color(color);
                        ui.add(egui::Label::new(if row.fault { rich.strong() } else { rich }).wrap_mode(egui::TextWrapMode::Extend));
                        if let Some(note) = &row.note {
                            ui.label(
                                egui::RichText::new(format!("                    ; {note}"))
                                    .monospace()
                                    .small()
                                    .color(pal.accent),
                            );
                        }
                    }
                });
        });
        ui.add_space(8.0);
    }

    if let Some(e) = &r.exception {
        widgets::card(ui, pal, |ui| {
            widgets::section(ui, pal, &c.registers);
            egui::Grid::new("crash_regs")
                .num_columns(4)
                .spacing([22.0, 4.0])
                .show(ui, |ui| {
                    for (i, (name, v)) in e.registers.values.iter().enumerate() {
                        ui.label(egui::RichText::new(*name).color(pal.weak).monospace());
                        ui.label(egui::RichText::new(format!("{v:016X}")).monospace());
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                    ui.end_row();
                });
        });
        ui.add_space(8.0);
    }

    widgets::card(ui, pal, |ui| {
        widgets::section(ui, pal, &c.frames);
        if r.frames.is_empty() {
            ui.label(egui::RichText::new(&c.no_frames).color(pal.weak));
        }
        for f in &r.frames {
            ui.monospace(format!("{:>3}  {:016X}  {}", f.slot, f.value, f.location.describe()));
        }
    });
    ui.add_space(8.0);

    widgets::card(ui, pal, |ui| {
        widgets::section(ui, pal, &c.modules);
        ui.label(egui::RichText::new(c.pdb_dirs.replace("{{dirs}}", &c.dir_list)).small().color(pal.weak));
        egui::ScrollArea::vertical()
            .id_salt("crash_modules")
            .max_height(240.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("crash_mods").num_columns(4).spacing([18.0, 3.0]).show(ui, |ui| {
                    for m in &r.modules {
                        ui.monospace(&m.module.name);
                        ui.monospace(format!("{:016X}", m.module.base));
                        ui.monospace(crate::explorer::human_size(m.module.size));
                        use hl2box_core::crashdump::PdbState;
                        let (text, color) = match &m.pdb {
                            PdbState::Loaded(n) => (c.pdb_loaded.replace("{{n}}", &n.to_string()), pal.ok),
                            PdbState::Missing => (c.pdb_missing.clone(), pal.weak),
                            PdbState::Mismatch => (c.pdb_mismatch.clone(), pal.warn),
                            PdbState::Unreadable(e) => (format!("{}: {e}", c.pdb_unreadable), pal.err),
                            PdbState::NoIdentity => (c.pdb_noid.clone(), pal.weak),
                            PdbState::NotLoaded => (c.pdb_not_loaded.clone(), pal.weak),
                        };
                        ui.label(egui::RichText::new(text).color(color).small());
                        ui.end_row();
                    }
                });
            });
    });
    ui.add_space(8.0);

    if !r.notes.is_empty() {
        widgets::card(ui, pal, |ui| {
            widgets::section(ui, pal, &c.notes);
            for n in &r.notes {
                let text = crate::dumpview::note_text(n, &c.note_labels());
                ui.label(egui::RichText::new(text).color(pal.warn));
            }
        });
    }
}

/// Jump to the next (or previous) error row relative to the last jump.
fn navigate(view: &mut LogView, forward: bool) {
    let rows = view.rows();
    let errors: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| logs::classify(&view.lines[r.first]) == Level::Error)
        .map(|(i, _)| i)
        .collect();
    if errors.is_empty() {
        return;
    }
    let cur = view.nav_row;
    let target = match (cur, forward) {
        (None, true) => errors[0],
        (None, false) => *errors.last().unwrap_or(&0),
        (Some(c), true) => errors.iter().copied().find(|&i| i > c).unwrap_or(errors[0]),
        (Some(c), false) => errors.iter().rev().copied().find(|&i| i < c).unwrap_or(*errors.last().unwrap_or(&0)),
    };
    view.nav_row = Some(target);
    view.scroll_to = Some(target);
    view.selected = Some(rows[target].first);
}

/// Write the filtered rows to a file the user picks.
fn export(view: &mut LogView, src: Option<&(String, PathBuf, bool)>, labels: &LogLabels) -> LogAction {
    let rows = view.rows();
    let name = src
        .and_then(|s| s.1.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "log.txt".into());
    let Some(path) = rfd::FileDialog::new()
        .set_file_name(format!("filtered_{name}"))
        .save_file()
    else {
        return LogAction::Nothing;
    };
    let text: String = rows
        .iter()
        .map(|r| view.lines[r.first].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    match std::fs::write(&path, text + "\n") {
        Ok(()) => LogAction::Note(
            labels
                .exported
                .replace("{{n}}", &rows.len().to_string())
                .replace("{{path}}", &path.to_string_lossy()),
            true,
        ),
        Err(e) => LogAction::Note(format!("export failed: {e}"), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_with(lines: &[&str]) -> LogView {
        let mut v = LogView::default();
        v.lines = lines.iter().map(|s| s.to_string()).collect();
        v.rebuild();
        v
    }

    #[test]
    fn collapse_and_filters_compose() {
        let mut v = view_with(&[
            "[1.0] Unknown command \"a\"",
            "[1.1] Unknown command \"a\"",
            "[1.2] Error: boom",
            "[1.3] [lua] hi",
        ]);
        // consecutive repeats fold into one row with a count
        assert_eq!(v.rows().len(), 3);
        v.collapse = false;
        assert_eq!(v.rows().len(), 4);
        // hide the error level
        v.shown[0] = false;
        assert_eq!(v.rows().len(), 3);
        // hide the lua source tag
        v.hidden_tags.insert("lua".into());
        assert_eq!(v.rows().len(), 2);
        v.query = "unknown".into();
        assert_eq!(v.rows().len(), 2);
    }

    #[test]
    fn tags_are_counted_most_frequent_first() {
        let v = view_with(&["[a] x", "[b] y", "[b] z", "plain"]);
        assert_eq!(v.tags[0], ("b".to_string(), 2));
        assert!(v.tags.iter().any(|(t, n)| t == "-" && *n == 1));
    }

    #[test]
    fn navigation_wraps_over_error_rows() {
        let mut v = view_with(&["Error: a", "plain", "Error: b", "plain", "Error: c"]);
        v.collapse = false;
        navigate(&mut v, true);
        assert_eq!(v.scroll_to, Some(0));
        navigate(&mut v, true);
        assert_eq!(v.nav_row, Some(2));
        navigate(&mut v, true);
        assert_eq!(v.nav_row, Some(4));
        navigate(&mut v, true);
        assert_eq!(v.nav_row, Some(0), "wraps to the first error");
        navigate(&mut v, false);
        assert_eq!(v.nav_row, Some(4), "previous wraps to the last error");
    }

    #[test]
    fn span_uses_first_and_last_stamp() {
        let v = view_with(&["plain", "[1.5] a", "L 10/27/2026 - 03:03:34: b", "[9.0] c"]);
        assert_eq!(v.time_span(), Some(("1.5".into(), "9.0".into())));
    }
}
