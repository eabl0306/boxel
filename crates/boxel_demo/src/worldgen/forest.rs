//! [`ForestGenerator`]: the M0 terrain with trees, the default world of the examples.

use super::names::Role;
use super::{MissingBlocks, PlantBlocks, Plants, Sea, TerrainGenerator, TerrainSettings, Trees};
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, CHUNK_SIZE, ChunkData, ChunkKey, LocalPos};
use boxel_worldgen::{LodSource, WorldGenerator};
use std::ops::RangeInclusive;

/// [`TerrainGenerator`] plus [`Trees`] on its grass. Each chunk places the parts of every tree
/// whose blocks reach into it (trunk column heights come from
/// [`TerrainGenerator::height_at`]), so trees crossing chunk borders are consistent without
/// neighbor data.
///
/// This is the world the examples streamed as `Forest` before it moved here: for the same seed
/// it gave the same chunks until the plants came (ADR 0018). Its blocks are the terrain's
/// [`TerrainSettings::names`]. Without their log or leaves in the registry the trees are skipped;
/// without their short grass or flower, the plants.
///
/// The trees go in with [`Trees::place`] ([`ChunkData::set`], about 60 us per chunk with trees)
/// on purpose: it is the default world of the stress runs, and the earlier runs (master and the
/// phase 0 baselines) paid that cost, so a faster path here would show up as an engine gain in
/// every comparison with them. [`Trees::place_dense`] (as in
/// [`OverworldGenerator`](super::OverworldGenerator)) is the fast path; switching to it needs
/// new forest baselines.
#[derive(Clone, Debug)]
pub struct ForestGenerator {
    /// Height map, surface and seed.
    pub terrain: TerrainGenerator,
    /// Trees.
    pub trees: Trees,
    /// Short grass and flowers on grass, after the trees (`None`: no plants).
    pub plants: Option<Plants>,
    /// Water up to a sea level over the low columns, with sand beaches (ADR 0021; `None`: dry,
    /// the terrain as it was).
    pub sea: Option<Sea>,
}

impl Default for ForestGenerator {
    /// The default terrain with the default trees, plants and sea.
    fn default() -> Self {
        Self {
            terrain: TerrainGenerator::default(),
            trees: Trees::default(),
            plants: Some(Plants::default()),
            sea: Some(Sea::default()),
        }
    }
}

impl ForestGenerator {
    /// New generator with the default trees, plants and sea.
    pub fn new(seed: u64, settings: TerrainSettings) -> Self {
        Self {
            terrain: TerrainGenerator::new(seed, settings),
            trees: Trees::default(),
            plants: Some(Plants::default()),
            sea: Some(Sea::default()),
        }
    }

    /// `Ok` if `registry` has every block of the terrain's [`TerrainSettings::names`] this
    /// generator places: the terrain's, the log and leaves, the water with a sea, the plants with
    /// plants. Else the error that names each missing one (see
    /// [`BlockNames`](super::BlockNames)).
    pub fn check(&self, registry: &BlockRegistry) -> Result<(), MissingBlocks> {
        let roles = TerrainGenerator::roles()
            .chain(self.sea.map(|_| Role::Water))
            .chain(Role::TREES)
            .chain(self.plants.iter().flat_map(|_| Role::PLANTS));
        let names = &self.terrain.settings.names;
        names.require(registry, "ForestGenerator", roles)
    }

    /// Ground heights trees may grow on: grass, `sand_level + 1 ..= snow_level`, above the
    /// beaches of the sea.
    pub(crate) fn grounds(&self) -> RangeInclusive<i64> {
        let s = &self.terrain.settings;
        let mut sand = i64::from(s.sand_level);
        if let Some(sea) = self.sea {
            sand = sand.max(sea.level.saturating_add(sea.beach));
        }
        sand + 1..=s.snow_level.into()
    }

    /// Puts the plants of the chunk at `origin` on the air cells above grass. Only the columns
    /// that grow something look up their height, and only in chunks that can hold a plant.
    fn place_plants(
        &self,
        plants: &Plants,
        blocks: PlantBlocks,
        origin: BlockPos,
        data: &mut ChunkData,
    ) {
        let size = i64::from(CHUNK_SIZE);
        let grounds = self.grounds();
        if origin.y > grounds.end() + 1 || origin.y + size <= grounds.start() + 1 {
            return;
        }
        for lx in 0..size {
            for lz in 0..size {
                let (x, z) = (origin.x + lx, origin.z + lz);
                let Some(kind) = plants.plant_at(self.terrain.seed, x, z) else {
                    continue;
                };
                let ground = i64::from(self.terrain.height_at(x, z));
                let ly = ground + 1 - origin.y;
                if !grounds.contains(&ground) || !(0..size).contains(&ly) {
                    continue;
                }
                let p = LocalPos::new(lx as u32, ly as u32, lz as u32);
                if data.get(p) == BlockState::AIR {
                    data.set(p, blocks.of(kind));
                }
            }
        }
    }
}

impl WorldGenerator for ForestGenerator {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let mut data = self.terrain.generate_with(key, registry, self.sea);
        let origin = key.pos.origin();
        let names = &self.terrain.settings.names;
        if let (Some(log), Some(leaves)) =
            (registry.by_name(names.log), registry.by_name(names.leaves))
        {
            let grounds = self.grounds();
            let trees = self
                .trees
                .reaching(self.terrain.seed, origin, grounds.clone(), |x, z| {
                    let ground = i64::from(self.terrain.height_at(x, z));
                    grounds.contains(&ground).then_some(ground)
                });
            Trees::place(&trees, origin, &mut data, log, leaves);
        }
        if let (Some(plants), Some(blocks)) = (&self.plants, Plants::blocks_named(registry, names))
        {
            self.place_plants(plants, blocks, origin, &mut data);
        }
        data
    }

    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        let inner = self.terrain.vertical_chunks_with(self.sea);
        let tree_top = (self.grounds().end() + Trees::HEIGHT) >> 5;
        *inner.start()..=(*inner.end()).max(tree_top)
    }

    fn name(&self) -> &str {
        "forest"
    }

    /// Its own coarse terrain (ADR 0031), with the trees and the sea.
    fn lod(&self) -> Option<&dyn LodSource> {
        Some(self)
    }
}
