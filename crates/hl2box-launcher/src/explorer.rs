//! File explorer over a [`TreeNode`]: breadcrumb, folder navigation, flat
//! filtered search across the whole tree, and a Lua listing grouped by realm.

use eframe::egui::{self, Color32, RichText};
use hl2box_core::lua::{realm_of, Realm};
use hl2box_core::tree::TreeNode;

use crate::theme::Palette;
use crate::widgets::list_row;

/// Navigation state: where the user is inside the tree, and the name filter.
#[derive(Default)]
pub struct Explorer {
    pub cwd: String,
    pub filter: String,
}

pub struct ExplorerLabels {
    pub root: String,
    pub up: String,
    pub empty: String,
    pub no_match: String,
    pub items: String,
    pub search_hint: String,
}

/// What the user clicked this frame.
pub enum Pick {
    Nothing,
    File(String),
}

const MAX_SEARCH_ROWS: usize = 400;

fn ext_tag(name: &str, pal: &Palette) -> (String, Color32) {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_uppercase()).unwrap_or_else(|| "FILE".into());
    let color = match ext.as_str() {
        "LUA" => pal.blue,
        "VMT" | "VTF" | "VTX" => pal.accent,
        "GMA" => pal.err,
        _ => pal.weak,
    };
    (ext, color)
}

pub fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    root: &TreeNode,
    ex: &mut Explorer,
    labels: &ExplorerLabels,
) -> Pick {
    if root.find(&ex.cwd).map(|n| !n.is_dir).unwrap_or(true) {
        ex.cwd.clear();
    }
    let mut pick = Pick::Nothing;

    // search box
    ui.add(
        egui::TextEdit::singleline(&mut ex.filter)
            .hint_text(&labels.search_hint)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);

    // breadcrumb
    if ex.filter.trim().is_empty() {
        ui.horizontal_wrapped(|ui| {
            if ui.add(egui::Button::new(RichText::new(&labels.root).strong()).frame(false)).clicked() {
                ex.cwd.clear();
            }
            let segs: Vec<String> = ex
                .cwd
                .split('/')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            let mut acc = String::new();
            for seg in &segs {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(seg);
                ui.weak("/");
                if ui.add(egui::Button::new(seg.as_str()).frame(false)).clicked() {
                    ex.cwd = acc.clone();
                }
            }
        });
        ui.add_space(4.0);
    }

    egui::ScrollArea::vertical()
        .id_salt("explorer_rows")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if !ex.filter.trim().is_empty() {
                // flat search across the whole tree
                let q = ex.filter.trim().to_lowercase();
                let hits: Vec<&TreeNode> = root
                    .files()
                    .into_iter()
                    .filter(|f| f.path.to_lowercase().contains(&q))
                    .collect();
                if hits.is_empty() {
                    ui.weak(&labels.no_match);
                }
                for f in hits.iter().take(MAX_SEARCH_ROWS) {
                    let (tag, color) = ext_tag(&f.name, pal);
                    let meta = human_size(f.size);
                    if list_row(ui, pal, &f.path, &meta, Some((&tag, color)), false).clicked() {
                        pick = Pick::File(f.path.clone());
                    }
                }
                if hits.len() > MAX_SEARCH_ROWS {
                    ui.weak(format!("+{}", hits.len() - MAX_SEARCH_ROWS));
                }
                return;
            }

            let Some(node) = root.find(&ex.cwd) else { return };
            if !ex.cwd.is_empty()
                && list_row(ui, pal, &labels.up, "", None, false).clicked()
            {
                ex.cwd = match ex.cwd.rsplit_once('/') {
                    Some((parent, _)) => parent.to_string(),
                    None => String::new(),
                };
            }
            if node.children.is_empty() {
                ui.weak(&labels.empty);
            }
            for child in &node.children {
                if child.is_dir {
                    let meta = format!("{} {}", child.file_count(), labels.items);
                    let resp = list_row(ui, pal, &format!("{}/", child.name), &meta, Some(("DIR", pal.ok)), false);
                    if resp.clicked() {
                        ex.cwd = child.path.clone();
                    }
                } else {
                    let (tag, color) = ext_tag(&child.name, pal);
                    let meta = human_size(child.size);
                    if list_row(ui, pal, &child.name, &meta, Some((&tag, color)), false).clicked() {
                        pick = Pick::File(child.path.clone());
                    }
                }
            }
        });

    pick
}

/// Every Lua file in the tree, grouped by realm. Returns the clicked path.
pub fn lua_listing(ui: &mut egui::Ui, pal: &Palette, root: &TreeNode, empty: &str, realm_label: &dyn Fn(Realm) -> String, count_fmt: &dyn Fn(usize) -> String) -> Option<String> {
    let mut pick = None;
    let files = root.files();
    let lua: Vec<(&TreeNode, Realm)> = files
        .iter()
        .filter_map(|f| realm_of(&f.path).map(|r| (*f, r)))
        .collect();
    if lua.is_empty() {
        ui.weak(empty);
        return None;
    }
    egui::ScrollArea::vertical()
        .id_salt("lua_listing")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for realm in Realm::ALL {
                let group: Vec<&TreeNode> = lua.iter().filter(|(_, r)| *r == realm).map(|(f, _)| *f).collect();
                if group.is_empty() {
                    continue;
                }
                let title = format!("{}  {}", realm_label(realm), count_fmt(group.len()));
                egui::CollapsingHeader::new(RichText::new(title).strong())
                    .id_salt(("lua_realm", realm.key()))
                    .default_open(realm != Realm::Other)
                    .show(ui, |ui| {
                        for f in group {
                            if list_row(ui, pal, &f.path, &human_size(f.size), None, false).clicked() {
                                pick = Some(f.path.clone());
                            }
                        }
                    });
            }
        });
    pick
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0usize;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}
