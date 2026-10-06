//! Physics core benches on generated worlds, read through `ChunkSource` like the ECS layer does
//! (one hash lookup per chunk touched, uniform chunks without decoding).
//!
//! Targets (physics plan, phases P1 and P2; dev laptop, one thread):
//!
//! | Bench | Target |
//! |---|---|
//! | `sweep/player_walk`, `sweep/player_land`, `sweep/player_fall` | <= 0.3 us |
//! | `raycast/pick_6` | <= 0.5 us |
//! | `raycast/cave_256` | <= 5 us |
//! | `raycast/sky_1024` | <= 10 us |
//! | `tick/0` | <= 20 us |
//! | `tick/100` | <= 0.05 ms |
//! | `tick/1000` | <= 0.5 ms |
//!
//! The worlds are streamed once by a headless app (`WorldPlugin`, inline jobs) before the
//! measurements: forest terrain around the origin, a wide sky above it and a line of
//! underground overworld chunks with caves. A `tick` is one run of `FixedPostUpdate` with
//! `PhysicsPlugin` (every physics system of a fixed step) on the forest, with that many awake
//! bodies: 70 % walking characters that turn around every 64 steps (some jump then), 30 % items
//! dropped from up to 20 blocks. Run when the machine is idle (`tools/wait-idle.sh`).
#![expect(clippy::print_stdout, reason = "bench setup output")]

use boxel_app::{App, FixedPostUpdate, FixedTime};
use boxel_ecs::message::Messages;
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, Vec2};
use boxel_physics::{
    BodyPosition, BodySettings, CharacterController, ChunkSource, Collider, CollisionTable,
    LocalBox, MoveIntent, PhysicsPlugin, PhysicsStats, RayResult, RigidBody, SweepConfig, Teleport,
    Velocity, VoxelFilter, VoxelImpact, move_box, raycast_voxels,
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

/// A headless app with the forest streamed around the origin (radius 2) and `PhysicsPlugin`,
/// without `TimePlugin`: the bench runs `FixedPostUpdate` itself.
fn physics_app() -> App {
    let start = Instant::now();
    let mut app = App::new();
    app.add_plugins((
        WorldPlugin {
            generator: Arc::new(ForestGenerator::default()),
            executor: Arc::new(InlineExecutor),
            ..WorldPlugin::default()
        },
        PhysicsPlugin::default(),
    ))
    .init_resource::<FixedTime>();
    app.finish();
    app.cleanup();
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(0.0, 70.0, 0.0)),
        ChunkViewer::new(2).with_vertical_radius(2),
    ));
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
    app
}

/// Where a walker started: the bench puts it back there every 1024 steps, so walkers that get
/// stuck on one side of a tree do not drift out of the loaded area over a long measurement.
#[derive(Component, Clone, Copy)]
struct Home(WorldPos);

/// `SplitMix64`, for the bodies' places.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Spawns `n` bodies on the forest within 48 blocks of the origin (see the module docs).
fn spawn_bodies(app: &mut App, table: &CollisionTable, n: u64) {
    let mut places = Vec::new();
    {
        let world = app.world();
        let mut source = ChunkSource::new(
            world.resource::<ChunkMap>(),
            world.resource::<WorldSettings>(),
        );
        for i in 0..n {
            let r = mix(i);
            let x = (r % 96) as f64 - 47.5;
            let z = ((r >> 8) % 96) as f64 - 47.5;
            let top = WorldPos::from_dvec3(DVec3::new(x, 150.0, z));
            let RayResult::Hit(hit) = raycast_voxels(
                &mut source,
                table,
                top,
                DVec3::NEG_Y,
                200.0,
                VoxelFilter::Collision,
            ) else {
                panic!("no ground under ({x}, {z})");
            };
            places.push((r, hit.point));
        }
    }
    for (r, ground) in places {
        if r % 10 < 7 {
            let angle = (r >> 16) % 8;
            let wish = [
                Vec2::X,
                Vec2::NEG_X,
                Vec2::Y,
                Vec2::NEG_Y,
                Vec2::ONE.normalize(),
                Vec2::NEG_ONE.normalize(),
                Vec2::new(1.0, -1.0).normalize(),
                Vec2::new(-1.0, 1.0).normalize(),
            ][angle as usize];
            app.world_mut().spawn((
                RigidBody::Dynamic,
                Collider::standing(0.6, 1.8),
                CharacterController::default(),
                MoveIntent { wish, jump: false },
                Teleport(ground),
                Home(ground),
            ));
        } else {
            let height = ((r >> 24) % 20) as f64 + 0.5;
            app.world_mut().spawn((
                RigidBody::Dynamic,
                Collider::aabb(DVec3::splat(0.125)).sensor(),
                BodySettings::item(),
                Teleport(ground.offset(DVec3::new(0.0, height, 0.0))),
            ));
        }
    }
}

/// Every 64 steps the walkers turn around and every fourth one jumps, and every 1024 steps they
/// go back home; the impact messages of those steps are dropped (no `First` runs here to
/// rotate them).
fn turn(app: &mut App, tick: u64) {
    let world = app.world_mut();
    let home = tick.is_multiple_of(1024);
    let mut walkers = world.query::<(
        Entity,
        &mut MoveIntent,
        &Home,
        &mut BodyPosition,
        &mut Velocity,
    )>();
    for (entity, mut intent, start, mut position, mut velocity) in walkers.iter_mut(world) {
        intent.wish = -intent.wish;
        if (entity.to_bits() ^ (tick / 64)).is_multiple_of(4) {
            intent.jump = true;
        }
        if home {
            // `Transform` still holds what physics wrote: no teleport, the position stays.
            position.0 = start.0;
            velocity.0 = DVec3::ZERO;
        }
    }
    world.resource_mut::<Messages<VoxelImpact>>().update();
}

fn tick_benches(c: &mut Criterion) {
    let table = CollisionTable::from_registry(&BlockRegistry::default_blocks());
    let mut app = physics_app();
    let mut tick = c.benchmark_group("tick");
    let mut spawned = 0;
    for n in [0, 100, 1000] {
        spawn_bodies(&mut app, &table, n - spawned);
        spawned = n;
        // Settle the drops a little before measuring.
        for t in 0..256u64 {
            if t.is_multiple_of(64) {
                turn(&mut app, t);
            }
            app.world_mut().run_schedule(FixedPostUpdate);
        }
        let mut t = 0u64;
        tick.bench_function(n.to_string(), |b| {
            b.iter(|| {
                t += 1;
                if t.is_multiple_of(64) {
                    turn(&mut app, t);
                }
                app.world_mut().run_schedule(FixedPostUpdate);
            });
        });
        let stats = *app.world().resource::<PhysicsStats>();
        println!(
            "physics bench: tick/{n}: bodies={} dynamic={} frozen={}",
            stats.bodies, stats.dynamic, stats.frozen
        );
        assert_eq!(stats.frozen, 0, "every body is on loaded ground");
    }
    tick.finish();
}

criterion_group!(physics, benches, tick_benches);
criterion_main!(physics);
