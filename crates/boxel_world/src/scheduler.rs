//! Priority job queue with a bounded number of jobs in flight (neither rayon nor `boxel_tasks`
//! have priorities).
use crate::ChunkId;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Job kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// Generate data.
    Generate,
    /// Build a mesh.
    Mesh,
}

impl JobKind {
    /// Heap encoding (keeps the heap entry `Ord` without deriving it on the public enum).
    const fn code(self) -> u8 {
        match self {
            JobKind::Generate => 0,
            JobKind::Mesh => 1,
        }
    }
    const fn from_code(code: u8) -> Self {
        match code {
            0 => JobKind::Generate,
            _ => JobKind::Mesh,
        }
    }
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
    heap: BinaryHeap<Reverse<(u32, u64, ChunkId, u8)>>,
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
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.heap.push(Reverse((priority, seq, id, kind.code())));
    }
    /// Pops the best job if below the in-flight cap (and counts it as in flight).
    pub fn pop_ready(&mut self) -> Option<QueuedJob> {
        if self.in_flight >= self.max_in_flight {
            return None;
        }
        let Reverse((priority, seq, id, code)) = self.heap.pop()?;
        self.in_flight += 1;
        Some(QueuedJob {
            priority,
            seq,
            id,
            kind: JobKind::from_code(code),
        })
    }
    /// Marks one job finished (or discarded after pop).
    pub fn complete(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }
    /// Rebuilds priorities (`f` returns `None` to drop a job). Called when a viewer changes chunk.
    /// Insertion order (`seq`) is kept, so ties stay FIFO.
    pub fn reprioritize(&mut self, mut f: impl FnMut(ChunkId, JobKind) -> Option<u32>) {
        let mut jobs = std::mem::take(&mut self.heap).into_vec();
        jobs.retain_mut(|Reverse((priority, _, id, code))| {
            f(*id, JobKind::from_code(*code)).is_some_and(|p| {
                *priority = p;
                true
            })
        });
        self.heap = BinaryHeap::from(jobs);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<ChunkId> {
        let mut slots = slotmap::SlotMap::<ChunkId, ()>::with_key();
        (0..n).map(|_| slots.insert(())).collect()
    }

    fn drain(s: &mut JobScheduler) -> Vec<(ChunkId, JobKind, u32)> {
        let mut out = Vec::new();
        while let Some(job) = s.pop_ready() {
            out.push((job.id, job.kind, job.priority));
            s.complete();
        }
        out
    }

    #[test]
    fn pops_lowest_priority_first() {
        let id = ids(4);
        let mut s = JobScheduler::new(8);
        s.push(id[0], JobKind::Generate, 9);
        s.push(id[1], JobKind::Mesh, 1);
        s.push(id[2], JobKind::Generate, 4);
        s.push(id[3], JobKind::Mesh, 0);
        assert_eq!(s.queued(), 4);
        assert_eq!(
            drain(&mut s),
            vec![
                (id[3], JobKind::Mesh, 0),
                (id[1], JobKind::Mesh, 1),
                (id[2], JobKind::Generate, 4),
                (id[0], JobKind::Generate, 9),
            ]
        );
        assert_eq!(s.queued(), 0);
        assert_eq!(s.in_flight(), 0);
    }

    #[test]
    fn ties_are_fifo() {
        // Ids are pushed in reverse key order so a tie broken by id would show up.
        let id = ids(5);
        let mut s = JobScheduler::new(8);
        for &i in id.iter().rev() {
            s.push(i, JobKind::Generate, 7);
        }
        let order: Vec<ChunkId> = drain(&mut s).into_iter().map(|j| j.0).collect();
        let expected: Vec<ChunkId> = id.iter().rev().copied().collect();
        assert_eq!(order, expected);
        // Kinds do not break ties either.
        s.push(id[0], JobKind::Mesh, 3);
        s.push(id[0], JobKind::Generate, 3);
        let kinds: Vec<JobKind> = drain(&mut s).into_iter().map(|j| j.1).collect();
        assert_eq!(kinds, vec![JobKind::Mesh, JobKind::Generate]);
    }

    #[test]
    fn in_flight_cap() {
        let id = ids(3);
        let mut s = JobScheduler::new(2);
        assert_eq!(s.max_in_flight(), 2);
        for &i in &id {
            s.push(i, JobKind::Generate, 1);
        }
        let first = s.pop_ready().expect("first job");
        let second = s.pop_ready().expect("second job");
        assert_eq!((first.seq, second.seq), (0, 1));
        assert!(s.pop_ready().is_none(), "cap reached");
        assert_eq!((s.in_flight(), s.queued()), (2, 1));
        s.complete();
        assert_eq!(s.pop_ready().map(|j| j.id), Some(id[2]));
        assert!(s.pop_ready().is_none(), "queue empty");
        s.complete();
        s.complete();
        assert_eq!(s.in_flight(), 0);
        s.complete();
        assert_eq!(s.in_flight(), 0, "complete saturates at zero");
        assert_eq!(JobScheduler::new(0).max_in_flight(), 1, "cap is at least 1");
    }

    #[test]
    fn reprioritize_reorders_and_drops() {
        let id = ids(4);
        let mut s = JobScheduler::new(8);
        s.push(id[0], JobKind::Generate, 1);
        s.push(id[1], JobKind::Generate, 2);
        s.push(id[2], JobKind::Mesh, 3);
        s.push(id[3], JobKind::Generate, 4);
        s.reprioritize(|i, kind| {
            if i == id[1] {
                None
            } else if kind == JobKind::Mesh || i == id[3] {
                Some(0)
            } else {
                Some(5)
            }
        });
        assert_eq!(s.queued(), 3);
        // id[2] and id[3] tie at 0: FIFO by the original insertion order.
        assert_eq!(
            drain(&mut s),
            vec![
                (id[2], JobKind::Mesh, 0),
                (id[3], JobKind::Generate, 0),
                (id[0], JobKind::Generate, 5),
            ]
        );
    }
}
