//! Sun shadows (ADR 0065): the maps (part 1, lane S3) and the receivers (part 2, lane S4).
//! Self-skips without an adapter unless `BOXEL_REQUIRE_GPU=1`.
//!
//! Hand scenes meshed by the real `Mesher` in the origin chunk: a stone floor, a pillar on it and
//! a slab floating above it, open to the sky (no light: every face has sky 15). For every adapter
//! (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) whose High tier a local capability check
//! allows (the one of `gpu_tiers.rs`), with shadows on (512 texels, on integrated GPUs too):
//!
//! - every texel of every cascade's map holds the depth of the first back face a CPU ray from
//!   the sun finds in the scene's boxes (within the depth bias), or the far depth 1 where the ray
//!   meets nothing; texels within 2 texels of an edge are not judged, and at most
//!   [`MAX_MISMATCH`] of the rest may disagree. At 15:00 and at 17:00 (long shadows);
//! - glass, water and plants cast nothing: their maps stay at 1; leaves cast through the alpha
//!   test, never more than a stone block in their place;
//! - a chunk without a sky-lit face, and a sky-lit chunk in another realm than the camera's,
//!   draw nothing (the cull's draw counts equal the scene without them, and the same chunk lit
//!   or in the camera's realm does draw);
//! - with shadows the main image is never brighter than without; at night nothing is drawn;
//! - the same scene at shifts that are multiples of 1024 voxels, up to the world's limit, gives
//!   the same maps bit for bit;
//! - shadows off allocate nothing; on Mid and Low, shadows on draw no pass, allocate nothing and
//!   give the same image byte for byte.
//!
//! Part 2, the receivers on High, against the same scene without shadows and a CPU oracle:
//!
//! - never brighter, per pixel and channel, in every scene;
//! - the oracle: for each pixel a CPU ray from the eye finds the surface and its face, and an
//!   exact ray from there toward the sun says "shadow" or "sun" (a face looking away from the
//!   sun is in shadow). The image with shadows over the one without must say the same: about
//!   0.6 of the sky light in shadow, all of it in the sun. Pixels on a geometric edge or within
//!   [`EDGE_TEXELS`] texels of a shadow edge are not judged; at most [`MAX_MISMATCH`] of the
//!   rest may disagree. Scenes: the pillar at noon, 15:00 and 17:00, a wall with a window under
//!   an overhang (from the south and from the north), a pillar's shadow on a pond;
//! - a canopy of leaves casts a dappled shadow: fewer shadowed pixels than the same canopy of
//!   stone, and some;
//! - byte-identical where nothing can change: inside a closed cave (no sky light), at midnight,
//!   with strength 0, in a realm without the sun, with the camera in another realm than all it
//!   sees, and past the shadow distance;
//! - the same image at shifts that are multiples of 1024 voxels, up to the world's limit; at a
//!   shift that is not, at most [`MAX_MISMATCH`] of the pixels differ (shadow edges move by a
//!   fraction of a texel).
//!
//! Failing maps are reported as counts; failing images are saved under
//! `CARGO_TARGET_TMPDIR/gpu_sun_shadows/`.
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_light::{LightInput, LightScratch, PaddedLight, SkyInput};
use boxel_math::glam::DVec4;
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::shadow::{Cascade, ShadowCascades};
use boxel_render::{
    DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, SceneRealms, SceneSun, SunShadowSettings,
    ViewParams, VoxelRenderer,
};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, LightTables, LocalPos, MAX_BLOCK_COORD,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Texels per side of each map in these tests (a quarter of the default, for quick reads).
const RESOLUTION: u32 = 512;
/// Of the judged texels of a map, at most this fraction may disagree with the CPU ray.
const MAX_MISMATCH: f64 = 0.005;
/// A texel is judged only if the rays this many texels away meet the same face (or nothing).
const EDGE_TEXELS: f64 = 2.0;

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
        label: Some("gpu_sun_shadows"),
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

/// A solid box of a scene, in blocks from the chunk's min corner.
#[derive(Clone, Copy, Debug)]
struct Solid {
    min: DVec3,
    max: DVec3,
}

impl Solid {
    const fn new(min: [f64; 3], max: [f64; 3]) -> Self {
        Self {
            min: DVec3::new(min[0], min[1], min[2]),
            max: DVec3::new(max[0], max[1], max[2]),
        }
    }
}

/// The floor (stone, y 2 to 4), the pillar on it (stone, 6 high) and a bottom slab floating at
/// y = 8: the boxes the CPU rays meet. The floor and the pillar touch: the face between them is
/// not meshed, so they are one solid for the rays too.
const PILLAR_SCENE: [Solid; 3] = [
    Solid::new([4.0, 2.0, 4.0], [28.0, 4.0, 28.0]),
    Solid::new([12.0, 4.0, 12.0], [13.0, 10.0, 13.0]),
    Solid::new([20.0, 8.0, 16.0], [21.0, 8.5, 17.0]),
];

/// The blocks of the scenes.
struct Blocks {
    registry: BlockRegistry,
    stone: BlockState,
    slab: BlockState,
    glass: BlockState,
    water: BlockState,
    poppy: BlockState,
    short_grass: BlockState,
    leaves: BlockState,
}

impl Blocks {
    fn new() -> Self {
        let registry = boxel_demo::blocks::registry();
        let s = |name: &str| registry.by_name(name).unwrap_or_else(|| panic!("{name}"));
        Self {
            stone: s("boxel:stone"),
            slab: s("boxel:stone_slab"),
            glass: s("boxel:glass"),
            water: s("boxel:water"),
            poppy: s("boxel:poppy"),
            short_grass: s("boxel:short_grass"),
            leaves: s("boxel:leaves"),
            registry,
        }
    }
}

/// A chunk whose block at local `(x, y, z)` is `block(x, y, z)`.
fn chunk(block: impl Fn(i64, i64, i64) -> BlockState) -> ChunkData {
    let dense: Vec<BlockState> = (0..CHUNK_VOLUME)
        .map(|i| {
            let p = LocalPos::from_index(i);
            block(i64::from(p.x()), i64::from(p.y()), i64::from(p.z()))
        })
        .collect();
    ChunkData::from_dense(&dense)
}

/// The mesh of `chunk` alone, open to the sky (no light: sky 15 everywhere), or (`dark`) lit
/// as an underground chunk is: no sky light on any face.
fn mesh(registry: &BlockRegistry, chunk: &ChunkData, dark: bool) -> ChunkMesh {
    let materials = MaterialTable::from_registry(registry);
    let mut light = PaddedLight::default();
    if dark {
        let tables = LightTables::from_registry(registry);
        let mut chunks = [None; 27];
        chunks[boxel_light::box_light::chunk_index(0, 0, 0)] = Some(chunk);
        let input = LightInput {
            origin: boxel_voxel::ChunkPos::new(0, 0, 0),
            chunks,
            sky: SkyInput::BelowFloor,
            beams: [None; 9],
            tables: &tables,
            colored_sky: false,
        };
        boxel_light::compute(&input, &mut LightScratch::new(), &mut light);
    }
    let input = MeshInput {
        center: chunk,
        neighbors: [None; 26],
        light: dark.then_some(&light),
    };
    Mesher::new().mesh_chunk(&input, &materials, MeshOptions::default())
}

/// [`PILLAR_SCENE`]'s chunk.
fn pillar(b: &Blocks) -> ChunkData {
    chunk(|x, y, z| match (x, y, z) {
        (4..28, 2..4, 4..28) | (12, 4..10, 12) => b.stone,
        (20, 8, 16) => b.slab,
        _ => BlockState::AIR,
    })
}

/// Glass, water and two plants floating apart: nothing that casts.
fn clear(b: &Blocks) -> ChunkData {
    chunk(|x, y, z| match (x, y, z) {
        (10, 10, 10) => b.glass,
        (14, 10, 10) => b.water,
        (18, 10, 10) => b.poppy,
        (22, 10, 10) => b.short_grass,
        _ => BlockState::AIR,
    })
}

/// A floating 3 x 3 square of `block` at y = 10.
fn square(block: BlockState) -> ChunkData {
    chunk(|x, y, z| match (x, y, z) {
        (14..17, 10, 14..17) => block,
        _ => BlockState::AIR,
    })
}

/// A closed stone room underground (the room of `gpu_dynamic_lights.rs`): dark, it casts
/// nothing, though its walls could shade a receiver (the sky-lit gate's missed caster).
fn room(b: &Blocks) -> ChunkData {
    chunk(|x, y, z| {
        let inside = (8..24).contains(&x) && (4..10).contains(&y) && (8..24).contains(&z);
        if inside { BlockState::AIR } else { b.stone }
    })
}

// ---------------------------------------------------------------- views

/// The direction toward the sun at `hours` with the default 30 degree tilt (`SunPath`).
fn sun_at(hours: f64) -> DVec3 {
    let theta = (hours - 6.0) / 24.0 * std::f64::consts::TAU;
    let (s, c) = 30f64.to_radians().sin_cos();
    DVec3::new(theta.cos(), theta.sin() * c, theta.sin() * s)
}

/// The settings of these tests: on, also on integrated GPUs, small maps.
fn settings() -> SunShadowSettings {
    SunShadowSettings {
        on_integrated: true,
        resolution: RESOLUTION,
        ..SunShadowSettings::on()
    }
}

/// Where a view's eye is (in blocks from the origin chunk's min corner, before any shift) and
/// where it looks.
#[derive(Clone, Copy, Debug)]
struct Eye {
    at: DVec3,
    yaw: f32,
    pitch: f32,
}

/// Looking down at the pillar scene from the south (+z).
const SOUTH: Eye = Eye {
    at: DVec3::new(16.5, 16.5, 36.5),
    yaw: 0.1,
    pitch: -0.45,
};

/// Looking from `eye`, shifted by `shift`, with realm planes `planes` (unshifted) and no sun.
/// The view is camera-relative, so a shifted one is the same view with its render origin moved:
/// exact at any shift, also where an `f64` camera position no longer is (near the world's
/// limit).
fn look(eye: Eye, shift: I64Vec3, planes: &[i64]) -> ViewParams {
    let transform = Transform::from_yaw_pitch(eye.at, eye.yaw, eye.pitch);
    let camera = Camera::default();
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    let mut v = ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default());
    v.render_origin += shift;
    v.light.realms =
        SceneRealms::with_planes(&planes.iter().map(|p| p + shift.y).collect::<Vec<_>>());
    v
}

/// `v` with the sun at `hours`: shadow strength `strength` in the realms `has_sun` says have
/// the sun.
fn sunlit(
    mut v: ViewParams,
    hours: f64,
    strength: f32,
    has_sun: impl Fn(usize) -> bool,
) -> ViewParams {
    let realm = v.light.realms.realm_of(v.render_origin.y);
    v.light.sun = Some(SceneSun::new(sun_at(hours), strength, has_sun, realm));
    v
}

/// [`SOUTH`], shifted by `shift`, the sun at `hours` (`None`: no sun), realm planes `planes`
/// (unshifted).
fn view(shift: I64Vec3, hours: Option<f64>, planes: &[i64]) -> ViewParams {
    let v = look(SOUTH, shift, planes);
    match hours {
        Some(h) => sunlit(v, h, 0.4, |_| true),
        None => v,
    }
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_sun_shadows"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

fn renderer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tier: Tier,
    b: &Blocks,
) -> Result<VoxelRenderer, String> {
    let config = RendererConfig {
        chunk_memory: boxel_render::ChunkMemorySettings {
            initial_quads: 1 << 16,
            initial_slots: 64,
            ..boxel_render::ChunkMemorySettings::default()
        },
        ..RendererConfig::default()
    };
    let mut r = VoxelRenderer::new(
        device,
        queue,
        tier,
        &config,
        &boxel_render::generate_block_textures(b.registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    // The slab's box (its faces index the View's box table).
    r.set_model_boxes(
        queue,
        MaterialTable::from_registry(&b.registry).model_boxes(),
    );
    Ok(r)
}

/// Puts exactly `chunks` (chunk position, mesh) in `renderer`, shifted by `shift` voxels.
fn place(
    renderer: &mut VoxelRenderer,
    shift: I64Vec3,
    chunks: &[(I64Vec3, &ChunkMesh)],
) -> Result<(), String> {
    renderer.clear_chunks();
    for (id, (pos, mesh)) in chunks.iter().enumerate() {
        renderer
            .upsert_chunk(id as u64 + 1, shift + *pos * 32, Arc::new((*mesh).clone()))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Renders `view` twice (the first frame uploads) and returns the second image.
fn shoot(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    render(device, queue, renderer, view)?;
    render(device, queue, renderer, view)
}

/// Every layer of the last frame's maps.
fn read_maps(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &VoxelRenderer,
    layers: usize,
) -> Result<Vec<Vec<f32>>, String> {
    (0..layers as u32)
        .map(|layer| {
            renderer
                .debug_read_shadow_map(device, queue, layer)
                .ok_or_else(|| "no shadow maps".to_string())?
        })
        .collect()
}

/// The sum of the last frame's shadow draw counts (every cascade, page and list).
fn shadow_draws(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &VoxelRenderer,
) -> Result<u32, String> {
    let counts = renderer
        .debug_shadow_counts(device, queue)
        .ok_or_else(|| "no shadow counts".to_string())??;
    let n = boxel_shaders::gpu::shadow::STATE_COUNTS as usize;
    if counts[n] != 0 {
        return Err(format!("{} buckets dropped", counts[n]));
    }
    Ok(counts[..n].iter().sum())
}

// ---------------------------------------------------------------- the CPU rays

/// A face of a solid: its index, axis and side.
type FaceId = (usize, usize, bool);

/// Where the ray `origin + t * dir` (`t` in `0..=1`) first leaves the union of `solids` into
/// air, with the face it leaves through: the nearest back face seen from the sun.
fn first_exit(origin: DVec3, dir: DVec3, solids: &[Solid]) -> Option<(f64, FaceId)> {
    // The span of the ray inside solid `i`, and the face it leaves through.
    let span = |i: usize| -> Option<(f64, f64, FaceId)> {
        let s = solids[i];
        let (mut t0, mut t1, mut exit) = (f64::NEG_INFINITY, f64::INFINITY, (i, 0, false));
        for axis in 0..3 {
            let (o, d) = (origin[axis], dir[axis]);
            let (lo, hi) = (s.min[axis], s.max[axis]);
            if d.abs() < 1e-12 {
                if o < lo || o > hi {
                    return None;
                }
                continue;
            }
            let (a, b) = ((lo - o) / d, (hi - o) / d);
            // Leaving through the high side when the ray runs toward it.
            let (near, far, far_hi) = if a < b { (a, b, true) } else { (b, a, false) };
            t0 = t0.max(near);
            if far < t1 {
                t1 = far;
                exit = (i, axis, far_hi);
            }
        }
        (t0 < t1).then_some((t0, t1, exit))
    };
    let entry = (0..solids.len())
        .filter_map(|i| span(i).filter(|&(_, t1, _)| t1 > 0.0).map(|s| (i, s)))
        .min_by(|a, b| a.1.0.total_cmp(&b.1.0))?;
    let (_, (_, mut out, mut face)) = entry;
    // On through the solids it touches.
    loop {
        let next = (0..solids.len())
            .filter_map(span)
            .filter(|&(t0, t1, _)| t0 <= out + 1e-9 && t1 > out + 1e-9)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match next {
            Some((_, t1, f)) => {
                out = t1;
                face = f;
            }
            None => return Some((out, face)),
        }
    }
}

/// How a map compares with the CPU rays.
#[derive(Debug, Default)]
struct MapCheck {
    /// Texels judged.
    judged: usize,
    /// Of them, with a caster.
    casters: usize,
    /// Of them, disagreeing.
    wrong: usize,
    /// The first few disagreements.
    examples: Vec<String>,
}

/// Compares `depths` (one cascade's map) with the CPU rays through `solids` (relative to the
/// frame's render origin).
fn check_map(cascade: &Cascade, depths: &[f32], solids: &[Solid]) -> MapCheck {
    let res = f64::from(RESOLUTION);
    let inverse = cascade.view_proj.as_dmat4().inverse();
    let point = |x: f64, y: f64, z: f64| {
        let p = inverse * DVec4::new(x, y, z, 1.0);
        p.truncate() / p.w
    };
    // The ray through the map at texel coordinates (column, row), from depth 0 to depth 1.
    let ray = |col: f64, row: f64| {
        let (x, y) = (2.0 * col / res - 1.0, 1.0 - 2.0 * row / res);
        let near = point(x, y, 0.0);
        let far = point(x, y, 1.0);
        first_exit(near, far - near, solids).filter(|&(t, _)| t <= 1.0)
    };
    let mut check = MapCheck::default();
    for row in 0..RESOLUTION {
        for col in 0..RESOLUTION {
            let (c, r) = (f64::from(col) + 0.5, f64::from(row) + 0.5);
            let center = ray(c, r);
            let around = [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)];
            let mut slope = 0.0_f64;
            let mut edge = false;
            for (dx, dy) in
                around
                    .into_iter()
                    .chain([(-1.0, -1.0), (1.0, 1.0), (-1.0, 1.0), (1.0, -1.0)])
            {
                let far = ray(c + dx * EDGE_TEXELS, r + dy * EDGE_TEXELS);
                let near = ray(c + dx, r + dy);
                let face = |hit: Option<(f64, FaceId)>| hit.map(|(_, f)| f);
                if face(far) != face(center) || face(near) != face(center) {
                    edge = true;
                    break;
                }
                if let (Some((t, _)), Some((n, _))) = (center, near) {
                    slope = slope.max((n - t).abs());
                }
            }
            if edge || center.is_some_and(|(t, _)| t > 1.0 - 1e-4) {
                continue;
            }
            check.judged += 1;
            let got = f64::from(depths[(row * RESOLUTION + col) as usize]);
            let ok = match center {
                None => got == 1.0,
                Some((t, _)) => {
                    check.casters += 1;
                    (got - t).abs() <= 2.0 * slope + 2e-6
                }
            };
            if !ok {
                check.wrong += 1;
                if check.examples.len() < 5 {
                    check
                        .examples
                        .push(format!("texel ({col}, {row}): {got} for {center:?}"));
                }
            }
        }
    }
    check
}

// ---------------------------------------------------------------- the checks

/// The same cascades relative to their render origins (the anchors aside).
fn same_cascades(a: &ShadowCascades, b: &ShadowCascades) -> bool {
    a.cascades() == b.cascades()
        && a.light == b.light
        && (a.bucket_mask, a.distance, a.resolution) == (b.bucket_mask, b.distance, b.resolution)
}

struct Report {
    failures: Vec<String>,
    dir: PathBuf,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_sun_shadows"),
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_sun_shadows: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }

    /// Saves `images` under the report's directory.
    fn save(&self, images: &[(&str, &RgbaImage)]) {
        let _ = std::fs::create_dir_all(&self.dir);
        for (name, image) in images {
            let file: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let path = self.dir.join(format!("{file}.png"));
            match image.save_png(&path) {
                Ok(()) => eprintln!("gpu_sun_shadows: saved {}", path.display()),
                Err(e) => eprintln!("gpu_sun_shadows: cannot save {}: {e}", path.display()),
            }
        }
    }

    /// Fails with `message` and saves `images` if `failed`; else only saves them.
    fn fail_or_save(&mut self, failed: bool, message: String, images: &[(&str, &RgbaImage)]) {
        if failed {
            self.fail(message, images);
        } else {
            self.save(images);
        }
    }

    /// Fails with `message` unless `ok`.
    fn expect(&mut self, ok: bool, message: impl FnOnce() -> String) {
        if !ok {
            self.fail(message(), &[]);
        }
    }

    /// No channel of any pixel of `on` may be brighter than in `off`.
    fn never_brighter(&mut self, what: &str, on: &RgbaImage, off: &RgbaImage) {
        let brighter = on
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .zip(off.data.as_chunks::<4>().0)
            .filter(|(a, b)| a[..3].iter().zip(&b[..3]).any(|(x, y)| x > y))
            .count();
        if brighter > 0 {
            self.fail(
                format!("{what}: {brighter} pixels brighter with shadows on"),
                &[(&format!("{what}_on"), on), (&format!("{what}_off"), off)],
            );
        }
    }

    /// `on` must equal `off` byte for byte.
    fn same_image(&mut self, what: &str, on: &RgbaImage, off: &RgbaImage) {
        if on != off {
            let diff = on.compare(off, 0);
            self.fail(
                format!(
                    "{what}: {} pixels differ with shadows on (max channel {})",
                    diff.differing_pixels, diff.max_channel_diff
                ),
                &[(&format!("{what}_on"), on), (&format!("{what}_off"), off)],
            );
        }
    }
}

/// The scenes' meshes.
struct Meshes {
    pillar: ChunkMesh,
    clear: ChunkMesh,
    leaves: ChunkMesh,
    stones: ChunkMesh,
    room: ChunkMesh,
    room_open: ChunkMesh,
}

/// High: the maps against the CPU rays, what casts and what does not, the main image, far
/// shifts, off and night.
fn check_high(
    report: &mut Report,
    label: &str,
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    b: &Blocks,
    m: &Meshes,
) -> Result<(), String> {
    let (device, queue, errors) = device_for(adapter, caps, Tier::High)?;
    let mut r = renderer(&device, &queue, Tier::High, b)?;
    let (device, queue) = (&device, &queue);
    let settings = settings();
    let layers = settings.cascades as usize;
    place(&mut r, I64Vec3::ZERO, &[(I64Vec3::ZERO, &m.pillar)])?;
    // Off: nothing exists.
    let afternoon = view(I64Vec3::ZERO, Some(15.0), &[]);
    let off = shoot(device, queue, &mut r, &afternoon)?;
    let stats = r.stats().shadows;
    report.expect(stats == boxel_render::ShadowStats::default(), || {
        format!("{label}: shadows off: {stats:?}")
    });
    report.expect(r.debug_read_shadow_map(device, queue, 0).is_none(), || {
        format!("{label}: shadows off hold maps")
    });
    r.set_sun_shadows(device, &settings);
    for hours in [15.0, 17.0] {
        let v = view(I64Vec3::ZERO, Some(hours), &[]);
        let image = shoot(device, queue, &mut r, &v)?;
        let base = if hours == 15.0 {
            off.clone()
        } else {
            r.set_sun_shadows(device, &SunShadowSettings::default());
            let base = shoot(device, queue, &mut r, &v)?;
            r.set_sun_shadows(device, &settings);
            shoot(device, queue, &mut r, &v)?;
            base
        };
        report.never_brighter(&format!("{label}_pillar_{hours}"), &image, &base);
        let stats = r.stats().shadows;
        report.expect(
            stats.cascades == settings.cascades && stats.draws > 0,
            || format!("{label}: {hours}:00: {stats:?}"),
        );
        report.expect(stats.bytes >= 4 * u64::from(RESOLUTION).pow(2) * 3, || {
            format!("{label}: {hours}:00: {} bytes", stats.bytes)
        });
        let cascades = r
            .debug_shadow_cascades()
            .ok_or_else(|| format!("{hours}:00: no cascades"))?;
        let maps = read_maps(device, queue, &r, layers)?;
        // The scene's boxes relative to the render origin (the eye's voxel).
        let origin = DVec3::new(16.0, 16.0, 36.0);
        let solids: Vec<Solid> = PILLAR_SCENE
            .iter()
            .map(|s| Solid {
                min: s.min - origin,
                max: s.max - origin,
            })
            .collect();
        for (c, (cascade, depths)) in cascades.cascades().iter().zip(&maps).enumerate() {
            let check = check_map(cascade, depths, &solids);
            eprintln!(
                "gpu_sun_shadows: {label}: {hours}:00 cascade {c}: {} texels judged, {} with a \
                 caster, {} wrong",
                check.judged, check.casters, check.wrong
            );
            report.expect(check.casters >= 150, || {
                format!(
                    "{label}: {hours}:00 cascade {c}: only {} casters",
                    check.casters
                )
            });
            report.expect(
                check.wrong as f64 <= MAX_MISMATCH * check.judged as f64,
                || {
                    format!(
                        "{label}: {hours}:00 cascade {c}: {} of {} texels wrong: {:?}",
                        check.wrong, check.judged, check.examples
                    )
                },
            );
        }
    }
    // Far shifts: the same maps, bit for bit.
    let here = read_maps(device, queue, &r, layers)?;
    let here_cascades = r.debug_shadow_cascades();
    let edge = (MAX_BLOCK_COORD & !1023) - 1024;
    for shift in [
        I64Vec3::new(1 << 31, 0, -(1 << 31)),
        I64Vec3::new(1 << 40, -(1 << 35), 1 << 35),
        I64Vec3::new(edge, -edge, edge),
    ] {
        place(&mut r, shift, &[(I64Vec3::ZERO, &m.pillar)])?;
        shoot(device, queue, &mut r, &view(shift, Some(17.0), &[]))?;
        let there = read_maps(device, queue, &r, layers)?;
        let same_cascades = match (&here_cascades, r.debug_shadow_cascades()) {
            (Some(a), Some(b)) => same_cascades(a, &b) && b.anchor - a.anchor == shift,
            _ => false,
        };
        report.expect(same_cascades, || {
            format!("{label}: {shift}: other cascades")
        });
        let same = here.iter().zip(&there).all(|(a, b)| {
            a.iter()
                .map(|d| d.to_bits())
                .eq(b.iter().map(|d| d.to_bits()))
        });
        report.expect(same, || format!("{label}: {shift}: other maps"));
    }
    // Night: nothing drawn.
    place(&mut r, I64Vec3::ZERO, &[(I64Vec3::ZERO, &m.pillar)])?;
    let night = view(I64Vec3::ZERO, Some(23.0), &[]);
    shoot(device, queue, &mut r, &night)?;
    let stats = r.stats().shadows;
    report.expect(stats.cascades == 0 && stats.draws == 0, || {
        format!("{label}: night: {stats:?}")
    });
    report.expect(r.debug_shadow_cascades().is_none(), || {
        format!("{label}: night has cascades")
    });
    // Glass, water and plants cast nothing.
    place(&mut r, I64Vec3::ZERO, &[(I64Vec3::ZERO, &m.clear)])?;
    shoot(device, queue, &mut r, &afternoon)?;
    let maps = read_maps(device, queue, &r, layers)?;
    let cast: usize = maps
        .iter()
        .map(|m| m.iter().filter(|&&d| d != 1.0).count())
        .sum();
    report.expect(r.stats().shadows.cascades > 0 && cast == 0, || {
        format!("{label}: glass, water and plants cast {cast} texels")
    });
    // Leaves cast through the alpha test, never more than stone.
    let mut covered = |mesh: &ChunkMesh| -> Result<usize, String> {
        place(&mut r, I64Vec3::ZERO, &[(I64Vec3::ZERO, mesh)])?;
        shoot(device, queue, &mut r, &afternoon)?;
        let maps = read_maps(device, queue, &r, layers)?;
        Ok(maps[1].iter().filter(|&&d| d < 1.0).count())
    };
    let leaves = covered(&m.leaves)?;
    let stones = covered(&m.stones)?;
    eprintln!("gpu_sun_shadows: {label}: leaves cover {leaves} texels, stone {stones}");
    report.expect(leaves > 0 && leaves <= stones && stones > 100, || {
        format!("{label}: leaves cover {leaves} texels, stone {stones}")
    });
    // What does not cast: a dark chunk, and a sky-lit chunk in another realm.
    let draws = |r: &mut VoxelRenderer, chunks: &[(I64Vec3, &ChunkMesh)], planes: &[i64]| {
        place(r, I64Vec3::ZERO, chunks)?;
        let v = view(I64Vec3::ZERO, Some(15.0), planes);
        let image = shoot(device, queue, r, &v)?;
        Ok::<_, String>((shadow_draws(device, queue, r)?, image, v))
    };
    let pillar = (I64Vec3::ZERO, &m.pillar);
    let (alone, _, _) = draws(&mut r, &[pillar], &[])?;
    let away = I64Vec3::new(2, 0, 0);
    let (with_room, room_on, v) = draws(&mut r, &[pillar, (away, &m.room)], &[])?;
    let (with_open_room, _, _) = draws(&mut r, &[pillar, (away, &m.room_open)], &[])?;
    report.expect(
        alone > 0 && with_room == alone && with_open_room > alone,
        || {
            format!(
                "{label}: draws {alone} alone, {with_room} with a dark chunk, {with_open_room} \
             with the same chunk sky-lit"
            )
        },
    );
    // The dark chunk adds no shadow (part 2 looks inside it).
    r.set_sun_shadows(device, &SunShadowSettings::default());
    let room_off = shoot(device, queue, &mut r, &v)?;
    report.never_brighter(&format!("{label}_dark_chunk"), &room_on, &room_off);
    r.set_sun_shadows(device, &settings);
    let above = (I64Vec3::new(0, 1, 0), &m.pillar);
    let (realms, _, _) = draws(&mut r, &[pillar, above], &[32])?;
    let (one_realm, _, _) = draws(&mut r, &[pillar, above], &[])?;
    report.expect(realms == alone && one_realm > alone, || {
        format!(
            "{label}: draws {alone} alone, {realms} with a chunk of the realm above, \
             {one_realm} with it in the camera's realm"
        )
    });
    // Off again: everything is freed.
    r.set_sun_shadows(device, &SunShadowSettings::default());
    shoot(device, queue, &mut r, &night)?;
    let stats = r.stats().shadows;
    report.expect(stats == boxel_render::ShadowStats::default(), || {
        format!("{label}: off again: {stats:?}")
    });
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(())
}

/// Mid and Low: shadows on draw nothing, allocate nothing, change nothing.
fn check_lower(
    report: &mut Report,
    label: &str,
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    b: &Blocks,
    m: &Meshes,
) -> Result<(), String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let mut r = renderer(&device, &queue, tier, b)?;
    place(&mut r, I64Vec3::ZERO, &[(I64Vec3::ZERO, &m.pillar)])?;
    let v = view(I64Vec3::ZERO, Some(15.0), &[]);
    let off = shoot(&device, &queue, &mut r, &v)?;
    r.set_sun_shadows(&device, &settings());
    let on = shoot(&device, &queue, &mut r, &v)?;
    report.same_image(&format!("{label}_{tier}"), &on, &off);
    let stats = r.stats().shadows;
    report.expect(stats == boxel_render::ShadowStats::default(), || {
        format!("{label} {tier}: {stats:?}")
    });
    report.expect(
        r.debug_shadow_cascades().is_none()
            && r.debug_read_shadow_map(&device, &queue, 0).is_none(),
        || format!("{label} {tier}: shadow resources"),
    );
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(())
}

// ---------------------------------------------------------------- part 2: the receivers

/// A wall (z 14) with a 2 x 2 window, under a roof that reaches south over the floor: the
/// boxes the oracle's rays meet.
const OVERHANG_SCENE: [Solid; 6] = [
    Solid::new([4.0, 2.0, 4.0], [28.0, 4.0, 28.0]),
    Solid::new([8.0, 4.0, 14.0], [24.0, 7.0, 15.0]),
    Solid::new([8.0, 9.0, 14.0], [24.0, 11.0, 15.0]),
    Solid::new([8.0, 7.0, 14.0], [15.0, 9.0, 15.0]),
    Solid::new([17.0, 7.0, 14.0], [24.0, 9.0, 15.0]),
    Solid::new([8.0, 11.0, 14.0], [24.0, 12.0, 20.0]),
];

/// A pond sunk one block into the floor (water in y 3, its surface at 3.875) and a thick pillar
/// on its south rim whose shadow falls on the water in the afternoon.
const POND_SCENE: [Solid; 6] = [
    Solid::new([4.0, 2.0, 4.0], [28.0, 3.0, 28.0]),
    Solid::new([4.0, 3.0, 4.0], [28.0, 4.0, 8.0]),
    Solid::new([4.0, 3.0, 20.0], [28.0, 4.0, 28.0]),
    Solid::new([4.0, 3.0, 8.0], [8.0, 4.0, 20.0]),
    Solid::new([24.0, 3.0, 8.0], [28.0, 4.0, 20.0]),
    Solid::new([10.0, 4.0, 21.0], [12.0, 14.0, 23.0]),
];

/// The pond's water surface: seen, but casting nothing.
const POND_WATER: [Solid; 1] = [Solid::new([8.0, 3.0, 8.0], [24.0, 3.875, 20.0])];

/// [`OVERHANG_SCENE`]'s chunk.
fn overhang(b: &Blocks) -> ChunkData {
    chunk(|x, y, z| match (x, y, z) {
        // The window first: the wall's arm would take it.
        (15..17, 7..9, 14) => BlockState::AIR,
        (4..28, 2..4, 4..28) | (8..24, 4..11, 14) | (8..24, 11, 14..20) => b.stone,
        _ => BlockState::AIR,
    })
}

/// [`POND_SCENE`]'s chunk.
fn pond(b: &Blocks) -> ChunkData {
    chunk(|x, y, z| match (x, y, z) {
        (8..24, 3, 8..20) => b.water,
        (4..28, 2..4, 4..28) | (10..12, 4..14, 21..23) => b.stone,
        _ => BlockState::AIR,
    })
}

/// A floor, a stone trunk and a 5 x 5 canopy of `canopy` (one block thick) over it.
fn tree(b: &Blocks, canopy: BlockState) -> ChunkData {
    chunk(|x, y, z| match (x, y, z) {
        (4..28, 2..4, 4..28) | (12, 4..9, 12) => b.stone,
        (10..15, 9, 10..15) => canopy,
        _ => BlockState::AIR,
    })
}

/// Where the ray `origin + t * dir` spans box `s`: `(t_in, t_out, the axis it enters through)`.
fn span(origin: DVec3, dir: DVec3, s: &Solid) -> Option<(f64, f64, usize)> {
    let (mut t0, mut t1, mut axis_in) = (f64::NEG_INFINITY, f64::INFINITY, 0);
    for axis in 0..3 {
        let (o, d) = (origin[axis], dir[axis]);
        if d.abs() < 1e-12 {
            if o < s.min[axis] || o > s.max[axis] {
                return None;
            }
            continue;
        }
        let (a, b) = ((s.min[axis] - o) / d, (s.max[axis] - o) / d);
        let (near, far) = if a < b { (a, b) } else { (b, a) };
        if near > t0 {
            t0 = near;
            axis_in = axis;
        }
        t1 = t1.min(far);
    }
    (t0 < t1).then_some((t0, t1, axis_in))
}

/// Where the ray from `origin` along `dir` first enters one of `solids`: `t` and the face
/// (solid, axis, high side).
fn first_entry(origin: DVec3, dir: DVec3, solids: &[Solid]) -> Option<(f64, FaceId)> {
    solids
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let (t0, _, axis) = span(origin, dir, s)?;
            (t0 > 0.0).then_some((t0, (i, axis, dir[axis] < 0.0)))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

/// The outward normal of face `(_, axis, high)`.
fn normal(face: FaceId) -> DVec3 {
    let mut n = DVec3::ZERO;
    n[face.1] = if face.2 { 1.0 } else { -1.0 };
    n
}

/// What the oracle says of a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sun {
    Lit,
    Shadow,
}

/// The sun at `p` (on a face of normal `n`) toward unit `light` through `solids`; `None` in the
/// soft step of `N . L` (0 to 0.1, with a margin), where neither answer is exact.
fn oracle(p: DVec3, n: DVec3, light: DVec3, solids: &[Solid]) -> Option<Sun> {
    let facing = n.dot(light);
    if facing <= 0.0 {
        return Some(Sun::Shadow);
    }
    if facing < 0.12 {
        return None;
    }
    let o = p + n * 1e-4;
    let blocked = solids
        .iter()
        .any(|s| span(o, light, s).is_some_and(|(_, t1, _)| t1 > 0.0));
    Some(if blocked { Sun::Shadow } else { Sun::Lit })
}

/// The oracle at `p` if every point `radius` blocks around it in the face's plane says the
/// same; `None` near a shadow edge.
fn oracle_away_from_edges(
    p: DVec3,
    n: DVec3,
    light: DVec3,
    solids: &[Solid],
    radius: f64,
) -> Option<Sun> {
    let center = oracle(p, n, light, solids)?;
    let axis = (0..3).find(|&a| n[a] != 0.0).unwrap_or(0);
    let (mut a, mut b) = (DVec3::ZERO, DVec3::ZERO);
    a[(axis + 1) % 3] = radius;
    b[(axis + 2) % 3] = radius;
    let around = [
        a,
        -a,
        b,
        -b,
        (a + b) * 0.7,
        (a - b) * 0.7,
        (b - a) * 0.7,
        -(a + b) * 0.7,
    ];
    around
        .iter()
        .all(|&d| oracle(p + d, n, light, solids) == Some(center))
        .then_some(center)
}

/// The texel size at view distance `d`: its cascade's, the larger of two in a blend band.
fn texel_at(cascades: &ShadowCascades, d: f64) -> f64 {
    let all = cascades.cascades();
    let c = all
        .iter()
        .position(|c| d < f64::from(c.end))
        .unwrap_or(all.len() - 1);
    let mut texel = f64::from(all[c].texel);
    if let Some(next) = all.get(c + 1)
        && d > f64::from(all[c].end) * 0.9
    {
        texel = texel.max(f64::from(next.texel));
    }
    texel
}

/// What the image says of a pixel: the sum of its channels with shadows over without.
fn measured(on: &[u8], off: &[u8]) -> Option<Option<Sun>> {
    let sum = |p: &[u8]| p[..3].iter().map(|&c| f64::from(c)).sum::<f64>();
    let (a, b) = (sum(on), sum(off));
    // Too dark to tell.
    if b < 60.0 {
        return None;
    }
    let ratio = a / b;
    Some(if ratio < 0.9 {
        Some(Sun::Shadow)
    } else if ratio > 0.97 {
        Some(Sun::Lit)
    } else {
        None
    })
}

/// How an image with shadows compares with the oracle.
#[derive(Debug, Default)]
struct PixelCheck {
    judged: usize,
    shadow: usize,
    lit: usize,
    /// Of the shadow ones, seen through water.
    wet_shadow: usize,
    wrong: usize,
    examples: Vec<String>,
}

/// Compares `on` (shadows) over `off` (none) with the oracle, for `view` of a scene of
/// `solids` (they cast) and `water` (seen only), in blocks from the origin chunk with the eye
/// at `eye`.
#[expect(clippy::too_many_arguments, reason = "one scene's whole description")]
fn check_pixels(
    view: &ViewParams,
    eye: DVec3,
    cascades: &ShadowCascades,
    solids: &[Solid],
    water: &[Solid],
    on: &RgbaImage,
    off: &RgbaImage,
    light: DVec3,
) -> PixelCheck {
    let base = eye.floor();
    let rel = |s: &[Solid]| -> Vec<Solid> {
        s.iter()
            .map(|s| Solid {
                min: s.min - base,
                max: s.max - base,
            })
            .collect()
    };
    let (solids, water) = (rel(solids), rel(water));
    let inverse = view.view_proj.as_dmat4().inverse();
    let origin = view.eye_rel.as_dvec3();
    let ray = |x: f64, y: f64| {
        let ndc = DVec4::new(
            2.0 * x / f64::from(WIDTH) - 1.0,
            1.0 - 2.0 * y / f64::from(HEIGHT),
            0.5,
            1.0,
        );
        let p = inverse * ndc;
        (p.truncate() / p.w - origin).normalize()
    };
    // What a ray from the eye sees: the opaque face, and the water in front of it if any.
    let seen = |x: f64, y: f64| {
        let dir = ray(x, y);
        let opaque = first_entry(origin, dir, &solids);
        let wet = first_entry(origin, dir, &water)
            .filter(|w| opaque.is_none_or(|o| w.0 < o.0))
            .map(|w| w.1);
        (dir, opaque, wet)
    };
    let mut check = PixelCheck::default();
    for py in 0..HEIGHT {
        for px in 0..WIDTH {
            let (x, y) = (f64::from(px) + 0.5, f64::from(py) + 0.5);
            let (dir, opaque, wet) = seen(x, y);
            let Some((t, face)) = opaque else {
                continue;
            };
            let key =
                |(_, o, w): (DVec3, Option<(f64, FaceId)>, Option<FaceId>)| (o.map(|o| o.1), w);
            let on_edge = [-1.0, 0.0, 1.0]
                .into_iter()
                .flat_map(|dx| [-1.0, 0.0, 1.0].map(|dy| (dx, dy)))
                .any(|(dx, dy)| key(seen(x + dx, y + dy)) != (Some(face), wet));
            if on_edge {
                continue;
            }
            let i = ((py * WIDTH + px) * 4) as usize;
            let Some(got) = measured(&on.data[i..i + 4], &off.data[i..i + 4]) else {
                continue;
            };
            // Every surface the pixel shows must say the same, away from shadow edges.
            let mut points = vec![(origin + dir * t, normal(face))];
            if let Some(w) = wet {
                let (tw, _) = first_entry(origin, dir, &water).unwrap_or((t, w));
                points.push((origin + dir * tw, DVec3::Y));
            }
            let mut truth = None;
            let mut sure = true;
            for (p, n) in points {
                let radius = EDGE_TEXELS * texel_at(cascades, (p - origin).length());
                match oracle_away_from_edges(p, n, light, &solids, radius) {
                    Some(sun) if truth.is_none_or(|t| t == sun) => truth = Some(sun),
                    _ => sure = false,
                }
            }
            let Some(truth) = truth.filter(|_| sure) else {
                continue;
            };
            check.judged += 1;
            match truth {
                Sun::Shadow => check.shadow += 1,
                Sun::Lit => check.lit += 1,
            }
            if truth == Sun::Shadow && wet.is_some() {
                check.wet_shadow += 1;
            }
            if got != Some(truth) {
                check.wrong += 1;
                if check.examples.len() < 5 {
                    check.examples.push(format!(
                        "pixel ({px}, {py}): {got:?} for {truth:?} ({:?} -> {:?})",
                        &off.data[i..i + 3],
                        &on.data[i..i + 3]
                    ));
                }
            }
        }
    }
    check
}

/// The meshes of part 2.
struct Scenes {
    pillar: ChunkMesh,
    overhang: ChunkMesh,
    pond: ChunkMesh,
    leafy: ChunkMesh,
    stony: ChunkMesh,
    room: ChunkMesh,
}

/// Renders `on` with shadows and `off` without (the same view), restoring shadows.
fn on_and_off(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    r: &mut VoxelRenderer,
    settings: &SunShadowSettings,
    view: &ViewParams,
) -> Result<(RgbaImage, RgbaImage, Option<ShadowCascades>), String> {
    r.set_sun_shadows(device, settings);
    let on = shoot(device, queue, r, view)?;
    let cascades = r.debug_shadow_cascades();
    r.set_sun_shadows(device, &SunShadowSettings::default());
    let off = shoot(device, queue, r, view)?;
    r.set_sun_shadows(device, settings);
    Ok((on, off, cascades))
}

/// High: the receivers against the oracle and the image without shadows.
fn check_receivers(
    report: &mut Report,
    label: &str,
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    b: &Blocks,
    s: &Scenes,
) -> Result<(), String> {
    let (device, queue, errors) = device_for(adapter, caps, Tier::High)?;
    let mut r = renderer(&device, &queue, Tier::High, b)?;
    let (device, queue) = (&device, &queue);
    let settings = settings();
    let save_all = std::env::var_os("GPU_SUN_SHADOWS_SAVE").is_some();
    // The oracle's scenes.
    let north = Eye {
        at: DVec3::new(16.5, 22.5, -2.5),
        yaw: std::f32::consts::PI,
        pitch: -0.75,
    };
    let under = Eye {
        at: DVec3::new(16.5, 8.5, 34.5),
        yaw: 0.0,
        pitch: -0.2,
    };
    // Each with the fewest shadowed pixels it must judge (at noon the pillar's shadow hides
    // behind it).
    type Case<'a> = (
        &'a str,
        &'a ChunkMesh,
        &'a [Solid],
        &'a [Solid],
        Eye,
        f64,
        usize,
    );
    let cases: [Case<'_>; 7] = [
        ("pillar_12", &s.pillar, &PILLAR_SCENE, &[], SOUTH, 12.0, 0),
        ("pillar_15", &s.pillar, &PILLAR_SCENE, &[], SOUTH, 15.0, 40),
        ("pillar_17", &s.pillar, &PILLAR_SCENE, &[], SOUTH, 17.0, 40),
        (
            "overhang_south",
            &s.overhang,
            &OVERHANG_SCENE,
            &[],
            under,
            15.0,
            1000,
        ),
        (
            "overhang_north",
            &s.overhang,
            &OVERHANG_SCENE,
            &[],
            north,
            15.0,
            1000,
        ),
        (
            "overhang_17",
            &s.overhang,
            &OVERHANG_SCENE,
            &[],
            north,
            17.0,
            1000,
        ),
        ("pond", &s.pond, &POND_SCENE, &POND_WATER, SOUTH, 15.0, 50),
    ];
    for (name, mesh, solids, water, eye, hours, min_shadow) in cases {
        place(&mut r, I64Vec3::ZERO, &[(I64Vec3::ZERO, mesh)])?;
        let v = sunlit(look(eye, I64Vec3::ZERO, &[]), hours, 0.4, |_| true);
        let (on, off, cascades) = on_and_off(device, queue, &mut r, &settings, &v)?;
        let what = format!("{label}_{name}");
        report.never_brighter(&what, &on, &off);
        let cascades = cascades.ok_or_else(|| format!("{name}: no cascades"))?;
        let check = check_pixels(
            &v,
            eye.at,
            &cascades,
            solids,
            water,
            &on,
            &off,
            sun_at(hours),
        );
        eprintln!(
            "gpu_sun_shadows: {label}: {name}: {} pixels judged ({} shadow, {} of them on \
             water, {} sun), {} wrong",
            check.judged, check.shadow, check.wet_shadow, check.lit, check.wrong
        );
        let bad = check.wrong as f64 > MAX_MISMATCH * check.judged as f64
            || check.shadow < min_shadow
            || check.lit < 100
            || (!water.is_empty() && check.wet_shadow < 20);
        if bad || save_all {
            report.fail_or_save(
                bad,
                format!(
                    "{what}: {} of {} pixels disagree with the oracle ({} shadow, {} sun): {:?}",
                    check.wrong, check.judged, check.shadow, check.lit, check.examples
                ),
                &[(&format!("{what}_on"), &on), (&format!("{what}_off"), &off)],
            );
        }
    }
    // Leaves cast a dappled shadow: fewer shadowed pixels than stone, and some.
    let mut shadowed = |mesh: &ChunkMesh| -> Result<usize, String> {
        place(&mut r, I64Vec3::ZERO, &[(I64Vec3::ZERO, mesh)])?;
        let v = view(I64Vec3::ZERO, Some(15.0), &[]);
        let (on, off, _) = on_and_off(device, queue, &mut r, &settings, &v)?;
        report.never_brighter(&format!("{label}_tree"), &on, &off);
        Ok(on
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .zip(off.data.as_chunks::<4>().0)
            .filter(|(a, b)| measured(&a[..], &b[..]) == Some(Some(Sun::Shadow)))
            .count())
    };
    let leafy = shadowed(&s.leafy)?;
    let stony = shadowed(&s.stony)?;
    eprintln!(
        "gpu_sun_shadows: {label}: shadowed pixels under leaves {leafy}, under stone {stony}"
    );
    report.expect(leafy > 50 && leafy < stony, || {
        format!("{label}: shadowed pixels under leaves {leafy}, under stone {stony}")
    });
    // Byte-identical where nothing can change.
    let pillar = (I64Vec3::ZERO, &s.pillar);
    let cave = Eye {
        at: DVec3::new(80.5, 7.5, 22.5),
        yaw: 0.0,
        pitch: -0.2,
    };
    let near = SunShadowSettings {
        distance: 16.0,
        ..settings
    };
    let far_eye = Eye {
        at: DVec3::new(16.5, 20.5, 64.5),
        yaw: 0.0,
        pitch: -0.35,
    };
    let still: [(
        &str,
        Vec<(I64Vec3, &ChunkMesh)>,
        ViewParams,
        SunShadowSettings,
        bool,
    ); 6] = [
        (
            "closed_cave",
            vec![pillar, (I64Vec3::new(2, 0, 0), &s.room)],
            sunlit(look(cave, I64Vec3::ZERO, &[]), 15.0, 0.4, |_| true),
            settings,
            true,
        ),
        (
            "midnight",
            vec![pillar],
            sunlit(look(SOUTH, I64Vec3::ZERO, &[]), 0.0, 0.4, |_| true),
            settings,
            false,
        ),
        (
            "strength_0",
            vec![pillar],
            sunlit(look(SOUTH, I64Vec3::ZERO, &[]), 15.0, 0.0, |_| true),
            settings,
            false,
        ),
        (
            "sunless_realm",
            vec![pillar],
            sunlit(look(SOUTH, I64Vec3::ZERO, &[]), 15.0, 0.4, |_| false),
            settings,
            false,
        ),
        (
            "other_realm",
            vec![pillar],
            sunlit(look(SOUTH, I64Vec3::ZERO, &[12]), 15.0, 0.4, |_| true),
            settings,
            true,
        ),
        (
            "past_the_distance",
            vec![pillar],
            sunlit(look(far_eye, I64Vec3::ZERO, &[]), 15.0, 0.4, |_| true),
            near,
            true,
        ),
    ];
    for (name, chunks, v, settings, drawn) in still {
        place(&mut r, I64Vec3::ZERO, &chunks)?;
        let (on, off, cascades) = on_and_off(device, queue, &mut r, &settings, &v)?;
        report.same_image(&format!("{label}_{name}"), &on, &off);
        report.expect(cascades.is_some() == drawn, || {
            format!(
                "{label}: {name}: cascades drawn {}, expected {drawn}",
                cascades.is_some()
            )
        });
    }
    // Far shifts: the same image at multiples of 1024, nearly the same elsewhere.
    r.set_sun_shadows(device, &settings);
    place(&mut r, I64Vec3::ZERO, &[pillar])?;
    let here = shoot(device, queue, &mut r, &view(I64Vec3::ZERO, Some(17.0), &[]))?;
    let edge = (MAX_BLOCK_COORD & !1023) - 1024;
    for shift in [
        I64Vec3::new(1 << 31, 0, -(1 << 31)),
        I64Vec3::new(1 << 40, -(1 << 35), 1 << 35),
        I64Vec3::new(edge, -edge, edge),
        I64Vec3::new(160, 32, -96),
    ] {
        place(&mut r, shift, &[pillar])?;
        let there = shoot(device, queue, &mut r, &view(shift, Some(17.0), &[]))?;
        if shift.x % 1024 == 0 {
            report.same_image(&format!("{label}_shift_{shift}"), &there, &here);
        } else {
            let diff = there.compare(&here, 0);
            eprintln!(
                "gpu_sun_shadows: {label}: shift {shift}: {} pixels differ",
                diff.differing_pixels
            );
            report.expect(
                diff.differing_pixels as f64 <= MAX_MISMATCH * diff.total_pixels as f64,
                || {
                    format!(
                        "{label}: shift {shift}: {} pixels differ",
                        diff.differing_pixels
                    )
                },
            );
        }
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(())
}

#[test]
fn sun_shadows_on_high_only() {
    let b = Blocks::new();
    let m = Meshes {
        pillar: mesh(&b.registry, &pillar(&b), false),
        clear: mesh(&b.registry, &clear(&b), false),
        leaves: mesh(&b.registry, &square(b.leaves), false),
        stones: mesh(&b.registry, &square(b.stone), false),
        room: mesh(&b.registry, &room(&b), true),
        room_open: mesh(&b.registry, &room(&b), false),
    };
    let scenes = Scenes {
        pillar: m.pillar.clone(),
        overhang: mesh(&b.registry, &overhang(&b), false),
        pond: mesh(&b.registry, &pond(&b), false),
        leafy: mesh(&b.registry, &tree(&b, b.leaves), false),
        stony: mesh(&b.registry, &tree(&b, b.stone), false),
        room: m.room.clone(),
    };
    // The CPU rays agree with themselves: straight down through the pillar, it leaves through
    // the floor's bottom; beside it, through the floor; the slab's bottom is half a block down.
    let down = DVec3::new(0.0, -100.0, 0.0);
    let exit = |x: f64, z: f64| first_exit(DVec3::new(x, 50.0, z), down, &PILLAR_SCENE);
    assert_eq!(
        exit(12.5, 12.5).map(|(t, f)| (50.0 - 100.0 * t, f)),
        Some((2.0, (0, 1, false)))
    );
    assert_eq!(
        exit(20.5, 16.5).map(|(t, f)| (50.0 - 100.0 * t, f)),
        Some((8.0, (2, 1, false)))
    );
    assert_eq!(exit(2.0, 2.0), None);
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let mut report = Report::new();
    let mut high = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let result = if tier == Tier::High {
                high += 1;
                check_high(&mut report, &label, adapter, &caps, &b, &m).and_then(|()| {
                    check_receivers(&mut report, &label, adapter, &caps, &b, &scenes)
                })
            } else {
                check_lower(&mut report, &label, adapter, &caps, tier, &b, &m)
            };
            if let Err(e) = result {
                report.fail(format!("{label} {tier}: {e}"), &[]);
            }
        }
    }
    assert!(
        report.failures.is_empty(),
        "{} failures:\n{}",
        report.failures.len(),
        report.failures.join("\n")
    );
    assert!(
        high > 0 || !gpu_tests_required(),
        "BOXEL_REQUIRE_GPU=1 but no adapter runs High"
    );
}
