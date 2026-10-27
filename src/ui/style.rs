//! Look of the app chrome: sidebar, menus, tabs, buttons, dialogs.
//!
//! Standard chrome follows the terminal palette; special appearances retain
//! their monitor treatments.

use crate::settings::{AppAppearance, Theme};
use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const BANNER_BG: Color32 = Color32::from_rgb(0x55, 0x22, 0x22);
pub const BANNER_INFO_BG: Color32 = Color32::from_rgb(0x2a, 0x3a, 0x4a);

pub fn lighter(c: Color32, factor: f32) -> Color32 {
    let f = |v: u8| ((v as f32 * factor).round().min(255.0)) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

/// Black or near-white, whichever reads better on the given color.
pub fn text_on(c: Color32) -> Color32 {
    let dark = Color32::from_rgb(0x10, 0x11, 0x10);
    let light = Color32::from_rgb(0xf5, 0xf5, 0xf3);
    if contrast(c, dark) >= contrast(c, light) { dark } else { light }
}

fn luminance(c: Color32) -> f32 {
    let linear = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
}

fn contrast(a: Color32, b: Color32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

pub fn apply_theme(ctx: &egui::Context, theme: Theme) {
    let light = luminance(theme.bg) > luminance(theme.fg);
    let mut visuals = if light { Visuals::light() } else { Visuals::dark() };
    let panel = theme.bg.lerp_to_gamma(theme.fg, 0.025);
    let surface = theme.bg.lerp_to_gamma(theme.fg, 0.045);
    let input = if light { theme.bg.lerp_to_gamma(Color32::WHITE, 0.6) } else { surface };
    let hover = theme.bg.lerp_to_gamma(theme.fg, 0.10);
    let selected = theme.bg.lerp_to_gamma(theme.fg, 0.14);
    let ink = if contrast(theme.fg, selected) >= 4.5 { theme.fg } else { text_on(theme.bg) };
    let muted = ink.lerp_to_gamma(theme.bg, if light { 0.16 } else { 0.22 });
    let muted = if contrast(muted, input).min(contrast(muted, surface)) >= 4.5 { muted } else { ink };
    let border = theme.bg.lerp_to_gamma(ink, 0.18);
    let focus = if contrast(theme.cursor, input) >= 3.0 { theme.cursor } else { ink };
    visuals.panel_fill = panel;
    visuals.window_fill = surface;
    visuals.window_stroke = Stroke::new(1.0, border);
    visuals.extreme_bg_color = input;
    visuals.faint_bg_color = theme.bg;
    visuals.code_bg_color = input;
    visuals.selection.bg_fill = selected;
    visuals.selection.stroke = Stroke::new(1.0, ink);
    visuals.hyperlink_color = focus;
    visuals.weak_text_color = Some(muted);
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(4);

    let radius = CornerRadius::same(4);
    let w = &mut visuals.widgets;
    w.noninteractive.bg_fill = surface;
    w.noninteractive.weak_bg_fill = surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, ink);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = input;
    w.inactive.weak_bg_fill = input;
    w.inactive.bg_stroke = Stroke::new(1.0, border);
    w.inactive.fg_stroke = Stroke::new(1.0, ink);
    w.inactive.corner_radius = radius;

    w.hovered.bg_fill = hover;
    w.hovered.weak_bg_fill = hover;
    w.hovered.bg_stroke = Stroke::new(1.0, focus);
    w.hovered.fg_stroke = Stroke::new(1.0, ink);
    w.hovered.corner_radius = radius;

    w.active.bg_fill = selected;
    w.active.weak_bg_fill = selected;
    w.active.bg_stroke = Stroke::new(1.0, focus);
    w.active.fg_stroke = Stroke::new(1.0, ink);
    w.active.corner_radius = radius;

    w.open.bg_fill = hover;
    w.open.weak_bg_fill = hover;
    w.open.bg_stroke = Stroke::new(1.0, focus);
    w.open.fg_stroke = Stroke::new(1.0, ink);
    w.open.corner_radius = radius;

    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.combo_width = 160.0;
        style.text_styles = egui::Style::default().text_styles;
        for text in [egui::TextStyle::Body, egui::TextStyle::Button] {
            style.text_styles.insert(text, egui::FontId::proportional(14.0));
        }
        style.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
    });
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
    let label_width =
        ui.fonts_mut(|f| f.layout_no_wrap(label.into(), egui::FontId::monospace(10.0), theme.fg).size().x);
    let label = if label_width <= rect.width() - 96.0 {
        label
    } else {
        match appearance {
            AppAppearance::EInk => "E-INK",
            AppAppearance::Blueprint => "BLUEPRINT",
            AppAppearance::Amber => "AMBER",
            _ => "CRT",
        }
    };
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

/// A single high-contrast action; colour stays on data and selection lines.
pub fn primary_button(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    let fill = primary(ui);
    egui::Button::new(egui::RichText::new(text.to_string()).color(text_on(fill)).strong())
        .fill(fill)
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
    // Reserve the fill before the button paints its label.
    let background = ui.painter().add(egui::Shape::Noop);
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
        ui.painter().hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, accent));
    } else if enabled && response.hovered() {
        ui.painter()
            .set(background, egui::Shape::rect_filled(response.rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill));
    }
    response
}

pub fn wordmark(ui: &mut egui::Ui, accent: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    let points = [(15.0, 3.0), (4.0, 3.0), (4.0, 9.0), (14.0, 9.0), (14.0, 15.0), (2.0, 15.0)];
    ui.painter().add(egui::Shape::line(
        points.into_iter().map(|(x, y)| rect.min + egui::vec2(x, y)).collect(),
        Stroke::new(1.5, accent),
    ));
    ui.label(egui::RichText::new("Snekkie").size(16.0).strong());
    ui.add_space(8.0);
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
        for (_, colors) in crate::settings::THEMES {
            let accent = crate::settings::parse_hex(colors[2]).unwrap();
            assert!(contrast(accent, text_on(accent)) >= 4.5, "{accent:?}");
        }
    }

    #[test]
    fn standard_chrome_follows_each_palette_and_keeps_controls_readable() {
        let ctx = egui::Context::default();
        for (name, _) in crate::settings::THEMES {
            let theme = crate::settings::AppSettings { theme: name.into(), ..Default::default() }.colors();
            apply_theme(&ctx, theme);
            if theme.effects.is_super() {
                continue;
            }
            let style = ctx.global_style();
            let v = &style.visuals;
            assert_eq!(v.dark_mode, luminance(theme.bg) < luminance(theme.fg), "{name}");
            for bg in [
                v.panel_fill,
                v.window_fill,
                v.extreme_bg_color,
                v.selection.bg_fill,
                v.widgets.hovered.bg_fill,
                v.widgets.active.bg_fill,
            ] {
                assert!(contrast(v.text_color(), bg) >= 4.5, "{name}: primary on {bg:?}");
            }
            for bg in [v.panel_fill, v.window_fill, v.extreme_bg_color] {
                assert!(contrast(v.weak_text_color(), bg) >= 4.5, "{name}: secondary on {bg:?}");
            }
            assert!(contrast(v.selection.stroke.color, v.selection.bg_fill) >= 4.5, "{name}: selected");
            assert!(contrast(v.hyperlink_color, v.extreme_bg_color) >= 3.0, "{name}: focus");
        }
        apply_theme(&ctx, Theme::default());
        assert!(ctx.global_style().visuals.dark_mode);
        assert!(ctx.global_style().visuals.override_text_color.is_none());
    }

    #[test]
    fn narrow_monitor_labels_fit_between_the_bezel_controls() {
        for name in ["CRT Super", "E-Ink Super", "Blueprint Super", "Amber Super"] {
            let ctx = egui::Context::default();
            let theme = crate::settings::AppSettings { theme: name.into(), ..Default::default() }.colors();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                monitor(ui, theme, |ui| ui.allocate_exact_size(egui::vec2(100.0, 80.0), egui::Sense::hover()));
            });
            output.textures_delta.clear();
            let label = output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(text) => Some(text),
                    _ => None,
                })
                .unwrap();
            assert!(label.galley.size().x <= 64.0, "{name}: {:?}", label.galley.text());
        }
    }
}
