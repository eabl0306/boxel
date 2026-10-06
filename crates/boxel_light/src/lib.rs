//! `boxel_light`: voxel light as a pure function of the blocks (ADR 0015). Pure CPU: no ECS, no
//! GPU.
//!
//! Light is never stored with the blocks and never propagated across chunks. A chunk's light
//! (and that of its 1-voxel border, what the mesher reads) is computed from scratch from the
//! 27 chunks around it and the sky tops of the 3x3 columns around it:
//!
//! - [`sky_tile`]: the sky top `T(x, z)` of each column, the highest voxel with opacity above
//!   0 in the world content (generator plus edits), found by walking each chunk column down
//!   ([`sky_tile::walk`]). A voxel above `T` is direct sky (15). `T` never depends on what is
//!   loaded, so streaming never relights anything.
//! - [`box_light`]: the exact light of the padded chunk (local `-1..=32`). Any light that
//!   reaches it comes from at most 14 voxels away, so a breadth-first flood over the box chunk
//!   +-16 (64^3) gives the global solution bit for bit, with fast paths for dark and open
//!   chunks.
//! - [`PaddedLight`]: that result, in the mesher's padded order; [`ChunkLight`]: the chunk's
//!   own 32^3 light, compressed, for gameplay queries.
//! - `oracle` (feature `oracle`): a scalar global solver over an explicit world, the reference
//!   the tests compare against.
//!
//! The rules (also in [`boxel_voxel::light`]): opacity 15 stops light, entering a voxel costs
//! `max(1, opacity)` levels, emitters keep their emission even when opaque, channels (sky, red,
//! green, blue) mix by per-channel max. All integer, so the result is unique and independent
//! of order, thread count and position (every absolute comparison is made relative to the box
//! in `i64`).

pub mod box_light;
mod chunk_light;
#[cfg(any(test, feature = "oracle"))]
pub mod oracle;
mod padded;
pub mod sky_tile;

pub use box_light::{LightInput, LightPath, LightScratch, SkyInput, compute, compute_full};
pub use chunk_light::ChunkLight;
pub use padded::{PADDED, PADDED_AREA, PADDED_VOLUME, PaddedLight};
pub use sky_tile::{LayerData, Rescan, SkyTile};
