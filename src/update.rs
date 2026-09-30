//! Checking GitHub for a newer Snekkie, and installing it.
//!
//! Only full releases are offered: GitHub's "latest release" skips drafts
//! and pre-releases, so a beta tag never reaches everyone.
//!
//! A copy put in place by the installer can update itself: it downloads the
//! new installer, checks it against the digest GitHub publishes for it, and
//! runs it with /UPDATE. The installer waits for Snekkie to close, upgrades
//! it in place and starts it again. A portable copy, or a Linux build, can
//! only point at the release page.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// GitHub's description of the newest full release.
pub const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/Dynamitecrown/Snekkie/releases/latest";

/// This build's version.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the installer records the install. Must match UNINSTALL_KEY in
/// installer/snekkie.nsi.
#[cfg(windows)]
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Snekkie";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// "2.2.0", without the tag's "v".
    pub version: String,
    /// The release's page on GitHub: notes and every download.
    pub page: String,
    /// The Windows installer, if the release has one.
    pub installer: Option<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Lowercase hex SHA-256, if GitHub published one.
    pub sha256: Option<String>,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// Read GitHub's JSON description of a release. Drafts and pre-releases
/// come back as None: they're never offered.
pub fn parse_release(json: &str) -> Result<Option<Release>, String> {
    let release: GitHubRelease = serde_json::from_str(json)
        .map_err(|e| format!("GitHub sent a release description Snekkie can't read ({e})"))?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let tag = release.tag_name.trim();
    let version = semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag))
        .map_err(|_| format!("the latest release is tagged “{tag}”, which isn't a version number"))?;
    // The version is checked above, so the name is safe to use as a file name.
    let installer_name = format!("Snekkie-Setup-{version}.exe");
    let installer = release.assets.into_iter().find(|a| a.name == installer_name).map(|a| Asset {
        sha256: a.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase),
        name: a.name,
        url: a.browser_download_url,
        size: a.size,
    });
    Ok(Some(Release { version: version.to_string(), page: release.html_url, installer }))
}

/// True if `candidate` is a later version than `current`. Anything that
/// isn't a version number never is.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (semver::Version::parse(candidate), semver::Version::parse(current)) {
        (Ok(candidate), Ok(current)) => candidate.cmp_precedence(&current).is_gt(),
        _ => false,
    }
}

fn agent(https_only: bool) -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    ureq::Agent::config_builder()
        .user_agent(format!("Snekkie/{CURRENT_VERSION}"))
        .https_only(https_only)
        // Trust what Windows trusts, so networks that inspect TLS with
        // their own certificate authority work too.
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(15 * 60)))
        .build()
        .new_agent()
}

/// Ask GitHub for the newest full release. None if there isn't one that
/// can be offered.
pub fn fetch_latest(url: &str) -> Result<Option<Release>, String> {
    let mut response = agent(true)
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .config()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .call()
        .map_err(|e| format!("could not reach GitHub ({e})"))?;
    let json = response.body_mut().read_to_string().map_err(|e| format!("could not reach GitHub ({e})"))?;
    parse_release(&json)
}

/// An empty folder to download into. Emptying it also removes the
/// installer left over from the last update.
pub fn fresh_download_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("snekkie-update");
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// Download `asset` into `dir` and check it's all there and, if GitHub
/// published a digest, that it's the file GitHub has. `progress` is told
/// the number of bytes so far.
pub fn download(asset: &Asset, dir: &Path, progress: impl FnMut(u64)) -> Result<PathBuf, String> {
    download_with(&agent(true), asset, dir, progress)
}

fn download_with(agent: &ureq::Agent, asset: &Asset, dir: &Path, progress: impl FnMut(u64)) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(&asset.name);
    // Only a complete, checked download gets the real name.
    let partial = dir.join(format!("{}.part", asset.name));
    let result = fetch_to(agent, asset, &partial, progress)
        .and_then(|()| fs::rename(&partial, &path).map_err(|e| format!("could not save the installer: {e}")));
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result.map(|()| path)
}

fn fetch_to(agent: &ureq::Agent, asset: &Asset, path: &Path, mut progress: impl FnMut(u64)) -> Result<(), String> {
    let failed = |e: &dyn std::fmt::Display| format!("the download failed ({e})");
    let mut response = agent.get(&asset.url).call().map_err(|e| failed(&e))?;
    // One byte of slack: the limit trips on the read that finds the end of
    // a body exactly `size` long. The size check below catches the rest.
    let mut body = response.body_mut().with_config().limit(asset.size + 1).reader();
    let mut file = File::create(path).map_err(|e| format!("could not save the installer: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut received = 0u64;
    loop {
        let n = body.read(&mut buf).map_err(|e| failed(&e))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("could not save the installer: {e}"))?;
        hasher.update(&buf[..n]);
        received += n as u64;
        progress(received);
    }
    file.sync_all().map_err(|e| format!("could not save the installer: {e}"))?;

    if received != asset.size {
        return Err(format!("the download was cut short ({received} of {} bytes)", asset.size));
    }
    if let Some(expected) = &asset.sha256 {
        let actual = hasher.finalize().iter().fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        if &actual != expected {
            return Err("the downloaded installer doesn't match the release (its SHA-256 differs)".into());
        }
    }
    Ok(())
}

/// Start a downloaded installer in update mode. It waits for Snekkie to
/// close before installing, then starts it again.
pub fn run_installer(path: &Path) -> std::io::Result<()> {
    std::process::Command::new(path).arg("/UPDATE").spawn().map(drop)
}

/// True if this copy of Snekkie is the one the installer manages, so
/// running a newer installer upgrades this copy and not another.
#[cfg(windows)]
pub fn installed_here() -> bool {
    let Some(location) = registry_string(UNINSTALL_KEY, "InstallLocation") else { return false };
    let Some(exe_dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) else {
        return false;
    };
    match (fs::canonicalize(location), fs::canonicalize(exe_dir)) {
        (Ok(installed), Ok(running)) => installed == running,
        _ => false,
    }
}

/// Only the Windows build has an installer.
#[cfg(not(windows))]
pub fn installed_here() -> bool {
    false
}

/// A string value under HKEY_CURRENT_USER.
#[cfg(windows)]
fn registry_string(key: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};

    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (key, value) = (wide(key), wide(value));
    let mut buf = vec![0u16; 4096];
    let mut bytes = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let text = &buf[..(bytes as usize / 2).min(buf.len())];
    let text = text.split(|&c| c == 0).next().unwrap_or_default();
    Some(String::from_utf16_lossy(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    const INSTALLER: &[u8] = b"pretend this is an installer";

    fn release_json(tag: &str, prerelease: bool) -> String {
        format!(
            r#"{{
              "tag_name": "{tag}",
              "html_url": "https://github.com/Dynamitecrown/Snekkie/releases/tag/{tag}",
              "draft": false,
              "prerelease": {prerelease},
              "assets": [
                {{"name": "snekkie-2.2.0-portable.exe", "size": 16941568,
                  "browser_download_url": "https://example.invalid/portable.exe",
                  "digest": "sha256:193b3b84a94c8ad00fca8b16fa20fb1abc3dae6564439040700b278b333db524"}},
                {{"name": "Snekkie-Setup-2.2.0.exe", "size": 6313194,
                  "browser_download_url": "https://example.invalid/Snekkie-Setup-2.2.0.exe",
                  "digest": "sha256:EF53D89D66FA2DC0A38233E2A80CA7E13F92073AD339CCC1C07F7879AE115366"}}
              ]
            }}"#
        )
    }

    #[test]
    fn reads_a_github_release() {
        let release = parse_release(&release_json("v2.2.0", false)).unwrap().unwrap();
        assert_eq!(release.version, "2.2.0");
        assert_eq!(release.page, "https://github.com/Dynamitecrown/Snekkie/releases/tag/v2.2.0");
        assert_eq!(
            release.installer,
            Some(Asset {
                name: "Snekkie-Setup-2.2.0.exe".into(),
                url: "https://example.invalid/Snekkie-Setup-2.2.0.exe".into(),
                size: 6313194,
                sha256: Some("ef53d89d66fa2dc0a38233e2a80ca7e13f92073ad339ccc1c07f7879ae115366".into()),
            })
        );
    }

    #[test]
    fn only_the_installer_for_that_version_is_picked() {
        // The installer asset is for 2.2.0, the tag says 2.3.0.
        let release = parse_release(&release_json("v2.3.0", false)).unwrap().unwrap();
        assert_eq!(release.version, "2.3.0");
        assert_eq!(release.installer, None);
    }

    #[test]
    fn pre_releases_and_odd_tags_are_not_offered() {
        assert_eq!(parse_release(&release_json("v2.2.0-beta.1", true)).unwrap(), None);
        assert!(parse_release(&release_json("nightly", false)).unwrap_err().contains("“nightly”"));
        assert!(parse_release("<html>rate limited</html>").is_err());
    }

    /// Talks to the real GitHub: `cargo test --lib update -- --ignored`.
    #[test]
    #[ignore]
    fn github_has_a_release_with_an_installer() {
        let release = fetch_latest(LATEST_RELEASE_URL).unwrap().expect("no full release");
        let installer = release.installer.expect("no installer");
        assert!(installer.url.starts_with("https://"));
        assert_eq!(installer.sha256.map(|d| d.len()), Some(64));
    }

    #[test]
    fn version_ordering() {
        assert!(is_newer("2.2.0", "2.1.0"));
        assert!(is_newer("2.10.0", "2.9.3"));
        assert!(is_newer("3.0.0", "2.99.99"));
        assert!(is_newer("2.1.0", "2.1.0-beta.1"));
        assert!(!is_newer("2.1.0", "2.1.0"));
        assert!(!is_newer("2.0.9", "2.1.0"));
        assert!(!is_newer("2.1.0-beta.1", "2.1.0"));
        assert!(!is_newer("2.1.0+build.5", "2.1.0"));
        assert!(!is_newer("junk", "2.1.0"));
    }

    /// Serve `body` once over plain HTTP, claiming it's `length` bytes long.
    fn serve(body: &'static [u8], length: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/Snekkie-Setup-2.2.0.exe", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let mut stream = stream;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        });
        url
    }

    fn asset(url: String, sha256: Option<&str>) -> Asset {
        Asset {
            name: "Snekkie-Setup-2.2.0.exe".into(),
            url,
            size: INSTALLER.len() as u64,
            sha256: sha256.map(String::from),
        }
    }

    fn sha256_of(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn downloads_and_checks_the_installer() {
        let dir = tempfile::tempdir().unwrap();
        let expected = sha256_of(INSTALLER);
        let asset = asset(serve(INSTALLER, INSTALLER.len()), Some(&expected));
        let mut seen = 0;
        let path = download_with(&agent(false), &asset, dir.path(), |bytes| seen = bytes).unwrap();
        assert_eq!(path, dir.path().join("Snekkie-Setup-2.2.0.exe"));
        assert_eq!(fs::read(&path).unwrap(), INSTALLER);
        assert_eq!(seen, INSTALLER.len() as u64);
    }

    #[test]
    fn a_corrupt_download_is_thrown_away() {
        let dir = tempfile::tempdir().unwrap();
        let wrong = "0".repeat(64);
        let asset = asset(serve(INSTALLER, INSTALLER.len()), Some(&wrong));
        let error = download_with(&agent(false), &asset, dir.path(), |_| {}).unwrap_err();
        assert!(error.contains("SHA-256"), "{error}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_short_download_is_thrown_away() {
        let dir = tempfile::tempdir().unwrap();
        // The server promises the whole file but hangs up half way.
        let asset = asset(serve(&INSTALLER[..10], INSTALLER.len()), None);
        assert!(download_with(&agent(false), &asset, dir.path(), |_| {}).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_download_bigger_than_promised_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut asset = asset(serve(INSTALLER, INSTALLER.len()), None);
        asset.size -= 1;
        assert!(download_with(&agent(false), &asset, dir.path(), |_| {}).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
