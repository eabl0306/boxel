//! World generation. A generator is a pure function of `(seed, key)`: no I/O, no global state,
//! callable from any worker thread. M0 has one stage (no cross-chunk decoration yet).
// SKELETON: remove this attribute when the crate is fully implemented (owner: WP4).
#![allow(
    unused_variables,
    unused_mut,
    dead_code,
    clippy::todo,
    reason = "skeleton stubs"
)]

use boxel_voxel::{BlockRegistry, ChunkData, ChunkKey};
use std::ops::RangeInclusive;

pub mod noise;

/// A chunk generator.
pub trait WorldGenerator: Send + Sync + 'static {
    /// Generates the chunk at `key` (lod 0 in M0). Must be deterministic: same inputs, same output.
    /// Must return `Uniform` storage for chunks entirely above/below the terrain (fast path).
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData;
    /// Chunk-y range that may contain non-air blocks; outside it the world streams nothing.
    fn vertical_chunks(&self) -> RangeInclusive<i32>;
    /// Name for logs.
    fn name(&self) -> &str;
}

/// Horizontal layers from y = 0 upwards, e.g. `[("boxel:bedrock", 1), ("boxel:stone", 3),
/// ("boxel:dirt", 2), ("boxel:grass", 1)]`; air above, air below 0. Unknown names panic at
/// construction ([`FlatGenerator::new`]).
#[derive(Clone, Debug)]
pub struct FlatGenerator {
    layers: Vec<(String, u32)>,
}

impl FlatGenerator {
    /// New generator.
    pub fn new(layers: Vec<(String, u32)>) -> Self {
        Self { layers }
    }
}

impl WorldGenerator for FlatGenerator {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        todo!("WP4")
    }
    fn vertical_chunks(&self) -> RangeInclusive<i32> {
        todo!("WP4")
    }
    fn name(&self) -> &str {
        "flat"
    }
}

/// Parameters of [`TerrainGenerator`].
#[derive(Clone, Debug, PartialEq)]
pub struct TerrainSettings {
    /// Average surface height (voxels).
    pub base_height: i32,
    /// Max deviation from `base_height` (voxels).
    pub amplitude: f32,
    /// Base frequency of the height fbm (1/voxels).
    pub frequency: f64,
    /// fbm octaves.
    pub octaves: u32,
    /// Dirt thickness under grass.
    pub dirt_depth: i32,
    /// Columns at or below this height get sand on top instead of grass.
    pub sand_level: i32,
    /// Columns above this height get snow on top.
    pub snow_level: i32,
    /// Lowest generated y (bedrock); below is air (and out of `vertical_chunks`).
    pub bottom: i32,
}

impl Default for TerrainSettings {
    fn default() -> Self {
        Self {
            base_height: 64,
            amplitude: 48.0,
            frequency: 1.0 / 256.0,
            octaves: 5,
            dirt_depth: 3,
            sand_level: 40,
            snow_level: 100,
            bottom: -64,
        }
    }
}

/// Heightmap terrain (fbm value noise): bedrock at `bottom`, stone, dirt, grass/sand/snow top.
/// Per chunk: compute the 32x32 column heights once; return Uniform air above `max height`,
/// Uniform stone fully below `min height - dirt_depth`.
#[derive(Clone, Debug)]
pub struct TerrainGenerator {
    /// World seed.
    pub seed: u64,
    /// Parameters.
    pub settings: TerrainSettings,
}

impl TerrainGenerator {
    /// New generator.
    pub fn new(seed: u64, settings: TerrainSettings) -> Self {
        Self { seed, settings }
    }
    /// Surface height at world column `(x, z)` (top solid block y). Exposed for tests/spawn.
    pub fn height_at(&self, x: i32, z: i32) -> i32 {
        todo!("WP4")
    }
}

impl Default for TerrainGenerator {
    fn default() -> Self {
        Self::new(0x00b0_7e1d, TerrainSettings::default())
    }
}

impl WorldGenerator for TerrainGenerator {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        todo!("WP4")
    }
    fn vertical_chunks(&self) -> RangeInclusive<i32> {
        todo!("WP4")
    }
    fn name(&self) -> &str {
        "terrain"
    }
}
