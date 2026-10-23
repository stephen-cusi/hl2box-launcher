//! Version reporting.
//!
//! Sources, best first:
//! - `<engine root>/version.txt` (single line) - written by release builds.
//! - The engine executable's modification time as a build hint.
//! - The launcher's own version + build stamp (baked in by build.rs).

use std::path::Path;
use std::time::UNIX_EPOCH;

pub const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn launcher_build() -> &'static str {
    option_env!("HL2BOX_BUILD_DATE").unwrap_or("dev")
}

/// Engine version string when it can be determined, else None.
pub fn engine_version(engine_root: &Path) -> Option<String> {
    let txt = engine_root.join("version.txt");
    if let Ok(s) = std::fs::read_to_string(&txt) {
        let t = s.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }

    let exe = engine_root.join(if cfg!(windows) {
        "hl2_launcher.exe"
    } else {
        "hl2_launcher"
    });
    let meta = std::fs::metadata(exe).ok()?;
    let modified = meta.modified().ok()?;
    let secs = modified
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .ok()?;
    // ISO-ish stamp without pulling in chrono.
    Some(format!("build {}", stamp(secs as i64)))
}

fn stamp(secs: i64) -> String {
    // Civil-from-days algorithm (Howard Hinnant), enough for a date label.
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}
