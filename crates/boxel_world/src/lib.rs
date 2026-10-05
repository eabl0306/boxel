//! `boxel_world`: the voxel world inside the ECS. Chunks are NOT entities: they live in the
//! [`ChunkMap`] resource (slotmap + hash index), as `Arc<ChunkData>` snapshots. Viewers are
//! entities with `Transform` + [`ChunkViewer`].
//!
//! Per frame (in `Update`, chained [`WorldSystems`]):
//! 1. `Interest`: compute the wanted set around viewers (data radius = mesh radius + 1, plus
//!    every column next to a mesh column; unload beyond data radius + hysteresis), create/unload
//!    slots, (re)prioritize the job queue.
//! 2. `Edits`: apply [`EditQueue`] (copy-on-write, version bump, remesh center + touched face
//!    neighbors, bump their `epoch`).
//! 3. `Dispatch`: pop jobs by priority while in-flight < cap; run them on the
//!    `AsyncComputeTaskPool` through a [`jobs::JobExecutor`].
//! 4. `Collect`: drain results within a time budget; drop stale ones (slot generation, data
//!    version, neighbor epoch, cancel flag); publish [`ChunkMeshEvent`]s.

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
