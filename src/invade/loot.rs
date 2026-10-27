//! Rare elite cargo. Items are unique passives for the current run.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemKind {
    JumboFrame,
    PortMirroring,
}

impl ItemKind {
    pub const ALL: [Self; 2] = [Self::JumboFrame, Self::PortMirroring];

    pub fn name(self) -> &'static str {
        match self {
            Self::JumboFrame => "Jumbo Frame",
            Self::PortMirroring => "Port Mirroring",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::JumboFrame => "All player projectiles are 25% larger and travel 50% slower (including orbitals).",
            Self::PortMirroring => {
                "Directional attacks also fire backward; each direction has a 25% chance to miss. Radial weapons already cover both sides."
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PivotKind {
    #[default]
    Chain,
    Missile,
}

impl PivotKind {
    pub const ALL: [Self; 2] = [Self::Chain, Self::Missile];

    pub fn name(self) -> &'static str {
        match self {
            Self::Chain => "Chain Pivot",
            Self::Missile => "Missile Pivot",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Chain => "A heavy packet chains through nearby devices.",
            Self::Missile => {
                "Locks onto the nearest visible enemy with at least 6 HP, falling back to the nearest enemy. Explodes in an 8-unit radius for double Pivot damage; Tool Amplifier increases damage and radius."
            }
        }
    }
}
