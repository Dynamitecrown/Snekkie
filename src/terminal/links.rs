//! Finding a web link or an IP address in a line of terminal output, for
//! Ctrl+click (Cmd+click on macOS).

use std::net::{Ipv4Addr, Ipv6Addr};
use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// An http or https address to open in the browser.
    Url(String),
    /// An IPv4 or IPv6 address to start a new session to.
    Host(String),
}

/// The link covering character `at` of `text`, with its range in characters.
pub fn find(text: &str, at: usize) -> Option<(Range<usize>, Link)> {
    static URL: OnceLock<Regex> = OnceLock::new();
    static ADDRESS: OnceLock<Regex> = OnceLock::new();
    let url = URL.get_or_init(|| Regex::new(r#"https?://[^\s<>"'`]+"#).unwrap());
    let address = ADDRESS.get_or_init(|| Regex::new(r"[0-9A-Fa-f:.]+").unwrap());
    let byte = text.char_indices().nth(at)?.0;
    let chars = |range: Range<usize>| text[..range.start].chars().count()..text[..range.end].chars().count();

    for found in url.find_iter(text) {
        // Sentence punctuation after a link isn't part of it.
        let link = found.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
        let range = found.start()..found.start() + link.len();
        if range.contains(&byte) && link.len() > "https://".len() {
            return Some((chars(range), Link::Url(link.to_string())));
        }
    }
    for found in address.find_iter(text) {
        if !found.range().contains(&byte) {
            continue;
        }
        let candidate = found.as_str();
        let lead = candidate.len() - candidate.trim_start_matches([':', '.']).len();
        for host in [candidate, candidate.trim_matches([':', '.'])] {
            let start = found.start() + if host.len() == candidate.len() { 0 } else { lead };
            let range = start..start + host.len();
            // Not part of a longer word, such as a hex string or a name.
            let before = text[..range.start].chars().next_back();
            let after = text[range.end..].chars().next();
            if [before, after].into_iter().flatten().any(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let valid =
                host.parse::<Ipv4Addr>().is_ok() || host.parse::<Ipv6Addr>().is_ok_and(|ip| !ip.is_unspecified());
            if valid && range.contains(&byte) {
                return Some((chars(range), Link::Host(host.to_string())));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(text: &str, at: &str) -> Option<Link> {
        let at = text[..text.find(at).unwrap()].chars().count();
        find(text, at).map(|(_, link)| link)
    }

    #[test]
    fn urls_trim_trailing_punctuation() {
        let text = "See https://example.com/docs?a=1. Then (http://10.0.0.1/x).";
        assert_eq!(link(text, "example"), Some(Link::Url("https://example.com/docs?a=1".into())));
        assert_eq!(link(text, "10.0"), Some(Link::Url("http://10.0.0.1/x".into())));
        assert_eq!(link(text, "Then"), None);
        assert_eq!(find("é https://a.b", 4), Some((2..13, Link::Url("https://a.b".into()))));
    }

    #[test]
    fn ipv4_and_ipv6_addresses() {
        let text = "Gi0/1 10.1.2.3/24 via 192.168.1.254, peer 2001:db8::1. ll fe80::1%eth0";
        assert_eq!(link(text, "10.1"), Some(Link::Host("10.1.2.3".into())));
        assert_eq!(link(text, "168"), Some(Link::Host("192.168.1.254".into())));
        assert_eq!(link(text, "db8"), Some(Link::Host("2001:db8::1".into())));
        assert_eq!(link(text, "fe80"), Some(Link::Host("fe80::1".into())));
        assert_eq!(link("lo ::1", "::1"), Some(Link::Host("::1".into())));
        assert_eq!(link("ip:10.0.0.9 up", "10."), Some(Link::Host("10.0.0.9".into())));
    }

    #[test]
    fn look_alikes_are_not_addresses() {
        for (text, at) in [
            ("uptime 12:30:45", "12"),
            ("mac aa:bb:cc:dd:ee:ff", "aa"),
            ("version 15.2.4.300 build", "15"),
            ("hash deadbeef:cafe::1x", "dead"),
            ("v1.2.3.4", "1."),
            ("just ::", "::"),
        ] {
            assert_eq!(link(text, at), None, "{text}");
        }
    }
}
