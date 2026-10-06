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

use super::fonts::{self, Fonts};
use super::paste::{Action as PasteAction, PastePreview};
use super::preferences::{self, Preferences};
use super::profile_transfer::ProfileTransfer;
use super::putty_import::PuttyImport;
use super::sidebar::{self, Sidebar};
use super::snippets::{Action as SnippetAction, SnippetTarget, SnippetWindow};
use super::style;
use super::terminal_view::{TerminalView, TextExport, ViewOptions};
use super::updater::{self, Updater};
use crate::network::{NetworkAccess, NetworkQuestion};
use crate::paste::{PasteQueue, PasteTarget};
use crate::profiles::{Auth, Kind, Profile, ProfileStore};
use crate::session::{ConnectContext, LogStatus, NoticeLevel, Session, State};
use crate::settings::{AppSettings, SettingsStore, Theme};
use crate::snippets::{Snippet, SnippetStore};
use crate::terminal::keys;
use crate::transport::serial::PortInfo;
use crate::transport::ssh::{self, HostKeyAsker, HostKeyQuestion};
use crate::update::{self, Release};

/// How long a status bar message stays up, in seconds.
const FLASH_SECONDS: f64 = 4.0;

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
Shift+PgUp/Dn   Scroll back through history
Ctrl+Shift+L    Clear screen and scrollback
Ctrl+Shift+B    Send break (serial, telnet)

Ctrl+B          Toggle sidebar
Ctrl+,          Preferences";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    NewTab,
    Reconnect(u64),
}

#[derive(Debug, Clone)]
struct PendingConnect {
    profile: Profile,
    target: Target,
}

enum ConfirmAction {
    CloseTab(u64),
    Quit,
    DeleteSaved(String),
    DeleteSnippet(u64),
    ReplaceTheme(String),
    DeleteTheme(String),
    InstallUpdate,
}

enum InputAction {
    Password(Box<PendingConnect>),
    Passphrase(Box<PendingConnect>),
    SaveSession(Box<Profile>),
    OtherDevice,
    OtherBaud,
    SaveTheme,
}

enum Dialog {
    Message { title: String, text: String, monospace: bool },
    Confirm { title: String, text: String, action: ConfirmAction },
    Input { title: String, label: String, text: String, secret: bool, action: InputAction, focus: bool },
    HostKey(Box<HostKeyQuestion>),
    Network(Box<NetworkQuestion>),
    Update { release: Release, installable: bool },
    Import(Box<PuttyImport>),
    ProfileTransfer(Box<ProfileTransfer>),
    TabColor { id: u64, color: Color32 },
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
    ToggleAutoPaging,
    Preferences,
    ImportPutty,
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

struct Tab {
    session: Session,
    view: TerminalView,
    color: Option<Color32>,
    /// Clearing a color is an explicit override, distinct from using the default.
    color_override: bool,
}

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
    paste_preview: Option<PastePreview>,
    paste_queue: Option<PasteQueue>,
    retired_log_workers: Vec<std::thread::JoinHandle<()>>,
    settings_store: SettingsStore,
    settings: AppSettings,
    sidebar: Sidebar,
    tabs: Vec<Tab>,
    active: usize,
    runtime: tokio::runtime::Runtime,
    fonts: Fonts,
    host_keys: Arc<Mutex<VecDeque<HostKeyQuestion>>>,
    network_questions: Arc<Mutex<VecDeque<NetworkQuestion>>>,
    network: NetworkAccess,
    dialogs: Vec<Dialog>,
    preferences: Option<Preferences>,
    flash: Option<(String, f64)>,
    clipboard: Option<arboard::Clipboard>,
    folder_open: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    allow_close: bool,
    accent: Color32,
    styled_theme: Option<Theme>,
    dragging_tab: Option<usize>,
    /// Whether the terminal had the keyboard when a dialog opened, so it
    /// can have it back when the dialog closes.
    terminal_had_focus: bool,
    updater: Updater,
}

impl SnekkieApp {
    pub fn new(paths: Paths) -> Self {
        let mut app = Self::with_port_lister(paths, crate::transport::serial::list_ports);
        // Only the real app goes online by itself; tests build theirs
        // with_port_lister.
        if app.settings.check_for_updates && !app.settings.offline_mode {
            app.updater.check_at_startup();
        }
        app
    }

    /// Styling needs the egui context, so it happens on the first frame
    /// (see `poll_background`), not here.
    pub fn with_port_lister(paths: Paths, list_ports: fn() -> Vec<PortInfo>) -> Self {
        let snippet_store = SnippetStore::open(paths.sessions.with_file_name("snippets.json"));
        let store = ProfileStore::open(paths.sessions);
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
            sidebar: Sidebar::with_port_lister(&settings, list_ports),
            store,
            snippet_store,
            snippets: None,
            paste_preview: None,
            paste_queue: None,
            retired_log_workers: Vec::new(),
            settings_store,
            settings,
            tabs: Vec::new(),
            active: 0,
            runtime,
            fonts: Fonts::new(),
            host_keys: Arc::new(Mutex::new(VecDeque::new())),
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
            dragging_tab: None,
            terminal_had_focus: false,
            updater: Updater::new(),
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

    fn current(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.session.id == id)
    }

    fn clipboard_text(&mut self) -> Option<String> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()?.get_text().ok()
    }

    fn finish_text_export(&mut self, ctx: &egui::Context, path: Option<PathBuf>, export: TextExport) {
        let Some(path) = path else { return };
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

    fn live_sessions(&self) -> usize {
        self.tabs.iter().filter(|t| t.session.is_live()).count()
    }

    /// Close the window without asking, hanging up every session.
    fn quit_now(&mut self, ctx: &egui::Context) {
        self.allow_close = true;
        for tab in &self.tabs {
            tab.session.shutdown();
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
        if live == 0 {
            self.install_update(ctx);
        } else {
            self.confirm(
                "Restart to update",
                format!(
                    "Snekkie will close to install the update, then start again. \
                     This disconnects {live} session(s). Restart now?"
                ),
                ConfirmAction::InstallUpdate,
            );
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
                    if self.live_sessions() == 0 && !self.modal_open() {
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
        if profile.kind() == Kind::Ssh {
            match profile.auth() {
                Auth::Password => {
                    let user = if profile.username.is_empty() { "(user)" } else { &profile.username };
                    let label = format!("Password for {user}@{}:", profile.host);
                    self.input(
                        "SSH password",
                        label,
                        "",
                        true,
                        InputAction::Password(Box::new(PendingConnect { profile, target })),
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
                            InputAction::Passphrase(Box::new(PendingConnect { profile, target })),
                        );
                        return;
                    }
                }
                _ => {}
            }
        }
        self.start_connect(ctx, PendingConnect { profile, target }, String::new(), String::new());
    }

    fn start_connect(
        &mut self,
        ctx: &egui::Context,
        pending: PendingConnect,
        password: String,
        key_passphrase: String,
    ) {
        if let Target::Reconnect(id) = pending.target
            && self.paste_queue.as_ref().is_some_and(|q| q.target.id == id)
        {
            self.paste_queue = None;
            self.flash(ctx, "Paste canceled by reconnect. Remaining lines were discarded.");
        }
        let cx = ConnectContext {
            runtime: self.runtime.handle(),
            ask_host_key: self.host_key_asker(ctx),
            password,
            key_passphrase,
            network: {
                let queue = self.network_questions.clone();
                let ctx = ctx.clone();
                self.network.with_asker(Arc::new(move |question| {
                    queue.lock().push_back(question);
                    ctx.request_repaint();
                }))
            },
        };
        match pending.target {
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
                self.tabs.push(Tab { session, view, color, color_override: false });
                self.active = self.tabs.len() - 1;
            }
            Target::Reconnect(id) => {
                let Some(index) = self.tab_index(id) else { return };
                let tab = &mut self.tabs[index];
                tab.session.shutdown();
                tab.session.shared.emulator.lock().reset();
                // The view's animator and highlight cache still describe the old screen.
                tab.view.clear_highlight_cache();
                tab.view.reset_animations();
                tab.session.connect(cx);
                tab.view.focus();
                self.active = index;
            }
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
        let tab = self.tabs.remove(index);
        if self.paste_queue.as_ref().is_some_and(|q| q.target.id == id) {
            self.paste_queue = None;
        }
        tab.session.shutdown();
        self.retired_log_workers.extend(tab.session.take_log_workers());
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if index < self.active {
            self.active -= 1;
        }
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.view.focus();
        }
    }

    // -- commands -------------------------------------------------------

    fn run_command(&mut self, ctx: &egui::Context, command: Command) {
        if self.settings.offline_mode
            && matches!(command, Command::CheckForUpdates | Command::ShowUpdate | Command::InstallUpdate)
        {
            self.message("Offline mode", "Online updates are disabled. Turn off Offline mode under Settings → Preferences → General to check or download updates.");
            return;
        }
        match command {
            Command::NewSession => {
                self.settings.show_sidebar = true;
                self.sidebar.reset(&self.settings);
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
                if let Some(id) = self.current().map(|t| t.session.id) {
                    self.close_tab(id, false);
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
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    // Selected text on one line is probably what to look for.
                    let selected = tab.session.shared.emulator.lock().selection_text();
                    tab.view.open_search(selected.filter(|t| !t.contains('\n') && t.chars().count() <= 200));
                }
            }
            Command::Snippets => {
                let target = self.current().filter(|t| t.session.is_connected()).map(|t| SnippetTarget {
                    id: t.session.id,
                    generation: t.session.connection_generation(),
                    description: format!("{} — {}", t.session.title(), t.session.description()),
                });
                self.snippets = Some(SnippetWindow::new(target));
            }
            Command::ClearScreen => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.session.shared.emulator.lock().clear();
                    tab.view.reset_animations();
                }
            }
            Command::ResetTerminal => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
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
                self.settings.show_sidebar = !self.settings.show_sidebar;
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
            Command::About => self.message(
                "About Snekkie",
                format!(
                    "Snekkie {}\nA tabbed SSH and serial terminal.\n\nFree software under the GNU GPL v3 or later.",
                    env!("CARGO_PKG_VERSION")
                ),
            ),
            Command::NextTab | Command::PreviousTab => {
                if !self.tabs.is_empty() {
                    let n = self.tabs.len();
                    self.active =
                        if command == Command::NextTab { (self.active + 1) % n } else { (self.active + n - 1) % n };
                    self.tabs[self.active].view.focus();
                }
            }
        }
    }

    /// Keyboard shortcuts, taken out of the event queue before the
    /// terminal sees them.
    fn shortcuts(&mut self, ctx: &egui::Context) -> Vec<Command> {
        const CTRL: Modifiers = Modifiers::CTRL;
        const CTRL_SHIFT: Modifiers = Modifiers { ctrl: true, shift: true, ..Modifiers::NONE };
        let table: [(Modifiers, Key, Command); 16] = [
            (CTRL_SHIFT, Key::N, Command::NewSession),
            (CTRL_SHIFT, Key::D, Command::Duplicate),
            (CTRL_SHIFT, Key::R, Command::Reconnect),
            (CTRL, Key::W, Command::CloseTab),
            (CTRL, Key::Q, Command::Quit),
            (CTRL_SHIFT, Key::A, Command::SelectAll),
            (CTRL, Key::F, Command::Find),
            (CTRL_SHIFT, Key::S, Command::Snippets),
            (CTRL_SHIFT, Key::L, Command::ClearScreen),
            (CTRL_SHIFT, Key::B, Command::SendBreak),
            (CTRL, Key::B, Command::ToggleSidebar),
            (CTRL, Key::Comma, Command::Preferences),
            (CTRL, Key::Tab, Command::NextTab),
            (CTRL_SHIFT, Key::Tab, Command::PreviousTab),
            (CTRL, Key::PageDown, Command::NextTab),
            (CTRL, Key::PageUp, Command::PreviousTab),
        ];
        ctx.input_mut(|input| {
            let mut commands = Vec::new();
            input.events.retain(|event| {
                let Event::Key { key, pressed: true, modifiers, .. } = event else { return true };
                let exact =
                    |m: Modifiers| m.ctrl == modifiers.ctrl && m.shift == modifiers.shift && m.alt == modifiers.alt;
                match table.iter().find(|(m, k, _)| k == key && exact(*m)) {
                    Some((_, _, command)) => {
                        commands.push(*command);
                        false
                    }
                    None => true,
                }
            });
            commands
        })
    }

    // -- per-frame plumbing ---------------------------------------------

    fn poll_background(&mut self, ctx: &egui::Context) {
        self.fonts.poll(ctx);
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
        let questions: Vec<HostKeyQuestion> = self.host_keys.lock().drain(..).collect();
        self.dialogs.extend(questions.into_iter().map(|q| Dialog::HostKey(Box::new(q))));
        // A host key question whose connection gave up (tab closed, timed
        // out) has nobody left to answer.
        self.dialogs.retain(|d| !matches!(d, Dialog::HostKey(q) if q.reply.is_closed()));
        let network_questions: Vec<_> = self.network_questions.lock().drain(..).collect();
        self.dialogs.extend(network_questions.into_iter().map(|q| Dialog::Network(Box::new(q))));
        self.dialogs.retain(|d| !matches!(d, Dialog::Network(q) if q.reply.is_closed()));

        let mut notices = Vec::new();
        for tab in &self.tabs {
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
                    button = button.shortcut_text(shortcut);
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
            if let Some(queue) = &self.paste_queue {
                ui.label(format!("Paste: {}/{} line(s) — {}", queue.submitted, queue.total, queue.target.description))
                    .on_hover_text("Progress counts lines submitted to the transport, not device acknowledgments.");
                if ui.button("Stop paste").clicked() {
                    self.paste_queue = None;
                    self.flash(ui.ctx(), "Paste stopped. Already submitted lines cannot be recalled.");
                }
                ui.separator();
            }
            let text = self.current().map_or_else(|| "No session".to_string(), |t| t.session.status_text());
            if self.settings.offline_mode {
                ui.label(RichText::new("Offline mode").strong())
                    .on_hover_text("Online updates are disabled; DNS and non-local connections require approval.");
                ui.separator();
            }
            ui.label(RichText::new(text).color(style::secondary(ui)));
            if let Some(tab) = self.current() {
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
                    self.input("Save session", "Name:", suggested, false, InputAction::SaveSession(Box::new(profile)));
                }
                sidebar::Action::Delete(name) => {
                    self.confirm("Delete session", format!("Delete “{name}”?"), ConfirmAction::DeleteSaved(name));
                }
                sidebar::Action::ToggleFavorite(name) => {
                    if let Some(mut profile) = self.store.get(&name).cloned() {
                        profile.favorite = !profile.favorite;
                        self.save_profile_metadata(profile);
                    }
                }
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
        let policy_path = self.settings_store.path.with_file_name("network.ini");
        if [
            self.store.path.as_path(),
            self.settings_store.path.as_path(),
            policy_path.as_path(),
            self.snippet_store.path.as_path(),
        ]
        .into_iter()
        .any(|store| same_file_path(&path, store))
        {
            self.message("Export profiles", "Choose a different file. Exporting over Snekkie's active profiles, preferences or snippets would replace saved data.");
            return;
        }
        let result = crate::profiles::export_profiles(profiles)
            .and_then(|text| crate::config::write_atomic(&path, &text).map_err(|e| e.to_string()));
        match result {
            Ok(()) => self.flash(ctx, format!("Exported {} profile(s) to {}", profiles.len(), path.display())),
            Err(error) => self.message("Export profiles", format!("Could not export {}: {error}", path.display())),
        }
    }

    fn tab_strip(&mut self, ui: &mut Ui) -> Option<(u64, TabAction)> {
        let mut action = None;
        let mut rects = Vec::with_capacity(self.tabs.len());
        egui::ScrollArea::horizontal().id_salt("tab_strip").auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (index, tab) in self.tabs.iter().enumerate() {
                    let selected = index == self.active;
                    let state = tab.session.state();
                    let color = if selected { style::primary(ui) } else { style::secondary(ui) };
                    let dot = match state {
                        State::Connected => self.accent,
                        State::Connecting => Color32::from_rgb(0xd0, 0xa0, 0x30),
                        State::Closed(_) => Color32::from_rgb(0xa0, 0x40, 0x40),
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
                                        let (dot_rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                                        ui.painter().circle_filled(dot_rect.center(), 3.5, dot);
                                        ui.add(
                                            egui::Label::new(RichText::new(tab.session.title()).color(color))
                                                .selectable(false),
                                        );
                                        let close = ui.add(
                                            egui::Button::new(RichText::new("×").color(style::secondary(ui)))
                                                .frame(false)
                                                .small(),
                                        );
                                        if close.on_hover_text("Close tab").clicked() {
                                            action = Some((id, TabAction::Close));
                                        }
                                    });
                                });
                        },
                    );
                    let response = inner.response;
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
                            Stroke::new(2.0, tab.color.unwrap_or(self.accent)),
                        );
                    } else if response.hovered() {
                        ui.painter().rect_filled(rect, 3.0, Color32::from_white_alpha(8));
                    }
                    if response.clicked() {
                        action = Some((id, TabAction::Select));
                    }
                    if response.middle_clicked() {
                        action = Some((id, TabAction::Close));
                    }
                    if response.drag_started() {
                        // Like a browser: the tab you pick up is the one shown.
                        self.dragging_tab = Some(index);
                        action = Some((id, TabAction::Select));
                    }
                    response.context_menu(|ui| {
                        if ui.button("Reconnect").clicked() {
                            action = Some((id, TabAction::Reconnect));
                        }
                        if ui.button("Duplicate").clicked() {
                            action = Some((id, TabAction::Duplicate));
                        }
                        ui.menu_button("Tab color", |ui| {
                            for (name, color) in TAB_COLORS {
                                if ui.add(egui::Button::new(name).fill(color.gamma_multiply(0.15))).clicked() {
                                    action = Some((id, TabAction::Color(Some(color))));
                                    ui.close();
                                }
                            }
                            ui.separator();
                            if ui.button("Custom color…").clicked() {
                                action = Some((id, TabAction::CustomColor));
                                ui.close();
                            }
                            if ui.button("Clear tab color").clicked() {
                                action = Some((id, TabAction::Color(None)));
                                ui.close();
                            }
                            if ui.button("Use profile color").clicked() {
                                action = Some((id, TabAction::UseProfileColor));
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
                                action = Some((id, TabAction::SaveProfileColor));
                                ui.close();
                            }
                        });
                        ui.separator();
                        if ui.button("Close").clicked() {
                            action = Some((id, TabAction::Close));
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
            } else if let Some(pos) = pointer
                && let Some(to) = rects.iter().position(|r| r.x_range().contains(pos.x))
                && to != from
                && from < self.tabs.len()
            {
                let tab = self.tabs.remove(from);
                self.tabs.insert(to, tab);
                if self.active == from {
                    self.active = to;
                } else if from < self.active && to >= self.active {
                    self.active -= 1;
                } else if from > self.active && to <= self.active {
                    self.active += 1;
                }
                self.dragging_tab = Some(to);
            }
        }
        action
    }

    fn central(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if self.tabs.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "No session open.\nFill in the form on the left and click Connect,\nor double-click a saved session.",
                        )
                        .color(style::secondary(ui)),
                    )
                    .selectable(false),
                );
            });
            return;
        }

        if let Some((id, action)) = self.tab_strip(ui) {
            match action {
                TabAction::Select => {
                    if let Some(index) = self.tab_index(id) {
                        self.active = index;
                        self.tabs[index].view.focus();
                    }
                }
                TabAction::Close => self.close_tab(id, false),
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
                        self.dialogs
                            .push(Dialog::TabColor { id, color: self.tabs[index].color.unwrap_or(self.accent) });
                    }
                }
                TabAction::Reconnect | TabAction::Duplicate => {
                    if let Some(index) = self.tab_index(id) {
                        let profile = self.tabs[index].session.profile.clone();
                        let target =
                            if action == TabAction::Reconnect { Target::Reconnect(id) } else { Target::NewTab };
                        self.request_connect(&ctx, profile, target);
                    }
                }
            }
        }
        if self.tabs.is_empty() {
            return;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        ui.add_space(2.0);

        // Banner: why the session ended, or that it's still connecting.
        let (state, id, description) = {
            let tab = &self.tabs[self.active];
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
                let profile = self.tabs[self.active].session.profile.clone();
                self.request_connect(&ctx, profile, Target::Reconnect(id));
            }
        }

        let theme = self.settings.colors();
        let tab = &mut self.tabs[self.active];
        let family = self.fonts.family(&ctx, &tab.session.profile.font_family);
        let (regular, bold) =
            Fonts::font_ids(family.as_deref(), fonts::points_to_pixels(tab.session.profile.font_size));
        let syntax = tab.session.profile.device_syntax.clone();
        let sending = self.paste_queue.as_ref().is_some_and(|q| q.target.id == tab.session.id);
        let mouse_input = self.dialogs.is_empty()
            && self.preferences.is_none()
            && self.snippets.is_none()
            && self.paste_preview.is_none()
            && !sending;
        let keyboard = mouse_input && (!self.settings.show_sidebar || !self.sidebar.blocks_terminal_input());
        let animations = self.settings.animations;
        let options = ViewOptions {
            theme,
            syntax: &syntax,
            highlighting_intensity: self.settings.highlighting_intensity,
            regular,
            bold,
            keyboard,
            mouse_input,
            right_click_paste: self.settings.right_click_paste,
            animations,
        };
        let out = if theme.effects.is_super() {
            style::monitor(ui, theme, |ui| tab.view.show(ui, &tab.session, &options))
        } else {
            tab.view.show(ui, &tab.session, &options)
        };
        if let Some(text) = out.copy {
            ctx.copy_text(text);
        }
        if let Some(text) = out.paste_text {
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
                            "The server at {host} presented a host key that is not in known_hosts."
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
                        "This copy of Snekkie wasn't installed with the installer, so it can't update \
                         itself. Download the new version from its release page."
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
        if modal.should_close() && !matches!(self.dialogs.last(), Some(Dialog::HostKey(_))) {
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
                self.store.profiles = crate::putty::merge(&original, selected);
                if let Err(e) = self.store.save() {
                    self.store.profiles = original;
                    self.message("Import PuTTY sessions", format!("Could not save imported sessions: {e}"));
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
            (Dialog::HostKey(question), result) => {
                let _ = question.reply.send(result == DialogResult::Yes);
            }
            (Dialog::Network(question), result) => {
                let _ = question.reply.send(result == DialogResult::Yes);
            }
            (Dialog::Confirm { action, .. }, DialogResult::Yes) => match action {
                ConfirmAction::CloseTab(id) => self.close_tab(id, true),
                ConfirmAction::Quit => self.quit_now(ctx),
                ConfirmAction::InstallUpdate => self.install_update(ctx),
                ConfirmAction::DeleteSaved(name) => {
                    if let Err(e) = self.store.remove(&name) {
                        self.message("Delete session", format!("Could not save sessions: {e}"));
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
        let monospace = self.fonts.monospace.clone();
        let mut outcome = preferences::Outcome::Open;
        let modal = egui::Modal::new(Id::new("preferences")).show(ctx, |ui| {
            ui.set_width(preferences::WIDTH);
            outcome = prefs.ui(ui, &monospace);
        });
        if modal.should_close() && !blocked {
            outcome = preferences::Outcome::Cancel;
        }
        match outcome {
            preferences::Outcome::Open => {}
            preferences::Outcome::Cancel => self.preferences = None,
            preferences::Outcome::Save(settings) => {
                let enabling_offline = settings.offline_mode && !self.settings.offline_mode;
                self.settings = AppSettings { show_sidebar: self.settings.show_sidebar, ..settings };
                self.network.set_offline(self.settings.offline_mode);
                self.updater.set_offline(self.settings.offline_mode);
                if enabling_offline {
                    for tab in &self.tabs {
                        if tab.session.profile.kind() != Kind::Serial {
                            tab.session.shutdown();
                        }
                    }
                    self.dialogs.retain(|d| !matches!(d, Dialog::Update { .. }));
                }
                for tab in &self.tabs {
                    tab.session.set_auto_paging(self.settings.auto_paging);
                }
                self.save_settings();
                self.preferences = None;
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
        } else if let Err(error) = self.start_paste(ctx, target, text, 0) {
            self.message("Paste", error);
        }
    }

    fn start_paste(
        &mut self,
        ctx: &egui::Context,
        target: PasteTarget,
        text: &str,
        delay_ms: u64,
    ) -> Result<(), String> {
        if self.paste_queue.is_some() {
            return Err("A paste is already running. Stop it before starting another.".into());
        }
        if !self.snippet_target_valid(&target) {
            return Err("The reviewed connection changed or closed. Review the commands again.".into());
        }
        let queue = PasteQueue::new(target, text, delay_ms, std::time::Instant::now());
        if queue.total <= 1 || delay_ms == 0 {
            let index = self.tab_index(queue.target.id).ok_or("The destination tab closed.")?;
            if !self.tabs[index].session.write_reviewed(queue.target.generation, keys::encode_paste(text)) {
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
                if let Err(error) = self.start_paste(ctx, preview.target.clone(), &preview.text, preview.delay_ms) {
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
        let index = self.tab_index(target.id).ok_or("The destination tab closed.")?;
        let mut text = text.to_string();
        if !text.ends_with(['\r', '\n']) {
            text.push('\n');
        }
        self.start_paste(ctx, target.clone(), &text, self.settings.paste_delay_ms)?;
        self.active = index;
        self.tabs[index].view.focus();
        Ok(())
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
        let policy_path = self.settings_store.path.with_file_name("network.ini");
        if [
            self.store.path.as_path(),
            self.settings_store.path.as_path(),
            policy_path.as_path(),
            self.snippet_store.path.as_path(),
        ]
        .into_iter()
        .any(|store| same_file_path(&path, store))
        {
            self.message("Export snippets", "Choose a different file. Exporting over Snekkie's active profiles, preferences or snippets would replace saved data.");
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
        self.poll_background(&ctx);
        self.handle_close_request(&ctx);

        let modal_open = self.modal_open();
        if !modal_open {
            let focused = ctx.memory(|m| m.focused());
            self.terminal_had_focus =
                self.current().is_some_and(|t| focused == Some(Id::new(("terminal", t.session.id))));
        }
        let mut commands = if modal_open { Vec::new() } else { self.shortcuts(&ctx) };

        egui::Panel::top("menu")
            .frame(Frame::new().fill(ui.visuals().panel_fill).inner_margin(Margin::symmetric(6, 3)))
            .show(ui, |ui| commands.extend(self.menu_bar(ui)));
        egui::Panel::bottom("status")
            .frame(
                Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(Margin::symmetric(8, 3))
                    .stroke(Stroke::new(1.0, style::border(ui))),
            )
            .show(ui, |ui| self.status_bar(ui));
        if self.settings.show_sidebar {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(320.0)
                .size_range(280.0..=460.0)
                .frame(Frame::new().fill(ui.visuals().window_fill).inner_margin(Margin::same(10)))
                .show(ui, |ui| self.sidebar_ui(ui));
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(ui.visuals().panel_fill).inner_margin(Margin {
                left: 2,
                right: 0,
                top: 2,
                bottom: 0,
            }))
            .show(ui, |ui| self.central(ui));

        for command in commands {
            self.run_command(&ctx, command);
        }

        // Preferences sits under any dialog it opened (theme name, confirm).
        let dialog_open = !self.dialogs.is_empty();
        self.preferences_ui(&ctx, dialog_open);
        self.snippets_ui(&ctx, dialog_open);
        self.paste_ui(&ctx, dialog_open);
        self.dialogs_ui(&ctx);
        // Process Stop, close and reconnect before considering the next line.
        self.advance_paste(&ctx);

        // Back to typing where you left off once the last dialog closes.
        if modal_open
            && !self.modal_open()
            && self.terminal_had_focus
            && let Some(tab) = self.tabs.get_mut(self.active)
        {
            tab.view.focus();
        }
    }

    /// Hang up everything; called on exit.
    pub fn shutdown(&mut self) {
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

    #[doc(hidden)]
    pub fn tab_titles(&self) -> Vec<String> {
        self.tabs.iter().map(|t| t.session.title()).collect()
    }

    #[doc(hidden)]
    pub fn dialog_texts(&self) -> Vec<String> {
        self.dialogs
            .iter()
            .map(|d| match d {
                Dialog::Message { text, .. } | Dialog::Confirm { text, .. } => text.clone(),
                Dialog::Input { label, .. } => label.clone(),
                Dialog::HostKey(q) => q.fingerprint.clone(),
                Dialog::Network(q) => q.text(),
                Dialog::Update { release, .. } => update_summary(release),
                Dialog::Import(_) => "Import PuTTY sessions".into(),
                Dialog::ProfileTransfer(transfer) => {
                    if transfer.is_export() {
                        "Export profiles".into()
                    } else {
                        "Import profiles".into()
                    }
                }
                Dialog::TabColor { .. } => "Session tab color".into(),
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
    Close,
    Reconnect,
    Duplicate,
    Color(Option<Color32>),
    CustomColor,
    UseProfileColor,
    SaveProfileColor,
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
    fn profile_metadata_updates_matching_sessions_and_rolls_back_on_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let mut app = SnekkieApp::with_port_lister(paths, Vec::new);
        let profile = Profile { name: "Saved".into(), host: "192.0.2.1".into(), ..Default::default() };
        app.store.put(profile.clone()).unwrap();
        app.sidebar.load(profile.clone());
        for (host, color_override) in [("192.0.2.1", false), ("192.0.2.1", true), ("192.0.2.2", false)] {
            let session = Session::new(Profile { host: host.into(), ..profile.clone() }, app.settings.colors(), || {});
            app.tabs.push(Tab { session, view: TerminalView::default(), color: None, color_override });
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
        });
        app.active = 1;
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
            assert_eq!(app.active, 0);
        }
        app.start_paste(&ctx, target.clone(), "first\nnever replay\n", 5000).unwrap();
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
        app.start_paste(&ctx, new_target, "must not send\nsecond\n", 5000).unwrap();
        assert!(app.paste_queue.is_some());
        app.close_tab(target.id, true);
        assert!(app.paste_queue.is_none());
        assert_eq!(replacement.read(&mut [0; 64]).unwrap_or(0), 0);
        assert!(app.send_snippet(&ctx, &target, "closed command").is_err());
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
