//! Model regressions for a whole run, stepped under an explicit clock.

use super::*;
use crate::invade::arsenal::{MAX_WEAPONS, StarterModule, WEAPON_RANK_CAP};
use crate::invade::autoplay::{self, Autoplay, Skill};
use crate::invade::upgrades::UpgradeId;

const IDLE: Input = Input {
    turn_left: false,
    turn_right: false,
    thrust: false,
    brake: false,
    fire: false,
    special: false,
    mobility: false,
    aim: None,
};
const FIRE: Input = Input { fire: true, aim: Some((40.0, -1000.0)), ..IDLE };
const THRUST: Input = Input { thrust: true, ..IDLE };
const BRAKE: Input = Input { brake: true, ..IDLE };
/// Headings: 0 points up and they grow clockwise.
const UP: f32 = 0.0;
const RIGHT: f32 = std::f32::consts::FRAC_PI_2;
const DOWN: f32 = std::f32::consts::PI;
const LEFT: f32 = -std::f32::consts::FRAC_PI_2;

fn started(seed: u64, rig: Rig) -> InvadeRun {
    let mut run = InvadeRun::new(seed, rig, [], 0);
    run.countdown = 0.0;
    run
}

fn seconds(run: &mut InvadeRun, input: Input, seconds: f32) {
    for _ in 0..(seconds / STEP).round() as usize {
        run.step(input);
    }
}

/// An empty arena with nothing attacking, for placing things by hand.
fn quiet(rig: Rig) -> InvadeRun {
    let mut run = started(1, rig);
    run.enemies.clear();
    run.encounter = Encounter::None;
    run.clear_asteroids();
    // Fixtures use the world's left edge so hand-placed coordinates stay in
    // the camera horizontally; free-flight scenarios use the real spawn.
    run.player.x = 40.0;
    run.player.y = 26.0;
    run.player_previous = (run.player.x, run.player.y);
    run
}

fn target(run: &mut InvadeRun, at: (f32, f32), hp: f32) -> u32 {
    run.add_enemy(Role::Device(DeviceKind::Firewall), at, (3.0, 1.0), hp, true, 100)
}

fn hostile_at_player(run: &mut InvadeRun) {
    let x = run.player.x;
    run.hostiles.push(Hostile { kind: HostileKind::Straight, x, y: run.player.y - 0.05, vx: 0.0, vy: 20.0 });
}

fn kill_all(run: &mut InvadeRun) {
    run.defeat_all();
}

/// Clear whatever encounter is running, stepping until it completes.
fn clear_encounter(run: &mut InvadeRun) {
    for _ in 0..1000 {
        if !matches!(run.phase, Phase::Combat) {
            return;
        }
        kill_all(run);
        run.step(IDLE);
    }
    panic!("the encounter never cleared");
}

fn with(rig: Rig, upgrades: &[UpgradeId]) -> InvadeRun {
    let mut run = quiet(rig);
    for &id in upgrades {
        run.build.add(id);
    }
    run.stats = stats(rig, &run.build);
    run.player.charges = run.stats.max_charges;
    run
}

/// Put `choices` on the level screen, as if EXP had just filled the bar.
fn offer(run: &mut InvadeRun, choices: &[LevelChoice]) {
    if run.phase != Phase::Upgrade {
        run.suspended_phase = Some(run.phase);
    }
    run.phase = Phase::Upgrade;
    run.offers = choices.to_vec();
}

/// Drop exactly enough EXP under the ship for one level, and collect it.
fn level_up(run: &mut InvadeRun) {
    while run.countdown > 0.0 {
        run.step(IDLE);
    }
    let missing = run.experience_needed() - run.experience();
    run.spawn_experience(run.player.x, run.player.y + 0.5, missing);
    run.step(IDLE);
    assert_eq!(run.phase, Phase::Upgrade, "the drop under the ship was collected");
}

fn seed_with_miniboss_on(wave: u32) -> u64 {
    (0..1000).find(|&seed| matches!(encounter_for(seed, wave), EncounterKind::Miniboss(_))).unwrap()
}

#[test]
fn the_same_seed_and_inputs_replay_the_same_run() {
    let play = |seed| {
        let mut run = InvadeRun::new(seed, Rig::Probe, [], 0);
        let mut player = Autoplay::new(Skill::EXPERT, 3);
        let mut offers = Vec::new();
        for step in 0..(200.0 / STEP) as usize {
            if run.phase == Phase::Upgrade {
                offers.push(run.offers.clone());
                run.choose(step % run.offers.len());
                continue;
            }
            let input = player.input(&run);
            run.step(input);
        }
        let positions: Vec<(u32, f32, f32, f32)> = run.enemies().map(|e| (e.id, e.x, e.y, e.hp)).collect();
        let rocks: Vec<(f32, f32)> = run.asteroids().iter().map(|a| (a.x, a.y)).collect();
        let flight = (run.player.x, run.player.y, run.player.heading, run.player.velocity);
        (run.score, run.wave, run.completed, run.integrity, flight, positions, rocks, offers, run.elapsed)
    };
    let a = play(77);
    assert_eq!(a, play(77));
    assert!(a.2 >= 2 && !a.7.is_empty(), "the run got somewhere: {:?}", (a.0, a.2));
    assert_ne!(a, play(78));
}

#[test]
fn layouts_and_offers_do_not_depend_on_how_combat_went() {
    let mut a = started(5, Rig::Probe);
    let mut b = started(5, Rig::Probe);
    a.set_immortal();
    b.set_immortal();
    seconds(&mut a, Input { fire: true, thrust: true, turn_left: true, ..IDLE }, 20.0);
    seconds(&mut b, Input { fire: true, thrust: true, ..IDLE }, 3.0);
    for run in [&mut a, &mut b] {
        run.skip_to_wave(3);
        clear_encounter(run);
    }
    assert_eq!(a.offers, b.offers);
    a.skip_to_wave(14);
    b.skip_to_wave(14);
    assert_eq!(a.roster, b.roster);
}

#[test]
fn countdowns_freeze_the_arena_and_are_not_play_time() {
    let mut run = InvadeRun::new(3, Rig::Probe, [], 0);
    assert_eq!(run.countdown(), START_COUNTDOWN);
    let before: Vec<f32> = run.enemies().map(|e| e.x).collect();
    let rocks: Vec<(f32, f32)> = run.asteroids().iter().map(|a| (a.x, a.y)).collect();
    seconds(&mut run, Input { turn_right: true, thrust: true, fire: true, ..IDLE }, 2.9);
    assert_eq!(run.enemies().map(|e| e.x).collect::<Vec<_>>(), before);
    assert_eq!(run.asteroids().iter().map(|a| (a.x, a.y)).collect::<Vec<_>>(), rocks);
    assert_eq!((run.player.x, run.shots.len(), run.elapsed), (WORLD_W / 2.0, 0, 0.0));
    assert_eq!((run.player.heading, run.player.velocity), (0.0, (0.0, 0.0)), "no turning or thrust either");
    seconds(&mut run, IDLE, 0.2);
    assert_eq!(run.countdown(), 0.0);
    seconds(&mut run, IDLE, 1.0);
    // 3.1 s minus the 3 s countdown, then a second more.
    assert!((run.elapsed - 1.1).abs() < 0.02, "{}", run.elapsed);
    run.begin_countdown(RESUME_COUNTDOWN);
    let frozen = run.elapsed;
    seconds(&mut run, IDLE, 0.95);
    assert_eq!(run.elapsed, frozen);
}

#[test]
fn every_rig_turns_thrusts_to_its_top_speed_coasts_brakes_and_stops_at_walls() {
    for rig in Rig::ALL {
        let flight = rig.flight();
        let top = rig.base().move_speed;
        let half = rig.width() / 2.0;
        let mut run = quiet(rig);
        // Turning only turns, and opposite turns cancel.
        seconds(&mut run, Input { turn_right: true, ..IDLE }, 0.25);
        assert!((run.player.heading - flight.turn_rate * 0.25).abs() < 1e-3, "{rig:?}");
        assert_eq!((run.player.x, run.player.y, run.player.velocity), (40.0, 26.0, (0.0, 0.0)));
        let heading = run.player.heading;
        seconds(&mut run, Input { turn_left: true, turn_right: true, ..IDLE }, 0.5);
        assert_eq!(run.player.heading, heading, "{rig:?}: opposite turns cancel");
        seconds(&mut run, Input { turn_left: true, ..IDLE }, 0.25);
        assert!(run.player.heading.abs() < 1e-3 || (run.player.heading - std::f32::consts::TAU).abs() < 1e-3);
        // Thrust builds speed along the nose up to exactly the top speed.
        run.set_flight(RIGHT, (0.0, 0.0));
        (run.player.x, run.player.y) = (20.0, 40.0);
        let mut last = 0.0;
        for _ in 0..((flight.spool / STEP) as usize / 2) {
            run.step(THRUST);
            assert!(run.player.speed() > last, "{rig:?}: thrust keeps accelerating");
            last = run.player.speed();
        }
        seconds(&mut run, THRUST, flight.spool);
        assert!((run.player.speed() - top).abs() < 1e-3, "{rig:?}: top speed {}", run.player.speed());
        assert!(run.player.velocity.0 > 0.0 && run.player.velocity.1.abs() < 1e-3);
        // Releasing coasts: the ship keeps going while drag slows it.
        let (x, speed) = (run.player.x, run.player.speed());
        seconds(&mut run, IDLE, 0.5);
        assert!(run.player.x > x + 3.0 && run.player.speed() < speed && run.player.speed() > speed * 0.5);
        // Braking stops it without reversing, and overrides thrust.
        let x = run.player.x;
        seconds(&mut run, BRAKE, 0.5);
        assert_eq!(run.player.speed(), 0.0, "{rig:?}");
        assert!(run.player.x > x);
        let x = run.player.x;
        seconds(&mut run, Input { thrust: true, brake: true, ..IDLE }, 0.5);
        assert_eq!((run.player.x, run.player.speed()), (x, 0.0), "{rig:?}: brake wins");
        // Walls stop the momentum into them.
        run.set_flight(RIGHT, (top, 0.0));
        run.player.x = WORLD_W - 20.0;
        seconds(&mut run, THRUST, 5.0);
        assert_eq!((run.player.x, run.player.velocity.0), (WORLD_W - half, 0.0));
        run.set_flight(LEFT, (0.0, 0.0));
        run.player.x = 20.0;
        seconds(&mut run, THRUST, 5.0);
        assert_eq!((run.player.x, run.player.velocity.0), (half, 0.0));
    }
}

#[test]
fn fast_packets_collide_without_tunneling_and_misses_miss() {
    let mut run = quiet(Rig::Probe);
    let id = target(&mut run, (40.0, 10.0), 1.0);
    for (x, hits) in [(41.5, true), (44.0, false), (39.9, true)] {
        let before = run.enemies.iter().find(|e| e.id == id).unwrap().hp;
        run.shots.push(Shot {
            kind: ShotKind::Primary,
            x,
            y: 20.0,
            vx: 0.0,
            vy: -1500.0,
            damage: 0.25,
            pierce: 0,
            hit: Vec::new(),
            travelled: 0.0,
            range: 60.0,
            guided: false,
        });
        run.move_shots();
        let enemy = run.enemies.iter().find(|e| e.id == id).unwrap();
        assert_eq!(enemy.hp < before, hits, "x {x}");
    }
    // Hostile packets can't tunnel through the player either.
    let x = run.player.x;
    run.hostiles.push(Hostile { kind: HostileKind::Straight, x, y: 10.0, vx: 0.0, vy: 3000.0 });
    run.move_hostiles();
    assert_eq!(run.integrity, MAX_INTEGRITY - 1);
}

#[test]
fn pierce_combines_with_bastion_and_keeps_65_percent_per_penetration() {
    let mut run = with(Rig::Bastion, &[UpgradeId::Pierce]);
    let ids: Vec<u32> = (0..4).map(|row| target(&mut run, (38.5, 4.0 + row as f32 * 3.0), 100.0)).collect();
    run.player.x = 40.0;
    run.step(FIRE);
    seconds(&mut run, IDLE, 1.0);
    let hp: Vec<f32> = ids.iter().map(|id| run.enemies.iter().find(|e| e.id == *id).unwrap().hp).collect();
    // Nearest first: 3, then 3 × 0.65, then 3 × 0.65²; the fourth is untouched.
    let dealt: Vec<f32> = hp.iter().rev().map(|hp| 100.0 - hp).collect();
    for (dealt, expected) in dealt.iter().zip([3.0, 1.95, 1.2675, 0.0]) {
        assert!((dealt - expected).abs() < 1e-3, "{dealt:?}");
    }
}

#[test]
fn ship_spreads_and_boss_fans_turn_toward_the_player_in_every_quadrant() {
    for player in [(40.0, 3.0), (40.0, 24.0), (5.0, 14.0), (74.0, 14.0)] {
        let mut run = quiet(Rig::Probe);
        (run.player.x, run.player.y) = player;
        run.add_enemy(Role::Device(DeviceKind::Router), (38.5, 13.5), (3.0, 1.0), 100.0, true, 150);
        run.swarm_shot();
        // A ship warns for over a second, then fires at the spot it marked
        // even if the player has moved away since.
        assert!(run.hostiles.is_empty(), "no packet before its warning");
        assert_eq!(run.marks.len(), 1);
        assert!(run.marks[0].is_shot() && run.marks[0].warn >= 1.0);
        run.update_telegraph_flags();
        assert!(run.enemies[0].telegraphing, "the ship shows its warning");
        let recorded = (run.marks[0].x, run.marks[0].y);
        (run.player.x, run.player.y) = ((player.0 + 30.0) % ARENA_W, (player.1 + 10.0) % ARENA_H);
        for _ in 0..=(TELEGRAPH / STEP).ceil() as usize {
            run.update_marks();
        }
        assert_eq!(run.hostiles.len(), 2);
        let expected = direction((40.0, 14.0), recorded).unwrap();
        for h in &run.hostiles {
            assert!(h.vx * expected.0 + h.vy * ASPECT * expected.1 > 0.0);
        }
        run.hostiles.clear();
        (run.player.x, run.player.y) = player;
        let expected = direction((40.0, 14.0), (player.0, player.1 + 0.5)).unwrap();
        run.fan((40.0, 14.0), 5, 80.0);
        let center = run.hostiles[2];
        let heading = unit((center.vx, center.vy * ASPECT)).unwrap();
        assert!((heading.0 - expected.0).abs() < 1e-4 && (heading.1 - expected.1).abs() < 1e-4);
    }
    let (vx, vy) = aimed((10.0, 5.0), (30.0, 15.0), 10.0);
    assert!((10.0 + vx * 10.0 / vy - 30.0).abs() < 1e-3);
}

#[test]
fn fork_packets_share_damage_modifiers_but_do_not_inherit_pierce() {
    for rig in Rig::ALL {
        let mut run = with(
            rig,
            &[UpgradeId::Payload, UpgradeId::HeavyPackets, UpgradeId::Overclock, UpgradeId::Fork, UpgradeId::Pierce],
        );
        run.player.x = 40.0;
        target(&mut run, (38.5, 10.0), 100.0);
        target(&mut run, (36.4, 20.0), 100.0);
        target(&mut run, (40.6, 20.0), 100.0);
        target(&mut run, (34.25, 15.0), 100.0);
        target(&mut run, (42.75, 15.0), 100.0);
        run.step(FIRE);
        seconds(&mut run, IDLE, 0.7);
        let primary = if rig == Rig::Bastion { 3.96 } else { 1.32 };
        let fork = if rig == Rig::Bastion { 1.188 } else { 0.396 };
        assert!((run.enemies[0].hp - (100.0 - primary)).abs() < 1e-4, "{rig:?}");
        for enemy in &run.enemies[1..3] {
            assert!((enemy.hp - (100.0 - fork)).abs() < 1e-4, "{rig:?}: {}", enemy.hp);
        }
        assert!(run.enemies[3..].iter().all(|e| e.hp == 100.0), "{rig:?}: Fork packets pierced");
    }
}

#[test]
fn guidance_steers_only_toward_targets_inside_its_cone() {
    for (offset, steers) in [(1.0, true), (10.0, false)] {
        let mut run = with(Rig::Probe, &[UpgradeId::Guidance]);
        run.player.y = 14.0;
        target(&mut run, (40.0 + offset - 1.5, 8.0), 100.0);
        run.step(FIRE);
        for _ in 0..6 {
            run.move_shots();
        }
        let shot = &run.shots[0];
        assert_eq!(shot.vx > 0.01, steers, "offset {offset}: vx {}", shot.vx);
    }
    // Without Guidance nothing turns.
    let mut run = with(Rig::Probe, &[]);
    run.player.y = 14.0;
    target(&mut run, (40.5, 4.0), 100.0);
    run.step(FIRE);
    run.move_shots();
    assert_eq!(run.shots[0].vx, 0.0);
}

#[test]
fn hits_cost_integrity_with_invulnerability_and_ghost_takes_two() {
    let mut run = quiet(Rig::Probe);
    hostile_at_player(&mut run);
    run.step(IDLE);
    assert_eq!(run.integrity, 4);
    assert!(run.take_events().contains(&Event::Damaged(1)));
    hostile_at_player(&mut run);
    hostile_at_player(&mut run);
    run.step(IDLE);
    assert_eq!(run.integrity, 4, "overlapping packets in the hit window do nothing");
    assert!(run.hostiles.is_empty(), "blocked packets are used up");
    seconds(&mut run, IDLE, 1.0);
    hostile_at_player(&mut run);
    run.step(IDLE);
    assert_eq!(run.integrity, 3);

    let mut ghost = quiet(Rig::Ghost);
    hostile_at_player(&mut ghost);
    ghost.step(IDLE);
    assert_eq!(ghost.integrity, 3);

    let mut graceful = with(Rig::Probe, &[UpgradeId::GracePeriod, UpgradeId::GracePeriod]);
    hostile_at_player(&mut graceful);
    graceful.step(IDLE);
    assert!((graceful.player.invulnerable - 1.5).abs() < 1e-4);
}

#[test]
fn barrier_slip_and_cache_shield_interactions() {
    let mut bastion = quiet(Rig::Bastion);
    bastion.step(Input { special: true, ..IDLE });
    assert!(bastion.player.barrier && bastion.player.charges == 0);
    bastion.set_flight(RIGHT, (0.0, 0.0));
    seconds(&mut bastion, THRUST, 0.5);
    for _ in 0..3 {
        hostile_at_player(&mut bastion);
        seconds(&mut bastion, IDLE, 0.2);
    }
    assert_eq!(bastion.integrity, MAX_INTEGRITY, "the barrier moves with the player");
    seconds(&mut bastion, IDLE, 1.2);
    assert!(!bastion.player.barrier);
    hostile_at_player(&mut bastion);
    bastion.step(IDLE);
    assert_eq!(bastion.integrity, 4);

    let mut ghost = quiet(Rig::Ghost);
    ghost.set_flight(LEFT, (0.0, 0.0));
    let start = ghost.player.x;
    ghost.step(Input { special: true, ..IDLE });
    seconds(&mut ghost, IDLE, 0.2);
    assert!((ghost.player.x - (start - SLIP_DISTANCE)).abs() < 1e-3, "Slip goes where the nose points");
    assert_eq!(ghost.player.speed(), 0.0, "Slip keeps the ship's momentum");
    hostile_at_player(&mut ghost);
    ghost.step(IDLE);
    assert_eq!(ghost.integrity, MAX_INTEGRITY);
    let mut edge = quiet(Rig::Ghost);
    edge.player.x = 3.0;
    edge.set_flight(LEFT, (0.0, 0.0));
    edge.step(Input { special: true, ..IDLE });
    seconds(&mut edge, IDLE, 0.2);
    assert_eq!(edge.player.x, Rig::Ghost.width() / 2.0, "Slip stops at the boundary");

    let mut shielded = with(Rig::Ghost, &[UpgradeId::CacheShield]);
    shielded.player.shield_ready = true;
    hostile_at_player(&mut shielded);
    shielded.step(IDLE);
    assert_eq!(shielded.integrity, MAX_INTEGRITY, "even Ghost's double hit is absorbed");
    assert!(shielded.take_events().contains(&Event::Absorbed));
    assert!(!shielded.player.shield_ready);
    seconds(&mut shielded, IDLE, 1.1);
    hostile_at_player(&mut shielded);
    shielded.step(IDLE);
    assert_eq!(shielded.integrity, 3);
    // It comes back after three completed waves.
    shielded.player.shield_used_at = 4;
    for wave in 5..=7 {
        shielded.skip_to_wave(wave);
        clear_encounter(&mut shielded);
        assert_eq!(shielded.player.shield_ready, wave == 7, "after wave {wave}");
        if shielded.phase == Phase::Upgrade {
            shielded.choose(0);
        }
    }
}

#[test]
fn perimeter_streams_start_outside_respect_live_caps_and_wait_for_the_full_budget() {
    let mut run = started(1, Rig::Probe);
    assert_eq!(run.enemies().count(), OPENING_SHIPS);
    let camera = run.camera();
    assert!(run.enemies().all(|e| !e.visible_in(camera)), "every opening ship starts outside the camera");
    assert!(run.enemies().all(|e| e.x >= 0.0 && e.y >= 0.0 && e.x + e.w <= WORLD_W && e.y + e.h <= WORLD_H));
    let ship = (run.player.x, run.player.y + 0.5);
    let distances: Vec<_> = run.enemies().map(|e| visual_distance(e.center(), ship)).collect();
    seconds(&mut run, IDLE, 0.4);
    for (enemy, before) in run.enemies().zip(distances) {
        assert!(visual_distance(enemy.center(), ship) < before);
    }
    for index in 0..run.enemies.len() {
        run.damage(index, 1e6);
    }
    run.step(IDLE);
    assert_eq!(run.completed_wave(), 0, "unspawned ships still belong to this wave");
    seconds(&mut run, IDLE, 1.0);
    assert!(run.enemies().count() > 0);
    run.set_immortal();
    for _ in 0..(40.0 / STEP) as usize {
        run.step(IDLE);
        assert!(run.enemies().count() <= SWARM_CAP);
    }
    assert_eq!(run.completed_wave(), 0);
    run.defeat_all();
    run.step(IDLE);
    assert_eq!(run.completed_wave(), 1);
    assert_eq!(run.compromised(), 48, "the QA helper drains and credits queued ships too");
    assert_eq!(run.score(), 48 * 100 + 100);
}

#[test]
fn zero_integrity_ends_the_run_and_it_finishes_once() {
    let mut run = quiet(Rig::Probe);
    run.score = 1234;
    run.integrity = 1;
    seconds(&mut run, IDLE, 2.0);
    let elapsed = run.elapsed;
    hostile_at_player(&mut run);
    run.step(IDLE);
    assert_eq!(run.phase, Phase::Over);
    assert!(run.take_events().contains(&Event::Defeated));
    assert!(!run.is_unfinished());
    run.step(Input { turn_right: true, thrust: true, ..FIRE });
    assert_eq!(run.elapsed, elapsed + f64::from(STEP), "nothing advances after defeat");
    let summary = run.finish(EndReason::Closed).unwrap();
    assert_eq!((summary.reason, summary.score, summary.completed_wave), (EndReason::Defeated, 1234, 0));
    assert!(run.finish(EndReason::AppClosed).is_none());
    assert!(run.finish(EndReason::Restarted).is_none());
}

#[test]
fn closing_an_unfinished_run_records_it_once_without_a_clear_reward() {
    let mut run = started(9, Rig::Probe);
    clear_encounter(&mut run);
    run.choose(0);
    seconds(&mut run, IDLE, 3.0);
    assert_eq!((run.wave, run.completed), (2, 1));
    let score = run.score;
    let summary = run.finish(EndReason::Closed).unwrap();
    assert_eq!((summary.score, summary.completed_wave, summary.reason), (score, 1, EndReason::Closed));
    assert!((summary.duration - run.elapsed).abs() < 1e-9);
    assert!(run.finish(EndReason::Closed).is_none());
    assert!(!run.is_unfinished());
}

#[test]
fn specials_start_charged_recharge_sequentially_and_store_two_with_tool_charge() {
    let mut run = quiet(Rig::Probe);
    assert_eq!(run.player.charges, 1);
    run.step(Input { special: true, ..IDLE });
    assert_eq!(run.player.charges, 0);
    run.step(Input { special: true, ..IDLE });
    assert_eq!(run.shots.iter().filter(|s| s.kind == ShotKind::Pivot).count(), 1, "no charge, no second Pivot");
    seconds(&mut run, IDLE, 11.9);
    assert_eq!(run.player.charges, 0);
    seconds(&mut run, IDLE, 0.15);
    assert_eq!(run.player.charges, 1);

    let mut stored = with(Rig::Ghost, &[UpgradeId::ToolCharge, UpgradeId::Heatsink]);
    let recharge = 8.0 * 0.85;
    assert_eq!(stored.player.charges, 2);
    stored.step(Input { special: true, ..IDLE });
    seconds(&mut stored, IDLE, 0.2);
    stored.step(Input { special: true, ..IDLE });
    assert_eq!(stored.player.charges, 0);
    seconds(&mut stored, IDLE, recharge + 0.05);
    assert_eq!(stored.player.charges, 1, "one charge at a time");
    seconds(&mut stored, IDLE, recharge);
    assert_eq!(stored.player.charges, 2);
    // Specials do nothing between waves.
    stored.phase = Phase::Intermission { remaining: 5.0 };
    stored.step(Input { special: true, ..IDLE });
    assert_eq!(stored.player.charges, 2);
}

#[test]
fn feedback_refunds_at_most_twice_per_second_and_only_from_primary_hits() {
    let mut run = with(Rig::Probe, &[UpgradeId::Feedback, UpgradeId::Feedback]);
    run.player.charges = 0;
    run.player.x = 40.0;
    target(&mut run, (38.5, 20.0), 1000.0);
    seconds(&mut run, FIRE, 0.9);
    let hits = run.enemies[0].max_hp - run.enemies[0].hp;
    assert!(hits >= 4.0, "{hits} damage");
    // 0.9 s of recharge plus two refunds of 0.15 s.
    assert!((run.player.recharge - (0.9 + 0.3)).abs() < 0.02, "{}", run.player.recharge);
    let mut pivot = with(Rig::Probe, &[UpgradeId::Feedback]);
    pivot.player.x = 40.0;
    target(&mut pivot, (38.5, 20.0), 1000.0);
    pivot.step(Input { special: true, ..IDLE });
    seconds(&mut pivot, IDLE, 0.5);
    assert!(pivot.enemies[0].hp < 1000.0);
    assert!((pivot.player.recharge - 0.5).abs() < 0.02, "special hits never refund");
}

#[test]
fn pivot_chains_to_distinct_devices_and_tool_amplifier_adds_a_jump_and_damage() {
    for (amplified, expected_damaged) in [(false, 4), (true, 5)] {
        let upgrades: &[UpgradeId] = if amplified { &[UpgradeId::ToolAmplifier] } else { &[] };
        let mut run = with(Rig::Probe, upgrades);
        run.player.x = 40.0;
        let first = target(&mut run, (38.5, 10.0), 100.0);
        for i in 0..6 {
            target(&mut run, (30.0 + 4.0 * i as f32, 8.0), 100.0);
        }
        run.step(Input { special: true, ..IDLE });
        seconds(&mut run, IDLE, 0.5);
        let damaged: Vec<&Enemy> = run.enemies.iter().filter(|e| e.hp < 100.0).collect();
        assert_eq!(damaged.len(), expected_damaged);
        let direct = run.enemies.iter().find(|e| e.id == first).unwrap();
        assert_eq!(100.0 - direct.hp, if amplified { 7.0 } else { 6.0 });
        assert!(damaged.iter().filter(|e| e.id != first).all(|e| e.hp == 97.0));
    }
}

#[test]
fn fragments_and_specials_never_fragment_further_and_kills_credit_once() {
    let mut run = with(Rig::Probe, &[UpgradeId::Fragment, UpgradeId::Fragment]);
    run.player.x = 40.0;
    target(&mut run, (38.5, 10.0), 1.0);
    let left = target(&mut run, (33.0, 10.0), 0.5);
    target(&mut run, (27.0, 10.0), 100.0);
    run.step(FIRE);
    seconds(&mut run, IDLE, 1.0);
    // The kill released fragments; one killed `left`, which released none.
    assert!(!run.enemies.iter().find(|e| e.id == left).unwrap().alive);
    assert_eq!(run.enemies[2].hp, 100.0, "a fragment's kill does not fragment");
    assert_eq!(run.score, 200);
    assert_eq!(run.compromised, 2);

    let mut pivot = with(Rig::Probe, &[UpgradeId::Fragment]);
    pivot.player.x = 40.0;
    target(&mut pivot, (38.5, 10.0), 1.0);
    target(&mut pivot, (33.0, 10.0), 1.0);
    pivot.step(Input { special: true, ..IDLE });
    // A fragment would appear on the very step of a kill, so check each one.
    for _ in 0..(0.8 / STEP).round() as usize {
        pivot.step(IDLE);
        assert!(pivot.shots.iter().all(|s| s.kind != ShotKind::Fragment), "special kills never fragment");
    }
    assert_eq!(pivot.compromised, 2);

    // Simultaneous hits on a shared pool credit it once.
    let mut boss = started(1, Rig::Bastion);
    boss.skip_to_wave(20);
    assert_eq!(boss.enemies.len(), 2);
    let (a, b) = (0, 1);
    boss.enemies[a].hp = 0.5;
    boss.pools[0].hp = 0.5;
    assert!(boss.damage(a, 1.0));
    assert!(!boss.damage(b, 1.0));
    assert!(boss.enemies.iter().all(|e| !e.alive));
    assert_eq!(boss.compromised, 1);
}

#[test]
fn clearing_waves_and_encounters_never_offers_cards_without_collected_experience() {
    let mut run = started(seed_with_miniboss_on(6), Rig::Probe);
    for wave in 1..=6 {
        assert_eq!(run.wave, wave);
        clear_encounter(&mut run);
        assert!(matches!(run.phase, Phase::Intermission { .. }), "wave {wave}: {:?}", run.phase);
        assert!(run.offers.is_empty());
        seconds(&mut run, IDLE, 2.5);
    }
    assert!(run.is_unlocked(Unlock::Fork) && run.is_unlocked(Unlock::Ghost));
    assert_eq!(run.level(), 1, "drops far from the ship stay on the field");
    assert!(!run.orbs().is_empty());
    assert_eq!(run.build.acquired().count(), 0);
}

#[test]
fn score_follows_kills_clear_bonuses_and_encounter_rewards() {
    let mut run = started(1, Rig::Probe);
    clear_encounter(&mut run);
    assert_eq!(run.score, 48 * 100 + 100);
    let seed = seed_with_miniboss_on(4);
    let mut mini = started(seed, Rig::Probe);
    mini.skip_to_wave(4);
    clear_encounter(&mut mini);
    assert_eq!(mini.score, 1500 + 400, "minibosses give their reward, not kill score");
    let mut boss = started(1, Rig::Probe);
    boss.skip_to_wave(10);
    boss.integrity = 2;
    clear_encounter(&mut boss);
    assert_eq!(boss.score, 5000 + 2500);
    assert_eq!(boss.integrity, 3, "a boss repairs one");
    assert!(matches!(boss.phase, Phase::Intermission { .. }), "bosses drop EXP instead of cards");
    let mut backup = with(Rig::Probe, &[UpgradeId::WarmBackup]);
    backup.skip_to_wave(20);
    backup.integrity = 1;
    clear_encounter(&mut backup);
    assert_eq!(backup.integrity, 3, "Warm Backup repairs one more on boss clears");
    let mut cache = started(1, Rig::Probe);
    cache.skip_to_wave(3);
    offer(&mut cache, &[LevelChoice::Module(Offer::DataCache)]);
    let before = cache.score;
    cache.choose(0);
    assert_eq!(cache.score - before, 250 * u64::from(cache.completed));
}

#[test]
fn milestones_unlock_on_waves_minibosses_gateway_and_devices_before_offers() {
    let mut run = started(1, Rig::Probe);
    for id in UpgradeId::ALL {
        if id != UpgradeId::CacheShield {
            while run.build.add(id) {}
        }
    }
    run.skip_to_wave(12);
    clear_encounter(&mut run);
    assert!(run.take_events().contains(&Event::Unlocked(Unlock::CacheShield)));
    level_up(&mut run);
    assert!(
        run.offers.contains(&LevelChoice::Module(Offer::Upgrade(UpgradeId::CacheShield))),
        "offered on the first level after it unlocks"
    );
    for unlock in [Unlock::Ghost, Unlock::CacheShield] {
        assert!(run.earned_unlocks().contains(&unlock));
    }

    let mut gateway = started(1, Rig::Probe);
    gateway.skip_to_wave(10);
    clear_encounter(&mut gateway);
    for unlock in [Unlock::Guidance, Unlock::Bastion, Unlock::Ghost] {
        assert!(gateway.is_unlocked(unlock), "{unlock:?}");
    }
    assert!(!gateway.is_unlocked(Unlock::Fork));

    let mut devices = InvadeRun::new(1, Rig::Probe, [], FRAGMENT_DEVICES - 48);
    devices.countdown = 0.0;
    clear_encounter(&mut devices);
    assert!(devices.is_unlocked(Unlock::Fragment), "48 devices on wave 1 reach 100");
    let mut short = InvadeRun::new(1, Rig::Probe, [], FRAGMENT_DEVICES - 49);
    short.countdown = 0.0;
    clear_encounter(&mut short);
    assert!(!short.is_unlocked(Unlock::Fragment));

    let mut late = started(1, Rig::Probe);
    late.skip_to_wave(30);
    clear_encounter(&mut late);
    for unlock in [Unlock::RootAccess, Unlock::Feedback, Unlock::ToolCharge] {
        assert!(late.is_unlocked(unlock));
    }
    // Already-earned content is not reported again.
    let mut again = InvadeRun::new(1, Rig::Probe, [Unlock::Ghost], 0);
    again.skip_to_wave(6);
    clear_encounter(&mut again);
    assert!(!again.take_events().contains(&Event::Unlocked(Unlock::Ghost)));
}

#[test]
fn fragment_unlocks_on_the_hundredth_credit_even_if_the_run_ends_midwave() {
    for reason in [EndReason::Closed, EndReason::Defeated] {
        let mut run = InvadeRun::new(1, Rig::Probe, [], FRAGMENT_DEVICES - 2);
        run.countdown = 0.0;
        run.damage(0, 1e6);
        let guard = run.enemies.len();
        run.add_enemy(Role::Guard, (10.0, 8.0), (3.0, 1.0), 1.0, false, 0);
        run.damage(guard, 1e6);
        assert_eq!(run.compromised(), 1, "uncredited guards do not advance the milestone");
        assert!(!run.is_unlocked(Unlock::Fragment));
        run.damage(1, 1e6);
        assert!(run.is_unlocked(Unlock::Fragment), "the unlock must not wait for a wave clear");
        assert_eq!((run.phase(), run.completed_wave()), (Phase::Combat, 0));
        run.damage(1, 1e6);
        assert_eq!(run.compromised(), 2);
        assert_eq!(run.take_events().iter().filter(|&&e| e == Event::Unlocked(Unlock::Fragment)).count(), 1);
        if reason == EndReason::Defeated {
            run.lose();
        }
        let summary = run.finish(reason).unwrap();
        assert_eq!(summary.reason, reason);
        assert_eq!(summary.completed_wave, 0);
        assert_eq!(summary.compromised, 2);
        assert_eq!(summary.unlocks, [Unlock::Fragment]);
    }
}

#[test]
fn depleted_pools_offer_only_meaningful_fallbacks() {
    let mut run = started(1, Rig::Probe);
    for id in UpgradeId::ALL {
        while run.build.add(id) {}
    }
    run.stats = stats(Rig::Probe, &run.build);
    run.player.charges = run.stats.max_charges;
    for kind in [WeaponKind::Packet, WeaponKind::Scatter, WeaponKind::Rail, WeaponKind::Arc] {
        while run.grant_weapon(kind) {}
    }
    assert!(!run.grant_weapon(WeaponKind::Orbit), "four slots");
    run.skip_to_wave(3);
    clear_encounter(&mut run);
    level_up(&mut run);
    assert_eq!(run.offers, [LevelChoice::Module(Offer::DataCache)]);
    assert!(run.choose(0));
    run.integrity = 3;
    run.player.charges = 0;
    level_up(&mut run);
    let mut offers = run.offers.clone();
    offers.sort_by_key(|offer| offer.name());
    assert_eq!(
        offers,
        [
            LevelChoice::Module(Offer::DataCache),
            LevelChoice::Module(Offer::Patch),
            LevelChoice::Module(Offer::Recharge)
        ]
    );
    let recharge = run.offers.iter().position(|&o| o == LevelChoice::Module(Offer::Recharge)).unwrap();
    assert!(run.choose(recharge));
    assert_eq!(run.player.charges, run.stats.max_charges);
}

#[test]
fn repairs_from_warm_backup_patch_and_bosses_stop_at_five() {
    for (start, offer, expected) in [
        (2, Offer::Upgrade(UpgradeId::WarmBackup), 4),
        (4, Offer::Upgrade(UpgradeId::WarmBackup), 5),
        (3, Offer::Patch, 4),
        (5, Offer::Patch, 5),
    ] {
        let mut run = started(1, Rig::Probe);
        run.skip_to_wave(3);
        clear_encounter(&mut run);
        run.integrity = start;
        self::offer(&mut run, &[LevelChoice::Module(offer)]);
        assert!(run.choose(0));
        assert_eq!(run.integrity, expected, "{offer:?} from {start}");
        assert!(matches!(run.phase, Phase::Intermission { .. }));
    }
    let mut boss = with(Rig::Probe, &[UpgradeId::WarmBackup]);
    boss.skip_to_wave(10);
    boss.integrity = 5;
    clear_encounter(&mut boss);
    assert_eq!(boss.integrity, MAX_INTEGRITY, "a boss repair never goes past five");
}

#[test]
fn spare_parts_repairs_every_five_completed_waves_after_acquisition() {
    let mut run = started(1, Rig::Probe);
    run.skip_to_wave(3);
    clear_encounter(&mut run);
    offer(&mut run, &[LevelChoice::Module(Offer::Upgrade(UpgradeId::SpareParts))]);
    assert!(run.choose(0));
    run.integrity = 2;
    for wave in 4..=13 {
        run.skip_to_wave(wave);
        clear_encounter(&mut run);
        assert!(matches!(run.phase, Phase::Intermission { .. }), "after wave {wave}");
        let expected = match wave {
            ..=7 => 2,
            8..=9 => 3,
            // Gateway on 10 repairs one too.
            10..=12 => 4,
            _ => 5,
        };
        assert_eq!(run.integrity, expected, "after wave {wave}");
    }
}

#[test]
fn bosses_and_minibosses_appear_on_schedule_with_their_health() {
    for wave in [10, 20, 30, 40, 50] {
        let mut run = started(1, Rig::Probe);
        run.skip_to_wave(wave);
        let EncounterKind::Boss(kind) = run.encounter() else { panic!("wave {wave}") };
        assert_eq!(kind, BossKind::CYCLE[((wave / 10 - 1) % 4) as usize]);
        assert_eq!(run.boss_health(), Some((kind.name(), 1.0)));
        let hp: f32 = if kind == BossKind::LoadBalancer { run.pools[0].hp } else { run.enemies[0].hp };
        assert_eq!(hp, boss_hp(wave) as f32);
    }
    let seed = seed_with_miniboss_on(5);
    let mut run = started(seed, Rig::Probe);
    run.skip_to_wave(5);
    let EncounterKind::Miniboss(kind) = run.encounter() else { panic!() };
    let total: f32 = run.enemies().map(|e| e.hp).sum();
    assert_eq!(total, miniboss_hp(5) as f32);
    assert_eq!(run.enemies().count(), if kind == MinibossKind::RelayPair { 2 } else { 1 });
    assert!(run.banner().unwrap().contains("MINIBOSS"));
}

#[test]
fn relay_pair_survivor_speeds_up_and_root_controller_summons_uncredited_guards() {
    let seed = (0..1000)
        .find(|&s| (1..10).any(|w| encounter_for(s, w) == EncounterKind::Miniboss(MinibossKind::RelayPair)))
        .unwrap();
    let wave = (1..10).find(|&w| encounter_for(seed, w) == EncounterKind::Miniboss(MinibossKind::RelayPair)).unwrap();
    let mut run = started(seed, Rig::Probe);
    run.set_immortal();
    run.skip_to_wave(wave);
    let phase_rate = |run: &mut InvadeRun| {
        let Encounter::Special(before) = run.encounter else { panic!() };
        run.step(IDLE);
        let Encounter::Special(after) = run.encounter else { panic!() };
        after.phase - before.phase
    };
    seconds(&mut run, IDLE, 2.0);
    let both = phase_rate(&mut run);
    run.damage(0, 1e6);
    let one = phase_rate(&mut run);
    assert!((one / both - 1.2).abs() < 1e-3, "{one} vs {both}");

    let mut root = started(1, Rig::Probe);
    root.set_immortal();
    root.skip_to_wave(40);
    seconds(&mut root, IDLE, 2.0);
    let total = boss_hp(40) as f32;
    let guards = |run: &InvadeRun| run.enemies.iter().filter(|e| e.role == Role::Guard).count();
    for (fraction, expected) in [(0.2, 0), (0.15, 4), (0.2, 8), (0.25, 12)] {
        root.damage(0, total * fraction);
        root.step(IDLE);
        assert_eq!(guards(&root), expected);
    }
    let score = root.score;
    let compromised = root.compromised;
    for i in 0..root.enemies.len() {
        if root.enemies[i].role == Role::Guard {
            root.damage(i, 1e6);
        }
    }
    assert_eq!((root.score, root.compromised), (score, compromised), "reinforcements award nothing");
    assert!(root.enemies[0].alive, "the core stays exposed and damageable");
}

#[test]
fn telegraphs_warn_and_commit_to_recorded_two_dimensional_positions() {
    let mut run = started(1, Rig::Probe);
    run.set_immortal();
    run.skip_to_wave(30);
    let mut labels = Vec::new();
    for step in 0..(10.0 / STEP) as usize {
        let weave = step % 200;
        run.step(Input { thrust: true, turn_left: weave < 40, turn_right: (100..140).contains(&weave), ..IDLE });
        for scan in &run.scans {
            if let Some(label) = scan.label
                && !labels.iter().any(|(l, _, _)| *l == label)
            {
                labels.push((label, scan.warn, scan.x));
            }
        }
        if labels.len() == 3 {
            break;
        }
    }
    assert_eq!(labels.iter().map(|l| l.0).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(labels.iter().all(|(_, warn, _)| *warn >= TELEGRAPH));

    let mut switch = quiet(Rig::Probe);
    (switch.player.x, switch.player.y) = (20.0, 16.0);
    switch.add_enemy(Role::Device(DeviceKind::Switch), (50.0, 4.0), (3.0, 1.0), 8.0, true, 200);
    switch.swarm_telegraph();
    let mark = switch.marks[0];
    assert_eq!((mark.x, mark.y), (20.0, 16.5));
    assert!(mark.warn >= 1.0);
    // Fly away along the row; the packet still goes to the marked spot.
    switch.set_flight(LEFT, (0.0, 0.0));
    seconds(&mut switch, THRUST, TELEGRAPH + 0.05);
    assert!(switch.player.x < 15.0, "the ship left the mark");
    let shot = switch.hostiles[0];
    let t = (mark.y - shot.y) / shot.vy;
    assert!((shot.x + shot.vx * t - mark.x).abs() < 0.01);
}

#[test]
fn scans_and_bidirectional_curtains_hit_the_actual_player_position() {
    for top in [0.0, 20.0] {
        let mut run = quiet(Rig::Probe);
        run.player.y = 14.0;
        run.player_previous = (run.player.x, run.player.y);
        run.scans.push(Scan {
            x: run.player.x,
            top,
            bottom: top + ARENA_H,
            warn: 0.0,
            active: 0.4,
            label: None,
            source: None,
            hit: false,
        });
        run.update_scans();
        assert_eq!(run.integrity(), if top == 0.0 { 4 } else { 5 });
    }
    for sign in [-1.0, 1.0] {
        for safe in [false, true] {
            let mut run = quiet(Rig::Probe);
            run.player.y = 14.0;
            run.player_previous = (run.player.x, run.player.y);
            run.curtains.push(Curtain {
                left: 0.0,
                right: WORLD_W,
                top: 0.0,
                bottom: WORLD_H,
                y: 14.5 - sign * 0.6,
                gap_left: if safe { 34.0 } else { 4.0 },
                gap_width: 12.0,
                warn: 0.0,
                speed: sign * 120.0,
                hit: false,
            });
            run.update_curtains();
            assert_eq!(run.integrity(), if safe { 5 } else { 4 });
        }
    }
}

#[test]
fn every_boss_and_miniboss_can_be_survived_with_the_slowest_build() {
    let encounters: Vec<(u64, u32)> = [10, 20, 30, 40]
        .into_iter()
        .map(|w| (1, w))
        .chain(MinibossKind::ALL.into_iter().map(|kind| {
            let seed = (0..2000).find(|&s| encounter_for(s, 7) == EncounterKind::Miniboss(kind)).unwrap();
            (seed, 7)
        }))
        .collect();
    for (seed, wave) in encounters {
        let mut run = started(seed, Rig::Bastion);
        for id in [UpgradeId::Grounded, UpgradeId::Payload, UpgradeId::Payload, UpgradeId::HeavyPackets] {
            run.build.add(id);
        }
        // Later bosses represent a developed run, not four early offensive
        // choices stretched through wave 40. Movement remains the slowest
        // legal speed: Bastion + Grounded, with no Light Frame ranks.
        if wave >= 20 {
            for id in [UpgradeId::Heatsink, UpgradeId::CacheShield] {
                run.build.add(id);
            }
            run.player.shield_ready = true;
        }
        if wave >= 30 {
            for id in [UpgradeId::GracePeriod, UpgradeId::ToolAmplifier] {
                run.build.add(id);
            }
        }
        if wave >= 40 {
            for id in [UpgradeId::Heatsink, UpgradeId::ToolAmplifier, UpgradeId::GracePeriod] {
                run.build.add(id);
            }
        }
        run.stats = stats(Rig::Bastion, &run.build);
        run.skip_to_wave(wave);
        let mut player = Autoplay::new(Skill::EXPERT, seed);
        let mut max_hostiles = 0;
        autoplay::play(&mut run, &mut player, 240.0, |run| {
            max_hostiles = max_hostiles.max(run.hostiles.len());
            run.completed >= wave
        });
        assert!(max_hostiles <= BOSS_HOSTILE_CAP);
        assert_eq!(
            run.completed,
            wave,
            "{:?} on wave {wave} (seed {seed}) was not cleared: integrity {}, elapsed {:.1}, boss {:?}",
            run.encounter(),
            run.integrity,
            run.elapsed,
            run.boss_health()
        );
        assert!(run.integrity >= 2, "{:?}: integrity {}", run.encounter(), run.integrity);
    }
}

#[test]
fn long_runs_stay_bounded_and_difficulty_stops_scaling() {
    let mut run = started(21, Rig::Probe);
    run.set_immortal();
    use UpgradeId::*;
    for id in [Payload, Payload, Payload, ClockBoost, ClockBoost, Fork, Pierce, Pierce, Fragment, Fragment, Guidance] {
        run.build.add(id);
    }
    run.stats = stats(Rig::Probe, &run.build);
    let mut player = Autoplay::new(Skill::EXPERT, 21);
    let mut checks = 0;
    for start in [1, 37, 58, 64] {
        if start > 1 {
            run.skip_to_wave(start);
        }
        let goal = start + 3;
        let seconds = autoplay::play(&mut run, &mut player, 900.0, |run| {
            checks += 1;
            assert!(run.hostiles.len() <= run.hostile_cap());
            assert!(run.shots.len() <= MAX_SHOTS + 3);
            assert!(run.shots.iter().filter(|s| s.kind == ShotKind::Fragment).count() <= MAX_FRAGMENTS);
            assert!(run.effects.len() <= MAX_EFFECTS);
            assert!(run.enemies.len() <= 60);
            assert!(run.scans.len() + run.marks.len() <= 8);
            run.completed >= goal
        });
        assert!(run.completed >= goal, "stuck on wave {} after {seconds} s", run.wave);
    }
    assert!(checks > 10_000);
    assert_eq!(device_hp(4, 64), device_hp(4, 60));
    assert_eq!(hostile_speed(64), hostile_speed(60));
}

#[test]
fn thrust_reaches_the_same_top_speed_in_every_direction_and_corners_stop_momentum() {
    for rig in Rig::ALL {
        for heading in [UP, RIGHT, DOWN, LEFT, 0.6, 2.3, -1.1] {
            let mut run = quiet(rig);
            (run.player.x, run.player.y) = (WORLD_W / 2.0, PLAYER_START_Y);
            run.set_flight(heading, (0.0, 0.0));
            seconds(&mut run, THRUST, 2.0 * rig.flight().spool);
            let v = run.player.velocity;
            let n = run.player.nose();
            assert!((run.player.speed() - rig.base().move_speed).abs() < 1e-3, "{rig:?} at {heading}");
            assert!((v.0 * n.1 - v.1 * n.0).abs() < 1e-3, "{rig:?}: flying along the nose");
        }
        let half = rig.width() / 2.0;
        let mut run = quiet(rig);
        (run.player.x, run.player.y) = (12.0, 6.0);
        run.set_flight(heading_of((-1.0, -1.0)), (0.0, 0.0));
        seconds(&mut run, THRUST, 10.0);
        assert_eq!((run.player.x, run.player.y, run.player.speed()), (half, 0.0, 0.0));
        // The camera stays centered on the ship and shows space past the edge.
        assert_eq!(run.camera(), (half - ARENA_W / 2.0, 0.5 - ARENA_H / 2.0));
        (run.player.x, run.player.y) = (WORLD_W - 12.0, WORLD_H - 6.0);
        run.set_flight(heading_of((1.0, 1.0)), (0.0, 0.0));
        seconds(&mut run, THRUST, 10.0);
        assert_eq!((run.player.x, run.player.y, run.player.speed()), (WORLD_W - half, WORLD_H - 1.0, 0.0));
    }
}

#[test]
fn automatic_aim_uses_nearest_visible_enemy_with_stable_ties_and_retains_heading() {
    let mut run = quiet(Rig::Probe);
    run.player.y = 14.0;
    target(&mut run, (5.0, 14.0), 100.0);
    target(&mut run, (72.0, 14.0), 100.0);
    // Nearer than either, but one row past the bottom of the camera.
    target(&mut run, (38.5, 28.6), 100.0);
    assert!(!run.enemies[2].visible_in(run.camera()));
    run.step(IDLE);
    assert_eq!(run.player.aim, (-1.0, 0.0), "equal distances choose the lower entity ID; offscreen ships are ignored");
    run.damage(0, 1e6);
    run.step(IDLE);
    assert_eq!(run.player.aim, (1.0, 0.0));
    run.damage(1, 1e6);
    run.step(IDLE);
    assert_eq!(run.player.aim, (1.0, 0.0), "empty visible space retains aim");
    run.step(Input { aim: Some((40.0, 20.0)), ..IDLE });
    assert_eq!(run.player.aim, (0.0, 1.0), "utilities may explicitly override automatic aim");
}

#[test]
fn automatic_fire_hits_close_targets_in_every_direction_instead_of_skipping_the_muzzle_gap() {
    for rig in Rig::ALL {
        for offset in [(0.0, 0.75), (0.0, -0.75), (0.75, 0.0), (-0.75, 0.0)] {
            let mut run = quiet(rig);
            run.player.y = 14.0;
            target(&mut run, (40.0 + offset.0 - 1.5, 14.0 + offset.1), 100.0);
            run.step(Input { fire: true, ..IDLE });
            assert!((100.0 - run.enemies[0].hp - rig.base().damage).abs() < 1e-4, "{rig:?}, {offset:?}");
        }
    }
}

#[test]
fn mobility_is_rig_specific_follows_the_nose_and_is_independent_of_x_special_charges() {
    for rig in Rig::ALL {
        let mut run = quiet(rig);
        run.player.y = 14.0;
        run.set_flight(RIGHT, (0.0, 0.0));
        let mut plain = quiet(rig);
        plain.set_flight(RIGHT, (0.0, 0.0));
        run.step(Input { mobility: true, ..IDLE });
        assert_eq!(run.player.charges, 1);
        assert_eq!(run.player.mobility_cooldown, rig.mobility_recharge());
        match rig {
            Rig::Probe => {
                while run.player.dash.is_some() {
                    run.step(IDLE);
                }
                assert!((run.player.x - 52.0).abs() < 0.01, "{}", run.player.x);
                assert!((run.player.speed() - rig.base().move_speed).abs() < 1e-3, "Dash leaves it flying");
                assert!(run.player.velocity.0 > 0.0);
            }
            Rig::Ghost => {
                assert!((run.player.x - 54.0).abs() < 0.01 && (run.player.y - 14.0).abs() < 1e-3);
                assert_eq!(run.player.speed(), 0.0, "Blink keeps the momentum it had");
            }
            Rig::Bastion => {
                seconds(&mut run, THRUST, 0.25);
                seconds(&mut plain, THRUST, 0.25);
                assert!((run.player.speed() / plain.player.speed() - BOOST).abs() < 0.01);
                seconds(&mut run, THRUST, 2.0);
                assert!((run.player.speed() - rig.base().move_speed).abs() < 0.01, "back to normal top speed");
            }
        }
        let cooldown = run.player.mobility_cooldown;
        run.step(Input { mobility: true, special: true, ..IDLE });
        assert_eq!(run.player.charges, 0, "X still works while Shift recharges");
        assert!(run.player.mobility_cooldown < cooldown, "a repeated press does not restart the cooldown");
        (run.player.x, run.player.y) = (rig.width() / 2.0 + 1.0, 0.5);
        run.set_flight(heading_of((-1.0, -1.0)), (0.0, 0.0));
        seconds(&mut run, IDLE, 10.0);
        run.step(Input { mobility: true, ..IDLE });
        seconds(&mut run, IDLE, 0.3);
        assert!(run.player.x >= rig.width() / 2.0 && run.player.y >= 0.0);
    }
}

#[test]
fn player_motion_cannot_tunnel_through_stationary_packets_or_ship_bodies() {
    for body in [false, true] {
        let mut run = quiet(Rig::Probe);
        run.player.x = 10.0;
        run.player.y = 14.0;
        run.stats.move_speed = 8000.0;
        run.set_flight(RIGHT, (8000.0, 0.0));
        if body {
            target(&mut run, (38.5, 14.0), 100.0);
        } else {
            run.hostiles.push(Hostile { kind: HostileKind::Straight, x: 40.0, y: 14.5, vx: 0.0, vy: 0.0 });
        }
        run.step(IDLE);
        assert_eq!(run.integrity(), 4, "body={body}");
    }
}

#[test]
fn player_packets_leave_every_arena_edge_and_fragments_follow_impact_orientation() {
    for point in [(-100.0, 14.5), (200.0, 14.5), (40.0, -100.0), (40.0, 100.0)] {
        let mut run = quiet(Rig::Probe);
        run.player.y = 14.0;
        run.step(Input { fire: true, aim: Some(point), ..IDLE });
        seconds(&mut run, IDLE, 2.0);
        assert!(run.shots.is_empty(), "packets expired toward {point:?}");
    }
    let mut run = with(Rig::Probe, &[UpgradeId::Fragment]);
    run.fragment(1, (40.0, 14.0), 1.0, (30.0, 0.0));
    assert_eq!(run.new_shots.len(), 2);
    assert!(run.new_shots.iter().all(|s| s.vx.abs() < 1e-4 && s.vy.abs() > 1.0));
    assert!(run.new_shots[0].vy * run.new_shots[1].vy < 0.0);
}

#[test]
fn guidance_takes_the_short_turn_across_the_angle_boundary() {
    let mut run = quiet(Rig::Probe);
    let angle = 179.0f32.to_radians();
    let (vx, vy) = velocity((angle.cos(), angle.sin()), 40.0);
    let mut shot = Shot {
        kind: ShotKind::Primary,
        x: 40.0,
        y: 14.0,
        vx,
        vy,
        damage: 1.0,
        pierce: 0,
        hit: Vec::new(),
        travelled: 0.0,
        range: 60.0,
        guided: true,
    };
    target(&mut run, (28.5, 13.4127), 100.0);
    for _ in 0..5 {
        run.steer(&mut shot);
    }
    assert!(shot.vx < 0.0 && shot.vy < 0.0, "the guided shot crosses +180/-180 toward the target");
}

#[test]
fn a_blink_during_a_slip_lands_where_the_blink_goes() {
    for delay in 1..=10 {
        let mut run = quiet(Rig::Ghost);
        run.player.x = 30.0;
        run.player.y = 14.0;
        run.set_flight(RIGHT, (0.0, 0.0));
        run.step(Input { special: true, ..IDLE });
        for _ in 0..delay {
            run.step(IDLE);
        }
        let before = (run.player.x, run.player.y);
        run.set_flight(UP, run.player.velocity);
        run.step(Input { mobility: true, ..IDLE });
        let blinked = (run.player.x, run.player.y);
        assert!((blinked.1 - (before.1 - 14.0 / ASPECT).max(0.0)).abs() < 1e-3, "delay {delay}: {blinked:?}");
        seconds(&mut run, IDLE, 0.3);
        assert_eq!((run.player.x, run.player.y), blinked, "delay {delay}: the Slip took the ship back");
    }
}

#[test]
fn a_blink_updates_automatic_and_explicit_aim_before_same_frame_fire() {
    for aim in [None, Some((40.0, 0.0))] {
        let mut run = quiet(Rig::Ghost);
        run.player.x = 40.0;
        run.player.y = 14.0;
        run.set_flight(RIGHT, (0.0, 0.0));
        target(&mut run, (38.5, 7.5), 100.0);
        target(&mut run, (58.5, 13.5), 100.0);
        run.step(Input { mobility: true, fire: true, aim, ..IDLE });
        assert!((run.player.x - 54.0).abs() < 1e-4 && (run.player.y - 14.0).abs() < 1e-4);
        let expected = direction((54.0, 14.5), aim.unwrap_or((60.0, 14.0))).unwrap();
        let shot = run.shots.iter().find(|s| s.kind == ShotKind::Primary).unwrap();
        let actual = unit((shot.vx, shot.vy * ASPECT)).unwrap();
        assert!(
            (actual.0 - expected.0).abs() < 1e-5 && (actual.1 - expected.1).abs() < 1e-5,
            "aim {aim:?}: {actual:?} instead of {expected:?}"
        );
    }
}

#[test]
fn blinking_across_a_curtain_does_not_consume_a_later_real_collision() {
    let mut run = quiet(Rig::Ghost);
    run.player.y = 14.0;
    run.set_flight(DOWN, (0.0, 0.0));
    run.curtains.push(Curtain {
        left: 0.0,
        right: WORLD_W,
        top: 0.0,
        bottom: WORLD_H,
        y: 18.0,
        gap_left: 4.0,
        gap_width: 9.0,
        warn: 0.0,
        speed: 10.0,
        hit: false,
    });
    run.step(Input { mobility: true, ..IDLE });
    assert_eq!(run.player.y, 21.0);
    assert_eq!(run.integrity(), 5);
    assert!(!run.curtains[0].hit, "instant travel does not collide with the intervening wall");
    // The wall reaches the destination after Blink's 0.25 s protection.
    seconds(&mut run, IDLE, 0.4);
    assert_eq!(run.integrity(), 3, "the later physical overlap must still damage Ghost");
    assert!(run.curtains[0].hit);
}

#[test]
fn protected_curtain_crossings_do_not_consume_the_wall_after_the_ship_moves_away() {
    let mut run = quiet(Rig::Probe);
    run.player.y = 14.0;
    run.player.protection = 0.1;
    run.stats.move_speed = 1200.0;
    run.set_flight(DOWN, (0.0, 1200.0));
    run.curtains.push(Curtain {
        left: 0.0,
        right: WORLD_W,
        top: 0.0,
        bottom: WORLD_H,
        y: 16.0,
        gap_left: 4.0,
        gap_width: 9.0,
        warn: 0.0,
        speed: 0.0,
        hit: false,
    });
    run.step(IDLE);
    assert!((run.player.y - 19.0).abs() < 0.05, "{}", run.player.y);
    assert_eq!(run.integrity(), 5);
    assert!(!run.curtains[0].hit, "protection resets this wall even outside the destination row");
    run.player.y = 14.0;
    run.player.protection = 0.0;
    run.set_flight(DOWN, (0.0, 1200.0));
    run.step(IDLE);
    assert_eq!(run.integrity(), 4);
    assert!(run.curtains[0].hit);
}

#[test]
fn every_single_aimed_packet_follows_a_warning_from_its_source() {
    let seed_for = |kind| (0..2000u64).find(|&s| encounter_for(s, 7) == EncounterKind::Miniboss(kind)).unwrap();
    let cases = [(seed_for(MinibossKind::PortSentry), 7), (seed_for(MinibossKind::TraceHunter), 7), (1, 40), (1, 3)];
    for (seed, wave) in cases {
        let mut run = started(seed, Rig::Probe);
        run.set_immortal();
        run.skip_to_wave(wave);
        if wave == 40 {
            // Summon the guards.
            let total = boss_hp(40) as f32;
            run.damage(0, total * 0.3);
        }
        let mut player = Autoplay::new(Skill::EXPERT, seed);
        let mut fired = 0;
        for _ in 0..(40.0 / STEP) as usize {
            if run.phase != Phase::Combat {
                break;
            }
            // Shot marks that fire this step, and the packets already flying.
            let due = run.marks.iter().filter(|m| m.attack == MarkAttack::Shot && m.warn <= STEP).count();
            let before = run.hostiles.iter().filter(|h| h.kind == HostileKind::Straight).count();
            let input = player.input(&run);
            run.step(input);
            let after = run.hostiles.iter().filter(|h| h.kind == HostileKind::Straight).count();
            assert!(after <= before + due, "wave {wave}: a single packet fired without a warning");
            fired += after.saturating_sub(before);
            for mark in run.marks.iter().filter(|m| m.is_shot()) {
                assert!(mark.warn <= TELEGRAPH && mark.warn > 0.0);
            }
        }
        assert!(fired > 0, "wave {wave} (seed {seed}) never fired a single packet");
    }
}

/// Simulated arrival at the first boss for the automatic players. Not a
/// substitute for playtesting; run with `--ignored --nocapture`.
#[test]
#[ignore]
fn pacing_report() {
    const SEEDS: u64 = 20;
    for (name, skill) in [("expert", Skill::EXPERT), ("typical", Skill::TYPICAL)] {
        let mut arrivals = Vec::new();
        let mut levels = Vec::new();
        let mut waves_seen = Vec::new();
        for seed in 1..=SEEDS {
            let mut run = InvadeRun::new(seed, Rig::Probe, [], 0);
            let mut player = Autoplay::new(skill, seed);
            let mut last = 0.0;
            let mut seconds = 0.0;
            while run.wave < 10 && run.phase != Phase::Over && seconds < 1800.0 {
                if run.phase == Phase::Upgrade {
                    let c = player.choose(&run);
                    run.choose(c);
                    continue;
                }
                let before = run.completed;
                let input = player.input(&run);
                run.step(input);
                seconds += STEP;
                if run.completed != before {
                    if run.completed <= 4 && run.encounter() == EncounterKind::Ordinary {
                        waves_seen.push(run.elapsed - last);
                    }
                    last = run.elapsed;
                }
            }
            if run.wave >= 10 {
                arrivals.push(run.elapsed / 60.0);
                levels.push(run.level() - 1);
            }
            println!(
                "{name} seed {seed}: reached wave {} at {:.1} min, integrity {}, {} level choices",
                run.wave,
                run.elapsed / 60.0,
                run.integrity,
                run.level() - 1
            );
        }
        arrivals.sort_by(f64::total_cmp);
        let median = arrivals.get(arrivals.len() / 2).copied().unwrap_or(f64::NAN);
        let early = waves_seen.iter().sum::<f64>() / waves_seen.len().max(1) as f64;
        levels.sort_unstable();
        let choices = levels.get(levels.len() / 2).copied().unwrap_or(0);
        println!(
            "{name}: {}/{SEEDS} reached wave 10; median arrival {median:.1} min of active play (range {:.1}-{:.1}); waves 1-4 average {early:.0} s; median {choices} level choices before the boss",
            arrivals.len(),
            arrivals.first().copied().unwrap_or(f64::NAN),
            arrivals.last().copied().unwrap_or(f64::NAN),
        );
    }
}

#[test]
fn experience_drops_where_ships_die_and_counts_only_once_collected() {
    let mut run = quiet(Rig::Probe);
    target(&mut run, (200.0, 26.0), 1.0);
    run.damage(0, 10.0);
    assert_eq!(run.orbs().len(), 1);
    assert_eq!((run.orbs()[0].x, run.orbs()[0].y, run.orbs()[0].value), (201.5, 26.5, 1));
    seconds(&mut run, IDLE, 1.0);
    assert_eq!((run.experience(), run.orbs().len()), (0, 1), "a remote kill awards nothing until collected");
    target(&mut run, (44.0, 26.0), 1.0);
    run.damage(1, 10.0);
    seconds(&mut run, IDLE, 1.0);
    assert_eq!((run.experience(), run.orbs().len()), (1, 1), "a nearby drop is pulled in and collected");
    // Flying to the remote orb collects it too.
    run.set_flight(RIGHT, (0.0, 0.0));
    for _ in 0..(10.0 / STEP) as usize {
        if run.orbs().is_empty() {
            break;
        }
        run.step(THRUST);
    }
    assert_eq!((run.experience(), run.orbs().len()), (2, 0));

    // Each boss leaves 48 once, even a shared health pool; minibosses 24.
    for wave in [10, 20, 30, 40] {
        let mut boss = started(1, Rig::Probe);
        boss.skip_to_wave(wave);
        boss.defeat_all();
        assert_eq!(boss.orbs().iter().map(|o| o.value).sum::<u64>(), 48, "wave {wave}");
    }
    for kind in MinibossKind::ALL {
        let seed = (0..2000).find(|&s| encounter_for(s, 7) == EncounterKind::Miniboss(kind)).unwrap();
        let mut mini = started(seed, Rig::Probe);
        mini.skip_to_wave(7);
        mini.defeat_all();
        assert_eq!(mini.orbs().iter().map(|o| o.value).sum::<u64>(), 24, "{kind:?}");
    }
}

#[test]
fn collector_pulls_experience_from_farther_away() {
    for (module, collected) in [(StarterModule::Collector, true), (StarterModule::Payload, false)] {
        let mut run = InvadeRun::new_with_loadout(1, Loadout { module, ..Loadout::default() }, [], 0);
        run.countdown = 0.0;
        run.enemies.clear();
        run.encounter = Encounter::None;
        run.clear_asteroids();
        run.spawn_experience(run.player.x + 9.0, run.player.y + 0.5, 1);
        seconds(&mut run, IDLE, 1.0);
        assert_eq!(run.experience() == 1, collected, "{module:?}");
    }
}

#[test]
fn excess_experience_queues_each_level_and_resumes_with_the_remainder() {
    let mut run = quiet(Rig::Probe);
    assert_eq!((run.level(), run.experience_needed()), (1, 12));
    run.spawn_experience(run.player.x, run.player.y + 0.5, 12 + 20 + 5);
    run.step(IDLE);
    assert_eq!((run.phase, run.level(), run.experience()), (Phase::Upgrade, 2, 25));
    assert!(run.take_events().contains(&Event::Leveled(2)));
    let time = run.elapsed;
    seconds(&mut run, Input { turn_right: true, thrust: true, ..IDLE }, 1.0);
    assert_eq!((run.elapsed, run.player.x), (time, 40.0), "nothing advances while choosing");
    assert_eq!((run.player.heading, run.player.velocity), (0.0, (0.0, 0.0)), "or turns or thrusts");
    assert!(run.choose(0));
    assert_eq!((run.phase, run.level(), run.experience()), (Phase::Upgrade, 3, 5), "the next level waits");
    assert!(run.take_events().contains(&Event::Leveled(3)));
    assert_eq!(run.countdown(), 0.0);
    assert!(run.choose(0));
    assert_eq!((run.phase, run.countdown()), (Phase::Combat, RESUME_COUNTDOWN));
    assert_eq!((run.level(), run.experience(), run.experience_needed()), (3, 5, 28));
    assert!(!run.choose(0), "one choice per level");

    // A level reached between waves returns to the same intermission.
    let mut between = started(1, Rig::Probe);
    clear_encounter(&mut between);
    let Phase::Intermission { remaining } = between.phase else { panic!("{:?}", between.phase) };
    level_up(&mut between);
    assert!(between.choose(0));
    assert_eq!(between.phase, Phase::Intermission { remaining });
}

#[test]
fn experience_orbs_stay_bounded_by_merging_without_losing_value() {
    let mut run = quiet(Rig::Probe);
    let mut total = 0u64;
    for n in 0..600u32 {
        let value = 1 + n % 3;
        total += u64::from(value);
        run.spawn_experience(100.0 + (n % 40) as f32 * 3.0, 50.0 + (n / 40) as f32 * 2.0, value);
    }
    assert_eq!(run.orbs().len(), MAX_XP_ORBS);
    assert_eq!(run.orbs().iter().map(|o| o.value).sum::<u64>(), total);
    // Drops outside the world land on its edge, where the ship can reach them.
    run.spawn_experience(-50.0, WORLD_H + 9.0, 1);
    let last = run.orbs().last().unwrap();
    assert_eq!((last.x, last.y), (0.0, WORLD_H));
}

#[test]
fn arrivals_start_outside_the_current_camera_after_travel_and_at_world_corners() {
    for corner in [false, true] {
        let mut run = started(2, Rig::Probe);
        run.set_immortal();
        if corner {
            (run.player.x, run.player.y) = (run.rig.width() / 2.0, 0.0);
            run.player_previous = (run.player.x, run.player.y);
            run.set_flight(heading_of((-1.0, -1.0)), (0.0, 0.0));
        }
        let mut seen: Vec<u32> = run.enemies().map(|e| e.id).collect();
        let mut arrivals = 0;
        for step in 0..(14.0 / STEP) as usize {
            let input = if corner { THRUST } else { Input { thrust: true, turn_right: step % 720 < 60, ..IDLE } };
            run.step(input);
            let camera = run.camera();
            for e in run.enemies() {
                if seen.contains(&e.id) {
                    continue;
                }
                seen.push(e.id);
                arrivals += 1;
                assert!(!e.visible_in(camera), "corner={corner}: ship {} appeared on screen at {:?}", e.id, (e.x, e.y));
                assert!(e.x >= 0.0 && e.y >= 0.0 && e.x + e.w <= WORLD_W && e.y + e.h <= WORLD_H);
            }
        }
        assert!(arrivals >= 8, "corner={corner}: only {arrivals} arrivals");
    }
}

#[test]
fn distant_stragglers_return_offscreen_without_kills_experience_or_healing() {
    let mut run = started(3, Rig::Probe);
    run.set_immortal();
    run.step(IDLE);
    (run.player.x, run.player.y) = (20.0, 42.0);
    run.player_previous = (run.player.x, run.player.y);
    let id = run.enemies[0].id;
    (run.enemies[0].x, run.enemies[0].y) = (WORLD_W - 4.0, 42.0);
    run.enemies[0].previous = (run.enemies[0].x, run.enemies[0].y);
    run.enemies[0].velocity = (20.0, 0.0);
    run.enemies[0].hp *= 0.5;
    let hp = run.enemies[0].hp;
    let (score, compromised) = (run.score, run.compromised);
    run.step(IDLE);
    let ship = (run.player.x, run.player.y + 0.5);
    let e = run.enemies.iter().find(|e| e.id == id).unwrap();
    assert!(e.alive && e.hp == hp, "the straggler keeps its damage");
    assert!(visual_distance(e.center(), ship) < RECYCLE_DISTANCE);
    assert!(!e.visible_in(run.camera()));
    let toward = direction(e.center(), ship).unwrap();
    let inbound = e.velocity.0 * toward.0 + e.velocity.1 * toward.1;
    assert!(inbound > 0.0, "it comes back toward the ship, not away: {:?}", e.velocity);
    assert_eq!((run.score, run.compromised, run.orbs().len()), (score, compromised, 0));
}

#[test]
fn bosses_trail_the_camera_more_slowly_than_the_ship_and_cannot_be_left_behind() {
    let slowest = stats(Rig::Bastion, &Build::default().with(UpgradeId::Grounded)).move_speed;
    assert!(FRAME_SPEED < slowest, "{FRAME_SPEED} vs {slowest}");
    let boss = |run: &InvadeRun| run.enemies().find(|e| matches!(e.role, Role::Boss(_))).unwrap().center();
    for wave in [10, 20, 30, 40] {
        let mut run = started(1, Rig::Probe);
        run.set_immortal();
        run.skip_to_wave(wave);
        seconds(&mut run, IDLE, 4.0);
        let (start, from) = (boss(&run), run.player.x);
        run.set_flight(RIGHT, (0.0, 0.0));
        seconds(&mut run, THRUST, 1.0);
        let followed = boss(&run).0 - start.0;
        assert!(
            followed > 0.0 && followed < run.player.x - from,
            "wave {wave}: the boss follows ({followed:.1}) but more slowly than the ship"
        );
    }
    // Even the fastest rig fleeing in any direction keeps the encounter in
    // view, and the formation is never carried onto the ship.
    let seed_for = |kind| (0..2000u64).find(|&s| encounter_for(s, 7) == EncounterKind::Miniboss(kind)).unwrap();
    let encounters = [10, 20, 30, 40].map(|w| (1, w)).into_iter().chain(MinibossKind::ALL.map(|k| (seed_for(k), 7)));
    for (seed, wave) in encounters {
        for flee in [LEFT, RIGHT, UP, DOWN, heading_of((1.0, -1.0))] {
            let mut run = started(seed, Rig::Ghost);
            run.set_immortal();
            run.skip_to_wave(wave);
            seconds(&mut run, IDLE, 4.0);
            run.set_flight(flee, (0.0, 0.0));
            for _ in 0..(6.0 / STEP) as usize {
                run.step(THRUST);
                let camera = run.camera();
                let (left, top, right, bottom) = run.player_box();
                let parts: Vec<_> =
                    run.enemies().filter(|e| matches!(e.role, Role::Boss(_) | Role::Miniboss(_))).collect();
                assert!(parts.iter().any(|e| e.visible_in(camera)), "wave {wave}: left behind by {flee:?}");
                assert!(
                    !parts.iter().any(|e| e.x < right && e.x + e.w > left && e.y < bottom && e.y + e.h > top),
                    "wave {wave}: carried onto the ship by {flee:?}"
                );
            }
            seconds(&mut run, IDLE, 15.0);
            let (camera, at) = (run.camera(), run.encounter());
            assert!(run.enemies().all(|e| e.role == Role::Guard || e.visible_in(camera)), "{at:?} returned");
        }
    }
}

/// A quiet run with only `kind` equipped, ready to fire.
fn armed(kind: WeaponKind) -> InvadeRun {
    let mut run = quiet(Rig::Probe);
    run.weapons = vec![WeaponState::new(kind)];
    run.weapon_cooldowns = vec![0.0];
    run
}

fn hp_lost(run: &InvadeRun) -> Vec<f32> {
    run.enemies.iter().map(|e| e.max_hp - e.hp.max(0.0)).collect()
}

fn close_to(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

#[test]
fn all_six_weapons_damage_through_their_own_paths() {
    let spec = |run: &InvadeRun| run.weapons[0].spec(run.rig, &run.stats);

    // Packet: one aimed packet.
    let mut run = armed(WeaponKind::Packet);
    target(&mut run, (38.5, 20.0), 100.0);
    run.step(Input { fire: true, ..IDLE });
    seconds(&mut run, IDLE, 0.5);
    assert!(close_to(hp_lost(&run)[0], spec(&run).damage), "Packet {:?}", hp_lost(&run));

    // Scatter: a fan of packets; the center packet hits the aimed target.
    let mut run = armed(WeaponKind::Scatter);
    target(&mut run, (38.5, 20.0), 100.0);
    run.step(Input { fire: true, ..IDLE });
    let s = spec(&run);
    assert_eq!(run.shots.len(), usize::from(s.projectiles));
    let headings: Vec<f32> = run.shots.iter().map(|shot| (shot.vy * ASPECT).atan2(shot.vx)).collect();
    assert!(close_to(headings[2] - headings[0], s.spread), "{headings:?}");
    seconds(&mut run, IDLE, 0.5);
    assert!(close_to(hp_lost(&run)[0], s.damage), "Scatter {:?}", hp_lost(&run));

    // Rail: one shot through three ships in a line, keeping part of its damage.
    let mut run = armed(WeaponKind::Rail);
    for y in [20.0, 16.0, 13.0] {
        target(&mut run, (38.5, y), 100.0);
    }
    run.step(Input { fire: true, ..IDLE });
    seconds(&mut run, IDLE, 0.5);
    let d = spec(&run).damage;
    let lost = hp_lost(&run);
    assert!(close_to(lost[0], d) && close_to(lost[1], d * PIERCE_RETAIN), "Rail {lost:?}");
    assert!(close_to(lost[2], d * PIERCE_RETAIN * PIERCE_RETAIN), "Rail {lost:?}");

    // Arc: jumps between visible ships in reach, three at most.
    let mut run = armed(WeaponKind::Arc);
    for at in [(38.5, 21.5), (50.5, 21.5), (50.5, 15.5), (62.5, 15.5)] {
        target(&mut run, at, 100.0);
    }
    run.step(Input { fire: true, ..IDLE });
    let d = spec(&run).damage;
    let lost = hp_lost(&run);
    assert!(lost[..3].iter().all(|&l| close_to(l, d)) && lost[3] == 0.0, "Arc {lost:?}");

    // Orbit: circling packets sweep through ships beside the rig.
    let mut run = armed(WeaponKind::Orbit);
    target(&mut run, (38.5, 29.0), 100.0);
    run.step(Input { fire: true, ..IDLE });
    assert_eq!(run.orbitals().len(), usize::from(spec(&run).projectiles));
    seconds(&mut run, IDLE, 0.5);
    assert!(close_to(hp_lost(&run)[0], spec(&run).damage), "Orbit {:?}", hp_lost(&run));

    // Pulse: an area around the rig, hitting everything inside its radius.
    let mut run = armed(WeaponKind::Pulse);
    for at in [(46.0, 26.0), (38.5, 30.0), (52.0, 26.0)] {
        target(&mut run, at, 100.0);
    }
    run.step(Input { fire: true, ..IDLE });
    let d = spec(&run).damage;
    let lost = hp_lost(&run);
    assert!(close_to(lost[0], d) && close_to(lost[1], d) && lost[2] == 0.0, "Pulse {lost:?}");
}

#[test]
fn weapons_learn_rank_up_and_stop_at_their_slot_and_rank_caps() {
    let mut run = quiet(Rig::Probe);
    assert_eq!(run.weapons(), [WeaponState { kind: WeaponKind::Packet, rank: 1 }]);
    let before = run.weapons()[0].spec(run.rig, &run.stats);
    offer(&mut run, &[LevelChoice::Weapon(WeaponKind::Packet)]);
    assert!(run.choose(0));
    let after = run.weapons()[0].spec(run.rig, &run.stats);
    assert!(after.damage > before.damage && after.cooldown < before.cooldown);
    while run.grant_weapon(WeaponKind::Packet) {}
    assert_eq!(run.weapons()[0].rank, 5);
    offer(&mut run, &[LevelChoice::Weapon(WeaponKind::Pulse)]);
    assert!(run.choose(0));
    for kind in [WeaponKind::Scatter, WeaponKind::Rail] {
        assert!(run.grant_weapon(kind));
    }
    assert!(!run.grant_weapon(WeaponKind::Orbit), "four weapon slots");
    offer(&mut run, &[LevelChoice::Weapon(WeaponKind::Orbit)]);
    assert!(!run.choose(0), "a full arsenal refuses a fifth weapon");
    run.phase = Phase::Combat;
    run.suspended_phase = None;
    assert_eq!(run.weapons().len(), MAX_WEAPONS);
    for _ in 0..20 {
        level_up(&mut run);
        assert!(run.offers.iter().all(|choice| match choice {
            LevelChoice::Weapon(kind) => run.weapons().iter().any(|w| w.kind == *kind && w.rank < WEAPON_RANK_CAP),
            LevelChoice::Module(_) => true,
        }));
        assert!(run.choose(0));
    }
    // Every equipped weapon keeps its own timer.
    run.countdown = 0.0;
    run.step(Input { fire: true, ..IDLE });
    assert_eq!(run.weapon_cooldowns.len(), run.weapons().len());
    assert!(run.weapon_cooldowns.iter().all(|&c| c > 0.0));
}

fn assert_rocks_in_bounds(run: &InvadeRun) {
    for a in run.asteroids() {
        assert!(a.x >= a.radius && a.x <= WORLD_W - a.radius, "x {:?}, radius {}", (a.x, a.y), a.radius);
        assert!(
            a.y >= a.radius / ASPECT && a.y <= WORLD_H - a.radius / ASPECT,
            "y {:?}, radius {}",
            (a.x, a.y),
            a.radius
        );
        assert!(a.velocity.0.is_finite() && a.velocity.1.is_finite());
    }
}

#[test]
fn asteroid_fields_are_seeded_in_three_sizes_outside_the_opening_camera_and_stay_bounded() {
    for seed in 0..24 {
        let mut run = started(seed, Rig::Probe);
        let same = started(seed, Rig::Probe);
        let snapshot = |r: &InvadeRun| {
            r.asteroids()
                .iter()
                .map(|a| (a.id, a.x, a.y, a.radius, a.velocity, a.spin, a.spin_rate))
                .collect::<Vec<_>>()
        };
        assert_eq!(snapshot(&run), snapshot(&same));
        assert_eq!(run.asteroids().len(), MAX_ASTEROIDS);
        for (radius, count) in ASTEROID_SIZES {
            assert_eq!(run.asteroids().iter().filter(|a| a.radius == radius).count(), count);
        }
        let camera = run.camera();
        assert!(
            run.asteroids().iter().all(|a| box_nearest(
                (a.x, a.y),
                camera.0,
                camera.1,
                camera.0 + ARENA_W,
                camera.1 + ARENA_H
            )
            .0 >= a.radius + 6.0)
        );
        let mut combat = run.rng.clone();
        let mut arrivals = run.arrivals.clone();
        run.seed_asteroids();
        assert_eq!(run.rng.next_u64(), combat.next_u64(), "asteroid seeding never consumes combat randomness");
        assert_eq!(run.arrivals.next_u64(), arrivals.next_u64(), "or arrival randomness");
        run.asteroids.truncate(MAX_ASTEROIDS);
        for _ in 0..2400 {
            run.update_asteroids();
            assert_rocks_in_bounds(&run);
        }
    }
}

#[test]
fn asteroid_pair_separation_keeps_both_circles_inside_every_wall_and_corner() {
    for radius in [2.2, 3.5, 5.0] {
        for corner in [(false, false), (true, false), (false, true), (true, true)] {
            let mut run = quiet(Rig::Probe);
            let (x, y) = (
                if corner.0 { WORLD_W - radius } else { radius },
                if corner.1 { WORLD_H - radius / ASPECT } else { radius / ASPECT },
            );
            let (dx, dy) = (if corner.0 { -1.0 } else { 1.0 }, if corner.1 { -0.5 } else { 0.5 });
            run.place_asteroid((x, y), radius, (-dx, -dy * ASPECT));
            run.place_asteroid((x + dx, y + dy), radius, (0.0, 0.0));
            for _ in 0..240 {
                run.update_asteroids();
                assert_rocks_in_bounds(&run);
            }
        }
    }
}

#[test]
fn asteroid_contacts_push_and_bounce_and_use_the_same_hit_protection_as_packets() {
    for rig in Rig::ALL {
        for protection in 0..4 {
            let mut run = quiet(rig);
            run.place_asteroid((44.0, 26.5), 3.0, (0.0, 0.0));
            run.player.velocity = (10.0, 0.0);
            match protection {
                1 => run.player.protection = 1.0,
                2 => run.player.invulnerable = 1.0,
                3 => run.player.shield_ready = true,
                _ => {}
            }
            let before = run.player.x;
            run.rock_contacts();
            assert!(
                run.player.x < before && run.player.velocity.0 < 0.0,
                "the rock pushes back and reverses incoming momentum"
            );
            let expected = if protection == 0 { MAX_INTEGRITY - rig.base().hit_cost } else { MAX_INTEGRITY };
            assert_eq!(run.integrity(), expected, "{rig:?} protection {protection}");
            assert!(!run.player.shield_ready);
            let damage = run.integrity();
            run.rock_contacts();
            assert_eq!(run.integrity(), damage, "the same touch cannot bypass hit invulnerability");
        }
    }
    let mut immortal = quiet(Rig::Probe);
    immortal.set_immortal();
    immortal.place_asteroid((44.0, 26.5), 3.0, (0.0, 0.0));
    immortal.rock_contacts();
    assert_eq!(immortal.integrity(), MAX_INTEGRITY);
    assert!(immortal.player.x < 40.0, "immortal still collides physically");
}

#[test]
fn asteroids_absorb_packets_before_targets_but_do_not_block_nearer_targets() {
    for rock_first in [true, false] {
        let mut run = quiet(Rig::Probe);
        target(&mut run, (if rock_first { 60.0 } else { 48.0 }, 26.0), 100.0);
        run.place_asteroid((if rock_first { 50.0 } else { 60.0 }, 26.5), 3.0, (0.0, 0.0));
        run.shots.push(Shot {
            kind: ShotKind::Primary,
            x: 40.0,
            y: 26.5,
            vx: 4000.0,
            vy: 0.0,
            damage: 5.0,
            pierce: 0,
            hit: Vec::new(),
            travelled: 0.0,
            range: 100.0,
            guided: false,
        });
        run.move_shots();
        assert!(run.shots.is_empty());
        assert_eq!(run.enemies[0].hp, if rock_first { 100.0 } else { 95.0 });

        let mut hostile = quiet(Rig::Probe);
        hostile.place_asteroid((if rock_first { 50.0 } else { 30.0 }, 26.5), 3.0, (0.0, 0.0));
        hostile.hostiles.push(Hostile { kind: HostileKind::Straight, x: 70.0, y: 26.5, vx: -4000.0, vy: 0.0 });
        hostile.move_hostiles();
        assert!(hostile.hostiles.is_empty());
        assert_eq!(hostile.integrity(), if rock_first { MAX_INTEGRITY } else { MAX_INTEGRITY - 1 });
    }
}

#[test]
fn enemy_flight_steers_around_a_rock_instead_of_flying_straight_through_it() {
    let mut run = quiet(Rig::Probe);
    run.add_enemy(Role::Device(DeviceKind::Endpoint), (70.0, 26.0), (3.0, 1.0), 100.0, true, 100);
    let top = flight_speed(DeviceKind::Endpoint, run.wave);
    run.enemies[0].heading = LEFT;
    run.enemies[0].velocity = (-top, 0.0);
    run.place_asteroid((55.0, 26.5), 3.5, (0.0, 0.0));
    let mut detour = 0.0f32;
    let mut nearest = 100.0f32;
    for _ in 0..(10.0 / STEP) as usize {
        run.fly_ship(0);
        let e = &run.enemies[0];
        detour = detour.max((e.center().1 - 26.5).abs() * ASPECT);
        nearest = nearest.min(visual_distance(e.center(), (run.player.x, run.player.y + 0.5)));
        assert!(
            box_nearest((55.0, 26.5), e.x, e.y, e.x + e.w, e.y + e.h).0 >= 3.5 - 0.02,
            "enemy is outside the rock after collision resolution"
        );
    }
    assert!(detour > 3.5, "it takes a visible detour");
    assert!(nearest < 12.0, "it gets past the obstacle and continues pursuing, nearest {nearest}");
}

#[test]
fn root_guards_warn_only_while_visible_but_keep_their_committed_warning() {
    let mut run = started(1, Rig::Probe);
    run.skip_to_wave(40);
    for enemy in &mut run.enemies {
        enemy.entry = None;
        enemy.entered = true;
    }
    let camera = run.camera();
    let id = run.add_enemy(Role::Guard, (camera.0 + ARENA_W + 5.0, run.player.y), (3.0, 1.0), 100.0, false, 0);
    let index = run.enemies.len() - 1;
    run.enemies[index].entered = true;
    run.enemies[index].cooldown = 0.0;
    let Encounter::Special(pattern) = run.encounter else { panic!("boss") };
    run.update_pattern(pattern);
    assert!(!run.marks.iter().any(|m| m.source == id), "an offscreen guard cannot start a warning");
    run.enemies[index].x = camera.0 + ARENA_W - 4.0;
    run.enemies[index].cooldown = 0.0;
    let Encounter::Special(pattern) = run.encounter else { panic!("boss") };
    run.update_pattern(pattern);
    assert!(run.marks.iter().any(|m| m.source == id), "an onscreen guard can commit an attack");
    run.enemies[index].x = camera.0 + ARENA_W + 5.0;
    run.update_marks();
    assert!(run.marks.iter().any(|m| m.source == id), "leaving view does not erase a committed warning");
}

#[test]
fn root_streams_warn_then_sweep_outward_from_the_recorded_point_without_tracking_the_ship() {
    let mut run = started(1, Rig::Bastion);
    run.clear_asteroids();
    run.skip_to_wave(40);
    for e in &mut run.enemies {
        e.entry = None;
        e.entered = true;
    }
    let Encounter::Special(mut pattern) = run.encounter else { panic!("Root Controller") };
    pattern.timer = 0.0;
    pattern.step = 0;
    run.update_pattern(pattern);
    let mark = *run.marks.iter().find(|m| m.attack == MarkAttack::Stream).expect("stream has a warning");
    assert!(mark.warn >= TELEGRAPH);
    assert!(run.hostiles.is_empty());
    run.player.x += 24.0;
    run.player.y += 8.0;
    for _ in 0..(TELEGRAPH / STEP).floor() as usize {
        run.update_marks();
        assert!(run.hostiles.is_empty(), "no stream before the warning finishes");
    }
    while run.marks.iter().any(|m| m.attack == MarkAttack::Stream) {
        run.update_marks();
    }
    let Encounter::Special(pattern) = run.encounter else { panic!("Root Controller") };
    assert_eq!(pattern.stream_target, Some((mark.x, mark.y)));
    run.update_pattern(pattern);
    assert_eq!(run.hostiles.len(), 2);
    let core = run.enemies.iter().find(|e| matches!(e.role, Role::Boss(_))).unwrap();
    let recorded = direction(core.center(), (mark.x, mark.y)).unwrap();
    let live = direction(core.center(), (run.player.x, run.player.y + 0.5)).unwrap();
    assert!(visual_distance(recorded, live) > 0.1, "fixture distinguishes the recorded target from a moving ship");
    let angle = (STREAM_SWEEP * STEP / STREAM_TIME).to_radians();
    for (h, side) in run.hostiles.iter().zip([1.0, -1.0]) {
        let actual = unit((h.vx, h.vy * ASPECT)).unwrap();
        let expected = rotated(recorded, side * angle);
        assert!(
            visual_distance(actual, expected) < 0.001,
            "stream follows its warned point: {actual:?} vs {expected:?}"
        );
    }
    run.hostiles.clear();
    run.player.x -= 36.0;
    run.player.y -= 14.0;
    let Encounter::Special(mut pattern) = run.encounter else { panic!("Root Controller") };
    pattern.stream = STREAM_TIME / 2.0 + STEP;
    pattern.stream_timer = 0.0;
    run.update_pattern(pattern);
    let core = run.enemies.iter().find(|e| matches!(e.role, Role::Boss(_))).unwrap();
    let recorded = direction(core.center(), (mark.x, mark.y)).unwrap();
    for (h, side) in run.hostiles.iter().zip([1.0, -1.0]) {
        let actual = unit((h.vx, h.vy * ASPECT)).unwrap();
        let expected = rotated(recorded, side * (STREAM_SWEEP / 2.0).to_radians());
        let outward = (-recorded.1 * side, recorded.0 * side);
        assert!(
            actual.0 * outward.0 + actual.1 * outward.1 > 0.0,
            "the left stream sweeps left and the right stream sweeps right, leaving the center open"
        );
        assert!(visual_distance(actual, expected) < 0.001, "later packets retain the same recorded point");
    }
}

#[test]
fn moving_asteroids_bounce_off_each_other_and_each_world_wall() {
    let mut run = quiet(Rig::Probe);
    run.place_asteroid((40.0, 30.0), 3.0, (3.0, 0.0));
    run.place_asteroid((45.0, 30.0), 3.0, (-3.0, 0.0));
    run.update_asteroids();
    assert!(
        run.asteroids[0].velocity.0 < 0.0 && run.asteroids[1].velocity.0 > 0.0,
        "equal rocks exchange their closing momentum"
    );
    assert!(
        visual_distance((run.asteroids[0].x, run.asteroids[0].y), (run.asteroids[1].x, run.asteroids[1].y))
            >= 6.0 - 0.001
    );
    for (at, velocity) in [
        ((3.0, 30.0), (-4.0, 0.0)),
        ((WORLD_W - 3.0, 30.0), (4.0, 0.0)),
        ((40.0, 1.5), (0.0, -4.0)),
        ((40.0, WORLD_H - 1.5), (0.0, 4.0)),
    ] {
        run.clear_asteroids();
        run.place_asteroid(at, 3.0, velocity);
        run.update_asteroids();
        assert_rocks_in_bounds(&run);
        let bounced = run.asteroids[0].velocity;
        assert!(
            bounced.0 * velocity.0 + bounced.1 * velocity.1 < 0.0,
            "wall reverses the outward component {velocity:?} -> {bounced:?}"
        );
    }
}

#[test]
fn a_fatal_asteroid_contact_stops_the_step_before_the_encounter_can_fire_again() {
    let mut run = started(1, Rig::Probe);
    run.clear_asteroids();
    run.skip_to_wave(10);
    for e in &mut run.enemies {
        e.entry = None;
        e.entered = true;
    }
    let Encounter::Special(mut pattern) = run.encounter else { panic!("boss") };
    pattern.timer = 0.0;
    pattern.step = 0;
    run.encounter = Encounter::Special(pattern);
    run.integrity = 1;
    let at = (run.player.x + 4.0, run.player.y + 0.5);
    run.place_asteroid(at, 3.0, (0.0, 0.0));
    run.step(IDLE);
    assert_eq!((run.phase, run.integrity), (Phase::Over, 0));
    assert!(
        run.hostiles.is_empty() && run.marks.is_empty() && run.scans.is_empty(),
        "defeat clears attacks and does not let the due boss pattern recreate them"
    );
}
