//! Lua script classification for the addon browser.
//!
//! GMod decides where a script runs from its path and its file name. The
//! launcher shows a Lua listing grouped by that realm so a user can see at a
//! glance which side of the connection a file is loaded on.

/// Where a script is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Realm {
    /// `autorun/server/*`, `sv_*.lua`: only the server runs it.
    Server,
    /// `autorun/client/*`, `cl_*.lua`: only the client runs it.
    Client,
    /// Loaded on both sides (`autorun/*`, `shared.lua`, `entities/`, ...).
    Shared,
    /// `lua/menu/*`: runs in the main menu realm.
    Menu,
    /// A `.lua` file that matches none of the rules above.
    Other,
}

impl Realm {
    pub fn key(self) -> &'static str {
        match self {
            Realm::Server => "server",
            Realm::Client => "client",
            Realm::Shared => "shared",
            Realm::Menu => "menu",
            Realm::Other => "other",
        }
    }

    /// Stable ordering used when the browser lists realms.
    pub const ALL: [Realm; 5] = [
        Realm::Shared,
        Realm::Server,
        Realm::Client,
        Realm::Menu,
        Realm::Other,
    ];
}

/// True when the path names a Lua file.
pub fn is_lua(path: &str) -> bool {
    path.rsplit(['/', '\\'])
        .next()
        .map(|name| name.to_ascii_lowercase().ends_with(".lua"))
        .unwrap_or(false)
}

/// Classify a Lua file by path. `None` when the path is not a Lua file.
///
/// Directory rules win over file name rules: a `cl_init.lua` inside
/// `autorun/server/` still runs on the server.
pub fn realm_of(path: &str) -> Option<Realm> {
    if !is_lua(path) {
        return None;
    }
    let norm = path.replace('\\', "/").to_ascii_lowercase();
    let norm = norm.trim_start_matches("./").trim_start_matches('/');
    let padded = format!("/{norm}");

    if padded.contains("/autorun/server/") {
        return Some(Realm::Server);
    }
    if padded.contains("/autorun/client/") {
        return Some(Realm::Client);
    }
    if padded.contains("/lua/menu/") || padded.starts_with("/menu/") {
        return Some(Realm::Menu);
    }

    let file = norm.rsplit('/').next().unwrap_or(norm);
    if file.starts_with("sv_") {
        return Some(Realm::Server);
    }
    if file.starts_with("cl_") {
        return Some(Realm::Client);
    }
    if file == "shared.lua" || file == "init.lua" {
        return Some(Realm::Shared);
    }

    const SHARED_DIRS: [&str; 9] = [
        "/autorun/",
        "/entities/",
        "/weapons/",
        "/effects/",
        "/vgui/",
        "/includes/",
        "/gamemodes/",
        "/stools/",
        "/matproxy/",
    ];
    if SHARED_DIRS.iter().any(|d| padded.contains(d)) {
        return Some(Realm::Shared);
    }
    Some(Realm::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_detection() {
        assert!(is_lua("lua/autorun/x.lua"));
        assert!(is_lua("LUA\\AUTORUN\\X.LUA"));
        assert!(!is_lua("materials/x.vmt"));
        assert!(!is_lua("lua/autorun/x.lua.bak"));
    }

    #[test]
    fn directory_rules_win() {
        assert_eq!(realm_of("lua/autorun/server/a.lua"), Some(Realm::Server));
        assert_eq!(realm_of("lua/autorun/server/cl_init.lua"), Some(Realm::Server));
        assert_eq!(realm_of("lua/autorun/client/hud.lua"), Some(Realm::Client));
        assert_eq!(realm_of("lua/menu/menu.lua"), Some(Realm::Menu));
    }

    #[test]
    fn file_name_rules() {
        assert_eq!(realm_of("lua/entities/gun/sv_fire.lua"), Some(Realm::Server));
        assert_eq!(realm_of("lua/entities/gun/cl_hud.lua"), Some(Realm::Client));
        assert_eq!(realm_of("lua/entities/gun/shared.lua"), Some(Realm::Shared));
        assert_eq!(realm_of("lua/entities/gun/init.lua"), Some(Realm::Shared));
    }

    #[test]
    fn shared_and_other() {
        assert_eq!(realm_of("lua/weapons/gun/x.lua"), Some(Realm::Shared));
        assert_eq!(realm_of("lua/somewhere/x.lua"), Some(Realm::Other));
        assert_eq!(realm_of("materials/x.vmt"), None);
    }
}
