//! Log viewing: tail the log files the game already writes.
//!
//! The mod directory carries `hl2sb_lua.log` (Lua output and errors) and
//! `ds_debug.log` (server debug lines); the engine writes `console.log`
//! into the mod directory when `-condebug` / `con_log` is on. Files are
//! only read - the launcher never fights the game for the write handle,
//! and a file being written mid-read is handled by decoding lossily and
//! dropping the partial trailing line.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::Paths;

/// Log sources offered in the UI, in display order.
pub fn known_logs(paths: &Paths) -> Vec<LogSource> {
    let mod_root = paths.mod_root();
    let engine_root = paths.engine_root.clone();
    let mut out = vec![
        LogSource {
            id: "lua".into(),
            title: "Lua log".into(),
            path: mod_root.join("hl2sb_lua.log"),
        },
        LogSource {
            id: "server".into(),
            title: "Server debug log".into(),
            path: mod_root.join("ds_debug.log"),
        },
        LogSource {
            id: "console".into(),
            title: "Console log (con_log / -condebug)".into(),
            path: mod_root.join("console.log"),
        },
        LogSource {
            id: "engine".into(),
            title: "Engine log (engine_root)".into(),
            path: engine_root.join("hl2sb_engine.log"),
        },
    ];
    // Crash dumps deserve a one-click view too.
    let dumps = mod_root.join("dumps");
    if dumps.is_dir() {
        if let Some(newest) = newest_file(&dumps, &["mdmp", "dmp", "txt"]) {
            out.push(LogSource {
                id: "dump".into(),
                title: "Newest crash dump report".into(),
                path: newest,
            });
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSource {
    pub id: String,
    pub title: String,
    pub path: PathBuf,
}

/// Read the tail of a log: last `max_bytes` of the file, split into lines,
/// oldest first. Returns an empty vector when the file does not exist
/// (yet) - the UI treats that as "no output so far".
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
    // Chunked read instead of one giant allocation.
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
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.split('\n').map(|s| s.trim_end_matches('\r')).map(String::from).collect();
    if start > 0 && !lines.is_empty() {
        // First line is likely cut in half; drop it.
        lines.remove(0);
    }
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines
}

fn newest_file(dir: &Path, exts: &[&str]) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for ent in std::fs::read_dir(dir).ok()?.flatten() {
        let p = ent.path();
        if !p.is_file() {
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !exts.contains(&ext.to_ascii_lowercase().as_str()) {
            continue;
        }
        let m = ent.metadata().ok().and_then(|m| m.modified().ok());
        if let Some(t) = m {
            if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
                best = Some((t, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// How a log line reads at a glance. Drives the colour in the viewer and the
/// level filter; purely textual, so it works on any engine output.
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

/// Classify one line. Errors win over warnings, warnings over info markers.
/// The patterns cover Lua runtime errors ("attempt to", "stack traceback"),
/// engine "Error" lines and the launcher's own "[HL2SB]" prefix.
pub fn classify(line: &str) -> Level {
    let lower = line.to_ascii_lowercase();
    const ERROR: [&str; 7] = [
        "error",
        "attempt to ",
        "stack traceback",
        "exception",
        "fatal",
        "assertion failed",
        "segfault",
    ];
    const WARN: [&str; 3] = ["warning", "warn:", "deprecated"];
    const INFO: [&str; 3] = ["[hl2sb]", "info:", "loaded "];
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

/// Case-insensitive substring match. An empty query matches everything.
pub fn matches(line: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    line.to_lowercase().contains(&q.to_lowercase())
}

/// Count of lines per level, for the filter chips.
pub fn level_counts(lines: &[String]) -> [usize; 4] {
    let mut out = [0usize; 4];
    for l in lines {
        let idx = match classify(l) {
            Level::Error => 0,
            Level::Warn => 1,
            Level::Info => 2,
            Level::Plain => 3,
        };
        out[idx] += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_of_missing_file_is_empty() {
        assert!(tail(Path::new("Z:/definitely/not/here.log"), 1024).is_empty());
    }

    #[test]
    fn tail_reads_lines() {
        let dir = std::env::temp_dir();
        let p = dir.join("hl2box_tail_test.log");
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        let lines = tail(&p, 4096);
        assert_eq!(lines, vec!["one", "two", "three"]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn classify_levels() {
        assert_eq!(classify("lua_error: attempt to index a nil value"), Level::Error);
        assert_eq!(classify("[HL2SB] menu page loaded"), Level::Info);
        assert_eq!(classify("WARNING: something odd"), Level::Warn);
        assert_eq!(classify("error wins over warning here warn"), Level::Error);
        assert_eq!(classify("just a normal line"), Level::Plain);
    }

    #[test]
    fn matches_is_case_insensitive() {
        assert!(matches("Attempt To index", "attempt"));
        assert!(matches("anything", "   "));
        assert!(!matches("abc", "xyz"));
    }

    #[test]
    fn counts_levels() {
        let lines: Vec<String> = ["error a", "warning b", "plain c", "[hl2sb] d", "plain e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(level_counts(&lines), [1, 1, 1, 2]);
    }
}
