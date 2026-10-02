//! Continue paged show output, one Space per prompt. Only enabled sessions
//! and commands the user submitted can start it; ordinary output can't.

use std::time::Duration;

use alacritty_terminal::vte::{Params, Parser, Perform};

/// Let the rest of a fragmented line arrive before treating it as a prompt.
pub const PROMPT_WAIT: Duration = Duration::from_millis(60);
const LINE_LIMIT: usize = 512;

#[derive(Default)]
pub struct AutoPager {
    enabled: bool,
    active: bool,
    input_parser: Parser,
    input: CommandLine,
    output_parser: Parser,
    output: OutputLine,
    prompt: Option<String>,
    ticket: u64,
    pending: Option<(u64, u64)>,
    answered: Option<u64>,
}

impl AutoPager {
    pub fn configure(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.reset();
            self.enabled = enabled;
        }
    }

    /// Reconnects, closing a session and disabling paging invalidate timers.
    pub fn reset(&mut self) {
        self.active = false;
        self.pending = None;
        self.answered = None;
        self.input = CommandLine::default();
        self.input_parser = Parser::new();
        self.output = OutputLine::default();
        self.output_parser = Parser::new();
        self.prompt = None;
    }

    /// Observe user input before it goes to the device. The visible command
    /// is a fallback for history recall and completion, which the device edits.
    pub fn sent(&mut self, bytes: &[u8], visible: &str) {
        if !self.enabled || bytes.is_empty() {
            return;
        }
        if !self.active
            && is_more_text(visible)
            && matches!(bytes, b"q" | b"Q" | b" " | b"\r" | b"\n" | b"\r\n" | b"\x03" | b"\x1b")
        {
            // Enabling the setting while an older, manual listing is
            // paused mustn't count its pager keys as the next command.
            self.reset();
            return;
        }
        if self.active {
            if matches!(bytes, b" " | b"\r" | b"\n" | b"\r\n") && self.output.is_more() {
                // The user advanced this page before the timer did.
                self.pending = None;
                self.answered = Some(self.output.progress);
                return;
            }
            self.reset();
            if bytes.iter().any(|&b| matches!(b, b'q' | b'Q' | 0x03 | 0x1b)) {
                return;
            }
        }
        self.input.submitted = None;
        self.input.submissions = 0;
        // VT ignores DEL, but Cisco uses it as the default Backspace key.
        for part in bytes.split_inclusive(|&byte| byte == 0x7f) {
            if let Some(rest) = part.strip_suffix(&[0x7f]) {
                self.input_parser.advance(&mut self.input, rest);
                self.input.execute(0x08);
            } else {
                self.input_parser.advance(&mut self.input, part);
            }
        }
        let Some((typed, uncertain)) = self.input.submitted.take() else { return };
        // A paste queuing several commands isn't one interactive listing.
        if self.input.submissions != 1 {
            return;
        }
        let (prompt, shown) = visible_command(visible);
        let command = if uncertain || typed.trim().is_empty() { shown } else { typed.trim() };
        if is_show(command) {
            self.active = true;
            self.prompt = prompt.map(str::to_owned);
            self.output = OutputLine::default();
            self.output_parser = Parser::new();
            self.pending = None;
            self.answered = None;
        }
    }

    /// A new candidate schedules one delayed check. ANSI decoration and
    /// duplicate redraws of the same prompt don't schedule extra Spaces.
    pub fn received(&mut self, bytes: &[u8], alt_screen: bool) -> Option<u64> {
        if !self.enabled || !self.active {
            return None;
        }
        if alt_screen {
            self.reset();
            return None;
        }
        self.output_parser.advance(&mut self.output, bytes);
        let text = self.output.text();
        if self.output.newlines > 0 && self.prompt.as_deref().is_some_and(|prompt| text.trim() == prompt) {
            self.reset();
            return None;
        }
        if !self.output.is_more() {
            self.pending = None;
            return None;
        }
        let revision = self.output.revision;
        if self.answered == Some(self.output.progress) || self.pending.is_some_and(|(_, seen)| seen == revision) {
            return None;
        }
        self.ticket = self.ticket.wrapping_add(1);
        self.pending = Some((self.ticket, revision));
        Some(self.ticket)
    }

    /// Called with the session's locks held before sending the Space, so
    /// cancelling, manual input or a newer connection can't race the reply.
    pub fn advance(&mut self, ticket: u64) -> bool {
        if !self.enabled || !self.active || !self.output.is_more() {
            return false;
        }
        let Some((pending, revision)) = self.pending else { return false };
        if pending != ticket || revision != self.output.revision {
            return false;
        }
        self.pending = None;
        self.answered = Some(self.output.progress);
        true
    }
}

fn visible_command(line: &str) -> (Option<&str>, &str) {
    let line = line.trim();
    match line.find(['#', '>']) {
        Some(end) => (Some(&line[..end + 1]), line[end + 1..].trim()),
        None => (None, line),
    }
}

fn is_show(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let mut first = words.next().unwrap_or_default();
    if first.eq_ignore_ascii_case("do") {
        first = words.next().unwrap_or_default();
    }
    ["sh", "sho", "show"].iter().any(|word| first.eq_ignore_ascii_case(word))
}

fn is_more_text(text: &str) -> bool {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).flat_map(|c| c.to_lowercase()).collect();
    matches!(text.as_str(), "--more--" | "---more---" | "<---more--->")
}

/// A bounded view of the last output line. The existing VT parser keeps
/// escape sequences, OSC text and split UTF-8 out of the prompt detector.
#[derive(Default)]
struct OutputLine {
    cells: Vec<char>,
    column: usize,
    overflow: bool,
    revision: u64,
    progress: u64,
    newlines: usize,
}

impl OutputLine {
    fn text(&self) -> String {
        self.cells.iter().collect()
    }

    fn clear(&mut self) {
        self.finish_line();
        self.cells.clear();
        self.column = 0;
        self.overflow = false;
        self.revision = self.revision.wrapping_add(1);
    }

    fn finish_line(&mut self) {
        let text: String = self.cells.iter().filter(|c| !c.is_whitespace()).flat_map(|c| c.to_lowercase()).collect();
        let marker = ["--more--", "---more---", "<---more--->"].iter().any(|candidate| candidate.starts_with(&text));
        if !text.is_empty() && !marker {
            self.progress = self.progress.wrapping_add(1);
        }
    }

    fn is_more(&self) -> bool {
        if self.overflow {
            return false;
        }
        is_more_text(&self.text())
    }
}

impl Perform for OutputLine {
    fn print(&mut self, c: char) {
        if self.column >= LINE_LIMIT {
            self.overflow = true;
            return;
        }
        self.cells.resize(self.cells.len().max(self.column + 1), ' ');
        if self.cells[self.column] != c {
            self.cells[self.column] = c;
            self.revision = self.revision.wrapping_add(1);
        }
        self.column += 1;
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.column = 0,
            b'\n' => {
                self.clear();
                self.progress = self.progress.wrapping_add(1);
                self.newlines = self.newlines.saturating_add(1);
            }
            0x08 => self.column = self.column.saturating_sub(1),
            b'\t' => self.column = ((self.column / 8 + 1) * 8).min(LINE_LIMIT),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        let value = usize::from(params.iter().next().and_then(|p| p.first()).copied().unwrap_or(0));
        match action {
            'K' => {
                self.finish_line();
                match value {
                    0 => self.cells.truncate(self.column),
                    1 => {
                        let end = (self.column + 1).min(self.cells.len());
                        self.cells[..end].fill(' ');
                    }
                    2 => self.cells.clear(),
                    _ => return,
                }
                self.overflow = false;
                self.revision = self.revision.wrapping_add(1);
            }
            'J' | 'H' | 'f' => self.clear(),
            'G' => self.column = value.max(1).saturating_sub(1).min(LINE_LIMIT),
            'D' => self.column = self.column.saturating_sub(value.max(1)),
            'C' => self.column = (self.column + value.max(1)).min(LINE_LIMIT),
            _ => {}
        }
    }
}

/// Enough local editing to recognise a command even when its remote echo
/// is still in flight. Device-managed edits fall back to the live screen.
#[derive(Default)]
struct CommandLine {
    cells: Vec<char>,
    column: usize,
    uncertain: bool,
    cr: bool,
    submitted: Option<(String, bool)>,
    submissions: usize,
}

impl Perform for CommandLine {
    fn print(&mut self, c: char) {
        self.cr = false;
        if self.cells.len() < LINE_LIMIT {
            self.cells.insert(self.column.min(self.cells.len()), c);
            self.column += 1;
        } else {
            self.uncertain = true;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' | b'\n' => {
                if byte == b'\n' && self.cr {
                    self.cr = false;
                    return;
                }
                self.submitted = Some((self.cells.iter().collect(), self.uncertain));
                self.submissions += 1;
                self.cells.clear();
                self.column = 0;
                self.uncertain = false;
                self.cr = byte == b'\r';
            }
            0x08 | 0x7f => {
                if self.column > 0 {
                    self.column -= 1;
                    self.cells.remove(self.column);
                }
            }
            0x01 => self.column = 0,
            0x05 => self.column = self.cells.len(),
            0x03 | 0x15 => {
                self.cells.clear();
                self.column = 0;
                self.uncertain = false;
            }
            0x17 => {
                while self.column > 0 && self.cells[self.column - 1].is_whitespace() {
                    self.column -= 1;
                    self.cells.remove(self.column);
                }
                while self.column > 0 && !self.cells[self.column - 1].is_whitespace() {
                    self.column -= 1;
                    self.cells.remove(self.column);
                }
            }
            b'\t' | 0x10 | 0x0e => self.uncertain = true,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {
        self.uncertain = true;
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {
        self.uncertain = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn showing(command: &[u8]) -> AutoPager {
        let mut pager = AutoPager::default();
        pager.configure(true);
        pager.sent(command, "Switch#");
        assert!(pager.active);
        pager
    }

    #[test]
    fn disabled_and_other_commands_never_advance() {
        let mut pager = AutoPager::default();
        pager.sent(b"show run\r", "Switch#show run");
        assert_eq!(pager.received(b"\r\n--More--", false), None);
        pager.configure(true);
        for command in ["configure terminal", "more file", "showcase", "ssh other", "s"] {
            pager.sent(format!("{command}\r").as_bytes(), "Switch#");
            assert_eq!(pager.received(b"\r\n--More--", false), None, "{command}");
        }
    }

    #[test]
    fn every_show_command_and_its_abbreviations_can_page() {
        for command in [
            "show run",
            "show running-config",
            "show interfaces",
            "show ip route",
            "sh ver",
            "sho log",
            "do show run",
            "SHOW RUN",
        ] {
            let mut pager = showing(format!("{command}\r\n").as_bytes());
            let ticket = pager.received(b"\r\ninterface Vlan1\r\n--More--", false).unwrap();
            assert!(pager.advance(ticket), "{command}");
            assert!(!pager.advance(ticket), "answered twice: {command}");
        }
    }

    #[test]
    fn split_colored_prompts_advance_once_per_page() {
        for marker in ["--More--", "---More---", "<--- More --->"] {
            let mut pager = showing(b"sh run\r");
            let bytes = format!("\r\nline one\r\n\x1b[7m{marker}\x1b[0m");
            let mut ticket = None;
            for byte in bytes.bytes() {
                ticket = pager.received(&[byte], false).or(ticket);
            }
            assert!(pager.advance(ticket.unwrap()), "{marker}");
            assert_eq!(pager.received(b"\x1b[7m\x1b[0m", false), None);
            assert_eq!(pager.received(format!("\r{marker}").as_bytes(), false), None);
            assert_eq!(pager.received(format!("\r\x1b[2K{marker}").as_bytes(), false), None);
            let next = pager.received(format!("\r\x1b[Kline two\r\n{marker}").as_bytes(), false).unwrap();
            assert!(pager.advance(next));
            pager.received(b"\r\x1b[Kend\r\nSwitch#", false);
            assert!(!pager.active);
            assert_eq!(pager.received(bytes.as_bytes(), false), None);
        }
    }

    #[test]
    fn ordinary_text_and_transient_markers_do_not_send_spaces() {
        let mut pager = showing(b"show run\r");
        for bytes in [
            b"\r\n description --More--".as_slice(),
            b"\r\n--More--\r\n",
            b"\r\n\x1b]0;--More--\x07",
            b"\r\nconfirm [yes/no]:",
        ] {
            assert_eq!(pager.received(bytes, false), None, "{bytes:?}");
        }
        let ticket = pager.received(b"\r\n--More--", false).unwrap();
        pager.received(b"\r\nnot a pause", false);
        assert!(!pager.advance(ticket));
    }

    #[test]
    fn cancellation_manual_paging_and_reconnect_invalidate_the_timer() {
        for cancel in [b"q".as_slice(), b"\x03", b"\x1b"] {
            let mut pager = showing(b"show run\r");
            let ticket = pager.received(b"\r\n--More--", false).unwrap();
            pager.sent(cancel, "--More--");
            assert!(!pager.advance(ticket));
            assert!(!pager.active);
        }
        for manual in [b" ".as_slice(), b"\r"] {
            let mut pager = showing(b"show run\r");
            let ticket = pager.received(b"\r\n--More--", false).unwrap();
            pager.sent(manual, "--More--");
            assert!(!pager.advance(ticket));
            assert!(pager.active);
            assert_eq!(pager.received(b"\x1b[0m", false), None);
        }
        let mut pager = showing(b"show run\r");
        let ticket = pager.received(b"\r\n--More--", false).unwrap();
        pager.configure(false);
        assert!(!pager.advance(ticket));
        pager.configure(true);
        pager.sent(b"show ip route\r", "Switch#");
        let ticket = pager.received(b"\r\n--More--", false).unwrap();
        pager.reset();
        assert!(!pager.advance(ticket));
    }

    #[test]
    fn line_editing_history_and_pastes_recognise_the_submitted_command() {
        for bytes in [
            b"x\x08show run\r".as_slice(),
            b"x\x7fshow run\r",
            b"bad\x15sh ver\r",
            b"bad\x03show ip route\r",
            b"show bad\x17run\r",
        ] {
            showing(bytes);
        }
        let mut pager = AutoPager::default();
        pager.configure(true);
        pager.sent(b"\x1b[A", "Switch#");
        pager.sent(b"\r", "Switch#show interfaces");
        assert!(pager.active);
        pager.reset();
        pager.sent(b"show run\rshow ip route\r", "Switch#");
        assert!(!pager.active);
        let mut pager = showing(b"show run\r");
        assert_eq!(pager.received(b"\x1b[?1049h--More--", true), None);
        assert!(!pager.active);
    }

    #[test]
    fn enabling_during_a_manual_listing_does_not_pollute_the_next_command() {
        let mut pager = AutoPager::default();
        pager.sent(b"show run\r", "Switch#");
        pager.configure(true);
        pager.sent(b"q", "--More--");
        pager.received(b"\r\x1b[KSwitch#", false);
        pager.sent(b"show interfaces\r", "Switch#");
        let ticket = pager.received(b"\r\nGi1/0/1 is up\r\n--More--", false).unwrap();
        assert!(pager.advance(ticket));
    }
}
