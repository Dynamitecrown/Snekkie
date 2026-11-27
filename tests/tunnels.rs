//! SSH port forwarding end to end: in-process SSH servers that open
//! direct-tcpip channels and honor tcpip-forward requests, as OpenSSH does,
//! and plain TCP echo servers as the far ends.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Event, Recorder};
use parking_lot::Mutex;
use russh::server::{self, Auth, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use snekkie::network::NetworkAccess;
use snekkie::profiles::{self, Profile, Tunnel, TunnelKind};
use snekkie::transport::ssh::{self, Credentials, HostKeyAsker, HostKeyQuestion};
use snekkie::transport::tunnel::{TunnelState, TunnelStatus};
use snekkie::transport::{Command, Link, Sink};

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

/// What a server was asked: ("direct", host, port) or ("forward", address, port).
type Requests = Arc<Mutex<Vec<(&'static str, String, u32)>>>;

#[derive(Clone)]
struct Server {
    requests: Requests,
    shells: Vec<ChannelId>,
}

impl server::Handler for Server {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if password == "pw" { Auth::Accept } else { Auth::reject() })
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
        session.data(channel, "router#\r\n".to_string())?;
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
        self.requests.lock().push(("direct", host_to_connect.to_string(), port_to_connect));
        match tokio::net::TcpStream::connect((host_to_connect, port_to_connect as u16)).await {
            Ok(mut tcp) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let _ = tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut tcp).await;
                });
            }
            Err(_) => reply.reject(russh::ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.requests.lock().push(("forward", address.to_string(), *port));
        // Like sshd: listen where asked, or refuse if the port is taken.
        let Ok(listener) = tokio::net::TcpListener::bind((address, *port as u16)).await else { return Ok(false) };
        let (handle, address, port) = (session.handle(), address.to_string(), *port);
        tokio::spawn(async move {
            while let Ok((mut socket, peer)) = listener.accept().await {
                let opened = handle
                    .channel_open_forwarded_tcpip(address.clone(), port, peer.ip().to_string(), peer.port().into())
                    .await;
                if let Ok(channel) = opened {
                    tokio::spawn(async move {
                        let _ = tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut socket).await;
                    });
                }
            }
        });
        Ok(true)
    }
}

/// Records like [`Recorder`], plus the latest tunnel states.
struct TunnelSink {
    recorder: Arc<Recorder>,
    tunnels: Mutex<Vec<TunnelStatus>>,
}

impl Sink for TunnelSink {
    fn connected(&self) {
        self.recorder.connected();
    }
    fn data(&self, bytes: &[u8]) {
        self.recorder.data(bytes);
    }
    fn closed(&self, reason: Option<String>) {
        self.recorder.closed(reason);
    }
    fn notice(&self, message: String) {
        self.recorder.notice(message);
    }
    fn tunnels(&self, status: Vec<TunnelStatus>) {
        *self.tunnels.lock() = status;
    }
}

struct Harness {
    runtime: tokio::runtime::Runtime,
    known_hosts: tempfile::TempDir,
}

struct Run {
    sink: Arc<TunnelSink>,
    link: Link,
}

impl Run {
    /// Wait until `check` holds for the tunnel states.
    fn wait_tunnels(&self, check: impl Fn(&[TunnelStatus]) -> bool) -> Vec<TunnelStatus> {
        let deadline = Instant::now() + WAIT;
        loop {
            let tunnels = self.sink.tunnels.lock().clone();
            if check(&tunnels) {
                return tunnels;
            }
            assert!(Instant::now() < deadline, "tunnels never got there: {tunnels:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_listening(&self, count: usize) {
        self.wait_tunnels(|t| t.iter().filter(|t| t.state == TunnelState::Listening).count() == count);
    }
}

impl Harness {
    fn new() -> Harness {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        Harness { runtime, known_hosts: tempfile::tempdir().unwrap() }
    }

    fn server(&self) -> (u16, Requests) {
        let requests = Requests::default();
        let handler = Server { requests: requests.clone(), shells: Vec::new() };
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
        (port, requests)
    }

    /// A TCP server on 127.0.0.1 that echoes everything back.
    fn echo(&self) -> u16 {
        let listener = self.runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let port = listener.local_addr().unwrap().port();
        self.runtime.spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = socket.split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });
        port
    }

    fn connect(&self, target: &Profile, saved: &[Profile], access: NetworkAccess) -> Run {
        let ask: HostKeyAsker = Arc::new(|q: HostKeyQuestion| {
            let _ = q.reply.send(true);
        });
        let credentials = Credentials {
            password: "pw".into(),
            key_passphrase: String::new(),
            known_hosts: self.known_hosts.path().join("known_hosts"),
            ask_prompts: Some(Arc::new(|q: ssh::PromptQuestion| {
                let _ = q.reply.send(Some(vec!["pw".into()]));
            })),
            jumps: profiles::jump_chain(target, saved).unwrap(),
        };
        let sink = Arc::new(TunnelSink { recorder: Recorder::new(), tunnels: Mutex::new(Vec::new()) });
        let link = ssh::start_with_access(
            self.runtime.handle(),
            target.clone(),
            credentials,
            (80, 24),
            ask,
            sink.clone() as Arc<dyn Sink>,
            access,
        );
        sink.recorder.wait_event(WAIT, |e| *e == Event::Connected);
        Run { sink, link }
    }
}

fn profile(name: &str, port: u16, tunnels: Vec<Tunnel>) -> Profile {
    Profile {
        name: name.into(),
        host: "127.0.0.1".into(),
        port,
        username: "netops".into(),
        auth: "password".into(),
        tunnels,
        ..Profile::default()
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn rule(kind: TunnelKind, listen_port: u16, port: u16) -> Tunnel {
    Tunnel { kind, listen_port, host: "127.0.0.1".into(), port, ..Tunnel::default() }
}

/// Send `text` and read the same number of bytes back.
fn round_trip(stream: &mut TcpStream, text: &[u8]) -> Vec<u8> {
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.write_all(text).unwrap();
    let mut back = vec![0; text.len()];
    stream.read_exact(&mut back).unwrap();
    back
}

/// This computer's address on its network, if it has one: what another
/// computer would connect to.
fn lan_address() -> Option<std::net::IpAddr> {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // No packet is sent; this only picks the outgoing interface.
    probe.connect("192.0.2.1:9").ok()?;
    Some(probe.local_addr().ok()?.ip()).filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
}

#[test]
fn local_forward_reaches_an_echo_server_through_the_server_on_loopback_only() {
    let harness = Harness::new();
    let (ssh_port, requests) = harness.server();
    let echo = harness.echo();
    let listen = free_port();
    let run = harness.connect(
        &profile("router", ssh_port, vec![rule(TunnelKind::Local, listen, echo)]),
        &[],
        NetworkAccess::default(),
    );
    run.wait_listening(1);

    let mut stream = TcpStream::connect(("127.0.0.1", listen)).unwrap();
    assert_eq!(round_trip(&mut stream, b"show ip route"), b"show ip route");
    let tunnels = run.wait_tunnels(|t| t[0].connections == 1);
    assert_eq!(tunnels[0].label, format!("L {listen} → 127.0.0.1:{echo}"));
    assert_eq!(requests.lock().as_slice(), [("direct", "127.0.0.1".to_string(), u32::from(echo))]);
    drop(stream);
    run.wait_tunnels(|t| t[0].connections == 0);

    // The default listens on 127.0.0.1 only, so other computers can't reach it.
    if let Some(lan) = lan_address() {
        assert!(TcpStream::connect_timeout(&(lan, listen).into(), Duration::from_secs(2)).is_err());
    }

    // The tunnel stops with the session.
    run.link.send(Command::Close);
    run.sink.recorder.wait_event(WAIT, |e| *e == Event::Closed(None));
    let deadline = Instant::now() + WAIT;
    while TcpStream::connect(("127.0.0.1", listen)).is_ok() {
        assert!(Instant::now() < deadline, "the tunnel kept listening after the session closed");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// SOCKS5 greeting and CONNECT, returning the reply code.
fn socks_connect(stream: &mut TcpStream, address: &[u8], port: u16) -> u8 {
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.write_all(&[5, 1, 0]).unwrap();
    let mut choice = [0; 2];
    stream.read_exact(&mut choice).unwrap();
    assert_eq!(choice, [5, 0]);
    let mut request = vec![5, 1, 0];
    request.extend_from_slice(address);
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).unwrap();
    let mut reply = [0; 10];
    stream.read_exact(&mut reply).unwrap();
    reply[1]
}

#[test]
fn dynamic_forward_is_a_socks5_proxy_for_ip_and_name_destinations() {
    let harness = Harness::new();
    let (ssh_port, requests) = harness.server();
    let echo = harness.echo();
    let listen = free_port();
    let mut dynamic = rule(TunnelKind::Dynamic, listen, 0);
    dynamic.host.clear();
    let run = harness.connect(&profile("router", ssh_port, vec![dynamic]), &[], NetworkAccess::default());
    run.wait_listening(1);

    let mut by_ip = TcpStream::connect(("127.0.0.1", listen)).unwrap();
    assert_eq!(socks_connect(&mut by_ip, &[1, 127, 0, 0, 1], echo), 0);
    assert_eq!(round_trip(&mut by_ip, b"ping"), b"ping");

    let mut by_name = TcpStream::connect(("127.0.0.1", listen)).unwrap();
    let name = b"localhost";
    let address: Vec<u8> = [3, name.len() as u8].into_iter().chain(name.iter().copied()).collect();
    assert_eq!(socks_connect(&mut by_name, &address, echo), 0);
    assert_eq!(round_trip(&mut by_name, b"pong"), b"pong");
    run.wait_tunnels(|t| t[0].connections == 2 && t[0].label == format!("D {listen} (SOCKS)"));

    // Somewhere the server can't reach gets a SOCKS failure, not a hang.
    let mut refused = TcpStream::connect(("127.0.0.1", listen)).unwrap();
    assert_eq!(socks_connect(&mut refused, &[1, 127, 0, 0, 1], free_port()), 5);
    run.wait_tunnels(|t| t[0].last_error.contains("the server could not open 127.0.0.1:"));

    // Refusals get their reply and then a clean close, not a reset.
    let refusal = |request: &[u8]| {
        let mut stream = TcpStream::connect(("127.0.0.1", listen)).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.write_all(request).unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).expect("closed cleanly");
        reply
    };
    // BIND, then a name that isn't UTF-8.
    assert_eq!(refusal(&[5, 1, 0, 5, 2, 0, 1, 127, 0, 0, 1, 0, 22])[..4], [5, 0, 5, 7]);
    assert_eq!(refusal(&[5, 1, 0, 5, 1, 0, 3, 2, 0xff, 0xfe, 0, 80])[..4], [5, 0, 5, 1]);

    let requests = requests.lock();
    assert_eq!(requests[0], ("direct", "127.0.0.1".to_string(), u32::from(echo)));
    assert_eq!(requests[1], ("direct", "localhost".to_string(), u32::from(echo)));
}

#[test]
fn remote_forward_delivers_to_a_local_echo_server_through_offline_mode() {
    let harness = Harness::new();
    let (ssh_port, requests) = harness.server();
    let echo = harness.echo();
    let (server_port, public_port) = (free_port(), free_port());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    // Offline mode: a local destination needs no approval; this one is denied.
    let access = NetworkAccess::new(
        true,
        Arc::new(move |q: snekkie::network::NetworkQuestion| {
            seen.lock().push(q.host.clone());
            let _ = q.reply.send(false);
        }),
    );
    let mut public = rule(TunnelKind::Remote, public_port, 80);
    public.host = "192.0.2.1".into();
    let run = harness.connect(
        &profile("router", ssh_port, vec![rule(TunnelKind::Remote, server_port, echo), public]),
        &[],
        access,
    );
    run.wait_listening(2);
    assert!(requests.lock().contains(&("forward", "127.0.0.1".to_string(), u32::from(server_port))));

    // A connection to the server's port comes back to the echo server here.
    let mut stream = TcpStream::connect(("127.0.0.1", server_port)).unwrap();
    assert_eq!(round_trip(&mut stream, b"hello from the far side"), b"hello from the far side");
    run.wait_tunnels(|t| t[0].connections == 1);
    assert!(asked.lock().is_empty());

    let mut denied = TcpStream::connect(("127.0.0.1", public_port)).unwrap();
    denied.set_read_timeout(Some(WAIT)).unwrap();
    assert_eq!(denied.read(&mut [0; 1]).unwrap_or(0), 0);
    run.wait_tunnels(|t| t[1].last_error.contains("canceled in offline mode"));
    assert_eq!(asked.lock().as_slice(), ["192.0.2.1"]);
}

#[test]
fn failing_rules_are_named_and_the_shell_and_other_rules_keep_working() {
    let harness = Harness::new();
    let (ssh_port, _) = harness.server();
    let echo = harness.echo();
    // Taken here and on the "server" alike, since both are this computer.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = taken.local_addr().unwrap().port();
    let good = free_port();
    let run = harness.connect(
        &profile(
            "router",
            ssh_port,
            vec![
                rule(TunnelKind::Local, busy, echo),
                rule(TunnelKind::Remote, busy, echo),
                rule(TunnelKind::Local, 0, echo),
                Tunnel { enabled: false, ..rule(TunnelKind::Local, free_port(), echo) },
                rule(TunnelKind::Local, good, echo),
            ],
        ),
        &[],
        NetworkAccess::default(),
    );
    let tunnels = run.wait_tunnels(|t| t.len() == 4 && t.iter().all(|t| t.state != TunnelState::Starting));
    assert!(
        matches!(&tunnels[0].state, TunnelState::Failed(e) if e.contains(&format!("could not listen on 127.0.0.1:{busy}")))
    );
    assert!(
        matches!(&tunnels[1].state, TunnelState::Failed(e) if e.contains(&format!("the server refused to forward its port {busy}")))
    );
    assert!(matches!(&tunnels[2].state, TunnelState::Failed(e) if e == "choose a listen port"));
    assert_eq!(tunnels[3].state, TunnelState::Listening);

    let notices: Vec<String> = run
        .sink
        .recorder
        .events()
        .into_iter()
        .filter_map(|e| if let Event::Notice(n) = e { Some(n) } else { None })
        .collect();
    assert!(
        notices.iter().any(|n| n.starts_with(&format!("Tunnel L {busy} → 127.0.0.1:{echo}: could not listen"))),
        "{notices:?}"
    );
    assert!(
        notices.iter().any(|n| n.starts_with(&format!("Tunnel R {busy} → 127.0.0.1:{echo}: the server refused"))),
        "{notices:?}"
    );

    run.link.write(b"still here\r".to_vec());
    run.sink.recorder.wait_text(WAIT, "still here");
    let mut stream = TcpStream::connect(("127.0.0.1", good)).unwrap();
    assert_eq!(round_trip(&mut stream, b"ok"), b"ok");
    assert!(!run.sink.recorder.events().iter().any(|e| matches!(e, Event::Closed(_))));
}

#[test]
fn a_jump_hosted_tab_forwards_over_the_final_target() {
    let harness = Harness::new();
    let (edge_port, edge_requests) = harness.server();
    let (router_port, router_requests) = harness.server();
    let echo = harness.echo();
    let listen = free_port();
    let edge = profile("edge", edge_port, Vec::new());
    let router = Profile {
        jump_host: "edge".into(),
        ..profile("router", router_port, vec![rule(TunnelKind::Local, listen, echo)])
    };
    let run = harness.connect(&router, &[edge], NetworkAccess::default());
    run.wait_listening(1);

    let mut stream = TcpStream::connect(("127.0.0.1", listen)).unwrap();
    assert_eq!(round_trip(&mut stream, b"through the bastion"), b"through the bastion");
    // The hop only carried SSH to the router; the router opened the forward.
    assert_eq!(edge_requests.lock().as_slice(), [("direct", "127.0.0.1".to_string(), u32::from(router_port))]);
    assert_eq!(router_requests.lock().as_slice(), [("direct", "127.0.0.1".to_string(), u32::from(echo))]);
}

#[test]
fn a_session_lists_its_tunnels_while_connected_and_clears_them_when_closed() {
    use snekkie::session::{ConnectContext, Session};
    let harness = Harness::new();
    use_test_known_hosts();
    let (ssh_port, _) = harness.server();
    let echo = harness.echo();
    let listen = free_port();
    let mut session = Session::new(
        profile("router", ssh_port, vec![rule(TunnelKind::Local, listen, echo)]),
        snekkie::settings::Theme::default(),
        || {},
    );
    session.connect(ConnectContext {
        runtime: harness.runtime.handle(),
        ask_host_key: Arc::new(|q: HostKeyQuestion| {
            let _ = q.reply.send(true);
        }),
        ask_prompts: None,
        jumps: Vec::new(),
        password: "pw".into(),
        key_passphrase: String::new(),
        network: NetworkAccess::default(),
    });
    let deadline = Instant::now() + WAIT;
    while session.tunnels().first().is_none_or(|t| t.state != TunnelState::Listening) {
        assert!(Instant::now() < deadline, "{:?}", session.tunnels());
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut stream = TcpStream::connect(("127.0.0.1", listen)).unwrap();
    assert_eq!(round_trip(&mut stream, b"ok"), b"ok");
    session.shutdown();
    while !session.tunnels().is_empty() || TcpStream::connect(("127.0.0.1", listen)).is_ok() {
        assert!(Instant::now() < deadline, "{:?}", session.tunnels());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The app-level tests' known_hosts: one per test process, since the first
/// call wins for the whole process.
fn use_test_known_hosts() {
    ssh::use_known_hosts_for_tests(std::env::temp_dir().join(format!("snekkie-tunnels-test-{}", std::process::id())));
}

#[test]
fn the_app_opens_a_jump_hosted_tab_whose_tunnel_works() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use snekkie::ui::{Paths, SnekkieApp};

    let harness = Harness::new();
    use_test_known_hosts();
    let (edge_port, edge_requests) = harness.server();
    let (router_port, _) = harness.server();
    let echo = harness.echo();
    let listen = free_port();
    let edge = profile("edge", edge_port, Vec::new());
    let router = Profile {
        jump_host: "edge".into(),
        ..profile("router", router_port, vec![rule(TunnelKind::Local, listen, echo)])
    };
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
    let mut store = profiles::ProfileStore::open(paths.sessions.clone());
    store.profiles = vec![edge, router.clone()];
    store.save().unwrap();
    let mut app = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1200.0, 760.0))
        .with_step_dt(0.05)
        .build_eframe(move |_cc| SnekkieApp::new(paths));
    app.run_steps(2);
    let ctx = app.ctx.clone();
    app.state_mut().open_session(&ctx, router);

    // The router's password, then the edge's own password and host key, then the router's host key.
    let (mut passwords, mut host_keys) = (0, 0);
    let deadline = Instant::now() + WAIT;
    while !app.state().active_screen_text().is_some_and(|text| text.contains("router#")) {
        assert!(Instant::now() < deadline, "no shell; dialogs {:?}", app.state().dialog_texts());
        if let Some(field) = app.query_by_role(Role::PasswordInput) {
            field.focus();
            field.type_text("pw");
            app.run_steps(2);
            app.get_by_label("OK").click();
            passwords += 1;
        } else if let Some(accept) = app.query_by_label("Accept") {
            accept.click();
            host_keys += 1;
        }
        std::thread::sleep(Duration::from_millis(15));
        app.run_steps(4);
    }
    assert_eq!((passwords, host_keys), (2, 2), "one password and host key per hop");
    assert!(app.state().tab_titles().iter().any(|t| t.contains("router")), "{:?}", app.state().tab_titles());

    // The tunnel listens on loopback and goes over the router, not the edge.
    while TcpStream::connect(("127.0.0.1", listen)).is_err() {
        assert!(Instant::now() < deadline, "the tunnel never listened");
        std::thread::sleep(Duration::from_millis(20));
        app.run_steps(2);
    }
    let mut stream = TcpStream::connect(("127.0.0.1", listen)).unwrap();
    assert_eq!(round_trip(&mut stream, b"via the app"), b"via the app");
    assert_eq!(edge_requests.lock().as_slice(), [("direct", "127.0.0.1".to_string(), u32::from(router_port))]);
    app.run_steps(2);
    app.get_by_label_contains("Tunnels 1/1");
}
