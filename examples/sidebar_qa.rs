//! Render the sidebar, saved-profile actions and tab states with fictional data.
//! Run `cargo run --locked --example sidebar_qa`; images go to target/sidebar-qa/.
use std::io::Write;
use std::time::{Duration, Instant};

use egui_kittest::{Harness, kittest::Queryable};
use snekkie::{
    profiles::{Kind, Profile, ProfileStore},
    settings::{AppSettings, SettingsStore},
    ui::{Paths, SnekkieApp},
};

fn main() {
    std::fs::create_dir_all("target/sidebar-qa").unwrap();
    for (theme, name) in
        [("Snekkie Dark", "dark"), ("Paper Light", "light"), ("CRT Super", "crt"), ("E-Ink Super", "e-ink")]
    {
        for (size, viewport) in [([1000.0, 640.0], "normal"), ([520.0, 320.0], "small")] {
            let dir = tempfile::tempdir().unwrap();
            let paths =
                Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
            SettingsStore::new(paths.settings.clone())
                .save(&AppSettings { theme: theme.into(), check_for_updates: false, ..Default::default() })
                .unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let mut store = ProfileStore::open(paths.sessions.clone());
            for title in ["Core switch", "Access switch", "Lab console"] {
                store
                    .put(Profile {
                        name: title.into(),
                        kind: "raw".into(),
                        host: "127.0.0.1".into(),
                        port: listener.local_addr().unwrap().port(),
                        favorite: title == "Core switch",
                        ..Default::default()
                    })
                    .unwrap();
            }
            let mut app =
                Harness::builder().with_size(size).build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
            app.state_mut().set_form_kind(Kind::Ssh);
            app.state_mut().sidebar_draft().host = "draft.example".into();
            app.run_ok();
            app.get_by_label("New session").click();
            app.run_ok();
            app.get_by_label("Core switch").scroll_to_me();
            app.run_ok();
            app.get_by_label("Core switch").click();
            app.run_ok();
            app.get_by_label("Connect selected").scroll_to_me();
            app.run_ok();
            app.get_by_label("Connect selected").click();
            app.run_ok();
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut peer = loop {
                match listener.accept() {
                    Ok((peer, _)) => break peer,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "local device did not connect");
                        app.run_ok();
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            peer.write_all(b"Core#show ip interface brief\r\nGi0/1 192.0.2.1 up up\r\nCore#").unwrap();
            while app.state().active_screen_text().is_none_or(|s| !s.trim_end().ends_with("Core#")) {
                assert!(Instant::now() < deadline);
                app.run_ok();
                std::thread::sleep(Duration::from_millis(10));
            }
            app.get_by_label("New session").scroll_to_me();
            app.run_ok();
            app.get_by_label("New session").click();
            app.run_ok();
            app.render().unwrap().save(format!("target/sidebar-qa/expanded-{name}-{viewport}.png")).unwrap();
            app.get_by_label("New session").click();
            app.run_ok();
            app.render().unwrap().save(format!("target/sidebar-qa/saved-{name}-{viewport}.png")).unwrap();
            app.get_by_label("Hide sessions").click();
            app.run_ok();
            app.render().unwrap().save(format!("target/sidebar-qa/hidden-{name}-{viewport}.png")).unwrap();
            drop(peer);
            while app.state().tab_titles() != ["Core switch (closed)"] {
                assert!(Instant::now() < deadline);
                app.run_ok();
                std::thread::sleep(Duration::from_millis(10));
            }
            app.run_ok();
            app.get_by_label("Core switch (closed)").hover();
            app.run_ok();
            app.render().unwrap().save(format!("target/sidebar-qa/closed-{name}-{viewport}.png")).unwrap();
            println!("Rendered {theme} at {size:?}");
        }
    }
}
