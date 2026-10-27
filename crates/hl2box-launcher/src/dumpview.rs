//! Crash dump page.
//!
//! Left: every dump in `dumps/` (and `dumps_wer/`), newest first.  Right: the
//! analysis of the selected one - exception, faulting code, call chain,
//! modules - plus an explorer: pick a module, type any address (or click one
//! in the report) and read the code the dump captured there, symbolised from
//! the module's PDB when one matches.

use std::path::PathBuf;

use eframe::egui::{self, Color32};
use hl2box_core::crashdump::{self, AsmRow, ModuleContext, PdbState, Report};

use crate::theme::Palette;
use crate::widgets;

/// Labels, resolved once per frame like the other pages do.
pub struct DumpLabels {
    pub title: String,
    pub subtitle: String,
    pub none: String,
    pub open_folder: String,
    pub refresh: String,
    pub analysing: String,
    pub error: String,
    pub exception: String,
    pub fault: String,
    pub registers: String,
    pub frames: String,
    pub no_frames: String,
    pub modules: String,
    pub notes: String,
    pub explorer: String,
    pub explorer_hint: String,
    pub go: String,
    pub no_code: String,
    pub click_jump: String,
    pub export: String,
    pub exported: String,
    pub thread: String,
    pub params: String,
    pub unverified: String,
    pub outside: String,
    pub no_exc: String,
    pub pdb_ok: String,
    pub pdb_missing: String,
    pub pdb_mismatch: String,
    pub pdb_unreadable: String,
    pub pdb_noid: String,
    pub pdb_not_loaded: String,
    pub pick_pdb: String,
    pub pdb_forced_ok: String,
    pub pdb_forced_mismatch: String,
    pub symbols: String,
    pub note_av_no_target: String,
    pub note_no_registers: String,
    pub note_no_fault_code: String,
    pub note_no_exception_stream: String,
    pub note_no_verified_frames: String,
}

/// Render an analyser note through the page labels.
pub fn note_text(note: &hl2box_core::crashdump::Note, labels: &DumpLabels) -> String {
    let (key, args) = note.key();
    let mut s = match key {
        "dumps.note.av_no_target" => labels.note_av_no_target.clone(),
        "dumps.note.no_registers" => labels.note_no_registers.clone(),
        "dumps.note.no_fault_code" => labels.note_no_fault_code.clone(),
        "dumps.note.no_exception_stream" => labels.note_no_exception_stream.clone(),
        "dumps.note.no_verified_frames" => labels.note_no_verified_frames.clone(),
        other => other.to_string(),
    };
    for (k, v) in args {
        // needle is the literal `{{k}}`; spelled out so format! escaping
        // cannot mangle it again
        let needle = format!("{}{}{}", "{{", k, "}}");
        s = s.replace(&needle, &v);
    }
    s
}

/// Everything the page remembers.
#[derive(Default)]
pub struct DumpView {
    /// (title, path, exists), newest first; rebuilt by [`refresh`].
    pub list: Vec<(String, PathBuf, bool)>,
    pub pick: usize,
    pub report: Option<Report>,
    pub error: Option<String>,
    pub pending: Option<std::sync::mpsc::Receiver<Result<Report, String>>>,
    /// (path, mtime) the running/finished analysis was for.
    pub key: Option<(PathBuf, Option<std::time::SystemTime>)>,
    /// Explorer state: which module row, the address entry text, its rows.
    pub module_pick: usize,
    pub addr_text: String,
    pub count_text: String,
    /// PDB chosen by hand for the selected module (overrides the search).
    pub manual_pdb: Option<PathBuf>,
    /// (header line, rows) of the explorer disassembly.
    pub asm: Option<(String, Vec<AsmRow>)>,
    pub asm_error: Option<String>,
    pub asm_pending: Option<
        std::sync::mpsc::Receiver<
            Result<(ModuleContext, Vec<AsmRow>, Option<crashdump::Registers>), String>,
        >,
    >,
    pub asm_key: Option<(PathBuf, usize, u64, usize)>,
    /// The not-on-fault-path modules were queued for PDB resolution.
    pub pdb_resolved: bool,
    pub pdb_patch: Option<std::sync::mpsc::Receiver<Vec<PdbState>>>,
}

/// Re-scan both dump folders, newest first.
pub fn refresh(view: &mut DumpView, mod_root: &std::path::Path) {
    view.list.clear();
    list_dir(view, mod_root.join("dumps"), "dumps");
    list_dir(view, mod_root.join("dumps_wer"), "dumps_wer");
    if view.pick >= view.list.len() {
        view.pick = 0;
    }
    view.report = None;
    view.error = None;
    view.key = None;
    view.asm = None;
    view.asm_error = None;
    view.asm_key = None;
    view.pdb_resolved = false;
    view.pdb_patch = None;
    view.manual_pdb = None;
}

fn list_dir(view: &mut DumpView, dir: PathBuf, title: &str) {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("mdmp") || e.eq_ignore_ascii_case("dmp"))
                .unwrap_or(false)
        })
        .filter_map(|p| {
            let t = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some((t, p))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in files {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        view.list.push((format!("{title}: {name}"), p, true));
    }
}

/// What the user did this frame.
pub enum Action {
    Nothing,
    Pick(usize),
    Reload,
    /// Disassemble `count` rows of module `module` at `va`.
    Disassemble(usize, u64, usize),
    /// Open a file dialog; the picked path becomes the selected module's PDB.
    PickPdb,
}

fn parse_addr(text: &str) -> Option<u64> {
    let t = text.trim().trim_start_matches("0x").trim_start_matches("0X");
    if t.is_empty() {
        return None;
    }
    u64::from_str_radix(t, 16).ok()
}

fn default_count(text: &str) -> usize {
    text.trim().parse::<usize>().unwrap_or(40).clamp(1, 2000)
}

fn module_index_of(r: &Report, va: u64) -> Option<usize> {
    r.modules
        .iter()
        .position(|m| va >= m.module.base && va < m.module.base + m.module.size)
}

/// One frame of the page. `engine_root` feeds PDB resolution; `note` receives
/// a toast-style message (text, is_ok).
pub fn show(
    ui: &mut egui::Ui,
    pal: &Palette,
    view: &mut DumpView,
    engine_root: &std::path::Path,
    labels: &DumpLabels,
    note: &mut Option<(String, bool)>,
) -> Action {
    let mut action = Action::Nothing;
    widgets::page_header(ui, pal, &labels.title, Some(&labels.subtitle));

    if view.list.is_empty() {
        widgets::card(ui, pal, |ui| {
            ui.label(egui::RichText::new(&labels.none).color(pal.weak));
        });
        return action;
    }

    // collect finished background work
    if let Some(rx) = &view.pending {
        match rx.try_recv() {
            Ok(Ok(r)) => {
                view.report = Some(r);
                view.error = None;
                view.pending = None;
            }
            Ok(Err(e)) => {
                view.error = Some(e);
                view.pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(150));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                view.pending = None;
                view.error = Some("analysis worker stopped".into());
            }
        }
    }
    if let Some(rx) = &view.asm_pending {
        match rx.try_recv() {
            Ok(Ok((ctx, rows, _))) => {
                let header = format!(
                    "{}  base 0x{:016X}   {}{}",
                    ctx.module.name,
                    ctx.module.base,
                    pdb_text(&ctx.pdb, labels),
                    if ctx.symbols.funcs.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "   {}",
                            labels.symbols.replace("{{n}}", &ctx.symbols.funcs.len().to_string())
                        )
                    }
                );
                view.asm = Some((header, rows));
                view.asm_error = None;
                view.asm_pending = None;
            }
            Ok(Err(e)) => {
                view.asm_error = Some(e);
                view.asm_pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(150));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                view.asm_pending = None;
                view.asm_error = Some("disassembly worker stopped".into());
            }
        }
    }

    // ---- left: dump list
    egui::SidePanel::left("dumps_list")
        .resizable(true)
        .default_width(280.0)
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| {
            widgets::card(ui, pal, |ui| {
                ui.horizontal(|ui| {
                    widgets::section(ui, pal, "dumps");
                    if ui.small_button(&labels.refresh).clicked() {
                        action = Action::Reload;
                    }
                });
                egui::ScrollArea::both()
                    .id_salt("dump_files")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, (title, path, exists)) in view.list.iter().enumerate() {
                            // group folder prefix on its own dim line, file name below
                            let (group, name) = title
                                .split_once(": ")
                                .map(|(g, n)| (g.to_string(), n.to_string()))
                                .unwrap_or_else(|| (String::new(), title.clone()));
                            let mut label = egui::RichText::new(name);
                            if i > 0 {
                                if let Some((pg, _, _)) = view.list.get(i - 1) {
                                    let pg = pg.split_once(": ").map(|x| x.0).unwrap_or("");
                                    if pg != group {
                                        label = label.heading().strong();
                                    }
                                }
                            }
                            if !*exists {
                                label = label.weak();
                            }
                            let resp = ui.add(egui::SelectableLabel::new(
                                i == view.pick,
                                label.size(13.0),
                            ));
                            if resp.clicked() {
                                action = Action::Pick(i);
                            }
                            if resp.hovered() {
                                resp.on_hover_text(path.display().to_string());
                            }
                        }
                    });
                if ui.button(&labels.open_folder).clicked() {
                    if let Some((_, path, _)) = view.list.get(view.pick) {
                        if let Some(dir) = path.parent() {
                            hl2box_core::addons::open_in_file_manager(dir);
                        }
                    }
                }
            });
        });

    // ---- right: report + explorer
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("dump_report")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // start (or restart) the analysis when needed
                    if view.report.is_none() && view.error.is_none() && view.pending.is_none() {
                        let want = view
                            .list
                            .get(view.pick)
                            .filter(|(_, _, exists)| *exists)
                            .map(|(_, p, _)| p.clone());
                        if let Some(path) = want {
                            start_analysis(view, &path, engine_root);
                        }
                    }
                    if view.pending.is_some() && view.report.is_none() {
                        widgets::card(ui, pal, |ui| {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(egui::RichText::new(&labels.analysing).color(pal.weak));
                            });
                        });
                        return;
                    }
                    if let Some(err) = &view.error {
                        widgets::card(ui, pal, |ui| {
                            widgets::status_text(ui, pal.err, &labels.error.replace("{{e}}", err));
                        });
                        return;
                    }
                    let Some(r) = &view.report else { return };

                    // (module index, address) pairs the user clicked this frame
                    let mut jumps: Vec<(usize, u64)> = Vec::new();

                    // exception summary
                    widgets::card(ui, pal, |ui| {
                        widgets::section(ui, pal, &labels.exception);
                        match &r.exception {
                            Some(e) => {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{}  (0x{:08X})",
                                        e.code_name, e.code
                                    ))
                                    .strong()
                                    .size(16.0)
                                    .color(pal.err),
                                );
                                egui::ScrollArea::both()
                                    .id_salt("dump_exc_scroll")
                                    .max_height(140.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                egui::Grid::new("dump_exc")
                                    .num_columns(2)
                                    .spacing([16.0, 3.0])
                                    .show(ui, |ui| {
                                        ui.label(egui::RichText::new(&labels.thread).color(pal.weak));
                                        ui.monospace(e.thread_id.to_string());
                                        ui.end_row();
                                        ui.label(egui::RichText::new("address").color(pal.weak));
                                        let mut loc = e
                                            .location
                                            .as_ref()
                                            .map(|l| l.describe())
                                            .unwrap_or_else(|| {
                                                labels.outside.clone()
                                            });
                                        if let Some(src) =
                                            e.location.as_ref().and_then(|l| l.line_desc())
                                        {
                                            loc.push_str(&format!("  [{}]", src));
                                        }
                                        let resp = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(format!(
                                                    "0x{:016X}  {loc}",
                                                    e.address
                                                ))
                                                .monospace(),
                                            )
                                            .sense(egui::Sense::click()),
                                        );
                                        if resp.clicked() {
                                            if let Some(mi) = module_index_of(r, e.address) {
                                                jumps.push((mi, e.address));
                                            }
                                        }
                                        ui.end_row();
                                        if !e.params.is_empty() {
                                            ui.label(
                                                egui::RichText::new(&labels.params).color(pal.weak),
                                            );
                                            ui.monospace(
                                                e.params
                                                    .iter()
                                                    .map(|p| format!("0x{p:X}"))
                                                    .collect::<Vec<_>>()
                                                    .join(", "),
                                            );
                                            ui.end_row();
                                        }
                                    });
                                });
                            }
                            None => {
                                ui.label(
                                    egui::RichText::new(&labels.no_exc).color(pal.weak),
                                );
                            }
                        }
                    });
                    ui.add_space(8.0);

                    // faulting code
                    if !r.fault_asm.is_empty() {
                        widgets::card(ui, pal, |ui| {
                            widgets::section(ui, pal, &labels.fault);
                            collect_asm(ui, pal, "fault", &r.fault_asm, &mut jumps, r);
                        });
                        ui.add_space(8.0);
                    }

                    // registers
                    if let Some(e) = &r.exception {
                        widgets::card(ui, pal, |ui| {
                            widgets::section(ui, pal, &labels.registers);
                            egui::Grid::new("dump_regs")
                                .num_columns(4)
                                .spacing([20.0, 3.0])
                                .show(ui, |ui| {
                                    for (i, (name, v)) in e.registers.values.iter().enumerate() {
                                        ui.label(
                                            egui::RichText::new(*name).color(pal.weak).monospace(),
                                        );
                                        let resp = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(format!("{v:016X}"))
                                                    .monospace(),
                                            )
                                            .sense(egui::Sense::click()),
                                        );
                                        if resp.clicked() {
                                            if let Some(mi) = module_index_of(r, *v) {
                                                jumps.push((mi, *v));
                                            }
                                        }
                                        if i % 2 == 1 {
                                            ui.end_row();
                                        }
                                    }
                                    ui.end_row();
                                });
                        });
                        ui.add_space(8.0);
                    }

                    // call chain
                    widgets::card(ui, pal, |ui| {
                        widgets::section(ui, pal, &labels.frames);
                        if r.frames.is_empty() {
                            ui.label(egui::RichText::new(&labels.no_frames).color(pal.weak));
                        }
                        egui::ScrollArea::both()
                            .id_salt("dump_frames_scroll")
                            .max_height(300.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                egui::Grid::new("dump_frames")
                                    .num_columns(3)
                                    .spacing([18.0, 2.0])
                                    .show(ui, |ui| {
                                        for f in &r.frames {
                                            frames_row(ui, f, r, &mut jumps);
                                        }
                                    });
                            });
                        if r.unverified_slots > 0 {
                            ui.label(
                                egui::RichText::new(
                                    labels.unverified.replace("{{n}}", &r.unverified_slots.to_string()),
                                )
                                .small()
                                .color(pal.weak),
                            );
                        }
                    });
                    ui.add_space(8.0);

                    // modules
                    widgets::card(ui, pal, |ui| {
                        widgets::section(ui, pal, &labels.modules);
                        egui::ScrollArea::both()
                            .id_salt("dump_mods")
                            .max_height(220.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                egui::Grid::new("dump_mods_grid")
                                    .num_columns(5)
                                    .spacing([16.0, 2.0])
                                    .show(ui, |ui| {
                                        for (i, m) in r.modules.iter().enumerate() {
                                            if ui
                                                .add(egui::SelectableLabel::new(
                                                    view.module_pick == i,
                                                    egui::RichText::new(&m.module.name)
                                                        .monospace(),
                                                ))
                                                .clicked()
                                            {
                                                view.module_pick = i;
                                            }
                                            ui.monospace(format!("0x{:016X}", m.module.base))
                                                .on_hover_text(m.module.path.clone());
                                            ui.monospace(crate::explorer::human_size(
                                                m.module.size,
                                            ));
                                            let selected = view.module_pick == i;
                                            let (text, color) = pdb_text_row(&m.pdb, pal, labels);
                                            let text = if selected && view.manual_pdb.is_some() {
                                                match &m.pdb {
                                                    PdbState::Mismatch => labels.pdb_forced_mismatch.clone(),
                                                    PdbState::Loaded(n) => labels.pdb_forced_ok.replace("{{n}}", &n.to_string()),
                                                    _ => text,
                                                }
                                            } else {
                                                text
                                            };
                                            ui.label(
                                                egui::RichText::new(text).color(color).small(),
                                            );
                                            ui.end_row();
                                        }
                                    });
                            });
                    });
                    ui.add_space(8.0);

                    // notes
                    if !r.notes.is_empty() {
                        widgets::card(ui, pal, |ui| {
                            widgets::section(ui, pal, &labels.notes);
                            for n in &r.notes {
                                let text = note_text(n, labels);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(text).color(pal.warn),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Wrap),
                                );
                            }
                        });
                        ui.add_space(8.0);
                    }

                    // ---- explorer
                    widgets::card(ui, pal, |ui| {
                        widgets::section(ui, pal, &labels.explorer);
                        ui.label(
                            egui::RichText::new(&labels.explorer_hint)
                                .small()
                                .color(pal.weak),
                        );
                        ui.add_space(4.0);
                        let base = r
                            .modules
                            .get(view.module_pick)
                            .map(|m| m.module.base)
                            .unwrap_or(0);
                        let module_name = r
                            .modules
                            .get(view.module_pick)
                            .map(|m| m.module.name.clone())
                            .unwrap_or_default();
                        let addr_focus =
                            ui.ctx().memory(|m| m.has_focus(egui::Id::new("dump_addr")));
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&module_name)
                                    .monospace()
                                    .color(pal.blue),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut view.addr_text)
                                    .id(egui::Id::new("dump_addr"))
                                    .hint_text("RVA or VA, hex")
                                    .desired_width(170.0),
                            );
                            if view.count_text.is_empty() {
                                view.count_text = "40".into();
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut view.count_text)
                                    .hint_text("rows")
                                    .desired_width(48.0),
                            );
                            if ui.button(&labels.pick_pdb).clicked() {
                                action = Action::PickPdb;
                            }
                            if let Some(pdb) = &view.manual_pdb {
                                let name = pdb
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                ui.label(egui::RichText::new(name).small().color(pal.blue))
                                    .on_hover_text(pdb.display().to_string());
                            }
                            let go = ui.button(&labels.go).clicked();
                            let enter = addr_focus && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if go || enter {
                                if let Some(addr) = parse_addr(&view.addr_text) {
                                    // below the base means RVA
                                    let va = if addr >= base { addr } else { base + addr };
                                    action = Action::Disassemble(
                                        view.module_pick,
                                        va,
                                        default_count(&view.count_text),
                                    );
                                }
                            }
                        });
                        if let Some(err) = &view.asm_error {
                            widgets::status_text(ui, pal.err, err);
                        }
                        if view.asm_pending.is_some() && view.asm.is_none() {
                            ui.spinner();
                        }
                        if let Some((header, rows)) = &view.asm {
                            ui.label(egui::RichText::new(header).small().color(pal.weak));
                            collect_asm(ui, pal, "explore", rows, &mut jumps, r);
                        } else {
                            ui.label(
                                egui::RichText::new(&labels.no_code).small().color(pal.weak),
                            );
                        }
                        ui.label(
                            egui::RichText::new(&labels.click_jump).small().color(pal.weak),
                        );
                    });

                    // apply the first jump collected this frame
                    if let Some((mi, va)) = jumps.first().copied() {
                        view.module_pick = mi;
                        view.addr_text = format!("{va:X}");
                        action = Action::Disassemble(mi, va, default_count(&view.count_text));
                    }

                    // export the whole report as text next to the dump
                    ui.add_space(4.0);
                    if ui.button(&labels.export).clicked() {
                        let text = report_text(r, labels);
                        let path =
                            std::path::Path::new(&r.dump_path).with_extension("report.txt");
                        match std::fs::write(&path, text) {
                            Ok(_) => {
                                *note = Some((
                                    format!("{}  {}", labels.exported, path.display()),
                                    true,
                                ));
                            }
                            Err(e) => {
                                *note = Some((format!("{}: {e}", labels.error), false));
                            }
                        }
                    }
                });
        });
    action
}

fn frames_row(ui: &mut egui::Ui, f: &hl2box_core::crashdump::Frame, r: &Report, jumps: &mut Vec<(usize, u64)>) {
    ui.monospace(format!("{:>3}", f.slot));
    let resp = ui.add(
        egui::Label::new(egui::RichText::new(format!("0x{:016X}", f.value)).monospace())
            .sense(egui::Sense::click()),
    );
    if resp.clicked() {
        if let Some(mi) = module_index_of(r, f.value) {
            jumps.push((mi, f.value));
        }
    }
    let mut loc = f.location.describe();
    if let Some(src) = f.location.line_desc() {
        loc.push_str(&format!("  [{}]", src));
    }
    ui.monospace(loc);
    ui.end_row();
}

/// The asm table; clicking an address adds (module, address) to `jumps`.
fn collect_asm(
    ui: &mut egui::Ui,
    pal: &Palette,
    id: &str,
    rows: &[AsmRow],
    jumps: &mut Vec<(usize, u64)>,
    r: &Report,
) {
    egui::ScrollArea::both()
        .id_salt(format!("asm_{id}"))
        .max_height(if id == "fault" { 240.0 } else { 420.0 })
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Grid::new(format!("asm_grid_{id}"))
                .num_columns(2)
                .spacing([14.0, 1.0])
                .show(ui, |ui| {
                    for row in rows {
                        let color = if row.fault { pal.err } else { pal.text };
                        let mut line = format!("{:016X}  {}", row.address, row.text);
                        if let Some(sym) = &row.symbol {
                            line.push_str(&format!("   ; {}", sym));
                        }
                        if let Some(src) = &row.source {
                            line.push_str(&format!("  [{}]", src));
                        }
                        let resp = ui.add(
                            egui::Label::new(
                                egui::RichText::new(line).monospace().color(color),
                            )
                            .sense(egui::Sense::click()),
                        );
                        if resp.clicked() {
                            if let Some(mi) = module_index_of(r, row.address) {
                                jumps.push((mi, row.address));
                            }
                        }
                        if let Some(note) = &row.note {
                            ui.label(
                                egui::RichText::new(format!("; {note}"))
                                    .monospace()
                                    .small()
                                    .color(pal.accent),
                            );
                        }
                        ui.end_row();
                    }
                });
        });
}

fn pdb_text(state: &PdbState, labels: &DumpLabels) -> String {
    match state {
        PdbState::Loaded(n) => labels.pdb_ok.replace("{{n}}", &n.to_string()),
        PdbState::Missing => labels.pdb_missing.clone(),
        PdbState::Mismatch => labels.pdb_mismatch.clone(),
        PdbState::Unreadable(e) => labels.pdb_unreadable.replace("{{e}}", e),
        PdbState::NoIdentity => labels.pdb_noid.clone(),
        PdbState::NotLoaded => labels.pdb_not_loaded.clone(),
    }
}

fn pdb_text_row(state: &PdbState, pal: &Palette, labels: &DumpLabels) -> (String, Color32) {
    match state {
        PdbState::Loaded(n) => (labels.pdb_ok.replace("{{n}}", &n.to_string()), pal.ok),
        PdbState::Missing => (labels.pdb_missing.clone(), pal.weak),
        PdbState::Mismatch => (labels.pdb_mismatch.clone(), pal.warn),
        PdbState::Unreadable(e) => (labels.pdb_unreadable.replace("{{e}}", e), pal.err),
        PdbState::NoIdentity => (labels.pdb_noid.clone(), pal.weak),
        PdbState::NotLoaded => (labels.pdb_not_loaded.clone(), pal.weak),
    }
}

/// Plain-text rendering of the whole report, written next to the dump.
fn report_text(r: &Report, labels: &DumpLabels) -> String {
    let mut out = String::new();
    out.push_str(&format!("dump: {}\nsize: {} bytes\n\n", r.dump_path, r.size));
    if let Some(e) = &r.exception {
        out.push_str(&format!(
            "exception {} (0x{:08X})\n  thread: {}\n  address: 0x{:016X}  {}\n  params: {}\n\n",
            e.code_name,
            e.code,
            e.thread_id,
            e.address,
            {
                let mut s = e
                    .location
                    .as_ref()
                    .map(|l| l.describe())
                    .unwrap_or_default();
                if let Some(src) = e.location.as_ref().and_then(|l| l.line_desc()) {
                    s.push_str(&format!("  [{}]", src));
                }
                s
            },
            e.params
                .iter()
                .map(|p| format!("0x{p:X}"))
                .collect::<Vec<_>>()
                .join(", "),
        ));
        out.push_str("registers:\n");
        for (name, v) in &e.registers.values {
            out.push_str(&format!("  {name:<6} {v:016X}\n"));
        }
        out.push('\n');
    }
    if !r.fault_asm.is_empty() {
        out.push_str("faulting code:\n");
        for row in &r.fault_asm {
            out.push_str(&format!(
                "{} 0x{:016X}  {}{}{}\n",
                if row.fault { ">" } else { " " },
                row.address,
                row.text,
                row.note
                    .clone()
                    .map(|n| format!("   ; {n}"))
                    .unwrap_or_default(),
                row.source
                    .as_ref()
                    .map(|s| format!("  [{}]", s))
                    .unwrap_or_default()
            ));
        }
        out.push('\n');
    }
    if !r.frames.is_empty() {
        out.push_str("call chain:\n");
        for f in &r.frames {
            let mut loc = f.location.describe();
            if let Some(src) = f.location.line_desc() {
                loc.push_str(&format!("  [{}]", src));
            }
            out.push_str(&format!("{:>4}  0x{:016X}  {}\n", f.slot, f.value, loc));
        }
        out.push('\n');
    }
    out.push_str("modules:\n");
    for m in &r.modules {
        let pdb = match &m.pdb {
            PdbState::Loaded(n) => labels.pdb_ok.replace("{{n}}", &n.to_string()),
            PdbState::Missing => labels.pdb_missing.clone(),
            PdbState::Mismatch => labels.pdb_mismatch.clone(),
            PdbState::Unreadable(e) => labels.pdb_unreadable.replace("{{e}}", e),
            PdbState::NoIdentity => labels.pdb_noid.clone(),
            PdbState::NotLoaded => labels.pdb_not_loaded.clone(),
        };
        out.push_str(&format!(
            "  {:<24} base 0x{:012X}  {:>9}  {}\n",
            m.module.name,
            m.module.base,
            crate::explorer::human_size(m.module.size),
            pdb
        ));
    }
    if !r.notes.is_empty() {
        out.push_str("\nnotes:\n");
        for n in &r.notes {
            out.push_str(&format!("  - {}\n", n.english()));
        }
    }
    out
}

/// Kick off (or re-use) the background analysis for `path`.
fn start_analysis(view: &mut DumpView, path: &std::path::Path, engine_root: &std::path::Path) {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let key = (path.to_path_buf(), mtime);
    if view.key.as_ref() == Some(&key) {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let p = path.to_path_buf();
    let engine = engine_root.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(crashdump::analyze(&p, &engine));
    });
    view.report = None;
    view.error = None;
    view.asm = None;
    view.asm_error = None;
    view.asm_key = None;
    view.pending = Some(rx);
    view.key = Some(key);
}

/// Request a disassembly window (worker thread, same shape as the report).
pub fn request_disassembly(
    view: &mut DumpView,
    dump: &std::path::Path,
    engine_root: &std::path::Path,
    module: usize,
    va: u64,
    count: usize,
    manual_pdb: Option<PathBuf>,
) {
    let key = (dump.to_path_buf(), module, va, count);
    if view.asm_key.as_ref() == Some(&key) && manual_pdb == view.manual_pdb {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let p = dump.to_path_buf();
    let engine = engine_root.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(crashdump::disassemble_range(
            &p,
            &engine,
            module,
            va,
            count,
            manual_pdb.as_deref(),
        ));
    });
    view.asm_pending = Some(rx);
    view.asm_key = Some(key);
}
