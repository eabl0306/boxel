//! The main thread's side of saving (ADR 0024): the index of saved chunks, the chunks waiting
//! for a save job and the job itself. Owned by the world's job state, so the streaming code
//! stages a chunk as it unloads it and asks the index before generating one.
use super::store::{MetaValue, WorldStore};
use crate::jobs::JobExecutor;
use crate::{ChunkId, ChunkMap, DataState};
use boxel_voxel::{BlockPos, ChunkData, ChunkKey, ChunkPos, DimensionId, MAX_CHUNK_COORD};
use crossbeam_channel::{Receiver, Sender};
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A chunk column of the index: dimension and chunk x, z.
pub(crate) type ColumnKey = (DimensionId, i64, i64);

/// What a save job sends back (exactly once: see [`ReportGuard`]).
struct SaveReport {
    keys: Vec<ChunkKey>,
    meta: Vec<(String, MetaValue)>,
    result: Result<usize, String>,
}

/// Sends the job's report, or a failure from `Drop` if the job ends without one (the executor
/// dropped it, or it panicked outside the guarded write): the main thread never waits for a job
/// that is gone.
struct ReportGuard {
    tx: Option<Sender<SaveReport>>,
    keys: Vec<ChunkKey>,
    meta: Vec<(String, MetaValue)>,
}

impl ReportGuard {
    fn send(mut self, result: Result<usize, String>) {
        if let Some(tx) = self.tx.take() {
            let report = SaveReport {
                keys: std::mem::take(&mut self.keys),
                meta: std::mem::take(&mut self.meta),
                result,
            };
            // Err only if the world is gone.
            let _ = tx.send(report);
        }
    }
}

impl Drop for ReportGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(SaveReport {
                keys: std::mem::take(&mut self.keys),
                meta: std::mem::take(&mut self.meta),
                result: Err("save job dropped before it ran".to_string()),
            });
        }
    }
}

/// The world's saving state (see the [module docs](self)).
pub(crate) struct Persist {
    pub(crate) store: WorldStore,
    executor: Arc<dyn JobExecutor>,
    /// Saved (or staged) layers of each chunk column, ascending.
    saved: HashMap<ColumnKey, Vec<i64>, foldhash::fast::FixedState>,
    /// Staged chunks waiting for a save job, oldest first, and the same keys as a set.
    queue: VecDeque<ChunkKey>,
    queued: HashSet<ChunkKey, foldhash::fast::FixedState>,
    /// Metadata for the next save job (by name, the newest value).
    meta: Vec<(String, MetaValue)>,
    /// Loaded slots with edits that were not staged yet
    /// ([`ChunkSlot::dirty`](crate::ChunkSlot::dirty)).
    pub(crate) dirty: HashSet<ChunkId, foldhash::fast::FixedState>,
    /// A save job is running.
    in_flight: bool,
    tx: Sender<SaveReport>,
    rx: Receiver<SaveReport>,
    /// No save job starts before this (after a failure).
    retry_at: Option<Instant>,
    /// The last save failed (the next failures log quietly until one succeeds).
    failing: bool,
    /// The wait after a failure
    /// ([`PersistSettings::retry_after`](super::PersistSettings::retry_after), from the last
    /// frame).
    retry_after: Duration,
    /// When the last autosave staged the edited chunks.
    pub(crate) last_autosave: Instant,
    /// The pending ticks and the clock read when the world opened, until the startup restores
    /// them.
    pub(crate) restore: Option<(Option<Vec<(BlockPos, u64)>>, Option<u64>)>,
}

impl Persist {
    /// The saving state of `store`, whose save jobs run on `executor`.
    ///
    /// # Panics
    /// If `store` (or a clone of it) already served one: what it read when the world opened is
    /// gone, and starting without the index of saved chunks would save generated copies over
    /// them.
    pub(crate) fn new(store: WorldStore, executor: Arc<dyn JobExecutor>) -> Self {
        let opened = store.take_opened().expect(
            "a WorldStore serves one PersistPlugin: open the world again (WorldStore::open) for \
             a new App",
        );
        let mut saved: HashMap<ColumnKey, Vec<i64>, _> = HashMap::default();
        let inside = |c: i64| (-MAX_CHUNK_COORD..=MAX_CHUNK_COORD).contains(&c);
        for key in opened.keys {
            // The world never streams chunks past its limit (ADR 0014) or of another lod.
            if key.lod != 0 || ![key.pos.x, key.pos.y, key.pos.z].into_iter().all(inside) {
                tracing::warn!(?key, "a saved chunk outside the world is ignored");
                continue;
            }
            saved
                .entry((key.dim, key.pos.x, key.pos.z))
                .or_default()
                .push(key.pos.y);
        }
        for layers in saved.values_mut() {
            layers.sort_unstable();
            layers.dedup();
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            store,
            executor,
            saved,
            queue: VecDeque::new(),
            queued: HashSet::default(),
            meta: Vec::new(),
            dirty: HashSet::default(),
            in_flight: false,
            tx,
            rx,
            retry_at: None,
            failing: false,
            retry_after: Duration::from_secs(1),
            last_autosave: Instant::now(),
            restore: Some((opened.ticks, opened.clock)),
        }
    }

    /// True if chunk `key` has a saved (or staged) copy: it is loaded, not generated.
    pub(crate) fn is_saved(&self, key: ChunkKey) -> bool {
        key.lod == 0
            && self
                .saved
                .get(&(key.dim, key.pos.x, key.pos.z))
                .is_some_and(|ys| ys.binary_search(&key.pos.y).is_ok())
    }

    /// [`Persist::is_saved`] of the lod-0 chunk at `pos` of dimension `dim`.
    pub(crate) fn is_saved_at(&self, dim: DimensionId, pos: ChunkPos) -> bool {
        self.is_saved(ChunkKey { dim, lod: 0, pos })
    }

    /// The saved layers of chunk column `(x, z)` of `dim`, ascending.
    pub(crate) fn layers(&self, dim: DimensionId, x: i64, z: i64) -> &[i64] {
        self.saved.get(&(dim, x, z)).map_or(&[], Vec::as_slice)
    }

    /// Chunks staged and not handed to a save job yet.
    pub(crate) fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Hands chunk `key` (lod 0) with blocks `data` over for saving: loads read this copy from
    /// now on, and a save job writes it.
    pub(crate) fn stage(&mut self, key: ChunkKey, data: Arc<ChunkData>) {
        debug_assert_eq!(key.lod, 0, "only lod-0 chunks are saved");
        self.store.stage(key, data);
        let layers = self
            .saved
            .entry((key.dim, key.pos.x, key.pos.z))
            .or_default();
        if let Err(i) = layers.binary_search(&key.pos.y) {
            layers.insert(i, key.pos.y);
        }
        if self.queued.insert(key) {
            self.queue.push_back(key);
        }
    }

    /// Stages every loaded slot with edits ([`Persist::dirty`]); they are clean afterwards.
    pub(crate) fn stage_dirty(&mut self, map: &mut ChunkMap) {
        let mut ids: Vec<ChunkId> = self.dirty.drain().collect();
        // Staging order is the order of the keys in a save job: make it the map's, not the
        // hash set's.
        ids.sort_unstable();
        for id in ids {
            let Some(slot) = map.slots.get_mut(id) else {
                continue;
            };
            if !slot.dirty() {
                continue;
            }
            slot.set_dirty(false);
            if let DataState::Ready(data) = slot.data() {
                let (key, data) = (slot.key(), Arc::clone(data));
                self.stage(key, data);
            }
        }
    }

    /// Metadata waits for a save job: a failed job's, put back (one that just started took it).
    pub(crate) fn has_meta(&self) -> bool {
        !self.meta.is_empty()
    }

    /// Metadata for the next save (replaces an earlier value of the same name).
    pub(crate) fn set_meta(&mut self, name: String, value: MetaValue) {
        self.meta.retain(|(n, _)| *n != name);
        self.meta.push((name, value));
    }

    /// A frame's step: [`Persist::poll`], then [`Persist::start`].
    #[cfg(test)]
    pub(crate) fn frame(&mut self, now: Instant, max_batch: usize, retry_after: Duration) {
        self.poll(now, retry_after);
        self.start(now, max_batch);
    }

    /// Takes the report of the save job that finished. A failed save waits `retry_after` before
    /// the next try.
    pub(crate) fn poll(&mut self, now: Instant, retry_after: Duration) {
        self.retry_after = retry_after;
        while let Ok(report) = self.rx.try_recv() {
            self.on_report(report, now);
        }
    }

    /// A save job can start now: none runs, and no failure waits for its next try.
    pub(crate) fn can_start(&self, now: Instant) -> bool {
        !self.in_flight && !self.retry_at.is_some_and(|at| now < at)
    }

    /// The staged chunks the next job takes: at most `max_batch`, or every one when it writes
    /// metadata (the ticks and the clock are of the moment the chunks were staged up to: they go
    /// in the same transaction).
    fn batch_len(&self, max_batch: usize) -> usize {
        if self.meta.is_empty() {
            max_batch.max(1).min(self.queue.len())
        } else {
            self.queue.len()
        }
    }

    /// Starts the next save job if one can start and something waits (see
    /// [`Persist::batch_len`]).
    pub(crate) fn start(&mut self, now: Instant, max_batch: usize) {
        if !self.can_start(now) || (self.queue.is_empty() && self.meta.is_empty()) {
            return;
        }
        let n = self.batch_len(max_batch);
        let keys: Vec<ChunkKey> = self.queue.drain(..n).collect();
        for key in &keys {
            self.queued.remove(key);
        }
        let meta = std::mem::take(&mut self.meta);
        let guard = ReportGuard {
            tx: Some(self.tx.clone()),
            keys,
            meta,
        };
        let store = self.store.clone();
        self.in_flight = true;
        self.executor.spawn(Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(|| store.write(&guard.keys, &guard.meta)));
            guard.send(match result {
                Ok(Ok(n)) => Ok(n),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("save job panicked".to_string()),
            });
        }));
    }

    /// A save job finished: a failure puts its chunks and metadata back (in front: they are the
    /// oldest) and waits `retry_after` before the next try.
    fn on_report(&mut self, report: SaveReport, now: Instant) {
        self.in_flight = false;
        match report.result {
            Ok(_) => {
                if self.failing {
                    tracing::info!("world saves work again");
                }
                self.failing = false;
                self.retry_at = None;
            }
            Err(e) => {
                if self.failing {
                    tracing::debug!("world save failed again: {e}");
                } else {
                    tracing::error!(
                        "world save failed (the chunks stay in memory and are saved again): {e}"
                    );
                }
                self.failing = true;
                self.retry_at = Some(now + self.retry_after);
                for key in report.keys.into_iter().rev() {
                    if self.queued.insert(key) {
                        self.queue.push_front(key);
                    }
                }
                for (name, value) in report.meta {
                    if !self.meta.iter().any(|(n, _)| *n == name) {
                        self.meta.push((name, value));
                    }
                }
            }
        }
    }

    /// Waits for the running save job, if any, and takes its report (exit).
    pub(crate) fn wait(&mut self) {
        while self.in_flight {
            match self.rx.recv_timeout(Duration::from_secs(60)) {
                Ok(report) => self.on_report(report, Instant::now()),
                Err(_) => {
                    tracing::error!("a world save job did not report within 60 s");
                    self.in_flight = false;
                }
            }
        }
    }

    /// Writes everything staged and the metadata now, on this thread, in batches as save jobs
    /// take them (exit). Returns the first error; what failed stays in memory.
    pub(crate) fn save_now(&mut self, max_batch: usize) -> std::io::Result<()> {
        self.wait();
        let mut first_error = None;
        loop {
            let n = self.batch_len(max_batch);
            let keys: Vec<ChunkKey> = self.queue.drain(..n).collect();
            for key in &keys {
                self.queued.remove(key);
            }
            let meta = std::mem::take(&mut self.meta);
            if keys.is_empty() && meta.is_empty() {
                break;
            }
            if let Err(e) = self.store.write(&keys, &meta) {
                let text = e.to_string();
                first_error.get_or_insert(e);
                // Back in the queue: still staged, so loads keep reading them.
                self.on_report(
                    SaveReport {
                        keys,
                        meta,
                        result: Err(text),
                    },
                    Instant::now(),
                );
                self.retry_at = None;
                break;
            }
        }
        if let Err(e) = self.store.backend().flush() {
            first_error.get_or_insert(e);
        }
        first_error.map_or(Ok(()), Err)
    }
}
