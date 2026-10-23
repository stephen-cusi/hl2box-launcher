//! Steam Workshop downloads without a Steam client.
//!
//! Two anonymous, key-less endpoints are enough:
//!  1. ISteamRemoteStorage/GetPublishedFileDetails (POST) for the title,
//!     size and file url of a published file.
//!  2. A plain GET of that file url (a CDN .gma).
//!
//! No login, no subscription, no cache games - exactly the part a no-steam
//! install cannot do by itself.

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{err, Result};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkshopDetails {
    pub publishedfileid: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub file_size: String,
    #[serde(default)]
    pub file_url: String,
    #[serde(default)]
    pub time_updated: i64,
    #[serde(default)]
    pub creator: String,
    #[serde(default)]
    pub tags: Vec<WorkshopTag>,
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
    if let Some(pos) = s.rfind('/') {
        let tail = &s[pos + 1..];
        if tail.chars().all(|c| c.is_ascii_digit()) && !tail.is_empty() {
            return Ok(tail.to_string());
        }
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return Ok(s.to_string());
    }
    err(format!("cannot find a workshop id in: {s}"))
}

pub fn fetch_details(id: &str) -> Result<WorkshopDetails> {
    let body = format!("itemcount=1&publishedfileids[0]={id}");
    let (status, bytes) = crate::http::post_form(
        "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/",
        &body,
        std::time::Duration::from_secs(20),
    )?;
    if status != 200 {
        return err(format!("workshop api: http status {status}"));
    }
    let parsed: DetailsResponse =
        serde_json::from_slice(&bytes).map_err(|e| crate::Error(format!("workshop api json: {e}")))?;
    let mut list = parsed.response.publishedfiledetails;
    let d = list
        .pop()
        .ok_or_else(|| crate::Error("workshop api: no details returned".into()))?;
    if d.file_url.is_empty() {
        return err(format!("workshop item {} exposes no downloadable file", d.publishedfileid));
    }
    Ok(d)
}

/// Download the addon to `dest` (a .gma path). `progress` receives the
/// running byte count and the total, when known.
pub fn download_to(
    details: &WorkshopDetails,
    dest: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<u64> {
    let (status, mut reader) =
        crate::http::get_stream(&details.file_url, std::time::Duration::from_secs(300))?;
    if status != 200 {
        return err(format!("workshop download: http status {status}"));
    }
    let total: Option<u64> = details
        .file_size
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0);

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(dest)?;
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
        progress(written, total);
    }
    if written == 0 {
        let _ = std::fs::remove_file(dest);
        return err("workshop download: empty file");
    }
    Ok(written)
}
