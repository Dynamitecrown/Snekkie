//! Rare elite cargo. Items are unique passives for the current run.

use super::content::{Rig, Unlock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemKind {
    JumboFrame,
    PortMirroring,
    PacketSniffer,
    Failover,
}

impl ItemKind {
    pub const ALL: [Self; 4] = [Self::JumboFrame, Self::PortMirroring, Self::PacketSniffer, Self::Failover];

    pub fn name(self) -> &'static str {
        match self {
            Self::JumboFrame => "Jumbo Frame",
            Self::PortMirroring => "Port Mirroring",
            Self::PacketSniffer => "Packet Sniffer",
            Self::Failover => "Failover",
        }
    }

    pub fn unlock(self) -> Option<Unlock> {
        (self == Self::Failover).then_some(Unlock::Failover)
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::JumboFrame => "All player projectiles are 25% larger and travel 50% slower (including orbitals).",
            Self::PortMirroring => {
                "Directional attacks also fire backward; each direction has a 25% chance to miss. Radial weapons already cover both sides."
            }
            Self::PacketSniffer => "The XP magnet reaches twice as far.",
            Self::Failover => {
                "Survive one fatal hit with 1 integrity and two seconds of protection. Failover is then spent; another elite can drop it again."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemDrop {
    pub kind: ItemKind,
    pub x: f32,
    pub y: f32,
}

/// The special on X. `Chain` stands for the rig's own special (Probe's
/// chaining Pivot, Ghost's Slip, Bastion's Barrier and so on).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PivotKind {
    #[default]
    Chain,
    Missile,
    Cloak,
    Stinger,
    RateLimit,
}

/// Stinger's cloud radius, in visual units.
pub const STINGER_RADIUS: f32 = 6.0;
/// Poison stacks one enemy can carry.
pub const POISON_MAX_STACKS: u8 = 8;
/// Seconds stacks last after the newest. Longer than every rig's base
/// recharge, so repeat casts build stacks instead of just refreshing one.
pub const POISON_DURATION: f32 = 14.5;
/// Rate Limit: hostile packets and ordinary ships move at this fraction of
/// their speed, for this many seconds plus one per Tool Amplifier rank.
pub const RATE_LIMIT_SLOW: f32 = 0.35;
pub const RATE_LIMIT_TIME: f32 = 4.0;

impl PivotKind {
    pub const ALL: [Self; 5] = [Self::Chain, Self::Missile, Self::Cloak, Self::Stinger, Self::RateLimit];

    /// The specials a rig can start a run with, unlocked or not.
    pub fn options(rig: Rig) -> &'static [Self] {
        match rig {
            Rig::Probe => &[Self::Chain, Self::Missile, Self::RateLimit, Self::Stinger],
            Rig::Ghost => &[Self::Chain, Self::RateLimit, Self::Cloak, Self::Stinger],
            _ => &[Self::Chain, Self::RateLimit, Self::Stinger],
        }
    }

    pub fn unlock(self) -> Option<Unlock> {
        match self {
            Self::Cloak => Some(Unlock::Cloak),
            Self::Stinger => Some(Unlock::Stinger),
            Self::RateLimit => Some(Unlock::RateLimit),
            Self::Chain | Self::Missile => None,
        }
    }

    /// Whether `rig` may start a run with this special.
    pub fn allowed(self, rig: Rig, unlocked: impl Fn(Unlock) -> bool) -> bool {
        Self::options(rig).contains(&self) && self.unlock().is_none_or(unlocked)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Chain => "Chain Pivot",
            Self::Missile => "Missile Pivot",
            Self::Cloak => "Cloak",
            Self::Stinger => "Stinger",
            Self::RateLimit => "Rate Limit",
        }
    }

    /// The name shown for `rig`: `Chain` is called by the rig's own special.
    pub fn label(self, rig: Rig) -> &'static str {
        match (self, rig) {
            (Self::Chain, Rig::Probe) => self.name(),
            (Self::Chain, rig) => rig.special_name(),
            _ => self.name(),
        }
    }

    pub fn description(self) -> &'static str {
        self.description_for(Rig::Probe)
    }

    pub fn description_for(self, rig: Rig) -> &'static str {
        match (self, rig) {
            (Self::Chain, Rig::Ghost) => "Slip: a short protected dash along the nose.",
            (Self::Chain, Rig::Bastion) => "Barrier: blocks every hit for two seconds.",
            (Self::Chain, Rig::Tonk) => {
                "Reflect: a shield that blocks hits for 2.5 seconds and bounces ordinary packets back at the swarm. Boss attacks are only blocked."
            }
            (Self::Chain, Rig::Relay) => "Overclock: every weapon fires about twice as often for four seconds.",
            (Self::Chain, Rig::Kernel) => {
                "Purge: erases hostile packets within 16 units of the rig and damages every device in that radius. Boss scans and curtains are not packets."
            }
            (Self::Chain, _) => "A heavy packet chains through nearby devices.",
            (Self::Missile, _) => {
                "Locks onto the nearest visible enemy with at least 6 HP, falling back to the nearest enemy. Explodes in an 8-unit radius for double Pivot damage; Tool Amplifier increases damage and radius."
            }
            (Self::Cloak, _) => {
                "Turn invisible and unhittable for three seconds. Ordinary ships cannot target you while cloaked."
            }
            (Self::Stinger, _) => {
                "A big, slow, short-range poison glob. It bursts into a cloud that poisons every enemy inside; poison deals slow damage over time and stacks."
            }
            (Self::RateLimit, _) => {
                "Throttle the network for four seconds: hostile packets and ordinary ships move at about a third of their speed. Scans, curtains and bosses are not slowed; Tool Amplifier adds a second per rank."
            }
        }
    }
}
