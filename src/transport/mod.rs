//! Transport layer.
//!
//! Everything in here reduces a connection to the same thing: a
//! bidirectional stream of bytes. Each transport runs its own worker (a
//! thread for serial, a task on the async runtime for the network ones),
//! takes [`Command`]s from the session, and reports back through a [`Sink`].
//! Adding a transport means writing one more worker; nothing else in the
//! app needs to know how it works.

pub mod serial;
pub mod sftp;
pub mod ssh;
pub mod tcp;
pub mod telnet;
pub mod transfer;
pub mod tunnel;

use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// How long a TCP connection (and, for SSH, the handshake) gets before
/// giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);

/// The terminal type reported to the far end.
pub const TERM: &str = "xterm-256color";

/// Open a TCP connection, giving up after [`CONNECT_TIMEOUT`].
pub async fn connect_tcp(host: &str, port: u16, access: &crate::network::NetworkAccess) -> Result<TcpStream, String> {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    let addresses = resolve(host, port, access).await?;
    let mut last_error = "no addresses were found".to_string();
    for address in addresses {
        access.approve_address(host, address).await?;
        let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                last_error = e.to_string();
                continue;
            }
            Err(_) => {
                last_error = "timed out".into();
                continue;
            }
        };
        // Without this, Nagle's algorithm sits on the one-byte packets an
        // interactive session sends per keystroke: the classic "typing feels
        // laggy" complaint.
        let _ = stream.set_nodelay(true);
        return Ok(stream);
    }
    Err(format!("Could not connect to {host}: {last_error}"))
}

/// The addresses to try for `host`, asking first in offline mode before any DNS lookup.
async fn resolve(
    host: &str,
    port: u16,
    access: &crate::network::NetworkAccess,
) -> Result<Vec<std::net::SocketAddr>, String> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(vec![(ip, port).into()]);
    }
    if host.eq_ignore_ascii_case("localhost") || host.eq_ignore_ascii_case("localhost.") {
        return Ok(vec![(std::net::Ipv4Addr::LOCALHOST, port).into(), (std::net::Ipv6Addr::LOCALHOST, port).into()]);
    }
    access.approve_lookup(host, port).await?;
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::lookup_host((host, port))).await {
        Ok(Ok(addresses)) => Ok(addresses.collect()),
        Ok(Err(e)) => Err(format!("Could not resolve {host}: {e}")),
        Err(_) => Err(format!("Could not resolve {host}: timed out")),
    }
}

/// A private, loopback or link-local IP address (or localhost), which
/// offline mode lets Snekkie connect to without asking.
pub fn is_local_destination(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    let host = host.split('%').next().unwrap_or(host);
    host.eq_ignore_ascii_case("localhost") || host.parse().is_ok_and(crate::network::is_local)
}

/// What one TCP connect to a port found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortCheck {
    Open(Duration),
    Refused,
    TimedOut,
    Failed(String),
}

impl PortCheck {
    pub fn describe(&self, host: &str, port: u16) -> String {
        match self {
            PortCheck::Open(took) => format!("{host}:{port} is open ({} ms)", took.as_millis()),
            PortCheck::Refused => format!("{host}:{port} refused the connection"),
            PortCheck::TimedOut => format!("{host}:{port} timed out"),
            PortCheck::Failed(error) => format!("{host}:{port}: {error}"),
        }
    }
}

/// One TCP connect, closed straight away. Waiting for offline-mode approval
/// doesn't count towards `timeout`.
pub async fn check_port(host: &str, port: u16, timeout: Duration, access: &crate::network::NetworkAccess) -> PortCheck {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    // shortcut: only the first resolved address is tried; try each if dual-stack hosts mislead.
    let address = match resolve(host, port, access).await.map(|a| a.first().copied()) {
        Ok(Some(address)) => address,
        Ok(None) => return PortCheck::Failed("no addresses were found".into()),
        Err(error) => return PortCheck::Failed(error),
    };
    if let Err(error) = access.approve_address(host, address).await {
        return PortCheck::Failed(error);
    }
    let started = std::time::Instant::now();
    match tokio::time::timeout(timeout, TcpStream::connect(address)).await {
        Ok(Ok(_)) => PortCheck::Open(started.elapsed()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => PortCheck::Refused,
        Ok(Err(e)) => PortCheck::Failed(e.to_string()),
        Err(_) => PortCheck::TimedOut,
    }
}

/// Longest pause between checks while waiting for a host to come back.
const WAIT_MAX_DELAY: Duration = Duration::from_secs(30);

/// Check the port until it accepts a connection, backing off from
/// `first_delay` to 30 seconds. False after `give_up` without success.
pub async fn wait_for_port(
    host: &str,
    port: u16,
    access: &crate::network::NetworkAccess,
    first_delay: Duration,
    give_up: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + give_up;
    let mut delay = first_delay;
    loop {
        if let PortCheck::Open(_) = check_port(host, port, Duration::from_secs(3), access).await {
            return true;
        }
        if tokio::time::Instant::now() + delay > deadline {
            return false;
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(WAIT_MAX_DELAY);
    }
}

/// The keepalive interval a profile asks for, if any.
pub fn keepalive_interval(seconds: u32) -> Option<Duration> {
    (seconds > 0).then(|| Duration::from_secs(seconds.into()))
}

/// What the session asks of a running transport.
#[derive(Debug)]
pub enum Command {
    Write(Vec<u8>),
    /// Tell the far end the window changed size. Meaningful for SSH and
    /// telnet, ignored by the others.
    Resize {
        columns: u16,
        lines: u16,
    },
    /// Send a line break. Cisco password recovery lives here. Telnet
    /// passes it on as a telnet BREAK, which console servers turn into a
    /// real one on the device's line.
    Break,
    /// Move a file beside the shell. SSH only.
    Transfer(transfer::Request),
    /// Hand over the logged-in connection for a file explorer. SSH only.
    ShareConnection(tokio::sync::oneshot::Sender<ssh::Connection>),
    Close,
}

/// Where a transport reports what happens. Called from the worker, never
/// from the UI thread.
pub trait Sink: Send + Sync + 'static {
    fn connected(&self);
    fn data(&self, bytes: &[u8]);
    /// The connection is over. `None` means we closed it on purpose.
    fn closed(&self, reason: Option<String>);
    /// A message worth showing the user without ending the session.
    fn notice(&self, message: String);
    /// Whether the far end has agreed to echo what's typed. Only telnet
    /// negotiates this.
    fn remote_echo(&self, _on: bool) {}
    /// The SSH port forwards and what each is doing, whenever that changes.
    fn tunnels(&self, _status: Vec<tunnel::TunnelStatus>) {}
}

/// The session's handle on a running transport.
#[derive(Clone)]
pub struct Link {
    commands: UnboundedSender<Command>,
}

impl Link {
    pub fn new() -> (Link, UnboundedReceiver<Command>) {
        let (tx, rx) = unbounded_channel();
        (Link { commands: tx }, rx)
    }

    pub fn send(&self, command: Command) {
        // A closed channel means the worker already finished; its Sink has
        // reported why, so there's nothing more to do here.
        let _ = self.commands.send(command);
    }

    pub fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.send(Command::Write(bytes));
        }
    }

    pub fn try_write(&self, bytes: Vec<u8>) -> bool {
        bytes.is_empty() || self.commands.send(Command::Write(bytes)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::NetworkAccess;
    use std::sync::Arc;

    fn closed_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    #[test]
    fn port_checks_report_open_refused_and_offline_denials() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let access = NetworkAccess::default();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listener.local_addr().unwrap().port();
        runtime.block_on(async {
            let timeout = Duration::from_secs(3);
            assert!(matches!(check_port("127.0.0.1", open, timeout, &access).await, PortCheck::Open(_)));
            assert_eq!(check_port("127.0.0.1", closed_port(), timeout, &access).await, PortCheck::Refused);
            // Offline with nobody to approve: refused before any traffic.
            let offline = NetworkAccess::new(true, Arc::new(|_| {}));
            match check_port("192.0.2.1", 22, timeout, &offline).await {
                PortCheck::Failed(error) => assert!(error.contains("canceled"), "{error}"),
                other => panic!("expected a denial, got {other:?}"),
            }
        });
        assert_eq!(PortCheck::Open(Duration::from_millis(12)).describe("r1", 22), "r1:22 is open (12 ms)");
    }

    #[test]
    fn waiting_for_a_port_returns_once_it_opens_or_gives_up() {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let access = NetworkAccess::default();
        let port = closed_port();
        let first = Duration::from_millis(50);
        assert!(!runtime.block_on(wait_for_port("127.0.0.1", port, &access, first, Duration::from_millis(10))));
        let opener = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
            std::thread::sleep(Duration::from_secs(10));
            drop(listener);
        });
        assert!(runtime.block_on(wait_for_port("127.0.0.1", port, &access, first, Duration::from_secs(9))));
        drop(opener);
    }

    #[test]
    fn local_destinations_are_private_ip_literals_or_localhost() {
        for local in ["192.168.1.1", " 10.0.0.1 ", "[fe80::1%eth0]", "::1", "localhost"] {
            assert!(is_local_destination(local), "{local}");
        }
        for remote in ["8.8.8.8", "router.lab", "192.168.1.1.example", ""] {
            assert!(!is_local_destination(remote), "{remote}");
        }
    }
}
