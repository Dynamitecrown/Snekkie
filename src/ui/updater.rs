//! The update check and download, run in the background so a slow or
//! blocked network never holds up the window.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::update::{self, Release};

/// Where updating has got to.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Not checked yet, or nothing newer.
    Idle,
    Checking,
    Available(Release),
    Downloading {
        release: Release,
        percent: u8,
    },
    /// Downloaded; installing it needs Snekkie to close.
    Ready {
        release: Release,
        installer: PathBuf,
    },
}

/// Something the app should act on.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A check the user asked for found nothing newer.
    UpToDate,
    /// A check the user asked for found a newer version.
    Found,
    /// A check the user asked for, or a download, went wrong.
    Failed(String),
    /// The installer is downloaded and checked.
    Downloaded,
}

enum Message {
    Checked(Result<Option<Release>, String>),
    Progress(u8),
    Downloaded(Result<PathBuf, String>),
}

pub struct Updater {
    pub status: Status,
    /// True if this copy was put here by the installer and so can install
    /// an update itself. Otherwise the user is pointed at the release page.
    pub can_install: bool,
    /// Checking starts on the first frame, which has a context to wake.
    check_on_start: bool,
    /// The user asked for the check under way, so its outcome is reported
    /// whatever it is. A check at startup keeps quiet unless it finds one.
    asked: bool,
    offline: bool,
    tx: Sender<Message>,
    rx: Receiver<Message>,
}

impl Default for Updater {
    fn default() -> Self {
        Self::new()
    }
}

impl Updater {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Updater {
            status: Status::Idle,
            can_install: update::installed_here(),
            check_on_start: false,
            asked: false,
            offline: false,
            tx,
            rx,
        }
    }

    /// Check quietly as soon as the window is up.
    pub fn check_at_startup(&mut self) {
        self.check_on_start = !self.offline;
    }

    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Checking | Status::Downloading { .. })
    }

    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
        if offline {
            self.check_on_start = false;
            if !self.busy() {
                self.status = Status::Idle;
            }
        }
    }

    /// Ask GitHub for the latest release. `asked` if the user asked.
    pub fn check(&mut self, ctx: &egui::Context, asked: bool) {
        if self.offline {
            return;
        }
        match self.status {
            Status::Idle => {}
            // Already on it; just make sure the answer gets reported.
            Status::Checking => {
                self.asked |= asked;
                return;
            }
            _ => return,
        }
        self.status = Status::Checking;
        self.asked = asked;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        spawn("update check", move || {
            let result = update::fetch_latest(update::LATEST_RELEASE_URL)
                .map(|latest| latest.filter(|r| update::is_newer(&r.version, update::CURRENT_VERSION)));
            let _ = tx.send(Message::Checked(result));
            ctx.request_repaint();
        });
    }

    /// Start downloading the installer for the available release.
    pub fn download(&mut self, ctx: &egui::Context) {
        if self.offline {
            return;
        }
        let Status::Available(release) = &self.status else { return };
        let Some(asset) = release.installer.clone() else { return };
        self.status = Status::Downloading { release: release.clone(), percent: 0 };
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        spawn("update download", move || {
            let mut shown = 0;
            let result = update::download(&asset, &update::fresh_download_dir(), |bytes| {
                let percent = (bytes.saturating_mul(100) / asset.size.max(1)).min(100) as u8;
                if percent != shown {
                    shown = percent;
                    let _ = tx.send(Message::Progress(percent));
                    ctx.request_repaint();
                }
            });
            let _ = tx.send(Message::Downloaded(result));
            ctx.request_repaint();
        });
    }

    /// The installer to run, once downloaded.
    pub fn installer(&self) -> Option<&PathBuf> {
        match &self.status {
            Status::Ready { installer, .. } => Some(installer),
            _ => None,
        }
    }

    /// Pick up what the background work has done since last frame.
    pub fn poll(&mut self, ctx: &egui::Context) -> Vec<Event> {
        if std::mem::take(&mut self.check_on_start) {
            self.check(ctx, false);
        }
        let mut events = Vec::new();
        while let Ok(message) = self.rx.try_recv() {
            if self.offline {
                self.status = Status::Idle;
                continue;
            }
            match message {
                Message::Checked(result) => {
                    let asked = std::mem::take(&mut self.asked);
                    self.status = Status::Idle;
                    match result {
                        Ok(Some(release)) => {
                            self.status = Status::Available(release);
                            if asked {
                                events.push(Event::Found);
                            }
                        }
                        Ok(None) if asked => events.push(Event::UpToDate),
                        Err(e) if asked => events.push(Event::Failed(format!("Could not check for updates: {e}."))),
                        _ => {}
                    }
                }
                Message::Progress(percent) => {
                    if let Status::Downloading { percent: p, .. } = &mut self.status {
                        *p = percent;
                    }
                }
                Message::Downloaded(result) => {
                    let Status::Downloading { release, .. } = &self.status else { continue };
                    let release = release.clone();
                    match result {
                        Ok(installer) => {
                            self.status = Status::Ready { release, installer };
                            events.push(Event::Downloaded);
                        }
                        Err(e) => {
                            self.status = Status::Available(release);
                            events.push(Event::Failed(format!("Could not download the update: {e}.")));
                        }
                    }
                }
            }
        }
        events
    }
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
    // Without a thread there's no update, which is no reason to stop.
    let _ = std::thread::Builder::new().name(name.into()).spawn(work);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_mode_blocks_startup_manual_checks_and_downloads() {
        let ctx = egui::Context::default();
        let mut updater = Updater::new();
        updater.set_offline(true);
        updater.check_at_startup();
        updater.check(&ctx, true);
        assert_eq!(updater.status, Status::Idle);
        assert!(updater.poll(&ctx).is_empty());
        updater.status = Status::Available(Release {
            version: "99.0.0".into(),
            page: "https://example.invalid".into(),
            installer: Some(crate::update::Asset {
                name: "test.exe".into(),
                url: "https://example.invalid".into(),
                size: 1,
                sha256: None,
            }),
        });
        updater.download(&ctx);
        assert!(matches!(updater.status, Status::Available(_)));
        updater.set_offline(true);
        assert_eq!(updater.status, Status::Idle);
    }
}
