//! Worldgen benchmarks: `TerrainGenerator::generate` per chunk (surface and uniform), near the
//! origin and far from it (`*_far`: voxel `2^40`, where the lattice cells leave `i32`).
use boxel_voxel::{BlockRegistry, BlockState, ChunkKey, ChunkPos};
use boxel_worldgen::{FlatGenerator, TerrainGenerator, WorldGenerator};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench(c: &mut Criterion) {
    let registry = BlockRegistry::default_blocks();
    let terrain = TerrainGenerator::default();
    let range = terrain.vertical_chunks();

    // The chunk holding the surface at column (0, 0), and at column (2^40, -2^40).
    let surface = ChunkKey::new(ChunkPos::new(0, (terrain.height_at(0, 0) >> 5).into(), 0));
    let far = 1i64 << 40;
    let surface_far = ChunkKey::new(ChunkPos::new(
        far >> 5,
        (terrain.height_at(far, -far) >> 5).into(),
        -far >> 5,
    ));
    // An all-air chunk inside the height band: needs the 32x32 heights before it can answer.
    let air_in_band = (0..64)
        .flat_map(|x| {
            range
                .clone()
                .rev()
                .map(move |y| ChunkKey::new(ChunkPos::new(x, y, 0)))
        })
        .find(|&k| {
            k.pos.origin().y
                <= i64::from(
                    terrain.settings.base_height + terrain.settings.amplitude.ceil() as i32,
                )
                && terrain.generate(k, &registry).as_uniform() == Some(BlockState::AIR)
        })
        .expect("an all-air chunk inside the height band");
    // Above every possible column: answered without heights.
    let air_above = ChunkKey::new(ChunkPos::new(0, range.end() + 1, 0));
    // Fully below the lowest column minus the dirt: Uniform stone after the heights.
    let stone = ChunkKey::new(ChunkPos::new(0, -1, 0));
    for key in [surface, surface_far] {
        assert!(terrain.generate(key, &registry).as_uniform().is_none());
    }
    assert!(terrain.generate(stone, &registry).as_uniform().is_some());

    let mut g = c.benchmark_group("terrain");
    for (name, key) in [
        ("surface_chunk", surface),
        ("surface_chunk_far", surface_far),
        ("air_chunk_in_band", air_in_band),
        ("air_chunk_above", air_above),
        ("stone_chunk", stone),
    ] {
        g.bench_function(name, |b| {
            b.iter(|| terrain.generate(black_box(key), &registry));
        });
    }
    g.bench_function("height_at_32x32", |b| {
        b.iter(|| {
            let mut sum = 0i64;
            for x in 0..32 {
                for z in 0..32 {
                    sum += i64::from(terrain.height_at(black_box(x), z));
                }
            }
            sum
        });
    });
    g.bench_function("height_at_32x32_far", |b| {
        b.iter(|| {
            let mut sum = 0i64;
            for x in far..far + 32 {
                for z in -far..-far + 32 {
                    sum += i64::from(terrain.height_at(black_box(x), z));
                }
            }
            sum
        });
    });
    g.finish();

    let flat = FlatGenerator::new(vec![
        ("boxel:bedrock".into(), 1),
        ("boxel:stone".into(), 3),
        ("boxel:dirt".into(), 2),
        ("boxel:grass".into(), 1),
    ]);
    c.bench_function("flat/surface_chunk", |b| {
        b.iter(|| flat.generate(black_box(ChunkKey::new(ChunkPos::new(0, 0, 0))), &registry));
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
