//! The integrator of a body: gravity, drag, the speed limit, the voxel sweep and bounces, one
//! fixed step at a time (ADR 0016, phase P2). Pure: the ECS layer calls [`step_body`] for every
//! dynamic body, and tests, benches and aim previews ([`predict`]) call it without an `App`.
//!
//! One step, with `dt` the fixed timestep in `f64`:
//! 1. `v += gravity * gravity_scale * dt`.
//! 2. Drag, in the implicit form (stable for any rate, only multiply and divide):
//!    `v.y /= 1 + drag_y * dt`, and `v.x`, `v.z /= 1 + k * dt` with `k` the
//!    [`BodySettings::ground_friction`] when the body was on the ground at the end of the previous
//!    step, else [`BodySettings::drag_xz`].
//! 3. Each component is clamped to `max_speed`, and one slower than [`STOP_SPEED`] becomes
//!    exactly 0 (drag alone only decays a speed geometrically, so a slide would never end).
//! 4. The body's box moves by `v * dt` through the voxels ([`move_box`]).
//! 5. On each clipped axis the velocity bounces (`v = -v * restitution`) if its speed is above
//!    both [`BodySettings::bounce_min_speed`] and one step of gravity along that axis
//!    (`|gravity| * gravity_scale * dt`, the speed a body resting on the ground hits it with),
//!    and stops otherwise. As in Minecraft, the rest of the step after a bounce is dropped. A
//!    clip faster than the impact speed is reported as a [`BodyImpact`].
//!
//! The defaults are Minecraft's per-tick factors turned into per-second rates: gravity 32
//! blocks/s^2 (0.08 per tick^2 at 20 Hz), vertical drag 0.4 /s (0.98 per tick, a terminal fall
//! speed of 80 blocks/s), horizontal drag 1.9 /s in the air (0.91 per tick) and 12 /s on the
//! ground (0.546 per tick).

use super::shape::LocalBox;
use super::source::{CollisionTable, VoxelSource};
use super::sweep::{SweepConfig, SweepOut, Unloaded, move_box};
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, Face, WorldPos};

/// How one body moves: gravity, drag, friction and bounces. The ECS layer uses it as a
/// component (`boxel_physics::BodySettings`, required by `RigidBody`). Rates are per second;
/// the presets are Minecraft's values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodySettings {
    /// Multiplies the world's gravity (`PhysicsSettings::gravity`): 1 for living things, 0.5
    /// for dropped items, 0 to float.
    pub gravity_scale: f64,
    /// Horizontal drag in the air, per second.
    pub drag_xz: f64,
    /// Vertical drag, per second (with gravity it gives the terminal fall speed
    /// `gravity * gravity_scale / drag_y`).
    pub drag_y: f64,
    /// Horizontal drag while on the ground, per second (friction).
    pub ground_friction: f64,
    /// Fraction of the speed kept by a bounce, in `0..=1` (0: no bounce).
    pub restitution: f64,
    /// A clip slower than this (blocks per second) stops the body on that axis instead of
    /// bouncing it, so bounces decay to rest. A clip no faster than one step of gravity along
    /// the axis never bounces either, whatever this is: otherwise a body resting on the ground
    /// would bounce in place every step and never reach a velocity of 0.
    pub bounce_min_speed: f64,
    /// Share of a push between two solid bodies (phase P3): a body with twice the weight moves
    /// half as much. Unused today.
    pub push_weight: f64,
}

impl Default for BodySettings {
    /// [`BodySettings::player`]: Minecraft's living entities.
    fn default() -> Self {
        Self::player()
    }
}

impl BodySettings {
    /// A player or a mob: full gravity, drag 1.9 /s in the air and 0.4 /s vertically, ground
    /// friction 12 /s, no bounce.
    pub const fn player() -> Self {
        Self {
            gravity_scale: 1.0,
            drag_xz: 1.9,
            drag_y: 0.4,
            ground_friction: 12.0,
            restitution: 0.0,
            bounce_min_speed: 1.0,
            push_weight: 1.0,
        }
    }

    /// A dropped item: half gravity, drag 0.4 /s on every axis, ground friction 10.6 /s (0.588
    /// per tick), no bounce.
    pub const fn item() -> Self {
        Self {
            gravity_scale: 0.5,
            drag_xz: 0.4,
            drag_y: 0.4,
            ground_friction: 10.6,
            restitution: 0.0,
            bounce_min_speed: 1.0,
            push_weight: 0.25,
        }
    }

    /// An arrow: gravity scale 0.625 (20 blocks/s^2), drag 0.2 /s, no bounce. Projectile rules
    /// (no speed limit, hits on entities) come in phase P4.
    pub const fn arrow() -> Self {
        Self {
            gravity_scale: 0.625,
            drag_xz: 0.2,
            drag_y: 0.2,
            ground_friction: 12.0,
            restitution: 0.0,
            bounce_min_speed: 1.0,
            push_weight: 0.1,
        }
    }

    /// A bouncy ball: full gravity, drag 0.4 /s, ground friction 2 /s (it rolls on), restitution
    /// 0.6, bounces stop below 2 blocks/s.
    pub const fn ball() -> Self {
        Self {
            gravity_scale: 1.0,
            drag_xz: 0.4,
            drag_y: 0.4,
            ground_friction: 2.0,
            restitution: 0.6,
            bounce_min_speed: 2.0,
            push_weight: 0.5,
        }
    }
}

/// A velocity component slower than this after drag (blocks per second) is set to exactly 0, so
/// a slide or a drift ends instead of decaying forever through ever smaller floats. At 64 Hz it
/// moves a body far less than the contact tolerance per step. Not a sleep threshold: sleeping
/// (phase P4) uses its own, larger one.
pub const STOP_SPEED: f64 = 1e-6;

/// The world-wide inputs of one step: gravity, the timestep and the limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepParams {
    /// Gravity, blocks per second squared (Minecraft: `(0, -32, 0)`).
    pub gravity: DVec3,
    /// The fixed timestep, seconds (`FixedTime::timestep().as_secs_f64()`; `2^-6` at 64 Hz).
    pub dt: f64,
    /// Speed limit per axis, blocks per second.
    pub max_speed: f64,
    /// A clip faster than this (blocks per second, along the clipped axis) is an impact.
    pub impact_speed: f64,
    /// Sweep tuning (contact tolerance, depenetration).
    pub sweep: SweepConfig,
}

impl Default for StepParams {
    /// Gravity `(0, -32, 0)`, 64 Hz, 100 blocks/s, impacts from 4 blocks/s, default sweep.
    fn default() -> Self {
        Self {
            gravity: DVec3::new(0.0, -32.0, 0.0),
            dt: 1.0 / 64.0,
            max_speed: 100.0,
            impact_speed: 4.0,
            sweep: SweepConfig::default(),
        }
    }
}

/// A clip of one face of a body against a block, faster than [`StepParams::impact_speed`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyImpact {
    /// The face of the body that hit (`Face::NegY`: it landed).
    pub face: Face,
    /// The block it hit.
    pub block: BlockPos,
    /// The speed along the face's axis just before the hit, blocks per second.
    pub speed: f64,
}

/// What [`step_body`] did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyStep {
    /// The new position.
    pub pos: WorldPos,
    /// The new velocity.
    pub vel: DVec3,
    /// The sweep: faces that touched, ground, fluid.
    pub sweep: SweepOut,
    /// Impacts of this step, at most one per axis, in axis order X, Y, Z.
    pub impacts: [Option<BodyImpact>; 3],
}

impl BodyStep {
    /// The impacts of this step.
    pub fn impacts(&self) -> impl Iterator<Item = BodyImpact> + '_ {
        self.impacts.iter().flatten().copied()
    }
}

/// Steps 1 to 3 of the integrator (gravity, drag, speed limit, [`STOP_SPEED`]): the velocity a
/// body moves with this step. `on_ground` is whether it ended the previous step on the ground
/// (ground friction instead of air drag). A component that is not finite counts as 0; a
/// `max_speed` that is NaN or not positive means no limit.
pub fn integrate_velocity(
    vel: DVec3,
    on_ground: bool,
    settings: &BodySettings,
    params: &StepParams,
) -> DVec3 {
    let dt = params.dt;
    let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
    let mut v = DVec3::new(finite(vel.x), finite(vel.y), finite(vel.z));
    v += params.gravity * (settings.gravity_scale * dt);
    let k = if on_ground {
        settings.ground_friction
    } else {
        settings.drag_xz
    };
    let horizontal = 1.0 + k * dt;
    v.x /= horizontal;
    v.z /= horizontal;
    v.y /= 1.0 + settings.drag_y * dt;
    // NaN settings must not leak into the state.
    let mut v = DVec3::new(finite(v.x), finite(v.y), finite(v.z));
    let max = params.max_speed;
    if max > 0.0 {
        v = v.clamp(DVec3::splat(-max), DVec3::splat(max));
    }
    let stop = |v: f64| if v.abs() < STOP_SPEED { 0.0 } else { v };
    DVec3::new(stop(v.x), stop(v.y), stop(v.z))
}

/// One step of a dynamic body with the voxel box `shape` (relative to `pos`): gravity, drag,
/// the speed limit, the sweep against the voxels of `src`, bounces and impacts (see the module
/// docs). `on_ground` is whether the body ended the previous step on the ground.
///
/// # Errors
///
/// [`Unloaded`] when the move needs a block of an unloaded chunk. Nothing happened then: the
/// caller keeps the body's position and velocity (the freeze policy) and retries next step.
pub fn step_body<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    pos: WorldPos,
    vel: DVec3,
    shape: LocalBox,
    on_ground: bool,
    settings: &BodySettings,
    params: &StepParams,
) -> Result<BodyStep, Unloaded> {
    let mut v = integrate_velocity(vel, on_ground, settings, params);
    let sweep = move_box(src, table, pos, shape, v * params.dt, &[], &params.sweep)?;
    let mut impacts = [None; 3];
    for (axis, impact) in impacts.iter_mut().enumerate() {
        let face = face_of(axis, v[axis] > 0.0);
        if v[axis] == 0.0 || !sweep.touched(face) {
            continue;
        }
        let speed = v[axis].abs();
        if speed > params.impact_speed
            && let Some(block) = sweep.blocks[axis]
        {
            *impact = Some(BodyImpact { face, block, speed });
        }
        // A resting body hits the ground with one step of gravity every step: bouncing that
        // would never let it rest. (`max` ignores a NaN side.)
        let resting = (params.gravity[axis] * settings.gravity_scale * params.dt).abs();
        let bounced = -v[axis] * settings.restitution;
        let min_speed = settings.bounce_min_speed.max(resting);
        v[axis] = if speed > min_speed && bounced != 0.0 && bounced.is_finite() {
            bounced
        } else {
            0.0
        };
    }
    Ok(BodyStep {
        pos: sweep.end,
        vel: v,
        sweep,
        impacts,
    })
}

/// Where a body starting at `pos` with velocity `vel` is after `steps` steps in open air, and
/// its velocity: the integrator of [`step_body`] without voxels (the body never touches the
/// ground), with the same arithmetic, so it matches a real flight bit for bit until something is
/// hit. For aim previews and tests.
pub fn predict(
    pos: WorldPos,
    vel: DVec3,
    settings: &BodySettings,
    params: &StepParams,
    steps: u32,
) -> (WorldPos, DVec3) {
    let mut pos = pos.normalize();
    let mut v = vel;
    for _ in 0..steps {
        v = integrate_velocity(v, false, settings, params);
        pos = WorldPos::from_block(pos.block, pos.frac + sweep_distance(v * params.dt));
    }
    (pos, v)
}

/// A displacement as [`move_box`] uses it: non-finite components count as 0 and each one is
/// clamped to [`super::sweep::MAX_SWEEP_DISTANCE`].
fn sweep_distance(d: DVec3) -> DVec3 {
    let max = super::sweep::MAX_SWEEP_DISTANCE;
    let clean = |v: f64| {
        if v.is_finite() {
            v.clamp(-max, max)
        } else {
            0.0
        }
    };
    DVec3::new(clean(d.x), clean(d.y), clean(d.z))
}

/// The face of a box on `axis` (0 = X, 1 = Y, 2 = Z) facing the positive or negative side.
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
