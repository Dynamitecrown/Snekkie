//! The Invade tab: draws a run in fixed-width ASCII, turns keys into game
//! input at a fixed step, and shows the pause, build, upgrade and game over
//! screens. Silent by design.
//!
//! Gameplay keys are taken out of egui's event queue only while the arena
//! has the keyboard, so nothing here can reach a terminal or a connection.

use std::collections::VecDeque;
use std::path::PathBuf;

use egui::{
    Align2, Color32, Event, FontId, Frame, Id, Key, Margin, Modifiers, Pos2, Rect, RichText, Sense, Stroke, Ui,
    UiBuilder, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::invade::content::{ARENA_H, ARENA_W, ASPECT, WORLD_H, WORLD_W};
use crate::invade::rng::mix;
use crate::invade::run::{
    Curtain, EffectKind, HostileKind, RESUME_COUNTDOWN, Role, SCAN_WIDTH, STEP, ShotKind, Summary,
};
use crate::invade::upgrades::{Card, Stats, weapon_details};
use crate::invade::{
    EncounterKind, EndReason, Event as GameEvent, Input, InvadeRun, Loadout, Phase, ProgressStore, Rig, StarterModule,
    Unlock, WeaponKind,
};
use crate::settings::Theme;

mod sprites;

/// Below this app viewport the game pauses and asks for a bigger window.
pub const MIN_VIEWPORT: Vec2 = vec2(640.0, 400.0);
/// The arena plus its border, and the HUD and footer rows around it.
const COLUMNS: f32 = ARENA_W + 2.0;
const ROWS: f32 = ARENA_H + 7.0;
const ARENA_TOP_ROW: f32 = 4.0;
const DETAIL: f32 = 4.0;
/// At most this much game time is caught up after a slow frame.
const MAX_CATCH_UP: u32 = 12;
const NOTICE_SECONDS: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    Pause,
    Build,
    ConfirmRestart,
}

#[derive(Debug, Clone, Copy, Default)]
struct Held {
    left_arrow: bool,
    a: bool,
    right_arrow: bool,
    d: bool,
    up_arrow: bool,
    w: bool,
    down_arrow: bool,
    s: bool,
    space: bool,
}

impl Held {
    fn left(self) -> bool {
        self.left_arrow || self.a
    }
    fn right(self) -> bool {
        self.right_arrow || self.d
    }
    fn up(self) -> bool {
        self.up_arrow || self.w
    }
    fn down(self) -> bool {
        self.down_arrow || self.s
    }
    fn keys(self) -> Vec<Key> {
        [
            (self.left_arrow, Key::ArrowLeft),
            (self.a, Key::A),
            (self.right_arrow, Key::ArrowRight),
            (self.d, Key::D),
            (self.up_arrow, Key::ArrowUp),
            (self.w, Key::W),
            (self.down_arrow, Key::ArrowDown),
            (self.s, Key::S),
            (self.space, Key::Space),
        ]
        .into_iter()
        .filter_map(|(down, key)| down.then_some(key))
        .collect()
    }
}

/// What the app tells the game about this frame.
pub struct InvadeOptions {
    pub theme: Theme,
    /// False while an app menu, dialog or other window is in front.
    pub interactive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvadeAction {
    Close,
}

/// A summary for the status bar and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvadeStatus {
    pub rig: Rig,
    pub wave: u32,
    pub completed_wave: u32,
    pub score: u64,
    pub integrity: u8,
    pub paused: bool,
    pub state: &'static str,
}

pub struct InvadeTab {
    store: ProgressStore,
    run: InvadeRun,
    base_devices: u64,
    next_rig: Rig,
    loadout: Loadout,
    selecting_loadout: bool,
    has_started_run: bool,
    setup_armed: bool,
    choice_visible: bool,
    overlay: Option<Overlay>,
    held: Held,
    special: bool,
    mobility: bool,
    /// Shift must be released in focused play before it can trigger.
    shift_armed: bool,
    clock: Option<f64>,
    accumulator: f32,
    request_focus: bool,
    /// The arena has had the keyboard since play last started, so losing
    /// it means something else took it.
    armed: bool,
    /// Keys are only read when the last frame could read them too, so a
    /// key that closed a menu or dialog never reaches the game.
    ready: bool,
    focus_overlay: bool,
    /// Focusable controls from the last overlay frame. Shortcuts belong
    /// to these controls, never to an exposed sidebar editor.
    overlay_controls: Vec<Id>,
    seed: u64,
    pending_notices: Vec<String>,
    notices: VecDeque<(String, f64)>,
    summary: Option<Summary>,
    best_before: Option<(u32, u64)>,
    overlay_height: f32,
}

impl InvadeTab {
    /// Open the progress store and choose a loadout before starting.
    pub fn open(path: PathBuf, seed: u64) -> InvadeTab {
        InvadeTab::with_store(ProgressStore::open(path), seed)
    }

    /// Choose a loadout with a store already in memory: one kept from an
    /// earlier Invade tab, with anything that could not be saved yet.
    pub fn with_store(store: ProgressStore, seed: u64) -> InvadeTab {
        let rig = store.progress.selected_rig();
        let mut tab = InvadeTab {
            run: InvadeRun::new(seed, rig, [], 0),
            store,
            base_devices: 0,
            next_rig: rig,
            loadout: Loadout { rig, ..Loadout::default() },
            selecting_loadout: true,
            has_started_run: false,
            setup_armed: false,
            choice_visible: false,
            overlay: None,
            held: Held::default(),
            special: false,
            mobility: false,
            shift_armed: false,
            clock: None,
            accumulator: 0.0,
            request_focus: false,
            armed: false,
            ready: false,
            focus_overlay: true,
            overlay_controls: Vec::new(),
            seed,
            pending_notices: Vec::new(),
            notices: VecDeque::new(),
            summary: None,
            best_before: None,
            overlay_height: 0.0,
        };
        if let Some(warning) = tab.store.warning() {
            tab.pending_notices.push(warning);
        }
        tab
    }

    pub fn progress_path(&self) -> &std::path::Path {
        &self.store.path
    }

    /// The store, kept by the app after the tab closes.
    pub fn into_store(self) -> ProgressStore {
        self.store
    }

    /// Why the latest save failed, if it did. The game shows this itself
    /// while it is open; the app reports it when the tab closes.
    pub fn save_warning(&self) -> Option<String> {
        self.store.save_error.as_ref().and(self.store.warning())
    }

    /// Retry progress already kept in memory without finishing or recording
    /// the run again, including a completed run on the Game Over screen.
    pub fn retry_failed_save(&mut self) {
        if self.store.save_error.is_some() {
            self.save();
        }
    }

    /// Still playable: closing it records the run.
    pub fn unfinished(&self) -> bool {
        self.has_started_run && self.run.is_unfinished()
    }

    pub fn is_selecting_loadout(&self) -> bool {
        self.selecting_loadout
    }

    /// Movement and fire keys held right now, so the app can keep them
    /// from reaching a terminal after a tab switch.
    pub fn held_keys(&self) -> Vec<Key> {
        self.held.keys()
    }

    pub fn status(&self) -> InvadeStatus {
        if self.selecting_loadout {
            return InvadeStatus {
                rig: self.loadout.rig,
                wave: 0,
                completed_wave: 0,
                score: 0,
                integrity: 5,
                paused: true,
                state: "Choose loadout",
            };
        }
        let paused = self.overlay.is_some();
        let state = match self.run.phase() {
            Phase::Over => "Game over",
            Phase::Upgrade => "Choosing an upgrade",
            _ if paused => "Paused",
            _ if self.run.countdown() > 0.0 => "Get ready",
            _ => "Playing",
        };
        InvadeStatus {
            rig: self.run.rig(),
            wave: self.run.wave(),
            completed_wave: self.run.completed_wave(),
            score: self.run.score(),
            integrity: self.run.integrity(),
            paused,
            state,
        }
    }

    #[doc(hidden)]
    pub fn run(&self) -> &InvadeRun {
        &self.run
    }

    #[doc(hidden)]
    pub fn run_mut(&mut self) -> &mut InvadeRun {
        &mut self.run
    }

    /// Messages showing over the arena now, newest last.
    #[doc(hidden)]
    pub fn notices(&self) -> Vec<String> {
        self.notices.iter().map(|(text, _)| text.clone()).chain(self.pending_notices.iter().cloned()).collect()
    }

    #[doc(hidden)]
    pub fn progress(&self) -> &crate::invade::Progress {
        &self.store.progress
    }

    /// Something else needs the player: stop the clock and let go of every
    /// key. Returning takes an explicit Resume.
    pub fn pause(&mut self) {
        self.held = Held::default();
        self.special = false;
        self.mobility = false;
        self.shift_armed = false;
        self.ready = false;
        if self.has_started_run && self.run.is_advancing() && self.overlay.is_none() {
            self.overlay = Some(Overlay::Pause);
            self.focus_overlay = true;
        }
    }

    /// Pause and show this run's upgrades.
    pub fn show_build(&mut self) {
        self.pause();
        if self.overlay.is_some() {
            self.overlay = Some(Overlay::Build);
            self.focus_overlay = true;
        }
    }

    /// Playing, or counting down to play.
    pub fn is_running(&self) -> bool {
        self.running()
    }

    pub fn resume(&mut self) {
        if self.overlay.take().is_some() {
            self.run.begin_countdown(RESUME_COUNTDOWN);
        }
        self.held = Held::default();
        self.mobility = false;
        self.shift_armed = false;
        self.request_focus = true;
        self.armed = false;
    }

    /// End and record the run, once.
    pub fn finish(&mut self, reason: EndReason) {
        if !self.has_started_run {
            return;
        }
        self.process_events();
        if let Some(summary) = self.run.finish(reason) {
            self.best_before = self.store.progress.best.get(summary.rig.id()).map(|b| (b.wave, b.score));
            self.store.progress.devices_compromised = self.base_devices + summary.compromised;
            self.store.progress.add_record(&summary);
            self.save();
            self.summary = Some(summary);
            self.focus_overlay = true;
        }
    }

    fn show_loadout(&mut self, seed: u64) {
        self.seed = seed;
        self.loadout.rig = self.next_rig;
        self.selecting_loadout = true;
        self.has_started_run = false;
        self.setup_armed = false;
        self.choice_visible = false;
        self.overlay = None;
        self.held = Held::default();
        self.special = false;
        self.mobility = false;
        self.shift_armed = false;
        self.accumulator = 0.0;
        self.request_focus = false;
        self.focus_overlay = true;
        self.armed = false;
        self.ready = false;
    }

    fn start_run(&mut self) {
        self.base_devices = self.store.progress.devices_compromised;
        let unlocks: Vec<Unlock> = self.store.progress.unlocks().collect();
        self.run = InvadeRun::new_with_loadout(self.seed, self.loadout, unlocks, self.base_devices);
        self.selecting_loadout = false;
        self.has_started_run = true;
        self.choice_visible = false;
        self.overlay = None;
        self.held = Held::default();
        self.special = false;
        self.mobility = false;
        self.shift_armed = false;
        self.accumulator = 0.0;
        self.request_focus = true;
        self.armed = false;
        self.ready = false;
        self.summary = None;
    }

    fn restart(&mut self) {
        self.finish(EndReason::Restarted);
        self.show_loadout(mix(self.seed.wrapping_add(1)));
    }

    fn select_rig(&mut self, rig: Rig) {
        if !self.store.progress.rig_available(rig) || self.next_rig == rig {
            return;
        }
        self.next_rig = rig;
        self.loadout.rig = rig;
        self.store.progress.selected_rig = rig.id().into();
        self.save();
    }

    fn save(&mut self) {
        if self.store.writable()
            && self.store.save().is_err()
            && let Some(warning) = self.store.warning()
        {
            self.pending_notices.push(warning);
        }
    }

    fn process_events(&mut self) {
        for event in self.run.take_events() {
            match event {
                GameEvent::Unlocked(unlock) => {
                    self.store.progress.unlocked.insert(unlock.id().into());
                    self.pending_notices.push(format!("UNLOCKED  {}", unlock.name()));
                    self.save();
                }
                GameEvent::Completed(_) => {
                    if self.run.phase() == Phase::Upgrade {
                        self.held = Held::default();
                        self.special = false;
                        self.mobility = false;
                        self.shift_armed = false;
                        self.focus_overlay = true;
                    }
                    self.store.progress.devices_compromised = self.base_devices + self.run.compromised();
                    self.save();
                }
                GameEvent::Defeated => self.finish(EndReason::Defeated),
                _ => {}
            }
        }
    }

    fn running(&self) -> bool {
        self.has_started_run && !self.selecting_loadout && self.overlay.is_none() && self.run.is_advancing()
    }

    // -- the frame -------------------------------------------------------

    pub fn ui(&mut self, ui: &mut Ui, options: &InvadeOptions) -> Option<InvadeAction> {
        let now = ui.input(|i| i.time);
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        let id = Id::new("invade_arena");
        let response = ui.interact(rect, id, Sense::click());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, "Invade arena"));
        if response.clicked() && self.running() {
            response.request_focus();
        }

        let viewport = ui.ctx().content_rect().size();
        let too_small = viewport.x < MIN_VIEWPORT.x || viewport.y < MIN_VIEWPORT.y;
        let (window_focused, minimized) =
            ui.input(|i| (i.viewport().focused.unwrap_or(true), i.viewport().minimized.unwrap_or(false)));
        if self.running() && self.request_focus && options.interactive && !too_small {
            self.request_focus = false;
            response.request_focus();
        }
        let has_focus = response.has_focus();
        if has_focus && self.running() {
            self.armed = true;
            // Arrows, Tab and Escape stay with the game instead of moving
            // keyboard focus.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                )
            });
        }
        // A click somewhere else (a background tab's close button, Stop
        // paste) leaves the keyboard with nobody: take it back. Anything that
        // actually takes the keyboard pauses the game.
        let mut lost_focus = false;
        if self.armed && !has_focus && !self.request_focus && self.running() {
            let elsewhere = ui.memory(|m| m.focused()).is_some_and(|focused| focused != id);
            if elsewhere {
                lost_focus = true;
            } else if !ui.input(|i| i.pointer.any_down()) {
                response.request_focus();
            }
        }
        if self.running() && (!options.interactive || !window_focused || minimized || too_small || lost_focus) {
            self.pause();
        }

        // Read gameplay keys only on a frame that follows another one where
        // the arena had them.
        let can_read = options.interactive && window_focused && !minimized && !too_small;
        if self.selecting_loadout && !self.setup_armed && can_read {
            self.setup_armed = ui.input(|i| {
                !i.keys_down.contains(&Key::Space)
                    && !i.keys_down.contains(&Key::Enter)
                    && !i
                        .events
                        .iter()
                        .any(|event| matches!(event, Event::Key { key: Key::Space | Key::Enter, pressed: true, .. }))
            });
        }
        let mut action = None;
        if can_read && self.ready {
            action = self.read_keys(ui, has_focus);
            if self.running() && has_focus {
                self.mobility |= ui.input(|i| shift_input(&mut self.shift_armed, i.modifiers, &i.events));
            } else {
                self.shift_armed = false;
            }
        } else if !can_read {
            self.held = Held::default();
            self.mobility = false;
            self.shift_armed = false;
        }
        self.ready = can_read;

        // The fixed-step clock.
        let dt = self.clock.map_or(0.0, |last| (now - last).max(0.0) as f32);
        self.clock = Some(now);
        if self.running() {
            self.accumulator = (self.accumulator + dt).min(STEP * MAX_CATCH_UP as f32);
            let mut stepped = false;
            while self.accumulator >= STEP && self.run.is_advancing() {
                self.accumulator -= STEP;
                let input = Input {
                    turn_left: self.held.left(),
                    turn_right: self.held.right(),
                    thrust: self.held.up(),
                    brake: self.held.down(),
                    fire: self.held.space,
                    special: self.special && !stepped,
                    mobility: self.mobility && !stepped,
                    aim: None,
                };
                self.run.step(input);
                stepped = true;
                self.process_events();
            }
            if stepped {
                self.special = false;
                self.mobility = false;
            }
        } else {
            self.accumulator = 0.0;
            self.special = false;
            self.mobility = false;
        }

        // A defeat or unlock may have happened outside a step.
        self.process_events();
        let choice_visible = self.has_started_run && self.run.phase() == Phase::Upgrade;
        if choice_visible && !self.choice_visible {
            self.held = Held::default();
            self.special = false;
            self.mobility = false;
            self.shift_armed = false;
            self.focus_overlay = true;
        }
        self.choice_visible = choice_visible;
        for text in self.pending_notices.drain(..) {
            self.notices.push_back((text, now + NOTICE_SECONDS));
        }
        while self.notices.len() > 3 {
            self.notices.pop_front();
        }
        self.notices.retain(|(_, until)| *until > now);

        let palette = Palette::new(options.theme);
        let layout = Layout::fit(ui, rect);
        ui.painter().rect_filled(rect, 0.0, palette.bg);
        self.draw(ui, &layout, &palette, now);
        if self.has_started_run && self.overlay.is_none() && !too_small && self.run.is_advancing() {
            self.hud_buttons(ui, &layout, &palette);
        }
        let overlay_action = self.overlays(ui, rect, &layout, &palette, too_small, options.interactive);
        action = action.or(overlay_action);

        if self.running() {
            ui.ctx().request_repaint();
        } else if let Some((_, until)) = self.notices.front() {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((until - now).max(0.0)));
        }
        action
    }

    /// `[Pause]` and `[Build]` at the right of the HUD, drawn as text.
    fn hud_buttons(&mut self, ui: &mut Ui, layout: &Layout, palette: &Palette) {
        for (row, label, overlay, hint) in [
            (0.0, "[Pause]", Overlay::Pause, "Pause (Esc or P)"),
            (1.0, "[Build]", Overlay::Build, "Pause and show the build (B)"),
        ] {
            let width = label.len() as f32 * layout.cell.x;
            let rect = Rect::from_min_size(layout.cell(COLUMNS - 8.0, row), vec2(width, layout.cell.y));
            let button =
                egui::Button::new(RichText::new(label).font(layout.font.clone()).color(palette.fg)).frame(false);
            let clicked = ui
                .scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                    ui.spacing_mut().button_padding = Vec2::ZERO;
                    ui.add_sized(rect.size(), button).on_hover_text(hint).clicked()
                })
                .inner;
            if clicked {
                self.pause();
                if self.overlay.is_some() {
                    self.overlay = Some(overlay);
                }
            }
        }
    }

    /// Take the game's keys out of the queue. Returns an action if a key
    /// asked for one.
    fn read_keys(&mut self, ui: &mut Ui, arena_focused: bool) -> Option<InvadeAction> {
        let phase = self.run.phase();
        let overlay = self.overlay;
        let game_focused = arena_focused
            || ui.memory(|memory| {
                memory.focused().map_or_else(
                    // egui releases a button's focus before delivering Esc.
                    || self.overlay_controls.iter().any(|&id| memory.had_focus_last_frame(id)),
                    |id| self.overlay_controls.contains(&id),
                )
            });
        let mut pause = false;
        let mut build = false;
        let mut resume = false;
        let mut back = false;
        let mut choice = None;
        let mut held = self.held;
        let mut special = self.special;
        ui.input_mut(|input| {
            input.events.retain(|event| match event {
                Event::Key { key, pressed, repeat, modifiers, .. }
                    if (!*pressed || modifiers.is_none() || *modifiers == Modifiers::SHIFT) && is_game_key(*key) =>
                {
                    let playing = self.has_started_run
                        && !self.selecting_loadout
                        && overlay.is_none()
                        && matches!(phase, Phase::Combat | Phase::Intermission { .. });
                    // Held fire must never activate a newly focused menu
                    // control. A fresh Space press still works like Enter.
                    if game_focused && !playing && *key == Key::Space && *pressed && *repeat {
                        return false;
                    }
                    if !self.selecting_loadout && game_focused && overlay.is_none() && phase == Phase::Upgrade {
                        if *pressed && !*repeat {
                            match key {
                                Key::Num1 => choice = Some(0),
                                Key::Num2 => choice = Some(1),
                                Key::Num3 => choice = Some(2),
                                _ => {}
                            }
                        }
                        // The cards are ordinary buttons: let Space and
                        // Enter reach a focused one.
                        return !matches!(key, Key::Num1 | Key::Num2 | Key::Num3);
                    }
                    if playing && arena_focused {
                        let down = *pressed;
                        match key {
                            Key::ArrowLeft => held.left_arrow = down,
                            Key::A => held.a = down,
                            Key::ArrowRight => held.right_arrow = down,
                            Key::D => held.d = down,
                            Key::ArrowUp => held.up_arrow = down,
                            Key::W => held.w = down,
                            Key::ArrowDown => held.down_arrow = down,
                            Key::S => held.s = down,
                            Key::Space => held.space = down,
                            Key::X if down && !*repeat => special = true,
                            Key::Escape | Key::P if down && !*repeat => pause = true,
                            Key::B if down && !*repeat => build = true,
                            _ => {}
                        }
                        // A repeat can't start a hold that began elsewhere.
                        if *repeat && down {
                            match key {
                                Key::ArrowLeft => held.left_arrow = self.held.left_arrow,
                                Key::A => held.a = self.held.a,
                                Key::ArrowRight => held.right_arrow = self.held.right_arrow,
                                Key::D => held.d = self.held.d,
                                Key::ArrowUp => held.up_arrow = self.held.up_arrow,
                                Key::W => held.w = self.held.w,
                                Key::ArrowDown => held.down_arrow = self.held.down_arrow,
                                Key::S => held.s = self.held.s,
                                Key::Space => held.space = self.held.space,
                                _ => {}
                            }
                        }
                        return false;
                    }
                    if !*pressed {
                        // Letting go always counts, wherever the keyboard is.
                        match key {
                            Key::ArrowLeft => held.left_arrow = false,
                            Key::A => held.a = false,
                            Key::ArrowRight => held.right_arrow = false,
                            Key::D => held.d = false,
                            Key::ArrowUp => held.up_arrow = false,
                            Key::W => held.w = false,
                            Key::ArrowDown => held.down_arrow = false,
                            Key::S => held.s = false,
                            Key::Space => held.space = false,
                            _ => {}
                        }
                        return true;
                    }
                    if game_focused && !*repeat {
                        match (overlay, key) {
                            (Some(Overlay::Pause), Key::Escape | Key::P) => resume = true,
                            (Some(Overlay::Pause), Key::B) => build = true,
                            (Some(Overlay::Build), Key::Escape | Key::B | Key::P) => back = true,
                            (Some(Overlay::ConfirmRestart), Key::Escape) => back = true,
                            _ => return true,
                        }
                        return false;
                    }
                    true
                }
                // Typed text from those keys belongs to the game too.
                Event::Text(text) if arena_focused && overlay.is_none() && is_game_text(text) => false,
                _ => true,
            });
            // Whatever happened to the events, a key that is up is not held.
            let down = |key| input.keys_down.contains(&key);
            held.left_arrow &= down(Key::ArrowLeft);
            held.a &= down(Key::A);
            held.right_arrow &= down(Key::ArrowRight);
            held.d &= down(Key::D);
            held.up_arrow &= down(Key::ArrowUp);
            held.w &= down(Key::W);
            held.down_arrow &= down(Key::ArrowDown);
            held.s &= down(Key::S);
            held.space &= down(Key::Space);
        });
        self.held = held;
        self.special = special;
        if pause {
            self.pause();
        }
        if build {
            self.pause();
            if self.overlay.is_some() {
                self.overlay = Some(Overlay::Build);
            }
        }
        if back {
            self.overlay = Some(Overlay::Pause);
            self.focus_overlay = true;
        }
        if resume {
            self.resume();
        }
        if let Some(choice) = choice
            && self.run.choose(choice)
        {
            self.held = Held::default();
            self.request_focus = true;
            self.armed = false;
        }
        None
    }

    // -- drawing ---------------------------------------------------------

    fn draw(&self, ui: &Ui, layout: &Layout, palette: &Palette, now: f64) {
        let painter = ui.painter_at(layout.grid);
        let font = layout.font.clone();
        let text = |col: f32, row: f32, s: &str, color: Color32| {
            painter.text(layout.cell(col, row), Align2::LEFT_TOP, s, font.clone(), color);
        };
        let run = &self.run;
        let stats = run.stats();

        if self.selecting_loadout {
            text(0.0, 0.0, "INVADE", palette.fg);
            text(0.0, 1.0, "Prepare your next intrusion", palette.dim);
            return;
        }

        // HUD.
        let integrity: String = (0..5).map(|i| if i < run.integrity() { '#' } else { '-' }).collect::<String>();
        let hud = format!(
            "INVADE   WAVE {:02}   SCORE {:06}   INTEGRITY [{integrity}] {}/5{}",
            run.wave(),
            run.score(),
            run.integrity(),
            if run.player().shield_ready { "  SHIELD" } else { "" }
        );
        text(0.0, 0.0, &hud, palette.fg);
        let special = special_meter(run, stats);
        let mobility = mobility_meter(run);
        let tag = if self.store.progress.is_unlocked(Unlock::RootAccess) { "   root@invade" } else { "" };
        text(0.0, 1.0, &format!("{:<8} {special}  {mobility}{tag}", run.rig().name().to_uppercase()), palette.fg);
        let needed = run.experience_needed().max(1);
        let filled = ((run.experience() as f32 / needed as f32).clamp(0.0, 1.0) * 16.0).floor() as usize;
        text(
            0.0,
            2.0,
            &format!(
                "LEVEL {:02}  XP [{}{}] {}/{}  {}",
                run.level(),
                "#".repeat(filled),
                "-".repeat(16 - filled),
                run.experience(),
                needed,
                weapon_summary(run)
            ),
            palette.experience,
        );

        // The world has four small ASCII glyphs per logical cell in each
        // direction; the HUD and controls keep their readable font.
        let rule = format!("+{}+", "-".repeat((ARENA_W * DETAIL) as usize));
        layout.detail_text(&painter, -0.25, -0.25, &rule, palette.dim);
        layout.detail_text(&painter, -0.25, ARENA_H, &rule, palette.dim);
        for row in 0..(ARENA_H * DETAIL) as usize {
            layout.detail_text(&painter, -0.25, row as f32 / DETAIL, "|", palette.dim);
            layout.detail_text(&painter, ARENA_W, row as f32 / DETAIL, "|", palette.dim);
        }
        for (row, footer) in control_footer(run.rig()).lines().enumerate() {
            text(0.0, ARENA_TOP_ROW + ARENA_H + 1.0 + row as f32, footer, palette.dim);
        }

        let arena = ui.painter_at(layout.arena);
        let camera = run.camera();
        let put = |x: f32, y: f32, s: &str, color: Color32| {
            layout.detail_text(&arena, x - camera.0, y - camera.1, s, color);
        };
        let world_label = |x: f32, y: f32, s: &str, color: Color32| {
            arena.text(layout.at(x - camera.0, y - camera.1), Align2::LEFT_TOP, s, font.clone(), color);
        };
        let hud = |x: f32, y: f32, s: &str, color: Color32| {
            arena.text(layout.at(x, y), Align2::LEFT_TOP, s, font.clone(), color);
        };
        let paused = self.overlay.is_some() || matches!(run.phase(), Phase::Upgrade | Phase::Over);

        // Only visit the visible world tiles. The grid is anchored in the
        // network, so its drift makes the ship-following camera legible.
        arena.rect_filled(layout.arena, 0.0, mix_colors(palette.bg, palette.fg, 0.035));
        let world =
            Rect::from_min_max(layout.at(-camera.0, -camera.1), layout.at(WORLD_W - camera.0, WORLD_H - camera.1));
        arena.rect_filled(world.intersect(layout.arena), 0.0, palette.bg);
        let grid_color = mix_colors(palette.bg, palette.fg, 0.18);
        let first_x = (camera.0 / 8.0).floor().max(0.0) as i32;
        let last_x = ((camera.0 + ARENA_W).min(WORLD_W) / 8.0).ceil() as i32;
        let first_y = (camera.1 / 4.0).floor().max(0.0) as i32;
        let last_y = ((camera.1 + ARENA_H).min(WORLD_H) / 4.0).ceil() as i32;
        for y in first_y..=last_y {
            for x in first_x..=last_x {
                let node = if x % 4 == 0 && y % 4 == 0 { "+--+\n|::|\n+--+" } else { "." };
                put(x as f32 * 8.0, y as f32 * 4.0, node, grid_color);
            }
        }
        for edge in [0.0, WORLD_W] {
            if edge >= camera.0 && edge <= camera.0 + ARENA_W {
                for row in 0..(ARENA_H * DETAIL) as usize {
                    put(edge, camera.1 + row as f32 / DETAIL, "#", palette.dim);
                }
            }
        }
        for edge in [0.0, WORLD_H] {
            if edge >= camera.1 && edge <= camera.1 + ARENA_H {
                let left = camera.0.max(0.0);
                let right = (camera.0 + ARENA_W).min(WORLD_W);
                put(left, edge, &"#".repeat(((right - left).max(0.0) * DETAIL) as usize), palette.dim);
            }
        }
        // Rocks are solid circles in visual space. Keep their rasterized
        // edge at that same radius; only their crater texture tumbles.
        for rock in run.asteroids().iter().filter(|rock| {
            rock.x + rock.radius >= camera.0
                && rock.x - rock.radius <= camera.0 + ARENA_W
                && rock.y + rock.radius / ASPECT >= camera.1
                && rock.y - rock.radius / ASPECT <= camera.1 + ARENA_H
        }) {
            put(
                rock.x - rock.radius,
                rock.y - rock.radius / ASPECT,
                &sprites::asteroid(rock.radius, rock.spin).join("\n"),
                palette.fg,
            );
        }
        for orb in run.orbs().iter().filter(|orb| {
            orb.x >= camera.0 - 1.0
                && orb.x <= camera.0 + ARENA_W + 1.0
                && orb.y >= camera.1 - 1.0
                && orb.y <= camera.1 + ARENA_H + 1.0
        }) {
            put(orb.x - 0.25, orb.y - 0.125, "<>", palette.experience);
        }

        // Warnings first, so everything else draws over them.
        for scan in run.scans() {
            let left = scan.x - SCAN_WIDTH / 2.0;
            let top = scan.top.max(camera.1);
            let bottom = scan.bottom.min(camera.1 + ARENA_H);
            let rows = ((top * DETAIL).floor() as i32)..((bottom * DETAIL).ceil() as i32);
            if scan.warn > 0.0 {
                for row in rows.step_by(4) {
                    put(left, row as f32 / DETAIL, ":          :", palette.warning);
                }
            } else {
                for row in rows {
                    put(left, row as f32 / DETAIL, "||||||||||||", palette.danger);
                }
            }
        }
        for mark in run.marks() {
            if mark.is_shot() {
                put(mark.x - 0.125, mark.y - 0.125, "+", palette.warning);
            } else {
                put(mark.x - 2.5, mark.y - 0.125, ">        !!        <", palette.warning);
            }
        }
        for curtain in run.curtains() {
            draw_curtain(&put, &world_label, curtain, palette);
        }

        // Devices, minibosses and bosses.
        for enemy in run.visible_enemies() {
            // Devices flash when hit; minibosses and bosses show it in their
            // health bar instead.
            let device = matches!(enemy.role, Role::Device(_) | Role::Guard);
            let color = if enemy.flash > 0.0 && device {
                palette.warning
            } else if enemy.telegraphing && device {
                palette.danger
            } else {
                palette.fg
            };
            if enemy.telegraphing {
                // Above the ship when there is room, otherwise just below it.
                let row = if enemy.y - camera.1 >= 1.0 { enemy.y - 1.0 } else { enemy.y + enemy.h };
                put(enemy.x + enemy.w / 2.0 - 0.125, row, "!", palette.danger);
            }
            match enemy.role {
                Role::Device(kind) => {
                    put(enemy.x, enemy.y, &sprites::device(kind, enemy.damaged()).join("\n"), color);
                }
                Role::Guard => put(enemy.x, enemy.y, &sprites::guard().join("\n"), color),
                Role::Miniboss(kind) => put(enemy.x, enemy.y, &sprites::detail(kind.sprite()).join("\n"), color),
                Role::Boss(kind) => put(enemy.x, enemy.y, &sprites::detail(kind.sprite()).join("\n"), color),
            }
            if device {
                draw_flight_markers(
                    &put,
                    enemy.center(),
                    (enemy.w, enemy.h),
                    enemy.nose(),
                    enemy.thrusting,
                    false,
                    color,
                );
            }
        }
        // Linked pairs show their link.
        let linked: Vec<_> = run
            .enemies()
            .filter(|e| {
                matches!(
                    e.role,
                    Role::Miniboss(crate::invade::MinibossKind::RelayPair)
                        | Role::Boss(crate::invade::BossKind::LoadBalancer)
                )
            })
            .collect();
        if let [a, b] = linked[..] {
            let (left, right) = if a.x < b.x { (a, b) } else { (b, a) };
            let from = left.x + left.w;
            let length = ((right.x - from).max(0.0) * DETAIL) as usize;
            let link: String = (0..length).map(|i| if i % 2 == 0 { '-' } else { ' ' }).collect();
            put(from, left.y + 1.0, &link, palette.dim);
        }

        // Packets.
        for shot in run.shots() {
            let glyph = match shot.kind {
                ShotKind::Primary | ShotKind::Fork => direction_glyph(shot.vx, shot.vy * ASPECT),
                ShotKind::Fragment => "~",
                ShotKind::Pivot => "@",
            };
            put(shot.x - 0.125, shot.y - 0.125, glyph, palette.friendly);
        }
        for orbital in run.orbitals() {
            put(orbital.x - 0.125, orbital.y - 0.125, "O", palette.friendly);
        }
        for hostile in run.hostiles() {
            let glyph = hostile.kind.glyph().to_string();
            let color = if hostile.kind == HostileKind::Aimed { palette.warning } else { palette.danger };
            put(hostile.x - 0.125, hostile.y - 0.125, &glyph, color);
        }

        // Effects, kept small, and only while things move.
        let effects = if matches!(run.phase(), Phase::Upgrade | Phase::Over) { &[][..] } else { run.effects() };
        for effect in effects {
            let age = 1.0 - effect.ttl / effect.life;
            match effect.kind {
                EffectKind::Burst => {
                    let glyph = if age < 0.35 {
                        "\\|/"
                    } else if age < 0.7 {
                        "-*-"
                    } else {
                        " . "
                    };
                    put(effect.x - 0.375, effect.y - 0.125, glyph, palette.warning);
                }
                EffectKind::Spark => put(effect.x - 0.125, effect.y - 0.125, "'", palette.friendly),
                EffectKind::Chain => {
                    let (dx, dy) = (effect.to.0 - effect.x, effect.to.1 - effect.y);
                    let steps = ((dx.abs()).max(dy.abs() * ASPECT) / 0.75).ceil().max(1.0) as usize;
                    for i in 0..=steps {
                        let t = i as f32 / steps as f32;
                        put(
                            effect.x + dx * t - 0.125,
                            effect.y + dy * t - 0.125,
                            if i == steps { "*" } else { "." },
                            palette.friendly,
                        );
                    }
                }
                EffectKind::Absorb => put(effect.x - 0.875, effect.y - 0.125, "(     )", palette.friendly),
                EffectKind::Damage => put(effect.x - 0.625, effect.y - 0.125, "## ##", palette.danger),
                EffectKind::Pulse => {
                    let color = palette.friendly.gamma_multiply(1.0 - age * 0.75);
                    for point in 0..72 {
                        let angle = point as f32 / 72.0 * std::f32::consts::TAU;
                        put(
                            effect.x + angle.cos() * effect.radius - 0.125,
                            effect.y + angle.sin() * effect.radius / ASPECT - 0.125,
                            ":",
                            color,
                        );
                    }
                }
            }
        }

        // The player's ship. Invulnerability blinks it, unless paused.
        let player = run.player();
        let width = run.rig().width();
        let blink = player.invulnerable > 0.0 && !paused && (now * 10.0) as i64 % 2 == 0;
        if !blink {
            put(
                player.x - width / 2.0,
                player.y,
                &sprites::flight_rig(run.rig(), player.heading).join("\n"),
                palette.friendly,
            );
            draw_flight_markers(
                &put,
                (player.x, player.y + 0.5),
                (width, 1.0),
                player.nose(),
                player.thrusting,
                player.braking,
                palette.friendly,
            );
        }
        if player.protection > 0.0 {
            let shield = if player.barrier {
                format!(".{}.", "=".repeat((width * DETAIL) as usize))
            } else {
                format!(":{}:", " ".repeat((width * DETAIL) as usize))
            };
            put(player.x - width / 2.0 - 0.25, player.y - 0.25, &shield, palette.friendly);
        }

        // Miniboss or boss health.
        if let Some((name, health)) = run.boss_health() {
            let filled = (health * 20.0).ceil() as usize;
            let bar = format!(
                "{} [{}{}] {:>3}%",
                name.to_uppercase(),
                "#".repeat(filled),
                "-".repeat(20 - filled),
                (health * 100.0).ceil() as u32
            );
            hud(ARENA_W / 2.0 - bar.len() as f32 / 2.0, 0.0, &bar, palette.danger);
        }

        // Keep scan order visible below the full-size encounter health
        // band, even when recorded columns overlap an encounter's artwork.
        let label_band = if run.boss_health().is_some() { 1.25 } else { 0.0 };
        for scan in run.scans().iter().filter(|scan| scan.warn > 0.0) {
            if scan.x < camera.0 || scan.x > camera.0 + ARENA_W {
                continue;
            }
            let label = scan.label.map_or_else(|| "SCAN".to_string(), |n| format!("[{n}]"));
            let row = (scan.top - camera.1).max(label_band) + f32::from(scan.label.unwrap_or(1).saturating_sub(1));
            let x = (scan.x - camera.0 - label.len() as f32 / 2.0).clamp(0.0, ARENA_W - label.len() as f32);
            arena.rect_filled(
                Rect::from_min_size(layout.at(x, row), vec2(label.len() as f32 * layout.cell.x, layout.cell.y)),
                0.0,
                palette.bg,
            );
            hud(x, row, &label, palette.warning);
        }

        // Banners and countdowns.
        if run.countdown() > 0.0 && self.overlay.is_none() {
            let count = run.countdown().ceil() as u32;
            let big = layout.font_scaled(2.5);
            arena.text(layout.at(ARENA_W / 2.0, 11.0), Align2::CENTER_CENTER, count.to_string(), big, palette.fg);
            let ready = if run.elapsed() == 0.0 { "GET READY" } else { "RESUMING" };
            hud(ARENA_W / 2.0 - ready.len() as f32 / 2.0, 14.0, ready, palette.fg);
        } else if let Some(banner) = run.banner()
            && !paused
        {
            hud(ARENA_W / 2.0 - banner.chars().count() as f32 / 2.0, 13.0, banner, palette.fg);
        }
        if let Phase::Intermission { .. } = run.phase()
            && !paused
        {
            let next = match run.next_encounter() {
                EncounterKind::Ordinary => format!("NEXT  WAVE {:02}", run.wave() + 1),
                kind => format!("NEXT  {}", kind.label().to_uppercase()),
            };
            hud(ARENA_W / 2.0 - next.len() as f32 / 2.0, 15.0, &next, palette.dim);
        }
        for (i, (notice, _)) in self.notices.iter().enumerate() {
            let shown: String = notice.chars().take(ARENA_W as usize - 4).collect();
            hud(ARENA_W - shown.chars().count() as f32 - 1.0, 1.0 + i as f32, &shown, palette.warning);
        }
    }

    // -- screens over the arena -----------------------------------------

    fn overlays(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        layout: &Layout,
        palette: &Palette,
        too_small: bool,
        interactive: bool,
    ) -> Option<InvadeAction> {
        self.overlay_controls.clear();
        let phase = self.run.phase();
        let screen = if too_small {
            Screen::TooSmall
        } else if self.selecting_loadout {
            Screen::Loadout
        } else if phase == Phase::Over {
            Screen::GameOver
        } else if let Some(overlay) = self.overlay {
            Screen::Overlay(overlay)
        } else if phase == Phase::Upgrade {
            Screen::Upgrade
        } else {
            return None;
        };
        // After a menu, a dialog, another tab or a click on the dimmed arena
        // nothing has the keyboard; give it back so Esc, P, B and 1-3 work.
        if interactive && ui.memory(|m| m.focused().is_none()) {
            self.focus_overlay = true;
        }
        // Dim the arena underneath.
        ui.painter().rect_filled(layout.arena, 0.0, palette.bg.gamma_multiply(0.82));
        // Centered over the game, sized to its contents (as measured last
        // frame) and scrolling when the window is short.
        let width = (area.width() - 24.0).clamp(160.0, 620.0);
        let height = (area.height() - 16.0).max(80.0);
        let shown = self.overlay_height.clamp(0.0, height);
        let top = (area.center().y - shown / 2.0).max(area.top() + 8.0);
        let panel = Rect::from_min_size(pos2(area.center().x - width / 2.0, top), vec2(width, height));
        let mut action = None;
        let mut child = ui.new_child(UiBuilder::new().max_rect(panel).id_salt("invade_overlay"));
        let overlay_visuals = palette.overlay_visuals(child.visuals());
        *child.visuals_mut() = overlay_visuals;
        if !interactive {
            child.disable();
        }
        let frame = Frame::new()
            .fill(palette.panel)
            .stroke(Stroke::new(1.0, palette.dim))
            .inner_margin(Margin::same(12))
            .show(&mut child, |ui| {
                ui.set_max_height(height - 24.0);
                egui::ScrollArea::vertical().id_salt("invade_overlay_scroll").auto_shrink([false, true]).show(
                    ui,
                    |ui| {
                        ui.set_width(ui.available_width());
                        action = match screen {
                            Screen::TooSmall => self.too_small_ui(ui),
                            Screen::Loadout => self.loadout_ui(ui, palette),
                            Screen::GameOver => self.game_over_ui(ui, palette),
                            Screen::Upgrade => self.upgrade_ui(ui, palette),
                            Screen::Overlay(Overlay::Pause) => self.pause_ui(ui, palette),
                            Screen::Overlay(Overlay::Build) => self.build_ui(ui),
                            Screen::Overlay(Overlay::ConfirmRestart) => self.confirm_ui(ui),
                        };
                    },
                );
            });
        if frame.response.rect.height() != self.overlay_height {
            self.overlay_height = frame.response.rect.height();
            ui.ctx().request_repaint();
        }
        action
    }

    fn heading(ui: &mut Ui, text: &str) {
        ui.label(RichText::new(text).monospace().strong().size(18.0));
        ui.add_space(4.0);
    }

    fn focus_first(&mut self, response: &egui::Response) {
        self.overlay_controls.push(response.id);
        if std::mem::take(&mut self.focus_overlay) {
            response.request_focus();
        }
    }

    fn too_small_ui(&mut self, ui: &mut Ui) -> Option<InvadeAction> {
        Self::heading(ui, "INVADE PAUSED");
        ui.label(format!(
            "Enlarge the window to at least {} × {} to play Invade.",
            MIN_VIEWPORT.x as u32, MIN_VIEWPORT.y as u32
        ));
        ui.add_space(6.0);
        let close = ui.button("Close Invade").on_hover_text("End and record this run");
        self.focus_first(&close);
        if close.clicked() {
            return Some(InvadeAction::Close);
        }
        None
    }

    fn loadout_ui(&mut self, ui: &mut Ui, palette: &Palette) -> Option<InvadeAction> {
        Self::heading(ui, "CHOOSE LOADOUT");
        ui.label("Choose a rig, starting weapon and module. Your run begins when you start.");
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            let start = ui.add_enabled(self.setup_armed, egui::Button::new("Start run"));
            self.focus_first(&start);
            if start.clicked() {
                self.start_run();
            }
            let close = ui.button("Close Invade");
            self.overlay_controls.push(close.id);
            if close.clicked() {
                action = Some(InvadeAction::Close);
            }
        });
        self.warning_ui(ui, palette);
        ui.separator();
        ui.label(RichText::new("Rig").strong());
        ui.horizontal_wrapped(|ui| {
            for rig in Rig::ALL {
                let available = self.store.progress.rig_available(rig);
                let label = if available { rig.name().to_string() } else { format!("{} (locked)", rig.name()) };
                let radio = ui.add_enabled(available, egui::RadioButton::new(self.next_rig == rig, label));
                self.overlay_controls.push(radio.id);
                let radio = radio.on_hover_text(if available {
                    rig.summary()
                } else {
                    rig.unlock().map_or("", Unlock::milestone)
                });
                if radio.clicked() {
                    self.select_rig(rig);
                }
            }
        });
        ui.label(self.loadout.rig.summary());
        ui.separator();
        ui.label(RichText::new("Starting weapon").strong());
        ui.horizontal_wrapped(|ui| {
            for weapon in WeaponKind::STARTERS {
                let radio = ui.radio(self.loadout.weapon == weapon, weapon.name());
                self.overlay_controls.push(radio.id);
                if radio.clicked() {
                    self.loadout.weapon = weapon;
                }
            }
        });
        ui.label(self.loadout.weapon.summary());
        ui.separator();
        ui.label(RichText::new("Starter module").strong());
        ui.horizontal_wrapped(|ui| {
            for module in StarterModule::ALL {
                let radio = ui.radio(self.loadout.module == module, module.name());
                self.overlay_controls.push(radio.id);
                if radio.clicked() {
                    self.loadout.module = module;
                }
            }
        });
        ui.label(self.loadout.module.summary());
        action
    }

    fn pause_ui(&mut self, ui: &mut Ui, palette: &Palette) -> Option<InvadeAction> {
        Self::heading(ui, "PAUSED");
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            let resume = ui.button("Resume").on_hover_text("Continue after a one-second countdown (Esc or P)");
            self.focus_first(&resume);
            if resume.clicked() {
                self.resume();
            }
            let build = ui.button("Build").on_hover_text("Inspect this run's upgrades (B)");
            self.overlay_controls.push(build.id);
            if build.clicked() {
                self.overlay = Some(Overlay::Build);
                self.focus_overlay = true;
            }
            let restart = ui.button("Restart run…");
            self.overlay_controls.push(restart.id);
            if restart.clicked() {
                self.overlay = Some(Overlay::ConfirmRestart);
                self.focus_overlay = true;
            }
            let close = ui.button("Close Invade").on_hover_text("End and record this run");
            self.overlay_controls.push(close.id);
            if close.clicked() {
                action = Some(InvadeAction::Close);
            }
        });
        self.warning_ui(ui, palette);
        ui.separator();
        self.rig_ui(ui, "Rig for the next run");
        ui.separator();
        Self::controls_ui(ui, self.run.rig());
        ui.separator();
        self.unlocks_ui(ui, palette);
        action
    }

    fn warning_ui(&self, ui: &mut Ui, palette: &Palette) {
        if let Some(warning) = self.store.warning() {
            ui.add(egui::Label::new(RichText::new(warning).color(palette.warning)).wrap());
        }
    }

    fn rig_ui(&mut self, ui: &mut Ui, title: &str) {
        ui.label(RichText::new(title).strong());
        for rig in Rig::ALL {
            let available = self.store.progress.rig_available(rig);
            let label = if available {
                format!("{} — {}", rig.name(), rig.summary())
            } else {
                format!("{} — locked: {}", rig.name(), rig.unlock().map_or("", Unlock::milestone))
            };
            let radio = ui.add_enabled(available, egui::RadioButton::new(self.next_rig == rig, label));
            self.overlay_controls.push(radio.id);
            if radio.clicked() {
                self.select_rig(rig);
            }
        }
    }

    fn controls_ui(ui: &mut Ui, rig: Rig) {
        ui.label(RichText::new("Controls").strong());
        egui::Grid::new("invade_controls").num_columns(2).spacing(vec2(12.0, 2.0)).show(ui, |ui| {
            for (keys, what) in [
                ("A/D or Left/Right", "Turn the ship's nose".to_string()),
                ("W or Up", "Thrust along the nose; release to coast".to_string()),
                ("S or Down", "Brake without reversing (overrides thrust)".to_string()),
                ("Hold Space", "Fire at the nearest visible device".to_string()),
                ("Shift", format!("{} along the nose", rig.mobility_name())),
                ("X", format!("{} (special)", rig.special_name())),
                ("Esc or P", "Pause".to_string()),
                ("B", "Pause and show the build".to_string()),
                ("1, 2, 3", "Choose an offered upgrade".to_string()),
            ] {
                ui.label(RichText::new(keys).monospace());
                ui.label(what);
                ui.end_row();
            }
        });
    }

    fn unlocks_ui(&self, ui: &mut Ui, palette: &Palette) {
        let progress = &self.store.progress;
        ui.label(RichText::new("Unlocks").strong());
        for unlock in Unlock::ALL {
            let done = progress.is_unlocked(unlock);
            let detail = if unlock == Unlock::Fragment && !done {
                format!(
                    "{} ({}/{})",
                    unlock.milestone(),
                    (self.base_devices + self.run.compromised()).min(crate::invade::content::FRAGMENT_DEVICES),
                    crate::invade::content::FRAGMENT_DEVICES
                )
            } else {
                unlock.milestone().to_string()
            };
            ui.horizontal(|ui| {
                let color = if done { palette.panel_text } else { palette.dim_text };
                ui.label(RichText::new(if done { "[x]" } else { "[ ]" }).monospace().color(color));
                ui.label(RichText::new(format!("{} — {detail}", unlock.name())).color(color));
            });
        }
        if let Some(best) = progress.best.get(self.next_rig.id()) {
            ui.label(format!("Best with {}: wave {}, score {}", self.next_rig.name(), best.wave, best.score));
        }
    }

    fn build_ui(&mut self, ui: &mut Ui) -> Option<InvadeAction> {
        Self::heading(ui, "BUILD");
        let run = &self.run;
        let s = run.stats();
        let mut back = false;
        ui.horizontal(|ui| {
            let button = ui.button("Back");
            self.overlay_controls.push(button.id);
            if std::mem::take(&mut self.focus_overlay) {
                button.request_focus();
            }
            back = button.clicked();
        });
        let run = &self.run;
        ui.label(format!("{} · {}", run.rig().name(), run.rig().summary()));
        ui.label(RichText::new("Weapons").strong());
        for weapon in run.weapons() {
            ui.label(format!(
                "{} — rank {} of {}: {}",
                weapon.kind.name(),
                weapon.rank,
                weapon.kind.cap(),
                weapon.kind.summary()
            ));
            let spec = weapon.spec(run.rig(), s);
            ui.label(RichText::new(weapon_details(weapon.kind, &spec)).monospace());
        }
        ui.separator();
        egui::Grid::new("invade_stats").num_columns(2).spacing(vec2(12.0, 2.0)).show(ui, |ui| {
            for (name, value) in stat_lines(run.rig(), s) {
                ui.label(name);
                ui.label(RichText::new(value).monospace());
                ui.end_row();
            }
        });
        ui.label(format!("Movement ability: {}", mobility_meter(run)));
        ui.label(format!("Current flight speed: {:.1} units/s", run.player().speed()));
        ui.separator();
        if run.build().is_empty() {
            ui.label("No modules yet. Collect experience to choose weapons and modules.");
        }
        for (id, rank) in run.build().acquired() {
            ui.label(format!("{} — rank {rank} of {}: {}", id.name(), id.cap(), id.rule()));
        }
        ui.separator();
        Self::controls_ui(ui, run.rig());
        if back {
            self.overlay = Some(Overlay::Pause);
            self.focus_overlay = true;
        }
        None
    }

    fn confirm_ui(&mut self, ui: &mut Ui) -> Option<InvadeAction> {
        Self::heading(ui, "RESTART RUN?");
        ui.add(
            egui::Label::new("End this run and choose a new loadout? The current run is recorded with its score.")
                .wrap(),
        );
        ui.horizontal(|ui| {
            let restart = ui.button("Restart");
            self.focus_first(&restart);
            if restart.clicked() {
                self.restart();
            }
            let keep = ui.button("Keep playing");
            self.overlay_controls.push(keep.id);
            if keep.clicked() {
                self.overlay = Some(Overlay::Pause);
                self.focus_overlay = true;
            }
        });
        None
    }

    fn upgrade_ui(&mut self, ui: &mut Ui, palette: &Palette) -> Option<InvadeAction> {
        let run = &self.run;
        Self::heading(ui, &format!("LEVEL {:02} — CHOOSE ONE", run.level()));
        ui.label(RichText::new("Choose a weapon or module for this run.").color(palette.dim_text));
        ui.add_space(4.0);
        let cards = run.cards();
        let mut chosen = None;
        for (i, card) in cards.iter().enumerate() {
            let response =
                ui.add(egui::Button::new(card_text(i, card, palette)).wrap().min_size(vec2(ui.available_width(), 0.0)));
            self.overlay_controls.push(response.id);
            if i == 0 && std::mem::take(&mut self.focus_overlay) {
                response.request_focus();
            }
            if response.clicked() {
                chosen = Some(i);
            }
        }
        if cards.len() < 3 {
            ui.label(RichText::new("Fewer choices: other upgrades are capped or unavailable.").color(palette.dim_text));
        }
        ui.add_space(4.0);
        ui.label(RichText::new(format!("Weapons: {}", weapon_summary(&self.run))).color(palette.dim_text));
        let build: Vec<String> =
            self.run.build().acquired().map(|(id, rank)| format!("{} {rank}", id.name())).collect();
        ui.label(
            RichText::new(if build.is_empty() {
                "Build: none yet".into()
            } else {
                format!("Build: {}", build.join(", "))
            })
            .color(palette.dim_text),
        );
        if let Some(i) = chosen
            && self.run.choose(i)
        {
            self.held = Held::default();
            self.request_focus = true;
            self.armed = false;
        }
        None
    }

    fn game_over_ui(&mut self, ui: &mut Ui, palette: &Palette) -> Option<InvadeAction> {
        let mut action = None;
        let summary = self.summary.clone().unwrap_or_else(|| Summary {
            rig: self.run.rig(),
            score: self.run.score(),
            completed_wave: self.run.completed_wave(),
            duration: self.run.elapsed(),
            seed: self.run.seed(),
            reason: self.run.ended().unwrap_or(EndReason::Defeated),
            compromised: self.run.compromised(),
            unlocks: self.run.earned_unlocks().to_vec(),
        });
        Self::heading(ui, &format!("RUN OVER — {}", summary.reason.label().to_uppercase()));
        egui::Grid::new("invade_results").num_columns(2).spacing(vec2(12.0, 2.0)).show(ui, |ui| {
            let minutes = summary.duration / 60.0;
            for (name, value) in [
                ("Score", summary.score.to_string()),
                ("Waves completed", summary.completed_wave.to_string()),
                ("Rig", summary.rig.name().to_string()),
                ("Time played", format!("{}:{:02}", minutes.floor() as u64, (summary.duration % 60.0).floor() as u64)),
                ("Devices compromised", summary.compromised.to_string()),
            ] {
                ui.label(name);
                ui.label(RichText::new(value).monospace());
                ui.end_row();
            }
        });
        if let Some((wave, score)) = self.best_before
            && (summary.score > score || summary.completed_wave > wave)
        {
            ui.label(RichText::new(format!("New best for {}!", summary.rig.name())).color(palette.friendly_text));
        }
        if !summary.unlocks.is_empty() {
            ui.label(RichText::new("Unlocked this run").strong());
            for unlock in &summary.unlocks {
                ui.label(format!("  {}", unlock.name()));
            }
        }
        self.warning_ui(ui, palette);
        ui.horizontal(|ui| {
            let retry = ui.button(format!("Retry with {}", self.next_rig.name()));
            self.overlay_controls.push(retry.id);
            if std::mem::take(&mut self.focus_overlay) || self.request_focus {
                self.request_focus = false;
                retry.request_focus();
            }
            if retry.clicked() {
                self.show_loadout(mix(self.seed.wrapping_add(1)));
            }
            let close = ui.button("Close Invade");
            self.overlay_controls.push(close.id);
            if close.clicked() {
                action = Some(InvadeAction::Close);
            }
        });
        ui.separator();
        self.rig_ui(ui, "Rig");
        action
    }
}

#[derive(Debug, Clone, Copy)]
enum Screen {
    TooSmall,
    Loadout,
    GameOver,
    Upgrade,
    Overlay(Overlay),
}

fn is_game_key(key: Key) -> bool {
    matches!(
        key,
        Key::ArrowLeft
            | Key::ArrowRight
            | Key::ArrowUp
            | Key::ArrowDown
            | Key::A
            | Key::D
            | Key::W
            | Key::S
            | Key::Space
            | Key::X
            | Key::Escape
            | Key::P
            | Key::B
            | Key::Num1
            | Key::Num2
            | Key::Num3
    )
}

fn is_game_text(text: &str) -> bool {
    matches!(
        text,
        " " | "a" | "A" | "d" | "D" | "w" | "W" | "s" | "S" | "x" | "X" | "p" | "P" | "b" | "B" | "1" | "2" | "3"
    )
}

fn shift_press(armed: &mut bool, down: bool) -> bool {
    if !down {
        *armed = true;
        false
    } else {
        std::mem::take(armed)
    }
}

/// Preserve a short Shift tap even if it is released before this frame
/// ends. Shortcut chords disarm the gate until a clean release follows.
fn shift_input(armed: &mut bool, final_modifiers: Modifiers, events: &[Event]) -> bool {
    let blocked = |m: Modifiers| m.ctrl || m.alt || m.mac_cmd || m.command;
    let mut trigger = false;
    let mut last_modifiers = None;
    let mut apply = |modifiers: Modifiers| {
        if blocked(modifiers) {
            *armed = false;
            trigger = false;
        } else {
            trigger |= shift_press(armed, modifiers.shift);
        }
    };
    for event in events {
        match event {
            Event::ModifiersChanged(modifiers) => {
                last_modifiers = Some(*modifiers);
                apply(*modifiers);
            }
            Event::Key { modifiers, .. } if blocked(*modifiers) => apply(*modifiers),
            _ => {}
        }
    }
    // Some integrations report only the final state. Do not reapply it
    // after a matching event, which could turn a chord into a fresh press.
    if last_modifiers != Some(final_modifiers) {
        apply(final_modifiers);
    }
    trigger
}

fn direction_glyph(dx: f32, dy: f32) -> &'static str {
    if dx.abs() > dy.abs() * 2.0 {
        if dx < 0.0 { "<" } else { ">" }
    } else if dy.abs() > dx.abs() * 2.0 {
        if dy < 0.0 { "^" } else { "v" }
    } else if dx * dy < 0.0 {
        "/"
    } else {
        "\\"
    }
}

fn control_footer(rig: Rig) -> String {
    format!(
        "[A/D or Left/Right] Turn [W/Up] Thrust [S/Down] Brake - release to coast\n\
         [Space] Auto-fire [Shift] {} [X] {} [Esc/P] Pause [B] Build",
        rig.mobility_name(),
        rig.special_name()
    )
}

/// Place the flight indicators outside the unchanged axis-aligned body.
/// Heading and turret aim are independent: these markers always use nose().
fn flight_marker_distance(size: (f32, f32), nose: (f32, f32)) -> f32 {
    let horizontal = if nose.0.abs() > f32::EPSILON { size.0 / 2.0 / nose.0.abs() } else { f32::INFINITY };
    let vertical = if nose.1.abs() > f32::EPSILON { size.1 * ASPECT / 2.0 / nose.1.abs() } else { f32::INFINITY };
    horizontal.min(vertical)
}

fn draw_flight_markers(
    put: &impl Fn(f32, f32, &str, Color32),
    center: (f32, f32),
    size: (f32, f32),
    nose: (f32, f32),
    thrusting: bool,
    braking: bool,
    color: Color32,
) {
    let edge = flight_marker_distance(size, nose);
    put(
        center.0 + nose.0 * (edge + 0.5) - 0.125,
        center.1 + nose.1 * (edge + 0.5) / ASPECT - 0.125,
        direction_glyph(nose.0, nose.1),
        color,
    );
    if thrusting {
        for i in 0..3 {
            let rear = edge + 0.5 + i as f32 * 0.4;
            put(center.0 - nose.0 * rear - 0.125, center.1 - nose.1 * rear / ASPECT - 0.125, ":", color);
        }
    } else if braking {
        let rear = edge + 0.5;
        put(center.0 - nose.0 * rear - 0.125, center.1 - nose.1 * rear / ASPECT - 0.125, "=", color);
    }
}

fn mobility_meter(run: &InvadeRun) -> String {
    let player = run.player();
    let name = run.rig().mobility_name().to_uppercase();
    let state = if player.mobility_active > 0.0 {
        "ACTIVE".to_string()
    } else if player.mobility_cooldown > 0.0 {
        format!("{:.1}s", player.mobility_cooldown)
    } else {
        "READY".to_string()
    };
    format!("{name}:{state}")
}

fn weapon_summary(run: &InvadeRun) -> String {
    run.weapons()
        .iter()
        .map(|weapon| {
            let glyph = match weapon.kind {
                WeaponKind::Packet => "PKT",
                WeaponKind::Scatter => "SCT",
                WeaponKind::Rail => "RAIL",
                WeaponKind::Arc => "ARC",
                WeaponKind::Orbit => "ORB",
                WeaponKind::Pulse => "PLS",
            };
            format!("{glyph}{}", weapon.rank)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn special_meter(run: &InvadeRun, stats: &Stats) -> String {
    let player = run.player();
    let name = run.rig().special_name().to_uppercase();
    let filled = if player.charges >= stats.max_charges {
        10
    } else {
        ((player.recharge / stats.recharge) * 10.0).floor().clamp(0.0, 10.0) as usize
    };
    let bar: String = (0..10).map(|i| if i < filled { '#' } else { '-' }).collect();
    let state = if player.charges > 0 { "READY" } else { "CHARGING" };
    let charges =
        if stats.max_charges > 1 { format!(" x{}/{}", player.charges, stats.max_charges) } else { String::new() };
    format!("{name:<8} [{bar}] {state}{charges}")
}

fn stat_lines(rig: Rig, s: &Stats) -> Vec<(&'static str, String)> {
    let mut lines = vec![
        ("Damage per packet", format!("{:.2}", s.damage)),
        ("Fire rate", format!("{:.1} packets/s", s.fire_rate)),
        ("Flight top speed", format!("{:.1} units/s", s.move_speed)),
        ("Turn rate", format!("{:.0} degrees/s", rig.flight().turn_rate.to_degrees())),
        ("Thrust spool", format!("{:.2} s to top speed", rig.flight().spool)),
        ("Aim", "Nearest visible device (automatic)".into()),
        ("Movement ability", format!("{} · {:.1} s recharge", rig.mobility_name(), rig.mobility_recharge())),
        ("Pierce", format!("{} extra target(s)", s.pierce)),
        ("Damage taken per hit", format!("{}", s.hit_cost)),
        ("Invulnerability after a hit", format!("{:.2} s", s.invulnerability)),
        ("Special recharge", format!("{:.1} s × {} charge(s)", s.recharge, s.max_charges)),
    ];
    lines.push(match rig {
        Rig::Probe => ("Pivot", format!("{:.0} direct, {} jumps of 3", s.pivot_direct, s.pivot_jumps)),
        Rig::Ghost => ("Slip protection", format!("{:.2} s", s.slip_protection)),
        Rig::Bastion => ("Barrier", format!("{:.1} s", s.barrier_protection)),
    });
    if let Some(fork) = s.fork_damage {
        lines.push(("Fork packets", format!("2 × {fork:.2}")));
    }
    if let Some(share) = s.fragment_share {
        lines.push(("Fragments", format!("2 × {:.0}% of the killing hit", share * 100.0)));
    }
    if let Some(refund) = s.feedback_refund {
        lines.push(("Feedback", format!("{refund:.2} s per hit, twice a second")));
    }
    if s.guidance {
        lines.push(("Guidance", "8-degree cone".into()));
    }
    lines
}

fn card_text(index: usize, card: &Card, palette: &Palette) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let plain = |color| egui::TextFormat { color, ..Default::default() };
    let bold =
        egui::TextFormat { font_id: FontId::proportional(15.0), color: palette.panel_text, ..Default::default() };
    job.append(&format!("[{}] {}", index + 1, card.title), 0.0, bold);
    job.append(&format!("   {}\n", card.rank), 0.0, plain(palette.dim_text));
    job.append(&card.effect, 0.0, plain(palette.panel_text));
    if let Some(downside) = &card.downside {
        job.append(&format!("\nDownside: {downside}"), 0.0, plain(palette.danger_text));
    }
    job
}

fn draw_curtain(
    put: &impl Fn(f32, f32, &str, Color32),
    label: &impl Fn(f32, f32, &str, Color32),
    curtain: &Curtain,
    palette: &Palette,
) {
    let gap_end = curtain.gap_left + curtain.gap_width;
    let row = |glyph: char| -> Vec<(f32, String)> {
        // Two runs: left of the gap and right of it.
        let left: String =
            std::iter::repeat_n(glyph, ((curtain.gap_left - curtain.left).max(0.0) * DETAIL) as usize).collect();
        let right: String =
            std::iter::repeat_n(glyph, ((curtain.right - gap_end).max(0.0) * DETAIL).ceil() as usize).collect();
        vec![(curtain.left, left), (gap_end, right)]
    };
    if curtain.warn > 0.0 {
        for (x, text) in row('.') {
            put(x, curtain.y, &text, palette.warning);
        }
        let safe = format!("[{}]", " ".repeat((curtain.gap_width * DETAIL - 2.0).max(0.0) as usize));
        let safe_y = curtain.y.clamp(curtain.top + 0.5, curtain.bottom - 1.0);
        put(curtain.gap_left, safe_y + 0.5, &safe, palette.friendly);
        label(curtain.gap_left + curtain.gap_width / 2.0 - 2.0, safe_y, "SAFE", palette.friendly);
    } else {
        for (x, text) in row(if curtain.speed < 0.0 { '^' } else { 'v' }) {
            for row in 0..DETAIL as usize {
                put(x, curtain.y - 0.5 + row as f32 / DETAIL, &text, palette.danger);
            }
        }
    }
}

/// Where everything goes: a grid of fixed-width cells, as large as fits.
struct Layout {
    grid: Rect,
    arena: Rect,
    cell: Vec2,
    font: FontId,
    detail_font: FontId,
}

impl Layout {
    fn fit(ui: &Ui, rect: Rect) -> Layout {
        let probe = FontId::monospace(16.0);
        let (glyph, row) = ui.fonts_mut(|f| (f.glyph_width(&probe, 'M'), f.row_height(&probe)));
        let size = (16.0 * rect.width() / COLUMNS / glyph).min(16.0 * rect.height() / ROWS / row).clamp(4.0, 28.0);
        let font = FontId::monospace(size);
        let detail_font = FontId::monospace(size / DETAIL);
        let (glyph, row) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let cell = vec2(glyph.max(1.0), row.max(1.0));
        let size = vec2(cell.x * COLUMNS, cell.y * ROWS);
        let min = (rect.center() - size / 2.0).round();
        let grid = Rect::from_min_size(min, size);
        let arena = Rect::from_min_size(
            pos2(min.x + cell.x, min.y + cell.y * ARENA_TOP_ROW),
            vec2(cell.x * ARENA_W, cell.y * ARENA_H),
        );
        Layout { grid, arena, cell, font, detail_font }
    }

    /// A grid cell, counting the border.
    fn cell(&self, column: f32, row: f32) -> Pos2 {
        self.grid.min + vec2(column * self.cell.x, row * self.cell.y)
    }

    /// An arena position.
    fn at(&self, x: f32, y: f32) -> Pos2 {
        self.arena.min + vec2(x * self.cell.x, y * self.cell.y)
    }

    /// Dense sprites occupy the same world geometry as before: four tiny
    /// characters per cell and exactly four tiny rows per world row.
    fn detail_text(&self, painter: &egui::Painter, x: f32, y: f32, text: &str, color: Color32) {
        for (row, line) in text.split('\n').enumerate() {
            painter.text(self.at(x, y + row as f32 / DETAIL), Align2::LEFT_TOP, line, self.detail_font.clone(), color);
        }
    }

    fn font_scaled(&self, factor: f32) -> FontId {
        FontId::monospace(self.font.size * factor)
    }
}

/// The selected theme's colors, with readable accents for danger,
/// warnings and the player's own ship and packets.
struct Palette {
    bg: Color32,
    fg: Color32,
    dim: Color32,
    friendly: Color32,
    experience: Color32,
    danger: Color32,
    warning: Color32,
    panel: Color32,
    panel_text: Color32,
    dim_text: Color32,
    danger_text: Color32,
    friendly_text: Color32,
}

fn luminance(c: Color32) -> f32 {
    let linear = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
}

pub(crate) fn contrast(a: Color32, b: Color32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn mix_colors(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

fn most_readable(candidates: &[Color32], bg: Color32) -> Color32 {
    candidates.iter().copied().max_by(|a, b| contrast(*a, bg).total_cmp(&contrast(*b, bg))).unwrap()
}

impl Palette {
    /// Keep the app's control geometry while adapting every overlay label
    /// and widget state to the game's panel, including light themes.
    fn overlay_visuals(&self, base: &egui::Visuals) -> egui::Visuals {
        let mut visuals = base.clone();
        visuals.dark_mode = luminance(self.panel) < 0.5;
        visuals.override_text_color = Some(self.panel_text);
        visuals.weak_text_color = Some(self.dim_text);
        visuals.disabled_alpha = 0.85;
        visuals.panel_fill = self.panel;
        visuals.window_fill = self.panel;
        visuals.extreme_bg_color = self.panel;
        visuals.faint_bg_color = self.panel;
        visuals.selection.bg_fill = mix_colors(self.panel, self.panel_text, 0.15);
        visuals.selection.stroke = Stroke::new(1.0, self.panel_text);
        for (widget, tint) in [
            (&mut visuals.widgets.noninteractive, 0.0),
            (&mut visuals.widgets.inactive, 0.04),
            (&mut visuals.widgets.hovered, 0.10),
            (&mut visuals.widgets.active, 0.15),
            (&mut visuals.widgets.open, 0.10),
        ] {
            let fill = mix_colors(self.panel, self.panel_text, tint);
            widget.bg_fill = fill;
            widget.weak_bg_fill = fill;
            widget.bg_stroke.color = self.dim_text;
            widget.fg_stroke.color = self.panel_text;
        }
        visuals.widgets.active.bg_stroke.color = self.friendly_text;
        visuals.widgets.open.bg_stroke.color = self.friendly_text;
        visuals
    }

    fn new(theme: Theme) -> Palette {
        let (bg, fg) = (theme.bg, theme.fg);
        let dim = mix_colors(fg, bg, 0.45);
        let mono = theme.effects.monochrome;
        let accent = |candidates: &[Color32]| {
            let best = most_readable(candidates, bg);
            if mono || contrast(best, bg) < 3.0 { fg } else { best }
        };
        let danger = accent(&[
            Color32::from_rgb(0xf0, 0x5a, 0x5a),
            Color32::from_rgb(0xff, 0x7a, 0x6e),
            Color32::from_rgb(0xc0, 0x26, 0x26),
            Color32::from_rgb(0x9e, 0x1a, 0x1a),
        ]);
        let warning = accent(&[
            Color32::from_rgb(0xf2, 0xc0, 0x4a),
            Color32::from_rgb(0xff, 0xd8, 0x6a),
            Color32::from_rgb(0x9a, 0x62, 0x00),
            Color32::from_rgb(0x7a, 0x4a, 0x00),
        ]);
        let friendly = if mono || contrast(theme.cursor, bg) < 3.0 { fg } else { theme.cursor };
        // Panels follow the theme too, a step off the background.
        let panel = mix_colors(bg, fg, 0.08);
        Palette {
            bg,
            fg,
            dim,
            friendly,
            experience: accent(&[Color32::from_rgb(0x49, 0xd8, 0xf0), Color32::from_rgb(0x00, 0x62, 0x78)]),
            danger,
            warning,
            panel,
            panel_text: fg,
            dim_text: mix_colors(fg, panel, 0.3),
            danger_text: danger,
            friendly_text: friendly,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_theme_gets_readable_accents() {
        let settings = crate::settings::AppSettings::default();
        for name in settings.theme_names() {
            let theme = settings.theme_named(&name);
            let p = Palette::new(theme);
            for (label, color) in [
                ("fg", p.fg),
                ("friendly", p.friendly),
                ("xp", p.experience),
                ("danger", p.danger),
                ("warning", p.warning),
            ] {
                assert!(contrast(color, p.bg) >= 3.0, "{name} {label}: {:.2}", contrast(color, p.bg));
            }
            assert!(contrast(p.panel_text, p.panel) >= 4.0, "{name} panel text");
            if theme.effects.monochrome {
                assert_eq!((p.danger, p.friendly), (p.fg, p.fg), "{name}: monochrome stays one color");
            }
        }
    }

    #[test]
    fn overlay_text_and_disabled_rigs_stay_readable_in_every_theme() {
        let settings = crate::settings::AppSettings::default();
        for name in settings.theme_names() {
            let palette = Palette::new(settings.theme_named(&name));
            let base = egui::Visuals::dark();
            let visuals = palette.overlay_visuals(&base);
            assert_eq!(visuals.override_text_color, Some(palette.panel_text), "{name}");
            assert_eq!(visuals.widgets.inactive.corner_radius, base.widgets.inactive.corner_radius);
            for widget in [
                visuals.widgets.noninteractive,
                visuals.widgets.inactive,
                visuals.widgets.hovered,
                visuals.widgets.active,
                visuals.widgets.open,
            ] {
                assert!(contrast(palette.panel_text, widget.bg_fill) >= 3.0, "{name}: widget text");
            }
            let disabled_text = mix_colors(palette.panel, palette.panel_text, visuals.disabled_alpha);
            assert!(contrast(disabled_text, palette.panel) >= 3.0, "{name}: disabled rig");
        }
    }

    #[test]
    fn held_keys_and_game_text_cover_every_control() {
        let held = Held { left_arrow: true, w: true, space: true, ..Held::default() };
        assert_eq!(held.keys(), [Key::ArrowLeft, Key::W, Key::Space]);
        assert!(held.left() && !held.right() && held.up() && !held.down());
        for key in [
            Key::A,
            Key::D,
            Key::W,
            Key::S,
            Key::ArrowUp,
            Key::ArrowDown,
            Key::X,
            Key::P,
            Key::B,
            Key::Num2,
            Key::Escape,
        ] {
            assert!(is_game_key(key));
        }
        assert!(!is_game_key(Key::Enter) && !is_game_key(Key::Tab));
        assert!(
            is_game_text(" ")
                && is_game_text("x")
                && is_game_text("W")
                && is_game_text("s")
                && !is_game_text("show run")
        );
    }

    #[test]
    fn hostile_packets_never_look_like_the_players_own() {
        let mut friendly: Vec<&str> = vec!["~", "@"];
        for step in 0..72 {
            let angle = (step as f32 * 5.0).to_radians();
            friendly.push(direction_glyph(angle.cos(), angle.sin()));
        }
        for kind in [HostileKind::Straight, HostileKind::Diagonal, HostileKind::Aimed, HostileKind::Stream] {
            let glyph = kind.glyph().to_string();
            assert!(!friendly.contains(&glyph.as_str()), "{kind:?} draws like a player packet: {glyph}");
        }
    }

    #[test]
    fn shift_requires_release_and_never_repeats() {
        let mut armed = false;
        assert!(!shift_press(&mut armed, true), "Shift already held on entry");
        assert!(!shift_press(&mut armed, false));
        assert!(shift_press(&mut armed, true));
        assert!(!shift_press(&mut armed, true));
        assert!(!shift_press(&mut armed, false));
        armed = false; // A pause discards an armed release.
        assert!(!shift_press(&mut armed, true));
    }

    #[test]
    fn a_same_frame_shift_tap_triggers_once_without_activating_shortcuts() {
        let tap = [Event::ModifiersChanged(Modifiers::SHIFT), Event::ModifiersChanged(Modifiers::NONE)];
        let mut armed = true;
        assert!(shift_input(&mut armed, Modifiers::NONE, &tap));
        assert!(!shift_input(&mut armed, Modifiers::NONE, &[]));

        armed = false;
        assert!(!shift_input(&mut armed, Modifiers::SHIFT, &[]), "held on entry is not a press");
        assert!(!shift_input(&mut armed, Modifiers::NONE, &[]));
        assert!(shift_input(&mut armed, Modifiers::SHIFT, &[]));
        assert!(!shift_input(&mut armed, Modifiers::SHIFT, &[]), "holding Shift never repeats");

        armed = true;
        let chord = Modifiers { ctrl: true, shift: true, ..Modifiers::NONE };
        let events = [
            Event::ModifiersChanged(Modifiers::SHIFT),
            Event::ModifiersChanged(chord),
            Event::ModifiersChanged(Modifiers::NONE),
        ];
        assert!(!shift_input(&mut armed, Modifiers::NONE, &events), "Ctrl+Shift is not mobility");
        assert!(shift_input(&mut armed, Modifiers::NONE, &tap), "a fresh tap after release still works");
    }

    #[test]
    fn directions_and_controls_fit_the_ascii_arena() {
        for (direction, expected) in [
            ((1.0, 0.0), ">"),
            ((-1.0, 0.0), "<"),
            ((0.0, -1.0), "^"),
            ((0.0, 1.0), "v"),
            ((1.0, -1.0), "/"),
            ((1.0, 1.0), "\\"),
        ] {
            assert_eq!(direction_glyph(direction.0, direction.1), expected);
        }
        for rig in Rig::ALL {
            let footer = control_footer(rig);
            assert!(footer.is_ascii() && footer.lines().all(|line| line.len() <= COLUMNS as usize), "{footer}");
            for control in ["Turn", "Thrust", "Brake", "coast", "Auto-fire", "[Esc/P] Pause", "[B] Build"] {
                assert!(footer.contains(control), "missing {control}: {footer}");
            }
        }
    }

    #[test]
    fn flight_markers_use_the_visual_body_edge_and_rear_thrust() {
        use std::cell::RefCell;
        for (nose, expected) in [
            ((0.0, -1.0), (40.0, 13.25, "^")),
            ((1.0, 0.0), (42.5, 14.0, ">")),
            ((0.0, 1.0), (40.0, 14.75, "v")),
            ((-1.0, 0.0), (37.5, 14.0, "<")),
        ] {
            let drawn = RefCell::new(Vec::new());
            draw_flight_markers(
                &|x, y, glyph, _| drawn.borrow_mut().push((x + 0.125, y + 0.125, glyph.to_string())),
                (40.0, 14.0),
                (4.0, 1.0),
                nose,
                true,
                false,
                Color32::WHITE,
            );
            let drawn = drawn.into_inner();
            assert_eq!(drawn[0], (expected.0, expected.1, expected.2.to_string()));
            assert_eq!(drawn.len(), 4);
            for (x, y, glyph) in &drawn[1..] {
                assert_eq!(glyph, ":");
                assert!((x - 40.0) * nose.0 + (y - 14.0) * ASPECT * nose.1 < 0.0, "flame must trail the nose");
            }
        }
    }
}
