//! Log viewer: source picker, level filter chips, text search, follow mode,
//! and a virtualised list so a multi-megabyte log stays responsive.

use std::path::PathBuf;

use eframe::egui::{self, RichText, ScrollArea, TextWrapMode};
use hl2box_core::logs::{self, Level, LogSource};

use crate::theme::Palette;
use crate::widgets;

pub struct LogView {
    pub query: String,
    /// Visible levels, indexed like [`Level::ALL`].
    pub shown: [bool; 4],
    pub follow: bool,
    pub auto_refresh: bool,
    pub lines: Vec<String>,
    pub file_len: u64,
    pub modified: Option<String>,
}

impl Default for LogView {
    fn default() -> Self {
        LogView {
            query: String::new(),
            shown: [true; 4],
            follow: true,
            auto_refresh: true,
            lines: Vec::new(),
            file_len: 0,
            modified: None,
        }
    }
}

pub struct LogLabels {
    pub title: String,
    pub auto: String,
    pub refresh: String,
    pub follow: String,
    pub search_hint: String,
    pub open_folder: String,
    pub copy: String,
    pub none: String,
    pub missing: String,
    pub showing: String,
    pub level: [String; 4],
    pub size: String,
    pub modified: String,
}

/// What the page asks the app to do.
pub enum LogAction {
    Nothing,
    Pick(usize),
    Reload,
}

fn level_index(l: Level) -> usize {
    Level::ALL.iter().position(|x| *x == l).unwrap_or(3)
}

fn level_color(pal: &Palette, l: Level) -> egui::Color32 {
    match l {
        Level::Error => pal.err,
        Level::Warn => pal.warn,
        Level::Info => pal.blue,
        Level::Plain => pal.text,
    }
}

/// Re-read the selected source into `view`. Missing files give an empty view.
pub fn load(view: &mut LogView, src: Option<&LogSource>) {
    let Some(src) = src else {
        view.lines.clear();
        view.file_len = 0;
        view.modified = None;
        return;
    };
    view.lines = logs::tail(&src.path, 1024 * 1024);
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
}

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

    // ---- toolbar
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
            if ui.button(&labels.open_folder).clicked() {
                if let Some(src) = sources.get(pick) {
                    if let Some(dir) = src.1.parent() {
                        hl2box_core::addons::open_in_file_manager(dir);
                    }
                }
            }
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let counts = logs::level_counts(&view.lines);
            for (i, level) in Level::ALL.iter().enumerate() {
                let color = level_color(pal, *level);
                let text = format!("{}  {}", labels.level[i], counts[i]);
                let on = view.shown[i];
                // chip fill carries the level colour; text stays readable on both states
                let fill = if on { crate::theme::alpha(color, 90) } else { pal.card };
                let resp = ui.add(egui::Button::new(RichText::new(text).color(pal.text)).fill(fill));
                if resp.clicked() {
                    view.shown[i] = !view.shown[i];
                }
            }
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut view.query)
                    .hint_text(&labels.search_hint)
                    .desired_width(240.0),
            );
            if ui.button(&labels.copy).clicked() {
                let text = filtered(view).map(|l| view.lines[l].as_str()).collect::<Vec<_>>().join("\n");
                ui.ctx().copy_text(text);
            }
        });
    });

    ui.add_space(6.0);

    // ---- status line
    let rows: Vec<usize> = filtered(view).collect();
    ui.horizontal(|ui| {
        let shown = labels.showing.replacen("{{shown}}", &rows.len().to_string(), 1).replacen("{{total}}", &view.lines.len().to_string(), 1);
        ui.weak(shown);
        ui.weak(format!("{} {}", labels.size, crate::explorer::human_size(view.file_len)));
        if let Some(m) = &view.modified {
            ui.weak(format!("{} {m}", labels.modified));
        }
    });
    ui.add_space(4.0);

    // ---- body
    if view.lines.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.weak(&labels.none);
        });
        return action;
    }

    let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 3.0;
    let follow = view.follow;
    ScrollArea::both()
        .id_salt("log_body")
        .auto_shrink([false, false])
        .stick_to_bottom(follow)
        .show_rows(ui, row_h, rows.len(), |ui, range| {
            for r in range {
                let idx = rows[r];
                let line = &view.lines[idx];
                let level = logs::classify(line);
                let text = RichText::new(line).monospace().color(level_color(pal, level));
                ui.add(egui::Label::new(text).wrap_mode(TextWrapMode::Extend).selectable(true));
            }
        });

    action
}

/// Indices of lines that pass both the level chips and the search box.
fn filtered(view: &LogView) -> impl Iterator<Item = usize> + '_ {
    view.lines.iter().enumerate().filter_map(move |(i, line)| {
        let lvl = logs::classify(line);
        if view.shown[level_index(lvl)] && logs::matches(line, &view.query) {
            Some(i)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_combines_level_and_query() {
        let mut v = LogView::default();
        v.lines = vec![
            "error boom".into(),
            "warning meh".into(),
            "plain thing".into(),
        ];
        assert_eq!(filtered(&v).collect::<Vec<_>>(), vec![0, 1, 2]);
        v.shown[0] = false; // hide errors
        assert_eq!(filtered(&v).collect::<Vec<_>>(), vec![1, 2]);
        v.query = "plain".into();
        assert_eq!(filtered(&v).collect::<Vec<_>>(), vec![2]);
    }
}
