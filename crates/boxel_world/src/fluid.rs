//! Flowing fluids, Minecraft Java's rules (`FlowingFluid`, `WaterFluid`, `LavaFluid` and
//! `LiquidBlock`) on the scheduled block ticks ([`crate::ticks`]): water (ADR 0021) and lava
//! (ADR 0025).
//!
//! - A fluid cell is a source (`level` 0, amount 8), flowing (`level` 1..=7, amount `8 - level`)
//!   or falling (`level` 8..=15, amount 8, fed from above).
//! - A tick of a flowing cell first recomputes it from its neighbors: falling if the same
//!   fluid is above; for water, a source if two horizontal neighbors are sources and the cell
//!   below is solid or a source (infinite water); else the largest horizontal amount minus the
//!   fluid's drop-off (1 for water, 2 for lava), or air when that is 0 or less (so a fluid
//!   drains once its source is gone).
//! - Then it spreads: down into the cell below if it can (as falling fluid), and to the sides
//!   only when it cannot fall (or, falling, with 3 or more source neighbors). Sideways it takes
//!   the directions with the shortest way down within the fluid's slope distance (4 blocks for
//!   water, 2 for lava; all directions if none has one), each neighbor getting what its own
//!   neighbors give it. A falling cell spreads like amount 8. So water reaches 7 blocks and
//!   lava 3.
//! - A cell can hold a fluid if nothing in it stops movement (air, plants, torches, water and
//!   lava). The fluid may flow into it if what it holds can be replaced: an empty cell always,
//!   water only by lava falling into it (which turns it to stone), lava only by water where the
//!   lava is at least 4/9 high. A fluid never replaces its own cells.
//! - A waterlogged block (a slab, stairs or a fence whose `waterlogged` is `true`, ADR 0025)
//!   holds a water source: it ticks and spreads like one and counts as one for its neighbors.
//!   Flowing water never fills a block (it stops movement), but a dry one where water would
//!   make a new source (two sources beside it) takes it and becomes waterlogged, as in
//!   Minecraft (`canPlaceLiquid`). Between two cells, a fluid passes only where their collision
//!   faces toward each other leave part of the face open (Minecraft's `canPassThroughWall`):
//!   water flows out of the sides of a waterlogged slab but not through the bottom of a bottom
//!   slab.
//! - Lava next to water hardens at once (Minecraft's `LiquidBlock::shouldSpreadLiquid`): a lava
//!   cell with water above it or beside it turns to obsidian if it is a source, to cobblestone
//!   otherwise. This is the lava's tick reaction ([`TickHandler::react`]), so it happens in the
//!   write that brings the two together.
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
    BlockCollision, BlockModel, BlockPos, BlockRegistry, BlockState, FLUID_LEVEL, Face,
};

/// Water's tick delay in game ticks (Minecraft's 5).
pub const WATER_TICK_DELAY: u32 = FluidRules::WATER.delay;
/// Lava's tick delay in game ticks (Minecraft's 30 in the overworld).
pub const LAVA_TICK_DELAY: u32 = FluidRules::LAVA.delay;
/// How far a water cell looks for a way down (Minecraft's slope find distance for water).
pub const SLOPE_FIND_DISTANCE: u32 = FluidRules::WATER.slope_find_distance;

/// The horizontal directions in Minecraft's order (`Direction.Plane.HORIZONTAL`: north, east,
/// south, west), which decides the order of a tick's writes and so of the ticks they schedule.
const HORIZONTAL: [Face; 4] = [Face::NegZ, Face::PosX, Face::PosZ, Face::NegX];

/// Where lava looks for water to harden (Minecraft's `POSSIBLE_FLOW_DIRECTIONS` turned around:
/// above, north, south, west, east; never below).
const LAVA_MEETS: [Face; 5] = [Face::PosY, Face::NegZ, Face::PosZ, Face::NegX, Face::PosX];

/// What a tick of water reads: up to 5 blocks to the sides (a neighbor and 4 more for the way
/// down), 2 down (a cell that would turn into a source below this one checks the block under
/// it) and 1 up.
pub const WATER_REACH: [u32; 3] = FluidRules::WATER.reach();
/// What a tick of lava reads: 3 blocks to the sides, 2 down and 1 up.
pub const LAVA_REACH: [u32; 3] = FluidRules::LAVA.reach();

/// The fluids that flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FluidKind {
    /// `boxel:water`.
    Water,
    /// `boxel:lava`.
    Lava,
}

impl FluidKind {
    /// Both, in the order [`FluidPlugin`] registers them.
    pub const ALL: [FluidKind; 2] = [FluidKind::Water, FluidKind::Lava];

    /// The block's name.
    pub const fn block_name(self) -> &'static str {
        match self {
            FluidKind::Water => "boxel:water",
            FluidKind::Lava => "boxel:lava",
        }
    }

    /// Its rules.
    pub const fn rules(self) -> FluidRules {
        match self {
            FluidKind::Water => FluidRules::WATER,
            FluidKind::Lava => FluidRules::LAVA,
        }
    }
}

/// The numbers that tell one fluid's flow from another's (Minecraft's `FlowingFluid`
/// overrides).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FluidRules {
    /// Game ticks from a change to the tick (`getTickDelay`).
    pub delay: u32,
    /// How far a cell looks for a way down (`getSlopeFindDistance`).
    pub slope_find_distance: u32,
    /// Amount lost per block sideways (`getDropOff`).
    pub drop_off: u8,
    /// Two sources over solid ground make a third (`canConvertToSource`).
    pub infinite: bool,
}

impl FluidRules {
    /// Water: delay 5, slope distance 4, drop-off 1, infinite.
    pub const WATER: FluidRules = FluidRules {
        delay: 5,
        slope_find_distance: 4,
        drop_off: 1,
        infinite: true,
    };
    /// Lava in the overworld: delay 30, slope distance 2, drop-off 2, not infinite.
    pub const LAVA: FluidRules = FluidRules {
        delay: 30,
        slope_find_distance: 2,
        drop_off: 2,
        infinite: false,
    };

    /// What a tick reads: `[slope distance + 1, 2, 1]` (sideways, down, up).
    pub const fn reach(self) -> [u32; 3] {
        [self.slope_find_distance + 1, 2, 1]
    }
}

/// Adds flowing water and lava: registers the ticks of `boxel:water`, of every waterlogged
/// state (water's) and of `boxel:lava` (with [`BlockTicksPlugin`] if it is not there yet), and
/// lava's reaction to water. Needs [`WorldPlugin`](crate::WorldPlugin) first; a fluid missing
/// from the registry (or without its 16 `level` values) does not flow.
pub struct FluidPlugin;

impl Plugin for FluidPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<BlockTicks>() {
            app.add_plugins(BlockTicksPlugin);
        }
        let registry = std::sync::Arc::clone(&app.world().resource::<WorldContent>().registry);
        for kind in FluidKind::ALL {
            let Some(block) = registry.block_by_name(kind.block_name()) else {
                continue;
            };
            if first_level(&registry, kind).is_none() {
                continue;
            }
            let rules = kind.rules();
            let (tick, react): (fn(&mut TickContext<'_>, BlockPos, BlockState), _) = match kind {
                FluidKind::Water => (water_tick, None),
                FluidKind::Lava => (lava_tick, Some(lava_react as _)),
            };
            let handler = TickHandler {
                delay: rules.delay,
                reach: rules.reach(),
                tick,
                react,
            };
            let mut ticks = app.world_mut().resource_mut::<BlockTicks>();
            ticks.register(&registry, block, handler);
            if kind == FluidKind::Water {
                // The water of the waterlogged blocks.
                for state in (0..registry.len() as u16).map(BlockState) {
                    if registry.is_waterlogged(state) {
                        ticks.register_state(&registry, state, handler);
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

/// The first state of `kind`'s block if its states are exactly its 16 levels in order (the
/// block's only property is `level`, `0..=15`): level `l` is that state plus `l`.
fn first_level(registry: &BlockRegistry, kind: FluidKind) -> Option<u16> {
    let block = registry.block_by_name(kind.block_name())?;
    let [property] = registry.properties(block) else {
        return None;
    };
    let values = property.values();
    let ordered = property.name() == FLUID_LEVEL
        && values.len() == 16
        && values.first().is_some_and(|v| v == "0")
        && values.last().is_some_and(|v| v == "15");
    let first = registry.states(block).next()?;
    (ordered && registry.model(first) == BlockModel::Fluid).then_some(first.0)
}

/// The fluids of the registry, as one tick sees them: which states are water and lava.
#[derive(Clone, Copy, Debug)]
struct Fluids<'a> {
    registry: &'a BlockRegistry,
    /// The first state (level 0) of water and of lava ([`first_level`]).
    first: [Option<u16>; 2],
    stone: Option<BlockState>,
    cobblestone: Option<BlockState>,
    obsidian: Option<BlockState>,
}

impl<'a> Fluids<'a> {
    fn new(registry: &'a BlockRegistry) -> Self {
        Self {
            registry,
            first: FluidKind::ALL.map(|kind| first_level(registry, kind)),
            stone: registry.by_name("boxel:stone"),
            cobblestone: registry.by_name("boxel:cobblestone"),
            obsidian: registry.by_name("boxel:obsidian"),
        }
    }

    /// The fluid `state` holds and its kind (a waterlogged block holds a water source).
    fn content(&self, state: BlockState) -> Option<(FluidKind, Fluid)> {
        for (kind, first) in FluidKind::ALL.into_iter().zip(self.first) {
            if let Some(first) = first
                && let Some(level) = state.0.checked_sub(first).filter(|&l| l < 16)
            {
                return Some((kind, Fluid::from_level(level as u8)));
            }
        }
        if self.first[0].is_some() && self.registry.is_waterlogged(state) {
            return Some((FluidKind::Water, Fluid::SOURCE));
        }
        None
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

    /// The state of `kind` holding `fluid`.
    fn state(&self, kind: FluidKind, fluid: Fluid) -> BlockState {
        let first = self.first[kind as usize].unwrap_or_default();
        BlockState(first + u16::from(fluid.level()))
    }

    /// True if `state` holds water.
    fn is_water(&self, state: Option<BlockState>) -> bool {
        state
            .and_then(|s| self.content(s))
            .is_some_and(|(kind, _)| kind == FluidKind::Water)
    }
}

/// One fluid's view of the world in its tick.
struct Flow<'a> {
    fluids: Fluids<'a>,
    kind: FluidKind,
    rules: FluidRules,
}

impl Flow<'_> {
    fn registry(&self) -> &BlockRegistry {
        self.fluids.registry
    }

    /// This fluid in `state`.
    fn fluid(&self, state: Option<BlockState>) -> Option<Fluid> {
        let (kind, fluid) = self.fluids.content(state?)?;
        (kind == self.kind).then_some(fluid)
    }

    fn state(&self, fluid: Fluid) -> BlockState {
        self.fluids.state(self.kind, fluid)
    }

    /// True for a source of this fluid.
    fn is_source(&self, state: Option<BlockState>) -> bool {
        self.fluid(state).is_some_and(|f| f.source)
    }

    /// Minecraft's `canHoldFluid`: nothing in the cell stops movement (air, plants, torches,
    /// water and lava), and it is not a fluid this module does not know.
    fn can_hold(&self, state: Option<BlockState>) -> bool {
        let Some(state) = state else {
            return false;
        };
        let registry = self.registry();
        registry.collision(state).is_none()
            && (registry.model(state) != BlockModel::Fluid || self.fluids.content(state).is_some())
    }

    /// Minecraft's `isSolid`: something that collides (not loaded counts as solid).
    fn is_solid(&self, state: Option<BlockState>) -> bool {
        state.is_none_or(|s| !self.registry().collision(s).is_none())
    }

    /// Minecraft's `FluidState::canBeReplacedWith`: what `state` holds gives way to this fluid
    /// flowing into it (`down`: falling into it). An empty cell does; water only to lava
    /// falling into it; lava only to water, where it is at least 4/9 high (amount 4).
    fn replaceable(&self, state: Option<BlockState>, down: bool) -> bool {
        match state.and_then(|s| self.fluids.content(s)) {
            None => true,
            Some((FluidKind::Water, _)) => down && self.kind != FluidKind::Water,
            Some((FluidKind::Lava, lava)) => self.kind == FluidKind::Water && lava.amount >= 4,
        }
    }

    /// The wall between `from` and `to` across `face` of `from` ([`Fluids::wall_open`]).
    fn wall_open(&self, from: Option<BlockState>, to: Option<BlockState>, face: Face) -> bool {
        self.fluids.wall_open(from, to, face)
    }

    /// True if `state` is a dry block that would take a water source and this fluid is water
    /// (Minecraft's `SimpleWaterloggedBlock::canPlaceLiquid`).
    fn takes_source(&self, state: Option<BlockState>) -> bool {
        self.kind == FluidKind::Water
            && state.is_some_and(|s| {
                let registry = self.registry();
                registry.can_be_waterlogged(s) && !registry.is_waterlogged(s)
            })
    }

    /// Minecraft's `canHoldFluid` for the fluid `new`: [`Self::can_hold`], or a dry block that
    /// takes `new` because it is a water source ([`Self::takes_source`]).
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

    /// What `pos` would hold given its neighbors (Minecraft's `getNewLiquid`); `None` is empty.
    /// A neighbor counts through an open wall only.
    fn new_liquid(&self, cx: &TickContext<'_>, pos: BlockPos) -> Option<Fluid> {
        let here = cx.get(pos);
        let (mut amount, mut sources) = (0, 0);
        for face in HORIZONTAL {
            let neighbor = cx.get(offset(pos, face));
            if let Some(f) = self.fluid(neighbor)
                && self.wall_open(here, neighbor, face)
            {
                sources += usize::from(f.source);
                amount = amount.max(f.amount);
            }
        }
        if self.rules.infinite && sources >= 2 {
            let below = cx.get(offset(pos, Face::NegY));
            if self.is_solid(below) || self.is_source(below) {
                return Some(Fluid::SOURCE);
            }
        }
        let above = cx.get(offset(pos, Face::PosY));
        if self.fluid(above).is_some() && self.wall_open(here, above, Face::PosY) {
            return Some(Fluid::FALLING);
        }
        let left = amount.saturating_sub(self.rules.drop_off);
        (left > 0).then(|| Fluid::flowing(left))
    }

    /// Horizontal neighbors of `pos` that are sources.
    fn source_neighbors(&self, cx: &TickContext<'_>, pos: BlockPos) -> usize {
        HORIZONTAL
            .iter()
            .filter(|&&f| self.is_source(cx.get(offset(pos, f))))
            .count()
    }

    /// Shortest way down from `pos` (reached at `depth` from the cell spreading, coming from
    /// `from`), within the slope distance; 1000 when there is none.
    fn slope_distance(&self, cx: &TickContext<'_>, pos: BlockPos, depth: u32, from: Face) -> u32 {
        let here = cx.get(pos);
        let mut best = 1000;
        for face in HORIZONTAL {
            if face == from {
                continue;
            }
            let next = offset(pos, face);
            let state = cx.get(next);
            if !self.can_pass_through(here, state, face, None) {
                continue;
            }
            if self.is_hole(state, cx.get(offset(next, Face::NegY))) {
                return depth;
            }
            if depth < self.rules.slope_find_distance {
                best = best.min(self.slope_distance(cx, next, depth + 1, face.opposite()));
            }
        }
        best
    }

    /// The directions `pos` spreads to and what each neighbor gets (Minecraft's `getSpread`):
    /// those with the shortest way down.
    fn spread_targets(&self, cx: &TickContext<'_>, pos: BlockPos) -> Vec<(Face, Option<Fluid>)> {
        let here = cx.get(pos);
        let mut min = 1000;
        let mut out = Vec::new();
        for face in HORIZONTAL {
            let next = offset(pos, face);
            let state = cx.get(next);
            // What the neighbor would hold only matters to a dry block that takes a source:
            // skip working it out for the neighbors that cannot hold this fluid at all.
            if !self.takes_source(state) && !self.can_pass_through(here, state, face, None) {
                continue;
            }
            let new = self.new_liquid(cx, next);
            if !self.can_pass_through(here, state, face, new) {
                continue;
            }
            let distance = if self.is_hole(state, cx.get(offset(next, Face::NegY))) {
                0
            } else {
                self.slope_distance(cx, next, 1, face.opposite())
            };
            if distance < min {
                out.clear();
            }
            if distance <= min {
                out.push((face, new));
                min = distance;
            }
        }
        // Minecraft keeps them in an `EnumMap`: north, south, west, east.
        out.sort_by_key(|(face, _)| match face {
            Face::NegZ => 0,
            Face::PosZ => 1,
            Face::NegX => 2,
            _ => 3,
        });
        out
    }

    /// Writes `fluid` into `pos` (empty writes nothing), Minecraft's `spreadTo`: lava falling
    /// into water turns it to stone instead, and a dry block that takes a water source keeps
    /// itself and holds it (`placeLiquid`).
    fn spread_to(&self, cx: &mut TickContext<'_>, pos: BlockPos, fluid: Option<Fluid>, down: bool) {
        let state = cx.get(pos);
        if down && self.kind == FluidKind::Lava && self.fluids.is_water(state) {
            if let Some(stone) = self.fluids.stone {
                cx.set(pos, stone);
            }
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

    fn spread_to_sides(&self, cx: &mut TickContext<'_>, pos: BlockPos, fluid: Fluid) {
        let amount = if fluid.falling {
            7
        } else {
            fluid.amount.saturating_sub(self.rules.drop_off)
        };
        // Nothing to write without a neighbor it may flow into: skip the search for the way
        // down (it only picks among the neighbors it may write to). Most ticks of standing
        // water end here.
        // A dry block that takes a source counts as open: whether it gets one is worked out
        // with the targets.
        let here = cx.get(pos);
        let open = HORIZONTAL.iter().any(|&f| {
            let next = cx.get(offset(pos, f));
            self.takes_source(next) || self.can_spread_to(here, next, f, None)
        });
        if amount == 0 || !open {
            return;
        }
        for (face, new) in self.spread_targets(cx, pos) {
            let next = offset(pos, face);
            if self.can_spread_to(cx.get(pos), cx.get(next), face, new) {
                self.spread_to(cx, next, new, false);
            }
        }
    }

    fn spread(&self, cx: &mut TickContext<'_>, pos: BlockPos, fluid: Fluid) {
        let here = cx.get(pos);
        let below = offset(pos, Face::NegY);
        let below_state = cx.get(below);
        // Minecraft works out what the cell below would hold first; it only matters to a dry
        // block that takes a source.
        let new = (self.takes_source(below_state)
            || self.can_spread_to(here, below_state, Face::NegY, None))
        .then(|| self.new_liquid(cx, below))
        .flatten();
        if self.can_spread_to(here, below_state, Face::NegY, new) {
            self.spread_to(cx, below, new, true);
            if self.source_neighbors(cx, pos) >= 3 {
                self.spread_to_sides(cx, pos, fluid);
            }
        } else if fluid.source || !self.is_hole(here, below_state) {
            self.spread_to_sides(cx, pos, fluid);
        }
    }

    /// Minecraft's `FlowingFluid::tick`.
    fn tick(&self, cx: &mut TickContext<'_>, pos: BlockPos, mut fluid: Fluid) {
        if !fluid.source {
            match self.new_liquid(cx, pos) {
                None => {
                    cx.set(pos, BlockState::AIR);
                    return;
                }
                Some(new) if new != fluid => {
                    fluid = new;
                    cx.set(pos, self.state(new));
                }
                Some(_) => {}
            }
        }
        self.spread(cx, pos, fluid);
    }
}

/// The tick of a fluid cell of `kind`.
fn fluid_tick(kind: FluidKind, cx: &mut TickContext<'_>, pos: BlockPos, state: BlockState) {
    let fluids = Fluids::new(cx.registry());
    let Some((found, fluid)) = fluids.content(state) else {
        return;
    };
    if found != kind {
        return;
    }
    let flow = Flow {
        fluids,
        kind,
        rules: kind.rules(),
    };
    flow.tick(cx, pos, fluid);
}

/// The tick of a water cell.
fn water_tick(cx: &mut TickContext<'_>, pos: BlockPos, state: BlockState) {
    fluid_tick(FluidKind::Water, cx, pos, state);
}

/// The tick of a lava cell.
fn lava_tick(cx: &mut TickContext<'_>, pos: BlockPos, state: BlockState) {
    fluid_tick(FluidKind::Lava, cx, pos, state);
}

/// Lava meeting water (Minecraft's `LiquidBlock::shouldSpreadLiquid`): with water above it or
/// beside it, a lava source turns to obsidian and other lava to cobblestone.
fn lava_react(cx: &mut TickContext<'_>, pos: BlockPos, state: BlockState) {
    let fluids = Fluids::new(cx.registry());
    let Some((FluidKind::Lava, lava)) = fluids.content(state) else {
        return;
    };
    let meets = LAVA_MEETS
        .iter()
        .any(|&face| fluids.is_water(cx.get(offset(pos, face))));
    if !meets {
        return;
    }
    let product = if lava.source {
        fluids.obsidian
    } else {
        fluids.cobblestone
    };
    if let Some(product) = product {
        cx.set(pos, product);
    }
}
