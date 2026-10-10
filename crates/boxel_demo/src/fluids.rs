//! The demo fluids: water and lava (ADR 0021, ADR 0025), their tick delays, reach and what lava
//! makes when it meets water. The blocks are in [`crate::blocks`]; [`declare`] makes them
//! fluids of the registry.
use boxel_voxel::{BlockRegistryBuilder, Face, FluidHardening, FluidReplace, FluidRules};

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

/// Water's tick delay in game ticks (Minecraft's 5).
pub const WATER_TICK_DELAY: u32 = WATER.delay;
/// Lava's tick delay in game ticks (Minecraft's 30 in the overworld).
pub const LAVA_TICK_DELAY: u32 = LAVA.delay;
/// How far a water cell looks for a way down (Minecraft's slope find distance for water).
pub const SLOPE_FIND_DISTANCE: u32 = WATER.slope_find_distance;

/// What a tick of water reads: up to 5 blocks to the sides (a neighbor and 4 more for the way
/// down), 2 down (a cell that would turn into a source below this one checks the block under
/// it) and 1 up.
pub const WATER_REACH: [u32; 3] = WATER.reach();
/// What a tick of lava reads: 3 blocks to the sides, 2 down and 1 up.
pub const LAVA_REACH: [u32; 3] = LAVA.reach();

/// Where lava looks for water to harden (Minecraft's `POSSIBLE_FLOW_DIRECTIONS` turned around:
/// above, north, south, west, east; never below).
pub const LAVA_MEETS: [Face; 5] = [Face::PosY, Face::NegZ, Face::PosZ, Face::NegX, Face::PosX];

/// Declares the demo fluids in `b`, which holds the demo blocks: water (the fluid of the
/// waterlogged blocks) and lava; lava flows into water only falling into it, and water into
/// lava where the lava is at least 4/9 high; lava falling into water makes stone; lava beside
/// water hardens, a source to obsidian and the rest to cobblestone.
pub fn declare(b: &mut BlockRegistryBuilder) {
    let block = |b: &BlockRegistryBuilder, name: &str| b.block_by_name(name).expect(name);
    let state = |b: &BlockRegistryBuilder, name: &str| b.by_name(name).expect(name);
    let water_block = block(b, "boxel:water");
    let water = b.add_fluid(water_block, WATER).expect("demo water");
    b.set_waterlog_fluid(water).expect("demo water");
    let lava_block = block(b, "boxel:lava");
    let lava = b.add_fluid(lava_block, LAVA).expect("demo lava");
    let replace = |min_amount, only_down| FluidReplace {
        min_amount,
        only_down,
    };
    b.fluid_replaced_by(water, lava, replace(0, true))
        .expect("demo water");
    b.fluid_replaced_by(lava, water, replace(4, false))
        .expect("demo lava");
    let stone = state(b, "boxel:stone");
    b.fluid_falls_into(lava, water, stone).expect("demo lava");
    let hardening = FluidHardening {
        with: water,
        faces: LAVA_MEETS.to_vec(),
        source: state(b, "boxel:obsidian"),
        flowing: state(b, "boxel:cobblestone"),
    };
    b.fluid_hardens(lava, hardening).expect("demo lava");
}
