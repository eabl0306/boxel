//! `TaskPoolPlugin` creates the global pools. One test per binary: the pools are process-global,
//! so this binary must not create them anywhere else first.
use boxel_app::prelude::*;
use boxel_tasks::{AsyncComputeTaskPool, ComputeTaskPool, IoTaskPool};

#[test]
fn task_pool_plugin_creates_the_global_pools() {
    assert!(ComputeTaskPool::try_get().is_none());
    let mut app = App::empty();
    app.add_plugins(TaskPoolPlugin::default());
    assert!(ComputeTaskPool::try_get().is_some());
    assert!(AsyncComputeTaskPool::try_get().is_some());
    assert!(IoTaskPool::try_get().is_some());

    let (io, async_compute, compute) = TaskPoolOptions::default()
        .split(std::thread::available_parallelism().map_or(1, std::num::NonZero::get));
    assert_eq!(ComputeTaskPool::get().thread_num(), compute);
    assert_eq!(AsyncComputeTaskPool::get().thread_num(), async_compute);
    assert_eq!(IoTaskPool::get().thread_num(), io);

    // Idempotent: a second app does not replace or resize the pools.
    let mut other = App::empty();
    other.add_plugins(TaskPoolPlugin {
        task_pool_options: TaskPoolOptions {
            min_total_threads: 64,
            ..TaskPoolOptions::default()
        },
    });
    assert_eq!(ComputeTaskPool::get().thread_num(), compute);
}
