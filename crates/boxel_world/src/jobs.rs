//! Where chunk jobs run.
use boxel_tasks::{AsyncComputeTaskPool, IoTaskPool, TaskPool};

/// Runs boxed jobs somewhere. Results come back through channels owned by the world.
///
/// Result contract (frozen, enforced by `boxel_world`, not by executors): every job the world
/// spawns sends EXACTLY ONE message back: `Generated`, `Meshed`, `Cancelled { id, kind }` (it
/// saw the cancel flag) or `Failed { id, kind, panic }`. The world wraps each job body in
/// `std::panic::catch_unwind(AssertUnwindSafe(..))` and holds an in-flight token whose `Drop`
/// sends `Failed` if nothing was sent, so a panicking generator/mesher or a dropped task can
/// never leak an in-flight slot (`boxel_tasks` workers swallow task panics and restart).
pub trait JobExecutor: Send + Sync + 'static {
    /// Starts `job` (must not block the caller for long).
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>);
}

/// Runs jobs on `boxel_tasks::AsyncComputeTaskPool` (detached tasks). Uses
/// `AsyncComputeTaskPool::get_or_init(TaskPool::default)`, so it also works without
/// `TaskPoolPlugin`.
#[derive(Clone, Copy, Debug, Default)]
pub struct TaskPoolExecutor;

impl JobExecutor for TaskPoolExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        AsyncComputeTaskPool::get_or_init(TaskPool::default)
            .spawn(async move { job() })
            .detach();
    }
}

/// Runs jobs on `boxel_tasks::IoTaskPool` (detached tasks): jobs that wait on the disk, such as
/// the world's saves (ADR 0024), so they never hold a chunk worker. Uses
/// `IoTaskPool::get_or_init(TaskPool::default)`, so it also works without `TaskPoolPlugin`.
#[derive(Clone, Copy, Debug, Default)]
pub struct IoTaskPoolExecutor;

impl JobExecutor for IoTaskPoolExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        IoTaskPool::get_or_init(TaskPool::default)
            .spawn(async move { job() })
            .detach();
    }
}

/// Runs jobs immediately on the calling thread (deterministic tests).
#[derive(Clone, Copy, Debug, Default)]
pub struct InlineExecutor;

impl JobExecutor for InlineExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        job();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn inline_executor_runs_immediately() {
        let (tx, rx) = crossbeam_channel::unbounded();
        InlineExecutor.spawn(Box::new(move || tx.send(7).expect("receiver alive")));
        assert_eq!(rx.try_recv(), Ok(7));
    }

    #[test]
    fn task_pool_executor_runs_jobs_off_thread() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let caller = std::thread::current().id();
        for i in 0..8 {
            let tx = tx.clone();
            TaskPoolExecutor.spawn(Box::new(move || {
                tx.send((i, std::thread::current().id()))
                    .expect("receiver alive");
            }));
        }
        let mut seen = Vec::new();
        for _ in 0..8 {
            let (i, thread) = rx
                .recv_timeout(Duration::from_secs(30))
                .expect("job result within 30 s");
            assert_ne!(thread, caller, "jobs run on the async compute pool");
            seen.push(i);
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..8).collect::<Vec<_>>());
    }
}
