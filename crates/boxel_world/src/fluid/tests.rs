//! The fast path of the spread ([`Flow::spread_targets`]: every block read once, what the rules
//! say about each cell worked out once, through a [`Memo`]) gives what the general one gives
//! (Minecraft's `getSpread` and `getSlopeDistance` as written: every rule checked in full at
//! every step, every block read from the world), on random blocks near the origin and far from
//! it.
use super::*;
use boxel_voxel::MAX_BLOCK_COORD;
use proptest::prelude::*;
use std::collections::HashMap;
use std::sync::LazyLock;

static REGISTRY: LazyLock<BlockRegistry> = LazyLock::new(BlockRegistry::default_blocks);

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

    /// The spread of water and of lava picks the same neighbors, with the same fluid for each,
    /// on the fast path as on the general one: from every cell around the center, whatever it
    /// holds.
    #[test]
    fn the_fast_spread_matches_the_general_one(
        cells in cells(),
        center in center(),
    ) {
        let world = world(center, &cells);
        for kind in FluidKind::ALL {
            let flow = Flow {
                fluids: Fluids::new(&REGISTRY),
                kind,
                rules: kind.rules(),
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
