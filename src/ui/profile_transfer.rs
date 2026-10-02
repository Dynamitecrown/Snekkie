use crate::profiles::{Profile, ProfileImport, merge_import};

/// A selection preview only. File selection, writes and store updates are
/// performed by the app after the user chooses the final action.
pub struct ProfileTransfer {
    export: bool,
    profiles: Vec<Profile>,
    selected: Vec<bool>,
    warnings: Vec<String>,
}

impl ProfileTransfer {
    pub fn export(profiles: &[Profile], selected: Option<&str>) -> Self {
        let selected = selected.filter(|name| profiles.iter().any(|profile| profile.name == *name));
        Self {
            export: true,
            profiles: profiles.to_vec(),
            selected: profiles.iter().map(|profile| selected.is_none_or(|name| profile.name == name)).collect(),
            warnings: Vec::new(),
        }
    }

    pub fn import(batch: ProfileImport, existing: &[Profile]) -> Self {
        let merged = merge_import(existing, batch.profiles.iter().cloned());
        let profiles = merged.into_iter().skip(existing.len()).collect::<Vec<_>>();
        let mut warnings = batch.warnings;
        for (original, renamed) in batch.profiles.iter().zip(&profiles) {
            if original.name != renamed.name {
                warnings.push(format!(
                    "{} will be added as {} because that name is already in use.",
                    original.name, renamed.name
                ));
            }
        }
        Self { export: false, selected: vec![true; profiles.len()], profiles, warnings }
    }

    pub fn is_export(&self) -> bool {
        self.export
    }

    pub fn profiles(&self) -> Vec<Profile> {
        self.profiles
            .iter()
            .zip(&self.selected)
            .filter(|(_, selected)| **selected)
            .map(|(profile, _)| profile.clone())
            .collect()
    }

    /// Some(true) applies the selected action; Some(false) cancels the preview.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<bool> {
        let count = self.selected.iter().filter(|selected| **selected).count();
        ui.heading(if self.export { "Export saved sessions" } else { "Import saved sessions" });
        let room = (ui.ctx().content_rect().height() - 185.0).clamp(60.0, 400.0);
        egui::ScrollArea::vertical().id_salt("profile_transfer_body").max_height(room).auto_shrink([false, true]).show(ui, |ui| {
            if self.export {
                ui.label("Choose the saved sessions to include in the JSON export. Private-key and log file paths are included as references. Passwords, key contents and host-key trust are excluded.");
            } else {
                ui.label("Review the names and destinations before adding sessions. Existing saved sessions are preserved. Importing does not connect to a device.");
            }
            ui.label(format!("{count} of {} session(s) selected", self.profiles.len()));
            ui.horizontal_wrapped(|ui| {
                if ui.button("Select all").clicked() {
                    self.selected.fill(true);
                }
                if ui.button("Select none").clicked() {
                    self.selected.fill(false);
                }
            });
            ui.separator();
            for (profile, selected) in self.profiles.iter().zip(&mut self.selected) {
                ui.checkbox(selected, &profile.name);
                let destination = if profile.kind().is_network() {
                    format!("{}:{}", profile.host, profile.port)
                } else {
                    format!("{} / {} baud", profile.device, profile.baud)
                };
                ui.label(egui::RichText::new(format!("{} — {destination}", profile.kind().label())).small());
                ui.add_space(4.0);
            }
            if !self.warnings.is_empty() {
                egui::CollapsingHeader::new(format!("{} import note(s)", self.warnings.len())).default_open(true).show(ui, |ui| {
                    for warning in &self.warnings {
                        ui.label(egui::RichText::new(warning).small());
                    }
                });
            }
        });
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let action = if self.export { "Export selected…" } else { "Import selected" };
            if ui.add_enabled(count > 0, egui::Button::new(action)).clicked() {
                return Some(true);
            }
            if ui.button("Cancel").clicked() {
                return Some(false);
            }
            None
        })
        .inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str) -> Profile {
        Profile { name: name.into(), host: "192.0.2.1".into(), ..Profile::default() }
    }

    #[test]
    fn export_preselects_the_current_saved_session_or_all() {
        let profiles = [profile("Alpha"), profile("Beta")];
        let transfer = ProfileTransfer::export(&profiles, Some("Beta"));
        assert!(transfer.is_export());
        assert_eq!(transfer.profiles(), [profiles[1].clone()]);
        assert_eq!(ProfileTransfer::export(&profiles, None).profiles(), profiles);
        assert_eq!(ProfileTransfer::export(&profiles, Some("Missing")).profiles(), profiles);
    }

    #[test]
    fn import_preview_and_selection_keep_final_collision_safe_names() {
        let batch = ProfileImport {
            profiles: vec![profile("alpha"), profile("Alpha")],
            warnings: vec!["A skipped row".into()],
        };
        let mut transfer = ProfileTransfer::import(batch, &[profile("ALPHA")]);
        assert!(!transfer.is_export());
        assert_eq!(
            transfer.profiles().iter().map(|profile| profile.name.as_str()).collect::<Vec<_>>(),
            ["alpha (Imported 1)", "Alpha (Imported 2)"]
        );
        assert_eq!(transfer.warnings.len(), 3);
        transfer.selected[0] = false;
        assert_eq!(transfer.profiles()[0].name, "Alpha (Imported 2)");
    }
}
