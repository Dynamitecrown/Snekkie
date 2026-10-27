//! Elite cargo, reflected packets and Probe's optional homing Pivot.
use super::*;

impl InvadeRun {
    /// Place elite cargo for deterministic QA scenes.
    #[doc(hidden)]
    pub fn place_elite(&mut self, at: (f32, f32)) {
        self.add_enemy(Role::Device(DeviceKind::Firewall), at, (3.0, 1.0), 24.0, true, 800);
        self.enemies.last_mut().unwrap().elite = true;
    }

    /// Place a collectible for deterministic QA scenes.
    #[doc(hidden)]
    pub fn spawn_item_drop(&mut self, kind: ItemKind, at: (f32, f32)) {
        self.drop_item(kind, at);
    }

    pub fn items(&self) -> impl Iterator<Item = ItemKind> + '_ {
        self.items.iter().copied()
    }

    pub fn item_drops(&self) -> &[ItemDrop] {
        &self.item_drops
    }

    pub fn pivot_kind(&self) -> PivotKind {
        self.pivot_kind
    }

    /// Selected at the loadout screen; preserved through waves in this run.
    pub fn set_pivot_kind(&mut self, kind: PivotKind) {
        self.pivot_kind = kind;
    }

    pub(super) fn projectile_size(&self) -> f32 {
        if self.items.contains(&ItemKind::JumboFrame) { 1.25 } else { 1.0 }
    }

    pub(super) fn projectile_speed(&self) -> f32 {
        if self.items.contains(&ItemKind::JumboFrame) { 0.5 } else { 1.0 }
    }

    /// A random item the player has neither equipped nor left lying around.
    pub(super) fn drop_elite_item(&mut self, at: (f32, f32)) {
        let open: Vec<_> = ItemKind::ALL
            .into_iter()
            .filter(|k| !self.items.contains(k) && !self.item_drops.iter().any(|d| d.kind == *k))
            .collect();
        if !open.is_empty() {
            let kind = open[self.loot_rng.below(open.len() as u32) as usize];
            self.drop_item(kind, at);
        }
    }

    pub(super) fn drop_item(&mut self, kind: ItemKind, at: (f32, f32)) {
        if self.items.contains(&kind) || self.item_drops.iter().any(|d| d.kind == kind) {
            return;
        }
        self.item_drops.push(ItemDrop { kind, x: at.0.clamp(2.0, WORLD_W - 2.0), y: at.1.clamp(1.0, WORLD_H - 1.0) });
    }

    pub(super) fn update_items(&mut self) {
        let at = (self.player.x, self.player.y + 0.5);
        let mut drops = std::mem::take(&mut self.item_drops);
        drops.retain(|drop| {
            if visual_distance(at, (drop.x, drop.y)) > XP_PICKUP_RADIUS {
                return true;
            }
            self.collect_item(drop.kind);
            false
        });
        self.item_drops = drops;
    }

    pub(super) fn collect_item(&mut self, kind: ItemKind) {
        if !self.items.insert(kind) {
            return;
        }
        if kind == ItemKind::JumboFrame {
            // Apply to projectiles already in flight as well as new ones.
            for s in self.shots.iter_mut().chain(&mut self.new_shots) {
                s.radius *= 1.25;
                s.vx *= 0.5;
                s.vy *= 0.5;
            }
            for o in &mut self.orbitals {
                o.size *= 1.25;
                o.speed *= 0.5;
                o.remaining *= 2.0;
            }
        }
        self.events.push(Event::ItemCollected(kind));
    }

    pub(super) fn launch_pivot(&mut self) {
        let mirrored = self.items.contains(&ItemKind::PortMirroring);
        for side in 0..if mirrored { 2 } else { 1 } {
            if mirrored && self.mirror_rng.chance(0.25) {
                continue;
            }
            let dir = self.player.aim;
            let dir = if side == 0 { dir } else { (-dir.0, -dir.1) };
            self.launch(ShotKind::Pivot, dir, self.stats.pivot_direct, 0, false);
        }
    }

    pub(super) fn missile_target(&self, at: (f32, f32), hemisphere: Option<(f32, f32)>) -> Option<u32> {
        self.enemies
            .iter()
            .filter(|e| e.alive && e.visible_in(self.camera()))
            .filter(|e| {
                hemisphere.is_none_or(|dir| {
                    let c = e.center();
                    (c.0 - self.player.x) * dir.0 + (c.1 - self.player.y - 0.5) * ASPECT * dir.1 >= 0.0
                })
            })
            .min_by(|a, b| {
                let high = |e: &Enemy| e.hp >= 6.0;
                high(b)
                    .cmp(&high(a))
                    .then_with(|| visual_distance(at, a.center()).total_cmp(&visual_distance(at, b.center())))
                    .then(a.id.cmp(&b.id))
            })
            .map(|e| e.id)
    }

    pub(super) fn launch_missile(&mut self) {
        let mirrored = self.items.contains(&ItemKind::PortMirroring);
        for side in 0..if mirrored { 2 } else { 1 } {
            if self.shots.len() >= MAX_SHOTS {
                break;
            }
            if mirrored && self.mirror_rng.chance(0.25) {
                continue;
            }
            let aim = self.player.aim;
            let dir = if side == 0 { aim } else { (-aim.0, -aim.1) };
            let at = (self.player.x, self.player.y + 0.5);
            let target = self.missile_target(at, if side == 1 { Some(dir) } else { None });
            let heading = target
                .and_then(|id| self.alive_index(id))
                .and_then(|i| direction(at, self.enemies[i].center()))
                .unwrap_or(dir);
            let (vx, vy) = velocity(heading, 42.0 * self.projectile_speed());
            self.shots.push(Shot {
                radius: 0.45 * self.projectile_size(),
                target,
                kind: ShotKind::Missile,
                x: at.0,
                y: at.1,
                vx,
                vy,
                damage: self.stats.pivot_direct * 2.0,
                pierce: 0,
                hit: Vec::new(),
                travelled: 0.0,
                range: 100.0,
                guided: false,
            });
        }
    }

    pub(super) fn steer_missile(&self, shot: &mut Shot) {
        let at = (shot.x, shot.y);
        if shot.target.and_then(|id| self.alive_index(id)).is_none() {
            shot.target = self.missile_target(at, None);
        }
        let Some(index) = shot.target.and_then(|id| self.alive_index(id)) else { return };
        let Some(dir) = direction(at, self.enemies[index].center()) else { return };
        let current = unit((shot.vx, shot.vy * ASPECT)).unwrap_or(dir);
        let angle = (current.0 * dir.1 - current.1 * dir.0).atan2(current.0 * dir.0 + current.1 * dir.1);
        let heading = rotated(current, angle.clamp(-5.0 * STEP, 5.0 * STEP));
        (shot.vx, shot.vy) = velocity(heading, shot.vx.hypot(shot.vy * ASPECT));
    }

    pub(super) fn explode(&mut self, at: (f32, f32), damage: f32) {
        let radius = 8.0 + f32::from(self.stats.pivot_jumps.saturating_sub(3));
        self.effect(EffectKind::Pulse, at, at, 0.45);
        if let Some(effect) = self.effects.last_mut() {
            effect.radius = radius;
        }
        // Linked boss parts share one HP pool: a blast damages it only once.
        let mut pools = BTreeSet::new();
        let targets: Vec<_> = self
            .enemies
            .iter()
            .enumerate()
            .filter(|(_, e)| e.alive)
            .filter(|(_, e)| visual_distance(at, (at.0.clamp(e.x, e.x + e.w), at.1.clamp(e.y, e.y + e.h))) <= radius)
            .filter(|(_, e)| e.pool.is_none_or(|p| pools.insert(p)))
            .map(|(i, _)| i)
            .collect();
        for i in targets {
            self.damage(i, damage);
        }
    }

    pub(super) fn reflect(&mut self, at: (f32, f32), dir: (f32, f32), damage: f32) {
        self.effect(EffectKind::Absorb, at, at, 0.2);
        if self.shots.len() >= MAX_SHOTS {
            return;
        }
        let dir = unit((dir.0, dir.1 * ASPECT)).unwrap_or(self.player.aim);
        let (vx, vy) = velocity(dir, 60.0 * self.projectile_speed());
        self.shots.push(Shot {
            radius: 0.15 * self.projectile_size(),
            target: None,
            kind: ShotKind::Reflected,
            x: at.0,
            y: at.1,
            vx,
            vy,
            damage,
            pierce: 0,
            hit: Vec::new(),
            travelled: 0.0,
            range: 45.0,
            guided: false,
        });
    }
}
