//! Snekkie: a tabbed SSH and serial terminal.

pub mod config;
pub mod logging;
pub mod network;
pub mod paste;
pub mod profiles;
pub mod putty;
pub mod session;
pub mod settings;
pub mod snippets;
pub mod terminal;
pub mod transport;
pub mod ui;
pub mod update;

/// Name of the mutex a running Snekkie holds on Windows. The installer
/// checks for it so it can ask you to close Snekkie before upgrading.
pub const APP_MUTEX: &str = "Snekkie.Running";
