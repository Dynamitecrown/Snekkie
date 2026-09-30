//! The telnet protocol (RFC 854 and friends), without the socket.
//!
//! Network gear and console servers only need a handful of options: echo
//! and suppress-go-ahead for character-at-a-time typing, window size so long
//! lines wrap where the screen does, and terminal type. Everything else is
//! politely refused. Kept free of I/O so it can be tested byte by byte; the
//! socket side is in `tcp.rs`.

use super::TERM;

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const BRK: u8 = 243;
const NOP: u8 = 241;
const SE: u8 = 240;

const BINARY: u8 = 0;
const ECHO: u8 = 1;
const SGA: u8 = 3;
const TTYPE: u8 = 24;
const NAWS: u8 = 31;

const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

/// A telnet BREAK. Console servers turn it into a real break on the line.
pub const BREAK: [u8; 2] = [IAC, BRK];

/// A no-op, sent as a keepalive.
pub const KEEPALIVE: [u8; 2] = [IAC, NOP];

/// Longest subnegotiation kept; anything past this is junk.
const MAX_SUB: usize = 1024;

/// Options the server may turn on at its end.
fn server_may(option: u8) -> bool {
    matches!(option, BINARY | ECHO | SGA)
}

/// Options we'll turn on at our end.
fn we_may(option: u8) -> bool {
    matches!(option, SGA | TTYPE | NAWS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    Iac,
    /// Got IAC and WILL/WONT/DO/DONT; the option byte is next.
    Option(u8),
    Sub,
    SubIac,
}

/// Where an option stands at one end of the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Side {
    #[default]
    Off,
    /// We asked and are waiting for the answer.
    Asked,
    On,
}

/// What came in, split into bytes for the screen and replies for the server.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Received {
    pub data: Vec<u8>,
    pub reply: Vec<u8>,
}

pub struct Telnet {
    state: State,
    /// Options at the server's end (it WILLs, we DO).
    remote: [Side; 256],
    /// Options at our end (we WILL, it DOes).
    local: [Side; 256],
    sub: Vec<u8>,
    size: (u16, u16),
    after_cr: bool,
}

impl Telnet {
    pub fn new(columns: u16, lines: u16) -> Self {
        Telnet {
            state: State::Data,
            remote: [Side::Off; 256],
            local: [Side::Off; 256],
            sub: Vec::new(),
            size: (columns, lines),
            after_cr: false,
        }
    }

    /// What to send as soon as the connection opens. Offering first, the way
    /// PuTTY does, gets gear that waits for the client to start into
    /// character-at-a-time mode.
    pub fn start(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for option in [NAWS, TTYPE, SGA] {
            self.local[option as usize] = Side::Asked;
            out.extend([IAC, WILL, option]);
        }
        for option in [ECHO, SGA] {
            self.remote[option as usize] = Side::Asked;
            out.extend([IAC, DO, option]);
        }
        out
    }

    /// Split bytes from the server into screen data and replies.
    pub fn receive(&mut self, bytes: &[u8]) -> Received {
        let mut received = Received::default();
        for &byte in bytes {
            self.state = match (self.state, byte) {
                (State::Data, IAC) => State::Iac,
                (State::Data, _) => {
                    // Outside binary mode a bare carriage return comes as CR NUL.
                    let padding = byte == 0 && self.after_cr && self.remote[BINARY as usize] != Side::On;
                    if !padding {
                        received.data.push(byte);
                    }
                    self.after_cr = byte == b'\r';
                    State::Data
                }
                (State::Iac, IAC) => {
                    received.data.push(IAC);
                    self.after_cr = false;
                    State::Data
                }
                (State::Iac, WILL | WONT | DO | DONT) => State::Option(byte),
                (State::Iac, SB) => {
                    self.sub.clear();
                    State::Sub
                }
                // NOP, GA, data mark and the rest: nothing to do.
                (State::Iac, _) => State::Data,
                (State::Option(verb), option) => {
                    self.negotiate(verb, option, &mut received.reply);
                    State::Data
                }
                (State::Sub, IAC) => State::SubIac,
                (State::Sub, _) => {
                    if self.sub.len() < MAX_SUB {
                        self.sub.push(byte);
                    }
                    State::Sub
                }
                (State::SubIac, SE) => {
                    self.subnegotiation(&mut received.reply);
                    State::Data
                }
                (State::SubIac, _) => {
                    if self.sub.len() < MAX_SUB {
                        self.sub.push(byte);
                    }
                    State::Sub
                }
            };
        }
        received
    }

    fn negotiate(&mut self, verb: u8, option: u8, reply: &mut Vec<u8>) {
        let index = option as usize;
        match verb {
            WILL => match self.remote[index] {
                Side::On => {}
                Side::Asked => self.remote[index] = Side::On,
                Side::Off if server_may(option) => {
                    self.remote[index] = Side::On;
                    reply.extend([IAC, DO, option]);
                }
                Side::Off => reply.extend([IAC, DONT, option]),
            },
            WONT => {
                if self.remote[index] == Side::On {
                    reply.extend([IAC, DONT, option]);
                }
                self.remote[index] = Side::Off;
            }
            DO => match self.local[index] {
                Side::On => {}
                Side::Asked => {
                    self.local[index] = Side::On;
                    self.enabled(option, reply);
                }
                Side::Off if we_may(option) => {
                    self.local[index] = Side::On;
                    reply.extend([IAC, WILL, option]);
                    self.enabled(option, reply);
                }
                Side::Off => reply.extend([IAC, WONT, option]),
            },
            DONT => {
                if self.local[index] == Side::On {
                    reply.extend([IAC, WONT, option]);
                }
                self.local[index] = Side::Off;
            }
            _ => {}
        }
    }

    /// An option just came on at our end.
    fn enabled(&self, option: u8, reply: &mut Vec<u8>) {
        if option == NAWS {
            reply.extend(self.window_size());
        }
    }

    fn subnegotiation(&mut self, reply: &mut Vec<u8>) {
        if self.sub.as_slice() == [TTYPE, TTYPE_SEND] && self.local[TTYPE as usize] == Side::On {
            reply.extend([IAC, SB, TTYPE, TTYPE_IS]);
            reply.extend(TERM.as_bytes());
            reply.extend([IAC, SE]);
        }
    }

    fn window_size(&self) -> Vec<u8> {
        let mut out = vec![IAC, SB, NAWS];
        for value in [self.size.0, self.size.1] {
            for byte in value.to_be_bytes() {
                out.push(byte);
                if byte == IAC {
                    out.push(IAC);
                }
            }
        }
        out.extend([IAC, SE]);
        out
    }

    /// Whether the server has agreed to echo what's typed.
    pub fn remote_echo(&self) -> bool {
        self.remote[ECHO as usize] == Side::On
    }

    /// The window changed size. Returns what to tell the server, if it
    /// asked to be told.
    pub fn resize(&mut self, columns: u16, lines: u16) -> Vec<u8> {
        self.size = (columns, lines);
        if self.local[NAWS as usize] == Side::On { self.window_size() } else { Vec::new() }
    }

    /// Escape what the user typed for the wire.
    pub fn encode(&self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len() + 4);
        for (i, &byte) in bytes.iter().enumerate() {
            match byte {
                IAC => out.extend([IAC, IAC]),
                // Enter sends CR; telnet's newline is CR LF (we never go
                // binary, so a lone CR isn't allowed on the wire).
                b'\r' if bytes.get(i + 1) != Some(&b'\n') => out.extend(b"\r\n"),
                _ => out.push(byte),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> Telnet {
        let mut telnet = Telnet::new(80, 24);
        telnet.start();
        telnet
    }

    #[test]
    fn opens_by_offering_the_usual_options() {
        let mut telnet = Telnet::new(80, 24);
        assert_eq!(telnet.start(), [IAC, WILL, NAWS, IAC, WILL, TTYPE, IAC, WILL, SGA, IAC, DO, ECHO, IAC, DO, SGA]);
    }

    #[test]
    fn answers_to_our_offers_are_not_answered_again() {
        let mut telnet = started();
        let received = telnet.receive(&[IAC, WILL, ECHO, IAC, WILL, SGA, IAC, DO, SGA, IAC, DO, TTYPE]);
        assert_eq!(received, Received::default());
    }

    #[test]
    fn window_size_is_sent_once_the_server_wants_it() {
        let mut telnet = started();
        assert!(telnet.resize(100, 30).is_empty(), "not asked for yet");
        let received = telnet.receive(&[IAC, DO, NAWS]);
        assert_eq!(received.reply, [IAC, SB, NAWS, 0, 100, 0, 30, IAC, SE]);
        assert_eq!(telnet.resize(132, 50), [IAC, SB, NAWS, 0, 132, 0, 50, IAC, SE]);
    }

    #[test]
    fn a_255_in_the_window_size_is_escaped() {
        let mut telnet = Telnet::new(255, 24);
        let received = telnet.receive(&[IAC, DO, NAWS]);
        assert_eq!(received.reply, [IAC, WILL, NAWS, IAC, SB, NAWS, 0, IAC, IAC, 0, 24, IAC, SE]);
    }

    #[test]
    fn terminal_type_is_reported_when_asked() {
        let mut telnet = started();
        telnet.receive(&[IAC, DO, TTYPE]);
        let received = telnet.receive(&[IAC, SB, TTYPE, TTYPE_SEND, IAC, SE]);
        let mut expected = vec![IAC, SB, TTYPE, TTYPE_IS];
        expected.extend(b"xterm-256color");
        expected.extend([IAC, SE]);
        assert_eq!(received.reply, expected);
    }

    #[test]
    fn negotiation_split_across_reads() {
        let mut telnet = started();
        telnet.receive(&[IAC, DO, TTYPE]);
        let mut reply = Vec::new();
        for byte in [IAC, SB, TTYPE, TTYPE_SEND, IAC, SE] {
            let received = telnet.receive(&[byte]);
            assert!(received.data.is_empty());
            reply.extend(received.reply);
        }
        assert!(reply.starts_with(&[IAC, SB, TTYPE, TTYPE_IS]));
    }

    #[test]
    fn a_server_that_starts_first_gets_answers() {
        let mut telnet = Telnet::new(80, 24);
        let received = telnet.receive(&[IAC, WILL, ECHO, IAC, DO, NAWS]);
        assert_eq!(received.reply, [IAC, DO, ECHO, IAC, WILL, NAWS, IAC, SB, NAWS, 0, 80, 0, 24, IAC, SE]);
        // Saying it again changes nothing, so gets no reply.
        assert!(telnet.receive(&[IAC, WILL, ECHO, IAC, DO, NAWS]).reply.is_empty());
    }

    #[test]
    fn unknown_options_are_refused() {
        let mut telnet = started();
        // Linemode (34) and environment (39).
        let received = telnet.receive(&[IAC, DO, 34, IAC, WILL, 39]);
        assert_eq!(received.reply, [IAC, WONT, 34, IAC, DONT, 39]);
    }

    #[test]
    fn remote_echo_follows_negotiation() {
        let mut telnet = started();
        assert!(!telnet.remote_echo(), "asked, not yet agreed");
        telnet.receive(&[IAC, WILL, ECHO]);
        assert!(telnet.remote_echo());
        telnet.receive(&[IAC, WONT, ECHO]);
        assert!(!telnet.remote_echo());
    }

    #[test]
    fn switching_an_option_off_is_acknowledged_once() {
        let mut telnet = started();
        telnet.receive(&[IAC, WILL, ECHO]);
        assert_eq!(telnet.receive(&[IAC, WONT, ECHO]).reply, [IAC, DONT, ECHO]);
        assert!(telnet.receive(&[IAC, WONT, ECHO]).reply.is_empty());
        // A refusal of something we asked for needs no answer either.
        let mut telnet = started();
        assert!(telnet.receive(&[IAC, DONT, TTYPE]).reply.is_empty());
    }

    #[test]
    fn commands_are_stripped_from_the_data() {
        let mut telnet = started();
        let received = telnet.receive(&[b'o', b'k', IAC, NOP, IAC, IAC, b'!', IAC, 249 /* GA */]);
        assert_eq!(received.data, [b'o', b'k', 255, b'!']);
        assert!(received.reply.is_empty());
    }

    #[test]
    fn padding_after_a_carriage_return_is_dropped() {
        let mut telnet = started();
        assert_eq!(telnet.receive(b"a\r\0b\r\n\0").data, b"a\rb\r\n\0");
        // In binary mode a NUL is just a NUL.
        telnet.receive(&[IAC, WILL, BINARY]);
        assert_eq!(telnet.receive(b"\r\0").data, b"\r\0");
    }

    #[test]
    fn typing_is_escaped_and_enter_becomes_cr_lf() {
        let telnet = started();
        assert_eq!(telnet.encode(b"show ver\r"), b"show ver\r\n");
        assert_eq!(telnet.encode(b"a\r\nb"), b"a\r\nb");
        assert_eq!(telnet.encode(&[b'x', IAC]), [b'x', IAC, IAC]);
    }
}
