//! The far LOD's region pass (#120, ADR 0039 "Presupuestos y medición"), on the `test:` content
//! of `tests/support/region.rs` with region cells of the default size (`2^12`), whose kinds carry
//! a detail term of 4 more octaves (6 octaves of gradient noise per kind, about 38 ns a corner,
//! over the mask's bounding box; the Context is the test one, a 16-block lattice over 3
//! octaves):
//!
//! - `source_{interior,border2,border3}_l{1,4,6}`: one source call for a node column of 1, 2 and 3
//!   kinds at levels 1, 4 and 6, its band's layers, with the corner memo missed (the call before
//!   was for the next column; the lattice table stays warm, as it does when a level fills in);
//! - `job_*`: the same call and, as a LOD job does, the fill, the mesher and the LOD post-passes
//!   for each node with cells (at most 3, `MAX_NODES_PER_JOB`);
//! - `cold_*`: the source call on an empty scratch (no memo at all: the registry's classes, the
//!   lattice points);
//! - `runs_source_*` and `runs_job_*` (#134): `source_*` and `job_*` with a test feature stage
//!   whose far LOD stacks a run of 6 blocks over every dry cell (the test trees of
//!   `tests/support/region.rs`): what the feature runs add to a node;
//! - `coarse_*` (levels 4 and 6): the source call with the kinds' coarse form, which leaves the
//!   detail out from level 3 (the game's lever for its kinds' cost far away);
//! - `machinery_*`: the source call with kinds of no cost of their own (the Context's base plus
//!   an offset): the Context and the region pass alone, the engine's share;
//! - `corners_context` and `corners_kind`: the Context's fields and one kind's heights over the
//!   35 x 35 corners, the part of a node that is the game's.
//!
//! Budget (ADR 0039): the source at most 0.15 ms and the job at most 0.4 ms at levels 1 to 4 on
//! interior, 2-kind and 3-kind nodes; levels 5 and 6 recorded against the same numbers.
#[path = "../tests/support/region.rs"]
mod support;

use boxel_mesh::lod::{LodMaterials, mesh_node};
use boxel_mesh::{ChunkMesh, Mesher, PaddedChunk};
use boxel_voxel::DimensionId;
use boxel_worldgen::field::{Field, NoiseField, Rect, Span};
use boxel_worldgen::lod::{LodNode, LodRequest, LodScratch, LodSource};
use boxel_worldgen::noise::{Basis, mix_seed};
use boxel_worldgen::region::{Context, HeightQ, KindGrid, KindLimits, RegionSettings, TerrainKind};
use boxel_worldgen::registry::{GenEntry, HashParams, ParamsHasher};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::*;

/// The most nodes with cells a LOD job meshes (`boxel_world::lod::jobs::MAX_NODES_PER_JOB`).
const NODES_PER_JOB: usize = 3;

/// A test kind with a detail term: 4 octaves of gradient noise of a block, for the cost. With
/// `coarse`, its far LOD leaves the detail out from level 3 (its octaves are shorter than 8
/// cells there): a coarse `lod_heights`, as a game writes one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Detailed(Relief, bool);

impl Detailed {
    fn detail(seed: u64) -> NoiseField {
        NoiseField::new(mix_seed(seed, 0xde7a11), fbm(64, 4), Basis::Gradient, 256)
    }
}

impl HashParams for Detailed {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0).write(&self.1);
    }
}

impl GenEntry for Detailed {
    fn version(&self) -> u32 {
        1
    }
}

impl TerrainKind<TestContext> for Detailed {
    fn limits(&self, cx: &TestContext) -> KindLimits {
        let l = self.0.limits(cx);
        KindLimits {
            slope: l.slope.sum(Self::detail(0).slope()),
            relief: Span::new(l.relief.lo - 256, l.relief.hi + 256),
            band: Span::new(l.band.lo - 256, l.band.hi + 256),
        }
    }
    fn range(&self, cx: &TestContext) -> Span {
        let r = self.0.range(cx);
        Span::new(r.lo - 256, r.hi + 256)
    }
    fn bounds(&self, cx: &TestContext, seed: u64, q: (i64, i64), reach: i64) -> Span {
        let b = self.0.bounds(cx, seed, q, reach);
        Span::new(b.lo - 256, b.hi + 256)
    }
    /// Without the detail from level 3 when coarse: its octaves (64 to 8 blocks) are shorter than
    /// 8 cells there. The margin is the relief's slope over half a cell's diagonal plus the
    /// detail's amplitude, which is also what the coarse corners may stand over the real ones.
    fn lod_heights(
        &self,
        grid: &KindGrid<'_, TestContext>,
        level: u8,
        mask: &[bool],
        out: &mut [HeightQ],
        margin: &mut [HeightQ],
    ) {
        if !self.1 || level < 3 {
            self.heights(grid, mask, out);
            let half = Rect::cell(0, 0, 1 << level).half_diagonal();
            margin.fill(self.limits(grid.cx).slope.over(half) as HeightQ);
            return;
        }
        self.0.heights(grid, mask, out);
        let half = Rect::cell(0, 0, 1 << level).half_diagonal();
        margin.fill((self.0.limits(grid.cx).slope.over(half) + 256) as HeightQ);
    }

    fn lod_left_out(&self, _cx: &TestContext, level: u8) -> HeightQ {
        if self.1 && level >= 3 { 256 } else { 0 }
    }

    /// The grid forms over the mask's bounding box (a far LOD node at a border asks only for the
    /// corners its kind reaches).
    fn heights(&self, grid: &KindGrid<'_, TestContext>, mask: &[bool], out: &mut [HeightQ]) {
        let nz = grid.zs.len();
        let (mut a, mut b) = ((usize::MAX, 0), (usize::MAX, 0));
        for (n, _) in mask.iter().enumerate().filter(|(_, m)| **m) {
            let (i, j) = (n / nz, n % nz);
            a = (a.0.min(i), a.1.max(i));
            b = (b.0.min(j), b.1.max(j));
        }
        if a.0 > a.1 {
            return;
        }
        let (xs, zs) = (&grid.xs[a.0..=a.1], &grid.zs[b.0..=b.1]);
        let (mut noise, mut detail) = (vec![0; xs.len() * zs.len()], vec![0; xs.len() * zs.len()]);
        self.0.noise(grid.seed).grid(xs, zs, &mut noise);
        Self::detail(grid.seed).grid(xs, zs, &mut detail);
        for (i, (n, d)) in noise
            .chunks_exact(zs.len())
            .zip(detail.chunks_exact(zs.len()))
            .enumerate()
        {
            let row = (a.0 + i) * nz + b.0;
            for (j, (n, d)) in n.iter().zip(d).enumerate() {
                out[row + j] = (grid.fields.base[row + j] + self.0.offset + n + d) as HeightQ;
            }
        }
    }
}

/// A kind with no cost of its own: the Context's base plus an offset (the region pass's share).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Plain(Relief);

impl HashParams for Plain {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0);
    }
}

impl GenEntry for Plain {
    fn version(&self) -> u32 {
        1
    }
}

impl TerrainKind<TestContext> for Plain {
    fn limits(&self, cx: &TestContext) -> KindLimits {
        self.0.limits(cx)
    }
    fn range(&self, cx: &TestContext) -> Span {
        self.0.range(cx)
    }
    fn bounds(&self, cx: &TestContext, seed: u64, q: (i64, i64), reach: i64) -> Span {
        self.0.bounds(cx, seed, q, reach)
    }
    fn heights(&self, grid: &KindGrid<'_, TestContext>, _mask: &[bool], out: &mut [HeightQ]) {
        for (o, &b) in out.iter_mut().zip(&grid.fields.base) {
            *o = (b + self.0.offset) as HeightQ;
        }
    }
}

fn request(level: u8, column: [i64; 2]) -> LodRequest {
    LodRequest {
        dim: DimensionId(0),
        level,
        column,
        layers: -64..=64,
    }
}

#[expect(clippy::print_stderr, reason = "the bench names the nodes it measures")]
fn bench(c: &mut Criterion) {
    let registry = registry();
    let detailed: Vec<(&str, Arc<dyn TerrainKind<TestContext>>)> = kinds()
        .into_iter()
        .map(|(name, relief)| {
            (
                name,
                Arc::new(Detailed(relief, false)) as Arc<dyn TerrainKind<TestContext>>,
            )
        })
        .collect();
    let world = world_of(
        vec![("test:surface", realm())],
        rules(12),
        Vec::new(),
        detailed,
    );
    let g = generator(&world, SEED, RegionSettings::default());
    let coarse: Vec<(&str, Arc<dyn TerrainKind<TestContext>>)> = kinds()
        .into_iter()
        .map(|(name, relief)| {
            (
                name,
                Arc::new(Detailed(relief, true)) as Arc<dyn TerrainKind<TestContext>>,
            )
        })
        .collect();
    let coarse_world = world_of(
        vec![("test:surface", realm())],
        rules(12),
        Vec::new(),
        coarse,
    );
    let coarse = generator(&coarse_world, SEED, RegionSettings::default());
    let plain: Vec<(&str, Arc<dyn TerrainKind<TestContext>>)> = kinds()
        .into_iter()
        .map(|(name, relief)| {
            (
                name,
                Arc::new(Plain(relief)) as Arc<dyn TerrainKind<TestContext>>,
            )
        })
        .collect();
    let plain_world = world_of(
        vec![("test:surface", realm())],
        rules(12),
        Vec::new(),
        plain,
    );
    let plain = generator(&plain_world, SEED, RegionSettings::default());
    // The detailed world with the test trees and their far LOD run on every dry cell.
    let runs = generator_with(
        &world,
        SEED,
        RegionSettings::default(),
        decoration().with_feature(id("test:groves"), Arc::new(Groves::new(12, 6))),
    );
    let materials = LodMaterials::from_registry(&registry);
    let mut padded = PaddedChunk::new();
    let mut mesher = Mesher::new();
    let mut mesh = ChunkMesh::default();
    let mut scratch = LodScratch::default();

    // The part of a node that is the game's: the Context and one kind over the corners.
    let level = 4u8;
    let s = 1i64 << level;
    let xs: Vec<i64> = (0..35).map(|i| 4000 + i * s).collect();
    let zs: Vec<i64> = (0..35).map(|j| -900 + j * s).collect();
    c.bench_function("lod/corners_context", |b| {
        b.iter(|| g.context().lod_grid(black_box(&xs), &zs, level));
    });
    let fields = g.context().lod_grid(&xs, &zs, level);
    let kind = Detailed(kinds_relief("test:hills"), false);
    let grid = KindGrid {
        cx: g.context(),
        fields: &fields,
        xs: &xs,
        zs: &zs,
        seed: SEED,
    };
    let (mut out, mut margin) = (vec![0; 35 * 35], vec![0; 34 * 34]);
    c.bench_function("lod/corners_kind", |b| {
        b.iter(|| {
            kind.lod_heights(
                black_box(&grid),
                level,
                &[true; 35 * 35],
                &mut out,
                &mut margin,
            );
        });
    });

    let mut g_bench = c.benchmark_group("lod");
    g_bench
        .sample_size(40)
        .warm_up_time(Duration::from_millis(800))
        .measurement_time(Duration::from_secs(3));
    for level in [1u8, 4, 6] {
        for (name, n_kinds) in [("interior", 1), ("border2", 2), ("border3", 3)] {
            let node = nodes_with(&g, level, n_kinds, [7, -11], 1)[0];
            let next = [node[0] + 1, node[1]];
            let (req, other) = (request(level, node), request(level, next));
            // Returns the nodes with cells and their quads, as a job meshes them.
            let mut job = |scratch: &mut LodScratch| {
                let (mut nodes, mut quads) = (0, 0);
                g.column(&req, &registry, scratch, &mut |_, node| {
                    if let LodNode::Cells(cells) = node
                        && nodes < NODES_PER_JOB
                    {
                        materials.fill(&cells.0[..], &mut padded);
                        mesh_node(&mut mesher, &padded, &materials, level, &mut mesh);
                        nodes += 1;
                        quads += mesh.quads.len();
                    }
                });
                (nodes, quads)
            };
            let (nodes, quads) = job(&mut scratch);
            let band = g.column(&req, &registry, &mut scratch, &mut |_, _| {});
            eprintln!(
                "lod l{level} {name}: node {node:?}, {} kinds, band {}..{}, {nodes} nodes \
                 meshed, {quads} quads",
                node_kinds(&g, level, node),
                band.lo,
                band.hi
            );
            let source = |scratch: &mut LodScratch| {
                let mut cells = 0;
                g.column(black_box(&req), &registry, scratch, &mut |_, node| {
                    cells += usize::from(matches!(node, LodNode::Cells(_)));
                });
                cells
            };
            // The call before is for the next column: the corner memo misses.
            let miss = |scratch: &mut LodScratch| {
                g.column(&other, &registry, scratch, &mut |_, _| {});
            };
            g_bench.bench_function(format!("source_{name}_l{level}"), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        miss(&mut scratch);
                        let start = Instant::now();
                        black_box(source(&mut scratch));
                        total += start.elapsed();
                    }
                    total
                });
            });
            g_bench.bench_function(format!("job_{name}_l{level}"), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        miss(&mut scratch);
                        let start = Instant::now();
                        black_box(job(&mut scratch));
                        total += start.elapsed();
                    }
                    total
                });
            });
            // The same node with the feature runs.
            let mut runs_job = |scratch: &mut LodScratch| {
                let (mut nodes, mut quads) = (0, 0);
                runs.column(&req, &registry, scratch, &mut |_, node| {
                    if let LodNode::Cells(cells) = node
                        && nodes < NODES_PER_JOB
                    {
                        materials.fill(&cells.0[..], &mut padded);
                        mesh_node(&mut mesher, &padded, &materials, level, &mut mesh);
                        nodes += 1;
                        quads += mesh.quads.len();
                    }
                });
                (nodes, quads)
            };
            let (nodes, quads) = runs_job(&mut scratch);
            let band = runs.column(&req, &registry, &mut scratch, &mut |_, _| {});
            eprintln!(
                "lod l{level} {name} with the runs: band {}..{}, {nodes} nodes meshed, {quads} quads",
                band.lo, band.hi
            );
            g_bench.bench_function(format!("runs_source_{name}_l{level}"), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        runs.column(&other, &registry, &mut scratch, &mut |_, _| {});
                        let start = Instant::now();
                        runs.column(black_box(&req), &registry, &mut scratch, &mut |_, _| {});
                        total += start.elapsed();
                    }
                    total
                });
            });
            g_bench.bench_function(format!("runs_job_{name}_l{level}"), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        runs.column(&other, &registry, &mut scratch, &mut |_, _| {});
                        let start = Instant::now();
                        black_box(runs_job(&mut scratch));
                        total += start.elapsed();
                    }
                    total
                });
            });
            // The same node with the kinds' coarse form (from level 3).
            if level >= 3 {
                g_bench.bench_function(format!("coarse_{name}_l{level}"), |b| {
                    b.iter_custom(|iters| {
                        let mut total = Duration::ZERO;
                        for _ in 0..iters {
                            coarse.column(&other, &registry, &mut scratch, &mut |_, _| {});
                            let start = Instant::now();
                            coarse.column(black_box(&req), &registry, &mut scratch, &mut |_, _| {});
                            total += start.elapsed();
                        }
                        total
                    });
                });
            }
            // The same node with kinds of no cost: the Context and the region pass alone.
            g_bench.bench_function(format!("machinery_{name}_l{level}"), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        plain.column(&other, &registry, &mut scratch, &mut |_, _| {});
                        let start = Instant::now();
                        plain.column(black_box(&req), &registry, &mut scratch, &mut |_, _| {});
                        total += start.elapsed();
                    }
                    total
                });
            });
            g_bench.bench_function(format!("cold_{name}_l{level}"), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let mut fresh = LodScratch::default();
                        let start = Instant::now();
                        black_box(source(&mut fresh));
                        total += start.elapsed();
                    }
                    total
                });
            });
        }
    }
    g_bench.finish();
}

/// The relief of the test kind `name`.
fn kinds_relief(name: &str) -> Relief {
    kinds()
        .into_iter()
        .find(|(n, _)| *n == name)
        .expect("a test kind")
        .1
}

criterion_group!(benches, bench);
criterion_main!(benches);
