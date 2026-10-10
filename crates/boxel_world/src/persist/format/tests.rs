use super::*;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockPos, BlockRegistry, BlockRegistryBuilder,
    FaceTextures, LocalPos, MAX_CHUNK_COORD, Property, RenderShape, TextureId,
};
use proptest::prelude::*;

/// An invisible block named `name` (its name is all these tests need).
pub(crate) fn plain(name: &str) -> BlockDef {
    BlockDef {
        name: name.to_string(),
        shape: RenderShape::None,
        model: BlockModel::CUBE,
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(TextureId(0)),
    }
}

/// Far coordinates every key test visits (ADR 0014): the origin, the old `i32` edge, 2^40 and
/// the world limit, both signs, and the ends of `i64`.
const FAR: [i64; 11] = [
    0,
    1,
    -1,
    i32::MAX as i64,
    i32::MIN as i64,
    1 << 40,
    -(1 << 40),
    MAX_CHUNK_COORD,
    -MAX_CHUNK_COORD,
    i64::MAX,
    i64::MIN,
];

fn key(dim: u16, x: i64, y: i64, z: i64) -> ChunkKey {
    ChunkKey {
        dim: DimensionId(dim),
        lod: 0,
        pos: ChunkPos::new(x, y, z),
    }
}

#[test]
fn keys_round_trip_far_from_the_origin() {
    for &x in &FAR {
        for &y in &FAR {
            for &z in &FAR {
                let k = key(3, x, y, z);
                assert_eq!(decode_key(&encode_key(k)), k, "{x} {y} {z}");
            }
        }
    }
}

proptest! {
    /// Byte order is the order of `(dim, lod, x, z, y)`, so a chunk column's layers are one
    /// range of the table, in height order.
    #[test]
    fn key_bytes_sort_like_dim_lod_x_z_y(
        a in (any::<u16>(), any::<u8>(), any::<i64>(), any::<i64>(), any::<i64>()),
        b in (any::<u16>(), any::<u8>(), any::<i64>(), any::<i64>(), any::<i64>()),
    ) {
        let make = |(d, l, x, y, z): (u16, u8, i64, i64, i64)| ChunkKey {
            dim: DimensionId(d),
            lod: l,
            pos: ChunkPos::new(x, y, z),
        };
        let (ka, kb) = (make(a), make(b));
        prop_assert_eq!(decode_key(&encode_key(ka)), ka);
        let tuple = |k: ChunkKey| (k.dim.0, k.lod, k.pos.x, k.pos.z, k.pos.y);
        prop_assert_eq!(encode_key(ka).cmp(&encode_key(kb)), tuple(ka).cmp(&tuple(kb)));
    }

    /// Damaged bytes never panic: a record either reads or is an error.
    #[test]
    fn damaged_records_are_errors_not_panics(
        bytes in proptest::collection::vec(any::<u8>(), 0..200)
    ) {
        let registry = boxel_demo::blocks::registry();
        let (ids, _) = IdMap::resolve(Vec::new(), &registry);
        let _ = decode_chunk(&bytes, &ids);
        let _ = decode_names(&bytes);
        let _ = decode_ticks(&bytes);
        let _ = decode_clock(&bytes);
        let _ = decode_entity(&bytes);
    }

    /// Entity keys: a chunk's entities are one range, in id order, between the chunk's
    /// neighbors in key order.
    #[test]
    fn entity_keys_sort_by_chunk_then_id(
        a in (any::<i64>(), any::<i64>(), any::<i64>(), any::<u64>()),
        b in (any::<i64>(), any::<i64>(), any::<i64>(), any::<u64>()),
    ) {
        let (ka, kb) = (key(0, a.0, a.1, a.2), key(0, b.0, b.1, b.2));
        prop_assert_eq!(decode_entity_key(&encode_entity_key(ka, a.3)), (ka, a.3));
        prop_assert_eq!(
            encode_entity_key(ka, a.3).cmp(&encode_entity_key(kb, b.3)),
            encode_key(ka).cmp(&encode_key(kb)).then(a.3.cmp(&b.3))
        );
    }
}

#[test]
fn entity_keys_round_trip_far_from_the_origin() {
    for &x in &FAR {
        for &y in &FAR {
            for &z in &FAR {
                for id in [1, 2, u64::from(u32::MAX), u64::MAX] {
                    let k = key(7, x, y, z);
                    assert_eq!(decode_entity_key(&encode_entity_key(k, id)), (k, id));
                }
            }
        }
    }
}

fn parts() -> Vec<EntityPart> {
    vec![
        EntityPart {
            codec: "boxel:body".to_string(),
            version: 1,
            bytes: (0..=255).collect(),
        },
        EntityPart {
            codec: "game:crate".to_string(),
            version: u32::MAX,
            bytes: Vec::new(),
        },
    ]
}

#[test]
fn entity_records_round_trip() {
    for parts in [Vec::new(), parts(), parts()[..1].to_vec()] {
        assert_eq!(decode_entity(&encode_entity(&parts)).unwrap(), parts);
    }
}

#[test]
fn any_damage_to_an_entity_record_is_an_error() {
    let good = encode_entity(&parts());
    for i in 0..good.len() {
        for bit in 0..8 {
            let mut bad = good.clone();
            bad[i] ^= 1 << bit;
            assert!(decode_entity(&bad).is_err(), "byte {i} bit {bit}");
        }
    }
    for len in 0..good.len() {
        assert!(decode_entity(&good[..len]).is_err(), "cut at {len}");
    }
    // Another record version is refused as such.
    let mut newer = good.clone();
    newer[4] = ENTITY_VERSION + 1;
    assert_eq!(
        decode_entity(&newer),
        Err(FormatError::Version {
            found: ENTITY_VERSION + 1,
            supported: ENTITY_VERSION
        })
    );
}

/// Every voxel of `data`.
fn voxels(data: &ChunkData) -> Vec<BlockState> {
    let mut out = vec![BlockState::AIR; CHUNK_VOLUME];
    data.to_dense(&mut out);
    out
}

/// A chunk with `n` distinct states (cycled through the registry's).
fn chunk_with(registry: &BlockRegistry, n: usize) -> ChunkData {
    let states: Vec<BlockState> = (0..CHUNK_VOLUME)
        .map(|i| BlockState(u16::try_from(i % n % registry.len()).unwrap()))
        .collect();
    ChunkData::from_dense(&states)
}

#[test]
fn chunk_records_round_trip_every_storage() {
    let registry = boxel_demo::blocks::registry();
    let (ids, grew) = IdMap::resolve(Vec::new(), &registry);
    assert!(grew, "a new world lists every state");
    let stone = registry.by_name("boxel:stone").unwrap();
    let mut edited = ChunkData::uniform(stone);
    edited.set(LocalPos::new(1, 2, 3), BlockState::AIR);
    let cases = [
        ChunkData::uniform(BlockState::AIR),
        ChunkData::uniform(stone),
        edited,
        chunk_with(&registry, 3),
        chunk_with(&registry, 16),
        chunk_with(&registry, registry.len()),
    ];
    for data in cases {
        let bytes = encode_chunk(&data, &ids).unwrap();
        let back = decode_chunk(&bytes, &ids).unwrap();
        assert_eq!(voxels(&back), voxels(&data));
        assert_eq!(back.version, 0);
    }
}

#[test]
fn direct_storage_round_trips() {
    // More than 256 distinct states: a registry with a block of 300 states.
    let mut builder = boxel_demo::blocks::builder();
    builder
        .add_block_with_states(
            plain("test:dial"),
            vec![Property::int("n", 0..=14), Property::int("m", 0..=19)],
            |_, _| {},
        )
        .unwrap();
    let registry = builder.build();
    let (ids, _) = IdMap::resolve(Vec::new(), &registry);
    let data = chunk_with(&registry, registry.len());
    assert!(matches!(data.blocks(), Paletted::Direct(_)));
    let back = decode_chunk(&encode_chunk(&data, &ids).unwrap(), &ids).unwrap();
    assert_eq!(voxels(&back), voxels(&data));
}

#[test]
fn edited_chunks_compress_and_compact() {
    let registry = boxel_demo::blocks::registry();
    let (ids, _) = IdMap::resolve(Vec::new(), &registry);
    let stone = registry.by_name("boxel:stone").unwrap();
    let dirt = registry.by_name("boxel:dirt").unwrap();
    // A palette that grew to 4 entries, then lost all but two: saved compacted.
    let mut data = ChunkData::uniform(stone);
    for (i, s) in [
        dirt,
        BlockState::AIR,
        registry.by_name("boxel:sand").unwrap(),
    ]
    .into_iter()
    .enumerate()
    {
        data.set(LocalPos::new(i as u32, 0, 0), s);
    }
    for i in 1..3 {
        data.set(LocalPos::new(i, 0, 0), stone);
    }
    let bytes = encode_chunk(&data, &ids).unwrap();
    // 32 768 voxels at 1 bit are 4 KiB raw; mostly stone compresses far below.
    assert!(bytes.len() < 512, "{} bytes", bytes.len());
    let back = decode_chunk(&bytes, &ids).unwrap();
    assert_eq!(back.blocks().bits_per_voxel(), 1);
    assert_eq!(voxels(&back), voxels(&data));
}

#[test]
fn ids_survive_a_registry_that_changed_order() {
    // Saved with a registry that has one more block.
    let saved_registry = {
        let mut b = boxel_demo::blocks::builder();
        b.add_block(plain("test:gem")).unwrap();
        b.build()
    };
    let gem = saved_registry.by_name("test:gem").unwrap();
    let log_x = saved_registry.parse_state("boxel:log[axis=x]").unwrap();
    let (saved_ids, _) = IdMap::resolve(Vec::new(), &saved_registry);
    let mut data = ChunkData::uniform(BlockState::AIR);
    data.set(LocalPos::new(0, 0, 0), gem);
    data.set(LocalPos::new(1, 0, 0), log_x);
    let bytes = encode_chunk(&data, &saved_ids).unwrap();

    // Loaded with the default registry: the gem is unknown (air), the log keeps its axis.
    let registry = boxel_demo::blocks::registry();
    let (ids, grew) = IdMap::resolve(saved_ids.names().to_vec(), &registry);
    assert!(!grew, "every default state is already named");
    assert_eq!(ids.unknown().len(), 1);
    assert_eq!(ids.unknown()[0].0, "test:gem");
    let back = decode_chunk(&bytes, &ids).unwrap();
    assert_eq!(back.get(LocalPos::new(0, 0, 0)), BlockState::AIR);
    assert_eq!(
        back.get(LocalPos::new(1, 0, 0)),
        registry.parse_state("boxel:log[axis=x]").unwrap()
    );
    // The gem keeps its stored id: a world saved again does not reuse it for another state.
    let (again, _) = IdMap::resolve(ids.names().to_vec(), &registry);
    assert_eq!(again.names(), ids.names());
}

/// Only a state's exact name becomes its stored id. A block that gained a property reads its old
/// bare name as the default state, but saves that state under its exact name: a later change of
/// the default changes no saved block.
#[test]
fn stored_ids_are_exact_names_so_a_new_default_changes_no_saved_block() {
    let log = |default: &str| {
        let mut b = BlockRegistryBuilder::new();
        b.add_block_with_states(
            plain("test:log"),
            vec![Property::new("axis", ["x", "y", "z"]).with_default(default)],
            |_, _| {},
        )
        .unwrap();
        b.build()
    };
    // Version 1: a log without properties.
    let v1 = {
        let mut b = BlockRegistryBuilder::new();
        b.add_block(plain("test:log")).unwrap();
        b.build()
    };
    let (ids1, _) = IdMap::resolve(Vec::new(), &v1);
    // Version 2 adds `axis` (default y): a log along y saved now...
    let v2 = log("y");
    let (ids2, grew) = IdMap::resolve(ids1.names().to_vec(), &v2);
    assert!(grew);
    let y = v2.parse_state("test:log[axis=y]").unwrap();
    assert_eq!(
        ids2.names()[ids2.stored(y).unwrap() as usize],
        "test:log[axis=y]"
    );
    assert!(
        ids2.unknown()
            .iter()
            .any(|(name, s)| name == "test:log" && *s == y)
    );
    let mut data = ChunkData::uniform(BlockState::AIR);
    data.set(LocalPos::new(0, 0, 0), y);
    let bytes = encode_chunk(&data, &ids2).unwrap();
    // ...is still along y once version 3 makes x the default.
    let v3 = log("x");
    let (ids3, _) = IdMap::resolve(ids2.names().to_vec(), &v3);
    assert_eq!(
        decode_chunk(&bytes, &ids3)
            .unwrap()
            .get(LocalPos::new(0, 0, 0)),
        v3.parse_state("test:log[axis=y]").unwrap()
    );
}

#[test]
fn unknown_properties_and_values_keep_the_rest() {
    let registry = boxel_demo::blocks::registry();
    let fence = registry.block_by_name("boxel:oak_fence").unwrap();
    let names = vec![
        // A property this registry lacks: dropped, the others kept.
        "boxel:oak_fence[east=false,north=true,powered=true,south=false,waterlogged=true,\
         west=false]"
            .to_string(),
        // A value this registry lacks: that property takes its default.
        "boxel:log[axis=w]".to_string(),
        // A block this registry lacks: air.
        "test:ghost[x=1]".to_string(),
    ];
    let (ids, _) = IdMap::resolve(names, &registry);
    let unknown: Vec<(&str, BlockState)> = ids
        .unknown()
        .iter()
        .map(|(n, s)| (n.as_str(), *s))
        .collect();
    let north = registry
        .with(registry.default_state(fence), "north", "true")
        .and_then(|s| registry.with_water(s, true))
        .unwrap();
    assert_eq!(unknown[0].1, north);
    assert_eq!(unknown[1].1, registry.by_name("boxel:log").unwrap());
    assert_eq!(unknown[2].1, BlockState::AIR);
    // Fallbacks never take the id of the state they became: it has its own name.
    let stored = ids.stored(north).unwrap() as usize;
    assert_eq!(ids.names()[stored], registry.state_name(north).to_string());
}

#[test]
fn damaged_envelopes_are_refused() {
    let registry = boxel_demo::blocks::registry();
    let (ids, _) = IdMap::resolve(Vec::new(), &registry);
    let data = chunk_with(&registry, 5);
    let good = encode_chunk(&data, &ids).unwrap();
    assert!(decode_chunk(&good, &ids).is_ok());
    // Every single flipped bit of the header or of the body is caught.
    for i in 0..good.len() {
        let mut bad = good.clone();
        bad[i] ^= 0x10;
        assert!(decode_chunk(&bad, &ids).is_err(), "byte {i}");
    }
    // Truncated anywhere.
    for len in 0..good.len() {
        assert!(decode_chunk(&good[..len], &ids).is_err(), "length {len}");
    }
    // Another record version is not read as this one.
    let mut newer = good.clone();
    newer[4] = CHUNK_VERSION + 1;
    assert_eq!(
        decode_chunk(&newer, &ids).unwrap_err(),
        FormatError::Version {
            found: CHUNK_VERSION + 1,
            supported: CHUNK_VERSION
        }
    );
    // A stored id past the id map is damage too.
    let (small, _) = IdMap::resolve(
        vec!["boxel:air".to_string()],
        &BlockRegistryBuilder::new().build(),
    );
    assert!(decode_chunk(&good, &small).is_err());
}

#[test]
fn names_ticks_and_clock_round_trip() {
    let names = vec!["boxel:air".to_string(), "boxel:log[axis=x]".to_string()];
    assert_eq!(decode_names(&encode_names(&names)).unwrap(), names);
    let ticks: Vec<(BlockPos, u64)> = FAR
        .iter()
        .zip(0..)
        .map(|(&c, i)| (BlockPos::new(c, c.wrapping_neg(), c.wrapping_mul(3)), i * 5))
        .collect();
    assert_eq!(decode_ticks(&encode_ticks(&ticks)).unwrap(), ticks);
    assert_eq!(decode_ticks(&encode_ticks(&[])).unwrap(), Vec::new());
    assert_eq!(decode_clock(&encode_clock(123_456)).unwrap(), 123_456);
    // A names blob is not a chunk record.
    let registry = boxel_demo::blocks::registry();
    let (ids, _) = IdMap::resolve(Vec::new(), &registry);
    assert!(decode_chunk(&encode_names(&names), &ids).is_err());
}

/// Whatever the writer seals the reader opens: a pending tick list past 64 MiB (about 2.7
/// million ticks) reads back. A damaged length is still refused before anything is allocated
/// for it.
#[test]
fn a_tick_list_past_64_mib_reads_back() {
    let ticks: Vec<(BlockPos, u64)> = (0..2_700_000)
        .map(|i| (BlockPos::new(i, -(1 << 40), MAX_CHUNK_COORD << 5), 0))
        .collect();
    let blob = encode_ticks(&ticks);
    let payload = u32::from_le_bytes([blob[6], blob[7], blob[8], blob[9]]);
    assert!(payload > 64 << 20, "{payload} bytes");
    assert_eq!(decode_ticks(&blob).unwrap(), ticks);
    drop(blob);
    // A length no body this size can give.
    let mut small = encode_ticks(&ticks[..10]);
    small[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        decode_ticks(&small).unwrap_err(),
        FormatError::Damaged("bad header")
    );
}
