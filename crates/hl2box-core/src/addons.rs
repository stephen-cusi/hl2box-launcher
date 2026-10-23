//! Addon management over `<mod>/addons/` and `addons_disabled.txt`.
//!
//! The engine honours the disable list for both folder addons and .gma
//! archives, so "disable" here means exactly "write the name into the
//! list" and "enable" means "take it out". The list format: one name per
//! line, '#' or ';' comments, case-insensitive match.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::gma::{self, GmaInfo};
use crate::paths::Paths;
use crate::{err, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AddonKind {
    Folder,
    Gma,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonEntry {
    /// The name the engine knows it by: folder name or file stem of the
    /// .gma ("foo" for addons/foo.gma). This is the disable-list key.
    pub name: String,
    pub kind: AddonKind,
    pub enabled: bool,
    /// Bytes on disk (folder size or file size).
    pub disk_size: u64,
    #[serde(skip)]
    pub info: Option<GmaInfo>,
}

impl AddonEntry {
    /// Human title: the addon's own name when the gma carries one, else
    /// the folder/file name.
    pub fn title(&self) -> &str {
        match &self.info {
            Some(g) if !g.name.is_empty() => &g.name,
            _ => &self.name,
        }
    }

    pub fn author(&self) -> Option<&str> {
        self.info
            .as_ref()
            .map(|g| g.author.as_str())
            .filter(|a| !a.is_empty())
    }

    pub fn file_count(&self) -> usize {
        self.info.as_ref().map(|g| g.entries.len()).unwrap_or(0)
    }
}

fn read_disabled_list(path: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(out);
    };
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
            continue;
        }
        out.push(t.to_ascii_lowercase());
    }
    Ok(out)
}

fn write_disabled_list(path: &Path, names: &[String]) -> Result<()> {
    let mut text = String::new();
    text.push_str("# hl2box launcher: one addon name per line, disabled addons are not mounted\n");
    let mut sorted: Vec<&String> = names.iter().collect();
    sorted.sort();
    for n in sorted {
        text.push_str(n);
        text.push('\n');
    }
    std::fs::write(path, text)?;
    Ok(())
}

/// List every addon in the addons directory. Folders count as Folder,
/// *.gma files as Gma (with header parsed for the info pane). Names that
/// appear twice (folder + gma) stay two entries, matching the engine.
pub fn scan(paths: &Paths) -> Result<Vec<AddonEntry>> {
    let dir = paths.addons_dir();
    std::fs::create_dir_all(&dir)?;
    let disabled = read_disabled_list(&paths.addons_disabled_txt())?;

    let mut out = Vec::new();
    let mut rd = std::fs::read_dir(&dir)?;
    while let Some(ent) = rd.next() {
        let ent = match ent {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = ent.path();
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if file_name.starts_with('.') {
            continue;
        }
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if meta.is_dir() {
            out.push(AddonEntry {
                enabled: !disabled.contains(&file_name.to_ascii_lowercase()),
                kind: AddonKind::Folder,
                disk_size: dir_size(&path),
                name: file_name,
                info: None,
            });
        } else if file_name.to_ascii_lowercase().ends_with(".gma") {
            let stem = file_name
                .strip_suffix(".gma")
                .or_else(|| file_name.strip_suffix(".GMA"))
                .unwrap_or(&file_name)
                .to_string();
            let info = gma::parse(&path).ok();
            out.push(AddonEntry {
                enabled: !disabled.contains(&stem.to_ascii_lowercase()),
                kind: AddonKind::Gma,
                disk_size: meta.len(),
                name: stem,
                info,
            });
        }
    }
    out.sort_by(|a, b| a.title().to_lowercase().cmp(&b.title().to_lowercase()));
    Ok(out)
}

/// Enable or disable one addon by name (the entry's `name` field).
pub fn set_enabled(paths: &Paths, name: &str, enabled: bool) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
    {
        return err("invalid addon name");
    }
    let list_path = paths.addons_disabled_txt();
    let mut list = read_disabled_list(&list_path)?;
    let key = name.to_ascii_lowercase();
    list.retain(|n| n != &key);
    if !enabled {
        list.push(key);
    }
    write_disabled_list(&list_path, &list)
}

/// Enable or disable every listed addon at once (one write).
pub fn set_all_enabled(paths: &Paths, names: &[&str], enabled: bool) -> Result<()> {
    let list_path = paths.addons_disabled_txt();
    let mut list = read_disabled_list(&list_path)?;
    for name in names {
        let key = name.to_ascii_lowercase();
        list.retain(|n| n != &key);
        if !enabled {
            list.push(key);
        }
    }
    write_disabled_list(&list_path, &list)
}

/// Install a .gma (or a folder addon packed as .zip) from anywhere on disk
/// into the addons directory. Existing files with the same name are
/// replaced. Returns the installed addon name.
pub fn import(paths: &Paths, source: &Path) -> Result<String> {
    let dir = paths.addons_dir();
    std::fs::create_dir_all(&dir)?;
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .ok_or_else(|| crate::Error("import: source has no file name".into()))?;

    if name.to_ascii_lowercase().ends_with(".gma") {
        // Sanity: must parse as gma before we accept it.
        gma::parse(source)?;
        std::fs::copy(source, dir.join(name))?;
        return Ok(name
            .strip_suffix(".gma")
            .or_else(|| name.strip_suffix(".GMA"))
            .unwrap_or(name)
            .to_string());
    }

    if name.to_ascii_lowercase().ends_with(".zip") {
        return err("import: zip addons are not supported, pack a .gma (or drop the folder in addons/)");
    }

    // A folder: copy recursively.
    let src = source.to_path_buf();
    if !src.is_dir() {
        return err("import: expected a .gma file or a folder");
    }
    copy_dir(&src, &dir.join(name))?;
    Ok(name.to_string())
}

/// Remove an addon from disk (folder or .gma). The disable entry, if any,
/// is dropped too. This deletes files: callers should confirm.
pub fn remove(paths: &Paths, name: &str) -> Result<()> {
    let dir = paths.addons_dir();
    let folder = dir.join(name);
    let gma = dir.join(format!("{name}.gma"));
    if folder.is_dir() {
        std::fs::remove_dir_all(&folder)?;
    } else if gma.is_file() {
        std::fs::remove_file(&gma)?;
    } else {
        return err(format!("remove: no such addon: {name}"));
    }
    set_enabled(paths, name, true)
}

pub fn open_in_file_manager(path: &Path) {
    // Open the folder itself, as an absolute path: a relative or
    // verbatim (\\?\) form confuses some file managers and shells.
    let target = crate::paths::absolute(path);
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(&target).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&target).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&target).spawn();
    }
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(path) {
        let mut stack: Vec<std::path::PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        while let Some(p) = stack.pop() {
            if let Ok(m) = std::fs::symlink_metadata(&p) {
                if m.is_dir() {
                    if let Ok(rd) = std::fs::read_dir(&p) {
                        stack.extend(rd.filter_map(|e| e.ok()).map(|e| e.path()));
                    }
                } else {
                    total += m.len();
                }
            }
        }
    }
    total
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for ent in std::fs::read_dir(src)? {
        let ent = ent?;
        let from = ent.path();
        let to = dst.join(ent.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Read one file from inside an addon, for the text viewer.
///
/// `rel` is a path relative to the addon root (forward or back slashes).
/// Refused: `..` segments, absolute paths, symlinks that resolve outside the
/// addon folder, missing entries, and anything larger than `max` bytes.
pub fn read_file(paths: &Paths, addon: &AddonEntry, rel: &str, max: u64) -> Result<Vec<u8>> {
    let segs = crate::tree::split_path(rel)
        .ok_or_else(|| crate::Error(format!("invalid path: {rel}")))?;
    let norm = segs.join("/");
    match addon.kind {
        AddonKind::Folder => {
            let root = paths.addons_dir().join(&addon.name);
            let mut target = root.clone();
            for seg in &segs {
                target.push(seg);
            }
            let canon_root = root.canonicalize()?;
            let canon = target.canonicalize()?;
            if !canon.starts_with(&canon_root) {
                return err(format!("path escapes the addon: {norm}"));
            }
            let len = std::fs::metadata(&canon)?.len();
            if len > max {
                return err(format!("{norm} is {len} bytes, over the {max} byte limit"));
            }
            Ok(std::fs::read(&canon)?)
        }
        AddonKind::Gma => {
            let info = addon
                .info
                .as_ref()
                .ok_or_else(|| crate::Error("archive index not readable".into()))?;
            let entry = info
                .find(&norm)
                .ok_or_else(|| crate::Error(format!("not in archive: {norm}")))?;
            if entry.size > max {
                return err(format!("{norm} is {} bytes, over the {max} byte limit", entry.size));
            }
            let lower = paths.addons_dir().join(format!("{}.gma", addon.name));
            let upper = paths.addons_dir().join(format!("{}.GMA", addon.name));
            let gma_path = if lower.is_file() { lower } else { upper };
            Ok(gma::read_entry_from_path(&gma_path, entry)?)
        }
    }
}

#[cfg(test)]
mod read_tests {
    use super::*;

    fn folder_addon(base: &std::path::Path) -> (Paths, AddonEntry) {
        let eng = base.join("eng");
        let paths = Paths::new(&eng, "hl2sb");
        let addon_dir = paths.addons_dir().join("demo");
        std::fs::create_dir_all(addon_dir.join("lua/autorun")).unwrap();
        std::fs::write(addon_dir.join("lua/autorun/hi.lua"), b"print('hi')").unwrap();
        let entry = AddonEntry {
            name: "demo".into(),
            kind: AddonKind::Folder,
            enabled: true,
            disk_size: 0,
            info: None,
        };
        (paths, entry)
    }

    #[test]
    fn reads_inside_and_refuses_traversal() {
        let base = std::env::temp_dir().join("hl2box_read_file_test");
        let _ = std::fs::remove_dir_all(&base);
        let (paths, entry) = folder_addon(&base);
        let ok = read_file(&paths, &entry, "lua/autorun/hi.lua", 1024).unwrap();
        assert_eq!(ok, b"print('hi')");
        assert!(read_file(&paths, &entry, "../demo/lua/autorun/hi.lua", 1024).is_err());
        assert!(read_file(&paths, &entry, "/etc/passwd", 1024).is_err());
        assert!(read_file(&paths, &entry, "lua/autorun/missing.lua", 1024).is_err());
        assert!(read_file(&paths, &entry, "lua/autorun/hi.lua", 3).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
