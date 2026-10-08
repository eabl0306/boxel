//! Headless world streaming: generates and meshes a world (the default terrain, or `--world`)
//! around one viewer with no GPU and no window, then prints the world counters.
//!
//! `cargo run --release --example headless_world -- [options]` (see `--help`).
//!
//! Output (stdout): once settled, `chunks=N ready=N meshed=N empty=N quads=N elapsed=S frames=N
//! max_update_ms=X p99_update_ms=X`, where `quads` is the sum of the ready mesh quads and the
//! update times are the longest and 99th percentile `App::update` while loading, and `world:
//! meshed_fraction=X quads_per_mesh_p50=N p99=N max=N voxel_mib=X`: the share of the meshed
//! chunks with a non-empty mesh, the quads of those meshes, and the heap bytes of the voxel data;
//! `light: ao=MODE palette_p50=N p99=N max=N requantized=N arena_entries=N`: the AO policy
//! (`--ao`), the light palette entries of the non-empty meshes, how many of them overflowed the
//! palette and were requantized, and the renderer arena entries they take (quads plus palettes,
//! at least one palette entry per mesh); and `sky: light=on|off full=N fast=N columns=N
//! walked=N walks=N private_generations=N cache_mib=X sky_mib=X`: the light boxes that ran the floods or took a
//! fast path (dark, open sky or under the walk floor), the chunk columns with a sky tile slot
//! and with a walked tile, the walks run and the chunks they generated because they were not
//! loaded, and the memory of the gameplay light cache and of the sky tiles. Compare `quads=`
//! across `--light on|off` and `--ao off|baked|lit-only` for the cost of AO: with `--light off`
//! every face counts as lit (a worst case for `lit-only`).
//! With `--cull`, three more lines describe cave culling (ADR 0022) for a camera at the viewer
//! (`--camera` sets where it is and where it looks; view distance = radius x 32, 16:9):
//! `cull: eye=X,Y,Z yaw=D pitch=D box=N reached=N fresh_ms=X search_ms=X`, the visibility search
//! (the box around the camera, the chunks it reached, a search that also fills the grid, and one
//! with the grid already filled, as in a frame where the camera turned); `draws: off chunks=N
//! buckets=N quads=N on chunks=N buckets=N quads=N hidden: chunks=P% buckets=P% quads=P%`, what
//! every tier draws without and with cave culling (`boxel_render::cull_counts`: frustum, per-face
//! bucket test; a bucket is one draw call on Mid and Low); and, in the overworld, `underground:
//! off quads=N on quads=N hidden=P%`, the same for the chunks entirely under the surface.
//! Then, in this order and only when asked for:
//! - `--idle`: `idle: frames=N median_update_ms=X p99_update_ms=X max_update_ms=X` while the
//!   viewer stands still.
//! - `--speed`: `walk: moved=V voxels in S s, frames=N max_update_ms=X p99_update_ms=X, behind=N,
//!   caught_up_after=S`, where `behind` counts the jobs and interest work left when the walk
//!   stops.
//! - `--jump`: `jump: chunks=N settled_after=S frames=N max_update_ms=X p99_update_ms=X
//!   center=X,Y,Z` from the frame of the teleport until the new area is settled; `center` is the
//!   viewer's chunk, and the run fails (exit 5) unless its whole mesh region is loaded there.
//!   World coordinates are `i64`: `--jump 70000000` (past the former `i32` edge of voxels,
//!   `2^26` chunks) or `--jump -40000000000` (past `2^40` voxels) stream at the true position.
//!
//! Exit codes: 0 settled, 1 usage error, 4 not settled within the timeout, 5 the world settled
//! somewhere else than around the viewer after a jump. Set `RUST_LOG` for more logging (default
//! filter `warn`).
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

#[path = "shared/world.rs"]
mod world;

use boxel::camera::{Camera, CameraView};
use boxel::log::LogPlugin;
use boxel::math::I64Vec3;
use boxel::mesh::ChunkMesh;
use boxel::mesh::{AoPolicy, LightQuantization};
use boxel::prelude::*;
use boxel::render::{DEFAULT_SKY_COLOR, ViewParams, cull_counts};
use boxel::voxel::{CHUNK_SIZE, MAX_BLOCK_COORD, MAX_CHUNK_COORD};
use boxel::world::DataState;
use boxel::world::visibility::visible_chunks;
use boxel::world::{ChunkVisibility, SkyMap, VisibilityView, WorldStats, WorldSystems};
use boxel::worldgen::OverworldGenerator;
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};
use world::{World, WorldChoice};

const USAGE: &str = "\
usage: headless_world [options]
  --view-distance N    horizontal mesh radius in chunks (default 4)
  --vertical-radius N  vertical mesh radius in chunks (default 8)
  --fixed-height       stream every layer of the generator and nothing else (boxel M0 streaming;
                       not with the overworld, which has no floor)
  --viewer-y Y         viewer height in voxels (default 80; -1000 is deep in the overworld's
                       caves)
  --camera X,Y,Z,YAW,PITCH  viewer position and, for --cull, where the camera looks (degrees, as
                       stress: yaw -90 looks along +x); replaces --viewer-y
  --cull               report cave culling for the camera once settled (see the docs above)
  --ao MODE            ambient occlusion baked into the meshes: off, baked (every face) or
                       lit-only (faces with some light; the default)
  --light MODE         on (the default: sky and block light computed and baked) or off (open
                       sky everywhere, the look before light)
  --hz N               frames per second of the loop (default 62.5; 0 = no wait between frames)
  --idle S             once settled, stand still for S seconds and report the update times
  --speed V            then move the viewer along +x at V voxels/s
  --duration S         walk duration in seconds with --speed (default 5)
  --jump N             then teleport the viewer N chunks along x (negative: -x, up to the
                       world limit of 2^56) and wait for the new area
  --timeout S          wall-clock limit in seconds (default 60), exit 4 past it
  --help               this text";

/// The whole help: [`USAGE`] and the world options.
fn usage() -> String {
    format!("{USAGE}\nworld options:\n{}", world::usage(World::Terrain))
}

/// Exit code for a timeout.
const EXIT_TIMEOUT: u8 = 4;
/// Exit code when the world settled away from the viewer after a jump.
const EXIT_OFF_CENTER: u8 = 5;

/// Viewer position (`--viewer-y` replaces y).
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
    /// Chunks along x (`0`: no jump).
    jump: i64,
    timeout: Duration,
    /// The terrain unless `--world` says otherwise (the world of the earlier measurements).
    world: WorldChoice,
    viewer_y: f64,
    ao: AoPolicy,
    light: bool,
    /// `--camera`: x, y, z, yaw, pitch (degrees).
    camera: Option<[f64; 5]>,
    /// `--cull`.
    cull: bool,
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
        world: WorldChoice::with_world(World::Terrain),
        viewer_y: VIEWER.y,
        ao: AoPolicy::default(),
        light: true,
        camera: None,
        cull: false,
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
        if o.world.parse_flag(flag, &mut value)? {
            continue;
        }
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
                let v = value()?;
                o.jump = v
                    .parse::<i64>()
                    .ok()
                    .filter(|n| n.unsigned_abs() <= MAX_CHUNK_COORD.unsigned_abs())
                    .ok_or_else(|| {
                        format!("{flag}: {v:?} is not a chunk count within +-{MAX_CHUNK_COORD}")
                    })?;
            }
            "--timeout" => o.timeout = Duration::from_secs_f64(number(flag, &value()?)?),
            "--ao" => {
                let v = value()?;
                o.ao = AO_MODES
                    .iter()
                    .find(|(name, _)| *name == v)
                    .map(|&(_, policy)| policy)
                    .ok_or_else(|| format!("{flag}: {v:?} is not off, baked or lit-only"))?;
            }
            "--light" => {
                o.light = match value()?.as_str() {
                    "on" => true,
                    "off" => false,
                    v => return Err(format!("{flag}: {v:?} is not on or off")),
                };
            }
            "--cull" => o.cull = true,
            "--camera" => {
                let v = value()?;
                let parts: Vec<f64> = v
                    .split(',')
                    .map(|p| p.trim().parse::<f64>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| format!("{flag}: {v:?} is not x,y,z,yaw,pitch"))?;
                let camera: [f64; 5] = parts
                    .try_into()
                    .map_err(|_| format!("{flag}: {v:?} is not x,y,z,yaw,pitch"))?;
                if camera.iter().any(|c| !c.is_finite())
                    || camera[..3].iter().any(|c| c.abs() > MAX_BLOCK_COORD as f64)
                {
                    return Err(format!("{flag}: {v:?} is outside the world"));
                }
                o.viewer_y = camera[1];
                o.camera = Some(camera);
            }
            "--viewer-y" => {
                let v = value()?;
                o.viewer_y = v
                    .parse::<f64>()
                    .ok()
                    .filter(|y| y.abs() <= MAX_BLOCK_COORD as f64)
                    .ok_or_else(|| {
                        format!("{flag}: {v:?} is not a height within +-{MAX_BLOCK_COORD}")
                    })?;
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    o.world.validate(o.fixed_height)?;
    Ok(Some(o))
}

/// `--ao` values.
const AO_MODES: [(&str, AoPolicy); 3] = [
    ("off", AoPolicy::Off),
    ("baked", AoPolicy::Baked),
    ("lit-only", AoPolicy::BakedLitOnly),
];

/// The `--ao` name of a policy.
fn ao_name(policy: AoPolicy) -> &'static str {
    AO_MODES
        .iter()
        .find(|(_, p)| *p == policy)
        .map_or("?", |(name, _)| name)
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
            (o.jump != 0).then_some(Phase::Jumping { since: now }),
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
                // At most 2^61 voxels (the jump is within the world limit); exact up to 2^53.
                self.teleport = Some((o.jump * i64::from(CHUNK_SIZE)) as f64);
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
            println!("{}", usage());
            return AppExit::Success;
        }
        Err(message) => {
            eprintln!("headless_world: {message}\n\n{}", usage());
            return AppExit::from_code(1);
        }
    };
    let generator = options.world.generator(TerrainGenerator::default().seed);
    let settings = WorldSettings {
        vertical_bounds: options.fixed_height.then(|| generator.vertical_chunks()),
        ao_policy: options.ao,
        light: options.light,
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
                generator,
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
    let at = o
        .camera
        .map_or(DVec3::new(VIEWER.x, o.viewer_y, VIEWER.z), |c| {
            DVec3::new(c[0], c[1], c[2])
        });
    commands.spawn((
        Transform::from_translation(at),
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

/// Positions of the mesh region of a viewer in chunk `center` (columns within `radius`, layers
/// within `vertical` clipped to `bounds`) for which `loaded` is false: empty when the world is
/// loaded around the viewer.
fn missing_around(
    center: ChunkPos,
    radius: u32,
    vertical: u32,
    bounds: Option<&RangeInclusive<i64>>,
    loaded: impl Fn(ChunkPos) -> bool,
) -> Vec<ChunkPos> {
    let (r, v) = (i64::from(radius), i64::from(vertical));
    let mut layers = center.y.saturating_sub(v)..=center.y.saturating_add(v);
    if let Some(b) = bounds {
        layers = *layers.start().max(b.start())..=*layers.end().min(b.end());
    }
    let mut missing = Vec::new();
    for y in layers {
        for dz in -r..=r {
            for dx in -r..=r {
                let p = ChunkPos::new(center.x + dx, y, center.z + dz);
                if dx * dx + dz * dz <= r * r && !loaded(p) {
                    missing.push(p);
                }
            }
        }
    }
    missing
}

/// The `--cull` lines: the visibility search from the camera, and what every tier draws without
/// and with cave culling (and, in the overworld, for the chunks under the surface).
fn cull_lines(o: &Options, map: &ChunkMap, transform: &Transform, viewer: ChunkViewer) -> String {
    let [_, _, _, yaw, pitch] = o.camera.unwrap_or([0.0, 0.0, 0.0, -90.0, 0.0]);
    let camera = Camera {
        view_distance: viewer.radius as f32 * 32.0,
        ..Camera::default()
    };
    let look = |yaw: f64| {
        let t = Transform::from_yaw_pitch(
            transform.translation,
            yaw.to_radians() as f32,
            pitch.to_radians() as f32,
        );
        CameraView::compute(&camera, &t, 16.0 / 9.0)
    };
    let cv = look(yaw);
    let view = ViewParams::from_camera(&cv, &camera, DEFAULT_SKY_COLOR, 0.6);
    let search_view = |cv: &CameraView| {
        VisibilityView::new(
            cv.render_origin,
            Some(cv.frustum),
            viewer.radius,
            viewer.vertical_radius,
        )
    };
    let links = |p: ChunkPos| map.visibility_links_at(p);
    let start = Instant::now();
    let set = visible_chunks(&search_view(&cv), links);
    let fresh = start.elapsed();
    // A frame where the camera turned half a degree: the grid is filled, only the search runs.
    let mut state = ChunkVisibility::default();
    state.update(&search_view(&cv), links);
    let turned = search_view(&look(yaw + 0.5));
    let start = Instant::now();
    state.update(&turned, links);
    let search = start.elapsed();
    let meshes: Vec<(I64Vec3, &ChunkMesh)> = map
        .ready_meshes()
        .map(|(_, key, mesh)| (key.pos.origin().as_i64vec3(), &**mesh))
        .collect();
    let off = cull_counts(&view, meshes.iter().copied(), None);
    let on = cull_counts(&view, meshes.iter().copied(), Some(&set));
    let hidden = |a: u64, b: u64| {
        if a == 0 {
            0.0
        } else {
            100.0 * (a - b) as f64 / a as f64
        }
    };
    let eye = transform.translation;
    let mut out = format!(
        "cull: eye={},{},{} yaw={yaw} pitch={pitch} box={} reached={} fresh_ms={:.3} \
         search_ms={:.3}\ndraws: off chunks={} buckets={} quads={} on chunks={} buckets={} \
         quads={} hidden: chunks={:.1}% buckets={:.1}% quads={:.1}%",
        eye.x,
        eye.y,
        eye.z,
        set.box_len(),
        set.visible_count(),
        fresh.as_secs_f64() * 1e3,
        search.as_secs_f64() * 1e3,
        off.chunks,
        off.buckets,
        off.quads,
        on.chunks,
        on.buckets,
        on.quads,
        hidden(off.chunks.into(), on.chunks.into()),
        hidden(off.buckets.into(), on.buckets.into()),
        hidden(off.quads, on.quads),
    );
    if o.world.world == World::Overworld {
        // Chunks whose top is under the surface of their 4 corner columns and center.
        let heights = OverworldGenerator::default();
        let under = |origin: I64Vec3| {
            let top = origin.y + 32;
            [(0, 0), (31, 0), (0, 31), (31, 31), (16, 16)]
                .iter()
                .all(|&(dx, dz)| heights.height_at(origin.x + dx, origin.z + dz) >= top)
        };
        let deep: Vec<(I64Vec3, &ChunkMesh)> =
            meshes.iter().copied().filter(|(o, _)| under(*o)).collect();
        let off = cull_counts(&view, deep.iter().copied(), None);
        let on = cull_counts(&view, deep.iter().copied(), Some(&set));
        out += &format!(
            "\nunderground: off quads={} on quads={} hidden={:.1}%",
            off.quads,
            on.quads,
            hidden(off.quads, on.quads)
        );
    }
    out
}

/// The `world:` line: meshed fraction, quads per non-empty mesh and voxel memory.
fn world_line(map: &ChunkMap) -> String {
    let mut quads: Vec<u32> = map
        .ready_meshes()
        .map(|(_, _, mesh)| mesh.total_quads())
        .collect();
    quads.sort_unstable();
    let stats = map.stats();
    let meshed = stats.mesh_ready + stats.mesh_empty;
    let fraction = if meshed > 0 {
        stats.mesh_ready as f64 / meshed as f64
    } else {
        0.0
    };
    let at = |q: usize| {
        quads
            .get(quads.len().saturating_sub(1) * q / 100)
            .copied()
            .unwrap_or(0)
    };
    let voxel_bytes: usize = map
        .iter()
        .filter_map(|(_, slot)| match slot.data() {
            DataState::Ready(data) => Some(data.heap_bytes()),
            _ => None,
        })
        .sum();
    format!(
        "world: meshed_fraction={fraction:.3} quads_per_mesh_p50={} p99={} max={} voxel_mib={:.1}",
        at(50),
        at(99),
        quads.last().copied().unwrap_or(0),
        voxel_bytes as f64 / f64::from(1 << 20),
    )
}

/// The `light:` line: AO policy, light palette entries per non-empty mesh and the meshes whose
/// palette overflowed.
fn light_line(map: &ChunkMap, ao: AoPolicy) -> String {
    let mut entries: Vec<usize> = Vec::new();
    let mut requantized = 0;
    // What `boxel_render::arena_len` gives (this example has no renderer).
    let mut arena = 0u64;
    for (_, _, mesh) in map.ready_meshes() {
        entries.push(mesh.palette.len());
        requantized += usize::from(mesh.light_quantization != LightQuantization::Exact);
        arena += u64::from(mesh.total_quads()) + mesh.palette.len().max(1) as u64;
    }
    entries.sort_unstable();
    let at = |q: usize| {
        entries
            .get(entries.len().saturating_sub(1) * q / 100)
            .copied()
            .unwrap_or(0)
    };
    format!(
        "light: ao={} palette_p50={} p99={} max={} requantized={requantized} arena_entries={arena}",
        ao_name(ao),
        at(50),
        at(99),
        entries.last().copied().unwrap_or(0),
    )
}

/// The `sky:` line: light paths, sky walks and the memory of the light caches.
fn sky_line(map: &ChunkMap, sky: &SkyMap, stats: &WorldStats, light: bool) -> String {
    let cache: usize = map
        .iter()
        .filter_map(|(_, slot)| Some(slot.light()?.heap_bytes()))
        .sum();
    let mib = |bytes: usize| bytes as f64 / f64::from(1 << 20);
    format!(
        "sky: light={} full={} fast={} columns={} walked={} walks={} private_generations={} \
         cache_mib={:.1} sky_mib={:.1}",
        if light { "on" } else { "off" },
        stats.light_full,
        stats.light_fast,
        sky.len(),
        sky.walked(),
        stats.sky_walks,
        stats.sky_private_generations,
        mib(cache),
        mib(sky.heap_bytes()),
    )
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
    sky: Res<SkyMap>,
    settings: Res<WorldSettings>,
    frames: Res<FrameStats>,
    viewers: Query<(&Transform, &ChunkViewer)>,
    mut exit: MessageWriter<AppExit>,
) {
    let now = Instant::now();
    let elapsed = run.started.elapsed();
    let settled = stats.chunks > 0 && stats.is_settled();
    let x = viewers
        .iter()
        .next()
        .map_or(VIEWER.x, |(t, _)| t.translation.x);
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
            println!("{}", world_line(&map));
            println!("{}", light_line(&map, settings.ao_policy));
            println!("{}", sky_line(&map, &sky, &stats, settings.light));
            if run.options.cull
                && let Some((transform, viewer)) = viewers.iter().next()
            {
                println!("{}", cull_lines(&run.options, &map, transform, *viewer));
            }
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
            let Some((transform, viewer)) = viewers.iter().next() else {
                return;
            };
            // The world clamps viewers to its limit; so does the expected center.
            let center =
                ChunkPos::containing(BlockPos::from_world(transform.translation)).clamp_to_world();
            println!(
                "jump: chunks={} settled_after={:.3}s frames={} max_update_ms={:.2} \
                 p99_update_ms={:.2} center={},{},{}",
                run.options.jump,
                now.duration_since(since).as_secs_f64(),
                run.updates.len(),
                max(&run.updates),
                p99(&run.updates),
                center.x,
                center.y,
                center.z,
            );
            let missing = missing_around(
                center,
                viewer.radius,
                viewer.vertical_radius,
                settings.vertical_bounds.as_ref(),
                |p| map.id_at(p).is_some(),
            );
            if let Some(first) = missing.first() {
                eprintln!(
                    "headless_world: the world settled away from the viewer: {} chunks of its \
                     mesh region around {center:?} are not loaded (first {first:?})",
                    missing.len()
                );
                run.exit(EXIT_OFF_CENTER, &mut exit);
                return;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Options>, String> {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        parse_options(&args)
    }

    #[test]
    fn jumps_reach_the_world_limit_in_both_directions() {
        for (arg, chunks) in [
            ("100", 100),
            ("70000000", 70_000_000),
            ("-40000000000", -40_000_000_000),
            ("72057594037927936", MAX_CHUNK_COORD),
        ] {
            let o = parse(&["--jump", arg]).expect("valid").expect("not help");
            assert_eq!(o.jump, chunks);
        }
        for arg in ["72057594037927937", "-9223372036854775808", "1.5", "x"] {
            assert!(parse(&["--jump", arg]).is_err(), "{arg} was accepted");
        }
    }

    #[test]
    fn world_and_viewer_height() {
        let o = parse(&[]).expect("valid").expect("not help");
        assert_eq!((o.world.label(), o.viewer_y), ("terrain".to_string(), 80.0));
        let o = parse(&["--world", "overworld", "--viewer-y", "-1000"])
            .expect("valid")
            .expect("not help");
        assert_eq!(
            (o.world.label(), o.viewer_y),
            ("overworld/mc".to_string(), -1000.0)
        );
        for args in [
            &["--world", "overworld", "--fixed-height"][..],
            &["--viewer-y", "3e18"],
            &["--viewer-y", "x"],
        ] {
            assert!(parse(args).is_err(), "{args:?} was accepted");
        }
    }

    #[test]
    fn camera_and_cull() {
        let o = parse(&["--camera", "16,-1000,16,-90,-20", "--cull"])
            .expect("valid")
            .expect("not help");
        assert_eq!(o.camera, Some([16.0, -1000.0, 16.0, -90.0, -20.0]));
        assert_eq!(o.viewer_y, -1000.0);
        assert!(o.cull);
        for args in [
            &["--camera", "1,2,3"][..],
            &["--camera", "0,3e18,0,0,0"],
            &["--camera", "0,0,0,nan,0"],
        ] {
            assert!(parse(args).is_err(), "{args:?} was accepted");
        }
    }

    #[test]
    fn ao_and_light_modes() {
        let o = parse(&[]).expect("valid").expect("not help");
        assert_eq!(o.ao, AoPolicy::BakedLitOnly);
        for (name, policy) in AO_MODES {
            let o = parse(&["--ao", name]).expect("valid").expect("not help");
            assert_eq!((o.ao, ao_name(policy)), (policy, name));
        }
        assert!(parse(&["--ao", "on"]).is_err());
        assert!(o.light);
        let o = parse(&["--light", "off"])
            .expect("valid")
            .expect("not help");
        assert!(!o.light);
        assert!(parse(&["--light", "yes"]).is_err());
    }

    #[test]
    fn missing_around_finds_an_off_center_world() {
        let center = ChunkPos::new((1 << 26) + 3, 2, -(1 << 26));
        let mesh = |p: ChunkPos| {
            let (dx, dz) = (p.x - center.x, p.z - center.z);
            dx * dx + dz * dz <= 4 && (p.y - center.y).abs() <= 1
        };
        assert!(missing_around(center, 2, 1, None, mesh).is_empty());
        // Saturated at the former i32 edge: the world is around another chunk.
        let elsewhere = ChunkPos::new((1 << 26) - 1, 2, -(1 << 26));
        let wrong = |p: ChunkPos| (p.x - elsewhere.x).abs() <= 2;
        assert!(!missing_around(center, 2, 1, None, wrong).is_empty());
        // Vertical bounds clip the layers that must be loaded.
        let one_layer = |p: ChunkPos| p.y == 2 && mesh(p);
        assert!(missing_around(center, 2, 1, Some(&(2..=2)), one_layer).is_empty());
        assert!(!missing_around(center, 2, 1, None, one_layer).is_empty());
    }
}
