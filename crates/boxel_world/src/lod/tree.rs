//! The resident LOD tree (ADR 0031): per column and level what the planner wants, what the
//! source reported (the band) and one record per non-uniform node; availability, the octant
//! masks and the ops for the renderer.
//!
//! - A node is *available* when the renderer has it or it has nothing to draw: a sent mesh, a
//!   node built blank, or a layer outside its column's band (uniform: no record at all).
//! - The chain rule: a node is sent only while its parent chain is available (every ancestor up
//!   to the top level), and a node that stops being available takes its sent descendants off
//!   the renderer. So a node on the renderer always has its parent there to draw what it does
//!   not: no finer node ever shows inside an octant its parent draws, and moving away
//!   un-refines at once (a departing node's parent is always available).
//! - A node is *active* when it is wanted and available; *covered* when it is active or its 8
//!   children are covered. A lod-1 node's children are full-detail chunks ([`ChunkCoverage`]).
//! - A node is *touched* when some full-detail chunk inside it is published, and *lost* when it
//!   failed (a job, a face bucket too big) or the renderer refused it and it waits to be sent
//!   again.
//! - A node hides (its mask) the octants whose child is covered, and at level 2 and up also the
//!   octants whose child is touched and not lost: a short hole is better than drawing over full
//!   detail, but a lost child would leave the hole for good, so its parent draws there.
//!
//! Covered and touched children bits are kept per node position (for nodes with a record or
//! not) and updated incrementally: a change walks up at most `K` ancestors.
//!
//! Ops never leave between frames: whatever changes (a result in an idle step, a replan) only
//! marks masks dirty and queues sends; [`Tree::collect`] turns them into ops in the frame.
use super::coverage::ChunkCoverage;
use super::jobs::Built;
use super::plan::{ColumnKey, ColumnPlan, LevelPlan, Span, contains, intersect};
use super::{LevelStats, LodOp};
use boxel_mesh::ChunkMesh;
use boxel_voxel::{ChunkKey, ChunkPos, DimensionId};
use boxel_worldgen::lod::LodColumn;
use foldhash::{HashMap, HashSet};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a node the renderer refused waits before it is sent again (and only once the arena
/// reports room for it).
pub(crate) const REFUSED_RETRY: Duration = Duration::from_secs(1);

/// A node record's state.
#[derive(Clone, Debug)]
pub(crate) enum NodeState {
    /// Waiting for a job.
    Pending,
    /// Built: nothing to draw (emitted uniform inside the band, or meshed to nothing).
    Blank,
    /// Built with quads.
    Ready {
        mesh: Arc<ChunkMesh>,
        /// The octants its quads lie in (bit `x | y << 1 | z << 2`).
        octants: u8,
        /// The renderer has it (its `Upsert` went out and was not refused).
        sent: bool,
        /// Refused by the renderer: not sent again before this.
        retry: Option<Instant>,
    },
    /// Lost: the job failed or a face bucket was too big. Its parent draws its octant.
    Failed,
}

/// A non-uniform node of a column (always a wanted one: a node that leaves the wanted tree
/// goes at once).
#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub(crate) state: NodeState,
    /// The hidden octants, as last computed (and sent, for a sent node).
    pub(crate) mask: u8,
    /// Built from blocks that changed since (an edit): built again, while the renderer keeps
    /// what it has of it.
    pub(crate) stale: bool,
}

impl Node {
    fn pending() -> Self {
        Self {
            state: NodeState::Pending,
            mask: 0,
            stale: false,
        }
    }

    /// Sent or blank.
    pub(crate) fn available(&self) -> bool {
        match self.state {
            NodeState::Blank => true,
            NodeState::Ready { sent, .. } => sent,
            NodeState::Pending | NodeState::Failed => false,
        }
    }

    /// Sent to the renderer.
    pub(crate) fn sent(&self) -> bool {
        matches!(self.state, NodeState::Ready { sent: true, .. })
    }

    /// Failed, or refused and waiting to be sent again.
    fn lost(&self) -> bool {
        matches!(
            self.state,
            NodeState::Failed
                | NodeState::Ready {
                    sent: false,
                    retry: Some(_),
                    ..
                }
        )
    }

    /// Built, not sent, not waiting for a retry: ready to go.
    fn sendable(&self) -> bool {
        matches!(
            self.state,
            NodeState::Ready {
                sent: false,
                retry: None,
                ..
            }
        )
    }
}

/// What a node's column says of it ([`Tree::state`]).
#[derive(Clone, Copy, Debug, Default)]
struct NodeFacts {
    /// It has a record.
    record: bool,
    /// Active (module docs): available and wanted.
    active: bool,
    /// Wanted and outside its column's known band: uniform, with no record (its covered state
    /// is its own).
    uniform: bool,
}

impl NodeFacts {
    /// True if the node is covered (module docs) when `children` are its covered children bits:
    /// active, or not known uniform with all 8 children covered.
    fn covered(self, children: u8) -> bool {
        self.active || (!self.uniform && children == 0xFF)
    }
}

/// The job of a column in flight.
#[derive(Debug)]
pub(crate) struct InFlight {
    pub(crate) id: u64,
    pub(crate) cancel: Arc<AtomicBool>,
}

/// A column of one level.
#[derive(Debug, Default)]
pub(crate) struct Column {
    /// What the planner wants (both `None`: unwanted, kept while a job runs).
    pub(crate) plan: ColumnPlan,
    /// The band the source reported, once known.
    pub(crate) band: Option<LodColumn>,
    /// Records of the non-uniform nodes, by layer.
    pub(crate) nodes: BTreeMap<i64, Node>,
    pub(crate) job: Option<InFlight>,
    /// The ticket of its queue entry (other entries are stale).
    pub(crate) ticket: Option<u32>,
    /// A job panicked: no more jobs for it.
    pub(crate) failed: bool,
}

/// What a column's next job asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Work {
    pub(crate) layers: Span,
    /// Mesh only these layers; `None`: all of `layers` (the band is not known yet).
    pub(crate) only: Option<Vec<i64>>,
}

impl Work {
    /// The layers it builds.
    pub(crate) fn layers(&self) -> impl Iterator<Item = i64> + '_ {
        let all = match &self.only {
            None => Some(self.layers.0..=self.layers.1),
            Some(_) => None,
        };
        all.into_iter()
            .flatten()
            .chain(self.only.iter().flatten().copied())
    }
}

impl Column {
    /// The next job of this column, if it needs one.
    pub(crate) fn work(&self) -> Option<Work> {
        if self.failed || self.job.is_some() {
            return None;
        }
        let wanted = self.plan.wanted?;
        if self.band.is_none() {
            return Some(Work {
                layers: wanted,
                only: None,
            });
        }
        let only: Vec<i64> = self
            .nodes
            .iter()
            .filter(|(_, n)| matches!(n.state, NodeState::Pending) || n.stale)
            .map(|(&y, _)| y)
            .collect();
        let (&lo, &hi) = (only.first()?, only.last()?);
        Some(Work {
            layers: (lo, hi),
            only: Some(only),
        })
    }

    /// The layer of its next job nearest to `eye_layer` (for its priority).
    pub(crate) fn nearest(&self, work: &Work, eye_layer: i64) -> i64 {
        match &work.only {
            None => eye_layer.clamp(work.layers.0, work.layers.1),
            Some(only) => only
                .iter()
                .copied()
                .min_by_key(|y| y.abs_diff(eye_layer))
                .unwrap_or(work.layers.0),
        }
    }

    /// Cancels its job in flight (its result will be dropped).
    fn cancel(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// The parent of node `key` (level + 1).
pub(crate) fn parent(key: ChunkKey) -> ChunkKey {
    ChunkKey {
        dim: key.dim,
        lod: key.lod + 1,
        pos: ChunkPos::new(key.pos.x >> 1, key.pos.y >> 1, key.pos.z >> 1),
    }
}

/// The octant bit of node `key` in its parent.
pub(crate) fn octant_bit(key: ChunkKey) -> u8 {
    let p = key.pos;
    1 << ((p.x & 1) | ((p.y & 1) << 1) | ((p.z & 1) << 2))
}

/// The child of node `key` (level 2 and up) in octant `o` (`x | y << 1 | z << 2`).
pub(crate) fn child(key: ChunkKey, o: u8) -> ChunkKey {
    let p = key.pos;
    ChunkKey {
        dim: key.dim,
        lod: key.lod - 1,
        pos: ChunkPos::new(
            p.x * 2 + i64::from(o & 1),
            p.y * 2 + i64::from((o >> 1) & 1),
            p.z * 2 + i64::from(o >> 2),
        ),
    }
}

/// The layers of `a` not in `b` (up to two intervals).
fn minus(a: Option<Span>, b: Option<Span>) -> [Option<Span>; 2] {
    let Some(a) = a else {
        return [None, None];
    };
    let Some(b) = intersect(Some(a), b) else {
        return [Some(a), None];
    };
    let below = (a.0 < b.0).then_some((a.0, b.0 - 1));
    let above = (b.1 < a.1).then_some((b.1 + 1, a.1));
    [below, above]
}

/// The resident tree (module docs).
#[derive(Debug)]
pub(crate) struct Tree {
    pub(crate) dim: DimensionId,
    /// `K`: the top level.
    pub(crate) levels: u8,
    pub(crate) columns: HashMap<ColumnKey, Column>,
    /// Covered children bits by node position (module docs).
    children: HashMap<ChunkKey, u8>,
    /// Touched children bits by node position, level 2 and up (a lod-1 node is touched when its
    /// coverage has a published bit).
    touched: HashMap<ChunkKey, u8>,
    /// Records whose mask may have changed.
    dirty: HashSet<ChunkKey>,
    /// Ready nodes to send, in order.
    sends: VecDeque<ChunkKey>,
    /// Ready nodes waiting for their parent chain to be available.
    parked: Vec<ChunkKey>,
    /// Something became available since the parked nodes were last looked at.
    chain_changed: bool,
    /// Refused nodes waiting to be sent again.
    refused: Vec<ChunkKey>,
    /// Ops for the next `Collect`.
    ops: Vec<LodOp>,
    /// Columns whose work may have changed (to queue).
    work: Vec<ColumnKey>,
    /// Queued columns whose priority class may have changed (coverage).
    reprioritize: Vec<ColumnKey>,
    /// Lod-1 nodes built with quads since the coverage last looked: position and octants.
    lod1_quads: Vec<(ChunkPos, u8)>,
    /// The octants with quads of every lod-1 node built with a mesh (the coverage asks for
    /// them on every data event: one probe here instead of a column and its records).
    lod1_octants: HashMap<ChunkPos, u8>,
    /// The covered and touched bits still have to be read from the whole coverage (a new tree).
    needs_seed: bool,
    /// Refusals since start.
    pub(crate) refusals: usize,
    /// Nodes lost to a too-big bucket, since start.
    pub(crate) too_big: usize,
    /// Per-level counts (`counts[k - 1]`), kept as records and plans change.
    counts: Vec<LevelStats>,
    /// Wanted columns whose band is known.
    known: usize,
    /// The generator's lowest chunk layer (its floor): level `k`'s floor layer is `floor >> k`.
    floor: Option<i64>,
    /// Columns reserved for ([`Tree::reserve`]), kept by [`Tree::clear`].
    reserved: usize,
}

/// Adds (`add`) or takes away node `key`'s share of the per-level counts.
fn tally(counts: &mut [LevelStats], floor: Option<i64>, key: ChunkKey, node: &Node, add: bool) {
    let Some(l) = counts.get_mut(usize::from(key.lod).wrapping_sub(1)) else {
        return;
    };
    let n = |v: &mut usize| {
        if add {
            *v += 1;
        } else {
            *v -= 1;
        }
    };
    let q = |v: &mut u64, by: u64| {
        if add {
            *v += by;
        } else {
            *v -= by;
        }
    };
    match &node.state {
        NodeState::Ready {
            mesh,
            octants,
            sent,
            ..
        } => {
            let quads = u64::from(mesh.total_quads());
            n(&mut l.nodes);
            q(&mut l.quads, quads);
            if floor.is_some_and(|f| f >> key.lod == key.pos.y) {
                n(&mut l.floor_nodes);
                q(&mut l.floor_quads, quads);
            }
            if *sent && octants & !node.mask != 0 {
                n(&mut l.drawn);
            }
        }
        NodeState::Blank => n(&mut l.blank),
        NodeState::Pending => n(&mut l.pending),
        NodeState::Failed => n(&mut l.failed),
    }
}

impl Tree {
    /// An empty tree of `levels` levels in `dim`; `floor` is the generator's lowest chunk layer
    /// (for the counts).
    pub(crate) fn new(dim: DimensionId, levels: u8, floor: Option<i64>) -> Self {
        Self {
            dim,
            levels,
            columns: HashMap::default(),
            children: HashMap::default(),
            touched: HashMap::default(),
            dirty: HashSet::default(),
            sends: VecDeque::new(),
            parked: Vec::new(),
            chain_changed: false,
            refused: Vec::new(),
            ops: Vec::new(),
            work: Vec::new(),
            reprioritize: Vec::new(),
            lod1_quads: Vec::new(),
            lod1_octants: HashMap::default(),
            needs_seed: true,
            refusals: 0,
            too_big: 0,
            counts: vec![LevelStats::default(); usize::from(levels)],
            known: 0,
            floor,
            reserved: 0,
        }
    }

    /// Makes room for `columns` columns (and their nodes' bits) at once: a hash map that grows
    /// moves every entry in one frame, a few milliseconds for the columns map at `far`.
    pub(crate) fn reserve(&mut self, columns: usize) {
        self.reserved = columns;
        let more = |len: usize| columns.saturating_sub(len);
        self.columns.reserve(more(self.columns.len()));
        self.children.reserve(more(self.children.len()));
        self.touched.reserve(more(self.touched.len()));
        let lod1 = columns / usize::from(self.levels.max(1)) * 2;
        self.lod1_octants
            .reserve(lod1.saturating_sub(self.lod1_octants.len()));
    }

    /// The per-level counts.
    pub(crate) fn counts(&self) -> &[LevelStats] {
        &self.counts
    }

    /// Wanted columns, and those whose band is known.
    pub(crate) fn columns_known(&self) -> (usize, usize) {
        (self.counts.iter().map(|l| l.columns).sum(), self.known)
    }

    /// Changes the record of `key` through `f`, keeping the counts.
    fn update_node<R>(&mut self, key: ChunkKey, f: impl FnOnce(&mut Node) -> R) -> Option<R> {
        let node = self
            .columns
            .get_mut(&ColumnKey::of(key))?
            .nodes
            .get_mut(&key.pos.y)?;
        tally(&mut self.counts, self.floor, key, node, false);
        let r = f(node);
        tally(&mut self.counts, self.floor, key, node, true);
        Some(r)
    }

    /// The record of `key`.
    pub(crate) fn node(&self, key: ChunkKey) -> Option<&Node> {
        self.columns.get(&ColumnKey::of(key))?.nodes.get(&key.pos.y)
    }

    fn node_mut(&mut self, key: ChunkKey) -> Option<&mut Node> {
        self.columns
            .get_mut(&ColumnKey::of(key))?
            .nodes
            .get_mut(&key.pos.y)
    }

    /// True if `key` is wanted and lies outside its column's known band (uniform, no record).
    /// Only wanted layers count: an unwanted column is dropped with its band, and its layers'
    /// covered state must not change then (nothing would tell their parents).
    #[cfg(test)]
    pub(crate) fn known_uniform(&self, key: ChunkKey) -> bool {
        self.columns.get(&ColumnKey::of(key)).is_some_and(|c| {
            contains(c.plan.wanted, key.pos.y)
                && c.band.is_some_and(|b| b.uniform_at(key.pos.y).is_some())
        })
    }

    /// True if node `key` is wanted.
    #[cfg(test)]
    pub(crate) fn wanted(&self, key: ChunkKey) -> bool {
        self.columns
            .get(&ColumnKey::of(key))
            .is_some_and(|c| contains(c.plan.wanted, key.pos.y))
    }

    /// True if node `key` is available (module docs).
    pub(crate) fn available(&self, key: ChunkKey) -> bool {
        let Some(col) = self.columns.get(&ColumnKey::of(key)) else {
            return false;
        };
        match col.nodes.get(&key.pos.y) {
            Some(node) => node.available(),
            None => col.band.is_some_and(|b| b.uniform_at(key.pos.y).is_some()),
        }
    }

    /// True if node `key` is active (module docs): every record is wanted.
    #[cfg(test)]
    pub(crate) fn active(&self, key: ChunkKey) -> bool {
        let Some(col) = self.columns.get(&ColumnKey::of(key)) else {
            return false;
        };
        match col.nodes.get(&key.pos.y) {
            Some(node) => node.available(),
            None => {
                contains(col.plan.wanted, key.pos.y)
                    && col.band.is_some_and(|b| b.uniform_at(key.pos.y).is_some())
            }
        }
    }

    /// True if node `key` is lost (module docs). A column whose job failed before its band was
    /// known has lost every wanted layer.
    pub(crate) fn lost(&self, key: ChunkKey) -> bool {
        let Some(col) = self.columns.get(&ColumnKey::of(key)) else {
            return false;
        };
        match col.nodes.get(&key.pos.y) {
            Some(node) => node.lost(),
            None => col.failed && col.band.is_none() && contains(col.plan.wanted, key.pos.y),
        }
    }

    /// True if every ancestor of node `key` is available (the chain rule of the module docs).
    pub(crate) fn chain_available(&self, mut key: ChunkKey) -> bool {
        while key.lod < self.levels {
            key = parent(key);
            if !self.available(key) {
                return false;
            }
        }
        true
    }

    /// True if an ancestor of node `key` is lost for good (failed): `key` will never be sent.
    fn chain_failed(&self, mut key: ChunkKey) -> bool {
        while key.lod < self.levels {
            key = parent(key);
            let failed = self.columns.get(&ColumnKey::of(key)).is_some_and(|col| {
                match col.nodes.get(&key.pos.y) {
                    Some(node) => matches!(node.state, NodeState::Failed),
                    None => col.failed && col.band.is_none(),
                }
            });
            if failed {
                return true;
            }
        }
        false
    }

    /// The covered children bits of node `key`.
    pub(crate) fn children_bits(&self, key: ChunkKey, cov: &ChunkCoverage) -> u8 {
        if key.lod == 1 {
            cov.get(key.pos).covered
        } else {
            self.children.get(&key).copied().unwrap_or(0)
        }
    }

    /// The touched children bits of node `key` (level 2 and up; for level 1, the published
    /// chunks).
    pub(crate) fn touched_bits(&self, key: ChunkKey, cov: &ChunkCoverage) -> u8 {
        if key.lod == 1 {
            cov.get(key.pos).published
        } else {
            self.touched.get(&key).copied().unwrap_or(0)
        }
    }

    /// True if node `key` is touched (module docs).
    #[cfg(test)]
    pub(crate) fn is_touched(&self, key: ChunkKey, cov: &ChunkCoverage) -> bool {
        self.touched_bits(key, cov) != 0
    }

    /// True if sending node `key` closes a hole in full detail: a lod-1 node with published
    /// chunks that full detail does not cover whole (its parent hides that octant until it is
    /// sent), or a coarser node with such a child (touched and not covered: the chain rule sends
    /// the child only after it).
    pub(crate) fn fills_hole(&self, key: ChunkKey, cov: &ChunkCoverage) -> bool {
        if key.lod == 1 {
            let o = cov.get(key.pos);
            o.published != 0 && o.covered != 0xFF
        } else {
            self.touched_bits(key, cov) & !self.children_bits(key, cov) != 0
        }
    }

    /// The hidden octants of node `key` (module docs).
    pub(crate) fn mask(&self, key: ChunkKey, cov: &ChunkCoverage) -> u8 {
        let covered = self.children_bits(key, cov);
        if key.lod == 1 {
            return covered;
        }
        let touched = self.touched_bits(key, cov) & !covered;
        let mut mask = covered;
        for o in 0..8u8 {
            if touched & (1 << o) != 0 && !self.lost(child(key, o)) {
                mask |= 1 << o;
            }
        }
        mask
    }

    fn mark_dirty(&mut self, key: ChunkKey) {
        if self.node(key).is_some() {
            self.dirty.insert(key);
        }
    }

    /// Node `key`'s lost state may have changed: its parent's mask.
    fn lost_changed(&mut self, key: ChunkKey) {
        if key.lod < self.levels {
            self.mark_dirty(parent(key));
        }
    }

    /// Node `key`'s covered state may have changed: brings its parent's bit up to date, and on
    /// up while the parents' states change.
    pub(crate) fn touch(&mut self, key: ChunkKey, cov: &ChunkCoverage) {
        self.touch_up(key, cov);
    }

    /// [`Tree::touch`], returning the coarsest level whose covered children bits changed (0:
    /// none).
    fn touch_up(&mut self, key: ChunkKey, cov: &ChunkCoverage) -> u8 {
        let now = self.state(key).covered(self.children_bits(key, cov));
        self.touch_up_from(key, now)
    }

    /// [`Tree::touch_up`] with node `key`'s covered state `now` known.
    fn touch_up_from(&mut self, mut key: ChunkKey, mut now: bool) -> u8 {
        let mut top = 0;
        while key.lod < self.levels {
            let up = parent(key);
            let bit = octant_bit(key);
            let old = self.children.get(&up).copied().unwrap_or(0);
            if (old & bit != 0) == now {
                return top;
            }
            let state = self.state(up);
            if now && state.uniform {
                // A uniform parent's covered state is its own: its bits are never read.
                return top;
            }
            let new = if now { old | bit } else { old & !bit };
            if new == 0 {
                self.children.remove(&up);
            } else {
                self.children.insert(up, new);
            }
            top = up.lod;
            if state.record {
                self.dirty.insert(up);
            }
            let after = state.covered(new);
            if after == state.covered(old) {
                return top;
            }
            key = up;
            now = after;
        }
        top
    }

    /// What node `key`'s column says of it, from one lookup.
    fn state(&self, key: ChunkKey) -> NodeFacts {
        let Some(col) = self.columns.get(&ColumnKey::of(key)) else {
            return NodeFacts::default();
        };
        let y = key.pos.y;
        let uniform =
            contains(col.plan.wanted, y) && col.band.is_some_and(|b| b.uniform_at(y).is_some());
        match col.nodes.get(&y) {
            Some(node) => NodeFacts {
                record: true,
                active: node.available(),
                uniform,
            },
            None => NodeFacts {
                record: false,
                active: uniform,
                uniform,
            },
        }
    }

    /// Lod-1 position `p`'s published bits changed (`now`: some chunk there is published): the
    /// touched bits up the levels (a touched node's column may jump to the first priority
    /// class).
    fn touch_published(&mut self, p: ChunkPos, mut now: bool) {
        let mut key = ChunkKey {
            dim: self.dim,
            lod: 1,
            pos: p,
        };
        while key.lod < self.levels {
            let up = parent(key);
            let bit = octant_bit(key);
            let bits = self.touched.get(&up).copied().unwrap_or(0);
            if (bits & bit != 0) == now {
                return;
            }
            let after = if now { bits | bit } else { bits & !bit };
            if after == 0 {
                self.touched.remove(&up);
            } else {
                self.touched.insert(up, after);
            }
            self.mark_dirty(up);
            self.reprioritize.push(ColumnKey::of(up));
            if (bits != 0) == (after != 0) {
                return;
            }
            key = up;
            now = after != 0;
        }
    }

    /// A lod-1 position's full-detail coverage changed: its mask, its parents' covered and
    /// touched bits, the priority of its column and of the ancestors whose bits changed (whether
    /// they close a hole, [`Tree::fills_hole`], depends on nothing else; the touched bits queue
    /// theirs in [`Tree::touch_published`]).
    pub(crate) fn chunks_changed(&mut self, lod1: ChunkPos, cov: &ChunkCoverage) {
        let mut key = ChunkKey {
            dim: self.dim,
            lod: 1,
            pos: lod1,
        };
        let octants = cov.get(lod1);
        let state = self.state(key);
        if state.record {
            self.dirty.insert(key);
        }
        let top = self.touch_up_from(key, state.covered(octants.covered));
        self.touch_published(lod1, octants.published != 0);
        self.reprioritize.push(ColumnKey::of(key));
        while key.lod < top {
            key = parent(key);
            self.reprioritize.push(ColumnKey::of(key));
        }
    }

    /// A new tree reads the whole coverage once (later changes come through
    /// [`Tree::chunks_changed`]).
    pub(crate) fn seed(&mut self, cov: &ChunkCoverage) {
        if std::mem::take(&mut self.needs_seed) {
            let positions: Vec<ChunkPos> = cov.positions().collect();
            for p in positions {
                self.chunks_changed(p, cov);
            }
        }
    }

    /// Touches every layer of `span` in column `c` that has no record (uniform layers whose
    /// active state changed).
    fn touch_uniform(&mut self, c: ColumnKey, span: Option<Span>, cov: &ChunkCoverage) {
        let Some((lo, hi)) = span else {
            return;
        };
        let Some(band) = self.columns.get(&c).and_then(|col| col.band) else {
            return;
        };
        // Only layers outside the band are uniform; the band's own layers have records.
        for part in [
            intersect(Some((lo, hi)), Some((i64::MIN, band.lo - 1))),
            intersect(Some((lo, hi)), Some((band.hi, i64::MAX))),
        ]
        .into_iter()
        .flatten()
        {
            for y in part.0..=part.1 {
                self.touch(c.node(self.dim, y), cov);
            }
        }
    }

    /// Records a column whose work may have changed.
    fn note_work(&mut self, c: ColumnKey) {
        self.work.push(c);
    }

    /// True if columns whose work may have changed wait to be queued.
    pub(crate) fn has_work(&self) -> bool {
        !self.work.is_empty()
    }

    /// The columns whose work may have changed since the last call.
    pub(crate) fn take_work(&mut self) -> Vec<ColumnKey> {
        std::mem::take(&mut self.work)
    }

    /// The queued columns whose priority class may have changed since the last call (sorted,
    /// no repeats).
    pub(crate) fn take_reprioritize(&mut self) -> Vec<ColumnKey> {
        let mut v = std::mem::take(&mut self.reprioritize);
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The lod-1 nodes built with quads since the last call (position, octants with quads).
    pub(crate) fn take_lod1_quads(&mut self) -> Vec<(ChunkPos, u8)> {
        std::mem::take(&mut self.lod1_quads)
    }

    /// The octants where lod-1 node `p` has quads (0 without a built mesh).
    pub(crate) fn lod1_octants(&self, p: ChunkPos) -> u8 {
        self.lod1_octants.get(&p).copied().unwrap_or(0)
    }

    /// Removes the record of `key` (an op if the renderer has it) and updates its parent. The
    /// column stays (the caller drops it once done with it: [`Tree::drop_if_idle`]). Its
    /// descendants leave with it: a node leaves the wanted tree with its whole subtree. A replan
    /// applies its levels coarse first, over frames under the budget, so the descendants' own
    /// records may go a few frames later: the ones on the renderer leave it now (the chain rule).
    fn remove_node(&mut self, key: ChunkKey, cov: &ChunkCoverage) {
        let c = ColumnKey::of(key);
        let Some(col) = self.columns.get_mut(&c) else {
            return;
        };
        let Some(node) = col.nodes.remove(&key.pos.y) else {
            return;
        };
        tally(&mut self.counts, self.floor, key, &node, false);
        if node.sent() {
            self.ops.push(LodOp::Remove { key });
        }
        if key.lod == 1 {
            self.lod1_octants.remove(&key.pos);
        }
        self.dirty.remove(&key);
        // A descendant on the renderer is active, so the covered bits lead to it.
        if key.lod >= 2 && self.children.get(&key).is_some_and(|&b| b != 0) {
            self.unsend_below(key, cov);
        }
        self.touch(key, cov);
        self.lost_changed(key);
    }

    /// Drops column `c` if nothing keeps it: unwanted, no record, no job.
    fn drop_if_idle(&mut self, c: ColumnKey) {
        if let Some(col) = self.columns.get(&c)
            && col.plan.wanted.is_none()
            && col.nodes.is_empty()
            && col.job.is_none()
        {
            self.columns.remove(&c);
        }
    }

    /// Applies column `[x, z]` of level `k` of a plan, `after`, over `before`: its wanted
    /// interval (departures, new records, uniform layers whose active state changed), if it
    /// changed. A replan applies its levels coarse first, every column of both windows
    /// ([`LevelPlan::two_windows`]; a column seen twice changes once).
    pub(crate) fn apply_column(
        &mut self,
        k: u8,
        [x, z]: [i64; 2],
        before: &LevelPlan,
        after: &LevelPlan,
        cov: &ChunkCoverage,
    ) {
        if k > self.levels {
            return;
        }
        let plan = after.get(x, z);
        if before.get(x, z) != plan {
            let c = ColumnKey { level: k, x, z };
            self.set_plan(c, plan.copied().unwrap_or_default(), cov);
        }
    }

    /// Sets the plan of column `c` (see [`Tree::apply_column`]).
    fn set_plan(&mut self, c: ColumnKey, new: ColumnPlan, cov: &ChunkCoverage) {
        let col = self.columns.entry(c).or_default();
        let old = col.plan;
        if old == new {
            return;
        }
        col.plan = new;
        if new.wanted.is_none() {
            col.cancel();
        }
        let band = col.band;
        if old.wanted.is_some() != new.wanted.is_some() {
            let level = &mut self.counts[usize::from(c.level) - 1].columns;
            let known = usize::from(band.is_some());
            if new.wanted.is_some() {
                *level += 1;
                self.known += known;
            } else {
                *level -= 1;
                self.known -= known;
            }
        }
        // Leaving layers: records go (their parent, an ancestor of a sent node, is available
        // and draws there from this frame), uniform layers stop being active.
        for span in minus(old.wanted, new.wanted).into_iter().flatten() {
            let leaving: Vec<i64> = self.columns[&c]
                .nodes
                .range(span.0..=span.1)
                .map(|(&y, _)| y)
                .collect();
            for y in leaving {
                self.remove_node(c.node(self.dim, y), cov);
            }
            self.touch_uniform(c, Some(span), cov);
            if self.columns[&c].failed && band.is_none() {
                for y in span.0..=span.1 {
                    self.lost_changed(c.node(self.dim, y));
                }
            }
        }
        // Entering layers: records inside a known band, uniform layers active. A failed column
        // builds nothing more: its entering layers are lost at once (failed records inside a
        // known band, every layer without one), and their parents draw there.
        let failed = self.columns[&c].failed;
        for span in minus(new.wanted, old.wanted).into_iter().flatten() {
            match band {
                None if failed => {
                    for y in span.0..=span.1 {
                        self.lost_changed(c.node(self.dim, y));
                    }
                }
                None => self.note_work(c),
                Some(band) => {
                    if let Some(inside) = intersect(Some(span), Some((band.lo, band.hi - 1))) {
                        for y in inside.0..=inside.1 {
                            self.want_record(c, y, failed);
                        }
                        self.note_work(c);
                    }
                    self.touch_uniform(c, Some(span), cov);
                }
            }
        }
        self.drop_if_idle(c);
    }

    /// A new record of `c` at layer `y` (none if it has one): pending, or lost (`failed`: the
    /// column's jobs failed, so none will build it).
    fn want_record(&mut self, c: ColumnKey, y: i64, failed: bool) {
        let key = c.node(self.dim, y);
        let col = self.columns.entry(c).or_default();
        if let std::collections::btree_map::Entry::Vacant(e) = col.nodes.entry(y) {
            let node = e.insert(Node::pending());
            if failed {
                node.state = NodeState::Failed;
            }
            tally(&mut self.counts, self.floor, key, node, true);
            if failed {
                self.lost_changed(key);
            }
        }
    }

    /// Starts job `id` for column `c`.
    pub(crate) fn start_job(&mut self, c: ColumnKey, id: u64, cancel: Arc<AtomicBool>) {
        if let Some(col) = self.columns.get_mut(&c) {
            col.ticket = None;
            col.job = Some(InFlight { id, cancel });
        }
    }

    /// The column waiting for job `id`, if any (a result counts only then).
    fn column_of_job(&self, c: ColumnKey, id: u64) -> bool {
        self.columns
            .get(&c)
            .and_then(|col| col.job.as_ref())
            .is_some_and(|j| j.id == id)
    }

    /// Job `id` of column `c` ended without a result (cancelled, or no source): it may be
    /// queued again.
    pub(crate) fn job_ended(&mut self, c: ColumnKey, id: u64) {
        if self.column_of_job(c, id) {
            if let Some(col) = self.columns.get_mut(&c) {
                col.job = None;
            }
            self.note_work(c);
            self.drop_if_idle(c);
        }
    }

    /// Job `id` of column `c` failed: no more jobs for the column, its waiting nodes are lost
    /// (their parents draw their octants).
    pub(crate) fn job_failed(&mut self, c: ColumnKey, id: u64) {
        if !self.column_of_job(c, id) {
            return;
        }
        let mut lost = Vec::new();
        if let Some(col) = self.columns.get_mut(&c) {
            col.job = None;
            col.failed = true;
            for (&y, node) in &mut col.nodes {
                if matches!(node.state, NodeState::Pending) {
                    let key = c.node(self.dim, y);
                    tally(&mut self.counts, self.floor, key, node, false);
                    node.state = NodeState::Failed;
                    tally(&mut self.counts, self.floor, key, node, true);
                    lost.push(key);
                }
            }
            if col.band.is_none()
                && let Some((lo, hi)) = col.plan.wanted
            {
                lost.extend((lo..=hi).map(|y| c.node(self.dim, y)));
            }
        }
        for key in lost {
            self.lost_changed(key);
        }
        self.drop_if_idle(c);
    }

    /// Applies job `id`'s result for column `c` (dropped unless the column still waits for it).
    /// Returns the number of nodes lost to a too-big bucket.
    pub(crate) fn apply_built(
        &mut self,
        c: ColumnKey,
        id: u64,
        band: LodColumn,
        nodes: Vec<(i64, Built)>,
        cov: &ChunkCoverage,
    ) -> usize {
        if !self.column_of_job(c, id) {
            return 0;
        }
        let dim = self.dim;
        let col = self
            .columns
            .get_mut(&c)
            .expect("the column waits for the job");
        col.job = None;
        let first = col.band.is_none();
        let wanted = col.plan.wanted;
        if first {
            col.band = Some(band);
            if wanted.is_some() {
                self.known += 1;
                // Uniform layers are available now.
                self.chain_changed = true;
            }
            if let Some(inside) = intersect(wanted, Some((band.lo, band.hi - 1))) {
                for y in inside.0..=inside.1 {
                    self.want_record(c, y, false);
                }
            }
        } else {
            // Edits came or went (`edits`): the band grows to their layers or shrinks back.
            self.set_band(c, band, true, cov);
        }
        let mut too_big = 0;
        for (y, built) in nodes {
            too_big += usize::from(self.apply_node(c.node(dim, y), built, cov));
        }
        self.too_big += too_big;
        if first {
            // The uniform layers of the wanted interval are active now.
            self.touch_uniform(c, wanted, cov);
        }
        self.note_work(c);
        too_big
    }

    /// Applies what a job built of node `key` if it waits for it (pending, or stale). A stale
    /// node the renderer has (a mesh, or nothing for a blank one) is replaced in this frame's
    /// ops, so it stays available: no hole while it is built again. True if it is lost to a
    /// too-big bucket.
    fn apply_node(&mut self, key: ChunkKey, built: Built, cov: &ChunkCoverage) -> bool {
        let Some(node) = self.node(key) else {
            return false;
        };
        if !matches!(node.state, NodeState::Pending) && !node.stale {
            return false;
        }
        let (was_available, was_lost, was_sent) = (node.available(), node.lost(), node.sent());
        let was_blank = matches!(node.state, NodeState::Blank);
        let retry = match node.state {
            NodeState::Ready { retry, .. } => retry,
            _ => None,
        };
        let too_big = matches!(built, Built::TooBig);
        let mut replace = false;
        let state = match built {
            Built::Uniform | Built::Blank => NodeState::Blank,
            Built::Mesh(mesh, octants) => {
                if key.lod == 1 {
                    self.lod1_quads.push((key.pos, octants));
                    self.lod1_octants.insert(key.pos, octants);
                }
                replace = was_sent || (was_blank && self.chain_available(key));
                NodeState::Ready {
                    mesh,
                    octants,
                    sent: replace,
                    retry: if replace { None } else { retry },
                }
            }
            Built::TooBig => NodeState::Failed,
        };
        if key.lod == 1 && !matches!(state, NodeState::Ready { .. }) {
            self.lod1_octants.remove(&key.pos);
        }
        let sendable = matches!(
            state,
            NodeState::Ready {
                sent: false,
                retry: None,
                ..
            }
        );
        self.update_node(key, |node| {
            node.state = state;
            node.stale = false;
        });
        if replace {
            let mask = self.mask(key, cov);
            let mesh = self
                .update_node(key, |node| {
                    node.mask = mask;
                    match &node.state {
                        NodeState::Ready { mesh, .. } => Arc::clone(mesh),
                        _ => unreachable!("set above"),
                    }
                })
                .expect("the record");
            self.ops.push(LodOp::Upsert { key, mesh, mask });
        } else if was_sent {
            // Blank or lost now: the renderer drops the old mesh.
            self.ops.push(LodOp::Remove { key });
        }
        if sendable {
            self.sends.push_back(key);
        }
        let node = self.node(key).expect("the record");
        let (now_available, now_lost) = (node.available(), node.lost());
        if was_available && !now_available && key.lod >= 2 {
            self.unsend_below(key, cov);
        }
        if was_available != now_available {
            self.touch(key, cov);
            if now_available {
                self.chain_changed = true;
            }
        }
        if was_lost != now_lost {
            self.lost_changed(key);
        }
        too_big
    }

    /// Changes the known band of column `c` to `band` (edits grew it, or their removal shrinks
    /// it). Records outside it go: their layers are uniform, so they stay available. Wanted
    /// layers that enter it were uniform: they get blank records (the renderer had nothing of
    /// them), `stale` if they are to be built.
    pub(crate) fn set_band(
        &mut self,
        c: ColumnKey,
        band: LodColumn,
        stale: bool,
        cov: &ChunkCoverage,
    ) {
        let Some(col) = self.columns.get_mut(&c) else {
            return;
        };
        let Some(old) = col.band.replace(band) else {
            return;
        };
        if old == band {
            return;
        }
        let wanted = col.plan.wanted;
        let outside: Vec<i64> = col
            .nodes
            .keys()
            .copied()
            .filter(|&y| band.uniform_at(y).is_some())
            .collect();
        for y in outside {
            self.drop_to_uniform(c.node(self.dim, y), cov);
        }
        let Some(inside) = intersect(wanted, Some((band.lo, band.hi - 1))) else {
            return;
        };
        for y in inside.0..=inside.1 {
            if old.uniform_at(y).is_none() || self.columns[&c].nodes.contains_key(&y) {
                continue;
            }
            let key = c.node(self.dim, y);
            let node = Node {
                state: NodeState::Blank,
                mask: 0,
                stale,
            };
            tally(&mut self.counts, self.floor, key, &node, true);
            self.columns
                .get_mut(&c)
                .expect("the column")
                .nodes
                .insert(y, node);
            self.recount_children(key, cov);
            self.touch(key, cov);
        }
    }

    /// Removes the record of `key`, whose layer is uniform now (an op if the renderer has it):
    /// the layer stays available, so its descendants stay.
    fn drop_to_uniform(&mut self, key: ChunkKey, cov: &ChunkCoverage) {
        let Some(node) = self
            .columns
            .get_mut(&ColumnKey::of(key))
            .and_then(|col| col.nodes.remove(&key.pos.y))
        else {
            return;
        };
        tally(&mut self.counts, self.floor, key, &node, false);
        if node.sent() {
            self.ops.push(LodOp::Remove { key });
        }
        if key.lod == 1 {
            self.lod1_octants.remove(&key.pos);
        }
        self.dirty.remove(&key);
        if !node.available() {
            self.chain_changed = true;
        }
        self.touch(key, cov);
        self.lost_changed(key);
    }

    /// Recomputes the covered children bits of node `key` (level 2 and up), which were not kept
    /// while its layer was uniform.
    fn recount_children(&mut self, key: ChunkKey, cov: &ChunkCoverage) {
        if key.lod < 2 {
            return;
        }
        let mut bits = 0u8;
        for o in 0..8u8 {
            let c = child(key, o);
            if self.state(c).covered(self.children_bits(c, cov)) {
                bits |= 1 << o;
            }
        }
        if bits == 0 {
            self.children.remove(&key);
        } else {
            self.children.insert(key, bits);
        }
        self.dirty.insert(key);
    }

    /// The blocks under layers `layers` of column `c` changed (an edit): a job running for the
    /// column read the old ones (it is cancelled), the band grows to hold the layers (`edits`),
    /// and their records are stale: built again, the old ones drawn meanwhile. A column whose
    /// band is not known yet sees the edits in its first job.
    pub(crate) fn mark_stale(&mut self, c: ColumnKey, layers: Span, cov: &ChunkCoverage) {
        let Some(col) = self.columns.get_mut(&c) else {
            return;
        };
        col.cancel();
        if let Some(band) = col.band {
            let grown = super::edits::widen(band, Some(layers));
            self.set_band(c, grown, false, cov);
            let col = self.columns.get_mut(&c).expect("the column");
            for (_, node) in col.nodes.range_mut(layers.0..=layers.1) {
                node.stale = true;
            }
        }
        self.note_work(c);
    }

    /// Takes the sent descendants of node `key` off the renderer (`key` is no longer
    /// available: the chain rule); they wait for it to be sent again.
    fn unsend_below(&mut self, key: ChunkKey, cov: &ChunkCoverage) {
        let mut unsent = Vec::new();
        for j in (1..key.lod).rev() {
            let shift = key.lod - j;
            let span = |v: i64| (v << shift, ((v + 1) << shift) - 1);
            let ((x0, x1), (y0, y1), (z0, z1)) =
                (span(key.pos.x), span(key.pos.y), span(key.pos.z));
            for z in z0..=z1 {
                for x in x0..=x1 {
                    let c = ColumnKey { level: j, x, z };
                    let Some(col) = self.columns.get(&c) else {
                        continue;
                    };
                    unsent.extend(
                        col.nodes
                            .range(y0..=y1)
                            .filter(|(_, n)| n.sent())
                            .map(|(&y, _)| c.node(self.dim, y)),
                    );
                }
            }
        }
        for &k in &unsent {
            self.update_node(k, |node| {
                if let NodeState::Ready { sent, .. } = &mut node.state {
                    *sent = false;
                }
            });
            self.ops.push(LodOp::Remove { key: k });
            self.parked.push(k);
        }
        for k in unsent {
            self.touch(k, cov);
        }
    }

    /// The renderer refused the meshes of `keys` (its arena is full): unavailable from now on,
    /// with their descendants off the renderer, sent again after [`REFUSED_RETRY`] once the
    /// arena has room. Lost meanwhile: their parents draw their octants. All of them are marked
    /// first, so a parent refused with its child does not remove the child the renderer never
    /// took.
    pub(crate) fn refuse(&mut self, keys: &[ChunkKey], now: Instant, cov: &ChunkCoverage) {
        let mut refused = Vec::new();
        for &key in keys {
            let changed = self.update_node(key, |node| {
                let NodeState::Ready { sent, retry, .. } = &mut node.state else {
                    return false;
                };
                *sent = false;
                *retry = Some(now + REFUSED_RETRY);
                true
            });
            if changed == Some(true) {
                self.refusals += 1;
                self.refused.push(key);
                refused.push(key);
            }
        }
        for key in refused {
            self.unsend_below(key, cov);
            self.touch(key, cov);
            self.lost_changed(key);
        }
    }

    /// The renderer lost every node (a resync): nothing is available until it is sent again.
    /// The ops not taken yet are dropped (they were for the old nodes), refused nodes stop
    /// waiting, and every built node is sent again: first the lod-1 nodes that close holes in
    /// full detail ([`Tree::fills_hole`]) with their ancestors, then the rest, each coarse to
    /// fine.
    pub(crate) fn resync(&mut self, cov: &ChunkCoverage) {
        self.ops.clear();
        let mut was_sent = Vec::new();
        let mut was_lost = Vec::new();
        let mut ready = Vec::new();
        for (&c, col) in &mut self.columns {
            for (&y, node) in &mut col.nodes {
                let key = c.node(self.dim, y);
                let NodeState::Ready { sent, retry, .. } = &mut node.state else {
                    continue;
                };
                ready.push(key);
                if !*sent && retry.is_none() {
                    continue;
                }
                tally(&mut self.counts, self.floor, key, node, false);
                if let NodeState::Ready { sent, retry, .. } = &mut node.state {
                    if *sent {
                        was_sent.push(key);
                    } else {
                        was_lost.push(key);
                    }
                    *sent = false;
                    *retry = None;
                }
                tally(&mut self.counts, self.floor, key, node, true);
            }
        }
        self.refused.clear();
        self.parked.clear();
        self.dirty.clear();
        for key in was_sent {
            self.touch(key, cov);
        }
        for key in was_lost {
            self.lost_changed(key);
        }
        let mut urgent: HashSet<ChunkKey> = HashSet::default();
        for &key in &ready {
            if key.lod == 1 && self.fills_hole(key, cov) {
                let mut k = key;
                while urgent.insert(k) && k.lod < self.levels {
                    k = parent(k);
                }
            }
        }
        ready.sort_unstable_by_key(|k| (!urgent.contains(k), std::cmp::Reverse(k.lod), *k));
        self.sends = ready.into();
    }

    /// Parked nodes whose chain became available go back to the sends.
    fn unpark(&mut self) {
        if !std::mem::take(&mut self.chain_changed) || self.parked.is_empty() {
            return;
        }
        let parked = std::mem::take(&mut self.parked);
        for key in parked {
            if !self.node(key).is_some_and(Node::sendable) {
                continue;
            }
            if self.chain_available(key) {
                self.sends.push_back(key);
            } else {
                self.parked.push(key);
            }
        }
    }

    /// The frame's ops: refused nodes due again, sends (at most `max_nodes` and `max_bytes` of
    /// quads, at least one; a node whose parent chain is not available waits, parked), then the
    /// masks that changed.
    pub(crate) fn collect(
        &mut self,
        max_nodes: usize,
        max_bytes: usize,
        arena_free: Option<u64>,
        now: Instant,
        cov: &ChunkCoverage,
    ) -> Vec<LodOp> {
        // Refused nodes due again, once the arena reports room for them.
        let mut i = 0;
        while i < self.refused.len() {
            let key = self.refused[i];
            let due = match self.node(key) {
                Some(Node {
                    state: NodeState::Ready { retry, mesh, .. },
                    ..
                }) => Some(
                    retry.is_none_or(|t| now >= t)
                        && arena_free.is_none_or(|f| f >= u64::from(mesh.total_quads())),
                ),
                _ => None,
            };
            match due {
                None => {
                    self.refused.swap_remove(i);
                }
                Some(true) => {
                    self.refused.swap_remove(i);
                    if let Some(Node {
                        state: NodeState::Ready { retry, .. },
                        ..
                    }) = self.node_mut(key)
                    {
                        *retry = None;
                    }
                    self.sends.push_back(key);
                    self.lost_changed(key);
                }
                Some(false) => i += 1,
            }
        }
        let (mut nodes, mut bytes) = (0, 0);
        // Sends; a sent node may free the chain of parked ones, which go in the same frame.
        loop {
            self.unpark();
            let before = nodes;
            while let Some(key) = self.sends.pop_front() {
                let size = self
                    .node(key)
                    .filter(|n| n.sendable())
                    .and_then(|n| match &n.state {
                        NodeState::Ready { mesh, .. } => {
                            Some(mesh.quads.len() * size_of::<boxel_mesh::PackedQuad>())
                        }
                        _ => None,
                    });
                let Some(size) = size else {
                    continue;
                };
                if !self.chain_available(key) {
                    self.parked.push(key);
                    continue;
                }
                if nodes > 0 && (nodes >= max_nodes || bytes + size > max_bytes) {
                    self.sends.push_front(key);
                    break;
                }
                let mask = self.mask(key, cov);
                let mesh = self
                    .update_node(key, |node| {
                        node.mask = mask;
                        let NodeState::Ready { mesh, sent, .. } = &mut node.state else {
                            unreachable!("checked above");
                        };
                        *sent = true;
                        Arc::clone(mesh)
                    })
                    .expect("checked above");
                self.ops.push(LodOp::Upsert { key, mesh, mask });
                nodes += 1;
                bytes += size;
                self.chain_changed = true;
                self.touch(key, cov);
                self.lost_changed(key);
            }
            let full = nodes >= max_nodes || bytes >= max_bytes;
            if nodes == before || full || !self.chain_changed || self.parked.is_empty() {
                break;
            }
        }
        // Masks.
        let dirty: Vec<ChunkKey> = self.dirty.drain().collect();
        for key in dirty {
            let mask = self.mask(key, cov);
            if self.node(key).is_none_or(|n| n.mask == mask) {
                continue;
            }
            let sent = self.update_node(key, |node| {
                node.mask = mask;
                node.sent()
            });
            if sent == Some(true) {
                self.ops.push(LodOp::Mask { key, mask });
            }
        }
        std::mem::take(&mut self.ops)
    }

    /// Built nodes waiting to be sent: the queue (a few entries may be stale) and the parked
    /// nodes, but those under a failed ancestor (they never will be sent). The parked list is
    /// short: parents are built first.
    pub(crate) fn sends_waiting(&self) -> usize {
        self.sends.len()
            + self
                .parked
                .iter()
                .filter(|&&k| self.node(k).is_some_and(Node::sendable) && !self.chain_failed(k))
                .count()
    }

    /// True if refused nodes wait to be sent again.
    pub(crate) fn has_refused(&self) -> bool {
        !self.refused.is_empty()
    }

    /// Drops everything: removes every node the renderer has and cancels every job.
    pub(crate) fn clear(&mut self) -> Vec<LodOp> {
        let mut ops = std::mem::take(&mut self.ops);
        for (&c, col) in &mut self.columns {
            col.cancel();
            for (&y, node) in &col.nodes {
                if node.sent() {
                    ops.push(LodOp::Remove {
                        key: c.node(self.dim, y),
                    });
                }
            }
        }
        let reserved = self.reserved;
        *self = Self::new(self.dim, self.levels, self.floor);
        self.reserve(reserved);
        ops
    }

    /// The per-level counts and the wanted columns with a known band from a walk over every
    /// record (the reference for tests against [`Tree::counts`] and [`Tree::columns_known`]).
    #[cfg(test)]
    pub(crate) fn count_from_scratch(&self) -> (Vec<LevelStats>, usize) {
        let mut levels = vec![LevelStats::default(); usize::from(self.levels)];
        let mut known = 0;
        for (c, col) in &self.columns {
            if col.plan.wanted.is_some() {
                levels[usize::from(c.level) - 1].columns += 1;
                known += usize::from(col.band.is_some());
            }
            for (&y, node) in &col.nodes {
                tally(&mut levels, self.floor, c.node(self.dim, y), node, true);
            }
        }
        (levels, known)
    }

    /// Recomputes every covered bit from scratch (the reference for tests): the children map a
    /// plain walk gives for the current records, bands and plans.
    #[cfg(test)]
    pub(crate) fn children_from_scratch(&self, cov: &ChunkCoverage) -> HashMap<ChunkKey, u8> {
        let mut out: HashMap<ChunkKey, u8> = HashMap::default();
        for k in 1..self.levels {
            // Candidate nodes: every wanted layer and record of level k, every node whose
            // children have bits, and at level 1 every position with coverage.
            let mut nodes: Vec<ChunkKey> = Vec::new();
            for (c, col) in self.columns.iter().filter(|(c, _)| c.level == k) {
                if let Some((lo, hi)) = col.plan.wanted {
                    nodes.extend((lo..=hi).map(|y| c.node(self.dim, y)));
                }
                nodes.extend(col.nodes.keys().map(|&y| c.node(self.dim, y)));
            }
            nodes.extend(out.keys().filter(|key| key.lod == k).copied());
            if k == 1 {
                nodes.extend(cov.positions().map(|pos| ChunkKey {
                    dim: self.dim,
                    lod: 1,
                    pos,
                }));
            }
            nodes.sort_unstable();
            nodes.dedup();
            for key in nodes {
                let bits = if k == 1 {
                    cov.get(key.pos).covered
                } else {
                    out.get(&key).copied().unwrap_or(0)
                };
                let is = self.active(key) || (!self.known_uniform(key) && bits == 0xFF);
                let up = parent(key);
                if is && !self.known_uniform(up) {
                    *out.entry(up).or_insert(0) |= octant_bit(key);
                }
            }
        }
        out
    }

    /// The touched children bits from scratch (the reference for tests).
    #[cfg(test)]
    pub(crate) fn touched_from_scratch(&self, cov: &ChunkCoverage) -> HashMap<ChunkKey, u8> {
        let mut out: HashMap<ChunkKey, u8> = HashMap::default();
        for pos in cov.positions() {
            if cov.get(pos).published == 0 {
                continue;
            }
            let mut key = ChunkKey {
                dim: self.dim,
                lod: 1,
                pos,
            };
            while key.lod < self.levels {
                let up = parent(key);
                *out.entry(up).or_insert(0) |= octant_bit(key);
                key = up;
            }
        }
        out
    }

    /// Ends the back-off of every refused node (tests: as if [`REFUSED_RETRY`] went by).
    #[cfg(test)]
    pub(crate) fn expire_refusals(&mut self) {
        for key in self.refused.clone() {
            if let Some(Node {
                state: NodeState::Ready { retry, .. },
                ..
            }) = self.node_mut(key)
            {
                *retry = None;
            }
        }
    }

    /// The incremental children map (for tests against [`Tree::children_from_scratch`]).
    #[cfg(test)]
    pub(crate) fn children(&self) -> &HashMap<ChunkKey, u8> {
        &self.children
    }

    /// The lod-1 octants with quads from a walk over the records (for tests against
    /// [`Tree::lod1_octants`]).
    #[cfg(test)]
    pub(crate) fn lod1_octants_from_scratch(
        &self,
    ) -> (HashMap<ChunkPos, u8>, HashMap<ChunkPos, u8>) {
        let mut scratch = HashMap::default();
        for (c, col) in self.columns.iter().filter(|(c, _)| c.level == 1) {
            for (&y, node) in &col.nodes {
                if let NodeState::Ready { octants, .. } = node.state {
                    scratch.insert(c.node(self.dim, y).pos, octants);
                }
            }
        }
        (scratch, self.lod1_octants.clone())
    }

    /// The incremental touched map (for tests against [`Tree::touched_from_scratch`]).
    #[cfg(test)]
    pub(crate) fn touched(&self) -> &HashMap<ChunkKey, u8> {
        &self.touched
    }
}
