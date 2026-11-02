//! Random ticks with and without the light read (ADR 0041, ADR 0059): one frame of a game tick
//! of random ticks over a field of plants, every pick a plant whose handler reads the block
//! under it, as a crop reads its soil. `plain` stops there; `light` also reads the plant's
//! light ([`boxel_world::TickContext::light`]) and checks a level of 9, as a crop does. Neither
//! writes, so every frame is the same. 25 chunks of plants (radius 2 around the viewer, one
//! layer) at 1024 picks per chunk: about 25,600 handler runs per frame. The rest of the frame
//! (a settled world) is the same in both.
//!
//! `cargo bench -p boxel_world --bench ticks` (release; never while something compiles).

use boxel_app::App;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey, LocalPos, testing};
use boxel_world::jobs::InlineExecutor;
use boxel_world::{
    BlockTickSettings, BlockTicks, BlockTicksPlugin, ChunkViewer, RandomTickHandler, TickContext,
    WorldPlugin, WorldStats,
};
use boxel_worldgen::WorldGenerator;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Chunks at y = 0: solid at local y 0, plants on every cell above.
struct Field;

impl WorldGenerator for Field {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let mut data = ChunkData::uniform(BlockState::AIR);
        if key.pos.y == 0 {
            let solid = registry.by_name("test:solid").expect("solid");
            let plant = registry.by_name("test:plant").expect("plant");
            data = ChunkData::uniform(plant);
            for z in 0..32 {
                for x in 0..32 {
                    data.set(LocalPos::new(x, 0, z), solid);
                }
            }
        }
        data
    }
    fn vertical_chunks(&self) -> std::ops::RangeInclusive<i64> {
        -1..=1
    }
    fn name(&self) -> &str {
        "field"
    }
}

/// Reads the block under the plant (a crop's soil).
fn plain(cx: &mut TickContext<'_>, pos: BlockPos, _: BlockState) {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    black_box(cx.get(below));
}

/// [`plain`], then the plant's light against a crop's 9.
fn light(cx: &mut TickContext<'_>, pos: BlockPos, _: BlockState) {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    black_box(cx.get(below));
    black_box(cx.light(pos).is_some_and(|l| l.raw(0) >= 9));
}

/// A settled world of [`Field`] with `tick` on the plants, the clock stopped.
fn field(tick: fn(&mut TickContext<'_>, BlockPos, BlockState)) -> App {
    let registry = Arc::new(testing::registry());
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(Field),
        registry: Arc::clone(&registry),
        executor: Arc::new(InlineExecutor),
        ..WorldPlugin::default()
    })
    .add_plugins(BlockTicksPlugin);
    {
        let mut settings = app.world_mut().resource_mut::<BlockTickSettings>();
        settings.random_tick_radius = [2, 0];
        settings.random_ticks_per_chunk = 1024;
        settings.paused = true;
    }
    let plant = registry.block_by_name("test:plant").expect("plant");
    app.world_mut()
        .resource_mut::<BlockTicks>()
        .register_random(
            &registry,
            plant,
            RandomTickHandler {
                reach: [0, 1, 0],
                tick,
            },
        );
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(16.0, 8.0, 16.0)),
        ChunkViewer::new(4).with_vertical_radius(1),
    ));
    let start = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "not settled");
    }
    app
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("random_ticks");
    type Tick = fn(&mut TickContext<'_>, BlockPos, BlockState);
    let ticks: [(&str, Tick); 2] = [("plain", plain), ("light", light)];
    for (name, tick) in ticks {
        let mut app = field(tick);
        group.bench_function(name, |b| {
            b.iter(|| {
                app.world_mut().resource_mut::<BlockTicks>().advance(1);
                app.update();
            });
        });
        let ran = app.world().resource::<BlockTicks>().stats().random;
        assert!(ran > 20_000, "{ran} handler runs per frame");
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
