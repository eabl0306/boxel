//! Border-crossing cost: main-thread time of the frame in which the viewer enters the next chunk
//! (`App::update` of a world-only app: interest, job dispatch, stats), at radius 16 and 32 on
//! `TerrainGenerator::default()`.
//!
//! Every crossing starts from a settled world, like a player walking: jobs queued by the measured
//! frame are held, then run on the async compute pool, untimed, until the world settles again.
//!
//! Groups:
//! - `crossing_x` / `crossing_up`: the default world (no height limit, vertical radius 8,
//!   default budgets). The frame is bounded by `interest_budget`; the rest of a crossing is spread
//!   over the next frames.
//! - `crossing_x_total` / `crossing_up_total`: unlimited budgets, so the measured frame does all
//!   of a crossing's interest work (its total cost; it grows with the change, not the view).
//! - `crossing_x_fixed_height` / `crossing_up_fixed_height`: boxel M0's loaded set (the
//!   generator's 6 layers, every layer wanted) with unlimited budgets, comparable with M0's
//!   full recompute.
use boxel_app::App;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::CHUNK_SIZE;
use boxel_world::jobs::{JobExecutor, TaskPoolExecutor};
use boxel_world::{ChunkViewer, WorldPlugin, WorldSettings, WorldStats};
use boxel_worldgen::{TerrainGenerator, WorldGenerator};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Holds jobs until [`Deferred::release`] hands them to the async compute pool.
#[derive(Default)]
struct Deferred {
    held: Mutex<Vec<Job>>,
}

impl Deferred {
    fn release(&self) -> usize {
        let jobs = std::mem::take(&mut *self.held.lock().expect("lock"));
        let n = jobs.len();
        for job in jobs {
            TaskPoolExecutor.spawn(job);
        }
        n
    }
}

impl JobExecutor for Deferred {
    fn spawn(&self, job: Job) {
        self.held.lock().expect("lock").push(job);
    }
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    /// Default settings, vertical radius 8.
    Default,
    /// Unlimited budgets, vertical radius 8.
    Total,
    /// Unlimited budgets, the generator's fixed height.
    FixedHeight,
}

struct World {
    app: App,
    jobs: Arc<Deferred>,
    viewer: Entity,
    chunk: [i32; 3],
    /// Crossings so far (vertical ones go 6 up, 6 down, so the viewer stays near the terrain).
    crossings: u64,
}

impl World {
    fn new(radius: u32, mode: Mode) -> Self {
        let generator = TerrainGenerator::default();
        let mut settings = WorldSettings::default();
        let mut viewer = ChunkViewer::new(radius);
        if matches!(mode, Mode::Total | Mode::FixedHeight) {
            settings.interest_budget = Duration::MAX;
            settings.job_budget = Duration::MAX;
        }
        if matches!(mode, Mode::FixedHeight) {
            settings.vertical_bounds = Some(generator.vertical_chunks());
            viewer = viewer.with_vertical_radius(u32::MAX);
        }
        let jobs = Arc::new(Deferred::default());
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            generator: Arc::new(generator),
            settings,
            executor: jobs.clone(),
            ..WorldPlugin::default()
        });
        // Chunk (0, 2, 0): the surface layer (y = 64..95).
        let chunk = [0, 2, 0];
        let viewer = app
            .world_mut()
            .spawn((Transform::from_translation(center(chunk)), viewer))
            .id();
        let mut world = Self {
            app,
            jobs,
            viewer,
            chunk,
            crossings: 0,
        };
        world.settle();
        world
    }

    /// Runs frames (releasing held jobs) until the world settles.
    fn settle(&mut self) {
        let start = Instant::now();
        loop {
            self.app.update();
            let released = self.jobs.release();
            let stats = *self.app.world().resource::<WorldStats>();
            if released == 0 && stats.chunks > 0 && stats.is_settled() {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(300),
                "not settled: {stats:?}"
            );
            if released == 0 {
                std::thread::yield_now();
            }
        }
    }

    /// Moves the viewer one chunk along `axis` (x: always +1; y: 6 up, then 6 down) and times
    /// the frame that sees it.
    fn cross(&mut self, axis: usize) -> Duration {
        let up = axis == 0 || (self.crossings / 6).is_multiple_of(2);
        self.crossings += 1;
        self.chunk[axis] += if up { 1 } else { -1 };
        self.app
            .world_mut()
            .get_mut::<Transform>(self.viewer)
            .expect("viewer")
            .translation = center(self.chunk);
        let start = Instant::now();
        self.app.update();
        let frame = start.elapsed();
        self.settle();
        frame
    }
}

fn center([x, y, z]: [i32; 3]) -> DVec3 {
    let half = f64::from(CHUNK_SIZE) / 2.0;
    let at = |c: i32| f64::from(c * CHUNK_SIZE) + half;
    DVec3::new(at(x), at(y), at(z))
}

fn bench(c: &mut Criterion) {
    for (name, mode, axis) in [
        ("crossing_x", Mode::Default, 0),
        ("crossing_x_total", Mode::Total, 0),
        ("crossing_x_fixed_height", Mode::FixedHeight, 0),
        ("crossing_up", Mode::Default, 1),
        ("crossing_up_total", Mode::Total, 1),
        ("crossing_up_fixed_height", Mode::FixedHeight, 1),
    ] {
        let mut group = c.benchmark_group(name);
        group
            .sample_size(10)
            .warm_up_time(Duration::from_millis(50))
            .measurement_time(Duration::from_millis(400));
        for radius in [16, 32] {
            let mut world = World::new(radius, mode);
            group.bench_function(BenchmarkId::from_parameter(radius), |b| {
                b.iter_custom(|iters| (0..iters).map(|_| world.cross(axis)).sum());
            });
        }
        group.finish();
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
