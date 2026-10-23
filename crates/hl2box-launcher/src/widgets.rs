//! Small reusable pieces: cards, nav items, badges, key/value grids, buttons.

use eframe::egui::{self, Color32, CursorIcon, FontId, Margin, Rect, RichText, Sense, Stroke};

use crate::theme::{alpha, Palette};

/// A rounded panel with a hairline border. Everything grouped on a page sits in
/// one of these so the layout reads as blocks instead of a list of widgets.
pub fn card<R>(ui: &mut egui::Ui, pal: &Palette, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .corner_radius(8.0)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            // cards fill the column so inputs line up across the page
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Section label with a short accent bar, used as the first line of a card.
pub fn section(ui: &mut egui::Ui, pal: &Palette, title: &str) {
    ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(egui::vec2(3.0, 14.0), Sense::hover());
        ui.painter().rect_filled(bar, 1.5, pal.accent);
        ui.label(RichText::new(title).strong().size(15.0));
    });
    ui.add_space(4.0);
}

/// Page title plus an optional one-line subtitle.
pub fn page_header(ui: &mut egui::Ui, pal: &Palette, title: &str, subtitle: Option<&str>) {
    ui.label(RichText::new(title).heading().strong());
    if let Some(sub) = subtitle {
        ui.label(RichText::new(sub).color(pal.weak));
    }
    ui.add_space(8.0);
}

/// Sidebar navigation row: a flat painted item with an accent bar when active.
pub fn nav_item(ui: &mut egui::Ui, pal: &Palette, label: &str, selected: bool) -> egui::Response {
    let h = 36.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 6.0, alpha(pal.accent, 38));
        let bar = Rect::from_min_size(rect.left_top(), egui::vec2(3.0, h));
        painter.rect_filled(bar, 1.5, pal.accent);
    } else if resp.hovered() {
        painter.rect_filled(rect, 6.0, pal.card_hi);
    }
    let color = if selected {
        pal.accent
    } else if resp.hovered() {
        pal.text
    } else {
        pal.weak
    };
    painter.text(
        rect.left_center() + egui::vec2(14.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(15.0),
        color,
    );
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Small rounded tag, e.g. "gma", "managed", "[on]".
pub fn badge(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) {
    egui::Frame::new()
        .fill(alpha(color, 46))
        .corner_radius(10.0)
        .inner_margin(Margin::symmetric(7, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().color(color));
        });
}

/// Two-column key/value grid. Values wrap; keys are dimmed.
pub fn kv_grid(ui: &mut egui::Ui, pal: &Palette, id: &str, rows: &[(String, String, bool)]) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([18.0, 7.0])
        .min_col_width(110.0)
        .show(ui, |ui| {
            for (k, v, mono) in rows {
                ui.label(RichText::new(k).color(pal.weak));
                let text = if *mono {
                    RichText::new(v).monospace()
                } else {
                    RichText::new(v)
                };
                ui.add(egui::Label::new(text).wrap());
                ui.end_row();
            }
        });
}

/// Primary call-to-action button: solid accent fill.
pub fn primary(pal: &Palette, text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).color(pal.accent_text).strong())
        .fill(pal.accent)
        .min_size(egui::vec2(0.0, 30.0))
}

/// A full-width clickable row with a left label, an optional right-aligned
/// meta text and a tag colour. Used by the explorer and the Lua listing.
pub fn list_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    name: &str,
    meta: &str,
    tag: Option<(&str, Color32)>,
    selected: bool,
) -> egui::Response {
    let h = 24.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 4.0, alpha(pal.accent, 50));
    } else if resp.hovered() {
        painter.rect_filled(rect, 4.0, pal.card_hi);
    }
    let mut left = rect.left() + 8.0;
    if let Some((text, color)) = tag {
        let galley = ui.fonts(|f| f.layout_no_wrap(text.to_string(), FontId::proportional(11.0), color));
        let w = galley.size().x + 8.0;
        let tag_rect = Rect::from_min_size(
            egui::pos2(left, rect.center().y - 8.0),
            egui::vec2(w, 16.0),
        );
        painter.rect_filled(tag_rect, 4.0, alpha(color, 46));
        painter.galley(
            egui::pos2(tag_rect.left() + 4.0, tag_rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        left = tag_rect.right() + 8.0;
    }
    painter.text(
        egui::pos2(left, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        FontId::proportional(14.0),
        pal.text,
    );
    if !meta.is_empty() {
        painter.text(
            egui::pos2(rect.right() - 8.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            meta,
            FontId::proportional(12.0),
            pal.weak,
        );
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Colour dot followed by text, for status lines.
pub fn status_text(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 4.0, color);
        ui.label(RichText::new(text).color(color));
    });
}
