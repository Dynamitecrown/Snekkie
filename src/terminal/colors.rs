//! Turning the emulator's colour references into real colours.

use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use egui::Color32;

use crate::settings::Theme;

/// The 16 ANSI colours. Fixed across themes, same values as the Python
/// release, which in turn are the Tango palette.
pub const ANSI: [Color32; 16] = [
    Color32::from_rgb(0x2e, 0x34, 0x36), // black
    Color32::from_rgb(0xcc, 0x33, 0x33), // red
    Color32::from_rgb(0x4e, 0x9a, 0x06), // green
    Color32::from_rgb(0xc4, 0xa0, 0x00), // yellow
    Color32::from_rgb(0x34, 0x65, 0xa4), // blue
    Color32::from_rgb(0xa3, 0x47, 0xba), // magenta
    Color32::from_rgb(0x06, 0x98, 0x9a), // cyan
    Color32::from_rgb(0xd3, 0xd7, 0xcf), // white
    Color32::from_rgb(0x66, 0x66, 0x66), // bright black
    Color32::from_rgb(0xef, 0x4a, 0x4a), // bright red
    Color32::from_rgb(0x8a, 0xe2, 0x34), // bright green
    Color32::from_rgb(0xfc, 0xe9, 0x4f), // bright yellow
    Color32::from_rgb(0x72, 0x9f, 0xcf), // bright blue
    Color32::from_rgb(0xad, 0x7f, 0xa8), // bright magenta
    Color32::from_rgb(0x34, 0xe2, 0xe2), // bright cyan
    Color32::from_rgb(0xee, 0xee, 0xec), // bright white
];

/// xterm's 256-colour palette: the 16 above, a 6x6x6 cube, and 24 greys.
pub fn indexed(index: u8) -> Color32 {
    match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let i = index - 16;
            Color32::from_rgb(LEVELS[(i / 36) as usize], LEVELS[((i / 6) % 6) as usize], LEVELS[(i % 6) as usize])
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            Color32::from_rgb(v, v, v)
        }
    }
}

pub fn to_rgb(c: Color32) -> Rgb {
    Rgb { r: c.r(), g: c.g(), b: c.b() }
}

fn from_rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

fn dim(c: Color32) -> Color32 {
    Color32::from_rgb((c.r() as u16 * 2 / 3) as u8, (c.g() as u16 * 2 / 3) as u8, (c.b() as u16 * 2 / 3) as u8)
}

/// Palette entry by alacritty's colour index (0-255 plus the named slots),
/// honouring any colour the remote redefined with OSC 4/10/11.
pub fn palette(index: usize, theme: &Theme, overrides: &Colors) -> Color32 {
    if let Some(rgb) = overrides[index] {
        return from_rgb(rgb);
    }
    match index {
        0..=255 => indexed(index as u8),
        i if i == NamedColor::Foreground as usize => theme.fg,
        i if i == NamedColor::Background as usize => theme.bg,
        i if i == NamedColor::Cursor as usize => theme.cursor,
        i if i == NamedColor::BrightForeground as usize => theme.fg,
        i if i == NamedColor::DimForeground as usize => dim(theme.fg),
        i if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) => {
            dim(ANSI[i - NamedColor::DimBlack as usize])
        }
        _ => theme.fg,
    }
}

fn resolve(color: Color, theme: &Theme, overrides: &Colors) -> Color32 {
    match color {
        Color::Spec(rgb) => from_rgb(rgb),
        Color::Indexed(i) => palette(i as usize, theme, overrides),
        Color::Named(n) => palette(n as usize, theme, overrides),
    }
}

/// Foreground and background for one cell, after bold-brightening, dim,
/// reverse video and hidden text.
pub fn cell_colors(cell: &Cell, theme: &Theme, overrides: &Colors) -> (Color32, Color32) {
    let flags = cell.flags;
    let mut fg_ref = cell.fg;
    // Bold on one of the first eight colours means its bright variant, the
    // way xterm and PuTTY draw it.
    if flags.contains(Flags::BOLD)
        && let Color::Named(n) = fg_ref
        && (n as usize) < 8
    {
        fg_ref = Color::Indexed(n as u8 + 8);
    }
    let mut fg = resolve(fg_ref, theme, overrides);
    let mut bg = resolve(cell.bg, theme, overrides);
    if flags.contains(Flags::DIM) && !flags.contains(Flags::BOLD) {
        fg = dim(fg);
    }
    if flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if flags.contains(Flags::HIDDEN) {
        fg = bg;
    }
    (fg, bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_and_greys() {
        assert_eq!(indexed(16), Color32::from_rgb(0, 0, 0));
        assert_eq!(indexed(196), Color32::from_rgb(255, 0, 0));
        assert_eq!(indexed(231), Color32::from_rgb(255, 255, 255));
        assert_eq!(indexed(232), Color32::from_rgb(8, 8, 8));
        assert_eq!(indexed(255), Color32::from_rgb(238, 238, 238));
    }

    #[test]
    fn default_colours_come_from_the_theme() {
        let theme = Theme::default();
        let colors = Colors::default();
        let cell = Cell::default();
        assert_eq!(cell_colors(&cell, &theme, &colors), (theme.fg, theme.bg));

        let inverse = Cell { flags: Flags::INVERSE, ..Cell::default() };
        assert_eq!(cell_colors(&inverse, &theme, &colors), (theme.bg, theme.fg));
    }

    #[test]
    fn bold_brightens_the_first_eight() {
        let theme = Theme::default();
        let cell = Cell { fg: Color::Named(NamedColor::Red), flags: Flags::BOLD, ..Cell::default() };
        assert_eq!(cell_colors(&cell, &theme, &Colors::default()).0, ANSI[9]);
    }
}
