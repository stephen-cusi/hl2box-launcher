//! gameinfo.txt SearchPath handling for mounting extra games.
//!
//! The launcher only ever edits a marked block inside SearchPaths:
//!
//! ```text
//! // hl2box-launcher mounts BEGIN
//! game    |all_source_engine_paths|portal/portal_pak.vpk
//! game    |all_source_engine_paths|portal
//! // hl2box-launcher mounts END
//! ```
//!
//! Everything outside the markers is preserved byte for byte, so hand
//! edits (and upstream comment conventions) survive every apply.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::{err, Result};

const MARK_BEGIN: &str = "// hl2box-launcher mounts BEGIN";
const MARK_END: &str = "// hl2box-launcher mounts END";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountableGame {
    /// Directory name under the engine root, e.g. "hl2", "episodic",
    /// "portal", "cstrike".
    pub dir: String,
    /// Vpk files of that game, in mount-friendly order (textures last is
    /// not needed; engine handles it).
    pub vpks: Vec<String>,
    /// True when the gameinfo of the mod already references this game
    /// (inside or outside the marker block).
    pub mounted: bool,
    /// True when the mount comes from the launcher block (removable by us).
    pub managed: bool,
}

/// Scan the engine root for game directories: a directory counts when it
/// contains a gameinfo.txt of its own or at least one .vpk archive.
/// Directories under the engine root that are never game content: engine
/// plumbing, tooling, caches, and the launcher's own output folders.
const NOT_A_GAME: [&str; 16] = [
    "bin", "bin_x64", "platform", "logs", "lua", "reslists", "tool", "cache", "custom", "dumps",
    "dumps_wer", "cfg", "lang", "maps", "addons", "sourceengine",
];

pub fn available_games(paths: &Paths) -> Result<Vec<MountableGame>> {
    let root = &paths.engine_root;
    let mut mounted = mounted_dirs(paths)?;
    let mod_dir = paths.mod_dir.to_ascii_lowercase();

    let mut out = Vec::new();
    for ent in std::fs::read_dir(root)? {
        let ent = ent?;
        let path = ent.path();
        // symlink_metadata: a link (e.g. garrysmod -> the Steam install) is
        // not followed, so stock games are never offered for mounting.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let lower = name.to_ascii_lowercase();
        // Plumbing, tooling, and the mod itself (plus copies such as hl2sb_clear).
        if NOT_A_GAME.contains(&lower.as_str())
            || name.starts_with('.')
            || lower == mod_dir
            || lower.starts_with(&format!("{mod_dir}_"))
        {
            continue;
        }
        if name == paths.mod_dir {
            continue;
        }

        let has_gameinfo = path.join("gameinfo.txt").is_file();
        let mut vpks = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&path) {
            for f in rd.flatten() {
                let fname = f.file_name().to_string_lossy().to_ascii_lowercase();
                if fname.ends_with(".vpk")
                    && !fname.ends_with("_360.vpk")
                    && !fname.ends_with(".bsp.vpk")
                {
                    if let Some(n) = f.file_name().to_str() {
                        vpks.push(n.to_string());
                    }
                }
            }
        }
        vpks.sort();
        if !has_gameinfo && vpks.is_empty() {
            continue;
        }

        let m = mounted.remove(&name.to_ascii_lowercase());
        out.push(MountableGame {
            dir: name,
            vpks,
            mounted: m.map(|x| x.0).unwrap_or(false),
            managed: m.map(|x| x.1).unwrap_or(false),
        });
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    Ok(out)
}

/// Set of game directory names referenced from the mod's gameinfo
/// SearchPaths, mapped to (mounted at all, mounted through the launcher
/// block). A hand-written mount line counts as mounted without being
/// launcher-managed, so the UI shows it ticked but not removable by us.
fn mounted_dirs(paths: &Paths) -> Result<std::collections::HashMap<String, (bool, bool)>> {
    let mut out: std::collections::HashMap<String, (bool, bool)> =
        std::collections::HashMap::new();
    let text = match std::fs::read_to_string(paths.gameinfo_txt()) {
        Ok(t) => t,
        Err(_) => return Ok(out),
    };
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with(MARK_BEGIN) {
            in_block = true;
            continue;
        }
        if t.starts_with(MARK_END) {
            in_block = false;
            continue;
        }
        if let Some(dir) = referenced_game_dir(t) {
            let entry = out.entry(dir).or_insert((false, false));
            entry.0 = true;
            if in_block {
                entry.1 = true;
            }
        }
    }
    Ok(out)
}

/// Pull the game directory out of a SearchPath line:
/// `game |all_source_engine_paths|portal/portal_pak.vpk` -> "portal".
/// Lines that do not reference another game (hl2sb, hl2/...) still count
/// only when they are not the mod itself; the mod dir is skipped by the
/// caller through `name == paths.mod_dir`.
fn referenced_game_dir(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with("//") {
        return None;
    }
    let token = line.split_whitespace().nth(1)?;
    let token = token.trim_matches('"');
    let tail = token.split("|all_source_engine_paths|").nth(1)?;
    let first = tail.split(['/', '\\']).next()?;
    if first.is_empty() {
        return None;
    }
    Some(first.to_ascii_lowercase())
}

/// Write the launcher mount block so that exactly `selected` games are
/// mounted. Every other line of gameinfo.txt is preserved as-is.
pub fn apply_mounts(paths: &Paths, selected: &[String]) -> Result<()> {
    let gameinfo = paths.gameinfo_txt();
    if !gameinfo.is_file() {
        return err("gameinfo.txt not found in the mod directory");
    }
    let text = std::fs::read_to_string(&gameinfo)?;

    let games = available_games(paths)?;
    for s in selected {
        if !games.iter().any(|g| g.dir.eq_ignore_ascii_case(s)) {
            return err(format!("apply_mounts: unknown game directory: {s}"));
        }
    }

    // Build the new block body.
    let mut body = Vec::new();
    let selected_set: BTreeSet<String> = selected.iter().map(|s| s.to_ascii_lowercase()).collect();
    for game in &games {
        if !selected_set.contains(&game.dir.to_ascii_lowercase()) {
            continue;
        }
        for vpk in &game.vpks {
            body.push(format!(
                "\t\t\tgame\t\t\t\t|all_source_engine_paths|{}/{}",
                game.dir, vpk
            ));
        }
        // Mount the bare directory too so loose files and the map list work.
        body.push(format!(
            "\t\t\tgame\t\t\t\t|all_source_engine_paths|{}",
            game.dir
        ));
    }

    let mut out = String::with_capacity(text.len() + 256);
    let mut in_block = false;
    let mut had_block = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with(MARK_BEGIN) {
            in_block = true;
            had_block = true;
            continue;
        }
        if t.starts_with(MARK_END) {
            in_block = false;
            // Emit the new block at the position of the old one.
            emit_block(&mut out, &body);
            out.push_str(MARK_END);
            out.push('\n');
            continue;
        }
        if in_block {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if in_block {
        return err("gameinfo.txt: mount block BEGIN without END");
    }
    if !had_block {
        // Insert the block at the end of SearchPaths (the closing brace of
        // SearchPaths is the last '}' before FileSystem's own '}').
        out = insert_block_before_searchpaths_close(&out, &body)?;
    }

    // gameinfo.txt in this tree uses CRLF; keep whatever the file already
    // uses so diffs stay clean.
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let normalized = if newline == "\r\n" {
        out.replace('\n', "\r\n")
    } else {
        out
    };
    std::fs::write(&gameinfo, normalized)?;
    Ok(())
}

fn emit_block(out: &mut String, body: &[String]) {
    out.push_str(MARK_BEGIN);
    out.push('\n');
    for b in body {
        out.push_str(b);
        out.push('\n');
    }
}

fn insert_block_before_searchpaths_close(text: &str, body: &[String]) -> Result<String> {
    // Find "SearchPaths" then the matching closing brace.
    let idx = text
        .find("SearchPaths")
        .ok_or_else(|| crate::Error("gameinfo.txt: no SearchPaths block".into()))?;
    let after = &text[idx..];
    let close = after
        .find('}')
        .ok_or_else(|| crate::Error("gameinfo.txt: SearchPaths never closes".into()))?;
    let abs_close = idx + close;

    let mut out = String::with_capacity(text.len() + 256);
    out.push_str(&text[..abs_close]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    emit_block(&mut out, body);
    out.push_str(MARK_END);
    out.push('\n');
    out.push_str(&text[abs_close..]);
    Ok(out)
}

/// Names currently mounted through the launcher block.
pub fn launcher_mounted(paths: &Paths) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(paths.gameinfo_txt())?;
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with(MARK_BEGIN) {
            in_block = true;
            continue;
        }
        if t.starts_with(MARK_END) {
            in_block = false;
            continue;
        }
        if in_block {
            if let Some(dir) = referenced_game_dir(t) {
                if !out.contains(&dir) {
                    out.push(dir);
                }
            }
        }
    }
    Ok(out)
}

/// Path of the next available file name for a workshop download, e.g.
/// addons/myaddon.gma, adding " (2)", " (3)" on collisions.
pub fn unique_addon_path(paths: &Paths, base: &str) -> PathBuf {
    let dir = paths.addons_dir();
    let stem = base.trim_end_matches(".gma");
    let mut p = dir.join(format!("{stem}.gma"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem} ({n}).gma"));
        n += 1;
    }
    p
}

#[cfg(test)]
mod game_listing_tests {
    use super::*;
    use crate::paths::Paths;

    /// A fixture shaped like the real install: real games (gameinfo or vpks),
    /// tooling and mod copies that must be ignored, and a symlinked stock game
    /// that must not be followed.
    fn fixture(root: &std::path::Path) {
        let mk = |p: &str| std::fs::create_dir_all(root.join(p)).unwrap();
        mk("hl2");
        std::fs::write(root.join("hl2/gameinfo.txt"), "GameInfo {}").unwrap();
        mk("portal");
        std::fs::write(root.join("portal/portal_pak.vpk"), b"vpk").unwrap();
        mk("tool/VPKEdit");
        std::fs::write(root.join("tool/VPKEdit/readme.txt"), b"x").unwrap();
        mk("dumps");
        std::fs::write(root.join("dumps/crash.mdmp"), b"x").unwrap();
        mk("hl2sb_clear");
        std::fs::write(root.join("hl2sb_clear/gameinfo.txt"), "GameInfo {}").unwrap();
        mk("bin");
        mk("hl2sb");
        std::fs::write(root.join("hl2sb/gameinfo.txt"), "GameInfo {}").unwrap();
        // the stock install lives outside the engine root; the root only links to it
        let stock = root.parent().unwrap().join("stock_garrysmod_install");
        std::fs::create_dir_all(&stock).unwrap();
        std::fs::write(stock.join("gameinfo.txt"), "GameInfo {}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&stock, root.join("garrysmod")).unwrap();
    }

    #[test]
    fn lists_only_real_games_and_never_follows_links() {
        let base = std::env::temp_dir().join("hl2box_game_listing_test");
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("srceng");
        std::fs::create_dir_all(&root).unwrap();
        fixture(&root);
        let paths = Paths::new(&root, "hl2sb");
        let games: Vec<String> = available_games(&paths).unwrap().into_iter().map(|g| g.dir).collect();
        assert_eq!(games, vec!["hl2", "portal"], "got {games:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
