//! [`ForestGenerator`]: the M0 terrain with trees, the default world of the examples.

use crate::{TerrainGenerator, TerrainSettings, Trees, WorldGenerator};
use boxel_voxel::{BlockRegistry, ChunkData, ChunkKey};
use std::ops::RangeInclusive;

/// [`TerrainGenerator`] plus [`Trees`] on its grass. Each chunk places the parts of every tree
/// whose blocks reach into it (trunk column heights come from
/// [`TerrainGenerator::height_at`]), so trees crossing chunk borders are consistent without
/// neighbor data.
///
/// This is the world the examples streamed as `Forest` before it moved here: for the same seed
/// it gives the same chunks. Without `boxel:log` or `boxel:leaves` in the registry the trees
/// are skipped.
#[derive(Clone, Debug, Default)]
pub struct ForestGenerator {
    /// Height map, surface and seed.
    pub terrain: TerrainGenerator,
    /// Trees.
    pub trees: Trees,
}

impl ForestGenerator {
    /// New generator with the default trees.
    pub fn new(seed: u64, settings: TerrainSettings) -> Self {
        Self {
            terrain: TerrainGenerator::new(seed, settings),
            trees: Trees::default(),
        }
    }

    /// Ground heights trees may grow on: grass, `sand_level + 1 ..= snow_level`.
    fn grounds(&self) -> RangeInclusive<i64> {
        let s = &self.terrain.settings;
        i64::from(s.sand_level) + 1..=s.snow_level.into()
    }
}

impl WorldGenerator for ForestGenerator {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let mut data = self.terrain.generate(key, registry);
        let (Some(log), Some(leaves)) = (
            registry.by_name("boxel:log"),
            registry.by_name("boxel:leaves"),
        ) else {
            return data;
        };
        let origin = key.pos.origin();
        let grounds = self.grounds();
        let trees = self
            .trees
            .reaching(self.terrain.seed, origin, grounds.clone(), |x, z| {
                let ground = i64::from(self.terrain.height_at(x, z));
                grounds.contains(&ground).then_some(ground)
            });
        Trees::place(&trees, origin, &mut data, log, leaves);
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
