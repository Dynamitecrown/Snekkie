//! Drives the serial transport over a real pty pair, so the actual
//! serialport code path runs rather than a mock. Unix only.

#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

use common::{Event, Recorder};
use serialport::{SerialPort, TTYPort};
use snekkie::profiles::Profile;
use snekkie::transport::{Command, Sink, serial};

const WAIT: Duration = Duration::from_secs(5);

fn open_pair() -> (TTYPort, String) {
    let (mut master, slave) = TTYPort::pair().expect("pty pair");
    let name = slave.name().expect("pty name");
    // Keep the slave side open for the test's lifetime by leaking it; the
    // transport opens its own handle by name.
    std::mem::forget(slave);
    master.set_timeout(Duration::from_millis(100)).unwrap();
    (master, name)
}

#[test]
fn round_trips_bytes_both_ways() {
    let (mut master, device) = open_pair();
    let recorder = Recorder::new();
    let profile = Profile { kind: "serial".into(), device, baud: 9600, ..Profile::default() };
    let link = serial::start(profile, recorder.clone() as Arc<dyn Sink>);

    recorder.wait_event(WAIT, |e| *e == Event::Connected);
    master.write_all(b"Switch>").unwrap();
    recorder.wait_text(WAIT, "Switch>");

    link.write(b"enable\r".to_vec());
    let mut received = Vec::new();
    let mut buf = [0u8; 64];
    let deadline = std::time::Instant::now() + WAIT;
    while !received.ends_with(b"enable\r") && std::time::Instant::now() < deadline {
        if let Ok(n) = master.read(&mut buf) {
            received.extend_from_slice(&buf[..n]);
        }
    }
    assert!(received.ends_with(b"enable\r"), "got {received:?}");

    link.send(Command::Close);
    let closed = recorder.wait_event(WAIT, |e| matches!(e, Event::Closed(_)));
    assert_eq!(closed, Event::Closed(None));
}

#[test]
fn missing_device_reports_why() {
    let recorder = Recorder::new();
    let profile = Profile { kind: "serial".into(), device: "/dev/does-not-exist".into(), ..Profile::default() };
    let _link = serial::start(profile, recorder.clone() as Arc<dyn Sink>);
    let Event::Closed(Some(reason)) = recorder.wait_event(WAIT, |e| matches!(e, Event::Closed(_))) else {
        panic!("expected a failure reason");
    };
    assert!(reason.contains("/dev/does-not-exist"), "{reason}");
    assert!(!recorder.events().contains(&Event::Connected));
}

#[test]
fn dropping_the_link_stops_the_worker() {
    let (_master, device) = open_pair();
    let recorder = Recorder::new();
    let profile = Profile { kind: "serial".into(), device, ..Profile::default() };
    let link = serial::start(profile, recorder.clone() as Arc<dyn Sink>);
    recorder.wait_event(WAIT, |e| *e == Event::Connected);
    drop(link);
    assert_eq!(recorder.wait_event(WAIT, |e| matches!(e, Event::Closed(_))), Event::Closed(None));
}
