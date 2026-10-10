//! The demo world generators: the M0 heightmap terrain, the forest (that terrain with trees,
//! plants and a sea) and the overworld (the heightmap over infinite stone carved by caves), and
//! their stages.
//!
//! Generators: [`TerrainGenerator`] (the M0 heightmap, frozen by its goldens),
//! [`ForestGenerator`] (that terrain with [`Trees`]) and [`OverworldGenerator`] (the heightmap
//! over infinite stone carved by caves, with trees; its stages are in [`overworld`]). Every
//! stage reads only its own chunk and the seed: features that cross chunk borders (trees, cave
//! corners) are recomputed by each chunk they touch. They implement the engine's
//! [`WorldGenerator`](boxel_worldgen::WorldGenerator) on the engine's noise
//! ([`boxel_worldgen::noise`]).

mod forest;
pub mod overworld;
mod plants;
mod sea;
mod terrain;
mod trees;

pub use forest::ForestGenerator;
pub use overworld::{
    CaveSettings, Caves, Heightmap, OverworldGenerator, OverworldSettings, SurfaceRules,
};
pub use plants::{PlantBlocks, PlantKind, Plants};
pub use sea::Sea;
pub use terrain::{TerrainGenerator, TerrainSettings};
pub use trees::{Tree, TreeBlock, Trees};

/// The seed of the demo worlds.
pub const DEFAULT_SEED: u64 = 0x00b0_7e1d;
