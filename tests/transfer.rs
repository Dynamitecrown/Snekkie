//! File transfers against an in-process SSH server with an in-memory disk,
//! so they run anywhere. It offers SFTP and SCP, only SCP, or neither.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Event, Recorder};
use russh::server::{self, Auth, Msg, Session};
use russh::{Channel, ChannelId, ChannelStream, MethodKind, MethodSet};
use russh_sftp::protocol::{Attrs, Data, FileAttributes, Handle, OpenFlags, Status, StatusCode};
use snekkie::profiles::Profile;
use snekkie::transport::transfer::{Direction, Request};
use snekkie::transport::{Command, Link, Sink, ssh};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const WAIT: Duration = Duration::from_secs(20);

/// Throwaway key for the test server only.
const HOST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACAIdCtIeTKkWpGPwZ1WGfgEmJVkMCDSbhvvS00tyosB7AAAAJgK59adCufW
nQAAAAtzc2gtZWQyNTUxOQAAACAIdCtIeTKkWpGPwZ1WGfgEmJVkMCDSbhvvS00tyosB7A
AAAEAsQ81aU+QfOwsOXZ+Q/59IP1AZ8y/yUHTk0ga7xgtfugh0K0h5MqRakY/BnVYZ+ASY
lWQwINJuG+9LTS3KiwHsAAAAE3NuZWtraWUgdGVzdCBzZXJ2ZXIBAg==
-----END OPENSSH PRIVATE KEY-----
";

type Disk = Arc<Mutex<HashMap<String, Vec<u8>>>>;

#[derive(Clone, Copy, PartialEq)]
enum Offers {
    SftpAndScp,
    ScpOnly,
    Nothing,
}

struct Sftp {
    disk: Disk,
}

fn ok(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

impl Sftp {
    fn attrs(&self, id: u32, name: &str) -> Result<Attrs, StatusCode> {
        let disk = self.disk.lock().unwrap();
        let file = disk.get(name).ok_or(StatusCode::NoSuchFile)?;
        let attrs =
            FileAttributes { size: Some(file.len() as u64), permissions: Some(0o640), ..FileAttributes::empty() };
        Ok(Attrs { id, attrs })
    }
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn open(&mut self, id: u32, name: String, flags: OpenFlags, _: FileAttributes) -> Result<Handle, StatusCode> {
        let mut disk = self.disk.lock().unwrap();
        if flags.contains(OpenFlags::CREATE) {
            disk.entry(name.clone()).or_default();
        }
        let file = disk.get_mut(&name).ok_or(StatusCode::NoSuchFile)?;
        if flags.contains(OpenFlags::TRUNCATE) {
            file.clear();
        }
        Ok(Handle { id, handle: name })
    }

    async fn close(&mut self, id: u32, _: String) -> Result<Status, StatusCode> {
        Ok(ok(id))
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, StatusCode> {
        let disk = self.disk.lock().unwrap();
        let file = disk.get(&handle).ok_or(StatusCode::NoSuchFile)?;
        let start = offset as usize;
        if start >= file.len() {
            return Err(StatusCode::Eof);
        }
        let end = file.len().min(start + len as usize);
        Ok(Data { id, data: file[start..end].to_vec() })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, StatusCode> {
        let mut disk = self.disk.lock().unwrap();
        let file = disk.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        let end = offset as usize + data.len();
        if file.len() < end {
            file.resize(end, 0);
        }
        file[offset as usize..end].copy_from_slice(&data);
        Ok(ok(id))
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.attrs(id, &path)
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        self.attrs(id, &handle)
    }

    async fn setstat(&mut self, id: u32, _: String, _: FileAttributes) -> Result<Status, StatusCode> {
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, name: String) -> Result<Status, StatusCode> {
        self.disk.lock().unwrap().remove(&name).ok_or(StatusCode::NoSuchFile)?;
        Ok(ok(id))
    }

    // Refuses to overwrite, like plain SFTP v3 on OpenSSH.
    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, StatusCode> {
        let mut disk = self.disk.lock().unwrap();
        if disk.contains_key(&to) {
            return Err(StatusCode::Failure);
        }
        let file = disk.remove(&from).ok_or(StatusCode::NoSuchFile)?;
        disk.insert(to, file);
        Ok(ok(id))
    }
}

/// The far end of `scp -f`: sends a file. Two names misbehave on purpose.
async fn scp_send(mut stream: ChannelStream<Msg>, disk: Disk, path: String) {
    let _ = stream.read_u8().await;
    let stored = disk.lock().unwrap().get(&path).cloned();
    let (header, body): (String, Vec<u8>) = match (path.as_str(), stored) {
        ("truncated" | "stalled", _) => ("C0644 10 x\n".into(), b"12345".to_vec()),
        (_, Some(file)) => (format!("C0644 {} evil/../../name\n", file.len()), file),
        (_, None) => {
            let _ = stream.write_all(b"\x01scp: no such file\n").await;
            let _ = stream.shutdown().await;
            return;
        }
    };
    stream.write_all(header.as_bytes()).await.unwrap();
    let _ = stream.read_u8().await;
    stream.write_all(&body).await.unwrap();
    match path.as_str() {
        "stalled" => tokio::time::sleep(Duration::from_secs(3600)).await,
        "truncated" => {}
        _ => {
            stream.write_all(&[0]).await.unwrap();
            let _ = stream.read_u8().await;
        }
    }
    let _ = stream.shutdown().await;
}

/// The far end of `scp -t`: receives a file.
async fn scp_receive(mut stream: ChannelStream<Msg>, disk: Disk, path: String) {
    stream.write_all(&[0]).await.unwrap();
    let mut header = Vec::new();
    loop {
        match stream.read_u8().await.unwrap() {
            b'\n' => break,
            byte => header.push(byte),
        }
    }
    let header = String::from_utf8(header).unwrap();
    let size: usize = header.split(' ').nth(1).unwrap().parse().unwrap();
    stream.write_all(&[0]).await.unwrap();
    let mut file = vec![0; size];
    stream.read_exact(&mut file).await.unwrap();
    assert_eq!(stream.read_u8().await.unwrap(), 0);
    disk.lock().unwrap().insert(path, file);
    stream.write_all(&[0]).await.unwrap();
    let _ = stream.shutdown().await;
}

struct Handler {
    offers: Offers,
    disk: Disk,
    shell: Option<ChannelId>,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        self.shell = Some(channel);
        session.data(channel, "welcome\r\n")?;
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if self.shell == Some(channel) {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
    }

    async fn channel_eof(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.close(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" && self.offers == Offers::SftpAndScp {
            session.channel_success(channel)?;
            let stream = self.channels.remove(&channel).unwrap().into_stream();
            russh_sftp::server::run(stream, Sftp { disk: self.disk.clone() }).await;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.offers == Offers::Nothing {
            session.channel_failure(channel)?;
            session.close(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        let command = String::from_utf8_lossy(data).into_owned();
        let stream = self.channels.remove(&channel).unwrap().into_stream();
        let disk = self.disk.clone();
        let path = command.split(' ').nth(2).unwrap().trim_matches('\'').to_string();
        if command.starts_with("scp -f ") {
            tokio::spawn(scp_send(stream, disk, path));
        } else {
            tokio::spawn(scp_receive(stream, disk, path));
        }
        Ok(())
    }
}

struct Harness {
    runtime: tokio::runtime::Runtime,
    disk: Disk,
    recorder: Arc<Recorder>,
    link: Link,
    local: tempfile::TempDir,
}

fn connect(offers: Offers) -> Harness {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let config = Arc::new(server::Config {
        keys: vec![russh::keys::decode_secret_key(HOST_KEY, None).unwrap()],
        methods: MethodSet::from(&[MethodKind::Password][..]),
        ..Default::default()
    });
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let disk = Disk::default();
    let server_disk = disk.clone();
    runtime.spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let handler = Handler { offers, disk: server_disk.clone(), shell: None, channels: HashMap::new() };
            let config = config.clone();
            tokio::spawn(async move {
                if let Ok(session) = server::run_stream(config, socket, handler).await {
                    let _ = session.await;
                }
            });
        }
    });
    let local = tempfile::tempdir().unwrap();
    let recorder = Recorder::new();
    let profile = Profile {
        host: "127.0.0.1".into(),
        port,
        username: "admin".into(),
        auth: "password".into(),
        ..Profile::default()
    };
    let credentials = ssh::Credentials {
        password: "secret".into(),
        key_passphrase: String::new(),
        known_hosts: local.path().join("known_hosts"),
        ask_prompts: None,
        jumps: Vec::new(),
    };
    let ask: ssh::HostKeyAsker = Arc::new(|q: ssh::HostKeyQuestion| {
        let _ = q.reply.send(true);
    });
    let link = ssh::start(runtime.handle(), profile, credentials, (80, 24), ask, recorder.clone() as Arc<dyn Sink>);
    recorder.wait_event(WAIT, |e| *e == Event::Connected);
    Harness { runtime, disk, recorder, link, local }
}

impl Harness {
    fn start(&self, direction: Direction, local: &Path, remote: &str, replace: bool) -> Request {
        Request::new(direction, local.to_path_buf(), remote.to_string(), replace)
    }

    fn run(&self, direction: Direction, local: &Path, remote: &str, replace: bool) -> Result<&'static str, String> {
        let request = self.start(direction, local, remote, replace);
        let progress = request.progress.clone();
        self.link.send(Command::Transfer(request));
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(outcome) = progress.outcome() {
                return outcome;
            }
            assert!(Instant::now() < deadline, "the transfer never finished: {:?}", progress.bytes());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn file(&self, name: &str) -> Option<Vec<u8>> {
        self.disk.lock().unwrap().get(name).cloned()
    }

    fn remote_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.disk.lock().unwrap().keys().cloned().collect();
        names.sort();
        names
    }

    fn local_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.local.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "known_hosts")
            .collect();
        names.sort();
        names
    }

    fn assert_shell_still_works(&self) {
        self.link.write(b"still here".to_vec());
        self.recorder.wait_text(WAIT, "still here");
    }
}

/// Big enough to take several chunks.
fn content() -> Vec<u8> {
    (0..300_000u32).map(|i| (i % 251) as u8).collect()
}

fn round_trip(offers: Offers, expected: &str) {
    let h = connect(offers);
    let source = h.local.path().join("source.bin");
    std::fs::write(&source, content()).unwrap();
    assert_eq!(h.run(Direction::Upload, &source, "image.bin", true).unwrap(), expected);
    assert_eq!(h.file("image.bin").unwrap(), content());
    assert_eq!(h.remote_names(), ["image.bin"]);

    let saved = h.local.path().join("saved.bin");
    assert_eq!(h.run(Direction::Download, &saved, "image.bin", false).unwrap(), expected);
    assert_eq!(std::fs::read(&saved).unwrap(), content());
    assert_eq!(h.local_names(), ["saved.bin", "source.bin"]);
    h.assert_shell_still_works();
    drop(h.runtime);
}

#[test]
fn sftp_round_trip_beside_the_shell() {
    round_trip(Offers::SftpAndScp, "SFTP");
}

#[test]
fn scp_is_used_when_sftp_is_not_offered() {
    round_trip(Offers::ScpOnly, "SCP");
}

#[test]
fn sftp_only_replaces_a_remote_file_when_allowed() {
    let h = connect(Offers::SftpAndScp);
    h.disk.lock().unwrap().insert("config".into(), b"original".to_vec());
    let source = h.local.path().join("new");
    std::fs::write(&source, b"replacement").unwrap();

    let refused = h.run(Direction::Upload, &source, "config", false).unwrap_err();
    assert!(refused.contains("already exists"), "{refused}");
    assert_eq!(h.file("config").unwrap(), b"original");

    h.run(Direction::Upload, &source, "config", true).unwrap();
    assert_eq!(h.file("config").unwrap(), b"replacement");
    assert_eq!(h.remote_names(), ["config"]);
}

#[test]
fn scp_upload_needs_permission_because_it_cannot_check() {
    let h = connect(Offers::ScpOnly);
    h.disk.lock().unwrap().insert("config".into(), b"original".to_vec());
    let source = h.local.path().join("new");
    std::fs::write(&source, b"replacement").unwrap();
    let refused = h.run(Direction::Upload, &source, "config", false).unwrap_err();
    assert!(refused.contains("only offers SCP"), "{refused}");
    assert_eq!(h.file("config").unwrap(), b"original");
}

#[test]
fn a_cut_off_download_leaves_the_existing_file_alone() {
    let h = connect(Offers::ScpOnly);
    let saved = h.local.path().join("saved");
    std::fs::write(&saved, b"keep me").unwrap();
    assert!(h.run(Direction::Download, &saved, "truncated", false).is_err());
    assert_eq!(std::fs::read(&saved).unwrap(), b"keep me");
    assert_eq!(h.local_names(), ["saved"]);
}

#[test]
fn a_missing_remote_file_reports_the_device_message() {
    let h = connect(Offers::ScpOnly);
    let error = h.run(Direction::Download, &h.local.path().join("saved"), "absent", false).unwrap_err();
    assert!(error.contains("no such file"), "{error}");
    assert!(h.local_names().is_empty());
}

#[test]
fn canceling_a_stalled_download_cleans_up() {
    let h = connect(Offers::ScpOnly);
    let saved = h.local.path().join("saved");
    std::fs::write(&saved, b"keep me").unwrap();
    let request = h.start(Direction::Download, &saved, "stalled", false);
    let progress = request.progress.clone();
    h.link.send(Command::Transfer(request));
    let deadline = Instant::now() + WAIT;
    while progress.bytes().0 < 5 {
        assert!(Instant::now() < deadline, "the transfer never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(progress.bytes(), (5, 10));
    progress.cancel();
    while progress.outcome().is_none() {
        assert!(Instant::now() < deadline, "the cancel was ignored");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(progress.outcome().unwrap().unwrap_err(), "Canceled");
    assert_eq!(std::fs::read(&saved).unwrap(), b"keep me");
    assert_eq!(h.local_names(), ["saved"]);
    h.assert_shell_still_works();
}

#[test]
fn closing_the_session_stops_a_transfer() {
    let h = connect(Offers::ScpOnly);
    let saved = h.local.path().join("saved");
    let request = h.start(Direction::Download, &saved, "stalled", false);
    let progress = request.progress.clone();
    h.link.send(Command::Transfer(request));
    let deadline = Instant::now() + WAIT;
    while progress.bytes().0 < 5 {
        assert!(Instant::now() < deadline, "the transfer never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    h.link.send(Command::Close);
    while progress.outcome().is_none() {
        assert!(Instant::now() < deadline, "the transfer outlived its session");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(progress.outcome().unwrap().is_err());
    assert!(h.local_names().is_empty());
}

#[test]
fn a_device_without_either_protocol_gets_a_clear_error() {
    let h = connect(Offers::Nothing);
    let error = h.run(Direction::Download, &h.local.path().join("saved"), "file", false).unwrap_err();
    assert!(error.contains("ip scp server enable"), "{error}");
    h.assert_shell_still_works();
}

#[test]
fn a_request_after_the_session_ended_does_not_hang() {
    let h = connect(Offers::SftpAndScp);
    h.link.send(Command::Close);
    h.recorder.wait_event(WAIT, |e| matches!(e, Event::Closed(_)));
    let error = h.run(Direction::Download, &h.local.path().join("saved"), "file", false).unwrap_err();
    assert!(error.contains("connection closed"), "{error}");
}

fn agent_files(h: &Harness) -> snekkie::transport::sftp::Files {
    let (reply, shared) = tokio::sync::oneshot::channel();
    h.link.send(Command::ShareConnection(reply));
    h.runtime.block_on(async { snekkie::transport::sftp::Files::open(shared.await.unwrap()).await.unwrap() })
}

#[test]
fn agent_file_tools_use_scp_on_a_device_without_sftp() {
    let h = connect(Offers::ScpOnly);
    let files = agent_files(&h);
    h.runtime.block_on(async {
        // Sending can't check what's there, so it must be asked for.
        let refused = files.write("flash:/config", b"hostname a", false).await.unwrap_err();
        assert!(refused.contains("overwrite"), "{refused}");
        assert!(h.file("flash:/config").is_none());
        files.write("flash:/config", b"hostname a", true).await.unwrap();
        assert_eq!(h.file("flash:/config").unwrap(), b"hostname a");

        assert_eq!(files.read("flash:/config", 100).await.unwrap(), b"hostname a");
        let too_big = files.read("flash:/config", 4).await.unwrap_err();
        assert!(too_big.contains("over 4 bytes"), "{too_big}");
        let missing = files.read("flash:/none", 100).await.unwrap_err();
        assert!(missing.contains("no such file"), "{missing}");

        // A cut-off read is an error, not half a file.
        assert!(files.read("truncated", 100).await.is_err());

        let saved = h.local.path().join("saved");
        files.download("flash:/config", &saved, false).await.unwrap();
        assert_eq!(std::fs::read(&saved).unwrap(), b"hostname a");
        std::fs::write(&saved, b"mine").unwrap();
        assert!(files.download("flash:/config", &saved, false).await.is_err());
        assert_eq!(std::fs::read(&saved).unwrap(), b"mine");

        assert!(files.upload(&saved, "flash:/copy", false).await.is_err());
        files.upload(&saved, "flash:/copy", true).await.unwrap();
        assert_eq!(h.file("flash:/copy").unwrap(), b"mine");

        // What SCP can't do says what to use instead.
        for error in [
            files.list(".").await.unwrap_err(),
            files.delete("flash:/copy").await.unwrap_err(),
            files.make_folder("flash:/box").await.unwrap_err(),
        ] {
            assert!(error.contains("send_command"), "{error}");
        }
        assert_eq!(h.file("flash:/copy").unwrap(), b"mine");
    });
    h.runtime.block_on(files.close(true));
    assert_eq!(h.local_names(), ["saved"], "nothing left beside the download");
    h.assert_shell_still_works();
}

#[test]
fn agent_file_tools_explain_a_device_with_neither_protocol() {
    let h = connect(Offers::Nothing);
    let files = agent_files(&h);
    let error = h.runtime.block_on(files.read("file", 100)).unwrap_err();
    assert!(error.contains("ip scp server enable"), "{error}");
    h.runtime.block_on(files.close(false));
    h.assert_shell_still_works();
}
