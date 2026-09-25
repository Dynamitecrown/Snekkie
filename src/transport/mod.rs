//! Transport layer.
//!
//! Everything in here reduces a connection to the same thing: a
//! bidirectional stream of bytes. Each transport runs its own worker (a
//! thread for serial, a task on the async runtime for SSH), takes
//! [`Command`]s from the session, and reports back through a [`Sink`].
//! Adding a transport (telnet, raw TCP...) means writing one more worker;
//! nothing else in the app needs to know how it works.

pub mod serial;
pub mod ssh;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// What the session asks of a running transport.
#[derive(Debug)]
pub enum Command {
    Write(Vec<u8>),
    /// Tell the far end the window changed size. Meaningful for SSH,
    /// ignored by serial.
    Resize {
        columns: u16,
        lines: u16,
    },
    /// Send a line break. Cisco password recovery lives here.
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
