//! Jump hosts (ProxyJump) end to end: in-process SSH servers where each jump
//! host forwards direct-tcpip channels to the next, as a bastion does.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Event, Recorder};
use parking_lot::Mutex;
use russh::server::{self, Auth, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use snekkie::profiles::{self, Profile};
use snekkie::transport::ssh::{self, Credentials, HostKeyAsker, HostKeyQuestion, PromptAsker, PromptQuestion};
use snekkie::transport::{Link, Sink};

const WAIT: Duration = Duration::from_secs(10);

/// Throwaway key for the test servers only.
const HOST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACAIdCtIeTKkWpGPwZ1WGfgEmJVkMCDSbhvvS00tyosB7AAAAJgK59adCufW
nQAAAAtzc2gtZWQyNTUxOQAAACAIdCtIeTKkWpGPwZ1WGfgEmJVkMCDSbhvvS00tyosB7A
AAAEAsQ81aU+QfOwsOXZ+Q/59IP1AZ8y/yUHTk0ga7xgtfugh0K0h5MqRakY/BnVYZ+ASY
lWQwINJuG+9LTS3KiwHsAAAAE3NuZWtraWUgdGVzdCBzZXJ2ZXIBAg==
-----END OPENSSH PRIVATE KEY-----
";

/// Where each server was asked to forward to.
type Forwards = Arc<Mutex<Vec<(String, u32)>>>;

#[derive(Clone)]
struct Server {
    password: &'static str,
    greeting: &'static str,
    forwards: Forwards,
    /// Shell channels, the only ones echoed: forwarded channels carry SSH.
    shells: Vec<ChannelId>,
}

impl server::Handler for Server {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if password == self.password { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.shells.push(channel.id());
        reply.accept().await;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.data(channel, format!("{}\r\n", self.greeting))?;
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if self.shells.contains(&channel) {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.forwards.lock().push((host_to_connect.to_string(), port_to_connect));
        match tokio::net::TcpStream::connect((host_to_connect, port_to_connect as u16)).await {
            Ok(mut tcp) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
                });
            }
            Err(_) => reply.reject(russh::ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }
}

struct Harness {
    runtime: tokio::runtime::Runtime,
    known_hosts: tempfile::TempDir,
}

impl Harness {
    fn new() -> Harness {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        Harness { runtime, known_hosts: tempfile::tempdir().unwrap() }
    }

    /// An SSH server on a free port, and what it was asked to forward.
    fn server(&self, password: &'static str, greeting: &'static str) -> (u16, Forwards) {
        let forwards = Forwards::default();
        let handler = Server { password, greeting, forwards: forwards.clone(), shells: Vec::new() };
        let config = Arc::new(server::Config {
            keys: vec![russh::keys::decode_secret_key(HOST_KEY, None).unwrap()],
            methods: MethodSet::from(&[MethodKind::Password][..]),
            auth_rejection_time: Duration::from_millis(10),
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let listener = self.runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let port = listener.local_addr().unwrap().port();
        self.runtime.spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (config, handler) = (config.clone(), handler.clone());
                tokio::spawn(async move {
                    if let Ok(session) = server::run_stream(config, socket, handler).await {
                        let _ = session.await;
                    }
                });
            }
        });
        (port, forwards)
    }

    fn known_hosts_path(&self) -> std::path::PathBuf {
        self.known_hosts.path().join("known_hosts")
    }

    /// Connect `target` through its saved jump hosts, answering every
    /// password prompt from `passwords` by the prompt's host label.
    fn connect(&self, target: &Profile, saved: &[Profile], passwords: &'static [(&'static str, &'static str)]) -> Run {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let host_keys = Arc::new(Mutex::new(Vec::new()));
        let seen = prompts.clone();
        let ask_prompts: PromptAsker = Arc::new(move |q: PromptQuestion| {
            let answer = passwords.iter().find(|(host, _)| q.host.contains(host)).map(|(_, p)| vec![p.to_string()]);
            seen.lock().push((q.host.clone(), q.name.clone(), q.prompts.clone()));
            let _ = q.reply.send(answer);
        });
        let keys = host_keys.clone();
        let ask: HostKeyAsker = Arc::new(move |q: HostKeyQuestion| {
            keys.lock().push((q.host.clone(), q.port, q.jump_host.clone()));
            let _ = q.reply.send(true);
        });
        let credentials = Credentials {
            password: "target-pw".into(),
            key_passphrase: String::new(),
            known_hosts: self.known_hosts_path(),
            ask_prompts: Some(ask_prompts),
            jumps: profiles::jump_chain(target, saved).unwrap(),
        };
        let recorder = Recorder::new();
        let link = ssh::start(
            self.runtime.handle(),
            target.clone(),
            credentials,
            (80, 24),
            ask,
            recorder.clone() as Arc<dyn Sink>,
        );
        Run { recorder, link, prompts, host_keys }
    }
}

/// Each prompt shown: its host label, title and prompts.
type Prompts = Arc<Mutex<Vec<(String, String, Vec<(String, bool)>)>>>;

type HostKeyAsked = (String, u16, Option<String>);

struct Run {
    recorder: Arc<Recorder>,
    link: Link,
    prompts: Prompts,
    /// (host, port, jump host) for each host-key question.
    host_keys: Arc<Mutex<Vec<HostKeyAsked>>>,
}

impl Run {
    fn closed_reason(&self) -> String {
        match self.recorder.wait_event(WAIT, |e| matches!(e, Event::Closed(_))) {
            Event::Closed(Some(reason)) => reason,
            other => panic!("expected a failure, got {other:?}"),
        }
    }
}

fn profile(name: &str, port: u16, user: &str, jump: &str) -> Profile {
    Profile {
        name: name.into(),
        host: "127.0.0.1".into(),
        port,
        username: user.into(),
        auth: "password".into(),
        jump_host: jump.into(),
        ..Profile::default()
    }
}

#[test]
fn logs_in_through_two_jump_hosts_checking_each_host_key_and_asking_each_password() {
    let harness = Harness::new();
    let (edge_port, edge_forwards) = harness.server("edge-pw", "edge shell");
    let (core_port, core_forwards) = harness.server("core-pw", "core shell");
    let (target_port, target_forwards) = harness.server("target-pw", "router shell");
    let saved = [profile("edge", edge_port, "jump", ""), profile("core", core_port, "ops", "edge")];
    let target = profile("router", target_port, "admin", "core");
    const PASSWORDS: &[(&str, &str)] = &[("Jump host edge", "edge-pw"), ("Jump host core", "core-pw")];

    let run = harness.connect(&target, &saved, PASSWORDS);
    run.recorder.wait_event(WAIT, |e| *e == Event::Connected);
    run.recorder.wait_text(WAIT, "router shell");
    run.link.write(b"show version\r".to_vec());
    run.recorder.wait_text(WAIT, "show version");
    assert!(!run.recorder.text().contains("edge shell") && !run.recorder.text().contains("core shell"));

    // Each hop reached the next; the target was only reached through core.
    assert_eq!(*edge_forwards.lock(), [("127.0.0.1".to_string(), core_port as u32)]);
    assert_eq!(*core_forwards.lock(), [("127.0.0.1".to_string(), target_port as u32)]);
    assert!(target_forwards.lock().is_empty());

    // Every host key was asked about under its own port, first hop first,
    // naming the jump hosts so the user knows which hop they are trusting.
    let local = |port, hop: Option<&str>| ("127.0.0.1".to_string(), port, hop.map(String::from));
    assert_eq!(
        *run.host_keys.lock(),
        [local(edge_port, Some("edge")), local(core_port, Some("core")), local(target_port, None)]
    );
    let known = std::fs::read_to_string(harness.known_hosts_path()).unwrap();
    for port in [edge_port, core_port, target_port] {
        assert!(known.contains(&format!("[127.0.0.1]:{port}")), "{known}");
    }

    // Each jump host asked for its own password, naming itself.
    let prompts = run.prompts.lock().clone();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_eq!(prompts[0].0, "Jump host edge (127.0.0.1)");
    assert_eq!(prompts[0].1, "SSH password");
    assert_eq!(prompts[0].2, [("Password for jump@127.0.0.1:".to_string(), false)]);
    assert_eq!(prompts[1].0, "Jump host core (127.0.0.1)");
    assert_eq!(prompts[1].2, [("Password for ops@127.0.0.1:".to_string(), false)]);
    run.link.send(snekkie::transport::Command::Close);
    run.recorder.wait_event(WAIT, |e| *e == Event::Closed(None));

    // Recorded keys are trusted next time without asking.
    let again = harness.connect(&target, &saved, PASSWORDS);
    again.recorder.wait_text(WAIT, "router shell");
    assert!(again.host_keys.lock().is_empty(), "{:?}", again.host_keys.lock());
}

#[test]
fn a_failing_jump_host_is_named() {
    let harness = Harness::new();
    let (edge_port, _) = harness.server("edge-pw", "edge shell");
    let (target_port, target_forwards) = harness.server("target-pw", "router shell");
    let saved = [profile("edge", edge_port, "jump", "")];

    // Wrong password at the jump host: the target is never contacted.
    let target = profile("router", target_port, "admin", "edge");
    let run = harness.connect(&target, &saved, &[("Jump host edge", "wrong")]);
    assert_eq!(run.closed_reason(), "Jump host edge: Authentication failed for jump@127.0.0.1");
    assert!(target_forwards.lock().is_empty());
    assert_eq!(run.host_keys.lock().len(), 1);

    // Canceling the jump host's password prompt.
    let run = harness.connect(&target, &saved, &[]);
    assert_eq!(run.closed_reason(), "Jump host edge: Login canceled");

    // The jump host can't reach the next host.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = closed.local_addr().unwrap().port();
    drop(closed);
    let target = profile("router", dead_port, "admin", "edge");
    let run = harness.connect(&target, &saved, &[("Jump host edge", "edge-pw")]);
    let reason = run.closed_reason();
    assert!(reason.starts_with(&format!("Jump host edge could not reach 127.0.0.1:{dead_port}")), "{reason}");

    // The first hop itself is down.
    let saved = [profile("edge", dead_port, "jump", "")];
    let target = profile("router", target_port, "admin", "edge");
    let run = harness.connect(&target, &saved, &[("Jump host edge", "edge-pw")]);
    let reason = run.closed_reason();
    assert!(reason.starts_with("Jump host edge: "), "{reason}");
}

#[test]
fn the_session_closes_naming_the_jump_host_that_dropped() {
    let harness = Harness::new();
    let (edge_port, _) = harness.server("edge-pw", "edge shell");
    let (target_port, _) = harness.server("target-pw", "router shell");

    // The first hop runs through a proxy we can cut, like a link going down.
    let cut = Arc::new(tokio::sync::Notify::new());
    let listener = harness.runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let signal = cut.clone();
    harness.runtime.spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let mut server = tokio::net::TcpStream::connect(("127.0.0.1", edge_port)).await.unwrap();
            let signal = signal.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut client, &mut server) => {}
                    () = signal.notified() => {}
                }
            });
        }
    });

    let saved = [profile("edge", proxy_port, "jump", "")];
    let target = profile("router", target_port, "admin", "edge");
    let run = harness.connect(&target, &saved, &[("Jump host edge", "edge-pw")]);
    run.recorder.wait_text(WAIT, "router shell");
    cut.notify_waiters();
    let reason = run.closed_reason();
    assert!(reason.starts_with("Connection lost at jump host edge"), "{reason}");
}

#[test]
fn file_explorer_connections_go_through_jump_hosts_too() {
    let harness = Harness::new();
    let (edge_port, edge_forwards) = harness.server("edge-pw", "edge shell");
    let (target_port, _) = harness.server("target-pw", "router shell");
    let saved = [profile("edge", edge_port, "jump", "")];
    let target = profile("router", target_port, "admin", "edge");
    let ask_prompts: PromptAsker = Arc::new(|q: PromptQuestion| {
        let _ = q.reply.send(Some(vec!["edge-pw".into()]));
    });
    let credentials = Credentials {
        password: "target-pw".into(),
        key_passphrase: String::new(),
        known_hosts: harness.known_hosts_path(),
        ask_prompts: Some(ask_prompts),
        jumps: profiles::jump_chain(&target, &saved).unwrap(),
    };
    let ask: HostKeyAsker = Arc::new(|q: HostKeyQuestion| {
        let _ = q.reply.send(true);
    });
    let pending = ssh::connect(
        harness.runtime.handle(),
        target,
        credentials,
        ask,
        Recorder::new() as Arc<dyn Sink>,
        Default::default(),
    );
    let connection = harness.runtime.block_on(pending).unwrap().unwrap();
    assert!(!connection.is_closed());
    assert_eq!(*edge_forwards.lock(), [("127.0.0.1".to_string(), target_port as u32)]);
    harness.runtime.block_on(connection.release());
}
