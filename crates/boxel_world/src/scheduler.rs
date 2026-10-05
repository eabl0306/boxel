//! Priority job queue with a bounded number of jobs in flight (neither rayon nor `boxel_tasks`
//! have priorities).
use crate::ChunkId;

/// Job kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// Generate data.
    Generate,
    /// Build a mesh.
    Mesh,
}

/// A queued job. Lower `priority` runs first (squared chunk distance to the nearest viewer;
/// edit remeshes use 0). Ties: FIFO by `seq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueuedJob {
    /// Priority (lower first).
    pub priority: u32,
    /// Insertion counter.
    pub seq: u64,
    /// Target chunk.
    pub id: ChunkId,
    /// Kind.
    pub kind: JobKind,
}

/// Min-priority queue + in-flight accounting. Duplicate (id, kind) entries are tolerated; the
/// dispatcher re-validates slot state when popping.
#[derive(Debug, Default)]
pub struct JobScheduler {
    heap: std::collections::BinaryHeap<std::cmp::Reverse<(u32, u64, ChunkId, u8)>>,
    next_seq: u64,
    in_flight: usize,
    max_in_flight: usize,
}

impl JobScheduler {
    /// New queue with a cap (>= 1).
    pub fn new(max_in_flight: usize) -> Self {
        Self {
            max_in_flight: max_in_flight.max(1),
            ..Self::default()
        }
    }
    /// Queues a job.
    pub fn push(&mut self, id: ChunkId, kind: JobKind, priority: u32) {
        todo!("WP6")
    }
    /// Pops the best job if below the in-flight cap (and counts it as in flight).
    pub fn pop_ready(&mut self) -> Option<QueuedJob> {
        todo!("WP6")
    }
    /// Marks one job finished (or discarded after pop).
    pub fn complete(&mut self) {
        todo!("WP6")
    }
    /// Rebuilds priorities (`f` returns `None` to drop a job). Called when a viewer changes chunk.
    pub fn reprioritize(&mut self, f: impl FnMut(ChunkId, JobKind) -> Option<u32>) {
        todo!("WP6")
    }
    /// Jobs waiting.
    pub fn queued(&self) -> usize {
        self.heap.len()
    }
    /// Jobs running.
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }
    /// Cap.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }
}
