//! The demo's rules on its blocks (#48): which blocks are of each kind, and that the builder
//! registered a neighbor rule on exactly the blocks the engine gave one before the content
//! moved out (stairs, the fence, both torches, the wall and the gate) and a use rule on the gate.
//! And which shape each rule reads (ADR 0037, #112): torches and fences the support, walls the
//! collision of the block above.
use boxel_demo::rules::{Kind, Kinds, fence_connects, torch_rule, wall_shape_of, wall_torch_rule};
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockState, CollisionBox, Face,
    FaceTextures, Facing, OPAQUE_OPACITY, RenderShape, TextureId,
};

fn registry() -> BlockRegistry {
    boxel_demo::blocks::registry()
}

#[test]
fn the_demo_blocks_have_their_kinds() {
    let r = registry();
    let kinds = Kinds::of_registry(&r);
    let expected = [
        ("boxel:stone_stairs", Some(Kind::Stairs)),
        ("boxel:oak_stairs", Some(Kind::Stairs)),
        ("boxel:oak_fence", Some(Kind::Fence)),
        ("boxel:torch", Some(Kind::WallPostOverride)),
        ("boxel:wall_torch", Some(Kind::WallTorch)),
        ("boxel:cobblestone_wall", Some(Kind::Wall)),
        ("boxel:oak_fence_gate", Some(Kind::Gate)),
        ("boxel:stone", None),
        ("boxel:stone_slab", None),
        ("boxel:water", None),
    ];
    let all = [
        Kind::Stairs,
        Kind::Fence,
        Kind::Wall,
        Kind::Gate,
        Kind::WallPostOverride,
        Kind::WallTorch,
    ];
    for (name, kind) in expected {
        let block = r.block_by_name(name).unwrap();
        for k in all {
            assert_eq!(kinds.block_is(block, k), kind == Some(k), "{name} {k:?}");
        }
        for state in r.states(block) {
            assert_eq!(kind.is_some_and(|k| kinds.is(&r, state, k)), kind.is_some());
        }
    }
    // The builder and the registry see the same kinds; a registry without the demo blocks has
    // none.
    assert_eq!(Kinds::of_builder(&boxel_demo::blocks::builder()), kinds);
    let empty = boxel_voxel::BlockRegistryBuilder::new().build();
    assert_eq!(Kinds::of_registry(&empty), Kinds::default());
}

#[test]
fn the_rules_are_on_the_blocks_that_had_them() {
    let r = registry();
    let with_rules = [
        "boxel:stone_stairs",
        "boxel:oak_stairs",
        "boxel:oak_fence",
        "boxel:torch",
        "boxel:wall_torch",
        "boxel:cobblestone_wall",
        "boxel:oak_fence_gate",
    ];
    for block in r.blocks() {
        let name = r.block_name(block);
        assert_eq!(
            r.neighbor_rule(block).is_some(),
            with_rules.contains(&name),
            "{name}"
        );
        assert_eq!(
            r.use_rule(block).is_some(),
            name == "boxel:oak_fence_gate",
            "{name}"
        );
    }
}

/// The demo blocks plus `test:sink` and `test:sunk`, two cubes that collide 14/16 high
/// (Minecraft's mud): the sink holds as a full cube (its support shape), the sunk by its
/// collision. And `test:ghost`, which does not collide but holds as a full cube.
fn sink_registry() -> BlockRegistry {
    let mut b = boxel_demo::blocks::builder();
    let def = |name: &str, collision| BlockDef {
        name: name.into(),
        shape: RenderShape::Opaque,
        model: BlockModel::CUBE,
        collision,
        emission: [0; 3],
        light_opacity: OPAQUE_OPACITY,
        textures: FaceTextures::all(TextureId(0)),
    };
    let sunk = BlockCollision::Boxes(vec![CollisionBox::new([0; 3], [16, 14, 16])]);
    for (name, collision, support) in [
        ("test:sink", sunk.clone(), true),
        ("test:sunk", sunk, false),
        ("test:ghost", BlockCollision::None, true),
    ] {
        b.add_block(def(name, collision)).unwrap();
        if support {
            let block = b.block_by_name(name).unwrap();
            b.set_support(block, |_| Some(BlockCollision::Full))
                .unwrap();
        }
    }
    b.build()
}

#[test]
fn torches_and_fences_read_the_support() {
    let r = sink_registry();
    let kinds = Kinds::of_registry(&r);
    let s = |name: &str| r.parse_state(name).unwrap();
    let torch = s("boxel:torch");
    let wall_torch = s("boxel:wall_torch[facing=east]");
    for (ground, holds) in [("test:sink", true), ("test:sunk", false)] {
        let ground = s(ground);
        let mut n = [BlockState::AIR; 6];
        n[Face::NegY.index()] = ground;
        let want = |state| if holds { state } else { BlockState::AIR };
        assert_eq!(torch_rule(&r, torch, &n), want(torch));
        // A wall torch facing east hangs on the block west of it.
        let mut n = [BlockState::AIR; 6];
        n[Face::NegX.index()] = ground;
        assert_eq!(
            wall_torch_rule(&kinds, &r, wall_torch, &n),
            want(wall_torch)
        );
        assert_eq!(fence_connects(&kinds, &r, ground, Facing::East), holds);
    }
}

#[test]
fn walls_read_the_collision_above() {
    let r = sink_registry();
    let kinds = Kinds::of_registry(&r);
    let s = |name: &str| r.parse_state(name).unwrap();
    for mask in 0..16u8 {
        let connected = [0, 1, 2, 3].map(|i| mask & (1 << i) != 0);
        let shape = |above| wall_shape_of(&kinds, &r, connected, above);
        // The bottoms of both sinking cubes cover the wall as a stone above it does.
        let stone = shape(s("boxel:stone"));
        assert_eq!(shape(s("test:sink")), stone, "{connected:?}");
        assert_eq!(shape(s("test:sunk")), stone, "{connected:?}");
        // A block that holds as a full cube without colliding is nothing above a wall.
        assert_eq!(
            shape(s("test:ghost")),
            shape(BlockState::AIR),
            "{connected:?}"
        );
    }
}
