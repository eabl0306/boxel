//! The surface stages: [`Heightmap`] (one height per column) and [`SurfaceRules`] (which block
//! each depth of a column gets).

use crate::worldgen::TerrainSettings;
use boxel_voxel::{BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SIZE};
use boxel_worldgen::Frequency;
use boxel_worldgen::noise::{self, Fbm};

/// Lacunarity of the height fbm.
const LACUNARITY: u32 = 2;
/// Gain of the height fbm.
const GAIN: f64 = 0.5;

/// The surface height of every column: `base_height + round(amplitude * fbm_2d(seed, x, z))`
/// with lacunarity 2 and gain 0.5, the heightmap of
/// [`TerrainGenerator`](crate::worldgen::TerrainGenerator) (which uses this type). It never
/// leaves `base_height +- ceil(|amplitude|)`.
///
/// The fields keep the types of [`TerrainSettings`], so the arithmetic (`f32` amplitude, `i32`
/// saturating sum) is the same bit for bit; heights come out as `i64` world coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Heightmap {
    /// Average surface height (voxels).
    pub base_height: i32,
    /// Max deviation from `base_height` (voxels).
    pub amplitude: f32,
    /// Lattice cells per voxel of the first octave, an exact fraction.
    pub frequency: Frequency,
    /// fbm octaves.
    pub octaves: u32,
}

impl Default for Heightmap {
    /// The [`TerrainSettings`] defaults: 64 +- 48, `1 / 256`, 5 octaves.
    fn default() -> Self {
        Self::from_terrain(&TerrainSettings::default())
    }
}

impl Heightmap {
    /// The heightmap of a [`TerrainGenerator`](crate::worldgen::TerrainGenerator) with `settings`.
    pub fn from_terrain(settings: &TerrainSettings) -> Self {
        Self {
            base_height: settings.base_height,
            amplitude: settings.amplitude,
            frequency: settings.frequency,
            octaves: settings.octaves,
        }
    }

    /// The height fbm.
    pub fn fbm(&self) -> Fbm {
        Fbm {
            frequency: self.frequency,
            octaves: self.octaves,
            lacunarity: LACUNARITY,
            gain: GAIN,
        }
    }

    /// Height of the column `(x, z)`: the y of its top solid block.
    pub fn height_at(&self, seed: u64, x: i64, z: i64) -> i64 {
        self.height_from_noise(noise::fbm_2d(seed, x, z, self.fbm()))
    }

    /// [`Heightmap::height_at`] at every column of the grid `xs` x `zs` (`out[i * zs.len() + j]`
    /// gets column `(xs[i], zs[j])`), bit-identical to it and computed with
    /// [`noise::fbm_2d_grid`].
    ///
    /// # Panics
    ///
    /// If `out.len()` is not `xs.len() * zs.len()`.
    pub fn heights(&self, seed: u64, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        self.heights_with(seed, xs, zs, &mut vec![0.0; out.len()], out);
    }

    /// [`Heightmap::heights`] with a scratch buffer for the noise (`noise.len() == out.len()`).
    pub(crate) fn heights_with(
        &self,
        seed: u64,
        xs: &[i64],
        zs: &[i64],
        noise: &mut [f64],
        out: &mut [i64],
    ) {
        noise::fbm_2d_grid(seed, xs, zs, self.fbm(), noise);
        for (h, &n) in out.iter_mut().zip(noise.iter()) {
            *h = self.height_from_noise(n);
        }
    }

    /// Heights of the 32x32 columns whose lowest corner is `(x0, z0)`, indexed `x * 32 + z` (like
    /// one y layer of a chunk).
    pub fn chunk_heights(&self, seed: u64, x0: i64, z0: i64) -> [i64; CHUNK_AREA] {
        // `origin + i` cannot overflow: chunk origins are within the world limit (`+-2^61`).
        let axis = |origin: i64| -> [i64; CHUNK_SIZE as usize] {
            std::array::from_fn(|i| origin + i as i64)
        };
        let mut out = [0; CHUNK_AREA];
        self.heights_with(seed, &axis(x0), &axis(z0), &mut [0.0; CHUNK_AREA], &mut out);
        out
    }

    /// Upper bound of every column height: `base_height + ceil(|amplitude|)` (saturating).
    pub fn max_height(&self) -> i64 {
        self.base_height
            .saturating_add(f64::from(self.amplitude).abs().ceil() as i32)
            .into()
    }

    /// Lower bound of every column height: `base_height - ceil(|amplitude|)` (saturating).
    pub fn min_height(&self) -> i64 {
        self.base_height
            .saturating_sub(f64::from(self.amplitude).abs().ceil() as i32)
            .into()
    }

    /// `base_height + round(amplitude * n)`, saturating in `i32` (`|amplitude * n| <=
    /// |amplitude|`).
    fn height_from_noise(&self, n: f64) -> i64 {
        self.base_height
            .saturating_add((f64::from(self.amplitude) * n).round() as i32)
            .into()
    }
}

/// Which block each depth of a column gets, from its height `h`: the top block at `h` (sand if
/// `h <= sand_level`, snow if `h > snow_level`, else grass), dirt for the `dirt_depth` blocks
/// below it, stone further down, and air above `h`. The rules of
/// [`TerrainGenerator`](crate::worldgen::TerrainGenerator) without its bedrock floor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceRules {
    /// Dirt thickness under the top block.
    pub dirt_depth: i64,
    /// Columns at or below this height get sand on top instead of grass.
    pub sand_level: i64,
    /// Columns above this height get snow on top.
    pub snow_level: i64,
}

impl Default for SurfaceRules {
    /// The [`TerrainSettings`] defaults: 3 dirt, sand up to 40, snow above 100.
    fn default() -> Self {
        Self::from_terrain(&TerrainSettings::default())
    }
}

impl SurfaceRules {
    /// The rules of a [`TerrainGenerator`](crate::worldgen::TerrainGenerator) with `settings`.
    pub fn from_terrain(settings: &TerrainSettings) -> Self {
        Self {
            dirt_depth: settings.dirt_depth.into(),
            sand_level: settings.sand_level.into(),
            snow_level: settings.snow_level.into(),
        }
    }

    /// True if a column of height `h` has grass on top (where trees grow).
    pub fn is_grass(&self, h: i64) -> bool {
        h > self.sand_level && h <= self.snow_level
    }

    /// The block at height `y` of a column of height `h`.
    pub fn block_at(&self, blocks: &SurfaceBlocks, h: i64, y: i64) -> BlockState {
        if y > h {
            BlockState::AIR
        } else if y == h {
            self.top_block(blocks, h)
        } else if y >= h.saturating_sub(self.dirt_depth) {
            blocks.dirt
        } else {
            blocks.stone
        }
    }

    /// The top block of a column of height `h`.
    fn top_block(&self, blocks: &SurfaceBlocks, h: i64) -> BlockState {
        if h <= self.sand_level {
            blocks.sand
        } else if h > self.snow_level {
            blocks.snow
        } else {
            blocks.grass
        }
    }

    /// Fills `column` (32 voxels from height `y0`, any stride) like [`SurfaceRules::block_at`]
    /// per voxel, as runs.
    pub(crate) fn fill_column(
        &self,
        blocks: &SurfaceBlocks,
        h: i64,
        y0: i64,
        mut put: impl FnMut(usize, BlockState),
    ) {
        // World y to chunk-local y, clamped to the chunk.
        let local = |y: i64| (y.saturating_sub(y0)).clamp(0, CHUNK_SIZE.into()) as usize;
        let mut from = i64::MIN;
        for (end, state) in [
            (h.saturating_sub(self.dirt_depth), blocks.stone),
            (h, blocks.dirt),
            (h.saturating_add(1), self.top_block(blocks, h)),
        ] {
            let end = end.max(from);
            for ly in local(from)..local(end) {
                put(ly, state);
            }
            from = end;
        }
    }
}

/// The blocks of the surface rules, resolved from a registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceBlocks {
    /// `boxel:stone`.
    pub stone: BlockState,
    /// `boxel:dirt`.
    pub dirt: BlockState,
    /// `boxel:grass`.
    pub grass: BlockState,
    /// `boxel:sand`.
    pub sand: BlockState,
    /// `boxel:snow`.
    pub snow: BlockState,
}

impl SurfaceBlocks {
    /// The built-in blocks; panics with the name if `registry` lacks one (the generator and the
    /// registry do not match).
    pub fn new(registry: &BlockRegistry, generator: &str) -> Self {
        let block = |name| boxel_worldgen::block(registry, name, generator);
        Self {
            stone: block("boxel:stone"),
            dirt: block("boxel:dirt"),
            grass: block("boxel:grass"),
            sand: block("boxel:sand"),
            snow: block("boxel:snow"),
        }
    }
}
