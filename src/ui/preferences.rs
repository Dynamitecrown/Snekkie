//! Preferences dialog: colour theme and defaults for new sessions on one
//! page, animations on the other.

use std::collections::BTreeMap;

use egui::{Color32, DragValue, RichText, Slider, Stroke, Ui};

use super::animation_preview::AnimationPreview;
use super::{sidebar, style};
use crate::settings::{
    self, ANIMATION_SPEED_RANGE, AppSettings, CursorBlink, CursorMotion, KeystrokeBurst, NewLines, NewText, Reveal,
    Scheme, Scrolling, Shake, Theme, TypedText,
};

/// Width of the dialog, the same on both pages so it doesn't jump about
/// when you switch: room for two groups of animation styles side by side,
/// and still inside the smallest window.
pub const WIDTH: f32 = 500.0;

/// Width of each animation style drop-down, so the longest style name
/// ("Word by word") fits without the box growing.
const STYLE_WIDTH: f32 = 120.0;

/// Space between a page and the buttons under it.
const BUTTONS_GAP: f32 = 10.0;

/// Kept clear above and below the dialog in a short window: room for its
/// frame and a little of the window around it.
const EDGE_MARGIN: f32 = 24.0;

/// The least height a page is given, however short the window.
const MIN_PAGE_HEIGHT: f32 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    General,
    Animations,
}

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
    page: Page,
    /// The tallest a page has been, in points.
    page_height: f32,
    preview: AnimationPreview,
}

impl Preferences {
    pub fn new(settings: &AppSettings) -> Self {
        Preferences {
            settings: settings.clone(),
            saved_themes: settings.saved_themes.clone(),
            custom: settings.custom_theme(),
            page: Page::General,
            page_height: 0.0,
            preview: AnimationPreview::default(),
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
        self.page_tabs(ui);
        ui.add_space(8.0);

        // The page scrolls if the window is too short for it, so the tabs
        // above and the buttons below are always on screen. What's left of
        // the window's height after those, the dialog's frame and a margin
        // is the page's.
        let above = ui.cursor().min.y - ui.min_rect().min.y;
        let below = ui.spacing().item_spacing.y + BUTTONS_GAP + ui.spacing().interact_size.y;
        let room = (ui.ctx().content_rect().height() - 2.0 * EDGE_MARGIN - above - below).max(MIN_PAGE_HEIGHT);
        let page = self.page;
        egui::ScrollArea::vertical()
            .id_salt(page as u8) // each page keeps its own place
            .max_height(room)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let top = ui.cursor().min.y;
                match page {
                    Page::General => self.general_page(ui, monospace_fonts, &mut outcome),
                    Page::Animations => self.animations_page(ui),
                }
                // Each page takes the height of the tallest seen, so once
                // you've looked at both the dialog stays put as you switch,
                // or the room there is if that's less. A pass that's only
                // measuring, or will be thrown away, may be laid out oddly.
                let height = ui.cursor().min.y - top;
                if !ui.is_sizing_pass() && !ui.ctx().will_discard() {
                    self.page_height = self.page_height.max(height);
                }
                ui.add_space((self.page_height.min(room) - height).max(0.0));
            });

        ui.add_space(BUTTONS_GAP);
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

    fn page_tabs(&mut self, ui: &mut Ui) {
        let accent = ui.visuals().selection.bg_fill;
        // A rule along the bottom of the tabs, under the selected one's
        // underline, so it's laid down first and filled in after.
        let rule = ui.painter().add(egui::Shape::Noop);
        let tabs = ui.horizontal(|ui| {
            for (page, label) in [(Page::General, "General"), (Page::Animations, "Animations")] {
                if style::tab_button(ui, label, self.page == page, true, accent).clicked() && self.page != page {
                    self.page = page;
                    // A dialog is centred by its size last frame; draw again
                    // straight away so it doesn't sit off centre if this page
                    // is taller.
                    ui.ctx().request_repaint();
                }
            }
        });
        let y = tabs.response.rect.bottom();
        ui.painter().set(rule, egui::Shape::hline(ui.max_rect().x_range(), y, Stroke::new(1.0, style::BORDER)));
    }

    fn general_page(&mut self, ui: &mut Ui, monospace_fonts: &[String], outcome: &mut Outcome) {
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
                    *outcome = Outcome::AskThemeName { suggested };
                }
                let deletable = self.saved_themes.contains_key(&self.settings.theme);
                if ui.add_enabled(deletable, egui::Button::new("Delete")).clicked() {
                    *outcome = Outcome::ConfirmDelete(self.settings.theme.clone());
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

        ui.add_space(6.0);
        ui.checkbox(&mut self.settings.auto_paging, "Automatically page through show commands").on_hover_text(
            "Press Space at Cisco More prompts during show commands. Off by default. q or Ctrl+C stops it.",
        );
        ui.checkbox(&mut self.settings.check_for_updates, "Check for updates when Snekkie starts");
    }

    fn animations_page(&mut self, ui: &mut Ui) {
        let animations = &mut self.settings.animations;
        ui.checkbox(&mut animations.enabled, "Animations");
        ui.label(
            RichText::new("Off by default. Pick a style for each kind, or Off.").color(style::TEXT_SECONDARY).small(),
        );
        ui.add_space(6.0);

        // Greyed out while animations are off, but kept for when they're on.
        ui.add_enabled_ui(animations.enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Speed");
                ui.add_space(4.0);
                ui.label(RichText::new("Slower").color(style::TEXT_SECONDARY).small());
                ui.add(Slider::new(&mut animations.speed, ANIMATION_SPEED_RANGE).step_by(0.1).show_value(false))
                    .on_hover_text("How fast every animation plays. The cursor keeps blinking at its usual rate.");
                ui.label(RichText::new("Faster").color(style::TEXT_SECONDARY).small());
                ui.add_space(4.0);
                ui.label(format!("{:.1}×", animations.speed));
            });
            ui.add_space(6.0);
            ui.columns(2, |columns| {
                style::section_heading(&mut columns[0], "Typing");
                egui::Grid::new("animations_typing").num_columns(2).spacing([10.0, 6.0]).show(&mut columns[0], |ui| {
                    style_row(ui, "Cursor movement", &mut animations.cursor_motion);
                    style_row(ui, "Cursor blink", &mut animations.cursor_blink);
                    style_row(ui, "Typed characters", &mut animations.typed_text);
                    style_row(ui, "Keystroke burst", &mut animations.keystroke_burst);
                    style_row(ui, "Screen shake", &mut animations.shake);
                });
                style::section_heading(&mut columns[1], "Output");
                egui::Grid::new("animations_output").num_columns(2).spacing([10.0, 6.0]).show(&mut columns[1], |ui| {
                    style_row(ui, "New text", &mut animations.new_text);
                    style_row(ui, "Reveal", &mut animations.reveal);
                    style_row(ui, "Scrolling", &mut animations.scrolling);
                    style_row(ui, "New lines", &mut animations.new_lines);
                });
            });
        });

        ui.add_space(8.0);
        ui.label("Preview");
        let colors = self.current_colors();
        self.preview.ui(ui, colors, self.settings.animations);
    }
}

/// What a style drop-down needs to know about a kind of animation.
trait AnimationKind: Copy + PartialEq + 'static {
    fn styles() -> &'static [Self];
    fn name(self) -> &'static str;
    fn hint(self) -> &'static str;
}

macro_rules! animation_kinds {
    ($($kind:ident),+) => {
        $(impl AnimationKind for $kind {
            fn styles() -> &'static [Self] {
                $kind::ALL
            }

            fn name(self) -> &'static str {
                self.label()
            }

            fn hint(self) -> &'static str {
                self.help()
            }
        })+
    };
}

animation_kinds!(CursorMotion, CursorBlink, TypedText, KeystrokeBurst, Shake, NewText, Reveal, Scrolling, NewLines);

/// A kind of animation and a drop-down of its styles, each saying on hover
/// what it looks like.
fn style_row<K: AnimationKind>(ui: &mut Ui, label: &str, value: &mut K) {
    let label_id = ui.label(label).id;
    let combo = egui::ComboBox::from_id_salt(("animation", label))
        .width(STYLE_WIDTH)
        .selected_text(value.name())
        .show_ui(ui, |ui| {
            for &style in K::styles() {
                ui.selectable_value(value, style, style.name()).on_hover_text(style.hint());
            }
        });
    combo.response.labelled_by(label_id).on_hover_text(value.hint());
    ui.end_row();
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

    #[test]
    fn animation_choices_are_saved_with_the_rest() {
        let mut prefs = Preferences::new(&AppSettings::default());
        assert!(!prefs.working().animations.enabled);
        prefs.settings.animations.enabled = true;
        prefs.settings.animations.speed = 1.5;
        prefs.settings.animations.cursor_motion = CursorMotion::Smear;
        prefs.settings.animations.reveal = Reveal::Words;
        prefs.settings.theme = "Monokai".into();
        let saved = prefs.working();
        assert!(saved.animations.enabled);
        assert_eq!(saved.animations.speed, 1.5);
        assert_eq!(saved.animations.cursor_motion, CursorMotion::Smear);
        assert_eq!(saved.animations.reveal, Reveal::Words);
        assert_eq!(saved.theme, "Monokai");
    }

    #[test]
    fn each_drop_down_offers_every_style_by_its_label() {
        assert_eq!(CursorMotion::styles(), CursorMotion::ALL);
        assert_eq!(NewLines::styles(), NewLines::ALL);
        assert_eq!(Reveal::Words.name(), "Word by word");
        assert_eq!(Shake::Strong.hint(), Shake::Strong.help());
    }
}
