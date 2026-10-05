//! Headless world streaming: generates and meshes the default terrain around one viewer with no
//! GPU and no window, then prints the world counters.
//!
//! `cargo run --release --example headless_world`
//!
//! Output (stdout, one line): `chunks=N ready=N meshed=N empty=N quads=N elapsed=S frames=N`,
//! where `quads` is the sum of the ready mesh quads. Exit codes: 0 settled, 4 not settled within
//! 30 s. Set `RUST_LOG` for more logging (default filter `warn`).
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

use boxel::log::LogPlugin;
use boxel::prelude::*;
use boxel::world::WorldStats;
use std::time::{Duration, Instant};

/// Wall-clock limit for settling (exit code 4 past it).
const TIMEOUT: Duration = Duration::from_secs(30);

/// Exit code for a timeout.
const EXIT_TIMEOUT: u8 = 4;

/// Viewer position.
const VIEWER: DVec3 = DVec3::new(0.0, 80.0, 0.0);

/// Viewer mesh radius in chunks.
const RADIUS: u32 = 4;

/// When the app was built (the elapsed time includes plugin setup).
#[derive(Resource)]
struct Started(Instant);

fn main() -> AppExit {
    App::new()
        .add_plugins((
            LogPlugin {
                filter: "warn".to_string(),
            },
            MinimalPlugins,
            FrameStatsPlugin,
            WorldPlugin::default(),
        ))
        .insert_resource(Started(Instant::now()))
        .add_systems(Startup, spawn_viewer)
        .add_systems(PreUpdate, exit_when_settled)
        .run()
}

fn spawn_viewer(mut commands: Commands) {
    commands.spawn((
        Transform::from_translation(VIEWER),
        ChunkViewer { radius: RADIUS },
    ));
}

/// Runs before the world systems, so it sees the counters the world published in `Last` of the
/// previous frame, and the chunk map in the same state.
fn exit_when_settled(
    started: Res<Started>,
    stats: Res<WorldStats>,
    map: Res<ChunkMap>,
    frames: Res<FrameStats>,
    mut exit: MessageWriter<AppExit>,
) {
    let elapsed = started.0.elapsed();
    if stats.chunks > 0 && stats.is_settled() {
        let quads: u64 = map
            .ready_meshes()
            .map(|(_, _, mesh)| u64::from(mesh.total_quads()))
            .sum();
        println!(
            "chunks={} ready={} meshed={} empty={} quads={} elapsed={:.3}s frames={}",
            stats.chunks,
            stats.data_ready,
            stats.mesh_ready,
            stats.mesh_empty,
            quads,
            elapsed.as_secs_f64(),
            frames.frame,
        );
        exit.write(AppExit::Success);
    } else if elapsed > TIMEOUT {
        eprintln!(
            "headless_world: not settled after {} s: {:?}",
            TIMEOUT.as_secs(),
            *stats
        );
        exit.write(AppExit::from_code(EXIT_TIMEOUT));
    }
}
