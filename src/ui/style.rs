//! Look of the app chrome: sidebar, menus, tabs, buttons, dialogs.
//!
//! Most themes use dark chrome with the cursor color as an accent.
//! CRT Super also gives the controls a retro appearance.

use crate::settings::{AppAppearance, Theme};
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

/// Black or near-white, whichever reads better on the given color.
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
    visuals.weak_text_color = Some(TEXT_SECONDARY);
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
        style.text_styles = egui::Style::default().text_styles;
    });
}

pub fn apply_theme(ctx: &egui::Context, theme: Theme) {
    apply(ctx, theme.cursor);
    let appearance = theme.effects.appearance();
    if appearance == AppAppearance::Standard {
        return;
    }
    ctx.global_style_mut(|style| {
        if appearance != AppAppearance::EInk {
            for font in style.text_styles.values_mut() {
                font.family = egui::FontFamily::Monospace;
            }
        }
        let visuals = &mut style.visuals;
        visuals.panel_fill = theme.bg;
        visuals.dark_mode = appearance != AppAppearance::EInk;
        let (panel, input, faint) = match appearance {
            AppAppearance::EInk => {
                (Color32::from_rgb(220, 222, 211), Color32::from_rgb(245, 245, 235), Color32::from_rgb(228, 230, 218))
            }
            AppAppearance::Blueprint => {
                (Color32::from_rgb(18, 49, 79), Color32::from_rgb(10, 33, 58), Color32::from_rgb(21, 56, 87))
            }
            AppAppearance::Amber => {
                (Color32::from_rgb(39, 28, 17), Color32::from_rgb(20, 14, 8), Color32::from_rgb(34, 24, 13))
            }
            _ => (Color32::from_rgb(10, 24, 15), Color32::from_rgb(5, 15, 8), Color32::from_rgb(9, 21, 13)),
        };
        visuals.window_fill = panel;
        visuals.extreme_bg_color = input;
        visuals.code_bg_color = input;
        visuals.faint_bg_color = faint;
        visuals.override_text_color = Some(theme.fg);
        visuals.weak_text_color = Some(if appearance == AppAppearance::EInk {
            Color32::from_rgb(83, 89, 77)
        } else {
            lighter(theme.fg, 0.72)
        });
        let radius = if appearance == AppAppearance::EInk { CornerRadius::same(3) } else { CornerRadius::ZERO };
        visuals.window_corner_radius = radius;
        visuals.menu_corner_radius = radius;
        visuals.window_stroke = Stroke::new(1.0, lighter(theme.fg, 0.4));
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.bg_fill = input;
            widget.weak_bg_fill = input;
            widget.bg_stroke = Stroke::new(1.0, lighter(theme.fg, 0.4));
            widget.fg_stroke = Stroke::new(1.0, theme.fg);
            widget.corner_radius = radius;
        }
        visuals.widgets.hovered.bg_fill = theme.selection;
        visuals.widgets.hovered.weak_bg_fill = theme.selection;
        visuals.widgets.active.bg_fill = theme.selection;
        visuals.widgets.open.bg_fill = theme.selection;
    });
}

pub fn border(ui: &egui::Ui) -> Color32 {
    ui.visuals().widgets.noninteractive.bg_stroke.color
}
pub fn primary(ui: &egui::Ui) -> Color32 {
    ui.visuals().text_color()
}
pub fn secondary(ui: &egui::Ui) -> Color32 {
    ui.visuals().weak_text_color()
}

/// The terminal becomes the recessed glass screen of an old monitor.
pub fn monitor<R>(ui: &mut egui::Ui, theme: Theme, draw: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let appearance = theme.effects.appearance();
    let (case, outline, inset, label) = match appearance {
        AppAppearance::EInk => (
            Color32::from_rgb(210, 213, 199),
            Color32::from_rgb(112, 119, 100),
            Color32::from_rgb(88, 95, 79),
            "S N E K K I E  /  E - I N K",
        ),
        AppAppearance::Blueprint => (
            Color32::from_rgb(15, 43, 70),
            theme.cursor.gamma_multiply(0.65),
            Color32::from_rgb(48, 96, 126),
            "S N E K K I E  /  B L U E P R I N T",
        ),
        AppAppearance::Amber => (
            Color32::from_rgb(67, 48, 28),
            Color32::from_rgb(143, 113, 64),
            Color32::from_rgb(12, 8, 3),
            "S N E K K I E  /  A M B E R",
        ),
        _ => (
            Color32::from_rgb(35, 45, 38),
            Color32::from_rgb(74, 88, 77),
            Color32::from_rgb(2, 7, 4),
            "S N E K K I E  /  C R T",
        ),
    };
    let is_paper = appearance == AppAppearance::EInk;
    let outer = egui::Frame::new()
        .fill(case)
        .stroke(Stroke::new(2.0, outline))
        .corner_radius(if appearance == AppAppearance::Blueprint {
            0.0
        } else if is_paper {
            22.0
        } else {
            14.0
        })
        .inner_margin(egui::Margin { left: 16, right: 16, top: 16, bottom: 34 })
        .show(ui, |ui| {
            egui::Frame::new()
                .fill(theme.bg)
                .stroke(Stroke::new(if is_paper { 1.0 } else { 3.0 }, inset))
                .corner_radius(if appearance == AppAppearance::Blueprint { 0.0 } else { 8.0 })
                .inner_margin(4.0)
                .show(ui, draw)
                .inner
        });
    let rect = outer.response.rect;
    let painter = ui.painter();
    let y = rect.bottom() - 15.0;
    painter.text(
        egui::pos2(rect.center().x, y),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::monospace(10.0),
        if is_paper { theme.fg } else { theme.cursor },
    );
    if appearance == AppAppearance::Blueprint {
        for x in (0..(rect.width() as usize / 12)).map(|i| rect.left() + i as f32 * 12.0) {
            painter
                .line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.top() + 12.0)], Stroke::new(0.5, outline));
        }
        for corner in [rect.left_top(), rect.right_top(), rect.left_bottom(), rect.right_bottom()] {
            painter.circle_stroke(
                corner + (rect.center() - corner).normalized() * 7.0,
                2.0,
                Stroke::new(1.0, theme.cursor),
            );
        }
    } else if is_paper {
        // A printed status mark, with no luminous power indicator.
        painter.rect_stroke(
            egui::Rect::from_center_size(egui::pos2(rect.right() - 28.0, y), egui::vec2(18.0, 7.0)),
            1.0,
            Stroke::new(1.0, theme.fg),
            egui::StrokeKind::Inside,
        );
    } else {
        painter.circle_filled(egui::pos2(rect.right() - 23.0, y), 3.0, theme.cursor);
    }
    for i in 0..5 {
        let x = rect.left() + 20.0 + i as f32 * 5.0;
        painter.line_segment([egui::pos2(x, y - 3.0), egui::pos2(x, y + 3.0)], Stroke::new(2.0, inset));
    }
    outer.inner
}

/// The accent-filled Connect button.
pub fn accent_button(text: &str, accent: Color32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).color(text_on(accent)).strong())
        .fill(accent)
        .stroke(Stroke::NONE)
        .min_size(egui::vec2(0.0, 30.0))
}

/// A small, rounded accent-filled button that fits in the menu bar.
pub fn accent_pill(text: &str, accent: Color32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).color(text_on(accent)).strong())
        .fill(accent)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(10))
}

/// A tab-strip button: plain text, underlined in the accent color when
/// selected, greyed out when disabled.
pub fn tab_button(ui: &mut egui::Ui, text: &str, selected: bool, enabled: bool, accent: Color32) -> egui::Response {
    let color = if !enabled {
        lighter(secondary(ui), 0.6)
    } else if selected {
        primary(ui)
    } else {
        secondary(ui)
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
    ui.label(egui::RichText::new(text).color(secondary(ui)).size(12.0).strong());
    heading_rule(ui);
}

/// A section heading with a control at the right end of its row.
pub fn section_heading_with<R>(ui: &mut egui::Ui, text: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let inner = ui
        .horizontal(|ui| {
            ui.label(egui::RichText::new(text).color(secondary(ui)).size(12.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add).inner
        })
        .inner;
    heading_rule(ui);
    inner
}

fn heading_rule(ui: &mut egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(rect.x_range(), rect.top() - 2.0, Stroke::new(1.0, border(ui)));
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
