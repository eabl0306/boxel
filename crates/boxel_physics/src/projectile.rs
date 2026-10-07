//! Projectiles (phase P4 of ADR 0016): fast small bodies that never tunnel.
//!
//! Each step, after the bodies were pushed apart (so targets are where the step left them), a
//! projectile integrates its velocity like any body (gravity and drag, no speed limit) and
//! moves its center along the segment `velocity * dt`:
//! - against the voxels, with the exact DDA of [`crate::raycast_voxels`] from the center: no
//!   block is skipped at any speed;
//! - against the solid bodies in its layers (awake or asleep, not its `ignore` and not other
//!   projectiles), as a ray against each body's box grown by the projectile's half extents:
//!   exact for boxes, a sphere counts as its bounding box. A body the segment starts inside or
//!   touching is hit at once in the first step after the projectile was spawned, teleported or
//!   nudged (a point-blank shot); later it is not (it was hit when the projectile entered it,
//!   or the projectile stopped against it).
//!
//! The earliest hit decides (a body wins a tie with a block): `Stop` on a body leaves the
//! projectile touching it with velocity 0; `Pass` reports each body it enters and flies on; a
//! block hit puts the center on the block's face (`eps` outside it) and sticks there
//! (`OnBlock::Stop`, asleep at once) or bounces: the motion along the face's axis ends there
//! and the rest of the step goes on along the face, tested the same way (so a ball that landed
//! slides on). Every hit is a [`ProjectileHit`] (a bounce only when faster than
//! `PhysicsSettings::impact_speed` along the hit axis), with the velocity just before it.
//!
//! A stuck projectile (`OnBlock::Stop`) sleeps until its block goes, it is moved, or its
//! velocity is set: then it flies again.
//!
//! A projectile whose segment reaches an unloaded chunk freezes, like any body. Each one only
//! reads the world and writes itself, so the step runs in parallel above
//! `PhysicsSettings::parallel_threshold` with bit-identical results.

use crate::components::{
    BodyPosition, Collider, Contacts, Frozen, OnBlock, OnEntity, PhysicsSettings, PhysicsStats,
    PhysicsSync, Projectile, ProjectileHit, ProjectileTarget, RigidBody, Sleeping, Velocity,
};
use crate::contacts::{BodyIndex, IndexedBody};
use crate::core::{
    BlockBounds, BodySettings, CollisionLayers, CollisionTable, LocalBox, MAX_SWEEP_DISTANCE,
    Probe, RayResult, StepParams, VoxelFilter, VoxelSource, integrate_velocity, raycast_voxels,
};
use crate::plugin::PhysicsContent;
use crate::sleep::put_to_sleep;
use crate::source::ChunkSource;
use crate::systems::{Sink, ensure_compute_pool};
use boxel_app::FixedTime;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_voxel::{Face, WorldPos};
use boxel_world::{ChunkMap, WorldSettings};
use std::time::Instant;

/// Hits of the current step, written as messages in `PhysicsSystems::Events`: each with its
/// projectile's bits and its order within that projectile's step.
#[derive(Resource, Default)]
pub(crate) struct PendingHits(pub(crate) Vec<(u64, u8, ProjectileHit)>);

/// What the projectile step of one thread gives back.
#[derive(Default)]
pub(crate) struct FlightOut {
    hits: Vec<(u64, u8, ProjectileHit)>,
    freeze: Vec<Entity>,
    unfreeze: Vec<Entity>,
    sleep: Vec<Entity>,
    moved: usize,
    frozen: usize,
}

impl FlightOut {
    fn append(&mut self, other: &mut FlightOut) {
        self.hits.append(&mut other.hits);
        self.freeze.append(&mut other.freeze);
        self.unfreeze.append(&mut other.unfreeze);
        self.sleep.append(&mut other.sleep);
        self.moved += other.moved;
        self.frozen += other.frozen;
        other.moved = 0;
        other.frozen = 0;
    }
}

/// What every projectile of a step reads.
struct Shared<'a> {
    table: &'a CollisionTable,
    index: &'a BodyIndex,
    params: StepParams,
    eps: f64,
}

/// One thread's state: its chunk cache, scratch buffers and output (flushed into the sink when
/// the thread's batch ends).
struct Flyer<'a> {
    voxels: ChunkSource<'a>,
    ids: Vec<u32>,
    near: Vec<IndexedBody>,
    hits: Vec<(f64, u64, Entity)>,
    out: FlightOut,
    sink: &'a Sink<FlightOut>,
}

impl Drop for Flyer<'_> {
    fn drop(&mut self) {
        self.sink.with(|sink| sink.append(&mut self.out));
    }
}

/// `d` with non-finite components as 0 and each one clamped like a sweep.
fn clean(d: DVec3) -> DVec3 {
    let axis = |v: f64| {
        if v.is_finite() {
            v.clamp(-MAX_SWEEP_DISTANCE, MAX_SWEEP_DISTANCE)
        } else {
            0.0
        }
    };
    DVec3::new(axis(d.x), axis(d.y), axis(d.z))
}

/// The face of a body on `axis` facing the positive or negative side.
fn face_of(axis: usize, positive: bool) -> Face {
    match (axis, positive) {
        (0, true) => Face::PosX,
        (0, false) => Face::NegX,
        (1, true) => Face::PosY,
        (1, false) => Face::NegY,
        (_, true) => Face::PosZ,
        (_, false) => Face::NegZ,
    }
}

/// The projectile's state for one step.
struct Flight<'a, 'w> {
    entity: Entity,
    projectile: &'a mut Mut<'w, Projectile>,
    collider: Option<&'w Collider>,
    body: &'w BodySettings,
    position: &'a mut Mut<'w, BodyPosition>,
    velocity: &'a mut Mut<'w, Velocity>,
    contacts: &'a mut Mut<'w, Contacts>,
    sync: &'a mut Mut<'w, PhysicsSync>,
    frozen: bool,
}

/// One step of one projectile (see the module docs).
fn fly(flyer: &mut Flyer<'_>, shared: &Shared<'_>, f: Flight<'_, '_>) {
    let params = &shared.params;
    let start = f.position.0.normalize();
    // Moved away from its block, or launched again by the game (a stuck projectile sleeps with
    // velocity 0).
    if f.projectile.stuck.is_some() && (f.sync.teleported || f.velocity.0 != DVec3::ZERO) {
        f.projectile.stuck = None;
    }
    let freeze = |flyer: &mut Flyer<'_>| {
        flyer.out.frozen += 1;
        if !f.frozen {
            flyer.out.freeze.push(f.entity);
        }
    };
    // A stuck projectile stays while its block is there.
    if let Some(block) = f.projectile.stuck {
        match flyer.voxels.probe(block) {
            Probe::Unloaded => {
                freeze(flyer);
                return;
            }
            Probe::Block(state) if shared.table.collides(state) => {
                if f.frozen {
                    flyer.out.unfreeze.push(f.entity);
                }
                flyer.out.sleep.push(f.entity);
                return;
            }
            Probe::Block(_) => f.projectile.stuck = None,
        }
    }
    let v = integrate_velocity(f.velocity.0, f.contacts.on_ground, f.body, params);
    let d = clean(v * params.dt);
    let (offset, half, layers) = match f.collider {
        Some(c) => (c.offset, c.shape.bounds().max, c.layers),
        None => (DVec3::ZERO, DVec3::ZERO, CollisionLayers::default()),
    };
    let bits = f.entity.to_bits();
    let mut seq = 0u8;
    let mut report =
        |flyer: &mut Flyer<'_>, target: ProjectileTarget, point: WorldPos, v: DVec3| {
            flyer.out.hits.push((
                bits,
                seq,
                ProjectileHit {
                    projectile: f.entity,
                    target,
                    point,
                    velocity: v,
                    speed: v.length(),
                },
            ));
            seq = seq.saturating_add(1);
        };
    // Everything in the frame of the start's block: the position is `frac`, the center
    // `frac + offset`.
    let frame = WorldPos::from_block(start.block, DVec3::ZERO);
    let mut frac = start.frac;
    let mut contacts = Contacts::default();
    let mut vel = v;
    let mut stuck = false;
    // The motion left in this step. A bounce off a face drops the motion along its axis and
    // goes on along the face with the rest: at most one leg per axis.
    let mut rem = d;
    for leg in 0..3 {
        if rem == DVec3::ZERO {
            break;
        }
        let center = frac + offset;
        let origin = WorldPos::from_block(start.block, center);
        let (t_end, block_hit) = match raycast_voxels(
            &mut flyer.voxels,
            shared.table,
            origin,
            rem,
            1.0,
            VoxelFilter::Collision,
        ) {
            RayResult::Hit(hit) => (hit.t, Some(hit)),
            RayResult::Miss => (1.0, None),
            RayResult::Unloaded { .. } if leg == 0 => {
                freeze(flyer);
                return;
            }
            // The slide along a face reaches an unloaded chunk: it ends where the bounce left
            // it, and the next step freezes it.
            RayResult::Unloaded { .. } => break,
        };
        if leg == 0 && f.frozen {
            flyer.out.unfreeze.push(f.entity);
        }

        // Bodies along the leg up to the block. A body it starts inside or touching is only hit
        // in the first step after it was placed (spawned, teleported or nudged): later it is
        // one it entered or stopped against already.
        let fresh = leg == 0 && f.sync.teleported;
        let reach = rem * t_end;
        let from = LocalBox::from_center_half(center, half);
        let to = from.translate(reach);
        let swept = LocalBox::new(from.min.min(to.min), from.max.max(to.max));
        flyer.hits.clear();
        shared.index.near(
            BlockBounds::of_box(frame, swept),
            &mut flyer.ids,
            &mut flyer.near,
        );
        for target in &flyer.near {
            if !target.collides
                || target.sensor
                || target.entity == f.entity
                || Some(target.entity) == f.projectile.ignore
                || !layers.interacts(target.layers)
            {
                continue;
            }
            let at = frame.delta_to(&target.pos) + target.offset;
            let grown = LocalBox::from_center_half(at, target.shape.bounds().max + half);
            if let Some(hit) = grown.ray(center, rem, t_end)
                && (hit.t > 0.0 || fresh)
            {
                flyer
                    .hits
                    .push((hit.t, target.entity.to_bits(), target.entity));
            }
        }
        flyer
            .hits
            .sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let entity_hits = std::mem::take(&mut flyer.hits);
        let stopped = match f.projectile.on_entity {
            OnEntity::Stop => entity_hits.first().copied(),
            OnEntity::Pass => {
                for &(t, _, target) in &entity_hits {
                    let point = WorldPos::from_block(start.block, center + rem * t);
                    report(flyer, ProjectileTarget::Entity(target), point, vel);
                }
                None
            }
        };
        flyer.hits = entity_hits;
        if let Some((t, _, target)) = stopped {
            let point = WorldPos::from_block(start.block, center + rem * t);
            report(flyer, ProjectileTarget::Entity(target), point, vel);
            frac += rem * t;
            vel = DVec3::ZERO;
            break;
        }
        let Some(hit) = block_hit else {
            frac += rem;
            break;
        };
        let Some(axis) = hit.face.map(|face| face.axes().0) else {
            // It started inside a block (spawned there, or a block was placed on it): it
            // sticks in that block whatever its mode.
            report(
                flyer,
                ProjectileTarget::Block {
                    pos: hit.block,
                    face: None,
                    state: hit.state,
                },
                hit.point,
                vel,
            );
            vel = DVec3::ZERO;
            f.projectile.stuck = Some(hit.block);
            stuck = true;
            break;
        };
        // The center on the face's plane, `eps` back on the side it came from.
        let positive = rem[axis] > 0.0;
        let plane = (hit.point.block.as_i64vec3()[axis] - start.block.as_i64vec3()[axis]) as f64;
        let back = if positive { -shared.eps } else { shared.eps };
        let along_rem = rem * hit.t;
        frac += along_rem;
        frac[axis] = plane + back - offset[axis];
        let face = face_of(axis, positive);
        contacts.faces |= 1 << face.index();
        if face == Face::NegY {
            contacts.on_ground = true;
            contacts.ground_block = Some(hit.block);
        }
        let along = vel[axis].abs();
        let target = ProjectileTarget::Block {
            pos: hit.block,
            face: hit.face,
            state: hit.state,
        };
        let point = WorldPos::from_block(start.block, frac + offset);
        match f.projectile.on_block {
            OnBlock::Stop => {
                report(flyer, target, point, vel);
                vel = DVec3::ZERO;
                f.projectile.stuck = Some(hit.block);
                stuck = true;
                break;
            }
            OnBlock::Bounce => {
                if along > params.impact_speed {
                    report(flyer, target, point, vel);
                }
                // As a body's bounce: a clip no faster than one step of gravity (or
                // `bounce_min_speed`) stops instead (`max` ignores a NaN side).
                let resting = (params.gravity[axis] * f.body.gravity_scale * params.dt).abs();
                let bounced = -vel[axis] * f.body.restitution;
                vel[axis] = if along > f.body.bounce_min_speed.max(resting)
                    && bounced != 0.0
                    && bounced.is_finite()
                {
                    bounced
                } else {
                    0.0
                };
                // The rest of the step, along the face.
                rem -= along_rem;
                rem[axis] = 0.0;
            }
        }
    }
    let end = WorldPos::from_block(start.block, frac);
    let moved = frac - start.frac;
    if let Probe::Block(state) = flyer
        .voxels
        .probe(WorldPos::from_block(end.block, end.frac + offset).block)
    {
        contacts.in_fluid = shared.table.is_fluid(state);
    }
    flyer.out.moved += 1;
    f.position.set_if_neq(BodyPosition(end));
    f.velocity.set_if_neq(Velocity(vel));
    f.contacts.set_if_neq(contacts);
    if f.sync.moved != moved {
        f.sync.moved = moved;
    }
    f.sync.net = moved;
    if stuck {
        flyer.out.sleep.push(f.entity);
    }
}

/// [`crate::PhysicsSystems::Projectiles`]: one step of every awake projectile (see the module
/// docs), on several threads above `PhysicsSettings::parallel_threshold`.
pub(crate) fn step_projectiles(
    mut commands: Commands,
    map: Res<ChunkMap>,
    world: Res<WorldSettings>,
    content: Res<PhysicsContent>,
    settings: Res<PhysicsSettings>,
    fixed: Res<FixedTime>,
    index: Res<BodyIndex>,
    mut projectiles: Query<
        (
            Entity,
            &RigidBody,
            &mut Projectile,
            Option<&Collider>,
            &BodySettings,
            &mut BodyPosition,
            &mut Velocity,
            &mut Contacts,
            &mut PhysicsSync,
            Has<Frozen>,
        ),
        Without<Sleeping>,
    >,
    mut pending: ResMut<PendingHits>,
    mut stats: ResMut<PhysicsStats>,
) {
    let start = Instant::now();
    let shared = Shared {
        table: &content.collision,
        index: &index,
        params: settings
            .step_params(fixed.timestep().as_secs_f64())
            .without_speed_limit(),
        eps: crate::core::sweep::effective_eps(settings.eps),
    };
    let sink = Sink::<FlightOut>::default();
    let new_flyer = || Flyer {
        voxels: ChunkSource::new(&map, &world),
        ids: Vec::new(),
        near: Vec::new(),
        hits: Vec::new(),
        out: FlightOut::default(),
        sink: &sink,
    };
    let step = |flyer: &mut Flyer<'_>, item: ProjectileItem<'_>| {
        let (
            entity,
            kind,
            mut projectile,
            collider,
            body,
            mut position,
            mut velocity,
            mut contacts,
            mut sync,
            frozen,
        ) = item;
        if *kind != RigidBody::Dynamic {
            return;
        }
        fly(
            flyer,
            &shared,
            Flight {
                entity,
                projectile: &mut projectile,
                collider,
                body,
                position: &mut position,
                velocity: &mut velocity,
                contacts: &mut contacts,
                sync: &mut sync,
                frozen,
            },
        );
    };
    let parallel = projectiles.iter().len() > settings.parallel_threshold;
    if parallel {
        ensure_compute_pool();
        projectiles.par_iter_mut().for_each_init(new_flyer, step);
    } else {
        let mut flyer = new_flyer();
        for item in &mut projectiles {
            step(&mut flyer, item);
        }
    }
    let mut out = sink.into_inner();
    out.freeze.sort_unstable_by_key(|e| e.to_bits());
    out.unfreeze.sort_unstable_by_key(|e| e.to_bits());
    out.sleep.sort_unstable_by_key(|e| e.to_bits());
    for entity in out.freeze {
        commands.entity(entity).try_insert(Frozen);
    }
    for entity in out.unfreeze {
        commands.entity(entity).try_remove::<Frozen>();
    }
    for entity in out.sleep {
        put_to_sleep(&mut commands, entity);
    }
    stats.projectiles = out.moved + out.frozen;
    stats.dynamic += out.moved + out.frozen;
    stats.frozen += out.frozen;
    stats.parallel |= parallel;
    pending.0.append(&mut out.hits);
    stats.step_time += start.elapsed();
}

/// One item of the projectile query.
type ProjectileItem<'a> = (
    Entity,
    &'a RigidBody,
    Mut<'a, Projectile>,
    Option<&'a Collider>,
    &'a BodySettings,
    Mut<'a, BodyPosition>,
    Mut<'a, Velocity>,
    Mut<'a, Contacts>,
    Mut<'a, PhysicsSync>,
    bool,
);

/// [`crate::PhysicsSystems::Events`]: the step's projectile hits, sorted by projectile, each
/// projectile's in the order it hit.
pub(crate) fn write_hits(
    mut pending: ResMut<PendingHits>,
    mut hits: MessageWriter<ProjectileHit>,
    mut stats: ResMut<PhysicsStats>,
) {
    stats.hits = pending.0.len();
    if pending.0.is_empty() {
        return;
    }
    // (projectile, order) is unique.
    pending
        .0
        .sort_unstable_by_key(|&(bits, seq, _)| (bits, seq));
    hits.write_batch(pending.0.drain(..).map(|(_, _, hit)| hit));
}
