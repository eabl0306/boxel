//! Worldgen benchmarks (owner: WP4): `TerrainGenerator::generate` per chunk (surface and uniform).
use criterion::{Criterion, criterion_group, criterion_main};

fn bench(c: &mut Criterion) {
    c.bench_function("placeholder", |b| b.iter(|| 1 + 1));
}

criterion_group!(benches, bench);
criterion_main!(benches);
