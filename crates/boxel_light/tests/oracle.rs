//! The light box against the global solution: in random 5x5x5-chunk worlds (opacity 0, 1, 5
//! and 15, RGB emitters opaque and clear, sky tops from the blocks, roofs far above, caps over
//! air inside the world, tops below the world and `i64::MIN`), every chunk of the inner 3x3x3
//! gets the padded light of the scalar global solver, bit for bit, with and without the fast
//! paths, with and without light filters (ADR 0060), with and without sky sources and realm
//! planes (ADR 0061). End to end on generated content: tiles from the walk feed the box and match
//! the oracle with brute-force tops capped at the walk floor. Colored sky (ADR 0064): worlds with
//! glass roofs, windows and panes (above and below the tops), with and without sources through
//! the glass, give the oracle's uncolored light and its sky channels; hand scenes check the
//! closed forms; without glass the switch changes nothing.
mod common;

use boxel_demo::worldgen::OverworldGenerator;
use boxel_light::box_light::{chunk_index, tile_index};
use boxel_light::oracle::OracleWorld;
use boxel_light::sky_tile::{DEFAULT_WALK_LIMIT, GeneratorTop, walk, walk_floor};
use boxel_light::{
    BeamTile, LayerData, LightInput, LightPath, LightScratch, PaddedLight, SkyInput, SkyTile,
    beam_tile, compute, compute_full,
};
use boxel_voxel::{ChunkData, ChunkKey, ChunkPos, LightTables, LightValue, LocalPos, SkySourceDef};
use boxel_worldgen::WorldGenerator;
use common::{
    Blocks, CYAN_FILTER, RED_FILTER, Rng, World, random_world, test_registry, test_registry_full,
    test_registry_with,
};
use proptest::prelude::*;

/// Chunks per axis of the test worlds.
const N: usize = 5;

/// First differing voxel of a box result against the oracle, as a readable message.
fn first_mismatch(got: &PaddedLight, want: &[LightValue]) -> Option<String> {
    let got = got.to_vec();
    (0..want.len()).find(|&i| got[i] != want[i]).map(|i| {
        // Signed: the border is local -1.
        let v = i as i64;
        let (y, x, z) = (v / (34 * 34) - 1, v / 34 % 34 - 1, v % 34 - 1);
        format!(
            "local ({x}, {y}, {z}): got {:04x}, want {:04x}",
            got[i].0, want[i].0
        )
    })
}

/// Checks every inner chunk of `world` against the oracle; returns the paths taken.
fn check_world(world: &World, tables: &LightTables) -> Vec<LightPath> {
    check_world_with(world, tables, &[], &[])
}

/// [`check_world`] with the beam tiles `beams` (by `cx * n + cz`, empty for none) built with
/// the realm planes `planes`.
fn check_world_with(
    world: &World,
    tables: &LightTables,
    beams: &[BeamTile],
    planes: &[i64],
) -> Vec<LightPath> {
    let mut oracle =
        OracleWorld::from_chunks(world.min, [N; 3], &world.chunks, tables, world.tops.clone());
    if beams.is_empty() {
        oracle.sources.clear();
    }
    oracle.planes = planes.to_vec();
    let solution = oracle.solve();
    let tiles = world.tiles();
    let mut scratch = LightScratch::new();
    let mut out = PaddedLight::default();
    let mut paths = Vec::new();
    for cy in 1..N - 1 {
        for cx in 1..N - 1 {
            for cz in 1..N - 1 {
                let input = world.input_with(&tiles, beams, tables, (cx, cy, cz));
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
                                  filters in any::<bool>(), far in 0usize..4) {
        let (registry, blocks) = test_registry_with(heavy, filters);
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

/// A world of [`random_world`] with sky sources ([`World::add_sources`]) and 0 to 2 realm planes
/// inside it (any row: the clip does not need them chunk aligned), its beam tiles and planes.
fn world_with_sources(
    seed: u64,
    min: ChunkPos,
    blocks: &Blocks,
    tables: &LightTables,
) -> (World, Vec<BeamTile>, Vec<i64>) {
    let mut world = random_world(seed, N, min, blocks, tables);
    let mut rng = Rng::new(seed ^ 0x5eed_50c3);
    world.add_sources(&mut rng, blocks, tables);
    let y0 = min.origin().y;
    let mut planes: Vec<i64> = (0..rng.below(3))
        .map(|_| y0 + rng.below((N * 32) as u64) as i64)
        .collect();
    planes.sort_unstable();
    planes.dedup();
    let beams = world.beam_tiles(tables, &planes);
    (world, beams, planes)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 10, ..ProptestConfig::default() })]

    #[test]
    fn box_with_beams_equals_global_solution(seed in any::<u64>(), heavy in any::<bool>(),
                                             filters in any::<bool>(), far in 0usize..4) {
        let (registry, blocks) = test_registry_full(heavy, filters, true);
        let tables = LightTables::from_registry(&registry);
        let min = [
            ChunkPos::new(-2, -2, -2),
            ChunkPos::new(1 << 26, -3, -(1 << 26)),
            ChunkPos::new(-(1 << 35), 1 << 35, 7),
            ChunkPos::new(5, -(1 << 50), 1 << 50),
        ][far];
        let (world, beams, planes) = world_with_sources(seed, min, &blocks, &tables);
        check_world_with(&world, &tables, &beams, &planes);
    }
}

/// Fixed worlds with sky sources, in every registry, also under a roof far above every column,
/// where only the beams give sky light to boxes the tops leave dark.
#[test]
fn fixed_worlds_with_sources_match_and_light_dark_boxes() {
    let mut beam_lit = 0;
    for (seed, heavy, filters) in [(41, false, false), (42, true, false), (43, true, true)] {
        let (registry, blocks) = test_registry_full(heavy, filters, true);
        let tables = LightTables::from_registry(&registry);
        let (mut world, beams, planes) =
            world_with_sources(seed, ChunkPos::new(-2, -2, -2), &blocks, &tables);
        check_world_with(&world, &tables, &beams, &planes);
        // A roof far above every column: only beams light anything.
        let roof = world.min.origin().y + 1000;
        world.tops.iter_mut().for_each(|t| *t = (*t).max(roof));
        let roofed = check_world_with(&world, &tables, &beams, &planes);
        let dark = check_world(&world, &tables);
        let sky = |p: &LightPath| matches!(p, LightPath::Full { sky_pops, .. } if *sky_pops > 0);
        for (with, without) in roofed.iter().zip(&dark) {
            assert!(!sky(without));
            beam_lit += usize::from(sky(with));
        }
    }
    assert!(beam_lit > 0, "no dark box lit by a beam");
}

/// A fixed world per registry, so the regular run covers both every time.
#[test]
fn fixed_worlds_cover_every_path() {
    let mut all = Vec::new();
    for (seed, heavy, filters) in [
        (1, false, false),
        (2, true, false),
        (3, false, false),
        (4, true, false),
        (5, false, true),
        (6, true, true),
    ] {
        let (registry, blocks) = test_registry_with(heavy, filters);
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
    let input = LightInput {
        origin: ChunkPos::new(0, 0, 0),
        chunks: refs,
        sky: SkyInput::Tiles(tiles),
        beams: [None; 9],
        tables,
        colored_sky: false,
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

/// ADR 0019: in a corridor, a slab lets the lamp's light through at the cost of air, a double
/// slab stops it like stone.
#[test]
fn a_double_slab_stops_light_and_a_slab_does_not() {
    let (registry, b) = test_registry(false);
    let tables = LightTables::from_registry(&registry);
    let state = |name: &str| registry.parse_state(name).unwrap();
    let roof = SkyTile::uniform(1000, i64::MIN);
    for (slab, beyond) in [
        ("boxel:stone_slab[type=bottom]", [14 - 6, 11 - 6, 0]),
        ("boxel:stone_slab[type=top]", [14 - 6, 11 - 6, 0]),
        ("boxel:stone_slab[type=double]", [0; 3]),
    ] {
        let slab = state(slab);
        // Stone everywhere but the corridor x in 5..27 at y = z = 16; the lamp at x = 10, the
        // slab at x = 14.
        let chunks = scene(&b, |x, y, z| match (x, y, z) {
            (10, 16, 16) => b.lamps[0],
            (14, 16, 16) => slab,
            (5..27, 16, 16) => b.air,
            _ => b.stone,
        });
        let (light, _) = light_of(&chunks, [&roof; 9], &tables);
        assert_eq!(light.get(13, 16, 16), LightValue::from_rgb([11, 8, 3]));
        assert_eq!(
            light.get(16, 16, 16),
            LightValue::from_rgb(beyond),
            "{}",
            registry.state_name(slab)
        );
    }
}

/// ADR 0060: a white lamp behind a red glass wall lights the far side red, each channel scaled
/// after the cost of the glass and then falling by 1 per voxel; sky light ignores the filter.
#[test]
fn a_filter_scales_each_block_channel_and_leaves_sky_alone() {
    use boxel_voxel::transmit;
    for heavy in [false, true] {
        let (registry, b) = test_registry_with(heavy, true);
        let tables = LightTables::from_registry(&registry);
        let [red, cyan] = b.filters.unwrap();
        let white = b.glows[1]; // The lantern: (12, 12, 12), opacity 1.
        // A sealed room x, y, z in 5..27 with a white lantern at x = 10 and a wall at x = 14 of
        // red glass, then one of cyan water at x = 20.
        let chunks = scene(&b, |x, y, z| {
            let inside = |v: i64| (5..27).contains(&v);
            match (x, y, z) {
                (10, 16, 16) => white,
                _ if !(inside(x) && inside(y) && inside(z)) => b.stone,
                (14, _, _) => red,
                (20, _, _) => cyan,
                _ => b.air,
            }
        });
        let roof = SkyTile::uniform(1000, i64::MIN);
        let (light, path) = light_of(&chunks, [&roof; 9], &tables);
        assert!(matches!(path, LightPath::Full { sky_pops: 0, .. }));
        let scaled = |l: u16, f: [u8; 3]| f.map(|c| transmit(l, c) as u8);
        assert_eq!(light.get(13, 16, 16), LightValue::from_rgb([9; 3]));
        // Into the glass: 9 - 1 = 8, then scaled.
        let glass = scaled(8, RED_FILTER);
        assert_eq!(light.get(14, 16, 16), LightValue::from_rgb(glass));
        let after = glass.map(|c| c.saturating_sub(5));
        assert_eq!(light.get(19, 16, 16), LightValue::from_rgb(after));
        // Into the water: its cost (1), then its filter.
        let water = [0, 1, 2]
            .map(|k| transmit(u16::from(after[k]).saturating_sub(1), CYAN_FILTER[k]) as u8);
        assert_eq!(light.get(20, 16, 16), LightValue::from_rgb(water));
        // The same room without the walls: white.
        let open = scene(&b, |x, y, z| {
            let inside = |v: i64| (5..27).contains(&v);
            match (x, y, z) {
                (10, 16, 16) => white,
                _ if inside(x) && inside(y) && inside(z) => b.air,
                _ => b.stone,
            }
        });
        let (light, _) = light_of(&open, [&roof; 9], &tables);
        assert_eq!(light.get(14, 16, 16), LightValue::from_rgb([8; 3]));
    }
    // Sky through a red glass roof over a shaft stays white: glass does not stop direct sky,
    // and a filter never tints it.
    let (registry, b) = test_registry_with(false, true);
    let tables = LightTables::from_registry(&registry);
    let [red, _] = b.filters.unwrap();
    let chunks = scene(&b, |x, y, z| match (x, y, z) {
        (16, 41, 16) => red,
        (16, 0..=41, 16) | (8..24, 0..4, 8..24) => b.air,
        _ if y > 41 => b.air,
        _ => b.stone,
    });
    let tops: Box<[i64; 1024]> =
        std::array::from_fn(|i| if i == (16 << 5) | 16 { -1 } else { 41 }).into();
    let tile = SkyTile::from_tops(tops, i64::MIN);
    let stone_top = SkyTile::uniform(41, i64::MIN);
    let mut tiles = [&stone_top; 9];
    tiles[4] = &tile;
    let (light, _) = light_of(&chunks, tiles, &tables);
    assert_eq!(light.get(16, 0, 16), LightValue::FULL_SKY);
    assert_eq!(light.get(17, 0, 16), LightValue::new(14, 0, 0, 0));
}

/// A cavern 64 high under a roof far from the sky, lit by a down source of range 64 in the
/// first air cell under the roof of every column, with a ledge across it: the cavern is all 15
/// but under the ledge, which gets spread light from the beams beside it; the same with the
/// sky tops and below the floor. Without beams the box is dark.
#[test]
fn a_cavern_roof_of_sources_lights_it_like_daylight() {
    let (registry, b) = test_registry_full(false, false, true);
    let tables = LightTables::from_registry(&registry);
    let source = b
        .sources
        .iter()
        .copied()
        .find(|&s| {
            tables.sky_source(s)
                == Some(SkySourceDef {
                    dir: boxel_voxel::BeamDir::Down,
                    range: 64,
                })
        })
        .unwrap();
    // World voxels -32..64: the cavern y in -20..=43, the ledge y = 10, x in -8..8.
    let chunks = scene(&b, |x, y, z| match (x, y, z) {
        (_, 43, _) => source,
        (-8..8, 10, _) => b.stone,
        (_, -20..=42, _) => b.air,
        _ => b.stone,
    });
    let window = -16..=47;
    let beams: Vec<BeamTile> = (0..9)
        .map(|i| {
            let (dx, dz) = (i as i64 / 3 - 1, i as i64 % 3 - 1);
            beam_tile::build(window.clone(), &[], &tables, |cy| {
                if (-1..=1).contains(&cy) {
                    let ci = ((cy + 1) * 9 + (dx + 1) * 3 + dz + 1) as usize;
                    LayerData::Ref(&chunks[ci])
                } else {
                    LayerData::Air
                }
            })
        })
        .collect();
    let refs: [Option<&ChunkData>; 27] = std::array::from_fn(|i| Some(&chunks[i]));
    let roof = SkyTile::uniform(1000, i64::MIN);
    let mut input = LightInput {
        origin: ChunkPos::new(0, 0, 0),
        chunks: refs,
        sky: SkyInput::Tiles([&roof; 9]),
        beams: [None; 9],
        tables: &tables,
        colored_sky: false,
    };
    let mut scratch = LightScratch::new();
    let mut out = PaddedLight::default();
    assert_eq!(compute(&input, &mut scratch, &mut out), LightPath::FastDark);
    input.beams = std::array::from_fn(|i| Some(&beams[i]));
    let path = compute(&input, &mut scratch, &mut out);
    assert!(
        matches!(path, LightPath::Full { sky_pops, .. } if sky_pops > 0),
        "{path:?}"
    );
    assert_eq!(out.get(20, 0, 5).sky(), 15);
    assert_eq!(out.get(20, 32, 5).sky(), 15, "the padded top");
    assert_eq!(out.get(5, 11, 5).sky(), 15, "on the ledge");
    assert_eq!(out.get(5, 10, 5).sky(), 0, "the ledge");
    // Under the ledge: 8 cells from the beam at x = 8 (and 9 from the one at x = -9).
    assert_eq!(out.get(0, 9, 5).sky(), 15 - 8);
    assert_eq!(out.get(-1, 9, 5).sky(), 15 - 8, "8 from the beam at x = -9");
    assert_eq!(out.get(7, 9, 5).sky(), 14);
    // The oracle agrees, and so do the forced full path and below the floor.
    let mut oracle = OracleWorld::from_chunks(
        ChunkPos::new(-1, -1, -1),
        [3; 3],
        &chunks,
        &tables,
        vec![1000; 96 * 96],
    );
    let want = oracle.padded(&oracle.solve(), input.origin);
    assert_eq!(first_mismatch(&out, &want), None);
    let mut full = PaddedLight::default();
    compute_full(&input, &mut scratch, &mut full);
    assert_eq!(full, out);
    input.sky = SkyInput::BelowFloor;
    let below = compute(&input, &mut scratch, &mut full);
    assert!(matches!(below, LightPath::Full { .. }));
    assert_eq!(full, out);
    compute_full(&input, &mut scratch, &mut full);
    assert_eq!(full, out);
    // Without the sources the oracle is dark.
    oracle.sources.clear();
    assert!(oracle.solve().iter().all(|v| v.sky() == 0));
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
    let mut rng = Rng::new(5);
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

#[test]
fn mismatches_on_the_border_are_reported() {
    let want = vec![LightValue::DARK; boxel_light::PADDED_VOLUME];
    let got = PaddedLight::Uniform(LightValue::FULL_SKY);
    let m = first_mismatch(&got, &want).expect("a mismatch");
    assert!(m.starts_with("local (-1, -1, -1)"), "{m}");
}

#[test]
fn a_removed_lamp_or_one_outside_the_box_keeps_the_fast_paths() {
    let (registry, b) = test_registry(false);
    let tables = LightTables::from_registry(&registry);
    let mut chunks: Vec<ChunkData> = (0..27).map(|_| ChunkData::uniform(b.stone)).collect();
    // A lamp placed and removed in the center chunk: still in its palette.
    let center = chunk_index(0, 0, 0);
    chunks[center].set(LocalPos::new(5, 5, 5), b.lamps[0]);
    chunks[center].set(LocalPos::new(5, 5, 5), b.stone);
    assert!(chunks[center].may_emit(&tables));
    // A lamp in the far half of the +x neighbor: 20 voxels past the chunk, outside the box.
    let east = chunk_index(1, 0, 0);
    chunks[east].set(LocalPos::new(20, 16, 16), b.lamps[1]);
    let roof = SkyTile::uniform(1000, i64::MIN);
    let (light, path) = light_of(&chunks, [&roof; 9], &tables);
    assert_eq!(path, LightPath::FastDark);
    assert_eq!(light, PaddedLight::Uniform(LightValue::DARK));
    // The same under open sky.
    let below = SkyTile::uniform(-1000, i64::MIN);
    let (_, path) = light_of(&chunks, [&below; 9], &tables);
    assert_eq!(path, LightPath::FastOpen);
    // A lamp inside the box (local x 10 of the neighbor): the floods run.
    chunks[east].set(LocalPos::new(10, 16, 16), b.lamps[1]);
    let (_, path) = light_of(&chunks, [&roof; 9], &tables);
    assert!(
        matches!(path, LightPath::Full { block_pops, .. } if block_pops > 0),
        "{path:?}"
    );
}

/// The walk feeding the box on generated content: 3x3 overworld chunk columns, the 5 layers
/// under the generator's top. Tiles come from `walk`, with a short limit (most surface columns
/// end at the floor, over air inside the boxes) and the default one; the oracle uses the
/// brute-force tops of the same content, capped at the floor. The center column's inner
/// chunks match bit for bit.
#[test]
fn walked_tiles_light_generated_content_exactly() {
    let g = OverworldGenerator::default();
    let registry = boxel_demo::blocks::registry();
    let tables = LightTables::from_registry(&registry);
    let range = g.vertical_chunks();
    let top = GeneratorTop::of(&range).voxel().unwrap();
    let ny = 5usize;
    let center = (3i64, -2i64);
    let min = ChunkPos::new(center.0 - 1, *range.end() - (ny as i64 - 1), center.1 - 1);
    let dims = [3, ny, 3];
    let generate =
        |x: i64, y: i64, z: i64| g.generate(ChunkKey::new(ChunkPos::new(x, y, z)), &registry);
    let mut chunks = Vec::with_capacity(9 * ny);
    for cy in 0..ny as i64 {
        for cx in 0..3 {
            for cz in 0..3 {
                chunks.push(generate(min.x + cx, min.y + cy, min.z + cz));
            }
        }
    }
    let side = 96usize;
    let world_bottom = min.origin().y;
    let mut oracle = OracleWorld::from_chunks(min, dims, &chunks, &tables, vec![0; side * side]);
    let brute = oracle.brute_force_tops();
    let mut scratch = LightScratch::new();
    let mut out = PaddedLight::default();
    for limit in [40, DEFAULT_WALK_LIMIT] {
        let floor = walk_floor(top, limit);
        let tiles: Vec<SkyTile> = (0..9)
            .map(|i| {
                let (dx, dz) = (i / 3 - 1, i % 3 - 1);
                walk(top, floor, &tables, |cy| {
                    if cy > *range.end() {
                        LayerData::Air
                    } else {
                        LayerData::Owned(generate(center.0 + dx, cy, center.1 + dz))
                    }
                })
            })
            .collect();
        oracle.tops = brute.iter().map(|&t| t.max(floor)).collect();
        // The walk's tops are the capped brute force (or both lie below the world: the same
        // light inside it).
        let mut capped = 0;
        for x in 0..side {
            for z in 0..side {
                let (dx, dz) = ((x / 32) as i32 - 1, (z / 32) as i32 - 1);
                let got = tiles[tile_index(dx, dz)].top((x % 32) as u32, (z % 32) as u32);
                let want = oracle.tops[x * side + z];
                assert!(
                    got == want || (got < world_bottom && want < world_bottom),
                    "limit {limit}, column ({x}, {z}): walk {got}, brute force {want}"
                );
                capped += usize::from(got == floor && brute[x * side + z] < floor);
            }
        }
        if limit == 40 {
            assert!(capped > 0, "no column capped at the floor");
            assert!(
                floor > world_bottom + 32,
                "the floor lies inside an inner chunk"
            );
        }
        let solution = oracle.solve();
        let refs: [&SkyTile; 9] = std::array::from_fn(|i| &tiles[i]);
        let mut sky_pops = 0;
        for cy in 1..ny - 1 {
            let mut near = [None; 27];
            for dy in -1..=1i32 {
                for dx in -1..=1i32 {
                    for dz in -1..=1i32 {
                        let (x, y, z) = (
                            (1 + dx) as usize,
                            (cy as i32 + dy) as usize,
                            (1 + dz) as usize,
                        );
                        near[chunk_index(dx, dy, dz)] = Some(&chunks[(y * 3 + x) * 3 + z]);
                    }
                }
            }
            let input = LightInput {
                origin: ChunkPos::new(min.x + 1, min.y + cy as i64, min.z + 1),
                chunks: near,
                sky: SkyInput::Tiles(refs),
                beams: [None; 9],
                tables: &tables,
                colored_sky: false,
            };
            let want = oracle.padded(&solution, input.origin);
            let path = compute(&input, &mut scratch, &mut out);
            if let Some(m) = first_mismatch(&out, &want) {
                panic!("limit {limit}, chunk {:?} ({path:?}): {m}", input.origin);
            }
            if let LightPath::Full { sky_pops: n, .. } = path {
                sky_pops += n;
            }
            compute_full(&input, &mut scratch, &mut out);
            if let Some(m) = first_mismatch(&out, &want) {
                panic!("limit {limit}, chunk {:?} (forced full): {m}", input.origin);
            }
        }
        assert!(sky_pops > 0, "limit {limit}: no sky flood");
    }
}

/// The colored results of a [`check_colored`] run.
#[derive(Debug, Default)]
struct ColoredRun {
    paths: Vec<LightPath>,
    /// Boxes that took the colored path.
    colored_boxes: usize,
    /// Results in the colored form.
    colored_results: usize,
}

/// Checks every inner chunk of `world` with colored sky against the oracle: the uncolored light
/// (what gameplay reads) against [`OracleWorld::solve`], the sky channels against
/// [`OracleWorld::solve_colored`], with and without the fast paths. A result is colored exactly
/// when some padded voxel's sky channels differ from its uncolored sky.
fn check_colored(
    world: &World,
    tables: &LightTables,
    beams: &[BeamTile],
    planes: &[i64],
) -> ColoredRun {
    let mut oracle =
        OracleWorld::from_chunks(world.min, [N; 3], &world.chunks, tables, world.tops.clone());
    if beams.is_empty() {
        oracle.sources.clear();
    }
    oracle.planes = planes.to_vec();
    let light = oracle.solve();
    let rgb = oracle.solve_colored();
    let tiles = world.colored_tiles(tables);
    let mut scratch = LightScratch::new();
    let mut out = PaddedLight::default();
    let mut run = ColoredRun::default();
    for cy in 1..N - 1 {
        for cx in 1..N - 1 {
            for cz in 1..N - 1 {
                let mut input = world.input_with(&tiles, beams, tables, (cx, cy, cz));
                input.colored_sky = true;
                let want = oracle.padded(&light, input.origin);
                let want_rgb = oracle.padded(&rgb, input.origin);
                let tinted = want.iter().zip(&want_rgb).any(|(l, c)| *c != [l.sky(); 3]);
                for full in [false, true] {
                    let path = if full {
                        compute_full(&input, &mut scratch, &mut out)
                    } else {
                        compute(&input, &mut scratch, &mut out)
                    };
                    let at = format!("chunk {:?} ({path:?}, full {full})", input.origin);
                    if let Some(m) = first_mismatch(&out, &want) {
                        panic!("{at}: {m}");
                    }
                    let got_rgb = out.to_sky_rgb_vec();
                    if let Some(i) = (0..want_rgb.len()).find(|&i| got_rgb[i] != want_rgb[i]) {
                        let v = i as i64;
                        let (y, x, z) = (v / (34 * 34) - 1, v / 34 % 34 - 1, v % 34 - 1);
                        panic!(
                            "{at}: sky rgb at local ({x}, {y}, {z}): got {:?}, want {:?}",
                            got_rgb[i], want_rgb[i]
                        );
                    }
                    assert_eq!(out.is_colored(), tinted, "{at}: the colored form");
                    if !full {
                        run.colored_boxes += usize::from(matches!(
                            path,
                            LightPath::Full {
                                colored_pops: Some(_),
                                ..
                            }
                        ));
                        run.colored_results += usize::from(tinted);
                        run.paths.push(path);
                    }
                }
            }
        }
    }
    run
}

/// A world of [`random_world`] with glass ([`World::add_glass`]) and, with `sources`, sky
/// sources and realm planes as [`world_with_sources`], with its colored beam tiles and planes.
fn colored_world(
    seed: u64,
    min: ChunkPos,
    blocks: &Blocks,
    tables: &LightTables,
    sources: bool,
) -> (World, Vec<BeamTile>, Vec<i64>) {
    let mut world = random_world(seed, N, min, blocks, tables);
    let mut rng = Rng::new(seed ^ 0x6c_a55e);
    world.add_glass(&mut rng, blocks);
    if !sources {
        return (world, Vec::new(), Vec::new());
    }
    world.add_sources(&mut rng, blocks, tables);
    let y0 = min.origin().y;
    let mut planes: Vec<i64> = (0..rng.below(3))
        .map(|_| y0 + rng.below((N * 32) as u64) as i64)
        .collect();
    planes.sort_unstable();
    planes.dedup();
    let beams = world.colored_beam_tiles(tables, &planes);
    (world, beams, planes)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 10, ..ProptestConfig::default() })]

    #[test]
    fn colored_box_equals_global_solution(seed in any::<u64>(), heavy in any::<bool>(),
                                          sources in any::<bool>(), far in 0usize..4) {
        let (registry, blocks) = test_registry_full(heavy, true, sources);
        let tables = LightTables::from_registry(&registry);
        let min = [
            ChunkPos::new(-2, -2, -2),
            ChunkPos::new(1 << 26, -3, -(1 << 26)),
            ChunkPos::new(-(1 << 35), 1 << 35, 7),
            ChunkPos::new(5, -(1 << 50), 1 << 50),
        ][far];
        let (world, beams, planes) = colored_world(seed, min, &blocks, &tables, sources);
        check_colored(&world, &tables, &beams, &planes);
    }
}

/// Fixed colored worlds, so the regular run covers roofs, windows and tinted beams every time.
#[test]
fn fixed_colored_worlds_take_the_colored_path() {
    let mut run = ColoredRun::default();
    for (seed, heavy, sources) in [(51, false, false), (52, true, false), (53, false, true)] {
        let (registry, blocks) = test_registry_full(heavy, true, sources);
        let tables = LightTables::from_registry(&registry);
        let (world, beams, planes) =
            colored_world(seed, ChunkPos::new(-2, -2, -2), &blocks, &tables, sources);
        let r = check_colored(&world, &tables, &beams, &planes);
        run.colored_boxes += r.colored_boxes;
        run.colored_results += r.colored_results;
        run.paths.extend(r.paths);
        if sources {
            assert!(
                beams.iter().any(BeamTile::has_tints),
                "seed {seed}: no tinted beam"
            );
        }
    }
    assert!(run.colored_results > 0, "{run:?}");
    assert!(run.colored_boxes >= run.colored_results, "{run:?}");
}

/// The switch changes nothing where no glass can tint the sky: a registry without filters, and
/// one with filters but none placed, give the same light and paths with it on and off, and no
/// box takes the colored path. Glass in the world with the switch off gives the light of before
/// (white sky), whatever the tiles keep.
#[test]
fn without_glass_the_switch_changes_nothing() {
    for (heavy, filters, sources) in [
        (false, false, false),
        (true, true, false),
        (false, true, true),
    ] {
        let (registry, blocks) = test_registry_full(heavy, filters, sources);
        let tables = LightTables::from_registry(&registry);
        // No filter placed.
        let none = Blocks {
            filters: None,
            blue: None,
            ..blocks
        };
        let mut world = random_world(61, N, ChunkPos::new(-2, -2, -2), &none, &tables);
        world.add_sources(&mut Rng::new(62), &none, &tables);
        let beams = world.colored_beam_tiles(&tables, &[]);
        let plain_beams = world.beam_tiles(&tables, &[]);
        let colored_tiles = world.colored_tiles(&tables);
        let tiles = world.tiles();
        let (mut scratch, mut out, mut base) = (
            LightScratch::new(),
            PaddedLight::default(),
            PaddedLight::default(),
        );
        for cy in 1..N - 1 {
            for cx in 1..N - 1 {
                for cz in 1..N - 1 {
                    let off = world.input_with(&tiles, &plain_beams, &tables, (cx, cy, cz));
                    let want = compute(&off, &mut scratch, &mut base);
                    let mut on = world.input_with(&colored_tiles, &beams, &tables, (cx, cy, cz));
                    on.colored_sky = true;
                    assert_eq!(compute(&on, &mut scratch, &mut out), want);
                    assert_eq!(out, base);
                }
            }
        }
    }
    // Glass placed, switch off: the colored tiles and beam tiles light as the plain ones.
    let (registry, blocks) = test_registry_full(false, true, true);
    let tables = LightTables::from_registry(&registry);
    let (world, beams, planes) =
        colored_world(63, ChunkPos::new(-2, -2, -2), &blocks, &tables, true);
    let plain = world.beam_tiles(&tables, &planes);
    let (colored_tiles, tiles) = (world.colored_tiles(&tables), world.tiles());
    let (mut scratch, mut out, mut base) = (
        LightScratch::new(),
        PaddedLight::default(),
        PaddedLight::default(),
    );
    for cy in 1..N - 1 {
        for cx in 1..N - 1 {
            for cz in 1..N - 1 {
                let want = compute(
                    &world.input_with(&tiles, &plain, &tables, (cx, cy, cz)),
                    &mut scratch,
                    &mut base,
                );
                let off = world.input_with(&colored_tiles, &beams, &tables, (cx, cy, cz));
                assert_eq!(compute(&off, &mut scratch, &mut out), want);
                assert_eq!(out, base);
            }
        }
    }
}

/// The game's dye filters for the hand scenes: red, light blue and black glass, and clear glass.
fn dye_registry() -> (boxel_voxel::BlockRegistry, [boxel_voxel::BlockState; 4]) {
    use boxel_voxel::{BlockCollision, BlockDef, BlockModel, FaceTextures, RenderShape, TextureId};
    let mut b = boxel_demo::blocks::builder();
    let mut add = |name: &str, filter: Option<[u8; 3]>| {
        let s = b
            .add_block(BlockDef {
                name: format!("test:{name}"),
                shape: RenderShape::Translucent,
                model: BlockModel::CUBE,
                collision: BlockCollision::Full,
                emission: [0; 3],
                light_opacity: 0,
                textures: FaceTextures::all(TextureId(0)),
            })
            .unwrap();
        if let Some(f) = filter {
            let block = b.block_by_name(&format!("test:{name}")).unwrap();
            b.set_light_filter(block, f).unwrap();
        }
        s
    };
    let states = [
        add("dye_red", Some([10, 3, 2])),
        add("dye_light_blue", Some([3, 11, 13])),
        add("dye_black", Some([2, 2, 2])),
        add("clear", None),
    ];
    (b.build(), states)
}

/// The 9 tiles of a [`scene`] (by `tile_index`), from its blocks: tops by brute force (open
/// columns `i64::MIN`), tints the filters of opacity 0 above them.
fn scene_tiles(chunks: &[ChunkData], tables: &LightTables) -> Vec<SkyTile> {
    (0..9)
        .map(|ti| {
            let (dx, dz) = (ti / 3, ti % 3);
            let at = |cy: usize, x: u32, y: u32, z: u32| {
                chunks[cy * 9 + dx * 3 + dz].get(LocalPos::new(x, y, z))
            };
            let mut tops = vec![i64::MIN; 1024];
            let mut tints = Vec::new();
            for x in 0..32u32 {
                for z in 0..32u32 {
                    let col = ((x << 5) | z) as usize;
                    for y in (-32..64i64).rev() {
                        let (cy, ly) = ((y + 32) as usize / 32, (y.rem_euclid(32)) as u32);
                        let s = at(cy, x, ly, z);
                        if tables.blocks_sky(s) {
                            tops[col] = y;
                            break;
                        }
                        if tables.filters(s) {
                            let filter = tables.light_filter(s);
                            tints.push((x, z, boxel_light::Tint { y, filter }));
                        }
                    }
                }
            }
            SkyTile::from_tops(tops.into_boxed_slice().try_into().unwrap(), i64::MIN)
                .with_tints(tints)
        })
        .collect()
}

/// The colored light of a [`scene`]'s center chunk, checked against the oracle, and with the
/// switch off.
fn colored_light_of(chunks: &[ChunkData], tables: &LightTables) -> (PaddedLight, PaddedLight) {
    let tiles = scene_tiles(chunks, tables);
    let refs: [Option<&ChunkData>; 27] = std::array::from_fn(|i| Some(&chunks[i]));
    let mut input = LightInput {
        origin: ChunkPos::new(0, 0, 0),
        chunks: refs,
        sky: SkyInput::Tiles(std::array::from_fn(|i| &tiles[i])),
        beams: [None; 9],
        tables,
        colored_sky: true,
    };
    let (mut scratch, mut on, mut off) = (
        LightScratch::new(),
        PaddedLight::default(),
        PaddedLight::default(),
    );
    compute(&input, &mut scratch, &mut on);
    let tops: Vec<i64> = (0..96 * 96)
        .map(|i| {
            let (x, z) = (i / 96, i % 96);
            tiles[(x / 32) * 3 + z / 32].top((x % 32) as u32, (z % 32) as u32)
        })
        .collect();
    let oracle = OracleWorld::from_chunks(ChunkPos::new(-1, -1, -1), [3; 3], chunks, tables, tops);
    let want = oracle.padded(&oracle.solve(), input.origin);
    assert_eq!(first_mismatch(&on, &want), None);
    assert_eq!(
        on.to_sky_rgb_vec(),
        oracle.padded(&oracle.solve_colored(), input.origin)
    );
    input.colored_sky = false;
    compute(&input, &mut scratch, &mut off);
    assert_eq!(
        off.to_vec(),
        on.to_vec(),
        "gameplay reads the uncolored sky"
    );
    (on, off)
}

/// ADR 0064: under a roof of the game's red glass over a field, the direct sky is (13, 7, 6),
/// the glass itself included, and white above it; the roof's edge gets white 14 from the open
/// column beside it, fading into the tint; two roofs, red over light blue, give (5, 6, 5); black
/// glass darkens all three channels to 6, which only the colored form can carry next to the
/// uncolored 15; clear glass keeps the light of before. Gameplay reads 15 under every roof.
#[test]
fn a_glass_roof_tints_the_sky_under_it() {
    let (registry, [red, blue, black, clear]) = dye_registry();
    let tables = LightTables::from_registry(&registry);
    let (_, b) = test_registry(false);
    assert_eq!(registry.by_name("boxel:stone"), Some(b.stone));
    // A stone ground below y = 0, a roof at y = 20 over the center chunk column, a second roof at
    // y = 19 when `under` is some.
    let roofed = |roof, under: Option<boxel_voxel::BlockState>| {
        scene(&b, |x, y, z| {
            let over = (0..32).contains(&x) && (0..32).contains(&z);
            match y {
                _ if y < 0 => b.stone,
                20 if over => roof,
                19 if over => under.unwrap_or(b.air),
                _ => b.air,
            }
        })
    };
    let (light, off) = colored_light_of(&roofed(red, None), &tables);
    assert!(light.is_colored());
    assert_eq!(light.sky_rgb(16, 10, 16), [13, 7, 6]);
    assert_eq!(light.sky_rgb(16, 20, 16), [13, 7, 6], "the glass itself");
    assert_eq!(light.sky_rgb(16, 21, 16), [15; 3], "above the roof");
    assert_eq!(
        light.sky_rgb(0, 10, 16),
        [14; 3],
        "the edge, from the open column"
    );
    assert_eq!(light.sky_rgb(1, 10, 16), [13, 13, 13]);
    assert_eq!(light.sky_rgb(3, 10, 16), [13, 11, 11]);
    assert_eq!(light.sky_rgb(12, 10, 16), [13, 7, 6]);
    assert_eq!(light.get(16, 10, 16), LightValue::FULL_SKY, "gameplay: 15");
    assert_eq!(off.sky_rgb(16, 10, 16), [15; 3], "the switch off: white");

    let (light, _) = colored_light_of(&roofed(red, Some(blue)), &tables);
    assert_eq!(light.sky_rgb(16, 10, 16), [5, 6, 5]);
    assert_eq!(light.sky_rgb(16, 19, 16), [5, 6, 5]);

    let (light, _) = colored_light_of(&roofed(black, None), &tables);
    assert!(
        light.is_colored(),
        "6 for the picture next to 15 for gameplay"
    );
    assert_eq!(light.sky_rgb(16, 10, 16), [6; 3]);
    assert_eq!(light.get(16, 10, 16).sky(), 15);

    let (light, off) = colored_light_of(&roofed(clear, None), &tables);
    assert!(!light.is_colored());
    assert_eq!(light, off);
}

/// ADR 0064: a sealed stone room with a light blue window facing the open sky, and no sky
/// straight down into it, is lit through the window: (6, 13, 13) in the glass (14 enters), one
/// less per voxel inside; gameplay reads the uncolored 13 behind it. A red pane of the plain
/// test registry (opacity 0, red passing fully) keeps red at the uncolored level.
#[test]
fn a_window_lights_a_closed_room() {
    let (registry, [_, blue, _, _]) = dye_registry();
    let tables = LightTables::from_registry(&registry);
    let (_, b) = test_registry(false);
    let room = |window| {
        scene(&b, |x, y, z| {
            let shell = (4..=27).contains(&x) && (0..=20).contains(&y) && (4..=27).contains(&z);
            let inside = (5..27).contains(&x) && (1..20).contains(&y) && (5..27).contains(&z);
            match (x, y, z) {
                _ if y < 0 => b.stone,
                (4, 8..12, 12..16) => window,
                _ if inside => b.air,
                _ if shell => b.stone,
                _ => b.air,
            }
        })
    };
    let (light, _) = colored_light_of(&room(blue), &tables);
    assert_eq!(light.sky_rgb(3, 10, 14), [15; 3], "outside");
    assert_eq!(light.sky_rgb(4, 10, 14), [6, 13, 13], "the window");
    assert_eq!(light.sky_rgb(5, 10, 14), [5, 12, 12]);
    assert_eq!(light.sky_rgb(8, 10, 14), [2, 9, 9]);
    assert_eq!(light.get(5, 10, 14).sky(), 13, "gameplay");
    assert_eq!(light.sky_rgb(20, 10, 14), [0; 3], "far: dark");

    let (registry, b2) = test_registry_with(false, true);
    let tables = LightTables::from_registry(&registry);
    let [red, _] = b2.filters.unwrap();
    let chunks = scene(&b2, |x, y, z| {
        let shell = (4..=27).contains(&x) && (0..=20).contains(&y) && (4..=27).contains(&z);
        let inside = (5..27).contains(&x) && (1..20).contains(&y) && (5..27).contains(&z);
        match (x, y, z) {
            _ if y < 0 => b2.stone,
            (4, 8..12, 12..16) => red,
            _ if inside => b2.air,
            _ if shell => b2.stone,
            _ => b2.air,
        }
    });
    let (light, _) = colored_light_of(&chunks, &tables);
    let behind = light.get(5, 10, 14).sky();
    let scaled = |f: u8| (boxel_voxel::transmit(14, f) as u8).saturating_sub(1);
    assert_eq!(
        light.sky_rgb(5, 10, 14),
        [behind, scaled(RED_FILTER[1]), scaled(RED_FILTER[2])]
    );
}
