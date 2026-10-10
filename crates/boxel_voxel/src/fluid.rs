//! Fluid types (ADR 0021, ADR 0025): a block drawn as `BlockModel::Fluid` whose states are its 16
//! `level`s flows when the registry declares it a fluid
//! ([`BlockRegistryBuilder::add_fluid`](crate::BlockRegistryBuilder::add_fluid)), with its
//! [`FluidRules`] (how fast and how far it flows) and what it does with the other fluids: which
//! ones may flow into its cells ([`FluidReplace`]), what falling into one makes, and what it
//! hardens into beside one ([`FluidHardening`]). One fluid may be the one the blocks with a
//! `waterlogged` property hold (`set_waterlog_fluid`). `boxel_world`'s `FluidPlugin` runs them;
//! the registry only describes them.
use crate::{BlockId, BlockState, Face};

/// Most fluids a registry declares.
pub const MAX_FLUIDS: usize = 16;

/// The index of a fluid in its registry, in declaration order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FluidId(pub u8);

/// The numbers that tell one fluid's flow from another's (Minecraft's `FlowingFluid`
/// overrides).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// What a tick reads: `[slope distance + 1, 2, 1]` (sideways, down, up): a neighbor and the
    /// way down past it, 2 down (a cell that would turn into a source below checks the block
    /// under it) and 1 up.
    pub const fn reach(self) -> [u32; 3] {
        [self.slope_find_distance + 1, 2, 1]
    }
}

/// When another fluid may flow into a fluid's cells (Minecraft's
/// `FluidState::canBeReplacedWith`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FluidReplace {
    /// Only where the fluid is at least this high (its amount, `1..=8`; 0: anywhere).
    pub min_amount: u8,
    /// Only falling into it from above.
    pub only_down: bool,
}

/// What a fluid hardens into where it meets another (Minecraft's
/// `LiquidBlock::shouldSpreadLiquid`: lava next to water): a cell of the fluid with fluid `with`
/// across one of `faces` becomes `source` (a source) or `flowing` (any other level), at once.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FluidHardening {
    /// The fluid it meets.
    pub with: FluidId,
    /// Where it looks for it, in order.
    pub faces: Vec<Face>,
    /// What a source becomes.
    pub source: BlockState,
    /// What flowing or falling fluid becomes.
    pub flowing: BlockState,
}

/// A fluid of a registry.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FluidType {
    /// Its block.
    pub block: BlockId,
    /// Its first state, `level` 0 (a source); level `l` is this state plus `l`.
    pub first: BlockState,
    /// How it flows.
    pub rules: FluidRules,
    /// The fluids that may flow into its cells, and when ([`FluidReplace`]). It never replaces
    /// its own cells.
    pub replaced_by: Vec<(FluidId, FluidReplace)>,
    /// What a cell of another fluid becomes when this one falls into it (lava falling into
    /// water makes stone).
    pub falls_into: Vec<(FluidId, BlockState)>,
    /// What it hardens into beside another fluid.
    pub hardens: Option<FluidHardening>,
}
