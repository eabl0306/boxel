//! The region generator against ADR 0039 "Cómo se prueba" (worldgen plan sec. 10, tests 1 to 3,
//! 6, 8 and 14), over `test:` content (`support/region.rs`):
//! - exactness: a point query, a chunk grid and the chunk's blocks agree bit for bit at the
//!   origin, `+-2^31`, `2^40` and near `+-MAX_BLOCK_COORD`, at interior and border columns;
//! - determinism: a 6 x 6 x 6 block of chunks in shuffled order on several threads gives the same
//!   fingerprints with the memo cold, warm, evicted and off, and world A then world B on the same
//!   threads gives B's (the build counter);
//! - additivity: an added entry changes only the lattice points it now wins, and removing it gives
//!   the old map and digest back;
//! - seams and slope: heights stay within 3:1 (in 1/256 block, with the kinds' rounding) along
//!   random lines across kind borders, diagonals and triple points included, and the border
//!   columns of two chunks agree with the point query;
//! - bounds: the fast paths (air over the local top, the deep path) give what the column stage
//!   gives, nothing but air lies over the local top, and every ground lies between the local
//!   bounds;
//! - `Uniform` and 1-bit chunks survive; the hand-fused twin equals a one-kind map;
//! - realms: `realm_of` at and around the planes, one realm per chunk, the dimension's union of
//!   ranges, version and query side, and other realms leave a realm's chunks alone;
//! - the version follows every entry, setting and the realm, never the seed, the memo or another
//!   dimension;
//! - every freeze check of the region map and the kinds fails with its ids in the message.
#[path = "support/region.rs"]
mod support;

use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SIZE, ChunkData, ChunkKey, ChunkPos, LocalPos,
    MAX_BLOCK_COORD, Paletted,
};
use boxel_worldgen::field::{Slope, Span};
use boxel_worldgen::lod::LodSource;
use boxel_worldgen::noise::splitmix64;
use boxel_worldgen::realm::{FluidRule, RealmLod, RealmSky, RealmSpan, RealmSpec, RealmStack};
use boxel_worldgen::region::{
    Decoration, DimensionGenerator, Entry, FlatKind, FloorFill, HEIGHT_ONE, HeightQ, Interval,
    KindForm, KindGrid, KindLimits, NoSurface, RegionGenerator, RegionSettings, RegionSpec,
    RuleEntry, Scope, SelectorRule, TerrainKind, check_rule_entries, clear_worker_memo,
};
use boxel_worldgen::registry::{
    Checks, DigestBuilder, GenEntry, GenId, GenRegistryBuilder, HashParams, ParamsHasher,
};
use boxel_worldgen::{FlatGenerator, WorldGenerator};
use proptest::prelude::*;
use std::sync::Arc;
use support::*;

const SHIFT: u32 = 8;
/// Region cells of the tests: 1024 blocks, so borders are close.
const CELLS: u32 = 10;

fn test_world() -> (TestWorld, RegionGenerator<TestContext>) {
    let world = world(CELLS);
    let g = generator(&world, SEED, RegionSettings::default());
    (world, g)
}

fn key(x: i64, y: i64, z: i64) -> ChunkKey {
    ChunkKey::new(ChunkPos::new(x, y, z))
}

/// The distinct kinds of the 16 lattice points around the lattice cell of chunk column `(cx, cz)`.
fn window_kinds(g: &RegionGenerator<TestContext>, cx: i64, cz: i64) -> Vec<GenId> {
    let s = g.lattice() - 5;
    let (i0, j0) = (cx >> s, cz >> s);
    let mut kinds: Vec<GenId> = (0..16)
        .map(|n| g.lattice_kind(i0 - 1 + n / 4, j0 - 1 + n % 4).clone())
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

/// The first chunk columns from `(cx, cz)` along +x whose window holds at least `n` kinds.
fn border_columns(
    g: &RegionGenerator<TestContext>,
    (cx, cz): (i64, i64),
    n: usize,
) -> Vec<(i64, i64)> {
    let mut found = Vec::new();
    let mut x = cx;
    while found.len() < 4 {
        for dz in 0..64 {
            if window_kinds(g, x, cz + dz * 8).len() >= n {
                found.push((x, cz + dz * 8));
                break;
            }
        }
        x += 8;
        assert!(
            x - cx < 1 << 14,
            "no column with {n} kinds near ({cx}, {cz})"
        );
    }
    found
}

/// The `y` of the top solid (non-air, non-fluid) block of column `(lx, lz)` of `chunk`, if any.
fn top_solid(chunk: &ChunkData, y0: i64, lx: u32, lz: u32, fluid: BlockState) -> Option<i64> {
    (0..CHUNK_SIZE as u32).rev().find_map(|ly| {
        let s = chunk.get(LocalPos::new(lx, ly, lz));
        (s != BlockState::AIR && s != fluid).then_some(y0 + i64::from(ly))
    })
}

// ---- Exactness --------------------------------------------------------------------------------

#[test]
fn the_test_world_opens_with_its_digest() {
    let (_, g) = test_world();
    let lines: Vec<String> = g
        .digest()
        .entries()
        .iter()
        .map(|e| format!("{} {}", e.registry, e.id))
        .collect();
    assert_eq!(
        lines,
        [
            "context test:surface",
            "fill_stage test:fill",
            "realm test:surface",
            "region test:surface",
            "selector_rule test:basins",
            "selector_rule test:land",
            "surface_stage test:surface",
            "terrain_kind test:basin",
            "terrain_kind test:hills",
            "terrain_kind test:plains",
        ]
    );
    assert_eq!(g.version(), g.digest().version());
    assert_eq!(
        g.kinds().map(GenId::as_str).collect::<Vec<_>>(),
        ["test:basin", "test:hills", "test:plains"]
    );
}

#[test]
fn points_chunks_and_blocks_agree_far_from_the_origin() {
    let (_, g) = test_world();
    let registry = registry();
    let fluid = registry.by_name("test:fluid").unwrap();
    let far = MAX_BLOCK_COORD - (1 << 14);
    let mut interiors = 0;
    let mut borders = 0;
    for origin in [0, 1 << 31, -(1 << 31), 1 << 40, far, -far] {
        // Searches go toward the origin, so they stay inside the world.
        let (c, s) = (origin >> 5, if origin > 0 { -1 } else { 1 });
        let mut columns = border_columns(&g, (c - (2048 * i64::from(origin > 0)), -c), 2);
        // And interior ones: away from every border.
        columns.extend(
            (0..256)
                .map(|n| (c + s * n * 3, -c - s * n * 5))
                .filter(|&(x, z)| window_kinds(&g, x, z).len() == 1)
                .take(2),
        );
        for (cx, cz) in columns {
            if window_kinds(&g, cx, cz).len() == 1 {
                interiors += 1;
            } else {
                borders += 1;
            }
            let mut grid = [0; CHUNK_AREA];
            g.chunk_heights(cx, cz, &mut grid);
            for (col, &h) in grid.iter().enumerate() {
                let (x, z) = ((cx << 5) + (col >> 5) as i64, (cz << 5) + (col & 31) as i64);
                let info = g.column_info(x, z);
                assert_eq!(info.height, h, "column ({x}, {z})");
                assert_eq!(info.ground, i64::from(h >> 8));
            }
            // The blocks: the chunk holding a column's ground has its top solid block there.
            for col in [0usize, 31, 32 * 17 + 5, 1023] {
                let (lx, lz) = ((col >> 5) as u32, (col & 31) as u32);
                let ground = i64::from(grid[col] >> 8);
                let chunk = g.generate(key(cx, ground >> 5, cz), &registry);
                assert_eq!(chunk, g.generate_full(key(cx, ground >> 5, cz), &registry));
                let top = top_solid(&chunk, (ground >> 5) << 5, lx, lz, fluid);
                assert_eq!(top, Some(ground), "chunk column ({cx}, {cz}) column {col}");
            }
        }
    }
    assert!(
        interiors > 0 && borders >= 6,
        "{interiors} interior, {borders} border columns"
    );
}

// ---- Determinism ------------------------------------------------------------------------------

/// The 6 x 6 x 6 chunks from `(cx, cy, cz)`.
fn block_of_chunks(cx: i64, cy: i64, cz: i64) -> Vec<ChunkKey> {
    let mut keys = Vec::new();
    for x in 0..6 {
        for y in cy..cy + 6 {
            for z in 0..6 {
                keys.push(key(cx + x, y, cz + z));
            }
        }
    }
    keys
}

/// `keys` shuffled by `seed` (Fisher-Yates with `SplitMix64`).
fn shuffled(keys: &[ChunkKey], seed: u64) -> Vec<ChunkKey> {
    let mut keys = keys.to_vec();
    let mut state = seed;
    for i in (1..keys.len()).rev() {
        state = splitmix64(state);
        keys.swap(i, (state % (i as u64 + 1)) as usize);
    }
    keys
}

/// The fingerprint of every key, generated on `threads` threads in `order`; `before` runs on each
/// thread before each chunk (to clear or to evict its memo).
fn fingerprints(
    g: &dyn WorldGenerator,
    order: &[ChunkKey],
    threads: usize,
    before: &(dyn Fn(ChunkKey) + Sync),
) -> Vec<(ChunkKey, u64)> {
    let registry = registry();
    let mut all: Vec<(ChunkKey, u64)> = std::thread::scope(|s| {
        let handles: Vec<_> = order
            .chunks(order.len().div_ceil(threads))
            .map(|part| {
                let registry = &registry;
                s.spawn(move || {
                    part.iter()
                        .map(|&k| {
                            before(k);
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
}

#[test]
fn chunks_are_the_same_in_any_order_on_any_thread_with_any_memo() {
    let (world, g) = test_world();
    let (cx, cz) = border_columns(&g, (0, 0), 2)[0];
    // Across the ground of the border column.
    let ground = g.column_info(cx << 5, cz << 5).ground;
    let keys = block_of_chunks(cx - 2, (ground >> 5) - 3, cz - 2);
    let off = generator(
        &world,
        SEED,
        RegionSettings {
            memo: false,
            ..RegionSettings::default()
        },
    );
    let reference = fingerprints(&off, &keys, 1, &|_| {});
    // Not all one chunk: the block crosses the ground and a border.
    let distinct: std::collections::BTreeSet<u64> = reference.iter().map(|&(_, f)| f).collect();
    assert!(distinct.len() > 20, "{} distinct chunks", distinct.len());
    // A chunk whose window maps to the same memo slots, 32 lattice cells away.
    let far = 32 << (SHIFT - 5);
    let evict = |k: ChunkKey| {
        let _ = g.generate(key(k.pos.x + far, k.pos.y, k.pos.z + far), &registry());
    };
    for (n, threads) in [1, 3, 4].into_iter().enumerate() {
        let order = shuffled(&keys, n as u64);
        assert_eq!(
            fingerprints(&g, &order, threads, &|_| clear_worker_memo()),
            reference,
            "cold"
        );
        assert_eq!(
            fingerprints(&g, &order, threads, &|_| {}),
            reference,
            "warm"
        );
        assert_eq!(
            fingerprints(&g, &order, threads, &evict),
            reference,
            "evicted"
        );
        assert_eq!(
            fingerprints(&off, &order, threads, &|_| {}),
            reference,
            "off"
        );
    }
}

#[test]
fn world_b_after_world_a_on_the_same_threads_is_world_b() {
    let world = world(CELLS);
    let a = generator(&world, SEED, RegionSettings::default());
    let b = generator(&world, SEED ^ 0xb, RegionSettings::default());
    let b_off = generator(
        &world,
        SEED ^ 0xb,
        RegionSettings {
            memo: false,
            ..RegionSettings::default()
        },
    );
    let keys = block_of_chunks(10, -2, 10);
    let fresh = fingerprints(&b_off, &keys, 1, &|_| {});
    // The same threads: world A fills their memos, then world B reads them.
    let registry = registry();
    let after: Vec<(ChunkKey, u64)> = std::thread::scope(|s| {
        s.spawn(|| {
            for &k in &keys {
                let _ = a.generate(k, &registry);
            }
            let mut v: Vec<_> = keys
                .iter()
                .map(|&k| (k, fingerprint(&b.generate(k, &registry))))
                .collect();
            v.sort();
            v
        })
        .join()
        .expect("a worker")
    });
    assert_eq!(after, fresh);
    assert_ne!(
        fingerprints(&a, &keys, 1, &|_| {}),
        fresh,
        "the two worlds differ"
    );
}

// ---- Additivity -------------------------------------------------------------------------------

/// The kinds of lattice points `0..24` x `0..24` around `(i, j)`.
fn lattice_map(g: &RegionGenerator<TestContext>, (i, j): (i64, i64)) -> Vec<GenId> {
    let mut out = Vec::new();
    for a in 0..24 {
        for b in 0..24 {
            out.push(g.lattice_kind(i + a * 2, j + b * 2).clone());
        }
    }
    out
}

fn with_entry(entry: Option<(&str, u8)>) -> TestWorld {
    let entries = entry
        .map(|(kind, weight)| {
            (
                "test:mod_entry",
                RuleEntry {
                    rule: id("test:land"),
                    entry: Entry::new(id(kind), weight),
                    version: 1,
                },
            )
        })
        .into_iter()
        .collect();
    world_of(
        vec![("test:surface", realm())],
        rules(CELLS),
        entries,
        kind_entries(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, ..ProptestConfig::default() })]

    #[test]
    fn an_added_entry_changes_only_the_cells_it_wins(seed in any::<u64>(), weight in 1u8..=64, at in -1000i64..1000) {
        let base = with_entry(None);
        let added = with_entry(Some(("test:basin", weight)));
        let (g0, g1) = (generator(&base, seed, RegionSettings::default()), generator(&added, seed, RegionSettings::default()));
        let origin = (at * 97, -at * 61);
        let (before, after) = (lattice_map(&g0, origin), lattice_map(&g1, origin));
        let basin = id("test:basin");
        let mut won = 0;
        for (b, a) in before.iter().zip(&after) {
            prop_assert!(a == b || *a == basin, "{} became {}", b, a);
            won += usize::from(a != b);
        }
        // Removing it gives the old map and the old digest back, bit for bit.
        let removed = generator(&with_entry(None), seed, RegionSettings::default());
        prop_assert_eq!(lattice_map(&removed, origin), before);
        prop_assert_eq!(removed.digest(), g0.digest());
        prop_assert!(g1.digest().entries().iter().any(|e| e.id.as_str() == "test:mod_entry"));
        let _ = won;
    }
}

#[test]
fn an_added_entry_wins_about_its_share() {
    // A weight of 5 against 3 + 2: about half the land cells, over many cells.
    let (g0, g1) = (
        generator(&with_entry(None), SEED, RegionSettings::default()),
        generator(
            &with_entry(Some(("test:basin", 5))),
            SEED,
            RegionSettings::default(),
        ),
    );
    let (mut land, mut won) = (0, 0);
    for n in 0..400i64 {
        // One lattice point per region cell, far apart.
        let (i, j) = (n * 13, n * 29 - 4000);
        let (b, a) = (g0.lattice_kind(i, j), g1.lattice_kind(i, j));
        if b.as_str() != "test:basin" {
            land += 1;
            won += usize::from(a.as_str() == "test:basin");
        }
    }
    let share = won as f64 / land as f64;
    assert!((0.35..0.65).contains(&share), "share {share} of {land}");
}

// ---- Seams and slope --------------------------------------------------------------------------

/// The largest kind jitter of the test world, plus 1 for the blend's floor.
fn jitter(g: &RegionGenerator<TestContext>) -> u64 {
    kinds()
        .iter()
        .map(|(_, k)| k.limits(g.context()).slope.jitter)
        .max()
        .unwrap()
        + 1
}

/// Asserts `|H(p + t d) - H(p + (t + 1) d)| <= 3 |d|` (in 1/256 block, plus `jitter`) along `steps`
/// steps from `p`; returns how many steps crossed a column whose window holds 2 kinds or more.
fn check_line(
    g: &RegionGenerator<TestContext>,
    (x, z): (i64, i64),
    (dx, dz): (i64, i64),
    steps: i64,
    jitter: u64,
) -> usize {
    let len_sq = u128::from((dx * dx + dz * dz) as u64);
    let cap = u128::from(3 * HEIGHT_ONE.unsigned_abs());
    let mut borders = 0;
    let mut last = g.column_info(x, z).height;
    for t in 1..=steps {
        let (px, pz) = (x + t * dx, z + t * dz);
        let h = g.column_info(px, pz).height;
        let d = u128::from(h.abs_diff(last).saturating_sub(jitter as u32));
        assert!(
            d * d <= cap * cap * len_sq,
            "slope at ({px}, {pz}) step ({dx}, {dz}): {last} -> {h}"
        );
        last = h;
        if t % 32 == 0 && window_kinds(g, px >> 5, pz >> 5).len() > 1 {
            borders += 1;
        }
    }
    borders
}

#[test]
fn heights_stay_within_three_to_one_across_borders() {
    let (_, g) = test_world();
    let j = jitter(&g);
    let mut borders = 0;
    let mut triples = 0;
    for (n, &(cx, cz)) in border_columns(&g, (100, -50), 2)
        .iter()
        .chain(&border_columns(&g, (-300, 700), 3))
        .enumerate()
    {
        if window_kinds(&g, cx, cz).len() >= 3 {
            triples += 1;
        }
        let (x, z) = ((cx << 5) - 600, (cz << 5) - 600);
        for (k, d) in [(1, 0), (0, 1), (1, 1), (1, -1), (2, 1), (1, 3), (-3, 2)]
            .into_iter()
            .enumerate()
        {
            let seed = splitmix64((n * 8 + k) as u64);
            let start = (x + (seed % 400) as i64, z + ((seed >> 20) % 400) as i64);
            borders += check_line(&g, start, d, 1300, j);
        }
    }
    assert!(triples > 0, "no triple point");
    assert!(borders > 20, "{borders} border steps");
}

#[test]
fn border_columns_agree_from_either_chunk() {
    let (_, g) = test_world();
    let registry = registry();
    let fluid = registry.by_name("test:fluid").unwrap();
    for (cx, cz) in border_columns(&g, (40, 40), 2) {
        for (ax, az, bx, bz, a_col, b_col) in [
            (cx, cz, cx + 1, cz, (31, 7), (0, 7)),
            (cx, cz, cx, cz + 1, (9, 31), (9, 0)),
        ] {
            for (chunk_x, chunk_z, (lx, lz)) in [(ax, az, a_col), (bx, bz, b_col)] {
                let (x, z) = (
                    (chunk_x << 5) + i64::from(lx),
                    (chunk_z << 5) + i64::from(lz),
                );
                let ground = g.column_info(x, z).ground;
                let chunk = g.generate(key(chunk_x, ground >> 5, chunk_z), &registry);
                assert_eq!(
                    top_solid(&chunk, (ground >> 5) << 5, lx, lz, fluid),
                    Some(ground)
                );
            }
            // The two border columns step at most 3:1 (one block apart).
            let ha = g
                .column_info(
                    (ax << 5) + i64::from(a_col.0),
                    (az << 5) + i64::from(a_col.1),
                )
                .height;
            let hb = g
                .column_info(
                    (bx << 5) + i64::from(b_col.0),
                    (bz << 5) + i64::from(b_col.1),
                )
                .height;
            assert!(u64::from(ha.abs_diff(hb)) <= 3 * 256 + jitter(&g));
        }
    }
}

// ---- Bounds -----------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn fast_paths_equal_the_column_stage(pick in 0usize..4, dx in -12i64..12, dz in -12i64..12, y in -4i64..8) {
        let (_, g) = test_world();
        let registry = registry();
        let (cx, cz) = border_columns(&g, (7, 3), 2)[pick];
        let k = key(cx + dx, y, cz + dz);
        let chunk = g.generate(k, &registry);
        prop_assert_eq!(&chunk, &g.generate_full(k, &registry));
        let bounds = g.local_bounds(k.pos.x, k.pos.z);
        let mut heights = [0; CHUNK_AREA];
        g.chunk_heights(k.pos.x, k.pos.z, &mut heights);
        for &h in &heights {
            let ground = i64::from(h >> 8);
            prop_assert!(bounds.ground_lo <= ground && ground <= bounds.ground_hi);
            prop_assert!(bounds.deep < ground && ground <= bounds.top);
        }
        let y0 = k.pos.y << 5;
        for ly in 0..32u32 {
            if y0 + i64::from(ly) <= bounds.top {
                continue;
            }
            for x in 0..32 {
                for z in 0..32 {
                    prop_assert_eq!(chunk.get(LocalPos::new(x, ly, z)), BlockState::AIR);
                }
            }
        }
    }
}

/// A flat kind at `height` blocks that declares bounds of `bounds` blocks and a range of `range`
/// blocks: right or wrong.
struct Declares {
    height: i64,
    bounds: i64,
    range: i64,
}

impl HashParams for Declares {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.height).write(&self.bounds).write(&self.range);
    }
}
impl GenEntry for Declares {
    fn version(&self) -> u32 {
        1
    }
}
impl TerrainKind<TestContext> for Declares {
    fn limits(&self, _: &TestContext) -> KindLimits {
        let h = Span::point(self.height << 8);
        KindLimits {
            slope: Slope::ZERO,
            relief: h,
            band: h,
        }
    }
    fn range(&self, _: &TestContext) -> Span {
        Span::point(self.range << 8)
    }
    fn bounds(&self, _: &TestContext, _: u64, _: (i64, i64), _: i64) -> Span {
        Span::point(self.bounds << 8)
    }
    fn heights(&self, _: &KindGrid<'_, TestContext>, _: &[bool], out: &mut [HeightQ]) {
        out.fill((self.height << 8) as HeightQ);
    }
}

/// The generator of a map whose one kind is `kind`.
fn declares(kind: Declares) -> RegionGenerator<TestContext> {
    let world = world_of(
        vec![("test:surface", realm())],
        vec![(
            "test:land",
            rule(
                0,
                Vec::new(),
                Scope::CELL,
                vec![Entry::new(id("test:k"), 1)],
            ),
        )],
        Vec::new(),
        vec![(
            "test:k",
            Arc::new(kind) as Arc<dyn TerrainKind<TestContext>>,
        )],
    );
    generator(&world, SEED, RegionSettings::default())
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

#[test]
#[cfg(debug_assertions)]
fn the_span_check_catches_a_range_or_bound_under_the_heights() {
    let registry = registry();
    let right = declares(Declares {
        height: 100,
        bounds: 100,
        range: 100,
    });
    assert_eq!(
        panic_of(|| drop(right.generate(key(0, 3, 0), &registry))),
        None
    );
    assert_eq!(panic_of(|| drop(right.column_info(5, 5))), None);
    // Right bounds, so every local bound holds, but the realm's vertical range comes from the
    // range: the world would never ask for the chunks of the ground, and leave them air.
    let low_range = declares(Declares {
        height: 100,
        bounds: 100,
        range: 0,
    });
    assert_eq!(*low_range.vertical_chunks().end(), 0);
    let message = panic_of(|| drop(low_range.generate(key(0, 3, 0), &registry))).expect("caught");
    assert!(message.contains("vertical range"), "{message}");
    // Bounds under the heights: the chunk of the ground is air over the local top, so only the
    // chunks the bounds cross ran the check. The query side computes the ground: it checks too.
    let low_bounds = declares(Declares {
        height: 100,
        bounds: 0,
        range: 100,
    });
    assert_eq!(
        low_bounds.generate(key(0, 3, 0), &registry).as_uniform(),
        Some(BlockState::AIR)
    );
    let message = panic_of(|| drop(low_bounds.column_info(5, 5))).expect("caught");
    assert!(message.contains("span check"), "{message}");
}

// ---- Uniform and 1-bit chunks -----------------------------------------------------------------

/// A world of `FlatKind`s under `()`: kinds at `heights` blocks, picked by region cells of
/// `2^cells`, with no fluid and no surface.
fn flat_world(heights: &[i32], cells: u32) -> RegionGenerator<()> {
    let mut checks = Checks::new();
    let mut realms = GenRegistryBuilder::new("realm");
    realms.register(
        id("test:flat"),
        RealmSpec {
            fluid: FluidRule::None,
            ..realm()
        },
    );
    let names: Vec<String> = (0..heights.len())
        .map(|n| format!("test:flat_{n}"))
        .collect();
    let mut rules = GenRegistryBuilder::new("selector_rule");
    rules.register(
        id("test:all"),
        SelectorRule {
            realm: id("test:flat"),
            priority: 0,
            condition: Vec::new(),
            scope: Scope::Cell(cells),
            entries: names.iter().map(|n| Entry::new(id(n), 1)).collect(),
            version: 1,
        },
    );
    let mut kinds = GenRegistryBuilder::new("terrain_kind");
    for (name, &h) in names.iter().zip(heights) {
        kinds.register(
            id(name),
            Arc::new(FlatKind::blocks(h)) as Arc<dyn TerrainKind<()>>,
        );
    }
    let (realms, rules, entries, kinds) = (
        checks.freeze(realms).unwrap(),
        checks.freeze(rules).unwrap(),
        checks
            .freeze(GenRegistryBuilder::<RuleEntry>::new("selector_entry"))
            .unwrap(),
        checks.freeze(kinds).unwrap(),
    );
    let g = RegionGenerator::build(
        RegionSpec {
            seed: SEED,
            dimension: &id("test:overworld"),
            realm: &id("test:flat"),
            realms: &realms,
            rules: &rules,
            entries: &entries,
            kinds: &kinds,
            decoration: Decoration::new(
                (id("test:fill"), Arc::new(FloorFill)),
                (id("test:none"), Arc::new(NoSurface)),
            ),
            settings: RegionSettings::default(),
        },
        |_| (),
        &mut checks,
    );
    checks.finish().expect("opens");
    g.unwrap()
}

/// One `FlatKind` at `height` blocks.
fn flat_generator(height: i32) -> RegionGenerator<()> {
    flat_world(&[height], CELLS)
}

#[test]
fn the_blend_alone_stays_under_the_cap() {
    // Two flat kinds 1600 blocks apart: 1600 sqrt(2) / 768 = 2.95 passes the check, and the
    // blend is the whole slope. Along an axis it reaches 1600 / 768 = 2.08, more on diagonals
    // near corners; never over 3 (plus 1 / 256 for the floor).
    let g = flat_world(&[0, 1600], 8);
    let cap = u128::from(3 * HEIGHT_ONE.unsigned_abs());
    let mut steepest: f64 = 0.0;
    for n in 0..60i64 {
        let seed = splitmix64(n as u64);
        let (x, z) = (
            (seed % 20_000) as i64 - 10_000,
            ((seed >> 24) % 20_000) as i64 - 10_000,
        );
        let (dx, dz) = [(1, 0), (0, 1), (1, 1), (1, -1), (2, 1), (1, 2), (3, -1)][n as usize % 7];
        let len_sq = u128::from((dx * dx + dz * dz) as u64);
        let mut last = g.column_info(x, z).height;
        for t in 1..=1500 {
            let h = g.column_info(x + t * dx, z + t * dz).height;
            let d = u128::from(h.abs_diff(last).saturating_sub(1));
            assert!(
                d * d <= cap * cap * len_sq,
                "({}, {}): {last} -> {h}",
                x + t * dx,
                z + t * dz
            );
            steepest = steepest.max(f64::from(h.abs_diff(last)) / 256.0 / (len_sq as f64).sqrt());
            last = h;
        }
    }
    assert!(steepest > 2.0, "the blend reached only {steepest}");
    // 1700 blocks apart fails the check.
    let mut checks = Checks::new();
    let limits = |h: i64| KindLimits {
        slope: Slope::ZERO,
        relief: Span::point(h << 8),
        band: Span::point(h << 8),
    };
    let kinds = [
        (id("test:a"), KindForm::Height, limits(0)),
        (id("test:b"), KindForm::Height, limits(1700)),
    ];
    boxel_worldgen::region::check_kinds(&mut checks, &id("test:flat"), &kinds, 8);
    assert_eq!(checks.problems().len(), 1);
}

#[test]
fn uniform_and_one_bit_chunks_survive() {
    let registry = registry();
    let solid = registry.by_name("test:solid").unwrap();
    let flat = flat_generator(10);
    assert_eq!(flat.vertical_chunks(), i64::MIN..=0);
    assert_eq!(
        flat.generate(key(3, 1, -2), &registry).as_uniform(),
        Some(BlockState::AIR)
    );
    assert_eq!(
        flat.generate(key(3, -1, -2), &registry).as_uniform(),
        Some(solid)
    );
    assert_eq!(
        flat.generate(key(3, i64::MIN >> 6, -2), &registry)
            .as_uniform(),
        Some(solid)
    );
    // The ground's chunk: stone and air only, so 1 bit per voxel.
    let ground = flat.generate(key(3, 0, -2), &registry);
    assert!(
        matches!(ground.blocks(), Paletted::Packed { bits: 1, .. }),
        "{:?}",
        ground.blocks()
    );
    assert_eq!(ground.get(LocalPos::new(4, 10, 4)), solid);
    assert_eq!(ground.get(LocalPos::new(4, 11, 4)), BlockState::AIR);
    assert_eq!(ground, flat.generate_full(key(3, 0, -2), &registry));

    // The test world: air over the local top and stone under the deep bound stay Uniform.
    let (_, g) = test_world();
    for (cx, cz) in border_columns(&g, (0, 0), 2) {
        let b = g.local_bounds(cx, cz);
        assert_eq!(
            g.generate(key(cx, (b.top >> 5) + 1, cz), &registry)
                .as_uniform(),
            Some(BlockState::AIR)
        );
        assert_eq!(
            g.generate(key(cx, (b.deep >> 5) - 1, cz), &registry)
                .as_uniform(),
            Some(solid)
        );
    }
}

#[test]
fn the_twin_equals_a_one_kind_map() {
    let registry = registry();
    let (name, hills) = kinds()[1];
    let one_kind = world_of(
        vec![("test:surface", realm())],
        vec![(
            "test:only",
            SelectorRule {
                realm: id("test:surface"),
                priority: 0,
                condition: Vec::new(),
                scope: Scope::CELL,
                entries: vec![Entry::new(id(name), 1)],
                version: 1,
            },
        )],
        Vec::new(),
        kind_entries(),
    );
    let g = generator(&one_kind, SEED, RegionSettings::default());
    let twin = Twin::new(name, hills, SEED, SHIFT);
    for &k in &block_of_chunks(-3, 3, 5) {
        assert_eq!(
            g.generate(k, &registry),
            twin.generate(k, &registry),
            "{k:?}"
        );
    }
    for y in [-8, 6, 9] {
        let k = key(1, y, 1);
        assert_eq!(
            g.generate(k, &registry),
            twin.generate(k, &registry),
            "{k:?}"
        );
    }
}

// ---- Realms -----------------------------------------------------------------------------------

/// A realm generator that fills every chunk with one state.
struct Fill(BlockState, RangeLayers);
type RangeLayers = std::ops::RangeInclusive<i64>;

impl WorldGenerator for Fill {
    fn generate(&self, _: ChunkKey, _: &BlockRegistry) -> ChunkData {
        ChunkData::uniform(self.0)
    }
    fn vertical_chunks(&self) -> RangeLayers {
        self.1.clone()
    }
    fn name(&self) -> &str {
        "fill"
    }
}

fn spec(span: RealmSpan, lod: RealmLod) -> RealmSpec {
    RealmSpec {
        span,
        lod,
        env: id(if lod == RealmLod::Draw {
            "test:env"
        } else {
            "test:dark"
        }),
        ..realm()
    }
}

#[test]
fn a_dimension_dispatches_each_chunk_to_its_realm() {
    let registry = registry();
    let realms = world_of(
        vec![
            ("test:deep", spec(RealmSpan::below(-4096), RealmLod::Off)),
            (
                "test:surface",
                spec(RealmSpan::between(-4096, 12288), RealmLod::Draw),
            ),
            ("test:sky", spec(RealmSpan::above(12288), RealmLod::Off)),
        ],
        rules(CELLS),
        Vec::new(),
        kind_entries(),
    );
    let surface = generator(&realms, SEED, RegionSettings::default());
    let mut checks = Checks::new();
    let stack = RealmStack::build(&mut checks, &id("test:overworld"), {
        let mut v: Vec<_> = realms
            .realms
            .iter()
            .map(|(_, id, s)| (id.clone(), s.clone()))
            .collect();
        v.reverse();
        v
    })
    .expect("a good layout");
    assert_eq!(stack.planes(), [-4096, 12288]);
    for (y, realm) in [
        (i64::MIN, 0),
        (-4097, 0),
        (-4096, 1),
        (0, 1),
        (12287, 1),
        (12288, 2),
        (i64::MAX, 2),
    ] {
        assert_eq!(stack.realm_of(y), realm, "y {y}");
    }
    let mut digest = DigestBuilder::new();
    digest.extend(surface.digest());
    let surface_version = surface.version();
    let deep = BlockState(2);
    let sky = BlockState(3);
    let dim = DimensionGenerator::new(
        id("test:overworld"),
        stack,
        vec![
            Box::new(Fill(deep, i64::MIN..=i64::MAX)),
            Box::new(surface),
            Box::new(Fill(sky, RangeLayers::new(1, 0))),
        ],
        &digest.finish(),
    );
    // One realm per chunk: chunk layers -129 and -128 meet at the plane -4096.
    for (layer, state) in [
        (-129, Some(deep)),
        (-128, None),
        (383, None),
        (384, Some(sky)),
    ] {
        let chunk = dim.generate(key(2, layer, 2), &registry);
        match state {
            Some(s) => assert_eq!(chunk.as_uniform(), Some(s), "layer {layer}"),
            None => assert!(
                chunk.as_uniform().is_none_or(|s| s != deep && s != sky),
                "layer {layer}"
            ),
        }
    }
    assert_eq!(dim.vertical_chunks().start(), &i64::MIN);
    assert!(*dim.vertical_chunks().end() < 383);
    assert_eq!(
        WorldGenerator::version(&dim),
        surface_version,
        "one realm's lines: the same digest"
    );
    assert!(dim.lod().is_none(), "no source yet");
    assert_eq!(dim.realms().len(), 3);
    let q = dim.as_query().expect("a query side");
    assert_eq!(q.realm_at(-4097).as_str(), "test:deep");
    assert_eq!(q.realm_at(0).as_str(), "test:surface");
    assert_eq!(q.environment_at(0, 20000, 0).as_str(), "test:dark");
    assert_eq!(q.environment_at(0, 0, 0).as_str(), "test:env");
    assert_eq!(
        q.height_at(5, 9),
        dim.realm_generator(1).as_query().unwrap().height_at(5, 9)
    );
    assert_eq!(
        q.biome_at(5, -5000, 9),
        None,
        "the deep realm has no query side"
    );
    let info = q.column(100, -7).expect("a column");
    assert_eq!(q.biome_at(100, 0, -7), info.kind);
    assert_eq!(info.realm.as_str(), "test:surface");
    // Other realms around it do not move its terrain: the chunks of the one-realm test world.
    let (_, alone) = test_world();
    for k in [key(3, 3, -4), key(-60, 5, 17), key(9, -2, 9)] {
        assert_eq!(
            dim.generate(k, &registry),
            alone.generate(k, &registry),
            "{k:?}"
        );
    }
}

#[test]
fn a_dimension_refuses_a_realm_generator_in_another_realms_place() {
    // The stack sorts its realms bottom first, whatever order they came in: a caller that builds
    // the realm generators in id order can put them in the wrong places, and the dimension came
    // out silently wrong (a realm generator gives air outside its own span).
    let world = world_of(
        vec![
            ("test:deep", spec(RealmSpan::below(-4096), RealmLod::Off)),
            (
                "test:surface",
                spec(RealmSpan::above(-4096), RealmLod::Draw),
            ),
        ],
        rules(CELLS),
        Vec::new(),
        kind_entries(),
    );
    let stack = || {
        let realms = world
            .realms
            .iter()
            .map(|(_, id, s)| (id.clone(), s.clone()))
            .collect();
        RealmStack::build(&mut Checks::new(), &id("test:overworld"), realms).unwrap()
    };
    let surface = || -> Box<dyn WorldGenerator> {
        Box::new(generator(&world, SEED, RegionSettings::default()))
    };
    let deep = || -> Box<dyn WorldGenerator> { Box::new(Fill(BlockState(2), i64::MIN..=-129)) };
    let digest = DigestBuilder::new().finish();
    let message = panic_of(|| {
        drop(DimensionGenerator::new(
            id("test:overworld"),
            stack(),
            vec![surface(), deep()],
            &digest,
        ));
    })
    .expect("refused");
    assert!(
        message.contains("realm 0 of the stack is test:deep, its generator's is test:surface"),
        "{message}"
    );
    // In place, and a generator without realms anywhere.
    let dim = DimensionGenerator::new(
        id("test:overworld"),
        stack(),
        vec![deep(), surface()],
        &digest,
    );
    assert_eq!(
        dim.realm_generator(1).realms().id(0).as_str(),
        "test:surface"
    );
}

#[test]
fn the_version_follows_every_entry_of_its_dimension() {
    let version = |world: &TestWorld, settings: RegionSettings| {
        WorldGenerator::version(&generator(world, SEED, settings))
    };
    let base = version(&world(CELLS), RegionSettings::default());
    assert_eq!(version(&world(CELLS), RegionSettings::default()), base);
    // The seed is not in it.
    assert_eq!(
        WorldGenerator::version(&generator(
            &world(CELLS),
            SEED ^ 1,
            RegionSettings::default()
        )),
        base
    );
    let mut bumped = rules(CELLS);
    bumped[1].1.version = 2;
    let mut heavier = rules(CELLS);
    heavier[1].1.entries[0].weight = 4;
    let mut steeper = kind_entries();
    steeper[1].1 = Arc::new(Relief::blocks(200, 61, 256, 2));
    let others = [
        (world(CELLS + 1), RegionSettings::default()),
        (
            world(CELLS),
            RegionSettings {
                lattice: SHIFT + 1,
                ..RegionSettings::default()
            },
        ),
        (
            world_of(
                vec![("test:surface", realm())],
                bumped,
                Vec::new(),
                kind_entries(),
            ),
            RegionSettings::default(),
        ),
        (
            world_of(
                vec![("test:surface", realm())],
                heavier,
                Vec::new(),
                kind_entries(),
            ),
            RegionSettings::default(),
        ),
        (
            world_of(
                vec![("test:surface", realm())],
                rules(CELLS),
                Vec::new(),
                steeper,
            ),
            RegionSettings::default(),
        ),
        (
            with_entry(Some(("test:basin", 1))),
            RegionSettings::default(),
        ),
        (
            world_of(
                vec![(
                    "test:surface",
                    RealmSpec {
                        datum: 1,
                        ..realm()
                    },
                )],
                rules(CELLS),
                Vec::new(),
                kind_entries(),
            ),
            RegionSettings::default(),
        ),
    ];
    for (n, (world, settings)) in others.iter().enumerate() {
        assert_ne!(version(world, *settings), base, "change {n}");
    }
    // The memo is not in it either.
    assert_eq!(
        version(
            &world(CELLS),
            RegionSettings {
                memo: false,
                ..RegionSettings::default()
            }
        ),
        base
    );

    // A dimension's version reads its own lines only: another dimension's leave it alone.
    let (_, g) = test_world();
    let one = || -> Vec<Box<dyn WorldGenerator>> {
        vec![Box::new(generator(
            &world(CELLS),
            SEED,
            RegionSettings::default(),
        ))]
    };
    let stack = || {
        RealmStack::build(
            &mut Checks::new(),
            &id("test:overworld"),
            vec![(id("test:surface"), realm())],
        )
        .unwrap()
    };
    let mut digest = DigestBuilder::new();
    digest.extend(g.digest());
    let alone = DimensionGenerator::new(id("test:overworld"), stack(), one(), &digest.finish());
    let mut digest = DigestBuilder::new();
    digest.extend(g.digest());
    let other = RegionGenerator::build(
        RegionSpec {
            seed: SEED,
            dimension: &id("test:nether"),
            realm: &id("test:surface"),
            realms: &world(CELLS).realms,
            rules: &world(CELLS).rules,
            entries: &world(CELLS).entries,
            kinds: &world(CELLS).kinds,
            decoration: decoration(),
            settings: RegionSettings {
                lattice: 9,
                ..RegionSettings::default()
            },
        },
        |s| TestContext::new(s, BASE),
        &mut Checks::new(),
    )
    .unwrap();
    digest.extend(other.digest());
    let beside = DimensionGenerator::new(id("test:overworld"), stack(), one(), &digest.finish());
    assert_eq!(
        WorldGenerator::version(&alone),
        WorldGenerator::version(&beside)
    );
    assert_eq!(WorldGenerator::version(&alone), base);
}

#[test]
fn generators_without_realms_keep_their_defaults() {
    let flat = FlatGenerator::new(vec![("test:solid".into(), 3)]);
    assert_eq!(WorldGenerator::version(&flat), 0);
    assert_eq!(LodSource::version(&flat), WorldGenerator::version(&flat));
    assert!(flat.as_query().is_none());
    let realms = flat.realms();
    assert_eq!(realms.len(), 1);
    let (id, spec) = realms.iter().next().unwrap();
    assert_eq!(
        (id.as_str(), spec.span, spec.sky, spec.lod),
        (
            "boxel:default",
            RealmSpan::ALL,
            RealmSky::Open,
            RealmLod::Draw
        )
    );
    assert_eq!(realms.realm_of(i64::MIN), 0);
}

#[test]
fn a_realm_query_matches_its_chunks() {
    let (_, g) = test_world();
    let registry = registry();
    let fluid = registry.by_name("test:fluid").unwrap();
    let q = g.as_query().unwrap();
    let (mut checked, mut wet) = (0, 0);
    for n in 0..20_000i64 {
        if checked >= 40 && wet >= 3 {
            break;
        }
        let (x, z) = (n * 389 - 7000, n * 211 + 300);
        let info = q.column(x, z).unwrap();
        if checked >= 40 && info.fluid_top.is_none() {
            continue;
        }
        checked += 1;
        assert_eq!(q.height_at(x, z), Some(info.ground));
        assert_eq!(q.biome_at(x, 99, z), info.kind);
        assert!(info.ground_lo <= info.ground && info.ground <= info.top);
        let chunk = g.generate(key(x >> 5, info.ground >> 5, z >> 5), &registry);
        let (lx, lz) = ((x & 31) as u32, (z & 31) as u32);
        assert_eq!(
            top_solid(&chunk, (info.ground >> 5) << 5, lx, lz, fluid),
            Some(info.ground)
        );
        if let Some(top) = info.fluid_top {
            wet += 1;
            assert!(top > info.ground);
            let c = g.generate(key(x >> 5, top >> 5, z >> 5), &registry);
            assert_eq!(c.get(LocalPos::new(lx, (top & 31) as u32, lz)), fluid);
            let over = g.generate(key(x >> 5, (top + 1) >> 5, z >> 5), &registry);
            assert_eq!(
                over.get(LocalPos::new(lx, ((top + 1) & 31) as u32, lz)),
                BlockState::AIR
            );
        }
    }
    assert!(wet >= 3, "{wet} wet columns");
}

// ---- Freeze checks ----------------------------------------------------------------------------

/// A kind with declared limits and a flat height (for the checks only).
struct Declared(KindLimits, KindForm);

impl HashParams for Declared {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0);
    }
}
impl GenEntry for Declared {
    fn version(&self) -> u32 {
        1
    }
}
impl TerrainKind<TestContext> for Declared {
    fn form(&self) -> KindForm {
        self.1
    }
    fn limits(&self, _: &TestContext) -> KindLimits {
        self.0
    }
    fn range(&self, _: &TestContext) -> Span {
        self.0.relief
    }
    fn heights(&self, _: &KindGrid<'_, TestContext>, _: &[bool], out: &mut [HeightQ]) {
        out.fill(self.0.relief.lo as HeightQ);
    }
}

/// A declared kind of `slope` blocks per block (in 1/1000) and a relief of `lo..=hi` blocks.
fn declared(slope_milli: u64, lo: i64, hi: i64) -> Arc<dyn TerrainKind<TestContext>> {
    Arc::new(Declared(
        declared_limits(slope_milli, lo, hi),
        KindForm::Height,
    ))
}

fn open(world: &TestWorld) -> Result<(), String> {
    let mut checks = Checks::new();
    check_rule_entries(&mut checks, &world.rules, &world.entries);
    let g = RegionGenerator::build(
        RegionSpec {
            seed: SEED,
            dimension: &id("test:overworld"),
            realm: &id("test:surface"),
            realms: &world.realms,
            rules: &world.rules,
            entries: &world.entries,
            kinds: &world.kinds,
            decoration: decoration(),
            settings: RegionSettings::default(),
        },
        |s| TestContext::new(s, BASE),
        &mut checks,
    );
    match checks.finish() {
        Ok(()) => {
            assert!(g.is_some());
            Ok(())
        }
        Err(e) => {
            assert!(g.is_none());
            Err(e.to_string())
        }
    }
}

fn rule(
    priority: i32,
    condition: Vec<Interval>,
    scope: Scope,
    entries: Vec<Entry>,
) -> SelectorRule {
    SelectorRule {
        realm: id("test:surface"),
        priority,
        condition,
        scope,
        entries,
        version: 1,
    }
}

#[test]
fn a_bad_region_map_fails_with_every_id() {
    let bad = world_of(
        vec![(
            "test:surface",
            RealmSpec {
                floor: "boxel:air".into(),
                ..realm()
            },
        )],
        vec![
            (
                "test:two_points",
                rule(
                    9,
                    Vec::new(),
                    Scope::Point,
                    vec![
                        Entry::new(id("test:plains"), 1),
                        Entry::new(id("test:hills"), 1),
                    ],
                ),
            ),
            (
                "test:heavy",
                rule(
                    8,
                    Vec::new(),
                    Scope::Cell(7),
                    vec![
                        Entry::new(id("test:plains"), 0),
                        Entry::new(id("test:hills"), 65),
                    ],
                ),
            ),
            (
                "test:twice",
                rule(
                    7,
                    Vec::new(),
                    Scope::CELL,
                    vec![Entry::new(id("test:plains"), 1)],
                ),
            ),
            ("test:empty", rule(6, Vec::new(), Scope::CELL, Vec::new())),
            (
                "test:dry",
                rule(
                    5,
                    vec![Interval::new(id("test:rain"), 0, 9)],
                    Scope::CELL,
                    vec![Entry::new(id("test:missing"), 1)],
                ),
            ),
        ],
        vec![
            (
                "test:again",
                RuleEntry {
                    rule: id("test:twice"),
                    entry: Entry::new(id("test:plains"), 2),
                    version: 1,
                },
            ),
            (
                "test:orphan",
                RuleEntry {
                    rule: id("test:gone"),
                    entry: Entry::new(id("test:plains"), 2),
                    version: 1,
                },
            ),
            (
                "test:typo",
                RuleEntry {
                    rule: id("test:twice"),
                    entry: Entry::new(id("test:plain"), 2),
                    version: 1,
                },
            ),
        ],
        kind_entries(),
    );
    let message = open(&bad).expect_err("refused");
    for needle in [
        "selector rule test:two_points has 2 entries (test:plains and test:hills); a Point rule needs exactly one",
        "selector rule test:heavy gives kind test:plains the weight 0, outside 1 to 64",
        "selector rule test:heavy gives kind test:hills the weight 65",
        "selector rule test:heavy has region cells of 2^7 blocks, outside 2^8",
        "selector rule test:twice lists kind test:plains more than once",
        "selector rule test:empty has 0 entries (); a Cell rule needs at least one",
        "test:dry names context_param test:rain, which is not registered",
        "test:dry names terrain_kind test:missing, which is not registered",
        "test:typo names terrain_kind test:plain, which is not registered",
        "test:orphan names selector_rule test:gone, which is not registered",
        "the region map of realm test:surface has no fallback rule: its last rule test:dry has a condition",
        "realm test:surface of dimension test:overworld has the form Ground and the floor boxel:air",
    ] {
        assert!(message.contains(needle), "{needle:?} not in {message}");
    }
}

#[test]
fn kinds_that_break_the_cap_where_they_meet_are_refused() {
    let world = |kinds: Vec<(&'static str, Arc<dyn TerrainKind<TestContext>>)>| {
        let entries = kinds
            .iter()
            .map(|&(name, _)| Entry::new(id(name), 1))
            .collect();
        world_of(
            vec![("test:surface", realm())],
            vec![("test:land", rule(0, Vec::new(), Scope::CELL, entries))],
            Vec::new(),
            kinds,
        )
    };
    // ADR 0039 sec. 6: every pair passes, the triple point does not.
    let triple = world(vec![
        ("test:a", declared(2500, 90, 110)),
        ("test:b", declared(2500, -110, -90)),
        ("test:c", declared(2750, -10, 10)),
    ]);
    let message = open(&triple).expect_err("the triple breaks the cap");
    assert!(
        message.contains(
            "kinds test:a, test:b and test:c of realm test:surface can meet with a slope of 3.155"
        ),
        "{message}"
    );
    assert!(!message.contains("kinds test:a and"), "{message}");
    // Without the steep one, the pair passes; steeper, the pair fails too.
    assert_eq!(
        open(&world(vec![
            ("test:a", declared(2500, 90, 110)),
            ("test:b", declared(2500, -110, -90))
        ])),
        Ok(())
    );
    let pair = open(&world(vec![
        ("test:a", declared(2700, 90, 110)),
        ("test:b", declared(2500, -110, -90)),
    ]))
    .expect_err("the pair breaks it");
    assert!(
        pair.contains(
            "kinds test:a and test:b of realm test:surface can meet with a slope of 3.105"
        ),
        "{pair}"
    );
    // A volume kind is not built yet; a band relief outside the relief is refused.
    let volume = Declared(declared_limits(0, 0, 1), KindForm::HeightAndVolume);
    let bad_band = Declared(
        KindLimits {
            band: Span::new(0, 1 << 10),
            ..declared_limits(0, 0, 1)
        },
        KindForm::Height,
    );
    let message = open(&world(vec![
        ("test:a", Arc::new(volume)),
        ("test:b", Arc::new(bad_band)),
    ]))
    .expect_err("refused");
    assert!(
        message
            .contains("terrain kind test:a has the form HeightAndVolume, which is not built yet"),
        "{message}"
    );
    assert!(
        message.contains(
            "terrain kind test:b declares a relief within the blend band outside its relief"
        ),
        "{message}"
    );
}

#[test]
fn a_kind_alone_keeps_the_cap() {
    // A kind steeper than 3:1 was refused beside any other kind (each pair holds the steeper
    // slope) but opened alone.
    let world = |kinds: Vec<(&'static str, Arc<dyn TerrainKind<TestContext>>)>| {
        let entries = kinds
            .iter()
            .map(|&(name, _)| Entry::new(id(name), 1))
            .collect();
        world_of(
            vec![("test:surface", realm())],
            vec![("test:land", rule(0, Vec::new(), Scope::CELL, entries))],
            Vec::new(),
            kinds,
        )
    };
    let alone = open(&world(vec![("test:steep", declared(3100, 0, 0))])).expect_err("too steep");
    assert!(
        alone
            .contains("terrain kind test:steep of realm test:surface has a slope of 3.100, over 3"),
        "{alone}"
    );
    // Beside a flat kind it is reported alone, not again with every pair it is in.
    let beside = open(&world(vec![
        ("test:steep", declared(3100, 0, 0)),
        ("test:flat", declared(0, 0, 0)),
    ]))
    .expect_err("too steep");
    assert!(beside.contains("terrain kind test:steep"), "{beside}");
    assert!(!beside.contains("kinds test:"), "{beside}");
    assert_eq!(
        open(&world(vec![("test:steep", declared(3000, 0, 0))])),
        Ok(())
    );
}

#[test]
fn the_slope_check_counts_the_jitter() {
    // A kind's rounding adds to its slope over a step of one block: a slope of 2.9 with a jitter
    // of 52/256 block steps up to 3.1 blocks between neighbor columns.
    let world = |kinds: Vec<(&'static str, KindLimits)>| {
        let entries = kinds
            .iter()
            .map(|&(name, _)| Entry::new(id(name), 1))
            .collect();
        world_of(
            vec![("test:surface", realm())],
            vec![("test:land", rule(0, Vec::new(), Scope::CELL, entries))],
            Vec::new(),
            kinds
                .into_iter()
                .map(|(name, l)| {
                    let kind: Arc<dyn TerrainKind<TestContext>> =
                        Arc::new(Declared(l, KindForm::Height));
                    (name, kind)
                })
                .collect(),
        )
    };
    let jittered = |slope_milli, lo, hi, jitter| {
        let mut l = declared_limits(slope_milli, lo, hi);
        l.slope.jitter = jitter;
        l
    };
    assert_eq!(
        open(&world(vec![("test:a", jittered(2900, 0, 0, 0))])),
        Ok(())
    );
    let alone = open(&world(vec![("test:a", jittered(2900, 0, 0, 52))])).expect_err("over the cap");
    assert!(
        alone.contains("terrain kind test:a of realm test:surface has a slope of 3.103, over 3"),
        "{alone}"
    );
    // A pair at 2.5 with a border relief of 220 blocks (2.905) passes; a jitter of 30/256 on one
    // kind takes it to 3.02.
    let pair = |jitter| {
        open(&world(vec![
            ("test:a", jittered(2500, 90, 110, jitter)),
            ("test:b", jittered(2500, -110, -90, 0)),
        ]))
    };
    assert_eq!(pair(0), Ok(()));
    let message = pair(30).expect_err("over the cap");
    assert!(
        message.contains("kinds test:a and test:b of realm test:surface can meet"),
        "{message}"
    );
}

fn declared_limits(slope_milli: u64, lo: i64, hi: i64) -> KindLimits {
    let relief = Span::new(lo << 8, hi << 8);
    KindLimits {
        slope: Slope {
            per_block: boxel_worldgen::field::Q16::from_raw(slope_milli * 256 * 65536 / 1000),
            jitter: 0,
        },
        relief,
        band: relief,
    }
}

#[test]
fn the_check_and_the_blend_agree_on_the_band() {
    // The check passes for the test kinds, with room: the slope test above is not vacuous.
    let (_, g) = test_world();
    let limits: Vec<KindLimits> = kinds().iter().map(|(_, k)| k.limits(g.context())).collect();
    let steepest = limits
        .iter()
        .map(|l| l.slope.per_block.raw())
        .max()
        .unwrap();
    let relief = boxel_worldgen::region::border_relief(&limits[1], &limits[2]);
    assert!(boxel_worldgen::region::slope_passes(
        steepest, relief, SHIFT
    ));
    let blocks = steepest as f64 / 65536.0 / 256.0 + relief as f64 / 256.0 / 543.06;
    assert!(
        (1.0..3.0).contains(&blocks),
        "the test kinds reach a slope of {blocks}"
    );
}
