//! App-wide preferences: colour theme and defaults for new sessions.
//!
//! Distinct from a Profile (one saved connection): this is the one set of
//! look-and-feel settings shared by the whole app. Same settings.json as the
//! Python releases.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use egui::Color32;
use serde::{Deserialize, Serialize};

use crate::config;
use crate::profiles::lenient;

/// Keys every theme carries.
pub const THEME_KEYS: [&str; 4] = ["fg", "bg", "cursor", "selection"];

pub const DEFAULT_THEME: &str = "Snekkie Dark";

/// Built-in presets: foreground, background, cursor, selection. The
/// 16-colour ANSI palette stays fixed across themes.
pub const THEMES: [(&str, [&str; 4]); 6] = [
    ("Snekkie Dark", ["#d0d0d0", "#1a1a1a", "#3ad900", "#3a5a80"]),
    ("Solarized Dark", ["#839496", "#002b36", "#268bd2", "#073642"]),
    ("Solarized Light", ["#657b83", "#fdf6e3", "#268bd2", "#eee8d5"]),
    ("Monokai", ["#f8f8f2", "#272822", "#a6e22e", "#49483e"]),
    ("Classic Green", ["#33ff33", "#0c0c0c", "#33ff33", "#1f4d1f"]),
    ("High Contrast", ["#ffffff", "#000000", "#ffff00", "#444444"]),
];

/// Themes that have been renamed. A settings.json written before the app
/// was renamed still says "PyTerm Dark".
const THEME_ALIASES: [(&str, &str); 1] = [("PyTerm Dark", "Snekkie Dark")];

/// The four colours a theme sets, resolved to real colours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub fg: Color32,
    pub bg: Color32,
    pub cursor: Color32,
    pub selection: Color32,
}

impl Default for Theme {
    fn default() -> Self {
        preset(DEFAULT_THEME).unwrap()
    }
}

/// Parse "#rrggbb" (or "rrggbb").
pub fn parse_hex(s: &str) -> Option<Color32> {
    let hex = s.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn preset(name: &str) -> Option<Theme> {
    let name = THEME_ALIASES.iter().find(|(old, _)| *old == name).map_or(name, |(_, new)| new);
    THEMES.iter().find(|(n, _)| *n == name).map(|(_, [fg, bg, cursor, selection])| Theme {
        fg: parse_hex(fg).unwrap(),
        bg: parse_hex(bg).unwrap(),
        cursor: parse_hex(cursor).unwrap(),
        selection: parse_hex(selection).unwrap(),
    })
}

pub fn is_builtin(name: &str) -> bool {
    THEMES.iter().any(|(n, _)| *n == name)
}

/// A theme saved by the user, as stored in settings.json.
pub type Scheme = BTreeMap<String, String>;

impl Theme {
    /// Build from a stored scheme, falling back per key to the default
    /// theme so a hand-edited file missing one colour still works.
    pub fn from_scheme(scheme: &Scheme) -> Theme {
        let base = Theme::default();
        let get = |key: &str, fallback: Color32| scheme.get(key).and_then(|v| parse_hex(v)).unwrap_or(fallback);
        Theme {
            fg: get("fg", base.fg),
            bg: get("bg", base.bg),
            cursor: get("cursor", base.cursor),
            selection: get("selection", base.selection),
        }
    }

    pub fn to_scheme(self) -> Scheme {
        [("fg", self.fg), ("bg", self.bg), ("cursor", self.cursor), ("selection", self.selection)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), to_hex(v)))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    #[serde(deserialize_with = "lenient")]
    pub theme: String,
    #[serde(deserialize_with = "lenient")]
    pub custom_fg: String,
    #[serde(deserialize_with = "lenient")]
    pub custom_bg: String,
    #[serde(deserialize_with = "lenient")]
    pub custom_cursor: String,
    #[serde(deserialize_with = "lenient")]
    pub custom_selection: String,
    /// Themes the user saved, name -> {fg, bg, cursor, selection}.
    #[serde(deserialize_with = "lenient_themes")]
    pub saved_themes: BTreeMap<String, Scheme>,
    // Defaults filled into a brand-new session's Advanced tab.
    #[serde(deserialize_with = "lenient")]
    pub font_family: String,
    #[serde(deserialize_with = "lenient")]
    pub font_size: u32,
    #[serde(deserialize_with = "lenient")]
    pub scrollback: u32,
    #[serde(deserialize_with = "lenient")]
    pub show_sidebar: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            theme: DEFAULT_THEME.into(),
            custom_fg: "#d0d0d0".into(),
            custom_bg: "#1a1a1a".into(),
            custom_cursor: "#3ad900".into(),
            custom_selection: "#3a5a80".into(),
            saved_themes: BTreeMap::new(),
            font_family: String::new(),
            font_size: 11,
            scrollback: 5000,
            show_sidebar: true,
        }
    }
}

/// Keep only well-formed saved themes: a malformed one would otherwise
/// surface far from the cause, at paint time.
fn lenient_themes<'de, D>(deserializer: D) -> Result<BTreeMap<String, Scheme>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let mut clean = BTreeMap::new();
    if let serde_json::Value::Object(themes) = value {
        for (name, scheme) in themes {
            let serde_json::Value::Object(scheme) = scheme else { continue };
            let colours: Scheme = scheme
                .into_iter()
                .filter(|(k, _)| THEME_KEYS.contains(&k.as_str()))
                .filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_string())))
                .collect();
            if !colours.is_empty() {
                clean.insert(name, colours);
            }
        }
    }
    Ok(clean)
}

impl AppSettings {
    /// The unsaved, currently-being-edited custom scheme.
    pub fn custom_theme(&self) -> Theme {
        let base = Theme::default();
        Theme {
            fg: parse_hex(&self.custom_fg).unwrap_or(base.fg),
            bg: parse_hex(&self.custom_bg).unwrap_or(base.bg),
            cursor: parse_hex(&self.custom_cursor).unwrap_or(base.cursor),
            selection: parse_hex(&self.custom_selection).unwrap_or(base.selection),
        }
    }

    pub fn set_custom_theme(&mut self, theme: Theme) {
        self.custom_fg = to_hex(theme.fg);
        self.custom_bg = to_hex(theme.bg);
        self.custom_cursor = to_hex(theme.cursor);
        self.custom_selection = to_hex(theme.selection);
    }

    /// Colours for any theme name: preset, saved, or "Custom".
    pub fn theme_named(&self, name: &str) -> Theme {
        if name == "Custom" {
            return self.custom_theme();
        }
        if let Some(scheme) = self.saved_themes.get(name) {
            return Theme::from_scheme(scheme);
        }
        preset(name).unwrap_or_default()
    }

    /// Colours of the active theme.
    pub fn colors(&self) -> Theme {
        self.theme_named(&self.theme)
    }

    /// Built-in presets first, then the user's own, then Custom.
    pub fn theme_names(&self) -> Vec<String> {
        THEMES
            .iter()
            .map(|(n, _)| n.to_string())
            .chain(self.saved_themes.keys().cloned())
            .chain(std::iter::once("Custom".to_string()))
            .collect()
    }
}

pub struct SettingsStore {
    pub path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: PathBuf) -> Self {
        SettingsStore { path }
    }

    pub fn default_path() -> PathBuf {
        config::config_dir().join("settings.json")
    }

    pub fn load(&self) -> AppSettings {
        let mut settings: AppSettings =
            fs::read_to_string(&self.path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        if settings.font_size == 0 {
            settings.font_size = 11;
        }
        if settings.theme.is_empty() {
            settings.theme = DEFAULT_THEME.into();
        }
        settings
    }

    pub fn save(&self, settings: &AppSettings) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?;
        config::write_atomic(&self.path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> AppSettings {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, text).unwrap();
        SettingsStore::new(path).load()
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let settings = SettingsStore::new(dir.path().join("nope.json")).load();
        assert_eq!(settings, AppSettings::default());
        assert_eq!(settings.colors(), Theme::default());
    }

    #[test]
    fn loads_python_settings_and_saved_themes() {
        let settings = load(
            r##"{
              "theme": "Mine",
              "custom_fg": "#111111", "custom_bg": "#222222",
              "custom_cursor": "#333333", "custom_selection": "#444444",
              "saved_themes": {
                "Mine": {"fg": "#010203", "bg": "#040506"},
                "Broken": "not a dict",
                "Empty": {"nonsense": "#ffffff"}
              },
              "font_family": "Consolas", "font_size": 13,
              "scrollback": 20000, "show_sidebar": false
            }"##,
        );
        assert_eq!(settings.saved_themes.keys().collect::<Vec<_>>(), ["Mine"]);
        let theme = settings.colors();
        assert_eq!(theme.fg, Color32::from_rgb(1, 2, 3));
        assert_eq!(theme.bg, Color32::from_rgb(4, 5, 6));
        // Missing keys fall back to the default theme's.
        assert_eq!(theme.cursor, Theme::default().cursor);
        assert_eq!(settings.font_family, "Consolas");
        assert!(!settings.show_sidebar);
        assert_eq!(settings.theme_names().last().unwrap(), "Custom");
        assert!(settings.theme_names().contains(&"Mine".to_string()));
    }

    #[test]
    fn renamed_theme_resolves() {
        let settings = load(r#"{"theme": "PyTerm Dark"}"#);
        assert_eq!(settings.colors(), Theme::default());
    }

    #[test]
    fn custom_theme_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(dir.path().join("settings.json"));
        let mut settings = AppSettings { theme: "Custom".into(), ..AppSettings::default() };
        let theme = Theme {
            fg: Color32::from_rgb(10, 20, 30),
            bg: Color32::from_rgb(40, 50, 60),
            cursor: Color32::from_rgb(70, 80, 90),
            selection: Color32::from_rgb(100, 110, 120),
        };
        settings.set_custom_theme(theme);
        store.save(&settings).unwrap();
        assert_eq!(store.load().colors(), theme);
    }

    #[test]
    fn hex_parsing() {
        assert_eq!(parse_hex("#ff8000"), Some(Color32::from_rgb(255, 128, 0)));
        assert_eq!(parse_hex("ff8000"), Some(Color32::from_rgb(255, 128, 0)));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("#gggggg"), None);
        assert_eq!(to_hex(Color32::from_rgb(1, 171, 255)), "#01abff");
    }
}
