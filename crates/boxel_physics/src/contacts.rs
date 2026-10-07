//! Collisions between bodies (phase P3 of ADR 0016): the obstacle index of static and kinematic
//! solids, the [`BodyIndex`] (broadphase grid and snapshot for queries), the solve that pushes
//! solid bodies apart, sensor overlaps and the collision events, and the body queries
//! ([`BodyIndex::raycast`], [`BodyIndex::overlap_box`], [`BodyIndex::block_is_free`]).
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
//!
//! Sensor pairs never push: they are tested after the pushes, and the overlapping ones (by more
//! than the tolerance; touching does not count) join the step's contact set with the solid
//! pairs that carry `ReportContacts`. The events compare it with the previous step's set.

use crate::components::{
    BodyPosition, Collider, CollisionEnded, CollisionStarted, Contacts, Frozen, PhysicsSettings,
    PhysicsStats, PhysicsSync, ReportContacts, RigidBody, Velocity,
};
use crate::core::sweep::effective_eps as sweep_eps;
use crate::core::{
    BlockBounds, BodySettings, CollisionLayers, CollisionTable, Contact, Grid, LocalBox,
    MAX_RAY_DISTANCE, Shape, SweepConfig, box_box_moving, move_box, shape_box, shape_contact,
};
use crate::plugin::PhysicsContent;
use crate::source::ChunkSource;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, Face, WorldPos};
use boxel_world::{ChunkMap, WorldSettings};
use std::time::Instant;

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

/// [`crate::PhysicsSystems::Kinematic`], after kinematic bodies moved: the obstacle index.
pub(crate) fn build_obstacles(
    settings: Res<PhysicsSettings>,
    mut index: ResMut<ObstacleIndex>,
    bodies: Query<(&RigidBody, &Collider, &BodyPosition)>,
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

/// One body of the [`BodyIndex`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct IndexedBody {
    entity: Entity,
    pos: WorldPos,
    shape: Shape,
    offset: DVec3,
    layers: CollisionLayers,
    sensor: bool,
    report: bool,
    kind: RigidBody,
    frozen: bool,
    weight: f64,
    /// How far it moved in this step before the solve.
    step: DVec3,
    vel: DVec3,
    faces: u8,
    on_ground: bool,
    /// The solve changed its velocity or contacts.
    dirty: bool,
    /// The solve moved it.
    moved: bool,
}

impl IndexedBody {
    /// Its voxel box, relative to `pos`.
    fn bx(&self) -> LocalBox {
        self.shape.bounds().translate(self.offset)
    }

    /// A dynamic body physics may move (not frozen).
    fn free(&self) -> bool {
        self.kind == RigidBody::Dynamic && !self.frozen
    }

    /// The bottom of its box, exact anywhere: `(block, offset in [0, 1))`.
    fn bottom(&self) -> (i64, f64) {
        let f = self.pos.frac.y + self.bx().min.y;
        let whole = f.floor();
        (self.pos.block.y.saturating_add(whole as i64), f - whole)
    }
}

/// Every body with a [`Collider`] as of the end of the latest fixed step (after the pushes), in
/// the broadphase grid: what the entity raycast, [`BodyIndex::overlap_box`] and
/// [`BodyIndex::block_is_free`] search (also through [`crate::PhysicsQuery`]). Rebuilt in
/// `PhysicsSystems::Broadphase` every step and updated by the solve. Bodies spawned, moved or
/// despawned since the latest step are not seen until the next one. Empty before the first
/// step.
#[derive(Resource)]
pub struct BodyIndex {
    bodies: Vec<IndexedBody>,
    grid: Grid,
    pairs: Vec<(u32, u32)>,
    eps: f64,
    order: Vec<(i64, f64, u32)>,
    scratch: ObstacleScratch,
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

impl BodyIndex {
    /// Number of bodies.
    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    /// No bodies.
    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    /// The bodies, sorted by `Entity::to_bits`.
    pub fn entities(&self) -> impl Iterator<Item = Entity> + '_ {
        self.bodies.iter().map(|b| b.entity)
    }

    /// The candidate pairs of the latest broadphase, as entities (`a < b` by bits), sorted.
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
        if self.bodies.is_empty() {
            return None;
        }
        let origin = origin.normalize();
        let dir = if dir.is_finite() { dir } else { DVec3::ZERO };
        let max_t = if max_t.is_nan() {
            0.0
        } else {
            max_t.min(MAX_RAY_DISTANCE / dir.abs().max_element())
        };
        let mut best: Option<(f64, u32, DVec3)> = None;
        let test = |i: u32, best: &mut Option<(f64, u32, DVec3)>| {
            let body = &self.bodies[i as usize];
            if !filter.accepts(body) {
                return;
            }
            let center = origin.delta_to(&body.pos) + body.offset;
            if let Some(hit) = body.shape.ray(center, DVec3::ZERO, dir, max_t)
                && best.is_none_or(|(t, j, _)| hit.t < t || (hit.t == t && i < j))
            {
                *best = Some((hit.t, i, hit.normal));
            }
        };
        if self.grid.is_built() {
            for &l in self.grid.large() {
                test(l, &mut best);
            }
            for (cell, enter) in self.grid.cell_ray(origin, dir, max_t) {
                // A body hit at `t` has the hit point in a cell the ray entered by `t`.
                if best.is_some_and(|(t, ..)| enter > t) {
                    break;
                }
                for i in self.grid.cell(cell) {
                    test(i, &mut best);
                }
            }
        } else {
            for i in 0..self.bodies.len() as u32 {
                test(i, &mut best);
            }
        }
        best.map(|(t, i, normal)| BodyHit {
            entity: self.bodies[i as usize].entity,
            t,
            point: origin.offset(dir * t),
            normal,
        })
    }

    /// Calls `visit` with every body `filter` accepts that overlaps the box `bx` (relative to
    /// `pos`) by more than the contact tolerance (touching does not count), in entity order.
    fn for_each_overlap(
        &self,
        pos: WorldPos,
        bx: LocalBox,
        filter: &BodyFilter<'_>,
        mut visit: impl FnMut(&IndexedBody) -> bool,
    ) {
        if self.bodies.is_empty() {
            return;
        }
        let pos = pos.normalize();
        let mut ids = Vec::new();
        self.grid.query(BlockBounds::of_box(pos, bx), &mut ids);
        for i in ids {
            let body = &self.bodies[i as usize];
            if !filter.accepts(body) {
                continue;
            }
            let center = pos.delta_to(&body.pos) + body.offset;
            if shape_box(body.shape, center, bx).is_some_and(|c| c.depth > self.eps) && !visit(body)
            {
                return;
            }
        }
    }

    /// The bodies `filter` accepts that overlap the box `bx` (relative to `pos`) by more than
    /// the contact tolerance, sorted by entity. Spheres are tested exactly.
    pub fn overlap_box(&self, pos: WorldPos, bx: LocalBox, filter: &BodyFilter<'_>) -> Vec<Entity> {
        let mut found = Vec::new();
        self.for_each_overlap(pos, bx, filter, |body| {
            found.push(body.entity);
            true
        });
        found
    }

    /// No solid body overlaps block `block` (by more than the contact tolerance): a block placed
    /// there would not end up inside a player, a mob or a crate. Sensors (dropped items) do not
    /// count; whatever the block holds now is not checked.
    pub fn block_is_free(&self, block: BlockPos) -> bool {
        let mut free = true;
        self.for_each_overlap(
            WorldPos::from(block),
            LocalBox::new(DVec3::ZERO, DVec3::ONE),
            &BodyFilter::default(),
            |_| {
                free = false;
                false
            },
        );
        free
    }
}

/// [`crate::PhysicsSystems::Broadphase`]: the [`BodyIndex`] of every body with a collider, in
/// entity order, and its candidate pairs.
pub(crate) fn broadphase(
    settings: Res<PhysicsSettings>,
    mut index: ResMut<BodyIndex>,
    mut stats: ResMut<PhysicsStats>,
    bodies: Query<(
        Entity,
        &RigidBody,
        &Collider,
        &BodyPosition,
        &Velocity,
        &Contacts,
        &BodySettings,
        &PhysicsSync,
        Has<Frozen>,
        Has<ReportContacts>,
    )>,
) {
    let start = Instant::now();
    let index = &mut *index;
    index.eps = sweep_eps(settings.eps);
    index.bodies.clear();
    for (entity, kind, collider, position, velocity, contacts, body, sync, frozen, report) in
        &bodies
    {
        index.bodies.push(IndexedBody {
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
            dirty: false,
            moved: false,
        });
    }
    // Entity order: pairs, pushes and events come out the same on every run.
    index
        .bodies
        .sort_unstable_by_key(|body| body.entity.to_bits());
    index.grid.reset(settings.grid_shift);
    for body in &index.bodies {
        let active = body.kind == RigidBody::Dynamic || body.sensor || body.report;
        index.grid.insert(
            BlockBounds::of_box(body.pos, body.bx()),
            body.layers,
            active,
        );
    }
    index.grid.pairs(&mut index.pairs);
    stats.pairs = index.pairs.len();
    stats.solve_time = start.elapsed();
}

// ------------------------------------------------------------------------------- the solve

/// The pairs overlapping at the end of a step that report events, as entities sorted by bits,
/// and the previous step's.
#[derive(Resource, Default)]
pub(crate) struct ContactSet {
    previous: Vec<(Entity, Entity)>,
    current: Vec<(Entity, Entity)>,
    scratch: Vec<(u32, u32)>,
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
        }
        if b_free {
            b.vel -= n * (approach * share_b);
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

/// [`crate::PhysicsSystems::Solve`]: pushes solid pairs apart, finds the overlapping pairs that
/// report events, writes the changed bodies back and refreshes the index for queries.
pub(crate) fn solve(
    map: Res<ChunkMap>,
    world: Res<WorldSettings>,
    content: Res<PhysicsContent>,
    settings: Res<PhysicsSettings>,
    obstacles: Res<ObstacleIndex>,
    mut index: ResMut<BodyIndex>,
    mut contacts: ResMut<ContactSet>,
    mut stats: ResMut<PhysicsStats>,
    mut writeback: Query<(&mut BodyPosition, &mut Velocity, &mut Contacts)>,
) {
    let start = Instant::now();
    let BodyIndex {
        bodies,
        grid,
        pairs,
        eps,
        order,
        scratch,
    } = &mut *index;
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
        } else if first.report || second.report {
            // Two static or kinematic solids: no push, only events.
            if pair_contact(first, second, eps).is_some_and(|c| c.depth > eps) {
                contacts.scratch.push((a, b));
            }
        }
    }
    order.sort_unstable_by(|x, y| x.0.cmp(&y.0).then(x.1.total_cmp(&y.1)).then(x.2.cmp(&y.2)));
    let mut pushes = 0;
    for pass in 0..settings.solver_passes.max(1) {
        for &(_, _, k) in order.iter() {
            let (a, b) = pairs[k as usize];
            let (mut first, mut second) = (bodies[a as usize], bodies[b as usize]);
            let Some(contact) = pair_contact(&first, &second, eps).filter(|c| c.depth > eps) else {
                continue;
            };
            if pass == 0 && (first.report || second.report) {
                contacts.scratch.push((a, b));
            }
            if settings.solver_passes > 0
                && push_pair(&mut first, &mut second, contact, &mut push_world)
            {
                pushes += 1;
            }
            bodies[a as usize] = first;
            bodies[b as usize] = second;
        }
    }

    // Sensor pairs, at the bodies' final places.
    for &(a, b) in pairs.iter() {
        let (first, second) = (&bodies[a as usize], &bodies[b as usize]);
        if (first.sensor || second.sensor)
            && pair_contact(first, second, eps).is_some_and(|c| c.depth > eps)
        {
            contacts.scratch.push((a, b));
        }
    }
    contacts.scratch.sort_unstable();
    contacts.current.clear();
    contacts.current.extend(
        contacts
            .scratch
            .iter()
            .map(|&(a, b)| (bodies[a as usize].entity, bodies[b as usize].entity)),
    );

    // Write back and refresh the grid for queries.
    for (i, body) in bodies.iter_mut().enumerate() {
        if body.dirty
            && let Ok((mut position, mut velocity, mut body_contacts)) =
                writeback.get_mut(body.entity)
        {
            if body.moved {
                position.set_if_neq(BodyPosition(body.pos));
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
            grid.set_bounds(i as u32, BlockBounds::of_box(body.pos, body.bx()));
        }
    }
    if !grid.is_built() {
        grid.build();
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
    let mut new = Vec::new();
    let mut gone = Vec::new();
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
        started.write_batch(new.into_iter().map(|(a, b)| CollisionStarted { a, b }));
    }
    if !gone.is_empty() {
        ended.write_batch(gone.into_iter().map(|(a, b)| CollisionEnded { a, b }));
    }
    std::mem::swap(&mut contacts.previous, &mut contacts.current);
    contacts.current.clear();
}
