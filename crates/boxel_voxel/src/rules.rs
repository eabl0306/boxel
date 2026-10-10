//! States that follow their neighbors (ADR 0020), as pure functions of the registry, the state
//! and its 6 face neighbors: the `shape` of stairs, the sides of a fence, and torches that drop
//! when their support goes. `boxel_world` applies them to the blocks around every edit
//! (`NeighborRules`); they are plain functions so they can be tested without a world.
//!
//! A rule only reads what no rule changes (whether a neighbor is stairs and its `facing` and
//! `half`, whether it is a fence, its collision faces), so one ring of updates around each edit
//! is enough: a state a rule changed never makes another rule change its answer.
//!
//! Also the water of the blocks that hold it when a player places or breaks one (ADR 0025):
//! [`placed_into`] and [`broken`].
use crate::shapes::{Facing, StairShape, half_name};
use crate::{BlockCollision, BlockModel, BlockRegistry, BlockState, Face, RenderShape, SlabHalf};

/// A neighbor rule: the state `state` should have given its 6 face neighbors (indexed by
/// [`Face::index`]). Returning `state` changes nothing; returning air removes the block.
pub type NeighborRule = fn(&BlockRegistry, BlockState, &[BlockState; 6]) -> BlockState;

/// The rule of the blocks drawn as `model`, if their state depends on their neighbors: stairs
/// ([`stairs_rule`]), fences ([`fence_rule`]), standing and wall torches ([`torch_rule`],
/// [`wall_torch_rule`]).
pub fn rule_for_model(model: BlockModel) -> Option<NeighborRule> {
    match model {
        BlockModel::Stairs { .. } => Some(stairs_rule),
        BlockModel::Fence { .. } => Some(fence_rule),
        BlockModel::Torch => Some(torch_rule),
        BlockModel::WallTorch { .. } => Some(wall_torch_rule),
        BlockModel::Cube { .. } | BlockModel::Slab(_) | BlockModel::Cross | BlockModel::Fluid => {
            None
        }
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
/// fence, or a block whose face toward the fence is sturdy, except cutout blocks (leaves).
pub fn fence_connects(registry: &BlockRegistry, neighbor: BlockState, toward: Facing) -> bool {
    match registry.model(neighbor) {
        BlockModel::Fence { .. } => true,
        _ => {
            registry.shape(neighbor) != RenderShape::Cutout
                && is_sturdy(registry, neighbor, toward.opposite().face())
        }
    }
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
