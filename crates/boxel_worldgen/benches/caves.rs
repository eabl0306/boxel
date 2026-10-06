//! Cave generation benchmarks.
//!
//! - `fbm_3d_*`: the 3D noise over one chunk's density lattice (9x5x9 corners every 4x8x4
//!   voxels) with the cheese caverns' fbm (3 octaves, 1/128 across, 3/256 up), near the origin
//!   and far from it (`*_far`: x = 2^40, y = -2^40), against 405 point calls.
//! - `OverworldGenerator::generate` with the default caves: a deep chunk with caves
//!   (`cave_chunk`, and `cave_chunk_far` at x = 2^40, y = -2^40), the mean of 64 consecutive
//!   deep chunks (`deep_chunks_mean`, per chunk), a deep chunk the caves leave whole
//!   (`solid_chunk`), the surface chunk at column (0, 0) with caves and trees
//!   (`surface_chunk_caves`), and the 405 corner densities alone (`density_corners_405`).
//!
//! Phase 2 targets: `cave_chunk` 60 us or less (aim 40), `cave_chunk_far` within 1.3x of it,
//! `solid_chunk` 25 us or less, `surface_chunk_caves` 90 us or less.
#![expect(clippy::print_stderr, reason = "bench scene statistics")]

use boxel_voxel::{BlockRegistry, BlockState, CHUNK_VOLUME, ChunkKey, ChunkPos, Paletted};
use boxel_worldgen::noise::{Fbm3, Frequency3, fbm_3d, fbm_3d_grid};
use boxel_worldgen::{Frequency, OverworldGenerator, WorldGenerator};
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

fn noise(c: &mut Criterion) {
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

/// Air share of a chunk.
fn air(g: &OverworldGenerator, r: &BlockRegistry, pos: ChunkPos) -> f64 {
    let mut d = vec![BlockState::AIR; CHUNK_VOLUME];
    g.generate(ChunkKey::new(pos), r).to_dense(&mut d);
    d.iter().filter(|s| **s == BlockState::AIR).count() as f64 / CHUNK_VOLUME as f64
}

/// The first chunk along +x from `pos` whose generation passes `pick`.
fn find(
    g: &OverworldGenerator,
    r: &BlockRegistry,
    pos: ChunkPos,
    pick: impl Fn(&Paletted) -> bool,
) -> ChunkPos {
    (0..4096)
        .map(|dx| ChunkPos::new(pos.x + dx, pos.y, pos.z))
        .find(|&p| pick(&g.generate(ChunkKey::new(p), r).blocks))
        .unwrap_or_else(|| panic!("no such chunk near {pos:?}"))
}

fn overworld(c: &mut Criterion) {
    let r = BlockRegistry::default_blocks();
    let g = OverworldGenerator::default();
    let caves = g.settings.caves.clone().expect("default caves");
    let far = 1i64 << 35; // chunks: 2^40 voxels
    // A deep chunk with at least 5% air (a typical cave chunk, not a pinhole).
    let with_caves = |pos: ChunkPos| {
        (0..4096)
            .map(|dx| ChunkPos::new(pos.x + dx, pos.y, pos.z))
            .find(|&p| air(&g, &r, p) >= 0.05)
            .expect("a cave chunk")
    };
    let cave = with_caves(ChunkPos::new(0, -10, 0));
    let cave_far = with_caves(ChunkPos::new(far, -far, 0));
    let solid = find(&g, &r, ChunkPos::new(0, -12, 3), |b| {
        b.as_uniform().is_some()
    });
    let surface = ChunkPos::new(0, g.height_at(0, 0) >> 5, 0);
    for (name, pos) in [
        ("cave_chunk", cave),
        ("cave_chunk_far", cave_far),
        ("solid_chunk", solid),
        ("surface_chunk_caves", surface),
    ] {
        eprintln!("{name}: {pos:?}, air {:.3}", air(&g, &r, pos));
    }
    let mut meshed = 0;
    let deep: Vec<ChunkKey> = (0..64)
        .map(|i| ChunkKey::new(ChunkPos::new(i % 8, -5 - i / 32, i / 8 % 4)))
        .inspect(|&k| meshed += usize::from(g.generate(k, &r).as_uniform().is_none()))
        .collect();
    eprintln!("deep_chunks_mean: {meshed} of 64 with caves");

    let mut group = c.benchmark_group("caves");
    for (name, pos) in [
        ("cave_chunk", cave),
        ("cave_chunk_far", cave_far),
        ("solid_chunk", solid),
        ("surface_chunk_caves", surface),
    ] {
        let key = ChunkKey::new(pos);
        group.bench_function(name, |b| b.iter(|| g.generate(black_box(key), &r)));
    }
    group.bench_function("deep_chunks_mean", |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for _ in 0..iters {
                for &key in &deep {
                    black_box(g.generate(black_box(key), &r));
                }
            }
            // Per chunk.
            start.elapsed() / deep.len() as u32
        });
    });
    let origin = cave.origin();
    group.bench_function("density_corners_405", |b| {
        b.iter(|| caves.chunk_corners(black_box(g.seed), black_box(origin), None));
    });
    group.finish();
}

criterion_group!(benches, noise, overworld);
criterion_main!(benches);
