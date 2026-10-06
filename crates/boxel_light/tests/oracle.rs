//! The light box against the global solution: in random 5x5x5-chunk worlds (opacity 0, 1, 5
//! and 15, RGB emitters opaque and clear, sky tops from the blocks, roofs far above, tops below
//! the world and `i64::MIN`), every chunk of the inner 3x3x3 gets the padded light of the
//! scalar global solver, bit for bit, with and without the fast paths.
mod common;

use boxel_light::oracle::OracleWorld;
use boxel_light::{LightPath, LightScratch, PaddedLight, SkyInput, SkyTile, compute, compute_full};
use boxel_voxel::{ChunkData, ChunkPos, LightTables, LightValue, LocalPos};
use common::{Blocks, World, random_world, test_registry};
use proptest::prelude::*;

/// Chunks per axis of the test worlds.
const N: usize = 5;

/// First differing voxel of a box result against the oracle, as a readable message.
fn first_mismatch(got: &PaddedLight, want: &[LightValue]) -> Option<String> {
    let got = got.to_vec();
    (0..want.len()).find(|&i| got[i] != want[i]).map(|i| {
        let (y, x, z) = (i / (34 * 34) - 1, i / 34 % 34 - 1, i % 34 - 1);
        format!(
            "local ({x}, {y}, {z}): got {:04x}, want {:04x}",
            got[i].0, want[i].0
        )
    })
}

/// Checks every inner chunk of `world` against the oracle; returns the paths taken.
fn check_world(world: &World, tables: &LightTables) -> Vec<LightPath> {
    let oracle =
        OracleWorld::from_chunks(world.min, [N; 3], &world.chunks, tables, world.tops.clone());
    let solution = oracle.solve();
    let tiles = world.tiles();
    let mut scratch = LightScratch::new();
    let mut out = PaddedLight::default();
    let mut paths = Vec::new();
    for cy in 1..N - 1 {
        for cx in 1..N - 1 {
            for cz in 1..N - 1 {
                let input = world.input(&tiles, tables, (cx, cy, cz));
                let want = oracle.padded(&solution, input.origin);
                let path = compute(&input, &mut scratch, &mut out);
                if let Some(m) = first_mismatch(&out, &want) {
                    panic!("chunk {:?} ({path:?}): {m}", input.origin);
                }
                paths.push(path);
                let full = compute_full(&input, &mut scratch, &mut out);
                assert!(matches!(full, LightPath::Full { .. }));
                if let Some(m) = first_mismatch(&out, &want) {
                    panic!("chunk {:?} (forced full): {m}", input.origin);
                }
            }
        }
    }
    paths
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 10, ..ProptestConfig::default() })]

    #[test]
    fn box_equals_global_solution(seed in any::<u64>(), heavy in any::<bool>(),
                                  far in 0usize..4) {
        let (registry, blocks) = test_registry(heavy);
        let tables = LightTables::from_registry(&registry);
        let min = [
            ChunkPos::new(-2, -2, -2),
            ChunkPos::new(1 << 26, -3, -(1 << 26)),
            ChunkPos::new(-(1 << 35), 1 << 35, 7),
            ChunkPos::new(5, -(1 << 50), 1 << 50),
        ][far];
        let world = random_world(seed, N, min, &blocks, &tables);
        check_world(&world, &tables);
    }
}

/// A fixed world per registry, so the regular run covers both every time.
#[test]
fn fixed_worlds_cover_every_path() {
    let mut all = Vec::new();
    for (seed, heavy) in [(1, false), (2, true), (3, false), (4, true)] {
        let (registry, blocks) = test_registry(heavy);
        let tables = LightTables::from_registry(&registry);
        let world = random_world(seed, N, ChunkPos::new(-2, -2, -2), &blocks, &tables);
        all.extend(check_world(&world, &tables));
    }
    assert!(
        all.iter()
            .any(|p| matches!(p, LightPath::Full { sky_pops, .. } if *sky_pops > 0))
    );
    assert!(
        all.iter()
            .any(|p| matches!(p, LightPath::Full { block_pops, .. } if *block_pops > 0))
    );
}

/// Hand-made scenes with simple closed forms.
fn scene(
    blocks: &Blocks,
    fill: impl Fn(i64, i64, i64) -> boxel_voxel::BlockState,
) -> Vec<ChunkData> {
    // 3x3x3 chunks from (-1, -1, -1): world voxels -32..64.
    let mut chunks = Vec::new();
    for cy in -1..=1i64 {
        for cx in -1..=1i64 {
            for cz in -1..=1i64 {
                let mut c = ChunkData::uniform(blocks.air);
                for i in 0..boxel_voxel::CHUNK_VOLUME {
                    let p = LocalPos::from_index(i);
                    let s = fill(
                        cx * 32 + i64::from(p.x()),
                        cy * 32 + i64::from(p.y()),
                        cz * 32 + i64::from(p.z()),
                    );
                    if s != blocks.air {
                        c.set(p, s);
                    }
                }
                chunks.push(c);
            }
        }
    }
    chunks
}

fn light_of(
    chunks: &[ChunkData],
    tiles: [&SkyTile; 9],
    tables: &LightTables,
) -> (PaddedLight, LightPath) {
    let refs: [Option<&ChunkData>; 27] = std::array::from_fn(|i| Some(&chunks[i]));
    let input = boxel_light::LightInput {
        origin: ChunkPos::new(0, 0, 0),
        chunks: refs,
        sky: SkyInput::Tiles(tiles),
        tables,
    };
    let mut out = PaddedLight::default();
    let path = compute(&input, &mut LightScratch::new(), &mut out);
    (out, path)
}

#[test]
fn a_lamp_in_the_dark_fades_by_distance() {
    let (registry, b) = test_registry(false);
    let tables = LightTables::from_registry(&registry);
    // Stone everywhere except a hollow cube 5..27; an orange lamp (14, 11, 6) at (16, 16, 16).
    let chunks = scene(&b, |x, y, z| {
        let inside = |v: i64| (5..27).contains(&v);
        if (x, y, z) == (16, 16, 16) {
            b.lamps[0]
        } else if inside(x) && inside(y) && inside(z) {
            b.air
        } else {
            b.stone
        }
    });
    let roof = SkyTile::uniform(1000, i64::MIN);
    let (light, path) = light_of(&chunks, [&roof; 9], &tables);
    assert!(matches!(path, LightPath::Full { sky_pops: 0, .. }));
    for (x, y, z) in [
        (16, 16, 16),
        (17, 16, 16),
        (20, 18, 16),
        (10, 16, 16),
        (5, 5, 5),
        (4, 16, 16),
    ] {
        let d = (x - 16i32).abs() + (y - 16i32).abs() + (z - 16i32).abs();
        let inside = |v: i32| (5..27).contains(&v);
        let want = if !(inside(x) && inside(y) && inside(z)) {
            [0; 3]
        } else if d == 0 {
            [14, 11, 6]
        } else {
            [14, 11, 6].map(|c: i32| (c - d).max(0) as u8)
        };
        assert_eq!(
            light.get(x, y, z),
            LightValue::from_rgb(want),
            "({x}, {y}, {z})"
        );
    }
}

#[test]
fn a_shaft_gets_full_sky_at_its_bottom() {
    let (registry, b) = test_registry(false);
    let tables = LightTables::from_registry(&registry);
    // Stone up to y = 40, a 1x1 shaft at (16, *, 16) down to y = 0 (open), a cave room 0..4.
    let chunks = scene(&b, |x, y, z| {
        let shaft = x == 16 && z == 16 && y >= 0;
        let room = (0..4).contains(&y) && (8..24).contains(&x) && (8..24).contains(&z);
        if y > 40 || shaft || room {
            b.air
        } else {
            b.stone
        }
    });
    let tops: Box<[i64; 1024]> =
        std::array::from_fn(|i| if i == (16 << 5) | 16 { -1 } else { 40 }).into();
    let tile = SkyTile::from_tops(tops, i64::MIN);
    let stone_top = SkyTile::uniform(40, i64::MIN);
    let mut tiles = [&stone_top; 9];
    tiles[4] = &tile;
    let (light, _) = light_of(&chunks, tiles, &tables);
    assert_eq!(light.get(16, 0, 16).sky(), 15, "direct down the shaft");
    assert_eq!(light.get(17, 0, 16).sky(), 14);
    // Six steps from the shaft through the room.
    assert_eq!(light.get(20, 2, 18).sky(), 15 - 6);
    assert_eq!(light.get(16, 20, 17).sky(), 0, "stone beside the shaft");
    assert_eq!(light.get(16, 32, 16).sky(), 15);
    assert_eq!(light.get(5, 32, 5).sky(), 0);
}

#[test]
fn fast_paths_equal_the_global_solution() {
    let (registry, b) = test_registry(false);
    let tables = LightTables::from_registry(&registry);
    // No emitters: the lamps are stone and the glows air.
    let plain = Blocks {
        lamps: [b.stone; 3],
        glows: [b.air; 2],
        smoke: None,
        ..b
    };
    let min = ChunkPos::new(3, -7, 1 << 40);
    let mut world = random_world(11, N, min, &plain, &tables);
    // A roof far above every column: every inner box is dark.
    let roof = min.origin().y + (N as i64) * 32 + (1 << 40);
    world.tops.iter_mut().for_each(|t| *t = roof);
    let paths = check_world(&world, &tables);
    assert!(paths.iter().all(|p| *p == LightPath::FastDark), "{paths:?}");

    // Air and glass under open sky (tops below the world or open forever): all open.
    let mut rng = common::Rng::new(5);
    for chunk in &mut world.chunks {
        *chunk = ChunkData::uniform(b.air);
        for _ in 0..rng.below(300) {
            let p = LocalPos::from_index(rng.below(boxel_voxel::CHUNK_VOLUME as u64) as usize);
            chunk.set(p, b.glass);
        }
    }
    let y0 = min.origin().y;
    for (i, t) in world.tops.iter_mut().enumerate() {
        *t = if i % 3 == 0 {
            i64::MIN
        } else {
            y0 - 1 - (i % 50) as i64
        };
    }
    let paths = check_world(&world, &tables);
    assert!(paths.iter().all(|p| *p == LightPath::FastOpen), "{paths:?}");
}

#[test]
fn missing_chunks_read_as_air_and_below_floor_is_dark() {
    let (registry, b) = test_registry(false);
    let tables = LightTables::from_registry(&registry);
    let world = random_world(31, 3, ChunkPos::new(-1, -1, -1), &b, &tables);
    let tiles = world.tiles();
    let air = ChunkData::uniform(b.air);
    let mut scratch = LightScratch::new();
    let (mut a, mut c) = (PaddedLight::default(), PaddedLight::default());
    // Every other neighbor missing, against the same neighbors as uniform air.
    let mut missing = world.input(&tiles, &tables, (1, 1, 1));
    let mut explicit = missing;
    for i in (0..27).filter(|i| i % 2 == 1) {
        missing.chunks[i] = None;
        explicit.chunks[i] = Some(&air);
    }
    let pa = compute(&missing, &mut scratch, &mut a);
    let pc = compute(&explicit, &mut scratch, &mut c);
    assert_eq!((pa, &a), (pc, &c));
    // Below the floor: as under a roof.
    let roof = SkyTile::uniform(1 << 40, i64::MIN);
    let mut under = explicit;
    under.sky = SkyInput::Tiles([&roof; 9]);
    let mut below = explicit;
    below.sky = SkyInput::BelowFloor;
    compute(&under, &mut scratch, &mut a);
    compute(&below, &mut scratch, &mut c);
    assert_eq!(a, c);
    assert!(a.to_vec().iter().all(|v| v.sky() == 0));
}
