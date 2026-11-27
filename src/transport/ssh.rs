//! SSH transport backed by russh (pure Rust, no OpenSSL to ship).

use std::borrow::Cow;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use russh::client::{self, AuthResult, DisconnectReason, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::known_hosts::{Entry, HostPatterns};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, MethodKind, Preferred, cipher, kex, mac};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;

use super::{CONNECT_TIMEOUT, Command, Link, Sink, TERM, connect_tcp, keepalive_interval, transfer, tunnel};
use crate::profiles::{Auth, Profile};

/// Default key files tried in "SSH agent / default keys" mode, like
/// OpenSSH and paramiko do.
const DEFAULT_KEYS: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];

/// A question for the user: do we trust this host key?
pub struct HostKeyQuestion {
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
    /// The saved session's name when this host is a jump host on the way.
    pub jump_host: Option<String>,
    pub reply: oneshot::Sender<bool>,
}

/// Hands host key questions to the UI.
pub type HostKeyAsker = Arc<dyn Fn(HostKeyQuestion) + Send + Sync>;

/// A keyboard-interactive round for the user to answer, such as a one-time
/// code. Answers go straight back to the server and are never kept.
pub struct PromptQuestion {
    pub host: String,
    pub name: String,
    pub instructions: String,
    /// Prompt text and whether the answer may be shown while typed.
    pub prompts: Vec<(String, bool)>,
    /// `None` cancels the login.
    pub reply: oneshot::Sender<Option<Vec<String>>>,
}

/// Hands keyboard-interactive prompts to the UI.
pub type PromptAsker = Arc<dyn Fn(PromptQuestion) + Send + Sync>;

/// Everything needed to connect that isn't stored in the profile.
pub struct Credentials {
    pub password: String,
    pub key_passphrase: String,
    /// Where trusted host keys are recorded; normally [`known_hosts_path`].
    pub known_hosts: PathBuf,
    /// Asks the user prompts the password can't answer. Without it, such a
    /// login fails.
    pub ask_prompts: Option<PromptAsker>,
    /// Saved SSH sessions to log in to and connect through first, first hop
    /// first (see [`crate::profiles::jump_chain`]). Each asks for its own
    /// password or passphrase through `ask_prompts`.
    pub jumps: Vec<Profile>,
}

static KNOWN_HOSTS_FOR_TESTS: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Keeps app-level tests away from the user's real known_hosts file.
#[doc(hidden)]
pub fn use_known_hosts_for_tests(path: PathBuf) {
    let _ = KNOWN_HOSTS_FOR_TESTS.set(path);
}

pub fn known_hosts_path() -> PathBuf {
    if let Some(path) = KNOWN_HOSTS_FOR_TESTS.get() {
        return path.clone();
    }
    dirs::home_dir().unwrap_or_default().join(".ssh").join("known_hosts")
}

pub fn description(profile: &Profile) -> String {
    let who = if profile.username.is_empty() { String::new() } else { format!("{}@", profile.username) };
    let port = if profile.port == 22 { String::new() } else { format!(":{}", profile.port) };
    format!("SSH  {who}{}{port}", profile.host)
}

/// Algorithms offered, strongest first.
///
/// The tail end re-enables SHA-1 key exchange, CBC ciphers and SHA-1 MACs.
/// They are only ever picked when the server supports nothing better, which
/// in practice means older Cisco IOS and other network gear that would
/// otherwise be unreachable. This matches what PuTTY and the Python release
/// (paramiko) accept.
pub fn preferred_algorithms() -> Preferred {
    Preferred {
        kex: Cow::Owned(vec![
            kex::MLKEM768X25519_SHA256,
            kex::CURVE25519,
            kex::CURVE25519_PRE_RFC_8731,
            kex::ECDH_SHA2_NISTP256,
            kex::ECDH_SHA2_NISTP384,
            kex::ECDH_SHA2_NISTP521,
            kex::DH_GEX_SHA256,
            kex::DH_G16_SHA512,
            kex::DH_G18_SHA512,
            kex::DH_G14_SHA256,
            // Legacy, for older network gear.
            kex::DH_G14_SHA1,
            kex::DH_GEX_SHA1,
            kex::DH_G1_SHA1,
            kex::EXTENSION_SUPPORT_AS_CLIENT,
            kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
        ]),
        cipher: Cow::Owned(vec![
            cipher::CHACHA20_POLY1305,
            cipher::AES_256_GCM,
            cipher::AES_128_GCM,
            cipher::AES_256_CTR,
            cipher::AES_192_CTR,
            cipher::AES_128_CTR,
            // Legacy, for older network gear.
            cipher::AES_256_CBC,
            cipher::AES_192_CBC,
            cipher::AES_128_CBC,
            cipher::TRIPLE_DES_CBC,
        ]),
        mac: Cow::Owned(vec![
            mac::HMAC_SHA512_ETM,
            mac::HMAC_SHA256_ETM,
            mac::HMAC_SHA512,
            mac::HMAC_SHA256,
            // Legacy, for older network gear.
            mac::HMAC_SHA1_ETM,
            mac::HMAC_SHA1,
        ]),
        ..Preferred::default()
    }
}

fn client_config(profile: &Profile) -> client::Config {
    let mut config = client::Config {
        preferred: preferred_algorithms(),
        // Sent only after this long without hearing from the server. After
        // `keepalive_max` (3) go unanswered, the connection is given up as
        // dead instead of hanging forever.
        keepalive_interval: keepalive_interval(profile.keepalive),
        // Without this, Nagle's algorithm sits on the one-byte packets an
        // interactive shell sends per keystroke: the classic "SSH typing
        // feels laggy" complaint.
        nodelay: true,
        ..Default::default()
    };
    // Older gear offers 2048-bit group exchange at most.
    if let Ok(gex) = client::GexParams::new(2048, 8192, 8192) {
        config.gex = gex;
    }
    config
}

#[derive(Debug)]
pub(super) enum Error {
    Ssh(russh::Error),
    HostKeyChanged { line: usize },
    HostKeyRevoked { line: usize },
    KnownHosts(String),
    HostKeyRejected { fingerprint: String },
}

impl From<russh::Error> for Error {
    fn from(e: russh::Error) -> Self {
        Error::Ssh(e)
    }
}

fn matches_host(host: &str, patterns: &HostPatterns) -> Result<bool, Error> {
    match patterns {
        HostPatterns::Patterns(patterns) => {
            let mut matched = false;
            for pattern in patterns {
                let (negated, pattern) = pattern.strip_prefix('!').map_or((false, pattern.as_str()), |p| (true, p));
                let pattern = regex::escape(pattern).replace("\\*", ".*").replace("\\?", ".");
                let regex =
                    regex::Regex::new(&format!("(?i)\\A{pattern}\\z")).map_err(|e| Error::KnownHosts(e.to_string()))?;
                if regex.is_match(host) {
                    if negated {
                        return Ok(false);
                    }
                    matched = true;
                }
            }
            Ok(matched)
        }
        HostPatterns::HashedName { salt, hash } => {
            let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, salt);
            Ok(ring::hmac::verify(&key, host.as_bytes(), hash).is_ok())
        }
    }
}

fn check_revoked_host_key(host: &str, port: u16, key: &PublicKey, path: &Path) -> Result<(), Error> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::KnownHosts(e.to_string())),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    let host = if port == 22 { host } else { format!("[{host}]:{port}") };
    // russh's ordinary known_hosts check ignores @revoked entries.
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| Error::KnownHosts(e.to_string()))?;
        let mut fields = line.split_whitespace();
        if fields.next() != Some("@revoked") {
            continue;
        }
        let entry = format!("@revoked {}", fields.take(3).collect::<Vec<_>>().join(" "))
            .parse::<Entry>()
            .map_err(|e| Error::KnownHosts(format!("invalid revoked entry on line {}: {e}", index + 1)))?;
        if entry.public_key().key_data() == key.key_data() && matches_host(&host, entry.host_patterns())? {
            return Err(Error::HostKeyRevoked { line: index + 1 });
        }
    }
    Ok(())
}

pub(super) struct Client {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    ask: HostKeyAsker,
    /// Set while a host key prompt is up, so the handshake timeout pauses.
    waiting_for_user: Arc<AtomicBool>,
    sink: Arc<dyn Sink>,
    /// Why the connection dropped, if it failed rather than being closed.
    lost: Arc<Mutex<Option<String>>>,
    /// Remote forwards whose channels this connection accepts.
    forwards: tunnel::RemoteForwards,
}

impl client::Handler for Client {
    type Error = Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            // We never offer certificate algorithms, so a server can't pick one.
            return Ok(false);
        };
        check_revoked_host_key(&self.host, self.port, key, &self.known_hosts)?;
        match russh::keys::check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => return Ok(true),
            Err(russh::keys::Error::KeyChanged { line }) => return Err(Error::HostKeyChanged { line }),
            // Not recorded, or known_hosts unreadable: ask.
            Ok(false) | Err(_) => {}
        }

        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let (reply, answer) = oneshot::channel();
        self.waiting_for_user.store(true, Ordering::SeqCst);
        (self.ask)(HostKeyQuestion {
            host: self.host.clone(),
            port: self.port,
            key_type: key.algorithm().to_string(),
            fingerprint: fingerprint.clone(),
            jump_host: None,
            reply,
        });
        let accepted = answer.await.unwrap_or(false);
        self.waiting_for_user.store(false, Ordering::SeqCst);
        if !accepted {
            return Err(Error::HostKeyRejected { fingerprint });
        }
        if let Err(e) = russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.known_hosts)
        {
            // Non-fatal: we still trust it for this session.
            self.sink.notice(format!("Could not save the host key to known_hosts: {e}"));
        }
        Ok(true)
    }

    async fn auth_banner(&mut self, banner: &str, _session: &mut client::Session) -> Result<(), Error> {
        // Pre-login banners (a Cisco `banner login`, legal notices) would
        // otherwise never be seen.
        let text = banner.replace("\r\n", "\n").replace('\n', "\r\n");
        self.sink.data(text.as_bytes());
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Error> {
        // Only ports this tab asked the server to forward; anything else is refused.
        match tunnel::remote_target(&self.forwards, connected_port) {
            Some(target) => {
                reply.accept().await;
                tokio::spawn(tunnel::deliver(channel, target));
            }
            None => reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await,
        }
        Ok(())
    }

    async fn disconnected(&mut self, reason: DisconnectReason<Error>) -> Result<(), Error> {
        match reason {
            DisconnectReason::ReceivedDisconnect(_) => Ok(()),
            DisconnectReason::Error(e) => {
                // Called before the channel reports it's gone, so the session
                // can say why instead of a bare "closed".
                *self.lost.lock() = Some(match &e {
                    Error::Ssh(russh::Error::KeepaliveTimeout) => {
                        format!("Connection lost: {} stopped answering keepalives", self.host)
                    }
                    Error::Ssh(e) => format!("Connection lost: {e}"),
                    other => format!("Connection lost: {other:?}"),
                });
                Err(e)
            }
        }
    }
}

fn describe(error: &Error, host: &str, known_hosts: &Path) -> String {
    match error {
        Error::HostKeyRevoked { line } => format!(
            "The host key for {host} is revoked in {} (line {line}). The connection was refused.",
            known_hosts.display()
        ),
        Error::KnownHosts(message) => {
            format!("Could not verify the host key for {host} using {}: {message}", known_hosts.display())
        }
        Error::HostKeyChanged { line } => format!(
            "The host key for {host} does not match the one recorded in known_hosts (line {line}). \
             This could mean someone is intercepting the connection, or the device was replaced \
             or re-keyed. If you know the key legitimately changed, remove that line from {} and \
             connect again.",
            known_hosts.display()
        ),
        Error::HostKeyRejected { fingerprint } => {
            format!("Host key for {host} was not accepted ({fingerprint})")
        }
        Error::Ssh(russh::Error::NoCommonAlgo { kind, ours, theirs }) => format!(
            "Could not connect to {host}: no {kind:?} algorithm in common.\nServer offers: {}\nSnekkie supports: {}",
            theirs.join(", "),
            ours.join(", ")
        ),
        Error::Ssh(e) => format!("Could not connect to {host}: {e}"),
    }
}

fn local_username() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "root".into())
}

/// `~/...` relative to the home directory, as people type key paths.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

pub(super) type Session = Handle<Client>;

/// A logged-in connection that a shell and a file explorer can share. Whoever
/// lets go of it last hangs up.
#[derive(Clone)]
pub struct Connection {
    handle: Arc<Session>,
    /// Jump hosts the connection runs through, first hop first. They live as
    /// long as the connection does.
    hops: Arc<Vec<Hop>>,
    /// Remote forwards the shell's tunnels registered.
    remote: tunnel::RemoteForwards,
    users: Arc<()>,
}

struct Hop {
    name: String,
    handle: Session,
    lost: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Connection")
    }
}

impl Connection {
    fn new(handle: Session, hops: Vec<Hop>, remote: tunnel::RemoteForwards) -> Connection {
        Connection { handle: Arc::new(handle), hops: Arc::new(hops), remote, users: Arc::new(()) }
    }

    pub(super) fn handle(&self) -> &Session {
        &self.handle
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_closed() || self.hops.iter().any(|hop| hop.handle.is_closed())
    }

    /// Why the connection dropped, if a jump host it runs through went first.
    fn dropped_hop(&self) -> Option<String> {
        let hop = self.hops.iter().find(|hop| hop.handle.is_closed())?;
        let reason = hop.lost.lock().take().unwrap_or_else(|| "it closed the connection".into());
        Some(format!("Connection lost at jump host {}: {reason}", hop.name))
    }

    /// Stop using the connection, hanging up if nobody else is.
    pub async fn release(self) {
        // into_inner picks exactly one winner when two holders release together.
        if Arc::into_inner(self.users).is_some() {
            let _ = self.handle.disconnect(Disconnect::ByApplication, "", "en").await;
            for hop in self.hops.iter().rev() {
                let _ = hop.handle.disconnect(Disconnect::ByApplication, "", "en").await;
            }
        }
    }
}

async fn try_password(handle: &mut Session, user: &str, host: &str, credentials: &Credentials) -> Result<bool, String> {
    let result = handle
        .authenticate_password(user, &credentials.password)
        .await
        .map_err(|e| format!("Authentication failed: {e}"))?;
    let remaining = match result {
        AuthResult::Success => return Ok(true),
        AuthResult::Failure { remaining_methods, .. } => remaining_methods,
    };
    // A lot of network gear (and PAM-based servers) only accept the
    // password through keyboard-interactive, often followed by an MFA code.
    if remaining.contains(&MethodKind::KeyboardInteractive) {
        return try_keyboard_interactive(handle, user, host, credentials).await;
    }
    Ok(false)
}

/// The saved password answers a round that is just one hidden "password"
/// prompt, once. Anything else (codes, tokens, questions) is for the user.
fn password_answers(prompts: &[(String, bool)], password: &str, used: bool) -> Option<Vec<String>> {
    match prompts {
        [] => Some(Vec::new()),
        [(text, false)] if !used && !password.is_empty() && text.to_lowercase().contains("password") => {
            Some(vec![password.to_string()])
        }
        _ => None,
    }
}

async fn try_keyboard_interactive(
    handle: &mut Session,
    user: &str,
    host: &str,
    credentials: &Credentials,
) -> Result<bool, String> {
    let mut response = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(|e| format!("Authentication failed: {e}"))?;
    let mut password_used = false;
    // Servers may send several rounds, including empty ones.
    for _ in 0..8 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                let prompts: Vec<(String, bool)> = prompts.into_iter().map(|p| (p.prompt, p.echo)).collect();
                let answers = match password_answers(&prompts, &credentials.password, password_used) {
                    Some(answers) => {
                        password_used |= !answers.is_empty();
                        answers
                    }
                    None => {
                        let Some(ask) = &credentials.ask_prompts else { return Ok(false) };
                        let (reply, answer) = oneshot::channel();
                        ask(PromptQuestion { host: host.to_string(), name, instructions, prompts, reply });
                        match answer.await {
                            Ok(Some(answers)) => answers,
                            _ => return Err("Login canceled".into()),
                        }
                    }
                };
                response = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|e| format!("Authentication failed: {e}"))?;
            }
        }
    }
    Ok(false)
}

async fn try_key(handle: &mut Session, user: &str, key: russh::keys::PrivateKey) -> Result<bool, String> {
    let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
    let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
    match handle.authenticate_publickey(user, key).await {
        Ok(AuthResult::Success) => Ok(true),
        Ok(AuthResult::Failure { .. }) => Ok(false),
        Err(e) => Err(format!("Authentication failed: {e}")),
    }
}

async fn try_key_file(handle: &mut Session, user: &str, path: &Path, passphrase: &str) -> Result<bool, String> {
    let passphrase = (!passphrase.is_empty()).then_some(passphrase);
    let key = russh::keys::load_secret_key(path, passphrase).map_err(|e| match e {
        russh::keys::Error::KeyIsEncrypted => {
            format!("{} is encrypted. Enter its passphrase when connecting.", path.display())
        }
        e => format!("Could not load key {}: {e}", path.display()),
    })?;
    try_key(handle, user, key).await
}

type DynAgent = AgentClient<Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin + 'static>>;

async fn connect_agents() -> Vec<DynAgent> {
    #[cfg(unix)]
    {
        AgentClient::connect_env().await.ok().map(|a| a.dynamic()).into_iter().collect()
    }
    #[cfg(windows)]
    {
        // Both are tried: connecting to Pageant "succeeds" even when it isn't
        // running, and only fails on the first request.
        let mut agents = Vec::new();
        if let Ok(agent) = AgentClient::connect_pageant().await {
            agents.push(agent.dynamic());
        }
        if let Ok(agent) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
            agents.push(agent.dynamic());
        }
        agents
    }
}

async fn try_agent(handle: &mut Session, user: &str) -> bool {
    let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
    for mut agent in connect_agents().await {
        let Ok(identities) = agent.request_identities().await else {
            continue;
        };
        for identity in identities {
            let key: PublicKey = identity.public_key().into_owned();
            if let Ok(AuthResult::Success) = handle.authenticate_publickey_with(user, key, hash, &mut agent).await {
                return true;
            }
        }
    }
    false
}

async fn try_default_keys(handle: &mut Session, user: &str) -> bool {
    let dir = dirs::home_dir().unwrap_or_default().join(".ssh");
    for name in DEFAULT_KEYS {
        // Encrypted default keys are skipped rather than prompted for, as
        // OpenSSH does when an agent is expected to hold them.
        if let Ok(key) = russh::keys::load_secret_key(dir.join(name), None)
            && let Ok(true) = try_key(handle, user, key).await
        {
            return true;
        }
    }
    false
}

async fn authenticate(handle: &mut Session, profile: &Profile, credentials: &Credentials) -> Result<(), String> {
    let user = if profile.username.is_empty() { local_username() } else { profile.username.clone() };
    let ok = match profile.auth() {
        Auth::Password => try_password(handle, &user, &profile.host, credentials).await?,
        Auth::Key => {
            let key_ok = if profile.key_file.is_empty() {
                false
            } else {
                try_key_file(handle, &user, &expand_home(&profile.key_file), &credentials.key_passphrase).await?
            };
            key_ok || try_agent(handle, &user).await || try_default_keys(handle, &user).await
        }
        Auth::Agent => try_agent(handle, &user).await || try_default_keys(handle, &user).await,
    };
    if ok { Ok(()) } else { Err(format!("Authentication failed for {user}@{}", profile.host)) }
}

/// Handshake (with host key check) and authentication over an open stream:
/// a TCP connection, or a channel through a jump host.
async fn login<S>(
    profile: &Profile,
    stream: S,
    credentials: &Credentials,
    ask: HostKeyAsker,
    sink: Arc<dyn Sink>,
    lost: Arc<Mutex<Option<String>>>,
    forwards: tunnel::RemoteForwards,
) -> Result<Session, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let host = profile.host.trim().to_string();
    // Time spent waiting for the user to answer a host key prompt doesn't
    // count towards the handshake timeout.
    let waiting = Arc::new(AtomicBool::new(false));
    let handler = Client {
        host: host.clone(),
        port: profile.port,
        known_hosts: credentials.known_hosts.clone(),
        ask,
        waiting_for_user: waiting.clone(),
        sink,
        lost,
        forwards,
    };
    let handshake = client::connect_stream(Arc::new(client_config(profile)), stream, handler);
    tokio::pin!(handshake);
    let mut idle = Duration::ZERO;
    let tick = Duration::from_millis(250);
    let mut handle = loop {
        tokio::select! {
            result = &mut handshake => break result.map_err(|e| describe(&e, &host, &credentials.known_hosts))?,
            _ = tokio::time::sleep(tick) => {
                if !waiting.load(Ordering::SeqCst) {
                    idle += tick;
                    if idle >= CONNECT_TIMEOUT {
                        return Err(format!("Could not connect to {host}: the SSH handshake timed out"));
                    }
                }
            }
        }
    };
    authenticate(&mut handle, profile, credentials).await?;
    Ok(handle)
}

/// One hidden answer from the user for a jump host, such as its password.
async fn ask_hop_secret(
    credentials: &Credentials,
    hop: &Profile,
    title: &str,
    prompt: String,
) -> Result<String, String> {
    let Some(ask) = &credentials.ask_prompts else {
        return Err(format!("it needs a {}, and there is nobody to ask", title.to_lowercase()));
    };
    let (reply, answer) = oneshot::channel();
    ask(PromptQuestion {
        host: format!("Jump host {} ({})", hop.name, hop.host.trim()),
        name: title.into(),
        instructions: String::new(),
        prompts: vec![(prompt, false)],
        reply,
    });
    match answer.await {
        Ok(Some(mut answers)) if !answers.is_empty() => Ok(answers.swap_remove(0)),
        _ => Err("Login canceled".into()),
    }
}

/// What a jump host needs to log in: its own password or key passphrase,
/// asked for now and never stored.
async fn hop_credentials(hop: &Profile, credentials: &Credentials) -> Result<Credentials, String> {
    let mut password = String::new();
    let mut key_passphrase = String::new();
    match hop.auth() {
        Auth::Password => {
            let user = if hop.username.is_empty() { local_username() } else { hop.username.clone() };
            let prompt = format!("Password for {user}@{}:", hop.host.trim());
            password = ask_hop_secret(credentials, hop, "SSH password", prompt).await?;
        }
        Auth::Key if !hop.key_file.is_empty() => {
            let path = expand_home(&hop.key_file);
            if let Err(russh::keys::Error::KeyIsEncrypted) = russh::keys::load_secret_key(&path, None) {
                let prompt = format!("Passphrase for {}:", path.display());
                key_passphrase = ask_hop_secret(credentials, hop, "Key passphrase", prompt).await?;
            }
        }
        _ => {}
    }
    Ok(Credentials {
        password,
        key_passphrase,
        known_hosts: credentials.known_hosts.clone(),
        ask_prompts: credentials.ask_prompts.clone(),
        jumps: Vec::new(),
    })
}

/// A channel from a jump host to the next host's SSH port.
async fn open_through(hop: &Hop, next: &Profile) -> Result<russh::ChannelStream<client::Msg>, String> {
    let host = next.host.trim();
    let open = hop.handle.channel_open_direct_tcpip(host, next.port.into(), "127.0.0.1", 0);
    match tokio::time::timeout(CONNECT_TIMEOUT, open).await {
        Ok(Ok(channel)) => Ok(channel.into_stream()),
        Ok(Err(e)) => Err(format!("Jump host {} could not reach {host}:{}: {e}", hop.name, next.port)),
        Err(_) => Err(format!("Jump host {} could not reach {host}:{}: timed out", hop.name, next.port)),
    }
}

/// Log in to every jump host in turn, each reaching the next through a
/// direct-tcpip channel, then to the profile itself. Every host key is checked
/// under that host's own name and port. Only the first hop is a TCP
/// connection from this computer, so only it goes through `access`.
async fn establish(
    profile: &Profile,
    credentials: &Credentials,
    ask: HostKeyAsker,
    sink: Arc<dyn Sink>,
    lost: Arc<Mutex<Option<String>>>,
    access: &crate::network::NetworkAccess,
) -> Result<Connection, String> {
    let mut hops: Vec<Hop> = Vec::new();
    for hop in &credentials.jumps {
        let named = |e: String| format!("Jump host {}: {e}", hop.name);
        let secrets = hop_credentials(hop, credentials).await.map_err(named)?;
        let hop_lost = Arc::new(Mutex::new(None));
        let hop_ask: HostKeyAsker = {
            let (ask, name) = (ask.clone(), hop.name.clone());
            Arc::new(move |mut question: HostKeyQuestion| {
                question.jump_host = Some(name.clone());
                ask(question)
            })
        };
        let handle = match hops.last() {
            None => {
                let stream = connect_tcp(hop.host.trim(), hop.port, access).await.map_err(named)?;
                login(hop, stream, &secrets, hop_ask, sink.clone(), hop_lost.clone(), Default::default())
                    .await
                    .map_err(named)?
            }
            // Already names the hop it went through.
            Some(previous) => {
                let stream = open_through(previous, hop).await?;
                login(hop, stream, &secrets, hop_ask, sink.clone(), hop_lost.clone(), Default::default())
                    .await
                    .map_err(named)?
            }
        };
        hops.push(Hop { name: hop.name.clone(), handle, lost: hop_lost });
    }
    let remote = tunnel::RemoteForwards::default();
    let handle = match hops.last() {
        None => {
            let stream = connect_tcp(profile.host.trim(), profile.port, access).await?;
            login(profile, stream, credentials, ask, sink, lost, remote.clone()).await?
        }
        Some(previous) => {
            let stream = open_through(previous, profile).await?;
            login(profile, stream, credentials, ask, sink, lost, remote.clone()).await?
        }
    };
    Ok(Connection::new(handle, hops, remote))
}

/// Log in without opening a shell, for a file explorer of its own.
pub fn connect(
    runtime: &tokio::runtime::Handle,
    profile: Profile,
    credentials: Credentials,
    ask: HostKeyAsker,
    sink: Arc<dyn Sink>,
    access: crate::network::NetworkAccess,
) -> oneshot::Receiver<Result<Connection, String>> {
    let (mut reply, connection) = oneshot::channel();
    runtime.spawn(async move {
        let lost = Arc::new(Mutex::new(None));
        tokio::select! {
            result = establish(&profile, &credentials, ask, sink, lost, &access) => {
                let _ = reply.send(result);
            }
            // Closing the tab cancels a slow connect.
            () = reply.closed() => {}
        }
    });
    connection
}

/// Start connecting in the background. Everything after this is reported
/// through the sink.
pub fn start(
    runtime: &tokio::runtime::Handle,
    profile: Profile,
    credentials: Credentials,
    size: (u16, u16),
    ask: HostKeyAsker,
    sink: Arc<dyn Sink>,
) -> Link {
    start_with_access(runtime, profile, credentials, size, ask, sink, crate::network::NetworkAccess::default())
}

pub fn start_with_access(
    runtime: &tokio::runtime::Handle,
    profile: Profile,
    credentials: Credentials,
    size: (u16, u16),
    ask: HostKeyAsker,
    sink: Arc<dyn Sink>,
    access: crate::network::NetworkAccess,
) -> Link {
    let (link, commands) = Link::new();
    runtime.spawn(async move {
        let reason = run(profile, credentials, size, ask, sink.clone(), commands, access).await;
        sink.closed(reason);
    });
    link
}

async fn run(
    profile: Profile,
    credentials: Credentials,
    mut size: (u16, u16),
    ask: HostKeyAsker,
    sink: Arc<dyn Sink>,
    mut commands: UnboundedReceiver<Command>,
    access: crate::network::NetworkAccess,
) -> Option<String> {
    let lost = Arc::new(Mutex::new(None));
    // Keep an eye on the command queue while connecting, so closing the tab
    // cancels a slow connect and a resize during it isn't lost.
    let connection = {
        let connecting = establish(&profile, &credentials, ask, sink.clone(), lost.clone(), &access);
        tokio::pin!(connecting);
        loop {
            tokio::select! {
                result = &mut connecting => match result {
                    Ok(connection) => break connection,
                    Err(message) => return Some(message),
                },
                command = commands.recv() => match command {
                    Some(Command::Resize { columns, lines }) => size = (columns, lines),
                    Some(Command::Close) | None => return None,
                    Some(_) => {}
                },
            }
        }
    };
    // Don't keep the password in memory for the life of the session.
    drop(credentials);
    let handle = connection.handle.clone();
    // Transfers stop when this goes away, however the session ends.
    let alive = Arc::new(());

    let mut channel = match handle.channel_open_session().await {
        Ok(channel) => channel,
        Err(e) => return Some(format!("Could not open a shell: {e}")),
    };
    if let Err(e) = channel.request_pty(false, TERM, size.0.into(), size.1.into(), 0, 0, &[]).await {
        return Some(format!("Could not open a shell: {e}"));
    }
    if let Err(e) = channel.request_shell(false).await {
        return Some(format!("Could not open a shell: {e}"));
    }
    sink.connected();
    // Forwards go over this (the final target's) connection and stop when run returns.
    let _tunnels = tunnel::start(&handle, &profile.tunnels, &connection.remote, &access, sink.clone());

    loop {
        tokio::select! {
            message = channel.wait() => match message {
                Some(ChannelMsg::Data { data }) => sink.data(&data),
                Some(ChannelMsg::ExtendedData { data, .. }) => sink.data(&data),
                Some(ChannelMsg::Close) | None => {
                    if !connection.hops.is_empty() {
                        // A hop's end reaches the shell first; give its own report a moment.
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    return Some(
                        connection
                            .dropped_hop()
                            .or_else(|| lost.lock().take())
                            .unwrap_or_else(|| "Connection closed by remote host".into()),
                    );
                }
                Some(_) => {}
            },
            command = commands.recv() => match command {
                Some(Command::Write(bytes)) => {
                    if let Err(e) = channel.data(&bytes[..]).await {
                        return Some(format!("Write failed: {e}"));
                    }
                }
                Some(Command::Resize { columns, lines }) => {
                    let _ = channel.window_change(columns.into(), lines.into(), 0, 0).await;
                }
                Some(Command::Break) => sink.notice("SSH sessions do not support break".into()),
                Some(Command::Transfer(mut request)) => {
                    request.connection = Arc::downgrade(&alive);
                    tokio::spawn(transfer::run(handle.clone(), request));
                }
                Some(Command::ShareConnection(reply)) => {
                    let _ = reply.send(connection.clone());
                }
                Some(Command::Close) | None => {
                    let _ = channel.close().await;
                    connection.release().await;
                    return None;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_answers_only_one_hidden_password_prompt_once() {
        let prompts = |list: &[(&str, bool)]| list.iter().map(|(t, e)| (t.to_string(), *e)).collect::<Vec<_>>();
        assert_eq!(password_answers(&[], "pw", false), Some(Vec::new()));
        assert_eq!(password_answers(&prompts(&[("Password: ", false)]), "pw", false), Some(vec!["pw".into()]));
        assert_eq!(password_answers(&prompts(&[("admin's PASSWORD:", false)]), "pw", false), Some(vec!["pw".into()]));
        // Already used, none given, shown while typed, not a password, or more than one prompt.
        assert_eq!(password_answers(&prompts(&[("Password: ", false)]), "pw", true), None);
        assert_eq!(password_answers(&prompts(&[("Password: ", false)]), "", false), None);
        assert_eq!(password_answers(&prompts(&[("Password: ", true)]), "pw", false), None);
        assert_eq!(password_answers(&prompts(&[("Verification code: ", false)]), "pw", false), None);
        assert_eq!(password_answers(&prompts(&[("Password: ", false), ("OTP: ", false)]), "pw", false), None);
    }

    const TEST_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";

    #[test]
    fn revocation_matches_host_patterns_ports_and_hashed_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = PublicKey::from_openssh(TEST_KEY).unwrap();
        let hmac = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, b"test salt");
        let hashed = HostPatterns::HashedName {
            salt: b"test salt".to_vec(),
            hash: ring::hmac::sign(&hmac, b"[switch.example]:2222").as_ref().try_into().unwrap(),
        };
        for (host, port, pattern, revoked) in [
            ("switch.example", 22, "switch.example", true),
            ("SWITCH.example", 22, "switch.example", true),
            ("switch.example", 22, "*.example", true),
            ("switch.example", 22, "swi??h.example,other.example", true),
            ("switch.example", 22, "*.example,!switch.example", false),
            ("switch.example", 22, "!switch.example,*.example", false),
            ("switch.example", 22, "other.example", false),
            ("switch.example", 2222, "[switch.example]:2222", true),
            ("switch.example", 2222, "[*.example]:2222", true),
            ("switch.example", 2222, "[switch.example]:22", false),
            ("switch.example", 2222, "switch.example", false),
            ("[::1]", 2222, "[::1]:2222", true),
            ("switch.example", 2222, hashed.to_string().as_str(), true),
            ("other.example", 2222, hashed.to_string().as_str(), false),
        ] {
            std::fs::write(&path, format!("# fixture\n  @revoked\t{pattern}\t{TEST_KEY} revoked fixture\n")).unwrap();
            let result = check_revoked_host_key(host, port, &key, &path);
            if revoked {
                assert!(matches!(result, Err(Error::HostKeyRevoked { line: 2 })), "{pattern}: {result:?}");
            } else {
                assert!(result.is_ok(), "{pattern}: {result:?}");
            }
        }
        std::fs::write(&path, format!("@revoked * {TEST_KEY}\n")).unwrap();
        let other = russh::keys::parse_public_key_base64(
            "AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF",
        )
        .unwrap();
        assert!(check_revoked_host_key("switch.example", 22, &other, &path).is_ok());
    }

    #[test]
    fn revocation_check_fails_closed_on_invalid_or_unreadable_trust_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = PublicKey::from_openssh(TEST_KEY).unwrap();
        assert!(check_revoked_host_key("switch.example", 22, &key, &path).is_ok());
        assert!(matches!(check_revoked_host_key("switch.example", 22, &key, dir.path()), Err(Error::KnownHosts(_))));
        for contents in [b"@revoked * ssh-ed25519 invalid\n".as_slice(), b"@revoked\n", b"\xff\n"] {
            std::fs::write(&path, contents).unwrap();
            assert!(matches!(check_revoked_host_key("switch.example", 22, &key, &path), Err(Error::KnownHosts(_))));
        }
        std::fs::write(&path, format!("@cert-authority * {TEST_KEY}\n# @revoked * {TEST_KEY}\n")).unwrap();
        assert!(check_revoked_host_key("switch.example", 22, &key, &path).is_ok());
    }

    #[test]
    fn description_matches_python_format() {
        let profile = Profile { host: "10.0.0.1".into(), ..Profile::default() };
        assert_eq!(description(&profile), "SSH  10.0.0.1");
        let profile = Profile { username: "admin".into(), port: 2222, ..profile };
        assert_eq!(description(&profile), "SSH  admin@10.0.0.1:2222");
    }

    #[test]
    fn legacy_algorithms_come_last() {
        let preferred = preferred_algorithms();
        let kex: Vec<&str> = preferred.kex.iter().map(|k| k.as_ref()).collect();
        let modern = kex.iter().position(|k| *k == "curve25519-sha256").unwrap();
        let legacy = kex.iter().position(|k| *k == "diffie-hellman-group1-sha1").unwrap();
        assert!(modern < legacy);
        let ciphers: Vec<&str> = preferred.cipher.iter().map(|c| c.as_ref()).collect();
        assert_eq!(ciphers.first(), Some(&"chacha20-poly1305@openssh.com"));
        assert_eq!(ciphers.last(), Some(&"3des-cbc"));
    }

    #[test]
    fn home_expansion() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_home("~/.ssh/id_rsa"), home.join(".ssh/id_rsa"));
        assert_eq!(expand_home("/abs/key"), PathBuf::from("/abs/key"));
    }
}
