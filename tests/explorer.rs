//! The file explorer against an in-process SSH server with an in-memory file
//! system that has folders, links and permissions.

mod common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Event, Recorder};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use russh::server::{self, Auth, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode};
use snekkie::profiles::Profile;
use snekkie::transport::sftp::{Client, EditState, Entry, JobState, Opener, Resolution};
use snekkie::transport::ssh::{self, Connection};
use snekkie::transport::{Command, Link, Sink};
use snekkie::ui::explorer::{BannerSink, ExplorerContext, ExplorerOptions, ExplorerTab};
use snekkie::ui::{Paths, SnekkieApp};

const WAIT: Duration = Duration::from_secs(20);
const HOME: &str = "/home/admin";

/// Throwaway key for the test server only.
const HOST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACAIdCtIeTKkWpGPwZ1WGfgEmJVkMCDSbhvvS00tyosB7AAAAJgK59adCufW
nQAAAAtzc2gtZWQyNTUxOQAAACAIdCtIeTKkWpGPwZ1WGfgEmJVkMCDSbhvvS00tyosB7A
AAAEAsQ81aU+QfOwsOXZ+Q/59IP1AZ8y/yUHTk0ga7xgtfugh0K0h5MqRakY/BnVYZ+ASY
lWQwINJuG+9LTS3KiwHsAAAAE3NuZWtraWUgdGVzdCBzZXJ2ZXIBAg==
-----END OPENSSH PRIVATE KEY-----
";

#[derive(Debug, Clone)]
enum Node {
    Dir,
    File { data: Vec<u8>, mode: u32, mtime: u32 },
    Link(String),
}

#[derive(Default)]
struct Files {
    nodes: BTreeMap<String, Node>,
    /// Names a hostile server adds to every listing.
    hostile: Vec<String>,
    clock: u32,
}

type Disk = Arc<Mutex<Files>>;

fn ok(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

fn attributes(node: &Node) -> FileAttributes {
    let (size, mode, mtime) = match node {
        Node::Dir => (0, 0o040755, 100),
        Node::File { data, mode, mtime } => (data.len() as u64, 0o100000 | mode, *mtime),
        Node::Link(_) => (0, 0o120777, 100),
    };
    FileAttributes { size: Some(size), permissions: Some(mode), mtime: Some(mtime), ..FileAttributes::empty() }
}

fn parent(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/",
        Some((parent, _)) => parent,
    }
}

struct Sftp {
    disk: Disk,
    listed: HashSet<String>,
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        let path = if path == "." { HOME.to_string() } else { path };
        let file = File::dummy(path);
        Ok(Name { id, files: vec![file] })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        match self.disk.lock().unwrap().nodes.get(&path) {
            Some(Node::Dir) => {}
            _ => return Err(StatusCode::NoSuchFile),
        }
        self.listed.remove(&path);
        Ok(Handle { id, handle: path })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        if !self.listed.insert(handle.clone()) {
            return Err(StatusCode::Eof);
        }
        let disk = self.disk.lock().unwrap();
        let mut files: Vec<File> = disk
            .nodes
            .iter()
            .filter(|(path, _)| path.as_str() != "/" && parent(path) == handle)
            .map(|(path, node)| File {
                filename: path.rsplit('/').next().unwrap().to_string(),
                longname: String::new(),
                attrs: attributes(node),
            })
            .collect();
        for name in &disk.hostile {
            files.push(File {
                filename: name.clone(),
                longname: String::new(),
                attrs: attributes(&Node::File { data: b"hostile".to_vec(), mode: 0o644, mtime: 1 }),
            });
        }
        Ok(Name { id, files })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        let disk = self.disk.lock().unwrap();
        let mut node = disk.nodes.get(&path).ok_or(StatusCode::NoSuchFile)?;
        if let Node::Link(target) = node {
            node = disk.nodes.get(target).ok_or(StatusCode::NoSuchFile)?;
        }
        Ok(Attrs { id, attrs: attributes(node) })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        let disk = self.disk.lock().unwrap();
        let node = disk.nodes.get(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Attrs { id, attrs: attributes(node) })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        self.stat(id, handle).await
    }

    async fn setstat(&mut self, id: u32, path: String, attrs: FileAttributes) -> Result<Status, StatusCode> {
        let mut disk = self.disk.lock().unwrap();
        if let (Some(Node::File { mode, .. }), Some(permissions)) = (disk.nodes.get_mut(&path), attrs.permissions) {
            *mode = permissions & 0o7777;
        }
        Ok(ok(id))
    }

    async fn open(&mut self, id: u32, name: String, flags: OpenFlags, _: FileAttributes) -> Result<Handle, StatusCode> {
        let mut disk = self.disk.lock().unwrap();
        disk.clock += 1;
        let clock = disk.clock;
        if flags.contains(OpenFlags::CREATE) {
            disk.nodes.entry(name.clone()).or_insert(Node::File { data: Vec::new(), mode: 0o644, mtime: clock });
        }
        match disk.nodes.get_mut(&name) {
            Some(Node::File { data, mtime, .. }) => {
                if flags.contains(OpenFlags::TRUNCATE) {
                    data.clear();
                    *mtime = clock;
                }
            }
            _ => return Err(StatusCode::NoSuchFile),
        }
        Ok(Handle { id, handle: name })
    }

    async fn close(&mut self, id: u32, _: String) -> Result<Status, StatusCode> {
        Ok(ok(id))
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, StatusCode> {
        let disk = self.disk.lock().unwrap();
        let Some(Node::File { data, .. }) = disk.nodes.get(&handle) else { return Err(StatusCode::NoSuchFile) };
        let start = offset as usize;
        if start >= data.len() {
            return Err(StatusCode::Eof);
        }
        Ok(Data { id, data: data[start..data.len().min(start + len as usize)].to_vec() })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, bytes: Vec<u8>) -> Result<Status, StatusCode> {
        let mut disk = self.disk.lock().unwrap();
        disk.clock += 1;
        let clock = disk.clock;
        let Some(Node::File { data, mtime, .. }) = disk.nodes.get_mut(&handle) else {
            return Err(StatusCode::NoSuchFile);
        };
        let end = offset as usize + bytes.len();
        if data.len() < end {
            data.resize(end, 0);
        }
        data[offset as usize..end].copy_from_slice(&bytes);
        *mtime = clock;
        Ok(ok(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, _: FileAttributes) -> Result<Status, StatusCode> {
        let mut disk = self.disk.lock().unwrap();
        if disk.nodes.contains_key(&path) {
            return Err(StatusCode::Failure);
        }
        disk.nodes.insert(path, Node::Dir);
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        let mut disk = self.disk.lock().unwrap();
        if disk.nodes.keys().any(|child| child != "/" && parent(child) == path) {
            return Err(StatusCode::Failure);
        }
        disk.nodes.remove(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, name: String) -> Result<Status, StatusCode> {
        let mut disk = self.disk.lock().unwrap();
        match disk.nodes.get(&name) {
            Some(Node::Dir) => return Err(StatusCode::Failure),
            None => return Err(StatusCode::NoSuchFile),
            _ => {}
        }
        disk.nodes.remove(&name);
        Ok(ok(id))
    }

    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, StatusCode> {
        let mut disk = self.disk.lock().unwrap();
        if disk.nodes.contains_key(&to) {
            return Err(StatusCode::Failure);
        }
        let node = disk.nodes.remove(&from).ok_or(StatusCode::NoSuchFile)?;
        disk.nodes.insert(to, node);
        Ok(ok(id))
    }
}

struct Handler {
    sftp: bool,
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

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" && self.sftp {
            session.channel_success(channel)?;
            let stream = self.channels.remove(&channel).unwrap().into_stream();
            russh_sftp::server::run(stream, Sftp { disk: self.disk.clone(), listed: HashSet::new() }).await;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }
}

struct Server {
    runtime: tokio::runtime::Runtime,
    disk: Disk,
    port: u16,
    local: tempfile::TempDir,
    /// How many client connections have ended.
    ended: Arc<AtomicUsize>,
}

fn file(data: &[u8]) -> Node {
    Node::File { data: data.to_vec(), mode: 0o644, mtime: 50 }
}

fn server(sftp: bool) -> Server {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let config = Arc::new(server::Config {
        keys: vec![russh::keys::decode_secret_key(HOST_KEY, None).unwrap()],
        methods: MethodSet::from(&[MethodKind::Password][..]),
        ..Default::default()
    });
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let disk = Disk::default();
    {
        let mut files = disk.lock().unwrap();
        files.clock = 1000;
        for dir in ["/", "/home", HOME] {
            files.nodes.insert(dir.into(), Node::Dir);
        }
    }
    let server_disk = disk.clone();
    let ended = Arc::new(AtomicUsize::new(0));
    let server_ended = ended.clone();
    runtime.spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let handler = Handler { sftp, disk: server_disk.clone(), shell: None, channels: HashMap::new() };
            let config = config.clone();
            let ended = server_ended.clone();
            tokio::spawn(async move {
                if let Ok(session) = server::run_stream(config, socket, handler).await {
                    let _ = session.await;
                }
                ended.fetch_add(1, Ordering::SeqCst);
            });
        }
    });
    Server { runtime, disk, port, local: tempfile::tempdir().unwrap(), ended }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn opens_nothing() -> Opener {
    Arc::new(|_: &Path| Ok(()))
}

impl Server {
    fn profile(&self) -> Profile {
        Profile {
            host: "127.0.0.1".into(),
            port: self.port,
            username: "admin".into(),
            auth: "password".into(),
            kind: "sftp".into(),
            name: "lab".into(),
            ..Profile::default()
        }
    }

    fn credentials(&self) -> ssh::Credentials {
        ssh::Credentials {
            password: "secret".into(),
            key_passphrase: String::new(),
            known_hosts: self.local.path().join("known_hosts"),
            ask_prompts: None,
            jumps: Vec::new(),
        }
    }

    fn ask(&self) -> ssh::HostKeyAsker {
        Arc::new(|q: ssh::HostKeyQuestion| {
            let _ = q.reply.send(true);
        })
    }

    fn login(&self) -> tokio::sync::oneshot::Receiver<Result<Connection, String>> {
        ssh::connect(
            self.runtime.handle(),
            self.profile(),
            self.credentials(),
            self.ask(),
            Arc::new(BannerSink::default()) as Arc<dyn Sink>,
            Default::default(),
        )
    }

    fn client_on(&self, connection: Connection, opener: Opener) -> Option<Client> {
        self.runtime
            .block_on(Client::open(connection, self.runtime.handle(), Arc::new(|| {}), opener))
            .unwrap()
            .unwrap()
    }

    fn client(&self) -> Client {
        self.client_with(opens_nothing())
    }

    fn client_with(&self, opener: Opener) -> Client {
        let connection = self.runtime.block_on(self.login()).unwrap().unwrap();
        self.client_on(connection, opener).expect("the server offers SFTP")
    }

    fn put(&self, path: &str, node: Node) {
        self.disk.lock().unwrap().nodes.insert(path.into(), node);
    }

    fn data(&self, path: &str) -> Option<Vec<u8>> {
        match self.disk.lock().unwrap().nodes.get(path) {
            Some(Node::File { data, .. }) => Some(data.clone()),
            _ => None,
        }
    }

    fn exists(&self, path: &str) -> bool {
        self.disk.lock().unwrap().nodes.contains_key(path)
    }

    fn list(&self, client: &Client, path: &str) -> Vec<Entry> {
        let (_, mut entries) = self.runtime.block_on(client.list(path.into())).unwrap().unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    fn entry(&self, client: &Client, folder: &str, name: &str) -> (String, Entry) {
        let found = self.list(client, folder).into_iter().find(|entry| entry.name == name).unwrap();
        (format!("{folder}/{name}"), found)
    }

    fn write_local(&self, name: &str, data: &[u8]) -> PathBuf {
        let path = self.local.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, data).unwrap();
        path
    }
}

fn wait_idle(client: &Client) {
    wait_until("the queue to finish", || client.queue().active() == 0);
}

fn states(client: &Client) -> Vec<(String, JobState)> {
    let mut states: Vec<_> =
        client.queue().with_jobs(|jobs| jobs.iter().map(|job| (job.name.clone(), job.state.clone())).collect());
    states.sort_by(|a: &(String, JobState), b| a.0.cmp(&b.0));
    states
}

#[test]
fn folders_and_files_travel_both_ways_with_their_structure() {
    let s = server(true);
    let client = s.client();
    assert_eq!(client.home, HOME);

    let source = s.local.path().join("site");
    s.write_local("site/index.html", b"home page");
    s.write_local("site/css/main.css", b"body {}");
    std::fs::create_dir_all(source.join("empty")).unwrap();
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let single = s.write_local("big.bin", &big);

    client.upload(vec![source, single], HOME.into());
    wait_idle(&client);
    assert!(states(&client).iter().all(|(_, state)| *state == JobState::Done), "{:?}", states(&client));
    assert_eq!(s.data("/home/admin/site/index.html").unwrap(), b"home page");
    assert_eq!(s.data("/home/admin/site/css/main.css").unwrap(), b"body {}");
    assert!(s.exists("/home/admin/site/empty"));
    assert_eq!(s.data("/home/admin/big.bin").unwrap(), big);
    assert!(!s.disk.lock().unwrap().nodes.keys().any(|path| path.contains("snekkie-")));

    let back = s.local.path().join("back");
    std::fs::create_dir(&back).unwrap();
    client.queue().clear_finished();
    let items = vec![s.entry(&client, HOME, "site"), s.entry(&client, HOME, "big.bin")];
    client.download(items, back.clone());
    wait_idle(&client);
    assert!(states(&client).iter().all(|(_, state)| *state == JobState::Done), "{:?}", states(&client));
    assert_eq!(std::fs::read(back.join("site/css/main.css")).unwrap(), b"body {}");
    assert_eq!(std::fs::read(back.join("big.bin")).unwrap(), big);
    assert!(back.join("site/empty").is_dir());
}

#[test]
fn folder_transfers_do_not_follow_links_or_trust_names() {
    let s = server(true);
    s.put("/secret", Node::Dir);
    s.put("/secret/keys", file(b"private"));
    s.put("/home/admin/box", Node::Dir);
    s.put("/home/admin/box/ok.txt", file(b"fine"));
    s.put("/home/admin/box/out", Node::Link("/secret".into()));
    s.disk.lock().unwrap().hostile = vec!["../escape.txt".into(), "a/b".into(), "..".into()];
    let client = s.client();

    // Listings drop names that aren't one file in the folder.
    let names: Vec<_> = s.list(&client, "/home/admin/box").into_iter().map(|entry| entry.name).collect();
    assert_eq!(names, ["ok.txt", "out"]);

    let target = s.local.path().join("target");
    std::fs::create_dir(&target).unwrap();
    client.download(vec![s.entry(&client, HOME, "box")], target.clone());
    wait_idle(&client);
    assert_eq!(std::fs::read(target.join("box/ok.txt")).unwrap(), b"fine");
    assert!(!target.join("box/out").exists());
    assert!(!s.local.path().join("escape.txt").exists());
    assert!(!target.join("escape.txt").exists());
    let rows = states(&client);
    assert!(rows.iter().any(|(name, state)| name == "box/out" && matches!(state, JobState::Skipped(_))), "{rows:?}");
    assert!(
        rows.iter().any(|(name, state)| name.ends_with("escape.txt") && matches!(state, JobState::Skipped(_))),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|(_, state)| matches!(state, JobState::Failed(_))), "{rows:?}");
}

#[test]
fn a_hostile_name_passed_directly_is_refused() {
    let s = server(true);
    let client = s.client();
    let target = s.local.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let entry = Entry {
        name: "../outside.txt".into(),
        is_dir: false,
        is_symlink: false,
        size: 1,
        modified: None,
        permissions: None,
    };
    client.download(vec![("/x".into(), entry)], target);
    wait_idle(&client);
    assert!(!s.local.path().join("outside.txt").exists());
    assert!(matches!(states(&client)[0].1, JobState::Skipped(_)));
}

#[test]
fn an_existing_file_waits_for_an_answer() {
    let s = server(true);
    s.put("/home/admin/a.txt", file(b"old a"));
    s.put("/home/admin/b.txt", file(b"old b"));
    s.put("/home/admin/c.txt", file(b"old c"));
    s.put("/home/admin/d.txt", file(b"old d"));
    let client = s.client();
    let upload = |name: &str| client.upload(vec![s.write_local(name, format!("new {name}").as_bytes())], HOME.into());

    upload("a.txt");
    wait_until("the question", || client.queue().next_conflict().is_some());
    let job = client.queue().next_conflict().unwrap();
    assert_eq!(s.data("/home/admin/a.txt").unwrap(), b"old a", "nothing happens before the answer");
    client.queue().resolve(job.id, Resolution::Skip, false);
    wait_idle(&client);
    assert_eq!(s.data("/home/admin/a.txt").unwrap(), b"old a");

    upload("b.txt");
    wait_until("the question", || client.queue().next_conflict().is_some());
    client.queue().resolve(client.queue().next_conflict().unwrap().id, Resolution::Overwrite, false);
    wait_idle(&client);
    assert_eq!(s.data("/home/admin/b.txt").unwrap(), b"new b.txt");

    upload("c.txt");
    wait_until("the question", || client.queue().next_conflict().is_some());
    client.queue().resolve(client.queue().next_conflict().unwrap().id, Resolution::Rename, false);
    wait_idle(&client);
    assert_eq!(s.data("/home/admin/c.txt").unwrap(), b"old c");
    assert_eq!(s.data("/home/admin/c (1).txt").unwrap(), b"new c.txt");

    // Cancel leaves it alone too.
    upload("d.txt");
    wait_until("the question", || client.queue().next_conflict().is_some());
    client.queue().cancel(client.queue().next_conflict().unwrap().id);
    wait_idle(&client);
    assert_eq!(s.data("/home/admin/d.txt").unwrap(), b"old d");
}

#[test]
fn apply_to_all_answers_the_rest_of_the_batch_only() {
    let s = server(true);
    let names = ["a", "b", "c", "d", "e"];
    for name in names {
        s.put(&format!("/home/admin/{name}"), file(b"old"));
    }
    let client = s.client();
    let paths: Vec<_> = names.iter().map(|name| s.write_local(name, b"new")).collect();
    client.upload(paths, HOME.into());
    wait_until("the question", || client.queue().next_conflict().is_some());
    client.queue().resolve(client.queue().next_conflict().unwrap().id, Resolution::Overwrite, true);
    wait_idle(&client);
    for name in names {
        assert_eq!(s.data(&format!("/home/admin/{name}")).unwrap(), b"new", "{name}");
    }

    // The next batch asks again.
    client.upload(vec![s.write_local("a", b"newer")], HOME.into());
    wait_until("a new question", || client.queue().next_conflict().is_some());
    client.queue().cancel_all();
    wait_idle(&client);
    assert_eq!(s.data("/home/admin/a").unwrap(), b"new");
}

#[test]
fn downloads_ask_before_replacing_local_files() {
    let s = server(true);
    s.put("/home/admin/config", file(b"server"));
    let client = s.client();
    let local = s.write_local("config", b"mine");
    client.download(vec![s.entry(&client, HOME, "config")], s.local.path().to_path_buf());
    wait_until("the question", || client.queue().next_conflict().is_some());
    assert_eq!(std::fs::read(&local).unwrap(), b"mine");
    client.queue().resolve(client.queue().next_conflict().unwrap().id, Resolution::Rename, false);
    wait_idle(&client);
    assert_eq!(std::fs::read(&local).unwrap(), b"mine");
    assert_eq!(std::fs::read(s.local.path().join("config (1)")).unwrap(), b"server");
}

#[test]
fn deleting_counts_first_and_removes_links_not_their_targets() {
    let s = server(true);
    s.put("/keep", Node::Dir);
    s.put("/keep/precious", file(b"keep"));
    s.put("/home/admin/old", Node::Dir);
    s.put("/home/admin/old/sub", Node::Dir);
    s.put("/home/admin/old/sub/deep.txt", file(b"x"));
    s.put("/home/admin/old/top.txt", file(b"y"));
    s.put("/home/admin/old/link", Node::Link("/keep".into()));
    s.put("/home/admin/toplink", Node::Link("/keep".into()));
    let client = s.client();

    let items = vec![s.entry(&client, HOME, "old"), s.entry(&client, HOME, "toplink")];
    assert!(items[1].1.is_dir && items[1].1.is_symlink);
    let plan = s.runtime.block_on(client.plan_delete(items)).unwrap().unwrap();
    assert_eq!(plan.folders.len(), 2, "{plan:?}");
    assert_eq!(plan.files.len(), 4, "{plan:?}");
    assert!(s.exists("/home/admin/old/top.txt"), "counting removes nothing");

    s.runtime.block_on(client.delete(plan)).unwrap().unwrap();
    assert!(!s.exists("/home/admin/old"));
    assert!(!s.exists("/home/admin/toplink"));
    assert_eq!(s.data("/keep/precious").unwrap(), b"keep");
}

#[test]
fn renames_folders_and_permissions_change_the_server() {
    let s = server(true);
    s.put("/home/admin/a", file(b"a"));
    s.put("/home/admin/b", file(b"b"));
    s.put("/home/admin/setuid", Node::File { data: vec![], mode: 0o4755, mtime: 1 });
    let client = s.client();

    let refused = s.runtime.block_on(client.rename("/home/admin/a".into(), "/home/admin/b".into())).unwrap();
    assert!(refused.unwrap_err().contains("already exists"));
    assert_eq!(s.data("/home/admin/b").unwrap(), b"b");
    s.runtime.block_on(client.rename("/home/admin/a".into(), "/home/admin/c".into())).unwrap().unwrap();
    assert_eq!(s.data("/home/admin/c").unwrap(), b"a");

    s.runtime.block_on(client.make_folder("/home/admin/new".into())).unwrap().unwrap();
    assert!(s.exists("/home/admin/new"));
    assert!(s.runtime.block_on(client.make_folder("/home/admin/new".into())).unwrap().is_err());

    let items = vec![("/home/admin/b".to_string(), 0o644), ("/home/admin/setuid".to_string(), 0o4755)];
    s.runtime.block_on(client.set_permissions(items, 0o600)).unwrap().unwrap();
    let entries = s.list(&client, HOME);
    let bits = |name: &str| entries.iter().find(|entry| entry.name == name).unwrap().permissions.unwrap();
    assert_eq!(bits("b"), 0o600);
    assert_eq!(bits("setuid"), 0o4600, "each file keeps its special bits");
}

#[test]
fn saving_an_edited_copy_uploads_it() {
    let s = server(true);
    s.put("/home/admin/router.cfg", Node::File { data: b"hostname a".to_vec(), mode: 0o640, mtime: 50 });
    let opened: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let record = opened.clone();
    let client = s.client_with(Arc::new(move |path: &Path| {
        record.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }));

    client.edit("/home/admin/router.cfg".into()).unwrap();
    wait_until("the copy to open", || !opened.lock().unwrap().is_empty());
    let copy = opened.lock().unwrap()[0].clone();
    assert_eq!(std::fs::read(&copy).unwrap(), b"hostname a");
    assert!(client.edit("/home/admin/router.cfg".into()).is_err(), "one copy at a time");
    wait_until("watching", || client.edits()[0].state == EditState::Watching);

    std::fs::write(&copy, b"hostname b").unwrap();
    wait_until("the save to upload", || client.edits()[0].uploads == 1);
    assert_eq!(s.data("/home/admin/router.cfg").unwrap(), b"hostname b");
    assert_eq!(s.list(&client, HOME)[0].permissions, Some(0o640), "the file keeps its permissions");

    // Changed on the server as well: nothing is overwritten without asking.
    s.put("/home/admin/router.cfg", Node::File { data: b"theirs".to_vec(), mode: 0o640, mtime: 9999 });
    std::fs::write(&copy, b"hostname c, longer").unwrap();
    wait_until("the question", || client.edits()[0].state == EditState::RemoteChanged);
    assert_eq!(s.data("/home/admin/router.cfg").unwrap(), b"theirs");
    let id = client.edits()[0].id;
    client.resolve_edit(id, false);
    assert_eq!(client.edits()[0].state, EditState::Watching);
    assert_eq!(s.data("/home/admin/router.cfg").unwrap(), b"theirs");

    std::fs::write(&copy, b"hostname d, longer still").unwrap();
    wait_until("the question again", || client.edits()[0].state == EditState::RemoteChanged);
    client.resolve_edit(id, true);
    wait_until("the upload", || client.edits()[0].uploads == 2);
    assert_eq!(s.data("/home/admin/router.cfg").unwrap(), b"hostname d, longer still");

    client.stop_edit(id);
    assert!(client.edits().is_empty());
    assert!(!copy.exists(), "stopping deletes the copy");
}

#[test]
fn closing_the_explorer_deletes_temporary_copies() {
    let s = server(true);
    s.put("/home/admin/notes.txt", file(b"notes"));
    let opened: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let record = opened.clone();
    let client = s.client_with(Arc::new(move |path: &Path| {
        record.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }));
    client.edit("/home/admin/notes.txt".into()).unwrap();
    wait_until("the copy to open", || !opened.lock().unwrap().is_empty());
    let copy = opened.lock().unwrap()[0].clone();
    assert!(copy.exists());
    drop(client);
    assert!(!copy.exists());
}

#[test]
fn programs_are_not_opened_for_editing() {
    let s = server(true);
    s.put("/home/admin/setup.exe", file(b"MZ"));
    let opened: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let record = opened.clone();
    let client = s.client_with(Arc::new(move |path: &Path| {
        record.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }));
    let refused = client.edit("/home/admin/setup.exe".into()).unwrap_err();
    assert!(refused.contains("run as a program"), "{refused}");
    assert!(client.edits().is_empty());
    assert!(opened.lock().unwrap().is_empty());
}

#[test]
fn a_server_without_sftp_says_so() {
    let s = server(false);
    let connection = s.runtime.block_on(s.login()).unwrap().unwrap();
    let opened = s.runtime.block_on(Client::open(connection, s.runtime.handle(), Arc::new(|| {}), opens_nothing()));
    assert!(matches!(opened, Ok(Ok(None))));
}

fn shell(s: &Server) -> (Link, Arc<Recorder>) {
    let recorder = Recorder::new();
    let mut profile = s.profile();
    profile.kind = "ssh".into();
    let link =
        ssh::start(s.runtime.handle(), profile, s.credentials(), (80, 24), s.ask(), recorder.clone() as Arc<dyn Sink>);
    recorder.wait_event(WAIT, |e| *e == Event::Connected);
    (link, recorder)
}

fn share(s: &Server, link: &Link) -> Connection {
    let (reply, connection) = tokio::sync::oneshot::channel();
    link.send(Command::ShareConnection(reply));
    s.runtime.block_on(connection).unwrap()
}

#[test]
fn closing_the_shell_keeps_a_shared_explorer_working() {
    let s = server(true);
    let (link, recorder) = shell(&s);
    let connection = share(&s, &link);
    let watch = connection.clone();
    let client = s.client_on(connection, opens_nothing()).unwrap();

    s.write_local("first", b"one");
    client.upload(vec![s.local.path().join("first")], HOME.into());
    wait_idle(&client);
    assert_eq!(s.data("/home/admin/first").unwrap(), b"one");

    link.send(Command::Close);
    recorder.wait_event(WAIT, |e| matches!(e, Event::Closed(_)));
    assert!(!watch.is_closed(), "the explorer is still using it");

    s.write_local("second", b"two");
    client.upload(vec![s.local.path().join("second")], HOME.into());
    wait_idle(&client);
    assert_eq!(s.data("/home/admin/second").unwrap(), b"two");

    // The last one out hangs up.
    assert_eq!(s.ended.load(Ordering::SeqCst), 0);
    drop(watch);
    drop(client);
    wait_until("the connection to end", || s.ended.load(Ordering::SeqCst) == 1);
}

#[test]
fn closing_a_shared_explorer_leaves_the_shell_alone() {
    let s = server(true);
    let (link, recorder) = shell(&s);
    let connection = share(&s, &link);
    let client = s.client_on(connection, opens_nothing()).unwrap();
    assert!(!s.list(&client, "/").is_empty());
    drop(client);
    std::thread::sleep(Duration::from_millis(300));
    link.write(b"still here".to_vec());
    recorder.wait_text(WAIT, "still here");
    assert!(!recorder.events().iter().any(|e| matches!(e, Event::Closed(_))));
}

#[test]
fn canceling_stops_queued_and_running_transfers() {
    let s = server(true);
    let client = s.client();
    let paths: Vec<_> = (0..12).map(|i| s.write_local(&format!("f{i}"), &vec![7; 400_000])).collect();
    client.upload(paths, HOME.into());
    client.queue().cancel_all();
    wait_idle(&client);
    let rows = states(&client);
    assert!(!rows.iter().any(|(_, state)| matches!(state, JobState::Failed(_))), "{rows:?}");
    // No half-written files stay behind under the real name or a temporary one.
    assert!(!s.disk.lock().unwrap().nodes.keys().any(|path| path.contains("snekkie-part")));
}

// -- the tab itself ----------------------------------------------------------

fn tab_harness(s: &Server, profile: Profile) -> Harness<'static, ExplorerTab> {
    let context = ExplorerContext {
        runtime: s.runtime.handle().clone(),
        repaint: Arc::new(|| {}),
        opener: opens_nothing(),
        show_hidden: true,
    };
    let tab = ExplorerTab::new(1, profile, s.login(), None, context);
    Harness::builder().with_size(egui::vec2(1100.0, 700.0)).with_step_dt(0.05).build_ui_state(
        |ui, tab: &mut ExplorerTab| {
            tab.poll(ui.ctx());
            tab.ui(ui, &ExplorerOptions { keyboard: true, drops: true, interactive: true, show_hidden: true });
        },
        tab,
    )
}

fn settle(
    harness: &mut Harness<'static, ExplorerTab>,
    what: &str,
    mut done: impl FnMut(&mut Harness<'static, ExplorerTab>) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    while !done(harness) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(15));
        harness.run_steps(2);
    }
}

fn shows(harness: &Harness<'static, ExplorerTab>, label: &str) -> bool {
    harness.query_by_label(label).is_some()
}

#[test]
fn the_tab_browses_and_deletes_only_after_confirming() {
    let s = server(true);
    s.put("/home/admin/doomed.txt", file(b"bye"));
    s.put("/home/admin/logs", Node::Dir);
    s.put("/home/admin/logs/inner.log", file(b"log"));
    let local = s.local.path().to_string_lossy().into_owned();
    s.write_local("from-here.txt", b"local file");
    let mut profile = s.profile();
    profile.explorer_local = local;
    let mut harness = tab_harness(&s, profile);

    settle(&mut harness, "the remote list", |h| shows(h, "doomed.txt") && shows(h, "from-here.txt"));

    harness.get_by_label("doomed.txt").click();
    harness.run_steps(2);
    harness.get_by_label("Delete").click();
    settle(&mut harness, "the confirmation", |h| shows(h, "Delete from the server?"));
    assert!(s.exists("/home/admin/doomed.txt"), "asking removes nothing");

    // Enter must not approve something that can't be undone.
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert!(s.exists("/home/admin/doomed.txt"));

    let delete = harness.get_all_by_label("Delete").last().unwrap();
    delete.click();
    settle(&mut harness, "the file to go", |_| !s.exists("/home/admin/doomed.txt"));
    settle(&mut harness, "the list to refresh", |h| !shows(h, "doomed.txt"));

    // Double-clicking opens the folder.
    // egui counts a third quick click as a triple, so let the earlier ones age.
    harness.run_steps(15);
    harness.get_by_label("logs").click();
    harness.run_steps(1);
    harness.get_by_label("logs").click();
    settle(&mut harness, "the folder to open", |h| shows(h, "inner.log"));
}

#[test]
fn the_tab_uploads_with_the_toolbar_and_reports_it() {
    let s = server(true);
    s.write_local("send-me.txt", b"payload");
    let mut profile = s.profile();
    profile.explorer_local = s.local.path().to_string_lossy().into_owned();
    let mut harness = tab_harness(&s, profile);
    settle(&mut harness, "the local list", |h| shows(h, "send-me.txt"));
    settle(&mut harness, "the connection", |h| shows(h, "Server"));

    harness.get_by_label("send-me.txt").click();
    harness.run_steps(2);
    harness.get_by_label("Upload ➡").click();
    settle(&mut harness, "the upload", |_| s.data("/home/admin/send-me.txt").is_some());
    assert_eq!(s.data("/home/admin/send-me.txt").unwrap(), b"payload");
    settle(&mut harness, "the status", |h| shows(h, "Done"));
}

#[test]
fn the_tab_explains_a_device_without_sftp() {
    let s = server(false);
    let mut harness = tab_harness(&s, s.profile());
    settle(&mut harness, "the explanation", |h| shows(h, "This device doesn't offer SFTP"));
    assert!(harness.state().title().contains("closed"));
}

// -- inside the whole app ----------------------------------------------------

fn app_harness(dir: &tempfile::TempDir) -> Harness<'static, SnekkieApp> {
    ssh::use_known_hosts_for_tests(std::env::temp_dir().join(format!("snekkie-explorer-test-{}", std::process::id())));
    let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1200.0, 760.0))
        .with_step_dt(0.05)
        .build_eframe(move |_cc| SnekkieApp::new(paths));
    harness.run_steps(2);
    harness
}

fn app_until(
    harness: &mut Harness<'static, SnekkieApp>,
    what: &str,
    mut done: impl FnMut(&mut Harness<'static, SnekkieApp>) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    while !done(harness) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; tabs {:?}, dialogs {:?}",
            harness.state().workspace_titles(),
            harness.state().dialog_texts()
        );
        std::thread::sleep(Duration::from_millis(15));
        harness.run_steps(2);
    }
}

/// Answers the password prompt and, the first time, the host key prompt.
fn log_in(harness: &mut Harness<'static, SnekkieApp>, profile: Profile) {
    let ctx = harness.ctx.clone();
    harness.state_mut().open_session(&ctx, profile);
    app_until(harness, "the password prompt", |h| h.state().dialog_texts().iter().any(|t| t.contains("Password for")));
    harness.run_steps(2);
    harness.get_by_label("OK").click();
    harness.run_steps(2);
    app_until(harness, "the host key prompt", |h| h.query_by_label("Accept").is_some());
    harness.run_steps(4); // A new dialog settles into place first.
    harness.get_by_label("Accept").click();
    harness.run_steps(2);
}

#[test]
fn a_saved_sftp_session_opens_its_own_tab_and_terminal_commands_stay_out() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/remote-file.txt", file(b"x"));
    let mut app = app_harness(&dir);
    log_in(&mut app, s.profile());
    app_until(&mut app, "the remote list", |h| h.query_by_label("remote-file.txt").is_some());
    assert_eq!(app.state().workspace_titles().len(), 1);
    assert!(app.state().active_screen_text().is_none(), "there is no terminal here");

    // A shortcut for the terminal has nowhere to go.
    let ctx = app.ctx.clone();
    app.state_mut().paste_clipboard_text(
        &ctx, "reload
",
    );
    app.run_steps(2);
    assert!(!app.state().paste_running());

    app.key_press_modifiers(
        egui::Modifiers {
            ctrl: !cfg!(target_os = "macos"),
            mac_cmd: cfg!(target_os = "macos"),
            command: true,
            ..egui::Modifiers::NONE
        },
        egui::Key::W,
    );
    app.run_steps(2);
    assert!(app.state().workspace_titles().is_empty(), "nothing is transferring, so no question");
    wait_ended(&s, 1);
}

fn wait_ended(s: &Server, count: usize) {
    wait_until("the connection to end", || s.ended.load(Ordering::SeqCst) == count);
}

#[test]
fn an_ssh_tab_opens_an_explorer_on_the_same_login_and_either_can_close_first() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/shared.txt", file(b"x"));
    let mut app = app_harness(&dir);
    let mut profile = s.profile();
    profile.kind = "ssh".into();
    profile.name = "shell".into();
    log_in(&mut app, profile);
    app_until(&mut app, "the shell", |h| h.state().active_screen_text().is_some_and(|t| t.contains("welcome")));

    app.get_by_label("shell").click_secondary();
    app.run_steps(3);
    app.get_by_label("Open file explorer").click();
    app_until(&mut app, "the remote list", |h| h.query_by_label("shared.txt").is_some());
    assert!(app.state().dialog_texts().is_empty(), "no second login");
    assert_eq!(app.state().workspace_titles().len(), 2);
    assert!(app.state().active_screen_text().is_none());

    // Close the shell. The explorer keeps working on the same connection.
    let shell = app.state().workspace_titles().into_iter().find(|t| t.contains("shell")).unwrap();
    app.get_by_label(&shell).click_secondary();
    app.run_steps(3);
    app.get_by_label("Close").click();
    app_until(&mut app, "the confirmation", |h| !h.state().dialog_texts().is_empty());
    app.run_steps(2);
    app.get_by_label("Yes").click();
    app_until(&mut app, "the shell tab to close", |h| h.state().workspace_titles().len() == 1);
    assert_eq!(s.ended.load(Ordering::SeqCst), 0);
    s.put("/home/admin/after.txt", file(b"y"));
    app.key_press(egui::Key::F5);
    app_until(&mut app, "a fresh list", |h| h.query_by_label("after.txt").is_some());

    app.key_press_modifiers(
        egui::Modifiers {
            ctrl: !cfg!(target_os = "macos"),
            mac_cmd: cfg!(target_os = "macos"),
            command: true,
            ..egui::Modifiers::NONE
        },
        egui::Key::W,
    );
    app.run_steps(2);
    assert!(app.state().workspace_titles().is_empty());
    wait_ended(&s, 1);
}

#[test]
fn the_sidebar_form_connects_an_sftp_session() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/from-form.txt", file(b"x"));
    let mut app = app_harness(&dir);
    app.state_mut().set_form_kind(snekkie::profiles::Kind::Sftp);
    let profile = s.profile();
    let draft = app.state_mut().sidebar_draft();
    draft.host = profile.host;
    draft.port = profile.port;
    draft.username = profile.username;
    app.run_steps(2);
    app.get_by_label("Connect").click();
    app_until(&mut app, "the password prompt", |h| h.query_by_label("OK").is_some());
    app.run_steps(4); // A new dialog settles into place first.
    app.get_by_label("OK").click();
    app_until(&mut app, "the host key prompt", |h| h.query_by_label("Accept").is_some());
    app.run_steps(4); // A new dialog settles into place first.
    app.get_by_label("Accept").click();
    app_until(&mut app, "the remote list", |h| h.query_by_label("from-form.txt").is_some());
    assert_eq!(app.state().workspace_titles().len(), 1);
}

fn drag(harness: &mut Harness<'static, ExplorerTab>, from: egui::Pos2, to: egui::Pos2) {
    harness.hover_at(from);
    harness.drag_at(from);
    harness.run_steps(2);
    harness.hover_at(to);
    harness.run_steps(2);
    harness.drop_at(to);
    harness.run_steps(2);
}

#[test]
fn dragging_between_panes_transfers_the_row() {
    let s = server(true);
    s.write_local("drag-up.txt", b"going up");
    s.put("/home/admin/drag-down.txt", file(b"coming down"));
    let mut profile = s.profile();
    profile.explorer_local = s.local.path().to_string_lossy().into_owned();
    let mut harness = tab_harness(&s, profile);
    settle(&mut harness, "both lists", |h| shows(h, "drag-up.txt") && shows(h, "drag-down.txt"));

    let up = harness.get_by_label("drag-up.txt").rect().center();
    let down = harness.get_by_label("drag-down.txt").rect().center();
    drag(&mut harness, up, down);
    settle(&mut harness, "the upload", |_| s.data("/home/admin/drag-up.txt").is_some());
    assert_eq!(s.data("/home/admin/drag-up.txt").unwrap(), b"going up");

    harness.run_steps(15);
    drag(&mut harness, down, up);
    let local = s.local.path().join("drag-down.txt");
    settle(&mut harness, "the download", |_| local.exists());
    assert_eq!(std::fs::read(&local).unwrap(), b"coming down");
}

#[derive(Debug)]
struct DesktopFile(PathBuf);

impl egui::DroppedFile for DesktopFile {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        unreachable!("the explorer streams files from their path")
    }
}

#[test]
fn files_dropped_from_the_desktop_upload_to_the_server_folder() {
    let s = server(true);
    let path = s.write_local("dropped.txt", b"from the desktop");
    let mut harness = tab_harness(&s, s.profile());
    settle(&mut harness, "the connection", |h| h.state().status().0);
    harness.run_steps(2);
    harness.input_mut().dropped_files.push(Arc::new(DesktopFile(path)));
    settle(&mut harness, "the upload", |_| s.data("/home/admin/dropped.txt").is_some());
    assert_eq!(s.data("/home/admin/dropped.txt").unwrap(), b"from the desktop");
}

// -- side by side --------------------------------------------------------------

fn type_into_app(app: &mut Harness<'static, SnekkieApp>, text: &str) {
    app.input_mut().events.push(egui::Event::Text(text.into()));
    app.run_steps(2);
}

/// A shell tab with its explorer beside it.
fn split_app(s: &Server, dir: &tempfile::TempDir) -> Harness<'static, SnekkieApp> {
    let mut app = app_harness(dir);
    let mut profile = s.profile();
    profile.kind = "ssh".into();
    profile.name = "shell".into();
    log_in(&mut app, profile);
    app_until(&mut app, "the shell", |h| h.state().active_screen_text().is_some_and(|t| t.contains("welcome")));
    app.get_by_label("shell").click_secondary();
    app.run_steps(3);
    app.get_by_label("Open file explorer beside").click();
    app_until(&mut app, "the remote list", |h| h.query_by_label("beside.txt").is_some());
    app
}

fn terminal_shown(app: &Harness<'static, SnekkieApp>) -> bool {
    app.query_by_label("Terminal output").is_some()
}

#[test]
fn a_terminal_and_explorer_show_together_on_one_login_and_only_the_clicked_pane_gets_input() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/beside.txt", file(b"x"));
    let mut app = split_app(&s, &dir);
    assert!(terminal_shown(&app), "both show at once");
    assert!(app.state().dialog_texts().is_empty(), "no second login");
    assert_eq!(app.state().workspace_titles().len(), 2);

    // The explorer is active: typing goes nowhere near the device.
    assert!(app.state().active_screen_text().is_none());
    type_into_app(&mut app, "wrongplace");

    // Click the terminal: now it has the keyboard, and the explorer doesn't.
    app.get_by_label("Terminal output").click();
    app.run_steps(3);
    assert!(app.state().active_screen_text().is_some());
    type_into_app(&mut app, "rightplace");
    app_until(&mut app, "the echo", |h| h.state().active_screen_text().is_some_and(|t| t.contains("rightplace")));
    assert!(!app.state().active_screen_text().unwrap().contains("wrongplace"));

    // Delete in the terminal is for the device, not the selected file.
    app.get_by_label("beside.txt").click();
    app.run_steps(3);
    app.get_by_label("Terminal output").click();
    app.run_steps(3);
    app.key_press(egui::Key::Delete);
    app.run_steps(3);
    assert!(app.query_by_label("Delete from the server?").is_none());

    // And back: the explorer's shortcut works once it's clicked.
    app.get_by_label("beside.txt").click();
    app.run_steps(3);
    assert!(app.state().active_screen_text().is_none());
    app.key_press(egui::Key::Delete);
    app_until(&mut app, "the confirmation", |h| h.query_by_label("Delete from the server?").is_some());
    assert!(s.exists("/home/admin/beside.txt"));
}

#[test]
fn closing_one_side_gives_the_other_the_whole_area() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/beside.txt", file(b"x"));
    let mut app = split_app(&s, &dir);
    let title = app.state().workspace_titles().into_iter().find(|t| t.contains("files")).unwrap();
    app.get_by_label(&title).click_secondary();
    app.run_steps(3);
    app.get_by_label("Close").click();
    app_until(&mut app, "the explorer to close", |h| h.state().workspace_titles().len() == 1);
    app.run_steps(3);
    assert!(terminal_shown(&app));
    assert!(app.state().active_screen_text().is_some(), "the shell has the keyboard");
    assert_eq!(s.ended.load(Ordering::SeqCst), 0, "the shell still uses the connection");
}

#[test]
fn a_third_tab_shows_alone_and_the_pair_comes_back() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/beside.txt", file(b"x"));
    let mut app = split_app(&s, &dir);
    let mut other = s.profile();
    other.kind = "ssh".into();
    other.name = "second".into();
    // The same server, so only the password prompt this time.
    let ctx = app.ctx.clone();
    app.state_mut().open_session(&ctx, other);
    app_until(&mut app, "the password prompt", |h| h.query_by_label("OK").is_some());
    app.run_steps(4);
    app.get_by_label("OK").click();
    app_until(&mut app, "the second shell", |h| h.state().workspace_titles().len() == 3);
    app_until(&mut app, "the other pane to go", |h| h.query_by_label("beside.txt").is_none());

    app.get_by_label("shell").click();
    app_until(&mut app, "the pair", |h| h.query_by_label("beside.txt").is_some());
    assert!(terminal_shown(&app));

    // End it from the tab's menu: only the selected tab shows.
    app.get_by_label("shell").click_secondary();
    app.run_steps(3);
    app.get_by_label("End side by side").click();
    app_until(&mut app, "one pane", |h| h.query_by_label("beside.txt").is_none());
    assert!(terminal_shown(&app));
}

#[test]
fn two_explorers_side_by_side_take_a_desktop_drop_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/beside.txt", file(b"x"));
    let path = s.write_local("one-drop.txt", b"only once");
    let mut app = split_app(&s, &dir);
    app.get_by_label("shell").click_secondary();
    app.run_steps(3);
    app.get_by_label("Open file explorer").click();
    app_until(&mut app, "the third tab", |h| h.state().workspace_titles().len() == 3);
    let title = app.state().workspace_titles()[1].clone();
    app.get_all_by_label(&title).next().unwrap().click_secondary();
    app.run_steps(3);
    app.get_by_label_contains("Show side by side with").hover();
    app.run_steps(3);
    app.get_all_by_label(&title).last().unwrap().click();
    app_until(&mut app, "both explorers", |h| h.get_all_by_label("beside.txt").count() == 2);

    app.input_mut().dropped_files.push(Arc::new(DesktopFile(path)));
    app_until(&mut app, "the upload", |_| s.data("/home/admin/one-drop.txt").is_some());
    app.run_steps(10);
    assert!(
        app.query_by_label("That file already exists").is_none(),
        "a second pane taking the drop would have found the file already there"
    );
}

#[test]
fn dragging_the_divider_resizes_both_panes() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/beside.txt", file(b"x"));
    let mut app = split_app(&s, &dir);
    let before = app.get_by_label("Terminal output").rect().width();
    let divider = app.get_by_label("Split divider").rect().center();
    app.hover_at(divider);
    app.drag_at(divider);
    app.run_steps(2);
    let to = divider + egui::vec2(-120.0, 0.0);
    app.hover_at(to);
    app.run_steps(2);
    app.drop_at(to);
    app.run_steps(3);
    let after = app.get_by_label("Terminal output").rect().width();
    assert!((before - after - 120.0).abs() < 12.0, "{before} to {after}");
}

#[test]
fn edit_uses_the_program_chosen_in_preferences_in_an_explorer_already_open() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/router.cfg", file(b"hostname a"));
    let mut app = app_harness(&dir);
    log_in(&mut app, s.profile());
    app_until(&mut app, "the remote list", |h| h.query_by_label("router.cfg").is_some());

    // A program that can't exist: nothing launches, and the error names it.
    let keys = egui::Modifiers {
        ctrl: !cfg!(target_os = "macos"),
        mac_cmd: cfg!(target_os = "macos"),
        command: true,
        ..egui::Modifiers::NONE
    };
    app.key_press_modifiers(keys, egui::Key::Comma);
    app.run_steps(12);
    app.get_all_by_role_and_label(egui::accesskit::Role::Button, "Files").next_back().unwrap().click();
    app.run_steps(4);
    let field = app.get_by_role_and_label(egui::accesskit::Role::TextInput, "Open with");
    field.focus();
    field.type_text("no-such-editor-for-snekkie");
    app.run_steps(4);
    app.get_by_label("Save changes").click();
    app.run_steps(4);

    app.get_by_label("router.cfg").click();
    app.run_steps(2);
    app.get_all_by_role_and_label(egui::accesskit::Role::Button, "Edit").next_back().unwrap().click();
    app_until(&mut app, "the failure", |h| h.query_by_label_contains("no-such-editor-for-snekkie").is_some());
}

#[test]
fn a_chosen_program_gets_the_file_as_one_argument() {
    let dir = tempfile::tempdir().unwrap();
    let opened = dir.path().join("a copy with spaces.txt");
    // Makes the file named by its first argument, so a split path shows up.
    #[cfg(windows)]
    let program = {
        let script = dir.path().join("editor.cmd");
        std::fs::write(&script, "@type nul > \"%~1\"\r\n").unwrap();
        script.display().to_string()
    };
    #[cfg(not(windows))]
    let program = "touch".to_string();

    snekkie::ui::explorer::open_with(&format!(" \"{program}\" "), &opened).unwrap();
    wait_until("the program to run", || opened.exists());

    let error = snekkie::ui::explorer::open_with("no-such-editor-for-snekkie", &opened).unwrap_err();
    assert!(error.contains("no-such-editor-for-snekkie"), "{error}");
}

/// An app with agent access on, and file access if asked, plus how to call its tools.
fn agent_app(
    dir: &tempfile::TempDir,
    file_access: bool,
) -> (Harness<'static, SnekkieApp>, impl Fn(&str, serde_json::Value) -> (String, bool)) {
    use snekkie::settings::{AppSettings, SettingsStore};
    SettingsStore::new(dir.path().join("settings.json"))
        .save(&AppSettings {
            agent_access: true,
            agent_file_access: file_access,
            check_for_updates: false,
            ..Default::default()
        })
        .unwrap();
    let mcp = dir.path().join(snekkie::mcp::FILE_NAME);
    let call =
        move |tool: &str, arguments: serde_json::Value| snekkie::mcp::forward(&mcp, "Test", tool, &arguments).unwrap();
    (app_harness(dir), call)
}

fn tab_id(call: &impl Fn(&str, serde_json::Value) -> (String, bool), kind: &str) -> u64 {
    let (text, _) = call("list_sessions", serde_json::json!({}));
    let tabs: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| panic!("{text}"));
    let tab = tabs.as_array().unwrap().iter().find(|tab| tab["type"] == kind);
    tab.unwrap_or_else(|| panic!("no {kind} in {text}"))["session_id"].as_u64().unwrap()
}

#[test]
fn agents_work_with_files_in_an_explorer_tab_and_it_shows() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/router.cfg", Node::File { data: b"hostname a".to_vec(), mode: 0o640, mtime: 1 });
    s.put("/home/admin/blob.bin", file(&[0xff, 0xfe, 0x00]));
    let (mut app, call) = agent_app(&dir, true);
    log_in(&mut app, s.profile());
    app_until(&mut app, "the remote list", |h| h.query_by_label("router.cfg").is_some());
    app.run_steps(2);
    let id = tab_id(&call, "File explorer");
    let on = |extra: serde_json::Value| {
        let mut arguments = extra;
        arguments["session_id"] = json!(id);
        arguments
    };
    let ok = |tool: &str, extra: serde_json::Value| {
        let (text, failed) = call(tool, on(extra));
        assert!(!failed, "{tool}: {text}");
        text
    };
    let refused = |tool: &str, extra: serde_json::Value| {
        let (text, failed) = call(tool, on(extra));
        assert!(failed, "{tool} should fail: {text}");
        text
    };

    let listed: serde_json::Value = serde_json::from_str(&ok("list_files", json!({}))).unwrap();
    assert_eq!(listed["path"], HOME);
    assert!(
        listed["entries"].as_array().unwrap().iter().any(|e| e["name"] == "router.cfg" && e["permissions"] == "640")
    );

    assert_eq!(ok("read_file", json!({ "path": "/home/admin/router.cfg" })), "hostname a");
    assert!(refused("read_file", json!({ "path": "/home/admin/blob.bin" })).contains("download_file"));

    // Editing: nothing is replaced unless the agent says so, and permissions stay.
    let edit = json!({ "path": "/home/admin/router.cfg", "content": "hostname b" });
    assert!(refused("write_file", edit.clone()).contains("overwrite"));
    assert_eq!(s.data("/home/admin/router.cfg").unwrap(), b"hostname a");
    let mut replacing = edit;
    replacing["overwrite"] = json!(true);
    ok("write_file", replacing);
    assert_eq!(s.data("/home/admin/router.cfg").unwrap(), b"hostname b");
    let listed: serde_json::Value = serde_json::from_str(&ok("list_files", json!({ "path": HOME }))).unwrap();
    assert!(
        listed["entries"].as_array().unwrap().iter().any(|e| e["name"] == "router.cfg" && e["permissions"] == "640")
    );

    // The tab shows what the agent made, without being asked to refresh.
    ok("write_file", json!({ "path": "/home/admin/from-agent.txt", "content": "" }));
    assert_eq!(s.data("/home/admin/from-agent.txt").unwrap(), b"");
    app_until(&mut app, "the new file on show", |h| {
        // Its queue row and its place in the list.
        h.query_all_by_label("from-agent.txt").count() == 2
    });

    // This computer's files.
    let local = s.local.path().join("copy.cfg");
    let local_text = local.display().to_string();
    assert!(
        refused("download_file", json!({ "remote_path": "router.cfg", "local_path": "copy.cfg" }))
            .contains("full path")
    );
    let download = json!({ "remote_path": "/home/admin/router.cfg", "local_path": local_text });
    ok("download_file", download.clone());
    assert_eq!(std::fs::read(&local).unwrap(), b"hostname b");
    std::fs::write(&local, b"mine").unwrap();
    assert!(refused("download_file", download.clone()).contains("overwrite"));
    assert_eq!(std::fs::read(&local).unwrap(), b"mine");
    let upload = json!({ "local_path": local_text, "remote_path": "/home/admin/up.cfg" });
    ok("upload_file", upload.clone());
    assert_eq!(s.data("/home/admin/up.cfg").unwrap(), b"mine");
    assert!(refused("upload_file", upload).contains("overwrite"));

    // Tidying up.
    ok("make_folder", json!({ "path": "/home/admin/box" }));
    ok("rename_path", json!({ "from": "/home/admin/up.cfg", "to": "/home/admin/box/up.cfg" }));
    assert!(
        refused("rename_path", json!({ "from": "/home/admin/router.cfg", "to": "/home/admin/box/up.cfg" }))
            .contains("exists")
    );
    ok("set_permissions", json!({ "path": "/home/admin/box/up.cfg", "permissions": "600" }));
    assert!(
        refused("set_permissions", json!({ "path": "/home/admin/box/up.cfg", "permissions": "rwx" })).contains("octal")
    );
    refused("delete_path", json!({ "path": "/home/admin/box" }));
    ok("delete_path", json!({ "path": "/home/admin/box/up.cfg" }));
    ok("delete_path", json!({ "path": "/home/admin/box" }));
    assert!(s.data("/home/admin/box/up.cfg").is_none());
}

#[test]
fn agents_reach_an_ssh_tabs_files_without_disturbing_its_shell() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    let (mut app, call) = agent_app(&dir, true);
    let mut profile = s.profile();
    profile.kind = "ssh".into();
    log_in(&mut app, profile);
    app_until(&mut app, "the shell", |h| h.state().active_screen_text().is_some_and(|t| t.contains("welcome")));
    app.run_steps(2);
    let id = tab_id(&call, "SSH");
    for _ in 0..2 {
        let (text, failed) = call(
            "write_file",
            serde_json::json!({ "session_id": id, "path": "/home/admin/n.txt", "content": "hi", "overwrite": true }),
        );
        assert!(!failed, "{text}");
    }
    assert_eq!(s.data("/home/admin/n.txt").unwrap(), b"hi");
    app.run_steps(4);
    assert!(app.state().active_screen_text().is_some_and(|t| t.contains("welcome")));
    let (state, _) = call("list_sessions", serde_json::json!({}));
    assert!(state.contains("\"connected\""), "{state}");
}

#[test]
fn file_tools_stay_off_until_allowed_and_say_when_a_server_has_no_sftp() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(false);
    let (mut app, call) = agent_app(&dir, false);
    let mut profile = s.profile();
    profile.kind = "ssh".into();
    log_in(&mut app, profile);
    app_until(&mut app, "the shell", |h| h.state().active_screen_text().is_some_and(|t| t.contains("welcome")));
    app.run_steps(2);
    let arguments = serde_json::json!({ "session_id": tab_id(&call, "SSH") });
    let (text, failed) = call("list_files", arguments);
    assert!(failed && text.contains("Preferences"), "{text}");

    // A new server, since the host key prompt only comes up once for each.
    let s = server(false);
    let dir = tempfile::tempdir().unwrap();
    let (mut app, call) = agent_app(&dir, true);
    let mut profile = s.profile();
    profile.kind = "ssh".into();
    log_in(&mut app, profile);
    app_until(&mut app, "the shell", |h| h.state().active_screen_text().is_some_and(|t| t.contains("welcome")));
    app.run_steps(2);
    let arguments = serde_json::json!({ "session_id": tab_id(&call, "SSH") });
    let (text, failed) = call("list_files", arguments);
    assert!(failed && text.contains("SFTP"), "{text}");
}

fn pick_color(app: &mut Harness<'static, SnekkieApp>, tab: &str, color: &str) {
    app.get_by_label(tab).click_secondary();
    app.run_steps(3);
    app.get_by_label_contains("Tab color").hover();
    app.run_steps(3);
    app.get_by_label(color).click();
    app.run_steps(3);
}

#[test]
fn an_explorer_follows_the_color_of_the_session_it_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let s = server(true);
    s.put("/home/admin/beside.txt", file(b"x"));
    let mut app = split_app(&s, &dir);
    assert_eq!(app.state().explorer_colors(), [None]);

    pick_color(&mut app, "shell", "Blue");
    let blue = app.state().explorer_colors()[0];
    assert!(blue.is_some(), "the explorer changes with its session");

    pick_color(&mut app, "shell", "Red");
    let red = app.state().explorer_colors()[0];
    assert!(red.is_some() && red != blue, "and again");

    // A second explorer starts out matching.
    app.get_by_label("shell").click_secondary();
    app.run_steps(3);
    app.get_by_label("Open file explorer").click();
    app.run_steps(4);
    assert_eq!(app.state().explorer_colors(), [red, red]);

    pick_color(&mut app, "shell", "Clear tab color");
    assert_eq!(app.state().explorer_colors(), [None, None]);
    pick_color(&mut app, "shell", "Green");
    let green = app.state().explorer_colors()[0];

    // With its session gone it keeps the color it had.
    app.get_by_label("shell").click_secondary();
    app.run_steps(3);
    app.get_by_label("Close").click();
    app.run_steps(4);
    if app.query_by_label("Yes").is_some() {
        app.get_by_label("Yes").click();
    }
    app_until(&mut app, "the tab to close", |h| !h.state().workspace_titles().contains(&"shell".to_string()));
    assert_eq!(app.state().explorer_colors(), [green, green]);
}
