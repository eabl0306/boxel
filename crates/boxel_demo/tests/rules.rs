//! The demo's rules on its blocks (#48): which blocks are of each kind, and that the builder
//! registered a neighbor rule on exactly the blocks the engine gave one before the content
//! moved out (stairs, the fence, both torches, the wall and the gate) and a use rule on the gate.
use boxel_demo::rules::{Kind, Kinds};
use boxel_voxel::BlockRegistry;

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
