//! Look of the app chrome: sidebar, menus, tabs, buttons, dialogs.
//!
//! Separate from the terminal colour theme, which only paints the terminal.
//! The frame is always dark, as most terminal emulators keep their own
//! chrome rather than following the OS. The accent colour comes from the
//! active terminal theme's cursor colour, so switching themes in
//! Preferences re-skins the whole app.

use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const BG_WINDOW: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x1b);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x20, 0x21, 0x20);
pub const BG_INPUT: Color32 = Color32::from_rgb(0x2a, 0x2b, 0x2a);
pub const BG_LIST: Color32 = Color32::from_rgb(0x1e, 0x1f, 0x1e);
pub const BORDER: Color32 = Color32::from_rgb(0x34, 0x34, 0x32);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xe6);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x9a, 0x9a, 0x96);
pub const BANNER_BG: Color32 = Color32::from_rgb(0x55, 0x22, 0x22);
pub const BANNER_INFO_BG: Color32 = Color32::from_rgb(0x2a, 0x3a, 0x4a);

pub fn lighter(c: Color32, factor: f32) -> Color32 {
    let f = |v: u8| ((v as f32 * factor).round().min(255.0)) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

/// Black or near-white, whichever reads better on the given colour.
pub fn text_on(c: Color32) -> Color32 {
    let luminance = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    if luminance > 140.0 { Color32::from_rgb(0x10, 0x11, 0x10) } else { Color32::from_rgb(0xf5, 0xf5, 0xf3) }
}

pub fn apply(ctx: &egui::Context, accent: Color32) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = BG_WINDOW;
    visuals.window_fill = BG_PANEL;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.extreme_bg_color = BG_INPUT;
    visuals.faint_bg_color = BG_LIST;
    visuals.code_bg_color = BG_INPUT;
    visuals.selection.bg_fill = accent;
    visuals.selection.stroke = Stroke::new(1.0, text_on(accent));
    visuals.hyperlink_color = accent;
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(4);

    let radius = CornerRadius::same(4);
    let w = &mut visuals.widgets;
    w.noninteractive.bg_fill = BG_PANEL;
    w.noninteractive.weak_bg_fill = BG_PANEL;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = BG_INPUT;
    w.inactive.weak_bg_fill = BG_INPUT;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.inactive.corner_radius = radius;

    w.hovered.bg_fill = lighter(BG_INPUT, 1.3);
    w.hovered.weak_bg_fill = lighter(BG_INPUT, 1.3);
    w.hovered.bg_stroke = Stroke::new(1.0, lighter(BORDER, 1.4));
    w.hovered.fg_stroke = Stroke::new(1.5, TEXT_PRIMARY);
    w.hovered.corner_radius = radius;

    w.active.bg_fill = BG_WINDOW;
    w.active.weak_bg_fill = BG_WINDOW;
    w.active.bg_stroke = Stroke::new(1.0, accent);
    w.active.fg_stroke = Stroke::new(1.5, TEXT_PRIMARY);
    w.active.corner_radius = radius;

    w.open.bg_fill = BG_INPUT;
    w.open.weak_bg_fill = BG_INPUT;
    w.open.bg_stroke = Stroke::new(1.0, accent);
    w.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    w.open.corner_radius = radius;

    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.combo_width = 160.0;
    });
}

/// The accent-filled Connect button.
pub fn accent_button(text: &str, accent: Color32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).color(text_on(accent)).strong())
        .fill(accent)
        .stroke(Stroke::NONE)
        .min_size(egui::vec2(0.0, 30.0))
}

/// A tab-strip button: plain text, underlined in the accent colour when
/// selected, greyed out when disabled.
pub fn tab_button(ui: &mut egui::Ui, text: &str, selected: bool, enabled: bool, accent: Color32) -> egui::Response {
    let color = if !enabled {
        lighter(TEXT_SECONDARY, 0.6)
    } else if selected {
        TEXT_PRIMARY
    } else {
        TEXT_SECONDARY
    };
    let label = egui::RichText::new(text).color(color);
    let response = ui.add_enabled(enabled, egui::Button::new(label).frame(false).min_size(egui::vec2(0.0, 26.0)));
    if selected {
        let rect = response.rect;
        ui.painter().hline(rect.x_range(), rect.bottom(), Stroke::new(2.0, accent));
    } else if enabled && response.hovered() {
        ui.painter().rect_filled(response.rect, 3.0, Color32::from_white_alpha(8));
    }
    response
}

/// A small grey section heading with a rule under it.
pub fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).color(TEXT_SECONDARY).size(12.0).strong());
    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(rect.x_range(), rect.top() - 2.0, Stroke::new(1.0, BORDER));
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_text() {
        assert_eq!(text_on(Color32::from_rgb(0x3a, 0xd9, 0x00)), Color32::from_rgb(0x10, 0x11, 0x10));
        assert_eq!(text_on(Color32::from_rgb(0x26, 0x8b, 0xd2)), Color32::from_rgb(0xf5, 0xf5, 0xf3));
    }
}
