//! Steam Workshop downloads without a Steam client.
//!
//! Two routes, picked per item:
//!
//! 1. **Direct link.** `ISteamRemoteStorage/GetPublishedFileDetails` (anonymous,
//!    no key) returns a `file_url` for most public items: a plain HTTPS GET of
//!    a `.gma`. This is the fast path and needs nothing else installed.
//! 2. **SteamCMD, anonymous.** Some items have only an `hcontent_file` and no
//!    `file_url`. For those the launcher runs SteamCMD's
//!    `workshop_download_item 4000 <id>` as `login anonymous` (Garry's Mod is
//!    app 4000) into a scratch folder and takes the addon file from
//!    `steamapps/workshop/content/4000/<id>/`. SteamCMD is Valve's own tool and
//!    is not bundled; the launcher finds it from the configured path or PATH.
//!
//! Either way the result must start with the `GMAD` magic. Legacy workshop
//! items can ship LZMA-compressed `.bin` files; those are reported as such
//! instead of being installed as broken addons.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer, Serialize};

use crate::{err, Result};

/// Garry's Mod's Steam app id: the workshop content folder is named after it.
pub const GMOD_APPID: &str = "4000";

/// Largest addon the launcher will accept (a guard against a wrong URL).
pub const MAX_ADDON_BYTES: u64 = 2 << 30;

/// SteamCMD run limit. The first run of a fresh SteamCMD downloads its own
/// updates and can take several minutes on top of the item itself.
pub const STEAMCMD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Accept either a JSON string or a number: the Web API is inconsistent about
/// which fields it quotes.
fn flex_string<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    })
}

fn flex_i64<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<i64, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    })
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkshopDetails {
    pub publishedfileid: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub title: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub file_size: String,
    /// Empty for items that only exist as an `hcontent_file` (SteamCMD route).
    #[serde(default, deserialize_with = "flex_string")]
    pub file_url: String,
    #[serde(default, deserialize_with = "flex_string")]
    pub hcontent_file: String,
    /// The file name the author uploaded, when Steam reports it.
    #[serde(default, deserialize_with = "flex_string")]
    pub filename: String,
    #[serde(default, deserialize_with = "flex_i64")]
    pub time_updated: i64,
    #[serde(default, deserialize_with = "flex_string")]
    pub creator: String,
    #[serde(default)]
    pub tags: Vec<WorkshopTag>,
    /// Steam's result code for the lookup: 1 means the item was found.
    #[serde(default, deserialize_with = "flex_i64")]
    pub result: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkshopTag {
    #[serde(default)]
    pub tag: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DetailsResponse {
    #[serde(default)]
    response: DetailsBody,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DetailsBody {
    #[serde(default)]
    publishedfiledetails: Vec<WorkshopDetails>,
}

/// How an item will be fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `file_url` is present: stream it.
    Direct,
    /// No direct link: anonymous SteamCMD.
    SteamCmd,
}

pub fn route(d: &WorkshopDetails) -> Route {
    if d.file_url.is_empty() {
        Route::SteamCmd
    } else {
        Route::Direct
    }
}

/// Accepts a bare id ("2968968802"), a full workshop URL or a query URL.
pub fn extract_workshop_id(input: &str) -> Result<String> {
    let s = input.trim();
    if s.is_empty() {
        return err("empty workshop id");
    }
    if let Some(pos) = s.find("?id=") {
        let tail = &s[pos + 4..];
        let id: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let trimmed = s.trim_end_matches('/');
    if let Some(pos) = trimmed.rfind('/') {
        let tail = &trimmed[pos + 1..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            return Ok(tail.to_string());
        }
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return Ok(s.to_string());
    }
    err(format!("cannot find a workshop id in: {s}"))
}

/// Look the item up. An item that exists but has no direct link is still a
/// valid result (the SteamCMD route handles it); only a missing item is an error.
pub fn fetch_details(id: &str) -> Result<WorkshopDetails> {
    let body = format!("itemcount=1&publishedfileids[0]={id}");
    let (status, bytes) = crate::http::post_form(
        "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/",
        &body,
        Duration::from_secs(20),
    )?;
    if status != 200 {
        return err(format!("workshop api: http status {status}"));
    }
    let parsed: DetailsResponse =
        serde_json::from_slice(&bytes).map_err(|e| crate::Error(format!("workshop api json: {e}")))?;
    let d = parsed
        .response
        .publishedfiledetails
        .into_iter()
        .next()
        .ok_or_else(|| crate::Error("workshop api: no details returned".into()))?;
    if d.result != 1 {
        return err(format!(
            "workshop item {id}: not found or not public (steam result {})",
            d.result
        ));
    }
    Ok(d)
}

/// Whether a file starts with the `GMAD` magic every addon archive carries.
pub fn check_gma(path: &Path) -> Result<()> {
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 4];
    let n = f.read(&mut head)?;
    if n == 4 && &head == b"GMAD" {
        return Ok(());
    }
    let shown: Vec<String> = head[..n].iter().map(|b| format!("{b:02X}")).collect();
    err(format!(
        "not a GMA archive (first bytes {}). Legacy workshop items can be LZMA-compressed .bin files; \
         those are not supported yet",
        shown.join(" ")
    ))
}

/// Download a direct-link item into `dest`. Streams to `<dest>.part`, checks
/// the GMAD magic, then renames. `progress` receives the running byte count
/// and the total when Steam reported one.
pub fn download_direct(
    details: &WorkshopDetails,
    dest: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<u64> {
    if details.file_url.is_empty() {
        return err("this item has no direct download link");
    }
    let (status, mut reader) =
        crate::http::get_stream(&details.file_url, Duration::from_secs(300))?;
    if status != 200 {
        return err(format!("workshop download: http status {status}"));
    }
    let total: Option<u64> = details.file_size.parse::<u64>().ok().filter(|n| *n > 0);

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part)?;
    let mut buf = [0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| crate::Error(format!("workshop download read: {e}")))?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n])?;
        written += n as u64;
        if written > MAX_ADDON_BYTES {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return err("workshop download: larger than the 2 GiB limit");
        }
        progress(written, total);
    }
    drop(file);
    if written == 0 {
        let _ = std::fs::remove_file(&part);
        return err("workshop download: empty file");
    }
    if let Err(e) = check_gma(&part) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, dest)?;
    Ok(written)
}

/// Kept for the C API: direct download only.
pub fn download_to(
    details: &WorkshopDetails,
    dest: &Path,
    progress: impl FnMut(u64, Option<u64>),
) -> Result<u64> {
    download_direct(details, dest, progress)
}

/// Find a SteamCMD executable: the configured file, a folder containing one,
/// or the first match on PATH.
pub fn find_steamcmd(configured: &str) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["steamcmd.exe"]
    } else {
        &["steamcmd.sh", "steamcmd"]
    };
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if p.is_file() {
            return Some(p);
        }
        for n in names {
            let c = p.join(n);
            if c.is_file() {
                return Some(c);
            }
        }
        return None;
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for n in names {
            let c = dir.join(n);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// Run SteamCMD anonymously for one workshop item and return the addon file
/// it produced. `work` is a scratch folder SteamCMD installs into, so nothing
/// touches a user's real SteamCMD install.
pub fn steamcmd_download(exe: &Path, id: &str, work: &Path, timeout: Duration) -> Result<PathBuf> {
    std::fs::create_dir_all(work)?;
    let mut child = Command::new(exe)
        .arg("+force_install_dir")
        .arg(work)
        .arg("+login")
        .arg("anonymous")
        .arg("+workshop_download_item")
        .arg(GMOD_APPID)
        .arg(id)
        .arg("+quit")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| crate::Error(format!("could not start steamcmd: {e}")))?;

    // Keep the tail of the output: it is the only explanation SteamCMD gives
    // when an item is refused.
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let mut readers = Vec::new();
    for pipe in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tail = Arc::clone(&tail);
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(std::result::Result::ok) {
                if let Ok(mut t) = tail.lock() {
                    t.push_back(line);
                    if t.len() > 25 {
                        t.pop_front();
                    }
                }
            }
        }));
    }

    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return err(format!(
                "steamcmd did not finish within {} s and was stopped",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    for r in readers {
        let _ = r.join();
    }

    if let Some(file) = find_item_file(work, id) {
        return Ok(file);
    }
    let last: Vec<String> = tail.lock().map(|t| t.iter().cloned().collect()).unwrap_or_default();
    let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
    err(format!(
        "steamcmd (exit {code}) produced no file for item {id}. Last output:\n{}",
        last.join("\n")
    ))
}

/// The addon file SteamCMD left for `id`: a `.gma` (or `.bin`) inside the item
/// folder. The folder is looked up at the usual place first, then anywhere
/// under `work`, in case SteamCMD changed its layout.
pub fn find_item_file(work: &Path, id: &str) -> Option<PathBuf> {
    let usual = work.join("steamapps").join("workshop").join("content").join(GMOD_APPID).join(id);
    let dir = if usual.is_dir() {
        usual
    } else {
        find_dir_named(work, id, 6)?
    };
    let mut best: Option<(u8, u64, PathBuf)> = None;
    for ent in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = ent.path();
        if !p.is_file() {
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let rank = match ext.as_str() {
            "gma" => 0,
            "bin" => 1,
            _ => continue,
        };
        let size = ent.metadata().map(|m| m.len()).unwrap_or(0);
        let better = match &best {
            None => true,
            Some((r, s, _)) => rank < *r || (rank == *r && size > *s),
        };
        if better {
            best = Some((rank, size, p));
        }
    }
    best.map(|(_, _, p)| p)
}

fn find_dir_named(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    for ent in std::fs::read_dir(root).ok()?.flatten() {
        let p = ent.path();
        if !p.is_dir() {
            continue;
        }
        if p.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(p);
        }
        if let Some(found) = find_dir_named(&p, name, depth - 1) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(url: &str) -> WorkshopDetails {
        WorkshopDetails {
            publishedfileid: "1".into(),
            title: "t".into(),
            file_size: String::new(),
            file_url: url.into(),
            hcontent_file: String::new(),
            filename: String::new(),
            time_updated: 0,
            creator: String::new(),
            tags: Vec::new(),
            result: 1,
        }
    }

    #[test]
    fn ids_from_urls_and_bare_numbers() {
        assert_eq!(extract_workshop_id("2968968802").unwrap(), "2968968802");
        assert_eq!(
            extract_workshop_id("https://steamcommunity.com/sharedfiles/filedetails/?id=1162103281&searchtext=").unwrap(),
            "1162103281"
        );
        assert_eq!(extract_workshop_id("https://steamcommunity.com/workshop/filedetails/1162103281/").unwrap(), "1162103281");
        assert!(extract_workshop_id("   ").is_err());
        assert!(extract_workshop_id("hello").is_err());
    }

    #[test]
    fn route_follows_the_direct_link() {
        assert_eq!(route(&details("https://cdn.example/x.gma")), Route::Direct);
        assert_eq!(route(&details("")), Route::SteamCmd);
    }

    #[test]
    fn api_fields_accept_strings_or_numbers() {
        let json = r#"{"response":{"publishedfiledetails":[{"result":1,"publishedfileid":"42",
            "title":"Box","file_size":1234,"file_url":"","hcontent_file":"555","time_updated":"1700000000",
            "creator":76561198000000000,"tags":[{"tag":"Addon"}]}]}}"#;
        let parsed: DetailsResponse = serde_json::from_str(json).unwrap();
        let d = &parsed.response.publishedfiledetails[0];
        assert_eq!(d.file_size, "1234");
        assert_eq!(d.time_updated, 1_700_000_000);
        assert_eq!(d.creator, "76561198000000000");
        assert_eq!(d.hcontent_file, "555");
        assert_eq!(route(d), Route::SteamCmd);
    }

    #[test]
    fn gma_magic_is_checked() {
        let dir = std::env::temp_dir().join("hl2box_gma_magic_test");
        let _ = std::fs::create_dir_all(&dir);
        let good = dir.join("good.gma");
        std::fs::write(&good, b"GMAD\x03rest").unwrap();
        assert!(check_gma(&good).is_ok());
        let bad = dir.join("bad.bin");
        std::fs::write(&bad, [0x5D, 0x00, 0x00, 0x80, 0x00]).unwrap();
        let msg = check_gma(&bad).unwrap_err().0;
        assert!(msg.contains("5D 00 00 80"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn steamcmd_is_found_in_a_configured_folder() {
        let dir = std::env::temp_dir().join("hl2box_steamcmd_find_test");
        let _ = std::fs::create_dir_all(&dir);
        let name = if cfg!(windows) { "steamcmd.exe" } else { "steamcmd.sh" };
        std::fs::write(dir.join(name), b"").unwrap();
        assert_eq!(find_steamcmd(dir.to_str().unwrap()), Some(dir.join(name)));
        assert_eq!(find_steamcmd(dir.join("missing").to_str().unwrap()), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stand-in for SteamCMD: it reads `+force_install_dir` and writes the
    /// item the way the real client lays it out, so the whole flow runs here.
    #[cfg(unix)]
    #[test]
    fn steamcmd_flow_with_a_fake_client() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join("hl2box_steamcmd_flow_test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let fake = base.join("steamcmd.sh");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             dir=\"\"; id=\"\"\n\
             while [ $# -gt 0 ]; do\n\
               case \"$1\" in\n\
                 +force_install_dir) dir=\"$2\"; shift;;\n\
                 +workshop_download_item) id=\"$3\"; shift 2;;\n\
               esac\n\
               shift\n\
             done\n\
             d=\"$dir/steamapps/workshop/content/4000/$id\"\n\
             mkdir -p \"$d\"\n\
             printf 'GMAD\\003fake' > \"$d/box_$id.gma\"\n\
             echo \"Success. Downloaded item $id\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let work = base.join("work");
        let file = steamcmd_download(&fake, "777", &work, Duration::from_secs(20)).unwrap();
        assert!(file.ends_with("box_777.gma"), "{file:?}");
        assert!(check_gma(&file).is_ok());

        // a client that never finishes is stopped at the timeout
        let slow = base.join("slow.sh");
        std::fs::write(&slow, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
        let t = Instant::now();
        let e = steamcmd_download(&slow, "1", &base.join("w2"), Duration::from_secs(2)).unwrap_err().0;
        assert!(e.contains("did not finish"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(20));
        let _ = std::fs::remove_dir_all(&base);
    }
}
