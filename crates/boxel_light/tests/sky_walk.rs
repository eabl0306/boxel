//! The sky walk against a brute-force scan of the generated chunks, voxel by voxel, for the
//! overworld (with trees and a cave entrance), forest, flat and terrain generators, near the
//! origin and at x = z = 2^40; the limit cap; `i64::MIN` for open columns; the column rescan
//! after an edit; an edit far above the generator changes only its own column (the floor is
//! the world's); tiles without a floor; and generators without a top.
use boxel_light::sky_tile::{DEFAULT_WALK_LIMIT, GeneratorTop, walk, walk_floor};
use boxel_light::{LayerData, Rescan, SkyTile};
use boxel_voxel::{
    BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, LightTables, LocalPos,
    MAX_BLOCK_COORD, MAX_CHUNK_COORD,
};
use boxel_worldgen::{
    FlatGenerator, ForestGenerator, OverworldGenerator, TerrainGenerator, WorldGenerator,
};
use std::collections::HashMap;

/// Generated chunks of one chunk column, by layer.
struct Column<'g> {
    generator: &'g dyn WorldGenerator,
    registry: &'g BlockRegistry,
    cx: i64,
    cz: i64,
    cache: HashMap<i64, ChunkData>,
}

impl<'g> Column<'g> {
    fn new(
        generator: &'g dyn WorldGenerator,
        registry: &'g BlockRegistry,
        cx: i64,
        cz: i64,
    ) -> Self {
        Self {
            generator,
            registry,
            cx,
            cz,
            cache: HashMap::new(),
        }
    }

    /// The block at local column `(x, z)`, height `y`; `None` below the generator's range
    /// (air down to the bottom of the world).
    fn block(&mut self, x: u32, y: i64, z: u32) -> Option<BlockState> {
        let cy = y >> 5;
        let range = self.generator.vertical_chunks();
        if cy < *range.start() {
            return None;
        }
        if cy > *range.end() {
            return Some(BlockState::AIR);
        }
        let (g, r, cx, cz) = (self.generator, self.registry, self.cx, self.cz);
        let chunk = self
            .cache
            .entry(cy)
            .or_insert_with(|| g.generate(ChunkKey::new(ChunkPos::new(cx, cy, cz)), r));
        Some(chunk.get(LocalPos::new(x, (y & 31) as u32, z)))
    }

    /// Brute force: each column scanned voxel by voxel from `top` down to `top - limit`.
    fn brute_force(&mut self, tables: &LightTables, top: i64, limit: u32) -> Vec<i64> {
        let floor = top - i64::from(limit);
        let mut out = Vec::with_capacity(1024);
        for i in 0..1024u32 {
            let (x, z) = (i >> 5, i & 31);
            let mut t = floor;
            for y in (floor..=top).rev() {
                match self.block(x, y, z) {
                    None => {
                        t = i64::MIN;
                        break;
                    }
                    Some(s) if tables.blocks_sky(s) => {
                        t = y;
                        break;
                    }
                    Some(_) => {}
                }
            }
            out.push(t);
        }
        out
    }
}

/// The walk of chunk column `(cx, cz)`, every layer generated privately.
fn walk_generated(
    generator: &dyn WorldGenerator,
    registry: &BlockRegistry,
    tables: &LightTables,
    (cx, cz): (i64, i64),
    limit: u32,
) -> SkyTile {
    let range = generator.vertical_chunks();
    let top = GeneratorTop::of(&range).voxel().expect("bounded generator");
    walk(top, walk_floor(top, limit), tables, |cy| {
        if cy < *range.start() {
            LayerData::Void
        } else if cy > *range.end() {
            LayerData::Air
        } else {
            LayerData::Owned(generator.generate(ChunkKey::new(ChunkPos::new(cx, cy, cz)), registry))
        }
    })
}

/// Walks and brute-forces the tile; returns it.
fn check(generator: &dyn WorldGenerator, column: (i64, i64), limit: u32) -> SkyTile {
    let registry = BlockRegistry::default_blocks();
    let tables = LightTables::from_registry(&registry);
    let tile = walk_generated(generator, &registry, &tables, column, limit);
    let top = GeneratorTop::of(&generator.vertical_chunks())
        .voxel()
        .unwrap();
    let want =
        Column::new(generator, &registry, column.0, column.1).brute_force(&tables, top, limit);
    for (i, (&got, &want)) in tile.tops().iter().zip(&want).enumerate() {
        assert_eq!(
            got,
            want,
            "{} column {column:?} local ({}, {}), limit {limit}",
            generator.name(),
            i >> 5,
            i & 31
        );
    }
    assert_eq!(tile.min(), *want.iter().min().unwrap());
    assert_eq!(tile.max(), *want.iter().max().unwrap());
    assert_eq!(tile.floor(), top - i64::from(limit));
    tile
}

const FAR: i64 = 1 << 35;

#[test]
fn walk_equals_brute_force_for_every_generator() {
    let overworld = OverworldGenerator::default();
    let forest = ForestGenerator::default();
    let terrain = TerrainGenerator::default();
    let flat = FlatGenerator::new(
        [
            ("boxel:bedrock", 1),
            ("boxel:stone", 3),
            ("boxel:dirt", 2),
            ("boxel:grass", 1),
        ]
        .map(|(n, t)| (n.to_string(), t))
        .to_vec(),
    );
    let generators: [&dyn WorldGenerator; 4] = [&overworld, &forest, &terrain, &flat];
    for g in generators {
        for column in [(0, 0), (-3, 2), (FAR, FAR), (-FAR, FAR + 1)] {
            check(g, column, DEFAULT_WALK_LIMIT);
        }
    }
}

#[test]
fn trees_and_entrances_set_the_tops() {
    let g = OverworldGenerator::default();
    let caves = g.settings.caves.clone().expect("default caves");
    let threshold = caves
        .settings
        .entrances
        .as_ref()
        .map(|e| e.mask_threshold + e.mask_ramp)
        .expect("default entrances");
    // Trees: some top above its column's surface. Entrances: some surface voxel carved, so the
    // top is below the surface. Search tiles whose center column is deep in an entrance region.
    let (mut trees, mut entrance) = (false, false);
    'search: for r in 0i64..64 {
        for (cx, cz) in [(r, 0), (0, r), (-r, r), (r, -r), (r, r), (-r, -r)] {
            let mask = caves.entrance_mask(g.seed, cx * 32 + 16, cz * 32 + 16);
            if trees && mask < threshold {
                continue;
            }
            let tile = check(&g, (cx, cz), DEFAULT_WALK_LIMIT);
            for i in 0..1024 {
                let (x, z) = (cx * 32 + (i >> 5) as i64, cz * 32 + (i & 31) as i64);
                let h = g.height_at(x, z);
                trees |= tile.tops()[i] > h;
                entrance |= tile.tops()[i] < h;
            }
            if trees && entrance {
                break 'search;
            }
        }
    }
    assert!(trees, "no tree found");
    assert!(entrance, "no cave entrance found");
}

#[test]
fn the_limit_caps_open_columns() {
    let overworld = OverworldGenerator::default();
    // Surface columns lie 15 to 111 voxels below the top voxel: most are capped.
    let tile = check(&overworld, (0, 0), 40);
    assert_eq!(tile.min(), tile.floor());
    // A world of air: the walk stops at the limit before reaching below the range (1 layer).
    let air = FlatGenerator::new(vec![("boxel:air".to_string(), 4)]);
    let capped = check(&air, (5, 5), 10);
    assert_eq!((capped.min(), capped.max()), (21, 21));
}

#[test]
fn open_columns_reach_the_void() {
    // Air layers only: below the range everything is air, so every column is open forever.
    let air = FlatGenerator::new(vec![("boxel:air".to_string(), 40)]);
    let tile = check(&air, (1, -1), DEFAULT_WALK_LIMIT);
    assert_eq!((tile.min(), tile.max()), (i64::MIN, i64::MIN));
    // A flat world without layers has nothing to walk.
    let empty = FlatGenerator::new(Vec::new());
    assert_eq!(
        GeneratorTop::of(&empty.vertical_chunks()),
        GeneratorTop::Empty
    );
}

#[test]
fn rescan_after_removing_a_top() {
    let registry = BlockRegistry::default_blocks();
    let tables = LightTables::from_registry(&registry);
    let stone = registry.by_name("boxel:stone").unwrap();
    let leaves = registry.by_name("boxel:leaves").unwrap();
    // Two chunk layers: layer 1 has a leaf at y = 40 over column (3, 4); layer 0 has stone at
    // y = 10 in the same column.
    let mut upper = ChunkData::uniform(BlockState::AIR);
    upper.set(LocalPos::new(3, 8, 4), leaves);
    let mut lower = ChunkData::uniform(BlockState::AIR);
    lower.set(LocalPos::new(3, 10, 4), stone);
    let layers = |cy: i64| match cy {
        1 => LayerData::Ref(&upper),
        0 => LayerData::Ref(&lower),
        _ => LayerData::Void,
    };
    let mut tile = walk(63, walk_floor(63, DEFAULT_WALK_LIMIT), &tables, layers);
    assert_eq!(tile.top(3, 4), 40);
    assert_eq!(tile.top(0, 0), i64::MIN);
    assert_eq!((tile.min(), tile.max()), (i64::MIN, 40));
    // The leaf is removed: the rescan finds the stone.
    upper.set(LocalPos::new(3, 8, 4), BlockState::AIR);
    let rescan = tile.rescan_column(3, 4, 40, &tables, |cy| match cy {
        1 => Some(LayerData::Ref(&upper)),
        0 => Some(LayerData::Ref(&lower)),
        _ => Some(LayerData::Void),
    });
    assert_eq!(rescan, Rescan::Found(10));
    assert_eq!((tile.top(3, 4), tile.max(), tile.version), (10, 10, 1));
    assert!(tile.edited);
    // The stone is removed too, and the layer below is not loaded: unresolved, unchanged.
    lower.set(LocalPos::new(3, 10, 4), BlockState::AIR);
    let before = tile.clone();
    let rescan = tile.rescan_column(3, 4, 10, &tables, |cy| match cy {
        0 => Some(LayerData::Ref(&lower)),
        _ => None,
    });
    assert_eq!(rescan, Rescan::Unresolved);
    assert_eq!(tile, before);
    // With the void below: open forever.
    let rescan = tile.rescan_column(3, 4, 10, &tables, |cy| match cy {
        0 => Some(LayerData::Air),
        _ => Some(LayerData::Void),
    });
    assert_eq!(rescan, Rescan::Found(i64::MIN));
    assert_eq!(tile.max(), i64::MIN);
    // A blocker placed above raises the top.
    assert!(tile.raise(3, 4, 5000));
    assert!(!tile.raise(3, 4, 4999));
    assert_eq!(
        (tile.top(3, 4), tile.max(), tile.min()),
        (5000, 5000, i64::MIN)
    );
}

#[test]
fn an_edit_far_above_changes_only_its_column() {
    let g = OverworldGenerator::default();
    let registry = BlockRegistry::default_blocks();
    let tables = LightTables::from_registry(&registry);
    let stone = registry.by_name("boxel:stone").unwrap();
    let range = g.vertical_chunks();
    let top = GeneratorTop::of(&range).voxel().unwrap();
    let floor = walk_floor(top, DEFAULT_WALK_LIMIT);
    let (cx, cz) = (2, -1);
    // A platform block 5000 voxels above the generator's top, over local column (3, 4).
    let y = top + 5000;
    let mut platform = ChunkData::uniform(BlockState::AIR);
    platform.set(LocalPos::new(3, (y & 31) as u32, 4), stone);
    let layer = |cy: i64, edited: bool| {
        if edited && cy == y >> 5 {
            LayerData::Ref(&platform)
        } else if cy > *range.end() {
            LayerData::Air
        } else {
            LayerData::Owned(g.generate(ChunkKey::new(ChunkPos::new(cx, cy, cz)), &registry))
        }
    };
    let plain = walk(top, floor, &tables, |cy| layer(cy, false));
    // A fresh walk of the edited content starts at the platform, with the same floor.
    let fresh = walk(y, floor, &tables, |cy| layer(cy, true));
    assert_eq!(fresh.floor(), plain.floor());
    assert_eq!(fresh.top(3, 4), y);
    let edited = (3 << 5) | 4;
    for (i, (&got, &want)) in fresh.tops().iter().zip(plain.tops()).enumerate() {
        if i != edited {
            assert_eq!(got, want, "local ({}, {})", i >> 5, i & 31);
        }
    }
    // The edit applied to the plain tile gives the same tile.
    let mut incremental = plain.clone();
    assert!(incremental.raise(3, 4, y));
    assert_eq!(incremental.tops(), fresh.tops());
    assert_eq!(
        (incremental.min(), incremental.max()),
        (fresh.min(), fresh.max())
    );
    // Removing it: the rescan finds what the plain walk found.
    let mut removed = fresh.clone();
    let rescan = removed.rescan_column(3, 4, y - 1, &tables, |cy| Some(layer(cy, false)));
    assert_eq!(rescan, Rescan::Found(plain.top(3, 4)));
    assert_eq!(removed.tops(), plain.tops());
    assert_eq!((removed.min(), removed.max()), (plain.min(), plain.max()));
}

/// `LayerData::AirUntil` skips a gap of air in one step: a platform 2^50 voxels above the
/// generator walks and rescans like a layer-by-layer walk would, reading only the platform's
/// layer and the generator's layers.
#[test]
fn air_until_skips_long_gaps() {
    let g = ForestGenerator::default();
    let registry = BlockRegistry::default_blocks();
    let tables = LightTables::from_registry(&registry);
    let stone = registry.by_name("boxel:stone").unwrap();
    let range = g.vertical_chunks();
    let top = GeneratorTop::of(&range).voxel().unwrap();
    let floor = walk_floor(top, DEFAULT_WALK_LIMIT);
    let y = top + (1 << 50);
    let mut platform = ChunkData::uniform(BlockState::AIR);
    platform.set(LocalPos::new(7, (y & 31) as u32, 9), stone);
    let mut reads = 0;
    let mut layer = |cy: i64| {
        reads += 1;
        if cy == y >> 5 {
            LayerData::Ref(&platform)
        } else if cy > *range.end() {
            LayerData::AirUntil(*range.end())
        } else if cy < *range.start() {
            LayerData::Void
        } else {
            LayerData::Owned(g.generate(ChunkKey::new(ChunkPos::new(0, cy, 0)), &registry))
        }
    };
    let skipped = walk(y, floor, &tables, &mut layer);
    let plain = walk(top, floor, &tables, |cy| {
        if cy > *range.end() {
            LayerData::Air
        } else {
            LayerData::Owned(g.generate(ChunkKey::new(ChunkPos::new(0, cy, 0)), &registry))
        }
    });
    assert_eq!(skipped.top(7, 9), y);
    let edited = (7 << 5) | 9;
    for (i, (&got, &want)) in skipped.tops().iter().zip(plain.tops()).enumerate() {
        if i != edited {
            assert_eq!(got, want, "local ({}, {})", i >> 5, i & 31);
        }
    }
    assert!(reads < 40, "{reads} layers read");
    // The rescan after removing the platform skips the same gap.
    let mut removed = skipped.clone();
    let mut rescans = 0;
    let rescan = removed.rescan_column(7, 9, y - 1, &tables, |cy| {
        rescans += 1;
        Some(if cy > *range.end() {
            LayerData::AirUntil(*range.end())
        } else {
            LayerData::Owned(g.generate(ChunkKey::new(ChunkPos::new(0, cy, 0)), &registry))
        })
    });
    assert_eq!(rescan, Rescan::Found(plain.top(7, 9)));
    assert!(rescans < 40, "{rescans} layers read");
    // A gap past the world limit ends at the void.
    let low = walk(0, i64::MIN, &tables, |cy| {
        assert!(cy >= -MAX_CHUNK_COORD - 1, "layer {cy} below the world");
        LayerData::AirUntil(i64::MIN)
    });
    assert_eq!((low.min(), low.max()), (i64::MIN, i64::MIN));
}

#[test]
fn tiles_without_a_floor_rescan_and_walk_to_the_world_bottom() {
    let registry = BlockRegistry::default_blocks();
    let tables = LightTables::from_registry(&registry);
    let stone = registry.by_name("boxel:stone").unwrap();
    // A hand-made tile without a floor, in a positive layer: the top of column (1, 2) at
    // y = 100 (layer 3) is removed, a stone remains at y = 97.
    let mut tile = SkyTile::uniform(100, i64::MIN);
    let mut chunk = ChunkData::uniform(BlockState::AIR);
    chunk.set(LocalPos::new(1, 97 - 96, 2), stone);
    let rescan = tile.rescan_column(1, 2, 99, &tables, |cy| {
        Some(if cy == 3 {
            LayerData::Ref(&chunk)
        } else {
            LayerData::Void
        })
    });
    assert_eq!(rescan, Rescan::Found(97));
    // Near the bottom of the world, air layers end at the world limit: open forever.
    let bottom = -MAX_BLOCK_COORD;
    let mut deep = SkyTile::uniform(bottom + 40, i64::MIN);
    let mut layers = 0;
    let rescan = deep.rescan_column(0, 0, bottom + 39, &tables, |_| {
        layers += 1;
        Some(LayerData::Air)
    });
    assert_eq!(rescan, Rescan::Found(i64::MIN));
    assert_eq!(layers, 2);
    let walked = walk(bottom + 40, i64::MIN, &tables, |cy| {
        assert!(cy >= -MAX_CHUNK_COORD);
        LayerData::Air
    });
    assert_eq!((walked.min(), walked.max()), (i64::MIN, i64::MIN));
    // A start below the floor reads nothing: every column at the floor.
    let low = walk(10, 50, &tables, |_| {
        unreachable!("no layer above the floor")
    });
    assert_eq!((low.min(), low.max(), low.floor()), (50, 50, 50));
}

/// A generator that keeps the trait's default range (`i64::MIN..=i64::MAX`).
struct Unbounded;

impl WorldGenerator for Unbounded {
    fn generate(&self, _: ChunkKey, _: &BlockRegistry) -> ChunkData {
        ChunkData::uniform(BlockState::AIR)
    }
    fn name(&self) -> &str {
        "unbounded"
    }
}

#[test]
fn generator_tops() {
    assert_eq!(
        GeneratorTop::of(&Unbounded.vertical_chunks()),
        GeneratorTop::Unbounded
    );
    assert_eq!(GeneratorTop::Unbounded.voxel(), None);
    assert_eq!(
        GeneratorTop::of(&(0..=MAX_CHUNK_COORD + 1)),
        GeneratorTop::Unbounded
    );
    assert_eq!(
        GeneratorTop::of(&(0..=MAX_CHUNK_COORD)),
        GeneratorTop::Voxel(MAX_BLOCK_COORD + 31)
    );
    assert_eq!(
        GeneratorTop::of(&(i64::MIN..=-MAX_CHUNK_COORD - 1)),
        GeneratorTop::Empty
    );
    assert_eq!(GeneratorTop::of(&(i64::MIN..=2)).voxel(), Some(95));
    let overworld = OverworldGenerator::default();
    assert!(
        GeneratorTop::of(&overworld.vertical_chunks())
            .voxel()
            .is_some()
    );
}
