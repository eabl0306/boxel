//! The far LOD of the region generator (#120, ADR 0039 sec. 10 and "Cómo se prueba" test 9),
//! through the exported harness (`check_lod`) where it can, over `test:` content
//! (`support/region.rs`):
//! - border nodes of 2 and 3 kinds at levels 1 to 4 against the cell rule on real chunks: dry tops
//!   never under the real ones and within their declared margin over them, fluid only off within a
//!   cell of a fluid edge, bands conservative, borders equal between neighbors; with exact kinds
//!   and with a kind that drops octaves far away;
//! - a divide between two carved valleys inside one cell keeps the coarse top over the real one,
//!   and the harness catches a coarse form without the divide term;
//! - sea and coast cells at levels 1 to 6 agree with the chunks within a cell;
//! - exactness: the pass's corners are the chunk columns' heights, bit for bit, at the origin,
//!   `+-2^31`, `2^40` and near `+-MAX_BLOCK_COORD`, with a shape modifier too;
//! - determinism with the memo cold, warm, evicted and off, on several threads, and world A then
//!   world B on the same worker (the build counter); the memo stays within 512 KiB;
//! - the band and the cells stop at the realm's top plane, alone and inside a dimension;
//! - the shape modifiers: in the chunk path and the bounds, their order and digest lines,
//!   duplicates refused, an effect broken caught by the span check.
#[path = "support/region.rs"]
mod support;

use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_SIZE, ChunkData, ChunkKey, ChunkPos, DimensionId, LocalPos,
    MAX_BLOCK_COORD, MAX_CHUNK_COORD,
};
use boxel_worldgen::lod::{LodColumn, LodNode, LodRequest, LodScratch, LodSource};
use boxel_worldgen::realm::{FluidRule, RealmLod, RealmSpan, RealmSpec, RealmStack};
use boxel_worldgen::region::{
    Decoration, DimensionGenerator, Entry, FlatKind, FloorFill, HEIGHT_ONE, HeightQ, KindGrid,
    LodReport, NoSurface, RegionGenerator, RegionSettings, RegionSpec, RuleEntry, Scope,
    SelectorRule, ShapeEffect, ShapeModifier, TerrainKind, TopLayers, check_lod, lod_corners,
};
use boxel_worldgen::registry::{
    Checks, DigestBuilder, GenEntry, GenRegistryBuilder, HashParams, Order, ParamsHasher,
};
use boxel_worldgen::{WorldGenerator, noise::splitmix64};
use std::sync::Arc;
use support::*;

/// Region cells of 512 blocks: borders close to each other.
const CELLS: u32 = 9;

fn request(level: u8, column: [i64; 2]) -> LodRequest {
    LodRequest {
        dim: DimensionId(0),
        level,
        column,
        layers: -40..=40,
    }
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

fn report(at: &str, r: Result<LodReport, String>) -> LodReport {
    match r {
        Ok(r) => r,
        Err(e) => panic!("{at}: {e}"),
    }
}

// ---- Border nodes against the cell rule -------------------------------------------------------

#[test]
fn border_nodes_follow_the_cell_rule() {
    let registry = registry();
    let world = world(CELLS);
    let g = generator(&world, SEED, RegionSettings::default());
    let mut total = LodReport::default();
    for level in 1..=4u8 {
        for kinds in [1, 2, 3] {
            for (n, node) in nodes_with(&g, level, kinds, [3, -5], 2)
                .into_iter()
                .enumerate()
            {
                let columns = columns_in(level, node, 6, n as u64 + u64::from(level) * 31);
                let at = format!("level {level}, {kinds} kinds, node {node:?}");
                let r = report(&at, check_lod(&g, &registry, &columns, level..=level));
                total.cells += r.cells;
                total.dry += r.dry;
                total.wet += r.wet;
                total.same_top += r.same_top;
                total.borders += r.borders;
                total.max_over = total.max_over.max(r.max_over);
            }
        }
    }
    assert!(total.dry > 100, "{total:?}");
    assert!(total.borders > 0, "{total:?}");
    // Most coarse tops stand in the real top's cell (the margins are small at these levels).
    assert!(total.same_top * 2 > total.dry, "{total:?}");
}

#[test]
fn a_kind_that_drops_octaves_far_away_passes_the_harness() {
    let registry = registry();
    let kinds: Vec<(&str, Arc<dyn TerrainKind<TestContext>>)> = kinds()
        .into_iter()
        .map(|(name, relief)| {
            let kind = CoarseRelief { relief, from: 2 };
            (name, Arc::new(kind) as Arc<dyn TerrainKind<TestContext>>)
        })
        .collect();
    let world = world_of(
        vec![("test:surface", realm())],
        rules(CELLS),
        Vec::new(),
        kinds,
    );
    let g = generator(&world, SEED, RegionSettings::default());
    for level in 1..=4u8 {
        for kinds in [1, 2] {
            for node in nodes_with(&g, level, kinds, [-9, 4], 2) {
                let columns = columns_in(level, node, 5, u64::from(level));
                let at = format!("level {level}, {kinds} kinds, node {node:?}");
                report(&at, check_lod(&g, &registry, &columns, level..=level));
            }
        }
    }
}

/// A test kind whose far LOD claims exact tops: its corners with no margin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct NoMargin(Relief);

impl HashParams for NoMargin {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0);
    }
}

impl GenEntry for NoMargin {
    fn version(&self) -> u32 {
        1
    }
}

impl TerrainKind<TestContext> for NoMargin {
    fn limits(&self, cx: &TestContext) -> boxel_worldgen::region::KindLimits {
        self.0.limits(cx)
    }
    fn range(&self, cx: &TestContext) -> boxel_worldgen::field::Span {
        self.0.range(cx)
    }
    fn bounds(
        &self,
        cx: &TestContext,
        seed: u64,
        q: (i64, i64),
        reach: i64,
    ) -> boxel_worldgen::field::Span {
        self.0.bounds(cx, seed, q, reach)
    }
    fn heights(&self, grid: &KindGrid<'_, TestContext>, mask: &[bool], out: &mut [HeightQ]) {
        self.0.heights(grid, mask, out);
    }
    fn lod_heights(
        &self,
        grid: &KindGrid<'_, TestContext>,
        _level: u8,
        mask: &[bool],
        out: &mut [HeightQ],
        margin: &mut [HeightQ],
    ) {
        self.0.heights(grid, mask, out);
        margin.fill(0);
    }
}

/// The test world with `kind` everywhere (one Cell rule, one entry).
fn one_kind_world(kind: Arc<dyn TerrainKind<TestContext>>) -> TestWorld {
    world_of(
        vec![("test:surface", realm())],
        vec![(
            "test:land",
            SelectorRule {
                realm: id("test:surface"),
                priority: 0,
                condition: Vec::new(),
                scope: Scope::Cell(CELLS),
                entries: vec![Entry::new(id("test:hills"), 1)],
                version: 1,
            },
        )],
        Vec::new(),
        vec![("test:hills", kind)],
    )
}

/// Steep hills everywhere, their tops between the corners of a 16-block cell.
const STEEP: Relief = Relief::blocks(60, 20, 128, 2);

#[test]
fn the_harness_catches_a_margin_under_the_heights() {
    let registry = registry();
    let g = generator(
        &one_kind_world(Arc::new(NoMargin(STEEP))),
        SEED,
        RegionSettings::default(),
    );
    let columns: Vec<(i64, i64)> = (0..64).map(|n| (n * 37 + 3, n * 23 - 700)).collect();
    let e = check_lod(&g, &registry, &columns, 4..=4).expect_err("tops under the real ones");
    assert!(e.contains("under the real one"), "{e}");
    // The same kind with the default margin (its declared slope) passes.
    let g = generator(
        &one_kind_world(Arc::new(STEEP)),
        SEED,
        RegionSettings::default(),
    );
    report("default margin", check_lod(&g, &registry, &columns, 1..=4));
}

#[test]
fn a_top_run_that_is_not_solid_slides_up_with_the_ground() {
    let registry = registry();
    // Glass over the Solid run: a column's highest Solid block lies 2 under its ground, and a
    // cell's ground may stand up to its margin over its highest corner.
    let decoration = Decoration::new(
        (id("test:fill"), Arc::new(FloorFill)),
        (
            id("test:surface"),
            Arc::new(TopLayers::new(vec![
                ("test:glass".into(), 2),
                ("test:cutout".into(), 1),
            ])),
        ),
    );
    let g = generator_with(
        &one_kind_world(Arc::new(STEEP)),
        SEED,
        RegionSettings::default(),
        decoration,
    );
    // Cells whose real tops stand a cell over their highest corner's (found by brute force: with
    // the runs anchored at that corner, their coarse tops were a cell under the real ones), and a
    // few more.
    let mut columns = vec![(1680, -992), (-2464, 3856), (-2448, 3808), (5760, 6912)];
    columns.extend(columns_in(4, [3, -2], 8, 5));
    report("glass on top", check_lod(&g, &registry, &columns, 1..=4));
}

// ---- The divide term --------------------------------------------------------------------------

/// A flat kind at 40 blocks with the valleys of `valleys`, under `()`, dry.
fn valley_world(valleys: Valleys) -> RegionGenerator<()> {
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
            scope: Scope::Cell(12),
            entries: vec![Entry::new(id("test:high"), 1)],
            version: 1,
        },
    );
    let mut kinds = GenRegistryBuilder::new("terrain_kind");
    kinds.register(
        id("test:high"),
        Arc::new(FlatKind::blocks(40)) as Arc<dyn TerrainKind<()>>,
    );
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
            decoration: Decoration::new(
                (id("test:fill"), Arc::new(FloorFill)),
                (id("test:none"), Arc::new(NoSurface)),
            )
            .with_modifier(id("test:valleys"), Arc::new(valleys)),
            settings: RegionSettings::default(),
        },
        |_| (),
        &mut checks,
    );
    checks.finish().expect("opens");
    g.unwrap()
}

/// Two valleys 12 blocks apart, 12 blocks wide, carved from 40 down to 10.
const VALLEYS: Valleys = Valleys {
    period: 256,
    gap: 12,
    width: 12,
    floor: 10 * HEIGHT_ONE,
    depth: 30 * 256,
    divide: true,
};

#[test]
fn a_divide_between_two_valleys_keeps_the_coarse_top_over_the_real_one() {
    let registry = registry();
    // Columns across the pair of valleys and the divide between them (z = 6), and a few far
    // from them.
    let columns: Vec<(i64, i64)> = (0..24)
        .map(|z| (5 + z * 3, z))
        .chain([(100, 100), (-40, 200), (7, -20)])
        .collect();
    let with = valley_world(VALLEYS);
    // The real ground at the divide: half the depth carved (6 of 12 blocks from each valley).
    assert_eq!(with.column_info(0, 6).ground, 25);
    let r = report(
        "with the divide term",
        check_lod(&with, &registry, &columns, 1..=4),
    );
    assert!(r.dry == r.cells, "{r:?}");
    // The same carve whose coarse form takes every cell's corners as sharing a valley: the cells
    // whose corners lie in the two valleys get the carve of their corners, under the divide.
    let without = valley_world(Valleys {
        divide: false,
        ..VALLEYS
    });
    let e = check_lod(&without, &registry, &columns, 3..=4).expect_err("the harness sees it");
    assert!(e.contains("under the real one"), "{e}");
    // At level 1 the divide's cell holds no carved corner that low: both pass there.
    report(
        "without, level 1",
        check_lod(&without, &registry, &columns, 1..=1),
    );
}

// ---- Sea and coasts ---------------------------------------------------------------------------

#[test]
fn sea_and_coast_cells_agree_with_the_chunks_within_a_cell() {
    let registry = registry();
    let world = world(CELLS);
    let g = generator(&world, SEED, RegionSettings::default());
    // Coasts: around the basins (under the sea), where the ground crosses the sea level.
    let (mut coasts, mut seas) = (Vec::new(), Vec::new());
    'search: for i in -400i64..400 {
        for j in (-400i64..400).step_by(7) {
            if g.lattice_kind(i, j).as_str() != "test:basin" {
                continue;
            }
            let (x, z) = (i << 8, j << 8);
            if g.column_info(x, z).fluid_top.is_some() {
                seas.push((x, z));
            }
            let mut last = g.column_info(x, z).fluid_top.is_some();
            for d in 1..1024 {
                let wet = g.column_info(x + d, z).fluid_top.is_some();
                if wet != last {
                    coasts.push((x + d, z));
                    break;
                }
                last = wet;
            }
            if coasts.len() >= 24 {
                break 'search;
            }
        }
    }
    assert!(coasts.len() >= 8, "{} coasts", coasts.len());
    let near: Vec<(i64, i64)> = coasts
        .iter()
        .take(24)
        .flat_map(|&(x, z)| [(x, z), (x + 3, z - 2), (x - 5, z + 1)])
        .collect();
    let r = report("levels 1 to 4", check_lod(&g, &registry, &near, 1..=4));
    assert!(r.wet > 0 && r.dry > 0, "{r:?}");
    // Levels 5 and 6 on fewer columns (a cell is 32 or 64 blocks: the real cells are costly).
    let few: Vec<(i64, i64)> = coasts
        .iter()
        .step_by(3)
        .take(6)
        .chain(seas.iter().take(4))
        .copied()
        .collect();
    let far = report("levels 5 and 6", check_lod(&g, &registry, &few, 5..=6));
    eprintln!("coasts: {r:?}, far: {far:?}");
    assert!(far.wet > 0, "{far:?}");
}

// ---- Exactness --------------------------------------------------------------------------------

/// The exactness worlds: the test world, and the same with the valleys carved over it.
fn exact_worlds() -> Vec<RegionGenerator<TestContext>> {
    let world = world(CELLS);
    vec![
        generator(&world, SEED, RegionSettings::default()),
        generator_with(
            &world,
            SEED,
            RegionSettings::default(),
            decoration().with_modifier(
                id("test:valleys"),
                Arc::new(Valleys {
                    floor: -20 * HEIGHT_ONE,
                    depth: 400 * 256,
                    ..VALLEYS
                }),
            ),
        ),
    ]
}

#[test]
fn corners_are_the_chunk_columns_far_from_the_origin() {
    let registry = registry();
    let far = MAX_BLOCK_COORD - (1 << 16);
    for g in exact_worlds() {
        for (x, z) in [
            (0, 0),
            (1 << 31, -(1 << 31)),
            (-(1 << 31) - 77, 1 << 31),
            (1 << 40, 3 << 38),
            (far, -far),
            (-far, far),
        ] {
            for level in 1..=8u8 {
                let shift = 5 + u32::from(level);
                // A node with two kinds or more near the point, else the point's own.
                let own = [x >> shift, z >> shift];
                let node = (0..64)
                    .map(|n| [own[0] + n % 8, own[1] + n / 8])
                    .find(|&c| node_kinds(&g, level, c) > 1)
                    .unwrap_or(own);
                assert!(node[0].abs() <= MAX_CHUNK_COORD >> level);
                let corners = lod_corners(&g, &registry, &request(level, node));
                for (n, ((cx, cz), h)) in corners.into_iter().enumerate() {
                    // Every corner at levels 1 to 3, a sample above.
                    if level > 3 && n % 7 != 0 {
                        continue;
                    }
                    assert_eq!(
                        h,
                        g.column_info(cx, cz).height,
                        "level {level} node {node:?} corner ({cx}, {cz})"
                    );
                }
            }
        }
    }
}

/// The `y` of the top solid block of column `(lx, lz)` of `chunk`.
fn top_solid(chunk: &ChunkData, y0: i64, lx: u32, lz: u32, fluid: BlockState) -> Option<i64> {
    (0..CHUNK_SIZE as u32).rev().find_map(|ly| {
        let s = chunk.get(LocalPos::new(lx, ly, lz));
        (s != BlockState::AIR && s != fluid).then_some(y0 + i64::from(ly))
    })
}

#[test]
fn a_shape_modifier_reaches_points_chunks_and_bounds() {
    let registry = registry();
    let fluid = registry.by_name("test:fluid").unwrap();
    let [plain, carved] = <[_; 2]>::try_from(exact_worlds()).ok().unwrap();
    let mut lowered = 0;
    for &(x, z) in &[(17, 0), (17, 6), (300, 12), (-81, 268), (5, -250)] {
        let (a, b) = (plain.column_info(x, z), carved.column_info(x, z));
        assert!(b.height <= a.height, "({x}, {z}) raised");
        lowered += usize::from(b.height < a.height);
        // The chunk holding the carved ground has it, inside the column's bounds.
        let key = ChunkKey::new(ChunkPos::new(x >> 5, b.ground >> 5, z >> 5));
        let chunk = carved.generate(key, &registry);
        let y0 = key.pos.origin().y;
        assert_eq!(
            top_solid(&chunk, y0, (x & 31) as u32, (z & 31) as u32, fluid),
            Some(b.ground),
            "({x}, {z})"
        );
        assert!(
            b.ground_lo <= b.ground && b.ground <= b.top,
            "({x}, {z}): {b:?}"
        );
    }
    assert!(lowered >= 3, "the valleys carved {lowered} columns");
    // The cut moves the local bounds and the realm's range: deeper by the declared depth.
    let (bp, bc) = (plain.local_bounds(0, 0), carved.local_bounds(0, 0));
    assert_eq!(bc.ground_lo, bp.ground_lo - 400);
    assert_eq!(bc.ground_hi, bp.ground_hi);
    assert_ne!(
        plain.version(),
        carved.version(),
        "the modifier has a digest line"
    );
}

/// Past cells of a lattice cell (level 9 and up at the default lattice) the pass reads the kinds
/// at the corners alone: never asked for today (`MAX_LEVELS` is 6), but it answers, in bounded
/// time, with its debug checks on (every top over its corners) and bands that hold the ground.
#[test]
fn levels_beyond_a_lattice_cell_read_the_corners() {
    let registry = registry();
    let g = generator(&world(CELLS), SEED, RegionSettings::default());
    for level in 9..=12u8 {
        for column in [[0i64, 0], [-1, 1], [3, -2]] {
            let mut cells = 0;
            let band = g.column(
                &request(level, column),
                &registry,
                &mut LodScratch::default(),
                &mut |_, node| cells += usize::from(matches!(node, LodNode::Cells(_))),
            );
            assert!(band.lo < band.hi, "level {level} {column:?}: {band:?}");
            assert!(cells > 0, "level {level} {column:?}: no cells");
        }
    }
}

// ---- Determinism and the memo ------------------------------------------------------------------

/// A column's band and nodes, hashed.
fn fingerprint(
    source: &dyn LodSource,
    registry: &BlockRegistry,
    req: &LodRequest,
    scratch: &mut LodScratch,
) -> (LodColumn, u64) {
    let mut bytes = Vec::new();
    let band = source.column(req, registry, scratch, &mut |layer, node| {
        bytes.extend(layer.to_le_bytes());
        match node {
            LodNode::Uniform(state) => bytes.extend(state.0.to_le_bytes()),
            LodNode::Cells(cells) => {
                bytes.extend(cells.0.iter().flat_map(|s| s.0.to_le_bytes()));
            }
        }
    });
    (band, boxel_worldgen::registry::fnv1a64(&bytes))
}

/// Requests at levels 1 to 6 around a few places, shuffled by `seed`, each twice in a row
/// somewhere (a second job of a tall column).
fn requests(seed: u64) -> Vec<LodRequest> {
    let mut out = Vec::new();
    for level in 1..=6u8 {
        for (n, [x, z]) in [[0i64, 0], [-1, 2], [5, 5], [1 << 20, -(1 << 21)], [3, -7]]
            .into_iter()
            .enumerate()
        {
            out.push(request(level, [x + n as i64, z]));
        }
    }
    let mut keyed: Vec<(u64, LodRequest)> = out
        .into_iter()
        .enumerate()
        .map(|(i, r)| (splitmix64(seed ^ i as u64), r))
        .collect();
    keyed.sort_by_key(|(k, _)| *k);
    let mut out: Vec<LodRequest> = keyed.into_iter().map(|(_, r)| r).collect();
    let again = out[3].clone();
    out.insert(4, again);
    out
}

#[test]
fn the_cells_do_not_depend_on_the_memo() {
    let registry = registry();
    let world = world(CELLS);
    let g = generator(&world, SEED, RegionSettings::default());
    let off = generator(
        &world,
        SEED,
        RegionSettings {
            memo: false,
            ..RegionSettings::default()
        },
    );
    let reqs = requests(1);
    let cold: Vec<_> = reqs
        .iter()
        .map(|r| fingerprint(&g, &registry, r, &mut LodScratch::default()))
        .collect();
    let mut warm = LodScratch::default();
    let warm: Vec<_> = reqs
        .iter()
        .map(|r| fingerprint(&g, &registry, r, &mut warm))
        .collect();
    assert_eq!(cold, warm, "warm");
    let mut evicted = LodScratch::default();
    let evicted: Vec<_> = reqs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if i % 3 == 0 {
                evicted.memo = None;
            }
            fingerprint(&g, &registry, r, &mut evicted)
        })
        .collect();
    assert_eq!(cold, evicted, "evicted");
    let mut off_scratch = LodScratch::default();
    let off: Vec<_> = reqs
        .iter()
        .map(|r| fingerprint(&off, &registry, r, &mut off_scratch))
        .collect();
    assert_eq!(cold, off, "off");
    // Another generator's memo in the scratch (a flat world's) is no memo of this one.
    let mut mixed = LodScratch::default();
    let flat = boxel_worldgen::FlatGenerator::new(vec![("test:solid".into(), 3)]);
    for r in &reqs {
        fingerprint(&flat, &registry, r, &mut mixed);
        assert_eq!(
            fingerprint(&g, &registry, r, &mut mixed),
            fingerprint(&g, &registry, r, &mut LodScratch::default())
        );
    }
    // Shuffled on several threads, each with its own scratch.
    let threads: Vec<Vec<_>> = std::thread::scope(|scope| {
        (0..3u64)
            .map(|t| {
                let (g, registry) = (&g, &registry);
                scope.spawn(move || {
                    let mut scratch = LodScratch::default();
                    let mut out: Vec<_> = requests(t + 10)
                        .iter()
                        .map(|r| (r.clone(), fingerprint(g, registry, r, &mut scratch)))
                        .collect();
                    out.sort_by_key(|(r, _)| (r.level, r.column));
                    out.dedup_by_key(|(r, _)| (r.level, r.column));
                    out
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect()
    });
    for t in &threads[1..] {
        assert_eq!(t, &threads[0]);
    }
    for (r, f) in &threads[0] {
        assert_eq!(
            f,
            &fingerprint(&g, &registry, r, &mut LodScratch::default())
        );
    }
}

#[test]
fn world_b_after_world_a_on_the_same_worker_is_world_b() {
    let registry = registry();
    let world = world(CELLS);
    let a = generator(&world, SEED, RegionSettings::default());
    let b = generator(&world, SEED ^ 0xb, RegionSettings::default());
    // The same seed and content built again: its own key, the same cells.
    let a2 = generator(&world, SEED, RegionSettings::default());
    let mut scratch = LodScratch::default();
    let reqs = requests(7);
    for r in &reqs {
        fingerprint(&a, &registry, r, &mut scratch);
    }
    let mut differ = 0;
    for r in &reqs {
        let got = fingerprint(&b, &registry, r, &mut scratch);
        assert_eq!(
            got,
            fingerprint(&b, &registry, r, &mut LodScratch::default())
        );
        differ += usize::from(got != fingerprint(&a, &registry, r, &mut LodScratch::default()));
    }
    assert!(
        differ > reqs.len() / 2,
        "the two worlds differ in {differ} columns"
    );
    for r in &reqs {
        assert_eq!(
            fingerprint(&a2, &registry, r, &mut scratch),
            fingerprint(&a, &registry, r, &mut LodScratch::default())
        );
    }
}

#[test]
fn the_memo_stays_within_its_budget() {
    let registry = registry();
    let world = world(CELLS);
    let g = generator(&world, SEED, RegionSettings::default());
    let mut scratch = LodScratch::default();
    for seed in 0..4 {
        for r in requests(seed) {
            fingerprint(&g, &registry, &r, &mut scratch);
        }
    }
    let bytes = boxel_worldgen::region::lod_memo_bytes(&g, &mut scratch).expect("its memo");
    eprintln!("the far LOD memo of 3 kinds: {} KiB", bytes >> 10);
    assert!(bytes <= 512 << 10, "{bytes} bytes");
    assert!(bytes > 0);
}

// ---- Realms ------------------------------------------------------------------------------------

/// A flat kind at `height` blocks in a realm of `span`, dry.
fn flat_realm(span: RealmSpan, height: i32) -> RegionGenerator<()> {
    let mut checks = Checks::new();
    let spec = RealmSpec {
        span,
        fluid: FluidRule::None,
        ..realm()
    };
    let mut realms = GenRegistryBuilder::new("realm");
    realms.register(id("test:flat"), spec);
    let mut rules = GenRegistryBuilder::new("selector_rule");
    rules.register(
        id("test:all"),
        SelectorRule {
            realm: id("test:flat"),
            priority: 0,
            condition: Vec::new(),
            scope: Scope::Cell(12),
            entries: vec![Entry::new(id("test:tall"), 1)],
            version: 1,
        },
    );
    let mut kinds = GenRegistryBuilder::new("terrain_kind");
    kinds.register(
        id("test:tall"),
        Arc::new(FlatKind::blocks(height)) as Arc<dyn TerrainKind<()>>,
    );
    let (realms_r, rules, entries, kinds) = (
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
            realms: &realms_r,
            rules: &rules,
            entries: &entries,
            kinds: &kinds,
            blocks: &registry(),
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

/// Every non-air cell row of `req`'s layers, as block `y` ranges `(lo, hi)`: the rows of the
/// cells `source` emits and the whole padded blocks of the layers under its band.
fn solid_rows(
    source: &dyn LodSource,
    registry: &BlockRegistry,
    req: &LodRequest,
) -> (LodColumn, Vec<(i64, i64)>) {
    let (s, shift) = (req.cell_size(), 5 + u32::from(req.level));
    let mut rows = Vec::new();
    let band = source.column(
        req,
        registry,
        &mut LodScratch::default(),
        &mut |layer, node| {
            let y0 = layer << shift;
            match node {
                LodNode::Uniform(BlockState::AIR) => {}
                LodNode::Uniform(_) => rows.push((y0 - s, y0 + 33 * s - 1)),
                LodNode::Cells(cells) => {
                    for y in -1..=32 {
                        if (-1..=32)
                            .any(|x| (-1..=32).any(|z| cells.get(x, y, z) != BlockState::AIR))
                        {
                            let lo = y0 + i64::from(y) * s;
                            rows.push((lo, lo + s - 1));
                        }
                    }
                }
            }
        },
    );
    for layer in req.layers.clone() {
        if band
            .uniform_at(layer)
            .is_some_and(|state| state != BlockState::AIR)
        {
            let y0 = layer << shift;
            rows.push((y0 - s, y0 + 33 * s - 1));
        }
    }
    (band, rows)
}

#[test]
fn the_far_lod_stops_at_the_realms_top_plane() {
    let registry = registry();
    // Ground up to 5000 in a realm under 4096: nothing at or over the plane is drawn.
    let g = flat_realm(RealmSpan::below(4096), 5000);
    for level in 1..=6u8 {
        let e = 32i64 << level;
        let req = LodRequest {
            layers: (4096 / e - 3)..=(4096 / e + 3),
            ..request(level, [2, -3])
        };
        let (band, rows) = solid_rows(&g, &registry, &req);
        let s = req.cell_size();
        assert!(
            band.hi << (5 + level) <= 4096 + s + e,
            "level {level}: {band:?}"
        );
        assert!(
            rows.iter().all(|&(lo, _)| lo < 4096),
            "level {level}: rows {rows:?} at or over the plane"
        );
        assert!(
            rows.iter().any(|&(_, hi)| hi >= 4096 - s),
            "level {level}: the ground up to the plane, {rows:?}"
        );
        assert_eq!(band.uniform_at(band.hi), Some(BlockState::AIR));
    }
    // Inside a dimension, with a realm over it, the dimension's source is the drawn realm's.
    let surface = flat_realm(RealmSpan::below(4096), 5000);
    let mut checks = Checks::new();
    let stack = RealmStack::build(
        &mut checks,
        &id("test:overworld"),
        vec![
            (
                id("test:flat"),
                RealmSpec {
                    span: RealmSpan::below(4096),
                    fluid: FluidRule::None,
                    ..realm()
                },
            ),
            (
                id("test:sky"),
                RealmSpec {
                    span: RealmSpan::above(4096),
                    lod: RealmLod::Off,
                    ..realm()
                },
            ),
        ],
    )
    .expect("a good layout");
    let mut digest = DigestBuilder::new();
    digest.extend(surface.digest());
    let alone = flat_realm(RealmSpan::below(4096), 5000);
    let realms: Vec<Box<dyn WorldGenerator>> = vec![
        Box::new(surface),
        Box::new(boxel_worldgen::FlatGenerator::new(vec![])),
    ];
    let dim = DimensionGenerator::new(id("test:overworld"), stack, realms, &digest.finish());
    let source = dim.lod().expect("the drawn realm has a source");
    assert_eq!(source.version(), WorldGenerator::version(&dim));
    for level in 1..=4u8 {
        let req = request(level, [1, 1]);
        let mut a = Vec::new();
        let band_a = source.column(&req, &registry, &mut LodScratch::default(), &mut |l, n| {
            a.push((l, matches!(n, LodNode::Cells(_))));
        });
        let mut b = Vec::new();
        let band_b = alone.column(&req, &registry, &mut LodScratch::default(), &mut |l, n| {
            b.push((l, matches!(n, LodNode::Cells(_))));
        });
        assert_eq!((band_a, a), (band_b, b), "level {level}");
    }
}

// ---- Shape modifiers ---------------------------------------------------------------------------

/// A modifier that raises every column by `by` but declares `declared`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Lift {
    by: HeightQ,
    declared: u32,
    priority: i32,
}

impl HashParams for Lift {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.by).write(&self.declared);
    }
}

impl GenEntry for Lift {
    fn version(&self) -> u32 {
        1
    }
    fn order(&self) -> Order {
        Order::new(0, self.priority)
    }
}

impl<Cx: boxel_worldgen::region::Context> ShapeModifier<Cx> for Lift {
    fn effect(&self) -> ShapeEffect {
        ShapeEffect::Raise(self.declared)
    }
    fn apply(&self, _grid: &KindGrid<'_, Cx>, heights: &mut [HeightQ]) {
        for h in heights {
            *h += self.by;
        }
    }
}

fn open_with(modifiers: Vec<(&str, Lift)>) -> Result<RegionGenerator<TestContext>, String> {
    let world = world(CELLS);
    let mut decoration = decoration();
    for (name, m) in modifiers {
        decoration = decoration.with_modifier(id(name), Arc::new(m));
    }
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
            settings: RegionSettings::default(),
        },
        |realm_seed| TestContext::new(realm_seed, BASE),
        &mut checks,
    );
    checks.finish().map_err(|e| e.to_string())?;
    Ok(g.expect("no problem"))
}

#[test]
fn a_raise_lifts_the_bounds_and_the_far_lod() {
    let registry = registry();
    let plain = generator(&world(CELLS), SEED, RegionSettings::default());
    let lift = Lift {
        by: 7 * HEIGHT_ONE,
        declared: 7 * 256,
        priority: 0,
    };
    let g = open_with(vec![("test:lift", lift)]).unwrap();
    for (x, z) in [(0, 0), (900, -77), (-3000, 41)] {
        assert_eq!(
            g.column_info(x, z).height,
            plain.column_info(x, z).height + 7 * HEIGHT_ONE
        );
    }
    assert_eq!(
        g.local_bounds(3, 3).ground_hi,
        plain.local_bounds(3, 3).ground_hi + 7
    );
    assert!(g.vertical_chunks().end() >= plain.vertical_chunks().end());
    let columns = columns_in(2, [4, 9], 6, 3);
    report("lifted", check_lod(&g, &registry, &columns, 1..=3));
}

#[test]
fn modifiers_run_by_priority_and_a_duplicate_is_refused() {
    let small = Lift {
        by: HEIGHT_ONE,
        declared: 256,
        priority: 0,
    };
    // Two modifiers in either order give the same digest (they sort by priority, then id).
    let a = open_with(vec![
        ("test:a", small),
        (
            "test:b",
            Lift {
                priority: 5,
                ..small
            },
        ),
    ])
    .unwrap();
    let b = open_with(vec![
        (
            "test:b",
            Lift {
                priority: 5,
                ..small
            },
        ),
        ("test:a", small),
    ])
    .unwrap();
    assert_eq!(a.digest(), b.digest());
    let lines: Vec<&str> = a
        .digest()
        .entries()
        .iter()
        .filter(|e| e.registry == "shape_modifier")
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(lines, ["test:a", "test:b"]);
    let Err(e) = open_with(vec![("test:a", small), ("test:a", small)]) else {
        panic!("a duplicate opens");
    };
    assert!(e.contains("test:a"), "{e}");
}

#[test]
#[cfg(debug_assertions)]
fn the_span_check_catches_a_modifier_outside_its_effect() {
    let registry = registry();
    let g = open_with(vec![(
        "test:liar",
        Lift {
            by: 9 * HEIGHT_ONE,
            declared: 256,
            priority: 0,
        },
    )])
    .unwrap();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        g.column_info(3, 3);
    }))
    .expect_err("caught");
    let message = caught.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(
        message.contains("test:liar") && message.contains("declared effect"),
        "{message}"
    );
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        LodSource::column(
            &g,
            &request(2, [0, 0]),
            &registry,
            &mut LodScratch::default(),
            &mut |_, _| {},
        );
    }));
    assert!(caught.is_err(), "the far LOD checks it too");
}
