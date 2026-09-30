//! Translate key presses into the byte sequences a terminal expects.
//!
//! Reference behaviour is xterm, which is what nearly everything (Cisco IOS
//! included) is built to talk to.

use egui::{Key, Modifiers};

/// What the Backspace key sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backspace {
    /// DEL, PuTTY's default, which modern shells and IOS both expect.
    #[default]
    Del,
    /// ^H, for older and embedded consoles that only erase on that.
    CtrlH,
}

impl Backspace {
    pub const ALL: [Backspace; 2] = [Backspace::Del, Backspace::CtrlH];

    pub fn as_str(self) -> &'static str {
        match self {
            Backspace::Del => "del",
            Backspace::CtrlH => "ctrl-h",
        }
    }

    pub fn parse(s: &str) -> Backspace {
        if s == "ctrl-h" { Backspace::CtrlH } else { Backspace::Del }
    }

    pub fn label(self) -> &'static str {
        match self {
            Backspace::Del => "DEL (^?)",
            Backspace::CtrlH => "Ctrl+H (^H)",
        }
    }

    fn byte(self) -> u8 {
        match self {
            Backspace::Del => 0x7f,
            Backspace::CtrlH => 0x08,
        }
    }
}

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_code(mods: Modifiers) -> u8 {
    1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl)
}

/// Keys whose final byte switches between CSI and SS3 in application mode.
fn cursor_final(key: Key) -> Option<char> {
    Some(match key {
        Key::ArrowUp => 'A',
        Key::ArrowDown => 'B',
        Key::ArrowRight => 'C',
        Key::ArrowLeft => 'D',
        Key::Home => 'H',
        Key::End => 'F',
        _ => return None,
    })
}

/// Keys using the CSI n ~ form.
fn tilde_number(key: Key) -> Option<u8> {
    Some(match key {
        Key::Insert => 2,
        Key::Delete => 3,
        Key::PageUp => 5,
        Key::PageDown => 6,
        Key::F5 => 15,
        Key::F6 => 17,
        Key::F7 => 18,
        Key::F8 => 19,
        Key::F9 => 20,
        Key::F10 => 21,
        Key::F11 => 23,
        Key::F12 => 24,
        _ => return None,
    })
}

/// F1-F4 are SS3-prefixed, not CSI.
fn ss3_function(key: Key) -> Option<char> {
    Some(match key {
        Key::F1 => 'P',
        Key::F2 => 'Q',
        Key::F3 => 'R',
        Key::F4 => 'S',
        _ => return None,
    })
}

/// Ctrl+key -> control character, for the keys that have one.
fn control_byte(key: Key, shift: bool) -> Option<u8> {
    let letter = match key {
        Key::A => 0,
        Key::B => 1,
        Key::C => 2,
        Key::D => 3,
        Key::E => 4,
        Key::F => 5,
        Key::G => 6,
        Key::H => 7,
        Key::I => 8,
        Key::J => 9,
        Key::K => 10,
        Key::L => 11,
        Key::M => 12,
        Key::N => 13,
        Key::O => 14,
        Key::P => 15,
        Key::Q => 16,
        Key::R => 17,
        Key::S => 18,
        Key::T => 19,
        Key::U => 20,
        Key::V => 21,
        Key::W => 22,
        Key::X => 23,
        Key::Y => 24,
        Key::Z => 25,
        _ => 255,
    };
    if letter != 255 {
        return Some(letter + 1);
    }
    Some(match key {
        Key::OpenBracket => 0x1b,  // Ctrl+[
        Key::Backslash => 0x1c,    // Ctrl+\
        Key::CloseBracket => 0x1d, // Ctrl+]
        // Ctrl+Shift+6 (Ctrl+^) is the Cisco escape sequence, e.g. to break
        // out of a ping or a hung telnet.
        Key::Num6 => 0x1e,
        Key::Minus if shift => 0x1f, // Ctrl+_
        Key::Space | Key::Num2 => 0x00,
        _ => return None,
    })
}

/// Bytes for a key press, or None if the key should be left to the text
/// event that follows it (plain printable characters).
pub fn encode_key(key: Key, mods: Modifiers, app_cursor: bool, backspace: Backspace) -> Option<Vec<u8>> {
    let mod_code = modifier_code(mods);
    let modded = mod_code > 1;

    if let Some(fin) = cursor_final(key) {
        return Some(if modded {
            format!("\x1b[1;{mod_code}{fin}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{fin}").into_bytes()
        } else {
            format!("\x1b[{fin}").into_bytes()
        });
    }
    if let Some(num) = tilde_number(key) {
        return Some(if modded {
            format!("\x1b[{num};{mod_code}~").into_bytes()
        } else {
            format!("\x1b[{num}~").into_bytes()
        });
    }
    if let Some(fin) = ss3_function(key) {
        return Some(if modded {
            format!("\x1b[1;{mod_code}{fin}").into_bytes()
        } else {
            format!("\x1bO{fin}").into_bytes()
        });
    }

    match key {
        Key::Enter => return Some(b"\r".to_vec()),
        Key::Tab if mods.shift => return Some(b"\x1b[Z".to_vec()),
        Key::Tab => return Some(b"\t".to_vec()),
        Key::Escape => return Some(b"\x1b".to_vec()),
        Key::Backspace if mods.alt => return Some(vec![0x1b, backspace.byte()]),
        Key::Backspace => return Some(vec![backspace.byte()]),
        _ => {}
    }

    // Ctrl+key. AltGr arrives as Ctrl+Alt on Windows, and those keys type a
    // character through the text event instead, so leave them alone.
    if mods.ctrl && !mods.alt {
        return control_byte(key, mods.shift).map(|b| vec![b]);
    }
    None
}

/// Bytes for typed text. Alt+key sends ESC first (xterm "meta sends
/// escape"), except for AltGr, which Windows reports as Ctrl+Alt.
pub fn encode_text(text: &str, mods: Modifiers) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 1);
    if mods.alt && !mods.ctrl {
        out.push(0x1b);
    }
    out.extend_from_slice(text.as_bytes());
    out
}

/// Clipboard text as the far end should receive it: terminals want CR, not
/// CRLF or LF.
pub fn encode_paste(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
}

/// What to draw for bytes the user sent, when the far end won't echo them.
///
/// Text goes on screen as typed, Enter starts a new line and Backspace rubs
/// out the character before the cursor. Cursor keys and other escape
/// sequences are left out: drawing them would move the cursor about while
/// the device stays where it was. So are other control characters.
pub fn local_echo(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x1b => {
                i += 1;
                match bytes.get(i) {
                    // CSI: parameters and intermediates, then a final byte.
                    Some(b'[') => {
                        i += 1;
                        while bytes.get(i).is_some_and(|b| (0x20..0x40).contains(b)) {
                            i += 1;
                        }
                    }
                    // SS3 (application cursor keys, F1-F4): one more byte.
                    Some(b'O') => i += 1,
                    // Alt+key: the key itself.
                    _ => {}
                }
            }
            b'\r' => out.extend(b"\r\n"),
            0x08 | 0x7f => out.extend(b"\x08 \x08"),
            b'\t' => out.push(b'\t'),
            byte if byte >= 0x20 => out.push(byte),
            _ => {}
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;

    fn ctrl() -> Modifiers {
        Modifiers { ctrl: true, ..Modifiers::NONE }
    }

    fn key(k: Key, m: Modifiers) -> Option<Vec<u8>> {
        encode_key(k, m, false, Backspace::Del)
    }

    fn app_key(k: Key, m: Modifiers) -> Option<Vec<u8>> {
        encode_key(k, m, true, Backspace::Del)
    }

    #[test]
    fn arrows_follow_application_cursor_mode() {
        assert_eq!(key(Key::ArrowUp, NONE).unwrap(), b"\x1b[A");
        assert_eq!(app_key(Key::ArrowUp, NONE).unwrap(), b"\x1bOA");
        assert_eq!(key(Key::Home, NONE).unwrap(), b"\x1b[H");
    }

    #[test]
    fn modified_arrows_use_xterm_parameters() {
        assert_eq!(key(Key::ArrowLeft, ctrl()).unwrap(), b"\x1b[1;5D");
        assert_eq!(key(Key::ArrowRight, Modifiers::SHIFT).unwrap(), b"\x1b[1;2C");
        // Application mode doesn't apply once modifiers are involved.
        assert_eq!(app_key(Key::ArrowUp, Modifiers::ALT).unwrap(), b"\x1b[1;3A");
    }

    #[test]
    fn tilde_and_function_keys() {
        assert_eq!(key(Key::Delete, NONE).unwrap(), b"\x1b[3~");
        assert_eq!(key(Key::PageDown, NONE).unwrap(), b"\x1b[6~");
        assert_eq!(key(Key::F12, NONE).unwrap(), b"\x1b[24~");
        assert_eq!(key(Key::F1, NONE).unwrap(), b"\x1bOP");
        assert_eq!(key(Key::F2, Modifiers::SHIFT).unwrap(), b"\x1b[1;2Q");
        assert_eq!(key(Key::Delete, ctrl()).unwrap(), b"\x1b[3;5~");
    }

    #[test]
    fn simple_keys() {
        assert_eq!(key(Key::Enter, NONE).unwrap(), b"\r");
        assert_eq!(key(Key::Tab, NONE).unwrap(), b"\t");
        assert_eq!(key(Key::Tab, Modifiers::SHIFT).unwrap(), b"\x1b[Z");
        assert_eq!(key(Key::Backspace, NONE).unwrap(), b"\x7f");
        assert_eq!(key(Key::Escape, NONE).unwrap(), b"\x1b");
    }

    #[test]
    fn backspace_can_send_ctrl_h() {
        assert_eq!(encode_key(Key::Backspace, NONE, false, Backspace::CtrlH).unwrap(), [0x08]);
        assert_eq!(encode_key(Key::Backspace, Modifiers::ALT, false, Backspace::CtrlH).unwrap(), [0x1b, 0x08]);
        assert_eq!(key(Key::Backspace, Modifiers::ALT).unwrap(), [0x1b, 0x7f]);
        for option in Backspace::ALL {
            assert_eq!(Backspace::parse(option.as_str()), option);
        }
        assert_eq!(Backspace::parse(""), Backspace::Del);
    }

    #[test]
    fn control_characters() {
        assert_eq!(key(Key::C, ctrl()).unwrap(), [0x03]);
        assert_eq!(key(Key::Z, ctrl()).unwrap(), [0x1a]);
        assert_eq!(key(Key::OpenBracket, ctrl()).unwrap(), [0x1b]);
        assert_eq!(key(Key::Space, ctrl()).unwrap(), [0x00]);
        let ctrl_shift = Modifiers { ctrl: true, shift: true, ..Modifiers::NONE };
        assert_eq!(key(Key::Num6, ctrl_shift).unwrap(), [0x1e]);
        assert_eq!(key(Key::Minus, ctrl_shift).unwrap(), [0x1f]);
    }

    #[test]
    fn plain_letters_are_left_to_text_events() {
        assert_eq!(key(Key::A, NONE), None);
        assert_eq!(key(Key::A, Modifiers::SHIFT), None);
        // AltGr (Ctrl+Alt on Windows) types characters via text events.
        assert_eq!(key(Key::Q, Modifiers { ctrl: true, alt: true, ..Modifiers::NONE }), None);
    }

    #[test]
    fn text_with_alt_is_escape_prefixed() {
        assert_eq!(encode_text("x", NONE), b"x");
        assert_eq!(encode_text("x", Modifiers::ALT), b"\x1bx");
        let altgr = Modifiers { ctrl: true, alt: true, ..Modifiers::NONE };
        assert_eq!(encode_text("@", altgr), b"@");
    }

    #[test]
    fn paste_normalises_line_endings() {
        assert_eq!(encode_paste("a\r\nb\nc"), b"a\rb\rc");
    }

    #[test]
    fn local_echo_draws_text_newlines_and_rubouts() {
        assert_eq!(local_echo(b"show ver\r"), b"show ver\r\n");
        assert_eq!(local_echo(b"ab\x7f"), b"ab\x08 \x08");
        assert_eq!(local_echo(&[0x08]), b"\x08 \x08");
        assert_eq!(local_echo("naïve\t".as_bytes()), "naïve\t".as_bytes());
    }

    #[test]
    fn local_echo_leaves_out_keys_that_would_move_the_cursor() {
        assert_eq!(local_echo(b"\x1b[A"), b"");
        assert_eq!(local_echo(b"\x1bOA"), b"");
        assert_eq!(local_echo(b"\x1b[1;5D"), b"");
        assert_eq!(local_echo(b"\x1b[3~x"), b"x");
        // Alt+b, then Ctrl+C.
        assert_eq!(local_echo(b"a\x1bbc\x03"), b"ac");
        assert_eq!(local_echo(b"\x1b"), b"");
    }
}
