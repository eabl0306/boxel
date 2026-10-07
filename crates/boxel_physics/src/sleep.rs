//! Sleeping bodies (phase P4 of ADR 0016): the store that keeps them out of the per-step work,
//! the wake rules and the rule that puts bodies to sleep.
//!
//! A sleeping body is not stepped, not gathered into the step's [`BodyIndex`] list and not put
//! in its grid. The [`Sleepers`] store keeps a copy of each one in its own grid, rebuilt only
//! when enough bodies fell asleep or woke up since the last rebuild: bodies that fell asleep
//! since then wait in a short `pending` list (which joins every step's list), and woken ones are
//! marked dead. Each step the awake bodies that can disturb a sleeper (the "active" ones: awake
//! dynamic bodies, moving kinematic ones, teleported or changed ones) look up the sleepers their
//! bounds touch and pull a copy into the step's list, so pushes, events and the wake rules see
//! them; the rest of the store is never touched.
//!
//! The marker [`Sleeping`] is the truth: its hooks log every change (by physics or by the game,
//! despawns included) and the store follows the log at the start of the wake and broadphase
//! systems.

use crate::components::{
    BodyPosition, Collider, Contacts, Frozen, MoveIntent, PhysicsSettings, PhysicsStats,
    PhysicsSync, Projectile, ReportContacts, RigidBody, SleepLog, Sleeping, Vacated, Velocity,
};
use crate::contacts::{BodyIndex, IndexedBody};
use crate::core::{BlockBounds, BodySettings, Grid, Shape};
use boxel_app::FixedTime;
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_voxel::{CHUNK_SHIFT, ChunkPos};
use boxel_world::{BlockChanged, ChunkDataEvent};

/// Rebuild the store's grid when the bodies that fell asleep or woke up since the last rebuild
/// are more than this many...
const MIN_REBUILD: usize = 256;
/// ...and more than the live sleepers divided by this.
const REBUILD_FRACTION: usize = 8;

/// The sleeping bodies (see the module docs).
#[derive(Default)]
pub(crate) struct Sleepers {
    /// Sorted by entity bits; the grid holds them all, the dead ones too.
    bodies: Vec<IndexedBody>,
    /// Per body: woke up (or despawned) since the last rebuild.
    dead: Vec<bool>,
    /// Per body: the step that last pulled it into the step's list.
    stamp: Vec<u32>,
    grid: Grid,
    dead_count: usize,
    /// Fell asleep since the last rebuild: sorted by entity bits, not in the grid.
    pending: Vec<IndexedBody>,
    /// The broadphase step, for the stamps (starts at 1: a stamp of 0 is never current).
    step: u32,
    /// Chunks the live sleepers' bounds (plus one block) reach, `min..=max` (conservative: dead
    /// bodies may keep it wider until the next rebuild).
    chunks: Option<(I64Vec3, I64Vec3)>,
}

/// Where a sleeper is in the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Grid(usize),
    Pending(usize),
}

impl Sleepers {
    /// Sleeping bodies (alive).
    pub(crate) fn len(&self) -> usize {
        self.bodies.len() - self.dead_count + self.pending.len()
    }

    fn find(&self, entity: Entity) -> Option<Slot> {
        let key = entity.to_bits();
        if let Ok(i) = self
            .bodies
            .binary_search_by_key(&key, |b| b.entity.to_bits())
            && !self.dead[i]
        {
            return Some(Slot::Grid(i));
        }
        self.pending
            .binary_search_by_key(&key, |b| b.entity.to_bits())
            .ok()
            .map(Slot::Pending)
    }

    /// Forgets `entity` (it woke up or is gone).
    pub(crate) fn remove(&mut self, entity: Entity) {
        match self.find(entity) {
            Some(Slot::Grid(i)) => {
                self.dead[i] = true;
                self.dead_count += 1;
            }
            Some(Slot::Pending(i)) => {
                self.pending.remove(i);
            }
            None => {}
        }
    }

    /// Adds a body that fell asleep (replacing what the store had for it).
    fn insert(&mut self, body: IndexedBody) {
        self.remove(body.entity);
        self.widen_chunks(&body);
        let key = body.entity.to_bits();
        let at = self.pending.partition_point(|b| b.entity.to_bits() < key);
        self.pending.insert(at, body);
    }

    fn widen_chunks(&mut self, body: &IndexedBody) {
        let (lo, hi) = chunk_range(body);
        self.chunks = Some(match self.chunks {
            Some((min, max)) => (min.min(lo), max.max(hi)),
            None => (lo, hi),
        });
    }

    /// The step's stamp moves on; the grid is rebuilt (pending bodies join it, dead ones go)
    /// when enough changed since the last rebuild, or when the cell size changed.
    pub(crate) fn begin_step(&mut self, shift: u32) {
        self.step = self.step.wrapping_add(1).max(1);
        let changed = self.pending.len() + self.dead_count;
        let rebuild = changed > MIN_REBUILD.max(self.len() / REBUILD_FRACTION)
            || (self.grid.shift() != shift.min(crate::core::MAX_GRID_SHIFT)
                && !self.bodies.is_empty());
        if rebuild {
            self.rebuild(shift);
        }
    }

    fn rebuild(&mut self, shift: u32) {
        let mut bodies = Vec::with_capacity(self.len());
        // Both lists are sorted by entity and hold different entities: a merge keeps the order.
        let mut pending = std::mem::take(&mut self.pending).into_iter().peekable();
        for (body, dead) in self.bodies.drain(..).zip(self.dead.drain(..)) {
            if dead {
                continue;
            }
            while let Some(next) = pending.next_if(|p| p.entity.to_bits() < body.entity.to_bits()) {
                bodies.push(next);
            }
            bodies.push(body);
        }
        bodies.extend(pending);
        self.bodies = bodies;
        self.dead = vec![false; self.bodies.len()];
        self.stamp = vec![0; self.bodies.len()];
        self.dead_count = 0;
        self.chunks = None;
        self.grid.reset(shift);
        for body in &self.bodies {
            self.grid.insert(body.bounds, body.layers, false);
        }
        if !self.bodies.is_empty() {
            self.grid.build();
        }
        let ranges: Vec<_> = self.bodies.iter().map(chunk_range).collect();
        for (lo, hi) in ranges {
            self.chunks = Some(match self.chunks {
                Some((min, max)) => (min.min(lo), max.max(hi)),
                None => (lo, hi),
            });
        }
    }

    /// Calls `visit` with every live sleeper whose bounds overlap `bounds` (grid ones, then
    /// pending ones), with its slot.
    fn for_each_near(&self, bounds: BlockBounds, ids: &mut Vec<u32>, mut visit: impl FnMut(Slot)) {
        if !self.bodies.is_empty() {
            ids.clear();
            self.grid.query(bounds, ids);
            for &i in ids.iter() {
                if !self.dead[i as usize] {
                    visit(Slot::Grid(i as usize));
                }
            }
        }
        for (i, body) in self.pending.iter().enumerate() {
            if body.bounds.overlaps(&bounds) {
                visit(Slot::Pending(i));
            }
        }
    }

    /// The live sleepers whose bounds overlap `bounds`, as entities, into `out` (appended).
    pub(crate) fn near(&self, bounds: BlockBounds, ids: &mut Vec<u32>, out: &mut Vec<Entity>) {
        self.for_each_near(bounds, ids, |slot| {
            out.push(match slot {
                Slot::Grid(i) => self.bodies[i].entity,
                Slot::Pending(i) => self.pending[i].entity,
            });
        });
    }

    /// Pulls into `list` a copy of each grid sleeper that collides with bodies and whose bounds
    /// overlap `bounds`, unless this step already pulled it. Pending sleepers are not pulled:
    /// every step's list holds them anyway.
    pub(crate) fn pull(
        &mut self,
        bounds: BlockBounds,
        ids: &mut Vec<u32>,
        list: &mut Vec<IndexedBody>,
    ) {
        if self.bodies.is_empty() {
            return;
        }
        ids.clear();
        self.grid.query(bounds, ids);
        for &i in ids.iter() {
            let i = i as usize;
            if !self.dead[i] && self.bodies[i].collides && self.stamp[i] != self.step {
                self.stamp[i] = self.step;
                list.push(self.bodies[i]);
            }
        }
    }

    /// The grid holds some sleepers (pulling can find something).
    pub(crate) fn has_grid_bodies(&self) -> bool {
        self.bodies.len() > self.dead_count
    }

    /// The pending sleepers that collide with bodies (they join every step's list).
    pub(crate) fn pending_colliders(&self) -> impl Iterator<Item = &IndexedBody> + '_ {
        self.pending.iter().filter(|b| b.collides)
    }

    /// The grid sleepers queries must see: alive, colliding with bodies and not pulled into the
    /// current step's list (that copy is the current one).
    pub(crate) fn queryable(&self, i: u32) -> bool {
        let i = i as usize;
        !self.dead[i] && self.bodies[i].collides && self.stamp[i] != self.step
    }

    /// The grid sleepers, by grid index (see [`Sleepers::queryable`]).
    pub(crate) fn grid_bodies(&self) -> &[IndexedBody] {
        &self.bodies
    }

    /// The grid of the grid sleepers.
    pub(crate) fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Every live sleeper.
    fn all(&self) -> impl Iterator<Item = &IndexedBody> + '_ {
        self.bodies
            .iter()
            .zip(&self.dead)
            .filter(|(_, dead)| !**dead)
            .map(|(b, _)| b)
            .chain(&self.pending)
    }

    /// The live sleepers whose bounds plus one block reach one of the `chunks` (sorted), as
    /// entities, into `out`.
    fn in_chunks(&self, chunks: &[ChunkPos], out: &mut Vec<Entity>) {
        let Some((min, max)) = self.chunks else {
            return;
        };
        let near = |c: &&ChunkPos| {
            let c = I64Vec3::new(c.x, c.y, c.z);
            c.cmpge(min).all() && c.cmple(max).all()
        };
        let chunks: Vec<ChunkPos> = chunks.iter().filter(near).copied().collect();
        if chunks.is_empty() {
            return;
        }
        for body in self.all() {
            let (lo, hi) = chunk_range(body);
            let touches = (lo.x..=hi.x).any(|x| {
                (lo.y..=hi.y).any(|y| {
                    (lo.z..=hi.z).any(|z| chunks.binary_search(&ChunkPos::new(x, y, z)).is_ok())
                })
            });
            if touches {
                out.push(body.entity);
            }
        }
    }
}

/// The chunks a body's bounds plus one block reach.
fn chunk_range(body: &IndexedBody) -> (I64Vec3, I64Vec3) {
    let bounds = grow(body.bounds, 1);
    let shift =
        |v: I64Vec3| I64Vec3::new(v.x >> CHUNK_SHIFT, v.y >> CHUNK_SHIFT, v.z >> CHUNK_SHIFT);
    (shift(bounds.min), shift(bounds.max))
}

/// `bounds` grown by `n` blocks on every side (saturating at the `i64` ends).
pub(crate) fn grow(bounds: BlockBounds, n: i64) -> BlockBounds {
    BlockBounds::new(
        bounds.min.saturating_sub(I64Vec3::splat(n)),
        bounds.max.saturating_add(I64Vec3::splat(n)),
    )
}

/// What the store reads of a sleeping body.
pub(crate) type SleeperData<'a> = (
    &'a RigidBody,
    Option<&'a Collider>,
    &'a BodyPosition,
    &'a Velocity,
    &'a Contacts,
    &'a BodySettings,
    Has<ReportContacts>,
    Has<Projectile>,
);

/// A sleeping body as the store keeps it.
fn sleeper(
    entity: Entity,
    data: (
        &RigidBody,
        Option<&Collider>,
        &BodyPosition,
        &Velocity,
        &Contacts,
        &BodySettings,
        bool,
        bool,
    ),
) -> IndexedBody {
    let (kind, collider, position, velocity, contacts, settings, report, projectile) = data;
    let (shape, offset, layers, sensor) = match collider {
        Some(c) => (c.shape, c.offset, c.layers, c.is_sensor()),
        None => (
            Shape::Aabb { half: DVec3::ZERO },
            DVec3::ZERO,
            Default::default(),
            true,
        ),
    };
    IndexedBody {
        entity,
        pos: position.0.normalize(),
        shape,
        offset,
        layers,
        sensor,
        report,
        kind: *kind,
        frozen: false,
        weight: settings.push_weight,
        step: DVec3::ZERO,
        vel: velocity.0,
        faces: contacts.faces,
        on_ground: contacts.on_ground,
        sleeping: true,
        active: false,
        collides: collider.is_some() && !projectile,
        dirty: false,
        moved: false,
        woken: false,
        pushed: DVec3::ZERO,
        bounds: BlockBounds::default(),
    }
    .with_end_bounds()
}

/// Makes the store follow the [`Sleeping`] markers that came or went since it last looked.
pub(crate) fn sync_sleepers(
    sleepers: &mut Sleepers,
    log: &mut SleepLog,
    data: &Query<SleeperData, With<Sleeping>>,
) {
    if log.0.is_empty() {
        return;
    }
    log.0.sort_unstable_by_key(|e| e.to_bits());
    log.0.dedup();
    for entity in log.0.drain(..) {
        sleepers.remove(entity);
        if let Ok((kind, collider, position, velocity, contacts, settings, report, projectile)) =
            data.get(entity)
        {
            sleepers.insert(sleeper(
                entity,
                (
                    kind, collider, position, velocity, contacts, settings, report, projectile,
                ),
            ));
        }
    }
}

/// `PhysicsSettings` fields that change how bodies move: a change wakes every sleeper.
fn motion_settings(s: &PhysicsSettings) -> (DVec3, u32, u64, u64) {
    (
        s.gravity,
        s.sleep_steps,
        s.max_speed.to_bits(),
        s.eps.to_bits(),
    )
}

/// [`crate::PhysicsSystems::Wake`]: wakes the sleeping bodies something disturbed since the
/// latest step (see [`Sleeping`]): a velocity, collider or settings change, a vacated place
/// (teleports in `Prepare`, collider changes and despawns through [`Collider`]'s hook), block
/// changes and chunk loads and unloads, a change of the world-wide settings.
pub(crate) fn wake_bodies(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    mut last: Local<Option<(DVec3, u32, u64, u64)>>,
    mut index: ResMut<BodyIndex>,
    mut log: ResMut<SleepLog>,
    mut vacated: ResMut<Vacated>,
    data: Query<SleeperData, With<Sleeping>>,
    changed: Query<
        (
            Entity,
            Ref<Velocity>,
            Option<Ref<Collider>>,
            Ref<BodySettings>,
        ),
        (
            With<Sleeping>,
            Or<(Changed<Velocity>, Changed<Collider>, Changed<BodySettings>)>,
        ),
    >,
    mut blocks: MessageReader<BlockChanged>,
    mut chunks: MessageReader<ChunkDataEvent>,
    mut stats: ResMut<PhysicsStats>,
) {
    let index = &mut *index;
    sync_sleepers(&mut index.sleepers, &mut log, &data);
    let sleepers = &index.sleepers;
    let mut wake = Vec::new();
    let now = motion_settings(&settings);
    if last.is_some_and(|l| l != now) || settings.sleep_steps == 0 {
        wake.extend(sleepers.all().map(|b| b.entity));
    }
    *last = Some(now);
    for (entity, velocity, collider, body) in &changed {
        if (velocity.is_changed() && velocity.0 != DVec3::ZERO)
            || collider.is_some_and(|c| c.is_changed())
            || body.is_changed()
        {
            wake.push(entity);
        }
    }
    let mut ids = Vec::new();
    for (pos, bx) in vacated.0.drain(..) {
        let pos = pos.normalize();
        sleepers.near(grow(BlockBounds::of_box(pos, bx), 1), &mut ids, &mut wake);
    }
    for change in blocks.read() {
        sleepers.near(
            grow(BlockBounds::of_block(change.pos), 1),
            &mut ids,
            &mut wake,
        );
    }
    let mut loaded: Vec<ChunkPos> = chunks.read().map(ChunkDataEvent::pos).collect();
    if !loaded.is_empty() {
        loaded.sort_unstable();
        loaded.dedup();
        sleepers.in_chunks(&loaded, &mut wake);
    }
    wake.sort_unstable_by_key(|e| e.to_bits());
    wake.dedup();
    stats.woken = wake.len();
    for entity in wake {
        index.sleepers.remove(entity);
        commands.entity(entity).try_remove::<Sleeping>();
    }
}

/// [`crate::PhysicsSystems::Sleep`]: counts the steps each awake dynamic body stays still and
/// puts it to sleep after `PhysicsSettings::sleep_steps` (see [`Sleeping`]).
pub(crate) fn sleep_bodies(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    fixed: Res<FixedTime>,
    mut bodies: Query<
        (
            Entity,
            &RigidBody,
            &Velocity,
            &Contacts,
            &mut PhysicsSync,
            Has<Frozen>,
            Option<&MoveIntent>,
        ),
        Without<Sleeping>,
    >,
    sleeping: Query<(), (With<RigidBody>, With<Sleeping>)>,
    mut stats: ResMut<PhysicsStats>,
) {
    stats.sleeping = sleeping.iter().len();
    let steps = settings.sleep_steps;
    if steps == 0 {
        return;
    }
    let speed = settings.sleep_speed;
    let distance = speed * fixed.timestep().as_secs_f64();
    let mut asleep = 0;
    for (entity, kind, velocity, contacts, mut sync, frozen, intent) in &mut bodies {
        if *kind != RigidBody::Dynamic {
            continue;
        }
        // A character that wants to walk is not resting, even against a wall.
        let walking = intent.is_some_and(|i| i.wish != Vec2::ZERO);
        let still = !frozen
            && !walking
            && contacts.on_ground
            && velocity.0.abs().max_element() <= speed
            && sync.net.abs().max_element() <= distance;
        if !still {
            if sync.still != 0 {
                sync.still = 0;
            }
            continue;
        }
        sync.still += 1;
        if sync.still < steps {
            continue;
        }
        sync.still = 0;
        put_to_sleep(&mut commands, entity);
        asleep += 1;
    }
    stats.sleeping += asleep;
}

/// Puts a body to sleep: the [`Sleeping`] marker, whose hook sets the velocity to 0 and the
/// `Transform` on the position when the command applies.
pub(crate) fn put_to_sleep(commands: &mut Commands, entity: Entity) {
    // `try_`: a command of another system may despawn it at the same sync point.
    commands.entity(entity).try_insert(Sleeping);
}
