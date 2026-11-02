//! Per-realm light and face shading (ADR 0039; the sky sources plan, sec. 3) on every tier.
//! Self-skips without an adapter unless `BOXEL_REQUIRE_GPU=1`.
//!
//! The first test builds two realms that meet at a plane (`y = 0` of the scene): a stone slab
//! whose top layer is the last one under the plane, and a stone pillar across it, meshed by the
//! real `Mesher` under open sky. The lower realm has a constant curve (the day at every hour),
//! the upper one the default curve. At noon both look as one realm does; at midnight:
//!
//! - every pixel is the pixel of noon (one realm) or of midnight (one realm): a quad takes one
//!   realm's light, whole;
//! - the top faces of the slab, on the last layer under the plane, show the light of the cell
//!   above it and take the upper realm's curve: dark;
//! - the slab's side faces stay lit: the lower realm has no night;
//! - the pillar is dark above the plane and lit under it, split at the plane;
//! - two more planes `2^40` above and below, with garish realms beyond them, clamp in the
//!   uniform without changing a pixel.
//!
//! Every view is the same at every [`FAR_SHIFTS`] position (scene, plane and camera moved
//! together), and every tier draws exactly the same pixels (software adapters: within 0.1 % of
//! the pixels, channel tolerance 2).
//!
//! The second test looks straight down at the top of a floating stone slab and straight up at
//! its bottom, under the same light: with a flat shade table the two faces give the same pixels
//! (the bottom view mirrored), and the default table keeps today's ratio, the bottom at half
//! the top's light.
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_realms/` (every image with
//! `GPU_REALMS_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::{
    DistanceFog, RealmShading, RendererConfig, SceneLight, SceneRealms, ViewParams, VoxelRenderer,
};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkPos, LocalPos, NEIGHBOR_OFFSETS,
};
use boxel_world::{DaylightCurve, RealmLight, RealmLighting, TimeOfDay};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers of a software adapter in at most this fraction (channel
/// tolerance 2), as in `gpu_terrain.rs`.
const MAX_SOFTWARE_DIFF: f64 = 0.001;
/// The background: no voxel is this color.
const CLEAR: [f32; 4] = [1.0, 0.0, 1.0, 1.0];
/// Far-from-origin translations of the scene, its plane and the views (`gpu_terrain.rs`'s).
const FAR_SHIFTS: [I64Vec3; 5] = [
    I64Vec3::new((1 << 31) - 64, 0, -(1 << 31) + 64),
    I64Vec3::new((1 << 31) + (1 << 20), 0, -(1 << 31) - (1 << 20)),
    I64Vec3::new((1 << 40) + (1 << 31) - 64, 0, -(1 << 40) - (1 << 31) + 64),
    // The plane straddles +2^31 in y, then -2^31.
    I64Vec3::new(-(1 << 35), (1 << 31) - 64, 1 << 35),
    I64Vec3::new(1 << 31, -(1 << 31) - 64, 0),
];

// ---------------------------------------------------------------- adapters

/// Headless instance (`WGPU_BACKEND` or Vulkan + GL) and its adapters filtered by
/// `WGPU_ADAPTER_NAME` (case-insensitive substring).
fn adapters() -> (wgpu::Instance, Vec<wgpu::Adapter>) {
    let instance = wgpu::Instance::new({
        let mut d = wgpu::InstanceDescriptor::new_without_display_handle();
        d.flags = wgpu::InstanceFlags::empty();
        d.backends = wgpu::Backends::from_env()
            .filter(|b| !b.is_empty())
            .unwrap_or(wgpu::Backends::VULKAN | wgpu::Backends::GL);
        d
    });
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    let wanted = std::env::var("WGPU_ADAPTER_NAME")
        .ok()
        .map(|n| n.to_lowercase())
        .filter(|n| !n.is_empty());
    let adapters = adapters
        .into_iter()
        .filter(|a| {
            wanted
                .as_ref()
                .is_none_or(|n| a.get_info().name.to_lowercase().contains(n))
        })
        .collect();
    (instance, adapters)
}

/// The tiers of `gpu_tiers.rs::allowed_tiers`.
fn allowed_tiers(caps: &AdapterCaps) -> Vec<Tier> {
    let mut tiers = Vec::new();
    let high = wgpu::Features::MULTI_DRAW_INDIRECT_COUNT | wgpu::Features::INDIRECT_FIRST_INSTANCE;
    let storage = caps
        .downlevel
        .flags
        .contains(wgpu::DownlevelFlags::VERTEX_STORAGE)
        && caps.limits.max_storage_buffers_per_shader_stage >= floors::STORAGE_BUFFERS_PER_STAGE
        && (caps.info.backend != wgpu::Backend::Gl
            || caps
                .gl_version
                .is_some_and(GlVersion::supports_storage_and_compute));
    if storage
        && caps.features.contains(high)
        && caps
            .downlevel
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        && caps.info.backend == wgpu::Backend::Vulkan
    {
        tiers.push(Tier::High);
    }
    if storage {
        tiers.push(Tier::Mid);
    }
    if caps.features.contains(wgpu::Features::IMMEDIATES)
        && caps.limits.max_immediate_size >= floors::IMMEDIATE_SIZE
    {
        tiers.push(Tier::Low);
    }
    tiers
}

/// A device shaped like the one selection would create for `tier`, collecting uncaptured
/// errors.
fn device_for(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
) -> Result<(wgpu::Device, wgpu::Queue, Arc<Mutex<Vec<String>>>), String> {
    let request = tier_request(tier, &caps.limits);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu_realms"),
        required_features: request.features,
        required_limits: request.limits,
        ..Default::default()
    }))
    .map_err(|e| format!("request_device: {e}"))?;
    let errors = Arc::new(Mutex::new(Vec::new()));
    let sink = errors.clone();
    device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
        if let Ok(mut errors) = sink.lock() {
            errors.push(e.to_string());
        }
    }));
    Ok((device, queue, errors))
}

fn adapter_label(caps: &AdapterCaps) -> String {
    format!("{} ({:?})", caps.info.name, caps.info.backend)
}

// ---------------------------------------------------------------- scenes

/// Chunks by position and their meshes, under open sky (no light computed: every face shows
/// sky 15, so the realm's sky light is what a face shows).
fn mesh_chunks(
    registry: &BlockRegistry,
    at: impl Fn(i64, i64, i64) -> BlockState,
    chunks: &[ChunkPos],
) -> Vec<(ChunkPos, Arc<ChunkMesh>)> {
    let data: BTreeMap<ChunkPos, ChunkData> = chunks
        .iter()
        .map(|&pos| {
            let dense: Vec<BlockState> = (0..CHUNK_VOLUME)
                .map(|i| {
                    let l = LocalPos::from_index(i);
                    at(
                        pos.x * 32 + i64::from(l.x()),
                        pos.y * 32 + i64::from(l.y()),
                        pos.z * 32 + i64::from(l.z()),
                    )
                })
                .collect();
            (pos, ChunkData::from_dense(&dense))
        })
        .collect();
    let materials = MaterialTable::from_registry(registry);
    let mut mesher = Mesher::new();
    data.iter()
        .map(|(&pos, center)| {
            let input = MeshInput {
                center,
                neighbors: std::array::from_fn(|i| data.get(&pos.offset(NEIGHBOR_OFFSETS[i]))),
                light: None,
            };
            (
                pos,
                Arc::new(mesher.mesh_chunk(&input, &materials, MeshOptions::default())),
            )
        })
        .collect()
}

/// The two-realm scene, the plane at `y = 0`: a stone slab at x, z in `0..16`, y in `-3..0` (its
/// top layer the last under the plane) and a stone pillar at x in `24..27`, z in `4..7`, y in
/// `-8..8`.
fn plane_scene(registry: &BlockRegistry) -> Vec<(ChunkPos, Arc<ChunkMesh>)> {
    let stone = registry.by_name("boxel:stone").expect("stone");
    mesh_chunks(
        registry,
        |x, y, z| {
            let slab = (0..16).contains(&x) && (-3..0).contains(&y) && (0..16).contains(&z);
            let pillar = (24..27).contains(&x) && (-8..8).contains(&y) && (4..7).contains(&z);
            if slab || pillar {
                stone
            } else {
                BlockState::AIR
            }
        },
        &[ChunkPos::new(0, -1, 0), ChunkPos::new(0, 0, 0)],
    )
}

/// A floating stone slab at x, z in `0..16`, y = 8 (its top at 9, its bottom at 8).
fn slab_scene(registry: &BlockRegistry) -> Vec<(ChunkPos, Arc<ChunkMesh>)> {
    let stone = registry.by_name("boxel:stone").expect("stone");
    mesh_chunks(
        registry,
        |x, y, z| {
            if (0..16).contains(&x) && y == 8 && (0..16).contains(&z) {
                stone
            } else {
                BlockState::AIR
            }
        },
        &[ChunkPos::new(0, 0, 0)],
    )
}

/// A camera at `eye` turned by `yaw` and `pitch` (`Transform::from_yaw_pitch`).
struct Shot {
    name: &'static str,
    eye: [f64; 3],
    yaw: f32,
    pitch: f32,
}

/// The views of the two-realm scene. Eye coordinates are multiples of 1/64 (exact in `f64` at
/// every shift).
const PLANE_SHOTS: [Shot; 3] = [
    // Down at the slab's top faces (y = 0): nothing else in view.
    Shot {
        name: "top",
        eye: [8.0, 4.0, 8.25],
        yaw: 0.0,
        pitch: -1.5,
    },
    // Along -x at the slab's +x side (x = 16, y in -3..0), from under the plane.
    Shot {
        name: "side",
        eye: [20.5, -1.5, 8.0],
        yaw: std::f32::consts::FRAC_PI_2,
        pitch: 0.0,
    },
    // Along -z at the pillar's +z side (z = 7), across the plane.
    Shot {
        name: "pillar",
        eye: [25.5, 0.25, 20.0],
        yaw: 0.0,
        pitch: 0.0,
    },
];

/// The views of the slab: straight down at its top and straight up at its bottom, both 3 blocks
/// away from the face over its middle. Screen right is +x in both and screen up -z looking
/// down, +z looking up: the bottom view is the top view mirrored top to bottom.
const SLAB_SHOTS: [Shot; 2] = [
    Shot {
        name: "above",
        eye: [8.0, 12.0, 8.0],
        yaw: 0.0,
        pitch: -std::f32::consts::FRAC_PI_2,
    },
    Shot {
        name: "below",
        eye: [8.0, 5.0, 8.0],
        yaw: 0.0,
        pitch: std::f32::consts::FRAC_PI_2,
    },
];

/// The view parameters of `shot` moved by `shift`, with `light` and the background [`CLEAR`].
fn view_params(shot: &Shot, shift: I64Vec3, light: SceneLight) -> ViewParams {
    let camera = Camera::default();
    let eye = DVec3::from_array(shot.eye) + shift.as_dvec3();
    let transform = Transform::from_yaw_pitch(eye, shot.yaw, shot.pitch);
    let cv = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams {
        light,
        ..ViewParams::from_camera(&cv, &camera, CLEAR, DistanceFog::off())
    }
}

/// The realm entry of `light` at `time`, as the render plugin makes it.
fn shading(light: &RealmLight, time: &TimeOfDay) -> RealmShading {
    let lighting = RealmLighting {
        realms: vec![*light],
    };
    let lit = lighting.at(0, &DaylightCurve::default(), time);
    RealmShading {
        sky: Some(lit.sky_light),
        ambient: Some(lit.ambient),
        face_shade: lit.face_shade,
    }
}

/// The scene light of the default curve at `time`, with `realms`.
fn scene_light(time: &TimeOfDay, realms: SceneRealms) -> SceneLight {
    let daylight = DaylightCurve::default().at(time);
    SceneLight {
        sky: daylight.sky_light,
        ambient: daylight.ambient,
        realms,
        ..SceneLight::noon()
    }
}

/// A realm with no night: the default curve's day at every hour.
fn constant_realm() -> RealmLight {
    let day = DaylightCurve::default();
    RealmLight {
        curve: Some(DaylightCurve {
            dusk: day.day,
            night: day.day,
            ..day
        }),
        ..RealmLight::default()
    }
}

/// The light configurations of the two-realm scene, in render order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Config {
    /// One realm (no plane) at noon.
    NoonOne,
    /// One realm at midnight.
    MidnightOne,
    /// The two realms at noon.
    NoonTwo,
    /// The two realms at midnight.
    MidnightTwo,
    /// The two realms at midnight, plus planes `2^40` above and below with garish realms.
    MidnightFar,
}

const CONFIGS: [Config; 5] = [
    Config::NoonOne,
    Config::MidnightOne,
    Config::NoonTwo,
    Config::MidnightTwo,
    Config::MidnightFar,
];

/// The scene light of `config` with the plane at `plane`.
fn plane_light(config: Config, plane: i64) -> SceneLight {
    let noon = TimeOfDay::at_clock(12.0);
    let midnight = TimeOfDay::at_clock(0.0);
    let time = match config {
        Config::NoonOne | Config::NoonTwo => &noon,
        _ => &midnight,
    };
    let lower = shading(&constant_realm(), time);
    let upper = shading(&RealmLight::default(), time);
    let garish = RealmShading {
        sky: Some([1.0, 0.0, 0.0]),
        ambient: Some([0.0, 1.0, 0.0]),
        face_shade: [0.1; 6],
    };
    let realms = match config {
        Config::NoonOne | Config::MidnightOne => SceneRealms::default(),
        Config::NoonTwo | Config::MidnightTwo => {
            let mut r = SceneRealms::with_planes(&[plane]);
            r.realms[0] = lower;
            r.realms[1] = upper;
            r
        }
        Config::MidnightFar => {
            let mut r = SceneRealms::with_planes(&[plane - (1 << 40), plane, plane + (1 << 40)]);
            r.realms[0] = garish;
            r.realms[1] = lower;
            r.realms[2] = upper;
            r.realms[3] = garish;
            r.drawn = 1;
            r
        }
    };
    scene_light(time, realms)
}

// ---------------------------------------------------------------- rendering

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_realms"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// Uploads `meshes` moved by `shift` on one tier and renders each view (its view moved too).
fn render_views(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    registry: &BlockRegistry,
    meshes: &[(ChunkPos, Arc<ChunkMesh>)],
    shift: I64Vec3,
    views: &[ViewParams],
) -> Result<Vec<RgbaImage>, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        chunk_memory: boxel_render::ChunkMemorySettings {
            initial_quads: 1 << 16,
            initial_slots: 64,
            ..boxel_render::ChunkMemorySettings::default()
        },
        ..RendererConfig::default()
    };
    let mut renderer = VoxelRenderer::new(
        &device,
        &queue,
        tier,
        &config,
        &boxel_render::generate_block_textures(registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    let chunk_shift = shift / 32;
    for (i, (pos, mesh)) in meshes.iter().enumerate() {
        let moved = ChunkPos::new(
            pos.x + chunk_shift.x,
            pos.y + chunk_shift.y,
            pos.z + chunk_shift.z,
        );
        renderer
            .upsert_chunk(i as u64 + 1, moved.origin().as_i64vec3(), Arc::clone(mesh))
            .map_err(|e| format!("upsert {pos:?}: {e}"))?;
    }
    let mut images = Vec::new();
    for view in views {
        render(&device, &queue, &mut renderer, view)?; // warm-up
        images.push(render(&device, &queue, &mut renderer, view)?);
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(images)
}

// ---------------------------------------------------------------- checks

struct Report {
    failures: Vec<String>,
    dir: PathBuf,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_realms"),
        }
    }

    fn save(&self, images: &[(&str, &RgbaImage)]) {
        let _ = std::fs::create_dir_all(&self.dir);
        for (name, image) in images {
            let file: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let path = self.dir.join(format!("{file}.png"));
            match image.save_png(&path) {
                Ok(()) => eprintln!("gpu_realms: saved {}", path.display()),
                Err(e) => eprintln!("gpu_realms: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_realms: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }

    fn check(&mut self, ok: bool, message: impl FnOnce() -> String, images: &[(&str, &RgbaImage)]) {
        if !ok {
            self.fail(message(), images);
        }
    }

    /// `a` and `b` give the same pixels: exactly, or (`software`) within the software
    /// tolerance.
    fn same(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage), software: bool) {
        let (tolerance, max) = if software {
            (2, MAX_SOFTWARE_DIFF)
        } else {
            (0, 0.0)
        };
        let diff = a.1.compare(b.1, tolerance);
        if diff.fraction() > max {
            self.fail(
                format!(
                    "{what}: {} of {} pixels differ (max channel {})",
                    diff.differing_pixels, diff.total_pixels, diff.max_channel_diff
                ),
                &[a, b],
            );
        }
    }

    fn finish(self, rendered: usize) {
        assert!(
            self.failures.is_empty(),
            "{} failures:\n{}",
            self.failures.len(),
            self.failures.join("\n")
        );
        assert!(
            rendered > 0 || !gpu_tests_required(),
            "BOXEL_REQUIRE_GPU=1 but no tier could render"
        );
    }
}

fn pixels(image: &RgbaImage) -> &[[u8; 4]] {
    image.data.as_chunks::<4>().0
}

/// Whether a pixel shows the background.
fn is_clear(p: [u8; 4]) -> bool {
    p[0] == 255 && p[1] == 0 && p[2] == 255
}

/// The pixels of each class at midnight with two realms: lit as at noon (the lower realm), dark
/// as at midnight in one realm (the upper realm), as `(row, column, lit)` for every voxel
/// pixel. `None` when a pixel is neither.
fn classify(
    noon: &RgbaImage,
    midnight: &RgbaImage,
    two: &RgbaImage,
) -> Option<Vec<(u32, u32, bool)>> {
    let mut out = Vec::new();
    for (i, ((n, m), t)) in pixels(noon)
        .iter()
        .zip(pixels(midnight))
        .zip(pixels(two))
        .enumerate()
    {
        if is_clear(*t) {
            if !is_clear(*n) {
                return None;
            }
            continue;
        }
        let (row, column) = (i as u32 / noon.width, i as u32 % noon.width);
        match (t == n, t == m) {
            (true, false) => out.push((row, column, true)),
            (false, true) => out.push((row, column, false)),
            _ => return None,
        }
    }
    Some(out)
}

/// The checks of one tier's images of the two-realm scene near the origin: `images[c][v]` is
/// config `CONFIGS[c]`, view `PLANE_SHOTS[v]`.
fn check_planes(report: &mut Report, what: &str, images: &[Vec<RgbaImage>]) {
    let at = |c: Config, v: usize| {
        let i = CONFIGS.iter().position(|x| *x == c).expect("config");
        &images[i][v]
    };
    for (v, shot) in PLANE_SHOTS.iter().enumerate() {
        let name = |c: Config| format!("{what}_{}_{c:?}", shot.name);
        let (noon, midnight) = (at(Config::NoonOne, v), at(Config::MidnightOne, v));
        let (noon2, two, far) = (
            at(Config::NoonTwo, v),
            at(Config::MidnightTwo, v),
            at(Config::MidnightFar, v),
        );
        let named = [
            (name(Config::NoonOne), noon),
            (name(Config::MidnightOne), midnight),
            (name(Config::MidnightTwo), two),
        ];
        let named: Vec<(&str, &RgbaImage)> = named.iter().map(|(n, i)| (n.as_str(), *i)).collect();
        // At noon the constant realm is the day: as one realm, exactly.
        report.same(
            &format!("{what} {}: noon, two realms vs one", shot.name),
            (&name(Config::NoonTwo), noon2),
            (&name(Config::NoonOne), noon),
            false,
        );
        // Far planes clamp: the same pixels as without them.
        report.same(
            &format!("{what} {}: planes 2^40 away", shot.name),
            (&name(Config::MidnightFar), far),
            (&name(Config::MidnightTwo), two),
            false,
        );
        let Some(classes) = classify(noon, midnight, two) else {
            report.fail(
                format!(
                    "{what} {}: a pixel at midnight is neither the noon nor the midnight one",
                    shot.name
                ),
                &named,
            );
            continue;
        };
        let lit = classes.iter().filter(|c| c.2).count();
        let dark = classes.len() - lit;
        eprintln!(
            "gpu_realms: {what} {}: {lit} lit and {dark} dark pixels at midnight",
            shot.name
        );
        let ok = match shot.name {
            // The top faces under the plane take the upper realm's night.
            "top" => lit == 0 && dark as u32 == WIDTH * HEIGHT,
            // The lower realm has no night.
            "side" => dark == 0 && lit > 1000,
            // Dark above the plane, lit under it (screen rows grow downward), in the middle
            // columns, where only the pillar shows (the slab shows at the left, farther away).
            _ => {
                let middle: Vec<_> = classes
                    .iter()
                    .filter(|c| c.1.abs_diff(WIDTH / 2) < 8)
                    .collect();
                let last_dark = middle.iter().filter(|c| !c.2).map(|c| c.0).max();
                let first_lit = middle.iter().filter(|c| c.2).map(|c| c.0).min();
                let (lit, dark) = (first_lit.is_some(), last_dark.is_some());
                lit && dark && last_dark < first_lit
            }
        };
        report.check(
            ok,
            || format!("{what} {}: {lit} lit and {dark} dark pixels", shot.name),
            &named,
        );
        // Midnight is darker than noon where the realm follows the day.
        report.check(
            noon != midnight,
            || format!("{what} {}: midnight looks like noon", shot.name),
            &named,
        );
    }
}

#[test]
fn two_realms_light_by_their_own_curves_on_every_tier_and_far_away() {
    let registry = boxel_demo::blocks::registry();
    let meshes = plane_scene(&registry);
    assert_eq!(meshes.len(), 2);
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let views = |shift: I64Vec3| -> Vec<ViewParams> {
        CONFIGS
            .iter()
            .flat_map(|&c| {
                PLANE_SHOTS
                    .iter()
                    .map(move |shot| view_params(shot, shift, plane_light(c, shift.y)))
            })
            .collect()
    };
    let split = |images: Vec<RgbaImage>| -> Vec<Vec<RgbaImage>> {
        images
            .chunks(PLANE_SHOTS.len())
            .map(<[RgbaImage]>::to_vec)
            .collect()
    };
    let mut report = Report::new();
    let save_all = std::env::var_os("GPU_REALMS_SAVE").is_some();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let software = caps.info.device_type == wgpu::DeviceType::Cpu;
        let mut tiers: Vec<(Tier, Vec<Vec<RgbaImage>>)> = Vec::new();
        for tier in allowed_tiers(&caps) {
            let near = match render_views(
                adapter,
                &caps,
                tier,
                &registry,
                &meshes,
                I64Vec3::ZERO,
                &views(I64Vec3::ZERO),
            ) {
                Ok(images) => split(images),
                Err(e) => {
                    report.fail(format!("{label} {tier}: {e}"), &[]);
                    continue;
                }
            };
            let what = format!("{label}_{tier}");
            if save_all {
                for (c, row) in CONFIGS.iter().zip(&near) {
                    for (shot, image) in PLANE_SHOTS.iter().zip(row) {
                        report.save(&[(&format!("{what}_{}_{c:?}", shot.name), image)]);
                    }
                }
            }
            check_planes(&mut report, &what, &near);
            for shift in FAR_SHIFTS {
                match render_views(
                    adapter,
                    &caps,
                    tier,
                    &registry,
                    &meshes,
                    shift,
                    &views(shift),
                ) {
                    Ok(moved) => {
                        for ((c, a), b) in CONFIGS.iter().zip(&near).zip(split(moved)) {
                            for ((shot, a), b) in PLANE_SHOTS.iter().zip(a).zip(&b) {
                                report.same(
                                    &format!("{what} {} {c:?} shift {shift}", shot.name),
                                    (&format!("{what}_{}_{c:?}", shot.name), a),
                                    (&format!("{what}_{}_{c:?}_shift{shift}", shot.name), b),
                                    false,
                                );
                            }
                        }
                    }
                    Err(e) => report.fail(format!("{label} {tier} shift {shift}: {e}"), &[]),
                }
            }
            rendered += 1;
            tiers.push((tier, near));
        }
        let Some(((best, b), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, t) in rest {
            for ((c, a), b) in CONFIGS.iter().zip(b).zip(t) {
                for ((shot, a), b) in PLANE_SHOTS.iter().zip(a).zip(b) {
                    report.same(
                        &format!("{label}: {} {c:?} {tier} vs {best}", shot.name),
                        (&format!("{label}_{best}_{}_{c:?}", shot.name), a),
                        (&format!("{label}_{tier}_{}_{c:?}", shot.name), b),
                        software,
                    );
                }
            }
        }
    }
    report.finish(rendered);
}

/// `image` mirrored top to bottom.
fn flipped(image: &RgbaImage) -> RgbaImage {
    let row = image.width as usize * 4;
    let data = image.data.chunks(row).rev().flatten().copied().collect();
    RgbaImage { data, ..*image }
}

/// sRGB 8-bit to linear.
fn linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear to sRGB 8-bit.
fn srgb(l: f64) -> f64 {
    let c = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    c * 255.0
}

#[test]
fn a_flat_realm_shades_the_top_and_the_bottom_alike_on_every_tier() {
    let registry = boxel_demo::blocks::registry();
    let meshes = slab_scene(&registry);
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let noon = TimeOfDay::at_clock(12.0);
    let flat = {
        let mut r = SceneRealms::default();
        r.realms[0].face_shade = [1.0; 6];
        r
    };
    // Above and below, with the default table, then with the flat one.
    let views: Vec<ViewParams> = [SceneRealms::default(), flat]
        .iter()
        .flat_map(|&realms| {
            SLAB_SHOTS
                .iter()
                .map(move |shot| view_params(shot, I64Vec3::ZERO, scene_light(&noon, realms)))
        })
        .collect();
    let mut report = Report::new();
    let save_all = std::env::var_os("GPU_REALMS_SAVE").is_some();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let software = caps.info.device_type == wgpu::DeviceType::Cpu;
        let mut tiers: Vec<(Tier, Vec<RgbaImage>)> = Vec::new();
        for tier in allowed_tiers(&caps) {
            let images = match render_views(
                adapter,
                &caps,
                tier,
                &registry,
                &meshes,
                I64Vec3::ZERO,
                &views,
            ) {
                Ok(images) => images,
                Err(e) => {
                    report.fail(format!("{label} {tier}: {e}"), &[]);
                    continue;
                }
            };
            let what = format!("{label}_{tier}");
            let [above, below, above_flat, below_flat] = &images[..] else {
                report.fail(format!("{what}: {} images", images.len()), &[]);
                continue;
            };
            let names =
                ["above", "below", "above_flat", "below_flat"].map(|n| format!("{what}_{n}"));
            let named: Vec<(&str, &RgbaImage)> = names
                .iter()
                .map(String::as_str)
                .zip(images.iter())
                .collect();
            if save_all {
                report.save(&named);
            }
            report.check(
                pixels(above).iter().all(|p| !is_clear(*p)),
                || format!("{what}: the slab does not fill the view"),
                &named,
            );
            // The top's shade is 1 in both tables.
            report.same(
                &format!("{what}: the top, flat vs default"),
                (&names[2], above_flat),
                (&names[0], above),
                false,
            );
            // Flat: the bottom, mirrored, is the top.
            let mirrored = flipped(below_flat);
            report.same(
                &format!("{what}: flat, the bottom mirrored vs the top"),
                (&format!("{}_mirrored", names[3]), &mirrored),
                (&names[2], above_flat),
                false,
            );
            // Default: the bottom at half the light of the flat bottom, per pixel (within the
            // rounding of 8 bits).
            let worst = pixels(below)
                .iter()
                .zip(pixels(below_flat))
                .flat_map(|(d, f)| {
                    (0..3).map(move |c| (srgb(linear(f[c]) * 0.5) - f64::from(d[c])).abs())
                })
                .fold(0.0, f64::max);
            eprintln!(
                "gpu_realms: {what}: the default bottom is half the flat one within {worst:.2} levels"
            );
            report.check(
                worst <= 1.5,
                || format!("{what}: the default bottom is {worst:.2} levels off half the flat one"),
                &named,
            );
            rendered += 1;
            tiers.push((tier, images));
        }
        let Some(((best, b), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, t) in rest {
            for (i, (a, b)) in b.iter().zip(t).enumerate() {
                report.same(
                    &format!("{label}: slab view {i} {tier} vs {best}"),
                    (&format!("{label}_{best}_slab{i}"), a),
                    (&format!("{label}_{tier}_slab{i}"), b),
                    software,
                );
            }
        }
    }
    report.finish(rendered);
}
