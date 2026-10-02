//! Terminal animations: the bookkeeping behind what moves, fades and
//! sparkles as you type and as output arrives. Nothing is painted here.
//! Each frame the terminal view hands over a copy of the live screen, then
//! asks cell by cell how to draw it.
//!
//! Everything is in cells (rows and columns, fractional while something
//! moves), so fonts don't matter here. Positions are on the live screen,
//! row 0 being its top line whatever the scrollback view shows: the view
//! draws live row `r` at viewport row `r + display_offset`, moved down by
//! [`Animator::scroll_offset`] rows. That goes for cells, line marks, the
//! cursor and particles alike.
//!
//! Nothing here touches what the emulator holds or what the device is
//! sent. Output still waiting to be revealed is only left out when drawing.

use std::collections::VecDeque;
use std::f32::consts::{FRAC_PI_2, TAU};

use egui::{Color32, Vec2, vec2};

use crate::settings::{
    Animations, CursorBlink, CursorMotion, KeystrokeBurst, NewLines, NewText, Reveal, Scrolling, Shake, TypedText,
};

/// Stands in for "never happened" in timestamps: everything is long over.
const NEVER: f64 = f64::NEG_INFINITY;

/// How long after the last keystroke its echo is still expected.
const ECHO_WINDOW: f64 = 1.0;
/// New characters an echo may bring beyond the keys typed: a device
/// redraws the rest of the line when you insert in the middle of it.
const ECHO_SLACK: usize = 2;

/// Output held back by a reveal style is fully drawn this soon after it
/// arrived, however much there is.
const REVEAL_CAP: f64 = 0.5;
const TYPEWRITER_STEP: f64 = 0.0025;
const WORD_STEP: f64 = 0.03;
const LINE_STEP: f64 = 0.04;

const GLIDE: f64 = 0.09;
const SMEAR: f64 = 0.12;
const GHOST: f64 = 0.25;
/// How far apart in time the afterimages along one jump were left.
const GHOST_STAGGER: f64 = 0.03;
const MAX_GHOSTS: usize = 6;
/// A cursor jumping further than this many rows (a clear, a full-screen
/// app starting) just moves.
const SNAP_ROWS: f32 = 8.0;
/// Angular frequency (rad/s) and damping ratio.
const CURSOR_SPRING: (f32, f32) = (40.0, 0.55);
const SCROLL_SPRING: (f32, f32) = (28.0, 0.6);
/// Closer to rest than this, in cells, and stopping dead can't be seen.
const SETTLED: f32 = 0.004;

const MAX_PARTICLES: usize = 300;
/// About how many columns fit in a cell's height, so particles fly as far
/// sideways as they do up.
const ASPECT: f32 = 2.0;
const RIPPLE_RADIUS: f32 = 1.2;

const SHAKE: f64 = 0.15;

/// Width of the Shimmer band, in columns.
pub const SHIMMER_WIDTH: f32 = 6.0;

/// Glyphs rising or dropping into place move in steps of this many to a
/// cell (about a pixel), so cells shown a moment apart (a typewriter
/// reveal) are at the same height and can share a run of text.
const OFFSET_STEPS: f32 = 16.0;

const DECODE_GLYPHS: &[char] = &[
    '!', '<', '>', '-', '_', '\\', '/', '[', ']', '{', '}', '=', '+', '*', '^', '?', '#', '$', '%', '&', '@', '0', '1',
];
/// Decode picks a new random glyph this often.
const DECODE_TICK: f32 = 0.04;

const CONFETTI: [Color32; 5] = [
    Color32::from_rgb(0xff, 0x5f, 0x57),
    Color32::from_rgb(0xff, 0xc8, 0x3d),
    Color32::from_rgb(0x3d, 0xdc, 0x84),
    Color32::from_rgb(0x4a, 0xa3, 0xff),
    Color32::from_rgb(0xd3, 0x5c, 0xff),
];
const EMBERS: [Color32; 3] =
    [Color32::from_rgb(0xff, 0x9a, 0x3c), Color32::from_rgb(0xff, 0x5a, 0x1f), Color32::from_rgb(0xff, 0xd1, 0x66)];

/// The live screen as it is this frame, whatever the scrollback view shows.
pub struct Snapshot<'a> {
    /// Grid lines 0.. of the live screen, row after row, `columns` cells
    /// each. A blank or concealed cell is ' '.
    pub cells: &'a [char],
    pub columns: usize,
    /// Lines of scrollback above the screen.
    pub history_size: usize,
    /// Lines the view is scrolled back from live.
    pub display_offset: usize,
    /// Row and column on the live screen.
    pub cursor: (usize, usize),
    /// A full-screen app (vim, htop...) has the alternate screen up.
    pub alt_screen: bool,
}

impl Snapshot<'_> {
    pub fn lines(&self) -> usize {
        self.cells.len().checked_div(self.columns).unwrap_or(0)
    }
}

/// How to draw a cell that is animating. A cell that isn't gets `None`
/// from [`Animator::cell`] and is drawn as usual.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellLook {
    /// Glyph opacity: mix the foreground toward the cell's background by
    /// `1 - alpha`.
    pub alpha: f32,
    /// How far the glyph is moved, in cells; positive is down.
    pub offset_y: f32,
    /// Glyph scale about the cell's centre.
    pub scale: f32,
    /// 0..1: mix the foreground toward the accent (cursor) color.
    pub accent: f32,
    /// Draw this instead of the real character (Decode).
    pub glyph: Option<char>,
    /// 0..1: opacity of an accent fill behind the cell.
    pub flash: f32,
    /// Not revealed yet: draw neither the glyph nor a non-default
    /// background.
    pub hidden: bool,
}

impl CellLook {
    pub const PLAIN: CellLook =
        CellLook { alpha: 1.0, offset_y: 0.0, scale: 1.0, accent: 0.0, glyph: None, flash: 0.0, hidden: false };
    pub const HIDDEN: CellLook = CellLook { alpha: 0.0, hidden: true, ..CellLook::PLAIN };

    /// The glyph moves, changes size or is swapped for another, so it
    /// can't share a run of text with its neighbours.
    pub fn moves(&self) -> bool {
        self.offset_y != 0.0 || self.scale != 1.0 || self.glyph.is_some()
    }
}

/// A mark on a row that just got output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineMark {
    pub style: NewLines,
    /// Progress through the mark, 0..1.
    pub t: f32,
    /// Columns the row's text spans; `end` is one past the last.
    pub start: usize,
    pub end: usize,
    /// How strongly to draw it now, the style's curve already applied: the
    /// opacity of the Glow or Flash fill, the Marker bar or the Underline,
    /// or the Shimmer band at its brightest (see [`LineMark::band`]).
    pub alpha: f32,
    /// Underline: how many columns from `start` it has swept. Shimmer: the
    /// column the band is centred on.
    pub reach: f32,
}

impl LineMark {
    fn new(style: NewLines, t: f32, start: usize, end: usize) -> LineMark {
        let width = (end - start) as f32;
        let (alpha, reach) = match style {
            NewLines::Off => (0.0, 0.0),
            NewLines::Glow => (0.22 * (1.0 - smoothstep(t)), 0.0),
            NewLines::Flash => (0.25 * (1.0 - t) * (1.0 - t), 0.0),
            NewLines::Marker => (if t < 0.5 { 1.0 } else { 1.0 - smoothstep((t - 0.5) / 0.5) }, 0.0),
            NewLines::Underline => {
                // Sweeps across in the first 40%, then fades.
                let alpha = if t < 0.4 { 1.0 } else { 1.0 - smoothstep((t - 0.4) / 0.6) };
                (alpha, width * ease_out_cubic(t / 0.4))
            }
            NewLines::Shimmer => {
                // The band starts wholly left of the text and ends wholly right of it.
                let half = SHIMMER_WIDTH / 2.0;
                (0.3, start as f32 - half + (width + 2.0 * half) * t)
            }
            NewLines::Laser => (1.0 - t, start as f32 + width * ease_out_cubic(t)),
            NewLines::Radar => (0.6 * (1.0 - t), width.min(12.0) * ease_out_quad(t)),
        };
        LineMark { style, t, start, end, alpha, reach }
    }

    /// Shimmer: how bright the band is over a column's centre, 0..alpha.
    pub fn band(&self, column: f32) -> f32 {
        let distance = (column - self.reach).abs() / (SHIMMER_WIDTH / 2.0);
        if distance >= 1.0 { 0.0 } else { self.alpha * (1.0 - smoothstep(distance)) }
    }
}

/// Where to draw the cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorLook {
    /// Top-left of the cursor, as (row, column); fractional while it moves.
    pub pos: (f32, f32),
    /// Smear: top-left of the tail. Draw the convex hull of the tail's cell
    /// and the head's.
    pub tail: Option<(f32, f32)>,
    /// Ghost: afterimages, top-left and opacity, oldest first.
    pub ghosts: Vec<((f32, f32), f32)>,
}

impl CursorLook {
    /// The cell the cursor is on, once it has come to rest on one.
    pub fn cell(&self) -> Option<(usize, usize)> {
        let (row, column) = self.pos;
        let whole = |v: f32| v > -0.01 && (v - v.round()).abs() < 0.01;
        (whole(row) && whole(column)).then(|| (row.round() as usize, column.round() as usize))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParticleColor {
    /// The theme's cursor color.
    Accent,
    Fixed(Color32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParticleShape {
    Dot,
    Square {
        angle: f32,
    },
    /// Four-pointed.
    Star {
        angle: f32,
    },
    /// An outline circle.
    Ring,
    Beam {
        angle: f32,
        length: f32,
    },
    Bolt {
        angle: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParticleLook {
    /// Centre, as (row, column).
    pub pos: (f32, f32),
    /// Radius (half the side, for a square), in cell heights.
    pub size: f32,
    pub color: ParticleColor,
    pub alpha: f32,
    pub shape: ParticleShape,
}

/// Every running animation of one terminal. Feed it the screen each frame
/// with [`Animator::update`] and each keystroke with [`Animator::keystroke`];
/// everything else only reads.
pub struct Animator {
    settings: Animations,
    pace: f64,
    /// The next update only records the screen.
    baseline: bool,
    columns: usize,
    lines: usize,
    /// The live screen as of the last update, and a hash of each row.
    screen: Vec<char>,
    hashes: Vec<RowHash>,
    /// This frame's row hashes, and each row's score for staying where it
    /// was (see [`Animator::detect_movement`]); kept to save allocating
    /// every frame.
    new_hashes: Vec<RowHash>,
    in_place: Vec<i64>,
    cells: Vec<CellState>,
    marks: Vec<Mark>,
    /// Cells that got new text this frame, in reading order.
    fresh: Vec<usize>,
    history: usize,
    display_offset: usize,
    alt_screen: bool,
    cursor_cell: (usize, usize),
    /// Latest start of any typed, output or line effect, so it's cheap to
    /// know when they have all finished.
    latest_typed: f64,
    latest_output: f64,
    latest_mark: f64,
    /// When the next piece of held-back output may show.
    reveal_clock: f64,
    /// Printable keys typed whose echo hasn't shown up yet.
    pending_echo: usize,
    last_key: f64,
    key_row: Option<usize>,
    /// Enter was typed after those keys: their echo is on `key_row` or
    /// never comes (a password), so what shows up elsewhere isn't it.
    entered: bool,
    scroll: ScrollState,
    motion: Motion,
    particles: VecDeque<Particle>,
    particles_until: f64,
    shake: ShakeState,
    rng: Rng,
}

impl Default for Animator {
    fn default() -> Self {
        Animator {
            settings: Animations::default(),
            pace: 1.0,
            baseline: true,
            columns: 0,
            lines: 0,
            screen: Vec::new(),
            hashes: Vec::new(),
            new_hashes: Vec::new(),
            in_place: Vec::new(),
            cells: Vec::new(),
            marks: Vec::new(),
            fresh: Vec::new(),
            history: 0,
            display_offset: 0,
            alt_screen: false,
            cursor_cell: (0, 0),
            latest_typed: NEVER,
            latest_output: NEVER,
            latest_mark: NEVER,
            reveal_clock: NEVER,
            pending_echo: 0,
            last_key: NEVER,
            key_row: None,
            entered: false,
            scroll: ScrollState::default(),
            motion: Motion::default(),
            particles: VecDeque::new(),
            particles_until: NEVER,
            shake: ShakeState::default(),
            rng: Rng(0x9e37_79b9_7f4a_7c15),
        }
    }
}

impl Animator {
    /// Forget everything: the next update takes the screen as it is and
    /// animates none of it. For a resize, a clear or reset, a tab shown
    /// again, or animations just turned on.
    pub fn reset(&mut self) {
        self.baseline = true;
        self.scroll.clear();
        self.motion = Motion::default();
        self.particles.clear();
        self.particles_until = NEVER;
        self.shake = ShakeState::default();
        self.pending_echo = 0;
        self.key_row = None;
        self.entered = false;
    }

    /// Compare the screen with the last frame's and start animating what
    /// changed.
    pub fn update(&mut self, now: f64, settings: &Animations, screen: &Snapshot<'_>) {
        self.settings = *settings;
        self.pace = f64::from(settings.pace());
        let (columns, lines) = (screen.columns, screen.lines());
        if !settings.enabled || lines == 0 {
            self.reset();
            return;
        }
        let cells = &screen.cells[..columns * lines];
        self.scroll.sync(settings.scrolling);
        self.expire(now);
        self.new_hashes.clear();
        self.new_hashes.extend(cells.chunks_exact(columns).map(row_hash));
        if self.baseline || columns != self.columns || lines != self.lines {
            self.rebase(now, screen, true);
            return;
        }
        if screen.alt_screen != self.alt_screen {
            // Switching screens swaps what's shown wholesale; none of it is new.
            self.rebase(now, screen, false);
            return;
        }

        // History only grows by lines scrolled off the top, so it's a floor
        // for how far the screen moved. A clear (ED 3) shrinks it: no hint.
        let hint = screen.history_size.saturating_sub(self.history);
        let movement = self.detect_movement(cells, hint, screen.cursor);
        self.carry(movement);
        self.diff(cells, movement);

        if now - self.last_key > ECHO_WINDOW {
            self.pending_echo = 0;
        }
        self.key_row = self.key_row.and_then(|row| movement.target(row));
        let mut fresh = std::mem::take(&mut self.fresh);
        if let Some(row) = self.echo_row(screen, movement, &fresh) {
            let mut typed = 0;
            // A read can carry both an echo and the device's reply. Remove
            // only the echo's cells before scheduling the output below.
            fresh.retain(|&index| {
                if index / columns != row {
                    return true;
                }
                typed += 1;
                self.cells[index] = CellState { start: now, kind: Kind::Typed, seed: self.rng.next_u32() };
                false
            });
            self.pending_echo -= typed.min(self.pending_echo);
            self.latest_typed = now;
        }
        if !fresh.is_empty() {
            if screen.alt_screen {
                fresh.iter().for_each(|&index| self.cells[index] = CellState::NONE);
            } else {
                self.schedule(now, cells, &fresh);
            }
        }
        self.fresh = fresh;
        if self.entered && Some(screen.cursor.0) != self.key_row {
            // The device has moved on to a new line (told apart above, so
            // echoes arriving with it still count): whatever it shows next
            // isn't the echo of keys typed before Enter.
            self.pending_echo = 0;
        }
        self.refresh_marks(now, cells);

        // alacritty moves the display offset along with output while you're
        // scrolled back, so the view holds still; scrolling yourself moves
        // only the display offset.
        let bumped = screen.display_offset as i64 - self.display_offset as i64;
        let shift = movement.shift as i64;
        let moved = if screen.alt_screen {
            0
        } else if movement.replaced {
            // The shift is only a guess here; the offset bump is exact.
            shift.max(bumped.max(0))
        } else if movement.whole(lines) {
            shift
        } else if self.display_offset > 0 && movement.top == 0 {
            // Only part of the screen moved, which can't be drawn sliding.
            // What's scrolled off the top still bumps the display offset
            // while you're scrolled back, holding the view still.
            bumped.clamp(0, shift.max(0))
        } else {
            0
        };
        self.scroll.push(now, moved - bumped, lines, settings.scrolling, self.pace);

        // The cursor and particles ride along with the text they were on. A
        // full-screen app scrolls regions, not the screen, so there they
        // stay put.
        let carried = (movement.top..=movement.bottom).contains(&self.cursor_cell.0);
        if movement.shift != 0 && carried && !screen.alt_screen {
            let rows = movement.shift as f32;
            self.motion.shift(rows);
            self.particles.iter_mut().for_each(|p| p.origin.0 -= rows);
        }
        let target = self.reveal_head(now).unwrap_or(screen.cursor);
        self.motion.retarget(now, cell_pos(target), self.pace);

        self.screen.copy_from_slice(cells);
        std::mem::swap(&mut self.hashes, &mut self.new_hashes);
        self.history = screen.history_size;
        self.display_offset = screen.display_offset;
        self.cursor_cell = screen.cursor;
    }

    /// A key was pressed; `printable` is how many characters it should
    /// echo (0 for Enter, arrows or a paste), and `newline` is whether it
    /// ends a line (Enter, or a paste with a line break in it). `cursor` is
    /// where the real cursor is.
    pub fn keystroke(
        &mut self,
        now: f64,
        settings: &Animations,
        printable: usize,
        newline: bool,
        cursor: (usize, usize),
    ) {
        self.settings = *settings;
        self.pace = f64::from(settings.pace());
        if !settings.enabled {
            return;
        }
        // Keys typed before Enter can still echo, late on a slow link, but
        // only on their own row (see `echo_row`). Typing again after Enter
        // starts afresh, as does waiting long enough.
        if now - self.last_key > ECHO_WINDOW || (self.entered && printable > 0) {
            self.pending_echo = 0;
            self.entered = false;
        }
        self.pending_echo += printable;
        self.entered |= newline;
        self.last_key = now;
        self.key_row = Some(cursor.0);
        // You're typing: whatever output is still held back shows now, so
        // your text doesn't land ahead of it.
        self.flush_reveal(now);

        let drawn = if self.baseline { cell_pos(cursor) } else { self.cursor(now, self.cursor_cell).pos };
        self.spawn(now, settings.keystroke_burst, (drawn.0 + 0.5, drawn.1 + 0.5));
        if settings.shake != Shake::Off {
            self.shake = ShakeState { start: now, phase: (self.rng.range(0.0, TAU), self.rng.range(0.0, TAU)) };
        }
    }

    /// How to draw a live-screen cell, or `None` to draw it as usual.
    pub fn cell(&self, now: f64, row: usize, column: usize) -> Option<CellLook> {
        if self.baseline || row >= self.lines || column >= self.columns || now >= self.cells_until() {
            return None;
        }
        let state = self.cells[row * self.columns + column];
        // In seconds at normal speed, which is what the curves are written in.
        let age = ((now - state.start) * self.pace) as f32;
        match state.kind {
            Kind::None => None,
            Kind::Typed => typed_look(self.settings.typed_text, age),
            Kind::Output if now < state.start => Some(CellLook::HIDDEN),
            Kind::Output => output_look(self.settings.new_text, age, state.seed),
        }
    }

    /// The mark on a live-screen row that just got output, if any.
    pub fn line_mark(&self, now: f64, row: usize) -> Option<LineMark> {
        let style = self.settings.new_lines;
        if self.baseline || row >= self.lines || style == NewLines::Off {
            return None;
        }
        let mark = self.marks[row];
        let t = ((now - mark.start) * self.pace) as f32 / line_duration(style);
        ((0.0..1.0).contains(&t) && mark.end > mark.first).then(|| LineMark::new(style, t, mark.first, mark.end))
    }

    /// Rows to draw everything lower than where it is on the grid; negative
    /// is higher. Rows sliding in from above come from the scrollback.
    pub fn scroll_offset(&self, now: f64) -> f32 {
        if self.baseline {
            return 0.0;
        }
        let limit = self.lines as f32;
        self.scroll.offset(now, self.settings.scrolling, self.pace).clamp(-limit, limit)
    }

    /// Where to draw the cursor. `actual` is the cursor cell the snapshot
    /// carried; while output is being revealed the cursor is drawn at the
    /// reveal head instead.
    pub fn cursor(&self, now: f64, actual: (usize, usize)) -> CursorLook {
        let mut look = CursorLook { pos: cell_pos(actual), tail: None, ghosts: Vec::new() };
        let target = match self.motion.target {
            Some(target) if !self.baseline && actual == self.cursor_cell => target,
            _ => return look,
        };
        let pace = self.pace;
        look.pos = target;
        match self.settings.cursor_motion {
            CursorMotion::Off => {}
            CursorMotion::Glide => look.pos = self.motion.glide.at(now, target, GLIDE / pace),
            CursorMotion::Spring => {
                let [row, column] = self.motion.spring;
                look.pos = (target.0 + row.state(now).0, target.1 + column.state(now).0);
            }
            CursorMotion::Smear => {
                let tail = self.motion.smear.at(now, target, SMEAR / pace);
                if (tail.0 - target.0).abs() + (tail.1 - target.1).abs() > 0.01 {
                    look.tail = Some(tail);
                }
            }
            CursorMotion::Ghost => {
                look.ghosts = self
                    .motion
                    .ghosts
                    .iter()
                    .filter_map(|&(at, left)| {
                        let u = ((now - left) / GHOST * pace) as f32;
                        (0.0..1.0).contains(&u).then_some((at, 0.55 * (1.0 - u) * (1.0 - u)))
                    })
                    .collect();
            }
        }
        look
    }

    /// The first cell of output still held back, in reading order.
    pub fn reveal_head(&self, now: f64) -> Option<(usize, usize)> {
        if self.baseline || now >= self.latest_output {
            return None;
        }
        let index = self.cells.iter().position(|s| s.kind == Kind::Output && s.start > now)?;
        Some((index / self.columns, index % self.columns))
    }

    pub fn particles(&self, now: f64) -> impl Iterator<Item = ParticleLook> {
        self.particles.iter().filter_map(move |p| p.look(now))
    }

    /// How far to move the whole terminal, in pixels.
    pub fn shake(&self, now: f64) -> Vec2 {
        let amplitude = match self.settings.shake {
            Shake::Off => return Vec2::ZERO,
            Shake::Gentle => 1.5,
            Shake::Strong => 4.0,
        };
        let age = ((now - self.shake.start) * self.pace) as f32;
        if self.baseline || !(0.0..SHAKE as f32).contains(&age) {
            return Vec2::ZERO;
        }
        let decay = amplitude * (1.0 - age / SHAKE as f32).powi(2);
        let (x, y) = self.shake.phase;
        vec2(decay * (TAU * 19.0 * age + x).sin(), 0.8 * decay * (TAU * 23.0 * age + y).sin())
    }

    /// When to draw again: `Some(0.0)` next frame, while anything is
    /// moving; `None` once everything has settled. Cursor blinking is
    /// separate: see [`blink_wake`].
    pub fn next_wake(&self, now: f64) -> Option<f64> {
        if self.baseline || !self.settings.enabled {
            return None;
        }
        let pace = self.pace;
        let busy = now < self.cells_until()
            || now < self.latest_mark + f64::from(line_duration(self.settings.new_lines)) / pace
            || self.scroll.busy(now, self.settings.scrolling, pace)
            || self.motion.busy(now, self.settings.cursor_motion, pace)
            || now < self.particles_until
            || (self.settings.shake != Shake::Off && now < self.shake.start + SHAKE / pace);
        busy.then_some(0.0)
    }

    /// Past this, no cell is animating or hidden.
    fn cells_until(&self) -> f64 {
        let typed = self.latest_typed + f64::from(typed_duration(self.settings.typed_text)) / self.pace;
        let output = self.latest_output + f64::from(new_text_duration(self.settings.new_text)) / self.pace;
        typed.max(output)
    }

    /// Take the screen as it is, animating none of it. `hard` also drops
    /// the cursor's motion.
    fn rebase(&mut self, now: f64, screen: &Snapshot<'_>, hard: bool) {
        let (columns, lines) = (screen.columns, screen.lines());
        self.columns = columns;
        self.lines = lines;
        self.screen.clear();
        self.screen.extend_from_slice(&screen.cells[..columns * lines]);
        std::mem::swap(&mut self.hashes, &mut self.new_hashes);
        self.cells.clear();
        self.cells.resize(columns * lines, CellState::NONE);
        self.marks.clear();
        self.marks.resize(lines, Mark::NONE);
        self.scroll.clear();
        self.latest_typed = NEVER;
        self.latest_output = NEVER;
        self.latest_mark = NEVER;
        self.reveal_clock = now;
        self.history = screen.history_size;
        self.display_offset = screen.display_offset;
        self.alt_screen = screen.alt_screen;
        self.cursor_cell = screen.cursor;
        self.baseline = false;
        if hard {
            self.motion.place(cell_pos(screen.cursor));
        } else {
            self.motion.retarget(now, cell_pos(screen.cursor), self.pace);
        }
    }

    fn expire(&mut self, now: f64) {
        if now >= self.particles_until {
            self.particles.clear();
        } else {
            self.particles.retain(|p| p.alive(now));
        }
        let ghost = GHOST / self.pace;
        self.motion.ghosts.retain(|&(_, left)| now < left + ghost);
        self.scroll.expire(now, self.pace);
    }

    /// How the live screen's text moved since the last frame.
    ///
    /// alacritty's damage tracking can't say: any scroll damages the whole
    /// screen. So line the rows up against last frame's instead. Text either
    /// stays put or moves with a band of rows: the whole screen, or a scroll
    /// region with rows held in place around it (a status line, a pager's
    /// prompt, a progress display redrawn in place). Every shift up is tried
    /// with the band that explains it best, and the best of those is taken if
    /// it explains more rows of text than leaving every row where it was
    /// does, and more than it loses. Only if none does are shifts down tried
    /// the same way: output goes up, and a log that repeats every few lines
    /// looks as much like it going down by the rest of the cycle.
    ///
    /// The history only grows by rows scrolled off the top of the screen, so
    /// growth pins the band to the top, is a floor for the shift, and rules
    /// out nothing having moved. It stops growing once the scrollback is full
    /// (or is 0), and undercounts on the frame that fills it, which is why
    /// it's only a floor.
    fn detect_movement(&mut self, cells: &[char], hint: usize, cursor: (usize, usize)) -> Movement {
        let lines = self.lines;
        if hint >= lines {
            return Movement::replaced(hint, lines);
        }
        let still = Movement::still(lines);
        if hint == 0 && self.hashes == self.new_hashes {
            return still;
        }
        self.score_in_place(cells, cursor.0);
        let kept = self.tally(cells, still).matched;
        // A prompt or a separator line can match by chance after a burst; a
        // real move keeps more of the old text than it loses.
        let real = |tally: Tally| tally.matched > tally.lost && (hint > 0 || tally.matched > kept);

        // Ties go to the smaller shift, the hint first.
        let (mut best, mut best_score, mut at_hint) = (still, i64::MIN, still);
        for distance in hint.max(1)..lines {
            let (score, movement) = self.band(cells, distance as isize, hint > 0);
            if distance == hint {
                at_hint = movement;
            }
            if score > best_score {
                (best, best_score) = (movement, score);
            }
        }
        if real(self.tally(cells, best)) {
            return best;
        }

        // Text seldom moves down (a pager going back), and the history says
        // when it's moved up, so that's only without it. One row lining up
        // with one further up is more likely a prompt repeated after a burst,
        // so a shift down takes two.
        if hint == 0 {
            (best, best_score) = (still, i64::MIN);
            for distance in 1..lines {
                let (score, movement) = self.band(cells, -(distance as isize), false);
                if score > best_score && self.tally(cells, movement).moved >= 2 {
                    (best, best_score) = (movement, score);
                }
            }
            if real(self.tally(cells, best)) {
                return best;
            }
        }

        // Nothing lines up. With the cursor at the bottom and most of the
        // old text replaced with other text, more than a screenful went by;
        // but without the history to say so, not if the cursor's row or a
        // fair part of the rest stayed put: that's the screen redrawn in
        // place, like a pager's page.
        let text = self.hashes.iter().filter(|h| !h.blank).count();
        let in_place = !self.fits(cells, cursor.0, cursor.0) && 4 * kept <= text;
        if cursor.0 + 1 == lines && self.overwritten(cells, hint) && (hint > 0 || in_place) {
            Movement::replaced(lines, lines)
        } else {
            // Text landing on an otherwise blank screen, or redrawn in place.
            at_hint
        }
    }

    /// Score each row for staying where it was, for [`Animator::band`]. A
    /// row changed only a little (a progress line ticking over) scores a
    /// touch more than one rewritten, which settles which rows a scroll
    /// region left alone when nothing else does. Not on the cursor's row,
    /// though: new output lands there, and can look like what it replaced.
    fn score_in_place(&mut self, cells: &[char], cursor_row: usize) {
        self.in_place.clear();
        for row in 0..self.lines {
            let mut score = self.score(cells, row, row);
            if score < 0 && row != cursor_row && !self.new_hashes[row].blank && self.near(cells, row) {
                score += 1;
            }
            self.in_place.push(score);
        }
    }

    /// A row's score for showing what row `source` showed last frame. A row
    /// of text accounted for outweighs any number of rows of old text lost,
    /// which outweigh any number of small changes.
    fn score(&self, cells: &[char], row: usize, source: usize) -> i64 {
        let weight = self.lines as i64 + 1;
        if self.fits(cells, row, source) {
            weight * weight
        } else if self.hashes[source].blank {
            0
        } else {
            -weight
        }
    }

    /// The band of rows that best explains the screen as having moved
    /// `shift` rows up (negative: down), with the rest staying put, and its
    /// score: the sum of each row's, a row that came into the band scoring
    /// 0. `pinned` keeps the band at the top of the screen.
    fn band(&self, cells: &[char], shift: isize, pinned: bool) -> (i64, Movement) {
        let lines = self.lines;
        let distance = shift.unsigned_abs();
        // Text moving down is text moving up on the screen turned upside down.
        let flip = |row: usize| if shift > 0 { row } else { lines - 1 - row };
        let stay = |row: usize| self.in_place[flip(row)];
        let total: i64 = self.in_place.iter().sum();
        // For each bottom row in turn: rows top..=bottom - distance take the
        // text from `distance` rows below, and the `distance` rows under
        // them (`arrived`) have new text. `gained` is what moving rows up to
        // the last one gains over leaving them, and `least` the lowest it
        // got before `top`, the best place to start the band.
        let mut arrived: i64 = (1..=distance).map(stay).sum();
        let (mut gained, mut least, mut top) = (0, 0, 0);
        let mut best = (i64::MIN, 0, 0);
        for bottom in distance..lines {
            let last = bottom - distance;
            if !pinned && gained < least {
                (least, top) = (gained, last);
            }
            gained += self.score(cells, flip(last), flip(last + distance)) - stay(last);
            if bottom > distance {
                arrived += stay(bottom) - stay(last);
            }
            // Ties go to the longer band: blank rows look the same moved or not.
            let score = total + gained - least - arrived;
            if score >= best.0 {
                best = (score, top, bottom);
            }
        }
        let (score, top, bottom) = best;
        let (top, bottom) = if shift > 0 { (top, bottom) } else { (lines - 1 - bottom, lines - 1 - top) };
        (score, Movement { shift, top, bottom, replaced: false })
    }

    /// The rows of text a movement accounts for, and the rows of old text
    /// it loses.
    fn tally(&self, cells: &[char], movement: Movement) -> Tally {
        let mut tally = Tally::default();
        for row in 0..self.lines {
            let Some(source) = movement.source(row) else {
                continue;
            };
            if self.fits(cells, row, source) {
                tally.matched += 1;
                tally.moved += usize::from(source != row);
            } else if !self.hashes[source].blank {
                tally.lost += 1;
            }
        }
        tally
    }

    /// Whether row `row` shows the text row `source` showed last frame, or
    /// that and more after it: a line still being written, perhaps as it
    /// scrolled. Blank rows would all match each other, so they never do.
    fn fits(&self, cells: &[char], row: usize, source: usize) -> bool {
        let (new, old) = (self.new_hashes[row], self.hashes[source]);
        if new.blank {
            return false;
        }
        if new.hash == old.hash {
            return true;
        }
        let columns = self.columns;
        !old.blank
            && old.end < new.end
            && cells[row * columns..][..old.end] == self.screen[source * columns..][..old.end]
    }

    /// Whether a row's text changed only a little where it is: two thirds of
    /// the columns with text on either frame have the same character on both.
    fn near(&self, cells: &[char], row: usize) -> bool {
        let columns = self.columns;
        let (old, new) = (&self.screen[row * columns..][..columns], &cells[row * columns..][..columns]);
        let (mut same, mut text) = (0, 0);
        for (&o, &n) in old.iter().zip(new) {
            if !is_blank(o) || !is_blank(n) {
                text += 1;
                same += usize::from(o == n);
            }
        }
        3 * same >= 2 * text
    }

    /// Whether a full screen replaced most of the old text with other text:
    /// what's left when more than a screenful goes by and the history can't
    /// say so.
    fn overwritten(&self, cells: &[char], shift: usize) -> bool {
        let columns = self.columns;
        let filled = self.new_hashes.iter().filter(|h| !h.blank).count();
        let (mut text, mut rewritten) = (0, 0);
        for row in 0..self.lines - shift {
            if self.hashes[row + shift].blank {
                continue;
            }
            text += 1;
            let old = &self.screen[(row + shift) * columns..][..columns];
            let new = &cells[row * columns..][..columns];
            // Text that only grew (a line being typed) isn't overwritten.
            if old.iter().zip(new).any(|(&o, &n)| o != n && !is_blank(o) && !is_blank(n)) {
                rewritten += 1;
            }
        }
        2 * filled >= self.lines && rewritten >= 2 && 2 * rewritten >= text
    }

    /// Move each cell's and row's animation along with the text.
    fn carry(&mut self, movement: Movement) {
        let columns = self.columns;
        let (top, bottom, distance) = (movement.top, movement.bottom + 1, movement.shift.unsigned_abs());
        if movement.replaced || distance >= bottom - top {
            self.cells[top * columns..bottom * columns].fill(CellState::NONE);
            self.marks[top..bottom].fill(Mark::NONE);
        } else if movement.shift > 0 {
            self.cells.copy_within((top + distance) * columns..bottom * columns, top * columns);
            self.cells[(bottom - distance) * columns..bottom * columns].fill(CellState::NONE);
            self.marks.copy_within(top + distance..bottom, top);
            self.marks[bottom - distance..bottom].fill(Mark::NONE);
        } else if movement.shift < 0 {
            self.cells.copy_within(top * columns..(bottom - distance) * columns, (top + distance) * columns);
            self.cells[top * columns..(top + distance) * columns].fill(CellState::NONE);
            self.marks.copy_within(top..bottom - distance, top + distance);
            self.marks[top..top + distance].fill(Mark::NONE);
        }
    }

    /// Collect the cells whose text is new: changed to something that isn't
    /// blank, or on a row whose text came into view. Blanked cells stop
    /// animating.
    fn diff(&mut self, cells: &[char], movement: Movement) {
        let columns = self.columns;
        self.fresh.clear();
        for (row, new) in cells.chunks_exact(columns).enumerate() {
            let Some(source) = movement.source(row) else {
                self.fresh
                    .extend(new.iter().enumerate().filter(|(_, c)| !is_blank(**c)).map(|(i, _)| row * columns + i));
                continue;
            };
            if self.new_hashes[row].hash == self.hashes[source].hash {
                continue;
            }
            let old = &self.screen[source * columns..][..columns];
            for (column, (&o, &n)) in old.iter().zip(new).enumerate() {
                if o == n {
                    continue;
                }
                let index = row * columns + column;
                if is_blank(n) {
                    self.cells[index] = CellState::NONE;
                } else {
                    self.fresh.push(index);
                }
            }
        }
    }

    /// The row a typed echo landed on, if this frame's new text is one.
    ///
    /// Local and remote echo look alike: a few characters on the row the
    /// cursor was on when you typed (or is on now), soon after, with the
    /// screen not moving. Once Enter is typed it's only the row you typed
    /// on: the cursor's is the next line. The reply may arrive in the same
    /// read after Enter; a full-screen app may update a status line too.
    /// In those cases only the echo's row has to fit.
    fn echo_row(&self, screen: &Snapshot<'_>, movement: Movement, fresh: &[usize]) -> Option<usize> {
        if self.pending_echo == 0 || movement.shift != 0 {
            return None;
        }
        let columns = self.columns;
        let on = |row: usize| fresh.iter().filter(|&&i| i / columns == row).count();
        let row = match self.key_row.filter(|&row| on(row) > 0) {
            Some(row) => row,
            None if self.entered => return None,
            None => screen.cursor.0,
        };
        let count = on(row);
        let fits = count > 0 && count <= self.pending_echo + ECHO_SLACK;
        (fits && (count == fresh.len() || self.entered || screen.alt_screen)).then_some(row)
    }

    /// Output shows at once, or held back and let out a piece at a time in
    /// reading order. However much piles up, it's all out within
    /// `REVEAL_CAP` of arriving.
    fn schedule(&mut self, now: f64, cells: &[char], fresh: &[usize]) {
        let style = self.settings.reveal;
        let columns = self.columns;
        let starts_piece = |prev: Option<usize>, index: usize| match prev {
            None => true,
            Some(prev) => match style {
                Reveal::Instant => false,
                Reveal::Typewriter => true,
                Reveal::Words => {
                    prev / columns != index / columns || cells[prev + 1..index].iter().any(|&c| is_blank(c))
                }
                Reveal::Lines => prev / columns != index / columns,
            },
        };
        let mut pieces = 0;
        let mut prev = None;
        for &index in fresh {
            pieces += usize::from(starts_piece(prev, index));
            prev = Some(index);
        }
        let step = match style {
            Reveal::Instant => 0.0,
            Reveal::Typewriter => TYPEWRITER_STEP,
            Reveal::Words => WORD_STEP,
            Reveal::Lines => LINE_STEP,
        } / self.pace;
        let deadline = now + REVEAL_CAP / self.pace;
        let start = if style == Reveal::Instant { now } else { self.reveal_clock.max(now).min(deadline) };
        let step = if pieces > 1 { step.min((deadline - start) / (pieces - 1) as f64) } else { step };

        let (mut piece, mut prev, mut row, mut at) = (0, None, usize::MAX, start);
        for &index in fresh {
            if starts_piece(prev, index) {
                at = start + piece as f64 * step;
                piece += 1;
            }
            prev = Some(index);
            self.cells[index] = CellState { start: at, kind: Kind::Output, seed: self.rng.next_u32() };
            if index / columns != row {
                // The row's mark starts as its first new text shows.
                row = index / columns;
                self.marks[row].start = at;
                self.latest_mark = self.latest_mark.max(at);
            }
        }
        self.latest_output = self.latest_output.max(at);
        self.reveal_clock = if style == Reveal::Instant { now } else { at + step };
    }

    fn flush_reveal(&mut self, now: f64) {
        if self.baseline || now >= self.latest_output {
            return;
        }
        for cell in &mut self.cells {
            if cell.kind == Kind::Output && cell.start > now {
                cell.start = now;
            }
        }
        for mark in &mut self.marks {
            if mark.start > now {
                mark.start = now;
            }
        }
        self.latest_output = now;
        self.latest_mark = self.latest_mark.min(now);
        self.reveal_clock = now;
    }

    /// Keep each marked row's extent up to date, and drop finished marks.
    fn refresh_marks(&mut self, now: f64, cells: &[char]) {
        let until = f64::from(line_duration(self.settings.new_lines)) / self.pace;
        for (mark, text) in self.marks.iter_mut().zip(cells.chunks_exact(self.columns)) {
            if mark.start == NEVER {
                continue;
            }
            match text.iter().position(|&c| !is_blank(c)) {
                Some(first) if now < mark.start + until => {
                    mark.first = first;
                    mark.end = text.iter().rposition(|&c| !is_blank(c)).unwrap_or(first) + 1;
                }
                _ => *mark = Mark::NONE,
            }
        }
    }

    fn spawn(&mut self, now: f64, style: KeystrokeBurst, centre: (f32, f32)) {
        let count = match style {
            KeystrokeBurst::Off => 0,
            KeystrokeBurst::Sparks => self.rng.between(6, 9),
            KeystrokeBurst::Confetti => self.rng.between(6, 8),
            KeystrokeBurst::Embers => self.rng.between(5, 7),
            KeystrokeBurst::Bubbles => self.rng.between(3, 5),
            KeystrokeBurst::Stars => self.rng.between(4, 6),
            KeystrokeBurst::Ripple => 1,
            KeystrokeBurst::Explosion => 18,
            KeystrokeBurst::Lasers => 4,
            KeystrokeBurst::Lightning => 3,
            KeystrokeBurst::Portal => 9,
        };
        let pace = self.pace as f32;
        let first_color = self.rng.between(0, CONFETTI.len() - 1);
        for i in 0..count {
            let particle = Particle::new(style, i, count, first_color, now, pace, centre, &mut self.rng);
            self.particles_until = self.particles_until.max(now + f64::from(particle.life / pace));
            self.particles.push_back(particle);
        }
        while self.particles.len() > MAX_PARTICLES {
            self.particles.pop_front();
        }
    }
}

// -- blinking -------------------------------------------------------------

/// Half of the cursor's blink cycle, in seconds. Blinking keeps its rhythm
/// whatever the animation speed.
pub const BLINK: f64 = 0.53;
/// How often a blink that changes continuously is redrawn: about 30 fps is
/// plenty for something this slow.
pub const BLINK_FRAME: f64 = 1.0 / 30.0;
/// Timer wake-ups land a few milliseconds either side of a blink edge.
/// Treating anything that close as already past it means each edge costs
/// one redraw, not one just before it and another just after.
const BLINK_SLACK: f64 = 0.03;
const BLINK_FADE: f64 = 0.15;
const PULSE: f64 = 1.6;

/// Cursor opacity, 0..1, `epoch` being when the blink last restarted
/// (a keypress, or the terminal getting focus).
pub fn blink_level(style: CursorBlink, now: f64, epoch: f64) -> f32 {
    let elapsed = (now - epoch).max(0.0);
    match style {
        CursorBlink::Classic => {
            if ((now + BLINK_SLACK - epoch).max(0.0) / BLINK) as i64 % 2 == 0 {
                1.0
            } else {
                0.0
            }
        }
        CursorBlink::Fade => {
            // On, fade out, off, fade in; the fades end on the plain blink's edges.
            let phase = elapsed % (2.0 * BLINK);
            let fade = |from: f64| smoothstep(((phase - from) / BLINK_FADE) as f32);
            if phase < BLINK - BLINK_FADE {
                1.0
            } else if phase < BLINK {
                1.0 - fade(BLINK - BLINK_FADE)
            } else if phase < 2.0 * BLINK - BLINK_FADE {
                0.0
            } else {
                fade(2.0 * BLINK - BLINK_FADE)
            }
        }
        CursorBlink::Pulse => 0.675 + 0.325 * (std::f64::consts::TAU * elapsed / PULSE).cos() as f32,
        CursorBlink::Glow => 1.0,
    }
}

/// Glow: how far the halo around the cursor has swelled, 0..1.
pub fn blink_halo(style: CursorBlink, now: f64, epoch: f64) -> f32 {
    match style {
        CursorBlink::Glow => (0.5 - 0.5 * (std::f64::consts::TAU * (now - epoch).max(0.0) / PULSE).cos()) as f32,
        _ => 0.0,
    }
}

/// Seconds until the blink next needs drawing.
pub fn blink_wake(style: CursorBlink, now: f64, epoch: f64) -> f64 {
    match style {
        CursorBlink::Classic => BLINK - (now + BLINK_SLACK - epoch).max(0.0) % BLINK + BLINK_SLACK,
        CursorBlink::Fade => {
            let phase = (now - epoch).max(0.0) % (2.0 * BLINK);
            let rest_ends = if phase < BLINK - BLINK_FADE {
                BLINK - BLINK_FADE
            } else if (BLINK..2.0 * BLINK - BLINK_FADE).contains(&phase) {
                2.0 * BLINK - BLINK_FADE
            } else {
                return BLINK_FRAME; // fading
            };
            let wait = rest_ends - phase;
            if wait > BLINK_SLACK { wait } else { BLINK_FRAME }
        }
        CursorBlink::Pulse | CursorBlink::Glow => BLINK_FRAME,
    }
}

// -- curves ---------------------------------------------------------------

pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

pub fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn ease_out_quad(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (2.0 - 2.0 * t).powi(3) / 2.0 }
}

/// A glyph's offset rounded to a step of [`OFFSET_STEPS`] of a cell.
fn in_steps(offset: f32) -> f32 {
    (offset * OFFSET_STEPS).round() / OFFSET_STEPS
}

/// Falls from above into place, then one small hop.
fn bounce(t: f32) -> f32 {
    const FALL: f32 = 0.6;
    if t < FALL {
        let q = t / FALL;
        -0.45 * (1.0 - q * q)
    } else {
        let q = (t - FALL) / (1.0 - FALL);
        -0.48 * q * (1.0 - q)
    }
}

// Durations at normal speed, in seconds.

fn typed_duration(style: TypedText) -> f32 {
    match style {
        TypedText::Off => 0.0,
        TypedText::Pop => 0.16,
        TypedText::Bounce => 0.28,
        TypedText::Flash => 0.30,
        TypedText::Fade => 0.14,
        TypedText::Stamp => 0.24,
        TypedText::Laser => 0.28,
    }
}

fn new_text_duration(style: NewText) -> f32 {
    match style {
        NewText::Off => 0.0,
        NewText::Fade => 0.18,
        NewText::Rise | NewText::Drop => 0.22,
        NewText::Zoom => 0.20,
        NewText::Decode => 0.30,
        NewText::Heat => 0.5,
        NewText::Hologram => 0.32,
        NewText::Matrix => 0.38,
    }
}

fn line_duration(style: NewLines) -> f32 {
    match style {
        NewLines::Off => 0.0,
        NewLines::Glow => 0.7,
        NewLines::Flash => 0.25,
        NewLines::Marker => 1.0,
        NewLines::Underline => 0.6,
        NewLines::Shimmer => 0.5,
        NewLines::Laser => 0.55,
        NewLines::Radar => 0.65,
    }
}

/// Smooth and Float; Spring runs until it settles.
fn scroll_duration(style: Scrolling) -> f64 {
    match style {
        Scrolling::Smooth => 0.14,
        Scrolling::Float => 0.30,
        Scrolling::Off | Scrolling::Spring => 0.0,
    }
}

/// `age` in seconds at normal speed.
fn typed_look(style: TypedText, age: f32) -> Option<CellLook> {
    let duration = typed_duration(style);
    if age >= duration {
        return None;
    }
    let t = (age / duration).max(0.0);
    let look = match style {
        TypedText::Off => return None,
        TypedText::Pop => CellLook {
            scale: 1.0 + 0.35 * (1.0 - ease_out_cubic(t)),
            accent: 1.0 - ease_out_quad(t),
            ..CellLook::PLAIN
        },
        TypedText::Bounce => CellLook { offset_y: in_steps(bounce(t)), alpha: (t / 0.15).min(1.0), ..CellLook::PLAIN },
        TypedText::Flash => CellLook { flash: 0.55 * (1.0 - smoothstep(t)), ..CellLook::PLAIN },
        TypedText::Fade => CellLook { alpha: ease_out_quad(t), ..CellLook::PLAIN },
        TypedText::Stamp => CellLook {
            scale: 1.0 + 0.55 * (1.0 - ease_out_cubic(t)),
            offset_y: in_steps(-0.45 * (1.0 - ease_out_cubic(t))),
            alpha: (t / 0.1).min(1.0),
            ..CellLook::PLAIN
        },
        TypedText::Laser => CellLook {
            accent: 1.0 - smoothstep(t),
            flash: 0.35 * (1.0 - t).powi(2),
            alpha: (t / 0.12).min(1.0),
            ..CellLook::PLAIN
        },
    };
    Some(look)
}

/// `age` in seconds at normal speed since the cell showed.
fn output_look(style: NewText, age: f32, seed: u32) -> Option<CellLook> {
    let duration = new_text_duration(style);
    if age >= duration {
        return None;
    }
    let t = (age / duration).max(0.0);
    let alpha = ease_out_quad(t);
    let away = 1.0 - ease_out_cubic(t);
    let look = match style {
        NewText::Off => return None,
        NewText::Fade => CellLook { alpha, ..CellLook::PLAIN },
        NewText::Rise => CellLook { alpha, offset_y: in_steps(0.35 * away), ..CellLook::PLAIN },
        NewText::Drop => CellLook { alpha, offset_y: in_steps(-0.35 * away), ..CellLook::PLAIN },
        NewText::Zoom => CellLook { alpha, scale: 1.0 - 0.6 * away, ..CellLook::PLAIN },
        NewText::Decode | NewText::Matrix => {
            // Cells settle at slightly different moments, like they're being
            // cracked one by one. The tint goes by age alone, so cells shown
            // together share a color (and a run of text), and it's gone
            // before the first of them settles.
            let first = 0.7 * duration;
            let settle = first + 0.3 * duration * (seed & 0xffff) as f32 / 65535.0;
            if age >= settle {
                return None;
            }
            let tick = (age / DECODE_TICK) as u64;
            let glyphs = if style == NewText::Matrix { &['0', '1'][..] } else { DECODE_GLYPHS };
            let pick = scramble(u64::from(seed) << 32 | tick) % glyphs.len() as u64;
            CellLook {
                glyph: Some(glyphs[pick as usize]),
                accent: 0.4 * (1.0 - age / first).max(0.0),
                ..CellLook::PLAIN
            }
        }
        NewText::Heat => CellLook { accent: 1.0 - smoothstep(t), ..CellLook::PLAIN },
        NewText::Hologram => CellLook {
            alpha: (alpha * (0.88 + 0.12 * (t * TAU * 3.0).sin())).clamp(0.0, 1.0),
            accent: 0.6 * (1.0 - t),
            offset_y: in_steps(0.08 * (t * TAU * 2.0).sin() * (1.0 - t)),
            ..CellLook::PLAIN
        },
    };
    Some(look)
}

// -- state ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    None,
    Typed,
    Output,
}

#[derive(Debug, Clone, Copy)]
struct CellState {
    /// When the effect starts; for held-back output, when it shows.
    start: f64,
    kind: Kind,
    /// Stable randomness for the cell (Decode's glyphs).
    seed: u32,
}

impl CellState {
    const NONE: CellState = CellState { start: NEVER, kind: Kind::None, seed: 0 };
}

#[derive(Debug, Clone, Copy)]
struct Mark {
    start: f64,
    first: usize,
    end: usize,
}

impl Mark {
    const NONE: Mark = Mark { start: NEVER, first: 0, end: 0 };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowHash {
    hash: u64,
    blank: bool,
    /// One past the last column with text.
    end: usize,
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\0'
}

fn row_hash(row: &[char]) -> RowHash {
    // FNV-1a over whole characters.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut end = 0;
    for (column, &c) in row.iter().enumerate() {
        if !is_blank(c) {
            end = column + 1;
        }
        hash = (hash ^ u64::from(c)).wrapping_mul(0x0100_0000_01b3);
    }
    RowHash { hash, blank: end == 0, end }
}

/// How the live screen's text moved since the last frame: rows
/// `top..=bottom` moved `shift` rows up (negative: down) together, and the
/// rest stayed put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Movement {
    shift: isize,
    top: usize,
    bottom: usize,
    /// So much went by that none of the text was there before; `shift` is
    /// only a guess at how far it moved.
    replaced: bool,
}

impl Movement {
    fn still(lines: usize) -> Movement {
        Movement { shift: 0, top: 0, bottom: lines - 1, replaced: false }
    }

    fn replaced(shift: usize, lines: usize) -> Movement {
        Movement { shift: shift as isize, top: 0, bottom: lines - 1, replaced: true }
    }

    /// The whole screen moved together, so it can be drawn sliding.
    fn whole(&self, lines: usize) -> bool {
        self.top == 0 && self.bottom + 1 == lines
    }

    /// The row whose text `row` shows now, or `None` if its text came into
    /// view this frame.
    fn source(&self, row: usize) -> Option<usize> {
        let band = self.top..=self.bottom;
        if self.replaced {
            None
        } else if !band.contains(&row) {
            Some(row)
        } else {
            row.checked_add_signed(self.shift).filter(|from| band.contains(from))
        }
    }

    /// Where last frame's row `row` is now, if it's still in view.
    fn target(&self, row: usize) -> Option<usize> {
        let band = self.top..=self.bottom;
        if self.replaced {
            None
        } else if !band.contains(&row) {
            Some(row)
        } else {
            row.checked_add_signed(-self.shift).filter(|to| band.contains(to))
        }
    }
}

/// What a [`Movement`] makes of the screen.
#[derive(Debug, Clone, Copy, Default)]
struct Tally {
    /// Rows of text it accounts for, and how many of those by moving them.
    matched: usize,
    moved: usize,
    /// Rows of old text it has replaced by other text or wiped.
    lost: usize,
}

fn cell_pos((row, column): (usize, usize)) -> (f32, f32) {
    (row as f32, column as f32)
}

/// Progress 0..1 through something that started at `start`.
fn progress(now: f64, start: f64, duration: f64) -> f32 {
    if duration <= 0.0 {
        return 1.0;
    }
    ((now - start) / duration).clamp(0.0, 1.0) as f32
}

/// Eases out from `from` to wherever it's asked to go; retargeting starts a
/// new one from where the last one had got to.
#[derive(Debug, Clone, Copy)]
struct Tween {
    from: (f32, f32),
    start: f64,
}

impl Default for Tween {
    fn default() -> Self {
        Tween { from: (0.0, 0.0), start: NEVER }
    }
}

impl Tween {
    fn at(&self, now: f64, to: (f32, f32), duration: f64) -> (f32, f32) {
        let t = progress(now, self.start, duration);
        if t >= 1.0 {
            return to;
        }
        let e = ease_out_cubic(t);
        (lerp(self.from.0, to.0, e), lerp(self.from.1, to.1, e))
    }
}

/// A damped spring pulling a displacement back to 0. Solved in closed form
/// rather than stepped, so it can be read at any moment without changing
/// it, and it's exact however uneven the frames are.
#[derive(Debug, Clone, Copy)]
struct Spring {
    x: f32,
    v: f32,
    since: f64,
    omega: f32,
    zeta: f32,
    /// From here on it's close enough to rest to call it 0.
    settle: f64,
}

impl Default for Spring {
    fn default() -> Self {
        Spring { x: 0.0, v: 0.0, since: NEVER, omega: 1.0, zeta: 0.5, settle: NEVER }
    }
}

impl Spring {
    /// Displacement and velocity. Underdamped (zeta < 1).
    fn state(&self, now: f64) -> (f32, f32) {
        if now >= self.settle {
            return (0.0, 0.0);
        }
        let t = (now - self.since).max(0.0) as f32;
        let damping = self.zeta * self.omega;
        let wd = self.omega * (1.0 - self.zeta * self.zeta).sqrt();
        let (a, b) = (self.x, (self.v + damping * self.x) / wd);
        let decay = (-damping * t).exp();
        let (sin, cos) = (wd * t).sin_cos();
        (decay * (a * cos + b * sin), decay * ((b * wd - damping * a) * cos - (a * wd + damping * b) * sin))
    }

    /// Displace it by `dx`, keeping its velocity.
    fn kick(&mut self, now: f64, dx: f32, omega: f32, zeta: f32) {
        let (x, v) = self.state(now);
        let x = x + dx;
        let damping = zeta * omega;
        let wd = omega * (1.0 - zeta * zeta).sqrt();
        // The oscillation never exceeds this envelope, so past `settle` it's within SETTLED of rest.
        let amplitude = x.hypot((v + damping * x) / wd);
        let settle = if amplitude > SETTLED { now + f64::from((amplitude / SETTLED).ln() / damping) } else { now };
        *self = Spring { x, v, since: now, omega, zeta, settle };
    }
}

/// The screen's scroll animation.
#[derive(Default)]
struct ScrollState {
    /// The style the state below belongs to.
    style: Option<Scrolling>,
    /// Smooth and Float: each move eases out on its own and they add up, so
    /// a steady stream of lines scrolls steadily and every move is over
    /// on time.
    moves: Vec<(f64, f32)>,
    spring: Spring,
}

impl ScrollState {
    fn clear(&mut self) {
        self.moves.clear();
        self.spring = Spring::default();
    }

    fn sync(&mut self, style: Scrolling) {
        if self.style != Some(style) {
            self.clear();
            self.style = Some(style);
        }
    }

    fn expire(&mut self, now: f64, pace: f64) {
        let duration = self.style.map_or(0.0, scroll_duration) / pace;
        self.moves.retain(|&(at, _)| now < at + duration);
    }

    fn offset(&self, now: f64, style: Scrolling, pace: f64) -> f32 {
        let duration = scroll_duration(style) / pace;
        match style {
            Scrolling::Off => 0.0,
            Scrolling::Smooth => {
                self.moves.iter().map(|&(at, rows)| rows * (1.0 - ease_out_cubic(progress(now, at, duration)))).sum()
            }
            Scrolling::Float => {
                self.moves.iter().map(|&(at, rows)| rows * (1.0 - ease_in_out_cubic(progress(now, at, duration)))).sum()
            }
            Scrolling::Spring => self.spring.state(now).0,
        }
    }

    /// The content just moved `rows` up the view (negative: down). Never
    /// more than a screenful of it is animated.
    fn push(&mut self, now: f64, rows: i64, lines: usize, style: Scrolling, pace: f64) {
        if rows == 0 || style == Scrolling::Off {
            return;
        }
        let limit = lines as f32;
        let current = self.offset(now, style, pace);
        let delta = (current + rows as f32).clamp(-limit, limit) - current;
        if style == Scrolling::Spring {
            let (omega, zeta) = SCROLL_SPRING;
            self.spring.kick(now, delta, omega * pace as f32, zeta);
        } else if delta != 0.0 {
            self.moves.push((now, delta));
        }
    }

    fn busy(&self, now: f64, style: Scrolling, pace: f64) -> bool {
        let duration = scroll_duration(style) / pace;
        match style {
            Scrolling::Off => false,
            Scrolling::Spring => now < self.spring.settle,
            Scrolling::Smooth | Scrolling::Float => self.moves.iter().any(|&(at, _)| now < at + duration),
        }
    }
}

/// The cursor's motion. Every style's state is kept up to date, so changing
/// style mid-flight carries on smoothly.
#[derive(Default)]
struct Motion {
    /// Where the cursor is headed: the real cursor, or the reveal head.
    target: Option<(f32, f32)>,
    glide: Tween,
    /// Where Smear's tail is catching up from.
    smear: Tween,
    /// Row and column displacement from the target.
    spring: [Spring; 2],
    /// Afterimages and when the cursor left them, oldest first.
    ghosts: Vec<((f32, f32), f64)>,
}

impl Motion {
    fn place(&mut self, at: (f32, f32)) {
        self.target = Some(at);
        self.glide = Tween::default();
        self.smear = Tween::default();
        self.spring = [Spring::default(); 2];
        self.ghosts.clear();
    }

    fn retarget(&mut self, now: f64, to: (f32, f32), pace: f64) {
        let Some(from) = self.target else {
            self.place(to);
            return;
        };
        if from == to {
            return;
        }
        if (to.0 - from.0).abs() > SNAP_ROWS {
            self.place(to);
            return;
        }
        self.glide = Tween { from: self.glide.at(now, from, GLIDE / pace), start: now };
        self.smear = Tween { from: self.smear.at(now, from, SMEAR / pace), start: now };
        let (omega, zeta) = CURSOR_SPRING;
        let omega = omega * pace as f32;
        self.spring[0].kick(now, from.0 - to.0, omega, zeta);
        self.spring[1].kick(now, from.1 - to.1, omega, zeta);

        // An afterimage where it was, and a couple along the way on a long
        // jump, fresher the nearer they are to where it went.
        let reach = (to.0 - from.0).abs().max((to.1 - from.1).abs() / ASPECT);
        let along = if reach > 3.0 {
            2
        } else if reach > 1.5 {
            1
        } else {
            0
        };
        for i in 0..=along {
            let f = i as f32 / (along + 1) as f32;
            let at = (lerp(from.0, to.0, f).round(), lerp(from.1, to.1, f).round());
            self.ghosts.push((at, now - (along - i) as f64 * GHOST_STAGGER / pace));
        }
        if self.ghosts.len() > MAX_GHOSTS {
            let extra = self.ghosts.len() - MAX_GHOSTS;
            self.ghosts.drain(..extra);
        }
        self.target = Some(to);
    }

    /// The text moved up `rows`.
    fn shift(&mut self, rows: f32) {
        if let Some(target) = &mut self.target {
            target.0 -= rows;
        }
        self.glide.from.0 -= rows;
        self.smear.from.0 -= rows;
        self.ghosts.iter_mut().for_each(|(at, _)| at.0 -= rows);
    }

    fn busy(&self, now: f64, style: CursorMotion, pace: f64) -> bool {
        match style {
            CursorMotion::Off => false,
            CursorMotion::Glide => now < self.glide.start + GLIDE / pace,
            CursorMotion::Spring => self.spring.iter().any(|s| now < s.settle),
            CursorMotion::Smear => now < self.smear.start + SMEAR / pace,
            CursorMotion::Ghost => self.ghosts.iter().any(|&(_, left)| now < left + GHOST / pace),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ShakeState {
    start: f64,
    phase: (f32, f32),
}

impl Default for ShakeState {
    fn default() -> Self {
        ShakeState { start: NEVER, phase: (0.0, 0.0) }
    }
}

/// One particle, worked out from when and how it was thrown whenever it's
/// asked about, so nothing needs stepping each frame.
#[derive(Debug, Clone, Copy)]
struct Particle {
    kind: KeystrokeBurst,
    born: f64,
    /// Lifetime at normal speed.
    life: f32,
    pace: f32,
    /// Row, column.
    origin: (f32, f32),
    /// Rows per second down, cell heights per second right.
    velocity: (f32, f32),
    size: f32,
    color: ParticleColor,
    angle: f32,
    spin: f32,
    phase: f32,
}

impl Particle {
    #[allow(clippy::too_many_arguments)]
    fn new(
        kind: KeystrokeBurst,
        i: usize,
        count: usize,
        first_color: usize,
        born: f64,
        pace: f32,
        origin: (f32, f32),
        rng: &mut Rng,
    ) -> Particle {
        let base = Particle {
            kind,
            born,
            life: 0.5,
            pace,
            origin,
            velocity: (0.0, 0.0),
            size: 0.1,
            color: ParticleColor::Accent,
            angle: rng.range(0.0, TAU),
            spin: 0.0,
            phase: rng.range(0.0, TAU),
        };
        // Angles are measured with up being -FRAC_PI_2 (rows grow downward).
        let throw = |angle: f32, speed: f32| (angle.sin() * speed, angle.cos() * speed);
        match kind {
            KeystrokeBurst::Off | KeystrokeBurst::Ripple => Particle { life: 0.4, size: RIPPLE_RADIUS, ..base },
            KeystrokeBurst::Sparks => Particle {
                life: rng.range(0.35, 0.6),
                velocity: throw(-FRAC_PI_2 + rng.range(-1.2, 1.2), rng.range(5.0, 10.0)),
                size: rng.range(0.07, 0.11),
                ..base
            },
            KeystrokeBurst::Confetti => Particle {
                life: rng.range(0.7, 0.9),
                velocity: throw(-FRAC_PI_2 + rng.range(-1.0, 1.0), rng.range(4.0, 7.0)),
                size: rng.range(0.09, 0.13),
                color: ParticleColor::Fixed(CONFETTI[(first_color + i) % CONFETTI.len()]),
                spin: rng.range(5.0, 12.0) * if rng.unit() < 0.5 { -1.0 } else { 1.0 },
                ..base
            },
            KeystrokeBurst::Embers => Particle {
                life: rng.range(0.6, 0.8),
                velocity: (-rng.range(1.5, 3.0), rng.range(-0.4, 0.4)),
                size: rng.range(0.07, 0.11),
                color: ParticleColor::Fixed(EMBERS[i % EMBERS.len()]),
                ..base
            },
            KeystrokeBurst::Bubbles => Particle {
                life: rng.range(0.7, 0.9),
                origin: (origin.0 - 0.3, origin.1),
                velocity: (-rng.range(1.2, 2.2), rng.range(-0.3, 0.3)),
                size: rng.range(0.1, 0.14),
                ..base
            },
            KeystrokeBurst::Stars => Particle {
                life: rng.range(0.5, 0.7),
                velocity: throw(TAU * (i as f32 + rng.range(-0.25, 0.25)) / count as f32, rng.range(2.5, 4.5)),
                size: rng.range(0.2, 0.28),
                spin: rng.range(2.0, 4.0),
                ..base
            },
            KeystrokeBurst::Explosion => Particle {
                life: 0.55,
                velocity: if i == 0 { (0.0, 0.0) } else { throw(TAU * i as f32 / count as f32, rng.range(6.0, 12.0)) },
                size: if i == 0 { 2.6 } else { rng.range(0.08, 0.16) },
                color: ParticleColor::Fixed(EMBERS[i % EMBERS.len()]),
                phase: if i == 0 { -1.0 } else { base.phase },
                ..base
            },
            KeystrokeBurst::Lasers => Particle {
                life: 0.38,
                velocity: throw(TAU * i as f32 / count as f32, 13.0),
                angle: TAU * i as f32 / count as f32,
                size: 0.07,
                ..base
            },
            KeystrokeBurst::Lightning => {
                Particle { life: 0.3, angle: TAU * i as f32 / count as f32 + base.angle, size: 1.6, ..base }
            }
            KeystrokeBurst::Portal => Particle {
                life: 0.65,
                size: if i == 0 { 1.5 } else { 0.12 },
                phase: if i == 0 { -1.0 } else { TAU * i as f32 / count as f32 },
                ..base
            },
        }
    }

    fn alive(&self, now: f64) -> bool {
        now < self.born + f64::from(self.life / self.pace)
    }

    fn look(&self, now: f64) -> Option<ParticleLook> {
        let age = (now - self.born) as f32 * self.pace;
        if !(0.0..self.life).contains(&age) {
            return None;
        }
        let u = age / self.life;
        let (vy, vx) = self.velocity;
        let wobble = |amplitude: f32, hz: f32| amplitude * (TAU * hz * age + self.phase).sin();
        let (dy, dx, size, alpha, shape) = match self.kind {
            KeystrokeBurst::Off | KeystrokeBurst::Ripple => {
                (0.0, 0.0, self.size * ease_out_cubic(u), 0.85 * (1.0 - u) * (1.0 - u), ParticleShape::Ring)
            }
            KeystrokeBurst::Sparks => (
                drift(vy, 18.0, 4.0, age),
                drift(vx, 0.0, 4.0, age),
                self.size * (1.0 - 0.5 * u),
                (1.0 - u).powf(1.3),
                ParticleShape::Dot,
            ),
            KeystrokeBurst::Confetti => (
                drift(vy, 12.0, 3.0, age),
                drift(vx, 0.0, 3.0, age) + wobble(0.12, 1.5),
                self.size,
                if u < 0.7 { 1.0 } else { 1.0 - (u - 0.7) / 0.3 },
                ParticleShape::Square { angle: self.angle + self.spin * age },
            ),
            KeystrokeBurst::Embers => (
                drift(vy, -1.5, 1.0, age),
                drift(vx, 0.0, 1.0, age) + wobble(0.18, 2.6),
                self.size * (1.0 - 0.7 * u),
                (1.0 - u) * (0.75 + 0.25 * (TAU * 11.0 * age + self.phase).sin()),
                ParticleShape::Dot,
            ),
            KeystrokeBurst::Bubbles => (
                drift(vy, -0.8, 0.8, age),
                drift(vx, 0.0, 0.8, age) + wobble(0.15, 1.8),
                self.size * (1.0 + 1.4 * ease_out_quad(u)),
                0.9 * (1.0 - u * u),
                ParticleShape::Ring,
            ),
            KeystrokeBurst::Stars => (
                drift(vy, 1.5, 5.0, age),
                drift(vx, 0.0, 5.0, age),
                self.size * (0.75 + 0.25 * (TAU * 7.0 * age + self.phase).sin()),
                (1.0 - u) * (0.65 + 0.35 * (TAU * 9.0 * age + self.phase).sin()),
                ParticleShape::Star { angle: self.angle + self.spin * age },
            ),
            KeystrokeBurst::Explosion if self.phase < 0.0 => {
                (0.0, 0.0, self.size * ease_out_cubic(u), (1.0 - u).powi(2), ParticleShape::Ring)
            }
            KeystrokeBurst::Explosion => {
                (drift(vy, 4.0, 5.0, age), drift(vx, 0.0, 5.0, age), self.size * (1.0 - u), 1.0 - u, ParticleShape::Dot)
            }
            KeystrokeBurst::Lasers => {
                (vy * age, vx * age, self.size, 1.0 - u, ParticleShape::Beam { angle: self.angle, length: 1.3 })
            }
            KeystrokeBurst::Lightning => (
                0.0,
                0.0,
                self.size * ease_out_quad(u),
                (1.0 - u) * (0.8 + 0.2 * (age * 40.0).sin()),
                ParticleShape::Bolt { angle: self.angle },
            ),
            KeystrokeBurst::Portal if self.phase < 0.0 => {
                (0.0, 0.0, self.size * (std::f32::consts::PI * u).sin(), 0.7 * (1.0 - u), ParticleShape::Ring)
            }
            KeystrokeBurst::Portal => {
                let radius = 1.5 * (std::f32::consts::PI * u).sin();
                let angle = self.phase + age * 9.0;
                (radius * angle.sin(), radius * angle.cos(), self.size, 1.0 - u, ParticleShape::Star { angle })
            }
        };
        Some(ParticleLook {
            pos: (self.origin.0 + dy, self.origin.1 + dx * ASPECT),
            size,
            color: self.color,
            alpha: alpha.clamp(0.0, 1.0),
            shape,
        })
    }
}

/// Distance travelled after `t` seconds from speed `v`, under `gravity`
/// and air drag `drag` (per second).
fn drift(v: f32, gravity: f32, drag: f32, t: f32) -> f32 {
    if drag <= 0.0 {
        return v * t + 0.5 * gravity * t * t;
    }
    let terminal = gravity / drag;
    (v - terminal) * (1.0 - (-drag * t).exp()) / drag + terminal * t
}

/// Small, fast and deterministic, which is all sparks need.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        // xorshift64
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// 0..1
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.unit()
    }

    /// low..=high
    fn between(&mut self, low: usize, high: usize) -> usize {
        low + (self.next_u64() % (high - low + 1) as u64) as usize
    }
}

/// The same well-mixed number for the same input (splitmix64's finish).
fn scramble(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;
    use crate::terminal::emulator::Emulator;

    fn on() -> Animations {
        Animations { enabled: true, ..Animations::default() }
    }

    /// A screen with `rows` of text from the top and blanks below.
    fn grid(columns: usize, lines: usize, rows: &[&str]) -> Vec<char> {
        let mut cells = vec![' '; columns * lines];
        for (row, text) in rows.iter().enumerate().take(lines) {
            for (column, c) in text.chars().take(columns).enumerate() {
                cells[row * columns + column] = c;
            }
        }
        cells
    }

    fn snap(cells: &[char], columns: usize, history: usize, cursor: (usize, usize)) -> Snapshot<'_> {
        Snapshot { cells, columns, history_size: history, display_offset: 0, cursor, alt_screen: false }
    }

    /// "line 3", "line 4"...: rows that are all different, 12 columns wide.
    fn numbered(from: usize, lines: usize) -> Vec<char> {
        let rows: Vec<String> = (from..from + lines).map(|i| format!("line {i}")).collect();
        grid(12, lines, &rows.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// Rows with at least one cell animating.
    fn lively_rows(anim: &Animator, now: f64) -> Vec<usize> {
        (0..anim.lines).filter(|&row| (0..anim.columns).any(|c| anim.cell(now, row, c).is_some())).collect()
    }

    fn start(anim: &Animator, row: usize, column: usize) -> f64 {
        anim.cells[row * anim.columns + column].start
    }

    fn assert_still(anim: &Animator, now: f64) {
        assert_eq!(lively_rows(anim, now), Vec::<usize>::new());
        assert!((0..anim.lines).all(|row| anim.line_mark(now, row).is_none()));
        assert_eq!(anim.scroll_offset(now), 0.0);
        assert_eq!(anim.reveal_head(now), None);
        assert_eq!(anim.particles(now).count(), 0);
        assert_eq!(anim.next_wake(now), None);
    }

    /// The live screen, read straight out of the emulator.
    fn capture<'a>(emu: &Emulator, cells: &'a mut Vec<char>) -> Snapshot<'a> {
        use alacritty_terminal::index::{Column, Line};
        use alacritty_terminal::term::TermMode;
        cells.clear();
        let grid = emu.term().grid();
        for row in 0..emu.lines() {
            cells.extend((0..emu.columns()).map(|column| grid[Line(row as i32)][Column(column)].c));
        }
        let cursor = emu.cursor();
        let cells: &'a Vec<char> = cells;
        Snapshot {
            cells,
            columns: emu.columns(),
            history_size: emu.history_size(),
            display_offset: emu.display_offset(),
            cursor: (cursor.line.0 as usize, cursor.column.0),
            alt_screen: emu.term().mode().contains(TermMode::ALT_SCREEN),
        }
    }

    /// A prompt, with animations running from the start.
    fn at_prompt(settings: &Animations) -> Animator {
        let mut anim = Animator::default();
        let cells = grid(30, 3, &["Switch#"]);
        anim.update(0.0, settings, &snap(&cells, 30, 0, (0, 7)));
        anim
    }

    #[test]
    fn nothing_already_on_screen_animates() {
        let settings = on();
        let mut anim = Animator::default();
        let cells = grid(20, 4, &["Switch#show clock", "12:00:00.000 UTC", "Switch#"]);
        anim.update(0.0, &settings, &snap(&cells, 20, 0, (2, 7)));
        assert_still(&anim, 0.0);

        // After a reset a different screen (a clear, a resize) is taken as it is too.
        anim.reset();
        assert_still(&anim, 0.5);
        let cells = grid(20, 4, &["Switch#"]);
        anim.update(1.0, &settings, &snap(&cells, 20, 7, (0, 7)));
        assert_still(&anim, 1.0);

        // What arrives after that animates.
        let cells = grid(20, 4, &["Switch#", "hello"]);
        anim.update(1.1, &settings, &snap(&cells, 20, 7, (1, 5)));
        assert_eq!(lively_rows(&anim, 1.1), [1]);
        assert_eq!(anim.next_wake(1.1), Some(0.0));
    }

    #[test]
    fn switching_screens_is_not_new_text() {
        let settings = on();
        let mut anim = Animator::default();
        let cells = grid(20, 4, &["Switch#vi notes"]);
        anim.update(0.0, &settings, &snap(&cells, 20, 0, (1, 0)));
        let cells = grid(20, 4, &["hello", "~", "~", "\"notes\" 1L, 6B"]);
        anim.update(1.0, &settings, &Snapshot { alt_screen: true, ..snap(&cells, 20, 0, (0, 0)) });
        assert_eq!(lively_rows(&anim, 1.0), Vec::<usize>::new());
        let cells = grid(20, 4, &["Switch#vi notes", "Switch#"]);
        anim.update(2.0, &settings, &snap(&cells, 20, 0, (1, 7)));
        assert_eq!(lively_rows(&anim, 2.0), Vec::<usize>::new());
    }

    #[test]
    fn history_growth_gives_the_shift() {
        let settings = on();
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&numbered(0, 6), 12, 10, (5, 6)));
        anim.update(1.0, &settings, &snap(&numbered(2, 6), 12, 12, (5, 6)));
        assert_eq!(anim.scroll_offset(1.0), 2.0);
        // Rows that moved up are old text; the two that came in at the bottom are new.
        assert_eq!(lively_rows(&anim, 1.0), [4, 5]);
    }

    #[test]
    fn history_growth_tells_a_screen_of_identical_rows_moved() {
        // Enter at a screen full of the same prompt: lining rows up can't
        // tell it scrolled, the history can.
        let settings = on();
        let mut anim = Animator::default();
        let cells = grid(10, 5, &["Switch#"; 5]);
        anim.update(0.0, &settings, &snap(&cells, 10, 3, (4, 7)));
        anim.update(1.0, &settings, &snap(&cells, 10, 4, (4, 7)));
        assert_eq!(anim.scroll_offset(1.0), 1.0);
        assert_eq!(lively_rows(&anim, 1.0), [4]);
    }

    #[test]
    fn a_full_or_missing_scrollback_is_made_up_for_by_lining_rows_up() {
        for history in [0, 5000] {
            let settings = on();
            let mut anim = Animator::default();
            anim.update(0.0, &settings, &snap(&numbered(0, 6), 12, history, (5, 6)));
            anim.update(1.0, &settings, &snap(&numbered(3, 6), 12, history, (5, 6)));
            assert_eq!(anim.scroll_offset(1.0), 3.0, "history {history}");
            assert_eq!(lively_rows(&anim, 1.0), [3, 4, 5], "history {history}");
        }
    }

    #[test]
    fn more_than_a_screenful_replaces_the_screen() {
        // The history can say so...
        let settings = on();
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&numbered(0, 6), 12, 0, (5, 6)));
        anim.update(1.0, &settings, &snap(&numbered(40, 6), 12, 40, (5, 7)));
        assert_eq!(anim.scroll_offset(1.0), 6.0);
        assert_eq!(lively_rows(&anim, 1.0), [0, 1, 2, 3, 4, 5]);

        // ...and when it can't (full, or no scrollback), all the text being
        // replaced with other text does.
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&numbered(0, 6), 12, 5000, (5, 6)));
        anim.update(1.0, &settings, &snap(&numbered(40, 6), 12, 5000, (5, 7)));
        assert_eq!(anim.scroll_offset(1.0), 6.0);
        assert_eq!(lively_rows(&anim, 1.0), [0, 1, 2, 3, 4, 5]);

        // Rows redrawn in place with the cursor up the screen aren't a burst.
        let mut anim = Animator::default();
        let cells = grid(12, 6, &["line 0", "line 1", "line 2"]);
        anim.update(0.0, &settings, &snap(&cells, 12, 5000, (3, 0)));
        let cells = grid(12, 6, &["line 40", "line 41", "line 42"]);
        anim.update(1.0, &settings, &snap(&cells, 12, 5000, (3, 0)));
        assert_eq!(anim.scroll_offset(1.0), 0.0);
        assert_eq!(lively_rows(&anim, 1.0), [0, 1, 2]);
    }

    #[test]
    fn a_line_matching_by_chance_does_not_hide_a_burst() {
        let settings = on();
        let mut anim = Animator::default();
        let before = grid(20, 6, &["Switch#", "a1", "a2", "a3", "a4", "Switch#show run"]);
        anim.update(0.0, &settings, &snap(&before, 20, 5000, (5, 15)));
        // The prompt on the top row is a later one that happens to look the same.
        let after = grid(20, 6, &["Switch#", "b1", "b2", "b3", "b4", "Switch#"]);
        anim.update(1.0, &settings, &snap(&after, 20, 5000, (5, 7)));
        assert_eq!(anim.scroll_offset(1.0), 6.0);
        assert_eq!(lively_rows(&anim, 1.0), [0, 1, 2, 3, 4, 5]);

        // Nor does the new prompt looking like the old top line: the text
        // didn't move down five rows.
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&before, 20, 5000, (5, 15)));
        let after = grid(20, 6, &["b0", "b1", "b2", "b3", "b4", "Switch#"]);
        anim.update(1.0, &settings, &snap(&after, 20, 5000, (5, 7)));
        assert_eq!(anim.scroll_offset(1.0), 6.0);
        assert_eq!(lively_rows(&anim, 1.0), [0, 1, 2, 3, 4, 5]);

        // Nor as many lines lining up as were lost.
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&before, 20, 5000, (5, 15)));
        let after = grid(20, 6, &["a2", "a3", "b2", "b3", "b4", "Switch#"]);
        anim.update(1.0, &settings, &snap(&after, 20, 5000, (5, 7)));
        assert_eq!(anim.scroll_offset(1.0), 6.0);
        assert_eq!(lively_rows(&anim, 1.0), [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn text_landing_on_a_blank_screen_is_not_a_scroll() {
        let settings = on();
        let mut anim = Animator::default();
        let cells = grid(20, 5, &["Switch#ping"]);
        anim.update(0.0, &settings, &snap(&cells, 20, 0, (1, 0)));
        let cells = grid(20, 5, &["Switch#ping", "!!"]);
        anim.update(1.0, &settings, &snap(&cells, 20, 0, (1, 2)));
        assert_eq!(anim.scroll_offset(1.0), 0.0);
        assert_eq!(lively_rows(&anim, 1.0), [1]);

        // More streams onto the same row: only what's new animates.
        let cells = grid(20, 5, &["Switch#ping", "!!!!!"]);
        anim.update(1.5, &settings, &snap(&cells, 20, 0, (1, 5)));
        assert_eq!(anim.scroll_offset(1.5), 0.0);
        let lively: Vec<usize> = (0..20).filter(|&c| anim.cell(1.5, 1, c).is_some()).collect();
        assert_eq!(lively, [2, 3, 4]);

        // A lone line growing at the bottom of an empty screen isn't a burst either.
        let mut anim = Animator::default();
        let cells = grid(20, 5, &["", "", "", "", "abc"]);
        anim.update(0.0, &settings, &snap(&cells, 20, 0, (4, 3)));
        let cells = grid(20, 5, &["", "", "", "", "abcdef"]);
        anim.update(1.0, &settings, &snap(&cells, 20, 0, (4, 6)));
        assert_eq!(anim.scroll_offset(1.0), 0.0);
        let lively: Vec<usize> = (0..20).filter(|&c| anim.cell(1.0, 4, c).is_some()).collect();
        assert_eq!(lively, [3, 4, 5]);
    }

    #[test]
    fn animations_move_up_with_their_text() {
        let settings = Animations { new_text: NewText::Heat, scrolling: Scrolling::Off, ..on() };
        let mut anim = Animator::default();
        let cells = grid(10, 4, &["r0", "r1", "r2", "r3"]);
        anim.update(0.0, &settings, &snap(&cells, 10, 0, (3, 2)));
        let cells = grid(10, 4, &["r0", "r1", "r2", "NEW"]);
        anim.update(1.0, &settings, &snap(&cells, 10, 0, (3, 3)));
        let cells = grid(10, 4, &["r2", "NEW", "x", "y"]);
        anim.update(1.2, &settings, &snap(&cells, 10, 2, (3, 1)));

        // Two rows up, and still 0.2 s into cooling down.
        let moved = anim.cell(1.2, 1, 0).unwrap();
        let expected = output_look(NewText::Heat, 0.2, 0).unwrap();
        assert!((moved.accent - expected.accent).abs() < 1e-3, "{moved:?}");
        assert!(moved.accent < 0.9);
        // What came in underneath has only just started.
        assert_eq!(anim.cell(1.2, 3, 0).unwrap().accent, 1.0);
        assert_eq!(anim.cell(1.2, 0, 0), None);
    }

    #[test]
    fn an_echo_is_told_apart_from_output() {
        let settings = Animations { reveal: Reveal::Typewriter, ..on() };

        // A key, then its echo on the cursor's row: typed, and never held back.
        let mut anim = at_prompt(&settings);
        anim.keystroke(1.0, &settings, 1, false, (0, 7));
        let cells = grid(30, 3, &["Switch#s"]);
        anim.update(1.05, &settings, &snap(&cells, 30, 0, (0, 8)));
        let look = anim.cell(1.05, 0, 7).unwrap();
        assert!(look.scale > 1.2 && look.accent > 0.5 && !look.hidden, "{look:?}");

        // The device saying something with nothing typed: output.
        let cells = grid(30, 3, &["Switch#s", "% Incomplete"]);
        anim.update(3.0, &settings, &snap(&cells, 30, 0, (1, 12)));
        assert_eq!(anim.cell(3.0, 1, 0).unwrap().scale, 1.0);
        assert!(anim.cell(3.0, 1, 5).unwrap().hidden);

        // Inserting mid-line redraws the rest of it: still an echo.
        let mut anim = at_prompt(&settings);
        let cells = grid(30, 3, &["Switch#sh ip"]);
        anim.update(0.5, &settings, &snap(&cells, 30, 0, (0, 9)));
        anim.keystroke(1.0, &settings, 1, false, (0, 9));
        let cells = grid(30, 3, &["Switch#shx ip"]);
        anim.update(1.05, &settings, &snap(&cells, 30, 0, (0, 10)));
        assert!(anim.cell(1.05, 0, 9).unwrap().scale > 1.0);
        assert!(anim.cell(1.05, 0, 12).unwrap().scale > 1.0);

        // An echo spilling onto a second row, far more than was typed, or
        // long after the key: output.
        let cases: [(&[&str], f64); 3] =
            [(&["Switch#s", "h"], 1.05), (&["Switch#show ip route"], 1.05), (&["Switch#s"], 2.5)];
        for (rows, when) in cases {
            let mut anim = at_prompt(&settings);
            anim.keystroke(1.0, &settings, 1, false, (0, 7));
            let cells = grid(30, 3, rows);
            anim.update(when, &settings, &snap(&cells, 30, 0, (1, 1)));
            let look = anim.cell(when, 0, 7).unwrap();
            assert_eq!(look.scale, 1.0, "{rows:?} at {when}: {look:?}");
        }
    }

    #[test]
    fn typewriter_goes_in_reading_order_and_never_falls_behind() {
        for pace in [1.0, 2.0] {
            let settings = Animations {
                reveal: Reveal::Typewriter,
                new_text: NewText::Off,
                cursor_motion: CursorMotion::Off,
                speed: pace,
                ..on()
            };
            let cap = REVEAL_CAP / f64::from(pace);
            let mut anim = Animator::default();
            anim.update(0.0, &settings, &snap(&grid(40, 12, &[]), 40, 0, (0, 0)));

            // A little: typed out at its own pace.
            let rows = ["x".repeat(30), "y".repeat(30), "z".repeat(30)];
            let few = grid(40, 12, &rows.each_ref().map(String::as_str));
            anim.update(1.0, &settings, &snap(&few, 40, 0, (3, 0)));
            let last = start(&anim, 2, 29);
            assert!((last - (1.0 + 89.0 * TYPEWRITER_STEP / f64::from(pace))).abs() < 1e-9, "pace {pace}: {last}");

            // A screenful would take a second at that rate, but it's all out in half of one.
            let mut anim = Animator::default();
            anim.update(0.0, &settings, &snap(&grid(40, 12, &[]), 40, 0, (0, 0)));
            let rows: Vec<String> = (0..10).map(|r| r.to_string().repeat(40)).collect();
            let full = grid(40, 12, &rows.iter().map(String::as_str).collect::<Vec<_>>());
            anim.update(1.0, &settings, &snap(&full, 40, 0, (10, 0)));
            let starts: Vec<f64> = anim.cells[..400].iter().map(|s| s.start).collect();
            assert!(starts.windows(2).all(|w| w[0] < w[1]), "not in reading order");
            assert_eq!(starts[0], 1.0);
            assert!(starts[399] <= 1.0 + cap + 1e-9, "pace {pace}: {}", starts[399]);

            // The first character is out, the rest held back, and the cursor
            // is drawn where the typing has got to.
            assert_eq!(anim.cell(1.0, 0, 0), None);
            assert!(anim.cell(1.0, 5, 0).unwrap().hidden);
            assert_eq!(anim.reveal_head(1.0), Some((0, 1)));
            assert_eq!(anim.cursor(1.0, (10, 0)).pos, (0.0, 1.0));

            // More arriving meanwhile waits its turn, but not past its own half second.
            let mut rows = rows.clone();
            rows.push("a".repeat(40));
            let more = grid(40, 12, &rows.iter().map(String::as_str).collect::<Vec<_>>());
            anim.update(1.1, &settings, &snap(&more, 40, 0, (11, 0)));
            assert!(start(&anim, 10, 0) > starts[399]);
            assert!(start(&anim, 10, 39) <= 1.1 + cap + 1e-9);
            assert_eq!(anim.reveal_head(1.1 + cap + 1e-6), None);
            assert_eq!(anim.cell(1.1 + cap + 1e-6, 10, 39), None);
        }
    }

    #[test]
    fn words_and_lines_show_whole() {
        let settings = Animations { reveal: Reveal::Words, ..on() };
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&grid(30, 4, &[]), 30, 0, (0, 0)));
        let cells = grid(30, 4, &["show ip  interface"]);
        anim.update(1.0, &settings, &snap(&cells, 30, 0, (1, 0)));
        let word = |from: usize, to: usize| {
            let first = start(&anim, 0, from);
            assert!((from..to).all(|c| start(&anim, 0, c) == first), "{from}..{to}");
            first
        };
        let (show, ip, interface) = (word(0, 4), word(5, 7), word(9, 18));
        assert!((ip - show - WORD_STEP).abs() < 1e-9 && (interface - ip - WORD_STEP).abs() < 1e-9);

        let settings = Animations { reveal: Reveal::Lines, ..on() };
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&grid(30, 4, &[]), 30, 0, (0, 0)));
        let cells = grid(30, 4, &["Interface  IP-Address", "Gi1/0/1    unassigned"]);
        anim.update(1.0, &settings, &snap(&cells, 30, 0, (2, 0)));
        assert!((0..21).all(|c| is_blank(cells[c]) || start(&anim, 0, c) == 1.0));
        assert!((0..21).all(|c| is_blank(cells[30 + c]) || start(&anim, 1, c) == 1.0 + LINE_STEP));
        // A row's mark waits for the row to show.
        assert!(anim.line_mark(1.0, 1).is_none());
        assert!(anim.line_mark(1.0 + LINE_STEP, 1).is_some());
    }

    #[test]
    fn typing_lets_held_back_output_straight_out() {
        let settings = Animations { reveal: Reveal::Lines, ..on() };
        let mut anim = at_prompt(&settings);
        let cells = grid(30, 3, &["output 0", "output 1", "output 2"]);
        anim.update(1.0, &settings, &snap(&cells, 30, 0, (2, 8)));
        assert!(anim.cell(1.0, 2, 0).unwrap().hidden);
        anim.keystroke(1.01, &settings, 1, false, (2, 8));
        assert_eq!(anim.reveal_head(1.01), None);
        assert!(!anim.cell(1.01, 2, 0).unwrap().hidden);
    }

    #[test]
    fn scrolling_starts_where_the_text_was_and_eases_home() {
        let settings = on();
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&numbered(0, 6), 12, 0, (5, 6)));
        anim.update(1.0, &settings, &snap(&numbered(2, 6), 12, 2, (5, 6)));
        assert_eq!(anim.scroll_offset(1.0), 2.0);
        let mid = anim.scroll_offset(1.07);
        assert!(0.0 < mid && mid < 1.0, "{mid}");

        // Two more lines before that's over: they add on, and it's all over
        // one duration after the last.
        anim.update(1.07, &settings, &snap(&numbered(4, 6), 12, 4, (5, 6)));
        assert!((anim.scroll_offset(1.07) - (mid + 2.0)).abs() < 1e-5);
        assert!(anim.scroll_offset(1.20) > 0.0);
        assert_eq!(anim.scroll_offset(1.22), 0.0);
        assert!(!anim.scroll.busy(1.22, settings.scrolling, 1.0));

        // A burst slides a screenful, no more.
        anim.update(2.0, &settings, &snap(&numbered(100, 6), 12, 100, (5, 6)));
        assert_eq!(anim.scroll_offset(2.0), 6.0);

        // Scrolling back yourself slides the text down...
        let cells = numbered(100, 6);
        anim.update(3.0, &settings, &Snapshot { display_offset: 3, ..snap(&cells, 12, 100, (5, 6)) });
        assert_eq!(anim.scroll_offset(3.0), -3.0);
        // ...and output arriving while you're back there leaves the view still.
        let cells = numbered(102, 6);
        anim.update(4.0, &settings, &Snapshot { display_offset: 5, ..snap(&cells, 12, 102, (5, 6)) });
        assert_eq!(anim.scroll_offset(4.0), 0.0);

        // Float takes longer, easing in as well as out.
        let settings = Animations { scrolling: Scrolling::Float, ..on() };
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&numbered(0, 6), 12, 0, (5, 6)));
        anim.update(1.0, &settings, &snap(&numbered(1, 6), 12, 1, (5, 6)));
        assert!(anim.scroll_offset(1.03) > 0.95);
        assert!(anim.scroll_offset(1.2) > 0.0);
        assert_eq!(anim.scroll_offset(1.3), 0.0);
    }

    #[test]
    fn a_springy_scroll_overshoots_then_rests() {
        let settings = Animations {
            scrolling: Scrolling::Spring,
            new_text: NewText::Off,
            new_lines: NewLines::Off,
            cursor_motion: CursorMotion::Off,
            ..on()
        };
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&numbered(0, 6), 12, 0, (5, 6)));
        anim.update(1.0, &settings, &snap(&numbered(3, 6), 12, 3, (5, 6)));
        assert_eq!(anim.scroll_offset(1.0), 3.0);

        // Another line mid-flight adds on without a jump.
        let before = anim.scroll_offset(1.05);
        anim.update(1.05, &settings, &snap(&numbered(4, 6), 12, 4, (5, 6)));
        assert!((anim.scroll_offset(1.05) - (before + 1.0)).abs() < 1e-4);

        let path: Vec<f32> = (0..1000).map(|ms| anim.scroll_offset(1.05 + f64::from(ms) / 1000.0)).collect();
        assert!(path.iter().any(|&x| x < -0.05), "no overshoot");
        let rest = anim.scroll.spring.settle;
        assert!(rest < 2.0, "{rest}");
        assert!(anim.next_wake(rest - 0.01).is_some());
        assert_eq!(anim.scroll_offset(rest), 0.0);
        assert_eq!(anim.next_wake(rest), None);
        // Close enough to rest by then that stopping can't be seen.
        assert!(anim.scroll_offset(rest - 0.001).abs() < SETTLED);
    }

    #[test]
    fn full_screen_apps_get_typing_effects_but_not_output_ones() {
        let settings = Animations { reveal: Reveal::Typewriter, ..on() };
        let alt = |cells, cursor| Snapshot { alt_screen: true, ..snap(cells, 20, 0, cursor) };
        let mut anim = Animator::default();
        let cells = grid(20, 4, &["one", "two", "three", "-- INSERT --"]);
        anim.update(0.0, &settings, &alt(&cells, (0, 0)));

        // The app drawing and scrolling its text: nothing animates.
        let cells = grid(20, 4, &["two", "three", "four", "-- INSERT --  3,5"]);
        anim.update(1.0, &settings, &alt(&cells, (2, 4)));
        assert_eq!(anim.scroll_offset(1.0), 0.0);
        assert_eq!(lively_rows(&anim, 1.0), Vec::<usize>::new());
        assert!((0..4).all(|row| anim.line_mark(1.0, row).is_none()));
        assert_still(&anim, 1.5);

        // Typing: the echo pops, though the app updated its ruler too.
        anim.keystroke(2.0, &settings, 1, false, (2, 4));
        let cells = grid(20, 4, &["two", "three", "fours", "-- INSERT --  3,6"]);
        anim.update(2.02, &settings, &alt(&cells, (2, 5)));
        assert!(anim.cell(2.02, 2, 4).unwrap().scale > 1.0);
        assert_eq!(lively_rows(&anim, 2.02), [2]);
    }

    #[test]
    fn fresh_output_lines_are_marked_but_typing_is_not() {
        let settings = on();
        let mut anim = at_prompt(&settings);
        anim.keystroke(1.0, &settings, 1, false, (0, 7));
        let cells = grid(30, 3, &["Switch#s"]);
        anim.update(1.02, &settings, &snap(&cells, 30, 0, (0, 8)));
        assert!(anim.line_mark(1.02, 0).is_none());

        let cells = grid(30, 3, &["Switch#s", "  hello world", "Switch#"]);
        anim.update(2.0, &settings, &snap(&cells, 30, 0, (2, 7)));
        assert!(anim.line_mark(2.0, 0).is_none());
        let mark = anim.line_mark(2.0, 1).unwrap();
        assert_eq!((mark.style, mark.start, mark.end), (NewLines::Glow, 2, 13));
        assert!((mark.alpha - 0.22).abs() < 1e-6);
        assert_eq!(anim.line_mark(2.0, 2).map(|m| (m.start, m.end)), Some((0, 7)));
        assert!(anim.line_mark(2.69, 1).unwrap().alpha < 0.01);
        assert!(anim.line_mark(2.7, 1).is_none());

        // Underline sweeps across; Shimmer's band crosses the text.
        let underline = LineMark::new(NewLines::Underline, 0.2, 2, 12);
        assert!(underline.reach > 0.0 && underline.reach < 10.0);
        assert_eq!(LineMark::new(NewLines::Underline, 0.5, 2, 12).reach, 10.0);
        let early = LineMark::new(NewLines::Shimmer, 0.1, 2, 12);
        let late = LineMark::new(NewLines::Shimmer, 0.9, 2, 12);
        assert!(early.band(3.5) > late.band(3.5) && late.band(11.5) > early.band(11.5));
    }

    #[test]
    fn the_cursor_glides_springs_smears_and_leaves_ghosts() {
        fn moved(style: CursorMotion) -> Animator {
            let settings = Animations { cursor_motion: style, ..on() };
            let mut anim = Animator::default();
            let cells = grid(30, 12, &["Switch#"]);
            anim.update(0.0, &settings, &snap(&cells, 30, 0, (0, 7)));
            anim.update(1.0, &settings, &snap(&cells, 30, 0, (0, 17)));
            anim
        }
        let at = |anim: &Animator, now: f64| anim.cursor(now, (0, 17));

        let anim = moved(CursorMotion::Off);
        assert_eq!(at(&anim, 1.0).pos, (0.0, 17.0));

        let anim = moved(CursorMotion::Glide);
        assert_eq!(at(&anim, 1.0).pos, (0.0, 7.0));
        let (_, column) = at(&anim, 1.045).pos;
        assert!(7.0 < column && column < 17.0);
        assert_eq!(at(&anim, 1.09).cell(), Some((0, 17)));
        assert_eq!(anim.next_wake(1.09), None);
        // Asked about a cursor it wasn't told of, it draws that one as it is.
        assert_eq!(anim.cursor(1.0, (1, 0)).pos, (1.0, 0.0));

        let anim = moved(CursorMotion::Spring);
        let path: Vec<f32> = (0..500).map(|ms| at(&anim, 1.0 + f64::from(ms) / 1000.0).pos.1).collect();
        assert!(path.iter().any(|&c| c > 17.05), "no overshoot");
        let rest = anim.motion.spring[1].settle;
        assert!(rest < 1.5);
        assert_eq!(at(&anim, rest).pos, (0.0, 17.0));

        let anim = moved(CursorMotion::Smear);
        assert_eq!(at(&anim, 1.0).pos, (0.0, 17.0));
        assert_eq!(at(&anim, 1.0).tail, Some((0.0, 7.0)));
        assert_eq!(at(&anim, 1.12).tail, None);

        let anim = moved(CursorMotion::Ghost);
        let look = at(&anim, 1.0);
        assert_eq!(look.pos, (0.0, 17.0));
        let spots: Vec<(f32, f32)> = look.ghosts.iter().map(|g| g.0).collect();
        assert_eq!(spots, [(0.0, 7.0), (0.0, 10.0), (0.0, 14.0)]);
        assert!(look.ghosts.windows(2).all(|w| w[0].1 < w[1].1), "fresher nearer the cursor");
        assert!(at(&anim, 1.26).ghosts.is_empty());

        // A long jump (a clear, say) just lands.
        let settings = on();
        let mut anim = Animator::default();
        let cells = grid(30, 12, &["Switch#"]);
        anim.update(0.0, &settings, &snap(&cells, 30, 0, (11, 7)));
        anim.update(1.0, &settings, &snap(&cells, 30, 0, (0, 7)));
        assert_eq!(anim.cursor(1.0, (0, 7)).pos, (0.0, 7.0));

        // The cursor rides up with the text as the screen scrolls: from the
        // end of the command, now a row up, to the new line.
        let settings = Animations { scrolling: Scrolling::Off, ..on() };
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&numbered(0, 6), 12, 0, (5, 6)));
        anim.update(1.0, &settings, &snap(&numbered(1, 6), 12, 1, (5, 0)));
        assert_eq!(anim.cursor(1.0, (5, 0)).pos, (4.0, 6.0));
    }

    #[test]
    fn particles_burn_out_and_are_capped() {
        let settings = on();
        let mut anim = at_prompt(&settings);
        for i in 0..100 {
            anim.keystroke(1.0 + f64::from(i) * 0.001, &settings, 0, false, (0, 7));
        }
        assert_eq!(anim.particles.len(), MAX_PARTICLES);
        assert_eq!(anim.particles(1.1).count(), MAX_PARTICLES);
        // The oldest made way for the newest.
        assert!(anim.particles.iter().all(|p| p.born > 1.0));
        assert!(anim.particles.iter().any(|p| (p.born - 1.099).abs() < 1e-9));

        assert_eq!(anim.particles(1.8).count(), 0);
        assert_eq!(anim.next_wake(1.8), None);
        let cells = grid(30, 3, &["Switch#"]);
        anim.update(1.8, &settings, &snap(&cells, 30, 0, (0, 7)));
        assert!(anim.particles.is_empty());
    }

    #[test]
    fn creative_bursts_are_bounded_finite_and_finish_without_changing_output() {
        for style in
            [KeystrokeBurst::Explosion, KeystrokeBurst::Lasers, KeystrokeBurst::Lightning, KeystrokeBurst::Portal]
        {
            let mut rng = Rng(1234);
            for i in 0..18 {
                let particle = Particle::new(style, i, 18, 0, 1.0, 1.0, (5.0, 5.0), &mut rng);
                for age in [0.001, 0.05, 0.2] {
                    let look = particle.look(1.0 + age).unwrap();
                    assert!(look.pos.0.is_finite() && look.pos.1.is_finite());
                    assert!(look.size.is_finite() && look.size >= 0.0);
                    assert!((0.0..=1.0).contains(&look.alpha));
                }
                assert!(particle.look(2.0).is_none());
            }
        }
        for style in [NewText::Matrix, NewText::Hologram] {
            assert!(output_look(style, 0.01, 123).is_some());
            assert!(output_look(style, 1.0, 123).is_none());
        }
        for age in [0.01, 0.05, 0.1] {
            assert!(matches!(output_look(NewText::Matrix, age, 123).unwrap().glyph, Some('0' | '1')));
        }
    }

    #[test]
    fn every_burst_looks_like_itself() {
        let burst = |style: KeystrokeBurst| {
            let settings = Animations { keystroke_burst: style, ..on() };
            let mut anim = at_prompt(&settings);
            anim.keystroke(1.0, &settings, 1, false, (0, 7));
            anim
        };
        let centre = (0.5, 7.5);
        assert_eq!(burst(KeystrokeBurst::Off).particles(1.0).count(), 0);

        let sparks: Vec<_> = burst(KeystrokeBurst::Sparks).particles(1.05).collect();
        assert!((6..=9).contains(&sparks.len()));
        assert!(sparks.iter().all(|p| p.shape == ParticleShape::Dot && p.color == ParticleColor::Accent));

        let anim = burst(KeystrokeBurst::Confetti);
        let confetti: Vec<_> = anim.particles(1.5).collect();
        assert!((6..=8).contains(&confetti.len()));
        assert!(confetti.iter().all(|p| matches!(p.shape, ParticleShape::Square { .. })));
        let colors: HashSet<_> = confetti
            .iter()
            .map(|p| match p.color {
                ParticleColor::Fixed(c) if CONFETTI.contains(&c) => c,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(colors.len(), CONFETTI.len());
        // Thrown up, then falling.
        let later: Vec<_> = anim.particles(1.55).collect();
        assert!(confetti.iter().zip(&later).all(|(a, b)| b.pos.0 > a.pos.0));

        let embers: Vec<_> = burst(KeystrokeBurst::Embers).particles(1.3).collect();
        assert!((5..=7).contains(&embers.len()));
        assert!(embers.iter().all(|p| matches!(p.color, ParticleColor::Fixed(c) if EMBERS.contains(&c))));
        assert!(embers.iter().all(|p| p.shape == ParticleShape::Dot && p.pos.0 < centre.0 - 0.3));

        let anim = burst(KeystrokeBurst::Bubbles);
        let bubbles: Vec<_> = anim.particles(1.3).collect();
        assert!((3..=5).contains(&bubbles.len()));
        assert!(bubbles.iter().all(|p| p.shape == ParticleShape::Ring && p.pos.0 < centre.0 - 0.3));
        let young: Vec<_> = anim.particles(1.01).collect();
        assert!(young.iter().zip(&bubbles).all(|(a, b)| b.size > a.size));

        let stars: Vec<_> = burst(KeystrokeBurst::Stars).particles(1.1).collect();
        assert!((4..=6).contains(&stars.len()));
        assert!(stars.iter().all(|p| matches!(p.shape, ParticleShape::Star { .. })));

        let anim = burst(KeystrokeBurst::Ripple);
        let ripple: Vec<_> = anim.particles(1.1).collect();
        assert_eq!(ripple.len(), 1);
        assert_eq!((ripple[0].shape, ripple[0].pos), (ParticleShape::Ring, centre));
        let wider = anim.particles(1.3).next().unwrap();
        assert!(wider.size > ripple[0].size && wider.alpha < ripple[0].alpha);
    }

    #[test]
    fn it_goes_idle_once_everything_has_settled() {
        let settings = Animations {
            cursor_motion: CursorMotion::Spring,
            typed_text: TypedText::Bounce,
            keystroke_burst: KeystrokeBurst::Confetti,
            shake: Shake::Strong,
            new_text: NewText::Heat,
            reveal: Reveal::Lines,
            scrolling: Scrolling::Float,
            new_lines: NewLines::Marker,
            ..on()
        };
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &snap(&numbered(0, 6), 12, 0, (5, 6)));
        assert_eq!(anim.next_wake(0.0), None);
        anim.keystroke(1.0, &settings, 1, false, (5, 6));
        assert_ne!(anim.shake(1.01), Vec2::ZERO);
        anim.update(1.0, &settings, &snap(&numbered(2, 6), 12, 2, (5, 0)));
        assert_eq!(anim.next_wake(1.0), Some(0.0));

        let cells = numbered(2, 6);
        anim.update(4.0, &settings, &snap(&cells, 12, 2, (5, 0)));
        assert_still(&anim, 4.0);
        assert_eq!(anim.shake(4.0), Vec2::ZERO);
        assert_eq!(anim.cursor(4.0, (5, 0)).pos, (5.0, 0.0));

        // Turned off, it's idle and draws nothing.
        let off = Animations { enabled: false, ..settings };
        anim.update(5.0, &off, &snap(&cells, 12, 2, (5, 0)));
        assert_still(&anim, 5.0);
    }

    #[test]
    fn keeps_up_with_a_real_terminal() {
        let settings = on();
        let fill = |emu: &mut Emulator| {
            for i in 0..12 {
                emu.feed(format!("line {i}\r\n").as_bytes());
            }
        };
        let mut cells = Vec::new();
        for scrollback in [0, 3, 1000] {
            let mut emu = Emulator::new(20, 5, scrollback, Arc::new(|_| {}));
            fill(&mut emu);
            let mut anim = Animator::default();
            anim.update(0.0, &settings, &capture(&emu, &mut cells));
            emu.feed(b"line 12\r\nline 13\r\n");
            anim.update(1.0, &settings, &capture(&emu, &mut cells));
            assert_eq!(anim.scroll_offset(1.0), 2.0, "scrollback {scrollback}");
            assert_eq!(lively_rows(&anim, 1.0), [2, 3], "scrollback {scrollback}");
        }

        // Scrolled back, the view holds still as output arrives...
        let mut emu = Emulator::new(20, 5, 1000, Arc::new(|_| {}));
        fill(&mut emu);
        emu.scroll_by(2);
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &capture(&emu, &mut cells));
        emu.feed(b"line 12\r\n");
        anim.update(1.0, &settings, &capture(&emu, &mut cells));
        assert_eq!(anim.scroll_offset(1.0), 0.0);

        // ...unless the history is full and its oldest lines go from under
        // the view: then what's shown moves up.
        let mut emu = Emulator::new(20, 5, 3, Arc::new(|_| {}));
        fill(&mut emu);
        emu.scroll_by(3);
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &capture(&emu, &mut cells));
        emu.feed(b"line 12\r\n");
        anim.update(1.0, &settings, &capture(&emu, &mut cells));
        assert_eq!(anim.scroll_offset(1.0), 1.0);
    }

    /// docker build's progress, the `n`th time it's drawn: the cursor goes
    /// back up over the block and prints it all again, a timer ticking in
    /// the header, the finished steps the same every time, and the tail of
    /// a log rolling on a line at a time.
    fn build_progress(n: usize) -> String {
        let mut lines = vec![format!("[+] Building {:.1}s (4/7)", n as f64 / 10.0)];
        let steps = ["FROM docker.io/library/alpine:3.20", "WORKDIR /src", "COPY . .", "RUN apk add build-base"];
        lines.extend(steps.iter().enumerate().map(|(i, step)| format!(" => [{}/7] {step}", i + 1)));
        // After a while the next step starts, and its log replaces the last.
        let log = if n < 30 { "cc -c module{}.c -o module{}.o" } else { "strip --strip-unneeded lib{}.so" };
        lines.extend((n..n + 6).map(|i| format!(" => => # {}", log.replace("{}", &i.to_string()))));
        let up = if n == 0 { String::new() } else { format!("\x1b[{}A", lines.len()) };
        up + &lines.iter().map(|line| format!("\r\x1b[2K{line}\r\n")).collect::<String>()
    }

    #[test]
    fn a_block_redrawn_in_place_stays_put() {
        for &reveal in Reveal::ALL {
            let settings = Animations { reveal, ..on() };
            let mut emu = Emulator::new(60, 12, 1000, Arc::new(|_| {}));
            for i in 0..30 {
                emu.feed(format!("earlier output {i}\r\n").as_bytes());
            }
            emu.feed(build_progress(0).as_bytes());
            let mut cells = Vec::new();
            let mut anim = Animator::default();
            anim.update(0.0, &settings, &capture(&emu, &mut cells));
            let mut newest = NEVER;
            for n in 1..30 {
                let now = n as f64 / 10.0;
                emu.feed(build_progress(n).as_bytes());
                anim.update(now, &settings, &capture(&emu, &mut cells));
                let at = |t: f64| format!("{reveal:?}, drawn {n} times, at {t}");
                for t in [now, now + 0.03, now + 0.06, now + 0.09] {
                    // Nothing slides, and the finished steps never change:
                    // they don't animate, aren't marked and never hide.
                    assert_eq!(anim.scroll_offset(t), 0.0, "{}", at(t));
                    for row in 1..5 {
                        assert!((0..60).all(|c| anim.cell(t, row, c).is_none()), "row {row}: {}", at(t));
                        assert!(anim.line_mark(t, row).is_none(), "row {row}: {}", at(t));
                    }
                    // Nor does the header, but for its timer.
                    assert!((0..13).all(|c| anim.cell(t, 0, c).is_none()), "header: {}", at(t));
                }
                // The log line that moved up a row kept what it had; only the
                // newest is new.
                assert_eq!(start(&anim, 9, 1), newest, "{}", at(now));
                newest = start(&anim, 10, 1);
                assert!(newest >= now, "{}", at(now));
            }

            // A whole new log, where nothing lines up, is no burst either:
            // most of the block stayed put.
            emu.feed(build_progress(40).as_bytes());
            anim.update(4.0, &settings, &capture(&emu, &mut cells));
            assert_eq!(anim.scroll_offset(4.0), 0.0, "{reveal:?}");
            assert_eq!(lively_rows(&anim, 4.0), [0, 5, 6, 7, 8, 9, 10], "{reveal:?}");
        }
    }

    #[test]
    fn a_pager_going_back_moves_the_text_down() {
        // git log and journalctl run less without the alternate screen.
        // Going back a line inserts it at the top, then puts the prompt
        // back on the bottom row.
        let settings = Animations { reveal: Reveal::Lines, ..on() };
        let mut emu = Emulator::new(40, 8, 1000, Arc::new(|_| {}));
        for i in 0..30 {
            emu.feed(format!("commit {i:04} message\r\n").as_bytes());
        }
        emu.feed(b":");
        let mut cells = Vec::new();
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &capture(&emu, &mut cells));
        anim.keystroke(1.0, &settings, 1, false, (7, 1));
        emu.feed(b"\x1b[H\x1b[Lcommit 0022 message\x1b[8;1H\r\x1b[K:");
        anim.update(1.02, &settings, &capture(&emu, &mut cells));
        // The prompt stayed put, so the screen can't slide as a whole; only
        // the line that came in at the top is new.
        assert_eq!(anim.scroll_offset(1.02), 0.0);
        assert_eq!(lively_rows(&anim, 1.02), [0]);
        assert!((1..8).all(|row| anim.line_mark(1.02, row).is_none()));
        assert_eq!(anim.reveal_head(1.02), None);

        // A page back is drawn over the old one, prompt and all, ending on
        // the bottom row: nothing scrolled.
        emu.feed(b"\x1b[H");
        for i in 15..22 {
            emu.feed(format!("\x1b[Kcommit {i:04} message\r\n").as_bytes());
        }
        emu.feed(b"\x1b[K:");
        anim.update(3.0, &settings, &capture(&emu, &mut cells));
        assert_eq!(anim.scroll_offset(3.0), 0.0);
        assert!(!lively_rows(&anim, 3.0).contains(&7));

        // With nothing held in place, the whole screen slides down.
        let settings = Animations { scrolling: Scrolling::Smooth, ..on() };
        let mut emu = Emulator::new(20, 5, 1000, Arc::new(|_| {}));
        emu.feed(b"line 0");
        for i in 1..10 {
            emu.feed(format!("\r\nline {i}").as_bytes());
        }
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &capture(&emu, &mut cells));
        emu.feed(b"\x1b[H\x1bMline 4");
        anim.update(1.0, &settings, &capture(&emu, &mut cells));
        assert_eq!(anim.scroll_offset(1.0), -1.0);
        assert_eq!(lively_rows(&anim, 1.0), [0]);
    }

    #[test]
    fn a_log_cycling_through_a_few_lines_is_never_taken_for_scrolling_back() {
        // Lines repeating every four make text going up three rows look just
        // like it going down one, and two up like two down. With no history
        // to say which (no scrollback, or already full), only the order the
        // candidates are tried in can: output goes up.
        let log = ["[worker] poll start", "[worker] read 42 registers", "[worker] write ok", "[worker] poll done"];
        let cycle: usize = log.iter().map(|line| line.len() + 2).sum();
        let stream: String = log.iter().cycle().take(400).map(|line| format!("{line}\r\n")).collect();
        let (early, rest) = stream.as_bytes().split_at(16 * cycle);
        let settings = Animations { scrolling: Scrolling::Smooth, ..on() };
        for scrollback in [0, 5] {
            let mut emu = Emulator::new(80, 24, scrollback, Arc::new(|_| {}));
            emu.feed(early);
            let mut cells = Vec::new();
            let mut anim = Animator::default();
            anim.update(0.0, &settings, &capture(&emu, &mut cells));
            for (frame, chunk) in rest.chunks(64).take(44).enumerate() {
                let now = 1.0 + frame as f64 * 0.016;
                let at = format!("scrollback {scrollback}, frame {frame} at {now}");
                let before: Vec<f64> = anim.cells.iter().map(|state| state.start).collect();
                emu.feed(chunk);
                anim.update(now, &settings, &capture(&emu, &mut cells));

                // The text never slid down.
                assert!(anim.scroll.moves.iter().all(|&(_, rows)| rows >= 0.0), "{at}: {:?}", anim.scroll.moves);

                // A whole number of cycles looks the same moved or not, so
                // there's nothing to say about those.
                let lines = chunk.iter().filter(|&&b| b == b'\n').count();
                if lines % log.len() == 0 {
                    continue;
                }
                // Every finished row kept what it had as it moved up...
                for row in 0..23 - lines {
                    for column in 0..80 {
                        let index = row * 80 + column;
                        assert_eq!(anim.cells[index].start, before[index + lines * 80], "{at}: row {row}");
                    }
                }
                // ...and the lines that came in under them are the new ones.
                let fresh = |row: usize| (0..80).all(|c| is_blank(cells[row * 80 + c]) || start(&anim, row, c) >= now);
                assert!((24 - lines..24).all(fresh), "{at}");
            }
        }
    }

    #[test]
    fn repeating_logs_keep_scrolling_forward_across_chunk_sizes_and_reveal_styles() {
        let (columns, lines) = (80, 24);
        for period in 2..=7 {
            let stream: String = (0..240).map(|i| format!("[worker] stage {} complete\r\n", i % period)).collect();
            let split = stream.match_indices('\n').nth(63).unwrap().0 + 1;
            let (early, rest) = stream.as_bytes().split_at(split);
            for scrollback in [0, 5, 1000] {
                for chunk_size in [17, 64, 200] {
                    for &reveal in Reveal::ALL {
                        let settings = Animations { reveal, speed: 0.5, ..on() };
                        let cap = REVEAL_CAP / f64::from(settings.pace());
                        let mut emu = Emulator::new(columns, lines, scrollback, Arc::new(|_| {}));
                        emu.feed(early);
                        let mut cells = Vec::new();
                        let mut anim = Animator::default();
                        anim.update(0.0, &settings, &capture(&emu, &mut cells));
                        let mut born = vec![NEVER; columns * lines];
                        let mut now = 0.0;
                        for (frame, chunk) in rest.chunks(chunk_size).enumerate() {
                            now = 1.0 + frame as f64 * 0.016;
                            let before = cells.clone();
                            let previous_born = born.clone();
                            let shift = chunk.iter().filter(|&&b| b == b'\n').count();
                            emu.feed(chunk);
                            anim.update(now, &settings, &capture(&emu, &mut cells));
                            let at = || {
                                format!(
                                    "period {period}, history {scrollback}, chunk {chunk_size}, {reveal:?}, frame {frame}"
                                )
                            };
                            assert!(
                                anim.scroll.moves.iter().all(|&(_, rows)| rows >= 0.0),
                                "{}: {:?}",
                                at(),
                                anim.scroll.moves
                            );
                            for index in 0..cells.len() {
                                // These streams have no cursor commands or
                                // wrapping: each newline scrolls exactly once.
                                let source = index + shift * columns;
                                born[index] = if source < cells.len() && cells[index] == before[source] {
                                    previous_born[source]
                                } else {
                                    now
                                };
                                if now >= born[index] + cap {
                                    assert!(
                                        anim.cell(now, index / columns, index % columns)
                                            .is_none_or(|look| !look.hidden),
                                        "{}: cell {index} hidden past its arrival deadline",
                                        at()
                                    );
                                }
                            }
                        }
                        anim.update(now + 5.0, &settings, &capture(&emu, &mut cells));
                        assert_still(&anim, now + 5.5);
                    }
                }
            }
        }
    }

    #[test]
    fn a_status_line_under_a_scroll_region_stays_put() {
        // apt keeps its progress on the bottom row and scrolls the rows
        // above it, which still adds them to the history.
        let settings = on();
        let mut emu = Emulator::new(30, 6, 1000, Arc::new(|_| {}));
        for i in 0..5 {
            emu.feed(format!("Reading package lists {i}\r\n").as_bytes());
        }
        emu.feed(b"\r\n\x1b[1;5r\x1b[6;1HProgress: [ 10%]\x1b[5;1H");
        let mut cells = Vec::new();
        let mut anim = Animator::default();
        anim.update(0.0, &settings, &capture(&emu, &mut cells));

        emu.feed(b"Unpacking libc6 ...\r\n");
        anim.update(1.0, &settings, &capture(&emu, &mut cells));
        assert_eq!(anim.scroll_offset(1.0), 0.0);
        assert_eq!(lively_rows(&anim, 1.0), [3]);
        assert!(anim.line_mark(1.0, 5).is_none());

        // The progress moving on as a line comes out: only what changed.
        emu.feed(b"Unpacking zlib1g ...\r\n\x1b7\x1b[6;1HProgress: [ 20%]\x1b8");
        anim.update(2.0, &settings, &capture(&emu, &mut cells));
        assert_eq!(anim.scroll_offset(2.0), 0.0);
        assert_eq!(lively_rows(&anim, 2.0), [3, 5]);
        let changed: Vec<usize> = (0..30).filter(|&c| anim.cell(2.0, 5, c).is_some()).collect();
        assert_eq!(changed, [12]);
    }

    #[test]
    fn keys_never_echoed_are_not_waited_for_after_enter() {
        // A password typed blind, then Enter: the prompt that follows is
        // the device talking, not an echo.
        let settings = on();
        let mut anim = Animator::default();
        let cells = grid(30, 3, &["Password: "]);
        anim.update(0.0, &settings, &snap(&cells, 30, 0, (0, 10)));
        for i in 0..8 {
            anim.keystroke(1.0 + f64::from(i) * 0.1, &settings, 1, false, (0, 10));
        }
        anim.keystroke(1.8, &settings, 0, true, (0, 10));
        let cells = grid(30, 3, &["Password: ", "Switch>"]);
        anim.update(1.85, &settings, &snap(&cells, 30, 0, (1, 7)));
        let look = anim.cell(1.85, 1, 0).unwrap();
        assert_eq!(look.scale, 1.0, "{look:?}");
        assert!(anim.line_mark(1.85, 1).is_some());

        // The same when the new line comes a frame before the prompt.
        let mut anim = Animator::default();
        let cells = grid(30, 3, &["Password: "]);
        anim.update(0.0, &settings, &snap(&cells, 30, 0, (0, 10)));
        for i in 0..8 {
            anim.keystroke(1.0 + f64::from(i) * 0.1, &settings, 1, false, (0, 10));
        }
        anim.keystroke(1.8, &settings, 0, true, (0, 10));
        anim.update(1.83, &settings, &snap(&cells, 30, 0, (1, 0)));
        let cells = grid(30, 3, &["Password: ", "Switch>"]);
        anim.update(1.86, &settings, &snap(&cells, 30, 0, (1, 7)));
        let look = anim.cell(1.86, 1, 0).unwrap();
        assert_eq!(look.scale, 1.0, "{look:?}");
        assert!(anim.line_mark(1.86, 1).is_some());
    }

    #[test]
    fn once_the_device_has_moved_on_to_a_new_line_nothing_is_waited_for() {
        let settings = on();
        let mut anim = Animator::default();
        let cells = grid(30, 3, &["Password: "]);
        anim.update(0.0, &settings, &snap(&cells, 30, 0, (0, 10)));
        for i in 0..3 {
            anim.keystroke(1.0 + f64::from(i) * 0.1, &settings, 1, false, (0, 10));
        }
        anim.keystroke(1.3, &settings, 0, true, (0, 10));
        anim.update(1.5, &settings, &snap(&cells, 30, 0, (1, 0)));
        // A change back on the first row, no more than the keys typed,
        // isn't their echo now.
        let cells = grid(30, 3, &["Password: ok!"]);
        anim.update(1.6, &settings, &snap(&cells, 30, 0, (1, 0)));
        let look = anim.cell(1.6, 0, 10).unwrap();
        assert_eq!(look.scale, 1.0, "{look:?}");
        assert!(anim.line_mark(1.6, 0).is_some());
    }

    #[test]
    fn typing_ahead_after_enter_starts_afresh() {
        // The next command's first key goes in before the prompt it's for
        // is even on the screen.
        let settings = on();
        let mut anim = Animator::default();
        let cells = grid(30, 3, &["Password: "]);
        anim.update(0.0, &settings, &snap(&cells, 30, 0, (0, 10)));
        for i in 0..8 {
            anim.keystroke(1.0 + f64::from(i) * 0.1, &settings, 1, false, (0, 10));
        }
        anim.keystroke(1.8, &settings, 0, true, (0, 10));
        anim.keystroke(1.9, &settings, 1, false, (0, 10));
        let cells = grid(30, 3, &["Password: ", "Switch>"]);
        anim.update(1.95, &settings, &snap(&cells, 30, 0, (1, 7)));
        assert_eq!(anim.cell(1.95, 1, 0).unwrap().scale, 1.0);
        assert!(anim.line_mark(1.95, 1).is_some());

        let cells = grid(30, 3, &["Password: ", "Switch>x"]);
        anim.update(2.0, &settings, &snap(&cells, 30, 0, (1, 8)));
        assert!(anim.cell(2.0, 1, 7).unwrap().scale > 1.2);
    }

    #[test]
    fn echoes_arriving_after_enter_are_still_typed() {
        // A slow link: each key's echo takes 150 ms to come back, so the last
        // few are still on their way when Enter is pressed.
        let settings = on();
        let mut anim = at_prompt(&settings);
        for at in [1.0, 1.04, 1.08, 1.12] {
            anim.keystroke(at, &settings, 1, false, (0, 7));
        }
        let mut shown = String::new();
        for (column, (key, arrives)) in [('e', 1.15), ('x', 1.19), ('i', 1.23), ('t', 1.27)].into_iter().enumerate() {
            if key == 'x' {
                anim.keystroke(1.16, &settings, 0, true, (0, 8));
            }
            shown.push(key);
            let cells = grid(30, 3, &[&format!("Switch#{shown}")]);
            anim.update(arrives, &settings, &snap(&cells, 30, 0, (0, 8 + column)));
            let look = anim.cell(arrives, 0, 7 + column).unwrap();
            assert!(look.scale > 1.2 && look.accent > 0.5 && !look.hidden, "{key}: {look:?}");
            assert!(anim.line_mark(arrives, 0).is_none(), "{key}");
        }

        // What the device says once it has the line is output.
        let cells = grid(30, 3, &["Switch#exit", "Switch>"]);
        anim.update(1.31, &settings, &snap(&cells, 30, 0, (1, 7)));
        assert_eq!(anim.cell(1.31, 1, 0).unwrap().scale, 1.0);
        assert!(anim.line_mark(1.31, 1).is_some());
        assert!(anim.line_mark(1.31, 0).is_none());
    }

    #[test]
    fn a_late_echo_and_the_reply_arriving_together_keep_their_own_effects() {
        for &reveal in Reveal::ALL {
            let settings = Animations { reveal, ..on() };
            let mut emu = Emulator::new(40, 6, 100, Arc::new(|_| {}));
            emu.feed(b"Switch#");
            let mut cells = Vec::new();
            let mut anim = Animator::default();
            anim.update(0.0, &settings, &capture(&emu, &mut cells));
            for at in [1.0, 1.04, 1.08, 1.12] {
                anim.keystroke(at, &settings, 1, false, (0, 7));
            }
            anim.keystroke(1.16, &settings, 0, true, (0, 7));

            // A single transport read can contain both the late echo and
            // the command's reply; typing must never join the reveal queue.
            emu.feed(b"exit\r\nConnection closed\r\nSwitch>");
            anim.update(1.2, &settings, &capture(&emu, &mut cells));
            for column in 7..11 {
                let look = anim.cell(1.2, 0, column).unwrap();
                assert!(look.scale > 1.2 && look.accent > 0.5 && !look.hidden, "{reveal:?}: {look:?}");
            }
            assert!(anim.line_mark(1.2, 0).is_none(), "{reveal:?}: the echo got an output mark");
            for row in 1..3 {
                assert!(anim.line_mark(1.7, row).is_some(), "{reveal:?}: the reply lost its output mark");
                assert_eq!(anim.cells[row * 40].kind, Kind::Output, "{reveal:?}: reply row {row}");
            }
            assert_eq!(anim.pending_echo, 0);
            assert_eq!(anim.reveal_head(1.7), None, "{reveal:?}: the reply missed its deadline");
            assert_eq!(emu.screen_text().trim_end(), "Switch#exit\nConnection closed\nSwitch>");
            anim.update(3.0, &settings, &capture(&emu, &mut cells));
            assert_still(&anim, 3.5);
        }
    }

    #[test]
    fn a_line_growing_as_it_scrolls_is_the_same_line() {
        // A mostly blank screen, and no history to say it scrolled (none
        // kept, or already full): the last line grows and wraps in one go.
        let settings = on();
        for scrollback in [0, 3] {
            let mut emu = Emulator::new(20, 6, scrollback, Arc::new(|_| {}));
            for i in 0..10 {
                emu.feed(format!("old {i}\r\n").as_bytes());
            }
            emu.feed(b"\x1b[2J\x1b[H\r\n\r\n\r\n\r\n\r\n");
            let mut cells = Vec::new();
            let mut anim = Animator::default();
            anim.update(0.0, &settings, &capture(&emu, &mut cells));
            emu.feed(b"ip show");
            anim.update(1.0, &settings, &capture(&emu, &mut cells));
            emu.feed(b" interface brief");
            anim.update(1.05, &settings, &capture(&emu, &mut cells));
            assert_eq!(anim.scroll_offset(1.05), 1.0, "scrollback {scrollback}");
            // What was already there carries on as it was; only the rest is new.
            assert_eq!(start(&anim, 4, 0), 1.0, "scrollback {scrollback}");
            assert_eq!(start(&anim, 4, 8), 1.05, "scrollback {scrollback}");
            assert_eq!(start(&anim, 5, 0), 1.05, "scrollback {scrollback}");
        }
    }

    #[test]
    fn text_shown_together_looks_alike() {
        // The view draws neighbouring cells as one run of text only if they
        // look the same. Decode tints by age alone, whatever each cell's
        // glyphs...
        for age in [0.0, 0.05, 0.1, 0.15, 0.2] {
            let tints: HashSet<u32> = (0..64u32)
                .filter_map(|i| output_look(NewText::Decode, age, i.wrapping_mul(0x9e37_79b9)))
                .map(|look| look.accent.to_bits())
                .collect();
            assert_eq!(tints.len(), 1, "at {age}: {tints:?}");
        }
        // ...and glyphs rising, dropping or bouncing into place move a step
        // at a time, so a typewriter's cells a few milliseconds apart are at
        // the same height.
        let moving: [fn(f32) -> Option<CellLook>; 3] = [
            |age| output_look(NewText::Rise, age, 0),
            |age| output_look(NewText::Drop, age, 0),
            |age| typed_look(TypedText::Bounce, age),
        ];
        for look in moving {
            let offsets: Vec<f32> =
                (0..40).filter_map(|i| look(0.05 + i as f32 * 0.0025)).map(|l| l.offset_y).collect();
            assert!(offsets.iter().all(|o| (o * 16.0).fract() == 0.0), "{offsets:?}");
            let heights: HashSet<u32> = offsets.iter().map(|o| o.to_bits()).collect();
            assert!(heights.len() <= 8, "{offsets:?}");
        }
    }

    #[test]
    fn whatever_the_device_sends_it_keeps_its_bounds_and_comes_to_rest() {
        // Text, prompts, cursor moves, clears, scroll regions, lines inserted
        // and deleted, scrolling either way, the alternate screen, scrolling
        // back and keys, at random on grids of all shapes.
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut cells = Vec::new();
        for _ in 0..80 {
            let (columns, lines) = (rng.between(2, 16), rng.between(2, 12));
            let scrollback = [0, 1, 3, 1000][rng.between(0, 3)];
            let settings = Animations {
                reveal: Reveal::ALL[rng.between(0, Reveal::ALL.len() - 1)],
                scrolling: Scrolling::ALL[rng.between(0, Scrolling::ALL.len() - 1)],
                new_text: NewText::ALL[rng.between(0, NewText::ALL.len() - 1)],
                speed: [0.5, 1.0, 2.0][rng.between(0, 2)],
                ..on()
            };
            let cap = REVEAL_CAP / f64::from(settings.pace());
            let mut emu = Emulator::new(columns, lines, scrollback, Arc::new(|_| {}));
            let mut anim = Animator::default();
            let mut now = 0.0;
            for _ in 0..150 {
                for _ in 0..rng.between(0, 4) {
                    let (row, column) = (rng.between(1, lines + 1), rng.between(1, columns + 1));
                    let piece = match rng.between(0, 15) {
                        0 | 1 => format!("w{}", rng.between(0, 999)),
                        2 | 3 => "\r\n".to_string(),
                        4 => "Switch#".to_string(),
                        5 => format!("\x1b[{row};{column}H"),
                        6 => format!("\x1b[{}J", rng.between(0, 3)),
                        7 => format!("\x1b[{}K", rng.between(0, 2)),
                        8 => format!("\x1b[{};{}r", rng.between(1, lines), row),
                        9 => "\x1bM".to_string(),
                        10 => format!("\x1b[{}L", rng.between(1, 3)),
                        11 => format!("\x1b[{}M", rng.between(1, 3)),
                        12 => format!("\x1b[{}S", rng.between(1, 3)),
                        13 => format!("\x1b[?1049{}", if rng.unit() < 0.5 { 'h' } else { 'l' }),
                        14 => {
                            emu.scroll_by(rng.between(0, 6) as i32 - 3);
                            String::new()
                        }
                        _ => {
                            let printable = rng.between(0, 1);
                            let cursor = capture(&emu, &mut cells).cursor;
                            anim.keystroke(now, &settings, printable, printable == 0, cursor);
                            "k".repeat(printable)
                        }
                    };
                    emu.feed(piece.as_bytes());
                }
                now += 0.016;
                anim.update(now, &settings, &capture(&emu, &mut cells));
                assert!(anim.scroll_offset(now).abs() <= lines as f32);
                // Nothing is held back longer than the reveal allows.
                assert!(anim.cells.iter().all(|s| s.kind != Kind::Output || s.start <= now + cap + 1e-9));
            }
            // Left alone, drawn for as long as it asks, it all settles in time.
            let quiet = now;
            while anim.next_wake(now).is_some() {
                assert!(now < quiet + 3.0, "still busy");
                now += 0.016;
                anim.update(now, &settings, &capture(&emu, &mut cells));
            }
            assert_still(&anim, now);
            let cursor = capture(&emu, &mut cells).cursor;
            assert!(anim.cursor(now, cursor).cell().is_some());
        }
    }

    #[test]
    fn every_style_looks_different() {
        let typed: Vec<_> = TypedText::ALL.iter().map(|&s| typed_look(s, 0.05)).collect();
        let shown: Vec<_> = NewText::ALL.iter().map(|&s| output_look(s, 0.05, 7)).collect();
        for looks in [typed, shown] {
            assert_eq!(looks[0], None, "Off");
            for (i, a) in looks.iter().enumerate() {
                assert!(a.is_some() || i == 0);
                assert!(looks[i + 1..].iter().all(|b| a != b), "{looks:?}");
            }
        }
        let marks: Vec<_> =
            NewLines::ALL[1..].iter().map(|&s| LineMark::new(s, 0.3, 0, 10)).map(|m| (m.alpha, m.reach)).collect();
        assert!(marks.iter().enumerate().all(|(i, a)| marks[i + 1..].iter().all(|b| a != b)), "{marks:?}");
        // Anything the view can't merge into a run of text says so.
        assert!(typed_look(TypedText::Pop, 0.05).unwrap().moves());
        assert!(!output_look(NewText::Heat, 0.05, 7).unwrap().moves());
        assert!(output_look(NewText::Decode, 0.05, 7).unwrap().moves());
    }

    #[test]
    fn decode_scrambles_then_settles() {
        let look = |age: f32| output_look(NewText::Decode, age, 12345);
        assert_eq!(look(0.01).unwrap().glyph, look(0.02).unwrap().glyph, "one glyph per tick");
        let glyphs: HashSet<char> =
            (0..5).map(|i| look(i as f32 * DECODE_TICK + 0.001).unwrap().glyph.unwrap()).collect();
        assert!(glyphs.len() >= 3, "{glyphs:?}");
        assert!(glyphs.iter().all(|g| DECODE_GLYPHS.contains(g)));
        assert_eq!(look(0.3), None);
    }

    /// The blink without animations, as terminal_view has it.
    fn plain_blink_on(now: f64, epoch: f64) -> bool {
        ((now + 0.03 - epoch).max(0.0) / 0.53) as i64 % 2 == 0
    }

    fn plain_until_next_blink(now: f64, epoch: f64) -> f64 {
        0.53 - (now + 0.03 - epoch).max(0.0) % 0.53 + 0.03
    }

    #[test]
    fn classic_blink_is_the_blink_without_animations() {
        for i in 0..500 {
            let now = 3.0 + f64::from(i) * 0.0137;
            assert_eq!(blink_level(CursorBlink::Classic, now, 3.0) == 1.0, plain_blink_on(now, 3.0), "at {now}");
            assert_eq!(blink_wake(CursorBlink::Classic, now, 3.0), plain_until_next_blink(now, 3.0), "at {now}");
        }
        // One redraw per edge.
        for wake in [0.525, 0.53, 0.535] {
            assert_eq!(blink_level(CursorBlink::Classic, wake, 0.0), 0.0, "at {wake}");
            assert!((blink_wake(CursorBlink::Classic, wake, 0.0) - BLINK).abs() < 0.011, "at {wake}");
        }
    }

    #[test]
    fn fade_blink_rests_between_fades() {
        let level = |now: f64| blink_level(CursorBlink::Fade, now, 0.0);
        assert_eq!(level(0.1), 1.0);
        assert_eq!(level(0.7), 0.0);
        // Each wake-up lands where the next fade starts, with nothing to draw before it.
        for (now, fade_starts) in [(0.1, 0.38), (0.7, 0.91), (1.1, 1.44)] {
            let wake = blink_wake(CursorBlink::Fade, now, 0.0);
            assert!((now + wake - fade_starts).abs() < 1e-9, "from {now}: {wake}");
            assert_eq!(level(now + wake - 0.001), level(now));
            assert_ne!(level(now + wake + 0.05), level(now));
        }
        // Mid-fade it's drawn at about 30 fps.
        assert_eq!(blink_wake(CursorBlink::Fade, 0.45, 0.0), BLINK_FRAME);
        assert!((level(0.455) - 0.5).abs() < 0.01);
        // Typing restarts it, fully on.
        assert_eq!(blink_level(CursorBlink::Fade, 5.0, 5.0), 1.0);
    }

    #[test]
    fn pulse_and_glow_never_hide_the_cursor() {
        let (mut low, mut high, mut halo_low, mut halo_high) = (1.0_f32, 0.0_f32, 1.0_f32, 0.0_f32);
        for i in 0..400 {
            let now = f64::from(i) * 0.01;
            let pulse = blink_level(CursorBlink::Pulse, now, 0.0);
            let halo = blink_halo(CursorBlink::Glow, now, 0.0);
            assert!((0.349..=1.0).contains(&pulse), "at {now}: {pulse}");
            assert!((0.0..=1.0).contains(&halo), "at {now}: {halo}");
            assert_eq!(blink_level(CursorBlink::Glow, now, 0.0), 1.0);
            assert_eq!(blink_halo(CursorBlink::Pulse, now, 0.0), 0.0);
            (low, high) = (low.min(pulse), high.max(pulse));
            (halo_low, halo_high) = (halo_low.min(halo), halo_high.max(halo));
        }
        assert!(low < 0.36 && high > 0.99 && halo_low < 0.01 && halo_high > 0.99);
        assert_eq!(blink_wake(CursorBlink::Pulse, 1.0, 0.0), BLINK_FRAME);
        assert_eq!(blink_wake(CursorBlink::Glow, 1.0, 0.0), BLINK_FRAME);
    }
}
