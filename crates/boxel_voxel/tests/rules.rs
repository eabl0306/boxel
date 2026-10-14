//! Neighbor and use rules registered per block (#48): the registry keeps what the builder was
//! given, by block, and refuses blocks it does not have. The rules are closures over what they
//! need; the engine only stores and hands them out. And the support shape the rule helpers read
//! (ADR 0037, #112): its collision unless a block sets another, per state.
use boxel_voxel::rules::{NeighborRule, UseRule, is_sturdy, supports_center};
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

/// The collision of `test:sink`: a cube that sinks 2/16 (Minecraft's mud and soul sand).
const SINK: CollisionBox = CollisionBox::new([0, 0, 0], [16, 14, 16]);

/// [`builder`] plus `test:sink` (a cube model colliding as [`SINK`]) and `test:layer` with
/// `layers` (`1..=8`), each `k` colliding `2 * (k - 1)` high (Minecraft's snow; nothing at 1).
fn support_builder() -> BlockRegistryBuilder {
    let mut b = builder();
    let t = TextureId(0);
    b.add_block(BlockDef {
        collision: BlockCollision::Boxes(vec![SINK]),
        ..cube("test:sink", t)
    })
    .unwrap();
    b.add_block_with_states(
        BlockDef {
            shape: RenderShape::Cutout,
            light_opacity: 0,
            ..cube("test:layer", t)
        },
        vec![Property::int("layers", 1..=8)],
        |values, def| {
            let k: u8 = values.get("layers").unwrap().parse().unwrap();
            def.collision = if k == 1 {
                BlockCollision::None
            } else {
                BlockCollision::Boxes(vec![CollisionBox::new([0; 3], [16, 2 * (k - 1), 16])])
            };
        },
    )
    .unwrap();
    b
}

fn block(b: &BlockRegistryBuilder, name: &str) -> BlockId {
    b.block_by_name(name).expect(name)
}

fn layer(r: &BlockRegistry, k: u8) -> BlockState {
    r.parse_state(&format!("test:layer[layers={k}]")).unwrap()
}

/// Every state of `r`.
fn states(r: &BlockRegistry) -> impl Iterator<Item = BlockState> {
    (0..r.len() as u16).map(BlockState)
}

const SIDES: [Face; 4] = [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ];

#[test]
fn a_support_holds_what_the_collision_does_not() {
    // Without a support, the sink holds with its collision: only its bottom is sturdy.
    let plain = support_builder().build();
    let sink = plain.by_name("test:sink").unwrap();
    assert_eq!(plain.support(sink), &BlockCollision::Boxes(vec![SINK]));
    assert!(!is_sturdy(&plain, sink, Face::PosY));
    assert!(!supports_center(&plain, sink, Face::PosY));
    assert!(SIDES.iter().all(|&f| !is_sturdy(&plain, sink, f)));
    assert!(is_sturdy(&plain, sink, Face::NegY));
    // With a full support it holds as a full cube on every face, and still collides as before.
    let mut b = support_builder();
    b.set_support(block(&b, "test:sink"), |_| Some(BlockCollision::Full))
        .unwrap();
    let r = b.build();
    assert_eq!(r.support(sink), &BlockCollision::Full);
    assert_eq!(r.collision(sink), &BlockCollision::Boxes(vec![SINK]));
    assert!(is_sturdy(&r, sink, Face::PosY));
    assert!(supports_center(&r, sink, Face::PosY));
    assert!(Face::ALL.iter().all(|&f| is_sturdy(&r, sink, f)));
}

#[test]
fn support_is_per_state_and_a_later_call_replaces() {
    let mut b = support_builder();
    let id = block(&b, "test:layer");
    b.set_support(id, |v| {
        (v.get("layers") == Some("8")).then_some(BlockCollision::Full)
    })
    .unwrap();
    let r = b.build();
    for k in 1..=8 {
        let s = layer(&r, k);
        assert_eq!(is_sturdy(&r, s, Face::PosY), k == 8, "layers={k}");
        assert_eq!(supports_center(&r, s, Face::PosY), k == 8, "layers={k}");
        if k < 8 {
            assert_eq!(r.support(s), r.collision(s), "layers={k}");
        }
    }
    // A later call sets every state again: `None` puts each back on its collision.
    let mut b = support_builder();
    b.set_support(id, |_| Some(BlockCollision::Full)).unwrap();
    b.set_support(id, |_| None).unwrap();
    let r = b.build();
    for k in 1..=8 {
        let s = layer(&r, k);
        assert_eq!(r.support(s), r.collision(s), "layers={k}");
        assert!(!is_sturdy(&r, s, Face::PosY), "layers={k}");
    }
}

#[test]
fn bad_supports_set_nothing() {
    let mut b = support_builder();
    let sink = block(&b, "test:sink");
    let layers = block(&b, "test:layer");
    b.set_support(sink, |_| Some(BlockCollision::Full)).unwrap();
    b.set_support(layers, |v| {
        (v.get("layers") == Some("8")).then_some(BlockCollision::Full)
    })
    .unwrap();
    let boxes = |b: CollisionBox| Some(BlockCollision::Boxes(vec![SINK, b]));
    let invalid = || Err(RegistryError::InvalidCollision("test:sink".into()));
    // An empty box, and one above `MAX_COLLISION_HEIGHT`.
    let empty = CollisionBox::new([0, 4, 0], [16, 4, 16]);
    let tall = CollisionBox::new([0, 0, 0], [16, MAX_COLLISION_HEIGHT + 1, 16]);
    assert_eq!(b.set_support(sink, |_| boxes(empty)), invalid());
    assert_eq!(b.set_support(sink, |_| boxes(tall)), invalid());
    // One bad state rejects the whole call.
    assert_eq!(
        b.set_support(layers, |v| {
            if v.get("layers") == Some("3") {
                boxes(tall)
            } else {
                None
            }
        }),
        Err(RegistryError::InvalidCollision("test:layer".into()))
    );
    assert_eq!(
        b.set_support(BlockId(99), |_| None),
        Err(RegistryError::UnknownBlock(99))
    );
    // The supports set before are kept.
    let r = b.build();
    assert_eq!(
        r.support(r.by_name("test:sink").unwrap()),
        &BlockCollision::Full
    );
    assert_eq!(r.support(layer(&r, 8)), &BlockCollision::Full);
    for k in 1..8 {
        assert_eq!(
            r.support(layer(&r, k)),
            r.collision(layer(&r, k)),
            "layers={k}"
        );
    }
    // A box reaching into the cell above, up to the limit, is a valid support (a fence's).
    let mut b = support_builder();
    let top = CollisionBox::new([6, 0, 6], [10, MAX_COLLISION_HEIGHT, 10]);
    assert_eq!(b.set_support(sink, |_| boxes(top)), Ok(()));
}

/// What physics, fluids and the mesher read is the same with and without supports.
#[test]
fn support_changes_no_other_table() {
    let plain = support_builder().build();
    let mut b = support_builder();
    b.set_support(block(&b, "test:sink"), |_| Some(BlockCollision::Full))
        .unwrap();
    b.set_support(block(&b, "test:layer"), |_| Some(BlockCollision::Full))
        .unwrap();
    b.set_support(block(&b, "test:solid"), |_| Some(BlockCollision::None))
        .unwrap();
    let r = b.build();
    assert_eq!(r.len(), plain.len());
    for s in states(&r) {
        let name = r.state_name(s);
        assert_eq!(r.collision(s), plain.collision(s), "{name}");
        assert_eq!(r.flags(s), plain.flags(s), "{name}");
        assert_eq!(r.outline(s), plain.outline(s), "{name}");
        assert_eq!(r.light_opacity(s), plain.light_opacity(s), "{name}");
        assert_eq!(r.model(s), plain.model(s), "{name}");
        assert_eq!(r.shape(s), plain.shape(s), "{name}");
    }
    // A support of `None` is a shape like any other: the solid cube holds nothing.
    let solid = r.by_name("test:solid").unwrap();
    assert_eq!(r.support(solid), &BlockCollision::None);
    assert!(!is_sturdy(&r, solid, Face::PosY));
}
