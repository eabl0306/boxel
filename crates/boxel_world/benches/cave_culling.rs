//! Cave culling costs (ADR 0022) on `OverworldGenerator::default()`.
//!
//! - `face_info/*`: `ChunkFaceInfo::compute` of one chunk: a cave chunk, a surface chunk (4-bit
//!   palette), and the mean over 64 deep chunks; `generate/*` is the generation of the same
//!   chunks, for the ratio (the generation job computes both).
//! - `cave_bfs/*`: the visibility search from the stress cameras (`examples/stress.rs`): the
//!   surface at (16, 140, 16) looking along +x 20 degrees down, and deep at (16, -1000, 16)
//!   looking along +x, at radius 16 and 32 (vertical radius 8, view distance = radius x 32).
//!   Every chunk of the box has the links of its generated data (inside the mesh circle; the
//!   rest is open, as the world sees chunks without a mesh). `search` is one search with the
//!   links already in the grid (a frame where only the view changed); `fresh` also fills the
//!   grid (a jump); `cross` moves the camera one chunk along x and back (a border crossing: one
//!   slab of the grid is read again, then the search); `no_frustum` searches every direction.
#![expect(clippy::print_stderr, reason = "bench notes")]
use boxel_demo::worldgen::OverworldGenerator;
use boxel_math::projection::{look_to, perspective_infinite_reverse};
use boxel_math::{DVec3, Frustum, Vec3};
use boxel_voxel::{ALL_LINKS, ChunkFaceInfo, ChunkKey, ChunkPos};
use boxel_world::visibility::visible_chunks;
use boxel_world::{ChunkVisibility, VisibilityView};
use boxel_worldgen::WorldGenerator;
use criterion::{Criterion, criterion_group, criterion_main};
use std::collections::HashMap;
use std::hint::black_box;

/// The links of the box around `center` (radius `r`, vertical radius 8), generated on all cores.
fn box_links(center: ChunkPos, r: i64) -> HashMap<ChunkPos, u16> {
    let registry = boxel_demo::blocks::registry();
    let generator = OverworldGenerator::default();
    let mut positions = Vec::new();
    for y in -8..=8 {
        for z in -r..=r {
            for x in -r..=r {
                if x * x + z * z <= r * r {
                    positions.push(ChunkPos::new(center.x + x, center.y + y, center.z + z));
                }
            }
        }
    }
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let chunk = positions.len().div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> = positions
            .chunks(chunk)
            .map(|part| {
                let (registry, generator) = (&registry, &generator);
                s.spawn(move || {
                    part.iter()
                        .map(|&p| {
                            let data = generator.generate(ChunkKey::new(p), registry);
                            (p, ChunkFaceInfo::compute(&data, registry).links)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("generation"))
            .collect()
    })
}

/// The stress camera at `eye` with yaw -90 degrees (along +x) and `pitch` (degrees).
fn view(eye: DVec3, pitch: f32, radius: u32, frustum_on: bool) -> VisibilityView {
    let render_origin = eye.floor().as_i64vec3();
    let eye_rel = (eye - render_origin.as_dvec3()).as_vec3();
    let (sp, cp) = pitch.to_radians().sin_cos();
    let forward = Vec3::new(cp, sp, 0.0);
    let mut frustum = Frustum::from_view_proj_reverse_z(
        perspective_infinite_reverse(70f32.to_radians(), 16.0 / 9.0, 0.1)
            * look_to(eye_rel, forward, Vec3::Y),
    );
    frustum.set_max_distance(eye_rel, forward, radius as f32 * 32.0);
    VisibilityView::new(render_origin, frustum_on.then_some(frustum), radius, 8)
}

fn bench(c: &mut Criterion) {
    let registry = boxel_demo::blocks::registry();
    let generator = OverworldGenerator::default();
    let cave = ChunkPos::new(0, -10, 0);
    let h = generator.height_at(16, 16);
    let surface = ChunkPos::new(0, (h - 4) >> 5, 0);
    let mut g = c.benchmark_group("face_info");
    for (name, pos) in [("cave_chunk", cave), ("surface_chunk", surface)] {
        let data = generator.generate(ChunkKey::new(pos), &registry);
        g.bench_function(name, |b| {
            b.iter(|| ChunkFaceInfo::compute(black_box(&data), &registry));
        });
    }
    let deep: Vec<_> = (0..64)
        .map(|i| {
            let p = ChunkPos::new(i % 8, -12 - i / 8, 3);
            generator.generate(ChunkKey::new(p), &registry)
        })
        .collect();
    g.bench_function("deep_chunks_mean_x64", |b| {
        b.iter(|| {
            for d in &deep {
                black_box(ChunkFaceInfo::compute(black_box(d), &registry));
            }
        });
    });
    g.finish();
    let mut g = c.benchmark_group("generate");
    for (name, pos) in [("cave_chunk", cave), ("surface_chunk", surface)] {
        g.bench_function(name, |b| {
            b.iter(|| generator.generate(ChunkKey::new(black_box(pos)), &registry));
        });
    }
    g.finish();

    let mut g = c.benchmark_group("cave_bfs");
    g.sample_size(30);
    for radius in [16u32, 32] {
        for (name, eye, pitch) in [
            ("surface", DVec3::new(16.0, 140.0, 16.0), -20.0),
            ("deep", DVec3::new(16.0, -1000.0, 16.0), 0.0),
        ] {
            let center = ChunkPos::from_i64vec3(eye.floor().as_i64vec3() >> 5);
            let links = box_links(center, i64::from(radius));
            let lookup = |p: ChunkPos| links.get(&p).copied().unwrap_or(ALL_LINKS);
            let v = view(eye, pitch, radius, true);
            let set = visible_chunks(&v, lookup);
            eprintln!(
                "cave_bfs r{radius} {name}: reached {} of {} box chunks",
                set.visible_count(),
                set.box_len()
            );
            g.bench_function(format!("r{radius}_{name}_fresh"), |b| {
                b.iter(|| visible_chunks(black_box(&v), lookup));
            });
            let no_frustum = view(eye, pitch, radius, false);
            g.bench_function(format!("r{radius}_{name}_no_frustum"), |b| {
                b.iter(|| visible_chunks(black_box(&no_frustum), lookup));
            });
            // A frame where the camera turned a little: the grid is current, the search runs.
            let mut state = ChunkVisibility::default();
            state.update(&v, lookup);
            let turned = [v, view(eye, pitch + 0.5, radius, true)];
            let mut i = 0;
            g.bench_function(format!("r{radius}_{name}_search"), |b| {
                b.iter(|| {
                    i ^= 1;
                    black_box(state.update(&turned[i], lookup));
                });
            });
            // The same with cold caches, as in a game frame that did other work since the last
            // search: 32 MiB are written between searches (not timed).
            let mut thrash = vec![0u8; 32 << 20];
            g.bench_function(format!("r{radius}_{name}_search_cold"), |b| {
                b.iter_batched(
                    || {
                        for (k, v) in thrash.iter_mut().enumerate().step_by(64) {
                            *v = v.wrapping_add(k as u8);
                        }
                        i ^= 1;
                        i
                    },
                    |i| black_box(state.update(&turned[i], lookup)),
                    criterion::BatchSize::PerIteration,
                );
            });
            // A chunk crossing: one slab of the grid is read from the links, then the search.
            let crossed = [v, view(eye + DVec3::X * 32.0, pitch, radius, true)];
            g.bench_function(format!("r{radius}_{name}_cross"), |b| {
                b.iter(|| {
                    i ^= 1;
                    black_box(state.update(&crossed[i], lookup));
                });
            });
        }
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
