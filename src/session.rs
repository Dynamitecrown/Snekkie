//! One tab = one session = transport + emulator + optional log file.
//!
//! The transport's worker feeds the emulator directly (under a lock), so a
//! big `show run` is parsed off the UI thread and nothing piles up while the
//! window is minimised. The UI thread only locks the emulator to draw it.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::profiles::{Kind, Profile};
use crate::settings::Theme;
use crate::terminal::emulator::{Emulator, Responder};
use crate::terminal::keys;
use crate::terminal::paging::{AutoPager, PROMPT_WAIT};
use crate::transport::ssh::{Credentials, HostKeyAsker};
use crate::transport::{Command, Link, Sink, serial, ssh, tcp};

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

/// Whether this session is currently recording output, including any failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogStatus {
    Off,
    Active,
    Failed(String),
}

enum SessionLog {
    Off,
    Active(Box<dyn Write + Send>),
    Failed,
}

impl SessionLog {
    fn stop(&mut self) -> bool {
        if matches!(self, Self::Active(_)) {
            *self = Self::Off;
            true
        } else {
            false
        }
    }
}

/// State shared between the UI and the transport worker.
pub struct Shared {
    pub emulator: Mutex<Emulator>,
    state: Mutex<State>,
    log: Mutex<SessionLog>,
    // UI status must never wait for a writer blocked on filesystem I/O.
    log_status: Mutex<LogStatus>,
    notices: Mutex<Vec<Notice>>,
    /// The far end has agreed to echo what's typed (telnet only).
    remote_echo: AtomicBool,
    pager: Mutex<AutoPager>,
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

    /// Put bytes on the screen and in the log.
    fn show(&self, bytes: &[u8]) {
        let failure = {
            let mut log = self.log.lock();
            if let SessionLog::Active(writer) = &mut *log {
                match writer.write_all(bytes).and_then(|_| writer.flush()) {
                    Ok(()) => None,
                    Err(error) => {
                        let message = format!("Session logging failed: {error}");
                        // Drop the failed writer; later output must not repeatedly warn.
                        *log = SessionLog::Failed;
                        *self.log_status.lock() = LogStatus::Failed(message.clone());
                        Some(message)
                    }
                }
            } else {
                None
            }
        };
        // Notice/repaint callbacks must not run while holding the log lock.
        if let Some(message) = failure {
            self.notify(NoticeLevel::Warning, message);
        }
        self.emulator.lock().feed(bytes);
        (self.repaint)();
    }
}

struct SessionSink {
    shared: Arc<Shared>,
    generation: u64,
    link: Arc<Mutex<Option<Link>>>,
    runtime: tokio::runtime::Handle,
}

impl Sink for SessionSink {
    fn connected(&self) {
        let mut state = self.shared.state.lock();
        if self.shared.is_current(self.generation) && !matches!(*state, State::Closed(_)) {
            *state = State::Connected;
            drop(state);
            (self.shared.repaint)();
        }
    }

    fn data(&self, bytes: &[u8]) {
        if self.shared.is_current(self.generation) {
            self.shared.show(bytes);
            let alt_screen = self.shared.emulator.lock().alt_screen();
            let ticket = self.shared.pager.lock().received(bytes, alt_screen);
            if let Some(ticket) = ticket {
                let shared = self.shared.clone();
                let link = self.link.clone();
                let generation = self.generation;
                self.runtime.spawn(async move {
                    tokio::time::sleep(PROMPT_WAIT).await;
                    let mut pager = shared.pager.lock();
                    let link = link.lock();
                    if shared.is_current(generation)
                        && *shared.state.lock() == State::Connected
                        && let Some(link) = link.as_ref()
                        && pager.advance(ticket)
                    {
                        // A pager response isn't typing or local echo.
                        link.write(vec![b' ']);
                    }
                });
            }
        }
    }

    fn closed(&self, reason: Option<String>) {
        if !self.shared.is_current(self.generation) {
            return;
        }
        {
            let mut pager = self.shared.pager.lock();
            if !self.shared.is_current(self.generation) {
                return;
            }
            pager.reset();
        }
        let mut state = self.shared.state.lock();
        if !self.shared.is_current(self.generation) {
            return;
        }
        if !matches!(*state, State::Closed(_)) {
            *state = State::Closed(reason);
        }
        drop(state);
        {
            let mut log = self.shared.log.lock();
            // Reconnecting may have replaced the log while this worker closed.
            if self.shared.is_current(self.generation) && log.stop() {
                *self.shared.log_status.lock() = LogStatus::Off;
            }
        }
        (self.shared.repaint)();
    }

    fn notice(&self, message: String) {
        if !self.shared.is_current(self.generation) {
            return;
        }
        let level = if message == "Break sent" { NoticeLevel::Info } else { NoticeLevel::Warning };
        self.shared.notify(level, message);
    }

    fn remote_echo(&self, on: bool) {
        if self.shared.is_current(self.generation) {
            self.shared.remote_echo.store(on, Ordering::SeqCst);
        }
    }
}

/// What a connection needs besides the profile.
pub struct ConnectContext<'a> {
    pub runtime: &'a tokio::runtime::Handle,
    pub ask_host_key: HostKeyAsker,
    pub password: String,
    pub key_passphrase: String,
    pub network: crate::network::NetworkAccess,
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
            log: Mutex::new(SessionLog::Off),
            log_status: Mutex::new(LogStatus::Off),
            notices: Mutex::new(Vec::new()),
            remote_echo: AtomicBool::new(false),
            pager: Mutex::new(AutoPager::default()),
            generation: AtomicU64::new(0),
            repaint: Box::new(repaint),
        });
        Session { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), profile, shared, link }
    }

    /// Start (or restart) the connection.
    pub fn connect(&mut self, cx: ConnectContext<'_>) {
        self.shared.pager.lock().reset();
        if let Some(old) = self.link.lock().take() {
            old.send(Command::Close);
        }
        let generation = self.shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.shared.state.lock() = State::Connecting;
        self.shared.remote_echo.store(false, Ordering::SeqCst);
        self.open_log();

        let sink: Arc<dyn Sink> = Arc::new(SessionSink {
            shared: self.shared.clone(),
            generation,
            link: self.link.clone(),
            runtime: cx.runtime.clone(),
        });
        let size = {
            let emu = self.shared.emulator.lock();
            (emu.columns() as u16, emu.lines() as u16)
        };
        let link = match self.profile.kind() {
            Kind::Serial => serial::start(self.profile.clone(), sink),
            Kind::Telnet | Kind::Raw => {
                tcp::start_with_access(cx.runtime, self.profile.clone(), size, sink, cx.network)
            }
            Kind::Ssh => {
                let credentials = Credentials {
                    password: cx.password,
                    key_passphrase: cx.key_passphrase,
                    known_hosts: ssh::known_hosts_path(),
                };
                ssh::start_with_access(
                    cx.runtime,
                    self.profile.clone(),
                    credentials,
                    size,
                    cx.ask_host_key,
                    sink,
                    cx.network,
                )
            }
        };
        *self.link.lock() = Some(link);
    }

    fn open_log(&mut self) {
        let path = self.profile.log_path.trim();
        let mut log = self.shared.log.lock();
        *log = SessionLog::Off;
        *self.shared.log_status.lock() = LogStatus::Off;
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
            Ok(file) => {
                *log = SessionLog::Active(Box::new(file));
                *self.shared.log_status.lock() = LogStatus::Active;
            }
            Err(e) => {
                let message = format!("Could not open log file: {e}");
                *log = SessionLog::Failed;
                *self.shared.log_status.lock() = LogStatus::Failed(message.clone());
                drop(log);
                self.shared.notify(NoticeLevel::Warning, message);
            }
        }
    }

    pub fn state(&self) -> State {
        self.shared.state.lock().clone()
    }

    pub fn log_status(&self) -> LogStatus {
        self.shared.log_status.lock().clone()
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

    /// Send what the user typed, and draw it too if the far end won't.
    /// Dropped unless connected.
    pub fn write(&self, bytes: Vec<u8>) {
        if !self.is_connected() {
            return;
        }
        let echo = self.echoes_locally().then(|| keys::local_echo(&bytes));
        let visible = {
            let emu = self.shared.emulator.lock();
            emu.cursor_line_text()
        };
        let mut pager = self.shared.pager.lock();
        pager.sent(&bytes, &visible);
        if let Some(link) = self.link.lock().as_ref() {
            link.write(bytes);
        }
        drop(pager);
        if let Some(echo) = echo.filter(|e| !e.is_empty()) {
            self.shared.show(&echo);
        }
    }

    pub fn echoes_locally(&self) -> bool {
        self.profile.local_echo().applies(self.profile.kind(), self.shared.remote_echo.load(Ordering::SeqCst))
    }

    /// Applies immediately, including cancelling any scheduled pager reply.
    pub fn set_auto_paging(&self, enabled: bool) {
        self.shared.pager.lock().configure(enabled);
    }

    pub fn resize(&self, columns: usize, lines: usize) {
        if let Some(link) = self.link.lock().as_ref() {
            link.send(Command::Resize { columns: columns as u16, lines: lines as u16 });
        }
    }

    pub fn send_break(&self) {
        match (self.profile.kind(), self.state()) {
            (Kind::Ssh | Kind::Raw, _) => {
                let text = format!("{} sessions do not support break", self.profile.kind().label());
                self.shared.notify(NoticeLevel::Warning, text);
            }
            (Kind::Serial | Kind::Telnet, State::Connected) => {
                if let Some(link) = self.link.lock().as_ref() {
                    link.send(Command::Break);
                }
            }
            (Kind::Serial | Kind::Telnet, _) => self.shared.notify(NoticeLevel::Warning, "Not connected".into()),
        }
    }

    /// Hang up. Safe to call more than once.
    pub fn shutdown(&self) {
        self.shared.pager.lock().reset();
        if let Some(link) = self.link.lock().take() {
            link.send(Command::Close);
        }
        let mut state = self.shared.state.lock();
        if !matches!(*state, State::Closed(_)) {
            *state = State::Closed(None);
        }
        drop(state);
        if self.shared.log.lock().stop() {
            *self.shared.log_status.lock() = LogStatus::Off;
        }
    }

    pub fn description(&self) -> String {
        match self.profile.kind() {
            Kind::Ssh => ssh::description(&self.profile),
            Kind::Telnet | Kind::Raw => tcp::description(&self.profile),
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

#[cfg(test)]
mod logging_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    fn wait_until(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    struct Console {
        session: Session,
        peer: TcpStream,
        listener: TcpListener,
        runtime: tokio::runtime::Runtime,
    }

    impl Console {
        fn new(path: &Path) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            let profile = Profile {
                kind: "raw".into(),
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                log_path: path.display().to_string(),
                ..Profile::default()
            };
            let mut session = Session::new(profile, Theme::default(), || {});
            assert_eq!(session.log_status(), LogStatus::Off, "a configured path is not an active writer");
            Self::connect(&mut session, &runtime);
            let (peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            wait_until(|| session.is_connected());
            Self { session, peer, listener, runtime }
        }

        fn connect(session: &mut Session, runtime: &tokio::runtime::Runtime) {
            session.connect(ConnectContext {
                runtime: runtime.handle(),
                ask_host_key: Arc::new(|_| {}),
                password: String::new(),
                key_passphrase: String::new(),
                network: Default::default(),
            });
        }

        fn reconnect(&mut self) {
            Self::connect(&mut self.session, &self.runtime);
            let (peer, _) = self.listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            self.peer = peer;
            wait_until(|| self.session.is_connected());
        }

        fn send(&mut self, bytes: &[u8], visible: &str) {
            self.peer.write_all(bytes).unwrap();
            wait_until(|| self.session.shared.emulator.lock().screen_text().contains(visible));
        }
    }

    #[derive(Default)]
    struct WriterState {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
        fail_write: bool,
        fail_flush: bool,
        dropped: bool,
    }

    struct InjectedWriter(Arc<Mutex<WriterState>>);

    impl Write for InjectedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let mut state = self.0.lock();
            state.writes += 1;
            if state.fail_write {
                return Err(std::io::Error::other("injected write failure"));
            }
            state.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let mut state = self.0.lock();
            state.flushes += 1;
            if state.fail_flush {
                return Err(std::io::Error::other("injected flush failure"));
            }
            Ok(())
        }
    }

    impl Drop for InjectedWriter {
        fn drop(&mut self) {
            self.0.lock().dropped = true;
        }
    }

    fn logging_failure_keeps_connection_alive(fail_write: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.log");
        let mut console = Console::new(&path);
        assert_eq!(console.session.log_status(), LogStatus::Active);
        console.send(b"Switch#", "Switch#");
        assert_eq!(std::fs::read(&path).unwrap(), b"Switch#");

        let writer = Arc::new(Mutex::new(WriterState::default()));
        *console.session.shared.log.lock() = SessionLog::Active(Box::new(InjectedWriter(writer.clone())));
        console.send(b" before", "Switch# before");
        assert_eq!(writer.lock().bytes, b" before");
        {
            let mut state = writer.lock();
            state.fail_write = fail_write;
            state.fail_flush = !fail_write;
        }
        console.send(b" failed", "Switch# before failed");
        let message =
            format!("Session logging failed: injected {} failure", if fail_write { "write" } else { "flush" });
        assert_eq!(console.session.log_status(), LogStatus::Failed(message.clone()));
        assert_eq!(console.session.take_notices(), vec![Notice { level: NoticeLevel::Warning, text: message }]);
        assert!(writer.lock().dropped, "the failed writer must close immediately");
        console.send(b" after", "Switch# before failed after");
        assert!(console.session.take_notices().is_empty());
        {
            let state = writer.lock();
            assert_eq!(state.writes, 2);
            assert_eq!(state.flushes, if fail_write { 1 } else { 2 });
            assert_eq!(state.bytes, if fail_write { b" before".as_slice() } else { b" before failed".as_slice() });
        }
        assert!(console.session.is_connected());
        console.session.write(b"show version\r".to_vec());
        let mut command = [0u8; 13];
        console.peer.read_exact(&mut command).unwrap();
        assert_eq!(&command, b"show version\r");

        console.session.shutdown();
        assert!(matches!(console.session.log_status(), LogStatus::Failed(_)));
        console.reconnect();
        assert_eq!(console.session.log_status(), LogStatus::Active);
        console.send(b" recovered", "recovered");
        assert_eq!(std::fs::read(&path).unwrap(), b"Switch# recovered");
    }

    #[test]
    fn write_failure_warns_once_without_interrupting_the_session() {
        logging_failure_keeps_connection_alive(true);
    }

    #[test]
    fn flush_failure_warns_once_without_interrupting_the_session() {
        logging_failure_keeps_connection_alive(false);
    }

    #[test]
    fn log_bytes_append_exactly_and_status_tracks_shutdown_and_remote_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("session.log");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"previous\r\n").unwrap();
        let mut console = Console::new(&path);
        let chunks: &[&[u8]] = &[b"\x1b[31mSwitch#\x1b[0m\r\ncaf\xc3", b"\xa9\xff\r\n"];
        console.send(chunks[0], "Switch#");
        console.send(chunks[1], "caf\u{e9}");
        let expected = [b"previous\r\n".as_slice(), chunks[0], chunks[1]].concat();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert_eq!(console.session.log_status(), LogStatus::Active);
        console.session.shutdown();
        assert_eq!(console.session.log_status(), LogStatus::Off);
        console.reconnect();
        assert_eq!(console.session.log_status(), LogStatus::Active);
        console.peer.shutdown(Shutdown::Both).unwrap();
        wait_until(|| !console.session.is_live());
        wait_until(|| console.session.log_status() == LogStatus::Off);
        assert_eq!(std::fs::read(&path).unwrap(), expected);
    }

    #[test]
    fn initial_open_failure_is_visible_and_can_be_retried_or_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        let mut console = Console::new(&blocker.join("session.log"));
        let LogStatus::Failed(message) = console.session.log_status() else {
            panic!("an unavailable log must report failure");
        };
        assert!(message.starts_with("Could not open log file:"));
        assert_eq!(console.session.take_notices(), vec![Notice { level: NoticeLevel::Warning, text: message }]);
        console.send(b"Switch#", "Switch#");
        assert!(console.session.take_notices().is_empty());
        assert!(console.session.is_connected());
        console.peer.shutdown(Shutdown::Both).unwrap();
        wait_until(|| !console.session.is_live());
        assert!(matches!(console.session.log_status(), LogStatus::Failed(_)));
        console.session.profile.log_path = dir.path().join("working.log").display().to_string();
        console.reconnect();
        assert_eq!(console.session.log_status(), LogStatus::Active);
        console.session.shutdown();
        console.session.profile.log_path.clear();
        console.reconnect();
        assert_eq!(console.session.log_status(), LogStatus::Off);
        console.send(b" no log", "no log");
        assert!(console.session.is_connected());
    }

    #[test]
    fn failure_notice_and_repaint_run_after_releasing_the_log_lock() {
        let shared_slot: Arc<Mutex<Option<std::sync::Weak<Shared>>>> = Arc::new(Mutex::new(None));
        let on_repaint = shared_slot.clone();
        let session = Session::new(Profile::default(), Theme::default(), move || {
            let shared = on_repaint.lock().as_ref().unwrap().upgrade().unwrap();
            assert!(shared.log.try_lock().is_some(), "repaint must not hold the log lock");
        });
        *shared_slot.lock() = Some(Arc::downgrade(&session.shared));
        let writer = Arc::new(Mutex::new(WriterState { fail_flush: true, ..Default::default() }));
        *session.shared.log.lock() = SessionLog::Active(Box::new(InjectedWriter(writer)));
        session.shared.show(b"Switch#");
        assert_eq!(session.shared.emulator.lock().line_text(0), "Switch#");
        assert_eq!(session.take_notices().len(), 1);
    }

    #[test]
    fn reading_log_status_does_not_wait_for_a_blocked_writer() {
        struct BlockingWriter {
            entered: std::sync::mpsc::Sender<()>,
            resume: std::sync::mpsc::Receiver<()>,
        }

        impl Write for BlockingWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.entered.send(()).unwrap();
                self.resume.recv().unwrap();
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let session = Arc::new(Session::new(Profile::default(), Theme::default(), || {}));
        let (entered, started) = std::sync::mpsc::channel();
        let (resume, released) = std::sync::mpsc::channel();
        *session.shared.log.lock() = SessionLog::Active(Box::new(BlockingWriter { entered, resume: released }));
        *session.shared.log_status.lock() = LogStatus::Active;
        let output_session = session.clone();
        let output = std::thread::spawn(move || output_session.shared.show(b"Switch#"));
        started.recv_timeout(Duration::from_secs(5)).unwrap();

        let (reported, received) = std::sync::mpsc::channel();
        let status_session = session.clone();
        let status = std::thread::spawn(move || reported.send(status_session.log_status()).unwrap());
        let result = received.recv_timeout(Duration::from_secs(1));
        // Always release/join both threads before asserting so a regression
        // cannot leave a worker stuck during test cleanup.
        resume.send(()).unwrap();
        output.join().unwrap();
        status.join().unwrap();
        assert_eq!(result.unwrap(), LogStatus::Active);
        assert_eq!(session.shared.emulator.lock().line_text(0), "Switch#");
    }

    #[test]
    fn stale_callbacks_cannot_change_a_reconnected_session_or_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile { log_path: dir.path().join("session.log").display().to_string(), ..Profile::default() };
        let mut session = Session::new(profile, Theme::default(), || {});
        session.open_log();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sink = Arc::new(SessionSink {
            shared: session.shared.clone(),
            generation: 0,
            link: session.link.clone(),
            runtime: runtime.handle().clone(),
        });

        let state = session.shared.state.lock();
        let callback_sink = sink.clone();
        let (started, waiting) = std::sync::mpsc::channel();
        let connected = std::thread::spawn(move || {
            started.send(()).unwrap();
            callback_sink.connected();
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        session.shared.generation.fetch_add(1, Ordering::SeqCst);
        drop(state);
        connected.join().unwrap();
        sink.closed(Some("old connection closed".into()));
        assert_eq!(session.state(), State::Connecting);
        assert_eq!(session.log_status(), LogStatus::Active);

        let current_sink = SessionSink {
            shared: session.shared.clone(),
            generation: 1,
            link: session.link.clone(),
            runtime: runtime.handle().clone(),
        };
        session.shutdown();
        current_sink.connected();
        assert_eq!(session.state(), State::Closed(None));
        assert_eq!(session.log_status(), LogStatus::Off);
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
            network: Default::default(),
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
