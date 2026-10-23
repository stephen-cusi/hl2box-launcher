//! Launcher configuration and game start.
//!
//! The launcher keeps its own settings next to its executable
//! (hl2box-launcher.json). Building the game command line is pure logic
//! so Android (or a future CLI) can reuse it verbatim.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity::Identity;
use crate::paths::Paths;
use crate::{err, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherConfig {
    pub engine_root: String,
    pub mod_dir: String,
    /// Free-form extra arguments, e.g. "-windowed -w 1280 -h 720 +map gm_construct".
    pub extra_args: String,
    #[serde(default = "default_true")]
    pub pass_name: bool,
    #[serde(default = "default_true")]
    pub pass_steamid: bool,
    #[serde(default)]
    pub start_map: String,
    /// "auto" | "zh" | "en" - UI language (auto follows the OS locale).
    #[serde(default = "default_lang")]
    pub language: String,
    /// "auto" | "light" | "dark" - UI theme (auto follows the OS).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Named sets of extra args + start map, managed from the launch page.
    #[serde(default)]
    pub presets: Vec<LaunchPreset>,
}

/// A saved launch setting: extra command line and start map under one name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchPreset {
    pub name: String,
    pub extra_args: String,
    pub start_map: String,
}

/// Preset names: 1 to 40 characters, no control characters.
pub fn valid_preset_name(name: &str) -> bool {
    let t = name.trim();
    !t.is_empty() && t.chars().count() <= 40 && !t.chars().any(|c| c.is_control())
}

/// Insert or replace (names compare case-insensitively), keeping the list sorted.
pub fn upsert_preset(cfg: &mut LauncherConfig, preset: LaunchPreset) {
    if let Some(p) = cfg.presets.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&preset.name)) {
        *p = preset;
    } else {
        cfg.presets.push(preset);
    }
    cfg.presets.sort_by_key(|p| p.name.to_lowercase());
}

/// Remove by name. Returns whether anything was removed.
pub fn remove_preset(cfg: &mut LauncherConfig, name: &str) -> bool {
    let before = cfg.presets.len();
    cfg.presets.retain(|p| !p.name.eq_ignore_ascii_case(name));
    cfg.presets.len() != before
}

fn default_true() -> bool {
    true
}

fn default_lang() -> String {
    "auto".into()
}

fn default_theme() -> String {
    "auto".into()
}

impl Default for LauncherConfig {
    fn default() -> Self {
        LauncherConfig {
            engine_root: String::new(),
            mod_dir: "hl2sb".into(),
            extra_args: String::new(),
            pass_name: true,
            pass_steamid: true,
            start_map: String::new(),
            language: default_lang(),
            theme: default_theme(),
            presets: Vec::new(),
        }
    }
}

impl LauncherConfig {
    pub fn file_path() -> Result<PathBuf> {
        let exe = std::env::current_exe()?;
        Ok(exe
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("hl2box-launcher.json"))
    }

    pub fn load() -> Result<LauncherConfig> {
        let path = Self::file_path()?;
        if !path.is_file() {
            let mut cfg = LauncherConfig::default();
            cfg.engine_root = guess_engine_root();
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::file_path()?;
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text + "\n")?;
        Ok(())
    }

    pub fn paths(&self) -> Paths {
        Paths::new(&self.engine_root, &self.mod_dir)
    }
}

/// Where the game probably lives when no config exists yet: next to the
/// launcher executable, or one level up (launcher in a tools/ subdir).
pub fn guess_engine_root() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join(if cfg!(windows) {
                "hl2_launcher.exe"
            } else {
                "hl2_launcher"
            })
            .is_file()
            {
                return dir.to_string_lossy().into_owned();
            }
            if let Some(parent) = dir.parent() {
                if parent.join("hl2sb").is_dir() {
                    return parent.to_string_lossy().into_owned();
                }
            }
        }
    }
    String::new()
}

/// Split a command line the way the engine does: whitespace separated,
/// double quotes group, backslash is not an escape.
pub fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in line.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Something the pre-flight check found wrong with the current settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Issue {
    EngineRootMissing,
    GameExeMissing,
    ModDirMissing,
    StartMapInvalid,
    ExtraArgsUnbalancedQuotes,
    NoSteamId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The game will not start; the Launch button is disabled.
    Error,
    /// It will start, but something is probably not what the user wants.
    Warning,
}

impl Issue {
    pub fn severity(self) -> Severity {
        match self {
            Issue::EngineRootMissing
            | Issue::GameExeMissing
            | Issue::StartMapInvalid
            | Issue::ExtraArgsUnbalancedQuotes => Severity::Error,
            Issue::ModDirMissing | Issue::NoSteamId => Severity::Warning,
        }
    }

    /// Stable key for the UI's translation table.
    pub fn key(self) -> &'static str {
        match self {
            Issue::EngineRootMissing => "launch.issue.engine_root",
            Issue::GameExeMissing => "launch.issue.game_exe",
            Issue::ModDirMissing => "launch.issue.mod_dir",
            Issue::StartMapInvalid => "launch.issue.start_map",
            Issue::ExtraArgsUnbalancedQuotes => "launch.issue.quotes",
            Issue::NoSteamId => "launch.issue.steamid",
        }
    }
}

/// Map names are one token of letters, digits, `_`, `-` or `/`, at most 64
/// characters. Anything else would be handed to the engine as a bogus `+map`.
pub fn valid_map_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/'))
}

/// Check the settings without starting anything. Errors come first.
pub fn validate(cfg: &LauncherConfig, identity: &Identity) -> Vec<Issue> {
    let paths = cfg.paths();
    let mut out = Vec::new();
    if cfg.engine_root.trim().is_empty() || !paths.engine_root.is_dir() {
        out.push(Issue::EngineRootMissing);
    } else if !paths.game_exe().is_file() {
        out.push(Issue::GameExeMissing);
    }
    if cfg.mod_dir.trim().is_empty() || !paths.mod_root().is_dir() {
        out.push(Issue::ModDirMissing);
    }
    let map = cfg.start_map.trim();
    if !map.is_empty() && !valid_map_name(map) {
        out.push(Issue::StartMapInvalid);
    }
    if cfg.extra_args.matches('"').count() % 2 == 1 {
        out.push(Issue::ExtraArgsUnbalancedQuotes);
    }
    if cfg.pass_steamid && identity.steam_id.is_none() {
        out.push(Issue::NoSteamId);
    }
    out.sort_by_key(|i| i.severity() != Severity::Error);
    out
}

/// True when no `Error` severity issue is present.
pub fn can_launch(issues: &[Issue]) -> bool {
    !issues.iter().any(|i| i.severity() == Severity::Error)
}

/// Build the full argument vector for hl2_launcher.
pub fn build_args(cfg: &LauncherConfig, identity: &Identity) -> Vec<String> {
    let mut args = vec![
        "-game".to_string(),
        cfg.mod_dir.clone(),
        "-allowdebug".to_string(),
    ];
    if cfg.pass_steamid {
        if let Some(id) = identity.steam_id {
            args.push("-steamid".into());
            args.push(id.to_string());
        }
    }
    if cfg.pass_name && !identity.name.trim().is_empty() {
        args.push("+name".into());
        args.push(identity.name.trim().to_string());
    }
    if !cfg.start_map.trim().is_empty() {
        args.push("+map".into());
        args.push(cfg.start_map.trim().to_string());
    }
    args.extend(split_args(&cfg.extra_args));
    args
}

/// Full command as a display string (quoted where needed).
pub fn command_line(cfg: &LauncherConfig, identity: &Identity) -> String {
    let paths = cfg.paths();
    let mut s = paths.game_exe().to_string_lossy().into_owned();
    for a in build_args(cfg, identity) {
        s.push(' ');
        if a.contains(' ') {
            s.push('"');
            s.push_str(&a);
            s.push('"');
        } else {
            s.push_str(&a);
        }
    }
    s
}

/// Start the game. Working directory is the engine root, matching how the
/// engine expects to find bin/ and the game dirs.
pub fn start_game(cfg: &LauncherConfig, identity: &Identity) -> Result<std::process::Child> {
    let paths = cfg.paths();
    let exe = paths.game_exe();
    if !exe.is_file() {
        return err(format!("game executable not found: {}", exe.display()));
    }
    let args = build_args(cfg, identity);
    std::process::Command::new(&exe)
        .args(&args)
        .current_dir(&paths.engine_root)
        .spawn()
        .map_err(|e| crate::Error(format!("start: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_args_quotes() {
        assert_eq!(split_args("-w 1280 +name \"Half Box\""), vec![
            "-w", "1280", "+name", "Half Box"
        ]);
    }

    #[test]
    fn build_args_shape() {
        let cfg = LauncherConfig {
            engine_root: "/x".into(),
            mod_dir: "hl2sb".into(),
            extra_args: "-windowed".into(),
            pass_name: true,
            pass_steamid: true,
            start_map: String::new(),
            ..Default::default()
        };
        let id = Identity {
            name: "Tester".into(),
            steam_id: Some(76561197960287930),
            avatar: None,
        };
        let args = build_args(&cfg, &id);
        assert_eq!(args[0], "-game");
        assert_eq!(args[1], "hl2sb");
        assert!(args.contains(&"-steamid".to_string()));
        assert!(args.contains(&"76561197960287930".to_string()));
        assert!(args.windows(2).any(|w| w == ["+name", "Tester"]));
        assert!(args.contains(&"-windowed".to_string()));
    }

    #[test]
    fn map_names() {
        assert!(valid_map_name("gm_construct"));
        assert!(valid_map_name("d1_trainstation_01"));
        assert!(valid_map_name("workshop/123"));
        assert!(!valid_map_name("-foo"));
        assert!(!valid_map_name("a b"));
        assert!(!valid_map_name("a;rm"));
        assert!(!valid_map_name(""));
        assert!(!valid_map_name(&"x".repeat(65)));
    }

    #[test]
    fn validate_reports_missing_engine_and_quotes() {
        let base = std::env::temp_dir().join("hl2box_validate_test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let cfg = LauncherConfig {
            engine_root: base.join("nope").to_string_lossy().into_owned(),
            extra_args: "+name \"open".into(),
            start_map: "bad map".into(),
            ..Default::default()
        };
        let id = Identity { name: "x".into(), steam_id: None, avatar: None };
        let issues = validate(&cfg, &id);
        assert!(issues.contains(&Issue::EngineRootMissing));
        assert!(issues.contains(&Issue::StartMapInvalid));
        assert!(issues.contains(&Issue::ExtraArgsUnbalancedQuotes));
        assert!(issues.contains(&Issue::NoSteamId));
        assert!(!can_launch(&issues));
        // errors sort before warnings
        assert_eq!(issues[0].severity(), Severity::Error);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn presets_upsert_sort_remove() {
        let mut cfg = LauncherConfig::default();
        upsert_preset(&mut cfg, LaunchPreset { name: "b".into(), extra_args: "-w".into(), start_map: String::new() });
        upsert_preset(&mut cfg, LaunchPreset { name: "A".into(), extra_args: "-h".into(), start_map: "gm_x".into() });
        upsert_preset(&mut cfg, LaunchPreset { name: "a".into(), extra_args: "-z".into(), start_map: String::new() });
        let names: Vec<&str> = cfg.presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a", "b"], "same name (any case) replaces the entry and takes the new spelling");
        assert_eq!(cfg.presets[0].extra_args, "-z");
        assert!(remove_preset(&mut cfg, "b"));
        assert!(!remove_preset(&mut cfg, "b"));
        assert_eq!(cfg.presets.len(), 1);
        assert!(valid_preset_name("fast 1080p"));
        assert!(!valid_preset_name("   "));
        assert!(!valid_preset_name("a\nb"));
    }

    #[test]
    fn old_config_without_presets_loads() {
        let cfg: LauncherConfig = serde_json::from_str(r#"{"engine_root":"x","mod_dir":"hl2sb","extra_args":"","start_map":""}"#).unwrap();
        assert!(cfg.presets.is_empty());
    }
}
