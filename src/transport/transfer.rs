//! File transfer over an open SSH connection: SFTP where the far end offers
//! it, otherwise SCP, which is all a lot of network gear speaks.
//!
//! Both run on their own channel beside the shell, so there's no second
//! login, host key check or network approval, and typing stays responsive.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use russh::client::Msg;
use russh::{Channel, ChannelMsg, ChannelStream};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::FileAttributes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::ssh;

const CHUNK: usize = 64 * 1024;
/// How long the far end gets to answer a channel or subsystem request.
const REQUEST_WAIT: Duration = Duration::from_secs(15);
/// How long SCP gets to acknowledge. Generous: routers write flash slowly.
const ACK_WAIT: Duration = Duration::from_secs(120);
const SCP_HELP: &str = "SCP may be disabled on this device (Cisco IOS: `ip scp server enable`)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

/// How a transfer is getting on; shared with the UI.
#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
    outcome: Mutex<Option<Result<&'static str, String>>>,
}

impl Progress {
    /// Bytes moved so far and the expected total, which is 0 until known.
    pub fn bytes(&self) -> (u64, u64) {
        (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed))
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Moved so far as a fraction, once the total is known.
    pub fn fraction(&self) -> Option<f32> {
        let (done, total) = self.bytes();
        (total > 0).then(|| (done as f64 / total as f64).min(1.0) as f32)
    }

    /// The protocol used, or why it failed. `None` while still running.
    pub fn outcome(&self) -> Option<Result<&'static str, String>> {
        self.outcome.lock().clone()
    }
}

#[derive(Debug)]
pub struct Request {
    pub direction: Direction,
    pub local: PathBuf,
    pub remote: String,
    /// Uploads only: whether an existing remote file may be replaced.
    pub replace: bool,
    pub progress: Arc<Progress>,
    /// Goes dead when the SSH worker ends; set by that worker.
    pub(super) connection: Weak<()>,
    /// The biggest download to accept, where the device says the size first.
    pub(super) limit: Option<u64>,
}

impl Request {
    pub fn new(direction: Direction, local: PathBuf, remote: String, replace: bool) -> Request {
        Request { direction, local, remote, replace, progress: Arc::default(), connection: Weak::new(), limit: None }
    }

    fn stop_reason(&self) -> Option<&'static str> {
        if self.progress.cancel.load(Ordering::SeqCst) {
            Some("Canceled")
        } else if self.connection.strong_count() == 0 {
            Some("The connection closed")
        } else {
            None
        }
    }
}

impl Drop for Request {
    // A request that never reached a worker must not look like it's still running.
    fn drop(&mut self) {
        self.progress.outcome.lock().get_or_insert(Err("The connection closed".into()));
    }
}

/// Run `work` until it finishes or the transfer is stopped.
async fn interruptible<T>(request: &Request, work: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    let stopped = async {
        loop {
            if let Some(reason) = request.stop_reason() {
                return reason.to_string();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    tokio::select! {
        result = work => result,
        reason = stopped => Err(reason),
    }
}

/// Copy to the end of `reader`, reporting progress. Returns the byte count.
async fn copy(
    request: &Request,
    mut reader: impl AsyncRead + Unpin,
    mut writer: impl AsyncWrite + Unpin,
) -> Result<u64, String> {
    let mut buffer = vec![0; CHUNK];
    let mut copied = 0;
    loop {
        let count = interruptible(request, async {
            let count = reader.read(&mut buffer).await.map_err(|e| format!("Read failed: {e}"))?;
            writer.write_all(&buffer[..count]).await.map_err(|e| format!("Write failed: {e}"))?;
            Ok(count)
        })
        .await?;
        if count == 0 {
            writer.flush().await.map_err(|e| format!("Write failed: {e}"))?;
            return Ok(copied);
        }
        copied += count as u64;
        request.progress.done.store(copied, Ordering::Relaxed);
    }
}

pub(super) async fn open_channel(handle: &ssh::Session) -> Result<Channel<Msg>, String> {
    match tokio::time::timeout(REQUEST_WAIT, handle.channel_open_session()).await {
        Ok(Ok(channel)) => Ok(channel),
        Ok(Err(e)) => Err(format!("The device refused a second channel beside the shell: {e}")),
        Err(_) => Err("The device did not answer a request for a second channel beside the shell".into()),
    }
}

/// An SFTP session, or `None` if the far end doesn't offer it.
pub(super) async fn open_sftp(handle: &ssh::Session) -> Result<Option<SftpSession>, String> {
    let mut channel = open_channel(handle).await?;
    if channel.request_subsystem(true, "sftp").await.is_err() {
        return Ok(None);
    }
    // Waiting here can't swallow data: an SFTP server speaks only after we do.
    loop {
        match tokio::time::timeout(REQUEST_WAIT, channel.wait()).await {
            Ok(Some(ChannelMsg::Success)) => break,
            Ok(Some(ChannelMsg::Failure | ChannelMsg::Close | ChannelMsg::Eof) | None) | Err(_) => return Ok(None),
            Ok(Some(_)) => {}
        }
    }
    Ok(SftpSession::new(channel.into_stream()).await.ok())
}

/// Where a download is written until it's complete. Unique, so another
/// transfer to the same name can't write into it.
fn partial_path(local: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = local.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}-{}.snekkie-part", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    local.with_file_name(name)
}

/// Write `reader` to the partial file, closing it before returning.
async fn receive(request: &Request, partial: &Path, reader: impl AsyncRead + Unpin) -> Result<u64, String> {
    let mut file =
        tokio::fs::File::create(partial).await.map_err(|e| format!("Could not create {}: {e}", partial.display()))?;
    let copied = copy(request, reader, &mut file).await?;
    file.sync_all().await.map_err(|e| format!("Could not save {}: {e}", partial.display()))?;
    Ok(copied)
}

async fn sftp_download(request: &Request, sftp: &SftpSession, partial: &Path) -> Result<(), String> {
    let remote = sftp.open(&request.remote).await.map_err(|e| format!("Could not open {}: {e}", request.remote))?;
    let total = remote.metadata().await.ok().and_then(|m| m.size).unwrap_or(0);
    request.progress.total.store(total, Ordering::Relaxed);
    let copied = receive(request, partial, remote).await?;
    // Only a shortfall: some servers report wrong sizes for special files.
    if copied < total {
        return Err(format!("The transfer ended early, after {copied} of {total} bytes"));
    }
    Ok(())
}

/// Writes to a temporary name and renames it into place, so a failure leaves
/// any existing remote file untouched.
async fn sftp_upload(request: &Request, sftp: &SftpSession) -> Result<(), String> {
    let target = &request.remote;
    let existing = match sftp.metadata(target).await {
        Ok(metadata) => Some(metadata),
        Err(russh_sftp::client::error::Error::Status(status))
            if status.status_code == russh_sftp::protocol::StatusCode::NoSuchFile =>
        {
            None
        }
        Err(e) => return Err(format!("Could not check {target}: {e}")),
    };
    if existing.is_some() && !request.replace {
        return Err(format!("{target} already exists"));
    }
    let local = open_local(request).await?;
    let partial = format!("{target}.snekkie-part");
    let sent = async {
        let mut remote = sftp.create(&partial).await.map_err(|e| format!("Could not create {partial}: {e}"))?;
        copy(request, local, &mut remote).await?;
        remote.shutdown().await.map_err(|e| format!("Could not finish {partial}: {e}"))?;
        // shortcut: only permissions carry over to a replaced file, not its owner; preserve more if that bites.
        if let Some(permissions) = existing.as_ref().and_then(|m| m.permissions) {
            let attributes = FileAttributes { permissions: Some(permissions), ..FileAttributes::empty() };
            let _ = sftp.set_metadata(&partial, attributes).await;
        }
        Ok(())
    }
    .await;
    if let Err(error) = sent {
        let _ = sftp.remove_file(&partial).await;
        return Err(error);
    }
    if existing.is_none() {
        return sftp.rename(&partial, target).await.map_err(|e| format!("Could not rename {partial}: {e}"));
    }
    // Plain SFTP rename won't overwrite, so the old file steps aside first.
    let old = format!("{target}.snekkie-old");
    let _ = sftp.remove_file(&old).await;
    sftp.rename(target, &old).await.map_err(|e| format!("Could not replace {target}: {e}"))?;
    if let Err(e) = sftp.rename(&partial, target).await {
        let restored = sftp.rename(&old, target).await;
        let _ = sftp.remove_file(&partial).await;
        return Err(match restored {
            Ok(()) => format!("Could not replace {target}: {e}"),
            Err(_) => format!("Could not replace {target}: {e}. The original is now at {old}"),
        });
    }
    let _ = sftp.remove_file(&old).await;
    Ok(())
}

/// Open the local file for upload and record its size.
async fn open_local(request: &Request) -> Result<tokio::io::Take<tokio::fs::File>, String> {
    let failed = |e| format!("Could not read {}: {e}", request.local.display());
    let file = tokio::fs::File::open(&request.local).await.map_err(failed)?;
    let size = file.metadata().await.map_err(failed)?.len();
    request.progress.total.store(size, Ordering::Relaxed);
    Ok(file.take(size))
}

/// A remote path as one word of a remote command. IOS doesn't take quotes
/// everywhere, so plain paths stay plain.
fn scp_path(path: &str) -> Result<String, String> {
    if path.is_empty() || path.starts_with('-') || path.chars().any(char::is_control) {
        return Err(format!("SCP can't use the remote path {path:?}"));
    }
    if path.chars().all(|c| c.is_ascii_alphanumeric() || "_./:@%+=,-~".contains(c)) {
        Ok(path.to_string())
    } else {
        Ok(format!("'{}'", path.replace('\'', r"'\''")))
    }
}

/// The size from an SCP file header such as `C0644 1234 name`. The name is
/// ignored: the user chose where this goes, not the server.
fn scp_size(header: &str) -> Result<u64, String> {
    let mut fields = header.splitn(3, ' ');
    let mode = fields.next().and_then(|mode| mode.strip_prefix('C'));
    let size = fields.next().and_then(|size| size.parse().ok());
    match (mode, size, fields.next()) {
        (Some(mode), Some(size), Some(_)) if mode.chars().all(|c| c.is_digit(8)) => Ok(size),
        _ => Err(format!("The device sent an unexpected SCP reply: {header:?}")),
    }
}

/// One line from the far end, without its newline.
async fn scp_line(stream: &mut ChannelStream<Msg>) -> Result<String, String> {
    let mut line = Vec::new();
    loop {
        match stream.read_u8().await.map_err(|e| format!("The SCP channel closed: {e}. {SCP_HELP}"))? {
            b'\n' => return Ok(String::from_utf8_lossy(&line).into_owned()),
            _ if line.len() >= 4096 => return Err("The device sent an overlong SCP reply".into()),
            byte => line.push(byte),
        }
    }
}

/// Treat a first byte of 1 or 2 as an SCP error message.
async fn scp_error(stream: &mut ChannelStream<Msg>) -> String {
    match scp_line(stream).await {
        Ok(message) => format!("SCP failed: {}", message.trim()),
        Err(error) => error,
    }
}

async fn scp_ack(request: &Request, stream: &mut ChannelStream<Msg>) -> Result<(), String> {
    interruptible(request, async {
        let reply = tokio::time::timeout(ACK_WAIT, stream.read_u8())
            .await
            .map_err(|_| format!("The device stopped responding to SCP. {SCP_HELP}"))?
            .map_err(|e| format!("The SCP channel closed: {e}. {SCP_HELP}"))?;
        match reply {
            0 => Ok(()),
            1 | 2 => Err(scp_error(stream).await),
            other => Err(format!("The device sent an unexpected SCP reply: {other}")),
        }
    })
    .await
}

async fn scp_send(stream: &mut ChannelStream<Msg>, bytes: &[u8]) -> Result<(), String> {
    stream.write_all(bytes).await.map_err(|e| format!("Write failed: {e}"))?;
    stream.flush().await.map_err(|e| format!("Write failed: {e}"))
}

async fn scp_open(handle: &ssh::Session, mode: &str, remote: &str) -> Result<ChannelStream<Msg>, String> {
    let path = scp_path(remote)?;
    let channel = open_channel(handle).await?;
    channel.exec(true, format!("scp {mode} {path}")).await.map_err(|e| format!("Could not start SCP: {e}"))?;
    Ok(channel.into_stream())
}

async fn scp_download(request: &Request, handle: &ssh::Session, partial: &Path) -> Result<(), String> {
    let mut stream = scp_open(handle, "-f", &request.remote).await?;
    scp_send(&mut stream, &[0]).await?;
    let header = interruptible(request, async {
        tokio::time::timeout(ACK_WAIT, async {
            let first = stream.read_u8().await.map_err(|e| format!("The SCP channel closed: {e}. {SCP_HELP}"))?;
            match first {
                1 | 2 => Err(scp_error(&mut stream).await),
                first => Ok(format!("{}{}", first as char, scp_line(&mut stream).await?)),
            }
        })
        .await
        .map_err(|_| format!("The device stopped responding to SCP. {SCP_HELP}"))?
    })
    .await?;
    let size = scp_size(&header)?;
    if let Some(limit) = request.limit.filter(|limit| size > *limit) {
        return Err(format!("{} is over {limit} bytes. Use download_file.", request.remote));
    }
    request.progress.total.store(size, Ordering::Relaxed);
    scp_send(&mut stream, &[0]).await?;
    let copied = receive(request, partial, (&mut stream).take(size)).await?;
    if copied != size {
        return Err(format!("The transfer ended early, after {copied} of {size} bytes"));
    }
    scp_ack(request, &mut stream).await?;
    scp_send(&mut stream, &[0]).await?;
    let _ = stream.shutdown().await;
    Ok(())
}

async fn scp_upload(request: &Request, handle: &ssh::Session) -> Result<(), String> {
    if !request.replace {
        return Err(format!(
            "This device only offers SCP, which can't check whether {} already exists. \
             Allow replacing to upload anyway.",
            request.remote
        ));
    }
    let name = request.local.file_name().unwrap_or_default().to_string_lossy().into_owned();
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(format!("SCP can't send a file named {name:?}"));
    }
    let local = open_local(request).await?;
    let size = request.progress.total.load(Ordering::Relaxed);
    let mut stream = scp_open(handle, "-t", &request.remote).await?;
    scp_ack(request, &mut stream).await?;
    scp_send(&mut stream, format!("C0644 {size} {name}\n").as_bytes()).await?;
    scp_ack(request, &mut stream).await?;
    let copied = copy(request, local, &mut stream).await?;
    if copied != size {
        return Err(format!("{} changed size while uploading", request.local.display()));
    }
    scp_send(&mut stream, &[0]).await?;
    scp_ack(request, &mut stream).await?;
    let _ = stream.shutdown().await;
    Ok(())
}

async fn transfer(request: &Request, handle: &ssh::Session, partial: &Path) -> Result<&'static str, String> {
    let sftp = interruptible(request, open_sftp(handle)).await?;
    let protocol = if sftp.is_some() { "SFTP" } else { "SCP" };
    if let Some(sftp) = &sftp {
        sftp.set_timeout(ACK_WAIT.as_secs());
    }
    match (sftp, request.direction) {
        (Some(sftp), Direction::Download) => sftp_download(request, &sftp, partial).await,
        (Some(sftp), Direction::Upload) => sftp_upload(request, &sftp).await,
        (None, Direction::Download) => scp_download(request, handle, partial).await,
        (None, Direction::Upload) => scp_upload(request, handle).await,
    }?;
    Ok(protocol)
}

/// A download only replaces the chosen file once all of it has arrived.
async fn settle_download<T>(request: &Request, partial: &Path, mut outcome: Result<T, String>) -> Result<T, String> {
    if request.direction == Direction::Download {
        if outcome.is_ok()
            && let Err(e) = tokio::fs::rename(partial, &request.local).await
        {
            outcome = Err(format!("Could not save {}: {e}", request.local.display()));
        }
        if outcome.is_err() {
            let _ = tokio::fs::remove_file(partial).await;
        }
    }
    outcome
}

/// Do the transfer and record how it went.
pub(super) async fn run(handle: Arc<ssh::Session>, request: Request) {
    let partial = partial_path(&request.local);
    let outcome = transfer(&request, &handle, &partial).await;
    let outcome = settle_download(&request, &partial, outcome).await;
    *request.progress.outcome.lock() = Some(outcome);
}

/// Do the transfer over SCP, for a device known not to offer SFTP.
pub(super) async fn run_scp(handle: &ssh::Session, request: &Request) -> Result<(), String> {
    let partial = partial_path(&request.local);
    let outcome = match request.direction {
        Direction::Download => scp_download(request, handle, &partial).await,
        Direction::Upload => scp_upload(request, handle).await,
    };
    let outcome = settle_download(request, &partial, outcome).await;
    *request.progress.outcome.lock() = Some(outcome.clone().map(|()| "SCP"));
    outcome
}

/// Do the transfer on an SFTP session that's already open.
pub(super) async fn run_sftp(sftp: &SftpSession, request: &Request) -> Result<(), String> {
    let partial = partial_path(&request.local);
    let outcome = interruptible(request, async {
        match request.direction {
            Direction::Download => sftp_download(request, sftp, &partial).await,
            Direction::Upload => sftp_upload(request, sftp).await,
        }
    })
    .await;
    let outcome = settle_download(request, &partial, outcome).await;
    *request.progress.outcome.lock() = Some(outcome.clone().map(|()| "SFTP"));
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scp_paths_are_quoted_only_when_needed() {
        assert_eq!(scp_path("flash:/image.bin").unwrap(), "flash:/image.bin");
        assert_eq!(scp_path("my file").unwrap(), "'my file'");
        assert_eq!(scp_path("a'; reboot; '").unwrap(), r"'a'\''; reboot; '\'''");
        assert!(scp_path("-oProxyCommand=x").is_err());
        assert!(scp_path("a\nb").is_err());
        assert!(scp_path("").is_err());
    }

    #[test]
    fn scp_headers_give_the_size_and_reject_the_rest() {
        assert_eq!(scp_size("C0644 1234 name with spaces").unwrap(), 1234);
        assert!(scp_size("D0755 0 folder").is_err());
        assert!(scp_size("C0644 -1 name").is_err());
        assert!(scp_size("C0644 1234").is_err());
        assert!(scp_size("Cabc 1234 name").is_err());
    }

    #[test]
    fn a_request_that_never_ran_reports_failure() {
        let request = Request::new(Direction::Download, "a".into(), "b".into(), false);
        let progress = request.progress.clone();
        assert_eq!(progress.outcome(), None);
        drop(request);
        assert!(progress.outcome().unwrap().is_err());
    }
}
