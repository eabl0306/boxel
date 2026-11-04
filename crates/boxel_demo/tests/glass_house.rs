//! The glass scene of the colored sky (ADR 0064): the stained glass blocks, [`GlassHouse`] and
//! [`GlassHouseWorld`].
use boxel_demo::blocks::{self, STAINED_GLASS};
use boxel_demo::worldgen::glass_house::{
    self, BLACK, CLEAR_GLASS, GRASS, LAMP, LAMP_BLOCK, LIGHT_BLUE, PLANKS, RED, ROOF, ROOM_ROOF,
    ROOM_X, SKYLIGHT, STONE, WALL,
};
use boxel_demo::worldgen::{Glass, GlassHouse, GlassHouseWorld, GlassView};
use boxel_voxel::{BlockPos, BlockState, ChunkKey, RenderShape};
use boxel_worldgen::{FlatGenerator, WorldGenerator};
use std::collections::HashMap;
use std::sync::Arc;

#[test]
fn the_stained_glass_comes_after_every_demo_state_with_its_filter() {
    let plain = blocks::registry();
    let glass = blocks::registry_with_stained_glass();
    // Every demo state and layer keeps its id and name; none filters.
    for s in 0..plain.len() as u16 {
        let s = BlockState(s);
        assert_eq!(
            plain.state_name(s).to_string(),
            glass.state_name(s).to_string()
        );
        assert_eq!(glass.light_filter(s), [15; 3], "{}", glass.state_name(s));
    }
    for (a, b) in plain.textures().iter().zip(glass.textures()) {
        assert_eq!(a.name, b.name);
    }
    assert_eq!(glass.len(), plain.len() + STAINED_GLASS.len());
    assert_eq!(
        glass.textures().len(),
        plain.textures().len() + STAINED_GLASS.len()
    );
    for g in STAINED_GLASS {
        let state = glass.by_name(g.name).expect(g.name);
        assert_eq!(glass.light_filter(state), g.filter, "{}", g.name);
        assert_eq!(glass.shape(state), RenderShape::Translucent);
        assert!(!glass.occludes(state));
    }
    assert!(plain.by_name(RED).is_none(), "not in the plain demo");
}

/// Ground at y = 63 (64 on the hill at x >= 8), loaded down to y = 0; `BlockState(1)` is
/// ground, `BlockState(2)` a tree that is not.
fn world(p: BlockPos) -> Option<BlockState> {
    let top = if p.x >= 8 { 64 } else { 63 };
    (p.y >= 0).then_some(if p.y <= top {
        BlockState(1)
    } else if (p.x, p.z) == (0, 0) && p.y <= 70 {
        BlockState(2)
    } else {
        BlockState::AIR
    })
}

fn is_ground(s: BlockState) -> bool {
    s == BlockState(1)
}

#[test]
fn the_scene_is_built_on_the_ground_once_it_is_loaded() {
    let house = GlassHouse::around((0, 0), 100, 256, Glass::Stained, world, is_ground)
        .expect("loaded")
        .expect("ground");
    // On the ground of the center column, under the tree.
    assert_eq!(house.floor, BlockPos::new(0, 63, 0));
    let cells: HashMap<BlockPos, &str> = house.cells.iter().copied().collect();
    assert_eq!(cells.len(), house.cells.len(), "each cell once");
    let at = |x, row, z| cells.get(&BlockPos::new(x, 63 + row, z)).copied();
    // The greenhouse: a grass floor, cobblestone walls, a red roof with a black skylight.
    assert_eq!(at(0, 0, 0), Some(GRASS));
    assert_eq!(at(-9, 3, 0), Some(WALL));
    assert_eq!(at(0, 3, -7), Some(WALL));
    assert_eq!(at(0, 1, 7), Some(WALL), "closed");
    assert_eq!(at(0, ROOF, 0), Some(RED));
    assert_eq!(at(-9, ROOF, -7), Some(RED), "over the walls too");
    let [(x0, z0), (x1, z1)] = SKYLIGHT;
    assert_eq!(at(x0, ROOF, z0), Some(BLACK));
    assert_eq!(at(x1, ROOF, z1), Some(BLACK));
    assert_eq!(at(x1 + 1, ROOF, z1), Some(RED));
    assert_eq!(
        at(LAMP.0, LAMP.1, LAMP.2),
        Some(LAMP_BLOCK),
        "under the roof"
    );
    // The room: closed, its planks roof under the greenhouse's, light blue only in its window.
    assert_eq!(at(ROOM_X[0], 3, 0), Some(WALL), "the shared wall");
    assert_eq!(at(12, 0, 0), Some(PLANKS));
    assert_eq!(at(12, ROOM_ROOF, 0), Some(PLANKS));
    assert_eq!(at(ROOM_X[1], 3, 0), Some(LIGHT_BLUE));
    assert_eq!(at(ROOM_X[1], 1, 0), Some(WALL));
    assert_eq!(at(ROOM_X[1], 3, 2), Some(WALL));
    let blue = house.cells.iter().filter(|c| c.1 == LIGHT_BLUE).count();
    assert_eq!(blue, 9, "a 3 by 3 window");
    // The roofs are the scene's only cover: nothing but air over them.
    for &(pos, name) in &house.cells {
        if pos.y > 63 + ROOF {
            assert_eq!(name, glass_house::AIR, "{pos:?}");
        }
    }
    // The tree is cleared and the hill under the floor stays: nothing goes under the ground.
    assert_eq!(at(0, 5, 0), Some(glass_house::AIR));
    assert_eq!(at(12, -1, 0), None);
    // Both cameras stand in air, inside.
    for view in GlassView::ALL {
        let [x, y, z, ..] = house.camera(view);
        let eye = BlockPos::new(x.floor() as i64, y.floor() as i64, z.floor() as i64);
        assert_eq!(cells.get(&eye), Some(&glass_house::AIR), "{view:?}");
        let below = BlockPos::new(eye.x, 63, eye.z);
        assert_ne!(
            cells.get(&below),
            Some(&glass_house::AIR),
            "{view:?} on the floor"
        );
    }
    assert_eq!(GlassView::from_name("window"), Some(GlassView::WindowRoom));

    // A column falling away: the foundation goes down to its ground, at most 16.
    let pit = |p: BlockPos| {
        if (p.x, p.z) == (5, 5) {
            Some(if p.y <= 50 {
                BlockState(1)
            } else {
                BlockState::AIR
            })
        } else {
            world(p)
        }
    };
    let house = GlassHouse::around((0, 0), 100, 256, Glass::Stained, pit, is_ground)
        .unwrap()
        .unwrap();
    let foundation: Vec<i64> = house
        .cells
        .iter()
        .filter(|(p, n)| (p.x, p.z) == (5, 5) && *n == STONE && p.y < 63)
        .map(|(p, _)| p.y)
        .collect();
    assert_eq!(foundation, (51..63).collect::<Vec<_>>());
    // The clear twin: the same cells, common glass where the stained one was.
    let clear = GlassHouse::around((0, 0), 100, 256, Glass::Clear, world, is_ground)
        .unwrap()
        .unwrap();
    let stained = GlassHouse::around((0, 0), 100, 256, Glass::Stained, world, is_ground)
        .unwrap()
        .unwrap();
    for (c, s) in clear.cells.iter().zip(&stained.cells) {
        assert_eq!(c.0, s.0);
        let glass = [RED, LIGHT_BLUE, BLACK].contains(&s.1);
        assert_eq!(c.1, if glass { CLEAR_GLASS } else { s.1 }, "{:?}", c.0);
    }
    // Not loaded yet: wait; no ground in reach: nothing.
    let none = |_| None;
    assert_eq!(
        GlassHouse::around((0, 0), 100, 256, Glass::Stained, none, is_ground),
        None
    );
    let air = |_| Some(BlockState::AIR);
    assert_eq!(
        GlassHouse::around((0, 0), 100, 256, Glass::Stained, air, is_ground),
        Some(None)
    );
}

#[test]
fn the_glass_house_world_builds_the_scene_into_its_chunks() {
    let registry = blocks::registry_with_stained_glass();
    let flat: Arc<dyn WorldGenerator> = Arc::new(FlatGenerator::new(vec![
        ("boxel:stone".to_string(), 3),
        ("boxel:grass".to_string(), 1),
    ]));
    let world = GlassHouseWorld::around(Arc::clone(&flat), &registry, (40, -20), Glass::Stained);
    let house = world.house.as_ref().expect("on the grass");
    assert_eq!(house.floor, BlockPos::new(40, 3, -20));
    // The scene reaches over the flat world's single layer: the generator says so.
    assert_eq!(world.vertical_chunks(), 0..=0);
    let block = |p: BlockPos| {
        let (chunk, local) = p.split();
        world.generate(ChunkKey::new(chunk), &registry).get(local)
    };
    for &(pos, name) in &house.cells {
        assert_eq!(Some(block(pos)), registry.by_name(name), "{pos:?} {name}");
    }
    // Away from the scene the flat world is untouched.
    let far = ChunkKey::new(BlockPos::new(400, 0, 400).split().0);
    assert_eq!(
        world
            .generate(far, &registry)
            .get(BlockPos::new(400, 3, 400).split().1),
        registry.by_name("boxel:grass").unwrap()
    );
    // A world of only air has no ground: no scene, the inner world as it is.
    let empty: Arc<dyn WorldGenerator> = Arc::new(FlatGenerator::new(vec![]));
    assert!(
        GlassHouseWorld::around(empty, &registry, (0, 0), Glass::Clear)
            .house
            .is_none()
    );
}
