//! LOD jobs (ADR 0031): one job builds one column of one level. It asks the generator's
//! [`LodSource`] for the column's layers, then for each node with cells fills a
//! [`PaddedChunk`], meshes it with the ordinary mesher (no light, no AO) and runs the LOD
//! post-passes ([`mesh_node`]).
//!
//! The jobs copy the world's result contract (`crate::jobs`) without sharing its state: every
//! spawned job sends EXACTLY ONE message, the body runs under `catch_unwind`, a token whose `Drop`
//! sends `Failed` stands for a job that never ran to the end, and a cancel flag is checked
//! before the source call and before each node. They run on the plugin's own executor (the
//! world's is private to its jobs), by default the same async compute pool.
//!
//! The queue is a max-heap by priority class (0 first) and then by screen size; entries carry a
//! ticket so a column queued again or dropped leaves its old entries to be skipped when popped.
use crate::jobs::JobExecutor;
use boxel_mesh::lod::{LodMaterials, fits, mesh_node};
use boxel_mesh::{ChunkMesh, Mesher, PaddedChunk};
use boxel_voxel::BlockRegistry;
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{LodColumn, LodNode, LodRequest, LodScratch};
use crossbeam_channel::{Receiver, Sender};
use std::cell::RefCell;
use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::plan::ColumnKey;

/// Nodes with cells a job meshes at most; the column's other nodes go to a second job.
pub(crate) const MAX_NODES_PER_JOB: usize = 3;

/// What a job builds: one column of one level.
pub(crate) struct JobInput {
    /// Unique per spawned job: a result counts only while its column still waits for this id.
    pub(crate) id: u64,
    pub(crate) generator: Arc<dyn WorldGenerator>,
    pub(crate) registry: Arc<BlockRegistry>,
    pub(crate) materials: Arc<LodMaterials>,
    pub(crate) request: LodRequest,
    /// Mesh only these layers (a second job for the nodes a first one left); `None`: all.
    pub(crate) only: Option<Vec<i64>>,
    pub(crate) cancel: Arc<AtomicBool>,
}

/// One node of a finished job.
#[derive(Clone, Debug)]
pub(crate) enum Built {
    /// The source emitted it uniform: nothing to draw.
    Uniform,
    /// Its cells meshed to nothing.
    Blank,
    /// Its mesh.
    Mesh(Arc<ChunkMesh>),
    /// A face bucket passed the renderer's limit: the node is lost (its parent draws there).
    TooBig,
}

/// The single message of a job.
#[derive(Debug)]
pub(crate) enum LodMessage {
    /// The column's band and the nodes the job built. Nodes with cells past
    /// [`MAX_NODES_PER_JOB`] are left out: they stay pending for another job.
    Built {
        id: u64,
        column: LodColumn,
        nodes: Vec<(i64, Built)>,
    },
    /// The generator has no LOD source.
    NoSource { id: u64 },
    /// The job saw its cancel flag.
    Cancelled { id: u64 },
    /// The job panicked or was dropped.
    Failed { id: u64, panic: String },
}

impl LodMessage {
    /// The job's id.
    pub(crate) fn id(&self) -> u64 {
        match *self {
            LodMessage::Built { id, .. }
            | LodMessage::NoSource { id }
            | LodMessage::Cancelled { id }
            | LodMessage::Failed { id, .. } => id,
        }
    }
}

/// A message and how long the job body ran.
pub(crate) struct LodReport {
    pub(crate) message: LodMessage,
    pub(crate) busy: Duration,
}

/// Owned by every spawned job: sends its single message, or `Failed` from `Drop`.
struct ResultToken {
    id: u64,
    tx: Option<Sender<LodReport>>,
}

impl ResultToken {
    fn send(mut self, message: LodMessage, busy: Duration) {
        if let Some(tx) = self.tx.take() {
            // Err only if the LOD (receiver) is gone: nobody waits for the result.
            let _ = tx.send(LodReport { message, busy });
        }
    }
}

impl Drop for ResultToken {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(LodReport {
                message: LodMessage::Failed {
                    id: self.id,
                    panic: "LOD job dropped before sending a result".to_string(),
                },
                busy: Duration::ZERO,
            });
        }
    }
}

/// A worker's scratch, reused from job to job on one thread.
#[derive(Default)]
struct Scratch {
    source: LodScratch,
    padded: PaddedChunk,
    mesher: Mesher,
    mesh: ChunkMesh,
}

thread_local! {
    /// One scratch per worker thread (cells, padded grid, mesher: about 0.8 MiB).
    static SCRATCH: RefCell<Option<Box<Scratch>>> = const { RefCell::new(None) };
}

/// Text of a panic payload.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// The column job (module docs).
pub(crate) fn run_column(input: &JobInput) -> LodMessage {
    let id = input.id;
    let Some(source) = input.generator.lod() else {
        return LodMessage::NoSource { id };
    };
    SCRATCH.with(|cell| {
        let mut slot = cell.borrow_mut();
        let s = slot.get_or_insert_with(Box::default);
        let Scratch {
            source: scratch,
            padded,
            mesher,
            mesh,
        } = &mut **s;
        let level = input.request.level;
        let mut nodes = Vec::new();
        let mut meshed = 0;
        let mut cancelled = false;
        let column = source.column(
            &input.request,
            &input.registry,
            scratch,
            &mut |layer, node| {
                if cancelled
                    || input
                        .only
                        .as_ref()
                        .is_some_and(|only| !only.contains(&layer))
                {
                    return;
                }
                match node {
                    LodNode::Uniform(_) => nodes.push((layer, Built::Uniform)),
                    LodNode::Cells(cells) => {
                        if input.cancel.load(Ordering::Relaxed) {
                            cancelled = true;
                            return;
                        }
                        if meshed == MAX_NODES_PER_JOB {
                            return;
                        }
                        meshed += 1;
                        input.materials.fill(&cells.0[..], padded);
                        mesh_node(mesher, padded, &input.materials, level, mesh);
                        let built = if mesh.is_empty() {
                            Built::Blank
                        } else if !fits(mesh) {
                            Built::TooBig
                        } else {
                            Built::Mesh(Arc::new(mesh.clone()))
                        };
                        nodes.push((layer, built));
                    }
                }
            },
        );
        if cancelled {
            LodMessage::Cancelled { id }
        } else {
            LodMessage::Built { id, column, nodes }
        }
    })
}

/// A queued column: class (0 first), then screen size (larger first), then the order queued.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Queued {
    pub(crate) class: u8,
    pub(crate) size: f32,
    pub(crate) ticket: u32,
    pub(crate) column: ColumnKey,
}

impl Queued {
    fn rank(&self) -> (std::cmp::Reverse<u8>, u32, std::cmp::Reverse<u32>) {
        // Sizes are positive: their bits order like the floats.
        (
            std::cmp::Reverse(self.class),
            self.size.max(0.0).to_bits(),
            std::cmp::Reverse(self.ticket),
        )
    }
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        self.rank() == other.rank()
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.rank().cmp(&other.rank())
    }
}

/// Job times in 10-microsecond buckets up to 40 ms (the last bucket holds the rest), for the
/// p50 and p99 of [`super::LodStats`].
#[derive(Clone, Debug)]
pub(crate) struct JobTimes {
    buckets: Vec<u32>,
    count: u64,
}

impl Default for JobTimes {
    fn default() -> Self {
        Self {
            buckets: vec![0; 4001],
            count: 0,
        }
    }
}

impl JobTimes {
    const BUCKET_US: u128 = 10;

    pub(crate) fn record(&mut self, busy: Duration) {
        let b = (busy.as_micros() / Self::BUCKET_US).min(self.buckets.len() as u128 - 1);
        self.buckets[b as usize] += 1;
        self.count += 1;
    }

    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    /// The upper edge of the bucket holding fraction `q` of the jobs (zero without jobs).
    pub(crate) fn quantile(&self, q: f64) -> Duration {
        if self.count == 0 {
            return Duration::ZERO;
        }
        let want = ((self.count as f64) * q).ceil().max(1.0) as u64;
        let mut seen = 0u64;
        for (i, &n) in self.buckets.iter().enumerate() {
            seen += u64::from(n);
            if seen >= want {
                return Duration::from_micros(((i as u128 + 1) * Self::BUCKET_US) as u64);
            }
        }
        Duration::from_micros((self.buckets.len() as u128 * Self::BUCKET_US) as u64)
    }
}

/// The LOD's jobs: executor, result channel, queue and counters.
pub(crate) struct LodJobs {
    executor: Arc<dyn JobExecutor>,
    tx: Sender<LodReport>,
    rx: Receiver<LodReport>,
    next_id: u64,
    next_ticket: u32,
    pub(crate) queue: BinaryHeap<Queued>,
    /// Jobs spawned whose message has not been received.
    pub(crate) in_flight: usize,
    pub(crate) times: JobTimes,
    /// Jobs that sent `Failed` (a panic or a dropped job), since start.
    pub(crate) failed: usize,
    /// Jobs that sent `Cancelled`, since start.
    pub(crate) cancelled: usize,
}

impl LodJobs {
    pub(crate) fn new(executor: Arc<dyn JobExecutor>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            executor,
            tx,
            rx,
            next_id: 1,
            next_ticket: 0,
            queue: BinaryHeap::new(),
            in_flight: 0,
            times: JobTimes::default(),
            failed: 0,
            cancelled: 0,
        }
    }

    /// A fresh job id.
    pub(crate) fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Queues `column` and returns the ticket the entry carries.
    pub(crate) fn push(&mut self, column: ColumnKey, class: u8, size: f32) -> u32 {
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.queue.push(Queued {
            class,
            size,
            ticket,
            column,
        });
        ticket
    }

    /// Spawns `input` under the result contract (module docs).
    pub(crate) fn spawn(&mut self, input: JobInput) {
        let token = ResultToken {
            id: input.id,
            tx: Some(self.tx.clone()),
        };
        self.in_flight += 1;
        self.executor.spawn(Box::new(move || {
            let id = input.id;
            if input.cancel.load(Ordering::Relaxed) {
                token.send(LodMessage::Cancelled { id }, Duration::ZERO);
                return;
            }
            let start = Instant::now();
            let message = match catch_unwind(AssertUnwindSafe(|| run_column(&input))) {
                Ok(message) => message,
                Err(payload) => LodMessage::Failed {
                    id,
                    panic: panic_text(payload.as_ref()),
                },
            };
            token.send(message, start.elapsed());
        }));
    }

    /// True if a finished job's report is waiting.
    pub(crate) fn has_results(&self) -> bool {
        !self.rx.is_empty()
    }

    /// The next finished job's report, if any (counts it out of flight).
    pub(crate) fn try_recv(&mut self) -> Option<LodReport> {
        let report = self.rx.try_recv().ok()?;
        self.in_flight = self.in_flight.saturating_sub(1);
        match report.message {
            LodMessage::Failed { .. } => self.failed += 1,
            LodMessage::Cancelled { .. } => self.cancelled += 1,
            LodMessage::Built { .. } => self.times.record(report.busy),
            LodMessage::NoSource { .. } => {}
        }
        Some(report)
    }
}
