//! The per-step systems of physics bodies (phase P2) and their sets.

use crate::components::{
    BodyPosition, CharacterController, Collider, Contacts, Frozen, Interpolated, MoveIntent,
    PhysicsSettings, PhysicsStats, PhysicsSync, RigidBody, Teleport, Velocity, VoxelImpact,
};
use crate::core::{BodySettings, LocalBox, MAX_SWEEP_DISTANCE, step_body};
use crate::plugin::PhysicsContent;
use crate::source::ChunkSource;
use boxel_app::FixedTime;
use boxel_diagnostic::DiagnosticsBoard;
use boxel_ecs::prelude::*;
use boxel_math::{DVec2, DVec3};
use boxel_transform::Transform;
use boxel_voxel::WorldPos;
use boxel_world::{ChunkMap, WorldSettings};
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
    /// [`CharacterController`]s turn [`MoveIntent`] into [`Velocity`].
    Controller,
    /// New bodies take their position from `Transform`; teleports (a [`Teleport`], or a
    /// `Transform` that differs from what physics wrote) move [`BodyPosition`]; [`Interpolated`]
    /// bodies keep their position before the step.
    Prepare,
    /// Kinematic bodies move by their velocity.
    Kinematic,
    /// Dynamic bodies move: gravity, drag, the voxel sweep, bounces, freezing in unloaded
    /// chunks.
    Step,
    /// Collision pairs between bodies (phase P3; empty today).
    Broadphase,
    /// Pushes between solid bodies (phase P3; empty today).
    Solve,
    /// The step's messages ([`VoxelImpact`]), sorted by entity.
    Events,
    /// `Transform` follows [`BodyPosition`].
    Writeback,
    /// Interpolated `Transform`s, after the frame's fixed steps.
    Interpolate,
}

/// Impacts of the current step, written as messages in [`PhysicsSystems::Events`].
#[derive(Resource, Default)]
pub(crate) struct PendingImpacts(Vec<VoxelImpact>);

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

/// [`PhysicsSystems::Controller`]: walking and jumping (see [`CharacterController`]). Frozen
/// bodies are skipped (their steps do not happen) and keep the jump latch.
pub(crate) fn drive_characters(
    fixed: Res<FixedTime>,
    mut characters: Query<
        (
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
    for (controller, mut intent, mut velocity, contacts, settings) in &mut characters {
        let mut wish = intent.wish.as_dvec2();
        if !wish.is_finite() {
            wish = DVec2::ZERO;
        }
        let length_sq = wish.length_squared();
        if length_sq > 1.0 {
            wish /= length_sq.sqrt();
        }
        let (rate, control) = if contacts.on_ground {
            (settings.ground_friction, 1.0)
        } else {
            (settings.drag_xz, controller.air_control)
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
    }
}

/// [`PhysicsSystems::Prepare`].
pub(crate) fn prepare_bodies(
    mut commands: Commands,
    mut bodies: Query<(
        Entity,
        &Transform,
        &mut BodyPosition,
        &PhysicsSync,
        Option<&Teleport>,
        Option<&mut Interpolated>,
    )>,
    mut stats: ResMut<PhysicsStats>,
) {
    let mut count = 0;
    for (entity, transform, mut position, sync, teleport, interpolated) in &mut bodies {
        count += 1;
        if let Some(teleport) = teleport {
            position.set_if_neq(BodyPosition(teleport.0.normalize()));
            commands.entity(entity).remove::<Teleport>();
        } else if sync.last_written != Some(transform.translation) {
            // A new body, or gameplay wrote `Transform`: start from it.
            position.set_if_neq(BodyPosition(WorldPos::from_dvec3(transform.translation)));
        }
        if let Some(mut interpolated) = interpolated {
            interpolated.prev = Some(position.0);
        }
    }
    stats.bodies = count;
}

/// [`PhysicsSystems::Kinematic`]: `position += velocity * dt`, through everything.
pub(crate) fn move_kinematic(
    fixed: Res<FixedTime>,
    mut bodies: Query<(&RigidBody, &Velocity, &mut BodyPosition)>,
) {
    let dt = timestep(&fixed);
    for (kind, velocity, mut position) in &mut bodies {
        if *kind != RigidBody::Kinematic {
            continue;
        }
        let d = clean(velocity.0 * dt);
        if d != DVec3::ZERO {
            let p = position.0.normalize();
            position.0 = WorldPos::from_block(p.block, p.frac + d);
        }
    }
}

/// [`PhysicsSystems::Step`]: one step of every dynamic body ([`step_body`]). A body whose
/// step needs an unloaded block keeps its state and gets [`Frozen`].
pub(crate) fn step_dynamic(
    mut commands: Commands,
    map: Res<ChunkMap>,
    world: Res<WorldSettings>,
    content: Res<PhysicsContent>,
    settings: Res<PhysicsSettings>,
    fixed: Res<FixedTime>,
    mut bodies: Query<(
        Entity,
        &RigidBody,
        Option<&Collider>,
        &BodySettings,
        &mut BodyPosition,
        &mut Velocity,
        &mut Contacts,
        Has<Frozen>,
    )>,
    mut pending: ResMut<PendingImpacts>,
    mut stats: ResMut<PhysicsStats>,
) {
    let start = Instant::now();
    let params = settings.step_params(timestep(&fixed));
    // One source for every body: consecutive bodies often share chunks.
    let mut voxels = ChunkSource::new(&map, &world);
    let (mut dynamic, mut frozen) = (0, 0);
    for (entity, kind, collider, body, mut position, mut velocity, mut contacts, was_frozen) in
        &mut bodies
    {
        if *kind != RigidBody::Dynamic {
            continue;
        }
        dynamic += 1;
        let shape = collider.map_or(LocalBox::default(), Collider::voxel_box);
        match step_body(
            &mut voxels,
            &content.collision,
            position.0,
            velocity.0,
            shape,
            contacts.on_ground,
            body,
            &params,
        ) {
            Ok(out) => {
                position.set_if_neq(BodyPosition(out.pos));
                velocity.set_if_neq(Velocity(out.vel));
                contacts.set_if_neq(Contacts::from_sweep(&out.sweep));
                pending.0.extend(out.impacts().map(|impact| VoxelImpact {
                    entity,
                    face: impact.face,
                    block: impact.block,
                    speed: impact.speed,
                }));
                if was_frozen {
                    commands.entity(entity).remove::<Frozen>();
                }
            }
            Err(_) => {
                frozen += 1;
                if !was_frozen {
                    commands.entity(entity).insert(Frozen);
                }
            }
        }
    }
    stats.dynamic = dynamic;
    stats.frozen = frozen;
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
    // Stable: a body's impacts keep their axis order.
    pending.0.sort_by_key(|impact| impact.entity.to_bits());
    impacts.write_batch(pending.0.drain(..));
}

/// [`PhysicsSystems::Writeback`]: `Transform.translation = BodyPosition` (the only rounding of
/// a position), and [`PhysicsSync`] remembers it. Rotation is never touched.
pub(crate) fn write_back(mut bodies: Query<(&BodyPosition, &mut Transform, &mut PhysicsSync)>) {
    for (position, mut transform, mut sync) in &mut bodies {
        let translation = position.0.to_dvec3();
        if transform.translation != translation {
            transform.translation = translation;
        }
        if sync.last_written != Some(translation) {
            sync.last_written = Some(translation);
        }
    }
}

/// [`PhysicsSystems::Interpolate`]: `Transform = prev + (curr - prev) * alpha` for
/// [`Interpolated`] bodies, with `alpha = FixedTime::overstep_fraction_f64`. A body whose
/// `Transform` gameplay changed since physics wrote it is left alone (the next step teleports
/// it). Without `TimePlugin` (no `FixedTime`) no step ever runs, and neither does this.
pub(crate) fn interpolate(
    fixed: Option<Res<FixedTime>>,
    mut bodies: Query<(
        &Interpolated,
        &BodyPosition,
        &mut Transform,
        &mut PhysicsSync,
    )>,
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
