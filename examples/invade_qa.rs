//! Render Invade's screens with temporary settings and progress.
//! `cargo run --locked --example invade_qa` writes target/invade-qa/.
use egui_kittest::{Harness, kittest::Queryable};
use snekkie::invade::autoplay::{Autoplay, Skill};
use snekkie::invade::run::STEP;
use snekkie::invade::{Input, Phase, UpgradeId};
use snekkie::settings::{AppSettings, SettingsStore};
use snekkie::ui::{Paths, SnekkieApp};

const OUT: &str = "target/invade-qa";

struct Qa {
    app: Harness<'static, SnekkieApp>,
    _dir: tempfile::TempDir,
    name: String,
}

impl Qa {
    fn new(theme: &str, size: [f32; 2], scale: f32, progress: Option<&str>) -> Qa {
        let mut qa = Qa::setup(theme, size, scale, progress);
        qa.start();
        qa
    }

    fn setup(theme: &str, size: [f32; 2], scale: f32, progress: Option<&str>) -> Qa {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { sessions: dir.path().join("sessions.json"), settings: dir.path().join("settings.json") };
        let settings = AppSettings { theme: theme.into(), check_for_updates: false, ..Default::default() };
        SettingsStore::new(paths.settings.clone()).save(&settings).unwrap();
        if let Some(progress) = progress {
            std::fs::write(dir.path().join("invade.json"), progress).unwrap();
        }
        let mut app = Harness::builder()
            .with_size(size)
            .with_pixels_per_point(scale)
            .with_step_dt(1.0 / 60.0)
            .build_eframe(move |_| SnekkieApp::with_port_lister(paths, Vec::new));
        app.run_ok();
        app.get_by_label("Help").click();
        app.run_ok();
        app.get_by_label("About Snekkie").click();
        app.run_ok();
        app.get_by_label("Invade").click();
        for _ in 0..3 {
            app.step();
        }
        let name = format!("{}-{}x{}-{scale}", theme.to_lowercase().replace(' ', "-"), size[0], size[1]);
        Qa { app, _dir: dir, name }
    }

    fn start(&mut self) {
        if self.app.query_by_label("Start run").is_some() {
            self.app.get_by_label("Start run").click();
            self.app.step();
            self.app.step();
        }
    }

    fn level_up(&mut self) {
        let run = self.run();
        let (x, y) = (run.player().x, run.player().y + 0.5);
        let needed = run.experience_needed() - run.experience();
        run.spawn_experience(x, y, needed);
        self.app.step();
        self.app.step();
        assert_eq!(self.run().phase(), Phase::Upgrade);
    }

    fn run(&mut self) -> &mut snekkie::invade::InvadeRun {
        self.app.state_mut().invade_tab().unwrap().run_mut()
    }

    /// Play automatically for `seconds` of game time, then draw a frame.
    fn play(&mut self, seconds: f32, seed: u64) {
        if self.app.state_mut().invade_tab().unwrap().is_selecting_loadout() {
            return;
        }
        let mut player = Autoplay::new(Skill::EXPERT, seed);
        let run = self.run();
        snekkie::invade::autoplay::play(run, &mut player, seconds, |_| false);
        self.app.step();
        self.app.step();
    }

    fn capture(&mut self, what: &str) {
        // No stray pointer or hover highlight in the pictures.
        self.app.event(egui::Event::PointerGone);
        self.app.step();
        let path = format!("{OUT}/{}-{what}.png", self.name);
        self.app.render().unwrap().save(&path).unwrap();
        println!("{path}");
    }
}

const UNLOCKED: &str = r#"{"version":1,"selected_rig":"ghost","unlocked":["ghost","bastion","fork","fragment","guidance","cache_shield","tool_charge","feedback","root_access"],"devices_compromised":420}"#;

fn main() {
    std::fs::create_dir_all(OUT).unwrap();

    // Every screen, in the default theme and window.
    let mut qa = Qa::setup("Snekkie Dark", [1000.0, 640.0], 1.0, None);
    qa.capture("loadout");
    qa.start();
    qa.capture("countdown");
    for _ in 0..200 {
        qa.app.step();
    }
    qa.play(14.0, 1);
    qa.capture("wave-1");
    qa.run().set_immortal();
    let (x, y) = (qa.run().player().x, qa.run().player().y + 0.5);
    for (dx, dy) in [(-12.0, -4.0), (15.0, -3.0), (10.0, 5.0)] {
        qa.run().spawn_experience(x + dx, y + dy, 2);
    }
    qa.capture("xp-orbs");
    // Keep this camera fixture separate from the seeded asteroid field:
    // collision/avoidance scenes below keep their normal rocks.
    qa.run().clear_asteroids();
    let travel_start = qa.run().player().x;
    let heading = if travel_start > snekkie::invade::content::WORLD_W / 2.0 {
        -std::f32::consts::FRAC_PI_2
    } else {
        std::f32::consts::FRAC_PI_2
    };
    qa.run().set_flight(heading, (0.0, 0.0));
    for _ in 0..1200 {
        if qa.run().phase() == Phase::Upgrade {
            qa.run().choose(0);
        }
        qa.run().step(snekkie::invade::Input { thrust: true, ..Default::default() });
    }
    assert!((qa.run().player().x - travel_start).abs() > 80.0, "the world extends beyond the original arena");
    let camera = qa.run().camera();
    assert!((camera.0 + 40.0 - qa.run().player().x).abs() < 1e-3);
    qa.capture("world-travel");
    // A ship's warning: its "!" and the "+" where its shot will go.
    let mut player = Autoplay::new(Skill::EXPERT, 6);
    for _ in 0..2400 {
        if qa.run().phase() == Phase::Upgrade {
            qa.run().choose(0);
        }
        if qa.run().marks().iter().any(|m| m.is_shot() && m.warn < 0.8) {
            break;
        }
        let input = player.input(qa.run());
        qa.run().step(input);
    }
    qa.app.step();
    qa.capture("ship-telegraph");
    qa.run().set_immortal();
    qa.run().skip_to_wave(3);
    for _ in 0..60 {
        qa.app.step();
    }
    qa.level_up();
    qa.capture("upgrade");
    qa.app.get_by_label_contains("[1]").click();
    qa.app.step();
    qa.app.step();
    qa.play(4.0, 3);
    qa.app.key_press(egui::Key::Escape);
    qa.app.step();
    qa.app.step();
    qa.capture("pause");
    qa.app.get_by_label("Build").click();
    qa.app.step();
    qa.app.step();
    qa.capture("build");
    qa.app.get_by_label("Back").click();
    qa.app.step();
    qa.app.step();
    qa.app.get_by_label("Restart run…").click();
    qa.app.step();
    qa.app.step();
    qa.capture("restart-confirm");

    // A controlled field shows all three rock sizes and a rotated,
    // thrusting ship. Use real W input so the flame reflects held thrust.
    for theme in ["Snekkie Dark", "Frost Light"] {
        let mut field = Qa::new(theme, [1000.0, 640.0], 1.0, None);
        field.run().set_immortal();
        // Let the countdown and centered wave banner finish so the nose
        // and body remain visible in this flight-focused capture.
        for _ in 0..400 {
            field.app.step();
        }
        let run = field.run();
        run.clear_asteroids();
        let (x, y) = (run.player().x, run.player().y);
        for (dx, dy, radius) in [(-22.0, -5.0, 5.0), (19.0, -6.0, 3.5), (25.0, 8.0, 2.2)] {
            run.place_asteroid((x + dx, y + dy), radius, (1.5, 0.5));
        }
        run.set_flight(std::f32::consts::FRAC_PI_4, (0.0, 0.0));
        field.app.key_down(egui::Key::W);
        for _ in 0..20 {
            field.app.step();
        }
        assert!(field.run().player().thrusting);
        assert_eq!(field.run().asteroids().len(), 3);
        field.capture("flight-asteroids");
        field.app.key_up(egui::Key::W);
    }

    // A complete four-weapon build, including the three level-up weapons.
    let mut arsenal = Qa::new("Snekkie Dark", [1000.0, 640.0], 1.0, Some(UNLOCKED));
    arsenal.run().set_immortal();
    arsenal.run().skip_to_wave(8);
    for weapon in [
        snekkie::invade::arsenal::WeaponKind::Arc,
        snekkie::invade::arsenal::WeaponKind::Orbit,
        snekkie::invade::arsenal::WeaponKind::Pulse,
    ] {
        assert!(arsenal.run().grant_weapon(weapon));
    }
    arsenal.play(10.0, 42);
    arsenal.capture("arsenal");
    arsenal.app.key_press(egui::Key::B);
    arsenal.app.step();
    arsenal.app.step();
    arsenal.capture("arsenal-build");

    // Encounters, with everything unlocked and a strong build.
    for (wave, what, seconds) in [
        (10, "gateway", 9.0),
        (20, "load-balancer", 3.2),
        (20, "load-balancer-curtain", 0.0),
        (30, "sentinel-ids", 3.1),
        (40, "root-controller", 4.0),
    ] {
        let mut qa = Qa::new("Snekkie Dark", [1000.0, 640.0], 1.0, Some(UNLOCKED));
        let run = qa.run();
        run.set_immortal();
        for id in [UpgradeId::Payload, UpgradeId::Fork, UpgradeId::Fragment] {
            run.grant(id);
        }
        run.skip_to_wave(wave);
        if wave == 40 {
            // Its first reinforcements arrive at three quarters health.
            for _ in 0..3 {
                run.grant(UpgradeId::Payload);
            }
        }
        if what != "load-balancer-curtain" {
            qa.play(seconds, wave.into());
        } else {
            // Hold still from the start until the first curtain is a third
            // of the way down, so the whole curtain and its SAFE gap show.
            let run = qa.run();
            for _ in 0..(12.0 / STEP) as usize {
                let top = run.camera().1;
                if run.curtains().iter().any(|c| c.warn <= 0.0 && (c.y - top - 9.0).abs() < 0.5) {
                    break;
                }
                run.step(Input::default());
            }
            qa.app.step();
            qa.app.step();
        }
        qa.capture(what);
    }
    for kind in snekkie::invade::MinibossKind::ALL {
        let seed = (0..5000)
            .find(|&s| snekkie::invade::content::encounter_for(s, 5) == snekkie::invade::EncounterKind::Miniboss(kind))
            .unwrap();
        let mut qa = Qa::new("Snekkie Dark", [1000.0, 640.0], 1.0, Some(UNLOCKED));
        // A new run with a seed that puts this miniboss on wave 5.
        let rig = qa.run().rig();
        *qa.run() = snekkie::invade::InvadeRun::new(seed, rig, [], 0);
        qa.run().set_immortal();
        qa.run().skip_to_wave(5);
        qa.play(3.5, seed);
        qa.capture(&format!("miniboss-{}", kind.name().to_lowercase().replace(' ', "-")));
    }

    // Each rig, its special, and the end of a run.
    for (rig, json) in [("probe", r#""probe""#), ("ghost", r#""ghost""#), ("bastion", r#""bastion""#)] {
        let progress = UNLOCKED.replace(r#""ghost","unlocked""#, &format!(r#"{json},"unlocked""#));
        let mut qa = Qa::new("Snekkie Dark", [1000.0, 640.0], 1.0, Some(&progress));
        for _ in 0..200 {
            qa.app.step();
        }
        qa.run().set_immortal();
        qa.play(6.0, 9);
        qa.app.key_press(egui::Key::X);
        qa.app.step();
        for _ in 0..6 {
            qa.app.step();
        }
        qa.capture(&format!("rig-{rig}"));
        // Exercise real turning/thrust input and a fresh Shift edge, not
        // only the model helper used to stage encounters above.
        for _ in 0..1000 {
            if qa.run().phase() == Phase::Upgrade {
                qa.run().choose(0);
            }
            if qa.run().player().mobility_cooldown <= 0.0 {
                break;
            }
            qa.run().step(snekkie::invade::Input::default());
        }
        assert_eq!(qa.run().player().mobility_cooldown, 0.0, "{rig} movement cooldown must recharge");
        qa.app.key_down(egui::Key::W);
        qa.app.key_down(egui::Key::D);
        for _ in 0..12 {
            qa.app.step();
        }
        qa.app.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
        qa.app.step();
        qa.app.step();
        assert!(
            qa.run().player().mobility_cooldown > qa.run().rig().mobility_recharge() - 0.1,
            "{rig} fresh Shift must trigger mobility"
        );
        qa.capture(&format!("rig-{rig}-mobility"));
        qa.app.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        qa.app.key_up(egui::Key::W);
        qa.app.key_up(egui::Key::D);
    }
    let mut qa = Qa::new("Snekkie Dark", [1000.0, 640.0], 1.0, Some(UNLOCKED));
    qa.run().skip_to_wave(4);
    qa.run().lose();
    qa.app.step();
    qa.app.step();
    assert_eq!(qa.run().integrity(), 0, "the defeat capture must show a real integrity loss");
    assert_eq!(qa.run().phase(), Phase::Over);
    qa.capture("game-over-recorded");

    // Themes, scales and window sizes.
    for theme in ["Paper Light", "Monochrome Green", "CRT Super", "E-Ink Super", "Blueprint Super", "Amber Super"] {
        let mut qa = Qa::new(theme, [1000.0, 640.0], 1.0, None);
        for _ in 0..200 {
            qa.app.step();
        }
        qa.play(12.0, 4);
        qa.capture("wave-1");
        qa.app.key_press(egui::Key::P);
        qa.app.step();
        qa.app.step();
        qa.capture("pause");
        qa.app.get_by_label("Build").click();
        qa.app.step();
        qa.app.step();
        qa.capture("build");

        // Cards inherit the overlay's colors too, including their precise
        // computed descriptions and tradeoffs in a light theme.
        let mut rewards = Qa::new(theme, [1000.0, 640.0], 1.0, None);
        rewards.run().set_immortal();
        rewards.run().skip_to_wave(3);
        rewards.level_up();
        assert_eq!(rewards.run().phase(), Phase::Upgrade);
        rewards.capture("upgrade");
    }
    for (size, scale) in [([640.0, 400.0], 1.0), ([520.0, 320.0], 1.0), ([1000.0, 640.0], 1.5), ([1400.0, 900.0], 1.25)]
    {
        let mut qa = Qa::new("Snekkie Dark", size, scale, None);
        for _ in 0..200 {
            qa.app.step();
        }
        qa.play(10.0, 5);
        qa.capture("play");
        if size[0] >= 640.0 && size[1] >= 400.0 {
            qa.app.key_press(egui::Key::P);
            qa.app.step();
            qa.app.step();
            qa.capture("pause");
            qa.app.get_by_label("Build").click();
            qa.app.step();
            qa.app.step();
            qa.capture("build");

            let mut rewards = Qa::new("Snekkie Dark", size, scale, None);
            rewards.run().set_immortal();
            rewards.run().skip_to_wave(3);
            rewards.level_up();
            assert_eq!(rewards.run().phase(), Phase::Upgrade);
            rewards.capture("upgrade");
        }
    }
}
