//! Behavior tests for `FlatGenerator`; the terrain ones moved to `boxel_demo` with it (#48).
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos, LocalPos,
};
use boxel_worldgen::{FlatGenerator, WorldGenerator};

fn registry() -> BlockRegistry {
    boxel_demo::blocks::registry()
}

fn state(r: &BlockRegistry, name: &str) -> BlockState {
    r.by_name(name).unwrap()
}

fn chunk(g: &dyn WorldGenerator, r: &BlockRegistry, x: i64, y: i64, z: i64) -> ChunkData {
    g.generate(ChunkKey::new(ChunkPos::new(x, y, z)), r)
}

fn flat() -> FlatGenerator {
    FlatGenerator::new(vec![
        ("boxel:bedrock".into(), 1),
        ("boxel:stone".into(), 3),
        ("boxel:dirt".into(), 2),
        ("boxel:grass".into(), 1),
    ])
}

#[test]
fn flat_layers_are_exact() {
    let r = registry();
    let g = flat();
    assert_eq!(g.name(), "flat");
    assert_eq!(g.vertical_chunks(), 0..=0);
    let expected = [
        "boxel:bedrock",
        "boxel:stone",
        "boxel:stone",
        "boxel:stone",
        "boxel:dirt",
        "boxel:dirt",
        "boxel:grass",
    ];
    for (cx, cz) in [(0, 0), (-3, 8)] {
        let data = chunk(&g, &r, cx, 0, cz);
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        data.to_dense(&mut dense);
        for (i, &got) in dense.iter().enumerate() {
            let y = LocalPos::from_index(i).y() as usize;
            let want = expected.get(y).map_or(BlockState::AIR, |n| state(&r, n));
            assert_eq!(got, want, "y = {y}");
        }
        assert_eq!(
            chunk(&g, &r, cx, -1, cz).as_uniform(),
            Some(BlockState::AIR)
        );
        assert_eq!(chunk(&g, &r, cx, 1, cz).as_uniform(), Some(BlockState::AIR));
    }
    // Deterministic.
    assert_eq!(chunk(&g, &r, 4, 0, 4), chunk(&g, &r, 4, 0, 4));
    // Any height: air far above and below.
    for y in [(1 << 26) - 1, 1000, -1000, -(1 << 26)] {
        assert_eq!(chunk(&g, &r, 3, y, -2), ChunkData::uniform(BlockState::AIR));
    }
}

#[test]
fn flat_layers_span_chunks() {
    let r = registry();
    let stone = state(&r, "boxel:stone");
    let g = FlatGenerator::new(vec![
        ("boxel:stone".into(), 64),
        ("boxel:air".into(), 2),
        ("boxel:planks".into(), 1),
    ]);
    // 67 voxels: y 0..=66 -> chunks 0..=2.
    assert_eq!(g.vertical_chunks(), 0..=2);
    assert_eq!(chunk(&g, &r, 0, 0, 0).as_uniform(), Some(stone));
    assert_eq!(chunk(&g, &r, 0, 1, 0).as_uniform(), Some(stone));
    let top = chunk(&g, &r, 0, 2, 0);
    assert_eq!(top.get(LocalPos::new(0, 0, 0)), BlockState::AIR);
    assert_eq!(top.get(LocalPos::new(5, 2, 6)), state(&r, "boxel:planks"));
    assert_eq!(top.get(LocalPos::new(5, 3, 6)), BlockState::AIR);
    assert_eq!(chunk(&g, &r, 0, 3, 0).as_uniform(), Some(BlockState::AIR));
    // Exactly 32 voxels end at the chunk border.
    let full = FlatGenerator::new(vec![("boxel:stone".into(), 32)]);
    assert_eq!(full.vertical_chunks(), 0..=0);
    assert_eq!(chunk(&full, &r, 0, 0, 0).as_uniform(), Some(stone));
}

#[test]
fn flat_without_layers_is_empty() {
    let r = registry();
    let g = FlatGenerator::new(Vec::new());
    assert!(g.vertical_chunks().is_empty());
    assert_eq!(chunk(&g, &r, 0, 0, 0).as_uniform(), Some(BlockState::AIR));
}

#[test]
#[should_panic(expected = "boxel:nope")]
fn flat_unknown_block_panics_with_the_name() {
    let r = registry();
    let g = FlatGenerator::new(vec![("boxel:nope".into(), 1)]);
    // Even far from the layers: names are validated on every call.
    let _ = chunk(&g, &r, 0, 50, 0);
}
