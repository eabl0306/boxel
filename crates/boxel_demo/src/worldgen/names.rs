//! [`BlockNames`]: the blocks the demo generators place, by name, so a game reuses the
//! generators with blocks of its own.

use boxel_voxel::BlockRegistry;
use std::fmt;

/// The names of the blocks the demo generators place, looked up in the registry of each chunk
/// they generate, as they always were. The default is the demo's `boxel:` names, so the default
/// generators make the same chunks and far cells as before the names were settings; a game with
/// its own registry sets its own names ([`TerrainSettings::names`](super::TerrainSettings::names),
/// [`OverworldSettings::names`](super::OverworldSettings::names)) and gets the same world with
/// its blocks.
///
/// The generators place each block's default state ([`BlockRegistry::by_name`]). The surface
/// blocks must be in the registry (generation panics without them); without the water there is
/// no sea, without the log or the leaves no trees, without the short grass or the flower no
/// plants. `check` on each generator
/// ([`OverworldGenerator::check`](super::OverworldGenerator::check),
/// [`ForestGenerator::check`](super::ForestGenerator::check),
/// [`TerrainGenerator::check`](super::TerrainGenerator::check)) reports every name its settings
/// use and the registry lacks, as a [`MissingBlocks`] error, before anything is generated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockNames {
    /// Under the dirt, down to the floor (and the deep ground of the far cells): `boxel:stone`.
    pub stone: &'static str,
    /// Under the top block: `boxel:dirt`.
    pub dirt: &'static str,
    /// The top block between the sand and the snow levels (where trees and plants grow):
    /// `boxel:grass`.
    pub grass: &'static str,
    /// The top block at or below the sand level and on the beaches: `boxel:sand`.
    pub sand: &'static str,
    /// The top block above the snow level: `boxel:snow`.
    pub snow: &'static str,
    /// The floor of the terrain and the forest (the overworld has none): `boxel:bedrock`.
    pub bedrock: &'static str,
    /// The sea: `boxel:water`.
    pub water: &'static str,
    /// Tree trunks: `boxel:log`.
    pub log: &'static str,
    /// Tree crowns: `boxel:leaves`.
    pub leaves: &'static str,
    /// The common plant ([`PlantKind::ShortGrass`](super::PlantKind)): `boxel:short_grass`.
    pub short_grass: &'static str,
    /// The rare plant ([`PlantKind::Flower`](super::PlantKind)): `boxel:poppy`.
    pub flower: &'static str,
}

impl Default for BlockNames {
    /// The demo's blocks ([`crate::blocks`]).
    fn default() -> Self {
        Self {
            stone: "boxel:stone",
            dirt: "boxel:dirt",
            grass: "boxel:grass",
            sand: "boxel:sand",
            snow: "boxel:snow",
            bedrock: "boxel:bedrock",
            water: "boxel:water",
            log: "boxel:log",
            leaves: "boxel:leaves",
            short_grass: "boxel:short_grass",
            flower: "boxel:poppy",
        }
    }
}

/// What each name is for, the field's name: what [`MissingBlocks`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Role {
    Stone,
    Dirt,
    Grass,
    Sand,
    Snow,
    Bedrock,
    Water,
    Log,
    Leaves,
    ShortGrass,
    Flower,
}

impl Role {
    /// The surface blocks every generator places.
    pub(crate) const SURFACE: [Self; 5] =
        [Self::Stone, Self::Dirt, Self::Grass, Self::Sand, Self::Snow];
    /// The trees.
    pub(crate) const TREES: [Self; 2] = [Self::Log, Self::Leaves];
    /// The plants.
    pub(crate) const PLANTS: [Self; 2] = [Self::ShortGrass, Self::Flower];
}

impl BlockNames {
    /// Every name with its field's name, in field order: `("stone", "boxel:stone")` first.
    pub fn entries(&self) -> [(&'static str, &'static str); 11] {
        [
            ("stone", self.stone),
            ("dirt", self.dirt),
            ("grass", self.grass),
            ("sand", self.sand),
            ("snow", self.snow),
            ("bedrock", self.bedrock),
            ("water", self.water),
            ("log", self.log),
            ("leaves", self.leaves),
            ("short_grass", self.short_grass),
            ("flower", self.flower),
        ]
    }

    /// The name of `role`, with its field's name.
    fn entry(&self, role: Role) -> (&'static str, &'static str) {
        self.entries()[role as usize]
    }

    /// `Ok` if `registry` has every block of `roles`, else the error that names the missing ones
    /// (in the order of `roles`) for `generator`.
    pub(crate) fn require(
        &self,
        registry: &BlockRegistry,
        generator: &'static str,
        roles: impl IntoIterator<Item = Role>,
    ) -> Result<(), MissingBlocks> {
        let missing: Vec<_> = roles
            .into_iter()
            .map(|role| self.entry(role))
            .filter(|(_, name)| registry.block_by_name(name).is_none())
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(MissingBlocks { generator, missing })
        }
    }
}

/// The blocks a generator's settings name that its registry lacks (`check` on each generator,
/// see [`BlockNames`]): a game whose names and registry disagree learns it when it builds its
/// world, not from a panic on a worker thread or from a world without its sea.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingBlocks {
    /// The generator (`"OverworldGenerator"`), as its panics name it.
    pub generator: &'static str,
    /// Each missing block: the [`BlockNames`] field and the name it holds, `("water",
    /// "game:water")`, in field order.
    pub missing: Vec<(&'static str, &'static str)>,
}

impl fmt::Display for MissingBlocks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: the registry has no block named", self.generator)?;
        for (i, (field, name)) in self.missing.iter().enumerate() {
            let sep = if i == 0 { " " } else { ", " };
            write!(f, "{sep}{name:?} (the {field})")?;
        }
        let them = if self.missing.len() == 1 {
            "it"
        } else {
            "them"
        };
        write!(f, "; register {them} or change the generator's BlockNames")
    }
}

impl std::error::Error for MissingBlocks {}
