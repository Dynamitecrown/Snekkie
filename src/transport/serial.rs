//! Serial transport backed by the serialport crate.

use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serialport::{DataBits, FlowControl, Parity, SerialPort, SerialPortType, StopBits};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;

use super::{Command, Link, Sink};
use crate::profiles::Profile;

/// How long one read waits for data. Reads and writes share one thread (on
/// Windows a blocking read on the handle would otherwise hold up writes),
/// so this is also the worst-case delay before a keystroke goes out.
const READ_TIMEOUT: Duration = Duration::from_millis(10);

/// How long a break holds the line low. Cisco's recovery window accepts
/// anything from a fraction of a second up.
const BREAK_DURATION: Duration = Duration::from_millis(300);

pub const BAUD_RATES: [u32; 9] = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400];
pub const PARITIES: [&str; 3] = ["None", "Even", "Odd"];
pub const DATA_BITS: [u8; 4] = [5, 6, 7, 8];
pub const STOP_BITS: [f32; 2] = [1.0, 2.0];

/// One detected serial port, labelled for the port picker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortInfo {
    pub device: String,
    pub description: String,
    pub serial_number: String,
    pub location: String,
    pub hwid: String,
}

impl PortInfo {
    /// e.g. "COM4 — USB Serial Port  [SN A10K3X]".
    ///
    /// Two identical USB console cables share a description, so the serial
    /// number (or, failing that, the USB location) is what tells them apart.
    pub fn label(&self) -> String {
        // Windows already appends "(COM4)" to the description; don't repeat it.
        let mut desc = self.description.as_str();
        let suffix = format!("({})", self.device);
        if let Some(stripped) = desc.strip_suffix(&suffix) {
            desc = stripped.trim_end();
        }
        let mut text = if desc.is_empty() || desc == "n/a" || desc == self.device {
            self.device.clone()
        } else {
            format!("{} — {}", self.device, desc)
        };
        if !self.serial_number.is_empty() {
            text.push_str(&format!("  [SN {}]", self.serial_number));
        } else if !self.location.is_empty() {
            text.push_str(&format!("  [USB {}]", self.location));
        }
        text
    }
}

/// Sort COM2 before COM10 and ttyUSB2 before ttyUSB10.
fn natural_key(device: &str) -> Vec<(String, u64)> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut digits = String::new();
    for ch in device.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            if !digits.is_empty() {
                parts.push((std::mem::take(&mut text), digits.parse().unwrap_or(u64::MAX)));
                digits.clear();
            }
            text.extend(ch.to_lowercase());
        }
    }
    parts.push((text, digits.parse().unwrap_or(0)));
    parts
}

fn port_info(port: serialport::SerialPortInfo) -> PortInfo {
    let mut info = PortInfo { device: port.port_name.clone(), ..PortInfo::default() };
    match port.port_type {
        SerialPortType::UsbPort(usb) => {
            info.description = usb.product.or(usb.manufacturer).unwrap_or_default();
            info.serial_number = usb.serial_number.unwrap_or_default();
            info.location = usb.location.map(|l| l.to_string()).unwrap_or_default();
            info.hwid = format!("USB VID:PID={:04X}:{:04X}", usb.vid, usb.pid);
            if !info.serial_number.is_empty() {
                info.hwid.push_str(&format!(" SER={}", info.serial_number));
            }
            if !info.location.is_empty() {
                info.hwid.push_str(&format!(" LOCATION={}", info.location));
            }
        }
        SerialPortType::PciPort => info.description = "PCI serial port".into(),
        SerialPortType::BluetoothPort => info.description = "Bluetooth serial port".into(),
        SerialPortType::Unknown => {}
    }
    info
}

/// Currently attached serial ports, in natural device order.
pub fn list_ports() -> Vec<PortInfo> {
    let mut ports: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        // macOS lists every port twice, as /dev/tty.* and /dev/cu.*; the
        // cu.* one is the right one to open from a terminal.
        .filter(|p| !p.port_name.starts_with("/dev/tty."))
        .map(port_info)
        .collect();
    ports.sort_by_key(|p| natural_key(&p.device));
    ports.dedup_by(|a, b| a.device == b.device);
    ports
}

fn settings(profile: &Profile) -> Result<(DataBits, Parity, StopBits, FlowControl), String> {
    let data_bits = match profile.bytesize {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    };
    let parity = match profile.parity.as_str() {
        "None" => Parity::None,
        "Even" => Parity::Even,
        "Odd" => Parity::Odd,
        other => return Err(format!("{other} parity isn't supported; use None, Even or Odd.")),
    };
    let stop_bits = if profile.stopbits == 2.0 {
        StopBits::Two
    } else if profile.stopbits == 1.0 {
        StopBits::One
    } else {
        return Err(format!("{} stop bits isn't supported; use 1 or 2.", profile.stopbits));
    };
    // The serial driver takes one flow control mode. Hardware wins if a
    // profile from the Python release asked for both.
    let flow = if profile.rtscts {
        FlowControl::Hardware
    } else if profile.xonxoff {
        FlowControl::Software
    } else {
        FlowControl::None
    };
    Ok((data_bits, parity, stop_bits, flow))
}

pub fn open_port(profile: &Profile) -> Result<Box<dyn SerialPort>, String> {
    let (data_bits, parity, stop_bits, flow) = settings(profile)?;
    serialport::new(&profile.device, profile.baud)
        .data_bits(data_bits)
        .parity(parity)
        .stop_bits(stop_bits)
        .flow_control(flow)
        .timeout(READ_TIMEOUT)
        .open()
        .map_err(|e| format!("Could not open {}: {e}", profile.device))
}

pub fn description(profile: &Profile) -> String {
    let stop = if profile.stopbits.fract() == 0.0 {
        format!("{}", profile.stopbits as u32)
    } else {
        format!("{}", profile.stopbits)
    };
    let parity = profile.parity.chars().next().unwrap_or('N').to_ascii_uppercase();
    format!("Serial  {}  {}-{}{}{}", profile.device, profile.baud, profile.bytesize, parity, stop)
}

/// Open the port and start the worker thread. Opening happens on the worker
/// too, so a slow USB driver never freezes the window.
pub fn start(profile: Profile, sink: Arc<dyn Sink>) -> Link {
    let (link, commands) = Link::new();
    let spawned = thread::Builder::new().name(format!("serial {}", profile.device)).spawn({
        let sink = sink.clone();
        move || match open_port(&profile) {
            Ok(port) => {
                sink.connected();
                let reason = run(port, commands, sink.as_ref());
                sink.closed(reason);
            }
            Err(message) => sink.closed(Some(message)),
        }
    });
    if let Err(e) = spawned {
        sink.closed(Some(format!("Could not start the serial worker: {e}")));
    }
    link
}

/// Pump the port until it closes. Returns why it ended (None = on purpose).
fn run(mut port: Box<dyn SerialPort>, mut commands: UnboundedReceiver<Command>, sink: &dyn Sink) -> Option<String> {
    let mut buf = vec![0u8; 8192];
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Write(bytes)) => {
                    if let Err(e) = port.write_all(&bytes).and_then(|_| port.flush()) {
                        return Some(format!("Write failed: {e}"));
                    }
                }
                Ok(Command::Break) => {
                    let result = port.set_break().and_then(|_| {
                        thread::sleep(BREAK_DURATION);
                        port.clear_break()
                    });
                    match result {
                        Ok(()) => sink.notice("Break sent".into()),
                        Err(e) => sink.notice(format!("Break failed: {e}")),
                    }
                }
                Ok(Command::Resize { .. }) => {}
                Ok(Command::Close) | Err(TryRecvError::Disconnected) => return None,
                Err(TryRecvError::Empty) => break,
            }
        }
        match port.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => sink.data(&buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            Err(e) => return Some(format!("Serial port error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb(device: &str, description: &str, serial: &str, location: &str) -> PortInfo {
        PortInfo {
            device: device.into(),
            description: description.into(),
            serial_number: serial.into(),
            location: location.into(),
            hwid: String::new(),
        }
    }

    #[test]
    fn label_strips_windows_suffix_and_shows_serial_number() {
        assert_eq!(usb("COM3", "USB Serial Port (COM3)", "A10K3X", "").label(), "COM3 — USB Serial Port  [SN A10K3X]");
    }

    #[test]
    fn label_falls_back_to_usb_location_then_bare_device() {
        assert_eq!(usb("/dev/ttyUSB0", "FT232R", "", "1-1.2").label(), "/dev/ttyUSB0 — FT232R  [USB 1-1.2]");
        assert_eq!(usb("/dev/ttyS0", "n/a", "", "").label(), "/dev/ttyS0");
        assert_eq!(usb("COM1", "", "", "").label(), "COM1");
    }

    #[test]
    fn natural_sort() {
        let mut devices = vec!["COM10", "COM2", "COM3", "/dev/ttyUSB10", "/dev/ttyUSB9"];
        devices.sort_by_key(|d| natural_key(d));
        assert_eq!(devices, ["/dev/ttyUSB9", "/dev/ttyUSB10", "COM2", "COM3", "COM10"]);
    }

    #[test]
    fn description_matches_python_format() {
        let profile = Profile { device: "COM3".into(), baud: 9600, ..Profile::default() };
        assert_eq!(description(&profile), "Serial  COM3  9600-8N1");
        let profile = Profile { device: "COM3".into(), parity: "Even".into(), stopbits: 1.5, ..profile };
        assert_eq!(description(&profile), "Serial  COM3  9600-8E1.5");
    }

    #[test]
    fn unsupported_line_settings_are_reported() {
        let profile = Profile { parity: "Mark".into(), ..Profile::default() };
        assert!(settings(&profile).unwrap_err().contains("Mark"));
        let profile = Profile { stopbits: 1.5, ..Profile::default() };
        assert!(settings(&profile).unwrap_err().contains("1.5"));
    }
}
