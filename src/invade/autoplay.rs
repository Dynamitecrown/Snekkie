//! A screen-only automatic pilot for deterministic regression tests,
//! pacing estimates and screenshots. It is not a player-facing feature.
//!
//! It flies like a player: it turns the nose, thrusts, brakes and coasts.
//! Every reaction it picks one of nine plans (eight directions or holding
//! still), predicts the momentum-limited path each would fly, and scores the
//! danger along it, including drifting asteroids.

use super::arsenal::WeaponKind;
use super::content::{ASPECT, Rig, WORLD_H, WORLD_W};
use super::rng::Rng;
use super::run::{Input, InvadeRun, Phase, SCAN_WIDTH, STEP};
use super::upgrades::{LevelChoice, Offer};

#[derive(Debug, Clone, Copy)]
pub struct Skill {
    pub reaction: f32,
    /// Small navigation errors, in radians of heading; targeting uses the
    /// real automatic aim.
    pub aim_error: f32,
    pub lookahead: f32,
    pub lapse: f32,
}

impl Skill {
    pub const EXPERT: Skill = Skill { reaction: 0.08, aim_error: 0.05, lookahead: 1.6, lapse: 0.0 };
    pub const TYPICAL: Skill = Skill { reaction: 0.25, aim_error: 0.3, lookahead: 1.0, lapse: 0.04 };
}

pub struct Autoplay {
    skill: Skill,
    rng: Rng,
    /// The chosen plan.
    plan: Plan,
    timer: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Plan {
    /// Fly in this visual direction at top speed.
    Fly((f32, f32)),
    /// Keep the current momentum without thrust.
    Coast,
    /// Brake to a stop.
    Stop,
}

/// Seconds between predicted path samples.
const SAMPLE: f32 = 0.08;
/// Must match the model's flight constants.
const COAST_DRAG: f32 = 0.8;
const BRAKE_FACTOR: f32 = 2.5;

fn nose(heading: f32) -> (f32, f32) {
    let (sin, cos) = heading.sin_cos();
    (sin, -cos)
}

fn heading_of(v: (f32, f32)) -> f32 {
    v.0.atan2(-v.1)
}

fn turn_between(from: f32, to: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let mut d = (to - from).rem_euclid(tau);
    if d > std::f32::consts::PI {
        d -= tau;
    }
    d
}

fn length(v: (f32, f32)) -> f32 {
    v.0.hypot(v.1)
}

impl Autoplay {
    pub fn new(skill: Skill, seed: u64) -> Autoplay {
        Autoplay { skill, rng: Rng::new(seed ^ 0xA070), plan: Plan::Stop, timer: 0.0 }
    }

    pub fn input(&mut self, run: &InvadeRun) -> Input {
        self.timer -= STEP;
        if self.timer <= 0.0 {
            self.timer = self.skill.reaction;
            let careless = self.rng.chance(self.skill.lapse);
            self.plan = self.plan(run, careless);
        }
        let p = run.player();
        let boost = if run.rig() == Rig::Bastion && p.mobility_active > 0.0 { 2.2 } else { 1.0 };
        let mut input =
            steer_input(self.plan, p.heading, p.velocity, run.stats().move_speed * boost, run.rig().flight().turn_rate);
        input.fire = true;
        let danger = danger_along(run, &track(run, Plan::Coast, 0.4), 0.4);
        let ahead = straight(run, DASH_REACH, 0.3);
        let clear_ahead = danger_along(run, &ahead, 0.3) == 0.0;
        input.special = run.phase() == Phase::Combat
            && p.charges > 0
            && match run.rig() {
                Rig::Probe => run.visible_enemies().next().is_some(),
                Rig::Ghost => danger > 0.0 && clear_ahead,
                Rig::Bastion => danger > 0.0,
            };
        input.mobility = p.mobility_cooldown <= 0.0
            && danger > 0.0
            && match run.rig() {
                Rig::Bastion => true,
                Rig::Probe | Rig::Ghost => clear_ahead,
            };
        input
    }

    pub fn choose(&mut self, run: &InvadeRun) -> usize {
        use super::upgrades::UpgradeId::*;
        let preference = [Payload, ClockBoost, Fork, Pierce, Fragment, HeavyPackets, Guidance, CacheShield, WarmBackup];
        let offers = run.offers();
        if run.integrity() <= 2
            && let Some(i) =
                offers.iter().position(|o| matches!(o, LevelChoice::Module(Offer::Upgrade(WarmBackup) | Offer::Patch)))
        {
            return i;
        }
        if let Some(i) = offers
            .iter()
            .position(|o| matches!(o, LevelChoice::Weapon(WeaponKind::Orbit | WeaponKind::Pulse | WeaponKind::Arc)))
        {
            return i;
        }
        for wanted in preference {
            if let Some(i) = offers.iter().position(|o| *o == LevelChoice::Module(Offer::Upgrade(wanted))) {
                return i;
            }
        }
        0
    }

    fn plan(&mut self, run: &InvadeRun, careless: bool) -> Plan {
        let p = run.player();
        let horizon = self.skill.lookahead;
        let center = (p.x, p.y + 0.5);
        let orb =
            run.orbs().iter().min_by(|a, b| distance(center, (a.x, a.y)).total_cmp(&distance(center, (b.x, b.y))));
        let top = run.stats().move_speed;
        let mut best = (f32::MAX, Plan::Stop);
        let options =
            (0..8).map(|i| Plan::Fly(nose(i as f32 * std::f32::consts::FRAC_PI_4))).chain([Plan::Coast, Plan::Stop]);
        for option in options {
            let path = track(run, option, horizon);
            let end = path.last().map_or((p.x, p.y), |s| (s.1, s.2));
            let end_center = (end.0, end.1 + 0.5);
            let danger = if careless { 0.0 } else { danger_along(run, &path, horizon) };
            let nearest = run.visible_enemies().map(|e| distance(end_center, e.center())).fold(40.0f32, f32::min);
            let close = (14.0 - nearest).max(0.0);
            let edge = end.0.min(WORLD_W - end.0).min(end.1 * ASPECT).min((WORLD_H - 1.0 - end.1) * ASPECT);
            let rock =
                run.asteroids().iter().map(|a| distance(end_center, (a.x, a.y)) - a.radius).fold(40.0f32, f32::min);
            let pickup = orb.map_or(0.0, |orb| distance(end_center, (orb.x, orb.y)));
            let boss = run
                .visible_enemies()
                .filter(|e| matches!(e.role, super::run::Role::Boss(_) | super::run::Role::Miniboss(_)))
                .map(|e| distance(end_center, e.center()))
                .fold(24.0f32, f32::min);
            // Turning costs time, so prefer plans along the current flight,
            // and a moving ship is harder to hit than a parked one.
            let turning = match option {
                Plan::Fly(d) => turn_between(p.heading, heading_of(d)).abs(),
                _ => 0.0,
            };
            let end_speed = path.last().map_or(0.0, |s| s.3);
            let parked = (1.0 - end_speed / (0.5 * top)).max(0.0);
            let cost = danger * 100.0
                + parked * 2.0
                + close * 2.0
                + 8.0 / (edge + 2.0)
                + (8.0 - rock).max(0.0) * 1.5
                + pickup * 0.6
                + (boss - 24.0).max(0.0) * 0.3
                + turning * 0.8;
            if cost < best.0 {
                best = (cost, option);
            }
        }
        match best.1 {
            Plan::Fly(d) => {
                let wobble = self.rng.range(-self.skill.aim_error, self.skill.aim_error);
                Plan::Fly(nose(heading_of(d) + wobble))
            }
            plan => plan,
        }
    }
}

/// How far Dash, Blink and Slip carry the ship, roughly.
const DASH_REACH: f32 = 12.0;

fn distance(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).hypot((a.1 - b.1) * ASPECT)
}

/// Flight controls shared by the actual pilot and its prediction. Turning
/// continually follows velocity error; reversing uses the real brake.
fn steer_input(plan: Plan, heading: f32, velocity: (f32, f32), top: f32, turn_rate: f32) -> Input {
    let want = match plan {
        Plan::Fly(d) => (d.0 * top, d.1 * top),
        Plan::Coast => velocity,
        Plan::Stop => (0.0, 0.0),
    };
    let error = (want.0 - velocity.0, want.1 - velocity.1);
    let speed = length(velocity);
    let small = 0.08 * top;
    let mut input = Input::default();
    if length(error) > small {
        let turn = turn_between(heading, heading_of(error));
        let deadband = turn_rate * STEP * 1.5;
        input.turn_left = turn < -deadband;
        input.turn_right = turn > deadband;
        let backward = velocity.0 * error.0 + velocity.1 * error.1 < -0.5 * speed * length(error);
        if backward && speed > small {
            input.brake = true;
        } else if turn.abs() < 0.45 {
            input.thrust = true;
        }
    } else if let Plan::Fly(d) = plan {
        let turn = turn_between(heading, heading_of(d));
        let deadband = turn_rate * STEP * 1.5;
        input.turn_left = turn < -deadband;
        input.turn_right = turn > deadband;
    }
    input
}

/// The predicted path, as (time, x, y, speed) samples, for a plan.
/// Integrate the same controls, turn rate, acceleration, drag, Boost expiry
/// and wall response at STEP; sample it sparsely for hazard scoring.
fn track(run: &InvadeRun, plan: Plan, horizon: f32) -> Vec<(f32, f32, f32, f32)> {
    let p = run.player();
    let flight = run.rig().flight();
    let base = run.stats().move_speed;
    let half = run.rig().width() / 2.0;
    let (mut x, mut y, mut v, mut heading) = (p.x, p.y, p.velocity, p.heading);
    let mut active = p.mobility_active;
    let mut samples = vec![(0.0, x, y, length(v))];
    let steps = (horizon / STEP).ceil() as usize;
    let stride = (SAMPLE / STEP).round() as usize;
    for step in 1..=steps {
        let input_boost = if run.rig() == Rig::Bastion && active > 0.0 { 2.2 } else { 1.0 };
        let input = steer_input(plan, heading, v, base * input_boost, flight.turn_rate);
        active = (active - STEP).max(0.0);
        let boost = if run.rig() == Rig::Bastion && active > 0.0 { 2.2 } else { 1.0 };
        let top = base * boost;
        let accel = base / flight.spool * boost;
        let turn = f32::from(i8::from(input.turn_right) - i8::from(input.turn_left));
        heading = (heading + turn * flight.turn_rate * STEP).rem_euclid(std::f32::consts::TAU);
        let before = v;
        if input.brake {
            let speed = length(v);
            if speed > 0.0 {
                let slower = (speed - accel * BRAKE_FACTOR * STEP).max(0.0) / speed;
                v = (v.0 * slower, v.1 * slower);
            }
        } else if input.thrust {
            let n = nose(heading);
            v = (v.0 + n.0 * accel * STEP, v.1 + n.1 * accel * STEP);
        } else {
            let drag = 1.0 - COAST_DRAG * STEP;
            v = (v.0 * drag, v.1 * drag);
        }
        let speed = length(v);
        let cap = top.max(length(before) * (1.0 - 3.0 * STEP));
        if speed > cap {
            v = (v.0 * cap / speed, v.1 * cap / speed);
        }
        x += v.0 * STEP;
        y += v.1 / ASPECT * STEP;
        if x < half {
            x = half;
            v.0 = v.0.max(0.0);
        } else if x > WORLD_W - half {
            x = WORLD_W - half;
            v.0 = v.0.min(0.0);
        }
        if y < 0.0 {
            y = 0.0;
            v.1 = v.1.max(0.0);
        } else if y > WORLD_H - 1.0 {
            y = WORLD_H - 1.0;
            v.1 = v.1.min(0.0);
        }
        if step % stride == 0 || step == steps {
            samples.push((step as f32 * STEP, x, y, length(v)));
        }
    }
    samples
}

/// A straight hop along the nose, for checking where a dash would land.
fn straight(run: &InvadeRun, reach: f32, horizon: f32) -> Vec<(f32, f32, f32, f32)> {
    let p = run.player();
    let n = p.nose();
    let half = run.rig().width() / 2.0;
    let steps = (horizon / SAMPLE).ceil() as usize;
    (0..=steps)
        .map(|i| {
            let f = i as f32 / steps as f32;
            let t = f * horizon;
            let x = (p.x + n.0 * reach * f).clamp(half, WORLD_W - half);
            let y = (p.y + n.1 * reach * f / ASPECT).clamp(0.0, WORLD_H - 1.0);
            (t, x, y, reach / horizon)
        })
        .collect()
}

fn danger_along(run: &InvadeRun, path: &[(f32, f32, f32, f32)], horizon: f32) -> f32 {
    let half = run.rig().width() / 2.0 + 0.4;
    let mut danger = 0.0;
    for &(t, px, py, _) in path {
        let weight = 1.0 + horizon - t;
        for h in run.hostiles() {
            let (hx, hy) = (h.x + h.vx * t, h.y + h.vy * t);
            if (hy - (py + 0.5)).abs() < 0.9 && (hx - px).abs() < half {
                danger += weight;
            }
        }
        for e in run.enemies() {
            let ex = e.x + e.velocity.0 * t;
            let ey = e.y + e.velocity.1 / ASPECT * t;
            if px + half > ex - 1.0 && px - half < ex + e.w + 1.0 && py + 1.0 > ey - 0.5 && py < ey + e.h + 0.5 {
                danger += 3.0 * weight;
            }
        }
        for a in run.asteroids() {
            let ax = a.x + a.velocity.0 * t;
            let ay = a.y + a.velocity.1 / ASPECT * t;
            if distance((px, py + 0.5), (ax, ay)) < a.radius + half + 0.6 {
                danger += 3.0 * weight;
            }
        }
        for scan in run.scans() {
            if t >= scan.warn - 0.1
                && t <= scan.warn.max(0.0) + scan.active + 0.1
                && py + 1.0 > scan.top
                && py < scan.bottom
                && (scan.x - px).abs() < SCAN_WIDTH / 2.0 + half
            {
                danger += 2.0 * weight;
            }
        }
        for mark in run.marks() {
            if t >= mark.warn && distance((px, py + 0.5), (mark.x, mark.y)) < 5.0 {
                danger += weight;
            }
        }
        for c in run.curtains() {
            let y = c.y + c.speed * (t - c.warn.max(0.0)).max(0.0);
            let safe = px - half >= c.gap_left && px + half <= c.gap_left + c.gap_width;
            if t >= c.warn && px + half > c.left && px - half < c.right && (y - py - 0.5).abs() < 1.0 && !safe {
                danger += 3.0 * weight;
            }
        }
    }
    danger
}

pub fn play(
    run: &mut InvadeRun,
    player: &mut Autoplay,
    max_seconds: f32,
    mut done: impl FnMut(&InvadeRun) -> bool,
) -> f32 {
    let mut seconds = 0.0;
    while seconds < max_seconds && !done(run) {
        match run.phase() {
            Phase::Over => break,
            Phase::Upgrade => {
                let choice = player.choose(run);
                run.choose(choice);
            }
            _ => {
                let input = player.input(run);
                run.step(input);
                seconds += STEP;
            }
        }
    }
    seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicted_turning_braking_and_boost_paths_match_actual_flight() {
        for rig in Rig::ALL {
            for heading in [0.0, 1.2, 3.0] {
                for (plan, velocity) in [
                    (Plan::Fly((1.0, 0.0)), (0.0, 0.0)),
                    (Plan::Fly((-1.0, 0.0)), (12.0, -5.0)),
                    (Plan::Fly((0.0, -1.0)), (-8.0, 6.0)),
                    (Plan::Stop, (12.0, 3.0)),
                    (Plan::Coast, (-5.0, -14.0)),
                ] {
                    let mut run = InvadeRun::new(1, rig, [], 0);
                    run.skip_to_wave(1);
                    run.clear_asteroids();
                    run.set_immortal();
                    run.set_flight(heading, velocity);
                    if rig == Rig::Bastion {
                        run.step(Input { mobility: true, ..Input::default() });
                    }
                    let horizon = 1.6;
                    let predicted = track(&run, plan, horizon);
                    for _ in 0..(horizon / STEP).ceil() as usize {
                        let p = run.player();
                        let boost = if rig == Rig::Bastion && p.mobility_active > 0.0 { 2.2 } else { 1.0 };
                        let input = steer_input(
                            plan,
                            p.heading,
                            p.velocity,
                            run.stats().move_speed * boost,
                            rig.flight().turn_rate,
                        );
                        run.step(input);
                    }
                    let end = predicted.last().unwrap();
                    assert!(
                        distance((end.1, end.2), (run.player().x, run.player().y)) < 0.001,
                        "{rig:?} heading {heading} {plan:?}: predicted {:?} actual {:?}",
                        (end.1, end.2),
                        (run.player().x, run.player().y)
                    );
                    assert!((end.3 - run.player().speed()).abs() < 0.001);
                }
            }
        }
    }
}
