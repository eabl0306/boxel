//! [`TerrainGenerator`]: the M0 heightmap terrain.

use super::{Heightmap, Sea};
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME, ChunkData,
    ChunkKey,
};
use boxel_worldgen::{Frequency, LodSource, WorldGenerator, block};
use std::ops::RangeInclusive;

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
    /// ceil(|amplitude|)`. The noise works on the integer columns
    /// ([`noise`](boxel_worldgen::noise)): exact at any `i64`. With a power-of-two frequency
    /// denominator it is bit-identical to the former `f64` noise wherever that one was exact:
    /// every octave's lattice cells (both corners) inside `i32` and `x * num` exact in `f64`
    /// (every `i32` column with the default `1/256` and 5 octaves; more octaves or a higher
    /// frequency saturated the former noise sooner). Other denominators differ from it by its
    /// rounding.
    pub fn height_at(&self, x: i64, z: i64) -> i32 {
        // Computed in i32 (`Heightmap` keeps the settings' types), so it fits.
        self.heightmap().height_at(self.seed, x, z) as i32
    }

    /// The heightmap stage of these settings.
    pub fn heightmap(&self) -> Heightmap {
        Heightmap::from_terrain(&self.settings)
    }

    /// Heights of the 32x32 columns whose min corner is `(x0, z0)`, indexed `x * 32 + z` (like
    /// one y layer of a chunk). Equal to [`TerrainGenerator::height_at`] per column, computed
    /// with one lattice hash per corner and octave.
    fn column_heights(&self, x0: i64, z0: i64) -> [i32; CHUNK_AREA] {
        self.heightmap()
            .chunk_heights(self.seed, x0, z0)
            .map(|h| h as i32)
    }

    /// Upper bound of every column height: `base_height + ceil(|amplitude|)`.
    fn max_possible_height(&self) -> i32 {
        self.heightmap().max_height() as i32
    }

    /// Lower bound of every column height: `base_height - ceil(|amplitude|)`.
    fn min_possible_height(&self) -> i32 {
        self.heightmap().min_height() as i32
    }
}

impl Default for TerrainGenerator {
    /// The default settings with [`DEFAULT_SEED`](super::DEFAULT_SEED).
    fn default() -> Self {
        Self::new(super::DEFAULT_SEED, TerrainSettings::default())
    }
}

impl WorldGenerator for TerrainGenerator {
    /// Fills each column from its height: air below `bottom`, bedrock at `bottom`, stone up to
    /// `h - dirt_depth`, dirt below `h`, the top block at `h` (sand if `h <= sand_level`, snow if
    /// `h > snow_level`, else grass) and air above. Chunks above every column (and above
    /// `bottom`) or below `bottom` are Uniform air, chunks fully inside the stone layer are
    /// Uniform stone. `key.lod` is ignored (M0 only generates lod 0).
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        self.generate_with(key, registry, None)
    }
    /// `(bottom >> 5) ..= ((base_height + ceil(|amplitude|) + 1) >> 5)`; the end never goes below
    /// the chunk of `bottom` (degenerate settings with the floor above every column).
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        self.vertical_chunks_with(None)
    }
    fn name(&self) -> &str {
        "terrain"
    }
    /// Its own coarse terrain (ADR 0031).
    fn lod(&self) -> Option<&dyn LodSource> {
        Some(self)
    }
}

impl TerrainGenerator {
    /// [`WorldGenerator::vertical_chunks`] with `sea` (its water may rise above every column).
    pub(crate) fn vertical_chunks_with(&self, sea: Option<Sea>) -> RangeInclusive<i64> {
        let bottom = self.settings.bottom;
        let mut top = self.max_possible_height().saturating_add(1).max(bottom);
        if let Some(sea) = sea {
            top = top.max(sea_level_i32(sea));
        }
        i64::from(bottom >> CHUNK_SHIFT)..=i64::from(top >> CHUNK_SHIFT)
    }

    /// [`WorldGenerator::generate`] with `sea`: source water from above each low column up to
    /// the sea level and sand on the columns of [`Sea::sandy`] (no water without `boxel:water`
    /// in the registry). `None` is the terrain as it was.
    pub(crate) fn generate_with(
        &self,
        key: ChunkKey,
        registry: &BlockRegistry,
        sea: Option<Sea>,
    ) -> ChunkData {
        let s = &self.settings;
        let origin = key.pos.origin();
        let water = sea.and_then(|_| registry.by_name("boxel:water"));
        let sea = sea.filter(|_| water.is_some());
        // The highest block of any column: its top, or the water over it.
        let sea_top = sea.map_or(i32::MIN, sea_level_i32);
        let y0 = origin.y;
        let y1 = y0 + i64::from(CHUNK_SIZE - 1);
        // Heights and settings are i32; the chunk's y range is compared in i64.
        let bottom = i64::from(s.bottom);
        // Fast paths that need no heights: above every column, below the floor, and (the rule
        // below with the lowest possible column) fully inside the stone.
        if y1 < bottom || y0 > i64::from(self.max_possible_height().max(s.bottom).max(sea_top)) {
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
        if y0 > i64::from(max_h.max(s.bottom).max(sea_top)) {
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
        let water = water.unwrap_or(BlockState::AIR);
        let sandy = |h: i32| sea.is_some_and(|sea| sea.sandy(i64::from(h)));
        let top_block = |h: i32| {
            if h <= s.sand_level || sandy(h) {
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
            // `h + 1`, sea water `sea level + 1`; air above.
            let mut from = s.bottom;
            for (end, state) in [
                (s.bottom.saturating_add(1), bedrock),
                (h.saturating_sub(s.dirt_depth), stone),
                (h, dirt),
                (h.saturating_add(1), top_block(h)),
                (sea_top.saturating_add(1), water),
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
}

/// The sea level as a terrain height (`i32`, saturating).
fn sea_level_i32(sea: Sea) -> i32 {
    sea.level.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
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
