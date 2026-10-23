//! Player identity: display name, steam id and avatar.
//!
//! Stored as JSON under `<mod>/data/hl2box/identity.json` (the game's
//! player-writable data area). The name is handed to the game as `+name`
//! on launch; the steam id rides the command line as `-steamid <id64>`
//! (the engine ignores unknown parameters today, so this is forward
//! compatible). The avatar is a PNG the engine's material loader can read
//! from `materials/vgui/hl2box/avatar.png`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::steamid::SteamId64;
use crate::{err, paths::Paths, Result};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    #[serde(default, serialize_with = "ser_opt_u64", deserialize_with = "de_opt_u64")]
    pub steam_id: Option<u64>,
    /// Relative to the mod root; the canonical copy is avatar.png.
    #[serde(default)]
    pub avatar: Option<String>,
}

impl Default for Identity {
    fn default() -> Self {
        Identity {
            name: "Spectator".into(),
            steam_id: None,
            avatar: None,
        }
    }
}

fn ser_opt_u64<S: serde::Serializer>(v: &Option<u64>, s: S) -> std::result::Result<S::Ok, S::Error> {
    match v {
        Some(n) => s.serialize_some(&n.to_string()),
        None => s.serialize_none(),
    }
}

fn de_opt_u64<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<u64>, D::Error> {
    let raw: Option<String> = Option::deserialize(d)?;
    match raw {
        None => Ok(None),
        Some(s) => s
            .parse::<u64>()
            .map(Some)
            .map_err(|e| serde::de::Error::custom(e)),
    }
}

impl Identity {
    pub fn load(paths: &Paths) -> Result<Identity> {
        let file = paths.data_dir().join("identity.json");
        if !file.is_file() {
            return Ok(Identity::default());
        }
        let text = std::fs::read_to_string(&file)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        paths.ensure_dirs()?;
        let file = paths.data_dir().join("identity.json");
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(file, text + "\n")?;
        Ok(())
    }

    pub fn steam_id(&self) -> Option<SteamId64> {
        self.steam_id.map(SteamId64)
    }

    /// Install a picture as the avatar. The game's AvatarImage looks the
    /// file up by steamid64 first and by player name second (both through
    /// the filesystem-safe key rule), so both files are written - any
    /// scoreboard showing this player finds it no matter which candidate
    /// it probes. Stored as a square 184x184 PNG.
    pub fn install_avatar(&mut self, paths: &Paths, source: &Path) -> Result<()> {
        let img = image::open(source).map_err(|e| crate::Error(format!("image: {e}")))?;
        let rgb = image::DynamicImage::ImageRgb8(to_square_rgb(img));
        let resized = rgb.resize_exact(184, 184, image::imageops::FilterType::Lanczos3);
        paths.ensure_dirs()?;

        let mut written: Vec<PathBuf> = Vec::new();
        if let Some(id) = self.steam_id {
            let p = paths.avatar_png_for(&id.to_string());
            resized
                .save(&p)
                .map_err(|e| crate::Error(format!("image save: {e}")))?;
            written.push(p);
        }
        let name_key = crate::paths::avatar_key(self.name.trim());
        if !name_key.is_empty() {
            let p = paths.avatar_png_for(&name_key);
            if !written.iter().any(|w| w == &p) {
                resized
                    .save(&p)
                    .map_err(|e| crate::Error(format!("image save: {e}")))?;
                written.push(p);
            }
        }
        if written.is_empty() {
            return err("avatar: no usable key (set a name or steam id first)");
        }
        self.avatar = Some(
            written[0]
                .strip_prefix(paths.mod_root())
                .unwrap_or(&written[0])
                .to_string_lossy()
                .replace('\\', "/"),
        );
        Ok(())
    }

    pub fn clear_avatar(&mut self, paths: &Paths) {
        // Best effort: remove both key files; the list field decides what
        // the UI still shows.
        if let Some(id) = self.steam_id {
            let _ = std::fs::remove_file(paths.avatar_png_for(&id.to_string()));
        }
        let name_key = crate::paths::avatar_key(self.name.trim());
        if !name_key.is_empty() {
            let _ = std::fs::remove_file(paths.avatar_png_for(&name_key));
        }
        self.avatar = None;
    }
}

fn to_square_rgb(img: image::DynamicImage) -> image::ImageBuffer<image::Rgb<u8>, Vec<u8>> {
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let side = w.max(h);
    let mut out = image::ImageBuffer::from_pixel(side, side, image::Rgb([0, 0, 0]));
    let dx = (side - w) / 2;
    let dy = (side - h) / 2;
    for (x, y, px) in rgb.enumerate_pixels() {
        out.put_pixel(x + dx, y + dy, *px);
    }
    out
}

/// Absolute path of the installed avatar, if the file still exists.
pub fn avatar_file(paths: &Paths, identity: &Identity) -> Option<PathBuf> {
    // Prefer the recorded file, then the two lookup keys the game probes.
    if let Some(rel) = &identity.avatar {
        let p = if rel.contains('/') || rel.contains('\\') {
            paths.mod_root().join(rel)
        } else {
            paths.avatars_dir().join(rel)
        };
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(id) = identity.steam_id {
        let p = paths.avatar_png_for(&id.to_string());
        if p.is_file() {
            return Some(p);
        }
    }
    let name_key = crate::paths::avatar_key(identity.name.trim());
    if !name_key.is_empty() {
        let p = paths.avatar_png_for(&name_key);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Validate a player name the way the engine's say/hostname paths like it:
/// non-empty, no control characters, sane length.
pub fn validate_name(name: &str) -> Result<()> {
    let n = name.trim();
    if n.is_empty() {
        return err("player name is empty");
    }
    if n.len() > 31 {
        return err("player name longer than 31 bytes");
    }
    if n.chars().any(|c| c.is_control()) {
        return err("player name contains control characters");
    }
    Ok(())
}
