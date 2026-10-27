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
    write_replacing(path, contents, false)
}

/// As [`write_atomic`], but readable only by the user on Unix; the Windows
/// profile folder already is.
pub fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    write_replacing(path, contents, true)
}

/// Each save gets its own hidden temporary file beside the target, so two
/// copies of Snekkie saving at once can't mix or delete each other's file
/// (the last rename wins whole), and saving `lab.json` never touches a
/// `lab.tmp` that belongs to the user.
fn write_replacing(path: &Path, contents: &str, private: bool) -> io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let tmp = path.with_file_name(tmp_name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let result = options
        .open(&tmp)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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

    /// Two copies of Snekkie saving at once: every save succeeds, the file
    /// is always one whole save or the other, and no temporary file is left.
    #[test]
    fn concurrent_saves_never_mix_or_fail() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sessions.json");
        // Large enough that a write takes several system calls.
        let contents = ["a", "b"].map(|letter| letter.repeat(256 * 1024));
        write_atomic(&path, &contents[0]).unwrap();
        let done = std::sync::atomic::AtomicBool::new(false);
        let (failures, torn) = std::thread::scope(|scope| {
            let writers: Vec<_> = contents
                .iter()
                .map(|text| {
                    scope.spawn(|| (0..100).filter_map(|_| write_atomic(&path, text).err()).collect::<Vec<_>>())
                })
                .collect();
            let reader = scope.spawn(|| {
                let mut torn = 0;
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    // A read can lose the race with a rename on Windows; that's
                    // not a torn file, so only check what was read.
                    if fs::read_to_string(&path).is_ok_and(|text| !contents.contains(&text)) {
                        torn += 1;
                    }
                }
                torn
            });
            let failures: Vec<_> = writers.into_iter().flat_map(|writer| writer.join().unwrap()).collect();
            done.store(true, std::sync::atomic::Ordering::Relaxed);
            (failures, reader.join().unwrap())
        });
        assert!(failures.is_empty(), "{} saves failed, first: {:?}", failures.len(), failures.first());
        assert_eq!(torn, 0, "read a mixed file {torn} times");
        assert!(contents.contains(&fs::read_to_string(&path).unwrap()));
        let left: Vec<_> = fs::read_dir(root.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(left, ["sessions.json"], "temporary files were left behind");
    }

    #[test]
    fn saving_never_touches_a_neighboring_tmp_file() {
        let root = tempfile::tempdir().unwrap();
        let mine = root.path().join("lab.tmp");
        fs::write(&mine, "the user's own file").unwrap();
        write_atomic(&root.path().join("lab.json"), "{}").unwrap();
        assert_eq!(fs::read_to_string(&mine).unwrap(), "the user's own file");
    }
}
