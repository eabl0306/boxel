use crate::{App, Plugin};
use boxel_tasks::{AsyncComputeTaskPool, ComputeTaskPool, IoTaskPool, TaskPoolBuilder};

/// Thread share for one pool: `clamp(total * percent, min, max)`.
#[derive(Clone, Debug)]
pub struct TaskPoolThreadAssignmentPolicy {
    /// Lower bound.
    pub min_threads: usize,
    /// Upper bound.
    pub max_threads: usize,
    /// Share of the total.
    pub percent: f32,
}

/// Sizes of the three global pools (`boxel_tasks`): `ComputeTaskPool` (ECS systems, `par_iter`),
/// `AsyncComputeTaskPool` (chunk generation/meshing jobs), `IoTaskPool` (persistence, files).
///
/// Defaults (frozen): the main thread also runs systems, so the worker total is
/// `available_parallelism - 1` (at least 1); io = 10% in [1, 2]; `async_compute` = 40% in [1, 8];
/// compute = the rest (at least 1). 16 hardware threads -> 15 workers: io 2, async 6, compute 7
/// (16 runnable threads with the main thread, before driver threads).
#[derive(Clone, Debug)]
pub struct TaskPoolOptions {
    /// Lower bound on the worker thread total.
    pub min_total_threads: usize,
    /// Upper bound on the worker thread total.
    pub max_total_threads: usize,
    /// IO pool policy.
    pub io: TaskPoolThreadAssignmentPolicy,
    /// Async compute (chunk jobs) policy.
    pub async_compute: TaskPoolThreadAssignmentPolicy,
    /// Compute (ECS) policy; takes the remainder.
    pub compute: TaskPoolThreadAssignmentPolicy,
}

impl Default for TaskPoolOptions {
    fn default() -> Self {
        Self {
            min_total_threads: 1,
            max_total_threads: usize::MAX,
            io: TaskPoolThreadAssignmentPolicy {
                min_threads: 1,
                max_threads: 2,
                percent: 0.10,
            },
            async_compute: TaskPoolThreadAssignmentPolicy {
                min_threads: 1,
                max_threads: 8,
                percent: 0.40,
            },
            compute: TaskPoolThreadAssignmentPolicy {
                min_threads: 1,
                max_threads: usize::MAX,
                percent: 1.0,
            },
        }
    }
}

impl TaskPoolThreadAssignmentPolicy {
    fn threads_for(&self, total: usize) -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "thread counts are small"
        )]
        let n = (total as f32 * self.percent).round() as usize;
        n.clamp(self.min_threads, self.max_threads.max(self.min_threads))
    }
}

impl TaskPoolOptions {
    /// Worker counts `(io, async_compute, compute)` for `hardware_threads` (pure; tested).
    pub fn split(&self, hardware_threads: usize) -> (usize, usize, usize) {
        let total = hardware_threads
            .saturating_sub(1)
            .clamp(self.min_total_threads.max(1), self.max_total_threads.max(1));
        let io = self.io.threads_for(total);
        let async_compute = self.async_compute.threads_for(total);
        let compute = total.saturating_sub(io + async_compute).clamp(
            self.compute.min_threads,
            self.compute.max_threads.max(self.compute.min_threads),
        );
        (io, async_compute, compute)
    }

    /// Initializes the three global pools if not yet initialized (idempotent). Thread names:
    /// `boxel-compute`, `boxel-async`, `boxel-io` (`boxel_tasks` appends the index).
    pub fn create_default_pools(&self) {
        let hw = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let (io, async_compute, compute) = self.split(hw);
        tracing::debug!(
            "task pools: io={io} async_compute={async_compute} compute={compute} (hw threads {hw})"
        );
        ComputeTaskPool::get_or_init(|| {
            TaskPoolBuilder::new()
                .num_threads(compute)
                .thread_name("boxel-compute".into())
                .build()
        });
        AsyncComputeTaskPool::get_or_init(|| {
            TaskPoolBuilder::new()
                .num_threads(async_compute)
                .thread_name("boxel-async".into())
                .build()
        });
        IoTaskPool::get_or_init(|| {
            TaskPoolBuilder::new()
                .num_threads(io)
                .thread_name("boxel-io".into())
                .build()
        });
    }
}

/// Creates the global task pools in `build` (must be added before anything that spawns tasks).
#[derive(Default)]
pub struct TaskPoolPlugin {
    /// Pool sizes.
    pub task_pool_options: TaskPoolOptions,
}

impl Plugin for TaskPoolPlugin {
    fn build(&self, _app: &mut App) {
        self.task_pool_options.create_default_pools();
    }
}
