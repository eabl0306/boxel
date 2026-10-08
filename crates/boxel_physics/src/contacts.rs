//! Collisions between bodies (phases P3 and P4 of ADR 0016): the obstacle index of static and
//! kinematic solids, the [`BodyIndex`] (the step's bodies in the broadphase grid, the store of
//! sleeping bodies, and the snapshot for queries), the solve that pushes solid bodies apart,
//! sensor overlaps and the collision events, and the body queries ([`BodyIndex::raycast`],
//! [`BodyIndex::overlap_box`], [`BodyIndex::block_is_free`]).
//!
//! The step's list holds the awake bodies with a collider, plus the sleeping ones that matter
//! this step: the ones that fell asleep since the sleeper store's last rebuild, and the ones the
//! active bodies reach (pulled from the store, see [`crate::sleep`]). Sleeping bodies are
//! inactive in the grid: two of them never form a pair.
//!
//! The solve, per fixed step, after the dynamic bodies moved against the voxels and the
//! obstacles:
//! - Solid pairs with a dynamic body are taken bottom-up (by the lower body's bottom, then by
//!   pair), for `PhysicsSettings::solver_passes` passes. Each pair that overlaps by more than
//!   the contact tolerance is pushed apart along the narrowphase's normal.
//! - A body that cannot be pushed that way (static, kinematic, frozen, or a dynamic body
//!   already touching something on the face it would be pushed toward) takes no share, and the
//!   other one takes the whole push: a crate on a crate on the ground rests without sinking.
//!   Two free dynamic bodies split the push by their `BodySettings::push_weight`.
//! - Pushes go through the voxel sweep (and the obstacles): walls always win. When the first
//!   body moves less than its share, the second one takes the rest.
//! - The velocity the two bodies approach each other with along the normal is removed, split
//!   by the same shares (an inelastic hit: a player walking into a crate of the same weight
//!   pushes it at half its speed).
//! - Each touching face of a dynamic body is set in its `Contacts::faces`; a body pushed up
//!   off another stands on it (`Contacts::on_ground`).
//! - A sleeping body that is pushed, or whose velocity changes, wakes up. So does one that
//!   touches a solid body that moved this step (the crate under it was pushed away).
//!
//! After the pushes, at the bodies' final places, the pairs that report join the step's contact
//! set: sensor pairs that overlap (by more than the tolerance; touching does not count; sensors
//! never push), and solid pairs that carry `ReportContacts` and touch or overlap (a body resting
//! on another or on a static platform touches it). Pairs of two inactive bodies (two sleepers, a
//! sleeper and a static body) are not tested: nothing moved, so they keep the state of the
//! previous step. The events compare the set with the previous step's.

use crate::components::{
    BodyPosition, Collider, CollisionEnded, CollisionStarted, Contacts, Frozen, PhysicsSettings,
    PhysicsStats, PhysicsSync, Projectile, ReportContacts, RigidBody, SleepLog, Sleeping, Velocity,
};
use crate::core::sweep::effective_eps as sweep_eps;
use crate::core::{
    BlockBounds, BodySettings, CollisionLayers, CollisionShape, CollisionTable, Contact, Grid,
    GridSlot, LocalBox, MAX_RAY_DISTANCE, Shape, SweepConfig, box_box_moving, move_box, shape_box,
    shape_contact,
};
use crate::plugin::PhysicsContent;
use crate::sleep::{SleeperData, Sleepers, sync_sleepers};
use crate::source::ChunkSource;
use crate::systems::{ensure_compute_pool, parallel_above};
use boxel_app::FixedTime;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_tasks::ComputeTaskPool;
use boxel_voxel::{BlockPos, Face, WorldPos};
use boxel_world::{ChunkMap, WorldSettings};
use std::time::Instant;

/// How close a sleeping body must be to a solid body that moved for it to wake up, in blocks.
const WAKE_MARGIN: f64 = 1.0 / 64.0;

/// The broadphase and the solve split their work over the compute threads from this many
/// bodies in the step's list (see [`parallel_above`]): below it the tasks cost more than they
/// save.
const PARALLEL_GRID_MIN: usize = 4096;

/// A body pushed along a normal whose `y` is above this stands on the other body
/// (`Contacts::on_ground`): slopes up to about 45 degrees.
const STAND_NORMAL_Y: f64 = 0.7;

// ------------------------------------------------------------------------------- obstacles

/// A static or kinematic solid collider: dynamic bodies sweep against its box.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Obstacle {
    pos: WorldPos,
    bx: LocalBox,
    layers: CollisionLayers,
}

/// The static and kinematic solid colliders of the current step, in a grid (rebuilt in
/// `PhysicsSystems::Kinematic`, after the kinematic bodies moved).
#[derive(Resource, Default)]
pub(crate) struct ObstacleIndex {
    obstacles: Vec<Obstacle>,
    grid: Grid,
}

impl ObstacleIndex {
    /// The obstacles that meet `layers` near the box `bx` (relative to `pos`, canonical) moving
    /// by `d`, as boxes in the frame of `pos.block`, into `out` (cleared first). `ids` is
    /// scratch.
    fn gather(
        &self,
        pos: WorldPos,
        bx: LocalBox,
        d: DVec3,
        layers: CollisionLayers,
        ids: &mut Vec<u32>,
        out: &mut Vec<LocalBox>,
    ) {
        out.clear();
        if self.obstacles.is_empty() {
            return;
        }
        let d = DVec3::new(finite(d.x), finite(d.y), finite(d.z));
        let swept = LocalBox::new(bx.min.min(bx.min + d), bx.max.max(bx.max + d));
        ids.clear();
        self.grid.query(BlockBounds::of_box(pos, swept), ids);
        let frame = WorldPos {
            block: pos.block,
            frac: DVec3::ZERO,
        };
        for &i in ids.iter() {
            let o = &self.obstacles[i as usize];
            if o.layers.interacts(layers) {
                out.push(o.bx.translate(frame.delta_to(&o.pos)));
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.obstacles.is_empty()
    }
}

/// Scratch buffers for gathering obstacles (one set per system).
#[derive(Default)]
pub(crate) struct ObstacleScratch {
    ids: Vec<u32>,
    boxes: Vec<LocalBox>,
}

impl ObstacleScratch {
    /// The obstacles in the way of `bx` (relative to `pos`, canonical) moving by `d`.
    pub(crate) fn gather(
        &mut self,
        index: &ObstacleIndex,
        pos: WorldPos,
        bx: LocalBox,
        d: DVec3,
        layers: CollisionLayers,
    ) -> &[LocalBox] {
        index.gather(pos, bx, d, layers, &mut self.ids, &mut self.boxes);
        &self.boxes
    }
}

fn finite(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

/// [`crate::PhysicsSystems::Kinematic`], after kinematic bodies moved: the obstacle index (the
/// static and kinematic solid colliders; a projectile is never one, whatever its kind).
pub(crate) fn build_obstacles(
    settings: Res<PhysicsSettings>,
    mut index: ResMut<ObstacleIndex>,
    bodies: Query<(&RigidBody, &Collider, &BodyPosition), Without<Projectile>>,
) {
    let index = &mut *index;
    index.obstacles.clear();
    index.grid.reset(settings.grid_shift);
    for (kind, collider, position) in &bodies {
        if *kind == RigidBody::Dynamic || collider.is_sensor() {
            continue;
        }
        let obstacle = Obstacle {
            pos: position.0.normalize(),
            bx: collider.voxel_box(),
            layers: collider.layers,
        };
        index.grid.insert(
            BlockBounds::of_box(obstacle.pos, obstacle.bx),
            obstacle.layers,
            false,
        );
        index.obstacles.push(obstacle);
    }
    if !index.obstacles.is_empty() {
        index.grid.build();
    }
}

// ------------------------------------------------------------------------------- body index

/// One body of the [`BodyIndex`] (in the step's list or in the sleeper store).
#[derive(Clone, Copy, Debug)]
pub(crate) struct IndexedBody {
    pub(crate) entity: Entity,
    pub(crate) pos: WorldPos,
    pub(crate) shape: Shape,
    pub(crate) offset: DVec3,
    pub(crate) layers: CollisionLayers,
    pub(crate) sensor: bool,
    pub(crate) report: bool,
    pub(crate) kind: RigidBody,
    pub(crate) frozen: bool,
    pub(crate) weight: f64,
    /// How far it moved in this step before the solve.
    pub(crate) step: DVec3,
    pub(crate) vel: DVec3,
    pub(crate) faces: u8,
    pub(crate) on_ground: bool,
    /// It is asleep (a copy from the sleeper store).
    pub(crate) sleeping: bool,
    /// It can disturb other bodies this step (see [`broadphase`]); pairs need one active body.
    pub(crate) active: bool,
    /// It takes part in collisions between bodies (false for sleepers without a collider and
    /// sleeping projectiles, which the store keeps only to wake them).
    pub(crate) collides: bool,
    /// The solve changed its velocity or contacts.
    pub(crate) dirty: bool,
    /// The solve moved it.
    pub(crate) moved: bool,
    /// A sleeping body the solve woke up.
    pub(crate) woken: bool,
    /// How far the solve's pushes moved it.
    pub(crate) pushed: DVec3,
    /// Its bounds in the grid it is in (see [`IndexedBody::step_bounds`]).
    pub(crate) bounds: BlockBounds,
}

impl IndexedBody {
    /// A value for list slots about to be written.
    fn placeholder() -> Self {
        Self {
            entity: Entity::PLACEHOLDER,
            pos: WorldPos::default(),
            shape: Shape::Aabb { half: DVec3::ZERO },
            offset: DVec3::ZERO,
            layers: CollisionLayers::default(),
            sensor: false,
            report: false,
            kind: RigidBody::Static,
            frozen: false,
            weight: 1.0,
            step: DVec3::ZERO,
            vel: DVec3::ZERO,
            faces: 0,
            on_ground: false,
            sleeping: false,
            active: false,
            collides: false,
            dirty: false,
            moved: false,
            woken: false,
            pushed: DVec3::ZERO,
            bounds: BlockBounds::default(),
        }
    }

    /// Its voxel box, relative to `pos`.
    pub(crate) fn bx(&self) -> LocalBox {
        self.shape.bounds().translate(self.offset)
    }

    /// The blocks its box reaches.
    pub(crate) fn end_bounds(&self) -> BlockBounds {
        BlockBounds::of_box(self.pos, self.bx())
    }

    /// Its bounds in this step's grid: an active body that moved also covers where it came
    /// from, so the sleepers it left wake up.
    pub(crate) fn step_bounds(&self) -> BlockBounds {
        let step = DVec3::new(
            finite(self.step.x),
            finite(self.step.y),
            finite(self.step.z),
        );
        if !self.active || step == DVec3::ZERO {
            return self.end_bounds();
        }
        let bx = self.bx();
        let from = bx.translate(-step);
        BlockBounds::of_box(
            self.pos,
            LocalBox::new(bx.min.min(from.min), bx.max.max(from.max)),
        )
    }

    /// The same body with `bounds` set to its end bounds.
    pub(crate) fn with_end_bounds(mut self) -> Self {
        self.bounds = self.end_bounds();
        self
    }

    /// A dynamic body physics may move (not frozen; a sleeping one wakes up when moved).
    fn free(&self) -> bool {
        self.kind == RigidBody::Dynamic && !self.frozen
    }

    /// Asleep, and not woken up by this step's solve.
    fn still_asleep(&self) -> bool {
        self.sleeping && !self.woken
    }

    /// The bottom of its box, exact anywhere: `(block, offset in [0, 1))`.
    fn bottom(&self) -> (i64, f64) {
        let f = self.pos.frac.y + self.bx().min.y;
        let whole = f.floor();
        (self.pos.block.y.saturating_add(whole as i64), f - whole)
    }
}

/// Every body with a [`Collider`] as of the end of the latest fixed step (after the pushes):
/// what the entity raycast, [`BodyIndex::overlap_box`] and [`BodyIndex::block_is_free`] search
/// (also through [`crate::PhysicsQuery`]). The awake bodies are rebuilt in
/// `PhysicsSystems::Broadphase` every step and updated by the solve; the sleeping ones live in
/// a store of their own (see [`crate::Sleeping`]). Bodies spawned, moved or despawned since the
/// latest step are not seen until the next one. Empty before the first step. Projectiles are
/// not in it.
#[derive(Resource)]
pub struct BodyIndex {
    /// The step's list, in query order (then the pulled and pending sleepers): only the pairs
    /// are sorted.
    bodies: Vec<IndexedBody>,
    grid: Grid,
    pairs: Vec<(u32, u32)>,
    eps: f64,
    order: Vec<(i64, f64, u32)>,
    scratch: ObstacleScratch,
    ids: Vec<u32>,
    /// One pair buffer per task of the parallel pair sweep.
    parts: Vec<Vec<(u32, u32)>>,
    /// The awake bodies of a parallel broadphase, in query order.
    entities: Vec<Entity>,
    pub(crate) sleepers: Sleepers,
}

impl Default for BodyIndex {
    fn default() -> Self {
        Self {
            bodies: Vec::new(),
            grid: Grid::default(),
            pairs: Vec::new(),
            eps: sweep_eps(SweepConfig::default().eps),
            order: Vec::new(),
            scratch: ObstacleScratch::default(),
            ids: Vec::new(),
            parts: Vec::new(),
            entities: Vec::new(),
            sleepers: Sleepers::default(),
        }
    }
}

/// Which bodies a query finds.
#[derive(Clone, Copy, Debug)]
pub struct BodyFilter<'a> {
    /// A body is found when its `CollisionLayers::member` shares a bit with this (default: every
    /// layer).
    pub mask: u32,
    /// Solid colliders are found (default true).
    pub solids: bool,
    /// Sensors are found (default false).
    pub sensors: bool,
    /// Entities never found: the shooter of a ray, the player looking around.
    pub exclude: &'a [Entity],
}

impl Default for BodyFilter<'_> {
    fn default() -> Self {
        Self {
            mask: u32::MAX,
            solids: true,
            sensors: false,
            exclude: &[],
        }
    }
}

impl<'a> BodyFilter<'a> {
    /// The default filter (solid bodies of every layer) without `exclude`.
    pub fn excluding(exclude: &'a [Entity]) -> Self {
        Self {
            exclude,
            ..Self::default()
        }
    }

    /// The same filter that also finds sensors (`true`) or not.
    pub fn with_sensors(self, sensors: bool) -> Self {
        Self { sensors, ..self }
    }

    /// The same filter for the layers in `mask`.
    pub fn with_mask(self, mask: u32) -> Self {
        Self { mask, ..self }
    }

    fn accepts(&self, body: &IndexedBody) -> bool {
        body.layers.member & self.mask != 0
            && if body.sensor {
                self.sensors
            } else {
                self.solids
            }
            && !self.exclude.contains(&body.entity)
    }
}

/// A body a ray hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyHit {
    /// The body.
    pub entity: Entity,
    /// Ray parameter of the hit (`origin + dir * t`).
    pub t: f64,
    /// Where the ray met the body's shape.
    pub point: WorldPos,
    /// Outward normal of the shape there; zero when the ray started inside it.
    pub normal: DVec3,
}

/// The best ray hit so far: `(t, entity bits, entity, normal)`.
type BestHit = Option<(f64, u64, Entity, DVec3)>;

impl BodyIndex {
    /// Number of bodies, awake and asleep.
    pub fn len(&self) -> usize {
        self.bodies.len() + self.queryable_sleepers().count()
    }

    /// No bodies.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The sleepers of the store's grid that queries search (the others are in the step's list
    /// or gone).
    fn queryable_sleepers(&self) -> impl Iterator<Item = &IndexedBody> + '_ {
        let sleepers = &self.sleepers;
        sleepers
            .grid_bodies()
            .iter()
            .enumerate()
            .filter(|&(i, _)| sleepers.queryable(i as u32))
            .map(|(_, body)| body)
    }

    /// The bodies, awake and asleep, sorted by `Entity::to_bits`.
    pub fn entities(&self) -> impl Iterator<Item = Entity> + '_ {
        let mut all: Vec<Entity> = self
            .bodies
            .iter()
            .chain(self.queryable_sleepers())
            .map(|b| b.entity)
            .collect();
        all.sort_unstable_by_key(|e| e.to_bits());
        all.into_iter()
    }

    /// The bodies of the latest step's list (awake ones, and the sleepers it pulled), with
    /// their positions at the end of that step.
    pub(crate) fn listed(&self) -> impl Iterator<Item = (Entity, WorldPos)> + '_ {
        self.bodies.iter().map(|b| (b.entity, b.pos))
    }

    /// The candidate pairs of the latest broadphase, as entities (`a < b` by bits), sorted. Two
    /// sleeping bodies never form a pair.
    pub fn pairs(&self) -> impl Iterator<Item = (Entity, Entity)> + '_ {
        self.pairs.iter().map(|&(a, b)| {
            (
                self.bodies[a as usize].entity,
                self.bodies[b as usize].entity,
            )
        })
    }

    /// The first body `filter` accepts on the ray `origin + dir * t`, `t` in `0..=max_t`: exact
    /// against boxes and spheres, found by walking the grid's cells along the ray. Ties go to
    /// the lower entity. A ray that starts inside a body hits it at `t = 0` with a zero normal
    /// (exclude your own body). `max_t` is limited like [`crate::raycast_voxels`]'s.
    pub fn raycast(
        &self,
        origin: WorldPos,
        dir: DVec3,
        max_t: f64,
        filter: &BodyFilter<'_>,
    ) -> Option<BodyHit> {
        let origin = origin.normalize();
        let dir = if dir.is_finite() { dir } else { DVec3::ZERO };
        let max_t = if max_t.is_nan() {
            0.0
        } else {
            max_t.min(MAX_RAY_DISTANCE / dir.abs().max_element())
        };
        let mut best: BestHit = None;
        ray_through(
            &self.bodies,
            &self.grid,
            |_| true,
            origin,
            dir,
            max_t,
            filter,
            &mut best,
        );
        let sleepers = &self.sleepers;
        ray_through(
            sleepers.grid_bodies(),
            sleepers.grid(),
            |i| sleepers.queryable(i),
            origin,
            dir,
            max_t,
            filter,
            &mut best,
        );
        best.map(|(t, _, entity, normal)| BodyHit {
            entity,
            t,
            point: origin.offset(dir * t),
            normal,
        })
    }

    /// Every body (awake or asleep) whose bounds overlap `bounds`, into `out` (cleared first),
    /// unsorted. `ids` is scratch.
    pub(crate) fn near(&self, bounds: BlockBounds, ids: &mut Vec<u32>, out: &mut Vec<IndexedBody>) {
        out.clear();
        for (bodies, grid, sleepers) in [
            (&self.bodies[..], &self.grid, false),
            (self.sleepers.grid_bodies(), self.sleepers.grid(), true),
        ] {
            if bodies.is_empty() {
                continue;
            }
            ids.clear();
            grid.query(bounds, ids);
            out.extend(
                ids.iter()
                    .filter(|&&i| !sleepers || self.sleepers.queryable(i))
                    .map(|&i| bodies[i as usize]),
            );
        }
    }

    /// The bodies `filter` accepts that overlap the box `bx` (relative to `pos`) by more than
    /// the contact tolerance, sorted by entity.
    fn overlapping(&self, pos: WorldPos, bx: LocalBox, filter: &BodyFilter<'_>) -> Vec<Entity> {
        let pos = pos.normalize();
        let mut ids = Vec::new();
        let mut near = Vec::new();
        self.near(BlockBounds::of_box(pos, bx), &mut ids, &mut near);
        let mut found: Vec<Entity> = near
            .iter()
            .filter(|body| {
                filter.accepts(body)
                    && shape_box(body.shape, pos.delta_to(&body.pos) + body.offset, bx)
                        .is_some_and(|c| c.depth > self.eps)
            })
            .map(|body| body.entity)
            .collect();
        found.sort_unstable_by_key(|e| e.to_bits());
        found
    }

    /// The bodies `filter` accepts that overlap the box `bx` (relative to `pos`) by more than
    /// the contact tolerance, sorted by entity. Spheres are tested exactly.
    pub fn overlap_box(&self, pos: WorldPos, bx: LocalBox, filter: &BodyFilter<'_>) -> Vec<Entity> {
        self.overlapping(pos, bx, filter)
    }

    /// No solid body overlaps block `block` (by more than the contact tolerance): a block placed
    /// there would not end up inside a player, a mob or a crate. Sensors (dropped items) do not
    /// count; whatever the block holds now is not checked.
    pub fn block_is_free(&self, block: BlockPos) -> bool {
        self.shape_is_free(block, CollisionShape::Full)
    }

    /// The placement check of a shaped block: no solid body overlaps `shape` put at `block`
    /// (any of its boxes for a slab; always free for a shape without collision, a plant).
    pub fn shape_is_free(&self, block: BlockPos, shape: CollisionShape<'_>) -> bool {
        let free = |bx: LocalBox| {
            self.overlapping(WorldPos::from(block), bx, &BodyFilter::default())
                .is_empty()
        };
        match shape {
            CollisionShape::Empty => true,
            CollisionShape::Full => free(LocalBox::new(DVec3::ZERO, DVec3::ONE)),
            CollisionShape::Boxes(boxes) => boxes.iter().all(|&bx| free(bx)),
        }
    }
}

/// The ray of [`BodyIndex::raycast`] through one set of bodies and its grid, keeping the best
/// hit in `best`. `keep` says which grid indices count.
#[expect(
    clippy::too_many_arguments,
    reason = "an internal helper of one method"
)]
fn ray_through(
    bodies: &[IndexedBody],
    grid: &Grid,
    keep: impl Fn(u32) -> bool,
    origin: WorldPos,
    dir: DVec3,
    max_t: f64,
    filter: &BodyFilter<'_>,
    best: &mut BestHit,
) {
    if bodies.is_empty() {
        return;
    }
    let test = |i: u32, best: &mut BestHit| {
        if !keep(i) {
            return;
        }
        let body = &bodies[i as usize];
        if !filter.accepts(body) {
            return;
        }
        let center = origin.delta_to(&body.pos) + body.offset;
        let bits = body.entity.to_bits();
        if let Some(hit) = body.shape.ray(center, DVec3::ZERO, dir, max_t)
            && best.is_none_or(|(t, b, ..)| hit.t < t || (hit.t == t && bits < b))
        {
            *best = Some((hit.t, bits, body.entity, hit.normal));
        }
    };
    if grid.is_built() {
        for &l in grid.large() {
            test(l, best);
        }
        for (cell, enter) in grid.cell_ray(origin, dir, max_t) {
            // A body hit at `t` has the hit point in a cell the ray entered by `t`.
            if best.is_some_and(|(t, ..)| enter > t) {
                break;
            }
            for i in grid.cell(cell) {
                test(i, best);
            }
        }
    } else {
        for i in 0..bodies.len() as u32 {
            test(i, best);
        }
    }
}

/// A body can disturb others this step (see [`broadphase`]).
fn is_active(kind: RigidBody, sync: &PhysicsSync, collider_changed: bool, asleep: bool) -> bool {
    match kind {
        RigidBody::Dynamic => !asleep,
        RigidBody::Kinematic => {
            !asleep && (sync.moved != DVec3::ZERO || sync.teleported || collider_changed)
        }
        RigidBody::Static => sync.teleported || collider_changed,
    }
}

/// What the broadphase reads of an awake body.
type BroadphaseItem<'a> = (
    Entity,
    &'a RigidBody,
    Ref<'a, Collider>,
    &'a BodyPosition,
    &'a Velocity,
    &'a Contacts,
    &'a BodySettings,
    &'a PhysicsSync,
    bool,
    bool,
);

/// The step's list entry of an awake body, with its bounds in this step's grid.
fn listed_body(
    (entity, kind, collider, position, velocity, contacts, body, sync, frozen, report): BroadphaseItem<'_>,
) -> IndexedBody {
    let active = is_active(*kind, sync, collider.is_changed(), false);
    let mut listed = IndexedBody {
        entity,
        pos: position.0.normalize(),
        shape: collider.shape,
        offset: collider.offset,
        layers: collider.layers,
        sensor: collider.is_sensor(),
        report,
        kind: *kind,
        frozen,
        weight: body.push_weight,
        step: sync.moved,
        vel: velocity.0,
        faces: contacts.faces,
        on_ground: contacts.on_ground,
        sleeping: false,
        active,
        collides: true,
        dirty: false,
        moved: false,
        woken: false,
        pushed: DVec3::ZERO,
        bounds: BlockBounds::default(),
    };
    listed.bounds = listed.step_bounds();
    listed
}

/// [`crate::PhysicsSystems::Broadphase`]: the step's list of the [`BodyIndex`] (every awake
/// body with a collider, plus the sleepers that matter this step) and its candidate pairs,
/// sorted by entity. The list's own order does not matter: everything that follows goes
/// through the pairs.
///
/// Active bodies (they can disturb others this step): awake dynamic bodies (frozen too),
/// kinematic bodies that moved, and bodies teleported or nudged this step or whose collider
/// changed. An active body that moved is in the grid with the bounds it swept (from where it was
/// to where it is), and pulls the sleepers those bounds reach out of the store.
///
/// With many bodies (see [`parallel_above`]) the awake bodies are listed and placed in the grid
/// on the compute threads, each one into its own slot of the list in query order, and the pair
/// sweep is split by cells: the list, the grid and the pairs are the same as on one thread.
#[expect(
    clippy::type_complexity,
    reason = "a system's parameters; the query is spelled out as its item type"
)]
pub(crate) fn broadphase(
    settings: Res<PhysicsSettings>,
    mut index: ResMut<BodyIndex>,
    mut stats: ResMut<PhysicsStats>,
    mut log: ResMut<SleepLog>,
    sleeper_data: Query<SleeperData, With<Sleeping>>,
    bodies: Query<
        (
            Entity,
            &RigidBody,
            Ref<Collider>,
            &BodyPosition,
            &Velocity,
            &Contacts,
            &BodySettings,
            &PhysicsSync,
            Has<Frozen>,
            Has<ReportContacts>,
        ),
        (Without<Sleeping>, Without<Projectile>),
    >,
) {
    let start = Instant::now();
    let BodyIndex {
        bodies: list,
        grid,
        pairs,
        eps,
        sleepers,
        ids,
        parts,
        entities,
        ..
    } = &mut *index;
    sync_sleepers(sleepers, &mut log, &sleeper_data);
    sleepers.begin_step(settings.grid_shift);
    *eps = sweep_eps(settings.eps);
    grid.reset(settings.grid_shift);
    let awake = bodies.iter().len();
    let parallel = parallel_above(&settings, awake, PARALLEL_GRID_MIN);
    if parallel {
        ensure_compute_pool();
        let pool = ComputeTaskPool::get();
        entities.clear();
        entities.extend(bodies.iter().map(|item| item.0));
        // Each body into its own slot of the list and of the grid, in query order. The list
        // keeps its length from step to step: only a longer one needs placeholders.
        list.truncate(awake);
        list.resize(awake, IndexedBody::placeholder());
        let size = awake.div_ceil(pool.thread_num().max(1) * 2).max(64);
        let shift = grid.shift();
        let (bodies, entities, list) = (&bodies, &*entities, &mut *list);
        grid.push_slots(awake, |slots| {
            let parts = pool.scope(|scope| {
                for (k, ((entities, list), slots)) in entities
                    .chunks(size)
                    .zip(list.chunks_mut(size))
                    .zip(slots.chunks_mut(size))
                    .enumerate()
                {
                    scope.spawn(async move {
                        let mut large = Vec::new();
                        for (i, ((&entity, body), slot)) in
                            entities.iter().zip(list).zip(slots).enumerate()
                        {
                            if let Ok(item) = bodies.get(entity) {
                                *body = listed_body(item);
                                *slot = GridSlot::new(shift, body.bounds, body.layers, body.active);
                                if slot.is_large() {
                                    large.push(((k * size + i) as u32, body.bounds));
                                }
                            }
                        }
                        large
                    });
                }
            });
            parts.into_iter().flatten().collect()
        });
    } else {
        list.clear();
        list.extend(bodies.iter().map(listed_body));
    }
    // The sleepers the active bodies reach, and the ones that fell asleep since the store's
    // last rebuild (not in its grid yet).
    if sleepers.has_grid_bodies() {
        for i in 0..awake {
            if list[i].active {
                let bounds = list[i].bounds;
                sleepers.pull(bounds, ids, list);
            }
        }
    }
    list.extend(sleepers.pending_colliders().copied());
    // The rest of the list (the awake bodies too on one thread).
    let placed = grid.len();
    for body in &list[placed..] {
        grid.insert(body.bounds, body.layers, body.active);
    }
    if parallel {
        // The cells split into ranges, one task each: the pairs come out sorted below anyway.
        grid.build();
        let pool = ComputeTaskPool::get();
        let grid = &*grid;
        let ranges = grid.pair_ranges(pool.thread_num().max(1) * 2);
        if parts.len() < ranges.len() {
            parts.resize_with(ranges.len(), Vec::new);
        }
        pool.scope(|scope| {
            for (range, found) in ranges.into_iter().zip(parts.iter_mut()) {
                scope.spawn(async move {
                    found.clear();
                    grid.pairs_in(range, found);
                });
            }
        });
        pairs.clear();
        for part in parts.iter_mut() {
            pairs.append(part);
        }
        grid.large_pairs(pairs);
    } else {
        grid.pairs(pairs);
    }
    // Entity order (each pair's lower entity first): pairs, pushes and events come out the same
    // on every run, whatever the order of the list.
    for pair in pairs.iter_mut() {
        if list[pair.0 as usize].entity.to_bits() > list[pair.1 as usize].entity.to_bits() {
            *pair = (pair.1, pair.0);
        }
    }
    pairs.sort_unstable_by_key(|&(a, b)| {
        (
            list[a as usize].entity.to_bits(),
            list[b as usize].entity.to_bits(),
        )
    });
    stats.pairs = pairs.len();
    stats.solve_time = start.elapsed();
}

// ------------------------------------------------------------------------------- the solve

/// The pairs in contact at the end of a step that report events, as entities sorted by bits,
/// and the previous step's; and the solve's and the events' reused buffers.
#[derive(Resource, Default)]
pub(crate) struct ContactSet {
    previous: Vec<(Entity, Entity)>,
    current: Vec<(Entity, Entity)>,
    scratch: Vec<(u32, u32)>,
    /// The bodies the solve may change.
    touched: Vec<u32>,
    /// Sleepers whose support moved.
    lost_support: Vec<u32>,
    /// Sleepers the solve woke up.
    woken: Vec<Entity>,
    /// The step's new and ended pairs.
    started: Vec<(Entity, Entity)>,
    ended: Vec<(Entity, Entity)>,
    /// The parallel solve's islands.
    islands: Islands,
}

/// The face of a box facing direction `d` (its largest component; ties Y, X, Z).
fn face_toward(d: DVec3) -> Face {
    let a = d.abs();
    if a.y >= a.x && a.y >= a.z {
        if d.y > 0.0 { Face::PosY } else { Face::NegY }
    } else if a.x >= a.z {
        if d.x > 0.0 { Face::PosX } else { Face::NegX }
    } else if d.z > 0.0 {
        Face::PosZ
    } else {
        Face::NegZ
    }
}

/// `push_weight` as the solve uses it.
fn weight(w: f64) -> f64 {
    if w.is_nan() { 1.0 } else { w.clamp(1e-6, 1e6) }
}

/// The overlap of two bodies, in `a`'s frame (normal from `a` toward `b`). Two boxes are
/// pushed back toward the side they came from in this step ([`box_box_moving`]).
fn pair_contact(a: &IndexedBody, b: &IndexedBody, eps: f64) -> Option<Contact> {
    let rel = a.pos.delta_to(&b.pos);
    match (a.shape, b.shape) {
        (Shape::Aabb { half: ha }, Shape::Aabb { half: hb }) => box_box_moving(
            LocalBox::from_center_half(a.offset, ha),
            LocalBox::from_center_half(rel + b.offset, hb),
            b.step - a.step,
            eps,
        ),
        _ => shape_contact(a.shape, a.offset, b.shape, rel + b.offset),
    }
}

/// Two solid bodies touch or overlap at their places now: a contact of [`ReportContacts`].
/// Boxes must touch face to face: within `eps` of each other on one axis and overlapping by
/// more than `eps` on the other two (grazing an edge does not count). A static or kinematic body
/// counts as its box (`Collider::voxel_box`), as the dynamic bodies' sweeps see it; between two
/// dynamic bodies a sphere is exact, grown by `eps`.
fn in_contact(a: &IndexedBody, b: &IndexedBody, eps: f64) -> bool {
    let rel = a.pos.delta_to(&b.pos);
    let boxes = matches!((a.shape, b.shape), (Shape::Aabb { .. }, Shape::Aabb { .. }))
        || a.kind != RigidBody::Dynamic
        || b.kind != RigidBody::Dynamic;
    if !boxes {
        let grown = match a.shape {
            Shape::Aabb { half } => Shape::Aabb {
                half: half + DVec3::splat(eps),
            },
            Shape::Sphere { radius } => Shape::Sphere {
                radius: radius + eps,
            },
        };
        return shape_contact(grown, a.offset, b.shape, rel + b.offset).is_some();
    }
    let (p, q) = (a.bx(), b.bx().translate(rel));
    let mut near = 0;
    for axis in 0..3 {
        let overlap = p.max[axis].min(q.max[axis]) - p.min[axis].max(q.min[axis]);
        if overlap > eps {
            continue;
        }
        if overlap.is_nan() || overlap < -eps {
            return false;
        }
        near += 1;
    }
    near <= 1
}

/// What a push reads: the voxels, the obstacles and the sweep tuning.
struct PushWorld<'a, 'w> {
    voxels: &'a mut ChunkSource<'w>,
    table: &'a CollisionTable,
    obstacles: &'a ObstacleIndex,
    scratch: &'a mut ObstacleScratch,
    cfg: SweepConfig,
}

impl PushWorld<'_, '_> {
    /// Moves `body` by `amount` along the unit vector `dir` through the voxels and obstacles;
    /// returns how far it moved along `dir`. A body whose move needs an unloaded chunk stays.
    fn push(&mut self, body: &mut IndexedBody, dir: DVec3, amount: f64) -> f64 {
        let d = dir * amount;
        let bx = body.bx();
        let obstacles = self
            .scratch
            .gather(self.obstacles, body.pos, bx, d, body.layers);
        match move_box(
            self.voxels,
            self.table,
            body.pos,
            bx,
            d,
            obstacles,
            &self.cfg,
        ) {
            Ok(out) => {
                body.pos = out.end;
                body.faces |= out.faces;
                body.on_ground |= out.on_ground();
                body.moved = true;
                body.dirty = true;
                body.woken |= body.sleeping;
                body.pushed += out.moved;
                out.moved.dot(dir)
            }
            Err(_) => 0.0,
        }
    }
}

/// How far `body` must move along the axis `dir` (a unit axis vector) for its face to rest
/// exactly on `other`'s opposite face, computed in `body`'s own frame: a body resting on another
/// computes the same position every step, bit for bit. `None` unless both are boxes and `dir` is
/// an axis.
fn exact_push(body: &IndexedBody, other: &IndexedBody, dir: DVec3) -> Option<f64> {
    if !matches!(
        (body.shape, other.shape),
        (Shape::Aabb { .. }, Shape::Aabb { .. })
    ) {
        return None;
    }
    let axis = (0..3).find(|&a| dir[a].abs() == 1.0)?;
    let (other_box, body_box) = (other.bx(), body.bx());
    let (other_face, body_face) = if dir[axis] > 0.0 {
        (other_box.max[axis], body_box.min[axis])
    } else {
        (other_box.min[axis], body_box.max[axis])
    };
    let blocks =
        other.pos.block.as_i64vec3()[axis].saturating_sub(body.pos.block.as_i64vec3()[axis]) as f64;
    let target = blocks + (other.pos.frac[axis] + other_face) - body_face;
    // `dir[axis]` is 1 or -1: the move `dir * amount` is exactly `target - frac` on the axis.
    Some((target - body.pos.frac[axis]) * dir[axis])
}

/// Pushes `a` and `b` apart along `contact` (see the module docs). Returns whether a push
/// happened (at least one of them could move).
fn push_pair(
    a: &mut IndexedBody,
    b: &mut IndexedBody,
    contact: Contact,
    world: &mut PushWorld<'_, '_>,
) -> bool {
    let n = contact.normal;
    let a_free = a.free() && a.faces & (1 << face_toward(-n).index()) == 0;
    let b_free = b.free() && b.faces & (1 << face_toward(n).index()) == 0;
    // Touching faces, and standing on the other body, for every dynamic body of the pair.
    if a.free() {
        a.faces |= 1 << face_toward(n).index();
        a.on_ground |= -n.y > STAND_NORMAL_Y;
        a.dirty = true;
    }
    if b.free() {
        b.faces |= 1 << face_toward(-n).index();
        b.on_ground |= n.y > STAND_NORMAL_Y;
        b.dirty = true;
    }
    let (share_a, share_b) = match (a_free, b_free) {
        (true, true) => {
            let (wa, wb) = (weight(a.weight), weight(b.weight));
            let share_a = wb / (wa + wb);
            (share_a, 1.0 - share_a)
        }
        (true, false) => (1.0, 0.0),
        (false, true) => (0.0, 1.0),
        (false, false) => return false,
    };
    let approach = (b.vel - a.vel).dot(n);
    if approach < 0.0 {
        if a_free {
            a.vel += n * (approach * share_a);
            a.woken |= a.sleeping;
        }
        if b_free {
            b.vel -= n * (approach * share_b);
            b.woken |= b.sleeping;
        }
    }
    // A body that takes the whole push lands exactly on the other one's face. With a split,
    // `a` moves its share and `b` the rest, up to `a`'s new place (all of it if `a` was
    // stopped by a wall).
    let mut moved_a = 0.0;
    if a_free {
        let amount = if b_free {
            contact.depth * share_a
        } else {
            exact_push(a, b, -n).unwrap_or(contact.depth)
        };
        if amount > 0.0 {
            moved_a = world.push(a, -n, amount);
        }
    }
    if b_free {
        let amount = exact_push(b, a, n).unwrap_or(contact.depth - moved_a);
        if amount > 0.0 {
            world.push(b, n, amount);
        }
    }
    true
}

/// The pushes of one group of solid pairs (`pairs`: indices into `bodies`, bottom-up):
/// `passes` passes (at least one; with 0 nothing is pushed), each pair that overlaps by more
/// than `eps` pushed apart in turn. The bodies of overlapping pairs go into `touched`; returns
/// how many pushes happened.
fn push_pairs(
    bodies: &mut [IndexedBody],
    pairs: impl Iterator<Item = (u32, u32)> + Clone,
    passes: u32,
    eps: f64,
    world: &mut PushWorld<'_, '_>,
    touched: &mut Vec<u32>,
) -> usize {
    let mut pushes = 0;
    for _ in 0..passes.max(1) {
        for (a, b) in pairs.clone() {
            let Ok([first, second]) = bodies.get_disjoint_mut([a as usize, b as usize]) else {
                continue;
            };
            let Some(contact) = pair_contact(first, second, eps).filter(|c| c.depth > eps) else {
                continue;
            };
            touched.push(a);
            touched.push(b);
            if passes > 0 && push_pair(first, second, contact, world) {
                pushes += 1;
            }
        }
    }
    pushes
}

/// What every task of a parallel solve reads.
struct PushShared<'a> {
    map: &'a ChunkMap,
    world: &'a WorldSettings,
    table: &'a CollisionTable,
    obstacles: &'a ObstacleIndex,
    cfg: SweepConfig,
    eps: f64,
    passes: u32,
}

/// The solve's pairs split into islands for the compute threads. A push only changes the two
/// bodies of its pair, and only the free ones (`IndexedBody::free`; the others are only read),
/// so pairs that share no free body never affect each other: the islands are the groups of
/// pairs linked through free bodies. Each island keeps the solve's order (bottom-up) and runs
/// on its own copy of its bodies, and the free ones are copied back: the same pushes, bit for
/// bit, as one run over every pair, whatever the threads.
#[derive(Default)]
pub(crate) struct Islands {
    /// Union-find over the step's list.
    parent: Vec<u32>,
    /// Per root body, its island (`u32::MAX`: none yet).
    island_of: Vec<u32>,
    /// Per pair of the order, its island.
    pair_island: Vec<u32>,
    /// Island `i`'s pairs are `pairs[starts[i]..starts[i + 1]]` (as indices into its bodies),
    /// its bodies `bodies[body_starts[i]..body_starts[i + 1]]` (as list indices).
    starts: Vec<u32>,
    body_starts: Vec<u32>,
    pairs: Vec<(u32, u32)>,
    bodies: Vec<u32>,
    /// The order's pairs grouped by island (a stable counting sort).
    grouped: Vec<u32>,
    cursor: Vec<u32>,
    /// Per list body, the island it was last copied into (plus one) and its index there.
    stamp: Vec<u32>,
    local: Vec<u32>,
}

impl Islands {
    fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            let up = self.parent[self.parent[x as usize] as usize];
            self.parent[x as usize] = up;
            x = up;
        }
        x
    }

    /// Groups `order` (pairs of `pairs` by index, bottom-up) into islands.
    fn split(&mut self, bodies: &[IndexedBody], pairs: &[(u32, u32)], order: &[(i64, f64, u32)]) {
        let n = bodies.len();
        self.parent.clear();
        self.parent.extend(0..n as u32);
        for &(_, _, k) in order {
            let (a, b) = pairs[k as usize];
            if bodies[a as usize].free() && bodies[b as usize].free() {
                let (ra, rb) = (self.find(a), self.find(b));
                // Link the higher root under the lower (any fixed rule works).
                if ra != rb {
                    self.parent[ra.max(rb) as usize] = ra.min(rb);
                }
            }
        }
        // Islands numbered by their first pair in the order.
        self.island_of.clear();
        self.island_of.resize(n, u32::MAX);
        self.pair_island.clear();
        self.starts.clear();
        for &(_, _, k) in order {
            let (a, b) = pairs[k as usize];
            let key = if bodies[a as usize].free() { a } else { b };
            let root = self.find(key) as usize;
            if self.island_of[root] == u32::MAX {
                self.island_of[root] = self.starts.len() as u32;
                self.starts.push(0);
            }
            let island = self.island_of[root];
            self.pair_island.push(island);
            self.starts[island as usize] += 1;
        }
        // Counts to starts, then the pairs in order within each island.
        let mut sum = 0;
        for start in &mut self.starts {
            let count = *start;
            *start = sum;
            sum += count;
        }
        self.starts.push(sum);
        self.grouped.clear();
        self.grouped.resize(order.len(), 0);
        self.cursor.clear();
        self.cursor.extend_from_slice(&self.starts);
        for (&(_, _, k), &island) in order.iter().zip(&self.pair_island) {
            let at = &mut self.cursor[island as usize];
            self.grouped[*at as usize] = k;
            *at += 1;
        }
        // Each island's bodies (a body that is only read may be in several) and its pairs as
        // indices into them.
        self.stamp.clear();
        self.stamp.resize(n, 0);
        self.local.resize(n, 0);
        self.bodies.clear();
        self.body_starts.clear();
        self.pairs.clear();
        for island in 0..self.starts.len() - 1 {
            let first = self.bodies.len() as u32;
            self.body_starts.push(first);
            let mark = island as u32 + 1;
            let (from, to) = (
                self.starts[island] as usize,
                self.starts[island + 1] as usize,
            );
            for j in from..to {
                let (a, b) = pairs[self.grouped[j] as usize];
                let mut pair = [a, b];
                for g in &mut pair {
                    let i = *g as usize;
                    if self.stamp[i] != mark {
                        self.stamp[i] = mark;
                        self.local[i] = self.bodies.len() as u32 - first;
                        self.bodies.push(*g);
                    }
                    *g = self.local[i];
                }
                self.pairs.push((pair[0], pair[1]));
            }
        }
        self.body_starts.push(self.bodies.len() as u32);
    }

    /// Runs the pushes of every island on the compute threads (see [`push_pairs`]) and copies
    /// the free bodies back into `bodies`; the touched bodies go into `touched` (as list
    /// indices, unsorted). Returns how many pushes happened.
    fn push_all(
        &self,
        bodies: &mut [IndexedBody],
        shared: &PushShared<'_>,
        touched: &mut Vec<u32>,
    ) -> usize {
        let pool = ComputeTaskPool::get();
        let islands = self.starts.len() - 1;
        // Batches of whole islands with about the same number of pairs.
        let per_task = self
            .pairs
            .len()
            .div_ceil(pool.thread_num().max(1) * 2)
            .max(16);
        let mut batches = Vec::new();
        let mut first = 0;
        for island in 0..islands {
            let size = (self.starts[island + 1] - self.starts[first]) as usize;
            if size >= per_task || island + 1 == islands {
                batches.push(first..island + 1);
                first = island + 1;
            }
        }
        let list: &[IndexedBody] = bodies;
        let results = pool.scope(|scope| {
            for batch in batches {
                scope.spawn(async move {
                    let (b0, b1) = (
                        self.body_starts[batch.start] as usize,
                        self.body_starts[batch.end] as usize,
                    );
                    let mut copies: Vec<IndexedBody> = self.bodies[b0..b1]
                        .iter()
                        .map(|&g| list[g as usize])
                        .collect();
                    let mut voxels = ChunkSource::new(shared.map, shared.world);
                    let mut scratch = ObstacleScratch::default();
                    let mut world = PushWorld {
                        voxels: &mut voxels,
                        table: shared.table,
                        obstacles: shared.obstacles,
                        scratch: &mut scratch,
                        cfg: shared.cfg,
                    };
                    let mut touched = Vec::new();
                    let mut pushes = 0;
                    for island in batch {
                        let (from, to) = (
                            self.body_starts[island] as usize - b0,
                            self.body_starts[island + 1] as usize - b0,
                        );
                        let pairs = &self.pairs
                            [self.starts[island] as usize..self.starts[island + 1] as usize];
                        let at = touched.len();
                        pushes += push_pairs(
                            &mut copies[from..to],
                            pairs.iter().copied(),
                            shared.passes,
                            shared.eps,
                            &mut world,
                            &mut touched,
                        );
                        for t in &mut touched[at..] {
                            *t = self.bodies[b0 + from + *t as usize];
                        }
                    }
                    (b0, copies, touched, pushes)
                });
            }
        });
        let mut pushes = 0;
        for (b0, copies, mut found, count) in results {
            for (copy, &g) in copies.into_iter().zip(&self.bodies[b0..]) {
                if copy.free() {
                    bodies[g as usize] = copy;
                }
            }
            touched.append(&mut found);
            pushes += count;
        }
        pushes
    }
}

/// A sleeping solid body `sleeper` touches (within [`WAKE_MARGIN`]) the solid body `mover`
/// anywhere between where `mover` started this step and where it ended (the sweep, then the
/// pushes).
fn touches_path(sleeper: &IndexedBody, mover: &IndexedBody) -> bool {
    let rel = sleeper.pos.delta_to(&mover.pos);
    let end = mover.bx().translate(rel);
    let net = mover.step + mover.pushed;
    let mut path = end;
    for back in [net, mover.pushed] {
        let at = end.translate(-back);
        path = LocalBox::new(path.min.min(at.min), path.max.max(at.max));
    }
    let margin = DVec3::splat(WAKE_MARGIN);
    let path = LocalBox::new(path.min - margin, path.max + margin);
    path.overlaps(sleeper.bx(), 0.0)
}

/// [`crate::PhysicsSystems::Solve`]: pushes solid pairs apart, wakes the sleepers that were
/// pushed or lost what they rested on, finds the overlapping pairs that report events, writes
/// the changed bodies back and refreshes the index for queries.
pub(crate) fn solve(
    mut commands: Commands,
    map: Res<ChunkMap>,
    world: Res<WorldSettings>,
    content: Res<PhysicsContent>,
    settings: Res<PhysicsSettings>,
    fixed: Res<FixedTime>,
    obstacles: Res<ObstacleIndex>,
    mut index: ResMut<BodyIndex>,
    mut contacts: ResMut<ContactSet>,
    mut stats: ResMut<PhysicsStats>,
    mut writeback: Query<(
        &mut BodyPosition,
        &mut Velocity,
        &mut Contacts,
        &mut PhysicsSync,
        &RigidBody,
        Ref<Collider>,
        Has<Sleeping>,
    )>,
) {
    let start = Instant::now();
    let BodyIndex {
        bodies,
        grid,
        pairs,
        eps,
        order,
        scratch,
        sleepers,
        ..
    } = &mut *index;
    // `scratch` holds index pairs taken from the entity-sorted `pairs`, so its entity pairs
    // come out sorted below.
    let eps = *eps;
    let contacts = &mut *contacts;
    contacts.scratch.clear();
    let mut voxels = ChunkSource::new(&map, &world);
    let mut push_world = PushWorld {
        voxels: &mut voxels,
        table: &content.collision,
        obstacles: &obstacles,
        scratch,
        cfg: SweepConfig {
            eps: settings.eps,
            max_depenetration: settings.max_depenetration,
        },
    };

    // Solid pairs with a dynamic body, bottom-up.
    order.clear();
    for (k, &(a, b)) in pairs.iter().enumerate() {
        let (first, second) = (&bodies[a as usize], &bodies[b as usize]);
        if first.sensor || second.sensor {
            continue;
        }
        if first.kind == RigidBody::Dynamic || second.kind == RigidBody::Dynamic {
            let (lo_a, lo_b) = (first.bottom(), second.bottom());
            let lower = if lo_b.0.cmp(&lo_a.0).then(lo_b.1.total_cmp(&lo_a.1)).is_lt() {
                lo_b
            } else {
                lo_a
            };
            order.push((lower.0, lower.1, k as u32));
        }
    }
    order.sort_unstable_by(|x, y| x.0.cmp(&y.0).then(x.1.total_cmp(&y.1)).then(x.2.cmp(&y.2)));
    // The bodies the solve may change: the ones of overlapping pairs.
    let touched = &mut contacts.touched;
    touched.clear();
    let passes = settings.solver_passes;
    let pushes = if parallel_above(&settings, bodies.len(), PARALLEL_GRID_MIN) && !order.is_empty()
    {
        ensure_compute_pool();
        let islands = &mut contacts.islands;
        islands.split(bodies, pairs, order);
        let shared = PushShared {
            map: &map,
            world: &world,
            table: &content.collision,
            obstacles: &obstacles,
            cfg: push_world.cfg,
            eps,
            passes,
        };
        islands.push_all(bodies, &shared, touched)
    } else {
        let order = order.iter().map(|&(_, _, k)| pairs[k as usize]);
        push_pairs(bodies, order, passes, eps, &mut push_world, touched)
    };

    // Sleepers touching a solid body that moved this step (what they rested on may be gone).
    let still = settings.sleep_speed * fixed.timestep().as_secs_f64();
    let moved = |body: &IndexedBody| {
        !body.sensor
            && !body.still_asleep()
            && (body.step + body.pushed).abs().max_element() > still
    };
    let lost_support = &mut contacts.lost_support;
    lost_support.clear();
    for &(a, b) in pairs.iter() {
        for (s, m) in [(a, b), (b, a)] {
            let (sleeper, mover) = (&bodies[s as usize], &bodies[m as usize]);
            if sleeper.still_asleep()
                && !sleeper.sensor
                && moved(mover)
                && touches_path(sleeper, mover)
            {
                lost_support.push(s);
            }
        }
    }
    for &s in lost_support.iter() {
        bodies[s as usize].woken = true;
        touched.push(s);
    }
    touched.sort_unstable();
    touched.dedup();

    // The pairs that report, at the bodies' final places: overlapping sensor pairs, and solid
    // pairs with `ReportContacts` that touch or overlap.
    for &(a, b) in pairs.iter() {
        let (first, second) = (&bodies[a as usize], &bodies[b as usize]);
        let contact = if first.sensor || second.sensor {
            pair_contact(first, second, eps).is_some_and(|c| c.depth > eps)
        } else {
            (first.report || second.report) && in_contact(first, second, eps)
        };
        if contact {
            contacts.scratch.push((a, b));
        }
    }
    contacts.current.clear();
    contacts.current.extend(
        contacts
            .scratch
            .iter()
            .map(|&(a, b)| (bodies[a as usize].entity, bodies[b as usize].entity)),
    );
    // Pairs of two inactive bodies were not tested: nothing moved them, so they overlap as
    // they did in the previous step (while both still exist with a collider; projectiles
    // never pair).
    let inactive = |entity: Entity| -> bool {
        writeback
            .get(entity)
            .is_ok_and(|(.., sync, kind, collider, asleep)| {
                !is_active(*kind, sync, collider.is_changed(), asleep)
            })
    };
    for &(a, b) in contacts.previous.iter() {
        if inactive(a) && inactive(b) {
            contacts.current.push((a, b));
        }
    }
    // Pairs come with the lower entity first.
    contacts
        .current
        .sort_unstable_by_key(|&(a, b)| (a.to_bits(), b.to_bits()));
    contacts.current.dedup();

    // Write back and refresh the grid for queries.
    let woken = &mut contacts.woken;
    woken.clear();
    for &i in contacts.touched.iter() {
        let body = &mut bodies[i as usize];
        if body.still_asleep() {
            // Nothing to write: its copy in the list is as the store keeps it, give or take
            // this step's touching faces.
            body.dirty = false;
            continue;
        }
        if body.woken {
            woken.push(body.entity);
        }
        if (body.dirty || body.woken)
            && let Ok((mut position, mut velocity, mut body_contacts, mut sync, ..)) =
                writeback.get_mut(body.entity)
        {
            if body.moved {
                position.set_if_neq(BodyPosition(body.pos));
                sync.net += body.pushed;
            }
            velocity.set_if_neq(Velocity(body.vel));
            let new = Contacts {
                faces: body.faces,
                on_ground: body.on_ground,
                ..*body_contacts
            };
            body_contacts.set_if_neq(new);
        }
        body.dirty = false;
        if body.moved {
            body.moved = false;
            body.bounds = body.end_bounds();
            grid.set_bounds(i, body.bounds);
        }
    }
    if !grid.is_built() {
        grid.build();
    }
    stats.woken += woken.len();
    for &entity in woken.iter() {
        sleepers.remove(entity);
        commands.entity(entity).try_remove::<Sleeping>();
    }
    stats.pushes = pushes;
    stats.overlaps = contacts.current.len();
    stats.solve_time += start.elapsed();
}

/// [`crate::PhysicsSystems::Events`]: `CollisionStarted` for the pairs new in this step's
/// contact set, `CollisionEnded` for the ones gone since the previous step, each sorted.
pub(crate) fn write_collisions(
    mut contacts: ResMut<ContactSet>,
    mut started: MessageWriter<CollisionStarted>,
    mut ended: MessageWriter<CollisionEnded>,
) {
    let contacts = &mut *contacts;
    let key = |&(a, b): &(Entity, Entity)| (a.to_bits(), b.to_bits());
    let (prev, curr) = (&contacts.previous, &contacts.current);
    let (mut i, mut j) = (0, 0);
    let (new, gone) = (&mut contacts.started, &mut contacts.ended);
    new.clear();
    gone.clear();
    loop {
        match (prev.get(i), curr.get(j)) {
            (Some(p), Some(c)) => match key(p).cmp(&key(c)) {
                std::cmp::Ordering::Less => {
                    gone.push(*p);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    new.push(*c);
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
            },
            (Some(p), None) => {
                gone.push(*p);
                i += 1;
            }
            (None, Some(c)) => {
                new.push(*c);
                j += 1;
            }
            (None, None) => break,
        }
    }
    if !new.is_empty() {
        started.write_batch(new.drain(..).map(|(a, b)| CollisionStarted { a, b }));
    }
    if !gone.is_empty() {
        ended.write_batch(gone.drain(..).map(|(a, b)| CollisionEnded { a, b }));
    }
    std::mem::swap(&mut contacts.previous, &mut contacts.current);
    contacts.current.clear();
}
