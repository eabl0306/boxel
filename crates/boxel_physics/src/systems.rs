//! The per-step systems of physics bodies (phase P2) and their sets.

use crate::components::{
    BodyPosition, CharacterController, Collider, Contacts, Frozen, Interpolated, MoveIntent,
    PhysicsSettings, PhysicsStats, PhysicsSync, RigidBody, Teleport, Velocity, VoxelImpact,
};
use crate::contacts::{ObstacleIndex, ObstacleScratch};
use crate::core::{BodySettings, LocalBox, MAX_SWEEP_DISTANCE, integrate_velocity, move_body};
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
    /// New bodies take their position from `Transform`; teleports (a [`Teleport`], or a
    /// `Transform` that differs from what physics wrote) move [`BodyPosition`] and clear
    /// [`Contacts`]; [`Interpolated`] bodies keep their position before the step.
    Prepare,
    /// [`CharacterController`]s turn [`MoveIntent`] into [`Velocity`]. After `Prepare`, so a
    /// body teleported this step is in the air for its controller (no jump from the old
    /// ground).
    Controller,
    /// Kinematic bodies move by their velocity; then the static and kinematic solid colliders
    /// are indexed as obstacles for the dynamic bodies.
    Kinematic,
    /// Dynamic bodies move: gravity, drag, the sweep against the voxels and the obstacles,
    /// bounces, freezing in unloaded chunks.
    Step,
    /// The broadphase: every body with a collider goes into the grid of
    /// [`crate::BodyIndex`], which gives the candidate pairs.
    Broadphase,
    /// Solid bodies are pushed apart; sensor overlaps (and solid contacts with
    /// [`crate::ReportContacts`]) are collected.
    Solve,
    /// The step's messages ([`VoxelImpact`], [`crate::CollisionStarted`],
    /// [`crate::CollisionEnded`]), sorted by entity.
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

/// [`PhysicsSystems::Controller`]: walking and jumping (see [`CharacterController`]). Only
/// dynamic bodies walk (the others have no drag to level the speed off). Frozen bodies are
/// skipped (their steps do not happen) and keep the jump latch.
pub(crate) fn drive_characters(
    fixed: Res<FixedTime>,
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
    for (kind, controller, mut intent, mut velocity, contacts, settings) in &mut characters {
        if *kind != RigidBody::Dynamic {
            continue;
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

/// [`PhysicsSystems::Prepare`]. A teleport clears [`Contacts`]: the body is not on its old
/// ground any more (the next step finds the new one).
pub(crate) fn prepare_bodies(
    mut commands: Commands,
    mut bodies: Query<(
        Entity,
        &Transform,
        &mut BodyPosition,
        &mut Contacts,
        &mut PhysicsSync,
        Option<&Teleport>,
        Option<&mut Interpolated>,
    )>,
    mut stats: ResMut<PhysicsStats>,
) {
    let mut count = 0;
    for (entity, transform, mut position, mut contacts, mut sync, teleport, interpolated) in
        &mut bodies
    {
        count += 1;
        if sync.moved != DVec3::ZERO {
            sync.moved = DVec3::ZERO;
        }
        let teleported = if let Some(teleport) = teleport {
            position.set_if_neq(BodyPosition(teleport.0.normalize()));
            // `try_`: a command of another system may have despawned it at this sync point.
            commands.entity(entity).try_remove::<Teleport>();
            true
        } else if sync.last_written != Some(transform.translation) {
            // A new body, or gameplay wrote `Transform`: start from it.
            position.set_if_neq(BodyPosition(WorldPos::from_dvec3(transform.translation)));
            true
        } else {
            false
        };
        if teleported {
            contacts.set_if_neq(Contacts::default());
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
    mut bodies: Query<(&RigidBody, &Velocity, &mut BodyPosition, &mut PhysicsSync)>,
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
        }
    }
}

/// [`PhysicsSystems::Step`]: one step of every dynamic body ([`crate::step_body`], plus the
/// static and kinematic solid obstacles for a solid body). A body whose step needs an unloaded
/// block keeps its state and gets [`Frozen`].
pub(crate) fn step_dynamic(
    mut commands: Commands,
    map: Res<ChunkMap>,
    world: Res<WorldSettings>,
    content: Res<PhysicsContent>,
    settings: Res<PhysicsSettings>,
    fixed: Res<FixedTime>,
    obstacles: Res<ObstacleIndex>,
    mut scratch: Local<ObstacleScratch>,
    mut bodies: Query<(
        Entity,
        &RigidBody,
        Option<&Collider>,
        &BodySettings,
        &mut BodyPosition,
        &mut Velocity,
        &mut Contacts,
        &mut PhysicsSync,
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
    for (
        entity,
        kind,
        collider,
        body,
        mut position,
        mut velocity,
        mut contacts,
        mut sync,
        was_frozen,
    ) in &mut bodies
    {
        if *kind != RigidBody::Dynamic {
            continue;
        }
        dynamic += 1;
        let shape = collider.map_or(LocalBox::default(), Collider::voxel_box);
        let pos = position.0.normalize();
        let v = integrate_velocity(velocity.0, contacts.on_ground, body, &params);
        // Solid colliders sweep against the static and kinematic solids too.
        let blockers = match collider {
            Some(collider) if !collider.is_sensor() && !obstacles.is_empty() => {
                scratch.gather(&obstacles, pos, shape, v * params.dt, collider.layers)
            }
            _ => &[],
        };
        match move_body(
            &mut voxels,
            &content.collision,
            pos,
            v,
            shape,
            blockers,
            body,
            &params,
        ) {
            Ok(out) => {
                if out.sweep.moved != DVec3::ZERO {
                    sync.moved = out.sweep.moved;
                }
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
                    commands.entity(entity).try_remove::<Frozen>();
                }
            }
            Err(_) => {
                frozen += 1;
                if !was_frozen {
                    // `try_`: another system's command applied at the same sync point may
                    // despawn the body first.
                    commands.entity(entity).try_insert(Frozen);
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
    // A body has at most one impact per axis, so the key is unique: an unstable sort (no
    // scratch allocation) gives one order, axis X, Y, Z within each body.
    pending
        .0
        .sort_unstable_by_key(|impact| (impact.entity.to_bits(), impact.face.axes().0));
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
