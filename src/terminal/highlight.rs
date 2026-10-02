//! Device-aware, graded highlighting of terminal text. This display
//! overlay never alters transmitted, copied, logged or exported text.

mod devices;

use egui::Color32;
use regex::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::LazyLock;

pub const DEFAULT_INTENSITY: u8 = 3;
pub const INTENSITY_RANGE: std::ops::RangeInclusive<u8> = 1..=5;

pub fn normalize_intensity(value: u8) -> u8 {
    if value == 0 { DEFAULT_INTENSITY } else { value.clamp(1, 5) }
}

pub fn intensity_label(value: u8) -> &'static str {
    match normalize_intensity(value) {
        1 => "Essential",
        2 => "Status",
        3 => "Commands",
        4 => "Detailed",
        _ => "Full",
    }
}

pub fn intensity_help(value: u8) -> &'static str {
    match normalize_intensity(value) {
        1 => "IP addresses and security settings, including encryption and authentication.",
        2 => "Also highlights prompts, protocols and positive or negative status.",
        3 => "Also highlights commands, negation and interface names.",
        4 => "Also highlights device-specific configuration details and MAC addresses.",
        _ => "Also highlights numbers, units, table headings and descriptions.",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Keyword,
    Value,
    Prompt,
    Negate,
    Security,
    Interface,
    Protocol,
    Good,
    Bad,
    Number,
    Metadata,
    Comment,
}

impl Category {
    pub fn color(self) -> Color32 {
        match self {
            Self::Keyword => Color32::from_rgb(0x4f, 0xa8, 0xe0),
            Self::Value => Color32::from_rgb(0xc9, 0xa8, 0x6a),
            Self::Prompt | Self::Good => Color32::from_rgb(0x5f, 0xd9, 0x7a),
            Self::Negate | Self::Bad => Color32::from_rgb(0xe0, 0x66, 0x5a),
            Self::Security => Color32::from_rgb(0xd5, 0x9c, 0xef),
            Self::Interface => Color32::from_rgb(0x65, 0xd6, 0xd1),
            Self::Protocol => Color32::from_rgb(0x9a, 0xa5, 0xff),
            Self::Number => Color32::from_rgb(0xea, 0xbd, 0x78),
            Self::Metadata => Color32::from_rgb(0xb9, 0xc6, 0xdc),
            Self::Comment => Color32::from_rgb(0x98, 0xad, 0x9d),
        }
    }

    pub fn color_on(self, background: Color32) -> Color32 {
        let color = self.color();
        if u16::from(background.r()) + u16::from(background.g()) + u16::from(background.b()) > 500 {
            Color32::from_rgb(color.r() / 2, color.g() / 2, color.b() / 2)
        } else {
            color
        }
    }
}

pub fn syntax_options() -> impl Iterator<Item = (&'static str, &'static str)> {
    std::iter::once(("none", "None")).chain(devices::DEVICES.iter().map(|d| (d.key, d.label)))
}

pub fn syntax_label(value: &str) -> &'static str {
    syntax_options().find(|(v, _)| *v == value).map_or("None", |(_, l)| l)
}

pub fn sample(syntax: &str) -> &'static str {
    devices::DEVICES.iter().find(|d| d.key == syntax).map_or("No device highlighting selected.", |d| d.sample)
}

struct Rule {
    pattern: Regex,
    category: Category,
    level: u8,
    priority: u8,
    group: usize,
    address: bool,
}

impl Rule {
    fn new(pattern: &str, category: Category, level: u8, priority: u8) -> Self {
        Self {
            pattern: RegexBuilder::new(pattern).case_insensitive(true).build().unwrap(),
            category,
            level,
            priority,
            group: 0,
            address: false,
        }
    }

    fn words(words: &str, category: Category, level: u8, priority: u8) -> Self {
        let mut words: Vec<_> = words.split_ascii_whitespace().collect();
        words.sort_by_key(|w| std::cmp::Reverse(w.len()));
        words.dedup();
        let pattern = words.into_iter().map(regex::escape).collect::<Vec<_>>().join("|");
        Self::new(&format!(r"\b(?:{pattern})\b"), category, level, priority)
    }

    fn group(mut self, group: usize) -> Self {
        self.group = group;
        self
    }
}

static COMMON: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    use Category::*;
    let mut ipv4 = Rule::new(r"\b(?:\d{1,3}\.){3}\d{1,3}(?:/\d{1,3})?", Value, 1, 100);
    ipv4.address = true;
    let mut ipv6 = Rule::new(r"(?:[0-9a-f]{0,4}:){2,}[0-9a-f:.]*(?:%[a-z0-9_.-]+)?(?:/\d{1,3})?", Value, 1, 100);
    ipv6.address = true;
    vec![
        ipv4,
        ipv6,
        Rule::words(
            "crypto encryption encrypt encrypted password-encryption secret password passwd authpasswd authentication authorization accounting aaa algorithm-type algorithm cipher ciphers aes aes128 aes192 aes256 aes-128 aes-192 aes-256 aes-gcm aes-cbc aes128-sha1 aes256-sha1 aes128-sha256 aes256-sha256 aes256gcm sha sha1 sha256 sha384 sha512 sha-1 sha-256 sha-384 sha-512 md5 scrypt pbkdf2 des 3des rsa ecdsa ed25519 diffie-hellman dh-group dhgrp ike ikev1 ikev2 ipsec isakmp tls ssl ssh ssh-rsa hmac-sha256 hmac-sha512 pre-shared-key psksecret authmethod proposal pfs certificate trustpoint key-chain key-string encrypted-password authentication-key ike-gateway crypto-profiles",
            Security,
            1,
            90,
        ),
        Rule::new(r"\b(?:secret|password)\s+([0-9])\b", Security, 1, 90).group(1),
        Rule::new(r"^\s*([a-z0-9_.@:/-]+(?:\([^\r\n)]*\)|\s+\([^\r\n)]*\))?\s*[>#])", Prompt, 2, 80).group(1),
        Rule::words(
            "up connected established active enabled reachable success successful permit accept allow forwarding full complete ready online",
            Good,
            2,
            70,
        ),
        Rule::words(
            "down disconnected failed failure error errors invalid denied deny reject rejected drop dropped disabled unreachable err-disabled inactive blocking administratively-down",
            Bad,
            2,
            70,
        ),
        Rule::words(
            "bgp ospf ospfv3 eigrp rip isis is-is pim igmp mpls vxlan evpn bfd stp rstp mstp lacp lldp cdp vrrp hsrp dhcp dns ntp snmp tcp udp icmp icmpv6 arp",
            Protocol,
            2,
            65,
        ),
        Rule::new(
            r"^\s*(?:[a-z0-9_.@:/-]+(?:\([^\r\n)]*\)|\s+\([^\r\n)]*\))?\s*[>#]\s*)?(no|undo|delete|unset)\b",
            Negate,
            3,
            85,
        )
        .group(1),
        Rule::new(
            r"\b(?:[0-9a-f]{2}:){5}[0-9a-f]{2}\b|\b(?:[0-9a-f]{2}-){5}[0-9a-f]{2}\b|\b[0-9a-f]{4}(?:\.[0-9a-f]{4}){2}\b",
            Value,
            4,
            55,
        ),
        Rule::new(r"\b\d+(?:\.\d+)?(?:[kmg]?(?:bps|bytes)|ms)?\b", Number, 5, 10),
        Rule::words(
            "interface ip-address ok method status protocol vlan name ports type mtu bw delay reliability load packets bytes input output errors drops crc collisions uptime version hardware software serial model total last never unassigned",
            Metadata,
            5,
            15,
        ),
        Rule::new(r"^\s*(?:description|remark|alias)\s+(.+)$", Comment, 5, 5).group(1),
        Rule::new(r"^\s*[!#](?:\s.*)?$|^\s*\[edit[^\]]*\]", Comment, 5, 5),
    ]
});

static DEVICE_RULES: LazyLock<HashMap<&'static str, Vec<Rule>>> = LazyLock::new(|| {
    devices::DEVICES
        .iter()
        .map(|d| {
            (
                d.key,
                vec![
                    Rule::words(d.commands, Category::Keyword, 3, 30),
                    Rule::words(d.details, Category::Keyword, 4, 30),
                    Rule::new(d.interfaces, Category::Interface, 3, 60),
                ],
            )
        })
        .collect()
});

fn valid_address(text: &str, start: usize, end: usize) -> bool {
    let continuation = |c: char| c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '%');
    if text[..start].chars().next_back().is_some_and(continuation)
        || text[end..].chars().next().is_some_and(continuation)
    {
        return false;
    }
    let token = &text[start..end];
    let (host, prefix) = token.split_once('/').map_or((token, None), |(host, prefix)| (host, Some(prefix)));
    let (host, zone) = host.split_once('%').map_or((host, None), |(host, zone)| (host, Some(zone)));
    let Ok(ip) = host.parse::<IpAddr>() else { return false };
    if zone.is_some() && !ip.is_ipv6() {
        return false;
    }
    prefix.is_none_or(|p| p.parse::<u8>().is_ok_and(|p| p <= if ip.is_ipv4() { 32 } else { 128 }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Character positions, including when the row contains Unicode.
    pub start: usize,
    pub end: usize,
    pub category: Category,
}

pub fn highlight_line(text: &str, syntax: &str, intensity: u8) -> Vec<Span> {
    let Some(device) = DEVICE_RULES.get(syntax) else { return Vec::new() };
    let text = text.trim_end();
    if text.trim_start().is_empty() {
        return Vec::new();
    }
    let intensity = normalize_intensity(intensity);
    let mut char_at = vec![0usize; text.len() + 1];
    let mut count = 0;
    for (byte, ch) in text.char_indices() {
        char_at[byte..byte + ch.len_utf8()].fill(count);
        count += 1;
    }
    char_at[text.len()] = count;
    let mut per_char: Vec<Option<(u8, Category)>> = vec![None; count];
    for rule in COMMON.iter().chain(device).filter(|r| r.level <= intensity) {
        for capture in rule.pattern.captures_iter(text) {
            let Some(m) = capture.get(rule.group) else { continue };
            if rule.address && !valid_address(text, m.start(), m.end()) {
                continue;
            }
            for slot in &mut per_char[char_at[m.start()]..char_at[m.end()]] {
                if slot.is_none_or(|(priority, _)| rule.priority > priority) {
                    *slot = Some((rule.priority, rule.category));
                }
            }
        }
    }
    let mut spans: Vec<Span> = Vec::new();
    for (i, cat) in per_char.into_iter().enumerate() {
        let Some((_, category)) = cat else { continue };
        match spans.last_mut() {
            Some(last) if last.end == i && last.category == category => last.end = i + 1,
            _ => spans.push(Span { start: i, end: i + 1, category }),
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str, syntax: &str, intensity: u8) -> Vec<(String, Category)> {
        let chars: Vec<char> = text.chars().collect();
        highlight_line(text, syntax, intensity)
            .into_iter()
            .map(|s| (chars[s.start..s.end].iter().collect(), s.category))
            .collect()
    }

    #[test]
    fn essential_focuses_on_valid_addresses_and_security() {
        let found =
            words("Switch# show ip address 192.0.2.1/24 crypto ikev2 encryption aes-256 secret 9", "cisco_ios", 1);
        assert_eq!(found[0], ("192.0.2.1/24".into(), Category::Value));
        assert!(found.iter().skip(1).all(|(_, c)| *c == Category::Security));
        for token in ["192.0.2.1", "2001:db8::1/64", "fe80::1%eth0", "::1", "::", "::ffff:192.0.2.1"] {
            assert_eq!(words(token, "cisco_ios", 1), [(token.into(), Category::Value)], "{token}");
        }
        for token in ["999.1.2.3", "1.2.3.4/33", "2001:db8::1/129", "version1.2.3.4", "12:30:45", "0011.2233.4455"] {
            assert!(words(token, "cisco_ios", 1).is_empty(), "{token}");
        }
    }

    #[test]
    fn higher_intensity_adds_coverage_without_replacing_essential_categories() {
        for d in devices::DEVICES {
            let mut previous = vec![None; d.sample.chars().count()];
            for level in 1..=5 {
                let mut current = vec![None; previous.len()];
                for span in highlight_line(d.sample, d.key, level) {
                    current[span.start..span.end].fill(Some(span.category));
                }
                for (old, new) in previous.iter().zip(&current) {
                    if old.is_some() {
                        assert_eq!(old, new, "{} level {level}", d.key);
                    }
                }
                previous = current;
            }
            assert!(!highlight_line(d.sample, d.key, 1).is_empty(), "{}", d.key);
        }
    }

    #[test]
    fn device_vocabularies_and_interface_names_are_specific() {
        for (syntax, text, interface, keyword) in [
            ("cisco_ios", "interface GigabitEthernet1/0/1", "GigabitEthernet1/0/1", "interface"),
            ("cisco_nxos", "feature vpc Ethernet1/1", "Ethernet1/1", "vpc"),
            ("cisco_asa", "nameif inside GigabitEthernet0/1", "GigabitEthernet0/1", "nameif"),
            ("juniper_junos", "set interfaces ge-0/0/0.0", "ge-0/0/0.0", "interfaces"),
            ("aruba_aoscx", "interface 1/1/1 lag", "1/1/1", "lag"),
            ("arista_eos", "mlag Ethernet1", "Ethernet1", "mlag"),
            ("fortinet_fortios", "set allowaccess port1", "port1", "allowaccess"),
            ("paloalto_panos", "set network interface ethernet1/1 layer3", "ethernet1/1", "layer3"),
        ] {
            let found = words(text, syntax, 4);
            assert!(found.contains(&(interface.into(), Category::Interface)), "{syntax}: {found:?}");
            assert!(found.contains(&(keyword.into(), Category::Keyword)), "{syntax}: {found:?}");
        }
        assert!(words("mlag vpc allowaccess", "juniper_junos", 4).is_empty());
    }

    #[test]
    fn prompts_negation_case_unicode_and_empty_rows() {
        for prompt in ["Switch(config-if)#", "user@router>", "FGT (root) #"] {
            assert_eq!(words(&format!("{prompt} show"), "cisco_ios", 2)[0], (prompt.into(), Category::Prompt));
        }
        assert_eq!(words("  no shutdown", "cisco_ios", 3)[0], ("no".into(), Category::Negate));
        assert_eq!(words("Switch(config)#no shutdown", "cisco_ios", 3)[1], ("no".into(), Category::Negate));
        assert_eq!(
            words("SHOW Version", "cisco_ios", 3),
            [("SHOW".into(), Category::Keyword), ("Version".into(), Category::Keyword)]
        );
        assert_eq!(highlight_line("é show", "cisco_ios", 3), [Span { start: 2, end: 6, category: Category::Keyword }]);
        assert_eq!(words("access-list 10", "cisco_ios", 3), [("access-list".into(), Category::Keyword)]);
        for syntax in ["none", "unknown"] {
            assert!(highlight_line("show 192.0.2.1", syntax, 5).is_empty());
        }
        assert!(words("     ", "cisco_ios", 5).is_empty());
        assert!(words("showing endless", "cisco_ios", 3).is_empty());
    }

    #[test]
    fn full_detail_marks_macs_numbers_and_descriptions_without_splitting_addresses() {
        let found = words("description uplink 0011.2233.4455 10.0.0.1 vlan 100", "cisco_ios", 5);
        assert!(found.contains(&("0011.2233.4455".into(), Category::Value)));
        assert!(found.contains(&("10.0.0.1".into(), Category::Value)));
        assert!(found.contains(&("100".into(), Category::Number)));
        assert!(found.iter().any(|(s, c)| s.contains("uplink") && *c == Category::Comment));
    }

    #[test]
    fn every_device_is_selectable_and_has_a_preview() {
        for d in devices::DEVICES {
            assert_eq!(syntax_label(d.key), d.label);
            assert!(!sample(d.key).is_empty());
        }
    }
}
