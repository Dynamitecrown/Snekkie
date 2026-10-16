//! Starting loadouts and the six independently ranked weapons. Execution,
//! collision ownership and runtime cooldowns belong to the run model.

use super::content::Rig;
use super::upgrades::{Build, Stats, UpgradeId};

pub use super::upgrades::{LevelChoice, describe_level, level_offers};

pub const MAX_WEAPONS: usize = 4;
pub const WEAPON_RANK_CAP: u8 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WeaponKind {
    Packet,
    Scatter,
    Rail,
    Arc,
    Orbit,
    Pulse,
}

impl WeaponKind {
    pub const ALL: [Self; 6] = [Self::Packet, Self::Scatter, Self::Rail, Self::Arc, Self::Orbit, Self::Pulse];
    pub const STARTERS: [Self; 3] = [Self::Packet, Self::Scatter, Self::Rail];

    pub fn id(self) -> &'static str {
        match self {
            Self::Packet => "packet",
            Self::Scatter => "scatter",
            Self::Rail => "rail",
            Self::Arc => "arc",
            Self::Orbit => "orbit",
            Self::Pulse => "pulse",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Packet => "Packet",
            Self::Scatter => "Scatter",
            Self::Rail => "Rail",
            Self::Arc => "Arc",
            Self::Orbit => "Orbit",
            Self::Pulse => "Pulse",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Packet => "Fast bolts at the nearest device. A balanced starting weapon.",
            Self::Scatter => "A wide fan of weaker packets. More coverage, less damage per packet.",
            Self::Rail => "Slow, heavy packets that pierce several devices with diminishing damage.",
            Self::Arc => "An electrical chain that damages up to three distinct nearby devices.",
            Self::Orbit => "Rotating packets circle the rig to intercept nearby devices.",
            Self::Pulse => "Periodic damage to every device inside a short radius around the rig.",
        }
    }

    pub fn summary(self) -> &'static str {
        self.description()
    }

    pub fn cap(self) -> u8 {
        WEAPON_RANK_CAP
    }

    /// All distances and speeds use aspect-correct visual world units.
    /// Damage/rate modules and the rig's existing weapon statistics apply
    /// before these weapon and rank multipliers.
    pub fn spec(self, rank: u8, rig: Rig, stats: &Stats) -> WeaponSpec {
        let ranks = rank.clamp(1, WEAPON_RANK_CAP) - 1;
        let r = f32::from(ranks);
        let base = rig.base();
        let damage_scale = stats.damage / base.damage;
        let cooldown_scale = base.fire_rate / stats.fire_rate;
        let damage = |multiplier: f32, gain: f32| base.damage * multiplier * (1.0 + gain * r) * damage_scale;
        let cooldown =
            |interval: f32, gain: f32| interval / base.fire_rate * gain.powi(i32::from(ranks)) * cooldown_scale;
        let mut spec = WeaponSpec {
            damage: damage(1.0, 0.20),
            cooldown: cooldown(1.0, 0.94),
            projectiles: 1,
            pierce: stats.pierce,
            spread: 0.0,
            speed: 74.0 + 3.0 * r,
            range: 45.0 + 2.0 * r,
            radius: 0.0,
            chain_targets: 0,
        };
        match self {
            Self::Packet => {}
            Self::Scatter => {
                spec.damage = damage(0.65, 0.18);
                spec.cooldown = cooldown(2.5, 0.95);
                spec.projectiles = 3 + 2 * (ranks / 2);
                spec.spread = 32.0f32.to_radians();
                spec.speed = 60.0 + 2.0 * r;
                spec.range = 34.0 + 2.0 * r;
            }
            Self::Rail => {
                spec.damage = damage(3.0, 0.22);
                spec.cooldown = cooldown(4.5, 0.95);
                spec.pierce = stats.pierce.saturating_add(2 + ranks / 2);
                spec.speed = 112.0;
                spec.range = 60.0;
            }
            Self::Arc => {
                spec.damage = damage(1.1, 0.22);
                spec.cooldown = cooldown(6.0, 0.95);
                spec.pierce = 0;
                spec.speed = 0.0;
                spec.range = 18.0 + 2.0 * r;
                spec.chain_targets = 3;
            }
            Self::Orbit => {
                spec.damage = damage(0.8, 0.20);
                spec.cooldown = cooldown(10.0, 0.95);
                spec.projectiles = 2 + ranks / 2;
                spec.pierce = 0;
                spec.speed = 30.0;
                spec.range = 60.0;
                spec.radius = 6.0 + 0.75 * r;
            }
            Self::Pulse => {
                spec.damage = damage(2.2, 0.20);
                spec.cooldown = cooldown(12.0, 0.95);
                spec.pierce = 0;
                spec.speed = 0.0;
                spec.range = 0.0;
                spec.radius = 9.0 + r;
            }
        }
        spec
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponSpec {
    pub damage: f32,
    /// Seconds between activations; each weapon has its own timer.
    pub cooldown: f32,
    pub projectiles: u8,
    /// Additional devices a projectile can pass through.
    pub pierce: u8,
    /// The total fan angle in radians.
    pub spread: f32,
    pub speed: f32,
    /// Projectile travel distance, or Arc's maximum jump distance.
    pub range: f32,
    /// Orbit distance from the rig, or Pulse's area radius.
    pub radius: f32,
    /// Total distinct devices hit by one Arc activation.
    pub chain_targets: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeaponState {
    pub kind: WeaponKind,
    pub rank: u8,
}

impl WeaponState {
    pub fn new(kind: WeaponKind) -> Self {
        Self { kind, rank: 1 }
    }

    pub fn upgrade(&mut self) -> bool {
        if self.rank >= self.kind.cap() {
            return false;
        }
        self.rank += 1;
        true
    }

    pub fn spec(self, rig: Rig, stats: &Stats) -> WeaponSpec {
        self.kind.spec(self.rank, rig, stats)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StarterModule {
    Payload,
    Thrusters,
    Collector,
}

impl StarterModule {
    pub const ALL: [Self; 3] = [Self::Payload, Self::Thrusters, Self::Collector];

    pub fn id(self) -> &'static str {
        match self {
            Self::Payload => "payload",
            Self::Thrusters => "thrusters",
            Self::Collector => "collector",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|module| module.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Payload => "Payload",
            Self::Thrusters => "Thrusters",
            Self::Collector => "Collector",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Payload => "Start with Payload rank 1: +15% weapon damage.",
            Self::Thrusters => "Start with Light Frame rank 1: +15% movement; +10% special recharge time.",
            Self::Collector => "+50% XP pickup magnet radius; no starting module rank.",
        }
    }

    pub fn summary(self) -> &'static str {
        self.description()
    }

    pub fn apply(self, build: &mut Build) {
        match self {
            Self::Payload => {
                build.add(UpgradeId::Payload);
            }
            Self::Thrusters => {
                build.add(UpgradeId::LightFrame);
            }
            Self::Collector => {}
        }
    }

    pub fn magnet_multiplier(self) -> f32 {
        if self == Self::Collector { 1.5 } else { 1.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loadout {
    pub rig: Rig,
    pub weapon: WeaponKind,
    pub module: StarterModule,
}

impl Default for Loadout {
    fn default() -> Self {
        Self { rig: Rig::Probe, weapon: WeaponKind::Packet, module: StarterModule::Collector }
    }
}

pub fn initial_weapons(loadout: Loadout) -> Vec<WeaponState> {
    vec![WeaponState::new(loadout.weapon)]
}

pub fn can_acquire_weapon(kind: WeaponKind, weapons: &[WeaponState]) -> bool {
    match weapons.iter().find(|weapon| weapon.kind == kind) {
        Some(weapon) => weapon.rank < kind.cap(),
        None => weapons.len() < MAX_WEAPONS,
    }
}

/// Learn a new weapon or add one rank, preserving unique slots and caps.
pub fn acquire_weapon(weapons: &mut Vec<WeaponState>, kind: WeaponKind) -> bool {
    if let Some(weapon) = weapons.iter_mut().find(|weapon| weapon.kind == kind) {
        return weapon.upgrade();
    }
    if weapons.len() >= MAX_WEAPONS {
        return false;
    }
    weapons.push(WeaponState::new(kind));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invade::upgrades::stats;

    #[test]
    fn loadout_modules_preserve_default_stats_and_apply_their_declared_tradeoff() {
        let default = Loadout::default();
        assert_eq!(
            (default.rig, default.weapon, default.module),
            (Rig::Probe, WeaponKind::Packet, StarterModule::Collector)
        );
        let mut build = Build::default();
        default.module.apply(&mut build);
        assert!(build.is_empty());
        assert_eq!(default.module.magnet_multiplier(), 1.5);
        StarterModule::Payload.apply(&mut build);
        assert_eq!(build.rank(UpgradeId::Payload), 1);
        let before = stats(Rig::Probe, &build);
        StarterModule::Thrusters.apply(&mut build);
        let after = stats(Rig::Probe, &build);
        assert!((after.move_speed / before.move_speed - 1.15).abs() < 1e-5);
        assert!((after.recharge / before.recharge - 1.1).abs() < 1e-5);
    }

    #[test]
    fn every_weapon_has_bounded_ranks_and_meaningful_computed_improvements() {
        for rig in Rig::ALL {
            let stats = stats(rig, &Build::default());
            for kind in WeaponKind::ALL {
                assert_eq!(WeaponKind::from_id(kind.id()), Some(kind));
                let mut state = WeaponState::new(kind);
                for rank in 1..WEAPON_RANK_CAP {
                    let before = state.spec(rig, &stats);
                    assert!(state.upgrade());
                    let after = state.spec(rig, &stats);
                    assert_eq!(state.rank, rank + 1);
                    assert!(after.damage > before.damage && after.cooldown < before.cooldown);
                    assert!(after.damage.is_finite() && after.cooldown > 0.0);
                    assert!(after.projectiles >= before.projectiles);
                }
                assert!(!state.upgrade());
                assert_eq!(state.rank, WEAPON_RANK_CAP);
            }
        }
        assert_eq!(initial_weapons(Loadout::default()), [WeaponState::new(WeaponKind::Packet)]);
    }

    #[test]
    fn acquiring_weapons_preserves_slots_and_capped_ranks() {
        let mut weapons = initial_weapons(Loadout::default());
        for kind in [WeaponKind::Scatter, WeaponKind::Rail, WeaponKind::Arc] {
            assert!(acquire_weapon(&mut weapons, kind));
        }
        assert_eq!(weapons.len(), MAX_WEAPONS);
        assert!(!can_acquire_weapon(WeaponKind::Pulse, &weapons));
        assert!(!acquire_weapon(&mut weapons, WeaponKind::Pulse));
        for _ in 1..WEAPON_RANK_CAP {
            assert!(acquire_weapon(&mut weapons, WeaponKind::Packet));
        }
        assert!(!acquire_weapon(&mut weapons, WeaponKind::Packet));
        assert!(!can_acquire_weapon(WeaponKind::Packet, &weapons));
        assert_eq!(weapons.len(), MAX_WEAPONS);
    }
}
