//! Preferences grouped by task, with draft settings and a fixed save footer.

use std::collections::BTreeMap;

use egui::{Color32, DragValue, RichText, Slider, Stroke, Ui};

use super::animation_preview::AnimationPreview;
use super::fonts::{self, Fonts};
use super::syntax_preview::SyntaxPreview;
use super::theme_preview::ThemePreview;
use super::{sidebar, style};
use crate::settings::{
    self, ANIMATION_SPEED_RANGE, AppSettings, CursorBlink, CursorMotion, KeystrokeBurst, NewLines, NewText, Reveal,
    Scheme, Scrolling, Shake, Theme, TypedText,
};
use crate::terminal::highlight;

/// Preferred content width. The app clamps this to the viewport.
pub const WIDTH: f32 = 640.0;

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
    Appearance,
    Terminal,
    Input,
    Network,
    Highlighting,
    Animations,
}

impl Page {
    const ALL: [(Self, &'static str); 6] = [
        (Self::Appearance, "Appearance"),
        (Self::Terminal, "Terminal"),
        (Self::Input, "Input & paste"),
        (Self::Network, "Network"),
        (Self::Highlighting, "Highlighting"),
        (Self::Animations, "Animations"),
    ];
}

/// What the dialog asks of the app this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Open,
    Cancel,
    Save(AppSettings),
    /// Ask for a name to save the current colors under.
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
    offline_at_open: bool,
    preview: AnimationPreview,
    theme_preview: ThemePreview,
    syntax_preview: SyntaxPreview,
    preview_syntax: String,
    pub update_busy: bool,
    pub network_sessions: usize,
}

impl Preferences {
    pub fn new(settings: &AppSettings) -> Self {
        Preferences {
            settings: settings.clone(),
            saved_themes: settings.saved_themes.clone(),
            custom: settings.custom_theme(),
            page: Page::Appearance,
            offline_at_open: settings.offline_mode,
            preview: AnimationPreview::default(),
            theme_preview: ThemePreview::default(),
            syntax_preview: SyntaxPreview::default(),
            preview_syntax: "cisco_ios".into(),
            update_busy: false,
            network_sessions: 0,
        }
    }

    fn working(&self) -> AppSettings {
        let mut s = self.settings.clone();
        s.saved_themes = self.saved_themes.clone();
        s.set_custom_theme(self.custom);
        s
    }

    /// Colors currently on screen.
    pub fn current_colors(&self) -> Theme {
        self.working().theme_named(&self.settings.theme)
    }

    /// True if saving under this name would replace one of the user's own.
    pub fn would_replace(&self, name: &str) -> bool {
        self.saved_themes.contains_key(name)
    }

    /// Store the colors on screen under a name. Built-in names are refused.
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

    pub fn ui(&mut self, ui: &mut Ui, fonts: &mut Fonts) -> Outcome {
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
        let below = ui.spacing().item_spacing.y + BUTTONS_GAP + 30.0;
        let room = (ui.ctx().content_rect().height() - 2.0 * EDGE_MARGIN - above - below).max(MIN_PAGE_HEIGHT);
        let page = self.page;
        egui::ScrollArea::vertical()
            .id_salt(page as u8) // each page keeps its own place
            .min_scrolled_height(MIN_PAGE_HEIGHT)
            .max_height(room)
            .auto_shrink([false, false])
            .show(ui, |ui| match page {
                Page::Appearance => self.appearance_page(ui, fonts, &mut outcome),
                Page::Terminal => self.terminal_page(ui, fonts),
                Page::Input => self.input_page(ui),
                Page::Network => self.network_page(ui),
                Page::Highlighting => self.highlighting_page(ui),
                Page::Animations => self.animations_page(ui),
            });

        ui.add_space(BUTTONS_GAP);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let accent = ui.visuals().selection.bg_fill;
                if ui.add(style::accent_button("Save changes", accent)).clicked() {
                    outcome = Outcome::Save(self.working());
                }
                if ui.add(egui::Button::new("Cancel").min_size(egui::vec2(0.0, 30.0))).clicked() {
                    outcome = Outcome::Cancel;
                }
            });
        });
        outcome
    }

    fn page_tabs(&mut self, ui: &mut Ui) {
        let accent = ui.visuals().selection.bg_fill;
        // A rule along the bottom of the tabs, under the selected one's
        // underline, so it's laid down first and filled in after.
        let rule = ui.painter().add(egui::Shape::Noop);
        let tabs = ui.horizontal_wrapped(|ui| {
            for (page, label) in Page::ALL {
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
        ui.painter().set(rule, egui::Shape::hline(ui.max_rect().x_range(), y, Stroke::new(1.0, style::border(ui))));
    }

    fn appearance_page(&mut self, ui: &mut Ui, fonts: &mut Fonts, outcome: &mut Outcome) {
        scope_note(ui, "Theme changes apply to all open and new sessions after you save.");
        egui::Grid::new("prefs_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Color theme");
            ui.horizontal(|ui| {
                let names = self.working().theme_names();
                egui::ComboBox::from_id_salt("theme")
                    .width((ui.available_width() - 190.0).clamp(120.0, 320.0))
                    .truncate()
                    .height(320.0)
                    .selected_text(self.settings.theme.clone())
                    .show_ui(ui, |ui| {
                        for name in names {
                            ui.selectable_value(&mut self.settings.theme, name.clone(), name);
                        }
                    });
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
        });
        self.font_preview(ui, fonts);

        let mut colors = self.current_colors();
        let mut colors_changed = false;
        ui.add_space(4.0);
        egui::CollapsingHeader::new("Theme colors").default_open(true).show(ui, |ui| {
            egui::Grid::new("custom_colors").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                for (label, color) in [
                    ("Text", &mut colors.fg),
                    ("Background", &mut colors.bg),
                    ("Cursor", &mut colors.cursor),
                    ("Selection", &mut colors.selection),
                ] {
                    let label_id = ui.label(label).id;
                    colors_changed |= ui.horizontal(|ui| color_button(ui, color, label_id)).inner;
                    ui.end_row();
                }
            });
            colors_changed |= ui.checkbox(&mut colors.effects.monochrome, "Monochrome output")
                .on_hover_text("Use the theme's text and background colors for all output, including device colors and syntax highlighting.")
                .changed();
            colors_changed |= ui.checkbox(&mut colors.effects.crt, "CRT glow and scanlines")
                .on_hover_text("Static glow and subtle scanlines inside the terminal. Works with animations off.")
                .changed();
            let mut appearance = colors.effects.appearance();
            let original_appearance = appearance;
            ui.horizontal(|ui| {
                ui.label("App appearance");
                egui::ComboBox::from_id_salt("app_appearance").selected_text(appearance.label()).show_ui(ui, |ui| {
                    for option in crate::settings::AppAppearance::ALL { ui.selectable_value(&mut appearance, option, option.label()); }
                });
            });
            if appearance != original_appearance {
                colors.effects.retro_ui = false;
                colors.effects.appearance = appearance;
                colors_changed = true;
            }
            ui.label(
                RichText::new("Adjust these colors, then use Save as… to name your theme.")
                    .color(style::secondary(ui))
                    .small(),
            );
        });
        if colors_changed {
            // Seed the Custom draft with the entire selected palette, including
            // every color the user left unchanged. Presets and saved themes
            // keep their original colors until explicitly saved under a name.
            self.custom = colors;
            self.settings.theme = "Custom".into();
        }
    }

    fn terminal_page(&mut self, ui: &mut Ui, fonts: &mut Fonts) {
        scope_note(
            ui,
            "Defaults for New session. Open sessions and saved profiles keep their own font and scrollback settings.",
        );
        style::section_heading(ui, "New session defaults");
        egui::Grid::new("prefs_defaults").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            let label = ui.label("Font family");
            sidebar::font_picker(ui, "prefs_font", &mut self.settings.font_family, &fonts.monospace)
                .labelled_by(label.id);
            ui.end_row();

            let label = ui.label("Font size");
            ui.add(DragValue::new(&mut self.settings.font_size).range(6..=48).suffix(" pt")).labelled_by(label.id);
            ui.end_row();

            let label = ui.label("Scrollback lines");
            ui.add(DragValue::new(&mut self.settings.scrollback).range(0..=200_000).speed(100.0)).labelled_by(label.id);
            ui.end_row();
        });
        self.font_preview(ui, fonts);
    }

    fn font_preview(&mut self, ui: &mut Ui, fonts: &mut Fonts) {
        ui.add_space(8.0);
        ui.label(format!("Preview — {} pt", self.settings.font_size));
        let family = fonts.family(ui.ctx(), &self.settings.font_family);
        let (regular, bold) = Fonts::font_ids(family.as_deref(), fonts::points_to_pixels(self.settings.font_size));
        let colors = self.current_colors();
        self.theme_preview.ui(ui, colors, regular, bold);
    }

    fn input_page(&mut self, ui: &mut Ui) {
        scope_note(ui, "Used for future input and paste actions in open and new sessions after you save.");
        style::section_heading(ui, "Mouse and clipboard");
        ui.checkbox(&mut self.settings.right_click_paste, "Right-click pastes (PuTTY style)").on_hover_text(
            "Enabled by default. Turn off to open the terminal context menu on right-click and choose Paste there. Ctrl+right-click opens the menu in either mode.",
        );
        ui.checkbox(&mut self.settings.preview_multiline_paste, "Preview multiline paste")
            .on_hover_text("Review the destination and edit text before sending. Single-line paste stays immediate.");
        ui.horizontal(|ui| {
            let label = ui.label("Paste delay between lines");
            ui.add(
                egui::DragValue::new(&mut self.settings.paste_delay_ms)
                    .range(0..=crate::paste::MAX_DELAY_MS)
                    .suffix(" ms"),
            )
            .labelled_by(label.id);
        });
        ui.checkbox(&mut self.settings.auto_paging, "Automatically page through show commands").on_hover_text(
            "Press Space at Cisco More prompts during show commands. Off by default. q or Ctrl+C stops it.",
        );
    }

    fn network_page(&mut self, ui: &mut Ui) {
        scope_note(ui, "Network policy and update settings apply to the whole app after you save.");
        style::section_heading(ui, "Network privacy");
        let offline =
            ui.add_enabled(!self.update_busy, egui::Checkbox::new(&mut self.settings.offline_mode, "Offline mode"));
        if self.update_busy {
            ui.label(
                RichText::new("Wait for the current update check or download to finish before changing offline mode.")
                    .small(),
            );
        }
        if self.settings.offline_mode {
            self.settings.check_for_updates = false;
        }
        ui.add(egui::Label::new("Offline mode asks before DNS lookups and connections outside private/local IP ranges. Serial and local IP connections work directly.").wrap());
        if self.settings.offline_mode && !self.offline_at_open {
            let warning = if self.network_sessions > 0 {
                format!(
                    "Saving will close {} open network {}. Serial sessions stay connected.",
                    self.network_sessions,
                    if self.network_sessions == 1 { "session" } else { "sessions" }
                )
            } else {
                "Enabling Offline mode closes open network sessions. Serial sessions stay connected.".into()
            };
            let warning = egui::Frame::new()
                .fill(ui.visuals().warn_fg_color.gamma_multiply(0.12))
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(warning).strong().color(ui.visuals().warn_fg_color)).wrap());
                });
            if offline.changed() {
                warning.response.scroll_to_me(Some(egui::Align::Center));
            }
        }
        ui.add_space(12.0);
        style::section_heading(ui, "Updates");
        if self.settings.offline_mode {
            ui.add(egui::Label::new("Update checks and downloads are disabled in Offline mode.").wrap());
        }
        ui.add_enabled(
            !self.settings.offline_mode,
            egui::Checkbox::new(&mut self.settings.check_for_updates, "Check for updates when Snekkie starts"),
        );
    }

    fn highlighting_page(&mut self, ui: &mut Ui) {
        scope_note(ui, "Highlighting intensity applies to all open and new sessions after you save.");
        ui.label("Syntax highlighting intensity");
        ui.horizontal(|ui| {
            ui.label("Less");
            ui.add(
                Slider::new(&mut self.settings.highlighting_intensity, highlight::INTENSITY_RANGE)
                    .step_by(1.0)
                    .text("Intensity"),
            );
            ui.label("More");
        });
        ui.label(RichText::new(highlight::intensity_label(self.settings.highlighting_intensity)).strong());
        ui.label(highlight::intensity_help(self.settings.highlighting_intensity));
        ui.label(
            RichText::new("Choose each session's vendor in the sidebar's Highlighting menu. The preview below does not change a session's vendor."),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label("Preview device");
            egui::ComboBox::from_id_salt("highlight_preview_device")
                .selected_text(highlight::syntax_label(&self.preview_syntax))
                .height(280.0)
                .show_ui(ui, |ui| {
                    for (key, label) in highlight::syntax_options() {
                        ui.selectable_value(&mut self.preview_syntax, key.to_string(), label);
                    }
                });
        });
        let theme = self.current_colors();
        if theme.effects.monochrome {
            ui.label(
                RichText::new(
                    "This theme uses monochrome output. Turn it off under Appearance → Theme colors to see syntax colors.",
                )
                .small(),
            );
        }
        self.syntax_preview.ui(ui, theme, &self.preview_syntax, self.settings.highlighting_intensity);
    }

    fn animations_page(&mut self, ui: &mut Ui) {
        scope_note(ui, "Animation changes apply to all open and new sessions after you save.");
        let animations = &mut self.settings.animations;
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut animations.enabled, "Animations");
            if ui
                .button("Reduce motion")
                .on_hover_text(
                    "Use a steady cursor and a short output fade. No movement, particles, shake or delayed reveal.",
                )
                .clicked()
            {
                *animations = settings::Animations::reduced_motion();
            }
        });
        ui.label(
            RichText::new(if animations.enabled {
                "Pick a style for each kind, or choose Off."
            } else {
                "Animations are disabled. Enable them to adjust styles; your choices are kept."
            })
            .color(style::secondary(ui)),
        );
        ui.add_space(6.0);

        // Greyed out while animations are off, but kept for when they're on.
        ui.add_enabled_ui(animations.enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Speed");
                ui.add_space(4.0);
                ui.label(RichText::new("Slower").color(style::secondary(ui)).small());
                ui.add(Slider::new(&mut animations.speed, ANIMATION_SPEED_RANGE).step_by(0.1).show_value(false))
                    .on_hover_text("How fast every animation plays. The cursor keeps blinking at its usual rate.");
                ui.label(RichText::new("Faster").color(style::secondary(ui)).small());
                ui.add_space(4.0);
                ui.label(format!("{:.1}×", animations.speed));
            });
            ui.add_space(6.0);
            if ui.available_width() < 560.0 {
                typing_styles(ui, animations);
                ui.add_space(12.0);
                output_styles(ui, animations);
            } else {
                ui.columns(2, |columns| {
                    typing_styles(&mut columns[0], animations);
                    output_styles(&mut columns[1], animations);
                });
            }
        });

        ui.add_space(8.0);
        ui.label("Preview");
        let colors = self.current_colors();
        self.preview.ui(ui, colors, self.settings.animations);
    }
}

fn scope_note(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(style::secondary(ui))).wrap());
    ui.add_space(8.0);
}

fn typing_styles(ui: &mut Ui, animations: &mut settings::Animations) {
    style::section_heading(ui, "Typing");
    egui::Grid::new("animations_typing").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        style_row(ui, "Cursor movement", &mut animations.cursor_motion);
        style_row(ui, "Cursor blink", &mut animations.cursor_blink);
        style_row(ui, "Typed characters", &mut animations.typed_text);
        style_row(ui, "Keystroke burst", &mut animations.keystroke_burst);
        style_row(ui, "Screen shake", &mut animations.shake);
    });
}

fn output_styles(ui: &mut Ui, animations: &mut settings::Animations) {
    style::section_heading(ui, "Output");
    egui::Grid::new("animations_output").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        style_row(ui, "New text", &mut animations.new_text);
        style_row(ui, "Reveal", &mut animations.reveal);
        style_row(ui, "Scrolling", &mut animations.scrolling);
        style_row(ui, "New lines", &mut animations.new_lines);
    });
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

fn color_button(ui: &mut Ui, color: &mut Color32, label_id: egui::Id) -> bool {
    let mut rgb = [color.r(), color.g(), color.b()];
    let changed = ui.color_edit_button_srgb(&mut rgb).labelled_by(label_id).changed();
    if changed {
        *color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    }
    ui.label(RichText::new(settings::to_hex(*color)).monospace().color(style::secondary(ui)));
    changed
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
    fn custom_colors_are_kept() {
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
