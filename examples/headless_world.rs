//! Headless world streaming: generates and meshes the default terrain around one viewer with no
//! GPU and no window, then prints the world counters.
//!
//! `cargo run --release --example headless_world -- [options]` (see `--help`).
//!
//! Output (stdout): once settled, `chunks=N ready=N meshed=N empty=N quads=N elapsed=S frames=N
//! max_update_ms=X p99_update_ms=X`, where `quads` is the sum of the ready mesh quads and the
//! update times are the longest and 99th percentile `App::update` while loading. Then, in this
//! order and only when asked for:
//! - `--idle`: `idle: frames=N median_update_ms=X p99_update_ms=X max_update_ms=X` while the
//!   viewer stands still.
//! - `--speed`: `walk: moved=V voxels in S s, frames=N max_update_ms=X p99_update_ms=X, behind=N,
//!   caught_up_after=S`, where `behind` counts the jobs and interest work left when the walk
//!   stops.
//! - `--jump`: `jump: chunks=N settled_after=S frames=N max_update_ms=X p99_update_ms=X` from the
//!   frame of the teleport until the new area is settled.
//!
//! Exit codes: 0 settled, 1 usage error, 4 not settled within the timeout. Set `RUST_LOG` for
//! more logging (default filter `warn`).
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

use boxel::log::LogPlugin;
use boxel::prelude::*;
use boxel::voxel::CHUNK_SIZE;
use boxel::world::{WorldStats, WorldSystems};
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: headless_world [options]
  --view-distance N    horizontal mesh radius in chunks (default 4)
  --vertical-radius N  vertical mesh radius in chunks (default 8)
  --fixed-height       stream every layer of the generator and nothing else (boxel M0 streaming)
  --hz N               frames per second of the loop (default 62.5; 0 = no wait between frames)
  --idle S             once settled, stand still for S seconds and report the update times
  --speed V            then move the viewer along +x at V voxels/s
  --duration S         walk duration in seconds with --speed (default 5)
  --jump N             then teleport the viewer N chunks along +x and wait for the new area
  --timeout S          wall-clock limit in seconds (default 60), exit 4 past it
  --help               this text";

/// Exit code for a timeout.
const EXIT_TIMEOUT: u8 = 4;

/// Viewer position.
const VIEWER: DVec3 = DVec3::new(0.0, 80.0, 0.0);

#[derive(Clone, Debug)]
struct Options {
    radius: u32,
    vertical_radius: u32,
    fixed_height: bool,
    hz: f64,
    speed: f64,
    duration: Duration,
    idle: Duration,
    jump: u32,
    timeout: Duration,
}

/// `Ok(None)` for `--help`.
fn parse_options(args: &[String]) -> Result<Option<Options>, String> {
    let mut o = Options {
        radius: 4,
        vertical_radius: ChunkViewer::DEFAULT_VERTICAL_RADIUS,
        fixed_height: false,
        hz: 62.5,
        speed: 0.0,
        duration: Duration::from_secs(5),
        idle: Duration::ZERO,
        jump: 0,
        timeout: Duration::from_secs(60),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--help" | "-h" => return Ok(None),
            "--view-distance" => o.radius = radius(flag, &value()?)?,
            "--vertical-radius" => o.vertical_radius = radius(flag, &value()?)?,
            "--fixed-height" => o.fixed_height = true,
            "--hz" => o.hz = number(flag, &value()?)?,
            "--speed" => o.speed = number(flag, &value()?)?,
            "--duration" => o.duration = Duration::from_secs_f64(number(flag, &value()?)?),
            "--idle" => o.idle = Duration::from_secs_f64(number(flag, &value()?)?),
            "--jump" => {
                o.jump = value()?
                    .parse()
                    .map_err(|_| format!("{flag}: not a chunk count"))?;
            }
            "--timeout" => o.timeout = Duration::from_secs_f64(number(flag, &value()?)?),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Some(o))
}

fn radius(flag: &str, value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|r| *r <= 64)
        .ok_or_else(|| format!("{flag}: {value:?} is not in 0..=64"))
}

fn number(flag: &str, value: &str) -> Result<f64, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| format!("{flag}: {value:?} is not a non-negative number"))
}

/// What the run is doing.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// Loading the first area.
    Loading,
    /// Standing still since `since`.
    Idle { since: Instant },
    /// Walking since `since`.
    Walking { since: Instant },
    /// Walk over at `since` with `behind` work left; waiting to settle again.
    CatchingUp { since: Instant, behind: usize },
    /// Teleported at `since`; waiting for the new area.
    Jumping { since: Instant },
}

#[derive(Resource)]
struct Run {
    options: Options,
    started: Instant,
    phase: Phase,
    /// Start of the current `App::update`.
    update_start: Option<Instant>,
    /// Update durations of the current phase (ms).
    updates: Vec<f64>,
    /// Longest update so far (ms).
    max_update: f64,
    /// Viewer x when the walk started.
    walk_from: f64,
    walk_report: Option<String>,
    /// Voxels to move the viewer along x in this frame (a jump).
    teleport: Option<f64>,
}

impl Run {
    fn exit(&self, code: u8, exit: &mut MessageWriter<AppExit>) {
        exit.write(AppExit::from_code(code));
    }

    /// Starts the phase after `done` that was asked for (idle, walk, jump), or exits.
    fn next_phase(&mut self, done: Phase, now: Instant, x: f64, exit: &mut MessageWriter<AppExit>) {
        let o = &self.options;
        let rank = |p: &Phase| match p {
            Phase::Loading => 0,
            Phase::Idle { .. } => 1,
            Phase::Walking { .. } | Phase::CatchingUp { .. } => 2,
            Phase::Jumping { .. } => 3,
        };
        let candidates = [
            (o.idle > Duration::ZERO).then_some(Phase::Idle { since: now }),
            (o.speed > 0.0).then_some(Phase::Walking { since: now }),
            (o.jump > 0).then_some(Phase::Jumping { since: now }),
        ];
        let Some(next) = candidates
            .into_iter()
            .flatten()
            .find(|p| rank(p) > rank(&done))
        else {
            self.exit(0, exit);
            return;
        };
        match next {
            Phase::Walking { .. } => self.walk_from = x,
            Phase::Jumping { .. } => {
                self.teleport = Some(f64::from(o.jump) * f64::from(CHUNK_SIZE));
            }
            _ => {}
        }
        self.updates.clear();
        self.phase = next;
    }
}

/// Median of `values` (0 when empty).
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.get(sorted.len() / 2).copied().unwrap_or(0.0)
}

/// Longest of `values` (0 when empty).
fn max(values: &[f64]) -> f64 {
    values.iter().copied().fold(0.0, f64::max)
}

fn main() -> AppExit {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_options(&args) {
        Ok(Some(o)) => o,
        Ok(None) => {
            println!("{USAGE}");
            return AppExit::Success;
        }
        Err(message) => {
            eprintln!("headless_world: {message}\n\n{USAGE}");
            return AppExit::from_code(1);
        }
    };
    let generator = TerrainGenerator::default();
    let settings = WorldSettings {
        vertical_bounds: options.fixed_height.then(|| generator.vertical_chunks()),
        ..WorldSettings::default()
    };
    let wait = if options.hz > 0.0 {
        Duration::from_secs_f64(1.0 / options.hz)
    } else {
        Duration::ZERO
    };
    App::new()
        .add_plugins((
            LogPlugin {
                filter: "warn".to_string(),
            },
            MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(wait)),
            FrameStatsPlugin,
            WorldPlugin {
                generator: std::sync::Arc::new(generator),
                settings,
                ..WorldPlugin::default()
            },
        ))
        .insert_resource(Run {
            options,
            started: Instant::now(),
            phase: Phase::Loading,
            update_start: None,
            updates: Vec::new(),
            max_update: 0.0,
            walk_from: VIEWER.x,
            walk_report: None,
            teleport: None,
        })
        .add_systems(Startup, spawn_viewer)
        .add_systems(First, start_update)
        .add_systems(PreUpdate, drive)
        .add_systems(Update, move_viewer.before(WorldSystems::Interest))
        .add_systems(Last, end_update)
        .run()
}

fn spawn_viewer(mut commands: Commands, run: Res<Run>) {
    let o = &run.options;
    let vertical = if o.fixed_height {
        u32::MAX
    } else {
        o.vertical_radius
    };
    commands.spawn((
        Transform::from_translation(VIEWER),
        ChunkViewer::new(o.radius).with_vertical_radius(vertical),
    ));
}

fn start_update(mut run: ResMut<Run>) {
    run.update_start = Some(Instant::now());
}

fn end_update(mut run: ResMut<Run>) {
    if let Some(start) = run.update_start.take() {
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        run.updates.push(ms);
        run.max_update = run.max_update.max(ms);
    }
}

/// 99th percentile of `values` (0 when empty).
fn p99(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
        .get(sorted.len().saturating_sub(1) * 99 / 100)
        .copied()
        .unwrap_or(0.0)
}

/// Moves the viewer along +x while walking, and teleports it for a jump.
fn move_viewer(
    mut run: ResMut<Run>,
    time: Res<Time>,
    mut viewers: Query<&mut Transform, With<ChunkViewer>>,
) {
    let mut dx = run.teleport.take().unwrap_or(0.0);
    if matches!(run.phase, Phase::Walking { .. }) {
        dx += run.options.speed * time.delta().as_secs_f64();
    }
    if dx != 0.0 {
        for mut transform in &mut viewers {
            transform.translation.x += dx;
        }
    }
}

/// Runs before the world systems, so it sees the counters the world published in `Last` of the
/// previous frame, and the chunk map in the same state.
fn drive(
    mut run: ResMut<Run>,
    stats: Res<WorldStats>,
    map: Res<ChunkMap>,
    frames: Res<FrameStats>,
    viewers: Query<&Transform, With<ChunkViewer>>,
    mut exit: MessageWriter<AppExit>,
) {
    let now = Instant::now();
    let elapsed = run.started.elapsed();
    let settled = stats.chunks > 0 && stats.is_settled();
    let x = viewers.iter().next().map_or(VIEWER.x, |t| t.translation.x);
    match run.phase {
        Phase::Loading if settled => {
            let quads: u64 = map
                .ready_meshes()
                .map(|(_, _, mesh)| u64::from(mesh.total_quads()))
                .sum();
            println!(
                "chunks={} ready={} meshed={} empty={} quads={} elapsed={:.3}s frames={} \
                 max_update_ms={:.2} p99_update_ms={:.2}",
                stats.chunks,
                stats.data_ready,
                stats.mesh_ready,
                stats.mesh_empty,
                quads,
                elapsed.as_secs_f64(),
                frames.frame,
                run.max_update,
                p99(&run.updates),
            );
            run.next_phase(Phase::Loading, now, x, &mut exit);
        }
        Phase::Idle { since } if now.duration_since(since) >= run.options.idle => {
            println!(
                "idle: frames={} median_update_ms={:.3} p99_update_ms={:.3} max_update_ms={:.3}",
                run.updates.len(),
                median(&run.updates),
                p99(&run.updates),
                max(&run.updates),
            );
            run.next_phase(Phase::Idle { since }, now, x, &mut exit);
        }
        Phase::Walking { since } if now.duration_since(since) >= run.options.duration => {
            let behind = stats.queued_jobs
                + stats.in_flight
                + stats.generating
                + stats.meshing
                + stats.interest_backlog;
            run.walk_report = Some(format!(
                "walk: moved={:.0} voxels in {:.1} s, frames={} max_update_ms={:.2} \
                 p99_update_ms={:.2}, behind={behind}",
                x - run.walk_from,
                now.duration_since(since).as_secs_f64(),
                run.updates.len(),
                max(&run.updates),
                p99(&run.updates),
            ));
            run.phase = Phase::CatchingUp { since: now, behind };
        }
        Phase::CatchingUp { since, behind } if settled => {
            let report = run.walk_report.take().unwrap_or_default();
            println!(
                "{report}, caught_up_after={:.3}s",
                now.duration_since(since).as_secs_f64()
            );
            run.next_phase(Phase::CatchingUp { since, behind }, now, x, &mut exit);
        }
        // The counters here are from the end of the previous frame, which already planned the
        // teleport: the new area is settled.
        Phase::Jumping { since } if settled => {
            println!(
                "jump: chunks={} settled_after={:.3}s frames={} max_update_ms={:.2} \
                 p99_update_ms={:.2}",
                run.options.jump,
                now.duration_since(since).as_secs_f64(),
                run.updates.len(),
                max(&run.updates),
                p99(&run.updates),
            );
            run.next_phase(Phase::Jumping { since }, now, x, &mut exit);
        }
        _ => {}
    }
    if elapsed > run.options.timeout {
        eprintln!(
            "headless_world: not done after {:.0} s ({:?}): {:?}",
            run.options.timeout.as_secs_f64(),
            run.phase,
            *stats
        );
        run.exit(EXIT_TIMEOUT, &mut exit);
    }
}
