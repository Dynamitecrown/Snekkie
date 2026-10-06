//! Render snippet dialogs using temporary stores and a fictional local device.
//! Run `cargo run --example snippets_qa`; output is in target/snippets-qa/.
use std::io::Write;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};
use snekkie::{
    profiles::Profile,
    settings::{AppSettings, SettingsStore},
    snippets::{Snippet, SnippetStore},
    ui::{Paths, SnekkieApp},
};

fn main() {
    std::fs::create_dir_all("target/snippets-qa").unwrap();
    for (theme, file) in
        [("Snekkie Dark", "dark"), ("Paper Light", "light"), ("CRT Super", "crt"), ("E-Ink Super", "e-ink")]
    {
        for (size, suffix) in [([1000.0, 640.0], "normal"), ([520.0, 320.0], "small")] {
            let dir = tempfile::tempdir().unwrap();
            SettingsStore::new(dir.path().join("settings.json"))
                .save(&AppSettings { theme: theme.into(), check_for_updates: false, ..Default::default() })
                .unwrap();
            let mut store = SnippetStore::open(dir.path().join("snippets.json"));
            for (name, group, template) in [
                ("Interface checks", "Cisco IOS", "show interface {{interface}}\nshow vlan id {{vlan}}"),
                ("System inventory", "Cisco IOS", "show version\nshow inventory"),
                ("Chassis hardware", "Juniper", "show chassis hardware"),
            ] {
                store
                    .put(Snippet {
                        name: name.into(),
                        group: group.into(),
                        template: template.into(),
                        ..Default::default()
                    })
                    .unwrap();
            }
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
                    ..Default::default()
                },
            );
            let (mut peer, _) = listener.accept().unwrap();
            peer.write_all(b"LabSwitch#show version\r\nFictional lab switch, version 1.0\r\nLabSwitch#").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while app.state().active_screen_text().is_none_or(|s| !s.trim_end().ends_with("LabSwitch#")) {
                assert!(Instant::now() < deadline);
                app.run_ok();
                std::thread::sleep(Duration::from_millis(10));
            }
            app.key_press_modifiers(egui::Modifiers { ctrl: true, shift: true, ..Default::default() }, egui::Key::S);
            app.run_ok();
            app.render().unwrap().save(format!("target/snippets-qa/library-{file}-{suffix}.png")).unwrap();
            app.get_by_label("Interface checks").click();
            app.run_ok();
            app.get_by_label("Edit snippet").click();
            app.run_ok();
            app.render().unwrap().save(format!("target/snippets-qa/editor-{file}-{suffix}.png")).unwrap();
            app.get_by_label("Cancel").click();
            app.run_ok();
            app.get_by_label("Use snippet").click();
            app.run_ok();
            for (label, value) in [("Variable: interface", "Gi1/0/24"), ("Variable: vlan", "20")] {
                app.get_by_role_and_label(Role::TextInput, label).scroll_to_me();
                app.run_ok();
                app.get_by_role_and_label(Role::TextInput, label).focus();
                app.run_ok();
                app.event(egui::Event::Text(value.into()));
                app.run_ok();
            }
            app.render().unwrap().save(format!("target/snippets-qa/review-{file}-{suffix}.png")).unwrap();
            app.get_by_label("Insert into draft").click();
            app.run_ok();
            app.render().unwrap().save(format!("target/snippets-qa/draft-{file}-{suffix}.png")).unwrap();
            app.get_by_label("Cancel").click();
            app.run_ok();
            println!("Rendered {theme} at {size:?}");
        }
    }
}
