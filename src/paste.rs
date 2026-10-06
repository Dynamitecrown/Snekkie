//! Reviewed, connection-bound paste queues. Timers never catch up in bursts.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::terminal::keys;

pub const DEFAULT_DELAY_MS: u64 = 100;
pub const MAX_DELAY_MS: u64 = 5000;

#[derive(Clone, Debug)]
pub struct PasteTarget {
    pub id: u64,
    pub generation: u64,
    pub description: String,
}

/// Normalize for the editable preview without dropping empty or final lines.
pub fn preview_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn multiline(text: &str) -> bool {
    text.contains(['\r', '\n'])
}

pub fn line_count(text: &str) -> usize {
    keys::encode_paste(text).split_inclusive(|b| *b == b'\r').count()
}

pub struct PasteQueue {
    pub target: PasteTarget,
    lines: VecDeque<Vec<u8>>,
    pub total: usize,
    pub submitted: usize,
    delay: Duration,
    due: Instant,
}

impl PasteQueue {
    pub fn new(target: PasteTarget, text: &str, delay_ms: u64, now: Instant) -> Self {
        let bytes = keys::encode_paste(text);
        let lines: VecDeque<_> = bytes.split_inclusive(|b| *b == b'\r').map(<[u8]>::to_vec).collect();
        Self {
            target,
            total: lines.len(),
            lines,
            submitted: 0,
            delay: Duration::from_millis(delay_ms.min(MAX_DELAY_MS)),
            due: now,
        }
    }

    /// Submit at most one line per tick; a delayed frame never drains a backlog.
    pub fn next_line(&mut self, now: Instant) -> Option<Vec<u8>> {
        if now < self.due {
            return None;
        }
        let line = self.lines.pop_front()?;
        self.submitted += 1;
        self.due = now + self.delay;
        Some(line)
    }

    pub fn remaining_delay(&self, now: Instant) -> Duration {
        self.due.saturating_duration_since(now)
    }

    pub fn complete(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> PasteTarget {
        PasteTarget { id: 12, generation: 3, description: "Lab switch".into() }
    }

    #[test]
    fn preserves_exact_manual_paste_bytes_and_empty_lines() {
        for text in ["", "show version", "a\r\nb\nc\r\r", "\n\n", "café 界\nlast", "a\n", "\ra"] {
            let now = Instant::now();
            let mut queue = PasteQueue::new(target(), text, 0, now);
            assert_eq!(queue.total, line_count(text));
            let mut result = Vec::new();
            while let Some(line) = queue.next_line(now) {
                result.extend(line);
            }
            assert_eq!(result, keys::encode_paste(text));
            assert!(queue.complete());
            assert_eq!(keys::encode_paste(&preview_text(text)), result);
        }
    }

    #[test]
    fn pacing_waits_between_lines_and_does_not_catch_up() {
        let now = Instant::now();
        let mut queue = PasteQueue::new(target(), "one\ntwo\nthree", 100, now);
        assert_eq!(queue.next_line(now).unwrap(), b"one\r");
        assert!(queue.next_line(now + Duration::from_millis(99)).is_none());
        let late = now + Duration::from_secs(5);
        assert_eq!(queue.next_line(late).unwrap(), b"two\r");
        assert!(queue.next_line(late).is_none());
        assert_eq!(queue.next_line(late + Duration::from_millis(100)).unwrap(), b"three");
        assert!(queue.complete());
    }
}
