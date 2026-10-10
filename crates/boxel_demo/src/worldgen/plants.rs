//! [`Plants`]: short grass and flowers on grass (ADR 0018). Whether a column has a plant, and
//! which, depends only on the seed and the column, so chunks agree across borders without
//! neighbor data; a plant goes on the cell above the ground block when that cell is air (trees
//! win).

use boxel_voxel::{BlockRegistry, BlockState};
use boxel_worldgen::noise::hash2;

/// Default salt of the plant hash (independent of the trees').
const PLANT_SALT: u64 = 0x0091_a275_f10e;

/// What grows on one column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlantKind {
    /// `boxel:short_grass`.
    ShortGrass,
    /// `boxel:poppy`.
    Flower,
}

/// Plants on grass: per column, a hash of the seed (salted) and the column picks short grass,
/// a flower or nothing. Cheap: one hash per column, and the ground height only for the columns
/// that grow something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plants {
    /// Salt of the column hash.
    pub salt: u64,
    /// Columns with short grass, per thousand.
    pub grass_per_mille: u32,
    /// Columns with a flower, per thousand.
    pub flowers_per_mille: u32,
}

impl Default for Plants {
    /// 12 % short grass, 1.2 % flowers.
    fn default() -> Self {
        Self {
            salt: PLANT_SALT,
            grass_per_mille: 120,
            flowers_per_mille: 12,
        }
    }
}

impl Plants {
    /// The plant of column `(x, z)`, if the ground there is grass.
    pub fn plant_at(&self, seed: u64, x: i64, z: i64) -> Option<PlantKind> {
        let roll = hash2(seed ^ self.salt, x, z) % 1000;
        if roll < self.flowers_per_mille {
            Some(PlantKind::Flower)
        } else if roll < self.flowers_per_mille + self.grass_per_mille {
            Some(PlantKind::ShortGrass)
        } else {
            None
        }
    }

    /// The block states of the two kinds (`None` when the registry lacks either).
    pub fn blocks(registry: &BlockRegistry) -> Option<PlantBlocks> {
        Some(PlantBlocks {
            grass: registry.by_name("boxel:short_grass")?,
            flower: registry.by_name("boxel:poppy")?,
        })
    }
}

/// The states [`Plants`] places.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantBlocks {
    /// Short grass.
    pub grass: BlockState,
    /// The flower.
    pub flower: BlockState,
}

impl PlantBlocks {
    /// The state of `kind`.
    pub fn of(self, kind: PlantKind) -> BlockState {
        match kind {
            PlantKind::ShortGrass => self.grass,
            PlantKind::Flower => self.flower,
        }
    }
}
