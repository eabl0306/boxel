//! The engine side of the rules of blocks (ADR 0020, ADR 0027): a block may register a
//! [`NeighborRule`] (the state it takes given its 6 face neighbors), a [`UseRule`] (the state
//! it takes when a player uses it, or the cells a use changes: both halves of a bed, ADR 0048)
//! and a [`PlaceRule`] (the cells placing it fills, from the click and the look) in its
//! registry ([`BlockRegistryBuilder::set_neighbor_rule`],
//! [`BlockRegistryBuilder::set_use_rule`], [`BlockRegistryBuilder::set_place_rule`]). `boxel_world` applies the neighbor rules to the
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
//! [`BlockRegistryBuilder::set_place_rule`]: crate::BlockRegistryBuilder::set_place_rule
use crate::shapes::Facing;
use crate::{BlockCollision, BlockRegistry, BlockState, Face};
use glam::{DVec3, IVec3};
use std::sync::Arc;

/// The signature of a neighbor rule: the registry, the state and its 6 face neighbors (indexed
/// by [`Face::index`]).
type NeighborFn = dyn Fn(&BlockRegistry, BlockState, &[BlockState; 6]) -> BlockState + Send + Sync;

/// The signature of a use rule: the registry, the state and where the user looks.
type UseFn = dyn Fn(&BlockRegistry, BlockState, Facing) -> Option<BlockState> + Send + Sync;

/// The signature of a use rule of several cells: the registry and the use.
type UseCellsFn = dyn Fn(&BlockRegistry, &Use<'_>) -> Option<Vec<CellChange>> + Send + Sync;

/// The signature of a placement rule: the registry and the placement.
type PlaceFn = dyn Fn(&BlockRegistry, &Placing<'_>) -> Option<Vec<CellChange>> + Send + Sync;

/// A cell a rule changes: where, from the cell the rule is about, and the state it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellChange {
    /// From the used or placed cell, in blocks.
    pub offset: IVec3,
    /// The state the cell takes.
    pub state: BlockState,
}

impl CellChange {
    /// `state` at `offset`.
    pub const fn new(offset: IVec3, state: BlockState) -> Self {
        Self { offset, state }
    }

    /// `state` in the cell the rule is about.
    pub const fn here(state: BlockState) -> Self {
        Self::new(IVec3::ZERO, state)
    }
}

/// What a rule may read of the world: the state of a cell by its offset from the cell the rule
/// is about (`None`: not loaded).
pub type CellReader<'a> = &'a dyn Fn(IVec3) -> Option<BlockState>;

/// A use of a block, for a [`UseRule::cells`] rule.
pub struct Use<'a> {
    /// The used block's state.
    pub state: BlockState,
    /// Where the user looks, horizontally.
    pub look: Facing,
    /// Reads the cells around the used one.
    pub read: CellReader<'a>,
}

impl Use<'_> {
    /// The state at `offset` from the used cell (`None`: not loaded).
    pub fn at(&self, offset: IVec3) -> Option<BlockState> {
        (self.read)(offset)
    }
}

/// A placement of a block, for a [`PlaceRule`]: what the player holds, what was clicked and
/// how, and where the player looks. The game picks the cell (the one in front of the clicked
/// face, or the clicked one when it is replaced); the rule picks the states.
pub struct Placing<'a> {
    /// The state the held item places (its block's default, or what the game's placement
    /// already chose).
    pub held: BlockState,
    /// The state of the clicked block.
    pub clicked: BlockState,
    /// The clicked face.
    pub face: Face,
    /// The clicked cell from the placed one (the face's opposite normal, or zero when the
    /// clicked cell is the one replaced).
    pub clicked_offset: IVec3,
    /// Where the click hit inside the clicked cell, `0..=1` on each axis.
    pub hit: DVec3,
    /// The player's view direction (unit length).
    pub look: DVec3,
    /// Reads the cells around the placed one.
    pub read: CellReader<'a>,
}

impl Placing<'_> {
    /// The state at `offset` from the placed cell (`None`: not loaded).
    pub fn at(&self, offset: IVec3) -> Option<BlockState> {
        (self.read)(offset)
    }

    /// The horizontal direction the player looks to most.
    pub fn look_facing(&self) -> Facing {
        Facing::of_look(self.look.x, self.look.z)
    }
}

/// How a block places: from a [`Placing`], the cells it fills (the placed cell at offset zero
/// and any other, the head of a bed), or `None` when it cannot go there. Registered per block
/// like [`UseRule`] ([`BlockRegistryBuilder::set_place_rule`]); the world keeps them
/// (`boxel_world::BlockUses::place`) and the game asks. A block without one places as the game
/// decides.
///
/// [`BlockRegistryBuilder::set_place_rule`]: crate::BlockRegistryBuilder::set_place_rule
#[derive(Clone)]
pub struct PlaceRule(Arc<PlaceFn>);

impl PlaceRule {
    /// The rule `rule`.
    pub fn new(
        rule: impl Fn(&BlockRegistry, &Placing<'_>) -> Option<Vec<CellChange>> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(rule))
    }

    /// The cells `placing` fills, if the block can go there.
    #[inline]
    pub fn apply(
        &self,
        registry: &BlockRegistry,
        placing: &Placing<'_>,
    ) -> Option<Vec<CellChange>> {
        (self.0)(registry, placing)
    }
}

impl std::fmt::Debug for PlaceRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PlaceRule(..)")
    }
}

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
///
/// A rule made with [`UseRule::cells`] reads the cells around and changes several of them in
/// one batch (both halves of a bed or a door, ADR 0048): a block's own neighbor rule runs on
/// every edited cell, so a two-cell block whose halves follow each other must change both.
#[derive(Clone)]
pub struct UseRule(UseKind);

#[derive(Clone)]
enum UseKind {
    One(Arc<UseFn>),
    Cells(Arc<UseCellsFn>),
}

impl UseRule {
    /// The rule `rule`.
    pub fn new(
        rule: impl Fn(&BlockRegistry, BlockState, Facing) -> Option<BlockState> + Send + Sync + 'static,
    ) -> Self {
        Self(UseKind::One(Arc::new(rule)))
    }

    /// A rule of several cells: from the [`Use`], the cells it changes (the used one at
    /// offset zero), or `None` when the block is not used that way.
    pub fn cells(
        rule: impl Fn(&BlockRegistry, &Use<'_>) -> Option<Vec<CellChange>> + Send + Sync + 'static,
    ) -> Self {
        Self(UseKind::Cells(Arc::new(rule)))
    }

    /// The state `state` takes when used by someone looking toward `look`, if it is used. A
    /// rule of several cells is asked with no cell around loaded, and answers with the change
    /// it makes to the used cell.
    #[inline]
    pub fn apply(
        &self,
        registry: &BlockRegistry,
        state: BlockState,
        look: Facing,
    ) -> Option<BlockState> {
        match &self.0 {
            UseKind::One(rule) => rule(registry, state, look),
            UseKind::Cells(_) => {
                let read = |_: IVec3| None;
                let used = Use {
                    state,
                    look,
                    read: &read,
                };
                self.apply_cells(registry, &used)?
                    .into_iter()
                    .find(|c| c.offset == IVec3::ZERO)
                    .map(|c| c.state)
            }
        }
    }

    /// The cells `used` changes, if the block is used that way: a rule of one cell changes the
    /// used one.
    pub fn apply_cells(&self, registry: &BlockRegistry, used: &Use<'_>) -> Option<Vec<CellChange>> {
        match &self.0 {
            UseKind::One(rule) => {
                rule(registry, used.state, used.look).map(|s| vec![CellChange::here(s)])
            }
            UseKind::Cells(rule) => rule(registry, used),
        }
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

    /// A rule of several cells reads around and answers both ways; a rule of one cell answers
    /// the cells question with its cell; a placement rule sees the click.
    #[test]
    fn cell_rules_read_around_and_answer_both_ways() {
        let r = testing::registry();
        let (a, b) = (BlockState(1), BlockState(2));
        let pair = UseRule::cells(|_, used| {
            let up = IVec3::Y;
            let mut cells = vec![CellChange::here(BlockState(2))];
            if used.at(up) == Some(used.state) {
                cells.push(CellChange::new(up, BlockState(2)));
            }
            Some(cells)
        });
        let world = |o: IVec3| (o == IVec3::Y).then_some(a);
        let used = Use {
            state: a,
            look: Facing::North,
            read: &world,
        };
        let cells = pair.apply_cells(&r, &used).unwrap();
        assert_eq!(cells, [CellChange::here(b), CellChange::new(IVec3::Y, b)]);
        assert_eq!(pair.apply(&r, a, Facing::North), Some(b), "the used cell");
        let one = UseRule::new(|_, s, _| Some(BlockState(s.0 + 1)));
        assert_eq!(one.apply_cells(&r, &used), Some(vec![CellChange::here(b)]));
        let place = PlaceRule::new(|_, p| {
            let side = p.face == Face::PosX && p.hit.y > 0.5;
            side.then(|| vec![CellChange::here(p.held)])
        });
        let mut placing = Placing {
            held: a,
            clicked: b,
            face: Face::PosX,
            clicked_offset: IVec3::NEG_X,
            hit: DVec3::new(1.0, 0.75, 0.5),
            look: DVec3::new(-1.0, 0.0, 0.2),
            read: &world,
        };
        assert_eq!(place.apply(&r, &placing), Some(vec![CellChange::here(a)]));
        assert_eq!(placing.look_facing(), Facing::West);
        placing.hit.y = 0.25;
        assert_eq!(place.apply(&r, &placing), None);
    }

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
