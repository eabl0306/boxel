//! Blocks whose state follows their neighbors (ADR 0020): the shape of stairs, the sides of a
//! fence, torches that drop without a support. A block declares a [`NeighborRule`]
//! (`(registry, state, 6 neighbors) -> state`); when the world applies an edit batch
//! (`WorldSystems::Edits`), every edited block and its 6 face neighbors that have a rule get
//! it applied, against the map with the batch's edits in, and a changed state joins the same
//! batch as one more edit. So placing stairs gives them their shape and turns the stairs next
//! to them in the same frame, with one remesh and one `BlockChanged` per block.
//!
//! One ring only: the cells a rule changed are not looked at again. The built-in rules
//! (`boxel_voxel::rules`) never need more, because no rule reads what another one changes. A
//! rule whose block has a neighbor in a chunk that is not readable is skipped (the block keeps
//! its state, so a chunk border never drops a torch or cuts a fence).
//!
//! This runs before the edits are applied and is separate from scheduled block ticks
//! (fluids): it only sees one edit batch and adds edits to it.
use crate::BlockEdit;
use boxel_ecs::prelude::Resource;
use boxel_voxel::rules::{NeighborRule, rule_for_model};
use boxel_voxel::{BlockId, BlockPos, BlockRegistry, BlockState, Face};
use std::collections::{HashMap, HashSet};

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
    /// in (the last edit of a position wins). `read` gives the current block of a position
    /// (`None`: not readable). A rule that changes nothing adds nothing.
    pub fn expand(
        &self,
        registry: &BlockRegistry,
        read: impl Fn(BlockPos) -> Option<BlockState>,
        edits: &mut Vec<BlockEdit>,
    ) {
        if edits.is_empty() || self.is_empty() {
            return;
        }
        let mut overlay: HashMap<BlockPos, BlockState> = HashMap::with_capacity(edits.len());
        for e in edits.iter() {
            overlay.insert(e.pos, e.state);
        }
        let mut seen: HashSet<BlockPos> = HashSet::new();
        let mut cells = Vec::new();
        for e in edits.iter() {
            for p in std::iter::once(e.pos).chain(Face::ALL.map(|f| neighbor(e.pos, f))) {
                if seen.insert(p) {
                    cells.push(p);
                }
            }
        }
        for p in cells {
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
            if new != state {
                edits.push(BlockEdit { pos: p, state: new });
                overlay.insert(p, new);
            }
        }
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
}
