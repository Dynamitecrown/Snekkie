//! Terminal emulation: turns raw bytes into a screen.
//!
//! Writing this yourself is the classic trap -- VT100 has scroll regions,
//! origin mode, character sets, 256-colour and truecolour SGR, and a hundred
//! edge cases that only show up when you run `nano` over a flaky link.
//! alacritty_terminal (the engine inside the Alacritty terminal) handles all
//! of it, so this is a thin wrapper that adds what Snekkie needs on top.

use std::sync::Arc;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use parking_lot::Mutex;

use super::colors;
use crate::settings::Theme;

/// Where the emulator sends bytes it has to answer with (cursor position
/// reports, device attributes...). Wired to the session's transport.
pub type Responder = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

/// State the emulator's event callbacks need, shared with the UI.
struct Shared {
    theme: Theme,
    cell_width: u16,
    cell_height: u16,
    columns: u16,
    lines: u16,
}

#[derive(Clone)]
pub struct Listener {
    respond: Responder,
    shared: Arc<Mutex<Shared>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => (self.respond)(text.into_bytes()),
            Event::ColorRequest(index, format) => {
                let theme = self.shared.lock().theme;
                // The emulator's own colour table is not reachable from here,
                // so remote-redefined colours report their defaults.
                let colour = colors::palette(index, &theme, &Default::default());
                (self.respond)(format(colors::to_rgb(colour)).into_bytes());
            }
            Event::TextAreaSizeRequest(format) => {
                let s = self.shared.lock();
                let size = WindowSize {
                    num_lines: s.lines,
                    num_cols: s.columns,
                    cell_width: s.cell_width,
                    cell_height: s.cell_height,
                };
                drop(s);
                (self.respond)(format(size).into_bytes());
            }
            _ => {}
        }
    }
}

pub struct Emulator {
    term: Term<Listener>,
    parser: Processor,
    shared: Arc<Mutex<Shared>>,
}

pub const MIN_COLUMNS: usize = 2;
pub const MIN_LINES: usize = 2;

impl Emulator {
    pub fn new(columns: usize, lines: usize, scrollback: usize, respond: Responder) -> Self {
        let columns = columns.max(MIN_COLUMNS);
        let lines = lines.max(MIN_LINES);
        let shared = Arc::new(Mutex::new(Shared {
            theme: Theme::default(),
            cell_width: 8,
            cell_height: 16,
            columns: columns as u16,
            lines: lines as u16,
        }));
        let config = Config { scrolling_history: scrollback, ..Config::default() };
        let listener = Listener { respond, shared: shared.clone() };
        let term = Term::new(config, &TermSize::new(columns, lines), listener);
        Emulator { term, parser: Processor::new(), shared }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn term(&self) -> &Term<Listener> {
        &self.term
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.shared.lock().theme = theme;
    }

    pub fn set_cell_size(&mut self, width: f32, height: f32) {
        let mut s = self.shared.lock();
        s.cell_width = width.round() as u16;
        s.cell_height = height.round() as u16;
    }

    // -- geometry -------------------------------------------------------

    pub fn columns(&self) -> usize {
        self.term.columns()
    }

    pub fn lines(&self) -> usize {
        self.term.screen_lines()
    }

    /// Returns true if the size actually changed.
    pub fn resize(&mut self, columns: usize, lines: usize) -> bool {
        let columns = columns.max(MIN_COLUMNS);
        let lines = lines.max(MIN_LINES);
        if columns == self.columns() && lines == self.lines() {
            return false;
        }
        self.term.resize(TermSize::new(columns, lines));
        let mut s = self.shared.lock();
        s.columns = columns as u16;
        s.lines = lines as u16;
        true
    }

    // -- modes ----------------------------------------------------------

    /// True when the far end wants ESC O A instead of ESC [ A.
    pub fn application_cursor_keys(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    pub fn cursor_visible(&self) -> bool {
        self.term.mode().contains(TermMode::SHOW_CURSOR)
    }

    /// Cursor position on the live screen (not affected by scrollback).
    pub fn cursor(&self) -> Point {
        self.term.grid().cursor.point
    }

    // -- scrollback -----------------------------------------------------

    /// Lines of history available above the screen.
    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Lines currently scrolled back from live. 0 == at the bottom.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Scroll up (positive, toward older output) or down by `lines` rows.
    /// Returns true if the view moved.
    pub fn scroll_by(&mut self, lines: i32) -> bool {
        let before = self.display_offset();
        self.term.scroll_display(Scroll::Delta(lines));
        self.display_offset() != before
    }

    /// Jump straight to a scroll depth. 0 == the live bottom.
    pub fn scroll_to(&mut self, lines_back: usize) {
        let target = lines_back.min(self.history_size()) as i32;
        let delta = target - self.display_offset() as i32;
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta));
        }
    }

    pub fn page_up(&mut self) -> bool {
        let before = self.display_offset();
        self.term.scroll_display(Scroll::PageUp);
        self.display_offset() != before
    }

    pub fn page_down(&mut self) -> bool {
        let before = self.display_offset();
        self.term.scroll_display(Scroll::PageDown);
        self.display_offset() != before
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    // -- selection ------------------------------------------------------

    /// Grid point (negative lines are scrollback) for a cell on screen.
    pub fn viewport_to_point(&self, row: usize, column: usize) -> Point {
        let row = row.min(self.lines() - 1);
        let column = column.min(self.columns() - 1);
        Point::new(Line(row as i32 - self.display_offset() as i32), Column(column))
    }

    pub fn start_selection(&mut self, point: Point, side: Side, lines: bool) {
        let ty = if lines { SelectionType::Lines } else { SelectionType::Simple };
        self.term.selection = Some(Selection::new(ty, point, side));
    }

    pub fn update_selection(&mut self, point: Point, side: Side) {
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// Selected text, trailing blanks trimmed from every line, the way the
    /// Python version copied it.
    pub fn selection_text(&self) -> Option<String> {
        let text = self.term.selection_to_string()?;
        let trimmed: Vec<&str> = text.split('\n').map(str::trim_end).collect();
        let text = trimmed.join("\n");
        let text = text.trim_end_matches('\n').to_string();
        (!text.is_empty()).then_some(text)
    }

    /// Select the whole document, scrollback included.
    pub fn select_all(&mut self) {
        let top = Point::new(self.term.grid().topmost_line(), Column(0));
        let bottom = Point::new(self.term.grid().bottommost_line(), self.term.grid().last_column());
        let mut selection = Selection::new(SelectionType::Simple, top, Side::Left);
        selection.update(bottom, Side::Right);
        self.term.selection = Some(selection);
    }

    // -- housekeeping ---------------------------------------------------

    /// Full reset -- the equivalent of typing `reset` in a wedged shell.
    pub fn reset(&mut self) {
        self.feed(b"\x1bc");
        self.term.grid_mut().clear_history();
    }

    /// Clear the screen the way Ctrl+L does in a shell: everything above
    /// the cursor's line scrolls into the history, so the prompt you're
    /// typing at ends up on the top row and nothing is lost.
    pub fn clear(&mut self) {
        self.scroll_to_bottom();
        let row = self.cursor().line.0;
        if row > 0 {
            // SU scrolls the screen up into the history; CUU follows it.
            self.feed(format!("\x1b[{row}S\x1b[{row}A").as_bytes());
        }
    }

    /// Plain text of one visible row, trailing blanks stripped.
    pub fn line_text(&self, row: usize) -> String {
        let line = Line(row as i32 - self.display_offset() as i32);
        let grid = &self.term.grid()[line];
        (0..self.columns()).map(|c| grid[Column(c)].c).collect::<String>().trim_end().to_string()
    }

    /// The visible screen as text, one line per row.
    pub fn screen_text(&self) -> String {
        (0..self.lines()).map(|r| self.line_text(r)).collect::<Vec<_>>().join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emulator(columns: usize, lines: usize, scrollback: usize) -> (Emulator, Arc<Mutex<Vec<u8>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sink = sent.clone();
        let respond: Responder = Arc::new(move |bytes| sink.lock().extend(bytes));
        (Emulator::new(columns, lines, scrollback, respond), sent)
    }

    #[test]
    fn plain_text_and_line_endings() {
        let (mut emu, _) = emulator(20, 4, 100);
        emu.feed(b"hello\r\nworld");
        assert_eq!(emu.line_text(0), "hello");
        assert_eq!(emu.line_text(1), "world");
        assert_eq!(emu.cursor(), Point::new(Line(1), Column(5)));
    }

    #[test]
    fn colours_and_attributes_are_parsed() {
        let (mut emu, _) = emulator(20, 4, 100);
        emu.feed(b"\x1b[1;31mR\x1b[0m\x1b[38;2;1;2;3mT");
        let grid = emu.term().grid();
        let red = &grid[Line(0)][Column(0)];
        assert!(red.flags.contains(alacritty_terminal::term::cell::Flags::BOLD));
        let (fg, _) = colors::cell_colors(red, &Theme::default(), emu.term().colors());
        assert_eq!(fg, colors::ANSI[9]);
        let (fg, _) = colors::cell_colors(&grid[Line(0)][Column(1)], &Theme::default(), emu.term().colors());
        assert_eq!(fg, egui::Color32::from_rgb(1, 2, 3));
    }

    #[test]
    fn answers_cursor_position_reports() {
        let (mut emu, sent) = emulator(20, 4, 100);
        emu.feed(b"ab\x1b[6n");
        assert_eq!(sent.lock().as_slice(), b"\x1b[1;3R");
    }

    #[test]
    fn application_cursor_mode_tracks_decckm() {
        let (mut emu, _) = emulator(20, 4, 100);
        assert!(!emu.application_cursor_keys());
        emu.feed(b"\x1b[?1h");
        assert!(emu.application_cursor_keys());
        emu.feed(b"\x1b[?1l");
        assert!(!emu.application_cursor_keys());
    }

    #[test]
    fn cursor_visibility_tracks_dectcem() {
        let (mut emu, _) = emulator(20, 4, 100);
        assert!(emu.cursor_visible());
        emu.feed(b"\x1b[?25l");
        assert!(!emu.cursor_visible());
    }

    fn fill(emu: &mut Emulator, count: usize) {
        for i in 0..count {
            emu.feed(format!("line {i}\r\n").as_bytes());
        }
    }

    #[test]
    fn scrollback_is_kept_and_limited() {
        let (mut emu, _) = emulator(20, 4, 10);
        fill(&mut emu, 30);
        assert_eq!(emu.history_size(), 10);
        assert_eq!(emu.display_offset(), 0);

        assert!(emu.scroll_by(3));
        assert_eq!(emu.display_offset(), 3);
        assert!(emu.scroll_by(100));
        assert_eq!(emu.display_offset(), 10);
        assert!(!emu.scroll_by(1)); // nothing older left

        emu.scroll_to(0);
        assert_eq!(emu.display_offset(), 0);
        emu.scroll_to(4);
        assert_eq!(emu.line_text(0), "line 23");
    }

    #[test]
    fn selection_reaches_into_scrollback() {
        let (mut emu, _) = emulator(20, 4, 100);
        fill(&mut emu, 10);
        // Screen now shows line 7..9 and the empty prompt row; select from
        // the first history line down to the screen.
        emu.scroll_to(3);
        let start = emu.viewport_to_point(0, 0);
        emu.start_selection(start, Side::Left, false);
        emu.scroll_to(0);
        let end = emu.viewport_to_point(1, 19);
        emu.update_selection(end, Side::Right);
        assert_eq!(emu.selection_text().unwrap(), "line 4\nline 5\nline 6\nline 7\nline 8");
    }

    #[test]
    fn line_selection_and_select_all() {
        let (mut emu, _) = emulator(20, 3, 100);
        emu.feed(b"one\r\ntwo   \r\nthree");
        let point = emu.viewport_to_point(1, 5);
        emu.start_selection(point, Side::Left, true);
        assert_eq!(emu.selection_text().unwrap(), "two");

        emu.select_all();
        assert_eq!(emu.selection_text().unwrap(), "one\ntwo\nthree");
        emu.clear_selection();
        assert!(emu.selection_text().is_none());
    }

    #[test]
    fn resize_changes_geometry() {
        let (mut emu, _) = emulator(20, 4, 100);
        assert!(emu.resize(40, 10));
        assert_eq!((emu.columns(), emu.lines()), (40, 10));
        assert!(!emu.resize(40, 10));
        emu.resize(0, 0);
        assert_eq!((emu.columns(), emu.lines()), (MIN_COLUMNS, MIN_LINES));
    }

    #[test]
    fn clear_keeps_the_prompt_and_the_history() {
        let (mut emu, _) = emulator(20, 4, 100);
        fill(&mut emu, 10);
        emu.feed(b"Switch#sh");
        let history = emu.history_size();
        emu.clear();
        assert_eq!(emu.screen_text().trim(), "Switch#sh");
        assert_eq!(emu.cursor(), Point::new(Line(0), Column(9)));
        // The lines that were on screen went into the history.
        assert_eq!(emu.history_size(), history + 3);
        emu.scroll_to(1);
        assert_eq!(emu.line_text(0), "line 9");
        emu.reset();
        assert_eq!(emu.history_size(), 0);
    }
}
