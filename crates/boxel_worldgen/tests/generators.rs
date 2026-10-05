//! Behavior tests for the M0 generators (`FlatGenerator`, `TerrainGenerator`).
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos,
    LocalPos, Paletted,
};
use boxel_worldgen::{FlatGenerator, TerrainGenerator, TerrainSettings, WorldGenerator};

fn registry() -> BlockRegistry {
    BlockRegistry::default_blocks()
}

fn state(r: &BlockRegistry, name: &str) -> BlockState {
    r.by_name(name).unwrap()
}

fn chunk(g: &dyn WorldGenerator, r: &BlockRegistry, x: i32, y: i32, z: i32) -> ChunkData {
    g.generate(ChunkKey::new(ChunkPos::new(x, y, z)), r)
}

fn block_at(g: &dyn WorldGenerator, r: &BlockRegistry, p: BlockPos) -> BlockState {
    let (c, l) = p.split();
    g.generate(ChunkKey::new(c), r).get(l)
}

/// The column rules of `TerrainGenerator::generate`, voxel by voxel.
fn expected_terrain(g: &TerrainGenerator, r: &BlockRegistry, p: BlockPos, h: i32) -> BlockState {
    let s = &g.settings;
    let y = p.y;
    if y < s.bottom {
        BlockState::AIR
    } else if y == s.bottom {
        state(r, "boxel:bedrock")
    } else if y < h - s.dirt_depth {
        state(r, "boxel:stone")
    } else if y < h {
        state(r, "boxel:dirt")
    } else if y == h {
        if h <= s.sand_level {
            state(r, "boxel:sand")
        } else if h > s.snow_level {
            state(r, "boxel:snow")
        } else {
            state(r, "boxel:grass")
        }
    } else {
        BlockState::AIR
    }
}

/// Compares every voxel of chunk `c` against the column rules.
fn assert_matches_rules(g: &TerrainGenerator, r: &BlockRegistry, c: ChunkPos) {
    let data = g.generate(ChunkKey::new(c), r);
    let origin = c.origin();
    let mut heights = [[0; 32]; 32];
    for (x, row) in heights.iter_mut().enumerate() {
        for (z, h) in row.iter_mut().enumerate() {
            *h = g.height_at(origin.x + x as i32, origin.z + z as i32);
        }
    }
    let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
    data.to_dense(&mut dense);
    for (i, &got) in dense.iter().enumerate() {
        let l = LocalPos::from_index(i);
        let p = BlockPos::from_parts(c, l);
        let want = expected_terrain(g, r, p, heights[l.x() as usize][l.z() as usize]);
        assert_eq!(got, want, "chunk {c:?} voxel {p:?}");
    }
}

#[test]
fn terrain_is_deterministic() {
    let r = registry();
    let a = TerrainGenerator::default();
    let b = TerrainGenerator::new(a.seed, a.settings.clone());
    for (x, y, z) in [(0, 1, 0), (-3, 2, 5), (7, 0, -9)] {
        let first = chunk(&a, &r, x, y, z);
        assert_eq!(first, chunk(&a, &r, x, y, z));
        assert_eq!(first, chunk(&b, &r, x, y, z));
    }
    // A different seed gives different terrain.
    let other = TerrainGenerator::new(a.seed ^ 1, a.settings.clone());
    let differs = (-4..4).any(|x| a.height_at(x * 37, 11) != other.height_at(x * 37, 11));
    assert!(differs);
}

#[test]
fn terrain_vertical_range_matches_settings() {
    let g = TerrainGenerator::default();
    // bottom -64 -> chunk -2; 64 + 48 + 1 = 113 -> chunk 3.
    assert_eq!(g.vertical_chunks(), -2..=3);
    let custom = TerrainGenerator::new(
        1,
        TerrainSettings {
            base_height: 0,
            amplitude: 30.5,
            bottom: -1,
            ..TerrainSettings::default()
        },
    );
    // 0 + ceil(30.5) + 1 = 32 -> chunk 1; -1 >> 5 = -1.
    assert_eq!(custom.vertical_chunks(), -1..=1);
    assert_eq!(g.name(), "terrain");
}

#[test]
fn terrain_is_air_outside_vertical_chunks() {
    let r = registry();
    for g in [
        TerrainGenerator::default(),
        TerrainGenerator::new(
            99,
            TerrainSettings {
                base_height: -20,
                amplitude: 70.0,
                bottom: -200,
                ..TerrainSettings::default()
            },
        ),
    ] {
        let range = g.vertical_chunks();
        for (x, z) in [(0, 0), (5, -5), (-17, 3)] {
            for y in [
                range.end() + 1,
                range.end() + 4,
                range.start() - 1,
                range.start() - 6,
            ] {
                assert_eq!(
                    chunk(&g, &r, x, y, z).as_uniform(),
                    Some(BlockState::AIR),
                    "chunk ({x}, {y}, {z})"
                );
            }
        }
    }
}

#[test]
fn terrain_heights_stay_within_amplitude() {
    let g = TerrainGenerator::default();
    let s = &g.settings;
    let bound = s.amplitude.ceil() as i32;
    let (mut lo, mut hi) = (i32::MAX, i32::MIN);
    for x in (-2000..2000).step_by(13) {
        for z in (-2000..2000).step_by(17) {
            let h = g.height_at(x, z);
            assert!((h - s.base_height).abs() <= bound, "h({x}, {z}) = {h}");
            lo = lo.min(h);
            hi = hi.max(h);
        }
    }
    // The noise is actually used: real hills and valleys.
    assert!(hi - lo > 30, "flat terrain: [{lo}, {hi}]");
    // Neighbor columns differ by little (smooth terrain, no white noise).
    for x in -100..100 {
        assert!((g.height_at(x, 7) - g.height_at(x + 1, 7)).abs() <= 3);
    }
}

#[test]
fn terrain_chunks_follow_the_column_rules() {
    let r = registry();
    let g = TerrainGenerator::default();
    for (x, z) in [(0, 0), (-1, -1), (3, -2)] {
        for y in g.vertical_chunks() {
            assert_matches_rules(&g, &r, ChunkPos::new(x, y, z));
        }
    }
    // Sand and snow tops, and a bedrock floor inside a surface chunk.
    let varied = TerrainGenerator::new(
        5,
        TerrainSettings {
            base_height: 8,
            amplitude: 20.0,
            frequency: 1.0 / 16.0,
            octaves: 3,
            dirt_depth: 2,
            sand_level: 2,
            snow_level: 14,
            bottom: -6,
        },
    );
    let mut tops = std::collections::BTreeSet::new();
    for (x, z) in [(0, 0), (1, 0), (0, -1), (-2, 2)] {
        for y in varied.vertical_chunks() {
            assert_matches_rules(&varied, &r, ChunkPos::new(x, y, z));
        }
        for lx in 0..CHUNK_SIZE {
            let (wx, wz) = (x * CHUNK_SIZE + lx, z * CHUNK_SIZE);
            let h = varied.height_at(wx, wz);
            tops.insert(block_at(&varied, &r, BlockPos::new(wx, h, wz)));
        }
    }
    for name in ["boxel:sand", "boxel:grass", "boxel:snow"] {
        assert!(tops.contains(&state(&r, name)), "no {name} top");
    }
    // Degenerate settings still follow the rules: no dirt, a negative dirt depth (stone
    // reaches above the surface), and a bedrock floor above every column.
    for (dirt_depth, bottom) in [(0, -6), (-2, -6), (3, 40)] {
        let g = TerrainGenerator::new(
            8,
            TerrainSettings {
                dirt_depth,
                bottom,
                ..varied.settings.clone()
            },
        );
        let range = g.vertical_chunks();
        assert!(range.contains(&(bottom >> 5)), "{range:?}");
        for y in *range.start() - 1..=*range.end() + 1 {
            assert_matches_rules(&g, &r, ChunkPos::new(1, y, -1));
        }
    }
}

#[test]
fn terrain_fast_paths_are_uniform() {
    let r = registry();
    let g = TerrainGenerator::default();
    // Default heights are in 16..=112: chunk y = -1 (-32..=-1) is fully stone, y = 4 is air.
    assert_eq!(
        chunk(&g, &r, 2, -1, 3).as_uniform(),
        Some(state(&r, "boxel:stone"))
    );
    assert_eq!(chunk(&g, &r, 2, 4, 3).as_uniform(), Some(BlockState::AIR));
    // The bedrock chunk holds the floor at y = -64 and stone above it.
    let floor = chunk(&g, &r, 2, -2, 3);
    assert_eq!(
        floor.get(LocalPos::new(4, 0, 9)),
        state(&r, "boxel:bedrock")
    );
    assert_eq!(floor.get(LocalPos::new(4, 1, 9)), state(&r, "boxel:stone"));
}

#[test]
fn surface_chunk_has_grass_on_top() {
    let r = registry();
    let g = TerrainGenerator::default();
    let s = &g.settings;
    // First column near the origin whose height is in the grass band.
    let (x, z, h) = (0..4096)
        .map(|x| (x, 0, g.height_at(x, 0)))
        .find(|&(_, _, h)| h > s.sand_level && h <= s.snow_level)
        .expect("a grass column");
    let grass = state(&r, "boxel:grass");
    assert_eq!(block_at(&g, &r, BlockPos::new(x, h, z)), grass);
    assert_eq!(
        block_at(&g, &r, BlockPos::new(x, h + 1, z)),
        BlockState::AIR
    );
    for depth in 1..=s.dirt_depth {
        assert_eq!(
            block_at(&g, &r, BlockPos::new(x, h - depth, z)),
            state(&r, "boxel:dirt")
        );
    }
    assert_eq!(
        block_at(&g, &r, BlockPos::new(x, h - s.dirt_depth - 1, z)),
        state(&r, "boxel:stone")
    );
    // The surface chunk is paletted compactly (a handful of states, not Direct).
    let (c, _) = BlockPos::new(x, h, z).split();
    let data = g.generate(ChunkKey::new(c), &r);
    assert!(matches!(data.blocks, Paletted::Packed { bits: 1..=4, .. }));
    assert!(data.heap_bytes() <= CHUNK_VOLUME * 4 / 8 + 64);
}

fn flat() -> FlatGenerator {
    FlatGenerator::new(vec![
        ("boxel:bedrock".into(), 1),
        ("boxel:stone".into(), 3),
        ("boxel:dirt".into(), 2),
        ("boxel:grass".into(), 1),
    ])
}

#[test]
fn flat_layers_are_exact() {
    let r = registry();
    let g = flat();
    assert_eq!(g.name(), "flat");
    assert_eq!(g.vertical_chunks(), 0..=0);
    let expected = [
        "boxel:bedrock",
        "boxel:stone",
        "boxel:stone",
        "boxel:stone",
        "boxel:dirt",
        "boxel:dirt",
        "boxel:grass",
    ];
    for (cx, cz) in [(0, 0), (-3, 8)] {
        let data = chunk(&g, &r, cx, 0, cz);
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        data.to_dense(&mut dense);
        for (i, &got) in dense.iter().enumerate() {
            let y = LocalPos::from_index(i).y() as usize;
            let want = expected.get(y).map_or(BlockState::AIR, |n| state(&r, n));
            assert_eq!(got, want, "y = {y}");
        }
        assert_eq!(
            chunk(&g, &r, cx, -1, cz).as_uniform(),
            Some(BlockState::AIR)
        );
        assert_eq!(chunk(&g, &r, cx, 1, cz).as_uniform(), Some(BlockState::AIR));
    }
    // Deterministic.
    assert_eq!(chunk(&g, &r, 4, 0, 4), chunk(&g, &r, 4, 0, 4));
}

#[test]
fn flat_layers_span_chunks() {
    let r = registry();
    let stone = state(&r, "boxel:stone");
    let g = FlatGenerator::new(vec![
        ("boxel:stone".into(), 64),
        ("boxel:air".into(), 2),
        ("boxel:planks".into(), 1),
    ]);
    // 67 voxels: y 0..=66 -> chunks 0..=2.
    assert_eq!(g.vertical_chunks(), 0..=2);
    assert_eq!(chunk(&g, &r, 0, 0, 0).as_uniform(), Some(stone));
    assert_eq!(chunk(&g, &r, 0, 1, 0).as_uniform(), Some(stone));
    let top = chunk(&g, &r, 0, 2, 0);
    assert_eq!(top.get(LocalPos::new(0, 0, 0)), BlockState::AIR);
    assert_eq!(top.get(LocalPos::new(5, 2, 6)), state(&r, "boxel:planks"));
    assert_eq!(top.get(LocalPos::new(5, 3, 6)), BlockState::AIR);
    assert_eq!(chunk(&g, &r, 0, 3, 0).as_uniform(), Some(BlockState::AIR));
    // Exactly 32 voxels end at the chunk border.
    let full = FlatGenerator::new(vec![("boxel:stone".into(), 32)]);
    assert_eq!(full.vertical_chunks(), 0..=0);
    assert_eq!(chunk(&full, &r, 0, 0, 0).as_uniform(), Some(stone));
}

#[test]
fn flat_without_layers_is_empty() {
    let r = registry();
    let g = FlatGenerator::new(Vec::new());
    assert!(g.vertical_chunks().is_empty());
    assert_eq!(chunk(&g, &r, 0, 0, 0).as_uniform(), Some(BlockState::AIR));
}

#[test]
#[should_panic(expected = "boxel:nope")]
fn flat_unknown_block_panics_with_the_name() {
    let r = registry();
    let g = FlatGenerator::new(vec![("boxel:nope".into(), 1)]);
    // Even far from the layers: names are validated on every call.
    let _ = chunk(&g, &r, 0, 50, 0);
}

#[test]
fn generators_are_usable_as_trait_objects_across_threads() {
    let r = std::sync::Arc::new(registry());
    let generators: Vec<std::sync::Arc<dyn WorldGenerator>> = vec![
        std::sync::Arc::new(TerrainGenerator::default()),
        std::sync::Arc::new(flat()),
    ];
    for g in generators {
        let expected = chunk(g.as_ref(), &r, 1, 0, 1);
        let (g2, r2) = (g.clone(), r.clone());
        let from_thread = std::thread::spawn(move || chunk(g2.as_ref(), &r2, 1, 0, 1))
            .join()
            .unwrap();
        assert_eq!(expected, from_thread);
    }
}
