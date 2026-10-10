//! The fast path of the spread ([`Flow::spread_targets`]: every block read once, what the rules
//! say about each cell worked out once, through a [`Memo`]) gives what the general one gives
//! (Minecraft's `getSpread` and `getSlopeDistance` as written: every rule checked in full at
//! every step, every block read from the world), on random blocks near the origin and far from
//! it, for every fluid of the registry: water, lava and a third one that looks further for a
//! way down than the memo keeps. And the fluids of a registry without a waterlog fluid.
use super::*;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockRegistryBuilder, FaceTextures, FluidReplace, MAX_BLOCK_COORD,
    Property, RenderShape, SlabHalf, TextureId, TexturePattern, TextureSpec, WATERLOGGED,
};
use proptest::prelude::*;
use std::collections::HashMap;
use std::sync::LazyLock;

/// The rules of the third fluid: it looks 6 blocks for a way down (past [`MEMO_REACH`]) and
/// loses 3 per block.
const GOO: FluidRules = FluidRules {
    delay: 10,
    slope_find_distance: 6,
    drop_off: 3,
    infinite: false,
};

/// A fluid block named `name` (its 16 levels), with layer 0 on every face.
fn fluid_block(b: &mut BlockRegistryBuilder, name: &str) -> boxel_voxel::BlockId {
    let def = BlockDef {
        name: name.into(),
        shape: RenderShape::Translucent,
        model: BlockModel::Fluid,
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 1,
        textures: FaceTextures::all(TextureId(0)),
    };
    b.add_block_with_states(def, vec![Property::int(FLUID_LEVEL, 0..=15)], |_, _| {})
        .unwrap();
    b.block_by_name(name).unwrap()
}

/// The demo's blocks and fluids, and a third fluid that water flows into where it is at least
/// 2 high.
static REGISTRY: LazyLock<BlockRegistry> = LazyLock::new(|| {
    let mut b = boxel_demo::blocks::builder();
    let goo = fluid_block(&mut b, "test:goo");
    let goo = b.add_fluid(goo, GOO).unwrap();
    let water = FluidId(0);
    let when = FluidReplace {
        min_amount: 2,
        only_down: false,
    };
    b.fluid_replaced_by(goo, water, when).unwrap();
    b.build()
});

/// Blocks in a hash map; the missing ones are not loaded.
struct World(HashMap<BlockPos, BlockState>);

impl Blocks for World {
    fn block(&self, pos: BlockPos) -> Option<BlockState> {
        self.0.get(&pos).copied()
    }
}

/// How far sideways and down and up from the center the random worlds go: past what a spread
/// reads and past the memo.
const SIDE: i64 = MEMO_REACH + 2;
const DOWN: i64 = 2;
const UP: i64 = 2;
/// Blocks under the center's layer, and in it and above.
const GROUND: usize = ((2 * SIDE + 1) * (2 * SIDE + 1) * DOWN) as usize;
const OPEN: usize = ((2 * SIDE + 1) * (2 * SIDE + 1) * (UP + 1)) as usize;

/// The states of `REGISTRY` that pass `keep`.
fn states(keep: impl Fn(BlockState) -> bool) -> Vec<BlockState> {
    (0..REGISTRY.len() as u16)
        .map(BlockState)
        .filter(|&s| keep(s))
        .collect()
}

/// Dry blocks that would take a water source (slabs, stairs and fences) and waterlogged ones:
/// the cells whose walls the rules check.
static DRY: LazyLock<Vec<BlockState>> =
    LazyLock::new(|| states(|s| REGISTRY.can_be_waterlogged(s) && !REGISTRY.is_waterlogged(s)));
static WET: LazyLock<Vec<BlockState>> = LazyLock::new(|| states(|s| REGISTRY.is_waterlogged(s)));

/// A random block, weighted out of `air + stone + 12`: air, stone, a water source, a dry block
/// that would take one (often enough that two sources sit beside one and it takes them), a
/// waterlogged block, any state of the registry (every level of water and lava, plants,
/// torches) or not loaded.
fn block(air: u32, stone: u32) -> impl Strategy<Value = Option<BlockState>> {
    let len = REGISTRY.len() as u16;
    let pick = |list: &'static LazyLock<Vec<BlockState>>| {
        (0..list.len()).prop_map(move |i| list.get(i).copied())
    };
    prop_oneof![
        air => Just(Some(BlockState::AIR)),
        stone => Just(REGISTRY.by_name("boxel:stone")),
        3 => Just(REGISTRY.by_name("boxel:water")),
        3 => pick(&DRY),
        1 => pick(&WET),
        4 => (0..len).prop_map(|s| Some(BlockState(s))),
        1 => Just(None),
    ]
}

/// The blocks of a random world, from the bottom layer up: mostly stone under the center's
/// layer (so the way down is often a few blocks away), air more than anything else from it up.
fn cells() -> impl Strategy<Value = Vec<Option<BlockState>>> {
    (
        prop::collection::vec(block(2, 9), GROUND),
        prop::collection::vec(block(9, 2), OPEN),
    )
        .prop_map(|(mut ground, open)| {
            ground.extend(open);
            ground
        })
}

/// The center of a world: near the origin, across the edge of `i32`, far out, or against the
/// world limit.
fn center() -> impl Strategy<Value = BlockPos> {
    let far = prop_oneof![
        3 => Just(0i64),
        1 => Just((1i64 << 31) - 3),
        1 => Just(-(1i64 << 40)),
        1 => Just(MAX_BLOCK_COORD - SIDE),
        1 => Just(-MAX_BLOCK_COORD + SIDE),
    ];
    (far.clone(), far.clone(), far).prop_map(|(x, y, z)| BlockPos::new(x, y, z))
}

/// The world of `cells` around `center`.
fn world(center: BlockPos, cells: &[Option<BlockState>]) -> World {
    let mut out = HashMap::new();
    let mut cells = cells.iter();
    for y in -DOWN..=UP {
        for x in -SIDE..=SIDE {
            for z in -SIDE..=SIDE {
                if let Some(Some(state)) = cells.next() {
                    out.insert(
                        BlockPos::new(center.x + x, center.y + y, center.z + z),
                        *state,
                    );
                }
            }
        }
    }
    World(out)
}

/// Minecraft's `getSpread`, the general path: what each neighbor would hold, whether the fluid
/// passes into it as that, the way down from it, and the neighbors with the shortest one (in
/// the order Minecraft writes them).
fn general_targets(
    flow: &Flow<'_>,
    world: &World,
    pos: BlockPos,
    here: Option<BlockState>,
) -> Targets {
    let mut min = 1000;
    let mut found = Vec::new();
    for face in HORIZONTAL {
        let next = offset(pos, face);
        let state = world.block(next);
        let new = flow.new_liquid(world, next, state);
        if !flow.can_pass_through(here, state, face, new) {
            continue;
        }
        let below = world.block(offset(next, Face::NegY));
        let distance = if flow.is_hole(state, below) {
            0
        } else {
            general_slope_distance(flow, world, next, 1, face.opposite())
        };
        if distance < min {
            found.clear();
        }
        if distance <= min {
            found.push((face, new));
            min = distance;
        }
    }
    let mut out = Targets::default();
    for (face, new) in found {
        out[target_slot(face)] = Some((face, new));
    }
    out
}

/// Minecraft's `getSlopeDistance`, the general path.
fn general_slope_distance(
    flow: &Flow<'_>,
    world: &World,
    pos: BlockPos,
    depth: u32,
    from: Face,
) -> u32 {
    let here = world.block(pos);
    let mut best = 1000;
    for face in HORIZONTAL {
        if face == from {
            continue;
        }
        let next = offset(pos, face);
        let state = world.block(next);
        if !flow.can_pass_through(here, state, face, None) {
            continue;
        }
        if flow.is_hole(state, world.block(offset(next, Face::NegY))) {
            return depth;
        }
        if depth < flow.rules.slope_find_distance {
            best = best.min(general_slope_distance(
                flow,
                world,
                next,
                depth + 1,
                face.opposite(),
            ));
        }
    }
    best
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The memo reads what the world holds, every time and anywhere: inside it, outside it
    /// (read through) and where the chunk is not loaded.
    #[test]
    fn the_memo_reads_what_the_world_holds(
        cells in cells(),
        center in center(),
        reads in prop::collection::vec((-SIDE..=SIDE, -DOWN..=UP, -SIDE..=SIDE), 1..96),
    ) {
        let world = world(center, &cells);
        let memo = Memo::new(&world, center);
        for (x, y, z) in reads {
            let pos = BlockPos::new(center.x + x, center.y + y, center.z + z);
            prop_assert_eq!(memo.block(pos), world.block(pos));
        }
    }

    /// The spread of every fluid (water, lava and the third one) picks the same neighbors, with
    /// the same fluid for each, on the fast path as on the general one: from every cell around
    /// the center, whatever it holds.
    #[test]
    fn the_fast_spread_matches_the_general_one(
        cells in cells(),
        center in center(),
    ) {
        let world = world(center, &cells);
        for (i, fluid) in REGISTRY.fluids().iter().enumerate() {
            let kind = FluidId(i as u8);
            let flow = Flow {
                fluids: Fluids::new(&REGISTRY),
                id: kind,
                rules: fluid.rules,
            };
            for (x, z) in (-1..=1).flat_map(|x| (-1..=1).map(move |z| (x, z))) {
                let pos = BlockPos::new(center.x + x, center.y, center.z + z);
                let here = world.block(pos);
                let fast = flow.targets(&Memo::new(&world, pos), pos, here);
                let general = general_targets(&flow, &world, pos, here);
                prop_assert_eq!(fast, general, "{:?} at {:?} holding {:?}", kind, pos, here);
            }
        }
    }
}

#[test]
fn the_registry_has_water_lava_and_the_third_fluid() {
    let rules: Vec<FluidRules> = REGISTRY.fluids().iter().map(|f| f.rules).collect();
    assert_eq!(
        rules,
        [boxel_demo::fluids::WATER, boxel_demo::fluids::LAVA, GOO]
    );
    assert!(GOO.slope_find_distance as i64 + 1 > MEMO_REACH);
    let goo = REGISTRY.by_name("test:goo").unwrap();
    let fluids = Fluids::new(&REGISTRY);
    assert_eq!(
        fluids.content(BlockState(goo.0 + 3)),
        Some((FluidId(2), Fluid::flowing(5)))
    );
}

/// A source of `test:goo` on a stone floor spreads 2 blocks (8, then 5 and 2 left), and only
/// toward the hole 6 blocks away, past what the memo keeps.
#[test]
fn the_third_fluid_finds_a_way_down_6_blocks_away() {
    let registry = &*REGISTRY;
    let stone = registry.by_name("boxel:stone").unwrap();
    let source = registry.by_name("test:goo").unwrap();
    let center = BlockPos::new(1 << 40, -(1 << 31), 7);
    let mut blocks = HashMap::new();
    for x in -8..=8 {
        for z in -8..=8 {
            let floor = BlockPos::new(center.x + x, center.y - 1, center.z + z);
            // The hole: 6 blocks east, open down to a lower floor.
            let hole = x == 6 && z == 0;
            blocks.insert(floor, if hole { BlockState::AIR } else { stone });
            blocks.insert(BlockPos::new(floor.x, floor.y - 1, floor.z), stone);
            for y in 0..=2 {
                let p = BlockPos::new(center.x + x, center.y + y, center.z + z);
                blocks.insert(p, BlockState::AIR);
            }
        }
    }
    blocks.insert(center, source);
    let world = World(blocks);
    let flow = Flow {
        fluids: Fluids::new(registry),
        id: FluidId(2),
        rules: GOO,
    };
    let fast = flow.targets(&Memo::new(&world, center), center, Some(source));
    assert_eq!(fast, general_targets(&flow, &world, center, Some(source)));
    let east = Some((Face::PosX, Some(Fluid::flowing(5))));
    assert_eq!(fast, [None, None, None, east]);
}

/// A registry whose blocks have `waterlogged` but no waterlog fluid: its waterlogged states hold
/// nothing and its dry ones take no source; with one, they hold and take its source.
#[test]
fn the_waterlogged_blocks_hold_the_waterlog_fluid_only() {
    let registry = |waterlog: bool| {
        let mut b = BlockRegistryBuilder::new();
        b.add_texture(TextureSpec {
            name: "test:t".into(),
            base_color: [0; 3],
            noise: 0,
            pattern: TexturePattern::Noise,
        })
        .unwrap();
        let slab = BlockDef {
            name: "test:slab".into(),
            shape: RenderShape::Opaque,
            model: BlockModel::Slab(SlabHalf::Bottom),
            collision: BlockModel::Slab(SlabHalf::Bottom).default_collision(),
            emission: [0; 3],
            light_opacity: 1,
            textures: FaceTextures::all(TextureId(0)),
        };
        b.add_block_with_states(slab, vec![Property::boolean(WATERLOGGED)], |_, _| {})
            .unwrap();
        let block = fluid_block(&mut b, "test:fluid");
        let fluid = b.add_fluid(block, GOO).unwrap();
        if waterlog {
            b.set_waterlog_fluid(fluid).unwrap();
        }
        b.build()
    };
    for waterlog in [false, true] {
        let r = registry(waterlog);
        let wet = r.parse_state("test:slab[waterlogged=true]").unwrap();
        let dry = r.parse_state("test:slab[waterlogged=false]").unwrap();
        assert!(r.is_waterlogged(wet) && r.can_be_waterlogged(dry));
        let fluids = Fluids::new(&r);
        let flow = Flow {
            fluids,
            id: FluidId(0),
            rules: GOO,
        };
        let held = waterlog.then_some((FluidId(0), Fluid::SOURCE));
        assert_eq!(fluids.content(wet), held, "waterlog {waterlog}");
        assert_eq!(fluids.content(dry), None);
        assert_eq!(flow.takes_source(Some(dry)), waterlog);
        assert!(!flow.takes_source(Some(wet)));
    }
}
