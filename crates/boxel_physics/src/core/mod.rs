//! The pure core: no ECS, only `glam` (through `boxel_math`) and `boxel_voxel`, testable and
//! benchable without an `App`.
//!
//! - [`source`]: [`VoxelSource`] (read blocks one at a time), [`CollisionTable`] (how each block
//!   state collides) and [`TestWorld`] (an in-memory source for tests and tools).
//! - [`shape`]: [`Shape`] (box or sphere), [`LocalBox`] and ray tests against them.
//! - [`sweep`]: [`move_box`], the per-axis AABB sweep against the voxel grid.
//! - [`ray`]: [`raycast_voxels`], the exact DDA voxel raycast.
//! - [`body`]: [`step_body`], one fixed step of a body (gravity, drag, sweep, bounces), and
//!   [`predict`].
//! - [`narrow`]: the narrowphase between two shapes ([`shape_contact`]: overlap and the minimum
//!   translation vector).
//! - [`grid`]: the broadphase [`Grid`] (candidate pairs, box queries, cells along a ray) and
//!   [`CollisionLayers`].

pub mod body;
pub mod grid;
pub mod narrow;
pub mod ray;
pub mod shape;
pub mod source;
pub mod sweep;

pub use body::{
    BodyImpact, BodySettings, BodyStep, STOP_SPEED, StepParams, integrate_velocity, move_body,
    predict, step_body,
};
pub use grid::{
    BlockBounds, CellRay, CollisionLayers, DEFAULT_GRID_SHIFT, Grid, MAX_CELLS_PER_AXIS,
    MAX_GRID_SHIFT,
};
pub use narrow::{
    Contact, box_box, box_box_moving, box_sphere, shape_box, shape_contact, sphere_box,
    sphere_sphere,
};
pub use ray::{MAX_RAY_DISTANCE, RayResult, VoxelFilter, VoxelHit, raycast_voxels};
pub use shape::{LocalBox, Shape, ShapeHit};
pub use source::{CollisionShape, CollisionTable, Probe, TestWorld, VoxelSource};
pub use sweep::{MAX_SWEEP_DISTANCE, SweepConfig, SweepOut, Unloaded, axis_order, move_box};
