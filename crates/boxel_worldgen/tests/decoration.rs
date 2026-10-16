//! The decoration of the region generator (#133, ADR 0039 sec. 9, 12 and 14; "Cómo se prueba"
//! tests 1, 3, 6 and 8), over `test:` content (`support/region.rs`):
//! - bounds per chunk column: the kinds' `column_bounds` and the shape modifiers' `column_effect`
//!   tighten the local bounds, the heights stay the same, and every block stays under them;
//! - the apron: what a stage reads around its chunk are its neighbor chunks' columns, bit for bit,
//!   across the corner of a lattice cell with 2 kinds, and the point query's; also at `+-2^31`,
//!   `2^40` and near `+-MAX_BLOCK_COORD`, with the widest apron;
//! - carvers: the deep path equals the column stage on stacks of chunks across the deep threshold
//!   with a carver whose rules read the surface over an apron, near and far from the origin; the
//!   same chunks in any order on any thread with the memo cold, warm and off; uniform rock and a
//!   mask give a 1-bit chunk; no mask adds a block or drains the fluid; the digest line, the
//!   order, duplicates, block names and the apron cap; carvers get the lowest ground of their
//!   apron (`apron_lo`) in the column stage and in the deep path;
//! - every debug check: a column bound under the heights, a column bound right over every chunk
//!   column and under the heights over a carver's apron, a column effect under the cut or outside
//!   the modifier's effect, a read past the apron (a carver's, the surface stage's), a carver
//!   whose rules read the surface deeper than its reach (and not one whose masks only spell the
//!   same voxels two ways), a kind or a modifier steeper than it declares.
#[path = "support/region.rs"]
mod support;

use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SIZE, ChunkData, ChunkKey, ChunkPos,
    LocalPos, MAX_BLOCK_COORD, Paletted,
};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::field::{Rect, Slope, Span};
use boxel_worldgen::noise::splitmix64;
use boxel_worldgen::realm::{FluidRule, RealmSpec};
use boxel_worldgen::region::{
    CarveChunk, CarveMask, CarveReach, CarverStage, ChunkColumns, Context, Decoration, Entry,
    FlatKind, FloorFill, HEIGHT_ONE, HeightQ, KindGrid, KindLimits, MAX_APRON, NoSurface,
    RegionGenerator, RegionSettings, RegionSpec, RuleEntry, Scope, SelectorRule, ShapeEffect,
    ShapeModifier, SurfaceReach, SurfaceStage, TerrainKind, block_of, clear_worker_memo,
};
use boxel_worldgen::registry::{
    Checks, GenEntry, GenId, GenRegistryBuilder, HashParams, Order, ParamsHasher,
};
use std::sync::{Arc, Mutex};
use support::*;

/// Region cells of 1024 blocks: kind borders close to each other.
const CELLS: u32 = 10;

/// The valleys of these tests: up to 320 blocks deep, walls of 320 / 1024 (the test kinds keep
/// the 3:1 cap with them), at `z = 0` and `z = 1024` every 4096 blocks, so chunk columns more than
/// 1024 blocks from both are not cut.
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

/// The test world with region cells of `2^CELLS`, its kinds with their bounds per chunk column or
/// not.
fn test_world(hooked: bool) -> TestWorld {
    let kinds = if hooked {
        hooked_entries()
    } else {
        kind_entries()
    };
    world_of(
        vec![("test:surface", realm())],
        rules(CELLS),
        Vec::new(),
        kinds,
    )
}

/// The test decoration with the valleys.
fn with_valleys() -> Decoration<TestContext> {
    decoration().with_modifier(id("test:valleys"), Arc::new(VALLEYS))
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

/// The distinct kinds of the 16 lattice points around the lattice cell of chunk column `(cx, cz)`.
fn window_kinds<Cx: Context>(g: &RegionGenerator<Cx>, cx: i64, cz: i64) -> usize {
    let s = g.lattice() - 5;
    let (i0, j0) = (cx >> s, cz >> s);
    let mut kinds: Vec<GenId> = (0..16)
        .map(|n| g.lattice_kind(i0 - 1 + n / 4, j0 - 1 + n % 4).clone())
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds.len()
}

/// Chunk columns on a long diagonal: kind borders and interiors, near the valleys and far from
/// them.
fn sample_columns() -> Vec<(i64, i64)> {
    (0..14).map(|n| (n * 13 - 50, n * 29 - 70)).collect()
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

/// Asserts that `chunk` (at layer `y`) holds nothing but air over `top`.
fn assert_air_over(chunk: &ChunkData, y: i64, top: i64) {
    let from = (top + 1 - (y << 5)).clamp(0, 32) as u32;
    for ly in from..CHUNK_SIZE as u32 {
        for x in 0..32 {
            for z in 0..32 {
                assert_eq!(
                    chunk.get(LocalPos::new(x, ly, z)),
                    BlockState::AIR,
                    "({x}, {}, {z}) over the top {top}",
                    (y << 5) + i64::from(ly)
                );
            }
        }
    }
}

// ---- Bounds per chunk column ------------------------------------------------------------------

#[test]
fn column_hooks_tighten_the_bounds_and_every_block_stays_under_them() {
    let registry = registry();
    let plain = open(&test_world(false), with_valleys());
    let hooked = open(&test_world(true), with_valleys());
    let (mut tighter, mut uncut, mut borders, mut chunks) = (0, 0, 0, 0);
    for (cx, cz) in sample_columns() {
        let (p, h) = (plain.local_bounds(cx, cz), hooked.local_bounds(cx, cz));
        assert!(
            p.ground_lo <= h.ground_lo && h.ground_hi <= p.ground_hi,
            "({cx}, {cz}): {h:?} against {p:?}"
        );
        assert!(h.top <= p.top && p.deep <= h.deep, "({cx}, {cz})");
        tighter += usize::from(h.ground_hi - h.ground_lo + 16 < p.ground_hi - p.ground_lo);
        borders += usize::from(window_kinds(&hooked, cx, cz) > 1);
        let far = VALLEYS.distance(cz << 5, (cz << 5) + 31) >= VALLEYS.width;
        uncut += usize::from(far);
        // The same heights, inside the tighter bounds.
        let (mut a, mut b) = ([0; CHUNK_AREA], [0; CHUNK_AREA]);
        plain.chunk_heights(cx, cz, &mut a);
        hooked.chunk_heights(cx, cz, &mut b);
        assert_eq!(a, b, "({cx}, {cz})");
        for &q in &b {
            let g = block_of(0, q);
            assert!(h.ground_lo <= g && g <= h.ground_hi && h.deep < g && g <= h.top);
        }
        // Every chunk across the bounds: the fast paths are the column stage's, the same chunks as
        // without the hooks, nothing over the top.
        for y in (h.deep >> 5) - 1..=(h.top >> 5) + 1 {
            let k = key(cx, y, cz);
            let chunk = hooked.generate(k, &registry);
            assert_eq!(chunk, hooked.generate_full(k, &registry), "{k:?}");
            assert_eq!(chunk, plain.generate(k, &registry), "{k:?}");
            assert_air_over(&chunk, y, h.top);
            chunks += 1;
        }
    }
    assert!(tighter >= 8, "{tighter} of 14 chunk columns tighter");
    assert!(
        uncut >= 2 && borders >= 3,
        "{uncut} uncut, {borders} borders"
    );
    assert!(chunks > 60, "{chunks} chunks");
}

#[test]
fn a_column_effect_lets_the_columns_away_from_the_cut_keep_their_bounds() {
    let plain = open(&test_world(true), decoration());
    let carved = open(&test_world(true), with_valleys());
    let deeper = |cz: i64| {
        let (cx, cz) = (7, cz);
        plain.local_bounds(cx, cz).ground_lo - carved.local_bounds(cx, cz).ground_lo
    };
    // Chunk column 50 (z 1600 to 1631) lies 576 blocks from the valley at 1024 and 2465 from
    // 4096: cut. Chunk column 80 (z 2560 to 2591) lies 1536 and 1505 from them: not.
    assert_eq!(deeper(50), 320);
    assert_eq!(deeper(80), 0);
}

// ---- The apron --------------------------------------------------------------------------------

/// What [`Records`] saw of one chunk: its own grounds and fluid tops, and its apron's.
#[derive(Clone, Debug)]
struct Seen {
    origin: BlockPos,
    own: Vec<(i64, i64)>,
    xs: Vec<i64>,
    zs: Vec<i64>,
    ground: Vec<i64>,
    fluid: Vec<i64>,
}

/// A carver that carves nothing and records its chunk's columns and the square of `apron` blocks
/// around them, as [`ChunkColumns::tops`] gives them.
struct Records {
    apron: u32,
    seen: Mutex<Vec<Seen>>,
}

impl HashParams for Records {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.apron);
    }
}

impl GenEntry for Records {
    fn version(&self) -> u32 {
        1
    }
}

impl CarverStage<TestContext> for Records {
    fn reach(&self) -> CarveReach {
        CarveReach {
            surface: 0,
            apron: self.apron,
        }
    }
    fn carve(&self, chunk: &CarveChunk<'_, TestContext>) -> CarveMask {
        if let Some(c) = chunk.columns {
            let (o, a) = (c.origin, i64::from(self.apron));
            assert_eq!(c.apron(), self.apron);
            let xs: Vec<i64> = (o.x - a..o.x + 32 + a).collect();
            let zs: Vec<i64> = (o.z - a..o.z + 32 + a).collect();
            let n = xs.len() * zs.len();
            let (mut ground, mut fluid) = (vec![0; n], vec![0; n]);
            c.tops(&xs, &zs, &mut ground, &mut fluid);
            let own = c.ground.iter().copied().zip(c.fluid_top.iter().copied());
            self.seen.lock().unwrap().push(Seen {
                origin: o,
                own: own.collect(),
                xs,
                zs,
                ground,
                fluid,
            });
        }
        CarveMask::Solid
    }
    fn block_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

#[test]
fn a_stage_reads_its_neighbor_chunks_columns_bit_for_bit() {
    let records = Arc::new(Records {
        apron: 3,
        seen: Mutex::new(Vec::new()),
    });
    let world = test_world(false);
    let g = open(
        &world,
        with_valleys().with_carver(id("test:records"), Arc::clone(&records) as _),
    );
    let registry = registry();
    // Chunk columns at the +x +z corner of their lattice cell (8 x 8 chunk columns), with 2 kinds
    // or more in their window and in the next cell's: their apron crosses into 3 other cells.
    let corners: Vec<(i64, i64)> = (0..4096)
        .map(|n| (8 * (n % 64) - 249, 8 * (n / 64) - 249))
        .filter(|&(cx, cz)| window_kinds(&g, cx, cz) > 1 && window_kinds(&g, cx + 1, cz + 1) > 1)
        .take(3)
        .collect();
    assert_eq!(corners.len(), 3, "corners with 2 kinds");
    for (cx, cz) in corners {
        // The layer of the lowest ground of the 3 x 3 chunk columns: every chunk there holds rock,
        // so the carver runs in all 9.
        let mut lowest = i64::MAX;
        for dx in -1..=1 {
            for dz in -1..=1 {
                let mut h = [0; CHUNK_AREA];
                g.chunk_heights(cx + dx, cz + dz, &mut h);
                lowest = lowest.min(block_of(0, *h.iter().min().unwrap()));
            }
        }
        records.seen.lock().unwrap().clear();
        for dx in -1..=1 {
            for dz in -1..=1 {
                drop(g.generate_full(key(cx + dx, lowest >> 5, cz + dz), &registry));
            }
        }
        let seen = std::mem::take(&mut *records.seen.lock().unwrap());
        assert_eq!(seen.len(), 9);
        let own_of = |x: i64, z: i64| {
            let s = seen
                .iter()
                .find(|s| (s.origin.x >> 5, s.origin.z >> 5) == (x >> 5, z >> 5))
                .expect("a chunk of the 3 x 3");
            s.own[(((x & 31) << 5) | (z & 31)) as usize]
        };
        let center = seen
            .iter()
            .find(|s| (s.origin.x >> 5, s.origin.z >> 5) == (cx, cz))
            .expect("the center");
        let mut outside = 0;
        for (i, &x) in center.xs.iter().enumerate() {
            for (j, &z) in center.zs.iter().enumerate() {
                let n = i * center.zs.len() + j;
                let tops = (center.ground[n], center.fluid[n]);
                // The neighbor chunk's own columns, and the point query.
                assert_eq!(tops, own_of(x, z), "column ({x}, {z})");
                let info = g.column_info(x, z);
                assert_eq!(tops, (info.ground, info.fluid_top.unwrap_or(info.ground)));
                outside += usize::from((x >> 5, z >> 5) != (cx, cz));
            }
        }
        assert_eq!(outside, 38 * 38 - 32 * 32);
        // The neighbors' blocks: their top solid block is the apron's ground.
        let fluid = registry.by_name("test:fluid").unwrap();
        for (x, z) in [
            ((cx << 5) + 32, (cz << 5) + 5),
            ((cx << 5) + 34, (cz << 5) + 34),
            ((cx << 5) + 7, (cz << 5) + 33),
        ] {
            let ground = own_of(x, z).0;
            let chunk = g.generate(key(x >> 5, ground >> 5, z >> 5), &registry);
            let top = (0..32u32).rev().find_map(|ly| {
                let s = chunk.get(LocalPos::new((x & 31) as u32, ly, (z & 31) as u32));
                (s != BlockState::AIR && s != fluid).then_some(((ground >> 5) << 5) + i64::from(ly))
            });
            assert_eq!(top, Some(ground), "({x}, {z})");
        }
    }
}

#[test]
fn the_apron_and_the_deep_path_agree_far_from_the_origin() {
    let records = Arc::new(Records {
        apron: MAX_APRON,
        seen: Mutex::new(Vec::new()),
    });
    let world = test_world(false);
    let g = open(
        &world,
        with_valleys().with_carver(id("test:records"), Arc::clone(&records) as _),
    );
    let holes = carved(true, RegionSettings::default());
    let registry = registry();
    let far = MAX_BLOCK_COORD - (1 << 14);
    for origin in [1 << 31, -(1 << 31), 1 << 40, far, -far] {
        // A chunk column at the +x +z corner of its lattice cell, toward the origin so the chunk
        // columns around it lie in the world: its apron of a whole chunk crosses into 3 cells.
        let c = ((origin >> 5) - origin.signum() * 64) | 7;
        let (cx, cz) = (c, -c | 7);
        records.seen.lock().unwrap().clear();
        let ground = g.column_info(cx << 5, cz << 5).ground;
        drop(g.generate_full(key(cx, ground >> 5, cz), &registry));
        let seen = std::mem::take(&mut *records.seen.lock().unwrap());
        assert_eq!(seen.len(), 1, "origin {origin}");
        // Every column of the apron: its own chunk column's heights, bit for bit.
        let mut heights = std::collections::HashMap::new();
        let s = &seen[0];
        for (i, &x) in s.xs.iter().enumerate() {
            for (j, &z) in s.zs.iter().enumerate() {
                let h = heights.entry((x >> 5, z >> 5)).or_insert_with(|| {
                    let mut h = [0; CHUNK_AREA];
                    g.chunk_heights(x >> 5, z >> 5, &mut h);
                    h
                });
                let ground = block_of(0, h[(((x & 31) << 5) | (z & 31)) as usize]);
                let n = i * s.zs.len() + j;
                assert_eq!(
                    (s.ground[n], s.fluid[n]),
                    (ground, ground.max(0)),
                    "column ({x}, {z})"
                );
            }
        }
        assert_eq!(heights.len(), 9);
        // The carved chunks across the deep bound: the deep path gives the column stage's.
        let b = holes.local_bounds(cx, cz);
        for y in (b.deep >> 5) - 1..=(b.top >> 5) + 1 {
            let k = key(cx, y, cz);
            assert_eq!(
                holes.generate(k, &registry),
                holes.generate_full(k, &registry),
                "{k:?}"
            );
        }
    }
}

// ---- Carvers ----------------------------------------------------------------------------------

/// The test world with the valleys and holes under a roof of 6 over an apron of 3 (as a cave
/// stage with a water shell reads its neighbors).
fn carved(hooked: bool, settings: RegionSettings) -> RegionGenerator<TestContext> {
    try_open(
        &test_world(hooked),
        with_valleys().with_carver(id("test:holes"), Arc::new(Holes::new(5, 6, 3))),
        settings,
    )
    .expect("opens")
}

#[test]
fn the_deep_path_equals_the_column_stage_with_a_carver() {
    let registry = registry();
    let g = carved(true, RegionSettings::default());
    let uncarved = open(&test_world(true), with_valleys());
    let (mut deep, mut deep_carved, mut column_carved) = (0, 0, 0);
    for (cx, cz) in sample_columns() {
        let b = g.local_bounds(cx, cz);
        // The deep bound moves under the roof, over the apron's grounds too.
        assert!(b.deep <= b.ground_lo - 6, "({cx}, {cz}): {b:?}");
        assert!(b.deep < uncarved.local_bounds(cx, cz).deep);
        for y in (b.deep >> 5) - 2..=(b.top >> 5) + 1 {
            let k = key(cx, y, cz);
            let chunk = g.generate(k, &registry);
            assert_eq!(chunk, g.generate_full(k, &registry), "{k:?}");
            assert_air_over(&chunk, y, b.top);
            let carved = chunk != uncarved.generate(k, &registry);
            if (y << 5) + 31 <= b.deep {
                deep += 1;
                deep_carved += usize::from(carved);
                assert!(
                    matches!(chunk.blocks(), Paletted::Packed { bits: 1, .. }),
                    "{k:?}: uniform rock and holes are 1 bit"
                );
            } else {
                column_carved += usize::from(carved);
            }
        }
    }
    assert!(
        deep >= 28 && deep_carved == deep,
        "{deep_carved} of {deep} deep chunks carved"
    );
    assert!(
        column_carved >= 20,
        "{column_carved} chunks carved by the column stage"
    );
}

#[test]
fn carved_chunks_are_the_same_in_any_order_on_any_thread_with_any_memo() {
    let g = carved(true, RegionSettings::default());
    let off = carved(
        true,
        RegionSettings {
            memo: false,
            ..RegionSettings::default()
        },
    );
    let (cx, cz) = sample_columns()[3];
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
    let distinct: std::collections::BTreeSet<u64> = reference.iter().map(|&(_, f)| f).collect();
    assert!(distinct.len() > 60, "{} distinct chunks", distinct.len());
    for (n, threads) in [1, 3, 4].into_iter().enumerate() {
        let mut order = keys.clone();
        let mut state = n as u64;
        for i in (1..order.len()).rev() {
            state = splitmix64(state);
            order.swap(i, (state % (i as u64 + 1)) as usize);
        }
        assert_eq!(fingerprints(&g, &order, threads, true), reference, "cold");
        assert_eq!(fingerprints(&g, &order, threads, false), reference, "warm");
        assert_eq!(fingerprints(&off, &order, threads, false), reference, "off");
    }
}

/// A dry realm of one flat kind at `height` blocks under `()`, on `test:solid`, with
/// `decoration`.
fn flat(height: i32, decoration: Decoration<()>) -> Result<RegionGenerator<()>, String> {
    dry(Arc::new(FlatKind::blocks(height)), decoration)
}

/// A dry realm of the one kind `kind` under `()`, on `test:solid`, with `decoration`.
fn dry(
    kind: Arc<dyn TerrainKind<()>>,
    decoration: Decoration<()>,
) -> Result<RegionGenerator<()>, String> {
    let mut checks = Checks::new();
    let mut realms = GenRegistryBuilder::new("realm");
    realms.register(
        id("test:dry"),
        RealmSpec {
            fluid: FluidRule::None,
            ..realm()
        },
    );
    let mut rules = GenRegistryBuilder::new("selector_rule");
    rules.register(
        id("test:all"),
        SelectorRule {
            realm: id("test:dry"),
            priority: 0,
            condition: Vec::new(),
            scope: Scope::CELL,
            entries: vec![Entry::new(id("test:flat"), 1)],
            version: 1,
        },
    );
    let mut kinds = GenRegistryBuilder::new("terrain_kind");
    kinds.register(id("test:flat"), kind);
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
            realm: &id("test:dry"),
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
    checks.finish().map_err(|e| e.to_string())?;
    Ok(g.expect("no problem"))
}

/// The dry decoration: the floor, no surface.
fn bare() -> Decoration<()> {
    Decoration::new(
        (id("test:fill"), Arc::new(FloorFill)),
        (id("test:none"), Arc::new(NoSurface)),
    )
}

#[test]
fn uniform_rock_and_a_mask_give_a_one_bit_chunk() {
    let registry = registry();
    let solid = registry.by_name("test:solid").unwrap();
    let holes = flat(
        40,
        bare().with_carver(id("test:holes"), Arc::new(Holes::new(3, 6, 0))),
    )
    .expect("opens");
    // Deep: the fill and the mask alone, 1 bit.
    let deep = holes.generate(key(2, -3, 5), &registry);
    assert!(matches!(deep.blocks(), Paletted::Packed { bits: 1, .. }));
    assert_eq!(deep, holes.generate_full(key(2, -3, 5), &registry));
    // The ground's chunk without fluid or surface: the ground's occupancy and the mask, 1 bit,
    // with no hole within the roof.
    let ground = holes.generate(key(2, 1, 5), &registry);
    assert!(matches!(ground.blocks(), Paletted::Packed { bits: 1, .. }));
    for x in 0..32 {
        for z in 0..32 {
            for y in 35..=40 {
                assert_eq!(ground.get(LocalPos::new(x, y - 32, z)), solid);
            }
            assert_eq!(ground.get(LocalPos::new(x, 41 - 32, z)), BlockState::AIR);
        }
    }
    // A carver that never carves leaves the chunk Uniform.
    let none = flat(
        40,
        bare().with_carver(id("test:holes"), Arc::new(Holes::new(u64::MAX, 6, 0))),
    )
    .expect("opens");
    assert_eq!(
        none.generate(key(2, -3, 5), &registry).as_uniform(),
        Some(solid)
    );
}

/// A carver that gives every chunk `mask`.
struct Gives(CarveMask);

impl HashParams for Gives {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&matches!(self.0, CarveMask::Air));
    }
}

impl GenEntry for Gives {
    fn version(&self) -> u32 {
        1
    }
}

impl CarverStage<TestContext> for Gives {
    fn reach(&self) -> CarveReach {
        CarveReach::default()
    }
    fn carve(&self, _: &CarveChunk<'_, TestContext>) -> CarveMask {
        self.0.clone()
    }
    fn block_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

#[test]
fn a_mask_adds_no_block_and_leaves_the_fluid() {
    let registry = registry();
    let fluid = registry.by_name("test:fluid").unwrap();
    let world = test_world(false);
    let plain = open(&world, decoration());
    let none = open(
        &world,
        decoration().with_carver(id("test:none"), Arc::new(Gives(CarveMask::Solid))),
    );
    let all = open(
        &world,
        decoration().with_carver(id("test:all"), Arc::new(Gives(CarveMask::Air))),
    );
    // Columns under the sea (near a basin): the ground's chunk holds rock, the surface and water.
    let wet: Vec<(i64, i64)> = (0..20_000)
        .map(|n| (n * 37 % 9000 - 4500, n * 53 % 9000 - 4500))
        .filter(|&(x, z)| plain.column_info(x, z).fluid_top.is_some())
        .take(4)
        .collect();
    assert_eq!(wet.len(), 4, "basin columns");
    for (x, z) in wet {
        let ground = plain.column_info(x, z).ground;
        for y in [ground >> 5, -1, 0, 1] {
            let k = key(x >> 5, y, z >> 5);
            let before = plain.generate(k, &registry);
            // A mask that carves nothing: the same chunk, so a mask cannot add a block.
            assert_eq!(none.generate(k, &registry), before, "{k:?}");
            // A mask that carves everything: every solid voxel air, the fluid in place.
            let after = all.generate(k, &registry);
            assert_eq!(after, all.generate_full(k, &registry), "{k:?}");
            for i in 0..32 * 32 * 32u32 {
                let p = LocalPos::new(i >> 10, (i >> 5) & 31, i & 31);
                let (b, a) = (before.get(p), after.get(p));
                let expect = if b == fluid { fluid } else { BlockState::AIR };
                assert_eq!(a, expect, "{k:?} {p:?}");
            }
        }
    }
}

#[test]
fn carvers_have_digest_lines_run_by_priority_and_check_their_names() {
    /// [`Holes`] with a priority and a block name.
    struct Named(Holes, i32, &'static str);
    impl HashParams for Named {
        fn hash_params(&self, h: &mut ParamsHasher) {
            h.write(&self.0).write(&self.2);
        }
    }
    impl GenEntry for Named {
        fn version(&self) -> u32 {
            3
        }
        fn order(&self) -> Order {
            Order::new(0, self.1)
        }
    }
    impl CarverStage<TestContext> for Named {
        fn reach(&self) -> CarveReach {
            CarverStage::<TestContext>::reach(&self.0)
        }
        fn carve(&self, chunk: &CarveChunk<'_, TestContext>) -> CarveMask {
            self.0.carve(chunk)
        }
        fn block_names(&self) -> Vec<&str> {
            vec![self.2]
        }
    }
    let world = test_world(false);
    let holes = Holes::new(5, 6, 3);
    let with = |carvers: Vec<(&str, Named)>| {
        let mut d = decoration();
        for (name, c) in carvers {
            d = d.with_carver(id(name), Arc::new(c));
        }
        try_open(&world, d, RegionSettings::default())
    };
    let a = with(vec![
        ("test:a", Named(holes, 0, "test:solid")),
        ("test:b", Named(holes, 5, "test:glass")),
    ])
    .unwrap();
    let b = with(vec![
        ("test:b", Named(holes, 5, "test:glass")),
        ("test:a", Named(holes, 0, "test:solid")),
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
        .filter(|e| e.registry == "carver_stage")
        .map(|e| format!("{} {}", e.id, e.version))
        .collect();
    assert_eq!(lines, ["test:a 3", "test:b 3"]);
    let plain = open(&world, decoration());
    assert_ne!(a.version(), plain.version(), "a carver changes the version");
    // A new priority is a changed line.
    let c = with(vec![
        ("test:a", Named(holes, 1, "test:solid")),
        ("test:b", Named(holes, 5, "test:glass")),
    ])
    .unwrap();
    assert_ne!(a.version(), c.version());
    // Refused at world open, with the carver's id: a typo in a block name, a carver given twice,
    // an apron wider than a chunk.
    let e = with(vec![("test:a", Named(holes, 0, "test:nope"))])
        .err()
        .expect("refused");
    assert!(e.contains("test:a") && e.contains("test:nope"), "{e}");
    let e = with(vec![
        ("test:a", Named(holes, 0, "test:solid")),
        ("test:a", Named(holes, 1, "test:solid")),
    ])
    .err()
    .expect("refused");
    assert!(e.contains("carver_stage test:a"), "{e}");
    let wide = Holes::new(5, 6, 33);
    let e = with(vec![("test:wide", Named(wide, 0, "test:solid"))])
        .err()
        .expect("refused");
    assert!(e.contains("test:wide") && e.contains("apron of 33"), "{e}");
}

// ---- The debug checks -------------------------------------------------------------------------

/// A flat kind at 100 blocks whose bounds per chunk column say 0: under its heights.
struct UnderHook;

impl HashParams for UnderHook {
    fn hash_params(&self, _: &mut ParamsHasher) {}
}

impl GenEntry for UnderHook {
    fn version(&self) -> u32 {
        1
    }
}

impl TerrainKind<TestContext> for UnderHook {
    fn limits(&self, _: &TestContext) -> KindLimits {
        let h = Span::point(100 << 8);
        KindLimits {
            slope: Slope::ZERO,
            relief: h,
            band: h,
        }
    }
    fn range(&self, _: &TestContext) -> Span {
        Span::point(100 << 8)
    }
    fn column_bounds(&self, _: &TestContext, _: u64, _: Rect) -> Option<Span> {
        Some(Span::point(0))
    }
    fn heights(&self, _: &KindGrid<'_, TestContext>, _: &[bool], out: &mut [HeightQ]) {
        out.fill(100 << 8);
    }
}

#[test]
#[cfg(debug_assertions)]
fn a_column_bound_under_the_heights_fails_the_span_check() {
    let registry = registry();
    let world = world_of(
        vec![("test:surface", realm())],
        vec![(
            "test:land",
            SelectorRule {
                realm: id("test:surface"),
                priority: 0,
                condition: Vec::new(),
                scope: Scope::CELL,
                entries: vec![Entry::new(id("test:under"), 1)],
                version: 1,
            },
        )],
        Vec::new(),
        vec![(
            "test:under",
            Arc::new(UnderHook) as Arc<dyn TerrainKind<TestContext>>,
        )],
    );
    let g = open(&world, decoration());
    // The ground's chunk (layer 3) is over the local top: air, without a check. The column stage
    // and the query side compute the ground and catch it.
    assert_eq!(
        g.generate(key(0, 3, 0), &registry).as_uniform(),
        Some(BlockState::AIR)
    );
    let message = panic_of(|| drop(g.generate_full(key(0, 3, 0), &registry))).expect("caught");
    assert!(message.contains("column bounds"), "{message}");
    let message = panic_of(|| drop(g.column_info(5, 5))).expect("caught");
    assert!(message.contains("span check"), "{message}");
}

/// The test valleys with another column effect: `None` keeps theirs, `Some` says it everywhere.
struct Says(Valleys, ShapeEffect);

impl HashParams for Says {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0).write(&self.1);
    }
}

impl GenEntry for Says {
    fn version(&self) -> u32 {
        1
    }
}

impl ShapeModifier<TestContext> for Says {
    fn effect(&self) -> ShapeEffect {
        ShapeModifier::<TestContext>::effect(&self.0)
    }
    fn slope(&self) -> Slope {
        ShapeModifier::<TestContext>::slope(&self.0)
    }
    fn column_effect(&self, _: &TestContext, _: u64, _: Rect) -> ShapeEffect {
        self.1
    }
    fn apply(&self, grid: &KindGrid<'_, TestContext>, heights: &mut [HeightQ]) {
        self.0.apply(grid, heights);
    }
}

#[test]
#[cfg(debug_assertions)]
fn a_column_effect_under_the_cut_or_outside_the_effect_fails() {
    let says = |effect| {
        open(
            &test_world(false),
            decoration().with_modifier(id("test:says"), Arc::new(Says(VALLEYS, effect))),
        )
    };
    // No cut, says the column effect; the valley at z = 0 cuts there.
    let none = says(ShapeEffect::LowerOnly(0));
    let message = panic_of(|| drop(none.column_info(40, 3))).expect("caught");
    assert!(
        message.contains("test:says") && message.contains("column effect"),
        "{message}"
    );
    // A column effect deeper than the declared one: the realm's range would not hold it.
    let deeper = says(ShapeEffect::LowerOnly(VALLEYS.depth + 256));
    let message = panic_of(|| {
        deeper.local_bounds(1, 1);
    })
    .expect("caught");
    assert!(
        message.contains("test:says") && message.contains("outside its declared effect"),
        "{message}"
    );
    let raise = says(ShapeEffect::Raise(256));
    assert!(
        panic_of(|| {
            raise.local_bounds(1, 1);
        })
        .is_some()
    );
}

/// A surface stage that reads the column next to its chunk, with no apron.
struct Peeks;

impl HashParams for Peeks {
    fn hash_params(&self, _: &mut ParamsHasher) {}
}

impl GenEntry for Peeks {
    fn version(&self) -> u32 {
        1
    }
}

impl SurfaceStage<TestContext> for Peeks {
    fn reach(&self) -> SurfaceReach {
        SurfaceReach { depth: 1, rise: 0 }
    }
    fn apply(
        &self,
        columns: &ChunkColumns<'_, TestContext>,
        _: &BlockRegistry,
        _: &mut [BlockState],
    ) {
        let o = columns.origin;
        columns.tops(&[o.x - 1], &[o.z], &mut [0], &mut [0]);
    }
    fn block_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

#[test]
#[cfg(debug_assertions)]
fn a_read_past_the_apron_fails() {
    let registry = registry();
    let world = test_world(false);
    let at = |g: &RegionGenerator<TestContext>| {
        let ground = g.column_info(64, 64).ground;
        key(2, ground >> 5, 2)
    };
    // A carver that reads 3 blocks around its chunk and declares 1.
    let holes = Holes {
        reach: CarveReach {
            surface: 6,
            apron: 1,
        },
        ..Holes::new(5, 6, 3)
    };
    let g = open(
        &world,
        decoration().with_carver(id("test:holes"), Arc::new(holes)),
    );
    let message = panic_of(|| drop(g.generate_full(at(&g), &registry))).expect("caught");
    assert!(
        message.contains("test:holes") && message.contains("past its apron of 1"),
        "{message}"
    );
    // The surface stage has no apron: each column is decided by its own chunk.
    let g = open(
        &world,
        Decoration::new(
            (id("test:fill"), Arc::new(FloorFill)),
            (id("test:peeks"), Arc::new(Peeks)),
        ),
    );
    let message = panic_of(|| drop(g.generate_full(at(&g), &registry))).expect("caught");
    assert!(
        message.contains("the surface stage") && message.contains("past its apron of 0"),
        "{message}"
    );
}

#[test]
#[cfg(debug_assertions)]
fn a_carver_reading_the_surface_past_its_reach_fails() {
    let registry = registry();
    // Every cube a hole under a roof of 6 blocks from the ground at y 35: chunk layer 0 (up to y
    // 31) lies under every ground, and its voxels at y 30 and 31 are within the roof. Declared
    // right, the deep path takes it; declared 0 deep, the column stage sees the difference.
    let right = flat(
        35,
        bare().with_carver(id("test:holes"), Arc::new(Holes::new(1, 6, 0))),
    )
    .expect("opens");
    assert_eq!(
        right.generate(key(0, 0, 0), &registry),
        right.generate_full(key(0, 0, 0), &registry)
    );
    let shallow = Holes {
        reach: CarveReach {
            surface: 0,
            apron: 0,
        },
        ..Holes::new(1, 6, 0)
    };
    let wrong = flat(35, bare().with_carver(id("test:holes"), Arc::new(shallow))).expect("opens");
    let message = panic_of(|| drop(wrong.generate_full(key(0, 0, 0), &registry))).expect("caught");
    assert!(
        message.contains("test:holes") && message.contains("deeper than its reach"),
        "{message}"
    );
}

/// A slope falling toward +x and +z (about 2.2:1) with its bounds per chunk column. Right, they
/// hold every column of `area`; `aligned`, they take `area` for the chunk column of its low
/// corner: right over every chunk column, under the heights over a carver's apron, which reaches
/// past the chunk column into the next ones.
struct Ramp {
    aligned: bool,
}

impl Ramp {
    /// The height of column `(x, z)`.
    const fn height(x: i64, z: i64) -> i64 {
        -(x * 549) - (z * 85)
    }
}

impl HashParams for Ramp {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.aligned);
    }
}

impl GenEntry for Ramp {
    fn version(&self) -> u32 {
        1
    }
}

impl TerrainKind<()> for Ramp {
    fn limits(&self, _: &()) -> KindLimits {
        let range = Span::new(-1 << 21, 1 << 21);
        KindLimits {
            slope: Slope {
                per_block: boxel_worldgen::field::Q16::from_raw(560 << 16),
                jitter: 0,
            },
            relief: range,
            band: range,
        }
    }
    fn range(&self, _: &()) -> Span {
        Span::new(-1 << 21, 1 << 21)
    }
    fn column_bounds(&self, _: &(), _: u64, area: Rect) -> Option<Span> {
        let (x0, z0, x1, z1) = if self.aligned {
            let (x0, z0) = (area.x0 & !31, area.z0 & !31);
            (x0, z0, x0 + 31, z0 + 31)
        } else {
            (area.x0, area.z0, area.x1, area.z1)
        };
        Some(Span::new(Self::height(x1, z1), Self::height(x0, z0)))
    }
    fn heights(&self, grid: &KindGrid<'_, ()>, _: &[bool], out: &mut [HeightQ]) {
        for (row, &x) in out.chunks_exact_mut(grid.zs.len()).zip(grid.xs) {
            for (h, &z) in row.iter_mut().zip(grid.zs) {
                *h = Self::height(x, z) as HeightQ;
            }
        }
    }
}

#[test]
#[cfg(debug_assertions)]
fn an_apron_bound_under_the_heights_fails_the_span_check() {
    let registry = registry();
    let ramp = |aligned| {
        dry(
            Arc::new(Ramp { aligned }),
            bare().with_carver(id("test:holes"), Arc::new(Holes::new(1, 6, 3))),
        )
        .expect("opens")
    };
    let (right, wrong) = (ramp(false), ramp(true));
    // The highest chunk of each column at or under its deep bound. The chunk columns' own grounds
    // lie in their bounds either way (the point query checks them); the aligned bounds put the
    // deep bound of some over the grounds of the +x and +z columns of their apron, where the
    // carver keeps its roof: the deep path would carve that chunk otherwise.
    let mut caught = 0;
    for cx in 1..7 {
        for cz in 1..7 {
            drop(wrong.column_info(cx << 5, cz << 5));
            let deep =
                |g: &RegionGenerator<()>| key(cx, (g.local_bounds(cx, cz).deep - 31) >> 5, cz);
            let k = deep(&right);
            assert_eq!(
                right.generate(k, &registry),
                right.generate_full(k, &registry),
                "{k:?}"
            );
            let k = deep(&wrong);
            if let Some(message) = panic_of(|| drop(wrong.generate_full(k, &registry))) {
                assert!(
                    message.contains("deep bound") && message.contains("over the apron"),
                    "{message}"
                );
                caught += 1;
            }
        }
    }
    assert!(caught >= 3, "{caught} chunks caught");
}

/// A carver that carves nothing, as a `Mixed` mask of every bit with columns (one it did not pass
/// through `CarveMask::of`) and as `Solid` without.
struct Unpacked;

impl HashParams for Unpacked {
    fn hash_params(&self, _: &mut ParamsHasher) {}
}

impl GenEntry for Unpacked {
    fn version(&self) -> u32 {
        1
    }
}

impl CarverStage<()> for Unpacked {
    fn reach(&self) -> CarveReach {
        CarveReach::default()
    }
    fn carve(&self, chunk: &CarveChunk<'_, ()>) -> CarveMask {
        match chunk.columns {
            Some(_) => CarveMask::Mixed(Box::new([u32::MAX; CHUNK_AREA])),
            None => CarveMask::Solid,
        }
    }
    fn block_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

#[test]
fn the_reach_check_compares_the_voxels_a_mask_leaves() {
    let registry = registry();
    let g = flat(
        40,
        bare().with_carver(id("test:unpacked"), Arc::new(Unpacked)),
    )
    .expect("opens");
    // Under every ground: the same voxels either way, so the same chunk, and no span check fails.
    let k = key(1, -2, 1);
    let chunk = g.generate_full(k, &registry);
    assert_eq!(chunk, g.generate(k, &registry));
    assert_eq!(chunk.as_uniform(), registry.by_name("test:solid"));
}

/// A kind flat inside each chunk column that steps 4 blocks up from one chunk column to the next
/// along x, declaring a slope of 1: the freeze check passes it, and only the border of two chunk
/// columns shows it.
struct Steep;

impl HashParams for Steep {
    fn hash_params(&self, _: &mut ParamsHasher) {}
}

impl GenEntry for Steep {
    fn version(&self) -> u32 {
        1
    }
}

impl TerrainKind<()> for Steep {
    fn limits(&self, _: &()) -> KindLimits {
        let range = Span::new(-1 << 21, 1 << 21);
        KindLimits {
            slope: Slope {
                per_block: boxel_worldgen::field::Q16::from_raw(256 << 16),
                jitter: 0,
            },
            relief: range,
            band: range,
        }
    }
    fn range(&self, _: &()) -> Span {
        Span::new(-1 << 21, 1 << 21)
    }
    fn heights(&self, grid: &KindGrid<'_, ()>, _: &[bool], out: &mut [HeightQ]) {
        for (row, &x) in out.chunks_exact_mut(grid.zs.len()).zip(grid.xs) {
            row.fill(((x >> 5) * 1024) as HeightQ);
        }
    }
}

/// Shape modifier `0`, declaring no slope.
struct Lies(Valleys);

impl HashParams for Lies {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0);
    }
}

impl GenEntry for Lies {
    fn version(&self) -> u32 {
        1
    }
}

impl ShapeModifier<()> for Lies {
    fn effect(&self) -> ShapeEffect {
        ShapeModifier::<()>::effect(&self.0)
    }
    fn slope(&self) -> Slope {
        Slope::ZERO
    }
    fn apply(&self, grid: &KindGrid<'_, ()>, heights: &mut [HeightQ]) {
        self.0.apply(grid, heights);
    }
}

#[test]
#[cfg(debug_assertions)]
fn a_slope_under_the_heights_fails_the_slope_check() {
    let registry = registry();
    let caught = |g: &RegionGenerator<()>, k: ChunkKey| {
        let message = panic_of(|| drop(g.generate_full(k, &registry))).expect("caught");
        assert!(
            message.contains("slope check") && message.contains("more than 3 blocks apart"),
            "{message}"
        );
    };
    // A kind steeper than it declares across the +x border of every chunk column.
    let steep = dry(Arc::new(Steep), bare()).expect("opens");
    caught(&steep, key(0, 1, 0));
    caught(&steep, key(-7, 1, 3));
    // Valleys whose walls run 7.5:1 (30 blocks over 4), declaring no slope, on flat ground.
    let walls = Valleys {
        period: 256,
        gap: 64,
        width: 4,
        floor: 10 * HEIGHT_ONE,
        depth: 30 * 256,
        divide: true,
    };
    let lying = flat(
        40,
        bare().with_modifier(id("test:valleys"), Arc::new(Lies(walls))),
    )
    .expect("opens");
    caught(&lying, key(3, 1, 0));
    // Away from the walls the heights keep the cap; out of the check (a test-only escape for
    // fixtures steep on purpose), the walls' chunks build.
    drop(lying.generate_full(key(3, 1, 4), &registry));
    let unchecked = lying.without_slope_check();
    assert_eq!(
        unchecked.generate_full(key(3, 1, 0), &registry),
        unchecked.generate(key(3, 1, 0), &registry)
    );
}

/// A carver that carves nothing and records, per chunk, its origin, whether it got columns and
/// its `apron_lo`, over an apron of 3 and a surface reach of 6 (a cave stage with a water shell).
struct Lowest {
    seen: Mutex<Vec<(BlockPos, bool, i64)>>,
}

impl HashParams for Lowest {
    fn hash_params(&self, _: &mut ParamsHasher) {}
}

impl GenEntry for Lowest {
    fn version(&self) -> u32 {
        1
    }
}

impl CarverStage<TestContext> for Lowest {
    fn reach(&self) -> CarveReach {
        CarveReach {
            surface: 6,
            apron: 3,
        }
    }
    fn carve(&self, chunk: &CarveChunk<'_, TestContext>) -> CarveMask {
        let at = (chunk.origin, chunk.columns.is_some(), chunk.apron_lo);
        self.seen.lock().unwrap().push(at);
        CarveMask::Solid
    }
    fn block_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

#[test]
fn carvers_get_the_lowest_ground_of_their_apron_in_both_paths() {
    let registry = registry();
    let lowest = Arc::new(Lowest {
        seen: Mutex::new(Vec::new()),
    });
    let g = open(
        &test_world(true),
        with_valleys().with_carver(id("test:lowest"), Arc::clone(&lowest) as _),
    );
    let (mut deep, mut column, mut under) = (0, 0, 0);
    for (cx, cz) in sample_columns() {
        let b = g.local_bounds(cx, cz);
        assert!(
            b.apron_lo <= b.ground_lo && b.deep <= b.apron_lo - 6,
            "{b:?}"
        );
        under += usize::from(b.apron_lo < b.ground_lo);
        // Every ground of the chunk column and of the apron of 3 around it lies at or over it.
        let (o, a) = ((cx << 5, cz << 5), 3);
        let xs: Vec<i64> = (o.0 - a..o.0 + 32 + a).collect();
        let zs: Vec<i64> = (o.1 - a..o.1 + 32 + a).collect();
        let n = xs.len() * zs.len();
        let (mut ground, mut fluid) = (vec![0; n], vec![0; n]);
        g.column_tops(&xs, &zs, &mut ground, &mut fluid);
        assert!(ground.iter().all(|&g| b.apron_lo <= g), "({cx}, {cz})");
        lowest.seen.lock().unwrap().clear();
        for y in (b.deep >> 5) - 1..=(b.top >> 5) {
            drop(g.generate(key(cx, y, cz), &registry));
        }
        // The carver got it in every chunk it ran in, with columns or without: the deep path,
        // whose chunks lie the surface reach under it, or the debug check of its reach, whose
        // chunks lie that far under the true grounds.
        let truth = ground.iter().copied().min().expect("columns");
        for (origin, columns, apron_lo) in std::mem::take(&mut *lowest.seen.lock().unwrap()) {
            let y1 = origin.y + 31;
            assert_eq!(apron_lo, b.apron_lo, "{origin:?}");
            if columns {
                column += 1;
            } else if y1 <= b.deep {
                assert!(y1 <= apron_lo - 6, "{origin:?}");
                deep += 1;
            } else {
                assert!(y1 <= truth - 6, "{origin:?}");
            }
        }
    }
    assert!(
        deep >= 14 && column >= 14,
        "{deep} deep, {column} column stage chunks"
    );
    // The apron reaches under the chunk column's own lowest ground somewhere.
    assert!(under > 0, "no apron under its chunk column");
}
