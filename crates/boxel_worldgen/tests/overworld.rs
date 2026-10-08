//! Behavior tests for `OverworldGenerator` and its stages, and for `ForestGenerator`.
//!
//! The cave look is not approved yet, so there are no cave goldens: the chunk path is tested
//! against the point spec (`block_at`) voxel by voxel, and the caves against statistics.
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos, LocalPos,
    MAX_CHUNK_COORD, Paletted,
};
use boxel_worldgen::overworld::{CaveSettings, Cheese, Fade, Heightmap, SurfaceFade};
use boxel_worldgen::{
    Caves, ForestGenerator, OverworldGenerator, OverworldSettings, PlantKind, Plants,
    TerrainGenerator, Tree, Trees, WorldGenerator,
};

fn registry() -> BlockRegistry {
    BlockRegistry::default_blocks()
}

fn state(r: &BlockRegistry, name: &str) -> BlockState {
    r.by_name(name).unwrap()
}

fn dense(g: &dyn WorldGenerator, r: &BlockRegistry, pos: ChunkPos) -> Vec<BlockState> {
    let mut out = vec![BlockState::AIR; CHUNK_VOLUME];
    g.generate(ChunkKey::new(pos), r).to_dense(&mut out);
    out
}

fn with_caves(caves: Option<CaveSettings>, trees: bool) -> OverworldGenerator {
    OverworldGenerator::new(
        0x00b0_7e1d,
        OverworldSettings {
            caves: caves.map(|settings| Caves { settings }),
            trees: trees.then(Trees::default),
            plants: trees.then(Plants::default),
            ..OverworldSettings::default()
        },
    )
}

/// The chunk holding the surface of the column in the middle of chunk column `(cx, cz)`.
fn surface_chunk(g: &OverworldGenerator, cx: i64, cz: i64) -> ChunkPos {
    let mid = |c: i64| c * 32 + 16;
    ChunkPos::new(cx, g.height_at(mid(cx), mid(cz)) >> 5, cz)
}

/// Every voxel of `generate` at `pos` equals `block_at`, the point spec (8 threads, one slab of
/// y layers each).
fn assert_matches_spec(g: &OverworldGenerator, r: &BlockRegistry, pos: ChunkPos) {
    let data = g.generate(ChunkKey::new(pos), r);
    let origin = pos.origin();
    std::thread::scope(|s| {
        for slab in 0..8u32 {
            let data = &data;
            s.spawn(move || {
                for ly in slab * 4..slab * 4 + 4 {
                    for lx in 0..32 {
                        for lz in 0..32 {
                            let l = LocalPos::new(lx, ly, lz);
                            let p = BlockPos::from_parts(pos, l);
                            assert_eq!(
                                data.get(l),
                                g.block_at(p, r),
                                "chunk {pos:?} voxel {p:?} (origin {origin:?})"
                            );
                        }
                    }
                }
            });
        }
    });
}

/// True if the chunk has stone and air.
fn is_mixed(data: &ChunkData, r: &BlockRegistry) -> bool {
    let mut d = vec![BlockState::AIR; CHUNK_VOLUME];
    data.to_dense(&mut d);
    d.contains(&BlockState::AIR) && d.contains(&state(r, "boxel:stone"))
}

/// The first chunk from `pos` along +x (at most 64 steps) with both stone and air.
fn mixed_near(g: &OverworldGenerator, r: &BlockRegistry, pos: ChunkPos) -> ChunkPos {
    (0..64)
        .map(|dx| ChunkPos::new(pos.x + dx, pos.y, pos.z))
        .find(|&p| is_mixed(&g.generate(ChunkKey::new(p), r), r))
        .unwrap_or_else(|| panic!("no chunk with caves near {pos:?}"))
}

/// The 32x32 column heights of chunk column `(cx, cz)`, indexed `x * 32 + z`.
fn column_heights(g: &OverworldGenerator, cx: i64, cz: i64) -> [i64; 1024] {
    g.settings.heightmap.chunk_heights(g.seed, cx * 32, cz * 32)
}

/// True if `data` (the chunk at `pos`) has air at the top block of one of its columns: a cave
/// opens the surface there.
fn opens_the_surface(g: &OverworldGenerator, pos: ChunkPos, data: &[BlockState]) -> bool {
    let heights = column_heights(g, pos.x, pos.z);
    heights.iter().enumerate().any(|(column, &h)| {
        let ly = h - pos.y * 32;
        (0..32).contains(&ly) && data[(ly as usize) << 10 | column] == BlockState::AIR
    })
}

/// Up to `want` surface chunks where the entrances open the ground, from the chunk columns
/// whose middle column is inside an entrance region (the mask picks them, so the coverage does
/// not depend on which columns a fixed list happens to hit).
fn opened_surface_chunks(g: &OverworldGenerator, r: &BlockRegistry, want: usize) -> Vec<ChunkPos> {
    let caves = g.settings.caves.as_ref().expect("caves");
    let e = caves.settings.entrances.expect("entrances");
    let mut found = Vec::new();
    for cx in -64..64 {
        for cz in -64..64 {
            if caves.entrance_mask(g.seed, cx * 32 + 16, cz * 32 + 16) <= e.mask_threshold {
                continue;
            }
            let heights = column_heights(g, cx, cz);
            let (lo, hi) = (heights.iter().min().unwrap(), heights.iter().max().unwrap());
            if let Some(pos) = (lo >> 5..=hi >> 5)
                .map(|y| ChunkPos::new(cx, y, cz))
                .find(|&pos| opens_the_surface(g, pos, &dense(g, r, pos)))
            {
                found.push(pos);
                if found.len() == want {
                    return found;
                }
            }
        }
    }
    found
}

/// A tree whose trunk ground a cave carved (so it is not generated), from the tree cells
/// around the origin.
fn lost_tree(g: &OverworldGenerator) -> Option<Tree> {
    let trees = g.settings.trees.as_ref().expect("trees");
    let grass = |x: i64, z: i64| {
        let h = g.height_at(x, z);
        g.settings.surface.is_grass(h).then_some(h)
    };
    (-120..120)
        .flat_map(|cx| (-120..120).map(move |cz| (cx, cz)))
        .filter_map(|(cx, cz)| trees.tree_in_cell(g.seed, cx, cz, grass))
        .find(|t| !g.is_solid(BlockPos::new(t.x, t.ground, t.z)))
}

/// True if a lattice corner of the chunk at `pos` is inside an entrance region and above the
/// entrances' bottom (the corners where the entrance tunnels count).
fn has_entrance_corner(g: &OverworldGenerator, pos: ChunkPos) -> bool {
    let caves = g.settings.caves.as_ref().expect("caves");
    let e = caves.settings.entrances.expect("entrances");
    let origin = pos.origin();
    let heights = Caves::corner_heights(&g.settings.heightmap, g.seed, origin);
    heights.iter().enumerate().any(|(c, &h)| {
        let [step_x, step_y, step_z] = Caves::CELL;
        let (x, z) = (
            origin.x + step_x * (c / 9) as i64,
            origin.z + step_z * (c % 9) as i64,
        );
        let mask = caves.entrance_mask(g.seed, x, z);
        (0..5).any(|j| mask > e.mask_threshold && h - (origin.y + step_y * j) < e.max_depth)
    })
}

#[test]
fn generate_matches_the_point_spec_near_far_and_deep() {
    let r = registry();
    let g = OverworldGenerator::default();
    let edge = 1i64 << 26; // 2^31 voxels
    let far = 1i64 << 35; // 2^40 voxels
    let limit = MAX_CHUNK_COORD - 1;
    let mut chunks = Vec::new();
    // Surface chunks (trees, the dense path) and the chunk above and below them.
    for (cx, cz) in [
        (0, 0),
        (7, -3),
        (edge - 1, -edge),
        (edge, 5),
        (far, -far),
        (-limit, limit),
    ] {
        let s = surface_chunk(&g, cx, cz);
        chunks.extend([-1, 0, 1].map(|dy| ChunkPos::new(s.x, s.y + dy, s.z)));
    }
    // Surface chunks where the entrances open the ground, with the chunk under them.
    let opened = opened_surface_chunks(&g, &r, 2);
    assert_eq!(
        opened.len(),
        2,
        "entrances that open the surface: {opened:?}"
    );
    for s in &opened {
        chunks.extend([0, -1].map(|dy| ChunkPos::new(s.x, s.y + dy, s.z)));
    }
    // The chunks of a tree that lost its ground to a cave: the ground's and the trunk's.
    let tree = lost_tree(&g).expect("a tree over a cave opening");
    for y in [tree.ground, tree.ground + 1] {
        chunks.push(ChunkPos::new(tree.x >> 5, y >> 5, tree.z >> 5));
    }
    // Deep chunks with caves (the 1-bit path), near the origin, deep, far and at the limit.
    for pos in [
        ChunkPos::new(0, -1, 0),
        ChunkPos::new(3, -100, -2),
        ChunkPos::new(1, -(1 << 20), 1),
        ChunkPos::new(-2, -(1 << 40), 5),
        ChunkPos::new(far, -3, far),
        ChunkPos::new(edge - 1, -20, -edge),
        ChunkPos::new(-edge, -(1 << 30), edge - 1),
        ChunkPos::new(limit - 63, -limit, -limit),
        ChunkPos::new(-limit, -limit, limit - 1),
    ] {
        chunks.push(mixed_near(&g, &r, pos));
    }
    chunks.sort();
    chunks.dedup();
    let mut one_bit = 0;
    for &pos in &chunks {
        let data = g.generate(ChunkKey::new(pos), &r);
        if matches!(data.blocks(), Paletted::Packed { bits: 1, .. }) {
            one_bit += 1;
        }
        assert_matches_spec(&g, &r, pos);
    }
    assert!(one_bit >= 9, "deep cave chunks are 1-bit ({one_bit})");
    // What the chunks covered: a carved top block, corners where the entrances count, and the
    // chunk where the lost tree would stand.
    assert!(
        chunks
            .iter()
            .any(|&pos| opens_the_surface(&g, pos, &dense(&g, &r, pos))),
        "no compared chunk opens the surface"
    );
    assert!(
        chunks.iter().any(|&pos| has_entrance_corner(&g, pos)),
        "no compared chunk has an entrance corner"
    );
    let trunk = ChunkPos::new(tree.x >> 5, (tree.ground + 1) >> 5, tree.z >> 5);
    assert!(chunks.contains(&trunk));
    let log = state(&r, "boxel:log");
    let trunk_base = BlockPos::new(tree.x, tree.ground + 1, tree.z);
    assert_ne!(g.block_at(trunk_base, &r), log, "a trunk over a hole");
}

#[test]
fn closed_caves_and_the_roof_match_the_spec() {
    let r = registry();
    let g = with_caves(Some(CaveSettings::closed()), true);
    for (cx, cz) in [(0, 0), (-5, 9)] {
        let s = surface_chunk(&g, cx, cz);
        for dy in [-1, 0] {
            assert_matches_spec(&g, &r, ChunkPos::new(s.x, s.y + dy, s.z));
        }
    }
    // The roof: nothing within 8 voxels under the surface is carved.
    let mut columns = 0;
    for x in (0..2048).step_by(7) {
        for z in (0..2048).step_by(13) {
            let h = g.height_at(x, z);
            for y in h - 7..=h {
                assert!(
                    g.is_solid(BlockPos::new(x, y, z)),
                    "({x}, {y}, {z}) under {h}"
                );
            }
            columns += 1;
        }
    }
    assert!(columns > 40_000);
}

#[test]
fn heightmap_equals_the_terrain_generator() {
    let terrain = TerrainGenerator::default();
    let heightmap = Heightmap::default();
    let overworld = OverworldGenerator::default();
    assert_eq!(heightmap, Heightmap::from_terrain(&terrain.settings));
    for (x, z) in [
        (0, 0),
        (-17, 33),
        (123_456, -654_321),
        ((1 << 31) - 1, -(1 << 31)),
        (1 << 40, -(1 << 40)),
        ((1 << 61) - 5, -(1 << 61)),
    ] {
        for dx in 0..8 {
            let (x, z) = (x + dx * 3, z - dx);
            let h = i64::from(terrain.height_at(x, z));
            assert_eq!(heightmap.height_at(terrain.seed, x, z), h, "({x}, {z})");
            assert_eq!(overworld.height_at(x, z), h, "({x}, {z})");
        }
        let grid = heightmap.chunk_heights(terrain.seed, x, z);
        for (i, &h) in grid.iter().enumerate() {
            let (cx, cz) = (x + (i >> 5) as i64, z + (i & 31) as i64);
            assert_eq!(h, i64::from(terrain.height_at(cx, cz)));
        }
    }
    // Without caves, trees and sea the overworld is the terrain without its bedrock floor.
    let r = registry();
    let mut plain = with_caves(None, false);
    plain.settings.sea = None;
    for (cx, cz) in [(0, 0), (9, -4), ((1 << 26) - 1, 1 << 35)] {
        for y in -1..=3 {
            let pos = ChunkPos::new(cx, y, cz);
            assert_eq!(dense(&plain, &r, pos), dense(&terrain, &r, pos), "{pos:?}");
        }
        assert_eq!(
            plain
                .generate(ChunkKey::new(ChunkPos::new(cx, -40, cz)), &r)
                .as_uniform(),
            Some(state(&r, "boxel:stone")),
            "stone forever below"
        );
    }
}

/// The state ids of the built-in blocks before block states (ADR 0019), which the recorded
/// fingerprints hash: a state's index here (the log only upright).
const RECORDED_IDS: [&str; 17] = [
    "boxel:air",
    "boxel:stone",
    "boxel:dirt",
    "boxel:grass",
    "boxel:sand",
    "boxel:gravel",
    "boxel:snow",
    "boxel:bedrock",
    "boxel:log[axis=y]",
    "boxel:leaves",
    "boxel:water[level=0]",
    "boxel:planks",
    "boxel:lamp_orange",
    "boxel:lamp_red",
    "boxel:lamp_blue",
    "boxel:lamp_white",
    "boxel:lava",
];

/// Fingerprint (log count, FNV-1a of every voxel state as its [`RECORDED_IDS`] index) of the
/// 3x3 chunk columns from `(x, z)`, layers 1..=4.
fn forest_fingerprint(g: &dyn WorldGenerator, r: &BlockRegistry, x: i64, z: i64) -> (usize, u64) {
    let log = r.by_name("boxel:log");
    let recorded: Vec<Option<u16>> = (0..r.len())
        .map(|s| {
            let name = r.state_name(BlockState(s as u16)).to_string();
            RECORDED_IDS
                .iter()
                .position(|n| *n == name)
                .map(|i| i as u16)
        })
        .collect();
    let (mut logs, mut h) = (0, 0xcbf2_9ce4_8422_2325u64);
    for dz in 0..3 {
        for dx in 0..3 {
            for y in 1..=4 {
                let d = dense(g, r, ChunkPos::new(x + dx, y, z + dz));
                logs += d.iter().filter(|s| Some(**s) == log).count();
                for s in &d {
                    let id = recorded[usize::from(s.0)].expect("a block of the recording");
                    for b in u64::from(id).to_le_bytes() {
                        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
                    }
                }
            }
        }
    }
    (logs, h)
}

#[test]
fn forest_keeps_the_recorded_trees() {
    // The fingerprints predate the plants (ADR 0018): without them, the same chunks.
    let forest = ForestGenerator {
        plants: None,
        sea: None,
        ..ForestGenerator::default()
    };
    let r = registry();
    // Recorded by the former example `Forest` with the i32 coordinates (the last chunks before
    // the old edge included).
    for ((x, z), expected) in [
        ((0, 0), (259, 0x0e4e_2255_79a0_f38d)),
        ((-3, 5), (284, 0xa713_c5ca_ff29_5b64)),
        ((7, -9), (6, 0x2418_de68_72ea_8247)),
        ((31_250, -31_250), (250, 0x9a87_d49f_8d57_45e4)),
        ((67_108_860, -67_108_863), (224, 0xdfd3_f57c_9c42_38a5)),
        ((-67_108_863, 67_108_860), (230, 0x96e8_7562_ea15_86c5)),
        ((12_345_678, -8_765_432), (245, 0x7995_606d_8843_dc2c)),
        ((-67_108_000, -67_100_000), (243, 0x97e4_a0c8_100c_a62c)),
    ] {
        assert_eq!(
            forest_fingerprint(&forest, &r, x, z),
            expected,
            "({x}, {z})"
        );
    }
    // Across +-2^31 voxels (chunks +-2^26), where the i32 tree math overflowed: trees keep
    // growing on both sides.
    let edge = 1i64 << 26;
    for (x, z) in [(edge - 1, 0), (edge + 2, 5), (-edge - 2, -edge - 1)] {
        let (logs, _) = forest_fingerprint(&forest, &r, x, z);
        assert!(logs > 0, "no trees at chunk ({x}, {z})");
    }
    let terrain = forest.terrain.vertical_chunks();
    assert_eq!(forest.vertical_chunks(), *terrain.start()..=(100 + 8) >> 5);
    assert_eq!(forest.name(), "forest");
}

#[test]
fn fast_paths_are_uniform_or_one_bit() {
    let r = registry();
    let stone = state(&r, "boxel:stone");
    let only_cheese = |threshold: f64| {
        let mut caves = CaveSettings {
            spaghetti: None,
            noodles: None,
            entrances: None,
            ..CaveSettings::default()
        };
        caves.cheese = caves.cheese.map(|c| Cheese { threshold, ..c });
        with_caves(Some(caves), false)
    };
    let deep = [
        ChunkPos::new(0, -3, 0),
        ChunkPos::new(5, -1000, -7),
        ChunkPos::new(1 << 35, -(1 << 35), 3),
    ];
    // Never carves: every corner is solid.
    let solid = only_cheese(10.0);
    // Carves everything, the surface fade included (1.5 at most).
    let hollow = only_cheese(-10.0);
    for pos in deep {
        let key = ChunkKey::new(pos);
        assert_eq!(solid.generate(key, &r).as_uniform(), Some(stone), "{pos:?}");
        assert_eq!(
            hollow.generate(key, &r).as_uniform(),
            Some(BlockState::AIR),
            "{pos:?}"
        );
        assert_eq!(
            with_caves(None, true).generate(key, &r).as_uniform(),
            Some(stone)
        );
    }
    // Near the surface the hollow caves leave only what is above the roof: nothing.
    let s = surface_chunk(&hollow, 0, 0);
    let below = ChunkPos::new(s.x, s.y - 1, s.z);
    assert_eq!(
        hollow.generate(ChunkKey::new(below), &r).as_uniform(),
        Some(BlockState::AIR)
    );
    // The default caves: a deep chunk with caves is stone and air in 1 bit per voxel.
    let g = OverworldGenerator::default();
    let pos = mixed_near(&g, &r, ChunkPos::new(0, -50, 0));
    let data = g.generate(ChunkKey::new(pos), &r);
    match data.blocks() {
        Paletted::Packed { palette, bits, .. } => {
            assert_eq!(*bits, 1);
            assert!(palette.contains(&stone) && palette.contains(&BlockState::AIR));
        }
        other => panic!("{other:?}"),
    }
    // Above every column and tree: air.
    let top = *g.vertical_chunks().end();
    assert_eq!(
        g.generate(ChunkKey::new(ChunkPos::new(3, top + 1, -4)), &r)
            .as_uniform(),
        Some(BlockState::AIR)
    );
}

#[test]
fn vertical_range_has_no_floor_and_covers_the_trees() {
    let g = OverworldGenerator::default();
    let range = g.vertical_chunks();
    assert_eq!(*range.start(), i64::MIN);
    // Highest column 112, trees up to 8 above: (112 + 1 + 8) >> 5.
    assert_eq!(*range.end(), (112 + 1 + 8) >> 5);
    assert_eq!(
        *with_caves(None, false).vertical_chunks().end(),
        (112 + 1) >> 5
    );
    assert_eq!(g.name(), "overworld");
}

/// Mean air fraction of `windows` 64^3 windows (2x2x2 chunks) at pseudo-random offsets of up
/// to 2048 chunks across and 1024 down from chunk `at`. Caverns are large and gather in layers
/// 512 voxels wide, so the windows must spread far to average them out.
fn air_fraction(g: &OverworldGenerator, r: &BlockRegistry, at: ChunkPos, windows: u32) -> f64 {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |n: u64| {
        // xorshift64
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n) as i64
    };
    let mut air = 0usize;
    let mut d = vec![BlockState::AIR; CHUNK_VOLUME];
    for _ in 0..windows {
        let (dx, dy, dz) = (next(4096) - 2048, next(1024), next(4096) - 2048);
        for i in 0..8 {
            let p = ChunkPos::new(
                at.x + dx + (i & 1),
                at.y - dy + (i >> 1 & 1),
                at.z + dz + (i >> 2),
            );
            g.generate(ChunkKey::new(p), r).to_dense(&mut d);
            air += d.iter().filter(|s| **s == BlockState::AIR).count();
        }
    }
    air as f64 / (windows as usize * 8 * CHUNK_VOLUME) as f64
}

#[test]
fn caves_have_the_same_statistics_at_any_distance_and_depth() {
    let r = registry();
    let g = OverworldGenerator::default();
    let limit = MAX_CHUNK_COORD - 4096;
    let places = [
        ChunkPos::new(0, -20, 0),
        ChunkPos::new(1 << 35, -20, 0),
        ChunkPos::new(0, -(1 << 35), 0),
        ChunkPos::new(-(1 << 26), -(1 << 20), 1 << 26),
        ChunkPos::new(limit, -limit, -limit),
    ];
    // Measured spread of one estimate from 1024 windows: 3% to 4% (standard deviation over 8
    // samplings), so two estimates differ by about 5%; 256 windows spread 4% to 8%.
    let windows = 1024;
    let air: Vec<f64> = std::thread::scope(|s| {
        let (g, r) = (&g, &r);
        let threads: Vec<_> = places
            .iter()
            .map(|&at| s.spawn(move || air_fraction(g, r, at, windows)))
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    });
    let near = air[0];
    // About 10% of the deep volume is air (the tuned look).
    assert!((0.08..0.12).contains(&near), "air near the origin {near}");
    for (at, &air) in places.iter().zip(&air).skip(1) {
        assert!(
            (air - near).abs() < 0.2 * near,
            "air at {at:?}: {air} against {near} near the origin"
        );
    }
}

#[test]
fn generation_is_deterministic_across_threads() {
    let r = registry();
    let g = OverworldGenerator::default();
    let mut chunks: Vec<ChunkPos> = (0..6)
        .flat_map(|i| {
            let s = surface_chunk(&g, i * 3 - 7, 11 - i);
            [
                s,
                ChunkPos::new(s.x, s.y - 1, s.z),
                ChunkPos::new(i, -5 - i, -i),
            ]
        })
        .collect();
    chunks.push(ChunkPos::new(1 << 35, -(1 << 35), -(1 << 35)));
    let expected: Vec<ChunkData> = chunks
        .iter()
        .map(|&p| g.generate(ChunkKey::new(p), &r))
        .collect();
    std::thread::scope(|s| {
        for t in 0..8 {
            let (g, r, chunks, expected) = (&g, &r, &chunks, &expected);
            s.spawn(move || {
                // Each thread in another order.
                for k in 0..chunks.len() {
                    let i = (k * 7 + t * 5) % chunks.len();
                    let data = g.generate(ChunkKey::new(chunks[i]), r);
                    assert!(data == expected[i], "thread {t}, {:?}", chunks[i]);
                }
            });
        }
    });
    // Another seed is another world.
    let other = OverworldGenerator::new(g.seed + 1, g.settings.clone());
    assert!(
        chunks
            .iter()
            .any(|&p| other.generate(ChunkKey::new(p), &r) != expected[0])
    );
}

#[test]
fn every_stage_has_its_own_salt() {
    let r = registry();
    let log = state(&r, "boxel:log");
    let leaves = state(&r, "boxel:leaves");
    let plants = [state(&r, "boxel:short_grass"), state(&r, "boxel:poppy")];
    let with_trees = OverworldGenerator::default();
    let without = with_caves(Some(CaveSettings::default()), false);
    // Trees and plants only add logs, leaves and plants over air: every other voxel is the same.
    for (cx, cz) in [(0, 0), (4, -6), (-11, 2)] {
        let s = surface_chunk(&with_trees, cx, cz);
        for dy in -1..=1 {
            let pos = ChunkPos::new(s.x, s.y + dy, s.z);
            let (a, b) = (dense(&with_trees, &r, pos), dense(&without, &r, pos));
            for (i, (&a, &b)) in a.iter().zip(&b).enumerate() {
                if a != b {
                    assert!(
                        b == BlockState::AIR && (a == log || a == leaves || plants.contains(&a)),
                        "{pos:?} {i}"
                    );
                }
            }
        }
    }
    // Another tree salt moves the trees, not the caves.
    let mut moved = OverworldGenerator::default();
    moved.settings.trees = Some(Trees { salt: 99 });
    let deep = ChunkPos::new(2, -4, 2);
    assert_eq!(dense(&moved, &r, deep), dense(&with_trees, &r, deep));
    // The entrances are off deep down: with or without them the deep caves are the same, so
    // the other types' noises do not depend on which types are on.
    let no_entrances = CaveSettings {
        entrances: None,
        ..CaveSettings::default()
    };
    let no_entrances = with_caves(Some(no_entrances), true);
    for pos in [deep, ChunkPos::new(-9, -40, 13)] {
        assert_eq!(dense(&no_entrances, &r, pos), dense(&with_trees, &r, pos));
    }
    // Another cave salt is other caves over the same surface.
    let salted = CaveSettings {
        salt: 7,
        ..CaveSettings::default()
    };
    let salted = with_caves(Some(salted), true);
    assert_ne!(dense(&salted, &r, deep), dense(&with_trees, &r, deep));
    assert_eq!(salted.height_at(5, 5), with_trees.height_at(5, 5));
}

#[test]
fn no_tree_floats() {
    let r = registry();
    let log = state(&r, "boxel:log");
    let leaves = state(&r, "boxel:leaves");
    let g = OverworldGenerator::default();
    let trees = Trees::default();
    // A tree stands on solid ground: the block under the lowest log is terrain.
    let mut checked = 0;
    for cx in -3..3 {
        for cz in -3..3 {
            let s = surface_chunk(&g, cx, cz);
            for dy in -1..=1 {
                let pos = ChunkPos::new(s.x, s.y + dy, s.z);
                let d = dense(&g, &r, pos);
                for (i, &b) in d.iter().enumerate() {
                    if b != log {
                        continue;
                    }
                    let p = BlockPos::from_parts(pos, LocalPos::from_index(i));
                    let below = BlockPos::new(p.x, p.y - 1, p.z);
                    let under = g.block_at(below, &r);
                    if under != log {
                        assert!(under != BlockState::AIR && under != leaves, "{p:?}");
                        assert!(g.is_solid(below), "{p:?}");
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked > 50, "only {checked} trees");
    // Some cells lose their tree to a cave that opens the ground under the trunk.
    let grass = |x: i64, z: i64| {
        let h = g.height_at(x, z);
        g.settings.surface.is_grass(h).then_some(h)
    };
    let mut lost = 0;
    for cx in -120..120 {
        for cz in -120..120 {
            if let Some(t) = trees.tree_in_cell(g.seed, cx, cz, grass)
                && !g.is_solid(BlockPos::new(t.x, t.ground, t.z))
            {
                lost += 1;
                let top = BlockPos::new(t.x, t.ground + 1, t.z);
                assert_ne!(g.block_at(top, &r), log, "a trunk over a hole at {top:?}");
            }
        }
    }
    assert!(lost > 0, "no tree stood over a cave opening");
}

#[test]
fn caves_off_near_the_surface_only_under_the_fades() {
    // A crafted fade that pushes caves 40 voxels down: nothing is carved above that depth.
    let deep_fade = Fade {
        full: 40,
        none: 41,
        strength: 4.0,
    };
    let default = CaveSettings::default();
    let caves = CaveSettings {
        entrances: None,
        surface_fade: SurfaceFade {
            cheese: deep_fade,
            spaghetti: deep_fade,
            noodles: deep_fade,
            ..default.surface_fade
        },
        ..default
    };
    let g = with_caves(Some(caves), false);
    for x in (0..512).step_by(5) {
        for z in (0..512).step_by(9) {
            let h = g.height_at(x, z);
            // Corner columns are up to 4 columns away, so a little shallower than the fade.
            for y in [h, h - 10, h - 20] {
                assert!(g.is_solid(BlockPos::new(x, y, z)), "({x}, {y}, {z})");
            }
        }
    }
}

#[test]
fn plants_grow_on_grass_where_the_column_hash_says() {
    let forest = ForestGenerator::default();
    let plants = forest.plants.clone().expect("default plants");
    let r = registry();
    let (grass, poppy) = (state(&r, "boxel:short_grass"), state(&r, "boxel:poppy"));
    let (log, leaves) = (state(&r, "boxel:log"), state(&r, "boxel:leaves"));
    let s = &forest.terrain.settings;
    let grows_on = i64::from(s.sand_level) + 1..=i64::from(s.snow_level);
    let mut counts = [0usize; 2];
    for (cx, cz) in [
        (0, 0),
        (-3, 5),
        (31_250, -31_250),
        (-67_108_863, 67_108_860),
    ] {
        let mut chunks = std::collections::HashMap::new();
        for lx in 0..32 {
            for lz in 0..32 {
                let (x, z) = (cx * 32 + lx, cz * 32 + lz);
                let ground = i64::from(forest.terrain.height_at(x, z));
                let cell = ground + 1;
                let pos = ChunkPos::new(cx, cell >> 5, cz);
                let data = chunks
                    .entry(pos)
                    .or_insert_with(|| forest.generate(ChunkKey::new(pos), &r));
                let local = LocalPos::new(lx as u32, (cell & 31) as u32, lz as u32);
                let got = data.get(local);
                let want = match plants.plant_at(forest.terrain.seed, x, z) {
                    _ if got == log || got == leaves => got,
                    Some(PlantKind::ShortGrass) if grows_on.contains(&ground) => grass,
                    Some(PlantKind::Flower) if grows_on.contains(&ground) => poppy,
                    _ => BlockState::AIR,
                };
                assert_eq!(got, want, "column ({x}, {z})");
                counts[0] += usize::from(got == grass);
                counts[1] += usize::from(got == poppy);
            }
        }
    }
    // About 12 % and 1.2 % of 4096 columns (fewer where the ground is not grass).
    assert!((250..=600).contains(&counts[0]), "{counts:?}");
    assert!((15..=90).contains(&counts[1]), "{counts:?}");
}
