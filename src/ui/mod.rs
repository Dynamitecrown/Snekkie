//! The egui front end.

pub mod animation;
pub mod animation_preview;
pub mod app;
pub mod fonts;
mod paste;
pub mod preferences;
mod profile_transfer;
mod putty_import;
pub mod render;
pub mod search;
pub mod sidebar;
mod snippets;
pub mod style;
mod syntax_preview;
pub mod terminal_view;
mod theme_preview;
pub mod updater;

pub use app::{Paths, SnekkieApp};
