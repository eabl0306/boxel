//! The per-step systems of physics bodies (phases P2 to P4) and their sets.

use crate::components::{
    BodyPosition, CharacterController, Collider, Contacts, Frozen, Interpolated, MoveIntent,
    PhysicsSettings, PhysicsStats, PhysicsSync, Projectile, RigidBody, Sleeping, Teleport, Vacated,
    Velocity, VoxelImpact,
};
use crate::contacts::{ObstacleIndex, ObstacleScratch};
use crate::core::{
    BodySettings, CollisionTable, LocalBox, MAX_SWEEP_DISTANCE, StepParams, integrate_velocity,
    move_body,
};
use crate::plugin::PhysicsContent;
use crate::source::ChunkSource;
use boxel_app::FixedTime;
use boxel_diagnostic::DiagnosticsBoard;
use boxel_ecs::prelude::*;
use boxel_math::{DVec2, DVec3};
use boxel_tasks::{ComputeTaskPool, TaskPool};
use boxel_transform::Transform;
use boxel_voxel::WorldPos;
use boxel_world::{ChunkMap, WorldSettings};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// The physics systems, chained in this order in `FixedPostUpdate` (after the game's
/// `FixedUpdate`, so gameplay never orders itself against physics), except
/// [`PhysicsSystems::Interpolate`], which runs once per frame in
/// `RunFixedMainLoopSystems::AfterFixedMainLoop`.
///
/// Gameplay rule: steer bodies through [`Velocity`] and [`MoveIntent`]; writing `Transform`
/// is a teleport.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhysicsSystems {
    /// New bodies take their position from `Transform`; teleports (a [`Teleport`], or a
    /// `Transform` that differs from what physics wrote) move [`BodyPosition`], clear
    /// [`Contacts`] and wake the body; [`Interpolated`] bodies keep their position before the
    /// step.
    Prepare,
    /// [`CharacterController`]s turn [`MoveIntent`] into [`Velocity`]. After `Prepare`, so a
    /// body teleported this step is in the air for its controller (no jump from the old
    /// ground).
    Controller,
    /// Sleeping bodies that something disturbed wake up (see [`Sleeping`]): a velocity, a
    /// teleport, a body that left, a block change, a chunk load or unload.
    Wake,
    /// Kinematic bodies move by their velocity; then the static and kinematic solid colliders
    /// are indexed as obstacles for the dynamic bodies.
    Kinematic,
    /// Dynamic bodies move: gravity, drag, the sweep against the voxels and the obstacles,
    /// bounces, freezing in unloaded chunks. On several threads above
    /// `PhysicsSettings::parallel_threshold` awake bodies.
    Step,
    /// The broadphase: every awake body with a collider (and the sleepers near the active ones)
    /// goes into the grid of [`crate::BodyIndex`], which gives the candidate pairs.
    Broadphase,
    /// Solid bodies are pushed apart; sensor overlaps (and solid contacts with
    /// [`crate::ReportContacts`]) are collected; pushed sleepers and sleepers that lost their
    /// support wake up.
    Solve,
    /// [`crate::Projectile`]s fly: segments against the voxels and the bodies where the solve
    /// left them.
    Projectiles,
    /// Bodies that stayed still long enough fall asleep.
    Sleep,
    /// The step's messages ([`VoxelImpact`], [`crate::CollisionStarted`],
    /// [`crate::CollisionEnded`], [`crate::ProjectileHit`]), sorted by entity.
    Events,
    /// `Transform` follows [`BodyPosition`].
    Writeback,
    /// Interpolated `Transform`s, after the frame's fixed steps.
    Interpolate,
}

/// Impacts of the current step, written as messages in [`PhysicsSystems::Events`].
#[derive(Resource, Default)]
pub(crate) struct PendingImpacts(Vec<VoxelImpact>);

/// The outputs of the threads of a parallel step, merged under a lock (once per batch of
/// bodies, when the thread's state is dropped).
pub(crate) struct Sink<T>(Mutex<T>);

impl<T: Default> Default for Sink<T> {
    fn default() -> Self {
        Self(Mutex::new(T::default()))
    }
}

impl<T> Sink<T> {
    /// Runs `f` on the merged output.
    pub(crate) fn with(&self, f: impl FnOnce(&mut T)) {
        f(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// The merged output.
    pub(crate) fn into_inner(self) -> T {
        self.0.into_inner().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Outputs that the threads of a parallel system merge.
pub(crate) trait Merge {
    /// Moves `other`'s output into `self` (leaving `other` empty).
    fn merge(&mut self, other: &mut Self);
}

/// One thread's share of a parallel system's output, merged into the sink when the thread's
/// batch of bodies ends (when it is dropped).
pub(crate) struct Part<'a, T: Merge> {
    pub(crate) out: T,
    sink: &'a Sink<T>,
}

impl<'a, T: Merge + Default> Part<'a, T> {
    pub(crate) fn new(sink: &'a Sink<T>) -> Self {
        Self {
            out: T::default(),
            sink,
        }
    }
}

impl<T: Merge> Drop for Part<'_, T> {
    fn drop(&mut self) {
        self.sink.with(|sink| sink.merge(&mut self.out));
    }
}

/// The compute pool a parallel step runs on (`TaskPoolPlugin` creates it; tests and tools
/// without it get a default one).
pub(crate) fn ensure_compute_pool() {
    ComputeTaskPool::get_or_init(TaskPool::default);
}

/// The fixed timestep in seconds, in `f64` (never through `Time::delta_secs`, which is `f32`).
fn timestep(fixed: &FixedTime) -> f64 {
    fixed.timestep().as_secs_f64()
}

/// `d` with non-finite components as 0 and each one clamped like a sweep (kinematic moves).
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

/// [`PhysicsSystems::Controller`]: walking and jumping (see [`CharacterController`]). Only
/// dynamic bodies walk (the others have no drag to level the speed off). Frozen bodies are
/// skipped (their steps do not happen) and keep the jump latch. A sleeping character that
/// wants to move gets a velocity, and that wakes it. Each character only writes itself: on the
/// compute threads above `PhysicsSettings::parallel_threshold`.
pub(crate) fn drive_characters(
    fixed: Res<FixedTime>,
    settings: Res<PhysicsSettings>,
    mut characters: Query<
        (
            &RigidBody,
            &CharacterController,
            &mut MoveIntent,
            &mut Velocity,
            &Contacts,
            &BodySettings,
        ),
        Without<Frozen>,
    >,
) {
    let dt = timestep(&fixed);
    let drive = |(kind, controller, mut intent, mut velocity, contacts, body): (
        &RigidBody,
        &CharacterController,
        Mut<MoveIntent>,
        Mut<Velocity>,
        &Contacts,
        &BodySettings,
    )| {
        if *kind != RigidBody::Dynamic {
            return;
        }
        let mut wish = intent.wish.as_dvec2();
        if !wish.is_finite() {
            wish = DVec2::ZERO;
        }
        let length_sq = wish.length_squared();
        if length_sq > 1.0 {
            wish /= length_sq.sqrt();
        }
        let (rate, control) = if contacts.on_ground {
            (body.ground_friction, 1.0)
        } else {
            (body.drag_xz, controller.air_control)
        };
        // With drag `rate`, an acceleration `walk_speed * rate` levels off at `walk_speed`.
        let accel = controller.walk_speed * rate * control * dt;
        let mut v = velocity.0;
        v.x += wish.x * accel;
        v.z += wish.y * accel;
        if intent.jump {
            intent.jump = false;
            if contacts.on_ground {
                v.y = controller.jump_speed;
            }
        }
        velocity.set_if_neq(Velocity(v));
    };
    if characters.iter().len() > settings.parallel_threshold {
        ensure_compute_pool();
        characters.par_iter_mut().for_each(drive);
    } else {
        characters.iter_mut().for_each(drive);
    }
}

/// What [`prepare_bodies`] reads and writes of a body.
type PrepareItem<'a> = (
    Entity,
    &'a Transform,
    &'a mut BodyPosition,
    &'a mut Contacts,
    &'a mut PhysicsSync,
    Option<&'a Teleport>,
    Option<&'a mut Interpolated>,
    Option<&'a Collider>,
    Has<Projectile>,
);

/// What [`prepare_bodies`] collects from the bodies (per thread, then merged).
#[derive(Default)]
pub(crate) struct Prepared {
    /// Bodies whose `Teleport` was used.
    teleports: Vec<Entity>,
    /// Sleeping bodies teleported (they wake up).
    wake: Vec<Entity>,
    /// Places solid bodies left by a teleport.
    vacated: Vec<(WorldPos, LocalBox)>,
    count: usize,
}

impl Merge for Prepared {
    fn merge(&mut self, other: &mut Self) {
        self.teleports.append(&mut other.teleports);
        self.wake.append(&mut other.wake);
        self.vacated.append(&mut other.vacated);
        self.count += std::mem::take(&mut other.count);
    }
}

/// [`PhysicsSystems::Prepare`]. A teleport clears [`Contacts`] (the body is not on its old
/// ground any more: the next step finds the new one), wakes a sleeping body, and records the
/// place a solid body left (the bodies asleep on it wake up). Sleeping bodies are only looked
/// at when their `Transform` changed or they have a [`Teleport`]. Each body only writes itself:
/// the awake ones are prepared on the compute threads above
/// `PhysicsSettings::parallel_threshold`.
pub(crate) fn prepare_bodies(
    mut commands: Commands,
    settings: Res<PhysicsSettings>,
    mut awake: Query<PrepareItem, Without<Sleeping>>,
    mut asleep: Query<PrepareItem, (With<Sleeping>, Or<(Changed<Transform>, With<Teleport>)>)>,
    sleeping: Query<(), (With<RigidBody>, With<Sleeping>)>,
    mut vacated: ResMut<Vacated>,
    mut stats: ResMut<PhysicsStats>,
) {
    let sink = Sink::<Prepared>::default();
    if awake.iter().len() > settings.parallel_threshold {
        ensure_compute_pool();
        awake.par_iter_mut().for_each_init(
            || Part::new(&sink),
            |part, item| prepare(&mut part.out, item, false),
        );
    } else {
        let mut part = Part::new(&sink);
        for item in &mut awake {
            prepare(&mut part.out, item, false);
        }
    }
    let mut out = sink.into_inner();
    for item in &mut asleep {
        prepare(&mut out, item, true);
    }
    out.teleports.sort_unstable_by_key(|e| e.to_bits());
    out.wake.sort_unstable_by_key(|e| e.to_bits());
    // `try_`: a command of another system may have despawned them at this sync point.
    for entity in out.teleports {
        commands.entity(entity).try_remove::<Teleport>();
    }
    for entity in out.wake {
        commands.entity(entity).try_remove::<Sleeping>();
    }
    vacated.0.append(&mut out.vacated);
    stats.bodies = out.count + sleeping.iter().len();
}

/// [`prepare_bodies`] for one body.
fn prepare(
    out: &mut Prepared,
    (
        entity,
        transform,
        mut position,
        mut contacts,
        mut sync,
        teleport,
        interpolated,
        collider,
        projectile,
    ): (
        Entity,
        &Transform,
        Mut<BodyPosition>,
        Mut<Contacts>,
        Mut<PhysicsSync>,
        Option<&Teleport>,
        Option<Mut<Interpolated>>,
        Option<&Collider>,
        bool,
    ),
    asleep: bool,
) {
    if !asleep {
        out.count += 1;
        if sync.moved != DVec3::ZERO {
            sync.moved = DVec3::ZERO;
        }
        if sync.net != DVec3::ZERO {
            sync.net = DVec3::ZERO;
        }
        if sync.teleported {
            sync.teleported = false;
        }
    }
    let target = if let Some(teleport) = teleport {
        out.teleports.push(entity);
        Some(teleport.0.normalize())
    } else if sync.last_written != Some(transform.translation) {
        // A new body, or gameplay wrote `Transform`: start from it.
        Some(WorldPos::from_dvec3(transform.translation))
    } else {
        None
    };
    if let Some(target) = target {
        // Nothing rests on a sensor or a projectile.
        if let Some(collider) = collider
            && !collider.is_sensor()
            && !projectile
            && sync.last_written.is_some()
        {
            out.vacated.push((position.0, collider.voxel_box()));
        }
        position.set_if_neq(BodyPosition(target));
        contacts.set_if_neq(Contacts::default());
        sync.teleported = true;
        sync.still = 0;
        if asleep {
            out.wake.push(entity);
        }
    }
    if let Some(mut interpolated) = interpolated
        && (!asleep || target.is_some())
    {
        interpolated.prev = Some(position.0);
    }
}

/// [`PhysicsSystems::Kinematic`]: `position += velocity * dt`, through everything.
pub(crate) fn move_kinematic(
    fixed: Res<FixedTime>,
    mut bodies: Query<
        (&RigidBody, &Velocity, &mut BodyPosition, &mut PhysicsSync),
        Without<Sleeping>,
    >,
) {
    let dt = timestep(&fixed);
    for (kind, velocity, mut position, mut sync) in &mut bodies {
        if *kind != RigidBody::Kinematic {
            continue;
        }
        let d = clean(velocity.0 * dt);
        if d != DVec3::ZERO {
            let p = position.0.normalize();
            position.0 = WorldPos::from_block(p.block, p.frac + d);
            sync.moved = d;
            sync.net = d;
        }
    }
}

/// What the step of one thread gives back.
#[derive(Default)]
pub(crate) struct StepOut {
    impacts: Vec<VoxelImpact>,
    freeze: Vec<Entity>,
    unfreeze: Vec<Entity>,
    dynamic: usize,
    frozen: usize,
}

impl StepOut {
    fn append(&mut self, other: &mut StepOut) {
        self.impacts.append(&mut other.impacts);
        self.freeze.append(&mut other.freeze);
        self.unfreeze.append(&mut other.unfreeze);
        self.dynamic += other.dynamic;
        self.frozen += other.frozen;
        other.dynamic = 0;
        other.frozen = 0;
    }
}

/// One thread's state in the step: its chunk cache, the obstacle scratch and its output
/// (flushed into the sink when the thread's batch ends).
struct Stepper<'a> {
    voxels: ChunkSource<'a>,
    scratch: ObstacleScratch,
    out: StepOut,
    sink: &'a Sink<StepOut>,
}

impl Drop for Stepper<'_> {
    fn drop(&mut self) {
        self.sink.with(|sink| sink.append(&mut self.out));
    }
}

/// What every body of a step reads.
struct StepShared<'a> {
    table: &'a CollisionTable,
    obstacles: &'a ObstacleIndex,
    params: StepParams,
}

/// One item of the step's query.
type StepItem<'a> = (
    Entity,
    &'a RigidBody,
    Option<&'a Collider>,
    &'a BodySettings,
    Mut<'a, BodyPosition>,
    Mut<'a, Velocity>,
    Mut<'a, Contacts>,
    Mut<'a, PhysicsSync>,
    bool,
);

/// One step of one dynamic body (it reads the world and writes only itself).
fn step_one(stepper: &mut Stepper<'_>, shared: &StepShared<'_>, item: StepItem<'_>) {
    let (
        entity,
        kind,
        collider,
        body,
        mut position,
        mut velocity,
        mut contacts,
        mut sync,
        was_frozen,
    ) = item;
    if *kind != RigidBody::Dynamic {
        return;
    }
    stepper.out.dynamic += 1;
    let params = &shared.params;
    let shape = collider.map_or(LocalBox::default(), Collider::voxel_box);
    let pos = position.0.normalize();
    let v = integrate_velocity(velocity.0, contacts.on_ground, body, params);
    // Solid colliders sweep against the static and kinematic solids too.
    let blockers = match collider {
        Some(collider) if !collider.is_sensor() && !shared.obstacles.is_empty() => stepper
            .scratch
            .gather(shared.obstacles, pos, shape, v * params.dt, collider.layers),
        _ => &[],
    };
    match move_body(
        &mut stepper.voxels,
        shared.table,
        pos,
        v,
        shape,
        blockers,
        body,
        params,
    ) {
        Ok(out) => {
            if out.sweep.moved != DVec3::ZERO {
                sync.moved = out.sweep.moved;
                sync.net = out.sweep.moved;
            }
            position.set_if_neq(BodyPosition(out.pos));
            velocity.set_if_neq(Velocity(out.vel));
            contacts.set_if_neq(Contacts::from_sweep(&out.sweep));
            stepper
                .out
                .impacts
                .extend(out.impacts().map(|impact| VoxelImpact {
                    entity,
                    face: impact.face,
                    block: impact.block,
                    speed: impact.speed,
                }));
            if was_frozen {
                stepper.out.unfreeze.push(entity);
            }
        }
        Err(_) => {
            stepper.out.frozen += 1;
            if !was_frozen {
                stepper.out.freeze.push(entity);
            }
        }
    }
}

/// [`PhysicsSystems::Step`]: one step of every awake dynamic body ([`crate::step_body`], plus
/// the static and kinematic solid obstacles for a solid body). A body whose step needs an
/// unloaded block keeps its state and gets [`Frozen`]. Above
/// `PhysicsSettings::parallel_threshold` bodies it runs on the compute threads: each body reads
/// the world and writes only itself, and the outputs are merged in entity order, so the results
/// are bit-identical to a run on one thread.
pub(crate) fn step_dynamic(
    mut commands: Commands,
    map: Res<ChunkMap>,
    world: Res<WorldSettings>,
    content: Res<PhysicsContent>,
    settings: Res<PhysicsSettings>,
    fixed: Res<FixedTime>,
    obstacles: Res<ObstacleIndex>,
    mut bodies: Query<
        (
            Entity,
            &RigidBody,
            Option<&Collider>,
            &BodySettings,
            &mut BodyPosition,
            &mut Velocity,
            &mut Contacts,
            &mut PhysicsSync,
            Has<Frozen>,
        ),
        (Without<Sleeping>, Without<Projectile>),
    >,
    mut pending: ResMut<PendingImpacts>,
    mut stats: ResMut<PhysicsStats>,
) {
    let start = Instant::now();
    let shared = StepShared {
        table: &content.collision,
        obstacles: &obstacles,
        params: settings.step_params(timestep(&fixed)),
    };
    let sink = Sink::<StepOut>::default();
    // One chunk cache per thread: consecutive bodies often share chunks.
    let new_stepper = || Stepper {
        voxels: ChunkSource::new(&map, &world),
        scratch: ObstacleScratch::default(),
        out: StepOut::default(),
        sink: &sink,
    };
    let parallel = bodies.iter().len() > settings.parallel_threshold;
    if parallel {
        ensure_compute_pool();
        bodies
            .par_iter_mut()
            .for_each_init(new_stepper, |stepper, item| {
                step_one(stepper, &shared, item);
            });
    } else {
        let mut stepper = new_stepper();
        for item in &mut bodies {
            step_one(&mut stepper, &shared, item);
        }
    }
    let mut out = sink.into_inner();
    out.freeze.sort_unstable_by_key(|e| e.to_bits());
    out.unfreeze.sort_unstable_by_key(|e| e.to_bits());
    // `try_`: another system's command applied at the same sync point may despawn the body
    // first.
    for entity in out.freeze {
        commands.entity(entity).try_insert(Frozen);
    }
    for entity in out.unfreeze {
        commands.entity(entity).try_remove::<Frozen>();
    }
    pending.0.append(&mut out.impacts);
    stats.dynamic = out.dynamic;
    stats.frozen = out.frozen;
    stats.parallel = parallel;
    stats.steps += 1;
    stats.step_time = start.elapsed();
}

/// [`PhysicsSystems::Events`]: the step's impacts as messages, sorted by entity (each body's in
/// axis order X, Y, Z).
pub(crate) fn write_events(
    mut pending: ResMut<PendingImpacts>,
    mut impacts: MessageWriter<VoxelImpact>,
    mut stats: ResMut<PhysicsStats>,
) {
    stats.impacts = pending.0.len();
    if pending.0.is_empty() {
        return;
    }
    // A body has at most one impact per axis, so the key is unique: an unstable sort (no
    // scratch allocation) gives one order, axis X, Y, Z within each body.
    pending
        .0
        .sort_unstable_by_key(|impact| (impact.entity.to_bits(), impact.face.axes().0));
    impacts.write_batch(pending.0.drain(..));
}

/// [`PhysicsSystems::Writeback`]: `Transform.translation = BodyPosition` (the only rounding of
/// a position), and [`PhysicsSync`] remembers it. Rotation is never touched. Sleeping bodies
/// are skipped: falling asleep already put their `Transform` on their position. On the compute
/// threads above `PhysicsSettings::parallel_threshold` bodies.
pub(crate) fn write_back(
    settings: Res<PhysicsSettings>,
    mut bodies: Query<(&BodyPosition, &mut Transform, &mut PhysicsSync), Without<Sleeping>>,
) {
    let write =
        |(position, mut transform, mut sync): (&BodyPosition, Mut<Transform>, Mut<PhysicsSync>)| {
            let translation = position.0.to_dvec3();
            if transform.translation != translation {
                transform.translation = translation;
            }
            if sync.last_written != Some(translation) {
                sync.last_written = Some(translation);
            }
        };
    if bodies.iter().len() > settings.parallel_threshold {
        ensure_compute_pool();
        bodies.par_iter_mut().for_each(write);
    } else {
        bodies.iter_mut().for_each(write);
    }
}

/// [`PhysicsSystems::Interpolate`]: `Transform = prev + (curr - prev) * alpha` for
/// [`Interpolated`] bodies, with `alpha = FixedTime::overstep_fraction_f64`. A body whose
/// `Transform` gameplay changed since physics wrote it is left alone (the next step teleports
/// it), and so is a sleeping one (it does not move). Without `TimePlugin` (no `FixedTime`) no
/// step ever runs, and neither does this.
pub(crate) fn interpolate(
    fixed: Option<Res<FixedTime>>,
    mut bodies: Query<
        (
            &Interpolated,
            &BodyPosition,
            &mut Transform,
            &mut PhysicsSync,
        ),
        Without<Sleeping>,
    >,
) {
    let Some(fixed) = fixed else {
        return;
    };
    let alpha = fixed.overstep_fraction_f64().clamp(0.0, 1.0);
    for (interpolated, position, mut transform, mut sync) in &mut bodies {
        let (Some(last), Some(prev)) = (sync.last_written, interpolated.prev) else {
            continue;
        };
        if transform.translation != last {
            continue;
        }
        let translation = prev.to_dvec3() + prev.delta_to(&position.0) * alpha;
        if translation != last {
            transform.translation = translation;
            sync.last_written = Some(translation);
        }
    }
}

/// The `"physics"` line of `DiagnosticsBoard` (when present), refreshed when the stats change.
pub(crate) fn publish_stats(stats: Res<PhysicsStats>, board: Option<ResMut<DiagnosticsBoard>>) {
    let Some(mut board) = board else {
        return;
    };
    if stats.is_changed() || board.get("physics").is_none() {
        board.set("physics", stats.summary());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_ecs::message::Messages;
    use boxel_ecs::system::RunSystemOnce;
    use boxel_voxel::{BlockPos, Face};

    #[test]
    fn impacts_are_written_by_entity_then_axis() {
        let mut world = World::new();
        let (a, b) = (world.spawn_empty().id(), world.spawn_empty().id());
        // `a` sorts first.
        let (a, b) = if a.to_bits() < b.to_bits() {
            (a, b)
        } else {
            (b, a)
        };
        let impact = |entity, face| VoxelImpact {
            entity,
            face,
            block: BlockPos::new(1, 2, 3),
            speed: 5.0,
        };
        world.insert_resource(PendingImpacts(vec![
            impact(b, Face::NegY),
            impact(a, Face::PosZ),
            impact(b, Face::PosX),
            impact(a, Face::NegY),
            impact(a, Face::NegX),
        ]));
        world.init_resource::<Messages<VoxelImpact>>();
        world.init_resource::<PhysicsStats>();
        world.run_system_once(write_events).expect("runs");
        let written: Vec<_> = world
            .resource::<Messages<VoxelImpact>>()
            .iter_current_update_messages()
            .map(|i| (i.entity, i.face))
            .collect();
        assert_eq!(
            written,
            [
                (a, Face::NegX),
                (a, Face::NegY),
                (a, Face::PosZ),
                (b, Face::PosX),
                (b, Face::NegY),
            ]
        );
        assert!(world.resource::<PendingImpacts>().0.is_empty());
        assert_eq!(world.resource::<PhysicsStats>().impacts, 5);
    }
}
