//! Cave generation benchmarks. The 3D noise over one chunk's density lattice: 9x5x9 corners
//! every 4x8x4 voxels, with the cheese caverns' fbm (3 octaves, 1/128 across, 3/256 up), near
//! the origin and far from it (`*_far`: x = 2^40, y = -2^40), against 405 point calls.
use boxel_worldgen::Frequency;
use boxel_worldgen::noise::{Fbm3, Frequency3, fbm_3d, fbm_3d_grid};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// The density lattice of the chunk whose lowest corner is `origin`: 9 x 5 x 9 world coordinates.
fn lattice(origin: (i64, i64, i64)) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
    let axis = |o: i64, step: i64, n: i64| (0..n).map(|i| o + i * step).collect();
    (
        axis(origin.0, 4, 9),
        axis(origin.1, 8, 5),
        axis(origin.2, 4, 9),
    )
}

fn bench(c: &mut Criterion) {
    let cheese = Fbm3 {
        frequency: Frequency3 {
            xz: Frequency::one_in(128),
            y: Frequency::new(3, 256),
        },
        octaves: 3,
        lacunarity: 2,
        gain: 0.5,
    };
    let seed = 0x00b0_7e1d;
    let far = 1i64 << 40;
    let mut out = [0.0; 9 * 5 * 9];

    let mut g = c.benchmark_group("caves");
    // Chunk (1, -3, 2), and the chunk at x = 2^40, y = -2^40.
    for (name, origin) in [
        ("fbm_3d_grid_9x5x9", (32, -96, 64)),
        ("fbm_3d_grid_9x5x9_far", (far, -far, 64)),
    ] {
        let (xs, ys, zs) = lattice(origin);
        g.bench_function(name, |b| {
            b.iter(|| {
                fbm_3d_grid(black_box(seed), black_box(&xs), &ys, &zs, cheese, &mut out);
                out[404]
            });
        });
    }
    let (xs, ys, zs) = lattice((32, -96, 64));
    g.bench_function("fbm_3d_points_9x5x9", |b| {
        b.iter(|| {
            let mut sum = 0.0;
            for &x in black_box(&xs) {
                for &y in &ys {
                    for &z in &zs {
                        sum += fbm_3d(seed, x, y, z, cheese);
                    }
                }
            }
            sum
        });
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
