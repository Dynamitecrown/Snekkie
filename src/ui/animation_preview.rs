//! The live preview on the Animations page of Preferences: a small terminal
//! acting out a few commands at a switch, drawn with the choices on screen
//! whether or not they've been saved yet.

use std::sync::Arc;
use std::time::Duration;

use egui::{Stroke, TextStyle, Ui, vec2};
use parking_lot::Mutex;

use super::style;
use super::terminal_view::{TerminalView, ViewOptions};
use crate::settings::{Animations, Theme};
use crate::terminal::emulator::Emulator;

/// Rows of terminal shown. The demo prints more than this, so it scrolls.
const ROWS: usize = 8;

/// Room above and below the text: the terminal view leaves this much above
/// it (and to its left), so the box leaves the same below.
const MARGIN: f32 = 4.0;

/// Seconds between keys as the demo types.
const KEY_INTERVAL: f64 = 0.11;

/// How long the finished demo stays up before it starts again.
const PAUSE: f64 = 1.5;

const PROMPT: &str = "Switch#";

const CLOCK: &str = "*10:42:07.512 UTC Wed Oct 1 2026";

/// `show ip interface brief`, squeezed to fit the dialog without wrapping.
const INTERFACES: [&str; 8] = [
    "Interface  IP-Address  OK? Method Status Protocol",
    "Vlan1      10.0.0.2    YES NVRAM  up     up",
    "Gi1/0/1    10.0.12.1   YES manual up     up",
    "Gi1/0/2    unassigned  YES unset  down   down",
    "Gi1/0/3    unassigned  YES unset  up     up",
    "Gi1/0/4    unassigned  YES unset  up     up",
    "Gi1/0/24   unassigned  YES unset  down   down",
    "Te1/1/1    172.16.0.1  YES NVRAM  up     up",
];

/// One moment of the demo.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    /// A key typed at the prompt, which the device echoes.
    Key(char),
    /// Enter, echoed as a new line.
    Enter,
    /// What the device answers with: these lines and then the prompt, all
    /// in one go, as a console sends them. A line at a time would let
    /// "Line by line" keep pace and look the same as "Instant".
    Reply(&'static [&'static str]),
    /// The prompt the demo starts at.
    Prompt,
    /// Clear the screen and go round again.
    Restart,
}

/// Puts the demo together, keeping time.
struct Script {
    steps: Vec<(f64, Step)>,
    time: f64,
}

impl Script {
    fn then(&mut self, wait: f64, step: Step) {
        self.time += wait;
        self.steps.push((self.time, step));
    }

    /// Type `command` after `wait`, then press Enter.
    fn command(&mut self, wait: f64, command: &str) {
        for (i, key) in command.chars().enumerate() {
            self.then(if i == 0 { wait } else { KEY_INTERVAL }, Step::Key(key));
        }
        self.then(3.0 * KEY_INTERVAL, Step::Enter);
    }
}

/// The demo as (seconds into it, step), in order. It ends by starting over.
fn script() -> Vec<(f64, Step)> {
    let mut script = Script { steps: Vec::new(), time: 0.0 };
    script.then(0.0, Step::Prompt);
    script.command(0.7, "show clock");
    script.then(0.2, Step::Reply(&[CLOCK]));
    script.command(0.9, "show ip interface brief");
    script.then(0.25, Step::Reply(&INTERFACES));
    script.then(PAUSE, Step::Restart);
    script.steps
}

/// `emulator` through `view`, in a box as wide as there's room for, in
/// `theme` with `animations`.
fn draw(ui: &mut Ui, view: &mut TerminalView, emulator: &Mutex<Emulator>, theme: Theme, animations: Animations) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let row = ui.fonts_mut(|f| f.row_height(&font)).ceil();
    let options = ViewOptions {
        theme,
        syntax: "none",
        highlighting_intensity: crate::terminal::highlight::DEFAULT_INTENSITY,
        regular: font.clone(),
        bold: font,
        keyboard: false,
        animations,
    };
    egui::Frame::new().stroke(Stroke::new(1.0, style::border(ui))).show(ui, |ui| {
        let size = vec2(ui.available_width(), ROWS as f32 * row + 2.0 * MARGIN);
        view.show_demo(ui, size, emulator, &options);
    });
}

pub struct AnimationPreview {
    emulator: Mutex<Emulator>,
    view: TerminalView,
    script: Vec<(f64, Step)>,
    /// When this time round started, and the next step to play.
    started: f64,
    next: usize,
    /// The egui pass it was last drawn on. Back after being hidden (another
    /// page showing), it starts from the top.
    drawn_pass: Option<u64>,
}

impl Default for AnimationPreview {
    fn default() -> Self {
        AnimationPreview {
            // Drawing sizes it to fit; the scrollback is for rows sliding in
            // from above while it scrolls.
            emulator: Mutex::new(Emulator::new(60, ROWS, 100, Arc::new(|_| {}))),
            view: TerminalView::default(),
            script: script(),
            started: 0.0,
            next: 0,
            drawn_pass: None,
        }
    }
}

impl AnimationPreview {
    /// Play the demo up to now and draw it, as wide as there's room for, in
    /// `theme` with `animations`. With animations off it plays plain.
    pub fn ui(&mut self, ui: &mut Ui, theme: Theme, animations: Animations) {
        let now = ui.input(|i| i.time);
        let pass = ui.ctx().cumulative_pass_nr();
        if self.drawn_pass.is_none_or(|last| pass != last.wrapping_add(1)) {
            self.restart(now);
        }
        self.drawn_pass = Some(pass);
        self.play(now, &animations);
        draw(ui, &mut self.view, &self.emulator, theme, animations);

        // Only while it's drawn. The view asks for its own frames while
        // anything moves; this is for the demo's next step.
        if let Some(&(at, _)) = self.script.get(self.next) {
            let wait = (self.started + at - now).max(0.0);
            ui.ctx().request_repaint_after(Duration::from_secs_f64(wait));
        }
    }

    fn restart(&mut self, now: f64) {
        self.emulator.lock().reset();
        self.view.reset_animations();
        self.started = now;
        self.next = 0;
    }

    /// Everything the script has due by `now`.
    fn play(&mut self, now: f64, animations: &Animations) {
        while let Some(&(at, step)) = self.script.get(self.next) {
            if self.started + at > now {
                break;
            }
            self.next += 1;
            match step {
                Step::Key(key) => {
                    // The key goes out before its echo comes back.
                    self.view.demo_keystroke(&self.emulator, 1, false, now, animations);
                    let mut utf8 = [0; 4];
                    self.emulator.lock().feed(key.encode_utf8(&mut utf8).as_bytes());
                }
                Step::Enter => {
                    self.view.demo_keystroke(&self.emulator, 0, true, now, animations);
                    self.emulator.lock().feed(b"\r\n");
                }
                Step::Reply(lines) => {
                    let mut reply = String::new();
                    for line in lines {
                        reply.push_str(line);
                        reply.push_str("\r\n");
                    }
                    reply.push_str(PROMPT);
                    self.emulator.lock().feed(reply.as_bytes());
                }
                Step::Prompt => self.emulator.lock().feed(PROMPT.as_bytes()),
                Step::Restart => self.restart(now),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use egui::{Pos2, Rect};

    use super::*;
    use crate::settings::{
        CursorBlink, CursorMotion, KeystrokeBurst, NewLines, NewText, Reveal, Scrolling, Shake, TypedText,
    };
    use crate::ui::preferences::WIDTH;

    /// A window as wide as the Preferences dialog, at `time`.
    fn input(time: f64) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(WIDTH, 400.0))),
            time: Some(time),
            predicted_dt: 0.0,
            ..Default::default()
        }
    }

    /// One frame at `time`, as wide as the Preferences dialog, with the
    /// preview drawn in it or not.
    fn frame(
        ctx: &egui::Context,
        preview: &mut AnimationPreview,
        time: f64,
        animations: Animations,
        shown: bool,
    ) -> egui::FullOutput {
        let mut out = ctx.run_ui(input(time), |ui| {
            if shown {
                preview.ui(ui, Theme::default(), animations);
            }
        });
        out.textures_delta.clear(); // there's no GPU to hand the font atlas to
        out
    }

    /// What a plain frame of the preview's screen looks like: drawn by a
    /// view that has never seen the screen change, with animations off.
    fn plain_frame(ctx: &egui::Context, preview: &AnimationPreview, time: f64) -> egui::FullOutput {
        let mut out = ctx.run_ui(input(time), |ui| {
            draw(ui, &mut TerminalView::default(), &preview.emulator, Theme::default(), Animations::default());
        });
        out.textures_delta.clear();
        out
    }

    /// The pieces of text a frame drew: what, where, how big, what colors.
    fn drawn_text(out: &egui::FullOutput) -> Vec<String> {
        out.shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => {
                    let colors: Vec<_> = text.galley.job.sections.iter().map(|s| s.format.color).collect();
                    Some(format!("{:?} at {:?}, {:?}, {colors:?}", text.galley.text(), text.pos, text.galley.rect))
                }
                _ => None,
            })
            .collect()
    }

    /// How many characters (other than blanks) a frame drew.
    fn glyphs(out: &egui::FullOutput) -> usize {
        out.shapes
            .iter()
            .map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => text.galley.text().chars().filter(|c| !c.is_whitespace()).count(),
                _ => 0,
            })
            .sum()
    }

    fn repaint_delay(out: &egui::FullOutput) -> Duration {
        out.viewport_output[&egui::ViewportId::ROOT].repaint_delay
    }

    fn screen(preview: &AnimationPreview) -> String {
        preview.emulator.lock().screen_text().trim_end().to_string()
    }

    /// Every different screen the demo shows in one time round and a bit,
    /// drawn 50 times a second.
    fn play_through(animations: Animations) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut preview = AnimationPreview::default();
        let length = preview.script.last().unwrap().0;
        let mut screens: Vec<String> = Vec::new();
        let mut time = 10.0;
        while time < 10.0 + length + 1.0 {
            frame(&ctx, &mut preview, time, animations, true);
            let text = screen(&preview);
            if screens.last() != Some(&text) {
                screens.push(text);
            }
            time += 0.02;
        }
        assert_eq!(preview.emulator.lock().lines(), ROWS);
        screens
    }

    #[test]
    fn the_demo_types_two_commands_and_starts_over() {
        let screens = play_through(Animations::default());
        assert_eq!(screens[0], PROMPT);
        // Typed a key at a time.
        assert!(screens.iter().any(|s| s == "Switch#s"), "{screens:#?}");
        assert!(screens.iter().any(|s| s == "Switch#sh"), "{screens:#?}");
        assert!(screens.iter().any(|s| s == &format!("Switch#show clock\n{CLOCK}\nSwitch#")), "{screens:#?}");

        // The table scrolls the first command off the top, and fits without
        // wrapping. It comes in one piece, as a console sends it, straight
        // after the command typed.
        let mut end: Vec<&str> = INTERFACES[1..].to_vec();
        end.push(PROMPT);
        let end = end.join("\n");
        let typed = format!("Switch#show clock\n{CLOCK}\nSwitch#show ip interface brief");
        let asked = screens.iter().position(|s| *s == typed).unwrap_or_else(|| panic!("{screens:#?}"));
        assert_eq!(screens[asked + 1], end, "{screens:#?}");
        // Then from the top.
        assert_eq!(screens[asked + 2], PROMPT);
    }

    /// Every style off, though animations are on.
    fn quiet() -> Animations {
        Animations {
            enabled: true,
            cursor_motion: CursorMotion::Off,
            cursor_blink: CursorBlink::Classic,
            typed_text: TypedText::Off,
            keystroke_burst: KeystrokeBurst::Off,
            shake: Shake::Off,
            new_text: NewText::Off,
            reveal: Reveal::Instant,
            scrolling: Scrolling::Off,
            new_lines: NewLines::Off,
            ..Animations::default()
        }
    }

    /// Whether, on some frame of a time round, the text drawn differs from
    /// what a plain frame of the same screen would draw.
    fn drawn_other_than_plain(animations: Animations) -> bool {
        let (ctx, plain_ctx) = (egui::Context::default(), egui::Context::default());
        let mut preview = AnimationPreview::default();
        let length = preview.script.last().unwrap().0;
        let mut time = 10.0;
        while time < 10.0 + length + 1.0 {
            let drawn = drawn_text(&frame(&ctx, &mut preview, time, animations, true));
            if drawn != drawn_text(&plain_frame(&plain_ctx, &preview, time)) {
                return true;
            }
            time += 0.04;
        }
        false
    }

    #[test]
    fn animations_change_what_is_drawn_and_off_draws_the_screen_plain() {
        // Each effect on its own shows in the text drawn: a cell hidden,
        // scaled or tinted.
        for (what, animations) in [
            ("hidden", Animations { reveal: Reveal::Typewriter, ..quiet() }),
            ("scaled", Animations { new_text: NewText::Zoom, ..quiet() }),
            ("tinted", Animations { new_text: NewText::Heat, ..quiet() }),
        ] {
            assert!(drawn_other_than_plain(animations), "no cell {what} on any frame");
        }

        // The comparison picks up nothing else: with every style off there's
        // no difference, and neither with the styles chosen but the master
        // switch off.
        assert!(!drawn_other_than_plain(quiet()), "differs with every style off");
        let off = Animations {
            enabled: false,
            reveal: Reveal::Typewriter,
            typed_text: TypedText::Pop,
            new_text: NewText::Heat,
            ..Animations::default()
        };
        assert!(!drawn_other_than_plain(off), "animations off still changed the drawing");
    }

    /// The most characters of the screen a frame of a time round left out.
    fn most_held_back(reveal: Reveal) -> i64 {
        let animations = Animations { reveal, ..quiet() };
        let ctx = egui::Context::default();
        let mut preview = AnimationPreview::default();
        let length = preview.script.last().unwrap().0;
        let (mut most, mut time) = (0, 10.0);
        while time < 10.0 + length + 1.0 {
            let out = frame(&ctx, &mut preview, time, animations, true);
            let on_screen = screen(&preview).chars().filter(|c| !c.is_whitespace()).count();
            most = most.max(on_screen as i64 - glyphs(&out) as i64);
            time += 0.02;
        }
        most
    }

    #[test]
    fn every_reveal_but_instant_holds_the_demo_back_on_some_frame() {
        assert!(most_held_back(Reveal::Instant) <= 0, "instant held something back");
        for reveal in [Reveal::Lines, Reveal::Words, Reveal::Typewriter] {
            // Not just the prompt, as when the table came a line at a time
            // and each line was shown the moment it arrived: a good part of
            // the table.
            let held = most_held_back(reveal);
            assert!(held > 100, "{reveal:?} held back only {held} characters at most");
        }
    }

    #[test]
    fn it_animates_only_with_animations_on() {
        let first_key = script().iter().find(|(_, step)| matches!(step, Step::Key(_))).unwrap().0;
        for (enabled, animating) in [(false, false), (true, true)] {
            let animations = Animations { enabled, ..Animations::default() };
            let ctx = egui::Context::default();
            let mut preview = AnimationPreview::default();
            for i in 0..5 {
                frame(&ctx, &mut preview, 10.0 + f64::from(i) * 0.02, animations, true);
            }
            let out = frame(&ctx, &mut preview, 10.0 + first_key + 0.01, animations, true);
            assert_eq!(screen(&preview), "Switch#s");
            assert_eq!(repaint_delay(&out) == Duration::ZERO, animating, "enabled: {enabled}");
        }
    }

    #[test]
    fn hidden_it_asks_for_nothing_and_starts_over_when_shown_again() {
        let ctx = egui::Context::default();
        let mut preview = AnimationPreview::default();
        let off = Animations::default();
        let mut time = 10.0;
        while time < 11.2 {
            frame(&ctx, &mut preview, time, off, true);
            time += 0.02;
        }
        assert!(screen(&preview).starts_with("Switch#sh"), "{}", screen(&preview));
        // Typing: the next key is never more than a key's time away.
        let out = frame(&ctx, &mut preview, time, off, true);
        assert!(repaint_delay(&out) <= Duration::from_secs_f64(KEY_INTERVAL), "{:?}", repaint_delay(&out));

        // Another page showing: no frames asked for, and the demo waits.
        let typed = screen(&preview);
        for later in [0.02, 5.0] {
            let out = frame(&ctx, &mut preview, time + later, off, false);
            assert_eq!(repaint_delay(&out), Duration::MAX);
        }
        assert_eq!(screen(&preview), typed);

        // Back again, it starts from the top.
        frame(&ctx, &mut preview, time + 6.0, off, true);
        assert_eq!(screen(&preview), PROMPT);
    }
}
