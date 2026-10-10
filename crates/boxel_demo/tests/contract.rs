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
            model: BlockModel::CUBE,
            collision: BlockCollision::Full,
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
    use boxel_voxel::{BlockState, Face, RenderShape};
    let r = boxel_demo::blocks::registry();
    // Every state, in id order (ADR 0019: a block's states are contiguous).
    let states = [
        "boxel:air",
        "boxel:stone",
        "boxel:dirt",
        "boxel:grass",
        "boxel:sand",
        "boxel:gravel",
        "boxel:snow",
        "boxel:bedrock",
        "boxel:log[axis=x]",
        "boxel:log[axis=y]",
        "boxel:log[axis=z]",
        "boxel:leaves",
        "boxel:water[level=0]",
        "boxel:water[level=1]",
        "boxel:water[level=2]",
        "boxel:water[level=3]",
        "boxel:water[level=4]",
        "boxel:water[level=5]",
        "boxel:water[level=6]",
        "boxel:water[level=7]",
        "boxel:water[level=8]",
        "boxel:water[level=9]",
        "boxel:water[level=10]",
        "boxel:water[level=11]",
        "boxel:water[level=12]",
        "boxel:water[level=13]",
        "boxel:water[level=14]",
        "boxel:water[level=15]",
        "boxel:planks",
        "boxel:lamp_orange",
        "boxel:lamp_red",
        "boxel:lamp_blue",
        "boxel:lamp_white",
        "boxel:lava[level=0]",
        "boxel:lava[level=1]",
        "boxel:lava[level=2]",
        "boxel:lava[level=3]",
        "boxel:lava[level=4]",
        "boxel:lava[level=5]",
        "boxel:lava[level=6]",
        "boxel:lava[level=7]",
        "boxel:lava[level=8]",
        "boxel:lava[level=9]",
        "boxel:lava[level=10]",
        "boxel:lava[level=11]",
        "boxel:lava[level=12]",
        "boxel:lava[level=13]",
        "boxel:lava[level=14]",
        "boxel:lava[level=15]",
        "boxel:glass",
        "boxel:stone_slab[type=bottom,waterlogged=false]",
        "boxel:stone_slab[type=bottom,waterlogged=true]",
        "boxel:stone_slab[type=top,waterlogged=false]",
        "boxel:stone_slab[type=top,waterlogged=true]",
        "boxel:stone_slab[type=double,waterlogged=false]",
        "boxel:stone_slab[type=double,waterlogged=true]",
        "boxel:short_grass",
        "boxel:poppy",
    ];
    // ADR 0020 adds 197 states after these (stairs, fence, torches; tests/shapes.rs), ADR 0025
    // cobblestone and obsidian, ADR 0027 the wall's 324 and the fence gate's 16
    // (tests/walls.rs).
    assert_eq!(r.len(), states.len() + 199 + 340);
    for (i, n) in states.iter().enumerate() {
        let s = BlockState(i as u16);
        assert_eq!(r.parse_state(n), Ok(s), "{n}");
        assert_eq!(r.state_name(s).to_string(), *n);
    }
    assert_eq!(r.block_count(), 30);
    // A plain name is the default state.
    assert_eq!(r.by_name("boxel:log"), Some(BlockState(9)));
    assert_eq!(r.by_name("boxel:stone_slab"), Some(BlockState(50)));
    // Water: the source is the default (ADR 0021).
    assert_eq!(r.by_name("boxel:water"), Some(BlockState(12)));
    assert_eq!(r.property(BlockState(19), "level"), Some("7"));
    assert_eq!(
        r.by_name("boxel:log[axis=x]"),
        None,
        "by_name takes a block name"
    );
    for gone in ["boxel:log_x", "boxel:log_z", "boxel:stone_slab_top"] {
        assert_eq!(r.by_name(gone), None, "{gone}");
    }
    assert_eq!(r.shape(BlockState(11)), RenderShape::Cutout);
    assert_eq!(r.shape(BlockState(12)), RenderShape::Translucent);
    assert_eq!(r.shape(BlockState(1)), RenderShape::Opaque);
    let grass = BlockState(3);
    let tex = |s, f| r.textures()[r.face_texture(s, f).0 as usize].name.clone();
    assert_eq!(tex(grass, Face::PosY), "boxel:grass_top");
    assert_eq!(tex(grass, Face::NegY), "boxel:dirt");
    assert_eq!(tex(grass, Face::PosX), "boxel:grass_side");
    // The last two: the flow textures of water and lava (ADR 0025).
    assert_eq!(r.textures().len(), 26);
    // The lamps: opaque solid cubes with their own texture.
    for (i, name) in states.iter().enumerate().skip(29).take(4) {
        let s = BlockState(i as u16);
        assert_eq!(r.shape(s), RenderShape::Opaque, "{name}");
        assert!(r.occludes(s), "{name}");
        assert_eq!(tex(s, Face::PosX), *name);
    }
    // ADR 0025: lava is an opaque fluid that hides nothing, lets everything through and glows,
    // its source the default; then cobblestone and obsidian.
    assert_eq!(r.by_name("boxel:lava"), Some(BlockState(33)));
    for level in 33..=48 {
        let lava = BlockState(level);
        assert_eq!(r.model(lava), BlockModel::Fluid);
        assert_eq!(r.shape(lava), RenderShape::Opaque);
        assert!(!r.occludes(lava));
        assert!(
            r.flags(lava)
                .contains(StateFlags::FLUID | StateFlags::EMISSIVE)
        );
        assert!(!r.flags(lava).contains(StateFlags::SOLID));
        assert_eq!(r.collision(lava), &BlockCollision::None);
        assert_eq!(r.light_opacity(lava), 1);
        assert_eq!(tex(lava, Face::PosY), "boxel:lava");
        assert_eq!(tex(lava, Face::NegY), "boxel:lava");
        assert_eq!(tex(lava, Face::PosX), "boxel:lava_flow");
    }
    for (state, name) in [(255, "boxel:cobblestone"), (256, "boxel:obsidian")] {
        let s = BlockState(state);
        assert_eq!(r.by_name(name), Some(s));
        assert_eq!(r.shape(s), RenderShape::Opaque);
        assert!(r.occludes(s) && r.flags(s).contains(StateFlags::SOLID));
        assert_eq!(r.light_opacity(s), 15);
        assert_eq!(tex(s, Face::PosX), name);
    }
    // ADR 0018: shapes, collision and light of the non-cube blocks and the turned logs.
    let by = |n: &str| r.parse_state(n).unwrap();
    let glass = by("boxel:glass");
    assert_eq!(r.shape(glass), RenderShape::Translucent);
    assert_eq!(r.collision(glass), &BlockCollision::Full);
    assert_eq!(r.light_opacity(glass), 0);
    assert!(!r.flags(glass).contains(StateFlags::FLUID));
    for level in 12..=27 {
        let water = BlockState(level);
        assert!(r.flags(water).contains(StateFlags::FLUID));
        assert_eq!(r.model(water), BlockModel::Fluid);
        assert_eq!(r.light_opacity(water), 1);
        assert_eq!(r.collision(water), &BlockCollision::None);
        assert_eq!(tex(water, Face::PosY), "boxel:water");
        assert_eq!(tex(water, Face::NegZ), "boxel:water_flow");
    }
    for (name, flow) in [
        ("boxel:water", "boxel:water_flow"),
        ("boxel:lava", "boxel:lava_flow"),
    ] {
        let still = r.textures().iter().find(|t| t.name == name).unwrap();
        let flow = r.textures().iter().find(|t| t.name == flow).unwrap();
        assert_eq!(flow.base_color, still.base_color);
        assert!(matches!(flow.pattern, TexturePattern::Flow { .. }));
    }
    for (name, half) in [
        ("boxel:stone_slab[type=bottom]", SlabHalf::Bottom),
        ("boxel:stone_slab[type=top]", SlabHalf::Top),
    ] {
        let s = by(name);
        assert_eq!(r.model(s), BlockModel::Slab(half));
        assert_eq!(r.shape(s), RenderShape::Opaque);
        assert!(!r.occludes(s), "a slab hides no neighbor");
        assert_eq!(
            r.collision(s),
            &BlockCollision::Boxes(vec![CollisionBox::slab(half)])
        );
        assert_eq!(r.light_opacity(s), 1);
        assert_eq!(tex(s, Face::PosY), "boxel:stone");
    }
    // ADR 0019: the double slab is a stone cube in every respect.
    let double = by("boxel:stone_slab[type=double]");
    let stone = BlockState(1);
    assert_eq!(r.model(double), BlockModel::CUBE);
    assert_eq!(r.shape(double), RenderShape::Opaque);
    assert_eq!(r.collision(double), &BlockCollision::Full);
    assert_eq!(r.light_opacity(double), 15);
    assert_eq!(r.flags(double), r.flags(stone));
    assert!(r.occludes(double));
    for face in Face::ALL {
        assert_eq!(r.face_texture(double, face), r.face_texture(stone, face));
    }
    for name in ["boxel:short_grass", "boxel:poppy"] {
        let s = by(name);
        assert_eq!(r.model(s), BlockModel::Cross);
        assert_eq!(r.shape(s), RenderShape::Cutout);
        assert!(r.collision(s).is_none());
        assert!(!r.flags(s).contains(StateFlags::SOLID));
        assert_eq!(r.light_opacity(s), 0);
        assert_eq!(tex(s, Face::PosX), name);
    }
    assert!(r.occludes(BlockState(1)));
    // Logs: the end texture across the axis, the bark turned on the faces along it.
    let (log_x, log, log_z) = (BlockState(8), BlockState(9), BlockState(10));
    for s in [log_x, log, log_z] {
        assert_eq!(r.shape(s), RenderShape::Opaque);
        assert!(r.occludes(s));
        assert_eq!(r.light_opacity(s), 15);
        assert_eq!(r.collision(s), &BlockCollision::Full);
    }
    for face in Face::ALL {
        let end = |s| tex(s, face) == "boxel:log_top";
        assert_eq!(
            end(log),
            matches!(face, Face::PosY | Face::NegY),
            "{face:?}"
        );
        assert_eq!(
            end(log_x),
            matches!(face, Face::PosX | Face::NegX),
            "{face:?}"
        );
        assert_eq!(
            end(log_z),
            matches!(face, Face::PosZ | Face::NegZ),
            "{face:?}"
        );
        assert!(!r.model(log).face_turned(face));
        assert_eq!(r.model(log_x).face_turned(face), !end(log_x), "{face:?}");
        // Along Z, the faces whose u axis is Z (+-X) turn; +-Y keep the grain along their v.
        assert_eq!(
            r.model(log_z).face_turned(face),
            matches!(face, Face::PosX | Face::NegX),
            "{face:?}"
        );
    }
}

#[test]
fn registry_rejects_bad_collision_boxes() {
    let mut b = BlockRegistryBuilder::new();
    let t = b
        .add_texture(TextureSpec {
            name: "test:t".into(),
            base_color: [0; 3],
            noise: 0,
            pattern: TexturePattern::Noise,
        })
        .unwrap();
    let def = |name: &str, boxes: Vec<CollisionBox>| BlockDef {
        name: format!("test:{name}"),
        shape: RenderShape::Opaque,
        model: BlockModel::Slab(SlabHalf::Bottom),
        collision: BlockCollision::Boxes(boxes),
        emission: [0; 3],
        light_opacity: 1,
        textures: FaceTextures::all(t),
    };
    for (name, bad) in [
        ("flat", CollisionBox::new([0, 4, 0], [16, 4, 16])),
        ("out", CollisionBox::new([0, 0, 0], [17, 8, 16])),
        ("upside", CollisionBox::new([0, 8, 0], [16, 0, 16])),
        // A collision box may reach half a block into the cell above (ADR 0026), no more, only
        // upward, and it starts in its own cell.
        ("too_tall", CollisionBox::new([6, 0, 6], [10, 25, 10])),
        ("wide", CollisionBox::new([0, 0, 0], [24, 8, 16])),
        ("above", CollisionBox::new([0, 16, 0], [16, 24, 16])),
    ] {
        assert_eq!(
            b.add_block(def(name, vec![CollisionBox::FULL, bad])),
            Err(RegistryError::InvalidCollision(format!("test:{name}")))
        );
    }
    // A slab may have any partial light opacity; an opaque cube may not.
    assert!(
        b.add_block(def("ok", vec![CollisionBox::slab(SlabHalf::Bottom)]))
            .is_ok()
    );
    let tall = CollisionBox::new([6, 0, 6], [10, MAX_COLLISION_HEIGHT, 10]);
    assert!(tall.is_valid_collision() && !tall.is_valid());
    assert!(b.add_block(def("tall", vec![tall])).is_ok());
    let cube = BlockDef {
        name: "test:leaky_cube".into(),
        model: BlockModel::CUBE,
        ..def("unused", vec![])
    };
    assert_eq!(
        b.add_block(cube),
        Err(RegistryError::InvalidLight("test:leaky_cube".into()))
    );
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
