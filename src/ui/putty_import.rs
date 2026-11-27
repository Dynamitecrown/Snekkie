use crate::{
    profiles::Profile,
    putty::{self, ImportBatch},
    settings::AppSettings,
    ssh_config,
};
use std::sync::mpsc::{self, Receiver};

pub struct PuttyImport {
    defaults: AppSettings,
    batch: ImportBatch,
    selected: Vec<bool>,
    pending: Option<Receiver<Result<ImportBatch, String>>>,
    error: Option<String>,
    loaded: bool,
    /// Reading an OpenSSH client config rather than PuTTY sessions.
    openssh: bool,
}

impl PuttyImport {
    pub fn new(defaults: &AppSettings) -> Self {
        Self {
            defaults: defaults.clone(),
            batch: ImportBatch::default(),
            selected: Vec::new(),
            pending: None,
            error: None,
            loaded: false,
            openssh: false,
        }
    }
    /// The same preview for ~/.ssh/config, read straight away.
    pub fn openssh(ctx: &egui::Context, defaults: &AppSettings) -> Self {
        let mut import = Self { openssh: true, ..Self::new(defaults) };
        import.load(ctx, None);
        import
    }
    /// The OpenSSH preview over an already parsed file (QA and tests).
    pub fn openssh_batch(defaults: &AppSettings, batch: ImportBatch) -> Self {
        let mut import = Self { openssh: true, ..Self::new(defaults) };
        import.set_batch(batch);
        import
    }
    pub fn is_openssh(&self) -> bool {
        self.openssh
    }
    pub fn set_batch(&mut self, batch: ImportBatch) {
        self.selected = vec![true; batch.profiles.len()];
        self.batch = batch;
        self.loaded = true;
        self.error = None;
    }
    pub fn profiles(&self) -> Vec<Profile> {
        self.batch
            .profiles
            .iter()
            .zip(&self.selected)
            .filter(|(_, selected)| **selected)
            .map(|(p, _)| p.clone())
            .collect()
    }
    fn load(&mut self, ctx: &egui::Context, path: Option<std::path::PathBuf>) {
        self.loaded = false;
        self.batch = ImportBatch::default();
        self.selected.clear();
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        self.error = None;
        let defaults = self.defaults.clone();
        let ctx = ctx.clone();
        let openssh = self.openssh;
        if let Err(e) = std::thread::Builder::new().name("PuTTY import".into()).spawn(move || {
            let result = match path {
                _ if openssh => ssh_config::read(&path.unwrap_or_else(ssh_config::default_path), &defaults),
                Some(p) => putty::read_export(&p, &defaults),
                None => putty::read_registry(&defaults),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        }) {
            self.pending = None;
            self.error = Some(format!("Could not start import: {e}"));
        }
    }
    pub fn title(&self) -> &'static str {
        if self.openssh { "Import OpenSSH config" } else { "Import PuTTY sessions" }
    }
    /// Some(true) imports the selected profiles; Some(false) cancels.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<bool> {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(result) => {
                    self.pending = None;
                    match result {
                        Ok(batch) => self.set_batch(batch),
                        Err(e) => self.error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.error = Some("The import worker stopped unexpectedly.".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        ui.heading(self.title());
        let room = (ui.ctx().content_rect().height() - 150.0).max(60.0);
        egui::ScrollArea::vertical().id_salt("putty_import_body").max_height(room).auto_shrink([false, true]).show(ui, |ui| {
        ui.label(if self.openssh {
            "Preview and select hosts before adding them. Existing Snekkie sessions are kept; conflicting names get an Imported suffix. Nothing is connected or run."
        } else {
            "Preview and select sessions before adding them. Existing Snekkie sessions are kept; conflicting names get a PuTTY suffix. Nothing is connected automatically."
        });
        ui.add_enabled_ui(self.pending.is_none(), |ui| {
            if self.openssh {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Read ~/.ssh/config").clicked() {
                        self.load(ui.ctx(), None);
                    }
                    if ui.button("Open other config file…").clicked()
                        && let Some(path) = rfd::FileDialog::new().set_title("OpenSSH config").pick_file()
                    {
                        self.load(ui.ctx(), Some(path));
                    }
                });
                return;
            }
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(cfg!(windows), egui::Button::new("Read Windows PuTTY sessions")).clicked() {
                    self.load(ui.ctx(), None);
                }
                if ui.button("Open PuTTY .reg export…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .set_title("PuTTY registry export")
                        .add_filter("Registry export", &["reg"])
                        .pick_file()
                {
                    self.load(ui.ctx(), Some(path));
                }
            });
        });
        if self.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading sessions…");
            });
        }
        if let Some(error) = &self.error {
            ui.label(error);
        }
        if self.loaded {
            ui.label(format!("{} supported session(s) found", self.batch.profiles.len()));
            ui.horizontal(|ui| {
                if ui.button("Select all").clicked() {
                    self.selected.fill(true);
                }
                if ui.button("Select none").clicked() {
                    self.selected.fill(false);
                }
            });
            egui::ScrollArea::vertical().id_salt("putty_sessions").max_height(230.0).show(ui, |ui| {
                for (profile, selected) in self.batch.profiles.iter().zip(&mut self.selected) {
                    ui.checkbox(selected, &profile.name);
                    let destination = if profile.kind().is_network() {
                        format!("{}:{}", profile.host, profile.port)
                    } else {
                        format!("{} / {} baud", profile.device, profile.baud)
                    };
                    ui.label(egui::RichText::new(format!("{} — {destination}", profile.kind().label())).small());
                }
            });
            if !self.batch.warnings.is_empty() {
                egui::CollapsingHeader::new(format!("{} import note(s)", self.batch.warnings.len()))
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().id_salt("putty_notes").max_height(90.0).show(ui, |ui| {
                            for warning in &self.batch.warnings {
                                ui.label(egui::RichText::new(warning).small());
                            }
                        });
                    });
            }
        }
        if self.openssh {
            ui.label(egui::RichText::new("Imports Host blocks: HostName, User, Port, IdentityFile (as a key file path), ProxyJump to another imported host, and simple Local, Remote and Dynamic forwards. Wildcard hosts, Match, Include, ProxyCommand and other options are not imported.").small());
        } else {
        ui.label(egui::RichText::new("Imports SSH, Telnet, raw TCP and supported serial settings. Simple port forwardings are imported as tunnels. Passwords, host keys, colors and proxies are not imported. .ppk keys require Pageant or conversion to OpenSSH.").small());
        }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.pending.is_none() && self.selected.contains(&true),
                    egui::Button::new("Import selected"),
                )
                .clicked()
            {
                return Some(true);
            }
            if ui.button("Cancel").clicked() {
                return Some(false);
            }
            None
        })
        .inner
    }
}
