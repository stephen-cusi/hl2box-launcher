//! hl2box companion launcher.
//!
//! A small, self-contained GUI around hl2box-core: player identity (name,
//! steam id, avatar), the addon manager (disable list, .gma inspection, folder
//! and archive browser, Lua listing, text viewer), workshop downloads, game
//! mounts and log viewing. It never replaces hl2_launcher - the Launch button
//! just starts the game with the command line shown on the front page.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod explorer;
mod i18n;
mod logview;
mod platform;
mod state;
mod textview;
mod theme;
mod widgets;

use eframe::egui;
use state::App;

const ICON_PNG: &[u8] = include_bytes!("../assets/logo-512.png");

fn main() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(ICON_PNG).ok().map(std::sync::Arc::new);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1120.0, 760.0])
        .with_min_inner_size([880.0, 600.0])
        .with_title("hl2box launcher");
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "hl2box launcher",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
