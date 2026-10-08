//! Flowing water throughput (ADR 0021): how many water ticks fit in the default 1 ms tick budget
//! of a frame, during a flood (400 sources on a flat plain with pits). Prints the mean ticks per
//! frame while the queue is behind, the block writes per frame and the frames the flood took.
//!
//! `cargo bench -p boxel_world --bench fluid` (release; never while something compiles).
#![expect(clippy::print_stdout, reason = "the bench prints its result")]

use boxel_app::App;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry};
use boxel_world::jobs::TaskPoolExecutor;
use boxel_world::{
    BlockTicks, ChunkViewer, EditQueue, FluidPlugin, WorldPlugin, WorldSettings, WorldStats,
};
use boxel_worldgen::FlatGenerator;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn settle(app: &mut App) {
    let start = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "not settled");
    }
}

fn main() {
    let registry = Arc::new(BlockRegistry::default_blocks());
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(FlatGenerator::new(vec![
            ("boxel:stone".to_string(), 3),
            ("boxel:grass".to_string(), 1),
        ])),
        registry: Arc::clone(&registry),
        executor: Arc::new(TaskPoolExecutor),
        settings: WorldSettings::default(),
    })
    .add_plugins(FluidPlugin);
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(0.0, 4.0, 0.0)),
        ChunkViewer::new(5).with_vertical_radius(1),
    ));
    settle(&mut app);
    let water = registry.by_name("boxel:water").expect("water");
    {
        let mut edits = app.world_mut().resource_mut::<EditQueue>();
        for i in 0..20i64 {
            for j in 0..20i64 {
                let (x, z) = (i * 7 - 70, j * 7 - 70);
                edits.set_block(BlockPos::new(x, 4, z), water);
                // a pit next to every other source: falling water and the way down
                if (i + j) % 2 == 0 {
                    edits.set_block(BlockPos::new(x + 3, 3, z), boxel_voxel::BlockState::AIR);
                    edits.set_block(BlockPos::new(x + 3, 2, z), boxel_voxel::BlockState::AIR);
                }
            }
        }
    }
    app.update();
    let (mut frames, mut behind_frames, mut ran_behind, mut written) = (0u64, 0u64, 0u64, 0u64);
    let start = Instant::now();
    loop {
        app.world_mut().resource_mut::<BlockTicks>().advance(1);
        app.update();
        frames += 1;
        let stats = app.world().resource::<BlockTicks>().stats();
        written += stats.written as u64;
        if stats.behind > 0 {
            behind_frames += 1;
            ran_behind += stats.ran as u64;
        }
        if stats.queued == 0 || frames > 20_000 {
            break;
        }
    }
    println!(
        "fluid flood: {frames} frames ({:.1} s), {written} blocks written, {behind_frames} frames \
         behind the clock, {:.0} ticks per 1 ms budget while behind",
        start.elapsed().as_secs_f64(),
        ran_behind as f64 / behind_frames.max(1) as f64
    );
}
