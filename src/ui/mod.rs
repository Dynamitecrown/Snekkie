//! The egui front end.

mod agents;
pub mod animation;
pub mod animation_preview;
pub mod app;
pub mod explorer;
pub mod fonts;
pub mod invade;
pub mod layout;
mod paste;
pub mod preferences;
mod profile_transfer;
mod putty_import;
pub mod render;
pub mod search;
pub mod sidebar;
mod snippets;
mod space;
pub mod style;
mod syntax_preview;
pub mod terminal_view;
mod theme_preview;
#[cfg(windows)]
pub(crate) mod tray;
pub mod updater;

pub use app::{Paths, SnekkieApp};
