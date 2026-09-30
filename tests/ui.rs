//! Drives the real app headlessly (no window, no GPU) with fake serial
//! ports, and checks what the user would see.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use snekkie::profiles::Kind;
use snekkie::transport::serial::PortInfo;
use snekkie::ui::{Paths, SnekkieApp};

fn two_cables() -> Vec<PortInfo> {
    ["COM3:A10K3X", "COM10:B77Q2Z"]
        .iter()
        .map(|spec| {
            let (device, serial) = spec.split_once(':').unwrap();
            PortInfo {
                device: device.into(),
                description: format!("USB Serial Port ({device})"),
                serial_number: serial.into(),
                ..PortInfo::default()
            }
        })
        .collect()
}

fn harness(dir: &tempfile::TempDir) -> Harness<'static, SnekkieApp> {
    let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
    let mut harness = Harness::builder()
        .with_size([1000.0, 640.0])
        .build_eframe(move |_cc| SnekkieApp::with_port_lister(paths, two_cables));
    harness.run_ok();
    harness
}

fn sidebar_width(harness: &Harness<'_, SnekkieApp>) -> f32 {
    egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("sidebar")).unwrap().size().x
}

#[test]
fn starts_empty() {
    let dir = tempfile::tempdir().unwrap();
    let harness = harness(&dir);
    harness.get_by_label("Connect");
    harness.get_by_label_contains("No session open.");
    assert!(harness.state().tab_titles().is_empty());
}

#[test]
fn two_cables_need_an_explicit_choice() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    harness.state_mut().set_form_kind(Kind::Serial);
    harness.run_ok();
    harness.get_by_value("Choose a port (2 detected)");

    harness.get_by_label("Connect").click();
    harness.run_ok();
    assert_eq!(harness.state().dialog_texts(), ["Choose a serial port from the Port list."]);
    assert!(harness.state().tab_titles().is_empty());
}

#[test]
fn picking_a_cable_from_the_list_connects_to_that_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    harness.state_mut().set_form_kind(Kind::Serial);
    harness.run_ok();

    harness.get_by_value("Choose a port (2 detected)").click();
    harness.run_ok();
    harness.get_by_label("COM10 — USB Serial Port  [SN B77Q2Z]").click();
    harness.run_ok();
    assert_eq!(harness.state_mut().sidebar_draft().device, "COM10");

    harness.get_by_label("Connect").click();
    harness.run_ok();
    // There's no COM10 on the test machine, so the tab opens and reports it.
    for _ in 0..50 {
        if harness.state().tab_titles() == ["COM10 (closed)"] {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        harness.run_ok();
    }
    assert_eq!(harness.state().tab_titles(), ["COM10 (closed)"]);
    harness.get_by_label_contains("Could not open COM10");
}

#[test]
fn sidebar_keeps_its_width_on_every_page() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let start = sidebar_width(&harness);
    assert!(start <= 321.0, "sidebar starts at {start}");
    harness.state_mut().set_form_kind(Kind::Serial);
    harness.run_ok();
    harness.run_ok();
    assert_eq!(sidebar_width(&harness), start);
    harness.get_by_label("Advanced").click();
    harness.run_ok();
    harness.run_ok();
    assert_eq!(sidebar_width(&harness), start);
}

/// A dialog's Enter must answer the dialog, not also go to the device.
#[cfg(unix)]
#[test]
fn confirming_a_dialog_sends_nothing_to_the_device() {
    use serialport::SerialPort;
    use std::io::Read;

    let (mut device, cable) = serialport::TTYPort::pair().unwrap();
    let path = cable.name().unwrap();
    std::mem::forget(cable);
    device.set_timeout(std::time::Duration::from_millis(50)).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let ctx = harness.ctx.clone();
    let profile = snekkie::profiles::Profile {
        name: "console".into(),
        kind: "serial".into(),
        device: path,
        ..Default::default()
    };
    harness.state_mut().open_session(&ctx, profile);
    for _ in 0..50 {
        harness.run_ok();
        if harness.state().tab_titles() == ["console"] && harness.state().active_screen_text().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Give the serial worker time to report it's connected.
    std::thread::sleep(std::time::Duration::from_millis(200));
    harness.run_ok();

    // Typing reaches the device...
    harness.key_press(egui::Key::Enter);
    harness.run_ok();
    let mut buf = [0u8; 16];
    assert_eq!(device.read(&mut buf).unwrap_or(0), 1, "Enter should reach the device");

    // Answering No to closing it hands the keyboard back to the terminal.
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::W);
    harness.run_ok();
    harness.key_press(egui::Key::Escape);
    harness.run_ok();
    assert!(harness.state().dialog_texts().is_empty());
    harness.key_press(egui::Key::Enter);
    harness.run_ok();
    assert_eq!(device.read(&mut buf).unwrap_or(0), 1, "the terminal should have the keyboard back");

    // ...and the Enter that confirms closing the tab doesn't reach the device.
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::W);
    harness.run_ok();
    assert_eq!(harness.state().dialog_texts(), ["“console” is still connected. Close it?"]);
    harness.key_press(egui::Key::Enter);
    harness.run_ok();
    assert!(harness.state().tab_titles().is_empty());
    assert_eq!(device.read(&mut buf).unwrap_or(0), 0, "the dialog's Enter leaked to the device");
}

/// Picking Telnet in the form shows its settings, and connecting from it
/// reaches a device with the negotiation kept off the screen.
#[test]
fn telnet_session_from_the_form() {
    use std::io::Write;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    harness.get_by_value("SSH").click();
    harness.run_ok();
    harness.get_by_label("Telnet").click();
    harness.run_ok();
    // No login fields for telnet, its usual port, keepalives on.
    assert!(harness.query_by_label("Username").is_none());
    assert_eq!(harness.state_mut().sidebar_draft().port, 23);
    harness.get_by_value("60 s");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let draft = harness.state_mut().sidebar_draft();
    draft.host = "127.0.0.1".into();
    draft.port = port;
    harness.get_by_label("Connect").click();
    harness.run_ok();

    let (mut device, _) = listener.accept().unwrap();
    device.write_all(b"\xff\xfb\x01Router>").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.state().active_screen_text().is_none_or(|text| text.trim() != "Router>") {
        assert!(Instant::now() < deadline, "screen: {:?}", harness.state().active_screen_text());
        harness.run_ok();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(harness.state().tab_titles(), [format!("127.0.0.1:{port}")]);
}

fn newer_release(installer_url: &str) -> snekkie::update::Release {
    snekkie::update::Release {
        version: "99.0.0".into(),
        page: "https://github.com/Dynamitecrown/Snekkie/releases/tag/v99.0.0".into(),
        installer: Some(snekkie::update::Asset {
            name: "Snekkie-Setup-99.0.0.exe".into(),
            url: installer_url.into(),
            size: 1000,
            sha256: None,
        }),
    }
}

fn update_summary() -> String {
    format!("Snekkie 99.0.0 is available. You have {}.", env!("CARGO_PKG_VERSION"))
}

/// A newer release shows in the menu bar. A copy the installer didn't put
/// there can't update itself, so it points at the release page.
#[test]
fn a_portable_copy_points_at_the_release_page() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    assert!(harness.query_by_label_contains("Update to").is_none());

    harness.state_mut().offer_update(newer_release("https://example.invalid/setup.exe"), false);
    harness.run_ok();
    harness.get_by_label("Update to 99.0.0").click();
    harness.run_ok();
    assert_eq!(harness.state().dialog_texts(), [update_summary()]);
    harness.get_by_label("Open release page");
    assert!(harness.query_by_label("Update now").is_none());

    harness.get_by_label("Later").click();
    harness.run_ok();
    assert!(harness.state().dialog_texts().is_empty());
    // Still on offer for later.
    harness.get_by_label("Update to 99.0.0");
}

/// An installed copy offers to update itself, but never on a stray Enter,
/// and a failed download is reported and can be tried again.
#[test]
fn an_installed_copy_updates_itself_only_when_asked() {
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    // Plain http is refused, so this fails without touching the network.
    harness.state_mut().offer_update(newer_release("http://127.0.0.1:9/Snekkie-Setup-99.0.0.exe"), true);
    harness.run_ok();
    harness.get_by_label("Update to 99.0.0").click();
    harness.run_ok();
    harness.get_by_label("What's new in 99.0.0");

    harness.key_press(egui::Key::Enter);
    harness.run_ok();
    assert_eq!(harness.state().dialog_texts(), [update_summary()], "Enter should not start the update");

    harness.get_by_label("Update now").click();
    harness.run_ok();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !harness.state().dialog_texts().iter().any(|t| t.starts_with("Could not download the update")) {
        assert!(Instant::now() < deadline, "dialogs: {:?}", harness.state().dialog_texts());
        std::thread::sleep(Duration::from_millis(20));
        harness.run_ok();
    }
    harness.key_press(egui::Key::Enter);
    harness.run_ok();
    harness.get_by_label("Update to 99.0.0");
}

/// A telnet device that hasn't agreed to echo gets what's typed drawn
/// locally, until it says it will. Backspace sends what the session asks.
#[test]
fn telnet_local_echo_follows_the_device() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    fn wait(
        harness: &mut Harness<'static, SnekkieApp>,
        what: &str,
        done: impl Fn(&Harness<'static, SnekkieApp>) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(harness) {
            assert!(Instant::now() < deadline, "{what}; screen: {:?}", harness.state().active_screen_text());
            harness.run_ok();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn screen(harness: &Harness<'static, SnekkieApp>) -> String {
        harness.state().active_screen_text().unwrap_or_default().trim().to_string()
    }

    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let profile = snekkie::profiles::Profile {
        name: "old-box".into(),
        kind: "telnet".into(),
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        backspace: "ctrl-h".into(),
        ..Default::default()
    };
    let ctx = harness.ctx.clone();
    harness.state_mut().open_session(&ctx, profile);
    let (mut device, _) = listener.accept().unwrap();
    device.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    let mut device_out = device.try_clone().unwrap();
    wait(&mut harness, "never connected", |h| h.query_by_label_contains("[connected]").is_some());

    let mut received = Vec::new();
    let mut expect = |needle: &[u8]| {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut buf = [0u8; 256];
        while !received.windows(needle.len()).any(|w| w == needle) {
            assert!(Instant::now() < deadline, "device never got {needle:?}; got {received:?}");
            if let Ok(n) = device.read(&mut buf) {
                received.extend_from_slice(&buf[..n]);
            }
        }
    };

    // The device hasn't agreed to echo: typing shows up anyway, and
    // Backspace (sent as ^H) rubs out what it deletes.
    harness.event(egui::Event::Text("sh".into()));
    harness.key_press(egui::Key::Backspace);
    harness.run_ok();
    expect(b"sh\x08");
    assert_eq!(screen(&harness), "s");

    // Once it says it will echo, Snekkie stops drawing keystrokes itself.
    device_out.write_all(b"\xff\xfb\x01|").unwrap();
    wait(&mut harness, "device output never arrived", |h| screen(h) == "s|");
    harness.event(egui::Event::Text("x".into()));
    harness.run_ok();
    expect(b"x");
    harness.run_ok();
    assert_eq!(screen(&harness), "s|", "typed text was drawn although the device echoes");
}
