//! Saved sessions.
//!
//! Profiles are plain JSON in the user's config directory, in exactly the
//! format the Python releases wrote, so an existing sessions.json loads
//! unchanged. Passwords are deliberately *not* stored.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::config;

/// Deserialize a field, falling back to its default if the value has the
/// wrong type. sessions.json is a text file people edit by hand, and one bad
/// value shouldn't make every saved session disappear.
pub fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ssh,
    Serial,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Ssh => "ssh",
            Kind::Serial => "serial",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Ssh => "SSH",
            Kind::Serial => "Serial",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    Password,
    Key,
    Agent,
}

impl Auth {
    pub const ALL: [Auth; 3] = [Auth::Password, Auth::Key, Auth::Agent];

    pub fn as_str(self) -> &'static str {
        match self {
            Auth::Password => "password",
            Auth::Key => "key",
            Auth::Agent => "agent",
        }
    }

    pub fn parse(s: &str) -> Auth {
        match s {
            "key" => Auth::Key,
            "agent" => Auth::Agent,
            _ => Auth::Password,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Auth::Password => "Password",
            Auth::Key => "Private key file",
            Auth::Agent => "SSH agent / default keys",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    /// "ssh" or "serial". Kept as a string so the file stays readable by the
    /// Python releases; see [`Profile::kind`].
    #[serde(deserialize_with = "lenient")]
    pub kind: String,

    // SSH
    #[serde(deserialize_with = "lenient")]
    pub host: String,
    #[serde(deserialize_with = "lenient")]
    pub port: u16,
    #[serde(deserialize_with = "lenient")]
    pub username: String,
    /// "password", "key" or "agent".
    #[serde(deserialize_with = "lenient")]
    pub auth: String,
    #[serde(deserialize_with = "lenient")]
    pub key_file: String,

    // Serial
    #[serde(deserialize_with = "lenient")]
    pub device: String,
    #[serde(deserialize_with = "lenient")]
    pub baud: u32,
    #[serde(deserialize_with = "lenient")]
    pub bytesize: u8,
    #[serde(deserialize_with = "lenient")]
    pub parity: String,
    #[serde(deserialize_with = "lenient")]
    pub stopbits: f32,
    #[serde(deserialize_with = "lenient")]
    pub rtscts: bool,
    #[serde(deserialize_with = "lenient")]
    pub xonxoff: bool,

    // Terminal
    #[serde(deserialize_with = "lenient")]
    pub scrollback: u32,
    /// Empty = the platform's default monospace font.
    #[serde(deserialize_with = "lenient")]
    pub font_family: String,
    #[serde(deserialize_with = "lenient")]
    pub font_size: u32,
    /// Empty = no logging.
    #[serde(deserialize_with = "lenient")]
    pub log_path: String,
    /// See `highlight::SYNTAX_LABELS`.
    #[serde(deserialize_with = "lenient")]
    pub device_syntax: String,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: "New session".into(),
            kind: "ssh".into(),
            host: String::new(),
            port: 22,
            username: String::new(),
            auth: "password".into(),
            key_file: String::new(),
            device: String::new(),
            baud: 9600,
            bytesize: 8,
            parity: "None".into(),
            stopbits: 1.0,
            rtscts: false,
            xonxoff: false,
            scrollback: 5000,
            font_family: String::new(),
            font_size: 11,
            log_path: String::new(),
            device_syntax: "none".into(),
        }
    }
}

impl Profile {
    pub fn kind(&self) -> Kind {
        if self.kind == "serial" { Kind::Serial } else { Kind::Ssh }
    }

    pub fn set_kind(&mut self, kind: Kind) {
        self.kind = kind.as_str().into();
    }

    pub fn auth(&self) -> Auth {
        Auth::parse(&self.auth)
    }

    /// Replace values a hand-edited file could have made unusable.
    fn sanitize(mut self) -> Self {
        let defaults = Profile::default();
        if self.port == 0 {
            self.port = defaults.port;
        }
        if self.baud == 0 {
            self.baud = defaults.baud;
        }
        if !(5..=8).contains(&self.bytesize) {
            self.bytesize = defaults.bytesize;
        }
        if ![1.0, 1.5, 2.0].contains(&self.stopbits) {
            self.stopbits = defaults.stopbits;
        }
        if self.font_size == 0 {
            self.font_size = defaults.font_size;
        }
        if self.kind.is_empty() {
            self.kind = defaults.kind;
        }
        if self.auth.is_empty() {
            self.auth = defaults.auth;
        }
        if self.parity.is_empty() {
            self.parity = defaults.parity;
        }
        if self.device_syntax.is_empty() {
            self.device_syntax = defaults.device_syntax;
        }
        self
    }
}

#[derive(Serialize, Deserialize)]
struct SessionsFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    sessions: Vec<serde_json::Value>,
}

/// Ordered collection of profiles, persisted to sessions.json.
pub struct ProfileStore {
    pub path: PathBuf,
    pub profiles: Vec<Profile>,
}

impl ProfileStore {
    pub fn open(path: PathBuf) -> Self {
        let mut store = ProfileStore { path, profiles: Vec::new() };
        store.load();
        store
    }

    pub fn default_path() -> PathBuf {
        config::config_dir().join("sessions.json")
    }

    pub fn load(&mut self) {
        self.profiles = read_profiles(&self.path);
    }

    pub fn save(&self) -> std::io::Result<()> {
        let payload = serde_json::json!({
            "version": 1,
            "sessions": self.profiles,
        });
        let text = serde_json::to_string_pretty(&payload).map_err(std::io::Error::other)?;
        config::write_atomic(&self.path, &text)
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Insert or overwrite by name, then persist.
    pub fn put(&mut self, profile: Profile) -> std::io::Result<()> {
        if let Some(existing) = self.profiles.iter_mut().find(|p| p.name == profile.name) {
            *existing = profile;
        } else {
            self.profiles.push(profile);
            self.profiles.sort_by_key(|p| p.name.to_lowercase());
        }
        self.save()
    }

    pub fn remove(&mut self, name: &str) -> std::io::Result<()> {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        if self.profiles.len() != before { self.save() } else { Ok(()) }
    }

    pub fn names(&self) -> Vec<String> {
        self.profiles.iter().map(|p| p.name.clone()).collect()
    }
}

fn read_profiles(path: &Path) -> Vec<Profile> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_str::<SessionsFile>(&text) else {
        return Vec::new();
    };
    file.sessions
        .into_iter()
        .filter_map(|value| serde_json::from_value::<Profile>(value).ok())
        .map(Profile::sanitize)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written by the Python release (json.dumps(indent=2) of the dataclass).
    const PYTHON_SESSIONS: &str = r#"{
  "version": 1,
  "sessions": [
    {
      "name": "core-switch",
      "kind": "ssh",
      "host": "10.0.0.1",
      "port": 2222,
      "username": "admin",
      "auth": "key",
      "key_file": "C:\\Users\\me\\.ssh\\id_ed25519",
      "device": "",
      "baud": 9600,
      "bytesize": 8,
      "parity": "None",
      "stopbits": 1,
      "rtscts": false,
      "xonxoff": false,
      "scrollback": 5000,
      "font_family": "",
      "font_size": 11,
      "log_path": "",
      "device_syntax": "cisco_ios"
    },
    {
      "name": "console",
      "kind": "serial",
      "device": "COM4",
      "baud": 115200,
      "parity": "Even",
      "stopbits": 1.5,
      "some_future_field": true
    }
  ]
}"#;

    fn store_with(text: &str) -> (tempfile::TempDir, ProfileStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        fs::write(&path, text).unwrap();
        let store = ProfileStore::open(path);
        (dir, store)
    }

    #[test]
    fn loads_sessions_written_by_the_python_release() {
        let (_dir, store) = store_with(PYTHON_SESSIONS);
        assert_eq!(store.names(), ["core-switch", "console"]);

        let ssh = store.get("core-switch").unwrap();
        assert_eq!(ssh.kind(), Kind::Ssh);
        assert_eq!(ssh.port, 2222);
        assert_eq!(ssh.auth(), Auth::Key);
        assert_eq!(ssh.device_syntax, "cisco_ios");

        let serial = store.get("console").unwrap();
        assert_eq!(serial.kind(), Kind::Serial);
        assert_eq!(serial.device, "COM4");
        assert_eq!(serial.baud, 115200);
        assert_eq!(serial.stopbits, 1.5);
        // Missing fields fall back to the defaults.
        assert_eq!(serial.font_size, 11);
        assert_eq!(serial.bytesize, 8);
    }

    #[test]
    fn a_bad_value_does_not_lose_the_session() {
        let (_dir, store) =
            store_with(r#"{"version": 1, "sessions": [{"name": "lab", "port": "twenty-two", "baud": null}]}"#);
        let lab = store.get("lab").unwrap();
        assert_eq!(lab.port, 22);
        assert_eq!(lab.baud, 9600);
    }

    #[test]
    fn garbage_file_loads_as_empty() {
        let (_dir, store) = store_with("{ not json");
        assert!(store.profiles.is_empty());
    }

    #[test]
    fn put_sorts_new_names_and_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProfileStore::open(dir.path().join("sessions.json"));
        for name in ["beta", "Alpha", "gamma"] {
            store.put(Profile { name: name.into(), ..Profile::default() }).unwrap();
        }
        assert_eq!(store.names(), ["Alpha", "beta", "gamma"]);

        store.put(Profile { name: "beta".into(), host: "new".into(), ..Profile::default() }).unwrap();
        assert_eq!(store.names(), ["Alpha", "beta", "gamma"]);
        assert_eq!(store.get("beta").unwrap().host, "new");

        store.remove("Alpha").unwrap();
        let reloaded = ProfileStore::open(store.path.clone());
        assert_eq!(reloaded.names(), ["beta", "gamma"]);
    }

    #[test]
    fn saved_file_keeps_the_python_layout() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProfileStore::open(dir.path().join("sessions.json"));
        store.put(Profile { name: "lab".into(), ..Profile::default() }).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&fs::read_to_string(&store.path).unwrap()).unwrap();
        assert_eq!(raw["version"], 1);
        let session = &raw["sessions"][0];
        for key in [
            "name",
            "kind",
            "host",
            "port",
            "username",
            "auth",
            "key_file",
            "device",
            "baud",
            "bytesize",
            "parity",
            "stopbits",
            "rtscts",
            "xonxoff",
            "scrollback",
            "font_family",
            "font_size",
            "log_path",
            "device_syntax",
        ] {
            assert!(session.get(key).is_some(), "missing {key}");
        }
    }
}
