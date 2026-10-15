//! The engine side of the rules of blocks (ADR 0020, ADR 0027): a block may register a
//! [`NeighborRule`] (the state it takes given its 6 face neighbors) and a [`UseRule`] (the state
//! it takes when a player uses it) in its registry ([`BlockRegistryBuilder::set_neighbor_rule`],
//! [`BlockRegistryBuilder::set_use_rule`]). `boxel_world` applies the neighbor rules to the
//! blocks around every edit (`NeighborRules`), and again around every block a rule changed, and
//! asks the use rules on a player's click (`BlockUses`). The rules themselves are content: the
//! game writes them, as closures over what they need, and they can be tested without a world.
//!
//! Also the helpers rules are written with: which sixteenths of a face a shape covers
//! ([`collision_face_cover`], of a collision or a support shape) and whether a face holds a
//! block ([`is_sturdy`], [`supports_center`], on the support shape, ADR 0037); and the fluid of
//! the blocks that hold one (the registry's waterlog fluid, water) when a player places or
//! breaks one (ADR 0025): [`placed_into`] and [`broken`].
//!
//! [`BlockRegistryBuilder::set_neighbor_rule`]: crate::BlockRegistryBuilder::set_neighbor_rule
//! [`BlockRegistryBuilder::set_use_rule`]: crate::BlockRegistryBuilder::set_use_rule
use crate::shapes::Facing;
use crate::{BlockCollision, BlockRegistry, BlockState, Face};
use std::sync::Arc;

/// The signature of a neighbor rule: the registry, the state and its 6 face neighbors (indexed
/// by [`Face::index`]).
type NeighborFn = dyn Fn(&BlockRegistry, BlockState, &[BlockState; 6]) -> BlockState + Send + Sync;

/// The signature of a use rule: the registry, the state and where the user looks.
type UseFn = dyn Fn(&BlockRegistry, BlockState, Facing) -> Option<BlockState> + Send + Sync;

/// A neighbor rule: the state `state` should have given its 6 face neighbors (indexed by
/// [`Face::index`]). Returning `state` changes nothing; returning air removes the block. A
/// closure, so a rule can hold what it needs (the blocks it treats alike, for instance); cheap
/// to clone (shared).
#[derive(Clone)]
pub struct NeighborRule(Arc<NeighborFn>);

impl NeighborRule {
    /// The rule `rule`.
    pub fn new(
        rule: impl Fn(&BlockRegistry, BlockState, &[BlockState; 6]) -> BlockState
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self(Arc::new(rule))
    }

    /// The state `state` takes among `neighbors`.
    #[inline]
    pub fn apply(
        &self,
        registry: &BlockRegistry,
        state: BlockState,
        neighbors: &[BlockState; 6],
    ) -> BlockState {
        (self.0)(registry, state, neighbors)
    }
}

impl std::fmt::Debug for NeighborRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NeighborRule(..)")
    }
}

/// What using a block (a player's right click on it) does: the state it takes when used by
/// someone looking toward `look` (horizontally), or `None` if the block is not used that way
/// (then the click places a block, as before). Registered per block like [`NeighborRule`]; the
/// world keeps them (`boxel_world::BlockUses`) and the game asks it.
#[derive(Clone)]
pub struct UseRule(Arc<UseFn>);

impl UseRule {
    /// The rule `rule`.
    pub fn new(
        rule: impl Fn(&BlockRegistry, BlockState, Facing) -> Option<BlockState> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(rule))
    }

    /// The state `state` takes when used by someone looking toward `look`, if it is used.
    #[inline]
    pub fn apply(
        &self,
        registry: &BlockRegistry,
        state: BlockState,
        look: Facing,
    ) -> Option<BlockState> {
        (self.0)(registry, state, look)
    }
}

impl std::fmt::Debug for UseRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UseRule(..)")
    }
}

/// Which sixteenths of `face` the boxes of the shape `collision` cover, flush with the cell
/// border: bit `u` of row `v` along the face's axes ([`Face::axes`]). Any shape: a collision
/// ([`BlockRegistry::collision`]) or a support shape ([`BlockRegistry::support`]). A box that
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

/// Bits `lo..hi` of a row of sixteenths (`lo <= hi <= 16`).
pub fn span_bits(lo: u8, hi: u8) -> u16 {
    ((1u32 << hi) - (1u32 << lo)) as u16
}

/// True if `face` of `state` is a sturdy face (Minecraft's `isFaceSturdy`): its support shape
/// ([`BlockRegistry::support`], its collision unless set) covers the whole face. A full cube,
/// the back of stairs, the bottom of a bottom slab.
pub fn is_sturdy(registry: &BlockRegistry, state: BlockState, face: Face) -> bool {
    collision_face_cover(registry.support(state), face) == [u16::MAX; 16]
}

/// True if `face` of `state` can hold something at its center (Minecraft's `CENTER` support):
/// its support shape ([`BlockRegistry::support`], its collision unless set) covers the middle
/// 2x2 sixteenths of the face (a fence post's top does).
pub fn supports_center(registry: &BlockRegistry, state: BlockState, face: Face) -> bool {
    let rows = collision_face_cover(registry.support(state), face);
    let mid = span_bits(7, 9);
    rows[7] & mid == mid && rows[8] & mid == mid
}

/// The source (`level` 0) of the fluid the waterlogged states of `registry` hold
/// (`BlockRegistry::waterlog_fluid`), if it has one.
pub(crate) fn waterlog_source(registry: &BlockRegistry) -> Option<BlockState> {
    let id = registry.waterlog_fluid()?;
    registry.fluid(id).map(|fluid| fluid.first)
}

/// The state `placed` takes in a cell that holds `existing` (Minecraft's `getStateForPlacement`
/// of the blocks that hold water, ADR 0025): a block that can be waterlogged keeps a source of
/// the waterlog fluid it is placed into and is dry anywhere else (flowing water is replaced,
/// not kept).
/// Other blocks are `placed` as it is.
pub fn placed_into(
    registry: &BlockRegistry,
    placed: BlockState,
    existing: BlockState,
) -> BlockState {
    if !registry.can_be_waterlogged(placed) {
        return placed;
    }
    let wet = waterlog_source(registry) == Some(existing);
    registry.with_water(placed, wet).unwrap_or(placed)
}

/// What breaking `state` leaves in its cell (Minecraft keeps a block's fluid when it breaks,
/// ADR 0025): a source of the waterlog fluid for a waterlogged block, air for anything else.
pub fn broken(registry: &BlockRegistry, state: BlockState) -> BlockState {
    if registry.is_waterlogged(state) {
        waterlog_source(registry).unwrap_or(BlockState::AIR)
    } else {
        BlockState::AIR
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    /// No block of the `test:` set sets a support shape, so every state (and one out of range)
    /// holds other blocks with its collision: [`is_sturdy`] and [`supports_center`] read what
    /// they read before ADR 0037.
    #[test]
    fn support_defaults_to_the_collision() {
        let r = testing::registry();
        let states = (0..r.len() as u16)
            .map(BlockState)
            .chain([BlockState(u16::MAX)]);
        for s in states {
            assert_eq!(r.support(s), r.collision(s), "{s:?}");
            for face in Face::ALL {
                let cover = collision_face_cover(r.collision(s), face);
                let mid = span_bits(7, 9);
                let center = cover[7] & mid == mid && cover[8] & mid == mid;
                let full = cover == [u16::MAX; 16];
                assert_eq!(is_sturdy(&r, s, face), full, "{s:?} {face:?}");
                assert_eq!(supports_center(&r, s, face), center, "{s:?} {face:?}");
            }
        }
    }
}
