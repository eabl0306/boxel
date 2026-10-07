//! The world options shared by the examples: `--world`, `--caves` and `--no-trees`, the
//! generator they build, and the render capacity each world needs.
#![allow(
    dead_code,
    reason = "each example uses its own part (headless_world has no renderer)"
)]

use boxel::prelude::*;
use boxel::worldgen::overworld::CaveSettings;
use boxel::worldgen::{Caves, FlatGenerator, OverworldSettings, TerrainSettings};
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
  --no-trees          no trees or plants (forest becomes terrain)",
        default.name()
    )
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
}

impl Default for WorldChoice {
    /// The forest, the world of the earlier `stress` and `voxel_world` measurements.
    fn default() -> Self {
        Self::with_world(World::Forest)
    }
}

/// Default quad arena of the renderer (`RenderSettings::arena_quads`).
const ARENA_QUADS: u32 = 8 << 20;
/// Default chunk table size of the renderer (`RenderSettings::max_chunk_slots`).
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
            (World::Forest, true) => {
                Arc::new(ForestGenerator::new(seed, TerrainSettings::default()))
            }
            (World::Overworld, trees) => {
                let mut settings = OverworldSettings {
                    caves: self
                        .cave_mode()
                        .and_then(CaveMode::settings)
                        .map(|settings| Caves { settings }),
                    ..OverworldSettings::default()
                };
                if !trees {
                    settings.trees = None;
                    settings.plants = None;
                }
                Arc::new(OverworldGenerator::new(seed, settings))
            }
        }
    }

    /// Default `(arena quads, chunk slots)` of the renderer for this world: the engine defaults
    /// (8 Mi quads, 32 768 slots), and 24 Mi quads (192 MiB) and 65 536 slots for the overworld.
    pub fn render_capacity(self) -> (u32, u32) {
        if self.world == World::Overworld {
            (OVERWORLD_ARENA_QUADS, OVERWORLD_CHUNK_SLOTS)
        } else {
            (ARENA_QUADS, CHUNK_SLOTS)
        }
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
