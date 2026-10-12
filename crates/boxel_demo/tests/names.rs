//! The block names of the demo generators as settings (`BlockNames`): with other names and a
//! registry that holds those blocks at other state ids, every chunk and every far LOD cell is the
//! default world mapped name by name, near the origin and far from it; a registry without a
//! block the settings use is a `MissingBlocks` error that names it. (With the default names the
//! goldens and fingerprints of the other tests prove nothing moved.)
use boxel_demo::worldgen::{
    BlockNames, DEFAULT_SEED, ForestGenerator, MissingBlocks, OverworldGenerator,
    OverworldSettings, TerrainGenerator, TerrainSettings,
};
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockRegistryBuilder, BlockState,
    CHUNK_SIZE, CHUNK_VOLUME, ChunkKey, ChunkPos, DimensionId, FLUID_LEVEL, FaceTextures,
    MAX_CHUNK_COORD, OPAQUE_OPACITY, Property, RenderShape, TextureImage, TextureSpec,
};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{LodColumn, LodNode, LodRequest, LodScratch, LodSource};
use std::collections::{BTreeSet, HashMap};

/// Names a game might pick: another namespace and other paths.
const RENAMED: BlockNames = BlockNames {
    stone: "test:rock",
    dirt: "test:soil",
    grass: "test:turf",
    sand: "test:shore",
    snow: "test:powder",
    bedrock: "test:floor",
    water: "test:sea",
    log: "test:trunk",
    leaves: "test:crown",
    short_grass: "test:tuft",
    flower: "test:bloom",
};

/// A registry with the blocks of [`RENAMED`] but those of `skip`, after three others and in reverse
/// field order, so no state id is the demo's; the log has an axis and the water 16 levels, as in
/// the demo, so their default states are not their first ids either.
fn renamed_registry(skip: &[&str]) -> BlockRegistry {
    let mut b = BlockRegistryBuilder::new();
    let layer = b
        .add_texture(TextureSpec {
            name: "test:layer".to_string(),
            image: TextureImage::solid(16, [128, 128, 128, 255]),
        })
        .expect("test:layer");
    let cube = |name: &str| BlockDef {
        name: name.to_string(),
        shape: RenderShape::Opaque,
        model: BlockModel::CUBE,
        collision: BlockCollision::Full,
        emission: [0; 3],
        light_opacity: OPAQUE_OPACITY,
        textures: FaceTextures::all(layer),
    };
    for name in ["test:first", "test:second", "test:third"] {
        b.add_block(cube(name)).expect(name);
    }
    for (_, name) in RENAMED.entries().into_iter().rev() {
        if skip.contains(&name) {
            continue;
        }
        let properties = if name == RENAMED.log {
            vec![Property::new("axis", ["x", "y", "z"]).with_default("y")]
        } else if name == RENAMED.water {
            vec![Property::int(FLUID_LEVEL, 0..=15).with_default("0")]
        } else {
            Vec::new()
        };
        let def = if name == RENAMED.water {
            BlockDef {
                shape: RenderShape::Translucent,
                model: BlockModel::Fluid,
                collision: BlockCollision::None,
                light_opacity: 1,
                ..cube(name)
            }
        } else {
            cube(name)
        };
        b.add_block_with_states(def, properties, |_, _| {})
            .expect(name);
    }
    b.build()
}

/// The demo's registry, the renamed one, and the state of each default name in the first to the
/// state of the same field of [`RENAMED`] in the second (air to air): the map every renamed
/// world must follow.
struct Worlds {
    demo: BlockRegistry,
    renamed: BlockRegistry,
    map: HashMap<BlockState, BlockState>,
}

impl Worlds {
    fn new() -> Self {
        let demo = boxel_demo::blocks::registry();
        let renamed = renamed_registry(&[]);
        let mut map = HashMap::from([(BlockState::AIR, BlockState::AIR)]);
        for ((field, from), (_, to)) in BlockNames::default()
            .entries()
            .into_iter()
            .zip(RENAMED.entries())
        {
            let (from, to) = (
                demo.by_name(from).expect(from),
                renamed.by_name(to).expect(to),
            );
            assert_ne!(
                from, to,
                "the {field} keeps its state id: the map proves nothing"
            );
            assert!(map.insert(from, to).is_none(), "two fields name one block");
        }
        Self { demo, renamed, map }
    }

    /// `state` of the demo world in the renamed one; panics on a state no generator places.
    fn mapped(&self, state: BlockState, at: &dyn std::fmt::Debug) -> BlockState {
        *self.map.get(&state).unwrap_or_else(|| {
            panic!(
                "{at:?}: {:?} is not a generator block",
                self.demo.state_name(state).to_string()
            )
        })
    }
}

/// Every voxel of chunk `pos`.
fn dense(g: &dyn WorldGenerator, r: &BlockRegistry, pos: ChunkPos) -> Vec<BlockState> {
    let mut out = vec![BlockState::AIR; CHUNK_VOLUME];
    g.generate(ChunkKey::new(pos), r).to_dense(&mut out);
    out
}

/// Checks that `renamed` (on the renamed registry) generates every chunk of `positions` as
/// `default` (on the demo's) mapped by name, voxel by voxel and uniform where it is uniform;
/// returns the names of the blocks the default world placed there.
fn same_chunks(
    worlds: &Worlds,
    default: &dyn WorldGenerator,
    renamed: &dyn WorldGenerator,
    positions: impl IntoIterator<Item = ChunkPos>,
) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    for pos in positions {
        let a = default.generate(ChunkKey::new(pos), &worlds.demo);
        let b = renamed.generate(ChunkKey::new(pos), &worlds.renamed);
        assert_eq!(
            b.as_uniform(),
            a.as_uniform().map(|s| worlds.mapped(s, &pos)),
            "{pos:?} uniform"
        );
        let (a, b) = (
            dense(default, &worlds.demo, pos),
            dense(renamed, &worlds.renamed, pos),
        );
        let mut states = BTreeSet::new();
        for (i, (&x, &y)) in a.iter().zip(&b).enumerate() {
            if states.insert(x) {
                seen.insert(worlds.demo.name(x).to_string());
            }
            assert_eq!(y, worlds.mapped(x, &pos), "{pos:?} voxel {i}");
        }
    }
    seen
}

/// The chunks around the surface of chunk column `(cx, cz)` (its middle column's height, from
/// `height`): the caves under it, the ground, and the trees and the sea over it.
fn around_surface(height: impl Fn(i64, i64) -> i64, cx: i64, cz: i64) -> Vec<ChunkPos> {
    let mid = |c: i64| c * i64::from(CHUNK_SIZE) + 16;
    let cy = height(mid(cx), mid(cz)) >> 5;
    (cy - 3..=cy + 1)
        .map(|y| ChunkPos::new(cx, y, cz))
        .collect()
}

/// Chunk columns near the origin, past the former `i32` edge, at `2^40` and at both ends of the
/// world.
fn chunk_columns() -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    for cx in -3..=3 {
        for cz in -3..=3 {
            out.push((cx, cz));
        }
    }
    for (cx, cz) in [
        ((1 << 26) - 1, -(1 << 26)),
        ((1 << 35) + 3, -(1 << 31) + 7),
        (MAX_CHUNK_COORD - 1, -MAX_CHUNK_COORD),
        (-MAX_CHUNK_COORD, MAX_CHUNK_COORD - 2),
    ] {
        for (dx, dz) in [(0, 0), (1, 0), (0, 1)] {
            out.push((cx + dx, cz + dz));
        }
    }
    out
}

/// The first chunk column of `-r..=r` squared (by rows) whose middle column's height satisfies
/// `pick`: a lake, a snowy peak.
fn find_column(height: impl Fn(i64, i64) -> i64, pick: impl Fn(i64) -> bool) -> (i64, i64) {
    let r = 64;
    (-r..=r)
        .flat_map(|cx| (-r..=r).map(move |cz| (cx, cz)))
        .find(|&(cx, cz)| pick(height(cx * 32 + 16, cz * 32 + 16)))
        .expect("no such column")
}

/// One column of far nodes: its band and every node it emitted, its cells mapped by `map`.
fn far_column(
    source: &dyn LodSource,
    r: &BlockRegistry,
    req: &LodRequest,
    map: impl Fn(BlockState) -> BlockState,
) -> (LodColumn, Vec<(i64, Vec<BlockState>)>) {
    let mut nodes = Vec::new();
    let mut band = source.column(req, r, &mut LodScratch::default(), &mut |layer, node| {
        let cells = match node {
            LodNode::Uniform(state) => vec![map(state)],
            LodNode::Cells(cells) => cells.0.iter().map(|&s| map(s)).collect(),
        };
        nodes.push((layer, cells));
    });
    band.below = map(band.below);
    nodes.sort_by_key(|(layer, _)| *layer);
    (band, nodes)
}

/// Checks that `renamed` answers every far column of levels 1 to 6 at the places of
/// [`chunk_columns`] (and a lake and hills) as `default` mapped by name, cell by cell.
fn same_far_cells(worlds: &Worlds, default: &dyn LodSource, renamed: &dyn LodSource) {
    let size = i64::from(CHUNK_SIZE);
    let mut places: Vec<[i64; 2]> = vec![[248, -256], [-3392, 1280]];
    places.extend(
        chunk_columns()
            .into_iter()
            .map(|(cx, cz)| [cx * size, cz * size]),
    );
    for level in 1..=6u8 {
        let shift = 5 + u32::from(level);
        for [x, z] in &places {
            let req = LodRequest {
                dim: DimensionId(0),
                level,
                column: [x >> shift, z >> shift],
                layers: -8..=8,
            };
            let expected = far_column(default, &worlds.demo, &req, |s| worlds.mapped(s, &req));
            let got = far_column(renamed, &worlds.renamed, &req, |s| s);
            assert_eq!(got.0, expected.0, "{req:?} band");
            assert_eq!(got.1.len(), expected.1.len(), "{req:?} nodes");
            for ((layer, a), (got_layer, b)) in expected.1.iter().zip(&got.1) {
                assert_eq!(got_layer, layer, "{req:?} layers");
                assert!(a == b, "{req:?} layer {layer}");
            }
        }
    }
}

/// Every block of `names` an overworld places.
const OVERWORLD_FIELDS: [&str; 10] = [
    "stone",
    "dirt",
    "grass",
    "sand",
    "snow",
    "water",
    "log",
    "leaves",
    "short_grass",
    "flower",
];

/// The demo names of `fields`.
fn demo_names(fields: &[&str]) -> BTreeSet<String> {
    let names = BlockNames::default().entries();
    fields
        .iter()
        .map(|f| {
            names
                .iter()
                .find(|(field, _)| field == f)
                .expect(f)
                .1
                .to_string()
        })
        .collect()
}

#[test]
fn the_overworld_with_other_names_is_the_same_world() {
    let worlds = Worlds::new();
    for seed in [DEFAULT_SEED, 0x005e_ed0f_0a75] {
        let default = OverworldGenerator::new(seed, OverworldSettings::default());
        let renamed = OverworldGenerator::new(
            seed,
            OverworldSettings {
                names: RENAMED,
                ..OverworldSettings::default()
            },
        );
        let height = |x, z| default.height_at(x, z);
        let mut columns = chunk_columns();
        // A lake and a snowy peak, so every block shows up.
        columns.push(find_column(height, |h| h < 30));
        columns.push(find_column(height, |h| h > 100));
        let positions = columns
            .into_iter()
            .flat_map(|(cx, cz)| around_surface(height, cx, cz));
        let seen = same_chunks(&worlds, &default, &renamed, positions);
        let mut expected = demo_names(&OVERWORLD_FIELDS);
        expected.insert("boxel:air".to_string());
        assert_eq!(seen, expected, "seed {seed:#x}");
        same_far_cells(&worlds, &default, &renamed);
    }
}

#[test]
fn the_forest_and_the_terrain_with_other_names_are_the_same_worlds() {
    let worlds = Worlds::new();
    let settings = TerrainSettings {
        names: RENAMED,
        ..TerrainSettings::default()
    };
    let terrain = (
        TerrainGenerator::default(),
        TerrainGenerator::new(DEFAULT_SEED, settings.clone()),
    );
    let forest = (
        ForestGenerator::default(),
        ForestGenerator::new(DEFAULT_SEED, settings),
    );
    let height = |x, z| i64::from(terrain.0.height_at(x, z));
    let mut columns = chunk_columns();
    columns.push(find_column(height, |h| h < 30));
    columns.push(find_column(height, |h| h > 100));
    // The bedrock floor at -64 is chunk -2, under every surface chunk of these columns.
    let positions: Vec<_> = columns
        .into_iter()
        .flat_map(|(cx, cz)| {
            let mut out = around_surface(height, cx, cz);
            out.push(ChunkPos::new(cx, -2, cz));
            out
        })
        .collect();
    let seen = same_chunks(&worlds, &terrain.0, &terrain.1, positions.clone());
    let mut expected = demo_names(&["stone", "dirt", "grass", "sand", "snow", "bedrock"]);
    expected.insert("boxel:air".to_string());
    assert_eq!(seen, expected, "terrain");
    let seen = same_chunks(&worlds, &forest.0, &forest.1, positions);
    let mut fields = OVERWORLD_FIELDS.to_vec();
    fields.push("bedrock");
    let mut expected = demo_names(&fields);
    expected.insert("boxel:air".to_string());
    assert_eq!(seen, expected, "forest");
    same_far_cells(&worlds, &terrain.0, &terrain.1);
    same_far_cells(&worlds, &forest.0, &forest.1);
}

#[test]
fn the_default_names_are_the_demo_blocks() {
    let demo = boxel_demo::blocks::registry();
    assert_eq!(OverworldGenerator::default().check(&demo), Ok(()));
    assert_eq!(ForestGenerator::default().check(&demo), Ok(()));
    assert_eq!(TerrainGenerator::default().check(&demo), Ok(()));
    for (field, name) in BlockNames::default().entries() {
        assert!(name.starts_with("boxel:"), "{field}: {name}");
        assert!(demo.block_by_name(name).is_some(), "{field}: {name}");
    }
}

#[test]
fn check_names_every_block_the_registry_lacks() {
    let overworld = |settings: OverworldSettings| {
        OverworldGenerator::new(
            DEFAULT_SEED,
            OverworldSettings {
                names: RENAMED,
                ..settings
            },
        )
    };
    let terrain = TerrainSettings {
        names: RENAMED,
        ..TerrainSettings::default()
    };
    let full = overworld(OverworldSettings::default());
    let forest = ForestGenerator::new(DEFAULT_SEED, terrain.clone());
    let plain = TerrainGenerator::new(DEFAULT_SEED, terrain.clone());
    // Their own registry has every block.
    let renamed = renamed_registry(&[]);
    for g in [
        full.check(&renamed),
        forest.check(&renamed),
        plain.check(&renamed),
    ] {
        assert_eq!(g, Ok(()));
    }
    // The demo's has none: every one is named, in field order.
    let err = full.check(&boxel_demo::blocks::registry()).unwrap_err();
    assert_eq!(err.generator, "OverworldGenerator");
    let fields: Vec<_> = err.missing.iter().map(|(field, _)| *field).collect();
    assert_eq!(fields, OVERWORLD_FIELDS);
    // One block missing, and the message a game prints.
    let without_flower = renamed_registry(&[RENAMED.flower]);
    assert_eq!(
        full.check(&without_flower),
        Err(MissingBlocks {
            generator: "OverworldGenerator",
            missing: vec![("flower", "test:bloom")],
        })
    );
    assert_eq!(
        full.check(&without_flower).unwrap_err().to_string(),
        "OverworldGenerator: the registry has no block named \"test:bloom\" (the flower); \
         register it or change the generator's BlockNames"
    );
    // Only what the settings use: no plants, no flower needed.
    let no_plants = overworld(OverworldSettings {
        plants: None,
        ..OverworldSettings::default()
    });
    assert_eq!(no_plants.check(&without_flower), Ok(()));
    // The water only with a sea, the trees only with trees.
    let without_water = renamed_registry(&[RENAMED.water]);
    let without_log = renamed_registry(&[RENAMED.log]);
    let dry = overworld(OverworldSettings {
        sea: None,
        ..OverworldSettings::default()
    });
    assert_eq!(dry.check(&without_water), Ok(()));
    assert_eq!(
        full.check(&without_water).unwrap_err().missing,
        [("water", "test:sea")]
    );
    let bare = overworld(OverworldSettings {
        trees: None,
        ..OverworldSettings::default()
    });
    assert_eq!(bare.check(&without_log), Ok(()));
    let dry_forest = ForestGenerator {
        sea: None,
        ..forest.clone()
    };
    assert_eq!(dry_forest.check(&without_water), Ok(()));
    assert_eq!(
        forest.check(&without_log).unwrap_err().missing,
        [("log", "test:trunk")]
    );
    // The bedrock: the terrain and the forest stand on it, the overworld has none.
    let without_bedrock = renamed_registry(&[RENAMED.bedrock]);
    assert_eq!(full.check(&without_bedrock), Ok(()));
    let bedrock = Err(MissingBlocks {
        generator: "TerrainGenerator",
        missing: vec![("bedrock", "test:floor")],
    });
    assert_eq!(plain.check(&without_bedrock), bedrock);
    let err = forest.check(&without_bedrock).unwrap_err();
    assert_eq!(
        (err.generator, err.missing),
        ("ForestGenerator", vec![("bedrock", "test:floor")])
    );
    // Several at once, all named, in field order.
    let err = full
        .check(&renamed_registry(&[RENAMED.water, RENAMED.stone]))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "OverworldGenerator: the registry has no block named \"test:rock\" (the stone), \
         \"test:sea\" (the water); register them or change the generator's BlockNames"
    );
}
