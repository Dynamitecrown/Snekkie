//! Left-hand panel: the new-session form and saved sessions.
//!
//! Always visible rather than a popup, so starting a second session never
//! means closing what you're looking at first.

use egui::{ComboBox, DragValue, RichText, TextEdit, Ui};

use super::style;
use crate::profiles::{Auth, Kind, LocalEcho, Profile, ProfileStore};
use crate::settings::AppSettings;
use crate::terminal::highlight::{syntax_label, syntax_options};
use crate::terminal::keys::Backspace;
use crate::transport::serial::{self, BAUD_RATES, DATA_BITS, PARITIES, PortInfo, STOP_BITS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    /// Host and port, plus login details for SSH.
    Network,
    Serial,
    Advanced,
}

impl Page {
    /// The page with a session type's own settings.
    fn for_kind(kind: Kind) -> Page {
        if kind == Kind::Serial { Page::Serial } else { Page::Network }
    }
}

/// What the sidebar asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Connect(Profile),
    /// Ask for a name, then save this profile under it.
    Save(Profile),
    /// Ask, then delete the saved session with this name.
    Delete(String),
    ToggleFavorite(String),
    ProfileColor(String),
    ImportProfiles,
    ExportProfiles,
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
        Kind::Serial if profile.device.trim().is_empty() => Err("Choose a serial port from the Port list.".into()),
        kind if kind.is_network() && profile.host.trim().is_empty() => Err("Enter a host to connect to.".into()),
        _ => Ok(()),
    }
}

pub struct Sidebar {
    /// The form's contents.
    pub draft: Profile,
    /// The destination that the form's saved name belongs to. Editing a
    /// host, protocol, network port or serial device starts a new name.
    name_destination: Option<(Kind, String, u16)>,
    page: Page,
    ports: Vec<PortInfo>,
    /// Devices typed in via "Other…" this run.
    custom_devices: Vec<String>,
    port_popup_was_open: bool,
    selected_saved: Option<String>,
    saved_filter: String,
    favorites_only: bool,
    blocks_terminal_input: bool,
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
            name_destination: None,
            page: Page::Network,
            ports: Vec::new(),
            custom_devices: Vec::new(),
            port_popup_was_open: false,
            selected_saved: None,
            saved_filter: String::new(),
            favorites_only: false,
            blocks_terminal_input: false,
            focus_host: false,
            list_ports,
        };
        sidebar.reset(settings);
        if !sidebar.ports.is_empty() {
            sidebar.set_kind(Kind::Serial);
        }
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
        self.selected_saved = None;
        self.focus_host = true;
    }

    /// Fill the form from a profile.
    pub fn load(&mut self, profile: Profile) {
        self.page = Page::for_kind(profile.kind());
        self.draft = profile;
        self.refresh_ports();
        self.name_destination = Some(destination(&self.draft));
    }

    pub fn set_name(&mut self, name: String) {
        self.draft.name = name;
        self.name_destination = Some(destination(&self.draft));
    }

    pub fn selected_saved(&self) -> Option<&str> {
        self.selected_saved.as_deref()
    }

    /// Also includes a saved-row menu's dismissal frame, when focus may
    /// already have returned to a terminal.
    pub fn blocks_terminal_input(&self) -> bool {
        self.blocks_terminal_input
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
        // Follow the protocol's usual port, unless another one was typed in.
        let usual = Kind::ALL.iter().any(|k| k.default_port() == Some(self.draft.port));
        if usual && let Some(port) = kind.default_port() {
            self.draft.port = port;
        }
        self.draft.set_kind(kind);
        self.page = Page::for_kind(kind);
    }

    /// The profile to connect with: the form, named sensibly.
    pub fn collect(&self) -> Profile {
        let mut profile = self.draft.clone();
        if profile.name.trim().is_empty()
            || profile.name == "New session"
            || self.name_destination.as_ref() != Some(&destination(&profile))
        {
            let kind = profile.kind();
            let host = profile.host.trim();
            profile.name = match kind {
                Kind::Serial => profile.device.trim().to_string(),
                // A console server is one host with a port per line, so
                // the port is what tells its sessions apart.
                _ if host.is_empty() || kind.default_port() == Some(profile.port) => host.to_string(),
                _ => format!("{host}:{}", profile.port),
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
        self.blocks_terminal_input = false;
        let width = ui.available_width();
        egui::ScrollArea::vertical()
            .id_salt("sidebar_contents")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(width.min(ui.available_width()));
                self.contents_ui(ui, store, monospace_fonts, accent)
            })
            .inner
    }

    fn contents_ui(
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
                    for option in Kind::ALL {
                        ui.selectable_value(&mut kind, option, option.label());
                    }
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
                    for (value, label) in syntax_options() {
                        ui.selectable_value(&mut self.draft.device_syntax, value.to_string(), label);
                    }
                });
        });
        ui.add_space(6.0);

        // Page tabs: the type's own settings, then Advanced.
        ui.horizontal(|ui| {
            let kind = self.draft.kind();
            for (page, label) in [(Page::for_kind(kind), kind.label()), (Page::Advanced, "Advanced")] {
                if style::tab_button(ui, label, self.page == page, true, accent).clicked() {
                    self.page = page;
                }
            }
        });

        egui::Frame::new().stroke(egui::Stroke::new(1.0, style::border(ui))).corner_radius(4.0).inner_margin(8.0).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                match self.page {
                    Page::Network => self.network_page(ui, &mut actions),
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

    fn network_page(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        row(ui, "Host", |ui| {
            let host = ui.add(text_field(&mut self.draft.host, "hostname or IP", ui.available_width()));
            if std::mem::take(&mut self.focus_host) && self.page == Page::Network {
                host.request_focus();
            }
        });
        row(ui, "Port", |ui| ui.add(DragValue::new(&mut self.draft.port).range(1..=65535).speed(0.2)));
        if self.draft.kind() == Kind::Ssh {
            self.ssh_login(ui, actions);
        }
        row(ui, "Keepalive", |ui| {
            ui.add(
                DragValue::new(&mut self.draft.keepalive)
                    .range(0..=3600)
                    .speed(1.0)
                    .custom_formatter(|v, _| if v == 0.0 { "off".into() } else { format!("{v} s") })
                    .custom_parser(|text| {
                        let text = text.trim();
                        if text.eq_ignore_ascii_case("off") {
                            return Some(0.0);
                        }
                        text.trim_end_matches('s').trim().parse().ok()
                    }),
            )
            .on_hover_text(
                "Seconds between keepalives, which stop a firewall dropping an idle session. 0 turns them off.",
            )
        });
    }

    fn ssh_login(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
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
                    None => RichText::new(port_placeholder(&self.ports)).color(style::secondary(ui)),
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
                    .color(style::secondary(ui))
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
        egui::CollapsingHeader::new("Logging options").show(ui, |ui| {
        if ui.button("Automatic log filename").clicked() {
            self.draft.log_path =
                crate::config::config_dir().join("logs").join("{host}-{date}-{session}.log").display().to_string();
        }
        row(ui, "Log format", |ui| {
            egui::ComboBox::from_id_salt("log_format")
                .width(ui.available_width().floor())
                .truncate()
                .selected_text(if self.draft.log_format == "text" {
                    "Text (input + output)"
                } else {
                    "Raw + input log"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.draft.log_format, "text".into(), "Text log (input + output)");
                    ui.selectable_value(&mut self.draft.log_format, "raw".into(), "Raw output + input log");
                })
                .response
        });
        ui.checkbox(&mut self.draft.log_passwords, "Log passwords")
            .on_hover_text("Off by default. Recognized password prompts and credential commands are redacted when off. Private input always suppresses logging.");
        if self.draft.log_passwords {
            ui.add(egui::Label::new(egui::RichText::new("Passwords will be stored locally in plain text.").color(ui.visuals().warn_fg_color)).wrap());
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Rotate log at");
            ui.add(DragValue::new(&mut self.draft.log_rotate_mb).range(0..=1024).suffix(" MiB"));
            ui.checkbox(&mut self.draft.log_rotate_daily, "Rotate daily");
        });
        });
        row(ui, "Local echo", |ui| {
            let mut echo = self.draft.local_echo();
            ComboBox::from_id_salt("local_echo")
                .width(ui.available_width())
                .truncate()
                .selected_text(echo.label())
                .show_ui(ui, |ui| {
                    for option in LocalEcho::ALL {
                        ui.selectable_value(&mut echo, option, option.label());
                    }
                })
                .response
                .on_hover_text(
                    "Show what you type, for devices that don't echo it back. Auto: on for telnet until \
                     the device says it will echo; off for SSH, serial and raw TCP.",
                );
            self.draft.local_echo = echo.as_str().to_string();
        });
        row(ui, "Backspace", |ui| {
            let mut backspace = self.draft.backspace();
            ComboBox::from_id_salt("backspace")
                .width(ui.available_width())
                .truncate()
                .selected_text(backspace.label())
                .show_ui(ui, |ui| {
                    for option in Backspace::ALL {
                        ui.selectable_value(&mut backspace, option, option.label());
                    }
                })
                .response
                .on_hover_text(
                    "What the Backspace key sends. Try Ctrl+H if Backspace doesn't erase on an older device.",
                );
            self.draft.backspace = backspace.as_str().to_string();
        });
    }

    fn saved_ui(&mut self, ui: &mut Ui, store: &ProfileStore, actions: &mut Vec<Action>) {
        // Favorites sits in the heading row to leave the list as much room as
        // possible in the default window.
        let favorites = style::section_heading_with(ui, "Saved sessions", |ui| {
            ui.checkbox(&mut self.favorites_only, "Favorites only")
        });
        self.blocks_terminal_input |= favorites.has_focus() || favorites.lost_focus();
        ui.horizontal(|ui| {
            let width = (ui.available_width() - button_width(ui, "Clear") - ui.spacing().item_spacing.x).max(40.0);
            let filter = ui
                .allocate_ui_with_layout(
                    egui::vec2(width, ui.spacing().interact_size.y),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_max_width(width);
                        ui.add(text_field(&mut self.saved_filter, "Filter saved sessions", width))
                    },
                )
                .inner;
            filter.widget_info(|| {
                let mut info = egui::WidgetInfo::text_edit(
                    ui.is_enabled(),
                    &self.saved_filter,
                    &self.saved_filter,
                    "Filter saved sessions",
                );
                info.label = Some("Filter saved sessions".into());
                info
            });
            self.blocks_terminal_input |= filter.has_focus() || filter.lost_focus();
            let clear = ui.add_enabled(!self.saved_filter.is_empty(), egui::Button::new("Clear"));
            clear.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, clear.enabled(), "Clear session filter")
            });
            if clear.on_hover_text("Clear session filter").clicked() {
                self.saved_filter.clear();
                filter.request_focus();
                self.blocks_terminal_input = true;
            }
        });
        let query = self.saved_filter.trim().to_lowercase();
        let visible: Vec<&Profile> =
            store.profiles.iter().filter(|p| matches_saved_filter(p, &query, self.favorites_only)).collect();
        let button_row = 34.0;
        let list_height = (ui.available_height() - button_row).max(60.0);
        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, style::border(ui)))
            .corner_radius(4.0)
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(list_height - 10.0).auto_shrink([false, false]).show(
                    ui,
                    |ui| {
                        if store.profiles.is_empty() {
                            ui.label(RichText::new("No saved sessions yet").color(style::secondary(ui)));
                        } else if visible.is_empty() {
                            ui.label(RichText::new("No matching sessions").color(style::secondary(ui)));
                        }
                        for profile in visible {
                            ui.push_id(("saved_profile", &profile.name), |ui| {
                                let selected = self.selected_saved.as_deref() == Some(profile.name.as_str());
                                let response = ui
                                    .horizontal(|ui| {
                                        let label = format!(
                                            "{} {}",
                                            if profile.favorite { "Unfavorite" } else { "Favorite" },
                                            profile.name
                                        );
                                        let star = ui.add_sized(
                                            [24.0, 22.0],
                                            egui::Button::new(if profile.favorite { "★" } else { "☆" }),
                                        );
                                        star.widget_info(|| {
                                            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), &label)
                                        });
                                        self.blocks_terminal_input |= star.has_focus() || star.lost_focus();
                                        if star.on_hover_text(label).clicked() {
                                            actions.push(Action::ToggleFavorite(profile.name.clone()));
                                        }
                                        ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                                            ui.add(
                                                egui::Button::selectable(selected, &profile.name)
                                                    .truncate()
                                                    .min_size(egui::vec2(0.0, 22.0)),
                                            )
                                        })
                                        .inner
                                    })
                                    .inner;
                                // The same strip open tabs show for the profile's default color.
                                if let Some(color) = crate::settings::parse_hex(&profile.tab_color) {
                                    let rect = response.rect;
                                    let strip = egui::Rect::from_x_y_ranges(
                                        rect.left()..=rect.left() + 3.0,
                                        rect.top() + 2.0..=rect.bottom() - 2.0,
                                    );
                                    ui.painter().rect_filled(strip, 1.0, color);
                                }
                                let response = response
                                    .on_hover_text(format!("{} — double-click to connect", profile.kind().label()));
                                let menu_was_open =
                                    egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
                                let menu = response.context_menu(|ui| {
                                    if ui.button("Default tab color…").clicked() {
                                        actions.push(Action::ProfileColor(profile.name.clone()));
                                        ui.close();
                                    }
                                });
                                self.blocks_terminal_input |=
                                    menu_was_open || menu.is_some() || response.has_focus() || response.lost_focus();
                                if response.clicked() {
                                    self.selected_saved = Some(profile.name.clone());
                                }
                                if response.double_clicked() {
                                    self.selected_saved = Some(profile.name.clone());
                                    self.load((*profile).clone());
                                    actions.push(Action::Connect(self.collect()));
                                }
                            });
                        }
                    },
                );
            });
        ui.horizontal(|ui| {
            let more_width = button_width(ui, MORE);
            // Rounded down: even a fraction too wide would grow the resizable panel.
            let width = ((ui.available_width() - more_width - ui.spacing().item_spacing.x * 3.0) / 3.0).floor();
            let selected = self
                .selected_saved
                .clone()
                .filter(|name| store.get(name).is_some_and(|p| matches_saved_filter(p, &query, self.favorites_only)));
            if ui.add_enabled(selected.is_some(), egui::Button::new("Load").min_size([width, 0.0].into())).clicked()
                && let Some(profile) = selected.as_deref().and_then(|n| store.get(n))
            {
                self.load(profile.clone());
            }
            if ui.add(egui::Button::new("Save…").min_size([width, 0.0].into())).clicked() {
                actions.push(Action::Save(self.collect()));
            }
            if ui.add_enabled(selected.is_some(), egui::Button::new("Delete").min_size([width, 0.0].into())).clicked()
                && let Some(name) = selected
            {
                actions.push(Action::Delete(name));
            }
            // Import/export are also under Settings; here they share a menu so
            // the list keeps its height.
            let (more, _) = egui::containers::menu::MenuButton::new(MORE).ui(ui, |ui| {
                if ui.button("Import profiles…").clicked() {
                    actions.push(Action::ImportProfiles);
                }
                if ui.add_enabled(!store.profiles.is_empty(), egui::Button::new("Export profiles…")).clicked() {
                    actions.push(Action::ExportProfiles);
                }
            });
            more.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "More profile actions"));
            let menu_open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&more));
            self.blocks_terminal_input |= menu_open || more.has_focus() || more.lost_focus();
            more.on_hover_text("Import or export profiles");
        });
    }
}

fn matches_saved_filter(profile: &Profile, query: &str, favorites_only: bool) -> bool {
    (!favorites_only || profile.favorite)
        && (query.is_empty()
            || [&profile.name, &profile.host, &profile.device, &profile.kind]
                .into_iter()
                .any(|value| value.to_lowercase().contains(query))
            || profile.kind().label().to_lowercase().contains(query))
}

fn destination(profile: &Profile) -> (Kind, String, u16) {
    let kind = profile.kind();
    if kind == Kind::Serial {
        (kind, profile.device.trim().to_string(), 0)
    } else {
        (kind, profile.host.trim().to_string(), profile.port)
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

/// Label of the saved-session menu with import and export.
const MORE: &str = "…";

/// Width of a plain text button, for sizing the rest of its row.
fn button_width(ui: &Ui, text: &str) -> f32 {
    ui.fonts_mut(|f| {
        f.layout_no_wrap(text.to_string(), egui::TextStyle::Button.resolve(ui.style()), egui::Color32::WHITE).size().x
    }) + ui.spacing().button_padding.x * 2.0
}

/// A widget filling the row with a button after it. Returns whether the
/// button was clicked.
fn with_trailing_button(ui: &mut Ui, button: &str, add: impl FnOnce(&mut Ui, f32)) -> bool {
    let width = (ui.available_width() - button_width(ui, button) - ui.spacing().item_spacing.x).max(40.0);
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
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;

    struct SavedSessionsTest {
        sidebar: Sidebar,
        store: ProfileStore,
        actions: Vec<Action>,
        _dir: tempfile::TempDir,
    }

    fn saved_sessions_harness() -> Harness<'static, SavedSessionsTest> {
        saved_sessions_harness_sized([300.0, 640.0])
    }

    fn saved_sessions_harness_sized(size: [f32; 2]) -> Harness<'static, SavedSessionsTest> {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore {
            path: dir.path().join("sessions.json"),
            profiles: vec![
                Profile { name: "Alpha".into(), host: "192.0.2.10".into(), ..Default::default() },
                Profile {
                    name: "Beta".into(),
                    kind: "serial".into(),
                    device: "COM10".into(),
                    favorite: true,
                    ..Default::default()
                },
            ],
        };
        let state = SavedSessionsTest {
            sidebar: Sidebar::with_port_lister(&AppSettings::default(), Vec::new),
            store,
            actions: Vec::new(),
            _dir: dir,
        };
        let mut harness = Harness::builder().with_size(size).build_ui_state(
            |ui, state: &mut SavedSessionsTest| {
                let actions = state.sidebar.ui(ui, &state.store, &[], egui::Color32::LIGHT_BLUE);
                state.actions.extend(actions);
            },
            state,
        );
        harness.run_ok();
        harness
    }

    #[test]
    fn saved_filter_searches_destinations_and_protocol_labels() {
        let profile = Profile {
            name: "Core Café".into(),
            host: "Router.EXAMPLE".into(),
            kind: "raw".into(),
            device: "COM10".into(),
            ..Default::default()
        };
        for query in ["café", "router.example", "com10", "raw", "tcp", ""] {
            assert!(matches_saved_filter(&profile, query, false), "{query}");
        }
        assert!(!matches_saved_filter(&profile, "other", false));
        assert!(!matches_saved_filter(&profile, "", true));
        assert!(matches_saved_filter(&Profile { favorite: true, ..profile }, "tcp", true));
    }

    #[test]
    fn filtering_hides_selected_actions_without_changing_the_destination() {
        let mut harness = saved_sessions_harness();
        harness.get_by_label("Alpha").click();
        harness.run_ok();
        assert_eq!(harness.state().sidebar.selected_saved(), Some("Alpha"));
        harness.get_by_label("Filter saved sessions").click();
        harness.event(egui::Event::Text("com10".into()));
        harness.run_ok();
        assert!(harness.query_by_label("Alpha").is_none());
        harness.get_by_label("Beta");
        assert!(harness.get_by_label("Load").accesskit_node().is_disabled());
        assert!(harness.get_by_label("Delete").accesskit_node().is_disabled());
        assert_eq!(harness.state().sidebar.selected_saved(), Some("Alpha"));
        assert!(harness.state().sidebar.blocks_terminal_input());
        assert!(harness.state().actions.is_empty());
        harness.get_by_label("Clear session filter").click();
        harness.run_ok();
        assert!(!harness.get_by_label("Load").accesskit_node().is_disabled());
        harness.get_by_label("Load").click();
        harness.run_ok();
        assert_eq!(harness.state().sidebar.draft.host, "192.0.2.10");
        assert_eq!(harness.state().sidebar.collect().name, "Alpha");
    }

    #[test]
    fn favorites_filter_and_star_target_the_named_profile_without_connecting() {
        let mut harness = saved_sessions_harness();
        harness.get_by_label("Alpha").click();
        harness.run_ok();
        harness.get_by_label("Favorites only").click();
        harness.run_ok();
        assert!(harness.query_by_label("Alpha").is_none());
        assert!(harness.get_by_label("Load").accesskit_node().is_disabled());
        harness.get_by_label("Unfavorite Beta").click();
        harness.run_ok();
        assert_eq!(harness.state().actions, [Action::ToggleFavorite("Beta".into())]);
        assert_eq!(harness.state().sidebar.selected_saved(), Some("Alpha"));
        assert_eq!(harness.state().sidebar.draft.host, "");
        harness.get_by_label("Favorites only").click();
        harness.run_ok();
        harness.get_by_label("Favorite Alpha");
        assert!(!harness.get_by_label("Load").accesskit_node().is_disabled());
    }

    #[test]
    fn saved_profile_context_color_targets_its_row() {
        let mut harness = saved_sessions_harness();
        harness.get_by_label("Alpha").click();
        harness.run_ok();
        harness.get_by_label("Beta").click_button_modifiers(egui::PointerButton::Secondary, egui::Modifiers::NONE);
        harness.run_ok();
        assert!(harness.state().sidebar.blocks_terminal_input());
        harness.get_by_label("Default tab color…").click();
        harness.run_ok();
        assert_eq!(harness.state().actions, [Action::ProfileColor("Beta".into())]);
        assert_eq!(harness.state().sidebar.selected_saved(), Some("Alpha"));
    }

    #[test]
    fn profile_actions_remain_reachable_in_a_short_sidebar() {
        let mut harness = saved_sessions_harness_sized([280.0, 320.0]);
        harness.get_by_label("More profile actions").scroll_to_me();
        harness.run_ok();
        let more = harness.get_by_label("More profile actions");
        assert!(more.rect().bottom() <= 320.0, "{:?}", more.rect());
        more.click();
        harness.run_ok();
        assert!(harness.state().sidebar.blocks_terminal_input());
        harness.get_by_label("Export profiles…").click();
        harness.run_ok();
        assert_eq!(harness.state().actions, [Action::ExportProfiles]);
        assert!(harness.query_by_label("Export profiles…").is_none(), "the menu closes after choosing");

        harness.get_by_label("More profile actions").click();
        harness.run_ok();
        harness.get_by_label("Import profiles…").click();
        harness.run_ok();
        assert_eq!(harness.state().actions, [Action::ExportProfiles, Action::ImportProfiles]);
    }

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
        sidebar.set_kind(Kind::Ssh);
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
    fn network_sessions_need_a_host() {
        for kind in [Kind::Ssh, Kind::Telnet, Kind::Raw] {
            let profile = Profile { kind: kind.as_str().into(), ..Profile::default() };
            assert_eq!(validate(&profile).unwrap_err(), "Enter a host to connect to.");
            assert!(validate(&Profile { host: "r1".into(), ..profile }).is_ok());
        }
    }

    #[test]
    fn port_follows_the_protocol_unless_typed_in() {
        let mut sidebar = Sidebar::with_port_lister(&AppSettings::default(), two_cables);
        assert_eq!(sidebar.draft.port, 22);
        sidebar.set_kind(Kind::Telnet);
        assert_eq!(sidebar.draft.port, 23);
        sidebar.set_kind(Kind::Ssh);
        assert_eq!(sidebar.draft.port, 22);
        // Raw TCP has no usual port, so it keeps what's there...
        sidebar.set_kind(Kind::Raw);
        assert_eq!(sidebar.draft.port, 22);
        // ...and a console server's line port survives switching to telnet.
        sidebar.draft.port = 2003;
        sidebar.set_kind(Kind::Telnet);
        assert_eq!(sidebar.draft.port, 2003);
    }

    #[test]
    fn console_server_sessions_are_named_by_port() {
        let mut sidebar = Sidebar::with_port_lister(&AppSettings::default(), two_cables);
        sidebar.set_kind(Kind::Telnet);
        sidebar.draft.host = "cs1".into();
        assert_eq!(sidebar.collect().name, "cs1");
        sidebar.draft.port = 2003;
        assert_eq!(sidebar.collect().name, "cs1:2003");
        sidebar.set_kind(Kind::Raw);
        assert_eq!(sidebar.collect().name, "cs1:2003");
    }

    #[test]
    fn changing_a_loaded_destination_does_not_reuse_its_saved_name() {
        let mut sidebar = Sidebar::with_port_lister(&AppSettings::default(), two_cables);
        sidebar.load(Profile { name: "core".into(), host: "10.0.0.1".into(), ..Default::default() });
        sidebar.draft.font_size = 18;
        sidebar.draft.username = "admin".into();
        assert_eq!(sidebar.collect().name, "core", "settings changes should keep the saved alias");
        sidebar.draft.host = " 10.0.0.2 ".into();
        assert_eq!(sidebar.collect().name, "10.0.0.2");
        sidebar.draft.host = "10.0.0.1".into();
        assert_eq!(sidebar.collect().name, "core", "returning to the saved destination restores its name");
        sidebar.draft.port = 2222;
        assert_eq!(sidebar.collect().name, "10.0.0.1:2222");
        sidebar.set_kind(Kind::Telnet);
        sidebar.draft.port = 23;
        assert_eq!(sidebar.collect().name, "10.0.0.1");

        sidebar.load(Profile {
            name: "console".into(),
            kind: "serial".into(),
            device: "COM3".into(),
            ..Default::default()
        });
        sidebar.draft.baud = 115200;
        assert_eq!(sidebar.collect().name, "console");
        sidebar.set_device("COM10".into());
        assert_eq!(sidebar.collect().name, "COM10");
    }
}
