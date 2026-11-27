//! Elite cargo, reflected packets, optional specials (Missile, Cloak,
//! Stinger and its poison, Rate Limit), blasts and Train's tail.
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
    /// A special this rig cannot use, or has not unlocked, falls back to its own.
    pub fn set_pivot_kind(&mut self, kind: PivotKind) {
        self.pivot_kind = if kind.allowed(self.rig, |u| self.unlocked.contains(&u)) { kind } else { PivotKind::Chain };
    }

    /// What X does in this run, for the HUD and control hints.
    pub fn special_name(&self) -> &'static str {
        match self.pivot_kind {
            PivotKind::Chain => self.rig.special_name(),
            PivotKind::Missile => "Missile",
            kind => kind.name(),
        }
    }

    /// Train's cars follow the one ahead like a snake, one spacing apart.
    pub(super) fn update_tail(&mut self) {
        let p = &mut self.player;
        let mut leader = (p.x, p.y + 0.5);
        while p.tail.len() < usize::from(self.stats.tail) {
            p.tail.push(p.tail.last().copied().unwrap_or(leader));
        }
        for car in &mut p.tail {
            let gap = visual_distance(*car, leader);
            if gap > TAIL_SPACING {
                let pull = (gap - TAIL_SPACING) / gap;
                car.0 += (leader.0 - car.0) * pull;
                car.1 += (leader.1 - car.1) * pull;
            }
            leader = *car;
        }
    }

    pub(super) fn launch_stinger(&mut self) {
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
            let (vx, vy) = velocity(dir, STINGER_SPEED * self.projectile_speed());
            self.shots.push(Shot {
                radius: STINGER_SHOT_RADIUS * self.projectile_size(),
                target: None,
                kind: ShotKind::Stinger,
                x: self.player.x,
                y: self.player.y + 0.5,
                vx,
                vy,
                damage: 0.0,
                pierce: 0,
                hit: Vec::new(),
                travelled: 0.0,
                range: STINGER_RANGE,
                guided: false,
            });
        }
    }

    /// Add a poison stack to every enemy in the cloud; stacks refresh the timer.
    pub(super) fn poison_cloud(&mut self, at: (f32, f32)) {
        self.effect(EffectKind::Poison, at, at, 0.6);
        if let Some(effect) = self.effects.last_mut() {
            effect.radius = STINGER_RADIUS;
        }
        // Linked boss parts share one HP pool: poison only one of them.
        let mut pools = BTreeSet::new();
        for e in &mut self.enemies {
            let near = visual_distance(at, (at.0.clamp(e.x, e.x + e.w), at.1.clamp(e.y, e.y + e.h))) <= STINGER_RADIUS;
            if e.alive && near && e.pool.is_none_or(|p| pools.insert(p)) {
                e.poison = (e.poison + 1).min(POISON_MAX_STACKS);
                e.poison_time = POISON_DURATION;
            }
        }
    }

    /// Seconds of Rate Limit left; hostile packets and ordinary ships crawl.
    pub fn rate_limited(&self) -> f32 {
        self.throttle
    }

    /// Seconds one Rate Limit lasts with the current build.
    pub fn rate_limit_time(&self) -> f32 {
        rate_limit_time(&self.stats)
    }

    /// Seconds of Relay's Overclock left; weapons cool down twice as fast.
    pub fn overclocked(&self) -> f32 {
        self.overclock
    }

    /// Kernel's Purge: erase nearby hostile packets and blast the devices around the rig.
    pub(super) fn purge(&mut self) {
        let at = (self.player.x, self.player.y + 0.5);
        let radius = self.stats.purge_radius;
        self.hostiles.retain(|h| visual_distance(at, (h.x, h.y)) > radius);
        self.explode(at, self.stats.pivot_direct, radius);
    }

    pub(super) fn slow(&self) -> f32 {
        if self.throttle > 0.0 { RATE_LIMIT_SLOW } else { 1.0 }
    }

    pub(super) fn rate_limit(&mut self) {
        self.throttle = rate_limit_time(&self.stats);
        let at = (self.player.x, self.player.y + 0.5);
        self.effect(EffectKind::Pulse, at, at, 0.6);
        if let Some(effect) = self.effects.last_mut() {
            effect.radius = 30.0;
        }
    }

    /// The blast radius of a shot that explodes, in visual units.
    pub(super) fn blast(&self, kind: ShotKind) -> Option<f32> {
        match kind {
            ShotKind::Missile => Some(8.0 + f32::from(self.stats.pivot_jumps.saturating_sub(3))),
            ShotKind::Mine => {
                let bomb = self.weapons.iter().find(|w| w.kind == WeaponKind::Mine);
                Some(bomb.map_or(6.0, |w| w.spec(self.rig, &self.stats).radius))
            }
            _ => None,
        }
    }

    /// Damage per poison stack per second.
    pub fn poison_per_stack(&self) -> f32 {
        poison_per_stack(&self.stats)
    }

    pub(super) fn update_poison(&mut self) {
        let per_stack = self.poison_per_stack();
        for i in 0..self.enemies.len() {
            let e = &mut self.enemies[i];
            if !e.alive || e.poison == 0 {
                continue;
            }
            e.poison_time -= STEP;
            e.poison_tick += STEP;
            let amount = if e.poison_tick >= POISON_TICK {
                e.poison_tick -= POISON_TICK;
                f32::from(e.poison) * per_stack * POISON_TICK
            } else {
                0.0
            };
            if e.poison_time <= 0.0 {
                (e.poison, e.poison_tick) = (0, 0.0);
            }
            if amount > 0.0 {
                self.damage(i, amount);
            }
        }
    }

    pub(super) fn projectile_size(&self) -> f32 {
        if self.items.contains(&ItemKind::JumboFrame) { 1.25 } else { 1.0 }
    }

    pub(super) fn projectile_speed(&self) -> f32 {
        if self.items.contains(&ItemKind::JumboFrame) { 0.5 } else { 1.0 }
    }

    /// A random unlocked item the player has neither equipped nor left lying around.
    pub(super) fn drop_elite_item(&mut self, at: (f32, f32)) {
        let open: Vec<_> = ItemKind::ALL
            .into_iter()
            .filter(|k| {
                !self.items.contains(k)
                    && !self.item_drops.iter().any(|d| d.kind == *k)
                    && k.unlock().is_none_or(|u| self.unlocked.contains(&u))
            })
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

    /// The nearest visible enemy with at least 6 HP, else the nearest one,
    /// leaving out any in `skip` (what a Worm has already been through).
    pub(super) fn missile_target(&self, at: (f32, f32), hemisphere: Option<(f32, f32)>, skip: &[u32]) -> Option<u32> {
        self.enemies
            .iter()
            .filter(|e| e.alive && e.visible_in(self.camera()) && !skip.contains(&e.id))
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
            let target = self.missile_target(at, if side == 1 { Some(dir) } else { None }, &[]);
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
            shot.target = self.missile_target(at, None, &shot.hit);
        }
        let Some(index) = shot.target.and_then(|id| self.alive_index(id)) else { return };
        let Some(dir) = direction(at, self.enemies[index].center()) else { return };
        let current = unit((shot.vx, shot.vy * ASPECT)).unwrap_or(dir);
        let angle = (current.0 * dir.1 - current.1 * dir.0).atan2(current.0 * dir.0 + current.1 * dir.1);
        // A Worm turns tightly, or it would orbit a device beside it forever.
        let turn = if shot.kind == ShotKind::Worm { 14.0 } else { 5.0 } * STEP;
        let heading = rotated(current, angle.clamp(-turn, turn));
        (shot.vx, shot.vy) = velocity(heading, shot.vx.hypot(shot.vy * ASPECT));
    }

    pub(super) fn explode(&mut self, at: (f32, f32), damage: f32, radius: f32) {
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
