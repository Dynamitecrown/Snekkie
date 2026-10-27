//! Snekkie's icon in the Windows notification area (the hidden icons beside
//! the clock). The app shows it while it runs; with the app closed, one of
//! the agents' `--mcp` bridges shows it instead, so there is one icon either
//! way and a bridge can always be closed from it.
//!
//! The icon belongs to a hidden window on its own thread, which pumps that
//! window's messages and hands the chosen command on.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{Receiver, channel};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    ToggleAgents,
    Quit,
    QuitWithBridges,
    OpenApp,
    CloseBridges,
}

/// One menu entry; None is a separator.
pub type MenuItem = Option<(String, TrayCommand)>;

fn item(label: &str, command: TrayCommand) -> MenuItem {
    Some((label.into(), command))
}

pub struct Tray {
    /// The hidden window, once its thread has made it; 0 before.
    window: Arc<AtomicIsize>,
    visible: Arc<AtomicBool>,
}

const CALLBACK: u32 = WM_APP + 1;
const VISIBILITY: u32 = WM_APP + 2;
const ICON_ID: u32 = 1;
/// NIN_SELECT | NINF_KEY, which windows-sys doesn't name.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

struct State {
    menu: Box<dyn Fn() -> Vec<MenuItem> + Send>,
    on_command: Box<dyn Fn(TrayCommand) + Send>,
    visible: Arc<AtomicBool>,
    /// Sent when Explorer restarts, which forgets every icon.
    taskbar_created: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

impl Tray {
    /// `menu` is asked for its entries each time the menu opens; clicking the
    /// icon does what the first one does. `on_command` runs on the tray's thread.
    pub fn new(
        visible: bool,
        menu: impl Fn() -> Vec<MenuItem> + Send + 'static,
        on_command: impl Fn(TrayCommand) + Send + 'static,
    ) -> Tray {
        let window = Arc::new(AtomicIsize::new(0));
        let visible = Arc::new(AtomicBool::new(visible));
        let state = State {
            menu: Box::new(menu),
            on_command: Box::new(on_command),
            visible: visible.clone(),
            taskbar_created: 0,
        };
        let made = window.clone();
        // Without a thread there's no icon, which is no reason to stop.
        let _ = std::thread::Builder::new().name("tray".into()).spawn(move || run(&made, state));
        Tray { window, visible }
    }

    pub fn set_visible(&self, visible: bool) {
        let window = self.window.load(Ordering::Acquire) as HWND;
        if self.visible.swap(visible, Ordering::AcqRel) != visible && !window.is_null() {
            unsafe { PostMessageW(window, VISIBILITY, 0, 0) };
        }
    }

    /// Take the icon away now. Done here, not on the tray thread: the
    /// process may exit before that thread runs again, leaving a dead icon.
    pub fn remove(&self) {
        self.visible.store(false, Ordering::Release);
        let window = self.window.load(Ordering::Acquire) as HWND;
        if !window.is_null() {
            unsafe { Shell_NotifyIconW(NIM_DELETE, &icon_data(window)) };
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        let window = self.window.load(Ordering::Acquire) as HWND;
        if !window.is_null() {
            unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
        }
    }
}

/// The app's icon. Its commands wait for the next frame, which `wake` asks for.
pub struct AppTray {
    _tray: Tray,
    agents: Arc<AtomicBool>,
    commands: Receiver<TrayCommand>,
}

impl AppTray {
    pub fn new(wake: impl Fn() + Send + 'static) -> AppTray {
        let agents = Arc::new(AtomicBool::new(false));
        let running = agents.clone();
        let menu = move || {
            let server = if running.load(Ordering::Relaxed) {
                "Stop AI agent server (MCP)"
            } else {
                "Start AI agent server (MCP)"
            };
            let bridge = crate::mcp::bridges::running();
            let mut items = vec![item("Show Snekkie", TrayCommand::Show), item(server, TrayCommand::ToggleAgents)];
            if bridge {
                items.push(item("Close AI agent bridge", TrayCommand::CloseBridges));
            }
            items.extend([None, item("Quit Snekkie", TrayCommand::Quit)]);
            if bridge {
                items.push(item("Quit Snekkie and AI agent bridge", TrayCommand::QuitWithBridges));
            }
            items
        };
        let (sender, commands) = channel();
        let tray = Tray::new(true, menu, move |command| {
            let _ = sender.send(command);
            wake();
        });
        AppTray { _tray: tray, agents, commands }
    }

    /// Whether the agent server is running, for the menu's wording.
    pub fn set_agents(&self, running: bool) {
        self.agents.store(running, Ordering::Relaxed);
    }

    pub fn commands(&self) -> impl Iterator<Item = TrayCommand> + '_ {
        self.commands.try_iter()
    }
}

/// The menu a bridge shows while the app is closed.
pub fn bridge_menu() -> Vec<MenuItem> {
    vec![item("Open Snekkie", TrayCommand::OpenApp), item("Close AI agent bridge", TrayCommand::CloseBridges)]
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn run(made: &AtomicIsize, mut state: State) {
    unsafe {
        // Explorer gives the icon's position in physical pixels. The bridge
        // isn't DPI aware, so without this its menu opens off toward the
        // bottom right whenever display scaling is above 100%.
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(std::ptr::null());
        let class = wide("SnekkieTray");
        let mut window_class: WNDCLASSW = std::mem::zeroed();
        window_class.lpfnWndProc = Some(procedure);
        window_class.hInstance = instance;
        window_class.lpszClassName = class.as_ptr();
        RegisterClassW(&window_class);
        // A hidden top-level window, not a message-only one: only top-level
        // windows hear that Explorer restarted.
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if window.is_null() {
            return;
        }
        state.taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        STATE.with_borrow_mut(|slot| *slot = Some(state));
        // Published first, so a set_visible from now on reaches the window.
        made.store(window as isize, Ordering::Release);
        sync_icon(window);
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn icon_data(window: HWND) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = ICON_ID;
    data
}

/// Add or take away the icon to match what was asked for. Adding one that
/// is already there just fails.
unsafe fn sync_icon(window: HWND) {
    let visible = STATE.with_borrow(|state| state.as_ref().is_some_and(|state| state.visible.load(Ordering::Acquire)));
    let mut data = icon_data(window);
    if !visible {
        unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        return;
    }
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    data.uCallbackMessage = CALLBACK;
    unsafe {
        // build.rs embeds the app icon as resource 1.
        let (width, height) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        data.hIcon = LoadImageW(GetModuleHandleW(std::ptr::null()), 1 as _, IMAGE_ICON, width, height, 0);
        if data.hIcon.is_null() {
            data.hIcon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
        }
    }
    for (slot, unit) in data.szTip.iter_mut().zip("Snekkie".encode_utf16()) {
        *slot = unit;
    }
    unsafe {
        Shell_NotifyIconW(NIM_ADD, &data);
        // Version 4 reports keyboard use too, so the icon works without a mouse.
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        Shell_NotifyIconW(NIM_SETVERSION, &data);
    }
}

fn menu_items() -> Vec<MenuItem> {
    STATE.with_borrow(|state| state.as_ref().map(|state| (state.menu)()).unwrap_or_default())
}

fn send(command: TrayCommand) {
    STATE.with_borrow(|state| {
        if let Some(state) = state {
            (state.on_command)(command);
        }
    });
}

unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let taskbar_created = STATE.with_borrow(|state| state.as_ref().map_or(0, |state| state.taskbar_created));
    unsafe {
        match message {
            // Version 4: the event is in the low word, the anchor point in wparam.
            CALLBACK => match (lparam & 0xffff) as u32 {
                WM_CONTEXTMENU => show_menu(window, wparam as i16 as i32, (wparam >> 16) as i16 as i32),
                NIN_SELECT | NIN_KEYSELECT => {
                    if let Some(Some((_, command))) = menu_items().into_iter().next() {
                        send(command);
                    }
                }
                _ => {}
            },
            VISIBILITY => sync_icon(window),
            WM_CLOSE => {
                DestroyWindow(window);
            }
            WM_DESTROY => PostQuitMessage(0),
            _ if message == taskbar_created && taskbar_created != 0 => sync_icon(window),
            _ => return DefWindowProcW(window, message, wparam, lparam),
        }
    }
    0
}

unsafe fn show_menu(window: HWND, x: i32, y: i32) {
    // Built before the menu opens: it runs its own message loop.
    let items = menu_items();
    unsafe {
        let menu = CreatePopupMenu();
        for (index, entry) in items.iter().enumerate() {
            match entry {
                // Ids start at 1: TrackPopupMenu returns 0 for nothing chosen.
                Some((label, _)) => AppendMenuW(menu, MF_STRING, index + 1, wide(label).as_ptr()),
                None => AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null()),
            };
        }
        SetMenuDefaultItem(menu, 0, 1);
        // Without this the menu won't close when the user clicks elsewhere.
        SetForegroundWindow(window);
        let chosen =
            TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, x, y, 0, window, std::ptr::null());
        PostMessageW(window, WM_NULL, 0, 0);
        DestroyMenu(menu);
        if let Some(Some((_, command))) = (chosen as usize).checked_sub(1).and_then(|index| items.get(index)) {
            send(*command);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for(deadline: Instant, what: &str, mut done: impl FnMut() -> bool) {
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn clicking_the_icon_does_the_first_menu_entry() {
        let (sender, received) = channel();
        let tray = Tray::new(false, bridge_menu, move |command| {
            let _ = sender.send(command);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        wait_for(deadline, "the tray window was never made", || tray.window.load(Ordering::Acquire) != 0);
        let window = tray.window.load(Ordering::Acquire) as HWND;
        // The test process isn't DPI aware, like the bridge: the menu still
        // has to use Explorer's physical-pixel coordinates.
        assert_ne!(
            unsafe {
                windows_sys::Win32::UI::HiDpi::AreDpiAwarenessContextsEqual(
                    windows_sys::Win32::UI::HiDpi::GetWindowDpiAwarenessContext(window),
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                )
            },
            0,
            "the tray window isn't per-monitor DPI aware"
        );
        // What Explorer sends for a click, or Enter on the focused icon.
        for event in [NIN_SELECT, NIN_KEYSELECT] {
            unsafe { PostMessageW(window, CALLBACK, 0, (event | (ICON_ID << 16)) as LPARAM) };
        }
        let mut seen = Vec::new();
        wait_for(deadline, "the clicks never arrived", || {
            seen.extend(received.try_iter());
            seen.len() == 2
        });
        assert_eq!(seen, [TrayCommand::OpenApp, TrayCommand::OpenApp]);
        // Showing and hiding only posts to the tray's own thread.
        tray.set_visible(true);
        tray.set_visible(false);
        drop(tray);
        wait_for(deadline, "closing the tray leaves its window behind", || unsafe { IsWindow(window) } == 0);
    }
}
