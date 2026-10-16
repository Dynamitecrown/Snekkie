//! Invade: a silent ASCII free-flight swarm roguelite in its own tab.
//!
//! A fictional network-intrusion game. Its devices and attacks are game
//! entities: nothing here inspects connected devices, reads session
//! contents, sends network traffic or issues terminal commands. The model
//! and its progress store never receive sessions, profiles, transports, the
//! clipboard or a terminal emulator.

pub mod arsenal;
#[doc(hidden)]
pub mod autoplay;
pub mod content;
pub mod progress;
pub mod rng;
pub mod run;
pub mod upgrades;

pub use arsenal::{Loadout, StarterModule, WeaponKind, WeaponSpec, WeaponState};
pub use content::{BossKind, DeviceKind, EncounterKind, MinibossKind, Rig, Unlock};
pub use progress::{Progress, ProgressStore};
pub use run::{EndReason, Event, Input, InvadeRun, Phase, Summary};
pub use upgrades::{Build, LevelChoice, Offer, UpgradeId, describe_level, level_offers};

/// The progress file's name, beside settings.json.
pub const PROGRESS_FILE: &str = "invade.json";
