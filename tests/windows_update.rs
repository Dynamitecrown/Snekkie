//! Opt-in production update-button test for a disposable GitHub-hosted Windows VM.
//! The installation script owns setup, test data, process cleanup and uninstall.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use snekkie::ui::{Paths, SnekkieApp};
use snekkie::update;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

#[derive(Debug, Deserialize)]
struct InstalledState {
    location: PathBuf,
    version: String,
    file_version: String,
    processes: Vec<RunningProcess>,
}

#[derive(Debug, Deserialize)]
struct RunningProcess {
    path: PathBuf,
    command_line: String,
}

fn installed_state() -> InstalledState {
    // Read-only inspection. All installation and process changes are made by
    // the production update code and the outer disposable-runner script.
    let script = r#"
$ErrorActionPreference = 'Stop'
$registration = Get-ItemProperty -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Snekkie'
$exe = Join-Path $registration.InstallLocation 'snekkie.exe'
$processes = @(Get-CimInstance Win32_Process -Filter "Name='snekkie.exe' OR Name LIKE 'Snekkie-Setup-%'" |
    Where-Object { $_.ExecutablePath } | ForEach-Object {
        @{ path = [string] $_.ExecutablePath; command_line = [string] $_.CommandLine }
    })
@{
    location = [string] $registration.InstallLocation
    version = [string] $registration.DisplayVersion
    file_version = [string] (Get-Item -LiteralPath $exe -ErrorAction SilentlyContinue).VersionInfo.ProductVersion
    processes = $processes
} | ConvertTo-Json -Depth 4 -Compress
"#;
    let output = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .expect("could not inspect the disposable Windows installation");
    assert!(output.status.success(), "installation inspection failed: {}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).expect("PowerShell returned invalid installation JSON")
}

fn same_file(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn file_hash(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).expect("could not read test executable")).to_vec()
}

struct RunningMutex(HANDLE);

impl RunningMutex {
    fn create() -> Self {
        let name: Vec<u16> = snekkie::APP_MUTEX.encode_utf16().chain(std::iter::once(0)).collect();
        // Model the old application's lifetime while the harness drives its UI.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        assert!(!handle.is_null(), "could not announce the test application");
        let mutex = Self(handle);
        assert!(!already_exists, "a Snekkie process already owns the running mutex; refusing to update");
        mutex
    }
}

impl Drop for RunningMutex {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

#[test]
#[ignore = "requires scripts/test-windows-installation.ps1 on a disposable GitHub-hosted Windows runner"]
fn update_button_downloads_installs_and_restarts_the_registered_copy() {
    for (name, expected) in
        [("GITHUB_ACTIONS", "true"), ("RUNNER_OS", "Windows"), ("RUNNER_ENVIRONMENT", "github-hosted")]
    {
        assert_eq!(std::env::var(name).as_deref(), Ok(expected), "refusing real installation outside a hosted runner");
    }
    let runner_temp = PathBuf::from(std::env::var_os("RUNNER_TEMP").expect("RUNNER_TEMP is required"))
        .canonicalize()
        .expect("RUNNER_TEMP must exist");
    let install = PathBuf::from(
        std::env::var_os("SNEKKIE_WINDOWS_UPDATE_E2E_INSTALL").expect("the script must supply its owned installation"),
    )
    .canonicalize()
    .expect("the script must install an older version before this test");
    assert!(install != runner_temp && install.starts_with(&runner_temp), "test install must be under RUNNER_TEMP");
    let exe = install.join("snekkie.exe");
    assert!(exe.is_file(), "the test installation has no executable");
    let before = installed_state();
    assert!(same_file(&before.location, &install), "HKCU points outside the script's owned installation: {before:?}");
    assert!(before.processes.is_empty(), "an app or installer is already running: {before:?}");
    let old_hash = file_hash(&exe);

    let release = update::fetch_latest(update::LATEST_RELEASE_URL)
        .expect("could not fetch the real latest GitHub release")
        .expect("GitHub returned no stable application release");
    assert!(update::is_newer(&release.version, &before.version), "the fixture must be older than the latest release");
    let asset = release.installer.as_ref().expect("the latest release has no full Windows installer").clone();
    assert!(asset.sha256.is_some(), "the live installer must have a GitHub SHA-256 digest");
    let latest_version = release.version.clone();
    println!("Driving the actual Update button: {} -> {latest_version}", before.version);

    let running = RunningMutex::create();
    let data = tempfile::tempdir().expect("could not create isolated harness settings");
    let paths = Paths { sessions: data.path().join("sessions.json"), settings: data.path().join("settings.json") };
    let mut harness = Harness::builder()
        .with_size([1000.0, 640.0])
        .build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
    harness.run_ok();
    // Discovery and installed-copy detection have separate coverage. Inject
    // only that offer, then use production UI, HTTP, verification and setup.
    harness.state_mut().offer_update(release, true);
    harness.run_ok();
    harness.get_by_label(&format!("Update to {latest_version}")).click();
    harness.run_ok();
    harness.get_by_label(&format!("What's new in {latest_version}"));
    harness.get_by_label("Update now").click();

    let deadline = Instant::now() + Duration::from_secs(5 * 60);
    loop {
        // A single step preserves a one-frame Close command that run_ok()
        // could otherwise consume while settling subsequent repaint frames.
        harness.step();
        let dialogs = harness.state().dialog_texts();
        assert!(dialogs.is_empty(), "update produced an unexpected dialog: {dialogs:?}");
        if harness
            .output()
            .viewport_output
            .values()
            .any(|viewport| viewport.commands.iter().any(|command| matches!(command, egui::ViewportCommand::Close)))
        {
            break;
        }
        assert!(Instant::now() < deadline, "Update now never requested app close after downloading");
        std::thread::sleep(Duration::from_millis(20));
    }

    let installer = std::env::temp_dir().join("snekkie-update").join(&asset.name);
    assert_eq!(std::fs::metadata(&installer).unwrap().len(), asset.size, "download size differs from GitHub");
    let digest = file_hash(&installer).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    assert_eq!(Some(digest), asset.sha256, "the button did not download the published installer bytes");

    // The production installer waits on our mutex, making inspection of its
    // real /UPDATE command deterministic even when installation is very fast.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state = installed_state();
        assert_eq!(state.version, before.version, "the installer replaced the app before it closed");
        if state.processes.iter().any(|process| {
            same_file(&process.path, &installer) && process.command_line.split_whitespace().any(|arg| arg == "/UPDATE")
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "the downloaded installer was not launched with /UPDATE: {state:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(harness);
    drop(running);

    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let state = installed_state();
        assert!(
            same_file(&state.location, &install),
            "the updater changed the registered install directory: {state:?}"
        );
        if state.version == latest_version
            && state.file_version == latest_version
            && state.processes.iter().any(|process| same_file(&process.path, &exe))
        {
            break;
        }
        assert!(Instant::now() < deadline, "the latest registered executable did not restart: {state:?}");
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_ne!(file_hash(&exe), old_hash, "the older app's executable was not replaced");
    println!(
        "PASS: Update now downloaded verified bytes, launched /UPDATE, closed, upgraded and restarted {latest_version}"
    );
    // The caller checks preserved user data and shortcuts, then stops this
    // exact restarted executable before continuing its other scenarios.
}
