//! Priority job queue with a bounded number of jobs in flight (neither rayon nor `boxel_tasks`
//! have priorities).
use crate::{ChunkId, ColumnId};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Job kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// Generate data.
    Generate,
    /// Build a mesh (and the chunk's light, ADR 0015).
    Mesh,
    /// Walk a chunk column down for its sky tops (ADR 0015).
    SkyTile,
}

impl JobKind {
    /// Heap encoding (keeps the heap entry `Ord` without deriving it on the public enum).
    const fn code(self) -> u8 {
        match self {
            JobKind::Generate => 0,
            JobKind::Mesh => 1,
            JobKind::SkyTile => 2,
        }
    }
    const fn from_code(code: u8) -> Self {
        match code {
            0 => JobKind::Generate,
            1 => JobKind::Mesh,
            _ => JobKind::SkyTile,
        }
    }
}

/// What a job works on: a chunk slot (generation, meshing) or a chunk column (sky tops).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum JobId {
    /// A chunk slot of the `ChunkMap`.
    Chunk(ChunkId),
    /// A chunk column of the `SkyMap`.
    Column(ColumnId),
}

impl From<ChunkId> for JobId {
    fn from(id: ChunkId) -> Self {
        JobId::Chunk(id)
    }
}

impl From<ColumnId> for JobId {
    fn from(id: ColumnId) -> Self {
        JobId::Column(id)
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
    /// Target chunk or column.
    pub id: JobId,
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
/// priorities first.
///
/// Dispatch order stays nearest first while priorities are stale: the caller says how many chunks
/// closer to a viewer any job may have come (the drift), which gives each stale job a lowest
/// possible priority. [`JobScheduler::refresh_ahead`] refreshes the stale jobs whose lowest
/// possible priority is below the best refreshed one, and [`JobScheduler::pop_ready`] only pops
/// a job when no stale job may have to run first.
#[derive(Debug, Default)]
pub struct JobScheduler {
    heap: BinaryHeap<Reverse<(u32, u64, JobId, u8)>>,
    stale: BinaryHeap<Reverse<(u32, u64, JobId, u8)>>,
    /// Chunks by which the distance of a stale job to its nearest viewer may have shrunk since its
    /// priority was computed (`u32::MAX`: unknown).
    drift: u32,
    next_seq: u64,
    in_flight: usize,
    max_in_flight: usize,
}

/// The lowest priority a job of stale priority `priority` can have once its distance shrank by at
/// most `drift` chunks (priorities are squared distances; edit remeshes use 0, which stays 0).
pub(crate) const fn lowest_priority(priority: u32, drift: u32) -> u32 {
    // `isqrt(p) <= sqrt(p)`, so the result never exceeds the real lowest value. It is at most
    // `isqrt(u32::MAX)^2`, which fits.
    let near = priority.isqrt().saturating_sub(drift);
    near * near
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
    pub fn push(&mut self, id: impl Into<JobId>, kind: JobKind, priority: u32) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.heap
            .push(Reverse((priority, seq, id.into(), kind.code())));
    }
    /// Pops the best refreshed job if below the in-flight cap (and counts it as in flight).
    /// Returns `None` while a stale job may have to run before it (or only stale jobs are left):
    /// [`JobScheduler::refresh_ahead`] refreshes those first.
    pub fn pop_ready(&mut self) -> Option<QueuedJob> {
        if self.in_flight >= self.max_in_flight || self.stale_may_come_first() {
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
    /// Marks every queued priority stale (the viewers changed): `drift` is how many chunks closer
    /// to its nearest viewer any position may have come (`u32::MAX` when unknown, for example
    /// when a viewer was added). Drifts add up until every stale job has been refreshed. See
    /// [`JobScheduler::refresh_priorities`] and [`JobScheduler::refresh_ahead`].
    pub fn invalidate_priorities(&mut self, drift: u32) {
        if self.stale.is_empty() {
            std::mem::swap(&mut self.heap, &mut self.stale);
            self.drift = drift;
        } else {
            self.stale.append(&mut self.heap);
            self.drift = self.drift.saturating_add(drift);
        }
    }
    /// Recomputes up to `max` stale priorities with `f` (`None` drops the job), nearest under the
    /// old priorities first. Insertion order (`seq`) is kept, so ties stay FIFO. Returns how many
    /// were processed.
    pub fn refresh_priorities(
        &mut self,
        max: usize,
        mut f: impl FnMut(JobId, JobKind) -> Option<u32>,
    ) -> usize {
        let mut done = 0;
        while done < max && self.refresh_one(&mut f) {
            done += 1;
        }
        done
    }
    /// Recomputes, like [`JobScheduler::refresh_priorities`], up to `max` stale jobs that may have
    /// to run before the best refreshed job: their stale priority lowered by the drift is below
    /// it, or no refreshed job is left. Returns how many were processed; once it returns 0,
    /// [`JobScheduler::pop_ready`] pops the job that runs next.
    pub fn refresh_ahead(
        &mut self,
        max: usize,
        mut f: impl FnMut(JobId, JobKind) -> Option<u32>,
    ) -> usize {
        let mut done = 0;
        while done < max && self.stale_may_come_first() && self.refresh_one(&mut f) {
            done += 1;
        }
        done
    }
    /// Rebuilds every priority at once (`f` returns `None` to drop a job). Insertion order
    /// (`seq`) is kept, so ties stay FIFO.
    pub fn reprioritize(&mut self, f: impl FnMut(JobId, JobKind) -> Option<u32>) {
        self.invalidate_priorities(u32::MAX);
        self.refresh_priorities(usize::MAX, f);
    }
    /// True if the best stale job may have to run before the best refreshed one.
    fn stale_may_come_first(&self) -> bool {
        let Some(Reverse((stale, ..))) = self.stale.peek() else {
            return false;
        };
        self.heap
            .peek()
            .is_none_or(|Reverse((best, ..))| lowest_priority(*stale, self.drift) < *best)
    }
    /// Refreshes the nearest stale job under the old priorities. False if none is stale.
    fn refresh_one(&mut self, f: &mut impl FnMut(JobId, JobKind) -> Option<u32>) -> bool {
        let Some(Reverse((_, seq, id, code))) = self.stale.pop() else {
            return false;
        };
        if let Some(priority) = f(id, JobKind::from_code(code)) {
            self.heap.push(Reverse((priority, seq, id, code)));
        }
        true
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
    /// True if the in-flight cap is reached.
    pub fn is_full(&self) -> bool {
        self.in_flight >= self.max_in_flight
    }
    /// Changes the cap (>= 1). Jobs already in flight above a lower cap finish normally.
    pub fn set_max_in_flight(&mut self, max_in_flight: usize) {
        self.max_in_flight = max_in_flight.max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<JobId> {
        let mut slots = slotmap::SlotMap::<ChunkId, ()>::with_key();
        (0..n).map(|_| JobId::Chunk(slots.insert(()))).collect()
    }

    /// Pops every job, refreshing the stale ones that may come first with `f`.
    fn drain_with(
        s: &mut JobScheduler,
        mut f: impl FnMut(JobId, JobKind) -> Option<u32>,
    ) -> Vec<(JobId, JobKind, u32)> {
        let mut out = Vec::new();
        loop {
            if s.refresh_ahead(usize::MAX, &mut f) > 0 {
                continue;
            }
            let Some(job) = s.pop_ready() else {
                break;
            };
            out.push((job.id, job.kind, job.priority));
            s.complete();
        }
        out
    }

    /// Pops every job of a queue without stale priorities.
    fn drain(s: &mut JobScheduler) -> Vec<(JobId, JobKind, u32)> {
        drain_with(s, |_, _| unreachable!("nothing is stale"))
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
        let order: Vec<JobId> = drain(&mut s).into_iter().map(|j| j.0).collect();
        let expected: Vec<JobId> = id.iter().rev().copied().collect();
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
        let id = ids(6);
        let mut s = JobScheduler::new(8);
        for (i, &chunk) in id[..5].iter().enumerate() {
            s.push(chunk, JobKind::Generate, i as u32);
        }
        s.invalidate_priorities(0);
        assert_eq!((s.queued(), s.stale()), (5, 5));
        // New jobs go to the refreshed heap.
        s.push(id[5], JobKind::Mesh, 50);
        // Batch of 2, old order: id[0] and id[1] get new priorities, id[1] is dropped.
        let n = s.refresh_priorities(2, |i, _| (i != id[1]).then_some(100));
        assert_eq!((n, s.queued(), s.stale()), (2, 5, 3));
        // With no drift the stale ones keep their order ahead of the refreshed ones (they are
        // refreshed on the way, here to the same priority).
        let order: Vec<(JobId, u32)> = drain_with(&mut s, |i, _| {
            Some(id.iter().position(|&j| j == i).expect("known") as u32)
        })
        .into_iter()
        .map(|j| (j.0, j.2))
        .collect();
        assert_eq!(
            order,
            vec![
                (id[2], 2),
                (id[3], 3),
                (id[4], 4),
                (id[5], 50),
                (id[0], 100)
            ]
        );
        // A second invalidation while jobs are still stale merges both heaps.
        s.push(id[0], JobKind::Generate, 1);
        s.invalidate_priorities(0);
        s.push(id[1], JobKind::Generate, 2);
        s.invalidate_priorities(0);
        assert_eq!(s.stale(), 2);
        assert_eq!(s.refresh_priorities(usize::MAX, |_, _| Some(0)), 2);
        assert_eq!(s.stale(), 0);
        // FIFO among equal refreshed priorities.
        let order: Vec<JobId> = drain(&mut s).into_iter().map(|j| j.0).collect();
        assert_eq!(order, vec![id[0], id[1]]);
    }

    /// Jobs on a line at x = 0, 1, 2, ... with a viewer at x = 0 (priority x^2), which then moves
    /// to x = 1: only a few stale priorities are refreshed before far jobs are queued with fresh
    /// ones, as after a one-chunk move. Dispatch must still go nearest first.
    #[test]
    fn dispatch_stays_nearest_first_while_priorities_are_stale() {
        let id = ids(60);
        let x_of = |c: JobId| id.iter().position(|&i| i == c).expect("known") as u32;
        let new_priority = |c: JobId| x_of(c).abs_diff(1).pow(2);
        let mut s = JobScheduler::new(20);
        for (x, &chunk) in id[..40].iter().enumerate() {
            s.push(chunk, JobKind::Generate, (x as u32).pow(2));
        }
        // Moved by one chunk.
        s.invalidate_priorities(1);
        assert_eq!(s.refresh_priorities(4, |c, _| Some(new_priority(c))), 4);
        for &chunk in &id[40..] {
            s.push(chunk, JobKind::Generate, new_priority(chunk));
        }
        assert!(s.pop_ready().is_some(), "the refreshed x = 1 runs first");
        s.complete();
        let mut started = vec![new_priority(id[1])];
        while !s.is_full() {
            if s.refresh_ahead(1, |c, _| Some(new_priority(c))) > 0 {
                continue;
            }
            let job = s.pop_ready().expect("jobs left");
            assert_eq!(job.priority, new_priority(job.id), "popped fresh");
            started.push(job.priority);
        }
        assert_eq!(
            started.len(),
            21,
            "20 running and the first one, which completed"
        );
        assert!(started.is_sorted(), "{started:?}");
        // Every waiting job is at least as far as every started one.
        let farthest_started = *started.last().expect("started");
        s.set_max_in_flight(usize::MAX);
        let waiting = drain_with(&mut s, |c, _| Some(new_priority(c)));
        assert_eq!(waiting.len(), 60 - started.len());
        assert!(waiting.iter().all(|j| j.2 >= farthest_started));
        // The far stale jobs were only refreshed when they came up.
        assert!(waiting.iter().map(|j| j.2).is_sorted());
    }

    #[test]
    fn stale_jobs_are_refreshed_only_when_they_may_come_first() {
        let id = ids(3);
        let mut s = JobScheduler::new(8);
        // Old priority 36 (distance 6); two moves of 1 and 2 chunks: distance 3 at best.
        s.push(id[0], JobKind::Generate, 36);
        s.invalidate_priorities(1);
        s.push(id[1], JobKind::Generate, 50);
        s.invalidate_priorities(2);
        assert_eq!(lowest_priority(36, 3), 9);
        s.push(id[2], JobKind::Mesh, 10);
        assert!(s.pop_ready().is_none(), "the stale job may be at 9");
        assert_eq!(s.refresh_ahead(usize::MAX, |_, _| Some(9)), 1);
        assert_eq!(s.stale(), 1, "id[1] (16 at best) waits");
        assert_eq!(s.pop_ready().map(|j| (j.id, j.priority)), Some((id[0], 9)));
        assert_eq!(s.pop_ready().map(|j| (j.id, j.priority)), Some((id[2], 10)));
        // Only stale jobs left: refreshed one by one.
        assert!(s.pop_ready().is_none());
        assert_eq!(s.refresh_ahead(usize::MAX, |_, _| Some(4)), 1);
        assert_eq!(s.pop_ready().map(|j| (j.id, j.priority)), Some((id[1], 4)));
        assert_eq!(s.refresh_ahead(usize::MAX, |_, _| Some(0)), 0);

        // The drift starts over once nothing is stale: no move, no refresh ahead.
        s.push(id[0], JobKind::Generate, 36);
        s.invalidate_priorities(0);
        s.push(id[1], JobKind::Generate, 25);
        assert_eq!(s.refresh_ahead(usize::MAX, |_, _| unreachable!()), 0);
        assert_eq!(s.pop_ready().map(|j| j.id), Some(id[1]));
        // Unknown drift (a new viewer): every stale job may come first.
        s.push(id[2], JobKind::Generate, 1);
        s.invalidate_priorities(u32::MAX);
        s.push(id[1], JobKind::Generate, 1);
        let new = |c: JobId| Some(if c == id[0] { 0 } else { 3 });
        assert_eq!(s.refresh_ahead(usize::MAX, |c, _| new(c)), 2);
        let order: Vec<JobId> = drain(&mut s).into_iter().map(|j| j.0).collect();
        assert_eq!(order, vec![id[0], id[1], id[2]]);
    }

    #[test]
    fn lowest_priority_bounds_the_new_distance() {
        assert_eq!(lowest_priority(0, 0), 0);
        assert_eq!(lowest_priority(36, 0), 36);
        assert_eq!(lowest_priority(37, 0), 36, "rounded down");
        assert_eq!(lowest_priority(36, 6), 0);
        assert_eq!(lowest_priority(36, u32::MAX), 0);
        assert_eq!(lowest_priority(u32::MAX, 0), 65_535 * 65_535);
        // Exhaustive on a grid: moving the viewer by d chunks never gives a priority below the
        // bound of its ceiling.
        for (x, y) in (-12i32..=12).flat_map(|x| (-12i32..=12).map(move |y| (x, y))) {
            let old = (x * x + y * y).unsigned_abs();
            for (mx, my) in [(1, 0), (1, 1), (3, -4), (-7, 2)] {
                let (nx, ny) = (x - mx, y - my);
                let new = (nx * nx + ny * ny).unsigned_abs();
                let d2 = (mx * mx + my * my).unsigned_abs();
                let drift = d2.isqrt() + u32::from(d2.isqrt().pow(2) < d2);
                assert!(lowest_priority(old, drift) <= new, "{x},{y} by {mx},{my}");
            }
        }
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
        assert!(s.is_full());
        s.set_max_in_flight(0);
        assert_eq!(s.max_in_flight(), 1);
        assert_eq!(s.in_flight(), 3, "running jobs are not affected");
    }
}
