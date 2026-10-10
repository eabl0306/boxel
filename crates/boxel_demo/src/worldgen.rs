//! The demo world generators: the M0 heightmap terrain, the forest (that terrain with trees,
//! plants and a sea) and the overworld (the heightmap over infinite stone carved by caves), and
//! their stages.

pub use boxel_worldgen::{
    CaveSettings, Caves, ForestGenerator, Heightmap, OverworldGenerator, OverworldSettings,
    PlantBlocks, PlantKind, Plants, Sea, SurfaceRules, TerrainGenerator, TerrainSettings, Tree,
    TreeBlock, Trees, overworld,
};

/// The seed of the demo worlds.
pub const DEFAULT_SEED: u64 = 0x00b0_7e1d;
