//! A static theme sample drawn by the same renderer as a real session.

use std::sync::Arc;

use alacritty_terminal::index::{Column, Line, Point, Side};
use egui::{Stroke, TextStyle, Ui, vec2};
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
        let mut emulator = Emulator::new(40, 2, 0, Arc::new(|_| {}));
        emulator.feed(b"Switch# \x1b[31mshow run\x1b[0m  selected\r\ninterface Vlan1  10.0.0.1\x1b[1;17H");
        emulator.start_selection(Point::new(Line(0), Column(18)), Side::Left, false);
        emulator.update_selection(Point::new(Line(0), Column(25)), Side::Right);
        Self { emulator: Mutex::new(emulator), view: TerminalView::default() }
    }
}

impl ThemePreview {
    pub fn ui(&mut self, ui: &mut Ui, theme: Theme) {
        let font = TextStyle::Monospace.resolve(ui.style());
        let row = ui.fonts_mut(|f| f.row_height(&font)).ceil();
        let options = ViewOptions {
            theme,
            syntax: "cisco_ios",
            highlighting_intensity: crate::terminal::highlight::DEFAULT_INTENSITY,
            regular: font.clone(),
            bold: font,
            keyboard: false,
            mouse_input: false,
            right_click_paste: true,
            animations: Animations::default(),
        };
        egui::Frame::new().stroke(Stroke::new(1.0, style::border(ui))).show(ui, |ui| {
            let size = vec2(ui.available_width().min(340.0), 2.0 * row + 8.0);
            self.view.show_demo(ui, size, &self.emulator, &options);
            // Fitting the sample to a new width can clear its selection.
            // Restore it for the next pass so the selection color is visible.
            let mut emulator = self.emulator.lock();
            if !emulator.has_selection() {
                emulator.start_selection(Point::new(Line(0), Column(18)), Side::Left, false);
                emulator.update_selection(Point::new(Line(0), Column(25)), Side::Right);
                ui.ctx().request_repaint();
            }
        });
    }
}
