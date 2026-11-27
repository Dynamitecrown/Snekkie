//! Main window: menus, sidebar, tabs, status bar, and the dialogs between
//! them.

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{
    Align, Color32, Event, Frame, Id, Key, Layout, Margin, Modifiers, RichText, Sense, Stroke, TextEdit, Ui,
    ViewportCommand, vec2,
};
use parking_lot::Mutex;

use super::agents::{self, Motion};
use super::explorer::{self, BannerSink, ExplorerAction, ExplorerContext, ExplorerOptions, ExplorerTab};
use super::fonts::{self, Fonts};
use super::invade::{InvadeAction, InvadeOptions, InvadeTab};
use super::layout::{Divider, Edge, TabLayout};
use super::paste::{Action as PasteAction, PastePreview};
use super::preferences::{self, Preferences};
use super::profile_transfer::ProfileTransfer;
use super::putty_import::PuttyImport;
use super::sidebar::{self, Sidebar};
use super::snippets::{Action as SnippetAction, SnippetTarget, SnippetWindow};
use super::style;
use super::terminal_view::{TerminalView, TextExport, ViewOptions};
use super::updater::{self, Updater};
use crate::invade::EndReason;
use crate::mcp::Cue;
use crate::network::{NetworkAccess, NetworkQuestion};
use crate::paste::{PasteQueue, PasteTarget};
use crate::profiles::{self, Auth, Kind, Profile, ProfileStore};
use crate::session::{ConnectContext, LogStatus, NoticeLevel, Session, State};
use crate::settings::{AppSettings, LastTab, LastTabs, SettingsStore, Theme};
use crate::snippets::{Snippet, SnippetStore};
use crate::terminal::highlight::UserRules;
use crate::terminal::keys;
use crate::terminal::links::Link;
use crate::terminal::paging::{CliMode, Prompt, parse_prompt};
use crate::transport::serial::PortInfo;
use crate::transport::ssh::{self, HostKeyAsker, HostKeyQuestion, PromptAsker, PromptQuestion};
use crate::transport::transfer::Direction;
use crate::transport::tunnel::TunnelState;
use crate::update::{self, Release};

/// How long a status bar message stays up, in seconds.
const FLASH_SECONDS: f64 = 4.0;

#[cfg(not(target_os = "macos"))]
const SHORTCUTS: &str = "\
Ctrl+Shift+N    New session
Ctrl+Shift+D    Duplicate current session
Ctrl+Shift+R    Reconnect
Ctrl+W          Close tab
Ctrl+Tab        Next tab (Ctrl+Shift+Tab: previous)
Ctrl+Q          Quit

Ctrl+Shift+C    Copy      (selecting also copies)
Ctrl+Shift+V    Paste     (right-click pastes by default; configurable)
Ctrl+F          Find in output (Enter: older, Shift+Enter: newer)
Ctrl+Shift+S    Command snippets
Ctrl+Right-click  Terminal context menu (copy, save text, selection)
Ctrl+click      Open a link, or a new session to an address
Shift+PgUp/Dn   Scroll back through history
Ctrl+Shift+L    Clear screen and scrollback
Ctrl+Shift+B    Send break (serial, telnet)

Ctrl+B          Toggle sidebar
Ctrl+,          Preferences";

#[cfg(target_os = "macos")]
const SHORTCUTS: &str = "\
Cmd+Shift+N     New session
Cmd+Shift+D     Duplicate current session
Cmd+Shift+R     Reconnect
Cmd+W           Close tab
Ctrl+Tab        Next tab (Ctrl+Shift+Tab: previous)
Cmd+Q           Quit

Cmd+C / Cmd+X   Copy      (selecting also copies)
Cmd+V           Paste     (right-click pastes by default; configurable)
Cmd+A           Select all
Cmd+F           Find in output (Enter: older, Shift+Enter: newer)
Cmd+Shift+S     Command snippets
Ctrl+Right-click  Terminal context menu (copy, save text, selection)
Cmd+click       Open a link, or a new session to an address
Shift+PgUp/Dn   Scroll back through history
Cmd+Shift+L     Clear screen and scrollback
Cmd+Shift+B     Send break (serial, telnet)

Cmd+B           Toggle sidebar
Cmd+,           Preferences
Ctrl+C/X/V      Send terminal control characters";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    NewTab,
    Reconnect(u64),
    /// As Reconnect, but an agent asked, so the user stays on their own tab.
    AgentReconnect(u64),
    /// Log an explorer tab in again, on its own connection.
    ReconnectExplorer(u64),
}

#[derive(Debug, Clone)]
struct PendingConnect {
    profile: Profile,
    target: Target,
    /// Jump hosts to go through, first hop first.
    jumps: Vec<Profile>,
}

enum ConfirmAction {
    CloseTab(u64),
    CloseExplorer(u64),
    Quit,
    DeleteSaved(String),
    DeleteFolder(String),
    DeleteSnippet(u64),
    ReplaceTheme(String),
    DeleteTheme(String),
    InstallUpdate,
}

enum InputAction {
    Password(Box<PendingConnect>),
    Passphrase(Box<PendingConnect>),
    SaveSession(Box<Profile>),
    /// A new folder under this one, and the saved session to put in it, if any.
    NewFolder {
        parent: String,
        session: Option<String>,
    },
    RenameFolder(String),
    OtherDevice,
    OtherBaud,
    SaveTheme,
}

enum Dialog {
    Message { title: String, text: String, monospace: bool },
    About { text: String, focus: bool },
    Confirm { title: String, text: String, action: ConfirmAction },
    Input { title: String, label: String, text: String, secret: bool, action: InputAction, focus: bool },
    HostKey(Box<HostKeyQuestion>),
    Prompts(Box<PromptQuestion>, Vec<String>, bool),
    Network(Box<NetworkQuestion>),
    Update { release: Release, installable: bool },
    Import(Box<PuttyImport>),
    ProfileTransfer(Box<ProfileTransfer>),
    TabColor { id: u64, color: Color32 },
    Transfer { id: u64, direction: Direction, local: Option<std::path::PathBuf>, remote: String, replace: bool },
    ProfileColor { name: String, color: Color32, enabled: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    NewSession,
    Duplicate,
    Reconnect,
    CloseTab,
    Quit,
    Copy,
    Paste,
    SelectAll,
    Find,
    Snippets,
    ClearScreen,
    ResetTerminal,
    SendBreak,
    ToggleSidebar,
    ToggleButtonBar,
    ToggleAutoPaging,
    Preferences,
    ImportPutty,
    ImportOpenSsh,
    ImportProfiles,
    ExportProfiles,
    Shortcuts,
    CheckForUpdates,
    ShowUpdate,
    InstallUpdate,
    About,
    NextTab,
    PreviousTab,
}

impl Command {
    /// These commands act on the selected connection rather than the app.
    fn targets_session(self) -> bool {
        matches!(
            self,
            Self::Duplicate
                | Self::Reconnect
                | Self::Copy
                | Self::Paste
                | Self::SelectAll
                | Self::Find
                | Self::ClearScreen
                | Self::ResetTerminal
                | Self::SendBreak
        )
    }
}

/// Match physical modifiers explicitly: macOS Command belongs to the app,
/// while Control still belongs to the remote terminal. Ctrl+Tab remains a
/// tab switch on every platform; Cmd+Tab is reserved by macOS.
fn shortcut_command(key: Key, modifiers: Modifiers, macos: bool) -> Option<Command> {
    if modifiers.alt {
        return None;
    }
    if modifiers.ctrl && !modifiers.mac_cmd && key == Key::Tab {
        return Some(if modifiers.shift { Command::PreviousTab } else { Command::NextTab });
    }
    let app_modifier = if macos { modifiers.mac_cmd && !modifiers.ctrl } else { modifiers.ctrl && !modifiers.mac_cmd };
    if !app_modifier {
        return None;
    }
    let table = [
        (true, Key::N, Command::NewSession),
        (true, Key::D, Command::Duplicate),
        (true, Key::R, Command::Reconnect),
        (false, Key::W, Command::CloseTab),
        (false, Key::Q, Command::Quit),
        (!macos, Key::A, Command::SelectAll),
        (false, Key::F, Command::Find),
        (true, Key::S, Command::Snippets),
        (true, Key::L, Command::ClearScreen),
        (true, Key::B, Command::SendBreak),
        (false, Key::B, Command::ToggleSidebar),
        (false, Key::Comma, Command::Preferences),
        (false, Key::PageDown, Command::NextTab),
        (false, Key::PageUp, Command::PreviousTab),
    ];
    table
        .into_iter()
        .find_map(|(shift, candidate, command)| (key == candidate && modifiers.shift == shift).then_some(command))
}

fn shortcut_label(shortcut: &str, macos: bool) -> std::borrow::Cow<'_, str> {
    if !macos || matches!(shortcut, "Ctrl+Tab" | "Ctrl+Shift+Tab") {
        return shortcut.into();
    }
    match shortcut {
        "Ctrl+Shift+C" => "Cmd+C".into(),
        "Ctrl+Shift+V" => "Cmd+V".into(),
        "Ctrl+Shift+A" => "Cmd+A".into(),
        _ => shortcut.replacen("Ctrl+", "Cmd+", 1).into(),
    }
}

/// Most the status bar gives to agent status.
const AGENT_BADGE_WIDTH: f32 = 260.0;

struct Tab {
    session: Session,
    view: TerminalView,
    color: Option<Color32>,
    /// Clearing a color is an explicit override, distinct from using the default.
    color_override: bool,
    /// The far end rang the bell while the tab was out of sight.
    bell: bool,
    /// Reconnect when back: true once the port accepts, false on giving up.
    /// Dropping it stops the wait.
    waiting: Option<tokio::sync::oneshot::Receiver<bool>>,
    /// The last device prompt seen at the cursor.
    prompt: Option<Prompt>,
}

/// What a tab in the strip shows: a real connection, found by its stable
/// session ID, or the Invade game. The game is never a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum WorkspaceTabId {
    Session(u64),
    Invade,
    Explorer(u64),
}

/// Tabs showing together. Selecting another tab shows it alone; selecting any
/// of these brings them all back.
type SplitView = TabLayout<WorkspaceTabId>;

/// Where the app keeps its files; replaced in tests.
pub struct Paths {
    pub sessions: PathBuf,
    pub settings: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Paths { sessions: ProfileStore::default_path(), settings: SettingsStore::default_path() }
    }
}

pub struct SnekkieApp {
    store: ProfileStore,
    snippet_store: SnippetStore,
    snippets: Option<SnippetWindow>,
    /// The tab whose Tunnels window is open.
    tunnels_window: Option<u64>,
    paste_preview: Option<PastePreview>,
    paste_queue: Option<PasteQueue>,
    retired_log_workers: Vec<std::thread::JoinHandle<()>>,
    settings_store: SettingsStore,
    /// Shared with open explorers, so a new choice reaches them straight away.
    explorer_editor: Arc<Mutex<String>>,
    settings: AppSettings,
    /// `settings.highlight_rules`, compiled.
    user_rules: UserRules,
    sidebar: Sidebar,
    /// Connection tabs, in the order they are shown.
    tabs: Vec<Tab>,
    /// File explorer tabs, in the order they are shown. Each knows its slot.
    explorers: Vec<ExplorerTab>,
    /// The selected tab, if any.
    active: Option<WorkspaceTabId>,
    split: Option<SplitView>,
    /// The singleton game tab, and where it sits among the connection tabs.
    invade: Option<Box<InvadeTab>>,
    /// The game after its tab closed: reopening resumes the same run, and
    /// progress that could not be saved is neither lost nor read back stale.
    parked_invade: Option<Box<InvadeTab>>,
    invade_slot: usize,
    /// The session sidebar shown on request while Invade is selected; it is
    /// otherwise hidden there without changing the saved preference.
    invade_sidebar: bool,
    /// Keys still held from the game after leaving it, kept from reaching a
    /// terminal until they are released.
    suppressed_keys: Vec<Key>,
    /// Shift used for game mobility must be released before it can modify
    /// terminal input after leaving the game.
    suppressed_shift: bool,
    runtime: tokio::runtime::Runtime,
    fonts: Fonts,
    host_keys: Arc<Mutex<VecDeque<HostKeyQuestion>>>,
    prompt_questions: Arc<Mutex<VecDeque<PromptQuestion>>>,
    network_questions: Arc<Mutex<VecDeque<NetworkQuestion>>>,
    /// Finished "Check port" results, shown in the status bar.
    port_checks: Arc<Mutex<Vec<String>>>,
    network: NetworkAccess,
    dialogs: Vec<Dialog>,
    preferences: Option<Preferences>,
    flash: Option<(String, f64)>,
    clipboard: Option<arboard::Clipboard>,
    folder_open: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    allow_close: bool,
    accent: Color32,
    styled_theme: Option<Theme>,
    space: super::space::Scene,
    dragging_tab: Option<usize>,
    /// The tab a dragged tab would go beside, if dropped on the workspace.
    tab_drag_target: Option<WorkspaceTabId>,
    /// A dragged tab, on the frame it was let go.
    tab_dropped: Option<WorkspaceTabId>,
    /// Where tabs draw, last frame. A tab dragged here isn't being reordered.
    workspace_area: Option<egui::Rect>,
    /// What showed before the current tab, to go beside it.
    last_shown: Option<WorkspaceTabId>,
    /// Tabs from last time were put back, on the first frame.
    tabs_restored: bool,
    /// Whether the terminal had the keyboard when a dialog opened, so it
    /// can have it back when the dialog closes.
    terminal_had_focus: bool,
    updater: Updater,
    /// Listening for local AI agents, while Preferences allows it.
    agents: Option<crate::mcp::Server>,
    agents_file: PathBuf,
    /// What agents are doing, as of this frame.
    agent_activity: crate::mcp::Activity,
    /// The notification-area icon. Made on the first frame, which has the
    /// context it wakes; tests never ask for one.
    #[cfg(windows)]
    tray: Option<super::tray::AppTray>,
    #[cfg(windows)]
    want_tray: bool,
    /// Chosen from the tray: close agents' bridges too, once Snekkie exits.
    #[cfg(windows)]
    close_bridges_on_exit: bool,
}

impl SnekkieApp {
    pub fn new(paths: Paths) -> Self {
        let mut app = Self::with_port_lister(paths, crate::transport::serial::list_ports);
        // Only the real app goes online by itself; tests build theirs
        // with_port_lister.
        if app.settings.check_for_updates && !app.settings.offline_mode {
            app.updater.check_at_startup();
        }
        #[cfg(windows)]
        {
            app.want_tray = true;
        }
        app
    }

    /// Styling needs the egui context, so it happens on the first frame
    /// (see `poll_background`), not here.
    pub fn with_port_lister(paths: Paths, list_ports: fn() -> Vec<PortInfo>) -> Self {
        let snippet_store = SnippetStore::open(paths.sessions.with_file_name("snippets.json"));
        let store = ProfileStore::open(paths.sessions);
        let agents_file = paths.settings.with_file_name(crate::mcp::FILE_NAME);
        let settings_store = SettingsStore::new(paths.settings);
        let settings = settings_store.load();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("snekkie-io")
            .enable_all()
            .build()
            .expect("could not start the network runtime");
        SnekkieApp {
            network: NetworkAccess::new(settings.offline_mode, Arc::new(|_| {})),
            network_questions: Arc::new(Mutex::new(VecDeque::new())),
            port_checks: Arc::new(Mutex::new(Vec::new())),
            sidebar: Sidebar::with_port_lister(&settings, list_ports),
            store,
            snippet_store,
            snippets: None,
            tunnels_window: None,
            paste_preview: None,
            paste_queue: None,
            retired_log_workers: Vec::new(),
            settings_store,
            explorer_editor: Arc::new(Mutex::new(settings.explorer_editor.clone())),
            user_rules: UserRules::compile(&settings.highlight_rules),
            settings,
            tabs: Vec::new(),
            explorers: Vec::new(),
            split: None,
            active: None,
            invade: None,
            parked_invade: None,
            invade_slot: 0,
            invade_sidebar: false,
            suppressed_keys: Vec::new(),
            suppressed_shift: false,
            runtime,
            fonts: Fonts::new(),
            host_keys: Arc::new(Mutex::new(VecDeque::new())),
            prompt_questions: Arc::new(Mutex::new(VecDeque::new())),
            dialogs: Vec::new(),
            preferences: None,
            flash: None,
            clipboard: None,
            folder_open: None,
            allow_close: false,
            // Transparent never matches a theme, so the first frame applies
            // the style.
            accent: Color32::TRANSPARENT,
            styled_theme: None,
            space: Default::default(),
            dragging_tab: None,
            tab_drag_target: None,
            tab_dropped: None,
            workspace_area: None,
            last_shown: None,
            tabs_restored: false,
            terminal_had_focus: false,
            updater: Updater::new(),
            agents: None,
            agents_file,
            agent_activity: Default::default(),
            #[cfg(windows)]
            tray: None,
            #[cfg(windows)]
            want_tray: false,
            #[cfg(windows)]
            close_bridges_on_exit: false,
        }
    }

    // -- helpers --------------------------------------------------------

    fn message(&mut self, title: &str, text: impl Into<String>) {
        self.dialogs.push(Dialog::Message { title: title.into(), text: text.into(), monospace: false });
    }

    fn confirm(&mut self, title: &str, text: impl Into<String>, action: ConfirmAction) {
        self.dialogs.push(Dialog::Confirm { title: title.into(), text: text.into(), action });
    }

    fn input(
        &mut self,
        title: &str,
        label: impl Into<String>,
        text: impl Into<String>,
        secret: bool,
        action: InputAction,
    ) {
        self.dialogs.push(Dialog::Input {
            title: title.into(),
            label: label.into(),
            text: text.into(),
            secret,
            action,
            focus: true,
        });
    }

    fn flash(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        let now = ctx.input(|i| i.time);
        self.flash = Some((text.into(), now + FLASH_SECONDS));
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(FLASH_SECONDS));
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings_store.save(&self.settings) {
            self.message("Preferences", format!("Could not save settings: {e}"));
        }
    }

    /// The selected connection tab; none while Invade is selected.
    fn current(&self) -> Option<&Tab> {
        self.active_index().map(|index| &self.tabs[index])
    }

    fn current_mut(&mut self) -> Option<&mut Tab> {
        let index = self.active_index()?;
        self.tabs.get_mut(index)
    }

    fn active_index(&self) -> Option<usize> {
        match self.active {
            Some(WorkspaceTabId::Session(id)) => self.tab_index(id),
            _ => None,
        }
    }

    /// What a session's tab says: the title the far end set, if the user
    /// chose that and the tab is still connected, or else the device's
    /// hostname (serial and unsaved tabs) or the profile name.
    fn tab_label(&self, tab: &Tab) -> String {
        let closed = matches!(tab.session.state(), State::Closed(_));
        let remote = self.settings.remote_titles && !closed;
        let host = tab.prompt.as_ref().filter(|_| self.names_from_prompt(tab)).map(|p| match closed {
            true => format!("{} (closed)", p.host),
            false => p.host.clone(),
        });
        remote
            .then(|| tab.session.shared.emulator.lock().remote_title())
            .flatten()
            .map(|title| title.trim().chars().filter(|c| !c.is_control()).take(48).collect::<String>())
            .filter(|title| !title.is_empty())
            .or(host)
            .unwrap_or_else(|| tab.session.title())
    }

    /// Serial tabs and unsaved sessions take their name from the device
    /// prompt; saved sessions keep the name the user gave them.
    fn names_from_prompt(&self, tab: &Tab) -> bool {
        let profile = &tab.session.profile;
        profile.kind() == Kind::Serial
            || self.store.get(&profile.name).is_none_or(|saved| !same_destination(saved, profile))
    }

    /// The New session form, filled in with a host clicked in the output.
    /// Nothing connects until the user does.
    fn new_session_to(&mut self, ctx: &egui::Context, host: String) {
        self.run_command(ctx, Command::NewSession);
        let profile = Profile { host, ..self.sidebar.draft.clone() };
        self.sidebar.load(profile);
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.session.id == id)
    }

    fn invade_selected(&self) -> bool {
        self.active == Some(WorkspaceTabId::Invade) && self.invade.is_some()
    }

    /// An unfinished game counts as activity, like a connected session.
    fn invade_unfinished(&self) -> bool {
        self.invade.as_ref().or(self.parked_invade.as_ref()).is_some_and(|game| game.unfinished())
    }

    /// Every tab in the order the strip shows them.
    fn workspace_order(&self) -> Vec<WorkspaceTabId> {
        let mut order: Vec<_> = self.tabs.iter().map(|t| WorkspaceTabId::Session(t.session.id)).collect();
        if self.invade.is_some() {
            order.insert(self.invade_slot.min(order.len()), WorkspaceTabId::Invade);
        }
        for explorer in &self.explorers {
            order.insert(explorer.slot.min(order.len()), WorkspaceTabId::Explorer(explorer.id));
        }
        order
    }

    /// Rearrange the tabs to `order`, which holds the same tabs.
    fn set_workspace_order(&mut self, order: &[WorkspaceTabId]) {
        let mut tabs = std::mem::take(&mut self.tabs);
        let mut sorted = Vec::with_capacity(tabs.len());
        let mut explorers = std::mem::take(&mut self.explorers);
        let mut sorted_explorers = Vec::with_capacity(explorers.len());
        for (position, id) in order.iter().enumerate() {
            match id {
                WorkspaceTabId::Explorer(explorer) => {
                    if let Some(index) = explorers.iter().position(|e| e.id == *explorer) {
                        let mut explorer = explorers.remove(index);
                        explorer.slot = position;
                        sorted_explorers.push(explorer);
                    }
                }
                WorkspaceTabId::Invade => self.invade_slot = sorted.len(),
                WorkspaceTabId::Session(session) => {
                    if let Some(index) = tabs.iter().position(|t| t.session.id == *session) {
                        sorted.push(tabs.remove(index));
                    }
                }
            }
        }
        sorted.append(&mut tabs);
        self.tabs = sorted;
        sorted_explorers.append(&mut explorers);
        self.explorers = sorted_explorers;
    }

    /// Show a tab. Leaving Invade pauses it; returning takes Resume.
    fn select_workspace_tab(&mut self, id: WorkspaceTabId) {
        if self.active != Some(id) && self.active.is_some() {
            self.last_shown = self.active;
        }
        match id {
            WorkspaceTabId::Session(session) => {
                let Some(index) = self.tab_index(session) else { return };
                if self.invade_selected() {
                    self.leave_invade();
                }
                self.active = Some(id);
                self.tabs[index].view.focus();
            }
            WorkspaceTabId::Invade => {
                if self.invade.is_some() {
                    self.active = Some(id);
                    self.invade_sidebar = false;
                }
            }
            WorkspaceTabId::Explorer(explorer) => {
                if !self.explorers.iter().any(|e| e.id == explorer) {
                    return;
                }
                if self.invade_selected() {
                    self.leave_invade();
                }
                self.active = Some(id);
            }
        }
    }

    /// A tab is closing: take it out of the layout and name the tab to show
    /// in its place.
    fn take_split_partner(&mut self, closing: WorkspaceTabId) -> Option<WorkspaceTabId> {
        let split = self.split.as_mut().filter(|split| split.contains(closing))?;
        split.remove(closing);
        let tabs = split.tabs();
        if tabs.len() < 2 {
            self.split = None;
        }
        tabs.first().copied()
    }

    /// The layout to draw now: one of its tabs is selected.
    fn visible_split(&self) -> Option<&SplitView> {
        let active = self.active?;
        self.split.as_ref().filter(|split| split.contains(active))
    }

    /// Put a tab beside another: in the other's layout if there's room, or
    /// else as a new pair.
    fn start_split(&mut self, entry: WorkspaceTabId, other: WorkspaceTabId) {
        let area = self.workspace_area;
        let added = self.split.as_ref().and_then(|split| {
            [Edge::Right, Edge::Bottom]
                .into_iter()
                .filter_map(|edge| split.dropped(entry, other, edge).filter(|(landed, _)| *landed == edge))
                .map(|(_, next)| next)
                .find(|next| area.is_none_or(|area| next.fits(area)))
        });
        self.split = Some(added.unwrap_or_else(|| TabLayout::pair(other, entry)));
        self.select_workspace_tab(entry);
    }

    /// Other tabs that can go beside this one. The game always plays alone.
    fn split_candidates(&self, entry: WorkspaceTabId) -> Vec<(WorkspaceTabId, String)> {
        self.workspace_order()
            .into_iter()
            .zip(self.workspace_titles())
            .filter(|(id, _)| *id != entry && *id != WorkspaceTabId::Invade)
            .collect()
    }

    /// Rename or move a folder, with its sessions and subfolders.
    fn rename_folder(&mut self, from: String, to: String) {
        if from == to {
            return;
        }
        if let Some(problem) = self.store.folder_move_error(&from, &to) {
            self.message("Saved sessions", problem);
        } else if let Err(e) = self.store.rename_folder(&from, &to) {
            self.message("Saved sessions", format!("Could not save sessions: {e}"));
        }
    }

    fn leave_invade(&mut self) {
        if let Some(game) = &mut self.invade {
            self.suppressed_keys.extend(game.held_keys());
            game.pause();
        }
        self.invade_sidebar = false;
    }

    fn invade_path(&self) -> PathBuf {
        self.settings_store.path.with_file_name(crate::invade::PROGRESS_FILE)
    }

    /// From About Snekkie, open the game once or show the one that is open.
    fn open_invade(&mut self) {
        if self.invade.is_none() {
            let game = self.parked_invade.take().unwrap_or_else(|| {
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as u64);
                Box::new(InvadeTab::open(self.invade_path(), seed))
            });
            self.invade = Some(game);
            self.invade_slot = self.tabs.len();
            // Explorer slots count every tab, so the game takes the far end.
            let order = self.workspace_order();
            self.set_workspace_order(&order);
        }
        self.select_workspace_tab(WorkspaceTabId::Invade);
    }

    /// Closing the game's tab pauses and keeps its run for reopening; only
    /// closing Snekkie ends and records it.
    fn close_invade(&mut self) {
        let order = self.workspace_order();
        let position = order.iter().position(|t| *t == WorkspaceTabId::Invade);
        let Some(mut game) = self.invade.take() else { return };
        let remaining: Vec<_> = order.iter().copied().filter(|t| *t != WorkspaceTabId::Invade).collect();
        self.set_workspace_order(&remaining);
        self.suppressed_keys.extend(game.held_keys());
        game.pause();
        self.invade_sidebar = false;
        if self.active == Some(WorkspaceTabId::Invade) {
            self.active = None;
            if let Some(next) = position.and_then(|p| neighbor(&order, p)) {
                self.select_workspace_tab(next);
            }
        }
        // The game's own warning closes with it; say it here instead.
        if let Some(warning) = game.save_warning() {
            self.message("Invade", warning);
        }
        self.parked_invade = Some(game);
    }

    fn close_workspace_tab(&mut self, id: WorkspaceTabId, confirmed: bool) {
        match id {
            WorkspaceTabId::Session(session) => self.close_tab(session, confirmed),
            WorkspaceTabId::Invade => self.close_invade(),
            WorkspaceTabId::Explorer(explorer) => self.close_explorer(explorer, confirmed),
        }
    }

    /// Close a file explorer, asking first if that would cut off a transfer.
    fn close_explorer(&mut self, id: u64, confirmed: bool) {
        let Some(index) = self.explorers.iter().position(|e| e.id == id) else { return };
        if !confirmed && let Some(warning) = self.explorers[index].close_warning() {
            let text = format!("{warning} Close “{}”?", self.explorers[index].title());
            self.confirm("Close file explorer", text, ConfirmAction::CloseExplorer(id));
            return;
        }
        let order = self.workspace_order();
        let position = order.iter().position(|t| *t == WorkspaceTabId::Explorer(id));
        let partner = self.take_split_partner(WorkspaceTabId::Explorer(id));
        let tab = self.explorers.remove(index);
        let remaining: Vec<_> = order.iter().copied().filter(|t| *t != WorkspaceTabId::Explorer(id)).collect();
        self.set_workspace_order(&remaining);
        self.remember_folders(&tab);
        if self.active == Some(WorkspaceTabId::Explorer(id)) {
            self.active = None;
            if let Some(next) = partner.or_else(|| position.and_then(|p| neighbor(&order, p))) {
                self.select_workspace_tab(next);
            }
        }
    }

    /// Note the saved-session tabs for next time, or forget them when the
    /// user doesn't want them back. Ad-hoc tabs and the game are left out.
    fn remember_tabs(&mut self) {
        let mut last = LastTabs::default();
        if self.settings.restore_tabs {
            let explorers = std::mem::take(&mut self.explorers);
            for explorer in &explorers {
                self.remember_folders(explorer);
            }
            self.explorers = explorers;
            let mut kept = Vec::new();
            for id in self.workspace_order() {
                let profile = match id {
                    WorkspaceTabId::Session(s) => self.tab_index(s).map(|i| (&self.tabs[i].session.profile, false)),
                    WorkspaceTabId::Explorer(e) => {
                        self.explorers.iter().find(|t| t.id == e).map(|t| (&t.profile, true))
                    }
                    WorkspaceTabId::Invade => None,
                };
                let saved = |p: &Profile| self.store.get(&p.name).is_some_and(|saved| same_destination(saved, p));
                if let Some((profile, explorer)) = profile.filter(|(p, _)| saved(p)) {
                    kept.push(id);
                    last.tabs.push(LastTab { profile: profile.name.clone(), explorer });
                }
            }
            let index = |id: &WorkspaceTabId| kept.iter().position(|t| t == id);
            last.active = self.active.as_ref().and_then(index);
            if let Some(split) = &self.split {
                let rows: Vec<Vec<_>> =
                    split.rows().iter().map(|column| column.iter().filter_map(index).collect()).collect();
                last.split = TabLayout::from_rows(&rows).map(|split| split.rows()).unwrap_or_default();
            }
        }
        if self.settings.last_tabs != last {
            self.settings.last_tabs = last;
            self.save_settings();
        }
    }

    /// Put back the tabs from last time, disconnected: nothing dials until
    /// the user chooses Reconnect. Profiles deleted since are skipped.
    fn restore_tabs(&mut self, ctx: &egui::Context) {
        let last = self.settings.last_tabs.clone();
        let mut ids = Vec::new();
        for entry in &last.tabs {
            let Some(profile) = self.store.get(&entry.profile).cloned() else {
                ids.push(None);
                continue;
            };
            let id = if entry.explorer {
                let id = crate::session::next_tab_id();
                // Never answered: the tab starts closed and connects on Reconnect.
                let (_, connection) = tokio::sync::oneshot::channel();
                let mut tab = ExplorerTab::new(id, profile, connection, None, self.explorer_context(ctx));
                tab.shutdown("Reopened from last time. Choose Reconnect to connect.");
                tab.slot = self.workspace_order().len();
                self.explorers.push(tab);
                WorkspaceTabId::Explorer(id)
            } else {
                let repaint = {
                    let ctx = ctx.clone();
                    move || ctx.request_repaint()
                };
                let session = Session::new(profile, self.settings.colors(), repaint);
                session.set_auto_paging(self.settings.auto_paging);
                session.shutdown();
                let color = crate::settings::parse_hex(&session.profile.tab_color);
                let id = session.id;
                self.tabs.push(Tab {
                    session,
                    view: TerminalView::default(),
                    color,
                    color_override: false,
                    bell: false,
                    waiting: None,
                    prompt: None,
                });
                WorkspaceTabId::Session(id)
            };
            ids.push(Some(id));
        }
        let id = |index: &usize| ids.get(*index).copied().flatten();
        let rows: Vec<Vec<_>> = last.split.iter().map(|column| column.iter().filter_map(id).collect()).collect();
        self.split = TabLayout::from_rows(&rows);
        if let Some(active) = last.active.as_ref().and_then(id).or_else(|| ids.iter().flatten().next().copied()) {
            self.select_workspace_tab(active);
        }
    }

    /// Reopen at these folders next time, if this is a saved session.
    fn remember_folders(&mut self, tab: &ExplorerTab) {
        let (local, remote) = tab.folders();
        if local.is_empty() && remote.is_empty() {
            return;
        }
        let Some(mut profile) =
            self.store.get(&tab.profile.name).cloned().filter(|p| same_destination(p, &tab.profile))
        else {
            return;
        };
        if profile.explorer_local != local || profile.explorer_remote != remote {
            local.clone_into(&mut profile.explorer_local);
            remote.clone_into(&mut profile.explorer_remote);
            self.save_profile_metadata(profile);
        }
    }

    fn explorer_context(&self, ctx: &egui::Context) -> ExplorerContext {
        let repaint = ctx.clone();
        let editor = self.explorer_editor.clone();
        ExplorerContext {
            runtime: self.runtime.handle().clone(),
            repaint: Arc::new(move || repaint.request_repaint()),
            opener: Arc::new(move |path| {
                let editor = editor.lock().clone();
                explorer::open_with(&editor, path)
            }),
            show_hidden: self.settings.explorer_show_hidden,
        }
    }

    /// Show a new explorer tab that's waiting on `connection`.
    fn add_explorer(
        &mut self,
        ctx: &egui::Context,
        profile: Profile,
        connection: crate::transport::sftp::Task<Result<ssh::Connection, String>>,
        banner: Option<Arc<BannerSink>>,
    ) {
        let id = crate::session::next_tab_id();
        let context = self.explorer_context(ctx);
        let mut tab = ExplorerTab::new(id, profile, connection, banner, context);
        tab.slot = self.workspace_order().len();
        self.explorers.push(tab);
        self.select_workspace_tab(WorkspaceTabId::Explorer(id));
    }

    /// Open an explorer on an SSH tab's connection, with no second login.
    fn open_explorer_from_session(&mut self, ctx: &egui::Context, id: u64) {
        let Some(index) = self.tab_index(id) else { return };
        let session = &self.tabs[index].session;
        let profile = session.profile.clone();
        let Some(shared) = session.share_connection() else {
            self.message("File explorer", "The explorer needs a connected SSH session.");
            return;
        };
        let connection = explorer::shared_connection(self.runtime.handle(), shared);
        self.add_explorer(ctx, profile, connection, None);
        if let Some(explorer) = self.explorers.last_mut() {
            explorer.session = Some(id);
        }
    }

    /// The active stores an export must never overwrite.
    fn protected_stores(&self) -> [PathBuf; 5] {
        [
            self.store.path.clone(),
            self.settings_store.path.clone(),
            self.settings_store.path.with_file_name("network.ini"),
            self.snippet_store.path.clone(),
            self.invade_path(),
        ]
    }

    fn is_protected_store(&self, path: &Path) -> bool {
        self.protected_stores().iter().any(|store| same_file_path(path, store))
    }

    fn clipboard_text(&mut self) -> Option<String> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()?.get_text().ok()
    }

    fn finish_text_export(&mut self, ctx: &egui::Context, path: Option<PathBuf>, export: TextExport) {
        let Some(path) = path else { return };
        if self.is_protected_store(&path) {
            self.message("Save terminal output", "Choose a different file. Saving terminal text over Snekkie's active profiles, preferences, snippets or Invade progress would replace saved data.");
            return;
        }
        let (result, verb) = match export {
            TextExport::Save(text) => (std::fs::write(&path, text), "Saved terminal text to"),
            TextExport::Append(text) => (append_terminal_text(&path, &text), "Appended terminal text to"),
        };
        match result {
            Ok(()) => self.flash(ctx, format!("{verb} {}", path.display())),
            Err(e) => self.message("Save terminal output", format!("Could not save {}: {e}", path.display())),
        }
    }

    fn host_key_asker(&self, ctx: &egui::Context) -> HostKeyAsker {
        let queue = self.host_keys.clone();
        let ctx = ctx.clone();
        Arc::new(move |question| {
            queue.lock().push_back(question);
            ctx.request_repaint();
        })
    }

    fn prompt_asker(&self, ctx: &egui::Context) -> PromptAsker {
        let queue = self.prompt_questions.clone();
        let ctx = ctx.clone();
        Arc::new(move |question| {
            queue.lock().push_back(question);
            ctx.request_repaint();
        })
    }

    fn live_sessions(&self) -> usize {
        self.tabs.iter().filter(|t| t.session.is_live()).count() + self.explorers.iter().filter(|e| e.is_live()).count()
    }

    /// Close the window without asking, hanging up every session.
    fn quit_now(&mut self, ctx: &egui::Context) {
        self.allow_close = true;
        for tab in &self.tabs {
            tab.session.shutdown();
        }
        // Remember where they were, and delete any copies open for editing.
        for tab in std::mem::take(&mut self.explorers) {
            self.remember_folders(&tab);
        }
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    // -- updating -------------------------------------------------------

    fn show_update(&mut self) {
        if self.settings.offline_mode {
            return;
        }
        if let updater::Status::Available(release) = &self.updater.status {
            let installable = self.updater.can_install && release.installer.is_some();
            self.dialogs.push(Dialog::Update { release: release.clone(), installable });
        }
    }

    /// Restart into the downloaded update, asking first if that would cut
    /// sessions off.
    fn request_install(&mut self, ctx: &egui::Context) {
        let live = self.live_sessions();
        let game = self.invade_unfinished();
        if live == 0 && !game {
            self.install_update(ctx);
        } else {
            let mut text = String::from("Snekkie will close to install the update, then start again.");
            if live > 0 {
                text.push_str(&format!(" This disconnects {live} session(s)."));
            }
            if game {
                text.push_str(" Your Invade run ends and is recorded.");
            }
            text.push_str(" Restart now?");
            self.confirm("Restart to update", text, ConfirmAction::InstallUpdate);
        }
    }

    fn install_update(&mut self, ctx: &egui::Context) {
        if self.settings.offline_mode {
            return;
        }
        let Some(installer) = self.updater.installer().cloned() else { return };
        match update::run_installer(&installer) {
            // The installer waits for this window to close.
            Ok(()) => self.quit_now(ctx),
            Err(e) => self.message("Update", format!("Could not start the installer: {e}")),
        }
    }

    /// A downloaded update installs by itself only when nothing would be
    /// cut off: no connected session, no open dialog, no unfinished game.
    fn can_install_silently(&self) -> bool {
        self.live_sessions() == 0 && !self.modal_open() && !self.invade_unfinished()
    }

    fn update_events(&mut self, ctx: &egui::Context) {
        self.updater.set_offline(self.settings.offline_mode);
        for event in self.updater.poll(ctx) {
            match event {
                updater::Event::UpToDate => self.message(
                    "Check for updates",
                    format!("You have the latest version, Snekkie {}.", update::CURRENT_VERSION),
                ),
                updater::Event::Found => self.show_update(),
                updater::Event::Failed(text) => self.message("Update", text),
                updater::Event::Downloaded => {
                    // Straight on to installing, unless that would cut off
                    // a session or something the user is in the middle of.
                    if self.can_install_silently() {
                        self.install_update(ctx);
                    } else {
                        self.flash(ctx, "Update downloaded. Click “Restart to update” when you're ready.");
                    }
                }
            }
        }
    }

    // -- connecting -----------------------------------------------------

    /// Collect anything we deliberately don't store, then connect.
    fn request_connect(&mut self, ctx: &egui::Context, profile: Profile, target: Target) {
        let jumps = if profile.kind().is_ssh() {
            match profiles::jump_chain(&profile, &self.store.profiles) {
                Ok(jumps) => jumps,
                Err(e) => return self.message("Jump host", e),
            }
        } else {
            Vec::new()
        };
        if profile.kind().is_ssh() {
            match profile.auth() {
                Auth::Password => {
                    let user = if profile.username.is_empty() { "(user)" } else { &profile.username };
                    let label = format!("Password for {user}@{}:", profile.host);
                    self.input(
                        "SSH password",
                        label,
                        "",
                        true,
                        InputAction::Password(Box::new(PendingConnect { profile, target, jumps })),
                    );
                    return;
                }
                Auth::Key if !profile.key_file.is_empty() => {
                    // Only ask for a passphrase if the key actually has one.
                    let path = ssh::expand_home(&profile.key_file);
                    if let Err(russh::keys::Error::KeyIsEncrypted) = russh::keys::load_secret_key(&path, None) {
                        let label = format!("Passphrase for {}:", path.display());
                        self.input(
                            "Key passphrase",
                            label,
                            "",
                            true,
                            InputAction::Passphrase(Box::new(PendingConnect { profile, target, jumps })),
                        );
                        return;
                    }
                }
                _ => {}
            }
        }
        self.start_connect(ctx, PendingConnect { profile, target, jumps }, String::new(), String::new());
    }

    fn start_connect(
        &mut self,
        ctx: &egui::Context,
        pending: PendingConnect,
        password: String,
        key_passphrase: String,
    ) {
        if let Target::Reconnect(id) | Target::AgentReconnect(id) = pending.target
            && let Some(index) = self.tab_index(id)
        {
            self.tabs[index].waiting = None;
        }
        if let Target::Reconnect(id) | Target::AgentReconnect(id) = pending.target
            && self.paste_queue.as_ref().is_some_and(|q| q.target.id == id)
        {
            self.paste_queue = None;
            self.flash(ctx, "Paste canceled by reconnect. Remaining lines were discarded.");
        }
        let cx = ConnectContext {
            runtime: self.runtime.handle(),
            ask_host_key: self.host_key_asker(ctx),
            ask_prompts: Some(self.prompt_asker(ctx)),
            jumps: pending.jumps.clone(),
            password,
            key_passphrase,
            network: self.network_access(ctx),
        };
        // A session of its own for the explorer, whichever way it came to need one.
        let explorer = match pending.target {
            Target::ReconnectExplorer(id) => Some(Some(id)),
            Target::NewTab if pending.profile.kind() == Kind::Sftp => Some(None),
            _ => None,
        };
        if let Some(existing) = explorer {
            let banner = Arc::new(BannerSink::default());
            let credentials = ssh::Credentials {
                password: cx.password,
                key_passphrase: cx.key_passphrase,
                known_hosts: ssh::known_hosts_path(),
                ask_prompts: cx.ask_prompts,
                jumps: cx.jumps,
            };
            let connection = ssh::connect(
                self.runtime.handle(),
                pending.profile.clone(),
                credentials,
                cx.ask_host_key,
                banner.clone(),
                cx.network,
            );
            match existing {
                Some(id) => {
                    if let Some(tab) = self.explorers.iter_mut().find(|e| e.id == id) {
                        tab.reconnect(connection, Some(banner));
                        self.select_workspace_tab(WorkspaceTabId::Explorer(id));
                    }
                }
                None => self.add_explorer(ctx, pending.profile, connection, Some(banner)),
            }
            return;
        }
        match pending.target {
            Target::ReconnectExplorer(_) => {}
            Target::NewTab => {
                let repaint = {
                    let ctx = ctx.clone();
                    move || ctx.request_repaint()
                };
                let mut session = Session::new(pending.profile, self.settings.colors(), repaint);
                session.set_auto_paging(self.settings.auto_paging);
                session.connect(cx);
                let mut view = TerminalView::default();
                view.focus();
                let color = crate::settings::parse_hex(&session.profile.tab_color);
                let id = session.id;
                self.tabs.push(Tab {
                    session,
                    view,
                    color,
                    color_override: false,
                    bell: false,
                    waiting: None,
                    prompt: None,
                });
                self.select_workspace_tab(WorkspaceTabId::Session(id));
            }
            Target::Reconnect(id) | Target::AgentReconnect(id) => {
                let Some(index) = self.tab_index(id) else { return };
                let tab = &mut self.tabs[index];
                tab.session.shutdown();
                tab.session.shared.emulator.lock().reset();
                // The view's animator and highlight cache still describe the old screen.
                tab.view.clear_highlight_cache();
                tab.view.reset_animations();
                tab.session.connect(cx);
                if pending.target == Target::Reconnect(id) {
                    self.select_workspace_tab(WorkspaceTabId::Session(id));
                }
            }
        }
    }

    /// Sessions whose reconnect waits for the user to type a password or passphrase.
    fn sessions_asking_user(&self) -> Vec<u64> {
        self.dialogs
            .iter()
            .filter_map(|dialog| match dialog {
                Dialog::Input { action: InputAction::Password(pending) | InputAction::Passphrase(pending), .. } => {
                    match pending.target {
                        Target::Reconnect(id) | Target::AgentReconnect(id) => Some(id),
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect()
    }

    fn handle_agent_reconnects(&mut self, ctx: &egui::Context) {
        let requests = self.agents.as_ref().map(|agents| agents.take_reconnects()).unwrap_or_default();
        for (id, agent) in requests {
            let Some(index) = self.tab_index(id) else { continue };
            let session = &self.tabs[index].session;
            // It may have changed since the agent asked, or already be asking the user.
            if !matches!(session.state(), State::Closed(Some(_))) || self.sessions_asking_user().contains(&id) {
                continue;
            }
            let profile = session.profile.clone();
            self.flash(ctx, format!("{agent} is reconnecting {}.", profile.name));
            self.request_connect(ctx, profile, Target::AgentReconnect(id));
        }
    }

    fn close_tab(&mut self, id: u64, confirmed: bool) {
        let Some(index) = self.tab_index(id) else { return };
        if !confirmed && self.tabs[index].session.is_live() {
            let name = self.tabs[index].session.profile.name.clone();
            self.confirm(
                "Close session",
                format!("“{name}” is still connected. Close it?"),
                ConfirmAction::CloseTab(id),
            );
            return;
        }
        let order = self.workspace_order();
        let position = order.iter().position(|t| *t == WorkspaceTabId::Session(id));
        let partner = self.take_split_partner(WorkspaceTabId::Session(id));
        let tab = self.tabs.remove(index);
        if index < self.invade_slot {
            self.invade_slot -= 1;
        }
        let remaining: Vec<_> = order.iter().copied().filter(|t| *t != WorkspaceTabId::Session(id)).collect();
        self.set_workspace_order(&remaining);
        if self.paste_queue.as_ref().is_some_and(|q| q.target.id == id) {
            self.paste_queue = None;
        }
        tab.session.shutdown();
        self.retired_log_workers.extend(tab.session.take_log_workers());
        if self.active == Some(WorkspaceTabId::Session(id)) {
            // The tab beside it, or else its neighbor, takes its place.
            self.active = None;
            if let Some(next) = partner.or_else(|| position.and_then(|p| neighbor(&order, p))) {
                self.select_workspace_tab(next);
            }
        } else if let Some(tab) = self.current_mut() {
            // A background tab closed: the selection, and the game if it is
            // selected, keep the keyboard.
            tab.view.focus();
        }
    }

    // -- commands -------------------------------------------------------

    fn run_command(&mut self, ctx: &egui::Context, command: Command) {
        if self.settings.offline_mode
            && matches!(command, Command::CheckForUpdates | Command::ShowUpdate | Command::InstallUpdate)
        {
            self.message("Offline mode", "Online updates are disabled. Turn off Offline mode under Settings → Preferences → Network to check or download updates.");
            return;
        }
        if self.invade_selected() && command.targets_session() {
            // Terminal commands never reach a connection from the game tab.
            self.flash(ctx, "Select a connection tab first: Invade is selected.");
            return;
        }
        match command {
            Command::NewSession => {
                self.settings.show_sidebar = true;
                self.sidebar.reset(&self.settings);
                if self.invade_selected() {
                    self.reveal_sidebar_over_invade();
                }
            }
            Command::Duplicate => {
                if let Some(profile) = self.current().map(|t| t.session.profile.clone()) {
                    self.request_connect(ctx, profile, Target::NewTab);
                }
            }
            Command::Reconnect => {
                if let Some((profile, id)) = self.current().map(|t| (t.session.profile.clone(), t.session.id)) {
                    self.request_connect(ctx, profile, Target::Reconnect(id));
                }
            }
            Command::CloseTab => {
                if let Some(id) = self.active {
                    self.close_workspace_tab(id, false);
                }
            }
            Command::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            Command::Copy => {
                if let Some(text) = self.current().and_then(|t| t.session.shared.emulator.lock().selection_text()) {
                    ctx.copy_text(text);
                }
            }
            Command::Paste => {
                if let Some(text) = self.clipboard_text() {
                    self.request_paste(ctx, &text);
                }
            }
            Command::SelectAll => {
                if let Some(tab) = self.current() {
                    tab.session.shared.emulator.lock().select_all();
                }
            }
            Command::Find => {
                if let Some(tab) = self.current_mut() {
                    // Selected text on one line is probably what to look for.
                    let selected = tab.session.shared.emulator.lock().selection_text();
                    tab.view.open_search(selected.filter(|t| !t.contains('\n') && t.chars().count() <= 200));
                }
            }
            Command::Snippets => self.snippets = Some(SnippetWindow::new(self.snippet_target())),
            Command::ClearScreen => {
                if let Some(tab) = self.current_mut() {
                    tab.session.shared.emulator.lock().clear();
                    tab.view.reset_animations();
                }
            }
            Command::ResetTerminal => {
                if let Some(tab) = self.current_mut() {
                    tab.session.shared.emulator.lock().reset();
                    tab.view.clear_highlight_cache();
                    tab.view.reset_animations();
                }
            }
            Command::SendBreak => {
                if let Some(tab) = self.current() {
                    tab.session.send_break();
                }
            }
            Command::ToggleSidebar => {
                if self.invade_selected() {
                    // Over the game the sidebar only shows on request; the
                    // saved preference stays as it is.
                    if self.invade_sidebar {
                        self.invade_sidebar = false;
                    } else {
                        self.reveal_sidebar_over_invade();
                    }
                } else {
                    self.settings.show_sidebar = !self.settings.show_sidebar;
                    self.save_settings();
                }
            }
            Command::ToggleButtonBar => {
                self.settings.button_bar = !self.settings.button_bar;
                self.save_settings();
            }
            Command::ToggleAutoPaging => {
                self.settings.auto_paging = !self.settings.auto_paging;
                for tab in &self.tabs {
                    tab.session.set_auto_paging(self.settings.auto_paging);
                }
                self.save_settings();
            }
            Command::Preferences => self.preferences = Some(Preferences::new(&self.settings)),
            Command::ImportPutty => self.dialogs.push(Dialog::Import(Box::new(PuttyImport::new(&self.settings)))),
            Command::ImportOpenSsh => {
                self.dialogs.push(Dialog::Import(Box::new(PuttyImport::openssh(ctx, &self.settings))));
            }
            Command::ImportProfiles => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Import Snekkie profiles")
                    .add_filter("Snekkie profiles (*.json)", &["json"])
                    .pick_file()
                {
                    let result = std::fs::File::open(&path)
                        .and_then(|file| {
                            let mut bytes = Vec::new();
                            file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                            Ok(bytes)
                        })
                        .map_err(|e| format!("Could not read {}: {e}", path.display()))
                        .and_then(|bytes| self.preview_profile_import(&bytes));
                    if let Err(error) = result {
                        self.message("Import profiles", error);
                    }
                }
            }
            Command::ExportProfiles => {
                self.dialogs.push(Dialog::ProfileTransfer(Box::new(ProfileTransfer::export(
                    &self.store.profiles,
                    self.sidebar.selected_saved(),
                ))));
            }
            Command::CheckForUpdates => match self.updater.status {
                updater::Status::Idle | updater::Status::Checking => {
                    self.flash(ctx, "Checking for updates…");
                    self.updater.check(ctx, true);
                }
                updater::Status::Available(_) => self.show_update(),
                updater::Status::Downloading { .. } => self.flash(ctx, "The update is downloading."),
                updater::Status::Ready { .. } => self.request_install(ctx),
            },
            Command::ShowUpdate => self.show_update(),
            Command::InstallUpdate => self.request_install(ctx),
            Command::Shortcuts => {
                self.dialogs.push(Dialog::Message {
                    title: "Keyboard shortcuts".into(),
                    text: SHORTCUTS.into(),
                    monospace: true,
                });
            }
            Command::About => self.dialogs.push(Dialog::About {
                text: format!(
                    "Snekkie {}\nA tabbed SSH and serial terminal.\n\nFree software under the GNU GPL v3 or later.",
                    env!("CARGO_PKG_VERSION")
                ),
                focus: true,
            }),
            Command::NextTab | Command::PreviousTab => {
                let order = self.workspace_order();
                if !order.is_empty() {
                    let n = order.len();
                    let at = self.active.and_then(|a| order.iter().position(|t| *t == a)).unwrap_or(0);
                    let next = if command == Command::NextTab { (at + 1) % n } else { (at + n - 1) % n };
                    self.select_workspace_tab(order[next]);
                }
            }
        }
    }

    fn reveal_sidebar_over_invade(&mut self) {
        self.invade_sidebar = true;
        if let Some(game) = &mut self.invade {
            game.pause();
        }
    }

    fn sidebar_visible(&self) -> bool {
        if self.invade_selected() { self.invade_sidebar } else { self.settings.show_sidebar }
    }

    /// Keys still held from the game never type into a terminal: each is
    /// dropped, with its text, until it is released.
    fn suppress_held_game_keys(&mut self, ctx: &egui::Context) {
        if self.invade_selected() {
            // A pause clears the game's movement/fire state, but does not
            // release a physical key. Remember the actual input, including
            // releases in a frame that may also select or close a tab.
            self.suppressed_keys = ctx.input(|input| {
                [
                    Key::ArrowLeft,
                    Key::ArrowRight,
                    Key::ArrowUp,
                    Key::ArrowDown,
                    Key::A,
                    Key::D,
                    Key::W,
                    Key::S,
                    Key::Space,
                    Key::X,
                    Key::Escape,
                    Key::P,
                    Key::B,
                    Key::Num1,
                    Key::Num2,
                    Key::Num3,
                ]
                .into_iter()
                .filter(|key| {
                    input.keys_down.contains(key)
                        || input
                            .events
                            .iter()
                            .any(|event| matches!(event, Event::Key { key: event_key, .. } if event_key == key))
                })
                .collect()
            });
            self.suppressed_shift = ctx.input(|input| input.modifiers.shift);
            return;
        }
        if self.suppressed_keys.is_empty() && !self.suppressed_shift {
            return;
        }
        let suppressed = std::mem::take(&mut self.suppressed_keys);
        let text_of = |key: Key| match key {
            Key::Space => Some(" "),
            Key::A => Some("a"),
            Key::D => Some("d"),
            Key::W => Some("w"),
            Key::S => Some("s"),
            Key::X => Some("x"),
            Key::P => Some("p"),
            Key::B => Some("b"),
            Key::Num1 => Some("1"),
            Key::Num2 => Some("2"),
            Key::Num3 => Some("3"),
            _ => None,
        };
        let (still_down, suppressed_shift) = ctx.input_mut(|input| {
            // The frame's final modifier state may already reflect a
            // release after earlier shifted keys/text. Release in order.
            let mut suppressed_shift = self.suppressed_shift;
            input.events.retain(|event| match event {
                Event::ModifiersChanged(modifiers) => {
                    if !modifiers.shift {
                        suppressed_shift = false;
                    }
                    true
                }
                Event::Key { key, modifiers, .. } => {
                    let chord = modifiers.ctrl || modifiers.mac_cmd || modifiers.command;
                    !suppressed.contains(key) && !(suppressed_shift && modifiers.shift && !chord)
                }
                Event::Text(text) => {
                    !suppressed_shift
                        && !suppressed.iter().filter_map(|k| text_of(*k)).any(|t| text.eq_ignore_ascii_case(t))
                }
                _ => true,
            });
            (
                suppressed.iter().copied().filter(|key| input.keys_down.contains(key)).collect::<Vec<_>>(),
                suppressed_shift && input.modifiers.shift,
            )
        });
        self.suppressed_keys = still_down;
        self.suppressed_shift = suppressed_shift;
    }

    /// Keyboard shortcuts, taken out of the event queue before the
    /// terminal sees them.
    fn shortcuts(&mut self, ctx: &egui::Context) -> Vec<Command> {
        let terminal_had_focus = self.terminal_had_focus;
        ctx.input_mut(|input| {
            let mut commands = Vec::new();
            input.events.retain(|event| {
                let Event::Key { key, pressed: true, modifiers, .. } = event else { return true };
                match shortcut_command(*key, *modifiers, cfg!(target_os = "macos")) {
                    // Cmd+A also belongs to every native text editor.
                    Some(Command::SelectAll) if cfg!(target_os = "macos") && !terminal_had_focus => true,
                    Some(command) => {
                        commands.push(command);
                        false
                    }
                    None => true,
                }
            });
            commands
        })
    }

    // -- per-frame plumbing ---------------------------------------------

    /// Offline mode's policy, asking the user through a dialog.
    fn network_access(&self, ctx: &egui::Context) -> NetworkAccess {
        let queue = self.network_questions.clone();
        let ctx = ctx.clone();
        self.network.with_asker(Arc::new(move |question| {
            queue.lock().push_back(question);
            ctx.request_repaint();
        }))
    }

    /// One TCP connect to a saved session's port, off the UI thread.
    fn check_port(&mut self, ctx: &egui::Context, profile: &Profile) {
        let (host, port) = (profile.host.trim().to_string(), profile.port);
        self.flash(ctx, format!("Checking {host}:{port}…"));
        let access = self.network_access(ctx);
        let results = self.port_checks.clone();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let check = crate::transport::check_port(&host, port, std::time::Duration::from_secs(3), &access).await;
            results.lock().push(check.describe(&host, port));
            ctx.request_repaint();
        });
    }

    /// Check the tab's port in the background and reconnect once it accepts.
    fn wait_for_host(&mut self, ctx: &egui::Context, id: u64) {
        let Some(index) = self.tab_index(id) else { return };
        let profile = &self.tabs[index].session.profile;
        let (host, port) = (profile.host.trim().to_string(), profile.port);
        // Offline approval is per connection, so each check would ask again.
        if self.settings.offline_mode && !crate::transport::is_local_destination(&host) {
            self.message(
                "Reconnect when back",
                format!(
                    "Offline mode is on. Waiting only works for private or local IP addresses, because every \
                     check of {host} would need your approval. Reconnect by hand instead."
                ),
            );
            return;
        }
        let (mut reply, answer) = tokio::sync::oneshot::channel();
        // Never asks: in offline mode only local addresses get this far.
        let access = self.network.with_asker(Arc::new(|_| {}));
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let (first, give_up) = (std::time::Duration::from_secs(5), std::time::Duration::from_secs(30 * 60));
            tokio::select! {
                back = crate::transport::wait_for_port(&host, port, &access, first, give_up) => {
                    let _ = reply.send(back);
                    ctx.request_repaint();
                }
                // Stopped, reconnected by hand or the tab closed.
                () = reply.closed() => {}
            }
        });
        self.tabs[index].waiting = Some(answer);
    }

    fn poll_waiting(&mut self, ctx: &egui::Context) {
        let mut done = Vec::new();
        for tab in &mut self.tabs {
            let Some(answer) = &mut tab.waiting else { continue };
            match answer.try_recv() {
                Ok(back) => done.push((tab.session.id, tab.session.profile.clone(), back)),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => continue,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {}
            }
            tab.waiting = None;
        }
        for (id, profile, back) in done {
            if back {
                self.flash(ctx, format!("{} is accepting connections again. Reconnecting.", profile.name));
                self.request_connect(ctx, profile, Target::Reconnect(id));
            } else {
                let text = format!(
                    "Stopped waiting: {}:{} did not accept connections within 30 minutes.",
                    profile.host, profile.port
                );
                self.message(&profile.name, text);
            }
        }
    }

    /// Bells mark tabs out of sight until they're shown; OSC 52 clipboard
    /// writes go through only when the user allowed them.
    fn poll_remote_events(&mut self, ctx: &egui::Context) {
        if self.user_rules.source != self.settings.highlight_rules {
            self.user_rules = UserRules::compile(&self.settings.highlight_rules);
        }
        let shown: Vec<WorkspaceTabId> = match self.visible_split() {
            Some(split) => split.tabs(),
            None => self.active.into_iter().collect(),
        };
        let mut remote_copied = None;
        for tab in &mut self.tabs {
            let emulator = tab.session.shared.emulator.lock();
            let rang = emulator.take_bell();
            let copied = emulator.take_clipboard();
            if let Some(prompt) = parse_prompt(&emulator.cursor_line_text()) {
                tab.prompt = Some(prompt);
            }
            drop(emulator);
            tab.bell = (tab.bell || rang) && !shown.contains(&WorkspaceTabId::Session(tab.session.id));
            if let Some(text) = copied.filter(|_| self.settings.remote_clipboard) {
                ctx.copy_text(text);
                remote_copied = Some(tab.session.profile.name.clone());
            }
        }
        if let Some(name) = remote_copied {
            self.flash(ctx, format!("{name} set the clipboard (OSC 52)"));
        }
    }

    fn poll_background(&mut self, ctx: &egui::Context) {
        self.fonts.poll(ctx);
        if !std::mem::replace(&mut self.tabs_restored, true) && self.settings.restore_tabs {
            self.restore_tabs(ctx);
        }
        if let Some(receiver) = &self.folder_open {
            match receiver.try_recv() {
                Ok(result) => {
                    self.folder_open = None;
                    if let Err(error) = result {
                        self.message("Open log folder", error);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.folder_open = None;
                    self.message("Open log folder", "The folder opener stopped unexpectedly.");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        self.update_events(ctx);
        #[cfg(windows)]
        self.poll_tray(ctx);
        self.sync_agents(ctx);
        let questions: Vec<HostKeyQuestion> = self.host_keys.lock().drain(..).collect();
        self.dialogs.extend(questions.into_iter().map(|q| Dialog::HostKey(Box::new(q))));
        // A host key question whose connection gave up (tab closed, timed
        // out) has nobody left to answer.
        self.dialogs.retain(|d| !matches!(d, Dialog::HostKey(q) if q.reply.is_closed()));
        let prompts: Vec<PromptQuestion> = self.prompt_questions.lock().drain(..).collect();
        self.dialogs.extend(prompts.into_iter().map(|q| {
            let answers = vec![String::new(); q.prompts.len()];
            Dialog::Prompts(Box::new(q), answers, true)
        }));
        self.dialogs.retain(|d| !matches!(d, Dialog::Prompts(q, ..) if q.reply.is_closed()));
        let network_questions: Vec<_> = self.network_questions.lock().drain(..).collect();
        self.dialogs.extend(network_questions.into_iter().map(|q| Dialog::Network(Box::new(q))));
        self.dialogs.retain(|d| !matches!(d, Dialog::Network(q) if q.reply.is_closed()));

        let mut notices = Vec::new();
        for tab in &mut self.explorers {
            tab.poll(ctx);
        }
        self.poll_remote_events(ctx);
        self.poll_waiting(ctx);
        let checks: Vec<String> = self.port_checks.lock().drain(..).collect();
        for check in checks {
            self.flash(ctx, check);
        }
        for tab in &self.tabs {
            if tab.session.poll_transfer() {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
            for notice in tab.session.take_notices() {
                notices.push((tab.session.profile.name.clone(), notice));
            }
        }
        for (name, notice) in notices {
            match notice.level {
                NoticeLevel::Info => self.flash(ctx, format!("{name}: {}", notice.text)),
                NoticeLevel::Warning => self.message(&name, notice.text),
            }
        }

        let theme = self.settings.colors();
        if self.styled_theme != Some(theme) {
            if self.accent == Color32::TRANSPARENT {
                // Ctrl+minus and friends belong to the far end, not UI zoom.
                ctx.options_mut(|o| o.zoom_with_keyboard = false);
            }
            self.accent = theme.cursor;
            self.styled_theme = Some(theme);
            style::apply_theme(ctx, theme);
        }
    }

    /// Act on what was chosen from the notification-area icon's menu.
    #[cfg(windows)]
    fn poll_tray(&mut self, ctx: &egui::Context) {
        use super::tray::{AppTray, TrayCommand};
        if std::mem::take(&mut self.want_tray) {
            let repaint = ctx.clone();
            self.tray = Some(AppTray::new(move || repaint.request_repaint()));
        }
        let Some(tray) = &self.tray else { return };
        tray.set_agents(self.agents.is_some());
        let commands: Vec<TrayCommand> = tray.commands().collect();
        for command in commands {
            if matches!(command, TrayCommand::Show | TrayCommand::Quit | TrayCommand::QuitWithBridges) {
                // Quit may need to ask about live sessions, so show it too.
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            match command {
                TrayCommand::Show | TrayCommand::OpenApp => {}
                TrayCommand::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
                TrayCommand::QuitWithBridges => {
                    self.close_bridges_on_exit = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                TrayCommand::CloseBridges => {
                    crate::mcp::bridges::close();
                    self.flash(ctx, "AI agent bridge closed");
                }
                TrayCommand::ToggleAgents => {
                    let on = self.agents.is_none();
                    self.settings.agent_access = on;
                    // An open Preferences would otherwise undo this on OK.
                    if let Some(preferences) = &mut self.preferences {
                        preferences.settings.agent_access = on;
                    }
                    self.save_settings();
                    let state = if on { "started" } else { "stopped" };
                    self.flash(ctx, format!("AI agent server {state}"));
                }
            }
        }
    }

    /// Start or stop the agent server to match Preferences, and show it the tabs.
    fn sync_agents(&mut self, ctx: &egui::Context) {
        if self.settings.agent_access != self.agents.is_some() {
            self.agents = None;
            if self.settings.agent_access {
                let repaint = ctx.clone();
                let file = self.agents_file.clone();
                match crate::mcp::Server::start(self.runtime.handle(), file, move || repaint.request_repaint()) {
                    Ok(server) => self.agents = Some(server),
                    Err(e) => {
                        // Not saved, so the next start tries again.
                        self.settings.agent_access = false;
                        self.message("AI agent access", format!("Could not start the agent server: {e}"));
                    }
                }
            }
        }
        self.handle_agent_reconnects(ctx);
        let asking = self.sessions_asking_user();
        let Some(agents) = &self.agents else {
            self.agent_activity = Default::default();
            return;
        };
        agents.sync_asking(asking);
        let sessions: Vec<&Session> = self.tabs.iter().map(|tab| &tab.session).collect();
        agents.sync_files(self.settings.agent_file_access, self.explorers.iter().map(ExplorerTab::agent_entry));
        agents.sync(self.active_index().map(|index| self.tabs[index].session.id), &sessions);
        self.agent_activity = agents.activity();
        let now = std::time::Instant::now();
        if self.agent_activity.sessions.values().any(|used| used.cue(now).is_some()) {
            // Keep animating, or at least clear the cue once it is over.
            let wait = if self.settings.agent_animations { 16 } else { 100 };
            ctx.request_repaint_after(std::time::Duration::from_millis(wait));
        }
    }

    fn agent_cue(&self, id: u64) -> Option<Cue> {
        self.agent_activity.sessions.get(&id)?.cue(std::time::Instant::now())
    }

    fn agent_motion(&self, ctx: &egui::Context) -> Option<Motion> {
        let speed = self.settings.animations.speed;
        self.settings.agent_animations.then(|| Motion { time: ctx.input(|i| i.time) * f64::from(speed), speed })
    }

    /// Who is connected and, while one works, what it is doing. The slot's
    /// width never changes, so the status bar can't wrap and resize the terminal.
    fn agent_badge(&self, ui: &mut Ui) {
        let width = (ui.available_width() * 0.4).min(AGENT_BADGE_WIDTH);
        let agents = &self.agent_activity.agents;
        let now = std::time::Instant::now();
        let busiest = self
            .agent_activity
            .sessions
            .iter()
            .filter_map(|(id, used)| {
                let name = match self.tabs.iter().find(|t| t.session.id == *id) {
                    Some(tab) => tab.session.profile.name.clone(),
                    None => self.explorers.iter().find(|e| e.id == *id)?.title(),
                };
                Some((used.cue(now)?, used, name))
            })
            .max_by_key(|(_, used, _)| used.last);
        let (text, color) = match (agents.as_slice(), &busiest) {
            (_, Some((cue, used, name))) => {
                let doing = match *cue {
                    Cue::Reading => format!("{} is reading {name}", used.agent),
                    Cue::Writing(_) => format!("{} is typing in {name}", used.agent),
                };
                (doing, agents::text_color(ui, *cue))
            }
            ([], None) => ("Agent access on".to_string(), style::secondary(ui)),
            ([one], None) => (format!("{one} connected"), style::primary(ui)),
            (many, None) => (format!("{} agents connected", many.len()), style::primary(ui)),
        };
        let hover = if agents.is_empty() {
            format!("{text}\nLocal AI agents can use open sessions. None is connected.")
        } else {
            format!("{text}\nConnected: {}", agents.join(", "))
        };
        let height = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(vec2(width, height), Layout::left_to_right(Align::Center), |ui| {
            ui.set_width(width);
            if !agents.is_empty() || busiest.is_some() {
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                let dot_color = if busiest.is_some() { color } else { self.accent };
                ui.painter().circle_filled(dot.center(), 3.5, dot_color);
            }
            ui.add(egui::Label::new(RichText::new(&text).color(color)).truncate()).on_hover_text(hover);
        });
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allow_close {
            return;
        }
        let live = self.live_sessions();
        if live == 0 {
            return;
        }
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        if !self.dialogs.iter().any(|d| matches!(d, Dialog::Confirm { action: ConfirmAction::Quit, .. })) {
            self.confirm("Quit", format!("{live} session(s) still connected. Quit anyway?"), ConfirmAction::Quit);
        }
    }

    // -- UI -------------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut Ui) -> Vec<Command> {
        let mut commands = Vec::new();
        let saved: Vec<Profile> = self.store.profiles.clone();
        let mut open_saved: Option<Profile> = None;
        egui::MenuBar::new().ui(ui, |ui| {
            style::wordmark(ui, self.accent);
            let buttons = ["Session", "Edit", "Terminal", "View", "Settings", "Help"].map(|label| ui.button(label));
            // Resolve hover switching before drawing any popup, so it works
            // in either direction and only one menu is drawn per frame.
            let menu_open = buttons
                .iter()
                .any(|button| egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(button)));
            if menu_open && let Some(button) = buttons.iter().find(|button| button.hovered() && !button.clicked()) {
                let id = egui::Popup::default_response_id(button);
                if !egui::Popup::is_id_open(ui.ctx(), id) {
                    egui::Popup::open_id(ui.ctx(), id);
                }
            }
            let [session, edit, terminal, view, settings, help] = buttons;
            let mut item = |ui: &mut Ui, text: &str, shortcut: &str, command: Command| {
                let mut button = egui::Button::new(text);
                if !shortcut.is_empty() {
                    button = button.shortcut_text(shortcut_label(shortcut, cfg!(target_os = "macos")));
                }
                if ui.add(button).clicked() {
                    commands.push(command);
                }
            };
            egui::Popup::menu(&session).show(|ui| {
                item(ui, "New session…", "Ctrl+Shift+N", Command::NewSession);
                ui.menu_button("Open saved", |ui| {
                    if saved.is_empty() {
                        ui.add_enabled(false, egui::Button::new("(none saved yet)"));
                    }
                    for profile in &saved {
                        if ui.button(format!("{}  —  {}", profile.name, profile.kind)).clicked() {
                            open_saved = Some(profile.clone());
                        }
                    }
                });
                item(ui, "Duplicate", "Ctrl+Shift+D", Command::Duplicate);
                item(ui, "Reconnect", "Ctrl+Shift+R", Command::Reconnect);
                ui.separator();
                item(ui, "Close tab", "Ctrl+W", Command::CloseTab);
                item(ui, "Exit", "Ctrl+Q", Command::Quit);
            });
            egui::Popup::menu(&edit).show(|ui| {
                item(ui, "Copy", "Ctrl+Shift+C", Command::Copy);
                item(ui, "Paste", "Ctrl+Shift+V", Command::Paste);
                item(ui, "Select all", "Ctrl+Shift+A", Command::SelectAll);
                ui.separator();
                item(ui, "Find…", "Ctrl+F", Command::Find);
                item(ui, "Command snippets…", "Ctrl+Shift+S", Command::Snippets);
            });
            egui::Popup::menu(&terminal).show(|ui| {
                let label = if self.settings.auto_paging {
                    "✔ Auto-page show commands"
                } else {
                    "   Auto-page show commands"
                };
                item(ui, label, "", Command::ToggleAutoPaging);
                ui.separator();
                item(ui, "Clear screen and scrollback", "Ctrl+Shift+L", Command::ClearScreen);
                item(ui, "Reset terminal", "", Command::ResetTerminal);
                ui.separator();
                item(ui, "Send break", "Ctrl+Shift+B", Command::SendBreak);
            });
            egui::Popup::menu(&view).show(|ui| {
                let label = if self.settings.show_sidebar { "✔ Sidebar" } else { "   Sidebar" };
                item(ui, label, "Ctrl+B", Command::ToggleSidebar);
                let label = if self.settings.button_bar { "✔ Button bar" } else { "   Button bar" };
                item(ui, label, "", Command::ToggleButtonBar);
                ui.separator();
                item(ui, "Next tab", "Ctrl+Tab", Command::NextTab);
                item(ui, "Previous tab", "Ctrl+Shift+Tab", Command::PreviousTab);
            });
            egui::Popup::menu(&settings).show(|ui| {
                item(ui, "Preferences…", "Ctrl+,", Command::Preferences);
                ui.separator();
                ui.menu_button("Import", |ui| {
                    item(ui, "Snekkie profiles (.json)…", "", Command::ImportProfiles);
                    item(ui, "PuTTY sessions…", "", Command::ImportPutty);
                    item(ui, "OpenSSH config (~/.ssh/config)…", "", Command::ImportOpenSsh);
                });
                item(ui, "Export profiles…", "", Command::ExportProfiles);
            });
            egui::Popup::menu(&help).show(|ui| {
                item(ui, "Keyboard shortcuts", "", Command::Shortcuts);
                item(ui, "Check for updates…", "", Command::CheckForUpdates);
                item(ui, "About Snekkie", "", Command::About);
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &self.updater.status {
                updater::Status::Available(release) => {
                    let button = style::accent_pill(&format!("Update to {}", release.version), self.accent);
                    if ui.add(button).on_hover_text(update_summary(release)).clicked() {
                        commands.push(Command::ShowUpdate);
                    }
                }
                updater::Status::Downloading { percent, .. } => {
                    ui.label(RichText::new(format!("Downloading update… {percent}%")).color(style::secondary(ui)));
                }
                updater::Status::Ready { release, .. } => {
                    let button = style::accent_pill("Restart to update", self.accent);
                    let hover = format!("Snekkie {} is downloaded and ready to install.", release.version);
                    if ui.add(button).on_hover_text(hover).clicked() {
                        commands.push(Command::InstallUpdate);
                    }
                }
                updater::Status::Idle | updater::Status::Checking => {}
            });
        });
        if let Some(profile) = open_saved {
            let ctx = ui.ctx().clone();
            self.request_connect(&ctx, profile, Target::NewTab);
        }
        commands
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let now = ui.input(|i| i.time);
        if self.flash.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.flash = None;
        }
        ui.horizontal_wrapped(|ui| {
            if self.agents.is_some() {
                self.agent_badge(ui);
                ui.separator();
            }
            if let Some(queue) = &self.paste_queue {
                ui.label(format!("Paste: {}/{} line(s) — {}", queue.submitted, queue.total, queue.target.description))
                    .on_hover_text("Progress counts lines submitted to the transport, not device acknowledgments.");
                if ui.button("Stop paste").clicked() {
                    self.paste_queue = None;
                    self.flash(ui.ctx(), "Paste stopped. Already submitted lines cannot be recalled.");
                }
                ui.separator();
            }
            let text = match (&self.invade, self.invade_selected()) {
                (Some(game), true) => {
                    let status = game.status();
                    format!("Invade — {} · wave {} · score {}", status.state, status.wave, status.score)
                }
                _ => match self.active {
                    Some(WorkspaceTabId::Explorer(id)) => {
                        self.explorers.iter().find(|e| e.id == id).map_or_else(String::new, ExplorerTab::status_text)
                    }
                    _ => self.current().map_or_else(|| "No session".to_string(), |t| t.session.status_text()),
                },
            };
            if self.settings.offline_mode {
                ui.label(RichText::new("Offline mode").strong())
                    .on_hover_text("Online updates are disabled; DNS and non-local connections require approval.");
                ui.separator();
            }
            ui.label(RichText::new(text).color(style::secondary(ui)));
            let mut stop_waiting = None;
            let mut show_tunnels = None;
            if let Some(tab) = self.current() {
                if let Some((direction, local, progress)) = tab.session.transfer_progress() {
                    ui.separator();
                    ui.label(transfer_status(direction, &local, progress.bytes()));
                    if ui.button("Cancel transfer").clicked() {
                        progress.cancel();
                    }
                }
                let tunnels = tab.session.tunnels();
                if !tunnels.is_empty() {
                    ui.separator();
                    let up = tunnels.iter().filter(|t| t.state == TunnelState::Listening).count();
                    let text = RichText::new(format!("Tunnels {up}/{}", tunnels.len()));
                    let failed = tunnels.iter().any(|t| matches!(t.state, TunnelState::Failed(_)));
                    let text = if failed { text.color(ui.visuals().error_fg_color) } else { text };
                    if ui.button(text).on_hover_text("Show this tab's SSH tunnels").clicked() {
                        show_tunnels = Some(tab.session.id);
                    }
                }
                if let Some(mode) = tab.prompt.as_ref().and_then(|p| p.mode).filter(|_| tab.session.is_connected()) {
                    ui.separator();
                    let label = RichText::new(mode.label());
                    let label = if mode == CliMode::Config {
                        label.strong().color(Color32::BLACK).background_color(ui.visuals().warn_fg_color)
                    } else {
                        label.color(style::secondary(ui))
                    };
                    ui.label(label).on_hover_text("CLI mode from the device prompt");
                }
                if tab.waiting.is_some() {
                    ui.separator();
                    ui.label("Waiting for host…").on_hover_text("Reconnects when the port accepts connections again.");
                    if ui.button("Stop waiting").clicked() {
                        stop_waiting = Some(tab.session.id);
                    }
                }
                ui.separator();
                let mut private = tab.session.private_input();
                if ui.checkbox(&mut private, "Private input").on_hover_text("Suppress input and output logging until switched off. Use this for unrecognized sensitive prompts.").changed() {
                    tab.session.set_private_input(private);
                }
                if tab.session.password_input_protected() {
                    ui.label("Password input protected");
                } else if tab.session.profile.log_passwords && tab.session.log_status() == LogStatus::Active {
                    ui.label("Password logging ON");
                }
                match tab.session.log_status() {
                    LogStatus::Active => {
                        ui.label("Logging").on_hover_text(tab.session.log_path().display().to_string());
                    }
                    LogStatus::Failed(error) => {
                        ui.label(RichText::new("Logging failed").color(ui.visuals().error_fg_color))
                            .on_hover_text(error);
                    }
                    LogStatus::Off => {
                        ui.label(RichText::new("Logging off").color(style::secondary(ui)));
                    }
                }
                let path = tab.session.profile.log_path.clone();
                if !path.trim().is_empty()
                    && ui
                        .add_enabled(self.folder_open.is_none(), egui::Button::new("Open log folder").small())
                        .clicked()
                {
                    self.request_open_log_folder(ui.ctx(), tab.session.log_path());
                }
            }
            if let Some(index) = stop_waiting.and_then(|id| self.tab_index(id)) {
                self.tabs[index].waiting = None;
            }
            if show_tunnels.is_some() {
                self.tunnels_window = show_tunnels;
            }
            if let Some((flash, _)) = &self.flash {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(flash).color(self.accent));
                });
            }
        });
    }

    fn sidebar_ui(&mut self, ui: &mut Ui) {
        let actions = self.sidebar.ui(ui, &self.store, &self.fonts.monospace, self.accent);
        let ctx = ui.ctx().clone();
        for action in actions {
            match action {
                sidebar::Action::Connect(profile) => self.request_connect(&ctx, profile, Target::NewTab),
                sidebar::Action::Save(profile) => {
                    let suggested = profile.name.clone();
                    let label = if profile.folder.is_empty() {
                        "Name:".to_string()
                    } else {
                        format!("Name (in {}):", profile.folder)
                    };
                    self.input("Save session", label, suggested, false, InputAction::SaveSession(Box::new(profile)));
                }
                sidebar::Action::Delete(name) => {
                    self.confirm("Delete session", format!("Delete “{name}”?"), ConfirmAction::DeleteSaved(name));
                }
                sidebar::Action::NewFolder { parent, session } => {
                    self.input("New folder", "Name:", "", false, InputAction::NewFolder { parent, session });
                }
                sidebar::Action::RenameFolder(path) => {
                    let name = profiles::folder_leaf(&path).to_string();
                    self.input("Rename folder", "Name:", name, false, InputAction::RenameFolder(path));
                }
                sidebar::Action::DeleteFolder(path) => {
                    let count = self.store.sessions_in(&path);
                    let name = profiles::folder_leaf(&path);
                    let parent = match profiles::folder_parent(&path) {
                        "" => "the top of the list".to_string(),
                        parent => format!("“{parent}”"),
                    };
                    let text = if count == 0 {
                        format!("Delete the folder “{name}”?")
                    } else {
                        format!(
                            "Delete the folder “{name}”? Its {count} saved session(s) stay, and move up to {parent}."
                        )
                    };
                    self.confirm("Delete folder", text, ConfirmAction::DeleteFolder(path));
                }
                sidebar::Action::MoveFolder { path, into } => {
                    let to = profiles::join_folder(&into, profiles::folder_leaf(&path));
                    self.rename_folder(path, to);
                }
                sidebar::Action::MoveSession { name, folder } => {
                    if let Err(e) = self.store.move_session(&name, &folder) {
                        self.message("Move session", format!("Could not save sessions: {e}"));
                    }
                    // The form keeps saving to where its session now lives.
                    if self.sidebar.selected_saved() == Some(name.as_str()) || self.sidebar.draft_name() == name {
                        self.sidebar.set_folder(folder);
                    }
                }
                sidebar::Action::ToggleFolder(path) => {
                    if let Err(e) = self.store.toggle_collapsed(&path) {
                        self.message("Saved sessions", format!("Could not save sessions: {e}"));
                    }
                }
                sidebar::Action::NewSessionIn(path) => {
                    self.run_command(&ctx, Command::NewSession);
                    self.sidebar.set_folder(path);
                }
                sidebar::Action::ToggleFavorite(name) => {
                    if let Some(mut profile) = self.store.get(&name).cloned() {
                        profile.favorite = !profile.favorite;
                        self.save_profile_metadata(profile);
                    }
                }
                sidebar::Action::CheckPort(profile) => self.check_port(&ctx, &profile),
                sidebar::Action::ProfileColor(name) => {
                    if let Some(profile) = self.store.get(&name) {
                        self.dialogs.push(Dialog::ProfileColor {
                            name,
                            color: crate::settings::parse_hex(&profile.tab_color).unwrap_or(self.accent),
                            enabled: !profile.tab_color.is_empty(),
                        });
                    }
                }
                sidebar::Action::ImportProfiles => self.run_command(&ctx, Command::ImportProfiles),
                sidebar::Action::ExportProfiles => self.run_command(&ctx, Command::ExportProfiles),
                sidebar::Action::OtherDevice => {
                    self.input(
                        "Serial port",
                        "Device name or path (e.g. COM7, /dev/ttyUSB0):",
                        "",
                        false,
                        InputAction::OtherDevice,
                    );
                }
                sidebar::Action::OtherBaud => {
                    self.input(
                        "Serial speed",
                        "Baud rate:",
                        self.sidebar.draft.baud.to_string(),
                        false,
                        InputAction::OtherBaud,
                    );
                }
                sidebar::Action::BrowseKey => {
                    if let Some(path) = rfd::FileDialog::new().set_title("Select private key").pick_file() {
                        self.sidebar.set_key_file(path.display().to_string());
                    }
                }
                sidebar::Action::BrowseLog => {
                    if let Some(path) = rfd::FileDialog::new().set_title("Session log file").save_file() {
                        self.sidebar.set_log_path(path.display().to_string());
                    }
                }
                sidebar::Action::Warn(text) => self.message("Incomplete", text),
            }
        }
    }

    /// Save a metadata edit atomically before updating the form or open tabs.
    fn save_profile_metadata(&mut self, profile: Profile) {
        let original = self.store.profiles.clone();
        if let Err(error) = self.store.put(profile.clone()) {
            self.store.profiles = original;
            self.message("Save profile", format!("Could not save profile changes: {error}"));
            return;
        }
        if self.sidebar.draft.name == profile.name && same_destination(&self.sidebar.draft, &profile) {
            self.sidebar.draft.favorite = profile.favorite;
            self.sidebar.draft.tab_color.clone_from(&profile.tab_color);
        }
        for tab in &mut self.tabs {
            if tab.session.profile.name == profile.name && same_destination(&tab.session.profile, &profile) {
                tab.session.profile.favorite = profile.favorite;
                tab.session.profile.tab_color.clone_from(&profile.tab_color);
                if !tab.color_override {
                    tab.color = crate::settings::parse_hex(&profile.tab_color);
                }
            }
        }
    }

    fn request_open_log_folder(&mut self, ctx: &egui::Context, path: PathBuf) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        match std::thread::Builder::new().name("Open log folder".into()).spawn(move || {
            let result = open_log_folder(&path).map_err(|error| format!("Could not open the log folder: {error}"));
            let _ = sender.send(result);
            ctx.request_repaint();
        }) {
            Ok(_) => self.folder_open = Some(receiver),
            Err(error) => self.message("Open log folder", format!("Could not start the folder opener: {error}")),
        }
    }

    fn finish_profile_export(&mut self, ctx: &egui::Context, path: Option<PathBuf>, profiles: &[Profile]) {
        let Some(path) = path else { return };
        if self.is_protected_store(&path) {
            self.message("Export profiles", "Choose a different file. Exporting over Snekkie's active profiles, preferences, snippets or Invade progress would replace saved data.");
            return;
        }
        let result = crate::profiles::export_profiles(profiles)
            .and_then(|text| crate::config::write_atomic(&path, &text).map_err(|e| e.to_string()));
        match result {
            Ok(()) => self.flash(ctx, format!("Exported {} profile(s) to {}", profiles.len(), path.display())),
            Err(error) => self.message("Export profiles", format!("Could not export {}: {error}", path.display())),
        }
    }

    fn tab_strip(&mut self, ui: &mut Ui) -> Option<(WorkspaceTabId, TabAction)> {
        // Once its session closes, an explorer keeps the color it last had.
        for explorer in &mut self.explorers {
            if let Some(tab) = self.tabs.iter().find(|tab| Some(tab.session.id) == explorer.session) {
                explorer.color = tab.color;
            }
        }
        let mut action = None;
        let order = self.workspace_order();
        let mut rects = Vec::with_capacity(order.len());
        egui::ScrollArea::horizontal().id_salt("tab_strip").auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let shown = self.visible_split().map_or_else(Vec::new, SplitView::tabs);
                let arranged = self.split.as_ref().map_or_else(Vec::new, SplitView::tabs);
                let columns = self.split.as_ref().map_or(0, SplitView::column_count);
                for (index, &entry) in order.iter().enumerate() {
                    let candidates =
                        if entry == WorkspaceTabId::Invade { Vec::new() } else { self.split_candidates(entry) };
                    let selected = self.active == Some(entry) || shown.contains(&entry);
                    let in_split = arranged.contains(&entry);
                    // Hover fills must stay behind the labels and close button.
                    let background = ui.painter().add(egui::Shape::Noop);
                    if let WorkspaceTabId::Explorer(explorer_id) = entry {
                        let Some(tab) = self.explorers.iter().find(|e| e.id == explorer_id) else { continue };
                        let color = if selected { style::primary(ui) } else { style::secondary(ui) };
                        let tint = tab.color;
                        let (connected, connecting) = tab.status();
                        let status = match (connected, connecting) {
                            (true, _) => "Connected",
                            (_, true) => "Connecting",
                            _ => "Disconnected",
                        };
                        let waiting = tab.needs_attention();
                        let title = if waiting { format!("{} (needs you)", tab.title()) } else { tab.title() };
                        let inner = ui.scope_builder(
                            egui::UiBuilder::new()
                                .id(Id::new(("tab", "explorer", explorer_id)))
                                .sense(Sense::click_and_drag()),
                            |ui| {
                                Frame::new()
                                    .fill(tint.map_or(Color32::TRANSPARENT, |c| {
                                        c.gamma_multiply(if selected { 0.16 } else { 0.08 })
                                    }))
                                    .inner_margin(Margin::symmetric(10, 5))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 6.0;
                                            let dot = if connected { self.accent } else { style::secondary(ui) };
                                            ui.add(egui::Label::new(RichText::new("📁").color(dot)).selectable(false));
                                            ui.add(
                                                egui::Label::new(RichText::new(title).color(color)).selectable(false),
                                            );
                                            let close = ui.add(
                                                egui::Button::new(RichText::new("×").color(style::secondary(ui)))
                                                    .frame(false)
                                                    .small(),
                                            );
                                            if close.on_hover_text("Close tab").clicked() {
                                                action = Some((entry, TabAction::Close));
                                            }
                                        });
                                    });
                            },
                        );
                        let response = inner.response.on_hover_text(format!(
                            "{}\nSFTP {} — {status}",
                            tab.title(),
                            tab.profile.host
                        ));
                        let rect = response.rect;
                        if let Some(color) = tint {
                            let strip = egui::Rect::from_x_y_ranges(
                                rect.left()..=rect.left() + 3.0,
                                rect.top() + 2.0..=rect.bottom() - 2.0,
                            );
                            ui.painter().rect_filled(strip, 1.0, color);
                        }
                        if let Some(cue) = self.agent_cue(explorer_id) {
                            let motion = self.agent_motion(ui.ctx());
                            ui.painter()
                                .set(background, egui::Shape::rect_filled(rect, 3.0, agents::tab_fill(cue, motion)));
                            let clip = ui.clip_rect().expand2(vec2(0.0, 6.0));
                            agents::tab(&ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(clip), rect, cue, motion);
                        }
                        if selected {
                            ui.painter().hline(
                                rect.x_range(),
                                rect.bottom() - 1.0,
                                Stroke::new(1.0, tint.unwrap_or(self.accent)),
                            );
                        } else if response.hovered() {
                            ui.painter().set(
                                background,
                                egui::Shape::rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill),
                            );
                        }
                        if response.clicked() {
                            action = Some((entry, TabAction::Select));
                        }
                        if response.middle_clicked() {
                            action = Some((entry, TabAction::Close));
                        }
                        if response.drag_started() {
                            self.dragging_tab = Some(index);
                            action = Some((entry, TabAction::PickUp));
                        }
                        response.context_menu(|ui| {
                            if ui.add_enabled(!connected, egui::Button::new("Reconnect")).clicked() {
                                action = Some((entry, TabAction::Reconnect));
                            }
                            split_menu(ui, entry, in_split, columns, &candidates, &mut action);
                            ui.separator();
                            if ui.button("Close").clicked() {
                                action = Some((entry, TabAction::Close));
                            }
                        });
                        rects.push(rect);
                        continue;
                    }
                    let WorkspaceTabId::Session(session_id) = entry else {
                        // The game's tab: game actions and Close, nothing
                        // about connections or profiles.
                        let state = self.invade.as_ref().map_or("", |game| game.status().state);
                        let color = if selected { style::primary(ui) } else { style::secondary(ui) };
                        let inner = ui.scope_builder(
                            egui::UiBuilder::new().id(Id::new(("tab", "invade"))).sense(Sense::click_and_drag()),
                            |ui| {
                                Frame::new().inner_margin(Margin::symmetric(10, 5)).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        ui.add(
                                            egui::Label::new(RichText::new(">_").monospace().color(self.accent))
                                                .selectable(false),
                                        );
                                        ui.add(
                                            egui::Label::new(RichText::new("Invade").color(color)).selectable(false),
                                        );
                                        let close = ui.add(
                                            egui::Button::new(RichText::new("×").color(style::secondary(ui)))
                                                .frame(false)
                                                .small(),
                                        );
                                        if close.on_hover_text("Close Invade (reopening resumes the run)").clicked() {
                                            action = Some((entry, TabAction::Close));
                                        }
                                    });
                                });
                            },
                        );
                        let response = inner.response.on_hover_text(format!("Invade\n{state}"));
                        let rect = response.rect;
                        if selected {
                            ui.painter().hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(1.0, self.accent));
                        } else if response.hovered() {
                            ui.painter().set(
                                background,
                                egui::Shape::rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill),
                            );
                        }
                        if response.clicked() {
                            action = Some((entry, TabAction::Select));
                        }
                        if response.middle_clicked() {
                            action = Some((entry, TabAction::Close));
                        }
                        if response.drag_started() {
                            self.dragging_tab = Some(index);
                            action = Some((entry, TabAction::PickUp));
                        }
                        response.context_menu(|ui| {
                            if ui.button("Pause").clicked() {
                                action = Some((entry, TabAction::Pause));
                            }
                            if ui.button("Show build").clicked() {
                                action = Some((entry, TabAction::Build));
                            }
                            ui.separator();
                            if ui.button("Close").on_hover_text("Ends and records the run").clicked() {
                                action = Some((entry, TabAction::Close));
                            }
                        });
                        rects.push(rect);
                        continue;
                    };
                    let Some(tab) = self.tabs.iter().find(|t| t.session.id == session_id) else { continue };
                    let state = tab.session.state();
                    let color = if selected { style::primary(ui) } else { style::secondary(ui) };
                    let dot = match state {
                        State::Connected => self.accent,
                        State::Connecting => Color32::from_rgb(0xd0, 0xa0, 0x30),
                        State::Closed(_) => Color32::from_rgb(0xa0, 0x40, 0x40),
                    };
                    let status = match state {
                        State::Connected => "Connected",
                        State::Connecting => "Connecting",
                        State::Closed(_) if tab.waiting.is_some() => "Disconnected, waiting for host…",
                        State::Closed(_) => "Disconnected",
                    };
                    let id = tab.session.id;
                    // Register the tab behind its contents so the close button
                    // receives clicks instead of the selection/drag area.
                    let inner = ui.scope_builder(
                        egui::UiBuilder::new().id(Id::new(("tab", id))).sense(Sense::click_and_drag()),
                        |ui| {
                            Frame::new()
                                .fill(tab.color.map_or(Color32::TRANSPARENT, |c| {
                                    c.gamma_multiply(if selected { 0.16 } else { 0.08 })
                                }))
                                .inner_margin(Margin::symmetric(10, 5))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        let (dot_rect, indicator) =
                                            ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                                        match state {
                                            State::Connected => {
                                                ui.painter().circle_filled(dot_rect.center(), 3.5, dot);
                                            }
                                            State::Connecting => {
                                                ui.painter().circle_stroke(
                                                    dot_rect.center(),
                                                    3.5,
                                                    Stroke::new(1.5, dot),
                                                );
                                            }
                                            State::Closed(_) => {
                                                ui.painter().line_segment(
                                                    [dot_rect.left_top(), dot_rect.right_bottom()],
                                                    Stroke::new(1.5, dot),
                                                );
                                                ui.painter().line_segment(
                                                    [dot_rect.right_top(), dot_rect.left_bottom()],
                                                    Stroke::new(1.5, dot),
                                                );
                                            }
                                        }
                                        indicator.widget_info(|| {
                                            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, status)
                                        });
                                        ui.add(
                                            egui::Label::new(RichText::new(self.tab_label(tab)).color(color))
                                                .selectable(false),
                                        );
                                        if tab.bell {
                                            ui.label(RichText::new("🔔").color(self.accent))
                                                .on_hover_text("The bell rang in this tab");
                                        }
                                        let close = ui.add(
                                            egui::Button::new(RichText::new("×").color(style::secondary(ui)))
                                                .frame(false)
                                                .small(),
                                        );
                                        if close.on_hover_text("Close tab").clicked() {
                                            action = Some((entry, TabAction::Close));
                                        }
                                    });
                                });
                        },
                    );
                    let host = tab.prompt.as_ref().map_or_else(String::new, |p| format!("\nDevice: {}", p.host));
                    let response = inner.response.on_hover_text(format!(
                        "{}\n{} — {status}{host}",
                        tab.session.title(),
                        tab.session.description()
                    ));
                    let rect = response.rect;
                    if let Some(color) = tab.color {
                        let strip = egui::Rect::from_x_y_ranges(
                            rect.left()..=rect.left() + 3.0,
                            rect.top() + 2.0..=rect.bottom() - 2.0,
                        );
                        ui.painter().rect_filled(strip, 1.0, color);
                    }
                    if selected {
                        ui.painter().hline(
                            rect.x_range(),
                            rect.bottom() - 1.0,
                            Stroke::new(1.0, tab.color.unwrap_or(self.accent)),
                        );
                    } else if response.hovered() {
                        ui.painter().set(
                            background,
                            egui::Shape::rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill),
                        );
                    }
                    if let Some(cue) = self.agent_cue(id) {
                        let motion = self.agent_motion(ui.ctx());
                        ui.painter()
                            .set(background, egui::Shape::rect_filled(rect, 3.0, agents::tab_fill(cue, motion)));
                        // The glow reaches past the strip's scroll area into the margin around it.
                        let clip = ui.clip_rect().expand2(vec2(0.0, 6.0));
                        agents::tab(&ui.ctx().layer_painter(ui.layer_id()).with_clip_rect(clip), rect, cue, motion);
                    }
                    if response.clicked() {
                        action = Some((entry, TabAction::Select));
                    }
                    if response.middle_clicked() {
                        action = Some((entry, TabAction::Close));
                    }
                    if response.drag_started() {
                        // Like a browser: the tab you pick up is the one shown.
                        self.dragging_tab = Some(index);
                        action = Some((entry, TabAction::PickUp));
                    }
                    response.context_menu(|ui| {
                        if ui.button("Reconnect").clicked() {
                            action = Some((entry, TabAction::Reconnect));
                        }
                        if matches!(tab.session.profile.kind(), Kind::Ssh | Kind::Telnet | Kind::Raw) {
                            if tab.waiting.is_some() {
                                if ui.button("Stop waiting for host").clicked() {
                                    action = Some((entry, TabAction::StopWaiting));
                                }
                            } else if ui
                                .add_enabled(matches!(state, State::Closed(_)), egui::Button::new("Reconnect when back"))
                                .on_hover_text(
                                    "Check the port until the host accepts connections (up to 30 minutes), then reconnect.",
                                )
                                .clicked()
                            {
                                action = Some((entry, TabAction::WaitForHost));
                            }
                        }
                        if ui.button("Duplicate").clicked() {
                            action = Some((entry, TabAction::Duplicate));
                        }
                        ui.menu_button("Tab color", |ui| {
                            for (name, color) in TAB_COLORS {
                                if ui.add(egui::Button::new(name).fill(color.gamma_multiply(0.15))).clicked() {
                                    action = Some((entry, TabAction::Color(Some(color))));
                                    ui.close();
                                }
                            }
                            ui.separator();
                            if ui.button("Custom color…").clicked() {
                                action = Some((entry, TabAction::CustomColor));
                                ui.close();
                            }
                            if ui.button("Clear tab color").clicked() {
                                action = Some((entry, TabAction::Color(None)));
                                ui.close();
                            }
                            if ui.button("Use profile color").clicked() {
                                action = Some((entry, TabAction::UseProfileColor));
                                ui.close();
                            }
                            let saved = self
                                .store
                                .get(&tab.session.profile.name)
                                .is_some_and(|p| same_destination(p, &tab.session.profile));
                            if ui
                                .add_enabled(saved, egui::Button::new("Save tab color as profile default"))
                                .on_hover_text("Save this color for future tabs opened from the profile.")
                                .clicked()
                            {
                                action = Some((entry, TabAction::SaveProfileColor));
                                ui.close();
                            }
                        });
                        if tab.session.profile.kind() == Kind::Ssh {
                            ui.separator();
                            let ready = tab.session.can_transfer();
                            if ui.add_enabled(ready, egui::Button::new("Upload file…")).clicked() {
                                action = Some((entry, TabAction::Transfer(Direction::Upload)));
                            }
                            if ui.add_enabled(ready, egui::Button::new("Download file…")).clicked() {
                                action = Some((entry, TabAction::Transfer(Direction::Download)));
                            }
                            let connected = tab.session.is_connected();
                            if ui.add_enabled(connected, egui::Button::new("Open file explorer")).clicked() {
                                action = Some((entry, TabAction::OpenExplorer));
                            }
                            if ui.add_enabled(connected, egui::Button::new("Open file explorer beside")).clicked() {
                                action = Some((entry, TabAction::OpenExplorerBeside));
                            }
                            if ui.button("Tunnels…").clicked() {
                                action = Some((entry, TabAction::Tunnels));
                            }
                        }
                        split_menu(ui, entry, in_split, columns, &candidates, &mut action);
                        ui.separator();
                        if ui.button("Close").clicked() {
                            action = Some((entry, TabAction::Close));
                        }
                    });
                    rects.push(rect);
                }
            });
        });

        // Drag a tab sideways to reorder.
        if let Some(from) = self.dragging_tab {
            let (down, pointer) = ui.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos()));
            if !down {
                self.dragging_tab = None;
                self.tab_dropped = order.get(from).copied();
            } else if let Some(pos) = pointer
                && !self.workspace_area.is_some_and(|area| area.contains(pos))
                && let Some(to) = rects.iter().position(|r| r.x_range().contains(pos.x))
                && to != from
                && from < order.len()
            {
                // Identities move with their tabs, so the selection follows.
                let mut order = order;
                let moved = order.remove(from);
                order.insert(to, moved);
                self.set_workspace_order(&order);
                self.dragging_tab = Some(to);
            }
        }
        action
    }

    /// A connection tab's own actions, always aimed at its stable session.
    fn session_tab_action(&mut self, ctx: &egui::Context, id: u64, action: TabAction) {
        match action {
            TabAction::Select
            | TabAction::PickUp
            | TabAction::Close
            | TabAction::Pause
            | TabAction::Build
            | TabAction::ShowBeside(_)
            | TabAction::EndSplit
            | TabAction::TakeOutOfSplit
            | TabAction::SwapSplit => {}
            TabAction::Color(color) => {
                if let Some(index) = self.tab_index(id) {
                    self.tabs[index].color = color;
                    self.tabs[index].color_override = true;
                }
            }
            TabAction::UseProfileColor => {
                if let Some(index) = self.tab_index(id) {
                    let tab = &mut self.tabs[index];
                    tab.color = crate::settings::parse_hex(&tab.session.profile.tab_color);
                    tab.color_override = false;
                }
            }
            TabAction::SaveProfileColor => {
                if let Some(index) = self.tab_index(id) {
                    let tab = &self.tabs[index];
                    if let Some(mut profile) = self
                        .store
                        .get(&tab.session.profile.name)
                        .cloned()
                        .filter(|p| same_destination(p, &tab.session.profile))
                    {
                        profile.tab_color = tab.color.map(crate::settings::to_hex).unwrap_or_default();
                        self.save_profile_metadata(profile);
                    }
                }
            }
            TabAction::CustomColor => {
                if let Some(index) = self.tab_index(id) {
                    self.dialogs.push(Dialog::TabColor { id, color: self.tabs[index].color.unwrap_or(self.accent) });
                }
            }
            TabAction::OpenExplorer => self.open_explorer_from_session(ctx, id),
            TabAction::OpenExplorerBeside => {
                let before = self.explorers.len();
                self.open_explorer_from_session(ctx, id);
                if self.explorers.len() != before {
                    let explorer = self.explorers[before].id;
                    self.start_split(WorkspaceTabId::Explorer(explorer), WorkspaceTabId::Session(id));
                }
            }
            TabAction::Transfer(direction) => {
                self.dialogs.push(Dialog::Transfer {
                    id,
                    direction,
                    local: None,
                    remote: String::new(),
                    replace: false,
                });
            }
            TabAction::WaitForHost => self.wait_for_host(ctx, id),
            TabAction::Tunnels => self.tunnels_window = Some(id),
            TabAction::StopWaiting => {
                if let Some(index) = self.tab_index(id) {
                    self.tabs[index].waiting = None;
                }
            }
            TabAction::Reconnect | TabAction::Duplicate => {
                if let Some(index) = self.tab_index(id) {
                    let profile = self.tabs[index].session.profile.clone();
                    let target = if action == TabAction::Reconnect { Target::Reconnect(id) } else { Target::NewTab };
                    self.request_connect(ctx, profile, target);
                }
            }
        }
    }

    fn central(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let game_was_selected = self.invade_selected();
        let strip = ui.horizontal(|ui| {
            let label = if self.sidebar_visible() { "Hide sessions" } else { "Show sessions" };
            let shortcut = shortcut_label("Ctrl+B", cfg!(target_os = "macos"));
            if ui.button(label).on_hover_text(format!("Show or hide the session sidebar ({shortcut})")).clicked() {
                self.run_command(&ctx, Command::ToggleSidebar);
                ctx.request_repaint();
            }
            self.tab_strip(ui)
        });
        let tab_action = strip.inner;
        ui.painter().hline(
            ui.max_rect().x_range(),
            strip.response.rect.bottom() + 2.0,
            Stroke::new(1.0, style::border(ui)),
        );
        ui.add_space(6.0);
        if self.tabs.is_empty() && self.invade.is_none() && self.explorers.is_empty() {
            Frame::new().inner_margin(20.0).show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(420.0));
                ui.add_space((ui.available_height() * 0.12).min(48.0));
                ui.label(RichText::new("Workspace").size(24.0).strong());
                ui.add_space(8.0);
                ui.label("No session open.");
                ui.add(
                    egui::Label::new(RichText::new("SSH, SFTP, serial, Telnet or TCP.").color(style::secondary(ui)))
                        .wrap(),
                );
                ui.add_space(12.0);
                if ui.add(style::primary_button(ui, "New connection")).clicked() {
                    self.run_command(&ctx, Command::NewSession);
                    ctx.request_repaint();
                }
            });
            return;
        }

        if let Some((entry, action)) = tab_action {
            match (entry, action) {
                (_, TabAction::Select) => self.select_workspace_tab(entry),
                (_, TabAction::PickUp) => {
                    self.tab_drag_target = self.drag_target(entry);
                    self.select_workspace_tab(entry);
                }
                (_, TabAction::Close) => self.close_workspace_tab(entry, false),
                (_, TabAction::ShowBeside(other)) => self.start_split(entry, other),
                (_, TabAction::EndSplit) => self.split = None,
                (_, TabAction::TakeOutOfSplit) => {
                    self.take_split_partner(entry);
                }
                (_, TabAction::SwapSplit) => {
                    if let Some(split) = &mut self.split {
                        split.mirror();
                    }
                }
                (WorkspaceTabId::Invade, TabAction::Pause) => {
                    if let Some(game) = &mut self.invade {
                        game.pause();
                    }
                }
                (WorkspaceTabId::Invade, TabAction::Build) => {
                    self.select_workspace_tab(entry);
                    if let Some(game) = &mut self.invade {
                        game.show_build();
                    }
                }
                (WorkspaceTabId::Explorer(id), TabAction::Reconnect) => self.reconnect_explorer(&ctx, id),
                (WorkspaceTabId::Invade | WorkspaceTabId::Explorer(_), _)
                | (_, TabAction::Pause | TabAction::Build) => {}
                (WorkspaceTabId::Session(id), action) => self.session_tab_action(&ctx, id, action),
            }
        }
        if self.tabs.is_empty() && self.invade.is_none() && self.explorers.is_empty() {
            return;
        }
        let order = self.workspace_order();
        if self.active.is_none_or(|active| !order.contains(&active))
            && let Some(first) = order.first()
        {
            self.select_workspace_tab(*first);
        }
        let area = ui.available_rect_before_wrap();
        self.workspace_area = Some(area);
        self.tab_drag_ui(&ctx, area);
        if self.invade_selected() {
            self.invade_ui(ui);
            return;
        }
        // Mouse selection and closing happen above, before drawing this
        // terminal. All keyboard/clipboard events in that frame originated
        // in the game, including text that is not a gameplay control.
        if game_was_selected {
            ctx.input_mut(|input| {
                input.events.retain(|event| {
                    !matches!(event, Event::Key { .. } | Event::Text(_) | Event::Paste(_) | Event::Copy | Event::Cut)
                });
            });
            // This frame was laid out for the game, without the sidebar.
            // Drawing the terminal now would resize the connection twice
            // and clear its selection; it appears next frame at its size.
            ctx.request_repaint();
            return;
        }
        // Keys still held after this frame remain quarantined until release.
        self.suppress_held_game_keys(&ctx);
        if self.settings.button_bar && self.current().is_some() {
            self.button_bar_ui(ui);
        }
        if let Some(split) = self.visible_split().cloned() {
            self.split_ui(ui, &split);
        } else if let Some(active) = self.active {
            self.pane_ui(ui, active, true, true);
        }
    }

    /// Pinned snippets as buttons. A click opens the snippet's review page
    /// for the current tab; nothing is sent from here.
    fn button_bar_ui(&mut self, ui: &mut Ui) {
        let mut clicked = None;
        egui::ScrollArea::horizontal().id_salt("button_bar").auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal(|ui| {
                let pinned: Vec<_> = self.snippet_store.snippets.iter().filter(|s| s.pinned).collect();
                if pinned.is_empty() {
                    ui.label(
                        RichText::new("No buttons yet. Edit a command snippet and tick “Show on button bar”.")
                            .color(style::secondary(ui)),
                    );
                }
                for snippet in pinned {
                    if ui.button(&snippet.name).on_hover_text(snippet.group_label()).clicked() {
                        clicked = Some(snippet.clone());
                    }
                }
            });
        });
        ui.add_space(4.0);
        if let Some(snippet) = clicked {
            self.snippets = Some(SnippetWindow::review(self.snippet_target(), &snippet));
        }
    }

    /// Where snippets opened now would go: the current tab, if connected.
    fn snippet_target(&self) -> Option<SnippetTarget> {
        self.current().filter(|t| t.session.is_connected()).map(|t| SnippetTarget {
            id: t.session.id,
            generation: t.session.connection_generation(),
            description: format!("{} — {}", t.session.title(), t.session.description()),
        })
    }

    /// What a tab picked up now would go beside: the other tab of its pair,
    /// the tab on show, or else the one shown last or next to it.
    fn drag_target(&self, entry: WorkspaceTabId) -> Option<WorkspaceTabId> {
        if entry == WorkspaceTabId::Invade {
            return None;
        }
        if let Some(split) = self.visible_split().filter(|split| split.contains(entry))
            && let Some(other) = split.tabs().into_iter().find(|tab| *tab != entry)
        {
            return Some(other);
        }
        let order = self.workspace_order();
        let usable = |id: &WorkspaceTabId| *id != entry && *id != WorkspaceTabId::Invade && order.contains(id);
        self.active
            .filter(usable)
            .or(self.last_shown.filter(usable))
            .or_else(|| order.iter().copied().find(|id| usable(id)))
    }

    /// Where a dropped tab would land: that part of the workspace, and the
    /// layout it would make. `target` shows alone unless it's in the layout.
    fn tab_drop_zone(
        &self,
        area: egui::Rect,
        entry: WorkspaceTabId,
        target: WorkspaceTabId,
        position: egui::Pos2,
    ) -> Option<(egui::Rect, SplitView)> {
        let base = self
            .split
            .clone()
            .filter(|split| split.contains(target) && split.fits(area))
            .unwrap_or_else(|| TabLayout::single(target));
        let (pane, rect) = base.panes(area).into_iter().find(|(_, rect)| rect.contains(position))?;
        let edge = Edge::at(rect, position, base.tabs().len() > 1);
        let (edge, next) = base.dropped(entry, pane, edge)?;
        next.fits(area).then(|| (edge.zone(rect), next))
    }

    /// A tab dragged out of the strip: show what it would go beside, mark the
    /// side it would land on, and make the pair when it's let go.
    fn tab_drag_ui(&mut self, ctx: &egui::Context, area: egui::Rect) {
        let position = ctx.input(|i| i.pointer.latest_pos());
        let order = self.workspace_order();
        let target = self.tab_drag_target.filter(|id| order.contains(id));
        if let Some(entry) = self.tab_dropped.take() {
            let zone = target.zip(position).and_then(|(target, at)| self.tab_drop_zone(area, entry, target, at));
            if let Some((_, split)) = zone {
                self.split = Some(split);
            }
            self.tab_drag_target = None;
            if order.contains(&entry) {
                self.active = None;
                self.select_workspace_tab(entry);
            }
            return;
        }
        let Some(entry) = self.dragging_tab.and_then(|from| order.get(from).copied()) else { return };
        let (Some(target), Some(position)) = (target, position) else { return };
        // Moving off the strip shows the tab it would go beside. No focus
        // moves until the drop, so nothing typed lands anywhere new.
        let over = area.contains(position);
        self.active = Some(if over { target } else { entry });
        let Some((zone, _)) = self.tab_drop_zone(area, entry, target, position) else { return };
        let title = self.workspace_titles().get(self.dragging_tab.unwrap_or(0)).cloned().unwrap_or_default();
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("tab_drop_zone")));
        painter.rect_filled(zone, 4.0, self.accent.gamma_multiply(0.18));
        painter.rect_stroke(zone, 4.0, Stroke::new(2.0, self.accent), egui::StrokeKind::Inside);
        painter.text(
            zone.center(),
            egui::Align2::CENTER_CENTER,
            format!("Show “{title}” here"),
            egui::FontId::proportional(16.0),
            self.accent,
        );
    }

    /// The tabs of a layout with draggable dividers. The active one has the keyboard.
    fn split_ui(&mut self, ui: &mut Ui, split: &SplitView) {
        let ctx = ui.ctx().clone();
        let area = ui.available_rect_before_wrap();
        let Some(active) = self.active else { return };
        if !split.fits(area) {
            self.pane_ui(ui, active, true, true);
            return;
        }
        let panes = split.panes(area);
        let tabs = split.tabs();

        // Clicking in a pane gives it the keyboard before it draws, so a
        // right-click paste can only ever reach the pane that was clicked.
        if !self.modal_open()
            && !egui::Popup::is_any_open(&ctx)
            && !self.explorers.iter().any(|tab| tabs.contains(&WorkspaceTabId::Explorer(tab.id)) && tab.shows_dialog())
            && let Some(position) = ctx.input(|i| i.pointer.press_origin().filter(|_| i.pointer.any_pressed()))
        {
            for (id, rect) in &panes {
                if rect.contains(position) && self.active != Some(*id) {
                    self.select_workspace_tab(*id);
                }
            }
        }
        let active = self.active.unwrap_or(active);

        for (divider, rect) in split.dividers(area) {
            let response = ui.interact(rect, ui.id().with(("split_divider", divider)), Sense::drag());
            let (label, cursor, vertical) = match divider {
                Divider::Column(_) => ("Split divider", egui::CursorIcon::ResizeHorizontal, true),
                Divider::Row(_) => ("Row divider", egui::CursorIcon::ResizeVertical, false),
            };
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, label));
            let live = response.hovered() || response.dragged();
            if live {
                ctx.set_cursor_icon(cursor);
            }
            if response.dragged()
                && let Some(pointer) = response.interact_pointer_pos()
                && let Some(split) = &mut self.split
            {
                split.drag_divider(area, divider, pointer);
            }
            let stroke = Stroke::new(1.0, if live { self.accent } else { style::border(ui) });
            if vertical {
                ui.painter().vline(rect.center().x, rect.y_range(), stroke);
            } else {
                ui.painter().hline(rect.x_range(), rect.center().y, stroke);
            }
        }

        // A terminal keeps egui's focus while drawn, which would hold back
        // the shortcuts of an explorer that is active beside it.
        if matches!(active, WorkspaceTabId::Explorer(_)) {
            for tab in &tabs {
                if let WorkspaceTabId::Session(session) = tab {
                    let terminal = Id::new(("terminal", *session));
                    ctx.memory_mut(|memory| {
                        if memory.has_focus(terminal) {
                            memory.surrender_focus(terminal);
                        }
                    });
                }
            }
        }

        // One explorer takes desktop drops: the active one, else the first.
        let drops = if matches!(active, WorkspaceTabId::Explorer(_)) {
            Some(active)
        } else {
            tabs.iter().copied().find(|tab| matches!(tab, WorkspaceTabId::Explorer(_)))
        };
        for (id, rect) in panes {
            // Clear of the line that marks the active pane.
            let inner = rect.with_min_y(rect.top() + 4.0);
            let mut pane = ui.new_child(egui::UiBuilder::new().max_rect(inner).id_salt(("split_pane", id)));
            pane.set_clip_rect(inner.intersect(ui.clip_rect()));
            self.pane_ui(&mut pane, id, id == active, Some(id) == drops);
            if id == active {
                let line = egui::Rect::from_x_y_ranges(rect.x_range(), rect.top()..=rect.top() + 2.0);
                ui.painter().rect_filled(line, 0.0, self.accent);
            }
        }
        ui.allocate_rect(area, Sense::hover());
    }

    fn pane_ui(&mut self, ui: &mut Ui, id: WorkspaceTabId, focused: bool, drops: bool) {
        match id {
            WorkspaceTabId::Explorer(explorer) => self.explorer_ui(ui, explorer, focused, drops),
            WorkspaceTabId::Session(session) => {
                if let Some(index) = self.tab_index(session) {
                    self.session_pane(ui, index, focused);
                }
            }
            WorkspaceTabId::Invade => {}
        }
    }

    fn session_pane(&mut self, ui: &mut Ui, index: usize, focused: bool) {
        let ctx = ui.ctx().clone();
        ui.add_space(2.0);

        // Banner: why the session ended, or that it's still connecting.
        let (state, id, description) = {
            let tab = &self.tabs[index];
            (tab.session.state(), tab.session.id, tab.session.description())
        };
        let banner = match &state {
            State::Closed(reason) => {
                Some((style::BANNER_BG, reason.as_deref().unwrap_or("Session closed").to_string(), true))
            }
            State::Connecting => Some((style::BANNER_INFO_BG, format!("Connecting…   {}", description.trim()), false)),
            State::Connected => None,
        };
        if let Some((fill, text, reconnect)) = banner {
            let mut clicked = false;
            let theme = self.settings.colors();
            let fill = if theme.effects.is_super() { theme.selection } else { fill };
            Frame::new().fill(fill).inner_margin(Margin::symmetric(8, 5)).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::Sides::new().shrink_left().wrap().show(
                    ui,
                    |ui| {
                        ui.label(RichText::new(text).color(if theme.effects.is_super() {
                            theme.fg
                        } else {
                            Color32::from_rgb(0xee, 0xee, 0xee)
                        }))
                    },
                    |ui| {
                        if reconnect {
                            clicked = ui.button("Reconnect").clicked();
                        }
                    },
                );
            });
            if clicked {
                let profile = self.tabs[index].session.profile.clone();
                self.request_connect(&ctx, profile, Target::Reconnect(id));
            }
        }

        let theme = self.settings.colors();
        // A dialog from the explorer beside this terminal takes the input too.
        let beside_dialog = self.visible_split().is_some_and(|split| {
            self.explorers.iter().any(|tab| split.contains(WorkspaceTabId::Explorer(tab.id)) && tab.shows_dialog())
        });
        let (agent_cue, agent_motion) = (self.agent_cue(id), self.agent_motion(&ctx));
        let tab = &mut self.tabs[index];
        // egui moves keyboard focus on arrow and Tab presses before a newly
        // focused terminal can claim them, so a key still held from the game
        // could strand its focus. Keep it here until those keys are let go.
        if focused && !self.suppressed_keys.is_empty() {
            tab.view.focus();
        }
        let family = self.fonts.family(&ctx, &tab.session.profile.font_family);
        let (regular, bold) =
            Fonts::font_ids(family.as_deref(), fonts::points_to_pixels(tab.session.profile.font_size));
        let syntax = tab.session.profile.device_syntax.clone();
        let sending = self.paste_queue.as_ref().is_some_and(|q| q.target.id == tab.session.id);
        let mouse_input = self.dialogs.is_empty()
            && self.preferences.is_none()
            && self.snippets.is_none()
            && self.paste_preview.is_none()
            && !sending
            && !beside_dialog;
        let keyboard = focused && mouse_input && (!self.settings.show_sidebar || !self.sidebar.blocks_terminal_input());
        let animations = self.settings.animations;
        let options = ViewOptions {
            theme,
            syntax: &syntax,
            highlighting_intensity: self.settings.highlighting_intensity,
            user_rules: &self.user_rules,
            regular,
            bold,
            keyboard,
            mouse_input,
            right_click_paste: self.settings.right_click_paste,
            animations,
        };
        let area = ui.available_rect_before_wrap();
        let out = if theme.effects.is_super() {
            style::monitor(ui, theme, |ui| tab.view.show(ui, &tab.session, &options))
        } else {
            tab.view.show(ui, &tab.session, &options)
        };
        if let Some((at, cells)) = out.fresh_text
            && theme.effects.appearance() == crate::settings::AppAppearance::Space
        {
            self.space.text_landed(&ctx, at, cells);
        }
        if let Some(cue) = agent_cue {
            agents::terminal(&ui.painter().with_clip_rect(area.expand(8.0)), area, cue, agent_motion);
        }
        if let Some(text) = out.copy {
            ctx.copy_text(text);
        }
        match out.open_link {
            Some(Link::Url(_)) if self.settings.offline_mode => {
                self.flash(&ctx, "Offline mode is on: links don't open in the browser.");
            }
            Some(Link::Url(url)) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            Some(Link::Host(host)) => self.new_session_to(&ctx, host),
            None => {}
        }
        // Pasting goes to the active tab, so nothing else may ask for it.
        if !focused {
        } else if let Some(text) = out.paste_text {
            self.request_paste(&ctx, &text);
        } else if out.paste_requested
            && let Some(text) = self.clipboard_text()
        {
            self.request_paste(&ctx, &text);
        }
        if let Some(export) = out.text_export {
            let picker = rfd::FileDialog::new()
                .add_filter("Plain text (*.txt)", &["txt"])
                .add_filter("Cisco configuration (*.cisco)", &["cisco"])
                .add_filter("Markdown (*.md)", &["md"])
                .add_filter("Configuration (*.cfg, *.conf)", &["cfg", "conf"])
                .add_filter("Log files (*.log)", &["log"])
                .add_filter("All files (*.*)", &["*"]);
            let path = match &export {
                TextExport::Save(_) => {
                    picker.set_title("Save terminal output").set_file_name("terminal-output").save_file()
                }
                TextExport::Append(_) => picker.set_title("Append terminal text to existing file").pick_file(),
            };
            self.finish_text_export(&ctx, path, export);
        }
    }

    fn explorer_ui(&mut self, ui: &mut Ui, id: u64, keyboard: bool, drops: bool) {
        let ctx = ui.ctx().clone();
        let options = ExplorerOptions {
            keyboard,
            drops,
            interactive: !self.modal_open(),
            show_hidden: self.settings.explorer_show_hidden,
        };
        let Some(tab) = self.explorers.iter_mut().find(|e| e.id == id) else { return };
        match tab.ui(ui, &options) {
            Some(ExplorerAction::Reconnect) => self.reconnect_explorer(&ctx, id),
            Some(ExplorerAction::ShowHidden(show)) => {
                self.settings.explorer_show_hidden = show;
                self.save_settings();
            }
            None => {}
        }
    }

    fn reconnect_explorer(&mut self, ctx: &egui::Context, id: u64) {
        if let Some(profile) = self.explorers.iter().find(|e| e.id == id).map(|e| e.profile.clone()) {
            self.request_connect(ctx, profile, Target::ReconnectExplorer(id));
        }
    }

    fn invade_ui(&mut self, ui: &mut Ui) {
        // Menus, dialogs and the sidebar shown over the game pause it; none
        // of them is part of the game, and paced sends keep going.
        let interactive = !self.modal_open() && !egui::Popup::is_any_open(ui.ctx());
        let options = InvadeOptions { interactive };
        let Some(game) = self.invade.as_mut() else { return };
        let action = game.ui(ui, &options);
        // Resuming puts the sidebar shown over the game away again.
        if self.invade_sidebar && game.is_running() {
            self.invade_sidebar = false;
        }
        if action == Some(InvadeAction::Close) {
            self.close_invade();
        }
    }

    /// Show the top dialog, if any. Returns true while one is open.
    fn dialogs_ui(&mut self, ctx: &egui::Context) -> bool {
        let Some(dialog) = self.dialogs.last_mut() else { return false };
        let mut close = false;
        let mut result: Option<DialogResult> = None;
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));
        let escape = ctx.input(|i| i.key_pressed(Key::Escape));

        let modal = egui::Modal::new(Id::new("dialog")).show(ctx, |ui| {
            ui.set_max_width(if matches!(dialog, Dialog::HostKey(_)) { 540.0 } else { 460.0 });
            match dialog {
                Dialog::Import(import) => {
                    ui.set_max_width(550.0);
                    if let Some(import) = import.ui(ui) {
                        if import {
                            result = Some(DialogResult::Yes);
                        } else {
                            close = true;
                        }
                    }
                }
                Dialog::ProfileTransfer(transfer) => {
                    if let Some(accepted) = transfer.ui(ui) {
                        result = Some(if accepted { DialogResult::Yes } else { DialogResult::No });
                    }
                }
                Dialog::ProfileColor { name, color, enabled } => {
                    ui.heading("Default tab color");
                    ui.label(name.as_str());
                    ui.label("Use this color when opening the saved profile.");
                    ui.checkbox(enabled, "Use default tab color");
                    ui.add_enabled_ui(*enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.color_edit_button_srgba(color);
                            ui.label(crate::settings::to_hex(*color));
                        });
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            result = Some(DialogResult::Yes);
                        }
                        if ui.button("Cancel").clicked() {
                            result = Some(DialogResult::No);
                        }
                    });
                }
                Dialog::TabColor { color, .. } => {
                    ui.heading("Session tab color");
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        ui.color_edit_button_srgba(color);
                        ui.label(crate::settings::to_hex(*color));
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            result = Some(DialogResult::Yes);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            close = true;
                        }
                    });
                }
                Dialog::Transfer { direction, local, remote, replace, .. } => {
                    let upload = *direction == Direction::Upload;
                    ui.heading(if upload { "Upload file" } else { "Download file" });
                    ui.horizontal(|ui| {
                        if ui.button(if upload { "Choose file…" } else { "Save as…" }).clicked() {
                            let picker = rfd::FileDialog::new();
                            let picked = if upload { picker.pick_file() } else { picker.save_file() };
                            *local = picked.or(local.take());
                        }
                        ui.label(local.as_ref().map_or("Nothing chosen".into(), |p| p.display().to_string()));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Remote file");
                        ui.add(egui::TextEdit::singleline(remote).hint_text("/tmp/file.txt or flash:file.bin"));
                    });
                    if upload {
                        ui.checkbox(replace, "Replace the remote file if it exists");
                        if *replace {
                            ui.label("On devices with only SCP, a failed upload can leave the remote file incomplete.");
                        }
                    }
                    ui.horizontal(|ui| {
                        let ready = local.is_some() && !remote.trim().is_empty();
                        if ui.add_enabled(ready, egui::Button::new("Start")).clicked() {
                            result = Some(DialogResult::Yes);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            close = true;
                        }
                    });
                }
                Dialog::About { text, focus } => {
                    ui.heading("About Snekkie");
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(text.as_str()).wrap());
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let ok = ui.button("OK");
                        if std::mem::take(focus) {
                            ok.request_focus();
                        }
                        if ok.clicked() || escape {
                            close = true;
                        }
                        if ui.button("Invade").clicked() {
                            result = Some(DialogResult::Yes);
                        }
                    });
                }
                Dialog::Message { title, text, monospace } => {
                    ui.heading(title.as_str());
                    ui.add_space(6.0);
                    let text = if *monospace {
                        RichText::new(text.as_str()).monospace()
                    } else {
                        RichText::new(text.as_str())
                    };
                    ui.add(egui::Label::new(text).wrap());
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() || enter || escape {
                        close = true;
                    }
                }
                Dialog::Confirm { title, text, .. } => {
                    ui.heading(title.as_str());
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(text.as_str()).wrap());
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Yes").clicked() || enter {
                            result = Some(DialogResult::Yes);
                        }
                        if ui.button("No").clicked() || escape {
                            close = true;
                        }
                    });
                }
                Dialog::Network(question) => {
                    ui.heading(if question.address.is_some() {
                        "Allow public connection?"
                    } else {
                        "Allow DNS lookup?"
                    });
                    ui.add(egui::Label::new(question.text()).wrap());
                    ui.horizontal(|ui| {
                        if ui.button("Allow once").clicked() {
                            result = Some(DialogResult::Yes);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            close = true;
                        }
                    });
                }
                Dialog::Input { title, label, text, secret, focus, .. } => {
                    ui.heading(title.as_str());
                    ui.add_space(6.0);
                    ui.label(label.as_str());
                    let edit = ui.add(TextEdit::singleline(text).password(*secret).desired_width(f32::INFINITY));
                    if std::mem::take(focus) {
                        edit.request_focus();
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() || enter {
                            result = Some(DialogResult::Text(text.clone()));
                        }
                        if ui.button("Cancel").clicked() || escape {
                            close = true;
                        }
                    });
                }
                Dialog::Prompts(question, answers, focus) => {
                    ui.heading(if question.name.trim().is_empty() { "Login" } else { question.name.trim() });
                    ui.add_space(6.0);
                    ui.label(RichText::new(&question.host).color(style::secondary(ui)));
                    if !question.instructions.trim().is_empty() {
                        ui.add(egui::Label::new(question.instructions.trim()).wrap());
                    }
                    for (index, ((prompt, echo), answer)) in question.prompts.iter().zip(answers).enumerate() {
                        ui.label(prompt.trim());
                        let edit = ui.add(TextEdit::singleline(answer).password(!echo).desired_width(f32::INFINITY));
                        if index == 0 && std::mem::take(focus) {
                            edit.request_focus();
                        }
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() || enter {
                            result = Some(DialogResult::Yes);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            close = true;
                        }
                    });
                }
                Dialog::HostKey(question) => {
                    ui.heading("Unknown host key");
                    ui.add_space(6.0);
                    let host = if question.port == 22 {
                        question.host.clone()
                    } else {
                        format!("{} (port {})", question.host, question.port)
                    };
                    ui.add(
                        egui::Label::new(format!(
                            "{} {host} presented a host key that is not in known_hosts.",
                            match &question.jump_host {
                                Some(name) => format!("Jump host {name} at"),
                                None => "The server at".into(),
                            }
                        ))
                        .wrap(),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new("Key type").color(style::secondary(ui)));
                    ui.label(RichText::new(&question.key_type).monospace());
                    ui.add_space(4.0);
                    ui.label(RichText::new("Fingerprint").color(style::secondary(ui)));
                    ui.add(egui::Label::new(RichText::new(&question.fingerprint).monospace()).wrap());
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            "Only accept this if you can verify the fingerprint out of band. Accept and remember it?",
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Accept").clicked() {
                            result = Some(DialogResult::Yes);
                        }
                        if ui.button("Reject").clicked() || escape {
                            result = Some(DialogResult::No);
                        }
                    });
                }
                Dialog::Update { release, installable } => {
                    ui.heading("Update available");
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(update_summary(release)).wrap());
                    ui.add_space(4.0);
                    let how = if *installable {
                        "Snekkie downloads it, then restarts to install it, asking first if any sessions \
                         are connected. Saved sessions and settings are kept."
                    } else {
                        update::install_kind().update_instructions()
                    };
                    ui.add(egui::Label::new(RichText::new(how).color(style::secondary(ui))).wrap());
                    if *installable {
                        ui.add_space(4.0);
                        ui.hyperlink_to(format!("What's new in {}", release.version), &release.page);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        // Deliberately not on Enter: a check from the Help
                        // menu can open this while you're typing.
                        let go = if *installable { "Update now" } else { "Open release page" };
                        if ui.button(go).clicked() {
                            result = Some(DialogResult::Yes);
                        }
                        if ui.button("Later").clicked() || escape {
                            close = true;
                        }
                    });
                }
            }
        });
        // A stray click outside shouldn't reject a key or cancel a login.
        if modal.should_close() && !matches!(self.dialogs.last(), Some(Dialog::HostKey(_) | Dialog::Prompts(..))) {
            close = true;
        }

        if let Some(result) = result {
            let dialog = self.dialogs.pop().unwrap();
            self.finish_dialog(ctx, dialog, result);
        } else if close {
            let dialog = self.dialogs.pop().unwrap();
            self.finish_dialog(ctx, dialog, DialogResult::No);
        }
        true
    }

    fn finish_dialog(&mut self, ctx: &egui::Context, dialog: Dialog, result: DialogResult) {
        match (dialog, result) {
            (Dialog::About { .. }, DialogResult::Yes) => self.open_invade(),
            (Dialog::ProfileTransfer(transfer), DialogResult::Yes) => {
                let selected = transfer.profiles();
                if transfer.is_export() {
                    let path = rfd::FileDialog::new()
                        .set_title("Export Snekkie profiles")
                        .set_file_name("snekkie-profiles.json")
                        .add_filter("Snekkie profiles (*.json)", &["json"])
                        .save_file();
                    self.finish_profile_export(ctx, path, &selected);
                } else {
                    let original = self.store.profiles.clone();
                    self.store.profiles = crate::profiles::merge_import(&original, selected.iter().cloned());
                    self.store.profiles.sort_by_key(|p| p.name.to_lowercase());
                    if let Err(error) = self.store.save() {
                        self.store.profiles = original;
                        self.message("Import profiles", format!("Could not save imported profiles: {error}"));
                    } else {
                        self.flash(ctx, format!("Imported {} profile(s).", selected.len()));
                    }
                }
            }
            (Dialog::ProfileColor { name, color, enabled }, DialogResult::Yes) => {
                if let Some(mut profile) = self.store.get(&name).cloned() {
                    profile.tab_color = if enabled { crate::settings::to_hex(color) } else { String::new() };
                    self.save_profile_metadata(profile);
                }
            }
            (Dialog::Import(import), DialogResult::Yes) => {
                let selected = import.profiles();
                let count = selected.len();
                let original = self.store.profiles.clone();
                if import.is_openssh() {
                    self.store.profiles = crate::profiles::merge_import(&original, selected);
                    self.store.profiles.sort_by_key(|p| p.name.to_lowercase());
                } else {
                    self.store.profiles = crate::putty::merge(&original, selected);
                }
                if let Err(e) = self.store.save() {
                    self.store.profiles = original;
                    self.message(import.title(), format!("Could not save imported sessions: {e}"));
                } else if import.is_openssh() {
                    self.flash(ctx, format!("Imported {count} OpenSSH host(s)."));
                } else {
                    self.flash(ctx, format!("Imported {count} PuTTY session(s)."));
                }
            }
            (Dialog::TabColor { id, color }, DialogResult::Yes) => {
                if let Some(index) = self.tab_index(id) {
                    self.tabs[index].color = Some(color);
                    self.tabs[index].color_override = true;
                }
            }
            (Dialog::Transfer { id, direction, local: Some(local), remote, replace }, DialogResult::Yes) => {
                if let Some(index) = self.tab_index(id) {
                    self.tabs[index].session.start_transfer(direction, local, remote.trim().to_string(), replace);
                }
            }
            (Dialog::HostKey(question), result) => {
                let _ = question.reply.send(result == DialogResult::Yes);
            }
            (Dialog::Prompts(question, answers, _), result) => {
                let _ = question.reply.send((result == DialogResult::Yes).then_some(answers));
            }
            (Dialog::Network(question), result) => {
                let _ = question.reply.send(result == DialogResult::Yes);
            }
            (Dialog::Confirm { action, .. }, DialogResult::Yes) => match action {
                ConfirmAction::CloseTab(id) => self.close_tab(id, true),
                ConfirmAction::CloseExplorer(id) => self.close_explorer(id, true),
                ConfirmAction::Quit => self.quit_now(ctx),
                ConfirmAction::InstallUpdate => self.install_update(ctx),
                ConfirmAction::DeleteSaved(name) => {
                    if let Err(e) = self.store.remove(&name) {
                        self.message("Delete session", format!("Could not save sessions: {e}"));
                    }
                }
                ConfirmAction::DeleteFolder(path) => {
                    if let Err(e) = self.store.delete_folder(&path) {
                        self.message("Delete folder", format!("Could not save sessions: {e}"));
                    }
                }
                ConfirmAction::DeleteSnippet(id) => {
                    if let Err(error) = self.snippet_store.delete(id) {
                        self.message("Delete snippet", error);
                    }
                }
                ConfirmAction::ReplaceTheme(name) => {
                    if let Some(prefs) = self.preferences.as_mut()
                        && let Err(e) = prefs.save_theme(&name)
                    {
                        self.message("Save theme", e);
                    }
                }
                ConfirmAction::DeleteTheme(name) => {
                    if let Some(prefs) = self.preferences.as_mut() {
                        prefs.delete_theme(&name);
                    }
                }
            },
            // Staying open keeps the agents' bridges too; a later plain Quit leaves them.
            #[cfg(windows)]
            (Dialog::Confirm { action: ConfirmAction::Quit, .. }, _) => self.close_bridges_on_exit = false,
            (Dialog::Update { release, installable }, DialogResult::Yes) => {
                if installable {
                    self.updater.download(ctx);
                } else {
                    ctx.open_url(egui::OpenUrl::new_tab(release.page));
                }
            }
            (Dialog::Input { action, .. }, DialogResult::Text(text)) => match action {
                InputAction::Password(pending) => self.start_connect(ctx, *pending, text, String::new()),
                InputAction::Passphrase(pending) => self.start_connect(ctx, *pending, String::new(), text),
                InputAction::SaveSession(mut profile) => {
                    let name = text.trim();
                    if name.is_empty() {
                        return;
                    }
                    profile.name = name.to_string();
                    self.sidebar.set_name(profile.name.clone());
                    if let Err(e) = self.store.put(*profile) {
                        self.message("Save session", format!("Could not save sessions: {e}"));
                    }
                }
                InputAction::NewFolder { parent, session } => {
                    let path = profiles::join_folder(&parent, text.trim());
                    if let Some(problem) = profiles::folder_name_error(text.trim())
                        .map(str::to_string)
                        .or_else(|| self.store.folder_create_error(&path))
                    {
                        self.message("New folder", problem);
                        return;
                    }
                    let saved = self.store.create_folder(&path).and_then(|()| match &session {
                        Some(name) => self.store.move_session(name, &path),
                        None => Ok(()),
                    });
                    if let Err(e) = saved {
                        self.message("New folder", format!("Could not save sessions: {e}"));
                    }
                }
                InputAction::RenameFolder(path) => {
                    if let Some(problem) = profiles::folder_name_error(text.trim()) {
                        self.message("Rename folder", problem);
                        return;
                    }
                    let to = profiles::join_folder(profiles::folder_parent(&path), text.trim());
                    self.rename_folder(path, to);
                }
                InputAction::OtherDevice => {
                    let device = text.trim();
                    if !device.is_empty() {
                        self.sidebar.set_device(device.to_string());
                    }
                }
                InputAction::OtherBaud => match text.trim().parse::<u32>() {
                    Ok(baud) if baud > 0 => self.sidebar.set_baud(baud),
                    _ => self.message("Serial speed", format!("“{}” is not a baud rate.", text.trim())),
                },
                InputAction::SaveTheme => {
                    let name = text.trim().to_string();
                    let Some(prefs) = self.preferences.as_mut() else { return };
                    if prefs.would_replace(&name) {
                        self.confirm(
                            "Save theme",
                            format!("Replace the saved theme “{name}”?"),
                            ConfirmAction::ReplaceTheme(name),
                        );
                    } else if let Err(e) = prefs.save_theme(&name) {
                        self.message("Save theme", e);
                    }
                }
            },
            _ => {}
        }
    }

    fn preferences_ui(&mut self, ctx: &egui::Context, blocked: bool) {
        let Some(prefs) = self.preferences.as_mut() else { return };
        prefs.update_busy = self.updater.busy();
        prefs.network_sessions = self
            .tabs
            .iter()
            .filter(|t| t.session.profile.kind().is_network() && !matches!(t.session.state(), State::Closed(_)))
            .count()
            + self.explorers.iter().filter(|e| e.is_live()).count();
        let mut outcome = preferences::Outcome::Open;
        let modal = egui::Modal::new(Id::new("preferences")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(200.0, preferences::WIDTH));
            outcome = prefs.ui(ui, &mut self.fonts);
        });
        if modal.should_close() && !blocked {
            outcome = preferences::Outcome::Cancel;
        }
        match outcome {
            preferences::Outcome::Open => {}
            preferences::Outcome::Cancel => self.preferences = None,
            preferences::Outcome::Save(settings) => {
                let enabling_offline = settings.offline_mode && !self.settings.offline_mode;
                self.settings = AppSettings {
                    show_sidebar: self.settings.show_sidebar,
                    button_bar: self.settings.button_bar,
                    ..*settings
                };
                self.network.set_offline(self.settings.offline_mode);
                self.updater.set_offline(self.settings.offline_mode);
                self.explorer_editor.lock().clone_from(&self.settings.explorer_editor);
                if enabling_offline {
                    for tab in &self.tabs {
                        if tab.session.profile.kind() != Kind::Serial {
                            tab.session.shutdown();
                        }
                    }
                    for tab in &mut self.explorers {
                        tab.shutdown("Offline mode closed this connection.");
                    }
                    self.dialogs.retain(|d| !matches!(d, Dialog::Update { .. }));
                }
                for tab in &self.tabs {
                    tab.session.set_auto_paging(self.settings.auto_paging);
                }
                self.save_settings();
                self.preferences = None;
                let theme = self.settings.colors();
                self.accent = theme.cursor;
                style::apply_theme(ctx, theme);
                self.styled_theme = Some(theme);
                ctx.request_repaint();
            }
            preferences::Outcome::AskThemeName { suggested } => {
                self.input("Save theme", "Theme name:", suggested, false, InputAction::SaveTheme);
            }
            preferences::Outcome::ConfirmDelete(name) => {
                self.confirm("Delete theme", format!("Delete “{name}”?"), ConfirmAction::DeleteTheme(name));
            }
        }
    }

    fn snippet_target_valid(&self, target: &SnippetTarget) -> bool {
        self.tab_index(target.id).is_some_and(|index| {
            let session = &self.tabs[index].session;
            session.is_connected() && session.connection_generation() == target.generation
        })
    }

    fn request_paste(&mut self, ctx: &egui::Context, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.paste_queue.is_some() {
            self.message("Paste", "A paste is already running. Stop it before starting another.");
            return;
        }
        let Some(tab) = self.current().filter(|t| t.session.is_connected()) else {
            self.flash(ctx, "Choose a connected tab before pasting.");
            return;
        };
        let target = PasteTarget {
            id: tab.session.id,
            generation: tab.session.connection_generation(),
            description: format!("{} — {}", tab.session.title(), tab.session.description()),
        };
        if self.settings.preview_multiline_paste && crate::paste::multiline(text) {
            self.paste_preview = Some(PastePreview::new(target, text, self.settings.paste_delay_ms));
        } else if let Err(error) = self.start_paste(ctx, target, text, 0, true) {
            self.message("Paste", error);
        }
    }

    /// `bracketable` is for clipboard pastes: when the far end has bracketed
    /// paste on, the whole text goes as one wrapped block with no line delay,
    /// since the shell or editor takes it in at once. Snippets are commands
    /// meant to run line by line, so they are never bracketed.
    fn start_paste(
        &mut self,
        ctx: &egui::Context,
        target: PasteTarget,
        text: &str,
        delay_ms: u64,
        bracketable: bool,
    ) -> Result<(), String> {
        if self.paste_queue.is_some() {
            return Err("A paste is already running. Stop it before starting another.".into());
        }
        if !self.snippet_target_valid(&target) {
            return Err("The reviewed connection changed or closed. Review the commands again.".into());
        }
        let queue = PasteQueue::new(target, text, delay_ms, std::time::Instant::now());
        let index = self.tab_index(queue.target.id).ok_or("The destination tab closed.")?;
        let bracketed = bracketable && self.tabs[index].session.shared.emulator.lock().bracketed_paste();
        if bracketed || queue.total <= 1 || delay_ms == 0 {
            let bytes = if bracketed { keys::encode_bracketed_paste(text) } else { keys::encode_paste(text) };
            if !self.tabs[index].session.write_reviewed(queue.target.generation, bytes) {
                return Err("The destination disconnected before the paste could be submitted.".into());
            }
            self.flash(ctx, format!("Paste submitted to {}", queue.target.description));
        } else {
            self.paste_queue = Some(queue);
            ctx.request_repaint();
        }
        Ok(())
    }

    fn advance_paste(&mut self, ctx: &egui::Context) {
        if self.modal_open() {
            return;
        }
        let Some(mut queue) = self.paste_queue.take() else { return };
        if !self.snippet_target_valid(&queue.target) {
            self.flash(
                ctx,
                "Paste canceled: the reviewed connection changed or closed. Remaining lines were discarded.",
            );
            return;
        }
        let now = std::time::Instant::now();
        if let Some(line) = queue.next_line(now) {
            let index = self.tab_index(queue.target.id).unwrap();
            if !self.tabs[index].session.write_reviewed(queue.target.generation, line) {
                self.flash(ctx, "Paste canceled: the destination disconnected. Remaining lines were discarded.");
                return;
            }
        }
        if queue.complete() {
            self.flash(ctx, format!("Paste submitted to {}", queue.target.description));
        } else {
            ctx.request_repaint_after(queue.remaining_delay(now));
            self.paste_queue = Some(queue);
        }
    }

    fn paste_ui(&mut self, ctx: &egui::Context, blocked: bool) {
        let Some(mut preview) = self.paste_preview.take() else { return };
        let valid = self.snippet_target_valid(&preview.target);
        let mut action = None;
        let modal = egui::Modal::new(Id::new("paste_preview")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 620.0));
            if blocked {
                ui.disable();
            }
            action = preview.ui(ui, valid);
        });
        if !blocked && modal.should_close() {
            action = Some(PasteAction::Cancel);
        }
        match action {
            Some(PasteAction::Send) => {
                if let Err(error) = self.start_paste(ctx, preview.target.clone(), &preview.text, preview.delay_ms, true)
                {
                    self.paste_preview = Some(preview);
                    self.message("Paste", error);
                }
            }
            Some(PasteAction::Cancel) => {}
            None => self.paste_preview = Some(preview),
        }
    }

    fn send_snippet(&mut self, ctx: &egui::Context, target: &SnippetTarget, text: &str) -> Result<(), String> {
        crate::snippets::validate_commands(text)?;
        if !self.snippet_target_valid(target) {
            return Err("The reviewed connection changed or closed. Reopen snippets on a connected tab and review the commands again.".into());
        }
        self.tab_index(target.id).ok_or("The destination tab closed.")?;
        let mut text = text.to_string();
        if !text.ends_with(['\r', '\n']) {
            text.push('\n');
        }
        self.start_paste(ctx, target.clone(), &text, self.settings.paste_delay_ms, false)?;
        self.select_workspace_tab(WorkspaceTabId::Session(target.id));
        Ok(())
    }

    /// A tab's SSH tunnels and what each is doing.
    fn tunnels_window_ui(&mut self, ctx: &egui::Context) {
        let Some(index) = self.tunnels_window.and_then(|id| self.tab_index(id)) else {
            self.tunnels_window = None;
            return;
        };
        let session = &self.tabs[index].session;
        let rows = session.tunnels();
        let mut open = true;
        egui::Window::new(format!("Tunnels — {}", session.profile.name))
            .id(Id::new("tunnels_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                if rows.is_empty() {
                    ui.label(if session.profile.tunnels.is_empty() {
                        "This session has no tunnels. Add them under Tunnels on its Advanced page."
                    } else {
                        "Tunnels start once the session is connected."
                    });
                }
                egui::Grid::new("tunnel_rows").striped(true).num_columns(3).show(ui, |ui| {
                    for row in &rows {
                        let label = ui.label(&row.label);
                        if !row.last_error.is_empty() {
                            label.on_hover_text(format!("Last connection failed: {}", row.last_error));
                        }
                        match &row.state {
                            TunnelState::Starting => ui.label("starting"),
                            TunnelState::Listening => ui.label("listening"),
                            TunnelState::Failed(reason) => {
                                ui.label(RichText::new(format!("error: {reason}")).color(ui.visuals().error_fg_color))
                            }
                        };
                        ui.label(format!("{} connection(s)", row.connections));
                        ui.end_row();
                    }
                });
            });
        if !open {
            self.tunnels_window = None;
        }
    }

    fn snippets_ui(&mut self, ctx: &egui::Context, blocked: bool) {
        let Some(mut window) = self.snippets.take() else { return };
        let target_valid = window.target.as_ref().is_some_and(|target| self.snippet_target_valid(target));
        let mut action = None;
        let modal = egui::Modal::new(Id::new("command_snippets")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(240.0, 620.0));
            if blocked {
                ui.disable();
            }
            action = window.ui(ui, &self.snippet_store, target_valid);
        });
        if !blocked && modal.should_close() {
            action = Some(SnippetAction::Close);
        }
        self.snippets = Some(window);
        match action {
            Some(SnippetAction::Close) => self.snippets = None,
            Some(SnippetAction::Save(snippet)) => match self.snippet_store.put(snippet) {
                Ok(id) => {
                    if let Some(window) = &mut self.snippets {
                        window.saved(id);
                    }
                }
                Err(error) => self.message("Save snippet", error),
            },
            Some(SnippetAction::Delete(id)) => {
                if let Some(snippet) = self.snippet_store.get(id) {
                    self.confirm(
                        "Delete snippet",
                        format!("Delete “{}” from {}?", snippet.name, snippet.group_label()),
                        ConfirmAction::DeleteSnippet(id),
                    );
                }
            }
            Some(SnippetAction::ImportFile) => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Import command snippets")
                    .add_filter("Snekkie snippets (*.json)", &["json"])
                    .pick_file()
                {
                    let result = std::fs::File::open(&path)
                        .and_then(|file| {
                            let mut bytes = Vec::new();
                            file.take(crate::snippets::MAX_FILE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
                            Ok(bytes)
                        })
                        .map_err(|e| format!("Could not read {}: {e}", path.display()))
                        .and_then(|bytes| self.preview_snippet_import(&bytes));
                    if let Err(error) = result {
                        self.message("Import snippets", error);
                    }
                }
            }
            Some(SnippetAction::Import(rows)) => match self.snippet_store.import(rows) {
                Ok(count) => {
                    if let Some(window) = &mut self.snippets {
                        window.imported();
                    }
                    self.flash(ctx, format!("Imported {count} snippet(s)."));
                }
                Err(error) => self.message("Import snippets", error),
            },
            Some(SnippetAction::Export(rows)) => {
                let path = rfd::FileDialog::new()
                    .set_title("Export command snippets")
                    .set_file_name("snekkie-snippets.json")
                    .add_filter("Snekkie snippets (*.json)", &["json"])
                    .save_file();
                self.finish_snippet_export(ctx, path, &rows);
            }
            Some(SnippetAction::Send(text)) => {
                let target = self.snippets.as_ref().and_then(|window| window.target.clone());
                let result = target
                    .as_ref()
                    .ok_or_else(|| "Choose a connected destination first.".to_string())
                    .and_then(|target| self.send_snippet(ctx, target, &text));
                match result {
                    Ok(()) => self.snippets = None,
                    Err(error) => self.message("Send commands", error),
                }
            }
            None => {}
        }
    }

    fn finish_snippet_export(&mut self, ctx: &egui::Context, path: Option<PathBuf>, snippets: &[Snippet]) {
        let Some(path) = path else { return };
        if self.is_protected_store(&path) {
            self.message("Export snippets", "Choose a different file. Exporting over Snekkie's active profiles, preferences, snippets or Invade progress would replace saved data.");
            return;
        }
        let result = crate::snippets::export_snippets(snippets)
            .and_then(|text| crate::config::write_atomic(&path, &text).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                if let Some(window) = &mut self.snippets {
                    window.imported();
                }
                self.flash(ctx, format!("Exported {} snippet(s) to {}", snippets.len(), path.display()));
            }
            Err(error) => self.message("Export snippets", format!("Could not export {}: {error}", path.display())),
        }
    }

    fn modal_open(&self) -> bool {
        !self.dialogs.is_empty()
            || self.preferences.is_some()
            || self.snippets.is_some()
            || self.paste_preview.is_some()
    }

    /// One frame of the whole app.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let command_origin = self.active;
        self.poll_background(&ctx);
        self.handle_close_request(&ctx);

        let modal_open = self.modal_open();
        if !modal_open {
            let focused = ctx.memory(|m| m.focused());
            self.terminal_had_focus =
                self.current().is_some_and(|t| focused == Some(Id::new(("terminal", t.session.id))));
        }
        self.suppress_held_game_keys(&ctx);
        let mut commands = if modal_open { Vec::new() } else { self.shortcuts(&ctx) };

        let theme = self.settings.colors();
        if theme.effects.appearance() == crate::settings::AppAppearance::Space {
            // First, so every panel draws over it.
            self.space.ui(ui, &theme, &self.settings.animations);
        }
        egui::Panel::top("menu")
            .frame(Frame::new().fill(ui.visuals().panel_fill).inner_margin(Margin::symmetric(10, 5)))
            .show(ui, |ui| commands.extend(self.menu_bar(ui)));
        egui::Panel::bottom("status")
            .frame(
                Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(Margin::symmetric(8, 3))
                    .stroke(Stroke::new(1.0, style::border(ui))),
            )
            .show(ui, |ui| self.status_bar(ui));
        if self.sidebar_visible() {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(320.0)
                .size_range(280.0..=460.0)
                .frame(Frame::new().fill(ui.visuals().panel_fill).inner_margin(Margin::same(12)))
                .show(ui, |ui| self.sidebar_ui(ui));
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(ui.visuals().faint_bg_color).inner_margin(Margin {
                left: 8,
                right: 8,
                top: 6,
                bottom: 6,
            }))
            .show(ui, |ui| self.central(ui));

        for command in commands {
            if command_origin == Some(WorkspaceTabId::Invade) && command == Command::CloseTab {
                // A mouse click may already have selected a connection or
                // closed the game. Close the shortcut's original tab once.
                self.close_workspace_tab(WorkspaceTabId::Invade, false);
                continue;
            }
            // Shortcuts were queued before a mouse click could select a
            // connection. Their origin remains the game even if it closed.
            if command_origin == Some(WorkspaceTabId::Invade) && command.targets_session() {
                self.flash(&ctx, "Terminal command ignored because it started in Invade.");
                continue;
            }
            self.run_command(&ctx, command);
        }

        // Preferences sits under any dialog it opened (theme name, confirm).
        let dialog_open = !self.dialogs.is_empty();
        self.preferences_ui(&ctx, dialog_open);
        self.snippets_ui(&ctx, dialog_open);
        self.tunnels_window_ui(&ctx);
        self.paste_ui(&ctx, dialog_open);
        self.dialogs_ui(&ctx);
        // Process Stop, close and reconnect before considering the next line.
        self.advance_paste(&ctx);

        // Back to typing where you left off once the last dialog closes.
        if modal_open
            && !self.modal_open()
            && self.terminal_had_focus
            && let Some(tab) = self.current_mut()
        {
            tab.view.focus();
        }
    }

    /// Hang up everything; called on exit.
    pub fn shutdown(&mut self) {
        // Closing Snekkie ends the game's run, open or parked; it is recorded once.
        if let Some(game) = self.invade.as_mut().or(self.parked_invade.as_mut()) {
            game.finish(EndReason::AppClosed);
            game.retry_failed_save();
        }
        self.paste_queue = None;
        self.paste_preview = None;
        for tab in &self.tabs {
            tab.session.shutdown();
        }
    }

    // -- test hooks -----------------------------------------------------

    #[doc(hidden)]
    pub fn paste_running(&self) -> bool {
        self.paste_queue.is_some()
    }

    #[doc(hidden)]
    pub fn paste_clipboard_text(&mut self, ctx: &egui::Context, text: &str) {
        self.request_paste(ctx, text);
    }

    /// Every tab's title in strip order; the game's is "Invade".
    #[doc(hidden)]
    #[doc(hidden)]
    pub fn explorer_colors(&self) -> Vec<Option<Color32>> {
        self.explorers.iter().map(|tab| tab.color).collect()
    }

    /// The titles of the tabs showing together, by column and then from the top.
    #[doc(hidden)]
    pub fn split_titles(&self) -> Option<Vec<Vec<String>>> {
        let split = self.split.as_ref()?;
        let order = self.workspace_order();
        let titles = self.workspace_titles();
        let title = |id: &WorkspaceTabId| order.iter().position(|tab| tab == id).map(|at| titles[at].clone());
        Some(split.rows().iter().map(|column| column.iter().filter_map(title).collect()).collect())
    }

    pub fn workspace_titles(&self) -> Vec<String> {
        self.workspace_order()
            .into_iter()
            .map(|entry| match entry {
                WorkspaceTabId::Invade => "Invade".to_string(),
                WorkspaceTabId::Explorer(id) => {
                    self.explorers.iter().find(|e| e.id == id).map(ExplorerTab::title).unwrap_or_default()
                }
                WorkspaceTabId::Session(id) => {
                    self.tab_index(id).map(|i| self.tabs[i].session.title()).unwrap_or_default()
                }
            })
            .collect()
    }

    #[doc(hidden)]
    pub fn active_title(&self) -> Option<String> {
        match self.active? {
            WorkspaceTabId::Invade => Some("Invade".into()),
            WorkspaceTabId::Explorer(id) => self.explorers.iter().find(|e| e.id == id).map(ExplorerTab::title),
            WorkspaceTabId::Session(_) => self.current().map(|t| t.session.title()),
        }
    }

    #[doc(hidden)]
    pub fn invade_status(&self) -> Option<super::invade::InvadeStatus> {
        self.invade.as_ref().map(|game| game.status())
    }

    #[doc(hidden)]
    pub fn invade_tab(&mut self) -> Option<&mut InvadeTab> {
        self.invade.as_deref_mut()
    }

    #[doc(hidden)]
    pub fn invade_progress_path(&self) -> PathBuf {
        self.invade_path()
    }

    /// Ask to restart into a downloaded update, as its button does.
    #[doc(hidden)]
    pub fn request_update_install(&mut self, ctx: &egui::Context) {
        self.request_install(ctx);
    }

    /// Whether a downloaded update would install by itself right now.
    #[doc(hidden)]
    pub fn update_would_install_automatically(&self) -> bool {
        self.can_install_silently()
    }

    /// Move a tab, by title, to `to` in the strip, as dragging would.
    #[doc(hidden)]
    pub fn move_tab(&mut self, title: &str, to: usize) {
        let mut order = self.workspace_order();
        let titles = self.workspace_titles();
        if let Some(from) = titles.iter().position(|t| t == title) {
            let moved = order.remove(from);
            order.insert(to.min(order.len()), moved);
            self.set_workspace_order(&order);
        }
    }

    #[doc(hidden)]
    pub fn tab_titles(&self) -> Vec<String> {
        self.tabs.iter().map(|t| t.session.title()).collect()
    }

    #[doc(hidden)]
    pub fn dialog_texts(&self) -> Vec<String> {
        self.dialogs
            .iter()
            .map(|d| match d {
                Dialog::Message { text, .. } | Dialog::About { text, .. } | Dialog::Confirm { text, .. } => {
                    text.clone()
                }
                Dialog::Input { label, .. } => label.clone(),
                Dialog::HostKey(q) => q.fingerprint.clone(),
                Dialog::Prompts(q, ..) => q.prompts.iter().map(|(p, _)| p.trim()).collect::<Vec<_>>().join(" "),
                Dialog::Network(q) => q.text(),
                Dialog::Update { release, .. } => update_summary(release),
                Dialog::Import(import) => import.title().into(),
                Dialog::ProfileTransfer(transfer) => {
                    if transfer.is_export() {
                        "Export profiles".into()
                    } else {
                        "Import profiles".into()
                    }
                }
                Dialog::TabColor { .. } => "Session tab color".into(),
                Dialog::Transfer { .. } => "File transfer".into(),
                Dialog::ProfileColor { name, .. } => format!("Default tab color for {name}"),
            })
            .collect()
    }

    /// Act as if the startup check had found `release`.
    #[doc(hidden)]
    pub fn offer_update(&mut self, release: Release, can_install: bool) {
        self.updater.status = updater::Status::Available(release);
        self.updater.can_install = can_install;
    }

    #[doc(hidden)]
    pub fn sidebar_draft(&mut self) -> &mut Profile {
        &mut self.sidebar.draft
    }

    #[doc(hidden)]
    pub fn open_session(&mut self, ctx: &egui::Context, profile: Profile) {
        self.request_connect(ctx, profile, Target::NewTab);
    }

    #[doc(hidden)]
    pub fn preview_putty_export(&mut self, bytes: &[u8]) -> Result<(), String> {
        let batch = crate::putty::parse_export(bytes, &self.settings)?;
        let mut import = PuttyImport::new(&self.settings);
        import.set_batch(batch);
        self.dialogs.push(Dialog::Import(Box::new(import)));
        Ok(())
    }

    #[doc(hidden)]
    pub fn preview_openssh_config(&mut self, path: &std::path::Path) -> Result<(), String> {
        let batch = crate::ssh_config::read(path, &self.settings)?;
        self.dialogs.push(Dialog::Import(Box::new(PuttyImport::openssh_batch(&self.settings, batch))));
        Ok(())
    }

    #[doc(hidden)]
    pub fn preview_profile_import(&mut self, bytes: &[u8]) -> Result<(), String> {
        let batch = crate::profiles::parse_import(bytes)?;
        self.dialogs.push(Dialog::ProfileTransfer(Box::new(ProfileTransfer::import(batch, &self.store.profiles))));
        Ok(())
    }

    #[doc(hidden)]
    pub fn preview_snippet_import(&mut self, bytes: &[u8]) -> Result<(), String> {
        let batch = crate::snippets::parse_import(bytes)?;
        let window = self.snippets.get_or_insert_with(|| SnippetWindow::new(None));
        window.preview_import(batch, &self.snippet_store.snippets);
        Ok(())
    }

    #[doc(hidden)]
    pub fn tab_colors(&self) -> Vec<(String, Option<Color32>)> {
        self.tabs.iter().map(|t| (t.session.title(), t.color)).collect()
    }

    #[doc(hidden)]
    pub fn set_form_kind(&mut self, kind: Kind) {
        self.sidebar.set_kind(kind);
    }

    #[doc(hidden)]
    pub fn active_screen_text(&self) -> Option<String> {
        self.current().map(|t| t.session.shared.emulator.lock().screen_text())
    }

    /// The current tab's find bar: its status text, if it's open.
    #[doc(hidden)]
    pub fn active_search_status(&self) -> Option<String> {
        self.current().filter(|t| t.view.search().is_open()).map(|t| t.view.search().status())
    }

    /// How far the current tab is scrolled back from the live screen.
    #[doc(hidden)]
    pub fn active_display_offset(&self) -> Option<usize> {
        self.current().map(|t| t.session.shared.emulator.lock().display_offset())
    }
}

fn update_summary(release: &Release) -> String {
    format!("Snekkie {} is available. You have {}.", release.version, update::CURRENT_VERSION)
}

/// Preserve the existing file byte-for-byte, adding a separator only if
/// needed. No create/truncate: append always targets an existing file.
fn append_terminal_text(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new().read(true).append(true).open(path)?;
    if text.is_empty() {
        return Ok(());
    }
    if file.metadata()?.len() != 0 {
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            file.write_all(b"\n")?;
        }
    }
    file.write_all(text.as_bytes())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabAction {
    Select,
    /// A drag began on this tab.
    PickUp,
    Close,
    /// Game tab only.
    Pause,
    Build,
    Reconnect,
    /// Wait for the host to accept connections again, then reconnect.
    WaitForHost,
    StopWaiting,
    Duplicate,
    Color(Option<Color32>),
    CustomColor,
    UseProfileColor,
    SaveProfileColor,
    Transfer(Direction),
    OpenExplorer,
    OpenExplorerBeside,
    Tunnels,
    /// Show this tab on the left and the named tab on the right.
    ShowBeside(WorkspaceTabId),
    EndSplit,
    TakeOutOfSplit,
    SwapSplit,
}

/// The side by side items of a tab's right-click menu.
fn split_menu(
    ui: &mut Ui,
    entry: WorkspaceTabId,
    in_split: bool,
    columns: usize,
    candidates: &[(WorkspaceTabId, String)],
    action: &mut Option<(WorkspaceTabId, TabAction)>,
) {
    ui.separator();
    ui.add_enabled_ui(!candidates.is_empty(), |ui| {
        ui.menu_button("Show side by side with", |ui| {
            for (other, title) in candidates {
                if ui.button(title).clicked() {
                    *action = Some((entry, TabAction::ShowBeside(*other)));
                    ui.close();
                }
            }
        });
    });
    if in_split {
        if columns > 1 && ui.button("Swap sides").clicked() {
            *action = Some((entry, TabAction::SwapSplit));
        }
        if ui.button("Take out of side by side").clicked() {
            *action = Some((entry, TabAction::TakeOutOfSplit));
        }
        if ui.button("End side by side").clicked() {
            *action = Some((entry, TabAction::EndSplit));
        }
    }
}

/// A status bar line for a running file transfer.
fn transfer_status(direction: Direction, local: &std::path::Path, (done, total): (u64, u64)) -> String {
    let verb = if direction == Direction::Upload { "Uploading" } else { "Downloading" };
    let file = local.file_name().unwrap_or_default().to_string_lossy();
    let megabytes = done as f64 / 1_048_576.0;
    match done.saturating_mul(100).checked_div(total) {
        Some(percent) => format!("{verb} {file}: {megabytes:.1} MB ({percent}%)"),
        None => format!("{verb} {file}: {megabytes:.1} MB"),
    }
}

/// The tab that takes a closed tab's place: the next one, or the previous
/// one if it was last. `order` still includes the closed tab.
fn neighbor(order: &[WorkspaceTabId], closed: usize) -> Option<WorkspaceTabId> {
    order.get(closed + 1).or_else(|| closed.checked_sub(1).and_then(|i| order.get(i))).copied()
}

fn same_destination(a: &Profile, b: &Profile) -> bool {
    a.kind() == b.kind()
        && if a.kind() == Kind::Serial {
            a.device.trim() == b.device.trim()
        } else {
            a.host.trim() == b.host.trim() && a.port == b.port
        }
}

fn log_folder(path: &Path) -> std::io::Result<PathBuf> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let folder = parent.canonicalize()?;
    if !folder.is_dir() {
        return Err(std::io::Error::other("The log folder is not a directory."));
    }
    Ok(folder)
}

fn same_file_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    fn resolved(path: &Path) -> std::io::Result<PathBuf> {
        path.canonicalize().or_else(|_| {
            let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            let Some(name) = path.file_name() else { return Err(std::io::Error::other("Missing file name")) };
            Ok(parent.canonicalize()?.join(name))
        })
    }
    match (resolved(a), resolved(b)) {
        (Ok(a), Ok(b)) => {
            #[cfg(windows)]
            {
                a.as_os_str().to_string_lossy().eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
            }
            #[cfg(not(windows))]
            {
                a == b
            }
        }
        _ => false,
    }
}

fn open_log_folder(path: &Path) -> std::io::Result<()> {
    let folder = log_folder(path)?;
    #[cfg(windows)]
    let mut command = std::process::Command::new("explorer.exe");
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    let output = command.arg(folder).output()?;
    // Explorer can return a nonzero exit when an existing shell opens the
    // folder. Other platforms report launcher failures through the exit code.
    #[cfg(not(windows))]
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        return Err(std::io::Error::other(format!("Folder opener exited with {}: {}", output.status, message.trim())));
    }
    #[cfg(windows)]
    let _ = output;
    Ok(())
}

const TAB_COLORS: [(&str, Color32); 8] = [
    ("Red", Color32::from_rgb(238, 87, 87)),
    ("Orange", Color32::from_rgb(243, 160, 61)),
    ("Yellow", Color32::from_rgb(227, 202, 70)),
    ("Green", Color32::from_rgb(98, 190, 109)),
    ("Blue", Color32::from_rgb(80, 156, 231)),
    ("Purple", Color32::from_rgb(177, 116, 231)),
    ("Pink", Color32::from_rgb(223, 122, 173)),
    ("Gray", Color32::from_rgb(148, 157, 164)),
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum DialogResult {
    Yes,
    No,
    Text(String),
}

impl eframe::App for SnekkieApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn on_exit(&mut self) {
        #[cfg(windows)]
        {
            self.tray = None;
            if self.close_bridges_on_exit {
                crate::mcp::bridges::close();
            }
        }
        self.agents = None;
        self.remember_tabs();
        self.shutdown();
        for tab in &self.tabs {
            tab.session.finish_logs();
        }
        for worker in self.retired_log_workers.drain(..) {
            let _ = worker.join();
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_shortcuts_use_command_and_preserve_physical_control_keys() {
        let command = Modifiers { mac_cmd: true, command: true, ..Modifiers::NONE };
        let control = Modifiers { ctrl: true, ..Modifiers::NONE };
        for (key, expected) in [
            (Key::W, Command::CloseTab),
            (Key::Q, Command::Quit),
            (Key::A, Command::SelectAll),
            (Key::F, Command::Find),
            (Key::B, Command::ToggleSidebar),
            (Key::Comma, Command::Preferences),
        ] {
            assert_eq!(shortcut_command(key, command, true), Some(expected));
            assert_eq!(shortcut_command(key, control, true), None, "Ctrl+{key:?} belongs to the device");
        }
        let shifted = Modifiers { shift: true, ..command };
        for (key, expected) in [
            (Key::N, Command::NewSession),
            (Key::D, Command::Duplicate),
            (Key::R, Command::Reconnect),
            (Key::S, Command::Snippets),
            (Key::L, Command::ClearScreen),
            (Key::B, Command::SendBreak),
        ] {
            assert_eq!(shortcut_command(key, shifted, true), Some(expected));
            assert_eq!(shortcut_command(key, Modifiers { shift: true, ..control }, true), None);
        }
        assert_eq!(shortcut_command(Key::Tab, command, true), None, "Cmd+Tab belongs to macOS");
        assert_eq!(shortcut_command(Key::Tab, control, true), Some(Command::NextTab));
        assert_eq!(shortcut_command(Key::Tab, Modifiers { shift: true, ..control }, true), Some(Command::PreviousTab));
        assert_eq!(shortcut_command(Key::W, Modifiers { ctrl: true, ..command }, true), None);
    }

    #[test]
    fn windows_and_linux_shortcuts_keep_their_control_bindings() {
        let control = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
        assert_eq!(shortcut_command(Key::W, control, false), Some(Command::CloseTab));
        assert_eq!(shortcut_command(Key::F, control, false), Some(Command::Find));
        assert_eq!(shortcut_command(Key::A, control, false), None, "plain Ctrl+A belongs to the device");
        assert_eq!(shortcut_command(Key::A, Modifiers { shift: true, ..control }, false), Some(Command::SelectAll));
        assert_eq!(shortcut_command(Key::PageDown, control, false), Some(Command::NextTab));
        assert_eq!(shortcut_command(Key::PageUp, control, false), Some(Command::PreviousTab));
        assert_eq!(shortcut_command(Key::F, Modifiers { alt: true, ..control }, false), None);
    }

    #[test]
    fn menu_shortcut_labels_match_each_platform() {
        for (original, macos) in [
            ("Ctrl+Shift+N", "Cmd+Shift+N"),
            ("Ctrl+F", "Cmd+F"),
            ("Ctrl+Shift+C", "Cmd+C"),
            ("Ctrl+Shift+V", "Cmd+V"),
            ("Ctrl+Shift+A", "Cmd+A"),
            ("Ctrl+Tab", "Ctrl+Tab"),
            ("Ctrl+Shift+Tab", "Ctrl+Shift+Tab"),
        ] {
            assert_eq!(shortcut_label(original, true), macos);
            assert_eq!(shortcut_label(original, false), original);
        }
    }

    #[test]
    fn profile_metadata_updates_matching_sessions_and_rolls_back_on_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let profile = Profile { name: "Saved".into(), host: "192.0.2.1".into(), ..Default::default() };
        app.store.put(profile.clone()).unwrap();
        app.sidebar.load(profile.clone());
        for (host, color_override) in [("192.0.2.1", false), ("192.0.2.1", true), ("192.0.2.2", false)] {
            let session = Session::new(Profile { host: host.into(), ..profile.clone() }, app.settings.colors(), || {});
            app.tabs.push(Tab {
                session,
                view: TerminalView::default(),
                color: None,
                color_override,
                bell: false,
                waiting: None,
                prompt: None,
            });
        }
        let updated = Profile { favorite: true, tab_color: "#abcdef".into(), ..profile };
        app.save_profile_metadata(updated.clone());
        assert_eq!(ProfileStore::open(app.store.path.clone()).get("Saved"), Some(&updated));
        assert!(app.sidebar.draft.favorite);
        assert_eq!(app.tabs[0].color, crate::settings::parse_hex("#abcdef"));
        assert_eq!(app.tabs[1].color, None, "explicit clear stays an override");
        assert_eq!(app.tabs[2].session.profile.tab_color, "", "same name at another destination is unrelated");
        app.store.path = dir.path().join("blocked");
        std::fs::create_dir(&app.store.path).unwrap();
        app.save_profile_metadata(Profile { favorite: false, tab_color: "#123456".into(), ..updated.clone() });
        assert_eq!(app.store.get("Saved"), Some(&updated));
        assert!(app.sidebar.draft.favorite);
        assert_eq!(app.tabs[0].color, crate::settings::parse_hex("#abcdef"));
        assert!(app.dialog_texts()[0].contains("Could not save profile changes"));
    }

    #[test]
    fn transfer_dialog_starts_only_with_a_file_and_a_remote_path() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        app.dialogs.push(Dialog::Transfer {
            id: 1,
            direction: Direction::Upload,
            local: None,
            remote: String::new(),
            replace: false,
        });
        let mut harness = egui_kittest::Harness::builder().with_size([700.0, 400.0]).build_ui_state(
            |ui, app: &mut SnekkieApp| {
                app.dialogs_ui(ui.ctx());
            },
            app,
        );
        harness.run();
        assert!(harness.get_by_label("Start").accesskit_node().is_disabled());
        assert!(harness.query_by_label_contains("incomplete").is_none());

        harness.get_by_label("Replace the remote file if it exists").click();
        harness.run();
        assert!(harness.query_by_label_contains("incomplete").is_some());

        if let Dialog::Transfer { local, remote, .. } = &mut harness.state_mut().dialogs[0] {
            *local = Some("source.bin".into());
            *remote = "flash:source.bin".into();
        }
        harness.run();
        assert!(!harness.get_by_label("Start").accesskit_node().is_disabled());
        harness.get_by_label("Start").click();
        harness.run();
        assert!(harness.state().dialogs.is_empty());
    }

    #[test]
    fn profile_export_protects_live_stores_and_handles_cancel_or_write_errors() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let profiles = [
            Profile { name: "Alpha".into(), host: "192.0.2.1".into(), ..Default::default() },
            Profile { name: "Beta".into(), host: "192.0.2.2".into(), ..Default::default() },
        ];
        for profile in &profiles {
            app.store.put(profile.clone()).unwrap();
        }
        let original = std::fs::read(&app.store.path).unwrap();
        let ctx = egui::Context::default();
        app.finish_profile_export(&ctx, None, &profiles[..1]);
        assert!(app.flash.is_none() && app.dialogs.is_empty());
        for path in [
            app.store.path.clone(),
            app.settings_store.path.clone(),
            dir.path().join("network.ini"),
            app.snippet_store.path.clone(),
            dir.path().join("invade.json"),
        ] {
            app.finish_profile_export(&ctx, Some(path), &profiles[..1]);
            assert!(app.dialog_texts().last().unwrap().contains("Choose a different file"));
            app.dialogs.clear();
        }
        assert_eq!(std::fs::read(&app.store.path).unwrap(), original);
        let export = dir.path().join("export.json");
        app.finish_profile_export(&ctx, Some(export.clone()), &profiles[..1]);
        let batch = crate::profiles::parse_import(&std::fs::read(export).unwrap()).unwrap();
        assert_eq!(batch.profiles, profiles[..1]);
        app.finish_profile_export(&ctx, Some(dir.path().to_path_buf()), &profiles);
        assert!(app.dialog_texts().last().unwrap().contains("Could not export"));
    }

    #[test]
    fn profile_import_write_failure_preserves_the_original_store() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let original = Profile { name: "Alpha".into(), host: "192.0.2.1".into(), ..Default::default() };
        app.store.put(original.clone()).unwrap();
        let incoming = crate::profiles::ProfileImport {
            profiles: vec![Profile { host: "192.0.2.2".into(), ..original.clone() }],
            warnings: Vec::new(),
        };
        let transfer = ProfileTransfer::import(incoming, &app.store.profiles);
        app.store.path = dir.path().join("blocked");
        std::fs::create_dir(&app.store.path).unwrap();
        app.finish_dialog(&egui::Context::default(), Dialog::ProfileTransfer(Box::new(transfer)), DialogResult::Yes);
        assert_eq!(app.store.profiles, [original]);
        assert!(app.tabs.is_empty());
        assert!(app.dialog_texts()[0].contains("Could not save imported profiles"));
    }

    #[test]
    fn snippet_exports_preserve_live_stores_and_write_only_the_selected_templates() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let id = app
            .snippet_store
            .put(Snippet { name: "Version".into(), template: "show version".into(), ..Default::default() })
            .unwrap();
        let rows = vec![app.snippet_store.get(id).unwrap().clone()];
        let ctx = egui::Context::default();
        app.finish_snippet_export(&ctx, None, &rows);
        assert!(app.dialogs.is_empty() && app.flash.is_none());
        for path in [
            app.store.path.clone(),
            app.settings_store.path.clone(),
            dir.path().join("network.ini"),
            app.snippet_store.path.clone(),
            dir.path().join("invade.json"),
        ] {
            if !path.exists() {
                std::fs::write(&path, "original data").unwrap();
            }
            let original = std::fs::read(&path).unwrap();
            app.finish_snippet_export(&ctx, Some(path.clone()), &rows);
            assert!(app.dialog_texts().last().unwrap().contains("Choose a different file"));
            assert_eq!(std::fs::read(&path).unwrap(), original);
            app.dialogs.clear();
        }
        let export = dir.path().join("export.json");
        app.finish_snippet_export(&ctx, Some(export.clone()), &rows);
        let batch = crate::snippets::parse_import(&std::fs::read(export).unwrap()).unwrap();
        assert_eq!(batch.snippets[0].template, rows[0].template);
        assert!(app.tabs.is_empty());
        app.finish_snippet_export(&ctx, Some(dir.path().to_path_buf()), &rows);
        assert!(app.dialog_texts().last().unwrap().contains("Could not export"));
    }

    #[test]
    fn device_prompt_names_serial_and_unsaved_tabs_only() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let ctx = egui::Context::default();
        let saved = Profile { name: "Saved".into(), host: "192.0.2.1".into(), ..Default::default() };
        app.store.put(saved.clone()).unwrap();
        let serial = Profile { name: "Console".into(), kind: "serial".into(), device: "COM9".into(), ..saved.clone() };
        let adhoc = Profile { name: "192.0.2.9".into(), host: "192.0.2.9".into(), ..saved.clone() };
        for profile in [saved, serial, adhoc] {
            app.tabs.push(Tab {
                session: Session::new(profile, app.settings.colors(), || {}),
                view: TerminalView::default(),
                color: None,
                color_override: false,
                bell: false,
                waiting: None,
                prompt: None,
            });
        }
        for tab in &app.tabs {
            tab.session.shared.emulator.lock().feed(b"banner\r\nSW1(config-if)#");
        }
        app.poll_remote_events(&ctx);
        let labels: Vec<_> = app.tabs.iter().map(|tab| app.tab_label(tab)).collect();
        assert_eq!(labels, ["Saved", "SW1", "SW1"]);
        assert_eq!(app.tabs[0].prompt.as_ref().unwrap().mode, Some(CliMode::Config));
        // Typing after the prompt keeps the last one seen.
        app.tabs[1].session.shared.emulator.lock().feed(b"end");
        app.poll_remote_events(&ctx);
        assert_eq!(app.tabs[1].prompt.as_ref().unwrap().host, "SW1");
    }

    #[test]
    fn saved_tabs_and_their_split_come_back_disconnected() {
        let dir = tempfile::tempdir().unwrap();
        let paths = || Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths(), Vec::new);
        let ctx = egui::Context::default();
        let profile = |name: &str| Profile { name: name.into(), host: "192.0.2.1".into(), ..Default::default() };
        for name in ["A", "B"] {
            app.store.put(profile(name)).unwrap();
        }
        app.store.put(Profile { kind: "sftp".into(), ..profile("Files") }).unwrap();
        for p in [profile("A"), profile("Ad hoc"), profile("B")] {
            app.tabs.push(Tab {
                session: Session::new(p, app.settings.colors(), || {}),
                view: TerminalView::default(),
                color: None,
                color_override: false,
                bell: false,
                waiting: None,
                prompt: None,
            });
        }
        let (_, connection) = tokio::sync::oneshot::channel();
        let files = app.store.get("Files").cloned().unwrap();
        let mut explorer = ExplorerTab::new(99, files, connection, None, app.explorer_context(&ctx));
        explorer.slot = 3;
        app.explorers.push(explorer);
        let a = WorkspaceTabId::Session(app.tabs[0].session.id);
        app.split = Some(TabLayout::pair(a, WorkspaceTabId::Explorer(99)));
        app.active = Some(WorkspaceTabId::Explorer(99));

        // Off: nothing is kept.
        app.remember_tabs();
        assert_eq!(app.settings.last_tabs, LastTabs::default());
        app.settings.restore_tabs = true;
        app.remember_tabs();
        let names: Vec<_> = app.settings.last_tabs.tabs.iter().map(|t| (t.profile.as_str(), t.explorer)).collect();
        assert_eq!(names, [("A", false), ("B", false), ("Files", true)]);
        assert_eq!(app.settings.last_tabs.split, [[0], [2]]);
        assert_eq!(app.settings.last_tabs.active, Some(2));
        app.shutdown();

        let mut next = SnekkieApp::with_port_lister(paths(), Vec::new);
        next.store.remove("B").unwrap();
        next.restore_tabs(&ctx);
        let titles: Vec<_> = next.tabs.iter().map(|t| t.session.title()).collect();
        assert_eq!(titles, ["A (closed)"]);
        assert_eq!(next.tabs[0].session.state(), State::Closed(None));
        assert_eq!(next.explorers.len(), 1);
        assert_eq!(next.explorers[0].status(), (false, false), "the explorer waits for Reconnect");
        let explorer = WorkspaceTabId::Explorer(next.explorers[0].id);
        assert_eq!(next.workspace_order(), [WorkspaceTabId::Session(next.tabs[0].session.id), explorer]);
        assert_eq!(next.split.as_ref().map(|s| s.tabs().len()), Some(2));
        assert_eq!(next.active, Some(explorer));
    }

    #[test]
    fn button_bar_opens_the_review_page_without_sending() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let snippet =
            Snippet { name: "Version".into(), template: "show {{what}}".into(), pinned: true, ..Default::default() };
        let id = app.snippet_store.put(snippet).unwrap();
        let window = SnippetWindow::review(None, app.snippet_store.get(id).unwrap());
        assert!(window.target.is_none(), "no connected tab, so nothing can be sent");
        assert!(window.reviewing(), "it opens on the review page");
    }

    #[test]
    fn reviewed_snippet_target_survives_focus_changes_but_not_reconnect_or_close() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let ctx = egui::Context::default();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        app.open_session(
            &ctx,
            Profile {
                name: "Original".into(),
                kind: "raw".into(),
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                ..Default::default()
            },
        );
        let (mut first, _) = listener.accept().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !app.tabs[0].session.is_connected() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        app.run_command(&ctx, Command::Snippets);
        let target = app.snippets.as_ref().unwrap().target.clone().unwrap();
        app.snippets = None;
        app.tabs.push(Tab {
            session: Session::new(Profile::default(), app.settings.colors(), || {}),
            view: TerminalView::default(),
            color: None,
            color_override: false,
            bell: false,
            waiting: None,
            prompt: None,
        });
        app.active = Some(WorkspaceTabId::Session(app.tabs[1].session.id));
        for (commands, expected) in [("show a\r\nshow b\nshow c\r", "show a\rshow b\rshow c\r"), ("show d", "show d\r")]
        {
            app.send_snippet(&ctx, &target, commands).unwrap();
            while app.paste_queue.is_some() {
                assert!(Instant::now() < deadline);
                app.advance_paste(&ctx);
                std::thread::sleep(Duration::from_millis(10));
            }
            first.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut bytes = vec![0; expected.len()];
            first.read_exact(&mut bytes).unwrap();
            assert_eq!(bytes, expected.as_bytes());
            assert_eq!(app.active_index(), Some(0));
        }
        app.start_paste(&ctx, target.clone(), "first\nnever replay\n", 5000, false).unwrap();
        app.advance_paste(&ctx);
        let mut first_line = [0; 6];
        first.read_exact(&mut first_line).unwrap();
        assert_eq!(&first_line, b"first\r");
        app.run_command(&ctx, Command::Reconnect);
        assert!(app.paste_queue.is_none());
        let (mut replacement, _) = listener.accept().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !app.tabs[0].session.is_connected() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.send_snippet(&ctx, &target, "stale command").is_err());
        replacement.set_read_timeout(Some(Duration::from_millis(150))).unwrap();
        assert!(replacement.read(&mut [0; 64]).is_err());
        let new_target = PasteTarget { generation: app.tabs[0].session.connection_generation(), ..target.clone() };
        app.start_paste(&ctx, new_target, "must not send\nsecond\n", 5000, false).unwrap();
        assert!(app.paste_queue.is_some());
        app.close_tab(target.id, true);
        assert!(app.paste_queue.is_none());
        assert_eq!(replacement.read(&mut [0; 64]).unwrap_or(0), 0);
        assert!(app.send_snippet(&ctx, &target, "closed command").is_err());
    }

    #[test]
    fn bracketed_paste_bells_titles_clipboard_and_host_links() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let ctx = egui::Context::default();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        app.open_session(
            &ctx,
            Profile { name: "Lab".into(), kind: "raw".into(), host: "127.0.0.1".into(), port, ..Default::default() },
        );
        let (mut remote, _) = listener.accept().unwrap();
        remote.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !app.tabs[0].session.is_connected() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let target = PasteTarget {
            id: app.tabs[0].session.id,
            generation: app.tabs[0].session.connection_generation(),
            description: "Lab".into(),
        };
        app.tabs[0].session.shared.emulator.lock().feed(b"\x1b[?2004h");
        // One wrapped block, not paced lines, and an embedded end marker is gone.
        app.start_paste(&ctx, target.clone(), "a\n\x1b[201~b\n", 5000, true).unwrap();
        assert!(app.paste_queue.is_none());
        let expected = b"\x1b[200~a\rb\r\x1b[201~";
        let mut bytes = vec![0; expected.len()];
        remote.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, expected);

        // Bell on a background tab marks it until the tab is shown.
        app.tabs[0].session.shared.emulator.lock().feed(b"\x07\x1b]2;core-sw1\x07\x1b]52;c;aGk=\x07");
        app.active = None;
        app.poll_remote_events(&ctx);
        assert!(app.tabs[0].bell);
        app.active = Some(WorkspaceTabId::Session(target.id));
        app.poll_remote_events(&ctx);
        assert!(!app.tabs[0].bell);

        // Remote title only when chosen; the clipboard request was dropped
        // because OSC 52 is off by default.
        assert_eq!(app.tab_label(&app.tabs[0]), "Lab");
        app.settings.remote_titles = true;
        assert_eq!(app.tab_label(&app.tabs[0]), "core-sw1");
        assert_eq!(app.tabs[0].session.shared.emulator.lock().take_clipboard(), None);

        app.new_session_to(&ctx, "2001:db8::1".into());
        assert_eq!(app.sidebar.draft.host, "2001:db8::1");
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn reconnect_when_back_and_port_checks() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let ctx = egui::Context::default();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let profile = Profile {
            name: "Device".into(),
            kind: "raw".into(),
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        app.open_session(&ctx, profile.clone());
        drop(listener.accept().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(app.tabs[0].session.state(), State::Closed(_)) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let id = app.tabs[0].session.id;

        // Offline mode: only local addresses can be waited for without asking each time.
        app.settings.offline_mode = true;
        app.tabs[0].session.profile.host = "192.0.2.1".into();
        app.wait_for_host(&ctx, id);
        assert!(app.tabs[0].waiting.is_none());
        assert!(app.dialog_texts().iter().any(|t| t.contains("Offline mode is on")), "{:?}", app.dialog_texts());
        app.dialogs.clear();
        app.tabs[0].session.profile.host = "127.0.0.1".into();

        // The port already accepts, so the first check succeeds and it reconnects.
        app.wait_for_host(&ctx, id);
        assert!(app.tabs[0].waiting.is_some());
        while app.tabs[0].waiting.is_some() {
            assert!(Instant::now() < deadline);
            app.poll_waiting(&ctx);
            std::thread::sleep(Duration::from_millis(10));
        }
        let _second = listener.accept().unwrap();
        while !app.tabs[0].session.is_connected() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }

        // Stopping drops the wait, which ends the background checks.
        app.wait_for_host(&ctx, id);
        app.session_tab_action(&ctx, id, TabAction::StopWaiting);
        assert!(app.tabs[0].waiting.is_none());

        app.check_port(&ctx, &profile);
        while app.port_checks.lock().is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let result = app.port_checks.lock()[0].clone();
        assert!(result.starts_with(&format!("127.0.0.1:{} is open (", profile.port)), "{result}");
    }

    #[test]
    fn login_prompts_become_a_dialog_that_answers_or_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let ctx = egui::Context::default();
        let ask = app.prompt_asker(&ctx);
        for accept in [true, false] {
            let (reply, mut answer) = tokio::sync::oneshot::channel();
            let prompts = vec![("Verification code: ".to_string(), false)];
            ask(PromptQuestion { host: "r1".into(), name: String::new(), instructions: String::new(), prompts, reply });
            app.poll_background(&ctx);
            assert_eq!(app.dialog_texts(), ["Verification code:"]);
            let Some(Dialog::Prompts(_, answers, _)) = app.dialogs.last_mut() else { panic!("no prompt dialog") };
            answers[0] = "123456".into();
            let dialog = app.dialogs.pop().unwrap();
            app.finish_dialog(&ctx, dialog, if accept { DialogResult::Yes } else { DialogResult::No });
            let expected = accept.then(|| vec!["123456".to_string()]);
            assert_eq!(answer.try_recv().unwrap(), expected);
        }
    }

    #[test]
    fn log_folder_resolves_the_parent_and_reports_missing_locations() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(log_folder(&dir.path().join("device.log")).unwrap(), dir.path().canonicalize().unwrap());
        assert!(log_folder(&dir.path().join("missing").join("device.log")).is_err());
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(log_folder(&file.join("device.log")).is_err());
        assert!(same_file_path(&file, &dir.path().join(".").join("file")));
    }

    #[test]
    fn text_export_writes_utf8_and_reports_errors_but_cancel_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let ctx = egui::Context::default();
        app.finish_text_export(&ctx, None, TextExport::Save("cancelled".into()));
        app.finish_text_export(&ctx, None, TextExport::Append("cancelled".into()));
        assert!(app.flash.is_none() && app.dialogs.is_empty());
        let path = dir.path().join("output.txt");
        let text = "show run\ninterface GigabitEthernet0/1\n description café 界\n";
        app.finish_text_export(&ctx, Some(path.clone()), TextExport::Save(text.into()));
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
        assert!(app.flash.as_ref().unwrap().0.starts_with("Saved terminal text to "));
        app.flash = None;
        app.finish_text_export(&ctx, Some(dir.path().to_path_buf()), TextExport::Save(text.into()));
        assert!(app.flash.is_none());
        assert!(app.dialog_texts()[0].starts_with("Could not save "));
        // Terminal text never replaces an active store, the game's included.
        let progress = dir.path().join("invade.json");
        std::fs::write(&progress, "{\"version\":1}").unwrap();
        for export in [TextExport::Save(text.into()), TextExport::Append(text.into())] {
            app.dialogs.clear();
            app.finish_text_export(&ctx, Some(progress.clone()), export);
            assert!(app.dialog_texts()[0].starts_with("Choose a different file"));
        }
        assert_eq!(std::fs::read_to_string(&progress).unwrap(), "{\"version\":1}");
    }

    #[test]
    fn text_append_preserves_the_file_and_separates_successive_captures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.txt");
        for (old, expected) in [
            ("", "café 界"),
            ("first", "first\ncafé 界"),
            ("first\n", "first\ncafé 界"),
            ("first\r\n", "first\r\ncafé 界"),
        ] {
            std::fs::write(&path, old).unwrap();
            append_terminal_text(&path, "café 界").unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
            append_terminal_text(&path, "next capture").unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{expected}\nnext capture"));
            append_terminal_text(&path, "").unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{expected}\nnext capture"));
        }
        let missing = dir.path().join("missing.txt");
        assert!(append_terminal_text(&missing, "text").is_err());
        assert!(!missing.exists());
        assert!(append_terminal_text(dir.path(), "text").is_err());
    }

    /// One frame of the whole app at `now`, as the window would draw it.
    fn draw(app: &mut SnekkieApp, ctx: &egui::Context, now: f64) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 640.0))),
            time: Some(now),
            predicted_dt: 0.0,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| app.frame(ui));
        out.textures_delta.clear(); // there's no GPU to hand the font atlas to
    }

    /// Frames until `seconds` later, long enough for any animation to settle.
    fn settle(app: &mut SnekkieApp, ctx: &egui::Context, now: &mut f64, seconds: f64) {
        let until = *now + seconds;
        while *now < until {
            *now += 0.05;
            draw(app, ctx, *now);
        }
    }

    const ANIMATIONS_ON: &str = r#"{"animations": {"enabled": true}}"#;

    /// An app started with `settings` as the text of settings.json, and one
    /// tab. The session is on a serial port that isn't there, so it never
    /// sends anything.
    fn console_app(dir: &tempfile::TempDir, ctx: &egui::Context, settings: &str) -> SnekkieApp {
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        std::fs::write(&paths.settings, settings).unwrap();
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let profile = Profile {
            name: "console".into(),
            kind: "serial".into(),
            device: "no-such-port".into(),
            ..Default::default()
        };
        app.open_session(ctx, profile);
        app
    }

    /// An app with animations on and one tab that has been scrolled back
    /// through a long history, everything drawn and still.
    fn scrolled_back(dir: &tempfile::TempDir, ctx: &egui::Context) -> (SnekkieApp, f64) {
        let mut app = console_app(dir, ctx, ANIMATIONS_ON);

        let mut now = 1.0;
        settle(&mut app, ctx, &mut now, 0.2);
        let output: String = (0..200).map(|i| format!("line {i}\r\n")).collect();
        app.tabs[0].session.shared.emulator.lock().feed(output.as_bytes());
        settle(&mut app, ctx, &mut now, 2.0);
        app.tabs[0].session.shared.emulator.lock().scroll_by(15);
        settle(&mut app, ctx, &mut now, 2.0);

        let tab = &app.tabs[0];
        assert_eq!(tab.session.shared.emulator.lock().display_offset(), 15);
        assert!(tab.view.scroll_offset(now).abs() < 1e-3, "still sliding before the test starts");
        (app, now)
    }

    /// Whatever wipes the screen, the blank one that replaces it stays where
    /// it is: the animator mustn't take the view jumping back from the
    /// scrollback for the text scrolling.
    #[test]
    fn a_screen_wiped_while_scrolled_back_does_not_slide() {
        for command in [Command::Reconnect, Command::ClearScreen, Command::ResetTerminal] {
            let dir = tempfile::tempdir().unwrap();
            let ctx = egui::Context::default();
            let (mut app, mut now) = scrolled_back(&dir, &ctx);
            app.run_command(&ctx, command);
            now += 0.1;
            draw(&mut app, &ctx, now);
            let slide = app.tabs[0].view.scroll_offset(now);
            assert!(slide.abs() < 1e-3, "{command:?} left the screen {slide} rows off");
        }
    }

    /// Whether a screenful of output slides into place in the one tab of an
    /// app started with `settings` as the text of settings.json.
    fn output_slides(settings: &str) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut app = console_app(&dir, &ctx, settings);
        let mut now = 1.0;
        settle(&mut app, &ctx, &mut now, 0.2);
        let output: String = (0..60).map(|i| format!("line {i}\r\n")).collect();
        // More than one go: the system fonts turn up from another thread at
        // some point, which resizes the terminal and so starts its animations
        // afresh, and output arriving in that very frame doesn't slide.
        (0..20).any(|_| {
            app.tabs[0].session.shared.emulator.lock().feed(output.as_bytes());
            now += 0.3;
            draw(&mut app, &ctx, now);
            app.tabs[0].view.scroll_offset(now) > 0.0
        })
    }

    /// Animations ticked in settings.json reach each tab's view: output
    /// slides into place with them on, and never does with them off.
    #[test]
    fn a_tab_animates_only_when_the_saved_settings_have_animations_on() {
        assert!(!output_slides("{}"));
        assert!(output_slides(ANIMATIONS_ON));
    }
}
