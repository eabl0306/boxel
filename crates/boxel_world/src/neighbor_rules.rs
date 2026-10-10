//! Blocks whose state follows their neighbors (ADR 0020, ADR 0027): the shape of stairs, the
//! sides of a fence, torches that drop without a support, the sides and post of a wall, a fence
//! gate between walls. A block declares a [`NeighborRule`] (`(registry, state, 6 neighbors) ->
//! state`); when the world applies an edit batch (`WorldSystems::Edits`), every edited block and
//! its 6 face neighbors that have a rule get it applied, against the map with the batch's edits
//! in, and a changed state joins the same batch as one more edit. So placing stairs gives them
//! their shape and turns the stairs next to them in the same frame, with one remesh and one
//! `BlockChanged` per block.
//!
//! A block a rule changed has its 6 neighbors looked at again (ADR 0027): a wall reads the post
//! of the wall above it, so breaking the torch on a column of walls lowers every post down the
//! column, one block per step, in the same batch. The built-in rules end: stairs, fences, torches
//! and gates read nothing a rule changes (their second look changes nothing), and walls only read
//! upward. At most [`MAX_RULE_UPDATES`] such second looks per batch, in case a rule added later
//! never settles. A rule whose block has a neighbor in a chunk that is not readable is skipped
//! (the block keeps its state, so a chunk border never drops a torch or cuts a fence).
//!
//! This runs before the edits are applied and is separate from scheduled block ticks
//! (fluids): it only sees one edit batch and adds edits to it.
//!
//! [`BlockUses`] is the other engine hook of ADR 0027: what using a block does (a gate opens),
//! for the game to ask before it places a block.
use crate::BlockEdit;
use boxel_ecs::prelude::Resource;
use boxel_voxel::rules::{NeighborRule, UseRule, rule_for_model, use_rule_for_model};
use boxel_voxel::{BlockId, BlockPos, BlockRegistry, BlockState, Face, Facing};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};

/// Most blocks [`NeighborRules::expand`] looks at again around the blocks its rules changed, per
/// edit batch: far more than a column of walls needs, a stop for a rule that never settles.
pub const MAX_RULE_UPDATES: usize = 1 << 16;

/// The [`NeighborRule`] of each block of a registry (indexed by [`BlockId`]).
#[derive(Resource, Clone, Debug, Default)]
pub struct NeighborRules {
    by_block: Vec<Option<NeighborRule>>,
}

impl NeighborRules {
    /// The built-in rules for the blocks of `registry`, by the model of each block's default
    /// state (`boxel_voxel::rules::rule_for_model`): stairs, fences, torches and wall torches.
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let by_block = registry
            .blocks()
            .map(|b| rule_for_model(registry.model(registry.default_state(b))))
            .collect();
        Self { by_block }
    }

    /// Sets (or with `None` removes) the rule of `block`.
    pub fn set(&mut self, block: BlockId, rule: Option<NeighborRule>) {
        let i = usize::from(block.0);
        if self.by_block.len() <= i {
            self.by_block.resize(i + 1, None);
        }
        self.by_block[i] = rule;
    }

    /// The rule of the block of `state` in `registry`, if it has one.
    pub fn rule(&self, registry: &BlockRegistry, state: BlockState) -> Option<NeighborRule> {
        let block = registry.block(state)?;
        self.by_block.get(usize::from(block.0)).copied().flatten()
    }

    /// No block has a rule.
    pub fn is_empty(&self) -> bool {
        self.by_block.iter().all(Option::is_none)
    }

    /// Appends to `edits` the neighbor updates the batch needs: every edited position and its 6
    /// face neighbors whose block has a rule, in the order first met, with the batch's edits
    /// in (the last edit of a position wins); then, after each block a rule changed, its 6
    /// neighbors again (up to [`MAX_RULE_UPDATES`]). `read` gives the current block of a
    /// position (`None`: not readable). A rule that changes nothing adds nothing.
    pub fn expand(
        &self,
        registry: &BlockRegistry,
        read: impl Fn(BlockPos) -> Option<BlockState>,
        edits: &mut Vec<BlockEdit>,
    ) {
        if edits.is_empty() || self.is_empty() {
            return;
        }
        let mut overlay: FastMap<BlockPos, BlockState> = fast_map(edits.len());
        for e in edits.iter() {
            overlay.insert(e.pos, e.state);
        }
        // The first ring, in the order first met, and each cell's place in it.
        let mut ring: FastMap<BlockPos, usize> = fast_map(edits.len() * 7);
        let mut cells = Vec::with_capacity(edits.len() * 7);
        for e in edits.iter() {
            for p in std::iter::once(e.pos).chain(Face::ALL.map(|f| neighbor(e.pos, f))) {
                if let Entry::Vacant(slot) = ring.entry(p) {
                    slot.insert(cells.len());
                    cells.push(p);
                }
            }
        }
        // The second looks, after the ring: the neighbors of the blocks a rule changed that the
        // ring already went past, each waiting at most once at a time.
        let mut again = VecDeque::new();
        let mut waiting: FastSet<BlockPos> = FastSet::default();
        let mut updates = 0;
        let mut next = 0;
        loop {
            let p = if let Some(&p) = cells.get(next) {
                next += 1;
                p
            } else if let Some(p) = again.pop_front() {
                waiting.remove(&p);
                p
            } else {
                break;
            };
            let at = |q: BlockPos| overlay.get(&q).copied().or_else(|| read(q));
            let Some(state) = at(p) else {
                continue;
            };
            let Some(rule) = self.rule(registry, state) else {
                continue;
            };
            let mut around = [BlockState::AIR; 6];
            let readable = Face::ALL
                .iter()
                .zip(&mut around)
                .all(|(&f, slot)| at(neighbor(p, f)).map(|s| *slot = s).is_some());
            if !readable {
                continue;
            }
            let new = rule(registry, state, &around);
            if new == state {
                continue;
            }
            edits.push(BlockEdit { pos: p, state: new });
            overlay.insert(p, new);
            // Its neighbors may follow it (a wall under a wall): those with a rule that the ring
            // went past get a second look.
            for (f, &n) in Face::ALL.iter().zip(&around) {
                if updates >= MAX_RULE_UPDATES || self.rule(registry, n).is_none() {
                    continue;
                }
                let q = neighbor(p, *f);
                let ahead = ring.get(&q).is_some_and(|&i| i >= next);
                if !ahead && waiting.insert(q) {
                    updates += 1;
                    again.push_back(q);
                }
            }
        }
    }
}

/// A hash map with the fast fixed hasher of the world's maps (`foldhash`).
type FastMap<K, V> = HashMap<K, V, foldhash::fast::FixedState>;
/// A hash set with the same hasher.
type FastSet<K> = HashSet<K, foldhash::fast::FixedState>;

fn fast_map<K, V>(capacity: usize) -> FastMap<K, V> {
    HashMap::with_capacity_and_hasher(capacity, foldhash::fast::FixedState::default())
}

/// What using each block of a registry does (ADR 0027, indexed by [`BlockId`]): the
/// [`UseRule`] of the blocks a player can use (a fence gate opens and closes). The game asks it
/// before placing a block on a right click ([`BlockUses::use_block`]) and queues the state it
/// gives as an edit.
#[derive(Resource, Clone, Debug, Default)]
pub struct BlockUses {
    by_block: Vec<Option<UseRule>>,
}

impl BlockUses {
    /// The built-in use rules for the blocks of `registry`, by the model of each block's
    /// default state (`boxel_voxel::rules::use_rule_for_model`): fence gates.
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let by_block = registry
            .blocks()
            .map(|b| use_rule_for_model(registry.model(registry.default_state(b))))
            .collect();
        Self { by_block }
    }

    /// Sets (or with `None` removes) the use rule of `block`.
    pub fn set(&mut self, block: BlockId, rule: Option<UseRule>) {
        let i = usize::from(block.0);
        if self.by_block.len() <= i {
            self.by_block.resize(i + 1, None);
        }
        self.by_block[i] = rule;
    }

    /// The use rule of the block of `state`, if it has one.
    pub fn rule(&self, registry: &BlockRegistry, state: BlockState) -> Option<UseRule> {
        let block = registry.block(state)?;
        self.by_block.get(usize::from(block.0)).copied().flatten()
    }

    /// The state `state` takes when used by someone looking toward `look` (horizontally), or
    /// `None` when it is not used that way (the click places a block instead).
    pub fn use_block(
        &self,
        registry: &BlockRegistry,
        state: BlockState,
        look: Facing,
    ) -> Option<BlockState> {
        self.rule(registry, state)?(registry, state, look)
    }
}

/// The face neighbor of `p` (saturating at the ends of `i64`).
fn neighbor(p: BlockPos, face: Face) -> BlockPos {
    let n = face.normal();
    BlockPos::new(
        p.x.saturating_add(i64::from(n.x)),
        p.y.saturating_add(i64::from(n.y)),
        p.z.saturating_add(i64::from(n.z)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small world: the given blocks, air elsewhere, nothing readable at `x >= 100`.
    fn reader(
        blocks: &[(BlockPos, BlockState)],
    ) -> impl Fn(BlockPos) -> Option<BlockState> + use<> {
        let blocks: HashMap<BlockPos, BlockState> = blocks.iter().copied().collect();
        move |p| (p.x < 100).then(|| blocks.get(&p).copied().unwrap_or(BlockState::AIR))
    }

    #[test]
    fn rules_follow_the_batch_one_ring_out() {
        let registry = BlockRegistry::default_blocks();
        let rules = NeighborRules::from_registry(&registry);
        let s = |name: &str| registry.parse_state(name).unwrap();
        let fence = s("boxel:oak_fence");
        let p = BlockPos::new(0, 0, 0);
        let east = BlockPos::new(1, 0, 0);
        let far = BlockPos::new(3, 0, 0);
        // A fence placed next to a fence: both connect, as edits appended to the batch; a fence
        // two cells away is not looked at.
        let read = reader(&[(east, fence), (far, fence)]);
        let mut edits = vec![BlockEdit {
            pos: p,
            state: fence,
        }];
        rules.expand(&registry, &read, &mut edits);
        assert_eq!(
            edits,
            [
                BlockEdit {
                    pos: p,
                    state: fence
                },
                BlockEdit {
                    pos: p,
                    state: s("boxel:oak_fence[east=true]")
                },
                BlockEdit {
                    pos: east,
                    state: s("boxel:oak_fence[west=true]")
                },
            ]
        );
        // Blocks without a rule add nothing.
        let mut edits = vec![BlockEdit {
            pos: p,
            state: s("boxel:stone"),
        }];
        rules.expand(&registry, reader(&[]), &mut edits);
        assert_eq!(edits.len(), 1);
    }

    #[test]
    fn a_rule_next_to_unreadable_blocks_keeps_its_state() {
        let registry = BlockRegistry::default_blocks();
        let rules = NeighborRules::from_registry(&registry);
        let torch = registry.parse_state("boxel:torch").unwrap();
        // A torch in the air at the edge of the readable world: its east neighbor is not
        // readable, so it does not drop.
        let mut edits = vec![BlockEdit {
            pos: BlockPos::new(99, 5, 0),
            state: torch,
        }];
        rules.expand(&registry, reader(&[]), &mut edits);
        assert_eq!(edits.len(), 1);
        // One cell further in, it drops.
        let mut edits = vec![BlockEdit {
            pos: BlockPos::new(98, 5, 0),
            state: torch,
        }];
        rules.expand(&registry, reader(&[]), &mut edits);
        assert_eq!(edits[1].state, BlockState::AIR);
    }

    #[test]
    fn rules_can_be_replaced_per_block() {
        let registry = BlockRegistry::default_blocks();
        let mut rules = NeighborRules::from_registry(&registry);
        let torch = registry.parse_state("boxel:torch").unwrap();
        assert!(rules.rule(&registry, torch).is_some());
        rules.set(registry.block(torch).unwrap(), None);
        assert!(rules.rule(&registry, torch).is_none());
        assert!(!rules.is_empty());
        assert!(NeighborRules::default().is_empty());
    }

    /// Applies `edits` to `blocks`, the last edit of a position winning.
    fn apply(blocks: &mut HashMap<BlockPos, BlockState>, edits: &[BlockEdit]) {
        for e in edits {
            blocks.insert(e.pos, e.state);
        }
    }

    /// Every block of `blocks` with a rule is where its rule leaves it (air outside `blocks`).
    fn settled(
        rules: &NeighborRules,
        registry: &BlockRegistry,
        blocks: &HashMap<BlockPos, BlockState>,
    ) -> Result<(), String> {
        let at = |q: BlockPos| blocks.get(&q).copied().unwrap_or(BlockState::AIR);
        for (&p, &state) in blocks {
            let Some(rule) = rules.rule(registry, state) else {
                continue;
            };
            let around = Face::ALL.map(|f| at(neighbor(p, f)));
            let want = rule(registry, state, &around);
            if want != state {
                return Err(format!(
                    "{p:?} is {} but its rule gives {}",
                    registry.state_name(state),
                    registry.state_name(want)
                ));
            }
        }
        Ok(())
    }

    /// Breaking the torch on a column of walls lowers every post down the column in the same
    /// batch: the change runs down from wall to wall, past the first ring (ADR 0027).
    #[test]
    fn wall_posts_follow_down_a_column() {
        let registry = BlockRegistry::default_blocks();
        let rules = NeighborRules::from_registry(&registry);
        let s = |name: &str| registry.parse_state(name).unwrap();
        // A straight north-south wall three long and three high, far from the origin, with a
        // torch on its middle: placed as one batch.
        let base = BlockPos::new(1 << 40, -(1 << 31), 7);
        let at = |x: i64, y: i64, z: i64| BlockPos::new(base.x + x, base.y + y, base.z + z);
        let mut edits = Vec::new();
        for y in 0..3 {
            for z in -1..=1 {
                edits.push(BlockEdit {
                    pos: at(0, y, z),
                    state: s("boxel:cobblestone_wall"),
                });
            }
        }
        let torch = at(0, 3, 0);
        edits.push(BlockEdit {
            pos: torch,
            state: s("boxel:torch"),
        });
        let mut blocks = HashMap::new();
        rules.expand(&registry, |_| Some(BlockState::AIR), &mut edits);
        apply(&mut blocks, &edits);
        settled(&rules, &registry, &blocks).unwrap();
        let name =
            |blocks: &HashMap<BlockPos, BlockState>, p| registry.state_name(blocks[&p]).to_string();
        for y in 0..3 {
            assert_eq!(
                registry.property(blocks[&at(0, y, 0)], "up"),
                Some("true"),
                "{}",
                name(&blocks, at(0, y, 0))
            );
        }
        // Break the torch: the top wall loses its post, and so does every wall under it.
        let mut edits = vec![BlockEdit {
            pos: torch,
            state: BlockState::AIR,
        }];
        let read = {
            let blocks = blocks.clone();
            move |p| Some(blocks.get(&p).copied().unwrap_or(BlockState::AIR))
        };
        rules.expand(&registry, read, &mut edits);
        apply(&mut blocks, &edits);
        settled(&rules, &registry, &blocks).unwrap();
        for y in 0..3 {
            assert_eq!(
                name(&blocks, at(0, y, 0)),
                format!(
                    "boxel:cobblestone_wall[east=none,north={side},south={side},up=false,\
                     waterlogged=false,west=none]",
                    side = if y == 2 { "low" } else { "tall" }
                )
            );
        }
        // One edit per wall that changed, after the torch.
        assert_eq!(edits.len(), 4);
    }

    /// Random walls, gates, fences, stone and torches in a small box, settled; then random
    /// edits, one batch each: after every batch, every block with a rule is settled again. A
    /// brute-force check that the rules and their second looks reach every block they should.
    #[test]
    fn every_batch_leaves_the_rules_settled() {
        let registry = BlockRegistry::default_blocks();
        let rules = NeighborRules::from_registry(&registry);
        let s = |name: &str| registry.parse_state(name).unwrap();
        let palette = [
            s("boxel:air"),
            s("boxel:air"),
            s("boxel:cobblestone_wall"),
            s("boxel:cobblestone_wall"),
            s("boxel:cobblestone_wall[waterlogged=true]"),
            s("boxel:oak_fence_gate[facing=east]"),
            s("boxel:oak_fence_gate[facing=north,open=true]"),
            s("boxel:oak_fence"),
            s("boxel:stone"),
            s("boxel:torch"),
            s("boxel:stone_slab[type=top]"),
            s("boxel:oak_stairs[facing=west]"),
        ];
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for round in 0..40 {
            let base = BlockPos::new(-(1 << 31) + round, 1 << 30, (1 << 40) - 3);
            let cell = |i: usize| {
                BlockPos::new(
                    base.x + (i % 4) as i64,
                    base.y + (i / 4 % 5) as i64,
                    base.z + (i / 20) as i64,
                )
            };
            // Build the box in one batch and settle it.
            let mut edits: Vec<BlockEdit> = (0..80)
                .map(|i| BlockEdit {
                    pos: cell(i),
                    state: palette[next(palette.len())],
                })
                .collect();
            let mut blocks = HashMap::new();
            rules.expand(&registry, |_| Some(BlockState::AIR), &mut edits);
            apply(&mut blocks, &edits);
            settled(&rules, &registry, &blocks).unwrap_or_else(|e| panic!("round {round}: {e}"));
            for step in 0..6 {
                let mut edits: Vec<BlockEdit> = (0..1 + next(3))
                    .map(|_| BlockEdit {
                        pos: cell(next(80)),
                        state: palette[next(palette.len())],
                    })
                    .collect();
                let read = {
                    let blocks = blocks.clone();
                    move |p| Some(blocks.get(&p).copied().unwrap_or(BlockState::AIR))
                };
                rules.expand(&registry, read, &mut edits);
                apply(&mut blocks, &edits);
                settled(&rules, &registry, &blocks)
                    .unwrap_or_else(|e| panic!("round {round} step {step}: {e}"));
            }
        }
    }

    #[test]
    fn gates_are_used_and_other_blocks_are_not() {
        let registry = BlockRegistry::default_blocks();
        let mut uses = BlockUses::from_registry(&registry);
        let s = |name: &str| registry.parse_state(name).unwrap();
        let gate = s("boxel:oak_fence_gate[facing=north]");
        assert_eq!(
            uses.use_block(&registry, gate, Facing::South),
            Some(s("boxel:oak_fence_gate[facing=south,open=true]"))
        );
        assert_eq!(
            uses.use_block(&registry, s("boxel:stone"), Facing::South),
            None
        );
        uses.set(registry.block(gate).unwrap(), None);
        assert_eq!(uses.use_block(&registry, gate, Facing::South), None);
        assert!(BlockUses::default().rule(&registry, gate).is_none());
    }
}
