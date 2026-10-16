//! The decoration of the region generator (#133, ADR 0039 sec. 9, 12 and 14; "Cómo se prueba"
//! test 8), over `test:` content (`support/region.rs`):
//! - bounds per chunk column: the kinds' `column_bounds` and the shape modifiers' `column_effect`
//!   tighten the local bounds, the heights stay the same, and every block stays under them;
//! - every debug check: a column bound under the heights, a column effect under the cut or
//!   outside the modifier's effect.
#[path = "support/region.rs"]
mod support;

use boxel_voxel::{BlockState, CHUNK_AREA, CHUNK_SIZE, ChunkData, ChunkKey, ChunkPos, LocalPos};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::field::{Rect, Slope, Span};
use boxel_worldgen::region::{
    Context, Decoration, Entry, HEIGHT_ONE, HeightQ, KindGrid, KindLimits, RegionGenerator,
    RegionSettings, RegionSpec, Scope, SelectorRule, ShapeEffect, ShapeModifier, TerrainKind,
    block_of,
};
use boxel_worldgen::registry::{Checks, GenEntry, GenId, HashParams, ParamsHasher};
use std::sync::Arc;
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
