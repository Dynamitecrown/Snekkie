//! One Invade run: a deterministic simulation stepped at a fixed rate with
//! explicit input. It never sees sessions, terminals, the clipboard or egui;
//! the same seed and the same inputs always produce the same run.
//!
//! Coordinates are world cells: x is a column (0 to 240, left to right)
//! and y a row (0 to 84, top to bottom). An 80 x 28 camera stays centered
//! on the ship. Positions are fractional so things move smoothly between
//! character cells. Ships fly: they turn at a limited rate, thrust along
//! their nose and coast. Velocities and headings are in visual space, where
//! a row is `ASPECT` units tall; heading 0 points up and grows clockwise.

use std::collections::BTreeSet;

use super::arsenal::{Loadout, WeaponKind, WeaponSpec, WeaponState, acquire_weapon, initial_weapons};
use super::content::{
    ARENA_H, ARENA_W, ASPECT, BossKind, DeviceKind, EncounterKind, FRAGMENT_DEVICES, Flight, GUARD_BASE_HP,
    MinibossKind, Rig, Unlock, WORLD_H, WORLD_W, boss_hp, device_flight, device_hp, encounter_for, fire_interval,
    flight_speed, hostile_speed, level, miniboss_hp, pattern_pace, spawn_interval, telegraph_interval, wave_roster,
};
use super::rng::{Rng, STREAM_ASTEROIDS, STREAM_COMBAT, STREAM_OFFERS};
use super::upgrades::{
    Build, CACHE_SHIELD_RECHARGE, Card, FEEDBACK_PER_SECOND, LevelChoice, Offer, OfferContext, PIERCE_RETAIN,
    PIVOT_CHAIN_DAMAGE, SLIP_DISTANCE, SPARE_PARTS_INTERVAL, Stats, UpgradeId, describe_level, level_offers, stats,
};

/// The fixed simulation step, in seconds.
pub const STEP: f32 = 1.0 / 120.0;
/// The initial top of the player's one-row ship, near the arena center.
pub const PLAYER_START_Y: f32 = WORLD_H / 2.0;
pub const MAX_INTEGRITY: u8 = 5;
pub const START_COUNTDOWN: f32 = 3.0;
pub const RESUME_COUNTDOWN: f32 = 1.0;
pub const HOSTILE_CAP: usize = 18;
pub const BOSS_HOSTILE_CAP: usize = 24;
/// Aimed attacks and scans warn at least this long before they commit.
pub const TELEGRAPH: f32 = 1.15;
pub const SCAN_WIDTH: f32 = 3.0;
pub const CURTAIN_MIN_GAP: f32 = 9.0;
pub const CURTAIN_MAX_SHIFT: f32 = 12.0;

const MAX_SHOTS: usize = 96;
const MAX_FRAGMENTS: usize = 48;
const MAX_EFFECTS: usize = 160;
const SHOT_SPEED: f32 = 38.0;
const FORK_ANGLE: f32 = 12.0;
const FRAGMENT_SPEED: f32 = 34.0;
const FRAGMENT_RANGE: f32 = 12.0;
const PIVOT_SPEED: f32 = 46.0;
/// How far, in visual units, Pivot can jump between devices.
const PIVOT_REACH: f32 = 20.0;
const GUIDANCE_CONE: f32 = 8.0;
const GUIDANCE_TURN: f32 = 1.6;
const GUIDANCE_RANGE: f32 = 40.0;
const SCAN_ACTIVE: f32 = 0.45;
pub const SWARM_CAP: usize = 24;
const OPENING_SHIPS: usize = 4;
const ENTRY_SPEED: f32 = 18.0;
/// How fast a boss or miniboss frame trails the camera, below every rig's
/// slowest movement so the ship can reposition but never leave it behind.
const FRAME_SPEED: f32 = 12.0;
/// How far that frame may lag sideways or above the camera before it is
/// carried along, so a fast rig can never leave a boss out of view.
const FRAME_LAG_X: f32 = 16.0;
const FRAME_LAG_UP: f32 = 1.0;
/// Rows kept between a lagging formation and the ship at the view's middle.
const FRAME_CLEARANCE: f32 = 2.0;
/// Columns of a sideways-lagging formation that always stay in view.
const FRAME_VISIBLE: f32 = 8.0;
const FIRST_SHOT_DELAY: f32 = 1.2;
const INTERMISSION: f32 = 2.0;
pub const MAX_XP_ORBS: usize = 512;
pub const XP_MAGNET_RADIUS: f32 = 7.0;
pub const XP_PICKUP_RADIUS: f32 = 1.5;
const BANNER: f32 = 1.6;
const DASH_TIME: f32 = 0.1;
const BURST_GAP: f32 = 0.12;
const CURTAIN_WARN: f32 = 1.2;
const STREAM_TIME: f32 = 2.0;
const STREAM_GAP: f32 = 0.2;
const STREAM_SWEEP: f32 = 70.0;
const RECORD_GAP: f32 = 0.45;
const SCAN_ORDER_GAP: f32 = 0.55;
const SENTINEL_FIRST_SCAN: f32 = 1.9;
const HUNTER_SPACING: f32 = 19.5;
const HUNTER_DWELL: f32 = 1.8;
/// A Router's packets fly this many degrees either side of its mark.
const SPREAD_ANGLE: f32 = 18.0;
/// Projectiles can graze a hitbox edge this much without counting.
const FORGIVENESS: f32 = 0.2;
/// Coasting ships lose this fraction of their speed per second.
const COAST_DRAG: f32 = 0.8;
/// Braking slows a ship this many times faster than thrust speeds it up.
const BRAKE_FACTOR: f32 = 2.5;
/// Speed above the cap (after a Boost or a Dash) bleeds off this fast.
const OVERSPEED_DECAY: f32 = 3.0;
/// Bastion's Boost multiplies thrust and top speed for one second.
const BOOST: f32 = 2.2;
const DASH_DISTANCE: f32 = 12.0;
const BLINK_DISTANCE: f32 = 14.0;
/// Ordinary ships thrust only when the nose is this close to where they
/// want to go, so they visibly swing around before accelerating.
const THRUST_CONE: f32 = 0.9;
/// Arrivals and recycled ships come in at this fraction of top speed.
const ENTRY_FRACTION: f32 = 0.6;
/// A ship this far from the player returns offscreen.
const RECYCLE_DISTANCE: f32 = 110.0;
/// The asteroid field: (radius in visual units, how many) for each size.
const ASTEROID_SIZES: [(f32, usize); 3] = [(5.0, 4), (3.5, 5), (2.2, 3)];
pub const MAX_ASTEROIDS: usize = 12;
const ASTEROID_MIN_SPEED: f32 = 1.5;
const ASTEROID_MAX_SPEED: f32 = 5.0;
/// Rocks clear out of the view this fast during boss encounters.
const ASTEROID_CLEAR_SPEED: f32 = 14.0;
const ASTEROID_CLEAR_PUSH: f32 = 24.0;
/// A ship bouncing off a rock keeps this much of its approach speed.
const ROCK_BOUNCE: f32 = 0.5;
/// Ships steer to keep this much room between their center and a rock.
const ROCK_CLEARANCE: f32 = 3.0;
const HP_EPSILON: f32 = 1e-3;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Input {
    /// Turn the nose counter-clockwise (A or Left).
    pub turn_left: bool,
    /// Turn the nose clockwise (D or Right).
    pub turn_right: bool,
    /// Accelerate along the nose (W or Up).
    pub thrust: bool,
    /// Slow down without reversing (S or Down); overrides thrust.
    pub brake: bool,
    pub fire: bool,
    /// A fresh press of the special key; only acted on during combat.
    pub special: bool,
    /// A fresh Shift press, independent of the X special.
    pub mobility: bool,
    /// Explicit arena target for tests/utilities. None automatically aims
    /// at the nearest visible enemy, retaining the last aim if none exist.
    pub aim: Option<(f32, f32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Defeated,
    Closed,
    Restarted,
    AppClosed,
}

impl EndReason {
    pub fn id(self) -> &'static str {
        match self {
            EndReason::Defeated => "defeated",
            EndReason::Closed => "closed",
            EndReason::Restarted => "restarted",
            EndReason::AppClosed => "app_closed",
        }
    }

    pub fn from_id(id: &str) -> Option<EndReason> {
        [EndReason::Defeated, EndReason::Closed, EndReason::Restarted, EndReason::AppClosed]
            .into_iter()
            .find(|r| r.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            EndReason::Defeated => "Integrity lost",
            EndReason::Closed => "Tab closed",
            EndReason::Restarted => "Restarted",
            EndReason::AppClosed => "Snekkie closed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Combat,
    /// Between encounters; ships still move and packets still fly.
    Intermission {
        remaining: f32,
    },
    /// Waiting for the player to choose one of [`InvadeRun::offers`].
    Upgrade,
    Over,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Unlocked(Unlock),
    /// An encounter was cleared: the wave number.
    Completed(u32),
    /// A collected XP threshold opened a choice for this level.
    Leveled(u32),
    Damaged(u8),
    Absorbed,
    Breach,
    Defeated,
}

/// What a finished run reports, once.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub rig: Rig,
    pub score: u64,
    pub completed_wave: u32,
    /// Active simulation seconds, excluding pauses, countdowns and choices.
    pub duration: f64,
    pub seed: u64,
    pub reason: EndReason,
    /// Devices compromised this run.
    pub compromised: u64,
    pub unlocks: Vec<Unlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Device(DeviceKind),
    Guard,
    Miniboss(MinibossKind),
    Boss(BossKind),
}

#[derive(Debug, Clone)]
pub struct Enemy {
    pub id: u32,
    pub role: Role,
    /// Top-left corner.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub hp: f32,
    pub max_hp: f32,
    pub alive: bool,
    /// Seconds left of the hit flash.
    pub flash: f32,
    /// Warning that this device is about to attack.
    pub telegraphing: bool,
    /// Radians: 0 points up, growing clockwise.
    pub heading: f32,
    /// Visual units per second.
    pub velocity: (f32, f32),
    pub thrusting: bool,
    pool: Option<usize>,
    credit: bool,
    score: u64,
    /// Offset from the formation origin.
    slot: (f32, f32),
    cooldown: f32,
    previous: (f32, f32),
    entry: Option<(f32, f32)>,
    entered: bool,
}

impl Enemy {
    pub fn nose(&self) -> (f32, f32) {
        nose(self.heading)
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Below half health, a Firewall's armor shows as broken.
    pub fn damaged(&self) -> bool {
        self.hp < self.max_hp / 2.0
    }

    pub fn visible(&self) -> bool {
        self.visible_in((0.0, 0.0))
    }
    pub fn visible_in(&self, camera: (f32, f32)) -> bool {
        self.x + self.w > camera.0
            && self.x < camera.0 + ARENA_W
            && self.y + self.h > camera.1
            && self.y < camera.1 + ARENA_H
    }
}

#[derive(Debug, Clone, Copy)]
struct Pool {
    hp: f32,
    alive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotKind {
    Primary,
    Fork,
    Fragment,
    Pivot,
}

#[derive(Debug, Clone)]
pub struct Shot {
    pub kind: ShotKind,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub damage: f32,
    pierce: u8,
    hit: Vec<u32>,
    travelled: f32,
    range: f32,
    guided: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostileKind {
    /// A single packet fired at a marked position (ships, sentries, guards).
    Straight,
    /// Router diagonals and fans.
    Diagonal,
    /// Aimed at a recorded position.
    Aimed,
    /// Root Controller's sweeping streams.
    Stream,
}

impl HostileKind {
    pub fn glyph(self) -> char {
        match self {
            HostileKind::Straight => 'o',
            HostileKind::Diagonal => 'x',
            HostileKind::Aimed => 'o',
            HostileKind::Stream => '*',
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Hostile {
    pub kind: HostileKind,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
}

/// A column that is marked, then scanned top to bottom.
#[derive(Debug, Clone, Copy)]
pub struct Scan {
    /// Center column.
    pub x: f32,
    pub top: f32,
    pub bottom: f32,
    /// Seconds of warning left; scanning once this reaches zero.
    pub warn: f32,
    pub active: f32,
    /// Sentinel IDS announces its scan order.
    pub label: Option<u8>,
    source: Option<u32>,
    hit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkAttack {
    /// Three packets in quick succession.
    Burst,
    /// One packet from each end of the source at once.
    Pair,
    /// Scan the marked column.
    Scan,
    /// An ordinary ship's single packet at the marked spot.
    Shot,
    /// A Router's two packets either side of the marked spot.
    Spread,
    /// Root Controller's two sweeping streams, which start at the mark.
    Stream,
}

/// A recorded position an attack is committed to.
#[derive(Debug, Clone, Copy)]
pub struct Mark {
    pub x: f32,
    pub y: f32,
    pub warn: f32,
    source: u32,
    attack: MarkAttack,
}

impl Mark {
    /// An ordinary ship's shot, drawn more quietly than a burst or scan.
    pub fn is_shot(&self) -> bool {
        matches!(self.attack, MarkAttack::Shot | MarkAttack::Spread)
    }
}

#[derive(Debug, Clone, Copy)]
struct Burst {
    source: u32,
    target: (f32, f32),
    left: u8,
    timer: f32,
}

/// A falling wall of packets with one safe gap.
#[derive(Debug, Clone, Copy)]
pub struct Curtain {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
    pub y: f32,
    pub gap_left: f32,
    pub gap_width: f32,
    pub warn: f32,
    pub speed: f32,
    hit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    /// A device breaking apart.
    Burst,
    /// A packet landing.
    Spark,
    /// Pivot jumping between devices.
    Chain,
    /// A hit soaked up by a shield.
    Absorb,
    /// The player losing integrity.
    Damage,
    /// A radial pulse centered on the rig.
    Pulse,
}

#[derive(Debug, Clone, Copy)]
pub struct Effect {
    pub kind: EffectKind,
    pub x: f32,
    pub y: f32,
    pub to: (f32, f32),
    pub ttl: f32,
    pub life: f32,
    pub radius: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct XpOrb {
    pub x: f32,
    pub y: f32,
    pub value: u64,
}

#[derive(Debug, Clone)]
pub struct Orbital {
    pub x: f32,
    pub y: f32,
    angle: f32,
    radius: f32,
    speed: f32,
    remaining: f32,
    damage: f32,
    hit: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct Player {
    /// Center column.
    pub x: f32,
    /// Top of the one-row ship.
    pub y: f32,
    /// Unit direction in visual space: (columns, rows times ASPECT).
    pub aim: (f32, f32),
    pub mobility_cooldown: f32,
    pub mobility_active: f32,
    /// Radians: 0 points up, growing clockwise.
    pub heading: f32,
    /// Visual units per second.
    pub velocity: (f32, f32),
    pub thrusting: bool,
    pub braking: bool,
    /// Seconds left of post-hit invulnerability.
    pub invulnerable: f32,
    /// Seconds left of Slip or Barrier protection.
    pub protection: f32,
    pub barrier: bool,
    pub charges: u8,
    /// Seconds accumulated toward the next charge.
    pub recharge: f32,
    pub shield_ready: bool,
    dash: Option<Dash>,
    fire_cooldown: f32,
    feedback: [f64; FEEDBACK_PER_SECOND],
    shield_used_at: u32,
    spare_parts_from: Option<u32>,
}

impl Player {
    /// Unit vector along the nose, in visual space.
    pub fn nose(&self) -> (f32, f32) {
        nose(self.heading)
    }
    pub fn speed(&self) -> f32 {
        self.velocity.0.hypot(self.velocity.1)
    }
}

#[derive(Debug, Clone, Copy)]
struct Dash {
    from: (f32, f32),
    to: (f32, f32),
    elapsed: f32,
    duration: f32,
    /// Speed along the nose when the dash ends; None keeps the velocity.
    exit: Option<f32>,
}

/// A drifting rock: indestructible, solid and dangerous to touch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Asteroid {
    pub id: u32,
    /// Center, in world cells.
    pub x: f32,
    pub y: f32,
    /// Visual units: it spans `radius` columns and `radius / ASPECT` rows
    /// either side of its center.
    pub radius: f32,
    /// Visual units per second.
    pub velocity: (f32, f32),
    /// Drawing angle; rocks tumble slowly.
    pub spin: f32,
    spin_rate: f32,
}

#[derive(Debug, Clone, Copy)]
struct Swarm {
    spawned: usize,
    timer: f32,
    fire: f32,
    telegraph: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct Pattern {
    origin: (f32, f32),
    timer: f32,
    step: u32,
    dir: f32,
    phase: f32,
    gap: f32,
    summons: u8,
    relocate: Option<(f32, f32, f32)>,
    station: usize,
    record: u8,
    record_timer: f32,
    record_elapsed: f32,
    stream: f32,
    stream_timer: f32,
    stream_target: Option<(f32, f32)>,
    light: f32,
    total_hp: f32,
}

#[derive(Debug, Clone, Copy)]
enum Encounter {
    None,
    Swarm(Swarm),
    Special(Pattern),
}

pub struct InvadeRun {
    seed: u64,
    rng: Rng,
    rig: Rig,
    build: Build,
    stats: Stats,
    phase: Phase,
    countdown: f32,
    wave: u32,
    completed: u32,
    score: u64,
    integrity: u8,
    player: Player,
    enemies: Vec<Enemy>,
    pools: Vec<Pool>,
    next_id: u32,
    shots: Vec<Shot>,
    new_shots: Vec<Shot>,
    live_fragments: usize,
    live_shots: usize,
    hostiles: Vec<Hostile>,
    scans: Vec<Scan>,
    marks: Vec<Mark>,
    bursts: Vec<Burst>,
    curtains: Vec<Curtain>,
    effects: Vec<Effect>,
    encounter: Encounter,
    encounter_kind: EncounterKind,
    offers: Vec<LevelChoice>,
    loadout: Loadout,
    weapons: Vec<WeaponState>,
    weapon_cooldowns: Vec<f32>,
    orbitals: Vec<Orbital>,
    orbs: Vec<XpOrb>,
    asteroids: Vec<Asteroid>,
    xp: u64,
    xp_level: u32,
    suspended_phase: Option<Phase>,
    offer_count: u64,
    elapsed: f64,
    unlocked: BTreeSet<Unlock>,
    earned: Vec<Unlock>,
    prior_devices: u64,
    compromised: u64,
    events: Vec<Event>,
    banner: Option<(String, f32)>,
    ended: Option<EndReason>,
    finished: bool,
    immortal: bool,
    roster: Vec<DeviceKind>,
    arrivals: Rng,
    player_previous: (f32, f32),
    player_teleported: bool,
}

/// Where along a→b (0 to 1) the segment first touches the rectangle.
fn segment_enters(a: (f32, f32), b: (f32, f32), left: f32, top: f32, right: f32, bottom: f32) -> Option<f32> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [(-dx, a.0 - left), (dx, right - a.0), (-dy, a.1 - top), (dy, bottom - a.1)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                if r > t1 {
                    return None;
                }
                t0 = t0.max(r);
            } else {
                if r < t0 {
                    return None;
                }
                t1 = t1.min(r);
            }
        }
    }
    Some(t0)
}

/// Where a segment first enters a circle, as a fraction of the segment.
/// Measured in visual space; a start inside the circle enters at once.
fn segment_circle(a: (f32, f32), b: (f32, f32), center: (f32, f32), radius: f32) -> Option<f32> {
    let d = (b.0 - a.0, (b.1 - a.1) * ASPECT);
    let f = (a.0 - center.0, (a.1 - center.1) * ASPECT);
    let c = f.0 * f.0 + f.1 * f.1 - radius * radius;
    if c <= 0.0 {
        return Some(0.0);
    }
    let a2 = d.0 * d.0 + d.1 * d.1;
    if a2 <= f32::EPSILON {
        return None;
    }
    let b2 = 2.0 * (f.0 * d.0 + f.1 * d.1);
    let disc = b2 * b2 - 4.0 * a2 * c;
    if disc < 0.0 {
        return None;
    }
    let t = (-b2 - disc.sqrt()) / (2.0 * a2);
    (0.0..=1.0).contains(&t).then_some(t)
}

/// The visual distance from a point to a box, and the nearest box point.
fn box_nearest(p: (f32, f32), left: f32, top: f32, right: f32, bottom: f32) -> (f32, (f32, f32)) {
    let near = (p.0.clamp(left, right), p.1.clamp(top, bottom));
    (visual_distance(p, near), near)
}

/// Unit vector along a heading, in visual space.
fn nose(heading: f32) -> (f32, f32) {
    let (sin, cos) = heading.sin_cos();
    (sin, -cos)
}

/// The heading that points along a visual vector.
fn heading_of(v: (f32, f32)) -> f32 {
    v.0.atan2(-v.1)
}

/// The signed turn from `from` to `to`, in -PI..=PI.
fn turn_between(from: f32, to: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let mut d = (to - from).rem_euclid(tau);
    if d > std::f32::consts::PI {
        d -= tau;
    }
    d
}

fn wrap_heading(heading: f32) -> f32 {
    heading.rem_euclid(std::f32::consts::TAU)
}

/// Cap a velocity at `top`. Speed that was already above the cap before
/// this step (after a Boost or a Dash) bleeds off instead of snapping.
fn limit_speed(v: (f32, f32), top: f32, before: (f32, f32)) -> (f32, f32) {
    let speed = v.0.hypot(v.1);
    let allowed = top.max(before.0.hypot(before.1) * (1.0 - OVERSPEED_DECAY * STEP));
    if speed <= allowed || speed <= f32::EPSILON {
        return v;
    }
    (v.0 * allowed / speed, v.1 * allowed / speed)
}

/// One step of turn-limited, thrust-and-coast flight toward a velocity.
/// Returns the new heading and velocity and whether it thrust.
fn steer_ship(
    heading: f32,
    velocity: (f32, f32),
    want: (f32, f32),
    flight: Flight,
    top: f32,
) -> (f32, (f32, f32), bool) {
    let error = (want.0 - velocity.0, want.1 - velocity.1);
    let goal = if error.0.hypot(error.1) > 0.05 * top { heading_of(error) } else { heading_of(want) };
    let turn = turn_between(heading, goal);
    let step = flight.turn_rate * STEP;
    let heading = wrap_heading(heading + turn.clamp(-step, step));
    let thrust = turn.abs() < THRUST_CONE && error.0.hypot(error.1) > 0.05 * top;
    let mut v = velocity;
    if thrust {
        let n = nose(heading);
        let accel = top / flight.spool;
        v = (v.0 + n.0 * accel * STEP, v.1 + n.1 * accel * STEP);
    } else {
        let drag = 1.0 - COAST_DRAG * STEP;
        v = (v.0 * drag, v.1 * drag);
    }
    (heading, limit_speed(v, top, velocity), thrust)
}

fn aimed(from: (f32, f32), to: (f32, f32), speed: f32) -> (f32, f32) {
    let dx = to.0 - from.0;
    let dy = (to.1 - from.1) * ASPECT;
    let length = (dx * dx + dy * dy).sqrt().max(1e-3);
    (dx / length * speed * ASPECT, dy / length * speed)
}

/// Distance as it looks on screen, where a row is two columns tall.
fn visual_distance(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = (a.1 - b.1) * ASPECT;
    (dx * dx + dy * dy).sqrt()
}

fn unit(vector: (f32, f32)) -> Option<(f32, f32)> {
    let length = vector.0.hypot(vector.1);
    (length.is_finite() && length > 1e-5).then(|| (vector.0 / length, vector.1 / length))
}

fn direction(from: (f32, f32), to: (f32, f32)) -> Option<(f32, f32)> {
    unit((to.0 - from.0, (to.1 - from.1) * ASPECT))
}

fn rotated(vector: (f32, f32), radians: f32) -> (f32, f32) {
    let (sin, cos) = radians.sin_cos();
    (vector.0 * cos - vector.1 * sin, vector.0 * sin + vector.1 * cos)
}

fn velocity(heading: (f32, f32), visual_speed: f32) -> (f32, f32) {
    (heading.0 * visual_speed, heading.1 * visual_speed / ASPECT)
}

fn sprite_size(lines: &[&str]) -> (f32, f32) {
    (lines.iter().map(|l| l.len()).max().unwrap_or(1) as f32, lines.len() as f32)
}

impl InvadeRun {
    /// A run on wave 1, waiting out the three-second start countdown.
    pub fn new(seed: u64, rig: Rig, unlocked: impl IntoIterator<Item = Unlock>, prior_devices: u64) -> InvadeRun {
        Self::new_with_loadout(seed, Loadout { rig, ..Loadout::default() }, unlocked, prior_devices)
    }

    pub fn new_with_loadout(
        seed: u64,
        loadout: Loadout,
        unlocked: impl IntoIterator<Item = Unlock>,
        prior_devices: u64,
    ) -> InvadeRun {
        let rig = loadout.rig;
        let mut build = Build::default();
        loadout.module.apply(&mut build);
        let stats = stats(rig, &build);
        let mut run = InvadeRun {
            seed,
            rng: Rng::stream(seed, STREAM_COMBAT, 0),
            rig,
            build,
            stats,
            phase: Phase::Combat,
            countdown: START_COUNTDOWN,
            wave: 0,
            completed: 0,
            score: 0,
            integrity: MAX_INTEGRITY,
            player: Player {
                x: WORLD_W / 2.0,
                y: PLAYER_START_Y,
                aim: (0.0, -1.0),
                mobility_cooldown: 0.0,
                mobility_active: 0.0,
                heading: 0.0,
                velocity: (0.0, 0.0),
                thrusting: false,
                braking: false,
                invulnerable: 0.0,
                protection: 0.0,
                barrier: false,
                charges: stats.max_charges,
                recharge: 0.0,
                shield_ready: false,
                dash: None,
                fire_cooldown: 0.0,
                feedback: [f64::NEG_INFINITY; FEEDBACK_PER_SECOND],
                shield_used_at: 0,
                spare_parts_from: None,
            },
            enemies: Vec::new(),
            pools: Vec::new(),
            next_id: 1,
            shots: Vec::new(),
            new_shots: Vec::new(),
            live_fragments: 0,
            live_shots: 0,
            hostiles: Vec::new(),
            scans: Vec::new(),
            marks: Vec::new(),
            bursts: Vec::new(),
            curtains: Vec::new(),
            effects: Vec::new(),
            encounter: Encounter::None,
            encounter_kind: EncounterKind::Ordinary,
            offers: Vec::new(),
            loadout,
            weapons: initial_weapons(loadout),
            weapon_cooldowns: vec![0.0],
            orbitals: Vec::new(),
            orbs: Vec::new(),
            asteroids: Vec::new(),
            xp: 0,
            xp_level: 1,
            suspended_phase: None,
            offer_count: 0,
            elapsed: 0.0,
            unlocked: unlocked.into_iter().collect(),
            earned: Vec::new(),
            prior_devices,
            compromised: 0,
            events: Vec::new(),
            banner: None,
            ended: None,
            finished: false,
            immortal: false,
            roster: Vec::new(),
            arrivals: Rng::stream(seed, STREAM_COMBAT ^ 0xA771, 1),
            player_previous: (WORLD_W / 2.0, PLAYER_START_Y),
            player_teleported: false,
        };
        run.seed_asteroids();
        run.start_wave(1);
        run
    }

    /// Scatter the field outside the opening view, from its own stream so
    /// rocks never shift rosters, offers or attack timing.
    fn seed_asteroids(&mut self) {
        let mut rng = Rng::stream(self.seed, STREAM_ASTEROIDS, 0);
        let camera = self.camera();
        for (radius, count) in ASTEROID_SIZES {
            for _ in 0..count {
                for _ in 0..64 {
                    let rows = radius / ASPECT;
                    let at =
                        (rng.range(radius + 1.0, WORLD_W - radius - 1.0), rng.range(rows + 1.0, WORLD_H - rows - 1.0));
                    let (gap, _) = box_nearest(at, camera.0, camera.1, camera.0 + ARENA_W, camera.1 + ARENA_H);
                    let crowded =
                        self.asteroids.iter().any(|a| visual_distance(at, (a.x, a.y)) < a.radius + radius + 4.0);
                    if gap < radius + 6.0 || crowded {
                        continue;
                    }
                    let angle = rng.range(0.0, std::f32::consts::TAU);
                    let speed = rng.range(ASTEROID_MIN_SPEED, ASTEROID_MAX_SPEED);
                    let spin_rate = rng.range(-1.0, 1.0);
                    self.asteroids.push(Asteroid {
                        id: self.asteroids.len() as u32 + 1,
                        x: at.0,
                        y: at.1,
                        radius,
                        velocity: (angle.sin() * speed, -angle.cos() * speed),
                        spin: rng.range(0.0, std::f32::consts::TAU),
                        spin_rate,
                    });
                    break;
                }
            }
        }
    }

    // -- reading ---------------------------------------------------------

    pub fn seed(&self) -> u64 {
        self.seed
    }
    pub fn rig(&self) -> Rig {
        self.rig
    }
    pub fn build(&self) -> &Build {
        &self.build
    }
    pub fn stats(&self) -> &Stats {
        &self.stats
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    /// Seconds left of a start or resume countdown; nothing moves until 0.
    pub fn countdown(&self) -> f32 {
        self.countdown
    }
    /// The wave being played, or just played during an intermission.
    pub fn wave(&self) -> u32 {
        self.wave
    }
    pub fn completed_wave(&self) -> u32 {
        self.completed
    }
    pub fn score(&self) -> u64 {
        self.score
    }
    pub fn integrity(&self) -> u8 {
        self.integrity
    }
    pub fn player(&self) -> &Player {
        &self.player
    }
    pub fn enemies(&self) -> impl Iterator<Item = &Enemy> {
        self.enemies.iter().filter(|e| e.alive)
    }
    /// Camera follows the ship even at a world edge.
    pub fn camera(&self) -> (f32, f32) {
        (self.player.x - ARENA_W / 2.0, self.player.y + 0.5 - ARENA_H / 2.0)
    }
    pub fn visible_enemies(&self) -> impl Iterator<Item = &Enemy> {
        let camera = self.camera();
        self.enemies().filter(move |e| e.visible_in(camera))
    }
    pub fn level(&self) -> u32 {
        self.xp_level
    }
    pub fn experience(&self) -> u32 {
        self.xp.min(u64::from(u32::MAX)) as u32
    }
    pub fn experience_needed(&self) -> u32 {
        12u32.saturating_add(8u32.saturating_mul(self.xp_level - 1))
    }
    pub fn orbs(&self) -> &[XpOrb] {
        &self.orbs
    }
    pub fn asteroids(&self) -> &[Asteroid] {
        &self.asteroids
    }
    pub fn weapons(&self) -> &[WeaponState] {
        &self.weapons
    }
    pub fn orbitals(&self) -> &[Orbital] {
        &self.orbitals
    }
    pub fn shots(&self) -> &[Shot] {
        &self.shots
    }
    pub fn hostiles(&self) -> &[Hostile] {
        &self.hostiles
    }
    pub fn scans(&self) -> &[Scan] {
        &self.scans
    }
    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }
    pub fn curtains(&self) -> &[Curtain] {
        &self.curtains
    }
    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }
    pub fn offers(&self) -> &[LevelChoice] {
        &self.offers
    }
    pub fn encounter(&self) -> EncounterKind {
        self.encounter_kind
    }
    /// What follows the current wave, for announcements.
    pub fn next_encounter(&self) -> EncounterKind {
        encounter_for(self.seed, self.wave + 1)
    }
    /// Active simulation seconds.
    pub fn elapsed(&self) -> f64 {
        self.elapsed
    }
    pub fn compromised(&self) -> u64 {
        self.compromised
    }
    pub fn earned_unlocks(&self) -> &[Unlock] {
        &self.earned
    }
    pub fn is_unlocked(&self, unlock: Unlock) -> bool {
        self.unlocked.contains(&unlock)
    }
    pub fn banner(&self) -> Option<&str> {
        self.banner.as_ref().map(|(text, _)| text.as_str())
    }
    pub fn ended(&self) -> Option<EndReason> {
        self.ended
    }
    /// Still playable: not defeated, closed or replaced.
    pub fn is_unfinished(&self) -> bool {
        self.ended.is_none()
    }
    /// The countdown and the playing field advance; offers and the game
    /// over screen wait for the player.
    pub fn is_advancing(&self) -> bool {
        self.ended.is_none() && matches!(self.phase, Phase::Combat | Phase::Intermission { .. })
    }
    pub fn hostile_cap(&self) -> usize {
        if self.encounter_kind == EncounterKind::Ordinary { HOSTILE_CAP } else { BOSS_HOSTILE_CAP }
    }

    fn offer_context(&self) -> OfferContext<'_> {
        OfferContext {
            rig: self.rig,
            build: &self.build,
            integrity: self.integrity,
            max_integrity: MAX_INTEGRITY,
            charges: self.player.charges,
            wave: self.completed,
        }
    }

    /// The offered upgrades, described for this rig and build.
    pub fn cards(&self) -> Vec<Card> {
        let ctx = self.offer_context();
        self.offers.iter().map(|offer| describe_level(*offer, &ctx, &self.weapons)).collect()
    }

    /// The miniboss or boss's name and remaining health, 0 to 1.
    pub fn boss_health(&self) -> Option<(&'static str, f32)> {
        let Encounter::Special(pattern) = self.encounter else { return None };
        let name = match self.encounter_kind {
            EncounterKind::Ordinary => return None,
            EncounterKind::Miniboss(kind) => kind.name(),
            EncounterKind::Boss(kind) => kind.name(),
        };
        let remaining: f32 = if let Some(pool) = self.pools.first() {
            if pool.alive { pool.hp } else { 0.0 }
        } else {
            self.enemies
                .iter()
                .filter(|e| e.alive && matches!(e.role, Role::Miniboss(_) | Role::Boss(_)))
                .map(|e| e.hp.max(0.0))
                .sum()
        };
        Some((name, (remaining / pattern.total_hp.max(1.0)).clamp(0.0, 1.0)))
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    // -- control ---------------------------------------------------------

    pub fn begin_countdown(&mut self, seconds: f32) {
        if self.is_advancing() {
            self.countdown = self.countdown.max(seconds);
        }
    }

    /// Take offer `index`. False if there is no such offer right now.
    pub fn choose(&mut self, index: usize) -> bool {
        if self.phase != Phase::Upgrade || self.ended.is_some() {
            return false;
        }
        let Some(&offer) = self.offers.get(index) else { return false };
        match offer {
            LevelChoice::Weapon(kind) => {
                if !self.grant_weapon(kind) {
                    return false;
                }
            }
            LevelChoice::Module(offer) => match offer {
                Offer::Upgrade(id) => {
                    if !self.build.add(id) {
                        return false;
                    }
                    self.stats = stats(self.rig, &self.build);
                    match id {
                        UpgradeId::WarmBackup => self.repair(2),
                        UpgradeId::SpareParts => self.player.spare_parts_from = Some(self.completed),
                        UpgradeId::CacheShield => self.player.shield_ready = true,
                        _ => {}
                    }
                }
                Offer::Patch => self.repair(1),
                Offer::Recharge => {
                    self.player.charges = self.stats.max_charges;
                    self.player.recharge = 0.0;
                }
                Offer::DataCache => self.score += 250 * u64::from(self.completed.max(1)),
            },
        }
        self.offers.clear();
        self.phase = self.suspended_phase.unwrap_or(Phase::Combat);
        if !self.enter_level_choice() {
            self.suspended_phase = None;
            self.countdown = RESUME_COUNTDOWN;
        }
        true
    }

    /// End the run and report it. Only the first call reports anything;
    /// a defeat stays a defeat whatever ends the run afterward.
    pub fn finish(&mut self, reason: EndReason) -> Option<Summary> {
        if self.finished {
            return None;
        }
        self.finished = true;
        let reason = *self.ended.get_or_insert(reason);
        self.phase = Phase::Over;
        Some(Summary {
            rig: self.rig,
            score: self.score,
            completed_wave: self.completed,
            duration: self.elapsed,
            seed: self.seed,
            reason,
            compromised: self.compromised,
            unlocks: self.earned.clone(),
        })
    }

    // -- the simulation --------------------------------------------------

    /// Advance one fixed [`STEP`].
    pub fn step(&mut self, input: Input) {
        if !self.is_advancing() {
            return;
        }
        if self.countdown > 0.0 {
            self.countdown = (self.countdown - STEP).max(0.0);
            return;
        }
        self.elapsed += f64::from(STEP);
        self.player_previous = (self.player.x, self.player.y);
        self.player_teleported = false;
        for enemy in &mut self.enemies {
            enemy.previous = (enemy.x, enemy.y);
        }
        if let Some((_, ttl)) = &mut self.banner {
            *ttl -= STEP;
            if *ttl <= 0.0 {
                self.banner = None;
            }
        }
        self.update_player(input);
        self.update_asteroids();
        self.rock_contacts();
        if self.phase == Phase::Over {
            return;
        }
        self.recycle_stragglers();
        if let Phase::Intermission { remaining } = self.phase {
            self.move_shots();
            self.update_orbitals();
            self.update_effects();
            self.update_orbs();
            if self.phase == Phase::Upgrade {
                return;
            }
            let remaining = remaining - STEP;
            if remaining <= 0.0 {
                self.start_wave(self.wave + 1);
            } else {
                self.phase = Phase::Intermission { remaining };
            }
            return;
        }
        for enemy in &mut self.enemies {
            enemy.flash = (enemy.flash - STEP).max(0.0);
        }
        match self.encounter {
            Encounter::Swarm(swarm) => self.update_swarm(swarm),
            Encounter::Special(pattern) => self.update_pattern(pattern),
            Encounter::None => {}
        }
        self.update_marks();
        self.update_bursts();
        self.update_scans();
        self.update_curtains();
        self.move_shots();
        self.update_orbitals();
        self.move_hostiles();
        self.update_effects();
        self.update_telegraph_flags();
        if self.phase == Phase::Combat {
            self.check_contacts();
        }
        if self.phase == Phase::Combat {
            self.check_cleared();
        }
        if self.is_advancing() {
            self.update_orbs();
        }
    }

    fn update_player(&mut self, input: Input) {
        let s = self.stats;
        let combat = self.phase == Phase::Combat;
        let half = self.rig.width() / 2.0;
        let flight = self.rig.flight();
        let turn = f32::from(i8::from(input.turn_right) - i8::from(input.turn_left));
        let p = &mut self.player;
        p.invulnerable = (p.invulnerable - STEP).max(0.0);
        p.protection = (p.protection - STEP).max(0.0);
        p.mobility_cooldown = (p.mobility_cooldown - STEP).max(0.0);
        p.mobility_active = (p.mobility_active - STEP).max(0.0);
        if p.protection == 0.0 {
            p.barrier = false;
        }
        p.heading = wrap_heading(p.heading + turn * flight.turn_rate * STEP);
        let boost = if self.rig == Rig::Bastion && p.mobility_active > 0.0 { BOOST } else { 1.0 };
        let top = s.move_speed * boost;
        let accel = s.move_speed / flight.spool * boost;
        p.braking = input.brake;
        p.thrusting = input.thrust && !input.brake;
        if let Some(dash) = &mut p.dash {
            dash.elapsed += STEP;
            let t = (dash.elapsed / dash.duration).min(1.0);
            p.x = dash.from.0 + (dash.to.0 - dash.from.0) * t;
            p.y = dash.from.1 + (dash.to.1 - dash.from.1) * t;
            if t >= 1.0 {
                if let Some(speed) = dash.exit {
                    let n = nose(p.heading);
                    p.velocity = (n.0 * speed, n.1 * speed);
                }
                p.dash = None;
            }
        } else {
            let before = p.velocity;
            let v = &mut p.velocity;
            if p.thrusting {
                let n = nose(p.heading);
                *v = (v.0 + n.0 * accel * STEP, v.1 + n.1 * accel * STEP);
            } else if p.braking {
                let speed = v.0.hypot(v.1);
                if speed > 0.0 {
                    let slower = (speed - accel * BRAKE_FACTOR * STEP).max(0.0) / speed;
                    *v = (v.0 * slower, v.1 * slower);
                }
            } else {
                let drag = 1.0 - COAST_DRAG * STEP;
                *v = (v.0 * drag, v.1 * drag);
            }
            *v = limit_speed(*v, top, before);
            p.x += v.0 * STEP;
            p.y += v.1 / ASPECT * STEP;
        }
        self.clamp_player();

        self.update_aim(input.aim);

        let p = &mut self.player;
        if input.mobility && combat && p.mobility_cooldown <= 0.0 {
            let dir = p.nose();
            p.mobility_cooldown = self.rig.mobility_recharge();
            match self.rig {
                Rig::Probe => {
                    let to = (
                        (p.x + dir.0 * DASH_DISTANCE).clamp(half, WORLD_W - half),
                        (p.y + dir.1 * DASH_DISTANCE / ASPECT).clamp(0.0, WORLD_H - 1.0),
                    );
                    // The burst leaves the ship flying along its nose.
                    let exit = Some(s.move_speed.max(p.speed()));
                    p.dash = Some(Dash { from: (p.x, p.y), to, elapsed: 0.0, duration: 0.16, exit });
                    p.protection = p.protection.max(0.20);
                    p.mobility_active = 0.16;
                }
                Rig::Ghost => {
                    // A Blink replaces any Slip still in progress and keeps
                    // the ship's momentum.
                    p.dash = None;
                    p.x = (p.x + dir.0 * BLINK_DISTANCE).clamp(half, WORLD_W - half);
                    p.y = (p.y + dir.1 * BLINK_DISTANCE / ASPECT).clamp(0.0, WORLD_H - 1.0);
                    p.protection = p.protection.max(0.25);
                    p.mobility_active = 0.25;
                    self.player_teleported = true;
                }
                Rig::Bastion => p.mobility_active = 1.0,
            }
        }
        if self.player_teleported {
            // Blink can change which visible ship is nearest. Fire and X
            // must use the direction from the destination this same step.
            self.update_aim(input.aim);
        }
        let p = &mut self.player;
        if p.charges < s.max_charges {
            p.recharge += STEP;
            if p.recharge >= s.recharge {
                p.charges += 1;
                p.recharge = 0.0;
            }
        } else {
            p.recharge = 0.0;
        }
        if input.special && combat && p.charges > 0 {
            p.charges -= 1;
            match self.rig {
                Rig::Probe => self.launch(ShotKind::Pivot, self.player.aim, s.pivot_direct, 0, false),
                Rig::Ghost => {
                    let p = &mut self.player;
                    let dir = p.nose();
                    let to = (
                        (p.x + dir.0 * SLIP_DISTANCE).clamp(half, WORLD_W - half),
                        (p.y + dir.1 * SLIP_DISTANCE / ASPECT).clamp(0.0, WORLD_H - 1.0),
                    );
                    p.dash = Some(Dash { from: (p.x, p.y), to, elapsed: 0.0, duration: DASH_TIME, exit: None });
                    p.protection = p.protection.max(s.slip_protection);
                    p.barrier = false;
                }
                Rig::Bastion => {
                    let p = &mut self.player;
                    p.protection = p.protection.max(s.barrier_protection);
                    p.barrier = true;
                }
            }
        }
        self.player.fire_cooldown = (self.player.fire_cooldown - STEP).max(0.0);
        self.fire_weapons(input.fire);
    }

    /// Keep the ship inside the world; a wall stops the momentum into it.
    fn clamp_player(&mut self) {
        let half = self.rig.width() / 2.0;
        let p = &mut self.player;
        if p.x < half {
            p.x = half;
            p.velocity.0 = p.velocity.0.max(0.0);
        } else if p.x > WORLD_W - half {
            p.x = WORLD_W - half;
            p.velocity.0 = p.velocity.0.min(0.0);
        }
        if p.y < 0.0 {
            p.y = 0.0;
            p.velocity.1 = p.velocity.1.max(0.0);
        } else if p.y > WORLD_H - 1.0 {
            p.y = WORLD_H - 1.0;
            p.velocity.1 = p.velocity.1.min(0.0);
        }
    }

    /// Drift, tumble, bounce off the walls and each other, and clear out of
    /// the view while a miniboss or boss is fighting.
    fn update_asteroids(&mut self) {
        let camera = self.camera();
        let clearing = matches!(self.encounter, Encounter::Special(_));
        let view_center = (camera.0 + ARENA_W / 2.0, camera.1 + ARENA_H / 2.0);
        for a in &mut self.asteroids {
            let rows = a.radius / ASPECT;
            if clearing {
                let (gap, _) = box_nearest((a.x, a.y), camera.0, camera.1, camera.0 + ARENA_W, camera.1 + ARENA_H);
                if gap < a.radius + 2.0 {
                    let away = direction(view_center, (a.x, a.y)).unwrap_or((0.0, -1.0));
                    a.velocity.0 += away.0 * ASTEROID_CLEAR_PUSH * STEP;
                    a.velocity.1 += away.1 * ASTEROID_CLEAR_PUSH * STEP;
                }
            }
            let cap = if clearing { ASTEROID_CLEAR_SPEED } else { ASTEROID_MAX_SPEED };
            a.velocity = limit_speed(a.velocity, cap, a.velocity);
            a.x += a.velocity.0 * STEP;
            a.y += a.velocity.1 / ASPECT * STEP;
            a.spin = wrap_heading(a.spin + a.spin_rate * STEP);
            if a.x < a.radius {
                a.x = a.radius;
                a.velocity.0 = a.velocity.0.abs();
            } else if a.x > WORLD_W - a.radius {
                a.x = WORLD_W - a.radius;
                a.velocity.0 = -a.velocity.0.abs();
            }
            if a.y < rows {
                a.y = rows;
                a.velocity.1 = a.velocity.1.abs();
            } else if a.y > WORLD_H - rows {
                a.y = WORLD_H - rows;
                a.velocity.1 = -a.velocity.1.abs();
            }
        }
        // Rocks bounce off each other like billiard balls weighted by size.
        for i in 0..self.asteroids.len() {
            for j in i + 1..self.asteroids.len() {
                let (a, b) = (self.asteroids[i], self.asteroids[j]);
                let reach = a.radius + b.radius;
                let gap = visual_distance((a.x, a.y), (b.x, b.y));
                if gap >= reach {
                    continue;
                }
                let n = direction((a.x, a.y), (b.x, b.y)).unwrap_or((1.0, 0.0));
                let (ma, mb) = (a.radius * a.radius, b.radius * b.radius);
                let push = reach - gap;
                let (pa, pb) = (push * mb / (ma + mb), push * ma / (ma + mb));
                let closing = (a.velocity.0 - b.velocity.0) * n.0 + (a.velocity.1 - b.velocity.1) * n.1;
                let (mut va, mut vb) = (a.velocity, b.velocity);
                if closing > 0.0 {
                    let (ia, ib) = (2.0 * mb / (ma + mb) * closing, 2.0 * ma / (ma + mb) * closing);
                    va = (va.0 - n.0 * ia, va.1 - n.1 * ia);
                    vb = (vb.0 + n.0 * ib, vb.1 + n.1 * ib);
                }
                let first = &mut self.asteroids[i];
                (first.x, first.y) = (first.x - n.0 * pa, first.y - n.1 * pa / ASPECT);
                first.velocity = va;
                let second = &mut self.asteroids[j];
                (second.x, second.y) = (second.x + n.0 * pb, second.y + n.1 * pb / ASPECT);
                second.velocity = vb;
            }
        }
        // Pair separation can push a rock back through a wall after its
        // first bounce. Project the final positions as well so the entire
        // circle stays inside the world on every completed step.
        for a in &mut self.asteroids {
            let rows = a.radius / ASPECT;
            if a.x < a.radius {
                a.x = a.radius;
                a.velocity.0 = a.velocity.0.abs();
            } else if a.x > WORLD_W - a.radius {
                a.x = WORLD_W - a.radius;
                a.velocity.0 = -a.velocity.0.abs();
            }
            if a.y < rows {
                a.y = rows;
                a.velocity.1 = a.velocity.1.abs();
            } else if a.y > WORLD_H - rows {
                a.y = WORLD_H - rows;
                a.velocity.1 = -a.velocity.1.abs();
            }
        }
    }

    /// The ship never passes through a rock: it is pushed out and bounces,
    /// and the touch is a hit unless something protects it.
    fn rock_contacts(&mut self) {
        let mut touched = false;
        for i in 0..self.asteroids.len() {
            let a = self.asteroids[i];
            let (left, top, right, bottom) = self.player_box();
            let (gap, near) = box_nearest((a.x, a.y), left, top, right, bottom);
            if gap >= a.radius {
                continue;
            }
            touched = true;
            let center = (self.player.x, self.player.y + 0.5);
            let n = direction((a.x, a.y), near).or_else(|| direction((a.x, a.y), center)).unwrap_or((0.0, -1.0));
            let push = a.radius - gap + 0.01;
            let p = &mut self.player;
            p.x += n.0 * push;
            p.y += n.1 * push / ASPECT;
            let closing = (p.velocity.0 - a.velocity.0) * n.0 + (p.velocity.1 - a.velocity.1) * n.1;
            if closing < 0.0 {
                let bounce = (1.0 + ROCK_BOUNCE) * closing;
                p.velocity = (p.velocity.0 - n.0 * bounce, p.velocity.1 - n.1 * bounce);
            }
            if let Some(dash) = &mut p.dash {
                // A dash ends where it meets the rock.
                dash.to = (p.x, p.y);
                dash.from = (p.x, p.y);
            }
            self.clamp_player();
        }
        if touched {
            let at = (self.player.x, self.player.y + 0.5);
            if self.hit_player() {
                self.effect(EffectKind::Spark, at, (0.0, 0.0), 0.2);
            }
        }
    }

    /// The first rock a straight path from `from` to `to` meets.
    fn rock_entry(&self, from: (f32, f32), to: (f32, f32)) -> Option<f32> {
        self.asteroids.iter().filter_map(|a| segment_circle(from, to, (a.x, a.y), a.radius)).min_by(f32::total_cmp)
    }

    /// Steering away from rocks ahead, in visual units, for a ship at `at`
    /// heading along `look`. `side` breaks exact head-on ties without RNG.
    fn rock_avoidance(&self, at: (f32, f32), look: (f32, f32), speed: f32, side: f32) -> (f32, f32) {
        let reach = 8.0 + speed * 1.2;
        let mut steer = (0.0f32, 0.0f32);
        for a in &self.asteroids {
            let rel = (a.x - at.0, (a.y - at.1) * ASPECT);
            let room = a.radius + ROCK_CLEARANCE;
            let dist = rel.0.hypot(rel.1);
            if dist < room {
                // Too close: back straight away.
                let away = unit((-rel.0, -rel.1)).unwrap_or((-look.0, -look.1));
                let weight = 2.0 * (1.0 - dist / room) + 1.0;
                steer = (steer.0 + away.0 * weight, steer.1 + away.1 * weight);
                continue;
            }
            let ahead = rel.0 * look.0 + rel.1 * look.1;
            let lateral = rel.0 * -look.1 + rel.1 * look.0;
            if ahead <= 0.0 || ahead > reach || lateral.abs() >= room {
                continue;
            }
            let away = if lateral.abs() < 0.01 { side } else { -lateral.signum() };
            let weight = 2.0 * (1.0 - ahead / reach) * (1.0 - lateral.abs() / room);
            steer = (steer.0 + -look.1 * away * weight, steer.1 + look.0 * away * weight);
        }
        steer
    }

    /// Push an enemy ship out of any rock it overlaps; no damage.
    fn rock_push(&mut self, index: usize) {
        for i in 0..self.asteroids.len() {
            let a = self.asteroids[i];
            let e = &mut self.enemies[index];
            let (gap, near) = box_nearest((a.x, a.y), e.x, e.y, e.x + e.w, e.y + e.h);
            if gap >= a.radius {
                continue;
            }
            let n = direction((a.x, a.y), near).or_else(|| direction((a.x, a.y), e.center())).unwrap_or((0.0, -1.0));
            let push = a.radius - gap + 0.01;
            e.x += n.0 * push;
            e.y += n.1 * push / ASPECT;
            let closing = (e.velocity.0 - a.velocity.0) * n.0 + (e.velocity.1 - a.velocity.1) * n.1;
            if closing < 0.0 {
                e.velocity = (e.velocity.0 - n.0 * closing, e.velocity.1 - n.1 * closing);
            }
        }
    }

    fn update_aim(&mut self, explicit: Option<(f32, f32)>) {
        // Test/utility targets override nearest-visible targeting; empty
        // space retains the previous heading.
        let center = (self.player.x, self.player.y + 0.5);
        let target = explicit.or_else(|| {
            self.enemies
                .iter()
                .filter(|e| e.alive && e.visible_in(self.camera()))
                .min_by(|a, b| {
                    visual_distance(center, a.center())
                        .total_cmp(&visual_distance(center, b.center()))
                        .then(a.id.cmp(&b.id))
                })
                .map(Enemy::center)
        });
        if let Some(heading) = target.and_then(|at| direction(center, at)) {
            self.player.aim = heading;
        }
    }

    fn launch(&mut self, kind: ShotKind, heading: (f32, f32), damage: f32, pierce: u8, guided: bool) {
        if self.shots.len() >= MAX_SHOTS {
            return;
        }
        let speed = if kind == ShotKind::Pivot { PIVOT_SPEED } else { SHOT_SPEED };
        let (vx, vy) = velocity(heading, speed * ASPECT);
        self.shots.push(Shot {
            kind,
            x: self.player.x,
            y: self.player.y + 0.5,
            vx,
            vy,
            damage,
            pierce,
            hit: Vec::new(),
            travelled: 0.0,
            range: 60.0,
            guided,
        });
    }

    fn fire_weapons(&mut self, fire: bool) {
        self.weapon_cooldowns.resize(self.weapons.len(), 0.0);
        for index in 0..self.weapons.len() {
            self.weapon_cooldowns[index] = (self.weapon_cooldowns[index] - STEP).max(0.0);
            if !fire || self.weapon_cooldowns[index] > 0.0 {
                continue;
            }
            let weapon = self.weapons[index];
            let spec = weapon.spec(self.rig, &self.stats);
            self.weapon_cooldowns[index] = spec.cooldown;
            match weapon.kind {
                WeaponKind::Packet | WeaponKind::Scatter | WeaponKind::Rail => {
                    let heading = self.player.aim;
                    for n in 0..spec.projectiles {
                        let angle = if spec.projectiles <= 1 {
                            0.0
                        } else {
                            spec.spread * (f32::from(n) / f32::from(spec.projectiles - 1) - 0.5)
                        };
                        self.weapon_packet(ShotKind::Primary, rotated(heading, angle), spec.damage, spec);
                    }
                    if self.stats.fork_damage.is_some() {
                        for side in [-1.0, 1.0] {
                            self.weapon_packet(
                                ShotKind::Fork,
                                rotated(heading, side * FORK_ANGLE.to_radians()),
                                spec.damage * 0.3,
                                WeaponSpec { pierce: 0, ..spec },
                            );
                        }
                    }
                }
                WeaponKind::Arc => self.arc(spec),
                WeaponKind::Pulse => {
                    let at = (self.player.x, self.player.y + 0.5);
                    self.effect(EffectKind::Pulse, at, at, 0.35);
                    if let Some(effect) = self.effects.last_mut() {
                        effect.radius = spec.radius;
                    }
                    let targets: Vec<_> = self
                        .enemies
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| e.alive)
                        .filter(|(_, e)| {
                            visual_distance(at, (at.0.clamp(e.x, e.x + e.w), at.1.clamp(e.y, e.y + e.h))) <= spec.radius
                        })
                        .map(|(i, _)| i)
                        .collect();
                    for target in targets {
                        self.damage(target, spec.damage);
                    }
                }
                WeaponKind::Orbit => {
                    let room = 32usize.saturating_sub(self.orbitals.len());
                    for n in 0..usize::from(spec.projectiles).min(room) {
                        let angle = std::f32::consts::TAU * n as f32 / f32::from(spec.projectiles);
                        self.orbitals.push(Orbital {
                            x: self.player.x + angle.cos() * spec.radius,
                            y: self.player.y + 0.5 + angle.sin() * spec.radius / ASPECT,
                            angle,
                            radius: spec.radius,
                            speed: spec.speed,
                            remaining: spec.range / spec.speed.max(1.0),
                            damage: spec.damage,
                            hit: Vec::new(),
                        });
                    }
                }
            }
        }
    }

    fn weapon_packet(&mut self, kind: ShotKind, heading: (f32, f32), damage: f32, spec: WeaponSpec) {
        if self.shots.len() >= MAX_SHOTS {
            return;
        }
        let (vx, vy) = velocity(heading, spec.speed);
        self.shots.push(Shot {
            kind,
            x: self.player.x,
            y: self.player.y + 0.5,
            vx,
            vy,
            damage,
            pierce: spec.pierce,
            hit: Vec::new(),
            travelled: 0.0,
            range: spec.range,
            guided: self.stats.guidance,
        });
    }

    fn arc(&mut self, spec: WeaponSpec) {
        let mut at = (self.player.x, self.player.y + 0.5);
        let mut hit = Vec::new();
        for _ in 0..spec.chain_targets {
            let next = self
                .enemies
                .iter()
                .enumerate()
                .filter(|(_, e)| e.alive && e.visible_in(self.camera()) && !hit.contains(&e.id))
                .map(|(i, e)| (visual_distance(at, e.center()), e.id, i))
                .filter(|(d, _, _)| *d <= spec.range)
                .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let Some((_, id, index)) = next else { break };
            let to = self.enemies[index].center();
            self.effect(EffectKind::Chain, at, to, 0.2);
            self.damage(index, spec.damage);
            hit.push(id);
            at = to;
        }
    }

    fn update_orbitals(&mut self) {
        let mut orbitals = std::mem::take(&mut self.orbitals);
        for orb in &mut orbitals {
            let from = (orb.x, orb.y);
            orb.remaining -= STEP;
            orb.angle += orb.speed / orb.radius.max(0.1) * STEP;
            orb.x = self.player.x + orb.angle.cos() * orb.radius;
            orb.y = self.player.y + 0.5 + orb.angle.sin() * orb.radius / ASPECT;
            let start = if self.player_teleported { (orb.x, orb.y) } else { from };
            let hits: Vec<_> = self
                .enemies
                .iter()
                .enumerate()
                .filter(|(_, e)| e.alive && !orb.hit.contains(&e.id))
                .filter(|(_, e)| {
                    segment_enters(
                        (start.0 + e.x - e.previous.0, start.1 + e.y - e.previous.1),
                        (orb.x, orb.y),
                        e.x - 0.3,
                        e.y - 0.15,
                        e.x + e.w + 0.3,
                        e.y + e.h + 0.15,
                    )
                    .is_some()
                })
                .map(|(i, e)| (i, e.id))
                .collect();
            for (i, id) in hits {
                self.damage(i, orb.damage);
                orb.hit.push(id);
            }
        }
        orbitals.retain(|o| o.remaining > 0.0);
        self.orbitals = orbitals;
    }

    fn add_orb(&mut self, at: (f32, f32), value: u32) {
        if value == 0 {
            return;
        }
        if self.orbs.len() >= MAX_XP_ORBS {
            // Merge existing XP rather than deleting a drop. Natural runs
            // are accumulated as u64 so merging u32 drops loses no XP.
            let old = self.orbs.remove(0);
            let nearest = self
                .orbs
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    visual_distance((old.x, old.y), (a.x, a.y)).total_cmp(&visual_distance((old.x, old.y), (b.x, b.y)))
                })
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.orbs[nearest].value += old.value;
        }
        self.orbs.push(XpOrb { x: at.0.clamp(0.0, WORLD_W), y: at.1.clamp(0.0, WORLD_H), value: u64::from(value) });
    }

    fn update_orbs(&mut self) {
        let at = (self.player.x, self.player.y + 0.5);
        let previous = if self.player_teleported { at } else { (self.player_previous.0, self.player_previous.1 + 0.5) };
        let magnet = XP_MAGNET_RADIUS * self.loadout.module.magnet_multiplier();
        let mut gained = 0u64;
        self.orbs.retain_mut(|orb| {
            let from = (orb.x, orb.y);
            let d = visual_distance(from, at);
            if d <= magnet
                && let Some(heading) = direction(from, at)
            {
                let travel = (22.0 * STEP).min(d);
                orb.x += heading.0 * travel;
                orb.y += heading.1 * travel / ASPECT;
            }
            // Relative swept pickup measures actual motion, never Blink's path.
            let rel_from = (from.0 - previous.0, (from.1 - previous.1) * ASPECT);
            let rel_to = (orb.x - at.0, (orb.y - at.1) * ASPECT);
            let delta = (rel_to.0 - rel_from.0, rel_to.1 - rel_from.1);
            let denom = delta.0 * delta.0 + delta.1 * delta.1;
            let t = if denom > 0.0 {
                (-(rel_from.0 * delta.0 + rel_from.1 * delta.1) / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let close = (rel_from.0 + t * delta.0).hypot(rel_from.1 + t * delta.1) <= XP_PICKUP_RADIUS;
            if close {
                gained += orb.value;
            }
            !close
        });
        self.xp = self.xp.saturating_add(gained);
        self.enter_level_choice();
    }

    fn enter_level_choice(&mut self) -> bool {
        if self.xp < u64::from(self.experience_needed()) || !self.is_advancing() {
            return false;
        }
        if self.suspended_phase.is_none() {
            self.suspended_phase = Some(self.phase);
        }
        self.xp -= u64::from(self.experience_needed());
        self.xp_level = self.xp_level.saturating_add(1);
        self.offer_count += 1;
        let mut rng = Rng::stream(self.seed, STREAM_OFFERS, self.offer_count);
        self.offers = level_offers(&mut rng, &self.offer_context(), &self.weapons, |u| self.unlocked.contains(&u));
        self.phase = Phase::Upgrade;
        self.events.push(Event::Leveled(self.xp_level));
        true
    }

    /// A fixture drop that still requires the real pickup simulation.
    #[doc(hidden)]
    pub fn spawn_experience(&mut self, x: f32, y: f32, value: u32) {
        self.add_orb((x, y), value);
    }

    /// Use the same acquisition and caps as a level card.
    #[doc(hidden)]
    pub fn grant_weapon(&mut self, kind: WeaponKind) -> bool {
        let accepted = acquire_weapon(&mut self.weapons, kind);
        self.weapon_cooldowns.resize(self.weapons.len(), 0.0);
        accepted
    }

    fn repair(&mut self, amount: u8) {
        self.integrity = (self.integrity + amount).min(MAX_INTEGRITY);
    }

    fn unlock(&mut self, unlock: Unlock) {
        if self.unlocked.insert(unlock) {
            self.earned.push(unlock);
            self.events.push(Event::Unlocked(unlock));
        }
    }

    fn effect(&mut self, kind: EffectKind, at: (f32, f32), to: (f32, f32), life: f32) {
        if self.effects.len() >= MAX_EFFECTS {
            self.effects.remove(0);
        }
        self.effects.push(Effect { kind, x: at.0, y: at.1, to, ttl: life, life, radius: 0.0 });
    }

    fn update_effects(&mut self) {
        for effect in &mut self.effects {
            effect.ttl -= STEP;
        }
        self.effects.retain(|e| e.ttl > 0.0);
    }

    // -- the player's packets -------------------------------------------

    fn move_shots(&mut self) {
        let mut shots = std::mem::take(&mut self.shots);
        self.live_fragments = shots.iter().filter(|s| s.kind == ShotKind::Fragment).count();
        self.live_shots = shots.len();
        shots.retain_mut(|shot| {
            if shot.guided {
                self.steer(shot);
            }
            let from = (shot.x, shot.y);
            shot.x += shot.vx * STEP;
            shot.y += shot.vy * STEP;
            shot.travelled += (shot.vx.hypot(shot.vy * ASPECT)) * STEP;
            let consumed = self.phase == Phase::Combat && self.collide(shot, from);
            let gone = shot.travelled > shot.range;
            if consumed || gone {
                self.live_shots = self.live_shots.saturating_sub(1);
            }
            !consumed && !gone
        });
        shots.append(&mut self.new_shots);
        self.shots = shots;
    }

    /// Turn a guided packet toward the best target inside its cone.
    fn steer(&self, shot: &mut Shot) {
        let speed = shot.vx.hypot(shot.vy * ASPECT);
        let Some(heading) = unit((shot.vx, shot.vy * ASPECT)) else { return };
        let mut best: Option<(f32, u32, (f32, f32))> = None;
        for enemy in self.enemies.iter().filter(|e| e.alive && e.visible_in(self.camera())) {
            let at = enemy.center();
            if visual_distance((shot.x, shot.y), at) > GUIDANCE_RANGE {
                continue;
            }
            let Some(target) = direction((shot.x, shot.y), at) else { continue };
            let dot = (heading.0 * target.0 + heading.1 * target.1).clamp(-1.0, 1.0);
            let cross = heading.0 * target.1 - heading.1 * target.0;
            let turn = cross.atan2(dot);
            if turn.abs() <= GUIDANCE_CONE.to_radians()
                && best.is_none_or(|(old, id, _)| turn.abs() < old || (turn.abs() == old && enemy.id < id))
            {
                best = Some((turn.abs(), enemy.id, (turn, 0.0)));
            }
        }
        if let Some((_, _, (turn, _))) = best {
            let heading = rotated(heading, turn.clamp(-GUIDANCE_TURN * STEP, GUIDANCE_TURN * STEP));
            (shot.vx, shot.vy) = velocity(heading, speed);
        }
    }

    /// Resolve everything a packet touched this step, nearest first.
    /// Returns true when the packet is used up.
    fn collide(&mut self, shot: &mut Shot, from: (f32, f32)) -> bool {
        let to = (shot.x, shot.y);
        let mut hits: Vec<(f32, u32, usize)> = self
            .enemies
            .iter()
            .enumerate()
            .filter(|(_, e)| e.alive && !shot.hit.contains(&e.id))
            .filter_map(|(i, e)| {
                let relative_from = (from.0 + e.x - e.previous.0, from.1 + e.y - e.previous.1);
                segment_enters(relative_from, to, e.x - 0.15, e.y, e.x + e.w + 0.15, e.y + e.h).map(|t| (t, e.id, i))
            })
            .collect();
        hits.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let rock = self.rock_entry(from, to);
        for (t, id, index) in hits {
            if rock.is_some_and(|r| r < t) {
                break;
            }
            if !self.enemies[index].alive {
                continue;
            }
            let at = self.enemies[index].center();
            if shot.kind == ShotKind::Pivot {
                self.pivot(index, shot.damage);
                return true;
            }
            let impact = shot.damage;
            let killed = self.damage(index, impact);
            self.effect(EffectKind::Spark, (shot.x, at.1), (0.0, 0.0), 0.12);
            shot.hit.push(id);
            if matches!(shot.kind, ShotKind::Primary | ShotKind::Fork) {
                self.feedback();
                if killed {
                    self.fragment(id, at, impact, (shot.vx, shot.vy));
                }
            }
            if shot.pierce > 0 {
                shot.pierce -= 1;
                shot.damage *= PIERCE_RETAIN;
            } else {
                return true;
            }
        }
        if let Some(t) = rock {
            // Rocks absorb packets from either side.
            let at = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            self.effect(EffectKind::Spark, at, (0.0, 0.0), 0.12);
            return true;
        }
        false
    }

    /// Damage an enemy; true if this killed it. Credit is given once.
    fn damage(&mut self, index: usize, amount: f32) -> bool {
        self.enemies[index].flash = 0.08;
        if let Some(pool) = self.enemies[index].pool {
            if !self.pools[pool].alive {
                return false;
            }
            self.pools[pool].hp -= amount;
            for enemy in self.enemies.iter_mut().filter(|e| e.pool == Some(pool)) {
                enemy.hp = self.pools[pool].hp;
            }
            if self.pools[pool].hp > HP_EPSILON {
                return false;
            }
            self.pools[pool].alive = false;
            let members: Vec<usize> = (0..self.enemies.len()).filter(|&i| self.enemies[i].pool == Some(pool)).collect();
            for member in members {
                self.kill(member);
            }
            return true;
        }
        let enemy = &mut self.enemies[index];
        enemy.hp -= amount;
        if enemy.hp <= HP_EPSILON && enemy.alive {
            self.kill(index);
            return true;
        }
        false
    }

    fn kill(&mut self, index: usize) {
        let enemy = &mut self.enemies[index];
        if !enemy.alive {
            return;
        }
        enemy.alive = false;
        enemy.hp = 0.0;
        if enemy.credit {
            self.score += enemy.score;
            self.compromised += 1;
        }
        let at = enemy.center();
        let value = match enemy.role {
            Role::Device(_) | Role::Guard => 1,
            Role::Miniboss(MinibossKind::RelayPair) => 12,
            Role::Miniboss(_) => 24,
            Role::Boss(_) => {
                if enemy.credit {
                    48
                } else {
                    0
                }
            }
        };
        if value > 0 {
            self.add_orb(at, value);
        }
        self.effect(EffectKind::Burst, at, (0.0, 0.0), 0.35);
        if self.prior_devices.saturating_add(self.compromised) >= FRAGMENT_DEVICES {
            self.unlock(Unlock::Fragment);
        }
    }

    fn feedback(&mut self) {
        let Some(refund) = self.stats.feedback_refund else { return };
        let p = &mut self.player;
        if p.charges >= self.stats.max_charges {
            return;
        }
        let now = self.elapsed;
        let Some(oldest) = p.feedback.iter_mut().min_by(|a, b| a.total_cmp(b)) else { return };
        if now - *oldest < 1.0 {
            return;
        }
        *oldest = now;
        p.recharge += refund;
        if p.recharge >= self.stats.recharge {
            p.charges += 1;
            p.recharge = 0.0;
        }
    }

    fn fragment(&mut self, source: u32, at: (f32, f32), impact: f32, incoming: (f32, f32)) {
        let Some(share) = self.stats.fragment_share else { return };
        let heading = unit((incoming.0, incoming.1 * ASPECT)).unwrap_or((0.0, -1.0));
        for side in [-1.0, 1.0] {
            if self.live_fragments >= MAX_FRAGMENTS || self.live_shots + self.new_shots.len() >= MAX_SHOTS {
                return;
            }
            self.live_fragments += 1;
            let (vx, vy) = velocity(rotated(heading, side * std::f32::consts::FRAC_PI_2), FRAGMENT_SPEED);
            self.new_shots.push(Shot {
                kind: ShotKind::Fragment,
                x: at.0,
                y: at.1,
                vx,
                vy,
                damage: impact * share,
                pierce: 0,
                hit: vec![source],
                travelled: 0.0,
                range: FRAGMENT_RANGE,
                guided: false,
            });
        }
    }

    fn pivot(&mut self, first: usize, direct: f32) {
        let mut visited = vec![self.enemies[first].id];
        let mut at = self.enemies[first].center();
        self.damage(first, direct);
        self.effect(EffectKind::Chain, at, at, 0.35);
        for _ in 0..self.stats.pivot_jumps {
            let next = self
                .enemies
                .iter()
                .enumerate()
                .filter(|(_, e)| e.alive && !visited.contains(&e.id))
                .map(|(i, e)| (visual_distance(at, e.center()), e.id, i))
                .filter(|(d, _, _)| *d <= PIVOT_REACH)
                .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let Some((_, id, index)) = next else { break };
            visited.push(id);
            let to = self.enemies[index].center();
            self.effect(EffectKind::Chain, at, to, 0.35);
            at = to;
            self.damage(index, PIVOT_CHAIN_DAMAGE);
        }
    }

    // -- hostile packets and hazards ------------------------------------

    fn player_box(&self) -> (f32, f32, f32, f32) {
        let half = self.rig.width() / 2.0 - FORGIVENESS;
        (self.player.x - half, self.player.y, self.player.x + half, self.player.y + 1.0)
    }

    fn move_hostiles(&mut self) {
        let (left, top, right, bottom) = self.player_box();
        let camera = self.camera();
        let mut hostiles = std::mem::take(&mut self.hostiles);
        let mut hits = 0;
        let mut absorbed = Vec::new();
        hostiles.retain_mut(|h| {
            let from = (h.x, h.y);
            h.x += h.vx * STEP;
            h.y += h.vy * STEP;
            let previous = if self.player_teleported { (self.player.x, self.player.y) } else { self.player_previous };
            let relative_from = (from.0 + self.player.x - previous.0, from.1 + self.player.y - previous.1);
            let player = segment_enters(relative_from, (h.x, h.y), left, top, right, bottom);
            let rock = self.rock_entry(from, (h.x, h.y));
            if let Some(t) = rock
                && player.is_none_or(|p| t < p)
            {
                absorbed.push((from.0 + h.vx * STEP * t, from.1 + h.vy * STEP * t));
                return false;
            }
            if player.is_some() {
                hits += 1;
                return false;
            }
            h.y > camera.1 - 6.0
                && h.y < camera.1 + ARENA_H + 6.0
                && h.x > camera.0 - 12.0
                && h.x < camera.0 + ARENA_W + 12.0
        });
        self.hostiles = hostiles;
        for at in absorbed {
            self.effect(EffectKind::Spark, at, (0.0, 0.0), 0.12);
        }
        for _ in 0..hits {
            self.hit_player();
        }
    }

    /// One incoming hit. True if it did something: damage or an absorb.
    fn hit_player(&mut self) -> bool {
        if self.phase != Phase::Combat {
            return false;
        }
        let at = (self.player.x, self.player.y + 0.5);
        let p = &mut self.player;
        if p.invulnerable > 0.0 || p.protection > 0.0 {
            return false;
        }
        p.invulnerable = self.stats.invulnerability;
        if p.shield_ready {
            p.shield_ready = false;
            p.shield_used_at = self.completed;
            self.events.push(Event::Absorbed);
            self.effect(EffectKind::Absorb, at, at, 0.5);
            return true;
        }
        let cost = self.stats.hit_cost.min(self.integrity);
        self.lose_integrity(cost);
        self.events.push(Event::Damaged(cost));
        self.effect(EffectKind::Damage, at, at, 0.5);
        true
    }

    fn lose_integrity(&mut self, amount: u8) {
        if self.immortal {
            return;
        }
        self.integrity = self.integrity.saturating_sub(amount);
        if self.integrity == 0 {
            self.ended = Some(EndReason::Defeated);
            self.phase = Phase::Over;
            self.events.push(Event::Defeated);
            self.clear_hazards();
        }
    }

    fn clear_hazards(&mut self) {
        self.hostiles.clear();
        self.marks.clear();
        self.scans.clear();
        self.bursts.clear();
        self.curtains.clear();
    }

    fn spawn(&mut self, kind: HostileKind, at: (f32, f32), velocity: (f32, f32)) -> bool {
        if self.hostiles.len() >= self.hostile_cap() {
            return false;
        }
        self.hostiles.push(Hostile { kind, x: at.0, y: at.1, vx: velocity.0, vy: velocity.1 });
        true
    }

    /// `count` packets spread evenly across `spread` degrees.
    fn fan(&mut self, at: (f32, f32), count: u32, spread: f32) {
        if self.hostiles.len() + count as usize > self.hostile_cap() {
            return;
        }
        let speed = hostile_speed(self.wave);
        let heading = direction(at, (self.player.x, self.player.y + 0.5)).unwrap_or((0.0, 1.0));
        for i in 0..count {
            let angle = if count == 1 { 0.0 } else { -spread / 2.0 + spread * i as f32 / (count - 1) as f32 };
            self.spawn(HostileKind::Diagonal, at, velocity(rotated(heading, angle.to_radians()), speed * ASPECT));
        }
    }

    fn alive_index(&self, id: u32) -> Option<usize> {
        self.enemies.iter().position(|e| e.id == id && e.alive)
    }

    fn update_marks(&mut self) {
        let mut due = Vec::new();
        for mark in &mut self.marks {
            mark.warn -= STEP;
            if mark.warn <= 0.0 {
                due.push(*mark);
            }
        }
        self.marks.retain(|m| m.warn > 0.0);
        // A destroyed source's warning goes with it.
        let alive: Vec<u32> = self.enemies.iter().filter(|e| e.alive).map(|e| e.id).collect();
        self.marks.retain(|m| alive.contains(&m.source));
        for mark in due {
            let Some(index) = self.alive_index(mark.source) else { continue };
            let source = &self.enemies[index];
            let bottom = source.y + source.h;
            match mark.attack {
                MarkAttack::Burst => {
                    self.bursts.push(Burst { source: mark.source, target: (mark.x, mark.y), left: 3, timer: 0.0 })
                }
                MarkAttack::Pair => {
                    let speed = hostile_speed(self.wave);
                    let ends = [(source.x + 1.0, bottom), (source.x + source.w - 1.0, bottom)];
                    if self.hostiles.len() + 2 <= self.hostile_cap() {
                        for from in ends {
                            self.spawn(HostileKind::Aimed, from, aimed(from, (mark.x, mark.y), speed));
                        }
                    }
                }
                MarkAttack::Scan => self.scans.push(Scan {
                    x: mark.x,
                    top: self.camera().1,
                    bottom: self.camera().1 + ARENA_H,
                    warn: 0.0,
                    active: SCAN_ACTIVE,
                    label: None,
                    source: None,
                    hit: false,
                }),
                MarkAttack::Stream => {
                    if let Encounter::Special(pattern) = &mut self.encounter {
                        pattern.stream = STREAM_TIME;
                        pattern.stream_timer = 0.0;
                        pattern.stream_target = Some((mark.x, mark.y));
                    }
                }
                MarkAttack::Shot | MarkAttack::Spread => {
                    let from = source.center();
                    let target = (mark.x, mark.y);
                    let heading = direction(from, target).unwrap_or((0.0, 1.0));
                    let speed = hostile_speed(self.wave) * ASPECT;
                    if mark.attack == MarkAttack::Shot {
                        self.spawn(HostileKind::Straight, from, aimed(from, target, hostile_speed(self.wave)));
                    } else if self.hostiles.len() + 2 <= self.hostile_cap() {
                        for side in [-1.0, 1.0] {
                            self.spawn(
                                HostileKind::Diagonal,
                                from,
                                velocity(rotated(heading, side * SPREAD_ANGLE.to_radians()), speed),
                            );
                        }
                    }
                }
            }
        }
    }

    fn update_bursts(&mut self) {
        let speed = hostile_speed(self.wave);
        let mut bursts = std::mem::take(&mut self.bursts);
        bursts.retain_mut(|burst| {
            burst.timer -= STEP;
            if burst.timer > 0.0 {
                return true;
            }
            let Some(index) = self.alive_index(burst.source) else { return false };
            let source = &self.enemies[index];
            let from = (source.x + source.w / 2.0, source.y + source.h);
            self.spawn(HostileKind::Aimed, from, aimed(from, burst.target, speed));
            burst.left -= 1;
            burst.timer = BURST_GAP;
            burst.left > 0
        });
        self.bursts = bursts;
    }

    fn update_scans(&mut self) {
        let (left, player_top, right, player_bottom) = self.player_box();
        let mut scans = std::mem::take(&mut self.scans);
        let mut hits = Vec::new();
        scans.retain_mut(|scan| {
            if scan.warn > 0.0 {
                scan.warn -= STEP;
                if let Some(source) = scan.source
                    && self.alive_index(source).is_none()
                {
                    return false;
                }
                return true;
            }
            scan.active -= STEP;
            let overlaps = player_bottom > scan.top
                && player_top < scan.bottom
                && right > scan.x - SCAN_WIDTH / 2.0
                && left < scan.x + SCAN_WIDTH / 2.0;
            if overlaps && !scan.hit {
                hits.push(scan.x);
                scan.hit = true;
            }
            scan.active > 0.0
        });
        self.scans = scans;
        for x in hits {
            if !self.hit_player() {
                // Blocked this step; the beam can still catch the player
                // once the protection ends.
                if let Some(scan) = self.scans.iter_mut().find(|s| s.x == x && s.warn <= 0.0) {
                    scan.hit = false;
                }
            }
        }
    }

    fn update_curtains(&mut self) {
        let (left, _, right, _) = self.player_box();
        let previous_y = if self.player_teleported { self.player.y } else { self.player_previous.1 };
        let mut curtains = std::mem::take(&mut self.curtains);
        let mut hits = Vec::new();
        for (index, c) in curtains.iter_mut().enumerate() {
            if c.warn > 0.0 {
                c.warn -= STEP;
                continue;
            }
            let from = c.y - previous_y;
            c.y += c.speed * STEP;
            let to = c.y - self.player.y;
            let in_row = from.min(to) <= 1.0 && from.max(to) >= 0.0;
            let safe = left >= c.gap_left && right <= c.gap_left + c.gap_width;
            if in_row && right > c.left && left < c.right && !safe && !c.hit {
                c.hit = true;
                hits.push(index);
            }
        }
        self.curtains = curtains;
        for index in hits {
            if !self.hit_player()
                && let Some(c) = self.curtains.get_mut(index)
            {
                // Protection does not consume the wall. Reset the exact
                // curtain that crossed, even if the ship has moved away.
                c.hit = false;
            }
        }
        self.curtains.retain(|c| c.y > c.top - 1.0 && c.y < c.bottom + 1.0);
    }

    fn update_telegraph_flags(&mut self) {
        for enemy in &mut self.enemies {
            enemy.telegraphing = self.marks.iter().any(|m| m.source == enemy.id)
                || self.scans.iter().any(|s| s.warn > 0.0 && s.source == Some(enemy.id));
        }
    }

    // -- encounters ------------------------------------------------------

    fn start_wave(&mut self, wave: u32) {
        self.wave = wave;
        self.enemies.clear();
        self.pools.clear();
        self.clear_hazards();
        self.encounter_kind = encounter_for(self.seed, wave);
        self.roster.clear();
        self.arrivals = Rng::stream(self.seed, STREAM_COMBAT ^ 0xA771, u64::from(wave));
        self.banner = Some((
            match self.encounter_kind {
                EncounterKind::Ordinary => format!("WAVE {wave:02}"),
                EncounterKind::Miniboss(kind) => format!("WAVE {wave:02} · MINIBOSS · {}", kind.name().to_uppercase()),
                EncounterKind::Boss(kind) => format!("WAVE {wave:02} · BOSS · {}", kind.name().to_uppercase()),
            },
            BANNER,
        ));
        self.encounter = match self.encounter_kind {
            EncounterKind::Ordinary => {
                self.roster = wave_roster(self.seed, wave);
                for index in 0..OPENING_SHIPS.min(self.roster.len()) {
                    self.spawn_device(self.roster[index], Some(index as u32));
                }
                Encounter::Swarm(Swarm {
                    spawned: OPENING_SHIPS.min(self.roster.len()),
                    timer: spawn_interval(wave),
                    fire: FIRST_SHOT_DELAY,
                    telegraph: telegraph_interval(wave) * 0.6,
                })
            }
            kind => Encounter::Special(self.spawn_special(kind)),
        };
        self.phase = Phase::Combat;
    }

    fn add_enemy(&mut self, role: Role, at: (f32, f32), size: (f32, f32), hp: f32, credit: bool, score: u64) -> u32 {
        let camera = self.camera();
        let center = (at.0 + size.0 / 2.0, at.1 + size.1 / 2.0);
        let heading = direction(center, (self.player.x, self.player.y + 0.5)).map_or(std::f32::consts::PI, heading_of);
        let id = self.next_id;
        self.next_id += 1;
        self.enemies.push(Enemy {
            id,
            role,
            x: at.0,
            y: at.1,
            w: size.0,
            h: size.1,
            hp,
            max_hp: hp,
            alive: true,
            flash: 0.0,
            telegraphing: false,
            heading,
            velocity: (0.0, 0.0),
            thrusting: false,
            pool: None,
            credit,
            score,
            slot: (0.0, 0.0),
            cooldown: 0.0,
            previous: at,
            entry: None,
            entered: at.0 >= camera.0
                && at.1 >= camera.1
                && at.0 + size.0 <= camera.0 + ARENA_W
                && at.1 + size.1 <= camera.1 + ARENA_H,
        });
        id
    }

    fn perimeter(&mut self, size: (f32, f32), edge: Option<u32>) -> (f32, f32) {
        let (left, top) = self.camera();
        for attempt in 0..32 {
            let side =
                if attempt == 0 { edge.unwrap_or_else(|| self.arrivals.below(4)) } else { self.arrivals.below(4) };
            let at = match side % 4 {
                0 => (left - size.0 - 2.0, top + self.arrivals.range(0.0, ARENA_H - size.1)),
                1 => (left + ARENA_W + 2.0, top + self.arrivals.range(0.0, ARENA_H - size.1)),
                2 => (left + self.arrivals.range(0.0, ARENA_W - size.0), top - size.1 - 2.0),
                _ => (left + self.arrivals.range(0.0, ARENA_W - size.0), top + ARENA_H + 2.0),
            };
            let clear = self.asteroids.iter().all(|a| {
                box_nearest((a.x, a.y), at.0, at.1, at.0 + size.0, at.1 + size.1).0 >= a.radius + ROCK_CLEARANCE
            });
            if clear && at.0 >= 0.0 && at.1 >= 0.0 && at.0 + size.0 <= WORLD_W && at.1 + size.1 <= WORLD_H {
                return at;
            }
        }
        // At a map corner the opposite edge always points into the world.
        let x = if self.player.x < WORLD_W / 2.0 { left + ARENA_W + 2.0 } else { left - size.0 - 2.0 };
        (x.clamp(0.0, WORLD_W - size.0), (top + ARENA_H / 2.0).clamp(0.0, WORLD_H - size.1))
    }

    fn recycle_stragglers(&mut self) {
        let player = (self.player.x, self.player.y + 0.5);
        let far: Vec<_> = self
            .enemies
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.alive
                    && matches!(e.role, Role::Device(_) | Role::Guard)
                    && visual_distance(e.center(), player) > RECYCLE_DISTANCE
            })
            .map(|(i, _)| i)
            .collect();
        for index in far {
            let at = self.perimeter((self.enemies[index].w, self.enemies[index].h), None);
            let wave = self.wave;
            let e = &mut self.enemies[index];
            let id = e.id;
            (e.x, e.y) = at;
            e.previous = at;
            e.entered = false;
            e.cooldown = e.cooldown.max(1.5);
            // Come back in facing the ship, not still fleeing the old way.
            e.heading = direction(e.center(), player).map_or(e.heading, heading_of);
            let top = Self::flies_like(e.role).map_or(0.0, |kind| flight_speed(kind, wave));
            let n = e.nose();
            e.velocity = (n.0 * top * ENTRY_FRACTION, n.1 * top * ENTRY_FRACTION);
            self.marks.retain(|m| m.source != id);
            self.scans.retain(|m| m.source != Some(id));
            self.bursts.retain(|m| m.source != id);
        }
    }

    fn spawn_device(&mut self, kind: DeviceKind, edge: Option<u32>) {
        let at = self.perimeter((3.0, 1.0), edge);
        let hp = device_hp(kind.base_hp(), self.wave) as f32;
        self.add_enemy(Role::Device(kind), at, (3.0, 1.0), hp, true, kind.score());
        let top = flight_speed(kind, self.wave);
        let e = self.enemies.last_mut().unwrap();
        e.cooldown = 1.5;
        let n = e.nose();
        e.velocity = (n.0 * top * ENTRY_FRACTION, n.1 * top * ENTRY_FRACTION);
    }

    /// The kind a ship flies like: guards fly like Endpoints.
    fn flies_like(role: Role) -> Option<DeviceKind> {
        match role {
            Role::Device(kind) => Some(kind),
            Role::Guard => Some(DeviceKind::Endpoint),
            _ => None,
        }
    }

    fn update_swarm(&mut self, mut swarm: Swarm) {
        swarm.timer -= STEP;
        let alive = self.enemies.iter().filter(|e| e.alive).count();
        if swarm.spawned < self.roster.len() && swarm.timer <= 0.0 && alive < SWARM_CAP {
            self.spawn_device(self.roster[swarm.spawned], None);
            swarm.spawned += 1;
            swarm.timer = spawn_interval(self.wave);
        }
        let ships: Vec<usize> = (0..self.enemies.len()).filter(|&i| self.enemies[i].alive).collect();
        for ship in ships {
            self.fly_ship(ship);
        }
        swarm.fire -= STEP;
        if swarm.fire <= 0.0 {
            swarm.fire = fire_interval(self.wave) * self.rng.range(0.8, 1.2);
            self.swarm_shot();
        }
        swarm.telegraph -= STEP;
        if swarm.telegraph <= 0.0 {
            swarm.telegraph = telegraph_interval(self.wave) * self.rng.range(0.8, 1.2);
            self.swarm_telegraph();
        }
        self.encounter = Encounter::Swarm(swarm);
    }

    fn fly_ship(&mut self, index: usize) {
        let camera = self.camera();
        let player = (self.player.x, self.player.y + 0.5);
        let e = &mut self.enemies[index];
        e.cooldown = (e.cooldown - STEP).max(0.0);
        let Some(kind) = Self::flies_like(e.role) else { return };
        let at = e.center();
        let top = flight_speed(kind, self.wave);
        let side = if e.id.is_multiple_of(2) { 1.0 } else { -1.0 };
        let (heading, velocity, entered) = (e.heading, e.velocity, e.entered);
        // Where the ship wants to go: the same pursuit, orbit and stand-off
        // choices as before, now flown with momentum.
        let (want_dir, want_speed) = match direction(at, player) {
            None => (nose(heading), 0.0),
            Some(toward) if !entered => (toward, top),
            Some(toward) => {
                let distance = visual_distance(at, player);
                let tangent = (-toward.1 * side, toward.0 * side);
                match kind {
                    DeviceKind::Router => (
                        unit((toward.0 * 0.65 + tangent.0 * 0.65, toward.1 * 0.65 + tangent.1 * 0.65))
                            .unwrap_or(toward),
                        top,
                    ),
                    DeviceKind::Switch | DeviceKind::IdsProbe => {
                        let preferred = if kind == DeviceKind::IdsProbe { 20.0 } else { 16.0 };
                        let radial = ((distance - preferred) / 4.0).clamp(-1.0, 1.0);
                        let dir = unit((toward.0 * radial + tangent.0 * 0.5, toward.1 * radial + tangent.1 * 0.5))
                            .unwrap_or(tangent);
                        (dir, top * (0.5 + 0.5 * radial.abs()))
                    }
                    _ => (toward, top),
                }
            }
        };
        let look = unit(velocity).unwrap_or(nose(heading));
        let avoid = self.rock_avoidance(at, look, velocity.0.hypot(velocity.1), side);
        let dir = unit((want_dir.0 + avoid.0, want_dir.1 + avoid.1)).unwrap_or(want_dir);
        let want = (dir.0 * want_speed, dir.1 * want_speed);
        let (heading, velocity, thrusting) = steer_ship(heading, velocity, want, device_flight(kind), top);
        let e = &mut self.enemies[index];
        (e.heading, e.velocity, e.thrusting) = (heading, velocity, thrusting);
        e.x += velocity.0 * STEP;
        e.y += velocity.1 / ASPECT * STEP;
        self.rock_push(index);
        let e = &mut self.enemies[index];
        if e.x >= camera.0 && e.y >= camera.1 && e.x + e.w <= camera.0 + ARENA_W && e.y + e.h <= camera.1 + ARENA_H {
            e.entered = true;
        }
        if e.entered {
            if e.x < 0.0 || e.x > WORLD_W - e.w {
                e.x = e.x.clamp(0.0, WORLD_W - e.w);
                e.velocity.0 = 0.0;
            }
            if e.y < 0.0 || e.y > WORLD_H - e.h {
                e.y = e.y.clamp(0.0, WORLD_H - e.h);
                e.velocity.1 = 0.0;
            }
        }
    }

    fn swarm_shot(&mut self) {
        let player = (self.player.x, self.player.y + 0.5);
        let shooters: Vec<usize> = (0..self.enemies.len())
            .filter(|&i| {
                let e = &self.enemies[i];
                e.alive
                    && e.entered
                    && e.visible_in(self.camera())
                    && e.cooldown <= 0.0
                    && matches!(e.role, Role::Device(DeviceKind::Endpoint | DeviceKind::Router | DeviceKind::Firewall))
                    && visual_distance(e.center(), player) >= 12.0
            })
            .collect();
        if shooters.is_empty() {
            return;
        }
        // Shots in flight and shots still warning share the packet cap.
        let pending: usize =
            self.marks.iter().filter(|m| m.is_shot()).map(|m| if m.attack == MarkAttack::Spread { 2 } else { 1 }).sum();
        if self.hostiles.len() + pending + 2 > self.hostile_cap() {
            return;
        }
        let index = shooters[self.rng.below(shooters.len() as u32) as usize];
        let e = &self.enemies[index];
        let router = e.role == Role::Device(DeviceKind::Router);
        let interval = if e.role == Role::Device(DeviceKind::Firewall) { 6.0 } else { 3.0 };
        self.enemies[index].cooldown = interval * pattern_pace(self.wave);
        // Like every aimed attack, a ship's shot warns first and then flies
        // at the spot it marked, not at wherever the player went.
        let attack = if router { MarkAttack::Spread } else { MarkAttack::Shot };
        self.marks.push(Mark { x: player.0, y: player.1, warn: TELEGRAPH, source: self.enemies[index].id, attack });
    }

    fn busy(&self, id: u32) -> bool {
        self.marks.iter().any(|m| m.source == id)
            || self.bursts.iter().any(|b| b.source == id)
            || self.scans.iter().any(|s| s.source == Some(id))
    }

    fn swarm_telegraph(&mut self) {
        let heavy = self.marks.iter().filter(|m| !m.is_shot()).count();
        if heavy + self.bursts.len() + self.scans.len() >= 2 {
            return;
        }
        let candidates: Vec<usize> = (0..self.enemies.len())
            .filter(|&i| {
                let e = &self.enemies[i];
                e.alive
                    && e.entered
                    && e.visible_in(self.camera())
                    && matches!(e.role, Role::Device(DeviceKind::Switch | DeviceKind::IdsProbe))
                    && !self.busy(e.id)
            })
            .collect();
        if candidates.is_empty() {
            return;
        }
        let pick = candidates[self.rng.below(candidates.len() as u32) as usize];
        if self.enemies[pick].role == Role::Device(DeviceKind::Switch) {
            self.mark_player(pick, MarkAttack::Burst, TELEGRAPH);
        } else {
            let source = self.enemies[pick].id;
            self.scans.push(Scan {
                x: self.player.x,
                top: self.camera().1,
                bottom: self.camera().1 + ARENA_H,
                warn: TELEGRAPH,
                active: SCAN_ACTIVE,
                label: None,
                source: Some(source),
                hit: false,
            });
        }
    }

    fn check_contacts(&mut self) {
        let half = self.rig.width() / 2.0 - FORGIVENESS;
        let from = if self.player_teleported { (self.player.x, self.player.y) } else { self.player_previous };
        let to = (self.player.x, self.player.y + 0.5);
        let touching = self.enemies.iter().filter(|e| e.alive).any(|e| {
            let relative_from = (from.0 + e.x - e.previous.0, from.1 + 0.5 + e.y - e.previous.1);
            segment_enters(
                relative_from,
                to,
                e.x - half,
                e.y - 0.5 + FORGIVENESS,
                e.x + e.w + half,
                e.y + e.h + 0.5 - FORGIVENESS,
            )
            .is_some()
        });
        if touching {
            self.hit_player();
        }
    }

    fn check_cleared(&mut self) {
        let cleared = match self.encounter {
            Encounter::Swarm(swarm) => {
                swarm.spawned >= self.roster.len()
                    && !self.enemies.iter().any(|e| e.alive && matches!(e.role, Role::Device(_)))
            }
            Encounter::Special(_) => {
                !self.enemies.iter().any(|e| e.alive && matches!(e.role, Role::Miniboss(_) | Role::Boss(_)))
            }
            Encounter::None => false,
        };
        if cleared {
            self.complete_encounter();
        }
    }

    fn complete_encounter(&mut self) {
        let wave = self.wave;
        let kind = self.encounter_kind;
        self.clear_hazards();
        // Remaining reinforcements leave without credit.
        self.enemies.clear();
        self.pools.clear();
        self.encounter = Encounter::None;
        self.completed = wave;
        match kind {
            EncounterKind::Ordinary => self.score += 100 * u64::from(wave),
            EncounterKind::Miniboss(_) => {
                self.score += 1500 + 100 * u64::from(wave);
                self.unlock(Unlock::Fork);
            }
            EncounterKind::Boss(boss) => {
                self.score += 5000 + 250 * u64::from(wave);
                self.repair(1 + self.build.rank(UpgradeId::WarmBackup));
                if boss == BossKind::Gateway {
                    self.unlock(Unlock::Guidance);
                    self.unlock(Unlock::Bastion);
                }
            }
        }
        for unlock in Unlock::ALL {
            if unlock.wave().is_some_and(|needed| wave >= needed) {
                self.unlock(unlock);
            }
        }
        if self.prior_devices + self.compromised >= FRAGMENT_DEVICES {
            self.unlock(Unlock::Fragment);
        }
        if let Some(from) = self.player.spare_parts_from
            && wave > from
            && (wave - from).is_multiple_of(SPARE_PARTS_INTERVAL)
        {
            self.repair(1);
        }
        if self.build.rank(UpgradeId::CacheShield) > 0
            && !self.player.shield_ready
            && wave >= self.player.shield_used_at + CACHE_SHIELD_RECHARGE
        {
            self.player.shield_ready = true;
        }
        self.events.push(Event::Completed(wave));
        self.banner = Some((format!("WAVE {wave:02} CLEARED"), BANNER));
        self.phase = Phase::Intermission { remaining: INTERMISSION };
    }

    // -- minibosses and bosses -------------------------------------------

    fn spawn_special(&mut self, kind: EncounterKind) -> Pattern {
        let wave = self.wave;
        let origin = self.camera();
        let mut pattern =
            Pattern { origin, timer: 1.4, dir: if self.rng.chance(0.5) { 1.0 } else { -1.0 }, ..Default::default() };
        let centered = |w: f32| (ARENA_W - w) / 2.0;
        match kind {
            EncounterKind::Miniboss(mini) => {
                let hp = miniboss_hp(wave) as f32;
                pattern.total_hp = hp;
                let size = sprite_size(mini.sprite());
                match mini {
                    MinibossKind::PortSentry => {
                        self.add_enemy(Role::Miniboss(mini), (centered(size.0), 2.0), size, hp, true, 0);
                    }
                    MinibossKind::RelayPair => {
                        for x in [14.0, ARENA_W - 14.0 - size.0] {
                            self.add_enemy(Role::Miniboss(mini), (x, 2.0), size, hp / 2.0, true, 0);
                            self.enemies.last_mut().unwrap().slot = (x, 0.0);
                        }
                    }
                    MinibossKind::TraceHunter => {
                        pattern.station = 1;
                        self.add_enemy(Role::Miniboss(mini), (Self::station_x(1, size.0), 1.5), size, hp, true, 0);
                    }
                }
            }
            EncounterKind::Boss(boss) => {
                let hp = boss_hp(wave) as f32;
                pattern.total_hp = hp;
                let size = sprite_size(boss.sprite());
                if boss == BossKind::LoadBalancer {
                    self.pools.push(Pool { hp, alive: true });
                    for (i, x) in [8.0, ARENA_W - 8.0 - size.0].into_iter().enumerate() {
                        self.add_enemy(Role::Boss(boss), (x, 2.0), size, hp, i == 0, 0);
                        let core = self.enemies.last_mut().unwrap();
                        core.pool = Some(0);
                        core.slot = (x, 0.0);
                    }
                    pattern.gap = (self.player.x - 7.0).clamp(origin.0 + 1.0, origin.0 + ARENA_W - 15.0);
                } else {
                    self.add_enemy(Role::Boss(boss), (centered(size.0), 1.0), size, hp, true, 0);
                }
            }
            EncounterKind::Ordinary => {}
        }
        // Large encounters visibly fly in from outside the arena instead
        // of appearing on the player's current position.
        for enemy in &mut self.enemies {
            enemy.x += origin.0;
            enemy.y += origin.1;
            enemy.slot.0 += origin.0;
            enemy.entry = Some((enemy.x, enemy.y));
            enemy.y = origin.1 - enemy.h - 2.0;
            enemy.previous = (enemy.x, enemy.y);
            enemy.entered = false;
        }
        pattern
    }

    /// Trace Hunter's three positions, close enough for the slowest rig to
    /// follow between attacks.
    fn station_x(station: usize, width: f32) -> f32 {
        let center = (ARENA_W - width) / 2.0;
        [center - HUNTER_SPACING, center, center + HUNTER_SPACING][station.min(2)]
    }

    fn special_parts(&self) -> Vec<usize> {
        (0..self.enemies.len())
            .filter(|&i| self.enemies[i].alive && matches!(self.enemies[i].role, Role::Miniboss(_) | Role::Boss(_)))
            .collect()
    }

    fn bottom_center(&self, index: usize) -> (f32, f32) {
        let e = &self.enemies[index];
        (e.x + e.w / 2.0, e.y + e.h)
    }

    /// Mark the player's position for one packet from `source`, which then
    /// fires there after the usual warning. A source has one shot pending at
    /// most, and warned shots count toward the packet cap.
    fn mark_shot(&mut self, source: usize) {
        let id = self.enemies[source].id;
        if self.marks.iter().any(|m| m.source == id && m.is_shot()) {
            return;
        }
        let pending: usize =
            self.marks.iter().filter(|m| m.is_shot()).map(|m| if m.attack == MarkAttack::Spread { 2 } else { 1 }).sum();
        if self.hostiles.len() + pending + 1 > self.hostile_cap() {
            return;
        }
        self.mark_player(source, MarkAttack::Shot, TELEGRAPH);
    }

    fn mark_player(&mut self, source: usize, attack: MarkAttack, warn: f32) {
        let id = self.enemies[source].id;
        self.marks.push(Mark { x: self.player.x, y: self.player.y + 0.5, warn, source: id, attack });
    }

    fn scan_column(&mut self, x: f32, _top: f32, warn: f32, label: Option<u8>) {
        let (left, top) = self.camera();
        let x = x.clamp(left + SCAN_WIDTH / 2.0, left + ARENA_W - SCAN_WIDTH / 2.0);
        self.scans.push(Scan {
            x,
            top,
            bottom: top + ARENA_H,
            warn,
            active: SCAN_ACTIVE,
            label,
            source: None,
            hit: false,
        });
    }

    fn drift(&mut self, index: usize, pattern: &mut Pattern, speed: f32) {
        let e = &mut self.enemies[index];
        e.x += pattern.dir * speed * STEP;
        if e.x < pattern.origin.0 + 1.0 {
            e.x = pattern.origin.0 + 1.0;
            pattern.dir = 1.0;
        } else if e.x + e.w > pattern.origin.0 + ARENA_W - 1.0 {
            e.x = pattern.origin.0 + ARENA_W - 1.0 - e.w;
            pattern.dir = -1.0;
        }
    }

    /// Move the encounter's whole formation toward the current camera.
    fn follow_camera(&mut self, pattern: &mut Pattern, parts: &[usize]) {
        let camera = self.camera();
        let mut origin = pattern.origin;
        if let Some(heading) = direction(origin, camera) {
            let (dx, dy) = velocity(heading, (FRAME_SPEED * STEP).min(visual_distance(origin, camera)));
            origin = (origin.0 + dx, origin.1 + dy);
        }
        // The formation's extent inside its frame, including where entering
        // parts are headed.
        let (mut first, mut last, mut lowest) = (ARENA_W, 0.0f32, 0.0f32);
        for &i in parts {
            let e = &self.enemies[i];
            let (x, y) = e.entry.unwrap_or((e.x, e.y));
            first = first.min(x - pattern.origin.0);
            last = last.max(x + e.w - pattern.origin.0);
            lowest = lowest.max(y + e.h - pattern.origin.1);
        }
        // Part of it always stays in view, and lagging below never carries
        // it onto the ship.
        let left = FRAME_LAG_X.min(last - FRAME_VISIBLE).max(0.0);
        let right = FRAME_LAG_X.min(ARENA_W - FRAME_VISIBLE - first).max(0.0);
        let down = (ARENA_H / 2.0 - 0.5 - FRAME_CLEARANCE - lowest).max(0.0);
        origin.0 = origin.0.clamp(camera.0 - left, camera.0 + right);
        origin.1 = origin.1.clamp(camera.1 - FRAME_LAG_UP, camera.1 + down);
        let (dx, dy) = (origin.0 - pattern.origin.0, origin.1 - pattern.origin.1);
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        pattern.origin = origin;
        pattern.gap += dx;
        if let Some((from, to, _)) = &mut pattern.relocate {
            *from += dx;
            *to += dx;
        }
        for &part in parts {
            let e = &mut self.enemies[part];
            (e.x, e.y) = (e.x + dx, e.y + dy);
            e.slot.0 += dx;
            if let Some(entry) = &mut e.entry {
                *entry = (entry.0 + dx, entry.1 + dy);
            }
        }
    }

    fn update_pattern(&mut self, mut pattern: Pattern) {
        let pace = pattern_pace(self.wave);
        let parts = self.special_parts();
        if parts.is_empty() {
            self.encounter = Encounter::Special(pattern);
            return;
        }
        self.follow_camera(&mut pattern, &parts);
        if parts.iter().any(|&i| self.enemies[i].entry.is_some()) {
            for &part in &parts {
                let enemy = &mut self.enemies[part];
                if let Some(to) = enemy.entry {
                    let from = (enemy.x, enemy.y);
                    if visual_distance(from, to) <= ENTRY_SPEED * STEP {
                        (enemy.x, enemy.y) = to;
                        enemy.entry = None;
                        enemy.entered = true;
                    } else if let Some(heading) = direction(from, to) {
                        let (vx, vy) = velocity(heading, ENTRY_SPEED);
                        enemy.x += vx * STEP;
                        enemy.y += vy * STEP;
                    }
                }
            }
            self.encounter = Encounter::Special(pattern);
            return;
        }
        pattern.timer -= STEP;
        let due = pattern.timer <= 0.0;
        match self.encounter_kind {
            EncounterKind::Miniboss(MinibossKind::PortSentry) => {
                let main = parts[0];
                self.drift(main, &mut pattern, 7.0 * (1.0 + 0.4 * level(self.wave)));
                if due {
                    if pattern.step.is_multiple_of(2) {
                        self.mark_player(main, MarkAttack::Pair, TELEGRAPH);
                    } else {
                        let top = self.enemies[main].y + self.enemies[main].h;
                        self.scan_column(self.player.x, top, TELEGRAPH, None);
                    }
                    pattern.step += 1;
                    pattern.timer = 2.3 * pace;
                }
                pattern.light -= STEP;
                if pattern.light <= 0.0 {
                    pattern.light = 1.4 * pace;
                    self.mark_shot(main);
                }
            }
            EncounterKind::Miniboss(MinibossKind::RelayPair) => {
                let faster = if parts.len() == 1 { 1.2 } else { 1.0 };
                pattern.phase += 0.9 * faster * STEP;
                for &part in &parts {
                    let home = self.enemies[part].slot.0;
                    let sign = if home < pattern.origin.0 + ARENA_W / 2.0 { 1.0 } else { -1.0 };
                    self.enemies[part].x = home + sign * 10.0 * pattern.phase.sin();
                }
                if due {
                    let shooter = parts[(pattern.step as usize) % parts.len()];
                    let from = self.bottom_center(shooter);
                    self.fan(from, 5, 80.0);
                    pattern.step += 1;
                    pattern.timer = 1.8 * pace;
                }
            }
            EncounterKind::Miniboss(MinibossKind::TraceHunter) => {
                let main = parts[0];
                if let Some((from, to, elapsed)) = &mut pattern.relocate {
                    *elapsed += STEP;
                    let t = (*elapsed / 0.6).min(1.0);
                    self.enemies[main].x = *from + (*to - *from) * t;
                    if t >= 1.0 {
                        pattern.relocate = None;
                    }
                }
                if due {
                    let attack = if pattern.step.is_multiple_of(2) { MarkAttack::Burst } else { MarkAttack::Scan };
                    self.mark_player(main, attack, TELEGRAPH + 0.35);
                    let next = (pattern.station + 1 + self.rng.below(2) as usize) % 3;
                    let width = self.enemies[main].w;
                    pattern.relocate =
                        Some((self.enemies[main].x, pattern.origin.0 + Self::station_x(next, width), 0.0));
                    pattern.station = next;
                    pattern.step += 1;
                    pattern.timer = TELEGRAPH + 0.35 + HUNTER_DWELL * pace;
                }
                pattern.light -= STEP;
                if pattern.light <= 0.0 {
                    pattern.light = 1.6 * pace;
                    if pattern.relocate.is_none() {
                        self.mark_shot(main);
                    }
                }
            }
            EncounterKind::Boss(BossKind::Gateway) => {
                let main = parts[0];
                self.drift(main, &mut pattern, 4.0);
                if due {
                    let from = self.bottom_center(main);
                    if pattern.step.is_multiple_of(2) {
                        let (count, spread) = if level(self.wave) >= 0.5 { (9, 108.0) } else { (7, 108.0) };
                        self.fan(from, count, spread);
                        pattern.timer = 1.6 * pace;
                    } else {
                        let x = self.player.x;
                        let side = if x < self.camera().0 + ARENA_W / 2.0 { 1.0 } else { -1.0 };
                        let other = x + side * self.rng.range(14.0, 22.0);
                        self.scan_column(x, from.1, TELEGRAPH, None);
                        self.scan_column(other, from.1, TELEGRAPH, None);
                        pattern.timer = 2.2 * pace;
                    }
                    pattern.step += 1;
                }
            }
            EncounterKind::Boss(BossKind::LoadBalancer) => {
                pattern.phase += 0.6 * STEP;
                let spread = 10.0 * (1.0 - pattern.phase.cos());
                for &part in &parts {
                    let e = &mut self.enemies[part];
                    e.x =
                        if e.slot.0 < pattern.origin.0 + ARENA_W / 2.0 { e.slot.0 + spread } else { e.slot.0 - spread };
                }
                if due {
                    if pattern.step.is_multiple_of(2) {
                        let width = (14.0 - 5.0 * level(self.wave)).max(CURTAIN_MIN_GAP);
                        let (left, top) = self.camera();
                        let shift = self.rng.range(-CURTAIN_MAX_SHIFT, CURTAIN_MAX_SHIFT);
                        pattern.gap = (pattern.gap + shift).clamp(left + 1.0, left + ARENA_W - 1.0 - width);
                        let speed = hostile_speed(self.wave)
                            * 0.75
                            * if (pattern.step / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
                        let y = if speed > 0.0 { top } else { top + ARENA_H - 0.5 };
                        self.curtains.push(Curtain {
                            left,
                            right: left + ARENA_W,
                            top,
                            bottom: top + ARENA_H,
                            y,
                            gap_left: pattern.gap,
                            gap_width: width,
                            warn: CURTAIN_WARN,
                            speed,
                            hit: false,
                        });
                        pattern.timer = CURTAIN_WARN + ARENA_H / speed.abs() + 0.2;
                    } else {
                        for &part in &parts {
                            self.mark_player(part, MarkAttack::Burst, TELEGRAPH);
                        }
                        pattern.timer = TELEGRAPH + 0.6 + 1.0 * pace;
                    }
                    pattern.step += 1;
                }
            }
            EncounterKind::Boss(BossKind::SentinelIds) => {
                let main = parts[0];
                pattern.phase += 0.5 * STEP;
                let width = self.enemies[main].w;
                self.enemies[main].x = pattern.origin.0 + (ARENA_W - width) / 2.0 + 6.0 * pattern.phase.sin();
                let top = self.enemies[main].y + self.enemies[main].h;
                if pattern.record > 0 {
                    pattern.record_timer -= STEP;
                    pattern.record_elapsed += STEP;
                    if pattern.record_timer <= 0.0 {
                        let order = 4 - pattern.record;
                        let activation = SENTINEL_FIRST_SCAN + SCAN_ORDER_GAP * f32::from(order - 1);
                        let warn = activation - pattern.record_elapsed;
                        self.scan_column(self.player.x, top, warn.max(TELEGRAPH), Some(order));
                        pattern.record -= 1;
                        pattern.record_timer = RECORD_GAP;
                    }
                }
                if due {
                    if pattern.step.is_multiple_of(2) {
                        pattern.record = 3;
                        pattern.record_timer = 0.0;
                        pattern.record_elapsed = -STEP;
                        pattern.timer = SENTINEL_FIRST_SCAN + 2.0 * SCAN_ORDER_GAP + SCAN_ACTIVE + 0.6 * pace;
                    } else {
                        let from = self.bottom_center(main);
                        self.fan(from, 5, 72.0);
                        pattern.timer = 1.5 * pace;
                    }
                    pattern.step += 1;
                }
            }
            EncounterKind::Boss(BossKind::RootController) => {
                let main = parts[0];
                self.drift(main, &mut pattern, 3.0);
                let ratio = self.enemies[main].hp / pattern.total_hp.max(1.0);
                let thresholds = [0.75, 0.5, 0.25];
                while (pattern.summons as usize) < thresholds.len() && ratio < thresholds[pattern.summons as usize] {
                    pattern.summons += 1;
                    let hp = device_hp(GUARD_BASE_HP, self.wave) as f32;
                    for side in 0..4 {
                        let at = self.perimeter((3.0, 1.0), Some(side));
                        self.add_enemy(Role::Guard, at, (3.0, 1.0), hp, false, 0);
                        let cooldown = self.rng.range(1.0, 2.5);
                        self.enemies.last_mut().unwrap().cooldown = cooldown;
                    }
                }
                if pattern.stream > 0.0 {
                    pattern.stream -= STEP;
                    pattern.stream_timer -= STEP;
                    if pattern.stream_timer <= 0.0 {
                        pattern.stream_timer = STREAM_GAP;
                        let progress = 1.0 - (pattern.stream / STREAM_TIME).clamp(0.0, 1.0);
                        let angle = STREAM_SWEEP * progress;
                        let e = &self.enemies[main];
                        let left = (e.x + 4.0, e.y + e.h);
                        let right = (e.x + e.w - 4.0, e.y + e.h);
                        let speed = hostile_speed(self.wave);
                        // The warning commits this entire sweep to its
                        // recorded point, just like a marked pair/burst.
                        let target = pattern.stream_target.unwrap_or((self.player.x, self.player.y + 0.5));
                        let heading = direction(self.enemies[main].center(), target).unwrap_or((0.0, 1.0));
                        self.spawn(
                            HostileKind::Stream,
                            left,
                            velocity(rotated(heading, angle.to_radians()), speed * ASPECT),
                        );
                        self.spawn(
                            HostileKind::Stream,
                            right,
                            velocity(rotated(heading, -angle.to_radians()), speed * ASPECT),
                        );
                    }
                }
                if due {
                    match pattern.step % 3 {
                        0 => {
                            // Like every aimed attack, the streams warn first.
                            self.mark_player(main, MarkAttack::Stream, TELEGRAPH);
                            pattern.timer = TELEGRAPH + STREAM_TIME + 1.0 * pace;
                        }
                        1 => {
                            self.mark_player(main, MarkAttack::Pair, TELEGRAPH);
                            pattern.timer = TELEGRAPH + 0.5 * pace;
                        }
                        _ => {
                            let from = self.bottom_center(main);
                            self.fan(from, 5, 80.0);
                            pattern.timer = 1.4 * pace;
                        }
                    }
                    pattern.step += 1;
                }
                let guards: Vec<usize> = (0..self.enemies.len())
                    .filter(|&i| self.enemies[i].alive && self.enemies[i].role == Role::Guard)
                    .collect();
                for guard in guards {
                    self.fly_ship(guard);
                    if self.enemies[guard].entered
                        && self.enemies[guard].visible_in(self.camera())
                        && self.enemies[guard].cooldown <= 0.0
                        && visual_distance(self.enemies[guard].center(), (self.player.x, self.player.y + 0.5)) >= 12.0
                    {
                        self.enemies[guard].cooldown = self.rng.range(1.8, 3.4) * pace;
                        self.mark_shot(guard);
                    }
                }
            }
            EncounterKind::Ordinary => {}
        }
        self.encounter = Encounter::Special(pattern);
    }

    // -- hooks for tests and screenshots ---------------------------------

    /// Integrity never drops; for automated runs that need to see a pattern.
    #[doc(hidden)]
    pub fn set_immortal(&mut self) {
        self.immortal = true;
    }

    /// Start `wave` now, as if every earlier wave had been completed.
    #[doc(hidden)]
    pub fn skip_to_wave(&mut self, wave: u32) {
        self.completed = wave.saturating_sub(1);
        self.countdown = 0.0;
        self.offers.clear();
        self.start_wave(wave.max(1));
    }

    /// Destroy everything in the current encounter, with normal credit.
    #[doc(hidden)]
    pub fn defeat_all(&mut self) {
        if let Encounter::Swarm(mut swarm) = self.encounter {
            let remaining = self.roster[swarm.spawned..].to_vec();
            for kind in remaining {
                self.spawn_device(kind, None);
            }
            swarm.spawned = self.roster.len();
            self.encounter = Encounter::Swarm(swarm);
        }
        for index in 0..self.enemies.len() {
            if self.enemies[index].alive && !matches!(self.enemies[index].role, Role::Guard) {
                self.damage(index, f32::MAX);
            }
        }
    }

    /// Lose every remaining integrity point at once.
    #[doc(hidden)]
    pub fn lose(&mut self) {
        self.immortal = false;
        let all = self.integrity;
        self.lose_integrity(all);
    }

    /// Remove every asteroid, for fixtures that place things by hand.
    #[doc(hidden)]
    pub fn clear_asteroids(&mut self) {
        self.asteroids.clear();
    }

    /// Put a rock at a center with a visual radius and velocity.
    #[doc(hidden)]
    pub fn place_asteroid(&mut self, at: (f32, f32), radius: f32, velocity: (f32, f32)) {
        let id = self.asteroids.iter().map(|a| a.id).max().unwrap_or(0) + 1;
        self.asteroids.push(Asteroid { id, x: at.0, y: at.1, radius, velocity, spin: 0.0, spin_rate: 0.0 });
    }

    /// Point the ship and set its velocity directly.
    #[doc(hidden)]
    pub fn set_flight(&mut self, heading: f32, velocity: (f32, f32)) {
        self.player.heading = wrap_heading(heading);
        self.player.velocity = velocity;
    }

    /// Add an upgrade rank directly, without its one-time effects.
    #[doc(hidden)]
    pub fn grant(&mut self, id: UpgradeId) {
        self.build.add(id);
        self.stats = stats(self.rig, &self.build);
        self.player.charges = self.player.charges.min(self.stats.max_charges);
    }
}

#[cfg(test)]
mod tests;
