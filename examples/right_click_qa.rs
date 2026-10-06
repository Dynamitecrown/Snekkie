//! Render the right-click preference and terminal menu with temporary lab data.
//! Run `cargo run --example right_click_qa`; output is in target/right-click-qa/.
use std::io::Write;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};
use snekkie::{
    profiles::Profile,
    settings::{AppSettings, SettingsStore},
    ui::{Paths, SnekkieApp},
};

fn main() {
    std::fs::create_dir_all("target/right-click-qa").unwrap();
    for (theme, file) in [("Snekkie Dark", "dark"), ("CRT Super", "crt"), ("E-Ink Super", "e-ink")] {
        for (size, suffix) in [([1000.0, 640.0], "normal"), ([520.0, 320.0], "small")] {
            for (direct_paste, mode) in [(true, "paste"), (false, "menu")] {
                let dir = tempfile::tempdir().unwrap();
                SettingsStore::new(dir.path().join("settings.json"))
                    .save(&AppSettings {
                        theme: theme.into(),
                        right_click_paste: direct_paste,
                        check_for_updates: false,
                        ..Default::default()
                    })
                    .unwrap();
                let paths =
                    Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
                let mut app = Harness::builder()
                    .with_size(size)
                    .build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
                app.run_ok();
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let ctx = app.ctx.clone();
                app.state_mut().open_session(
                    &ctx,
                    Profile {
                        name: "Lab".into(),
                        kind: "raw".into(),
                        host: "127.0.0.1".into(),
                        port: listener.local_addr().unwrap().port(),
                        ..Default::default()
                    },
                );
                let (mut peer, _) = listener.accept().unwrap();
                peer.write_all(b"Lab#show version\r\nFictional lab router\r\nready").unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                while app.state().active_screen_text().is_none_or(|text| !text.trim_end().ends_with("ready")) {
                    assert!(Instant::now() < deadline);
                    app.run_ok();
                    std::thread::sleep(Duration::from_millis(10));
                }
                app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Comma);
                app.run_ok();
                app.get_by_role_and_label(Role::CheckBox, "Right-click pastes (PuTTY style)").scroll_to_me();
                app.run_ok();
                app.render()
                    .unwrap()
                    .save(format!("target/right-click-qa/preferences-{file}-{suffix}-{mode}.png"))
                    .unwrap();
                app.get_by_label("Cancel").click();
                app.run_ok();
                if direct_paste {
                    app.get_by_label("Terminal output")
                        .click_button_modifiers(egui::PointerButton::Secondary, egui::Modifiers::CTRL);
                } else {
                    app.get_by_label("Terminal output").click_secondary();
                }
                app.run_ok();
                app.get_by_label("Copy all");
                app.render()
                    .unwrap()
                    .save(format!("target/right-click-qa/context-{file}-{suffix}-{mode}.png"))
                    .unwrap();
                app.key_press(egui::Key::Escape);
                app.run_ok();
                println!("Rendered {theme} at {size:?}, {mode} mode");
            }
        }
    }
}
