//! The terminal widget: draws a session's screen and turns keyboard and
//! mouse input into bytes, selections and scrolling. With animations on it
//! also draws what the animator (see [`super::animation`]) says is moving.

use std::collections::HashMap;
use std::f32::consts::FRAC_PI_2;
use std::ops::Range;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::term::cell::Flags;
use egui::emath::{Rot2, TSTransform};
use egui::epaint::{RectShape, TextShape};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, CornerRadius, CursorIcon, Event, EventFilter, FontId, Id, Key, MouseWheelUnit, Painter, Popup, Pos2, Rect,
    Sense, SetOpenCommand, Shape, Stroke, Ui, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};
use parking_lot::Mutex;

use super::animation::{
    self, Animator, CellLook, CursorLook, LineMark, ParticleColour, ParticleLook, ParticleShape, Snapshot,
};
use crate::session::Session;
use crate::settings::{Animations, NewLines, Theme};
use crate::terminal::colors;
use crate::terminal::emulator::Emulator;
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

/// Colour mixes in a fade are rounded to this many steps, so cells a moment
/// apart in a typewriter reveal still share a run of text rather than each
/// being laid out on its own. Too fine a difference to see.
const MIX_STEPS: f32 = 32.0;

/// Glyphs that change size are laid out and transformed one at a time, so
/// when more than this many do at once (a screenful of output zooming in)
/// they only fade, which lets them share runs of text. Thousands of single
/// glyph shapes cost more than a frame.
const MAX_SCALED: usize = 1500;

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
    /// With `enabled` off, the view draws and redraws exactly as it did
    /// before there were animations.
    pub animations: Animations,
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
    /// The user asked to paste (right-click or the context menu).
    pub paste_requested: bool,
    /// A snapshot of output to save or append with the app's file dialog.
    pub text_export: Option<TextExport>,
}

pub enum TextExport {
    Save(String),
    Append(String),
}

fn ctrl_secondary_click(ui: &Ui) -> bool {
    ui.input(|i| {
        // Ctrl may have been released in the same frame as the click.
        // Use the modifier state attached to the mouse release itself.
        i.events
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::PointerButton { button: egui::PointerButton::Secondary, pressed: false, modifiers, .. } => {
                    Some(modifiers.ctrl)
                }
                _ => None,
            })
            .unwrap_or(i.modifiers.ctrl)
    })
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
    animator: Animator,
    /// Whether animations were on when this view was last drawn, and the
    /// egui pass that was.
    animating: bool,
    last_pass: u64,
    /// The live screen as handed to the animator; kept to save allocating
    /// it every frame.
    screen: Vec<char>,
}

#[derive(Clone, Copy)]
struct Metrics {
    origin: Pos2,
    cell: Vec2,
    columns: usize,
    lines: usize,
}

impl Metrics {
    /// The grid that fits in `rect` in `font`.
    fn fit(ui: &Ui, rect: Rect, font: &FontId) -> Metrics {
        let (cell_w, cell_h) = ui.fonts_mut(|f| (f.glyph_width(font, 'M'), f.row_height(font)));
        let cell = vec2(cell_w.max(1.0), cell_h.ceil().max(1.0));
        let columns = ((rect.width() / cell.x).floor() as usize).max(2);
        let lines = ((rect.height() / cell.y).floor() as usize).max(2);
        Metrics { origin: rect.min.round(), cell, columns, lines }
    }

    fn cell_rect(&self, row: usize, column: usize, width: usize) -> Rect {
        let x0 = (self.origin.x + column as f32 * self.cell.x).round();
        let x1 = (self.origin.x + (column + width) as f32 * self.cell.x).round();
        let y0 = self.origin.y + row as f32 * self.cell.y;
        Rect::from_min_max(pos2(x0, y0), pos2(x1, y0 + self.cell.y))
    }

    /// [`Metrics::cell_rect`] for something between cells (moving) or off
    /// the grid (sliding in past its edge).
    fn rect_at(&self, row: f32, column: f32, width: f32) -> Rect {
        let x0 = (self.origin.x + column * self.cell.x).round();
        let x1 = (self.origin.x + (column + width) * self.cell.x).round();
        let y0 = self.origin.y + row * self.cell.y;
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
    /// Animations only: how far the text is moved down, in cells, and how
    /// much it's scaled about its centre.
    offset_y: f32,
    scale: f32,
}

/// The cursor as the animations draw it.
struct AnimatedCursor {
    look: CursorLook,
    /// Cells wide.
    width: f32,
    /// The character under it once it's come to rest on one, and whether
    /// it's bold.
    glyph: Option<(char, bool)>,
}

impl TerminalView {
    pub fn focus(&mut self) {
        self.request_focus = true;
    }

    pub fn clear_highlight_cache(&mut self) {
        self.highlight_cache.clear();
    }

    /// Take the screen as it is from here: nothing on it animates. For a
    /// clear or a reset.
    pub fn reset_animations(&mut self) {
        self.animator.reset();
    }

    /// How far the scroll animation has the text from where it rests, in
    /// rows; for checking what the app does to a view.
    #[cfg(test)]
    pub(super) fn scroll_offset(&self, now: f64) -> f32 {
        self.animator.scroll_offset(now)
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

        // Follow the widget's size.
        let (metrics, resized) = self.fit(ui, term_rect, &session.shared.emulator, options);
        if resized {
            session.resize(metrics.columns, metrics.lines);
        }

        let response = ui.interact(term_rect, id, Sense::click_and_drag());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, "Terminal output"));
        if self.request_focus {
            self.request_focus = false;
            response.request_focus();
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }
        let now = ui.input(|i| i.time);
        if response.gained_focus() {
            self.blink_epoch = now;
        }

        self.handle_mouse(ui, &response, session, &metrics, now, &mut out);
        let menu_was_open = Popup::is_id_open(ui.ctx(), Popup::default_response_id(&response));
        let menu_shown = self.context_menu(ui, &response, session, options.keyboard, &mut out);
        // Menu navigation and dismissal must never go to the device, even
        // on the frame that closes the popup and restores terminal focus.
        let focused = response.has_focus() && options.keyboard && !menu_shown && !menu_was_open;
        if focused {
            // Keep Tab, arrows and Escape for the far end instead of letting
            // egui move focus with them.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                )
            });
            self.handle_keys(ui, session, now, &options.animations, &mut out);
        }

        // The cursor blinks only while this terminal has the keyboard and
        // the window is in front; otherwise it's drawn hollow and nothing
        // needs redrawing until something happens.
        let window_focused = ui.input(|i| i.viewport().focused.unwrap_or(true));
        let blinking = focused && window_focused;
        let cursor_on = !blinking || blink_on(now, self.blink_epoch);

        let area = Rect::from_min_max(full.min, pos2(term_rect.max.x, full.max.y));
        self.paint(ui, area, &session.shared.emulator, options, &metrics, blinking, cursor_on, now);
        self.scrollbar(ui, id, bar_rect, session, metrics.lines);

        self.schedule_repaint(ui.ctx(), &options.animations, blinking, now);
        out
    }

    fn context_menu(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        session: &Session,
        keyboard: bool,
        out: &mut ViewOutput,
    ) -> bool {
        let open = keyboard && response.secondary_clicked() && ctrl_secondary_click(ui);
        let state = if open {
            response.surrender_focus();
            Some(SetOpenCommand::Bool(true))
        } else if response.clicked() || !keyboard {
            Some(SetOpenCommand::Bool(false))
        } else {
            None
        };
        let mut acted = false;
        let shown = Popup::context_menu(response).open_memory(state).show(|ui| {
            let selected = session.shared.emulator.lock().has_selection();
            let copy = ui.add_enabled(selected, egui::Button::new("Copy"));
            if open && selected {
                copy.request_focus();
            }
            if copy.clicked() {
                out.copy = session.shared.emulator.lock().selection_text();
                acted = true;
            }
            let copy_all = ui.button("Copy all");
            if open && !selected {
                copy_all.request_focus();
            }
            if copy_all.clicked() {
                out.copy = Some(session.shared.emulator.lock().all_text());
                acted = true;
            }
            if ui.add_enabled(session.is_live(), egui::Button::new("Paste")).clicked() {
                out.paste_requested = true;
                acted = true;
            }
            if ui.button("Select all").clicked() {
                session.shared.emulator.lock().select_all();
                acted = true;
            }
            if ui.add_enabled(selected, egui::Button::new("Clear selection")).clicked() {
                session.shared.emulator.lock().clear_selection();
                acted = true;
            }
            ui.separator();
            if ui.button("Save output to text file…").clicked() {
                out.text_export = Some(TextExport::Save(session.shared.emulator.lock().all_text()));
                acted = true;
            }
            if ui.add_enabled(selected, egui::Button::new("Save selection to text file…")).clicked() {
                out.text_export = session.shared.emulator.lock().selection_text().map(TextExport::Save);
                acted = true;
            }
            if ui
                .button("Append to text file…")
                .on_hover_text(
                    "Append the selection, or all retained output if nothing is selected, to an existing file.",
                )
                .clicked()
            {
                let emu = session.shared.emulator.lock();
                let text = if selected { emu.selection_text().unwrap_or_default() } else { emu.all_text() };
                out.text_export = Some(TextExport::Append(text));
                acted = true;
            }
            ui.separator();
            let scrolled_back = session.shared.emulator.lock().display_offset() != 0;
            if ui.add_enabled(scrolled_back, egui::Button::new("Scroll to bottom")).clicked() {
                session.shared.emulator.lock().scroll_to_bottom();
                acted = true;
            }
            if acted {
                ui.close();
            }
        });
        if shown.is_some() && (acted || ui.input(|i| i.key_pressed(Key::Escape))) {
            self.focus();
        }
        shown.is_some()
    }

    /// Draw `emulator` in a `size` box with no scrollbar and no input, as if
    /// it had the keyboard so the cursor blinks: the live preview in
    /// Preferences. Feed it keys with [`TerminalView::demo_keystroke`].
    pub fn show_demo(&mut self, ui: &mut Ui, size: Vec2, emulator: &Mutex<Emulator>, options: &ViewOptions<'_>) {
        let (area, _) = ui.allocate_exact_size(size, Sense::hover());
        let term_rect = Rect::from_min_max(area.min + vec2(PADDING, PADDING), pos2(area.max.x - PADDING, area.max.y));
        let (metrics, _) = self.fit(ui, term_rect, emulator, options);
        let now = ui.input(|i| i.time);
        let cursor_on = blink_on(now, self.blink_epoch);
        self.paint(ui, area, emulator, options, &metrics, true, cursor_on, now);
        self.schedule_repaint(ui.ctx(), &options.animations, true, now);
    }

    /// A key pressed in the demo, whose echo will bring `printable`
    /// characters (0 for Enter); `newline` for Enter. Call it before feeding
    /// the echo to `emulator`, as a real keystroke goes out before its echo
    /// comes back.
    pub fn demo_keystroke(
        &mut self,
        emulator: &Mutex<Emulator>,
        printable: usize,
        newline: bool,
        now: f64,
        animations: &Animations,
    ) {
        self.blink_epoch = now;
        if animations.enabled {
            let cursor = live_cursor(&emulator.lock());
            self.animator.keystroke(now, animations, printable, newline, cursor);
        }
    }

    /// Size the grid to `rect` and the emulator to the grid, and see whether
    /// animations start afresh this frame. Returns the grid and whether it
    /// changed size.
    fn fit(&mut self, ui: &Ui, rect: Rect, emulator: &Mutex<Emulator>, options: &ViewOptions<'_>) -> (Metrics, bool) {
        let metrics = Metrics::fit(ui, rect, &options.regular);
        let resized = {
            let mut emu = emulator.lock();
            emu.set_theme(options.theme);
            emu.set_cell_size(metrics.cell.x, metrics.cell.y);
            emu.resize(metrics.columns, metrics.lines)
        };
        if resized {
            self.highlight_cache.clear();
        }
        self.start_frame(ui.ctx().cumulative_pass_nr(), &options.animations, resized);
        (metrics, resized)
    }

    /// Before any input each frame: the animations start afresh, animating
    /// nothing already on screen, when they've just been turned on, the
    /// grid changed size, or this view wasn't drawn on the last pass (its
    /// tab was in the background, or the preview was hidden).
    fn start_frame(&mut self, pass: u64, animations: &Animations, resized: bool) {
        if animations.enabled && (!self.animating || resized || pass != self.last_pass.wrapping_add(1)) {
            self.animator.reset();
        }
        self.animating = animations.enabled;
        self.last_pass = pass;
    }

    fn schedule_repaint(&self, ctx: &egui::Context, animations: &Animations, blinking: bool, now: f64) {
        if animations.enabled {
            // Every frame while something moves; otherwise only when the
            // blink next changes, if it's blinking at all.
            let blink = blinking.then(|| animation::blink_wake(animations.cursor_blink, now, self.blink_epoch));
            match next_frame(self.animator.next_wake(now), blink) {
                Some(wait) if wait <= 0.0 => ctx.request_repaint(),
                Some(wait) => ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait)),
                None => {}
            }
        } else if blinking {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(until_next_blink(now, self.blink_epoch)));
        }
    }

    /// `printable` is how many characters the key should echo: 0 for Enter,
    /// arrows, control keys or a paste.
    fn typed(&mut self, session: &Session, bytes: Vec<u8>, now: f64, printable: usize, animations: &Animations) {
        if bytes.is_empty() {
            return;
        }
        let cursor = {
            let mut emu = session.shared.emulator.lock();
            emu.scroll_to_bottom();
            // Where the key was typed, before a local echo moves the cursor on.
            animations.enabled.then(|| live_cursor(&emu))
        };
        let newline = bytes.iter().any(|&b| b == b'\r' || b == b'\n');
        session.write(bytes);
        self.blink_epoch = now;
        if let Some(cursor) = cursor {
            self.animator.keystroke(now, animations, printable, newline, cursor);
        }
    }

    fn handle_keys(&mut self, ui: &mut Ui, session: &Session, now: f64, animations: &Animations, out: &mut ViewOutput) {
        let (events, mods_now) = ui.input(|i| (i.events.clone(), i.modifiers));
        let app_cursor = session.shared.emulator.lock().application_cursor_keys();
        let backspace = session.profile.backspace();
        for event in events {
            match event {
                Event::Text(text) => {
                    let printable = text.chars().count();
                    self.typed(session, keys::encode_text(&text, mods_now), now, printable, animations);
                }
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
                    if let Some(bytes) = keys::encode_key(key, modifiers, app_cursor, backspace) {
                        self.typed(session, bytes, now, 0, animations);
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
                        self.typed(session, vec![0x03], now, 0, animations);
                    }
                }
                Event::Cut => {
                    // Shift+Delete on Windows also arrives as Cut.
                    let bytes = if mods_now.ctrl { vec![0x18] } else { b"\x1b[3;2~".to_vec() };
                    self.typed(session, bytes, now, 0, animations);
                }
                Event::Paste(text) => {
                    if mods_now.ctrl && !mods_now.shift {
                        self.typed(session, vec![0x16], now, 0, animations);
                    } else {
                        // One burst for the lot; what it prints is output.
                        self.typed(session, keys::encode_paste(&text), now, 0, animations);
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
            // PuTTY habit: plain right-click pastes. Ctrl+right-click opens
            // the context menu without sending anything to the device.
            out.paste_requested = !ctrl_secondary_click(ui);
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
        emulator: &Mutex<Emulator>,
        options: &ViewOptions<'_>,
        m: &Metrics,
        focused: bool,
        cursor_on: bool,
        now: f64,
    ) {
        let painter = ui.painter().clone();
        let theme = options.theme;
        painter.rect_filled(area, 0.0, theme.bg);

        let emu = emulator.lock();
        // The animator compares the live screen with last frame's under the
        // same lock the drawing reads it with. With animations off it's
        // neither told nor asked anything, and every cell draws as it
        // always has.
        let anim = if options.animations.enabled {
            let screen = live_screen(&emu, &mut self.screen);
            self.animator.update(now, &options.animations, &screen);
            Some(&self.animator)
        } else {
            None
        };

        let term = emu.term();
        let grid = term.grid();
        let offset = grid.display_offset();
        let overrides = term.colors();
        let selection = term.selection.as_ref().and_then(|s| s.to_range(term));
        let highlighting = options.syntax != "none";
        let columns = m.columns.min(grid.columns());
        let lines = m.lines.min(grid.screen_lines());
        // With this many glyphs zooming at once they fade in at their own size.
        let keep_size = anim.is_some_and(|a| too_many_scaled(a, now, columns, lines));

        // Everything slides with the scroll animation and shakes together,
        // kept inside the grid it slides past the edges of.
        let (scroll, d, painter) = match anim {
            Some(a) => {
                let scroll = a.scroll_offset(now);
                let d = Metrics { origin: m.origin + a.shake(now) + vec2(0.0, scroll * m.cell.y), ..*m };
                let rows = m.origin.y..=m.origin.y + lines as f32 * m.cell.y;
                (scroll, d, painter.with_clip_rect(Rect::from_x_y_ranges(area.x_range(), rows)))
            }
            None => (0.0, *m, painter),
        };

        let mut jobs: Vec<(Pos2, LayoutJob, Option<TSTransform>)> = Vec::new();
        for row in drawn_rows(scroll, lines, offset, grid.history_size()) {
            let line = Line(row - offset as i32);
            let cells = &grid[line];
            // Only the live screen (lines 0 and down) animates.
            let live = anim.zip(usize::try_from(line.0).ok());
            let rect = |column: usize, width: usize| d.rect_at(row as f32, column as f32, width as f32);

            let spans = if highlighting {
                let text: String = (0..columns).map(|c| cells[Column(c)].c).collect();
                highlight_cached(&mut self.highlight_cache, &text, options.syntax)
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
                    painter.rect_filled(rect(start, width), 0.0, color);
                }
            };
            // Typed characters flashing the cell behind them.
            let mut flashes: Vec<(usize, usize, f32)> = Vec::new();

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
                let look = live.and_then(|(a, live)| a.cell(now, live, column));
                if look.is_some_and(|l| l.hidden) && !selected {
                    bg = theme.bg; // not shown yet, so blank
                }

                match &mut bg_run {
                    Some((start, w, color)) if *color == bg && *start + *w == column => *w += width,
                    _ => {
                        flush_bg(&mut bg_run);
                        bg_run = Some((column, width, bg));
                    }
                }

                let mut ch = if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c };
                let (mut offset_y, mut scale) = (0.0, 1.0);
                if let Some(look) = look {
                    if look.hidden {
                        continue;
                    }
                    if look.flash > 0.0 {
                        flashes.push((column, width, look.flash));
                    }
                    fg = animated_colour(fg, bg, theme.cursor, &look);
                    ch = look.glyph.unwrap_or(ch);
                    (offset_y, scale) = (look.offset_y, if keep_size { 1.0 } else { look.scale });
                }
                let bold = cell.flags.contains(Flags::BOLD);
                let italic = cell.flags.contains(Flags::ITALIC);
                let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
                let strike = cell.flags.contains(Flags::STRIKEOUT);
                // Non-ASCII glyphs may come from a fallback font with a
                // different advance, so each gets its own run pinned to its
                // column; plain ASCII runs stay monospace-aligned. A glyph
                // changing size grows about its own centre, so it's alone too.
                let simple = ch.is_ascii() && width == 1 && scale == 1.0;
                // While text fades or moves, a plain blank joins the run it
                // follows whatever its colour: it draws nothing, and splitting
                // there would lay out every word on its own.
                let blank = anim.is_some() && ch == ' ' && !underline && !strike;
                let extend = simple
                    && runs.last().is_some_and(|r| {
                        r.column + r.width == column
                            && (r.fg == fg || blank)
                            && r.bold == bold
                            && r.italic == italic
                            && r.underline == underline
                            && r.strike == strike
                            && r.text.is_ascii()
                            && (r.offset_y == offset_y || blank)
                            && r.scale == 1.0
                    });
                if extend {
                    let run = runs.last_mut().unwrap();
                    run.text.push(ch);
                    run.width += 1;
                } else {
                    let mut text = String::new();
                    text.push(ch);
                    if let Some(extra) = cell.zerowidth().filter(|_| look.is_none_or(|l| l.glyph.is_none())) {
                        text.extend(extra.iter());
                    }
                    runs.push(Run { column, width, text, fg, bold, italic, underline, strike, offset_y, scale });
                }
            }
            flush_bg(&mut bg_run);
            for (column, width, flash) in flashes {
                painter.rect_filled(rect(column, width), 0.0, theme.cursor.gamma_multiply(flash));
            }
            if let Some(mark) = live.and_then(|(a, live)| a.line_mark(now, live)) {
                paint_line_mark(&painter, &d, row as f32, &mark, &theme, columns);
            }

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
                let at = rect(run.column, run.width);
                // Moved or resized as it's drawn: laying text out at
                // fractional sizes would fill the font atlas.
                let motion = (run.offset_y != 0.0 || run.scale != 1.0)
                    .then(|| scale_about(at.center(), run.scale, vec2(0.0, run.offset_y * d.cell.y)));
                jobs.push((at.min, LayoutJob::single_section(run.text, format), motion));
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
        let animated_cursor = anim.zip(cursor).map(|(a, (point, _, _, wide))| {
            let actual = (point.line.0.max(0) as usize, point.column.0);
            // Unfocused it stays a still outline, but keeps pace with the text.
            let look = if focused {
                a.cursor(now, actual)
            } else {
                let (row, column) = a.reveal_head(now).unwrap_or(actual);
                CursorLook { pos: (row as f32, column as f32), tail: None, ghosts: Vec::new() }
            };
            let under = look.cell().filter(|&(row, column)| row < lines && column < columns);
            let cell = under.map(|(row, column)| &grid[Line(row as i32)][Column(column)]);
            let wide = cell.map_or(wide, |c| c.flags.contains(Flags::WIDE_CHAR));
            let glyph = under.zip(cell).and_then(|((row, column), cell)| {
                let shown = !a.cell(now, row, column).is_some_and(|l| l.hidden)
                    && !cell.flags.intersects(Flags::HIDDEN | Flags::WIDE_CHAR_SPACER);
                (shown && !cell.c.is_whitespace()).then(|| (cell.c, cell.flags.contains(Flags::BOLD)))
            });
            AnimatedCursor { look, width: if wide { 2.0 } else { 1.0 }, glyph }
        });
        drop(emu);

        for (pos, job, motion) in jobs {
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            match motion {
                None => painter.galley(pos, galley, theme.fg),
                Some(transform) => {
                    let mut text = TextShape::new(pos, galley, theme.fg);
                    text.transform(transform);
                    painter.add(text);
                }
            }
        }

        if let Some(cursor) = &animated_cursor {
            self.paint_animated_cursor(&painter, &d, cursor, options, focused, now);
        } else if let Some((point, ch, bold, wide)) = cursor {
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

        // Last, over everything, but still inside the terminal.
        if let Some(anim) = anim {
            let painter = ui.painter().with_clip_rect(area);
            for particle in anim.particles(now) {
                paint_particle(&painter, &d, &particle, offset, theme.cursor);
            }
        }
    }

    /// The cursor with animations on: moving, smeared or trailing
    /// afterimages, and blinking in its style while it has the keyboard.
    fn paint_animated_cursor(
        &self,
        painter: &Painter,
        d: &Metrics,
        cursor: &AnimatedCursor,
        options: &ViewOptions<'_>,
        focused: bool,
        now: f64,
    ) {
        let theme = options.theme;
        let rect = |(row, column): (f32, f32)| d.rect_at(row, column, cursor.width);
        let head = rect(cursor.look.pos);
        if !focused {
            painter.rect_stroke(
                head.shrink(0.5),
                CornerRadius::ZERO,
                Stroke::new(1.0, theme.cursor),
                egui::StrokeKind::Inside,
            );
            return;
        }
        for &(at, alpha) in &cursor.look.ghosts {
            painter.rect_filled(rect(at), 0.0, theme.cursor.gamma_multiply(alpha));
        }
        let style = options.animations.cursor_blink;
        let halo = animation::blink_halo(style, now, self.blink_epoch);
        if halo > 0.01 {
            let spread = 1.0 + 3.0 * halo;
            let glow =
                RectShape::filled(head.expand(spread), CornerRadius::same(2), theme.cursor.gamma_multiply(0.5 * halo));
            painter.add(glow.with_blur_width(2.0 * spread + 2.0));
        }
        let level = animation::blink_level(style, now, self.blink_epoch);
        if level <= 0.0 {
            return;
        }
        let fill = theme.cursor.gamma_multiply(level);
        match cursor.look.tail {
            Some(tail) => {
                painter.add(Shape::convex_polygon(smear_outline(rect(tail), head), fill, Stroke::NONE));
            }
            None => {
                painter.rect_filled(head, 0.0, fill);
            }
        }
        if let Some((ch, bold)) = cursor.glyph {
            let font = if bold { options.bold.clone() } else { options.regular.clone() };
            painter.text(head.min, egui::Align2::LEFT_TOP, ch, font, theme.bg.gamma_multiply(level));
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

fn highlight_cached(cache: &mut HashMap<String, Vec<Span>>, text: &str, syntax: &str) -> Vec<Span> {
    if let Some(spans) = cache.get(text) {
        return spans.clone();
    }
    if cache.len() >= HIGHLIGHT_CACHE_MAX {
        cache.clear(); // cheaper than tracking an LRU for this
    }
    let spans = highlight::highlight_line(text, syntax);
    cache.insert(text.to_string(), spans.clone());
    spans
}

// -- animations -------------------------------------------------------------

/// The cursor's cell on the live screen; on the second half of a wide
/// character it's drawn over the whole character.
fn live_cursor(emu: &Emulator) -> (usize, usize) {
    let mut point = emu.cursor();
    if point.column.0 > 0 && emu.term().grid()[point].flags.contains(Flags::WIDE_CHAR_SPACER) {
        point.column -= 1;
    }
    (point.line.0.max(0) as usize, point.column.0)
}

/// The live screen for the animator, whatever the scrollback view shows.
/// Concealed text and the second halves of wide characters read as blank.
fn live_screen<'a>(emu: &Emulator, cells: &'a mut Vec<char>) -> Snapshot<'a> {
    let grid = emu.term().grid();
    let (columns, lines) = (grid.columns(), grid.screen_lines());
    let blank = Flags::HIDDEN | Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
    cells.clear();
    for line in 0..lines {
        let row = &grid[Line(line as i32)];
        cells.extend((0..columns).map(|column| {
            let cell = &row[Column(column)];
            if cell.flags.intersects(blank) { ' ' } else { cell.c }
        }));
    }
    let cells: &'a [char] = cells;
    Snapshot {
        cells,
        columns,
        history_size: grid.history_size(),
        display_offset: grid.display_offset(),
        cursor: live_cursor(emu),
        alt_screen: emu.alt_screen(),
    }
}

/// Viewport rows to draw with everything `scroll` rows lower than its grid
/// position: those with any part inside the grid, and only as far as the
/// scrollback and the screen go.
fn drawn_rows(scroll: f32, lines: usize, display_offset: usize, history: usize) -> Range<i32> {
    let first = (-scroll).floor() as i32;
    let end = (lines as f32 - scroll).ceil() as i32;
    // Viewport row r shows grid line r - display_offset.
    let oldest = display_offset as i32 - history as i32;
    let past_newest = (lines + display_offset) as i32;
    first.max(oldest)..end.min(past_newest)
}

/// A glyph's colour partway through an animation: `accent` of the way to
/// the accent colour, then faded toward its cell's background by
/// `1 - alpha`.
fn animated_colour(fg: Color32, bg: Color32, accent: Color32, look: &CellLook) -> Color32 {
    let step = |t: f32| (t.clamp(0.0, 1.0) * MIX_STEPS).round() / MIX_STEPS;
    fg.lerp_to_gamma(accent, step(look.accent)).lerp_to_gamma(bg, step(1.0 - look.alpha))
}

/// Whether more than [`MAX_SCALED`] cells of the live screen are changing
/// size this frame.
fn too_many_scaled(anim: &Animator, now: f64, columns: usize, lines: usize) -> bool {
    // Stops at the first one over the limit, and every cell of a screen with
    // nothing animating says so at once.
    (0..lines)
        .flat_map(|row| (0..columns).map(move |column| (row, column)))
        .filter(|&(row, column)| anim.cell(now, row, column).is_some_and(|look| look.scale != 1.0))
        .nth(MAX_SCALED)
        .is_some()
}

/// Scale by `scale` about `centre`, then move by `offset`.
fn scale_about(centre: Pos2, scale: f32, offset: Vec2) -> TSTransform {
    TSTransform::new(centre.to_vec2() * (1.0 - scale) + offset, scale)
}

/// When to draw again with animations on, from when the animations and the
/// blink each next need it: 0 for the next frame, `None` for not until
/// something happens.
fn next_frame(animating: Option<f64>, blink: Option<f64>) -> Option<f64> {
    match (animating, blink) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// A cursor smeared from `tail` to `head`: the convex hull of both cells.
fn smear_outline(tail: Rect, head: Rect) -> Vec<Pos2> {
    let corners =
        [tail, head].into_iter().flat_map(|r| [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]);
    convex_hull(corners.collect())
}

/// Andrew's monotone chain. Clockwise on screen (y down), which is the
/// order egui fills fastest.
fn convex_hull(mut points: Vec<Pos2>) -> Vec<Pos2> {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let turn = |o: Pos2, a: Pos2, b: Pos2| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut hull: Vec<Pos2> = Vec::with_capacity(points.len() + 1);
    for (pass, chain) in [points.clone(), points.iter().rev().copied().collect()].into_iter().enumerate() {
        let floor = if pass == 0 { 0 } else { hull.len() - 1 };
        for p in chain.into_iter().skip(pass) {
            while hull.len() >= floor + 2 && turn(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
                hull.pop();
            }
            hull.push(p);
        }
    }
    hull.pop(); // back where it started
    hull
}

/// The mark on a row of fresh output; [`LineMark`] says how each style is
/// drawn.
fn paint_line_mark(painter: &Painter, d: &Metrics, row: f32, mark: &LineMark, theme: &Theme, columns: usize) {
    let span = |from: f32, to: f32| d.rect_at(row, from, to - from);
    let (start, end) = (mark.start as f32, mark.end as f32);
    match mark.style {
        NewLines::Off => {}
        NewLines::Glow => {
            let to = (mark.end + 1).min(columns) as f32;
            painter.rect_filled(span(start, to), 0.0, theme.cursor.gamma_multiply(mark.alpha));
        }
        NewLines::Flash => {
            painter.rect_filled(span(start, end), 0.0, theme.fg.gamma_multiply(mark.alpha));
        }
        NewLines::Marker => {
            // In the padding, left of the text.
            let bar = Rect::from_x_y_ranges(d.origin.x - 3.5..=d.origin.x - 1.0, span(0.0, 1.0).y_range());
            painter.rect_filled(bar, 0.0, theme.cursor.gamma_multiply(mark.alpha));
        }
        NewLines::Underline => {
            if mark.reach > 0.0 {
                let under = span(start, start + mark.reach);
                let line = Rect::from_x_y_ranges(under.x_range(), under.max.y - 2.0..=under.max.y - 0.5);
                painter.rect_filled(line, 0.0, theme.cursor.gamma_multiply(mark.alpha));
            }
        }
        NewLines::Shimmer => {
            for column in mark.start..mark.end {
                let light = mark.band(column as f32 + 0.5);
                if light > 0.0 {
                    let cell = span(column as f32, column as f32 + 1.0);
                    painter.rect_filled(cell, 0.0, theme.fg.gamma_multiply(light));
                }
            }
        }
    }
}

/// One particle of a keystroke burst. Its position is on the live screen,
/// so it's drawn `display_offset` rows further down the view.
fn paint_particle(painter: &Painter, d: &Metrics, particle: &ParticleLook, display_offset: usize, accent: Color32) {
    let (row, column) = particle.pos;
    let centre = pos2(d.origin.x + column * d.cell.x, d.origin.y + (row + display_offset as f32) * d.cell.y);
    let radius = particle.size * d.cell.y;
    if radius <= 0.0 || particle.alpha <= 0.0 {
        return;
    }
    let colour = match particle.colour {
        ParticleColour::Accent => accent,
        ParticleColour::Fixed(colour) => colour,
    }
    .gamma_multiply(particle.alpha);
    // Corners clockwise on screen, turned by `angle` about the centre.
    let turned = |angle: f32, corners: [Vec2; 4]| {
        let rot = Rot2::from_angle(angle);
        corners.iter().map(|&v| centre + rot * v).collect::<Vec<Pos2>>()
    };
    match particle.shape {
        ParticleShape::Dot => {
            painter.circle_filled(centre, radius, colour);
        }
        ParticleShape::Square { angle } => {
            let r = radius;
            let corners = turned(angle, [vec2(-r, -r), vec2(r, -r), vec2(r, r), vec2(-r, r)]);
            painter.add(Shape::convex_polygon(corners, colour, Stroke::NONE));
        }
        ParticleShape::Star { angle } => {
            // Two slim crossed diamonds make the four points.
            let (long, thin) = (radius, 0.3 * radius);
            for arm in [angle, angle + FRAC_PI_2] {
                let corners = turned(arm, [vec2(long, 0.0), vec2(0.0, thin), vec2(-long, 0.0), vec2(0.0, -thin)]);
                painter.add(Shape::convex_polygon(corners, colour, Stroke::NONE));
            }
        }
        ParticleShape::Ring => {
            painter.circle_stroke(centre, radius, Stroke::new((0.12 * radius).clamp(1.0, 1.5), colour));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;
    use crate::settings::{CursorBlink, NewText, Reveal, Scrolling};

    fn metrics() -> Metrics {
        Metrics { origin: pos2(10.0, 20.0), cell: vec2(8.0, 16.0), columns: 80, lines: 24 }
    }

    struct ContextMenuTest {
        view: TerminalView,
        session: Session,
        requests: Vec<ViewOutput>,
    }

    fn context_harness() -> Harness<'static, ContextMenuTest> {
        let state = ContextMenuTest {
            view: TerminalView::default(),
            session: Session::new(Default::default(), Theme::default(), || {}),
            requests: Vec::new(),
        };
        let mut harness = Harness::builder().with_size([600.0, 400.0]).build_ui_state(
            |ui, state: &mut ContextMenuTest| {
                let options = ViewOptions { keyboard: true, ..view_options(Animations::default()) };
                let out = state.view.show(ui, &state.session, &options);
                if out.copy.is_some() || out.paste_requested || out.text_export.is_some() {
                    state.requests.push(out);
                }
            },
            state,
        );
        harness.run_ok();
        let mut emu = harness.state().session.shared.emulator.lock();
        let output: String = (0..80).map(|i| format!("line {i}\r\n")).collect();
        emu.feed(output.as_bytes());
        emu.feed("switch# café".as_bytes());
        emu.scroll_to(5);
        drop(emu);
        harness.run_ok();
        harness
    }

    fn open_context_menu(harness: &mut Harness<'_, ContextMenuTest>) {
        harness
            .get_by_label("Terminal output")
            .click_button_modifiers(egui::PointerButton::Secondary, egui::Modifiers::CTRL);
        harness.run_ok();
        harness.get_by_label("Save output to text file…");
    }

    #[test]
    fn ctrl_right_click_copies_and_exports_history_and_selection_without_pasting() {
        let mut harness = context_harness();
        let text = format!("{}switch# café", (0..80).map(|i| format!("line {i}\n")).collect::<String>());
        open_context_menu(&mut harness);
        assert!(harness.state().requests.is_empty(), "opening the menu also pasted");
        assert!(harness.get_by_label("Copy").accesskit_node().is_disabled());
        assert!(harness.get_by_label("Save selection to text file…").accesskit_node().is_disabled());
        harness.get_by_label("Copy all").click();
        harness.run_ok();
        assert_eq!(harness.state().requests.last().unwrap().copy.as_deref(), Some(text.as_str()));
        assert!(!harness.state().requests.last().unwrap().paste_requested);
        assert_eq!(harness.state().session.shared.emulator.lock().display_offset(), 5);
        assert!(!harness.state().session.shared.emulator.lock().has_selection());

        open_context_menu(&mut harness);
        harness.get_by_label("Save output to text file…").click();
        harness.run_ok();
        assert!(
            matches!(harness.state().requests.last().unwrap().text_export.as_ref(), Some(TextExport::Save(saved)) if saved == &text)
        );
        // The export is a snapshot: subsequent device output cannot alter it.
        harness.state().session.shared.emulator.lock().feed(b" new output");
        assert!(
            matches!(harness.state().requests.last().unwrap().text_export.as_ref(), Some(TextExport::Save(saved)) if saved == &text)
        );

        {
            let mut emu = harness.state().session.shared.emulator.lock();
            let point = emu.cursor();
            emu.start_selection(point, Side::Left, true);
        }
        let selected = "switch# café new output";
        open_context_menu(&mut harness);
        harness.get_by_label("Copy").click();
        harness.run_ok();
        assert_eq!(harness.state().requests.last().unwrap().copy.as_deref(), Some(selected));
        open_context_menu(&mut harness);
        harness.get_by_label("Save selection to text file…").click();
        harness.run_ok();
        assert!(
            matches!(harness.state().requests.last().unwrap().text_export.as_ref(), Some(TextExport::Save(saved)) if saved == selected)
        );
        open_context_menu(&mut harness);
        harness.get_by_label("Append to text file…").click();
        harness.run_ok();
        assert!(
            matches!(harness.state().requests.last().unwrap().text_export.as_ref(), Some(TextExport::Append(saved)) if saved == selected)
        );
        open_context_menu(&mut harness);
        harness.get_by_label("Clear selection").click();
        harness.run_ok();
        assert!(!harness.state().session.shared.emulator.lock().has_selection());
        open_context_menu(&mut harness);
        harness.get_by_label("Append to text file…").click();
        harness.run_ok();
        assert!(
            matches!(harness.state().requests.last().unwrap().text_export.as_ref(), Some(TextExport::Append(saved)) if saved == &(text.clone() + " new output"))
        );
        open_context_menu(&mut harness);
        harness.get_by_label("Select all").click();
        harness.run_ok();
        assert_eq!(harness.state().session.shared.emulator.lock().selection_text().unwrap(), text + " new output");
        open_context_menu(&mut harness);
        harness.get_by_label("Scroll to bottom").click();
        harness.run_ok();
        assert_eq!(harness.state().session.shared.emulator.lock().display_offset(), 0);
    }

    #[test]
    fn context_menu_paste_and_plain_right_click_request_paste() {
        let mut harness = context_harness();
        harness.get_by_label("Terminal output").click_secondary();
        harness.run_ok();
        assert!(harness.state().requests.last().unwrap().paste_requested);
        assert!(harness.query_by_label("Save output to text file…").is_none());
        harness.state_mut().requests.clear();
        open_context_menu(&mut harness);
        assert!(harness.state().requests.is_empty());
        harness.get_by_label("Paste").click();
        harness.run_ok();
        assert!(harness.state().requests.last().unwrap().paste_requested);
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

    #[test]
    fn rects_between_cells_land_on_cells_when_whole() {
        let m = Metrics { origin: pos2(10.3, 20.7), cell: vec2(8.4, 17.0), ..metrics() };
        for (row, column, width) in [(0, 0, 1), (3, 7, 2), (23, 79, 1)] {
            assert_eq!(m.rect_at(row as f32, column as f32, width as f32), m.cell_rect(row, column, width));
        }
        // Halfway between rows, and a row above the grid.
        assert_eq!(m.rect_at(1.5, 0.0, 1.0).min.y, 20.7 + 1.5 * 17.0);
        assert_eq!(m.rect_at(-1.0, 0.0, 1.0).max.y, 20.7);
    }

    #[test]
    fn sliding_rows_come_from_the_scrollback_and_stop_at_its_ends() {
        // Nothing moving: the screen as it is.
        assert_eq!(drawn_rows(0.0, 24, 0, 100), 0..24);
        assert_eq!(drawn_rows(0.0, 24, 7, 100), 0..24);
        // Text drawn lower: rows slide in from above, the bottom ones slide out.
        assert_eq!(drawn_rows(2.0, 24, 0, 100), -2..22);
        assert_eq!(drawn_rows(0.5, 24, 0, 100), -1..24);
        // ...but there's only so much history above.
        assert_eq!(drawn_rows(5.0, 24, 0, 2), -2..19);
        assert_eq!(drawn_rows(1.0, 24, 0, 0), 0..23);
        // Drawn higher (scrolling back): rows come up from below, as far as
        // the live screen goes.
        assert_eq!(drawn_rows(-0.5, 24, 3, 100), 0..25);
        assert_eq!(drawn_rows(-3.0, 24, 3, 100), 3..27);
        assert_eq!(drawn_rows(-3.0, 24, 1, 100), 3..25);
    }

    #[test]
    fn animated_colours_mix_toward_the_accent_and_fade_into_the_cell() {
        let (fg, bg, accent) = (Color32::from_rgb(200, 200, 200), Color32::from_rgb(20, 20, 20), Color32::GREEN);
        let colour = |alpha: f32, accent_t: f32| {
            animated_colour(fg, bg, accent, &CellLook { alpha, accent: accent_t, ..CellLook::PLAIN })
        };
        assert_eq!(colour(1.0, 0.0), fg);
        assert_eq!(colour(0.0, 0.0), bg);
        assert_eq!(colour(1.0, 1.0), accent);
        assert_eq!(colour(0.0, 1.0), bg);
        let half = colour(0.5, 0.0);
        assert!(half.r() > bg.r() && half.r() < fg.r());
        // Nearly the same point in a fade: the same colour, so one run of text.
        assert_eq!(colour(0.501, 0.0), colour(0.505, 0.0));
        assert_eq!(colour(0.999, 0.0), fg);
    }

    #[test]
    fn glyphs_grow_about_their_centre() {
        let centre = pos2(40.0, 30.0);
        let grow = scale_about(centre, 1.5, Vec2::ZERO);
        assert_eq!(grow * centre, centre);
        assert_eq!(grow * pos2(44.0, 30.0), pos2(46.0, 30.0));
        let rise = scale_about(centre, 1.0, vec2(0.0, 5.0));
        assert_eq!(rise * pos2(1.0, 2.0), pos2(1.0, 7.0));
    }

    /// Whether `p` is inside or on the convex polygon `hull`, going either way round.
    fn inside(hull: &[Pos2], p: Pos2) -> bool {
        let turns: Vec<f32> = (0..hull.len())
            .map(|i| {
                let (a, b) = (hull[i], hull[(i + 1) % hull.len()]);
                (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
            })
            .collect();
        turns.iter().all(|&t| t >= -1e-3) || turns.iter().all(|&t| t <= 1e-3)
    }

    #[test]
    fn a_smear_is_the_hull_of_both_cells() {
        let cell = |x: f32, y: f32| Rect::from_min_size(pos2(x, y), vec2(8.0, 16.0));

        // Along a row: one long bar.
        let (tail, head) = (cell(0.0, 0.0), cell(40.0, 0.0));
        let outline = smear_outline(tail, head);
        assert_eq!(outline.len(), 4, "{outline:?}");
        assert!(outline.contains(&pos2(0.0, 0.0)) && outline.contains(&pos2(48.0, 16.0)));

        // Diagonally: a six-sided sweep that covers both cells.
        let (tail, head) = (cell(0.0, 0.0), cell(40.0, 32.0));
        let outline = smear_outline(tail, head);
        assert_eq!(outline.len(), 6, "{outline:?}");
        for r in [tail, head] {
            assert!(
                [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()].iter().all(|&p| inside(&outline, p))
            );
        }
        assert!(!inside(&outline, pos2(47.0, 1.0)), "the empty corner is left out");

        // Clockwise on screen: turning right at every corner.
        let n = outline.len();
        assert!((0..n).all(|i| {
            let (a, b, c) = (outline[i], outline[(i + 1) % n], outline[(i + 2) % n]);
            (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x) > 0.0
        }));

        // No distance at all: just the cell.
        assert_eq!(smear_outline(tail, tail).len(), 4);
    }

    #[test]
    fn redraws_follow_whichever_needs_it_first() {
        assert_eq!(next_frame(None, None), None);
        assert_eq!(next_frame(Some(0.0), None), Some(0.0));
        assert_eq!(next_frame(None, Some(0.38)), Some(0.38));
        assert_eq!(next_frame(Some(0.0), Some(0.38)), Some(0.0));
        assert_eq!(next_frame(Some(0.5), Some(0.1)), Some(0.1));
    }

    fn emulator(columns: usize, lines: usize, scrollback: usize) -> Mutex<Emulator> {
        Mutex::new(Emulator::new(columns, lines, scrollback, Arc::new(|_| {})))
    }

    #[test]
    fn the_animator_sees_the_live_screen_with_wide_and_hidden_cells_blank() {
        let emu = emulator(10, 3, 100);
        emu.lock().feed("ab\u{4e2d}c\x1b[8mpw\x1b[0m\r\n\u{4e2d}".as_bytes());
        let mut cells = Vec::new();
        let screen = live_screen(&emu.lock(), &mut cells);
        assert_eq!((screen.columns, screen.lines()), (10, 3));
        let row = |r: usize| screen.cells[r * 10..(r + 1) * 10].iter().collect::<String>();
        assert_eq!(row(0), "ab\u{4e2d} c     ");
        assert_eq!(screen.cursor, (1, 2));
        // On the second half of a wide character, the cursor is on the character.
        emu.lock().feed(b"\x1b[D");
        assert_eq!(live_cursor(&emu.lock()), (1, 0));

        // Scrolled back, it's still the live screen.
        emu.lock().feed(b"\r\nx\r\ny\r\nz");
        emu.lock().scroll_by(2);
        let screen = live_screen(&emu.lock(), &mut cells);
        assert_eq!(screen.display_offset, 2);
        assert_eq!(screen.cells[20], 'z');
        assert!(!screen.alt_screen);
    }

    fn on() -> Animations {
        Animations { enabled: true, ..Animations::default() }
    }

    /// One frame's bookkeeping and animator update, as `show` does it.
    fn frame(view: &mut TerminalView, emu: &Mutex<Emulator>, pass: u64, now: f64, animations: &Animations) {
        view.start_frame(pass, animations, false);
        let emu = emu.lock();
        let screen = live_screen(&emu, &mut view.screen);
        view.animator.update(now, animations, &screen);
    }

    #[test]
    fn animations_start_afresh_when_turned_on_or_shown_again() {
        let settings = on();
        let off = Animations { enabled: false, ..settings };
        let emu = emulator(20, 5, 100);
        let mut view = TerminalView::default();
        let fresh = |view: &TerminalView, now: f64| view.animator.cell(now, 0, 0).is_some();

        // Drawn on consecutive passes, new text animates.
        frame(&mut view, &emu, 10, 1.0, &settings);
        emu.lock().feed(b"hello");
        frame(&mut view, &emu, 11, 1.1, &settings);
        assert!(fresh(&view, 1.1));

        // Not drawn for a while (a tab in the background): what arrived
        // meanwhile doesn't animate when it's shown again.
        emu.lock().feed(b"\r\x1b[Kworld");
        frame(&mut view, &emu, 20, 5.0, &settings);
        assert!(!fresh(&view, 5.0));

        // Turned off, then on again: the same.
        view.start_frame(21, &off, false);
        emu.lock().feed(b"\r\x1b[Kagain");
        frame(&mut view, &emu, 22, 6.0, &settings);
        assert!(!fresh(&view, 6.0));

        // A clear or reset from the menu.
        emu.lock().feed(b"\r\x1b[Kmore");
        view.reset_animations();
        frame(&mut view, &emu, 23, 7.0, &settings);
        assert!(!fresh(&view, 7.0));
        emu.lock().feed(b"\r\x1b[Klast");
        frame(&mut view, &emu, 24, 7.1, &settings);
        assert!(fresh(&view, 7.1));

        // Resized.
        view.start_frame(25, &settings, true);
        assert_eq!(view.animator.cell(7.1, 0, 0), None);
    }

    #[test]
    fn a_demo_keystroke_is_typing() {
        let settings = Animations { reveal: Reveal::Typewriter, ..on() };
        let emu = emulator(20, 3, 100);
        emu.lock().feed(b"Switch#");
        let mut view = TerminalView::default();
        frame(&mut view, &emu, 1, 1.0, &settings);
        view.demo_keystroke(&emu, 1, false, 2.0, &settings);
        emu.lock().feed(b"s");
        frame(&mut view, &emu, 2, 2.01, &settings);
        let look = view.animator.cell(2.01, 0, 7).unwrap();
        assert!(look.scale > 1.0 && !look.hidden, "{look:?}");
        assert_eq!(view.blink_epoch, 2.0);
        assert!(view.animator.particles(2.01).count() > 0);

        // Turned off, it only restarts the blink.
        let mut view = TerminalView::default();
        view.demo_keystroke(&emu, 1, false, 3.0, &Animations::default());
        assert_eq!(view.blink_epoch, 3.0);
        assert_eq!(view.animator.particles(3.0).count(), 0);
    }

    // -- drawing whole frames headlessly ------------------------------------

    fn view_options(animations: Animations) -> ViewOptions<'static> {
        ViewOptions {
            theme: Theme::default(),
            syntax: "none",
            regular: FontId::monospace(14.0),
            bold: FontId::monospace(14.0),
            keyboard: false,
            animations,
        }
    }

    /// Draw one frame of the demo at `time`, as a window `size` big.
    fn draw(
        ctx: &egui::Context,
        view: &mut TerminalView,
        emu: &Mutex<Emulator>,
        options: &ViewOptions<'_>,
        time: f64,
        size: Vec2,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(time),
            predicted_dt: 0.0,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| view.show_demo(ui, size, emu, options));
        out.textures_delta.clear(); // there's no GPU to hand the font atlas to
        out
    }

    fn repaint_delay(out: &egui::FullOutput) -> std::time::Duration {
        out.viewport_output[&egui::ViewportId::ROOT].repaint_delay
    }

    fn texts(out: &egui::FullOutput) -> usize {
        out.shapes.iter().filter(|s| matches!(s.shape, Shape::Text(_))).count()
    }

    #[test]
    fn with_animations_off_only_the_blink_redraws() {
        let ctx = egui::Context::default();
        let emu = emulator(20, 5, 100);
        emu.lock().feed(b"Switch#");
        let mut view = TerminalView::default();
        let options = view_options(Animations::default());
        let size = vec2(400.0, 200.0);
        for i in 0..3 {
            let _ = draw(&ctx, &mut view, &emu, &options, 10.0 + f64::from(i) * 0.01, size);
        }
        let out = draw(&ctx, &mut view, &emu, &options, 10.1, size);
        let expected = until_next_blink(10.1, 0.0);
        assert!((repaint_delay(&out).as_secs_f64() - expected).abs() < 1e-6, "{:?}", repaint_delay(&out));
        // Nothing was handed to the animator.
        assert!(view.screen.is_empty());
    }

    #[test]
    fn with_animations_on_it_redraws_while_they_run_then_rests() {
        let ctx = egui::Context::default();
        let emu = emulator(20, 5, 100);
        emu.lock().feed(b"Switch#");
        let mut view = TerminalView::default();
        let settings = Animations { cursor_blink: CursorBlink::Classic, reveal: Reveal::Typewriter, ..on() };
        let options = view_options(settings);
        let size = vec2(400.0, 200.0);
        for i in 0..3 {
            let _ = draw(&ctx, &mut view, &emu, &options, 10.0 + f64::from(i) * 0.01, size);
        }
        // Settled: the classic blink's own schedule, exactly.
        let out = draw(&ctx, &mut view, &emu, &options, 10.1, size);
        assert!((repaint_delay(&out).as_secs_f64() - until_next_blink(10.1, 0.0)).abs() < 1e-6);

        // Output arriving, scrolling the screen with rows sliding in from
        // the scrollback, keeps it drawing every frame...
        let lines: String = (0..30).map(|i| format!("\r\nline {i}")).collect();
        emu.lock().feed(lines.as_bytes());
        let out = draw(&ctx, &mut view, &emu, &options, 10.2, size);
        assert_eq!(repaint_delay(&out), std::time::Duration::ZERO);
        assert!(view.animator.scroll_offset(10.2) > 0.0);
        let mut now = 10.2;
        while now < 12.0 {
            now += 0.016;
            let _ = draw(&ctx, &mut view, &emu, &options, now, size);
        }
        // ...until it's all settled.
        let out = draw(&ctx, &mut view, &emu, &options, 12.1, size);
        assert!((repaint_delay(&out).as_secs_f64() - until_next_blink(12.1, 0.0)).abs() < 1e-6);

        // Scrolling back yourself slides rows up from below.
        emu.lock().scroll_by(3);
        let _ = draw(&ctx, &mut view, &emu, &options, 12.2, size);
        assert!(view.animator.scroll_offset(12.2) < 0.0);
        for i in 1..20 {
            let _ = draw(&ctx, &mut view, &emu, &options, 12.2 + f64::from(i) * 0.016, size);
        }

        // With no scrollback, nothing slides in from above.
        let emu = emulator(20, 5, 0);
        let mut view = TerminalView::default();
        for i in 0..3 {
            let _ = draw(&ctx, &mut view, &emu, &options, 20.0 + f64::from(i) * 0.01, size);
        }
        emu.lock().feed(lines.as_bytes());
        for i in 0..20 {
            let _ = draw(&ctx, &mut view, &emu, &options, 20.1 + f64::from(i) * 0.016, size);
        }
    }

    #[test]
    fn a_whole_screen_fading_in_is_drawn_in_runs() {
        let ctx = egui::Context::default();
        let size = vec2(1000.0, 1000.0);
        let settings = Animations { new_text: NewText::Fade, scrolling: Scrolling::Off, ..on() };
        let options = view_options(settings);
        let emu = emulator(80, 50, 100);
        let mut view = TerminalView::default();
        for i in 0..3 {
            let _ = draw(&ctx, &mut view, &emu, &options, 1.0 + f64::from(i) * 0.01, size);
        }
        let lines = emu.lock().lines();
        let screenful: String =
            (0..lines - 1).map(|i| format!("interface Gi1/0/{i} is up, line protocol is up\r\n")).collect();
        emu.lock().feed(screenful.as_bytes());
        let out = draw(&ctx, &mut view, &emu, &options, 1.1, size);
        assert!(view.animator.cell(1.1, 0, 0).is_some(), "not fading in");
        assert!(texts(&out) <= 2 * lines, "{} pieces of text for {lines} lines", texts(&out));

        // A typewriter reveal fading each character in shares runs too.
        let settings = Animations { reveal: Reveal::Typewriter, ..settings };
        let options = view_options(settings);
        let mut view = TerminalView::default();
        emu.lock().feed(b"\x1b[2J\x1b[H");
        for i in 0..3 {
            let _ = draw(&ctx, &mut view, &emu, &options, 2.0 + f64::from(i) * 0.01, size);
        }
        emu.lock().feed(screenful.as_bytes());
        for i in 0..6 {
            let out = draw(&ctx, &mut view, &emu, &options, 2.1 + f64::from(i) * 0.05, size);
            assert!(texts(&out) <= 2 * lines, "{} pieces of text for {lines} lines", texts(&out));
        }

        // The same on a big terminal with the styles that move or resize
        // glyphs: each of those could easily mean a piece of text per glyph.
        let big_screenful = |columns: usize, lines: usize| {
            let row: String =
                "interface Gi1/0/1 is up, line protocol is up ".chars().cycle().take(columns - 1).collect();
            (0..lines - 1).map(|_| format!("{row}\r\n")).collect::<String>()
        };
        for (new_text, reveal) in [
            (NewText::Zoom, Reveal::Instant),
            (NewText::Decode, Reveal::Instant),
            (NewText::Rise, Reveal::Typewriter),
            (NewText::Drop, Reveal::Words),
        ] {
            let (lines, counts) = texts_as_output_arrives(Animations { new_text, reveal, ..on() }, big_screenful);
            for (frame, n) in counts.iter().enumerate() {
                assert!(
                    *n <= 2 * lines,
                    "{new_text:?}, {reveal:?}: {n} pieces of text for {lines} lines, frame {frame}"
                );
            }
        }

        // Zoom with a typewriter reveal: a piece for each glyph while there
        // are few enough, then only a fade.
        let zoom = Animations { new_text: NewText::Zoom, reveal: Reveal::Typewriter, ..on() };
        let (lines, counts) = texts_as_output_arrives(zoom, big_screenful);
        assert!(counts.iter().all(|&n| n <= MAX_SCALED + 2 * lines), "{counts:?} for {lines} lines");
        assert!(counts.iter().any(|&n| n > 2 * lines), "never zoomed: {counts:?}");
    }

    /// A big terminal (200 columns or so) gets all of `output` in one go, and
    /// the number of pieces of text drawn in each of the next frames comes
    /// back with its height in lines. `output` is told the columns and lines.
    fn texts_as_output_arrives(settings: Animations, output: impl Fn(usize, usize) -> String) -> (usize, Vec<usize>) {
        let ctx = egui::Context::default();
        let size = vec2(1700.0, 1000.0);
        let options = view_options(Animations { scrolling: Scrolling::Off, ..settings });
        let emu = emulator(80, 50, 100);
        let mut view = TerminalView::default();
        for i in 0..3 {
            let _ = draw(&ctx, &mut view, &emu, &options, 1.0 + f64::from(i) * 0.01, size);
        }
        let (columns, lines) = {
            let emu = emu.lock();
            (emu.columns(), emu.lines())
        };
        assert!(columns >= 150 && lines >= 50, "only {columns}x{lines}");
        emu.lock().feed(output(columns, lines).as_bytes());
        let mut counts = Vec::new();
        for i in 0..16 {
            let now = 1.1 + f64::from(i) * 0.016;
            let out = draw(&ctx, &mut view, &emu, &options, now, size);
            if i == 0 {
                assert!(view.animator.cell(now, 0, 0).is_some(), "not animating");
            }
            counts.push(texts(&out));
        }
        (lines, counts)
    }

    /// Glyphs that change size can't share a run of text, so a screenful of
    /// them would be thousands of shapes; they stay one each only while
    /// there are few.
    #[test]
    fn glyphs_zoom_one_by_one_until_there_are_too_many() {
        let zoom = Animations { new_text: NewText::Zoom, ..on() };
        let rows = |more: usize| {
            move |columns: usize, _lines: usize| {
                let row = "X".repeat(columns - 1);
                (0..MAX_SCALED / (columns - 1) + more).map(|_| format!("{row}\r\n")).collect::<String>()
            }
        };
        // Just under the limit: each glyph grows about its own centre.
        let (lines, few) = texts_as_output_arrives(zoom, rows(0));
        assert!(few[0] > MAX_SCALED / 2, "{few:?}");
        // One more row takes it over, and they only fade in.
        let (_, many) = texts_as_output_arrives(zoom, rows(1));
        assert!(many.iter().all(|&n| n <= 2 * lines), "{many:?} for {lines} lines");
    }
}
