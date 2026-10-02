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
    /// Ask GitHub for a newer release each time Snekkie starts.
    #[serde(deserialize_with = "lenient")]
    pub check_for_updates: bool,
    /// Automatically press Space at paging prompts during show commands.
    #[serde(deserialize_with = "lenient")]
    pub auto_paging: bool,
    #[serde(deserialize_with = "lenient")]
    pub animations: Animations,
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
            check_for_updates: true,
            auto_paging: false,
            animations: Animations::default(),
        }
    }
}

/// Declares one kind of animation: its styles, each with the name stored in
/// settings.json, the label Preferences shows and a line of hover help. An
/// unknown name in the file falls back to the default style.
macro_rules! animation_kind {
    (
        $(#[$meta:meta])*
        $name:ident, default $default:ident,
        { $($variant:ident = $key:literal, $label:literal, $help:literal;)+ }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $key)] $variant,)+
        }

        impl Default for $name {
            fn default() -> Self {
                $name::$default
            }
        }

        impl $name {
            pub const ALL: &[$name] = &[$($name::$variant),+];

            pub fn label(self) -> &'static str {
                match self {
                    $($name::$variant => $label,)+
                }
            }

            pub fn help(self) -> &'static str {
                match self {
                    $($name::$variant => $help,)+
                }
            }
        }
    };
}

animation_kind! {
    /// How the cursor gets from one cell to the next.
    CursorMotion, default Glide, {
        Off = "off", "Off", "The cursor jumps straight to its new cell.";
        Glide = "glide", "Glide", "The cursor slides to its new cell and eases to a stop.";
        Spring = "spring", "Spring", "The cursor springs to its new cell, overshooting a little before settling.";
        Smear = "smear", "Smear", "The cursor stretches toward its new cell and its tail catches up.";
        Ghost = "ghost", "Ghost", "The cursor jumps, leaving fading afterimages along the way it went.";
    }
}

animation_kind! {
    /// How the cursor blinks while the terminal has the keyboard.
    CursorBlink, default Fade, {
        Classic = "classic", "Classic", "The cursor switches on and off, as it does with animations off.";
        Fade = "fade", "Fade", "The cursor fades out and back in.";
        Pulse = "pulse", "Pulse", "The cursor dims and brightens without ever disappearing.";
        Glow = "glow", "Glow", "The cursor stays on, with a soft halo that swells and fades around it.";
    }
}

animation_kind! {
    /// What a character you type does when it lands on the screen.
    TypedText, default Pop, {
        Off = "off", "Off", "Typed characters appear as they are.";
        Pop = "pop", "Pop", "Each character pops in a little larger, in the cursor colour, then settles.";
        Bounce = "bounce", "Bounce", "Each character drops into place with a small bounce.";
        Flash = "flash", "Flash", "The cell behind each character flashes the cursor colour and fades.";
        Fade = "fade", "Fade", "Each character fades in.";
    }
}

animation_kind! {
    /// Particles thrown off the cursor on each keystroke.
    KeystrokeBurst, default Sparks, {
        Off = "off", "Off", "Nothing comes off the cursor.";
        Sparks = "sparks", "Sparks", "A spray of sparks in the cursor colour that fall away.";
        Confetti = "confetti", "Confetti", "A pinch of colourful confetti that tumbles down.";
        Embers = "embers", "Embers", "Warm embers that drift upward and die out.";
        Bubbles = "bubbles", "Bubbles", "Small bubbles that float up and fade.";
        Stars = "stars", "Stars", "A few twinkling stars that scatter outward.";
        Ripple = "ripple", "Ripple", "A ring that spreads out from the cursor.";
    }
}

animation_kind! {
    /// A jolt of the whole terminal on each keystroke.
    Shake, default Off, {
        Off = "off", "Off", "The terminal stays still.";
        Gentle = "gentle", "Gentle", "The terminal nudges a pixel or two with each keystroke.";
        Strong = "strong", "Strong", "The terminal shakes with each keystroke.";
    }
}

animation_kind! {
    /// How characters arriving from the device appear.
    NewText, default Fade, {
        Off = "off", "Off", "New text appears as it is.";
        Fade = "fade", "Fade", "New text fades in from the background.";
        Rise = "rise", "Rise", "New text rises into place as it fades in.";
        Drop = "drop", "Drop", "New text drops into place as it fades in.";
        Zoom = "zoom", "Zoom", "New text grows from small to full size.";
        Decode = "decode", "Decode", "New text flickers through random symbols before settling, like decrypting.";
        Heat = "heat", "Heat", "New text arrives in the cursor colour and cools to its own colour.";
    }
}

animation_kind! {
    /// Whether output is held back to be drawn a piece at a time. Big bursts
    /// always catch up quickly, so you're never left waiting.
    Reveal, default Instant, {
        Instant = "instant", "Instant", "Output is drawn the moment it arrives.";
        Typewriter = "typewriter", "Typewriter", "Output is typed out a character at a time.";
        Words = "words", "Word by word", "Output appears a word at a time.";
        Lines = "lines", "Line by line", "Output appears a line at a time.";
    }
}

animation_kind! {
    /// How the screen moves when new lines push it up, or when you scroll.
    Scrolling, default Smooth, {
        Off = "off", "Off", "The screen jumps a whole line at a time.";
        Smooth = "smooth", "Smooth", "The screen glides up and eases to a stop.";
        Float = "float", "Float", "The screen drifts more slowly, easing in and out.";
        Spring = "spring", "Spring", "The screen springs into place, overshooting a little.";
    }
}

animation_kind! {
    /// A mark on each line of fresh output, so it stands out as it arrives.
    NewLines, default Glow, {
        Off = "off", "Off", "New lines are not marked.";
        Glow = "glow", "Glow", "New lines get a faint glow in the cursor colour that fades away.";
        Flash = "flash", "Flash", "New lines flash briefly.";
        Marker = "marker", "Marker", "A bar in the margin marks new lines, then fades.";
        Underline = "underline", "Underline", "A line sweeps under new text, then fades.";
        Shimmer = "shimmer", "Shimmer", "A band of light passes across new lines.";
    }
}

pub const ANIMATION_SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.5..=2.0;

/// Terminal animations. Off unless turned on; each kind can then be turned
/// off on its own or given another style.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Animations {
    #[serde(deserialize_with = "lenient")]
    pub enabled: bool,
    /// Pace of every animation: 2.0 plays them twice as fast.
    #[serde(deserialize_with = "lenient")]
    pub speed: f32,
    // Typing.
    #[serde(deserialize_with = "lenient")]
    pub cursor_motion: CursorMotion,
    #[serde(deserialize_with = "lenient")]
    pub cursor_blink: CursorBlink,
    #[serde(deserialize_with = "lenient")]
    pub typed_text: TypedText,
    #[serde(deserialize_with = "lenient")]
    pub keystroke_burst: KeystrokeBurst,
    #[serde(deserialize_with = "lenient")]
    pub shake: Shake,
    // Output.
    #[serde(deserialize_with = "lenient")]
    pub new_text: NewText,
    #[serde(deserialize_with = "lenient")]
    pub reveal: Reveal,
    #[serde(deserialize_with = "lenient")]
    pub scrolling: Scrolling,
    #[serde(deserialize_with = "lenient")]
    pub new_lines: NewLines,
}

impl Default for Animations {
    fn default() -> Self {
        Animations {
            enabled: false,
            speed: 1.0,
            cursor_motion: CursorMotion::default(),
            cursor_blink: CursorBlink::default(),
            typed_text: TypedText::default(),
            keystroke_burst: KeystrokeBurst::default(),
            shake: Shake::default(),
            new_text: NewText::default(),
            reveal: Reveal::default(),
            scrolling: Scrolling::default(),
            new_lines: NewLines::default(),
        }
    }
}

impl Animations {
    /// The speed, kept within the range Preferences offers. A hand-edited
    /// or unreadable value plays at normal speed.
    pub fn pace(&self) -> f32 {
        if self.speed.is_finite() && self.speed > 0.0 {
            self.speed.clamp(*ANIMATION_SPEED_RANGE.start(), *ANIMATION_SPEED_RANGE.end())
        } else {
            1.0
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
        settings.animations.speed = settings.animations.pace();
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
        // Written before update checks existed: they're on.
        assert!(settings.check_for_updates);
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
    fn animations_are_off_until_turned_on() {
        let settings = load(r#"{"theme": "Monokai"}"#);
        assert!(!settings.animations.enabled);
        assert_eq!(settings.animations, Animations::default());
        assert_eq!(settings.animations.speed, 1.0);
    }

    #[test]
    fn automatic_paging_defaults_off_and_round_trips() {
        for json in ["{}", r#"{"auto_paging": "yes"}"#, r#"{"auto_paging": null}"#] {
            assert!(!load(json).auto_paging);
        }
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(dir.path().join("settings.json"));
        let mut settings = AppSettings { auto_paging: true, ..AppSettings::default() };
        store.save(&settings).unwrap();
        assert!(store.load().auto_paging);
        settings.auto_paging = false;
        store.save(&settings).unwrap();
        assert!(!store.load().auto_paging);
    }

    #[test]
    fn animation_choices_round_trip_and_bad_ones_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(dir.path().join("settings.json"));
        let settings = AppSettings {
            animations: Animations {
                enabled: true,
                speed: 1.5,
                cursor_motion: CursorMotion::Smear,
                reveal: Reveal::Typewriter,
                new_lines: NewLines::Off,
                ..Animations::default()
            },
            ..AppSettings::default()
        };
        store.save(&settings).unwrap();
        let text = fs::read_to_string(&store.path).unwrap();
        assert!(text.contains(r#""cursor_motion": "smear""#), "{text}");
        assert_eq!(store.load(), settings);

        let settings = load(
            r#"{"animations": {"enabled": true, "speed": "fast", "cursor_motion": "warp",
                "keystroke_burst": "confetti", "scrolling": 7}}"#,
        );
        let animations = settings.animations;
        assert!(animations.enabled);
        assert_eq!(animations.speed, 1.0);
        assert_eq!(animations.cursor_motion, CursorMotion::default());
        assert_eq!(animations.keystroke_burst, KeystrokeBurst::Confetti);
        assert_eq!(animations.scrolling, Scrolling::default());

        let animations = load(r#"{"animations": "yes please"}"#).animations;
        assert_eq!(animations, Animations::default());
    }

    #[test]
    fn animation_speed_stays_in_range() {
        for (stored, expected) in [(0.0, 1.0), (-3.0, 1.0), (0.1, 0.5), (9.0, 2.0), (1.25, 1.25)] {
            let animations = Animations { speed: stored, ..Animations::default() };
            assert_eq!(animations.pace(), expected, "stored {stored}");
        }
        assert_eq!(Animations { speed: f32::NAN, ..Animations::default() }.pace(), 1.0);
        assert_eq!(load(r#"{"animations": {"speed": 40}}"#).animations.speed, 2.0);
    }

    #[test]
    fn every_animation_kind_lists_its_default() {
        assert!(CursorMotion::ALL.contains(&CursorMotion::default()));
        assert!(CursorBlink::ALL.contains(&CursorBlink::default()));
        assert!(TypedText::ALL.contains(&TypedText::default()));
        assert!(KeystrokeBurst::ALL.contains(&KeystrokeBurst::default()));
        assert!(Shake::ALL.contains(&Shake::default()));
        assert!(NewText::ALL.contains(&NewText::default()));
        assert!(Reveal::ALL.contains(&Reveal::default()));
        assert!(Scrolling::ALL.contains(&Scrolling::default()));
        assert!(NewLines::ALL.contains(&NewLines::default()));
        assert_eq!(Reveal::default(), Reveal::Instant);
        assert_eq!(Shake::default(), Shake::Off);
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
