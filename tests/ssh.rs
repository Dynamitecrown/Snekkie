//! End-to-end SSH tests against a real OpenSSH server started for each test.
//!
//! Opt-in with `SNEKKIE_SSHD_TESTS=1` (and skipped when sshd isn't
//! installed): sshd's behaviour depends on how the machine runs it, so these
//! stay out of CI. Password and keyboard-interactive logins are covered
//! everywhere by tests/ssh_auth.rs instead.

#![cfg(unix)]

mod common;

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Event, Recorder};
use snekkie::profiles::Profile;
use snekkie::transport::ssh::{self, Credentials, HostKeyAsker, HostKeyQuestion};
use snekkie::transport::{Command, Link, Sink};

const WAIT: Duration = Duration::from_secs(15);

fn find(binary: &str) -> Option<PathBuf> {
    ["/usr/sbin", "/usr/bin", "/usr/local/sbin", "/usr/local/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(binary))
        .find(|p| p.exists())
}

fn whoami() -> String {
    let out = Process::new("id").arg("-un").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn keygen(path: &Path, kind: &str, passphrase: &str) {
    let status = Process::new(find("ssh-keygen").unwrap())
        .args(["-q", "-t", kind, "-N", passphrase, "-C", "snekkie-test", "-f"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

struct Sshd {
    child: Child,
    port: u16,
    dir: tempfile::TempDir,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sshd {
    /// Start sshd with the given extra config lines, or None to skip.
    fn start(extra: &str) -> Option<Sshd> {
        std::env::var_os("SNEKKIE_SSHD_TESTS")?;
        let sshd = find("sshd")?;
        find("ssh-keygen")?;
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        keygen(&d.join("host_ed25519"), "ed25519", "");
        keygen(&d.join("host_rsa"), "rsa", "");
        keygen(&d.join("user"), "ed25519", "");
        keygen(&d.join("user_encrypted"), "ed25519", "hunter2");
        let mut authorized = fs::read_to_string(d.join("user.pub")).unwrap();
        authorized += &fs::read_to_string(d.join("user_encrypted.pub")).unwrap();
        fs::write(d.join("authorized_keys"), authorized).unwrap();
        fs::write(d.join("banner"), "Authorized access only\n").unwrap();

        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let config = format!(
            "Port {port}\nListenAddress 127.0.0.1\n\
             HostKey {d}/host_ed25519\nHostKey {d}/host_rsa\n\
             PidFile {d}/sshd.pid\nAuthorizedKeysFile {d}/authorized_keys\n\
             StrictModes no\nPermitRootLogin yes\nBanner {d}/banner\nLogLevel ERROR\n{extra}\n",
            d = d.display()
        );
        fs::write(d.join("sshd_config"), config).unwrap();
        let child = Process::new(sshd)
            .args(["-D", "-e", "-f"])
            .arg(d.join("sshd_config"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(Sshd { child, port, dir })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn profile(&self, auth: &str) -> Profile {
        Profile {
            kind: "ssh".into(),
            host: "127.0.0.1".into(),
            port: self.port,
            username: whoami(),
            auth: auth.into(),
            key_file: self.path("user").display().to_string(),
            ..Profile::default()
        }
    }

    fn credentials(&self, password: &str, passphrase: &str) -> Credentials {
        Credentials {
            password: password.into(),
            key_passphrase: passphrase.into(),
            known_hosts: self.path("known_hosts"),
        }
    }
}

macro_rules! sshd_or_skip {
    ($extra:expr) => {
        match Sshd::start($extra) {
            Some(server) => server,
            None => {
                eprintln!("skipping: set SNEKKIE_SSHD_TESTS=1 with sshd installed to run");
                return;
            }
        }
    };
}

/// Records host key questions and answers them with a fixed reply.
fn asker(accept: bool) -> (HostKeyAsker, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let record = asked.clone();
    let ask: HostKeyAsker = Arc::new(move |q: HostKeyQuestion| {
        record.lock().unwrap().push(format!("{} {} {}", q.host, q.key_type, q.fingerprint));
        let _ = q.reply.send(accept);
    });
    (ask, asked)
}

struct Connection {
    runtime: tokio::runtime::Runtime,
    recorder: Arc<Recorder>,
    link: Link,
}

fn connect(profile: Profile, credentials: Credentials, ask: HostKeyAsker) -> Connection {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let recorder = Recorder::new();
    let link = ssh::start(runtime.handle(), profile, credentials, (80, 24), ask, recorder.clone() as Arc<dyn Sink>);
    Connection { runtime, recorder, link }
}

fn closed_reason(conn: &Connection) -> String {
    match conn.recorder.wait_event(WAIT, |e| matches!(e, Event::Closed(_))) {
        Event::Closed(Some(reason)) => reason,
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn run_shell_command(conn: &Connection, command: &str, expect: &str) {
    conn.recorder.wait_event(WAIT, |e| *e == Event::Connected);
    conn.link.write(format!("{command}\r").into_bytes());
    conn.recorder.wait_text(WAIT, expect);
}

#[test]
fn key_login_asks_about_the_host_key_once() {
    let server = sshd_or_skip!("PasswordAuthentication no");
    let (ask, asked) = asker(true);
    let conn = connect(server.profile("key"), server.credentials("", ""), ask);
    run_shell_command(&conn, "echo snekkie-$((6*7))", "snekkie-42");
    // The pre-login banner is shown too.
    assert!(conn.recorder.text().contains("Authorized access only"));
    let questions = asked.lock().unwrap().clone();
    assert_eq!(questions.len(), 1, "{questions:?}");
    assert!(questions[0].starts_with("127.0.0.1 ssh-ed25519 SHA256:"), "{questions:?}");
    assert!(fs::read_to_string(server.path("known_hosts")).unwrap().contains("ssh-ed25519"));

    conn.link.send(Command::Close);
    assert_eq!(conn.recorder.wait_event(WAIT, |e| matches!(e, Event::Closed(_))), Event::Closed(None));
    drop(conn);

    // Now that the key is recorded, a second connection doesn't ask.
    let (ask, asked) = asker(false);
    let conn = connect(server.profile("key"), server.credentials("", ""), ask);
    run_shell_command(&conn, "echo again-$((1+1))", "again-2");
    assert!(asked.lock().unwrap().is_empty());
    drop(conn.runtime);
}

#[test]
fn rejecting_the_host_key_ends_the_session() {
    let server = sshd_or_skip!("");
    let (ask, _) = asker(false);
    let conn = connect(server.profile("key"), server.credentials("", ""), ask);
    let reason = closed_reason(&conn);
    assert!(reason.contains("not accepted"), "{reason}");
    assert!(!server.path("known_hosts").exists());
}

#[test]
fn a_changed_host_key_is_refused_without_asking() {
    let server = sshd_or_skip!("");
    // Record a different key for this host and port.
    let other = server.path("other");
    keygen(&other, "ed25519", "");
    let other_pub = fs::read_to_string(other.with_extension("pub")).unwrap();
    let mut fields = other_pub.split_whitespace();
    let line = format!("[127.0.0.1]:{} {} {}\n", server.port, fields.next().unwrap(), fields.next().unwrap());
    fs::write(server.path("known_hosts"), line).unwrap();

    let (ask, asked) = asker(true);
    let conn = connect(server.profile("key"), server.credentials("", ""), ask);
    let reason = closed_reason(&conn);
    assert!(reason.contains("does not match"), "{reason}");
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn encrypted_key_needs_its_passphrase() {
    let server = sshd_or_skip!("PasswordAuthentication no");
    let mut profile = server.profile("key");
    profile.key_file = server.path("user_encrypted").display().to_string();

    let (ask, _) = asker(true);
    let conn = connect(profile.clone(), server.credentials("", ""), ask);
    let reason = closed_reason(&conn);
    assert!(reason.contains("encrypted"), "{reason}");

    let (ask, _) = asker(true);
    let conn = connect(profile, server.credentials("", "hunter2"), ask);
    run_shell_command(&conn, "echo unlocked", "unlocked");
}

#[test]
fn window_size_follows_resizes() {
    let server = sshd_or_skip!("");
    let (ask, _) = asker(true);
    let conn = connect(server.profile("key"), server.credentials("", ""), ask);
    run_shell_command(&conn, "stty size", "24 80");
    conn.link.send(Command::Resize { columns: 100, lines: 30 });
    run_shell_command(&conn, "stty size", "30 100");
}

#[test]
fn legacy_only_server_is_still_reachable() {
    // What an older Cisco IOS image offers: SHA-1 key exchange, CBC,
    // HMAC-SHA1 and an ssh-rsa host key.
    let server = sshd_or_skip!(
        "KexAlgorithms diffie-hellman-group14-sha1\nCiphers aes128-cbc\nMACs hmac-sha1\n\
         HostKeyAlgorithms ssh-rsa\nPubkeyAcceptedAlgorithms +ssh-rsa"
    );
    let (ask, asked) = asker(true);
    let conn = connect(server.profile("key"), server.credentials("", ""), ask);
    run_shell_command(&conn, "echo legacy-ok", "legacy-ok");
    assert!(asked.lock().unwrap()[0].contains("ssh-rsa"));
}

#[test]
fn unreachable_host_fails_quickly() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let profile = Profile { host: "127.0.0.1".into(), port, ..Profile::default() };
    let (ask, _) = asker(true);
    let dir = tempfile::tempdir().unwrap();
    let credentials =
        Credentials { password: String::new(), key_passphrase: String::new(), known_hosts: dir.path().join("kh") };
    let conn = connect(profile, credentials, ask);
    let reason = closed_reason(&conn);
    assert!(reason.starts_with("Could not connect to 127.0.0.1"), "{reason}");
}
