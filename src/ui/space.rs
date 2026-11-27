//! The Space appearance's sky: stars and planets behind the whole app. With
//! animations on they answer new terminal text and the pointer, and go quiet
//! (no frames asked for) once everything settles. With animations off the
//! sky holds still.

use std::f32::consts::{PI, TAU};

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Color32, InputState, Painter, PointerButton, Pos2, Rect, Shape, Stroke, Ui, Vec2, vec2};

use super::animation::scramble;
use crate::settings::{Animations, AppAppearance, Theme};

const STARS: u64 = 240;
/// Soft patches of color: x and y as fractions of the window, then radius as
/// a fraction of its shorter side.
const NEBULAE: [(f32, f32, f32); 3] = [(0.22, 0.3, 0.42), (0.68, 0.62, 0.5), (0.9, 0.12, 0.3)];
/// Pointer parallax of the nearest things, in points.
const PARALLAX: f32 = 12.0;
/// How far around a planet the pointer pushes it, in points, and how hard.
const PUSH_RANGE: f32 = 70.0;
const PUSH: f32 = 220.0;
const SPRING: f32 = 7.0;
const DAMPING: f32 = 3.5;
/// A wave set off by new text: how long it lasts at normal speed, how fast it
/// spreads, and how hard it shoves planets and stars.
const WAVE_LIFE: f32 = 1.4;
const WAVE_SPEED: f32 = 420.0;
const WAVE_KICK: f32 = 260.0;
const WAVE_STAR_SHOVE: f32 = 24.0;
const MAX_WAVES: usize = 5;
const SETTLED: f32 = 0.6;
/// Longest step the motion takes, so a stall can't fling anything.
const MAX_STEP: f32 = 1.0 / 20.0;

/// Whether the sky is live for this theme and these animations.
pub fn is_active(theme: &Theme, animations: &Animations) -> bool {
    animations.enabled && theme.effects.appearance() == AppAppearance::Space
}

/// Ctrl+Shift with the primary button picks a planet up. Plain drags stay
/// text selection.
pub fn grabbing(input: &InputState) -> bool {
    input.modifiers.ctrl && input.modifiers.shift && input.pointer.button_down(PointerButton::Primary)
}

#[derive(Clone, Copy)]
struct Planet {
    /// Where it rests, as a fraction of the window. Moved by dragging.
    home: (f32, f32),
    /// Radius as a fraction of the window's shorter side.
    size: f32,
    /// Lit-side color, and a second color for its bands.
    base: Color32,
    bands: Color32,
    /// How many latitude stripes; 0 for none.
    stripes: f32,
    ring: bool,
    /// Knocked away from home, in points, and how fast it's moving.
    offset: Vec2,
    velocity: Vec2,
}

const fn planet(home: (f32, f32), size: f32, base: [u8; 3], bands: [u8; 3], stripes: f32, ring: bool) -> Planet {
    Planet {
        home,
        size,
        base: Color32::from_rgb(base[0], base[1], base[2]),
        bands: Color32::from_rgb(bands[0], bands[1], bands[2]),
        stripes,
        ring,
        offset: Vec2::ZERO,
        velocity: Vec2::ZERO,
    }
}

// Muted on purpose: at their brightest these still sit far below terminal text.
const PLANETS: [Planet; 4] = [
    planet((0.82, 0.26), 0.17, [176, 140, 104], [112, 80, 70], 9.0, true),
    planet((0.13, 0.76), 0.12, [74, 124, 178], [70, 148, 128], 3.0, false),
    planet((0.5, 0.13), 0.05, [170, 92, 76], [118, 66, 60], 0.0, false),
    planet((0.62, 0.86), 0.07, [160, 164, 186], [112, 118, 148], 5.0, false),
];

#[derive(Clone, Copy)]
struct Wave {
    at: Pos2,
    age: f32,
    strength: f32,
}

impl Wave {
    fn fade(&self) -> f32 {
        self.strength * (1.0 - self.age / WAVE_LIFE)
    }
}

pub struct Scene {
    planets: [Planet; 4],
    parallax: Vec2,
    waves: Vec<Wave>,
    /// New text since the last frame, to be set off on the next one.
    landed: Option<(Pos2, usize)>,
    held: Option<usize>,
    last_time: f64,
    moving: bool,
}

impl Default for Scene {
    fn default() -> Self {
        Scene {
            planets: PLANETS,
            parallax: Vec2::ZERO,
            waves: Vec::new(),
            landed: None,
            held: None,
            last_time: 0.0,
            moving: false,
        }
    }
}

impl Scene {
    /// `cells` of new terminal text landed, centered on `at`.
    pub fn text_landed(&mut self, ctx: &egui::Context, at: Pos2, cells: usize) {
        let cells = self.landed.map_or(cells, |(_, before)| before + cells);
        self.landed = Some((at, cells));
        ctx.request_repaint();
    }

    /// Move the sky along and draw it, under whatever is drawn after this.
    pub fn ui(&mut self, ui: &Ui, theme: &Theme, animations: &Animations) {
        let rect = ui.max_rect();
        if animations.enabled {
            let (time, pointer, pressed, down, grab) = ui.input(|i| {
                (i.time, i.pointer.latest_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), grabbing(i))
            });
            let step = ((time - self.last_time) as f32).clamp(0.0, MAX_STEP) * animations.pace();
            self.last_time = time;
            self.grab(rect, pointer, pressed, down, grab);
            self.step(step, rect, pointer);
            if self.moving {
                ui.ctx().request_repaint();
            }
        } else {
            self.hold_still();
        }
        draw(ui.painter(), rect, theme, &self.planets, self.parallax, &self.waves);
    }

    fn hold_still(&mut self) {
        self.parallax = Vec2::ZERO;
        self.waves.clear();
        self.landed = None;
        self.held = None;
        self.moving = false;
        for planet in &mut self.planets {
            planet.offset = Vec2::ZERO;
            planet.velocity = Vec2::ZERO;
        }
    }

    fn grab(&mut self, rect: Rect, pointer: Option<Pos2>, pressed: bool, down: bool, grab: bool) {
        if !down {
            self.held = None;
        }
        let Some(pointer) = pointer else { return };
        if pressed && grab {
            let shorter = rect.width().min(rect.height());
            self.held = self
                .planets
                .iter()
                .map(|p| (p.centre(rect) + p.offset).distance(pointer) - p.size * shorter)
                .enumerate()
                .filter(|(_, gap)| *gap < 24.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(index, _)| index);
        }
        if let Some(index) = self.held {
            let planet = &mut self.planets[index];
            let inside = rect.clamp(pointer);
            planet.home = ((inside.x - rect.left()) / rect.width(), (inside.y - rect.top()) / rect.height());
            planet.offset = Vec2::ZERO;
            planet.velocity = Vec2::ZERO;
        }
    }

    fn step(&mut self, dt: f32, rect: Rect, pointer: Option<Pos2>) {
        if let Some((at, cells)) = self.landed.take() {
            self.set_off(rect, at, cells);
        }
        self.waves.retain_mut(|wave| {
            wave.age += dt;
            wave.age < WAVE_LIFE
        });

        let pointer = pointer.filter(|p| rect.contains(*p));
        let target = pointer.map_or(Vec2::ZERO, |p| {
            let half = rect.size() / 2.0;
            -vec2((p.x - rect.center().x) / half.x, (p.y - rect.center().y) / half.y) * PARALLAX
        });
        self.parallax += (target - self.parallax) * (1.0 - (-6.0 * dt).exp());
        let mut moving = !self.waves.is_empty() || (self.parallax - target).length() > 0.05;
        if !moving {
            self.parallax = target;
        }

        let shorter = rect.width().min(rect.height());
        for (index, planet) in self.planets.iter_mut().enumerate() {
            if self.held == Some(index) {
                continue;
            }
            let mut force = -planet.offset * SPRING - planet.velocity * DAMPING;
            if let Some(pointer) = pointer {
                let away = planet.centre(rect) + planet.offset - pointer;
                let gap = away.length() - planet.size * shorter;
                if gap < PUSH_RANGE {
                    force += away.normalized() * PUSH * (1.0 - gap.max(0.0) / PUSH_RANGE);
                }
            }
            // It rests wherever the forces balance, home or pushed aside.
            if planet.velocity.length() < SETTLED && force.length() < SETTLED * SPRING {
                planet.velocity = Vec2::ZERO;
                continue;
            }
            moving = true;
            planet.velocity += force * dt;
            planet.offset += planet.velocity * dt;
            let reach = 0.35 * shorter;
            if planet.offset.length() > reach {
                planet.offset = planet.offset.normalized() * reach;
            }
        }
        self.moving = moving;
    }

    fn set_off(&mut self, rect: Rect, at: Pos2, cells: usize) {
        let strength = (cells as f32 / 60.0).clamp(0.15, 1.0);
        // A stream of output swells the newest wave instead of adding more.
        match self.waves.last_mut() {
            Some(wave) if wave.age < 0.12 => wave.strength = (wave.strength + strength).min(1.0),
            _ => {
                if self.waves.len() == MAX_WAVES {
                    self.waves.remove(0);
                }
                self.waves.push(Wave { at, age: 0.0, strength });
            }
        }
        for (index, planet) in self.planets.iter_mut().enumerate() {
            if self.held != Some(index) {
                let away = planet.centre(rect) + planet.offset - at;
                let falloff = 1.0 / (1.0 + away.length() / 220.0);
                planet.velocity += away.normalized() * WAVE_KICK * strength * falloff;
            }
        }
    }
}

impl Planet {
    fn centre(&self, rect: Rect) -> Pos2 {
        rect.min + vec2(self.home.0 * rect.width(), self.home.1 * rect.height())
    }
}

/// The resting sky in `rect`, for a preview.
pub fn paint_still(painter: &Painter, rect: Rect, theme: &Theme) {
    draw(painter, rect, theme, &PLANETS, Vec2::ZERO, &[]);
}

fn draw(painter: &Painter, rect: Rect, theme: &Theme, planets: &[Planet], parallax: Vec2, waves: &[Wave]) {
    painter.rect_filled(rect, 0.0, theme.bg);
    let shorter = rect.width().min(rect.height());
    let mut shapes = Vec::with_capacity(STARS as usize + 16);
    for (x, y, radius) in NEBULAE {
        let centre = rect.min + vec2(x * rect.width(), y * rect.height()) + parallax * 0.15;
        let tint = if x < 0.5 { theme.selection } else { theme.cursor };
        shapes.push(glow(centre, radius * shorter, tint.gamma_multiply(0.16)));
    }
    let ink = star_color(theme);
    for i in 0..STARS {
        let (position, depth, size, brightness) = star(i);
        let mut at = rect.min + vec2(position.0 * rect.width(), position.1 * rect.height()) + parallax * depth;
        let mut lit = brightness;
        for wave in waves {
            let away = at - wave.at;
            // Strongest in a band around the wave's front.
            let near = 1.0 - ((away.length() - wave.age * WAVE_SPEED) / 70.0).abs();
            if near > 0.0 {
                at += away.normalized() * WAVE_STAR_SHOVE * near * wave.fade();
                lit = (lit + near * wave.fade()).min(1.0);
            }
        }
        shapes.push(Shape::circle_filled(at, size, ink.gamma_multiply(lit)));
    }
    for planet in planets {
        let centre = planet.centre(rect) + planet.offset + parallax * 0.6;
        draw_planet(&mut shapes, centre, planet.size * shorter, planet);
    }
    for wave in waves {
        // A bright edge in a soft band: it has to read through the terminal's cover.
        for (width, alpha) in [(14.0, 0.25), (1.5, 1.0)] {
            let stroke = Stroke::new(width, theme.cursor.gamma_multiply(alpha * wave.fade()));
            shapes.push(Shape::circle_stroke(wave.at, wave.age * WAVE_SPEED, stroke));
        }
    }
    painter.extend(shapes);
}

/// Bright on a dark sky; on a light one, the text color so stars still show.
fn star_color(theme: &Theme) -> Color32 {
    let sum = |c: Color32| u16::from(c.r()) + u16::from(c.g()) + u16::from(c.b());
    if sum(theme.fg) > sum(theme.bg) { Color32::WHITE } else { theme.fg }
}

/// Star `i`: where as a fraction of the window, parallax depth, radius and
/// brightness. The same every time.
fn star(i: u64) -> ((f32, f32), f32, f32, f32) {
    let unit = |salt: u64| (scramble(i.wrapping_mul(0x9e37_79b9) ^ (salt << 40)) % 65_536) as f32 / 65_536.0;
    let depth = 0.15 + 0.85 * unit(3) * unit(4);
    ((unit(1), unit(2)), depth, 0.8 + 1.5 * depth, 0.45 + 0.55 * unit(5))
}

/// A soft blob: full color at the middle, gone at the edge.
fn glow(centre: Pos2, radius: f32, color: Color32) -> Shape {
    const SIDES: u32 = 40;
    let mut mesh = Mesh::default();
    mesh.vertices.push(Vertex { pos: centre, uv: WHITE_UV, color });
    for side in 0..SIDES {
        let angle = TAU * side as f32 / SIDES as f32;
        let pos = centre + vec2(angle.cos(), angle.sin()) * radius;
        mesh.vertices.push(Vertex { pos, uv: WHITE_UV, color: Color32::TRANSPARENT });
        mesh.indices.extend([0, side + 1, (side + 1) % SIDES + 1]);
    }
    Shape::mesh(mesh)
}

/// A shaded disc lit from the upper left, with a soft rim and maybe a ring.
fn draw_planet(shapes: &mut Vec<Shape>, centre: Pos2, radius: f32, planet: &Planet) {
    const RINGS: u32 = 10;
    const SIDES: u32 = 56;
    if planet.ring {
        shapes.extend(ring(centre, radius, planet.base, true));
    }
    shapes.push(glow(centre, radius * 1.18, planet.base.gamma_multiply(0.22)));
    let shade = |x: f32, y: f32| {
        let z = (1.0 - x * x - y * y).max(0.0).sqrt();
        let light = (-0.55 * x - 0.55 * y + 0.63 * z).max(0.0);
        let band =
            if planet.stripes > 0.0 { 0.5 + 0.5 * (y * planet.stripes + 0.6 * (x * 2.0).sin()).sin() } else { 0.0 };
        let color = planet.base.lerp_to_gamma(planet.bands, band);
        Color32::BLACK.lerp_to_gamma(color, (0.06 + light).min(1.0))
    };
    let mut mesh = Mesh::default();
    mesh.vertices.push(Vertex { pos: centre, uv: WHITE_UV, color: shade(0.0, 0.0) });
    for circle in 1..=RINGS {
        let r = circle as f32 / RINGS as f32;
        for side in 0..SIDES {
            let angle = TAU * side as f32 / SIDES as f32;
            let (x, y) = (r * angle.cos(), r * angle.sin());
            mesh.vertices.push(Vertex { pos: centre + vec2(x, y) * radius, uv: WHITE_UV, color: shade(x, y) });
        }
    }
    let at = |circle: u32, side: u32| 1 + (circle - 1) * SIDES + side % SIDES;
    for side in 0..SIDES {
        mesh.indices.extend([0, at(1, side), at(1, side + 1)]);
        for circle in 1..RINGS {
            mesh.indices.extend([at(circle, side), at(circle + 1, side), at(circle + 1, side + 1)]);
            mesh.indices.extend([at(circle, side), at(circle + 1, side + 1), at(circle, side + 1)]);
        }
    }
    shapes.push(Shape::mesh(mesh));
    if planet.ring {
        shapes.extend(ring(centre, radius, planet.base, false));
    }
}

/// Half of a tilted ring: the half behind the planet, or the one in front.
fn ring(centre: Pos2, radius: f32, color: Color32, back: bool) -> impl Iterator<Item = Shape> {
    let (tilt_sin, tilt_cos) = (-0.32_f32).sin_cos();
    [(1.45, 2.2, 0.32), (1.7, 3.0, 0.42), (1.95, 1.6, 0.24)].into_iter().map(move |(reach, width, alpha)| {
        let points = (0..=48)
            .map(|i| {
                let angle = PI * i as f32 / 48.0 + if back { PI } else { 0.0 };
                let (x, y) = (reach * radius * angle.cos(), 0.3 * reach * radius * angle.sin());
                centre + vec2(x * tilt_cos - y * tilt_sin, x * tilt_sin + y * tilt_cos)
            })
            .collect();
        Shape::line(points, Stroke::new(width, color.gamma_multiply(alpha)))
    })
}

#[cfg(test)]
mod tests {
    use egui::pos2;

    use super::*;
    use crate::settings::AppSettings;
    use crate::terminal::highlight::Category;
    use crate::ui::style::{SPACE_PANEL, SPACE_TERMINAL, contrast};

    const ALL_CATEGORIES: [Category; 12] = [
        Category::Keyword,
        Category::Value,
        Category::Prompt,
        Category::Negate,
        Category::Security,
        Category::Interface,
        Category::Protocol,
        Category::Good,
        Category::Bad,
        Category::Number,
        Category::Metadata,
        Category::Comment,
    ];

    fn window() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 640.0))
    }

    fn energy(scene: &Scene) -> f32 {
        scene.planets.iter().map(|p| p.offset.length() + p.velocity.length()).sum::<f32>() + scene.parallax.length()
    }

    fn space() -> Theme {
        AppSettings { theme: "Space Super".into(), ..Default::default() }.colors()
    }

    /// Run the scene until it says it's still.
    fn settle(scene: &mut Scene, pointer: Option<Pos2>) {
        for _ in 0..3000 {
            scene.step(1.0 / 60.0, window(), pointer);
            if !scene.moving {
                return;
            }
        }
        panic!("never settled");
    }

    #[test]
    fn output_and_pointer_move_things_then_it_stops_asking_for_frames() {
        let mut scene = Scene::default();
        scene.step(1.0 / 60.0, window(), None);
        assert!(!scene.moving);
        assert_eq!(energy(&scene), 0.0);

        scene.landed = Some((pos2(700.0, 200.0), 200));
        scene.step(1.0 / 60.0, window(), None);
        assert!(scene.moving);
        assert!(!scene.waves.is_empty());
        assert!(scene.planets[0].velocity.x > 0.0, "pushed away from the text");
        settle(&mut scene, None);
        assert!(scene.waves.is_empty());
        assert!(energy(&scene) < 2.0, "came back home: {}", energy(&scene));

        // The pointer shifts the parallax and shoves a planet it comes close to.
        settle(&mut scene, Some(pos2(710.0, 166.0)));
        assert!(scene.parallax.length() > 1.0);
        assert!(scene.planets[0].offset.x > 1.0, "{:?}", scene.planets[0].offset);
        let resting = scene.planets[0].offset;
        scene.step(1.0 / 60.0, window(), Some(pos2(710.0, 166.0)));
        assert_eq!(scene.planets[0].offset, resting, "a still pointer lets it rest");
    }

    #[test]
    fn a_flood_of_output_stays_bounded() {
        let mut scene = Scene::default();
        for i in 0..2000 {
            scene.landed = Some((pos2(500.0, (i % 600) as f32), 4000));
            scene.step(1.0 / 60.0, window(), None);
            assert!(scene.waves.len() <= MAX_WAVES);
            for planet in &scene.planets {
                assert!(planet.offset.length() <= 0.35 * 640.0 + 0.01);
            }
        }
    }

    #[test]
    fn dragging_moves_a_planet_and_it_stays() {
        let mut scene = Scene::default();
        let on_giant = scene.planets[0].centre(window());
        scene.grab(window(), Some(on_giant), true, true, true);
        assert_eq!(scene.held, Some(0));
        scene.grab(window(), Some(pos2(300.0, 320.0)), false, true, true);
        scene.grab(window(), Some(pos2(300.0, 320.0)), false, false, false);
        assert_eq!(scene.held, None);
        assert_eq!(scene.planets[0].home, (0.3, 0.5));
        settle(&mut scene, None);
        assert_eq!(scene.planets[0].centre(window()), pos2(300.0, 320.0));

        // Off the window, it stops at the edge.
        scene.grab(window(), Some(pos2(300.0, 320.0)), true, true, true);
        scene.grab(window(), Some(pos2(-50.0, 9000.0)), false, true, true);
        assert_eq!(scene.planets[0].home, (0.0, 1.0));
        scene.grab(window(), None, false, false, false);
        // Nowhere near a planet, nothing is held.
        scene.grab(window(), Some(pos2(330.0, 300.0)), true, true, true);
        assert_eq!(scene.held, None);
        // Without the keys held, a press never picks anything up.
        let on_small = scene.planets[2].centre(window());
        scene.grab(window(), Some(on_small), true, true, false);
        assert_eq!(scene.held, None);
    }

    fn frame(ctx: &egui::Context, scene: &mut Scene, animations: &Animations, time: f64) -> egui::FullOutput {
        // Only the first second's frames carry the pointer: an event itself asks for a frame.
        let events = if time < 1.0 { vec![egui::Event::PointerMoved(pos2(100.0, 100.0))] } else { Vec::new() };
        let input = egui::RawInput {
            screen_rect: Some(window()),
            time: Some(time),
            predicted_dt: 0.0,
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| scene.ui(ui, &space(), animations));
        out.textures_delta.clear();
        out
    }

    #[test]
    fn animations_off_hold_the_sky_still_and_ask_for_nothing() {
        let off = Animations::default();
        let ctx = egui::Context::default();
        let mut scene = Scene::default();
        scene.text_landed(&ctx, pos2(100.0, 100.0), 500);
        let mut before = None;
        for i in 0..5 {
            let out = frame(&ctx, &mut scene, &off, f64::from(i));
            assert_eq!(energy(&scene), 0.0);
            assert!(scene.waves.is_empty());
            if i > 2 {
                assert_eq!(out.viewport_output[&egui::ViewportId::ROOT].repaint_delay, std::time::Duration::MAX);
                let shapes = format!("{:?}", out.shapes);
                assert!(before.as_ref().is_none_or(|b| *b == shapes), "the sky moved");
                before = Some(shapes);
            }
        }

        // Turned on and pushed, then off again: back at rest at once.
        let on = Animations { enabled: true, ..Animations::default() };
        scene.text_landed(&ctx, pos2(100.0, 100.0), 500);
        let out = frame(&ctx, &mut scene, &on, 6.0);
        assert!(scene.moving);
        assert_eq!(out.viewport_output[&egui::ViewportId::ROOT].repaint_delay, std::time::Duration::ZERO);
        frame(&ctx, &mut scene, &off, 7.0);
        assert_eq!(energy(&scene), 0.0);
    }

    #[test]
    fn speed_sets_the_pace() {
        let run = |speed: f32| {
            let animations = Animations { enabled: true, speed, ..Animations::default() };
            let ctx = egui::Context::default();
            let mut scene = Scene::default();
            frame(&ctx, &mut scene, &animations, 0.0);
            scene.text_landed(&ctx, pos2(500.0, 300.0), 500);
            for i in 1..=10 {
                frame(&ctx, &mut scene, &animations, f64::from(i) / 60.0);
            }
            scene.waves[0].age
        };
        assert!(run(2.0) > 1.8 * run(1.0));
    }

    #[test]
    fn text_stays_readable_over_the_brightest_things_in_the_sky() {
        for name in ["Space Super", "Paper Light"] {
            let mut theme = AppSettings { theme: name.into(), ..Default::default() }.colors();
            theme.effects.appearance = AppAppearance::Space;
            for sky in PLANETS.iter().map(|p| p.base) {
                let behind = sky.lerp_to_gamma(theme.bg, SPACE_TERMINAL);
                assert!(contrast(theme.fg, behind) >= 4.5, "{name} text on {sky:?}");
                assert!(contrast(theme.fg, sky.lerp_to_gamma(theme.bg, SPACE_PANEL)) >= 4.5, "{name} panel {sky:?}");
                for category in ALL_CATEGORIES {
                    let color = category.color_on(theme.bg);
                    assert!(contrast(color, behind) >= 3.0, "{name} {category:?} on {sky:?}");
                }
            }
            // A star is a point or two across, but text must still survive one.
            let star = star_color(&theme).lerp_to_gamma(theme.bg, SPACE_TERMINAL);
            assert!(contrast(theme.fg, star) >= 3.0, "{name} text on a star");
        }
        assert!(contrast(space().fg, space().selection) >= 4.5, "selected text");
    }

    #[test]
    fn stars_are_spread_across_the_window_and_stay_where_they_were() {
        assert_eq!(star(7), star(7));
        let stars: Vec<_> = (0..STARS).map(star).collect();
        for quadrant in 0..4 {
            let count = stars
                .iter()
                .filter(|((x, y), ..)| (*x >= 0.5) == (quadrant & 1 == 1) && (*y >= 0.5) == (quadrant & 2 == 2))
                .count();
            assert!(count > 30, "quadrant {quadrant} has only {count}");
        }
    }
}
