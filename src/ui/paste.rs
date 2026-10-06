use crate::paste::{MAX_DELAY_MS, PasteTarget, line_count, preview_text};

pub struct PastePreview {
    pub target: PasteTarget,
    pub text: String,
    pub delay_ms: u64,
    focus: bool,
}

pub enum Action {
    Send,
    Cancel,
}

impl PastePreview {
    pub fn new(target: PasteTarget, text: &str, delay_ms: u64) -> Self {
        Self { target, text: preview_text(text), delay_ms, focus: true }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, valid: bool) -> Option<Action> {
        let top = ui.cursor().min.y;
        ui.heading("Review paste");
        ui.add(egui::Label::new(format!("Destination: {}", self.target.description)).wrap());
        if !valid {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "The reviewed connection changed or closed. Cancel and paste again on a connected tab.",
            );
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} line(s)", line_count(&self.text)));
            ui.label("Delay between lines");
            ui.add(egui::DragValue::new(&mut self.delay_ms).range(0..=MAX_DELAY_MS).suffix(" ms"));
        });
        ui.label("Review and edit before sending. Add a final newline to submit the last command.");
        let above = ui.cursor().min.y - top;
        let room = (ui.ctx().content_rect().height() - above - 64.0).clamp(36.0, 300.0);
        egui::ScrollArea::vertical().max_height(room).auto_shrink([false, true]).show(ui, |ui| {
            let label = ui.label("Paste text");
            let edit = ui.add(
                egui::TextEdit::multiline(&mut self.text)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(8),
            );
            let edit = edit.labelled_by(label.id);
            if std::mem::take(&mut self.focus) {
                edit.request_focus();
            }
        });
        let mut action = None;
        ui.horizontal(|ui| {
            if ui.add_enabled(valid && !self.text.is_empty(), egui::Button::new("Send paste")).clicked() {
                action = Some(Action::Send);
            }
            if ui.button("Cancel").clicked() {
                action = Some(Action::Cancel);
            }
        });
        action
    }
}
