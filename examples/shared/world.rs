//! The world options shared by the examples: `--world`, `--caves`, `--no-trees` and `--no-sea`,
//! the generator they build, and the render capacity each world needs.
#![allow(
    dead_code,
    reason = "each example uses its own part (headless_world has no renderer)"
)]

use boxel::prelude::*;
use boxel_demo::worldgen::{
    CaveSettings, Caves, ForestGenerator, OverworldGenerator, OverworldSettings, Sea,
    TerrainGenerator, TerrainSettings,
};
use std::sync::Arc;

/// Help lines of the world options (two-space indent, flag column of 20), with the example's
/// default world.
pub fn usage(default: World) -> String {
    format!(
        "\
  --world NAME        flat, terrain, forest (terrain with trees) or overworld (the heightmap
                      over stone that goes down forever, with caves and trees); default {}
  --caves MODE        overworld caves: mc (default, with openings), closed (none reach the
                      surface) or off
  --no-trees          no trees or plants (forest becomes terrain)
  --no-sea            no sea: the forest and the overworld stay dry (water up to y = 46 by
                      default, ADR 0021)",
        default.name()
    )
}

/// The camera's view distance for `viewer`: where its meshed columns are complete
/// (`ChunkViewer::mesh_reach`, 340 blocks at radius 12 instead of 384), so the distance fog
/// hides every step of their edge (issue #51). At radius 0 and 1 that is nothing, so a chunk is
/// drawn instead, steps and all.
pub fn view_distance(viewer: ChunkViewer) -> f32 {
    viewer.mesh_reach().max(32.0)
}

/// The generator family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum World {
    /// Flat layers up to y = 63.
    Flat,
    /// The M0 heightmap with a bedrock floor.
    Terrain,
    /// The terrain with trees.
    Forest,
    /// The heightmap over infinite stone, with caves and trees.
    Overworld,
}

impl World {
    const ALL: [World; 4] = [World::Flat, World::Terrain, World::Forest, World::Overworld];

    pub fn name(self) -> &'static str {
        match self {
            World::Flat => "flat",
            World::Terrain => "terrain",
            World::Forest => "forest",
            World::Overworld => "overworld",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.name() == name)
    }
}

/// The overworld caves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaveMode {
    /// No caves.
    Off,
    /// Caves that never reach the surface.
    Closed,
    /// The Minecraft-like default, with openings.
    Mc,
}

impl CaveMode {
    const ALL: [CaveMode; 3] = [CaveMode::Off, CaveMode::Closed, CaveMode::Mc];

    pub fn name(self) -> &'static str {
        match self {
            CaveMode::Off => "off",
            CaveMode::Closed => "closed",
            CaveMode::Mc => "mc",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    fn settings(self) -> Option<CaveSettings> {
        match self {
            CaveMode::Off => None,
            CaveMode::Closed => Some(CaveSettings::closed()),
            CaveMode::Mc => Some(CaveSettings::default()),
        }
    }
}

/// The world an example streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldChoice {
    pub world: World,
    /// `--caves`, if given (only the overworld has caves).
    pub caves: Option<CaveMode>,
    pub trees: bool,
    /// The sea of the forest and the overworld (`--no-sea` turns it off).
    pub sea: bool,
}

impl Default for WorldChoice {
    /// The forest, the world of the earlier `stress` and `voxel_world` measurements.
    fn default() -> Self {
        Self::with_world(World::Forest)
    }
}

/// Default chunk memory floor of the renderer (`ChunkMemorySettings::initial_quads`).
const ARENA_QUADS: u32 = 8 << 20;
/// Default chunk table size of the renderer (`ChunkMemorySettings::initial_slots`).
const CHUNK_SLOTS: u32 = 32_768;
/// The overworld's: caves mesh far more chunks, with more quads each.
const OVERWORLD_ARENA_QUADS: u32 = 24 << 20;
const OVERWORLD_CHUNK_SLOTS: u32 = 65_536;

impl WorldChoice {
    /// `world` with trees and its default caves.
    pub fn with_world(world: World) -> Self {
        Self {
            world,
            caves: None,
            trees: true,
            sea: true,
        }
    }

    /// Handles `flag` if it is a world option (`value` reads its value): `Ok(true)` if it was.
    pub fn parse_flag(
        &mut self,
        flag: &str,
        value: impl FnOnce() -> Result<String, String>,
    ) -> Result<bool, String> {
        match flag {
            "--world" => {
                let v = value()?;
                self.world = World::parse(&v).ok_or_else(|| {
                    format!("{flag}: {v:?} is not flat, terrain, forest or overworld")
                })?;
            }
            "--caves" => {
                let v = value()?;
                self.caves = Some(
                    CaveMode::parse(&v)
                        .ok_or_else(|| format!("{flag}: {v:?} is not mc, closed or off"))?,
                );
            }
            "--no-trees" => self.trees = false,
            "--no-sea" => self.sea = false,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Checks the options together: `--caves` needs the overworld, and `--fixed-height` a world
    /// with a floor (it would stream every layer of the generator, 4096 down for the overworld).
    pub fn validate(self, fixed_height: bool) -> Result<(), String> {
        if self.caves.is_some() && self.world != World::Overworld {
            return Err(format!(
                "--caves needs --world overworld (the {} world has no caves)",
                self.world.name()
            ));
        }
        if fixed_height && self.unbounded() {
            return Err(format!(
                "--fixed-height needs a world with a floor: the {} world goes down forever",
                self.world.name()
            ));
        }
        Ok(())
    }

    /// The caves of the overworld (mc unless `--caves` says otherwise); `None` elsewhere.
    pub fn cave_mode(self) -> Option<CaveMode> {
        (self.world == World::Overworld).then(|| self.caves.unwrap_or(CaveMode::Mc))
    }

    /// True if the world has a sea to turn off: the forest with trees and the overworld (the
    /// terrain, which the forest without trees becomes, is always dry).
    pub fn has_sea_option(self) -> bool {
        self.has_trees() || self.world == World::Overworld
    }

    /// True if the generator places trees: the forest and the overworld, unless `--no-trees`
    /// (flat and terrain never have any).
    pub fn has_trees(self) -> bool {
        self.trees && matches!(self.world, World::Forest | World::Overworld)
    }

    /// True if the generator has no floor (`vertical_chunks` starts at `i64::MIN`).
    pub fn unbounded(self) -> bool {
        self.world == World::Overworld
    }

    /// Name for reports: `forest`, `terrain`, `overworld/mc`, with `-no-trees` where trees were
    /// turned off.
    pub fn label(self) -> String {
        let mut label = match (self.world, self.trees) {
            // The forest without trees is the plain terrain, as before `--world` existed.
            (World::Forest, false) => World::Terrain.name().to_string(),
            (world, _) => world.name().to_string(),
        };
        if let Some(caves) = self.cave_mode() {
            label = format!("{label}/{}", caves.name());
        }
        if !self.trees && self.world == World::Overworld {
            label.push_str("-no-trees");
        }
        if !self.sea && self.has_sea_option() {
            label.push_str("-dry");
        }
        label
    }

    /// The generator.
    pub fn generator(self, seed: u64) -> Arc<dyn WorldGenerator> {
        match (self.world, self.trees) {
            (World::Flat, _) => Arc::new(FlatGenerator::new(vec![
                ("boxel:bedrock".to_string(), 1),
                ("boxel:stone".to_string(), 59),
                ("boxel:dirt".to_string(), 3),
                ("boxel:grass".to_string(), 1),
            ])),
            (World::Terrain, _) | (World::Forest, false) => {
                Arc::new(TerrainGenerator::new(seed, TerrainSettings::default()))
            }
            (World::Forest, true) => Arc::new(ForestGenerator {
                sea: self.sea.then(Sea::default),
                ..ForestGenerator::new(seed, TerrainSettings::default())
            }),
            (World::Overworld, _) => Arc::new(self.overworld(seed).expect("the overworld")),
        }
    }

    /// The overworld generator of this choice (`None` for another world): its surface heights
    /// (`OverworldGenerator::height_at`) for `--cave`.
    pub fn overworld(self, seed: u64) -> Option<OverworldGenerator> {
        if self.world != World::Overworld {
            return None;
        }
        let mut settings = OverworldSettings {
            caves: self
                .cave_mode()
                .and_then(CaveMode::settings)
                .map(|settings| Caves { settings }),
            ..OverworldSettings::default()
        };
        if !self.trees {
            settings.trees = None;
            settings.plants = None;
        }
        if !self.sea {
            settings.sea = None;
        }
        Some(OverworldGenerator::new(seed, settings))
    }

    /// Default `(quads, chunk slots)` the renderer starts with for this world: the engine
    /// defaults (8 Mi quads, 32 768 slots), and 24 Mi quads (192 MiB) and 65 536 slots for the
    /// overworld.
    pub fn render_capacity(self) -> (u32, u32) {
        if self.world == World::Overworld {
            (OVERWORLD_ARENA_QUADS, OVERWORLD_CHUNK_SLOTS)
        } else {
            (ARENA_QUADS, CHUNK_SLOTS)
        }
    }

    /// The renderer's chunk memory for this world: [`WorldChoice::render_capacity`] as the
    /// floor and the initial table, the rest the engine's defaults.
    #[cfg(feature = "render")]
    pub fn chunk_memory(self) -> boxel::render::ChunkMemorySettings {
        chunk_memory(self.render_capacity())
    }
}

/// Chunk memory starting at `(quads, chunk slots)`, the rest the engine's defaults.
#[cfg(feature = "render")]
pub fn chunk_memory((quads, slots): (u32, u32)) -> boxel::render::ChunkMemorySettings {
    boxel::render::ChunkMemorySettings {
        initial_quads: quads,
        initial_slots: slots,
        ..boxel::render::ChunkMemorySettings::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<WorldChoice, String> {
        let mut choice = WorldChoice::default();
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let value = || {
                it.next()
                    .map(ToString::to_string)
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            if !choice.parse_flag(flag, value)? {
                return Err(format!("unknown {flag}"));
            }
        }
        choice.validate(false)?;
        Ok(choice)
    }

    #[test]
    fn worlds_and_caves() {
        let default = parse(&[]).expect("valid");
        assert_eq!(default, WorldChoice::default());
        assert_eq!(default.label(), "forest");
        assert_eq!(default.generator(1).name(), "forest");
        assert_eq!(default.cave_mode(), None);
        assert!(default.sea);
        let dry = parse(&["--no-sea"]).expect("valid");
        assert_eq!((dry.sea, dry.label().as_str()), (false, "forest-dry"));
        let terrain = parse(&["--world", "terrain", "--no-sea"]).expect("valid");
        assert_eq!(terrain.label(), "terrain", "the terrain is always dry");
        let o = parse(&["--world", "overworld"]).expect("valid");
        assert_eq!(
            (o.cave_mode(), o.label().as_str()),
            (Some(CaveMode::Mc), "overworld/mc")
        );
        assert_eq!(o.render_capacity(), (24 << 20, 65_536));
        assert_eq!(*o.generator(1).vertical_chunks().start(), i64::MIN);
        let closed =
            parse(&["--world", "overworld", "--caves", "closed", "--no-trees"]).expect("valid");
        assert_eq!(closed.label(), "overworld/closed-no-trees");
        assert_eq!(parse(&["--no-trees"]).expect("valid").label(), "terrain");
        // Trees only where the generator has them: one record per generator.
        for (args, trees) in [
            (&[][..], true),
            (&["--no-trees"], false),
            (&["--world", "terrain"], false),
            (&["--world", "flat"], false),
            (&["--world", "overworld"], true),
            (&["--world", "overworld", "--no-trees"], false),
        ] {
            assert_eq!(parse(args).expect("valid").has_trees(), trees, "{args:?}");
        }
        assert_eq!(
            parse(&["--no-trees"]).expect("valid").generator(1).name(),
            "terrain"
        );
        for world in World::ALL {
            let c = WorldChoice::with_world(world);
            assert_eq!(World::parse(world.name()), Some(world));
            assert!(c.generator(5).vertical_chunks().start() > &i64::MIN || c.unbounded());
        }
        assert_eq!(
            WorldChoice::with_world(World::Flat).render_capacity(),
            (8 << 20, 32_768)
        );
    }

    #[test]
    fn the_view_distance_ends_where_the_meshed_disc_is_complete() {
        for radius in 2..=64 {
            let viewer = ChunkViewer::new(radius);
            assert_eq!(view_distance(viewer), viewer.mesh_reach(), "{radius}");
            assert!(view_distance(viewer) <= radius as f32 * 32.0, "{radius}");
        }
        assert!((view_distance(ChunkViewer::new(12)) - 340.16).abs() < 0.01);
        assert_eq!(view_distance(ChunkViewer::new(1)), 32.0);
        assert_eq!(view_distance(ChunkViewer::new(0)), 32.0);
    }

    #[test]
    fn bad_world_options_are_rejected() {
        for args in [
            &["--world", "caves"][..],
            &["--caves", "mc"],
            &["--world", "forest", "--caves", "off"],
            &["--world", "overworld", "--caves", "open"],
            &["--world"],
        ] {
            assert!(parse(args).is_err(), "{args:?} was accepted");
        }
        let overworld = WorldChoice::with_world(World::Overworld);
        assert!(
            overworld
                .validate(true)
                .unwrap_err()
                .contains("--fixed-height")
        );
        assert!(WorldChoice::default().validate(true).is_ok());
    }
}
