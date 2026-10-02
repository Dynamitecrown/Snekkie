//! Finding text in a terminal's output: the screen and its scrollback.
//!
//! Text is matched a logical line at a time: rows the terminal wrapped are
//! joined, as copying joins them, and trailing blanks are dropped. Matching
//! uses the `regex` crate, so `^`, `$` and `\b` mean what they do anywhere
//! else, and a match never spans two lines.
//!
//! A history can hold hundreds of thousands of lines, and scanning that
//! takes far longer than a frame. Counting and stepping between matches are
//! done a slice at a time (see [`SLICE_CELLS`]), each slice under one hold
//! of the emulator's lock, so neither the UI nor the connection feeding the
//! terminal waits long.

use std::cmp::Ordering;

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};
use regex::{Regex, RegexBuilder};

use super::emulator::Emulator;

/// About how many cells one slice of a long search reads before letting go
/// of the emulator. A few milliseconds' work in a release build.
pub const SLICE_CELLS: usize = 400_000;

/// What to look for.
#[derive(Clone, Debug)]
pub struct Pattern {
    regex: Regex,
}

impl Pattern {
    /// `query` is literal text unless `regex` is set. Matching ignores case
    /// unless `match_case` is set. Errors are short enough to show inline.
    pub fn new(query: &str, match_case: bool, regex: bool) -> Result<Pattern, String> {
        let source = if regex { query.to_string() } else { regex::escape(query) };
        RegexBuilder::new(&source).case_insensitive(!match_case).build().map(|regex| Pattern { regex }).map_err(
            |error| match error {
                regex::Error::CompiledTooBig(_) => "Pattern is too complex".to_string(),
                // The full message repeats the pattern with a caret under the
                // problem; its last line says what the problem is.
                error => {
                    let text = error.to_string();
                    let reason = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("invalid pattern");
                    format!("Invalid regex: {}", reason.trim().trim_start_matches("error: "))
                }
            },
        )
    }
}

/// One match: its first and last cell, inclusive. Lines below 0 are in the
/// history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub start: Point,
    pub end: Point,
}

impl Found {
    /// The columns this match covers on a row, if any.
    pub fn columns_on(&self, line: Line, last_column: Column) -> Option<(usize, usize)> {
        if line < self.start.line || line > self.end.line {
            return None;
        }
        let first = if line == self.start.line { self.start.column.0 } else { 0 };
        let last = if line == self.end.line { self.end.column.0 } else { last_column.0 };
        Some((first, last))
    }
}

/// Reuses buffers between logical lines.
#[derive(Default)]
struct Scratch {
    text: String,
    /// Byte offset in `text` where each cell's text starts, and the cell.
    cells: Vec<(usize, Point)>,
}

/// The first row of the logical line `line` belongs to.
fn line_start(grid: &Grid<Cell>, mut line: Line) -> Line {
    let last = grid.last_column();
    while line > grid.topmost_line() && grid[line - 1i32][last].flags.contains(Flags::WRAPLINE) {
        line -= 1;
    }
    line
}

/// The last row of the logical line `line` belongs to.
fn line_end(grid: &Grid<Cell>, mut line: Line) -> Line {
    let last = grid.last_column();
    while line < grid.bottommost_line() && grid[line][last].flags.contains(Flags::WRAPLINE) {
        line += 1;
    }
    line
}

/// Matches on the logical line made of rows `first..=last`, left to right.
/// Returns how many cells were read.
fn scan_line(
    grid: &Grid<Cell>,
    pattern: &Pattern,
    first: Line,
    last: Line,
    scratch: &mut Scratch,
) -> (usize, Vec<Found>) {
    let columns = grid.columns();
    let skipped = Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
    scratch.text.clear();
    for row in first.0..=last.0 {
        let cells = &grid[Line(row)];
        for column in 0..columns {
            let cell = &cells[Column(column)];
            if cell.flags.intersects(skipped) {
                continue;
            }
            // Concealed text is drawn blank, so it isn't found either.
            if cell.flags.contains(Flags::HIDDEN) {
                scratch.text.push(' ');
                continue;
            }
            scratch.text.push(cell.c);
            if let Some(marks) = cell.zerowidth() {
                scratch.text.extend(marks);
            }
        }
    }
    let read = columns * (last.0 - first.0 + 1) as usize;
    let text = scratch.text.trim_end();
    let spans: Vec<(usize, usize)> =
        pattern.regex.find_iter(text).filter(|m| !m.is_empty()).map(|m| (m.start(), m.end())).collect();
    if spans.is_empty() {
        return (read, Vec::new());
    }

    // Only lines with matches pay for working out which cell each byte is in.
    scratch.cells.clear();
    let mut offset = 0;
    for row in first.0..=last.0 {
        let cells = &grid[Line(row)];
        for column in 0..columns {
            let cell = &cells[Column(column)];
            if cell.flags.intersects(skipped) {
                continue;
            }
            scratch.cells.push((offset, Point::new(Line(row), Column(column))));
            offset += if cell.flags.contains(Flags::HIDDEN) {
                1
            } else {
                cell.c.len_utf8() + cell.zerowidth().map_or(0, |m| m.iter().map(|c| c.len_utf8()).sum())
            };
        }
    }
    let cell_at = |byte: usize| {
        let index = scratch.cells.partition_point(|&(start, _)| start <= byte).saturating_sub(1);
        scratch.cells[index].1
    };
    let found = spans.into_iter().map(|(start, end)| Found { start: cell_at(start), end: cell_at(end - 1) }).collect();
    (read, found)
}

/// Every match on the logical lines that touch rows `first..=last`: what
/// the screen needs to show. Rows outside the grid are ignored.
pub fn matches_between(emulator: &Emulator, pattern: &Pattern, first: Line, last: Line) -> Vec<Found> {
    let grid = emulator.term().grid();
    let first = first.max(grid.topmost_line());
    let last = last.min(grid.bottommost_line());
    let mut found = Vec::new();
    let mut scratch = Scratch::default();
    let mut line = line_start(grid, first);
    while line <= last {
        let end = line_end(grid, line);
        found.extend(scan_line(grid, pattern, line, end, &mut scratch).1);
        line = end + 1;
    }
    found
}

/// Whether `found` is still exactly a match: the output may have moved or
/// changed since it was found.
pub fn still_matches(emulator: &Emulator, pattern: &Pattern, found: Found) -> bool {
    let grid = emulator.term().grid();
    let inside =
        |p: Point| grid.topmost_line() <= p.line && p.line <= grid.bottommost_line() && p.column.0 < grid.columns();
    if !inside(found.start) || !inside(found.end) || found.end < found.start {
        return false;
    }
    let start = line_start(grid, found.start.line);
    let end = line_end(grid, found.start.line);
    found.end.line <= end && scan_line(grid, pattern, start, end, &mut Scratch::default()).1.contains(&found)
}

/// Which way to step from the current match. Newer output is further down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Up, toward older output.
    Older,
    /// Down, toward newer output.
    Newer,
}

/// Looking for the next match one way from a point, a slice at a time.
#[derive(Clone, Debug)]
pub struct Find {
    direction: Direction,
    /// Matches must start before (Older) or after (Newer) this cell, unless
    /// `inclusive` lets one start right on it. Its row is a row id (see
    /// [`Emulator::row_id`]), as is `next`, so both survive output
    /// arriving between slices.
    from: (i64, Column),
    inclusive: bool,
    /// The first row of the next logical line to read.
    next: Option<i64>,
    /// Gone past one end and started again from the other.
    wrapped: bool,
    /// Cells read so far, to give up if output keeps moving things around
    /// so much that the search never gets back to where it started.
    read: usize,
}

/// What a slice of [`Find`] came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Found(Found),
    /// Nothing anywhere.
    NotFound,
    /// Not finished; call again.
    Pending,
}

impl Find {
    /// The nearest match `direction` of `from`. With `inclusive`, a match
    /// starting on `from` itself counts: that's how a new search picks the
    /// match at or above where the user was looking.
    pub fn new(emulator: &Emulator, direction: Direction, from: Point, inclusive: bool) -> Find {
        Find {
            direction,
            from: (emulator.row_id(from.line), from.column),
            inclusive,
            next: None,
            wrapped: false,
            read: 0,
        }
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Whether it went past one end of the output and round to the other.
    pub fn wrapped(&self) -> bool {
        self.wrapped
    }

    /// Read up to about `budget` cells. Output may have arrived since the
    /// last slice.
    pub fn step(&mut self, emulator: &Emulator, pattern: &Pattern, budget: usize) -> Step {
        let grid = emulator.term().grid();
        let (top, bottom) = (grid.topmost_line(), grid.bottommost_line());
        let total = grid.total_lines() * grid.columns();
        if self.read > 2 * total + budget {
            return Step::NotFound;
        }
        // Where it started has gone (a cleared or trimmed history): start
        // from the nearest end instead.
        let from = match emulator.row_line(self.from.0) {
            Some(line) => Point::new(line, self.from.1.min(grid.last_column())),
            None => match self.direction {
                Direction::Older => Point::new(bottom, grid.last_column()),
                Direction::Newer => Point::new(top, Column(0)),
            },
        };
        let origin = line_start(grid, from.line);
        let mut line = self.next.and_then(|id| emulator.row_line(id)).map_or(origin, |l| line_start(grid, l));
        let mut scratch = Scratch::default();
        let mut read = 0;
        loop {
            let end = line_end(grid, line);
            let (cells, found) = scan_line(grid, pattern, line, end, &mut scratch);
            read += cells;
            self.read += cells;
            // On the starting line, the first time only the matches before
            // (or after) `from` count; once round past the end, the rest do.
            let wanted = |f: &Found| {
                if line != origin {
                    return true;
                }
                let at = f.start == from && self.inclusive != self.wrapped;
                match (self.direction, self.wrapped) {
                    (Direction::Older, false) | (Direction::Newer, true) => f.start < from || at,
                    (Direction::Newer, false) | (Direction::Older, true) => f.start > from || at,
                }
            };
            let pick = match self.direction {
                Direction::Older => found.into_iter().rev().find(wanted),
                Direction::Newer => found.into_iter().find(wanted),
            };
            if let Some(found) = pick {
                return Step::Found(found);
            }
            if line == origin && self.wrapped {
                return Step::NotFound;
            }
            // On to the next logical line, round past the end if need be.
            line = match self.direction {
                Direction::Older if line > top => line_start(grid, line - 1),
                Direction::Newer if end < bottom => end + 1,
                Direction::Older => {
                    self.wrapped = true;
                    line_start(grid, bottom)
                }
                Direction::Newer => {
                    self.wrapped = true;
                    top
                }
            };
            if read >= budget {
                self.next = Some(emulator.row_id(line));
                return Step::Pending;
            }
        }
    }
}

/// Counting every match, and how many come before the current one, a slice
/// at a time.
#[derive(Clone, Debug)]
pub struct Count {
    /// The content this count is of; any change means starting again.
    revision: u64,
    next: Option<Line>,
    total: usize,
    before: usize,
    focus_seen: bool,
}

/// A finished count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tally {
    pub total: usize,
    /// 1-based position of the current match, if it was among them.
    pub index: Option<usize>,
}

impl Count {
    pub fn new(emulator: &Emulator) -> Count {
        Count { revision: emulator.revision(), next: None, total: 0, before: 0, focus_seen: false }
    }

    /// Whether this count is of what the emulator holds now.
    pub fn is_current(&self, emulator: &Emulator) -> bool {
        self.revision == emulator.revision()
    }

    /// Read up to about `budget` cells; the tally once everything has been
    /// read. Restarts by itself if the output changed since the last slice.
    pub fn step(
        &mut self,
        emulator: &Emulator,
        pattern: &Pattern,
        focus: Option<Found>,
        budget: usize,
    ) -> Option<Tally> {
        if !self.is_current(emulator) {
            *self = Count::new(emulator);
        }
        let grid = emulator.term().grid();
        let mut line = self.next.unwrap_or(grid.topmost_line());
        let mut scratch = Scratch::default();
        let mut read = 0;
        while line <= grid.bottommost_line() {
            if read >= budget {
                self.next = Some(line);
                return None;
            }
            let end = line_end(grid, line);
            let (cells, found) = scan_line(grid, pattern, line, end, &mut scratch);
            read += cells;
            self.total += found.len();
            if let Some(focus) = focus {
                for f in &found {
                    match f.start.cmp(&focus.start) {
                        Ordering::Less => self.before += 1,
                        Ordering::Equal if *f == focus => self.focus_seen = true,
                        _ => {}
                    }
                }
            }
            line = end + 1;
        }
        self.next = Some(line);
        Some(Tally { total: self.total, index: self.focus_seen.then_some(self.before + 1) })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::terminal::emulator::Responder;

    fn emulator(columns: usize, lines: usize, scrollback: usize) -> Emulator {
        let respond: Responder = Arc::new(|_| {});
        Emulator::new(columns, lines, scrollback, respond)
    }

    fn pattern(query: &str) -> Pattern {
        Pattern::new(query, false, false).unwrap()
    }

    fn at(line: i32, column: usize) -> Point {
        Point::new(Line(line), Column(column))
    }

    fn all(emu: &Emulator, pattern: &Pattern) -> Vec<Found> {
        let grid = emu.term().grid();
        matches_between(emu, pattern, grid.topmost_line(), grid.bottommost_line())
    }

    /// Runs a find to the end in slices of `budget` cells.
    fn find(emu: &Emulator, pattern: &Pattern, direction: Direction, from: Point, budget: usize) -> Option<Found> {
        let mut find = Find::new(emu, direction, from, false);
        for _ in 0..10_000 {
            match find.step(emu, pattern, budget) {
                Step::Found(found) => return Some(found),
                Step::NotFound => return None,
                Step::Pending => {}
            }
        }
        panic!("the find never finished");
    }

    fn count(emu: &Emulator, pattern: &Pattern, focus: Option<Found>, budget: usize) -> Tally {
        let mut count = Count::new(emu);
        (0..10_000).find_map(|_| count.step(emu, pattern, focus, budget)).expect("the count never finished")
    }

    #[test]
    fn literal_text_ignores_case_unless_asked() {
        let mut emu = emulator(40, 4, 100);
        emu.feed(b"interface Gi1/0/1\r\nINTERFACE gi1/0/2\r\n(a+b)");
        assert_eq!(all(&emu, &pattern("interface")).len(), 2);
        let exact = Pattern::new("INTERFACE", true, false).unwrap();
        assert_eq!(all(&emu, &exact), [Found { start: at(1, 0), end: at(1, 8) }]);
        // Literal text: the brackets and plus are just characters.
        assert_eq!(all(&emu, &pattern("(a+b)")), [Found { start: at(2, 0), end: at(2, 4) }]);
    }

    #[test]
    fn regular_expressions_and_readable_errors() {
        let mut emu = emulator(40, 4, 100);
        emu.feed(b"ip address 192.0.2.1 255.255.255.0\r\nup up\r\naaa");
        let ip = Pattern::new(r"\b\d+\.\d+\.\d+\.\d+\b", false, true).unwrap();
        assert_eq!(all(&emu, &ip).len(), 2);
        // ^ and $ mean the start and end of the line, not of each try.
        let start = Pattern::new("^a", false, true).unwrap();
        assert_eq!(all(&emu, &start), [Found { start: at(2, 0), end: at(2, 0) }]);
        let end = Pattern::new("up$", false, true).unwrap();
        assert_eq!(all(&emu, &end), [Found { start: at(1, 3), end: at(1, 4) }]);
        // Patterns that only match empty text find nothing, and don't hang.
        assert!(all(&emu, &Pattern::new("x*", false, true).unwrap()).is_empty());

        let error = Pattern::new("(ab", false, true).unwrap_err();
        assert!(error.starts_with("Invalid regex: ") && error.contains("unclosed group"), "{error}");
        assert!(!error.contains('\n'));
        // The same text is fine as a literal.
        assert!(Pattern::new("(ab", false, false).is_ok());
    }

    #[test]
    fn matches_cross_wrapped_rows_but_not_line_breaks() {
        let mut emu = emulator(10, 5, 100);
        // "GigabitEthernet" wraps after ten columns.
        emu.feed(b"x GigabitEthernet1/0/1\r\nGigabit\r\nEthernet");
        let found = all(&emu, &pattern("gigabitethernet1/0/1"));
        assert_eq!(found, [Found { start: at(0, 2), end: at(2, 1) }]);
        // Separate lines don't join up.
        assert_eq!(all(&emu, &pattern("gigabitethernet")).len(), 1);
        assert_eq!(found[0].columns_on(Line(1), Column(9)), Some((0, 9)));
        assert_eq!(found[0].columns_on(Line(3), Column(9)), None);
    }

    #[test]
    fn wide_and_combining_characters_map_to_their_cells() {
        let mut emu = emulator(20, 3, 100);
        emu.feed("名前: 界e\u{301} end".as_bytes());
        // Each wide character is two cells; matches report the wide cell.
        assert_eq!(all(&emu, &pattern("界")), [Found { start: at(0, 6), end: at(0, 6) }]);
        assert_eq!(all(&emu, &pattern("前:")), [Found { start: at(0, 2), end: at(0, 4) }]);
        // A combining accent belongs to its letter's cell.
        assert_eq!(all(&emu, &pattern("e\u{301} end")), [Found { start: at(0, 8), end: at(0, 12) }]);
    }

    #[test]
    fn a_wide_character_pushed_to_the_next_row_still_matches() {
        let mut emu = emulator(5, 3, 100);
        // The fourth and fifth columns can't hold 界, so it wraps whole.
        emu.feed("abcd界x".as_bytes());
        assert_eq!(all(&emu, &pattern("d界x")), [Found { start: at(0, 3), end: at(1, 2) }]);
    }

    #[test]
    fn steps_older_and_newer_through_history_and_wraps_round() {
        let mut emu = emulator(20, 4, 100);
        for i in 0..20 {
            emu.feed(format!("line {i}{}\r\n", if i % 5 == 0 { " match" } else { "" }).as_bytes());
        }
        // Matches on lines 0, 5, 10, 15; the screen shows 17..19 and a prompt.
        let found = all(&emu, &pattern("match"));
        assert_eq!(found.len(), 4);
        let bottom = at(3, 0);
        assert_eq!(find(&emu, &pattern("match"), Direction::Older, bottom, SLICE_CELLS), Some(found[3]));
        assert_eq!(find(&emu, &pattern("match"), Direction::Older, found[3].start, SLICE_CELLS), Some(found[2]));
        assert_eq!(find(&emu, &pattern("match"), Direction::Newer, found[2].start, SLICE_CELLS), Some(found[3]));
        // Off either end and round to the other.
        assert_eq!(find(&emu, &pattern("match"), Direction::Older, found[0].start, SLICE_CELLS), Some(found[3]));
        assert_eq!(find(&emu, &pattern("match"), Direction::Newer, found[3].start, SLICE_CELLS), Some(found[0]));
        assert_eq!(find(&emu, &pattern("nothing"), Direction::Older, bottom, SLICE_CELLS), None);
    }

    #[test]
    fn several_matches_on_one_line_step_in_order() {
        let mut emu = emulator(40, 3, 100);
        emu.feed(b"up up up\r\n");
        let found = all(&emu, &pattern("up"));
        assert_eq!(found.len(), 3);
        let p = pattern("up");
        assert_eq!(find(&emu, &p, Direction::Older, found[2].start, SLICE_CELLS), Some(found[1]));
        assert_eq!(find(&emu, &p, Direction::Older, found[1].start, SLICE_CELLS), Some(found[0]));
        assert_eq!(find(&emu, &p, Direction::Older, found[0].start, SLICE_CELLS), Some(found[2]));
        assert_eq!(find(&emu, &p, Direction::Newer, found[2].start, SLICE_CELLS), Some(found[0]));
        // The only match finds itself after going all the way round.
        let one = all(&emu, &pattern("up up up"));
        assert_eq!(find(&emu, &pattern("up up up"), Direction::Older, one[0].start, 1), Some(one[0]));
        // A new search can stop on a match right where it starts.
        let mut first = Find::new(&emu, Direction::Older, found[1].start, true);
        assert_eq!(first.step(&emu, &p, SLICE_CELLS), Step::Found(found[1]));
    }

    #[test]
    fn slices_of_any_size_give_the_same_answers() {
        let mut emu = emulator(12, 4, 500);
        for i in 0..120 {
            // Some lines wrap, so slices end in the middle of nothing.
            let text = if i % 7 == 0 { format!("{i} needle in a long wrapped line\r\n") } else { format!("{i}\r\n") };
            emu.feed(text.as_bytes());
        }
        let p = pattern("needle");
        let found = all(&emu, &p);
        assert_eq!(found.len(), 18);
        for budget in [1, 12, 50, SLICE_CELLS] {
            assert_eq!(count(&emu, &p, Some(found[4]), budget), Tally { total: 18, index: Some(5) }, "{budget}");
            assert_eq!(find(&emu, &p, Direction::Older, found[4].start, budget), Some(found[3]), "{budget}");
            assert_eq!(find(&emu, &p, Direction::Newer, found[4].start, budget), Some(found[5]), "{budget}");
        }
        assert_eq!(count(&emu, &p, None, 10), Tally { total: 18, index: None });
    }

    #[test]
    fn a_count_starts_again_when_output_arrives() {
        let mut emu = emulator(20, 4, 100);
        for _ in 0..30 {
            emu.feed(b"match\r\n");
        }
        let p = pattern("match");
        let mut count = Count::new(&emu);
        assert_eq!(count.step(&emu, &p, None, 20), None);
        emu.feed(b"match\r\n");
        assert!(!count.is_current(&emu));
        let tally = (0..1000).find_map(|_| count.step(&emu, &p, None, 20)).unwrap();
        assert_eq!(tally.total, 31);
    }

    #[test]
    fn a_find_carries_on_from_the_same_row_after_output_arrives() {
        let mut emu = emulator(20, 4, 1000);
        emu.feed(b"needle\r\n");
        for i in 0..200 {
            emu.feed(format!("filler {i}\r\n").as_bytes());
        }
        let p = pattern("needle");
        let mut find = Find::new(&emu, Direction::Older, at(3, 0), false);
        assert_eq!(find.step(&emu, &p, 100), Step::Pending);
        for i in 0..10 {
            emu.feed(format!("more {i}\r\n").as_bytes());
        }
        let found = loop {
            match find.step(&emu, &p, 100) {
                Step::Found(found) => break found,
                Step::Pending => {}
                Step::NotFound => panic!("lost the needle"),
            }
        };
        assert_eq!(found, all(&emu, &p)[0]);
    }

    #[test]
    fn stored_matches_are_checked_before_use() {
        let mut emu = emulator(20, 4, 100);
        emu.feed(b"needle\r\n");
        let p = pattern("needle");
        let found = all(&emu, &p)[0];
        assert!(still_matches(&emu, &p, found));
        assert!(!still_matches(&emu, &pattern("other"), found));
        emu.clear();
        assert!(!still_matches(&emu, &p, found));
        // Far outside the grid is simply not a match.
        assert!(!still_matches(&emu, &p, Found { start: at(-500, 0), end: at(-500, 5) }));
        assert!(!still_matches(&emu, &p, Found { start: at(0, 50), end: at(0, 55) }));
    }

    #[test]
    fn row_ids_follow_rows_into_the_history() {
        let mut emu = emulator(20, 4, 10);
        emu.feed(b"first\r\n");
        let id = emu.row_id(Line(0));
        // Not full yet: the history's growth says how far rows moved.
        for i in 0..5 {
            emu.feed(format!("line {i}\r\n").as_bytes());
        }
        let line = emu.row_line(id).unwrap();
        assert_eq!(emu.term().grid()[line][Column(0)].c, 'f');
        // Scrolled back, the view's offset says how far rows moved, even
        // once the history is full.
        for i in 0..6 {
            emu.feed(format!("more {i}\r\n").as_bytes());
        }
        emu.scroll_to(4);
        let id = emu.row_id(Line(-4));
        let before = emu.term().grid()[Line(-4)][Column(5)].c;
        emu.feed(b"x\r\ny\r\n");
        let line = emu.row_line(id).unwrap();
        assert_eq!(emu.term().grid()[line][Column(5)].c, before);
        // Rows that leave the history have no line any more.
        assert_eq!(emu.row_line(emu.row_id(emu.term().grid().topmost_line()) - 1), None);
    }

    #[test]
    fn full_screen_programs_are_searched_on_their_own_screen() {
        let mut emu = emulator(20, 4, 100);
        emu.feed(b"shell needle\r\n");
        emu.feed(b"\x1b[?1049h\x1b[Heditor text");
        assert!(all(&emu, &pattern("needle")).is_empty());
        assert_eq!(all(&emu, &pattern("editor")).len(), 1);
        emu.feed(b"\x1b[?1049l");
        assert_eq!(all(&emu, &pattern("needle")).len(), 1);
    }
}
