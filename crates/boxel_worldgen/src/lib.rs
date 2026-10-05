//! World generation. A generator is a pure function of `(seed, key)`: no I/O, no global state,
//! callable from any worker thread. M0 has one stage (no cross-chunk decoration yet).

use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME, ChunkData,
    ChunkKey,
};
use std::ops::RangeInclusive;

pub mod noise;

pub use noise::Frequency;

/// A chunk generator.
///
/// The world has no height limit: it asks for chunks at any height around its viewers, so a
/// generator must answer fast far above and below its terrain.
pub trait WorldGenerator: Send + Sync + 'static {
    /// Generates the chunk at `key` (lod 0 in M0). Must be deterministic: same inputs, same output.
    /// Must return `Uniform` storage, without walking the voxels, for chunks entirely above or
    /// below the terrain (fast path), at any height.
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData;
    /// Chunk-y range that may contain non-air blocks: every chunk outside it is uniform air. The
    /// world creates those chunks as air without calling [`WorldGenerator::generate`] (they still
    /// exist and can be edited: the range is not a height limit). The default,
    /// `i64::MIN..=i64::MAX`, promises nothing.
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        i64::MIN..=i64::MAX
    }
    /// Name for logs.
    fn name(&self) -> &str;
}

/// Horizontal layers from y = 0 upwards, e.g. `[("boxel:bedrock", 1), ("boxel:stone", 3),
/// ("boxel:dirt", 2), ("boxel:grass", 1)]`; air above, air below 0. Layer names are resolved
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
    /// Lattice cells per voxel of the first octave of the height fbm, an exact fraction so the
    /// terrain is the same function at any distance from the origin.
    pub frequency: Frequency,
    /// fbm octaves.
    pub octaves: u32,
    /// Dirt thickness under grass.
    pub dirt_depth: i32,
    /// Columns at or below this height get sand on top instead of grass.
    pub sand_level: i32,
    /// Columns above this height get snow on top.
    pub snow_level: i32,
    /// Lowest generated y (bedrock); below is air (and outside `vertical_chunks`).
    pub bottom: i32,
}

impl Default for TerrainSettings {
    fn default() -> Self {
        Self {
            base_height: 64,
            amplitude: 48.0,
            frequency: Frequency::one_in(256),
            octaves: 5,
            dirt_depth: 3,
            sand_level: 40,
            snow_level: 100,
            bottom: -64,
        }
    }
}

/// Lacunarity of the terrain height fbm.
const FBM_LACUNARITY: u32 = 2;
/// Gain of the terrain height fbm.
const FBM_GAIN: f64 = 0.5;

/// Heightmap terrain (fbm value noise): bedrock at `bottom`, stone, dirt, grass/sand/snow top.
/// Per chunk: Uniform air above the highest possible column or below `bottom`, and Uniform
/// stone below the lowest possible column minus `dirt_depth`, without computing heights;
/// otherwise compute the 32x32 column heights once and return Uniform air above the chunk's
/// highest column, Uniform stone fully below its lowest column minus `dirt_depth`.
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
    ///
    /// `base_height + round(amplitude * fbm_2d(seed, x, z, fbm))` with the settings' frequency
    /// and octaves, lacunarity 2 and gain 0.5, so it never leaves `base_height +-
    /// ceil(|amplitude|)`. The noise works on the integer columns ([`noise`]): exact at any
    /// `i64`, and bit-identical to the former `f64` noise inside `i32` for a power-of-two
    /// frequency denominator (the default `1/256`).
    pub fn height_at(&self, x: i64, z: i64) -> i32 {
        self.height_from_noise(noise::fbm_2d(self.seed, x, z, self.fbm()))
    }

    /// The height fbm.
    fn fbm(&self) -> noise::Fbm {
        let s = &self.settings;
        noise::Fbm {
            frequency: s.frequency,
            octaves: s.octaves,
            lacunarity: FBM_LACUNARITY,
            gain: FBM_GAIN,
        }
    }

    /// Heights of the 32x32 columns whose min corner is `(x0, z0)`, indexed `x * 32 + z` (like
    /// one y layer of a chunk). Equal to [`TerrainGenerator::height_at`] per column, computed
    /// with one lattice hash per corner and octave.
    fn column_heights(&self, x0: i64, z0: i64) -> [i32; CHUNK_AREA] {
        // `origin + i` cannot overflow: chunk origins are within the world limit (`+-2^61`).
        let axis = |origin: i64| -> [i64; CHUNK_SIZE as usize] {
            std::array::from_fn(|i| origin + i as i64)
        };
        let mut noise = [0.0; CHUNK_AREA];
        noise::fbm_2d_grid(self.seed, &axis(x0), &axis(z0), self.fbm(), &mut noise);
        noise.map(|n| self.height_from_noise(n))
    }

    /// `base_height + round(amplitude * n)` (saturating; `|amplitude * n| <= |amplitude|`).
    fn height_from_noise(&self, n: f64) -> i32 {
        let s = &self.settings;
        s.base_height
            .saturating_add((f64::from(s.amplitude) * n).round() as i32)
    }

    /// Upper bound of every column height: `base_height + ceil(|amplitude|)`.
    fn max_possible_height(&self) -> i32 {
        let s = &self.settings;
        s.base_height
            .saturating_add(f64::from(s.amplitude).abs().ceil() as i32)
    }

    /// Lower bound of every column height: `base_height - ceil(|amplitude|)`.
    fn min_possible_height(&self) -> i32 {
        let s = &self.settings;
        s.base_height
            .saturating_sub(f64::from(s.amplitude).abs().ceil() as i32)
    }
}

impl Default for TerrainGenerator {
    fn default() -> Self {
        Self::new(0x00b0_7e1d, TerrainSettings::default())
    }
}

impl WorldGenerator for TerrainGenerator {
    /// Fills each column from its height: air below `bottom`, bedrock at `bottom`, stone up to
    /// `h - dirt_depth`, dirt below `h`, the top block at `h` (sand if `h <= sand_level`, snow if
    /// `h > snow_level`, else grass) and air above. Chunks above every column (and above
    /// `bottom`) or below `bottom` are Uniform air, chunks fully inside the stone layer are
    /// Uniform stone. `key.lod` is ignored (M0 only generates lod 0).
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let s = &self.settings;
        let origin = key.pos.origin();
        let y0 = origin.y;
        let y1 = y0 + i64::from(CHUNK_SIZE - 1);
        // Heights and settings are i32; the chunk's y range is compared in i64.
        let bottom = i64::from(s.bottom);
        // Fast paths that need no heights: above every column, below the floor, and (the rule
        // below with the lowest possible column) fully inside the stone.
        if y1 < bottom || y0 > i64::from(self.max_possible_height().max(s.bottom)) {
            return ChunkData::uniform(BlockState::AIR);
        }
        if y0 > bottom && y1 < i64::from(self.min_possible_height().saturating_sub(s.dirt_depth)) {
            return ChunkData::uniform(block(registry, "boxel:stone", "TerrainGenerator"));
        }
        // Column heights, indexed like one y layer of the chunk: `x * 32 + z`.
        let heights = self.column_heights(origin.x, origin.z);
        let (min_h, max_h) = heights
            .iter()
            .fold((i32::MAX, i32::MIN), |(lo, hi), &h| (lo.min(h), hi.max(h)));
        // `max(bottom)`: the bedrock floor exists even under (degenerate) terrain below it.
        if y0 > i64::from(max_h.max(s.bottom)) {
            return ChunkData::uniform(BlockState::AIR);
        }
        let stone = block(registry, "boxel:stone", "TerrainGenerator");
        if y0 > bottom && y1 < i64::from(min_h.saturating_sub(s.dirt_depth)) {
            return ChunkData::uniform(stone);
        }
        let bedrock = block(registry, "boxel:bedrock", "TerrainGenerator");
        let dirt = block(registry, "boxel:dirt", "TerrainGenerator");
        let grass = block(registry, "boxel:grass", "TerrainGenerator");
        let sand = block(registry, "boxel:sand", "TerrainGenerator");
        let snow = block(registry, "boxel:snow", "TerrainGenerator");
        let top_block = |h: i32| {
            if h <= s.sand_level {
                sand
            } else if h > s.snow_level {
                snow
            } else {
                grass
            }
        };
        // World y to chunk-local y, clamped to the chunk.
        let local = |y: i32| (i64::from(y) - y0).clamp(0, CHUNK_SIZE.into()) as usize;
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        for (column, &h) in heights.iter().enumerate() {
            // The column rules as runs: each voxel takes the first run whose end is above it,
            // starting at `bottom` (below is air), so a run that ends below the previous one
            // is empty. Ends: bedrock `bottom + 1`, stone `h - dirt_depth`, dirt `h`, top
            // `h + 1`; air above.
            let mut from = s.bottom;
            for (end, state) in [
                (s.bottom.saturating_add(1), bedrock),
                (h.saturating_sub(s.dirt_depth), stone),
                (h, dirt),
                (h.saturating_add(1), top_block(h)),
            ] {
                let end = end.max(from);
                for ly in local(from)..local(end) {
                    // `LocalPos` index: y << 10 | x << 5 | z, and `column` is x << 5 | z.
                    dense[(ly << (2 * CHUNK_SHIFT)) | column] = state;
                }
                from = end;
            }
        }
        ChunkData::from_dense(&dense)
    }
    /// `(bottom >> 5) ..= ((base_height + ceil(|amplitude|) + 1) >> 5)`; the end never goes below
    /// the chunk of `bottom` (degenerate settings with the floor above every column).
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        let bottom = self.settings.bottom;
        let top = self.max_possible_height().saturating_add(1).max(bottom);
        i64::from(bottom >> CHUNK_SHIFT)..=i64::from(top >> CHUNK_SHIFT)
    }
    fn name(&self) -> &str {
        "terrain"
    }
}

/// State of block `name`; panics with the name if `registry` lacks it (a programmer error: the
/// generator and the registry do not match).
fn block(registry: &BlockRegistry, name: &str, generator: &str) -> BlockState {
    registry
        .by_name(name)
        .unwrap_or_else(|| panic!("{generator}: block {name:?} is not in the registry"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_heights_match_height_at() {
        let settings = [
            TerrainSettings::default(),
            TerrainSettings {
                frequency: Frequency::one_in(7),
                octaves: 7,
                amplitude: 12.5,
                ..TerrainSettings::default()
            },
            // Higher than one lattice cell per voxel: the per-point fallback.
            TerrainSettings {
                frequency: Frequency::new(19, 10),
                octaves: 2,
                ..TerrainSettings::default()
            },
            TerrainSettings {
                octaves: 0,
                ..TerrainSettings::default()
            },
        ];
        for (seed, s) in (3u64..).zip(settings) {
            let g = TerrainGenerator::new(seed, s);
            for (x0, z0) in [
                (0, 0),
                (-32, 64),
                (-4096, -1_000_000),
                (1 << 25, -(1 << 24)),
                // Across the former i32 edge, far out, and at the ends of i64.
                ((1 << 31) - 16, -(1 << 31) - 16),
                (1 << 40, -(1 << 50)),
                (-(1 << 61), (1 << 61) - 32),
                (i64::MAX - 31, i64::MIN),
            ] {
                let grid = g.column_heights(x0, z0);
                for (column, &h) in grid.iter().enumerate() {
                    let (x, z) = (x0 + (column >> 5) as i64, z0 + (column & 31) as i64);
                    assert_eq!(h, g.height_at(x, z), "seed {seed} column ({x}, {z})");
                }
            }
        }
    }
}
