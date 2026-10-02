//! Read-only PuTTY session import. Registry exports are parsed as data, never
//! executed or installed. Only supported connection/terminal fields are used.

use crate::profiles::Profile;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

type Values = BTreeMap<String, String>;
const ROOT: &str = r"Software\SimonTatham\PuTTY\Sessions";
const STRINGS: &[&str] = &[
    "HostName",
    "Protocol",
    "UserName",
    "PublicKeyFile",
    "SerialLine",
    "Font",
    "ProxyHost",
    "PortForwardings",
    "RemoteCommand",
];
const NUMBERS: &[&str] = &[
    "PortNumber",
    "PingInterval",
    "PingIntervalSecs",
    "SerialSpeed",
    "SerialDataBits",
    "SerialStopHalfbits",
    "SerialParity",
    "SerialFlowControl",
    "ScrollbackLines",
    "FontHeight",
    "LocalEcho",
    "BackspaceIsDelete",
    "TryAgent",
    "ProxyMethod",
    "SSH2DES",
    "SshNoAuth",
];

#[derive(Debug, Default)]
pub struct ImportBatch {
    pub profiles: Vec<Profile>,
    pub warnings: Vec<String>,
}

fn decode_name(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() && bytes[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit) {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
            out.push(u8::from_str_radix(hex, 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn convert(sessions: BTreeMap<String, Values>, defaults: &crate::settings::AppSettings) -> ImportBatch {
    let mut batch = ImportBatch::default();
    for (encoded, values) in sessions {
        let name = decode_name(&encoded);
        if name == "Default Settings" {
            continue;
        }
        let get = |key: &str| values.get(key).map_or("", String::as_str);
        let number = |key: &str, fallback: u32| get(key).parse::<u32>().unwrap_or(fallback);
        let protocol = if get("Protocol").is_empty() { "ssh" } else { get("Protocol") };
        if !matches!(protocol, "ssh" | "telnet" | "raw" | "serial") {
            batch.warnings.push(format!("{name}: unsupported protocol {protocol}; skipped."));
            continue;
        }
        let host = get("HostName").trim();
        let device = get("SerialLine").trim();
        if (protocol == "serial" && device.is_empty()) || (protocol != "serial" && host.is_empty()) {
            batch.warnings.push(format!("{name}: no destination; skipped."));
            continue;
        }
        let port = number("PortNumber", if protocol == "telnet" { 23 } else { 22 });
        if protocol != "serial" && !(1..=65535).contains(&port) {
            batch.warnings.push(format!("{name}: invalid port; skipped."));
            continue;
        }
        let mut profile = Profile {
            name,
            kind: protocol.into(),
            host: host.into(),
            port: port as u16,
            username: get("UserName").into(),
            device: device.into(),
            keepalive: number("PingInterval", 0).saturating_mul(60).saturating_add(number("PingIntervalSecs", 0)),
            scrollback: number("ScrollbackLines", defaults.scrollback).min(200_000),
            font_family: if get("Font").is_empty() { defaults.font_family.clone() } else { get("Font").into() },
            font_size: number("FontHeight", defaults.font_size).clamp(6, 48),
            local_echo: match number("LocalEcho", 2) {
                0 => "on",
                1 => "off",
                _ => "auto",
            }
            .into(),
            backspace: if number("BackspaceIsDelete", 1) != 0 { "del" } else { "ctrl-h" }.into(),
            ..Profile::default()
        };
        if protocol == "serial" {
            profile.baud = number("SerialSpeed", 9600).max(1);
            profile.bytesize = number("SerialDataBits", 8).clamp(5, 8) as u8;
            let parity = number("SerialParity", 0);
            let stop = number("SerialStopHalfbits", 2);
            let flow = number("SerialFlowControl", 1);
            if parity > 2 || !matches!(stop, 2 | 4) || flow > 2 {
                batch
                    .warnings
                    .push(format!("{}: unsupported serial parity, stop bits or flow control; skipped.", profile.name));
                continue;
            }
            profile.parity = ["None", "Odd", "Even"][parity as usize].into();
            profile.stopbits = stop as f32 / 2.0;
            profile.xonxoff = flow == 1;
            profile.rtscts = flow == 2;
        }
        if protocol == "ssh" {
            let key = get("PublicKeyFile");
            profile.auth = if number("TryAgent", 1) != 0 { "agent" } else { "password" }.into();
            if !key.is_empty() {
                if key.to_ascii_lowercase().ends_with(".ppk") {
                    batch.warnings.push(format!("{}: PuTTY .ppk key is not imported. Use Pageant or export the key to OpenSSH format, then select it in Snekkie.", profile.name));
                } else {
                    profile.key_file = key.into();
                    profile.auth = "key".into();
                }
            }
        }
        if number("ProxyMethod", 0) != 0 || !get("PortForwardings").is_empty() || !get("RemoteCommand").is_empty() {
            batch.warnings.push(format!("{}: proxy, forwarding or remote-command settings are not imported. Check that the destination is directly reachable.", profile.name));
        }
        batch.profiles.push(profile);
    }
    batch.profiles.sort_by_key(|p| p.name.to_lowercase());
    batch
}

fn quoted(text: &str) -> Option<(String, &str)> {
    let text = text.strip_prefix('"')?;
    let mut out = String::new();
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if escaped {
            if !matches!(ch, '\\' | '"') {
                return None;
            }
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some((out, &text[i + 1..]));
        } else {
            out.push(ch);
        }
    }
    None
}

pub fn parse_export(bytes: &[u8], defaults: &crate::settings::AppSettings) -> Result<ImportBatch, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("The PuTTY export is larger than 16 MiB.".into());
    }
    let text = if bytes.starts_with(&[0xff, 0xfe]) {
        if !bytes.len().is_multiple_of(2) {
            return Err("The UTF-16 registry export is incomplete.".into());
        }
        let units: Vec<_> = bytes[2..].as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
        String::from_utf16(&units).map_err(|_| "The registry export contains invalid UTF-16.".to_string())?
    } else {
        std::str::from_utf8(bytes)
            .map_err(|_| "Use a UTF-16 or UTF-8 PuTTY registry export.".to_string())?
            .trim_start_matches('\u{feff}')
            .to_string()
    };
    if !matches!(text.lines().next().map(str::trim), Some("Windows Registry Editor Version 5.00" | "REGEDIT4")) {
        return Err("Choose a Windows .reg export of PuTTY's Sessions registry key.".into());
    }
    let prefix = format!("hkey_current_user\\{}\\", ROOT.to_ascii_lowercase());
    let mut sessions = BTreeMap::<String, Values>::new();
    let mut current = None;
    for line in text.lines().skip(1).map(str::trim) {
        if line.starts_with('[') {
            current = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')).and_then(|key| {
                if !key.to_ascii_lowercase().starts_with(&prefix) {
                    return None;
                }
                let name = &key[prefix.len()..];
                (!name.is_empty() && !name.contains('\\')).then(|| name.to_string())
            });
            if let Some(name) = &current {
                sessions.entry(name.clone()).or_default();
            }
            continue;
        }
        let Some(session) = current.as_ref() else { continue };
        let Some((key, rest)) = quoted(line) else { continue };
        let Some(value) = rest.trim().strip_prefix('=').map(str::trim) else { continue };
        let value = if STRINGS.contains(&key.as_str()) {
            quoted(value).filter(|(_, rest)| rest.trim().is_empty()).map(|(s, _)| s)
        } else if NUMBERS.contains(&key.as_str()) {
            value.strip_prefix("dword:").and_then(|n| u32::from_str_radix(n, 16).ok()).map(|n| n.to_string())
        } else {
            None
        };
        if let Some(value) = value {
            sessions.get_mut(session).unwrap().insert(key, value);
        }
    }
    Ok(convert(sessions, defaults))
}

pub fn read_export(path: &Path, defaults: &crate::settings::AppSettings) -> Result<ImportBatch, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    parse_export(&bytes, defaults)
}

/// Add selected profiles as new names, keeping every existing profile intact.
pub fn merge(existing: &[Profile], incoming: impl IntoIterator<Item = Profile>) -> Vec<Profile> {
    let mut out = existing.to_vec();
    let mut names: BTreeSet<String> = existing.iter().map(|p| p.name.to_lowercase()).collect();
    for mut profile in incoming {
        let original = profile.name.clone();
        let mut suffix = 1;
        while names.contains(&profile.name.to_lowercase()) {
            profile.name = format!("{original} (PuTTY {suffix})");
            suffix += 1;
        }
        names.insert(profile.name.to_lowercase());
        out.push(profile);
    }
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

#[cfg(not(windows))]
pub fn read_registry(_defaults: &crate::settings::AppSettings) -> Result<ImportBatch, String> {
    Err("Windows registry import is available on Windows. You can import a .reg export here instead.".into())
}

#[cfg(windows)]
pub fn read_registry(defaults: &crate::settings::AppSettings) -> Result<ImportBatch, String> {
    read_registry_at(ROOT, defaults)
}

#[cfg(windows)]
fn read_registry_at(path: &str, defaults: &crate::settings::AppSettings) -> Result<ImportBatch, String> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::*;
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut handle = std::ptr::null_mut();
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide(path).as_ptr(), 0, KEY_READ, &mut handle) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(ImportBatch::default());
    }
    if status != ERROR_SUCCESS {
        return Err(format!("Could not read PuTTY sessions (Windows error {status})."));
    }
    let root = Key(handle);
    let mut sessions = BTreeMap::new();
    for index in 0..100_000 {
        let mut name = [0u16; 512];
        let mut length = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                root.0,
                index,
                name.as_mut_ptr(),
                &mut length,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            return Err(format!("Could not list PuTTY sessions (Windows error {status})."));
        }
        let encoded = String::from_utf16_lossy(&name[..length as usize]);
        let subkey = wide(&encoded);
        let mut values = Values::new();
        for &field in STRINGS.iter().chain(NUMBERS) {
            let key = wide(field);
            let mut size = 0;
            let status = unsafe {
                RegGetValueW(
                    root.0,
                    subkey.as_ptr(),
                    key.as_ptr(),
                    RRF_RT_REG_SZ | RRF_RT_REG_DWORD,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            };
            if status != ERROR_SUCCESS || size > 4 * 1024 * 1024 {
                continue;
            }
            let mut data = vec![0u8; size as usize];
            let mut kind = 0;
            let status = unsafe {
                RegGetValueW(
                    root.0,
                    subkey.as_ptr(),
                    key.as_ptr(),
                    RRF_RT_REG_SZ | RRF_RT_REG_DWORD,
                    &mut kind,
                    data.as_mut_ptr().cast(),
                    &mut size,
                )
            };
            if status != ERROR_SUCCESS {
                continue;
            }
            let value = if kind == REG_DWORD && size == 4 {
                u32::from_le_bytes(data[..4].try_into().unwrap()).to_string()
            } else if kind == REG_SZ {
                let units: Vec<_> = data[..size as usize]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| u16::from_le_bytes(*b))
                    .take_while(|&u| u != 0)
                    .collect();
                String::from_utf16_lossy(&units)
            } else {
                continue;
            };
            values.insert(field.into(), value);
        }
        sessions.insert(encoded, values);
    }
    Ok(convert(sessions, defaults))
}

#[cfg(test)]
mod tests {
    use super::*;
    const EXPORT: &str = r#"Windows Registry Editor Version 5.00

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\Lab%20%CE%A9%25]
"HostName"="192.0.2.1"
"Protocol"="ssh"
"PortNumber"=dword:000008ae
"UserName"="operator"
"PublicKeyFile"="C:\\keys\\router.ppk"
"PingInterval"=dword:00000001
"PingIntervalSecs"=dword:00000005
"LocalEcho"=dword:00000000
"BackspaceIsDelete"=dword:00000000
"ProxyMethod"=dword:00000002

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\Console]
"Protocol"="serial"
"SerialLine"="COM7"
"SerialSpeed"=dword:0001c200
"SerialParity"=dword:00000002
"SerialStopHalfbits"=dword:00000004
"SerialFlowControl"=dword:00000002

[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\Default%20Settings]
"HostName"="ignore-me"

[HKEY_CURRENT_USER\Software\OtherApp\DeleteEverything]
"HostName"="ignore-me"
"Protocol"="raw"
"PortNumber"=dword:00000017
"#;

    #[test]
    fn parses_supported_fields_utf16_unicode_and_key_notes_without_other_registry_data() {
        let settings = crate::settings::AppSettings::default();
        let bytes: Vec<u8> = [0xff, 0xfe].into_iter().chain(EXPORT.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        for export in [EXPORT.as_bytes(), bytes.as_slice()] {
            let batch = parse_export(export, &settings).unwrap();
            assert_eq!(batch.profiles.len(), 2);
            let ssh = &batch.profiles[1];
            assert_eq!(ssh.name, "Lab Ω%");
            assert_eq!(ssh.port, 2222);
            assert_eq!(ssh.username, "operator");
            assert_eq!(ssh.keepalive, 65);
            assert_eq!(ssh.local_echo, "on");
            assert_eq!(ssh.backspace, "ctrl-h");
            assert!(ssh.key_file.is_empty());
            assert_eq!(ssh.auth, "agent");
            let serial = &batch.profiles[0];
            assert_eq!(serial.device, "COM7");
            assert_eq!(serial.baud, 115200);
            assert_eq!(serial.parity, "Even");
            assert_eq!(serial.stopbits, 2.0);
            assert!(serial.rtscts);
            assert!(!serial.xonxoff);
            assert_eq!(batch.warnings.len(), 2);
        }
        assert!(parse_export(b"not a registry export", &settings).is_err());
        assert!(parse_export(&[0xff, 0xfe, 0], &settings).is_err());
    }

    #[test]
    fn incompatible_sessions_are_skipped_and_names_never_replace_existing_profiles() {
        let settings = crate::settings::AppSettings::default();
        let unsupported = EXPORT.replace("SerialParity\"=dword:00000002", "SerialParity\"=dword:00000003");
        assert_eq!(parse_export(unsupported.as_bytes(), &settings).unwrap().profiles.len(), 1);
        let existing = vec![Profile { name: "lab Ω%".into(), host: "original".into(), ..Profile::default() }];
        let imported = parse_export(EXPORT.as_bytes(), &settings).unwrap().profiles;
        let merged = merge(&existing, imported.clone());
        assert!(merged.contains(&existing[0]));
        assert!(merged.iter().any(|p| p.name == "Lab Ω% (PuTTY 1)"));
        let repeated = merge(&merged, imported);
        assert!(repeated.iter().any(|p| p.name == "Lab Ω% (PuTTY 2)"));
        assert_eq!(repeated.len(), 5);
    }

    #[cfg(windows)]
    #[test]
    fn reads_native_registry_strings_and_dwords_from_an_isolated_fixture() {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::*;
        let unique = tempfile::tempdir().unwrap();
        let path = format!(r"Software\Snekkie\ImportTests\{}", unique.path().file_name().unwrap().to_str().unwrap());
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        struct Cleanup(Vec<u16>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe {
                    RegDeleteTreeW(HKEY_CURRENT_USER, self.0.as_ptr());
                }
            }
        }
        assert!(path.starts_with(r"Software\Snekkie\ImportTests\"));
        let _cleanup = Cleanup(wide(&path));
        let mut key = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    wide(&format!(r"{path}\Native%20test")).as_ptr(),
                    0,
                    std::ptr::null(),
                    0,
                    KEY_WRITE,
                    std::ptr::null(),
                    &mut key,
                    std::ptr::null_mut(),
                )
            },
            ERROR_SUCCESS
        );
        for (field, value) in [("HostName", "192.0.2.1"), ("Protocol", "telnet"), ("UserName", "operator")] {
            let data = wide(value);
            assert_eq!(
                unsafe {
                    RegSetValueExW(key, wide(field).as_ptr(), 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32)
                },
                ERROR_SUCCESS
            );
        }
        let port = 2323u32.to_le_bytes();
        assert_eq!(
            unsafe { RegSetValueExW(key, wide("PortNumber").as_ptr(), 0, REG_DWORD, port.as_ptr(), 4) },
            ERROR_SUCCESS
        );
        unsafe {
            RegCloseKey(key);
        }
        let batch = read_registry_at(&path, &crate::settings::AppSettings::default()).unwrap();
        assert_eq!(batch.profiles.len(), 1);
        assert_eq!(batch.profiles[0].name, "Native test");
        assert_eq!(batch.profiles[0].port, 2323);
        assert_eq!(batch.profiles[0].kind, "telnet");
        assert_eq!(batch.profiles[0].username, "operator");
    }
}
