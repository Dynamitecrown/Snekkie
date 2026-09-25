//! Where Snekkie keeps its files, and small helpers for writing them safely.
//!
//! The location matches the Python releases (`%APPDATA%\snekkie` on
//! Windows, `~/.config/snekkie` on Linux), so saved sessions and themes
//! carry straight over to this version.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// What the config directory was called before the app was renamed. Kept so
/// an install that has only ever run the old pyterm build still finds its
/// saved sessions.
pub const LEGACY_DIR_NAME: &str = "pyterm";

/// Files carried over from the legacy directory.
const MIGRATED_FILES: [&str; 2] = ["sessions.json", "settings.json"];

pub fn config_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join("AppData").join("Roaming")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("snekkie")
    } else if cfg!(target_os = "macos") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library")
            .join("Application Support")
            .join("snekkie")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("snekkie")
    }
}

/// Copy sessions and settings over from the old pyterm directory.
///
/// Copies rather than moves, and only files that don't exist yet, so an
/// interrupted migration can resume without overwriting newer settings.
pub fn migrate_legacy_config(new: &Path) {
    let Some(old) = new.parent().map(|p| p.join(LEGACY_DIR_NAME)) else {
        return;
    };
    if old == new || !old.is_dir() {
        return;
    }
    if fs::create_dir_all(new).is_err() {
        return;
    }
    for name in MIGRATED_FILES {
        let from = old.join(name);
        let to = new.join(name);
        if from.is_file() && !to.exists() {
            let _ = fs::copy(&from, &to);
        }
    }
}

/// Write via a temporary file and rename, so a crash mid-save can never
/// leave a half-written sessions.json behind.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_copies_only_missing_files() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join(LEGACY_DIR_NAME);
        let new = root.path().join("snekkie");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("sessions.json"), "old sessions").unwrap();
        fs::write(old.join("settings.json"), "old settings").unwrap();
        fs::write(new.join("settings.json"), "newer settings").unwrap();

        migrate_legacy_config(&new);

        assert_eq!(fs::read_to_string(new.join("sessions.json")).unwrap(), "old sessions");
        assert_eq!(fs::read_to_string(new.join("settings.json")).unwrap(), "newer settings");
    }

    #[test]
    fn migration_without_legacy_dir_is_a_no_op() {
        let root = tempfile::tempdir().unwrap();
        let new = root.path().join("snekkie");
        migrate_legacy_config(&new);
        assert!(!new.exists());
    }

    #[test]
    fn atomic_write_replaces_content() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nested").join("file.json");
        write_atomic(&path, "one").unwrap();
        write_atomic(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert!(!path.with_extension("tmp").exists());
    }
}
