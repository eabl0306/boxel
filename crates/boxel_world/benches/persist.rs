//! A chunk record written and read (ADR 0024, ADR 0048): `format::encode_chunk` and
//! `format::decode_chunk` of an edited surface chunk (a palette of 6 states, 4 bits per voxel),
//! as the save job and the load job run them, without block data (`plain`, the record of every
//! chunk before ADR 0048) and with 16 furnace-like cells (`data`).
//!
//! `cargo bench -p boxel_world --bench persist` (release; never while something compiles).

use boxel_voxel::{BlockData, BlockState, CHUNK_VOLUME, ChunkData, DataValue, ItemStack, LocalPos};
use boxel_world::persist::IdMap;
use boxel_world::persist::format::{decode_chunk, encode_chunk};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// Ground with a few blocks of 6 kinds and caves, the shape of an edited surface chunk.
fn surface(registry: &boxel_voxel::BlockRegistry) -> ChunkData {
    let s = |name: &str| registry.parse_state(name).expect(name);
    let kinds = [
        s("boxel:stone"),
        s("boxel:dirt"),
        s("boxel:grass"),
        s("boxel:sand"),
        s("boxel:log[axis=y]"),
    ];
    let states: Vec<BlockState> = (0..CHUNK_VOLUME)
        .map(|i| {
            let p = LocalPos::from_index(i);
            let (x, y, z) = (p.x(), p.y(), p.z());
            let top = 12 + (x * 7 + z * 3) % 9;
            if y > top || (x * 31 + y * 17 + z * 13) % 23 == 0 {
                BlockState::AIR
            } else {
                kinds[((x + y + z) % 5) as usize]
            }
        })
        .collect();
    ChunkData::from_dense(&states)
}

fn furnace(k: i64) -> BlockData {
    let items = vec![
        Some(ItemStack::new("game:raw_iron", 9)),
        Some(ItemStack::new("game:coal", 3)),
        Some(ItemStack::new("game:iron_ingot", 12)),
    ];
    BlockData::new()
        .with("items", DataValue::Items(items.into()))
        .with("lit", DataValue::Int(1200 + k))
        .with("lit_total", DataValue::Int(1600))
        .with("cook", DataValue::Int(80))
}

fn bench(c: &mut Criterion) {
    let registry = boxel_demo::blocks::registry();
    let (ids, _) = IdMap::resolve(Vec::new(), &registry);
    let plain = surface(&registry);
    let mut data = plain.clone();
    for k in 0..16u32 {
        data.set_block_data(LocalPos::new(k * 2, 3, k), Some(furnace(i64::from(k))));
    }
    for (name, chunk) in [("plain", &plain), ("data", &data)] {
        let bytes = encode_chunk(chunk, &ids).unwrap();
        c.bench_function(&format!("encode_chunk/{name}"), |b| {
            b.iter(|| encode_chunk(black_box(chunk), &ids).unwrap());
        });
        c.bench_function(&format!("decode_chunk/{name}"), |b| {
            b.iter(|| decode_chunk(black_box(&bytes), &ids).unwrap());
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
