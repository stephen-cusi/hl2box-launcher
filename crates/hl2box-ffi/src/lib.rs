//! C ABI over hl2box-core for native embedders (the Android launcher).
//!
//! Conventions:
//! - Every function is `hl2box_<group>_<action>` and returns a status or a
//!   heap pointer. Heap results are freed with [`hl2box_free`].
//! - Errors: most functions return 0 on success, -1 on failure; call
//!   [`hl2box_last_error`] for the message of the most recent failure on
//!   the calling thread.
//! - Strings are UTF-8, NUL-terminated, valid until the next call on the
//!   same thread that produced them (or until freed when explicitly
//!   heap-allocated).
//! - Paths: the caller owns the layout. On Android pass the engine root
//!   (the files directory holding hl2_launcher and bin/) and the mod dir
//!   name exactly like the desktop config does.

use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;

use hl2box_core::addons::{self, AddonEntry};
use hl2box_core::config::LauncherConfig;
use hl2box_core::gameinfo::{self, MountableGame};
use hl2box_core::gma::{self, GmaInfo};
use hl2box_core::identity::{self, Identity};
use hl2box_core::paths::Paths;
use hl2box_core::steamid::SteamId64;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(msg: String) {
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = CString::new(msg).ok();
    });
}

macro_rules! try_i {
    ($expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => {
                set_error(e.to_string());
                return -1;
            }
        }
    };
}

macro_rules! try_p {
    ($expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => {
                set_error(e.to_string());
                return std::ptr::null_mut();
            }
        }
    };
}

/// Take a C string argument; empty pointer counts as "".
fn arg<'a>(s: *const c_char) -> std::borrow::Cow<'a, str> {
    if s.is_null() {
        "".into()
    } else {
        unsafe { CStr::from_ptr(s) }.to_string_lossy()
    }
}

// ---------------------------------------------------------------------------
// General
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn hl2box_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// Free anything this library returned as a heap pointer.
#[no_mangle]
pub extern "C" fn hl2box_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        unsafe {
            drop(CString::from_raw(ptr as *mut c_char));
        }
    }
}

/// UTF-8 message of the most recent error on this thread, or "".
#[no_mangle]
pub extern "C" fn hl2box_last_error() -> *const c_char {
    LAST_ERROR.with(|e| match &*e.borrow() {
        Some(s) => s.as_ptr(),
        None => "\0".as_ptr() as *const c_char,
    })
}

fn heap_str(s: impl Into<String>) -> *mut c_char {
    match CString::new(s.into()) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

fn make_paths(engine_root: *const c_char, mod_dir: *const c_char) -> Paths {
    Paths::new(arg(engine_root).as_ref(), arg(mod_dir).as_ref())
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Load the identity JSON; returns a heap JSON string, free with hl2box_free.
#[no_mangle]
pub extern "C" fn hl2box_identity_load(
    engine_root: *const c_char,
    mod_dir: *const c_char,
) -> *mut c_char {
    let paths = make_paths(engine_root, mod_dir);
    match Identity::load(&paths) {
        Ok(id) => match serde_json::to_string(&id) {
            Ok(s) => heap_str(s),
            Err(e) => {
                set_error(e.to_string());
                std::ptr::null_mut()
            }
        },
        Err(e) => {
            set_error(e.0);
            std::ptr::null_mut()
        }
    }
}

/// Save an identity JSON payload (same schema as the load output).
#[no_mangle]
pub extern "C" fn hl2box_identity_save(
    engine_root: *const c_char,
    mod_dir: *const c_char,
    json: *const c_char,
) -> i32 {
    let paths = make_paths(engine_root, mod_dir);
    let id: Identity = try_i!(serde_json::from_str(&arg(json)));
    try_i!(id.save(&paths));
    0
}

/// Install a picture file as the avatar (converted/stored as PNG).
#[no_mangle]
pub extern "C" fn hl2box_identity_set_avatar(
    engine_root: *const c_char,
    mod_dir: *const c_char,
    image_path: *const c_char,
) -> i32 {
    let paths = make_paths(engine_root, mod_dir);
    let mut id = try_i!(Identity::load(&paths));
    try_i!(id.install_avatar(&paths, &PathBuf::from(arg(image_path).as_ref())));
    try_i!(id.save(&paths));
    0
}

// ---------------------------------------------------------------------------
// SteamID
// ---------------------------------------------------------------------------

/// Random individual SteamID64 as a decimal string (heap).
#[no_mangle]
pub extern "C" fn hl2box_steamid_random() -> *mut c_char {
    heap_str(SteamId64::random().render())
}

/// Every form in one JSON object: id64, steam2, uform, account_id.
#[no_mangle]
pub extern "C" fn hl2box_steamid_parse(input: *const c_char) -> *mut c_char {
    let id = try_p!(SteamId64::parse(&arg(input)));
    let json = format!(
        "{{\"id64\":\"{}\",\"steam2\":\"{}\",\"uform\":\"{}\",\"account_id\":{}}}",
        id.render(),
        id.steam2(),
        id.u_form(),
        id.account_id()
    );
    heap_str(json)
}

// ---------------------------------------------------------------------------
// Addons
// ---------------------------------------------------------------------------

/// List addons as a JSON array (heap), or NULL on failure.
#[no_mangle]
pub extern "C" fn hl2box_addons_list(
    engine_root: *const c_char,
    mod_dir: *const c_char,
) -> *mut c_char {
    let paths = make_paths(engine_root, mod_dir);
    let list = try_p!(addons::scan(&paths));
    // jpeg bytes are not part of the FFI payload
    let list: Vec<&AddonEntry> = list.iter().collect();
    match serde_json::to_string(&list) {
        Ok(s) => heap_str(s),
        Err(e) => {
            set_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn hl2box_addon_set_enabled(
    engine_root: *const c_char,
    mod_dir: *const c_char,
    name: *const c_char,
    enabled: bool,
) -> i32 {
    let paths = make_paths(engine_root, mod_dir);
    try_i!(addons::set_enabled(&paths, &arg(name), enabled));
    0
}

/// Import a .gma or a folder into the addons directory.
#[no_mangle]
pub extern "C" fn hl2box_addon_import(
    engine_root: *const c_char,
    mod_dir: *const c_char,
    source: *const c_char,
) -> *mut c_char {
    let paths = make_paths(engine_root, mod_dir);
    let name = try_p!(addons::import(&paths, &PathBuf::from(arg(source).as_ref())));
    heap_str(name)
}

#[no_mangle]
pub extern "C" fn hl2box_addon_remove(
    engine_root: *const c_char,
    mod_dir: *const c_char,
    name: *const c_char,
) -> i32 {
    let paths = make_paths(engine_root, mod_dir);
    try_i!(addons::remove(&paths, &arg(name)));
    0
}

/// Parse a .gma header (name, author, entries, sizes) as JSON.
#[no_mangle]
pub extern "C" fn hl2box_gma_info(path: *const c_char) -> *mut c_char {
    let info = try_p!(gma::parse(&PathBuf::from(arg(path).as_ref())));
    let no_jpeg = GmaInfo {
        jpeg: None,
        ..info
    };
    match serde_json::to_string(&no_jpeg) {
        Ok(s) => heap_str(s),
        Err(e) => {
            set_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Mounts
// ---------------------------------------------------------------------------

/// Available source games + their mounted/managed state as a JSON array.
#[no_mangle]
pub extern "C" fn hl2box_mounts_list(
    engine_root: *const c_char,
    mod_dir: *const c_char,
) -> *mut c_char {
    let paths = make_paths(engine_root, mod_dir);
    let games = try_p!(gameinfo::available_games(&paths));
    let games: Vec<&MountableGame> = games.iter().collect();
    match serde_json::to_string(&games) {
        Ok(s) => heap_str(s),
        Err(e) => {
            set_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// `selected_json` is a JSON array of directory names to mount.
#[no_mangle]
pub extern "C" fn hl2box_mounts_apply(
    engine_root: *const c_char,
    mod_dir: *const c_char,
    selected_json: *const c_char,
) -> i32 {
    let paths = make_paths(engine_root, mod_dir);
    let selected: Vec<String> = try_i!(serde_json::from_str(&arg(selected_json)));
    try_i!(gameinfo::apply_mounts(&paths, &selected));
    0
}

// ---------------------------------------------------------------------------
// Launch
// ---------------------------------------------------------------------------

/// Full command line (program + args) as one string (heap).
#[no_mangle]
pub extern "C" fn hl2box_launch_command_line(
    engine_root: *const c_char,
    mod_dir: *const c_char,
    extra_args: *const c_char,
    identity_json: *const c_char,
) -> *mut c_char {
    let mut cfg = LauncherConfig {
        engine_root: arg(engine_root).into_owned(),
        mod_dir: arg(mod_dir).into_owned(),
        extra_args: arg(extra_args).into_owned(),
        ..Default::default()
    };
    cfg.pass_name = true;
    cfg.pass_steamid = true;
    let id: Identity = if identity_json.is_null() {
        Identity::default()
    } else {
        try_p!(serde_json::from_str(&arg(identity_json)))
    };
    heap_str(hl2box_core::config::command_line(&cfg, &id))
}

/// Start the game now. Returns the OS pid, or -1.
#[no_mangle]
pub extern "C" fn hl2box_launch_start(
    engine_root: *const c_char,
    mod_dir: *const c_char,
    extra_args: *const c_char,
    identity_json: *const c_char,
) -> i32 {
    let cfg = LauncherConfig {
        engine_root: arg(engine_root).into_owned(),
        mod_dir: arg(mod_dir).into_owned(),
        extra_args: arg(extra_args).into_owned(),
        ..Default::default()
    };
    let id: Identity = if identity_json.is_null() {
        Identity::default()
    } else {
        try_i!(serde_json::from_str(&arg(identity_json)))
    };
    let child = try_i!(hl2box_core::config::start_game(&cfg, &id));
    child.id() as i32
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

/// Tail a log file as JSON: {"lines":[...]} (heap). max_bytes caps the read.
#[no_mangle]
pub extern "C" fn hl2box_log_tail(
    path: *const c_char,
    max_bytes: u32,
) -> *mut c_char {
    let lines = hl2box_core::logs::tail(
        &PathBuf::from(arg(path).as_ref()),
        max_bytes.max(4096) as u64,
    );
    match serde_json::to_string(&lines) {
        Ok(s) => heap_str(s),
        Err(e) => {
            set_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Workshop (net feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "net")]
#[no_mangle]
pub extern "C" fn hl2box_workshop_details(id_or_url: *const c_char) -> *mut c_char {
    let id = try_p!(hl2box_core::workshop::extract_workshop_id(&arg(id_or_url)));
    let details = try_p!(hl2box_core::workshop::fetch_details(&id));
    let details = hl2box_core::workshop::WorkshopDetails {
        file_url: String::new(), // keep the CDN url out of cross-process payloads
        ..details
    };
    match serde_json::to_string(&details) {
        Ok(s) => heap_str(s),
        Err(e) => {
            set_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

#[cfg(feature = "net")]
#[no_mangle]
pub extern "C" fn hl2box_workshop_download(
    engine_root: *const c_char,
    mod_dir: *const c_char,
    id_or_url: *const c_char,
) -> *mut c_char {
    let paths = make_paths(engine_root, mod_dir);
    let id = try_p!(hl2box_core::workshop::extract_workshop_id(&arg(id_or_url)));
    let details = try_p!(hl2box_core::workshop::fetch_details(&id));
    let dest = gameinfo::unique_addon_path(&paths, &id);
    try_p!(hl2box_core::workshop::download_to(&details, &dest, |_, _| {}));
    heap_str(dest.to_string_lossy().into_owned())
}

// Keep identity module linked even though only some entry points use it.
#[allow(unused_imports)]
use identity as _identity_anchor;
