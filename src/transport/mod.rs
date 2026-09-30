//! Transport layer.
//!
//! Everything in here reduces a connection to the same thing: a
//! bidirectional stream of bytes. Each transport runs its own worker (a
//! thread for serial, a task on the async runtime for the network ones),
//! takes [`Command`]s from the session, and reports back through a [`Sink`].
//! Adding a transport means writing one more worker; nothing else in the
//! app needs to know how it works.

pub mod serial;
pub mod ssh;
pub mod tcp;
pub mod telnet;

use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// How long a TCP connection (and, for SSH, the handshake) gets before
/// giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);

/// The terminal type reported to the far end.
pub const TERM: &str = "xterm-256color";

/// Open a TCP connection, giving up after [`CONNECT_TIMEOUT`].
pub async fn connect_tcp(host: &str, port: u16) -> Result<TcpStream, String> {
    let target = (host.trim_start_matches('[').trim_end_matches(']'), port);
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(target)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => return Err(format!("Could not connect to {host}: {e}")),
        Err(_) => return Err(format!("Could not connect to {host}: timed out")),
    };
    // Without this, Nagle's algorithm sits on the one-byte packets an
    // interactive session sends per keystroke: the classic "typing feels
    // laggy" complaint.
    let _ = stream.set_nodelay(true);
    Ok(stream)
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
}
