//! The feature stages of the region generator (#134, ADR 0039 sec. 9, 10, 12 and 14; "Cómo se
//! prueba" tests 3, 8 and 9), over `test:` content (`support/region.rs`: trees whose canopies
//! cross chunk borders, plants):
//! - the local bounds: the local top counts the features' height per chunk column (none where
//!   nothing grows), every block stays under it, the fast paths are the column stage's, and the
//!   features only add blocks over the ground;
//! - a tree across chunk and layer borders is whole whichever chunk is generated first, on any
//!   thread, with the memo cold or off; the same chunks in any order on any thread with any memo;
//! - the far LOD: a run over the coarse ground drawn as cells (under the fluid too), the exported
//!   harness passing with a carver and a feature stage, and catching a run under the ground or
//!   taller than the features;
//! - the digest line, the order by priority, the block names, duplicates and the declared reach;
//! - the debug checks: a block outside the reach, a column height under the features or over the
//!   declared height.
#[path = "support/region.rs"]
mod support;

use boxel_voxel::{
    BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, DimensionId, LocalPos,
};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::field::Rect;
use boxel_worldgen::lod::{LodNode, LodRequest, LodScratch, LodSource};
use boxel_worldgen::noise::splitmix64;
use boxel_worldgen::region::{
    ChunkColumns, Context, Decoration, Entry, FeatureReach, FeatureStage, FlatKind, FloorFill,
    HEIGHT_ONE, LodReport, NoSurface, RegionGenerator, RegionSettings, RegionSpec, RuleEntry,
    Scope, SelectorRule, TerrainKind, check_lod, clear_worker_memo,
};
use boxel_worldgen::registry::{
    Checks, GenEntry, GenRegistryBuilder, HashParams, Order, ParamsHasher,
};
use std::collections::BTreeSet;
use std::sync::Arc;
use support::*;

/// Region cells of 1024 blocks: kind borders close to each other.
const CELLS: u32 = 10;

/// Trees in one cell of 16 x 16 columns in 12 (a third of the chunk columns have none), a far
/// LOD run of 6.
const GROVES: Groves = Groves::new(12, 6);

/// Plants on one dry column in 9.
const TUFTS: Tufts = Tufts { one_in: 9 };

/// The valleys of `tests/decoration.rs`.
const VALLEYS: Valleys = Valleys {
    period: 4096,
    gap: 1024,
    width: 1024,
    floor: -40 * HEIGHT_ONE,
    depth: 320 * 256,
    divide: true,
};

fn key(x: i64, y: i64, z: i64) -> ChunkKey {
    ChunkKey::new(ChunkPos::new(x, y, z))
}

/// The test world with region cells of `2^cells`, its kinds with their bounds per chunk column.
fn test_world(cells: u32) -> TestWorld {
    world_of(
        vec![("test:surface", realm())],
        rules(cells),
        Vec::new(),
        hooked_entries(),
    )
}

/// The test decoration with the valleys and the holes of `tests/decoration.rs`.
fn carved() -> Decoration<TestContext> {
    decoration()
        .with_modifier(id("test:valleys"), Arc::new(VALLEYS))
        .with_carver(id("test:holes"), Arc::new(Holes::new(5, 6, 3)))
}

/// [`carved`] with the groves and the tufts.
fn featured() -> Decoration<TestContext> {
    carved()
        .with_feature(id("test:groves"), Arc::new(GROVES))
        .with_feature(id("test:tufts"), Arc::new(TUFTS))
}

/// The generator of `world` with `decoration`, or its freeze error.
fn try_open(
    world: &TestWorld,
    decoration: Decoration<TestContext>,
    settings: RegionSettings,
) -> Result<RegionGenerator<TestContext>, String> {
    let mut checks = Checks::new();
    let g = RegionGenerator::build(
        RegionSpec {
            seed: SEED,
            dimension: &id("test:overworld"),
            realm: &id("test:surface"),
            realms: &world.realms,
            rules: &world.rules,
            entries: &world.entries,
            kinds: &world.kinds,
            blocks: &registry(),
            decoration,
            settings,
        },
        |realm_seed| TestContext::new(realm_seed, BASE),
        &mut checks,
    );
    checks.finish().map_err(|e| e.to_string())?;
    Ok(g.expect("no problem"))
}

/// [`try_open`] with the default settings, which must pass.
fn open(world: &TestWorld, decoration: Decoration<TestContext>) -> RegionGenerator<TestContext> {
    try_open(world, decoration, RegionSettings::default()).expect("opens")
}

/// The seed of the stage `name` of realm `test:surface` under the tests' world seed.
fn stage_seed(name: &str) -> u64 {
    id(name).seed(id("test:surface").seed(SEED))
}

/// The columns of chunk column `(cx, cz)`.
fn chunk_rect(cx: i64, cz: i64) -> Rect {
    Rect::new(cx << 5, cz << 5, (cx << 5) + 31, (cz << 5) + 31)
}

/// Chunk columns on a long diagonal: kind borders and interiors, near the valleys and far from
/// them (those of `tests/decoration.rs`, and more).
fn sample_columns() -> Vec<(i64, i64)> {
    (0..20).map(|n| (n * 13 - 50, n * 29 - 70)).collect()
}

/// The panic message of `f`, `None` if it returns.
fn panic_of(f: impl FnOnce()) -> Option<String> {
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).err()?;
    Some(
        err.downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default(),
    )
}

/// Every voxel of a chunk.
fn voxels() -> impl Iterator<Item = LocalPos> {
    (0..32 * 32 * 32u32).map(|i| LocalPos::new(i >> 10, (i >> 5) & 31, i & 31))
}

// ---- The local bounds -------------------------------------------------------------------------

#[test]
fn the_local_top_counts_the_features_and_covers_every_block() {
    let registry = registry();
    let world = test_world(CELLS);
    let g = open(&world, featured());
    let bare = open(&world, carved());
    let (lamp, cutout, plant) = (
        registry.by_name("test:lamp").unwrap(),
        registry.by_name("test:cutout").unwrap(),
        registry.by_name("test:plant").unwrap(),
    );
    let seed = stage_seed("test:groves");
    let (mut treeless, mut grown, mut chunks) = (0, 0, 0);
    let (mut logs, mut leaves, mut plants) = (0, 0, 0);
    for (cx, cz) in sample_columns() {
        let (b, nb) = (g.local_bounds(cx, cz), bare.local_bounds(cx, cz));
        // The features move the top alone: by their height where a tree may reach the chunk
        // column, by the plants' block elsewhere.
        assert_eq!(
            (b.ground_lo, b.ground_hi, b.deep),
            (nb.ground_lo, nb.ground_hi, nb.deep)
        );
        let h = FeatureStage::<TestContext>::column_height(
            &GROVES,
            g.context(),
            seed,
            chunk_rect(cx, cz),
        );
        assert_eq!(
            b.top,
            nb.top.max(b.ground_hi + i64::from(h.max(1))),
            "({cx}, {cz})"
        );
        treeless += usize::from(h == 0);
        grown += usize::from(h == Groves::HEIGHT);
        for y in (b.deep >> 5) - 2..=(b.top >> 5) + 1 {
            let k = key(cx, y, cz);
            let chunk = g.generate(k, &registry);
            assert_eq!(chunk, g.generate_full(k, &registry), "{k:?}");
            chunks += 1;
            if y << 5 > b.top {
                assert_eq!(chunk.as_uniform(), Some(BlockState::AIR), "{k:?}");
            }
            // The features only add blocks, over air.
            let before = bare.generate(k, &registry);
            if chunk == before {
                continue;
            }
            for p in voxels() {
                let (a, s) = (before.get(p), chunk.get(p));
                if a == s {
                    continue;
                }
                assert_eq!(a, BlockState::AIR, "{k:?} {p:?}");
                assert!(s == lamp || s == cutout || s == plant, "{k:?} {p:?}: {s:?}");
                assert!((y << 5) + i64::from(p.y()) <= b.top, "{k:?} {p:?}");
                logs += usize::from(s == lamp);
                leaves += usize::from(s == cutout);
                plants += usize::from(s == plant);
            }
        }
    }
    assert!(
        treeless >= 3 && grown >= 3,
        "{treeless} treeless, {grown} grown"
    );
    assert!(
        logs >= 10 && leaves >= 80 && plants >= 200,
        "{logs} logs, {leaves} leaves, {plants} plants in {chunks} chunks"
    );
}

#[test]
fn a_tree_across_chunk_and_layer_borders_is_whole_from_every_chunk() {
    let registry = registry();
    let world = test_world(CELLS);
    let groves = Groves::new(2, 0);
    let trees = || decoration().with_feature(id("test:groves"), Arc::new(groves));
    let g = open(&world, trees());
    let off = try_open(
        &world,
        trees(),
        RegionSettings {
            memo: false,
            ..RegionSettings::default()
        },
    )
    .expect("opens");
    let bare = open(&world, decoration());
    let seed = stage_seed("test:groves");
    let (lamp, cutout) = (
        registry.by_name("test:lamp").unwrap(),
        registry.by_name("test:cutout").unwrap(),
    );
    // A tree on dry ground whose trunk stands a block from a chunk border along x and z and whose
    // blocks, where they go (over air, inside their own column's reach), lie in 4 chunk columns
    // and 2 layers.
    let edge = |v: i64| !(2..30).contains(&(v & 31));
    let (expected, keys) = (0..1 << 20)
        .map(|n: i64| (n % 1024 - 512, n / 1024 - 512))
        .filter_map(|cell| groves.tree(seed, cell))
        .filter(|&(x, z, _)| edge(x) && edge(z))
        .find_map(|(x, z, trunk)| {
            let info = g.column_info(x, z);
            if info.ground < 0 || info.fluid_top.is_some() {
                return None;
            }
            let mut expected = Vec::new();
            Groves::blocks(x, z, info.ground, trunk, |bx, by, bz, _| {
                let g_col = g.column_info(bx, bz).ground;
                let k = key(bx >> 5, by >> 5, bz >> 5);
                let p = LocalPos::new((bx & 31) as u32, (by & 31) as u32, (bz & 31) as u32);
                let air = bare.generate(k, &registry).get(p) == BlockState::AIR;
                if by > g_col && by <= g_col + i64::from(Groves::HEIGHT) && air {
                    expected.push((k, p));
                }
            });
            let keys: BTreeSet<(i64, i64, i64)> = expected
                .iter()
                .map(|(k, _)| (k.pos.x, k.pos.y, k.pos.z))
                .collect();
            let columns: BTreeSet<(i64, i64)> = keys.iter().map(|&(x, _, z)| (x, z)).collect();
            let layers: BTreeSet<i64> = keys.iter().map(|&(_, y, _)| y).collect();
            (columns.len() == 4 && layers.len() == 2).then(|| {
                let keys: Vec<ChunkKey> = keys.into_iter().map(|(x, y, z)| key(x, y, z)).collect();
                (expected, keys)
            })
        })
        .expect("a tree across the borders");
    // Every block of the tree is there (a neighbor's may stand in its place).
    let reference: Vec<ChunkData> = keys.iter().map(|&k| off.generate(k, &registry)).collect();
    let at = |k: &ChunkKey| keys.iter().position(|q| q == k).expect("its chunk");
    for (k, p) in &expected {
        let state = reference[at(k)].get(*p);
        assert!(state == lamp || state == cutout, "{k:?} {p:?}: {state:?}");
    }
    // Each chunk first, the memo warm after it, on its own thread; and each chunk alone on a
    // thread of its own, the memo cold: the same chunks.
    for first in 0..keys.len() {
        let mut order = keys.clone();
        order.rotate_left(first);
        let chunks: Vec<(ChunkKey, ChunkData)> = std::thread::scope(|s| {
            s.spawn(|| {
                order
                    .iter()
                    .map(|&k| (k, g.generate(k, &registry)))
                    .collect()
            })
            .join()
            .unwrap()
        });
        for (k, chunk) in chunks {
            assert_eq!(chunk, reference[at(&k)], "{k:?} after {:?}", order[0]);
        }
    }
    std::thread::scope(|s| {
        for &k in &keys {
            let (g, registry, reference) = (&g, &registry, &reference);
            s.spawn(move || {
                clear_worker_memo();
                assert_eq!(g.generate(k, registry), reference[at(&k)], "{k:?} cold");
            });
        }
    });
}

#[test]
fn featured_chunks_are_the_same_in_any_order_on_any_thread_with_any_memo() {
    let world = test_world(CELLS);
    let g = open(&world, featured());
    let off = try_open(
        &world,
        featured(),
        RegionSettings {
            memo: false,
            ..RegionSettings::default()
        },
    )
    .expect("opens");
    let bare = open(&world, carved());
    // A chunk column on dry land with trees around.
    let seed = stage_seed("test:groves");
    let (cx, cz) = sample_columns()
        .into_iter()
        .find(|&(cx, cz)| {
            let h = FeatureStage::<TestContext>::column_height(
                &GROVES,
                g.context(),
                seed,
                chunk_rect(cx, cz),
            );
            h > 0 && g.column_info(cx << 5, cz << 5).fluid_top.is_none()
        })
        .expect("a dry column with trees");
    let ground = g.column_info(cx << 5, cz << 5).ground;
    let mut keys = Vec::new();
    for x in 0..6 {
        for y in 0..6 {
            for z in 0..6 {
                keys.push(key(cx - 2 + x, (ground >> 5) - 4 + y, cz - 2 + z));
            }
        }
    }
    let fingerprints = |g: &RegionGenerator<TestContext>, order: &[ChunkKey], threads, clear| {
        let registry = registry();
        let mut all: Vec<(ChunkKey, u64)> = std::thread::scope(|s| {
            let handles: Vec<_> = order
                .chunks(order.len().div_ceil(threads))
                .map(|part| {
                    let registry = &registry;
                    s.spawn(move || {
                        part.iter()
                            .map(|&k| {
                                if clear {
                                    clear_worker_memo();
                                }
                                (k, fingerprint(&g.generate(k, registry)))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().expect("a worker"))
                .collect()
        });
        all.sort();
        all
    };
    let reference = fingerprints(&off, &keys, 1, false);
    let distinct: BTreeSet<u64> = reference.iter().map(|&(_, f)| f).collect();
    assert!(distinct.len() > 60, "{} distinct chunks", distinct.len());
    let unfeatured = fingerprints(&bare, &keys, 1, false);
    let changed = reference
        .iter()
        .zip(&unfeatured)
        .filter(|(a, b)| a.1 != b.1)
        .count();
    assert!(changed >= 20, "{changed} chunks with features");
    for (n, threads) in [1, 3, 4].into_iter().enumerate() {
        let mut order = keys.clone();
        let mut state = n as u64 + 7;
        for i in (1..order.len()).rev() {
            state = splitmix64(state);
            order.swap(i, (state % (i as u64 + 1)) as usize);
        }
        assert_eq!(fingerprints(&g, &order, threads, true), reference, "cold");
        assert_eq!(fingerprints(&g, &order, threads, false), reference, "warm");
        assert_eq!(fingerprints(&off, &order, threads, false), reference, "off");
    }
}

// ---- The far LOD ------------------------------------------------------------------------------

/// A realm of one flat kind at `height` blocks under `()`, with the test realm's sea at y 0, no
/// surface stage and `features`.
fn flat(height: i32, features: Groves) -> RegionGenerator<()> {
    let mut checks = Checks::new();
    let mut realms = GenRegistryBuilder::new("realm");
    realms.register(id("test:surface"), realm());
    let mut rules = GenRegistryBuilder::new("selector_rule");
    rules.register(
        id("test:all"),
        SelectorRule {
            realm: id("test:surface"),
            priority: 0,
            condition: Vec::new(),
            scope: Scope::CELL,
            entries: vec![Entry::new(id("test:flat"), 1)],
            version: 1,
        },
    );
    let mut kinds = GenRegistryBuilder::new("terrain_kind");
    kinds.register(
        id("test:flat"),
        Arc::new(FlatKind::blocks(height)) as Arc<dyn TerrainKind<()>>,
    );
    let (realms, rules, entries, kinds) = (
        checks.freeze(realms).unwrap(),
        checks.freeze(rules).unwrap(),
        checks
            .freeze(GenRegistryBuilder::<RuleEntry>::new("selector_entry"))
            .unwrap(),
        checks.freeze(kinds).unwrap(),
    );
    let decoration = Decoration::new(
        (id("test:fill"), Arc::new(FloorFill)),
        (id("test:none"), Arc::new(NoSurface)),
    )
    .with_feature(id("test:groves"), Arc::new(features));
    let g = RegionGenerator::build(
        RegionSpec {
            seed: SEED,
            dimension: &id("test:overworld"),
            realm: &id("test:surface"),
            realms: &realms,
            rules: &rules,
            entries: &entries,
            kinds: &kinds,
            blocks: &registry(),
            decoration,
            settings: RegionSettings::default(),
        },
        |_| (),
        &mut checks,
    );
    checks.finish().expect("opens");
    g.expect("no problem")
}

/// The cells of cell column `(x, z)` (`0..32` each) of node column `[0, 0]` of `level`, from
/// `y0` up by cells, `n` of them.
fn cells_up(
    g: &RegionGenerator<()>,
    level: u8,
    (x, z): (i32, i32),
    y0: i64,
    n: i64,
) -> Vec<BlockState> {
    let registry = registry();
    let s = 1i64 << level;
    let shift = 5 + u32::from(level);
    let req = LodRequest {
        dim: DimensionId(0),
        level,
        column: [0, 0],
        layers: -4..=4,
    };
    let mut layers = std::collections::HashMap::new();
    let band = g.column(
        &req,
        &registry,
        &mut LodScratch::default(),
        &mut |layer, node| {
            let cells = match node {
                LodNode::Uniform(state) => vec![state; 32],
                LodNode::Cells(cells) => (0..32).map(|y| cells.get(x, y, z)).collect(),
            };
            layers.insert(layer, cells);
        },
    );
    (0..n)
        .map(|k| {
            let y = y0 + k * s;
            let layer = y >> shift;
            match layers.get(&layer) {
                Some(cells) => cells[((y - (layer << shift)) / s) as usize],
                None => band
                    .uniform_at(layer)
                    .expect("in a layer or out of the band"),
            }
        })
        .collect()
}

#[test]
fn a_run_stands_over_the_coarse_ground_as_cells() {
    let registry = registry();
    let (solid, cutout, fluid) = (
        registry.by_name("test:solid").unwrap(),
        registry.by_name("test:cutout").unwrap(),
        registry.by_name("test:fluid").unwrap(),
    );
    let air = BlockState::AIR;
    // Dry ground at y 40: the run of 6 is y 41 to 46.
    let dry = flat(40, Groves::new(4, 6));
    assert_eq!(
        cells_up(&dry, 1, (5, 7), 36, 7),
        [solid, solid, cutout, cutout, cutout, cutout, air],
        "level 1, from y 36"
    );
    assert_eq!(
        cells_up(&dry, 4, (5, 7), 16, 3),
        [solid, cutout, air],
        "level 4, from y 16"
    );
    // Without a run: the ground alone, its top cell y 40 and 41.
    let bare = flat(40, Groves::new(4, 0));
    assert_eq!(cells_up(&bare, 1, (5, 7), 38, 3), [solid, solid, air]);
    // A sea floor at y -10 with a run on wet cells: the run up to y -4, the sea over it.
    let wet = flat(
        -10,
        Groves {
            wet_runs: true,
            ..Groves::new(4, 6)
        },
    );
    assert_eq!(
        cells_up(&wet, 1, (5, 7), -12, 7),
        [solid, cutout, cutout, cutout, fluid, fluid, fluid],
        "level 1, from y -12"
    );
    // A dry run is not given on wet cells.
    let dry_runs = flat(-10, Groves::new(4, 6));
    assert_eq!(
        cells_up(&dry_runs, 1, (5, 7), -12, 3),
        [solid, fluid, fluid]
    );
}

/// The test world on region cells of 512 blocks with `decoration`.
fn lod_world(decoration: Decoration<TestContext>) -> RegionGenerator<TestContext> {
    generator_with(&world(9), SEED, RegionSettings::default(), decoration)
}

/// Block columns inside node column `column` of `level`: its corners, middle and a few more.
fn columns_in(level: u8, column: [i64; 2], n: usize, seed: u64) -> Vec<(i64, i64)> {
    let e = 32i64 << level;
    let (x0, z0) = (column[0] * e, column[1] * e);
    let mut out = vec![(x0, z0), (x0 + e - 1, z0 + e - 1), (x0 + e / 2, z0 + e / 2)];
    for k in 0..n as u64 {
        let r = splitmix64(seed ^ k);
        out.push((
            x0 + (r % e as u64) as i64,
            z0 + ((r >> 32) % e as u64) as i64,
        ));
    }
    out
}

#[test]
fn the_harness_passes_with_a_carver_and_a_feature_stage() {
    let registry = registry();
    let decoration = decoration()
        .with_carver(id("test:holes"), Arc::new(Holes::new(5, 6, 3)))
        .with_feature(id("test:groves"), Arc::new(Groves::new(3, 6)))
        .with_feature(id("test:tufts"), Arc::new(TUFTS));
    let g = lod_world(decoration);
    let mut total = LodReport::default();
    let mut featured = 0;
    for level in 1..=4u8 {
        for kinds in [1, 2, 3] {
            let node = nodes_with(&g, level, kinds, [3, -5], 1)[0];
            let columns = columns_in(level, node, 6, u64::from(level) * 31 + kinds as u64);
            let r = check_lod(&g, &registry, &columns, level..=level)
                .unwrap_or_else(|e| panic!("level {level}, {kinds} kinds, node {node:?}: {e}"));
            total.cells += r.cells;
            total.dry += r.dry;
            total.runs += r.runs;
            total.borders += r.borders;
            // The real chunks hold the features the far LOD compares without.
            for &(x, z) in &columns {
                let info = g.column_info(x, z);
                let k = key(x >> 5, (info.ground + 3) >> 5, z >> 5);
                featured +=
                    usize::from(g.generate(k, &registry) != g.generate_terrain(k, &registry));
            }
        }
    }
    assert!(total.dry > 50 && total.runs > 50, "{total:?}");
    assert!(total.borders > 0, "{total:?}");
    assert!(featured >= 20, "{featured} chunks with features or caves");
}

#[test]
fn the_harness_catches_a_run_under_the_ground_or_over_the_height() {
    let registry = registry();
    // A sea a few blocks deep along low land (`tests/region_lod.rs`): a wet cell's ground is its
    // highest corner's, and land between its corners may stand over it.
    let kinds: Vec<(&str, Arc<dyn TerrainKind<TestContext>>)> = vec![
        ("test:plains", Arc::new(Relief::blocks(-1, 3, 96, 2))),
        ("test:hills", Arc::new(Relief::blocks(1, 4, 128, 2))),
        ("test:basin", Arc::new(Relief::blocks(-4, 3, 256, 2))),
    ];
    let world = world_of(vec![("test:surface", realm())], rules(9), Vec::new(), kinds);
    let coast = |groves: Groves| {
        generator_with(
            &world,
            SEED,
            RegionSettings::default(),
            decoration().with_feature(id("test:groves"), Arc::new(groves)),
        )
    };
    let columns = [(800, -488), (803, -483)];
    let wet = coast(Groves {
        wet_runs: true,
        ..Groves::new(3, 6)
    });
    let e = check_lod(&wet, &registry, &columns, 3..=3).expect_err("a buried run");
    assert!(e.contains("under the real ground"), "{e}");
    // The runs on dry cells only pass there.
    let dry = coast(Groves::new(3, 6));
    check_lod(&dry, &registry, &columns, 3..=3).expect("dry runs");
    // A run taller than the feature stages reach.
    let tall =
        lod_world(decoration().with_feature(id("test:groves"), Arc::new(Groves::new(3, 13))));
    let columns = columns_in(2, [3, -2], 8, 4);
    let e = check_lod(&tall, &registry, &columns, 2..=2).expect_err("a tall run");
    assert!(e.contains("over the feature stages' height 12"), "{e}");
}

// ---- Registries and order ---------------------------------------------------------------------

/// A feature stage under its own name, priority and block name: `inner`'s blocks, or one block of
/// `block` over every dry ground where it finds air (`inner` `None`).
struct Named {
    inner: Option<Arc<dyn FeatureStage<TestContext>>>,
    priority: i32,
    block: &'static str,
    reach: FeatureReach,
}

impl Named {
    fn caps(priority: i32, block: &'static str) -> Self {
        Self {
            inner: None,
            priority,
            block,
            reach: FeatureReach {
                height: 1,
                apron: 0,
            },
        }
    }
}

impl HashParams for Named {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.block)
            .write(&self.reach.height)
            .write(&self.reach.apron);
    }
}

impl GenEntry for Named {
    fn version(&self) -> u32 {
        3
    }
    fn order(&self) -> Order {
        Order::new(0, self.priority)
    }
}

impl FeatureStage<TestContext> for Named {
    fn reach(&self) -> FeatureReach {
        self.reach
    }
    fn apply(
        &self,
        columns: &ChunkColumns<'_, TestContext>,
        seed: u64,
        registry: &BlockRegistry,
        out: &mut [BlockState],
    ) {
        if let Some(inner) = &self.inner {
            return inner.apply(columns, seed, registry, out);
        }
        let state = registry.by_name(self.block).expect("checked at world open");
        let o = columns.origin;
        for (c, (&g, &f)) in columns.ground.iter().zip(columns.fluid_top).enumerate() {
            let ly = g + 1 - o.y;
            if f == g && (0..32).contains(&ly) && out[((ly as usize) << 10) | c] == BlockState::AIR
            {
                out[((ly as usize) << 10) | c] = state;
            }
        }
    }
    fn block_names(&self) -> Vec<&str> {
        vec![self.block]
    }
}

#[test]
fn feature_stages_have_digest_lines_run_by_priority_and_check_their_reach() {
    let registry = registry();
    let world = test_world(CELLS);
    let with = |features: Vec<(&str, Named)>| {
        let mut d = decoration();
        for (name, f) in features {
            d = d.with_feature(id(name), Arc::new(f));
        }
        try_open(&world, d, RegionSettings::default())
    };
    let a = with(vec![
        ("test:a", Named::caps(0, "test:plant")),
        ("test:b", Named::caps(5, "test:glass")),
    ])
    .unwrap();
    let b = with(vec![
        ("test:b", Named::caps(5, "test:glass")),
        ("test:a", Named::caps(0, "test:plant")),
    ])
    .unwrap();
    assert_eq!(
        a.digest(),
        b.digest(),
        "the order of the decoration does not matter"
    );
    let lines: Vec<String> = a
        .digest()
        .entries()
        .iter()
        .filter(|e| e.registry == "feature_stage")
        .map(|e| format!("{} {}", e.id, e.version))
        .collect();
    assert_eq!(lines, ["test:a 3", "test:b 3"]);
    let plain = open(&world, decoration());
    assert_ne!(
        a.version(),
        plain.version(),
        "a feature stage changes the version"
    );
    // The higher priority runs first and takes the air: glass over every dry ground, then the
    // other way around.
    let swapped = with(vec![
        ("test:a", Named::caps(9, "test:plant")),
        ("test:b", Named::caps(5, "test:glass")),
    ])
    .unwrap();
    assert_ne!(
        a.version(),
        swapped.version(),
        "a new priority is a changed line"
    );
    let (glass, plant) = (
        registry.by_name("test:glass").unwrap(),
        registry.by_name("test:plant").unwrap(),
    );
    let (x, z) = (0..4096)
        .map(|n| (n * 37 % 4000, n * 53 % 4000))
        .find(|&(x, z)| {
            let info = a.column_info(x, z);
            info.fluid_top.is_none() && info.ground & 31 != 31
        })
        .expect("a dry column");
    let ground = a.column_info(x, z).ground;
    let at = |g: &RegionGenerator<TestContext>| {
        let chunk = g.generate(key(x >> 5, (ground + 1) >> 5, z >> 5), &registry);
        chunk.get(LocalPos::new(
            (x & 31) as u32,
            ((ground + 1) & 31) as u32,
            (z & 31) as u32,
        ))
    };
    assert_eq!((at(&a), at(&swapped)), (glass, plant));
    // Refused at world open, with the stage's id: a typo in a block name, a stage given twice, no
    // height, an apron wider than a chunk.
    let e = with(vec![("test:a", Named::caps(0, "test:nope"))])
        .err()
        .expect("refused");
    assert!(e.contains("test:a") && e.contains("test:nope"), "{e}");
    let e = with(vec![
        ("test:a", Named::caps(0, "test:plant")),
        ("test:a", Named::caps(1, "test:plant")),
    ])
    .err()
    .expect("refused");
    assert!(e.contains("feature_stage test:a"), "{e}");
    for (height, apron) in [(0, 0), (4, 33)] {
        let named = Named {
            reach: FeatureReach { height, apron },
            ..Named::caps(0, "test:plant")
        };
        let e = with(vec![("test:wide", named)]).err().expect("refused");
        assert!(
            e.contains("feature stage test:wide")
                && e.contains(&format!(
                    "height of {height} blocks and an apron of {apron}"
                )),
            "{e}"
        );
    }
    // A stage that declares a wider reach than it writes gives the same chunks.
    let narrow = open(
        &world,
        decoration().with_feature(id("test:groves"), Arc::new(GROVES)),
    );
    let wide = Named {
        inner: Some(Arc::new(GROVES)),
        reach: FeatureReach {
            height: 30,
            apron: 32,
        },
        ..Named::caps(0, "test:lamp")
    };
    let wide = with(vec![("test:groves", wide)]).unwrap();
    for (cx, cz) in sample_columns().into_iter().take(6) {
        let ground = narrow.column_info(cx << 5, cz << 5).ground;
        for y in (ground >> 5) - 1..=(ground >> 5) + 1 {
            let k = key(cx, y, cz);
            assert_eq!(
                narrow.generate(k, &registry),
                wide.generate(k, &registry),
                "{k:?}"
            );
        }
    }
}

// ---- The debug checks -------------------------------------------------------------------------

/// A feature stage that writes `test:lamp` at `over` blocks over the ground of every column of
/// its chunk, and declares a height of 1.
struct Writes(i64);

impl HashParams for Writes {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0);
    }
}

impl GenEntry for Writes {
    fn version(&self) -> u32 {
        1
    }
}

impl FeatureStage<TestContext> for Writes {
    fn reach(&self) -> FeatureReach {
        FeatureReach {
            height: 1,
            apron: 0,
        }
    }
    fn apply(
        &self,
        columns: &ChunkColumns<'_, TestContext>,
        _: u64,
        registry: &BlockRegistry,
        out: &mut [BlockState],
    ) {
        let lamp = registry.by_name("test:lamp").unwrap();
        for (c, &g) in columns.ground.iter().enumerate() {
            let ly = g + self.0 - columns.origin.y;
            if (0..32).contains(&ly) {
                out[((ly as usize) << 10) | c] = lamp;
            }
        }
    }
    fn block_names(&self) -> Vec<&str> {
        vec!["test:lamp"]
    }
}

#[test]
#[cfg(debug_assertions)]
fn a_feature_block_outside_the_reach_fails() {
    let registry = registry();
    let world = test_world(CELLS);
    for over in [2, 0] {
        let g = open(
            &world,
            decoration().with_feature(id("test:writes"), Arc::new(Writes(over))),
        );
        // A column whose ground, the block over it and the one over that share a layer.
        let (x, z) = (0..4096)
            .map(|n| (n * 37 % 4000, n * 53 % 4000))
            .find(|&(x, z)| (g.column_info(x, z).ground & 31) < 29)
            .expect("a column");
        let ground = g.column_info(x, z).ground;
        let k = key(x >> 5, ground >> 5, z >> 5);
        let message = panic_of(|| drop(g.generate_full(k, &registry))).expect("caught");
        assert!(
            message.contains("feature stage test:writes") && message.contains("outside its reach"),
            "{over}: {message}"
        );
    }
}

/// [`Groves`] whose column height says `height` everywhere.
struct Says(u32);

impl HashParams for Says {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0);
    }
}

impl GenEntry for Says {
    fn version(&self) -> u32 {
        1
    }
}

impl<Cx: Context> FeatureStage<Cx> for Says {
    fn reach(&self) -> FeatureReach {
        FeatureStage::<Cx>::reach(&Groves::new(2, 0))
    }
    fn column_height(&self, _: &Cx, _: u64, _: Rect) -> u32 {
        self.0
    }
    fn apply(
        &self,
        columns: &ChunkColumns<'_, Cx>,
        seed: u64,
        registry: &BlockRegistry,
        out: &mut [BlockState],
    ) {
        Groves::new(2, 0).apply(columns, seed, registry, out);
    }
    fn block_names(&self) -> Vec<&str> {
        vec!["test:lamp", "test:cutout"]
    }
}

#[test]
#[cfg(debug_assertions)]
fn a_column_height_under_the_features_or_over_the_height_fails() {
    let registry = registry();
    let world = test_world(CELLS);
    // Says none grow: some chunk of a dry column holds a tree over the local top.
    let none = open(
        &world,
        decoration().with_feature(id("test:says"), Arc::new(Says(0))),
    );
    let message = sample_columns()
        .into_iter()
        .flat_map(|(cx, cz)| {
            let b = none.local_bounds(cx, cz);
            (b.ground_lo >> 5..=(b.ground_hi + 12) >> 5).map(move |y| key(cx, y, cz))
        })
        .find_map(|k| panic_of(|| drop(none.generate_full(k, &registry))))
        .expect("a tree over the top");
    assert!(
        message.contains("over the top") && message.contains("feature stage's column height"),
        "{message}"
    );
    // Says more than the declared height.
    let over = open(
        &world,
        decoration().with_feature(id("test:says"), Arc::new(Says(13))),
    );
    let message = panic_of(|| {
        let _ = over.local_bounds(3, 4);
    })
    .expect("caught");
    assert!(
        message.contains("test:says") && message.contains("over its declared height 12"),
        "{message}"
    );
}
