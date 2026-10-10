//! Walls and fence gates (ADR 0027): their states, boxes, collision, the neighbor rules that
//! connect walls, raise their posts and drop gates between walls, and what using a gate does.
//! The rules and shapes are checked against brute-force versions written here, sixteenth by
//! sixteenth from Minecraft's `WallBlock` and `FenceGateBlock`, independent of the crate's.

use boxel_voxel::rules::{
    fence_connects, fence_gate_rule, fence_gate_use, fence_rule, is_sturdy, supports_center,
    torch_rule, use_rule_for_model, wall_rule,
};
use boxel_voxel::shapes::{
    fence_gate_boxes, fence_gate_collision, fence_gate_outline, wall_boxes, wall_collision,
    wall_outline,
};
use boxel_voxel::{
    BlockCollision, BlockModel, BlockRegistry, BlockState, CollisionBox, Face, Facing,
    MAX_COLLISION_HEIGHT, RenderShape, StateFlags, WallSide, WallSides,
};
use std::collections::HashSet;

fn registry() -> BlockRegistry {
    BlockRegistry::default_blocks()
}

fn s(r: &BlockRegistry, name: &str) -> BlockState {
    r.parse_state(name)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// A set of sixteenth cells `(x, y, z)` (`y` up to 24 for collision).
type Cells = HashSet<[u8; 3]>;

/// The sixteenth cells of a box `[x0, y0, z0]..[x1, y1, z1]`.
fn cells(min: [u8; 3], max: [u8; 3]) -> Cells {
    let mut out = Cells::new();
    for x in min[0]..max[0] {
        for y in min[1]..max[1] {
            for z in min[2]..max[2] {
                out.insert([x, y, z]);
            }
        }
    }
    out
}

fn union_of(boxes: &[CollisionBox]) -> Cells {
    boxes.iter().flat_map(|b| cells(b.min, b.max)).collect()
}

/// The cells of `boxes`, and that no two of them share a cell.
fn disjoint_union(boxes: &[CollisionBox]) -> Cells {
    let mut out = Cells::new();
    for b in boxes {
        for c in cells(b.min, b.max) {
            assert!(out.insert(c), "boxes overlap at {c:?}: {boxes:?}");
        }
    }
    out
}

/// A cell turned a quarter clockwise seen from above, `turns` times (east to south: the cell
/// `(x, z)` goes to `(15 - z, x)`).
fn turn_cell(c: [u8; 3], turns: u8) -> [u8; 3] {
    let mut c = c;
    for _ in 0..turns % 4 {
        c = [15 - c[2], c[1], c[0]];
    }
    c
}

/// Quarter turns clockwise from south (Minecraft's gate models face south, `y = 0`; west is
/// `y = 90`, north 180, east 270).
fn turns_from_south(f: Facing) -> u8 {
    match f {
        Facing::South => 0,
        Facing::West => 1,
        Facing::North => 2,
        Facing::East => 3,
    }
}

/// Minecraft's `WallBlock::makeShapes(4, 3, post, 0, low, tall)` for one state, as cells: the
/// post `[4, 0, 4]..[12, post, 12]` when `up`, and per side a bar `5..11` across from the
/// border to 11 sixteenths in (`Block.box(5, 0, 0, 11, h, 11)` toward north).
fn mc_wall_shape(up: bool, sides: [WallSide; 4], post: u8, low: u8, tall: u8) -> Cells {
    let mut out = Cells::new();
    if up {
        out.extend(cells([4, 0, 4], [12, post, 12]));
    }
    for f in Facing::ALL {
        let h = match sides[f as usize] {
            WallSide::None => continue,
            WallSide::Low => low,
            WallSide::Tall => tall,
        };
        let part = match f {
            Facing::North => cells([5, 0, 0], [11, h, 11]),
            Facing::South => cells([5, 0, 5], [11, h, 16]),
            Facing::West => cells([0, 0, 5], [11, h, 11]),
            Facing::East => cells([5, 0, 5], [16, h, 11]),
        };
        out.extend(part);
    }
    out
}

/// Minecraft's wall models (`template_wall_post`, `template_wall_side` from `[5, 0, 0]` to
/// `[11, 14, 8]` and `template_wall_side_tall` to `[11, 16, 8]`, turned per side).
fn mc_wall_model(up: bool, sides: [WallSide; 4]) -> Cells {
    let mut out = Cells::new();
    if up {
        out.extend(cells([4, 0, 4], [12, 16, 12]));
    }
    for f in Facing::ALL {
        let h = match sides[f as usize] {
            WallSide::None => continue,
            WallSide::Low => 14,
            WallSide::Tall => 16,
        };
        // Written for north; north to east is one clockwise turn.
        let turns = match f {
            Facing::North => 0,
            Facing::East => 1,
            Facing::South => 2,
            Facing::West => 3,
        };
        out.extend(
            cells([5, 0, 0], [11, h, 8])
                .into_iter()
                .map(|c| turn_cell(c, turns)),
        );
    }
    out
}

/// Every combination of `up` and the four sides.
fn every_wall() -> Vec<(bool, [WallSide; 4])> {
    let mut out = Vec::new();
    for up in [false, true] {
        for i in 0..81 {
            let mut k = i;
            let sides = std::array::from_fn(|_| {
                let side = WallSide::ALL[k % 3];
                k /= 3;
                side
            });
            out.push((up, sides));
        }
    }
    out
}

fn sides_of(sides: [WallSide; 4]) -> WallSides {
    Facing::ALL
        .into_iter()
        .fold(WallSides::NONE, |acc, f| acc.with(f, sides[f as usize]))
}

#[test]
fn walls_and_gates_have_their_states_and_ids() {
    let r = registry();
    assert_eq!(r.len(), 597);
    let wall = r.block_by_name("boxel:cobblestone_wall").unwrap();
    assert_eq!(r.states(wall).len(), 324);
    assert_eq!(r.states(wall).next(), Some(BlockState(257)));
    assert_eq!(r.default_state(wall), BlockState(263));
    assert_eq!(
        r.state_name(BlockState(263)).to_string(),
        "boxel:cobblestone_wall[east=none,north=none,south=none,up=true,waterlogged=false,\
         west=none]"
    );
    let gate = r.block_by_name("boxel:oak_fence_gate").unwrap();
    assert_eq!(r.states(gate).len(), 16);
    assert_eq!(r.default_state(gate), BlockState(581));
    assert_eq!(
        r.state_name(BlockState(581)).to_string(),
        "boxel:oak_fence_gate[facing=north,in_wall=false,open=false]"
    );
    let cobblestone = r.face_texture(s(&r, "boxel:cobblestone"), Face::PosY);
    let planks = r.face_texture(s(&r, "boxel:planks"), Face::PosY);
    for state in r.states(wall) {
        let BlockModel::Wall { post, sides } = r.model(state) else {
            panic!("wall");
        };
        assert_eq!(r.property(state, "up") == Some("true"), post);
        for f in Facing::ALL {
            assert_eq!(r.property(state, f.name()), Some(sides.get(f).name()));
        }
        assert_eq!(r.shape(state), RenderShape::Opaque);
        assert!(!r.occludes(state), "cave culling sees through walls");
        let wet = r.property(state, "waterlogged") == Some("true");
        assert_eq!(r.is_waterlogged(state), wet);
        assert!(r.can_be_waterlogged(state));
        assert_eq!(
            r.light_opacity(state),
            u8::from(wet),
            "light passes like a fence"
        );
        assert_eq!(r.emission(state), boxel_voxel::LightValue::DARK);
        for face in Face::ALL {
            assert_eq!(r.face_texture(state, face), cobblestone);
        }
        assert_eq!(
            r.collision(state),
            &BlockCollision::Boxes(wall_collision(post, sides))
        );
    }
    for state in r.states(gate) {
        let BlockModel::FenceGate {
            facing,
            open,
            in_wall,
        } = r.model(state)
        else {
            panic!("gate");
        };
        assert_eq!(r.property(state, "facing"), Some(facing.name()));
        assert_eq!(r.property(state, "open") == Some("true"), open);
        assert_eq!(r.property(state, "in_wall") == Some("true"), in_wall);
        assert!(!r.occludes(state));
        assert!(!r.can_be_waterlogged(state), "gates hold no water");
        assert_eq!(r.light_opacity(state), 0);
        assert_eq!(r.face_texture(state, Face::NegZ), planks);
        assert_eq!(
            r.collision(state).is_none(),
            open,
            "open gates let you through"
        );
    }
}

/// The boxes are Minecraft's parts as a union, without overlaps, and a straight wall without a
/// post is a single box.
#[test]
fn wall_boxes_are_minecrafts_parts_without_overlaps() {
    for (up, sides) in every_wall() {
        let ws = sides_of(sides);
        let model: Vec<CollisionBox> = wall_boxes(up, ws).iter().map(|b| b.bounds).collect();
        assert!(model.iter().all(|b| b.is_valid()));
        assert_eq!(
            disjoint_union(&model),
            mc_wall_model(up, sides),
            "{up} {sides:?}"
        );
        let outline = wall_outline(up, ws);
        assert!(outline.iter().all(|b| b.is_valid()));
        assert_eq!(
            disjoint_union(&outline),
            mc_wall_shape(up, sides, 16, 14, 16),
            "{up} {sides:?}"
        );
        let collision = wall_collision(up, ws);
        assert!(collision.iter().all(|b| b.is_valid_collision()));
        assert_eq!(
            disjoint_union(&collision),
            mc_wall_shape(up, sides, 24, 24, 24),
            "{up} {sides:?}"
        );
        assert!(
            model
                .iter()
                .chain(&outline)
                .chain(&collision)
                .all(|b| b.min[1] == 0)
        );
    }
    let straight = |side| {
        sides_of([side, side, WallSide::None, WallSide::None]) // north, south
    };
    for side in [WallSide::Low, WallSide::Tall] {
        assert_eq!(wall_boxes(false, straight(side)).len(), 1);
    }
    // A post and four sides: the post and four short bars.
    let all = sides_of([WallSide::Low; 4]);
    assert_eq!(wall_boxes(true, all).len(), 5);
}

/// Minecraft's fence gate models, as cells, facing `facing`: `template_fence_gate` (closed) or
/// `_open`, with the `_wall` variants 3 lower, turned from south.
fn mc_gate_model(facing: Facing, open: bool, in_wall: bool) -> Cells {
    let closed: &[([u8; 3], [u8; 3])] = &[
        ([0, 5, 7], [2, 16, 9]),
        ([14, 5, 7], [16, 16, 9]),
        ([6, 6, 7], [8, 15, 9]),
        ([8, 6, 7], [10, 15, 9]),
        ([2, 6, 7], [6, 9, 9]),
        ([2, 12, 7], [6, 15, 9]),
        ([10, 6, 7], [14, 9, 9]),
        ([10, 12, 7], [14, 15, 9]),
    ];
    let opened: &[([u8; 3], [u8; 3])] = &[
        ([0, 5, 7], [2, 16, 9]),
        ([14, 5, 7], [16, 16, 9]),
        ([0, 6, 13], [2, 15, 15]),
        ([14, 6, 13], [16, 15, 15]),
        ([0, 6, 9], [2, 9, 13]),
        ([0, 12, 9], [2, 15, 13]),
        ([14, 6, 9], [16, 9, 13]),
        ([14, 12, 9], [16, 15, 13]),
    ];
    let drop = if in_wall { 3 } else { 0 };
    let parts = if open { opened } else { closed };
    parts
        .iter()
        .flat_map(|&(lo, hi)| cells([lo[0], lo[1] - drop, lo[2]], [hi[0], hi[1] - drop, hi[2]]))
        .map(|c| turn_cell(c, turns_from_south(facing)))
        .collect()
}

#[test]
fn gate_boxes_outline_and_collision_are_minecrafts() {
    for facing in Facing::ALL {
        for open in [false, true] {
            for in_wall in [false, true] {
                let model: Vec<CollisionBox> = fence_gate_boxes(facing, open, in_wall)
                    .iter()
                    .map(|b| b.bounds)
                    .collect();
                assert!(model.iter().all(|b| b.is_valid()));
                assert_eq!(
                    disjoint_union(&model),
                    mc_gate_model(facing, open, in_wall),
                    "{facing:?} open {open} in_wall {in_wall}"
                );
                let m = BlockModel::FenceGate {
                    facing,
                    open,
                    in_wall,
                };
                // Minecraft's shapes: 4 thick across the facing, the outline the same open or
                // closed (13 tall between walls), the collision 24 tall when closed.
                let across = |top: u8| match facing {
                    Facing::North | Facing::South => cells([0, 0, 6], [16, top, 10]),
                    Facing::West | Facing::East => cells([6, 0, 0], [10, top, 16]),
                };
                let BlockCollision::Boxes(outline) = m.outline() else {
                    panic!("picked by a box");
                };
                assert_eq!(union_of(&outline), across(if in_wall { 13 } else { 16 }));
                assert_eq!(outline, vec![fence_gate_outline(facing, in_wall)]);
                match m.default_collision() {
                    BlockCollision::None => assert!(open),
                    BlockCollision::Boxes(b) => {
                        assert!(!open);
                        assert_eq!(b, vec![fence_gate_collision(facing)]);
                        assert_eq!(union_of(&b), across(MAX_COLLISION_HEIGHT));
                    }
                    BlockCollision::Full => panic!("not a cube"),
                }
            }
        }
    }
}

/// A gate between walls is the free gate 3 sixteenths lower and shows the same texels:
/// Minecraft's `template_fence_gate_wall` models move the elements down and keep the UVs of
/// `template_fence_gate`. Each box moved by its texture offset is the free gate's box (as the
/// raised wall torch shows the standing torch's texels).
#[test]
fn a_gate_between_walls_shows_the_texels_of_a_free_gate() {
    for facing in Facing::ALL {
        for open in [false, true] {
            let free = fence_gate_boxes(facing, open, false);
            let in_wall = fence_gate_boxes(facing, open, true);
            assert_eq!(free.len(), in_wall.len());
            for (f, w) in free.iter().zip(&in_wall) {
                let what = format!("{facing:?} open {open}: {w:?}");
                assert_eq!(f.texture_offset, [0; 3], "{what}");
                assert!(w.is_valid(), "{what}");
                let moved = |c: [u8; 3]| {
                    [0, 1, 2].map(|a| (i16::from(c[a]) + i16::from(w.texture_offset[a])) as u8)
                };
                assert_eq!(
                    CollisionBox::new(moved(w.bounds.min), moved(w.bounds.max)),
                    f.bounds,
                    "{what}"
                );
                assert_eq!(w.bounds.min[1] + 3, f.bounds.min[1], "{what}");
            }
        }
    }
}

/// Every fence gate stops movement, open or closed (Minecraft's gates are solid wood: fluids
/// never flow into one), though an open one does not collide.
#[test]
fn gates_block_motion_open_or_closed() {
    let r = registry();
    let gate = r.block(s(&r, "boxel:oak_fence_gate")).unwrap();
    for state in r.states(gate) {
        let open = r.property(state, "open") == Some("true");
        assert!(r.blocks_motion(state), "{}", r.state_name(state));
        assert_eq!(
            r.flags(state).contains(StateFlags::SOLID),
            !open,
            "{}",
            r.state_name(state)
        );
    }
    for name in [
        "boxel:air",
        "boxel:water",
        "boxel:torch",
        "boxel:short_grass",
    ] {
        assert!(!r.blocks_motion(s(&r, name)), "{name}");
    }
    for name in ["boxel:stone", "boxel:cobblestone_wall", "boxel:oak_fence"] {
        assert!(r.blocks_motion(s(&r, name)), "{name}");
    }
}

/// A brute-force sturdy face: every sixteenth of the face's border layer is in the collision.
fn sturdy(r: &BlockRegistry, state: BlockState, face: Face) -> bool {
    let set: Cells = match r.collision(state) {
        BlockCollision::None => return false,
        BlockCollision::Full => return true,
        BlockCollision::Boxes(b) => union_of(b),
    };
    (0..16u8).all(|a| {
        (0..16u8).all(|b| {
            let c = match face {
                Face::PosX => [15, a, b],
                Face::NegX => [0, a, b],
                Face::PosZ => [a, b, 15],
                Face::NegZ => [a, b, 0],
                Face::PosY => [a, 15, b],
                Face::NegY => [a, 0, b],
            };
            set.contains(&c)
        })
    })
}

/// The bottom face cells `(x, z)` of `state`'s collision (Minecraft's `getFaceShape(DOWN)`).
fn bottom_face(r: &BlockRegistry, state: BlockState) -> HashSet<[u8; 2]> {
    let set: Cells = match r.collision(state) {
        BlockCollision::None => Cells::new(),
        BlockCollision::Full => cells([0; 3], [16; 3]),
        BlockCollision::Boxes(b) => union_of(b),
    };
    set.into_iter()
        .filter(|c| c[1] == 0)
        .map(|c| [c[0], c[2]])
        .collect()
}

/// Minecraft's `isCovered(aboveShape, test)`: every cell of the test rectangle
/// `[x0, x1, z0, z1]` is in the face.
fn covered(face: &HashSet<[u8; 2]>, t: [u8; 4]) -> bool {
    (t[0]..t[1]).all(|x| (t[2]..t[3]).all(|z| face.contains(&[x, z])))
}

/// Minecraft's `WallBlock` rule, written from its source: the state a wall takes with these
/// neighbors (indexed by `Face::index`), as a state name.
fn mc_wall_state(r: &BlockRegistry, wall: BlockState, n: &[BlockState; 6]) -> String {
    let is = |st: BlockState, name: &str| r.name(st) == name;
    // connectsTo(state, faceSturdy, direction).
    let connects = |toward: Facing| {
        let st = n[toward.face().index()];
        if is(st, "boxel:cobblestone_wall") {
            return true;
        }
        if is(st, "boxel:oak_fence_gate") {
            // FenceGateBlock.connectsToDirection: the gate's facing axis is the clockwise turn
            // of the direction from the neighbor to the wall.
            let gate = Facing::from_name(r.property(st, "facing").unwrap()).unwrap();
            let dir = toward.opposite().clockwise();
            return gate == dir || gate == dir.opposite();
        }
        let exception = is(st, "boxel:leaves");
        !exception && sturdy(r, st, toward.opposite().face())
    };
    let above = n[Face::PosY.index()];
    let face = bottom_face(r, above);
    let test = |f: Option<Facing>| match f {
        None => [7, 9, 7, 9],
        Some(Facing::North) => [7, 9, 0, 9],
        Some(Facing::South) => [7, 9, 7, 16],
        Some(Facing::West) => [0, 9, 7, 9],
        Some(Facing::East) => [7, 16, 7, 9],
    };
    let side = |f: Facing| {
        if !connects(f) {
            "none"
        } else if covered(&face, test(Some(f))) {
            "tall"
        } else {
            "low"
        }
    };
    let [north, south, west, east] = Facing::ALL.map(side);
    let up = if is(above, "boxel:cobblestone_wall") && r.property(above, "up") == Some("true") {
        true
    } else {
        let (n0, s0, w0, e0) = (
            north == "none",
            south == "none",
            west == "none",
            east == "none",
        );
        if (n0 && s0 && w0 && e0) || n0 != s0 || w0 != e0 {
            true
        } else if (north == "tall" && south == "tall") || (east == "tall" && west == "tall") {
            false
        } else {
            is(above, "boxel:torch") || covered(&face, test(None))
        }
    };
    let wet = r.property(wall, "waterlogged").unwrap();
    format!(
        "boxel:cobblestone_wall[east={east},north={north},south={south},up={up},\
         waterlogged={wet},west={west}]"
    )
}

/// Neighbors worth trying: nothing, full and partial blocks, leaves, walls, gates of every
/// facing (open or not), fences, stairs and torches.
fn candidates(r: &BlockRegistry) -> Vec<BlockState> {
    [
        "boxel:air",
        "boxel:stone",
        "boxel:leaves",
        "boxel:glass",
        "boxel:stone_slab",
        "boxel:stone_slab[type=top]",
        "boxel:oak_stairs[facing=east]",
        "boxel:oak_fence",
        "boxel:oak_fence[north=true,south=true]",
        "boxel:cobblestone_wall",
        "boxel:cobblestone_wall[north=low,south=low,up=false]",
        "boxel:cobblestone_wall[east=tall,west=tall,up=false]",
        "boxel:cobblestone_wall[north=low,east=low]",
        "boxel:oak_fence_gate[facing=north]",
        "boxel:oak_fence_gate[facing=east,open=true]",
        "boxel:oak_fence_gate[facing=south,in_wall=true]",
        "boxel:torch",
        "boxel:wall_torch[facing=north]",
        "boxel:water",
    ]
    .into_iter()
    .map(|name| s(r, name))
    .collect()
}

/// The wall rule against Minecraft's, for every mix of these neighbors on the four sides and
/// above (a pseudo-random sample of them, the same every run), wet and dry.
#[test]
fn wall_rule_matches_minecrafts() {
    let r = registry();
    let c = candidates(&r);
    let walls = [
        s(&r, "boxel:cobblestone_wall"),
        s(
            &r,
            "boxel:cobblestone_wall[north=tall,up=false,waterlogged=true]",
        ),
    ];
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let mut seen_posts = [false; 2];
    let mut seen_sides = HashSet::new();
    for i in 0..10_000 {
        let mut n = [BlockState::AIR; 6];
        for slot in &mut n {
            *slot = c[next(c.len())];
        }
        let wall = walls[i % 2];
        let got = wall_rule(&r, wall, &n);
        let want = mc_wall_state(&r, wall, &n);
        assert_eq!(
            r.state_name(got).to_string(),
            want,
            "neighbors {:?}",
            n.map(|x| r.state_name(x).to_string())
        );
        let BlockModel::Wall { post, sides } = r.model(got) else {
            panic!("a wall stays a wall");
        };
        seen_posts[usize::from(post)] = true;
        seen_sides.insert(sides);
    }
    assert_eq!(seen_posts, [true, true]);
    assert!(seen_sides.len() > 40, "{}", seen_sides.len());
}

/// The rule's cases, by hand: a lone wall, an end, a straight run, a corner, a cross, under a
/// block, under a torch, under a wall with a post.
#[test]
fn wall_posts_and_tall_sides_follow_minecraft() {
    let r = registry();
    let wall = s(&r, "boxel:cobblestone_wall");
    let stone = s(&r, "boxel:stone");
    let torch = s(&r, "boxel:torch");
    let at = |pairs: &[(Face, BlockState)]| {
        let mut n = [BlockState::AIR; 6];
        for &(f, st) in pairs {
            n[f.index()] = st;
        }
        r.state_name(wall_rule(&r, wall, &n)).to_string()
    };
    let name = |sides: &str, up: bool| {
        format!("boxel:cobblestone_wall[{sides}up={up},waterlogged=false,west=none]")
    };
    let (n, so, e, w, up) = (Face::NegZ, Face::PosZ, Face::PosX, Face::NegX, Face::PosY);
    assert_eq!(at(&[]), name("east=none,north=none,south=none,", true));
    // An end: a post.
    assert_eq!(
        at(&[(n, wall)]),
        name("east=none,north=low,south=none,", true)
    );
    // A straight run: no post; a torch on it raises the post, a stone makes it tall.
    assert_eq!(
        at(&[(n, wall), (so, wall)]),
        name("east=none,north=low,south=low,", false)
    );
    assert_eq!(
        at(&[(n, wall), (so, wall), (up, torch)]),
        name("east=none,north=low,south=low,", true)
    );
    assert_eq!(
        at(&[(n, wall), (so, stone), (up, stone)]),
        name("east=none,north=tall,south=tall,", false)
    );
    // A corner: a post.
    assert_eq!(
        at(&[(n, wall), (e, wall)]),
        name("east=low,north=low,south=none,", true)
    );
    // A cross without a post, and with the west side it is the full state name.
    let mut cross = [BlockState::AIR; 6];
    for f in [n, so, e, w] {
        cross[f.index()] = wall;
    }
    assert_eq!(
        r.state_name(wall_rule(&r, wall, &cross)).to_string(),
        "boxel:cobblestone_wall[east=low,north=low,south=low,up=false,waterlogged=false,\
         west=low]"
    );
    // Under a wall with a post, the post stands even when tall on both sides.
    let above = s(&r, "boxel:cobblestone_wall[north=low,south=low,up=true]");
    assert_eq!(
        at(&[(n, wall), (so, wall), (up, above)]),
        name("east=none,north=tall,south=tall,", true)
    );
    let above = s(&r, "boxel:cobblestone_wall[north=low,south=low,up=false]");
    assert_eq!(
        at(&[(n, wall), (so, wall), (up, above)]),
        name("east=none,north=tall,south=tall,", false)
    );
}

#[test]
fn gates_drop_between_walls_and_connect_fences_and_walls() {
    let r = registry();
    let wall = s(&r, "boxel:cobblestone_wall");
    let fence = s(&r, "boxel:oak_fence");
    for facing in Facing::ALL {
        for open in ["false", "true"] {
            let gate = s(
                &r,
                &format!(
                    "boxel:oak_fence_gate[facing={facing},open={open}]",
                    facing = facing.name()
                ),
            );
            // Brute force: a wall on any side; in_wall iff one is at an end of the gate (the
            // two sides across its facing).
            for mask in 0..16u8 {
                let mut n = [BlockState::AIR; 6];
                for (i, f) in Facing::ALL.into_iter().enumerate() {
                    if mask & (1 << i) != 0 {
                        n[f.face().index()] = wall;
                    }
                }
                let ends = [facing.clockwise(), facing.counterclockwise()];
                let want = ends
                    .iter()
                    .any(|&f| mask & (1 << Facing::ALL.iter().position(|&g| g == f).unwrap()) != 0);
                let got = fence_gate_rule(&r, gate, &n);
                assert_eq!(
                    r.property(got, "in_wall"),
                    Some(if want { "true" } else { "false" })
                );
                assert_eq!(r.property(got, "facing"), Some(facing.name()));
                assert_eq!(r.property(got, "open"), Some(open));
                // A fence beside it is not a wall.
                let mut f = n;
                for slot in &mut f {
                    if *slot == wall {
                        *slot = fence;
                    }
                }
                assert_eq!(
                    r.property(fence_gate_rule(&r, gate, &f), "in_wall"),
                    Some("false")
                );
            }
            // Fences and walls connect to the gate's ends, not to its faces.
            for toward in Facing::ALL {
                let end = toward == facing.clockwise() || toward == facing.counterclockwise();
                assert_eq!(
                    fence_connects(&r, gate, toward),
                    end,
                    "{facing:?} {toward:?}"
                );
                assert_eq!(
                    boxel_voxel::rules::wall_connects(&r, gate, toward),
                    end,
                    "{facing:?} {toward:?}"
                );
            }
        }
    }
    // A fence between two gate ends, by the rule.
    let gate = s(&r, "boxel:oak_fence_gate[facing=north]");
    let mut n = [BlockState::AIR; 6];
    n[Face::PosX.index()] = gate;
    n[Face::NegX.index()] = wall;
    assert_eq!(
        r.state_name(fence_rule(&r, fence, &n)).to_string(),
        "boxel:oak_fence[east=true,north=false,south=false,waterlogged=false,west=false]",
        "a fence connects to a gate, not to a wall"
    );
}

#[test]
fn using_a_gate_opens_it_away_from_the_user_and_closes_it() {
    let r = registry();
    assert!(use_rule_for_model(r.model(s(&r, "boxel:oak_fence_gate"))).is_some());
    for name in [
        "boxel:stone",
        "boxel:oak_fence",
        "boxel:cobblestone_wall",
        "boxel:torch",
    ] {
        assert!(use_rule_for_model(r.model(s(&r, name))).is_none(), "{name}");
        assert_eq!(fence_gate_use(&r, s(&r, name), Facing::North), None);
    }
    for facing in Facing::ALL {
        for in_wall in ["false", "true"] {
            let closed = s(
                &r,
                &format!(
                    "boxel:oak_fence_gate[facing={},in_wall={in_wall}]",
                    facing.name()
                ),
            );
            for look in Facing::ALL {
                let open = fence_gate_use(&r, closed, look).unwrap();
                // Minecraft: it opens away from the user, turning around if it faced them.
                let want = if look == facing.opposite() {
                    look
                } else {
                    facing
                };
                assert_eq!(r.property(open, "facing"), Some(want.name()));
                assert_eq!(r.property(open, "open"), Some("true"));
                assert_eq!(r.property(open, "in_wall"), Some(in_wall));
                assert!(r.collision(open).is_none());
                // Using it again closes it where it is.
                let shut = fence_gate_use(&r, open, look).unwrap();
                assert_eq!(r.property(shut, "facing"), Some(want.name()));
                assert_eq!(r.property(shut, "open"), Some("false"));
                assert!(!r.collision(shut).is_none());
            }
        }
    }
}

/// Walls and closed gates collide 1.5 blocks tall and are picked a block tall (13 sixteenths a
/// gate between walls); their faces are not sturdy; a wall's top holds a torch.
#[test]
fn walls_and_gates_collide_a_block_and_a_half_tall() {
    let r = registry();
    let torch = s(&r, "boxel:torch");
    let wall = r.block_by_name("boxel:cobblestone_wall").unwrap();
    for state in r.states(wall) {
        let BlockCollision::Boxes(collision) = r.collision(state) else {
            panic!("walls collide with boxes");
        };
        if collision.is_empty() {
            continue; // no post and no side: nothing
        }
        let top = collision.iter().map(|b| b.max[1]).max().unwrap();
        assert_eq!(top, MAX_COLLISION_HEIGHT);
        let BlockCollision::Boxes(outline) = r.model(state).outline() else {
            panic!("picked by boxes");
        };
        assert!(outline.iter().all(|b| b.is_valid() && b.max[1] <= 16));
        for face in Face::ALL {
            assert!(!is_sturdy(&r, state, face), "{face:?}");
            assert_eq!(is_sturdy(&r, state, face), sturdy(&r, state, face));
        }
        // Every state the rule makes holds a torch (a post, or a bar through the middle).
        let BlockModel::Wall { post, sides } = r.model(state) else {
            panic!("wall");
        };
        let ns = sides.get(Facing::North).is_connected() && sides.get(Facing::South).is_connected();
        let we = sides.get(Facing::West).is_connected() && sides.get(Facing::East).is_connected();
        if post || ns || we {
            assert!(supports_center(&r, state, Face::PosY));
            let mut n = [BlockState::AIR; 6];
            n[Face::NegY.index()] = state;
            assert_eq!(torch_rule(&r, torch, &n), torch);
        }
    }
    let lone = s(&r, "boxel:cobblestone_wall[up=false]");
    assert!(
        r.collision(lone).is_none(),
        "no post, no side: nothing, as in Minecraft"
    );
    for facing in Facing::ALL {
        let gate = s(
            &r,
            &format!("boxel:oak_fence_gate[facing={}]", facing.name()),
        );
        let BlockCollision::Boxes(b) = r.collision(gate) else {
            panic!("a closed gate collides");
        };
        assert_eq!(b[0].max[1], MAX_COLLISION_HEIGHT);
        assert!(b[0].is_valid_collision() && !b[0].is_valid());
        for face in Face::ALL {
            assert!(!is_sturdy(&r, gate, face));
        }
    }
}

#[test]
fn the_default_blocks_still_fit_the_box_table() {
    let r = registry();
    let mut boxes = HashSet::new();
    for i in 0..r.len() {
        boxes.extend(r.model(BlockState(i as u16)).boxes());
    }
    assert!(boxes.len() <= boxel_voxel::MAX_MODEL_BOXES);
    // Walls: their cut-up bars and posts; gates: 7 or 8 boxes per facing, half again in walls.
    assert!(boxes.len() < 256, "{}", boxes.len());
}

/// Cave culling (ADR 0022) sees through walls and gates: a chunk of them, between stone layers,
/// links its sides like air does.
#[test]
fn cave_culling_counts_walls_and_gates_open() {
    use boxel_voxel::{ALL_LINKS, CHUNK_VOLUME, ChunkData, ChunkFaceInfo, LocalPos, link_bit};
    let r = registry();
    let blocks = [
        s(&r, "boxel:cobblestone_wall[north=tall,south=tall,up=false]"),
        s(&r, "boxel:cobblestone_wall"),
        s(&r, "boxel:oak_fence_gate[facing=east,in_wall=true]"),
        s(&r, "boxel:oak_fence_gate[open=true]"),
    ];
    let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
    for (i, slot) in dense.iter_mut().enumerate() {
        *slot = blocks[i % blocks.len()];
    }
    let walls = ChunkData::from_dense(&dense);
    assert_eq!(ChunkFaceInfo::compute(&walls, &r).links, ALL_LINKS);
    // The same with stone on every other layer: still open sideways, through the wall layers.
    let stone = s(&r, "boxel:stone");
    for (i, slot) in dense.iter_mut().enumerate() {
        if LocalPos::from_index(i).y().is_multiple_of(2) {
            *slot = stone;
        }
    }
    let info = ChunkFaceInfo::compute(&ChunkData::from_dense(&dense), &r);
    assert_ne!(info.links & link_bit(Face::PosX, Face::NegX), 0);
    assert_eq!(info.links & link_bit(Face::PosY, Face::NegY), 0);
}
