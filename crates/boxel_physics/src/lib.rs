//! `boxel_physics`: boxel's own minimal physics (ADR 0016), with no third-party physics engine.
//!
//! Scope of the engine: axis-aligned boxes and spheres, gravity, no rotations; bounces;
//! projectiles; voxel and entity raycasts; between entities, a per-entity choice of solid push
//! or sensor events. This crate grows in phases (see ADR 0016); today (P4) it has the voxel
//! core, the raycast, bodies that move against the voxels, collisions between bodies,
//! projectiles that never tunnel, sleeping bodies and a parallel step.
//!
//! Two layers:
//! - [`core`] is pure (no ECS): the [`CollisionTable`] (how each block state collides, derived
//!   from `BlockDef::solid`), the [`VoxelSource`] trait, the per-axis AABB sweep against the
//!   voxel grid ([`move_box`]), the exact voxel raycast ([`raycast_voxels`]), ray tests against
//!   [`Shape`]s and the integrator of a body ([`step_body`], [`predict`]), the narrowphase
//!   ([`shape_contact`]) and the broadphase [`Grid`]. Tests and benches drive it without an
//!   `App`.
//! - The ECS layer: [`ChunkSource`] (the world's `ChunkMap` as a [`VoxelSource`]),
//!   [`PhysicsPlugin`], the [`PhysicsQuery`] system param (voxel and entity raycasts, box
//!   queries, the block placement check) and bodies: [`RigidBody`], [`Collider`],
//!   [`Velocity`], [`BodyPosition`], [`Contacts`], [`BodySettings`], [`Interpolated`],
//!   [`Teleport`], [`Frozen`], [`Sleeping`], [`ReportContacts`], [`CharacterController`] with
//!   [`MoveIntent`], [`Projectile`], and the messages [`VoxelImpact`], [`CollisionStarted`],
//!   [`CollisionEnded`] and [`ProjectileHit`].
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
//! disallows fused multiply-add and the libm functions. Bodies step each one alone (no body
//! reads another, except through the obstacles of the current step), so the order they step in,
//! and the threads of a parallel step, change nothing; pairs are sorted by entity and pushed in
//! a fixed order, and messages are sorted by entity.
//!
//! Scale: bodies that rest fall asleep ([`Sleeping`]) and cost almost nothing until something
//! wakes them; above `PhysicsSettings::parallel_threshold` awake bodies the step runs on the
//! compute threads.

mod components;
mod contacts;
pub mod core;
mod plugin;
mod projectile;
mod query;
mod sleep;
mod source;
mod systems;

pub use crate::components::{
    BodyPosition, CharacterController, Collider, CollisionEnded, CollisionStarted, Contacts,
    Frozen, Interpolated, MoveIntent, OnBlock, OnEntity, PhysicsSettings, PhysicsStats,
    PhysicsSync, Projectile, ProjectileHit, ProjectileTarget, ReportContacts, Response, RigidBody,
    Sleeping, Teleport, Velocity, VoxelImpact,
};
pub use crate::contacts::{BodyFilter, BodyHit, BodyIndex};
pub use crate::core::{
    BlockBounds, BodyImpact, BodySettings, BodyStep, CollisionLayers, CollisionShape,
    CollisionTable, Contact, Grid, LocalBox, MAX_RAY_DISTANCE, MAX_SWEEP_DISTANCE, Probe,
    RayResult, STOP_SPEED, Shape, ShapeHit, StepParams, StepUp, SweepConfig, SweepOut, TestWorld,
    Unloaded, VoxelFilter, VoxelHit, VoxelSource, axis_order, integrate_velocity, move_body,
    move_box, move_box_stepping, predict, raycast_voxels, shape_contact, step_body,
};
pub use boxel_voxel::WorldPos;
pub use plugin::{PhysicsContent, PhysicsPlugin};
pub use query::{PhysicsQuery, RayFilter, RayHit};
pub use source::ChunkSource;
pub use systems::PhysicsSystems;

/// Common items.
pub mod prelude {
    pub use crate::{
        BodyFilter, BodyPosition, BodySettings, CharacterController, Collider, CollisionEnded,
        CollisionLayers, CollisionStarted, Contacts, Interpolated, LocalBox, MoveIntent, OnBlock,
        OnEntity, PhysicsPlugin, PhysicsQuery, PhysicsSystems, Projectile, ProjectileHit,
        ProjectileTarget, RayFilter, RayHit, RayResult, ReportContacts, RigidBody, Shape, Sleeping,
        Teleport, Velocity, VoxelFilter, VoxelHit, VoxelImpact,
    };
}
