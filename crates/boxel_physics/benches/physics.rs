//! Physics core benches on generated worlds, read through `ChunkSource` like the ECS layer does
//! (one hash lookup per chunk touched, uniform chunks without decoding).
//!
//! Targets (physics plan, phase P1; dev laptop, one thread):
//!
//! | Bench | Target |
//! |---|---|
//! | `sweep/player_walk`, `sweep/player_land`, `sweep/player_fall` | <= 0.3 us |
//! | `raycast/pick_6` | <= 0.5 us |
//! | `raycast/cave_256` | <= 5 us |
//! | `raycast/sky_1024` | <= 10 us |
//!
//! The worlds are streamed once by a headless app (`WorldPlugin`, inline jobs) before the
//! measurements: forest terrain around the origin, a wide sky above it and a line of
//! underground overworld chunks with caves. Run when the machine is idle (`tools/wait-idle.sh`).
#![expect(clippy::print_stdout, reason = "bench setup output")]

use boxel_app::App;
use boxel_math::DVec3;
use boxel_physics::{
    ChunkSource, CollisionTable, LocalBox, RayResult, SweepConfig, VoxelFilter, move_box,
    raycast_voxels,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, WorldPos};
use boxel_world::jobs::InlineExecutor;
use boxel_world::{ChunkMap, ChunkViewer, WorldPlugin, WorldSettings, WorldStats};
use boxel_worldgen::{ForestGenerator, OverworldGenerator, WorldGenerator};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A streamed world: its chunk map and settings.
struct World {
    map: ChunkMap,
    settings: WorldSettings,
}

impl World {
    fn source(&self) -> ChunkSource<'_> {
        ChunkSource::new(&self.map, &self.settings)
    }
}

/// Streams `generator` around `viewers` (position, radius, vertical radius) until settled.
fn streamed(generator: Arc<dyn WorldGenerator>, viewers: &[(DVec3, u32, u32)]) -> World {
    let start = Instant::now();
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator,
        executor: Arc::new(InlineExecutor),
        ..WorldPlugin::default()
    });
    for &(at, radius, vertical) in viewers {
        app.world_mut().spawn((
            Transform::from_translation(at),
            ChunkViewer::new(radius).with_vertical_radius(vertical),
        ));
    }
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(300),
            "not settled: {stats:?}"
        );
    }
    let map = app
        .world_mut()
        .remove_resource::<ChunkMap>()
        .expect("world plugin map");
    let settings = app.world().resource::<WorldSettings>().clone();
    println!(
        "physics bench: {} chunks streamed in {:.1} s",
        map.len(),
        start.elapsed().as_secs_f64()
    );
    World { map, settings }
}

/// The height of the first full block's top under `(x, z)`, from y = 150 down.
fn surface(world: &World, table: &CollisionTable, x: f64, z: f64) -> f64 {
    match raycast_voxels(
        &mut world.source(),
        table,
        WorldPos::from_dvec3(DVec3::new(x, 150.0, z)),
        DVec3::NEG_Y,
        400.0,
        VoxelFilter::Collision,
    ) {
        RayResult::Hit(hit) => hit.point.to_dvec3().y,
        other => panic!("no ground under ({x}, {z}): {other:?}"),
    }
}

fn player() -> LocalBox {
    LocalBox::new(DVec3::new(-0.3, 0.0, -0.3), DVec3::new(0.3, 1.8, 0.3))
}

fn benches(c: &mut Criterion) {
    let table = CollisionTable::from_registry(&BlockRegistry::default_blocks());
    let cfg = SweepConfig::default();
    let forest = streamed(
        Arc::new(ForestGenerator::default()),
        &[(DVec3::new(0.0, 70.0, 0.0), 2, 2)],
    );
    let ground = surface(&forest, &table, 0.5, 0.5);
    println!("physics bench: forest ground at y = {ground}");

    let mut sweep = c.benchmark_group("sweep");
    // Walking at 4.3 blocks/s on the ground with one step of gravity (64 Hz): Y clips on the
    // ground, X and Z move.
    let on_ground = WorldPos::from_dvec3(DVec3::new(0.5, ground, 0.5));
    let walk = DVec3::new(4.3 / 64.0, -32.0 / 4096.0, 1.1 / 64.0);
    sweep.bench_function("player_walk", |b| {
        b.iter(|| {
            black_box(move_box(
                &mut forest.source(),
                &table,
                black_box(on_ground),
                player(),
                black_box(walk),
                &[],
                &cfg,
            ))
        });
    });
    // Landing: half a block of fall that clips on the ground.
    let above = WorldPos::from_dvec3(DVec3::new(0.5, ground + 0.3, 0.5));
    let drop = DVec3::new(0.0, -0.5, 0.0);
    sweep.bench_function("player_land", |b| {
        b.iter(|| {
            black_box(move_box(
                &mut forest.source(),
                &table,
                black_box(above),
                player(),
                black_box(drop),
                &[],
                &cfg,
            ))
        });
    });
    // Falling through air (nothing hit).
    let high = WorldPos::from_dvec3(DVec3::new(0.5, ground + 10.0, 0.5));
    sweep.bench_function("player_fall", |b| {
        b.iter(|| {
            black_box(move_box(
                &mut forest.source(),
                &table,
                black_box(high),
                player(),
                black_box(drop),
                &[],
                &cfg,
            ))
        });
    });
    sweep.finish();

    let sky = streamed(
        Arc::new(ForestGenerator::default()),
        &[(DVec3::new(0.0, 200.0, 0.0), 17, 0)],
    );
    // A line of underground chunks with caves along +X at y = -200.
    let line: Vec<(DVec3, u32, u32)> = (0..10)
        .map(|i| (DVec3::new(f64::from(i) * 32.0 + 16.0, -200.0, 16.0), 0, 0))
        .collect();
    let caves = streamed(Arc::new(OverworldGenerator::default()), &line);

    let mut ray = c.benchmark_group("raycast");
    // Picking: from the eye (1.62 above the ground) 45 degrees down, reach 6.
    let eye = WorldPos::from_dvec3(DVec3::new(0.5, ground + 1.62, 0.5));
    let look = DVec3::new(0.0, -1.0, -1.0).normalize();
    assert!(matches!(
        raycast_voxels(
            &mut forest.source(),
            &table,
            eye,
            look,
            6.0,
            VoxelFilter::Outline { fluids: false }
        ),
        RayResult::Hit(_)
    ));
    ray.bench_function("pick_6", |b| {
        b.iter(|| {
            black_box(raycast_voxels(
                &mut forest.source(),
                &table,
                black_box(eye),
                black_box(look),
                6.0,
                VoxelFilter::Outline { fluids: false },
            ))
        });
    });
    // 256 blocks underground through stone and caves, with a filter that accepts nothing: the
    // whole length is walked (the worst case).
    let deep = WorldPos::from_dvec3(DVec3::new(8.5, -199.5, 16.5));
    let tunnel = DVec3::new(1.0, 0.02, 0.01).normalize();
    let mut never = |_: boxel_voxel::BlockPos, _: boxel_voxel::BlockState| false;
    assert_eq!(
        raycast_voxels(
            &mut caves.source(),
            &table,
            deep,
            tunnel,
            256.0,
            VoxelFilter::Custom(&mut never)
        ),
        RayResult::Miss
    );
    ray.bench_function("cave_256", |b| {
        b.iter(|| {
            let mut never = |_: boxel_voxel::BlockPos, _: boxel_voxel::BlockState| false;
            black_box(raycast_voxels(
                &mut caves.source(),
                &table,
                black_box(deep),
                black_box(tunnel),
                256.0,
                VoxelFilter::Custom(&mut never),
            ))
        });
    });
    // 1024 blocks diagonally through the sky above the forest (uniform air chunks).
    let sky_start = WorldPos::from_dvec3(DVec3::new(-360.5, 205.5, -360.5));
    let sky_dir = DVec3::new(1.0, 0.03, 1.0).normalize();
    assert_eq!(
        raycast_voxels(
            &mut sky.source(),
            &table,
            sky_start,
            sky_dir,
            1024.0,
            VoxelFilter::Collision
        ),
        RayResult::Miss,
        "the sky ray stays in loaded air"
    );
    ray.bench_function("sky_1024", |b| {
        b.iter(|| {
            black_box(raycast_voxels(
                &mut sky.source(),
                &table,
                black_box(sky_start),
                black_box(sky_dir),
                1024.0,
                VoxelFilter::Collision,
            ))
        });
    });
    ray.finish();
}

criterion_group!(physics, benches);
criterion_main!(physics);
