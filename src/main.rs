// No console window behind the app on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

use snekkie::config;
use snekkie::ui::{Paths, SnekkieApp};

fn icon() -> Option<egui::IconData> {
    let image = image::load_from_memory(include_bytes!("../assets/icon.png")).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData { rgba: image.into_raw(), width, height })
}

/// Hold a named mutex for the life of the process, so the installer can
/// tell Snekkie is running before it tries to replace the exe.
#[cfg(windows)]
fn announce_running() {
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = snekkie::APP_MUTEX.encode_utf16().chain(std::iter::once(0)).collect();
    // Deliberately never closed: Windows releases it when the process exits.
    unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
}

#[cfg(not(windows))]
fn announce_running() {}

/// Tell the user why Snekkie couldn't start. The Windows build has no
/// console, so without this a startup failure would just be a window that
/// never appears.
#[cfg(windows)]
fn report_startup_error(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let text = wide(&format!(
        "Snekkie could not start.\n\n{message}\n\nIf this mentions the graphics adapter, updating the display \
         driver usually fixes it. You can also try setting the environment variable \
         WGPU_BACKEND=gl (or dx12, or vulkan) before starting Snekkie."
    ));
    let title = wide("Snekkie");
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

#[cfg(not(windows))]
fn report_startup_error(message: &str) {
    eprintln!("Snekkie could not start: {message}");
}

fn main() -> std::process::ExitCode {
    announce_running();
    // Before anything reads config: the app used to store it elsewhere.
    config::migrate_legacy_config(&config::config_dir());

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Snekkie")
        .with_app_id("snekkie")
        .with_inner_size([1000.0, 640.0])
        .with_min_inner_size([520.0, 320.0]);
    if let Some(icon) = icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: snekkie::ui::render::wgpu_options(),
        ..Default::default()
    };
    let result =
        eframe::run_native("Snekkie", options, Box::new(|_cc| Ok(Box::new(SnekkieApp::new(Paths::default())))));
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            report_startup_error(&e.to_string());
            std::process::ExitCode::FAILURE
        }
    }
}
