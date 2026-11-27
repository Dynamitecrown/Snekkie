//! Read-only OpenSSH client config (~/.ssh/config) import. Only plain Host
//! blocks are used. Nothing in the file is ever run, ProxyCommand included.

use crate::profiles::{Profile, Tunnel, TunnelKind, parse_destination, parse_listen};
use crate::putty::ImportBatch;
use std::path::{Path, PathBuf};

/// Settings people rely on that Snekkie can't honor, so they get a note.
const UNSUPPORTED: &[&str] =
    &["ProxyJump", "ProxyCommand", "LocalCommand", "RemoteCommand", "ForwardAgent", "CertificateFile"];

pub fn default_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".ssh").join("config")
}

/// A config line as a lowercased keyword and its arguments, quotes removed.
fn split(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    let (mut args, mut current, mut quoted, mut started) = (Vec::new(), String::new(), false, false);
    for c in rest.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '#' if !quoted && !started => break,
            c if c.is_whitespace() && !quoted => {
                if std::mem::take(&mut started) {
                    args.push(std::mem::take(&mut current));
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    Some((line[..end].to_ascii_lowercase(), args))
}

struct Block {
    patterns: Vec<String>,
    values: Vec<(String, Vec<String>)>,
}

pub fn parse(text: &str, defaults: &crate::settings::AppSettings) -> ImportBatch {
    let mut batch = ImportBatch::default();
    let mut blocks: Vec<Block> = Vec::new();
    let mut in_match = false;
    let mut global_noted = false;
    for (keyword, args) in text.lines().filter_map(split) {
        match keyword.as_str() {
            "host" => {
                in_match = false;
                blocks.push(Block { patterns: args, values: Vec::new() });
            }
            "match" => {
                in_match = true;
                batch.warnings.push(format!("Match {}: Match blocks are not imported; skipped.", args.join(" ")));
            }
            "include" => batch.warnings.push(format!(
                "Include {}: included files are not read. Open them on their own to import their hosts.",
                args.join(" ")
            )),
            _ if in_match => {}
            _ => match blocks.last_mut() {
                Some(block) => block.values.push((keyword, args)),
                None if !global_noted => {
                    global_noted = true;
                    batch
                        .warnings
                        .push("Settings before the first Host line apply to every host and are not imported.".into());
                }
                None => {}
            },
        }
    }

    let mut aliases: Vec<&str> = Vec::new();
    for pattern in blocks.iter().flat_map(|b| &b.patterns) {
        if pattern.contains(['*', '?']) || pattern.starts_with('!') {
            batch.warnings.push(format!(
                "Host {pattern}: wildcard pattern skipped; its settings are not applied to other hosts."
            ));
        } else if !aliases.iter().any(|a| a.eq_ignore_ascii_case(pattern)) {
            aliases.push(pattern);
        }
    }

    for &alias in &aliases {
        // Every block naming this host, in file order: the first value wins, as in OpenSSH.
        let values: Vec<&(String, Vec<String>)> = blocks
            .iter()
            .filter(|b| b.patterns.iter().any(|p| p.eq_ignore_ascii_case(alias)))
            .flat_map(|b| &b.values)
            .collect();
        let all = |key: &str| -> Vec<&String> {
            values.iter().filter(|(k, _)| k == key).filter_map(|(_, a)| a.first()).collect()
        };
        let get = |key: &str| all(key).first().map(|v| v.as_str());
        let port = match get("port").map(str::parse::<u16>) {
            None => 22,
            Some(Ok(port)) if port > 0 => port,
            Some(_) => {
                batch.warnings.push(format!("{alias}: invalid port; skipped."));
                continue;
            }
        };
        let identities: Vec<&String> =
            all("identityfile").into_iter().filter(|p| !p.eq_ignore_ascii_case("none")).collect();
        let key_file = identities.first().map_or(String::new(), |p| p.to_string());
        if identities.len() > 1 {
            batch.warnings.push(format!("{alias}: only the first IdentityFile ({key_file}) is used."));
        }
        // ProxyJump naming another imported Host becomes that session's jump
        // host; user@host:port forms and chains stay a note.
        let proxy_jump = get("proxyjump").filter(|value| !value.eq_ignore_ascii_case("none"));
        let jump_host = proxy_jump
            .and_then(|value| aliases.iter().find(|a| a.eq_ignore_ascii_case(value) && !a.eq_ignore_ascii_case(alias)))
            .map_or(String::new(), |a| a.to_string());
        let unsupported: Vec<&str> = UNSUPPORTED
            .iter()
            .copied()
            .filter(|name| get(&name.to_ascii_lowercase()).is_some())
            .filter(|name| *name != "ProxyJump" || (proxy_jump.is_some() && jump_host.is_empty()))
            .collect();
        if !unsupported.is_empty() {
            batch.warnings.push(format!(
                "{alias}: {} not supported and not imported. Check that the host is directly reachable.",
                unsupported.join(", ")
            ));
        }
        let mut tunnels = Vec::new();
        for (keyword, args) in &values {
            let name = match keyword.as_str() {
                "localforward" => "LocalForward",
                "remoteforward" => "RemoteForward",
                "dynamicforward" => "DynamicForward",
                _ => continue,
            };
            match forward(keyword, args) {
                Some((tunnel, others)) => {
                    if others {
                        batch.warnings.push(format!(
                            "{alias}: tunnel {} listens on 127.0.0.1 only after import; tick Allow other computers to change that.",
                            tunnel.label()
                        ));
                    }
                    tunnels.push(tunnel);
                }
                None => batch
                    .warnings
                    .push(format!("{alias}: {name} {} is not supported and not imported.", args.join(" "))),
            }
        }
        batch.profiles.push(Profile {
            name: alias.into(),
            kind: "ssh".into(),
            host: get("hostname").map_or(alias.to_string(), |h| h.replace("%h", alias)),
            port,
            username: get("user").unwrap_or_default().into(),
            // Like the PuTTY import: a password unless a key file is named.
            auth: if key_file.is_empty() { "password" } else { "key" }.into(),
            key_file,
            jump_host,
            tunnels,
            scrollback: defaults.scrollback,
            font_family: defaults.font_family.clone(),
            font_size: defaults.font_size,
            ..Profile::default()
        });
    }
    batch.profiles.sort_by_key(|p| p.name.to_lowercase());
    batch
}

/// A simple LocalForward/RemoteForward/DynamicForward line as a tunnel, and
/// whether it listened beyond loopback. Socket paths and other forms aren't.
fn forward(keyword: &str, args: &[String]) -> Option<(Tunnel, bool)> {
    let kind = match keyword {
        "localforward" => TunnelKind::Local,
        "remoteforward" => TunnelKind::Remote,
        _ => TunnelKind::Dynamic,
    };
    let (listen_port, others) = parse_listen(args.first()?)?;
    let mut tunnel = Tunnel { kind, listen_port, ..Tunnel::default() };
    match (kind, args) {
        (TunnelKind::Dynamic, [_]) => {}
        (TunnelKind::Local | TunnelKind::Remote, [_, destination]) => {
            (tunnel.host, tunnel.port) = parse_destination(destination)?;
        }
        _ => return None,
    }
    Some((tunnel, others))
}

pub fn read(path: &Path, defaults: &crate::settings::AppSettings) -> Result<ImportBatch, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(4 * 1024 * 1024).read_to_end(&mut bytes))
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    Ok(parse(&String::from_utf8_lossy(&bytes), defaults))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
# Global defaults
ServerAliveInterval 30

Host core-sw1 core-sw2
    HostName %h.lab.example
    User netops
    Port 2222

Host bastion
    HostName=10.0.0.1
    User = admin   # trailing comment
    IdentityFile "~/.ssh/my key"
    IdentityFile ~/.ssh/other
    ProxyJump jump.example
    LocalForward 8080 localhost:80
    LocalForward *:8443 [fd00::1]:443
    RemoteForward 2222 10.9.9.9:22
    DynamicForward 127.0.0.1:1080
    LocalForward /tmp/socket host:80
    RemoteForward 9000
    ProxyCommand rm -rf /

Host BASTION
    User ignored
    Port 2200

Host *.example !skip.example
    User nobody

Match host secret
    User matched

Include conf.d/*

Host badport
    Port 99999

Host inside
    HostName 10.1.1.1
    ProxyJump Bastion

Host direct
    ProxyJump none
"#;

    #[test]
    fn parses_host_blocks_and_notes_what_is_skipped() {
        let batch = parse(CONFIG, &crate::settings::AppSettings::default());
        let names: Vec<&str> = batch.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["bastion", "core-sw1", "core-sw2", "direct", "inside"]);
        // ProxyJump naming an imported Host becomes its jump host; others stay notes.
        assert_eq!(batch.profiles[4].jump_host, "bastion");
        assert_eq!(batch.profiles[0].jump_host, "");
        assert_eq!(batch.profiles[3].jump_host, "");

        let bastion = &batch.profiles[0];
        assert_eq!((bastion.host.as_str(), bastion.port, bastion.username.as_str()), ("10.0.0.1", 2200, "admin"));
        assert_eq!((bastion.auth.as_str(), bastion.key_file.as_str()), ("key", "~/.ssh/my key"));
        let sw2 = &batch.profiles[2];
        assert_eq!((sw2.host.as_str(), sw2.port, sw2.username.as_str()), ("core-sw2.lab.example", 2222, "netops"));
        assert_eq!((sw2.kind.as_str(), sw2.auth.as_str(), sw2.key_file.as_str()), ("ssh", "password", ""));

        let notes = batch.warnings.join("\n");
        for expected in [
            "before the first Host line",
            "Host *.example: wildcard",
            "Host !skip.example: wildcard",
            "Match host secret",
            "Include conf.d/*",
            "badport: invalid port",
            "bastion: only the first IdentityFile",
            "bastion: ProxyJump, ProxyCommand not supported",
            "bastion: LocalForward /tmp/socket host:80 is not supported",
            "bastion: RemoteForward 9000 is not supported",
            "bastion: tunnel L 8443 → [fd00::1]:443 listens on 127.0.0.1 only",
        ] {
            assert!(notes.contains(expected), "missing {expected:?} in\n{notes}");
        }
        assert!(!notes.contains("matched") && !notes.contains("rm -rf"), "{notes}");

        let labels: Vec<String> = bastion.tunnels.iter().map(Tunnel::label).collect();
        assert_eq!(
            labels,
            ["L 8080 → localhost:80", "L 8443 → [fd00::1]:443", "R 2222 → 10.9.9.9:22", "D 1080 (SOCKS)"]
        );
        assert!(bastion.tunnels.iter().all(|t| t.enabled && !t.allow_others));
        assert!(batch.profiles[1].tunnels.is_empty());
    }

    #[test]
    fn splits_keywords_equals_signs_and_quotes() {
        assert_eq!(split("  # comment"), None);
        assert_eq!(split("HostName=host"), Some(("hostname".into(), vec!["host".into()])));
        assert_eq!(split("User = me # note"), Some(("user".into(), vec!["me".into()])));
        assert_eq!(
            split(r#"IdentityFile "C:\My Keys\id" extra"#),
            Some(("identityfile".into(), vec![r"C:\My Keys\id".into(), "extra".into()]))
        );
    }
}
