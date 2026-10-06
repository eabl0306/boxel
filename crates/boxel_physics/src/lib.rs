//! `boxel_physics`: boxel's own minimal physics (ADR 0016), with no third-party physics engine.
//!
//! Scope of the engine: axis-aligned boxes and spheres, gravity, no rotations; bounces;
//! projectiles later; voxel and entity raycasts; between entities, a per-entity choice of solid
//! push or sensor events. This crate grows in phases (see ADR 0016); today (P2) it has the voxel
//! core, the raycast and bodies that move against the voxels.
//!
//! Two layers:
//! - [`core`] is pure (no ECS): the [`CollisionTable`] (how each block state collides, derived
//!   from `BlockDef::solid`), the [`VoxelSource`] trait, the per-axis AABB sweep against the
//!   voxel grid ([`move_box`]), the exact voxel raycast ([`raycast_voxels`]), ray tests against
//!   [`Shape`]s and the integrator of a body ([`step_body`], [`predict`]). Tests and benches
//!   drive it without an `App`.
//! - The ECS layer: [`ChunkSource`] (the world's `ChunkMap` as a [`VoxelSource`]),
//!   [`PhysicsPlugin`], the [`PhysicsQuery`] system param and bodies: [`RigidBody`],
//!   [`Collider`], [`Velocity`], [`BodyPosition`], [`Contacts`], [`BodySettings`],
//!   [`Interpolated`], [`Teleport`], [`Frozen`], [`CharacterController`] with [`MoveIntent`],
//!   and the [`VoxelImpact`] message.
//!
//! Frame structure with physics (see [`PhysicsSystems`]):
//! 1. `RunFixedMainLoopSystems::BeforeFixedMainLoop`: the game samples input into
//!    [`MoveIntent`] (the jump is a latch).
//! 2. Every fixed step (64 Hz): the game's `FixedUpdate` writes [`Velocity`] and intents, then
//!    physics runs in `FixedPostUpdate`.
//! 3. `RunFixedMainLoopSystems::AfterFixedMainLoop`: [`Interpolated`] bodies get a smooth
//!    `Transform`.
//! 4. `Update` reads the step's messages and the interpolated `Transform`s (a camera follows a
//!    player here).
//!
//! Positions are [`WorldPos`]: a block (`i64`) plus the offset inside it. Every computation
//! happens in the frame of a block, with small numbers, so results are exact at any distance
//! from the origin and bit-identical when the scene is shifted by any integer vector (the
//! far-coords tests check shifts up to the world limit).
//!
//! Determinism: floats only go through `+ - * /`, `sqrt`, `floor`, `ceil`, `abs`, `min` and
//! `max`, which IEEE 754 defines exactly on every platform. `clippy.toml` in this crate
//! disallows fused multiply-add and the libm functions. Bodies step in query order, each one
//! alone (no body reads another), and messages are sorted by entity.

mod components;
pub mod core;
mod plugin;
mod query;
mod source;
mod systems;

pub use crate::components::{
    BodyPosition, CharacterController, Collider, CollisionLayers, Contacts, Frozen, Interpolated,
    MoveIntent, PhysicsSettings, PhysicsStats, PhysicsSync, Response, RigidBody, Teleport,
    Velocity, VoxelImpact,
};
pub use crate::core::{
    BodyImpact, BodySettings, BodyStep, CollisionShape, CollisionTable, LocalBox, MAX_RAY_DISTANCE,
    MAX_SWEEP_DISTANCE, Probe, RayResult, Shape, ShapeHit, StepParams, SweepConfig, SweepOut,
    TestWorld, Unloaded, VoxelFilter, VoxelHit, VoxelSource, axis_order, integrate_velocity,
    move_box, predict, raycast_voxels, step_body,
};
pub use boxel_voxel::WorldPos;
pub use plugin::{PhysicsContent, PhysicsPlugin};
pub use query::PhysicsQuery;
pub use source::ChunkSource;
pub use systems::PhysicsSystems;

/// Common items.
pub mod prelude {
    pub use crate::{
        BodyPosition, BodySettings, CharacterController, Collider, Contacts, Interpolated,
        LocalBox, MoveIntent, PhysicsPlugin, PhysicsQuery, PhysicsSystems, RayResult, RigidBody,
        Shape, Teleport, Velocity, VoxelFilter, VoxelHit, VoxelImpact,
    };
}
