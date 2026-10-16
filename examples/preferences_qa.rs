//! Render every Preferences section using temporary settings.
//! `cargo run --locked --example preferences_qa` writes target/preferences-qa/.
use egui::{Key, Modifiers, accesskit::Role};
use egui_kittest::{Harness, kittest::Queryable};
use snekkie::{
    settings::{AppSettings, SettingsStore},
    ui::{Paths, SnekkieApp},
};

fn page(app: &mut Harness<'static, SnekkieApp>, name: &str) {
    app.get_all_by_role_and_label(Role::Button, name).next_back().unwrap().click();
    app.run_ok();
}

fn capture(app: &mut Harness<'static, SnekkieApp>, name: &str) {
    app.render().unwrap().save(format!("target/preferences-qa/{name}.png")).unwrap();
}

fn main() {
    std::fs::create_dir_all("target/preferences-qa").unwrap();
    for (theme, name) in [
        ("Snekkie Dark", "dark"),
        ("Paper Light", "light"),
        ("CRT Super", "crt"),
        ("E-Ink Super", "e-ink"),
        ("Frost Light", "frost"),
        ("Copper Night", "copper"),
        ("Forest Night", "forest"),
    ] {
        for (size, viewport) in [([1000.0, 640.0], "normal"), ([520.0, 320.0], "small")] {
            let dir = tempfile::tempdir().unwrap();
            let paths =
                Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
            let settings = AppSettings { theme: theme.into(), check_for_updates: false, ..Default::default() };
            SettingsStore::new(paths.settings.clone()).save(&settings).unwrap();
            let mut app =
                Harness::builder().with_size(size).build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
            app.run_ok();
            let modifiers = Modifiers {
                ctrl: !cfg!(target_os = "macos"),
                mac_cmd: cfg!(target_os = "macos"),
                command: true,
                ..Default::default()
            };
            app.key_press_modifiers(modifiers, Key::Comma);
            app.run_ok();
            for section in ["Appearance", "Terminal", "Input & paste", "Network", "Highlighting", "Animations"] {
                page(&mut app, section);
                capture(&mut app, &format!("{name}-{viewport}-{section}"));
                if section == "Appearance" {
                    app.get_by_label("App appearance").scroll_to_me();
                    app.run_ok();
                    capture(&mut app, &format!("{name}-{viewport}-theme-colors"));
                } else if section == "Network" {
                    app.get_by_label("Offline mode").click();
                    app.run_ok();
                    capture(&mut app, &format!("{name}-{viewport}-offline-warning"));
                } else if section == "Animations" {
                    app.get_by_role_and_label(Role::CheckBox, "Animations").click();
                    app.run_ok();
                    capture(&mut app, &format!("{name}-{viewport}-animations-enabled"));
                    app.get_by_label("New lines").scroll_to_me();
                    app.run_ok();
                    capture(&mut app, &format!("{name}-{viewport}-animations-output"));
                }
            }
            page(&mut app, "Terminal");
            app.get_by_label("Font size").click();
            app.run_ok();
            app.key_press_modifiers(Modifiers::COMMAND, Key::A);
            app.event(egui::Event::Text("32".into()));
            app.key_press(Key::Enter);
            app.run_ok();
            app.get_by_label("Preview — 32 pt").scroll_to_me();
            app.run_ok();
            capture(&mut app, &format!("{name}-{viewport}-large-font"));
            app.get_by_label("Cancel").click();
            app.run_ok();
            assert_eq!(SettingsStore::new(dir.path().join("settings.json")).load(), settings);
            println!("Rendered Preferences: {theme}, {size:?}");
        }
    }
}
