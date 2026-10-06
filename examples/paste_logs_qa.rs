//! Render paste and logging controls with temporary stores and a fictional device.
//! `cargo run --example paste_logs_qa` writes target/paste-logs-qa/.
use std::io::Write;
use std::time::{Duration, Instant};

use egui_kittest::{Harness, kittest::Queryable};
use snekkie::{
    profiles::Profile,
    settings::{AppSettings, SettingsStore},
    ui::{Paths, SnekkieApp},
};

fn main() {
    std::fs::create_dir_all("target/paste-logs-qa").unwrap();
    for (theme, file) in
        [("Snekkie Dark", "dark"), ("Paper Light", "light"), ("CRT Super", "crt"), ("E-Ink Super", "e-ink")]
    {
        for (size, suffix) in [([1000.0, 640.0], "normal"), ([520.0, 320.0], "small")] {
            let dir = tempfile::tempdir().unwrap();
            SettingsStore::new(dir.path().join("settings.json"))
                .save(&AppSettings {
                    theme: theme.into(),
                    paste_delay_ms: 5000,
                    check_for_updates: false,
                    ..Default::default()
                })
                .unwrap();
            let paths =
                Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
            let mut app =
                Harness::builder().with_size(size).build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
            app.run_ok();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let ctx = app.ctx.clone();
            app.state_mut().open_session(
                &ctx,
                Profile {
                    name: "Lab switch".into(),
                    kind: "raw".into(),
                    host: "127.0.0.1".into(),
                    port: listener.local_addr().unwrap().port(),
                    log_path: dir.path().join("lab.log").display().to_string(),
                    ..Default::default()
                },
            );
            let (mut peer, _) = listener.accept().unwrap();
            peer.write_all(b"LabSwitch#show interface status\r\nGi1/0/24 connected VLAN 20\r\nLabSwitch#").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while app.state().active_screen_text().is_none_or(|s| !s.trim_end().ends_with("LabSwitch#")) {
                assert!(Instant::now() < deadline);
                app.run_ok();
                std::thread::sleep(Duration::from_millis(10));
            }
            app.state_mut().paste_clipboard_text(&ctx, "configure terminal\ninterface GigabitEthernet1/0/24\n description Fictional lab uplink\n no shutdown\nend\n");
            app.run_ok();
            app.render().unwrap().save(format!("target/paste-logs-qa/review-{file}-{suffix}.png")).unwrap();
            app.get_by_label("Send paste").click();
            app.run_ok();
            app.render().unwrap().save(format!("target/paste-logs-qa/progress-{file}-{suffix}.png")).unwrap();
            app.get_by_label("Stop paste").click();
            app.run_ok();
            app.get_by_label("Advanced").click();
            app.run_ok();
            app.get_by_label("Logging options").scroll_to_me();
            app.run_ok();
            app.get_by_label("Logging options").click();
            app.run_ok();
            app.get_by_label("Logging options").hover();
            app.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -180.0),
                modifiers: Default::default(),
                phase: egui::TouchPhase::Move,
            });
            app.run_ok();
            app.get_by_label("Log passwords").scroll_to_me();
            app.run_ok();
            app.render().unwrap().save(format!("target/paste-logs-qa/logging-{file}-{suffix}.png")).unwrap();
            app.state_mut().shutdown();
            println!("Rendered {theme} at {size:?}");
        }
    }
}
