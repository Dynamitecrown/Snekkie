//! Password and keyboard-interactive logins against an in-process SSH
//! server, so they run anywhere without touching system accounts.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Event, Recorder};
use russh::server::{self, Auth, Msg, Response, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use snekkie::profiles::Profile;
use snekkie::transport::ssh::{self, Credentials, HostKeyAsker, HostKeyQuestion};
use snekkie::transport::{Link, Sink};

const WAIT: Duration = Duration::from_secs(10);
const PASSWORD: &str = "correct horse";

/// Throwaway key for the test server only.
const HOST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACAIdCtIeTKkWpGPwZ1WGfgEmJVkMCDSbhvvS00tyosB7AAAAJgK59adCufW
nQAAAAtzc2gtZWQyNTUxOQAAACAIdCtIeTKkWpGPwZ1WGfgEmJVkMCDSbhvvS00tyosB7A
AAAEAsQ81aU+QfOwsOXZ+Q/59IP1AZ8y/yUHTk0ga7xgtfugh0K0h5MqRakY/BnVYZ+ASY
lWQwINJuG+9LTS3KiwHsAAAAE3NuZWtraWUgdGVzdCBzZXJ2ZXIBAg==
-----END OPENSSH PRIVATE KEY-----
";

#[derive(Clone, Copy)]
enum Mode {
    /// Plain password authentication.
    Password,
    /// Keyboard-interactive only, one "Password:" prompt. How a lot of
    /// network gear and PAM-based servers are set up.
    KeyboardInteractive,
    /// Offers password, but always rejects it, then accepts the same
    /// password through keyboard-interactive after an empty first round.
    PasswordRejectedThenKeyboardInteractive,
}

struct Handler {
    mode: Mode,
    round: usize,
}

impl server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(match self.mode {
            Mode::Password if password == PASSWORD => Auth::Accept,
            _ => Auth::reject(),
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        if matches!(self.mode, Mode::Password) {
            return Ok(Auth::reject());
        }
        self.round += 1;
        // Rounds that ask something before the answer is checked. The
        // fallback mode starts with an empty round, as some servers do.
        let empty_first = matches!(self.mode, Mode::PasswordRejectedThenKeyboardInteractive);
        let asking_rounds = if empty_first { 2 } else { 1 };
        if self.round <= asking_rounds {
            let prompts: Vec<(std::borrow::Cow<'static, str>, bool)> =
                if empty_first && self.round == 1 { Vec::new() } else { vec![("Password: ".into(), false)] };
            return Ok(Auth::Partial { name: "".into(), instructions: "".into(), prompts: prompts.into() });
        }
        let answer = response.and_then(|mut r| r.next()).unwrap_or_default();
        Ok(if answer.as_ref() == PASSWORD.as_bytes() { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.data(channel, "welcome\r\n")?;
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        session.data(channel, data.to_vec())?;
        Ok(())
    }
}

struct Harness {
    runtime: tokio::runtime::Runtime,
    port: u16,
    known_hosts: tempfile::TempDir,
}

fn start_server(mode: Mode) -> Harness {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let methods: &[MethodKind] = match mode {
        Mode::Password => &[MethodKind::Password],
        Mode::KeyboardInteractive => &[MethodKind::KeyboardInteractive],
        Mode::PasswordRejectedThenKeyboardInteractive => &[MethodKind::Password, MethodKind::KeyboardInteractive],
    };
    let config = Arc::new(server::Config {
        keys: vec![russh::keys::decode_secret_key(HOST_KEY, None).unwrap()],
        methods: MethodSet::from(methods),
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
                if let Ok(session) = server::run_stream(config, socket, Handler { mode, round: 0 }).await {
                    let _ = session.await;
                }
            });
        }
    });
    Harness { runtime, port, known_hosts: tempfile::tempdir().unwrap() }
}

impl Harness {
    fn connect(&self, password: &str) -> (Arc<Recorder>, Link) {
        let recorder = Recorder::new();
        let profile = Profile {
            host: "127.0.0.1".into(),
            port: self.port,
            username: "admin".into(),
            auth: "password".into(),
            ..Profile::default()
        };
        let credentials = Credentials {
            password: password.into(),
            key_passphrase: String::new(),
            known_hosts: self.known_hosts.path().join("known_hosts"),
        };
        let ask: HostKeyAsker = Arc::new(|q: HostKeyQuestion| {
            let _ = q.reply.send(true);
        });
        let link =
            ssh::start(self.runtime.handle(), profile, credentials, (80, 24), ask, recorder.clone() as Arc<dyn Sink>);
        (recorder, link)
    }
}

fn assert_logs_in(mode: Mode) {
    let harness = start_server(mode);
    let (recorder, link) = harness.connect(PASSWORD);
    recorder.wait_event(WAIT, |e| *e == Event::Connected);
    recorder.wait_text(WAIT, "welcome");
    link.write(b"show version\r".to_vec());
    recorder.wait_text(WAIT, "show version");
}

fn assert_rejected(mode: Mode) {
    let harness = start_server(mode);
    let (recorder, _link) = harness.connect("wrong");
    match recorder.wait_event(WAIT, |e| matches!(e, Event::Closed(_))) {
        Event::Closed(Some(reason)) => assert!(reason.starts_with("Authentication failed"), "{reason}"),
        other => panic!("expected an authentication failure, got {other:?}"),
    }
    assert!(!recorder.events().contains(&Event::Connected));
}

#[test]
fn password_login() {
    assert_logs_in(Mode::Password);
}

#[test]
fn wrong_password_is_reported() {
    assert_rejected(Mode::Password);
}

#[test]
fn keyboard_interactive_only_server() {
    assert_logs_in(Mode::KeyboardInteractive);
}

#[test]
fn wrong_password_through_keyboard_interactive() {
    assert_rejected(Mode::KeyboardInteractive);
}

#[test]
fn password_rejected_then_keyboard_interactive_with_empty_round() {
    assert_logs_in(Mode::PasswordRejectedThenKeyboardInteractive);
}
