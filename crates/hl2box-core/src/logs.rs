//! Log viewing: find and read the logs the game and launcher write.
//!
//! Three things make the real game logs readable:
//!
//! * **Decoding.** Logs are UTF-8 when the engine is configured for it and
//!   ANSI (GBK here) when a Windows Chinese console wrote them. Decoding picks
//!   whichever reads with the fewest replacement characters instead of
//!   turning every non-ASCII byte into a question mark.
//! * **Structure.** Engine lines start with a `[seconds]` stamp, Lua lines
//!   with `L MM/DD/YYYY - HH:MM:SS:`, and many messages carry a leading
//!   `[Tag]`. [`parse`] pulls those apart so the viewer can filter by time
//!   and by source, not only by keywords.
//! * **Binary dumps.** Crash dumps (`.mdmp` / `.dmp`) are not text. They are
//!   never decoded as one; [`binary_strings`] pulls out the printable runs,
//!   which is usually enough to see the module, the exception and the build.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::Paths;

/// Whether a source is decoded as text or summarised as a binary file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    #[default]
    Text,
    BinaryDump,
}

/// A log the launcher knows where to find.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSource {
    pub id: String,
    pub title: String,
    pub path: PathBuf,
    #[serde(default)]
    pub kind: SourceKind,
}

/// Sources offered in the UI, in display order. Missing files are still
/// listed (with their expected path) so the user can see where to look.
pub fn known_logs(paths: &Paths) -> Vec<LogSource> {
    let mod_root = paths.mod_root();
    let engine_root = paths.engine_root.clone();

    // The engine writes `engine.log` into its root; older builds used
    // `hl2sb_engine.log`. Prefer whichever exists.
    let engine_path = [engine_root.join("engine.log"), engine_root.join("hl2sb_engine.log")]
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| engine_root.join("engine.log"));

    let mut out = vec![
        LogSource {
            id: "lua".into(),
            title: "Lua log".into(),
            path: mod_root.join("hl2sb_lua.log"),
            kind: SourceKind::Text,
        },
        LogSource {
            id: "engine".into(),
            title: "Engine log".into(),
            path: engine_path,
            kind: SourceKind::Text,
        },
        LogSource {
            id: "crash".into(),
            title: "Crash handler log".into(),
            path: mod_root.join("hl2sb_crash.log"),
            kind: SourceKind::Text,
        },
        LogSource {
            id: "server".into(),
            title: "Server debug log".into(),
            path: mod_root.join("ds_debug.log"),
            kind: SourceKind::Text,
        },
        LogSource {
            id: "console".into(),
            title: "Console log (-condebug)".into(),
            path: mod_root.join("console.log"),
            kind: SourceKind::Text,
        },
    ];

    // Every crash dump the handler wrote, newest first. The crash handler
    // names them crash_<date>_<time>_..., so a folder full of them is common.
    for path in dump_files(&mod_root.join("dumps"), MAX_LISTED_DUMPS) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let is_text = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("txt"))
            .unwrap_or(false);
        out.push(LogSource {
            id: format!("dump:{name}"),
            title: format!("Crash dump: {name}"),
            path,
            kind: if is_text { SourceKind::Text } else { SourceKind::BinaryDump },
        });
    }
    out
}

/// Most dumps listed in the source picker; older ones stay on disk.
pub const MAX_LISTED_DUMPS: usize = 30;

/// Dump files in `dir`, newest first, at most `limit`.
pub fn dump_files(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for ent in rd.flatten() {
        let p = ent.path();
        if !p.is_file() {
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if !matches!(ext.as_str(), "mdmp" | "dmp" | "txt") {
            continue;
        }
        let mtime = ent.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        found.push((mtime, p));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().take(limit).map(|(_, p)| p).collect()
}

/// Decode raw log bytes into display text.
///
/// BOM first (UTF-8 and UTF-16LE). Then valid UTF-8 as is. Otherwise try
/// GBK (what a Chinese Windows console writes) and keep it only if it is
/// cleaner than a lossy UTF-8 read.
pub fn decode_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let (gbk, _, _) = encoding_rs::GBK.decode(bytes);
    let lossy = String::from_utf8_lossy(bytes);
    let bad = |s: &str| s.chars().filter(|c| *c == '\u{FFFD}').count();
    if bad(&gbk) <= bad(&lossy) {
        gbk.into_owned()
    } else {
        lossy.into_owned()
    }
}

/// Read the last `max_bytes` of a text log, split into lines, oldest first.
/// Returns an empty vector when the file does not exist (yet). The first
/// line of a partial read is dropped because it is almost certainly cut.
pub fn tail(path: &Path, max_bytes: u64) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match f.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if (buf.len() as u64) > max_bytes + (1 << 20) {
            break;
        }
    }
    let text = decode_bytes(&buf);
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|s| s.trim_end_matches('\r').to_string())
        .collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines
}

/// Printable strings from a binary file, the way `strings(1)` finds them:
/// runs of at least `min` printable ASCII bytes, then runs of UTF-16LE
/// characters (Windows dumps keep module paths in UTF-16). Capped at
/// `max_out` entries.
pub fn binary_strings(bytes: &[u8], min: usize, max_out: usize) -> Vec<String> {
    let mut out = Vec::new();
    let printable = |b: u8| (0x20..=0x7e).contains(&b) || b == b'\t';

    // Random bytes that happen to be printable are mostly punctuation, so a
    // run only counts when at least half of it is letters.
    let readable = |run: &[u8]| {
        run.len() >= min && run.iter().filter(|b| b.is_ascii_alphabetic()).count() * 2 >= run.len()
    };
    let mut run = Vec::new();
    for &b in bytes {
        if printable(b) {
            run.push(b);
        } else {
            if readable(&run) {
                out.push(String::from_utf8_lossy(&run).into_owned());
            }
            run.clear();
        }
        if out.len() >= max_out {
            return out;
        }
    }
    if readable(&run) {
        out.push(String::from_utf8_lossy(&run).into_owned());
    }

    let mut wide = String::new();
    let mut i = 0;
    let flush = |wide: &mut String, out: &mut Vec<String>| {
        let letters = wide.chars().filter(|c| c.is_ascii_alphabetic()).count();
        if wide.chars().count() >= min && letters * 2 >= wide.chars().count() {
            out.push(std::mem::take(wide));
        } else {
            wide.clear();
        }
    };
    // UTF-16 code units sit on even offsets in dumps. Pairing only at even
    // offsets stops ASCII text followed by a NUL from reading as wide text.
    while i + 1 < bytes.len() {
        let (lo, hi) = (bytes[i], bytes[i + 1]);
        if hi == 0 && printable(lo) {
            wide.push(lo as char);
        } else {
            flush(&mut wide, &mut out);
        }
        i += 2;
        if out.len() >= max_out {
            break;
        }
    }
    flush(&mut wide, &mut out);
    out.truncate(max_out);
    out
}

/// Read a binary dump (at most `max_bytes`) and return its printable strings.
pub fn read_dump_strings(path: &Path, max_bytes: u64) -> std::io::Result<Vec<String>> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.by_ref().take(max_bytes).read_to_end(&mut buf)?;
    Ok(binary_strings(&buf, 6, 20_000))
}

/// Structure pulled from one line. Borrowed where possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed<'a> {
    /// `12.3456` for engine stamps, `03:02:56` for Lua stamps.
    pub time: Option<String>,
    /// The leading `[Tag]` of the message, lower-cased (`hl2sb wp`, `lua`).
    pub tag: Option<String>,
    pub message: &'a str,
}

/// Split a line into its time stamp, leading tag and message.
pub fn parse(line: &str) -> Parsed<'_> {
    let (time, rest) = if let Some(r) = strip_lua_stamp(line) {
        (Some(r.0), r.1)
    } else if let Some(r) = strip_engine_stamp(line) {
        (Some(r.0), r.1)
    } else {
        (None, line)
    };
    let (tag, message) = match rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some((t, msg)) if valid_tag(t) => (Some(t.trim().to_ascii_lowercase()), msg.trim_start()),
        _ => (None, rest),
    };
    Parsed { time, tag, message }
}

/// A source tag is a short name: letters, digits, spaces and `_ - .`.
/// Anything with a bracket or an equals sign is message text, not a tag
/// (`[102.4269] ...` style values must not become sources).
fn valid_tag(t: &str) -> bool {
    let t = t.trim();
    !t.is_empty()
        && t.len() <= 32
        && t.chars().any(|c| c.is_ascii_alphabetic())
        && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-' | '.'))
}

/// `L 10/27/2026 - 03:02:56: message`
fn strip_lua_stamp(line: &str) -> Option<(String, &str)> {
    let rest = line.strip_prefix("L ")?;
    let b = rest.as_bytes();
    // MM/DD/YYYY - HH:MM:SS:<space>  is exactly 23 bytes
    if b.len() < 23 {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(|c| c.is_ascii_digit());
    let ok = digits(0..2)
        && b[2] == b'/'
        && digits(3..5)
        && b[5] == b'/'
        && digits(6..10)
        && &b[10..13] == b" - "
        && digits(13..15)
        && b[15] == b':'
        && digits(16..18)
        && b[18] == b':'
        && digits(19..21)
        && b[21] == b':'
        && b[22] == b' ';
    if !ok {
        return None;
    }
    Some((rest[13..21].to_string(), &rest[23..]))
}

/// `[12.3456] message`
fn strip_engine_stamp(line: &str) -> Option<(String, &str)> {
    let rest = line.strip_prefix('[')?;
    let (stamp, msg) = rest.split_once("] ")?;
    let valid = !stamp.is_empty()
        && stamp.contains('.')
        && stamp.chars().all(|c| c.is_ascii_digit() || c == '.');
    if valid {
        Some((stamp.to_string(), msg))
    } else {
        None
    }
}

/// How a line reads at a glance. Drives colour, level chips and "next error".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Error,
    Warn,
    Info,
    Plain,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Error, Level::Warn, Level::Info, Level::Plain];

    pub fn key(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Plain => "plain",
        }
    }
}

/// Classify one line. Error patterns win over warnings, warnings over info.
///
/// The patterns come from the real engine and Lua logs: `Error:` / `lua_error:`
/// / `attempt to ...` / `uses unknown shader` are errors; `Unknown command`,
/// `not allowed`, `Missing ...`, `Unable to ...` are warnings; the launcher's
/// own `[HL2SB]` and `[lua]` lines are info. Engine chatter such as
/// `LoadLibrary:` stays plain.
pub fn classify(line: &str) -> Level {
    let lower = line.to_ascii_lowercase();
    const ERROR: [&str; 9] = [
        "error:",
        "[error",
        "attempt to ",
        "stack traceback",
        "exception",
        "fatal",
        "assertion failed",
        "uses unknown shader",
        "segmentation",
    ];
    const WARN: [&str; 14] = [
        "warning",
        "warn:",
        "deprecated",
        "unknown command",
        "not allowed",
        "must be marked",
        "missing",
        "doesn't point to",
        "unable to",
        "can't use",
        "is blocked",
        "undefined",
        "does not exist",
        "not be modified",
    ];
    const INFO: [&str; 9] = [
        "[hl2sb",
        "[lua]",
        "loaded",
        "initialized",
        "mounted",
        "started at",
        "session",
        "registered",
        "ready",
    ];
    if ERROR.iter().any(|p| lower.contains(p)) {
        Level::Error
    } else if WARN.iter().any(|p| lower.contains(p)) {
        Level::Warn
    } else if INFO.iter().any(|p| lower.contains(p)) {
        Level::Info
    } else {
        Level::Plain
    }
}

/// Session banners: `=== Lua log session ===`, `>>> Engine(...) started at`.
pub fn is_session(line: &str) -> bool {
    let msg = parse(line).message;
    msg.starts_with("===") || msg.starts_with(">>>") || msg.contains("started at")
}

/// Case-insensitive substring match. An empty query matches everything.
pub fn matches(line: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    line.to_lowercase().contains(&q.to_lowercase())
}

/// Count per level, in [`Level::ALL`] order.
pub fn level_counts(lines: &[String]) -> [usize; 4] {
    let mut out = [0usize; 4];
    for l in lines {
        let idx = Level::ALL.iter().position(|x| *x == classify(l)).unwrap_or(3);
        out[idx] += 1;
    }
    out
}

/// A run of consecutive lines with the same message (time stamps ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Index of the first line of the run.
    pub first: usize,
    /// How many lines in a row carried this message.
    pub count: usize,
}

/// Fold consecutive identical messages. Engine spam such as repeated
/// `Unknown command` or `Missing Vgui material` lines becomes one row with a
/// count.
pub fn collapse_runs(lines: &[String]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut prev: Option<&str> = None;
    for (i, line) in lines.iter().enumerate() {
        let msg = parse(line).message;
        match (prev, runs.last_mut()) {
            (Some(p), Some(run)) if p == msg => run.count += 1,
            _ => runs.push(Run { first: i, count: 1 }),
        }
        prev = Some(msg);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_engine_stamp_and_tag() {
        let p = parse("[1.7858] Error: Material \"x\" uses unknown shader \"DebugLuxels\"");
        assert_eq!(p.time.as_deref(), Some("1.7858"));
        assert_eq!(p.tag, None);
        assert!(p.message.starts_with("Error:"));

        let p = parse("[0.3673] [HL2SB] materialsystem image-cache build Oct 26 2026");
        assert_eq!(p.time.as_deref(), Some("0.3673"));
        assert_eq!(p.tag.as_deref(), Some("hl2sb"));
    }

    #[test]
    fn parses_lua_stamp_and_wide_tag() {
        let p = parse("L 10/27/2026 - 03:03:34: [HL2SB wp] CLIENT weapon bound ent=45");
        assert_eq!(p.time.as_deref(), Some("03:03:34"));
        assert_eq!(p.tag.as_deref(), Some("hl2sb wp"));
        assert_eq!(p.message, "CLIENT weapon bound ent=45");
    }

    #[test]
    fn plain_lines_keep_everything() {
        let p = parse("Loaded.");
        assert_eq!(p.time, None);
        assert_eq!(p.tag, None);
        assert_eq!(p.message, "Loaded.");
        // a bracket that is not a stamp stays in the message
        let p = parse("[not a stamp] text");
        assert_eq!(p.time, None);
        assert_eq!(p.tag.as_deref(), Some("not a stamp"));
        // numeric and bracketed values inside the message are not sources
        let p = parse("[1.0] [102.4269] dynamic player height");
        assert_eq!(p.tag, None);
    }

    #[test]
    fn classifies_real_engine_lines() {
        assert_eq!(classify("[0.8178] Loading unsigned module client.dll"), Level::Plain);
        assert_eq!(classify("[1.7858] Error: Material \"debug/debugluxels\" uses unknown shader \"DebugLuxels\""), Level::Error);
        assert_eq!(classify("[2.1440] Parent cvar in server.dll not allowed (hl2sb_physgun_beamdebug)"), Level::Warn);
        assert_eq!(classify("[5.3148] Unknown command \"hl2sb_ctx_window_width\""), Level::Warn);
        assert_eq!(classify("[3.1225] Unable to access SteamHTMLSurface"), Level::Warn);
        assert_eq!(classify("[2.0976] [HL2SB] Crash handler installed (SEH + SIGABRT)"), Level::Info);
        assert_eq!(classify("[17.5616] [lua] AddCSLuaFile: lua/includes/extensions/coroutine.lua"), Level::Info);
        assert_eq!(classify("[0.1187] LoadLibrary: pModule: video_bink, path: x"), Level::Plain);
    }

    #[test]
    fn classifies_real_lua_lines() {
        assert_eq!(classify("L 10/27/2026 - 03:03:13: [HitNumbers] WARNING: "), Level::Warn);
        assert_eq!(classify("L 10/27/2026 - 03:03:40: lua_error: attempt to index a nil value"), Level::Error);
        assert_eq!(classify("L 10/27/2026 - 03:03:25: === Lua log session (state init, client) ==="), Level::Info);
        assert!(is_session("L 10/27/2026 - 03:03:25: === Lua log session (state init, client) ==="));
        assert!(is_session("[0.0001] Engine(arch:amd64) started at Tue Oct 27 03:02:54 2026"));
        assert!(!is_session("[1.0] just a line"));
    }

    #[test]
    fn decodes_utf8_gbk_and_bom() {
        assert_eq!(decode_bytes("中文 ok".as_bytes()), "中文 ok");
        // "中文" in GBK
        let gbk = [0xD6, 0xD0, 0xCE, 0xC4, b' ', b'o', b'k'];
        assert_eq!(decode_bytes(&gbk), "中文 ok");
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice("hi".as_bytes());
        assert_eq!(decode_bytes(&bom), "hi");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("ok".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(decode_bytes(&utf16), "ok");
    }

    #[test]
    fn binary_strings_finds_ascii_and_utf16() {
        // dumps keep strings on even offsets: ascii at 0, padding to 18, wide at 18
        let mut buf = Vec::new();
        buf.extend_from_slice(b"hl2sb_module.dll");
        buf.extend_from_slice(&[0, 0]);
        // "d:\srceng" in UTF-16LE
        for c in "d:\\srceng".chars() {
            buf.push(c as u8);
            buf.push(0);
        }
        buf.push(0xFF);
        buf.extend_from_slice(b"ab");
        let s = binary_strings(&buf, 4, 100);
        assert!(s.contains(&"hl2sb_module.dll".to_string()));
        assert!(s.contains(&"d:\\srceng".to_string()));
        assert!(!s.iter().any(|x| x == "ab"), "runs shorter than min are dropped");
        // a printable run that is mostly punctuation is noise, not text
        let noise = b"!@#$%^&*()_+{}|:\"<>?~`-=[]\\;',./".to_vec();
        assert!(binary_strings(&noise, 6, 10).is_empty());
    }

    #[test]
    fn collapse_folds_consecutive_repeats_only() {
        let l = lines(&[
            "[1.0] Unknown command \"a\"",
            "[1.1] Unknown command \"a\"",
            "[1.2] Unknown command \"a\"",
            "[1.3] other",
            "[1.4] Unknown command \"a\"",
        ]);
        let runs = collapse_runs(&l);
        assert_eq!(runs, vec![Run { first: 0, count: 3 }, Run { first: 3, count: 1 }, Run { first: 4, count: 1 }]);
    }

    #[test]
    fn matches_and_counts() {
        assert!(matches("Attempt To index", "attempt"));
        assert!(matches("anything", "   "));
        assert!(!matches("abc", "xyz"));
        let l = lines(&["[1.0] Error: boom", "[1.1] Unknown command x", "[1.2] plain", "[1.3] [HL2SB] ok"]);
        assert_eq!(level_counts(&l), [1, 1, 1, 1]);
    }

    #[test]
    fn tail_drops_partial_first_line_and_decodes() {
        let dir = std::env::temp_dir().join("hl2box_tail_test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("t.log");
        let mut data = b"partial cut line\r\n".to_vec();
        data.extend_from_slice(b"[1.0] first\r\n");
        data.extend_from_slice(b"[2.0] ");
        data.extend_from_slice(&[0xD6, 0xD0, 0xCE, 0xC4]);
        data.extend_from_slice(b"\r\n");
        std::fs::write(&p, &data).unwrap();
        let all = tail(&p, 10_000);
        assert_eq!(all.first().map(|s| s.as_str()), Some("partial cut line"), "small file is read whole");
        let small = tail(&p, 40);
        assert!(small.iter().all(|l| !l.contains("partial cut")));
        assert!(small.iter().any(|l| l.ends_with("中文")));
        let _ = std::fs::remove_file(&p);
    }
}

#[cfg(test)]
mod dump_listing_tests {
    use super::*;

    #[test]
    fn dumps_are_listed_newest_first_and_capped() {
        let dir = std::env::temp_dir().join("hl2box_dump_listing_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = std::time::SystemTime::UNIX_EPOCH;
        for (i, name) in ["crash_a.mdmp", "crash_b.mdmp", "crash_c.dmp"].iter().enumerate() {
            let p = dir.join(name);
            std::fs::write(&p, b"x").unwrap();
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_modified(base + std::time::Duration::from_secs(100 * (i as u64 + 1))).unwrap();
        }
        std::fs::write(dir.join("notes.7z"), b"x").unwrap();
        let all: Vec<String> = dump_files(&dir, 10)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(all, vec!["crash_c.dmp", "crash_b.mdmp", "crash_a.mdmp"]);
        assert_eq!(dump_files(&dir, 2).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
