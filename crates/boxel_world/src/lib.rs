//! `boxel_world`: the voxel world inside the ECS. Chunks are NOT entities: they live in the
//! [`ChunkMap`] resource (slotmap + hash index), as `Arc<ChunkData>` snapshots. Viewers are
//! entities with `Transform` + [`ChunkViewer`].
//!
//! Each viewer wants a vertical cylinder of chunks around it ([`ChunkViewer`]: horizontal and
//! vertical radius); the world has no height limit unless [`WorldSettings::vertical_bounds`] sets
//! one.
//!
//! Per frame (in `Update`, chained [`WorldSystems`]):
//! 1. `Interest`: when a viewer moves, list only the chunk positions whose interest changes (the
//!    slabs entering and leaving), then create, update and unload slots within
//!    [`WorldSettings::interest_budget`], nearest first; refresh the stale job priorities a batch
//!    at a time.
//! 2. `Edits`: apply [`EditQueue`] (copy-on-write, version bump, remesh center + touched face
//!    neighbors, bump their `epoch`).
//! 3. `Dispatch`: collect finished jobs, then pop jobs by priority while in-flight < cap and run
//!    them on the `AsyncComputeTaskPool` through a [`jobs::JobExecutor`]; repeated while results
//!    keep arriving, within [`WorldSettings::job_budget`]. Stale results are dropped (slot
//!    generation, data version, neighbor epoch, cancel flag).
//! 4. `Collect`: publish mesh results as [`ChunkMeshEvent`]s, up to
//!    [`WorldSettings::max_mesh_events_per_frame`].

mod interest;
mod map;
mod plugin;
mod streaming;
#[cfg(test)]
mod tests;

pub mod jobs;
pub mod persist;
pub mod scheduler;

pub use map::{ChunkId, ChunkMap, ChunkSlot, DataState, Interest, MeshState, WorldStats};
pub use plugin::{
    BlockEdit, ChunkMeshEvent, ChunkViewer, EditQueue, WorldContent, WorldPlugin, WorldSettings,
    WorldSystems,
};

/// Common items.
pub mod prelude {
    pub use crate::{ChunkMap, ChunkViewer, EditQueue, WorldPlugin, WorldSettings};
}
