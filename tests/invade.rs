//! Invade inside the real app, headless: launching, pausing, tab handling,
//! input isolation from real (loopback) devices, paced sends, updates and
//! the local records. Gameplay runs on the harness's fixed clock.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Key, Modifiers, ViewportId};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use snekkie::invade::Rig;
use snekkie::ui::{Paths, SnekkieApp};

fn harness(dir: &tempfile::TempDir) -> Harness<'static, SnekkieApp> {
    harness_sized(dir, [1000.0, 640.0])
}

fn harness_sized(dir: &tempfile::TempDir, size: [f32; 2]) -> Harness<'static, SnekkieApp> {
    let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
    let mut harness = Harness::builder()
        .with_size(size)
        .with_step_dt(1.0 / 30.0)
        .build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
    harness.run_ok();
    harness
}

fn frames(app: &mut Harness<'static, SnekkieApp>, n: usize) {
    for _ in 0..n {
        app.step();
    }
}

fn app_keys(shift: bool) -> Modifiers {
    Modifiers {
        ctrl: !cfg!(target_os = "macos"),
        mac_cmd: cfg!(target_os = "macos"),
        command: true,
        shift,
        ..Modifiers::NONE
    }
}

fn next_tab(app: &mut Harness<'static, SnekkieApp>, back: bool) {
    let modifiers = Modifiers { ctrl: true, shift: back, ..Modifiers::NONE };
    app.key_press_modifiers(modifiers, Key::Tab);
    frames(app, 2);
}

fn open_loadout(app: &mut Harness<'static, SnekkieApp>) {
    app.get_by_label("Help").click();
    frames(app, 3);
    assert!(app.query_by_role_and_label(Role::Button, "Invade").is_none(), "Invade belongs in About");
    app.get_by_role_and_label(Role::Button, "About Snekkie").click();
    frames(app, 3);
    app.get_by_role_and_label(Role::Button, "Invade").click();
    frames(app, 4);
    assert!(app.state().dialog_texts().is_empty(), "About closed before selecting the game");
}

fn start_run(app: &mut Harness<'static, SnekkieApp>) {
    if app.query_by_label("Start run").is_some() {
        app.get_by_label("Start run").click();
        frames(app, 4);
    }
}

fn open_invade(app: &mut Harness<'static, SnekkieApp>) {
    open_loadout(app);
    start_run(app);
}

fn status(app: &Harness<'static, SnekkieApp>) -> snekkie::ui::invade::InvadeStatus {
    app.state().invade_status().expect("Invade is open")
}

fn run<'a>(app: &'a mut Harness<'static, SnekkieApp>) -> &'a mut snekkie::invade::InvadeRun {
    app.state_mut().invade_tab().unwrap().run_mut()
}

/// Skip the start countdown and let the arena take the keyboard.
fn play(app: &mut Harness<'static, SnekkieApp>) {
    start_run(app);
    run(app).skip_to_wave(1);
    frames(app, 4);
    assert_eq!(status(app).state, "Playing");
}

fn level_up(app: &mut Harness<'static, SnekkieApp>) {
    let r = run(app);
    let (x, y) = (r.player().x, r.player().y + 0.5);
    let needed = r.experience_needed() - r.experience();
    r.spawn_experience(x, y, needed);
    frames(app, 4);
    assert_eq!(status(app).state, "Choosing an upgrade");
}

fn press(app: &mut Harness<'static, SnekkieApp>, key: Key) {
    app.key_press(key);
    frames(app, 2);
}

/// Harness::event steps once per event. These raw events share one frame
/// with any keyboard/clipboard events added to input_mut() before step().
fn batch_click(app: &mut Harness<'static, SnekkieApp>, position: egui::Pos2, button: egui::PointerButton) {
    let events = &mut app.input_mut().events;
    events.push(egui::Event::PointerMoved(position));
    for pressed in [true, false] {
        events.push(egui::Event::PointerButton { pos: position, button, pressed, modifiers: Modifiers::NONE });
    }
}

fn records(dir: &tempfile::TempDir) -> Vec<serde_json::Value> {
    let path = dir.path().join("invade.json");
    if !path.exists() {
        return Vec::new();
    }
    let text = std::fs::read_to_string(path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    json["records"].as_array().cloned().unwrap_or_default()
}

#[test]
fn a_loadout_is_chosen_before_start_and_unstarted_setup_records_no_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_loadout(&mut app);
    assert_eq!(status(&app).state, "Choose loadout");
    assert!(app.state().update_would_install_automatically());
    frames(&mut app, 120);
    assert_eq!(run(&mut app).elapsed(), 0.0, "setup never advances the simulation");
    app.get_by_role_and_label(Role::Label, "Invade").click_button(egui::PointerButton::Middle);
    frames(&mut app, 3);
    app.state_mut().shutdown();
    assert!(records(&dir).is_empty(), "choosing nothing is not a failed run");

    open_loadout(&mut app);
    app.get_by_label_contains("Rail").click();
    frames(&mut app, 2);
    app.get_by_label_contains("Payload").click();
    frames(&mut app, 2);
    app.get_by_label("Start run").focus();
    press(&mut app, Key::Enter);
    assert_eq!(status(&app).state, "Get ready");
    assert_eq!(run(&mut app).weapons()[0].kind, snekkie::invade::arsenal::WeaponKind::Rail);
    assert_eq!(run(&mut app).build().rank(snekkie::invade::UpgradeId::Payload), 1);
    assert!(!app.state().update_would_install_automatically());
}

#[test]
fn starting_and_retrying_require_fresh_thrust_and_shift_after_setup() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "core");
    open_loadout(&mut app);
    app.input_mut().events.extend([
        egui::Event::ModifiersChanged(Modifiers::SHIFT),
        egui::Event::Key { key: Key::W, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::SHIFT },
    ]);
    frames(&mut app, 3);

    for retry in [false, true] {
        if retry {
            run(&mut app).lose();
            frames(&mut app, 3);
            assert_eq!(status(&app).state, "Game over");
            app.get_by_label_contains("Retry").click();
            frames(&mut app, 3);
            assert_eq!(status(&app).state, "Choose loadout");
            assert_eq!(records(&dir).len(), 1, "Retry has not started another run");
        }
        assert!(app.ctx.input(|input| input.keys_down.contains(&Key::W) && input.modifiers.shift));
        start_run(&mut app);
        assert!(app.ctx.input(|input| input.keys_down.contains(&Key::W) && input.modifiers.shift));
        let r = run(&mut app);
        r.clear_asteroids();
        r.set_flight(std::f32::consts::FRAC_PI_2, (0.0, 0.0));
        r.skip_to_wave(1);
        frames(&mut app, 6);
        app.input_mut().events.extend([
            egui::Event::Key {
                key: Key::W,
                physical_key: None,
                pressed: true,
                repeat: true,
                modifiers: Modifiers::SHIFT,
            },
            egui::Event::Text("W".into()),
        ]);
        frames(&mut app, 6);
        let player = run(&mut app).player();
        assert!(!player.thrusting, "held or repeated W cannot start thrust across setup");
        assert_eq!(player.speed(), 0.0);
        assert_eq!(player.mobility_cooldown, 0.0, "held Shift cannot start Dash across setup");
        assert!(app.state_mut().invade_tab().unwrap().held_keys().is_empty());

        app.input_mut().events.extend([
            egui::Event::ModifiersChanged(Modifiers::NONE),
            egui::Event::Key {
                key: Key::W,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
        ]);
        frames(&mut app, 3);
        app.key_down(Key::W);
        frames(&mut app, 3);
        assert!(run(&mut app).player().thrusting, "a fresh W press starts thrust");
        app.input_mut().events.push(egui::Event::ModifiersChanged(Modifiers::SHIFT));
        frames(&mut app, 3);
        assert!(run(&mut app).player().mobility_cooldown > 0.0, "a fresh Shift press starts Dash");
        assert_device_received_nothing(&mut device);
    }
}

#[test]
fn experience_requires_collection_and_the_camera_follows_into_a_level_choice() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    let r = run(&mut app);
    r.clear_asteroids();
    r.set_flight(std::f32::consts::FRAC_PI_2, (0.0, 0.0));
    let (x, y) = (r.player().x, r.player().y + 0.5);
    let needed = r.experience_needed();
    r.spawn_experience(x + 16.0, y, needed);
    frames(&mut app, 10);
    assert_eq!(run(&mut app).level(), 1);
    assert_eq!(run(&mut app).experience(), 0, "distant orbs do not award EXP");
    app.key_down(Key::W);
    frames(&mut app, 30);
    app.key_up(Key::W);
    frames(&mut app, 2);
    assert_eq!(run(&mut app).level(), 2);
    assert_eq!(status(&app).state, "Choosing an upgrade");
    let r = run(&mut app);
    let (camera_x, camera_y) = r.camera();
    assert!((camera_x + 40.0 - r.player().x).abs() < 1e-3);
    assert!((camera_y + 14.0 - r.player().y - 0.5).abs() < 1e-3);
    let time = r.elapsed();
    frames(&mut app, 30);
    assert_eq!(run(&mut app).elapsed(), time, "level choices freeze combat");
    press(&mut app, Key::Num1);
    assert_ne!(status(&app).state, "Choosing an upgrade");
}

#[test]
fn overflow_experience_queues_each_level_without_resuming_combat_between_choices() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    let r = run(&mut app);
    let (x, y) = (r.player().x, r.player().y + 0.5);
    r.spawn_experience(x, y, 12 + 20 + 5);
    frames(&mut app, 3);
    assert_eq!(run(&mut app).level(), 2);
    let time = run(&mut app).elapsed();
    press(&mut app, Key::Num1);
    assert_eq!(run(&mut app).level(), 3);
    assert_eq!(status(&app).state, "Choosing an upgrade");
    assert_eq!(run(&mut app).elapsed(), time);
    press(&mut app, Key::Num1);
    assert_ne!(status(&app).state, "Choosing an upgrade");
    assert_eq!(run(&mut app).experience(), 5);
}

fn open_raw_tab(app: &mut Harness<'static, SnekkieApp>, name: &str) -> std::net::TcpStream {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    open_raw_tab_on(app, name, &listener)
}

fn open_raw_tab_on(
    app: &mut Harness<'static, SnekkieApp>,
    name: &str,
    listener: &std::net::TcpListener,
) -> std::net::TcpStream {
    let ctx = app.ctx.clone();
    app.state_mut().open_session(
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
    while app.state().active_screen_text().is_none_or(|text| text.trim() != prompt) {
        assert!(Instant::now() < deadline, "{name} never connected");
        app.run_ok();
        std::thread::sleep(Duration::from_millis(20));
    }
    app.run_ok();
    device
}

fn assert_device_received_nothing(device: &mut std::net::TcpStream) {
    device.set_read_timeout(Some(Duration::from_millis(150))).unwrap();
    match device.read(&mut [0; 64]) {
        Err(error) => {
            assert!(matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
        }
        Ok(n) => panic!("the device received {n} bytes"),
    }
}

#[test]
fn help_opens_one_invade_tab_with_zero_one_and_several_connections() {
    for count in [0, 1, 3] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = harness(&dir);
        let mut devices: Vec<_> = (0..count).map(|i| open_raw_tab(&mut app, &format!("dev{i}"))).collect();
        open_invade(&mut app);
        let titles = app.state().workspace_titles();
        assert_eq!(titles.iter().filter(|t| *t == "Invade").count(), 1);
        assert_eq!(titles.last().map(String::as_str), Some("Invade"));
        assert_eq!(app.state().active_title().as_deref(), Some("Invade"));
        let first = status(&app);
        assert_eq!((first.rig, first.wave, first.state), (Rig::Probe, 1, "Get ready"));
        assert!(app.state().tab_titles().len() == count, "the game is not a session");

        play(&mut app);
        frames(&mut app, 30);
        let elapsed = run(&mut app).elapsed();
        // Help again shows the same run; the menu paused it.
        open_invade(&mut app);
        assert_eq!(app.state().workspace_titles(), titles);
        assert_eq!(status(&app).state, "Paused");
        assert!(run(&mut app).elapsed() >= elapsed, "not restarted");
        for device in &mut devices {
            assert_device_received_nothing(device);
        }
    }
}

#[test]
fn about_owns_the_invade_launcher_and_keyboard_activation_closes_it_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "core");
    app.get_by_label("Help").click();
    frames(&mut app, 3);
    assert!(app.query_by_role_and_label(Role::Button, "Invade").is_none());
    app.get_by_role_and_label(Role::Button, "About Snekkie").click();
    frames(&mut app, 3);
    app.get_by_role_and_label(Role::Button, "Invade");
    press(&mut app, Key::Enter);
    assert!(app.state().dialog_texts().is_empty(), "Enter dismisses the focused OK button");
    assert!(app.state().invade_status().is_none(), "opening About does not start a game");
    assert_device_received_nothing(&mut device);

    app.get_by_label("Help").click();
    frames(&mut app, 3);
    app.get_by_role_and_label(Role::Button, "About Snekkie").click();
    frames(&mut app, 3);
    app.get_by_role_and_label(Role::Button, "Invade").focus();
    frames(&mut app, 2);
    press(&mut app, Key::Enter);
    assert!(app.state().dialog_texts().is_empty(), "About is gone when Invade opens");
    assert_eq!(app.state().active_title().as_deref(), Some("Invade"));
    assert_eq!(status(&app).state, "Choose loadout");
    assert_device_received_nothing(&mut device);
}

#[test]
fn later_runs_use_the_saved_rig_only_if_it_is_unlocked() {
    for (progress, expected) in [
        (r#"{"version":1,"selected_rig":"ghost","unlocked":["ghost"]}"#, Rig::Ghost),
        (r#"{"version":1,"selected_rig":"bastion","unlocked":["ghost"]}"#, Rig::Probe),
        (r#"{"version":1}"#, Rig::Probe),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("invade.json"), progress).unwrap();
        let mut app = harness(&dir);
        open_invade(&mut app);
        assert_eq!(status(&app).rig, expected, "{progress}");
    }
}

#[test]
fn leaving_menus_dialogs_blur_minimizing_and_esc_pause_and_resume_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "core");
    open_invade(&mut app);
    play(&mut app);
    let resume = |app: &mut Harness<'static, SnekkieApp>| {
        app.get_by_label("Resume").click();
        frames(app, 3);
        assert_eq!(status(app).state, "Get ready", "a one-second countdown first");
        frames(app, 34);
        assert_eq!(status(app).state, "Playing");
    };

    // Another tab, and back: still paused.
    next_tab(&mut app, false);
    assert_eq!(app.state().active_title().as_deref(), Some("core"));
    next_tab(&mut app, false);
    assert_eq!(app.state().active_title().as_deref(), Some("Invade"));
    assert!(status(&app).paused);
    frames(&mut app, 20);
    assert!(status(&app).paused, "returning needs Resume");
    resume(&mut app);

    // An app menu.
    app.get_by_label("View").click();
    frames(&mut app, 2);
    assert!(status(&app).paused);
    app.key_press(Key::Escape);
    frames(&mut app, 2);
    assert!(status(&app).paused);
    resume(&mut app);

    // A dialog.
    app.get_by_label("Help").click();
    frames(&mut app, 2);
    app.get_by_label("Keyboard shortcuts").click();
    frames(&mut app, 2);
    assert!(status(&app).paused);
    app.get_by_label("OK").click();
    frames(&mut app, 2);
    assert!(status(&app).paused);
    resume(&mut app);

    // The window losing focus, then being minimized.
    for minimized in [false, true] {
        let viewport = app.input_mut().viewports.entry(ViewportId::ROOT).or_default();
        if minimized {
            viewport.minimized = Some(true);
        } else {
            viewport.focused = Some(false);
        }
        frames(&mut app, 2);
        assert!(status(&app).paused, "minimized {minimized}");
        let viewport = app.input_mut().viewports.entry(ViewportId::ROOT).or_default();
        viewport.focused = Some(true);
        viewport.minimized = Some(false);
        frames(&mut app, 2);
        resume(&mut app);
    }

    // Esc and P from the keyboard.
    press(&mut app, Key::Escape);
    assert!(status(&app).paused);
    press(&mut app, Key::P);
    assert_eq!(status(&app).state, "Get ready");
    assert_device_received_nothing(&mut device);
}

#[test]
fn build_and_upgrade_screens_work_from_the_keyboard_and_mouse() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    run(&mut app).clear_asteroids();
    press(&mut app, Key::B);
    app.get_by_label("BUILD");
    app.get_by_label_contains("No modules yet");
    press(&mut app, Key::Escape);
    app.get_by_label("PAUSED");
    press(&mut app, Key::Escape);
    assert_eq!(status(&app).state, "Get ready");
    frames(&mut app, 34);

    for (wave, choose_with_key) in [(3, true), (6, false)] {
        run(&mut app).skip_to_wave(wave);
        frames(&mut app, 2);
        level_up(&mut app);
        frames(&mut app, 3);
        assert_eq!(status(&app).state, "Choosing an upgrade", "wave {wave}");
        app.get_by_label_contains("CHOOSE ONE");
        let cards = run(&mut app).cards();
        assert!(!cards.is_empty() && cards.len() <= 3);
        if choose_with_key {
            press(&mut app, Key::Num2);
        } else {
            app.get_by_label_contains("[1]").click();
            frames(&mut app, 3);
        }
        assert_ne!(status(&app).state, "Choosing an upgrade");
    }
    let r = run(&mut app);
    let ranks: u8 = r.build().acquired().map(|(_, rank)| rank).sum::<u8>()
        + r.weapons().iter().map(|weapon| weapon.rank).sum::<u8>();
    assert_eq!(ranks, 3, "two choices improve modules or weapons beyond the starter");
    // Choosing never left a key held.
    let player = run(&mut app).player();
    let flight = (player.x, player.y, player.heading, player.velocity);
    frames(&mut app, 30);
    let player = run(&mut app).player();
    assert_eq!((player.x, player.y, player.heading, player.velocity), flight);
}

#[test]
fn small_windows_pause_with_a_usable_close_and_the_threshold_window_plays() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness_sized(&dir, [640.0, 400.0]);
    open_invade(&mut app);
    play(&mut app);
    assert!(app.query_by_label_contains("Enlarge the window").is_none());
    // Shrinking the window pauses the game.
    app.set_size(egui::vec2(600.0, 380.0));
    frames(&mut app, 3);
    assert!(status(&app).paused);
    app.get_by_label_contains("Enlarge the window to at least 640 × 400");

    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    app.set_size(egui::vec2(520.0, 320.0));
    frames(&mut app, 3);
    app.get_by_label_contains("Enlarge the window to at least 640 × 400");
    let close = app.get_by_label("Close Invade").rect();
    assert!(close.right() <= 520.0 && close.bottom() <= 320.0, "{close:?}");
    app.get_by_label("Close Invade").click();
    frames(&mut app, 3);
    assert!(app.state().invade_status().is_none());
    assert!(app.state().workspace_titles().is_empty());
    assert_eq!(records(&dir)[0]["ending"], "closed");
}

#[test]
fn reordering_cycling_and_closing_keep_stable_targets() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let first = open_raw_tab(&mut app, "first");
    let _second = open_raw_tab(&mut app, "second");
    open_invade(&mut app);
    play(&mut app);
    app.state_mut().move_tab("Invade", 1);
    frames(&mut app, 2);
    assert_eq!(app.state().workspace_titles(), ["first", "Invade", "second"]);
    assert_eq!(app.state().active_title().as_deref(), Some("Invade"), "the selection moved with its tab");
    next_tab(&mut app, false);
    assert_eq!(app.state().active_title().as_deref(), Some("second"));
    next_tab(&mut app, true);
    assert_eq!(app.state().active_title().as_deref(), Some("Invade"));
    next_tab(&mut app, true);
    assert_eq!(app.state().active_title().as_deref(), Some("first"));
    next_tab(&mut app, false);
    app.get_by_label("Resume").click();
    frames(&mut app, 40);
    assert_eq!(status(&app).state, "Playing");

    // A background connection disconnects and its X closes it: the game
    // stays selected, running and in control of the keyboard.
    drop(first);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !app.state().workspace_titles().contains(&"first (closed)".to_string()) {
        assert!(Instant::now() < deadline);
        frames(&mut app, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    app.get_all_by_label("×").next().unwrap().click();
    frames(&mut app, 4);
    assert_eq!(app.state().workspace_titles(), ["Invade", "second"]);
    assert_eq!(app.state().active_title().as_deref(), Some("Invade"));
    assert_eq!(status(&app).state, "Playing");
    let r = run(&mut app);
    r.clear_asteroids();
    r.set_flight(0.0, (0.0, 0.0));
    let position = (r.player().x, r.player().y);
    let heading = r.player().heading;
    app.key_down(Key::ArrowRight);
    frames(&mut app, 6);
    app.key_up(Key::ArrowRight);
    frames(&mut app, 1);
    let player = run(&mut app).player();
    assert_ne!(player.heading, heading, "the arena kept the turn key");
    assert_eq!((player.x, player.y), position, "turning alone does not translate the ship");
    let heading = player.heading;
    frames(&mut app, 6);
    assert_eq!(run(&mut app).player().heading, heading, "releasing the turn key stops rotation");

    // Closing the selected game selects its neighbor and records the run.
    app.key_press_modifiers(app_keys(false), Key::W);
    frames(&mut app, 3);
    assert_eq!(app.state().workspace_titles(), ["second"]);
    assert_eq!(app.state().active_title().as_deref(), Some("second"));
    assert_eq!(records(&dir).len(), 1);
}

#[test]
fn the_game_tab_menu_has_game_actions_and_connection_actions_keep_their_targets() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut old = open_raw_tab_on(&mut app, "core", &listener);
    open_invade(&mut app);
    play(&mut app);

    app.get_by_role_and_label(Role::Label, "Invade").click_secondary();
    frames(&mut app, 2);
    app.get_by_label("Show build");
    assert!(app.query_by_label("Reconnect").is_none() && app.query_by_label("Tab color").is_none());
    app.get_by_role_and_label(Role::Button, "Pause").click();
    frames(&mut app, 2);
    assert!(status(&app).paused);

    // Reconnecting a background connection from its own menu targets it.
    app.get_by_label("core").click_secondary();
    frames(&mut app, 2);
    app.get_by_label("Reconnect").click();
    frames(&mut app, 2);
    listener.set_nonblocking(false).unwrap();
    let (mut replacement, _) = listener.accept().unwrap();
    replacement.write_all(b"core again>").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.state().active_screen_text().is_none_or(|t| !t.contains("core again>")) {
        assert!(Instant::now() < deadline);
        frames(&mut app, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.state().active_title().as_deref(), Some("core"));
    assert!(status(&app).paused);
    old.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    assert_eq!(old.read(&mut [0; 8]).unwrap_or(0), 0, "the old connection hung up");
    assert_device_received_nothing(&mut replacement);
}

#[test]
fn game_keys_paste_menus_and_tab_switches_send_nothing_to_devices() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "core");
    open_invade(&mut app);
    play(&mut app);
    for key in [Key::Space, Key::ArrowLeft, Key::ArrowRight, Key::A, Key::D, Key::X, Key::Num1, Key::Num2, Key::Num3] {
        app.key_press(key);
        app.event(egui::Event::Text(key.symbol_or_name().to_lowercase()));
        frames(&mut app, 1);
    }
    app.event(egui::Event::Text(" ".into()));
    app.event(egui::Event::Paste("show running-config\n".into()));
    app.event(egui::Event::Copy);
    frames(&mut app, 2);
    app.get_by_label("Invade arena").click_secondary();
    frames(&mut app, 2);
    app.get_by_label("Invade arena").click_button(egui::PointerButton::Middle);
    frames(&mut app, 2);
    app.key_press_modifiers(app_keys(true), Key::V);
    frames(&mut app, 2);
    app.get_by_label("Edit").click();
    frames(&mut app, 2);
    app.get_by_label_contains("Paste").click();
    frames(&mut app, 2);
    let ctx = app.ctx.clone();
    app.state_mut().paste_clipboard_text(&ctx, "configure terminal\n");
    frames(&mut app, 2);
    assert!(!app.state().paste_running());
    assert!(app.state().dialog_texts().is_empty(), "no paste review opened for the game");
    for key in [Key::B, Key::Escape, Key::Escape, Key::P] {
        press(&mut app, key);
    }
    assert_device_received_nothing(&mut device);

    // Hold Space in the game, switch to the terminal, and let the key
    // repeat there: nothing reaches the device until it is released.
    app.get_by_label("Resume").click();
    frames(&mut app, 40);
    app.key_down(Key::Space);
    frames(&mut app, 3);
    next_tab(&mut app, false);
    assert_eq!(app.state().active_title().as_deref(), Some("core"));
    for _ in 0..5 {
        app.event(egui::Event::Key {
            key: Key::Space,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        });
        app.event(egui::Event::Text(" ".into()));
        frames(&mut app, 1);
    }
    assert_device_received_nothing(&mut device);
    app.key_up(Key::Space);
    frames(&mut app, 2);
    app.event(egui::Event::Text("z".into()));
    frames(&mut app, 3);
    device.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut byte = [0; 1];
    device.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"z", "typing works again once the key is released");
}

#[test]
fn regression_game_controls_stay_off_the_wire_on_mouse_transitions_and_after_pause() {
    for transition in ["select", "close", "paused"] {
        for (key, text) in [
            (Key::Space, " "),
            (Key::A, "a"),
            (Key::X, "x"),
            (Key::W, "w"),
            (Key::S, "s"),
            (Key::ArrowUp, ""),
            (Key::ArrowDown, ""),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = harness(&dir);
            let mut device = open_raw_tab(&mut app, "core");
            open_invade(&mut app);
            play(&mut app);
            app.key_down(key);
            frames(&mut app, 3);
            match transition {
                "select" => {
                    let position = app.get_by_label("core").rect().center();
                    batch_click(&mut app, position, egui::PointerButton::Primary);
                }
                "close" => {
                    let position = app.get_by_role_and_label(Role::Label, "Invade").rect().center();
                    batch_click(&mut app, position, egui::PointerButton::Middle);
                }
                _ => {
                    // Pausing clears the game's logical held state, but the
                    // physical key is still down when the terminal returns.
                    app.get_by_label("View").click();
                    frames(&mut app, 2);
                    press(&mut app, Key::Escape);
                    assert!(status(&app).paused);
                    app.key_press_modifiers(Modifiers::CTRL, Key::Tab);
                }
            }
            // A repeat can share the mouse transition's frame, before the
            // newly selected terminal is painted for the first time.
            app.input_mut().events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: true,
                modifiers: Modifiers::NONE,
            });
            app.input_mut().events.push(egui::Event::Text(text.into()));
            frames(&mut app, 2);
            assert_eq!(app.state().active_title().as_deref(), Some("core"), "{transition} {key:?}");
            app.event(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: true,
                modifiers: Modifiers::NONE,
            });
            app.event(egui::Event::Text(text.into()));
            frames(&mut app, 2);
            assert_device_received_nothing(&mut device);
            app.key_up(key);
            frames(&mut app, 2);
            app.event(egui::Event::Text("z".into()));
            frames(&mut app, 2);
            device.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut byte = [0; 1];
            device.read_exact(&mut byte).unwrap_or_else(|e| panic!("{transition} {key:?}: {e}"));
            assert_eq!(&byte, b"z", "typing works again after {transition} {key:?} is released");
        }
    }
}

#[test]
fn shift_mobility_is_quarantined_until_release_after_game_exit() {
    for close in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = harness(&dir);
        let mut device = open_raw_tab(&mut app, "core");
        open_invade(&mut app);
        play(&mut app);
        app.input_mut().events.push(egui::Event::ModifiersChanged(Modifiers::SHIFT));
        frames(&mut app, 2);
        let position = if close {
            app.get_by_role_and_label(Role::Label, "Invade").rect().center()
        } else {
            app.get_by_label("core").rect().center()
        };
        batch_click(&mut app, position, if close { egui::PointerButton::Middle } else { egui::PointerButton::Primary });
        frames(&mut app, 2);
        assert_eq!(app.state().active_title().as_deref(), Some("core"));
        app.input_mut().events.extend([
            egui::Event::Key {
                key: Key::ArrowLeft,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::SHIFT,
            },
            egui::Event::Text("Z".into()),
        ]);
        frames(&mut app, 2);
        assert_device_received_nothing(&mut device);
        app.input_mut().events.extend([
            egui::Event::Key {
                key: Key::ArrowLeft,
                physical_key: None,
                pressed: true,
                repeat: true,
                modifiers: Modifiers::SHIFT,
            },
            egui::Event::Text("Z".into()),
            egui::Event::ModifiersChanged(Modifiers::NONE),
        ]);
        frames(&mut app, 2);
        assert_device_received_nothing(&mut device);
        app.key_up(Key::ArrowLeft);
        frames(&mut app, 2);
        app.event(egui::Event::Text("z".into()));
        frames(&mut app, 2);
        device.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut byte = [0; 1];
        device.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"z", "typing recovers after the game modifier is released");

        app.input_mut().events.extend([egui::Event::ModifiersChanged(Modifiers::SHIFT), egui::Event::Text("Z".into())]);
        frames(&mut app, 2);
        device.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"Z", "a fresh Shift press belongs to the terminal");
    }
}

#[test]
fn regression_mouse_exit_frame_does_not_submit_text_or_clipboard_to_a_device() {
    for close in [false, true] {
        for paste in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut app = harness(&dir);
            let mut device = open_raw_tab(&mut app, "core");
            open_invade(&mut app);
            play(&mut app);
            if close {
                let position = app.get_by_role_and_label(Role::Label, "Invade").rect().center();
                batch_click(&mut app, position, egui::PointerButton::Middle);
            } else {
                let position = app.get_by_label("core").rect().center();
                batch_click(&mut app, position, egui::PointerButton::Primary);
            }
            // These events arrived while the game was selected. The
            // terminal shown by this click must not consume any of them.
            app.input_mut().events.push(if paste {
                egui::Event::Paste("hostname wrong".into())
            } else {
                egui::Event::Text("hostname wrong".into())
            });
            frames(&mut app, 2);
            assert_eq!(app.state().active_title().as_deref(), Some("core"));
            assert_device_received_nothing(&mut device);
            assert!(!app.state().paste_running());
            assert!(app.state().dialog_texts().is_empty());
        }
    }
}

#[test]
fn game_origin_shortcuts_cannot_clear_a_terminal_selected_in_the_same_frame() {
    for (close, key) in [(false, Key::L), (true, Key::L), (false, Key::W), (true, Key::W)] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = harness(&dir);
        let mut device = open_raw_tab(&mut app, "core");
        device.write_all(b"\r\nPreserve the working session>").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.state().active_screen_text().is_none_or(|text| !text.contains("Preserve the working session>")) {
            assert!(Instant::now() < deadline);
            frames(&mut app, 1);
            std::thread::sleep(Duration::from_millis(10));
        }
        let before = app.state().active_screen_text().unwrap();
        open_invade(&mut app);
        play(&mut app);
        let position = if close {
            app.get_by_role_and_label(Role::Label, "Invade").rect().center()
        } else {
            app.get_by_label("core").rect().center()
        };
        batch_click(&mut app, position, if close { egui::PointerButton::Middle } else { egui::PointerButton::Primary });
        let modifiers = app_keys(key == Key::L);
        app.input_mut().events.extend([
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers },
            egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers },
            egui::Event::ModifiersChanged(Modifiers::NONE),
        ]);
        frames(&mut app, 2);
        assert_eq!(app.state().active_title().as_deref(), Some("core"));
        assert_eq!(app.state().active_screen_text().as_deref(), Some(before.as_str()));
        assert!(app.state().dialog_texts().is_empty(), "the connection was not targeted by {key:?}");
        if key == Key::W {
            assert!(app.state().invade_status().is_none());
            assert_eq!(app.state().workspace_titles(), ["core"]);
            assert_eq!(records(&dir).len(), 1, "Invade closes exactly once");
        }
        assert_device_received_nothing(&mut device);
    }
}

#[test]
fn regression_sidebar_editing_keeps_pause_shortcuts_and_upgrade_choices() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    app.get_by_label("Show sessions").click();
    frames(&mut app, 3);
    app.get_by_role_and_label(Role::TextInput, "Filter saved sessions").click();
    frames(&mut app, 3);
    for (key, text) in [(Key::P, "p"), (Key::B, "b"), (Key::Escape, "")] {
        app.key_press(key);
        if !text.is_empty() {
            app.event(egui::Event::Text(text.into()));
        }
        frames(&mut app, 3);
        assert_eq!(status(&app).state, "Paused", "sidebar owns {key:?}");
        assert!(app.query_by_label("Connect").is_some(), "the sidebar stays visible after {key:?}");
    }
    app.get_by_label("Resume").click();
    frames(&mut app, 40);
    run(&mut app).skip_to_wave(3);
    frames(&mut app, 2);
    level_up(&mut app);
    frames(&mut app, 3);
    assert_eq!(status(&app).state, "Choosing an upgrade");
    app.get_by_label("Show sessions").click();
    frames(&mut app, 3);
    app.get_by_role_and_label(Role::TextInput, "Filter saved sessions").click();
    frames(&mut app, 3);
    for key in [Key::Num1, Key::Num2, Key::Num3] {
        app.key_press(key);
        app.event(egui::Event::Text(key.symbol_or_name().into()));
        frames(&mut app, 3);
        assert_eq!(status(&app).state, "Choosing an upgrade", "sidebar owns {key:?}");
    }
}

#[test]
fn regression_upgrade_offers_accept_tab_then_enter() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    run(&mut app).skip_to_wave(3);
    frames(&mut app, 2);
    app.key_down(Key::Space);
    frames(&mut app, 2);
    level_up(&mut app);
    frames(&mut app, 3);
    assert_eq!(status(&app).state, "Choosing an upgrade");
    app.event(egui::Event::Key {
        key: Key::Space,
        physical_key: None,
        pressed: true,
        repeat: true,
        modifiers: Modifiers::NONE,
    });
    app.event(egui::Event::Text(" ".into()));
    frames(&mut app, 2);
    assert_eq!(status(&app).state, "Choosing an upgrade", "held fire does not accept an offer");
    app.key_up(Key::Space);
    frames(&mut app, 2);
    press(&mut app, Key::Tab);
    press(&mut app, Key::Enter);
    assert_ne!(status(&app).state, "Choosing an upgrade");
    let r = run(&mut app);
    assert_eq!(
        r.build().acquired().map(|(_, rank)| u32::from(rank)).sum::<u32>()
            + r.weapons().iter().map(|weapon| u32::from(weapon.rank)).sum::<u32>(),
        2
    );
}

#[test]
fn held_fire_cannot_retry_after_game_over() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    app.key_down(Key::Space);
    frames(&mut app, 2);
    run(&mut app).lose();
    frames(&mut app, 3);
    assert_eq!(status(&app).state, "Game over");
    assert_eq!(records(&dir).len(), 1);
    app.input_mut().events.extend([
        egui::Event::Key {
            key: Key::Space,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        },
        egui::Event::Text(" ".into()),
    ]);
    frames(&mut app, 3);
    assert_eq!(status(&app).state, "Game over", "held fire cannot activate Retry");
    assert_eq!(records(&dir).len(), 1, "the completed run is recorded once");
    app.key_up(Key::Space);
    frames(&mut app, 2);
    app.get_by_label_contains("Retry with").click();
    frames(&mut app, 3);
    assert_eq!(status(&app).state, "Choose loadout", "Retry opens setup before the next run");
    start_run(&mut app);
    assert_eq!(status(&app).state, "Get ready");
    assert_eq!(records(&dir).len(), 1);
}

#[test]
fn device_output_and_terminal_contents_survive_time_spent_playing() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "core");
    let before = app.state().active_screen_text().unwrap();
    open_invade(&mut app);
    play(&mut app);
    device.write_all(b"\r\nwhile you were playing>").unwrap();
    for _ in 0..40 {
        frames(&mut app, 1);
        std::thread::sleep(Duration::from_millis(5));
    }
    next_tab(&mut app, false);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.state().active_screen_text().is_none_or(|t| !t.contains("while you were playing>")) {
        assert!(Instant::now() < deadline);
        frames(&mut app, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(app.state().active_screen_text().unwrap().contains(before.trim()));
    assert_device_received_nothing(&mut device);
}

#[test]
fn paced_pastes_keep_going_while_playing_and_app_dialogs_still_hold_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "core");
    let lines: Vec<String> = (0..24).map(|i| format!("line {i:02}")).collect();
    let expected: String = lines.iter().map(|l| format!("{l}\r")).collect();
    let ctx = app.ctx.clone();
    app.state_mut().paste_clipboard_text(&ctx, &format!("{}\n", lines.join("\n")));
    app.run_ok();
    app.get_by_label("Send paste").click();
    frames(&mut app, 2);
    assert!(app.state().paste_running());
    open_invade(&mut app);
    play(&mut app);
    // Destination, progress and Stop stay in view over the game.
    app.get_by_label("Stop paste");
    app.get_by_label_contains("Paste: ");
    let mut received = Vec::new();
    let read_now = |device: &mut std::net::TcpStream, received: &mut Vec<u8>| {
        device.set_nonblocking(true).unwrap();
        let mut buffer = [0; 256];
        while let Ok(n) = device.read(&mut buffer) {
            if n == 0 {
                break;
            }
            received.extend_from_slice(&buffer[..n]);
        }
        device.set_nonblocking(false).unwrap();
    };
    for _ in 0..30 {
        frames(&mut app, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    read_now(&mut device, &mut received);
    assert!(!received.is_empty(), "lines went out while playing");
    // A real app dialog still holds the queue.
    app.get_by_label("Help").click();
    frames(&mut app, 2);
    app.get_by_label("Keyboard shortcuts").click();
    frames(&mut app, 2);
    std::thread::sleep(Duration::from_millis(150));
    read_now(&mut device, &mut received);
    let held = received.len();
    for _ in 0..30 {
        frames(&mut app, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    read_now(&mut device, &mut received);
    assert_eq!(received.len(), held, "the dialog paused the queue");
    assert!(app.state().paste_running());
    app.get_by_label("OK").click();
    frames(&mut app, 2);
    let deadline = Instant::now() + Duration::from_secs(20);
    while app.state().paste_running() {
        assert!(Instant::now() < deadline, "the paste stalled while playing");
        frames(&mut app, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while received.len() < expected.len() {
        assert!(Instant::now() < deadline);
        read_now(&mut device, &mut received);
    }
    assert_eq!(String::from_utf8(received).unwrap(), expected);
    assert_eq!(app.state().active_title().as_deref(), Some("Invade"));
}

#[test]
fn an_unfinished_run_blocks_automatic_update_installation() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    assert!(app.state().update_would_install_automatically());
    open_invade(&mut app);
    assert!(!app.state().update_would_install_automatically());
    run(&mut app).lose();
    frames(&mut app, 3);
    app.get_by_label_contains("RUN OVER");
    assert!(app.state().update_would_install_automatically(), "a finished run is not activity");
    app.get_by_label_contains("Retry").click();
    frames(&mut app, 3);
    assert!(app.state().update_would_install_automatically(), "setup has no active run");
    start_run(&mut app);
    assert!(!app.state().update_would_install_automatically());
}

#[test]
fn defeat_close_restart_and_shutdown_each_record_a_run_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    run(&mut app).lose();
    frames(&mut app, 3);
    assert_eq!(records(&dir).len(), 1);
    assert_eq!(records(&dir)[0]["ending"], "defeated");
    app.get_by_label_contains("Retry").click();
    frames(&mut app, 3);
    assert_eq!(status(&app).state, "Choose loadout", "retry opens a new loadout");
    assert_eq!(records(&dir).len(), 1, "retrying records nothing more");
    play(&mut app);
    press(&mut app, Key::P);
    app.get_by_label("Restart run…").click();
    frames(&mut app, 3);
    app.get_by_label("Restart").click();
    frames(&mut app, 3);
    assert_eq!(records(&dir).len(), 2);
    assert_eq!(status(&app).state, "Choose loadout", "restart opens a new loadout");
    start_run(&mut app);
    app.get_by_role_and_label(Role::Label, "Invade").click_button(egui::PointerButton::Middle);
    frames(&mut app, 3);
    assert_eq!(records(&dir).len(), 3);
    app.state_mut().shutdown();
    assert_eq!(records(&dir).len(), 3, "nothing left to record");
    open_invade(&mut app);
    app.state_mut().shutdown();
    app.state_mut().shutdown();
    let endings: Vec<String> = records(&dir).iter().map(|r| r["ending"].as_str().unwrap().to_string()).collect();
    let mut sorted = endings.clone();
    sorted.sort();
    assert_eq!(sorted, ["app_closed", "closed", "defeated", "restarted"]);
}

#[test]
fn unlocks_persist_when_earned_and_damaged_progress_files_are_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    run(&mut app).skip_to_wave(6);
    frames(&mut app, 2);
    run(&mut app).defeat_all();
    frames(&mut app, 4);
    let saved = std::fs::read_to_string(dir.path().join("invade.json")).unwrap();
    assert!(saved.contains("\"ghost\""), "unlocked before the run ends: {saved}");
    assert!(app.state_mut().invade_tab().unwrap().notices().iter().any(|n| n == "UNLOCKED  Ghost rig"));

    let dir = tempfile::tempdir().unwrap();
    let damaged = "{\"version\": 1, \"unlocked\": [\"ghost\"], oops";
    std::fs::write(dir.path().join("invade.json"), damaged).unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    press(&mut app, Key::P);
    app.get_by_label_contains("could not be loaded");
    run(&mut app).skip_to_wave(6);
    run(&mut app).defeat_all();
    frames(&mut app, 4);
    app.state_mut().shutdown();
    assert_eq!(std::fs::read_to_string(dir.path().join("invade.json")).unwrap(), damaged);
}

#[test]
fn the_sidebar_hides_over_the_game_without_changing_its_preference() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let _device = open_raw_tab(&mut app, "core");
    app.get_by_label("Connect");
    open_invade(&mut app);
    assert!(app.query_by_label("Connect").is_none(), "hidden over the game");
    play(&mut app);
    app.key_press_modifiers(app_keys(false), Key::B);
    frames(&mut app, 2);
    app.get_by_label("Connect");
    assert!(status(&app).paused, "showing the sidebar pauses the game");
    app.get_by_label("Resume").click();
    frames(&mut app, 2);
    assert!(app.query_by_label("Connect").is_none());
    next_tab(&mut app, false);
    app.get_by_label("Connect");
    assert!(
        !std::fs::read_to_string(dir.path().join("settings.json"))
            .unwrap_or_default()
            .contains("\"show_sidebar\": false")
    );
    // Terminal commands never act from the game tab.
    next_tab(&mut app, false);
    app.key_press_modifiers(app_keys(true), Key::R);
    frames(&mut app, 2);
    assert_eq!(app.state().active_title().as_deref(), Some("Invade"));
}

#[test]
fn exports_cannot_overwrite_the_progress_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    app.state_mut().shutdown();
    let path = app.state().invade_progress_path();
    assert_eq!(path, dir.path().join("invade.json"));
    assert!(path.exists());
}

/// The arena stays exactly as it was for a while: nothing moves, no time
/// passes in the run.
fn assert_frozen(app: &mut Harness<'static, SnekkieApp>) {
    let snapshot = |app: &mut Harness<'static, SnekkieApp>| {
        let r = run(app);
        let enemies: Vec<(u32, f32, f32)> = r.enemies().map(|e| (e.id, e.x, e.y)).collect();
        let player = r.player();
        let flight = (player.x, player.y, player.heading, player.velocity, player.thrusting, player.braking);
        (r.elapsed(), flight, enemies, r.asteroids().to_vec(), r.hostiles().len(), r.countdown())
    };
    let before = snapshot(app);
    frames(app, 20);
    assert_eq!(snapshot(app), before, "the game clock ran while paused");
}

#[test]
fn pausing_freezes_the_game_clock_for_every_interruption() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let _device = open_raw_tab(&mut app, "core");
    open_invade(&mut app);
    play(&mut app);
    frames(&mut app, 30);
    let resume = |app: &mut Harness<'static, SnekkieApp>| {
        app.get_by_label("Resume").click();
        frames(app, 40);
        assert_eq!(status(app).state, "Playing");
    };
    press(&mut app, Key::Escape);
    assert_frozen(&mut app);
    resume(&mut app);
    // A real app dialog with no menu involved: Preferences from its shortcut.
    app.key_press_modifiers(app_keys(false), Key::Comma);
    frames(&mut app, 2);
    assert!(status(&app).paused, "Preferences paused the game");
    assert_frozen(&mut app);
    app.get_by_label("Cancel").click();
    frames(&mut app, 3);
    assert!(status(&app).paused, "closing the dialog does not resume by itself");
    resume(&mut app);
    // Another tab.
    next_tab(&mut app, false);
    let elapsed = run(&mut app).elapsed();
    frames(&mut app, 20);
    assert_eq!(run(&mut app).elapsed(), elapsed);
    next_tab(&mut app, false);
    assert_frozen(&mut app);
}

#[test]
fn the_rig_chosen_while_paused_applies_to_the_next_run_and_locked_rigs_stay_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("invade.json"), r#"{"version":1,"unlocked":["ghost"]}"#).unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    press(&mut app, Key::P);
    assert!(app.get_by_label_contains("Bastion \u{2014} locked").accesskit_node().is_disabled());
    app.get_by_label_contains("Ghost \u{2014} ").click();
    frames(&mut app, 3);
    assert_eq!(status(&app).rig, Rig::Probe, "the current run keeps its rig");
    let saved = |dir: &tempfile::TempDir| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("invade.json")).unwrap()).unwrap()
    };
    assert_eq!(saved(&dir)["selected_rig"], "ghost");
    app.get_by_label_contains("Bastion \u{2014} locked").click();
    frames(&mut app, 3);
    assert_eq!(saved(&dir)["selected_rig"], "ghost", "a locked rig cannot be chosen");
    app.get_by_label("Restart run\u{2026}").click();
    frames(&mut app, 3);
    app.get_by_label("Restart").click();
    frames(&mut app, 3);
    start_run(&mut app);
    assert_eq!(status(&app).rig, Rig::Ghost);
}

#[test]
fn compromises_accumulate_across_runs_and_unlock_fragment_at_one_hundred() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("invade.json"), r#"{"version":1,"devices_compromised":80}"#).unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    assert!(!run(&mut app).is_unlocked(snekkie::invade::Unlock::Fragment));
    run(&mut app).defeat_all();
    frames(&mut app, 4);
    let killed = run(&mut app).compromised();
    assert!(killed >= 20);
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("invade.json")).unwrap()).unwrap();
    assert_eq!(saved["devices_compromised"], 80 + killed);
    assert!(saved["unlocked"].as_array().unwrap().iter().any(|u| u == "fragment"));
}

#[test]
fn held_keys_let_go_during_the_upgrade_choice_do_not_steer_afterwards() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    run(&mut app).skip_to_wave(3);
    run(&mut app).clear_asteroids();
    frames(&mut app, 2);
    app.key_down(Key::ArrowRight);
    app.key_down(Key::Space);
    frames(&mut app, 4);
    level_up(&mut app);
    frames(&mut app, 3);
    assert_eq!(status(&app).state, "Choosing an upgrade");
    // Let go while the cards are up, where the game does not read keys.
    app.key_up(Key::ArrowRight);
    app.key_up(Key::Space);
    frames(&mut app, 2);
    press(&mut app, Key::Num1);
    assert_ne!(status(&app).state, "Choosing an upgrade");
    let player = run(&mut app).player();
    let flight = (player.x, player.y, player.heading, player.velocity);
    let shots = run(&mut app).shots().len();
    frames(&mut app, 40);
    let player = run(&mut app).player();
    assert_eq!((player.x, player.y, player.heading, player.velocity), flight, "no turn key is still held");
    assert!(!player.thrusting && !player.braking);
    assert!(run(&mut app).shots().len() <= shots, "no fire key is still held");
}

#[test]
fn closing_the_game_with_ctrl_w_sends_nothing_to_the_next_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let mut device = open_raw_tab(&mut app, "core");
    open_invade(&mut app);
    play(&mut app);
    app.key_down(Key::Space);
    app.key_down(Key::D);
    frames(&mut app, 3);
    app.key_press_modifiers(app_keys(false), Key::W);
    frames(&mut app, 3);
    assert_eq!(app.state().active_title().as_deref(), Some("core"));
    for _ in 0..3 {
        app.event(egui::Event::Text(" ".into()));
        app.event(egui::Event::Text("d".into()));
        frames(&mut app, 1);
    }
    assert_device_received_nothing(&mut device);
    app.key_up(Key::Space);
    app.key_up(Key::D);
    frames(&mut app, 2);
    assert_eq!(records(&dir).len(), 1);
}

#[test]
fn restarting_to_update_warns_that_an_unfinished_run_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    let ctx = app.ctx.clone();
    app.state_mut().request_update_install(&ctx);
    frames(&mut app, 2);
    let texts = app.state().dialog_texts();
    assert!(texts.iter().any(|t| t.contains("Invade run ends")), "{texts:?}");
    assert!(status(&app).paused, "the confirmation pauses the game");
}

#[test]
fn background_connection_actions_follow_the_session_not_its_position() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let first_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let second_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut first = open_raw_tab_on(&mut app, "a", &first_listener);
    let mut second = open_raw_tab_on(&mut app, "b", &second_listener);
    open_invade(&mut app);
    app.state_mut().move_tab("Invade", 1);
    app.state_mut().move_tab("b", 0);
    frames(&mut app, 2);
    assert_eq!(app.state().workspace_titles(), ["b", "a", "Invade"]);
    play(&mut app);
    app.get_by_label("b").click_secondary();
    frames(&mut app, 2);
    app.get_by_label("Reconnect").click();
    frames(&mut app, 2);
    let (mut replacement, _) = second_listener.accept().unwrap();
    replacement.write_all(b"b again>").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.state().active_screen_text().is_none_or(|t| !t.contains("b again>")) {
        assert!(Instant::now() < deadline);
        frames(&mut app, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.state().active_title().as_deref(), Some("b"));
    second.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    assert_eq!(second.read(&mut [0; 8]).unwrap_or(0), 0, "b's old connection hung up");
    assert_device_received_nothing(&mut first);
    assert_device_received_nothing(&mut replacement);
}

#[test]
fn progress_that_could_not_be_saved_survives_closing_and_reopening_the_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    // Make invade.json impossible to write: a folder stands in its place.
    let progress = dir.path().join("invade.json");
    let _ = std::fs::remove_file(&progress);
    std::fs::create_dir(&progress).unwrap();
    run(&mut app).skip_to_wave(6);
    frames(&mut app, 2);
    run(&mut app).defeat_all();
    frames(&mut app, 4);
    assert!(run(&mut app).is_unlocked(snekkie::invade::Unlock::Ghost));
    app.key_press_modifiers(app_keys(false), Key::W);
    frames(&mut app, 3);
    let texts = app.state().dialog_texts();
    assert!(texts.iter().any(|t| t.contains("Could not save Invade progress")), "{texts:?}");
    app.get_by_label("OK").click();
    frames(&mut app, 2);
    // The folder goes away; reopening keeps the unsaved unlock and saves it.
    std::fs::remove_dir(&progress).unwrap();
    open_invade(&mut app);
    assert!(app.state_mut().invade_tab().unwrap().progress().is_unlocked(snekkie::invade::Unlock::Ghost));
    app.state_mut().shutdown();
    assert!(std::fs::read_to_string(&progress).unwrap().contains("\"ghost\""));
}

#[test]
fn a_failed_game_over_save_is_retried_at_shutdown_without_duplicating_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    let progress = dir.path().join("invade.json");
    let _ = std::fs::remove_file(&progress);
    std::fs::create_dir(&progress).unwrap();
    run(&mut app).lose();
    frames(&mut app, 3);
    assert_eq!(status(&app).state, "Game over");
    assert_eq!(app.state_mut().invade_tab().unwrap().progress().records.len(), 1);
    assert!(app.state_mut().invade_tab().unwrap().save_warning().is_some());
    std::fs::remove_dir(&progress).unwrap();
    app.state_mut().shutdown();
    app.state_mut().shutdown();
    let saved = records(&dir);
    assert_eq!(saved.len(), 1, "shutdown retries the save rather than recording the run again");
    assert_eq!(saved[0]["ending"], "defeated");
    assert!(app.state_mut().invade_tab().unwrap().save_warning().is_none());
}

#[test]
fn a_thrust_key_let_go_with_ctrl_or_alt_down_stops_thrust_and_coasts() {
    for modifiers in [Modifiers::CTRL, Modifiers::ALT] {
        let dir = tempfile::tempdir().unwrap();
        let mut app = harness(&dir);
        open_invade(&mut app);
        play(&mut app);
        let r = run(&mut app);
        r.clear_asteroids();
        r.set_flight(std::f32::consts::FRAC_PI_2, (0.0, 0.0));
        app.key_down(Key::W);
        frames(&mut app, 6);
        assert!(run(&mut app).player().thrusting);
        app.event(egui::Event::Key { key: Key::W, physical_key: None, pressed: false, repeat: false, modifiers });
        frames(&mut app, 2);
        assert!(app.state_mut().invade_tab().unwrap().held_keys().is_empty(), "{modifiers:?}");
        let player = run(&mut app).player();
        assert!(!player.thrusting && !player.braking, "{modifiers:?}: physical release stopped thrust");
        let (x, speed, heading) = (player.x, player.speed(), player.heading);
        assert!(speed > 0.0, "releasing thrust preserves momentum");
        frames(&mut app, 20);
        let player = run(&mut app).player();
        assert!(!player.thrusting && !player.braking, "{modifiers:?}: thrust stayed released");
        assert_eq!(player.heading, heading);
        assert!(player.x > x, "{modifiers:?}: the ship coasts after releasing thrust");
        assert!(player.speed() > 0.0 && player.speed() < speed, "{modifiers:?}: coasting decelerates");
    }
}

#[test]
fn shift_inside_a_ctrl_shortcut_is_not_a_movement_ability() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    open_invade(&mut app);
    play(&mut app);
    app.event(egui::Event::ModifiersChanged(Modifiers::CTRL));
    frames(&mut app, 2);
    app.event(egui::Event::ModifiersChanged(Modifiers { shift: true, ..Modifiers::CTRL }));
    frames(&mut app, 3);
    assert_eq!(run(&mut app).player().mobility_cooldown, 0.0, "Ctrl+Shift fired the movement ability");
    // Letting go of Ctrl with Shift still down needs a fresh Shift press.
    app.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
    frames(&mut app, 3);
    assert_eq!(run(&mut app).player().mobility_cooldown, 0.0);
    app.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    frames(&mut app, 2);
    app.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
    frames(&mut app, 2);
    assert!(run(&mut app).player().mobility_cooldown > 0.0, "a plain Shift press still works");
    app.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    frames(&mut app, 2);
}

#[test]
fn pause_and_upgrade_screens_take_the_keyboard_back_after_menus_and_tab_switches() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let _device = open_raw_tab(&mut app, "core");
    open_invade(&mut app);
    play(&mut app);
    // A menu pauses the game; once it closes, P resumes.
    app.get_by_label("View").click();
    frames(&mut app, 2);
    app.key_press(Key::Escape);
    frames(&mut app, 3);
    assert!(status(&app).paused);
    press(&mut app, Key::P);
    assert_eq!(status(&app).state, "Get ready", "P resumes after a menu");
    frames(&mut app, 40);
    // Paused, away to the terminal and back: P still resumes.
    press(&mut app, Key::P);
    assert!(status(&app).paused);
    next_tab(&mut app, false);
    next_tab(&mut app, false);
    frames(&mut app, 2);
    press(&mut app, Key::P);
    assert_eq!(status(&app).state, "Get ready", "P resumes after returning to the tab");
    frames(&mut app, 40);
    // On the upgrade screen a menu comes and goes; 2 still chooses.
    run(&mut app).skip_to_wave(3);
    frames(&mut app, 2);
    level_up(&mut app);
    frames(&mut app, 3);
    assert_eq!(status(&app).state, "Choosing an upgrade");
    app.get_by_label("View").click();
    frames(&mut app, 2);
    app.key_press(Key::Escape);
    frames(&mut app, 3);
    press(&mut app, Key::Num2);
    assert_ne!(status(&app).state, "Choosing an upgrade", "2 chooses after a menu");
}

#[test]
fn holding_ctrl_shift_cycles_backward_through_the_game_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = harness(&dir);
    let _a = open_raw_tab(&mut app, "s1");
    let _b = open_raw_tab(&mut app, "s2");
    open_invade(&mut app);
    let _c = open_raw_tab(&mut app, "s3");
    assert_eq!(app.state().workspace_titles(), ["s1", "s2", "Invade", "s3"]);
    assert_eq!(app.state().active_title().as_deref(), Some("s3"));
    let chord = Modifiers { ctrl: true, shift: true, ..Modifiers::NONE };
    app.event(egui::Event::ModifiersChanged(chord));
    frames(&mut app, 1);
    for expected in ["Invade", "s2", "s1"] {
        app.key_press_modifiers(chord, Key::Tab);
        frames(&mut app, 2);
        assert_eq!(app.state().active_title().as_deref(), Some(expected));
    }
    app.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    frames(&mut app, 2);
}
