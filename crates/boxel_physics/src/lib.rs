//! `boxel_physics`: boxel's own minimal physics (ADR 0016), with no third-party physics engine.
//!
//! Scope of the engine: axis-aligned boxes and spheres, gravity, no rotations; bounces and
//! projectiles later; voxel and entity raycasts; between entities, a per-entity choice of solid
//! push or sensor events. This crate grows in phases (see ADR 0016); today (P1) it has the voxel
//! core and the raycast.
//!
//! Two layers:
//! - [`core`] is pure (no ECS): the [`CollisionTable`] (how each block state collides, derived
//!   from `BlockDef::solid`), the [`VoxelSource`] trait, the per-axis AABB sweep against the
//!   voxel grid ([`move_box`]), the exact voxel raycast ([`raycast_voxels`]) and ray tests
//!   against [`Shape`]s. Tests and benches drive it without an `App`.
//! - The ECS layer: [`ChunkSource`] (the world's `ChunkMap` as a [`VoxelSource`]),
//!   [`PhysicsPlugin`] and the [`PhysicsQuery`] system param.
//!
//! Positions are [`WorldPos`]: a block (`i64`) plus the offset inside it. Every computation
//! happens in the frame of a block, with small numbers, so results are exact at any distance
//! from the origin and bit-identical when the scene is shifted by any integer vector (the
//! far-coords tests check shifts up to the world limit).
//!
//! Determinism: floats only go through `+ - * /`, `sqrt`, `floor`, `ceil`, `abs`, `min` and
//! `max`, which IEEE 754 defines exactly on every platform. `clippy.toml` in this crate
//! disallows fused multiply-add and the libm functions.

pub mod core;
mod plugin;
mod query;
mod source;

pub use crate::core::{
    CollisionShape, CollisionTable, LocalBox, MAX_SWEEP_DISTANCE, Probe, RayResult, Shape,
    ShapeHit, SweepConfig, SweepOut, TestWorld, Unloaded, VoxelFilter, VoxelHit, VoxelSource,
    axis_order, move_box, raycast_voxels,
};
pub use boxel_voxel::WorldPos;
pub use plugin::{PhysicsContent, PhysicsPlugin};
pub use query::PhysicsQuery;
pub use source::ChunkSource;

/// Common items.
pub mod prelude {
    pub use crate::{
        LocalBox, PhysicsPlugin, PhysicsQuery, RayResult, Shape, VoxelFilter, VoxelHit,
    };
}
