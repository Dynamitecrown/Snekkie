//! Main window: menus, sidebar, tabs, status bar, and the dialogs between
//! them.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{
    Align, Color32, Event, Frame, Id, Key, Layout, Margin, Modifiers, RichText, Sense, Stroke, TextEdit, Ui,
    ViewportCommand, vec2,
};
use parking_lot::Mutex;

use super::fonts::{self, Fonts};
use super::preferences::{self, Preferences};
use super::sidebar::{self, Sidebar};
use super::style;
use super::terminal_view::{TerminalView, ViewOptions};
use crate::profiles::{Auth, Kind, Profile, ProfileStore};
use crate::session::{ConnectContext, NoticeLevel, Session, State};
use crate::settings::{AppSettings, SettingsStore};
use crate::terminal::keys;
use crate::transport::serial::PortInfo;
use crate::transport::ssh::{self, HostKeyAsker, HostKeyQuestion};

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
Ctrl+Shift+V    Paste     (right-click also pastes)
Shift+PgUp/Dn   Scroll back through history
Ctrl+Shift+L    Clear screen
Ctrl+Shift+B    Send break (serial only)

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
    ReplaceTheme(String),
    DeleteTheme(String),
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
    ClearScreen,
    ResetTerminal,
    SendBreak,
    ToggleSidebar,
    Preferences,
    Shortcuts,
    About,
    NextTab,
    PreviousTab,
}

struct Tab {
    session: Session,
    view: TerminalView,
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
    settings_store: SettingsStore,
    settings: AppSettings,
    sidebar: Sidebar,
    tabs: Vec<Tab>,
    active: usize,
    runtime: tokio::runtime::Runtime,
    fonts: Fonts,
    host_keys: Arc<Mutex<VecDeque<HostKeyQuestion>>>,
    dialogs: Vec<Dialog>,
    preferences: Option<Preferences>,
    flash: Option<(String, f64)>,
    clipboard: Option<arboard::Clipboard>,
    allow_close: bool,
    accent: Color32,
    dragging_tab: Option<usize>,
    /// Whether the terminal had the keyboard when a dialog opened, so it
    /// can have it back when the dialog closes.
    terminal_had_focus: bool,
}

impl SnekkieApp {
    pub fn new(paths: Paths) -> Self {
        Self::with_port_lister(paths, crate::transport::serial::list_ports)
    }

    /// Styling needs the egui context, so it happens on the first frame
    /// (see `poll_background`), not here.
    pub fn with_port_lister(paths: Paths, list_ports: fn() -> Vec<PortInfo>) -> Self {
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
            sidebar: Sidebar::with_port_lister(&settings, list_ports),
            store,
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
            allow_close: false,
            // Transparent never matches a theme, so the first frame applies
            // the style.
            accent: Color32::TRANSPARENT,
            dragging_tab: None,
            terminal_had_focus: false,
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

    fn host_key_asker(&self, ctx: &egui::Context) -> HostKeyAsker {
        let queue = self.host_keys.clone();
        let ctx = ctx.clone();
        Arc::new(move |question| {
            queue.lock().push_back(question);
            ctx.request_repaint();
        })
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
        let cx = ConnectContext {
            runtime: self.runtime.handle(),
            ask_host_key: self.host_key_asker(ctx),
            password,
            key_passphrase,
        };
        match pending.target {
            Target::NewTab => {
                let repaint = {
                    let ctx = ctx.clone();
                    move || ctx.request_repaint()
                };
                let mut session = Session::new(pending.profile, self.settings.colors(), repaint);
                session.connect(cx);
                let mut view = TerminalView::default();
                view.focus();
                self.tabs.push(Tab { session, view });
                self.active = self.tabs.len() - 1;
            }
            Target::Reconnect(id) => {
                let Some(index) = self.tab_index(id) else { return };
                let tab = &mut self.tabs[index];
                tab.session.shutdown();
                tab.session.shared.emulator.lock().reset();
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
        tab.session.shutdown();
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
                if let Some(text) = self.clipboard_text()
                    && let Some(tab) = self.current()
                {
                    tab.session.write(keys::encode_paste(&text));
                }
            }
            Command::SelectAll => {
                if let Some(tab) = self.current() {
                    tab.session.shared.emulator.lock().select_all();
                }
            }
            Command::ClearScreen => {
                if let Some(tab) = self.current() {
                    tab.session.shared.emulator.lock().clear();
                }
            }
            Command::ResetTerminal => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.session.shared.emulator.lock().reset();
                    tab.view.clear_highlight_cache();
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
            Command::Preferences => self.preferences = Some(Preferences::new(&self.settings)),
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
        let table: [(Modifiers, Key, Command); 14] = [
            (CTRL_SHIFT, Key::N, Command::NewSession),
            (CTRL_SHIFT, Key::D, Command::Duplicate),
            (CTRL_SHIFT, Key::R, Command::Reconnect),
            (CTRL, Key::W, Command::CloseTab),
            (CTRL, Key::Q, Command::Quit),
            (CTRL_SHIFT, Key::A, Command::SelectAll),
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
        let questions: Vec<HostKeyQuestion> = self.host_keys.lock().drain(..).collect();
        self.dialogs.extend(questions.into_iter().map(|q| Dialog::HostKey(Box::new(q))));
        // A host key question whose connection gave up (tab closed, timed
        // out) has nobody left to answer.
        self.dialogs.retain(|d| !matches!(d, Dialog::HostKey(q) if q.reply.is_closed()));

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

        let accent = self.settings.colors().cursor;
        if accent != self.accent {
            if self.accent == Color32::TRANSPARENT {
                // Ctrl+minus and friends belong to the far end, not UI zoom.
                ctx.options_mut(|o| o.zoom_with_keyboard = false);
            }
            self.accent = accent;
            style::apply(ctx, accent);
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allow_close {
            return;
        }
        let live = self.tabs.iter().filter(|t| t.session.is_live()).count();
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
            let mut item = |ui: &mut Ui, text: &str, shortcut: &str, command: Command| {
                let mut button = egui::Button::new(text);
                if !shortcut.is_empty() {
                    button = button.shortcut_text(shortcut);
                }
                if ui.add(button).clicked() {
                    commands.push(command);
                }
            };
            ui.menu_button("Session", |ui| {
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
            ui.menu_button("Edit", |ui| {
                item(ui, "Copy", "Ctrl+Shift+C", Command::Copy);
                item(ui, "Paste", "Ctrl+Shift+V", Command::Paste);
                item(ui, "Select all", "Ctrl+Shift+A", Command::SelectAll);
            });
            ui.menu_button("Terminal", |ui| {
                item(ui, "Clear screen", "Ctrl+Shift+L", Command::ClearScreen);
                item(ui, "Reset terminal", "", Command::ResetTerminal);
                ui.separator();
                item(ui, "Send break", "Ctrl+Shift+B", Command::SendBreak);
            });
            ui.menu_button("View", |ui| {
                let label = if self.settings.show_sidebar { "✔ Sidebar" } else { "   Sidebar" };
                item(ui, label, "Ctrl+B", Command::ToggleSidebar);
                ui.separator();
                item(ui, "Next tab", "Ctrl+Tab", Command::NextTab);
                item(ui, "Previous tab", "Ctrl+Shift+Tab", Command::PreviousTab);
            });
            ui.menu_button("Settings", |ui| {
                item(ui, "Preferences…", "Ctrl+,", Command::Preferences);
            });
            ui.menu_button("Help", |ui| {
                item(ui, "Keyboard shortcuts", "", Command::Shortcuts);
                item(ui, "About Snekkie", "", Command::About);
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
        ui.horizontal(|ui| {
            let text = self.current().map_or_else(|| "No session".to_string(), |t| t.session.status_text());
            ui.label(RichText::new(text).color(style::TEXT_SECONDARY));
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
                    let suggested =
                        if profile.name == "New session" { self.sidebar.collect().name } else { profile.name.clone() };
                    self.input("Save session", "Name:", suggested, false, InputAction::SaveSession(Box::new(profile)));
                }
                sidebar::Action::Delete(name) => {
                    self.confirm("Delete session", format!("Delete “{name}”?"), ConfirmAction::DeleteSaved(name));
                }
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

    fn tab_strip(&mut self, ui: &mut Ui) -> Option<(u64, TabAction)> {
        let mut action = None;
        let mut rects = Vec::with_capacity(self.tabs.len());
        egui::ScrollArea::horizontal().id_salt("tab_strip").auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (index, tab) in self.tabs.iter().enumerate() {
                    let selected = index == self.active;
                    let state = tab.session.state();
                    let color = if selected { style::TEXT_PRIMARY } else { style::TEXT_SECONDARY };
                    let dot = match state {
                        State::Connected => self.accent,
                        State::Connecting => Color32::from_rgb(0xd0, 0xa0, 0x30),
                        State::Closed(_) => Color32::from_rgb(0xa0, 0x40, 0x40),
                    };
                    let id = tab.session.id;
                    let inner = Frame::new().inner_margin(Margin::symmetric(10, 5)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let (dot_rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                            ui.painter().circle_filled(dot_rect.center(), 3.5, dot);
                            ui.label(RichText::new(tab.session.title()).color(color));
                            let close = ui.add(
                                egui::Button::new(RichText::new("×").color(style::TEXT_SECONDARY)).frame(false).small(),
                            );
                            if close.on_hover_text("Close tab").clicked() {
                                action = Some((id, TabAction::Close));
                            }
                        });
                    });
                    let rect = inner.response.rect;
                    let response = ui.interact(rect, Id::new(("tab", id)), Sense::click_and_drag());
                    if selected {
                        ui.painter().hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(2.0, self.accent));
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
                        .color(style::TEXT_SECONDARY),
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
            Frame::new().fill(fill).inner_margin(Margin::symmetric(8, 5)).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::Sides::new().shrink_left().wrap().show(
                    ui,
                    |ui| ui.label(RichText::new(text).color(Color32::from_rgb(0xee, 0xee, 0xee))),
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
        let keyboard = self.dialogs.is_empty() && self.preferences.is_none();
        let options = ViewOptions { theme, syntax: &syntax, regular, bold, keyboard };
        let out = tab.view.show(ui, &tab.session, &options);
        if let Some(text) = out.copy {
            ctx.copy_text(text);
        }
        if out.paste_requested
            && let Some(text) = self.clipboard_text()
        {
            self.tabs[self.active].session.write(keys::encode_paste(&text));
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
                    ui.label(RichText::new("Key type").color(style::TEXT_SECONDARY));
                    ui.label(RichText::new(&question.key_type).monospace());
                    ui.add_space(4.0);
                    ui.label(RichText::new("Fingerprint").color(style::TEXT_SECONDARY));
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
            (Dialog::HostKey(question), result) => {
                let _ = question.reply.send(result == DialogResult::Yes);
            }
            (Dialog::Confirm { action, .. }, DialogResult::Yes) => match action {
                ConfirmAction::CloseTab(id) => self.close_tab(id, true),
                ConfirmAction::Quit => {
                    self.allow_close = true;
                    for tab in &self.tabs {
                        tab.session.shutdown();
                    }
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                ConfirmAction::DeleteSaved(name) => {
                    if let Err(e) = self.store.remove(&name) {
                        self.message("Delete session", format!("Could not save sessions: {e}"));
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
            (Dialog::Input { action, .. }, DialogResult::Text(text)) => match action {
                InputAction::Password(pending) => self.start_connect(ctx, *pending, text, String::new()),
                InputAction::Passphrase(pending) => self.start_connect(ctx, *pending, String::new(), text),
                InputAction::SaveSession(mut profile) => {
                    let name = text.trim();
                    if name.is_empty() {
                        return;
                    }
                    profile.name = name.to_string();
                    self.sidebar.draft.name = profile.name.clone();
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
        let monospace = self.fonts.monospace.clone();
        let mut outcome = preferences::Outcome::Open;
        let modal = egui::Modal::new(Id::new("preferences")).show(ctx, |ui| {
            ui.set_width(460.0);
            outcome = prefs.ui(ui, &monospace);
        });
        if modal.should_close() && !blocked {
            outcome = preferences::Outcome::Cancel;
        }
        match outcome {
            preferences::Outcome::Open => {}
            preferences::Outcome::Cancel => self.preferences = None,
            preferences::Outcome::Save(settings) => {
                self.settings = AppSettings { show_sidebar: self.settings.show_sidebar, ..settings };
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

    fn modal_open(&self) -> bool {
        !self.dialogs.is_empty() || self.preferences.is_some()
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
            .frame(Frame::new().fill(style::BG_WINDOW).inner_margin(Margin::symmetric(6, 3)))
            .show(ui, |ui| commands.extend(self.menu_bar(ui)));
        egui::Panel::bottom("status")
            .frame(
                Frame::new()
                    .fill(style::BG_WINDOW)
                    .inner_margin(Margin::symmetric(8, 3))
                    .stroke(Stroke::new(1.0, style::BORDER)),
            )
            .show(ui, |ui| self.status_bar(ui));
        if self.settings.show_sidebar {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(320.0)
                .size_range(280.0..=460.0)
                .frame(Frame::new().fill(style::BG_PANEL).inner_margin(Margin::same(10)))
                .show(ui, |ui| self.sidebar_ui(ui));
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(style::BG_WINDOW).inner_margin(Margin { left: 2, right: 0, top: 2, bottom: 0 }))
            .show(ui, |ui| self.central(ui));

        for command in commands {
            self.run_command(&ctx, command);
        }

        // Preferences sits under any dialog it opened (theme name, confirm).
        let dialog_open = !self.dialogs.is_empty();
        self.preferences_ui(&ctx, dialog_open);
        self.dialogs_ui(&ctx);

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
        for tab in &self.tabs {
            tab.session.shutdown();
        }
    }

    // -- test hooks -----------------------------------------------------

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
            })
            .collect()
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
    pub fn set_form_kind(&mut self, kind: Kind) {
        self.sidebar.set_kind(kind);
    }

    #[doc(hidden)]
    pub fn active_screen_text(&self) -> Option<String> {
        self.current().map(|t| t.session.shared.emulator.lock().screen_text())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabAction {
    Select,
    Close,
    Reconnect,
    Duplicate,
}

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
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        style::BG_WINDOW.to_normalized_gamma_f32()
    }
}
