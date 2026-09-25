//! Terminal fonts: the platform's monospace fonts, loaded on demand.
//!
//! egui draws with fonts it has been handed the bytes of, so installed
//! fonts are found with fontdb and registered as a named family the first
//! time a session asks for them. The system scan runs in the background at
//! startup; until it finishes, terminals draw with egui's built-in
//! monospace font.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use egui::{FontData, FontDefinitions, FontFamily, FontId};

/// Tried in order when a session doesn't name a font.
#[cfg(windows)]
const PREFERRED: [&str; 3] = ["Cascadia Mono", "Consolas", "Lucida Console"];
#[cfg(not(windows))]
const PREFERRED: [&str; 4] = ["DejaVu Sans Mono", "Liberation Mono", "Ubuntu Mono", "Noto Sans Mono"];

/// Qt measured the old app's font sizes in points; egui works in logical
/// pixels. 11 pt at 96 DPI is 14.67 px, so this keeps sizes looking the same.
pub fn points_to_pixels(points: u32) -> f32 {
    points.clamp(4, 96) as f32 * 96.0 / 72.0
}

pub struct Fonts {
    defs: FontDefinitions,
    db: Option<Arc<fontdb::Database>>,
    pending: Option<Receiver<fontdb::Database>>,
    /// Families whose bytes are in `defs`, by name.
    loaded: HashSet<String>,
    /// Families egui has actually applied. `set_fonts` only takes effect
    /// from the next frame, and drawing with a family egui doesn't know yet
    /// panics, so a family is only handed out once it's in here.
    active: HashSet<String>,
    /// Loaded this frame; become active next frame.
    staged: Vec<String>,
    /// Families asked for that the system doesn't have.
    missing: HashSet<String>,
    /// Monospace families installed, for the font pickers.
    pub monospace: Vec<String>,
    default_family: Option<String>,
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

impl Fonts {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("font scan".into())
            .spawn(move || {
                let mut db = fontdb::Database::new();
                db.load_system_fonts();
                let _ = tx.send(db);
            })
            .ok();
        Fonts {
            defs: FontDefinitions::default(),
            db: None,
            pending: Some(rx),
            loaded: HashSet::new(),
            active: HashSet::new(),
            staged: Vec::new(),
            missing: HashSet::new(),
            monospace: Vec::new(),
            default_family: None,
        }
    }

    /// Call at the start of every frame. Activates fonts staged last frame
    /// and picks up the finished system scan. Returns true when fonts
    /// changed.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let activated = !self.staged.is_empty();
        self.active.extend(self.staged.drain(..));
        let Some(rx) = &self.pending else { return activated };
        let Ok(db) = rx.try_recv() else { return false };
        self.pending = None;
        let mut families = BTreeSet::new();
        for face in db.faces() {
            if face.monospaced
                && let Some((name, _)) = face.families.first()
            {
                families.insert(name.clone());
            }
        }
        self.monospace = families.into_iter().collect();
        self.db = Some(Arc::new(db));
        self.default_family = PREFERRED.iter().find(|name| self.register(name)).map(|s| s.to_string());
        if let Some(name) = self.default_family.clone() {
            self.stage(ctx, name);
        }
        true
    }

    fn stage(&mut self, ctx: &egui::Context, family: String) {
        if !self.active.contains(&family) && !self.staged.contains(&family) {
            self.staged.push(family);
            ctx.set_fonts(self.defs.clone());
            ctx.request_repaint();
        }
    }

    /// Load one family's regular and bold faces into the definitions.
    /// Returns false if the system doesn't have it.
    fn register(&mut self, family: &str) -> bool {
        if self.loaded.contains(family) {
            return true;
        }
        if self.missing.contains(family) {
            return false;
        }
        let Some(db) = self.db.clone() else { return false };
        let load = |weight: fontdb::Weight| -> Option<FontData> {
            let id = db.query(&fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                weight,
                stretch: fontdb::Stretch::Normal,
                style: fontdb::Style::Normal,
            })?;
            db.with_face_data(id, |data, index| {
                let mut font = FontData::from_owned(data.to_vec());
                font.index = index;
                font
            })
        };
        let Some(regular) = load(fontdb::Weight::NORMAL) else {
            self.missing.insert(family.to_string());
            return false;
        };
        let bold = load(fontdb::Weight::BOLD);

        let fallbacks = self.defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
        self.defs.font_data.insert(family.to_string(), Arc::new(regular));
        let mut chain = vec![family.to_string()];
        chain.extend(fallbacks.iter().cloned());
        self.defs.families.insert(FontFamily::Name(family.into()), chain);

        let bold_name = bold_key(family);
        let mut bold_chain = Vec::new();
        if let Some(bold) = bold {
            self.defs.font_data.insert(bold_name.clone(), Arc::new(bold));
            bold_chain.push(bold_name.clone());
        } else {
            bold_chain.push(family.to_string());
        }
        bold_chain.extend(fallbacks);
        self.defs.families.insert(FontFamily::Name(bold_name.into()), bold_chain);

        self.loaded.insert(family.to_string());
        true
    }

    /// Resolve a session's font setting to the family to draw with,
    /// registering it with egui if needed.
    pub fn family(&mut self, ctx: &egui::Context, requested: &str) -> Option<String> {
        let requested = requested.trim();
        if !requested.is_empty() {
            if self.active.contains(requested) {
                return Some(requested.to_string());
            }
            if self.register(requested) {
                // Usable from the next frame; draw with the default until then.
                self.stage(ctx, requested.to_string());
            }
        }
        self.default_family.clone().filter(|name| self.active.contains(name))
    }

    /// Regular and bold font ids for a family (None = built-in monospace).
    pub fn font_ids(family: Option<&str>, size: f32) -> (FontId, FontId) {
        match family {
            Some(name) => (
                FontId::new(size, FontFamily::Name(name.into())),
                FontId::new(size, FontFamily::Name(bold_key(name).into())),
            ),
            None => (FontId::monospace(size), FontId::monospace(size)),
        }
    }
}

fn bold_key(family: &str) -> String {
    format!("{family} (bold)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eleven_points_is_the_old_default_size() {
        assert!((points_to_pixels(11) - 14.666).abs() < 0.01);
        assert_eq!(points_to_pixels(0), points_to_pixels(4));
    }

    #[test]
    fn built_in_font_ids() {
        let (regular, bold) = Fonts::font_ids(None, 12.0);
        assert_eq!(regular, FontId::monospace(12.0));
        assert_eq!(bold, FontId::monospace(12.0));
        let (regular, bold) = Fonts::font_ids(Some("Consolas"), 12.0);
        assert_eq!(regular.family, FontFamily::Name("Consolas".into()));
        assert_eq!(bold.family, FontFamily::Name("Consolas (bold)".into()));
    }
}
