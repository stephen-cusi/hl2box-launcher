//! Visual language: Half-Life 2 panel steel, Valve orange accents and the GMod
//! physgun blue for live/attention states. One palette per light/dark mode;
//! everything else reads colours from [`Palette`] so the two stay in step.

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle, Visuals};

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// Outer frame: top bar, status bar, sidebar.
    pub window: Color32,
    /// Main content background.
    pub panel: Color32,
    /// Cards, inputs, buttons at rest.
    pub card: Color32,
    /// Hovered card / button.
    pub card_hi: Color32,
    pub border: Color32,
    pub text: Color32,
    pub weak: Color32,
    /// Half-Life 2 orange: selection, active nav, primary action.
    pub accent: Color32,
    /// Text drawn on top of `accent`.
    pub accent_text: Color32,
    /// GMod physgun blue: info, links, "managed" tags.
    pub blue: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    /// Background for text inputs and code panes.
    pub code_bg: Color32,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            dark,
            window: rgb(18, 20, 23),
            panel: rgb(27, 30, 35),
            card: rgb(37, 41, 47),
            card_hi: rgb(48, 53, 61),
            border: rgb(58, 64, 73),
            text: rgb(226, 229, 234),
            weak: rgb(139, 147, 158),
            accent: rgb(240, 160, 48),
            accent_text: rgb(24, 20, 12),
            blue: rgb(92, 186, 255),
            ok: rgb(124, 197, 118),
            warn: rgb(240, 192, 96),
            err: rgb(229, 104, 79),
            code_bg: rgb(19, 21, 25),
        }
    } else {
        Palette {
            dark,
            window: rgb(226, 229, 233),
            panel: rgb(242, 243, 245),
            card: rgb(255, 255, 255),
            card_hi: rgb(246, 247, 249),
            border: rgb(212, 217, 223),
            text: rgb(31, 35, 40),
            weak: rgb(106, 114, 124),
            accent: rgb(201, 125, 18),
            accent_text: rgb(255, 255, 255),
            blue: rgb(43, 127, 208),
            ok: rgb(40, 140, 60),
            warn: rgb(176, 110, 0),
            err: rgb(190, 58, 40),
            code_bg: rgb(248, 249, 250),
        }
    }
}

pub fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// `c` with its alpha replaced by `a` (0..=255).
pub fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// Linear blend of two colours.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// Push the palette into egui: visuals, rounding, fonts and spacing.
pub fn apply(ctx: &egui::Context, pal: &Palette) {
    let mut v = if pal.dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = pal.panel;
    v.window_fill = pal.card;
    v.window_stroke = Stroke::new(1.0_f32, pal.border);
    v.extreme_bg_color = pal.code_bg;
    v.faint_bg_color = if pal.dark { rgb(31, 34, 39) } else { rgb(247, 248, 250) };
    v.code_bg_color = pal.code_bg;
    v.hyperlink_color = pal.blue;
    v.warn_fg_color = pal.warn;
    v.error_fg_color = pal.err;
    v.selection.bg_fill = alpha(pal.accent, 70);
    v.selection.stroke = Stroke::new(1.0_f32, pal.accent);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);

    let r = CornerRadius::same(6);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_fill = pal.panel;
    v.widgets.noninteractive.weak_bg_fill = pal.panel;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, pal.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, pal.text);

    v.widgets.inactive.bg_fill = pal.card;
    v.widgets.inactive.weak_bg_fill = pal.card;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, pal.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, pal.text);

    v.widgets.hovered.bg_fill = pal.card_hi;
    v.widgets.hovered.weak_bg_fill = pal.card_hi;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, alpha(pal.accent, 160));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, pal.text);

    let pressed = mix(pal.card_hi, pal.accent, 0.28);
    v.widgets.active.bg_fill = pressed;
    v.widgets.active.weak_bg_fill = pressed;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, pal.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, pal.text);

    v.widgets.open.bg_fill = pal.card_hi;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, pal.accent);

    ctx.set_visuals(v);

    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.indent = 16.0;
        style.spacing.menu_margin = egui::Margin::same(6);
        // solid scrollbars reserve their own column, so content never sits under them
        style.spacing.scroll = egui::style::ScrollStyle::solid();
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(22.0)),
            (TextStyle::Body, FontId::proportional(14.5)),
            (TextStyle::Button, FontId::proportional(14.5)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
    });
}
