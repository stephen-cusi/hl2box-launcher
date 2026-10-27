//! Launcher UI state and pages.
//!
//! Every page is a method on [`App`]. Labels are fetched into owned strings at
//! the top of a page (`tl`), so closures that mutate `self` never fight the
//! borrow checker over a translation lookup.

use std::path::PathBuf;
use std::sync::{mpsc::Receiver, Arc, Mutex};

use eframe::egui::{self, Image, Margin, RichText, TextureHandle};
use eframe::epaint::ColorImage;

use hl2box_core::addons::{self, AddonEntry, AddonKind};
use hl2box_core::config::{self, LauncherConfig};
use hl2box_core::gameinfo::{self, MountableGame};
use hl2box_core::identity::{avatar_file, Identity};
use hl2box_core::launch::{self, Severity};
use hl2box_core::logs::{self, LogSource};
use hl2box_core::lua::is_lua;
use hl2box_core::paths::Paths;
use hl2box_core::preview;
use hl2box_core::steamid::SteamId64;
use hl2box_core::tree::{self, TreeNode};
use hl2box_core::version;
use hl2box_core::workshop::{self, WorkshopDetails};

use crate::explorer::{self, Explorer, ExplorerLabels, Pick};
use crate::i18n::{format as i18n_format, Catalog};
use crate::logview::{self, LogAction, LogLabels, LogView};
use crate::platform::{self, Theme, ThemeSetting};
use crate::textview::{TextView, ViewLabels};
use crate::theme::{self, Palette};
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Launch,
    Player,
    Addons,
    Workshop,
    Mounts,
    Logs,
    About,
}

const NAV: [(&str, Page); 7] = [
    ("nav.launch", Page::Launch),
    ("nav.player", Page::Player),
    ("nav.addons", Page::Addons),
    ("nav.workshop", Page::Workshop),
    ("nav.mounts", Page::Mounts),
    ("nav.logs", Page::Logs),
    ("nav.about", Page::About),
];

type Progress = Arc<Mutex<(u64, Option<u64>)>>;
type DownloadResult = Result<(String, u64), String>;

enum Job {
    Idle,
    Downloading {
        id: String,
        progress: Progress,
        done: Receiver<DownloadResult>,
        started: f64,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToastKind {
    Ok,
    Err,
    Info,
}

struct Toast {
    text: String,
    kind: ToastKind,
    at: f64,
}

/// How long a status message stays in the bottom bar.
const TOAST_SECS: f64 = 6.0;
/// Largest file the viewer opens (matches core's preview gate).
const VIEW_MAX: u64 = preview::MAX_PREVIEW_BYTES;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AddonsMode {
    /// One card per addon, with details on the right.
    List,
    /// The raw `addons/` directory as a file tree.
    Folder,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Overview,
    Files,
    Lua,
    Viewer,
}

/// Everything the addons page remembers between frames.
pub struct AddonsUi {
    pub mode: AddonsMode,
    pub selected: Option<usize>,
    pub filter: String,
    pub tab: DetailTab,
    /// Explorer inside the selected addon (Files tab).
    pub explorer: Explorer,
    /// Explorer over `addons/` in folder mode.
    pub folder_explorer: Explorer,
    pub folder_root: Option<TreeNode>,
    /// Tree of the selected addon, keyed by its name so it is rebuilt on change.
    pub tree: Option<(String, TreeNode)>,
    pub viewer: Option<TextView>,
    /// Full-text search over every addon's Lua.
    pub lua_query: String,
    pub lua_hits: Vec<hl2box_core::search::Hit>,
    pub lua_searched: bool,
}

impl Default for AddonsUi {
    fn default() -> Self {
        AddonsUi {
            mode: AddonsMode::List,
            selected: None,
            filter: String::new(),
            tab: DetailTab::Overview,
            explorer: Explorer::default(),
            folder_explorer: Explorer::default(),
            folder_root: None,
            tree: None,
            viewer: None,
            lua_query: String::new(),
            lua_hits: Vec::new(),
            lua_searched: false,
        }
    }
}

pub struct App {
    cfg: LauncherConfig,
    identity: Identity,
    page: Page,
    pal: Palette,
    now: f64,
    logo: Option<TextureHandle>,

    // Player
    name_edit: String,
    steamid_edit: String,
    avatar_tex: Option<TextureHandle>,
    avatar_failed: bool,

    // Addons
    addons: Vec<AddonEntry>,
    addons_ui: AddonsUi,
    gma_tex: Option<(String, TextureHandle)>,

    // Workshop
    workshop_input: String,
    workshop_details: Option<WorkshopDetails>,
    job: Job,

    // Mounts
    mounts: Vec<MountableGame>,
    mounts_dirty: bool,
    /// Dirs mounted by hand-written gameinfo lines: never touched by the launcher.
    mounts_handwritten: std::collections::HashSet<String>,

    // Logs
    log_sources: Vec<LogSource>,
    log_pick: usize,
    logs: LogView,
    log_reload_at: f64,

    // Look and feel
    catalog: Catalog,
    locale: String,
    lang_setting_auto: bool,
    theme_setting: ThemeSetting,
    system_theme: Theme,
    last_theme_poll: f64,
    toast: Option<Toast>,
    /// A file the folder-mode explorer asked to open this frame.
    pending_open: Option<String>,

    // Launch page extras
    maps: Vec<String>,
    preset_pick: usize,
    preset_name: String,
}

/// Directory holding the json language files: `lang/` next to the executable,
/// falling back to the source tree during development.
fn lang_dir() -> PathBuf {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("lang")))
        .filter(|d| d.is_dir());
    beside_exe.unwrap_or_else(|| PathBuf::from("lang"))
}

fn load_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(ctx.load_texture(
        name,
        ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba),
        egui::TextureOptions::LINEAR,
    ))
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut cfg = LauncherConfig::load().unwrap_or_default();
        if cfg.engine_root.is_empty() {
            cfg.engine_root = config::guess_engine_root();
        }
        let paths = cfg.paths();
        let identity = Identity::load(&paths).unwrap_or_default();

        // The game's cfg/config.cfg owns the player name (the engine archives
        // it there); the launcher identity only fills in when no cfg exists.
        let identity = match hl2box_core::gamecfg::read_name(&paths) {
            Some(n) => {
                let mut id = identity;
                if id.name != n {
                    id.name = n;
                    let _ = id.save(&paths);
                }
                id
            }
            None => identity,
        };

        let catalog = Catalog::load(&lang_dir());
        let lang_setting_auto = !catalog.has(&cfg.language);
        let locale = if lang_setting_auto {
            catalog.pick_system_locale()
        } else {
            cfg.language.clone()
        };
        let theme_setting = ThemeSetting::from_cfg(&cfg.theme);
        let system_theme = platform::system_theme();
        let pal = theme::palette(theme_setting.resolve(system_theme) == Theme::Dark);

        platform::install_cjk_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &pal);

        let mut app = App {
            name_edit: identity.name.clone(),
            steamid_edit: identity.steam_id().map(|s| s.render()).unwrap_or_default(),
            avatar_tex: None,
            avatar_failed: false,
            cfg,
            identity,
            page: Page::Launch,
            pal,
            now: 0.0,
            logo: load_texture(&cc.egui_ctx, "logo", include_bytes!("../assets/logo-128.png")),
            addons: Vec::new(),
            addons_ui: AddonsUi::default(),
            gma_tex: None,
            workshop_input: String::new(),
            workshop_details: None,
            job: Job::Idle,
            mounts: Vec::new(),
            mounts_dirty: false,
            mounts_handwritten: std::collections::HashSet::new(),
            log_sources: Vec::new(),
            log_pick: 0,
            logs: LogView::default(),
            log_reload_at: 0.0,
            catalog,
            locale,
            lang_setting_auto,
            theme_setting,
            system_theme,
            last_theme_poll: 0.0,
            toast: None,
            pending_open: None,
            maps: Vec::new(),
            preset_pick: 0,
            preset_name: String::new(),
        };
        app.refresh_addons();
        app.refresh_mounts();
        app.refresh_logs();
        app.refresh_maps();
        app.load_avatar_texture(cc.egui_ctx.clone());
        app
    }

    // ------------------------------------------------------------ helpers

    /// Translation lookup, borrowed.
    fn t<'a>(&'a self, key: &'a str) -> &'a str {
        self.catalog.tr(&self.locale, key)
    }

    /// Translation lookup, owned. Use this when the string must outlive a
    /// mutable borrow of `self` (any closure that writes state).
    fn tl(&self, key: &str) -> String {
        self.t(key).to_string()
    }

    fn tf(&self, key: &str, args: &[(&str, &str)]) -> String {
        i18n_format(self.t(key), args)
    }

    fn paths(&self) -> Paths {
        self.cfg.paths()
    }

    fn ok(&mut self, msg: impl Into<String>) {
        self.set_toast(msg, ToastKind::Ok);
    }

    fn err(&mut self, msg: impl Into<String>) {
        self.set_toast(msg, ToastKind::Err);
    }

    fn info(&mut self, msg: impl Into<String>) {
        self.set_toast(msg, ToastKind::Info);
    }

    fn set_toast(&mut self, msg: impl Into<String>, kind: ToastKind) {
        self.toast = Some(Toast {
            text: msg.into(),
            kind,
            at: self.now,
        });
    }

    fn save_cfg(&mut self) {
        if let Err(e) = self.cfg.save() {
            self.err(format!("settings save failed: {e}"));
        }
    }

    // ------------------------------------------------------------- data

    pub fn refresh_addons(&mut self) {
        let keep = self
            .addons_ui
            .selected
            .and_then(|i| self.addons.get(i))
            .map(|a| (a.name.clone(), a.kind));
        match addons::scan(&self.paths()) {
            Ok(list) => {
                self.addons = list;
                self.addons_ui.selected = keep.and_then(|(name, kind)| {
                    self.addons.iter().position(|a| a.name == name && a.kind == kind)
                });
                if self.addons_ui.selected.is_none() {
                    self.addons_ui.tree = None;
                    self.addons_ui.viewer = None;
                    self.gma_tex = None;
                    self.addons_ui.tab = DetailTab::Overview;
                }
            }
            Err(e) => self.err(format!("addon scan failed: {e}")),
        }
        self.addons_ui.folder_root = None;
        if self.addons_ui.mode == AddonsMode::Folder {
            self.ensure_folder_root();
        }
    }

    fn ensure_folder_root(&mut self) {
        if self.addons_ui.folder_root.is_some() {
            return;
        }
        let dir = self.paths().addons_dir();
        let _ = std::fs::create_dir_all(&dir);
        match tree::scan_dir(&dir) {
            Ok(t) => self.addons_ui.folder_root = Some(t),
            Err(e) => {
                self.addons_ui.folder_root = Some(TreeNode::root());
                self.err(format!("folder scan failed: {e}"));
            }
        }
    }

    /// Build the tree for the selected addon if it is not cached yet.
    fn ensure_addon_tree(&mut self) {
        let Some(i) = self.addons_ui.selected else { return };
        let Some(addon) = self.addons.get(i) else { return };
        if matches!(&self.addons_ui.tree, Some((k, _)) if *k == addon.name) {
            return;
        }
        let built = match addon.kind {
            AddonKind::Folder => {
                let dir = self.paths().addons_dir().join(&addon.name);
                tree::scan_dir(&dir).unwrap_or_else(|_| TreeNode::root())
            }
            AddonKind::Gma => match &addon.info {
                Some(info) => tree::from_files(info.entries.iter().map(|e| (e.path.as_str(), e.size))),
                None => TreeNode::root(),
            },
        };
        self.addons_ui.tree = Some((addon.name.clone(), built));
        self.addons_ui.explorer = Explorer::default();
    }

    pub fn refresh_maps(&mut self) {
        self.maps = hl2box_core::maps::list_maps(&self.paths());
    }

    fn run_lua_search(&mut self) {
        let q = self.addons_ui.lua_query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let paths = self.paths();
        self.addons_ui.lua_hits = hl2box_core::search::search_lua(&paths, &self.addons, &q, 300);
        self.addons_ui.lua_searched = true;
        let n = self.addons_ui.lua_hits.len().to_string();
        let msg = self.tf("addons.lua.found", &[("n", &n)]);
        self.info(msg);
    }

    /// Open a search hit: select its addon, open the file, jump to the line.
    fn open_hit(&mut self, idx: usize, ctx: egui::Context) {
        let Some(hit) = self.addons_ui.lua_hits.get(idx).cloned() else { return };
        let Some(i) = self.addons.iter().position(|a| a.name == hit.addon) else { return };
        self.select_addon(i, &ctx);
        self.ensure_addon_tree();
        self.open_text(&hit.path);
        let query = self.addons_ui.lua_query.clone();
        if let Some(v) = &mut self.addons_ui.viewer {
            v.search = query;
            v.jump_to(hit.line.saturating_sub(1));
        }
    }

    pub fn refresh_mounts(&mut self) {
        match gameinfo::available_games(&self.paths()) {
            Ok(g) => {
                self.mounts_handwritten = g
                    .iter()
                    .filter(|m| m.mounted && !m.managed)
                    .map(|m| m.dir.to_ascii_lowercase())
                    .collect();
                self.mounts = g;
                self.mounts_dirty = false;
            }
            Err(e) => self.err(format!("mount scan failed: {e}")),
        }
    }

    pub fn refresh_logs(&mut self) {
        self.log_sources = logs::known_logs(&self.paths());
        if self.log_pick >= self.log_sources.len() {
            self.log_pick = 0;
        }
        self.reload_logs();
    }

    fn reload_logs(&mut self) {
        let engine = self.paths().engine_root;
        logview::load(&mut self.logs, self.log_sources.get(self.log_pick), &engine);
        self.log_reload_at = self.now;
    }

    fn load_avatar_texture(&mut self, ctx: egui::Context) {
        let Some(path) = avatar_file(&self.paths(), &self.identity) else {
            self.avatar_tex = None;
            self.avatar_failed = false;
            return;
        };
        match image::open(&path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                self.avatar_tex = Some(ctx.load_texture(
                    "avatar",
                    ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba),
                    egui::TextureOptions::LINEAR,
                ));
                self.avatar_failed = false;
            }
            Err(_) => {
                self.avatar_tex = None;
                self.avatar_failed = true;
            }
        }
    }

    fn save_identity(&mut self, ctx: egui::Context) {
        if let Err(e) = self.identity.save(&self.paths()) {
            self.err(format!("identity save failed: {e}"));
            return;
        }
        self.load_avatar_texture(ctx);
        let msg = self.tl("player.saved");
        self.ok(msg);
    }

    /// Select an addon in list mode: reset the detail pane and load its preview.
    fn select_addon(&mut self, idx: usize, ctx: &egui::Context) {
        self.addons_ui.selected = Some(idx);
        self.addons_ui.tab = DetailTab::Overview;
        self.addons_ui.viewer = None;
        self.gma_tex = None;
        self.ensure_addon_tree();
        self.load_gma_preview(idx, ctx);
    }

    fn load_gma_preview(&mut self, idx: usize, ctx: &egui::Context) {
        self.gma_tex = None;
        let Some(a) = self.addons.get(idx) else { return };
        let Some(info) = &a.info else { return };
        let Some(jpeg) = &info.jpeg else { return };
        if let Ok(img) = image::load_from_memory(jpeg) {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let tex = ctx.load_texture(
                format!("gma_preview_{}", a.name),
                ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba),
                egui::TextureOptions::LINEAR,
            );
            self.gma_tex = Some((a.name.clone(), tex));
        }
    }

    /// Open one file in the viewer. `rel` is relative to the selected addon in
    /// list mode, or to `addons/` (first segment = addon name) in folder mode.
    fn open_text(&mut self, rel: &str) {
        let segs = match tree::split_path(rel) {
            Some(s) => s,
            None => {
                let msg = self.tl("viewer.invalid_path");
                return self.err(msg);
            }
        };
        let paths = self.paths();
        let (addon, inner) = match self.addons_ui.mode {
            AddonsMode::List => {
                let Some(i) = self.addons_ui.selected else { return };
                let Some(a) = self.addons.get(i) else { return };
                (a.clone(), segs.join("/"))
            }
            AddonsMode::Folder => {
                let name = segs[0].clone();
                let Some(a) = self.addons.iter().find(|a| a.name == name && a.kind == AddonKind::Folder) else {
                    let msg = self.tl("addons.archive_leaf");
                    return self.info(msg);
                };
                if segs.len() < 2 {
                    return;
                }
                (a.clone(), segs[1..].join("/"))
            }
        };

        if !preview::is_text_name(&inner) && !is_lua(&inner) {
            let msg = self.tl("viewer.not_text");
            return self.info(msg);
        }
        match addons::read_file(&paths, &addon, &inner, VIEW_MAX) {
            Ok(bytes) => {
                if !preview::looks_like_text(&bytes) {
                    let msg = self.tl("viewer.not_text");
                    return self.info(msg);
                }
                let text = preview::decode_text(&bytes);
                let title = format!("{}/{}", addon.name, inner);
                let source = explorer::human_size(bytes.len() as u64);
                self.addons_ui.viewer = Some(TextView::new(title, source, &text, is_lua(&inner)));
                self.addons_ui.tab = DetailTab::Viewer;
            }
            Err(e) => {
                let msg = format!("{}: {e}", self.tl("viewer.failed"));
                self.err(msg);
            }
        }
    }

    fn start_game(&mut self) {
        self.save_cfg();
        let issues = launch::validate(&self.cfg, &self.identity);
        if !launch::can_launch(&issues) {
            let msg = self.tl("launch.not_ready");
            return self.err(msg);
        }
        match config::start_game(&self.cfg, &self.identity) {
            Ok(child) => {
                let msg = self.tf("launch.started", &[("n", &child.id().to_string())]);
                self.ok(msg);
            }
            Err(e) => {
                let msg = self.tf("launch.start_failed", &[("e", &e.to_string())]);
                self.err(msg);
            }
        }
    }

    /// Pull progress and completion from the download thread.
    fn poll_job(&mut self) {
        let Job::Downloading { done, .. } = &self.job else { return };
        let Ok(res) = done.try_recv() else { return };
        match res {
            Ok((path, n)) => {
                let size = explorer::human_size(n);
                let msg = self.tf("workshop.done", &[("path", &path), ("size", &size)]);
                self.ok(msg);
                self.refresh_addons();
            }
            Err(e) => {
                let msg = self.tf("workshop.failed", &[("e", &e)]);
                self.err(msg);
            }
        }
        self.job = Job::Idle;
    }

    fn start_download(&mut self) {
        let Ok(id) = workshop::extract_workshop_id(&self.workshop_input) else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let progress: Progress = Arc::new(Mutex::new((0, None)));
        let progress_worker = Arc::clone(&progress);
        let paths = self.paths();
        let id_worker = id.clone();
        std::thread::spawn(move || {
            let result = (|| -> DownloadResult {
                let d = workshop::fetch_details(&id_worker).map_err(|e| e.0)?;
                let stem = sanitize_stem(&d.title, &id_worker);
                let dest = gameinfo::unique_addon_path(&paths, &stem);
                let n = workshop::download_to(&d, &dest, |done, total| {
                    if let Ok(mut p) = progress_worker.lock() {
                        *p = (done, total);
                    }
                })
                .map_err(|e| e.0)?;
                Ok((dest.to_string_lossy().into_owned(), n))
            })();
            let _ = tx.send(result);
        });
        self.job = Job::Downloading {
            id,
            progress,
            done: rx,
            started: self.now,
        };
    }

    // ------------------------------------------------------------ chrome

    fn side_navigation(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if let Some(logo) = &self.logo {
                ui.add(Image::from_texture(logo).fit_to_exact_size(egui::vec2(40.0, 40.0)));
            }
            ui.vertical(|ui| {
                ui.label(RichText::new("hl2box").size(18.0).strong());
                ui.label(RichText::new("launcher").size(12.0).color(pal.weak));
            });
        });
        ui.add_space(14.0);

        for (key, page) in NAV {
            let label = self.tl(key);
            if widgets::nav_item(ui, &pal, &label, self.page == page).clicked() && self.page != page {
                self.page = page;
                match page {
                    Page::Addons => self.refresh_addons(),
                    Page::Mounts => self.refresh_mounts(),
                    Page::Logs => self.refresh_logs(),
                    _ => {}
                }
            }
            ui.add_space(2.0);
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(8.0);
            ui.label(RichText::new(format!("v{}", version::LAUNCHER_VERSION)).small().color(pal.weak));
        });
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        egui::TopBottomPanel::top("top_bar")
            .frame(egui::Frame::new().fill(pal.window).inner_margin(Margin::symmetric(12, 6)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(self.tl("app.title")).strong().color(pal.weak));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // right-to-left: the first group added sits furthest right
                        self.language_picker(ui, &pal);
                        ui.add_space(12.0);
                        self.theme_picker(ui, &pal);
                    });
                });
            });
    }

    fn theme_picker(&mut self, ui: &mut egui::Ui, _pal: &Palette) {
        let current = self.theme_setting;
        let picked: [(ThemeSetting, String); 3] = [
            (ThemeSetting::Auto, self.tl("settings.theme.auto")),
            (ThemeSetting::Light, self.tl("settings.theme.light")),
            (ThemeSetting::Dark, self.tl("settings.theme.dark")),
        ];
        let selected_text = picked
            .iter()
            .find(|(v, _)| *v == current)
            .map(|(_, t)| t.clone())
            .unwrap_or_default();
        // right-to-left parent: the combo goes in first so the label lands on its left
        egui::ComboBox::from_id_salt("theme_pick")
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                for (value, text) in &picked {
                    ui.selectable_value(&mut self.theme_setting, *value, text);
                }
            });
        ui.label(RichText::new(self.tl("settings.theme")).color(_pal.weak));
        if self.theme_setting != current {
            self.cfg.theme = self.theme_setting.as_cfg().to_string();
            self.save_cfg();
        }
    }

    fn language_picker(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let auto_text = self.tl("settings.language.auto");
        let selected = if self.lang_setting_auto {
            auto_text.clone()
        } else {
            self.locale.clone()
        };
        let locales = self.catalog.locales.clone();
        let mut picked: Option<Option<String>> = None;
        egui::ComboBox::from_id_salt("lang_pick")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                if ui.selectable_label(self.lang_setting_auto, &auto_text).clicked() {
                    picked = Some(None);
                }
                for locale in &locales {
                    let on = !self.lang_setting_auto && self.locale == *locale;
                    if ui.selectable_label(on, locale).clicked() {
                        picked = Some(Some(locale.clone()));
                    }
                }
            });
        ui.label(RichText::new(self.tl("settings.language")).color(pal.weak));
        match picked {
            Some(Some(locale)) => {
                self.lang_setting_auto = false;
                self.locale = locale.clone();
                self.cfg.language = locale;
                self.save_cfg();
            }
            Some(None) => {
                self.lang_setting_auto = true;
                self.locale = self.catalog.pick_system_locale();
                self.cfg.language = "auto".into();
                self.save_cfg();
            }
            None => {}
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        egui::TopBottomPanel::bottom("status_bar")
            .frame(egui::Frame::new().fill(pal.window).inner_margin(Margin::symmetric(12, 5)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let fresh = self.toast.as_ref().filter(|t| self.now - t.at < TOAST_SECS);
                    match fresh {
                        Some(t) => {
                            let color = match t.kind {
                                ToastKind::Ok => pal.ok,
                                ToastKind::Err => pal.err,
                                ToastKind::Info => pal.blue,
                            };
                            widgets::status_text(ui, color, &t.text);
                        }
                        None => {
                            ui.label(RichText::new(self.t("launch.ready_hint")).color(pal.weak));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let valid = hl2box_core::paths::install_looks_valid(&self.paths());
                        let (color, key) = if valid {
                            (pal.ok, "nav.install_ok")
                        } else {
                            (pal.err, "nav.install_missing")
                        };
                        widgets::status_text(ui, color, self.t(key));
                    });
                });
            });
    }

    // ------------------------------------------------------------- pages

    fn page_launch(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let title = self.tl("launch.title");
        let subtitle = self.tl("launch.subtitle");
        widgets::page_header(ui, &pal, &title, Some(&subtitle));

        let l_install = self.tl("launch.install");
        let l_root = self.tl("launch.engine_root");
        let l_mod = self.tl("launch.mod_dir");
        let l_map = self.tl("launch.start_map");
        let l_browse = self.tl("launch.browse");
        let l_open_engine = self.tl("launch.open_engine");

        let mut new_root: Option<String> = None;
        widgets::card(ui, &pal, |ui| {
            widgets::section(ui, &pal, &l_install);
            egui::Grid::new("install")
                .num_columns(3)
                .spacing([12.0, 8.0])
                .min_col_width(120.0)
                .show(ui, |ui| {
                    ui.label(&l_root);
                    let mut root = self.cfg.engine_root.clone();
                    ui.add(egui::TextEdit::singleline(&mut root).desired_width(380.0));
                    if ui.button(&l_browse).clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            root = dir.to_string_lossy().into_owned();
                        }
                    }
                    if root != self.cfg.engine_root {
                        new_root = Some(root.trim().to_string());
                    }
                    ui.end_row();

                    ui.label(&l_mod);
                    let mut mdir = self.cfg.mod_dir.clone();
                    ui.add_enabled(false, egui::TextEdit::singleline(&mut mdir));
                    ui.end_row();

                    ui.label(&l_map);
                    let map_ok = launch::valid_map_name(self.cfg.start_map.trim()) || self.cfg.start_map.trim().is_empty();
                    let mut map_edit = egui::TextEdit::singleline(&mut self.cfg.start_map).desired_width(260.0);
                    if !map_ok {
                        map_edit = map_edit.text_color(pal.err);
                    }
                    ui.add(map_edit);
                    let pick_label = self.tl("launch.pick_map");
                    let no_maps = self.tl("launch.no_maps");
                    egui::ComboBox::from_id_salt("map_pick")
                        .selected_text(pick_label)
                        .show_ui(ui, |ui| {
                            if self.maps.is_empty() {
                                ui.weak(&no_maps);
                            }
                            for m in &self.maps {
                                if ui.selectable_label(self.cfg.start_map == *m, m).clicked() {
                                    self.cfg.start_map = m.clone();
                                }
                            }
                        });
                    ui.end_row();
                });
            ui.add_space(4.0);
            if ui.button(&l_open_engine).clicked() {
                hl2box_core::addons::open_in_file_manager(&self.paths().engine_root);
            }
        });
        if let Some(root) = new_root {
            self.cfg.engine_root = root;
            self.save_cfg();
            self.refresh_addons();
            self.refresh_mounts();
            self.refresh_logs();
            self.refresh_maps();
        }

        ui.add_space(10.0);
        let l_options = self.tl("launch.options");
        let l_args = self.tl("launch.extra_args");
        let l_sid = self.tl("launch.pass_steamid");
        let l_name = self.tl("launch.pass_name");
        widgets::card(ui, &pal, |ui| {
            widgets::section(ui, &pal, &l_options);
            ui.label(&l_args);
            ui.add(
                egui::TextEdit::multiline(&mut self.cfg.extra_args)
                    .desired_rows(2)
                    .desired_width(ui.available_width())
                    .font(egui::TextStyle::Monospace),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.cfg.pass_steamid, &l_sid);
                ui.add_space(16.0);
                ui.checkbox(&mut self.cfg.pass_name, &l_name);
            });
        });

        ui.add_space(10.0);
        let l_presets = self.tl("launch.presets");
        let l_load = self.tl("launch.preset_load");
        let l_save = self.tl("launch.preset_save");
        let l_delete = self.tl("launch.preset_delete");
        let l_name_hint = self.tl("launch.preset_name_hint");
        let l_none = self.tl("launch.preset_none");
        let names: Vec<String> = self.cfg.presets.iter().map(|p| p.name.clone()).collect();
        let (mut do_load, mut do_delete, mut do_save) = (false, false, false);
        widgets::card(ui, &pal, |ui| {
            widgets::section(ui, &pal, &l_presets);
            ui.horizontal_wrapped(|ui| {
                let current = names.get(self.preset_pick).cloned().unwrap_or_else(|| l_none.clone());
                egui::ComboBox::from_id_salt("preset_pick")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (i, n) in names.iter().enumerate() {
                            if ui.selectable_label(i == self.preset_pick, n).clicked() {
                                self.preset_pick = i;
                            }
                        }
                    });
                if ui.add_enabled(!names.is_empty(), egui::Button::new(&l_load)).clicked() {
                    do_load = true;
                }
                if ui.add_enabled(!names.is_empty(), egui::Button::new(&l_delete)).clicked() {
                    do_delete = true;
                }
                ui.separator();
                ui.add(egui::TextEdit::singleline(&mut self.preset_name).hint_text(&l_name_hint).desired_width(180.0));
                if ui.button(&l_save).clicked() {
                    do_save = true;
                }
            });
        });
        if do_load {
            if let Some(p) = self.cfg.presets.get(self.preset_pick).cloned() {
                self.cfg.extra_args = p.extra_args;
                self.cfg.start_map = p.start_map;
                self.save_cfg();
                let msg = self.tf("launch.preset_loaded", &[("name", &p.name)]);
                self.ok(msg);
            }
        }
        if do_delete {
            if let Some(name) = names.get(self.preset_pick).cloned() {
                hl2box_core::launch::remove_preset(&mut self.cfg, &name);
                self.preset_pick = 0;
                self.save_cfg();
            }
        }
        if do_save {
            let name = self.preset_name.trim().to_string();
            if launch::valid_preset_name(&name) {
                let preset = hl2box_core::launch::LaunchPreset {
                    name: name.clone(),
                    extra_args: self.cfg.extra_args.clone(),
                    start_map: self.cfg.start_map.clone(),
                };
                hl2box_core::launch::upsert_preset(&mut self.cfg, preset);
                self.save_cfg();
                self.preset_pick = self
                    .cfg
                    .presets
                    .iter()
                    .position(|p| p.name.eq_ignore_ascii_case(&name))
                    .unwrap_or(0);
                let msg = self.tf("launch.preset_saved", &[("name", &name)]);
                self.ok(msg);
            } else {
                let msg = self.tl("launch.preset_bad_name");
                self.err(msg);
            }
        }

        ui.add_space(10.0);
        let issues = launch::validate(&self.cfg, &self.identity);
        let can = launch::can_launch(&issues);
        let l_cmd = self.tl("launch.command_line");
        let l_copy = self.tl("launch.copy");
        let cmd = config::command_line(&self.cfg, &self.identity);
        widgets::card(ui, &pal, |ui| {
            ui.horizontal(|ui| {
                widgets::section(ui, &pal, &l_cmd);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(&l_copy).clicked() {
                        ui.ctx().copy_text(cmd.clone());
                    }
                });
            });
            egui::Frame::new()
                .fill(pal.code_bg)
                .corner_radius(6.0)
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(&cmd).monospace()).wrap());
                });
        });

        if !issues.is_empty() {
            ui.add_space(10.0);
            let l_issues = self.tl("launch.issues");
            widgets::card(ui, &pal, |ui| {
                widgets::section(ui, &pal, &l_issues);
                for issue in &issues {
                    let color = match issue.severity() {
                        Severity::Error => pal.err,
                        Severity::Warning => pal.warn,
                    };
                    let text = self.t(issue.key()).to_string();
                    widgets::status_text(ui, color, &text);
                }
            });
        }

        ui.add_space(14.0);
        let launch_label = self.tl("launch.start");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let btn = widgets::primary(&pal, format!("   {launch_label}   "));
            if ui.add_enabled(can, btn.min_size(egui::vec2(200.0, 40.0))).clicked() {
                self.start_game();
            }
        });
        ui.add_space(24.0);
    }

    fn page_player(&mut self, ui: &mut egui::Ui, ctx: egui::Context) {
        let pal = self.pal;
        let title = self.tl("player.title");
        let subtitle = self.tl("player.subtitle");
        widgets::page_header(ui, &pal, &title, Some(&subtitle));

        let choose_label = self.tl("player.choose_picture");
        let remove_label = self.tl("player.remove_avatar");
        let avatar_hint = self.tl("player.avatar_hint");
        let l_avatar = self.tl("player.avatar");
        widgets::card(ui, &pal, |ui| {
            widgets::section(ui, &pal, &l_avatar);
            ui.horizontal(|ui| {
                let size = egui::Vec2::splat(112.0);
                match &self.avatar_tex {
                    Some(tex) => {
                        ui.add(Image::from_texture(tex).fit_to_exact_size(size));
                    }
                    None => {
                        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        ui.painter().rect_filled(rect, 6.0, pal.code_bg);
                        let text = if self.avatar_failed {
                            self.tl("player.unreadable")
                        } else {
                            "?".to_string()
                        };
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            text,
                            egui::FontId::proportional(18.0),
                            pal.weak,
                        );
                    }
                }
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    if ui.button(&choose_label).clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("pictures", &["png", "jpg", "jpeg", "bmp", "webp"])
                            .pick_file()
                        {
                            match self.identity.install_avatar(&self.paths(), &p) {
                                Ok(()) => self.save_identity(ctx.clone()),
                                Err(e) => {
                                    let msg = self.tf("player.avatar_failed", &[("e", &e.to_string())]);
                                    self.err(msg);
                                }
                            }
                        }
                    }
                    if self.identity.avatar.is_some() && ui.button(&remove_label).clicked() {
                        let paths = self.paths();
                        self.identity.clear_avatar(&paths);
                        let _ = self.identity.save(&paths);
                        self.avatar_tex = None;
                        let msg = self.tl("player.saved");
                        self.ok(msg);
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new(avatar_hint).color(pal.weak));
                });
            });
        });

        ui.add_space(10.0);
        let l_identity = self.tl("player.identity");
        let name_label = self.tl("player.name");
        let apply_label = self.tl("player.apply_name");
        let sid_label = self.tl("player.steamid");
        let gen_label = self.tl("player.generate");
        let gen_random = self.tl("player.generate.random");
        let save_label = self.tl("player.save_steamid");
        let sid_hint = self.tl("player.steamid_forms_hint");
        widgets::card(ui, &pal, |ui| {
            widgets::section(ui, &pal, &l_identity);
            egui::Grid::new("identity")
                .num_columns(3)
                .spacing([12.0, 8.0])
                .min_col_width(90.0)
                .show(ui, |ui| {
                    ui.label(&name_label);
                    ui.add(egui::TextEdit::singleline(&mut self.name_edit).desired_width(260.0));
                    if ui.button(&apply_label).clicked() {
                        self.apply_name(ctx.clone());
                    }
                    ui.end_row();

                    ui.label(&sid_label);
                    ui.add(egui::TextEdit::singleline(&mut self.steamid_edit).desired_width(260.0));
                    ui.menu_button(&gen_label, |ui| {
                        if ui.button(&gen_random).clicked() {
                            self.steamid_edit = SteamId64::random().render();
                            ui.close_menu();
                        }
                    });
                    ui.end_row();
                });

            match SteamId64::parse(&self.steamid_edit) {
                Ok(id) => {
                    ui.add_space(6.0);
                    egui::Grid::new("sid_forms").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
                        ui.label(RichText::new("STEAM_0").color(pal.weak));
                        ui.monospace(id.steam2());
                        ui.end_row();
                        ui.label(RichText::new("[U:1:N]").color(pal.weak));
                        ui.monospace(id.u_form());
                        ui.end_row();
                        ui.label(RichText::new("id64").color(pal.weak));
                        ui.monospace(id.render());
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    if ui.button(&save_label).clicked() {
                        self.identity.steam_id = Some(id.0);
                        self.save_identity(ctx.clone());
                    }
                }
                Err(_) if !self.steamid_edit.trim().is_empty() => {
                    widgets::status_text(ui, pal.err, &sid_hint);
                }
                Err(_) => {
                    ui.add_space(4.0);
                    ui.label(RichText::new(sid_hint).color(pal.weak).small());
                }
            }
        });
    }

    fn apply_name(&mut self, ctx: egui::Context) {
        let name = self.name_edit.trim().to_string();
        if let Err(e) = hl2box_core::identity::validate_name(&name) {
            let msg = self.tf("player.name_error", &[("e", &e.0)]);
            return self.err(msg);
        }
        self.identity.name = name.clone();
        // cfg/config.cfg is what the engine reads and rewrites on exit.
        if let Err(e) = hl2box_core::gamecfg::write_name(&self.paths(), &name) {
            let msg = format!("cfg write failed: {e}");
            self.err(msg);
        }
        // Keep the name-keyed avatar in step with the steamid64 one.
        let paths = self.paths();
        if let Some(id) = self.identity.steam_id {
            let src = paths.avatar_png_for(&id.to_string());
            if src.is_file() {
                let dst = paths.avatar_png_for(&hl2box_core::paths::avatar_key(&name));
                if dst != src {
                    let _ = std::fs::copy(&src, &dst);
                }
            }
        }
        self.save_identity(ctx);
    }

    fn page_addons(&mut self, ui: &mut egui::Ui, ctx: egui::Context) {
        let pal = self.pal;
        let title = self.tl("addons.title");
        let subtitle = self.tl("addons.subtitle");
        widgets::page_header(ui, &pal, &title, Some(&subtitle));

        let l_list = self.tl("addons.mode.list");
        let l_folder = self.tl("addons.mode.folder");
        let l_filter = self.tl("addons.filter");
        let l_import = self.tl("addons.import");
        let l_open = self.tl("addons.open_folder");
        let l_refresh = self.tl("addons.refresh");
        let mut want_import = false;
        let mut want_refresh = false;
        let mut want_open = false;
        widgets::card(ui, &pal, |ui| {
            ui.horizontal_wrapped(|ui| {
                let mode = self.addons_ui.mode;
                if ui.add(egui::SelectableLabel::new(mode == AddonsMode::List, &l_list)).clicked() {
                    self.addons_ui.mode = AddonsMode::List;
                }
                if ui.add(egui::SelectableLabel::new(mode == AddonsMode::Folder, &l_folder)).clicked() {
                    self.addons_ui.mode = AddonsMode::Folder;
                    self.ensure_folder_root();
                }
                ui.separator();
                ui.label(&l_filter);
                ui.add(egui::TextEdit::singleline(&mut self.addons_ui.filter).desired_width(160.0));
                if ui.button(&l_import).clicked() {
                    want_import = true;
                }
                if ui.button(&l_open).clicked() {
                    want_open = true;
                }
                if ui.button(&l_refresh).clicked() {
                    want_refresh = true;
                }
            });
        });
        if want_open {
            let _ = std::fs::create_dir_all(self.paths().addons_dir());
            addons::open_in_file_manager(&self.paths().addons_dir());
        }
        if want_refresh {
            self.refresh_addons();
        }
        if want_import {
            if let Some(p) = rfd::FileDialog::new().add_filter("addon", &["gma"]).pick_file() {
                match addons::import(&self.paths(), &p) {
                    Ok(name) => {
                        let msg = self.tf("addons.imported", &[("name", &name)]);
                        self.ok(msg);
                        self.refresh_addons();
                    }
                    Err(e) => {
                        let msg = self.tf("addons.import_failed", &[("e", &e.0)]);
                        self.err(msg);
                    }
                }
            }
        }
        ui.add_space(8.0);

        if self.addons_ui.mode == AddonsMode::Folder {
            self.addons_folder_view(ui, &ctx);
            return;
        }

        egui::SidePanel::left("addon_list_panel")
            .resizable(true)
            .width_range(260.0..=520.0)
            .default_width(330.0)
            .frame(egui::Frame::new())
            .show_inside(ui, |ui| {
                self.addon_list(ui, &ctx);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(Margin {
                left: 12,
                right: 0,
                top: 0,
                bottom: 0,
            }))
            .show_inside(ui, |ui| {
                widgets::card(ui, &pal, |ui| {
                    ui.set_min_height(ui.available_height() - 4.0);
                    self.addon_detail(ui, &ctx);
                });
            });
    }

    /// Card-per-addon list. Clicking selects; the toggle never selects.
    fn addon_list(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let pal = self.pal;
        let filter = self.addons_ui.filter.trim().to_lowercase();
        let selected_idx = self.addons_ui.selected;
        let on_tag = self.tl("addons.on");
        let off_tag = self.tl("addons.off");
        let kind_gma = self.tl("addons.kind_gma");
        let kind_folder = self.tl("addons.kind_folder");
        let disable = self.tl("addons.disable");
        let enable = self.tl("addons.enable");
        let empty = self.tl("addons.empty");
        if self.addons.is_empty() {
            ui.add_space(12.0);
            ui.label(RichText::new(empty).color(pal.weak));
        }

        let mut clicked: Option<usize> = None;
        let mut toggles: Vec<(String, bool)> = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt("addon_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for i in 0..self.addons.len() {
                    let a = &self.addons[i];
                    let title = a.title().to_string();
                    if !filter.is_empty()
                        && !title.to_lowercase().contains(&filter)
                        && !a.name.to_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    let selected = selected_idx == Some(i);
                    let name = a.name.clone();
                    let enabled = a.enabled;
                    let kind = a.kind;
                    let disk = a.disk_size;
                    let border = if selected { pal.accent } else { pal.border };
                    let fill = if selected { crate::theme::alpha(pal.accent, 28) } else { pal.card };
                    let resp = egui::Frame::new()
                        .fill(fill)
                        .stroke(egui::Stroke::new(1.0_f32, border))
                        .corner_radius(6.0)
                        .inner_margin(Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let (tag, color) = if enabled { (on_tag.as_str(), pal.ok) } else { (off_tag.as_str(), pal.weak) };
                                widgets::badge(ui, tag, color);
                                ui.add(
                                    egui::Label::new(RichText::new(&title).strong())
                                        .truncate(),
                                );
                            });
                            ui.horizontal(|ui| {
                                let kind_text = match kind {
                                    AddonKind::Gma => kind_gma.as_str(),
                                    AddonKind::Folder => kind_folder.as_str(),
                                };
                                ui.label(RichText::new(format!("{kind_text} - {}", explorer::human_size(disk))).small().color(pal.weak));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let label = if enabled { disable.as_str() } else { enable.as_str() };
                                    if ui.small_button(label).clicked() {
                                        toggles.push((name.clone(), enabled));
                                    }
                                });
                            });
                        })
                        .response
                        .interact(egui::Sense::click());
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                    ui.add_space(4.0);
                }
            });

        if let Some(i) = clicked {
            self.select_addon(i, ctx);
        }
        for (name, enabled) in toggles {
            if addons::set_enabled(&self.paths(), &name, !enabled).is_ok() {
                let action = if enabled { self.tl("addons.disable") } else { self.tl("addons.enable") };
                let msg = self.tf("addons.toggled", &[("action", &action), ("name", &name)]);
                self.ok(msg);
                self.refresh_addons();
            } else {
                let msg = self.tl("addons.toggle_failed");
                self.err(msg);
            }
        }
    }

    fn addon_detail(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let pal = self.pal;
        let Some(i) = self.addons_ui.selected else {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(self.t("addons.select_hint")).color(pal.weak));
            });
            return;
        };
        self.ensure_addon_tree();
        let Some(a) = self.addons.get(i).cloned() else { return };

        // header
        ui.horizontal(|ui| {
            ui.label(RichText::new(a.title()).heading().strong());
            let (tag, color) = if a.enabled {
                (self.tl("addons.on"), pal.ok)
            } else {
                (self.tl("addons.off"), pal.weak)
            };
            widgets::badge(ui, tag, color);
        });
        ui.monospace(&a.name);
        ui.add_space(6.0);

        // tabs
        let labels = [
            (DetailTab::Overview, self.tl("addons.tab.overview")),
            (DetailTab::Files, self.tl("addons.tab.files")),
            (DetailTab::Lua, self.tl("addons.tab.lua")),
            (DetailTab::Viewer, self.tl("addons.tab.viewer")),
        ];
        ui.horizontal(|ui| {
            for (tab, label) in &labels {
                let on = self.addons_ui.tab == *tab;
                if ui.add(egui::SelectableLabel::new(on, label)).clicked() {
                    self.addons_ui.tab = *tab;
                }
            }
        });
        ui.separator();

        match self.addons_ui.tab {
            DetailTab::Overview => self.addon_overview(ui, &a),
            DetailTab::Files => self.addon_files(ui),
            DetailTab::Lua => self.addon_lua(ui),
            DetailTab::Viewer => self.addon_viewer(ui),
        }
        let _ = ctx;
    }

    fn addon_overview(&mut self, ui: &mut egui::Ui, a: &AddonEntry) {
        let pal = self.pal;
        let mut rows: Vec<(String, String, bool)> = Vec::new();
        let kind = match a.kind {
            AddonKind::Gma => self.tl("addons.kind_gma"),
            AddonKind::Folder => self.tl("addons.kind_folder"),
        };
        rows.push((self.tl("addons.meta.kind"), kind, false));
        let yes = if a.enabled { self.tl("addons.meta.enabled_yes") } else { self.tl("addons.meta.enabled_no") };
        rows.push((self.tl("addons.meta.enabled"), yes, false));
        rows.push((self.tl("addons.meta.disk_size"), explorer::human_size(a.disk_size), false));
        if let Some(author) = a.author() {
            rows.push((self.tl("addons.meta.author"), author.to_string(), false));
        }
        match &a.info {
            Some(info) => {
                rows.push((self.tl("addons.meta.files"), info.entries.len().to_string(), false));
                rows.push((self.tl("addons.meta.content_size"), explorer::human_size(info.total_size()), false));
                if !info.description.is_empty() {
                    rows.push((
                        self.tl("addons.meta.description"),
                        truncate(&info.description.replace("\\n", " "), 400),
                        false,
                    ));
                }
            }
            None if a.kind == AddonKind::Gma => {
                rows.push((self.tl("addons.meta.files"), self.tl("addons.no_index"), false));
            }
            None => {
                let n = self.addons_ui.tree.as_ref().map(|(_, t)| t.file_count()).unwrap_or(0);
                rows.push((self.tl("addons.meta.files"), n.to_string(), false));
            }
        }
        widgets::kv_grid(ui, &pal, "addon_meta", &rows);

        if let Some((owner, tex)) = &self.gma_tex {
            if owner == &a.name {
                ui.add_space(8.0);
                ui.add(Image::from_texture(tex).max_size(egui::vec2(260.0, 180.0)));
                ui.label(RichText::new(self.t("addons.preview")).small().color(pal.weak));
            }
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let toggle = if a.enabled { self.tl("addons.disable") } else { self.tl("addons.enable") };
            if ui.button(toggle).clicked() {
                if addons::set_enabled(&self.paths(), &a.name, !a.enabled).is_ok() {
                    let action = if a.enabled { self.tl("addons.disable") } else { self.tl("addons.enable") };
                    let msg = self.tf("addons.toggled", &[("action", &action), ("name", &a.name)]);
                    self.ok(msg);
                    self.refresh_addons();
                }
            }
            if a.kind == AddonKind::Folder && ui.button(self.tl("addons.open_in_fm")).clicked() {
                addons::open_in_file_manager(&self.paths().addons_dir().join(&a.name));
            }
        });
        if a.kind == AddonKind::Folder {
            ui.add_space(4.0);
            ui.label(RichText::new(self.t("addons.folder_browse")).color(pal.weak));
        }
    }

    fn explorer_labels(&self, root_mode: bool) -> ExplorerLabels {
        let search = if root_mode { "addons.browse.search_root" } else { "addons.browse.search" };
        ExplorerLabels {
            root: self.tl("addons.browse.root"),
            up: self.tl("addons.browse.up"),
            empty: self.tl("addons.browse.empty"),
            no_match: self.tl("addons.browse.no_match"),
            items: self.tl("addons.browse.items"),
            search_hint: self.tl(search),
        }
    }

    fn addon_files(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let labels = self.explorer_labels(false);
        let pick = match &self.addons_ui.tree {
            Some((_, root)) => explorer::show(ui, &pal, root, &mut self.addons_ui.explorer, &labels),
            None => Pick::Nothing,
        };
        if let Pick::File(rel) = pick {
            self.open_text(&rel);
        }
    }

    fn addon_lua(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let l_hint = self.tl("addons.lua.search_hint");
        let l_btn = self.tl("addons.lua.search");
        let l_none = self.tl("addons.lua.no_results");
        let mut run = false;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.addons_ui.lua_query)
                    .hint_text(&l_hint)
                    .desired_width(300.0),
            );
            if ui.button(&l_btn).clicked() {
                run = true;
            }
        });
        if run {
            self.run_lua_search();
        }
        let mut open_hit: Option<usize> = None;
        if self.addons_ui.lua_searched {
            if self.addons_ui.lua_hits.is_empty() {
                ui.weak(&l_none);
            }
            egui::ScrollArea::vertical()
                .id_salt("lua_hits")
                .max_height(190.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (i, h) in self.addons_ui.lua_hits.iter().enumerate() {
                        let label = format!("{}:{}", h.path, h.line);
                        if crate::widgets::list_row(ui, &pal, &label, &h.addon, None, false).clicked() {
                            open_hit = Some(i);
                        }
                        ui.label(RichText::new(&h.text).monospace().small().color(pal.weak));
                    }
                });
            ui.separator();
        }
        if let Some(i) = open_hit {
            let ctx = ui.ctx().clone();
            self.open_hit(i, ctx);
        }
        let empty = self.tl("addons.lua.empty");
        let realm_labels: [String; 5] = [
            self.tl("lua.realm.shared"),
            self.tl("lua.realm.server"),
            self.tl("lua.realm.client"),
            self.tl("lua.realm.menu"),
            self.tl("lua.realm.other"),
        ];
        let count_tpl = self.tl("lua.count");
        let pick = match &self.addons_ui.tree {
            Some((_, root)) => {
                let realm_label = |r: hl2box_core::lua::Realm| {
                    let idx = hl2box_core::lua::Realm::ALL.iter().position(|x| *x == r).unwrap_or(4);
                    realm_labels[idx].clone()
                };
                let count_fmt = |n: usize| count_tpl.replace("{{n}}", &n.to_string());
                explorer::lua_listing(ui, &pal, root, &empty, &realm_label, &count_fmt)
            }
            None => None,
        };
        if let Some(rel) = pick {
            self.open_text(&rel);
        }
    }

    fn addon_viewer(&mut self, ui: &mut egui::Ui) {
        let labels = ViewLabels {
            search_hint: self.tl("viewer.search"),
            prev: self.tl("viewer.prev"),
            next: self.tl("viewer.next"),
            wrap: self.tl("viewer.wrap"),
            copy: self.tl("viewer.copy"),
            lines: self.tl("viewer.lines"),
            no_hits: self.tl("viewer.no_hits"),
            lua: self.tl("viewer.lua"),
        };
        let none = self.tl("viewer.none");
        let pal = self.pal;
        match &mut self.addons_ui.viewer {
            Some(view) => view.ui(ui, &pal, &labels),
            None => {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| ui.label(RichText::new(none).color(pal.weak)));
            }
        }
    }

    /// Folder mode: the real `addons/` directory on the left, the viewer on the right.
    fn addons_folder_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let _ = ctx;
        let pal = self.pal;
        let labels = self.explorer_labels(true);
        let hint = self.tl("addons.folder_mode_hint");
        egui::SidePanel::left("addon_folder_panel")
            .resizable(true)
            .width_range(260.0..=520.0)
            .default_width(340.0)
            .frame(egui::Frame::new())
            .show_inside(ui, |ui| {
                widgets::card(ui, &pal, |ui| {
                    ui.set_min_height(ui.available_height() - 4.0);
                    ui.label(RichText::new(hint).small().color(pal.weak));
                    ui.add_space(4.0);
                    let pick = match &self.addons_ui.folder_root {
                        Some(root) => explorer::show(ui, &pal, root, &mut self.addons_ui.folder_explorer, &labels),
                        None => Pick::Nothing,
                    };
                    if let Pick::File(rel) = pick {
                        self.pending_open = Some(rel);
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(Margin {
                left: 12,
                right: 0,
                top: 0,
                bottom: 0,
            }))
            .show_inside(ui, |ui| {
                widgets::card(ui, &pal, |ui| {
                    ui.set_min_height(ui.available_height() - 4.0);
                    self.addon_viewer(ui);
                });
            });
        if let Some(rel) = self.pending_open.take() {
            self.open_text(&rel);
        }
    }

    fn page_workshop(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let title = self.tl("workshop.title");
        let subtitle = self.tl("workshop.subtitle");
        widgets::page_header(ui, &pal, &title, Some(&subtitle));

        let hint1 = self.tl("workshop.hint1");
        let hint2 = self.tl("workshop.hint2");
        let details_label = self.tl("workshop.details");
        let dl_label = self.tl("workshop.download");
        let input_hint = self.tl("workshop.input_hint");
        let parse_ok = workshop::extract_workshop_id(&self.workshop_input).is_ok();

        widgets::card(ui, &pal, |ui| {
            widgets::section(ui, &pal, &details_label);
            ui.label(hint1);
            ui.label(RichText::new(hint2).color(pal.weak).small());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.workshop_input)
                        .hint_text(input_hint)
                        .desired_width(480.0),
                );
                let fetch = ui.add_enabled(parse_ok, egui::Button::new(&details_label));
                if fetch.clicked() {
                    match workshop::extract_workshop_id(&self.workshop_input).and_then(|id| workshop::fetch_details(&id)) {
                        Ok(d) => self.workshop_details = Some(d),
                        Err(e) => {
                            self.workshop_details = None;
                            let msg = format!("workshop: {e}");
                            self.err(msg);
                        }
                    }
                }
            });
        });

        if let Some(d) = self.workshop_details.clone() {
            ui.add_space(10.0);
            widgets::card(ui, &pal, |ui| {
                let rows = vec![
                    (self.tl("workshop.meta.title"), d.title.clone(), false),
                    (self.tl("workshop.meta.id"), d.publishedfileid.clone(), true),
                    (
                        self.tl("workshop.meta.size"),
                        d.file_size.parse::<u64>().map(explorer::human_size).unwrap_or_else(|_| d.file_size.clone()),
                        false,
                    ),
                    (self.tl("workshop.meta.updated"), format_unix(d.time_updated), false),
                    (self.tl("workshop.meta.creator"), d.creator.clone(), true),
                ];
                widgets::kv_grid(ui, &pal, "ws_details", &rows);
            });
        }

        ui.add_space(10.0);
        widgets::card(ui, &pal, |ui| {
            match &self.job {
                Job::Idle => {
                    ui.horizontal(|ui| {
                        let can = parse_ok && self.workshop_details.is_some();
                        if ui.add_enabled(can, widgets::primary(&pal, dl_label)).clicked() {
                            self.start_download();
                        }
                        if !can {
                            ui.label(RichText::new(self.t("workshop.need_details")).color(pal.weak));
                        }
                    });
                }
                Job::Downloading { id, progress, .. } => {
                    let (done, total) = *progress.lock().unwrap_or_else(|p| p.into_inner());
                    let elapsed = self.now - self.job_started();
                    ui.label(self.tf("workshop.downloading", &[("id", id)]));
                    match total {
                        Some(t) if t > 0 => {
                            let frac = (done as f32 / t as f32).clamp(0.0, 1.0);
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .show_percentage()
                                    .text(format!("{} / {}", explorer::human_size(done), explorer::human_size(t))),
                            );
                        }
                        _ => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.weak(format!("{} ({:.0}s)", explorer::human_size(done), elapsed));
                            });
                        }
                    }
                    ctx_repaint(ui);
                }
            }
        });
    }

    fn job_started(&self) -> f64 {
        match &self.job {
            Job::Downloading { started, .. } => *started,
            Job::Idle => self.now,
        }
    }

    fn page_mounts(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let title = self.tl("mounts.title");
        let subtitle = self.tl("mounts.subtitle");
        widgets::page_header(ui, &pal, &title, Some(&subtitle));

        let hint1 = self.tl("mounts.hint1");
        let hint2 = self.tl("mounts.hint2");
        let none = self.tl("mounts.none");
        let vpk_tpl = self.tl("mounts.vpks");
        let hand_label = self.tl("mounts.handwritten");
        let managed_label = self.tl("mounts.managed");
        let mounted_count = self.mounts.iter().filter(|m| m.mounted).count();
        let summary = self.tf("mounts.summary", &[("n", &mounted_count.to_string())]);

        widgets::card(ui, &pal, |ui| {
            ui.label(hint1);
            ui.label(RichText::new(hint2).color(pal.weak).small());
            ui.add_space(6.0);
            ui.label(RichText::new(summary).strong());
        });
        ui.add_space(8.0);

        widgets::card(ui, &pal, |ui| {
            if self.mounts.is_empty() {
                ui.label(RichText::new(none).color(pal.weak));
            }
            let handwritten_flags: Vec<bool> = self
                .mounts
                .iter()
                .map(|m| self.mounts_handwritten.contains(&m.dir.to_ascii_lowercase()))
                .collect();
            for mi in 0..self.mounts.len() {
                let (dir, vpk_count, managed, mounted) = {
                    let m = &self.mounts[mi];
                    (m.dir.clone(), m.vpks.len(), m.managed, m.mounted)
                };
                let handwritten = handwritten_flags[mi];
                let mut new_mounted = mounted;
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!handwritten, |ui| {
                        if ui.checkbox(&mut new_mounted, dir.as_str()).changed() {
                            self.mounts_dirty = true;
                        }
                    });
                    widgets::badge(ui, vpk_tpl.replace("{{n}}", &vpk_count.to_string()), pal.weak);
                    if handwritten {
                        widgets::badge(ui, hand_label.as_str(), pal.weak);
                    } else if managed {
                        widgets::badge(ui, managed_label.as_str(), pal.blue);
                    }
                });
                if new_mounted != mounted {
                    self.mounts[mi].mounted = new_mounted;
                }
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let apply_label = self.tl("mounts.apply");
            let rescan_label = self.tl("mounts.rescan");
            if ui
                .add_enabled(self.mounts_dirty, widgets::primary(&pal, apply_label))
                .clicked()
            {
                let selected: Vec<String> = self
                    .mounts
                    .iter()
                    .filter(|m| m.mounted && !self.mounts_handwritten.contains(&m.dir.to_ascii_lowercase()))
                    .map(|m| m.dir.clone())
                    .collect();
                match gameinfo::apply_mounts(&self.paths(), &selected) {
                    Ok(()) => {
                        let msg = self.tl("mounts.applied");
                        self.ok(msg);
                        self.refresh_mounts();
                    }
                    Err(e) => {
                        let msg = self.tf("mounts.apply_failed", &[("e", &e.0)]);
                        self.err(msg);
                    }
                }
            }
            if ui.button(&rescan_label).clicked() {
                self.refresh_mounts();
            }
        });
    }

    fn page_logs(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let sources: Vec<(String, PathBuf, bool)> = self
            .log_sources
            .iter()
            .map(|s| (self.log_source_title(s), s.path.clone(), s.path.is_file()))
            .collect();
        let dirs = [
            self.paths().engine_root.join("hl2sb").join("bin"),
            self.paths().engine_root.join("bin"),
        ]
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("   ");
        let labels = LogLabels {
            crash: crate::logview::CrashLabels {
                thread: self.tl("crash.thread"),
                address: self.tl("crash.address"),
                params: self.tl("crash.params"),
                modules: self.tl("crash.modules"),
                pdb_dirs: self.tl("crash.pdb_dirs"),
                pdb_loaded: self.tl("crash.pdb_loaded"),
                pdb_missing: self.tl("crash.pdb_missing"),
                pdb_mismatch: self.tl("crash.pdb_mismatch"),
                pdb_unreadable: self.tl("crash.pdb_unreadable"),
                pdb_noid: self.tl("crash.pdb_noid"),
                pdb_not_loaded: self.tl("crash.pdb_not_loaded"),
                loading: self.tl("crash.loading"),
                fault: self.tl("crash.fault"),
                registers: self.tl("crash.registers"),
                frames: self.tl("crash.frames"),
                no_frames: self.tl("crash.no_frames"),
                notes: self.tl("crash.notes"),
                toggle_report: self.tl("crash.toggle_report"),
                toggle_strings: self.tl("crash.toggle_strings"),
                error: self.tl("crash.error"),
                no_exception: self.tl("crash.no_exception"),
                dir_list: dirs,
            },
            title: self.tl("logs.title"),
            auto: self.tl("logs.auto"),
            refresh: self.tl("logs.refresh"),
            follow: self.tl("logs.follow"),
            collapse: self.tl("logs.collapse"),
            search_hint: self.tl("logs.search"),
            open_folder: self.tl("logs.open_folder"),
            copy_visible: self.tl("logs.copy"),
            export: self.tl("logs.export"),
            none: self.tl("logs.none"),
            missing: self.tl("logs.missing"),
            showing: self.tl("logs.showing"),
            summary: self.tl("logs.summary"),
            span: self.tl("logs.span"),
            level: [
                self.tl("logs.level.error"),
                self.tl("logs.level.warn"),
                self.tl("logs.level.info"),
                self.tl("logs.level.plain"),
            ],
            all: self.tl("logs.all"),
            sources: self.tl("logs.sources"),
            prev_error: self.tl("logs.prev_error"),
            next_error: self.tl("logs.next_error"),
            size: self.tl("logs.size"),
            modified: self.tl("logs.modified"),
            binary_banner: self.tl("logs.binary_banner"),
            col_time: self.tl("logs.col.time"),
            col_level: self.tl("logs.col.level"),
            col_source: self.tl("logs.col.source"),
            col_message: self.tl("logs.col.message"),
            no_rows: self.tl("logs.no_rows"),
            copied: self.tl("logs.copied"),
            exported: self.tl("logs.exported"),
        };
        let pick = self.log_pick;
        match logview::show(ui, &pal, &mut self.logs, &sources, pick, &labels) {
            LogAction::Nothing => {}
            LogAction::Pick(i) => {
                self.log_pick = i;
                self.reload_logs();
            }
            LogAction::Reload => self.reload_logs(),
            LogAction::Note(text, ok) => {
                if ok {
                    self.ok(text);
                } else {
                    self.err(text);
                }
            }
        }
    }

    fn page_about(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        let title = self.tl("about.title");
        let subtitle = self.tl("about.subtitle");
        widgets::page_header(ui, &pal, &title, Some(&subtitle));

        let paths = self.paths();
        let ev = version::engine_version(&paths.engine_root).unwrap_or_else(|| self.tl("about.unknown"));
        let launcher_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let rows = vec![
            (self.tl("about.launcher"), version::LAUNCHER_VERSION.to_string(), false),
            (self.tl("about.build"), version::launcher_build().to_string(), false),
            (self.tl("about.engine"), ev, false),
            (self.tl("about.engine_root"), paths.engine_root.to_string_lossy().into_owned(), true),
            (self.tl("about.mod_dir"), paths.mod_dir.clone(), true),
            (self.tl("about.game_exe"), paths.game_exe().to_string_lossy().into_owned(), true),
            (self.tl("about.launcher_dir"), launcher_dir, true),
        ];
        widgets::card(ui, &pal, |ui| {
            widgets::kv_grid(ui, &pal, "about", &rows);
        });
        ui.add_space(10.0);

        let open_engine = self.tl("about.open_engine");
        let open_launcher = self.tl("about.open_launcher");
        let open_logs = self.tl("about.open_logs");
        ui.horizontal(|ui| {
            if ui.button(open_engine).clicked() {
                addons::open_in_file_manager(&paths.engine_root);
            }
            if ui.button(open_launcher).clicked() {
                if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
                    addons::open_in_file_manager(&dir);
                }
            }
            if ui.button(open_logs).clicked() {
                addons::open_in_file_manager(&paths.mod_root());
            }
        });
        ui.add_space(10.0);
        widgets::card(ui, &pal, |ui| {
            for key in ["about.note1", "about.note2", "about.note3"] {
                ui.label(RichText::new(self.t(key)).color(pal.weak));
            }
        });
    }

    /// Display title of a log source: the locale table first, the built-in
    /// english title as fallback.
    fn log_source_title(&self, source: &LogSource) -> String {
        let key = format!("logs.source.{}", source.id);
        let text = self.t(&key);
        if text == key {
            source.title.clone()
        } else {
            text.to_string()
        }
    }
}

/// Keep the download spinner and progress bar moving.
fn ctx_repaint(ui: &egui::Ui) {
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.now = ctx.input(|i| i.time);
        self.apply_theme(ctx);
        self.poll_job();

        if self.page == Page::Logs && self.logs.auto_refresh && self.now - self.log_reload_at > 1.5 {
            self.reload_logs();
        }

        self.top_bar(ctx);
        self.status_bar(ctx);

        let pal = self.pal;
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(186.0)
            .frame(egui::Frame::new().fill(pal.window).inner_margin(Margin::symmetric(12, 4)))
            .show(ctx, |ui| self.side_navigation(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.panel).inner_margin(Margin::same(16)))
            .show(ctx, |ui| match self.page {
                // these two lay out their own panels and scroll areas
                Page::Addons => self.page_addons(ui, ctx.clone()),
                Page::Logs => self.page_logs(ui),
                _ => {
                    egui::ScrollArea::vertical()
                        .id_salt("page_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| match self.page {
                            Page::Launch => self.page_launch(ui),
                            Page::Player => self.page_player(ui, ctx.clone()),
                            Page::Workshop => self.page_workshop(ui),
                            Page::Mounts => self.page_mounts(ui),
                            _ => self.page_about(ui),
                        });
                }
            });

        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

impl App {
    /// Follow the theme setting (and the OS when on auto) and push the palette.
    fn apply_theme(&mut self, ctx: &egui::Context) {
        if self.now - self.last_theme_poll > 5.0 {
            self.last_theme_poll = self.now;
            self.system_theme = platform::system_theme();
        }
        let dark = self.theme_setting.resolve(self.system_theme) == Theme::Dark;
        if dark != self.pal.dark {
            self.pal = theme::palette(dark);
        }
        theme::apply(ctx, &self.pal);
    }
}

// ---------------------------------------------------------------- helpers

pub fn format_unix(secs: i64) -> String {
    // Same civil-date math as core::version; enough for a table cell.
    let days = secs.div_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}")
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut cut = n;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}...", &s[..cut])
    }
}

fn sanitize_stem(title: &str, id: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let cleaned = cleaned.trim_matches('_').to_string();
    if cleaned.is_empty() {
        format!("workshop_{id}")
    } else {
        cleaned
    }
}
