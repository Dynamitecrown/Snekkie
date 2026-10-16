//! What Invade is made of: rigs, devices, encounters, the schedule that
//! places them, unlockable content and the difficulty curves.
//!
//! Every number here is an initial tuning default. Names are stable
//! identifiers saved in `invade.json`; never rename an `id()`.

use super::rng::{Rng, STREAM_LAYOUT, STREAM_SCHEDULE};

/// The arena is 80 columns by 28 rows. A row is drawn about twice as tall
/// as a column is wide, which matters for angles and distances.
pub const ARENA_W: f32 = 80.0;
pub const ARENA_H: f32 = 28.0;
/// Bounded world, three camera views across and three down.
pub const WORLD_W: f32 = ARENA_W * 3.0;
pub const WORLD_H: f32 = ARENA_H * 3.0;
pub const ASPECT: f32 = 2.0;

/// Waves past this stop getting harder; patterns still combine.
pub const SCALING_CAP_WAVE: u32 = 60;

// -- rigs ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rig {
    Probe,
    Ghost,
    Bastion,
}

/// A rig's starting statistics, before any upgrade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigBase {
    pub damage: f32,
    pub fire_rate: f32,
    /// Columns per second.
    pub move_speed: f32,
    /// Seconds for one special charge.
    pub recharge: f32,
    /// Additional targets each primary packet passes through.
    pub pierce: u8,
    /// Integrity lost per ordinary hit.
    pub hit_cost: u8,
}

/// How a ship flies: it turns at a limited rate and takes `spool` seconds
/// of thrust to go from rest to its top speed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flight {
    /// Radians per second.
    pub turn_rate: f32,
    /// Seconds of thrust from rest to top speed.
    pub spool: f32,
}

impl Rig {
    pub const ALL: [Rig; 3] = [Rig::Probe, Rig::Ghost, Rig::Bastion];

    pub fn id(self) -> &'static str {
        match self {
            Rig::Probe => "probe",
            Rig::Ghost => "ghost",
            Rig::Bastion => "bastion",
        }
    }

    pub fn from_id(id: &str) -> Option<Rig> {
        Rig::ALL.into_iter().find(|rig| rig.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Rig::Probe => "Probe",
            Rig::Ghost => "Ghost",
            Rig::Bastion => "Bastion",
        }
    }

    pub fn special_name(self) -> &'static str {
        match self {
            Rig::Probe => "Pivot",
            Rig::Ghost => "Slip",
            Rig::Bastion => "Barrier",
        }
    }

    pub fn mobility_name(self) -> &'static str {
        match self {
            Rig::Probe => "Dash",
            Rig::Ghost => "Blink",
            Rig::Bastion => "Boost",
        }
    }

    /// Ghost turns fastest and Bastion slowest; top speed is `move_speed`.
    pub fn flight(self) -> Flight {
        match self {
            Rig::Probe => Flight { turn_rate: 5.0, spool: 0.28 },
            Rig::Ghost => Flight { turn_rate: 6.0, spool: 0.22 },
            Rig::Bastion => Flight { turn_rate: 4.2, spool: 0.32 },
        }
    }

    pub fn mobility_recharge(self) -> f32 {
        match self {
            Rig::Probe => 4.0,
            Rig::Ghost => 5.0,
            Rig::Bastion => 6.0,
        }
    }

    pub fn summary(self) -> &'static str {
        match self {
            Rig::Probe => "Balanced. Pivot chains a heavy packet through nearby devices.",
            Rig::Ghost => "Fast and fragile: hits cost 2 integrity. Slip makes a short protected dash along the nose.",
            Rig::Bastion => "Slow and defensive. Heavy piercing packets; Barrier blocks hits for two seconds.",
        }
    }

    pub fn base(self) -> RigBase {
        match self {
            Rig::Probe => {
                RigBase { damage: 1.0, fire_rate: 6.6, move_speed: 26.0, recharge: 12.0, pierce: 0, hit_cost: 1 }
            }
            Rig::Ghost => {
                RigBase { damage: 1.0, fire_rate: 8.8, move_speed: 34.0, recharge: 8.0, pierce: 0, hit_cost: 2 }
            }
            Rig::Bastion => {
                RigBase { damage: 3.0, fire_rate: 2.2, move_speed: 19.0, recharge: 14.0, pierce: 1, hit_cost: 1 }
            }
        }
    }

    /// The ship on the player's row. Its width is the hitbox width.
    pub fn sprite(self) -> &'static str {
        match self {
            Rig::Probe => "[>_]",
            Rig::Ghost => "<~>",
            Rig::Bastion => "[|^|]",
        }
    }

    pub fn width(self) -> f32 {
        self.sprite().len() as f32
    }

    /// The unlock that makes this rig available, if it starts locked.
    pub fn unlock(self) -> Option<Unlock> {
        match self {
            Rig::Probe => None,
            Rig::Ghost => Some(Unlock::Ghost),
            Rig::Bastion => Some(Unlock::Bastion),
        }
    }
}

// -- unlocks ------------------------------------------------------------

/// Content that persistent milestones make available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Unlock {
    Ghost,
    Bastion,
    Fork,
    Fragment,
    Guidance,
    CacheShield,
    ToolCharge,
    Feedback,
    RootAccess,
}

/// How many devices across all runs unlock Fragment.
pub const FRAGMENT_DEVICES: u64 = 100;

impl Unlock {
    pub const ALL: [Unlock; 9] = [
        Unlock::Ghost,
        Unlock::Bastion,
        Unlock::Fork,
        Unlock::Fragment,
        Unlock::Guidance,
        Unlock::CacheShield,
        Unlock::ToolCharge,
        Unlock::Feedback,
        Unlock::RootAccess,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Unlock::Ghost => "ghost",
            Unlock::Bastion => "bastion",
            Unlock::Fork => "fork",
            Unlock::Fragment => "fragment",
            Unlock::Guidance => "guidance",
            Unlock::CacheShield => "cache_shield",
            Unlock::ToolCharge => "tool_charge",
            Unlock::Feedback => "feedback",
            Unlock::RootAccess => "root_access",
        }
    }

    pub fn from_id(id: &str) -> Option<Unlock> {
        Unlock::ALL.into_iter().find(|unlock| unlock.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Unlock::Ghost => "Ghost rig",
            Unlock::Bastion => "Bastion rig",
            Unlock::Fork => "Fork module",
            Unlock::Fragment => "Fragment module",
            Unlock::Guidance => "Guidance module",
            Unlock::CacheShield => "Cache Shield module",
            Unlock::ToolCharge => "Tool Charge module",
            Unlock::Feedback => "Feedback module",
            Unlock::RootAccess => "Root Access player tag",
        }
    }

    pub fn milestone(self) -> &'static str {
        match self {
            Unlock::Ghost => "Complete wave 6",
            Unlock::Bastion | Unlock::Guidance => "Defeat Gateway",
            Unlock::Fork => "Defeat any miniboss",
            Unlock::Fragment => "Compromise 100 devices across runs",
            Unlock::CacheShield => "Complete wave 12",
            Unlock::ToolCharge => "Complete wave 15",
            Unlock::Feedback => "Complete wave 20",
            Unlock::RootAccess => "Complete wave 30",
        }
    }

    /// The wave whose completion earns this, for wave milestones.
    pub fn wave(self) -> Option<u32> {
        match self {
            Unlock::Ghost => Some(6),
            Unlock::CacheShield => Some(12),
            Unlock::ToolCharge => Some(15),
            Unlock::Feedback => Some(20),
            Unlock::RootAccess => Some(30),
            _ => None,
        }
    }
}

// -- devices ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DeviceKind {
    Endpoint,
    Router,
    Firewall,
    Switch,
    IdsProbe,
}

impl DeviceKind {
    pub const ALL: [DeviceKind; 5] =
        [DeviceKind::Endpoint, DeviceKind::Router, DeviceKind::Firewall, DeviceKind::Switch, DeviceKind::IdsProbe];

    pub fn name(self) -> &'static str {
        match self {
            DeviceKind::Endpoint => "Endpoint",
            DeviceKind::Router => "Router",
            DeviceKind::Firewall => "Firewall",
            DeviceKind::Switch => "Switch",
            DeviceKind::IdsProbe => "IDS Probe",
        }
    }

    pub fn sprite(self) -> &'static str {
        match self {
            DeviceKind::Endpoint => "[.]",
            DeviceKind::Router => "<+>",
            DeviceKind::Firewall => "[#]",
            DeviceKind::Switch => "[=]",
            DeviceKind::IdsProbe => "<!>",
        }
    }

    pub fn base_hp(self) -> u32 {
        match self {
            DeviceKind::Endpoint => 4,
            DeviceKind::Router => 6,
            DeviceKind::Firewall => 12,
            DeviceKind::Switch | DeviceKind::IdsProbe => 8,
        }
    }

    pub fn score(self) -> u64 {
        match self {
            DeviceKind::Endpoint => 100,
            DeviceKind::Router => 150,
            DeviceKind::Firewall | DeviceKind::IdsProbe => 250,
            DeviceKind::Switch => 200,
        }
    }

    /// The first wave this device appears in.
    pub fn introduced(self) -> u32 {
        match self {
            DeviceKind::Endpoint => 1,
            DeviceKind::Router => 2,
            DeviceKind::Firewall => 3,
            DeviceKind::Switch => 6,
            DeviceKind::IdsProbe => 8,
        }
    }
}

/// Firewall armor visibly breaks below half health.
pub const FIREWALL_BROKEN: &str = "[%]";
/// Root Controller's reinforcements. They award nothing.
pub const GUARD_SPRITE: &str = "|#|";
pub const GUARD_BASE_HP: u32 = 8;

// -- encounters ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MinibossKind {
    PortSentry,
    RelayPair,
    TraceHunter,
}

impl MinibossKind {
    pub const ALL: [MinibossKind; 3] = [MinibossKind::PortSentry, MinibossKind::RelayPair, MinibossKind::TraceHunter];

    pub fn name(self) -> &'static str {
        match self {
            MinibossKind::PortSentry => "Port Sentry",
            MinibossKind::RelayPair => "Relay Pair",
            MinibossKind::TraceHunter => "Trace Hunter",
        }
    }

    pub fn sprite(self) -> &'static [&'static str] {
        match self {
            MinibossKind::PortSentry => &[" .-=====-. ", "<[o--+--o]>", " '-.._..-' "],
            MinibossKind::RelayPair => &[" /=+=\\ ", "<|(o)|>", " \\=+=/ "],
            MinibossKind::TraceHunter => &[" _/~~~\\_ ", "{ <(@)> }", " `-._.-' "],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BossKind {
    Gateway,
    LoadBalancer,
    SentinelIds,
    RootController,
}

impl BossKind {
    pub const CYCLE: [BossKind; 4] =
        [BossKind::Gateway, BossKind::LoadBalancer, BossKind::SentinelIds, BossKind::RootController];

    pub fn name(self) -> &'static str {
        match self {
            BossKind::Gateway => "Gateway",
            BossKind::LoadBalancer => "Load Balancer",
            BossKind::SentinelIds => "Sentinel IDS",
            BossKind::RootController => "Root Controller",
        }
    }

    pub fn sprite(self) -> &'static [&'static str] {
        match self {
            BossKind::Gateway => &[
                "    ___[GATEWAY]___    ",
                "  /[]  []  []  []  []\\ ",
                "<|=======(+++)=======|>",
                "  \\_/\\_/\\_/\\_/\\_/\\_/\\_/ ",
            ],
            // One of the two linked cores.
            BossKind::LoadBalancer => &[" .--LB--. ", "[(o)=(o)]", " '--+--' "],
            BossKind::SentinelIds => {
                &["   .-[SENTINEL]-.   ", "  / <!>  <!>  <!> \\ ", " |===[ ( O ) ]===| ", "  \\_____________/  "]
            }
            BossKind::RootController => &[
                "      ._[ ROOT ]_.      ",
                "  .-'  ##########  '-.  ",
                " <|==[  ( @@@@ )  ]==|> ",
                "  '-._  ##########  _.-' ",
                "       '-.______.-'       ",
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncounterKind {
    Ordinary,
    Miniboss(MinibossKind),
    Boss(BossKind),
}

impl EncounterKind {
    pub fn label(self) -> String {
        match self {
            EncounterKind::Ordinary => "Swarm".into(),
            EncounterKind::Miniboss(kind) => format!("Miniboss: {}", kind.name()),
            EncounterKind::Boss(kind) => format!("Boss: {}", kind.name()),
        }
    }
}

/// Every tenth wave is a major boss, cycling through four encounters.
pub fn boss_for_wave(wave: u32) -> Option<BossKind> {
    (wave > 0 && wave.is_multiple_of(10)).then(|| BossKind::CYCLE[((wave / 10 - 1) % 4) as usize])
}

/// The one miniboss wave in a ten-wave block: positions 4 to 7.
pub fn miniboss_wave(seed: u64, block: u32) -> u32 {
    block * 10 + 4 + Rng::stream(seed, STREAM_SCHEDULE, u64::from(block)).below(4)
}

/// Minibosses come from a shuffled bag of the three types; a new bag never
/// starts with the type the last one ended on.
pub fn miniboss_kind(seed: u64, block: u32) -> MinibossKind {
    let bag_index = block / 3;
    let mut previous_last: Option<MinibossKind> = None;
    let mut bag = MinibossKind::ALL;
    for index in 0..=bag_index {
        bag = MinibossKind::ALL;
        Rng::stream(seed, STREAM_SCHEDULE ^ 0xBA6, u64::from(index)).shuffle(&mut bag);
        if previous_last == Some(bag[0]) {
            bag.swap(0, 1);
        }
        previous_last = Some(bag[2]);
    }
    bag[(block % 3) as usize]
}

pub fn encounter_for(seed: u64, wave: u32) -> EncounterKind {
    if let Some(boss) = boss_for_wave(wave) {
        return EncounterKind::Boss(boss);
    }
    let block = (wave - 1) / 10;
    if wave == miniboss_wave(seed, block) {
        EncounterKind::Miniboss(miniboss_kind(seed, block))
    } else {
        EncounterKind::Ordinary
    }
}

// -- perimeter arrivals -------------------------------------------------

/// A finite stream of ships; the next wave waits until every ship is
/// spawned and defeated. Populations retain the original wave budgets.
pub fn wave_budget(wave: u32) -> usize {
    match wave {
        0..=4 => 48,
        5..=8 => 52,
        9..=12 => 56,
        _ => 60,
    }
}

/// The seeded composition and order do not depend on kills or movement.
pub fn wave_roster(seed: u64, wave: u32) -> Vec<DeviceKind> {
    let mut rng = Rng::stream(seed, STREAM_LAYOUT, u64::from(wave));
    let half = wave_budget(wave) / 2;
    let mut kinds = device_group(&mut rng, wave, half);
    kinds.extend(device_group(&mut rng, wave, half));
    rng.shuffle(&mut kinds);
    kinds
}

fn device_group(rng: &mut Rng, wave: u32, count: usize) -> Vec<DeviceKind> {
    use DeviceKind::*;
    let fill = |kind, n: usize| std::iter::repeat_n(kind, n);
    match wave {
        0 | 1 => return fill(Endpoint, count).collect(),
        2 => return fill(Router, 6).chain(fill(Endpoint, count - 6)).collect(),
        3 => return fill(Router, 3).chain(fill(Endpoint, count - 6)).chain(fill(Firewall, 3)).collect(),
        _ => {}
    }
    let late = ((wave.saturating_sub(10)) as f32 / 40.0).clamp(0.0, 1.0);
    let min_endpoints = if wave < 10 { 0.5 } else { 0.5 - 0.25 * late };
    let max_firewalls = if wave < 10 { 4 } else { (4 + (wave - 10) / 6).min(8) as usize };
    let max_switches = (2 + wave / 8).min(6) as usize;
    let max_ids = (1 + wave / 10).min(5) as usize;
    let endpoints =
        ((count as f32 * min_endpoints).ceil() as usize + rng.below(count as u32 / 8 + 1) as usize).min(count);
    let mut kinds: Vec<DeviceKind> = Vec::with_capacity(count);
    // Toughness ramps in slowly before the first boss.
    let firewall = if wave < 10 { 1.5 } else { 2.5 };
    let weights = [(Router, 4.0), (Firewall, firewall), (Switch, 2.0), (IdsProbe, 1.5)];
    for _ in endpoints..count {
        let available: Vec<(DeviceKind, f32)> = weights
            .iter()
            .copied()
            .filter(|(kind, _)| kind.introduced() <= wave)
            .filter(|(kind, _)| {
                let have = kinds.iter().filter(|k| *k == kind).count();
                match kind {
                    Firewall => have < max_firewalls,
                    Switch => have < max_switches,
                    IdsProbe => have < max_ids,
                    _ => true,
                }
            })
            .collect();
        let total: f32 = available.iter().map(|(_, w)| w).sum();
        let mut pick = rng.range(0.0, total);
        let mut chosen = Endpoint;
        for (kind, weight) in available {
            if pick < weight {
                chosen = kind;
                break;
            }
            pick -= weight;
        }
        kinds.push(chosen);
    }
    kinds.extend(fill(Endpoint, count - kinds.len()));
    kinds
}

// -- health and difficulty ----------------------------------------------

/// `ceil(base × (1 + 0.01 × clamp(wave − 10, 0, 50)))`, in exact integers.
pub fn device_hp(base: u32, wave: u32) -> u32 {
    let percent = 100 + wave.saturating_sub(10).min(50);
    (base * percent).div_ceil(100)
}

pub fn miniboss_hp(wave: u32) -> u32 {
    300 + 8 * wave.saturating_sub(5).min(55)
}

pub fn boss_hp(wave: u32) -> u32 {
    360 + 10 * wave.saturating_sub(10).min(50)
}

/// 0 at wave 1, 1 from wave 60 on.
pub fn level(wave: u32) -> f32 {
    (wave.clamp(1, SCALING_CAP_WAVE) - 1) as f32 / (SCALING_CAP_WAVE - 1) as f32
}

/// Seconds between ordinary perimeter arrivals.
pub fn spawn_interval(wave: u32) -> f32 {
    // Calibrated with the automatic players so wave 10 arrives after about
    // eight to ten minutes; the swarm still thickens with every wave.
    0.98 - 0.2 * level(wave)
}

/// How each device flies. Heavy Firewalls turn slowly; Routers are agile.
pub fn device_flight(kind: DeviceKind) -> Flight {
    match kind {
        DeviceKind::Endpoint => Flight { turn_rate: 2.6, spool: 0.6 },
        DeviceKind::Router => Flight { turn_rate: 3.2, spool: 0.5 },
        DeviceKind::Firewall => Flight { turn_rate: 1.6, spool: 0.9 },
        DeviceKind::Switch | DeviceKind::IdsProbe => Flight { turn_rate: 2.2, spool: 0.6 },
    }
}

/// Top flight speed in visual columns per second.
pub fn flight_speed(kind: DeviceKind, wave: u32) -> f32 {
    let base = match kind {
        DeviceKind::Endpoint => 5.0,
        DeviceKind::Router => 6.5,
        DeviceKind::Firewall => 3.6,
        DeviceKind::Switch => 4.8,
        DeviceKind::IdsProbe => 4.2,
    };
    base * (1.0 + 0.4 * level(wave))
}

/// Hostile packets, rows per second.
pub fn hostile_speed(wave: u32) -> f32 {
    11.0 + 6.0 * level(wave)
}

/// Seconds between ordinary ship attacks.
pub fn fire_interval(wave: u32) -> f32 {
    // Gentle early waves; the same pressure as before from wave 60.
    1.5 - 0.95 * level(wave)
}

/// Seconds between Switch and IDS Probe telegraphs in a swarm.
pub fn telegraph_interval(wave: u32) -> f32 {
    5.5 - 2.5 * level(wave)
}

/// Scales the waits inside miniboss and boss patterns.
pub fn pattern_pace(wave: u32) -> f32 {
    1.0 - 0.35 * level(wave)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bosses_every_tenth_wave_cycle_through_four_encounters() {
        let bosses: Vec<_> = (1..=90).filter_map(boss_for_wave).collect();
        assert_eq!(bosses.len(), 9);
        assert_eq!(
            &bosses[..5],
            &[
                BossKind::Gateway,
                BossKind::LoadBalancer,
                BossKind::SentinelIds,
                BossKind::RootController,
                BossKind::Gateway
            ]
        );
        assert_eq!(boss_for_wave(9), None);
        assert_eq!(boss_for_wave(0), None);
    }

    #[test]
    fn exactly_one_miniboss_per_block_on_positions_four_to_seven() {
        for seed in 0..200u64 {
            for block in 0..8 {
                let waves: Vec<u32> = (block * 10 + 1..=block * 10 + 10)
                    .filter(|&w| matches!(encounter_for(seed, w), EncounterKind::Miniboss(_)))
                    .collect();
                assert_eq!(waves.len(), 1, "seed {seed} block {block}");
                assert!((4..=7).contains(&(waves[0] - block * 10)));
                assert!(matches!(encounter_for(seed, block * 10 + 10), EncounterKind::Boss(_)));
            }
        }
    }

    #[test]
    fn miniboss_bags_use_every_type_and_never_repeat_across_boundaries() {
        for seed in 0..200u64 {
            let kinds: Vec<_> = (0..30).map(|block| miniboss_kind(seed, block)).collect();
            for bag in kinds.chunks(3) {
                for kind in MinibossKind::ALL {
                    assert!(bag.contains(&kind));
                }
            }
            for pair in kinds.windows(2) {
                assert_ne!(pair[0], pair[1], "seed {seed}: {kinds:?}");
            }
            assert_eq!(miniboss_kind(seed, 7), kinds[7], "deterministic");
        }
    }

    #[test]
    fn finite_spawn_budgets_and_early_rosters_follow_the_design() {
        for (wave, size) in [(1, 48), (4, 48), (5, 52), (8, 52), (9, 56), (12, 56), (13, 60), (99, 60)] {
            assert_eq!(wave_budget(wave), size);
            assert_eq!(wave_roster(1, wave).len(), size);
        }
        let count = |wave, kind| wave_roster(1, wave).iter().filter(|&&k| k == kind).count();
        assert_eq!(count(1, DeviceKind::Endpoint), 48);
        assert_eq!((count(2, DeviceKind::Endpoint), count(2, DeviceKind::Router)), (36, 12));
        assert_eq!([DeviceKind::Endpoint, DeviceKind::Router, DeviceKind::Firewall].map(|k| count(3, k)), [36, 6, 6]);
    }

    #[test]
    fn rosters_are_seeded_mostly_endpoints_early_and_introduce_kinds_on_time() {
        for seed in 0..300u64 {
            for wave in 1..10 {
                let kinds = wave_roster(seed, wave);
                let count = |kind| kinds.iter().filter(|&&k| k == kind).count();
                assert!(count(DeviceKind::Endpoint) * 2 >= kinds.len());
                assert!(count(DeviceKind::Firewall) <= 8);
                for kind in DeviceKind::ALL {
                    if wave < kind.introduced() {
                        assert_eq!(count(kind), 0, "{kind:?} before wave {}", kind.introduced());
                    }
                }
            }
            assert_eq!(wave_roster(seed, 14), wave_roster(seed, 14));
        }
    }

    #[test]
    fn health_formulas_hit_their_endpoints_exactly() {
        assert_eq!(device_hp(4, 1), 4);
        assert_eq!(device_hp(4, 10), 4);
        assert_eq!(device_hp(4, 11), 5); // ceil(4.04)
        assert_eq!(device_hp(4, 60), 6);
        assert_eq!(device_hp(12, 60), 18);
        assert_eq!(device_hp(12, 200), 18, "scaling stops at wave 60");
        assert_eq!(miniboss_hp(4), 300);
        assert_eq!(miniboss_hp(7), 316);
        assert_eq!(miniboss_hp(60), 740);
        assert_eq!(miniboss_hp(90), 740);
        assert_eq!(boss_hp(10), 360);
        assert_eq!(boss_hp(20), 460);
        assert_eq!(boss_hp(60), 860);
        assert_eq!(boss_hp(100), 860);
    }

    #[test]
    fn difficulty_scaling_is_monotonic_and_capped() {
        for wave in 1..SCALING_CAP_WAVE {
            for kind in DeviceKind::ALL {
                assert!(flight_speed(kind, wave + 1) >= flight_speed(kind, wave));
            }
            assert!(spawn_interval(wave + 1) <= spawn_interval(wave));
            assert!(hostile_speed(wave + 1) >= hostile_speed(wave));
            assert!(fire_interval(wave + 1) <= fire_interval(wave));
        }
        assert_eq!(hostile_speed(60), hostile_speed(500));
        assert_eq!(fire_interval(60), fire_interval(500));
        assert_eq!(spawn_interval(60), spawn_interval(500));
        assert_eq!(telegraph_interval(60), telegraph_interval(500));
        assert_eq!(pattern_pace(60), pattern_pace(500));
        for wave in 1..SCALING_CAP_WAVE {
            assert!(telegraph_interval(wave + 1) <= telegraph_interval(wave));
            assert!(pattern_pace(wave + 1) <= pattern_pace(wave));
        }
        for kind in DeviceKind::ALL {
            assert_eq!(flight_speed(kind, 60), flight_speed(kind, 500));
        }
    }

    #[test]
    fn identifiers_round_trip() {
        for rig in Rig::ALL {
            assert_eq!(Rig::from_id(rig.id()), Some(rig));
        }
        for unlock in Unlock::ALL {
            assert_eq!(Unlock::from_id(unlock.id()), Some(unlock));
        }
        assert_eq!(Rig::from_id("nope"), None);
    }
}
