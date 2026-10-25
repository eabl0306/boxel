//! The region generator (#119, ADR 0039 "Presupuestos y medición"), on the `test:` content of
//! `tests/support/region.rs` with region cells of the default size (`2^12`):
//! - `select_*`: the selection of one chunk column (its 16 lattice points and its bounds): warm
//!   (memo hits), cold (memo off: 16 points computed), and cold amortized over a fresh 64 x 64
//!   area of chunk columns (each lattice point computed once, as when a world streams in);
//! - `region_*` against `twin_*`: a surface chunk (the ground's), a deep chunk (under the deep
//!   bound) and an air chunk (over the local top) on an interior column (16 points of one kind)
//!   and on a border column (2 or 3 kinds), against the hand-fused single-kind twin of the
//!   column's dominant kind.
//!
//! `kind_heights_*`, `context_grid` and `region_heights_*` split a chunk's cost: one kind's
//! heights over a chunk column, the Context's fields, and the blended heights of the chunk column.
//!
//! `carve_*` (#133): the test world with the test carver (holes of 4 blocks, one cube in 5, under
//! a roof of 6 over an apron of 3, `support/region.rs`) on the interior column: a deep chunk (the
//! fill and the mask, 1 bit), the ground's chunk (the column stage, the carver reading its apron),
//! and the carver's own mask without columns (what the deep chunk pays for the carver itself).
//!
//! `carve_dry_under`, `carve_flood_*` and `carve_surface_sea` (E-C): the same holes with their
//! voids flooded (`Flooded`, a shell of 12 under wet grounds): a deep chunk of the interior
//! column under the level (y -64 to -33) with the holes dry, all its voids flooded (rock and
//! fluid, 1 bit) and flooded under a water table at y -48 (rock, fluid and air: dense); and the
//! ground's chunk of a column under the sea with the holes dry and flooded under that table
//! (the column stage, the flood reading the apron's fluid tops).
//!
//! `carve_band_*` (#135): the same holes and shafts 40 blocks deep in one region of 256 x 256
//! blocks in 3 (`Entrances`), on an interior column away from the shafts: the chunks between the
//! deep bound of the carver's global reach and that of its reach per chunk column, which the
//! first builds through the column stage and the second through the deep path. The bench also
//! prints how many chunks of 32 x 32 chunk columns each builds through the column stage.
//!
//! `feature_*` (#134): the test world with the test trees (one cell of 16 x 16 columns in 12,
//! canopies across chunk borders through an apron of 2) and plants (one dry column in 9) on the
//! interior column: the ground's chunk and the chunk over it.
//!
//! Budgets: selection 0.5 us warm, 2 us cold amortized; interior chunks at most 5 % over the twin;
//! on a border column the machinery (selection and blend: `region_heights_border` less
//! `region_heights_interior` and one `kind_heights_*` per extra kind) at most 4 us per extra kind;
//! an air chunk over the local top at most 2 us.
#[path = "../tests/support/region.rs"]
mod support;

use boxel_voxel::{ChunkKey, ChunkPos};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::region::{
    CarveChunk, CarverStage, Context, KindGrid, RegionGenerator, RegionSettings, TerrainKind,
    clear_worker_memo,
};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::*;

/// The distinct kinds of the window of chunk column `(cx, cz)`.
fn window_kinds(g: &RegionGenerator<TestContext>, cx: i64, cz: i64) -> usize {
    let s = g.lattice() - 5;
    let (i0, j0) = (cx >> s, cz >> s);
    let mut kinds: Vec<_> = (0..16)
        .map(|n| g.lattice_kind(i0 - 1 + n / 4, j0 - 1 + n % 4).clone())
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds.len()
}

/// The first chunk column from `(cx, cz)` along +x whose window holds `kinds` kinds (1: interior)
/// and whose dominant kind is not the basin (under the sea everywhere).
fn find(g: &RegionGenerator<TestContext>, (cx, cz): (i64, i64), kinds: usize) -> (i64, i64) {
    (0..1 << 16)
        .map(|n| (cx + n * 3, cz + (n % 7) * 5))
        .find(|&(x, z)| {
            let ok = if kinds == 1 {
                window_kinds(g, x, z) == 1
            } else {
                window_kinds(g, x, z) >= kinds
            };
            ok && g
                .column_info(x << 5, z << 5)
                .kind
                .is_some_and(|k| k.as_str() != "test:basin")
        })
        .expect("a column")
}

fn key(x: i64, y: i64, z: i64) -> ChunkKey {
    ChunkKey::new(ChunkPos::new(x, y, z))
}

#[expect(
    clippy::print_stderr,
    reason = "the bench names the columns it measures"
)]
fn bench(c: &mut Criterion) {
    let world = world(12);
    let g = generator(&world, SEED, RegionSettings::default());
    let cold = generator(
        &world,
        SEED,
        RegionSettings {
            memo: false,
            ..RegionSettings::default()
        },
    );
    let registry = registry();

    let (col_x, col_z) = find(&g, (0, 0), 1);
    c.bench_function("select_warm", |b| {
        b.iter(|| g.local_bounds(black_box(col_x), col_z));
    });
    c.bench_function("query_column_warm", |b| {
        b.iter(|| g.column_info(black_box(col_x << 5), col_z << 5));
    });
    c.bench_function("select_cold", |b| {
        b.iter(|| cold.local_bounds(black_box(col_x), col_z));
    });
    c.bench_function("select_cold_amortized_per_column", |b| {
        b.iter_custom(|iters| {
            // Each iteration a fresh area (memos never hold it), 64 x 64 chunk columns.
            let mut total = Duration::ZERO;
            let per = 64 * 64;
            let areas = iters.div_ceil(per);
            for area in 0..areas {
                clear_worker_memo();
                let (x0, z0) = (area as i64 * 4096 + 1_000_000, -(area as i64) * 4096);
                let start = Instant::now();
                for n in 0..per as i64 {
                    black_box(g.local_bounds(x0 + n / 64, z0 + n % 64));
                }
                total += start.elapsed();
            }
            total.mul_f64(iters as f64 / (areas * per) as f64)
        });
    });

    // One kind's heights over a chunk column (what a border column pays again per extra kind).
    let (xs, zs): (Vec<i64>, Vec<i64>) = ((0..32).collect(), (0..32).collect());
    let fields = g.context().grid(&xs, &zs);
    for (name, kind) in kinds() {
        let grid = KindGrid {
            cx: g.context(),
            fields: &fields,
            xs: &xs,
            zs: &zs,
            seed: SEED,
        };
        let mut out = [0; 1024];
        c.bench_function(&format!("kind_heights_{}", &name[5..]), |b| {
            b.iter(|| kind.heights(black_box(&grid), &[true; 1024], &mut out));
        });
    }
    c.bench_function("context_grid", |b| {
        b.iter(|| g.context().grid(black_box(&xs), &zs));
    });

    for (name, n_kinds) in [("interior", 1), ("border", 2)] {
        let (cx, cz) = find(&g, (40, -40), n_kinds);
        let info = g.column_info(cx << 5, cz << 5);
        let dominant = info.kind.expect("a kind");
        let (kind_name, relief) = kinds()
            .into_iter()
            .find(|(n, _)| *n == dominant.as_str())
            .expect("a test kind");
        let twin = Twin::new(kind_name, relief, SEED, g.lattice());
        let bounds = g.local_bounds(cx, cz);
        // Each its own surface chunk (the twin's ground differs from the blend's at a border).
        let twin_ground = twin.ground(cx << 5, cz << 5);
        let layers = [
            ("surface", info.ground >> 5, twin_ground >> 5),
            ("deep", (bounds.deep >> 5) - 1, (bounds.deep >> 5) - 1),
            ("air", (bounds.top >> 5) + 1, (bounds.top >> 5) + 1),
        ];
        for (chunk, layer, twin_layer) in layers {
            let (k, t) = (key(cx, layer, cz), key(cx, twin_layer, cz));
            c.bench_function(&format!("region_{chunk}_{name}"), |b| {
                b.iter(|| g.generate(black_box(k), &registry));
            });
            c.bench_function(&format!("twin_{chunk}_{name}"), |b| {
                b.iter(|| twin.generate(black_box(t), &registry));
            });
        }
        let mut heights = [0; 1024];
        c.bench_function(&format!("region_heights_{name}"), |b| {
            b.iter(|| g.chunk_heights(black_box(cx), cz, &mut heights));
        });
        eprintln!(
            "{name} column ({cx}, {cz}): {} kinds, ground {} (twin {twin_ground}), {:?}",
            window_kinds(&g, cx, cz),
            info.ground,
            bounds
        );
    }

    // The carver, on the interior column.
    let holes = Holes::new(5, 6, 3);
    let carved = generator_with(
        &world,
        SEED,
        RegionSettings::default(),
        decoration().with_carver(id("test:holes"), Arc::new(holes)),
    );
    let (cx, cz) = find(&carved, (40, -40), 1);
    let ground = carved.column_info(cx << 5, cz << 5).ground;
    let bounds = carved.local_bounds(cx, cz);
    let (deep, surface) = (
        key(cx, (bounds.deep >> 5) - 1, cz),
        key(cx, ground >> 5, cz),
    );
    c.bench_function("carve_deep_interior", |b| {
        b.iter(|| carved.generate(black_box(deep), &registry));
    });
    c.bench_function("carve_surface_interior", |b| {
        b.iter(|| carved.generate(black_box(surface), &registry));
    });
    let solid = registry.by_name("test:solid").expect("test:solid");
    let fluid = registry.by_name("test:fluid").expect("test:fluid");
    c.bench_function("carve_mask_deep", |b| {
        b.iter(|| {
            let chunk = CarveChunk {
                cx: carved.context(),
                seed: SEED,
                origin: black_box(deep).pos.origin(),
                columns: None,
                apron_lo: bounds.apron_lo,
                floor: solid,
                fluid,
                level: Some(0),
                registry: &registry,
            };
            CarverStage::<TestContext>::carve(&holes, &chunk)
        });
    });
    eprintln!("carved interior column ({cx}, {cz}): ground {ground}, {bounds:?}");

    // The same holes with their voids flooded (E-C).
    let flooded = |table| {
        generator_with(
            &world,
            SEED,
            RegionSettings::default(),
            decoration().with_carver(id("test:holes"), Arc::new(Flooded::new(holes, table, 12))),
        )
    };
    let (all, across) = (flooded(i64::MAX), flooded(-48));
    let under = key(cx, -2, cz);
    c.bench_function("carve_dry_under", |b| {
        b.iter(|| carved.generate(black_box(under), &registry));
    });
    c.bench_function("carve_flood_deep", |b| {
        b.iter(|| all.generate(black_box(under), &registry));
    });
    c.bench_function("carve_flood_across", |b| {
        b.iter(|| across.generate(black_box(under), &registry));
    });
    let (sx, sz) = (0..1 << 16)
        .map(|n| (cx + n * 3, cz + (n % 7) * 5))
        .find(|&(x, z)| carved.column_info(x << 5, z << 5).fluid_top.is_some())
        .expect("a column under the sea");
    let sea_ground = carved.column_info(sx << 5, sz << 5).ground;
    let sea = key(sx, sea_ground >> 5, sz);
    c.bench_function("carve_surface_sea", |b| {
        b.iter(|| carved.generate(black_box(sea), &registry));
    });
    c.bench_function("carve_flood_surface", |b| {
        b.iter(|| across.generate(black_box(sea), &registry));
    });
    eprintln!(
        "flooded chunk under the level {under:?}; column under the sea ({sx}, {sz}): ground \
         {sea_ground}"
    );

    // The carver with shafts in some regions only, its reach global or per chunk column.
    let shafts = |declares| {
        generator_with(
            &world,
            SEED,
            RegionSettings::default(),
            decoration().with_carver(
                id("test:entrances"),
                Arc::new(Entrances::new(holes, 40, declares)),
            ),
        )
    };
    let (global, regional) = (shafts(Declares::Global), shafts(Declares::Regional));
    let (cx, cz) = (0..)
        .map(|n| find(&regional, (40 + 9 * n, -40), 1))
        .find(|&(x, z)| regional.local_bounds(x, z).deep > global.local_bounds(x, z).deep)
        .expect("an interior column away from the shafts");
    let (g, r) = (global.local_bounds(cx, cz), regional.local_bounds(cx, cz));
    let band: Vec<ChunkKey> = (((g.deep - 31) >> 5) + 1..=(r.deep - 31) >> 5)
        .map(|y| key(cx, y, cz))
        .collect();
    c.bench_function("carve_band_global", |b| {
        b.iter(|| {
            for &k in &band {
                black_box(global.generate(black_box(k), &registry));
            }
        });
    });
    c.bench_function("carve_band_regional", |b| {
        b.iter(|| {
            for &k in &band {
                black_box(regional.generate(black_box(k), &registry));
            }
        });
    });
    eprintln!(
        "shafts away at ({cx}, {cz}): {} chunks between the deep bounds {} and {}; column stage \
         chunks of 32 x 32 chunk columns: {} global, {} per chunk column",
        band.len(),
        g.deep,
        r.deep,
        column_stage_chunks(&global, (cx - 16, cz - 16)),
        column_stage_chunks(&regional, (cx - 16, cz - 16)),
    );

    // The feature stages, on the interior column.
    let featured = generator_with(
        &world,
        SEED,
        RegionSettings::default(),
        decoration()
            .with_feature(id("test:groves"), Arc::new(Groves::new(12, 6)))
            .with_feature(id("test:tufts"), Arc::new(Tufts { one_in: 9 })),
    );
    let (cx, cz) = find(&featured, (40, -40), 1);
    let ground = featured.column_info(cx << 5, cz << 5).ground;
    let bounds = featured.local_bounds(cx, cz);
    let (surface, over) = (key(cx, ground >> 5, cz), key(cx, (ground >> 5) + 1, cz));
    c.bench_function("feature_surface_interior", |b| {
        b.iter(|| featured.generate(black_box(surface), &registry));
    });
    c.bench_function("feature_over_interior", |b| {
        b.iter(|| featured.generate(black_box(over), &registry));
    });
    eprintln!("featured interior column ({cx}, {cz}): ground {ground}, {bounds:?}");
}

criterion_group!(benches, bench);
criterion_main!(benches);
