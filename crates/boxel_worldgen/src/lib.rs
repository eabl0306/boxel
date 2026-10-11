//! World generation. A generator is a pure function of `(seed, key)`: no I/O, no global state,
//! callable from any worker thread.
//!
//! The engine holds the mechanism: the [`WorldGenerator`] trait, [`FlatGenerator`] (layers), the
//! integer noise ([`noise`], [`Frequency`]) that stays exact at any `i64` coordinate, and
//! [`block`] to resolve block names. The terrains of a game (heightmaps, caves, trees) are its
//! content: they implement [`WorldGenerator`] on top of this crate.
//!
//! [`lod`] is the far level of detail's side of a generator (ADR 0031): a [`LodSource`] samples
//! coarse cells level by level, for terrain far beyond the full-detail view distance.

use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME, ChunkData,
    ChunkKey,
};
use std::ops::RangeInclusive;

pub mod lod;
pub mod noise;

pub use lod::LodSource;
pub use noise::Frequency;

/// A chunk generator.
///
/// The world has no height limit: it asks for chunks at any height around its viewers, so a
/// generator must answer fast far above and below its terrain.
pub trait WorldGenerator: Send + Sync + 'static {
    /// Generates the chunk at `key` (lod 0 in M0). Must be deterministic: same inputs, same output.
    /// Must answer in bounded time at any height: a chunk with nothing to place (above the
    /// terrain, or inside plain ground with no feature in it) is `Uniform` storage, built
    /// without walking the voxels (fast path). A generator with features at every depth, such
    /// as caves, may return other storage there (a 1-bit chunk), but its cost per chunk must not
    /// grow with the height.
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData;
    /// Chunk-y range that may contain non-air blocks: every chunk outside it is uniform air. The
    /// world creates those chunks as air without calling [`WorldGenerator::generate`] (they still
    /// exist and can be edited: the range is not a height limit). The default,
    /// `i64::MIN..=i64::MAX`, promises nothing.
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        i64::MIN..=i64::MAX
    }
    /// Far level of detail (ADR 0031): this generator's coarse source, `None` (the default) if
    /// it has none. A generator that has one usually implements [`LodSource`] itself and returns
    /// `Some(self)`, so the source reads its current settings.
    fn lod(&self) -> Option<&dyn LodSource> {
        None
    }
    /// Name for logs.
    fn name(&self) -> &str;
}

/// Horizontal layers from y = 0 upwards, e.g. `[("game:bedrock", 1), ("game:stone", 3),
/// ("game:dirt", 2), ("game:grass", 1)]`; air above, air below 0. Layer names are resolved
/// against the registry passed to [`WorldGenerator::generate`]: an unknown name panics there
/// with the name (a programmer error; the constructor has no registry to check against).
#[derive(Clone, Debug)]
pub struct FlatGenerator {
    layers: Vec<(String, u32)>,
}

impl FlatGenerator {
    /// New generator.
    pub fn new(layers: Vec<(String, u32)>) -> Self {
        Self { layers }
    }

    /// Total height of all layers (voxels), without overflow.
    fn total_height(&self) -> i64 {
        self.layers.iter().map(|(_, n)| i64::from(*n)).sum()
    }
}

impl WorldGenerator for FlatGenerator {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        // Resolve (and so validate) every layer name on every call, even for chunks outside
        // the layers: a bad name must fail loudly wherever the first chunk is generated.
        let layers: Vec<(BlockState, i64)> = self
            .layers
            .iter()
            .map(|(name, thickness)| {
                (
                    block(registry, name, "FlatGenerator"),
                    i64::from(*thickness),
                )
            })
            .collect();
        let y0 = key.pos.origin().y;
        if y0 + i64::from(CHUNK_SIZE) <= 0 || y0 >= self.total_height() {
            // Fully below 0 or above the layers.
            return ChunkData::uniform(BlockState::AIR);
        }
        let mut column = [BlockState::AIR; CHUNK_SIZE as usize];
        for (ly, slot) in (0i64..).zip(column.iter_mut()) {
            let y = y0 + ly;
            if y < 0 {
                continue;
            }
            let mut top = 0;
            for &(state, thickness) in &layers {
                top += thickness;
                if y < top {
                    *slot = state;
                    break;
                }
            }
        }
        if column.iter().all(|&s| s == column[0]) {
            return ChunkData::uniform(column[0]);
        }
        // One y layer is CHUNK_AREA contiguous voxels (`LocalPos` index = y << 10 | x << 5 | z).
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        let (y_layers, _) = dense.as_chunks_mut::<CHUNK_AREA>();
        for (y_layer, &state) in y_layers.iter_mut().zip(&column) {
            y_layer.fill(state);
        }
        ChunkData::from_dense(&dense)
    }
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        // Empty (`0..=-1`) when there are no layers.
        0..=((self.total_height() - 1) >> CHUNK_SHIFT)
    }
    /// Exact coarse cells (the cell rule applied to its own blocks, [`lod`]).
    fn lod(&self) -> Option<&dyn LodSource> {
        Some(self)
    }
    fn name(&self) -> &str {
        "flat"
    }
}

/// State of block `name`; panics with the name if `registry` lacks it (a programmer error: the
/// generator and the registry do not match). `generator` names the caller in the message.
pub fn block(registry: &BlockRegistry, name: &str, generator: &str) -> BlockState {
    registry
        .by_name(name)
        .unwrap_or_else(|| panic!("{generator}: block {name:?} is not in the registry"))
}
