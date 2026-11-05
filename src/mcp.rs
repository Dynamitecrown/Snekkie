//! Lets AI agents on this computer use open sessions, over the Model
//! Context Protocol.
//!
//! Off by default. When on, the app listens on a random loopback port and
//! writes the port and a fresh token to `mcp.json` in the config directory,
//! which only the user's own programs can read. An agent starts
//! `snekkie --mcp`, which speaks MCP over stdio and forwards each tool call
//! to the running app. The bridge answers the handshake itself, so an agent
//! started before Snekkie (or before access is turned on) still connects and
//! is told why its calls fail. It also holds one connection open for as long
//! as the agent runs, so the app can show who is connected, and every call
//! names its agent so the app can show who is reading or typing where.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::session::{Remote, Session, State};
use crate::terminal::emulator::Emulator;

mod files;

pub use files::ExplorerEntry;

pub const FILE_NAME: &str = "mcp.json";

/// Newest first; a client asking for another gets the newest.
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_REQUEST: u64 = 1024 * 1024;
const REQUEST_WAIT: Duration = Duration::from_secs(10);
const MAX_COMMAND_SECONDS: u64 = 120;
const MAX_TRANSFER_SECONDS: u64 = 6 * 60 * 60;
/// Output kept for one command; a flood beyond this is counted, not stored.
const MAX_CAPTURE: usize = 4 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(50);
/// Output quiet this long at something that looks like a prompt is done...
const SETTLE: Duration = Duration::from_millis(500);
/// ...and quiet this long anywhere, so a device that pauses mid-output
/// (`Building configuration...`) is waited for.
const QUIET: Duration = Duration::from_secs(3);
/// How long cues stay after an agent stops reading or typing.
const READ_LINGER: Duration = Duration::from_millis(1500);
const WRITE_LINGER: Duration = Duration::from_millis(900);
/// How often a bridge looks for the app again to say it's connected.
const PRESENCE_RETRY: Duration = Duration::from_secs(2);

const NOT_RUNNING: &str = "Snekkie is not running, or AI agent access is off. Ask the user to open Snekkie and \
                           turn on Preferences → Network → AI agent access.";

const INSTRUCTIONS: &str = "Snekkie is the user's terminal for network devices and servers. Its sessions are live \
     SSH, Telnet, raw TCP and serial connections the user opened. Anything you send runs on real equipment exactly \
     as if the user typed it, and is recorded in the session log when logging is on. Call list_sessions first, then \
     send_command one line at a time and read what comes back. If the user allows file access, the file tools work \
     on SSH sessions and file explorer tabs over SFTP.";

/// What agents are doing, for the app to show.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    /// One name per connected agent.
    pub agents: Vec<String>,
    /// By session ID.
    pub sessions: HashMap<u64, Use>,
}

/// An agent's latest reading and typing in one session.
#[derive(Clone, Debug, Default)]
pub struct Use {
    pub agent: String,
    reads: u32,
    read_end: Option<Instant>,
    wrote: Option<Instant>,
    /// The latest read or write, to pick the busiest session.
    pub last: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cue {
    Reading,
    /// Seconds since the agent typed, so each command pulses once.
    Writing(f32),
}

impl Use {
    /// What to show at `now`. Typing outranks the reading that follows a command.
    pub fn cue(&self, now: Instant) -> Option<Cue> {
        if let Some(wrote) = self.wrote.filter(|wrote| now < *wrote + WRITE_LINGER) {
            return Some(Cue::Writing(now.saturating_duration_since(wrote).as_secs_f32()));
        }
        (self.reads > 0 || self.read_end.is_some_and(|end| now < end + READ_LINGER)).then_some(Cue::Reading)
    }
}

#[derive(Default)]
struct Listing {
    selected: Option<u64>,
    sessions: Vec<Remote>,
    explorers: Vec<ExplorerEntry>,
    activity: HashMap<u64, Use>,
    agents: Vec<(u64, String)>,
    next_agent: u64,
}

struct Inner {
    listing: Mutex<Listing>,
    files_allowed: AtomicBool,
    repaint: Box<dyn Fn() + Send + Sync>,
}

impl Inner {
    fn touch(&self, id: u64, agent: &str, change: impl FnOnce(&mut Use)) {
        {
            let mut listing = self.listing.lock();
            let used = listing.activity.entry(id).or_default();
            used.agent = agent.into();
            used.last = Some(Instant::now());
            change(used);
        }
        (self.repaint)();
    }

    fn wrote(&self, id: u64, agent: &str) {
        self.touch(id, agent, |used| used.wrote = Some(Instant::now()));
    }

    /// The session shows as read until the guard drops, even if the call is cut short.
    fn reading<'a>(&'a self, id: u64, agent: &'a str) -> Reading<'a> {
        self.touch(id, agent, |used| used.reads += 1);
        Reading { inner: self, id, agent }
    }

    fn join(&self, agent: &str) -> u64 {
        let id = {
            let mut listing = self.listing.lock();
            listing.next_agent += 1;
            let id = listing.next_agent;
            listing.agents.push((id, agent.into()));
            id
        };
        (self.repaint)();
        id
    }

    fn leave(&self, id: u64) {
        self.listing.lock().agents.retain(|(agent, _)| *agent != id);
        (self.repaint)();
    }
}

struct Reading<'a> {
    inner: &'a Inner,
    id: u64,
    agent: &'a str,
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        self.inner.touch(self.id, self.agent, |used| {
            used.reads = used.reads.saturating_sub(1);
            used.read_end = Some(Instant::now());
        });
    }
}

/// The app's end: listens while it exists, and forgets its sessions, its
/// agents and its `mcp.json` when dropped.
pub struct Server {
    inner: Arc<Inner>,
    task: tokio::task::JoinHandle<()>,
    file: PathBuf,
    token: Arc<str>,
}

impl Server {
    /// `repaint` is called from other threads whenever an agent's activity changes.
    pub fn start(
        runtime: &tokio::runtime::Handle,
        file: PathBuf,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Server> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let token: Arc<str> = new_token()?.into();
        crate::config::write_private(&file, &json!({ "port": port, "token": &*token }).to_string())?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener)?
        };
        let inner = Arc::new(Inner {
            listing: Mutex::default(),
            files_allowed: AtomicBool::new(false),
            repaint: Box::new(repaint),
        });
        let task = runtime.spawn(serve(listener, token.clone(), inner.clone()));
        Ok(Server { inner, task, file, token })
    }

    /// The sessions agents can see, in tab order; cheap when nothing changed.
    pub fn sync(&self, selected: Option<u64>, sessions: &[&Session]) {
        let mut listing = self.inner.listing.lock();
        let listing = &mut *listing;
        listing.selected = selected;
        let same = listing.sessions.len() == sessions.len()
            && listing.sessions.iter().zip(sessions).all(|(remote, session)| remote.id == session.id);
        if !same {
            listing.sessions = sessions.iter().map(|session| session.remote()).collect();
            let (open, explorers) = (&listing.sessions, &listing.explorers);
            listing.activity.retain(|id, _| {
                open.iter().any(|session| session.id == *id) || explorers.iter().any(|tab| tab.id == *id)
            });
        }
    }

    /// Whether agents may use the file tools, and the file explorers they can name.
    pub fn sync_files(&self, allowed: bool, explorers: impl Iterator<Item = ExplorerEntry>) {
        self.inner.files_allowed.store(allowed, Ordering::Relaxed);
        self.inner.listing.lock().explorers = explorers.collect();
    }

    pub fn activity(&self) -> Activity {
        let listing = self.inner.listing.lock();
        Activity {
            agents: listing.agents.iter().map(|(_, name)| name.clone()).collect(),
            sessions: listing.activity.clone(),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        *self.inner.listing.lock() = Listing::default();
        // Another copy of Snekkie may have started its own server since.
        if std::fs::read_to_string(&self.file).is_ok_and(|text| text.contains(&*self.token)) {
            let _ = std::fs::remove_file(&self.file);
        }
    }
}

async fn serve(listener: tokio::net::TcpListener, token: Arc<str>, inner: Arc<Inner>) {
    // Owning the connections means stopping the server ends every one of them.
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(answer(stream, token.clone(), inner.clone()));
                }
                // Out of sockets or similar: try again shortly rather than spin.
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

/// One tool call per connection: the token, the call, then the reply. Or
/// the token and a presence, held until the agent's bridge exits.
async fn answer(stream: tokio::net::TcpStream, token: Arc<str>, inner: Arc<Inner>) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read.take(MAX_REQUEST));
    let (mut key, mut request) = (String::new(), String::new());
    let received = tokio::time::timeout(REQUEST_WAIT, async {
        reader.read_line(&mut key).await?;
        reader.read_line(&mut request).await
    })
    .await;
    if !matches!(received, Ok(Ok(_))) || !same(key.trim_end().as_bytes(), token.as_bytes()) {
        return;
    }
    let request: Value = serde_json::from_str(&request).unwrap_or_default();
    let agent = request["agent"].as_str().unwrap_or("An agent");
    if request["presence"] == true {
        let id = inner.join(agent);
        let mut buf = [0u8; 64];
        while reader.read(&mut buf).await.is_ok_and(|n| n > 0) {}
        inner.leave(id);
        return;
    }
    let tool = request["tool"].as_str().unwrap_or_default();
    let (text, is_error) = match call(&inner, agent, tool, &request["arguments"]).await {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    let mut reply = json!({ "text": text, "isError": is_error }).to_string();
    reply.push('\n');
    let _ = write.write_all(reply.as_bytes()).await;
}

async fn call(inner: &Inner, agent: &str, tool: &str, arguments: &Value) -> Result<String, String> {
    let listing = &inner.listing;
    match tool {
        "list_sessions" => Ok(list(&listing.lock())),
        "read_output" => {
            let session = find(listing, arguments)?;
            let _reading = inner.reading(session.id, agent);
            let lines = arguments["lines"].as_u64().unwrap_or(100).clamp(1, 5000) as usize;
            let text = session.text();
            let all: Vec<&str> = text.lines().collect();
            Ok(all[all.len().saturating_sub(lines)..].join("\n"))
        }
        "send_command" => send_command(inner, agent, &find(listing, arguments)?, arguments).await,
        tool if files::handles(tool) => files::call(inner, agent, tool, arguments).await,
        "send_keys" => {
            let session = find(listing, arguments)?;
            let text = arguments["text"].as_str().filter(|text| !text.is_empty()).ok_or("text is required.")?;
            if !session.write(text.as_bytes().to_vec()) {
                return Err(not_connected(&session));
            }
            inner.wrote(session.id, agent);
            Ok(format!("Sent {} bytes to {}.", text.len(), session.name))
        }
        _ => Err(format!("Unknown tool: {tool}")),
    }
}

fn list(listing: &Listing) -> String {
    if listing.sessions.is_empty() && listing.explorers.is_empty() {
        return "No sessions are open in Snekkie. Ask the user to open one.".into();
    }
    let mut sessions: Vec<Value> = listing
        .sessions
        .iter()
        .map(|session| {
            json!({
                "session_id": session.id,
                "name": session.name,
                "type": session.kind().label(),
                "destination": session.description,
                "state": state_text(&session.state()),
                "selected": listing.selected == Some(session.id),
            })
        })
        .collect();
    sessions.extend(listing.explorers.iter().map(|tab| {
        json!({
            "session_id": tab.id,
            "name": tab.name,
            "type": "File explorer",
            "destination": tab.description,
            "state": if tab.link.is_some() { "connected" } else { "disconnected" },
            "selected": false,
        })
    }));
    serde_json::to_string_pretty(&sessions).unwrap_or_default()
}

fn state_text(state: &State) -> String {
    match state {
        State::Connecting => "connecting".into(),
        State::Connected => "connected".into(),
        State::Closed(None) => "disconnected".into(),
        State::Closed(Some(reason)) => format!("disconnected: {reason}"),
    }
}

fn session_id(arguments: &Value) -> Result<u64, String> {
    let id = &arguments["session_id"];
    id.as_u64()
        .or_else(|| id.as_str()?.trim().parse().ok())
        .ok_or_else(|| "session_id is required; call list_sessions to find one.".to_string())
}

fn find(listing: &Mutex<Listing>, arguments: &Value) -> Result<Remote, String> {
    let id = session_id(arguments)?;
    let found = listing.lock().sessions.iter().find(|session| session.id == id).cloned();
    found.ok_or_else(|| format!("No open session has session_id {id}; call list_sessions."))
}

fn not_connected(session: &Remote) -> String {
    format!("{} is {}, so nothing was sent. Ask the user to reconnect it.", session.name, state_text(&session.state()))
}

async fn send_command(inner: &Inner, agent: &str, session: &Remote, arguments: &Value) -> Result<String, String> {
    let command = arguments["command"].as_str().ok_or("command is required.")?;
    if command.contains(['\r', '\n']) {
        return Err("Send one command line at a time.".into());
    }
    let wait = Duration::from_secs(arguments["timeout_seconds"].as_u64().unwrap_or(15).clamp(1, MAX_COMMAND_SECONDS));
    // Listen before sending, so not even the echo is missed.
    let tap = session.tap();
    if !session.write(format!("{command}\r").into_bytes()) {
        return Err(not_connected(session));
    }
    inner.wrote(session.id, agent);
    let _reading = inner.reading(session.id, agent);
    let start = Instant::now();
    let (mut output, mut received, mut changed) = (Vec::new(), 0, start);
    let finished = loop {
        tokio::time::sleep(POLL).await;
        let before = received;
        while let Ok(bytes) = tap.try_recv() {
            received += bytes.len();
            let room = MAX_CAPTURE.saturating_sub(output.len());
            output.extend_from_slice(&bytes[..bytes.len().min(room)]);
        }
        let now = Instant::now();
        if received != before {
            changed = now;
        }
        let quiet = now - changed;
        if received > 0 && (quiet >= QUIET || quiet >= SETTLE && waiting_for_input(&session.cursor_line())) {
            break true;
        }
        if now - start >= wait || session.state() != State::Connected {
            break false;
        }
    };
    drop(tap);
    let mut text = render(&output, session.columns());
    if received > output.len() {
        text.push_str(&format!("\n[Output cut off after {} MiB.]", MAX_CAPTURE >> 20));
    }
    if session.state() != State::Connected {
        text.push_str(&format!("\n[{} is now {}.]", session.name, state_text(&session.state())));
    } else if !finished {
        text.push_str(&format!(
            "\n[Still running after {} s. Use read_output to see more, or send_keys to answer or interrupt.]",
            wait.as_secs()
        ));
    }
    Ok(text)
}

/// A shell or device prompt, a question, or a pager waiting for a key.
fn waiting_for_input(line: &str) -> bool {
    let line = line.trim_end().to_ascii_lowercase();
    line.ends_with(['#', '>', '$', '%', ':', ']', '?', ')']) || line.contains("-more") || line.contains("(more")
}

/// What the output alone looks like on a terminal, without its escapes.
fn render(output: &[u8], columns: usize) -> String {
    // Nobody to answer the device's queries: the real session already has.
    let mut emulator = Emulator::new(columns, 24, 100_000, Arc::new(|_| {}));
    emulator.feed(output);
    emulator.all_text()
}

/// Compare without stopping at the first difference, so timing can't
/// reveal the token.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn new_token() -> io::Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 32];
    ring::rand::SystemRandom::new().fill(&mut bytes).map_err(|_| io::Error::other("no secure random numbers"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

// -- the agent's end ----------------------------------------------------

/// `snekkie --mcp`: an MCP server on stdio for an agent to start.
pub fn bridge(file: &Path) -> ExitCode {
    #[cfg(windows)]
    let tray = bridges::start();
    let code = serve_stdio(file);
    #[cfg(windows)]
    tray.remove();
    code
}

fn serve_stdio(file: &Path) -> ExitCode {
    let mut stdout = io::stdout().lock();
    let mut agent = None;
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => {
                if message["method"] == "initialize" && agent.is_none() {
                    let name = display_name(message["params"]["clientInfo"]["name"].as_str().unwrap_or_default());
                    let (file, present) = (file.to_path_buf(), name.clone());
                    // Ends with the process, when the agent closes stdin.
                    std::thread::spawn(move || stay_connected(&file, &present));
                    agent = Some(name);
                }
                let agent = agent.as_deref().unwrap_or("An agent");
                respond(&message, |tool, arguments| forward(file, agent, tool, arguments).unwrap_or_else(|e| (e, true)))
            }
            Err(_) => {
                Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }))
            }
        };
        if let Some(reply) = reply
            && writeln!(stdout, "{reply}").and_then(|()| stdout.flush()).is_err()
        {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// Bridges on Windows, which have no window: they hold named objects so the
/// app and each other can find them. Every bridge watches one event that
/// closes them all. While the app is closed, the bridge holding the tray lock
/// shows Snekkie's icon, so there is still exactly one.
#[cfg(windows)]
pub mod bridges {
    use std::sync::Arc;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, OpenEventW, OpenMutexW, ResetEvent,
        SYNCHRONIZATION_SYNCHRONIZE, SetEvent, WaitForSingleObject,
    };

    use crate::ui::tray::{Tray, TrayCommand, bridge_menu};

    /// Held by every running bridge.
    const RUNNING: &str = "Snekkie.Bridge.Running";
    /// Set to close every bridge.
    const QUIT: &str = "Snekkie.Bridge.Quit";
    /// Owned by the one bridge that shows the icon.
    const TRAY: &str = "Snekkie.Bridge.Tray";
    /// How quickly the icon moves between the app and a bridge.
    const CHECK: Duration = Duration::from_millis(500);

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn mutex_exists(name: &str) -> bool {
        let handle = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, wide(name).as_ptr()) };
        !handle.is_null() && unsafe { CloseHandle(handle) } != 0
    }

    fn signal(name: &str) {
        let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, wide(name).as_ptr()) };
        if !handle.is_null() {
            unsafe {
                SetEvent(handle);
                CloseHandle(handle);
            }
        }
    }

    /// Whether any agent's bridge is running.
    pub fn running() -> bool {
        mutex_exists(RUNNING)
    }

    /// Close every bridge. Their agents see Snekkie's tools stop.
    pub fn close() {
        signal(QUIT);
    }

    /// Announce this bridge and show the icon whenever it should be this
    /// bridge's. The returned tray is for taking the icon away at exit.
    pub(crate) fn start() -> Arc<Tray> {
        unsafe {
            // Deliberately never closed: Windows releases them when the process exits.
            CreateMutexW(std::ptr::null(), 0, wide(RUNNING).as_ptr());
            let quit = CreateEventW(std::ptr::null(), 1, 0, wide(QUIT).as_ptr());
            // Left set if the last close happened while another program held it open.
            ResetEvent(quit);
            let tray = Arc::new(Tray::new(false, bridge_menu, on_command));
            let shown = tray.clone();
            let quit = quit as isize;
            std::thread::spawn(move || watch(&shown, quit as _));
            tray
        }
    }

    fn on_command(command: TrayCommand) {
        match command {
            TrayCommand::OpenApp => {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).spawn();
                }
            }
            TrayCommand::CloseBridges => close(),
            _ => {}
        }
    }

    /// Until told to close: show the icon while this bridge holds the tray
    /// lock and the app isn't running.
    fn watch(tray: &Tray, quit: windows_sys::Win32::Foundation::HANDLE) {
        let lock = unsafe { CreateMutexW(std::ptr::null(), 0, wide(TRAY).as_ptr()) };
        let mut owner = false;
        loop {
            // Abandoned means the bridge that showed it exited: it's ours now.
            owner = owner || matches!(unsafe { WaitForSingleObject(lock, 0) }, WAIT_OBJECT_0 | WAIT_ABANDONED);
            tray.set_visible(owner && !mutex_exists(crate::APP_MUTEX));
            if unsafe { WaitForSingleObject(quit, CHECK.as_millis() as u32) } == WAIT_OBJECT_0 {
                tray.remove();
                std::process::exit(0);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Names of its own, so the user's real bridges are never touched.
        #[test]
        fn named_objects_are_found_and_signalled() {
            let mutex = format!("Snekkie.Test.{}.Mutex", std::process::id());
            let event = format!("Snekkie.Test.{}.Event", std::process::id());
            assert!(!mutex_exists(&mutex));
            let held = unsafe { CreateMutexW(std::ptr::null(), 0, wide(&mutex).as_ptr()) };
            assert!(mutex_exists(&mutex));
            unsafe { CloseHandle(held) };
            assert!(!mutex_exists(&mutex));

            signal(&event); // Nobody is listening: nothing happens.
            let waiting = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide(&event).as_ptr()) };
            assert_ne!(unsafe { WaitForSingleObject(waiting, 0) }, WAIT_OBJECT_0);
            signal(&event);
            assert_eq!(unsafe { WaitForSingleObject(waiting, 0) }, WAIT_OBJECT_0);
            unsafe { CloseHandle(waiting) };
        }
    }
}

/// The reply to one message from the agent; notifications get none.
fn respond(message: &Value, call: impl FnOnce(&str, &Value) -> (String, bool)) -> Option<Value> {
    let (Some(id), Some(method)) = (message.get("id"), message["method"].as_str()) else { return None };
    let params = &message["params"];
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = PROTOCOL_VERSIONS.into_iter().find(|v| *v == asked).unwrap_or(PROTOCOL_VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "snekkie", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let (text, is_error) = call(params["name"].as_str().unwrap_or_default(), &params["arguments"]);
            json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
        }
        _ => {
            let error = json!({ "code": -32601, "message": format!("Method not found: {method}") });
            return Some(json!({ "jsonrpc": "2.0", "id": id, "error": error }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// A readable name for an agent from the client name it gives MCP.
fn display_name(client: &str) -> String {
    match client {
        "claude-code" => "Claude Code".into(),
        "claude-ai" => "Claude Desktop".into(),
        "codex-mcp-client" | "codex" => "Codex".into(),
        "" => "An agent".into(),
        other => other.chars().take(40).collect(),
    }
}

/// A connection to the running app that has already given its token.
fn open(file: &Path) -> Result<TcpStream, String> {
    let info: Value =
        std::fs::read_to_string(file).ok().and_then(|text| serde_json::from_str(&text).ok()).ok_or(NOT_RUNNING)?;
    let port = info["port"].as_u64().and_then(|port| u16::try_from(port).ok()).ok_or(NOT_RUNNING)?;
    let token = info["token"].as_str().ok_or(NOT_RUNNING)?;
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).map_err(|_| NOT_RUNNING)?;
    writeln!(stream, "{token}").map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Hold a connection so the app shows this agent as connected, and find the
/// app again whenever it starts or access is turned back on.
fn stay_connected(file: &Path, agent: &str) {
    loop {
        if let Ok(mut stream) = open(file)
            && writeln!(stream, "{}", json!({ "agent": agent, "presence": true })).is_ok()
        {
            // Returns once the app hangs up.
            let _ = io::copy(&mut stream, &mut io::sink());
        }
        std::thread::sleep(PRESENCE_RETRY);
    }
}

/// Run one tool call in the running app.
#[doc(hidden)]
pub fn forward(file: &Path, agent: &str, tool: &str, arguments: &Value) -> Result<(String, bool), String> {
    let request = json!({ "agent": agent, "tool": tool, "arguments": arguments }).to_string();
    if request.len() as u64 >= MAX_REQUEST {
        return Ok(("That call is over the 1 MB limit. Use upload_file for big files.".into(), true));
    }
    let mut stream = open(file)?;
    let wait = if files::long_running(tool) { MAX_TRANSFER_SECONDS } else { MAX_COMMAND_SECONDS + 30 };
    stream.set_read_timeout(Some(Duration::from_secs(wait))).map_err(|e| e.to_string())?;
    writeln!(stream, "{request}").map_err(|e| e.to_string())?;
    let mut reply = String::new();
    io::BufReader::new(stream).read_line(&mut reply).map_err(|e| format!("Snekkie did not answer: {e}"))?;
    // A stale file can point at another program's port, which won't answer.
    let reply: Value = serde_json::from_str(&reply).map_err(|_| NOT_RUNNING)?;
    Ok((reply["text"].as_str().unwrap_or_default().into(), reply["isError"].as_bool().unwrap_or(true)))
}

fn tools() -> Value {
    let mut all = terminal_tools();
    if let Some(list) = all.as_array_mut() {
        list.extend(files::tools());
    }
    all
}

fn terminal_tools() -> Value {
    let session_id = json!({ "type": "integer", "description": "A session_id from list_sessions." });
    json!([
        {
            "name": "list_sessions",
            "description": "List the terminal sessions open in Snekkie (SSH, Telnet, raw TCP and serial consoles): \
                session_id, name, type, destination, connection state and whether it is the tab the user has selected.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "read_output",
            "description": "Read the most recent text of a session's screen and scrollback, as the user sees it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": session_id,
                    "lines": { "type": "integer", "description": "How many recent lines (default 100, at most 5000)." },
                },
                "required": ["session_id"],
            },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "send_command",
            "description": "Type one command line into a session, press Enter and wait for the output to settle. \
                Returns what the device printed, usually ending with the next prompt. Paging prompts such as \
                --More-- stop the output; answer them with send_keys or turn paging off on the device. If output \
                is still arriving when this returns, read_output shows the rest.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": session_id,
                    "command": { "type": "string", "description": "One command line, without a line break." },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Longest to wait for the output (default 15, at most 120).",
                    },
                },
                "required": ["session_id", "command"],
            },
            "annotations": { "destructiveHint": true },
        },
        {
            "name": "send_keys",
            "description": "Send exact keystrokes without pressing Enter or waiting: answer a prompt, page with a \
                space, or interrupt. Control keys: \\u0003 Ctrl+C, \\u001a Ctrl+Z, \\u001e Ctrl+Shift+6, \\t Tab, \
                \\r Enter, \\u001b Escape. Use read_output afterwards to see the result.",
            "inputSchema": {
                "type": "object",
                "properties": { "session_id": session_id, "text": { "type": "string" } },
                "required": ["session_id", "text"],
            },
            "annotations": { "destructiveHint": true },
        },
    ])
}

/// How an agent starts the bridge for this copy of Snekkie.
pub fn launch_command() -> (String, Vec<String>) {
    if let Ok(id) = std::env::var("FLATPAK_ID") {
        return ("flatpak".into(), vec!["run".into(), id, "--mcp".into()]);
    }
    // An AppImage runs from a temporary mount; the image itself stays put.
    let program = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| "snekkie".into());
    (program.display().to_string(), vec!["--mcp".into()])
}

/// The bridge's command line, quoted for a shell.
pub fn launch_line() -> String {
    let (program, args) = launch_command();
    format!("\"{program}\" {}", args.join(" "))
}

/// `claude mcp add` for Claude Code.
pub fn claude_code_command() -> String {
    format!("claude mcp add snekkie -- {}", launch_line())
}

/// `codex mcp add` for OpenAI Codex.
pub fn codex_command() -> String {
    format!("codex mcp add snekkie -- {}", launch_line())
}

/// The `mcpServers` entry Claude Desktop and many other MCP clients use.
pub fn json_config() -> String {
    let (command, args) = launch_command();
    let config = json!({ "mcpServers": { "snekkie": { "command": command, "args": args } } });
    serde_json::to_string_pretty(&config).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::Profile;
    use crate::session::ConnectContext;
    use crate::settings::Theme;
    use std::io::Read;

    fn no_call(_: &str, _: &Value) -> (String, bool) {
        unreachable!("not a tool call")
    }

    #[test]
    fn answers_the_protocol_and_ignores_notifications() {
        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" } });
        let reply = respond(&init, no_call).unwrap();
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert!(reply["result"]["capabilities"]["tools"].is_object());
        let future = json!({ "id": 2, "method": "initialize", "params": { "protocolVersion": "2099-01-01" } });
        assert_eq!(respond(&future, no_call).unwrap()["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);

        assert!(respond(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), no_call).is_none());
        assert!(respond(&json!({ "jsonrpc": "2.0", "id": 9, "result": {} }), no_call).is_none(), "not a request");
        assert_eq!(respond(&json!({ "id": 3, "method": "ping" }), no_call).unwrap()["result"], json!({}));

        let tools = respond(&json!({ "id": 4, "method": "tools/list" }), no_call).unwrap();
        let names: Vec<_> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].clone()).collect();
        assert_eq!(names[..4], ["list_sessions", "read_output", "send_command", "send_keys"]);
        assert!(names.contains(&json!("write_file")));

        let call = json!({ "id": "x", "method": "tools/call",
            "params": { "name": "read_output", "arguments": { "session_id": 4 } } });
        let reply = respond(&call, |tool, arguments| {
            assert_eq!((tool, &arguments["session_id"]), ("read_output", &json!(4)));
            ("hi".into(), true)
        })
        .unwrap();
        assert_eq!(reply["id"], "x");
        assert_eq!(reply["result"], json!({ "content": [{ "type": "text", "text": "hi" }], "isError": true }));

        let unknown = respond(&json!({ "id": 5, "method": "resources/list" }), no_call).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn prompts_and_pagers_count_as_waiting() {
        for line in ["lab-router#", "lab-router(config-if)# ", "user@host:~$ ", "Password:", "[confirm]", "--More--"] {
            assert!(waiting_for_input(line), "{line}");
        }
        for line in ["", "Building configuration...", "lab-router#show run | include more"] {
            assert!(!waiting_for_input(line), "{line}");
        }
    }

    #[test]
    fn agents_use_open_sessions_only_while_the_server_runs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE_NAME);
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let device = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let profile = Profile {
            name: "lab-router".into(),
            kind: "raw".into(),
            host: "127.0.0.1".into(),
            port: device.local_addr().unwrap().port(),
            ..Profile::default()
        };
        let mut session = Session::new(profile, Theme::default(), || {});
        session.connect(ConnectContext {
            runtime: runtime.handle(),
            ask_host_key: Arc::new(|_| {}),
            password: String::new(),
            key_passphrase: String::new(),
            network: Default::default(),
        });
        let (mut peer, _) = device.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !session.is_connected() {
            assert!(Instant::now() < deadline, "never connected");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(forward(&file, "Test agent", "list_sessions", &json!({})), Err(NOT_RUNNING.into()));

        let server = Server::start(runtime.handle(), file.clone(), || {}).unwrap();
        server.sync(Some(session.id), &[&session]);
        let (list, error) = forward(&file, "Test agent", "list_sessions", &json!({})).unwrap();
        assert!(!error);
        let list: Value = serde_json::from_str(&list).unwrap();
        assert_eq!(list[0]["session_id"], session.id);
        assert_eq!(
            (&list[0]["name"], &list[0]["state"], &list[0]["selected"]),
            (&json!("lab-router"), &json!("connected"), &json!(true))
        );

        // The device echoes the command, answers and prompts again.
        let device_side = std::thread::spawn(move || {
            let (mut seen, mut buf) = (Vec::new(), [0u8; 256]);
            while !seen.ends_with(b"show clock\r") {
                let n = peer.read(&mut buf).unwrap();
                assert!(n > 0, "hung up after {seen:?}");
                seen.extend_from_slice(&buf[..n]);
            }
            peer.write_all(b"show clock\r\n\x1b[1m12:00:00.000 UTC\x1b[0m\r\nlab-router#").unwrap();
            peer
        });
        let started = Instant::now();
        let arguments = json!({ "session_id": session.id, "command": "show clock" });
        let (output, error) = forward(&file, "Test agent", "send_command", &arguments).unwrap();
        assert!(!error, "{output}");
        assert_eq!(output, "show clock\n12:00:00.000 UTC\nlab-router#");
        assert!(started.elapsed() < QUIET, "a prompt ends the wait early");
        let mut peer = device_side.join().unwrap();

        let arguments = json!({ "session_id": session.id, "lines": 1 });
        assert_eq!(forward(&file, "Test agent", "read_output", &arguments), Ok(("lab-router#".into(), false)));
        let cue = |at: Instant| server.activity().sessions[&session.id].cue(at);
        // The command typed a moment ago still outranks the read, briefly.
        assert_eq!(cue(Instant::now() + WRITE_LINGER), Some(Cue::Reading));
        assert_eq!(server.activity().sessions[&session.id].agent, "Test agent");
        assert_eq!(cue(Instant::now() + READ_LINGER), None, "the cue fades");

        let arguments = json!({ "session_id": session.id.to_string(), "text": "\u{3}" });
        assert!(!forward(&file, "Test agent", "send_keys", &arguments).unwrap().1);
        assert!(matches!(cue(Instant::now()), Some(Cue::Writing(_))));
        let mut byte = [0u8];
        peer.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [3]);

        // A bridge stays connected for as long as its agent runs.
        let mut present = open(&file).unwrap();
        writeln!(present, "{}", json!({ "agent": "Codex", "presence": true })).unwrap();
        wait_until(|| server.activity().agents == ["Codex"]);
        drop(present);
        wait_until(|| server.activity().agents.is_empty());
        let mut held = open(&file).unwrap();
        writeln!(held, "{}", json!({ "agent": "Codex", "presence": true })).unwrap();
        wait_until(|| server.activity().agents.len() == 1);

        let two_lines = json!({ "session_id": session.id, "command": "conf t\nend" });
        assert!(forward(&file, "Test agent", "send_command", &two_lines).unwrap().1);
        assert!(forward(&file, "Test agent", "read_output", &json!({ "session_id": 999 })).unwrap().1);

        // Without the token the app hangs up without running anything.
        let info: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let mut stranger = TcpStream::connect(("127.0.0.1", info["port"].as_u64().unwrap() as u16)).unwrap();
        writeln!(
            stranger,
            "guess\n{}",
            json!({ "tool": "send_keys", "arguments": { "session_id": session.id, "text": "x" } })
        )
        .unwrap();
        let mut reply = String::new();
        stranger.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "");

        drop(server);
        assert!(!file.exists());
        assert_eq!(forward(&file, "Test agent", "list_sessions", &json!({})), Err(NOT_RUNNING.into()));
        // Turning access off hangs up on connected agents, so they look for the app again.
        held.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert!(held.read_to_end(&mut Vec::new()).is_ok());
    }

    fn wait_until(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn agents_are_named_as_people_know_them() {
        assert_eq!(display_name("claude-code"), "Claude Code");
        assert_eq!(display_name("claude-ai"), "Claude Desktop");
        assert_eq!(display_name("codex-mcp-client"), "Codex");
        assert_eq!(display_name(""), "An agent");
        assert_eq!(display_name("my-local-agent"), "my-local-agent");
        assert_eq!(display_name(&"x".repeat(100)).len(), 40);
    }
}
