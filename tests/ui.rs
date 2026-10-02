//! Drives the real app headlessly (no window, no GPU) with fake serial
//! ports, and checks what the user would see.

use egui::accesskit::{Role, Toggled};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
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
    harness_sized(dir, [1000.0, 640.0])
}

fn harness_sized(dir: &tempfile::TempDir, size: [f32; 2]) -> Harness<'static, SnekkieApp> {
    let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
    let mut harness =
        Harness::builder().with_size(size).build_eframe(move |_cc| SnekkieApp::with_port_lister(paths, two_cables));
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
fn open_top_menu_switches_on_hover_in_both_directions() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let menus = [
        ("Session", "New session…"),
        ("Edit", "Copy"),
        ("Terminal", "Reset terminal"),
        ("View", "Next tab"),
        ("Settings", "Preferences…"),
        ("Help", "About Snekkie"),
    ];
    harness.get_by_label("Session").click();
    harness.run_ok();
    harness.get_by_label_contains("New session…");
    // Switching also works while a nested menu is open.
    harness.get_by_label_contains("Open saved").hover();
    harness.run_ok();
    harness.get_by_label("(none saved yet)");

    for (menu, item) in menus.iter().skip(1).chain(menus.iter().rev().skip(1)) {
        harness.get_by_label(menu).hover();
        harness.run_ok();
        assert!(harness.query_by_label_contains(item).is_some(), "hovering {menu} didn't open it");
        for (other_menu, other_item) in menus {
            if other_menu != *menu {
                assert!(harness.query_by_label_contains(other_item).is_none(), "{other_menu} stayed open over {menu}");
            }
        }
        assert!(harness.query_by_label("(none saved yet)").is_none(), "the previous submenu stayed open");
    }
    // Moving straight to a distant menu doesn't require crossing its neighbours.
    harness.get_by_label("Help").hover();
    harness.run_ok();
    harness.get_by_label("About Snekkie");
    assert!(harness.query_by_label_contains("New session…").is_none());
    // A menu opened by hovering can still be dismissed with a click.
    harness.get_by_label("Help").click();
    harness.run_ok();
    assert!(harness.query_by_label("About Snekkie").is_none());
    harness.get_by_label("Session").click();
    harness.run_ok();
    harness.get_by_label_contains("New session…");
}

#[test]
fn top_menu_hover_only_opens_after_a_click_and_stops_after_dismissal() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    harness.get_by_label("Edit").hover();
    harness.run_ok();
    assert!(harness.query_by_label_contains("Copy").is_none());

    harness.get_by_label("Session").click();
    harness.run_ok();
    harness.get_by_label("Edit").hover();
    harness.run_ok();
    assert!(harness.query_by_label_contains("Copy").is_some(), "hovering Edit didn't open it");
    harness.get_by_label_contains("Copy").hover();
    harness.run_ok();
    harness.get_by_label_contains("Paste");
    harness.key_press(egui::Key::Escape);
    harness.run_ok();
    harness.get_by_label("Terminal").hover();
    harness.run_ok();
    assert!(harness.query_by_label("Reset terminal").is_none());

    harness.get_by_label("Terminal").click();
    harness.run_ok();
    harness.get_by_label_contains("No session open.").click();
    harness.run_ok();
    harness.get_by_label("View").hover();
    harness.run_ok();
    assert!(harness.query_by_label_contains("Next tab").is_none());

    harness.get_by_label("View").click();
    harness.run_ok();
    harness.get_by_label("View").click();
    harness.run_ok();
    harness.get_by_label("Settings").hover();
    harness.run_ok();
    assert!(harness.query_by_label_contains("Preferences…").is_none());

    harness.get_by_label("Settings").click();
    harness.run_ok();
    harness.get_by_label_contains("Preferences…").click();
    harness.run_ok();
    harness.get_by_label("Preferences");
    assert!(harness.query_by_label_contains("Preferences…").is_none());
    harness.get_by_label("Cancel").click();
    harness.run_ok();
    harness.get_by_label("Help").hover();
    harness.run_ok();
    assert!(harness.query_by_label("About Snekkie").is_none());
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

fn open_preferences(harness: &mut Harness<'static, SnekkieApp>) {
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Comma);
    harness.run_ok();
    harness.get_by_label("Preferences");
}

fn show_page(harness: &mut Harness<'static, SnekkieApp>, page: &str) {
    harness.get_by_role_and_label(Role::Button, page).click();
    harness.run_ok();
}

/// The master switch on the Animations page.
fn animations_ticked(harness: &Harness<'static, SnekkieApp>) -> bool {
    let master = harness.get_by_role_and_label(Role::CheckBox, "Animations");
    master.accesskit_node().toggled() == Some(Toggled::True)
}

fn saved_settings(dir: &tempfile::TempDir) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(dir.path().join("settings.json")).ok()?;
    Some(serde_json::from_str(&text).unwrap())
}

const TYPING: [&str; 5] = ["Cursor movement", "Cursor blink", "Typed characters", "Keystroke burst", "Screen shake"];
const OUTPUT: [&str; 4] = ["New text", "Reveal", "Scrolling", "New lines"];

/// Animations are off until ticked in Preferences, with every style greyed
/// out meanwhile, and what's picked is saved with OK.
#[test]
fn animations_are_turned_on_and_styled_in_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    open_preferences(&mut harness);
    // It opens on the General page.
    harness.get_by_label("Colour theme");
    show_page(&mut harness, "Animations");
    assert!(!animations_ticked(&harness));
    for kind in TYPING.iter().chain(&OUTPUT) {
        assert!(harness.get_by_label(kind).accesskit_node().is_disabled(), "{kind} should be greyed out");
    }

    harness.get_by_role_and_label(Role::CheckBox, "Animations").click();
    harness.run_ok();
    assert!(animations_ticked(&harness));
    let motion = harness.get_by_label("Cursor movement");
    assert!(!motion.accesskit_node().is_disabled());
    assert_eq!(motion.value().as_deref(), Some("Glide"));
    motion.click();
    harness.run_ok();
    harness.get_by_label("Smear").click();
    harness.run_ok();
    assert_eq!(harness.get_by_label("Cursor movement").value().as_deref(), Some("Smear"));

    harness.get_by_label("OK").click();
    harness.run_ok();
    assert!(harness.query_by_label("Preferences").is_none(), "Preferences should have closed");
    let saved = saved_settings(&dir).expect("settings.json wasn't written");
    assert_eq!(saved["animations"]["enabled"], true, "{saved:#}");
    assert_eq!(saved["animations"]["cursor_motion"], "smear", "{saved:#}");
    // Everything else as it was.
    assert_eq!(saved["animations"]["new_text"], "fade", "{saved:#}");
    assert_eq!(saved["theme"], "Snekkie Dark", "{saved:#}");
}

#[test]
fn cancelling_preferences_leaves_animations_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    open_preferences(&mut harness);
    show_page(&mut harness, "Animations");
    harness.get_by_role_and_label(Role::CheckBox, "Animations").click();
    harness.run_ok();
    assert!(animations_ticked(&harness));
    harness.get_by_label("Cancel").click();
    harness.run_ok();
    assert!(harness.query_by_label("Preferences").is_none(), "Preferences should have closed");
    assert!(saved_settings(&dir).is_none_or(|saved| saved["animations"]["enabled"] != true));

    open_preferences(&mut harness);
    show_page(&mut harness, "Animations");
    assert!(!animations_ticked(&harness), "the cancelled change came back");
}

#[test]
fn automatic_paging_defaults_off_and_both_toggles_save_the_same_setting() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    open_preferences(&mut harness);
    let checkbox = harness.get_by_role_and_label(Role::CheckBox, "Automatically page through show commands");
    assert_eq!(checkbox.accesskit_node().toggled(), Some(Toggled::False));
    harness.get_by_label("Cancel").click();
    harness.run_ok();

    harness.get_by_label("Terminal").click();
    harness.run_ok();
    harness.get_by_label_contains("Auto-page show commands").click();
    harness.run_ok();
    assert_eq!(saved_settings(&dir).unwrap()["auto_paging"], true);

    open_preferences(&mut harness);
    let checkbox = harness.get_by_role_and_label(Role::CheckBox, "Automatically page through show commands");
    assert_eq!(checkbox.accesskit_node().toggled(), Some(Toggled::True));
    checkbox.click();
    harness.run_ok();
    harness.get_by_label("OK").click();
    harness.run_ok();
    assert_eq!(saved_settings(&dir).unwrap()["auto_paging"], false);

    // Reopening the app reads the saved choice, rather than just keeping
    // the last menu state in memory.
    drop(harness);
    let mut restarted = harness_sized(&dir, [1000.0, 640.0]);
    open_preferences(&mut restarted);
    let checkbox = restarted.get_by_role_and_label(Role::CheckBox, "Automatically page through show commands");
    assert_eq!(checkbox.accesskit_node().toggled(), Some(Toggled::False));
}

/// Both pages fit the window Snekkie opens with, the two groups of styles
/// side by side, and switching pages doesn't move the buttons.
#[test]
fn preferences_fit_the_window_on_both_pages() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    open_preferences(&mut harness);
    let mut buttons = Vec::new();
    for page in ["General", "Animations", "General"] {
        show_page(&mut harness, page);
        let heading = harness.get_by_label("Preferences").rect();
        let ok = harness.get_by_label("OK").rect();
        assert!(heading.min.x >= 0.0 && heading.min.y >= 0.0, "{page}: heading at {heading:?}");
        assert!(ok.max.y <= 640.0, "{page}: OK at {ok:?}");
        buttons.push(ok);
    }
    assert_eq!(buttons[0].min.x, buttons[1].min.x, "the dialog changed width");
    assert_eq!(buttons[1], buttons[2], "the buttons moved going back to General");

    show_page(&mut harness, "Animations");
    let left = harness.get_by_role_and_label(Role::CheckBox, "Animations").rect().min.x;
    let right = left + snekkie::ui::preferences::WIDTH;
    assert!(right <= 1000.0);
    let output = harness.get_by_label("Output").rect();
    for kind in TYPING {
        let rect = harness.get_by_label(kind).rect();
        assert!(rect.max.x < output.min.x, "{kind} at {rect:?} runs into the Output group at {output:?}");
    }
    for kind in OUTPUT {
        let rect = harness.get_by_label(kind).rect();
        assert!(rect.min.x > output.min.x && rect.max.x <= right, "{kind} at {rect:?}");
    }
}

/// In a window too short for a page, the page scrolls: the heading, the tabs
/// and the buttons stay where they can be reached, whatever order the pages
/// were visited in, and what's picked is saved.
#[test]
fn preferences_stay_usable_in_a_short_window() {
    // The one Snekkie is meant to be usable in, and the smallest it opens at.
    for size in [[1000.0, 480.0], [520.0, 320.0]] {
        let dir = tempfile::tempdir().unwrap();
        let mut harness = harness_sized(&dir, size);
        open_preferences(&mut harness);
        for page in ["General", "Animations", "General", "Animations"] {
            show_page(&mut harness, page);
            for (role, label) in [
                (Role::Label, "Preferences"),
                (Role::Button, "General"),
                (Role::Button, "Animations"),
                (Role::Button, "Cancel"),
                (Role::Button, "OK"),
            ] {
                let rect = harness.get_by_role_and_label(role, label).rect();
                let inside = rect.min.x >= 0.0 && rect.min.y >= 0.0 && rect.max.x <= size[0] && rect.max.y <= size[1];
                assert!(inside, "{size:?}, {page} page: {label} at {rect:?}");
            }
        }

        // Ticked on the Animations page, then a change on the other one, and
        // OK to save them both.
        harness.get_by_role_and_label(Role::CheckBox, "Animations").click();
        harness.run_ok();
        show_page(&mut harness, "General");
        harness.get_by_value("Snekkie Dark").click();
        harness.run_ok();
        harness.get_by_label("Monokai").click();
        harness.run_ok();
        harness.get_by_label("OK").click();
        harness.run_ok();
        assert!(harness.query_by_label("Preferences").is_none(), "{size:?}: Preferences should have closed");
        let saved = saved_settings(&dir).unwrap_or_else(|| panic!("{size:?}: settings.json wasn't written"));
        assert_eq!(saved["animations"]["enabled"], true, "{size:?}: {saved:#}");
        assert_eq!(saved["theme"], "Monokai", "{size:?}: {saved:#}");
    }
}
