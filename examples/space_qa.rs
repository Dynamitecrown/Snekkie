//! Render the newer super themes and palettes in the real app, talking to a
//! local fictional device. `cargo run --locked --example space_qa` writes
//! target/space-qa/.
use std::io::Write;

use egui::{Event, Modifiers, PointerButton, Pos2, accesskit::Role, pos2};
use egui_kittest::{Harness, kittest::Queryable};
use snekkie::{
    profiles::Profile,
    settings::{
        Animations, AppAppearance, AppSettings, KeystrokeBurst, NewLines, NewText, SettingsStore, ThemeEffects,
        TypedText,
    },
    ui::{Paths, SnekkieApp},
};

const OUTPUT: &[u8] = b"\r\nedge-sw1# show ip interface brief\r\n\
Interface              IP-Address      OK? Method Status                Protocol\r\n\
Vlan1                  192.0.2.1       YES NVRAM  up                    up\r\n\
GigabitEthernet1/0/1   198.51.100.1    YES manual up                    up\r\n\
GigabitEthernet1/0/2   unassigned      YES unset  administratively down down\r\n\
GigabitEthernet1/0/3   203.0.113.9     YES manual up                    up\r\n\
TenGigabitEthernet1/1/1 unassigned     YES unset  down                  down\r\n\
edge-sw1# show running-config | section line vty\r\n\
line vty 0 4\r\n transport input ssh\r\n access-class 10 in\r\n login local\r\n\
edge-sw1# ";

type App = Harness<'static, SnekkieApp>;

fn app(dir: &tempfile::TempDir, theme: &str, animations: Animations) -> App {
    let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
    let settings = AppSettings { theme: theme.into(), check_for_updates: false, animations, ..Default::default() };
    SettingsStore::new(paths.settings.clone()).save(&settings).unwrap();
    let mut app = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_step_dt(1.0 / 30.0)
        .build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
    app.run_ok();
    app
}

fn connect(app: &mut App) -> std::net::TcpStream {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ctx = app.ctx.clone();
    app.state_mut().open_session(
        &ctx,
        Profile {
            name: "edge-sw1".into(),
            kind: "raw".into(),
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            device_syntax: "cisco_ios".into(),
            ..Default::default()
        },
    );
    let (device, _) = listener.accept().unwrap();
    app.run_ok();
    device
}

fn feed(app: &mut App, device: &mut std::net::TcpStream) {
    device.write_all(OUTPUT).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !app.state().active_screen_text().is_some_and(|screen| screen.contains("login local")) {
        assert!(std::time::Instant::now() < deadline, "output never arrived");
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.step();
    }
}

fn capture(app: &mut App, name: &str) {
    app.render().unwrap().save(format!("target/space-qa/{name}.png")).unwrap();
}

fn pointer(app: &mut App, at: Pos2) {
    app.event(Event::PointerMoved(at));
    app.step();
}

fn button(app: &mut App, at: Pos2, pressed: bool, modifiers: Modifiers) {
    app.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers });
    app.step();
}

fn main() {
    std::fs::create_dir_all("target/space-qa").unwrap();
    let on = Animations { enabled: true, ..Animations::default() };
    let off = Animations::default();

    // Still, with animations off: empty, in a session, with text selected.
    let dir = tempfile::tempdir().unwrap();
    let mut space = app(&dir, "Space Super", off);
    capture(&mut space, "space-empty");
    let mut device = connect(&mut space);
    feed(&mut space, &mut device);
    space.run_ok();
    capture(&mut space, "space-still");
    pointer(&mut space, pos2(420.0, 130.0));
    button(&mut space, pos2(420.0, 130.0), true, Modifiers::NONE);
    pointer(&mut space, pos2(900.0, 200.0));
    pointer(&mut space, pos2(1000.0, 200.0));
    button(&mut space, pos2(1000.0, 200.0), false, Modifiers::NONE);
    space.run_ok();
    capture(&mut space, "space-selection");
    drop(space);

    // Live: the wave off new output, then a planet dragged across the window.
    let dir = tempfile::tempdir().unwrap();
    let mut space = app(&dir, "Space Super", on);
    let mut device = connect(&mut space);
    pointer(&mut space, pos2(640.0, 400.0));
    for _ in 0..10 {
        space.step();
    }
    capture(&mut space, "space-live-before");
    feed(&mut space, &mut device);
    for frame in 1..=12 {
        space.step();
        if frame % 4 == 0 {
            capture(&mut space, &format!("space-live-wave-{frame}"));
        }
    }
    let grip = Modifiers { ctrl: true, shift: true, ..Modifiers::NONE };
    let giant = pos2(1050.0, 208.0);
    pointer(&mut space, giant);
    space.event(Event::ModifiersChanged(grip));
    button(&mut space, giant, true, grip);
    for x in [900.0, 750.0, 600.0, 500.0] {
        pointer(&mut space, pos2(x, 480.0));
    }
    button(&mut space, pos2(500.0, 480.0), false, grip);
    space.event(Event::ModifiersChanged(Modifiers::NONE));
    pointer(&mut space, pos2(30.0, 30.0));
    for _ in 0..120 {
        space.step();
    }
    capture(&mut space, "space-planet-moved");
    drop(space);

    // Preferences' sample has no app sky behind it, so it draws a still one.
    let dir = tempfile::tempdir().unwrap();
    let mut prefs = app(&dir, "Space Super", off);
    let modifiers = Modifiers {
        ctrl: !cfg!(target_os = "macos"),
        mac_cmd: cfg!(target_os = "macos"),
        command: true,
        ..Modifiers::NONE
    };
    prefs.key_press_modifiers(modifiers, egui::Key::Comma);
    prefs.run_ok();
    capture(&mut prefs, "space-preferences");
    drop(prefs);

    // The newer styles in the Animations page's live preview.
    let dir = tempfile::tempdir().unwrap();
    let styles = Animations {
        keystroke_burst: KeystrokeBurst::Comet,
        typed_text: TypedText::Jelly,
        new_text: NewText::Glitch,
        new_lines: NewLines::Wipe,
        ..on
    };
    let mut prefs = app(&dir, "Neon Super", styles);
    prefs.key_press_modifiers(modifiers, egui::Key::Comma);
    for _ in 0..30 {
        prefs.step();
    }
    prefs.get_all_by_role_and_label(Role::Button, "Animations").next_back().unwrap().click();
    for frame in 0..240 {
        prefs.step();
        if frame % 60 == 59 {
            capture(&mut prefs, &format!("animations-{frame}"));
        }
    }
    drop(prefs);

    // Space appearance on someone's light palette.
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
    let mut light = AppSettings { theme: "Custom".into(), check_for_updates: false, ..Default::default() };
    let mut theme = light.theme_named("Paper Light");
    theme.effects = ThemeEffects { appearance: AppAppearance::Space, ..ThemeEffects::default() };
    light.set_custom_theme(theme);
    SettingsStore::new(paths.settings.clone()).save(&light).unwrap();
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
    harness.run_ok();
    let mut device = connect(&mut harness);
    feed(&mut harness, &mut device);
    harness.run_ok();
    capture(&mut harness, "space-on-light-palette");
    drop(harness);

    for theme in
        ["Neon Super", "Sonar Super", "Aurora", "Rose Dusk", "Graphite", "Ember", "Sandstone Light", "Harbor Light"]
    {
        let dir = tempfile::tempdir().unwrap();
        let mut harness = app(&dir, theme, off);
        let mut device = connect(&mut harness);
        feed(&mut harness, &mut device);
        harness.run_ok();
        capture(&mut harness, theme);
    }
}
