//! Grid against point for the noise kit and the field combinators (#67): every `*_point` and
//! `*_grid` pair computes the same 32 x 32 values (one chunk column of samples), near the origin
//! and at `2^40` (`*_far`, where the lattice cells of the noise leave `i32`).
use boxel_worldgen::exact::point_segment_distance;
use boxel_worldgen::field::{
    Add, Blend, Const, Field, LatticeField, Min, NoiseField, Rect, Spline, Splined,
};
use boxel_worldgen::noise::{
    Basis, Cellular, Fbm, Frequency, Warp, fbm_2d, fbm_2d_grid, gradient_fbm_2d,
    gradient_fbm_2d_grid,
};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

const FAR: i64 = 1 << 40;

/// 32 consecutive coordinates from `origin`.
fn axis(origin: i64) -> Vec<i64> {
    (origin..origin + 32).collect()
}

/// Times `point` once per sample of the 32 x 32 column at `origin` and `grid` once.
fn pair<T: Default + Clone>(
    c: &mut Criterion,
    name: &str,
    origin: i64,
    point: impl Fn(i64, i64) -> T,
    grid: impl Fn(&[i64], &[i64], &mut [T]),
) {
    let (xs, zs) = (axis(origin), axis(-origin));
    c.bench_function(&format!("{name}_point"), |b| {
        b.iter(|| {
            let mut last = T::default();
            for &x in &xs {
                for &z in &zs {
                    last = point(black_box(x), z);
                }
            }
            last
        });
    });
    c.bench_function(&format!("{name}_grid"), |b| {
        let mut out = vec![T::default(); xs.len() * zs.len()];
        b.iter(|| {
            grid(black_box(&xs), &zs, &mut out);
            out[0].clone()
        });
    });
}

fn bench(c: &mut Criterion) {
    let terrain = Fbm {
        frequency: Frequency::one_in(256),
        octaves: 5,
        lacunarity: 2,
        gain: 0.5,
    };
    for (suffix, o) in [("", 0), ("_far", FAR)] {
        pair(
            c,
            &format!("noise/value_fbm{suffix}"),
            o,
            |x, z| fbm_2d(7, x, z, terrain),
            |xs, zs, out| fbm_2d_grid(7, xs, zs, terrain, out),
        );
        pair(
            c,
            &format!("noise/gradient_fbm{suffix}"),
            o,
            |x, z| gradient_fbm_2d(7, x, z, terrain),
            |xs, zs, out| gradient_fbm_2d_grid(7, xs, zs, terrain, out),
        );
    }

    let meander = Fbm {
        frequency: Frequency::one_in(512),
        octaves: 3,
        ..terrain
    };
    let warp = Warp::new(7, meander, 48);
    pair(
        c,
        "noise/warp",
        0,
        |x, z| warp.offset(x, z),
        |xs, zs, out| warp.offset_grid(xs, zs, out),
    );

    let cells = Cellular::new(7, 8, 0);
    let first = cells.nearest(0, 0);
    pair(
        c,
        "noise/cellular",
        0,
        |x, z| cells.nearest(x, z).d1,
        |xs, zs, out| {
            let mut near = vec![first; out.len()];
            cells.nearest_grid(xs, zs, &mut near);
            for (o, n) in out.iter_mut().zip(near) {
                *o = n.d1;
            }
        },
    );
    let plates = Cellular::new(7, 15, 1 << 15 >> 2);
    c.bench_function("noise/voronoi_cell", |b| {
        b.iter(|| plates.voronoi_cell(black_box(plates.cell_of(0, 0))));
    });

    // 1024 distances to a 4 km segment, near the origin and next to the world limit.
    for (suffix, o) in [("", 0), ("_far", (1 << 61) - (1 << 20))] {
        let (a, b) = ((o - 1000, o + 300), (o + 3000, o - 2500));
        c.bench_function(&format!("exact/point_segment{suffix}"), |bench| {
            bench.iter(|| {
                let mut nearest = point_segment_distance((o, o), a, b);
                for i in 0..32 {
                    for j in 0..32 {
                        let p = (o + i * 97 - 1500, o + j * 89 - 1500);
                        nearest = nearest.min(point_segment_distance(black_box(p), a, b));
                    }
                }
                nearest
            });
        });
    }

    let height = Spline::new(&[(-1000, -200), (-100, 0), (0, 300), (400, 350), (1000, 2000)]);
    c.bench_function("field/spline_eval_1024", |b| {
        b.iter(|| {
            (-512..512)
                .map(|v| height.eval(black_box(v) * 3))
                .sum::<i64>()
        });
    });

    // The fields: a 32-block lattice of gradient noise, and a tree with every combinator.
    let noise = NoiseField::new(7, terrain, Basis::Gradient, 1000);
    let lattice = LatticeField::new(5, noise);
    let weight = LatticeField::new(
        8,
        Splined::new(
            NoiseField::new(8, terrain, Basis::Gradient, 300),
            Spline::new(&[(-300, 0), (300, 1 << 16)]),
        ),
    );
    let relief = Add::new(
        LatticeField::new(2, NoiseField::new(9, meander, Basis::Gradient, 40)),
        Splined::new(&lattice, height.clone()),
    );
    let tree = Blend::new(&weight, Min::new(&relief, Const(1500)), Const(64), 16);
    for (suffix, o) in [("", 0), ("_far", FAR)] {
        pair(
            c,
            &format!("field/noise{suffix}"),
            o,
            |x, z| noise.value(x, z),
            |xs, zs, out| noise.grid(xs, zs, out),
        );
        pair(
            c,
            &format!("field/lattice{suffix}"),
            o,
            |x, z| lattice.value(x, z),
            |xs, zs, out| lattice.grid(xs, zs, out),
        );
        pair(
            c,
            &format!("field/tree{suffix}"),
            o,
            |x, z| tree.value(x, z),
            |xs, zs, out| tree.grid(xs, zs, out),
        );
    }
    // The bounds a far LOD cell asks for: a level-4 cell (16 blocks).
    let cell = Rect::cell(48, 80, 16);
    c.bench_function("field/tree_bound_and_fine", |b| {
        b.iter(|| (tree.bound(black_box(cell)), tree.fine(cell)));
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
