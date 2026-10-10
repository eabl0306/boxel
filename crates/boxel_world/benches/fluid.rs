//! Flowing fluid throughput (ADR 0021, ADR 0025): how many fluid ticks fit in the default 1 ms
//! tick budget of a frame, during a flood (400 sources on a flat plain with pits). The clock
//! jumps ahead once the sources are placed, so the queue stays behind until the flood ends: every
//! tick of the flood counts, the same ticks in the same order on any version (a late tick runs as
//! if on time). Each scene floods [`FLOODS`] times, each in a new world. Prints, per scene, the
//! frames and block writes of one flood, and the mean ticks per frame while the queue is behind,
//! over all the floods and the lowest and highest of one flood (how much it moves in one run).
//!
//! - `water`: plain water among full cubes and air (the common case).
//! - `lava`: lava and water sources side by side, so the lava flows and hardens where they meet.
//! - `waterlogged`: the sources are waterlogged slabs and fences, with dry ones in the way.
//!
//! `cargo bench -p boxel_world --bench fluid` runs the three; `-- water` (or `lava`,
//! `waterlogged`) only those named (release; never while something compiles).
#![expect(clippy::print_stdout, reason = "the bench prints its result")]

use boxel_app::App;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState};
use boxel_world::jobs::TaskPoolExecutor;
use boxel_world::{
    BlockTicks, ChunkViewer, EditQueue, FluidPlugin, WorldPlugin, WorldSettings, WorldStats,
};
use boxel_worldgen::FlatGenerator;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The scenes, by name.
const SCENES: [&str; 3] = ["water", "lava", "waterlogged"];
/// Floods per scene: one is a few hundred frames, too few for a steady mean.
const FLOODS: usize = 10;
/// Game ticks the clock jumps once the sources are placed: past every tick of the flood.
const AHEAD: u64 = 1 << 20;

/// What one flood did.
struct Flood {
    frames: u64,
    written: u64,
    /// Frames that left due ticks for the next one (they used the whole budget).
    behind_frames: u64,
    /// Ticks run in those frames.
    ran_behind: u64,
}

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

/// The source of grid cell `(i, j)` in `scene`, and the dry blocks placed around it (offsets).
fn sources(registry: &BlockRegistry, scene: &str, i: i64, j: i64) -> Vec<([i64; 3], BlockState)> {
    let state = |text: &str| registry.parse_state(text).expect(text);
    let even = (i + j) % 2 == 0;
    match scene {
        "lava" if even => vec![([0, 0, 0], state("boxel:lava"))],
        "waterlogged" => {
            let source = if even {
                "boxel:stone_slab[type=bottom,waterlogged=true]"
            } else {
                "boxel:oak_fence[waterlogged=true]"
            };
            vec![
                ([0, 0, 0], state(source)),
                // dry blocks in the way: a fence and a top slab the water flows around and
                // under, a bottom slab it flows over
                ([1, 0, 2], state("boxel:oak_fence")),
                ([-2, 0, 0], state("boxel:stone_slab[type=top]")),
                ([0, 0, -3], state("boxel:stone_slab[type=bottom]")),
            ]
        }
        _ => vec![([0, 0, 0], state("boxel:water"))],
    }
}

fn flood(registry: &Arc<BlockRegistry>, scene: &str) -> Flood {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(FlatGenerator::new(vec![
            ("boxel:stone".to_string(), 3),
            ("boxel:grass".to_string(), 1),
        ])),
        registry: Arc::clone(registry),
        executor: Arc::new(TaskPoolExecutor),
        settings: WorldSettings::default(),
    })
    .add_plugins(FluidPlugin);
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(0.0, 4.0, 0.0)),
        ChunkViewer::new(5).with_vertical_radius(1),
    ));
    settle(&mut app);
    {
        let mut edits = app.world_mut().resource_mut::<EditQueue>();
        for i in 0..20i64 {
            for j in 0..20i64 {
                let (x, z) = (i * 7 - 70, j * 7 - 70);
                for ([dx, dy, dz], state) in sources(registry, scene, i, j) {
                    edits.set_block(BlockPos::new(x + dx, 4 + dy, z + dz), state);
                }
                // a pit next to every other source: falling water and the way down
                if (i + j) % 2 == 0 {
                    edits.set_block(BlockPos::new(x + 3, 3, z), BlockState::AIR);
                    edits.set_block(BlockPos::new(x + 3, 2, z), BlockState::AIR);
                }
            }
        }
    }
    app.update();
    app.world_mut().resource_mut::<BlockTicks>().advance(AHEAD);
    let (mut frames, mut behind_frames, mut ran_behind, mut written) = (0u64, 0u64, 0u64, 0u64);
    loop {
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
    Flood {
        frames,
        written,
        behind_frames,
        ran_behind,
    }
}

fn main() {
    let registry = Arc::new(BlockRegistry::default_blocks());
    let named: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| SCENES.contains(&a.as_str()))
        .collect();
    for scene in SCENES {
        if !named.is_empty() && !named.iter().any(|n| n == scene) {
            continue;
        }
        let start = Instant::now();
        let floods: Vec<Flood> = (0..FLOODS).map(|_| flood(&registry, scene)).collect();
        let per_budget = |f: &Flood| f.ran_behind as f64 / f.behind_frames.max(1) as f64;
        let (low, high) = floods
            .iter()
            .map(per_budget)
            .fold((f64::MAX, 0.0f64), |(l, h), x| (l.min(x), h.max(x)));
        let ran: u64 = floods.iter().map(|f| f.ran_behind).sum();
        let behind: u64 = floods.iter().map(|f| f.behind_frames).sum();
        let first = &floods[0];
        let label = if scene == "water" {
            String::new()
        } else {
            format!(" ({scene})")
        };
        println!(
            "fluid flood{label}: {} frames, {} blocks written; {FLOODS} floods ({:.1} s), {behind} \
             frames behind the clock, {:.0} ticks per 1 ms budget while behind (one flood: {low:.0} \
             to {high:.0})",
            first.frames,
            first.written,
            start.elapsed().as_secs_f64(),
            ran as f64 / behind.max(1) as f64,
        );
    }
}
