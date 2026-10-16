//! Render every palette and step new output effects through real terminal frames.
//! `cargo run --locked --example themes_motion_qa` writes target/themes-motion-qa/.
use std::sync::Arc;

use egui::{FontId, vec2};
use egui_kittest::Harness;
use parking_lot::Mutex;
use snekkie::{
    settings::{Animations, AppSettings, NewLines, NewText, THEMES, Theme},
    terminal::{emulator::Emulator, highlight},
    ui::{
        style,
        terminal_view::{TerminalView, ViewOptions},
    },
};

struct Sample {
    view: TerminalView,
    emulator: Mutex<Emulator>,
    theme: Theme,
    animations: Animations,
}

fn sample(name: &str, animations: Animations) -> Harness<'static, Sample> {
    let emulator = Mutex::new(Emulator::new(80, 8, 100, Arc::new(|_| {})));
    emulator.lock().feed(b"Switch# show ip interface brief\r\n");
    let theme = AppSettings::default().theme_named(name);
    let label = name.to_owned();
    let mut app = Harness::builder().with_size([760.0, 230.0]).with_step_dt(0.02).build_ui_state(
        move |ui, sample: &mut Sample| {
            style::apply_theme(ui.ctx(), sample.theme);
            ui.heading(&label);
            sample.view.show_demo(
                ui,
                vec2(ui.available_width(), 180.0),
                &sample.emulator,
                &ViewOptions {
                    theme: sample.theme,
                    syntax: "cisco_ios",
                    highlighting_intensity: highlight::DEFAULT_INTENSITY,
                    regular: FontId::monospace(15.0),
                    bold: FontId::monospace(15.0),
                    keyboard: false,
                    mouse_input: false,
                    right_click_paste: true,
                    animations: sample.animations,
                },
            );
        },
        Sample { view: TerminalView::default(), emulator, theme, animations },
    );
    app.run_steps(3);
    app
}

fn reply(app: &mut Harness<'static, Sample>) {
    app.state_mut().emulator.lock().feed(
        b"Interface       IP-Address      Status\r\nGi0/1           192.0.2.1       \x1b[32mup\x1b[0m\r\nGi0/2           unassigned      \x1b[31mdown\x1b[0m\r\n\x1b[31m% Access denied\x1b[0m\r\nSwitch#",
    );
    app.step();
}

fn capture(app: &mut Harness<'static, Sample>, name: &str) {
    app.render().unwrap().save(format!("target/themes-motion-qa/{name}.png")).unwrap();
}

fn main() {
    std::fs::create_dir_all("target/themes-motion-qa").unwrap();
    for (name, _) in THEMES {
        let mut app = sample(name, Animations::default());
        reply(&mut app);
        capture(&mut app, &format!("palette-{name}"));
    }
    for theme in ["Snekkie Dark", "Frost Light", "Copper Night", "Forest Night"] {
        for (name, text, line) in [
            ("focus", NewText::Focus, NewLines::Off),
            ("bookends", NewText::Off, NewLines::Bookends),
            ("outline", NewText::Off, NewLines::Outline),
        ] {
            let animations = Animations { new_text: text, new_lines: line, ..Animations::reduced_motion() };
            let mut app = sample(theme, animations);
            reply(&mut app);
            let text = app.state().emulator.lock().screen_text();
            for frame in 0..19 {
                capture(&mut app, &format!("{theme}-{name}-{frame:02}"));
                assert_eq!(app.state().emulator.lock().screen_text(), text);
                app.step();
            }
        }
    }
    println!("Rendered all {} palettes and 12 output-effect timelines", THEMES.len());
}
