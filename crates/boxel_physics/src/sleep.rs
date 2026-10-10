//! Sleeping bodies (phase P4 of ADR 0016): the store that keeps them out of the per-step work,
//! the wake rules and the rule that puts bodies to sleep.
//!
//! A sleeping body is not stepped, not gathered into the step's [`BodyIndex`] list and not put
//! in its grid. The [`Sleepers`] store keeps a copy of each one in its own grid, rebuilt only
//! when enough bodies fell asleep or woke up since the last rebuild: bodies that fell asleep
//! since then wait in a short `pending` list (which joins every step's list), and woken ones are
//! marked dead (dead pending ones are dropped in one pass when the list next changes). Each
//! step the awake bodies that can disturb a sleeper (the "active" ones: awake dynamic bodies,
//! moving kinematic ones, teleported, nudged or changed ones) look up the sleepers their bounds
//! touch and pull a copy into the step's list, so pushes, events and the wake rules see them;
//! the rest of the store is never touched. Chunk loads find their sleepers in a chunk index
//! built with the grid.
//!
//! The marker [`Sleeping`] is the truth: its hooks log every change (by physics or by the game,
//! despawns included) and the store follows the log at the start of the wake and broadphase
//! systems.

use crate::components::{
    BodyPosition, CharacterController, Collider, Contacts, Crouch, Disturbed, Frozen, MoveIntent,
    PhysicsSettings, PhysicsStats, PhysicsSync, Projectile, ReportContacts, RigidBody, SleepLog,
    Sleeping, Vacated, Velocity,
};
use crate::contacts::{BodyIndex, IndexedBody};
use crate::core::{BlockBounds, BodySettings, Grid, Shape};
use boxel_app::FixedTime;
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_voxel::{CHUNK_SHIFT, ChunkPos, WorldPos};
use boxel_world::{BlockChanged, ChunkDataEvent};

/// Rebuild the store's grid when the bodies that fell asleep or woke up since the last rebuild
/// are more than this many...
const MIN_REBUILD: usize = 256;
/// ...and more than the live sleepers divided by this.
const REBUILD_FRACTION: usize = 8;
/// A grid sleeper whose bounds (plus one block) reach more chunks than this is not in the
/// chunk index: chunk events test it on its own.
const MAX_INDEXED_CHUNKS: u128 = 64;

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
    /// The chunks each grid body's bounds plus one block reach, as `(chunk, body)`, sorted:
    /// chunk events find the bodies they touch without a scan.
    by_chunk: Vec<(ChunkPos, u32)>,
    /// Grid bodies that reach too many chunks for `by_chunk`.
    wide: Vec<u32>,
    /// Fell asleep since the last rebuild: sorted by entity bits, not in the grid.
    pending: Vec<IndexedBody>,
    /// Per pending body: woke up since (dropped when the list next changes).
    pending_dead: Vec<bool>,
    pending_dead_count: usize,
    /// The next `pending`, while a merge builds it (kept for its buffer).
    merged: Vec<IndexedBody>,
    /// The bodies [`sync_sleepers`] adds (kept for its buffer).
    fresh: Vec<IndexedBody>,
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
        self.bodies.len() - self.dead_count + self.pending.len() - self.pending_dead_count
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
            .filter(|&i| !self.pending_dead[i])
            .map(Slot::Pending)
    }

    /// Forgets `entity` (it woke up or is gone). Constant time apart from the lookup: the
    /// entry is only marked dead.
    pub(crate) fn remove(&mut self, entity: Entity) {
        match self.find(entity) {
            Some(Slot::Grid(i)) => {
                self.dead[i] = true;
                self.dead_count += 1;
            }
            Some(Slot::Pending(i)) => {
                self.pending_dead[i] = true;
                self.pending_dead_count += 1;
            }
            None => {}
        }
    }

    /// Adds bodies that fell asleep, sorted by entity and not in the store (removed first), to
    /// the pending list: one merge, which also drops the dead pending entries.
    fn add_pending(&mut self, fresh: &[IndexedBody]) {
        if fresh.is_empty() && self.pending_dead_count == 0 {
            return;
        }
        for body in fresh {
            self.widen_chunks(body);
        }
        let mut merged = std::mem::take(&mut self.merged);
        merged.clear();
        merged.reserve(self.pending.len() + fresh.len());
        let mut fresh = fresh.iter().copied().peekable();
        for (body, &dead) in self.pending.iter().zip(&self.pending_dead) {
            if dead {
                continue;
            }
            while let Some(next) = fresh.next_if(|f| f.entity.to_bits() < body.entity.to_bits()) {
                merged.push(next);
            }
            merged.push(*body);
        }
        merged.extend(fresh);
        self.merged = std::mem::replace(&mut self.pending, merged);
        self.pending_dead.clear();
        self.pending_dead.resize(self.pending.len(), false);
        self.pending_dead_count = 0;
    }

    /// The live pending sleepers, with their index in `pending`.
    fn live_pending(&self) -> impl Iterator<Item = (usize, &IndexedBody)> + '_ {
        self.pending
            .iter()
            .zip(&self.pending_dead)
            .enumerate()
            .filter(|(_, (_, dead))| !**dead)
            .map(|(i, (body, _))| (i, body))
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
        let changed = self.pending.len() - self.pending_dead_count + self.dead_count;
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
        let mut pending = self
            .pending
            .drain(..)
            .zip(self.pending_dead.drain(..))
            .filter(|(_, dead)| !dead)
            .map(|(body, _)| body)
            .peekable();
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
        self.pending_dead_count = 0;
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
        self.by_chunk.clear();
        self.wide.clear();
        let mut chunks = None;
        for (i, body) in self.bodies.iter().enumerate() {
            let (lo, hi) = chunk_range(body);
            chunks = Some(match chunks {
                Some((min, max)) => (I64Vec3::min(min, lo), I64Vec3::max(max, hi)),
                None => (lo, hi),
            });
            if chunk_count(lo, hi) > MAX_INDEXED_CHUNKS {
                self.wide.push(i as u32);
                continue;
            }
            for x in lo.x..=hi.x {
                for y in lo.y..=hi.y {
                    for z in lo.z..=hi.z {
                        self.by_chunk.push((ChunkPos::new(x, y, z), i as u32));
                    }
                }
            }
        }
        self.chunks = chunks;
        self.by_chunk.sort_unstable();
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
        for (i, body) in self.live_pending() {
            if body.bounds.overlaps(&bounds) {
                visit(Slot::Pending(i));
            }
        }
    }

    /// Where a live sleeper is, as the store keeps it.
    pub(crate) fn position_of(&self, entity: Entity) -> Option<WorldPos> {
        Some(match self.find(entity)? {
            Slot::Grid(i) => self.bodies[i].pos,
            Slot::Pending(i) => self.pending[i].pos,
        })
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
        self.live_pending()
            .map(|(_, body)| body)
            .filter(|b| b.collides)
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
            .chain(self.live_pending().map(|(_, body)| body))
    }

    /// The live sleepers whose bounds plus one block reach one of the `chunks` (sorted), as
    /// entities, into `out` (appended, maybe twice). Grid sleepers are looked up per chunk in
    /// the chunk index; only the pending ones (and the very wide ones) are tested one by one.
    fn in_chunks(&self, chunks: &[ChunkPos], out: &mut Vec<Entity>) {
        let Some((min, max)) = self.chunks else {
            return;
        };
        let inside = |c: &ChunkPos, lo: I64Vec3, hi: I64Vec3| {
            let c = I64Vec3::new(c.x, c.y, c.z);
            c.cmpge(lo).all() && c.cmple(hi).all()
        };
        let mut any = false;
        for chunk in chunks.iter().filter(|c| inside(c, min, max)) {
            any = true;
            let start = self.by_chunk.partition_point(|(c, _)| c < chunk);
            for &(_, i) in self.by_chunk[start..]
                .iter()
                .take_while(|(c, _)| c == chunk)
            {
                if !self.dead[i as usize] {
                    out.push(self.bodies[i as usize].entity);
                }
            }
        }
        if !any {
            return;
        }
        for &i in &self.wide {
            let body = &self.bodies[i as usize];
            let (lo, hi) = chunk_range(body);
            if !self.dead[i as usize] && chunks.iter().any(|c| inside(c, lo, hi)) {
                out.push(body.entity);
            }
        }
        for (_, body) in self.live_pending() {
            let (lo, hi) = chunk_range(body);
            let touches = if chunk_count(lo, hi) > MAX_INDEXED_CHUNKS {
                chunks.iter().any(|c| inside(c, lo, hi))
            } else {
                (lo.x..=hi.x).any(|x| {
                    (lo.y..=hi.y).any(|y| {
                        (lo.z..=hi.z).any(|z| chunks.binary_search(&ChunkPos::new(x, y, z)).is_ok())
                    })
                })
            };
            if touches {
                out.push(body.entity);
            }
        }
    }
}

/// How many chunks `lo..=hi` holds (saturating).
fn chunk_count(lo: I64Vec3, hi: I64Vec3) -> u128 {
    let span = |a: i64, b: i64| u128::try_from(i128::from(b) - i128::from(a) + 1).unwrap_or(0);
    span(lo.x, hi.x)
        .saturating_mul(span(lo.y, hi.y))
        .saturating_mul(span(lo.z, hi.z))
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
    for &entity in &log.0 {
        sleepers.remove(entity);
    }
    // Sorted like the log.
    let mut fresh = std::mem::take(&mut sleepers.fresh);
    fresh.clear();
    fresh.extend(log.0.drain(..).filter_map(|entity| {
        let (kind, collider, position, velocity, contacts, settings, report, projectile) =
            data.get(entity).ok()?;
        Some(sleeper(
            entity,
            (
                kind, collider, position, velocity, contacts, settings, report, projectile,
            ),
        ))
    }));
    sleepers.add_pending(&fresh);
    sleepers.fresh = fresh;
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

/// What [`wake_bodies`] reads of a sleeping body whose components changed.
type ChangedSleeper<'a> = (
    Entity,
    Ref<'a, Sleeping>,
    Ref<'a, Velocity>,
    Ref<'a, RigidBody>,
    Option<Ref<'a, Collider>>,
    Ref<'a, BodySettings>,
);

/// The reused buffers of [`wake_bodies`].
#[derive(Default)]
pub(crate) struct WakeScratch {
    wake: Vec<Entity>,
    ids: Vec<u32>,
    loaded: Vec<ChunkPos>,
}

/// [`crate::PhysicsSystems::Wake`]: wakes the sleeping bodies something disturbed since the
/// latest step (see [`Sleeping`]): a velocity, or a body, collider or settings change made
/// after the body fell asleep, a removed collider or [`ReportContacts`], a vacated place
/// (teleports and nudges in `Prepare`, collider changes and despawns through [`Collider`]'s
/// hook), block changes and chunk loads, a change of the world-wide settings.
#[expect(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "a system's parameters"
)]
pub(crate) fn wake_bodies(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    mut last: Local<Option<(DVec3, u32, u64, u64)>>,
    mut scratch: Local<WakeScratch>,
    mut index: ResMut<BodyIndex>,
    mut log: ResMut<SleepLog>,
    mut vacated: ResMut<Vacated>,
    mut disturbed: ResMut<Disturbed>,
    data: Query<SleeperData, With<Sleeping>>,
    changed: Query<
        ChangedSleeper,
        Or<(
            Changed<Velocity>,
            Changed<RigidBody>,
            Changed<Collider>,
            Changed<BodySettings>,
        )>,
    >,
    mut blocks: MessageReader<BlockChanged>,
    mut chunks: MessageReader<ChunkDataEvent>,
    mut stats: ResMut<PhysicsStats>,
) {
    let index = &mut *index;
    let WakeScratch { wake, ids, loaded } = &mut *scratch;
    wake.clear();
    sync_sleepers(&mut index.sleepers, &mut log, &data);
    let sleepers = &index.sleepers;
    let now = motion_settings(&settings);
    if last.is_some_and(|l| l != now) || settings.sleep_steps == 0 {
        wake.extend(sleepers.all().map(|b| b.entity));
    }
    *last = Some(now);
    for (entity, asleep, velocity, kind, collider, body) in &changed {
        // Changes made with the marker (a body spawned asleep) or before it (the store copy is
        // taken after them) do not count. Falling asleep zeroes the velocity: any other value
        // is new.
        let after = asleep.added();
        if (velocity.is_changed() && velocity.0 != DVec3::ZERO)
            || kind.is_changed_after(after)
            || collider.is_some_and(|c| c.is_changed_after(after))
            || body.is_changed_after(after)
        {
            wake.push(entity);
        }
    }
    wake.extend(disturbed.0.drain(..).filter(|&e| data.contains(e)));
    for (pos, bx) in vacated.0.drain(..) {
        let pos = pos.normalize();
        sleepers.near(grow(BlockBounds::of_box(pos, bx), 1), ids, wake);
    }
    for change in blocks.read() {
        sleepers.near(grow(BlockBounds::of_block(change.pos), 1), ids, wake);
    }
    // Only loads: a sleeping body needs no blocks, so an unload leaves it asleep (it would
    // only freeze), and the chunk's blocks are read again when it loads.
    loaded.clear();
    loaded.extend(chunks.read().filter_map(|event| match event {
        ChunkDataEvent::Loaded { key, .. } => Some(key.pos),
        ChunkDataEvent::Unloaded { .. } => None,
    }));
    if !loaded.is_empty() {
        loaded.sort_unstable();
        loaded.dedup();
        sleepers.in_chunks(loaded, wake);
    }
    wake.sort_unstable_by_key(|e| e.to_bits());
    wake.dedup();
    stats.woken = wake.len();
    for &entity in wake.iter() {
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
            Option<&Crouch>,
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
    for (entity, kind, velocity, contacts, mut sync, frozen, intent, crouch) in &mut bodies {
        if *kind != RigidBody::Dynamic {
            continue;
        }
        // A character that wants to walk is not resting, even against a wall; nor is one
        // that stays crouched only for want of room to stand up (it tries every step).
        let walking = intent.is_some_and(|i| i.wish != Vec2::ZERO);
        let rising = crouch.is_some_and(Crouch::is_crouching) && !intent.is_some_and(|i| i.sneak);
        let still = !frozen
            && !walking
            && !rising
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

/// [`crate::PhysicsSystems::Prepare`]: a sleeping character whose [`MoveIntent`] changed so that
/// its pose must change (it sneaks standing, or stops sneaking crouched; ADR 0026) wakes up in
/// this step, so it crouches or stands up without moving.
pub(crate) fn wake_to_pose(
    mut disturbed: ResMut<Disturbed>,
    characters: Query<
        (Entity, &CharacterController, &MoveIntent, &Crouch),
        (With<Sleeping>, Changed<MoveIntent>),
    >,
) {
    for (entity, controller, intent, crouch) in &characters {
        if (intent.sneak && !controller.flying) != crouch.is_crouching() {
            disturbed.0.push(entity);
        }
    }
}

/// Puts a body to sleep: the [`Sleeping`] marker, whose hook sets the velocity to 0 and the
/// `Transform` on the position when the command applies.
pub(crate) fn put_to_sleep(commands: &mut Commands, entity: Entity) {
    // `try_`: a command of another system may despawn it at the same sync point.
    commands.entity(entity).try_insert(Sleeping);
}
