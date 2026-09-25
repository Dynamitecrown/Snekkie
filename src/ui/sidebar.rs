//! Left-hand panel: the new-session form and saved sessions.
//!
//! Always visible rather than a popup, so starting a second session never
//! means closing what you're looking at first.

use egui::{ComboBox, DragValue, RichText, TextEdit, Ui};

use super::style;
use crate::profiles::{Auth, Kind, Profile, ProfileStore};
use crate::settings::AppSettings;
use crate::terminal::highlight::{SYNTAX_LABELS, syntax_label};
use crate::transport::serial::{self, BAUD_RATES, DATA_BITS, PARITIES, PortInfo, STOP_BITS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Ssh,
    Serial,
    Advanced,
}

/// What the sidebar asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Connect(Profile),
    /// Ask for a name, then save this profile under it.
    Save(Profile),
    /// Ask, then delete the saved session with this name.
    Delete(String),
    /// Ask for a device path typed by hand ("Other…").
    OtherDevice,
    /// Ask for a baud rate that isn't in the list.
    OtherBaud,
    BrowseKey,
    BrowseLog,
    Warn(String),
}

/// One entry in the Port drop-down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortChoice {
    pub label: String,
    pub device: String,
}

/// Detected ports, then any saved or typed-in device that isn't attached
/// right now, so it stays selectable instead of vanishing from the form.
pub fn port_choices(ports: &[PortInfo], extra: &[&str]) -> Vec<PortChoice> {
    let mut choices: Vec<PortChoice> =
        ports.iter().map(|p| PortChoice { label: p.label(), device: p.device.clone() }).collect();
    for device in extra {
        let device = device.trim();
        if device.is_empty() || choices.iter().any(|c| c.device == device) {
            continue;
        }
        choices.push(PortChoice { label: format!("{device}  (not detected)"), device: device.to_string() });
    }
    choices
}

/// With exactly one port attached it is picked for you. With several,
/// nothing is picked until you choose, so a session never silently lands on
/// the wrong console cable.
pub fn auto_select(current: &str, ports: &[PortInfo]) -> String {
    if current.is_empty() && ports.len() == 1 { ports[0].device.clone() } else { current.to_string() }
}

pub fn port_placeholder(ports: &[PortInfo]) -> String {
    match ports.len() {
        0 => "No serial ports detected".into(),
        1 => "Choose a port".into(),
        n => format!("Choose a port ({n} detected)"),
    }
}

pub fn validate(profile: &Profile) -> Result<(), String> {
    match profile.kind() {
        Kind::Ssh if profile.host.trim().is_empty() => Err("Enter a host to connect to.".into()),
        Kind::Serial if profile.device.trim().is_empty() => Err("Choose a serial port from the Port list.".into()),
        _ => Ok(()),
    }
}

pub struct Sidebar {
    /// The form's contents.
    pub draft: Profile,
    page: Page,
    ports: Vec<PortInfo>,
    /// Devices typed in via "Other…" this run.
    custom_devices: Vec<String>,
    port_popup_was_open: bool,
    selected_saved: Option<String>,
    focus_host: bool,
    list_ports: fn() -> Vec<PortInfo>,
}

impl Sidebar {
    pub fn new(settings: &AppSettings) -> Self {
        Self::with_port_lister(settings, serial::list_ports)
    }

    /// For tests: swap in a fake port list.
    pub fn with_port_lister(settings: &AppSettings, list_ports: fn() -> Vec<PortInfo>) -> Self {
        let mut sidebar = Sidebar {
            draft: Profile::default(),
            page: Page::Ssh,
            ports: Vec::new(),
            custom_devices: Vec::new(),
            port_popup_was_open: false,
            selected_saved: None,
            focus_host: false,
            list_ports,
        };
        sidebar.reset(settings);
        sidebar
    }

    /// Clear the form for a new session, with the app's defaults filled in.
    pub fn reset(&mut self, settings: &AppSettings) {
        let profile = Profile {
            font_family: settings.font_family.clone(),
            font_size: settings.font_size,
            scrollback: settings.scrollback,
            ..Profile::default()
        };
        self.load(profile);
        self.focus_host = true;
    }

    /// Fill the form from a profile.
    pub fn load(&mut self, profile: Profile) {
        self.page = match profile.kind() {
            Kind::Ssh => Page::Ssh,
            Kind::Serial => Page::Serial,
        };
        self.draft = profile;
        self.refresh_ports();
    }

    pub fn refresh_ports(&mut self) {
        self.ports = (self.list_ports)();
        self.draft.device = auto_select(&self.draft.device, &self.ports);
    }

    pub fn set_device(&mut self, device: String) {
        if !self.custom_devices.contains(&device) {
            self.custom_devices.push(device.clone());
        }
        self.draft.device = device;
    }

    pub fn set_baud(&mut self, baud: u32) {
        self.draft.baud = baud;
    }

    pub fn set_key_file(&mut self, path: String) {
        self.draft.key_file = path;
    }

    pub fn set_log_path(&mut self, path: String) {
        self.draft.log_path = path;
    }

    pub fn set_kind(&mut self, kind: Kind) {
        self.draft.set_kind(kind);
        self.page = match kind {
            Kind::Ssh => Page::Ssh,
            Kind::Serial => Page::Serial,
        };
    }

    /// The profile to connect with: the form, named sensibly.
    pub fn collect(&self) -> Profile {
        let mut profile = self.draft.clone();
        if profile.name.trim().is_empty() || profile.name == "New session" {
            profile.name = match profile.kind() {
                Kind::Ssh => profile.host.trim().to_string(),
                Kind::Serial => profile.device.trim().to_string(),
            };
            if profile.name.is_empty() {
                profile.name = "session".into();
            }
        }
        profile.host = profile.host.trim().to_string();
        profile.username = profile.username.trim().to_string();
        profile.key_file = profile.key_file.trim().to_string();
        profile.log_path = profile.log_path.trim().to_string();
        profile.font_family = profile.font_family.trim().to_string();
        profile
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        store: &ProfileStore,
        monospace_fonts: &[String],
        accent: egui::Color32,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        style::section_heading(ui, "New session");

        row(ui, "Type", |ui| {
            let mut kind = self.draft.kind();
            ComboBox::from_id_salt("kind").width(ui.available_width()).truncate().selected_text(kind.label()).show_ui(
                ui,
                |ui| {
                    ui.selectable_value(&mut kind, Kind::Ssh, "SSH");
                    ui.selectable_value(&mut kind, Kind::Serial, "Serial");
                },
            );
            if kind != self.draft.kind() {
                self.set_kind(kind);
            }
        });
        row(ui, "Device", |ui| {
            ComboBox::from_id_salt("device_syntax")
                .width(ui.available_width())
                .truncate()
                .selected_text(syntax_label(&self.draft.device_syntax))
                .show_ui(ui, |ui| {
                    for (value, label) in SYNTAX_LABELS {
                        ui.selectable_value(&mut self.draft.device_syntax, value.to_string(), label);
                    }
                });
        });
        ui.add_space(6.0);

        // Page tabs. Only the page matching the type is enabled, plus
        // Advanced.
        ui.horizontal(|ui| {
            let kind = self.draft.kind();
            for (page, label, enabled) in [
                (Page::Ssh, "SSH", kind == Kind::Ssh),
                (Page::Serial, "Serial", kind == Kind::Serial),
                (Page::Advanced, "Advanced", true),
            ] {
                if style::tab_button(ui, label, self.page == page, enabled, accent).clicked() {
                    self.page = page;
                }
            }
        });

        egui::Frame::new().stroke(egui::Stroke::new(1.0, style::BORDER)).corner_radius(4.0).inner_margin(8.0).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                match self.page {
                    Page::Ssh => self.ssh_page(ui, &mut actions),
                    Page::Serial => self.serial_page(ui, &mut actions),
                    Page::Advanced => self.advanced_page(ui, monospace_fonts, &mut actions),
                }
            },
        );

        ui.add_space(6.0);
        let connect = ui.add_sized([ui.available_width(), 30.0], style::accent_button("Connect", accent));
        if connect.clicked() {
            let profile = self.collect();
            match validate(&profile) {
                Ok(()) => actions.push(Action::Connect(profile)),
                Err(message) => actions.push(Action::Warn(message)),
            }
        }

        ui.add_space(12.0);
        self.saved_ui(ui, store, &mut actions);
        actions
    }

    fn ssh_page(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        row(ui, "Host", |ui| {
            let host = ui.add(text_field(&mut self.draft.host, "hostname or IP", ui.available_width()));
            if std::mem::take(&mut self.focus_host) && self.page == Page::Ssh {
                host.request_focus();
            }
        });
        row(ui, "Port", |ui| ui.add(DragValue::new(&mut self.draft.port).range(1..=65535).speed(0.2)));
        row(ui, "Username", |ui| ui.add(text_field(&mut self.draft.username, "", ui.available_width())));
        let mut auth = self.draft.auth();
        row(ui, "Authentication", |ui| {
            ComboBox::from_id_salt("auth").width(ui.available_width()).truncate().selected_text(auth.label()).show_ui(
                ui,
                |ui| {
                    for option in Auth::ALL {
                        ui.selectable_value(&mut auth, option, option.label());
                    }
                },
            );
        });
        self.draft.auth = auth.as_str().to_string();
        row(ui, "Key file", |ui| {
            ui.add_enabled_ui(auth == Auth::Key, |ui| {
                with_trailing_button(ui, "Browse…", |ui, width| {
                    ui.add(text_field(&mut self.draft.key_file, "~/.ssh/id_ed25519", width));
                })
                .then(|| actions.push(Action::BrowseKey));
            });
        });
    }

    fn serial_page(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        row(ui, "Port", |ui| {
            let refresh = with_trailing_button(ui, "🔄", |ui, width| {
                // Re-scan each time the list is opened, so a console cable
                // plugged in after launch shows up without hitting Refresh.
                let button_id = ui.make_persistent_id("serial_port");
                let open = ComboBox::is_open(ui.ctx(), button_id);
                if open && !self.port_popup_was_open {
                    self.refresh_ports();
                }
                self.port_popup_was_open = open;

                let mut extra: Vec<&str> = self.custom_devices.iter().map(String::as_str).collect();
                extra.push(&self.draft.device);
                let choices = port_choices(&self.ports, &extra);
                let selected = choices.iter().find(|c| c.device == self.draft.device);
                let selected_text = match selected {
                    Some(choice) => RichText::new(&choice.label),
                    None => RichText::new(port_placeholder(&self.ports)).color(style::TEXT_SECONDARY),
                };
                let mut picked: Option<String> = None;
                let mut other = false;
                ComboBox::from_id_salt("serial_port").width(width).truncate().selected_text(selected_text).show_ui(
                    ui,
                    |ui| {
                        for choice in &choices {
                            let r = ui.selectable_label(choice.device == self.draft.device, &choice.label);
                            if r.clicked() {
                                picked = Some(choice.device.clone());
                            }
                            if let Some(port) = self.ports.iter().find(|p| p.device == choice.device)
                                && !port.hwid.is_empty()
                            {
                                r.on_hover_text(&port.hwid);
                            }
                        }
                        if ui.selectable_label(false, "Other…").clicked() {
                            other = true;
                        }
                    },
                );
                if let Some(device) = picked {
                    self.draft.device = device;
                }
                if other {
                    actions.push(Action::OtherDevice);
                }
            });
            if refresh {
                self.refresh_ports();
            }
        });

        row(ui, "Speed", |ui| {
            let mut other_baud = false;
            ComboBox::from_id_salt("baud")
                .width(ui.available_width())
                .truncate()
                .selected_text(self.draft.baud.to_string())
                .show_ui(ui, |ui| {
                    let mut rates: Vec<u32> = BAUD_RATES.to_vec();
                    if !rates.contains(&self.draft.baud) {
                        rates.push(self.draft.baud);
                        rates.sort_unstable();
                    }
                    for rate in rates {
                        ui.selectable_value(&mut self.draft.baud, rate, rate.to_string());
                    }
                    if ui.selectable_label(false, "Other…").clicked() {
                        other_baud = true;
                    }
                });
            if other_baud {
                actions.push(Action::OtherBaud);
            }
        });
        row(ui, "Data bits", |ui| {
            ComboBox::from_id_salt("bytesize")
                .width(ui.available_width())
                .truncate()
                .selected_text(self.draft.bytesize.to_string())
                .show_ui(ui, |ui| {
                    for bits in DATA_BITS {
                        ui.selectable_value(&mut self.draft.bytesize, bits, bits.to_string());
                    }
                });
        });
        row(ui, "Parity", |ui| {
            ComboBox::from_id_salt("parity")
                .width(ui.available_width())
                .truncate()
                .selected_text(self.draft.parity.clone())
                .show_ui(ui, |ui| {
                    for parity in PARITIES {
                        ui.selectable_value(&mut self.draft.parity, parity.to_string(), parity);
                    }
                });
        });
        row(ui, "Stop bits", |ui| {
            ComboBox::from_id_salt("stopbits")
                .width(ui.available_width())
                .truncate()
                .selected_text(format!("{}", self.draft.stopbits))
                .show_ui(ui, |ui| {
                    for stop in STOP_BITS {
                        ui.selectable_value(&mut self.draft.stopbits, stop, format!("{stop}"));
                    }
                });
        });

        // The serial driver takes one flow control mode at a time.
        if ui.checkbox(&mut self.draft.rtscts, "RTS/CTS hardware flow control").changed() && self.draft.rtscts {
            self.draft.xonxoff = false;
        }
        if ui.checkbox(&mut self.draft.xonxoff, "XON/XOFF software flow control").changed() && self.draft.xonxoff {
            self.draft.rtscts = false;
        }
        ui.add(
            egui::Label::new(
                RichText::new("Cisco console default is 9600-8-N-1, no flow control.")
                    .color(style::TEXT_SECONDARY)
                    .small(),
            )
            .wrap(),
        );
    }

    fn advanced_page(&mut self, ui: &mut Ui, monospace_fonts: &[String], actions: &mut Vec<Action>) {
        row(ui, "Scrollback lines", |ui| {
            ui.add(DragValue::new(&mut self.draft.scrollback).range(0..=200_000).speed(100.0))
        });
        row(ui, "Font family", |ui| font_picker(ui, "session_font", &mut self.draft.font_family, monospace_fonts));
        row(ui, "Font size", |ui| ui.add(DragValue::new(&mut self.draft.font_size).range(6..=48)));
        row(ui, "Log session to", |ui| {
            with_trailing_button(ui, "Browse…", |ui, width| {
                ui.add(text_field(&mut self.draft.log_path, "(no session logging)", width));
            })
            .then(|| actions.push(Action::BrowseLog));
        });
    }

    fn saved_ui(&mut self, ui: &mut Ui, store: &ProfileStore, actions: &mut Vec<Action>) {
        style::section_heading(ui, "Saved sessions");
        let button_row = 34.0;
        let list_height = (ui.available_height() - button_row).max(60.0);
        egui::Frame::new()
            .fill(style::BG_LIST)
            .stroke(egui::Stroke::new(1.0, style::BORDER))
            .corner_radius(4.0)
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(list_height - 10.0).auto_shrink([false, false]).show(
                    ui,
                    |ui| {
                        if store.profiles.is_empty() {
                            ui.label(RichText::new("No saved sessions yet").color(style::TEXT_SECONDARY));
                        }
                        for profile in &store.profiles {
                            let selected = self.selected_saved.as_deref() == Some(profile.name.as_str());
                            let response = ui
                                .with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                                    ui.add(
                                        egui::Button::selectable(selected, &profile.name)
                                            .min_size(egui::vec2(0.0, 22.0)),
                                    )
                                })
                                .inner;
                            let response =
                                response.on_hover_text(format!("{} — double-click to connect", profile.kind().label()));
                            if response.clicked() {
                                self.selected_saved = Some(profile.name.clone());
                            }
                            if response.double_clicked() {
                                self.selected_saved = Some(profile.name.clone());
                                self.load(profile.clone());
                                actions.push(Action::Connect(self.collect()));
                            }
                        }
                    },
                );
            });
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 16.0) / 3.0;
            let selected = self.selected_saved.clone().filter(|name| store.get(name).is_some());
            if ui.add_enabled(selected.is_some(), egui::Button::new("Load").min_size([width, 0.0].into())).clicked()
                && let Some(profile) = selected.as_deref().and_then(|n| store.get(n))
            {
                self.load(profile.clone());
            }
            if ui.add(egui::Button::new("Save…").min_size([width, 0.0].into())).clicked() {
                actions.push(Action::Save(self.draft.clone()));
            }
            if ui.add_enabled(selected.is_some(), egui::Button::new("Delete").min_size([width, 0.0].into())).clicked()
                && let Some(name) = selected
            {
                actions.push(Action::Delete(name));
            }
        });
    }
}

/// A single-line text box as tall as the combo boxes next to it.
fn text_field<'t>(text: &'t mut String, hint: &str, width: f32) -> TextEdit<'t> {
    TextEdit::singleline(text)
        .hint_text(hint)
        .desired_width(width)
        .margin(egui::vec2(6.0, 4.0))
        .vertical_align(egui::Align::Center)
}

/// Width of the label column in the forms.
const LABEL_WIDTH: f32 = 96.0;

/// One form row: a fixed-width label, then the widget in the rest.
fn row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let height = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(
            egui::vec2(LABEL_WIDTH, height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(LABEL_WIDTH);
                ui.label(label);
            },
        );
        add(ui)
    })
    .inner
}

/// A widget filling the row with a button after it. Returns whether the
/// button was clicked.
fn with_trailing_button(ui: &mut Ui, button: &str, add: impl FnOnce(&mut Ui, f32)) -> bool {
    let button_width = ui.fonts_mut(|f| {
        f.layout_no_wrap(button.to_string(), egui::TextStyle::Button.resolve(ui.style()), egui::Color32::WHITE).size().x
    }) + ui.spacing().button_padding.x * 2.0;
    let width = (ui.available_width() - button_width - ui.spacing().item_spacing.x).max(40.0);
    // Boxed in, because a truncating combo box measures its text against
    // everything left in the row and would push the button out of the panel.
    let height = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(egui::vec2(width, height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_max_width(width);
        add(ui, width);
    });
    let response = ui.button(button);
    if button == "🔄" { response.on_hover_text("Refresh the port list").clicked() } else { response.clicked() }
}

/// Combo of installed monospace fonts, with "(system monospace)" for the
/// default. A font the profile names that isn't installed stays listed.
pub fn font_picker(ui: &mut Ui, id: &str, family: &mut String, monospace_fonts: &[String]) {
    let label = if family.is_empty() { "(system monospace)".to_string() } else { family.clone() };
    ComboBox::from_id_salt(id).width(ui.available_width()).truncate().selected_text(label).height(300.0).show_ui(
        ui,
        |ui| {
            ui.selectable_value(family, String::new(), "(system monospace)");
            if !family.is_empty() && !monospace_fonts.contains(family) {
                let current = family.clone();
                ui.selectable_value(family, current.clone(), format!("{current}  (not installed)"));
            }
            for name in monospace_fonts {
                ui.selectable_value(family, name.clone(), name);
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(device: &str, serial: &str) -> PortInfo {
        PortInfo {
            device: device.into(),
            description: "USB Serial Port".into(),
            serial_number: serial.into(),
            ..PortInfo::default()
        }
    }

    fn cables() -> Vec<PortInfo> {
        vec![port("COM3", "A10K3X"), port("COM10", "B77Q2Z")]
    }

    #[test]
    fn single_cable_is_picked_automatically() {
        assert_eq!(auto_select("", &[port("COM3", "")]), "COM3");
    }

    #[test]
    fn several_cables_require_a_choice() {
        assert_eq!(auto_select("", &cables()), "");
        assert_eq!(port_placeholder(&cables()), "Choose a port (2 detected)");
        let profile = Profile { kind: "serial".into(), ..Profile::default() };
        assert_eq!(validate(&profile).unwrap_err(), "Choose a serial port from the Port list.");
    }

    #[test]
    fn a_chosen_cable_is_kept() {
        assert_eq!(auto_select("COM10", &cables()), "COM10");
        assert_eq!(auto_select("COM10", &[port("COM3", "")]), "COM10");
    }

    #[test]
    fn unplugged_saved_port_stays_listed() {
        let choices = port_choices(&cables(), &["COM7", "COM3", ""]);
        let labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            ["COM3 — USB Serial Port  [SN A10K3X]", "COM10 — USB Serial Port  [SN B77Q2Z]", "COM7  (not detected)"]
        );
    }

    #[test]
    fn no_ports() {
        assert_eq!(port_placeholder(&[]), "No serial ports detected");
        assert!(port_choices(&[], &[]).is_empty());
    }

    fn two_cables() -> Vec<PortInfo> {
        cables()
    }

    #[test]
    fn form_resets_and_names_sessions() {
        let settings = AppSettings { font_size: 14, ..AppSettings::default() };
        let mut sidebar = Sidebar::with_port_lister(&settings, two_cables);
        assert_eq!(sidebar.draft.font_size, 14);
        sidebar.draft.host = " 10.0.0.1 ".into();
        assert_eq!(sidebar.collect().name, "10.0.0.1");

        sidebar.set_kind(Kind::Serial);
        assert_eq!(sidebar.draft.device, "");
        sidebar.set_device("/dev/ttyUSB7".into());
        assert_eq!(sidebar.collect().name, "/dev/ttyUSB7");

        sidebar.load(Profile {
            name: "core".into(),
            kind: "serial".into(),
            device: "COM10".into(),
            ..Profile::default()
        });
        assert_eq!(sidebar.collect().name, "core");
        assert_eq!(sidebar.draft.device, "COM10");
    }

    #[test]
    fn ssh_needs_a_host() {
        assert_eq!(validate(&Profile::default()).unwrap_err(), "Enter a host to connect to.");
        assert!(validate(&Profile { host: "r1".into(), ..Profile::default() }).is_ok());
    }
}
