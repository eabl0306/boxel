//! Contract tests for skeleton-implemented items (frozen behavior).
use boxel_voxel::*;
use glam::IVec3;

#[test]
fn block_pos_split_handles_negative_coordinates() {
    let p = BlockPos::new(-1, 33, -32);
    let (c, l) = p.split();
    assert_eq!(c, ChunkPos::new(-1, 1, -1));
    assert_eq!((l.x(), l.y(), l.z()), (31, 1, 0));
    assert_eq!(BlockPos::from_parts(c, l), p);
}

#[test]
fn local_pos_layout_is_y_x_z() {
    let l = LocalPos::new(3, 5, 7);
    assert_eq!(l.index(), (5 << 10) | (3 << 5) | 7);
    assert_eq!(LocalPos::from_index(l.index()), l);
}

#[test]
fn neighbor_offsets_and_index_agree() {
    for (i, d) in NEIGHBOR_OFFSETS.iter().enumerate() {
        assert_eq!(neighbor_index(*d), Some(i));
    }
    assert_eq!(neighbor_index(IVec3::ZERO), None);
    assert_eq!(neighbor_index(IVec3::new(2, 0, 0)), None);
}

#[test]
fn face_axes_are_canonical() {
    assert_eq!(Face::PosY.axes(), (1, 0, 2));
    assert_eq!(Face::NegX.axes(), (0, 2, 1));
    assert_eq!(Face::PosZ.axes(), (2, 0, 1));
    for f in Face::ALL {
        assert_eq!(f.opposite().opposite(), f);
        assert_eq!(Face::from_index(f.index() as u8), Some(f));
    }
}

#[test]
fn registry_builder_assigns_dense_states() {
    let mut b = BlockRegistryBuilder::new();
    let t = b
        .add_texture(TextureSpec {
            name: "test:stone".into(),
            base_color: [128, 128, 128],
            noise: 10,
            pattern: TexturePattern::Noise,
        })
        .unwrap();
    let stone = b
        .add_block(BlockDef {
            name: "test:stone".into(),
            shape: RenderShape::Opaque,
            solid: true,
            emission: [0; 3],
            light_opacity: 15,
            textures: FaceTextures::all(t),
        })
        .unwrap();
    assert_eq!(stone, BlockState(1));
    assert!(
        b.add_texture(TextureSpec {
            name: "Bad Name".into(),
            base_color: [0; 3],
            noise: 0,
            pattern: TexturePattern::Noise
        })
        .is_err()
    );
    let r = b.build();
    assert_eq!(r.by_name("boxel:air"), Some(BlockState::AIR));
    assert_eq!(r.by_name("test:stone"), Some(stone));
    assert_eq!(r.material(BlockState::AIR), 0);
    assert_eq!(r.material(stone), 1);
    assert!(r.flags(stone).contains(StateFlags::OCCLUDES));
}

#[test]
fn uniform_chunk_reads() {
    let c = ChunkData::uniform(BlockState(3));
    assert_eq!(c.get(LocalPos::new(1, 2, 3)), BlockState(3));
    assert_eq!(c.as_uniform(), Some(BlockState(3)));
}

#[test]
fn decode_matches_get_for_hand_built_packed() {
    // 1-bit palette: even voxels state 5, odd voxels state 9
    let words = vec![0xAAAA_AAAA_AAAA_AAAAu64; CHUNK_VOLUME / 64].into_boxed_slice();
    let p = Paletted::Packed {
        palette: vec![BlockState(5), BlockState(9)],
        bits: 1,
        words,
    };
    let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
    p.decode_into(&mut dense);
    for i in [0usize, 1, 2, 63, 64, 1000, CHUNK_VOLUME - 1] {
        assert_eq!(dense[i], p.get(LocalPos::from_index(i)));
        assert_eq!(
            dense[i],
            if i % 2 == 0 {
                BlockState(5)
            } else {
                BlockState(9)
            }
        );
    }
}

#[test]
fn default_registry_ids_shapes_and_faces_are_frozen() {
    use boxel_voxel::{BlockRegistry, BlockState, Face, RenderShape};
    let r = BlockRegistry::default_blocks();
    let names = [
        "boxel:air",
        "boxel:stone",
        "boxel:dirt",
        "boxel:grass",
        "boxel:sand",
        "boxel:gravel",
        "boxel:snow",
        "boxel:bedrock",
        "boxel:log",
        "boxel:leaves",
        "boxel:water",
        "boxel:planks",
        "boxel:lamp_orange",
        "boxel:lamp_red",
        "boxel:lamp_blue",
        "boxel:lamp_white",
        "boxel:lava",
    ];
    assert_eq!(r.len(), names.len());
    for (i, n) in names.iter().enumerate() {
        assert_eq!(r.by_name(n), Some(BlockState(i as u16)), "{n}");
    }
    assert_eq!(r.shape(BlockState(9)), RenderShape::Cutout);
    assert_eq!(r.shape(BlockState(10)), RenderShape::Translucent);
    assert_eq!(r.shape(BlockState(1)), RenderShape::Opaque);
    let grass = BlockState(3);
    let tex = |s, f| r.textures()[r.face_texture(s, f).0 as usize].name.clone();
    assert_eq!(tex(grass, Face::PosY), "boxel:grass_top");
    assert_eq!(tex(grass, Face::NegY), "boxel:dirt");
    assert_eq!(tex(grass, Face::PosX), "boxel:grass_side");
    assert_eq!(r.textures().len(), 18);
    // The emitters: opaque solid cubes with their own texture.
    for (i, name) in names.iter().enumerate().skip(12) {
        let s = BlockState(i as u16);
        assert_eq!(r.shape(s), RenderShape::Opaque, "{name}");
        assert_eq!(tex(s, Face::PosX), *name);
    }
}

#[test]
fn chunk_set_and_from_dense_baseline() {
    use boxel_voxel::{BlockState, CHUNK_VOLUME, ChunkData, LocalPos};
    let mut c = ChunkData::uniform(BlockState::AIR);
    let p = LocalPos::new(1, 2, 3);
    assert_eq!(c.set(p, BlockState(1)), BlockState::AIR);
    assert_eq!(c.version, 1);
    assert_eq!(c.set(p, BlockState(1)), BlockState(1));
    assert_eq!(c.version, 1, "no change, no bump");
    assert_eq!(c.get(p), BlockState(1));
    assert_eq!(c.get(LocalPos::new(0, 0, 0)), BlockState::AIR);
    let mut dense = vec![BlockState(2); CHUNK_VOLUME];
    assert_eq!(
        ChunkData::from_dense(&dense).as_uniform(),
        Some(BlockState(2))
    );
    dense[p.index()] = BlockState(5);
    let d = ChunkData::from_dense(&dense);
    assert_eq!((d.get(p), d.version), (BlockState(5), 0));
}
