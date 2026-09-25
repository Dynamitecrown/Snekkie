//! SSH transport backed by russh (pure Rust, no OpenSSL to ship).

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use russh::client::{self, AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::AgentClient;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, MethodKind, Preferred, cipher, kex, mac};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;

use super::{Command, Link, Sink};
use crate::profiles::{Auth, Profile};

/// How long the TCP connection and the SSH handshake each get before giving
/// up. Time spent waiting for the user to answer a host key prompt doesn't
/// count.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);

const TERM: &str = "xterm-256color";

/// Default key files tried in "SSH agent / default keys" mode, like
/// OpenSSH and paramiko do.
const DEFAULT_KEYS: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];

/// A question for the user: do we trust this host key?
pub struct HostKeyQuestion {
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
    pub reply: oneshot::Sender<bool>,
}

/// Hands host key questions to the UI.
pub type HostKeyAsker = Arc<dyn Fn(HostKeyQuestion) + Send + Sync>;

/// Everything needed to connect that isn't stored in the profile.
pub struct Credentials {
    pub password: String,
    pub key_passphrase: String,
    /// Where trusted host keys are recorded; normally [`known_hosts_path`].
    pub known_hosts: PathBuf,
}

pub fn known_hosts_path() -> PathBuf {
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

fn client_config() -> client::Config {
    let mut config = client::Config {
        preferred: preferred_algorithms(),
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
enum Error {
    Ssh(russh::Error),
    HostKeyChanged { line: usize },
    HostKeyRejected { fingerprint: String },
}

impl From<russh::Error> for Error {
    fn from(e: russh::Error) -> Self {
        Error::Ssh(e)
    }
}

struct Client {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    ask: HostKeyAsker,
    /// Set while a host key prompt is up, so the handshake timeout pauses.
    waiting_for_user: Arc<AtomicBool>,
    sink: Arc<dyn Sink>,
}

impl client::Handler for Client {
    type Error = Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            // We never offer certificate algorithms, so a server can't pick one.
            return Ok(false);
        };
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
}

fn describe(error: &Error, host: &str, known_hosts: &Path) -> String {
    match error {
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

type Session = Handle<Client>;

async fn try_password(handle: &mut Session, user: &str, password: &str) -> Result<bool, String> {
    let result =
        handle.authenticate_password(user, password).await.map_err(|e| format!("Authentication failed: {e}"))?;
    let remaining = match result {
        AuthResult::Success => return Ok(true),
        AuthResult::Failure { remaining_methods, .. } => remaining_methods,
    };
    // A lot of network gear (and PAM-based servers) only accept the
    // password through keyboard-interactive. Answer its prompts with the
    // same password, the way paramiko's fallback does.
    if remaining.contains(&MethodKind::KeyboardInteractive) {
        return try_keyboard_interactive(handle, user, password).await;
    }
    Ok(false)
}

async fn try_keyboard_interactive(handle: &mut Session, user: &str, password: &str) -> Result<bool, String> {
    let mut response = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(|e| format!("Authentication failed: {e}"))?;
    // Servers may send several rounds, including empty ones.
    for _ in 0..8 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let answers =
                    prompts.iter().map(|p| if p.echo { String::new() } else { password.to_string() }).collect();
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

async fn connect_agent() -> Option<DynAgent> {
    #[cfg(unix)]
    {
        AgentClient::connect_env().await.ok().map(|a| a.dynamic())
    }
    #[cfg(windows)]
    {
        if let Ok(agent) = AgentClient::connect_pageant().await {
            return Some(agent.dynamic());
        }
        AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await.ok().map(|a| a.dynamic())
    }
}

async fn try_agent(handle: &mut Session, user: &str) -> bool {
    let Some(mut agent) = connect_agent().await else {
        return false;
    };
    let Ok(identities) = agent.request_identities().await else {
        return false;
    };
    let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
    for identity in identities {
        let key: PublicKey = identity.public_key().into_owned();
        if let Ok(AuthResult::Success) = handle.authenticate_publickey_with(user, key, hash, &mut agent).await {
            return true;
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
        Auth::Password => try_password(handle, &user, &credentials.password).await?,
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

/// TCP connect, handshake (with host key check) and authentication.
async fn establish(
    profile: &Profile,
    credentials: &Credentials,
    ask: HostKeyAsker,
    sink: Arc<dyn Sink>,
) -> Result<Session, String> {
    let host = profile.host.trim().to_string();
    let target = (host.trim_start_matches('[').trim_end_matches(']'), profile.port);
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(target)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => return Err(format!("Could not connect to {host}: {e}")),
        Err(_) => return Err(format!("Could not connect to {host}: timed out")),
    };
    let _ = stream.set_nodelay(true);

    let waiting = Arc::new(AtomicBool::new(false));
    let handler = Client {
        host: host.clone(),
        port: profile.port,
        known_hosts: credentials.known_hosts.clone(),
        ask,
        waiting_for_user: waiting.clone(),
        sink,
    };
    let handshake = client::connect_stream(Arc::new(client_config()), stream, handler);
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
    let (link, commands) = Link::new();
    runtime.spawn(async move {
        let reason = run(profile, credentials, size, ask, sink.clone(), commands).await;
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
) -> Option<String> {
    // Keep an eye on the command queue while connecting, so closing the tab
    // cancels a slow connect and a resize during it isn't lost.
    let handle = {
        let connecting = establish(&profile, &credentials, ask, sink.clone());
        tokio::pin!(connecting);
        loop {
            tokio::select! {
                result = &mut connecting => match result {
                    Ok(handle) => break handle,
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

    loop {
        tokio::select! {
            message = channel.wait() => match message {
                Some(ChannelMsg::Data { data }) => sink.data(&data),
                Some(ChannelMsg::ExtendedData { data, .. }) => sink.data(&data),
                Some(ChannelMsg::Close) | None => {
                    return Some("Connection closed by remote host".into());
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
                Some(Command::Close) | None => {
                    let _ = channel.close().await;
                    let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
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
