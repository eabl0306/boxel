//! Paletted container benchmarks: encode/decode/get/set on a terrain-like chunk, and the light
//! masks (`ChunkMasks`) of a cave chunk and of that terrain chunk.
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkMasks, LightTables, LocalPos,
    Occupancy, Paletted,
};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

/// Ops per iteration of the random get/set benchmarks.
const OPS: usize = 1024;

/// A surface chunk like the terrain generator's: rolling heights, stone, 3 dirt, grass, air,
/// plus gravel pockets (6 states, 4-bit palette).
fn terrain_like() -> Vec<BlockState> {
    let (air, stone, dirt, grass, gravel, sand) = (
        BlockState(0),
        BlockState(1),
        BlockState(2),
        BlockState(3),
        BlockState(5),
        BlockState(4),
    );
    let mut out = vec![air; CHUNK_VOLUME];
    for (i, slot) in out.iter_mut().enumerate() {
        let l = LocalPos::from_index(i);
        let (x, y, z) = (f64::from(l.x()), l.y() as i32, f64::from(l.z()));
        let h = (16.0 + 6.0 * (x / 5.0).sin() + 4.0 * (z / 7.0).cos()).round() as i32;
        *slot = if y > h {
            air
        } else if y == h {
            if h < 12 { sand } else { grass }
        } else if y >= h - 3 {
            dirt
        } else if i.wrapping_mul(2_654_435_761) % 97 == 0 {
            gravel
        } else {
            stone
        };
    }
    out
}

/// Deterministic pseudo-random voxel positions (xorshift64).
fn positions(n: usize) -> Vec<LocalPos> {
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            LocalPos::from_index((s % CHUNK_VOLUME as u64) as usize)
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let dense = terrain_like();
    let packed = Paletted::encode(&dense);
    assert_eq!(
        packed.bits_per_voxel(),
        4,
        "terrain-like chunk should be 4-bit"
    );
    let registry = BlockRegistry::default_blocks();
    let chunk = ChunkData::from_dense(&dense);
    let at = positions(OPS);

    let mut g = c.benchmark_group("paletted");
    g.throughput(Throughput::Elements(CHUNK_VOLUME as u64));
    g.bench_function("encode_terrain", |b| {
        b.iter(|| Paletted::encode(black_box(&dense)));
    });
    let mut out = vec![BlockState::AIR; CHUNK_VOLUME];
    g.bench_function("decode_terrain", |b| {
        b.iter(|| black_box(&packed).decode_into(black_box(&mut out)));
    });
    g.bench_function("opaque_occupancy_terrain", |b| {
        b.iter(|| black_box(&chunk).opaque_occupancy(&registry));
    });
    g.bench_function("encode_uniform", |b| {
        let air = vec![BlockState::AIR; CHUNK_VOLUME];
        b.iter(|| Paletted::encode(black_box(&air)));
    });

    g.throughput(Throughput::Elements(OPS as u64));
    g.bench_function("get_random_1024", |b| {
        b.iter(|| {
            let p = black_box(&packed);
            at.iter().fold(0u32, |acc, &l| acc + u32::from(p.get(l).0))
        });
    });
    g.bench_function("set_random_1024", |b| {
        // States already in the palette: measures the in-place write (no widening).
        let mut p = packed.clone();
        let states = [BlockState(1), BlockState(2), BlockState(0), BlockState(3)];
        b.iter(|| {
            for (k, &l) in at.iter().enumerate() {
                black_box(p.set(l, states[k & 3]));
            }
        });
    });
    g.finish();

    // Masks without the cache (`ChunkMasks::compute`), what the first reader of a chunk pays.
    // Targets (ADR 0015): cave_1bit 2 us or less, surface_packed 25 us or less.
    let tables = LightTables::from_registry(&registry);
    let cave = cave_like(&registry);
    assert_eq!(
        cave.blocks().bits_per_voxel(),
        1,
        "cave chunk should be 1-bit"
    );
    let mut g = c.benchmark_group("masks");
    g.throughput(Throughput::Elements(CHUNK_VOLUME as u64));
    g.bench_function("cave_1bit", |b| {
        b.iter(|| ChunkMasks::compute(black_box(&cave), &tables));
    });
    g.bench_function("surface_packed", |b| {
        b.iter(|| ChunkMasks::compute(black_box(&chunk), &tables));
    });
    g.finish();
}

/// A stone chunk with about 10% air in blobs, as the overworld's 1-bit cave chunks.
fn cave_like(registry: &BlockRegistry) -> ChunkData {
    let stone = registry.by_name("boxel:stone").expect("stone");
    let solid: Occupancy = std::array::from_fn(|word| {
        let (y, x) = ((word >> 5) as f64, (word & 31) as f64);
        (0..32).fold(0u32, |w, z| {
            let d = (x / 6.0).sin() + (y / 5.0).cos() + (f64::from(z) / 7.0).sin();
            w | (u32::from(d < 1.6) << z)
        })
    });
    ChunkData::from_occupancy(&solid, stone, BlockState::AIR)
}

criterion_group!(benches, bench);
criterion_main!(benches);
