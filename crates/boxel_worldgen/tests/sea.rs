//! The sea (ADR 0021): source water from above every low column up to the sea level, sand on
//! the beaches, nothing growing under water, caves closed under it, in the forest and the
//! overworld, near the origin and far from it.
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_VOLUME, ChunkKey, ChunkPos, LocalPos,
    MAX_CHUNK_COORD,
};
use boxel_worldgen::{
    ForestGenerator, OverworldGenerator, OverworldSettings, Sea, TerrainGenerator, WorldGenerator,
};

fn dense(g: &dyn WorldGenerator, r: &BlockRegistry, pos: ChunkPos) -> Vec<BlockState> {
    let mut out = vec![BlockState::AIR; CHUNK_VOLUME];
    g.generate(ChunkKey::new(pos), r).to_dense(&mut out);
    out
}

/// Chunk columns to look at: near the origin, across +-2^31 voxels, at 2^40 and near the world
/// limit.
fn chunk_columns() -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    for (x, z) in [
        (0, 0),
        ((1 << 26) - 3, -(1 << 26) - 2),
        (1 << 35, -(1 << 35)),
        (MAX_CHUNK_COORD - 40, -MAX_CHUNK_COORD + 40),
    ] {
        for i in 0..12 {
            for j in 0..12 {
                out.push((x + i * 3, z + j * 3));
            }
        }
    }
    out
}

/// Checks the sea in every column of the chunk columns of `chunk_columns`: water (the source
/// state) from above each low column up to the level and nothing else above it, sand on top of
/// every column up to `level + beach`, and (with `floor`) solid ground for `Sea::FLOOR` blocks
/// under the water. Returns the number of columns under water.
fn check_sea(
    g: &dyn WorldGenerator,
    height: impl Fn(i64, i64) -> i64,
    sea: Sea,
    floor: bool,
) -> usize {
    let r = BlockRegistry::default_blocks();
    let water = r.by_name("boxel:water").unwrap();
    let sand = r.by_name("boxel:sand").unwrap();
    let mut wet = 0;
    for (cx, cz) in chunk_columns() {
        // The sea level lies in chunk layer 1 (32..=63); the ground under it in layers 0 and 1.
        let layers: Vec<Vec<BlockState>> = (0..=2)
            .map(|y| dense(g, &r, ChunkPos::new(cx, y, cz)))
            .collect();
        let at = |x: i64, y: i64, z: i64| {
            let l = LocalPos::new((x - cx * 32) as u32, (y & 31) as u32, (z - cz * 32) as u32);
            layers[(y >> 5) as usize][l.index()]
        };
        for x in cx * 32..cx * 32 + 32 {
            for z in cz * 32..cz * 32 + 32 {
                let h = height(x, z);
                if !(4..80).contains(&h) {
                    continue;
                }
                let p = BlockPos::new(x, h, z);
                if sea.sandy(h) && at(x, h, z) != BlockState::AIR {
                    assert_eq!(at(x, h, z), sand, "{p:?}: top of a beach or sea floor");
                }
                if !sea.covers(h) {
                    assert_ne!(at(x, h + 1, z), water, "{p:?}: water over a dry column");
                    continue;
                }
                wet += 1;
                for y in h + 1..=sea.level {
                    assert_eq!(at(x, y, z), water, "{p:?}: y {y} under the sea");
                }
                assert_eq!(
                    at(x, sea.level + 1, z),
                    BlockState::AIR,
                    "{p:?}: above the sea"
                );
                if floor {
                    for y in h - Sea::FLOOR + 1..=h {
                        assert_ne!(at(x, y, z), BlockState::AIR, "{p:?}: a cave under the sea");
                    }
                }
            }
        }
    }
    wet
}

#[test]
fn the_forest_sea_fills_the_low_columns() {
    let forest = ForestGenerator::default();
    let sea = forest.sea.expect("the default forest has a sea");
    assert_eq!(sea, Sea::default());
    let terrain = forest.terrain.clone();
    let wet = check_sea(
        &forest,
        |x, z| i64::from(terrain.height_at(x, z)),
        sea,
        false,
    );
    assert!(wet > 10_000, "only {wet} columns under water");
    // The same generator without a sea is the dry forest.
    let dry = ForestGenerator {
        sea: None,
        ..ForestGenerator::default()
    };
    let r = BlockRegistry::default_blocks();
    let water = r.by_name("boxel:water").unwrap();
    for (cx, cz) in chunk_columns().into_iter().step_by(17) {
        assert!(!dense(&dry, &r, ChunkPos::new(cx, 1, cz)).contains(&water));
    }
    // The terrain itself never had water.
    let plain = TerrainGenerator::default();
    for (cx, cz) in chunk_columns().into_iter().step_by(17) {
        assert!(!dense(&plain, &r, ChunkPos::new(cx, 1, cz)).contains(&water));
    }
}

#[test]
fn the_overworld_sea_keeps_its_caves_closed() {
    let overworld = OverworldGenerator::default();
    let sea = overworld
        .settings
        .sea
        .expect("the default overworld has a sea");
    let wet = check_sea(&overworld, |x, z| overworld.height_at(x, z), sea, true);
    assert!(wet > 10_000, "only {wet} columns under water");
}

#[test]
fn nothing_grows_under_water_or_on_the_beach() {
    let r = BlockRegistry::default_blocks();
    let growing = [
        "boxel:log",
        "boxel:leaves",
        "boxel:short_grass",
        "boxel:poppy",
    ]
    .map(|n| r.block_by_name(n).unwrap());
    let sea = Sea::default();
    let forest = ForestGenerator::default();
    let overworld = OverworldGenerator::new(
        forest.terrain.seed,
        OverworldSettings {
            caves: None,
            ..OverworldSettings::default()
        },
    );
    for g in [&forest as &dyn WorldGenerator, &overworld] {
        for (cx, cz) in chunk_columns().into_iter().take(144) {
            let d = dense(g, &r, ChunkPos::new(cx, 1, cz));
            for (i, &s) in d.iter().enumerate() {
                let l = LocalPos::from_index(i);
                let (x, z) = (cx * 32 + i64::from(l.x()), cz * 32 + i64::from(l.z()));
                let ground = i64::from(forest.terrain.height_at(x, z));
                if growing.contains(&r.block(s).unwrap()) && ground + 1 == 32 + i64::from(l.y()) {
                    assert!(
                        !sea.sandy(ground),
                        "{} on a sandy column ({x}, {z})",
                        g.name()
                    );
                }
            }
        }
    }
}

/// The overworld's water cells next to a cell they could flow into (air or a plant beside or
/// below them), in a block of `n` x `n` chunk columns from chunk `(cx, cz)`, layers 0 and 1 (all
/// the water of the default sea and the cells around it; cells next to the block's sides are left
/// out). Also returns the cells beside water the caves would have carved without the sea (their
/// density carves them, they are not under the sea floor).
fn open_shores(g: &OverworldGenerator, cx: i64, cz: i64, n: i64) -> (Vec<BlockPos>, Vec<BlockPos>) {
    let r = BlockRegistry::default_blocks();
    let water = r.by_name("boxel:water").unwrap();
    let plants = [
        r.by_name("boxel:short_grass").unwrap(),
        r.by_name("boxel:poppy").unwrap(),
    ];
    let caves = g.settings.caves.as_ref().unwrap();
    let side = n * 32;
    let mut cells = vec![BlockState::AIR; (side * side * 64) as usize];
    let index = |x: i64, y: i64, z: i64| ((y * side + x) * side + z) as usize;
    for i in 0..n {
        for j in 0..n {
            for layer in 0..=1 {
                let d = dense(g, &r, ChunkPos::new(cx + i, layer, cz + j));
                for (k, &s) in d.iter().enumerate() {
                    let l = LocalPos::from_index(k);
                    let (x, y, z) = (
                        i * 32 + i64::from(l.x()),
                        layer * 32 + i64::from(l.y()),
                        j * 32 + i64::from(l.z()),
                    );
                    cells[index(x, y, z)] = s;
                }
            }
        }
    }
    let (mut open, mut kept) = (Vec::new(), Vec::new());
    for y in 1..64 {
        for x in 1..side - 1 {
            for z in 1..side - 1 {
                if cells[index(x, y, z)] != water {
                    continue;
                }
                for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, -1, 0)] {
                    let s = cells[index(x + dx, y + dy, z + dz)];
                    let p = BlockPos::new(cx * 32 + x + dx, y + dy, cz * 32 + z + dz);
                    if s == BlockState::AIR || plants.contains(&s) {
                        open.push(p);
                    } else if s != water {
                        let h = g.height_at(p.x, p.z);
                        if p.y <= h.saturating_sub(Sea::FLOOR) || !Sea::default().covers(h) {
                            let carved = caves.carves(g.seed, p, h, |x, z| g.height_at(x, z));
                            if carved && g.beside_sea(p) {
                                kept.push(p);
                            }
                        }
                    }
                }
            }
        }
    }
    kept.sort_by_key(|p| (p.x, p.y, p.z));
    kept.dedup();
    (open, kept)
}

#[test]
fn caves_never_open_beside_the_sea() {
    let g = OverworldGenerator::default();
    let r = BlockRegistry::default_blocks();
    // Blocks of 16 x 16 chunk columns near the origin, across +-2^31 voxels, at 2^40 and near
    // the world limit.
    let mut kept_near_origin = 0;
    for (cx, cz) in [
        (-8, -8),
        ((1 << 26) - 8, -(1 << 26) - 8),
        (1 << 35, -(1 << 35)),
        (MAX_CHUNK_COORD - 40, -MAX_CHUNK_COORD + 40),
    ] {
        let (open, kept) = open_shores(&g, cx, cz, 16);
        assert!(
            open.is_empty(),
            "water next to air at {:?}",
            &open[..open.len().min(8)]
        );
        // The point form agrees: a kept cell is solid in `block_at`.
        for &p in kept.iter().step_by(5) {
            assert_ne!(g.block_at(p, &r), BlockState::AIR, "{p:?}");
        }
        if cx == -8 {
            kept_near_origin = kept.len();
        }
    }
    // Without the rule, caves near the origin open beside the sea.
    assert!(
        kept_near_origin > 0,
        "no cave reached the sea near the origin"
    );
}
