//! Neighbor and use rules registered per block (#48): the registry keeps what the builder was
//! given, by block, and refuses blocks it does not have. The rules are closures over what they
//! need; the engine only stores and hands them out.
use boxel_voxel::rules::{NeighborRule, UseRule};
use boxel_voxel::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn texture(b: &mut BlockRegistryBuilder) -> TextureId {
    b.add_texture(TextureSpec {
        name: "test:t".into(),
        image: TextureImage::solid(16, [0, 0, 0, 255]),
    })
    .unwrap()
}

fn cube(name: &str, t: TextureId) -> BlockDef {
    BlockDef {
        name: name.into(),
        shape: RenderShape::Opaque,
        model: BlockModel::CUBE,
        collision: BlockCollision::Full,
        emission: [0; 3],
        light_opacity: 15,
        textures: FaceTextures::all(t),
    }
}

/// `test:solid`, and `test:switch` with `on` (`false | true`).
fn builder() -> BlockRegistryBuilder {
    let mut b = BlockRegistryBuilder::new();
    let t = texture(&mut b);
    b.add_block(cube("test:solid", t)).unwrap();
    b.add_block_with_states(
        cube("test:switch", t),
        vec![Property::boolean("on")],
        |_, _| {},
    )
    .unwrap();
    b
}

#[test]
fn blocks_are_found_by_name_while_building() {
    let b = builder();
    assert_eq!(b.block_by_name("boxel:air"), Some(BlockId(0)));
    assert_eq!(b.block_by_name("test:solid"), Some(BlockId(1)));
    assert_eq!(b.block_by_name("test:switch"), Some(BlockId(2)));
    assert_eq!(b.block_by_name("test:missing"), None);
    let r = b.build();
    assert_eq!(r.block_by_name("test:switch"), Some(BlockId(2)));
}

#[test]
fn the_registry_keeps_the_rules_of_each_block() {
    let mut b = builder();
    let switch = b.block_by_name("test:switch").unwrap();
    // A rule that turns the switch on next to a solid block, holding a count of its calls.
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let rule = NeighborRule::new(move |r, s, n| {
        counted.fetch_add(1, Ordering::Relaxed);
        let solid = r.by_name("test:solid").unwrap();
        let on = n.contains(&solid);
        r.with(s, "on", if on { "true" } else { "false" }).unwrap()
    });
    b.set_neighbor_rule(switch, rule).unwrap();
    b.set_use_rule(
        switch,
        UseRule::new(|r, s, look| {
            (look == Facing::North).then(|| {
                let on = r.property(s, "on") == Some("true");
                r.with(s, "on", if on { "false" } else { "true" }).unwrap()
            })
        }),
    )
    .unwrap();
    let r = b.build();
    let off = r.parse_state("test:switch[on=false]").unwrap();
    let on = r.parse_state("test:switch[on=true]").unwrap();
    let solid = r.by_name("test:solid").unwrap();
    let rule = r.neighbor_rule(switch).expect("registered");
    let mut n = [BlockState::AIR; 6];
    assert_eq!(rule.apply(&r, off, &n), off);
    n[Face::NegY.index()] = solid;
    assert_eq!(rule.apply(&r, off, &n), on);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    // A clone shares the closure.
    let copy = rule.clone();
    assert_eq!(copy.apply(&r, on, &[BlockState::AIR; 6]), off);
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    let use_rule = r.use_rule(switch).expect("registered");
    assert_eq!(use_rule.apply(&r, off, Facing::North), Some(on));
    assert_eq!(use_rule.apply(&r, off, Facing::East), None);
    // Blocks without rules, and blocks out of range.
    for block in [BlockId(0), BlockId(1), BlockId(3), BlockId(u16::MAX)] {
        assert!(r.neighbor_rule(block).is_none(), "{block:?}");
        assert!(r.use_rule(block).is_none(), "{block:?}");
    }
    assert_eq!(format!("{rule:?}"), "NeighborRule(..)");
    assert_eq!(format!("{use_rule:?}"), "UseRule(..)");
}

#[test]
fn a_later_rule_replaces_an_earlier_one() {
    let mut b = builder();
    let switch = b.block_by_name("test:switch").unwrap();
    b.set_neighbor_rule(switch, NeighborRule::new(|_, _, _| BlockState::AIR))
        .unwrap();
    b.set_neighbor_rule(switch, NeighborRule::new(|_, s, _| s))
        .unwrap();
    let r = b.build();
    let state = r.by_name("test:switch").unwrap();
    let rule = r.neighbor_rule(switch).unwrap();
    assert_eq!(rule.apply(&r, state, &[BlockState::AIR; 6]), state);
}

#[test]
fn rules_of_unknown_blocks_are_refused() {
    let mut b = builder();
    assert_eq!(
        b.set_neighbor_rule(BlockId(3), NeighborRule::new(|_, s, _| s)),
        Err(RegistryError::UnknownBlock(3))
    );
    assert_eq!(
        b.set_use_rule(BlockId(7), UseRule::new(|_, _, _| None)),
        Err(RegistryError::UnknownBlock(7))
    );
    let r = b.build();
    assert!(r.blocks().all(|block| r.neighbor_rule(block).is_none()));
}
