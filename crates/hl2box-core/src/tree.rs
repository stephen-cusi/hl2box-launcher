//! Directory tree model for the addon browser.
//!
//! The same model serves two sources: a folder addon walked from disk, and a
//! `.gma` archive whose index is already parsed. Both end up as one sorted
//! tree of forward-slash paths, so the UI does not care where the bytes live.

use std::collections::BTreeMap;
use std::path::Path;

use crate::Result;

/// Depth and count caps for walking a folder addon. A pathological tree (or a
/// symlink loop, which is never followed) cannot hang the UI.
const MAX_DEPTH: usize = 24;
const MAX_ENTRIES: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    /// Leaf name ("" for the root).
    pub name: String,
    /// Full path relative to the addon root, forward slashes ("" for the root).
    pub path: String,
    pub is_dir: bool,
    /// File size, or the sum of everything below for a directory.
    pub size: u64,
    /// Directories first, then names, both case-insensitive.
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn root() -> TreeNode {
        TreeNode {
            name: String::new(),
            path: String::new(),
            is_dir: true,
            size: 0,
            children: Vec::new(),
        }
    }

    /// Node at a relative path ("" or "/" is the root itself).
    pub fn find(&self, rel: &str) -> Option<&TreeNode> {
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            return Some(self);
        }
        let mut cur = self;
        for seg in rel.split('/') {
            cur = cur.children.iter().find(|c| c.name == seg)?;
        }
        Some(cur)
    }

    /// Number of files at or below this node.
    pub fn file_count(&self) -> usize {
        if self.is_dir {
            self.children.iter().map(TreeNode::file_count).sum()
        } else {
            1
        }
    }

    /// Every file path below this node, in tree order. Used for Lua listings.
    pub fn files(&self) -> Vec<&TreeNode> {
        let mut out = Vec::new();
        self.collect_files(&mut out);
        out
    }

    fn collect_files<'a>(&'a self, out: &mut Vec<&'a TreeNode>) {
        if self.is_dir {
            for c in &self.children {
                c.collect_files(out);
            }
        } else {
            out.push(self);
        }
    }
}

#[derive(Default)]
struct Build {
    file_size: Option<u64>,
    kids: BTreeMap<String, Build>,
}

/// Normalise one archive/disk path into segments. Backslashes become slashes,
/// empty and "." segments vanish, and any ".." segment makes the whole path
/// invalid (it is dropped entirely rather than resolved).
pub fn split_path(raw: &str) -> Option<Vec<String>> {
    let norm = raw.replace('\\', "/");
    let mut out = Vec::new();
    for seg in norm.split('/') {
        match seg {
            "" | "." => continue,
            ".." => return None,
            s => out.push(s.to_string()),
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Build a tree from `(path, size)` pairs. Invalid paths are skipped; a path
/// that appears twice keeps the larger size.
pub fn from_files<'a, I>(files: I) -> TreeNode
where
    I: IntoIterator<Item = (&'a str, u64)>,
{
    let mut root = Build::default();
    for (raw, size) in files {
        let Some(segs) = split_path(raw) else { continue };
        let mut cur = &mut root;
        let last = segs.len() - 1;
        for (i, seg) in segs.iter().enumerate() {
            cur = cur.kids.entry(seg.clone()).or_default();
            if i == last {
                let prev = cur.file_size.unwrap_or(0);
                cur.file_size = Some(prev.max(size));
            }
        }
    }
    finish("", "", root)
}

fn finish(name: &str, parent: &str, b: Build) -> TreeNode {
    let path = match (parent.is_empty(), name.is_empty()) {
        (_, true) => String::new(),
        (true, false) => name.to_string(),
        (false, false) => format!("{parent}/{name}"),
    };
    let is_dir = b.file_size.is_none() || !b.kids.is_empty();
    let mut children: Vec<TreeNode> = b
        .kids
        .into_iter()
        .map(|(k, v)| finish(&k, &path, v))
        .collect();
    children.sort_by(|a, c| {
        c.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&c.name.to_lowercase()))
    });
    let size = if is_dir {
        children.iter().map(|c| c.size).sum()
    } else {
        b.file_size.unwrap_or(0)
    };
    TreeNode {
        name: name.to_string(),
        path,
        is_dir,
        size,
        children,
    }
}

/// Walk a folder addon from disk. Symlinks are reported but not followed.
pub fn scan_dir(root: &Path) -> Result<TreeNode> {
    let mut found: Vec<(String, u64)> = Vec::new();
    walk(root, root, 0, &mut found)?;
    let owned: Vec<(&str, u64)> = found.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    Ok(from_files(owned))
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, u64)>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    for ent in std::fs::read_dir(dir)? {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let Ok(ent) = ent else { continue };
        let name = ent.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = ent.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk(root, &path, depth + 1, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, meta.len()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_nested_sorted_tree() {
        let t = from_files([
            ("lua/autorun/a.lua", 10),
            ("lua/autorun/server/b.lua", 5),
            ("materials/x.vmt", 7),
            ("readme.txt", 3),
        ]);
        assert!(t.is_dir);
        assert_eq!(t.size, 25);
        assert_eq!(t.file_count(), 4);
        // dirs first, then files, both alphabetical
        let names: Vec<&str> = t.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["lua", "materials", "readme.txt"]);
        let autorun = t.find("lua/autorun").unwrap();
        assert_eq!(autorun.path, "lua/autorun");
        assert_eq!(autorun.size, 10 + 5);
        assert_eq!(t.find("lua/autorun/a.lua").unwrap().size, 10);
    }

    #[test]
    fn normalises_and_rejects_traversal() {
        let t = from_files([
            ("./lua\\x.lua", 1),
            ("lua//y.lua", 2),
            ("../escape.lua", 9),
            ("lua/../../nope.lua", 9),
            ("", 4),
        ]);
        assert_eq!(t.file_count(), 2);
        assert!(t.find("lua/x.lua").is_some());
        assert!(t.find("lua/y.lua").is_some());
        assert!(t.find("escape.lua").is_none());
    }

    #[test]
    fn duplicate_keeps_larger() {
        let t = from_files([("a.lua", 3), ("a.lua", 8)]);
        assert_eq!(t.find("a.lua").unwrap().size, 8);
    }

    #[test]
    fn find_root_and_missing() {
        let t = from_files([("a/b.lua", 1)]);
        assert_eq!(t.find("").unwrap().path, "");
        assert_eq!(t.find("/").unwrap().path, "");
        assert!(t.find("a/zzz").is_none());
    }

    #[test]
    fn scan_dir_walks_and_skips_hidden() {
        let base = std::env::temp_dir().join("hl2box_tree_scan_test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("lua/autorun")).unwrap();
        std::fs::create_dir_all(base.join(".git")).unwrap();
        std::fs::write(base.join("lua/autorun/hi.lua"), b"print(1)").unwrap();
        std::fs::write(base.join(".git/config"), b"x").unwrap();
        let t = scan_dir(&base).unwrap();
        assert_eq!(t.file_count(), 1);
        assert_eq!(t.find("lua/autorun/hi.lua").unwrap().size, 8);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn files_lists_only_leaves() {
        let t = from_files([("a/b.lua", 1), ("a/c.txt", 2)]);
        let paths: Vec<&str> = t.files().iter().map(|n| n.path.as_str()).collect();
        assert_eq!(paths, ["a/b.lua", "a/c.txt"]);
    }
}
