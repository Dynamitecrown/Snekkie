//! Saved sessions.
//!
//! Profiles are plain JSON in the user's config directory, in exactly the
//! format the Python releases wrote, so an existing sessions.json loads
//! unchanged. Passwords are deliberately *not* stored.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::terminal::keys::Backspace;
use crate::{config, settings};

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
    #[serde(deserialize_with = "lenient")]
    pub favorite: bool,
    /// Empty = the normal tab appearance; otherwise a saved #rrggbb color.
    #[serde(deserialize_with = "lenient")]
    pub tab_color: String,

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
    /// Older profiles retain raw received-byte logs; new profiles use text.
    #[serde(default = "legacy_log_format", deserialize_with = "lenient_log_format")]
    pub log_format: String,
    #[serde(deserialize_with = "lenient")]
    pub log_passwords: bool,
    /// 0 disables size rotation; defaults preserve old log behavior on load.
    #[serde(default, deserialize_with = "lenient")]
    pub log_rotate_mb: u32,
    #[serde(default, deserialize_with = "lenient")]
    pub log_rotate_daily: bool,
    /// "auto", "on" or "off"; see [`LocalEcho`].
    #[serde(deserialize_with = "lenient")]
    pub local_echo: String,
    /// "del" or "ctrl-h"; see [`Backspace`].
    #[serde(deserialize_with = "lenient")]
    pub backspace: String,
    /// See `highlight::syntax_options`.
    #[serde(deserialize_with = "lenient")]
    pub device_syntax: String,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: "New session".into(),
            kind: "ssh".into(),
            favorite: false,
            tab_color: String::new(),
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
            log_format: "text".into(),
            log_passwords: false,
            log_rotate_mb: 10,
            log_rotate_daily: true,
            local_echo: "auto".into(),
            backspace: "del".into(),
            device_syntax: "none".into(),
        }
    }
}

fn legacy_log_format() -> String {
    "raw".into()
}

fn lenient_log_format<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value.as_str() {
        Some("text") => "text",
        _ => "raw",
    }
    .into())
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
        self.tab_color = settings::parse_hex(&self.tab_color).map(settings::to_hex).unwrap_or_default();
        self
    }
}

/// Supported profiles and notes to review before importing. Reading this data
/// neither changes the saved collection nor opens a connection.
pub struct ProfileImport {
    pub profiles: Vec<Profile>,
    pub warnings: Vec<String>,
}

/// A deliberately explicit export schema. Arbitrary input fields, credentials,
/// key contents and host-key trust can never pass through a profile export.
#[derive(Serialize)]
struct ExportProfile<'a> {
    name: &'a str,
    kind: &'a str,
    favorite: bool,
    tab_color: String,
    host: &'a str,
    port: u16,
    keepalive: u32,
    username: &'a str,
    auth: &'a str,
    key_file: &'a str,
    device: &'a str,
    baud: u32,
    bytesize: u8,
    parity: &'a str,
    stopbits: f32,
    rtscts: bool,
    xonxoff: bool,
    scrollback: u32,
    font_family: &'a str,
    font_size: u32,
    log_path: &'a str,
    log_format: &'a str,
    log_passwords: bool,
    log_rotate_mb: u32,
    log_rotate_daily: bool,
    local_echo: &'a str,
    backspace: &'a str,
    device_syntax: &'a str,
}

impl<'a> From<&'a Profile> for ExportProfile<'a> {
    fn from(profile: &'a Profile) -> Self {
        Self {
            name: &profile.name,
            kind: &profile.kind,
            favorite: profile.favorite,
            tab_color: settings::parse_hex(&profile.tab_color).map(settings::to_hex).unwrap_or_default(),
            host: &profile.host,
            port: profile.port,
            keepalive: profile.keepalive,
            username: &profile.username,
            auth: &profile.auth,
            key_file: &profile.key_file,
            device: &profile.device,
            baud: profile.baud,
            bytesize: profile.bytesize,
            parity: &profile.parity,
            stopbits: profile.stopbits,
            rtscts: profile.rtscts,
            xonxoff: profile.xonxoff,
            scrollback: profile.scrollback,
            font_family: &profile.font_family,
            font_size: profile.font_size,
            log_path: &profile.log_path,
            log_format: &profile.log_format,
            // Transfer never enables secret recording on another installation.
            log_passwords: false,
            log_rotate_mb: profile.log_rotate_mb,
            log_rotate_daily: profile.log_rotate_daily,
            local_echo: &profile.local_echo,
            backspace: &profile.backspace,
            device_syntax: &profile.device_syntax,
        }
    }
}

/// Export selected profiles using the same version-1 sessions envelope as the
/// saved store. A private-key path is only a reference; its file is never read.
pub fn export_profiles(profiles: &[Profile]) -> Result<String, String> {
    let sessions: Vec<_> = profiles.iter().map(ExportProfile::from).collect();
    serde_json::to_string_pretty(&serde_json::json!({ "version": 1, "sessions": sessions }))
        .map_err(|error| format!("Could not encode profiles: {error}"))
}

/// Read a profile export with strict envelope and destination validation.
/// Malformed or unsupported rows become notes instead of silently changing a
/// protocol/destination. Optional fields absent in older exports keep defaults.
pub fn parse_import(bytes: &[u8]) -> Result<ProfileImport, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Profile exports must be no larger than 16 MiB.".into());
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let payload: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| format!("Could not read profile JSON: {error}"))?;
    let object = payload.as_object().ok_or("Expected a version-1 profile export object.")?;
    if object.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err("Unsupported profile export version; expected version 1.".into());
    }
    let sessions = object
        .get("sessions")
        .and_then(serde_json::Value::as_array)
        .ok_or("The profile export must contain a sessions array.")?;
    let mut batch = ProfileImport { profiles: Vec::new(), warnings: Vec::new() };
    for (index, value) in sessions.iter().enumerate() {
        match imported_profile(value) {
            Ok((profile, notes)) => {
                batch.warnings.extend(notes.into_iter().map(|note| format!("{}: {note}", profile.name)));
                batch.profiles.push(profile);
            }
            Err(reason) => batch.warnings.push(format!("Session {} skipped: {reason}", index + 1)),
        }
    }
    if sessions.is_empty() {
        batch.warnings.push("This export contains no saved sessions.".into());
    }
    Ok(batch)
}

fn imported_profile(value: &serde_json::Value) -> Result<(Profile, Vec<String>), String> {
    let fields = value.as_object().ok_or("expected a session object")?;
    let name = fields
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or("missing session name")?;
    let kind = fields.get("kind").and_then(serde_json::Value::as_str).ok_or("missing protocol")?;
    if !Kind::ALL.iter().any(|supported| supported.as_str() == kind) {
        return Err(format!("{name}: unsupported protocol"));
    }
    let string_fields = [
        "name",
        "kind",
        "tab_color",
        "host",
        "username",
        "auth",
        "key_file",
        "device",
        "parity",
        "font_family",
        "log_path",
        "log_format",
        "local_echo",
        "backspace",
        "device_syntax",
    ];
    let integer_fields = ["port", "keepalive", "baud", "bytesize", "scrollback", "font_size", "log_rotate_mb"];
    let boolean_fields = ["favorite", "rtscts", "xonxoff", "log_passwords", "log_rotate_daily"];
    for field in string_fields {
        if fields.get(field).is_some_and(|value| !value.is_string()) {
            return Err(format!("{name}: {field} must be text"));
        }
    }
    for field in integer_fields {
        let maximum = match field {
            "port" => u16::MAX as u64,
            "bytesize" => u8::MAX as u64,
            _ => u32::MAX as u64,
        };
        if fields.get(field).is_some_and(|value| value.as_u64().is_none_or(|number| number > maximum)) {
            return Err(format!("{name}: invalid {field}"));
        }
    }
    for field in boolean_fields {
        if fields.get(field).is_some_and(|value| !value.is_boolean()) {
            return Err(format!("{name}: {field} must be true or false"));
        }
    }
    if fields.get("stopbits").is_some_and(|value| value.as_f64().is_none()) {
        return Err(format!("{name}: invalid stopbits"));
    }
    let mut profile: Profile = serde_json::from_value(value.clone()).map_err(|error| format!("{name}: {error}"))?;
    profile.name = name.into();
    if profile.kind().is_network() {
        profile.host = profile.host.trim().into();
        if profile.host.is_empty() {
            return Err(format!("{name}: missing hostname or IP address"));
        }
        if !fields.contains_key("port") {
            profile.port = profile.kind().default_port().ok_or_else(|| format!("{name}: raw TCP requires a port"))?;
        }
        if profile.port == 0 {
            return Err(format!("{name}: port must be between 1 and 65535"));
        }
    } else {
        profile.device = profile.device.trim().into();
        if profile.device.is_empty() {
            return Err(format!("{name}: missing serial device"));
        }
        if profile.baud == 0
            || !(5..=8).contains(&profile.bytesize)
            || !["None", "Even", "Odd"].contains(&profile.parity.as_str())
            || ![1.0, 2.0].contains(&profile.stopbits)
        {
            return Err(format!("{name}: unsupported serial settings"));
        }
    }
    if !["password", "key", "agent"].contains(&profile.auth.as_str()) {
        return Err(format!("{name}: unsupported SSH authentication method"));
    }
    if profile.font_size == 0
        || !["auto", "on", "off"].contains(&profile.local_echo.as_str())
        || !["del", "ctrl-h"].contains(&profile.backspace.as_str())
    {
        return Err(format!("{name}: unsupported terminal settings"));
    }
    let mut notes = Vec::new();
    if profile.log_passwords {
        profile.log_passwords = false;
        notes.push("password logging disabled on import; enable it explicitly on this installation".into());
    }
    if fields.get("log_format").is_some_and(|value| !matches!(value.as_str(), Some("raw" | "text"))) {
        return Err(format!("{name}: unsupported log format"));
    }
    if !profile.tab_color.is_empty() && settings::parse_hex(&profile.tab_color).is_none() {
        notes.push("invalid tab color cleared".into());
    }
    let unsupported: Vec<_> = fields
        .keys()
        .filter(|field| {
            !string_fields.contains(&field.as_str())
                && !integer_fields.contains(&field.as_str())
                && !boolean_fields.contains(&field.as_str())
                && field.as_str() != "stopbits"
        })
        .collect();
    if !unsupported.is_empty() {
        notes.push(format!(
            "{} unsupported field(s) ignored; only supported profile settings are imported",
            unsupported.len()
        ));
    }
    Ok((profile.sanitize(), notes))
}

/// Keep existing profiles unchanged and give imports unique, case-insensitive
/// names. Order remains stable so a preview can show exactly what will be added.
pub fn merge_import(existing: &[Profile], incoming: impl IntoIterator<Item = Profile>) -> Vec<Profile> {
    let mut merged = existing.to_vec();
    let mut names: HashSet<_> = existing.iter().map(|profile| profile.name.to_lowercase()).collect();
    for mut profile in incoming {
        let original = profile.name.clone();
        let mut suffix = 1;
        while !names.insert(profile.name.to_lowercase()) {
            profile.name = format!("{original} (Imported {suffix})");
            suffix += 1;
        }
        merged.push(profile);
    }
    merged
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
    fn older_sessions_get_no_favorite_or_saved_color() {
        let (_dir, store) = store_with(PYTHON_SESSIONS);
        for profile in store.profiles {
            assert!(!profile.favorite);
            assert!(profile.tab_color.is_empty());
        }
    }

    #[test]
    fn logging_preferences_preserve_old_formats_and_require_explicit_password_opt_in() {
        let (_old_dir, old) = store_with(
            r#"{"version":1,"sessions":[{"name":"old"},{"name":"bad","log_passwords":"yes","log_format":42}]}"#,
        );
        for profile in &old.profiles {
            assert_eq!(profile.log_format, "raw");
            assert!(!profile.log_passwords);
            assert_eq!(profile.log_rotate_mb, 0);
            assert!(!profile.log_rotate_daily);
        }
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProfileStore::open(dir.path().join("sessions.json"));
        let profile =
            Profile { name: "Lab".into(), host: "192.0.2.3".into(), log_passwords: true, ..Default::default() };
        store.put(profile.clone()).unwrap();
        let loaded = ProfileStore::open(store.path.clone());
        assert_eq!(loaded.get("Lab").unwrap(), &profile);
        let export = export_profiles(&[profile]).unwrap();
        assert!(!parse_import(export.as_bytes()).unwrap().profiles[0].log_passwords);
        let imported = parse_import(
            br#"{"version":1,"sessions":[{"name":"Imported","kind":"ssh","host":"192.0.2.3","log_passwords":true}]}"#,
        )
        .unwrap();
        assert!(!imported.profiles[0].log_passwords);
        assert!(imported.warnings.iter().any(|note| note.contains("password logging disabled")));
    }

    #[test]
    fn favorite_and_saved_color_round_trip_and_load_leniently() {
        let (_dir, mut store) = store_with(
            r##"{"version":1,"sessions":[{"name":"bad","favorite":"yes","tab_color":17},{"name":"invalid","tab_color":"#gg0000"},{"name":"normalized","favorite":true,"tab_color":" AABBCC "}]}"##,
        );
        assert!(!store.get("bad").unwrap().favorite);
        assert!(store.get("bad").unwrap().tab_color.is_empty());
        assert!(store.get("invalid").unwrap().tab_color.is_empty());
        assert_eq!(store.get("normalized").unwrap().tab_color, "#aabbcc");
        store
            .put(Profile { name: "favorite".into(), favorite: true, tab_color: "#112233".into(), ..Profile::default() })
            .unwrap();
        let loaded = ProfileStore::open(store.path.clone());
        let favorite = loaded.get("favorite").unwrap();
        assert!(favorite.favorite);
        assert_eq!(favorite.tab_color, "#112233");
    }

    #[test]
    fn profile_exports_round_trip_all_protocols_and_supported_fields() {
        let profiles = Kind::ALL.map(|kind| Profile {
            name: format!("Lab {}", kind.label()),
            kind: kind.as_str().into(),
            host: "192.0.2.10".into(),
            port: 2222,
            username: "operator".into(),
            auth: "key".into(),
            key_file: "C:/example/id_ed25519".into(),
            device: "COM4".into(),
            baud: 115200,
            favorite: true,
            tab_color: "#aabbcc".into(),
            keepalive: 15,
            log_path: "example.log".into(),
            local_echo: "off".into(),
            backspace: "ctrl-h".into(),
            device_syntax: "cisco_ios".into(),
            ..Profile::default()
        });
        let text = export_profiles(&profiles).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(payload["version"], 1);
        assert_eq!(payload["sessions"].as_array().unwrap().len(), 4);
        let imported = parse_import(text.as_bytes()).unwrap();
        assert!(imported.warnings.is_empty());
        assert_eq!(imported.profiles, profiles);
    }

    #[test]
    fn imported_unsupported_fields_do_not_pass_through_export_or_persistence() {
        let input = br#"{"version":1,"sessions":[{"name":"Lab","kind":"ssh","host":"192.0.2.1","password":"secret-password","private_key":"secret-key-material","known_hosts":"secret-trust"}]}"#;
        let imported = parse_import(input).unwrap();
        assert_eq!(imported.profiles.len(), 1);
        assert_eq!(imported.warnings.len(), 1);
        let text = export_profiles(&imported.profiles).unwrap();
        for secret in ["secret-password", "secret-key-material", "secret-trust", "known_hosts", "private_key"] {
            assert!(!text.contains(secret));
        }
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore { path: dir.path().join("sessions.json"), profiles: imported.profiles };
        store.save().unwrap();
        let saved = fs::read_to_string(&store.path).unwrap();
        assert!(!saved.contains("secret-"));
    }

    #[test]
    fn import_rejects_invalid_envelopes_and_oversized_inputs() {
        for input in [
            "invalid",
            "[]",
            "{}",
            r#"{"version":2,"sessions":[]}"#,
            r#"{"version":"1","sessions":[]}"#,
            r#"{"version":1,"sessions":{}}"#,
        ] {
            assert!(parse_import(input.as_bytes()).is_err(), "{input}");
        }
        assert!(parse_import(&vec![b' '; 16 * 1024 * 1024 + 1]).is_err());
        assert!(parse_import(b"\xef\xbb\xbf{\"version\":1,\"sessions\":[]}").is_ok());
    }

    #[test]
    fn import_skips_malformed_and_incompatible_entries_without_changing_destination() {
        let input = br#"{"version":1,"sessions":[
            {"name":"SSH","kind":"ssh","host":"192.0.2.1"},
            {"name":"Telnet","kind":"telnet","host":"192.0.2.2"},
            {"name":"Bad port","kind":"ssh","host":"192.0.2.3","port":"22"},
            {"name":"Zero port","kind":"ssh","host":"192.0.2.3","port":0},
            {"name":"Huge port","kind":"ssh","host":"192.0.2.3","port":65536},
            {"name":"Missing host","kind":"ssh"},
            {"name":"Unsupported","kind":"ftp","host":"192.0.2.3"},
            {"name":"Missing protocol","host":"192.0.2.3"},
            {"name":"Raw no port","kind":"raw","host":"192.0.2.3"},
            {"name":"Bad serial","kind":"serial","device":"COM4","stopbits":1.5},
            {"name":"No serial device","kind":"serial"},
            {"name":"Bad bool","kind":"ssh","host":"192.0.2.3","favorite":"yes"},
            {"name":"Bad font","kind":"ssh","host":"192.0.2.3","font_size":-1},
            {"name":"Valid serial","kind":"serial","device":"COM4"},
            false
        ]}"#;
        let imported = parse_import(input).unwrap();
        assert_eq!(
            imported.profiles.iter().map(|profile| profile.name.as_str()).collect::<Vec<_>>(),
            ["SSH", "Telnet", "Valid serial"]
        );
        assert_eq!(imported.profiles[0].port, 22);
        assert_eq!(imported.profiles[1].port, 23);
        assert_eq!(imported.warnings.len(), 12);
    }

    #[test]
    fn import_clears_invalid_colors_with_a_preview_note() {
        let imported = parse_import(
            br#"{"version":1,"sessions":[{"name":"Lab","kind":"ssh","host":"192.0.2.1","tab_color":"invalid"}]}"#,
        )
        .unwrap();
        assert!(imported.profiles[0].tab_color.is_empty());
        assert_eq!(imported.warnings, ["Lab: invalid tab color cleared"]);
    }

    #[test]
    fn merge_import_preserves_existing_profiles_and_reserves_case_insensitive_suffixes() {
        let profile = |name: &str| Profile { name: name.into(), host: "192.0.2.1".into(), ..Profile::default() };
        let existing = [profile("LAB"), profile("Lab (Imported 1)")];
        let merged = merge_import(&existing, [profile("lab"), profile("Lab"), profile("Other")]);
        assert_eq!(merged[..2], existing);
        assert_eq!(
            merged.iter().map(|profile| profile.name.as_str()).collect::<Vec<_>>(),
            ["LAB", "Lab (Imported 1)", "lab (Imported 2)", "Lab (Imported 3)", "Other"]
        );
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
            "favorite",
            "tab_color",
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
