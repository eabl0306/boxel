//! Where chunk jobs run.

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
        todo!("WP6")
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
