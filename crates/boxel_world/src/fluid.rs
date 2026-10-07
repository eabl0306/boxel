//! Flowing water (ADR 0021), Minecraft Java's rules (`FlowingFluid`) on the scheduled block
//! ticks ([`crate::ticks`]):
//!
//! - A fluid cell is a source (`level` 0, amount 8), flowing (`level` 1..=7, amount `8 - level`)
//!   or falling (`level` 8..=15, amount 8, fed from above).
//! - A tick of a flowing cell first recomputes it from its neighbors: falling if the same
//!   fluid is above; a source if two horizontal neighbors are sources and the cell below is
//!   solid or a source (infinite water); else the largest horizontal amount minus one, or air
//!   when that is 0 (so water drains once its source is gone).
//! - Then it spreads: down into the cell below if that can hold it (as falling water), and to
//!   the sides only when it cannot fall (or, falling, with 3 or more source neighbors). Sideways
//!   it takes the directions with the shortest way down within 4 blocks (all of them if none
//!   has one), each neighbor getting what its own neighbors give it. A falling cell spreads like
//!   amount 8 (7 to the sides).
//! - A cell can hold the fluid if it is air or something without collision that is not another
//!   fluid (plants, which the water washes away).
//! - Every change schedules the cell and its neighbors that are fluid 5 game ticks later
//!   (water's delay, 4 updates a second). Worldgen water schedules nothing: a calm lake stays
//!   as it is until an edit touches it.
use crate::WorldContent;
use crate::ticks::{BlockTicks, BlockTicksPlugin, TickContext, TickHandler, offset};
use boxel_app::{App, Plugin};
use boxel_voxel::{BlockCollision, BlockPos, BlockRegistry, BlockState, FLUID_LEVEL, Face};

/// Water's tick delay in game ticks (Minecraft's 5).
pub const WATER_TICK_DELAY: u32 = 5;
/// How far a water cell looks for a way down (Minecraft's slope find distance for water).
pub const SLOPE_FIND_DISTANCE: u32 = 4;

/// The horizontal directions in Minecraft's order (`Direction.Plane.HORIZONTAL`: north, east,
/// south, west), which decides the order of a tick's writes and so of the ticks they schedule.
const HORIZONTAL: [Face; 4] = [Face::NegZ, Face::PosX, Face::PosZ, Face::NegX];

/// What a tick of water reads: up to 5 blocks to the sides (a neighbor and 4 more for the way
/// down), 2 down (a cell that would turn into a source below this one checks the block under
/// it) and 1 up.
pub const WATER_REACH: [u32; 3] = [SLOPE_FIND_DISTANCE + 1, 2, 1];

/// Adds flowing water: registers the tick of `boxel:water` (with [`BlockTicksPlugin`] if it is
/// not there yet). Needs [`WorldPlugin`](crate::WorldPlugin) first; does nothing if the
/// registry has no `boxel:water` with a `level` property.
pub struct FluidPlugin;

impl Plugin for FluidPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<BlockTicks>() {
            app.add_plugins(BlockTicksPlugin);
        }
        let registry = std::sync::Arc::clone(&app.world().resource::<WorldContent>().registry);
        let Some(block) = registry.block_by_name("boxel:water") else {
            return;
        };
        if Fluid::of(&registry, registry.default_state(block)).is_none() {
            return;
        }
        app.world_mut().resource_mut::<BlockTicks>().register(
            &registry,
            block,
            TickHandler {
                delay: WATER_TICK_DELAY,
                reach: WATER_REACH,
                tick: water_tick,
            },
        );
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
    /// model (water).
    pub fn of(registry: &BlockRegistry, state: BlockState) -> Option<Fluid> {
        if registry.model(state) != boxel_voxel::BlockModel::Fluid {
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

/// The water of one tick: the registry's states of one fluid block.
struct Water<'a> {
    registry: &'a BlockRegistry,
    /// The block's states by level.
    states: [BlockState; 16],
}

impl Water<'_> {
    fn fluid(&self, state: Option<BlockState>) -> Option<Fluid> {
        let state = state?;
        let level = self.states.iter().position(|&s| s == state)?;
        Some(Fluid::from_level(level as u8))
    }

    fn state(&self, fluid: Fluid) -> BlockState {
        self.states[usize::from(fluid.level())]
    }

    /// True for a source of this water.
    fn is_source(&self, state: Option<BlockState>) -> bool {
        self.fluid(state).is_some_and(|f| f.source)
    }

    /// Minecraft's `canHoldFluid`: air or anything without collision that is not another fluid
    /// (this water's cells too: they can hold it).
    fn can_hold(&self, state: Option<BlockState>) -> bool {
        let Some(state) = state else {
            return false;
        };
        state.is_air()
            || self.states.contains(&state)
            || (*self.registry.collision(state) == BlockCollision::None
                && Fluid::of(self.registry, state).is_none())
    }

    /// Minecraft's `isSolid`: something that collides (not loaded counts as solid).
    fn is_solid(&self, state: Option<BlockState>) -> bool {
        state.is_none_or(|s| !self.registry.collision(s).is_none())
    }

    /// Fluid that may flow into `state` (Minecraft's `canSpreadTo` for water): an empty cell
    /// that can hold it. Water never replaces water.
    fn can_spread_to(&self, state: Option<BlockState>) -> bool {
        self.fluid(state).is_none() && self.can_hold(state)
    }

    /// Minecraft's `canPassThrough`: not a source of this water and able to hold it.
    fn can_pass_through(&self, state: Option<BlockState>) -> bool {
        !self.is_source(state) && self.can_hold(state)
    }

    /// Minecraft's `isWaterHole`: the cell below holds this water or could.
    fn is_hole(&self, below: Option<BlockState>) -> bool {
        self.fluid(below).is_some() || self.can_hold(below)
    }

    /// What `pos` would hold given its neighbors (Minecraft's `getNewLiquid`); `None` is empty.
    fn new_liquid(&self, cx: &TickContext<'_>, pos: BlockPos) -> Option<Fluid> {
        let (mut amount, mut sources) = (0, 0);
        for face in HORIZONTAL {
            if let Some(f) = self.fluid(cx.get(offset(pos, face))) {
                sources += usize::from(f.source);
                amount = amount.max(f.amount);
            }
        }
        if sources >= 2 {
            let below = cx.get(offset(pos, Face::NegY));
            if self.is_solid(below) || self.is_source(below) {
                return Some(Fluid::SOURCE);
            }
        }
        if self.fluid(cx.get(offset(pos, Face::PosY))).is_some() {
            return Some(Fluid::FALLING);
        }
        (amount > 1).then(|| Fluid::flowing(amount - 1))
    }

    /// Horizontal neighbors of `pos` that are sources.
    fn source_neighbors(&self, cx: &TickContext<'_>, pos: BlockPos) -> usize {
        HORIZONTAL
            .iter()
            .filter(|&&f| self.is_source(cx.get(offset(pos, f))))
            .count()
    }

    /// Shortest way down from `pos` (reached at `depth` from the cell spreading, coming from
    /// `from`), within [`SLOPE_FIND_DISTANCE`]; 1000 when there is none.
    fn slope_distance(&self, cx: &TickContext<'_>, pos: BlockPos, depth: u32, from: Face) -> u32 {
        let mut best = 1000;
        for face in HORIZONTAL {
            if face == from {
                continue;
            }
            let next = offset(pos, face);
            if !self.can_pass_through(cx.get(next)) {
                continue;
            }
            if self.is_hole(cx.get(offset(next, Face::NegY))) {
                return depth;
            }
            if depth < SLOPE_FIND_DISTANCE {
                best = best.min(self.slope_distance(cx, next, depth + 1, face.opposite()));
            }
        }
        best
    }

    /// The directions `pos` spreads to and what each neighbor gets (Minecraft's `getSpread`):
    /// those with the shortest way down.
    fn spread_targets(&self, cx: &TickContext<'_>, pos: BlockPos) -> Vec<(Face, Option<Fluid>)> {
        let mut min = 1000;
        let mut out = Vec::new();
        for face in HORIZONTAL {
            let next = offset(pos, face);
            let state = cx.get(next);
            if !self.can_pass_through(state) {
                continue;
            }
            let new = self.new_liquid(cx, next);
            let distance = if self.is_hole(cx.get(offset(next, Face::NegY))) {
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

    /// Writes `fluid` into `pos` (empty writes nothing).
    fn spread_to(&self, cx: &mut TickContext<'_>, pos: BlockPos, fluid: Option<Fluid>) {
        if let Some(fluid) = fluid {
            cx.set(pos, self.state(fluid));
        }
    }

    fn spread_to_sides(&self, cx: &mut TickContext<'_>, pos: BlockPos, fluid: Fluid) {
        let amount = if fluid.falling { 7 } else { fluid.amount - 1 };
        if amount == 0 {
            return;
        }
        for (face, new) in self.spread_targets(cx, pos) {
            let next = offset(pos, face);
            if self.can_spread_to(cx.get(next)) {
                self.spread_to(cx, next, new);
            }
        }
    }

    fn spread(&self, cx: &mut TickContext<'_>, pos: BlockPos, fluid: Fluid) {
        let below = offset(pos, Face::NegY);
        let below_state = cx.get(below);
        if self.can_spread_to(below_state) {
            let new = self.new_liquid(cx, below);
            self.spread_to(cx, below, new);
            if self.source_neighbors(cx, pos) >= 3 {
                self.spread_to_sides(cx, pos, fluid);
            }
        } else if fluid.source || !self.is_hole(below_state) {
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

/// The tick of a water cell.
fn water_tick(cx: &mut TickContext<'_>, pos: BlockPos, state: BlockState) {
    let registry = cx.registry();
    let (Some(fluid), Some(states)) = (Fluid::of(registry, state), level_states(registry, state))
    else {
        return;
    };
    Water { registry, states }.tick(cx, pos, fluid);
}

/// The states of `state`'s block by level (`None` without a `level` of 16 values).
fn level_states(registry: &BlockRegistry, state: BlockState) -> Option<[BlockState; 16]> {
    let block = registry.block(state)?;
    let property = registry
        .properties(block)
        .iter()
        .find(|p| p.name() == FLUID_LEVEL)?;
    if property.values().len() != 16 {
        return None;
    }
    let mut out = [BlockState::AIR; 16];
    for (slot, value) in out.iter_mut().zip(property.values()) {
        *slot = registry.with(state, FLUID_LEVEL, value)?;
    }
    Some(out)
}
