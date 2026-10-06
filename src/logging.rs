//! Local session records, streamed decoding, rotation and input privacy.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak, mpsc};

use chrono::{Local, Utc};
use regex::Regex;

pub fn resolved_path(template: &str, host: &str) -> PathBuf {
    let host: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .take(100)
        .collect();
    let host = if host.trim_matches('.').is_empty() { "session" } else { host.trim_matches('.') };
    PathBuf::from(template.replace("{host}", host).replace("{date}", &Local::now().format("%Y-%m-%d").to_string()))
}

pub fn password_prompt(text: &str) -> bool {
    static PROMPT: OnceLock<Regex> = OnceLock::new();
    PROMPT
        .get_or_init(|| {
            Regex::new(r"(?i)\b(?:password|passphrase|passcode|secret|pin)(?:\s+for\s+[^:\r\n]+)?\s*:\s*$").unwrap()
        })
        .is_match(text)
}

fn redact_commands(text: &str) -> String {
    static SECRET: OnceLock<Regex> = OnceLock::new();
    SECRET
        .get_or_init(|| Regex::new(r"(?i)\b(password|passwd|passphrase|secret|community)\s+\S+.*$").unwrap())
        .replace(text, "$1 [redacted]")
        .into_owned()
}

#[derive(Default)]
pub struct Privacy {
    pub allow_passwords: bool,
    pub manual: bool,
    automatic: bool,
    hide_echo: bool,
    output_marker: bool,
    input_marker: bool,
}

impl Privacy {
    pub fn new(allow_passwords: bool) -> Self {
        Self { allow_passwords, ..Default::default() }
    }
    pub fn input(&mut self, bytes: &[u8], prompt: &str) -> Vec<u8> {
        if !self.allow_passwords && password_prompt(prompt) {
            self.automatic = true;
        }
        if self.manual || self.automatic {
            self.hide_echo = true;
            let marker = !std::mem::replace(&mut self.input_marker, true);
            if bytes.contains(&b'\r') || bytes.contains(&b'\n') {
                self.automatic = false;
                self.input_marker = false;
            }
            if marker { b"[private input]\n".to_vec() } else { Vec::new() }
        } else {
            bytes.to_vec()
        }
    }

    /// Hide remote echo through its newline. Screen and transport bytes stay intact.
    pub fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.manual {
            self.hide_echo = true;
            return self.marker();
        }
        if self.hide_echo {
            let mut safe = self.marker();
            if let Some(end) = bytes.iter().position(|b| *b == b'\n') {
                self.hide_echo = false;
                self.output_marker = false;
                safe.extend_from_slice(&bytes[end + 1..]);
            }
            safe
        } else {
            bytes.to_vec()
        }
    }

    fn marker(&mut self) -> Vec<u8> {
        if std::mem::replace(&mut self.output_marker, true) { Vec::new() } else { b"[private output]\r\n".to_vec() }
    }

    pub fn protected(&self, prompt: &str) -> bool {
        self.manual || self.automatic || self.hide_echo || (!self.allow_passwords && password_prompt(prompt))
    }
}

/// Archives use create_new: prior files are never replaced by rotation.
pub struct RotatingFile {
    path: PathBuf,
    file: File,
    size: u64,
    limit: u64,
    daily: bool,
    day: String,
    coordinate: Arc<parking_lot::Mutex<()>>,
}

impl RotatingFile {
    pub fn open(path: &Path, limit: u64, daily: bool) -> io::Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        static FILES: OnceLock<parking_lot::Mutex<BTreeMap<PathBuf, Weak<parking_lot::Mutex<()>>>>> = OnceLock::new();
        let key = path.canonicalize()?;
        let coordinate = {
            let mut files = FILES.get_or_init(Default::default).lock();
            files.retain(|_, file| file.strong_count() > 0);
            if let Some(existing) = files.get(&key).and_then(Weak::upgrade) {
                existing
            } else {
                let coordinate = Arc::new(parking_lot::Mutex::new(()));
                files.insert(key, Arc::downgrade(&coordinate));
                coordinate
            }
        };
        let day = file
            .metadata()?
            .modified()
            .ok()
            .map(chrono::DateTime::<Local>::from)
            .unwrap_or_else(Local::now)
            .format("%Y-%m-%d")
            .to_string();
        Ok(Self { size: file.metadata()?.len(), path: path.into(), file, limit, daily, day, coordinate })
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let stamp = Utc::now().format("%Y%m%dT%H%M%S%3fZ");
        let mut index = 0;
        let archive = loop {
            let mut archive = self.path.as_os_str().to_os_string();
            archive.push(format!(".{stamp}.{index}"));
            match OpenOptions::new().write(true).create_new(true).open(Path::new(&archive)) {
                Ok(mut destination) => {
                    let mut source = File::open(&self.path)?;
                    io::copy(&mut source, &mut destination)?;
                    destination.flush()?;
                    break archive;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => index += 1,
                Err(error) => return Err(error),
            }
        };
        // The complete archive exists before touching the active file.
        // Windows append handles do not carry the permission needed by set_len.
        OpenOptions::new().write(true).open(&self.path)?.set_len(0)?;
        self.size = 0;
        self.day = Local::now().format("%Y-%m-%d").to_string();
        let _ = archive;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let coordinate = self.coordinate.clone();
        let _guard = coordinate.lock();
        self.size = self.file.metadata()?.len();
        let today = Local::now().format("%Y-%m-%d").to_string();
        if self.size > 0
            && ((self.limit > 0 && self.size.saturating_add(bytes.len() as u64) > self.limit)
                || (self.daily && self.day != today))
        {
            self.rotate()?;
        }
        let written = self.file.write(bytes)?;
        self.size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Input,
    Output,
}

struct Record {
    direction: Direction,
    bytes: Vec<u8>,
    time: chrono::DateTime<Utc>,
}

#[derive(Clone)]
pub struct LogHandle {
    sender: mpsc::SyncSender<Record>,
}

impl LogHandle {
    pub fn start(
        path: &Path,
        limit: u64,
        daily: bool,
        allow_passwords: bool,
        raw: bool,
        failure: Arc<dyn Fn(String) + Send + Sync>,
    ) -> io::Result<(Self, std::thread::JoinHandle<()>)> {
        let file = RotatingFile::open(path, limit, daily)?;
        let writer = if raw {
            let mut input_path = path.as_os_str().to_os_string();
            input_path.push(".input.log");
            let input = RotatingFile::open(Path::new(&input_path), limit, daily)?;
            Recorder::Raw {
                output: RawWriter::new(file, allow_passwords),
                input: TextWriter::new(input, allow_passwords),
            }
        } else {
            Recorder::Text(TextWriter::new(file, allow_passwords))
        };
        Self::spawn(writer, failure)
    }

    #[cfg(test)]
    pub(crate) fn start_test(
        writer: Box<dyn Write + Send>,
        failure: Arc<dyn Fn(String) + Send + Sync>,
    ) -> io::Result<(Self, std::thread::JoinHandle<()>)> {
        Self::spawn(Recorder::Test(TextWriter::new(writer, false)), failure)
    }

    fn spawn(
        mut writer: Recorder,
        failure: Arc<dyn Fn(String) + Send + Sync>,
    ) -> io::Result<(Self, std::thread::JoinHandle<()>)> {
        let (sender, receiver) = mpsc::sync_channel::<Record>(1024);
        let worker = std::thread::Builder::new().name("snekkie-log".into()).spawn(move || {
            let result = (|| {
                while let Ok(record) = receiver.recv() {
                    writer.record(record)?;
                }
                writer.finish()
            })();
            if let Err(error) = result {
                failure(format!("Session logging failed: {error}"));
            }
        })?;
        Ok((Self { sender }, worker))
    }

    pub fn input(&self, bytes: Vec<u8>) -> io::Result<()> {
        self.submit(Direction::Input, bytes)
    }

    fn submit(&self, direction: Direction, bytes: Vec<u8>) -> io::Result<()> {
        self.sender
            .try_send(Record { direction, bytes, time: Utc::now() })
            .map_err(|error| io::Error::other(format!("log queue unavailable: {error}")))
    }
}

impl Write for LogHandle {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.submit(Direction::Output, bytes.to_vec())?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum Recorder {
    Text(TextWriter<RotatingFile>),
    Raw {
        output: RawWriter<RotatingFile>,
        input: TextWriter<RotatingFile>,
    },
    #[cfg(test)]
    Test(TextWriter<Box<dyn Write + Send>>),
}

impl Recorder {
    fn record(&mut self, record: Record) -> io::Result<()> {
        match self {
            Self::Text(writer) => writer.record(record),
            #[cfg(test)]
            Self::Test(writer) => writer.record(record),
            Self::Raw { output, input } => match record.direction {
                Direction::Input => input.record(record),
                Direction::Output => output.receive(&record.bytes),
            },
        }
    }

    fn finish(&mut self) -> io::Result<()> {
        match self {
            Self::Text(writer) => writer.finish(),
            #[cfg(test)]
            Self::Test(writer) => writer.finish(),
            Self::Raw { output, input } => {
                output.finish()?;
                input.finish()
            }
        }
    }
}

/// Preserve ordinary raw lines exactly. Buffer sensitive candidates until the
/// complete line is available so a transport chunk cannot bypass redaction.
struct RawWriter<W: Write> {
    writer: W,
    pending: Vec<u8>,
    allow_passwords: bool,
}

impl<W: Write> RawWriter<W> {
    fn new(writer: W, allow_passwords: bool) -> Self {
        Self { writer, pending: Vec::new(), allow_passwords }
    }

    fn emit(&mut self) -> io::Result<()> {
        let text = String::from_utf8_lossy(&self.pending);
        static ANSI: OnceLock<Regex> = OnceLock::new();
        let plain = ANSI
            .get_or_init(|| Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))").unwrap())
            .replace_all(&text, "");
        let plain = plain.trim_end_matches(['\r', '\n']);
        let safe = redact_commands(plain);
        if safe == plain {
            self.writer.write_all(&self.pending)?;
        } else {
            let trailing = self.pending.iter().rev().take_while(|b| matches!(b, b'\r' | b'\n')).count();
            let mut record = safe.into_bytes();
            record.extend_from_slice(&self.pending[self.pending.len() - trailing..]);
            self.writer.write_all(&record)?;
        }
        self.pending.clear();
        Ok(())
    }

    fn receive(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.allow_passwords {
            self.writer.write_all(bytes)?;
        } else {
            for &byte in bytes {
                self.pending.push(byte);
                if byte == b'\n' {
                    self.emit()?;
                }
            }
        }
        self.writer.flush()
    }

    fn finish(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            self.emit()?;
        }
        self.writer.flush()
    }
}

#[derive(Default)]
struct TextStream {
    bytes: Vec<u8>,
    escape: u8,
    cr: bool,
    time: Option<chrono::DateTime<Utc>>,
}

struct TextWriter<W: Write> {
    writer: W,
    streams: [TextStream; 2],
    allow_passwords: bool,
}

impl<W: Write> TextWriter<W> {
    fn new(writer: W, allow_passwords: bool) -> Self {
        Self { writer, streams: Default::default(), allow_passwords }
    }

    fn line(&mut self, index: usize) -> io::Result<()> {
        let stream = &mut self.streams[index];
        let text = String::from_utf8_lossy(&stream.bytes);
        let text = if self.allow_passwords { text.into_owned() } else { redact_commands(&text) };
        let stamp = stream.time.take().unwrap_or_else(Utc::now).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let record = format!("[{stamp}] {} {text}\n", if index == 0 { "TX" } else { "RX" });
        self.writer.write_all(record.as_bytes())?;
        stream.bytes.clear();
        Ok(())
    }

    fn record(&mut self, record: Record) -> io::Result<()> {
        let index = if matches!(record.direction, Direction::Input) { 0 } else { 1 };
        for byte in record.bytes {
            let stream = &mut self.streams[index];
            stream.time.get_or_insert(record.time);
            if index == 0 && (byte < 0x20 && !matches!(byte, b'\r' | b'\n' | b'\t') || byte == 127) {
                stream.cr = false;
                stream.bytes.extend_from_slice(format!("<0x{byte:02x}>").as_bytes());
                continue;
            }
            match stream.escape {
                1 => {
                    stream.escape = match byte {
                        b'[' => 2,
                        b']' => 3,
                        _ => 0,
                    };
                    continue;
                }
                2 => {
                    if (0x40..=0x7e).contains(&byte) {
                        stream.escape = 0;
                    }
                    continue;
                }
                3 => {
                    if byte == 7 {
                        stream.escape = 0;
                    } else if byte == 27 {
                        stream.escape = 4;
                    }
                    continue;
                }
                4 => {
                    stream.escape = if byte == b'\\' { 0 } else { 3 };
                    continue;
                }
                _ => {}
            }
            if byte == 27 {
                stream.escape = 1;
                continue;
            }
            if byte == b'\n' && stream.cr {
                stream.cr = false;
                stream.time = None;
                continue;
            }
            stream.cr = byte == b'\r';
            match byte {
                b'\r' | b'\n' => self.line(index)?,
                8 | 127 => {
                    let mut start = stream.bytes.len().saturating_sub(1);
                    while start > 0 && stream.bytes[start] & 0xc0 == 0x80 {
                        start -= 1;
                    }
                    stream.bytes.truncate(start);
                }
                b'\t' | 0x20..=0xff => stream.bytes.push(byte),
                _ if index == 0 => stream.bytes.extend_from_slice(format!("<0x{byte:02x}>").as_bytes()),
                _ => {}
            }
        }
        self.writer.flush()
    }

    fn finish(&mut self) -> io::Result<()> {
        for index in 0..2 {
            if !self.streams[index].bytes.is_empty() {
                self.line(index)?;
            }
        }
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_streams_handle_split_utf8_ansi_crlf_and_secrets_in_both_directions() {
        let mut writer = TextWriter::new(Vec::new(), false);
        for (direction, bytes) in [
            (Direction::Output, b"\x1b[3".as_slice()),
            (Direction::Output, b"2mcaf\xc3".as_slice()),
            (Direction::Output, b"\xa9\x1b[0m\r".as_slice()),
            (Direction::Output, b"\nusername bob secret 0 very-private\n".as_slice()),
            (Direction::Input, b"show version\npassword hidden\n\x03\x1b[D\x08".as_slice()),
        ] {
            writer.record(Record { direction, bytes: bytes.into(), time: Utc::now() }).unwrap();
        }
        writer.finish().unwrap();
        let text = String::from_utf8(writer.writer).unwrap();
        assert!(text.contains("RX café\n"));
        assert!(text.contains("TX show version\n"));
        assert!(text.contains("secret [redacted]"));
        assert!(!text.contains("very-private") && !text.contains("hidden") && !text.contains('\x1b'));
        assert!(text.contains("TX <0x03><0x1b>[D<0x08>"));
        assert_eq!(text.lines().count(), 5);
    }

    #[test]
    fn privacy_hides_chunked_echo_and_private_input_overrides_password_logging() {
        assert!(password_prompt("Enter password for lab: "));
        assert!(password_prompt("Password:"));
        assert!(!password_prompt("show password policy"));
        let mut privacy = Privacy::default();
        assert_eq!(privacy.input(b"s", "Password: "), b"[private input]\n");
        assert_eq!(privacy.output(b"s"), b"[private output]\r\n");
        assert!(privacy.output(b"upersecret").is_empty());
        privacy.input(b"upersecret\r", "Password: ");
        assert_eq!(privacy.output(b"\r\nWelcome\r\n"), b"Welcome\r\n");
        let mut privacy = Privacy::new(true);
        assert_eq!(privacy.input(b"allowed\r", "Password:"), b"allowed\r");
        privacy.manual = true;
        assert_eq!(privacy.input(b"must-hide\r", "custom prompt"), b"[private input]\n");
        assert!(!String::from_utf8(privacy.output(b"must-hide")).unwrap().contains("must-hide"));
    }

    #[test]
    fn rotation_preserves_prior_files_and_append_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.log");
        std::fs::write(&path, b"prior").unwrap();
        let mut writer = RotatingFile::open(&path, 6, false).unwrap();
        writer.write_all(b"new").unwrap();
        writer.write_all(b"more").unwrap();
        writer.flush().unwrap();
        let mut contents: Vec<_> =
            std::fs::read_dir(dir.path()).unwrap().map(|entry| std::fs::read(entry.unwrap().path()).unwrap()).collect();
        contents.sort();
        assert_eq!(contents, vec![b"more".to_vec(), b"new".to_vec(), b"prior".to_vec()]);
        writer.day = "2000-01-01".into();
        writer.daily = true;
        writer.write_all(b"next day").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"next day");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
    }

    #[test]
    fn enabled_password_logging_is_explicit_and_filename_substitutions_are_safe() {
        let mut writer = TextWriter::new(Vec::new(), true);
        writer
            .record(Record {
                direction: Direction::Input,
                bytes: b"password explicitly-enabled\n".to_vec(),
                time: Utc::now(),
            })
            .unwrap();
        assert!(String::from_utf8(writer.writer).unwrap().contains("explicitly-enabled"));
        let path = resolved_path("logs/{host}-{date}.log", "../../Lab:3");
        assert_eq!(path.parent().unwrap(), Path::new("logs"));
        assert!(!path.file_name().unwrap().to_string_lossy().contains(['/', '\\', ':']));
    }

    #[test]
    fn raw_redaction_cannot_be_bypassed_by_chunks_or_ansi_and_keeps_other_bytes_exact() {
        let ordinary = b"\x1b[32mready\x1b[0m\r\ncaf\xc3\xa9\xff\r\n";
        let mut writer = RawWriter::new(Vec::new(), false);
        for chunk in ordinary.chunks(3) {
            writer.receive(chunk).unwrap();
        }
        for chunk in b"username lab s\x1b[0mecret 0 never-log-this\r\n".chunks(2) {
            writer.receive(chunk).unwrap();
        }
        writer.receive(b"final partial").unwrap();
        writer.finish().unwrap();
        assert!(writer.writer.starts_with(ordinary));
        assert!(writer.writer.ends_with(b"final partial"));
        let text = String::from_utf8_lossy(&writer.writer);
        assert!(text.contains("secret [redacted]"));
        assert!(!text.contains("never-log-this"));
    }
}
