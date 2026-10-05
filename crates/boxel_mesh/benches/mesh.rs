//! Meshing benchmarks (owner: WP5): gather + mesh of terrain-like, caves and checkerboard chunks.
use criterion::{Criterion, criterion_group, criterion_main};

fn bench(c: &mut Criterion) {
    c.bench_function("placeholder", |b| b.iter(|| 1 + 1));
}

criterion_group!(benches, bench);
criterion_main!(benches);
