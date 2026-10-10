//! Flowing fluids, Minecraft Java's rules (`FlowingFluid`, `WaterFluid`, `LavaFluid` and
//! `LiquidBlock`) on the scheduled block ticks ([`crate::ticks`]), for the fluid types the
//! registry declares (`BlockRegistryBuilder::add_fluid`; written for water, ADR 0021, and lava,
//! ADR 0025, whose numbers the examples below give).
//!
//! - A fluid cell is a source (`level` 0, amount 8), flowing (`level` 1..=7, amount `8 - level`)
//!   or falling (`level` 8..=15, amount 8, fed from above).
//! - A tick of a flowing cell first recomputes it from its neighbors: falling if the same
//!   fluid is above; for water, a source if two horizontal neighbors are sources and the cell
//!   below is solid or a source (infinite water, `FluidRules::infinite`); else the largest
//!   horizontal amount minus the fluid's drop-off (1 for water, 2 for lava), or air when that is
//!   0 or less (so a fluid drains once its source is gone).
//! - Then it spreads: down into the cell below if it can (as falling fluid), and to the sides
//!   only when it cannot fall (or, falling, with 3 or more source neighbors). Sideways it takes
//!   the directions with the shortest way down within the fluid's slope distance (4 blocks for
//!   water, 2 for lava; all directions if none has one), each neighbor getting what its own
//!   neighbors give it. A falling cell spreads like amount 8. So water reaches 7 blocks and
//!   lava 3.
//! - A cell can hold a fluid if nothing in it stops movement (air, plants, torches, water and
//!   lava; a fence gate stops it open or closed, as in Minecraft, though an open one does not
//!   collide: `BlockRegistry::blocks_motion`). The fluid may flow into it if what it holds can
//!   be replaced (`FluidType::replaced_by`): an empty cell always, water only by lava falling
//!   into it (which turns it to stone, `FluidType::falls_into`), lava only by water where the
//!   lava is at least 4/9 high. A fluid never replaces its own cells.
//! - A waterlogged block (a slab, stairs or a fence whose `waterlogged` is `true`, ADR 0025)
//!   holds a source of the registry's waterlog fluid (water): it ticks and spreads like one and
//!   counts as one for its neighbors, but never hardens (only the fluid's own block does, as in
//!   Minecraft's `LiquidBlock`).
//!   Flowing water never fills a block (it stops movement), but a dry one where water would
//!   make a new source (two sources beside it) takes it and becomes waterlogged, as in
//!   Minecraft (`canPlaceLiquid`). Between two cells, a fluid passes only where their collision
//!   faces toward each other leave part of the face open (Minecraft's `canPassThroughWall`):
//!   water flows out of the sides of a waterlogged slab but not through the bottom of a bottom
//!   slab.
//! - A fluid next to another may harden at once (`FluidType::hardens`, Minecraft's
//!   `LiquidBlock::shouldSpreadLiquid`): a lava cell with water above it or beside it turns to
//!   obsidian if it is a source, to cobblestone otherwise. A fluid may harden beside several
//!   others; the first it meets, in declaration order, wins. This is the fluid's tick reaction
//!   ([`TickHandler::react`]), so it happens in the write that brings the two together.
//! - Every change schedules the cell and its neighbors that are fluid their fluid's delay later
//!   (water 5 game ticks, 4 updates a second; lava 30). Worldgen fluid schedules nothing: a
//!   calm lake stays as it is until an edit touches it.
//!
//! Left out of Minecraft's lava: the random slowdown of rising lava (`getSpreadDelay`), fire,
//! and the Nether's faster lava.
use crate::WorldContent;
use crate::ticks::{BlockTicks, BlockTicksPlugin, TickContext, TickHandler, offset};
use boxel_app::{App, Plugin};
use boxel_voxel::rules::collision_face_cover;
use boxel_voxel::{
    BlockCollision, BlockModel, BlockPos, BlockRegistry, BlockState, FLUID_LEVEL, Face, FluidId,
    FluidRules, FluidType,
};
use std::cell::Cell;

/// The horizontal directions in Minecraft's order (`Direction.Plane.HORIZONTAL`: north, east,
/// south, west), which decides the order of a tick's writes and so of the ticks they schedule.
const HORIZONTAL: [Face; 4] = [Face::NegZ, Face::PosX, Face::PosZ, Face::NegX];

/// Adds the fluids the registry declares (`BlockRegistry::fluids`): registers the ticks of
/// each fluid's block and, for the waterlog fluid, of every waterlogged state (with
/// [`BlockTicksPlugin`] if it is not there yet), and the reaction of each fluid that hardens on
/// its own block (a waterlogged block never hardens).
/// Needs [`WorldPlugin`](crate::WorldPlugin) first; a registry without fluids adds no tick.
pub struct FluidPlugin;

impl Plugin for FluidPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<BlockTicks>() {
            app.add_plugins(BlockTicksPlugin);
        }
        let registry = std::sync::Arc::clone(&app.world().resource::<WorldContent>().registry);
        let mut ticks = app.world_mut().resource_mut::<BlockTicks>();
        for (i, fluid) in registry.fluids().iter().enumerate() {
            let handler = TickHandler {
                delay: fluid.rules.delay,
                reach: fluid.rules.reach(),
                tick: fluid_tick,
                react: (!fluid.hardens.is_empty()).then_some(fluid_react as _),
            };
            ticks.register(&registry, fluid.block, handler);
            if registry.waterlog_fluid() == Some(FluidId(i as u8)) {
                // The fluid of the waterlogged blocks, without the reaction: only the fluid's
                // own block hardens, never a block that holds it.
                let held = TickHandler {
                    react: None,
                    ..handler
                };
                for state in (0..registry.len() as u16).map(BlockState) {
                    if registry.is_waterlogged(state) {
                        ticks.register_state(&registry, state, held);
                    }
                }
            }
        }
    }
}

/// One fluid cell: its amount (1..=8) and whether it is a source or falling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fluid {
    /// 8 for sources and falling fluid, `8 - level` for flowing fluid.
    pub amount: u8,
    /// `level` 0.
    pub source: bool,
    /// `level` 8 and above.
    pub falling: bool,
}

impl Fluid {
    /// A source.
    pub const SOURCE: Fluid = Fluid {
        amount: 8,
        source: true,
        falling: false,
    };
    /// Falling fluid.
    pub const FALLING: Fluid = Fluid {
        amount: 8,
        source: false,
        falling: true,
    };

    /// Flowing fluid of `amount` (1..=7).
    pub const fn flowing(amount: u8) -> Fluid {
        Fluid {
            amount,
            source: false,
            falling: false,
        }
    }

    /// The fluid of `state` if it is a state of a block with a `level` property and the fluid
    /// model (water, lava).
    pub fn of(registry: &BlockRegistry, state: BlockState) -> Option<Fluid> {
        if registry.model(state) != BlockModel::Fluid {
            return None;
        }
        let level = registry.values(state).index(FLUID_LEVEL)? as u8;
        Some(Fluid::from_level(level))
    }

    /// The fluid of a `level` value (0..=15).
    pub const fn from_level(level: u8) -> Fluid {
        match level {
            0 => Fluid::SOURCE,
            1..=7 => Fluid::flowing(8 - level),
            _ => Fluid::FALLING,
        }
    }

    /// Its `level` value: 0 for a source, 8 falling, `8 - amount` flowing.
    pub const fn level(self) -> u8 {
        if self.source {
            0
        } else if self.falling {
            8
        } else {
            8 - self.amount
        }
    }
}

/// The fluids of the registry, as one tick sees them.
#[derive(Clone, Copy, Debug)]
struct Fluids<'a> {
    registry: &'a BlockRegistry,
    /// Its fluid types (`BlockRegistry::fluids`).
    types: &'a [FluidType],
    /// The fluid the waterlogged states hold.
    waterlog: Option<FluidId>,
}

impl<'a> Fluids<'a> {
    fn new(registry: &'a BlockRegistry) -> Self {
        Self {
            registry,
            types: registry.fluids(),
            waterlog: registry.waterlog_fluid(),
        }
    }

    /// The type of fluid `id`.
    fn get(&self, id: FluidId) -> &'a FluidType {
        &self.types[usize::from(id.0)]
    }

    /// The fluid `state` holds and its id (a waterlogged block holds a source of the waterlog
    /// fluid).
    fn content(&self, state: BlockState) -> Option<(FluidId, Fluid)> {
        for (i, fluid) in self.types.iter().enumerate() {
            if let Some(level) = state.0.checked_sub(fluid.first.0).filter(|&l| l < 16) {
                return Some((FluidId(i as u8), Fluid::from_level(level as u8)));
            }
        }
        if let Some(waterlog) = self.waterlog
            && self.registry.is_waterlogged(state)
        {
            return Some((waterlog, Fluid::SOURCE));
        }
        None
    }

    /// True if `state` holds fluid `id`.
    fn holds(&self, state: Option<BlockState>, id: FluidId) -> bool {
        state
            .and_then(|s| self.content(s))
            .is_some_and(|(found, _)| found == id)
    }

    /// Minecraft's `canPassThroughWall`: a fluid passes from `a` to its neighbor `b` across
    /// `face` of `a` unless their collision faces there cover the whole face together (a full
    /// block always does). Cells without collision (air, fluids, plants) never stop it.
    fn wall_open(&self, a: Option<BlockState>, b: Option<BlockState>, face: Face) -> bool {
        let (Some(a), Some(b)) = (a, b) else {
            return true;
        };
        let (ca, cb) = (self.registry.collision(a), self.registry.collision(b));
        if ca.is_none() && cb.is_none() {
            return true;
        }
        if *ca == BlockCollision::Full || *cb == BlockCollision::Full {
            return false;
        }
        let (fa, fb) = (
            collision_face_cover(ca, face),
            collision_face_cover(cb, face.opposite()),
        );
        fa.iter().zip(&fb).any(|(x, y)| x | y != u16::MAX)
    }

    /// The state of fluid `id` holding `fluid`.
    fn state(&self, id: FluidId, fluid: Fluid) -> BlockState {
        BlockState(self.get(id).first.0 + u16::from(fluid.level()))
    }
}

/// Where the fluid rules read blocks: the world of a tick ([`TickContext`]) or a memo of it
/// ([`Memo`]).
trait Blocks {
    /// The block at `pos` (`None` where its chunk is not loaded).
    fn block(&self, pos: BlockPos) -> Option<BlockState>;
}

impl Blocks for TickContext<'_> {
    fn block(&self, pos: BlockPos) -> Option<BlockState> {
        self.get(pos)
    }
}

/// How far sideways from the spreading cell a [`Memo`] keeps blocks: as far as the search for a
/// way down of a fluid with slope distance 4 (water) reads, a neighbor and 4 more. A fluid that
/// looks further reads the blocks past it from the world every time, which gives the same
/// answers.
const MEMO_REACH: i64 = 5;
/// Blocks along a side of a [`Memo`] layer.
const MEMO_SIDE: usize = 2 * MEMO_REACH as usize + 1;
/// Blocks in a [`Memo`] layer.
const MEMO_AREA: usize = MEMO_SIDE * MEMO_SIDE;
/// A block of a [`Memo`] not read yet.
const UNREAD: u32 = u32::MAX;
/// A block of a [`Memo`] whose chunk is not loaded.
const UNLOADED: u32 = u32::MAX - 1;
/// The bits of a cell of a [`Memo`]: the answer of [`Memo::passes`], the answer of
/// [`Memo::hole`] and, for each one, whether it is known.
const PASSES: u8 = 1;
const PASSES_KNOWN: u8 = 2;
const HOLE: u8 = 4;
const HOLE_KNOWN: u8 = 8;

/// What one spread ([`Flow::spread_targets`]) reads and works out, each only once: the blocks of
/// the spreading cell's layer and of the layers above and below it, up to [`MEMO_REACH`]
/// blocks sideways (blocks further away are read every time), and for the cells of its layer
/// whether the fluid passes into them and falls there. The search for the way down visits the
/// same cells over and over, along every path it tries; Minecraft's `getSpread` keeps the same
/// cache. Exact because a spread only reads: nothing changes the world while the memo lives.
/// One memo serves one fluid.
struct Memo<'b, B> {
    blocks: &'b B,
    center: BlockPos,
    /// The blocks: `UNREAD`, `UNLOADED` or a state, by layer (below, the spreading one, above),
    /// then x, then z.
    read: [Cell<u32>; 3 * MEMO_AREA],
    /// The `PASSES` and `HOLE` bits of the cells of the spreading layer, by x, then z.
    cells: [Cell<u8>; MEMO_AREA],
}

impl<'b, B: Blocks> Memo<'b, B> {
    fn new(blocks: &'b B, center: BlockPos) -> Self {
        Self {
            blocks,
            center,
            read: [const { Cell::new(UNREAD) }; 3 * MEMO_AREA],
            cells: [const { Cell::new(0) }; MEMO_AREA],
        }
    }

    /// The layer of `pos` (0 below the spreading cell, 1 its own, 2 above) and its index in
    /// the layer, if the memo keeps it.
    fn slot(&self, pos: BlockPos) -> Option<(usize, usize)> {
        let index =
            |d: i64, reach: i64| (-reach..=reach).contains(&d).then(|| (d + reach) as usize);
        let x = index(pos.x.wrapping_sub(self.center.x), MEMO_REACH)?;
        let z = index(pos.z.wrapping_sub(self.center.z), MEMO_REACH)?;
        let y = index(pos.y.wrapping_sub(self.center.y), 1)?;
        Some((y, x * MEMO_SIDE + z))
    }

    /// Whether `flow` passes into the cell at `pos` from a neighbor without collision, and the
    /// block there: [`Flow::can_pass_through`] of flowing fluid, but for the wall (no wall
    /// stops a fluid between two cells without collision).
    fn passes(&self, flow: &Flow<'_>, pos: BlockPos) -> (Option<BlockState>, bool) {
        let state = self.block(pos);
        let passes = || !flow.is_source(state) && flow.can_hold(state);
        (state, self.known(pos, PASSES, PASSES_KNOWN, passes))
    }

    /// Whether `flow` falls from the cell at `pos` into the one below ([`Flow::is_hole`]).
    fn hole(&self, flow: &Flow<'_>, pos: BlockPos) -> bool {
        let hole = || flow.is_hole(self.block(pos), self.block(offset(pos, Face::NegY)));
        self.known(pos, HOLE, HOLE_KNOWN, hole)
    }

    /// The `bit` of the cell at `pos`, worked out by `work` the first time (and every time
    /// outside the spreading layer).
    fn known(&self, pos: BlockPos, bit: u8, known: u8, work: impl FnOnce() -> bool) -> bool {
        let Some(cell) = self
            .slot(pos)
            .filter(|&(layer, _)| layer == 1)
            .and_then(|(_, i)| self.cells.get(i))
        else {
            return work();
        };
        let bits = cell.get();
        if bits & known != 0 {
            return bits & bit != 0;
        }
        let answer = work();
        cell.set(bits | known | if answer { bit } else { 0 });
        answer
    }
}

impl<B: Blocks> Blocks for Memo<'_, B> {
    fn block(&self, pos: BlockPos) -> Option<BlockState> {
        let Some(slot) = self
            .slot(pos)
            .and_then(|(layer, i)| self.read.get(layer * MEMO_AREA + i))
        else {
            return self.blocks.block(pos);
        };
        match slot.get() {
            UNREAD => {
                let state = self.blocks.block(pos);
                slot.set(state.map_or(UNLOADED, |s| u32::from(s.0)));
                state
            }
            UNLOADED => None,
            state => u16::try_from(state).ok().map(BlockState),
        }
    }
}

/// The neighbors a cell spreads to and what each one gets, in the order Minecraft writes them
/// (it keeps them in an `EnumMap`: north, south, west, east).
type Targets = [Option<(Face, Option<Fluid>)>; 4];

/// The slot of the neighbor across `face` (horizontal) in [`Targets`].
fn target_slot(face: Face) -> usize {
    match face {
        Face::NegZ => 0,
        Face::PosZ => 1,
        Face::NegX => 2,
        _ => 3,
    }
}

/// One fluid's view of the world in its tick.
struct Flow<'a> {
    fluids: Fluids<'a>,
    /// The fluid.
    id: FluidId,
    rules: FluidRules,
}

impl Flow<'_> {
    fn registry(&self) -> &BlockRegistry {
        self.fluids.registry
    }

    /// This fluid in `state`.
    fn fluid(&self, state: Option<BlockState>) -> Option<Fluid> {
        let (id, fluid) = self.fluids.content(state?)?;
        (id == self.id).then_some(fluid)
    }

    fn state(&self, fluid: Fluid) -> BlockState {
        self.fluids.state(self.id, fluid)
    }

    /// True for a source of this fluid.
    fn is_source(&self, state: Option<BlockState>) -> bool {
        self.fluid(state).is_some_and(|f| f.source)
    }

    /// Minecraft's `canHoldFluid`: nothing in the cell stops movement (air, plants, torches,
    /// water and lava; not a fence gate, even open: [`BlockRegistry::blocks_motion`]), and it is
    /// not a fluid this module does not know.
    fn can_hold(&self, state: Option<BlockState>) -> bool {
        let Some(state) = state else {
            return false;
        };
        let registry = self.registry();
        !registry.blocks_motion(state)
            && (registry.model(state) != BlockModel::Fluid || self.fluids.content(state).is_some())
    }

    /// Minecraft's `isSolid`: something that stops movement (it collides, or a fence gate, open
    /// or closed); not loaded counts as solid.
    fn is_solid(&self, state: Option<BlockState>) -> bool {
        state.is_none_or(|s| self.registry().blocks_motion(s))
    }

    /// Minecraft's `FluidState::canBeReplacedWith`: what `state` holds gives way to this fluid
    /// flowing into it (`down`: falling into it). An empty cell does; a fluid only as its
    /// `replaced_by` says (water only to lava falling into it; lava only to water, where it is at
    /// least 4/9 high, amount 4).
    fn replaceable(&self, state: Option<BlockState>, down: bool) -> bool {
        let Some((target, held)) = state.and_then(|s| self.fluids.content(s)) else {
            return true;
        };
        self.fluids
            .get(target)
            .replaced_by
            .iter()
            .any(|&(by, when)| {
                by == self.id && (down || !when.only_down) && held.amount >= when.min_amount
            })
    }

    /// The wall between `from` and `to` across `face` of `from` ([`Fluids::wall_open`]).
    fn wall_open(&self, from: Option<BlockState>, to: Option<BlockState>, face: Face) -> bool {
        self.fluids.wall_open(from, to, face)
    }

    /// True if `state` is a dry block that would take a source of this fluid, the waterlog
    /// fluid (Minecraft's `SimpleWaterloggedBlock::canPlaceLiquid`).
    fn takes_source(&self, state: Option<BlockState>) -> bool {
        self.fluids.waterlog == Some(self.id)
            && state.is_some_and(|s| {
                let registry = self.registry();
                registry.can_be_waterlogged(s) && !registry.is_waterlogged(s)
            })
    }

    /// Minecraft's `canHoldFluid` for the fluid `new`: [`Self::can_hold`], or a dry block that
    /// takes `new` because it is a water source ([`Self::takes_source`]). Only for such a block
    /// does `new` matter.
    fn can_hold_new(&self, state: Option<BlockState>, new: Option<Fluid>) -> bool {
        self.can_hold(state) || new.is_some_and(|f| f.source) && self.takes_source(state)
    }

    /// Minecraft's `canSpreadTo`: this fluid, as `new`, may flow from `from` into its neighbor
    /// `to` across `face` (down when `face` is `NegY`).
    fn can_spread_to(
        &self,
        from: Option<BlockState>,
        to: Option<BlockState>,
        face: Face,
        new: Option<Fluid>,
    ) -> bool {
        self.replaceable(to, face == Face::NegY)
            && self.can_hold_new(to, new)
            && self.wall_open(from, to, face)
    }

    /// Minecraft's `canPassThrough`: from `from` across `face` into `to`, not a source of this
    /// fluid and able to hold it as `new` (flowing fluid when `None`).
    fn can_pass_through(
        &self,
        from: Option<BlockState>,
        to: Option<BlockState>,
        face: Face,
        new: Option<Fluid>,
    ) -> bool {
        !self.is_source(to) && self.can_hold_new(to, new) && self.wall_open(from, to, face)
    }

    /// Minecraft's `isWaterHole`: the cell below `from` holds this fluid or could, through the
    /// wall between them.
    fn is_hole(&self, from: Option<BlockState>, below: Option<BlockState>) -> bool {
        (self.fluid(below).is_some() || self.can_hold(below))
            && self.wall_open(from, below, Face::NegY)
    }

    /// What `pos`, holding `here`, would hold given its neighbors (Minecraft's
    /// `getNewLiquid`); `None` is empty. A neighbor counts through an open wall only.
    fn new_liquid(
        &self,
        b: &impl Blocks,
        pos: BlockPos,
        here: Option<BlockState>,
    ) -> Option<Fluid> {
        let (mut amount, mut sources) = (0, 0);
        for face in HORIZONTAL {
            let neighbor = b.block(offset(pos, face));
            if let Some(f) = self.fluid(neighbor)
                && self.wall_open(here, neighbor, face)
            {
                sources += usize::from(f.source);
                amount = amount.max(f.amount);
            }
        }
        if self.rules.infinite && sources >= 2 {
            let below = b.block(offset(pos, Face::NegY));
            if self.is_solid(below) || self.is_source(below) {
                return Some(Fluid::SOURCE);
            }
        }
        let above = b.block(offset(pos, Face::PosY));
        if self.fluid(above).is_some() && self.wall_open(here, above, Face::PosY) {
            return Some(Fluid::FALLING);
        }
        let left = amount.saturating_sub(self.rules.drop_off);
        (left > 0).then(|| Fluid::flowing(left))
    }

    /// Horizontal neighbors of `pos` that are sources.
    fn source_neighbors(&self, b: &impl Blocks, pos: BlockPos) -> usize {
        HORIZONTAL
            .iter()
            .filter(|&&f| self.is_source(b.block(offset(pos, f))))
            .count()
    }

    /// Shortest way down from `pos`, holding `here` (reached at `depth` from the cell
    /// spreading, coming from `from`), within the slope distance; 1000 when there is none
    /// (Minecraft's `getSlopeDistance`). What the rules say about each cell comes from `memo`.
    fn slope_distance(
        &self,
        memo: &Memo<'_, impl Blocks>,
        pos: BlockPos,
        here: Option<BlockState>,
        depth: u32,
        from: Face,
    ) -> u32 {
        // Out of a cell without collision, the wall into one the fluid passes into is open:
        // only the first cell of a search (a dry block taking a source) may have one.
        let open = here.is_none_or(|s| self.registry().collision(s).is_none());
        let mut best = 1000;
        for face in HORIZONTAL {
            if face == from {
                continue;
            }
            let next = offset(pos, face);
            let (state, passes) = memo.passes(self, next);
            if !passes || !open && !self.wall_open(here, state, face) {
                continue;
            }
            if memo.hole(self, next) {
                return depth;
            }
            if depth < self.rules.slope_find_distance {
                best = best.min(self.slope_distance(memo, next, state, depth + 1, face.opposite()));
            }
        }
        best
    }

    /// The directions `pos`, holding `here`, spreads to and what each neighbor gets
    /// (Minecraft's `getSpread`): those with the shortest way down.
    fn spread_targets(
        &self,
        cx: &TickContext<'_>,
        pos: BlockPos,
        here: Option<BlockState>,
    ) -> Targets {
        self.targets(&Memo::new(cx, pos), pos, here)
    }

    /// [`Self::spread_targets`] through `memo`, a memo around `pos`.
    fn targets(
        &self,
        memo: &Memo<'_, impl Blocks>,
        pos: BlockPos,
        here: Option<BlockState>,
    ) -> Targets {
        let mut min = 1000;
        let mut out = Targets::default();
        for face in HORIZONTAL {
            let next = offset(pos, face);
            let state = memo.block(next);
            // What the neighbor would hold only matters to a dry block that takes a source:
            // skip working it out for the neighbors that cannot hold this fluid at all. For
            // any other neighbor it does not change whether this fluid passes.
            let takes = self.takes_source(state);
            if !takes && !self.can_pass_through(here, state, face, None) {
                continue;
            }
            let new = self.new_liquid(memo, next, state);
            if takes && !self.can_pass_through(here, state, face, new) {
                continue;
            }
            let distance = if memo.hole(self, next) {
                0
            } else {
                self.slope_distance(memo, next, state, 1, face.opposite())
            };
            if distance < min {
                out = Targets::default();
            }
            if distance <= min {
                out[target_slot(face)] = Some((face, new));
                min = distance;
            }
        }
        out
    }

    /// Writes `fluid` into `pos` (empty writes nothing), Minecraft's `spreadTo`: falling into a
    /// fluid that its `falls_into` names makes that block instead (lava into water, stone), and
    /// a dry block that takes a source keeps itself and holds it (`placeLiquid`).
    fn spread_to(&self, cx: &mut TickContext<'_>, pos: BlockPos, fluid: Option<Fluid>, down: bool) {
        let state = cx.get(pos);
        if down
            && let Some(&(_, becomes)) = self
                .fluids
                .get(self.id)
                .falls_into
                .iter()
                .find(|&&(into, _)| self.fluids.holds(state, into))
        {
            cx.set(pos, becomes);
            return;
        }
        let Some(fluid) = fluid else {
            return;
        };
        if self.takes_source(state) {
            if fluid.source
                && let Some(wet) = state.and_then(|s| self.registry().with_water(s, true))
            {
                cx.set(pos, wet);
            }
            return;
        }
        cx.set(pos, self.state(fluid));
    }

    /// Spreads `pos`, holding `here` and `fluid`, to the sides.
    fn spread_to_sides(
        &self,
        cx: &mut TickContext<'_>,
        pos: BlockPos,
        here: Option<BlockState>,
        fluid: Fluid,
    ) {
        let amount = if fluid.falling {
            7
        } else {
            fluid.amount.saturating_sub(self.rules.drop_off)
        };
        if amount == 0 {
            return;
        }
        // Nothing to write without a neighbor it may flow into: skip the search for the way
        // down (it only picks among the neighbors it may write to). Most ticks of standing
        // water end here.
        // A dry block that takes a source counts as open: whether it gets one is worked out
        // with the targets.
        let open = HORIZONTAL.iter().any(|&f| {
            let next = cx.get(offset(pos, f));
            self.takes_source(next) || self.can_spread_to(here, next, f, None)
        });
        if !open {
            return;
        }
        for (face, new) in self.spread_targets(cx, pos, here).into_iter().flatten() {
            let next = offset(pos, face);
            if self.can_spread_to(cx.get(pos), cx.get(next), face, new) {
                self.spread_to(cx, next, new, false);
            }
        }
    }

    /// Spreads `pos`, holding `here` and `fluid`: down, or to the sides.
    fn spread(
        &self,
        cx: &mut TickContext<'_>,
        pos: BlockPos,
        here: Option<BlockState>,
        fluid: Fluid,
    ) {
        let below = offset(pos, Face::NegY);
        let below_state = cx.get(below);
        // Minecraft works out what the cell below would hold first; it only matters to a dry
        // block that takes a source. For any other block it does not change whether this
        // fluid flows there.
        let takes = self.takes_source(below_state);
        let open = takes || self.can_spread_to(here, below_state, Face::NegY, None);
        let new = if open {
            self.new_liquid(&*cx, below, below_state)
        } else {
            None
        };
        if open && (!takes || self.can_spread_to(here, below_state, Face::NegY, new)) {
            self.spread_to(cx, below, new, true);
            if self.source_neighbors(&*cx, pos) >= 3 {
                let here = cx.get(pos);
                self.spread_to_sides(cx, pos, here, fluid);
            }
        } else if fluid.source || !self.is_hole(here, below_state) {
            self.spread_to_sides(cx, pos, here, fluid);
        }
    }

    /// Minecraft's `FlowingFluid::tick` of `pos`, holding `state` and in it `fluid`.
    fn tick(&self, cx: &mut TickContext<'_>, pos: BlockPos, state: BlockState, mut fluid: Fluid) {
        let mut here = Some(state);
        if !fluid.source {
            match self.new_liquid(&*cx, pos, here) {
                None => {
                    cx.set(pos, BlockState::AIR);
                    return;
                }
                Some(new) if new != fluid => {
                    fluid = new;
                    cx.set(pos, self.state(new));
                    // Read again: a reaction to the write may have changed it.
                    here = cx.get(pos);
                }
                Some(_) => {}
            }
        }
        self.spread(cx, pos, here, fluid);
    }
}

/// The tick of a fluid cell: of the fluid it holds.
fn fluid_tick(cx: &mut TickContext<'_>, pos: BlockPos, state: BlockState) {
    let fluids = Fluids::new(cx.registry());
    let Some((id, fluid)) = fluids.content(state) else {
        return;
    };
    let flow = Flow {
        fluids,
        id,
        rules: fluids.get(id).rules,
    };
    flow.tick(cx, pos, state, fluid);
}

/// A fluid meeting another it hardens beside (Minecraft's `LiquidBlock::shouldSpreadLiquid`,
/// `FluidType::hardens`): lava with water above it or beside it, a source turning to obsidian
/// and other lava to cobblestone. The first hardening it meets, in declaration order, wins.
fn fluid_react(cx: &mut TickContext<'_>, pos: BlockPos, state: BlockState) {
    let fluids = Fluids::new(cx.registry());
    let Some((id, fluid)) = fluids.content(state) else {
        return;
    };
    let met = fluids.get(id).hardens.iter().find(|hardening| {
        hardening
            .faces
            .iter()
            .any(|&face| fluids.holds(cx.get(offset(pos, face)), hardening.with))
    });
    if let Some(hardening) = met {
        let product = if fluid.source {
            hardening.source
        } else {
            hardening.flowing
        };
        cx.set(pos, product);
    }
}

#[cfg(test)]
mod tests;
