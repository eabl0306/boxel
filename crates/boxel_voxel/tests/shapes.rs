//! Stairs, fences and torches (ADR 0020): their states, boxes, collision and the neighbor rules
//! that derive stair shapes, fence sides and torch supports.

use boxel_voxel::rules::{
    fence_connects, fence_rule, is_sturdy, stair_shape, stairs_rule, supports_center, torch_rule,
    wall_torch_rule,
};
use boxel_voxel::shapes::{stair_boxes, wall_torch_box};
use boxel_voxel::{
    BlockCollision, BlockModel, BlockRegistry, BlockState, CollisionBox, Face, Facing, FenceSides,
    MAX_MODEL_BOXES, ModelBox, RenderShape, SlabHalf, StairShape, TORCH_EMISSION,
};
use std::collections::HashSet;

fn registry() -> BlockRegistry {
    BlockRegistry::default_blocks()
}

fn s(r: &BlockRegistry, name: &str) -> BlockState {
    r.parse_state(name).unwrap()
}

/// Every sixteenth `(x, y, z)` of the cell inside one of `boxes`.
fn volume(boxes: &[ModelBox]) -> HashSet<[u8; 3]> {
    let mut out = HashSet::new();
    for b in boxes {
        let (lo, hi) = (b.bounds.min, b.bounds.max);
        for x in lo[0]..hi[0] {
            for y in lo[1]..hi[1] {
                for z in lo[2]..hi[2] {
                    out.insert([x, y, z]);
                }
            }
        }
    }
    out
}

/// The 6 neighbors of a block, air but for the given faces.
fn around(pairs: &[(Face, BlockState)]) -> [BlockState; 6] {
    let mut n = [BlockState::AIR; 6];
    for &(f, state) in pairs {
        n[f.index()] = state;
    }
    n
}

#[test]
fn the_new_blocks_have_their_states_and_ids() {
    let r = registry();
    assert_eq!(r.len(), 206);
    let stone_stairs = r.block_by_name("boxel:stone_stairs").unwrap();
    assert_eq!(r.states(stone_stairs).len(), 80);
    assert_eq!(r.default_state(stone_stairs), BlockState(25));
    assert_eq!(
        r.state_name(BlockState(25)).to_string(),
        "boxel:stone_stairs[facing=north,half=bottom,shape=straight,waterlogged=false]"
    );
    assert_eq!(r.by_name("boxel:oak_stairs"), Some(BlockState(105)));
    assert_eq!(r.by_name("boxel:oak_fence"), Some(BlockState(185)));
    assert_eq!(
        r.states(r.block_by_name("boxel:oak_fence").unwrap()).len(),
        16
    );
    assert_eq!(r.by_name("boxel:torch"), Some(BlockState(201)));
    assert_eq!(r.by_name("boxel:wall_torch"), Some(BlockState(202)));
    // Every state's model follows its properties.
    for state in r.states(stone_stairs) {
        let BlockModel::Stairs {
            facing,
            half,
            shape,
        } = r.model(state)
        else {
            panic!("stairs");
        };
        assert_eq!(r.property(state, "facing"), Some(facing.name()));
        assert_eq!(
            r.property(state, "half"),
            Some(if half == SlabHalf::Top {
                "top"
            } else {
                "bottom"
            })
        );
        assert_eq!(r.property(state, "shape"), Some(shape.name()));
        assert_eq!(r.shape(state), RenderShape::Opaque);
        assert_eq!(r.light_opacity(state), 1, "stairs pass light like slabs");
        // They collide with the boxes they are drawn with.
        let boxes: Vec<CollisionBox> = r.model(state).boxes().iter().map(|b| b.bounds).collect();
        assert_eq!(r.collision(state), &BlockCollision::Boxes(boxes));
        assert!(!r.occludes(state));
    }
    let fence = s(&r, "boxel:oak_fence[north=true,west=true]");
    assert_eq!(
        r.model(fence),
        BlockModel::Fence {
            sides: FenceSides::NONE
                .with(Facing::North, true)
                .with(Facing::West, true)
        }
    );
    assert_eq!(r.light_opacity(fence), 0);
    for name in ["boxel:torch", "boxel:wall_torch[facing=west]"] {
        let torch = s(&r, name);
        assert_eq!(r.shape(torch), RenderShape::Cutout);
        assert!(r.collision(torch).is_none());
        assert_eq!(
            r.emission(torch),
            boxel_voxel::LightValue::from_rgb(TORCH_EMISSION)
        );
        assert_eq!(r.light_opacity(torch), 0);
    }
    assert_eq!(TORCH_EMISSION[0], 14);
}

#[test]
fn stair_boxes_are_minecrafts() {
    let east = |shape| stair_boxes(Facing::East, SlabHalf::Bottom, shape);
    let slab = ModelBox::new([0, 0, 0], [16, 8, 16]);
    assert_eq!(
        east(StairShape::Straight),
        [slab, ModelBox::new([8, 8, 0], [16, 16, 16])]
    );
    // Facing east, right is south (+Z).
    assert_eq!(
        east(StairShape::OuterRight),
        [slab, ModelBox::new([8, 8, 8], [16, 16, 16])]
    );
    assert_eq!(
        east(StairShape::OuterLeft),
        [slab, ModelBox::new([8, 8, 0], [16, 16, 8])]
    );
    // Volumes: a straight stair is 3/4 of the cell, an outer corner 5/8, an inner one 7/8.
    for facing in Facing::ALL {
        for half in [SlabHalf::Bottom, SlabHalf::Top] {
            let v = |shape| volume(&stair_boxes(facing, half, shape)).len();
            assert_eq!(v(StairShape::Straight), 3072);
            assert_eq!(v(StairShape::OuterLeft), 2560);
            assert_eq!(v(StairShape::OuterRight), 2560);
            assert_eq!(v(StairShape::InnerLeft), 3584);
            assert_eq!(v(StairShape::InnerRight), 3584);
        }
        // The high side is toward `facing`: its face there is full.
        let straight = stair_boxes(facing, SlabHalf::Bottom, StairShape::Straight);
        let collision = BlockCollision::Boxes(straight.iter().map(|b| b.bounds).collect());
        assert_eq!(
            boxel_voxel::rules::collision_face_cover(&collision, facing.face()),
            [u16::MAX; 16]
        );
        // A quarter turn of the facing turns the shape.
        for shape in StairShape::ALL {
            let turned: Vec<ModelBox> = stair_boxes(facing, SlabHalf::Bottom, shape)
                .into_iter()
                .map(|b| b.turned(1))
                .collect();
            assert_eq!(
                volume(&turned),
                volume(&stair_boxes(facing.clockwise(), SlabHalf::Bottom, shape))
            );
        }
    }
    // Top stairs are bottom ones upside down.
    let top = stair_boxes(Facing::North, SlabHalf::Top, StairShape::Straight);
    assert_eq!(
        top,
        [
            ModelBox::new([0, 8, 0], [16, 16, 16]),
            ModelBox::new([0, 0, 0], [16, 8, 8])
        ]
    );
}

#[test]
fn stair_shapes_follow_minecrafts_rule() {
    let r = registry();
    let stairs = |facing: Facing, half: SlabHalf| {
        boxel_voxel::rules::stairs_with(&r, s(&r, "boxel:stone_stairs"), facing, half).unwrap()
    };
    for facing in Facing::ALL {
        for half in [SlabHalf::Bottom, SlabHalf::Top] {
            let other_half = if half == SlabHalf::Top {
                SlabHalf::Bottom
            } else {
                SlabHalf::Top
            };
            let front = facing.face();
            let back = facing.opposite().face();
            let shape = |n: &[(Face, BlockState)]| stair_shape(&r, facing, half, &around(n));
            assert_eq!(shape(&[]), StairShape::Straight);
            // Stairs on the high side facing across: an outer corner toward their facing.
            let ccw = facing.counterclockwise();
            let cw = facing.clockwise();
            assert_eq!(shape(&[(front, stairs(ccw, half))]), StairShape::OuterLeft);
            assert_eq!(shape(&[(front, stairs(cw, half))]), StairShape::OuterRight);
            // Facing along, or the other half: no corner. Other stairs (oak) count as stairs.
            for f in [facing, facing.opposite()] {
                assert_eq!(shape(&[(front, stairs(f, half))]), StairShape::Straight);
                assert_eq!(shape(&[(back, stairs(f, half))]), StairShape::Straight);
            }
            assert_eq!(
                shape(&[(front, stairs(ccw, other_half))]),
                StairShape::Straight
            );
            let oak =
                boxel_voxel::rules::stairs_with(&r, s(&r, "boxel:oak_stairs"), ccw, half).unwrap();
            assert_eq!(shape(&[(front, oak)]), StairShape::OuterLeft);
            // On the low side: an inner corner.
            assert_eq!(shape(&[(back, stairs(ccw, half))]), StairShape::InnerLeft);
            assert_eq!(shape(&[(back, stairs(cw, half))]), StairShape::InnerRight);
            // The outer corner wins over the inner one.
            assert_eq!(
                shape(&[(front, stairs(cw, half)), (back, stairs(ccw, half))]),
                StairShape::OuterRight
            );
            // A corner is not taken toward a side that continues this run of stairs.
            let same = stairs(facing, half);
            assert_eq!(
                shape(&[(front, stairs(ccw, half)), (ccw.opposite().face(), same)]),
                StairShape::Straight
            );
            assert_eq!(
                shape(&[(back, stairs(ccw, half)), (ccw.face(), same)]),
                StairShape::Straight
            );
            // A side with other stairs does not block it.
            assert_eq!(
                shape(&[
                    (front, stairs(ccw, half)),
                    (ccw.opposite().face(), stairs(cw, half))
                ]),
                StairShape::OuterLeft
            );
            // Anything else around changes nothing.
            let stone = s(&r, "boxel:stone");
            assert_eq!(
                shape(&[(front, stone), (back, stone), (Face::PosY, stone)]),
                StairShape::Straight
            );
        }
    }
    // The rule writes the shape and keeps the other properties.
    let wet = s(&r, "boxel:stone_stairs[facing=east,waterlogged=true]");
    let n = around(&[(Face::PosX, stairs(Facing::North, SlabHalf::Bottom))]);
    assert_eq!(
        r.state_name(stairs_rule(&r, wet, &n)).to_string(),
        "boxel:stone_stairs[facing=east,half=bottom,shape=outer_left,waterlogged=true]"
    );
    // Not stairs: kept.
    let stone = s(&r, "boxel:stone");
    assert_eq!(stairs_rule(&r, stone, &n), stone);
}

/// Two stairs meeting at a corner as Minecraft shapes them form one continuous step: the
/// corner's step touches its neighbor's step across their shared face.
#[test]
fn stair_corners_meet_their_neighbors() {
    let r = registry();
    let east = boxel_voxel::rules::stairs_with(
        &r,
        s(&r, "boxel:stone_stairs"),
        Facing::East,
        SlabHalf::Bottom,
    )
    .unwrap();
    let north = boxel_voxel::rules::stairs_with(
        &r,
        s(&r, "boxel:stone_stairs"),
        Facing::North,
        SlabHalf::Bottom,
    )
    .unwrap();
    // `east` with `north` on its east side: an outer left corner whose step sits in the north
    // east quarter, against the north half step of its neighbor.
    let corner = stairs_rule(&r, east, &around(&[(Face::PosX, north)]));
    let steps: Vec<ModelBox> = r.model(corner).boxes().into_iter().skip(1).collect();
    assert_eq!(steps, [ModelBox::new([8, 8, 0], [16, 16, 8])]);
    let neighbor_step = r.model(north).boxes()[1];
    assert_eq!(neighbor_step.bounds.min[0], 0);
    assert_eq!(neighbor_step.bounds.max[2], 8);
}

#[test]
fn fences_connect_like_minecraft_simplified() {
    let r = registry();
    let connects = |name: &str, toward| fence_connects(&r, s(&r, name), toward);
    for toward in Facing::ALL {
        assert!(connects("boxel:oak_fence", toward));
        assert!(connects("boxel:stone", toward));
        assert!(
            connects("boxel:glass", toward),
            "a full face, as in Minecraft"
        );
        assert!(connects("boxel:stone_slab[type=double]", toward));
        assert!(!connects("boxel:leaves", toward), "Minecraft's exception");
        assert!(!connects("boxel:stone_slab", toward));
        assert!(!connects("boxel:torch", toward));
        assert!(!connects("boxel:water", toward));
        assert!(!connects("boxel:air", toward));
        // Stairs: only by their full high side (toward their facing), turned to the fence.
        let stairs = |facing| {
            boxel_voxel::rules::stairs_with(&r, s(&r, "boxel:oak_stairs"), facing, SlabHalf::Bottom)
                .unwrap()
        };
        assert!(fence_connects(&r, stairs(toward.opposite()), toward));
        assert!(!fence_connects(&r, stairs(toward), toward));
        assert!(!fence_connects(&r, stairs(toward.clockwise()), toward));
    }
    let fence = s(&r, "boxel:oak_fence");
    let stone = s(&r, "boxel:stone");
    let n = around(&[
        (Face::NegZ, stone),
        (Face::PosX, fence),
        (Face::PosY, stone),
        (Face::NegY, stone),
    ]);
    assert_eq!(
        r.state_name(fence_rule(&r, fence, &n)).to_string(),
        "boxel:oak_fence[east=true,north=true,south=false,west=false]"
    );
    // Sides go back to false when the neighbor goes.
    let all = s(
        &r,
        "boxel:oak_fence[east=true,north=true,south=true,west=true]",
    );
    assert_eq!(fence_rule(&r, all, &around(&[])), fence);
}

#[test]
fn torches_need_a_support() {
    let r = registry();
    let torch = s(&r, "boxel:torch");
    let below = |name: &str| torch_rule(&r, torch, &around(&[(Face::NegY, s(&r, name))]));
    assert_eq!(below("boxel:stone"), torch);
    assert_eq!(
        below("boxel:oak_fence"),
        torch,
        "a fence post holds a torch"
    );
    assert_eq!(below("boxel:stone_slab[type=top]"), torch);
    assert_eq!(below("boxel:stone_slab"), BlockState::AIR);
    assert_eq!(below("boxel:air"), BlockState::AIR);
    assert_eq!(below("boxel:water"), BlockState::AIR);
    assert!(supports_center(&r, s(&r, "boxel:glass"), Face::PosY));
    for facing in Facing::ALL {
        let wall = r
            .with(s(&r, "boxel:wall_torch"), "facing", facing.name())
            .unwrap();
        let behind = facing.opposite().face();
        let stone = s(&r, "boxel:stone");
        assert_eq!(wall_torch_rule(&r, wall, &around(&[(behind, stone)])), wall);
        assert_eq!(
            wall_torch_rule(&r, wall, &around(&[(facing.face(), stone)])),
            BlockState::AIR
        );
        assert!(is_sturdy(&r, stone, facing.face()));
        // The stick stands against that wall and samples the standing torch's texels.
        let stick = wall_torch_box(facing);
        let b = stick.bounds;
        let against = match behind {
            Face::NegX => b.min[0] == 0,
            Face::PosX => b.max[0] == 16,
            Face::NegZ => b.min[2] == 0,
            _ => b.max[2] == 16,
        };
        assert!(against, "{facing:?} {stick:?}");
        let texture: [i16; 6] = std::array::from_fn(|k| {
            i16::from(if k < 3 { b.min[k] } else { b.max[k - 3] })
                + i16::from(stick.texture_offset[k % 3])
        });
        assert_eq!(texture, [7, 0, 7, 9, 10, 9]);
    }
}

#[test]
fn the_default_blocks_use_few_model_boxes() {
    let r = registry();
    let mut boxes = HashSet::new();
    for i in 0..r.len() {
        boxes.extend(r.model(BlockState(i as u16)).boxes());
    }
    assert!(boxes.len() <= MAX_MODEL_BOXES);
    // Slabs 2, stairs 2 slabs + 8 steps + 8 quarters (some shared), fences 9, torches 5.
    assert!(boxes.len() < 64, "{}", boxes.len());
    assert!(boxes.iter().all(|b| b.is_valid()));
}
