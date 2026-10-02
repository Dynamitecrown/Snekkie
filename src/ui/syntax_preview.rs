//! A device sample rendered with the same intensity as real terminals.

use super::style;
use super::terminal_view::{TerminalView, ViewOptions};
use crate::settings::{Animations, Theme};
use crate::terminal::{emulator::Emulator, highlight};
use egui::{Stroke, TextStyle, Ui, vec2};
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Default)]
pub struct SyntaxPreview {
    sample: Option<(String, Mutex<Emulator>)>,
    view: TerminalView,
}

impl SyntaxPreview {
    pub fn ui(&mut self, ui: &mut Ui, theme: Theme, syntax: &str, intensity: u8) {
        if self.sample.as_ref().is_none_or(|(key, _)| key != syntax) {
            let mut emulator = Emulator::new(80, 8, 100, Arc::new(|_| {}));
            emulator.feed(highlight::sample(syntax).as_bytes());
            self.sample = Some((syntax.to_string(), Mutex::new(emulator)));
        }
        let font = TextStyle::Monospace.resolve(ui.style());
        let row = ui.fonts_mut(|f| f.row_height(&font)).ceil();
        let options = ViewOptions {
            theme,
            syntax,
            highlighting_intensity: intensity,
            regular: font.clone(),
            bold: font,
            keyboard: false,
            animations: Animations::default(),
        };
        egui::Frame::new().stroke(Stroke::new(1.0, style::border(ui))).show(ui, |ui| {
            let size = vec2(ui.available_width(), 8.0 * row + 8.0);
            self.view.show_demo(ui, size, &self.sample.as_ref().unwrap().1, &options);
        });
    }
}
