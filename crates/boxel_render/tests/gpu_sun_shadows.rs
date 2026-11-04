//! Sun shadow maps (ADR 0065), part 1 (lane S3): the casters' depth, without receivers. Self-skips
//! without an adapter unless `BOXEL_REQUIRE_GPU=1`.
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
//! - the main image is byte-identical with shadows on and off (no receiver yet), the missed
//!   caster of the dark chunk included; at night nothing is drawn;
//! - the same scene at shifts that are multiples of 1024 voxels, up to the world's limit, gives
//!   the same maps bit for bit;
//! - shadows off allocate nothing; on Mid and Low, shadows on draw no pass, allocate nothing and
//!   give the same image byte for byte.
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

/// Looking down at the scene from the south (+z), shifted by `shift`, the sun at `hours`
/// (`None`: no sun), realm planes `planes` (unshifted). The view is camera-relative, so a
/// shifted one is the same view with its render origin moved: exact at any shift, also where
/// an `f64` camera position no longer is (near the world's limit).
fn view(shift: I64Vec3, hours: Option<f64>, planes: &[i64]) -> ViewParams {
    let eye = DVec3::new(16.5, 16.5, 36.5);
    let transform = Transform::from_yaw_pitch(eye, 0.1, -0.45);
    let camera = Camera::default();
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    let mut v = ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default());
    v.render_origin += shift;
    let realms = SceneRealms::with_planes(&planes.iter().map(|p| p + shift.y).collect::<Vec<_>>());
    v.light.realms = realms;
    v.light.sun = hours.map(|h| {
        let realm = realms.realm_of(v.render_origin.y);
        SceneSun::new(sun_at(h), 0.4, |_| true, realm)
    });
    v
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
    VoxelRenderer::new(
        device,
        queue,
        tier,
        &config,
        &boxel_render::generate_block_textures(b.registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))
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
        self.failures.push(message);
    }

    /// Fails with `message` unless `ok`.
    fn expect(&mut self, ok: bool, message: impl FnOnce() -> String) {
        if !ok {
            self.fail(message(), &[]);
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
        report.same_image(&format!("{label}_pillar_{hours}"), &image, &base);
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
    // The dark chunk's missed caster keeps today's pixels.
    r.set_sun_shadows(device, &SunShadowSettings::default());
    let room_off = shoot(device, queue, &mut r, &v)?;
    report.same_image(&format!("{label}_dark_chunk"), &room_on, &room_off);
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

#[test]
fn sun_shadow_maps_hold_the_casters_depth_on_high_only() {
    let b = Blocks::new();
    let m = Meshes {
        pillar: mesh(&b.registry, &pillar(&b), false),
        clear: mesh(&b.registry, &clear(&b), false),
        leaves: mesh(&b.registry, &square(b.leaves), false),
        stones: mesh(&b.registry, &square(b.stone), false),
        room: mesh(&b.registry, &room(&b), true),
        room_open: mesh(&b.registry, &room(&b), false),
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
                check_high(&mut report, &label, adapter, &caps, &b, &m)
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
