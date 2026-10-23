//! Filesystem layout of an hl2box install.
//!
//! The desktop layout (what the CI artifacts unpack to) is:
//!
//! ```text
//! <engine root>/            hl2_launcher(.exe), bin/, hl2/, hl2sb/ ...
//! <engine root>/hl2sb/      the mod: gameinfo.txt, addons/, cfg/, data/ ...
//! ```
//!
//! The same layout is used on Linux and macOS, only the executable suffix
//! changes. On Android the game data lives under the app's files directory
//! and the paths are supplied by the caller.

use std::path::{Path, PathBuf};

use crate::Result;

#[derive(Debug, Clone)]
pub struct Paths {
    /// Directory that contains hl2_launcher and the engine binaries.
    pub engine_root: PathBuf,
    /// Mod directory name relative to the engine root, e.g. "hl2sb".
    pub mod_dir: String,
}

impl Paths {
    pub fn new(engine_root: impl Into<PathBuf>, mod_dir: impl Into<String>) -> Self {
        Paths {
            engine_root: engine_root.into(),
            mod_dir: mod_dir.into(),
        }
    }

    /// The mod directory, `<engine root>/<mod dir>`.
    pub fn mod_root(&self) -> PathBuf {
        self.engine_root.join(&self.mod_dir)
    }

    pub fn addons_dir(&self) -> PathBuf {
        self.mod_root().join("addons")
    }

    pub fn addons_disabled_txt(&self) -> PathBuf {
        self.mod_root().join("addons_disabled.txt")
    }

    pub fn gameinfo_txt(&self) -> PathBuf {
        self.mod_root().join("gameinfo.txt")
    }

    /// Player-writable data directory the game already uses (`file.Write`
    /// defaults there). The launcher keeps its identity under `hl2box/`.
    pub fn data_dir(&self) -> PathBuf {
        self.mod_root().join("data").join("hl2box")
    }

    /// The game's avatar store: `data/avatars/`. The in-game AvatarImage
    /// control looks files up there by steamid64 first, then by player
    /// name (both run through [`avatar_key`]), and falls back to a
    /// bundled default. A plain PNG is drawn stretched over the panel.
    pub fn avatars_dir(&self) -> PathBuf {
        self.mod_root().join("data").join("avatars")
    }

    /// Avatar file for a lookup key (steamid64 or player name).
    pub fn avatar_png_for(&self, key: &str) -> PathBuf {
        self.avatars_dir().join(format!("{}.png", avatar_key(key)))
    }

    /// Path of the game executable for the current platform.
    pub fn game_exe(&self) -> PathBuf {
        let name = if cfg!(windows) {
            "hl2_launcher.exe"
        } else {
            "hl2_launcher"
        };
        self.engine_root.join(name)
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(self.addons_dir())?;
        std::fs::create_dir_all(self.data_dir())?;
        std::fs::create_dir_all(self.avatars_dir())?;
        Ok(())
    }
}

/// Filesystem-safe avatar key, the same rule the game's AvatarImage
/// applies to player names and steam ids: keep alphanumerics, '-' and '_',
/// drop everything else.
pub fn avatar_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Quick sanity check used by the UI to warn about a wrong root:
/// the engine root should contain the game executable and the mod dir.
pub fn install_looks_valid(paths: &Paths) -> bool {
    paths.game_exe().is_file() && paths.gameinfo_txt().is_file()
}

/// Normalize a user-supplied path: strip quotes and surrounding whitespace
/// (drag-and-drop from Windows Explorer often includes quotes).
pub fn clean_path(input: &str) -> PathBuf {
    let t = input.trim();
    let t = t.trim_matches('"');
    Path::new(t).to_path_buf()
}

/// Absolute form of a path without the `\\?\` verbatim prefix Windows'
/// canonicalize adds: relative inputs resolve against the current
/// directory. File managers and the shell behave better with a plain
/// absolute path.
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return strip_verbatim(path);
    }
    let joined = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(path);
    strip_verbatim(&joined)
}

fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}
