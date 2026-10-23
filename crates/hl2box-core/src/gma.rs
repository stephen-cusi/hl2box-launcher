//! Garry's Mod addon (.gma) parsing.
//!
//! Byte-for-byte the layout the engine's own pack-file mounter accepts
//! (filesystem/gmafile.cpp): magic "GMAD", version 1..3, version >= 3
//! carries steamid + timestamp, then the required-content strings, name /
//! description / author, an int32 addon version, and the file index whose
//! entry record is
//!
//!   u32 file number (0 ends the index), path cstring, i64 size, u32 crc
//!
//! followed by the data blocks in index order. Two header details vary
//! between writers (the required-content field and the addon version), so
//! the parser tries layout variants in the same spirit as the engine and
//! keeps the first self-consistent one. Workshop upload archives carry an
//! extra jpeg preview after the addon version; the engine skips those
//! bytes implicitly through its variant probing, here it is an explicit
//! variant so the preview image can be shown in the addon browser.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{err, Result};

pub const GMA_IDENT: &[u8; 4] = b"GMAD";

const MAX_VERSION: u8 = 3;
const MAX_STRING: usize = 4096;
const MAX_ENTRY_NAME: usize = 512;
const MAX_ENTRIES: usize = 256 * 1024;
const MAX_FILE_SIZE: u64 = 0xFFFF_FFF0;
const MAX_REQUIRED: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GmaFileEntry {
    pub path: String,
    pub size: u64,
    pub crc: u32,
    /// Absolute offset of the data block inside the .gma file.
    pub offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GmaInfo {
    pub version: u8,
    pub steam_id: u64,
    pub timestamp: u64,
    pub name: String,
    pub description: String,
    pub author: String,
    pub addon_version: i32,
    /// Workshop preview jpeg bytes, when the archive carries one.
    #[serde(skip)]
    pub jpeg: Option<Vec<u8>>,
    pub entries: Vec<GmaFileEntry>,
}

impl GmaInfo {
    pub fn total_size(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }

    pub fn find(&self, path: &str) -> Option<&GmaFileEntry> {
        let want = normalize(path);
        self.entries.iter().find(|e| e.path == want)
    }
}

fn normalize(p: &str) -> String {
    p.trim_start_matches('/')
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn is_printable(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b >= 32 && b != 127)
}

/// Entry paths the browser is willing to show and read. Mirrors the
/// engine's safety gate at the level this crate needs (no traversal, no
/// absolute paths, no drive letters).
fn is_safe_entry_path(p: &str) -> bool {
    if p.is_empty() || p.starts_with('/') || p.starts_with('\\') || p.contains(':') {
        return false;
    }
    if p.ends_with('/') || p.ends_with('\\') {
        return false;
    }
    p.split(['/', '\\'])
        .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// How the addon version int is treated for a variant.
#[derive(Clone, Copy, PartialEq)]
enum AddonVersionMode {
    /// Present when archive version >= 3 (the common case, engine default).
    Auto,
    Always,
    Never,
}

/// Header layout variant tried in order.
struct Layout {
    /// false: NUL-separated list ended by an empty string.
    /// true: one single string (some packers omit the terminator).
    required_single: bool,
    addon_version: AddonVersionMode,
    /// Some: u16 jpeg length + jpeg bytes right after the addon version
    /// (workshop upload format).
    jpeg: bool,
}

fn layouts() -> Vec<Layout> {
    let mut v = Vec::new();
    for &required_single in &[false, true] {
        for &mode in &[
            AddonVersionMode::Auto,
            AddonVersionMode::Never,
            AddonVersionMode::Always,
        ] {
            v.push(Layout {
                required_single,
                addon_version: mode,
                jpeg: false,
            });
        }
    }
    // Workshop upload archives: jpeg after the addon version.
    for &required_single in &[false, true] {
        v.push(Layout {
            required_single,
            addon_version: AddonVersionMode::Auto,
            jpeg: true,
        });
    }
    v
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn read(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
    fn u8(&mut self) -> Option<u8> {
        self.read(1).map(|b| b[0])
    }
    fn u32(&mut self) -> Option<u32> {
        self.read(4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn i64(&mut self) -> Option<i64> {
        self.read(8).map(|b| i64::from_le_bytes(b.try_into().unwrap()))
    }
    fn cstring(&mut self) -> Option<String> {
        let start = self.pos;
        let end = self.data[start..].iter().position(|&b| b == 0)? + start;
        if end - start > MAX_STRING {
            return None;
        }
        let s = String::from_utf8_lossy(&self.data[start..end]).into_owned();
        self.pos = end + 1;
        Some(s)
    }
}

/// Parse a .gma header and index. The data blocks stay on disk and are
/// read on demand with [`read_entry`].
pub fn parse(path: &Path) -> Result<GmaInfo> {
    let mut f = std::fs::File::open(path)?;
    let file_len = f.metadata()?.len();
    let window = 8 * 1024 * 1024u64.min(file_len);
    let mut head = vec![0u8; window as usize];
    let head_len = {
        let mut total = 0usize;
        while total < head.len() {
            let n = f.read(&mut head[total..])?;
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    };
    head.truncate(head_len);

    let mut last_err = crate::Error("gma: unknown".into());
    for layout in layouts() {
        let mut c = Cursor {
            data: &head,
            pos: 0,
        };
        match parse_with(&mut c, &layout) {
            Ok(info) => {
                // The table must fit the file: last entry's end <= file size.
                let last_end = info
                    .entries
                    .last()
                    .map(|e| e.offset + e.size)
                    .unwrap_or(0);
                if last_end <= file_len {
                    return Ok(info);
                }
                last_err = crate::Error("gma: file table walks past end of file".into());
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

fn parse_with(c: &mut Cursor, layout: &Layout) -> Result<GmaInfo> {
    let ident = c.read(4).ok_or_else(|| crate::Error("gma: short".into()))?;
    if ident != GMA_IDENT {
        return err("gma: bad ident, not a gma file");
    }

    let version = c
        .u8()
        .ok_or_else(|| crate::Error("gma: short (version)".into()))?;
    if version < 1 || version > MAX_VERSION {
        return err(format!("gma: unsupported version {version}"));
    }

    let (steam_id, timestamp) = if version >= 3 {
        let sid = c
            .read(8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
            .ok_or_else(|| crate::Error("gma: short (steamid)".into()))?;
        let ts = c
            .read(8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
            .ok_or_else(|| crate::Error("gma: short (timestamp)".into()))?;
        (sid, ts)
    } else {
        (0, 0)
    };

    if layout.required_single {
        let _ = c
            .cstring()
            .ok_or_else(|| crate::Error("gma: short (required)".into()))?;
    } else {
        let mut n = 0;
        loop {
            let s = c
                .cstring()
                .ok_or_else(|| crate::Error("gma: short (required)".into()))?;
            if s.is_empty() {
                break;
            }
            n += 1;
            if n >= MAX_REQUIRED {
                return err("gma: required content list never ends");
            }
        }
    }

    let name = c
        .cstring()
        .ok_or_else(|| crate::Error("gma: short (name)".into()))?;
    let description = c
        .cstring()
        .ok_or_else(|| crate::Error("gma: short (description)".into()))?;
    let author = c
        .cstring()
        .ok_or_else(|| crate::Error("gma: short (author)".into()))?;

    let read_av = match layout.addon_version {
        AddonVersionMode::Auto => version >= 3,
        AddonVersionMode::Always => true,
        AddonVersionMode::Never => false,
    };
    let addon_version = if read_av {
        c.u32()
            .map(|v| v as i32)
            .ok_or_else(|| crate::Error("gma: short (addon version)".into()))?
    } else {
        0
    };

    let jpeg = if layout.jpeg {
        let len = c
            .read(2)
            .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
            .ok_or_else(|| crate::Error("gma: short (jpeg length)".into()))?;
        if len as usize <= MAX_STRING * 8 {
            c.read(len as usize)
                .map(|s| s.to_vec())
                .ok_or_else(|| crate::Error("gma: short (jpeg)".into()))?
        } else {
            return err("gma: jpeg length out of range");
        }
    } else {
        Vec::new()
    };

    // File index: records of (u32 file number, path, i64 size, u32 crc),
    // terminated by a zero file number. The data blocks start where the
    // index ends, so offsets are computed only after the whole index has
    // been consumed.
    struct RawEntry {
        path: String,
        size: u64,
        crc: u32,
    }
    let mut raws: Vec<RawEntry> = Vec::new();
    loop {
        let file_number = c
            .u32()
            .ok_or_else(|| crate::Error("gma: short (file index)".into()))?;
        if file_number == 0 {
            break;
        }
        let path = c
            .cstring()
            .ok_or_else(|| crate::Error("gma: short (entry name)".into()))?;
        let size = c
            .i64()
            .ok_or_else(|| crate::Error("gma: short (entry size)".into()))?;
        let crc = c
            .u32()
            .ok_or_else(|| crate::Error("gma: short (entry crc)".into()))?;

        if raws.is_empty() {
            // Whole-header sanity gate, same idea as the engine: a misread
            // layout produces garbage right here.
            if path.is_empty() || path.len() >= MAX_ENTRY_NAME || !is_printable(&path) {
                return err("gma: first entry fails the sanity check");
            }
            if size < 0 || size as u64 > MAX_FILE_SIZE {
                return err("gma: first entry size out of range");
            }
        }
        if raws.len() >= MAX_ENTRIES {
            return err("gma: too many entries");
        }

        raws.push(RawEntry {
            path,
            size: size.max(0) as u64,
            crc,
        });
    }

    let mut entries = Vec::with_capacity(raws.len());
    let mut offset = c.pos as u64;
    for raw in raws {
        let keep = raw.size <= MAX_FILE_SIZE && is_safe_entry_path(&raw.path);
        if keep {
            entries.push(GmaFileEntry {
                path: normalize(&raw.path),
                size: raw.size,
                crc: raw.crc,
                offset,
            });
        }
        offset += raw.size;
    }

    Ok(GmaInfo {
        version,
        steam_id,
        timestamp,
        name,
        description,
        author,
        addon_version,
        jpeg: if jpeg.is_empty() { None } else { Some(jpeg) },
        entries,
    })
}

/// Read one data block out of an opened .gma file.
pub fn read_entry<R: Read + Seek>(file: &mut R, entry: &GmaFileEntry) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(entry.offset))?;
    let mut buf = vec![0u8; entry.size as usize];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// Read one data block straight from a path.
pub fn read_entry_from_path(path: &Path, entry: &GmaFileEntry) -> Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    read_entry(&mut f, entry)
}

/// Best-effort "is this text worth showing in a viewer" check for the
/// file-tree preview pane: text extensions, size-capped.
pub fn entry_is_previewable(entry: &GmaFileEntry) -> bool {
    const TEXT_EXT: [&str; 12] = [
        "lua", "json", "txt", "vmt", "vdf", "properties", "cfg", "res", "cur", "fnt", "svg",
        "xml",
    ];
    let ext = entry.path.rsplit('.').next().unwrap_or("");
    entry.size <= 512 * 1024 && TEXT_EXT.contains(&ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_non_gma() {
        let tmp = std::env::temp_dir().join("hl2box_not_gma.bin");
        std::fs::write(&tmp, b"not a gma at all........").unwrap();
        assert!(parse(&tmp).is_err());
    }

    #[test]
    fn parses_minimal_v3_like_the_engine_layout() {
        // Header bytes assembled exactly like the engine layout:
        // GMAD, v3, steamid, timestamp, empty required list, name, desc,
        // author, addon version, index (file number, path, size, crc),
        // terminator 0.
        let mut b: Vec<u8> = Vec::new();
        b.extend_from_slice(b"GMAD");
        b.push(3);
        b.extend_from_slice(&12345u64.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(b"\0"); // required list ends immediately
        b.extend_from_slice(b"Test Addon\0");
        b.extend_from_slice(b"desc\0");
        b.extend_from_slice(b"author\0");
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes()); // file number 1
        b.extend_from_slice(b"lua/test.lua\0");
        b.extend_from_slice(&3i64.to_le_bytes()); // size
        b.extend_from_slice(&0xC0FFEEu32.to_le_bytes()); // crc
        b.extend_from_slice(&0u32.to_le_bytes()); // end of index
        b.extend_from_slice(b"abc"); // data
        let tmp = std::env::temp_dir().join("hl2box_min.gma");
        std::fs::write(&tmp, &b).unwrap();

        let info = parse(&tmp).unwrap();
        assert_eq!(info.name, "Test Addon");
        assert_eq!(info.steam_id, 12345);
        assert_eq!(info.entries.len(), 1);
        assert_eq!(info.entries[0].path, "lua/test.lua");
        assert_eq!(info.entries[0].size, 3);
        let data = read_entry_from_path(&tmp, &info.entries[0]).unwrap();
        assert_eq!(data, b"abc");
        let _ = std::fs::remove_file(&tmp);
    }
}
