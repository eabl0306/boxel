//! Headless screenshots of the world: streams it around a camera with no window, places blocks
//! if asked, then renders the camera's view offscreen with the real renderer at one or more
//! times of day and saves each one as a PNG.
//!
//! `cargo run --release --example snapshot -- --camera X,Y,Z,YAW,PITCH --shot 12:00=noon.png
//! [--shot 00:00=night.png ...] [options]` (see `--help`). The GPU flags of `voxel_world`
//! (`--adapter nvidia`, `--tier mid`...) and `WGPU_ADAPTER_NAME` pick the adapter.
//!
//! Every shot of one run sees the same world: only the scene light changes with the time of day
//! (the meshes keep their baked light, as in the game). Blocks given with `--place`, `--drop` and
//! `--fill` are set once the area is loaded, and the shots wait for the remeshes they cause.
//! Water flows (`FluidPlugin`, ADR 0021): with `--flow N` the edits get N game ticks (20 per
//! second) to flow before the shots, run one by one on a stopped clock (the same pixels every
//! run). `--world-dir DIR` loads the world saved there and saves it after the shots (ADR 0024):
//! a run with edits and a later run without them give the same pixels. `--outline X,Y,Z` draws
//! the outline of that block, as `voxel_world` outlines the targeted block (ADR 0026).
//!
//! Output (stdout): `gpu: <adapter summary>`, `textures: <source, size, pack load time>`; with
//! edits, one `drop:` line per `--drop` (where the block landed) and `edits: set=N skipped=N
//! not_applied=N`; then `world: <world> chunks=N meshes=N quads=N light=on|off settled_after=S`,
//! `camera: ...` (the light of the camera's cell, the ground under it and what the center of the
//! view hits, to aim the edits),
//! and one `shot:` line per image with the scene light and the mean sRGB brightness (0 to 255).
//! The same camera and options give the same pixels on the same GPU and driver.
//!
//! `--lod PRESET` adds the far LOD (ADR 0031, world side only: the shots draw its nodes with the
//! renderer, as the render plugin would): after the world settles, the shots wait until the LOD
//! is filled (within `--timeout`), and a `lod:` line gives its nodes, quads, the edited chunks
//! it shows (`--fill`, or saved in `--world-dir`) and its fill time.
//! `--lod-tint` tints each level (F9 in `voxel_world`). `--fixed-height` streams every layer of
//! the generator and nothing else, as `voxel_world --fixed-height` does. `--no-distance-fog`
//! turns off the fade into the sky at the edge (`RenderSettings::distance_fog`, F10 in
//! `voxel_world`).
//!
//! Exit codes: 0 success, 1 usage error, 2 no usable GPU, or rendering or saving failed, 4 the
//! world did not settle within the timeout. Set `RUST_LOG` for more logging (default filter `warn`).
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

#[path = "shared/clock.rs"]
mod clock;
#[path = "shared/fog.rs"]
mod fog;
#[path = "shared/lod.rs"]
mod lod;
#[path = "shared/pack.rs"]
mod pack;
#[path = "shared/world.rs"]
mod world;

use boxel::camera::{Camera, CameraView};
use boxel::gfx::{
    CLI_HELP, GpuPreferences, RgbaImage, init_headless, read_texture_rgba8, run_in_error_scopes,
    submit_and_wait, wgpu,
};
use boxel::log::LogPlugin;
use boxel::mesh::{AoPolicy, ChunkMesh};
use boxel::prelude::*;
use boxel::render::fog::{FogInput, FogResolution};
use boxel::render::lod::{LodInput, LodOp};
use boxel::render::{
    BlockOutline, DistanceFog, FLUID_TIME_PERIOD, Outline, RenderSettings, RendererConfig,
    SceneLight, TextureSource, TranslucentSort, ViewParams, VoxelRenderer, block_outline_edges,
    light_curve, load_block_textures, texture_tick,
};
use boxel::voxel::{BlockRegistry, MAX_BLOCK_COORD};
use boxel::world::lod::{LodOps, LodStats, LodWorldPlugin};
use boxel::world::persist::{PersistPlugin, WorldStore};
use boxel::world::{
    BlockTickSettings, BlockTicks, DaylightCurve, FluidPlugin, WorldContent, WorldStats,
};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};
use world::{World, WorldChoice};

const USAGE: &str = "\
usage: snapshot --camera X,Y,Z,YAW,PITCH --shot HH:MM=PATH [--shot ...] [options]
  --camera X,Y,Z,YAW,PITCH  eye position (voxels) and angles (degrees; yaw 0 looks toward -z,
                       -90 toward +x; pitch negative looks down)
  --shot HH:MM=PATH    render at this clock time (06:00 sunrise, 12:00 noon, 18:00 sunset,
                       00:00 midnight) and save a PNG; repeat for more times of the same world
  --size WxH           image size (default 1280x720)
  --view-distance N    horizontal mesh radius in chunks (default 12, as voxel_world); the fog
                       ends where the meshed disc is complete (340 blocks at 12)
  --no-distance-fog    no fade into the sky at the view distance (or at the LOD's horizon):
                       the terrain ends with a hard edge (F10 in voxel_world)
  --vertical-radius N  vertical mesh radius in chunks (default 8)
  --fixed-height       stream every layer of the generator and nothing else, whatever the
                       camera height (as voxel_world --fixed-height; needs a world with a floor)
  --seed N             world seed (default: the terrain's)
  --light MODE         on (the default) or off (open sky everywhere)
  --ao MODE            ambient occlusion: off, baked or lit-only (the default); --light off
                       --ao off is the look before light
  --place NAME@X,Y,Z   set block NAME (boxel:lamp_orange, lamp_red, lava, air...) at X,Y,Z;
                       NAME may give state properties: stone_slab[type=top], log[axis=x]
  --drop NAME@X,Y,Z    set block NAME on the ground under X,Y,Z, which must be air (searching
                       64 blocks down in the generated world; prints where it landed)
  --fill NAME@X,Y,Z:X,Y,Z  set every block of the box between both corners (inclusive); a
                       later edit of a cell replaces an earlier one
  --flow N             let placed water flow for N game ticks (20 a second) before the shots
                       (default 0)
  --fluid-time S       seconds of the fluid animation in the shots: how far the flow textures
                       have scrolled and which frame animated pack textures show (20 ticks a
                       second; default 0)
  --world-dir DIR      load the world saved in DIR (made if missing) and save it there after
                       the shots, edits included
  --outline X,Y,Z      draw the outline of the block at X,Y,Z, as voxel_world outlines the
                       targeted block (default color and width)
  --fov DEG            vertical field of view in degrees (default 70; voxel_world widens it
                       1.1 times while sprinting: 77)
  --fog-tick N         ticks of the fog clock (64 a second) in the shots: where the wind has
                       taken the cells (default 0)
  --lod-tint           with --lod: tint each LOD level (F9 in voxel_world)
  --timeout S          wall-clock limit for loading the world, in seconds (default 300)
  --help               this text";

/// The whole help: [`USAGE`], the world options and the GPU flags.
fn usage() -> String {
    format!(
        "{USAGE}\n{}\n{}\n{}\nworld options:\n{}\n\n{CLI_HELP}",
        pack::USAGE,
        fog::USAGE,
        lod::USAGE,
        world::usage(World::Overworld)
    )
}

/// Exit code for a usage error.
const EXIT_USAGE: u8 = 1;
/// Exit code when no GPU can be used or rendering fails.
const EXIT_GPU: u8 = 2;
/// Exit code when the world does not settle in time.
const EXIT_TIMEOUT: u8 = 4;
/// Consecutive settled updates before the world counts as loaded (edit batches can hold their
/// meshes for a couple of frames).
const SETTLED_UPDATES: u32 = 8;
/// How far `--drop` searches down.
const DROP_DEPTH: i64 = 64;
/// Most blocks one `--fill` may set.
const MAX_FILL: u64 = 1 << 20;
/// How far the center ray of the `camera:` line looks.
const RAY_DISTANCE: f64 = 256.0;

/// How a block edit picks its cells.
#[derive(Clone, Debug, PartialEq)]
enum Edit {
    /// One cell.
    Place(I64Vec3),
    /// On the ground under a cell.
    Drop(I64Vec3),
    /// A box, both corners inclusive.
    Fill(I64Vec3, I64Vec3),
}

#[derive(Clone, Debug, PartialEq)]
struct Options {
    /// x, y, z, yaw, pitch.
    camera: [f64; 5],
    /// (clock hours, path).
    shots: Vec<(f64, PathBuf)>,
    size: (u32, u32),
    radius: u32,
    vertical_radius: u32,
    /// `--fixed-height`: every layer of the generator, whatever the camera height.
    fixed_height: bool,
    seed: u64,
    world: WorldChoice,
    light: bool,
    ao: AoPolicy,
    /// (block name, edit), in order.
    edits: Vec<(String, Edit)>,
    timeout: Duration,
    textures: TextureSource,
    /// Game ticks of fluid flow after the edits.
    flow: u32,
    /// Seconds of the fluid animation (`ViewParams::fluid_time`).
    fluid_time: f64,
    /// `--world-dir`: where the world is loaded from and saved.
    world_dir: Option<PathBuf>,
    /// `--outline`: the block whose outline the shots draw (ADR 0026).
    outline: Option<I64Vec3>,
    /// `--fov`: the vertical field of view, degrees.
    fov: f32,
    /// `--fog`.
    fog: fog::FogPreset,
    /// `--fog-res`.
    fog_res: FogResolution,
    /// `--fog-tick`.
    fog_tick: u64,
    /// `--fog-steps`.
    fog_steps: u32,
    /// `--lod`.
    lod: lod::LodPreset,
    /// `--lod-tint`.
    lod_tint: bool,
    /// `--no-distance-fog`: `RenderSettings::distance_fog` off.
    no_distance_fog: bool,
}

/// `Ok(None)` for `--help`.
fn parse_options(args: &[String]) -> Result<Option<Options>, String> {
    let mut o = Options {
        camera: [f64::NAN; 5],
        shots: Vec::new(),
        size: (1280, 720),
        radius: 12,
        vertical_radius: ChunkViewer::DEFAULT_VERTICAL_RADIUS,
        fixed_height: false,
        seed: boxel_demo::worldgen::DEFAULT_SEED,
        world: WorldChoice::with_world(World::Overworld),
        light: true,
        ao: AoPolicy::default(),
        edits: Vec::new(),
        timeout: Duration::from_secs(300),
        textures: TextureSource::Procedural,
        flow: 0,
        fluid_time: 0.0,
        world_dir: None,
        outline: None,
        // `Camera::default`'s (70 degrees): the same pixels as before the flag.
        fov: 70.0,
        fog: fog::FogPreset::Off,
        fog_res: FogResolution::Half,
        fog_tick: 0,
        fog_steps: boxel::fog::DEFAULT_MAX_STEPS,
        lod: lod::LodPreset::Off,
        lod_tint: false,
        no_distance_fog: false,
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
            "--camera" => o.camera = parse_camera(flag, &value()?)?,
            "--shot" => {
                let v = value()?;
                let (time, path) = v
                    .split_once('=')
                    .filter(|(_, p)| !p.is_empty())
                    .ok_or_else(|| format!("{flag}: {v:?} is not HH:MM=PATH"))?;
                o.shots
                    .push((clock::parse_clock(flag, time)?, PathBuf::from(path)));
            }
            "--size" => {
                let v = value()?;
                o.size = v
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
                    .filter(|&(w, h)| (16..=8192).contains(&w) && (16..=8192).contains(&h))
                    .ok_or_else(|| format!("{flag}: {v:?} is not WxH within 16..=8192"))?;
            }
            "--view-distance" => o.radius = radius(flag, &value()?)?,
            "--vertical-radius" => o.vertical_radius = radius(flag, &value()?)?,
            "--seed" => {
                let v = value()?;
                o.seed = v
                    .parse()
                    .map_err(|_| format!("{flag}: {v:?} is not an unsigned integer"))?;
            }
            "--light" => {
                o.light = match value()?.as_str() {
                    "on" => true,
                    "off" => false,
                    v => return Err(format!("{flag}: {v:?} is not on or off")),
                };
            }
            "--ao" => {
                o.ao = match value()?.as_str() {
                    "off" => AoPolicy::Off,
                    "baked" => AoPolicy::Baked,
                    "lit-only" => AoPolicy::BakedLitOnly,
                    v => return Err(format!("{flag}: {v:?} is not off, baked or lit-only")),
                };
            }
            "--place" | "--drop" | "--fill" => o.edits.push(parse_edit(flag, &value()?)?),
            "--flow" => {
                let v = value()?;
                o.flow = v
                    .parse::<u32>()
                    .ok()
                    .filter(|&n| n <= 100_000)
                    .ok_or_else(|| format!("{flag}: {v:?} is not in 0..=100000"))?;
            }
            "--fluid-time" => {
                let v = value()?;
                o.fluid_time = v
                    .parse::<f64>()
                    .ok()
                    .filter(|s| s.is_finite() && *s >= 0.0)
                    .ok_or_else(|| format!("{flag}: {v:?} is not a number of seconds >= 0"))?;
            }
            "--pack" => o.textures = pack::parse(flag, &value()?)?,
            "--fog" => o.fog = fog::parse(flag, &value()?)?,
            "--lod" => o.lod = lod::parse(flag, &value()?)?,
            "--lod-tint" => o.lod_tint = true,
            "--no-distance-fog" => o.no_distance_fog = true,
            "--fixed-height" => o.fixed_height = true,
            "--fog-res" => o.fog_res = fog::parse_resolution(flag, &value()?)?,
            "--fog-steps" => o.fog_steps = fog::parse_steps(flag, &value()?)?,
            "--fog-tick" => {
                let v = value()?;
                o.fog_tick = v
                    .parse::<u64>()
                    .map_err(|_| format!("{flag}: {v:?} is not a tick count"))?;
            }
            "--world-dir" => o.world_dir = Some(PathBuf::from(value()?)),
            "--outline" => o.outline = Some(parse_cell(flag, &value()?)?),
            "--fov" => {
                let v = value()?;
                o.fov = v
                    .parse::<f32>()
                    .ok()
                    .filter(|d| *d > 1.0 && *d < 179.0)
                    .ok_or_else(|| {
                        format!("{flag}: {v:?} is not an angle in degrees (1 to 179)")
                    })?;
            }
            "--timeout" => {
                let v = value()?;
                o.timeout = v
                    .parse::<f64>()
                    .ok()
                    .filter(|s| s.is_finite() && *s > 0.0)
                    .map(Duration::from_secs_f64)
                    .ok_or_else(|| format!("{flag}: {v:?} is not a positive number"))?;
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if o.camera.iter().any(|c| c.is_nan()) {
        return Err("--camera is required".to_string());
    }
    if o.shots.is_empty() {
        return Err("at least one --shot is required".to_string());
    }
    o.world.validate(o.fixed_height)?;
    if o.lod_tint && o.lod == lod::LodPreset::Off {
        return Err("--lod-tint needs --lod".to_string());
    }
    Ok(Some(o))
}

fn radius(flag: &str, value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|r| *r <= 32)
        .ok_or_else(|| format!("{flag}: {value:?} is not in 0..=32"))
}

/// Comma-separated numbers, exactly `N` of them.
fn numbers<const N: usize>(text: &str) -> Option<[f64; N]> {
    let parts: Vec<f64> = text
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    parts
        .try_into()
        .ok()
        .filter(|v: &[f64; N]| v.iter().all(|x| x.is_finite()))
}

fn parse_camera(flag: &str, value: &str) -> Result<[f64; 5], String> {
    let camera =
        numbers::<5>(value).ok_or_else(|| format!("{flag}: {value:?} is not x,y,z,yaw,pitch"))?;
    if camera[..3].iter().any(|c| c.abs() > MAX_BLOCK_COORD as f64) {
        return Err(format!(
            "{flag}: {value:?} is outside the world: x, y and z must be within \
             +-{MAX_BLOCK_COORD}"
        ));
    }
    Ok(camera)
}

/// `X,Y,Z` as a block position (integers within the world limit).
fn parse_cell(flag: &str, value: &str) -> Result<I64Vec3, String> {
    let bad = || format!("{flag}: {value:?} is not X,Y,Z (integers within the world)");
    let parts: Vec<i64> = value
        .split(',')
        .map(|p| p.trim().parse::<i64>())
        .collect::<Result<_, _>>()
        .map_err(|_| bad())?;
    let [x, y, z]: [i64; 3] = parts.try_into().map_err(|_| bad())?;
    if [x, y, z]
        .iter()
        .any(|c| c.unsigned_abs() > MAX_BLOCK_COORD.unsigned_abs())
    {
        return Err(bad());
    }
    Ok(I64Vec3::new(x, y, z))
}

/// `NAME@X,Y,Z` (or `NAME@X,Y,Z:X,Y,Z` for `--fill`). A name without a namespace gets `boxel:`.
fn parse_edit(flag: &str, value: &str) -> Result<(String, Edit), String> {
    let (name, at) = value
        .split_once('@')
        .filter(|(n, _)| !n.is_empty())
        .ok_or_else(|| format!("{flag}: {value:?} is not NAME@X,Y,Z"))?;
    let name = if name.contains(':') {
        name.to_string()
    } else {
        format!("boxel:{name}")
    };
    let edit = match flag {
        "--place" => Edit::Place(parse_cell(flag, at)?),
        "--drop" => Edit::Drop(parse_cell(flag, at)?),
        _ => {
            let (a, b) = at
                .split_once(':')
                .ok_or_else(|| format!("{flag}: {value:?} is not NAME@X,Y,Z:X,Y,Z"))?;
            let (a, b) = (parse_cell(flag, a)?, parse_cell(flag, b)?);
            let (min, max) = (a.min(b), a.max(b));
            let size = (max - min + I64Vec3::ONE).as_u64vec3();
            if size.x.saturating_mul(size.y).saturating_mul(size.z) > MAX_FILL {
                return Err(format!("{flag}: {value:?} is more than {MAX_FILL} blocks"));
            }
            Edit::Fill(min, max)
        }
    };
    Ok((name, edit))
}

// ---------------------------------------------------------------------------------------------
// World

/// Updates the app until the world has been settled for [`SETTLED_UPDATES`] updates in a row.
/// `Err` with the last counters after `deadline`.
fn settle(app: &mut App, deadline: Instant, lod: &mut LodNodes) -> Result<(), Box<WorldStats>> {
    let mut settled = 0;
    while settled < SETTLED_UPDATES {
        update(app, lod);
        let stats = *app.world().resource::<WorldStats>();
        settled = if stats.chunks > 0 && stats.is_settled() {
            settled + 1
        } else {
            0
        };
        if Instant::now() > deadline {
            return Err(Box::new(stats));
        }
        // This loop is the app's runner. The jobs run on the task pool: no need to spin.
        #[expect(clippy::disallowed_methods, reason = "the runner's pacing")]
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// One update, and the far LOD's ops of it (`--lod`: the renderer applies them, as the render
/// plugin would; ops nobody takes are dropped at the next update).
fn update(app: &mut App, lod: &mut LodNodes) {
    app.update();
    if let Some(mut ops) = app.world_mut().get_resource_mut::<LodOps>() {
        lod.apply(ops.take());
    }
}

/// The far LOD's nodes as a renderer holds them after every op so far: mesh and hidden octants.
#[derive(Default)]
struct LodNodes(HashMap<ChunkKey, (Arc<ChunkMesh>, u8)>);

impl LodNodes {
    fn apply(&mut self, ops: Vec<LodOp>) {
        for op in ops {
            match op {
                LodOp::Upsert { key, mesh, mask } => {
                    self.0.insert(key, (mesh, mask));
                }
                LodOp::Mask { key, mask } => {
                    if let Some(node) = self.0.get_mut(&key) {
                        node.1 = mask;
                    }
                }
                LodOp::Remove { key } => {
                    self.0.remove(&key);
                }
            }
        }
    }

    /// Every node, as the ops of a fresh renderer.
    fn ops(&self) -> Vec<LodOp> {
        self.0
            .iter()
            .map(|(key, (mesh, mask))| LodOp::Upsert {
                key: *key,
                mesh: Arc::clone(mesh),
                mask: *mask,
            })
            .collect()
    }
}

/// `--lod`: updates the app until the LOD is filled (every wanted node built and sent), after
/// the world settled. `Err` with the LOD's counters after `deadline`.
fn fill_lod(app: &mut App, deadline: Instant, lod: &mut LodNodes) -> Result<(), String> {
    loop {
        let stats = app.world().resource::<LodStats>();
        if stats.off.is_none() && stats.fill >= 1.0 && stats.sends_waiting == 0 {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("the LOD did not fill in time: {stats:?}"));
        }
        update(app, lod);
        #[expect(clippy::disallowed_methods, reason = "the runner's pacing")]
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The cells of the edits, with their states, once each: a later edit of a cell replaces an
/// earlier one (a `--fill` of air inside a `--fill` of stone makes a room). `Err` for an unknown
/// block, property or value (names are block states, `boxel:stone_slab[type=top]`); `--drop` edits whose column has no ground within [`DROP_DEPTH`] (or is not
/// loaded) are left out and counted.
fn edit_cells(
    edits: &[(String, Edit)],
    registry: &BlockRegistry,
    map: &ChunkMap,
) -> Result<(Vec<(BlockPos, BlockState)>, usize), String> {
    let mut cells: Vec<(BlockPos, BlockState)> = Vec::new();
    // Index of each cell in `cells`.
    let mut index: HashMap<BlockPos, usize> = HashMap::new();
    let mut set = |pos: BlockPos, state: BlockState| match index.entry(pos) {
        Entry::Occupied(e) => cells[*e.get()].1 = state,
        Entry::Vacant(e) => {
            e.insert(cells.len());
            cells.push((pos, state));
        }
    };
    let mut skipped = 0;
    for (name, edit) in edits {
        let state = registry.parse_state(name).map_err(|e| e.to_string())?;
        match *edit {
            Edit::Place(p) => set(BlockPos::new(p.x, p.y, p.z), state),
            Edit::Drop(p) => {
                // The cell above the first non-air block under the start, which must be air.
                let ground = (0..=DROP_DEPTH).find_map(|d| {
                    let below = BlockPos::new(p.x, p.y - d, p.z);
                    match map.block_at(below) {
                        Some(s) if !s.is_air() => Some((d > 0).then_some(below)),
                        Some(_) => None,
                        None => Some(None),
                    }
                });
                match ground.flatten() {
                    Some(g) => {
                        let cell = BlockPos::new(g.x, g.y + 1, g.z);
                        println!("drop: {name} at {},{},{}", cell.x, cell.y, cell.z);
                        set(cell, state);
                    }
                    None => {
                        println!(
                            "drop: {name} from {},{},{}: no ground (inside a block, not \
                             loaded or none within {DROP_DEPTH})",
                            p.x, p.y, p.z
                        );
                        skipped += 1;
                    }
                }
            }
            Edit::Fill(min, max) => {
                for y in min.y..=max.y {
                    for z in min.z..=max.z {
                        for x in min.x..=max.x {
                            set(BlockPos::new(x, y, z), state);
                        }
                    }
                }
            }
        }
    }
    Ok((cells, skipped))
}

/// First non-air block along the ray from `eye` (voxel DDA): the block, the cell before it (the
/// one whose light the visible face shows) and the distance. `None` when nothing is hit within
/// `max_t` or the ray reaches a chunk that is not loaded.
fn center_hit(
    map: &ChunkMap,
    eye: DVec3,
    dir: DVec3,
    max_t: f64,
) -> Option<(BlockPos, BlockPos, f64)> {
    let mut cell = eye.floor().as_i64vec3();
    // Per axis: the step, the ray distance to the first block plane and between planes (the
    // fractional part of the eye is exact in f64 anywhere in the world). An axis the ray does not
    // move along never steps.
    let frac = eye - eye.floor();
    let mut step = I64Vec3::ZERO;
    let mut next = DVec3::INFINITY;
    let mut inv = DVec3::INFINITY;
    for axis in 0..3 {
        let d = dir[axis];
        if d != 0.0 {
            step[axis] = if d > 0.0 { 1 } else { -1 };
            inv[axis] = 1.0 / d.abs();
            let to_plane = if d > 0.0 {
                1.0 - frac[axis]
            } else {
                frac[axis]
            };
            next[axis] = to_plane * inv[axis];
        }
    }
    let mut prev = cell;
    let mut t = 0.0;
    let at = |c: I64Vec3| BlockPos::new(c.x, c.y, c.z);
    loop {
        if !map.block_at(at(cell))?.is_air() {
            return Some((at(cell), at(prev), t));
        }
        prev = cell;
        let axis = if next.x <= next.y && next.x <= next.z {
            0
        } else if next.y <= next.z {
            1
        } else {
            2
        };
        t = next[axis];
        if t > max_t {
            return None;
        }
        cell[axis] += step[axis];
        next[axis] += inv[axis];
    }
}

/// `sky=S rgb=R,G,B` of a cell, or `unknown`.
fn light_text(map: &ChunkMap, p: BlockPos) -> String {
    map.light_at(p).map_or_else(
        || "unknown".to_string(),
        |l| {
            let [r, g, b] = l.rgb();
            format!("sky={} rgb={r},{g},{b}", l.sky())
        },
    )
}

// ---------------------------------------------------------------------------------------------
// Rendering

/// The sky color and scene light at clock `hours`, as the render plugin computes them from the
/// world clock with the default curve and `settings`.
fn scene_light(hours: f64, settings: &RenderSettings) -> ([f32; 4], SceneLight) {
    let daylight = DaylightCurve::default().at(&TimeOfDay::at_clock(hours));
    let light = SceneLight {
        sky: daylight.sky_light,
        ambient: daylight.ambient,
        ao_strength: settings.ao_strength,
        block_gain: settings.block_light_gain,
        curve: light_curve(settings.light_gamma),
    };
    (daylight.sky_color, light)
}

/// Renders one frame inside error scopes and reads the main color back.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("snapshot"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// Mean of the RGB bytes (sRGB-encoded, 0 to 255).
fn mean_brightness(image: &RgbaImage) -> f64 {
    let pixels = image.data.as_chunks::<4>().0;
    let sum: u64 = pixels
        .iter()
        .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
        .sum();
    sum as f64 / (3 * pixels.len().max(1)) as f64
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fail = |message: String| {
        eprintln!("snapshot: {message}\n\n{}", usage());
        ExitCode::from(EXIT_USAGE)
    };
    let (cli_gpu, gpu_config, rest) = match GpuPreferences::from_args(&args) {
        Ok(parsed) => parsed,
        Err(e) => return fail(e.to_string()),
    };
    let options = match parse_options(&rest) {
        Ok(Some(o)) => o,
        Ok(None) => {
            println!("{}", usage());
            return ExitCode::SUCCESS;
        }
        Err(message) => return fail(message),
    };
    match run(&options, cli_gpu, gpu_config) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("snapshot: {message}");
            ExitCode::from(code)
        }
    }
}

fn run(
    o: &Options,
    cli_gpu: GpuPreferences,
    gpu_config: Option<PathBuf>,
) -> Result<(), (u8, String)> {
    let gpu_err = |e: String| (EXIT_GPU, e);
    let (prefs, warnings) = GpuPreferences::resolve(cli_gpu, gpu_config.as_deref());
    for w in warnings {
        eprintln!("snapshot: warning: {w}");
    }
    let gpu = init_headless(&prefs).map_err(|e| gpu_err(e.to_string()))?;
    println!("gpu: {}", gpu.summary_line());

    // Block textures (a resource pack is read here, before the world).
    let registry = Arc::new(boxel_demo::blocks::registry());
    let (textures, report) =
        load_block_textures(&registry, &o.textures).map_err(|e| (EXIT_USAGE, e))?;
    for w in &report.warnings {
        eprintln!("snapshot: warning: textures: {w}");
    }
    println!("textures: {}", pack::summary(&report));

    // The world, loaded around the camera.
    let started = Instant::now();
    let generator = o.world.generator(o.seed);
    let store = match &o.world_dir {
        Some(dir) => Some(
            WorldStore::open_dir(dir, &registry, generator.name())
                .map_err(|e| (EXIT_USAGE, format!("--world-dir {}: {e}", dir.display())))?,
        ),
        None => None,
    };
    // `--fixed-height`: the generator's layers bound the streaming, as in `voxel_world`.
    let vertical_bounds = o.fixed_height.then(|| generator.vertical_chunks());
    let mut app = App::new();
    app.add_plugins((
        LogPlugin {
            filter: "warn".to_string(),
        },
        TaskPoolPlugin::default(),
        TimePlugin,
        WorldPlugin {
            generator,
            registry: Arc::clone(&registry),
            settings: WorldSettings {
                light: o.light,
                ao_policy: o.ao,
                vertical_bounds,
                ..WorldSettings::default()
            },
            ..WorldPlugin::default()
        },
        FluidPlugin,
    ));
    if let Some(store) = store {
        app.add_plugins(PersistPlugin::new(store));
    }
    // `--no-distance-fog`: as `RenderSettings::distance_fog` off, for the view and the LOD.
    let distance_fog = if o.no_distance_fog {
        DistanceFog::off()
    } else {
        DistanceFog::default()
    };
    // `--lod`: the world side only (no render plugin here); the eye is the viewer, the camera.
    // A shot is one frame: every node goes up in it (the renderer's upload budget is per frame,
    // and with it most nodes of a fresh renderer would wait for later frames, not drawn).
    let lod_settings = o.lod.settings().map(|settings| LodSettings {
        debug_tint: o.lod_tint,
        upload_bytes_per_frame: usize::MAX,
        ..settings
    });
    if let Some(settings) = &lod_settings {
        app.add_plugins(LodWorldPlugin {
            settings: settings.clone(),
            ..LodWorldPlugin::default()
        });
    }
    let mut lod_nodes = LodNodes::default();
    // The fluid clock only moves in `--flow` (deterministic shots).
    app.world_mut().resource_mut::<BlockTickSettings>().paused = true;
    app.finish();
    app.cleanup();
    let [x, y, z, yaw, pitch] = o.camera;
    let transform = Transform::from_yaw_pitch(
        DVec3::new(x, y, z),
        (yaw as f32).to_radians(),
        (pitch as f32).to_radians(),
    );
    app.world_mut().spawn((
        transform,
        ChunkViewer::new(o.radius).with_vertical_radius(if o.fixed_height {
            u32::MAX
        } else {
            o.vertical_radius
        }),
    ));
    let deadline = started + o.timeout;
    let timeout = |stats: Box<WorldStats>| {
        (
            EXIT_TIMEOUT,
            format!(
                "the world did not settle within {:.0} s: {stats:?}",
                o.timeout.as_secs_f64()
            ),
        )
    };
    settle(&mut app, deadline, &mut lod_nodes).map_err(timeout)?;
    if !o.edits.is_empty() {
        let (cells, skipped) = edit_cells(&o.edits, &registry, app.world().resource::<ChunkMap>())
            .map_err(|e| (EXIT_USAGE, e))?;
        let mut queue = app.world_mut().resource_mut::<EditQueue>();
        for &(pos, state) in &cells {
            queue.set_block(pos, state);
        }
        settle(&mut app, deadline, &mut lod_nodes).map_err(timeout)?;
        // The block counts, not the state: stairs take their shape from their neighbors and
        // fences their sides (ADR 0020). A torch without a support drops.
        let map = app.world().resource::<ChunkMap>();
        let missing = cells
            .iter()
            .filter(|&&(pos, state)| {
                map.block_at(pos).and_then(|s| registry.block(s)) != registry.block(state)
            })
            .count();
        println!(
            "edits: set={} skipped={} not_applied={missing}",
            cells.len() - missing,
            skipped
        );
    }
    if o.flow > 0 {
        let mut written = 0;
        for _ in 0..o.flow {
            app.world_mut().resource_mut::<BlockTicks>().advance(1);
            update(&mut app, &mut lod_nodes);
            written += app.world().resource::<BlockTicks>().stats().written;
        }
        settle(&mut app, deadline, &mut lod_nodes).map_err(timeout)?;
        let stats = app.world().resource::<BlockTicks>().stats();
        println!(
            "flow: ticks={} written={written} queued={} frozen={}",
            o.flow, stats.queued, stats.frozen
        );
    }
    let settled_after = started.elapsed();
    let lod_input = match &lod_settings {
        Some(settings) => {
            let lod_started = Instant::now();
            fill_lod(&mut app, deadline, &mut lod_nodes).map_err(|e| (EXIT_TIMEOUT, e))?;
            let stats = app.world().resource::<LodStats>();
            let quads: u64 = lod_nodes
                .0
                .values()
                .map(|(m, _)| u64::from(m.total_quads()))
                .sum();
            println!(
                "lod: {} horizon={} nodes={} quads={quads} edited_chunks={} filled_after={:.1}s",
                lod::label(o.lod),
                stats.horizon,
                lod_nodes.0.len(),
                stats.edited_chunks,
                lod_started.elapsed().as_secs_f64()
            );
            Some(LodInput {
                ops: lod_nodes.ops(),
                fog: distance_fog,
                ..LodInput::new(settings.clone(), Some(stats.horizon as f32))
            })
        }
        None => None,
    };
    let map = app.world().resource::<ChunkMap>();
    let quads: u64 = map
        .ready_meshes()
        .map(|(_, _, mesh)| u64::from(mesh.total_quads()))
        .sum();
    println!(
        "world: {} chunks={} meshes={} quads={quads} light={} settled_after={:.1}s",
        o.world.label(),
        map.len(),
        map.ready_meshes().count(),
        if o.light { "on" } else { "off" },
        settled_after.as_secs_f64()
    );
    let eye = BlockPos::from_world(transform.translation);
    let hit = center_hit(
        map,
        transform.translation,
        transform.forward().as_dvec3(),
        RAY_DISTANCE,
    )
    .map_or_else(
        || format!("nothing within {RAY_DISTANCE} voxels"),
        |(block, face, t)| {
            let state = map.block_at(block).unwrap_or(BlockState::AIR);
            format!(
                "{} at {},{},{} ({t:.1} voxels), lit by {}",
                registry.state_name(state),
                block.x,
                block.y,
                block.z,
                light_text(map, face)
            )
        },
    );
    let ground = center_hit(map, transform.translation, DVec3::NEG_Y, RAY_DISTANCE).map_or_else(
        || "none".to_string(),
        |(block, face, _)| format!("y={} lit by {}", block.y, light_text(map, face)),
    );
    println!(
        "camera: cell {},{},{} {}; ground {ground}; center hits {hit}",
        eye.x,
        eye.y,
        eye.z,
        light_text(map, eye)
    );

    // The renderer, with every ready mesh.
    let (arena_quads, max_chunk_slots) = o.world.render_capacity();
    let size = UVec2::new(o.size.0, o.size.1);
    let mut renderer = VoxelRenderer::new(
        &gpu.device,
        &gpu.queue,
        gpu.tier,
        &RendererConfig {
            arena_quads,
            max_chunk_slots,
            // the shot is the first frame: sorted in it
            translucent_sort: TranslucentSort::Inline,
        },
        &textures,
        gpu.output_format,
        size,
    )
    .map_err(|e| gpu_err(format!("VoxelRenderer::new: {e}")))?;
    // Every animated texture layer reaches the GPU in the shot's first frame (ADR 0028).
    renderer.set_texture_upload_budget(u64::MAX);
    // The model boxes of slabs, stairs, fences and torches (ADR 0020).
    let content = app.world().resource::<WorldContent>();
    renderer.set_model_boxes(&gpu.queue, content.materials.model_boxes());
    for (id, key, mesh) in map.ready_meshes() {
        renderer
            .upsert_chunk(
                id.to_bits(),
                key.pos.origin().as_i64vec3(),
                Arc::clone(mesh),
            )
            .map_err(|e| gpu_err(format!("upload {:?}: {e}", key.pos)))?;
    }
    // `--outline`: what the render plugin hands the renderer for a `BlockOutline` there.
    if let Some(block) = o.outline {
        let state = map
            .block_at(BlockPos::from_i64vec3(block))
            .unwrap_or(BlockState::AIR);
        let edges = block_outline_edges(&registry, state);
        println!(
            "outline: {} at {},{},{} edges={}",
            registry.state_name(state),
            block.x,
            block.y,
            block.z,
            edges.len()
        );
        let style = BlockOutline::default();
        renderer.set_outline((!edges.is_empty()).then(|| Outline {
            block,
            edges: edges.into(),
            color: style.color,
            width: style.width,
        }));
    }
    // `--fog`: the demo's preset at a fixed tick (the same cells every run).
    let fog_label = fog::label(o.fog, o.fog_res, o.fog_steps);
    if let Some(settings) = o.fog.settings(o.fog_res, o.fog_steps) {
        renderer.set_fog(
            &gpu.device,
            Some(FogInput {
                settings: Arc::new(settings),
                time: o.fog_tick.into(),
            }),
        );
    }
    // `--lod`: its nodes, drawn to its horizon (the renderer moves the far plane and the terrain
    // fog out to it).
    let lod_label = lod::label(o.lod);
    renderer.set_lod(&gpu.device, lod_input);
    let camera = Camera {
        view_distance: world::view_distance(ChunkViewer::new(o.radius)),
        fov_y_radians: o.fov.to_radians(),
        ..Camera::default()
    };
    let view = CameraView::compute(&camera, &transform, size.x as f32 / size.y as f32);
    let settings = RenderSettings {
        distance_fog,
        ..RenderSettings::default()
    };
    for (hours, path) in &o.shots {
        let (sky_color, light) = scene_light(*hours, &settings);
        let params = ViewParams {
            light,
            fluid_time: (o.fluid_time % FLUID_TIME_PERIOD) as f32,
            texture_tick: texture_tick(o.fluid_time),
            ..ViewParams::from_camera(&view, &camera, sky_color, settings.distance_fog)
        };
        // The first frame uploads; the second draws exactly what a steady game frame draws.
        render(&gpu.device, &gpu.queue, &mut renderer, &params).map_err(gpu_err)?;
        let image = render(&gpu.device, &gpu.queue, &mut renderer, &params).map_err(gpu_err)?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| gpu_err(format!("{}: {e}", dir.display())))?;
        }
        image
            .save_png(path)
            .map_err(|e| gpu_err(format!("{}: {e}", path.display())))?;
        let [r, g, b] = light.sky;
        println!(
            "shot: {} {} sky_light={r:.3},{g:.3},{b:.3} ambient={:.3} mean={:.1} chunks={} \
             fog={fog_label} fog_layers={} lod={lod_label} lod_draws={} distance_fog={}",
            TimeOfDay::at_clock(*hours).clock_text(),
            path.display(),
            light.ambient,
            mean_brightness(&image),
            renderer.stats().chunks,
            renderer.stats().fog_layers,
            renderer.stats().lod_draws,
            if settings.distance_fog.enabled {
                "on"
            } else {
                "off"
            },
        );
    }
    // `--world-dir`: the world is saved by the `Shutdown` schedule (nothing else runs there).
    app.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Options>, String> {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        parse_options(&args)
    }

    const BASE: [&str; 4] = [
        "--camera",
        "1.5,70,-3,-90,-20",
        "--shot",
        "12:00=a/noon.png",
    ];

    #[test]
    fn fog_flags() {
        let o = parse(&BASE).expect("valid").expect("not help");
        assert_eq!(
            (o.fog, o.fog_res, o.fog_tick),
            (fog::FogPreset::Off, FogResolution::Half, 0)
        );
        let mut args = BASE.to_vec();
        args.extend(["--fog", "sunset", "--fog-res", "full", "--fog-tick", "640"]);
        let o = parse(&args).expect("valid").expect("not help");
        assert_eq!(
            (o.fog, o.fog_res, o.fog_tick),
            (fog::FogPreset::Sunset, FogResolution::Full, 640)
        );
        let mut bad = BASE.to_vec();
        bad.extend(["--fog-tick", "-1"]);
        assert!(parse(&bad).is_err());
    }

    #[test]
    fn lod_flag() {
        let o = parse(&BASE).expect("valid").expect("not help");
        assert_eq!(o.lod, lod::LodPreset::Off, "no LOD unless asked");
        let mut args = BASE.to_vec();
        args.extend(["--lod", "near"]);
        let o = parse(&args).expect("valid").expect("not help");
        assert_eq!(o.lod, lod::LodPreset::Near);
        let mut bad = BASE.to_vec();
        bad.extend(["--lod", "huge"]);
        assert!(parse(&bad).is_err());
        // The level tint needs the LOD.
        assert!(!o.lod_tint);
        args.push("--lod-tint");
        assert!(parse(&args).expect("valid").expect("not help").lod_tint);
        let mut bad = BASE.to_vec();
        bad.push("--lod-tint");
        assert!(parse(&bad).unwrap_err().contains("--lod"));
    }

    #[test]
    fn no_distance_fog_flag() {
        let o = parse(&BASE).expect("valid").expect("not help");
        assert!(!o.no_distance_fog, "the distance fog is on unless asked");
        let mut args = BASE.to_vec();
        args.push("--no-distance-fog");
        assert!(
            parse(&args)
                .expect("valid")
                .expect("not help")
                .no_distance_fog
        );
        assert!(usage().contains("--no-distance-fog"));
    }

    #[test]
    fn fixed_height_needs_a_floor() {
        let o = parse(&BASE).expect("valid").expect("not help");
        assert!(!o.fixed_height);
        let mut args = BASE.to_vec();
        args.extend(["--world", "forest", "--fixed-height"]);
        assert!(parse(&args).expect("valid").expect("not help").fixed_height);
        // The default world, the overworld, goes down forever.
        let mut bad = BASE.to_vec();
        bad.push("--fixed-height");
        assert!(parse(&bad).unwrap_err().contains("--fixed-height"));
    }

    #[test]
    fn camera_and_shots_are_required() {
        assert!(parse(&BASE[..2]).unwrap_err().contains("--shot"));
        assert!(parse(&BASE[2..]).unwrap_err().contains("--camera"));
        let o = parse(&BASE).expect("valid").expect("not help");
        assert_eq!(o.camera, [1.5, 70.0, -3.0, -90.0, -20.0]);
        assert_eq!(o.shots, vec![(12.0, PathBuf::from("a/noon.png"))]);
        assert_eq!(o.world.label(), "overworld/mc");
        assert!(o.light);
        assert_eq!(parse(&["--help"]), Ok(None));
    }

    #[test]
    fn shots_times_and_sizes() {
        let mut args = BASE.to_vec();
        args.extend([
            "--shot=18:30=dusk.png",
            "--size",
            "640x360",
            "--light",
            "off",
            "--fluid-time",
            "1.25",
            "--outline",
            "3,64,-2",
            "--fov",
            "77",
        ]);
        let o = parse(&args).expect("valid").expect("not help");
        assert_eq!(o.fov, 77.0);
        assert_eq!(parse(&BASE).expect("valid").expect("not help").fov, 70.0);
        assert_eq!(o.shots[1], (18.5, PathBuf::from("dusk.png")));
        assert_eq!(o.size, (640, 360));
        assert!(!o.light);
        assert_eq!(o.fluid_time, 1.25);
        assert_eq!(o.outline, Some(I64Vec3::new(3, 64, -2)));
        assert_eq!(
            parse(&BASE).expect("valid").expect("not help").outline,
            None
        );
        // Far from the origin (ADR 0014): exact cells up to the world limit.
        let far = I64Vec3::new(1 << 40, -(1 << 31), MAX_BLOCK_COORD);
        let cell = format!("{},{},{}", far.x, far.y, far.z);
        let mut args = BASE.to_vec();
        args.extend(["--outline", &cell]);
        assert_eq!(
            parse(&args).expect("valid").expect("not help").outline,
            Some(far)
        );
        assert_eq!(
            parse(&BASE).expect("valid").expect("not help").fluid_time,
            0.0
        );
        for bad in [
            &["--shot", "25:00=x.png"][..],
            &["--shot", "12:00="],
            &["--shot", "noon.png"],
            &["--size", "10x10"],
            &["--camera", "1,2,3"],
            &["--camera", "3e18,0,0,0,0"],
            &["--ao", "on"],
            &["--fluid-time", "-1"],
            &["--fluid-time", "soon"],
            &["--outline", "1,2"],
            &["--fov", "0"],
            &["--fov", "wide"],
            &["--outline", "0,9223372036854775807,0"],
            &["--pack", "/nonexistent/boxel/pack"],
        ] {
            let mut args = BASE.to_vec();
            args.extend_from_slice(bad);
            assert!(parse(&args).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn edits() {
        let mut args = BASE.to_vec();
        args.extend([
            "--place",
            "lamp_red@1,-2,3",
            "--drop",
            "boxel:lava@1099511627776,0,5",
            "--fill",
            "air@4,4,4:2,6,3",
            "--fill",
            "stone_slab[type=top]@0,1,0:1,1,1",
        ]);
        let o = parse(&args).expect("valid").expect("not help");
        assert_eq!(
            o.edits,
            vec![
                (
                    "boxel:lamp_red".to_string(),
                    Edit::Place(I64Vec3::new(1, -2, 3))
                ),
                (
                    "boxel:lava".to_string(),
                    Edit::Drop(I64Vec3::new(1 << 40, 0, 5))
                ),
                (
                    "boxel:air".to_string(),
                    Edit::Fill(I64Vec3::new(2, 4, 3), I64Vec3::new(4, 6, 4))
                ),
                (
                    "boxel:stone_slab[type=top]".to_string(),
                    Edit::Fill(I64Vec3::new(0, 1, 0), I64Vec3::new(1, 1, 1))
                ),
            ]
        );
        // The names are block states of the registry (`edit_cells` parses them).
        let registry = boxel_demo::blocks::registry();
        for (name, _) in &o.edits {
            assert!(registry.parse_state(name).is_ok(), "{name}");
        }
        assert_eq!(
            registry.parse_state("boxel:stone_slab[type=top]"),
            registry
                .parse_state("boxel:stone_slab")
                .map(|s| { registry.with(s, "type", "top").expect("top") })
        );
        for bad in [
            &["--place", "@1,2,3"][..],
            &["--place", "lamp_red@1,2"],
            &["--drop", "lamp_red"],
            &["--fill", "air@0,0,0"],
            &["--fill", "air@0,0,0:2000,2000,2000"],
        ] {
            let mut args = BASE.to_vec();
            args.extend_from_slice(bad);
            assert!(parse(&args).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn scene_light_follows_the_clock() {
        let settings = RenderSettings::default();
        let (_, noon) = scene_light(12.0, &settings);
        assert_eq!(noon, SceneLight::noon());
        let (night_sky, night) = scene_light(0.0, &settings);
        assert!(night.sky[2] > night.sky[0] && night.sky[2] <= 0.2 + 1e-6);
        assert!(night_sky[2] < 0.1);
        assert_eq!(night.ambient, SceneLight::DEFAULT_AMBIENT);
    }
}
