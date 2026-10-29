//! Reading the directory of a Source VPK archive (version 1 and 2).
//!
//! Only the `*_dir.vpk` directory is read, which lists every file with its
//! size and archive index. File contents are never extracted, so a listing
//! costs one read of the directory. Format per the Valve Developer Community
//! "VPK (file format)" article: a header (signature, version, tree size, and
//! for version 2 four more fields), then a three-level tree of extension,
//! path and file name, each entry holding offset, length and the preload bytes
//! that follow it.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::{err, Result};

const VPK_SIGNATURE: u32 = 0x55AA_1234;
const ENTRY_TERMINATOR: u16 = 0xFFFF;
/// Archive index meaning "the data is in the directory file itself".
const ARCHIVE_IN_DIR: u16 = 0x7FFF;
/// Largest directory tree accepted (the biggest official ones are tens of MB).
const MAX_TREE_BYTES: u32 = 256 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpkEntry {
    /// Forward-slash path inside the archive, e.g. `lua/autorun/init.lua`.
    pub path: String,
    pub size: u32,
    /// Archive index, or `None` when the data lives in the directory file.
    pub archive: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpkListing {
    pub version: u32,
    pub entries: Vec<VpkEntry>,
    pub total_size: u64,
}

fn u16_le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Read a NUL-terminated string; returns the string and the position after it.
fn cstr(tree: &[u8], pos: usize) -> Result<(String, usize)> {
    let rest = tree.get(pos..).ok_or_else(|| crate::Error("vpk: tree truncated".into()))?;
    let end = rest
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| crate::Error("vpk: unterminated string".into()))?;
    let s = String::from_utf8_lossy(&rest[..end]).into_owned();
    Ok((s, pos + end + 1))
}

/// Header length for a version, or an error for an unknown version.
fn header_len(version: u32) -> Result<usize> {
    match version {
        1 => Ok(12),
        2 => Ok(28),
        v => err(format!("vpk: unsupported version {v}")),
    }
}

/// Parse a directory file's bytes. Pure function: the tests use it directly.
pub fn parse_directory(head: &[u8]) -> Result<VpkListing> {
    let sig = u32_le(head, 0).ok_or_else(|| crate::Error("vpk: file too short".into()))?;
    if sig != VPK_SIGNATURE {
        return err(format!("vpk: bad signature {sig:#010x}"));
    }
    let version = u32_le(head, 4).ok_or_else(|| crate::Error("vpk: file too short".into()))?;
    let hlen = header_len(version)?;
    let tree_size = u32_le(head, 8).ok_or_else(|| crate::Error("vpk: file too short".into()))?;
    if tree_size > MAX_TREE_BYTES {
        return err("vpk: directory tree is implausibly large");
    }
    let tree_end = hlen + tree_size as usize;
    let tree = head
        .get(hlen..tree_end)
        .ok_or_else(|| crate::Error("vpk: directory tree is cut off".into()))?;

    let mut entries = Vec::new();
    let mut total: u64 = 0;
    let mut pos = 0usize;
    loop {
        let (ext, p) = cstr(tree, pos)?;
        pos = p;
        if ext.is_empty() {
            break;
        }
        loop {
            let (dir, p) = cstr(tree, pos)?;
            pos = p;
            if dir.is_empty() {
                break;
            }
            loop {
                let (name, p) = cstr(tree, pos)?;
                pos = p;
                if name.is_empty() {
                    break;
                }
                // entry: crc u32, preload u16, archive u16, offset u32, length u32, terminator u16
                let e = tree
                    .get(pos..pos + 18)
                    .ok_or_else(|| crate::Error("vpk: entry cut off".into()))?;
                let preload = u16_le(e, 4).unwrap_or(0) as usize;
                let archive = u16_le(e, 6).unwrap_or(0);
                let length = u32_le(e, 12).unwrap_or(0);
                if u16_le(e, 16) != Some(ENTRY_TERMINATOR) {
                    return err("vpk: entry terminator missing");
                }
                pos += 18 + preload;

                let dir_part = if dir == " " { String::new() } else { format!("{dir}/") };
                let ext_part = if ext == " " || ext.is_empty() { String::new() } else { format!(".{ext}") };
                entries.push(VpkEntry {
                    path: format!("{dir_part}{name}{ext_part}"),
                    size: length,
                    archive: if archive == ARCHIVE_IN_DIR { None } else { Some(archive) },
                });
                total += length as u64;
            }
        }
    }
    entries.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
    Ok(VpkListing { version, entries, total_size: total })
}

/// Read and parse a `*_dir.vpk` file. Only the header and tree are read.
pub fn read_directory(path: &Path) -> Result<VpkListing> {
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 28];
    let n = f.read(&mut head)?;
    if n < 12 {
        return err("vpk: file too short");
    }
    let version = u32_le(&head, 4).unwrap_or(0);
    let hlen = header_len(version)?;
    let tree_size = u32_le(&head, 8).unwrap_or(0);
    if tree_size > MAX_TREE_BYTES {
        return err("vpk: directory tree is implausibly large");
    }
    f.seek(SeekFrom::Start(0))?;
    let mut buf = vec![0u8; hlen + tree_size as usize];
    f.read_exact(&mut buf)
        .map_err(|e| crate::Error(format!("vpk: reading directory: {e}")))?;
    parse_directory(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a version-1 directory: `<ext>\0 <path>\0 <name>\0 <entry> ... \0` nesting.
    fn build_v1(files: &[(&str, &str, &str, u32)]) -> Vec<u8> {
        let mut tree = Vec::new();
        let mut exts: Vec<&str> = files.iter().map(|f| f.0).collect();
        exts.sort();
        exts.dedup();
        for ext in exts {
            tree.extend_from_slice(ext.as_bytes());
            tree.push(0);
            let mut dirs: Vec<&str> = files.iter().filter(|f| f.0 == ext).map(|f| f.1).collect();
            dirs.sort();
            dirs.dedup();
            for dir in dirs {
                tree.extend_from_slice(dir.as_bytes());
                tree.push(0);
                for (_, _, name, len) in files.iter().filter(|f| f.0 == ext && f.1 == dir) {
                    tree.extend_from_slice(name.as_bytes());
                    tree.push(0);
                    tree.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // crc
                    tree.extend_from_slice(&0u16.to_le_bytes()); // preload
                    tree.extend_from_slice(&0u16.to_le_bytes()); // archive 0
                    tree.extend_from_slice(&0u32.to_le_bytes()); // offset
                    tree.extend_from_slice(&len.to_le_bytes());
                    tree.extend_from_slice(&ENTRY_TERMINATOR.to_le_bytes());
                }
                tree.push(0); // end of names
            }
            tree.push(0); // end of paths
        }
        tree.push(0); // end of extensions
        let mut out = Vec::new();
        out.extend_from_slice(&VPK_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(tree.len() as u32).to_le_bytes());
        out.extend_from_slice(&tree);
        out
    }

    #[test]
    fn lists_paths_sizes_and_sorts() {
        let bytes = build_v1(&[
            ("vmt", "materials/models", "box", 120),
            ("lua", "lua/autorun", "init", 300),
            ("txt", " ", "readme", 15),
        ]);
        let l = parse_directory(&bytes).unwrap();
        assert_eq!(l.version, 1);
        let paths: Vec<&str> = l.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["lua/autorun/init.lua", "materials/models/box.vmt", "readme.txt"]);
        assert_eq!(l.total_size, 435);
        assert_eq!(l.entries[0].archive, Some(0));
    }

    #[test]
    fn version_two_header_is_longer() {
        let mut v1 = build_v1(&[("lua", "lua", "a", 7)]);
        // rewrite as version 2: insert 16 more header bytes after the tree size
        let tree = v1.split_off(12);
        let mut v2 = Vec::new();
        v2.extend_from_slice(&VPK_SIGNATURE.to_le_bytes());
        v2.extend_from_slice(&2u32.to_le_bytes());
        v2.extend_from_slice(&(tree.len() as u32).to_le_bytes());
        v2.extend_from_slice(&[0u8; 16]);
        v2.extend_from_slice(&tree);
        let l = parse_directory(&v2).unwrap();
        assert_eq!(l.version, 2);
        assert_eq!(l.entries[0].path, "lua/a.lua");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_directory(&[0u8; 4]).is_err());
        let mut bad = build_v1(&[("lua", "lua", "a", 1)]);
        bad[0] = 0;
        assert!(parse_directory(&bad).is_err());
        let mut cut = build_v1(&[("lua", "lua", "a", 1)]);
        cut.truncate(cut.len() - 4);
        assert!(parse_directory(&cut).is_err());
    }

    #[test]
    fn reads_from_disk() {
        let dir = std::env::temp_dir().join("hl2box_vpk_test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("demo_dir.vpk");
        std::fs::write(&p, build_v1(&[("vtf", "materials", "wall", 42)])).unwrap();
        let l = read_directory(&p).unwrap();
        assert_eq!(l.entries[0].path, "materials/wall.vtf");
        assert_eq!(l.entries[0].size, 42);
        let _ = std::fs::remove_file(&p);
    }
}
