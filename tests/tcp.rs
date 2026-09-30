//! Telnet and raw TCP against a plain socket standing in for the device or
//! console server, so every byte on the wire can be checked.

mod common;

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Event, Recorder};
use snekkie::profiles::Profile;
use snekkie::transport::{Command, Link, Sink, tcp};

const WAIT: Duration = Duration::from_secs(10);

const IAC: u8 = 255;
const DO: u8 = 253;
const WILL: u8 = 251;
const SB: u8 = 250;
const BRK: u8 = 243;
const NOP: u8 = 241;
const SE: u8 = 240;
const ECHO: u8 = 1;
const SGA: u8 = 3;
const TTYPE: u8 = 24;
const NAWS: u8 = 31;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The server end of the connection, keeping everything the client sent.
struct Peer {
    stream: TcpStream,
    seen: Vec<u8>,
}

impl Peer {
    fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
    }

    /// Wait until the client has sent `needle`.
    fn expect(&mut self, needle: &[u8]) {
        let deadline = Instant::now() + WAIT;
        let mut buf = [0u8; 1024];
        while !contains(&self.seen, needle) {
            assert!(Instant::now() < deadline, "never got {needle:?}; got {:?}", self.seen);
            match self.stream.read(&mut buf) {
                Ok(0) => panic!("client hung up; got {:?}", self.seen),
                Ok(n) => self.seen.extend_from_slice(&buf[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => panic!("{e}"),
            }
        }
    }
}

struct Client {
    recorder: Arc<Recorder>,
    link: Link,
    _runtime: tokio::runtime::Runtime,
}

fn start(kind: &str, port: u16, keepalive: u32) -> Client {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let recorder = Recorder::new();
    let profile = Profile { kind: kind.into(), host: "127.0.0.1".into(), port, keepalive, ..Profile::default() };
    let link = tcp::start(runtime.handle(), profile, (80, 24), recorder.clone() as Arc<dyn Sink>);
    Client { recorder, link, _runtime: runtime }
}

fn connect(kind: &str, keepalive: u32) -> (Client, Peer) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = start(kind, listener.local_addr().unwrap().port(), keepalive);
    let (stream, _) = listener.accept().unwrap();
    stream.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    client.recorder.wait_event(WAIT, |e| *e == Event::Connected);
    (client, Peer { stream, seen: Vec::new() })
}

fn closed_reason(recorder: &Recorder) -> Option<String> {
    match recorder.wait_event(WAIT, |e| matches!(e, Event::Closed(_))) {
        Event::Closed(reason) => reason,
        _ => unreachable!(),
    }
}

#[test]
fn telnet_negotiates_like_a_cisco_device_expects() {
    let (client, mut peer) = connect("telnet", 0);
    // The client offers first, so gear that waits for it still gets going.
    peer.expect(&[IAC, WILL, NAWS, IAC, WILL, TTYPE]);
    peer.send(&[IAC, WILL, ECHO, IAC, WILL, SGA, IAC, DO, NAWS, IAC, DO, TTYPE]);
    peer.send(&[IAC, SB, TTYPE, 1, IAC, SE]);
    peer.send(b"User Access Verification\r\n\r\nUsername: ");

    peer.expect(&[IAC, SB, NAWS, 0, 80, 0, 24, IAC, SE]);
    peer.expect(b"\xff\xfa\x18\x00xterm-256color\xff\xf0");
    client.recorder.wait_text(WAIT, "Username: ");
    // The device echoes, so the session shouldn't.
    client.recorder.wait_event(WAIT, |e| *e == Event::RemoteEcho(true));
    // None of the negotiation reached the screen.
    assert_eq!(client.recorder.text(), "User Access Verification\r\n\r\nUsername: ");

    client.link.write(b"admin\r".to_vec());
    peer.expect(b"admin\r\n");

    client.link.send(Command::Resize { columns: 132, lines: 50 });
    peer.expect(&[IAC, SB, NAWS, 0, 132, 0, 50, IAC, SE]);

    client.link.send(Command::Break);
    peer.expect(&[IAC, BRK]);
    client.recorder.wait_event(WAIT, |e| *e == Event::Notice("Break sent".into()));
}

#[test]
fn telnet_keepalives_are_no_ops() {
    let (_client, mut peer) = connect("telnet", 1);
    peer.expect(&[IAC, NOP]);
}

#[test]
fn raw_passes_bytes_through_untouched() {
    let (client, mut peer) = connect("raw", 60);
    peer.send(&[IAC, WILL, ECHO]);
    peer.send(b"Router>\r\n");
    client.recorder.wait_text(WAIT, "Router>\r\n");
    assert_eq!(client.recorder.text(), "\u{fffd}\u{fffd}\u{1}Router>\r\n");

    client.link.write(b"en\r\xff".to_vec());
    peer.expect(b"en\r\xff");
    assert_eq!(peer.seen, b"en\r\xff", "no negotiation, no CR LF, no escaping");

    client.link.send(Command::Break);
    client.recorder.wait_event(WAIT, |e| *e == Event::Notice("Raw TCP sessions do not support break".into()));
    assert!(!client.recorder.events().iter().any(|e| matches!(e, Event::RemoteEcho(_))), "raw never negotiates");
    assert!(!client.recorder.events().iter().any(|e| matches!(e, Event::Notice(n) if n.contains("keepalive"))));
}

#[test]
fn remote_hang_up_is_reported() {
    for kind in ["telnet", "raw"] {
        let (client, mut peer) = connect(kind, 0);
        if kind == "telnet" {
            // Read the client's offers first: hanging up with unread data
            // makes Windows reset the connection instead of closing it.
            peer.expect(&[IAC, DO, SGA]);
        }
        drop(peer);
        assert_eq!(closed_reason(&client.recorder).as_deref(), Some("Connection closed by remote host"), "{kind}");
    }
}

#[test]
fn closing_hangs_up_quietly() {
    let (client, mut peer) = connect("telnet", 0);
    client.link.send(Command::Close);
    assert_eq!(closed_reason(&client.recorder), None);
    // The server sees the connection end.
    let deadline = Instant::now() + WAIT;
    let mut buf = [0u8; 64];
    loop {
        assert!(Instant::now() < deadline, "server never saw the hang-up");
        match peer.stream.read(&mut buf) {
            Ok(0) => break,
            Err(e) if !matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
            _ => {}
        }
    }
}

#[test]
fn refused_connection_is_reported() {
    // Nothing listens on a port that was just released.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let client = start("raw", port, 0);
    let reason = closed_reason(&client.recorder).unwrap();
    assert!(reason.starts_with("Could not connect to 127.0.0.1"), "{reason}");
    assert!(!client.recorder.events().contains(&Event::Connected));
}
