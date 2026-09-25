//! Preferences dialog: colour theme and defaults for new sessions.

use std::collections::BTreeMap;

use egui::{Color32, DragValue, RichText, Ui};

use super::{sidebar, style};
use crate::settings::{self, AppSettings, Scheme, Theme};

/// What the dialog asks of the app this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Open,
    Cancel,
    Save(AppSettings),
    /// Ask for a name to save the current colours under.
    AskThemeName {
        suggested: String,
    },
    /// Ask before deleting a saved theme.
    ConfirmDelete(String),
}

pub struct Preferences {
    settings: AppSettings,
    saved_themes: BTreeMap<String, Scheme>,
    custom: Theme,
}

impl Preferences {
    pub fn new(settings: &AppSettings) -> Self {
        Preferences {
            settings: settings.clone(),
            saved_themes: settings.saved_themes.clone(),
            custom: settings.custom_theme(),
        }
    }

    fn working(&self) -> AppSettings {
        let mut s = self.settings.clone();
        s.saved_themes = self.saved_themes.clone();
        s.set_custom_theme(self.custom);
        s
    }

    /// Colours currently on screen.
    pub fn current_colors(&self) -> Theme {
        self.working().theme_named(&self.settings.theme)
    }

    /// True if saving under this name would replace one of the user's own.
    pub fn would_replace(&self, name: &str) -> bool {
        self.saved_themes.contains_key(name)
    }

    /// Store the colours on screen under a name. Built-in names are refused.
    pub fn save_theme(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Enter a name for the theme.".into());
        }
        if settings::is_builtin(name) || name == "Custom" {
            return Err(format!("“{name}” is a built-in theme. Pick another name."));
        }
        let colors = self.current_colors();
        self.saved_themes.insert(name.to_string(), colors.to_scheme());
        self.settings.theme = name.to_string();
        Ok(())
    }

    pub fn delete_theme(&mut self, name: &str) {
        if self.saved_themes.remove(name).is_some() && self.settings.theme == name {
            self.settings.theme = settings::DEFAULT_THEME.into();
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, monospace_fonts: &[String]) -> Outcome {
        let mut outcome = Outcome::Open;
        ui.heading("Preferences");
        ui.add_space(6.0);

        egui::Grid::new("prefs_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Colour theme");
            ui.horizontal(|ui| {
                let names = self.working().theme_names();
                egui::ComboBox::from_id_salt("theme").width(180.0).selected_text(self.settings.theme.clone()).show_ui(
                    ui,
                    |ui| {
                        for name in names {
                            ui.selectable_value(&mut self.settings.theme, name.clone(), name);
                        }
                    },
                );
                if ui.button("Save as…").clicked() {
                    let current = &self.settings.theme;
                    let suggested = if settings::is_builtin(current) || current == "Custom" {
                        String::new()
                    } else {
                        current.clone()
                    };
                    outcome = Outcome::AskThemeName { suggested };
                }
                let deletable = self.saved_themes.contains_key(&self.settings.theme);
                if ui.add_enabled(deletable, egui::Button::new("Delete")).clicked() {
                    outcome = Outcome::ConfirmDelete(self.settings.theme.clone());
                }
            });
            ui.end_row();

            ui.label("Preview");
            let colors = self.current_colors();
            egui::Frame::new().fill(colors.bg).stroke(egui::Stroke::new(1.0, style::BORDER)).inner_margin(8.0).show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label(RichText::new("user@host:~$ ls -la").monospace().color(colors.fg));
                        ui.label(RichText::new(" ").monospace().background_color(colors.cursor));
                        ui.label(RichText::new("   ").monospace());
                        ui.label(
                            RichText::new("selected").monospace().color(colors.fg).background_color(colors.selection),
                        );
                    });
                },
            );
            ui.end_row();
        });

        let custom = self.settings.theme == "Custom";
        ui.add_space(4.0);
        ui.add_enabled_ui(custom, |ui| {
            egui::CollapsingHeader::new("Custom colours").default_open(true).show(ui, |ui| {
                egui::Grid::new("custom_colors").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    for (label, color) in [
                        ("Text", &mut self.custom.fg),
                        ("Background", &mut self.custom.bg),
                        ("Cursor", &mut self.custom.cursor),
                        ("Selection", &mut self.custom.selection),
                    ] {
                        ui.label(label);
                        color_button(ui, color);
                        ui.end_row();
                    }
                });
                if !custom {
                    ui.label(
                        RichText::new("Choose the Custom theme to edit these.").color(style::TEXT_SECONDARY).small(),
                    );
                }
            });
        });

        ui.add_space(6.0);
        egui::Grid::new("prefs_defaults").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Font (new sessions)");
            sidebar::font_picker(ui, "prefs_font", &mut self.settings.font_family, monospace_fonts);
            ui.end_row();

            ui.label("Font size");
            ui.add(DragValue::new(&mut self.settings.font_size).range(6..=48));
            ui.end_row();

            ui.label("Default scrollback");
            ui.add(DragValue::new(&mut self.settings.scrollback).range(0..=200_000).speed(100.0));
            ui.end_row();
        });
        ui.label(
            RichText::new("Font settings apply to sessions opened from now on.").color(style::TEXT_SECONDARY).small(),
        );

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                outcome = Outcome::Cancel;
            }
            if ui.add(egui::Button::new(RichText::new("OK").strong())).clicked() {
                outcome = Outcome::Save(self.working());
            }
        });
        outcome
    }
}

fn color_button(ui: &mut Ui, color: &mut Color32) {
    let mut rgb = [color.r(), color.g(), color.b()];
    if ui.color_edit_button_srgb(&mut rgb).changed() {
        *color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    }
    ui.label(RichText::new(settings::to_hex(*color)).monospace().color(style::TEXT_SECONDARY));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_a_theme_selects_it_and_refuses_builtin_names() {
        let mut prefs = Preferences::new(&AppSettings::default());
        prefs.settings.theme = "Monokai".into();
        assert!(prefs.save_theme("Monokai").unwrap_err().contains("built-in"));
        prefs.save_theme("Mine").unwrap();
        let saved = prefs.working();
        assert_eq!(saved.theme, "Mine");
        assert_eq!(
            saved.colors(),
            settings::AppSettings { theme: "Monokai".into(), ..AppSettings::default() }.colors()
        );
        assert!(prefs.would_replace("Mine"));

        prefs.delete_theme("Mine");
        assert_eq!(prefs.working().theme, settings::DEFAULT_THEME);
        assert!(prefs.working().saved_themes.is_empty());
    }

    #[test]
    fn custom_colours_are_kept() {
        let mut prefs = Preferences::new(&AppSettings::default());
        prefs.settings.theme = "Custom".into();
        prefs.custom.bg = Color32::from_rgb(1, 2, 3);
        let saved = prefs.working();
        assert_eq!(saved.custom_bg, "#010203");
        assert_eq!(saved.colors().bg, Color32::from_rgb(1, 2, 3));
    }
}
