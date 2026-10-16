//! A static theme sample drawn by the same renderer as a real session.

use std::sync::Arc;

use alacritty_terminal::index::{Column, Line, Point, Side};
use egui::{FontId, Stroke, Ui, vec2};
use parking_lot::Mutex;

use super::style;
use super::terminal_view::{TerminalView, ViewOptions};
use crate::settings::{Animations, Theme};
use crate::terminal::emulator::Emulator;

pub struct ThemePreview {
    emulator: Mutex<Emulator>,
    view: TerminalView,
}

impl Default for ThemePreview {
    fn default() -> Self {
        let mut emulator = Emulator::new(80, 6, 0, Arc::new(|_| {}));
        emulator.feed(b"Switch# show ip interface brief\r\nInterface       IP-Address     Status\r\nGi0/1           192.0.2.1       up\r\nGi0/2           unassigned      down\r\n\x1b[31m% Access denied\x1b[0m\r\nSwitch# selected");
        Self { emulator: Mutex::new(emulator), view: TerminalView::default() }
    }
}

impl ThemePreview {
    pub fn ui(&mut self, ui: &mut Ui, theme: Theme, regular: FontId, bold: FontId) {
        let row = ui.fonts_mut(|f| f.row_height(&regular)).ceil();
        let options = ViewOptions {
            theme,
            syntax: "cisco_ios",
            highlighting_intensity: crate::terminal::highlight::DEFAULT_INTENSITY,
            regular,
            bold,
            keyboard: false,
            mouse_input: false,
            right_click_paste: true,
            animations: Animations::default(),
        };
        egui::Frame::new().stroke(Stroke::new(1.0, style::border(ui))).show(ui, |ui| {
            let size = vec2(ui.available_width(), (6.0 * row + 8.0).clamp(96.0, 240.0));
            self.view.show_demo(ui, size, &self.emulator, &options);
            // Fitting the sample to a new width can clear its selection.
            // Restore it for the next pass so the selection color is visible.
            let mut emulator = self.emulator.lock();
            if !emulator.has_selection() {
                let line = Line(emulator.lines().saturating_sub(1) as i32);
                let end = emulator.columns().min(16).saturating_sub(1);
                emulator.start_selection(Point::new(line, Column(end.saturating_sub(7))), Side::Left, false);
                emulator.update_selection(Point::new(line, Column(end)), Side::Right);
                if emulator.has_selection() {
                    ui.ctx().request_repaint();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AppSettings;

    #[test]
    fn preview_grid_follows_the_requested_font_size_and_survives_narrow_large_text() {
        let ctx = egui::Context::default();
        let mut preview = ThemePreview::default();
        let mut draw = |width: f32, size: f32| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 400.0))),
                    ..Default::default()
                },
                |ui| preview.ui(ui, AppSettings::default().colors(), FontId::monospace(size), FontId::monospace(size)),
            );
            output.textures_delta.clear(); // No GPU consumes this unit test's atlas.
            let emu = preview.emulator.lock();
            assert!(emu.has_selection(), "selection sample disappeared");
            emu.columns()
        };
        let small = draw(500.0, 14.0);
        let large = draw(500.0, 28.0);
        assert!(small > large * 3 / 2, "changing the font did not resize the preview grid: {small}, {large}");
        assert!(draw(220.0, 64.0) < large);
    }
}
