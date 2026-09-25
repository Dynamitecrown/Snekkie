//! Snekkie: a tabbed SSH and serial terminal.

pub mod config;
pub mod profiles;
pub mod session;
pub mod settings;
pub mod terminal;
pub mod transport;
pub mod ui;

/// Name of the mutex a running Snekkie holds on Windows. The installer
/// checks for it so it can ask you to close Snekkie before upgrading.
pub const APP_MUTEX: &str = "Snekkie.Running";
