//! Header, entity, game-lump and embedded-file information of a Source BSP map.
//!
//! Everything here follows `public/bspfile.h` and `public/gamebspfile.h` of the
//! Source SDK (the lump list is generated from that header, not typed by hand):
//!
//! * the `VBSP` identifier, format version, map revision, and 64 lump entries;
//! * lump 0, the entities: KeyValue text, which Garry's Mod may store LZMA
//!   compressed (lzma_header_t: id `LZMA`, actual size, packed size, 5 bytes of
//!   properties, then the stream). Compressed entities are decoded with lzma-rs;
//! * lump 35, the game lump: a directory of `sprp` (static props), `dprp`
//!   (detail props) and other sub-lumps, each with its own offset and version;
//! * lump 40, the pakfile: a ZIP of the files the map embeds. Its central
//!   directory is read to list the names and sizes, without extracting anything.
//!
//! The map's geometry and textures are never parsed. Only the lumps above are
//! read, so large maps cost a few reads.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::{err, Result};

const IDENT: &[u8; 4] = b"VBSP";
const LUMP_COUNT: usize = 64;
const HEADER_LEN: usize = 8 + LUMP_COUNT * 16 + 4;
const LUMP_ENTITIES: usize = 0;
const LUMP_GAME: usize = 35;
const LUMP_PAKFILE: usize = 40;
/// lzma_header_t: id, actual size, packed size, 5 property bytes.
const LZMA_HEADER_LEN: usize = 17;
const LZMA_ID: &[u8; 4] = b"LZMA";
/// Largest lumps read into memory. Real maps stay far below these.
const MAX_ENTITY_BYTES: u64 = 32 << 20;
const MAX_PAKFILE_BYTES: u64 = 256 << 20;
const MAX_PAK_NAMES: usize = 20_000;

/// Lump names from `public/bspfile.h` (generated).
const LUMP_NAMES: &[(usize, &str)] = &[
    (0, "entities"),
    (1, "planes"),
    (2, "texdata"),
    (3, "vertexes"),
    (4, "visibility"),
    (5, "nodes"),
    (6, "texinfo"),
    (7, "faces"),
    (8, "lighting"),
    (9, "occlusion"),
    (10, "leafs"),
    (11, "faceids"),
    (12, "edges"),
    (13, "surfedges"),
    (14, "models"),
    (15, "worldlights"),
    (16, "leaffaces"),
    (17, "leafbrushes"),
    (18, "brushes"),
    (19, "brushsides"),
    (20, "areas"),
    (21, "areaportals"),
    (22, "unused0"),
    (23, "unused1"),
    (24, "unused2"),
    (25, "unused3"),
    (26, "dispinfo"),
    (27, "originalfaces"),
    (28, "physdisp"),
    (29, "physcollide"),
    (30, "vertnormals"),
    (31, "vertnormalindices"),
    (32, "disp_lightmap_alphas"),
    (33, "disp_verts"),
    (34, "disp_lightmap_sample_positions"),
    (35, "game_lump"),
    (36, "leafwaterdata"),
    (37, "primitives"),
    (38, "primverts"),
    (39, "primindices"),
    (40, "pakfile"),
    (41, "clipportalverts"),
    (42, "cubemaps"),
    (43, "texdata_string_data"),
    (44, "texdata_string_table"),
    (45, "overlays"),
    (46, "leafmindisttowater"),
    (47, "face_macro_texture_info"),
    (48, "disp_tris"),
    (49, "physcollidesurface"),
    (50, "wateroverlays"),
    (51, "leaf_ambient_index_hdr"),
    (52, "leaf_ambient_index"),
    (53, "lighting_hdr"),
    (54, "worldlights_hdr"),
    (55, "leaf_ambient_lighting_hdr"),
    (56, "leaf_ambient_lighting"),
    (57, "xzippakfile"),
    (58, "faces_hdr"),
    (59, "map_flags"),
    (60, "overlay_fades"),
];

/// Name of a lump index, or "" when the header does not name it.
pub fn lump_name(index: usize) -> &'static str {
    LUMP_NAMES.iter().find(|(i, _)| *i == index).map(|(_, n)| *n).unwrap_or("")
}

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
    /// Plain text, or LZMA-compressed and decoded here.
    Stats { stats: EntityStats, compressed: bool },
    /// The lump is compressed and could not be decoded; the reason is given.
    Unreadable(String),
    /// The map has no entity lump.
    Empty,
}

/// One sub-lump of the game lump directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameLump {
    /// The four-character id as written in the sources, e.g. `sprp`.
    pub id: String,
    pub flags: u16,
    pub version: u16,
    /// Absolute offset in the BSP file.
    pub offset: u32,
    pub length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PakEntry {
    pub name: String,
    pub size: u32,
    /// ZIP compression method: 0 stored, 8 deflate, 14 LZMA, others.
    pub method: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pakfile {
    /// Files listed in the central directory.
    pub count: usize,
    pub total_size: u64,
    /// Names and sizes, in archive order, capped at {MAX_PAK_NAMES}.
    pub entries: Vec<PakEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BspInfo {
    pub version: u32,
    pub map_revision: u32,
    /// Non-empty lumps only, in index order.
    pub lumps: Vec<Lump>,
    pub entities: EntityState,
    /// Game lump directory; empty when the map has none.
    pub game_lumps: Vec<GameLump>,
    /// The embedded-file archive, when the map has one.
    pub pakfile: Option<Pakfile>,
}

fn u16_le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
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

/// Count entities and classnames in KeyValue text. An entity is a top-level
/// `{ ... }` block; `"classname" "x"` lines directly inside it name it.
pub fn entity_stats(text: &str) -> EntityStats {
    let mut count = 0usize;
    let mut classes: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
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

/// Decode an lzma_header_t-wrapped lump. The properties and the actual size
/// are rebuilt into the LZMA-alone header that lzma-rs reads.
pub fn decode_lzma_lump(buf: &[u8], max_out: usize) -> Result<Vec<u8>> {
    if buf.len() < LZMA_HEADER_LEN || &buf[0..4] != LZMA_ID {
        return err("lzma: not an LZMA-wrapped lump");
    }
    let actual = u32_le(buf, 4).unwrap_or(0) as usize;
    if actual > max_out {
        return err(format!("lzma: lump would expand to {actual} bytes, over the limit"));
    }
    let mut alone = Vec::with_capacity(13 + buf.len() - LZMA_HEADER_LEN);
    alone.extend_from_slice(&buf[12..17]); // properties: lc/lp/pb byte + dictionary size
    alone.extend_from_slice(&(actual as u64).to_le_bytes());
    alone.extend_from_slice(&buf[LZMA_HEADER_LEN..]);
    let mut out = Vec::with_capacity(actual);
    lzma_rs::lzma_decompress(&mut std::io::BufReader::new(&alone[..]), &mut out)
        .map_err(|e| crate::Error(format!("lzma: decode failed: {e}")))?;
    Ok(out)
}

/// Read the game lump directory from the game-lump bytes.
pub fn parse_game_lumps(buf: &[u8]) -> Vec<GameLump> {
    let count = u32_le(buf, 0).unwrap_or(0) as usize;
    let mut out = Vec::new();
    for i in 0..count.min(256) {
        let o = 4 + i * 16;
        let Some(e) = buf.get(o..o + 16) else { break };
        // the id is a multi-character constant: its bytes are stored reversed
        let id: String = e[0..4].iter().rev().map(|&b| b as char).collect();
        out.push(GameLump {
            id,
            flags: u16_le(e, 4).unwrap_or(0),
            version: u16_le(e, 6).unwrap_or(0),
            offset: u32_le(e, 8).unwrap_or(0),
            length: u32_le(e, 12).unwrap_or(0),
        });
    }
    out
}

/// Read the ZIP central directory of a pakfile lump. Stored and deflated
/// entries are listed by name and size; nothing is extracted. ZIP64 archives
/// are not handled (map pakfiles never need it).
pub fn parse_pakfile(buf: &[u8]) -> Result<Pakfile> {
    const EOCD: u32 = 0x0605_4B50;
    const CDIR: u32 = 0x0201_4B50;
    // The end-of-central-directory record sits in the last 64 KiB + 22 bytes.
    let search_from = buf.len().saturating_sub(22 + 0xFFFF);
    let eocd = (search_from..buf.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_le(buf, i) == Some(EOCD))
        .ok_or_else(|| crate::Error("pakfile: no ZIP end record".into()))?;
    let total = u16_le(buf, eocd + 10).unwrap_or(0) as usize;
    let cd_off = u32_le(buf, eocd + 16).unwrap_or(0) as usize;
    let mut pos = cd_off;
    let mut entries = Vec::new();
    let mut total_size: u64 = 0;
    for _ in 0..total {
        if u32_le(buf, pos) != Some(CDIR) {
            return err("pakfile: central directory is damaged");
        }
        let method = u16_le(buf, pos + 10).unwrap_or(0);
        let size = u32_le(buf, pos + 24).unwrap_or(0);
        let name_len = u16_le(buf, pos + 28).unwrap_or(0) as usize;
        let extra = u16_le(buf, pos + 30).unwrap_or(0) as usize;
        let comment = u16_le(buf, pos + 32).unwrap_or(0) as usize;
        let name_bytes = buf
            .get(pos + 46..pos + 46 + name_len)
            .ok_or_else(|| crate::Error("pakfile: name cut off".into()))?;
        total_size += size as u64;
        if entries.len() < MAX_PAK_NAMES {
            entries.push(PakEntry {
                name: String::from_utf8_lossy(name_bytes).into_owned(),
                size,
                method,
            });
        }
        pos += 46 + name_len + extra + comment;
    }
    Ok(Pakfile { count: total, total_size, entries })
}

/// Full analysis from a file: header, entities, game lumps and pakfile.
pub fn read_info(path: &Path) -> Result<BspInfo> {
    let mut f = std::fs::File::open(path)?;
    let mut head = vec![0u8; HEADER_LEN];
    f.read_exact(&mut head)
        .map_err(|_| crate::Error("bsp: file too short for a header".into()))?;
    let (version, map_revision, lumps) = parse_header(&head)?;

    let read_lump = |f: &mut std::fs::File, idx: usize, cap: u64| -> Result<Option<Vec<u8>>> {
        let Some(l) = lumps.iter().find(|l| l.index == idx) else { return Ok(None) };
        if l.length as u64 > cap {
            return err(format!("bsp: lump {idx} is implausibly large"));
        }
        f.seek(SeekFrom::Start(l.offset as u64))?;
        let mut buf = vec![0u8; l.length as usize];
        f.read_exact(&mut buf)
            .map_err(|e| crate::Error(format!("bsp: reading lump {idx}: {e}")))?;
        Ok(Some(buf))
    };

    let entities = match read_lump(&mut f, LUMP_ENTITIES, MAX_ENTITY_BYTES)? {
        None => EntityState::Empty,
        Some(buf) => {
            if buf.starts_with(LZMA_ID) {
                match decode_lzma_lump(&buf, MAX_ENTITY_BYTES as usize) {
                    Ok(text) => EntityState::Stats {
                        stats: entity_stats(&String::from_utf8_lossy(&text)),
                        compressed: true,
                    },
                    Err(e) => EntityState::Unreadable(e.0),
                }
            } else {
                let text = String::from_utf8_lossy(buf.strip_suffix(&[0]).unwrap_or(&buf)).into_owned();
                EntityState::Stats { stats: entity_stats(&text), compressed: false }
            }
        }
    };

    let game_lumps = match read_lump(&mut f, LUMP_GAME, MAX_ENTITY_BYTES)? {
        None => Vec::new(),
        Some(buf) => parse_game_lumps(&buf),
    };

    let pakfile = match read_lump(&mut f, LUMP_PAKFILE, MAX_PAKFILE_BYTES)? {
        None => None,
        Some(buf) => parse_pakfile(&buf).ok(),
    };

    Ok(BspInfo { version, map_revision, lumps, entities, game_lumps, pakfile })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header whose lumps are placed one after another from `payloads`.
    /// `payloads[i]` is (lump index, bytes).
    fn build(version: u32, revision: u32, payloads: &[(usize, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_LEN];
        out[0..4].copy_from_slice(IDENT);
        out[4..8].copy_from_slice(&version.to_le_bytes());
        let rev_at = 8 + LUMP_COUNT * 16;
        out[rev_at..rev_at + 4].copy_from_slice(&revision.to_le_bytes());
        for (idx, bytes) in payloads {
            let at = out.len() as u32;
            let o = 8 + idx * 16;
            out[o..o + 4].copy_from_slice(&at.to_le_bytes());
            out[o + 4..o + 8].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
            out[o + 8..o + 12].copy_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(bytes);
        }
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
{
"classname" "prop_physics"
}
"#;

    #[test]
    fn lump_names_come_from_the_sdk_header() {
        assert_eq!(lump_name(0), "entities");
        assert_eq!(lump_name(7), "faces");
        assert_eq!(lump_name(26), "dispinfo");
        assert_eq!(lump_name(35), "game_lump");
        assert_eq!(lump_name(40), "pakfile");
        assert_eq!(lump_name(23), "unused1");
        assert_eq!(lump_name(63), "");
    }

    #[test]
    fn counts_entities_and_classes() {
        let s = entity_stats(ENTS);
        assert_eq!(s.count, 5);
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
        let bytes = build(21, 7, &[(0, ENTS.as_bytes().to_vec())]);
        let (v, rev, lumps) = parse_header(&bytes).unwrap();
        assert_eq!((v, rev), (21, 7));
        assert_eq!(lumps.len(), 1);
        assert_eq!(lumps[0].index, 0);
    }

    #[test]
    fn rejects_non_bsp_and_truncated() {
        assert!(parse_header(&[0u8; 2000]).is_err());
        let mut bytes = build(21, 0, &[]);
        bytes[0] = b'X';
        assert!(parse_header(&bytes).is_err());
        assert!(parse_header(b"VBSP").is_err());
    }

    /// Real LZMA-alone output of the entity text above, wrapped the way
    /// Garry's Mod stores a compressed lump (lzma_header_t + stream).
    fn lzma_lump() -> Vec<u8> {
        let props = hex("5d00008000");
        let packed = hex("003d8280452210f174dbd4fb9bd48080a9a839f63212b9b099241da89ed04d17349a21c6a3b7050bd673f56ef1af362a33ffff79680000");
        let mut lump = Vec::new();
        lump.extend_from_slice(b"LZMA");
        lump.extend_from_slice(&89u32.to_le_bytes());
        lump.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        lump.extend_from_slice(&props);
        lump.extend_from_slice(&packed);
        lump
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn decodes_lzma_compressed_entities() {
        let decoded = decode_lzma_lump(&lzma_lump(), 1 << 20).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "{\n\"classname\" \"worldspawn\"\n}\n{\n\"classname\" \"prop_static\"\n}\n{\n\"classname\" \"prop_static\"\n}\n");
        assert!(decode_lzma_lump(b"LZMAxx", 100).is_err());
        assert!(decode_lzma_lump(&lzma_lump(), 4).is_err(), "output cap is enforced");
    }

    #[test]
    fn game_lumps_read_their_ids_in_source_order() {
        // one entry: 'sprp' stored little-endian, as the SDK compiler writes it
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&0x7370_7270u32.to_le_bytes()); // 'sprp'
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&10u16.to_le_bytes());
        buf.extend_from_slice(&4000u32.to_le_bytes());
        buf.extend_from_slice(&256u32.to_le_bytes());
        let g = parse_game_lumps(&buf);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].id, "sprp");
        assert_eq!((g[0].version, g[0].offset, g[0].length), (10, 4000, 256));
    }

    /// A ZIP central directory with two stored entries, placed as a pakfile.
    fn zip_lump() -> Vec<u8> {
        let mut z = Vec::new();
        let mut cd = Vec::new();
        for (name, size) in [("materials/wall.vmt", 120u32), ("sound/a.wav", 3000u32)] {
            cd.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
            cd.extend_from_slice(&[0u8; 6]); // version made by, version needed
            cd.extend_from_slice(&0u16.to_le_bytes()); // method at offset 10: stored
            cd.extend_from_slice(&[0u8; 8]); // time, date, crc
            cd.extend_from_slice(&size.to_le_bytes()); // compressed
            cd.extend_from_slice(&size.to_le_bytes()); // uncompressed
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&[0u8; 12]); // extra, comment, disk, attrs
            cd.extend_from_slice(&[0u8; 4]); // local header offset (the attributes are in the 12 bytes above)
            cd.extend_from_slice(name.as_bytes());
        }
        let cd_at = z.len() as u32;
        z.extend_from_slice(&cd);
        z.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
        z.extend_from_slice(&[0u8; 4]); // disk numbers
        z.extend_from_slice(&2u16.to_le_bytes()); // entries on disk
        z.extend_from_slice(&2u16.to_le_bytes()); // entries total
        z.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        z.extend_from_slice(&cd_at.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes()); // comment length
        z
    }

    #[test]
    fn pakfile_central_directory_is_listed() {
        let p = parse_pakfile(&zip_lump()).unwrap();
        assert_eq!(p.count, 2);
        assert_eq!(p.total_size, 3120);
        assert_eq!(p.entries[0].name, "materials/wall.vmt");
        assert_eq!(p.entries[1].size, 3000);
        assert_eq!(p.entries[0].method, 0);
        assert!(parse_pakfile(b"no zip here at all, nothing to see").is_err());
    }

    #[test]
    fn reads_a_full_map_with_every_lump() {
        let dir = std::env::temp_dir().join("hl2box_bsp_full_test");
        let _ = std::fs::create_dir_all(&dir);
        let mut game = Vec::new();
        game.extend_from_slice(&1u32.to_le_bytes());
        game.extend_from_slice(&0x7370_7270u32.to_le_bytes());
        game.extend_from_slice(&[0u8; 12]);
        let plain = dir.join("plain.bsp");
        let bytes = build(20, 3, &[
            (0, ENTS.as_bytes().to_vec()),
            (35, game.clone()),
            (40, zip_lump()),
        ]);
        std::fs::write(&plain, bytes).unwrap();
        let info = read_info(&plain).unwrap();
        assert_eq!((info.version, info.map_revision), (20, 3));
        assert_eq!(info.lumps.len(), 3);
        match &info.entities {
            EntityState::Stats { stats, compressed } => {
                assert_eq!(stats.count, 5);
                assert!(!compressed);
            }
            other => panic!("expected stats, got {other:?}"),
        }
        assert_eq!(info.game_lumps[0].id, "sprp");
        assert_eq!(info.pakfile.as_ref().unwrap().count, 2);

        let packed = dir.join("packed.bsp");
        std::fs::write(&packed, build(20, 0, &[(0, lzma_lump())])).unwrap();
        match read_info(&packed).unwrap().entities {
            EntityState::Stats { stats, compressed } => {
                assert!(compressed);
                assert_eq!(stats.count, 3);
            }
            other => panic!("expected decoded stats, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
