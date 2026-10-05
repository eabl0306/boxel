//! A headless app (`TaskPoolPlugin + TimePlugin + ScheduleRunnerPlugin::run_loop`) runs, uses the
//! pools, advances time, runs fixed steps and exits on its own.
use boxel_app::prelude::*;
use boxel_ecs::prelude::*;
use boxel_tasks::{AsyncComputeTaskPool, ComputeTaskPool};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

#[derive(Resource, Default)]
struct Frames(u32);

#[test]
fn headless_app_runs_and_exits() {
    let frame_count = Arc::new(AtomicU64::new(0));
    let fixed_steps = Arc::new(AtomicU32::new(0));
    let pool_sum = Arc::new(AtomicU32::new(0));
    let shutdowns = Arc::new(AtomicU32::new(0));

    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        TimePlugin,
        ScheduleRunnerPlugin::run_loop(Duration::from_millis(2)),
    ))
    .init_resource::<Frames>()
    // Shrink the fixed step so a short run sees several steps.
    .add_systems(Startup, |mut fixed: ResMut<FixedTime>| {
        fixed.set_timestep(Duration::from_millis(1));
    })
    .add_systems(Update, {
        let frame_count = frame_count.clone();
        let pool_sum = pool_sum.clone();
        move |time: Res<Time>, mut frames: ResMut<Frames>, mut exit: MessageWriter<AppExit>| {
            frames.0 += 1;
            frame_count.store(time.frame_count(), Ordering::SeqCst);
            let parts = ComputeTaskPool::get().scope(|s| {
                for i in 1..=4u32 {
                    s.spawn(async move { i });
                }
            });
            pool_sum.fetch_add(parts.iter().sum::<u32>(), Ordering::SeqCst);
            if frames.0 == 20 {
                exit.write(AppExit::Success);
            }
        }
    })
    .add_systems(FixedUpdate, {
        let fixed_steps = fixed_steps.clone();
        move || {
            fixed_steps.fetch_add(1, Ordering::SeqCst);
        }
    })
    .add_systems(Shutdown, {
        let shutdowns = shutdowns.clone();
        move || {
            shutdowns.fetch_add(1, Ordering::SeqCst);
        }
    });

    assert_eq!(app.run(), AppExit::Success);
    assert!(AsyncComputeTaskPool::try_get().is_some());
    assert_eq!(frame_count.load(Ordering::SeqCst), 20);
    assert_eq!(pool_sum.load(Ordering::SeqCst), 20 * 10);
    // 19 paced frames of at least 2 ms with a 1 ms step: well over 8 fixed steps in total.
    assert!(fixed_steps.load(Ordering::SeqCst) >= 8);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
}
