//! The file explorer tab: this computer on the left, the server on the right,
//! the transfer queue below.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Align2, Color32, CursorIcon, FontId, Id, Key, Pos2, Rect, RichText, Sense, Stroke, Ui, UiBuilder, vec2};
use parking_lot::Mutex;
use tokio::sync::oneshot::error::TryRecvError;

use super::style;
use crate::profiles::{Kind, Profile};
use crate::transport::Sink;
use crate::transport::sftp::{
    self, Client, DeletePlan, Edit, EditState, Entry, JobState, Listing, Opener, Resolution, Task,
};
use crate::transport::ssh::Connection;
use crate::transport::transfer::Direction;

const ROW_HEIGHT: f32 = 20.0;
const SIZE_WIDTH: f32 = 76.0;
const MODIFIED_WIDTH: f32 = 118.0;
const PERMISSIONS_WIDTH: f32 = 84.0;
/// Below this, a list only shows names and sizes.
const WIDE_ENOUGH: f32 = 420.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Local,
    Remote,
}

/// What a tab asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplorerAction {
    Reconnect,
    ShowHidden(bool),
}

pub struct ExplorerOptions {
    /// This tab has the keyboard. In split view the other pane may have it.
    pub keyboard: bool,
    /// Files dropped from the desktop come here. Only one tab takes them.
    pub drops: bool,
    /// False while something else has the keyboard and mouse.
    pub interactive: bool,
    pub show_hidden: bool,
}

/// What every explorer tab is given by the app.
#[derive(Clone)]
pub struct ExplorerContext {
    pub runtime: tokio::runtime::Handle,
    pub repaint: Arc<dyn Fn() + Send + Sync>,
    pub opener: Opener,
    pub show_hidden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Name,
    Size,
    Modified,
}

struct Pane {
    side: Side,
    path: String,
    path_text: String,
    entries: Vec<Entry>,
    /// Which entries show, in order. Rebuilt when the list or sort changes.
    view: Vec<usize>,
    selected: BTreeSet<String>,
    anchor: Option<usize>,
    sort: Column,
    ascending: bool,
    loading: Option<Task<Result<Listing, String>>>,
    error: Option<String>,
    /// The hidden-files setting the view was built with; `None` rebuilds it.
    shown_hidden: Option<bool>,
}

impl Pane {
    fn new(side: Side) -> Pane {
        Pane {
            side,
            path: String::new(),
            path_text: String::new(),
            entries: Vec::new(),
            view: Vec::new(),
            selected: BTreeSet::new(),
            anchor: None,
            sort: Column::Name,
            ascending: true,
            loading: None,
            error: None,
            shown_hidden: None,
        }
    }

    fn rebuild_view(&mut self, show_hidden: bool) {
        self.shown_hidden = Some(show_hidden);
        let mut view: Vec<usize> = (0..self.entries.len())
            .filter(|&index| show_hidden || !self.entries[index].name.starts_with('.'))
            .collect();
        let entries = &self.entries;
        let (sort, ascending) = (self.sort, self.ascending);
        view.sort_by(|&a, &b| {
            let (a, b) = (&entries[a], &entries[b]);
            b.is_dir.cmp(&a.is_dir).then_with(|| {
                let order = match sort {
                    Column::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                    Column::Size => a.size.cmp(&b.size),
                    Column::Modified => a.modified.cmp(&b.modified),
                };
                let order = order.then_with(|| a.name.cmp(&b.name));
                if ascending { order } else { order.reverse() }
            })
        });
        self.view = view;
        // Anything now hidden must not be acted on by accident.
        let shown: BTreeSet<&str> = self.view.iter().map(|&index| self.entries[index].name.as_str()).collect();
        self.selected.retain(|name| shown.contains(name.as_str()));
        self.anchor = None;
    }

    fn accept(&mut self, (path, entries): Listing, show_hidden: bool) {
        if path != self.path {
            self.selected.clear();
        }
        self.path_text.clone_from(&path);
        self.path = path;
        self.entries = entries;
        self.rebuild_view(show_hidden);
    }

    fn click(&mut self, index: usize, modifiers: egui::Modifiers) {
        let Some(name) = self.view.get(index).map(|&entry| self.entries[entry].name.clone()) else { return };
        if modifiers.shift
            && let Some(anchor) = self.anchor
        {
            let (from, to) = (anchor.min(index), anchor.max(index));
            if !modifiers.command {
                self.selected.clear();
            }
            for &entry in &self.view[from..=to.min(self.view.len() - 1)] {
                self.selected.insert(self.entries[entry].name.clone());
            }
            return;
        }
        if modifiers.command {
            if !self.selected.remove(&name) {
                self.selected.insert(name);
            }
        } else {
            self.selected.clear();
            self.selected.insert(name);
        }
        self.anchor = Some(index);
    }

    fn selection(&self) -> Vec<&Entry> {
        self.view.iter().map(|&index| &self.entries[index]).filter(|e| self.selected.contains(&e.name)).collect()
    }

    fn select_only(&mut self, name: &str) {
        if !self.selected.contains(name) {
            self.selected.clear();
            self.selected.insert(name.to_string());
        }
    }

    fn select_all(&mut self) {
        self.selected = self.view.iter().map(|&index| self.entries[index].name.clone()).collect();
    }

    fn child(&self, name: &str) -> String {
        match self.side {
            Side::Remote => sftp::remote_join(&self.path, name),
            // On the drive list, the name is already the whole path.
            Side::Local if self.path.is_empty() => name.to_string(),
            Side::Local => Path::new(&self.path).join(name).to_string_lossy().into_owned(),
        }
    }

    fn parent(&self) -> String {
        match self.side {
            Side::Remote => sftp::remote_parent(&self.path),
            Side::Local => sftp::local_parent(&self.path),
        }
    }

    fn title(&self) -> &'static str {
        match self.side {
            Side::Local => "This computer",
            Side::Remote => "Server",
        }
    }
}

/// A row or button asked for something. Done after drawing, once nothing is
/// borrowed.
#[derive(Debug, Clone, PartialEq)]
enum Request {
    Open(Side, String),
    Up(Side),
    Go(Side, String),
    Refresh(Side),
    /// Send the side's selection to the other side.
    Transfer(Side),
    Edit,
    Rename,
    Delete,
    Permissions,
    NewFolder,
}

enum Dialog {
    Rename { name: String, text: String, focus: bool },
    NewFolder { text: String, focus: bool },
    Delete { names: Vec<String>, plan: DeletePlan },
    Permissions { items: Vec<(String, u32)>, bits: u32 },
    Message(String),
}

enum Link {
    Waiting(Task<Result<Connection, String>>),
    Opening(Task<Result<Option<Client>, String>>),
    Ready(Box<Client>),
    /// Logged in, but the server doesn't speak SFTP.
    NoSftp,
    Closed(String),
}

/// Keeps a server's login banner, which there's no terminal to show it in.
#[derive(Default)]
pub struct BannerSink(Mutex<String>);

impl Sink for BannerSink {
    fn connected(&self) {}

    fn data(&self, bytes: &[u8]) {
        let mut banner = self.0.lock();
        if banner.len() < 4096 {
            banner.push_str(&String::from_utf8_lossy(bytes).replace('\r', ""));
        }
    }

    fn closed(&self, _reason: Option<String>) {}

    fn notice(&self, _message: String) {}
}

/// Open a file with the app the user has chosen for its type.
/// Opens a file for Edit in the chosen program, or the default app if none is chosen.
pub fn open_with(editor: &str, path: &Path) -> Result<(), String> {
    // Windows' "Copy as path" adds quotes.
    let editor = editor.trim().trim_matches('"');
    if editor.is_empty() {
        return open_in_default_app(path);
    }
    let mut command = std::process::Command::new(editor);
    // A macOS app is a folder, which only `open` can start.
    #[cfg(target_os = "macos")]
    if editor.ends_with(".app") {
        command = std::process::Command::new("open");
        command.arg("-a").arg(editor);
    }
    command.arg(path).spawn().map(drop).map_err(|e| format!("Could not open {} with {editor}: {e}", path.display()))
}

pub fn open_in_default_app(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let mut command = std::process::Command::new("explorer.exe");
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command.arg(path).spawn().map(drop).map_err(|e| format!("Could not open {}: {e}", path.display()))
}

/// Turn a shell's shared login into the task a tab waits on.
pub fn shared_connection(
    runtime: &tokio::runtime::Handle,
    connection: tokio::sync::oneshot::Receiver<Connection>,
) -> Task<Result<Connection, String>> {
    let (reply, task) = tokio::sync::oneshot::channel();
    runtime.spawn(async move {
        let _ = reply.send(connection.await.map_err(|_| "The session closed before sharing its connection".into()));
    });
    task
}

type DeleteTask = Task<Result<DeletePlan, String>>;

pub struct ExplorerTab {
    pub id: u64,
    pub profile: Profile,
    /// Where it sits in the tab strip.
    pub slot: usize,
    link: Link,
    context: ExplorerContext,
    banner: Option<Arc<BannerSink>>,
    local: Pane,
    remote: Pane,
    /// The pane last clicked, which keyboard shortcuts act on.
    focused: Side,
    dialog: Option<Dialog>,
    /// Waiting on a change to finish, and what to call it.
    busy: Option<(&'static str, Task<Result<(), String>>)>,
    planning_delete: Option<(Vec<String>, DeleteTask)>,
    apply_to_all: bool,
    seen_finished: u64,
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

fn format_time(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// `rwxr-x---`.
fn format_permissions(bits: u32) -> String {
    (0..9).map(|index| if bits & (0o400 >> index) == 0 { '-' } else { ['r', 'w', 'x'][index % 3] }).collect()
}

/// Whether a name typed by the user is one item in the current folder.
fn valid_new_name(name: &str) -> bool {
    sftp::remote_name_ok(name) && !name.chars().any(char::is_control)
}

fn check<T>(task: &mut Task<T>) -> Option<Option<T>> {
    match task.try_recv() {
        Ok(value) => Some(Some(value)),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Closed) => Some(None),
    }
}

impl ExplorerTab {
    pub fn new(
        id: u64,
        profile: Profile,
        connection: Task<Result<Connection, String>>,
        banner: Option<Arc<BannerSink>>,
        context: ExplorerContext,
    ) -> ExplorerTab {
        let start = if profile.explorer_local.is_empty() {
            dirs::home_dir().unwrap_or_default().to_string_lossy().into_owned()
        } else {
            profile.explorer_local.clone()
        };
        let mut tab = ExplorerTab {
            id,
            slot: usize::MAX,
            link: Link::Waiting(connection),
            context,
            banner,
            local: Pane::new(Side::Local),
            remote: Pane::new(Side::Remote),
            focused: Side::Remote,
            dialog: None,
            busy: None,
            planning_delete: None,
            apply_to_all: false,
            seen_finished: 0,
            profile,
        };
        tab.load_local(start);
        tab
    }

    /// Connect again, keeping the folders on show.
    pub fn reconnect(&mut self, connection: Task<Result<Connection, String>>, banner: Option<Arc<BannerSink>>) {
        self.link = Link::Waiting(connection);
        self.banner = banner;
        self.dialog = None;
        self.busy = None;
        self.planning_delete = None;
    }

    pub fn title(&self) -> String {
        let name = if self.profile.kind() == Kind::Sftp {
            self.profile.name.clone()
        } else {
            format!("{} files", self.profile.name)
        };
        if matches!(self.link, Link::Closed(_) | Link::NoSftp) { format!("{name} (closed)") } else { name }
    }

    /// How agents see this tab.
    pub fn agent_entry(&self) -> crate::mcp::ExplorerEntry {
        crate::mcp::ExplorerEntry {
            id: self.id,
            name: self.title(),
            description: crate::transport::ssh::description(&self.profile).replacen("SSH", "SFTP", 1),
            link: self.client().map(Client::agent_link),
        }
    }

    /// What the status bar says while this tab is selected.
    pub fn status_text(&self) -> String {
        let place = crate::transport::ssh::description(&self.profile).replacen("SSH", "SFTP", 1);
        match &self.link {
            Link::Ready(_) => format!("{place} — connected"),
            Link::Waiting(_) | Link::Opening(_) => format!("{place} — connecting"),
            Link::NoSftp => format!("{place} — no SFTP on this device"),
            Link::Closed(_) => format!("{place} — closed"),
        }
    }

    /// A dialog of this tab's is up, so nothing beside it should take typing.
    pub fn shows_dialog(&self) -> bool {
        self.dialog.is_some() || self.needs_attention()
    }

    /// (connected, still connecting)
    pub fn status(&self) -> (bool, bool) {
        match self.link {
            Link::Ready(_) => (true, false),
            Link::Waiting(_) | Link::Opening(_) => (false, true),
            Link::NoSftp | Link::Closed(_) => (false, false),
        }
    }

    /// Connected or still trying: closing it would cut something off.
    pub fn is_live(&self) -> bool {
        self.status() != (false, false)
    }

    /// Hang up, as offline mode needs: no more network from this tab.
    pub fn shutdown(&mut self, reason: &str) {
        self.link = Link::Closed(reason.to_string());
        self.dialog = None;
    }

    /// What closing the tab would cut off, if anything.
    pub fn close_warning(&self) -> Option<String> {
        let Link::Ready(client) = &self.link else { return None };
        let active = client.queue().active();
        let edits = client.edits().len();
        let mut parts = Vec::new();
        if active > 0 {
            parts.push(format!("This stops {active} transfer(s)."));
        }
        if edits > 0 {
            parts.push(format!("This deletes the temporary copies of {edits} file(s) open for editing."));
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    /// The folders to reopen next time.
    pub fn folders(&self) -> (&str, &str) {
        (&self.local.path, &self.remote.path)
    }

    /// Whether a question is waiting for this tab to be looked at.
    pub fn needs_attention(&self) -> bool {
        let Link::Ready(client) = &self.link else { return false };
        client.queue().next_conflict().is_some()
            || client.edits().iter().any(|edit| edit.state == EditState::RemoteChanged)
    }

    fn client(&self) -> Option<&Client> {
        match &self.link {
            Link::Ready(client) => Some(client),
            _ => None,
        }
    }

    fn pane_mut(&mut self, side: Side) -> &mut Pane {
        match side {
            Side::Local => &mut self.local,
            Side::Remote => &mut self.remote,
        }
    }

    fn load_local(&mut self, path: String) {
        let (reply, task) = tokio::sync::oneshot::channel();
        let repaint = self.context.repaint.clone();
        self.context.runtime.spawn_blocking(move || {
            let _ = reply.send(sftp::list_local(&path));
            repaint();
        });
        self.local.loading = Some(task);
    }

    fn load_remote(&mut self, path: String) {
        if let Link::Ready(client) = &self.link {
            self.remote.loading = Some(client.list(path));
        }
    }

    fn load(&mut self, side: Side, path: String) {
        match side {
            Side::Local => self.load_local(path),
            Side::Remote => self.load_remote(path),
        }
    }

    fn refresh(&mut self, side: Side) {
        let path = self.pane_mut(side).path.clone();
        self.load(side, path);
    }

    /// Catch up with whatever finished since the last frame. Runs for every
    /// tab, on screen or not.
    pub fn poll(&mut self, ctx: &egui::Context) {
        match &mut self.link {
            Link::Waiting(task) => {
                if let Some(result) = check(task) {
                    self.link = match result {
                        Some(Ok(connection)) => Link::Opening(Client::open(
                            connection,
                            &self.context.runtime,
                            self.context.repaint.clone(),
                            self.context.opener.clone(),
                        )),
                        Some(Err(error)) => Link::Closed(error),
                        None => Link::Closed("The connection attempt stopped".into()),
                    };
                }
            }
            Link::Opening(task) => {
                if let Some(result) = check(task) {
                    match result {
                        Some(Ok(Some(client))) => {
                            let home = client.home.clone();
                            self.seen_finished = client.queue().finished_count();
                            self.link = Link::Ready(Box::new(client));
                            let saved = self.profile.explorer_remote.clone();
                            let start = [self.remote.path.clone(), saved].into_iter().find(|p| !p.is_empty());
                            self.load_remote(start.unwrap_or(home));
                        }
                        Some(Ok(None)) => self.link = Link::NoSftp,
                        Some(Err(error)) => self.link = Link::Closed(error),
                        None => self.link = Link::Closed("The connection attempt stopped".into()),
                    }
                }
            }
            Link::Ready(client) => {
                if client.is_closed() {
                    self.link = Link::Closed("The connection was lost. Unfinished transfers stopped.".into());
                    self.dialog = None;
                } else {
                    let active = client.queue().active();
                    let finished = client.queue().finished_count();
                    if active > 0 {
                        ctx.request_repaint_after(std::time::Duration::from_millis(200));
                    } else if finished != self.seen_finished {
                        self.seen_finished = finished;
                        self.refresh(Side::Local);
                        self.refresh(Side::Remote);
                    }
                }
            }
            Link::NoSftp | Link::Closed(_) => {}
        }

        for side in [Side::Local, Side::Remote] {
            let show_hidden = self.context.show_hidden;
            let pane = self.pane_mut(side);
            let Some(task) = &mut pane.loading else { continue };
            let Some(result) = check(task) else { continue };
            pane.loading = None;
            match result {
                Some(Ok(listing)) => {
                    pane.error = None;
                    pane.accept(listing, show_hidden);
                }
                Some(Err(error)) => {
                    pane.path_text.clone_from(&pane.path);
                    pane.error = Some(error);
                    // A remembered folder may be gone: start from home instead.
                    if pane.path.is_empty() {
                        match side {
                            Side::Remote => {
                                if let Some(home) = self.client().map(|client| client.home.clone()) {
                                    self.load_remote(home);
                                }
                            }
                            Side::Local => {
                                self.load_local(dirs::home_dir().unwrap_or_default().to_string_lossy().into_owned());
                            }
                        }
                    }
                }
                None => pane.error = Some("The listing stopped".into()),
            }
        }

        if let Some((_, task)) = &mut self.busy
            && let Some(result) = check(task)
        {
            self.busy = None;
            match result {
                Some(Ok(())) => {}
                Some(Err(error)) => self.dialog = Some(Dialog::Message(error)),
                None => self.dialog = Some(Dialog::Message("The change stopped before it finished.".into())),
            }
            self.refresh(Side::Remote);
        }
        if let Some((_, task)) = &mut self.planning_delete
            && let Some(result) = check(task)
        {
            let names = self.planning_delete.take().map_or(Vec::new(), |(names, _)| names);
            self.dialog = Some(match result {
                Some(Ok(plan)) => Dialog::Delete { names, plan },
                Some(Err(error)) => Dialog::Message(error),
                None => Dialog::Message("Counting what to delete stopped before it finished.".into()),
            });
        }
    }

    fn selected_items(&self, side: Side) -> Vec<(String, Entry)> {
        let pane = if side == Side::Local { &self.local } else { &self.remote };
        pane.selection().into_iter().map(|entry| (pane.child(&entry.name), entry.clone())).collect()
    }

    fn apply(&mut self, request: Request) {
        match request {
            Request::Open(side, name) => {
                let pane = self.pane_mut(side);
                let target = pane.child(&name);
                let folder = pane.entries.iter().any(|entry| entry.name == name && entry.is_dir);
                if folder {
                    self.load(side, target);
                } else {
                    self.pane_mut(side).select_only(&name);
                    self.apply(Request::Transfer(side));
                }
            }
            Request::Up(side) => {
                let parent = self.pane_mut(side).parent();
                self.load(side, parent);
            }
            Request::Go(side, path) => self.load(side, path),
            Request::Refresh(side) => self.refresh(side),
            Request::Transfer(Side::Local) => {
                if self.local.path.is_empty() {
                    return;
                }
                let paths = self.selected_items(Side::Local).into_iter().map(|(path, _)| PathBuf::from(path)).collect();
                if let Some(client) = self.client() {
                    client.upload(paths, self.remote.path.clone());
                }
            }
            Request::Transfer(Side::Remote) => {
                if self.local.path.is_empty() {
                    self.dialog = Some(Dialog::Message("Open a folder on this computer to download into.".into()));
                    return;
                }
                let items = self.selected_items(Side::Remote);
                if let Some(client) = self.client() {
                    client.download(items, PathBuf::from(&self.local.path));
                }
            }
            Request::Edit => {
                let files: Vec<String> = self
                    .selected_items(Side::Remote)
                    .into_iter()
                    .filter(|(_, entry)| !entry.is_dir)
                    .map(|(path, _)| path)
                    .collect();
                for file in files {
                    let result = self.client().map_or(Ok(()), |client| client.edit(file));
                    if let Err(error) = result {
                        self.dialog = Some(Dialog::Message(error));
                    }
                }
            }
            Request::Rename => {
                if let [(_, entry)] = self.selected_items(Side::Remote).as_slice() {
                    self.dialog =
                        Some(Dialog::Rename { name: entry.name.clone(), text: entry.name.clone(), focus: true });
                }
            }
            Request::NewFolder => self.dialog = Some(Dialog::NewFolder { text: String::new(), focus: true }),
            Request::Delete => {
                let items = self.selected_items(Side::Remote);
                if items.is_empty() {
                    return;
                }
                let names = items.iter().map(|(_, entry)| entry.name.clone()).collect();
                if let Some(client) = self.client() {
                    self.planning_delete = Some((names, client.plan_delete(items)));
                }
            }
            Request::Permissions => {
                let items: Vec<(String, u32)> = self
                    .selected_items(Side::Remote)
                    .into_iter()
                    .filter_map(|(path, entry)| entry.permissions.map(|bits| (path, bits)))
                    .collect();
                if let Some(&(_, bits)) = items.first() {
                    self.dialog = Some(Dialog::Permissions { items, bits: bits & 0o777 });
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, options: &ExplorerOptions) -> Option<ExplorerAction> {
        self.context.show_hidden = options.show_hidden;
        let ctx = ui.ctx().clone();
        let mut action = None;
        if !matches!(self.link, Link::Ready(_)) {
            return self.waiting_ui(ui);
        }
        if self.local.shown_hidden != Some(options.show_hidden) {
            self.local.rebuild_view(options.show_hidden);
        }
        if self.remote.shown_hidden != Some(options.show_hidden) {
            self.remote.rebuild_view(options.show_hidden);
        }

        let mut requests = Vec::new();
        self.toolbar(ui, &mut requests, &mut action);
        ui.add_space(4.0);

        let area = ui.available_rect_before_wrap();
        let queue_height = (area.height() * 0.3).clamp(84.0, 260.0);
        let gap = 8.0;
        let panes_bottom = area.bottom() - queue_height - gap;
        let middle = area.center().x;
        let left = Rect::from_min_max(area.min, Pos2::new(middle - gap / 2.0, panes_bottom));
        let right =
            Rect::from_min_max(Pos2::new(middle + gap / 2.0, area.top()), Pos2::new(area.right(), panes_bottom));
        let bottom = Rect::from_min_max(Pos2::new(area.left(), panes_bottom + gap), area.max);

        let pointer = ctx.input(|input| input.pointer.latest_pos());
        let hovering_files = ctx.input(|input| !input.raw.hovered_files.is_empty());
        // A drag started in another explorer beside this one isn't ours.
        let dragged =
            egui::DragAndDrop::payload::<Dragged>(&ctx).filter(|payload| payload.0 == self.id).map(|payload| payload.1);
        for (rect, side) in [(left, Side::Local), (right, Side::Remote)] {
            let pane = if side == Side::Local { &mut self.local } else { &mut self.remote };
            let target = dragged.is_some_and(|from| from != side && pointer.is_some_and(|p| rect.contains(p)));
            ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                ui.set_clip_rect(rect);
                pane_ui(ui, self.id, pane, options.interactive, &mut self.focused, &mut requests);
            });
            let stroke = if target || (hovering_files && options.drops && side == Side::Remote) {
                Stroke::new(2.0, ui.visuals().selection.stroke.color)
            } else {
                Stroke::new(1.0, style::border(ui))
            };
            ui.painter().rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
        }

        if let Some(from) = dragged {
            let count = if from == Side::Local { self.local.selected.len() } else { self.remote.selected.len() };
            let destination = if from == Side::Local { "the server" } else { "this computer" };
            if let Some(position) = pointer {
                egui::Area::new(Id::new(("explorer_drag", self.id)))
                    .fixed_pos(position + vec2(14.0, 6.0))
                    .order(egui::Order::Tooltip)
                    .interactable(false)
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(format!("{count} item(s) to {destination}"));
                        });
                    });
            }
            if ctx.input(|input| input.pointer.any_released())
                && egui::DragAndDrop::take_payload::<Dragged>(&ctx).is_some()
            {
                let onto = if from == Side::Local { right } else { left };
                if pointer.is_some_and(|p| onto.contains(p)) {
                    requests.push(Request::Transfer(from));
                }
            }
        }

        // Files dropped from the desktop go to the folder on show on the server.
        let dropped: Vec<PathBuf> =
            ctx.input(|input| input.raw.dropped_files.iter().map(|file| file.path().to_path_buf()).collect());
        if !dropped.is_empty()
            && options.interactive
            && options.drops
            && let Some(client) = self.client()
        {
            client.upload(dropped, self.remote.path.clone());
        }

        ui.scope_builder(UiBuilder::new().max_rect(bottom), |ui| {
            ui.set_clip_rect(bottom);
            self.queue_ui(ui);
        });
        ui.painter().rect_stroke(bottom, 2.0, Stroke::new(1.0, style::border(ui)), egui::StrokeKind::Inside);

        if options.interactive && options.keyboard && self.dialog.is_none() && !ctx.egui_wants_keyboard_input() {
            self.shortcuts(&ctx, &mut requests);
        }
        for request in requests {
            self.apply(request);
        }
        if options.interactive {
            self.dialogs_ui(&ctx);
        }
        action
    }

    fn waiting_ui(&mut self, ui: &mut Ui) -> Option<ExplorerAction> {
        let mut action = None;
        let host = self.profile.host.clone();
        egui::Frame::new().inner_margin(20.0).show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(560.0));
            match &self.link {
                Link::Waiting(_) | Link::Opening(_) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Connecting to {host}…"));
                    });
                }
                Link::NoSftp => {
                    ui.heading("This device doesn't offer SFTP");
                    ui.add_space(6.0);
                    ui.label(
                        "The explorer needs SFTP to list folders. Many network devices only offer SCP, which can't. \
                         Connect over SSH, then use Upload file… or Download file… on the tab's right-click menu.",
                    );
                }
                Link::Closed(reason) => {
                    ui.heading("Not connected");
                    ui.add_space(6.0);
                    ui.label(reason.as_str());
                    ui.add_space(10.0);
                    if ui.button("Reconnect").clicked() {
                        action = Some(ExplorerAction::Reconnect);
                    }
                }
                Link::Ready(_) => {}
            }
            let banner = self.banner.as_ref().map(|banner| banner.0.lock().trim().to_string());
            if let Some(banner) = banner.filter(|banner| !banner.is_empty()) {
                ui.add_space(12.0);
                ui.label(RichText::new("Server banner").strong());
                ui.label(RichText::new(banner).monospace());
            }
        });
        action
    }

    fn toolbar(&mut self, ui: &mut Ui, requests: &mut Vec<Request>, action: &mut Option<ExplorerAction>) {
        let local = self.local.selected.len();
        let remote = self.remote.selected.len();
        let files = self.remote.selection().iter().filter(|entry| !entry.is_dir).count();
        let permissions = self.remote.selection().iter().any(|entry| entry.permissions.is_some());
        // Wraps rather than scrolls, so a narrow split pane hides no action.
        ui.scope(|ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(local > 0, egui::Button::new("Upload ➡")).clicked() {
                    requests.push(Request::Transfer(Side::Local));
                }
                if ui.add_enabled(remote > 0, egui::Button::new("⬅ Download")).clicked() {
                    requests.push(Request::Transfer(Side::Remote));
                }
                ui.separator();
                let edit = ui.add_enabled(files > 0, egui::Button::new("Edit"));
                if edit.on_hover_text("Open a copy in your own app, and upload it each time you save").clicked() {
                    requests.push(Request::Edit);
                }
                if ui.button("New folder").clicked() {
                    requests.push(Request::NewFolder);
                }
                if ui.add_enabled(remote == 1, egui::Button::new("Rename")).clicked() {
                    requests.push(Request::Rename);
                }
                if ui.add_enabled(remote > 0, egui::Button::new("Delete")).clicked() {
                    requests.push(Request::Delete);
                }
                if ui.add_enabled(permissions, egui::Button::new("Permissions")).clicked() {
                    requests.push(Request::Permissions);
                }
                ui.separator();
                let mut show_hidden = self.context.show_hidden;
                if ui.checkbox(&mut show_hidden, "Hidden files").changed() {
                    self.context.show_hidden = show_hidden;
                    *action = Some(ExplorerAction::ShowHidden(show_hidden));
                }
                if let Some((what, _)) = &self.busy {
                    ui.separator();
                    ui.spinner();
                    ui.label(*what);
                }
                if self.planning_delete.is_some() {
                    ui.separator();
                    ui.spinner();
                    ui.label("Counting what to delete…");
                }
            });
        });
    }

    fn shortcuts(&mut self, ctx: &egui::Context, requests: &mut Vec<Request>) {
        let side = self.focused;
        ctx.input(|input| {
            if input.key_pressed(Key::F5) {
                requests.push(Request::Refresh(side));
            }
            if input.key_pressed(Key::Backspace) {
                requests.push(Request::Up(side));
            }
            if side == Side::Remote && input.key_pressed(Key::F2) {
                requests.push(Request::Rename);
            }
            if side == Side::Remote && input.key_pressed(Key::Delete) {
                requests.push(Request::Delete);
            }
        });
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, Key::A)) {
            self.pane_mut(side).select_all();
        }
    }

    fn queue_ui(&mut self, ui: &mut Ui) {
        let Some(client) = self.client() else { return };
        let queue = client.queue();
        let edits = client.edits();
        let active = queue.active();
        let total = queue.with_jobs(<[_]>::len);
        let mut cancel = None;
        let mut stop_edit = None;
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("Transfers").strong());
            ui.label(RichText::new(if active > 0 { format!("{active} to go") } else { "Idle".to_string() }).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if ui.add_enabled(total > 0 && total > active, egui::Button::new("Clear finished").small()).clicked() {
                    queue.clear_finished();
                }
                if ui.add_enabled(active > 0, egui::Button::new("Cancel all").small()).clicked() {
                    queue.cancel_all();
                }
            });
        });
        for edit in &edits {
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                let name = edit.remote.rsplit('/').next().unwrap_or_default();
                ui.label(format!("✏ {name}"));
                ui.label(RichText::new(edit_status(edit)).small().color(style::secondary(ui)));
                if ui.button("Stop editing").clicked() {
                    stop_edit = Some(edit.id);
                }
            });
        }
        let rows = queue.with_jobs(<[_]>::len);
        egui::ScrollArea::vertical().id_salt(("explorer_queue", self.id)).auto_shrink([false, false]).show_rows(
            ui,
            ROW_HEIGHT,
            rows,
            |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let rows: Vec<_> = queue.with_jobs(|jobs| jobs.get(range).map(<[_]>::to_vec).unwrap_or_default());
                for job in rows {
                    let width = ui.available_width();
                    ui.allocate_ui_with_layout(
                        vec2(width, ROW_HEIGHT),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add_space(6.0);
                            ui.label(if job.direction == Direction::Upload { "⬆" } else { "⬇" });
                            let progress_width = 180.0;
                            let name_width = (width - progress_width - 70.0).max(40.0);
                            let name = egui::Label::new(&job.name).truncate();
                            left_cell(ui, name_width, name);
                            match &job.state {
                                JobState::Running => {
                                    let bar = egui::ProgressBar::new(job.progress.fraction().unwrap_or(0.0))
                                        .desired_width(progress_width)
                                        .desired_height(12.0)
                                        .show_percentage();
                                    ui.add(bar);
                                }
                                state => {
                                    let (text, color) = job_status(ui, state);
                                    let label = egui::Label::new(RichText::new(text).color(color)).truncate();
                                    let response = left_cell(ui, progress_width, label);
                                    if let JobState::Failed(why) | JobState::Skipped(why) = state {
                                        response.on_hover_text(why);
                                    }
                                }
                            }
                            if !job.state.finished()
                                && ui
                                    .add(egui::Button::new("×").frame(false).small())
                                    .on_hover_text(format!("Cancel {}", job.name))
                                    .clicked()
                            {
                                cancel = Some(job.id);
                            }
                        },
                    );
                }
            },
        );
        if let Some(id) = cancel {
            queue.cancel(id);
        }
        if let Some(id) = stop_edit {
            client.stop_edit(id);
        }
    }

    fn dialogs_ui(&mut self, ctx: &egui::Context) {
        let id = Id::new(("explorer_dialog", self.id));
        if self.dialog.is_none() {
            self.questions_ui(ctx, id);
            return;
        }
        let enter = ctx.input(|input| input.key_pressed(Key::Enter));
        let escape = ctx.input(|input| input.key_pressed(Key::Escape));
        let folder = self.remote.path.clone();
        let mut close = false;
        type Change = Box<dyn FnOnce(&Client) -> (&'static str, Task<Result<(), String>>)>;
        let mut after: Option<Change> = None;
        let Some(dialog) = &mut self.dialog else { return };
        egui::Modal::new(id).show(ctx, |ui| {
            ui.set_max_width(460.0);
            match dialog {
                Dialog::Message(text) => {
                    ui.heading("File explorer");
                    ui.add_space(4.0);
                    ui.label(text.as_str());
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() || enter || escape {
                        close = true;
                    }
                }
                Dialog::Rename { name, text, focus } => {
                    ui.heading(format!("Rename {name}"));
                    let label = ui.label("New name");
                    let field = ui.add(egui::TextEdit::singleline(text).desired_width(320.0)).labelled_by(label.id);
                    if std::mem::take(focus) {
                        field.request_focus();
                    }
                    let valid = valid_new_name(text.trim()) && text.trim() != name;
                    ui.horizontal(|ui| {
                        let go = ui.add_enabled(valid, egui::Button::new("Rename")).clicked();
                        if go || (enter && valid) {
                            let from = sftp::remote_join(&folder, name);
                            let to = sftp::remote_join(&folder, text.trim());
                            after = Some(Box::new(move |client| ("Renaming…", client.rename(from, to))));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() || escape {
                            close = true;
                        }
                    });
                }
                Dialog::NewFolder { text, focus } => {
                    ui.heading("New folder");
                    let label = ui.label("Folder name");
                    let field = ui.add(egui::TextEdit::singleline(text).desired_width(320.0)).labelled_by(label.id);
                    if std::mem::take(focus) {
                        field.request_focus();
                    }
                    let valid = valid_new_name(text.trim());
                    ui.horizontal(|ui| {
                        let go = ui.add_enabled(valid, egui::Button::new("Create")).clicked();
                        if go || (enter && valid) {
                            let path = sftp::remote_join(&folder, text.trim());
                            after = Some(Box::new(move |client| ("Creating folder…", client.make_folder(path))));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() || escape {
                            close = true;
                        }
                    });
                }
                Dialog::Delete { names, plan } => {
                    ui.heading("Delete from the server?");
                    ui.add_space(4.0);
                    for name in names.iter().take(8) {
                        ui.add(egui::Label::new(name.as_str()).truncate());
                    }
                    if names.len() > 8 {
                        ui.label(format!("…and {} more", names.len() - 8));
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "This permanently removes {} file(s) and {} folder(s). \
                             There is no recycle bin on the server.",
                            plan.files.len(),
                            plan.folders.len()
                        ))
                        .strong(),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        // No Enter here: this can't be undone.
                        if ui.button("Delete").clicked() {
                            let plan = std::mem::take(plan);
                            after = Some(Box::new(move |client| ("Deleting…", client.delete(plan))));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() || escape {
                            close = true;
                        }
                    });
                }
                Dialog::Permissions { items, bits } => {
                    ui.heading(if items.len() == 1 {
                        "Permissions".to_string()
                    } else {
                        format!("Permissions for {} items", items.len())
                    });
                    egui::Grid::new("explorer_permissions").show(ui, |ui| {
                        ui.label("");
                        ui.label("Read");
                        ui.label("Write");
                        ui.label("Run");
                        ui.end_row();
                        for (row, who) in ["Owner", "Group", "Others"].into_iter().enumerate() {
                            ui.label(who);
                            for (column, what) in ["read", "write", "run"].into_iter().enumerate() {
                                let flag = 0o400 >> (row * 3 + column);
                                let mut on = *bits & flag != 0;
                                let name = format!("{who} {what}");
                                let check = ui.checkbox(&mut on, "");
                                check.widget_info(|| {
                                    egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, name.clone())
                                });
                                if on != (*bits & flag != 0) {
                                    *bits ^= flag;
                                }
                            }
                            ui.end_row();
                        }
                    });
                    ui.label(RichText::new(format!("{:03o}   {}", *bits, format_permissions(*bits))).monospace());
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            let (items, bits) = (std::mem::take(items), *bits);
                            after = Some(Box::new(move |client| {
                                ("Changing permissions…", client.set_permissions(items, bits))
                            }));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() || escape {
                            close = true;
                        }
                    });
                }
            }
        });
        if close {
            self.dialog = None;
        }
        if let Some(after) = after
            && let Link::Ready(client) = &self.link
        {
            self.busy = Some(after(client));
        }
    }

    /// Questions the workers are waiting on.
    fn questions_ui(&mut self, ctx: &egui::Context, id: Id) {
        let Link::Ready(client) = &self.link else { return };
        if let Some(job) = client.queue().next_conflict() {
            let mut answer = None;
            let apply_to_all = &mut self.apply_to_all;
            egui::Modal::new(id).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.heading("That file already exists");
                ui.add_space(4.0);
                let target = match job.direction {
                    Direction::Upload => format!("{} on the server", job.remote),
                    Direction::Download => format!("{} on this computer", job.local.display()),
                };
                ui.add(egui::Label::new(target).wrap());
                ui.add_space(6.0);
                ui.checkbox(apply_to_all, "Do the same for the rest of this batch");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    // No Enter here: replacing a file can't be undone.
                    if ui.button("Overwrite").clicked() {
                        answer = Some(Resolution::Overwrite);
                    }
                    if ui.button("Skip").clicked() {
                        answer = Some(Resolution::Skip);
                    }
                    if ui.button("Keep both").on_hover_text("Saves this one as “name (1)”").clicked() {
                        answer = Some(Resolution::Rename);
                    }
                });
            });
            if let Some(answer) = answer {
                client.queue().resolve(job.id, answer, self.apply_to_all);
                self.apply_to_all = false;
            }
            return;
        }
        let changed = client.edits().into_iter().find(|edit| edit.state == EditState::RemoteChanged);
        if let Some(edit) = changed {
            let mut answer = None;
            egui::Modal::new(id).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.heading("The file changed on the server too");
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(format!(
                        "You saved {}, but it also changed on the server after you opened it. \
                         Uploading replaces that change.",
                        edit.remote
                    ))
                    .wrap(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Upload mine").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Don't upload").clicked() {
                        answer = Some(false);
                    }
                });
            });
            if let Some(upload) = answer {
                client.resolve_edit(edit.id, upload);
            }
        }
    }
}

/// Marks a drag between the panes; the selection says what's in it.
struct Dragged(u64, Side);

fn job_status(ui: &Ui, state: &JobState) -> (String, Color32) {
    let quiet = style::secondary(ui);
    match state {
        JobState::Queued => ("Queued".into(), quiet),
        JobState::Running => ("Moving".into(), quiet),
        JobState::Conflict => ("Waiting for you".into(), style::primary(ui)),
        JobState::Done => ("Done".into(), quiet),
        JobState::Canceled => ("Canceled".into(), quiet),
        JobState::Skipped(why) => (format!("Skipped: {why}"), quiet),
        JobState::Failed(why) => (format!("Failed: {why}"), ui.visuals().error_fg_color),
    }
}

/// A fixed-width, left-aligned table cell.
fn left_cell(ui: &mut Ui, width: f32, label: egui::Label) -> egui::Response {
    ui.allocate_ui_with_layout(vec2(width, ROW_HEIGHT - 2.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_width(width);
        ui.add(label)
    })
    .inner
}

fn edit_status(edit: &Edit) -> String {
    match &edit.state {
        EditState::Opening => "Downloading…".into(),
        EditState::Watching if edit.uploads == 0 => "Open in your app. Saves upload automatically.".into(),
        EditState::Watching => format!("Uploaded {} save(s).", edit.uploads),
        EditState::Uploading => "Uploading your save…".into(),
        EditState::RemoteChanged => "Changed on the server too".into(),
        EditState::Failed(why) => why.clone(),
    }
}

fn pane_ui(ui: &mut Ui, tab: u64, pane: &mut Pane, interactive: bool, focused: &mut Side, requests: &mut Vec<Request>) {
    let side = pane.side;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.label(RichText::new(pane.title()).strong());
        let at_top = pane.path.is_empty() || (side == Side::Remote && pane.path == "/");
        let up = ui.add_enabled(!at_top, egui::Button::new("⬆").small());
        let up = up.on_hover_text(format!("Up one folder: {}", pane.title()));
        if up.clicked() {
            requests.push(Request::Up(side));
        }
        let refresh = ui.add(egui::Button::new("⟳").small());
        if refresh.on_hover_text(format!("Refresh: {}", pane.title())).clicked() {
            requests.push(Request::Refresh(side));
        }
        if pane.loading.is_some() {
            ui.spinner();
        }
        let hint = if pane.path.is_empty() && side == Side::Local { "Drives" } else { "Folder" };
        let field = ui.add(
            egui::TextEdit::singleline(&mut pane.path_text)
                .id_salt(("explorer_path", side == Side::Local))
                .desired_width((ui.available_width() - 6.0).max(40.0))
                .hint_text(hint),
        );
        if field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
            requests.push(Request::Go(side, pane.path_text.trim().to_string()));
        }
    });
    if let Some(error) = &pane.error {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(error).color(ui.visuals().error_fg_color)).truncate());
        });
    }
    ui.add_space(2.0);
    let width = ui.available_width();
    let wide = width >= WIDE_ENOUGH;
    let show_permissions = wide && side == Side::Remote;
    let right = ui.max_rect().right() - 16.0;
    let permissions_x = right;
    let modified_x = permissions_x - if show_permissions { PERMISSIONS_WIDTH } else { 0.0 };
    let size_x = modified_x - if wide { MODIFIED_WIDTH } else { 0.0 };
    let name_right = size_x - SIZE_WIDTH - 6.0;
    let left = ui.max_rect().left() + 6.0;
    let font = FontId::proportional(13.0);
    let (primary, secondary) = (style::primary(ui), style::secondary(ui));

    // Column headers sort the list.
    let (header, _) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::hover());
    let mut headers = vec![
        (Column::Name, "Name", Rect::from_x_y_ranges(left..=name_right, header.y_range()), Align2::LEFT_CENTER),
        (Column::Size, "Size", Rect::from_x_y_ranges(name_right..=size_x, header.y_range()), Align2::RIGHT_CENTER),
    ];
    if wide {
        headers.push((
            Column::Modified,
            "Modified",
            Rect::from_x_y_ranges(size_x..=modified_x, header.y_range()),
            Align2::RIGHT_CENTER,
        ));
    }
    for (column, label, rect, align) in headers {
        let response = ui.interact(rect, ui.id().with(("sort", label, side == Side::Local)), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Sort by {label}")));
        if response.clicked() {
            if pane.sort == column {
                pane.ascending = !pane.ascending;
            } else {
                pane.sort = column;
                pane.ascending = true;
            }
            pane.shown_hidden = None;
        }
        let arrow = match (pane.sort == column, pane.ascending) {
            (false, _) => "",
            (true, true) => " ⬆",
            (true, false) => " ⬇",
        };
        let x = if align == Align2::LEFT_CENTER { rect.left() } else { rect.right() };
        let color = if response.hovered() { primary } else { secondary };
        ui.painter().text(Pos2::new(x, rect.center().y), align, format!("{label}{arrow}"), font.clone(), color);
    }
    if show_permissions {
        let position = Pos2::new(permissions_x, header.center().y);
        ui.painter().text(position, Align2::RIGHT_CENTER, "Permissions", font.clone(), secondary);
    }
    ui.painter().hline(header.x_range(), header.bottom(), Stroke::new(1.0, style::border(ui)));

    let row_count = pane.view.len();
    let scroll = egui::ScrollArea::vertical()
        .id_salt(("explorer_files", side == Side::Local))
        .auto_shrink([false, false])
        .show_rows(ui, ROW_HEIGHT, row_count, |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for index in range {
                let entry = pane.entries[pane.view[index]].clone();
                let selected = pane.selected.contains(&entry.name);
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click_and_drag());
                let label = entry.name.clone();
                response.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label.clone())
                });
                if selected {
                    ui.painter().rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
                } else if response.hovered() {
                    ui.painter().rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let text_color = if selected { ui.visuals().selection.stroke.color } else { primary };
                let quiet_color = if selected { text_color } else { secondary };
                let icon = match (entry.is_dir, entry.is_symlink) {
                    (_, true) => "🔗",
                    (true, false) => "📁",
                    (false, false) => "📄",
                };
                let name_rect = Rect::from_x_y_ranges(left..=name_right, rect.y_range());
                let name_painter = ui.painter().with_clip_rect(name_rect.intersect(ui.clip_rect()));
                name_painter.text(
                    Pos2::new(left, rect.center().y),
                    Align2::LEFT_CENTER,
                    format!("{icon} {}", entry.name),
                    font.clone(),
                    text_color,
                );
                if !entry.is_dir {
                    ui.painter().text(
                        Pos2::new(size_x, rect.center().y),
                        Align2::RIGHT_CENTER,
                        format_size(entry.size),
                        font.clone(),
                        quiet_color,
                    );
                }
                if wide && let Some(modified) = entry.modified {
                    ui.painter().text(
                        Pos2::new(modified_x, rect.center().y),
                        Align2::RIGHT_CENTER,
                        format_time(modified),
                        font.clone(),
                        quiet_color,
                    );
                }
                if show_permissions && let Some(bits) = entry.permissions {
                    ui.painter().text(
                        Pos2::new(permissions_x, rect.center().y),
                        Align2::RIGHT_CENTER,
                        format_permissions(bits),
                        FontId::monospace(11.0),
                        quiet_color,
                    );
                }
                if !interactive {
                    continue;
                }
                if response.clicked() {
                    *focused = side;
                    pane.click(index, ui.input(|input| input.modifiers));
                }
                if response.double_clicked() {
                    requests.push(Request::Open(side, entry.name.clone()));
                }
                if response.drag_started() {
                    *focused = side;
                    pane.select_only(&entry.name);
                    egui::DragAndDrop::set_payload(ui.ctx(), Dragged(tab, side));
                }
                if response.dragged() {
                    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                }
                if response.secondary_clicked() {
                    *focused = side;
                    pane.select_only(&entry.name);
                }
                response.context_menu(|ui| match side {
                    Side::Local => {
                        if ui.button("Upload").clicked() {
                            requests.push(Request::Transfer(Side::Local));
                        }
                        ui.separator();
                        if ui.button("Refresh").clicked() {
                            requests.push(Request::Refresh(Side::Local));
                        }
                    }
                    Side::Remote => {
                        if ui.button("Download").clicked() {
                            requests.push(Request::Transfer(Side::Remote));
                        }
                        if ui.add_enabled(!entry.is_dir, egui::Button::new("Edit")).clicked() {
                            requests.push(Request::Edit);
                        }
                        ui.separator();
                        if ui.button("Rename…").clicked() {
                            requests.push(Request::Rename);
                        }
                        if ui.button("Delete…").clicked() {
                            requests.push(Request::Delete);
                        }
                        if ui.add_enabled(entry.permissions.is_some(), egui::Button::new("Permissions…")).clicked() {
                            requests.push(Request::Permissions);
                        }
                        ui.separator();
                        if ui.button("New folder…").clicked() {
                            requests.push(Request::NewFolder);
                        }
                        if ui.button("Refresh").clicked() {
                            requests.push(Request::Refresh(Side::Remote));
                        }
                    }
                });
            }
        });
    // A click anywhere in the pane's empty space makes it the active one.
    if interactive
        && ui.input(|input| input.pointer.primary_clicked())
        && ui.input(|input| input.pointer.latest_pos()).is_some_and(|p| scroll.inner_rect.contains(p))
    {
        *focused = side;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool, size: u64) -> Entry {
        Entry { name: name.into(), is_dir, is_symlink: false, size, modified: None, permissions: None }
    }

    fn pane_with(names: &[&str]) -> Pane {
        let mut pane = Pane::new(Side::Remote);
        let entries = names.iter().map(|name| entry(name, false, 1)).collect();
        pane.accept(("/x".into(), entries), true);
        pane
    }

    #[test]
    fn lists_put_folders_first_and_can_hide_dotfiles() {
        let mut pane = Pane::new(Side::Remote);
        let listing = vec![entry("b", false, 1), entry(".hidden", false, 1), entry("Z", true, 0), entry("a", false, 9)];
        pane.accept(("/x".into(), listing), true);
        let shown = |pane: &Pane| pane.view.iter().map(|&i| pane.entries[i].name.clone()).collect::<Vec<_>>();
        assert_eq!(shown(&pane), ["Z", ".hidden", "a", "b"]);
        pane.rebuild_view(false);
        assert_eq!(shown(&pane), ["Z", "a", "b"]);
        pane.sort = Column::Size;
        pane.ascending = false;
        pane.rebuild_view(false);
        assert_eq!(shown(&pane), ["Z", "a", "b"]);
    }

    #[test]
    fn hiding_a_selected_file_unselects_it() {
        let mut pane = pane_with(&[".secret", "plain"]);
        pane.select_all();
        pane.rebuild_view(false);
        assert_eq!(pane.selected.iter().collect::<Vec<_>>(), ["plain"]);
    }

    #[test]
    fn selection_follows_clicks_and_modifiers() {
        let mut pane = pane_with(&["a", "b", "c", "d"]);
        let names = |pane: &Pane| pane.selected.iter().cloned().collect::<Vec<_>>();
        pane.click(1, egui::Modifiers::NONE);
        assert_eq!(names(&pane), ["b"]);
        pane.click(3, egui::Modifiers::SHIFT);
        assert_eq!(names(&pane), ["b", "c", "d"]);
        pane.click(0, egui::Modifiers::COMMAND);
        assert_eq!(names(&pane), ["a", "b", "c", "d"]);
        pane.click(0, egui::Modifiers::COMMAND);
        assert_eq!(names(&pane), ["b", "c", "d"]);
        pane.click(2, egui::Modifiers::NONE);
        assert_eq!(names(&pane), ["c"]);
    }

    #[test]
    fn moving_to_another_folder_clears_the_selection_but_a_refresh_keeps_it() {
        let mut pane = pane_with(&["a", "b"]);
        pane.click(0, egui::Modifiers::NONE);
        pane.accept(("/x".into(), vec![entry("a", false, 2), entry("b", false, 1)]), true);
        assert!(pane.selected.contains("a"));
        pane.accept(("/x".into(), vec![entry("b", false, 1)]), true);
        assert!(pane.selected.is_empty());
        pane.click(0, egui::Modifiers::NONE);
        pane.accept(("/y".into(), vec![entry("b", false, 1)]), true);
        assert!(pane.selected.is_empty());
    }

    #[test]
    fn sizes_and_permissions_read_naturally() {
        assert_eq!(format_size(12), "12 B");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_permissions(0o754), "rwxr-xr--");
        assert_eq!(format_permissions(0), "---------");
    }

    #[test]
    fn typed_names_must_be_a_single_item() {
        assert!(valid_new_name("logs"));
        assert!(valid_new_name(".config"));
        for name in ["", ".", "..", "a/b", "line\nbreak"] {
            assert!(!valid_new_name(name), "{name:?}");
        }
    }
}
