//! hl2box-core: shared launcher logic.
//!
//! Everything here is plain, GUI-free logic so the desktop launcher (eframe),
//! a future Android launcher and any test can share one implementation.
//!
//! Engine-side facts this crate builds on (verified against the hl2box tree):
//! - The game runs no-steam: the steam_api stub answers NULL for every
//!   interface, so player identity (name / steam id / avatar) has no Steam
//!   client to come from. The launcher owns it instead.
//! - Addon enable/disable is `addons_disabled.txt` in the game directory:
//!   one addon name per line, '#' and ';' comments, case-insensitive. Both
//!   the folder mounts and the .gma mounts honour the list.
//! - Mounting extra games is `gameinfo.txt` SearchPath lines; the launcher
//!   edits a marked block only and leaves hand-written lines untouched.

pub mod addons;
pub mod config;
pub mod gamecfg;
pub mod gameinfo;
pub mod gma;
pub mod identity;
pub mod launch;
pub mod logs;
pub mod maps;
pub mod search;
pub mod lua;
pub mod preview;
pub mod tree;
pub mod paths;
pub mod steamid;
pub mod version;

#[cfg(feature = "net")]
pub mod http;

#[cfg(feature = "net")]
pub mod workshop;

use std::fmt;

/// Error type shared by the whole crate. Kept as a plain string enum so it
/// can cross the C FFI boundary without panicking.
#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(format!("io: {e}"))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error(format!("json: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}
