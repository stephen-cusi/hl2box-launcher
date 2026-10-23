//! Reading and writing the game's own `cfg/config.cfg`.
//!
//! The engine archives `name` into config.cfg and rewrites the file on
//! exit, so the launcher treats that file - not its own identity.json -
//! as the source of truth for the player name: the Player page prefills
//! from it, "apply name" writes it back there, and the avatar filename
//! keys follow it. Editing while the game runs loses the race to the
//! exit-time writeback; the UI says so.

use std::path::{Path, PathBuf};

use crate::paths::Paths;
use crate::Result;

pub fn config_cfg(paths: &Paths) -> PathBuf {
    paths.mod_root().join("cfg").join("config.cfg")
}

/// Pull the archived `name` convar out of config.cfg. Accepts the
/// `name "smith"` and `seta name "smith"` spellings; the last match wins
/// (the engine appends on writeback).
pub fn read_name(paths: &Paths) -> Option<String> {
    let text = std::fs::read_to_string(config_cfg(paths)).ok()?;
    let mut found = None;
    for line in text.lines() {
        let t = line.trim();
        let t = t.strip_prefix("seta ").unwrap_or(t);
        let Some(rest) = t.strip_prefix("name ") else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() {
            continue;
        }
        let value = rest.trim_matches('"').trim();
        if !value.is_empty() {
            found = Some(value.to_string());
        }
    }
    found
}

/// Write the name back into config.cfg, preserving every other line and
/// the file's line endings. Existing `name` / `seta name` lines are
/// replaced (only the first - duplicates confuse nothing, but one is
/// cleaner); with no existing line the setting is appended.
pub fn write_name(paths: &Paths, name: &str) -> Result<()> {
    let path = config_cfg(paths);
    if !path.is_file() {
        // No config yet: create a minimal one the engine accepts.
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        std::fs::write(&path, format!("name \"{name}\"\n"))?;
        return Ok(());
    }
    let text = std::fs::read_to_string(&path)?;
    let crlf = text.contains("\r\n");
    let newline = if crlf { "\r\n" } else { "\n" };

    let mut replaced = false;
    let mut out_lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        let t = t.strip_prefix("seta ").unwrap_or(t);
        let is_name = t.starts_with("name ")
            && t["name ".len()..]
                .trim()
                .starts_with('"');
        if is_name && !replaced {
            out_lines.push(format!("name \"{name}\""));
            replaced = true;
        } else if !is_name {
            out_lines.push(line.to_string());
        }
    }
    if !replaced {
        out_lines.push(format!("name \"{name}\""));
    }

    let mut out = out_lines.join(newline);
    if text.ends_with('\n') {
        out.push_str(newline);
    }
    std::fs::write(&path, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    #[test]
    fn name_roundtrip_preserves_file() {
        let dir = std::env::temp_dir().join("hl2box_cfg_test");
        let mod_root = dir.join("hl2sb");
        std::fs::create_dir_all(mod_root.join("cfg")).unwrap();
        std::fs::write(
            mod_root.join("cfg").join("config.cfg"),
            "unbindall\r\nname \"old\"\r\nsensitivity 3\r\n",
        )
        .unwrap();

        let paths = Paths::new(&dir, "hl2sb");
        assert_eq!(read_name(&paths).as_deref(), Some("old"));
        write_name(&paths, "newname").unwrap();
        let text = std::fs::read_to_string(mod_root.join("cfg").join("config.cfg")).unwrap();
        assert!(text.contains("name \"newname\""));
        assert!(text.contains("unbindall"));
        assert!(text.contains("sensitivity 3"));
        assert!(text.matches("\r\n").count() >= 2);
        assert_eq!(read_name(&paths).as_deref(), Some("newname"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
