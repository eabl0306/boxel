//! Tests of the generation cache (ADR 0042): records come back exactly as they went in, near
//! the origin and far from it; another stamp, a damaged record, a missing node or uniform air is
//! a miss; LOD bands merge across requests; the cap drops the oldest regions, stale ones first;
//! the clear empties it.
use super::*;
use boxel_voxel::testing;
use boxel_voxel::{CHUNK_VOLUME, ChunkPos, LocalPos, MAX_CHUNK_COORD, Paletted};
use proptest::prelude::*;

/// Far coordinates (ADR 0014): the origin, the old `i32` edge, 2^40 and the world limit.
const FAR: [i64; 9] = [
    0,
    -1,
    i32::MAX as i64,
    i32::MIN as i64,
    1 << 40,
    -(1 << 40),
    MAX_CHUNK_COORD,
    -MAX_CHUNK_COORD,
    MAX_CHUNK_COORD - 1,
];

fn cache(settings: GenCacheSettings) -> GenCache {
    GenCache::in_memory(&testing::registry(), settings).expect("cache")
}

fn key(x: i64, y: i64, z: i64) -> ChunkKey {
    ChunkKey {
        dim: DimensionId(0),
        lod: 0,
        pos: ChunkPos::new(x, y, z),
    }
}

fn solid() -> BlockState {
    testing::registry()
        .by_name("test:solid")
        .expect("test:solid")
}

/// A chunk the way a generator leaves one: built by `set`, its palette grown in place (never
/// compacted) and its version bumped by every change.
fn generated(seed: u64) -> ChunkData {
    let registry = testing::registry();
    let states: Vec<BlockState> = (0..registry.len().min(6))
        .map(|i| BlockState(i as u16))
        .collect();
    let mut data = ChunkData::uniform(BlockState::AIR);
    let mut h = seed | 1;
    for i in 0..2000 {
        h ^= h << 13;
        h ^= h >> 7;
        h ^= h << 17;
        let p = LocalPos::new(
            (h % 32) as u32,
            ((h >> 8) % 32) as u32,
            ((h >> 16) % 32) as u32,
        );
        data.set(p, states[(i + (h >> 24) as usize) % states.len()]);
    }
    // A state set and then covered again: the palette keeps it.
    data.set(LocalPos::new(0, 0, 0), states[1]);
    data.set(LocalPos::new(0, 0, 0), BlockState::AIR);
    data
}

#[test]
fn a_chunk_comes_back_as_the_generator_left_it() {
    let c = cache(GenCacheSettings::default());
    let stamp = 11;
    let direct = {
        let states: Vec<BlockState> = (0..CHUNK_VOLUME)
            .map(|i| BlockState((i % testing::registry().len()) as u16))
            .collect();
        ChunkData::from_paletted(Paletted::Direct(states.into_boxed_slice()))
    };
    let chunks = [
        generated(1),
        generated(2),
        ChunkData::uniform(solid()),
        direct,
    ];
    for (i, data) in chunks.iter().enumerate() {
        c.put_chunk(key(i as i64, 0, 0), stamp, data);
    }
    c.flush().expect("flush");
    for (i, data) in chunks.iter().enumerate() {
        let back = c.chunk(key(i as i64, 0, 0), stamp).expect("a hit");
        // The whole value: storage as it is (not compacted) and its version.
        assert_eq!(&back, data);
        assert_eq!(back.blocks(), data.blocks());
        assert_eq!(back.version, data.version);
    }
    assert!(matches!(chunks[0].blocks(), Paletted::Packed { .. }));
    let stats = c.stats();
    assert_eq!((stats.chunk_hits, stats.chunk_misses), (4, 0));
    assert_eq!(stats.written, 4);
    assert!(stats.bytes > 0);
}

#[test]
fn uniform_air_is_never_kept() {
    let c = cache(GenCacheSettings::default());
    c.put_chunk(key(0, 5, 0), 1, &ChunkData::uniform(BlockState::AIR));
    c.flush().expect("flush");
    assert!(c.chunk(key(0, 5, 0), 1).is_none());
    assert_eq!(c.stats().offered, 0);
}

#[test]
fn far_chunks_keep_their_keys_apart() {
    let c = cache(GenCacheSettings::default());
    let mut keys = Vec::new();
    for &x in &FAR {
        for &z in &FAR {
            keys.push(key(x, x.wrapping_neg(), z));
        }
    }
    for (i, &k) in keys.iter().enumerate() {
        c.put_chunk(k, 3, &generated(i as u64 + 7));
    }
    c.flush().expect("flush");
    for (i, &k) in keys.iter().enumerate() {
        assert_eq!(c.chunk(k, 3), Some(generated(i as u64 + 7)), "{k:?}");
    }
    // Next to them, nothing.
    assert!(c.chunk(key(1 << 40, 1, 0), 3).is_none());
}

#[test]
fn another_stamp_is_a_miss() {
    let registry = testing::registry();
    let a = GenCache::in_memory(&registry, GenCacheSettings::default()).expect("cache");
    let gen1 = boxel_worldgen::FlatGenerator::new(vec![("test:solid".to_string(), 4)]);
    let stamp = a.chunk_stamp(&gen1);
    a.put_chunk(key(0, 0, 0), stamp, &generated(3));
    a.flush().expect("flush");
    assert!(a.chunk(key(0, 0, 0), stamp).is_some());
    // Another salt, part or registry: another stamp.
    let salted = GenCache::in_memory(
        &registry,
        GenCacheSettings {
            salt: 1,
            ..GenCacheSettings::default()
        },
    )
    .expect("cache");
    assert_ne!(salted.chunk_stamp(&gen1), stamp);
    assert_ne!(a.lod_stamp(&gen1), Some(stamp));
    let mut builder = boxel_voxel::BlockRegistryBuilder::new();
    builder
        .add_block(boxel_voxel::BlockDef {
            name: "test:other".to_string(),
            shape: boxel_voxel::RenderShape::None,
            model: boxel_voxel::BlockModel::CUBE,
            collision: boxel_voxel::BlockCollision::None,
            emission: [0; 3],
            light_opacity: 0,
            textures: boxel_voxel::FaceTextures::all(boxel_voxel::TextureId(0)),
        })
        .expect("block");
    let other = GenCache::in_memory(&builder.build(), GenCacheSettings::default()).expect("cache");
    assert_ne!(other.chunk_stamp(&gen1), stamp);
    assert!(a.chunk(key(0, 0, 0), stamp ^ 1).is_none());
    assert_eq!(a.stats().chunk_misses, 1);
}

/// Writes `value` raw under the record key of chunk `k`.
fn write_raw(c: &GenCache, k: ChunkKey, value: &[u8]) {
    let db = c.inner.shared.db.read().expect("lock");
    let db = db.as_ref().expect("open");
    let txn = db.begin_write().expect("txn");
    {
        let mut table = txn.open_table(RECORDS).expect("table");
        let rk = record_key(Kind::Chunk, k.dim, k.lod, k.pos.x, k.pos.y, k.pos.z);
        table.insert(&rk, value).expect("insert");
    }
    txn.commit().expect("commit");
}

fn read_raw(c: &GenCache, k: ChunkKey) -> Vec<u8> {
    let db = c.inner.shared.db.read().expect("lock");
    let db = db.as_ref().expect("open");
    let txn = db.begin_read().expect("txn");
    let table = txn.open_table(RECORDS).expect("table");
    let rk = record_key(Kind::Chunk, k.dim, k.lod, k.pos.x, k.pos.y, k.pos.z);
    table
        .get(&rk)
        .expect("get")
        .expect("a record")
        .value()
        .to_vec()
}

#[test]
fn a_damaged_record_is_a_miss_and_is_written_again() {
    let c = cache(GenCacheSettings::default());
    let k = key(-(1 << 40), 2, 1 << 40);
    let data = generated(9);
    c.put_chunk(k, 5, &data);
    c.flush().expect("flush");
    let good = read_raw(&c, k);
    // Every byte of the envelope flipped in turn (the stamp's bytes make another stamp): a miss,
    // or, where LZ4 reads the same payload out of it (the checksum is the payload's), the very
    // chunk. Never another one.
    let mut misses = 0;
    for i in STAMP_LEN..good.len() {
        let mut bad = good.clone();
        bad[i] ^= 0x40;
        write_raw(&c, k, &bad);
        match c.chunk(k, 5) {
            None => misses += 1,
            Some(back) => assert_eq!(back, data, "byte {i}"),
        }
    }
    assert!(misses * 10 >= (good.len() - STAMP_LEN) * 9, "{misses}");
    // Cut short, and empty.
    write_raw(&c, k, &good[..good.len() / 2]);
    assert!(c.chunk(k, 5).is_none());
    write_raw(&c, k, &[]);
    assert!(c.chunk(k, 5).is_none());
    assert!(c.stats().damaged >= misses as u64);
    // Generated again, written again: a hit.
    c.put_chunk(k, 5, &data);
    c.flush().expect("flush");
    assert_eq!(c.chunk(k, 5), Some(data));
}

/// A recorded column: `layers` emitted in this order, each of cells varying with the layer
/// (layer 0 uniform).
fn record_column(c: &GenCache, req: &LodRequest, stamp: u64, band: LodColumn, layers: &[i64]) {
    let mut recorder = c.recorder();
    for &l in layers {
        if l == 0 {
            recorder.record(l, LodNode::Uniform(solid()));
        } else {
            recorder.record(l, LodNode::Cells(&cells(l)));
        }
    }
    c.put_lod(req, stamp, band, recorder);
}

fn cells(seed: i64) -> LodCells {
    let mut cells = LodCells::new();
    for (i, cell) in cells.0.iter_mut().enumerate() {
        let v = (i as i64).wrapping_mul(seed.wrapping_add(31)) >> 7;
        if v % 3 == 0 {
            *cell = solid();
        }
    }
    cells
}

fn request(level: u8, x: i64, z: i64, layers: std::ops::RangeInclusive<i64>) -> LodRequest {
    LodRequest {
        dim: DimensionId(0),
        level,
        column: [x, z],
        layers,
    }
}

/// What `lod_column` emits for `req`: the layers in order, and each node (cells, or the uniform
/// state).
fn replay(c: &GenCache, req: &LodRequest, stamp: u64) -> Option<(LodColumn, Vec<(i64, String)>)> {
    let mut seen = Vec::new();
    let band = c.lod_column(req, stamp, &mut |l, node| {
        let what = match node {
            LodNode::Uniform(s) => format!("uniform {}", s.0),
            LodNode::Cells(cells) => format!("cells {}", checksum_cells(cells)),
        };
        seen.push((l, what));
    })?;
    Some((band, seen))
}

fn checksum_cells(cells: &LodCells) -> u64 {
    let bytes: Vec<u8> = cells.0.iter().flat_map(|s| s.0.to_le_bytes()).collect();
    checksum(&bytes)
}

#[test]
fn a_lod_column_comes_back_in_the_sources_order() {
    let c = cache(GenCacheSettings::default());
    let band = LodColumn {
        lo: -1,
        hi: 3,
        below: solid(),
    };
    for (level, &x) in (1..=15u8).zip(FAR.iter().cycle()) {
        let x = x >> level;
        let req = request(level, x, -x, -4..=4);
        // Top down, as a heightfield source emits.
        record_column(&c, &req, 9, band, &[2, 1, 0, -1]);
        c.flush().expect("flush");
        let (back, seen) = replay(&c, &req, 9).expect("a hit");
        assert_eq!(back, band);
        let layers: Vec<i64> = seen.iter().map(|(l, _)| *l).collect();
        assert_eq!(layers, [2, 1, 0, -1]);
        assert_eq!(seen[2].1, format!("uniform {}", solid().0));
        assert_eq!(seen[0].1, format!("cells {}", checksum_cells(&cells(2))));
        // A part of the column: its layers only, in the same order.
        let (_, part) = replay(&c, &request(level, x, -x, 0..=1), 9).expect("a hit");
        assert_eq!(part.iter().map(|(l, _)| *l).collect::<Vec<_>>(), [1, 0]);
        // Wider than the band: the same (layers outside it are not emitted).
        assert!(replay(&c, &request(level, x, -x, -100..=100), 9).is_some());
        // Another stamp, another level, another column: misses.
        assert!(replay(&c, &req, 10).is_none());
        if level < 15 {
            assert!(replay(&c, &request(level + 1, x, -x, -4..=4), 9).is_none());
        }
        assert!(replay(&c, &request(level, x.wrapping_add(1), -x, -4..=4), 9).is_none());
    }
}

#[test]
fn a_column_with_a_missing_node_is_a_miss_until_it_is_whole() {
    let c = cache(GenCacheSettings::default());
    let band = LodColumn {
        lo: 0,
        hi: 4,
        below: solid(),
    };
    // A column's second job: only its upper layers.
    record_column(&c, &request(1, 3, 3, 2..=3), 1, band, &[3, 2]);
    c.flush().expect("flush");
    assert!(replay(&c, &request(1, 3, 3, 2..=3), 1).is_some());
    assert!(
        replay(&c, &request(1, 3, 3, 0..=3), 1).is_none(),
        "layers 0 and 1 missing"
    );
    // The first job's layers join the band's: the whole column hits, both parts too.
    record_column(&c, &request(1, 3, 3, 0..=1), 1, band, &[1, 0]);
    c.flush().expect("flush");
    let (_, whole) = replay(&c, &request(1, 3, 3, 0..=3), 1).expect("a hit");
    assert_eq!(
        whole.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
        [1, 0, 3, 2]
    );
    assert!(replay(&c, &request(1, 3, 3, 2..=3), 1).is_some());
    // A band of another stamp replaces the old one instead of joining it.
    record_column(&c, &request(1, 3, 3, 0..=0), 2, band, &[0]);
    c.flush().expect("flush");
    assert!(replay(&c, &request(1, 3, 3, 0..=3), 2).is_none());
    assert!(replay(&c, &request(1, 3, 3, 0..=0), 2).is_some());
    // An empty band (all air) is a hit with nothing to emit.
    let air = LodColumn::AIR;
    record_column(&c, &request(2, 0, 0, 0..=5), 1, air, &[]);
    c.flush().expect("flush");
    let (back, seen) = replay(&c, &request(2, 0, 0, 0..=5), 1).expect("a hit");
    assert_eq!((back, seen.len()), (air, 0));
}

proptest! {
    #[test]
    fn any_cells_come_back(states in prop::collection::vec(0u16..300, 1..40), seed in any::<u64>()) {
        // A palette of up to 40 states (or 300 distinct ones: the 16-bit width), anywhere.
        let mut cells = LodCells::new();
        let mut h = seed | 1;
        for cell in cells.0.iter_mut() {
            h ^= h << 13;
            h ^= h >> 7;
            h ^= h << 17;
            *cell = BlockState(states[(h % states.len() as u64) as usize]);
        }
        let mut builder = boxel_voxel::BlockRegistryBuilder::new();
        for i in 0..300 {
            builder.add_block(boxel_voxel::BlockDef {
                name: format!("test:b{i}"),
                shape: boxel_voxel::RenderShape::None,
                model: boxel_voxel::BlockModel::CUBE,
                collision: boxel_voxel::BlockCollision::None,
                emission: [0; 3],
                light_opacity: 0,
                textures: boxel_voxel::FaceTextures::all(boxel_voxel::TextureId(0)),
            })
            .expect("block");
        }
        let c = GenCache::in_memory(&builder.build(), GenCacheSettings::default()).expect("cache");
        let mut recorder = c.recorder();
        recorder.record(5, LodNode::Cells(&cells));
        let req = request(3, -7, 9, 5..=5);
        c.put_lod(&req, 4, LodColumn { lo: 5, hi: 6, below: BlockState::AIR }, recorder);
        c.flush().expect("flush");
        let mut back = None;
        c.lod_column(&req, 4, &mut |_, node| {
            if let LodNode::Cells(got) = node {
                back = Some(got.clone());
            }
        })
        .expect("a hit");
        prop_assert_eq!(back.as_ref(), Some(&cells));
    }
}

#[test]
fn the_cap_drops_the_oldest_regions() {
    let data = generated(4);
    let c = cache(GenCacheSettings::default());
    c.put_chunk(key(0, 0, 0), 1, &data);
    c.flush().expect("flush");
    let one = c.stats().bytes;
    // Room for 10 records; four regions (32 chunk columns apart) of 3 each, oldest first.
    let c = cache(GenCacheSettings {
        max_bytes: one * 10,
        ..GenCacheSettings::default()
    });
    for r in 0..4 {
        for y in 0..3 {
            c.put_chunk(key(r * 32, y, 0), 2, &data);
        }
        c.flush().expect("flush");
    }
    let s = c.stats();
    assert_eq!(s.evicted, 1, "{s:?}");
    assert_eq!(s.bytes, one * 9, "{s:?}");
    assert!(c.chunk(key(0, 0, 0), 2).is_none(), "the oldest region goes");
    for r in 1..4 {
        assert!(c.chunk(key(r * 32, 2, 0), 2).is_some(), "region {r} stays");
    }
    // A region read is used again: it outlives an older write.
    c.put_chunk(key(4 * 32, 0, 0), 2, &data);
    c.flush().expect("flush");
    assert!(c.chunk(key(32, 0, 0), 2).is_some());
}

#[test]
fn stale_regions_go_before_old_ones() {
    let r = |x| Region::of(DimensionId(0), 0, x, 0);
    let mut regions = HashMap::<Region, RegionInfo, foldhash::fast::FixedState>::default();
    // Region 0: used last, but its stamp is not this run's. Region 1: old, current.
    regions.insert(
        r(0),
        RegionInfo {
            used: 100,
            bytes: 10,
            stamp: 1,
        },
    );
    regions.insert(
        r(32),
        RegionInfo {
            used: 1,
            bytes: 10,
            stamp: 2,
        },
    );
    regions.insert(
        r(64),
        RegionInfo {
            used: 50,
            bytes: 10,
            stamp: 2,
        },
    );
    let stamps: HashSet<u64, foldhash::fast::FixedState> = [2].into_iter().collect();
    assert_eq!(eviction_order(&regions, &stamps), [r(0), r(32), r(64)]);
}

#[test]
fn regions_keep_far_columns_apart() {
    for &x in &FAR {
        for lod in [0u8, 1, 4, 15] {
            let c = x >> lod;
            let region = Region::of(DimensionId(3), lod, c, -c);
            assert_eq!(Region::from_key(&region.key()), region);
            let (lo, hi) = region.range();
            let k = record_key(Kind::Node, DimensionId(3), lod, c, 7, -c);
            assert!(lo <= k && k <= hi, "{x} at level {lod}");
            let next = record_key(
                Kind::Node,
                DimensionId(3),
                lod,
                c + (64 >> lod.min(5)),
                7,
                -c,
            );
            assert!(!(lo <= next && next <= hi) || lod > 5, "{x} at level {lod}");
        }
    }
}

#[test]
fn a_clear_drops_everything() {
    let c = cache(GenCacheSettings::default());
    c.put_chunk(key(0, 0, 0), 1, &generated(1));
    record_column(
        &c,
        &request(1, 0, 0, 0..=0),
        1,
        LodColumn {
            lo: 0,
            hi: 1,
            below: solid(),
        },
        &[0],
    );
    c.flush().expect("flush");
    assert!(c.stats().bytes > 0);
    c.clear().expect("clear");
    assert!(c.chunk(key(0, 0, 0), 1).is_none());
    assert!(replay(&c, &request(1, 0, 0, 0..=0), 1).is_none());
    assert_eq!(c.stats().bytes, 0);
    // Usable after.
    c.put_chunk(key(0, 0, 0), 1, &generated(1));
    c.flush().expect("flush");
    assert!(c.chunk(key(0, 0, 0), 1).is_some());
}

#[test]
fn a_full_queue_drops_records_and_a_closed_cache_misses() {
    let c = cache(GenCacheSettings {
        queue: 1,
        ..GenCacheSettings::default()
    });
    for x in 0..200 {
        c.put_chunk(key(x, 0, 0), 1, &generated(x as u64));
    }
    c.flush().expect("flush");
    let s = c.stats();
    assert_eq!(s.offered, 200);
    assert_eq!(s.written + s.dropped, 200, "{s:?}");
    c.close();
    assert!(c.chunk(key(0, 0, 0), 1).is_none());
    c.put_chunk(key(0, 0, 0), 1, &generated(0));
    assert!(c.flush().is_err());
    // Closing again does nothing.
    c.close();
}

#[test]
fn a_cache_turned_off_is_left_alone_by_the_jobs() {
    let c = cache(GenCacheSettings::default());
    assert!(c.enabled());
    c.set_enabled(false);
    assert!(!c.enabled());
    c.set_enabled(true);
    assert!(c.enabled());
    c.close();
    assert!(!c.enabled(), "closed");
}
