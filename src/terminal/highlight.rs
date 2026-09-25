//! Keyword-based syntax highlighting for the terminal display.
//!
//! Purely a rendering overlay: it recolours words the screen happens to
//! contain that match the chosen device's command syntax. It doesn't touch
//! what's sent to the remote and can't tell your typed command from the
//! device's echo of it. Add a device by adding an entry to [`syntax_rules`]
//! and [`SYNTAX_LABELS`].

use std::sync::LazyLock;

use egui::Color32;
use regex::{Regex, RegexBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Keyword,
    Value,
    Prompt,
    Negate,
}

impl Category {
    /// Device-agnostic, so every syntax shares one look.
    pub fn color(self) -> Color32 {
        match self {
            Category::Keyword => Color32::from_rgb(0x4f, 0xa8, 0xe0),
            Category::Value => Color32::from_rgb(0xc9, 0xa8, 0x6a),
            Category::Prompt => Color32::from_rgb(0x5f, 0xd9, 0x7a),
            Category::Negate => Color32::from_rgb(0xe0, 0x66, 0x5a),
        }
    }
}

/// value -> label, in display order. "none" always means no highlighting.
pub const SYNTAX_LABELS: [(&str, &str); 2] = [("none", "None"), ("cisco_ios", "Cisco IOS")];

pub fn syntax_label(value: &str) -> &'static str {
    SYNTAX_LABELS.iter().find(|(v, _)| *v == value).map_or("None", |(_, l)| l)
}

const CISCO_IOS_KEYWORDS: &[&str] = &[
    "show",
    "configure",
    "terminal",
    "interface",
    "shutdown",
    "ip",
    "ipv6",
    "address",
    "hostname",
    "enable",
    "disable",
    "exit",
    "write",
    "copy",
    "running-config",
    "startup-config",
    "vlan",
    "switchport",
    "access",
    "access-list",
    "permit",
    "deny",
    "router",
    "network",
    "description",
    "duplex",
    "speed",
    "spanning-tree",
    "channel-group",
    "line",
    "vty",
    "console",
    "password",
    "secret",
    "login",
    "banner",
    "end",
    "reload",
    "ping",
    "traceroute",
    "clock",
    "logging",
    "snmp-server",
    "crypto",
    "key",
    "route",
    "default-gateway",
    "mode",
    "trunk",
    "encapsulation",
    "mtu",
    "version",
    "service",
    "boot",
    "system",
    "do",
    "wr",
    "conf",
];

type Rule = (Regex, Category);

fn ci(pattern: &str) -> Regex {
    RegexBuilder::new(pattern).case_insensitive(true).build().unwrap()
}

static CISCO_IOS: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    // Longest first, so "access-list" wins over "access" at the same spot.
    let mut words: Vec<&str> = CISCO_IOS_KEYWORDS.to_vec();
    words.sort_by_key(|w| std::cmp::Reverse(w.len()));
    let words: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
    vec![
        (ci(r"^no\b"), Category::Negate),
        (ci(&format!(r"\b(?:{})\b", words.join("|"))), Category::Keyword),
        (Regex::new(r"\b\d{1,3}(?:\.\d{1,3}){3}(?:/\d{1,2})?\b").unwrap(), Category::Value),
        (Regex::new(r"^\S+[>#]\s*$").unwrap(), Category::Prompt),
    ]
});

fn syntax_rules(syntax: &str) -> Option<&'static [Rule]> {
    match syntax {
        "cisco_ios" => Some(&CISCO_IOS),
        _ => None,
    }
}

/// A coloured span, in character positions of the text passed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub category: Category,
}

/// Coloured spans for one line of terminal text. Later rules win where
/// spans overlap, as in the Python version.
pub fn highlight_line(text: &str, syntax: &str) -> Vec<Span> {
    let Some(rules) = syntax_rules(syntax) else {
        return Vec::new();
    };
    // Trailing blanks are most of a terminal row and can never match.
    let text = text.trim_end();
    if text.trim_start().is_empty() {
        return Vec::new();
    }
    // Byte offset -> character index, for converting regex matches.
    let mut char_at = vec![0usize; text.len() + 1];
    let mut count = 0;
    for (byte, ch) in text.char_indices() {
        for slot in &mut char_at[byte..byte + ch.len_utf8()] {
            *slot = count;
        }
        count += 1;
    }
    char_at[text.len()] = count;

    let mut per_char: Vec<Option<Category>> = vec![None; count];
    for (pattern, category) in rules {
        for m in pattern.find_iter(text) {
            for slot in &mut per_char[char_at[m.start()]..char_at[m.end()]] {
                *slot = Some(*category);
            }
        }
    }

    let mut spans: Vec<Span> = Vec::new();
    for (i, cat) in per_char.into_iter().enumerate() {
        let Some(category) = cat else { continue };
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

    fn words(text: &str) -> Vec<(String, Category)> {
        let chars: Vec<char> = text.chars().collect();
        highlight_line(text, "cisco_ios")
            .into_iter()
            .map(|s| (chars[s.start..s.end].iter().collect(), s.category))
            .collect()
    }

    #[test]
    fn none_syntax_highlights_nothing() {
        assert!(highlight_line("show ip interface brief", "none").is_empty());
        assert!(highlight_line("show ip", "unknown").is_empty());
    }

    #[test]
    fn keywords_and_addresses() {
        assert_eq!(
            words("ip address 192.168.1.1/24 now"),
            [
                ("ip".into(), Category::Keyword),
                ("address".into(), Category::Keyword),
                ("192.168.1.1/24".into(), Category::Value),
            ]
        );
    }

    #[test]
    fn keyword_match_is_case_insensitive_and_whole_word() {
        assert_eq!(words("SHOW Version"), [("SHOW".into(), Category::Keyword), ("Version".into(), Category::Keyword)]);
        assert!(words("showing endless").is_empty());
    }

    #[test]
    fn hyphenated_keyword_wins_over_its_prefix() {
        assert_eq!(words("access-list 10"), [("access-list".into(), Category::Keyword)]);
    }

    #[test]
    fn negation_and_prompt() {
        assert_eq!(words("no shutdown")[0], ("no".into(), Category::Negate));
        assert_eq!(words("Switch(config-if)#"), [("Switch(config-if)#".into(), Category::Prompt)]);
        assert_eq!(words("router>  "), [("router>".into(), Category::Prompt)]);
    }

    #[test]
    fn positions_are_characters_not_bytes() {
        let spans = highlight_line("é show", "cisco_ios");
        assert_eq!(spans, [Span { start: 2, end: 6, category: Category::Keyword }]);
    }

    #[test]
    fn blank_line() {
        assert!(highlight_line("      ", "cisco_ios").is_empty());
    }
}
