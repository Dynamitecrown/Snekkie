//! Helpers shared by the integration tests.

#![allow(dead_code)]

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use snekkie::transport::Sink;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Connected,
    Closed(Option<String>),
    Notice(String),
}

/// A Sink that records everything, with helpers to wait for it.
#[derive(Default)]
pub struct Recorder {
    state: Mutex<(Vec<u8>, Vec<Event>)>,
    changed: Condvar,
}

impl Recorder {
    pub fn new() -> Arc<Recorder> {
        Arc::new(Recorder::default())
    }

    fn wait_for<T>(&self, timeout: Duration, mut check: impl FnMut(&(Vec<u8>, Vec<Event>)) -> Option<T>) -> T {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(found) = check(&state) {
                return found;
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "timed out; data so far: {:?}; events: {:?}",
                String::from_utf8_lossy(&state.0),
                state.1
            );
            state = self.changed.wait_timeout(state, deadline - now).unwrap().0;
        }
    }

    pub fn wait_event(&self, timeout: Duration, pred: impl Fn(&Event) -> bool) -> Event {
        self.wait_for(timeout, |(_, events)| events.iter().find(|e| pred(e)).cloned())
    }

    pub fn wait_text(&self, timeout: Duration, needle: &str) {
        self.wait_for(timeout, |(data, _)| String::from_utf8_lossy(data).contains(needle).then_some(()))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.state.lock().unwrap().0).into_owned()
    }

    pub fn events(&self) -> Vec<Event> {
        self.state.lock().unwrap().1.clone()
    }

    fn push(&self, f: impl FnOnce(&mut (Vec<u8>, Vec<Event>))) {
        f(&mut self.state.lock().unwrap());
        self.changed.notify_all();
    }
}

impl Sink for Recorder {
    fn connected(&self) {
        self.push(|s| s.1.push(Event::Connected));
    }
    fn data(&self, bytes: &[u8]) {
        self.push(|s| s.0.extend_from_slice(bytes));
    }
    fn closed(&self, reason: Option<String>) {
        self.push(|s| s.1.push(Event::Closed(reason)));
    }
    fn notice(&self, message: String) {
        self.push(|s| s.1.push(Event::Notice(message)));
    }
}
