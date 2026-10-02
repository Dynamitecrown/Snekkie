//! The real session and TCP worker against a paged console, with no UI
//! frames: every automatic Space must reach the device exactly once.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use snekkie::profiles::Profile;
use snekkie::session::{ConnectContext, Session};
use snekkie::settings::Theme;

const WAIT: Duration = Duration::from_secs(5);

fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Console {
    session: Session,
    peer: TcpStream,
    _runtime: tokio::runtime::Runtime,
}

impl Console {
    fn new(enabled: bool, log_path: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let profile = Profile {
            kind: "raw".into(),
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            log_path,
            ..Profile::default()
        };
        let mut session = Session::new(profile, Theme::default(), || {});
        session.set_auto_paging(enabled);
        session.connect(ConnectContext {
            runtime: runtime.handle(),
            ask_host_key: Arc::new(|_| {}),
            password: String::new(),
            key_passphrase: String::new(),
        });
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(WAIT)).unwrap();
        wait_until(|| session.is_connected());
        peer.write_all(b"Switch#").unwrap();
        wait_until(|| session.shared.emulator.lock().line_text(0) == "Switch#");
        Console { session, peer, _runtime: runtime }
    }

    fn expect(&mut self, bytes: &[u8]) {
        self.peer.set_read_timeout(Some(WAIT)).unwrap();
        let mut got = vec![0; bytes.len()];
        self.peer.read_exact(&mut got).unwrap();
        assert_eq!(got, bytes);
    }

    fn expect_quiet(&mut self) {
        self.peer.set_read_timeout(Some(Duration::from_millis(180))).unwrap();
        let mut byte = [0];
        let result = self.peer.read(&mut byte);
        assert!(
            matches!(result, Err(ref e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)),
            "unexpected bytes: {result:?}, {byte:?}"
        );
    }

    fn send(&mut self, bytes: &[u8], expected: &str) {
        self.peer.write_all(bytes).unwrap();
        wait_until(|| self.session.shared.emulator.lock().screen_text().contains(expected));
    }
}

#[test]
fn show_commands_page_in_the_background_and_preserve_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("session.log");
    let mut console = Console::new(true, log.to_str().unwrap().into());
    console.session.write(b"show run\r".to_vec());
    console.expect(b"show run\r");
    let first = b"show run\r\ninterface Vlan1\r\n\x1b[7m--Mo";
    console.send(first, "--Mo");
    console.expect_quiet(); // split prompt, still incomplete
    let rest = b"re--\x1b[0m";
    console.send(rest, "--More--");
    console.expect(b" ");
    console.send(b"\r\x1b[2K--More--\x1b[0m", "--More--");
    console.expect_quiet(); // redraw, still the same page

    let second = b"\r\x1b[2K ip address 10.0.0.1 255.255.255.0\r\n--More--";
    console.send(second, "ip address 10.0.0.1");
    console.expect(b" ");
    let end = b"\r\x1b[2Kend\r\nSwitch#";
    console.send(end, "end\nSwitch#");
    console.expect_quiet();
    let expected: Vec<u8> = [b"Switch#".as_slice(), first, rest, b"\r\x1b[2K--More--\x1b[0m", second, end].concat();
    assert_eq!(std::fs::read(&log).unwrap(), expected);

    console.session.write(b"sh ip route\r".to_vec());
    console.expect(b"sh ip route\r");
    console.send(b"sh ip route\r\n10.0.0.0/24 connected\r\n<--- More --->", "<--- More --->");
    console.expect(b" ");
    console.session.write(b"q".to_vec());
    console.expect(b"q");
    console.peer.write_all(b"\r\x1b[2K--More--").unwrap();
    console.expect_quiet();
}

#[test]
fn disabled_paging_is_manual_and_turning_it_off_cancels_a_pending_space() {
    let mut console = Console::new(false, String::new());
    console.session.write(b"show interfaces\r".to_vec());
    console.expect(b"show interfaces\r");
    console.send(b"show interfaces\r\nGi1/0/1 is up\r\n--More--", "--More--");
    console.expect_quiet();
    console.session.write(b" ".to_vec());
    console.expect(b" ");
    console.send(b"\r\x1b[2Kend\r\nSwitch#", "end\nSwitch#");

    console.session.set_auto_paging(true);
    console.session.write(b"show version\r".to_vec());
    console.expect(b"show version\r");
    console.send(b"show version\r\nCisco IOS Software\r\n--More--", "Cisco IOS Software");
    console.session.set_auto_paging(false);
    console.expect_quiet();
    console.session.write(b"q".to_vec());
    console.expect(b"q");
}

#[test]
fn unrelated_commands_and_confirmation_questions_never_get_spaces() {
    let mut console = Console::new(true, String::new());
    console.session.write(b"configure terminal\r".to_vec());
    console.expect(b"configure terminal\r");
    console.send(b"configure terminal\r\n--More--", "--More--");
    console.expect_quiet();
    console.send(b"\r\x1b[2KSwitch#", "Switch#");
    console.session.write(b"show run\r".to_vec());
    console.expect(b"show run\r");
    console.send(b"show run\r\nContinue? [yes/no]:", "Continue? [yes/no]:");
    console.expect_quiet();
}
