//! States that follow their neighbors (ADR 0020, ADR 0027), as pure functions of the registry,
//! the state and its 6 face neighbors: the `shape` of stairs, the sides of a fence, torches that
//! drop when their support goes, the sides and post of a wall and a fence gate between walls.
//! `boxel_world` applies them to the blocks around every edit (`NeighborRules`), and again
//! around every block a rule changed; they are plain functions so they can be tested without a
//! world.
//!
//! Most rules only read what no rule changes (whether a neighbor is stairs and its `facing` and
//! `half`, whether it is a fence or a wall, the `facing` of a gate, the collision faces of
//! blocks without rules). A wall also reads the block above it: its collision and, for another
//! wall, its post. A change runs down a column of walls one block at a time, so the world
//! applies the rules again around what a rule changed (ADR 0027). Walls only read upward, so
//! that ends.
//!
//! Also the water of the blocks that hold it when a player places or breaks one (ADR 0025):
//! [`placed_into`] and [`broken`]; and what using a block does ([`UseRule`], ADR 0027): a fence
//! gate opens and closes.
use crate::shapes::{Facing, StairShape, WallSide, WallSides, half_name};
use crate::{BlockCollision, BlockModel, BlockRegistry, BlockState, Face, RenderShape, SlabHalf};

/// A neighbor rule: the state `state` should have given its 6 face neighbors (indexed by
/// [`Face::index`]). Returning `state` changes nothing; returning air removes the block.
pub type NeighborRule = fn(&BlockRegistry, BlockState, &[BlockState; 6]) -> BlockState;

/// The rule of the blocks drawn as `model`, if their state depends on their neighbors: stairs
/// ([`stairs_rule`]), fences ([`fence_rule`]), standing and wall torches ([`torch_rule`],
/// [`wall_torch_rule`]), walls ([`wall_rule`]) and fence gates ([`fence_gate_rule`]).
pub fn rule_for_model(model: BlockModel) -> Option<NeighborRule> {
    match model {
        BlockModel::Stairs { .. } => Some(stairs_rule),
        BlockModel::Fence { .. } => Some(fence_rule),
        BlockModel::Torch => Some(torch_rule),
        BlockModel::WallTorch { .. } => Some(wall_torch_rule),
        BlockModel::Wall { .. } => Some(wall_rule),
        BlockModel::FenceGate { .. } => Some(fence_gate_rule),
        BlockModel::Cube { .. } | BlockModel::Slab(_) | BlockModel::Cross | BlockModel::Fluid => {
            None
        }
    }
}

/// What using a block (a player's right click on it) does: the state it takes when used by
/// someone looking toward `look` (horizontally), or `None` if the block is not used that way
/// (then the click places a block, as before). Engine level, like [`NeighborRule`]: the world
/// keeps one per block (`boxel_world::BlockUses`) and the game asks it.
pub type UseRule = fn(&BlockRegistry, BlockState, Facing) -> Option<BlockState>;

/// The use rule of the blocks drawn as `model`: fence gates open and close
/// ([`fence_gate_use`]).
pub fn use_rule_for_model(model: BlockModel) -> Option<UseRule> {
    match model {
        BlockModel::FenceGate { .. } => Some(fence_gate_use),
        _ => None,
    }
}

/// The `facing` and `half` of stairs (`None` for anything else).
pub fn stairs_of(registry: &BlockRegistry, state: BlockState) -> Option<(Facing, SlabHalf)> {
    match registry.model(state) {
        BlockModel::Stairs { facing, half, .. } => Some((facing, half)),
        _ => None,
    }
}

/// True if `a` and `b` lie on the same horizontal axis.
fn same_axis(a: Facing, b: Facing) -> bool {
    a == b || a == b.opposite()
}

/// The shape stairs with `facing` and `half` take from their neighbors, as Minecraft's
/// `StairBlock::getStairsShape`: stairs of the same half on the high side (toward `facing`)
/// facing across make an outer corner, toward the side they face; failing that, stairs of the
/// same half on the low side facing across make an inner corner. A corner is not taken when the
/// side it would open toward has stairs like these ones (they continue the straight run).
pub fn stair_shape(
    registry: &BlockRegistry,
    facing: Facing,
    half: SlabHalf,
    neighbors: &[BlockState; 6],
) -> StairShape {
    let at = |f: Facing| neighbors[f.face().index()];
    // The stairs toward `side` do not continue this run (another facing or half, or none).
    let can_take = |side: Facing| stairs_of(registry, at(side)) != Some((facing, half));
    if let Some((front, front_half)) = stairs_of(registry, at(facing))
        && front_half == half
        && !same_axis(front, facing)
        && can_take(front.opposite())
    {
        return if front == facing.counterclockwise() {
            StairShape::OuterLeft
        } else {
            StairShape::OuterRight
        };
    }
    if let Some((back, back_half)) = stairs_of(registry, at(facing.opposite()))
        && back_half == half
        && !same_axis(back, facing)
        && can_take(back)
    {
        return if back == facing.counterclockwise() {
            StairShape::InnerLeft
        } else {
            StairShape::InnerRight
        };
    }
    StairShape::Straight
}

/// Stairs take the [`stair_shape`] of their neighbors (other states are kept).
pub fn stairs_rule(
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    let Some((facing, half)) = stairs_of(registry, state) else {
        return state;
    };
    let shape = stair_shape(registry, facing, half, neighbors);
    registry.with(state, "shape", shape.name()).unwrap_or(state)
}

/// Which sixteenths of `face` of a block colliding as `collision` its boxes cover, flush with
/// the cell border: bit `u` of row `v` along the face's axes ([`Face::axes`]). A box that
/// reaches into the cell above (a fence's 1.5 blocks, ADR 0026) covers the top face with its
/// cross-section, as Minecraft cuts a shape at the face; the part above the cell is left out.
pub fn collision_face_cover(collision: &BlockCollision, face: Face) -> [u16; 16] {
    match collision {
        BlockCollision::None => [0; 16],
        BlockCollision::Full => [u16::MAX; 16],
        BlockCollision::Boxes(boxes) => {
            let (n, u, v) = face.axes();
            let positive = face.index().is_multiple_of(2);
            let mut rows = [0u16; 16];
            for b in boxes {
                let flush = if positive {
                    b.max[n] >= 16
                } else {
                    b.min[n] == 0
                };
                if !flush {
                    continue;
                }
                let bits = span_bits(b.min[u], b.max[u].min(16));
                for row in &mut rows[usize::from(b.min[v])..usize::from(b.max[v].min(16))] {
                    *row |= bits;
                }
            }
            rows
        }
    }
}

/// Bits `lo..hi` of a row of sixteenths.
fn span_bits(lo: u8, hi: u8) -> u16 {
    ((1u32 << hi) - (1u32 << lo)) as u16
}

/// True if `face` of `state` is a sturdy face (Minecraft's "full face"): its collision covers
/// the whole face. A full cube, the back of stairs, the bottom of a bottom slab.
pub fn is_sturdy(registry: &BlockRegistry, state: BlockState, face: Face) -> bool {
    collision_face_cover(registry.collision(state), face) == [u16::MAX; 16]
}

/// True if `face` of `state` can hold something at its center (Minecraft's `CENTER` support):
/// its collision covers the middle 2x2 sixteenths of the face (a fence post's top does).
pub fn supports_center(registry: &BlockRegistry, state: BlockState, face: Face) -> bool {
    let rows = collision_face_cover(registry.collision(state), face);
    let mid = span_bits(7, 9);
    rows[7] & mid == mid && rows[8] & mid == mid
}

/// True if a fence connects toward `toward` to `neighbor`, Minecraft's rule simplified: another
/// fence, a fence gate that runs across `toward` ([`gate_connects`]), or a block whose face
/// toward the fence is sturdy, except cutout blocks (leaves). Not a wall: its sides are not
/// sturdy, as in Minecraft.
pub fn fence_connects(registry: &BlockRegistry, neighbor: BlockState, toward: Facing) -> bool {
    match registry.model(neighbor) {
        BlockModel::Fence { .. } => true,
        BlockModel::FenceGate { facing, .. } => gate_connects(facing, toward),
        _ => sturdy_toward(registry, neighbor, toward),
    }
}

/// `neighbor`'s face toward a block on its `toward.opposite()` side is sturdy, and it is not a
/// cutout block (leaves: Minecraft's `isExceptionForConnection`).
fn sturdy_toward(registry: &BlockRegistry, neighbor: BlockState, toward: Facing) -> bool {
    registry.shape(neighbor) != RenderShape::Cutout
        && is_sturdy(registry, neighbor, toward.opposite().face())
}

/// True if a fence gate facing `gate` connects to a fence or a wall on its `toward.opposite()`
/// side: the gate runs across its facing, so its ends are on the two sides across it
/// (Minecraft's `FenceGateBlock::connectsToDirection`). Open or closed.
pub const fn gate_connects(gate: Facing, toward: Facing) -> bool {
    !matches!(
        (gate, toward),
        (Facing::North | Facing::South, Facing::North | Facing::South)
            | (Facing::West | Facing::East, Facing::West | Facing::East)
    )
}

/// True if a wall connects toward `toward` to `neighbor` (Minecraft's
/// `WallBlock::connectsTo`): another wall, a fence gate that runs across `toward`
/// ([`gate_connects`]), or a sturdy face toward the wall, except cutout blocks (leaves). Not a
/// fence.
pub fn wall_connects(registry: &BlockRegistry, neighbor: BlockState, toward: Facing) -> bool {
    match registry.model(neighbor) {
        BlockModel::Wall { .. } => true,
        BlockModel::FenceGate { facing, .. } => gate_connects(facing, toward),
        _ => sturdy_toward(registry, neighbor, toward),
    }
}

/// The part of the bottom face of the block above a wall that Minecraft tests to make the wall
/// tall or raise its post, as `[x0, x1, z0, z1]` in sixteenths: 2 wide through the middle of the
/// cell, from the middle (`POST_TEST`) to each side (`NORTH_TEST`...).
const fn wall_test(toward: Option<Facing>) -> [u8; 4] {
    match toward {
        None => [7, 9, 7, 9],
        Some(Facing::North) => [7, 9, 0, 9],
        Some(Facing::South) => [7, 9, 7, 16],
        Some(Facing::West) => [0, 9, 7, 9],
        Some(Facing::East) => [7, 16, 7, 9],
    }
}

/// True if `cover` (a bottom face cover, [`collision_face_cover`]: rows along z, bits along x)
/// holds every sixteenth of `rect` (`[x0, x1, z0, z1]`).
fn covers(cover: &[u16; 16], rect: [u8; 4]) -> bool {
    let bits = span_bits(rect[0], rect[1]);
    cover[usize::from(rect[2])..usize::from(rect[3])]
        .iter()
        .all(|&row| row & bits == bits)
}

/// The post and sides of a wall (Minecraft's `WallBlock::updateShape`), given which sides
/// connect ([`wall_connects`], indexed by `Facing as usize`) and the block above it. A connected
/// side is tall when the bottom of the block above covers it (the collision of what is above,
/// cut at its bottom face: a block, a wall or a fence above), else low. The post stands:
/// - under a wall with a post;
/// - else at an end, a corner, a T or alone: when north and south differ in being connected,
///   east and west differ, or nothing connects;
/// - else (a straight run or a cross) not when an opposite pair is tall on both sides, and
///   otherwise when a standing torch is above (Minecraft's `wall_post_override` tag) or the
///   bottom of the block above covers the middle of the cell.
pub fn wall_shape_of(
    registry: &BlockRegistry,
    connected: [bool; 4],
    above: BlockState,
) -> (bool, WallSides) {
    let cover = collision_face_cover(registry.collision(above), Face::NegY);
    let mut sides = WallSides::NONE;
    for f in Facing::ALL {
        let side = if !connected[f as usize] {
            WallSide::None
        } else if covers(&cover, wall_test(Some(f))) {
            WallSide::Tall
        } else {
            WallSide::Low
        };
        sides = sides.with(f, side);
    }
    let above_model = registry.model(above);
    let post = if matches!(above_model, BlockModel::Wall { post: true, .. })
        && registry.shape(above) != RenderShape::None
    {
        true
    } else {
        let none = |f: Facing| !sides.get(f).is_connected();
        let tall = |f: Facing| sides.get(f) == WallSide::Tall;
        let (n, s, w, e) = (Facing::North, Facing::South, Facing::West, Facing::East);
        let lone = none(n) && none(s) && none(w) && none(e);
        if lone || none(n) != none(s) || none(w) != none(e) {
            true
        } else if (tall(n) && tall(s)) || (tall(w) && tall(e)) {
            false
        } else {
            let torch =
                above_model == BlockModel::Torch && registry.shape(above) != RenderShape::None;
            torch || covers(&cover, wall_test(None))
        }
    };
    (post, sides)
}

/// A wall takes its sides and post from its neighbors ([`wall_connects`], [`wall_shape_of`]);
/// other states (`waterlogged`) are kept.
pub fn wall_rule(
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    if !matches!(registry.model(state), BlockModel::Wall { .. }) {
        return state;
    }
    // `Facing::ALL` is in `Facing as usize` order.
    let connected = Facing::ALL.map(|f| wall_connects(registry, neighbors[f.face().index()], f));
    let (post, sides) = wall_shape_of(registry, connected, neighbors[Face::PosY.index()]);
    let mut out = registry
        .with(state, "up", if post { "true" } else { "false" })
        .unwrap_or(state);
    for f in Facing::ALL {
        out = registry
            .with(out, f.name(), sides.get(f).name())
            .unwrap_or(out);
    }
    out
}

/// True for a wall (any state of a block drawn as [`BlockModel::Wall`]).
pub fn is_wall(registry: &BlockRegistry, state: BlockState) -> bool {
    matches!(registry.model(state), BlockModel::Wall { .. })
        && registry.shape(state) != RenderShape::None
}

/// A fence gate is `in_wall` when a wall is at one of its ends, the two sides across its
/// facing (Minecraft's `FenceGateBlock::updateShape`); other states are kept.
pub fn fence_gate_rule(
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    let BlockModel::FenceGate { facing, .. } = registry.model(state) else {
        return state;
    };
    let wall = |f: Facing| is_wall(registry, neighbors[f.face().index()]);
    let in_wall = wall(facing.clockwise()) || wall(facing.counterclockwise());
    registry
        .with(state, "in_wall", if in_wall { "true" } else { "false" })
        .unwrap_or(state)
}

/// Using a fence gate (Minecraft's `FenceGateBlock::useWithoutItem`): an open gate closes; a
/// closed one opens away from the user, who looks toward `look`, turning it around when it
/// faces the user.
pub fn fence_gate_use(
    registry: &BlockRegistry,
    state: BlockState,
    look: Facing,
) -> Option<BlockState> {
    let BlockModel::FenceGate { facing, open, .. } = registry.model(state) else {
        return None;
    };
    if open {
        return registry.with(state, "open", "false");
    }
    let turned = if facing == look.opposite() {
        registry.with(state, "facing", look.name())?
    } else {
        state
    };
    registry.with(turned, "open", "true")
}

/// A fence connects its sides to the neighbors [`fence_connects`] accepts (other states are
/// kept).
pub fn fence_rule(
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    if !matches!(registry.model(state), BlockModel::Fence { .. }) {
        return state;
    }
    let mut out = state;
    for f in Facing::ALL {
        let connected = fence_connects(registry, neighbors[f.face().index()], f);
        out = registry
            .with(out, f.name(), if connected { "true" } else { "false" })
            .unwrap_or(out);
    }
    out
}

/// A standing torch drops (becomes air) without a block under it that supports its center.
pub fn torch_rule(
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    if supports_center(registry, neighbors[Face::NegY.index()], Face::PosY) {
        state
    } else {
        BlockState::AIR
    }
}

/// A wall torch drops (becomes air) without a sturdy face behind it.
pub fn wall_torch_rule(
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    let BlockModel::WallTorch { facing } = registry.model(state) else {
        return state;
    };
    let behind = neighbors[facing.opposite().face().index()];
    if is_sturdy(registry, behind, facing.face()) {
        state
    } else {
        BlockState::AIR
    }
}

/// The stairs state of block `stairs` (any of its states) facing `facing` with `half`, the other
/// properties kept: what placing stairs sets before their shape is derived.
pub fn stairs_with(
    registry: &BlockRegistry,
    stairs: BlockState,
    facing: Facing,
    half: SlabHalf,
) -> Option<BlockState> {
    let s = registry.with(stairs, "facing", facing.name())?;
    registry.with(s, "half", half_name(half))
}

/// The water source of `registry` (`boxel:water`, its default state: level 0), if it has one.
fn water_source(registry: &BlockRegistry) -> Option<BlockState> {
    registry
        .by_name("boxel:water")
        .filter(|&w| registry.model(w) == BlockModel::Fluid)
}

/// The state `placed` takes in a cell that holds `existing` (Minecraft's `getStateForPlacement`
/// of the blocks that hold water, ADR 0025): a block that can be waterlogged keeps a water
/// source it is placed into and is dry anywhere else (flowing water is replaced, not kept).
/// Other blocks are `placed` as it is.
pub fn placed_into(
    registry: &BlockRegistry,
    placed: BlockState,
    existing: BlockState,
) -> BlockState {
    if !registry.can_be_waterlogged(placed) {
        return placed;
    }
    let wet = water_source(registry) == Some(existing);
    registry.with_water(placed, wet).unwrap_or(placed)
}

/// What breaking `state` leaves in its cell (Minecraft keeps a block's fluid when it breaks,
/// ADR 0025): a water source for a waterlogged block, air for anything else.
pub fn broken(registry: &BlockRegistry, state: BlockState) -> BlockState {
    if registry.is_waterlogged(state) {
        water_source(registry).unwrap_or(BlockState::AIR)
    } else {
        BlockState::AIR
    }
}
