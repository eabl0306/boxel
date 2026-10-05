//! Paletted container benchmarks (owner: WP4): encode/decode/get/set on a terrain-like chunk.
use criterion::{Criterion, criterion_group, criterion_main};

fn bench(c: &mut Criterion) {
    c.bench_function("placeholder", |b| b.iter(|| 1 + 1));
}

criterion_group!(benches, bench);
criterion_main!(benches);
