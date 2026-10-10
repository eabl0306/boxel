//! Physics core benches on generated worlds, read through `ChunkSource` like the ECS layer does
//! (one hash lookup per chunk touched, uniform chunks without decoding).
//!
//! Targets (physics plan, phases P1 to P4; dev laptop, one thread unless noted):
//!
//! | Bench | Target |
//! |---|---|
//! | `sweep/player_walk`, `sweep/player_land`, `sweep/player_fall` | <= 0.3 us |
//! | `sweep/step_wall`, `sweep/step_wall_slide` and their `_tried` | no target (the step-up skip) |
//! | `sweep/*_fenced`, `raycast/sky_1024_fenced`, `tick/1000_fenced` | no target (fences, below) |
//! | `raycast/pick_6` | <= 0.5 us |
//! | `raycast/cave_256` | <= 5 us |
//! | `raycast/sky_1024` | <= 10 us |
//! | `tick/0` | <= 20 us |
//! | `tick/100` | <= 0.05 ms |
//! | `tick/1000` | <= 0.5 ms |
//! | `frame/1000`, `frame/1000_multi_thread` | no target (per-frame cost, compare the two) |
//! | `grid/10000` | <= 0.3 ms |
//! | `grid/1000`, `grid/10000_crowd` | no target (scaling, a dense crowd) |
//! | `scale/10000_asleep_one_thread` | <= 0.6 ms |
//! | `scale/10000_asleep` | no target (the same with the parallel step, as a game runs it) |
//! | `scale/10000_asleep_chunk_events` | no target (one thread, 16 chunk loads a step) |
//! | `scale/sleep_churn` | no target (one thread, 1000 bodies put to sleep and woken) |
//! | `scale/10000_awake` (parallel step) | <= 1.5 ms |
//! | `scale/10000_awake_one_thread` | no target (the same on one thread, to compare) |
//! | `scale/4096_packed` | no target (a packed crowd: its pairs are one island) |
//! | `scale/1000_projectiles_one_thread` | <= 0.3 ms |
//! | `scale/1000_projectiles` | no target (the same with the parallel step) |
//!
//! The worlds are streamed once by a headless app (`WorldPlugin`, inline jobs) before the
//! measurements: forest terrain around the origin, a wide sky above it and a line of
//! underground overworld chunks with caves. A `tick` is one run of `FixedPostUpdate` with
//! `PhysicsPlugin` (every physics system of a fixed step) on the forest, on one thread
//! (`PhysicsSettings::parallel_threshold` off, as the targets are), with that many bodies: 70 %
//! walking characters that turn around every 64 steps (some jump then), 30 % items dropped from
//! up to 20 blocks (they come to rest and fall asleep), all `Interpolated` as in the examples.
//! `sweep/step_wall` is one step of a walker (step height 0.6) pushing into a wall one block high
//! on a small test world, `step_wall_slide` the same sliding along it diagonally, and the
//! `_tried` variants always try the lifted moves (without the early skip of the step-up).
//! A `frame` is one run of
//! `RunFixedMainLoop` in a frame without a fixed step (144 fps): the interpolation of the 1000
//! bodies and the schedule's executor, on the single-threaded executor `PhysicsPlugin` sets and
//! on the multi-threaded one. `grid/N` builds the broadphase grid of N bodies and lists its
//! candidate pairs (what `PhysicsSystems::Broadphase` does every step): player-size bodies
//! (70 %) and items (30 %) at random, 10 % of them 2 blocks up, as spread as the `tick` bodies
//! (one per 9.2 blocks of ground: 1000 on 96 x 96 blocks, 10 000 on 304 x 304), with the grid
//! reused across steps. `grid/10000_crowd` packs the 10 000 on 128 x 128 blocks (one per 1.6
//! blocks of ground: cells of 4 x 4 blocks hold about 10 bodies).
//!
//! `scale/*` (phase P4) run on a wider forest (radius 7) with the bodies as spread as in `tick`
//! (10 000 on 304 x 304 blocks): `10000_asleep` has 9000 dropped items that came to rest and
//! fell asleep and 1000 walkers, with the default parallel step (1000 awake walkers are above
//! `PhysicsSettings::parallel_threshold`), and `10000_asleep_one_thread` is the same on one
//! thread. `10000_asleep_chunk_events` adds 16 chunk loads a step (`ChunkDataEvent::Loaded`) in
//! chunks among the sleepers that none of them reaches: finding that out is the cost.
//! `sleep_churn` puts 1000 resting crates to sleep by hand and wakes them all again, in turns
//! (the sleeper store's insertions and removals, without the 32 still steps). `10000_awake` has
//! 7000 walkers and 3000 items with sleeping off, so every body steps, on the compute threads,
//! and `10000_awake_one_thread` is the same on one thread. `4096_packed` has 4096 walkers
//! packed on 40 x 40 blocks (about one per 0.4 blocks of ground), with the parallel step:
//! they push each other all the time and their pairs make one island, so the solve's pushes
//! stay on one thread. `1000_projectiles` (radius 2) has 100 walkers and 1000 arrows shot over
//! the forest, shot again every 32 steps, with the parallel step, and
//! `1000_projectiles_one_thread` on one thread.
//!
//! The `_fenced` benches (run last) are `player_walk`, `player_land`, `player_fall`,
//! `step_wall`, `sky_1024` and `tick/1000` in a copy of their world with a fence in every chunk,
//! off their paths (the step one with a fence beside the wall): a fence's collision reaches
//! into the cell above (ADR 0026), so sweeps and collision rays read the cells under what they
//! cross everywhere there, while elsewhere they skip them. Run when the machine is idle
//! (`tools/wait-idle.sh`).
#![expect(clippy::print_stdout, reason = "bench setup output")]

use boxel_app::{App, FixedPostUpdate, FixedTime, RunFixedMainLoop};
use boxel_ecs::message::Messages;
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::MultiThreadedExecutor;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    BlockBounds, BodyPosition, BodySettings, CharacterController, ChunkSource, Collider,
    CollisionEnded, CollisionLayers, CollisionStarted, CollisionTable, Grid, Interpolated,
    LocalBox, MoveIntent, PhysicsPlugin, PhysicsSettings, PhysicsStats, Projectile, ProjectileHit,
    RayResult, RigidBody, Sleeping, StepUp, SweepConfig, Teleport, TestWorld, Velocity,
    VoxelFilter, VoxelImpact, move_box, move_box_stepping, move_box_stepping_unpruned,
    raycast_voxels,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, CHUNK_SHIFT, ChunkPos, LocalPos, WorldPos};
use boxel_world::jobs::InlineExecutor;
use boxel_world::{
    ChunkDataEvent, ChunkMap, ChunkViewer, DataState, EditQueue, WorldPlugin, WorldSettings,
    WorldStats,
};
use boxel_worldgen::{ForestGenerator, OverworldGenerator, WorldGenerator};
use criterion::measurement::WallTime;
use criterion::{BenchmarkGroup, Criterion, criterion_group, criterion_main};
use std::collections::BTreeSet;
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
    streamed_with(generator, viewers, false)
}

/// [`streamed`], with a fence in every chunk when `fenced` ([`fence_every_chunk`]).
fn streamed_with(
    generator: Arc<dyn WorldGenerator>,
    viewers: &[(DVec3, u32, u32)],
    fenced: bool,
) -> World {
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
    if fenced {
        fence_every_chunk(&mut app);
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

/// Puts a fence at local `(8, 8, 24)` of every loaded chunk of `app`'s world and updates it until
/// they are all there and the world settled again. Every chunk then holds a block whose
/// collision reaches into the cell above (ADR 0026): the sweeps and the collision rays read
/// the cells under what they cross everywhere, as before they learned to skip them in chunks
/// without such blocks. The fences are off the benches' paths (never in the column of a
/// diagonal ray at `x = z`).
fn fence_every_chunk(app: &mut App) {
    let start = Instant::now();
    let registry = BlockRegistry::default_blocks();
    let fence = registry.parse_state("boxel:oak_fence").expect("fence");
    let table = CollisionTable::from_registry(&registry);
    assert!(table.is_tall(fence));
    let spots: Vec<BlockPos> = app
        .world()
        .resource::<ChunkMap>()
        .iter()
        .filter(|(_, slot)| matches!(slot.data(), DataState::Ready(_)))
        .map(|(_, slot)| BlockPos::from_parts(slot.key().pos, LocalPos::new(8, 8, 24)))
        .collect();
    let mut queue = app.world_mut().resource_mut::<EditQueue>();
    for &p in &spots {
        queue.set_block(p, fence);
    }
    loop {
        app.update();
        let world = app.world();
        let map = world.resource::<ChunkMap>();
        // Neighbor rules may connect a fence to the blocks beside it: still a fence.
        if world.resource::<WorldStats>().is_settled()
            && spots
                .iter()
                .all(|&p| map.block_at(p).is_some_and(|s| table.is_tall(s)))
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(300),
            "fences not placed"
        );
    }
    println!(
        "physics bench: {} fences placed in {:.1} s",
        spots.len(),
        start.elapsed().as_secs_f64()
    );
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
    // A walker (step height 0.6) pushing into a wall one block high, straight and sliding along
    // it diagonally: with the early skip of the step-up, and with the lifted moves tried.
    let mut wall = TestWorld::new();
    wall.fill(
        BlockPos::new(-4, -1, -4),
        BlockPos::new(4, -1, 4),
        BlockState(1),
    );
    wall.fill(
        BlockPos::new(1, 0, -4),
        BlockPos::new(1, 0, 4),
        BlockState(1),
    );
    let touching = WorldPos::from_dvec3(DVec3::new(0.7, 0.0, 0.5));
    let step = StepUp {
        height: 0.6,
        grounded: true,
    };
    for (name, d) in [
        ("step_wall", DVec3::new(4.3 / 64.0, -32.0 / 4096.0, 0.0)),
        (
            "step_wall_slide",
            DVec3::new(3.0 / 64.0, -32.0 / 4096.0, 3.0 / 64.0),
        ),
    ] {
        let skip = move_box_stepping(&mut wall, &table, touching, player(), d, &[], &cfg, step);
        let tried =
            move_box_stepping_unpruned(&mut wall, &table, touching, player(), d, &[], &cfg, step);
        assert_eq!(skip, tried);
        assert!(skip.is_ok_and(|out| out.touched(boxel_voxel::Face::PosX)));
        sweep.bench_function(name, |b| {
            b.iter(|| {
                black_box(move_box_stepping(
                    &mut wall,
                    &table,
                    black_box(touching),
                    player(),
                    black_box(d),
                    &[],
                    &cfg,
                    step,
                ))
            });
        });
        sweep.bench_function(format!("{name}_tried"), |b| {
            b.iter(|| {
                black_box(move_box_stepping_unpruned(
                    &mut wall,
                    &table,
                    black_box(touching),
                    player(),
                    black_box(d),
                    &[],
                    &cfg,
                    step,
                ))
            });
        });
    }
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
    let mut never = |_: BlockPos, _: BlockState| false;
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
            let mut never = |_: BlockPos, _: BlockState| false;
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
    physics_app_with(2)
}

/// [`physics_app`] with the forest streamed `radius` chunks around the origin.
fn physics_app_with(radius: u32) -> App {
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
        ChunkViewer::new(radius).with_vertical_radius(2),
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

/// Where `n` bodies go on a square of `side` blocks around the origin: the ground under each
/// (the top of the first full block from y = 150 down), with each body's random bits. `seed`
/// picks another set of places.
fn ground_places(
    app: &App,
    table: &CollisionTable,
    n: u64,
    side: u64,
    seed: u64,
) -> Vec<(u64, WorldPos)> {
    let world = app.world();
    let mut source = ChunkSource::new(
        world.resource::<ChunkMap>(),
        world.resource::<WorldSettings>(),
    );
    let half = side as f64 * 0.5;
    (0..n)
        .map(|i| {
            let r = mix(i ^ seed);
            let x = (r % side) as f64 - half + 0.5;
            let z = ((r >> 16) % side) as f64 - half + 0.5;
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
            (r, hit.point)
        })
        .collect()
}

/// A walker on the ground at `ground` going one of 8 ways (from `r`), interpolated.
fn walker(r: u64, ground: WorldPos) -> impl Bundle {
    let wish = [
        Vec2::X,
        Vec2::NEG_X,
        Vec2::Y,
        Vec2::NEG_Y,
        Vec2::ONE.normalize(),
        Vec2::NEG_ONE.normalize(),
        Vec2::new(1.0, -1.0).normalize(),
        Vec2::new(-1.0, 1.0).normalize(),
    ][((r >> 40) % 8) as usize];
    (
        RigidBody::Dynamic,
        Collider::standing(0.6, 1.8),
        CharacterController::default(),
        MoveIntent {
            wish,
            ..MoveIntent::default()
        },
        Interpolated::default(),
        Teleport(ground),
        Home(ground),
    )
}

/// An item dropped from up to 20 blocks above `ground` (from `r`), interpolated.
fn item(r: u64, ground: WorldPos) -> impl Bundle {
    let height = ((r >> 24) % 20) as f64 + 0.5;
    (
        RigidBody::Dynamic,
        Collider::aabb(DVec3::splat(0.125)).sensor(),
        BodySettings::item(),
        Interpolated::default(),
        Teleport(ground.offset(DVec3::new(0.0, height, 0.0))),
    )
}

/// `n` bodies on a square of `side` blocks: `walkers` in 10 are walkers, the rest items.
fn spawn_spread(app: &mut App, table: &CollisionTable, n: u64, side: u64, walkers: u64, seed: u64) {
    for (r, ground) in ground_places(app, table, n, side, seed) {
        if r % 10 < walkers {
            app.world_mut().spawn(walker(r, ground));
        } else {
            app.world_mut().spawn(item(r, ground));
        }
    }
}

/// Spawns `n` bodies on the forest within 48 blocks of the origin (see the module docs; the
/// scene of `tick/*` since P2).
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
                MoveIntent {
                    wish,
                    ..MoveIntent::default()
                },
                Interpolated::default(),
                Teleport(ground),
                Home(ground),
            ));
        } else {
            let height = ((r >> 24) % 20) as f64 + 0.5;
            app.world_mut().spawn((
                RigidBody::Dynamic,
                Collider::aabb(DVec3::splat(0.125)).sensor(),
                BodySettings::item(),
                Interpolated::default(),
                Teleport(ground.offset(DVec3::new(0.0, height, 0.0))),
            ));
        }
    }
}

/// Every 64 steps the walkers turn around and every fourth one jumps, and every 1024 steps they
/// go back home; the messages of those steps are dropped (no `First` runs here to rotate
/// them).
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
    world.resource_mut::<Messages<CollisionStarted>>().update();
    world.resource_mut::<Messages<CollisionEnded>>().update();
    world.resource_mut::<Messages<ProjectileHit>>().update();
}

fn tick_benches(c: &mut Criterion) {
    let table = CollisionTable::from_registry(&BlockRegistry::default_blocks());
    let mut app = physics_app();
    // The targets are for one thread.
    app.world_mut()
        .resource_mut::<PhysicsSettings>()
        .parallel_threshold = usize::MAX;
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

    // Frames at 144 fps that run no step: the accumulator moves, so alpha (and every
    // interpolated `Transform`) changes each frame.
    let frame_time = Duration::from_nanos(6_944_444);
    let run_frame = |app: &mut App| {
        {
            let mut fixed = app.world_mut().resource_mut::<FixedTime>();
            fixed.accumulate(frame_time);
            while fixed.expend() {}
        }
        app.world_mut().run_schedule(RunFixedMainLoop);
    };
    let mut frame = c.benchmark_group("frame");
    frame.bench_function("1000", |b| b.iter(|| run_frame(&mut app)));
    app.edit_schedule(RunFixedMainLoop, |schedule| {
        schedule.set_executor(MultiThreadedExecutor::new());
    });
    frame.bench_function("1000_multi_thread", |b| b.iter(|| run_frame(&mut app)));
    frame.finish();
}

/// The bounds of `n` bodies of the grid bench on a square of `side` blocks.
fn crowd(n: u64, side: u64) -> Vec<BlockBounds> {
    let half = side as f64 * 0.5;
    (0..n)
        .map(|i| {
            let r = mix(i ^ 0xC0FF_EE00);
            let x = (r % (side * 100)) as f64 / 100.0 - half;
            let z = ((r >> 20) % (side * 100)) as f64 / 100.0 - half;
            let y = if (r >> 32).is_multiple_of(10) {
                66.0
            } else {
                64.0
            };
            let shape = if (r >> 40) % 10 < 7 {
                LocalBox::new(DVec3::new(-0.3, 0.0, -0.3), DVec3::new(0.3, 1.8, 0.3))
            } else {
                LocalBox::new(DVec3::splat(-0.125), DVec3::splat(0.125))
            };
            BlockBounds::of_box(WorldPos::from_dvec3(DVec3::new(x, y, z)), shape)
        })
        .collect()
}

fn grid_benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("grid");
    let mut grid = Grid::default();
    let mut pairs = Vec::new();
    for (name, n, side) in [
        ("1000", 1000, 96),
        ("10000", 10_000, 304),
        ("10000_crowd", 10_000, 128),
    ] {
        let bodies = crowd(n, side);
        let run = |grid: &mut Grid, pairs: &mut Vec<(u32, u32)>| {
            grid.reset(2);
            for &bounds in &bodies {
                grid.insert(bounds, CollisionLayers::default(), true);
            }
            grid.pairs(pairs);
        };
        run(&mut grid, &mut pairs);
        println!(
            "physics bench: grid/{name}: {} candidate pairs",
            pairs.len()
        );
        group.bench_function(name, |b| {
            b.iter(|| {
                run(&mut grid, &mut pairs);
                black_box(pairs.len())
            });
        });
    }
    group.finish();
}

/// A shot of `scale/1000_projectiles`: where it starts and its velocity.
#[derive(Component, Clone, Copy)]
struct Shot {
    from: WorldPos,
    velocity: DVec3,
}

/// Every arrow is shot again (`Teleport` and `Velocity`: stuck ones wake up).
fn shoot_again(app: &mut App) {
    let world = app.world_mut();
    let mut shots = world.query::<(Entity, &Shot)>();
    let again: Vec<(Entity, Shot)> = shots.iter(world).map(|(e, s)| (e, *s)).collect();
    for (entity, shot) in again {
        world
            .entity_mut(entity)
            .insert((Teleport(shot.from), Velocity(shot.velocity)));
    }
}

/// Runs `steps` fixed steps, turning the walkers every 64.
fn run_steps(app: &mut App, steps: u64) {
    for t in 0..steps {
        if t.is_multiple_of(64) {
            turn(app, t);
        }
        app.world_mut().run_schedule(FixedPostUpdate);
    }
}

/// Where the time of a step goes, over 128 steps: the whole step, the moves
/// (`PhysicsStats::step_time`) and the work between bodies (`solve_time`), in milliseconds.
fn breakdown(app: &mut App, name: &str) {
    let (mut total, mut step, mut solve) = (0.0, 0.0, 0.0);
    for t in 0..128u64 {
        if t.is_multiple_of(64) {
            turn(app, t);
        }
        let start = Instant::now();
        app.world_mut().run_schedule(FixedPostUpdate);
        total += start.elapsed().as_secs_f64();
        let stats = app.world().resource::<PhysicsStats>();
        step += stats.step_time.as_secs_f64();
        solve += stats.solve_time.as_secs_f64();
    }
    let ms = |s: f64| s / 128.0 * 1e3;
    let stats = *app.world().resource::<PhysicsStats>();
    println!(
        "physics bench: {name}: step {:.3} ms = moves {:.3} + between bodies {:.3} + rest {:.3} \
         (pairs={} pushes={} overlaps={})",
        ms(total),
        ms(step),
        ms(solve),
        ms(total - step - solve),
        stats.pairs,
        stats.pushes,
        stats.overlaps
    );
}

/// Up to `n` loads (`ChunkDataEvent::Loaded`) of loaded chunks among the sleepers (inside the
/// box of the chunks their bounds plus one block reach) that none of them reaches: such a load
/// wakes nothing, so a bench of it measures finding that out.
fn empty_chunk_loads(app: &mut App, n: usize) -> Vec<ChunkDataEvent> {
    let world = app.world_mut();
    let mut sleepers = world.query_filtered::<(&BodyPosition, &Collider), With<Sleeping>>();
    let chunk =
        |v: I64Vec3| I64Vec3::new(v.x >> CHUNK_SHIFT, v.y >> CHUNK_SHIFT, v.z >> CHUNK_SHIFT);
    let mut reached = BTreeSet::new();
    let mut area: Option<(I64Vec3, I64Vec3)> = None;
    for (position, collider) in sleepers.iter(world) {
        let bounds = BlockBounds::of_box(position.0.normalize(), collider.voxel_box());
        let (lo, hi) = (
            chunk(bounds.min - I64Vec3::ONE),
            chunk(bounds.max + I64Vec3::ONE),
        );
        area = Some(match area {
            Some((min, max)) => (min.min(lo), max.max(hi)),
            None => (lo, hi),
        });
        for x in lo.x..=hi.x {
            for y in lo.y..=hi.y {
                for z in lo.z..=hi.z {
                    reached.insert(ChunkPos::new(x, y, z));
                }
            }
        }
    }
    let Some((lo, hi)) = area else {
        return Vec::new();
    };
    world
        .resource::<ChunkMap>()
        .iter()
        .filter_map(|(id, slot)| {
            let key = slot.key();
            let p = I64Vec3::new(key.pos.x, key.pos.y, key.pos.z);
            (p.cmpge(lo).all() && p.cmple(hi).all() && !reached.contains(&key.pos))
                .then_some(ChunkDataEvent::Loaded { id, key })
        })
        .take(n)
        .collect()
}

/// The bench of one step at a time, turning the walkers every 64 steps.
fn bench_steps(group: &mut BenchmarkGroup<'_, WallTime>, name: &str, app: &mut App, t: &mut u64) {
    group.bench_function(name, |b| {
        b.iter(|| {
            *t += 1;
            if t.is_multiple_of(64) {
                turn(app, *t);
            }
            app.world_mut().run_schedule(FixedPostUpdate);
        });
    });
}

fn scale_benches(c: &mut Criterion) {
    let table = CollisionTable::from_registry(&BlockRegistry::default_blocks());
    let mut group = c.benchmark_group("scale");

    // 10 000 bodies, 9000 of them items that come to rest and sleep.
    let mut app = physics_app_with(7);
    spawn_spread(&mut app, &table, 10_000, 304, 1, 0x5EED);
    run_steps(&mut app, 512);
    let stats = *app.world().resource::<PhysicsStats>();
    println!(
        "physics bench: scale/10000_asleep: bodies={} dynamic={} sleeping={} frozen={}",
        stats.bodies, stats.dynamic, stats.sleeping, stats.frozen
    );
    assert_eq!(stats.frozen, 0, "every body is on loaded ground");
    assert!(stats.sleeping >= 8800, "{stats:?}");
    breakdown(&mut app, "scale/10000_asleep");
    let mut t = 0u64;
    bench_steps(&mut group, "10000_asleep", &mut app, &mut t);
    // The target is for one thread.
    app.world_mut()
        .resource_mut::<PhysicsSettings>()
        .parallel_threshold = usize::MAX;
    breakdown(&mut app, "scale/10000_asleep_one_thread");
    bench_steps(&mut group, "10000_asleep_one_thread", &mut app, &mut t);
    let stats = *app.world().resource::<PhysicsStats>();
    assert!(!stats.parallel && stats.sleeping >= 8800, "{stats:?}");

    // Chunk loads among the sleepers that wake none of them.
    let loads = empty_chunk_loads(&mut app, 16);
    println!(
        "physics bench: scale/10000_asleep_chunk_events: {} loads a step",
        loads.len()
    );
    assert!(
        !loads.is_empty(),
        "no chunk among the sleepers is free of them"
    );
    group.bench_function("10000_asleep_chunk_events", |b| {
        b.iter(|| {
            t += 1;
            if t.is_multiple_of(64) {
                turn(&mut app, t);
            }
            let world = app.world_mut();
            let mut messages = world.resource_mut::<Messages<ChunkDataEvent>>();
            messages.update();
            messages.write_batch(loads.iter().copied());
            world.run_schedule(FixedPostUpdate);
        });
    });
    {
        let mut messages = app.world_mut().resource_mut::<Messages<ChunkDataEvent>>();
        messages.update();
        messages.update();
    }
    let stats = *app.world().resource::<PhysicsStats>();
    assert!(stats.sleeping >= 8800, "the loads woke nobody: {stats:?}");
    app.world_mut()
        .resource_mut::<PhysicsSettings>()
        .parallel_threshold = PhysicsSettings::default().parallel_threshold;

    // 10 000 awake bodies (sleeping off), on the compute threads and on one.
    let bodies: Vec<Entity> = {
        let world = app.world_mut();
        let mut all = world.query_filtered::<Entity, With<RigidBody>>();
        all.iter(world).collect()
    };
    for entity in bodies {
        app.world_mut().despawn(entity);
    }
    app.world_mut()
        .resource_mut::<PhysicsSettings>()
        .sleep_steps = 0;
    spawn_spread(&mut app, &table, 10_000, 304, 7, 0xA3A3);
    run_steps(&mut app, 256);
    let stats = *app.world().resource::<PhysicsStats>();
    println!(
        "physics bench: scale/10000_awake: bodies={} dynamic={} sleeping={} frozen={} \
         parallel={}",
        stats.bodies, stats.dynamic, stats.sleeping, stats.frozen, stats.parallel
    );
    assert_eq!((stats.dynamic, stats.sleeping), (10_000, 0), "{stats:?}");
    assert!(stats.parallel);
    breakdown(&mut app, "scale/10000_awake");
    let mut t = 0u64;
    bench_steps(&mut group, "10000_awake", &mut app, &mut t);
    app.world_mut()
        .resource_mut::<PhysicsSettings>()
        .parallel_threshold = usize::MAX;
    breakdown(&mut app, "scale/10000_awake_one_thread");
    bench_steps(&mut group, "10000_awake_one_thread", &mut app, &mut t);

    // 4096 walkers packed on 40 x 40 blocks (sleeping still off), with the parallel step.
    let bodies: Vec<Entity> = {
        let world = app.world_mut();
        let mut all = world.query_filtered::<Entity, With<RigidBody>>();
        all.iter(world).collect()
    };
    for entity in bodies {
        app.world_mut().despawn(entity);
    }
    app.world_mut()
        .resource_mut::<PhysicsSettings>()
        .parallel_threshold = PhysicsSettings::default().parallel_threshold;
    spawn_spread(&mut app, &table, 4096, 40, 10, 0xC0DE);
    run_steps(&mut app, 128);
    let stats = *app.world().resource::<PhysicsStats>();
    println!(
        "physics bench: scale/4096_packed: bodies={} dynamic={} frozen={} pairs={} pushes={} \
         islands={}",
        stats.bodies, stats.dynamic, stats.frozen, stats.pairs, stats.pushes, stats.islands
    );
    assert_eq!((stats.dynamic, stats.sleeping), (4096, 0), "{stats:?}");
    breakdown(&mut app, "scale/4096_packed");
    let mut t = 0u64;
    bench_steps(&mut group, "4096_packed", &mut app, &mut t);
    drop(app);

    // 2000 resting crates asleep; 240 of them (fewer than the store rebuilds for, so they stay
    // in its pending list) put to sleep by hand and woken again, in turns, on one thread.
    let mut app = physics_app();
    app.world_mut()
        .resource_mut::<PhysicsSettings>()
        .parallel_threshold = usize::MAX;
    for (_, ground) in ground_places(&app, &table, 2000, 96, 0xC0C0) {
        app.world_mut().spawn((
            RigidBody::Dynamic,
            Collider::standing(0.5, 0.5),
            Teleport(ground),
        ));
    }
    run_steps(&mut app, 256);
    let stats = *app.world().resource::<PhysicsStats>();
    assert!(stats.sleeping >= 1900, "{stats:?}");
    let churn: Vec<Entity> = {
        let world = app.world_mut();
        let mut asleep = world.query_filtered::<Entity, With<Sleeping>>();
        asleep.iter(world).take(240).collect()
    };
    let mut t = 0u64;
    group.bench_function("sleep_churn", |b| {
        b.iter(|| {
            t += 1;
            let world = app.world_mut();
            for &entity in &churn {
                if t.is_multiple_of(2) {
                    world.entity_mut(entity).insert(Sleeping);
                } else {
                    world.entity_mut(entity).remove::<Sleeping>();
                }
            }
            world.run_schedule(FixedPostUpdate);
        });
    });
    drop(app);

    // 1000 arrows over the forest among 100 walkers, shot again every 32 steps.
    let mut app = physics_app();
    spawn_spread(&mut app, &table, 100, 96, 10, 0xB0B);
    for (r, ground) in ground_places(&app, &table, 1000, 80, 0xA77) {
        let unit = |bits: u64| (bits % 2001) as f64 / 1000.0 - 1.0;
        let velocity = DVec3::new(
            unit(r) * 45.0,
            ((r >> 22) % 21) as f64 - 10.0,
            unit(r >> 11) * 45.0,
        );
        let from = ground.offset(DVec3::new(0.0, 15.0, 0.0));
        app.world_mut().spawn((
            RigidBody::Dynamic,
            Projectile::arrow(),
            Collider::aabb(DVec3::splat(0.05)),
            BodySettings::arrow(),
            Interpolated::default(),
            Teleport(from),
            Velocity(velocity),
            Shot { from, velocity },
        ));
    }
    run_steps(&mut app, 16);
    let stats = *app.world().resource::<PhysicsStats>();
    println!(
        "physics bench: scale/1000_projectiles: bodies={} projectiles={} hits={} frozen={}",
        stats.bodies, stats.projectiles, stats.hits, stats.frozen
    );
    assert_eq!(stats.frozen, 0, "{stats:?}");
    breakdown(&mut app, "scale/1000_projectiles");
    let mut t = 0u64;
    let mut hits = 0;
    group.bench_function("1000_projectiles", |b| {
        b.iter(|| {
            t += 1;
            if t.is_multiple_of(32) {
                shoot_again(&mut app);
            }
            if t.is_multiple_of(64) {
                turn(&mut app, t);
            }
            app.world_mut().run_schedule(FixedPostUpdate);
            hits += app.world().resource::<PhysicsStats>().hits;
        });
    });
    println!("physics bench: scale/1000_projectiles: {hits} hits in {t} steps");
    // The target is for one thread.
    app.world_mut()
        .resource_mut::<PhysicsSettings>()
        .parallel_threshold = usize::MAX;
    breakdown(&mut app, "scale/1000_projectiles_one_thread");
    group.bench_function("1000_projectiles_one_thread", |b| {
        b.iter(|| {
            t += 1;
            if t.is_multiple_of(32) {
                shoot_again(&mut app);
            }
            if t.is_multiple_of(64) {
                turn(&mut app, t);
            }
            app.world_mut().run_schedule(FixedPostUpdate);
        });
    });
    group.finish();
}

/// Benches with fences, last so that the others set up as before they existed: a sweep, a
/// step-up, a collision ray and a tick of 1000 bodies in worlds with a fence in every chunk
/// (off their paths, [`fence_every_chunk`]), where the cells under what they cross are read.
fn fenced_benches(c: &mut Criterion) {
    let table = CollisionTable::from_registry(&BlockRegistry::default_blocks());
    let cfg = SweepConfig::default();
    let fence = BlockRegistry::default_blocks()
        .parse_state("boxel:oak_fence")
        .expect("fence");
    let forest = streamed_with(
        Arc::new(ForestGenerator::default()),
        &[(DVec3::new(0.0, 70.0, 0.0), 2, 2)],
        true,
    );
    let ground = surface(&forest, &table, 0.5, 0.5);
    let mut sweep = c.benchmark_group("sweep");
    // As `player_walk`, `player_land` and `player_fall`.
    let on_ground = WorldPos::from_dvec3(DVec3::new(0.5, ground, 0.5));
    let walk = DVec3::new(4.3 / 64.0, -32.0 / 4096.0, 1.1 / 64.0);
    let above = WorldPos::from_dvec3(DVec3::new(0.5, ground + 0.3, 0.5));
    let high = WorldPos::from_dvec3(DVec3::new(0.5, ground + 10.0, 0.5));
    let down = DVec3::new(0.0, -0.5, 0.0);
    for (name, start, d) in [
        ("player_walk_fenced", on_ground, walk),
        ("player_land_fenced", above, down),
        ("player_fall_fenced", high, down),
    ] {
        sweep.bench_function(name, |b| {
            b.iter(|| {
                black_box(move_box(
                    &mut forest.source(),
                    &table,
                    black_box(start),
                    player(),
                    black_box(d),
                    &[],
                    &cfg,
                ))
            });
        });
    }
    // As `step_wall`, with a fence beside the wall (in its chunk, off the path).
    let mut wall = TestWorld::new();
    wall.fill(
        BlockPos::new(-4, -1, -4),
        BlockPos::new(4, -1, 4),
        BlockState(1),
    );
    wall.fill(
        BlockPos::new(1, 0, -4),
        BlockPos::new(1, 0, 4),
        BlockState(1),
    );
    wall.set(BlockPos::new(4, 0, 4), fence);
    let touching = WorldPos::from_dvec3(DVec3::new(0.7, 0.0, 0.5));
    let step = StepUp {
        height: 0.6,
        grounded: true,
    };
    let d = DVec3::new(4.3 / 64.0, -32.0 / 4096.0, 0.0);
    assert!(
        move_box_stepping(&mut wall, &table, touching, player(), d, &[], &cfg, step)
            .is_ok_and(|out| out.touched(boxel_voxel::Face::PosX))
    );
    sweep.bench_function("step_wall_fenced", |b| {
        b.iter(|| {
            black_box(move_box_stepping(
                &mut wall,
                &table,
                black_box(touching),
                player(),
                black_box(d),
                &[],
                &cfg,
                step,
            ))
        });
    });
    sweep.finish();
    drop(forest);

    // As `sky_1024`.
    let sky = streamed_with(
        Arc::new(ForestGenerator::default()),
        &[(DVec3::new(0.0, 200.0, 0.0), 17, 0)],
        true,
    );
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
        "the fenced sky ray stays in loaded air"
    );
    let mut ray = c.benchmark_group("raycast");
    ray.bench_function("sky_1024_fenced", |b| {
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
    drop(sky);

    // As `tick/1000`, after the same settling.
    let mut app = physics_app();
    fence_every_chunk(&mut app);
    app.world_mut()
        .resource_mut::<PhysicsSettings>()
        .parallel_threshold = usize::MAX;
    spawn_bodies(&mut app, &table, 1000);
    for t in 0..256u64 {
        if t.is_multiple_of(64) {
            turn(&mut app, t);
        }
        app.world_mut().run_schedule(FixedPostUpdate);
    }
    let mut tick = c.benchmark_group("tick");
    let mut t = 0u64;
    tick.bench_function("1000_fenced", |b| {
        b.iter(|| {
            t += 1;
            if t.is_multiple_of(64) {
                turn(&mut app, t);
            }
            app.world_mut().run_schedule(FixedPostUpdate);
        });
    });
    tick.finish();
    let stats = *app.world().resource::<PhysicsStats>();
    println!(
        "physics bench: tick/1000_fenced: bodies={} dynamic={} frozen={}",
        stats.bodies, stats.dynamic, stats.frozen
    );
    assert_eq!(stats.frozen, 0, "every body is on loaded ground");
}

criterion_group!(
    physics,
    benches,
    tick_benches,
    grid_benches,
    scale_benches,
    fenced_benches
);
criterion_main!(physics);
