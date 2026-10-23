//! System theme detection (light / dark) and CJK font loading.

use eframe::egui;
use eframe::egui::FontDefinitions;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Light,
    Dark,
}

/// "auto" | "light" | "dark" from the config file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeSetting {
    Auto,
    Light,
    Dark,
}

impl ThemeSetting {
    pub fn from_cfg(value: &str) -> ThemeSetting {
        match value {
            "light" => ThemeSetting::Light,
            "dark" => ThemeSetting::Dark,
            _ => ThemeSetting::Auto,
        }
    }

    pub fn as_cfg(self) -> &'static str {
        match self {
            ThemeSetting::Auto => "auto",
            ThemeSetting::Light => "light",
            ThemeSetting::Dark => "dark",
        }
    }

    pub fn resolve(self, system: Theme) -> Theme {
        match self {
            ThemeSetting::Auto => system,
            ThemeSetting::Light => Theme::Light,
            ThemeSetting::Dark => Theme::Dark,
        }
    }
}

/// Ask the OS whether apps should be dark. Cheap enough to poll slowly
/// (a registry read / one subprocess), so the follow-system mode tracks
/// the control-centre toggle without a window restart.
pub fn system_theme() -> Theme {
    #[cfg(windows)]
    {
        // AppsUseLightTheme: 0 = dark, 1 = light. Missing key = light.
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(value) = hkcu.open_subkey(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
        )
        .and_then(|key| key.get_value::<u32, _>("AppsUseLightTheme"))
        {
            return if value == 0 { Theme::Dark } else { Theme::Light };
        }
        return Theme::Light;
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output();
        if let Ok(o) = out {
            if String::from_utf8_lossy(&o.stdout).to_lowercase().contains("dark") {
                return Theme::Dark;
            }
        }
        return Theme::Light;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let out = std::process::Command::new("gsettings")
            .args([
                "get",
                "org.gnome.desktop.interface",
                "color-scheme",
            ])
            .output();
        if let Ok(o) = out {
            if String::from_utf8_lossy(&o.stdout).to_lowercase().contains("dark") {
                return Theme::Dark;
            }
        }
        Theme::Light
    }
}

/// Install a CJK-capable font so Chinese labels actually render. egui's
/// default font has no CJK glyphs; rather than shipping megabytes of
/// font data, load the platform's own UI font at startup.
pub fn install_cjk_fonts(ctx: &egui::Context) {
    for candidate in cjk_font_candidates() {
        if let Ok(bytes) = std::fs::read(&candidate) {
            let mut fonts = FontDefinitions::default();
            fonts.font_data.insert(
                "cjk".into(),
                egui::FontData::from_owned(bytes).into(),
            );
            // Put the CJK face into every family, after the default so
            // latin text keeps the built-in look.
            for family in [
                egui::FontFamily::Proportional,
                egui::FontFamily::Monospace,
            ] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(1, "cjk".into());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
}

fn cjk_font_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(windir) = std::env::var("WINDIR") {
            let fonts = std::path::Path::new(&windir).join("Fonts");
            for name in [
                "msyh.ttc",
                "msyh.ttf",
                "Microsoft YaHei UI.ttf",
                "simhei.ttf",
                "simsun.ttc",
            ] {
                out.push(fonts.join(name));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        out.push(std::path::Path::new(
            "/System/Library/Fonts/PingFang.ttc",
        ));
        out.push(std::path::Path::new(
            "/System/Library/Fonts/STHeiti Light.ttc",
        ));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for base in ["/usr/share/fonts", "/usr/local/share/fonts"] {
            let root = std::path::Path::new(base);
            for rel in [
                "opentype/noto/NotoSansCJK-Regular.ttc",
                "noto-cjk/NotoSansCJK-Regular.ttc",
                "wqy-microhei/wqy-microhei.ttc",
                "wqy-zenhei/wqy-zenhei.ttc",
            ] {
                out.push(root.join(rel));
            }
        }
    }
    out
}
