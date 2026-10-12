//! The far LOD of ADR 0031 on every tier, with nodes built by hand from the demo forest's own LOD
//! source (`boxel_mesh::lod`, as the world side's column jobs build them). Self-skips without an
//! adapter unless `BOXEL_REQUIRE_GPU=1`; run with `--test-threads=1` like the other GPU tests.
//!
//! - **Zero cost:** a renderer without LOD input, with LOD that is off, without a horizon, with
//!   no node, and after the LOD is dropped draws the same pixels and (but with no node, where the
//!   far plane and the fog move) writes the same View uniform; no LOD draw.
//! - **Tiers:** a scene of full-detail chunks (a stone floor and a water wall, drawn after the
//!   LOD) and LOD nodes of levels 1 to 4 looks the same on every tier of an adapter.
//! - **Octant masks:** a node with some octants hidden draws exactly what the node without their
//!   quads draws, with full-detail chunks in those octants.
//! - **Seam:** a real forest world and its LOD, run by `WorldPlugin` and `LodWorldPlugin` until
//!   both settle, drawn as a renderer holds them after every op, with cave culling on and the
//!   search of the render plugin (`frame_visibility_with` and the LOD's horizon), under a magenta
//!   and a green sky: no pixel where a ray meets the ground follows the sky's color (not even
//!   through the translucent far water, issue #50), at eye level over a sea more than 8 blocks
//!   deep across the seam (whose LOD has water), looking diagonally over the keep band, and 200
//!   blocks up pitched 20 degrees down; the tiers of an adapter draw each alike. With the
//!   camera's own far plane in the search, holes show.
//! - **Far plane and fog:** a chunk past the view distance is culled without LOD and drawn with
//!   it; the demo's valley fog layer over full detail draws the same with and without LOD. The
//!   hazy valley's haze reaches the LOD's horizon over the LOD terrain, and without LOD ends at
//!   the view distance like the plain valley with LOD.
//! - **Origins:** the scene moved far from the origin, across the `i32` edges, draws the same
//!   pixels.
//! - **GL:** on GL backends the LOD stays off and the image is the one without LOD.
//! - **Smoke:** `RendererSmokeTest { lod: true }` passes on every tier at both levels; a failure
//!   turns the LOD off and keeps the tier.
//! - **Presets:** switching the arena size (far, extreme, near) rebuilds the LOD pass; refilled,
//!   it draws what a fresh pass draws, with no refusal.
//! - **Lost uploads:** a frame whose LOD staging buffer cannot be created draws none of those
//!   nodes (never stale quads), and the next frame uploads and draws them.
//! - **High, GPU-driven:** the GPU-culled lists draw what the instanced path draws, more than
//!   one segment of them included; nodes behind a wall of full detail get no draw (the depth
//!   pyramid); a cave culling search that never reaches its border draws no LOD on any tier.
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_lod/` (every image with
//! `GPU_LOD_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_demo::worldgen::ForestGenerator;
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, SmokeContext, SmokeLevel, SmokeTest, Tier, floors,
    gpu_tests_required, read_texture_rgba8, run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::lod::{LodMaterials, mesh_node, octant_of};
use boxel_mesh::{
    BUCKET_COUNT, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher, PaddedChunk,
    TRANSLUCENT_BUCKET,
};
use boxel_render::lod::{LodInput, LodOp, LodSettings};
use boxel_render::{
    DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, RendererSmokeTest, TranslucentSort, ViewParams,
    VoxelRenderer,
};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, DimensionId, LocalPos,
};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{LodNode, LodRequest, LodScratch};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_TIER_DIFF: f64 = 0.001;
/// The horizon the tests draw to.
const HORIZON: f32 = 4096.0;
/// The level-4 node edge: the scene moves in whole nodes of every level.
const TOP_EDGE: i64 = 512;
/// `gpu_terrain.rs`'s far shifts cut to whole level-4 nodes: across `+-2^31` in x, z and y, past
/// it, and `2^40` away.
const FAR_SHIFTS: [I64Vec3; 5] = [
    I64Vec3::new((1 << 31) - TOP_EDGE, 0, -(1 << 31) + TOP_EDGE),
    I64Vec3::new((1 << 31) + (1 << 20), 0, -(1 << 31) - (1 << 20)),
    I64Vec3::new(
        (1 << 40) + (1 << 31) - TOP_EDGE,
        0,
        -(1 << 40) - (1 << 31) + TOP_EDGE,
    ),
    I64Vec3::new(-(1 << 35), (1 << 31) - TOP_EDGE, 1 << 35),
    I64Vec3::new(1 << 31, -(1 << 31) - TOP_EDGE, 0),
];

// ---------------------------------------------------------------- adapters (as gpu_fog.rs)

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

type Errors = Arc<Mutex<Vec<String>>>;

/// A device shaped like the one selection would create for `tier`, collecting uncaptured
/// errors.
fn device_for(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
) -> Result<(wgpu::Device, wgpu::Queue, Errors), String> {
    let request = tier_request(tier, &caps.limits);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu_lod"),
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

fn take_errors(errors: &Errors) -> Result<(), String> {
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("uncaptured errors: {errors:?}"))
    }
}

/// One tier of one adapter.
struct Gpu<'a> {
    label: &'a str,
    tier: Tier,
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    caps: &'a AdapterCaps,
}

impl Gpu<'_> {
    /// True if the renderer keeps the LOD off here (a GL backend).
    fn gl(&self) -> bool {
        self.caps.info.backend == wgpu::Backend::Gl
    }

    fn name(&self, what: &str) -> String {
        format!("{}_{}_{what}", self.label, self.tier)
    }
}

/// Runs `check` on every tier of every adapter (in tier order, High first); returns the report.
/// Panics with every failure at `finish`.
fn for_every_tier(mut check: impl FnMut(&Gpu<'_>, &mut Report)) -> Report {
    let (_instance, adapters) = adapters();
    let mut report = Report::new();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return report;
    }
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let (device, queue, errors) = match device_for(adapter, &caps, tier) {
                Ok(d) => d,
                Err(e) => {
                    report.fail(format!("{label} {tier}: {e}"), &[]);
                    continue;
                }
            };
            let gpu = Gpu {
                label: &label,
                tier,
                device: &device,
                queue: &queue,
                caps: &caps,
            };
            check(&gpu, &mut report);
            if let Err(e) = take_errors(&errors) {
                report.fail(format!("{label} {tier}: {e}"), &[]);
            }
            report.tiers += 1;
        }
    }
    report
}

// ---------------------------------------------------------------- scenes

fn registry() -> &'static BlockRegistry {
    static REGISTRY: OnceLock<BlockRegistry> = OnceLock::new();
    REGISTRY.get_or_init(boxel_demo::blocks::registry)
}

/// The real mesher's mesh of one chunk whose blocks `block(local)` gives (air around it).
fn mesh_chunk(block: impl Fn(u32, u32, u32) -> &'static str) -> ChunkMesh {
    let registry = registry();
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for x in 0..32 {
        for y in 0..32 {
            for z in 0..32 {
                let name = block(x, y, z);
                if name != "air" {
                    dense[LocalPos::new(x, y, z).index()] = registry.by_name(name).expect(name);
                }
            }
        }
    }
    let chunk = ChunkData::from_dense(&dense);
    let input = MeshInput {
        center: &chunk,
        neighbors: [None; 26],
        light: None,
    };
    Mesher::new().mesh_chunk(
        &input,
        &MaterialTable::from_registry(registry),
        MeshOptions::default(),
    )
}

/// Full-detail chunks near the camera, `(chunk position, mesh)`: a stone floor (top at y 68)
/// under it and a free-standing water wall 40 blocks ahead (blocks y 96..120, z -42..-40), drawn
/// after the LOD with the LOD behind it.
fn full_detail() -> Vec<(I64Vec3, ChunkMesh)> {
    let floor = mesh_chunk(|_, y, _| if y < 4 { "boxel:stone" } else { "air" });
    let mut out = Vec::new();
    for cx in -1..=0 {
        for cz in -2..=0 {
            out.push((I64Vec3::new(cx, 2, cz), floor.clone()));
        }
    }
    let wall = mesh_chunk(|_, y, z| {
        if y < 24 && (22..24).contains(&z) {
            "boxel:water"
        } else {
            "air"
        }
    });
    out.push((I64Vec3::new(0, 3, -2), wall));
    out
}

/// The node of `key` from the demo forest's LOD source, meshed as a column job does.
struct NodeBuilder {
    forest: ForestGenerator,
    materials: LodMaterials,
    scratch: LodScratch,
    padded: PaddedChunk,
    mesher: Mesher,
}

impl NodeBuilder {
    fn new() -> Self {
        Self {
            forest: ForestGenerator::default(),
            materials: LodMaterials::from_registry(registry()),
            scratch: LodScratch::default(),
            padded: PaddedChunk::new(),
            mesher: Mesher::new(),
        }
    }

    /// Every non-empty node of column `[x, z]` of `level`.
    fn column(&mut self, level: u8, x: i64, z: i64, out: &mut Vec<(ChunkKey, Arc<ChunkMesh>)>) {
        let source = self.forest.lod().expect("the forest has a LOD source");
        let req = LodRequest {
            dim: DimensionId(0),
            level,
            column: [x, z],
            layers: -2..=4,
        };
        let (materials, padded, mesher) = (&self.materials, &mut self.padded, &mut self.mesher);
        source.column(&req, registry(), &mut self.scratch, &mut |y, node| {
            if let LodNode::Cells(cells) = node {
                let mut mesh = ChunkMesh::default();
                materials.fill(&cells.0[..], padded);
                mesh_node(mesher, padded, materials, level, &mut mesh);
                if !mesh.is_empty() {
                    out.push((req.key(y), Arc::new(mesh)));
                }
            }
        });
    }
}

/// LOD nodes of levels 1 to 4 in disjoint bands ahead of the camera (along -z), none in the
/// full-detail chunks: level `k` covers x in `[-2 e, 2 e)` and z in `[-4 e, -2 e)` with
/// `e = 32 * 2^k` (level 1: z from -256 to -128; level 4: from -2048 to -1024).
fn lod_nodes() -> &'static [(ChunkKey, Arc<ChunkMesh>)] {
    static NODES: OnceLock<Vec<(ChunkKey, Arc<ChunkMesh>)>> = OnceLock::new();
    NODES.get_or_init(|| {
        let mut builder = NodeBuilder::new();
        let mut out = Vec::new();
        for level in 1..=4u8 {
            for x in -2..2 {
                for z in -4..-2 {
                    builder.column(level, x, z, &mut out);
                }
            }
        }
        for level in 1..=4u8 {
            assert!(
                out.iter().any(|(k, _)| k.lod == level),
                "no level-{level} node"
            );
        }
        let quads: u32 = out.iter().map(|(_, m)| m.total_quads()).sum();
        eprintln!("gpu_lod: {} LOD nodes, {quads} quads", out.len());
        out
    })
}

/// `nodes` moved by `shift` blocks (whole level-4 nodes).
fn shifted(
    nodes: &[(ChunkKey, Arc<ChunkMesh>)],
    shift: I64Vec3,
) -> Vec<(ChunkKey, Arc<ChunkMesh>)> {
    nodes
        .iter()
        .map(|(k, m)| {
            let b = 5 + u32::from(k.lod);
            let pos = ChunkPos::new(
                k.pos.x + (shift.x >> b),
                k.pos.y + (shift.y >> b),
                k.pos.z + (shift.z >> b),
            );
            (ChunkKey { pos, ..*k }, Arc::clone(m))
        })
        .collect()
}

/// A camera at `eye` looking at `target`, for a `WIDTH x HEIGHT` target.
fn look(eye: DVec3, target: DVec3) -> ViewParams {
    let d = (target - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
}

/// The scene's camera, moved by `shift`: over the floor, through the water wall, toward the
/// LOD bands.
fn scene_view(shift: I64Vec3) -> ViewParams {
    let s = shift.as_dvec3();
    look(
        DVec3::new(16.5, 110.0, 20.5) + s,
        DVec3::new(16.0, 50.0, -600.0) + s,
    )
}

/// The default settings with an arena of `arena_quads`.
fn settings(arena_quads: u32) -> LodSettings {
    LodSettings {
        arena_quads,
        ..LodSettings::default()
    }
}

/// An input upserting `nodes` (no octant hidden), drawing to [`HORIZON`].
fn lod_input(settings: LodSettings, nodes: &[(ChunkKey, Arc<ChunkMesh>)]) -> LodInput {
    LodInput {
        ops: nodes
            .iter()
            .map(|(key, mesh)| LodOp::Upsert {
                key: *key,
                mesh: Arc::clone(mesh),
                mask: 0,
            })
            .collect(),
        ..LodInput::new(settings, Some(HORIZON))
    }
}

// ---------------------------------------------------------------- rendering

fn new_renderer(gpu: &Gpu<'_>) -> Result<VoxelRenderer, String> {
    let registry = registry();
    let config = RendererConfig {
        arena_quads: 1 << 18,
        max_chunk_slots: 256,
        translucent_sort: TranslucentSort::Inline,
    };
    VoxelRenderer::new(
        gpu.device,
        gpu.queue,
        gpu.tier,
        &config,
        &boxel_render::generate_block_textures(registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))
}

/// Uploads `chunks` moved by `shift` blocks (whole chunks).
fn load_world(
    renderer: &mut VoxelRenderer,
    chunks: &[(I64Vec3, ChunkMesh)],
    shift: I64Vec3,
) -> Result<(), String> {
    renderer.clear_chunks();
    for (i, (pos, mesh)) in chunks.iter().enumerate() {
        renderer
            .upsert_chunk(i as u64, *pos * 32 + shift, Arc::new(mesh.clone()))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Renders one frame and reads the main color back.
fn render(
    gpu: &Gpu<'_>,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(gpu.device, || {
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gpu_lod"),
            });
        renderer.render(gpu.device, gpu.queue, &mut encoder, view);
        submit_and_wait(gpu.device, gpu.queue, encoder)
    })?;
    read_texture_rgba8(gpu.device, gpu.queue, renderer.main_color())
}

/// The scene moved by `shift`: full detail, then the LOD nodes in one input.
fn render_scene(gpu: &Gpu<'_>, shift: I64Vec3) -> Result<(RgbaImage, VoxelRenderer), String> {
    let mut renderer = new_renderer(gpu)?;
    load_world(&mut renderer, &full_detail(), shift)?;
    renderer.set_lod(
        gpu.device,
        Some(lod_input(settings(1 << 20), &shifted(lod_nodes(), shift))),
    );
    let image = render(gpu, &mut renderer, &scene_view(shift))?;
    Ok((image, renderer))
}

// ---------------------------------------------------------------- reports

/// Collects failures and saves the images involved.
struct Report {
    failures: Vec<String>,
    dir: PathBuf,
    tiers: u32,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_lod"),
            tiers: 0,
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
                Ok(()) => eprintln!("gpu_lod: saved {}", path.display()),
                Err(e) => eprintln!("gpu_lod: cannot save {}: {e}", path.display()),
            }
        }
    }

    /// Saves `images` when `GPU_LOD_SAVE` is set.
    fn keep(&self, images: &[(&str, &RgbaImage)]) {
        if std::env::var_os("GPU_LOD_SAVE").is_some() {
            self.save(images);
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_lod: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }

    /// Fails if more than `max` of the pixels differ by more than `tolerance`.
    fn check_close(
        &mut self,
        what: &str,
        a: (&str, &RgbaImage),
        b: (&str, &RgbaImage),
        tolerance: u8,
        max: f64,
    ) {
        let diff = a.1.compare(b.1, tolerance);
        eprintln!(
            "gpu_lod: {what}: {:.4} % of pixels differ, max channel {}",
            diff.fraction() * 100.0,
            diff.max_channel_diff
        );
        if diff.fraction() > max {
            self.fail(
                format!(
                    "{what}: {} of {} pixels differ ({:.4} %)",
                    diff.differing_pixels,
                    diff.total_pixels,
                    diff.fraction() * 100.0
                ),
                &[a, b],
            );
        }
    }

    fn check_identical(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        if a.1.data != b.1.data {
            let diff = a.1.compare(b.1, 0);
            self.fail(
                format!(
                    "{what}: {} pixels changed (max channel {})",
                    diff.differing_pixels, diff.max_channel_diff
                ),
                &[a, b],
            );
        }
    }

    fn finish(self) {
        assert!(
            self.failures.is_empty(),
            "{} failures:\n{}",
            self.failures.len(),
            self.failures.join("\n")
        );
        assert!(
            self.tiers > 0 || !gpu_tests_required(),
            "BOXEL_REQUIRE_GPU=1 but no tier could run"
        );
    }
}

// ---------------------------------------------------------------- tests

/// Without LOD input, with LOD off (nodes resident), without a horizon, and after the LOD is
/// dropped: the same pixels and the same View uniform, no LOD draw. With no node but a horizon
/// the far plane and the terrain fog move out (the uniform changes) and a scene well inside the
/// old ones looks the same.
#[test]
fn lod_costs_nothing_when_absent_off_or_empty() {
    let chunks = full_detail();
    let nodes = lod_nodes();
    let view = scene_view(I64Vec3::ZERO);
    for_every_tier(|gpu, report| {
        let mut run = || -> Result<(), String> {
            let mut renderer = new_renderer(gpu)?;
            load_world(&mut renderer, &chunks, I64Vec3::ZERO)?;
            let plain = render(gpu, &mut renderer, &view)?;
            let uniform = renderer.debug_view_uniform();
            if renderer.take_lod_report().is_some() || renderer.lod_far().is_some() {
                return Err("a LOD pass without LOD input".into());
            }
            let mut off = lod_input(settings(1 << 20), nodes);
            off.settings.enabled = false;
            let no_horizon = LodInput::new(settings(1 << 20), None);
            for (what, input) in [("off", off), ("no horizon", no_horizon)] {
                renderer.set_lod(gpu.device, Some(input));
                let image = render(gpu, &mut renderer, &view)?;
                let stats = renderer.stats();
                if stats.lod_draws != 0 || stats.lod_drawn != 0 || renderer.lod_far().is_some() {
                    return Err(format!("{what}: the LOD draws: {stats:?}"));
                }
                if !gpu.gl() && stats.lod_nodes as usize != nodes.len() {
                    return Err(format!("{what}: {} nodes resident", stats.lod_nodes));
                }
                report.check_identical(
                    &format!("{} {}: lod {what}", gpu.label, gpu.tier),
                    (&gpu.name("plain"), &plain),
                    (&gpu.name(what), &image),
                );
                if renderer.debug_view_uniform() != uniform {
                    return Err(format!("{what}: the View uniform changed"));
                }
            }
            // No node, a horizon: the uniform moves to the LOD's far plane and fog only.
            renderer.set_lod(gpu.device, None);
            renderer.set_lod(
                gpu.device,
                Some(LodInput::new(settings(1 << 20), Some(HORIZON))),
            );
            let empty = render(gpu, &mut renderer, &view)?;
            let moved = renderer.debug_view_uniform();
            report.check_identical(
                &format!("{} {}: lod without nodes", gpu.label, gpu.tier),
                (&gpu.name("plain"), &plain),
                (&gpu.name("empty"), &empty),
            );
            // The distance fog's rule at the horizon instead of the view distance.
            let want_fog = if gpu.gl() {
                uniform.fog_params
            } else {
                let [start, end] = DistanceFog::default().range(HORIZON);
                [start, end, 0.0, 0.0]
            };
            if moved.fog_params != want_fog {
                return Err(format!("fog params {:?}", moved.fog_params));
            }
            let same = |a: &boxel_shaders::gpu::ViewUniform,
                        b: &boxel_shaders::gpu::ViewUniform| {
                let (mut a, b) = (*a, *b);
                a.fog_params = b.fog_params;
                a.frustum[5] = b.frustum[5];
                a == b
            };
            if !same(&moved, &uniform) || (moved.frustum[5] == uniform.frustum[5]) != gpu.gl() {
                return Err("the LOD changes more than the far plane and the fog".into());
            }
            if renderer.stats().lod_draws != 0 {
                return Err("draws without nodes".into());
            }
            // The distance fog off: no fade at the horizon either, a hard edge there.
            renderer.set_lod(
                gpu.device,
                Some(LodInput {
                    fog: DistanceFog::off(),
                    ..LodInput::new(settings(1 << 20), Some(HORIZON))
                }),
            );
            render(gpu, &mut renderer, &view)?;
            let [start, end] = DistanceFog::off().range(HORIZON);
            let off = renderer.debug_view_uniform().fog_params;
            if !gpu.gl() && off != [start, end, 0.0, 0.0] {
                return Err(format!("fog off: fog params {off:?}"));
            }
            renderer.set_lod(gpu.device, None);
            let dropped = render(gpu, &mut renderer, &view)?;
            report.check_identical(
                &format!("{} {}: lod dropped", gpu.label, gpu.tier),
                (&gpu.name("plain"), &plain),
                (&gpu.name("dropped"), &dropped),
            );
            let stats = renderer.stats();
            if renderer.debug_view_uniform() != uniform || stats.lod_nodes != 0 {
                return Err(format!("after the LOD: {stats:?}"));
            }
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// Full detail (a floor and a water wall drawn after the LOD) and LOD nodes of levels 1 to 4,
/// at noon, with the levels tinted and at night: every tier of an adapter draws what its best
/// tier draws. The LOD shows (it changes the image), with one draw per node in view and no
/// refusal; the tint and the night change it.
#[test]
fn every_tier_draws_the_lod_scene_alike() {
    let chunks = full_detail();
    let view = scene_view(I64Vec3::ZERO);
    let mut night = view;
    night.light.sky = [0.08, 0.08, 0.16];
    night.fog_color = [0.02, 0.02, 0.05, 1.0];
    night.clear_color = night.fog_color;
    let mut images: Vec<(String, Tier, Vec<(&str, RgbaImage)>)> = Vec::new();
    let mut report = for_every_tier(|gpu, report| {
        let run = || -> Result<Vec<(&str, RgbaImage)>, String> {
            let mut plain = new_renderer(gpu)?;
            load_world(&mut plain, &chunks, I64Vec3::ZERO)?;
            let without = render(gpu, &mut plain, &view)?;
            let (image, mut renderer) = render_scene(gpu, I64Vec3::ZERO)?;
            let stats = renderer.stats();
            let refused = renderer.take_lod_report().map(|r| r.refused);
            let mut tinted = settings(1 << 20);
            tinted.debug_tint = true;
            renderer.set_lod(gpu.device, Some(LodInput::new(tinted, Some(HORIZON))));
            let tint = render(gpu, &mut renderer, &view)?;
            renderer.set_lod(
                gpu.device,
                Some(LodInput::new(settings(1 << 20), Some(HORIZON))),
            );
            let dark = render(gpu, &mut renderer, &night)?;
            report.keep(&[
                (&gpu.name("scene"), &image),
                (&gpu.name("plain"), &without),
                (&gpu.name("tint"), &tint),
                (&gpu.name("night"), &dark),
            ]);
            let out = vec![("scene", image), ("tint", tint), ("night", dark)];
            if gpu.gl() {
                return Ok(out);
            }
            if stats.lod_draws == 0 || stats.lod_refused != 0 || refused != Some(Vec::new()) {
                return Err(format!("LOD stats {stats:?}, refused {refused:?}"));
            }
            // At most one draw per face bucket of a drawn node and two of its water (instanced),
            // or three per segment.
            if stats.lod_draws > 8 * stats.lod_drawn || stats.lod_quads == 0 {
                return Err(format!("LOD stats {stats:?}"));
            }
            // Every node went up once: the arena holds exactly the quads uploaded.
            if stats.lod_arena_used_quads == 0
                || stats.lod_upload_quads != u64::from(stats.lod_arena_used_quads)
                || stats.lod_arena_capacity_quads < stats.lod_arena_used_quads
            {
                return Err(format!("LOD arena stats {stats:?}"));
            }
            let changed = |a: &RgbaImage, b: &RgbaImage| a.compare(b, 2).fraction();
            if changed(&out[0].1, &without) < 0.05 || changed(&out[0].1, &out[1].1) < 0.05 {
                return Err("the LOD or its tint changed less than 5 % of the image".into());
            }
            Ok(out)
        };
        match run() {
            Ok(out) => images.push((gpu.label.to_string(), gpu.tier, out)),
            Err(e) => report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]),
        }
    });
    // The best tier of each adapter comes first.
    for (i, (label, tier, shots)) in images.iter().enumerate() {
        let Some((_, best_tier, best)) = images[..i].iter().find(|(l, _, _)| l == label) else {
            continue;
        };
        for ((what, image), (_, want)) in shots.iter().zip(best) {
            report.check_close(
                &format!("{label}: {what}: {tier} vs {best_tier}"),
                (&format!("{label}_{best_tier}_{what}"), want),
                (&format!("{label}_{tier}_{what}"), image),
                2,
                MAX_TIER_DIFF,
            );
        }
    }
    report.finish();
}

/// `mesh` without the quads of the octants in `mask`, and without seam marks: what is left is
/// drawn whole, as the seams by the hidden octants are (`boxel_mesh::lod::Seam`).
fn without_octants(mesh: &ChunkMesh, mask: u8) -> ChunkMesh {
    let mut quads = Vec::new();
    let mut counts = [0; BUCKET_COUNT];
    for (b, count) in counts.iter_mut().enumerate() {
        for &q in mesh.bucket(b) {
            if mask >> octant_of(q) & 1 == 0 {
                quads.push(q.with_light_index(0));
                *count += 1;
            }
        }
    }
    let mut out = ChunkMesh::from_quads(quads, counts);
    out.palette.clone_from(&mesh.palette);
    out
}

/// A level-1 node with three of its octants hidden, full-detail chunks in those octants (one
/// lod-1 octant is one chunk): exactly the pixels of the node without those octants' quads,
/// and not those of the whole node.
#[test]
fn hidden_octants_draw_nothing() {
    // The level-1 node with quads in the most octants, and three of them to hide.
    let (key, mesh) = lod_nodes()
        .iter()
        .filter(|(k, _)| k.lod == 1)
        .max_by_key(|(_, m)| {
            let octants: u8 = m.quads.iter().fold(0, |a, &q| a | 1 << octant_of(q));
            (octants.count_ones(), m.total_quads())
        })
        .expect("a level-1 node");
    let octants: u8 = mesh.quads.iter().fold(0, |a, &q| a | 1 << octant_of(q));
    assert!(octants.count_ones() >= 4, "octants {octants:08b}");
    let mut mask = 0u8;
    for o in 0..8 {
        if octants >> o & 1 != 0 && mask.count_ones() < 3 {
            mask |= 1 << o;
        }
    }
    let floor = mesh_chunk(|_, y, _| if y < 2 { "boxel:stone" } else { "air" });
    let chunks: Vec<(I64Vec3, ChunkMesh)> = (0..8)
        .filter(|o| mask >> o & 1 != 0)
        .map(|o| {
            let pos = I64Vec3::new(key.pos.x, key.pos.y, key.pos.z) * 2
                + I64Vec3::new(o & 1, o >> 1 & 1, o >> 2 & 1);
            (pos, floor.clone())
        })
        .collect();
    let center = (I64Vec3::new(key.pos.x, key.pos.y, key.pos.z) * 64 + 32).as_dvec3();
    let views = [
        look(center + DVec3::new(70.5, 60.0, 90.5), center),
        look(center + DVec3::new(-80.5, 20.0, -50.5), center),
    ];
    let filtered = Arc::new(without_octants(mesh, mask));
    for_every_tier(|gpu, report| {
        if gpu.gl() {
            return;
        }
        let mut run = || -> Result<(), String> {
            let draw = |node: &Arc<ChunkMesh>, mask: u8, view: &ViewParams| {
                let mut renderer = new_renderer(gpu)?;
                load_world(&mut renderer, &chunks, I64Vec3::ZERO)?;
                let mut input = LodInput::new(settings(1 << 20), Some(HORIZON));
                input.ops.push(LodOp::Upsert {
                    key: *key,
                    mesh: Arc::clone(node),
                    mask,
                });
                renderer.set_lod(gpu.device, Some(input));
                render(gpu, &mut renderer, view)
            };
            for (i, view) in views.iter().enumerate() {
                let masked = draw(mesh, mask, view)?;
                let reference = draw(&filtered, 0, view)?;
                let whole = draw(mesh, 0, view)?;
                report.keep(&[
                    (&gpu.name(&format!("masked{i}")), &masked),
                    (&gpu.name(&format!("whole{i}")), &whole),
                ]);
                report.check_identical(
                    &format!("{} {}: view {i}: hidden octants", gpu.label, gpu.tier),
                    (&gpu.name(&format!("masked{i}")), &masked),
                    (&gpu.name(&format!("filtered{i}")), &reference),
                );
                if whole.data == masked.data {
                    report.fail(
                        format!("{} {}: view {i}: the mask hid nothing", gpu.label, gpu.tier),
                        &[(&gpu.name(&format!("whole{i}")), &whole)],
                    );
                }
            }
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// The scene and its camera moved far from the origin (across the `i32` edges, `2^40` away) draw
/// exactly the pixels they draw near it: node origins are `i64` on the CPU and wrapped to 32
/// bits for the GPU.
#[test]
fn render_origins_far_away_draw_the_same_lod() {
    for_every_tier(|gpu, report| {
        let near = match render_scene(gpu, I64Vec3::ZERO) {
            Ok((image, _)) => image,
            Err(e) => {
                report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
                return;
            }
        };
        for shift in FAR_SHIFTS {
            match render_scene(gpu, shift) {
                Ok((far, renderer)) => {
                    if !gpu.gl() && renderer.stats().lod_draws == 0 {
                        report.fail(
                            format!("{} {} shift {shift}: no LOD draw", gpu.label, gpu.tier),
                            &[],
                        );
                    }
                    report.check_identical(
                        &format!("{} {}: shift {shift}", gpu.label, gpu.tier),
                        (&gpu.name("near"), &near),
                        (&gpu.name(&format!("shift{shift}")), &far),
                    );
                }
                Err(e) => report.fail(
                    format!("{} {} shift {shift}: {e}", gpu.label, gpu.tier),
                    &[],
                ),
            }
        }
    })
    .finish();
}

/// On GL backends (no clip control: far depth z-fights) the LOD stays off on every tier: no
/// pass, no report, the image without LOD.
#[test]
fn gl_adapters_keep_the_lod_off() {
    let chunks = full_detail();
    let view = scene_view(I64Vec3::ZERO);
    let mut gl_tiers = 0;
    for_every_tier(|gpu, report| {
        if !gpu.gl() {
            return;
        }
        gl_tiers += 1;
        let mut run = || -> Result<(), String> {
            let mut renderer = new_renderer(gpu)?;
            load_world(&mut renderer, &chunks, I64Vec3::ZERO)?;
            let plain = render(gpu, &mut renderer, &view)?;
            let uniform = renderer.debug_view_uniform();
            renderer.set_lod(gpu.device, Some(lod_input(settings(1 << 20), lod_nodes())));
            let image = render(gpu, &mut renderer, &view)?;
            let stats = renderer.stats();
            if renderer.lod_far().is_some()
                || renderer.take_lod_report().is_some()
                || stats.lod_nodes != 0
                || stats.lod_draws != 0
                || renderer.debug_view_uniform() != uniform
            {
                return Err(format!("the LOD is on: {stats:?}"));
            }
            report.check_identical(
                &format!("{} {}: GL", gpu.label, gpu.tier),
                (&gpu.name("plain"), &plain),
                (&gpu.name("lod"), &image),
            );
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
    eprintln!("gpu_lod: {gl_tiers} GL tiers checked");
}

/// `RendererSmokeTest { lod: true }` passes on every tier at both levels; a failing LOD check
/// turns the LOD off (`lod_ok` false) and keeps the tier. GL backends are not checked: the
/// renderer keeps the LOD off there.
#[test]
fn the_lod_smoke_check_passes_on_every_tier() {
    for_every_tier(|gpu, report| {
        for level in [SmokeLevel::Compile, SmokeLevel::Draw] {
            for fail in [false, true] {
                let smoke = RendererSmokeTest {
                    lod: true,
                    debug_fail_lod: fail,
                    ..RendererSmokeTest::default()
                };
                let ctx = SmokeContext {
                    device: gpu.device,
                    queue: gpu.queue,
                    tier: gpu.tier,
                    caps: gpu.caps,
                    level,
                    output_format: wgpu::TextureFormat::Rgba8UnormSrgb,
                };
                let what = format!("{} {} {level:?} fail {fail}", gpu.label, gpu.tier);
                if let Err(e) = smoke.run(&ctx) {
                    report.fail(format!("{what}: the tier failed: {e}"), &[]);
                }
                let ok = smoke.lod_ok.load(Ordering::Relaxed);
                if ok != (!fail || gpu.gl()) {
                    report.fail(format!("{what}: lod_ok {ok}"), &[]);
                }
            }
        }
    })
    .finish();
}

/// The presets' arenas one after the other (far, extreme, near): each change builds the LOD
/// pass again, empty, and the same input refills it. It then draws exactly what a fresh pass of
/// that size draws, with no refusal.
#[test]
fn switching_presets_rebuilds_the_pass_and_refills_it() {
    let chunks = full_detail();
    let view = scene_view(I64Vec3::ZERO);
    let nodes = lod_nodes();
    let quads: u64 = nodes.iter().map(|(_, m)| u64::from(m.total_quads())).sum();
    for_every_tier(|gpu, report| {
        if gpu.gl() {
            return;
        }
        let mut run = || -> Result<(), String> {
            let mut switched = new_renderer(gpu)?;
            load_world(&mut switched, &chunks, I64Vec3::ZERO)?;
            for (name, arena) in [
                ("far", 12u32 << 20),
                ("extreme", 16 << 20),
                ("near", 4 << 20),
            ] {
                switched.set_lod(gpu.device, Some(lod_input(settings(arena), nodes)));
                let image = render(gpu, &mut switched, &view)?;
                let report_after = switched.take_lod_report().ok_or("no LOD pass")?;
                let limit = gpu.device.limits().max_buffer_size / 8;
                let capacity = u64::from(arena).min(limit);
                if !report_after.refused.is_empty()
                    || report_after.arena_free_quads != capacity - quads
                    || switched.stats().lod_refused != 0
                {
                    return Err(format!("{name}: {report_after:?}, {:?}", switched.stats()));
                }
                let mut fresh = new_renderer(gpu)?;
                load_world(&mut fresh, &chunks, I64Vec3::ZERO)?;
                fresh.set_lod(gpu.device, Some(lod_input(settings(arena), nodes)));
                let want = render(gpu, &mut fresh, &view)?;
                report.check_identical(
                    &format!("{} {}: {name} after switching", gpu.label, gpu.tier),
                    (&gpu.name(&format!("{name}_fresh")), &want),
                    (&gpu.name(&format!("{name}_switched")), &image),
                );
            }
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// A frame whose LOD uploads cannot get a staging buffer (as when out of memory: wgpu reports
/// it) draws none of those nodes, never what their ranges held: the image is the one without
/// nodes. The next frame uploads them and draws what a renderer that never lost them draws.
#[test]
fn lost_lod_uploads_go_up_the_next_frame() {
    let chunks = full_detail();
    let view = scene_view(I64Vec3::ZERO);
    for_every_tier(|gpu, report| {
        if gpu.gl() {
            return;
        }
        let mut run = || -> Result<(), String> {
            let (want, _) = render_scene(gpu, I64Vec3::ZERO)?;
            let mut renderer = new_renderer(gpu)?;
            load_world(&mut renderer, &chunks, I64Vec3::ZERO)?;
            renderer.set_lod(
                gpu.device,
                Some(LodInput::new(settings(1 << 20), Some(HORIZON))),
            );
            let empty = render(gpu, &mut renderer, &view)?;
            renderer.set_lod(gpu.device, Some(lod_input(settings(1 << 20), lod_nodes())));
            renderer.debug_fail_next_lod_upload();
            let caught = run_in_error_scopes(gpu.device, || {
                let mut encoder =
                    gpu.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("gpu_lod lost uploads"),
                        });
                renderer.render(gpu.device, gpu.queue, &mut encoder, &view);
                submit_and_wait(gpu.device, gpu.queue, encoder)
            });
            if caught.is_ok() {
                return Err("no error for the staging buffer".into());
            }
            if renderer.stats().lod_draws != 0 {
                return Err(format!(
                    "nodes drawn before their upload: {:?}",
                    renderer.stats()
                ));
            }
            let lost = read_texture_rgba8(gpu.device, gpu.queue, renderer.main_color())?;
            report.check_identical(
                &format!("{} {}: lost uploads draw no node", gpu.label, gpu.tier),
                (&gpu.name("lost_empty"), &empty),
                (&gpu.name("lost"), &lost),
            );
            let again = render(gpu, &mut renderer, &view)?;
            report.check_identical(
                &format!("{} {}: uploaded the next frame", gpu.label, gpu.tier),
                (&gpu.name("lost_want"), &want),
                (&gpu.name("lost_again"), &again),
            );
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

// ---------------------------------------------------------------- the seam with a real world

/// The full-detail radius of the seam worlds at eye level, in chunks (12 for the eye 200 blocks
/// up, whose search box then reaches past the view distance on screen, as at `voxel_world`'s
/// default), and their vertical radius (`voxel_world`'s default: the high eye still has the
/// ground in its box).
const SEAM_RADIUS: u32 = 4;
const SEAM_VERTICAL: u32 = 8;
/// The clear color of the seam test: the sky shows as magenta.
const MAGENTA: [f32; 4] = [1.0, 0.0, 1.0, 1.0];
/// The seam test's second sky: where a pixel turns from magenta toward green, the sky shows
/// through it, if only through translucent water.
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
/// A ray meeting `y = SEAM_FLOOR` (the lowest top of the demo terrain) within this share of the
/// horizon meets drawn terrain first.
const SEAM_REACH: f64 = 0.7;
const SEAM_FLOOR: f64 = 16.0;
/// The full-detail edge of the seam worlds along -x: chunk column `x = -105` is the last one
/// drawn in full detail (its west face, `x = -3360`, has a sea more than 8 blocks deep across it
/// for `z` in 1282..1310, the spot of `boxel_demo`'s crack ray test) and the eye's chunk is
/// `SEAM_RADIUS` columns east of it.
const SEAM_X: i64 = -105 * 32;
const SEAM_EYE: [f64; 2] = [((-105 + SEAM_RADIUS as i64) * 32) as f64 + 16.5, 1296.5];

/// A forest world and its far LOD as the plugins run them (inline jobs), settled around one
/// viewer, and what a renderer holds after applying every op.
struct SeamWorld {
    app: boxel_app::App,
    viewer: boxel_ecs::prelude::Entity,
    /// The viewer's radius, in chunks.
    radius: u32,
    /// The renderer's LOD nodes: mesh and hidden octants.
    nodes: std::collections::HashMap<ChunkKey, (Arc<ChunkMesh>, u8)>,
}

impl SeamWorld {
    fn new(at: DVec3, radius: u32) -> Self {
        use boxel_world::lod::LodWorldPlugin;
        use boxel_world::{ChunkViewer, WorldPlugin, WorldSettings};
        let forest = ForestGenerator::default();
        let mut app = boxel_app::App::new();
        app.add_plugins(WorldPlugin {
            settings: WorldSettings {
                interest_budget: std::time::Duration::MAX,
                job_budget: std::time::Duration::MAX,
                max_jobs_in_flight: 4096,
                ..WorldSettings::default()
            },
            executor: Arc::new(boxel_world::jobs::InlineExecutor),
            ..WorldPlugin::new(Arc::new(forest), Arc::new(boxel_demo::blocks::registry()))
        });
        app.add_plugins(LodWorldPlugin {
            settings: seam_settings(),
            executor: Arc::new(boxel_world::jobs::InlineExecutor),
        });
        let viewer = app
            .world_mut()
            .spawn((
                Transform::from_translation(at),
                ChunkViewer::new(radius).with_vertical_radius(SEAM_VERTICAL),
            ))
            .id();
        let mut world = Self {
            app,
            viewer,
            radius,
            nodes: std::collections::HashMap::new(),
        };
        world.settle();
        world
    }

    /// Moves the viewer and settles again: the chunks it leaves in its keep band stay drawn.
    fn move_to(&mut self, at: DVec3) {
        self.app
            .world_mut()
            .get_mut::<Transform>(self.viewer)
            .expect("viewer")
            .translation = at;
        self.settle();
    }

    /// Runs frames until the world is settled and the LOD filled, applying the ops.
    fn settle(&mut self) {
        use boxel_world::WorldStats;
        use boxel_world::lod::{LodOps, LodStats};
        for frame in 0..2000 {
            self.app.update();
            let ops = self.app.world_mut().resource_mut::<LodOps>().take();
            self.apply(ops);
            let world = self.app.world();
            let stats = world.resource::<WorldStats>();
            let lod = world.resource::<LodStats>();
            if frame > 2
                && stats.chunks > 0
                && stats.is_settled()
                && lod.off.is_none()
                && lod.fill >= 1.0
                && lod.sends_waiting == 0
            {
                return;
            }
        }
        panic!("the seam world did not settle");
    }

    fn apply(&mut self, ops: Vec<LodOp>) {
        for op in ops {
            match op {
                LodOp::Upsert { key, mesh, mask } => {
                    self.nodes.insert(key, (mesh, mask));
                }
                LodOp::Mask { key, mask } => {
                    self.nodes.get_mut(&key).expect("a mask for a node sent").1 = mask;
                }
                LodOp::Remove { key } => {
                    self.nodes.remove(&key).expect("a node sent");
                }
            }
        }
    }

    fn horizon(&self) -> f32 {
        self.app
            .world()
            .resource::<boxel_world::lod::LodStats>()
            .horizon as f32
    }

    /// One input that puts every node on a fresh renderer.
    fn lod_input(&self) -> LodInput {
        LodInput {
            ops: self
                .nodes
                .iter()
                .map(|(key, (mesh, mask))| LodOp::Upsert {
                    key: *key,
                    mesh: Arc::clone(mesh),
                    mask: *mask,
                })
                .collect(),
            ..LodInput::new(seam_settings(), Some(self.horizon()))
        }
    }

    /// The published chunks, `(world block origin, mesh)`.
    fn chunks(&self) -> Vec<(I64Vec3, Arc<ChunkMesh>)> {
        self.app
            .world()
            .resource::<boxel_world::ChunkMap>()
            .ready_meshes()
            .map(|(_, key, mesh)| (key.pos.origin().as_i64vec3(), Arc::clone(mesh)))
            .collect()
    }

    /// The cave culling set of the render plugin for `camera` at `transform`, with the search's
    /// far plane at `far` (the LOD's horizon), or at the camera's view distance.
    fn visible(
        &mut self,
        camera: &Camera,
        transform: &Transform,
        far: Option<f32>,
    ) -> Arc<boxel_world::VisibleSet> {
        let registry = Arc::clone(
            &self
                .app
                .world()
                .resource::<boxel_world::WorldContent>()
                .registry,
        );
        let map = self.app.world().resource::<boxel_world::ChunkMap>();
        boxel_render::frame_visibility_with(
            false,
            &registry,
            map,
            &mut boxel_world::ChunkVisibility::default(),
            camera,
            transform,
            boxel_world::ChunkViewer::new(self.radius).with_vertical_radius(SEAM_VERTICAL),
            WIDTH as f32 / HEIGHT as f32,
            far,
        )
        .expect("the eye sees from open space")
    }
}

/// The seam worlds' LOD: `R = 8`, `K = 2` (a horizon of 1,024 blocks), every job and send at
/// once.
fn seam_settings() -> LodSettings {
    LodSettings {
        ring_radius: 8,
        levels: 2,
        frame_budget: std::time::Duration::from_secs(1),
        publish_per_frame: 1 << 16,
        upload_bytes_per_frame: 1 << 30,
        ..LodSettings::default()
    }
}

/// A seam pose: the viewer settles at `from`, then moves to the eye; the camera looks along
/// `yaw` and `pitch` (degrees).
struct SeamPose {
    name: &'static str,
    /// The viewer's radius, in chunks (the camera's view distance is as far).
    radius: u32,
    from: DVec3,
    eye: DVec3,
    yaw: f32,
    pitch: f32,
    /// The camera's far plane in the search loses chunks here.
    loses: bool,
}

/// The seam poses at [`SEAM_EYE`], over the ground there (above the trees).
fn seam_poses() -> Vec<SeamPose> {
    let forest = ForestGenerator::default();
    let [x, z] = SEAM_EYE;
    let ground = f64::from(forest.terrain.height_at(x as i64, z as i64)).max(46.0);
    let low = DVec3::new(x, ground + 12.0, z);
    let high = DVec3::new(x, ground + 200.0, z);
    // The diagonal view comes back from two chunks ahead, so its keep band holds chunks past the
    // view distance inside the searched box.
    let ahead = low + DVec3::new(-64.0, 0.0, -64.0);
    vec![
        SeamPose {
            name: "eye_level",
            radius: SEAM_RADIUS,
            from: low,
            eye: low,
            yaw: 90.0,
            pitch: -6.0,
            loses: false,
        },
        SeamPose {
            name: "diagonal",
            radius: SEAM_RADIUS,
            from: ahead,
            eye: low,
            yaw: 45.0,
            pitch: -6.0,
            loses: true,
        },
        SeamPose {
            name: "high",
            radius: 12,
            from: high,
            eye: high,
            yaw: 90.0,
            pitch: -20.0,
            loses: true,
        },
    ]
}

/// Pixels whose ray meets `y = SEAM_FLOOR` within `reach` blocks (horizontally) of the eye: drawn
/// terrain must cover them.
fn ground_pixels(transform: &Transform, camera: &Camera, reach: f64) -> Vec<bool> {
    let (f, r, u) = (
        transform.forward().as_dvec3(),
        transform.right().as_dvec3(),
        transform.up().as_dvec3(),
    );
    let tan = f64::from(camera.fov_y_radians / 2.0).tan();
    let aspect = f64::from(WIDTH) / f64::from(HEIGHT);
    let eye = transform.translation;
    let mut out = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for py in 0..HEIGHT {
        for px in 0..WIDTH {
            let nx = (f64::from(px) + 0.5) / f64::from(WIDTH) * 2.0 - 1.0;
            let ny = 1.0 - (f64::from(py) + 0.5) / f64::from(HEIGHT) * 2.0;
            let d = f + r * (nx * tan * aspect) + u * (ny * tan);
            let hit = d.y < 0.0 && {
                let t = (eye.y - SEAM_FLOOR) / -d.y;
                t * d.x.hypot(d.z) <= reach
            };
            out.push(hit);
        }
    }
    out
}

/// Pixels that must show terrain (`ground`) but change with the sky between `magenta` (the
/// scene under the [`MAGENTA`] sky) and `green` (under [`GREEN`]): more red and blue, less green.
/// The sky shows there, if only through translucent water, which only tints it. Nothing else in
/// a ground pixel follows the sky's color (the terrain fog lies past [`SEAM_REACH`]), so a pixel
/// that two draws of the same scene leave apart (a tie between coplanar quads, drawn in another
/// order) does not count unless it moves that way.
fn sky_shows(magenta: &RgbaImage, green: &RgbaImage, ground: &[bool]) -> usize {
    magenta
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .zip(green.data.as_chunks::<4>().0)
        .zip(ground)
        .filter(|((m, g), ground)| {
            **ground
                && m[0] > g[0].saturating_add(2)
                && m[1].saturating_add(2) < g[1]
                && m[2] > g[2].saturating_add(2)
        })
        .count()
}

/// Pixels of `image` that must show terrain (`ground`) but show the magenta sky.
fn holes(image: &RgbaImage, ground: &[bool]) -> usize {
    image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .zip(ground)
        .filter(|(p, g)| **g && p[0] >= 253 && p[1] <= 2 && p[2] >= 253)
        .count()
}

/// Everything a seam pose draws: the world's chunks and nodes and the camera (under the
/// [`MAGENTA`] sky).
struct SeamScene {
    name: &'static str,
    chunks: Vec<(I64Vec3, Arc<ChunkMesh>)>,
    lod: LodInput,
    view: ViewParams,
    /// The search with the LOD's far plane, and with the camera's.
    with_lod: Arc<boxel_world::VisibleSet>,
    without: Arc<boxel_world::VisibleSet>,
    ground: Vec<bool>,
    loses: bool,
}

/// The seam scenes, built once: one world per pose.
fn seam_scenes() -> &'static [SeamScene] {
    static SCENES: OnceLock<Vec<SeamScene>> = OnceLock::new();
    SCENES.get_or_init(|| {
        let forest = ForestGenerator::default();
        let sea = forest.sea.expect("the forest has a sea").level;
        // The deep sea across the seam, as `boxel_demo`'s crack ray test found it.
        assert!(
            (1282..1310).any(|z| [SEAM_X - 1, SEAM_X]
                .iter()
                .all(|&x| sea - i64::from(forest.terrain.height_at(x, z)) > 8)),
            "no deep sea across the seam"
        );
        seam_poses()
            .into_iter()
            .map(|pose| {
                let camera = Camera {
                    view_distance: (pose.radius * 32) as f32,
                    ..Camera::default()
                };
                let mut world = SeamWorld::new(pose.from, pose.radius);
                if pose.from != pose.eye {
                    world.move_to(pose.eye);
                }
                let transform = Transform::from_yaw_pitch(
                    pose.eye,
                    pose.yaw.to_radians(),
                    pose.pitch.to_radians(),
                );
                let horizon = world.horizon();
                let cv = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
                let scene = SeamScene {
                    name: pose.name,
                    chunks: world.chunks(),
                    lod: world.lod_input(),
                    view: ViewParams::from_camera(&cv, &camera, MAGENTA, DistanceFog::default()),
                    with_lod: world.visible(&camera, &transform, Some(horizon)),
                    without: world.visible(&camera, &transform, None),
                    ground: ground_pixels(&transform, &camera, SEAM_REACH * f64::from(horizon)),
                    loses: pose.loses,
                };
                eprintln!(
                    "gpu_lod: seam {}: {} chunks, {} LOD nodes, horizon {horizon}, {} ground \
                     pixels",
                    scene.name,
                    scene.chunks.len(),
                    scene.lod.ops.len(),
                    scene.ground.iter().filter(|g| **g).count()
                );
                scene
            })
            .collect()
    })
}

/// Renders a seam scene with the visible set `set` under the sky `sky`.
fn render_seam(
    gpu: &Gpu<'_>,
    scene: &SeamScene,
    set: &Arc<boxel_world::VisibleSet>,
    sky: [f32; 4],
) -> Result<RgbaImage, String> {
    let config = RendererConfig {
        arena_quads: 1 << 21,
        max_chunk_slots: 4096,
        translucent_sort: TranslucentSort::Inline,
    };
    let mut renderer = VoxelRenderer::new(
        gpu.device,
        gpu.queue,
        gpu.tier,
        &config,
        &boxel_render::generate_block_textures(registry().textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    for (i, (origin, mesh)) in scene.chunks.iter().enumerate() {
        renderer
            .upsert_chunk(i as u64, *origin, Arc::clone(mesh))
            .map_err(|e| e.to_string())?;
    }
    renderer.set_lod(gpu.device, Some(scene.lod.clone()));
    renderer.set_visibility(Some(Arc::clone(set)));
    let view = ViewParams {
        clear_color: sky,
        fog_color: sky,
        ..scene.view
    };
    let image = render(gpu, &mut renderer, &view)?;
    let report = renderer.take_lod_report().ok_or("no LOD pass")?;
    if !report.refused.is_empty() {
        return Err(format!("{} LOD nodes refused", report.refused.len()));
    }
    Ok(image)
}

/// Full detail and LOD meet without a hole: no ray that meets the ground shows the sky, at the
/// seam over a deep sea, looking diagonally over the keep band and from high up pitched down
/// (cave culling on, searched as the render plugin does with the LOD on). The far water is
/// translucent (issue #50), so a hole under it only tints the sky: each scene is drawn under two
/// skies, and no ground pixel may follow the sky's color. The sea scene holds LOD water, and the
/// tiers of an adapter draw it alike. Searched with the camera's far plane instead, the last two
/// poses show holes: the search must reach the horizon.
#[test]
fn full_detail_and_lod_meet_without_holes() {
    let mut scenes: Option<&[SeamScene]> = None;
    let mut images: Vec<(String, Tier, &str, RgbaImage)> = Vec::new();
    let mut report = for_every_tier(|gpu, report| {
        if gpu.gl() {
            return;
        }
        let scenes = *scenes.get_or_insert_with(seam_scenes);
        for scene in scenes {
            let what = format!("{} {}: seam {}", gpu.label, gpu.tier, scene.name);
            let images_of = || -> Result<_, String> {
                Ok((
                    render_seam(gpu, scene, &scene.with_lod, MAGENTA)?,
                    render_seam(gpu, scene, &scene.with_lod, GREEN)?,
                    render_seam(gpu, scene, &scene.without, MAGENTA)?,
                ))
            };
            let (with_lod, green, without) = match images_of() {
                Ok(images) => images,
                Err(e) => {
                    report.fail(format!("{what}: {e}"), &[]);
                    continue;
                }
            };
            let name = gpu.name(&format!("seam_{}", scene.name));
            let lost = format!("{name}_camera_far_plane");
            let sky = format!("{name}_green");
            report.keep(&[(&name, &with_lod), (&sky, &green), (&lost, &without)]);
            let (found, found_without) = (
                sky_shows(&with_lod, &green, &scene.ground),
                holes(&without, &scene.ground),
            );
            eprintln!(
                "gpu_lod: {what}: {found} pixels show the sky, {found_without} holes with the \
                 camera's far plane"
            );
            if found > 0 {
                report.fail(
                    format!("{what}: {found} ground pixels show the sky"),
                    &[(&name, &with_lod), (&sky, &green)],
                );
            }
            if scene.loses && found_without == 0 {
                report.fail(
                    format!("{what}: the camera's far plane in the search shows no hole"),
                    &[(&lost, &without)],
                );
            }
            images.push((gpu.label.to_string(), gpu.tier, scene.name, with_lod));
        }
    });
    // The sea pose's LOD has water to draw.
    if let Some(scenes) = scenes {
        let water: u32 = scenes[0]
            .lod
            .ops
            .iter()
            .filter_map(|op| match op {
                LodOp::Upsert { mesh, .. } => Some(mesh.bucket_counts[TRANSLUCENT_BUCKET]),
                _ => None,
            })
            .sum();
        eprintln!("gpu_lod: seam {}: {water} LOD water quads", scenes[0].name);
        if water == 0 {
            report.fail(format!("seam {}: no LOD water", scenes[0].name), &[]);
        }
    }
    // The best tier of each adapter comes first.
    for (i, (label, tier, scene, image)) in images.iter().enumerate() {
        let Some((_, best_tier, _, best)) = images[..i]
            .iter()
            .find(|(l, _, s, _)| l == label && s == scene)
        else {
            continue;
        };
        report.check_close(
            &format!("{label}: seam {scene}: {tier} vs {best_tier}"),
            (&format!("{label}_{best_tier}_seam_{scene}"), best),
            (&format!("{label}_{tier}_seam_{scene}"), image),
            2,
            MAX_TIER_DIFF,
        );
    }
    report.finish();
}

// ---------------------------------------------------------------- far plane and fog

/// While the LOD draws, a chunk past the camera's view distance is drawn (the world side counts
/// it as drawn and hides its octant); without LOD it is culled, as before. And the demo's valley
/// fog over full detail near the camera (inside the old terrain fog) draws exactly the same with
/// and without LOD: the fog layers keep their own range.
#[test]
fn the_lod_moves_the_far_plane_and_keeps_the_fog_layers() {
    use boxel_render::fog::{FogInput, FogSettings};
    let near = full_detail();
    let mut far = near.clone();
    // A floor chunk 160 to 192 blocks ahead, past the 128-block view distance.
    far.push((I64Vec3::new(0, 2, -6), near[0].1.clone()));
    // A 128-block view distance for the far chunk, and the default 384 for the fog: the floor
    // then lies inside the old terrain fog's start (230 blocks), which the LOD moves out.
    let view_at = |view_distance| {
        let camera = Camera {
            view_distance,
            ..Camera::default()
        };
        let transform = Transform::from_yaw_pitch(DVec3::new(16.5, 100.0, 20.5), 0.0, -0.25);
        let cv = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
        ViewParams::from_camera(&cv, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
    };
    let (view_far, view_fog) = (view_at(128.0), view_at(384.0));
    let fog = FogInput {
        settings: Arc::new(FogSettings::new(vec![boxel_demo::fog::valley()])),
        time: 7u64.into(),
    };
    let empty_lod = || Some(LodInput::new(settings(1 << 20), Some(HORIZON)));
    for_every_tier(|gpu, report| {
        if gpu.gl() {
            return;
        }
        let mut run = || -> Result<(), String> {
            let draw = |chunks: &[(I64Vec3, ChunkMesh)],
                        lod: Option<LodInput>,
                        fog: Option<FogInput>|
             -> Result<RgbaImage, String> {
                let mut renderer = new_renderer(gpu)?;
                load_world(&mut renderer, chunks, I64Vec3::ZERO)?;
                renderer.set_lod(gpu.device, lod);
                let view = if fog.is_some() { &view_fog } else { &view_far };
                renderer.set_fog(gpu.device, fog);
                render(gpu, &mut renderer, view)
            };
            let what = format!("{} {}", gpu.label, gpu.tier);
            // Without LOD the far chunk is culled: the image without it.
            let culled = draw(&far, None, None)?;
            let none = draw(&near, None, None)?;
            report.check_identical(
                &format!("{what}: a chunk past the view distance without LOD"),
                (&gpu.name("far_chunk_no_lod"), &culled),
                (&gpu.name("no_far_chunk_no_lod"), &none),
            );
            // With LOD it is drawn.
            let drawn = draw(&far, empty_lod(), None)?;
            let without = draw(&near, empty_lod(), None)?;
            let changed = drawn.compare(&without, 0).differing_pixels;
            report.keep(&[(&gpu.name("far_chunk_lod"), &drawn)]);
            if changed < 50 {
                report.fail(
                    format!("{what}: the chunk past the view distance changed {changed} pixels"),
                    &[(&gpu.name("far_chunk_lod"), &drawn)],
                );
            }
            // The valley fog near the camera, with and without LOD.
            let fogged = draw(&near, None, Some(fog.clone()))?;
            let fogged_lod = draw(&near, empty_lod(), Some(fog.clone()))?;
            let mut clear = new_renderer(gpu)?;
            load_world(&mut clear, &near, I64Vec3::ZERO)?;
            if fogged.data == render(gpu, &mut clear, &view_fog)?.data {
                return Err("the valley fog drew nothing".into());
            }
            report.keep(&[(&gpu.name("valley"), &fogged)]);
            report.check_identical(
                &format!("{what}: the valley fog with LOD"),
                (&gpu.name("valley"), &fogged),
                (&gpu.name("valley_lod"), &fogged_lod),
            );
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// The demo's hazy valley (its haze past the cells) over the LOD terrain: while the LOD draws,
/// its haze goes on to the horizon, so every texel whose ray meets the band 0.6 to 3 km away
/// has fog over an empty LOD, and over the forest's LOD those the hills do not hide; without
/// LOD none lies past the view distance, and neither does the plain valley's with LOD (layers
/// without haze keep their range).
#[test]
fn a_hazy_valley_reaches_the_lod_horizon() {
    use boxel_render::fog::{FogInput, FogSettings};
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(DVec3::new(16.5, 100.0, 20.5), 0.0, -0.06);
    let cv = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    let view = ViewParams::from_camera(&cv, &camera, DEFAULT_SKY_COLOR, DistanceFog::default());
    let basis =
        boxel_fog::RayBasis::new(view.view_proj.to_cols_array_2d(), view.eye_rel.to_array())
            .expect("basis");
    // Where each half-resolution texel's ray enters the valley's band (its top, y 70), in blocks.
    let top = 70.0 - view.render_origin.y as f32 - view.eye_rel.y;
    let entry = |tx: u32, ty: u32| -> Option<f32> {
        let ndc = [
            (2 * tx + 1) as f32 / WIDTH as f32 * 2.0 - 1.0,
            1.0 - (2 * ty + 1) as f32 / HEIGHT as f32 * 2.0,
        ];
        let dir = basis.dir(ndc[0], ndc[1]);
        let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        (dir[1] < 0.0).then(|| top / dir[1] * len)
    };
    let fog = |layer| FogInput {
        settings: Arc::new(FogSettings::new(vec![layer])),
        time: 7u64.into(),
    };
    for_every_tier(|gpu, report| {
        if gpu.gl() {
            return;
        }
        let run = || -> Result<(), String> {
            let texels =
                |lod: Option<LodInput>, fog: FogInput| -> Result<(UVec2, Vec<[u32; 2]>), String> {
                    let mut renderer = new_renderer(gpu)?;
                    load_world(&mut renderer, &full_detail(), I64Vec3::ZERO)?;
                    renderer.set_lod(gpu.device, lod);
                    renderer.set_fog(gpu.device, Some(fog));
                    render(gpu, &mut renderer, &view)?;
                    renderer
                        .fog_texels(gpu.device, gpu.queue)
                        .ok_or("no fog target")?
                };
            // (texels with fog, texels) whose ray enters the band within `range` blocks.
            let count = |(size, words): &(UVec2, Vec<[u32; 2]>), range: std::ops::Range<f32>| {
                let (mut fogged, mut all) = (0, 0);
                for ty in 0..size.y {
                    for tx in 0..size.x {
                        if entry(tx, ty).is_some_and(|d| range.contains(&d)) {
                            all += 1;
                            fogged += usize::from(words[(ty * size.x + tx) as usize][1] != 0);
                        }
                    }
                }
                (fogged, all)
            };
            let what = format!("{} {}", gpu.label, gpu.tier);
            let hazy = boxel_demo::fog::hazy_valley();
            let nodes = || Some(lod_input(settings(1 << 20), lod_nodes()));
            let empty = || Some(LodInput::new(settings(1 << 20), Some(HORIZON)));
            let far = 600.0..3000.0;
            // Over the void every far texel has haze; over the forest its hills stand out of it.
            for (name, lod, least) in [("an empty LOD", empty(), 1.0), ("the LOD", nodes(), 0.3)] {
                let (fogged, all) = count(&texels(lod, fog(hazy.clone()))?, far.clone());
                eprintln!("gpu_lod: {what}: hazy valley over {name}: {fogged} of {all} far texels");
                if all < 100 || (fogged as f64) < least * all as f64 {
                    return Err(format!(
                        "over {name}: {fogged} of {all} far texels have haze"
                    ));
                }
            }
            let past = 390.0..f32::INFINITY;
            for (name, lod, layer) in [
                ("the hazy valley without LOD", None, hazy),
                ("the valley with LOD", nodes(), boxel_demo::fog::valley()),
            ] {
                let (fogged, all) = count(&texels(lod, fog(layer))?, past.clone());
                if all < 100 || fogged > 0 {
                    return Err(format!(
                        "{name}: {fogged} of {all} texels past the view distance have fog"
                    ));
                }
            }
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// A flat floor of `count` level-1 nodes (one top quad each) in rows ahead of the camera: more
/// than one segment of the High draw lists.
fn floor_nodes(count: i64) -> Vec<(ChunkKey, Arc<ChunkMesh>)> {
    let mut counts = [0; BUCKET_COUNT];
    counts[0] = 1;
    let top = boxel_mesh::PackedQuad::new([0, 31, 0], 32, 32, boxel_voxel::Face::PosY, 1);
    let mesh = Arc::new(ChunkMesh::from_quads(vec![top], counts));
    let row = 128;
    (0..count)
        .map(|i| {
            let pos = ChunkPos::new(i % row - row / 2, -2, -(i / row) - 1);
            let key = ChunkKey {
                dim: DimensionId(0),
                lod: 1,
                pos,
            };
            (key, Arc::clone(&mesh))
        })
        .collect()
}

/// High draws the LOD GPU-driven (`lod_cull.wgsl`, `lod_pull.wgsl`): the same pixels as the
/// instanced path of Mid and Low on the same device, for the scene, tinted, and for more nodes
/// than one segment of its draw lists holds; it counts the same drawn nodes and quads.
#[test]
fn the_gpu_driven_path_draws_what_the_instanced_one_does() {
    let chunks = full_detail();
    let view = scene_view(I64Vec3::ZERO);
    let many = floor_nodes(i64::from(boxel_shaders::gpu::lod::SEGMENT_SLOTS) + 3000);
    for_every_tier(|gpu, report| {
        if gpu.tier != Tier::High || gpu.gl() {
            return;
        }
        let mut run = || -> Result<(), String> {
            let mut shots = Vec::new();
            for gpu_driven in [true, false] {
                let mut renderer = new_renderer(gpu)?;
                renderer.debug_set_lod_gpu_driven(gpu_driven);
                load_world(&mut renderer, &chunks, I64Vec3::ZERO)?;
                renderer.set_lod(gpu.device, Some(lod_input(settings(1 << 20), lod_nodes())));
                let scene = render(gpu, &mut renderer, &view)?;
                if renderer.debug_lod_gpu_driven() != gpu_driven {
                    return Err(format!("not drawn the way asked ({gpu_driven})"));
                }
                let stats = renderer.stats();
                let mut tinted = settings(1 << 20);
                tinted.debug_tint = true;
                renderer.set_lod(gpu.device, Some(LodInput::new(tinted, Some(HORIZON))));
                let tint = render(gpu, &mut renderer, &view)?;
                let mut big = new_renderer(gpu)?;
                big.debug_set_lod_gpu_driven(gpu_driven);
                big.set_lod(gpu.device, Some(lod_input(settings(1 << 20), &many)));
                let floor = render(gpu, &mut big, &view)?;
                let big_stats = big.stats();
                shots.push((gpu_driven, [scene, tint, floor], stats, big_stats));
            }
            let (gpu_shots, cpu_shots) = (&shots[0], &shots[1]);
            for (what, (a, b)) in ["scene", "tint", "floor"]
                .iter()
                .zip(gpu_shots.1.iter().zip(&cpu_shots.1))
            {
                report.check_close(
                    &format!("{} High: {what}: GPU-driven vs instanced", gpu.label),
                    (&gpu.name(&format!("{what}_instanced")), b),
                    (&gpu.name(&format!("{what}_gpu_driven")), a),
                    2,
                    MAX_TIER_DIFF,
                );
            }
            if cpu_shots.1[2].compare(&cpu_shots.1[0], 2).fraction() < 0.05 {
                return Err("the floor of nodes does not show".into());
            }
            for (g, c) in [(gpu_shots.2, cpu_shots.2), (gpu_shots.3, cpu_shots.3)] {
                if g.lod_drawn != c.lod_drawn || g.lod_drawn == 0 || g.lod_nodes != c.lod_nodes {
                    return Err(format!("drawn nodes: GPU-driven {g:?}, instanced {c:?}"));
                }
                // The GPU-driven path counts the drawn nodes' quads before its cull.
                if g.lod_quads < c.lod_quads || g.lod_refused != 0 {
                    return Err(format!("quads: GPU-driven {g:?}, instanced {c:?}"));
                }
            }
            if gpu_shots.3.lod_draws != 2 {
                return Err(format!("{} segments drawn, not 2", gpu_shots.3.lod_draws));
            }
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// Underground (ADR 0031, M3): with a cave culling search that reaches no chunk on the border
/// of its box, the LOD draws nothing, on every tier: the image is the one of a LOD with no
/// node. With a search that does, the LOD shows.
#[test]
fn a_sealed_search_draws_no_lod() {
    use boxel_voxel::ALL_LINKS;
    use boxel_world::VisibilityView;
    use boxel_world::visibility::visible_chunks;
    let chunks = full_detail();
    let view = scene_view(I64Vec3::ZERO);
    let search = VisibilityView::new(view.render_origin, None, 2, 2);
    let closed = Arc::new(visible_chunks(&search, |_| 0));
    let open = Arc::new(visible_chunks(&search, |_| ALL_LINKS));
    for_every_tier(|gpu, report| {
        if gpu.gl() {
            return;
        }
        let mut run = || -> Result<(), String> {
            let mut shots = Vec::new();
            for (set, nodes) in [
                (&closed, lod_nodes()),
                (&closed, &[][..]),
                (&open, lod_nodes()),
                (&open, &[][..]),
            ] {
                let mut renderer = new_renderer(gpu)?;
                load_world(&mut renderer, &chunks, I64Vec3::ZERO)?;
                renderer.set_visibility(Some(Arc::clone(set)));
                renderer.set_lod(gpu.device, Some(lod_input(settings(1 << 20), nodes)));
                // The first frame uploads; the second draws what is resident.
                render(gpu, &mut renderer, &view)?;
                shots.push((render(gpu, &mut renderer, &view)?, renderer.stats()));
            }
            report.check_identical(
                &format!("{} {}: sealed", gpu.label, gpu.tier),
                (&gpu.name("sealed_no_nodes"), &shots[1].0),
                (&gpu.name("sealed"), &shots[0].0),
            );
            if shots[0].1.lod_draws != 0 {
                return Err(format!("sealed: {:?}", shots[0].1));
            }
            if shots[2].1.lod_draws == 0 || shots[2].0.compare(&shots[3].0, 2).fraction() < 0.05 {
                return Err(format!("open: the LOD does not show: {:?}", shots[2].1));
            }
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// High culls the LOD against the depth pyramid of the solid terrain (ADR 0031, M3): behind a
/// stone wall that fills the view no LOD draw is written, and the image is the instanced path's;
/// without the wall the nodes are drawn. Without the wall the pyramid hides nothing, so a few
/// frames later the cull goes without it (no split pass), with the same pixels; behind the
/// wall it keeps it.
#[test]
fn nodes_behind_full_detail_are_not_drawn_on_high() {
    let wall = mesh_chunk(|_, _, _| "boxel:stone");
    let mut walled = full_detail();
    for x in -2..3 {
        for y in 2..5 {
            walled.push((I64Vec3::new(x, y, -1), wall.clone()));
        }
    }
    let view = scene_view(I64Vec3::ZERO);
    for_every_tier(|gpu, report| {
        if gpu.tier != Tier::High || gpu.gl() {
            return;
        }
        let mut run = || -> Result<(), String> {
            let mut draws = Vec::new();
            let mut shots = Vec::new();
            for (chunks, gpu_driven) in [(&walled, true), (&walled, false), (&full_detail(), true)]
            {
                let mut renderer = new_renderer(gpu)?;
                renderer.debug_set_lod_gpu_driven(gpu_driven);
                load_world(&mut renderer, chunks, I64Vec3::ZERO)?;
                renderer.set_lod(gpu.device, Some(lod_input(settings(1 << 20), lod_nodes())));
                let image = render(gpu, &mut renderer, &view)?;
                draws.push(renderer.debug_lod_gpu_draws(gpu.device, gpu.queue));
                if gpu_driven {
                    // The pyramid's samples come back within a few frames.
                    let first = renderer.debug_lod_occlusion_culling();
                    let mut later = image.clone();
                    for _ in 0..6 {
                        later = render(gpu, &mut renderer, &view)?;
                    }
                    let walled = std::ptr::eq(chunks, &walled);
                    let now = renderer.debug_lod_occlusion_culling();
                    if first != Some(true) || now != Some(walled) {
                        return Err(format!(
                            "depth pyramid (walled {walled}): first frame {first:?}, later {now:?}"
                        ));
                    }
                    report.check_identical(
                        &format!(
                            "{} High: walled {walled}: with and without the pyramid",
                            gpu.label
                        ),
                        (&gpu.name(&format!("first_{walled}")), &image),
                        (&gpu.name(&format!("later_{walled}")), &later),
                    );
                    if renderer.debug_lod_gpu_draws(gpu.device, gpu.queue)
                        != draws.last().copied().flatten()
                    {
                        return Err("the draws changed without the pyramid".into());
                    }
                }
                shots.push(image);
            }
            report.check_identical(
                &format!("{} High: behind a wall", gpu.label),
                (&gpu.name("wall_instanced"), &shots[1]),
                (&gpu.name("wall_gpu_driven"), &shots[0]),
            );
            match (draws[0], draws[1], draws[2]) {
                (Some(0), None, Some(open)) if open > 0 => Ok(()),
                other => Err(format!("LOD draws (walled, instanced, open): {other:?}")),
            }
        };
        if let Err(e) = run() {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}
