//! Chunk memory in pages (ADR 0035), the CPU side.
//!
//! - `cull/*`: Mid and Low's CPU cull of about 52,000 chunks (radius 32, 17 chunks tall) from
//!   the middle, looking along +x: every chunk in one page (main's single arena), placed as the
//!   renderer places them in pages of 10,922 chunks, and scattered over 9 pages (slot `s` in page
//!   `s % 9`, the worst case for the per-page lists).
//! - `place/*`: 512 placements a frame (remeshes and new chunks, one in ten removed) with the
//!   size mix of an underground world (median about 550 quads, a few near 2,000) over 9 pages.
//! - `flush/*`: one frame of 512 meshes uploaded, up to the copies (placement, the table runs and
//!   every write `Uploads::flush` makes).
#![expect(clippy::print_stderr, reason = "bench notes")]
use boxel_camera::{Camera, CameraView};
use boxel_math::{DVec3, I64Vec3};
use boxel_mesh::{BUCKET_COUNT, ChunkMesh, PackedQuad};
use boxel_render::bench::{CullBench, FlushBench, PlaceBench};
use boxel_render::{ChunkMemorySettings, DEFAULT_SKY_COLOR, DistanceFog, ViewParams};
use boxel_transform::Transform;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;

/// xorshift64.
struct Rng(u64);

impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }

    /// A mesh size of the underground mix: median about 550 quads, 1 in 100 near 2,000.
    fn size(&mut self) -> u32 {
        match self.next(100) {
            0 => 1800 + self.next(300) as u32,
            1..20 => 900 + self.next(700) as u32,
            _ => 250 + self.next(600) as u32,
        }
    }
}

/// A mesh of `quads` quads over the 6 face buckets and the cutout bucket.
fn mesh(quads: u32) -> Arc<ChunkMesh> {
    let mut counts = [0; BUCKET_COUNT];
    for (i, c) in counts.iter_mut().take(7).enumerate() {
        *c = quads / 7 + u32::from((i as u32) < quads % 7);
    }
    Arc::new(ChunkMesh::from_quads(
        vec![PackedQuad::default(); quads as usize],
        counts,
    ))
}

/// The chunks of radius 32, 17 chunks tall, around the origin.
fn r32_chunks() -> Vec<(I64Vec3, Arc<ChunkMesh>)> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::new();
    for y in -8..=8i64 {
        for z in -32..=32i64 {
            for x in -32..=32i64 {
                if x * x + z * z <= 32 * 32 {
                    out.push((I64Vec3::new(x, y, z) * 32, mesh(7 + rng.next(30) as u32)));
                }
            }
        }
    }
    out
}

fn view() -> ViewParams {
    let camera = Camera {
        view_distance: 32.0 * 32.0,
        ..Camera::default()
    };
    let transform =
        Transform::from_yaw_pitch(DVec3::new(16.5, 16.5, 16.5), -90f32.to_radians(), 0.0);
    let view = CameraView::compute(&camera, &transform, 16.0 / 9.0);
    ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
}

fn nine_pages() -> ChunkMemorySettings {
    ChunkMemorySettings {
        initial_quads: 9 << 22,
        initial_slots: 65536,
        grow: false,
        ..ChunkMemorySettings::default()
    }
}

fn cull(c: &mut Criterion) {
    let chunks = r32_chunks();
    let view = view();
    let mut group = c.benchmark_group("cull");
    for (name, mut bench) in [
        ("one_page", CullBench::scattered(&chunks, 1)),
        ("placed", CullBench::placed(&chunks, &nine_pages())),
        ("scattered_9", CullBench::scattered(&chunks, 9)),
    ] {
        let draws = bench.run(&view);
        eprintln!(
            "cull/{name}: {} chunks in {} pages, {draws} draws",
            bench.chunks(),
            bench.pages
        );
        group.bench_function(name, |b| b.iter(|| black_box(bench.run(&view))));
    }
    group.finish();
}

fn place(c: &mut Criterion) {
    let slots = 52_000;
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut bench = PlaceBench::new(&nine_pages(), slots);
    // settle at about 40,000 resident chunks
    for id in 0..40_000 {
        bench.place(id, rng.size(), rng.next(20) == 0);
    }
    eprintln!("place: {} quads in use", bench.used());
    c.bench_function("place/512_real_mix", |b| {
        b.iter(|| {
            for _ in 0..512 {
                let id = rng.next(slots as u64) as usize;
                if rng.next(10) == 0 {
                    bench.remove(id);
                } else {
                    black_box(bench.place(id, rng.size(), rng.next(20) == 0));
                }
            }
        });
    });
}

fn flush(c: &mut Criterion) {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let meshes: Vec<(I64Vec3, Arc<ChunkMesh>)> = (0..512)
        .map(|i| (I64Vec3::new(32 * i, 0, 0), mesh(rng.size())))
        .collect();
    c.bench_function("flush/512_meshes", |b| {
        b.iter_batched_ref(
            || FlushBench::new(&nine_pages()),
            |f| black_box(f.frame(0, &meshes)),
            BatchSize::LargeInput,
        );
    });
}

criterion_group!(benches, cull, place, flush);
criterion_main!(benches);
