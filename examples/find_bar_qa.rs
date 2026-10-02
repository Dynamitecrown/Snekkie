//! Render terminal search in several themes and a small window.
//! Run with `cargo run --example find_bar_qa`; screenshots go to
//! `target/terminal-search-qa/`. Uses temporary settings and a local TCP peer.
use std::io::Write;
use std::time::{Duration, Instant};

use egui_kittest::{Harness, kittest::Queryable};
use snekkie::{
    profiles::Profile,
    settings::{AppSettings, SettingsStore},
    ui::{Paths, SnekkieApp},
};

fn main() {
    std::fs::create_dir_all("target/terminal-search-qa").unwrap();
    for (theme, file) in [
        ("Snekkie Dark", "dark"),
        ("Paper Light", "paper-light"),
        ("Monochrome Green", "monochrome"),
        ("CRT Super", "crt-super"),
        ("E-Ink Super", "e-ink-super"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        SettingsStore::new(dir.path().join("settings.json"))
            .save(&AppSettings { check_for_updates: false, theme: theme.into(), ..Default::default() })
            .unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = Harness::builder()
            .with_size([1000.0, 640.0])
            .build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
        app.run_ok();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ctx = app.ctx.clone();
        app.state_mut().open_session(
            &ctx,
            Profile {
                name: "Core switch".into(),
                kind: "raw".into(),
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                device_syntax: "cisco_ios".into(),
                ..Default::default()
            },
        );
        let (mut peer, _) = listener.accept().unwrap();
        let mut text = String::from("Switch#show running-config\r\n");
        for i in 1..=40 {
            text.push_str(&format!(
                "interface GigabitEthernet1/0/{i}\r\n description Access port {i}\r\n switchport access vlan {}\r\n!\r\n",
                10 + i % 3 * 10
            ));
        }
        text.push_str("Switch#");
        peer.write_all(text.as_bytes()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.state().active_screen_text().is_none_or(|s| !s.trim_end().ends_with("Switch#")) {
            assert!(Instant::now() < deadline);
            app.run_ok();
            std::thread::sleep(Duration::from_millis(10));
        }
        app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
        app.run_ok();
        app.run_ok();
        app.event(egui::Event::Text("vlan 20".into()));
        for _ in 0..10 {
            app.run_ok();
        }
        app.key_press(egui::Key::Enter);
        for _ in 0..10 {
            app.run_ok();
        }
        app.render().unwrap().save(format!("target/terminal-search-qa/find-{file}.png")).unwrap();
        println!("{theme}: {:?}", app.state().active_search_status());
        if file == "dark" {
            app.get_by_label("Regular expression").click();
            app.run_ok();
            app.get_by_label("Find in terminal").focus();
            app.run_ok();
            app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
            app.event(egui::Event::Text("(vlan".into()));
            for _ in 0..5 {
                app.run_ok();
            }
            app.render().unwrap().save("target/terminal-search-qa/find-invalid-regex.png").unwrap();
        }
    }
    for (theme, file) in [("Snekkie Dark", "dark"), ("CRT Super", "crt-super"), ("E-Ink Super", "e-ink-super")] {
        let dir = tempfile::tempdir().unwrap();
        SettingsStore::new(dir.path().join("settings.json"))
            .save(&AppSettings { check_for_updates: false, theme: theme.into(), ..Default::default() })
            .unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut small = Harness::builder()
            .with_size([520.0, 320.0])
            .build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
        small.run_ok();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ctx = small.ctx.clone();
        small.state_mut().open_session(
            &ctx,
            Profile {
                name: "r1".into(),
                kind: "raw".into(),
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                ..Default::default()
            },
        );
        let (mut peer, _) = listener.accept().unwrap();
        peer.write_all(b"R1#show ip interface brief\r\nGi0/0 192.0.2.1 up up\r\nR1#").unwrap();
        for _ in 0..50 {
            small.run_ok();
            std::thread::sleep(Duration::from_millis(10));
        }
        small.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::F);
        small.run_ok();
        small.run_ok();
        small.event(egui::Event::Text("192.0.2.1".into()));
        for _ in 0..10 {
            small.run_ok();
        }
        small.render().unwrap().save(format!("target/terminal-search-qa/find-small-sidebar-{file}.png")).unwrap();
        small.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::B);
        small.run_ok();
        small.run_ok();
        small.render().unwrap().save(format!("target/terminal-search-qa/find-small-window-{file}.png")).unwrap();
    }
    println!("Rendered find bar QA.");
}
