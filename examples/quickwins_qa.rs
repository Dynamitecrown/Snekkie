//! Drive the 2026-11 quick-win features through the real app UI with fictional
//! lab devices on 127.0.0.1, printing a check per feature.
//! Run `cargo run --locked --example quickwins_qa`; images go to target/quickwins-qa/.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};
use russh::server::{self, Auth, Msg, Response, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use snekkie::{
    profiles::{Kind, Profile, ProfileStore},
    settings::{AppSettings, HighlightRule, LastTab, LastTabs, SettingsStore},
    snippets::{Snippet, SnippetStore},
    ui::{Paths, SnekkieApp},
};

const OUT: &str = "target/quickwins-qa";
type App<'a> = Harness<'a, SnekkieApp>;

fn check(name: &str, ok: bool, detail: impl std::fmt::Display) {
    println!("CHECK {:<34} {} {detail}", name, if ok { "ok  " } else { "FAIL" });
}

fn shot(app: &mut App, name: &str) {
    app.run_ok();
    app.render().unwrap().save(format!("{OUT}/{name}.png")).unwrap();
}

/// Run frames until `done` or the deadline; returns whether it happened.
fn wait(app: &mut App, secs: u64, mut done: impl FnMut(&mut App) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        app.run_ok();
        if done(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    false
}

fn screen(app: &App) -> String {
    app.state().active_screen_text().unwrap_or_default()
}

fn raw(name: &str, port: u16) -> Profile {
    Profile { name: name.into(), kind: "raw".into(), host: "127.0.0.1".into(), port, ..Default::default() }
}

/// Everything the app has sent so far.
fn drain(peer: &mut TcpStream) -> Vec<u8> {
    peer.set_nonblocking(true).unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = peer.read(&mut buf) {
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    peer.set_nonblocking(false).unwrap();
    out
}

fn click_with(app: &mut App, pos: egui::Pos2, modifiers: egui::Modifiers) {
    app.event(egui::Event::ModifiersChanged(modifiers));
    app.event(egui::Event::PointerMoved(pos));
    app.run_ok();
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers });
    }
    app.run_ok();
    app.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
}

fn main() {
    std::fs::create_dir_all(OUT).unwrap();
    let dir = tempfile::tempdir().unwrap();
    snekkie::transport::ssh::use_known_hosts_for_tests(dir.path().join("known_hosts"));
    let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
    let lab = TcpListener::bind("127.0.0.1:0").unwrap();
    let lab_port = lab.local_addr().unwrap().port();
    SettingsStore::new(paths.settings.clone())
        .save(&AppSettings {
            check_for_updates: false,
            remote_clipboard: true,
            remote_titles: true,
            button_bar: true,
            highlight_rules: vec![
                HighlightRule { text: "VLAN".into(), color: "#ff5f5f".into(), bold: true, ..Default::default() },
                HighlightRule { text: r"err-\w+".into(), regex: true, color: "#ffaf00".into(), ..Default::default() },
            ],
            ..Default::default()
        })
        .unwrap();
    let mut store = ProfileStore::open(paths.sessions.clone());
    store.put(raw("Lab", lab_port)).unwrap();
    store.put(raw("Lab B", lab_port)).unwrap();
    let mut snippets = SnippetStore::open(paths.sessions.with_file_name("snippets.json"));
    for (name, pinned) in [("Show version", true), ("Interfaces", true), ("Not pinned", false)] {
        snippets
            .put(Snippet { id: 0, name: name.into(), group: "Cisco".into(), template: "show version".into(), pinned })
            .unwrap();
    }

    let app_paths = copy(&paths);
    let mut app: App = Harness::builder()
        .with_size([1100.0, 700.0])
        .build_eframe(move |_| SnekkieApp::with_port_lister(app_paths, Vec::new));
    app.run_ok();
    let ctx = app.ctx.clone();

    // --- Device tab: highlight rules, button bar, CLI mode badge, links ---
    app.state_mut().open_session(&ctx, raw("Lab", lab_port));
    let (mut peer, _) = lab.accept().unwrap();
    peer.write_all(
        b"\x1b[2J\x1b[H10.0.0.5 10.0.0.5 10.0.0.5 10.0.0.5\r\n10.0.0.5 10.0.0.5 10.0.0.5 10.0.0.5\r\n\
https://example.com/a https://example.com/a\r\nhttps://example.com/a https://example.com/a\r\n\
https://example.com/a https://example.com/a\r\nVLAN 10 is up, Gi1/0/3 err-disabled\r\n\x1b[?2004hSW1(config-if)#",
    )
    .unwrap();
    let seen = wait(&mut app, 10, |a| screen(a).trim_end().ends_with("SW1(config-if)#"));
    check("device output arrives", seen, "");
    shot(&mut app, "01-device-highlights-buttonbar-mode");
    let mode = app.query_all_by_label_contains("config").next().is_some();
    check("config mode badge in UI", mode, "(also see 01 screenshot)");

    // Ctrl+click an IP (rows 0-1) and a URL (rows 2-4).
    let rect = app.get_by_label("Terminal output").rect();
    let row = (rect.height() / 24.0).clamp(12.0, 30.0);
    println!("terminal rect {rect:?}");
    click_with(&mut app, egui::pos2(345.0, 160.0), egui::Modifiers::COMMAND);
    let host = app.state_mut().sidebar_draft().host.clone();
    check("ctrl+click IP fills new session", host == "10.0.0.5", format!("host={host:?} row={row:.1}"));
    shot(&mut app, "02-ip-click-new-session");
    // Hover with Ctrl over the URL to show the underline, then click it.
    let url_pos = egui::pos2(360.0, 213.0);
    app.event(egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
    app.event(egui::Event::PointerMoved(url_pos));
    shot(&mut app, "03-url-hover-underline");
    let mut opened = false;
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton {
            pos: url_pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::COMMAND,
        });
    }
    for _ in 0..10 {
        app.step();
        opened |= app
            .output()
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::OpenUrl(u) if u.url == "https://example.com/a"));
    }
    app.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    check("ctrl+click URL opens browser", opened, "");
    drain(&mut peer);
    // Plain click on the terminal must not open anything.
    click_with(&mut app, url_pos, egui::Modifiers::NONE);

    // --- Bracketed paste (remote sent ESC[?2004h) ---
    app.get_by_label("Terminal output").click();
    app.run_ok();
    app.event(egui::Event::Paste("interface Gi1/0/3\nshutdown\x1b[201~".into()));
    app.run_ok();
    shot(&mut app, "04-paste-review");
    if let Some(send) = app.query_by_label("Send paste") {
        send.click();
    }
    let mut sent = Vec::new();
    wait(&mut app, 5, |_| {
        sent.extend(drain(&mut peer));
        sent.ends_with(b"\x1b[201~")
    });
    let text = String::from_utf8_lossy(&sent).to_string();
    let good = text.starts_with("\x1b[200~") && text.ends_with("\x1b[201~") && text.matches("\x1b[201~").count() == 1;
    check("bracketed paste wraps, strips marker", good, format!("{text:?}"));

    // --- Button bar ---
    app.get_by_label("Show version").click();
    app.run_ok();
    shot(&mut app, "05-button-bar-review");
    std::thread::sleep(Duration::from_millis(200));
    let leaked = drain(&mut peer);
    check("button bar sends nothing by itself", leaked.is_empty(), format!("{:?}", String::from_utf8_lossy(&leaked)));
    app.key_press(egui::Key::Escape);
    app.run_ok();
    if let Some(close) = app.query_by_label("Close") {
        close.click();
        app.run_ok();
    }

    // --- Ad-hoc tab: prompt names it; first tab goes to background ---
    let adhoc = TcpListener::bind("127.0.0.1:0").unwrap();
    let adhoc_port = adhoc.local_addr().unwrap().port();
    app.state_mut().open_session(&ctx, raw("", adhoc_port));
    let (mut peer2, _) = adhoc.accept().unwrap();
    peer2.write_all(b"Welcome\r\nedge-rtr1>").unwrap();
    let named = wait(&mut app, 10, |a| a.query_all_by_label_contains("edge-rtr1").next().is_some());
    check("ad-hoc tab named from prompt", named, format!("{:?}", app.state().tab_titles()));

    // Background Lab tab gets a remote title, a bell and an OSC 52 copy.
    peer.write_all(b"\x1b]2;core-sw1 (remote title)\x07\x07\x1b]52;c;aGVsbG8gZnJvbSBsYWI=\x07").unwrap();
    let mut clip = false;
    for _ in 0..200 {
        app.step();
        clip |= app
            .output()
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == "hello from lab"));
        if clip {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let titled = wait(&mut app, 5, |a| a.query_all_by_label_contains("core-sw1").next().is_some());
    check("remote title shown on tab", titled, "");
    let copied = clip;
    check("OSC 52 copies (setting on)", copied, "");
    let bell = app.query_all_by_label_contains("🔔").next().is_some();
    check("bell mark on background tab", bell, "");
    shot(&mut app, "06-tabs-title-bell-prompt");

    // --- Check port from the sidebar ---
    app.get_by_label("Lab B").click_secondary();
    app.run_ok();
    let menu = app.query_by_label("Check port").is_some();
    check("sidebar menu has Check port", menu, "");
    if menu {
        app.get_by_label("Check port").click();
        let shown = wait(&mut app, 6, |a| a.query_all_by_label_contains("open").next().is_some());
        check("port check result shown", shown, "");
        let _ = lab.accept(); // the check's own connection
        shot(&mut app, "07-check-port");
    }

    // --- Reconnect when back ---
    let boot = TcpListener::bind("127.0.0.1:0").unwrap();
    let boot_port = boot.local_addr().unwrap().port();
    app.state_mut().open_session(&ctx, raw("Rebooting switch", boot_port));
    let (peer3, _) = boot.accept().unwrap();
    wait(&mut app, 2, |_| false);
    drop(peer3);
    drop(boot);
    let closed = wait(&mut app, 10, |a| {
        a.query_all_by_label_contains("closed").next().is_some() || a.query_by_label("Reconnect").is_some()
    });
    check("rebooting tab shows disconnected", closed, "");
    let tab = app.query_all_by_label_contains("Rebooting switch").last().map(|n| n.rect());
    if let Some(node) = app.query_all_by_label_contains("Rebooting switch").last() {
        node.click_secondary();
    }
    app.run_ok();
    let item = app.query_by_label("Reconnect when back").is_some();
    check("tab menu has Reconnect when back", item && tab.is_some(), "");
    if item {
        app.get_by_label("Reconnect when back").click();
        let waiting = wait(&mut app, 3, |a| a.query_all_by_label_contains("Waiting for host").next().is_some());
        check("waiting state shown", waiting, "");
        shot(&mut app, "08-waiting-for-host");
        let back = TcpListener::bind(("127.0.0.1", boot_port)).unwrap();
        back.set_nonblocking(true).unwrap();
        let mut peer4 = None;
        let reconnected = wait(&mut app, 25, |_| {
            if peer4.is_none()
                && let Ok((s, _)) = back.accept()
            {
                s.set_nonblocking(false).unwrap();
                peer4 = Some(s);
            }
            peer4.is_some()
        });
        check("reconnects once host is back", reconnected, "");
        if let Some(mut p) = peer4 {
            p.write_all(b"\r\nSwitch is back\r\nboot-sw#").unwrap();
            wait(&mut app, 5, |a| screen(a).contains("boot-sw#"));
            shot(&mut app, "09-reconnected");
        }
    }

    // --- Preferences pages ---
    app.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Comma);
    app.run_ok();
    for (page, label, file) in [
        ("Input & paste", "Use title set by remote host for tabs", "10-prefs-remote-host"),
        ("Highlighting", "Highlight rules", "11-prefs-highlight-rules"),
        ("Terminal", "Reopen tabs from last time", "12-prefs-startup"),
    ] {
        if let Some(tab) = app.query_all_by_label(page).last() {
            tab.click();
            app.run_ok();
        }
        let found = app.query_all_by_label(label).next().is_some();
        if found {
            app.query_all_by_label(label).last().unwrap().scroll_to_me();
        }
        check(&format!("prefs: {label}"), found, page);
        shot(&mut app, file);
    }
    if let Some(cancel) = app.query_by_label("Cancel") {
        cancel.click();
        app.run_ok();
    }

    // --- Serial line ending options in the form ---
    app.state_mut().set_form_kind(Kind::Serial);
    app.run_ok();
    if let Some(adv) = app.query_by_label("Advanced") {
        adv.click();
        app.run_ok();
    }
    let enter = app.query_all_by_label_contains("Enter sends").next().is_some()
        || app.query_all_by_label_contains("CR+LF").next().is_some();
    check("serial Enter sends option", enter, "");
    shot(&mut app, "13-serial-line-endings");

    // --- OpenSSH config import preview ---
    let config = dir.path().join("ssh_config");
    std::fs::write(
        &config,
        "Host core-sw1\n  HostName 10.0.0.1\n  User admin\n\nHost fw-*\n  User ops\n\nHost bastion\n  HostName 192.0.2.10\n  Port 2222\n  IdentityFile ~/.ssh/id_ed25519\n  ProxyCommand nc %h %p\n",
    )
    .unwrap();
    let previewed = app.state_mut().preview_openssh_config(&config);
    app.run_ok();
    let rows = app.query_all_by_label_contains("core-sw1").next().is_some()
        && app.query_all_by_label_contains("bastion").next().is_some();
    check("OpenSSH import preview", previewed.is_ok() && rows, format!("{previewed:?}"));
    shot(&mut app, "14-openssh-import");
    if let Some(cancel) = app.query_by_label("Cancel") {
        cancel.click();
        app.run_ok();
    }
    drop(app);

    // --- Reopen tabs from last time: comes back disconnected ---
    let mut settings = SettingsStore::new(paths.settings.clone()).load();
    settings.restore_tabs = true;
    settings.last_tabs = LastTabs {
        tabs: vec![
            LastTab { profile: "Lab".into(), explorer: false },
            LastTab { profile: "Lab B".into(), explorer: false },
            LastTab { profile: "Deleted since".into(), explorer: false },
        ],
        split: vec![vec![0], vec![1]],
        active: Some(1),
    };
    SettingsStore::new(paths.settings.clone()).save(&settings).unwrap();
    lab.set_nonblocking(true).unwrap();
    while lab.accept().is_ok() {}
    let restore_paths = copy(&paths);
    let mut app: App = Harness::builder()
        .with_size([1100.0, 700.0])
        .build_eframe(move |_| SnekkieApp::with_port_lister(restore_paths, Vec::new));
    wait(&mut app, 2, |_| false);
    let titles = app.state().tab_titles();
    check("restored tabs (deleted one skipped)", titles.len() == 2, format!("{titles:?}"));
    check("restore did not dial", lab.accept().is_err(), "");
    shot(&mut app, "15-restored-tabs");
    drop(app);

    // --- MFA: password, then a one-time code in a dialog ---
    mfa(&paths);
}

const HOST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACAIdCtIeTKkWpGPwZ1WGfgEmJVkMCDSbhvvS00tyosB7AAAAJgK59adCufW
nQAAAAtzc2gtZWQyNTUxOQAAACAIdCtIeTKkWpGPwZ1WGfgEmJVkMCDSbhvvS00tyosB7A
AAAEAsQ81aU+QfOwsOXZ+Q/59IP1AZ8y/yUHTk0ga7xgtfugh0K0h5MqRakY/BnVYZ+ASY
lWQwINJuG+9LTS3KiwHsAAAAE3NuZWtraWUgdGVzdCBzZXJ2ZXIBAg==
-----END OPENSSH PRIVATE KEY-----
";

/// Keyboard-interactive: "Password: " then "Verification code: ".
struct Mfa(usize);

impl server::Handler for Mfa {
    type Error = russh::Error;
    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        self.0 += 1;
        let answer = response.and_then(|mut r| r.next()).unwrap_or_default();
        let ask = |p: &'static str| Auth::Partial {
            name: "Lab MFA".into(),
            instructions: "Enter the code from your token".into(),
            prompts: vec![(p.into(), false)].into(),
        };
        Ok(match self.0 {
            1 => ask("Password: "),
            2 if answer.as_ref() == b"lab-pass" => ask("Verification code: "),
            3 if answer.as_ref() == b"123456" => Auth::Accept,
            _ => Auth::reject(),
        })
    }
    async fn channel_open_session(
        &mut self,
        _c: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _s: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.data(channel, "MFA login ok\r\nbastion$ ")?;
        Ok(())
    }
}

fn mfa(paths: &Paths) {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let config = Arc::new(server::Config {
        keys: vec![russh::keys::decode_secret_key(HOST_KEY, None).unwrap()],
        methods: MethodSet::from(&[MethodKind::KeyboardInteractive][..]),
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    runtime.spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let config = config.clone();
            tokio::spawn(async move {
                if let Ok(s) = server::run_stream(config, socket, Mfa(0)).await {
                    let _ = s.await;
                }
            });
        }
    });
    let mut settings = SettingsStore::new(paths.settings.clone()).load();
    settings.restore_tabs = false;
    SettingsStore::new(paths.settings.clone()).save(&settings).unwrap();
    let p = copy(paths);
    let mut app: App =
        Harness::builder().with_size([1100.0, 700.0]).build_eframe(move |_| SnekkieApp::with_port_lister(p, Vec::new));
    app.run_ok();
    let ctx = app.ctx.clone();
    app.state_mut().open_session(
        &ctx,
        Profile {
            name: "Bastion".into(),
            kind: "ssh".into(),
            host: "127.0.0.1".into(),
            port,
            username: "admin".into(),
            auth: "password".into(),
            ..Default::default()
        },
    );
    // Password dialog first, then the host key, then the code.
    for step in 0..30 {
        app.run_ok();
        std::thread::sleep(Duration::from_millis(100));
        let dialogs = app.state().dialog_texts();
        if screen(&app).contains("MFA login ok") {
            break;
        }
        if dialogs.iter().any(|d| d.contains("Verification code")) {
            shot(&mut app, "16-mfa-code-dialog");
            let field = app
                .query_all_by_role(Role::PasswordInput)
                .last()
                .or_else(|| app.query_all_by_role(Role::TextInput).last());
            if let Some(field) = field {
                field.click();
                app.run_ok();
                app.event(egui::Event::Text("123456".into()));
                app.run_ok();
            }
            if let Some(ok) = app.query_by_label("OK") {
                ok.click();
            }
            continue;
        }
        if app.query_by_label("Accept").is_some() {
            shot(&mut app, "16a-host-key");
            app.get_by_label("Accept").click();
            continue;
        }
        if let Some(field) = app.query_all_by_role(Role::PasswordInput).last()
            && step < 29
            && !dialogs.iter().any(|d| d.contains("Verification"))
        {
            {
                field.click();
                app.run_ok();
                app.event(egui::Event::Text("lab-pass".into()));
                app.run_ok();
                shot(&mut app, "16b-password");
                app.key_press(egui::Key::Enter);
            }
        }
    }
    let ok = wait(&mut app, 5, |a| screen(a).contains("MFA login ok"));
    check("MFA: password then code dialog logs in", ok, format!("dialogs={:?}", app.state().dialog_texts()));
    shot(&mut app, "17-mfa-logged-in");
}

fn copy(paths: &Paths) -> Paths {
    Paths { sessions: paths.sessions.clone(), settings: paths.settings.clone() }
}
