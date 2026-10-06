//! The find bar over a terminal (Ctrl+F): what's being looked for, which
//! match is current, and the slices of work that find and count matches
//! (see [`crate::terminal::search`]). Each tab's terminal view has its own.
//!
//! Newer output is at the bottom, so a new search picks the nearest match at
//! or above the bottom of the view, and Enter keeps going up toward older
//! output; Shift+Enter comes back down. Matches are numbered from the top.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Line, Point};
use egui::text::{CCursor, CCursorRange};
use egui::{Align, Color32, Id, Key, Layout, Rect, RichText, Sense, TextEdit, Ui, UiBuilder, WidgetInfo, WidgetType};
use parking_lot::Mutex;

use crate::settings::Theme;
use crate::terminal::emulator::Emulator;
use crate::terminal::search::{Count, Direction, Find, Found, Pattern, SLICE_CELLS, Step, Tally, still_matches};

/// Slices of finding or counting done per frame; the rest waits for the
/// next one.
const SLICES_PER_FRAME: usize = 2;

/// Rows at the top of the view that the bar can cover. A match is moved out
/// from under them when jumped to.
const BAR_ROWS: usize = 2;

/// A tally's revision when it's out of date but still worth showing.
const STALE: u64 = u64::MAX;

/// Sizes of the bar and its parts, in points. Narrow terminals stack the
/// controls so they remain available with the sidebar open.
const BAR_MAX_WIDTH: f32 = 470.0;
const BAR_MIN_WIDTH: f32 = 120.0;
const BAR_MARGIN: f32 = 4.0;
const FIELD_MIN_WIDTH: f32 = 60.0;
const STATUS_WIDTH: f32 = 92.0;
const TOGGLE_WIDTH: f32 = 26.0;
const BUTTON_WIDTH: f32 = 24.0;

/// The current match, and enough to find it again after output moves it.
#[derive(Clone, Copy, Debug)]
struct Current {
    found: Found,
    /// Row id of its first row (see [`Emulator::row_id`]).
    row: i64,
    /// The content it was checked against.
    revision: u64,
}

/// A step from one match to another that hasn't finished yet.
struct Pending {
    find: Find,
    /// Starting from a match whose number is known, so the new one's can be
    /// worked out without counting again.
    from_index: Option<usize>,
}

/// Matches on the rows being drawn, ready for painting.
#[derive(Default)]
pub struct Marks {
    first: i32,
    /// For each row from `first`: covered columns (inclusive) and whether
    /// it's the current match.
    rows: Vec<Vec<(usize, usize, bool)>>,
}

impl Marks {
    /// Whether the cell is in a match, and if so whether the current one.
    pub fn at(&self, line: Line, column: usize) -> Option<bool> {
        let row = self.rows.get(usize::try_from(line.0 - self.first).ok()?)?;
        row.iter().find(|&&(first, last, _)| first <= column && column <= last).map(|&(_, _, current)| current)
    }
}

/// What the marks were worked out for; the same key means they still hold.
#[derive(Clone, Copy, PartialEq, Eq)]
struct MarksKey {
    revision: u64,
    pattern: u64,
    first: Line,
    last: Line,
    current: Option<(Point, Point)>,
}

#[derive(Default)]
pub struct SearchBar {
    open: bool,
    query: String,
    match_case: bool,
    regex: bool,
    /// What `pattern` was made from.
    compiled_for: Option<(String, bool, bool)>,
    /// None for an empty query.
    pattern: Option<Result<Pattern, String>>,
    /// Bumped whenever `pattern` changes.
    generation: u64,
    /// Start again from the bottom of the view (just opened, or the query
    /// changed).
    restart: bool,
    focus_field: bool,
    current: Option<Current>,
    pending: Option<Pending>,
    count: Option<Count>,
    tally: Option<(u64, Tally)>,
    /// The content revision on which the last search found nothing.
    not_found: Option<u64>,
    marks: Marks,
    marks_key: Option<MarksKey>,
    bar_at_bottom: bool,
}

impl SearchBar {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Show the bar and put the keyboard in it, everything selected so
    /// typing replaces it. `prefill` (selected terminal text) becomes the
    /// query.
    pub fn open(&mut self, prefill: Option<String>) {
        if let Some(text) = prefill {
            self.query = if self.regex { Pattern::literal_source(&text) } else { text };
        }
        if !self.open {
            self.restart = true;
        }
        self.open = true;
        self.focus_field = true;
    }

    /// Hide the bar and its highlights. The query and options stay for next
    /// time.
    pub fn close(&mut self) {
        self.open = false;
        self.current = None;
        self.pending = None;
        self.count = None;
        self.tally = None;
        self.not_found = None;
        self.marks_key = None;
        self.bar_at_bottom = false;
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// What the bar says about the matches.
    pub fn status(&self) -> String {
        match &self.pattern {
            None => String::new(),
            Some(Err(error)) => error.clone(),
            Some(Ok(_)) => {
                let tally = self.tally.map(|(_, tally)| tally);
                if self.not_found.is_some() || tally.is_some_and(|t| t.total == 0) {
                    "No matches".into()
                } else if self.pending.is_some() && self.current.is_none() {
                    "Searching…".into()
                } else {
                    match (tally, self.current.is_some()) {
                        (Some(Tally { total, index: Some(index) }), true) => format!("{index} of {total}"),
                        (Some(Tally { total, .. }), false) => {
                            format!("{total} match{}", if total == 1 { "" } else { "es" })
                        }
                        _ => "Counting…".into(),
                    }
                }
            }
        }
    }

    /// The current match, where it is now.
    pub fn current(&self) -> Option<Found> {
        self.current.map(|c| c.found)
    }

    /// Do this frame's share of the work: make the pattern, keep the current
    /// match pinned to its text as output arrives, step toward the next
    /// match and count. `lines` is how many rows the view shows.
    pub fn work(&mut self, ctx: &egui::Context, emulator: &Mutex<Emulator>, lines: usize) {
        if !self.open {
            return;
        }
        let wanted = (self.query.clone(), self.match_case, self.regex);
        if self.compiled_for.as_ref() != Some(&wanted) {
            self.pattern = (!self.query.is_empty()).then(|| Pattern::new(&self.query, self.match_case, self.regex));
            self.compiled_for = Some(wanted);
            self.generation += 1;
            self.restart = true;
        }
        let Some(Ok(pattern)) = self.pattern.clone() else {
            self.restart = false;
            self.current = None;
            self.pending = None;
            self.count = None;
            self.tally = None;
            self.not_found = None;
            return;
        };

        if self.not_found.is_some_and(|revision| revision != emulator.lock().revision()) {
            self.restart = true;
        }

        self.follow_current(emulator, &pattern);

        if std::mem::take(&mut self.restart) {
            let emu = emulator.lock();
            // Up from the bottom of what's on screen, taking a match right
            // there: the nearest one to what the user was looking at.
            let bottom = Line(lines.min(emu.lines()) as i32 - 1 - emu.display_offset() as i32);
            let from = Point::new(bottom, emu.term().grid().last_column());
            self.pending = Some(Pending { find: Find::new(&emu, Direction::Older, from, true), from_index: None });
            self.current = None;
            self.count = None;
            self.tally = None;
            self.not_found = None;
        }

        let mut slices = 0;
        while slices < SLICES_PER_FRAME
            && let Some(pending) = &mut self.pending
        {
            let mut emu = emulator.lock();
            slices += 1;
            match pending.find.step(&emu, &pattern, SLICE_CELLS) {
                Step::Pending => {}
                Step::NotFound => {
                    self.pending = None;
                    self.current = None;
                    self.not_found = Some(emu.revision());
                }
                Step::Found(found) => {
                    let pending = self.pending.take().expect("pending");
                    let revision = emu.revision();
                    let index = self.next_index(&pending, revision);
                    self.current = Some(Current { found, row: emu.row_id(found.start.line), revision });
                    self.not_found = None;
                    match (index, &mut self.tally) {
                        (Some(index), Some((_, tally))) => tally.index = Some(index),
                        _ => {
                            // Its number isn't known: count again.
                            self.count = None;
                            self.tally = self.tally.map(|(_, t)| (STALE, Tally { index: None, ..t }));
                        }
                    }
                    reveal(&mut emu, found, lines);
                }
            }
        }

        let counted = self.tally.is_some_and(|(revision, _)| revision == emulator.lock().revision());
        while !counted && slices < SLICES_PER_FRAME && self.pending.is_none() {
            let emu = emulator.lock();
            slices += 1;
            let focus = self.current.map(|c| c.found);
            let count = self.count.get_or_insert_with(|| Count::new(&emu));
            if let Some(tally) = count.step(&emu, &pattern, focus, SLICE_CELLS) {
                if tally.total == 0 {
                    self.not_found = Some(emu.revision());
                }
                self.tally = Some((emu.revision(), tally));
                self.count = None;
                break;
            }
        }

        let counted = self.tally.is_some_and(|(revision, _)| revision == emulator.lock().revision());
        if self.pending.is_some() || !counted {
            ctx.request_repaint();
        }
    }

    /// The number of the match a finished step landed on, worked out from
    /// where it started when that's known and nothing has changed.
    fn next_index(&self, pending: &Pending, revision: u64) -> Option<usize> {
        let (counted_at, tally) = self.tally?;
        let from = pending.from_index?;
        if counted_at != revision {
            return None;
        }
        let total = tally.total;
        Some(match (pending.find.direction(), pending.find.wrapped()) {
            (Direction::Older, false) => from.checked_sub(1).filter(|&i| i > 0)?,
            (Direction::Older, true) => total,
            (Direction::Newer, false) => Some(from + 1).filter(|&i| i <= total)?,
            (Direction::Newer, true) => 1,
        })
    }

    /// Output arrived: find the current match where its row went. Reflow or
    /// replaced text can invalidate its cells; pick a result again then.
    fn follow_current(&mut self, emulator: &Mutex<Emulator>, pattern: &Pattern) {
        let Some(current) = self.current else { return };
        let emu = emulator.lock();
        if current.revision == emu.revision() {
            return;
        }
        let moved = emu.row_line(current.row).map(|line| line.0 - current.found.start.line.0);
        let found = moved.map(|by| Found {
            start: Point::new(current.found.start.line + by, current.found.start.column),
            end: Point::new(current.found.end.line + by, current.found.end.column),
        });
        match found.filter(|&f| still_matches(&emu, pattern, f)) {
            Some(found) => self.current = Some(Current { found, revision: emu.revision(), ..current }),
            None => {
                self.current = None;
                self.restart = true;
            }
        }
    }

    /// Step to the next match up (older) or down (newer).
    pub fn step(&mut self, emulator: &Mutex<Emulator>, direction: Direction, lines: usize) {
        if !matches!(self.pattern, Some(Ok(_))) {
            return;
        }
        let emu = emulator.lock();
        let (from, inclusive, from_index) = match self.current {
            Some(current) => {
                let index = self.tally.and_then(|(_, t)| t.index);
                (current.found.start, false, index)
            }
            None => {
                // No match picked: start from the bottom of the view.
                let bottom = Line(lines.min(emu.lines()) as i32 - 1 - emu.display_offset() as i32);
                (Point::new(bottom, emu.term().grid().last_column()), true, None)
            }
        };
        self.pending = Some(Pending { find: Find::new(&emu, direction, from, inclusive), from_index });
    }

    /// Where matches are on rows `first..=last`, for painting, and which is
    /// current. Worked out again only when something they depend on changed.
    pub fn marks(&mut self, emulator: &Emulator, first: Line, last: Line) -> Option<&Marks> {
        let Some(Ok(pattern)) = &self.pattern else { return None };
        if !self.open {
            return None;
        }
        let current = self.current.map(|c| c.found);
        let key = MarksKey {
            revision: emulator.revision(),
            pattern: self.generation,
            first,
            last,
            current: current.map(|c| (c.start, c.end)),
        };
        if self.marks_key != Some(key) {
            let last_column = emulator.term().grid().last_column();
            let mut rows = vec![Vec::new(); (last.0 - first.0 + 1).max(0) as usize];
            for found in crate::terminal::search::matches_between(emulator, pattern, first, last) {
                let is_current = Some(found) == current;
                for line in found.start.line.0.max(first.0)..=found.end.line.0.min(last.0) {
                    if let Some((a, b)) = found.columns_on(Line(line), last_column) {
                        rows[(line - first.0) as usize].push((a, b, is_current));
                    }
                }
            }
            self.marks = Marks { first: first.0, rows };
            self.marks_key = Some(key);
        }
        Some(&self.marks)
    }

    /// Draw the bar over the top right of the terminal. Returns what the
    /// user asked for.
    pub fn ui(&mut self, ui: &mut Ui, terminal: Rect, id: Id, keyboard: bool, current: Option<Rect>) -> BarAction {
        let mut action = BarAction::None;
        let spacing = ui.spacing().item_spacing.x;
        let row = ui.spacing().interact_size.y;
        let mut width = (terminal.width() - 12.0).min(BAR_MAX_WIDTH);
        // The text box takes whatever the other parts leave. Buttons are
        // measured: the theme's padding decides how wide they really are.
        let padding = 2.0 * ui.spacing().button_padding.x;
        let font = egui::TextStyle::Button.resolve(ui.style());
        let measure = |text: &str, least: f32| {
            let text = ui.fonts_mut(|f| f.layout_no_wrap(text.into(), font.clone(), Color32::WHITE).size().x);
            (text + padding).max(least).ceil()
        };
        let toggles = [measure("Aa", TOGGLE_WIDTH), measure(".*", TOGGLE_WIDTH)];
        let buttons = [measure("⏶", BUTTON_WIDTH), measure("⏷", BUTTON_WIDTH), measure("×", BUTTON_WIDTH)];
        let fixed = toggles.iter().chain(&buttons).sum::<f32>() + STATUS_WIDTH + 6.0 * spacing;
        let mut inner = width - 2.0 * BAR_MARGIN;
        let mut compact = inner < fixed + FIELD_MIN_WIDTH;
        let compact_height = 3.0 * row + 2.0 * ui.spacing().item_spacing.y + 2.0 * BAR_MARGIN;
        // A tall bar in a short terminal can cover a middle result at either
        // end. Let a single row extend over the sidebar in this case.
        let extend_over_sidebar = width < BAR_MIN_WIDTH || (compact && compact_height > terminal.height() / 2.0);
        if extend_over_sidebar {
            width = (ui.ctx().content_rect().width() - 12.0).min(BAR_MAX_WIDTH);
            inner = width - 2.0 * BAR_MARGIN;
            compact = inner < fixed + FIELD_MIN_WIDTH;
        }
        let compact_toggles = toggles.map(|width| (width - padding + 8.0).max(TOGGLE_WIDTH));
        let compact_navigation = [buttons[0], buttons[1]].map(|width| (width - padding + 8.0).max(BUTTON_WIDTH));
        let controls_width = compact_toggles.iter().chain(&compact_navigation).sum::<f32>();
        let rows = if compact { 3.0 } else { 1.0 };
        let height = rows * row + (rows - 1.0) * ui.spacing().item_spacing.y + 2.0 * BAR_MARGIN;
        if width < BAR_MIN_WIDTH || terminal.height() < height + 8.0 {
            return action;
        }
        let top = Rect::from_min_size(
            egui::pos2(terminal.right() - width - 6.0, terminal.top() + 4.0),
            egui::vec2(width, height),
        );
        let bottom = top.translate(egui::vec2(0.0, terminal.bottom() - 4.0 - top.bottom()));
        let (mut rect, other) = if self.bar_at_bottom { (bottom, top) } else { (top, bottom) };
        // Keep its position until a result needs the space. Never move a
        // button away during a click while a changed query is being matched.
        let clicking = ui.input(|i| i.pointer.any_down() || i.pointer.any_pressed() || i.pointer.any_released());
        if !clicking && current.is_some_and(|current| current.intersects(rect) && !current.intersects(other)) {
            self.bar_at_bottom = !self.bar_at_bottom;
            rect = other;
        }
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
        if extend_over_sidebar {
            child.set_clip_rect(ui.ctx().content_rect());
        }
        // Clicks on the bar's own background stay off the controls under it.
        child.interact(rect, id.with("search_bar"), Sense::click_and_drag());
        let visuals = child.visuals().clone();
        egui::Frame::new()
            .fill(visuals.window_fill)
            .stroke(visuals.window_stroke)
            .corner_radius(4.0)
            .inner_margin(BAR_MARGIN)
            .show(&mut child, |ui| {
                ui.set_width(inner);
                ui.add_enabled_ui(keyboard, |ui| {
                    if compact {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                self.field(ui, id, inner - buttons[2] - spacing, &mut action);
                                close_button(ui, row, buttons[2], &mut action);
                            });
                            ui.horizontal(|ui| {
                                ui.spacing_mut().button_padding.x = 4.0;
                                ui.spacing_mut().item_spacing.x =
                                    spacing.min(((inner - controls_width) / 3.0).max(0.0));
                                self.options(ui, row, compact_toggles);
                                navigation(ui, row, compact_navigation, &mut action);
                            });
                            self.status_ui(ui, inner, row);
                        });
                    } else {
                        ui.horizontal(|ui| {
                            self.field(ui, id, inner - fixed, &mut action);
                            self.options(ui, row, toggles);
                            self.status_ui(ui, STATUS_WIDTH, row);
                            navigation(ui, row, [buttons[0], buttons[1]], &mut action);
                            close_button(ui, row, buttons[2], &mut action);
                        });
                    }
                });
            });
        action
    }

    fn field(&mut self, ui: &mut Ui, id: Id, width: f32, action: &mut BarAction) {
        let field_id = id.with("search_field");
        let output = TextEdit::singleline(&mut self.query)
            .id(field_id)
            .hint_text("Find")
            .desired_width(width.floor().max(FIELD_MIN_WIDTH))
            .return_key(None)
            .show(ui);
        let field = output.response;
        field.widget_info(|| {
            let mut info = WidgetInfo::text_edit(true, &self.query, &self.query, "Find in terminal");
            info.label = Some("Find in terminal".into());
            info
        });
        if std::mem::take(&mut self.focus_field) {
            field.request_focus();
            let mut state = output.state;
            state
                .cursor
                .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(self.query.chars().count()))));
            state.store(ui.ctx(), field_id);
        }
        if field.has_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            *action =
                BarAction::Step(if ui.input(|i| i.modifiers.shift) { Direction::Newer } else { Direction::Older });
        }
    }

    fn options(&mut self, ui: &mut Ui, row: f32, widths: [f32; 2]) {
        for (on, text, name, width) in [
            (&mut self.match_case, "Aa", "Match case", widths[0]),
            (&mut self.regex, ".*", "Regular expression", widths[1]),
        ] {
            let button = ui.add(egui::Button::selectable(*on, text).min_size(egui::vec2(width, row)));
            button.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, *on, name));
            if button.on_hover_text(name).clicked() {
                *on = !*on;
            }
        }
    }

    fn status_ui(&self, ui: &mut Ui, width: f32, row: f32) {
        let status = self.status();
        let failed = matches!(self.pattern, Some(Err(_))) || status == "No matches";
        let color = if failed { ui.visuals().warn_fg_color } else { ui.visuals().weak_text_color() };
        ui.allocate_ui_with_layout(egui::vec2(width, row), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(width);
            ui.add(egui::Label::new(RichText::new(&status).color(color)).truncate()).on_hover_text(&status);
        });
    }
}

fn button(ui: &mut Ui, text: &str, name: &str, hint: &str, row: f32, width: f32) -> bool {
    let button = ui.add(egui::Button::new(text).min_size(egui::vec2(width, row)));
    button.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
    button.on_hover_text(hint).clicked()
}

fn navigation(ui: &mut Ui, row: f32, widths: [f32; 2], action: &mut BarAction) {
    if button(ui, "⏶", "Previous match", "Previous match, further up (Enter)", row, widths[0]) {
        *action = BarAction::Step(Direction::Older);
    }
    if button(ui, "⏷", "Next match", "Next match, further down (Shift+Enter)", row, widths[1]) {
        *action = BarAction::Step(Direction::Newer);
    }
}

fn close_button(ui: &mut Ui, row: f32, width: f32, action: &mut BarAction) {
    if button(ui, "×", "Close search", "Close (Escape)", row, width) {
        *action = BarAction::Close;
    }
}

/// What the user did in the bar this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarAction {
    None,
    Step(Direction),
    Close,
}

/// Scroll so `found` is on screen and clear of the bar, near the middle,
/// unless it's already comfortably in view.
fn reveal(emu: &mut Emulator, found: Found, lines: usize) {
    let lines = lines.min(emu.lines()).max(1) as i32;
    let offset = emu.display_offset() as i32;
    let row = found.start.line.0 + offset;
    let top = (BAR_ROWS as i32).min(lines - 1);
    if row >= top && found.end.line.0 + offset < lines - top {
        return;
    }
    let target = lines / 2 - found.start.line.0;
    emu.scroll_to(target.max(0) as usize);
}

/// Colors for matches: the background of every match, and the background
/// and text of the current one. Monochrome themes stay in their two colors.
pub fn colors(theme: &Theme) -> (Color32, Color32, Color32) {
    if theme.effects.monochrome {
        (mix(theme.bg, theme.fg, 0.3), theme.fg, theme.bg)
    } else {
        (
            mix(theme.bg, Color32::from_rgb(0xf2, 0xb8, 0x3a), 0.45),
            Color32::from_rgb(0xff, 0x96, 0x32),
            Color32::from_rgb(0x1a, 0x1a, 0x1a),
        )
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(channel(a.r(), b.r()), channel(a.g(), b.g()), channel(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_text_prefill_escapes_metacharacters_and_tab_gaps_in_regex_mode() {
        let mut emu = Emulator::new(20, 3, 10, std::sync::Arc::new(|_| {}));
        emu.feed(b"a+\tb");
        let emulator = Mutex::new(emu);
        let mut search = SearchBar { regex: true, ..Default::default() };
        search.open(Some("a+\tb".into()));
        search.work(&egui::Context::default(), &emulator, 3);
        assert_eq!(search.status(), "1 of 1");
        assert!(search.current().is_some());
    }
}
