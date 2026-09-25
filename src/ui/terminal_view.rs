//! The terminal widget: draws a session's screen and turns keyboard and
//! mouse input into bytes, selections and scrolling.

use std::collections::HashMap;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::term::cell::Flags;
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, CornerRadius, CursorIcon, Event, EventFilter, FontId, Id, Key, MouseWheelUnit, Pos2, Rect, Sense, Stroke,
    Ui, Vec2, pos2, vec2,
};

use crate::session::Session;
use crate::settings::Theme;
use crate::terminal::colors;
use crate::terminal::highlight::{self, Span};
use crate::terminal::keys;

pub const SCROLLBAR_WIDTH: f32 = 12.0;
const PADDING: f32 = 4.0;

/// Distinct lines to keep syntax-highlighting results for: many screens'
/// worth, but bounded so a long session can't grow it forever.
const HIGHLIGHT_CACHE_MAX: usize = 1000;

/// Cursor blink half-period, in seconds.
const BLINK: f64 = 0.53;

/// How often a drag past the edge scrolls, and the most it moves per tick.
const AUTOSCROLL_INTERVAL: f64 = 0.04;
const AUTOSCROLL_MAX_LINES: i32 = 12;

/// Lines the wheel scrolls per notch, the way every other app does.
const WHEEL_LINES: f32 = 3.0;

/// How a view should draw; comes from the session's profile and the app
/// settings.
pub struct ViewOptions<'a> {
    pub theme: Theme,
    pub syntax: &'a str,
    pub regular: FontId,
    pub bold: FontId,
    /// False while a dialog is up: its Enter or Escape must not also reach
    /// the device through a terminal that still has keyboard focus.
    pub keyboard: bool,
}

/// Timer wake-ups land a few milliseconds either side of the blink edge.
/// Treating anything that close as already past it means each blink costs
/// one redraw, not a redraw just before the edge and another just after.
const BLINK_SLACK: f64 = 0.03;

fn blink_on(now: f64, epoch: f64) -> bool {
    ((now + BLINK_SLACK - epoch).max(0.0) / BLINK) as i64 % 2 == 0
}

fn until_next_blink(now: f64, epoch: f64) -> f64 {
    BLINK - (now + BLINK_SLACK - epoch).max(0.0) % BLINK + BLINK_SLACK
}

/// What a frame of input asks the app to do.
#[derive(Default)]
pub struct ViewOutput {
    /// Text to put on the clipboard.
    pub copy: Option<String>,
    /// The user asked to paste (right-click).
    pub paste_requested: bool,
}

#[derive(Default)]
pub struct TerminalView {
    selecting: bool,
    last_autoscroll: f64,
    wheel_remainder: f32,
    /// When the cursor last restarted its blink (keypress or focus).
    blink_epoch: f64,
    highlight_cache: HashMap<String, Vec<Span>>,
    scrollbar_grab: Option<f32>,
    request_focus: bool,
}

struct Metrics {
    origin: Pos2,
    cell: Vec2,
    columns: usize,
    lines: usize,
}

impl Metrics {
    fn cell_rect(&self, row: usize, column: usize, width: usize) -> Rect {
        let x0 = (self.origin.x + column as f32 * self.cell.x).round();
        let x1 = (self.origin.x + (column + width) as f32 * self.cell.x).round();
        let y0 = self.origin.y + row as f32 * self.cell.y;
        Rect::from_min_max(pos2(x0, y0), pos2(x1, y0 + self.cell.y))
    }

    /// Screen cell under a point, clamped to the grid, plus which half of
    /// the cell it's in.
    fn cell_at(&self, pos: Pos2) -> (usize, usize, Side) {
        let x = (pos.x - self.origin.x) / self.cell.x;
        let y = (pos.y - self.origin.y) / self.cell.y;
        let column = (x.floor().max(0.0) as usize).min(self.columns - 1);
        let row = (y.floor().max(0.0) as usize).min(self.lines - 1);
        let side = if x < 0.0 {
            Side::Left
        } else if x.fract() < 0.5 || x >= self.columns as f32 {
            if x >= self.columns as f32 { Side::Right } else { Side::Left }
        } else {
            Side::Right
        };
        (row, column, side)
    }
}

/// One stretch of cells drawn with the same style.
struct Run {
    column: usize,
    width: usize,
    text: String,
    fg: Color32,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

impl TerminalView {
    pub fn focus(&mut self) {
        self.request_focus = true;
    }

    pub fn clear_highlight_cache(&mut self) {
        self.highlight_cache.clear();
    }

    fn highlight(&mut self, text: &str, syntax: &str) -> Vec<Span> {
        if let Some(spans) = self.highlight_cache.get(text) {
            return spans.clone();
        }
        if self.highlight_cache.len() >= HIGHLIGHT_CACHE_MAX {
            self.highlight_cache.clear(); // cheaper than tracking an LRU for this
        }
        let spans = highlight::highlight_line(text, syntax);
        self.highlight_cache.insert(text.to_string(), spans.clone());
        spans
    }

    /// Lay out, draw and handle input for one frame.
    pub fn show(&mut self, ui: &mut Ui, session: &Session, options: &ViewOptions<'_>) -> ViewOutput {
        let mut out = ViewOutput::default();
        let id = Id::new(("terminal", session.id));
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());
        // A little breathing room between the text and the window edge.
        let term_rect =
            Rect::from_min_max(full.min + vec2(PADDING, PADDING), pos2(full.max.x - SCROLLBAR_WIDTH, full.max.y));
        let bar_rect = Rect::from_min_max(pos2(term_rect.max.x, full.min.y), full.max);

        let (cell_w, cell_h) = ui.fonts_mut(|f| (f.glyph_width(&options.regular, 'M'), f.row_height(&options.regular)));
        let cell = vec2(cell_w.max(1.0), cell_h.ceil().max(1.0));
        let columns = ((term_rect.width() / cell.x).floor() as usize).max(2);
        let lines = ((term_rect.height() / cell.y).floor() as usize).max(2);
        let metrics = Metrics { origin: term_rect.min.round(), cell, columns, lines };

        // Follow the widget's size.
        let resized = {
            let mut emu = session.shared.emulator.lock();
            emu.set_theme(options.theme);
            emu.set_cell_size(cell.x, cell.y);
            emu.resize(columns, lines)
        };
        if resized {
            session.resize(columns, lines);
            self.highlight_cache.clear();
        }

        let response = ui.interact(term_rect, id, Sense::click_and_drag());
        if self.request_focus {
            self.request_focus = false;
            response.request_focus();
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }
        let focused = response.has_focus() && options.keyboard;
        let now = ui.input(|i| i.time);
        if response.gained_focus() {
            self.blink_epoch = now;
        }

        self.handle_mouse(ui, &response, session, &metrics, now, &mut out);
        if focused {
            // Keep Tab, arrows and Escape for the far end instead of letting
            // egui move focus with them.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                )
            });
            self.handle_keys(ui, session, now, &mut out);
        }

        // The cursor blinks only while this terminal has the keyboard and
        // the window is in front; otherwise it's drawn hollow and nothing
        // needs redrawing until something happens.
        let window_focused = ui.input(|i| i.viewport().focused.unwrap_or(true));
        let blinking = focused && window_focused;
        let cursor_on = !blinking || blink_on(now, self.blink_epoch);

        let area = Rect::from_min_max(full.min, pos2(term_rect.max.x, full.max.y));
        self.paint(ui, area, session, options, &metrics, blinking, cursor_on);
        self.scrollbar(ui, id, bar_rect, session, lines);

        if blinking {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(until_next_blink(now, self.blink_epoch)));
        }
        out
    }

    fn typed(&mut self, session: &Session, bytes: Vec<u8>, now: f64) {
        if bytes.is_empty() {
            return;
        }
        session.shared.emulator.lock().scroll_to_bottom();
        session.write(bytes);
        self.blink_epoch = now;
    }

    fn handle_keys(&mut self, ui: &mut Ui, session: &Session, now: f64, out: &mut ViewOutput) {
        let (events, mods_now) = ui.input(|i| (i.events.clone(), i.modifiers));
        let app_cursor = session.shared.emulator.lock().application_cursor_keys();
        for event in events {
            match event {
                Event::Text(text) => self.typed(session, keys::encode_text(&text, mods_now), now),
                Event::Key { key, pressed: true, modifiers, .. } => {
                    // Shift+PgUp/PgDn scroll locally instead of going out.
                    if modifiers.shift && !modifiers.ctrl && matches!(key, Key::PageUp | Key::PageDown) {
                        let mut emu = session.shared.emulator.lock();
                        if key == Key::PageUp {
                            emu.page_up()
                        } else {
                            emu.page_down()
                        };
                        continue;
                    }
                    if let Some(bytes) = keys::encode_key(key, modifiers, app_cursor) {
                        self.typed(session, bytes, now);
                    }
                }
                // egui turns Ctrl+C/X/V into clipboard events before they are
                // keys. Only the Shift variants are clipboard commands here;
                // the plain ones are control characters the far end needs
                // (Ctrl+C interrupts a ping, Ctrl+V quotes the next key).
                Event::Copy => {
                    if mods_now.shift {
                        out.copy = session.shared.emulator.lock().selection_text();
                    } else {
                        self.typed(session, vec![0x03], now);
                    }
                }
                Event::Cut => {
                    // Shift+Delete on Windows also arrives as Cut.
                    let bytes = if mods_now.ctrl { vec![0x18] } else { b"\x1b[3;2~".to_vec() };
                    self.typed(session, bytes, now);
                }
                Event::Paste(text) => {
                    if mods_now.ctrl && !mods_now.shift {
                        self.typed(session, vec![0x16], now);
                    } else {
                        self.typed(session, keys::encode_paste(&text), now);
                    }
                }
                _ => {}
            }
        }
    }

    fn handle_mouse(
        &mut self,
        ui: &mut Ui,
        response: &egui::Response,
        session: &Session,
        m: &Metrics,
        now: f64,
        out: &mut ViewOutput,
    ) {
        if response.clicked() || response.drag_started() || response.secondary_clicked() {
            response.request_focus();
        }

        // Wheel scrolls the history a few lines per notch. Sub-line amounts
        // from trackpads are carried over rather than rounded away.
        if response.hovered() {
            let dy: f32 = ui.input(|i| {
                i.events
                    .iter()
                    .map(|e| match e {
                        Event::MouseWheel { unit, delta, .. } => match unit {
                            MouseWheelUnit::Line => delta.y * WHEEL_LINES,
                            MouseWheelUnit::Point => delta.y / m.cell.y,
                            MouseWheelUnit::Page => delta.y * m.lines as f32,
                        },
                        _ => 0.0,
                    })
                    .sum()
            });
            if dy != 0.0 {
                self.wheel_remainder += dy;
                let lines = self.wheel_remainder.trunc() as i32;
                self.wheel_remainder -= lines as f32;
                if lines != 0 {
                    session.shared.emulator.lock().scroll_by(lines);
                }
            }
        }

        if response.secondary_clicked() {
            // PuTTY habit: right-click pastes.
            out.paste_requested = true;
            return;
        }

        let pointer = ui.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()));
        if response.double_clicked()
            && let Some(pos) = pointer
        {
            // Double-click selects the whole line, and copies it.
            let (row, column, _) = m.cell_at(pos);
            let mut emu = session.shared.emulator.lock();
            let point = emu.viewport_to_point(row, column);
            emu.start_selection(point, Side::Left, true);
            out.copy = emu.selection_text();
            self.selecting = false;
            return;
        }

        if response.drag_started_by(egui::PointerButton::Primary) {
            let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
            if let Some(origin) = origin {
                let (row, column, side) = m.cell_at(origin);
                let mut emu = session.shared.emulator.lock();
                let point = emu.viewport_to_point(row, column);
                emu.start_selection(point, side, false);
                self.selecting = true;
            }
        }

        if self.selecting
            && let Some(pos) = pointer
        {
            let mut emu = session.shared.emulator.lock();
            // Dragging past the top or bottom edge scrolls, faster the
            // further out the pointer goes.
            let rect_top = m.origin.y;
            let rect_bottom = m.origin.y + m.lines as f32 * m.cell.y;
            let distance = if pos.y < rect_top {
                rect_top - pos.y
            } else if pos.y > rect_bottom {
                pos.y - rect_bottom
            } else {
                0.0
            };
            if distance > 0.0 {
                if now - self.last_autoscroll >= AUTOSCROLL_INTERVAL {
                    self.last_autoscroll = now;
                    let step = (1 + (distance / m.cell.y) as i32).min(AUTOSCROLL_MAX_LINES);
                    emu.scroll_by(if pos.y < rect_top { step } else { -step });
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(AUTOSCROLL_INTERVAL));
            }
            let (row, column, side) = m.cell_at(pos);
            let point = emu.viewport_to_point(row, column);
            emu.update_selection(point, side);
        }

        if response.drag_stopped() && self.selecting {
            self.selecting = false;
            let mut emu = session.shared.emulator.lock();
            if emu.has_selection() {
                // PuTTY habit: selecting copies.
                out.copy = emu.selection_text();
            } else {
                emu.clear_selection();
            }
        } else if response.clicked() {
            session.shared.emulator.lock().clear_selection();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        session: &Session,
        options: &ViewOptions<'_>,
        m: &Metrics,
        focused: bool,
        cursor_on: bool,
    ) {
        let painter = ui.painter().clone();
        let theme = options.theme;
        painter.rect_filled(area, 0.0, theme.bg);

        let emu = session.shared.emulator.lock();
        let term = emu.term();
        let grid = term.grid();
        let offset = grid.display_offset();
        let overrides = term.colors();
        let selection = term.selection.as_ref().and_then(|s| s.to_range(term));
        let highlighting = options.syntax != "none";
        let columns = m.columns.min(grid.columns());
        let lines = m.lines.min(grid.screen_lines());

        let mut jobs: Vec<(Pos2, LayoutJob)> = Vec::new();
        for row in 0..lines {
            let line = Line(row as i32 - offset as i32);
            let cells = &grid[line];

            let spans = if highlighting {
                let text: String = (0..columns).map(|c| cells[Column(c)].c).collect();
                self.highlight(&text, options.syntax)
            } else {
                Vec::new()
            };
            let syntax_color =
                |column: usize| spans.iter().find(|s| s.start <= column && column < s.end).map(|s| s.category.color());

            let mut runs: Vec<Run> = Vec::new();
            let mut bg_run: Option<(usize, usize, Color32)> = None;
            let flush_bg = |run: &mut Option<(usize, usize, Color32)>| {
                if let Some((start, width, color)) = run.take()
                    && color != theme.bg
                {
                    painter.rect_filled(m.cell_rect(row, start, width), 0.0, color);
                }
            };

            for column in 0..columns {
                let cell = &cells[Column(column)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
                let (mut fg, mut bg) = colors::cell_colors(cell, &theme, overrides);
                let selected = selection.is_some_and(|s| s.contains(Point::new(line, Column(column))));
                if selected {
                    bg = theme.selection;
                } else if let Some(color) = syntax_color(column) {
                    fg = color;
                }

                match &mut bg_run {
                    Some((start, w, color)) if *color == bg && *start + *w == column => *w += width,
                    _ => {
                        flush_bg(&mut bg_run);
                        bg_run = Some((column, width, bg));
                    }
                }

                let ch = if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c };
                let bold = cell.flags.contains(Flags::BOLD);
                let italic = cell.flags.contains(Flags::ITALIC);
                let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
                let strike = cell.flags.contains(Flags::STRIKEOUT);
                // Non-ASCII glyphs may come from a fallback font with a
                // different advance, so each gets its own run pinned to its
                // column; plain ASCII runs stay monospace-aligned.
                let simple = ch.is_ascii() && width == 1;
                let extend = simple
                    && runs.last().is_some_and(|r| {
                        r.column + r.width == column
                            && r.fg == fg
                            && r.bold == bold
                            && r.italic == italic
                            && r.underline == underline
                            && r.strike == strike
                            && r.text.is_ascii()
                    });
                if extend {
                    let run = runs.last_mut().unwrap();
                    run.text.push(ch);
                    run.width += 1;
                } else {
                    let mut text = String::new();
                    text.push(ch);
                    if let Some(extra) = cell.zerowidth() {
                        text.extend(extra.iter());
                    }
                    runs.push(Run { column, width, text, fg, bold, italic, underline, strike });
                }
            }
            flush_bg(&mut bg_run);

            for run in runs {
                if run.text.trim().is_empty() && !run.underline && !run.strike {
                    continue;
                }
                let font = if run.bold { options.bold.clone() } else { options.regular.clone() };
                let mut format = TextFormat::simple(font, run.fg);
                format.italics = run.italic;
                if run.underline {
                    format.underline = Stroke::new(1.0, run.fg);
                }
                if run.strike {
                    format.strikethrough = Stroke::new(1.0, run.fg);
                }
                let pos = m.cell_rect(row, run.column, run.width).min;
                jobs.push((pos, LayoutJob::single_section(run.text, format)));
            }
        }

        // Cursor, only on the live screen.
        let cursor = (offset == 0 && emu.cursor_visible()).then(|| {
            let mut point = emu.cursor();
            if point.column.0 > 0 && grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) {
                point.column -= 1;
            }
            let cell = &grid[point];
            let wide = cell.flags.contains(Flags::WIDE_CHAR);
            (point, cell.c, cell.flags.contains(Flags::BOLD), wide)
        });
        drop(emu);

        for (pos, job) in jobs {
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(pos, galley, theme.fg);
        }

        if let Some((point, ch, bold, wide)) = cursor {
            let row = point.line.0.max(0) as usize;
            let column = point.column.0;
            if row < m.lines && column < m.columns {
                let rect = m.cell_rect(row, column, if wide { 2 } else { 1 });
                if !focused {
                    painter.rect_stroke(
                        rect.shrink(0.5),
                        CornerRadius::ZERO,
                        Stroke::new(1.0, theme.cursor),
                        egui::StrokeKind::Inside,
                    );
                } else if cursor_on {
                    painter.rect_filled(rect, 0.0, theme.cursor);
                    if !ch.is_whitespace() {
                        let font = if bold { options.bold.clone() } else { options.regular.clone() };
                        painter.text(rect.min, egui::Align2::LEFT_TOP, ch, font, theme.bg);
                    }
                }
            }
        }
    }

    fn scrollbar(&mut self, ui: &mut Ui, id: Id, rect: Rect, session: &Session, lines: usize) {
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, crate::ui::style::BG_WINDOW);

        let (history, offset) = {
            let emu = session.shared.emulator.lock();
            (emu.history_size(), emu.display_offset())
        };
        let response = ui.interact(rect, id.with("scrollbar"), Sense::click_and_drag());
        if history == 0 {
            return;
        }
        let total = (history + lines) as f32;
        let track = rect.shrink2(vec2(2.0, 2.0));
        let thumb_h = (track.height() * lines as f32 / total).clamp(24.0_f32.min(track.height()), track.height());
        let travel = (track.height() - thumb_h).max(1.0);
        let fraction = 1.0 - offset as f32 / history as f32;
        let thumb =
            Rect::from_min_size(pos2(track.min.x, track.min.y + travel * fraction), vec2(track.width(), thumb_h));

        let pointer = ui.input(|i| i.pointer.interact_pos());
        if response.drag_started()
            && let Some(pos) = pointer
        {
            // Grabbing the thumb keeps the grab point under the pointer;
            // grabbing the track jumps the thumb's middle there.
            self.scrollbar_grab = Some(if thumb.contains(pos) { pos.y - thumb.min.y } else { thumb_h / 2.0 });
        }
        if let (Some(grab), Some(pos), true) = (self.scrollbar_grab, pointer, response.dragged()) {
            let fraction = ((pos.y - grab - track.min.y) / travel).clamp(0.0, 1.0);
            let target = ((1.0 - fraction) * history as f32).round() as usize;
            session.shared.emulator.lock().scroll_to(target);
        }
        if response.drag_stopped() {
            self.scrollbar_grab = None;
        }
        if response.clicked()
            && let Some(pos) = pointer
            && !thumb.contains(pos)
        {
            let mut emu = session.shared.emulator.lock();
            if pos.y < thumb.min.y {
                emu.page_up()
            } else {
                emu.page_down()
            };
        }

        let color = if response.dragged() || response.hovered() {
            crate::ui::style::lighter(crate::ui::style::BORDER, 2.2)
        } else {
            crate::ui::style::lighter(crate::ui::style::BORDER, 1.5)
        };
        ui.painter().rect_filled(thumb, 4.0, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics { origin: pos2(10.0, 20.0), cell: vec2(8.0, 16.0), columns: 80, lines: 24 }
    }

    #[test]
    fn one_redraw_per_blink() {
        // Waking a little early or late still lands on the next state and
        // schedules the one after it, a full period away.
        for wake in [0.525, 0.53, 0.535] {
            assert!(!blink_on(wake, 0.0), "at {wake}");
            let next = until_next_blink(wake, 0.0);
            assert!((next - BLINK).abs() < 0.011, "at {wake}: {next}");
        }
        assert!(blink_on(0.0, 0.0));
        assert!(blink_on(1.06, 0.0));
    }

    #[test]
    fn cell_hit_testing() {
        let m = metrics();
        assert_eq!(m.cell_at(pos2(10.0, 20.0)), (0, 0, Side::Left));
        assert_eq!(m.cell_at(pos2(10.0 + 8.0 * 3.0 + 6.0, 20.0 + 16.0 * 2.0 + 1.0)), (2, 3, Side::Right));
        // Past the edges clamps to the grid.
        assert_eq!(m.cell_at(pos2(-50.0, -50.0)), (0, 0, Side::Left));
        assert_eq!(m.cell_at(pos2(5000.0, 5000.0)), (23, 79, Side::Right));
    }

    #[test]
    fn cell_rects_tile_without_gaps() {
        let m = Metrics { cell: vec2(8.4, 17.0), ..metrics() };
        let a = m.cell_rect(0, 0, 3);
        let b = m.cell_rect(0, 3, 2);
        assert_eq!(a.max.x, b.min.x);
        assert_eq!(a.min.x, a.min.x.round());
    }
}
