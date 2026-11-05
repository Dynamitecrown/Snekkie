//! The file explorer's working parts: listing and changing remote files, and
//! a transfer queue that runs whether or not its tab is on screen.
//!
//! Names from the server are untrusted. Nothing it sends can choose where a
//! file lands on this computer: see [`remote_name_ok`] and [`local_name_ok`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, StatusCode};
use tokio::sync::{oneshot, watch};

use super::ssh::Connection;
use super::transfer::{self, Direction, Progress, Request};

/// Transfers running at once.
pub const PARALLEL: usize = 3;
const TIMEOUT_SECONDS: u64 = 120;
/// How often copies being edited are checked for a save.
const EDIT_POLL: Duration = Duration::from_millis(500);

/// Work that finishes later; the UI checks on it each frame.
pub type Task<T> = oneshot::Receiver<T>;
pub type Listing = (String, Vec<Entry>);
/// Opens a downloaded copy in the user's own app.
pub type Opener = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;
type BoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

/// One line in a file list.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    /// For a symlink, whether its target is a folder.
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: Option<u64>,
    /// Permission bits only. Local files don't report them.
    pub permissions: Option<u32>,
}

/// Whether a server-sent name is one file in the folder it was listed in.
pub fn remote_name_ok(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// Whether a server-sent name can be saved here without landing somewhere
/// else, such as `..\..\startup.bat` or the alternate stream in `a.txt:b`.
pub fn local_name_ok(name: &str) -> bool {
    if !remote_name_ok(name) || name.chars().any(char::is_control) {
        return false;
    }
    if !cfg!(windows) {
        return true;
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| c.is_ascii_digit()));
    !reserved && !name.contains(['\\', ':', '*', '?', '"', '<', '>', '|']) && !name.ends_with(['.', ' '])
}

/// File types that run code instead of opening for editing. Their names come
/// from the server, so opening one could run whatever it put there.
pub fn runs_when_opened(name: &str) -> bool {
    const TYPES: [&str; 34] = [
        "app",
        "appref-ms",
        "application",
        "bat",
        "cmd",
        "com",
        "command",
        "cpl",
        "desktop",
        "dll",
        "exe",
        "gadget",
        "hta",
        "inf",
        "ins",
        "isp",
        "jar",
        "js",
        "jse",
        "lnk",
        "msc",
        "msi",
        "msp",
        "pif",
        "ps1",
        "psm1",
        "reg",
        "scf",
        "scr",
        "url",
        "vbe",
        "vbs",
        "wsf",
        "wsh",
    ];
    // Windows ignores trailing dots and spaces when it picks what to do.
    let name = name.trim_end_matches(['.', ' ']).to_ascii_lowercase();
    name.rsplit_once('.').is_some_and(|(_, extension)| TYPES.contains(&extension))
}

pub fn remote_join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else if folder.ends_with('/') {
        format!("{folder}{name}")
    } else {
        format!("{folder}/{name}")
    }
}

pub fn remote_parent(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
    }
}

/// What kind of thing the mode bits describe. The library's own checks test
/// single bits, so they call a socket a folder and a symlink a regular file.
fn file_kind(attributes: &FileAttributes) -> u32 {
    attributes.permissions.unwrap_or(0) & 0o170000
}

fn is_folder(attributes: &FileAttributes) -> bool {
    file_kind(attributes) == 0o040000
}

fn is_link(attributes: &FileAttributes) -> bool {
    file_kind(attributes) == 0o120000
}

fn is_plain_file(attributes: &FileAttributes) -> bool {
    file_kind(attributes) == 0o100000
}

fn entry(name: String, attributes: &FileAttributes) -> Entry {
    Entry {
        name,
        is_dir: is_folder(attributes),
        is_symlink: is_link(attributes),
        size: attributes.size.unwrap_or(0),
        modified: attributes.mtime.map(u64::from),
        permissions: attributes.permissions.map(|p| p & 0o7777),
    }
}

/// `name (1).txt`, `name (2).txt`, and so on.
fn numbered_name(name: &str, number: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} ({number}).{extension}"),
        _ => format!("{name} ({number})"),
    }
}

/// A folder on this computer. On Windows the empty path lists the drives.
pub fn list_local(path: &str) -> Result<Listing, String> {
    #[cfg(windows)]
    if path.is_empty() {
        let drives = ('A'..='Z')
            .map(|letter| format!("{letter}:\\"))
            .filter(|drive| Path::new(drive).exists())
            .map(|name| Entry { name, is_dir: true, is_symlink: false, size: 0, modified: None, permissions: None })
            .collect();
        return Ok((String::new(), drives));
    }
    let folder = std::path::absolute(if path.is_empty() { Path::new("/") } else { Path::new(path) })
        .map_err(|e| format!("Could not open {path}: {e}"))?;
    let failed = |e: std::io::Error| format!("Could not open {}: {e}", folder.display());
    let mut entries = Vec::new();
    for item in std::fs::read_dir(&folder).map_err(failed)? {
        let item = item.map_err(failed)?;
        let Ok(name) = item.file_name().into_string() else { continue };
        let Ok(link) = item.path().symlink_metadata() else { continue };
        let target = if link.is_symlink() { item.path().metadata().ok() } else { Some(link.clone()) };
        entries.push(Entry {
            name,
            is_dir: target.as_ref().is_some_and(std::fs::Metadata::is_dir),
            is_symlink: link.is_symlink(),
            size: target.as_ref().map_or(0, std::fs::Metadata::len),
            modified: target
                .and_then(|m| m.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|age| age.as_secs()),
            permissions: None,
        });
    }
    Ok((folder.to_string_lossy().into_owned(), entries))
}

/// The folder above a local path: on Windows, the drive list above a drive.
pub fn local_parent(path: &str) -> String {
    match Path::new(path).parent() {
        Some(parent) => parent.to_string_lossy().into_owned(),
        None if cfg!(windows) => String::new(),
        None => path.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Overwrite,
    Skip,
    Rename,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Queued,
    Running,
    /// The target exists: waiting for the user to choose.
    Conflict,
    Done,
    Failed(String),
    Canceled,
    Skipped(String),
}

impl JobState {
    pub fn finished(&self) -> bool {
        !matches!(self, JobState::Queued | JobState::Running | JobState::Conflict)
    }
}

/// Why a job exists, beyond moving a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Plain,
    EditOpen(u64),
    EditSave(u64),
}

/// One row of the queue.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: u64,
    pub direction: Direction,
    /// What the row calls it: the path below the folder that was chosen.
    pub name: String,
    pub local: PathBuf,
    pub remote: String,
    pub state: JobState,
    pub progress: Arc<Progress>,
    purpose: Purpose,
    answer: Option<Resolution>,
}

#[derive(Default)]
struct Jobs {
    rows: Vec<Job>,
    /// The batch's "apply to all" answer. Forgotten once the queue goes idle.
    policy: Option<Resolution>,
}

/// The queue, shared between the UI and its workers.
pub struct Queue {
    jobs: Mutex<Jobs>,
    next_id: AtomicU64,
    /// Folder scans still adding jobs.
    planning: AtomicUsize,
    /// Moved on whenever a worker might have something new to do.
    changed: watch::Sender<u64>,
    stopped: AtomicBool,
    /// Bumped when a transfer finishes, so lists can be refreshed.
    finished: AtomicU64,
    /// Stops new folder scans' work, without closing the queue.
    cancel_epoch: AtomicU64,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl Queue {
    fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> Queue {
        Queue {
            jobs: Mutex::default(),
            next_id: AtomicU64::new(1),
            planning: AtomicUsize::new(0),
            changed: watch::channel(0).0,
            stopped: AtomicBool::new(false),
            finished: AtomicU64::new(0),
            cancel_epoch: AtomicU64::new(0),
            repaint,
        }
    }

    fn wake(&self) {
        self.changed.send_modify(|value| *value = value.wrapping_add(1));
        (self.repaint)();
    }

    fn push(&self, direction: Direction, name: String, local: PathBuf, remote: String, purpose: Purpose) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.jobs.lock().rows.push(Job {
            id,
            direction,
            name,
            local,
            remote,
            state: JobState::Queued,
            progress: Arc::default(),
            purpose,
            answer: None,
        });
        self.wake();
        id
    }

    fn push_finished(&self, direction: Direction, name: String, state: JobState) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.jobs.lock().rows.push(Job {
            id,
            direction,
            name,
            local: PathBuf::new(),
            remote: String::new(),
            state,
            progress: Arc::default(),
            purpose: Purpose::Plain,
            answer: None,
        });
        (self.repaint)();
    }

    fn take_next(&self) -> Option<Job> {
        let mut jobs = self.jobs.lock();
        let job = jobs.rows.iter_mut().find(|job| job.state == JobState::Queued)?;
        job.state = JobState::Running;
        Some(job.clone())
    }

    fn finish(&self, id: u64, state: JobState) {
        let mut jobs = self.jobs.lock();
        if let Some(job) = jobs.rows.iter_mut().find(|job| job.id == id) {
            job.state = state;
        }
        let idle = jobs.rows.iter().all(|job| job.state.finished());
        if idle && self.planning.load(Ordering::SeqCst) == 0 {
            jobs.policy = None;
        }
        drop(jobs);
        self.finished.fetch_add(1, Ordering::SeqCst);
        self.wake();
    }

    /// Look at the rows, without holding up the workers for long.
    pub fn with_jobs<R>(&self, read: impl FnOnce(&[Job]) -> R) -> R {
        read(&self.jobs.lock().rows)
    }

    /// Unfinished jobs, plus any folder scans that will add more.
    pub fn active(&self) -> usize {
        let running = self.jobs.lock().rows.iter().filter(|job| !job.state.finished()).count();
        running + self.planning.load(Ordering::SeqCst)
    }

    pub fn finished_count(&self) -> u64 {
        self.finished.load(Ordering::SeqCst)
    }

    /// The first job waiting for an answer.
    pub fn next_conflict(&self) -> Option<Job> {
        self.jobs.lock().rows.iter().find(|job| job.state == JobState::Conflict).cloned()
    }

    /// Answer a conflict, and with `all` every later one in this batch.
    pub fn resolve(&self, id: u64, answer: Resolution, all: bool) {
        let mut jobs = self.jobs.lock();
        if all {
            jobs.policy = Some(answer);
        }
        if let Some(job) = jobs.rows.iter_mut().find(|job| job.id == id) {
            job.answer = Some(answer);
        }
        drop(jobs);
        self.wake();
    }

    pub fn cancel(&self, id: u64) {
        let mut jobs = self.jobs.lock();
        if let Some(job) = jobs.rows.iter_mut().find(|job| job.id == id) {
            Self::cancel_job(job);
        }
        drop(jobs);
        self.wake();
    }

    fn cancel_job(job: &mut Job) {
        match job.state {
            JobState::Queued | JobState::Conflict => job.state = JobState::Canceled,
            JobState::Running => job.progress.cancel(),
            _ => {}
        }
    }

    pub fn cancel_all(&self) {
        self.cancel_epoch.fetch_add(1, Ordering::SeqCst);
        self.jobs.lock().rows.iter_mut().for_each(Self::cancel_job);
        self.wake();
    }

    /// Drop finished rows. Unfinished ones keep their place.
    pub fn clear_finished(&self) {
        self.jobs.lock().rows.retain(|job| !job.state.finished());
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.cancel_all();
    }
}

/// A copy of a remote file open in the user's own app.
#[derive(Debug, Clone, PartialEq)]
pub enum EditState {
    Opening,
    Watching,
    Uploading,
    /// Saved here, but the remote file also changed since it was downloaded.
    RemoteChanged,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Edit {
    pub id: u64,
    pub remote: String,
    pub local: PathBuf,
    pub state: EditState,
    /// How many saves have reached the server.
    pub uploads: u32,
    local_stamp: Option<(SystemTime, u64)>,
    /// A change seen on the last check: it uploads once it holds still.
    settling: Option<(SystemTime, u64)>,
    remote_stamp: Option<(Option<u32>, Option<u64>)>,
}

struct Shared {
    sftp: Arc<SftpSession>,
    queue: Queue,
    edits: Mutex<Vec<Edit>>,
    /// Where copies being edited live, created when first needed.
    edit_root: Mutex<Option<PathBuf>>,
    next_edit: AtomicU64,
    opener: Opener,
    /// Ends running transfers when the client goes away.
    alive: Arc<()>,
}

/// An explorer's connection, for an agent to borrow while it lasts.
#[derive(Clone)]
pub struct ExplorerLink(Weak<Shared>);

impl ExplorerLink {
    pub fn files(&self) -> Option<Files> {
        let shared = self.0.upgrade()?;
        Some(Files {
            sftp: Some(shared.sftp.clone()),
            alive: shared.alive.clone(),
            explorer: Some(shared),
            connection: None,
        })
    }
}

const SCP_ONLY: &str = "This device doesn't offer SFTP, so only read_file, write_file, upload_file and download_file \
                        work on it, over SCP. For the rest, use send_command, such as dir flash: on Cisco IOS.";

/// One agent call's way to a server's files. It uses an explorer's own SFTP
/// channel, so its work shows there, or else one opened beside a shell for the
/// call, or SCP on a device with no SFTP. It answers for itself rather than
/// using the queue: a queued conflict waits for someone to answer it in an
/// explorer that may not exist.
pub struct Files {
    sftp: Option<Arc<SftpSession>>,
    alive: Arc<()>,
    explorer: Option<Arc<Shared>>,
    connection: Option<Connection>,
}

impl Files {
    /// Open a channel on an SSH tab's login, or get ready to use SCP.
    pub async fn open(connection: Connection) -> Result<Files, String> {
        match transfer::open_sftp(connection.handle()).await {
            Ok(sftp) => {
                if let Some(sftp) = &sftp {
                    sftp.set_timeout(TIMEOUT_SECONDS);
                }
                Ok(Files {
                    sftp: sftp.map(Arc::new),
                    alive: Arc::new(()),
                    explorer: None,
                    connection: Some(connection),
                })
            }
            Err(error) => {
                connection.release().await;
                Err(error)
            }
        }
    }

    /// Finish up. `changed` has an explorer show the new state of things.
    pub async fn close(self, changed: bool) {
        if let Some(shared) = &self.explorer {
            if changed {
                shared.queue.finished.fetch_add(1, Ordering::SeqCst);
                (shared.queue.repaint)();
            }
            return;
        }
        if let Some(sftp) = &self.sftp {
            let _ = tokio::time::timeout(Duration::from_secs(2), sftp.close()).await;
        }
        if let Some(connection) = self.connection {
            connection.release().await;
        }
    }

    fn sftp(&self) -> Result<&SftpSession, String> {
        self.sftp.as_deref().ok_or_else(|| SCP_ONLY.to_string())
    }

    pub async fn list(&self, path: &str) -> Result<Listing, String> {
        let sftp = self.sftp()?;
        let path = sftp.canonicalize(path).await.map_err(|e| format!("Could not open {path}: {e}"))?;
        let entries = list(sftp, &path).await?;
        Ok((path, entries))
    }

    pub async fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>, String> {
        let Some(sftp) = self.sftp.as_deref() else {
            return self.read_over_scp(path, limit).await;
        };
        let stat = sftp.metadata(path).await.map_err(|e| format!("Could not read {path}: {e}"))?;
        if is_folder(&stat) {
            return Err(format!("{path} is a folder."));
        }
        let too_big = || format!("{path} is over {limit} bytes. Use download_file.");
        if stat.size.unwrap_or(0) > limit {
            return Err(too_big());
        }
        let file = sftp.open(path).await.map_err(|e| format!("Could not read {path}: {e}"))?;
        let mut bytes = Vec::new();
        // Special files can report no size, so the limit holds here too.
        tokio::io::AsyncReadExt::read_to_end(&mut tokio::io::AsyncReadExt::take(file, limit + 1), &mut bytes)
            .await
            .map_err(|e| format!("Could not read {path}: {e}"))?;
        if bytes.len() as u64 > limit {
            return Err(too_big());
        }
        Ok(bytes)
    }

    async fn read_over_scp(&self, path: &str, limit: u64) -> Result<Vec<u8>, String> {
        let folder = private_folder()?;
        let copy = folder.join("download");
        let result = async {
            self.transfer(Direction::Download, &copy, path, false, Some(limit)).await?;
            tokio::fs::read(&copy).await.map_err(|e| format!("Could not read {path}: {e}"))
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&folder).await;
        result
    }

    pub async fn write(&self, path: &str, content: &[u8], overwrite: bool) -> Result<(), String> {
        self.check_upload(path, overwrite).await?;
        let folder = private_folder()?;
        // SCP tells the far end this name, and a folder there would keep it.
        let name = path.rsplit(['/', ':']).next().filter(|name| local_name_ok(name)).unwrap_or("upload");
        let copy = folder.join(name);
        let result = match tokio::fs::write(&copy, content).await {
            Ok(()) => self.transfer(Direction::Upload, &copy, path, overwrite, None).await,
            Err(e) => Err(format!("Could not prepare {path}: {e}")),
        };
        let _ = tokio::fs::remove_dir_all(&folder).await;
        result
    }

    pub async fn upload(&self, local: &Path, remote: &str, overwrite: bool) -> Result<(), String> {
        self.check_upload(remote, overwrite).await?;
        let stat = tokio::fs::metadata(local).await.map_err(|e| format!("Could not read {}: {e}", local.display()))?;
        if !stat.is_file() {
            return Err(format!("{} isn't a file.", local.display()));
        }
        self.transfer(Direction::Upload, local, remote, overwrite, None).await
    }

    pub async fn download(&self, remote: &str, local: &Path, overwrite: bool) -> Result<(), String> {
        if let Ok(stat) = tokio::fs::symlink_metadata(local).await {
            if stat.is_dir() {
                return Err(format!("{} is a folder. Give the full path of the file.", local.display()));
            }
            if !overwrite {
                return Err(format!("{} already exists. Pass overwrite to replace it.", local.display()));
            }
        }
        self.transfer(Direction::Download, local, remote, overwrite, None).await
    }

    async fn check_upload(&self, remote: &str, overwrite: bool) -> Result<(), String> {
        let Some(sftp) = self.sftp.as_deref() else {
            return if overwrite {
                Ok(())
            } else {
                Err(format!(
                    "This device only offers SCP, which can't check whether {remote} already exists. Pass overwrite \
                     to send it anyway. It replaces any file there."
                ))
            };
        };
        match remote_exists(sftp, remote).await? {
            Some(attributes) if is_folder(&attributes) => Err(format!("{remote} is a folder.")),
            Some(_) if !overwrite => Err(format!("{remote} already exists. Pass overwrite to replace it.")),
            _ => Ok(()),
        }
    }

    async fn transfer(
        &self,
        direction: Direction,
        local: &Path,
        remote: &str,
        replace: bool,
        limit: Option<u64>,
    ) -> Result<(), String> {
        let mut request = Request::new(direction, local.to_path_buf(), remote.to_string(), replace);
        request.connection = Arc::downgrade(&self.alive);
        request.limit = limit;
        let outcome = match (&self.sftp, &self.connection) {
            (Some(sftp), _) => transfer::run_sftp(sftp, &request).await,
            (None, Some(connection)) => transfer::run_scp(connection.handle(), &request).await,
            (None, None) => Err(SCP_ONLY.to_string()),
        };
        if let Some(shared) = &self.explorer {
            let state = outcome.clone().map_or_else(JobState::Failed, |()| JobState::Done);
            let name = remote.rsplit('/').next().unwrap_or(remote).to_string();
            shared.queue.push_finished(direction, name, state);
        }
        outcome
    }

    pub async fn make_folder(&self, path: &str) -> Result<(), String> {
        self.sftp()?.create_dir(path).await.map_err(|e| format!("Could not create {path}: {e}"))
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        let sftp = self.sftp()?;
        if remote_exists(sftp, to).await?.is_some() {
            return Err(format!("{to} already exists"));
        }
        sftp.rename(from, to).await.map_err(|e| format!("Could not rename {from}: {e}"))
    }

    /// A folder must be empty, so one call can't remove a whole tree.
    pub async fn delete(&self, path: &str) -> Result<(), String> {
        let sftp = self.sftp()?;
        let Some(attributes) = remote_exists(sftp, path).await? else {
            return Err(format!("{path} doesn't exist."));
        };
        let removed = if is_folder(&attributes) { sftp.remove_dir(path).await } else { sftp.remove_file(path).await };
        removed.map_err(|e| format!("Could not delete {path}: {e}"))
    }

    /// Keeps the setuid, setgid and sticky bits, as the explorer does.
    pub async fn set_permissions(&self, path: &str, bits: u32) -> Result<(), String> {
        let sftp = self.sftp()?;
        let stat = sftp.metadata(path).await.map_err(|e| format!("Could not change {path}: {e}"))?;
        let mode = (stat.permissions.unwrap_or(0) & 0o7000) | (bits & 0o777);
        let attributes = FileAttributes { permissions: Some(mode), ..FileAttributes::empty() };
        sftp.set_metadata(path, attributes).await.map_err(|e| format!("Could not change {path}: {e}"))
    }
}

/// An open file explorer connection. Dropping it stops its transfers, deletes
/// its temporary copies and lets go of the connection.
pub struct Client {
    connection: Option<Connection>,
    shared: Arc<Shared>,
    runtime: tokio::runtime::Handle,
    /// Where the server put us, normally the home folder.
    pub home: String,
}

fn no_such_file(error: &SftpError) -> bool {
    matches!(error, SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile)
}

/// Whether an SFTP target exists, without following a symlink at the end.
async fn remote_exists(sftp: &SftpSession, path: &str) -> Result<Option<FileAttributes>, String> {
    match sftp.symlink_metadata(path).await {
        Ok(attributes) => Ok(Some(attributes)),
        Err(error) if no_such_file(&error) => Ok(None),
        Err(error) => Err(format!("Could not check {path}: {error}")),
    }
}

async fn list(sftp: &SftpSession, path: &str) -> Result<Vec<Entry>, String> {
    let items = sftp.read_dir(path).await.map_err(|e| format!("Could not open {path}: {e}"))?;
    let mut entries = Vec::new();
    for item in items {
        let name = item.file_name();
        let attributes = item.metadata();
        // A name with a slash in it could only be a hostile or broken server.
        if !remote_name_ok(&name) {
            continue;
        }
        let mut listed = entry(name, &attributes);
        if listed.is_symlink
            && let Ok(target) = sftp.metadata(remote_join(path, &listed.name)).await
        {
            listed.is_dir = is_folder(&target);
            listed.size = target.size.unwrap_or(0);
        }
        entries.push(listed);
    }
    Ok(entries)
}

/// A thing found under a folder being transferred.
#[derive(Debug)]
enum Found {
    Folder,
    File,
    Skipped(&'static str),
}

/// Everything under a remote path, parents first. Symlinks are never followed,
/// so a loop or a link out of the tree can't widen the transfer.
async fn walk_remote(queue: &Queue, sftp: &SftpSession, root: &str) -> Result<Vec<(Vec<String>, Found)>, String> {
    let epoch = queue.cancel_epoch.load(Ordering::SeqCst);
    let mut found = vec![(Vec::new(), Found::Folder)];
    let mut folders = VecDeque::from([Vec::<String>::new()]);
    while let Some(parts) = folders.pop_front() {
        if queue.cancel_epoch.load(Ordering::SeqCst) != epoch {
            return Err("Canceled".into());
        }
        let path = parts.iter().fold(root.to_string(), |path, part| remote_join(&path, part));
        let items = sftp.read_dir(&path).await.map_err(|e| format!("Could not open {path}: {e}"))?;
        for item in items {
            let name = item.file_name();
            let attributes = item.metadata();
            let mut child = parts.clone();
            child.push(name.clone());
            if !remote_name_ok(&name) {
                found.push((child, Found::Skipped("The server sent an unsafe name")));
            } else if is_link(&attributes) {
                found.push((child, Found::Skipped("Links inside folders are skipped")));
            } else if is_folder(&attributes) {
                found.push((child.clone(), Found::Folder));
                folders.push_back(child);
            } else if is_plain_file(&attributes) {
                found.push((child, Found::File));
            } else {
                found.push((child, Found::Skipped("Not a regular file")));
            }
        }
    }
    Ok(found)
}

/// Everything under a local folder, parents first, symlinks not followed.
fn walk_local(root: &Path) -> Result<Vec<(Vec<String>, Found)>, String> {
    let mut found = vec![(Vec::new(), Found::Folder)];
    let mut folders = VecDeque::from([Vec::<String>::new()]);
    while let Some(parts) = folders.pop_front() {
        let path = parts.iter().fold(root.to_path_buf(), |path, part| path.join(part));
        let items = std::fs::read_dir(&path).map_err(|e| format!("Could not open {}: {e}", path.display()))?;
        for item in items {
            let item = item.map_err(|e| format!("Could not read {}: {e}", path.display()))?;
            let Ok(name) = item.file_name().into_string() else {
                let mut child = parts.clone();
                child.push(item.file_name().to_string_lossy().into_owned());
                found.push((child, Found::Skipped("The name isn't valid text")));
                continue;
            };
            let mut child = parts.clone();
            child.push(name);
            let Ok(kind) = item.file_type() else {
                found.push((child, Found::Skipped("Could not read it")));
                continue;
            };
            if kind.is_symlink() {
                found.push((child, Found::Skipped("Links inside folders are skipped")));
            } else if kind.is_dir() {
                found.push((child.clone(), Found::Folder));
                folders.push_back(child);
            } else if kind.is_file() {
                found.push((child, Found::File));
            } else {
                found.push((child, Found::Skipped("Not a regular file")));
            }
        }
    }
    Ok(found)
}

/// Make a remote folder, which is fine if it's already there.
async fn ensure_remote_folder(sftp: &SftpSession, path: &str) -> Result<(), String> {
    let error = match sftp.create_dir(path).await {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    match sftp.metadata(path).await {
        Ok(existing) if is_folder(&existing) => Ok(()),
        Ok(_) => Err(format!("{path} exists and isn't a folder")),
        Err(_) => Err(format!("Could not create {path}: {error}")),
    }
}

/// Upload files and folders from this computer into `remote_folder`.
async fn plan_upload(shared: &Shared, paths: Vec<PathBuf>, remote_folder: &str) {
    let queue = &shared.queue;
    for path in paths {
        let shown = path.display().to_string();
        let Some(name) = path.file_name().and_then(|name| name.to_str()).map(str::to_string) else {
            queue.push_finished(Direction::Upload, shown, JobState::Failed("The name isn't valid text".into()));
            continue;
        };
        let target = remote_join(remote_folder, &name);
        let walk_path = path.clone();
        let found = tokio::task::spawn_blocking(move || -> Result<_, String> {
            let link = walk_path.symlink_metadata().map_err(|e| format!("Could not read it: {e}"))?;
            let stat = if link.is_symlink() { walk_path.metadata().ok() } else { Some(link) };
            match stat {
                Some(stat) if stat.is_dir() && walk_path.symlink_metadata().is_ok_and(|l| l.is_symlink()) => {
                    Ok(vec![(Vec::new(), Found::Skipped("Links to folders are skipped"))])
                }
                Some(stat) if stat.is_dir() => walk_local(&walk_path),
                Some(stat) if stat.is_file() => Ok(vec![(Vec::new(), Found::File)]),
                _ => Ok(vec![(Vec::new(), Found::Skipped("Not a regular file"))]),
            }
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        let found = match found {
            Ok(found) => found,
            Err(error) => {
                queue.push_finished(Direction::Upload, name, JobState::Failed(error));
                continue;
            }
        };
        for (parts, kind) in found {
            let shown = std::iter::once(name.as_str()).chain(parts.iter().map(String::as_str)).collect::<Vec<_>>();
            let shown = shown.join("/");
            let remote = parts.iter().fold(target.clone(), |path, part| remote_join(&path, part));
            let local = parts.iter().fold(path.clone(), |path, part| path.join(part));
            match kind {
                Found::Folder => {
                    if let Err(error) = ensure_remote_folder(&shared.sftp, &remote).await {
                        queue.push_finished(Direction::Upload, shown, JobState::Failed(error));
                    }
                }
                Found::File => {
                    queue.push(Direction::Upload, shown, local, remote, Purpose::Plain);
                }
                Found::Skipped(why) => {
                    queue.push_finished(Direction::Upload, shown, JobState::Skipped(why.into()));
                }
            }
        }
    }
}

/// Download remote files and folders into `local_folder`.
async fn plan_download(shared: &Shared, items: Vec<(String, Entry)>, local_folder: &Path) {
    let queue = &shared.queue;
    for (path, item) in items {
        let name = item.name.clone();
        if !local_name_ok(&name) {
            let why = "That name isn't safe to save on this computer";
            queue.push_finished(Direction::Download, name, JobState::Skipped(why.into()));
            continue;
        }
        let found = if item.is_symlink && item.is_dir {
            Ok(vec![(Vec::new(), Found::Skipped("Links to folders are skipped"))])
        } else if item.is_dir {
            walk_remote(queue, &shared.sftp, &path).await
        } else {
            Ok(vec![(Vec::new(), Found::File)])
        };
        let found = match found {
            Ok(found) => found,
            Err(error) => {
                let state = if error == "Canceled" { JobState::Canceled } else { JobState::Failed(error) };
                queue.push_finished(Direction::Download, name, state);
                continue;
            }
        };
        for (parts, kind) in found {
            let shown = std::iter::once(name.as_str()).chain(parts.iter().map(String::as_str)).collect::<Vec<_>>();
            let shown = shown.join("/");
            if !parts.iter().all(|part| local_name_ok(part)) {
                let why = "That name isn't safe to save on this computer";
                queue.push_finished(Direction::Download, shown, JobState::Skipped(why.into()));
                continue;
            }
            let remote = parts.iter().fold(path.clone(), |path, part| remote_join(&path, part));
            let local = parts.iter().fold(local_folder.join(&name), |path, part| path.join(part));
            match kind {
                Found::Folder => {
                    if let Err(error) = tokio::fs::create_dir_all(&local).await {
                        let error = format!("Could not create {}: {error}", local.display());
                        queue.push_finished(Direction::Download, shown, JobState::Failed(error));
                    }
                }
                Found::File => {
                    queue.push(Direction::Download, shown, local, remote, Purpose::Plain);
                }
                Found::Skipped(why) => {
                    queue.push_finished(Direction::Download, shown, JobState::Skipped(why.into()));
                }
            }
        }
    }
}

/// Wait for the user's answer about an existing target. `None` if the job was
/// canceled meanwhile or the explorer closed.
async fn ask_about_conflict(shared: &Shared, id: u64) -> Option<Resolution> {
    let queue = &shared.queue;
    let mut changed = queue.changed.subscribe();
    {
        let mut jobs = queue.jobs.lock();
        if let Some(policy) = jobs.policy {
            return Some(policy);
        }
        jobs.rows.iter_mut().find(|job| job.id == id)?.state = JobState::Conflict;
    }
    (queue.repaint)();
    loop {
        {
            let mut jobs = queue.jobs.lock();
            let policy = jobs.policy;
            let job = jobs.rows.iter_mut().find(|job| job.id == id)?;
            if job.state != JobState::Conflict {
                return None;
            }
            if let Some(answer) = job.answer.take().or(policy) {
                job.state = JobState::Running;
                return Some(answer);
            }
        }
        if queue.stopped.load(Ordering::SeqCst) || changed.changed().await.is_err() {
            return None;
        }
    }
}

/// The first free `name (n).ext` beside `path`, on the side being written to.
async fn free_name(shared: &Shared, job: &Job) -> Result<(PathBuf, String), String> {
    for number in 1..=9999 {
        match job.direction {
            Direction::Upload => {
                let folder = remote_parent(&job.remote);
                let name = job.remote.rsplit('/').next().unwrap_or_default();
                let candidate = remote_join(&folder, &numbered_name(name, number));
                if remote_exists(&shared.sftp, &candidate).await?.is_none() {
                    return Ok((job.local.clone(), candidate));
                }
            }
            Direction::Download => {
                let name = job.local.file_name().unwrap_or_default().to_string_lossy();
                let candidate = job.local.with_file_name(numbered_name(&name, number));
                if tokio::fs::symlink_metadata(&candidate).await.is_err() {
                    return Ok((candidate, job.remote.clone()));
                }
            }
        }
    }
    Err("No free name was found".into())
}

async fn run_job(shared: &Shared, mut job: Job) -> JobState {
    // Edited copies are Snekkie's own, and saving back is what the user asked for.
    let forced = job.purpose != Purpose::Plain;
    let existing = match job.direction {
        Direction::Upload => match remote_exists(&shared.sftp, &job.remote).await {
            Ok(existing) => existing.map(|attributes| is_folder(&attributes)),
            Err(error) => return JobState::Failed(error),
        },
        Direction::Download => tokio::fs::symlink_metadata(&job.local).await.ok().map(|stat| stat.is_dir()),
    };
    if existing == Some(true) {
        return JobState::Failed("A folder with that name is already there".into());
    }
    let mut replace = forced;
    if existing.is_some() && !forced {
        match ask_about_conflict(shared, job.id).await {
            None => return JobState::Canceled,
            Some(Resolution::Skip) => return JobState::Skipped("Already there".into()),
            Some(Resolution::Overwrite) => replace = true,
            Some(Resolution::Rename) => match free_name(shared, &job).await {
                Ok((local, remote)) => {
                    job.local = local;
                    job.remote = remote;
                }
                Err(error) => return JobState::Failed(error),
            },
        }
    }
    let mut request = Request::new(job.direction, job.local.clone(), job.remote.clone(), replace);
    request.progress = job.progress.clone();
    request.connection = Arc::downgrade(&shared.alive);
    match transfer::run_sftp(&shared.sftp, &request).await {
        Ok(()) => JobState::Done,
        Err(reason) if reason == "Canceled" => JobState::Canceled,
        Err(reason) => JobState::Failed(reason),
    }
}

fn local_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let stat = path.metadata().ok()?;
    Some((stat.modified().ok()?, stat.len()))
}

async fn remote_stamp(sftp: &SftpSession, path: &str) -> Result<(Option<u32>, Option<u64>), String> {
    let attributes = sftp.metadata(path).await.map_err(|e| format!("Could not check {path}: {e}"))?;
    Ok((attributes.mtime, attributes.size))
}

/// What a finished job means for the copy being edited.
async fn after_edit_job(shared: &Arc<Shared>, job: &Job, state: &JobState) {
    let (Purpose::EditOpen(id) | Purpose::EditSave(id)) = job.purpose else { return };
    let opening = matches!(job.purpose, Purpose::EditOpen(_));
    let stamp = if *state == JobState::Done { remote_stamp(&shared.sftp, &job.remote).await.ok() } else { None };
    let local = job.local.clone();
    let new_state = match state {
        JobState::Done if opening => {
            let opener = shared.opener.clone();
            let opened = {
                let local = local.clone();
                tokio::task::spawn_blocking(move || opener(&local)).await
            };
            match opened {
                Ok(Ok(())) => EditState::Watching,
                Ok(Err(error)) => EditState::Failed(error),
                Err(error) => EditState::Failed(error.to_string()),
            }
        }
        JobState::Done => EditState::Watching,
        JobState::Failed(reason) => EditState::Failed(reason.clone()),
        _ if opening => EditState::Failed("The download didn't finish".into()),
        _ => EditState::Failed("The upload didn't finish".into()),
    };
    let mut edits = shared.edits.lock();
    if let Some(edit) = edits.iter_mut().find(|edit| edit.id == id) {
        if opening {
            edit.local_stamp = local_stamp(&local);
        } else if *state == JobState::Done {
            edit.uploads += 1;
        }
        if stamp.is_some() {
            edit.remote_stamp = stamp;
        }
        edit.state = new_state;
    }
}

async fn worker(shared: Arc<Shared>) {
    let mut changed = shared.queue.changed.subscribe();
    loop {
        if shared.queue.stopped.load(Ordering::SeqCst) {
            return;
        }
        let Some(job) = shared.queue.take_next() else {
            if changed.changed().await.is_err() {
                return;
            }
            continue;
        };
        let id = job.id;
        let state = run_job(&shared, job.clone()).await;
        after_edit_job(&shared, &job, &state).await;
        shared.queue.finish(id, state);
    }
}

/// Upload copies being edited once a save has settled.
async fn watch_edits(shared: Weak<Shared>) {
    loop {
        tokio::time::sleep(EDIT_POLL).await;
        let Some(shared) = shared.upgrade() else { return };
        if shared.queue.stopped.load(Ordering::SeqCst) {
            return;
        }
        let saved: Vec<_> = {
            let mut edits = shared.edits.lock();
            let mut saved = Vec::new();
            for edit in edits.iter_mut() {
                if !matches!(edit.state, EditState::Watching | EditState::Failed(_)) || edit.local_stamp.is_none() {
                    continue;
                }
                let now = local_stamp(&edit.local);
                if now == edit.local_stamp || now.is_none() {
                    edit.settling = None;
                } else if edit.settling == now {
                    edit.local_stamp = now;
                    edit.settling = None;
                    saved.push((edit.id, edit.remote.clone(), edit.local.clone(), edit.remote_stamp));
                } else {
                    edit.settling = now;
                }
            }
            saved
        };
        for (id, remote, local, stamp) in saved {
            let current = remote_stamp(&shared.sftp, &remote).await;
            let mut edits = shared.edits.lock();
            let Some(edit) = edits.iter_mut().find(|edit| edit.id == id) else { continue };
            match current {
                Ok(current) if Some(current) != stamp => edit.state = EditState::RemoteChanged,
                // A deleted original is put back: the save is what the user asked for.
                _ => {
                    edit.state = EditState::Uploading;
                    drop(edits);
                    upload_edit(&shared, id, local, remote);
                    continue;
                }
            }
            drop(edits);
            (shared.queue.repaint)();
        }
    }
}

fn upload_edit(shared: &Shared, id: u64, local: PathBuf, remote: String) {
    let name = remote.rsplit('/').next().unwrap_or_default().to_string();
    shared.queue.push(Direction::Upload, name, local, remote, Purpose::EditSave(id));
}

/// A new private folder, which nothing else on this computer can write into.
fn private_folder() -> Result<PathBuf, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    let mut last = String::new();
    for _ in 0..16 {
        let stamp = SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |age| age.subsec_nanos());
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("snekkie-edit-{}-{number}-{stamp:08x}", std::process::id()));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(error) => last = error.to_string(),
        }
    }
    Err(format!("Could not create a temporary folder: {last}"))
}

impl Client {
    /// Start SFTP on `connection`. `Ok(None)` means the server doesn't offer it.
    pub fn open(
        connection: Connection,
        runtime: &tokio::runtime::Handle,
        repaint: Arc<dyn Fn() + Send + Sync>,
        opener: Opener,
    ) -> Task<Result<Option<Client>, String>> {
        let (reply, task) = oneshot::channel();
        let handle = runtime.clone();
        runtime.spawn(async move {
            let opened = transfer::open_sftp(connection.handle()).await;
            let result = match opened {
                Ok(Some(sftp)) => {
                    sftp.set_timeout(TIMEOUT_SECONDS);
                    match sftp.canonicalize(".").await {
                        Ok(home) => Ok(Some(Client::start(connection, sftp, home, &handle, repaint, opener))),
                        Err(error) => Err(format!("The server's SFTP isn't working: {error}")),
                    }
                }
                Ok(None) => {
                    connection.release().await;
                    Ok(None)
                }
                Err(error) => {
                    connection.release().await;
                    Err(error)
                }
            };
            let _ = reply.send(result);
            // A window closed while connecting drops the reply, and so the client.
        });
        task
    }

    fn start(
        connection: Connection,
        sftp: SftpSession,
        home: String,
        runtime: &tokio::runtime::Handle,
        repaint: Arc<dyn Fn() + Send + Sync>,
        opener: Opener,
    ) -> Client {
        let shared = Arc::new(Shared {
            sftp: Arc::new(sftp),
            queue: Queue::new(repaint),
            edits: Mutex::default(),
            edit_root: Mutex::default(),
            next_edit: AtomicU64::new(1),
            opener,
            alive: Arc::new(()),
        });
        for _ in 0..PARALLEL {
            runtime.spawn(worker(shared.clone()));
        }
        runtime.spawn(watch_edits(Arc::downgrade(&shared)));
        Client { connection: Some(connection), shared, runtime: runtime.clone(), home }
    }

    pub fn queue(&self) -> &Queue {
        &self.shared.queue
    }

    pub fn agent_link(&self) -> ExplorerLink {
        ExplorerLink(Arc::downgrade(&self.shared))
    }

    pub fn is_closed(&self) -> bool {
        self.connection.as_ref().is_none_or(Connection::is_closed)
    }

    fn task<T: Send + 'static>(&self, work: impl FnOnce(Arc<Shared>) -> BoxFuture<T> + Send + 'static) -> Task<T> {
        let (reply, task) = oneshot::channel();
        let shared = self.shared.clone();
        self.runtime.spawn(async move {
            let result = work(shared.clone()).await;
            let _ = reply.send(result);
            (shared.queue.repaint)();
        });
        task
    }

    /// A folder's contents, under its full path.
    pub fn list(&self, path: String) -> Task<Result<Listing, String>> {
        self.task(move |shared| {
            Box::pin(async move {
                let path =
                    shared.sftp.canonicalize(path.as_str()).await.map_err(|e| format!("Could not open {path}: {e}"))?;
                let entries = list(&shared.sftp, &path).await?;
                Ok((path, entries))
            })
        })
    }

    /// Rename within a folder or move; refuses to replace what's there.
    pub fn rename(&self, from: String, to: String) -> Task<Result<(), String>> {
        self.task(move |shared| {
            Box::pin(async move {
                if remote_exists(&shared.sftp, &to).await?.is_some() {
                    return Err(format!("{to} already exists"));
                }
                shared
                    .sftp
                    .rename(from.as_str(), to.as_str())
                    .await
                    .map_err(|e| format!("Could not rename {from}: {e}"))
            })
        })
    }

    pub fn make_folder(&self, path: String) -> Task<Result<(), String>> {
        self.task(move |shared| {
            Box::pin(async move {
                shared.sftp.create_dir(path.as_str()).await.map_err(|e| format!("Could not create {path}: {e}"))
            })
        })
    }

    /// Set permissions. Each file keeps its own setuid, setgid and sticky bits.
    pub fn set_permissions(&self, items: Vec<(String, u32)>, permissions: u32) -> Task<Result<(), String>> {
        self.task(move |shared| {
            Box::pin(async move {
                for (path, old) in items {
                    let mode = (old & 0o7000) | (permissions & 0o777);
                    let attributes = FileAttributes { permissions: Some(mode), ..FileAttributes::empty() };
                    shared
                        .sftp
                        .set_metadata(path.as_str(), attributes)
                        .await
                        .map_err(|e| format!("Could not change {path}: {e}"))?;
                }
                Ok(())
            })
        })
    }

    /// Work out what deleting these would remove, without removing anything.
    pub fn plan_delete(&self, items: Vec<(String, Entry)>) -> Task<Result<DeletePlan, String>> {
        self.task(move |shared| {
            Box::pin(async move {
                let mut plan = DeletePlan::default();
                for (path, item) in items {
                    // A link goes, not what it points at.
                    if item.is_symlink || !item.is_dir {
                        plan.files.push(path);
                        continue;
                    }
                    for (parts, found) in walk_remote(&shared.queue, &shared.sftp, &path).await? {
                        let child = parts.iter().fold(path.clone(), |path, part| remote_join(&path, part));
                        match found {
                            Found::Folder => plan.folders.push(child),
                            Found::File | Found::Skipped("Links inside folders are skipped") => plan.files.push(child),
                            Found::Skipped("Not a regular file") => plan.files.push(child),
                            Found::Skipped(why) => return Err(format!("Can't delete {child}: {why}")),
                        }
                    }
                }
                Ok(plan)
            })
        })
    }

    /// Carry out a plan: files first, then folders from the deepest up.
    pub fn delete(&self, plan: DeletePlan) -> Task<Result<(), String>> {
        self.task(move |shared| {
            Box::pin(async move {
                for path in &plan.files {
                    shared
                        .sftp
                        .remove_file(path.as_str())
                        .await
                        .map_err(|e| format!("Could not delete {path}: {e}"))?;
                }
                for path in plan.folders.iter().rev() {
                    shared.sftp.remove_dir(path.as_str()).await.map_err(|e| format!("Could not delete {path}: {e}"))?;
                }
                Ok(())
            })
        })
    }

    /// Queue files and folders from this computer.
    pub fn upload(&self, paths: Vec<PathBuf>, remote_folder: String) {
        self.shared.queue.planning.fetch_add(1, Ordering::SeqCst);
        let shared = self.shared.clone();
        self.runtime.spawn(async move {
            plan_upload(&shared, paths, &remote_folder).await;
            shared.queue.planning.fetch_sub(1, Ordering::SeqCst);
            shared.queue.finished.fetch_add(1, Ordering::SeqCst);
            shared.queue.wake();
        });
    }

    /// Queue remote files and folders, given by full path.
    pub fn download(&self, items: Vec<(String, Entry)>, local_folder: PathBuf) {
        self.shared.queue.planning.fetch_add(1, Ordering::SeqCst);
        let shared = self.shared.clone();
        self.runtime.spawn(async move {
            plan_download(&shared, items, &local_folder).await;
            shared.queue.planning.fetch_sub(1, Ordering::SeqCst);
            shared.queue.finished.fetch_add(1, Ordering::SeqCst);
            shared.queue.wake();
        });
    }

    /// Download a private copy, open it in the user's own app, and upload it
    /// again whenever it's saved.
    pub fn edit(&self, remote: String) -> Result<(), String> {
        let name = remote.rsplit('/').next().unwrap_or_default().to_string();
        if !local_name_ok(&name) {
            return Err(format!("{name:?} isn't safe to save on this computer."));
        }
        if runs_when_opened(&name) {
            return Err(format!(
                "“{name}” would run as a program instead of opening for editing, so Snekkie won't open it. \
                 Download it and check it first if you need to."
            ));
        }
        let mut edits = self.shared.edits.lock();
        if edits.iter().any(|edit| edit.remote == remote) {
            return Err(format!("{name} is already open for editing."));
        }
        let root = {
            let mut root = self.shared.edit_root.lock();
            match root.as_ref() {
                Some(root) => root.clone(),
                None => root.insert(private_folder()?).clone(),
            }
        };
        let id = self.shared.next_edit.fetch_add(1, Ordering::Relaxed);
        let folder = root.join(id.to_string());
        std::fs::create_dir(&folder).map_err(|e| format!("Could not create {}: {e}", folder.display()))?;
        let local = folder.join(&name);
        edits.push(Edit {
            id,
            remote: remote.clone(),
            local: local.clone(),
            state: EditState::Opening,
            uploads: 0,
            local_stamp: None,
            settling: None,
            remote_stamp: None,
        });
        drop(edits);
        self.shared.queue.push(Direction::Download, name, local, remote, Purpose::EditOpen(id));
        Ok(())
    }

    pub fn edits(&self) -> Vec<Edit> {
        self.shared.edits.lock().clone()
    }

    /// The remote file changed too: replace it with the saved copy, or don't.
    pub fn resolve_edit(&self, id: u64, upload: bool) {
        let mut edits = self.shared.edits.lock();
        let Some(edit) = edits.iter_mut().find(|edit| edit.id == id && edit.state == EditState::RemoteChanged) else {
            return;
        };
        if upload {
            edit.state = EditState::Uploading;
            let (local, remote) = (edit.local.clone(), edit.remote.clone());
            drop(edits);
            upload_edit(&self.shared, id, local, remote);
        } else {
            edit.state = EditState::Watching;
        }
    }

    /// Stop watching a copy and delete it. A save made since the last upload is lost.
    pub fn stop_edit(&self, id: u64) {
        let mut edits = self.shared.edits.lock();
        if let Some(position) = edits.iter().position(|edit| edit.id == id) {
            let edit = edits.remove(position);
            if let Some(folder) = edit.local.parent() {
                let _ = std::fs::remove_dir_all(folder);
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shared.queue.stop();
        if let Some(root) = self.shared.edit_root.lock().take() {
            let _ = std::fs::remove_dir_all(root);
        }
        let sftp = self.shared.sftp.clone();
        let connection = self.connection.take();
        self.runtime.spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(2), sftp.close()).await;
            if let Some(connection) = connection {
                connection.release().await;
            }
        });
    }
}

/// What a delete will remove, counted before asking the user.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DeletePlan {
    pub files: Vec<String>,
    /// Parents first.
    pub folders: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_a_server_cannot_escape_the_chosen_folder() {
        for name in ["", ".", "..", "a/b", "../x", "/etc/passwd", "a\0b"] {
            assert!(!remote_name_ok(name), "{name:?}");
            assert!(!local_name_ok(name), "{name:?}");
        }
        for name in ["notes.txt", "with space", ".hidden", "…"] {
            assert!(remote_name_ok(name), "{name:?}");
            assert!(local_name_ok(name), "{name:?}");
        }
        assert!(!local_name_ok("line\nbreak"));
    }

    #[cfg(windows)]
    #[test]
    fn names_that_mean_something_else_on_windows_are_refused() {
        for name in [
            "..\\..\\startup.bat",
            "C:evil",
            "a.txt:stream",
            "CON",
            "nul.txt",
            "COM1",
            "lpt9.log",
            "trailing.",
            "trailing ",
            "star*",
        ] {
            assert!(!local_name_ok(name), "{name:?}");
        }
        assert!(local_name_ok("console.txt"));
        assert!(local_name_ok("COMPUTER.txt"));
    }

    #[test]
    fn program_types_are_never_opened() {
        for name in ["setup.exe", "SETUP.EXE", "run.bat", "a.txt.lnk", "x.ps1", "evil.exe.", "evil.exe ", "o.desktop"] {
            assert!(runs_when_opened(name), "{name:?}");
        }
        for name in ["config.txt", "script.sh.txt", "exe", "router.cfg"] {
            assert!(!runs_when_opened(name), "{name:?}");
        }
    }

    #[test]
    fn remote_paths_join_and_climb() {
        assert_eq!(remote_join("/a", "b"), "/a/b");
        assert_eq!(remote_join("/", "b"), "/b");
        assert_eq!(remote_join("", "b"), "b");
        assert_eq!(remote_parent("/a/b"), "/a");
        assert_eq!(remote_parent("/a/b/"), "/a");
        assert_eq!(remote_parent("/a"), "/");
        assert_eq!(remote_parent("/"), "/");
    }

    #[test]
    fn renamed_copies_keep_their_extension() {
        assert_eq!(numbered_name("a.txt", 2), "a (2).txt");
        assert_eq!(numbered_name("archive.tar.gz", 1), "archive.tar (1).gz");
        assert_eq!(numbered_name("README", 1), "README (1)");
        assert_eq!(numbered_name(".bashrc", 1), ".bashrc (1)");
    }

    #[test]
    fn walking_a_local_folder_does_not_follow_links() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/file"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/", dir.path().join("sub/up")).unwrap();
        let found = walk_local(dir.path()).unwrap();
        assert!(matches!(found[0].1, Found::Folder));
        assert!(found.iter().any(|(parts, kind)| parts == &["sub", "file"] && matches!(kind, Found::File)));
        #[cfg(unix)]
        assert!(found.iter().any(|(parts, kind)| parts == &["sub", "up"] && matches!(kind, Found::Skipped(_))));
    }

    #[test]
    fn local_lists_show_files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("file"), b"abc").unwrap();
        let (_, mut entries) = list_local(dir.path().to_str().unwrap()).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(entries[0].name, "file");
        assert_eq!(entries[0].size, 3);
        assert!(!entries[0].is_dir);
        assert!(entries[1].is_dir);
    }
}
