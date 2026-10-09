//! [`OverworldGenerator`]: the heightmap surface over stone down to `i64::MIN`, carved by caves,
//! with trees.
//!
//! The generator is a chain of stages, each a pure function of `(seed, voxel)` with a point form
//! (the spec, for tests, spawning and debugging) and a bulk form per chunk that gives the same
//! voxels:
//!
//! 1. [`Heightmap`]: one surface height per column (the heightmap of
//!    [`TerrainGenerator`](crate::TerrainGenerator), bit for bit).
//! 2. [`SurfaceRules`]: grass, sand or snow on top, dirt under it, stone further down. No
//!    bedrock: the stone goes on forever.
//! 3. [`Caves`]: carve the stone ([`Caves`] describes the density model, [`CaveSettings`] the
//!    knobs).
//! 4. [`Trees`]: on grass whose top block the caves left solid.
//!
//! Per chunk, from the cheapest answer to the dearest:
//!
//! - Above every possible column and tree: air, without heights (and outside
//!   [`WorldGenerator::vertical_chunks`], so the world does not even ask).
//! - Below the reach of every surface rule (the deepest of the dirt, the cave fades, the
//!   entrances and the roof, under the lowest possible column): no heights; the caves' 405
//!   corners decide between `Uniform(stone)`, `Uniform(air)` and a stone/air 1-bit chunk built
//!   straight from the carve mask ([`ChunkData::from_occupancy`]).
//! - Below the dirt and the roof of the lowest possible column, but within reach of the cave
//!   fades or the entrances: the same, with only the heights of the 9x9 corner columns.
//! - Near the surface: the 32x32 column heights, then air above the chunk's highest column, the
//!   same 1-bit path when the whole chunk is stone before carving, or the dense path (surface
//!   rules, carve, trees, [`ChunkData::from_dense`]).

mod caves;
mod terrain;

pub use caves::{
    CarveMask, CaveSettings, Caves, Cheese, Entrances, Fade, Noodles, SurfaceFade, Tunnels,
    carve_mask,
};
pub use terrain::{Heightmap, SurfaceBlocks, SurfaceRules};

use crate::{Plants, Sea, Tree, Trees, WorldGenerator, block};
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME,
    ChunkData, ChunkKey, LocalPos,
};
use std::ops::RangeInclusive;

/// Name in logs and panics.
const NAME: &str = "OverworldGenerator";

/// Parameters of [`OverworldGenerator`].
#[derive(Clone, Debug, PartialEq)]
pub struct OverworldSettings {
    /// Surface heights.
    pub heightmap: Heightmap,
    /// Surface blocks.
    pub surface: SurfaceRules,
    /// Caves (`None`: plain stone under the surface).
    pub caves: Option<Caves>,
    /// Trees (`None`: no trees).
    pub trees: Option<Trees>,
    /// Short grass and flowers on grass whose top block the caves left solid, after the trees
    /// (`None`: no plants).
    pub plants: Option<Plants>,
    /// Water up to a sea level over the low columns, sand beaches, and no cave within
    /// [`Sea::FLOOR`] blocks under the water (ADR 0021; `None`: dry).
    pub sea: Option<Sea>,
}

impl Default for OverworldSettings {
    /// The default heightmap and surface, the default caves (with openings), trees, plants and
    /// sea.
    fn default() -> Self {
        Self {
            heightmap: Heightmap::default(),
            surface: SurfaceRules::default(),
            caves: Some(Caves {
                settings: CaveSettings::default(),
            }),
            trees: Some(Trees::default()),
            plants: Some(Plants::default()),
            sea: Some(Sea::default()),
        }
    }
}

/// The overworld: [`Heightmap`] surface, [`SurfaceRules`], stone down to `i64::MIN` carved by
/// [`Caves`], and [`Trees`] (see the [module docs](self) for the stages and the fast paths).
///
/// Exact everywhere in the world (`+-2^61` voxels on every axis, `MAX_BLOCK_COORD`, the limit
/// the world streams within): every stage works on integer columns and lattice corners, and
/// the carve decision is integer arithmetic. The point helpers ([`OverworldGenerator::block_at`],
/// [`OverworldGenerator::is_solid`], [`OverworldGenerator::density_at`]) take positions inside
/// that limit too (checked in debug builds): the cells at the ends of `i64` have no far corner.
#[derive(Clone, Debug)]
pub struct OverworldGenerator {
    /// World seed.
    pub seed: u64,
    /// Parameters.
    pub settings: OverworldSettings,
}

impl Default for OverworldGenerator {
    /// The default settings with the default seed of [`TerrainGenerator`](crate::TerrainGenerator).
    fn default() -> Self {
        Self::new(
            crate::TerrainGenerator::default().seed,
            OverworldSettings::default(),
        )
    }
}

impl OverworldGenerator {
    /// New generator.
    pub fn new(seed: u64, settings: OverworldSettings) -> Self {
        Self { seed, settings }
    }

    /// The surface rules with the sea's beaches: sand up to `level + beach`.
    fn rules(&self) -> SurfaceRules {
        let mut rules = self.settings.surface.clone();
        if let Some(sea) = self.settings.sea {
            rules.sand_level = rules.sand_level.max(sea.level.saturating_add(sea.beach));
        }
        rules
    }

    /// True if the sea keeps the caves out of `y` in a column of height `h`: under water, the
    /// top [`Sea::FLOOR`] blocks of ground are never carved.
    fn sea_floor(&self, y: i64, h: i64) -> bool {
        self.settings
            .sea
            .is_some_and(|sea| sea.covers(h) && y > h.saturating_sub(Sea::FLOOR))
    }

    /// The sea, if `boxel:water` is in `registry`, with its water.
    fn sea_water(&self, registry: &BlockRegistry) -> Option<(Sea, BlockState)> {
        Some((self.settings.sea?, registry.by_name("boxel:water")?))
    }

    /// Surface height of column `(x, z)` (before the caves).
    pub fn height_at(&self, x: i64, z: i64) -> i64 {
        self.settings.heightmap.height_at(self.seed, x, z)
    }

    /// True if the terrain (surface and caves, without trees) is solid at `p` (the spec).
    pub fn is_solid(&self, p: BlockPos) -> bool {
        self.is_solid_in(p, self.height_at(p.x, p.z))
    }

    /// [`OverworldGenerator::is_solid`] in a column of height `h`.
    fn is_solid_in(&self, p: BlockPos, h: i64) -> bool {
        p.y <= h && !self.carves(p, h)
    }

    /// True if the caves carve `p`, in a column of height `h` (the spec): not under the sea
    /// ([`Sea::FLOOR`]) nor beside it ([`OverworldGenerator::beside_sea`]).
    fn carves(&self, p: BlockPos, h: i64) -> bool {
        !self.sea_floor(p.y, h)
            && self
                .settings
                .caves
                .as_ref()
                .is_some_and(|caves| caves.carves(self.seed, p, h, |x, z| self.height_at(x, z)))
            && !self.beside_sea(p)
    }

    /// True if sea water lies beside `p` (in one of its 4 horizontal neighbors): the caves leave
    /// such a cell solid, as Minecraft's carvers keep out of the blocks next to a fluid. Water
    /// never stands next to a generated cave, so no cave opens sideways into the sea as a wall
    /// of still water (under the sea, [`Sea::FLOOR`] keeps the caves closed). Integer
    /// comparisons on the neighbors' heights.
    pub fn beside_sea(&self, p: BlockPos) -> bool {
        let Some(sea) = self.settings.sea else {
            return false;
        };
        if !self.shore_band(sea).contains(&p.y) {
            return false;
        }
        [(1, 0), (-1, 0), (0, 1), (0, -1)]
            .into_iter()
            .any(|(dx, dz)| sea.wets(self.height_at(p.x + dx, p.z + dz), p.y))
    }

    /// The heights at which a cell can have sea water beside it: from just above the lowest
    /// possible column up to the sea level.
    fn shore_band(&self, sea: Sea) -> RangeInclusive<i64> {
        self.settings.heightmap.min_height().saturating_add(1)..=sea.level
    }

    /// The cave density at voxel `p` (the trilinear sum of its cell's corners in density units;
    /// negative is carved where the roof allows), or `None` without caves. For debugging the
    /// look.
    pub fn density_at(&self, p: BlockPos) -> Option<f64> {
        let caves = self.settings.caves.as_ref()?;
        Some(caves.density_at(self.seed, p, |x, z| self.height_at(x, z)))
    }

    /// The block at `p`: what [`WorldGenerator::generate`] puts there, computed for that voxel
    /// alone (the spec the chunk path is tested against; slow).
    pub fn block_at(&self, p: BlockPos, registry: &BlockRegistry) -> BlockState {
        let h = self.height_at(p.x, p.z);
        if p.y <= h && !self.carves(p, h) {
            let blocks = SurfaceBlocks::new(registry, NAME);
            return self.rules().block_at(&blocks, h, p.y);
        }
        if let Some((sea, water)) = self.sea_water(registry)
            && sea.covers(h)
            && p.y > h
            && p.y <= sea.level
        {
            return water;
        }
        let (Some(trees), Some(log), Some(leaves)) = (
            &self.settings.trees,
            registry.by_name("boxel:log"),
            registry.by_name("boxel:leaves"),
        ) else {
            return BlockState::AIR;
        };
        let tree = trees.block_at(self.seed, p, |x, z| {
            let ground = self.height_at(x, z);
            let grows = self.rules().is_grass(ground)
                && self.is_solid_in(BlockPos::new(x, ground, z), ground);
            grows.then_some(ground)
        });
        match tree {
            Some(crate::TreeBlock::Log) => log,
            Some(crate::TreeBlock::Leaves) => leaves,
            None => self.plant_at(p, h, registry),
        }
    }

    /// The plant at `p` (air if none): on the cell above a solid grass top (`h` is the column's
    /// height).
    fn plant_at(&self, p: BlockPos, h: i64, registry: &BlockRegistry) -> BlockState {
        let (Some(plants), Some(blocks)) = (&self.settings.plants, Plants::blocks(registry)) else {
            return BlockState::AIR;
        };
        let grows = p.y == h + 1
            && self.rules().is_grass(h)
            && self.is_solid_in(BlockPos::new(p.x, h, p.z), h);
        match plants.plant_at(self.seed, p.x, p.z) {
            Some(kind) if grows => blocks.of(kind),
            _ => BlockState::AIR,
        }
    }

    /// Voxels above the surface that trees and plants may reach.
    fn reach_above(&self) -> i64 {
        if self.settings.trees.is_some() {
            Trees::HEIGHT
        } else {
            i64::from(self.settings.plants.is_some())
        }
    }

    /// Depth under the lowest possible column from which no surface rule applies: chunks wholly
    /// below it are stone (and caves) without heights.
    fn surface_reach(&self) -> i64 {
        let caves = self
            .settings
            .caves
            .as_ref()
            .map_or(0, |c| c.settings.surface_reach());
        let sea = if self.settings.sea.is_some() {
            Sea::FLOOR
        } else {
            0
        };
        caves.max(sea).max(self.settings.surface.dirt_depth)
    }

    /// True if every voxel of the chunk layer starting at `y0` is stone before carving and
    /// under the cave roof in a column of height `lowest` or more (so below every tree too).
    fn under_surface_rules(&self, y0: i64, lowest: i64) -> bool {
        let size = i64::from(CHUNK_SIZE);
        let mut roof = self
            .settings
            .caves
            .as_ref()
            .map_or(i64::MIN, |c| c.settings.surface_fade.roof);
        if self.settings.sea.is_some() {
            roof = roof.max(Sea::FLOOR);
        }
        y0 + size <= lowest.saturating_sub(self.settings.surface.dirt_depth)
            && y0 + size - 1 <= lowest.saturating_sub(roof)
    }

    /// Stone carved by the caves: the corners of the chunk at `origin` decide between uniform
    /// stone, uniform air and the 1-bit chunk. `corner_heights` gives the heights of its 9x9
    /// corner columns (called only with caves), or `None` deep under every depth rule. Without
    /// caves: uniform stone.
    fn carved_stone(
        &self,
        origin: BlockPos,
        corner_heights: impl FnOnce() -> Option<[i64; 81]>,
        stone: BlockState,
    ) -> ChunkData {
        let Some(caves) = &self.settings.caves else {
            return ChunkData::uniform(stone);
        };
        let q = caves.chunk_corners(self.seed, origin, corner_heights().as_ref());
        match carve_mask(&q) {
            CarveMask::Solid => ChunkData::uniform(stone),
            CarveMask::Air => ChunkData::uniform(BlockState::AIR),
            CarveMask::Mixed(occupancy) => {
                ChunkData::from_occupancy(&occupancy, stone, BlockState::AIR)
            }
        }
    }

    /// The dense path of a chunk near the surface: the surface rules per column, then the caves
    /// carve under the roof. Indexed like [`LocalPos::index`].
    fn surface_voxels(
        &self,
        origin: BlockPos,
        heights: &[i64; CHUNK_AREA],
        highest: i64,
        registry: &BlockRegistry,
    ) -> Vec<BlockState> {
        let blocks = SurfaceBlocks::new(registry, NAME);
        let y0 = origin.y;
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        let rules = self.rules();
        let sea = self.sea_water(registry);
        // World y to chunk-local y, clamped to the chunk.
        let local = |y: i64| (y.saturating_sub(y0)).clamp(0, CHUNK_SIZE.into()) as usize;
        for (column, &h) in heights.iter().enumerate() {
            // `LocalPos` index: y << 10 | x << 5 | z, and `column` is x << 5 | z.
            rules.fill_column(&blocks, h, y0, |ly, state| {
                dense[(ly << (2 * CHUNK_SHIFT)) | column] = state;
            });
            if let Some((sea, water)) = sea
                && sea.covers(h)
            {
                for ly in local(h.saturating_add(1))..local(sea.level.saturating_add(1)) {
                    dense[(ly << (2 * CHUNK_SHIFT)) | column] = water;
                }
            }
        }
        let Some(caves) = &self.settings.caves else {
            return dense;
        };
        let roof = caves.settings.surface_fade.roof;
        // No voxel is under the roof of its column.
        if y0 > highest.saturating_sub(roof) {
            return dense;
        }
        let corner_heights =
            Caves::corner_heights_from(&self.settings.heightmap, self.seed, origin, heights);
        let q = caves.chunk_corners(self.seed, origin, Some(&corner_heights));
        let solid = match carve_mask(&q) {
            CarveMask::Solid => return dense,
            CarveMask::Air => Box::new([0; CHUNK_AREA]),
            CarveMask::Mixed(occupancy) => occupancy,
        };
        let mut shore = Shore::new(self, origin, heights);
        for (i, &word) in solid.iter().enumerate() {
            // Word `ly * 32 + lx`, bit z.
            let (ly, lx) = (i >> CHUNK_SHIFT, i & 31);
            let y = y0 + ly as i64;
            let mut carved = !word;
            while carved != 0 {
                let lz = carved.trailing_zeros() as usize;
                carved &= carved - 1;
                let column = (lx << CHUNK_SHIFT) | lz;
                let h = heights[column];
                if y <= h.saturating_sub(roof) && !self.sea_floor(y, h) && !shore.wet(lx, lz, y) {
                    dense[(ly << (2 * CHUNK_SHIFT)) | column] = BlockState::AIR;
                }
            }
        }
        dense
    }

    /// [`OverworldGenerator::carved_stone`] for a chunk under the surface rules of its own
    /// lowest column (`heights` are its column heights), with the cells beside the sea left
    /// solid. Its cells lie at least [`Sea::FLOOR`] under every one of its columns, so only the
    /// columns just outside it can hold water beside them: only its border columns are checked,
    /// and the heights outside are computed only for a carved border cell low enough.
    fn carved_stone_ashore(
        &self,
        origin: BlockPos,
        heights: &[i64; CHUNK_AREA],
        stone: BlockState,
    ) -> ChunkData {
        let corners = || {
            Some(Caves::corner_heights_from(
                &self.settings.heightmap,
                self.seed,
                origin,
                heights,
            ))
        };
        let Some(sea) = self.settings.sea else {
            return self.carved_stone(origin, corners, stone);
        };
        let Some(caves) = &self.settings.caves else {
            return ChunkData::uniform(stone);
        };
        let band = self.shore_band(sea);
        let size = i64::from(CHUNK_SIZE);
        let q = caves.chunk_corners(self.seed, origin, corners().as_ref());
        let mut solid = match carve_mask(&q) {
            CarveMask::Solid => return ChunkData::uniform(stone),
            CarveMask::Air if origin.y > *band.end() || origin.y + size <= *band.start() => {
                return ChunkData::uniform(BlockState::AIR);
            }
            CarveMask::Air => Box::new([0; CHUNK_AREA]),
            CarveMask::Mixed(occupancy) => occupancy,
        };
        let mut shore = Shore::new(self, origin, heights);
        let last = CHUNK_SIZE as usize - 1;
        for ly in 0..CHUNK_SIZE as usize {
            let y = origin.y + ly as i64;
            if !band.contains(&y) {
                continue;
            }
            for lx in 0..=last {
                let word = &mut solid[(ly << CHUNK_SHIFT) | lx];
                // The border columns of this row: every z on the x sides, the two ends otherwise.
                let border = if lx == 0 || lx == last {
                    u32::MAX
                } else {
                    1 | 1 << last
                };
                let mut carved = !*word & border;
                while carved != 0 {
                    let lz = carved.trailing_zeros() as usize;
                    carved &= carved - 1;
                    if shore.wet(lx, lz, y) {
                        *word |= 1 << lz;
                    }
                }
            }
        }
        ChunkData::from_occupancy(&solid, stone, BlockState::AIR)
    }

    /// Ground heights trees may grow on.
    fn tree_grounds(&self) -> RangeInclusive<i64> {
        let s = self.rules();
        s.sand_level + 1..=s.snow_level.min(self.settings.heightmap.max_height())
    }

    /// The trees with a block in the chunk at `origin`, whose column heights are `heights` and
    /// whose terrain is `terrain` (`None`: all air), and the log and leaves blocks. Empty
    /// without trees or without those blocks.
    fn trees_in(
        &self,
        origin: BlockPos,
        heights: &[i64; CHUNK_AREA],
        terrain: Option<&[BlockState]>,
        registry: &BlockRegistry,
    ) -> (Vec<Tree>, BlockState, BlockState) {
        let (Some(trees), Some(log), Some(leaves)) = (
            &self.settings.trees,
            registry.by_name("boxel:log"),
            registry.by_name("boxel:leaves"),
        ) else {
            return (Vec::new(), BlockState::AIR, BlockState::AIR);
        };
        let size = i64::from(CHUNK_SIZE);
        let found = trees.reaching(self.seed, origin, self.tree_grounds(), |x, z| {
            // Trees stay in the chunk's columns (see `Trees`).
            let (lx, lz) = (x - origin.x, z - origin.z);
            let ground = heights[((lx << CHUNK_SHIFT) | lz) as usize];
            // Only trees with a block in this chunk need the (dearer) ground check.
            if !self.rules().is_grass(ground)
                || ground + 1 >= origin.y + size
                || ground + Trees::HEIGHT < origin.y
            {
                return None;
            }
            let ly = ground - origin.y;
            let solid = if (0..size).contains(&ly) {
                terrain.is_some_and(|t| {
                    t[LocalPos::new(lx as u32, ly as u32, lz as u32).index()] != BlockState::AIR
                })
            } else {
                self.is_solid_in(BlockPos::new(x, ground, z), ground)
            };
            solid.then_some(ground)
        });
        (found, log, leaves)
    }

    /// Puts the plants of the chunk at `origin` (column heights `heights`) into `dense` (`None`:
    /// all air; created when a plant goes in), on the air cells above a solid grass top.
    fn place_plants(
        &self,
        origin: BlockPos,
        heights: &[i64; CHUNK_AREA],
        dense: &mut Option<Vec<BlockState>>,
        registry: &BlockRegistry,
    ) {
        let (Some(plants), Some(blocks)) = (&self.settings.plants, Plants::blocks(registry)) else {
            return;
        };
        let size = i64::from(CHUNK_SIZE);
        for lx in 0..size {
            for lz in 0..size {
                let ground = heights[((lx << CHUNK_SHIFT) | lz) as usize];
                let ly = ground + 1 - origin.y;
                if !(0..size).contains(&ly) || !self.rules().is_grass(ground) {
                    continue;
                }
                let (x, z) = (origin.x + lx, origin.z + lz);
                let Some(kind) = plants.plant_at(self.seed, x, z) else {
                    continue;
                };
                self.put_plant(origin, [lx, ly, lz], ground, blocks.of(kind), dense);
            }
        }
    }

    /// Puts `plant` at local `[lx, ly, lz]` of the chunk at `origin` if that cell is air and the
    /// ground block under it (at height `ground`) is solid.
    fn put_plant(
        &self,
        origin: BlockPos,
        [lx, ly, lz]: [i64; 3],
        ground: i64,
        plant: BlockState,
        dense: &mut Option<Vec<BlockState>>,
    ) {
        let index = |ly: i64| LocalPos::new(lx as u32, ly as u32, lz as u32).index();
        let cell = dense.as_ref().map_or(BlockState::AIR, |d| d[index(ly)]);
        if cell != BlockState::AIR {
            return;
        }
        let solid = if ly >= 1 {
            dense
                .as_ref()
                .is_some_and(|d| d[index(ly - 1)] != BlockState::AIR)
        } else {
            self.is_solid_in(BlockPos::new(origin.x + lx, ground, origin.z + lz), ground)
        };
        if solid {
            dense.get_or_insert_with(|| vec![BlockState::AIR; CHUNK_VOLUME])[index(ly)] = plant;
        }
    }
}

impl WorldGenerator for OverworldGenerator {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let heightmap = &self.settings.heightmap;
        let origin = key.pos.origin();
        let (y0, size) = (origin.y, i64::from(CHUNK_SIZE));
        let sea_top = self.settings.sea.map_or(i64::MIN, |sea| sea.level);
        if y0 > (heightmap.max_height() + self.reach_above()).max(sea_top) {
            return ChunkData::uniform(BlockState::AIR);
        }
        let stone = || block(registry, "boxel:stone", NAME);
        // Deep: every corner and voxel is below every depth rule of every column.
        if y0 + size <= heightmap.min_height().saturating_sub(self.surface_reach()) {
            return self.carved_stone(origin, || None, stone());
        }
        // All stone before carving and under the roof of the lowest possible column: the caves
        // only need the heights of the corner columns (for the fades and the entrances).
        if self.under_surface_rules(y0, heightmap.min_height()) {
            let corners = || Some(Caves::corner_heights(heightmap, self.seed, origin));
            return self.carved_stone(origin, corners, stone());
        }
        let heights = heightmap.chunk_heights(self.seed, origin.x, origin.z);
        let (lowest, highest) = heights
            .iter()
            .fold((i64::MAX, i64::MIN), |(lo, hi), &h| (lo.min(h), hi.max(h)));
        if self.under_surface_rules(y0, lowest) {
            // The same under this chunk's lowest column (the columns around it may hold sea
            // water beside its border cells).
            return self.carved_stone_ashore(origin, &heights, stone());
        }
        // Air above the terrain, or the dense path; trees go into the dense voxels before they
        // are encoded once.
        let mut dense = (y0 <= highest.max(sea_top))
            .then(|| self.surface_voxels(origin, &heights, highest, registry));
        let (trees, log, leaves) = self.trees_in(origin, &heights, dense.as_deref(), registry);
        if !trees.is_empty() {
            let dense = dense.get_or_insert_with(|| vec![BlockState::AIR; CHUNK_VOLUME]);
            Trees::place_dense(&trees, origin, dense, log, leaves);
        }
        self.place_plants(origin, &heights, &mut dense, registry);
        dense.map_or_else(
            || ChunkData::uniform(BlockState::AIR),
            |d| ChunkData::from_dense(&d),
        )
    }

    /// `i64::MIN ..= (max_height + 1 + tree height) >> 5`: no floor, and air above the highest
    /// possible column and tree.
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        let mut top = self.settings.heightmap.max_height() + 1 + self.reach_above();
        if let Some(sea) = self.settings.sea {
            top = top.max(sea.level);
        }
        i64::MIN..=top >> CHUNK_SHIFT
    }

    fn name(&self) -> &str {
        "overworld"
    }
}

/// The sea beside the cells of one chunk ([`OverworldGenerator::beside_sea`]), from the heights
/// of its columns and, computed when a border cell first needs them, of the 4 rows of columns
/// just outside it.
struct Shore<'a> {
    generator: &'a OverworldGenerator,
    /// The sea and [`OverworldGenerator::shore_band`].
    sea: Option<(Sea, RangeInclusive<i64>)>,
    origin: BlockPos,
    heights: &'a [i64; CHUNK_AREA],
    /// The columns outside each side, along the side: `-x` and `+x` (by z), `-z` and `+z` (by
    /// x).
    outside: [Option<[i64; CHUNK_SIZE as usize]>; 4],
}

impl<'a> Shore<'a> {
    fn new(
        generator: &'a OverworldGenerator,
        origin: BlockPos,
        heights: &'a [i64; CHUNK_AREA],
    ) -> Self {
        let sea = generator
            .settings
            .sea
            .map(|sea| (sea, generator.shore_band(sea)));
        Self {
            generator,
            sea,
            origin,
            heights,
            outside: [None; 4],
        }
    }

    /// True if sea water lies beside local column `(lx, lz)` at height `y`.
    fn wet(&mut self, lx: usize, lz: usize, y: i64) -> bool {
        let Some(sea) = self
            .sea
            .as_ref()
            .and_then(|(sea, band)| band.contains(&y).then_some(*sea))
        else {
            return false;
        };
        let last = CHUNK_SIZE as usize - 1;
        let heights = self.heights;
        let inner = |x: usize, z: usize| heights[(x << CHUNK_SHIFT) | z];
        let mut beside = [0; 4];
        beside[0] = if lx > 0 {
            inner(lx - 1, lz)
        } else {
            self.side(0)[lz]
        };
        beside[1] = if lx < last {
            inner(lx + 1, lz)
        } else {
            self.side(1)[lz]
        };
        beside[2] = if lz > 0 {
            inner(lx, lz - 1)
        } else {
            self.side(2)[lx]
        };
        beside[3] = if lz < last {
            inner(lx, lz + 1)
        } else {
            self.side(3)[lx]
        };
        beside.into_iter().any(|h| sea.wets(h, y))
    }

    /// The heights of the columns outside side `k` (`-x`, `+x`, `-z`, `+z`).
    fn side(&mut self, k: usize) -> &[i64; CHUNK_SIZE as usize] {
        let (generator, origin) = (self.generator, self.origin);
        self.outside[k].get_or_insert_with(|| {
            let size = i64::from(CHUNK_SIZE);
            let along =
                |o: i64| -> [i64; CHUNK_SIZE as usize] { std::array::from_fn(|i| o + i as i64) };
            let mut out = [0; CHUNK_SIZE as usize];
            let heightmap = &generator.settings.heightmap;
            let seed = generator.seed;
            match k {
                0 => heightmap.heights(seed, &[origin.x - 1], &along(origin.z), &mut out),
                1 => heightmap.heights(seed, &[origin.x + size], &along(origin.z), &mut out),
                2 => heightmap.heights(seed, &along(origin.x), &[origin.z - 1], &mut out),
                _ => heightmap.heights(seed, &along(origin.x), &[origin.z + size], &mut out),
            }
            out
        })
    }
}
