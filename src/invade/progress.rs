//! Invade's local records in `invade.json`, beside settings.json: unlocked
//! content, milestones, the next run's rig, best results and the ten best
//! runs. Never a live arena or a resumable run.
//!
//! A file that can't be read, isn't valid, or comes from a newer version
//! is left exactly as it is; the game then plays from memory and says so.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::content::{Rig, Unlock};
use super::run::Summary;
use crate::config;

pub const VERSION: u32 = 1;
pub const MAX_RECORDS: usize = 10;
const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Best {
    #[serde(default)]
    pub wave: u32,
    #[serde(default)]
    pub score: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub rig: String,
    #[serde(default)]
    pub score: u64,
    #[serde(default)]
    pub wave: u32,
    /// Active play, in seconds.
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub ending: String,
}

/// Everything that persists. Identifiers are stable strings; ones this
/// version doesn't know are kept so a newer build's data survives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    #[serde(default = "default_rig")]
    pub selected_rig: String,
    #[serde(default)]
    pub unlocked: BTreeSet<String>,
    #[serde(default)]
    pub devices_compromised: u64,
    #[serde(default)]
    pub best: BTreeMap<String, Best>,
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

fn default_rig() -> String {
    Rig::Probe.id().into()
}

impl Default for Progress {
    fn default() -> Self {
        Progress {
            selected_rig: default_rig(),
            unlocked: BTreeSet::new(),
            devices_compromised: 0,
            best: BTreeMap::new(),
            records: Vec::new(),
            other: BTreeMap::new(),
        }
    }
}

#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    #[serde(flatten)]
    progress: &'a Progress,
}

#[derive(Deserialize)]
struct FileIn {
    version: u32,
    #[serde(flatten)]
    progress: Progress,
}

impl Progress {
    pub fn is_unlocked(&self, unlock: Unlock) -> bool {
        self.unlocked.contains(unlock.id())
    }

    pub fn unlocks(&self) -> impl Iterator<Item = Unlock> + '_ {
        Unlock::ALL.into_iter().filter(|u| self.is_unlocked(*u))
    }

    pub fn rig_available(&self, rig: Rig) -> bool {
        rig.unlock().is_none_or(|u| self.is_unlocked(u))
    }

    /// The saved rig if it is still unlocked, otherwise Probe.
    pub fn selected_rig(&self) -> Rig {
        Rig::from_id(&self.selected_rig).filter(|r| self.rig_available(*r)).unwrap_or(Rig::Probe)
    }

    /// Add a finished run: best results per rig and the ten best runs.
    pub fn add_record(&mut self, summary: &Summary) {
        let best = self.best.entry(summary.rig.id().into()).or_default();
        best.wave = best.wave.max(summary.completed_wave);
        best.score = best.score.max(summary.score);
        self.records.push(Record {
            rig: summary.rig.id().into(),
            score: summary.score,
            wave: summary.completed_wave,
            duration: (summary.duration * 10.0).round() / 10.0,
            seed: summary.seed,
            ending: summary.reason.id().into(),
        });
        // Stable: an earlier run keeps its place in a tie.
        self.records.sort_by(|a, b| b.score.cmp(&a.score).then(b.wave.cmp(&a.wave)));
        self.records.truncate(MAX_RECORDS);
    }
}

pub fn parse(bytes: &[u8]) -> Result<Progress, String> {
    let file: FileIn = serde_json::from_slice(bytes).map_err(|e| format!("it is not valid Invade progress ({e})"))?;
    if file.version != VERSION {
        return Err(format!("it was written by a different version of Invade (format {})", file.version));
    }
    Ok(file.progress)
}

pub fn encode(progress: &Progress) -> Result<String, String> {
    serde_json::to_string_pretty(&FileOut { version: VERSION, progress }).map_err(|e| e.to_string())
}

pub struct ProgressStore {
    pub path: PathBuf,
    pub progress: Progress,
    /// Loading failed: play continues from memory and the file is never
    /// written, so nothing in it is lost.
    pub load_error: Option<String>,
    /// The last save failed; the next one tries again.
    pub save_error: Option<String>,
}

impl ProgressStore {
    pub fn open(path: PathBuf) -> ProgressStore {
        let mut store = ProgressStore { path, progress: Progress::default(), load_error: None, save_error: None };
        let result = (|| {
            let file = match fs::File::open(&store.path) {
                Ok(file) => file,
                Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => return Ok(Progress::default()),
                Err(error) => return Err(format!("it could not be read ({error})")),
            };
            let mut bytes = Vec::new();
            file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(|e| format!("it could not be read ({e})"))?;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err("it is larger than 1 MiB".into());
            }
            parse(&bytes)
        })();
        match result {
            Ok(progress) => store.progress = progress,
            Err(error) => store.load_error = Some(error),
        }
        store
    }

    /// Whether changes reach the disk at all.
    pub fn writable(&self) -> bool {
        self.load_error.is_none()
    }

    pub fn save(&mut self) -> Result<(), String> {
        if !self.writable() {
            return Err("Invade progress is read-only this session".into());
        }
        let result =
            encode(&self.progress).and_then(|text| config::write_atomic(&self.path, &text).map_err(|e| e.to_string()));
        self.save_error = result.as_ref().err().cloned();
        result
    }

    /// The one warning the game should show about its records, if any.
    pub fn warning(&self) -> Option<String> {
        if let Some(error) = &self.load_error {
            return Some(format!(
                "Invade progress in {} could not be loaded: {error}. The file has not been changed; \
                 this session's progress will not be saved.",
                self.path.display()
            ));
        }
        self.save_error.as_ref().map(|error| {
            format!(
                "Could not save Invade progress to {}: {error}. Snekkie keeps it and tries again at the next save.",
                self.path.display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invade::run::EndReason;

    fn summary(rig: Rig, score: u64, wave: u32) -> Summary {
        Summary {
            rig,
            score,
            completed_wave: wave,
            duration: 61.234,
            seed: 7,
            reason: EndReason::Defeated,
            compromised: 10,
            unlocks: Vec::new(),
        }
    }

    #[test]
    fn missing_file_starts_fresh_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invade.json");
        let mut store = ProgressStore::open(path.clone());
        assert!(store.writable() && store.warning().is_none());
        assert_eq!(store.progress, Progress::default());
        assert_eq!(store.progress.selected_rig(), Rig::Probe);
        assert!(!path.exists(), "opening never creates the file");
        store.progress.unlocked.insert(Unlock::Ghost.id().into());
        store.progress.selected_rig = Rig::Ghost.id().into();
        store.progress.devices_compromised = 42;
        store.progress.add_record(&summary(Rig::Ghost, 900, 4));
        store.save().unwrap();
        let reopened = ProgressStore::open(path.clone());
        assert_eq!(reopened.progress, store.progress);
        assert_eq!(reopened.progress.selected_rig(), Rig::Ghost);
        assert_eq!(reopened.progress.records[0].duration, 61.2);
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("\"version\": 1"));
        assert!(!text.contains("projectile") && !text.contains("enemies"), "no live arena is stored");
    }

    #[test]
    fn old_partial_files_get_defaults_and_unknown_data_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invade.json");
        std::fs::write(&path, r#"{"version":1,"unlocked":["ghost","future_rig"],"future_field":{"x":1}}"#).unwrap();
        let mut store = ProgressStore::open(path.clone());
        assert!(store.writable());
        assert_eq!(store.progress.selected_rig(), Rig::Probe);
        assert!(store.progress.is_unlocked(Unlock::Ghost));
        assert_eq!(store.progress.unlocks().collect::<Vec<_>>(), [Unlock::Ghost]);
        store.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("future_rig") && text.contains("future_field"));
        // A locked or unknown saved rig falls back to Probe.
        store.progress.selected_rig = "bastion".into();
        assert_eq!(store.progress.selected_rig(), Rig::Probe);
        store.progress.selected_rig = "warp".into();
        assert_eq!(store.progress.selected_rig(), Rig::Probe);
    }

    #[test]
    fn corrupt_unreadable_and_future_files_are_preserved() {
        for contents in
            [&b"{not json"[..], br#"{"version":2,"unlocked":["ghost"]}"#, br#"{"unlocked":[]}"#, &[0xff, 0xfe]]
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("invade.json");
            std::fs::write(&path, contents).unwrap();
            let mut store = ProgressStore::open(path.clone());
            assert!(!store.writable());
            assert!(store.warning().unwrap().contains("has not been changed"));
            assert_eq!(store.progress, Progress::default(), "plays from memory");
            store.progress.unlocked.insert("ghost".into());
            assert!(store.save().is_err());
            assert_eq!(std::fs::read(&path).unwrap(), contents, "the file is untouched");
        }
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("invade.json");
        std::fs::create_dir(&folder).unwrap();
        let store = ProgressStore::open(folder.clone());
        assert!(!store.writable() && folder.is_dir());
    }

    #[test]
    fn write_failures_are_reported_and_retried() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocked");
        std::fs::write(&blocker, "a file where a folder should be").unwrap();
        let mut store = ProgressStore::open(blocker.join("invade.json"));
        assert!(store.writable());
        store.progress.devices_compromised = 5;
        assert!(store.save().is_err());
        assert!(store.warning().unwrap().starts_with("Could not save Invade progress"));
        assert_eq!(store.progress.devices_compromised, 5, "kept in memory");
        store.path = dir.path().join("invade.json");
        store.save().unwrap();
        assert!(store.warning().is_none());
    }

    #[test]
    fn records_keep_the_ten_best_and_best_results_per_rig() {
        let mut progress = Progress::default();
        for i in 0..15u64 {
            progress.add_record(&summary(if i % 2 == 0 { Rig::Probe } else { Rig::Ghost }, i * 100, i as u32));
        }
        assert_eq!(progress.records.len(), MAX_RECORDS);
        assert_eq!(progress.records[0].score, 1400);
        assert_eq!(progress.records[9].score, 500);
        assert_eq!(progress.best["probe"], Best { wave: 14, score: 1400 });
        assert_eq!(progress.best["ghost"], Best { wave: 13, score: 1300 });
        progress.add_record(&summary(Rig::Probe, 50, 20));
        assert_eq!(progress.best["probe"], Best { wave: 20, score: 1400 }, "wave and score are tracked separately");
        let record = &progress.records[0];
        assert_eq!((record.rig.as_str(), record.seed, record.ending.as_str()), ("probe", 7, "defeated"));
    }
}
