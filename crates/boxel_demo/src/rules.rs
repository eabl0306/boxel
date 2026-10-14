//! The rules of the demo blocks (ADR 0020, ADR 0027), as functions of the registry, the state
//! and its 6 face neighbors: the `shape` of stairs, the sides of a fence, torches that drop when
//! their support goes, the sides and post of a wall and a fence gate between walls; and what
//! using a block does (a gate opens). [`crate::blocks::builder`] registers them on their blocks
//! (`BlockRegistryBuilder::set_neighbor_rule`, `set_use_rule`); the world applies them.
//!
//! The rules find their partners by kind ([`Kinds`]: which blocks are stairs, fences, walls,
//! gates, wall torches, and the standing torch a wall raises its post under) and by their state
//! properties (`facing`, `half`, `up`, `open`, `in_wall`), the properties their shapes are built
//! from.
//!
//! Most rules only read what no rule changes (whether a neighbor is stairs and its `facing` and
//! `half`, whether it is a fence or a wall, the `facing` of a gate, the support faces of
//! blocks without rules, ADR 0037). A wall also reads the block above it: its collision and, for
//! another wall, its post. A change runs down a column of walls one block at a time, so the world
//! applies the rules again around what a rule changed (ADR 0027). Walls only read upward, so
//! that ends.

use boxel_voxel::rules::{collision_face_cover, is_sturdy, span_bits, supports_center};
use boxel_voxel::shapes::{half_from_name, half_name};
use boxel_voxel::{
    BlockId, BlockRegistry, BlockRegistryBuilder, BlockState, Face, Facing, RenderShape, SlabHalf,
};

use crate::shapes::{StairShape, WallSide, WallSides};

/// A kind of demo block the rules tell apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Stairs (`facing`, `half`, `shape`).
    Stairs,
    /// A fence (a `true | false` property per side).
    Fence,
    /// A wall (a `none | low | tall` property per side, `up`).
    Wall,
    /// A fence gate (`facing`, `open`, `in_wall`).
    Gate,
    /// What a wall raises its post under (Minecraft's `wall_post_override` tag: the standing
    /// torch).
    WallPostOverride,
    /// A torch on a wall (`facing`, away from the wall).
    WallTorch,
}

impl Kind {
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// Which blocks of a registry are of each [`Kind`] (by [`BlockId`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kinds {
    by_block: Vec<u8>,
}

impl Kinds {
    /// The demo's kinds, finding the demo blocks by name with `block` (missing ones are left
    /// out).
    pub fn demo(block: impl Fn(&str) -> Option<BlockId>) -> Self {
        let mut kinds = Kinds::default();
        let named = [
            ("boxel:stone_stairs", Kind::Stairs),
            ("boxel:oak_stairs", Kind::Stairs),
            ("boxel:oak_fence", Kind::Fence),
            ("boxel:torch", Kind::WallPostOverride),
            ("boxel:wall_torch", Kind::WallTorch),
            ("boxel:cobblestone_wall", Kind::Wall),
            ("boxel:oak_fence_gate", Kind::Gate),
        ];
        for (name, kind) in named {
            if let Some(b) = block(name) {
                kinds.add(b, kind);
            }
        }
        kinds
    }

    /// The demo's kinds of the blocks of `registry`.
    pub fn of_registry(registry: &BlockRegistry) -> Self {
        Self::demo(|name| registry.block_by_name(name))
    }

    /// The demo's kinds of the blocks registered in `builder` so far.
    pub fn of_builder(builder: &BlockRegistryBuilder) -> Self {
        Self::demo(|name| builder.block_by_name(name))
    }

    /// Makes `block` of `kind` too.
    pub fn add(&mut self, block: BlockId, kind: Kind) {
        let i = usize::from(block.0);
        if self.by_block.len() <= i {
            self.by_block.resize(i + 1, 0);
        }
        self.by_block[i] |= kind.bit();
    }

    /// True if `block` is of `kind`.
    pub fn block_is(&self, block: BlockId, kind: Kind) -> bool {
        self.by_block
            .get(usize::from(block.0))
            .is_some_and(|&bits| bits & kind.bit() != 0)
    }

    /// True if the block of `state` is of `kind`.
    pub fn is(&self, registry: &BlockRegistry, state: BlockState, kind: Kind) -> bool {
        registry
            .block(state)
            .is_some_and(|b| self.block_is(b, kind))
    }
}

/// The `facing` of a state (north if it has none).
fn facing_of(registry: &BlockRegistry, state: BlockState) -> Facing {
    registry
        .property(state, "facing")
        .and_then(Facing::from_name)
        .unwrap_or_default()
}

/// True if property `property` of `state` is `true`.
fn flag(registry: &BlockRegistry, state: BlockState, property: &str) -> bool {
    registry.property(state, property) == Some("true")
}

/// The `facing` and `half` of stairs (`None` for anything else).
pub fn stairs_of(
    kinds: &Kinds,
    registry: &BlockRegistry,
    state: BlockState,
) -> Option<(Facing, SlabHalf)> {
    if !kinds.is(registry, state, Kind::Stairs) {
        return None;
    }
    let half = registry
        .property(state, "half")
        .and_then(half_from_name)
        .unwrap_or_default();
    Some((facing_of(registry, state), half))
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
    kinds: &Kinds,
    registry: &BlockRegistry,
    facing: Facing,
    half: SlabHalf,
    neighbors: &[BlockState; 6],
) -> StairShape {
    let at = |f: Facing| neighbors[f.face().index()];
    // The stairs toward `side` do not continue this run (another facing or half, or none).
    let can_take = |side: Facing| stairs_of(kinds, registry, at(side)) != Some((facing, half));
    if let Some((front, front_half)) = stairs_of(kinds, registry, at(facing))
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
    if let Some((back, back_half)) = stairs_of(kinds, registry, at(facing.opposite()))
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
    kinds: &Kinds,
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    let Some((facing, half)) = stairs_of(kinds, registry, state) else {
        return state;
    };
    let shape = stair_shape(kinds, registry, facing, half, neighbors);
    registry.with(state, "shape", shape.name()).unwrap_or(state)
}

/// True if a fence connects toward `toward` to `neighbor`, Minecraft's rule simplified: another
/// fence, a fence gate that runs across `toward` ([`gate_connects`]), or a block whose face
/// toward the fence is sturdy, except cutout blocks (leaves). Not a wall: its sides are not
/// sturdy, as in Minecraft.
pub fn fence_connects(
    kinds: &Kinds,
    registry: &BlockRegistry,
    neighbor: BlockState,
    toward: Facing,
) -> bool {
    if kinds.is(registry, neighbor, Kind::Fence) {
        true
    } else if kinds.is(registry, neighbor, Kind::Gate) {
        gate_connects(facing_of(registry, neighbor), toward)
    } else {
        sturdy_toward(registry, neighbor, toward)
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
pub fn wall_connects(
    kinds: &Kinds,
    registry: &BlockRegistry,
    neighbor: BlockState,
    toward: Facing,
) -> bool {
    if kinds.is(registry, neighbor, Kind::Wall) {
        true
    } else if kinds.is(registry, neighbor, Kind::Gate) {
        gate_connects(facing_of(registry, neighbor), toward)
    } else {
        sturdy_toward(registry, neighbor, toward)
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
/// cut at its bottom face: a block, a wall or a fence above), else low. The collision on
/// purpose, not the support shape the other rules read (ADR 0037): Minecraft's `WallBlock` cuts
/// the collision shape of the block above. The post stands:
/// - under a wall with a post;
/// - else at an end, a corner, a T or alone: when north and south differ in being connected,
///   east and west differ, or nothing connects;
/// - else (a straight run or a cross) not when an opposite pair is tall on both sides, and
///   otherwise when a standing torch is above (Minecraft's `wall_post_override` tag) or the
///   bottom of the block above covers the middle of the cell.
pub fn wall_shape_of(
    kinds: &Kinds,
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
    let post = if kinds.is(registry, above, Kind::Wall)
        && flag(registry, above, "up")
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
            let torch = kinds.is(registry, above, Kind::WallPostOverride)
                && registry.shape(above) != RenderShape::None;
            torch || covers(&cover, wall_test(None))
        }
    };
    (post, sides)
}

/// A wall takes its sides and post from its neighbors ([`wall_connects`], [`wall_shape_of`]);
/// other states (`waterlogged`) are kept.
pub fn wall_rule(
    kinds: &Kinds,
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    if !kinds.is(registry, state, Kind::Wall) {
        return state;
    }
    // `Facing::ALL` is in `Facing as usize` order.
    let connected =
        Facing::ALL.map(|f| wall_connects(kinds, registry, neighbors[f.face().index()], f));
    let (post, sides) = wall_shape_of(kinds, registry, connected, neighbors[Face::PosY.index()]);
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

/// True for a wall (any drawn state of a block of kind [`Kind::Wall`]).
pub fn is_wall(kinds: &Kinds, registry: &BlockRegistry, state: BlockState) -> bool {
    kinds.is(registry, state, Kind::Wall) && registry.shape(state) != RenderShape::None
}

/// A fence gate is `in_wall` when a wall is at one of its ends, the two sides across its
/// facing (Minecraft's `FenceGateBlock::updateShape`); other states are kept.
pub fn fence_gate_rule(
    kinds: &Kinds,
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    if !kinds.is(registry, state, Kind::Gate) {
        return state;
    }
    let facing = facing_of(registry, state);
    let wall = |f: Facing| is_wall(kinds, registry, neighbors[f.face().index()]);
    let in_wall = wall(facing.clockwise()) || wall(facing.counterclockwise());
    registry
        .with(state, "in_wall", if in_wall { "true" } else { "false" })
        .unwrap_or(state)
}

/// Using a fence gate (Minecraft's `FenceGateBlock::useWithoutItem`): an open gate closes; a
/// closed one opens away from the user, who looks toward `look`, turning it around when it
/// faces the user.
pub fn fence_gate_use(
    kinds: &Kinds,
    registry: &BlockRegistry,
    state: BlockState,
    look: Facing,
) -> Option<BlockState> {
    if !kinds.is(registry, state, Kind::Gate) {
        return None;
    }
    let facing = facing_of(registry, state);
    if flag(registry, state, "open") {
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
    kinds: &Kinds,
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    if !kinds.is(registry, state, Kind::Fence) {
        return state;
    }
    let mut out = state;
    for f in Facing::ALL {
        let connected = fence_connects(kinds, registry, neighbors[f.face().index()], f);
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
    kinds: &Kinds,
    registry: &BlockRegistry,
    state: BlockState,
    neighbors: &[BlockState; 6],
) -> BlockState {
    if !kinds.is(registry, state, Kind::WallTorch) {
        return state;
    }
    let facing = facing_of(registry, state);
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
