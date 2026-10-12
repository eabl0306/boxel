//! `RenderPlugin` wiring without a window or GPU.
//!
//! The fork has no executor-kind getter, so the single-threaded `Render` executor is proven by
//! behavior: systems added to `Render` run on the thread that calls `App::update`, while the
//! same systems in `Update` (multi-threaded executor, `TaskPoolPlugin` present) also run on pool
//! threads (control).

use boxel_app::{App, Last, MainScheduleOrder, Plugin, TaskPoolOptions, TaskPoolPlugin, Update};
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::ScheduleLabel;
use boxel_gfx::PresentModePref;
use boxel_render::{
    ChunkMemoryError, ChunkMemorySettings, DebugAllocProbe, GpuFailed, GpuReady, MemoryCeiling,
    PresentSettings, Render, RenderInfo, RenderPlugin, RenderSettings, RenderSystems,
    RequestScreenshot, ScreenshotSaved, SurfaceInfo,
};
use boxel_world::ChunkMeshEvent;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

/// Systems per schedule in the thread tests (independent: no data access, so a multi-threaded
/// executor may run them in parallel).
const SYSTEMS: usize = 8;
/// Updates per thread test.
const UPDATES: usize = 4;

type Seen = Arc<Mutex<Vec<ThreadId>>>;

/// Busy work so a parallel executor has a reason to spread systems over its threads (no
/// `thread::sleep`: disallowed outside runners).
fn spin(duration: Duration) {
    let start = Instant::now();
    while start.elapsed() < duration {
        std::hint::spin_loop();
    }
}

/// `SYSTEMS` systems in `schedule` that record the thread they run on.
fn add_recorders(app: &mut App, schedule: impl ScheduleLabel + Clone) -> Seen {
    let seen: Seen = Arc::default();
    for _ in 0..SYSTEMS {
        let seen = Arc::clone(&seen);
        app.add_systems(schedule.clone(), move || {
            spin(Duration::from_millis(2));
            seen.lock().unwrap().push(thread::current().id());
        });
    }
    seen
}

fn finished(mut app: App) -> App {
    app.finish();
    app.cleanup();
    app
}

#[test]
fn render_systems_run_on_the_calling_thread_with_task_pools() {
    let mut app = App::new();
    app.add_plugins((TaskPoolPlugin::default(), RenderPlugin::default()));
    let render = add_recorders(&mut app, Render);
    let update = add_recorders(&mut app, Update);
    let mut app = finished(app);
    for _ in 0..UPDATES {
        app.update();
    }
    let me = thread::current().id();

    let render = render.lock().unwrap();
    assert_eq!(render.len(), SYSTEMS * UPDATES, "every Render system ran");
    assert!(
        render.iter().all(|id| *id == me),
        "a Render system ran off the calling thread: the Render executor is not single-threaded"
    );

    // Control: the default (multi-threaded) executor really uses the compute pool here, so the
    // check above can fail.
    let update = update.lock().unwrap();
    assert_eq!(update.len(), SYSTEMS * UPDATES);
    let threads: HashSet<_> = update.iter().collect();
    let hardware = thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let (_, _, pool_threads) = TaskPoolOptions::default().split(hardware);
    if pool_threads > 0 {
        assert!(
            threads.len() > 1,
            "control failed: {} Update systems all ran on one thread with {pool_threads} pool \
             threads",
            update.len()
        );
    }
}

/// Adds a counter system to `Render` before `RenderPlugin` exists.
struct EarlyRenderSystem(Arc<Mutex<u32>>);

impl Plugin for EarlyRenderSystem {
    fn build(&self, app: &mut App) {
        let runs = Arc::clone(&self.0);
        app.add_systems(Render, move || *runs.lock().unwrap() += 1);
    }
}

#[test]
fn render_systems_added_by_earlier_plugins_survive_render_plugin_build() {
    let runs = Arc::new(Mutex::new(0));
    let mut app = App::new();
    app.add_plugins((
        EarlyRenderSystem(Arc::clone(&runs)),
        RenderPlugin::default(),
    ));
    let mut app = finished(app);
    app.update();
    app.update();
    assert_eq!(*runs.lock().unwrap(), 2);
}

#[test]
fn render_runs_right_after_last_and_registers_its_messages() {
    let mut app = App::new();
    app.add_plugins(RenderPlugin {
        settings: RenderSettings {
            app_name: "plugin_test".to_string(),
            ..RenderSettings::default()
        },
    });
    let order = app.world().resource::<MainScheduleOrder>();
    let last = order
        .labels
        .iter()
        .position(|l| *l == Last.intern())
        .expect("Last in the order");
    assert_eq!(order.labels[last + 1], Render.intern());
    assert_eq!(
        order
            .labels
            .iter()
            .filter(|l| **l == Render.intern())
            .count(),
        1
    );
    let world = app.world();
    assert!(world.contains_resource::<Messages<GpuReady>>());
    assert!(world.contains_resource::<Messages<GpuFailed>>());
    assert!(world.contains_resource::<Messages<RequestScreenshot>>());
    assert!(world.contains_resource::<Messages<ScreenshotSaved>>());
    assert!(world.contains_resource::<Messages<ChunkMeshEvent>>());
    assert!(world.contains_resource::<Messages<DebugAllocProbe>>());
    assert!(world.contains_resource::<RenderInfo>());
    assert!(world.contains_resource::<SurfaceInfo>());
    assert_eq!(world.resource::<RenderSettings>().app_name, "plugin_test");
    // Vsync off by default, so the frame limit governs.
    assert_eq!(
        world.resource::<PresentSettings>().mode,
        PresentModePref::NoVsync
    );
    assert_eq!(world.resource::<RenderInfo>().present_mode, None);
}

#[test]
fn an_app_present_setting_survives_the_plugin() {
    for before in [true, false] {
        let mut app = App::new();
        if before {
            app.insert_resource(PresentSettings::vsync(true));
            app.add_plugins(RenderPlugin::default());
        } else {
            app.add_plugins(RenderPlugin::default());
            app.insert_resource(PresentSettings::vsync(true));
        }
        let mut app = finished(app);
        app.update();
        assert!(
            app.world().resource::<PresentSettings>().is_vsync(),
            "{before}"
        );
    }
}

#[derive(Resource, Default)]
struct Order(Vec<&'static str>);

#[test]
fn render_sets_are_chained_in_frame_order() {
    let mut app = App::new();
    app.add_plugins(RenderPlugin::default())
        .init_resource::<Order>();
    // Added in reverse to prove the set chain (not insertion order) decides.
    for (set, name) in [
        (RenderSystems::Cleanup, "cleanup"),
        (RenderSystems::Draw, "draw"),
        (RenderSystems::Prepare, "prepare"),
        (RenderSystems::Extract, "extract"),
        (RenderSystems::Setup, "setup"),
    ] {
        app.add_systems(
            Render,
            (move |mut order: ResMut<Order>| order.0.push(name)).in_set(set),
        );
    }
    let mut app = finished(app);
    app.update();
    assert_eq!(
        app.world().resource::<Order>().0,
        ["setup", "extract", "prepare", "draw", "cleanup"]
    );
}

#[derive(Resource, Default)]
struct Saved(Vec<Result<(), String>>);

#[test]
fn without_a_window_the_plugin_stays_idle() {
    let mut app = App::new();
    app.add_plugins(RenderPlugin::default())
        .init_resource::<Saved>()
        .add_systems(
            Update,
            |mut failed: MessageReader<GpuFailed>,
             mut ready: MessageReader<GpuReady>,
             mut saved: MessageReader<ScreenshotSaved>,
             mut out: ResMut<Saved>| {
                assert_eq!(failed.read().count(), 0, "no selection without a window");
                assert_eq!(ready.read().count(), 0);
                out.0.extend(saved.read().map(|s| s.result.clone()));
            },
        );
    let mut app = finished(app);
    app.world_mut().write_message(RequestScreenshot {
        path: std::env::temp_dir().join("boxel_render_plugin_idle.png"),
    });
    for _ in 0..3 {
        app.update();
    }
    let info = app.world().resource::<RenderInfo>();
    assert!(!info.ready);
    assert_eq!(info.frames_rendered, 0);
    assert_eq!(info.frames_presented, 0);
    // The request waits for a renderer instead of being answered with an error.
    assert!(app.world().resource::<Saved>().0.is_empty());
    assert_eq!(
        *app.world().resource::<SurfaceInfo>(),
        SurfaceInfo::default()
    );
    // `Shutdown` with nothing to release is fine.
    app.shutdown();
    assert!(!app.world().resource::<RenderInfo>().ready);
}

/// The chunk memory a renderer starts with (ADR 0035): two pages of 4 Mi quads, 32,768 slots,
/// the ceiling by adapter class, growth on; invalid settings are refused before a renderer is
/// built.
#[test]
fn chunk_memory_settings_default_and_validate() {
    let settings = RenderSettings::default().chunk_memory;
    assert_eq!(settings, ChunkMemorySettings::default());
    assert_eq!(
        (settings.initial_quads, settings.initial_slots),
        (8 << 20, 32768)
    );
    assert_eq!(
        (settings.page_quads, settings.page_chunks),
        (4 << 20, 10922)
    );
    assert_eq!(
        (settings.max_quads, settings.max_slots),
        (MemoryCeiling::Auto, 1 << 20)
    );
    assert!(settings.grow);
    assert_eq!(settings.validate(), Ok(()));
    let quads = |class| MemoryCeiling::Auto.quads(class);
    assert_eq!(quads(wgpu::DeviceType::DiscreteGpu), 192 << 20);
    assert_eq!(quads(wgpu::DeviceType::IntegratedGpu), 96 << 20);
    assert_eq!(quads(wgpu::DeviceType::Cpu), 32 << 20);
    let tiny = ChunkMemorySettings {
        page_quads: 1000,
        ..settings
    };
    assert_eq!(tiny.validate(), Err(ChunkMemoryError::PageQuads(1000)));
}
