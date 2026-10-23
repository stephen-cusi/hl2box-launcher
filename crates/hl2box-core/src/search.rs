//! Full-text search across the Lua files of every addon.
//!
//! Folder addons are walked from disk and archives are read through the same
//! bounded reader the viewer uses, so a search can never pull in a binary or
//! an oversized file. Results are capped so a broad query stays cheap.

use crate::addons::{self, AddonEntry, AddonKind};
use crate::lua;
use crate::paths::Paths;
use crate::preview;
use crate::tree::{self, TreeNode};

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub addon: String,
    /// Path inside the addon, forward slashes.
    pub path: String,
    /// 1-based line number.
    pub line: usize,
    /// The matching line, trimmed and cut to 200 characters.
    pub text: String,
}

/// Case-insensitive substring search over Lua files. Empty queries return
/// nothing. Stops as soon as `max_hits` results are collected.
pub fn search_lua(paths: &Paths, addon_list: &[AddonEntry], query: &str, max_hits: usize) -> Vec<Hit> {
    let q = query.trim().to_lowercase();
    let mut out = Vec::new();
    if q.is_empty() || max_hits == 0 {
        return out;
    }
    for addon in addon_list {
        let root = match addon.kind {
            AddonKind::Folder => {
                tree::scan_dir(&paths.addons_dir().join(&addon.name)).unwrap_or_else(|_| TreeNode::root())
            }
            AddonKind::Gma => match &addon.info {
                Some(info) => tree::from_files(info.entries.iter().map(|e| (e.path.as_str(), e.size))),
                None => continue,
            },
        };
        for file in root.files() {
            if !lua::is_lua(&file.path) {
                continue;
            }
            let Ok(bytes) = addons::read_file(paths, addon, &file.path, preview::MAX_PREVIEW_BYTES) else {
                continue;
            };
            if !preview::looks_like_text(&bytes) {
                continue;
            }
            let text = preview::decode_text(&bytes);
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&q) {
                    out.push(Hit {
                        addon: addon.name.clone(),
                        path: file.path.clone(),
                        line: i + 1,
                        text: line.trim().chars().take(200).collect(),
                    });
                    if out.len() >= max_hits {
                        return out;
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_lines_in_lua_only() {
        let base = std::env::temp_dir().join("hl2box_search_test");
        let _ = std::fs::remove_dir_all(&base);
        let paths = Paths::new(&base, "hl2sb");
        let dir = paths.addons_dir().join("demo");
        std::fs::create_dir_all(dir.join("lua")).unwrap();
        std::fs::write(dir.join("lua").join("a.lua"), "local x = 1\nprint('Hello')\n-- hello again\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "hello in a txt file\n").unwrap();
        let addon = AddonEntry {
            name: "demo".into(),
            kind: AddonKind::Folder,
            enabled: true,
            disk_size: 0,
            info: None,
        };
        let hits = search_lua(&paths, std::slice::from_ref(&addon), "HELLO", 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "lua/a.lua");
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[1].line, 3);
        assert!(search_lua(&paths, std::slice::from_ref(&addon), "   ", 10).is_empty());
        assert_eq!(search_lua(&paths, std::slice::from_ref(&addon), "hello", 1).len(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }
}
