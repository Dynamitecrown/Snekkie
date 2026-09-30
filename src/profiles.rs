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
use crate::terminal::keys::Backspace;

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
    Telnet,
    Raw,
    Serial,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Ssh, Kind::Telnet, Kind::Raw, Kind::Serial];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Ssh => "ssh",
            Kind::Telnet => "telnet",
            Kind::Raw => "raw",
            Kind::Serial => "serial",
        }
    }

    pub fn parse(s: &str) -> Kind {
        match s {
            "telnet" => Kind::Telnet,
            "raw" => Kind::Raw,
            "serial" => Kind::Serial,
            _ => Kind::Ssh,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Ssh => "SSH",
            Kind::Telnet => "Telnet",
            Kind::Raw => "Raw TCP",
            Kind::Serial => "Serial",
        }
    }

    /// The well-known port. Raw TCP has none: it's whatever port the
    /// console server maps to the line you want.
    pub fn default_port(self) -> Option<u16> {
        match self {
            Kind::Ssh => Some(22),
            Kind::Telnet => Some(23),
            Kind::Raw | Kind::Serial => None,
        }
    }

    /// Connects over the network, so has a host and port.
    pub fn is_network(self) -> bool {
        self != Kind::Serial
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

/// Whether Snekkie draws what you type itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalEcho {
    /// Telnet: until the device says it will echo. Everything else: off,
    /// since shells, network gear and console lines echo for themselves.
    Auto,
    On,
    Off,
}

impl LocalEcho {
    pub const ALL: [LocalEcho; 3] = [LocalEcho::Auto, LocalEcho::On, LocalEcho::Off];

    pub fn as_str(self) -> &'static str {
        match self {
            LocalEcho::Auto => "auto",
            LocalEcho::On => "on",
            LocalEcho::Off => "off",
        }
    }

    pub fn parse(s: &str) -> LocalEcho {
        match s {
            "on" => LocalEcho::On,
            "off" => LocalEcho::Off,
            _ => LocalEcho::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LocalEcho::Auto => "Auto",
            LocalEcho::On => "On",
            LocalEcho::Off => "Off",
        }
    }

    /// Whether to echo locally, given the session type and whether a
    /// telnet server has agreed to echo.
    pub fn applies(self, kind: Kind, remote_echo: bool) -> bool {
        match self {
            LocalEcho::On => true,
            LocalEcho::Off => false,
            LocalEcho::Auto => kind == Kind::Telnet && !remote_echo,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    /// "ssh", "telnet", "raw" or "serial". Kept as a string so the file
    /// stays readable by the Python releases; see [`Profile::kind`].
    #[serde(deserialize_with = "lenient")]
    pub kind: String,

    // SSH, telnet and raw TCP
    #[serde(deserialize_with = "lenient")]
    pub host: String,
    #[serde(deserialize_with = "lenient")]
    pub port: u16,
    /// Seconds between keepalives, so a firewall doesn't drop an idle
    /// session. 0 = off.
    #[serde(deserialize_with = "lenient")]
    pub keepalive: u32,

    // SSH
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
    /// "auto", "on" or "off"; see [`LocalEcho`].
    #[serde(deserialize_with = "lenient")]
    pub local_echo: String,
    /// "del" or "ctrl-h"; see [`Backspace`].
    #[serde(deserialize_with = "lenient")]
    pub backspace: String,
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
            keepalive: 60,
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
            local_echo: "auto".into(),
            backspace: "del".into(),
            device_syntax: "none".into(),
        }
    }
}

impl Profile {
    pub fn kind(&self) -> Kind {
        Kind::parse(&self.kind)
    }

    pub fn set_kind(&mut self, kind: Kind) {
        self.kind = kind.as_str().into();
    }

    pub fn auth(&self) -> Auth {
        Auth::parse(&self.auth)
    }

    pub fn local_echo(&self) -> LocalEcho {
        LocalEcho::parse(&self.local_echo)
    }

    pub fn backspace(&self) -> Backspace {
        Backspace::parse(&self.backspace)
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
        if self.local_echo.is_empty() {
            self.local_echo = defaults.local_echo;
        }
        if self.backspace.is_empty() {
            self.backspace = defaults.backspace;
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
    fn older_sessions_get_keepalives_and_new_kinds_round_trip() {
        let (_dir, mut store) = store_with(PYTHON_SESSIONS);
        assert_eq!(store.get("core-switch").unwrap().keepalive, 60);

        for kind in Kind::ALL {
            let name = kind.as_str();
            store.put(Profile { name: name.into(), kind: name.into(), keepalive: 0, ..Profile::default() }).unwrap();
        }
        let reloaded = ProfileStore::open(store.path.clone());
        for kind in Kind::ALL {
            let profile = reloaded.get(kind.as_str()).unwrap();
            assert_eq!(profile.kind(), kind);
            assert_eq!(profile.keepalive, 0);
        }
    }

    #[test]
    fn older_sessions_get_auto_echo_and_del() {
        let (_dir, store) = store_with(PYTHON_SESSIONS);
        let profile = store.get("console").unwrap();
        assert_eq!(profile.local_echo(), LocalEcho::Auto);
        assert_eq!(profile.backspace(), Backspace::Del);
    }

    #[test]
    fn auto_echo_is_only_for_telnet_servers_that_do_not_echo() {
        for kind in Kind::ALL {
            assert!(LocalEcho::On.applies(kind, true));
            assert!(!LocalEcho::Off.applies(kind, false));
            assert_eq!(LocalEcho::Auto.applies(kind, false), kind == Kind::Telnet, "{kind:?}");
            assert!(!LocalEcho::Auto.applies(kind, true));
        }
        for option in LocalEcho::ALL {
            assert_eq!(LocalEcho::parse(option.as_str()), option);
        }
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
            "keepalive",
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
            "local_echo",
            "backspace",
            "device_syntax",
        ] {
            assert!(session.get(key).is_some(), "missing {key}");
        }
    }
}
