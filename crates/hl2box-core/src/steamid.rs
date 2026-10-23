//! SteamID parsing, rendering and generation.
//!
//! Formulas mirror the engine's own Lua bindings (util.SteamIDTo64 /
//! SteamIDFrom64): the 64-bit form is `76561197960265728 + authserver +
//! account_id * 2`, which is the public-universe individual-account layout.

use rand::Rng;

use crate::{err, Result};

pub const STEAM_ID64_BASE: u64 = 7_656_119_796_026_5728;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SteamId64(pub u64);

impl SteamId64 {
    pub fn account_id(&self) -> u64 {
        (self.0 - STEAM_ID64_BASE) & 0xFFFF_FFFF
    }

    /// Y in STEAM_X:Y:Z.
    pub fn auth_server(&self) -> u64 {
        (self.0 - STEAM_ID64_BASE) & 1
    }

    pub fn is_valid(&self) -> bool {
        self.0 >= STEAM_ID64_BASE
            && self.0 < STEAM_ID64_BASE + (1u64 << 32)
            && self.account_id() != 0
    }

    /// `STEAM_0:Y:Z` (the engine renders universe 0 on this fork).
    pub fn steam2(&self) -> String {
        format!("STEAM_0:{}:{}", self.auth_server(), self.account_id() / 2)
    }

    /// `[U:1:W]` friend-chat form.
    pub fn u_form(&self) -> String {
        format!("[U:1:{}]", self.account_id())
    }

    pub fn render(&self) -> String {
        self.0.to_string()
    }

    /// Build from the raw account id (Z*2 + Y domain).
    pub fn from_account_id(account_id: u64) -> Self {
        SteamId64(STEAM_ID64_BASE + (account_id & 0xFFFF_FFFF))
    }

    /// Random individual account, same shape real Steam ids have.
    pub fn random() -> Self {
        // Account ids observed in the wild live below ~ 250 million;
        // sampling below 2^31 keeps the result believable.
        let account: u32 = rand::rng().random_range(1..u32::MAX / 2);
        SteamId64::from_account_id(account as u64)
    }

    /// Accepts every form a user is likely to paste: the plain 64-bit
    /// decimal, `STEAM_X:Y:Z` and `[U:1:N]` (brackets optional).
    pub fn parse(input: &str) -> Result<Self> {
        let s = input.trim();
        if s.is_empty() {
            return err("empty steam id");
        }

        if let Some(rest) = s.strip_prefix('[').map(|r| r.trim_end_matches(']')) {
            if let Some(num) = rest
                .strip_prefix("U:1:")
                .and_then(|n| n.trim().parse::<u64>().ok())
            {
                let id = SteamId64::from_account_id(num);
                if id.is_valid() {
                    return Ok(id);
                }
            }
            return err(format!("unrecognized steam id form: {s}"));
        }

        let upper = s.to_ascii_uppercase();
        if upper.starts_with("STEAM_") {
            let parts: Vec<&str> = upper.split(':').collect();
            if parts.len() == 3 {
                if let (Ok(_universe), Ok(auth), Ok(account)) = (
                    parts[0].trim_start_matches("STEAM_").parse::<u32>(),
                    parts[1].parse::<u32>(),
                    parts[2].parse::<u64>(),
                ) {
                    if auth <= 1 {
                        let id = SteamId64(STEAM_ID64_BASE + (account * 2) + auth as u64);
                        if id.is_valid() {
                            return Ok(id);
                        }
                    }
                }
            }
            return err(format!("bad STEAM_ form: {s}"));
        }

        match s.parse::<u64>() {
            Ok(n) => {
                let id = SteamId64(n);
                if id.is_valid() {
                    Ok(id)
                } else {
                    err(format!("steam id out of range: {s}"))
                }
            }
            Err(_) => err(format!("unrecognized steam id form: {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_forms() {
        // A well-known id: 76561197960287930 == STEAM_0:0:11101
        let id = SteamId64(7_656_119_796_028_7930);
        assert_eq!(id.steam2(), "STEAM_0:0:11101");
        assert_eq!(SteamId64::parse("STEAM_0:0:11101").unwrap(), id);
        assert_eq!(SteamId64::parse("76561197960287930").unwrap(), id);
        assert_eq!(SteamId64::parse("[U:1:22202]").unwrap(), id);
        assert_eq!(id.account_id(), 22202);
    }

    #[test]
    fn random_is_valid() {
        for _ in 0..64 {
            assert!(SteamId64::random().is_valid());
        }
    }
}
