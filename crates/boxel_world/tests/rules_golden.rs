//! G6 of #48: the neighbor and use rules of the built-in content. For every state whose block
//! has a neighbor rule, what the rule makes of seeded random neighborhoods (drawn from every
//! state, and from the blocks the rules look at: stairs, walls, fences, gates, torches,
//! leaves...); the edit lists `NeighborRules::expand` gives in scripted scenes; and what using
//! every state does, looking each way. Frozen before the content moved out of the engine: the
//! literal never changes, only the accessor helper and the setup line below may.

#[path = "../../boxel_voxel/tests/golden/mod.rs"]
mod golden;

use boxel_voxel::{BlockPos, BlockRegistry, BlockState, Facing};
use boxel_world::{BlockEdit, BlockUses, NeighborRules};
use golden::SplitMix64;
use std::collections::HashMap;
use std::fmt::Write as _;

/// What the rule of `s` (if its block has one) makes of `s` among `n`.
fn rule_of(
    rules: &NeighborRules,
    r: &BlockRegistry,
    s: BlockState,
    n: &[BlockState; 6],
) -> Option<BlockState> {
    rules.rule(r, s).map(|rule| rule(r, s, n))
}

/// The registry under test.
fn registry() -> BlockRegistry {
    boxel_demo::blocks::registry()
}

/// The blocks the rules look at, drawn by block and then by state.
const POOL: &[&str] = &[
    "boxel:air",
    "boxel:stone",
    "boxel:leaves",
    "boxel:glass",
    "boxel:water",
    "boxel:lava",
    "boxel:stone_slab",
    "boxel:short_grass",
    "boxel:stone_stairs",
    "boxel:oak_stairs",
    "boxel:oak_fence",
    "boxel:torch",
    "boxel:wall_torch",
    "boxel:cobblestone_wall",
    "boxel:oak_fence_gate",
];

fn draw_from_pool(r: &BlockRegistry, rng: &mut SplitMix64) -> BlockState {
    let block = r
        .block_by_name(POOL[rng.below(POOL.len())])
        .expect("pool block");
    let states: Vec<BlockState> = r.states(block).collect();
    states[rng.below(states.len())]
}

/// A small world for the scenes: `map` over stone at `y <= 0` and air above.
struct Scene<'a> {
    r: &'a BlockRegistry,
    rules: &'a NeighborRules,
    map: HashMap<BlockPos, BlockState>,
}

impl Scene<'_> {
    fn read(&self, p: BlockPos) -> BlockState {
        self.map.get(&p).copied().unwrap_or(if p.y <= 0 {
            self.r.by_name("boxel:stone").expect("stone")
        } else {
            BlockState::AIR
        })
    }

    fn state(&self, text: &str) -> BlockState {
        self.r.parse_state(text).expect(text)
    }

    /// Expands the batch `edits` (positions and state texts), applies it, and dumps it.
    fn batch(&mut self, d: &mut String, name: &str, edits: &[([i64; 3], &str)]) {
        let mut edits: Vec<BlockEdit> = edits
            .iter()
            .map(|&(p, s)| BlockEdit {
                pos: BlockPos::new(p[0], p[1], p[2]),
                state: self.state(s),
            })
            .collect();
        self.rules
            .expand(self.r, |p| Some(self.read(p)), &mut edits);
        let _ = writeln!(d, "scene {name}: {} edits", edits.len());
        for e in &edits {
            let _ = writeln!(
                d,
                "  {} {} {} {}",
                e.pos.x,
                e.pos.y,
                e.pos.z,
                self.r.state_name(e.state)
            );
            self.map.insert(e.pos, e.state);
        }
    }
}

fn dump(r: &BlockRegistry) -> String {
    let rules = NeighborRules::from_registry(r);
    let uses = BlockUses::from_registry(r);
    let mut d = String::new();
    let states: Vec<BlockState> = (0..r.len() as u16).map(BlockState).collect();
    for &s in &states {
        if rule_of(&rules, r, s, &[BlockState::AIR; 6]).is_none() {
            continue;
        }
        let mut rng = SplitMix64(u64::from(s.0));
        let mut out = Vec::with_capacity(128);
        for _ in 0..64 {
            let n: [BlockState; 6] = std::array::from_fn(|_| states[rng.below(states.len())]);
            out.push(rule_of(&rules, r, s, &n).expect("rule").0);
        }
        for _ in 0..64 {
            let n: [BlockState; 6] = std::array::from_fn(|_| draw_from_pool(r, &mut rng));
            out.push(rule_of(&rules, r, s, &n).expect("rule").0);
        }
        let _ = writeln!(d, "rule {} {}: {out:?}", s.0, r.state_name(s));
    }

    let mut scene = Scene {
        r,
        rules: &rules,
        map: HashMap::new(),
    };
    // Stairs around stairs: corners inside and out, a top half.
    scene.batch(
        &mut d,
        "stairs",
        &[
            ([0, 1, 0], "boxel:stone_stairs[facing=east]"),
            ([1, 1, 0], "boxel:stone_stairs[facing=north]"),
            ([0, 1, 1], "boxel:oak_stairs[facing=south]"),
            ([1, 1, 1], "boxel:stone_stairs[facing=east,half=top]"),
            ([2, 1, 1], "boxel:oak_stairs[facing=west,waterlogged=true]"),
        ],
    );
    // A fence run into a gate, beside leaves and glass.
    scene.batch(
        &mut d,
        "fences",
        &[
            ([10, 1, 0], "boxel:oak_fence"),
            ([11, 1, 0], "boxel:oak_fence"),
            ([12, 1, 0], "boxel:oak_fence_gate[facing=north]"),
            ([13, 1, 0], "boxel:oak_fence"),
            ([10, 1, 1], "boxel:leaves"),
            ([13, 1, 1], "boxel:glass"),
            ([11, 1, -1], "boxel:oak_fence_gate[facing=east]"),
        ],
    );
    // Three straight runs of walls stacked, a torch on the middle one, then the torch broken:
    // the posts follow down the column.
    scene.batch(
        &mut d,
        "wall_column",
        &[
            ([19, 1, 0], "boxel:cobblestone_wall"),
            ([20, 1, 0], "boxel:cobblestone_wall"),
            ([21, 1, 0], "boxel:cobblestone_wall"),
            ([19, 2, 0], "boxel:cobblestone_wall"),
            ([20, 2, 0], "boxel:cobblestone_wall"),
            ([21, 2, 0], "boxel:cobblestone_wall"),
            ([19, 3, 0], "boxel:cobblestone_wall"),
            ([20, 3, 0], "boxel:cobblestone_wall"),
            ([21, 3, 0], "boxel:cobblestone_wall"),
            ([20, 4, 0], "boxel:torch"),
        ],
    );
    scene.batch(
        &mut d,
        "wall_column_torch_broken",
        &[([20, 4, 0], "boxel:air")],
    );
    // Gates between walls, both ways.
    scene.batch(
        &mut d,
        "gates_in_walls",
        &[
            ([30, 1, 0], "boxel:cobblestone_wall"),
            ([31, 1, 0], "boxel:oak_fence_gate[facing=north]"),
            ([32, 1, 0], "boxel:cobblestone_wall"),
            ([30, 1, 5], "boxel:cobblestone_wall"),
            ([30, 1, 6], "boxel:oak_fence_gate[facing=east,open=true]"),
            ([30, 1, 7], "boxel:cobblestone_wall[waterlogged=true]"),
        ],
    );
    // Torches on a block that goes: they drop.
    scene.batch(
        &mut d,
        "torches_placed",
        &[
            ([40, 1, 0], "boxel:stone"),
            ([40, 2, 0], "boxel:torch"),
            ([41, 1, 0], "boxel:wall_torch[facing=east]"),
            ([39, 1, 0], "boxel:wall_torch[facing=west]"),
            ([40, 1, 1], "boxel:wall_torch[facing=south]"),
        ],
    );
    scene.batch(&mut d, "torches_support_gone", &[([40, 1, 0], "boxel:air")]);
    // Walls placed bottom up beside fences, a slab and stairs on top.
    scene.batch(
        &mut d,
        "walls_and_fences",
        &[
            ([50, 1, 0], "boxel:cobblestone_wall"),
            ([50, 2, 0], "boxel:cobblestone_wall"),
            ([50, 3, 0], "boxel:cobblestone_wall"),
            ([50, 4, 0], "boxel:cobblestone_wall"),
            ([51, 1, 0], "boxel:cobblestone_wall"),
            ([51, 2, 0], "boxel:cobblestone_wall"),
            ([52, 1, 0], "boxel:oak_fence"),
            ([50, 1, 1], "boxel:oak_fence"),
            ([50, 5, 0], "boxel:stone_slab[type=top]"),
            ([51, 3, 0], "boxel:stone_stairs[facing=west]"),
        ],
    );

    for &s in &states {
        let looks: Vec<Option<u16>> = Facing::ALL
            .iter()
            .map(|&f| uses.use_block(r, s, f).map(|u| u.0))
            .collect();
        if looks.iter().any(Option::is_some) {
            let _ = writeln!(d, "use {} {}: {looks:?}", s.0, r.state_name(s));
        }
    }
    d
}

#[test]
fn the_rules_of_the_content_are_frozen() {
    golden::check("rules", &dump(&registry()), 0xc81f_4bfe_79d3_ce7a);
}
