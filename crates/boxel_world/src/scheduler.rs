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
///
/// Priorities depend on where the viewers are, so they go stale when a viewer moves. Instead of
/// rebuilding the whole heap in one frame, [`JobScheduler::invalidate_priorities`] moves every
/// queued job to a stale heap (O(1) when nothing was stale yet) and
/// [`JobScheduler::refresh_priorities`] recomputes them a batch at a time, nearest under the old
/// priorities first. [`JobScheduler::pop_ready`] takes refreshed jobs first and falls back to the
/// stale ones.
#[derive(Debug, Default)]
pub struct JobScheduler {
    heap: BinaryHeap<Reverse<(u32, u64, ChunkId, u8)>>,
    stale: BinaryHeap<Reverse<(u32, u64, ChunkId, u8)>>,
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
    /// Pops the best job if below the in-flight cap (and counts it as in flight): refreshed jobs
    /// first, then stale ones (in their old order).
    pub fn pop_ready(&mut self) -> Option<QueuedJob> {
        if self.in_flight >= self.max_in_flight {
            return None;
        }
        let Reverse((priority, seq, id, code)) = match self.heap.pop() {
            Some(job) => job,
            None => self.stale.pop()?,
        };
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
    /// Marks every queued priority stale (the viewers changed); see
    /// [`JobScheduler::refresh_priorities`].
    pub fn invalidate_priorities(&mut self) {
        if self.stale.is_empty() {
            std::mem::swap(&mut self.heap, &mut self.stale);
        } else {
            self.stale.append(&mut self.heap);
        }
    }
    /// Recomputes up to `max` stale priorities with `f` (`None` drops the job), nearest under the
    /// old priorities first. Insertion order (`seq`) is kept, so ties stay FIFO. Returns how many
    /// were processed.
    pub fn refresh_priorities(
        &mut self,
        max: usize,
        mut f: impl FnMut(ChunkId, JobKind) -> Option<u32>,
    ) -> usize {
        let mut done = 0;
        while done < max {
            let Some(Reverse((_, seq, id, code))) = self.stale.pop() else {
                break;
            };
            if let Some(priority) = f(id, JobKind::from_code(code)) {
                self.heap.push(Reverse((priority, seq, id, code)));
            }
            done += 1;
        }
        done
    }
    /// Rebuilds every priority at once (`f` returns `None` to drop a job). Insertion order
    /// (`seq`) is kept, so ties stay FIFO.
    pub fn reprioritize(&mut self, f: impl FnMut(ChunkId, JobKind) -> Option<u32>) {
        self.invalidate_priorities();
        self.refresh_priorities(usize::MAX, f);
    }
    /// Jobs waiting (refreshed and stale).
    pub fn queued(&self) -> usize {
        self.heap.len() + self.stale.len()
    }
    /// Jobs waiting whose priority is stale.
    pub fn stale(&self) -> usize {
        self.stale.len()
    }
    /// Jobs running.
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }
    /// Cap.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }
    /// Changes the cap (>= 1). Jobs already in flight above a lower cap finish normally.
    pub fn set_max_in_flight(&mut self, max_in_flight: usize) {
        self.max_in_flight = max_in_flight.max(1);
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

    #[test]
    fn stale_priorities_refresh_in_batches() {
        let id = ids(5);
        let mut s = JobScheduler::new(8);
        for (i, &chunk) in id.iter().enumerate() {
            s.push(chunk, JobKind::Generate, i as u32);
        }
        s.invalidate_priorities();
        assert_eq!((s.queued(), s.stale()), (5, 5));
        // New jobs go to the refreshed heap.
        let extra = ids(6)[5];
        s.push(extra, JobKind::Mesh, 50);
        // Batch of 2, old order: id[0] and id[1] get new priorities (reversed), id[1] is dropped.
        let n = s.refresh_priorities(2, |i, _| (i != id[1]).then_some(100));
        assert_eq!((n, s.queued(), s.stale()), (2, 5, 3));
        // Refreshed jobs pop first (extra at 50, id[0] at 100), then the stale ones in old order.
        let order: Vec<(ChunkId, u32)> = drain(&mut s).into_iter().map(|j| (j.0, j.2)).collect();
        assert_eq!(
            order,
            vec![
                (extra, 50),
                (id[0], 100),
                (id[2], 2),
                (id[3], 3),
                (id[4], 4)
            ]
        );
        // A second invalidation while jobs are still stale merges both heaps.
        s.push(id[0], JobKind::Generate, 1);
        s.invalidate_priorities();
        s.push(id[1], JobKind::Generate, 2);
        s.invalidate_priorities();
        assert_eq!(s.stale(), 2);
        assert_eq!(s.refresh_priorities(usize::MAX, |_, _| Some(0)), 2);
        assert_eq!(s.stale(), 0);
        // FIFO among equal refreshed priorities.
        let order: Vec<ChunkId> = drain(&mut s).into_iter().map(|j| j.0).collect();
        assert_eq!(order, vec![id[0], id[1]]);
    }

    #[test]
    fn cap_can_change() {
        let id = ids(3);
        let mut s = JobScheduler::new(1);
        for &i in &id {
            s.push(i, JobKind::Generate, 1);
        }
        assert!(s.pop_ready().is_some());
        assert!(s.pop_ready().is_none());
        s.set_max_in_flight(3);
        assert!(s.pop_ready().is_some());
        assert!(s.pop_ready().is_some());
        s.set_max_in_flight(0);
        assert_eq!(s.max_in_flight(), 1);
        assert_eq!(s.in_flight(), 3, "running jobs are not affected");
    }
}
