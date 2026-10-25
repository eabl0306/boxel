//! The seam harness (`check_seam`, the TFC study's S1) over `test:` content
//! (`support/region.rs`): where full detail meets the level-1 far LOD at a chunk plane.
//! - a sound far form passes, inland and on coasts (the fluids meet: wet in both), every kind of
//!   ray cast; the same report on every run;
//! - a far form that lowers a dry corner at level 1 fails, as lowered edge columns and as sky
//!   hits from over the cells into the chunks;
//! - ground that rises over the fluid between the cell corners is a wet sliver, its own class;
//! - caves at the seam show only with the chunks as drawn (carvers in).
#[path = "support/region.rs"]
mod support;

use boxel_worldgen::field::{Q16, Slope, Span};
use boxel_worldgen::region::{
    Context, Decoration, Entry, FloorFill, HeightQ, KindGrid, KindLimits, RegionGenerator,
    RegionSettings, RegionSpec, RuleEntry, Scope, SeamOptions, SelectorRule, ShapeEffect,
    ShapeModifier, TerrainKind, TopLayers, check_seam,
};
use boxel_worldgen::registry::{Checks, GenEntry, GenRegistryBuilder, HashParams, ParamsHasher};
use std::sync::Arc;
use support::*;

/// A shape modifier that leaves the chunks as they are and, with `sinks`, lowers every dry corner
/// of its coarse form at level 1 by 4 blocks (and the cells' tops with them): the far form of a
/// river that lowers a dry bank's corner. Without, its coarse form is the default one, sound.
struct Sinks {
    sinks: bool,
}

/// What [`Sinks`] lowers, in 1/256 blocks.
const SINK: i64 = 4 * 256;

impl HashParams for Sinks {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.sinks);
    }
}

impl GenEntry for Sinks {
    fn version(&self) -> u32 {
        1
    }
}

impl ShapeModifier<TestContext> for Sinks {
    fn effect(&self) -> ShapeEffect {
        ShapeEffect::LowerOnly(SINK as u32)
    }
    fn slope(&self) -> Slope {
        Slope::ZERO
    }
    fn apply(&self, _: &KindGrid<'_, TestContext>, _: &mut [HeightQ]) {}
    fn lod(
        &self,
        grid: &KindGrid<'_, TestContext>,
        level: u8,
        corners: &mut [HeightQ],
        tops: &mut [i64],
    ) {
        if !self.sinks || level != 1 {
            return;
        }
        // The test realm's datum is 0 and its sea's level y 0: a corner at or over 0 is dry.
        for h in corners.iter_mut().filter(|h| **h >= 0) {
            *h = (i64::from(*h) - SINK) as HeightQ;
        }
        // Each cell's top as low, but over its corners.
        let (nx, nz) = (grid.xs.len() - 1, grid.zs.len() - 1);
        let d = nz + 1;
        for i in 0..nx {
            for j in 0..nz {
                let k = i * d + j;
                let corner = [k, k + 1, k + d, k + d + 1]
                    .map(|c| i64::from(corners[c]))
                    .into_iter()
                    .max()
                    .expect("4 corners");
                let top = &mut tops[i * nz + j];
                *top = (*top - SINK).max(corner);
            }
        }
    }
}

/// The floor under one opaque `test:lamp`: a see-through top (glass) shows the sky at every seam,
/// since the far LOD draws its cells solid.
fn opaque_top<Cx: Context>() -> Decoration<Cx> {
    Decoration::new(
        (id("test:fill"), Arc::new(FloorFill)),
        (
            id("test:surface"),
            Arc::new(TopLayers::new(vec![("test:lamp".into(), 1)])),
        ),
    )
}

/// The test world with region cells of 1024 blocks, an opaque top and `sinks` as a shape
/// modifier.
fn sunk(sinks: bool) -> RegionGenerator<TestContext> {
    generator_with(
        &world(10),
        SEED,
        RegionSettings::default(),
        opaque_top().with_modifier(id("test:sinks"), Arc::new(Sinks { sinks })),
    )
}

/// Chunk columns on a long diagonal, each octant of a node among them.
fn columns() -> Vec<(i64, i64)> {
    (0..12).map(|n| (n * 13 - 70, n * 7 - 41)).collect()
}

/// The first `n` chunk columns of a long diagonal that hold a coast: a column under the sea and a
/// dry one among their corners and middles.
fn coasts(g: &RegionGenerator<TestContext>, n: usize) -> Vec<(i64, i64)> {
    let found: Vec<(i64, i64)> = (0..200 * 200)
        .map(|k| (k % 200 * 5 - 500, k / 200 * 5 - 500))
        .filter(|&(cx, cz)| {
            let wet = [(0, 0), (31, 0), (0, 31), (31, 31), (16, 16)].map(|(dx, dz)| {
                g.column_info((cx << 5) + dx, (cz << 5) + dz)
                    .fluid_top
                    .is_some()
            });
            wet.contains(&true) && wet.contains(&false)
        })
        .take(n)
        .collect();
    assert_eq!(found.len(), n, "coasts");
    found
}

#[test]
fn a_sound_far_form_passes_the_seam_harness() {
    let registry = registry();
    let g = sunk(false);
    let mut chunks = columns();
    chunks.extend(coasts(&g, 4));
    let report = check_seam(&g, &registry, &chunks, SeamOptions::default())
        .check()
        .expect("sound");
    // Every cell column on the 4 planes of each chunk column, both sides, once.
    assert_eq!(report.cells, 16 * 128, "{report}");
    assert_eq!(report.columns, 2 * report.cells, "{report}");
    // The coasts: edge columns wet in both, and rays from under the fluid.
    assert!(report.wet_in_both > 500, "{report}");
    assert!(report.rays > 2 * 2 * 1000 * 16, "{report}");
    // The same on every run.
    assert_eq!(
        check_seam(&g, &registry, &chunks, SeamOptions::default()),
        report
    );
}

#[test]
fn a_dry_corner_lowered_at_level_one_fails() {
    let registry = registry();
    let report = check_seam(&sunk(true), &registry, &columns(), SeamOptions::default());
    assert!(
        report.lowered.len() > 1000 && report.wet_slivers.is_empty(),
        "{report}"
    );
    // The chunks' sides the cells no longer hide: seen from over the cells.
    assert!(report.sky_hits.len() > 1000, "{report}");
    assert!(
        report
            .sky_hits
            .iter()
            .all(|h| h.contains("from over the cells")),
        "{report}"
    );
    let text = report.check().expect_err("lowered");
    assert!(text.contains("- lowered: column") && text.contains("- sky hit: chunk column"));
}

/// Ripples on a sea floor: every column 1 block under the sea's level (y 0), but those with an
/// odd x or z, 1 block over it. The level-1 corners lie at even x and z: every cell is wet, and
/// the dry ground between its corners stands over its water.
struct Ripples;

impl HashParams for Ripples {
    fn hash_params(&self, _: &mut ParamsHasher) {}
}

impl GenEntry for Ripples {
    fn version(&self) -> u32 {
        1
    }
}

impl Ripples {
    const fn height(x: i64, z: i64) -> HeightQ {
        if (x | z) & 1 == 0 { -256 } else { 256 }
    }
}

impl TerrainKind<()> for Ripples {
    /// A step of 2 blocks between neighbors, as jitter.
    fn limits(&self, _: &()) -> KindLimits {
        let range = Span::new(-256, 256);
        KindLimits {
            slope: Slope {
                per_block: Q16::ZERO,
                jitter: 512,
            },
            relief: range,
            band: range,
        }
    }
    fn range(&self, _: &()) -> Span {
        Span::new(-256, 256)
    }
    fn heights(&self, grid: &KindGrid<'_, ()>, _: &[bool], out: &mut [HeightQ]) {
        for (row, &x) in out.chunks_exact_mut(grid.zs.len()).zip(grid.xs) {
            for (h, &z) in row.iter_mut().zip(grid.zs) {
                *h = Self::height(x, z);
            }
        }
    }
}

/// A realm of the one kind `kind` under `()`, with the test world's sea and an opaque top.
fn one_kind(kind: Arc<dyn TerrainKind<()>>) -> RegionGenerator<()> {
    let mut checks = Checks::new();
    let mut realms = GenRegistryBuilder::new("realm");
    realms.register(id("test:one"), realm());
    let mut rules = GenRegistryBuilder::new("selector_rule");
    rules.register(
        id("test:all"),
        SelectorRule {
            realm: id("test:one"),
            priority: 0,
            condition: Vec::new(),
            scope: Scope::CELL,
            entries: vec![Entry::new(id("test:kind"), 1)],
            version: 1,
        },
    );
    let mut kinds = GenRegistryBuilder::new("terrain_kind");
    kinds.register(id("test:kind"), kind);
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
            realm: &id("test:one"),
            realms: &realms,
            rules: &rules,
            entries: &entries,
            kinds: &kinds,
            blocks: &registry(),
            decoration: opaque_top(),
            settings: RegionSettings::default(),
        },
        |_| (),
        &mut checks,
    );
    checks.finish().expect("opens");
    g.expect("no problem")
}

#[test]
fn ground_over_the_fluid_between_the_corners_is_a_wet_sliver() {
    let registry = registry();
    let g = one_kind(Arc::new(Ripples));
    let report = check_seam(&g, &registry, &[(0, 0), (-3, 5)], SeamOptions::default());
    // The edge columns with an odd x or z are dry over wet cells; the others wet in both.
    assert!(
        report.lowered.is_empty() && report.wet_slivers.len() > 100 && report.wet_in_both > 100,
        "{report}"
    );
    assert!(
        report.wet_slivers[0].contains("under its fluid"),
        "{report}"
    );
}

#[test]
fn caves_at_the_seam_show_only_with_the_chunks_as_drawn() {
    let registry = registry();
    // Holes of 4 blocks in one cube of 2 right under the ground (a roof of 1), inland.
    let g = generator_with(
        &world(10),
        SEED,
        RegionSettings::default(),
        opaque_top().with_carver(id("test:holes"), Arc::new(Holes::new(2, 1, 0))),
    );
    let chunks = &columns()[..4];
    let terrain = check_seam(&g, &registry, chunks, SeamOptions::default());
    assert_eq!(terrain.failures(), 0, "{terrain}");
    let drawn = SeamOptions {
        drawn: true,
        ..SeamOptions::default()
    };
    let caves = check_seam(&g, &registry, chunks, drawn);
    // Part A compares the terrain alone either way; the rays reach the holes through the sides
    // the chunks leave out.
    assert!(caves.lowered.is_empty() && caves.wet_slivers.is_empty());
    assert!(caves.sky_hits.len() > 100, "{caves}");
    assert!(
        caves
            .sky_hits
            .iter()
            .all(|h| h.contains("from a cave of the chunk")),
        "{caves}"
    );
}
