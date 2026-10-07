//! [`ForestGenerator`]: the M0 terrain with trees, the default world of the examples.

use crate::{PlantBlocks, Plants, TerrainGenerator, TerrainSettings, Trees, WorldGenerator};
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, CHUNK_SIZE, ChunkData, ChunkKey, LocalPos};
use std::ops::RangeInclusive;

/// [`TerrainGenerator`] plus [`Trees`] on its grass. Each chunk places the parts of every tree
/// whose blocks reach into it (trunk column heights come from
/// [`TerrainGenerator::height_at`]), so trees crossing chunk borders are consistent without
/// neighbor data.
///
/// This is the world the examples streamed as `Forest` before it moved here: for the same seed
/// it gave the same chunks until the plants came (ADR 0018). Without `boxel:log` or
/// `boxel:leaves` in the registry the trees are skipped; without `boxel:short_grass` or
/// `boxel:poppy`, the plants.
///
/// The trees go in with [`Trees::place`] ([`ChunkData::set`], about 60 us per chunk with trees)
/// on purpose: it is the default world of the stress runs, and the earlier runs (master and the
/// phase 0 baselines) paid that cost, so a faster path here would show up as an engine gain in
/// every comparison with them. [`Trees::place_dense`] (as in
/// [`OverworldGenerator`](crate::OverworldGenerator)) is the fast path; switching to it needs
/// new forest baselines.
#[derive(Clone, Debug)]
pub struct ForestGenerator {
    /// Height map, surface and seed.
    pub terrain: TerrainGenerator,
    /// Trees.
    pub trees: Trees,
    /// Short grass and flowers on grass, after the trees (`None`: no plants).
    pub plants: Option<Plants>,
}

impl Default for ForestGenerator {
    /// The default terrain with the default trees and plants.
    fn default() -> Self {
        Self {
            terrain: TerrainGenerator::default(),
            trees: Trees::default(),
            plants: Some(Plants::default()),
        }
    }
}

impl ForestGenerator {
    /// New generator with the default trees and plants.
    pub fn new(seed: u64, settings: TerrainSettings) -> Self {
        Self {
            terrain: TerrainGenerator::new(seed, settings),
            trees: Trees::default(),
            plants: Some(Plants::default()),
        }
    }

    /// Ground heights trees may grow on: grass, `sand_level + 1 ..= snow_level`.
    fn grounds(&self) -> RangeInclusive<i64> {
        let s = &self.terrain.settings;
        i64::from(s.sand_level) + 1..=s.snow_level.into()
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
        let mut data = self.terrain.generate(key, registry);
        let origin = key.pos.origin();
        if let (Some(log), Some(leaves)) = (
            registry.by_name("boxel:log"),
            registry.by_name("boxel:leaves"),
        ) {
            let grounds = self.grounds();
            let trees = self
                .trees
                .reaching(self.terrain.seed, origin, grounds.clone(), |x, z| {
                    let ground = i64::from(self.terrain.height_at(x, z));
                    grounds.contains(&ground).then_some(ground)
                });
            Trees::place(&trees, origin, &mut data, log, leaves);
        }
        if let (Some(plants), Some(blocks)) = (&self.plants, Plants::blocks(registry)) {
            self.place_plants(plants, blocks, origin, &mut data);
        }
        data
    }

    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        let inner = self.terrain.vertical_chunks();
        let tree_top = (self.grounds().end() + Trees::HEIGHT) >> 5;
        *inner.start()..=(*inner.end()).max(tree_top)
    }

    fn name(&self) -> &str {
        "forest"
    }
}
