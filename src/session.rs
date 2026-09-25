//! One tab = one session = transport + emulator + optional log file.
//!
//! The transport's worker feeds the emulator directly (under a lock), so a
//! big `show run` is parsed off the UI thread and nothing piles up while the
//! window is minimised. The UI thread only locks the emulator to draw it.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::profiles::{Kind, Profile};
use crate::settings::Theme;
use crate::terminal::emulator::{Emulator, Responder};
use crate::transport::ssh::{Credentials, HostKeyAsker};
use crate::transport::{Command, Link, Sink, serial, ssh};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Connecting,
    Connected,
    /// Why it ended; `None` means the user closed it.
    Closed(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
}

/// Something to tell the user that doesn't end the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
}

/// State shared between the UI and the transport worker.
pub struct Shared {
    pub emulator: Mutex<Emulator>,
    state: Mutex<State>,
    log: Mutex<Option<File>>,
    notices: Mutex<Vec<Notice>>,
    /// Bumped on every (re)connect, so a worker from a previous connection
    /// can't write into the new one.
    generation: AtomicU64,
    repaint: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    fn notify(&self, level: NoticeLevel, text: String) {
        self.notices.lock().push(Notice { level, text });
        (self.repaint)();
    }
}

struct SessionSink {
    shared: Arc<Shared>,
    generation: u64,
}

impl Sink for SessionSink {
    fn connected(&self) {
        if self.shared.is_current(self.generation) {
            *self.shared.state.lock() = State::Connected;
            (self.shared.repaint)();
        }
    }

    fn data(&self, bytes: &[u8]) {
        if !self.shared.is_current(self.generation) {
            return;
        }
        if let Some(log) = self.shared.log.lock().as_mut() {
            // Losing the log shouldn't kill the session.
            let _ = log.write_all(bytes).and_then(|_| log.flush());
        }
        self.shared.emulator.lock().feed(bytes);
        (self.shared.repaint)();
    }

    fn closed(&self, reason: Option<String>) {
        if !self.shared.is_current(self.generation) {
            return;
        }
        let mut state = self.shared.state.lock();
        if !matches!(*state, State::Closed(_)) {
            *state = State::Closed(reason);
        }
        drop(state);
        (self.shared.repaint)();
    }

    fn notice(&self, message: String) {
        if !self.shared.is_current(self.generation) {
            return;
        }
        let level = if message == "Break sent" { NoticeLevel::Info } else { NoticeLevel::Warning };
        self.shared.notify(level, message);
    }
}

/// What a connection needs besides the profile.
pub struct ConnectContext<'a> {
    pub runtime: &'a tokio::runtime::Handle,
    pub ask_host_key: HostKeyAsker,
    pub password: String,
    pub key_passphrase: String,
}

pub struct Session {
    pub id: u64,
    pub profile: Profile,
    pub shared: Arc<Shared>,
    /// The current transport, also used by the emulator to answer queries.
    link: Arc<Mutex<Option<Link>>>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl Session {
    pub fn new(profile: Profile, theme: Theme, repaint: impl Fn() + Send + Sync + 'static) -> Session {
        let link: Arc<Mutex<Option<Link>>> = Arc::new(Mutex::new(None));
        let responder: Responder = {
            let link = link.clone();
            Arc::new(move |bytes| {
                if let Some(link) = link.lock().as_ref() {
                    link.write(bytes);
                }
            })
        };
        let mut emulator = Emulator::new(80, 24, profile.scrollback as usize, responder);
        emulator.set_theme(theme);
        let shared = Arc::new(Shared {
            emulator: Mutex::new(emulator),
            state: Mutex::new(State::Connecting),
            log: Mutex::new(None),
            notices: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
            repaint: Box::new(repaint),
        });
        Session { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), profile, shared, link }
    }

    /// Start (or restart) the connection.
    pub fn connect(&mut self, cx: ConnectContext<'_>) {
        if let Some(old) = self.link.lock().take() {
            old.send(Command::Close);
        }
        let generation = self.shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.shared.state.lock() = State::Connecting;
        self.open_log();

        let sink: Arc<dyn Sink> = Arc::new(SessionSink { shared: self.shared.clone(), generation });
        let link = match self.profile.kind() {
            Kind::Serial => serial::start(self.profile.clone(), sink),
            Kind::Ssh => {
                let size = {
                    let emu = self.shared.emulator.lock();
                    (emu.columns() as u16, emu.lines() as u16)
                };
                let credentials = Credentials {
                    password: cx.password,
                    key_passphrase: cx.key_passphrase,
                    known_hosts: ssh::known_hosts_path(),
                };
                ssh::start(cx.runtime, self.profile.clone(), credentials, size, cx.ask_host_key, sink)
            }
        };
        *self.link.lock() = Some(link);
    }

    fn open_log(&mut self) {
        let path = self.profile.log_path.trim();
        let mut log = self.shared.log.lock();
        *log = None;
        if path.is_empty() {
            return;
        }
        let path = Path::new(path);
        let opened = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| OpenOptions::new().create(true).append(true).open(path));
        match opened {
            Ok(file) => *log = Some(file),
            Err(e) => {
                drop(log);
                self.shared.notify(NoticeLevel::Warning, format!("Could not open log file: {e}"));
            }
        }
    }

    pub fn state(&self) -> State {
        self.shared.state.lock().clone()
    }

    pub fn is_connected(&self) -> bool {
        self.state() == State::Connected
    }

    /// Connected or still trying: closing it would cut something off.
    pub fn is_live(&self) -> bool {
        !matches!(self.state(), State::Closed(_))
    }

    pub fn take_notices(&self) -> Vec<Notice> {
        std::mem::take(&mut *self.shared.notices.lock())
    }

    /// Send what the user typed. Dropped unless connected.
    pub fn write(&self, bytes: Vec<u8>) {
        if self.is_connected()
            && let Some(link) = self.link.lock().as_ref()
        {
            link.write(bytes);
        }
    }

    pub fn resize(&self, columns: usize, lines: usize) {
        if let Some(link) = self.link.lock().as_ref() {
            link.send(Command::Resize { columns: columns as u16, lines: lines as u16 });
        }
    }

    pub fn send_break(&self) {
        match (self.profile.kind(), self.state()) {
            (Kind::Ssh, _) => self.shared.notify(NoticeLevel::Warning, "SSH sessions do not support break".into()),
            (Kind::Serial, State::Connected) => {
                if let Some(link) = self.link.lock().as_ref() {
                    link.send(Command::Break);
                }
            }
            (Kind::Serial, _) => self.shared.notify(NoticeLevel::Warning, "Not connected".into()),
        }
    }

    /// Hang up. Safe to call more than once.
    pub fn shutdown(&self) {
        if let Some(link) = self.link.lock().take() {
            link.send(Command::Close);
        }
        let mut state = self.shared.state.lock();
        if !matches!(*state, State::Closed(_)) {
            *state = State::Closed(None);
        }
        drop(state);
        *self.shared.log.lock() = None;
    }

    pub fn description(&self) -> String {
        match self.profile.kind() {
            Kind::Ssh => ssh::description(&self.profile),
            Kind::Serial => serial::description(&self.profile),
        }
    }

    pub fn title(&self) -> String {
        match self.state() {
            State::Closed(_) => format!("{} (closed)", self.profile.name),
            _ => self.profile.name.clone(),
        }
    }

    pub fn status_text(&self) -> String {
        let state = match self.state() {
            State::Connecting => "connecting",
            State::Connected => "connected",
            State::Closed(_) => "disconnected",
        };
        let emu = self.shared.emulator.lock();
        format!("{}   [{state}]   {}x{}", self.description(), emu.columns(), emu.lines())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serialport::{SerialPort, TTYPort};
    use std::io::Read;
    use std::time::{Duration, Instant};

    fn wait_until(mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn serial_session(log_path: &str) -> (Session, TTYPort, tokio::runtime::Runtime) {
        let (mut master, slave) = TTYPort::pair().unwrap();
        let device = slave.name().unwrap();
        std::mem::forget(slave);
        master.set_timeout(Duration::from_millis(100)).unwrap();
        let profile = Profile {
            name: "lab".into(),
            kind: "serial".into(),
            device,
            log_path: log_path.into(),
            ..Profile::default()
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut session = Session::new(profile, Theme::default(), || {});
        session.connect(ConnectContext {
            runtime: runtime.handle(),
            ask_host_key: Arc::new(|_| {}),
            password: String::new(),
            key_passphrase: String::new(),
        });
        wait_until(|| session.is_connected());
        (session, master, runtime)
    }

    #[test]
    fn output_reaches_the_screen_and_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("logs").join("lab.log");
        let (session, mut master, _rt) = serial_session(log.to_str().unwrap());
        std::io::Write::write_all(&mut master, b"Switch#").unwrap();
        wait_until(|| session.shared.emulator.lock().line_text(0) == "Switch#");
        assert_eq!(std::fs::read(&log).unwrap(), b"Switch#");
        assert_eq!(session.status_text(), format!("{}   [connected]   80x24", session.description()));
    }

    #[test]
    fn device_queries_are_answered_over_the_link() {
        let (session, mut master, _rt) = serial_session("");
        // Cursor position report request.
        std::io::Write::write_all(&mut master, b"\x1b[6n").unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 32];
        let deadline = Instant::now() + Duration::from_secs(5);
        while !got.ends_with(b"R") && Instant::now() < deadline {
            if let Ok(n) = master.read(&mut buf) {
                got.extend_from_slice(&buf[..n]);
            }
        }
        assert_eq!(got, b"\x1b[1;1R");
        drop(session);
    }

    #[test]
    fn shutdown_marks_the_session_closed_on_purpose() {
        let (session, _master, _rt) = serial_session("");
        session.shutdown();
        assert_eq!(session.state(), State::Closed(None));
        assert_eq!(session.title(), "lab (closed)");
        assert!(!session.is_live());
    }

    #[test]
    fn break_on_ssh_is_explained() {
        let session = Session::new(Profile::default(), Theme::default(), || {});
        session.send_break();
        let notices = session.take_notices();
        assert_eq!(notices[0].text, "SSH sessions do not support break");
        assert!(session.take_notices().is_empty());
    }

    #[test]
    fn unwritable_log_is_reported_but_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        // A path "inside" a regular file can't be created.
        let (session, _master, _rt) = serial_session(blocker.join("x.log").to_str().unwrap());
        let notices = session.take_notices();
        assert!(notices.iter().any(|n| n.text.starts_with("Could not open log file")), "{notices:?}");
        assert!(session.is_connected());
    }
}
