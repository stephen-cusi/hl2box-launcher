//! Header and entity information of a Source BSP map.
//!
//! Reads what the Valve Developer Community "BSP (Source)" article documents:
//! the `VBSP` identifier, the format version, the map revision, and the 64
//! lump directory (offset, length, version, fourCC). Lump 0 holds the entities:
//! plain text KeyValue blocks, from which the launcher counts entities and
//! classnames. The rest of the map is never read, so a 50 MB map costs one
//! header read plus the entity lump.
//!
//! Garry's Mod can LZMA-compress lumps; a compressed entity lump is reported
//! as such and not decoded.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::{err, Result};

const IDENT: &[u8; 4] = b"VBSP";
const LUMP_COUNT: usize = 64;
const HEADER_LEN: usize = 8 + LUMP_COUNT * 16 + 4;
/// Largest entity lump read; real maps are a few MB at most.
const MAX_ENTITY_BYTES: u64 = 32 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lump {
    pub index: usize,
    pub offset: u32,
    pub length: u32,
    pub version: u32,
    pub four_cc: [u8; 4],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityStats {
    pub count: usize,
    /// Most frequent classnames, highest count first (at most 12).
    pub classes: Vec<(String, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityState {
    Stats(EntityStats),
    /// The lump is LZMA-compressed (Garry's Mod); its text is not decoded.
    Compressed,
    /// The map has no entity lump.
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BspInfo {
    pub version: u32,
    pub map_revision: u32,
    /// Non-empty lumps only, in index order.
    pub lumps: Vec<Lump>,
    pub entities: EntityState,
}

/// Name of the lumps this launcher labels; others show their index.
pub fn lump_name(index: usize) -> &'static str {
    match index {
        0 => "entities",
        7 => "faces",
        8 => "lighting",
        23 => "displacement info",
        32 => "game lump",
        37 => "pakfile",
        _ => "",
    }
}

fn u32_le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Parse the fixed header from the first `HEADER_LEN` bytes of a map.
pub fn parse_header(head: &[u8]) -> Result<(u32, u32, Vec<Lump>)> {
    if head.len() < HEADER_LEN || &head[0..4] != IDENT {
        return err("bsp: not a Source map (no VBSP identifier)");
    }
    let version = u32_le(head, 4).unwrap_or(0);
    let mut lumps = Vec::new();
    for i in 0..LUMP_COUNT {
        let o = 8 + i * 16;
        let offset = u32_le(head, o).unwrap_or(0);
        let length = u32_le(head, o + 4).unwrap_or(0);
        let lversion = u32_le(head, o + 8).unwrap_or(0);
        let four_cc: [u8; 4] = head[o + 12..o + 16].try_into().unwrap_or([0; 4]);
        if length > 0 {
            lumps.push(Lump { index: i, offset, length, version: lversion, four_cc });
        }
    }
    let revision = u32_le(head, 8 + LUMP_COUNT * 16).unwrap_or(0);
    Ok((version, revision, lumps))
}

/// Count entities and classnames in KeyValue text. An entity is a `{ ... }`
/// block at the top level; `"classname" "x"` lines name it.
pub fn entity_stats(text: &str) -> EntityStats {
    let mut count = 0usize;
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    let mut depth = 0i32;
    for raw in text.lines() {
        let line = raw.trim();
        if line == "{" {
            if depth == 0 {
                count += 1;
            }
            depth += 1;
            continue;
        }
        if line == "}" {
            depth -= 1;
            continue;
        }
        if depth == 1 {
            if let Some(value) = quoted_pair(line, "classname") {
                *classes.entry(value).or_default() += 1;
            }
        }
    }
    let mut list: Vec<(String, usize)> = classes.into_iter().collect();
    list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    list.truncate(12);
    EntityStats { count, classes: list }
}

/// For a line `"key" "value"`, return the value when the key matches.
fn quoted_pair(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix('"')?;
    let (k, rest) = rest.split_once('"')?;
    if k != key {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('"')?;
    let (v, _) = rest.split_once('"')?;
    Some(v.to_string())
}

/// Full analysis from a file: header, then the entity lump only.
pub fn read_info(path: &Path) -> Result<BspInfo> {
    let mut f = std::fs::File::open(path)?;
    let mut head = vec![0u8; HEADER_LEN];
    f.read_exact(&mut head)
        .map_err(|_| crate::Error("bsp: file too short for a header".into()))?;
    let (version, map_revision, lumps) = parse_header(&head)?;
    let entities = match lumps.iter().find(|l| l.index == 0) {
        None => EntityState::Empty,
        Some(l) => {
            if l.length as u64 > MAX_ENTITY_BYTES {
                return err("bsp: entity lump is implausibly large");
            }
            f.seek(SeekFrom::Start(l.offset as u64))?;
            let mut buf = vec![0u8; l.length as usize];
            f.read_exact(&mut buf)
                .map_err(|e| crate::Error(format!("bsp: reading entities: {e}")))?;
            if buf.starts_with(b"LZMA") {
                EntityState::Compressed
            } else {
                let text = String::from_utf8_lossy(buf.strip_suffix(&[0]).unwrap_or(&buf)).into_owned();
                EntityState::Stats(entity_stats(&text))
            }
        }
    };
    Ok(BspInfo { version, map_revision, lumps, entities })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header plus an entity lump placed right after it.
    fn build(version: u32, revision: u32, entities: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_LEN];
        out[0..4].copy_from_slice(IDENT);
        out[4..8].copy_from_slice(&version.to_le_bytes());
        let off = HEADER_LEN as u32;
        out[8..12].copy_from_slice(&off.to_le_bytes());
        out[12..16].copy_from_slice(&(entities.len() as u32).to_le_bytes());
        out[16..20].copy_from_slice(&0u32.to_le_bytes());
        let rev_at = 8 + LUMP_COUNT * 16;
        out[rev_at..rev_at + 4].copy_from_slice(&revision.to_le_bytes());
        out.extend_from_slice(entities);
        out
    }

    const ENTS: &str = r#"{
"classname" "worldspawn"
"skyname" "sky_day01"
}
{
"classname" "prop_static"
}
{
"classname" "prop_static"
}
{
"classname" "info_player_start"
"origin" "0 0 0"
}
"#;

    #[test]
    fn counts_entities_and_classes() {
        let s = entity_stats(ENTS);
        assert_eq!(s.count, 4);
        assert_eq!(s.classes[0], ("prop_static".to_string(), 2));
        assert!(s.classes.iter().any(|(c, n)| c == "worldspawn" && *n == 1));
    }

    #[test]
    fn nested_blocks_are_not_entities() {
        let s = entity_stats("{\n\"classname\" \"a\"\n\"x\"\n{\n\"classname\" \"inner\"\n}\n}\n");
        assert_eq!(s.count, 1);
        assert_eq!(s.classes, vec![("a".to_string(), 1)]);
    }

    #[test]
    fn header_fields_and_lumps() {
        let bytes = build(21, 7, ENTS.as_bytes());
        let (v, rev, lumps) = parse_header(&bytes).unwrap();
        assert_eq!(v, 21);
        assert_eq!(rev, 7);
        assert_eq!(lumps.len(), 1);
        assert_eq!(lumps[0].index, 0);
        assert_eq!(lump_name(lumps[0].index), "entities");
    }

    #[test]
    fn rejects_non_bsp_and_truncated() {
        assert!(parse_header(&[0u8; 2000]).is_err());
        let mut bytes = build(21, 0, b"{}");
        bytes[0] = b'X';
        assert!(parse_header(&bytes).is_err());
        assert!(parse_header(&[b'V', b'B', b'S', b'P']).is_err());
    }

    #[test]
    fn reads_a_map_file_and_flags_compressed_entities() {
        let dir = std::env::temp_dir().join("hl2box_bsp_test");
        let _ = std::fs::create_dir_all(&dir);
        let plain = dir.join("plain.bsp");
        std::fs::write(&plain, build(20, 3, ENTS.as_bytes())).unwrap();
        let info = read_info(&plain).unwrap();
        assert_eq!(info.version, 20);
        assert_eq!(info.map_revision, 3);
        match info.entities {
            EntityState::Stats(s) => assert_eq!(s.count, 4),
            other => panic!("expected stats, got {other:?}"),
        }
        let packed = dir.join("packed.bsp");
        std::fs::write(&packed, build(20, 0, b"LZMA\x10\x00\x00\x00rest")).unwrap();
        assert_eq!(read_info(&packed).unwrap().entities, EntityState::Compressed);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
