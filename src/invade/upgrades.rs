//! Upgrades: the catalog, how ranks combine into a rig's statistics, the
//! seeded offers between waves and the exact text each card shows.
//!
//! Bonuses to the same statistic add together; stated penalties multiply
//! afterward. Damage stays fractional, so small differences survive.

use super::arsenal::{MAX_WEAPONS, WeaponKind, WeaponSpec, WeaponState, can_acquire_weapon};
use super::content::{Rig, Unlock};
use super::rng::Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UpgradeId {
    Payload,
    ClockBoost,
    Overclock,
    HeavyPackets,
    Fork,
    Pierce,
    Fragment,
    Guidance,
    SpareParts,
    WarmBackup,
    CacheShield,
    GracePeriod,
    LightFrame,
    Grounded,
    Heatsink,
    ToolCharge,
    ToolAmplifier,
    Feedback,
}

pub const PIERCE_RETAIN: f32 = 0.65;
pub const FORK_SHARE: f32 = 0.3;
pub const PIVOT_CHAIN_DAMAGE: f32 = 3.0;
pub const SLIP_DISTANCE: f32 = 8.0;
pub const SPARE_PARTS_INTERVAL: u32 = 5;
pub const CACHE_SHIELD_RECHARGE: u32 = 3;
/// Feedback refunds at most this many times per second.
pub const FEEDBACK_PER_SECOND: usize = 2;
/// However recharge modifiers stack, a charge never takes less than this.
pub const MIN_RECHARGE: f32 = 2.5;

impl UpgradeId {
    pub const ALL: [UpgradeId; 18] = [
        UpgradeId::Payload,
        UpgradeId::ClockBoost,
        UpgradeId::Overclock,
        UpgradeId::HeavyPackets,
        UpgradeId::Fork,
        UpgradeId::Pierce,
        UpgradeId::Fragment,
        UpgradeId::Guidance,
        UpgradeId::SpareParts,
        UpgradeId::WarmBackup,
        UpgradeId::CacheShield,
        UpgradeId::GracePeriod,
        UpgradeId::LightFrame,
        UpgradeId::Grounded,
        UpgradeId::Heatsink,
        UpgradeId::ToolCharge,
        UpgradeId::ToolAmplifier,
        UpgradeId::Feedback,
    ];

    fn index(self) -> usize {
        self as usize
    }

    pub fn id(self) -> &'static str {
        match self {
            UpgradeId::Payload => "payload",
            UpgradeId::ClockBoost => "clock_boost",
            UpgradeId::Overclock => "overclock",
            UpgradeId::HeavyPackets => "heavy_packets",
            UpgradeId::Fork => "fork",
            UpgradeId::Pierce => "pierce",
            UpgradeId::Fragment => "fragment",
            UpgradeId::Guidance => "guidance",
            UpgradeId::SpareParts => "spare_parts",
            UpgradeId::WarmBackup => "warm_backup",
            UpgradeId::CacheShield => "cache_shield",
            UpgradeId::GracePeriod => "grace_period",
            UpgradeId::LightFrame => "light_frame",
            UpgradeId::Grounded => "grounded",
            UpgradeId::Heatsink => "heatsink",
            UpgradeId::ToolCharge => "tool_charge",
            UpgradeId::ToolAmplifier => "tool_amplifier",
            UpgradeId::Feedback => "feedback",
        }
    }

    pub fn from_id(id: &str) -> Option<UpgradeId> {
        UpgradeId::ALL.into_iter().find(|upgrade| upgrade.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            UpgradeId::Payload => "Payload",
            UpgradeId::ClockBoost => "Clock Boost",
            UpgradeId::Overclock => "Overclock",
            UpgradeId::HeavyPackets => "Heavy Packets",
            UpgradeId::Fork => "Fork",
            UpgradeId::Pierce => "Pierce",
            UpgradeId::Fragment => "Fragment",
            UpgradeId::Guidance => "Guidance",
            UpgradeId::SpareParts => "Spare Parts",
            UpgradeId::WarmBackup => "Warm Backup",
            UpgradeId::CacheShield => "Cache Shield",
            UpgradeId::GracePeriod => "Grace Period",
            UpgradeId::LightFrame => "Light Frame",
            UpgradeId::Grounded => "Grounded",
            UpgradeId::Heatsink => "Heatsink",
            UpgradeId::ToolCharge => "Tool Charge",
            UpgradeId::ToolAmplifier => "Tool Amplifier",
            UpgradeId::Feedback => "Feedback",
        }
    }

    pub fn cap(self) -> u8 {
        match self {
            UpgradeId::Payload | UpgradeId::ClockBoost => 3,
            UpgradeId::Pierce
            | UpgradeId::Fragment
            | UpgradeId::GracePeriod
            | UpgradeId::LightFrame
            | UpgradeId::Heatsink
            | UpgradeId::ToolAmplifier
            | UpgradeId::Feedback => 2,
            _ => 1,
        }
    }

    /// The milestone a module needs before it can be offered.
    pub fn unlock(self) -> Option<Unlock> {
        match self {
            UpgradeId::Fork => Some(Unlock::Fork),
            UpgradeId::Fragment => Some(Unlock::Fragment),
            UpgradeId::Guidance => Some(Unlock::Guidance),
            UpgradeId::CacheShield => Some(Unlock::CacheShield),
            UpgradeId::ToolCharge => Some(Unlock::ToolCharge),
            UpgradeId::Feedback => Some(Unlock::Feedback),
            _ => None,
        }
    }

    /// The catalog's one-line rule, independent of the current rig.
    pub fn rule(self) -> &'static str {
        match self {
            UpgradeId::Payload => "+15% primary damage per rank",
            UpgradeId::ClockBoost => "+12% primary fire rate per rank",
            UpgradeId::Overclock => "+35% fire rate; -20% projectile damage",
            UpgradeId::HeavyPackets => "+50% projectile damage; -25% fire rate",
            UpgradeId::Fork => "Two angled packets per shot at 30% of base damage",
            UpgradeId::Pierce => "Pass through one more target per rank at 65% damage",
            UpgradeId::Fragment => "Primary kills release two sideways fragments (40%, then 60%)",
            UpgradeId::Guidance => "Primary packets steer toward targets in an 8-degree cone",
            UpgradeId::SpareParts => "Repair one integrity every five completed waves",
            UpgradeId::WarmBackup => "Repair two now; boss clears repair one more",
            UpgradeId::CacheShield => "Absorb one hit; recharges after three completed waves",
            UpgradeId::GracePeriod => "+0.25 s of damage invulnerability per rank",
            UpgradeId::LightFrame => "+15% movement; +10% special recharge time per rank",
            UpgradeId::Grounded => "+25% primary damage; -15% movement",
            UpgradeId::Heatsink => "-15% special recharge time per rank",
            UpgradeId::ToolCharge => "Store a second special charge",
            UpgradeId::ToolAmplifier => "Strengthens the rig's special per rank",
            UpgradeId::Feedback => "Primary hits refund special recharge (0.10 s, then 0.15 s)",
        }
    }
}

/// The ranks a run has acquired.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Build {
    ranks: [u8; 18],
}

impl Build {
    pub fn rank(&self, id: UpgradeId) -> u8 {
        self.ranks[id.index()]
    }

    /// Adds a rank. False, and no change, if it is already capped.
    pub fn add(&mut self, id: UpgradeId) -> bool {
        let rank = &mut self.ranks[id.index()];
        if *rank >= id.cap() {
            return false;
        }
        *rank += 1;
        true
    }

    pub fn with(&self, id: UpgradeId) -> Build {
        let mut next = self.clone();
        next.add(id);
        next
    }

    pub fn acquired(&self) -> impl Iterator<Item = (UpgradeId, u8)> + '_ {
        UpgradeId::ALL.into_iter().filter(|&id| self.rank(id) > 0).map(|id| (id, self.rank(id)))
    }

    pub fn is_empty(&self) -> bool {
        self.ranks.iter().all(|&r| r == 0)
    }
}

/// A rig's statistics with a build applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub damage: f32,
    pub fire_rate: f32,
    pub move_speed: f32,
    pub recharge: f32,
    pub max_charges: u8,
    pub pierce: u8,
    pub fork_damage: Option<f32>,
    pub fragment_share: Option<f32>,
    pub guidance: bool,
    pub invulnerability: f32,
    pub feedback_refund: Option<f32>,
    pub hit_cost: u8,
    pub pivot_jumps: u8,
    pub pivot_direct: f32,
    pub slip_protection: f32,
    pub barrier_protection: f32,
}

pub fn stats(rig: Rig, build: &Build) -> Stats {
    let base = rig.base();
    let r = |id| f32::from(build.rank(id));
    let has = |id| build.rank(id) > 0;
    let damage_bonus =
        1.0 + 0.15 * r(UpgradeId::Payload) + 0.5 * r(UpgradeId::HeavyPackets) + 0.25 * r(UpgradeId::Grounded);
    let damage_penalty = 1.0 - 0.2 * r(UpgradeId::Overclock);
    let rate_bonus = 1.0 + 0.12 * r(UpgradeId::ClockBoost) + 0.35 * r(UpgradeId::Overclock);
    let rate_penalty = 1.0 - 0.25 * r(UpgradeId::HeavyPackets);
    let move_bonus = 1.0 + 0.15 * r(UpgradeId::LightFrame);
    let move_penalty = 1.0 - 0.15 * r(UpgradeId::Grounded);
    let recharge_bonus = 1.0 - 0.15 * r(UpgradeId::Heatsink);
    let recharge_penalty = 1.0 + 0.1 * r(UpgradeId::LightFrame);
    let amplifier = build.rank(UpgradeId::ToolAmplifier);
    let damage = base.damage * damage_bonus * damage_penalty;
    Stats {
        damage,
        fire_rate: base.fire_rate * rate_bonus * rate_penalty,
        move_speed: base.move_speed * move_bonus * move_penalty,
        recharge: (base.recharge * recharge_bonus * recharge_penalty).max(MIN_RECHARGE),
        max_charges: 1 + build.rank(UpgradeId::ToolCharge),
        pierce: base.pierce + build.rank(UpgradeId::Pierce),
        fork_damage: has(UpgradeId::Fork).then_some(damage * FORK_SHARE),
        fragment_share: match build.rank(UpgradeId::Fragment) {
            0 => None,
            1 => Some(0.4),
            _ => Some(0.6),
        },
        guidance: has(UpgradeId::Guidance),
        invulnerability: 1.0 + 0.25 * r(UpgradeId::GracePeriod),
        feedback_refund: match build.rank(UpgradeId::Feedback) {
            0 => None,
            1 => Some(0.10),
            _ => Some(0.15),
        },
        hit_cost: base.hit_cost,
        pivot_jumps: 3 + amplifier,
        pivot_direct: 6.0 + f32::from(amplifier),
        slip_protection: 1.0 + 0.15 * f32::from(amplifier),
        barrier_protection: 2.0 + 0.4 * f32::from(amplifier),
    }
}

// -- offers -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Offer {
    Upgrade(UpgradeId),
    /// Repair one integrity.
    Patch,
    /// Refill special charges.
    Recharge,
    /// Gain 250 × wave score.
    DataCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LevelChoice {
    Module(Offer),
    /// Learn this weapon or add a rank to it if already equipped.
    Weapon(WeaponKind),
}

impl LevelChoice {
    pub fn name(self) -> &'static str {
        match self {
            Self::Module(offer) => offer.name(),
            Self::Weapon(kind) => kind.name(),
        }
    }
}

impl Offer {
    pub fn name(self) -> &'static str {
        match self {
            Offer::Upgrade(id) => id.name(),
            Offer::Patch => "Patch",
            Offer::Recharge => "Recharge",
            Offer::DataCache => "Data Cache",
        }
    }
}

/// What a card's text and eligibility depend on.
#[derive(Debug, Clone, Copy)]
pub struct OfferContext<'a> {
    pub rig: Rig,
    pub build: &'a Build,
    pub integrity: u8,
    pub max_integrity: u8,
    pub charges: u8,
    /// The current wave, or the wave just completed for a wave reward.
    pub wave: u32,
}

pub fn eligible(ctx: &OfferContext<'_>, unlocked: impl Fn(Unlock) -> bool) -> Vec<UpgradeId> {
    UpgradeId::ALL
        .into_iter()
        .filter(|id| id.unlock().is_none_or(&unlocked))
        .filter(|id| ctx.build.rank(*id) < id.cap())
        .collect()
}

fn meaningful_fallbacks(ctx: &OfferContext<'_>) -> Vec<Offer> {
    [
        (Offer::Patch, ctx.integrity < ctx.max_integrity),
        (Offer::Recharge, ctx.charges < stats(ctx.rig, ctx.build).max_charges),
        (Offer::DataCache, true),
    ]
    .into_iter()
    .filter_map(|(offer, meaningful)| meaningful.then_some(offer))
    .collect()
}

/// Up to three distinct choices: eligible upgrades first, then whichever
/// repeatable fallbacks would actually do something.
pub fn generate_offers(rng: &mut Rng, ctx: &OfferContext<'_>, unlocked: impl Fn(Unlock) -> bool) -> Vec<Offer> {
    let mut pool = eligible(ctx, unlocked);
    rng.shuffle(&mut pool);
    let mut offers: Vec<Offer> = pool.into_iter().take(3).map(Offer::Upgrade).collect();
    for offer in meaningful_fallbacks(ctx) {
        if offers.len() < 3 {
            offers.push(offer);
        }
    }
    offers
}

/// Three seeded choices when enough distinct improvements remain. Keep a
/// weapon and a module in the mix whenever both categories are available.
pub fn level_offers(
    rng: &mut Rng,
    ctx: &OfferContext<'_>,
    weapons: &[WeaponState],
    unlocked: impl Fn(Unlock) -> bool,
) -> Vec<LevelChoice> {
    let mut weapon_pool: Vec<_> = WeaponKind::ALL
        .into_iter()
        .filter(|&kind| can_acquire_weapon(kind, weapons))
        .map(LevelChoice::Weapon)
        .collect();
    let mut module_pool: Vec<_> =
        eligible(ctx, unlocked).into_iter().map(|id| LevelChoice::Module(Offer::Upgrade(id))).collect();
    rng.shuffle(&mut weapon_pool);
    rng.shuffle(&mut module_pool);
    let mut choices = Vec::with_capacity(3);
    if let Some(weapon) = weapon_pool.pop() {
        choices.push(weapon);
    }
    if let Some(module) = module_pool.pop() {
        choices.push(module);
    }
    weapon_pool.append(&mut module_pool);
    rng.shuffle(&mut weapon_pool);
    choices.extend(weapon_pool.into_iter().take(3 - choices.len()));
    for offer in meaningful_fallbacks(ctx) {
        if choices.len() < 3 {
            choices.push(LevelChoice::Module(offer));
        }
    }
    rng.shuffle(&mut choices);
    choices
}

/// One upgrade card, with numbers computed for the current rig and build.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub title: String,
    /// "New", "Rank 1 of 3 -> 2 of 3", or "Repeatable".
    pub rank: String,
    pub effect: String,
    pub downside: Option<String>,
}

fn weapon_effect(kind: WeaponKind, rank: u8, ctx: &OfferContext<'_>) -> String {
    let stats = stats(ctx.rig, ctx.build);
    weapon_details(kind, &kind.spec(rank, ctx.rig, &stats))
}

/// A weapon's computed numbers, as its cards and the Build screen show them.
pub fn weapon_details(kind: WeaponKind, spec: &WeaponSpec) -> String {
    let cadence = format!("{:.2} damage; every {:.2} s", spec.damage, spec.cooldown);
    match kind {
        WeaponKind::Packet => {
            format!("{cadence}; {} packet; {} extra pierce; {:.1} range.", spec.projectiles, spec.pierce, spec.range)
        }
        WeaponKind::Scatter => format!(
            "{cadence} per packet; {} packets over {:.0} degrees; {} extra pierce; {:.1} range.",
            spec.projectiles,
            spec.spread.to_degrees(),
            spec.pierce,
            spec.range
        ),
        WeaponKind::Rail => {
            format!("{cadence}; {} extra pierce at 65% damage retention; {:.1} range.", spec.pierce, spec.range)
        }
        WeaponKind::Arc => format!(
            "{cadence} per target; up to {} distinct targets; {:.1} jump range.",
            spec.chain_targets, spec.range
        ),
        WeaponKind::Orbit => format!(
            "{cadence} per contact; {} rotating packets at {:.1} radius; {:.1} s lifetime.",
            spec.projectiles,
            spec.radius,
            spec.range / spec.speed
        ),
        WeaponKind::Pulse => format!("{cadence} to each device within {:.1} radius.", spec.radius),
    }
}

fn weapon_downside(kind: WeaponKind) -> Option<String> {
    match kind {
        WeaponKind::Packet => None,
        WeaponKind::Scatter => Some("Weaker individual packets and shorter range than Packet.".into()),
        WeaponKind::Rail => Some("Slower fire; each penetration keeps 65% of the previous hit's damage.".into()),
        WeaponKind::Arc => Some("One chain can hit at most three distinct devices.".into()),
        WeaponKind::Orbit => {
            Some("The rotating packets stay near your rig instead of pursuing distant targets.".into())
        }
        WeaponKind::Pulse => Some("Short-range only; devices outside the radius are unaffected.".into()),
    }
}

fn weapon_changes(ctx: &OfferContext<'_>, weapons: &[WeaponState], next: &Stats, damage: bool) -> String {
    let now = stats(ctx.rig, ctx.build);
    let changes: Vec<_> = weapons
        .iter()
        .map(|weapon| {
            let before = weapon.spec(ctx.rig, &now);
            let after = weapon.spec(ctx.rig, next);
            let values = if damage {
                arrow(before.damage, after.damage, 2)
            } else {
                arrow(1.0 / before.cooldown, 1.0 / after.cooldown, 2)
            };
            format!("{} {values}", weapon.kind.name())
        })
        .collect();
    format!("{}: {}.", if damage { "Damage per hit" } else { "Activations/s" }, changes.join("; "))
}

fn projectile_weapon(kind: WeaponKind) -> bool {
    matches!(kind, WeaponKind::Packet | WeaponKind::Scatter | WeaponKind::Rail)
}

pub fn describe_level(choice: LevelChoice, ctx: &OfferContext<'_>, weapons: &[WeaponState]) -> Card {
    match choice {
        LevelChoice::Module(offer) => {
            let mut card = describe(offer, ctx);
            if let Offer::Upgrade(id) = offer
                && !weapons.is_empty()
            {
                let next = stats(ctx.rig, &ctx.build.with(id));
                match id {
                    UpgradeId::Payload | UpgradeId::Grounded => card.effect = weapon_changes(ctx, weapons, &next, true),
                    UpgradeId::ClockBoost => card.effect = weapon_changes(ctx, weapons, &next, false),
                    UpgradeId::Overclock => {
                        card.effect = weapon_changes(ctx, weapons, &next, false);
                        card.downside = Some(weapon_changes(ctx, weapons, &next, true));
                    }
                    UpgradeId::HeavyPackets => {
                        card.effect = weapon_changes(ctx, weapons, &next, true);
                        card.downside = Some(weapon_changes(ctx, weapons, &next, false));
                    }
                    UpgradeId::Fork => {
                        let packets: Vec<_> = weapons
                            .iter()
                            .filter(|weapon| projectile_weapon(weapon.kind))
                            .map(|weapon| {
                                format!("{} {:.2}", weapon.kind.name(), weapon.spec(ctx.rig, &next).damage * FORK_SHARE)
                            })
                            .collect();
                        card.effect = format!(
                            "Each primary packet adds two angled packets at 30% damage ({} per added packet). Added packets do not pierce.",
                            packets.join("; ")
                        );
                    }
                    UpgradeId::Pierce => {
                        let now = stats(ctx.rig, ctx.build);
                        let changes: Vec<_> = weapons
                            .iter()
                            .filter(|weapon| projectile_weapon(weapon.kind))
                            .map(|weapon| {
                                let before = weapon.spec(ctx.rig, &now);
                                let after = weapon.spec(ctx.rig, &next);
                                let hits: Vec<_> = (0..=after.pierce)
                                    .map(|hit| format!("{:.2}", after.damage * PIERCE_RETAIN.powi(i32::from(hit))))
                                    .collect();
                                format!(
                                    "{} {} -> {} extra targets; damage {}",
                                    weapon.kind.name(),
                                    before.pierce,
                                    after.pierce,
                                    hits.join(", ")
                                )
                            })
                            .collect();
                        card.effect = format!("{}. Each penetration keeps 65% damage.", changes.join("; "));
                    }
                    UpgradeId::Fragment => {
                        card.effect = format!(
                            "Packet, Scatter and Rail kills emit two perpendicular fragments at {:.0}% of the killing hit. Fragments, Arc, Orbit, Pulse and specials do not create further fragments.",
                            next.fragment_share.unwrap_or_default() * 100.0
                        );
                    }
                    UpgradeId::Guidance => {
                        card.effect = "Packet, Scatter and Rail projectiles steer toward targets inside an 8-degree cone ahead of them.".into();
                    }
                    UpgradeId::Feedback => {
                        card.effect = format!(
                            "Projectile weapon hits refund {:.2} s of {} recharge, at most twice per second. Arc, Orbit, Pulse and fragments do not trigger Feedback.",
                            next.feedback_refund.unwrap_or_default(),
                            ctx.rig.special_name()
                        );
                    }
                    _ => {}
                }
            }
            card
        }
        LevelChoice::Weapon(kind) => {
            let rank = weapons.iter().find(|weapon| weapon.kind == kind).map_or(0, |weapon| weapon.rank);
            let next = rank.saturating_add(1).min(kind.cap());
            Card {
                title: kind.name().into(),
                rank: if rank == 0 {
                    format!("New weapon · rank 1 of {} · slot {} of {MAX_WEAPONS}", kind.cap(), weapons.len() + 1)
                } else {
                    format!("Weapon rank {rank} -> {next} of {}", kind.cap())
                },
                effect: if rank == 0 {
                    format!("{} {}", kind.description(), weapon_effect(kind, next, ctx))
                } else {
                    format!("Before: {}\nAfter: {}", weapon_effect(kind, rank, ctx), weapon_effect(kind, next, ctx))
                },
                downside: weapon_downside(kind),
            }
        }
    }
}

fn arrow(before: f32, after: f32, decimals: usize) -> String {
    format!("{before:.decimals$} -> {after:.decimals$}")
}

pub fn describe(offer: Offer, ctx: &OfferContext<'_>) -> Card {
    let rig = ctx.rig;
    let special = rig.special_name();
    let now = stats(rig, ctx.build);
    let (id, next) = match offer {
        Offer::Upgrade(id) => (id, stats(rig, &ctx.build.with(id))),
        Offer::Patch => {
            let after = (ctx.integrity + 1).min(ctx.max_integrity);
            return Card {
                title: "Patch".into(),
                rank: "Repeatable".into(),
                effect: format!("Repair 1 integrity ({} -> {after} of {}).", ctx.integrity, ctx.max_integrity),
                downside: None,
            };
        }
        Offer::Recharge => {
            return Card {
                title: "Recharge".into(),
                rank: "Repeatable".into(),
                effect: format!("Refill {special} charges ({} -> {}).", ctx.charges, now.max_charges),
                downside: None,
            };
        }
        Offer::DataCache => {
            return Card {
                title: "Data Cache".into(),
                rank: "Repeatable".into(),
                effect: format!("Gain {} score.", 250 * u64::from(ctx.wave.max(1))),
                downside: None,
            };
        }
    };
    let rank = ctx.build.rank(id);
    let rank_text = if id.cap() == 1 {
        "New · one rank".to_string()
    } else if rank == 0 {
        format!("New · rank 1 of {}", id.cap())
    } else {
        format!("Rank {rank} -> {} of {}", rank + 1, id.cap())
    };
    let damage = || format!("Primary damage {} per packet.", arrow(now.damage, next.damage, 2));
    let rate = || format!("Fire rate {} packets/s.", arrow(now.fire_rate, next.fire_rate, 1));
    let speed = || format!("Movement {} columns/s.", arrow(now.move_speed, next.move_speed, 1));
    let recharge = || format!("{special} recharge {} s.", arrow(now.recharge, next.recharge, 1));
    let (effect, downside) = match id {
        UpgradeId::Payload => (damage(), None),
        UpgradeId::ClockBoost => (rate(), None),
        UpgradeId::Overclock => (rate(), Some(format!("Packet damage {}.", arrow(now.damage, next.damage, 2)))),
        UpgradeId::HeavyPackets => {
            (damage(), Some(format!("Fire rate {} packets/s.", arrow(now.fire_rate, next.fire_rate, 1))))
        }
        UpgradeId::Fork => (
            format!(
                "Each shot adds two angled packets dealing {:.2} damage each (30% of primary damage including bonuses and penalties; they do not pierce).",
                next.fork_damage.unwrap_or(0.0)
            ),
            None,
        ),
        UpgradeId::Pierce => {
            // The damage each successive target takes, at 65% per penetration.
            let hits: Vec<String> =
                (0..=next.pierce).map(|k| format!("{:.2}", next.damage * PIERCE_RETAIN.powi(i32::from(k)))).collect();
            (
                format!(
                    "Packets pass through {} -> {} additional target(s), dealing {} damage in turn.",
                    now.pierce,
                    next.pierce,
                    hits.join(", then ")
                ),
                None,
            )
        }
        UpgradeId::Fragment => (
            match (now.fragment_share, next.fragment_share) {
                (None, Some(share)) => format!(
                    "Primary kills release two sideways fragments dealing 40% of the killing hit ({:.2} each from a full packet).",
                    next.damage * share
                ),
                (Some(before), Some(after)) => format!(
                    "Fragments deal 40% -> 60% of the killing hit ({} each from a full packet).",
                    arrow(now.damage * before, next.damage * after, 2)
                ),
                _ => String::new(),
            },
            None,
        ),
        UpgradeId::Guidance => {
            ("Primary packets steer toward devices inside an 8-degree cone ahead of them.".into(), None)
        }
        UpgradeId::SpareParts => (
            format!(
                "Repair 1 integrity every {SPARE_PARTS_INTERVAL} completed waves, starting after wave {}.",
                ctx.wave + SPARE_PARTS_INTERVAL
            ),
            None,
        ),
        UpgradeId::WarmBackup => (
            format!(
                "Repair 2 integrity now ({} -> {} of {}). Later boss clears repair 1 more.",
                ctx.integrity,
                (ctx.integrity + 2).min(ctx.max_integrity),
                ctx.max_integrity
            ),
            None,
        ),
        UpgradeId::CacheShield => (
            format!(
                "Absorb the next hit completely. Recharges {CACHE_SHIELD_RECHARGE} completed waves after absorbing one."
            ),
            None,
        ),
        UpgradeId::GracePeriod => {
            (format!("Invulnerability after a hit {} s.", arrow(now.invulnerability, next.invulnerability, 2)), None)
        }
        UpgradeId::LightFrame => (speed(), Some(recharge())),
        UpgradeId::Grounded => (damage(), Some(speed())),
        UpgradeId::Heatsink => (recharge(), None),
        UpgradeId::ToolCharge => {
            (format!("Store a second {special} charge; charges recharge one after another."), None)
        }
        UpgradeId::ToolAmplifier => (
            match rig {
                Rig::Probe => format!(
                    "Pivot chains {} extra devices and hits for {} direct damage.",
                    format_args!("{} -> {}", now.pivot_jumps, next.pivot_jumps),
                    arrow(now.pivot_direct, next.pivot_direct, 0)
                ),
                Rig::Ghost => format!("Slip protection {} s.", arrow(now.slip_protection, next.slip_protection, 2)),
                Rig::Bastion => {
                    format!("Barrier lasts {} s.", arrow(now.barrier_protection, next.barrier_protection, 1))
                }
            },
            None,
        ),
        UpgradeId::Feedback => (
            match (now.feedback_refund, next.feedback_refund) {
                (None, Some(after)) => {
                    format!("Primary hits refund {after:.2} s of {special} recharge, at most twice per second.")
                }
                (Some(before), Some(after)) => format!("Refund per primary hit {} s.", arrow(before, after, 2)),
                _ => String::new(),
            },
            None,
        ),
    };
    Card { title: id.name().into(), rank: rank_text, effect, downside }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn level_choices_mix_weapons_and_modules_deterministically_without_violating_caps() {
        let build = Build::default();
        let ctx = OfferContext { rig: Rig::Probe, build: &build, integrity: 5, max_integrity: 5, charges: 1, wave: 1 };
        let weapons = [WeaponState::new(WeaponKind::Packet)];
        for seed in 0..100 {
            let choices = level_offers(&mut Rng::new(seed), &ctx, &weapons, |_| false);
            assert_eq!(choices, level_offers(&mut Rng::new(seed), &ctx, &weapons, |_| false));
            assert_eq!(choices.len(), 3);
            assert!(choices.iter().any(|choice| matches!(choice, LevelChoice::Weapon(_))));
            assert!(choices.iter().any(|choice| matches!(choice, LevelChoice::Module(Offer::Upgrade(_)))));
            assert!(choices.iter().enumerate().all(|(i, choice)| !choices[..i].contains(choice)));
            assert!(choices.iter().all(|choice| match choice {
                LevelChoice::Module(Offer::Upgrade(id)) => id.unlock().is_none(),
                _ => true,
            }));
        }
        let full = [
            WeaponState { kind: WeaponKind::Packet, rank: 5 },
            WeaponState { kind: WeaponKind::Scatter, rank: 5 },
            WeaponState { kind: WeaponKind::Rail, rank: 5 },
            WeaponState { kind: WeaponKind::Arc, rank: 4 },
        ];
        for seed in 0..50 {
            let choices = level_offers(&mut Rng::new(seed), &ctx, &full, |_| true);
            let offered: Vec<_> = choices
                .into_iter()
                .filter_map(|choice| match choice {
                    LevelChoice::Weapon(kind) => Some(kind),
                    _ => None,
                })
                .collect();
            assert_eq!(offered, [WeaponKind::Arc]);
        }
        let mut capped_build = Build::default();
        for id in UpgradeId::ALL {
            while capped_build.add(id) {}
        }
        let capped_weapons = full.map(|weapon| WeaponState { rank: 5, ..weapon });
        let healthy = OfferContext { build: &capped_build, charges: 2, ..ctx };
        assert_eq!(
            level_offers(&mut Rng::new(1), &healthy, &capped_weapons, |_| true),
            [LevelChoice::Module(Offer::DataCache)]
        );
        let hurt = OfferContext { integrity: 2, charges: 0, ..healthy };
        let choices = level_offers(&mut Rng::new(1), &hurt, &capped_weapons, |_| true);
        assert_eq!(choices.len(), 3);
        for fallback in [Offer::Patch, Offer::Recharge, Offer::DataCache] {
            assert!(choices.contains(&LevelChoice::Module(fallback)));
        }
    }

    #[test]
    fn weapon_and_module_cards_use_current_weapon_ranks_and_computed_stats() {
        let build = Build::default().with(UpgradeId::Payload);
        let ctx = OfferContext { rig: Rig::Ghost, build: &build, integrity: 3, max_integrity: 5, charges: 0, wave: 4 };
        let weapons = [WeaponState { kind: WeaponKind::Packet, rank: 2 }, WeaponState::new(WeaponKind::Rail)];
        let card = describe_level(LevelChoice::Weapon(WeaponKind::Packet), &ctx, &weapons);
        let current = stats(ctx.rig, &build);
        let before = WeaponKind::Packet.spec(2, ctx.rig, &current);
        let after = WeaponKind::Packet.spec(3, ctx.rig, &current);
        assert_eq!(card.rank, "Weapon rank 2 -> 3 of 5");
        assert!(card.effect.contains(&format!("{:.2} damage", before.damage)));
        assert!(card.effect.contains(&format!("{:.2} damage", after.damage)));
        let new = describe_level(LevelChoice::Weapon(WeaponKind::Pulse), &ctx, &weapons);
        assert!(new.rank.contains("New weapon") && new.rank.contains("slot 3 of 4"));
        assert!(new.effect.contains("9.0 radius") && new.downside.is_some());
        let overclock = describe_level(LevelChoice::Module(Offer::Upgrade(UpgradeId::Overclock)), &ctx, &weapons);
        assert!(overclock.effect.contains("Activations/s: Packet") && overclock.effect.contains("Rail"));
        let downside = overclock.downside.unwrap();
        assert!(downside.contains("Damage per hit: Packet") && downside.contains("Rail"));
        assert_eq!(describe_level(LevelChoice::Module(Offer::Patch), &ctx, &weapons), describe(Offer::Patch, &ctx));
    }

    #[test]
    fn twelve_upgrades_start_available_and_six_modules_unlock() {
        let initial: Vec<_> = UpgradeId::ALL.into_iter().filter(|id| id.unlock().is_none()).collect();
        assert_eq!(initial.len(), 12);
        let locked: Vec<_> = UpgradeId::ALL.into_iter().filter_map(UpgradeId::unlock).collect();
        assert_eq!(
            locked,
            [
                Unlock::Fork,
                Unlock::Fragment,
                Unlock::Guidance,
                Unlock::CacheShield,
                Unlock::ToolCharge,
                Unlock::Feedback
            ]
        );
        for id in UpgradeId::ALL {
            assert_eq!(UpgradeId::from_id(id.id()), Some(id));
        }
    }

    #[test]
    fn ranks_stop_at_their_caps() {
        let mut build = Build::default();
        for id in UpgradeId::ALL {
            for _ in 0..id.cap() {
                assert!(build.add(id));
            }
            assert!(!build.add(id), "{id:?} went past its cap");
            assert_eq!(build.rank(id), id.cap());
        }
        let caps: Vec<u8> = UpgradeId::ALL.map(UpgradeId::cap).into();
        assert_eq!(caps, [3, 3, 1, 1, 1, 2, 2, 1, 1, 1, 1, 2, 2, 1, 2, 1, 2, 2]);
    }

    #[test]
    fn bonuses_add_and_penalties_apply_afterward() {
        let mut build = Build::default();
        for id in
            [UpgradeId::Payload, UpgradeId::Payload, UpgradeId::HeavyPackets, UpgradeId::Grounded, UpgradeId::Overclock]
        {
            build.add(id);
        }
        let s = stats(Rig::Probe, &build);
        // (1 + 0.30 + 0.50 + 0.25) × (1 − 0.20)
        assert!(close(s.damage, 2.05 * 0.8));
        // (1 + 0.35) × (1 − 0.25)
        assert!(close(s.fire_rate, 6.6 * 1.35 * 0.75));
        assert!(close(s.move_speed, 26.0 * 0.85));
        let mut frame = Build::default();
        for id in [UpgradeId::Heatsink, UpgradeId::Heatsink, UpgradeId::LightFrame, UpgradeId::LightFrame] {
            frame.add(id);
        }
        let s = stats(Rig::Bastion, &frame);
        assert!(close(s.recharge, 14.0 * 0.7 * 1.2));
        assert!(close(s.move_speed, 19.0 * 1.3));
    }

    #[test]
    fn rig_specific_effects_and_fractional_damage_survive() {
        let mut build = Build::default();
        build.add(UpgradeId::Pierce);
        build.add(UpgradeId::Fork);
        let bastion = stats(Rig::Bastion, &build);
        assert_eq!(bastion.pierce, 2, "inherent penetration combines with Pierce");
        assert!(close(bastion.fork_damage.unwrap(), 0.9));
        let probe = stats(Rig::Probe, &build);
        assert_eq!(probe.pierce, 1);
        assert!(close(probe.fork_damage.unwrap(), 0.3));
        assert_eq!(stats(Rig::Ghost, &Build::default()).hit_cost, 2);
        let mut amp = Build::default();
        amp.add(UpgradeId::ToolAmplifier);
        amp.add(UpgradeId::ToolAmplifier);
        let s = stats(Rig::Probe, &amp);
        assert_eq!((s.pivot_jumps, s.pivot_direct), (5, 8.0));
        assert!(close(s.slip_protection, 1.3));
        assert!(close(s.barrier_protection, 2.8));
        let mut feedback = Build::default();
        feedback.add(UpgradeId::Feedback);
        assert_eq!(stats(Rig::Ghost, &feedback).feedback_refund, Some(0.10));
        feedback.add(UpgradeId::Feedback);
        assert_eq!(stats(Rig::Ghost, &feedback).feedback_refund, Some(0.15));
        let mut grace = Build::default();
        grace.add(UpgradeId::GracePeriod);
        assert!(close(stats(Rig::Probe, &grace).invulnerability, 1.25));
    }

    #[test]
    fn offers_are_distinct_seeded_and_fall_back_meaningfully() {
        let build = Build::default();
        let ctx = OfferContext { rig: Rig::Probe, build: &build, integrity: 5, max_integrity: 5, charges: 1, wave: 3 };
        let a = generate_offers(&mut Rng::new(9), &ctx, |_| false);
        assert_eq!(a, generate_offers(&mut Rng::new(9), &ctx, |_| false));
        assert_eq!(a.len(), 3);
        assert!(a.iter().all(|o| matches!(o, Offer::Upgrade(id) if id.unlock().is_none())));
        assert!(a[0] != a[1] && a[1] != a[2] && a[0] != a[2]);

        // Everything capped: only the fallbacks that would do something.
        let mut full = Build::default();
        for id in UpgradeId::ALL {
            while full.add(id) {}
        }
        let healthy = OfferContext { build: &full, charges: 2, ..ctx };
        assert_eq!(generate_offers(&mut Rng::new(1), &healthy, |_| true), [Offer::DataCache]);
        let hurt = OfferContext { build: &full, integrity: 2, charges: 0, ..ctx };
        assert_eq!(
            generate_offers(&mut Rng::new(1), &hurt, |_| true),
            [Offer::Patch, Offer::Recharge, Offer::DataCache]
        );
        // Two upgrades left: they come first, then one fallback.
        let mut almost = full.clone();
        almost.ranks[UpgradeId::Payload.index()] = 0;
        almost.ranks[UpgradeId::Fork.index()] = 0;
        let offers = generate_offers(&mut Rng::new(1), &OfferContext { build: &almost, ..hurt }, |_| true);
        assert_eq!(offers.len(), 3);
        assert!(offers[..2].contains(&Offer::Upgrade(UpgradeId::Payload)));
        assert!(offers[..2].contains(&Offer::Upgrade(UpgradeId::Fork)));
        assert_eq!(offers[2], Offer::Patch);
        // Locked modules are never offered.
        for seed in 0..200 {
            let offers = generate_offers(&mut Rng::new(seed), &ctx, |u| u == Unlock::Fork);
            assert!(
                !offers
                    .iter()
                    .any(|o| matches!(o, Offer::Upgrade(id) if id.unlock().is_some_and(|u| u != Unlock::Fork)))
            );
        }
    }

    #[test]
    fn cards_describe_the_computed_result_for_the_current_rig() {
        let build = Build::default();
        let ctx = |rig| OfferContext { rig, build: &build, integrity: 3, max_integrity: 5, charges: 0, wave: 6 };
        let card = describe(Offer::Upgrade(UpgradeId::Payload), &ctx(Rig::Bastion));
        assert_eq!(card.effect, "Primary damage 3.00 -> 3.45 per packet.");
        assert_eq!(card.rank, "New · rank 1 of 3");
        let amp = |rig| describe(Offer::Upgrade(UpgradeId::ToolAmplifier), &ctx(rig)).effect;
        assert_eq!(amp(Rig::Probe), "Pivot chains 3 -> 4 extra devices and hits for 6 -> 7 direct damage.");
        assert_eq!(amp(Rig::Ghost), "Slip protection 1.00 -> 1.15 s.");
        assert_eq!(amp(Rig::Bastion), "Barrier lasts 2.0 -> 2.4 s.");
        let overclock = describe(Offer::Upgrade(UpgradeId::Overclock), &ctx(Rig::Ghost));
        assert_eq!(overclock.effect, "Fire rate 8.8 -> 11.9 packets/s.");
        assert_eq!(overclock.downside.as_deref(), Some("Packet damage 1.00 -> 0.80."));
        let frame = describe(Offer::Upgrade(UpgradeId::LightFrame), &ctx(Rig::Probe));
        assert_eq!(frame.downside.as_deref(), Some("Pivot recharge 12.0 -> 13.2 s."));
        assert_eq!(describe(Offer::Patch, &ctx(Rig::Probe)).effect, "Repair 1 integrity (3 -> 4 of 5).");
        assert_eq!(describe(Offer::DataCache, &ctx(Rig::Probe)).effect, "Gain 1500 score.");
        assert_eq!(describe(Offer::Recharge, &ctx(Rig::Ghost)).effect, "Refill Slip charges (0 -> 1).");
        let mut ranked = Build::default();
        ranked.add(UpgradeId::ClockBoost);
        let card = describe(Offer::Upgrade(UpgradeId::ClockBoost), &OfferContext { build: &ranked, ..ctx(Rig::Probe) });
        assert_eq!(card.rank, "Rank 1 -> 2 of 3");
        assert_eq!(card.effect, "Fire rate 7.4 -> 8.2 packets/s.");
        let mut modified = Build::default();
        for id in [UpgradeId::Payload, UpgradeId::HeavyPackets, UpgradeId::Overclock] {
            modified.add(id);
        }
        let card = describe(Offer::Upgrade(UpgradeId::Fork), &OfferContext { build: &modified, ..ctx(Rig::Bastion) });
        assert_eq!(
            card.effect,
            "Each shot adds two angled packets dealing 1.19 damage each (30% of primary damage including bonuses and penalties; they do not pierce)."
        );
        // Pierce and Fragment name the damage they would actually deal.
        let pierce = |rig| describe(Offer::Upgrade(UpgradeId::Pierce), &ctx(rig)).effect;
        assert_eq!(
            pierce(Rig::Probe),
            "Packets pass through 0 -> 1 additional target(s), dealing 1.00, then 0.65 damage in turn."
        );
        assert_eq!(
            pierce(Rig::Bastion),
            "Packets pass through 1 -> 2 additional target(s), dealing 3.00, then 1.95, then 1.27 damage in turn."
        );
        assert_eq!(
            describe(Offer::Upgrade(UpgradeId::Fragment), &ctx(Rig::Probe)).effect,
            "Primary kills release two sideways fragments dealing 40% of the killing hit (0.40 each from a full packet)."
        );
        let mut fragment = Build::default();
        fragment.add(UpgradeId::Fragment);
        assert_eq!(
            describe(Offer::Upgrade(UpgradeId::Fragment), &OfferContext { build: &fragment, ..ctx(Rig::Bastion) })
                .effect,
            "Fragments deal 40% -> 60% of the killing hit (1.20 -> 1.80 each from a full packet)."
        );
        // Exactly the four tradeoff upgrades state a downside, on every rig.
        let tradeoffs = [UpgradeId::Overclock, UpgradeId::HeavyPackets, UpgradeId::LightFrame, UpgradeId::Grounded];
        for id in UpgradeId::ALL {
            for rig in Rig::ALL {
                let card = describe(Offer::Upgrade(id), &ctx(rig));
                assert!(!card.effect.is_empty(), "{id:?} on {rig:?}");
                assert_eq!(card.downside.is_some(), tradeoffs.contains(&id), "{id:?} on {rig:?}");
                assert_eq!(card.title, id.name());
            }
        }
    }
}
