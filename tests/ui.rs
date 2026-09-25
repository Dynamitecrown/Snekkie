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
