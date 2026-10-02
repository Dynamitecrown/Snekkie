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
fn super_themes_change_the_whole_app_and_standard_restores_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let _peer = open_raw_tab(&mut harness, "Monitor");
    let original = harness.get_by_label("Terminal output").rect();
    let mut current = "Snekkie Dark";
    for (name, light) in [("E-Ink Super", true), ("Blueprint Super", false), ("Amber Super", false)] {
        open_preferences(&mut harness);
        choose_theme(&mut harness, current, name);
        harness.get_by_label("OK").click();
        harness.run_ok();
        assert_eq!(harness.ctx.global_style().visuals.dark_mode, !light);
        assert!(harness.ctx.global_style().visuals.override_text_color.is_some());
        assert!(harness.get_by_label("Terminal output").rect().width() < original.width());
        current = name;
    }
    open_preferences(&mut harness);
    choose_theme(&mut harness, current, "Snekkie Dark");
    harness.get_by_label("OK").click();
    harness.run_ok();
    assert!(harness.ctx.global_style().visuals.override_text_color.is_none());
    assert_eq!(harness.get_by_label("Terminal output").rect(), original);
}

fn wait_for_dialog(harness: &mut Harness<'static, SnekkieApp>, needle: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !harness.state().dialog_texts().iter().any(|s| s.contains(needle)) {
        assert!(std::time::Instant::now() < deadline, "missing dialog {needle}");
        harness.run_ok();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn offline_mode_keeps_local_connections_and_requires_explicit_public_and_dns_approval() {
    use snekkie::settings::{AppSettings, SettingsStore};
    let dir = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(dir.path().join("settings.json"));
    store.save(&AppSettings { offline_mode: true, check_for_updates: false, ..Default::default() }).unwrap();
    let mut harness = harness(&dir);
    let _local = open_raw_tab(&mut harness, "Local");
    assert!(harness.state().dialog_texts().is_empty());
    for kind in ["raw", "telnet", "ssh"] {
        let ctx = harness.ctx.clone();
        harness.state_mut().open_session(
            &ctx,
            snekkie::profiles::Profile {
                name: kind.into(),
                kind: kind.into(),
                host: "192.0.2.1".into(),
                auth: "agent".into(),
                ..Default::default()
            },
        );
        wait_for_dialog(&mut harness, "outside private/local IP ranges");
        harness.key_press(egui::Key::Enter);
        harness.run_ok();
        assert!(
            harness.state().dialog_texts().iter().any(|s| s.contains("outside private/local")),
            "Enter must not approve external traffic"
        );
        harness.get_by_label("Cancel").click();
        harness.run_ok();
    }
    let ctx = harness.ctx.clone();
    harness.state_mut().open_session(
        &ctx,
        snekkie::profiles::Profile { kind: "raw".into(), host: "never-resolve.invalid".into(), ..Default::default() },
    );
    wait_for_dialog(&mut harness, "DNS servers");
    harness.key_press(egui::Key::Escape);
    harness.run_ok();
    harness.get_by_label("Help").click();
    harness.run_ok();
    harness.get_by_label("Check for updates…").click();
    harness.run_ok();
    assert!(harness.state().dialog_texts().iter().any(|s| s.contains("Online updates are disabled")));
    harness.get_by_label("OK").click();
    harness.run_ok();
    open_preferences(&mut harness);
    harness.get_by_role_and_label(Role::CheckBox, "Offline mode").scroll_to_me();
    harness.run_ok();
    harness.get_by_role_and_label(Role::CheckBox, "Offline mode").click();
    harness.run_ok();
    harness.get_by_label("Cancel").click();
    harness.run_ok();
    assert!(store.load().offline_mode);
    open_preferences(&mut harness);
    harness.get_by_role_and_label(Role::CheckBox, "Offline mode").scroll_to_me();
    harness.run_ok();
    harness.get_by_role_and_label(Role::CheckBox, "Offline mode").click();
    harness.run_ok();
    harness.get_by_label("OK").click();
    harness.run_ok();
    assert!(!store.load().offline_mode);
    assert!(!store.load().check_for_updates);
}

#[test]
fn enabling_offline_mode_closes_existing_network_sessions_and_new_local_sessions_still_work() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let mut old = open_raw_tab(&mut harness, "Before privacy change");
    old.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    open_preferences(&mut harness);
    harness.get_by_role_and_label(Role::CheckBox, "Offline mode").scroll_to_me();
    harness.run_ok();
    harness.get_by_role_and_label(Role::CheckBox, "Offline mode").click();
    harness.run_ok();
    harness.get_by_label("OK").click();
    harness.run_ok();
    let saved = snekkie::settings::SettingsStore::new(dir.path().join("settings.json")).load();
    assert!(saved.offline_mode);
    assert!(!saved.check_for_updates);
    let mut discarded = Vec::new();
    old.read_to_end(&mut discarded).unwrap();
    let _local = open_raw_tab(&mut harness, "After privacy change");
    assert!(harness.state().dialog_texts().is_empty());
}

#[test]
fn creative_animation_choices_are_selectable_and_saved() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    open_preferences(&mut harness);
    show_page(&mut harness, "Animations");
    harness.get_by_role_and_label(Role::CheckBox, "Animations").click();
    harness.run_ok();
    for (old, new) in [
        ("Sparks", "Explosion"),
        ("Explosion", "Lasers"),
        ("Lasers", "Lightning"),
        ("Lightning", "Portal"),
        ("Pop", "Stamp"),
    ] {
        harness.get_by_value(old).click();
        harness.run_ok();
        harness.get_by_label(new).scroll_to_me();
        harness.run_ok();
        harness.get_by_label(new).click();
        harness.run_ok();
    }
    harness.get_by_label("OK").click();
    harness.run_ok();
    let saved = snekkie::settings::SettingsStore::new(dir.path().join("settings.json")).load();
    assert!(saved.animations.enabled);
    assert_eq!(saved.animations.keystroke_burst, snekkie::settings::KeystrokeBurst::Portal);
    assert_eq!(saved.animations.typed_text, snekkie::settings::TypedText::Stamp);
}

#[test]
fn putty_import_is_selectable_cancelable_and_preserves_existing_profiles() {
    use snekkie::profiles::{Profile, ProfileStore};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.json");
    ProfileStore::open(path.clone())
        .put(Profile { name: "Switch".into(), host: "original".into(), ..Default::default() })
        .unwrap();
    let export = br#"Windows Registry Editor Version 5.00
[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\Switch]
"HostName"="192.0.2.1"
"Protocol"="ssh"
[HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions\Console]
"Protocol"="serial"
"SerialLine"="COM7"
"#;
    let mut harness = harness(&dir);
    harness.get_by_label("Settings").click();
    harness.run_ok();
    harness.get_by_label("Import ⏵").hover();
    harness.run_ok();
    harness.get_by_label("PuTTY sessions…").click();
    harness.run_ok();
    harness.get_by_label("Read Windows PuTTY sessions");
    harness.get_by_label("Open PuTTY .reg export…");
    harness.get_by_label("Cancel").click();
    harness.run_ok();
    harness.state_mut().preview_putty_export(export).unwrap();
    harness.run_ok();
    harness.get_by_label("Cancel").click();
    harness.run_ok();
    assert_eq!(ProfileStore::open(path.clone()).profiles.len(), 1);
    harness.state_mut().preview_putty_export(export).unwrap();
    harness.run_ok();
    harness.get_by_role_and_label(Role::CheckBox, "Console").click();
    harness.run_ok();
    harness.get_by_label("Import selected").click();
    harness.run_ok();
    let imported = ProfileStore::open(path);
    assert_eq!(imported.profiles.len(), 2);
    assert_eq!(imported.get("Switch").unwrap().host, "original");
    assert_eq!(imported.get("Switch (PuTTY 1)").unwrap().host, "192.0.2.1");
    assert!(harness.state().tab_titles().is_empty(), "import must not connect");
    drop(harness);
    let restarted = harness_sized(&dir, [1000.0, 640.0]);
    restarted.get_by_label("Switch (PuTTY 1)");
}

#[test]
fn putty_import_keeps_its_actions_visible_in_a_short_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness_sized(&dir, [520.0, 320.0]);
    let mut export = "Windows Registry Editor Version 5.00\n".to_string();
    for i in 0..20 {
        export.push_str(&format!("[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY\\Sessions\\Switch{i}]\n\"HostName\"=\"10.0.0.1\"\n\"Protocol\"=\"ssh\"\n\"ProxyMethod\"=dword:00000001\n"));
    }
    harness.state_mut().preview_putty_export(export.as_bytes()).unwrap();
    harness.run_ok();
    for label in ["Import selected", "Cancel"] {
        let rect = harness.get_by_label(label).rect();
        assert!(rect.top() >= 0.0 && rect.bottom() <= 320.0, "{label}: {rect:?}");
    }
    harness.get_by_label("Cancel").click();
    harness.run_ok();
    assert!(harness.state().dialog_texts().is_empty());
}

fn pick_tab_color(harness: &mut Harness<'static, SnekkieApp>, name: &str, color: &str) {
    harness.get_by_label(name).click_secondary();
    harness.run_ok();
    harness.get_by_label_contains("Tab color").hover();
    harness.run_ok();
    harness.get_by_label(color).click();
    harness.run_ok();
}

fn choose_theme(harness: &mut Harness<'static, SnekkieApp>, current: &str, next: &str) {
    // System font discovery can relayout the centered dialog between the
    // accessibility snapshot and a synthetic click. Re-read its position.
    for _ in 0..3 {
        if harness.query_by_label(next).is_some() {
            break;
        }
        harness.get_by_value(current).click();
        harness.run_ok();
    }
    harness.get_by_label(next).scroll_to_me();
    harness.run_ok();
    harness.get_by_label(next).click();
    harness.run_ok();
}

#[test]
fn tab_colors_follow_their_sessions_when_reordered_reconnected_and_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let _first = open_raw_tab(&mut harness, "first");
    let _second = open_raw_tab(&mut harness, "second");
    pick_tab_color(&mut harness, "first", "Blue");
    pick_tab_color(&mut harness, "second", "Red");
    let colors = harness.state().tab_colors();
    assert!(colors.iter().all(|(_, color)| color.is_some()));
    assert_ne!(colors[0].1, colors[1].1);
    let from = harness.get_by_label("first").rect().center();
    let to = harness.get_by_label("second").rect().center();
    harness.hover_at(from);
    harness.drag_at(from);
    harness.run_ok();
    harness.hover_at(to);
    harness.run_ok();
    harness.drop_at(to);
    harness.run_ok();
    assert_eq!(harness.state().tab_colors(), [colors[1].clone(), colors[0].clone()]);
    harness.get_by_label("first").click_secondary();
    harness.run_ok();
    harness.get_by_label("Reconnect").click();
    harness.run_ok();
    assert_eq!(harness.state().tab_colors()[1], colors[0]);
    pick_tab_color(&mut harness, "first", "Custom color…");
    harness.get_by_label("Cancel").click();
    harness.run_ok();
    assert_eq!(harness.state().tab_colors()[1], colors[0]);
    pick_tab_color(&mut harness, "first", "Clear tab color");
    assert_eq!(harness.state().tab_colors()[1].1, None);
    assert_eq!(harness.state().tab_colors()[0], colors[1]);
}

#[test]
fn favorites_save_without_connecting_and_survive_restart() {
    use snekkie::profiles::{Profile, ProfileStore};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.json");
    let mut store = ProfileStore::open(path.clone());
    for name in ["Alpha", "Beta"] {
        store.put(Profile { name: name.into(), host: "192.0.2.1".into(), ..Default::default() }).unwrap();
    }
    let mut app = harness(&dir);
    app.get_by_label("Alpha").click();
    app.run_ok();
    app.get_by_label("Load").click();
    app.run_ok();
    app.get_by_label("Favorite Beta").click();
    app.run_ok();
    assert!(ProfileStore::open(path.clone()).get("Beta").unwrap().favorite);
    assert_eq!(app.state_mut().sidebar_draft().name, "Alpha");
    assert!(app.state().tab_titles().is_empty());
    drop(app);
    let mut restarted = harness(&dir);
    restarted.get_by_label("Favorites only").click();
    restarted.run_ok();
    assert!(restarted.query_by_label("Alpha").is_none());
    restarted.get_by_label("Unfavorite Beta").click();
    restarted.run_ok();
    restarted.get_by_label("No matching sessions");
    assert!(!ProfileStore::open(path).get("Beta").unwrap().favorite);
}

/// The filter and favorites controls must not crowd the list or push its
/// buttons out of the default window.
#[test]
fn saved_sessions_and_their_actions_fit_the_default_window() {
    use snekkie::profiles::{Profile, ProfileStore};
    let dir = tempfile::tempdir().unwrap();
    let mut store = ProfileStore::open(dir.path().join("sessions.json"));
    for name in ["Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta"] {
        store.put(Profile { name: name.into(), host: "192.0.2.1".into(), ..Default::default() }).unwrap();
    }
    let mut app = harness(&dir);
    let connect = app.get_by_label("Connect").rect();
    app.get_by_label("Delete").scroll_to_me();
    app.run_ok();
    assert_eq!(app.get_by_label("Connect").rect(), connect, "reaching Delete needed the sidebar to scroll");
    let first = app.get_by_label("Alpha").rect();
    let delete = app.get_by_label("Delete").rect();
    assert!(delete.top() - first.top() >= 3.0 * first.height(), "list too short: {first:?} to {delete:?}");
}

#[test]
fn sidebar_filter_keys_stay_local_and_hiding_sidebar_restores_terminal_input() {
    use std::{io::Read, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut peer = open_raw_tab(&mut app, "Live");
    peer.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    app.get_by_label("Filter saved sessions").click();
    app.run_ok();
    app.event(egui::Event::Text("192.0.2".into()));
    app.key_press(egui::Key::Enter);
    app.key_press(egui::Key::ArrowDown);
    app.run_ok();
    let error = peer.read(&mut [0; 64]).unwrap_err();
    assert!(matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut));
    app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::B);
    app.run_ok();
    app.get_by_label("Terminal output").click();
    app.run_ok();
    app.event(egui::Event::Text("show clock".into()));
    app.run_ok();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut bytes = [0; 10];
    peer.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"show clock");
}

#[test]
fn profile_colors_persist_and_tab_overrides_target_the_inactive_session() {
    use snekkie::profiles::{Profile, ProfileStore};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.json");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let profile = Profile {
        name: "Saved".into(),
        kind: "raw".into(),
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        tab_color: "#509ce7".into(),
        ..Default::default()
    };
    ProfileStore::open(path.clone()).put(profile.clone()).unwrap();
    let mut app = harness(&dir);
    let ctx = app.ctx.clone();
    app.state_mut().open_session(&ctx, profile);
    let (_saved_peer, _) = listener.accept().unwrap();
    app.run_ok();
    assert_eq!(app.state().tab_colors()[0].1, snekkie::settings::parse_hex("#509ce7"));
    let _other_peer = open_raw_tab(&mut app, "Other");
    app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::B);
    app.run_ok();
    pick_tab_color(&mut app, "Saved", "Red");
    let red = app.state().tab_colors()[0].1.unwrap();
    assert_eq!(app.state().tab_colors()[1].1, None);
    assert_eq!(ProfileStore::open(path.clone()).get("Saved").unwrap().tab_color, "#509ce7");
    pick_tab_color(&mut app, "Saved", "Save tab color as profile default");
    assert_eq!(ProfileStore::open(path.clone()).get("Saved").unwrap().tab_color, snekkie::settings::to_hex(red));
    assert_eq!(app.state().tab_colors()[1].1, None);
    pick_tab_color(&mut app, "Saved", "Clear tab color");
    assert_eq!(app.state().tab_colors()[0].1, None);
    pick_tab_color(&mut app, "Saved", "Use profile color");
    assert_eq!(app.state().tab_colors()[0].1, Some(red));
    pick_tab_color(&mut app, "Saved", "Clear tab color");
    pick_tab_color(&mut app, "Saved", "Save tab color as profile default");
    assert_eq!(ProfileStore::open(path).get("Saved").unwrap().tab_color, "");
}

#[test]
fn json_import_previews_selected_final_names_and_cancel_saves_nothing() {
    use snekkie::profiles::{Profile, ProfileStore};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.json");
    ProfileStore::open(path.clone())
        .put(Profile { name: "Alpha".into(), host: "192.0.2.1".into(), ..Default::default() })
        .unwrap();
    let json = br##"{"version":1,"sessions":[
        {"name":"Alpha","kind":"ssh","host":"192.0.2.2","favorite":true,"tab_color":"#abcdef"},
        {"name":"Beta","kind":"telnet","host":"192.0.2.3"},
        {"name":"Bad","kind":"unsupported","host":"192.0.2.4"}
    ]}"##;
    let mut app = harness_sized(&dir, [520.0, 320.0]);
    app.state_mut().preview_profile_import(json).unwrap();
    app.run_ok();
    for label in ["Import selected", "Cancel"] {
        let rect = app.get_by_label(label).rect();
        assert!(rect.top() >= 0.0 && rect.bottom() <= 320.0, "{label}: {rect:?}");
    }
    app.get_by_label("Cancel").click();
    app.run_ok();
    assert_eq!(ProfileStore::open(path.clone()).profiles.len(), 1);
    app.state_mut().preview_profile_import(json).unwrap();
    app.run_ok();
    app.get_by_role_and_label(Role::CheckBox, "Beta").scroll_to_me();
    app.run_ok();
    app.get_by_role_and_label(Role::CheckBox, "Beta").click();
    app.run_ok();
    app.get_by_label("Import selected").click();
    app.run_ok();
    let saved = ProfileStore::open(path);
    assert_eq!(saved.profiles.len(), 2);
    assert_eq!(saved.get("Alpha").unwrap().host, "192.0.2.1");
    let imported = saved.get("Alpha (Imported 1)").unwrap();
    assert_eq!(imported.host, "192.0.2.2");
    assert!(imported.favorite);
    assert_eq!(imported.tab_color, "#abcdef");
    assert!(app.state().tab_titles().is_empty());
}

#[test]
fn logging_status_tracks_open_failure_active_writer_and_remote_close() {
    use snekkie::profiles::Profile;
    use std::{
        io::Write,
        time::{Duration, Instant},
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let profile = Profile {
        name: "Logged".into(),
        kind: "raw".into(),
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        log_path: dir.path().join("device.log").display().to_string(),
        ..Default::default()
    };
    let ctx = app.ctx.clone();
    app.state_mut().open_session(&ctx, profile);
    let (mut peer, _) = listener.accept().unwrap();
    peer.write_all(b"device output\r\n").unwrap();
    app.run_ok();
    app.get_by_label("Logging");
    app.get_by_label("Open log folder");
    drop(peer);
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.query_by_label("Logging off").is_none() {
        assert!(Instant::now() < deadline);
        app.run_ok();
        std::thread::sleep(Duration::from_millis(5));
    }
    app.state_mut().open_session(
        &ctx,
        Profile {
            name: "Failed log".into(),
            kind: "raw".into(),
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            log_path: dir.path().display().to_string(),
            ..Default::default()
        },
    );
    let (_peer, _) = listener.accept().unwrap();
    app.run_ok();
    app.get_by_label("Logging failed");
}

fn open_raw_tab(harness: &mut Harness<'static, SnekkieApp>, name: &str) -> std::net::TcpStream {
    use std::io::Write;
    use std::time::{Duration, Instant};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ctx = harness.ctx.clone();
    harness.state_mut().open_session(
        &ctx,
        snekkie::profiles::Profile {
            name: name.into(),
            kind: "raw".into(),
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        },
    );
    let (mut device, _) = listener.accept().unwrap();
    let prompt = format!("{name}>");
    device.write_all(prompt.as_bytes()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.state().active_screen_text().is_none_or(|text| text.trim() != prompt) {
        assert!(Instant::now() < deadline, "{name} never connected");
        harness.run_ok();
        std::thread::sleep(Duration::from_millis(20));
    }
    harness.run_ok();
    device
}

/// Runs frames until the find bar says `status`.
fn wait_for_search(harness: &mut Harness<'static, SnekkieApp>, status: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while harness.state().active_search_status().as_deref() != Some(status) {
        assert!(
            std::time::Instant::now() < deadline,
            "find bar says {:?}, not {status:?}, looking for {:?}",
            harness.state().active_search_status(),
            harness.query_by_label("Find in terminal").and_then(|field| field.value())
        );
        harness.run_ok();
    }
}

/// Sends output from the device and waits for its last line to show.
fn device_prints(harness: &mut Harness<'static, SnekkieApp>, device: &mut std::net::TcpStream, text: &str) {
    use std::io::Write;
    device.write_all(text.as_bytes()).unwrap();
    let last = text.trim_end().rsplit('\n').next().unwrap().trim().to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while harness.state().active_screen_text().is_none_or(|screen| !screen.contains(&last)) {
        assert!(std::time::Instant::now() < deadline, "{last:?} never arrived");
        harness.run_ok();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    harness.run_ok();
}

fn assert_device_received_nothing(device: &mut std::net::TcpStream) {
    use std::io::Read;
    device.set_read_timeout(Some(std::time::Duration::from_millis(150))).unwrap();
    match device.read(&mut [0; 64]) {
        Err(error) => {
            assert!(matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
        }
        Ok(n) => panic!("the device received {n} bytes"),
    }
}

#[test]
fn find_bar_steps_through_history_without_sending_anything_and_escape_restores_typing() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "core");
    let output: String = (0..80)
        .map(|i| format!("\r\nline {i}{}", if i % 30 == 5 { " interface GigabitEthernet1/0/5" } else { "" }))
        .collect();
    device_prints(&mut app, &mut device, &output);
    assert_eq!(app.state().active_display_offset(), Some(0));

    app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
    app.run_ok();
    app.run_ok();
    assert!(app.get_by_label("Find in terminal").is_focused());
    app.event(egui::Event::Text("gigabitethernet1/0/5".into()));
    app.run_ok();
    // Matches on lines 5, 35 and 65; the nearest above the bottom is current.
    wait_for_search(&mut app, "3 of 3");
    app.key_press(egui::Key::Enter);
    wait_for_search(&mut app, "2 of 3");
    let offset = app.state().active_display_offset().unwrap();
    assert!(offset > 0, "jumped back into the history");
    app.key_press(egui::Key::Enter);
    wait_for_search(&mut app, "1 of 3");
    assert!(app.state().active_display_offset().unwrap() > offset);
    // Past the oldest, round to the newest; Shift+Enter comes back down.
    app.key_press(egui::Key::Enter);
    wait_for_search(&mut app, "3 of 3");
    app.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
    wait_for_search(&mut app, "1 of 3");
    app.get_by_label("Next match").click();
    wait_for_search(&mut app, "2 of 3");
    app.get_by_label("Previous match").click();
    wait_for_search(&mut app, "1 of 3");
    assert_device_received_nothing(&mut device);

    // Escape closes the bar and gives typing back to the terminal.
    app.get_by_label("Find in terminal").click();
    app.run_ok();
    app.key_press(egui::Key::Escape);
    app.run_ok();
    app.run_ok();
    assert_eq!(app.state().active_search_status(), None);
    assert!(app.query_by_label("Find in terminal").is_none());
    assert_device_received_nothing(&mut device);
    app.event(egui::Event::Text("show clock".into()));
    app.run_ok();
    device.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    let mut bytes = [0; 10];
    device.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"show clock");
}

#[test]
fn find_options_errors_and_the_edit_menu() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "edge");
    device_prints(&mut app, &mut device, "\r\nVlan10 up\r\nvlan20 down\r\nVLAN30 up\r\n");

    app.get_by_label("Edit").click();
    app.run_ok();
    app.get_by_label_contains("Find…").click();
    app.run_ok();
    app.run_ok();
    app.event(egui::Event::Text("vlan".into()));
    wait_for_search(&mut app, "3 of 3");
    app.get_by_label("Match case").click();
    wait_for_search(&mut app, "1 of 1");
    app.get_by_label("Match case").click();
    app.get_by_label("Regular expression").click();
    app.run_ok();
    app.get_by_label("Find in terminal").focus();
    app.run_ok();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(r"vlan\d+ up$".into()));
    wait_for_search(&mut app, "2 of 2");
    app.event(egui::Event::Text("(".into()));
    app.run_ok();
    app.run_ok();
    let status = app.state().active_search_status().unwrap();
    assert!(status.starts_with("Invalid regex"), "{status}");
    app.key_press(egui::Key::Backspace);
    wait_for_search(&mut app, "2 of 2");
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("nothing like this".into()));
    wait_for_search(&mut app, "No matches");
    app.get_by_label("Close search").click();
    app.run_ok();
    assert_eq!(app.state().active_search_status(), None);
    assert_device_received_nothing(&mut device);
}

#[test]
fn find_bar_refreshes_after_no_matches_and_after_clearing_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "live");
    app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
    app.run_ok();
    app.run_ok();
    app.event(egui::Event::Text("needle".into()));
    wait_for_search(&mut app, "No matches");

    device_prints(&mut app, &mut device, "\r\nneedle\r\nready");
    wait_for_search(&mut app, "1 of 1");
    app.key_press_modifiers(egui::Modifiers { ctrl: true, shift: true, ..Default::default() }, egui::Key::L);
    wait_for_search(&mut app, "No matches");
    device_prints(&mut app, &mut device, "\r\nnew needle\r\nready again");
    wait_for_search(&mut app, "1 of 1");
    assert_device_received_nothing(&mut device);
}

#[test]
fn find_bar_escape_closes_from_controls_and_terminal_without_reaching_the_device() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "escape");
    for focus in ["Match case", "Previous match", "Terminal output"] {
        app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
        app.run_ok();
        app.run_ok();
        app.get_by_label(focus).focus();
        app.run_ok();
        app.key_press(egui::Key::Escape);
        app.run_ok();
        app.run_ok();
        assert_eq!(app.state().active_search_status(), None, "Escape with {focus} focused");
        assert!(app.query_by_label("Find in terminal").is_none());
        assert_device_received_nothing(&mut device);
    }
}

#[test]
fn find_queries_and_options_stay_with_each_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut first = open_raw_tab(&mut app, "first");
    device_prints(&mut app, &mut first, "\r\nneedle\r\nready");
    app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
    app.run_ok();
    app.run_ok();
    app.event(egui::Event::Text("needle".into()));
    wait_for_search(&mut app, "1 of 1");

    let mut second = open_raw_tab(&mut app, "second");
    device_prints(&mut app, &mut second, "\r\nVLAN10\r\nvlan20\r\nready");
    assert_eq!(app.state().active_search_status(), None);
    app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
    app.run_ok();
    app.run_ok();
    app.event(egui::Event::Text("VLAN".into()));
    wait_for_search(&mut app, "2 of 2");
    app.get_by_label("Match case").click();
    wait_for_search(&mut app, "1 of 1");

    for (modifiers, query) in
        [(egui::Modifiers { ctrl: true, shift: true, ..Default::default() }, "needle"), (egui::Modifiers::CTRL, "VLAN")]
    {
        app.key_press_modifiers(modifiers, egui::Key::Tab);
        app.run_ok();
        app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
        app.run_ok();
        app.run_ok();
        assert_eq!(app.get_by_label("Find in terminal").value().as_deref(), Some(query));
        wait_for_search(&mut app, "1 of 1");
    }
    assert_device_received_nothing(&mut first);
    assert_device_received_nothing(&mut second);
}

#[test]
fn find_bar_fits_the_minimum_window_with_sidebar_and_keeps_its_match_visible() {
    for theme in ["Snekkie Dark", "CRT Super", "E-Ink Super"] {
        let dir = tempfile::tempdir().unwrap();
        snekkie::settings::SettingsStore::new(dir.path().join("settings.json"))
            .save(&snekkie::settings::AppSettings {
                theme: theme.into(),
                check_for_updates: false,
                ..Default::default()
            })
            .unwrap();
        let mut app = harness_sized(&dir, [520.0, 320.0]);
        let mut device = open_raw_tab(&mut app, "small");
        device_prints(&mut app, &mut device, "\r\nR1#show ip interface brief\r\nGi0/0 192.0.2.1 up up\r\nready");
        app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
        app.run_ok();
        app.run_ok();
        assert!(
            app.query_by_label("Find in terminal").is_some_and(|field| field.is_focused()),
            "{theme}: terminal {:?}, sidebar width {}",
            app.get_by_label("Terminal output").rect(),
            sidebar_width(&app)
        );
        app.event(egui::Event::Text("192.0.2.1".into()));
        wait_for_search(&mut app, "1 of 1");
        app.run_ok();
        let terminal = app.get_by_label("Terminal output").rect();
        let mut bar = egui::Rect::NOTHING;
        for label in
            ["Find in terminal", "Match case", "Regular expression", "Previous match", "Next match", "Close search"]
        {
            let rect = app.get_by_label(label).rect();
            assert!(app.ctx.content_rect().contains_rect(rect), "{label} does not fit in {theme}");
            bar = bar.union(rect);
        }
        let screen = app.state().active_screen_text().unwrap();
        let row_height = terminal.height() / screen.lines().count() as f32;
        let row = screen.lines().position(|line| line.contains("192.0.2.1")).unwrap() as f32;
        let matched_row = egui::Rect::from_min_max(
            egui::pos2(terminal.left(), terminal.top() + row * row_height),
            egui::pos2(terminal.right(), terminal.top() + (row + 1.0) * row_height),
        );
        assert!(!bar.expand(4.0).intersects(matched_row), "the search bar covers the result in {theme}");
        for option in ["Match case", "Regular expression"] {
            app.get_by_label(option).click();
            app.run_ok();
            assert_eq!(app.get_by_label(option).accesskit_node().toggled(), Some(Toggled::True), "{theme}");
        }
        app.get_by_label("Previous match").click();
        wait_for_search(&mut app, "1 of 1");
        app.get_by_label("Close search").click();
        app.run_ok();
        assert_eq!(app.state().active_search_status(), None);
        assert_device_received_nothing(&mut device);
    }
}

#[test]
fn find_bar_keeps_its_query_but_drops_old_results_after_reconnect_and_reset() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ctx = app.ctx.clone();
    app.state_mut().open_session(
        &ctx,
        snekkie::profiles::Profile {
            name: "reconnect".into(),
            kind: "raw".into(),
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        },
    );
    let (mut old_device, _) = listener.accept().unwrap();
    device_prints(&mut app, &mut old_device, "old needle\r\nready");
    app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
    app.run_ok();
    app.run_ok();
    app.event(egui::Event::Text("needle".into()));
    wait_for_search(&mut app, "1 of 1");

    app.key_press_modifiers(egui::Modifiers { ctrl: true, shift: true, ..Default::default() }, egui::Key::R);
    app.run_ok();
    let (mut new_device, _) = listener.accept().unwrap();
    device_prints(&mut app, &mut new_device, "new connection\r\nready after reconnect");
    wait_for_search(&mut app, "No matches");
    assert_eq!(app.get_by_label("Find in terminal").value().as_deref(), Some("needle"));
    device_prints(&mut app, &mut new_device, "\r\nnew needle\r\nready to reset");
    wait_for_search(&mut app, "1 of 1");

    app.get_by_label("Terminal").click();
    app.run_ok();
    app.get_by_label_contains("Reset terminal").click();
    app.run_ok();
    wait_for_search(&mut app, "No matches");
    assert_eq!(app.get_by_label("Find in terminal").value().as_deref(), Some("needle"));
    new_device.write_all(b"needle after reset\r\nready again").unwrap();
    wait_for_search(&mut app, "1 of 1");
    assert_device_received_nothing(&mut new_device);
}

#[test]
fn selected_text_on_one_line_becomes_the_query() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "dist");
    device_prints(&mut app, &mut device, "\r\nneedle haystack\r\n");
    // Drag across the start of the row that says "needle".
    let terminal = app.get_by_label("Terminal output").rect();
    let row_height = terminal.height() / app.state().active_screen_text().unwrap().lines().count().max(24) as f32;
    let y = terminal.top() + 4.0 + row_height * 1.5;
    let (from, to) = (egui::pos2(terminal.left() + 5.0, y), egui::pos2(terminal.left() + 40.0, y));
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    app.event(egui::Event::PointerMoved(from));
    app.event(button(from, true));
    app.run_ok();
    for step in 1..=5 {
        app.event(egui::Event::PointerMoved(from + (to - from) * step as f32 / 5.0));
        app.run_ok();
    }
    app.event(button(to, false));
    app.run_ok();
    app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
    app.run_ok();
    app.run_ok();
    let query = app.get_by_label("Find in terminal").value().unwrap_or_default();
    assert!(query.len() > 1 && "needle haystack".contains(&query), "{query:?}");
    wait_for_search(&mut app, "1 of 1");
    assert_device_received_nothing(&mut device);
}

#[test]
fn terminal_context_menu_keeps_navigation_and_escape_off_the_wire_and_restores_typing() {
    use std::io::Read;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let mut device = open_raw_tab(&mut harness, "switch");
    device.set_read_timeout(Some(Duration::from_millis(150))).unwrap();
    harness
        .get_by_label("Terminal output")
        .click_button_modifiers(egui::PointerButton::Secondary, egui::Modifiers::CTRL);
    harness.run_ok();
    harness.get_by_label("Copy all");
    harness.key_press(egui::Key::ArrowDown);
    harness.key_press(egui::Key::Escape);
    harness.run_ok();
    assert!(harness.query_by_label("Copy all").is_none());
    let error = device.read(&mut [0; 64]).unwrap_err();
    assert!(matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut));

    harness.event(egui::Event::Text("show run".into()));
    harness.run_ok();
    device.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut bytes = [0; 8];
    device.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"show run");

    // Enter activates the focused Copy all item locally, rather than
    // submitting the command currently being typed on the device.
    harness
        .get_by_label("Terminal output")
        .click_button_modifiers(egui::PointerButton::Secondary, egui::Modifiers::CTRL);
    harness.run_ok();
    harness.key_press(egui::Key::Enter);
    harness.run_ok();
    assert!(harness.query_by_label("Copy all").is_none());
    device.set_read_timeout(Some(Duration::from_millis(150))).unwrap();
    let error = device.read(&mut [0; 64]).unwrap_err();
    assert!(matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut));
    harness.event(egui::Event::Text("x".into()));
    harness.run_ok();
    device.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut byte = [0];
    device.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"x");
}

#[test]
fn new_connections_with_other_tabs_open_do_not_inherit_the_last_saved_session_name() {
    use snekkie::profiles::{Profile, ProfileStore};
    use std::net::TcpListener;

    let dir = tempfile::tempdir().unwrap();
    let first_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let second_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut store = ProfileStore::open(dir.path().join("sessions.json"));
    store
        .put(Profile {
            name: "saved console".into(),
            kind: "raw".into(),
            host: "127.0.0.1".into(),
            port: first_listener.local_addr().unwrap().port(),
            ..Default::default()
        })
        .unwrap();
    let mut harness = harness(&dir);
    let _existing = open_raw_tab(&mut harness, "already open");
    harness.get_by_label("saved console").click();
    harness.run_ok();
    harness.get_by_label("Load").click();
    harness.run_ok();
    harness.get_by_label("Connect").click();
    harness.run_ok();
    let (_first, _) = first_listener.accept().unwrap();
    assert_eq!(harness.state().tab_titles(), ["already open", "saved console"]);

    // The same console server, but a different port and therefore a
    // different console. Its tab must be named for that destination.
    let port = second_listener.local_addr().unwrap().port();
    harness.state_mut().sidebar_draft().port = port;
    harness.run_ok();
    harness.get_by_label("Connect").click();
    harness.run_ok();
    let (_second, _) = second_listener.accept().unwrap();
    assert_eq!(harness.state().tab_titles(), ["already open", "saved console", &format!("127.0.0.1:{port}")]);

    // Saving the edited form associates its new alias with the new
    // destination, without changing earlier tabs or the original profile.
    harness.get_by_label("Save…").click();
    harness.run_ok();
    harness
        .get_all_by_value(&format!("127.0.0.1:{port}"))
        .find(|node| node.accesskit_node().role() == Role::TextInput)
        .unwrap()
        .click();
    harness.run_ok();
    harness.key_press_modifiers(egui::Modifiers { ctrl: true, command: true, ..egui::Modifiers::NONE }, egui::Key::A);
    harness.event(egui::Event::Text("second console".into()));
    harness.run_ok();
    harness.get_by_label("OK").click();
    harness.run_ok();
    harness.get_by_label("Connect").click();
    harness.run_ok();
    let (_third, _) = second_listener.accept().unwrap();
    assert_eq!(
        harness.state().tab_titles(),
        ["already open", "saved console", &format!("127.0.0.1:{port}"), "second console"]
    );
    let saved = ProfileStore::open(dir.path().join("sessions.json"));
    assert_eq!(saved.get("saved console").unwrap().port, first_listener.local_addr().unwrap().port());
    assert_eq!(saved.get("second console").unwrap().port, port);

    harness.key_press_modifiers(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::N);
    harness.run_ok();
    assert_eq!(harness.state_mut().sidebar_draft().name, "New session");
    assert_eq!(harness.state_mut().sidebar_draft().host, "");
    assert!(harness.get_by_label("Load").accesskit_node().is_disabled());
}

#[test]
fn tab_x_closes_a_disconnected_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let device = open_raw_tab(&mut harness, "switch");
    drop(device);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while harness.state().tab_titles() != ["switch (closed)"] {
        assert!(std::time::Instant::now() < deadline, "session did not disconnect");
        harness.run_ok();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    harness.run_ok();
    harness.get_by_label("×").click();
    harness.run_ok();
    assert!(harness.state().tab_titles().is_empty(), "the tab's X did not close it");
    assert!(harness.state().dialog_texts().is_empty());
}

#[test]
fn tab_x_confirms_the_correct_session_without_selecting_it() {
    use std::io::Read;

    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let mut first = open_raw_tab(&mut harness, "first");
    let mut second = open_raw_tab(&mut harness, "second");
    harness.get_all_by_label("×").next().unwrap().click();
    harness.run_ok();
    assert_eq!(harness.state().dialog_texts(), ["“first” is still connected. Close it?"]);
    harness.get_by_label("No").click();
    harness.run_ok();
    assert_eq!(harness.state().tab_titles(), ["first", "second"]);
    assert_eq!(harness.state().active_screen_text().unwrap().trim(), "second>");

    harness.get_all_by_label("×").next().unwrap().click();
    harness.run_ok();
    harness.get_by_label("Yes").click();
    harness.run_ok();
    assert_eq!(harness.state().tab_titles(), ["second"]);
    first.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    assert_eq!(first.read(&mut [0; 1]).unwrap(), 0, "the closed session did not hang up");

    harness.get_by_label("×").click();
    harness.run_ok();
    assert_eq!(harness.state().dialog_texts(), ["“second” is still connected. Close it?"]);
    harness.get_by_label("Yes").click();
    harness.run_ok();
    assert!(harness.state().tab_titles().is_empty());
    second.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    assert_eq!(second.read(&mut [0; 1]).unwrap(), 0);
}

#[test]
fn tab_body_still_selects_drags_and_offers_other_close_gestures() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let _first = open_raw_tab(&mut harness, "first");
    let _second = open_raw_tab(&mut harness, "second");
    harness.get_by_label("first").click();
    harness.run_ok();
    assert_eq!(harness.state().active_screen_text().unwrap().trim(), "first>");

    let from = harness.get_by_label("first").rect().center();
    let to = harness.get_by_label("second").rect().center();
    harness.hover_at(from);
    harness.drag_at(from);
    harness.run_ok();
    harness.hover_at(to);
    harness.run_ok();
    harness.drop_at(to);
    harness.run_ok();
    assert_eq!(harness.state().tab_titles(), ["second", "first"]);
    assert_eq!(harness.state().active_screen_text().unwrap().trim(), "first>");

    harness.get_by_label("second").click_secondary();
    harness.run_ok();
    harness.get_by_label("Close").click();
    harness.run_ok();
    assert_eq!(harness.state().dialog_texts(), ["“second” is still connected. Close it?"]);
    harness.key_press(egui::Key::Escape);
    harness.run_ok();
    harness.get_by_label("second").click_button(egui::PointerButton::Middle);
    harness.run_ok();
    assert_eq!(harness.state().dialog_texts(), ["“second” is still connected. Close it?"]);
    harness.get_by_label("Yes").click();
    harness.run_ok();
    assert_eq!(harness.state().tab_titles(), ["first"]);
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
    // The worker can close the tab between the last frame and the state
    // check. Render that final state before querying its error message.
    harness.run_ok();
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
    // The saved-session rows are sized from the panel's width; any overflow
    // would widen the panel a little more on every frame.
    harness.get_by_label("More profile actions").click();
    for _ in 0..5 {
        harness.run_ok();
    }
    harness.get_by_label("Import profiles…");
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

fn edit_cursor_red(harness: &mut Harness<'static, SnekkieApp>, from: u8, to: u8) {
    harness.get_by_role_and_label(Role::ColorWell, "Cursor").click();
    harness.run_ok();
    harness.get_by_value(&format!("R {from}")).click();
    harness.run_ok();
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::A);
    harness.run_ok();
    harness.event(egui::Event::Text(to.to_string()));
    harness.run_ok();
    harness.key_press(egui::Key::Enter);
    harness.run_ok();
    harness.get_by_role_and_label(Role::Label, "Color theme").click();
    harness.run_ok();
    harness.get_by_value("Custom");
}

fn save_theme_as(harness: &mut Harness<'static, SnekkieApp>, name: &str) {
    harness.get_by_label("Save as…").click();
    harness.run_ok();
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::A);
    harness.run_ok();
    harness.event(egui::Event::Text(name.into()));
    harness.run_ok();
    // The name dialog sits above Preferences, which also has an OK button.
    harness.get_all_by_role_and_label(Role::Button, "OK").next_back().unwrap().click();
    harness.run_ok();
}

#[test]
fn selected_theme_colors_can_be_edited_and_saved_as_a_named_copy() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    open_preferences(&mut harness);
    harness.get_by_value("Snekkie Dark").click();
    harness.run_ok();
    harness.get_by_label("Monokai").click();
    harness.run_ok();
    for color in harness.get_all_by_role(Role::ColorWell) {
        assert!(!color.accesskit_node().is_disabled(), "the selected theme's colors should be editable");
    }
    edit_cursor_red(&mut harness, 166, 10);
    for color in ["#f8f8f2", "#272822", "#0ae22e", "#49483e"] {
        harness.get_by_label(color);
    }
    save_theme_as(&mut harness, "Monokai blue");
    harness.get_by_value("Monokai blue");
    harness.get_by_label("OK").click();
    harness.run_ok();
    let saved = saved_settings(&dir).unwrap();
    assert_eq!(saved["theme"], "Monokai blue");
    assert_eq!(saved["custom_cursor"], "#0ae22e");
    assert_eq!(
        saved["saved_themes"]["Monokai blue"],
        serde_json::json!({
            "fg": "#f8f8f2", "bg": "#272822", "cursor": "#0ae22e", "selection": "#49483e"
        })
    );

    // A fresh app loads the named copy; selecting its base preset still
    // shows the original palette, and selecting the copy restores the edits.
    drop(harness);
    let mut restarted = harness_sized(&dir, [1000.0, 640.0]);
    open_preferences(&mut restarted);
    restarted.get_by_value("Monokai blue").click();
    restarted.run_ok();
    restarted.get_by_label("Monokai").click();
    restarted.run_ok();
    restarted.get_by_label("#a6e22e");
    restarted.get_by_value("Monokai").click();
    restarted.run_ok();
    restarted.get_by_label("Monokai blue").scroll_to_me();
    restarted.run_ok();
    restarted.get_by_label("Monokai blue").click();
    restarted.run_ok();
    restarted.get_by_label("#0ae22e");
}

#[test]
fn crt_effects_survive_color_edits_and_named_saves_and_can_be_toggled() {
    use snekkie::settings::{SettingsStore, ThemeEffects};

    let dir = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(dir.path().join("settings.json"));
    let mut harness = harness(&dir);
    open_preferences(&mut harness);
    harness.get_by_value("Snekkie Dark").click();
    harness.run_ok();
    harness.get_by_label("CRT").scroll_to_me();
    harness.run_ok();
    harness.get_by_label("CRT").click();
    harness.run_ok();
    for label in ["Monochrome output", "CRT glow and scanlines"] {
        assert_eq!(
            harness.get_by_role_and_label(Role::CheckBox, label).accesskit_node().toggled(),
            Some(Toggled::True)
        );
    }
    edit_cursor_red(&mut harness, 164, 112);
    save_theme_as(&mut harness, "My CRT");
    harness.get_by_label("OK").click();
    harness.run_ok();
    let saved = store.load();
    assert_eq!(saved.theme, "My CRT");
    assert_eq!(saved.colors().effects, ThemeEffects { monochrome: true, crt: true, ..ThemeEffects::default() });
    assert_eq!(saved.custom_theme().effects, saved.colors().effects);
    assert_eq!(saved.colors().cursor, egui::Color32::from_rgb(112, 255, 187));
    assert!(!saved.animations.enabled);

    drop(harness);
    let mut restarted = harness_sized(&dir, [1000.0, 640.0]);
    open_preferences(&mut restarted);
    restarted.get_by_role_and_label(Role::CheckBox, "CRT glow and scanlines").click();
    restarted.run_ok();
    restarted.get_by_value("Custom");
    assert_eq!(
        restarted.get_by_role_and_label(Role::CheckBox, "Monochrome output").accesskit_node().toggled(),
        Some(Toggled::True)
    );
    assert_eq!(
        restarted.get_by_role_and_label(Role::CheckBox, "CRT glow and scanlines").accesskit_node().toggled(),
        Some(Toggled::False)
    );
    restarted.get_by_label("#70ffbb");
    restarted.get_by_label("Cancel").click();
    restarted.run_ok();
    assert_eq!(store.load(), saved, "cancel must discard effect changes");
}

#[test]
fn highlighting_slider_is_global_saved_and_cancelable_and_vendors_are_available() {
    use snekkie::settings::SettingsStore;
    let dir = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(dir.path().join("settings.json"));
    let mut harness = harness(&dir);
    open_preferences(&mut harness);
    show_page(&mut harness, "Highlighting");
    harness.get_by_role_and_label(Role::Slider, "Intensity").focus();
    harness.run_ok();
    for _ in 0..2 {
        harness.key_press(egui::Key::ArrowLeft);
        harness.run_ok();
    }
    harness.get_by_label("Essential");
    harness.get_by_value("Cisco IOS / IOS XE").click();
    harness.run_ok();
    harness.get_by_label("Juniper Junos").click();
    harness.run_ok();
    harness.get_by_value("Juniper Junos");
    harness.get_by_label("OK").click();
    harness.run_ok();
    assert_eq!(store.load().highlighting_intensity, 1);
    open_preferences(&mut harness);
    show_page(&mut harness, "Highlighting");
    harness.get_by_role_and_label(Role::Slider, "Intensity").focus();
    harness.run_ok();
    for _ in 0..4 {
        harness.key_press(egui::Key::ArrowRight);
        harness.run_ok();
    }
    harness.get_by_label("Full");
    harness.get_by_label("Cancel").click();
    harness.run_ok();
    assert_eq!(store.load().highlighting_intensity, 1);

    harness.get_by_value("None").click();
    harness.run_ok();
    for (_, label) in snekkie::terminal::highlight::syntax_options() {
        harness.get_by_label(label);
    }
    harness.get_by_label("Fortinet FortiOS").scroll_to_me();
    harness.run_ok();
    harness.get_by_label("Fortinet FortiOS").click();
    harness.run_ok();
    harness.get_by_value("Fortinet FortiOS");
}

#[test]
fn crt_super_changes_app_and_terminal_then_regular_crt_restores_chrome() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let mut harness = harness(&dir);
    let mut device = open_raw_tab(&mut harness, "Lab");
    device.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    let original = harness.get_by_label("Terminal output").rect();
    open_preferences(&mut harness);
    choose_theme(&mut harness, "Snekkie Dark", "CRT Super");
    harness.get_by_label("OK").click();
    harness.run_ok();
    let retro = harness.ctx.global_style();
    assert!(retro.visuals.override_text_color.is_some());
    assert_eq!(retro.text_styles[&egui::TextStyle::Button].family, egui::FontFamily::Monospace);
    let framed = harness.get_by_label("Terminal output").rect();
    assert!(framed.width() < original.width() && framed.height() < original.height());
    harness.get_by_label("Terminal output").click();
    harness.run_ok();
    harness.event(egui::Event::Text("show".into()));
    harness.run_ok();
    let mut typed = [0; 4];
    device.read_exact(&mut typed).unwrap();
    assert_eq!(&typed, b"show");

    open_preferences(&mut harness);
    choose_theme(&mut harness, "CRT Super", "CRT");
    harness.get_by_label("OK").click();
    harness.run_ok();
    assert!(harness.ctx.global_style().visuals.override_text_color.is_none());
    assert_eq!(harness.ctx.global_style().text_styles[&egui::TextStyle::Button].family, egui::FontFamily::Proportional);
    assert_eq!(harness.get_by_label("Terminal output").rect(), original);
}

#[test]
fn editing_a_saved_theme_requires_replacement_confirmation_and_cancel_discards_edits() {
    use snekkie::settings::{AppSettings, SettingsStore};

    let dir = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(dir.path().join("settings.json"));
    let mut settings = AppSettings { theme: "Monokai".into(), ..Default::default() };
    settings.saved_themes.insert("Original".into(), settings.colors().to_scheme());
    settings.theme = "Original".into();
    store.save(&settings).unwrap();
    let mut harness = harness(&dir);
    open_preferences(&mut harness);
    edit_cursor_red(&mut harness, 166, 10);
    save_theme_as(&mut harness, "Original");
    assert_eq!(harness.state().dialog_texts(), ["Replace the saved theme “Original”?"]);
    harness.get_by_label("No").click();
    harness.run_ok();
    harness.get_by_value("Custom");
    assert_eq!(store.load(), settings);

    save_theme_as(&mut harness, "Original");
    harness.get_by_label("Yes").click();
    harness.run_ok();
    harness.get_by_value("Original");
    harness.get_by_label("#0ae22e");
    harness.get_by_label("Cancel").click();
    harness.run_ok();
    assert_eq!(store.load(), settings, "cancel saved the replacement or the draft colors");
    open_preferences(&mut harness);
    harness.get_by_value("Original");
    harness.get_by_label("#a6e22e");
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
    harness.get_by_label("Color theme");
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
