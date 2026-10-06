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
//! 2. `Edits`: apply [`EditQueue`] (copy-on-write, version bump) and remesh, as one edit batch:
//!    the center and every neighbor whose padded region holds a changed voxel (edges and corners
//!    included, for AO), every chunk the light change can reach (up to 8 per voxel, sized from
//!    the cached light) and the chunks around a column whose sky top moved; their `epoch` is
//!    bumped.
//! 3. `Dispatch`: collect finished jobs, then pop jobs by priority while in-flight < cap and run
//!    them on the `AsyncComputeTaskPool` through a [`jobs::JobExecutor`] (stale priorities that
//!    may come first are refreshed on the way, so jobs start nearest first); repeated while
//!    results keep arriving, within [`WorldSettings::job_budget`]. Stale results are dropped (slot
//!    generation, data version, neighbor epoch, cancel flag). Mesh jobs compute the chunk's light
//!    first (ADR 0015); one whose light box may hold direct sky waits for the sky tiles of its
//!    3x3 chunk columns ([`SkyMap`]), walked by their own jobs.
//! 4. `Collect`: publish mesh results as [`ChunkMeshEvent`]s, up to
//!    [`WorldSettings::max_mesh_events_per_frame`]. A mesh identical to the published one is not
//!    sent again, and the meshes of one edit batch are published together (held up to
//!    [`WorldSettings::edit_batch_hold_frames`]).

/// `let _span = frame_span!("name");` opens a tracing span until the end of the scope, for frame
/// breakdowns (`boxel_log::span_times`, the stress example's `--spans`). Only with feature
/// `trace`; without it the macro is a zero-sized value and no code.
#[cfg(feature = "trace")]
macro_rules! frame_span {
    ($name:literal) => {
        tracing::info_span!($name).entered()
    };
}
#[cfg(not(feature = "trace"))]
macro_rules! frame_span {
    ($name:literal) => {
        $crate::NoSpan
    };
}

/// What `frame_span!` gives without feature `trace`.
#[cfg(not(feature = "trace"))]
pub(crate) struct NoSpan;

mod interest;
mod map;
mod plugin;
mod sky;
mod streaming;
#[cfg(test)]
mod tests;

pub mod jobs;
pub mod persist;
pub mod reference;
pub mod scheduler;

pub use map::{ChunkId, ChunkMap, ChunkSlot, DataState, Interest, MeshState, WorldStats};
pub use plugin::{
    BlockEdit, ChunkMeshEvent, ChunkViewer, EditQueue, WorldContent, WorldPlugin, WorldSettings,
    WorldSystems,
};
pub use sky::{ColumnId, SkyMap};

/// Common items.
pub mod prelude {
    pub use crate::{ChunkMap, ChunkViewer, EditQueue, WorldPlugin, WorldSettings};
}
