//! A world switch on every tier (ADR 0034, issue #58, and the reset contract shared with #57).
//! Self-skips without an adapter unless `BOXEL_REQUIRE_GPU=1`; run with `--test-threads=1` like
//! the other GPU tests.
//!
//! The renderer's side, driven as the render plugin's `prepare` drives it (cases (a) to (e) of
//! the joint contract), through `VoxelRenderer::apply_ops` with the frame's `Resync`:
//!
//! - **(a) Reset state:** right after `clear_world` the renderer's per-world state
//!   (`debug_reset_state`) equals a fresh renderer's, refused chunks and a cave culling set
//!   included; nothing of the old world's refusals comes out of `take_refused` or
//!   `take_undrawn`, and the next frame is sky only, as a fresh renderer's.
//! - **(b) Sparse scene:** non-empty chunks only at even chunk coordinates, so no two touch. B
//!   after A draws exactly what a fresh renderer draws of B, and so does a fresh renderer fed B
//!   in one `Resync::Chunks` frame (a device lost after the switch: test 28's GPU half), at the
//!   origin and 2^40 away.
//! - **(c) Heightfield:** the `gpu_tiers` heightfield with leaves and water: B after A within
//!   `MAX_TIER_DIFF` of a fresh B on its tier and across tiers.
//! - **(d) 20 switches** between a small scene and a big one that grows chunk memory past its
//!   floor (test 29, #57's G9): the reset state, chunk memory included, equals a fresh
//!   renderer's after every switch, and no size grows past the big scene's peak.
//! - **(e) A busy clear:** `clear_world` and `clear_chunks` while a page is being cleared, a page
//!   is half drained, a table is cleared ahead and a table copy waits: a fresh renderer after the
//!   first, every chunk back after the second.
//!
//! The world's side: a headless app (`WorldPlugin` with no world at startup, the lifecycle, the
//! daylight clock and, for test 26, the far LOD and the fog) records every frame as the render
//! plugin extracts it: the mesh events or, on a resync, every ready mesh; the cave culling set;
//! the LOD and fog input; the view. The plugin's `forget_world` is mirrored by a system in
//! `TeardownSystems::Render`. Each tier replays the frames on its own renderer (GPU tests cannot
//! run the plugin: it starts only with a window). The clocks are pinned: no `TimePlugin` on
//! either side, so the fluid time and the texture tick are 0 and the fog clock is the open's.
//!
//! - **24 Empty frame:** the frame after a close is sky only, as a renderer that never had a world
//!   draws it, with no chunk left and a fresh renderer's reset state.
//! - **25 Switch image:** B after A, settled, matches a fresh B on each tier and across tiers,
//!   with water and an animated pack texture in view, at the origin and 2^40 away.
//! - **26 LOD and fog (every tier but GL, where the LOD stays off):** the LOD arena is empty
//!   after a close, and B after A draws its LOD and the fog layers as a fresh B does (the fog
//!   clock, tick and subtick, starts again at 0).
//! - **27 Same-frame switch:** a close and an open in one frame give test 25's image.
//!
//! The cameras are placed so no two translucent chunk centers are equidistant (until #57 breaks
//! those ties by chunk origin, B's slots may be handed out in another order than a fresh B's).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_world_switch/` (every image with
//! `GPU_WORLD_SWITCH_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_app::App;
use boxel_camera::{Camera, CameraView};
use boxel_demo::worldgen::ForestGenerator;
use boxel_ecs::prelude::*;
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, IVec3, UVec2};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::fog::{FogClock, FogInput, FogPlugin, FogSettings, FogTime};
use boxel_render::lod::{LodInput, LodSettings};
use boxel_render::{
    BlockTextureData, ChunkMemorySettings, ChunkOp, PackSource, RenderSettings, RendererConfig,
    ResetState, Resync, SceneLight, TextureSource, TranslucentSort, ViewParams, VoxelRenderer,
    light_curve, load_block_textures,
};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_SIZE, ChunkData, ChunkKey, ChunkPos, LocalPos, neighbor_index,
};
use boxel_world::lifecycle::{
    CloseOptions, OpenSystems, TeardownSystems, WorldEpoch, WorldLifecycle, WorldOpen, WorldScoped,
    WorldSpec, WorldStatus, WorldTeardown,
};
use boxel_world::lod::{LodOps, LodResync, LodStats, LodWorldPlugin};
use boxel_world::persist::{PersistPlugin, PersistSettings};
use boxel_world::visibility::visible_chunks;
use boxel_world::{
    ChunkMap, ChunkMeshEvent, ChunkMeshResync, ChunkViewer, ChunkVisibility, DaylightCurve,
    DaylightPlugin, TimeOfDay, VisibilityView, VisibleSet, WorldContent, WorldPlugin,
    WorldSettings, WorldStats,
};
use boxel_worldgen::WorldGenerator;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_TIER_DIFF: f64 = 0.001;
/// 2^40 blocks east and north: the far scenes sit there (whole chunks).
const FAR: I64Vec3 = I64Vec3::new(1 << 40, 0, -(1 << 40));
/// The frames of the animated stone: red, green, blue, 4 ticks each. Tick 0 draws red.
const FRAMES: [[u8; 4]; 3] = [[200, 40, 40, 255], [40, 200, 40, 255], [40, 40, 200, 255]];

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
        label: Some("gpu_world_switch"),
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
    /// The adapter's label.
    label: &'a str,
    tier: Tier,
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    caps: &'a AdapterCaps,
}

impl Gpu<'_> {
    /// True on a GL backend, where the far LOD stays off.
    fn gl(&self) -> bool {
        self.caps.info.backend == wgpu::Backend::Gl
    }

    fn name(&self, what: &str) -> String {
        format!("{}_{}_{what}", self.label, self.tier)
    }
}

/// Runs `check` on every tier of every adapter (High first); returns the report, which panics
/// with every failure at `finish`.
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

// ---------------------------------------------------------------- reports

/// Collects failures, saves the images involved, and compares every tier of an adapter with its
/// first (best) tier.
struct Report {
    failures: Vec<String>,
    dir: PathBuf,
    tiers: u32,
    /// The images of the adapter's best tier, by name (without the tier), for the cross-tier
    /// checks: `(adapter, name) -> image`.
    best: HashMap<(String, String), (String, RgbaImage)>,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_world_switch"),
            tiers: 0,
            best: HashMap::new(),
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
                Ok(()) => eprintln!("gpu_world_switch: saved {}", path.display()),
                Err(e) => eprintln!("gpu_world_switch: cannot save {}: {e}", path.display()),
            }
        }
    }

    /// Saves `images` when `GPU_WORLD_SWITCH_SAVE` is set.
    fn keep(&self, images: &[(&str, &RgbaImage)]) {
        if std::env::var_os("GPU_WORLD_SWITCH_SAVE").is_some() {
            self.save(images);
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_world_switch: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }

    fn check(&mut self, ok: bool, message: impl FnOnce() -> String) {
        if !ok {
            self.fail(message(), &[]);
        }
    }

    /// Fails if more than `MAX_TIER_DIFF` of the pixels differ by more than 2 in a channel.
    fn check_close(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        let diff = a.1.compare(b.1, 2);
        eprintln!(
            "gpu_world_switch: {what}: {:.4} % of pixels differ, max channel {}",
            diff.fraction() * 100.0,
            diff.max_channel_diff
        );
        if diff.fraction() > MAX_TIER_DIFF {
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

    /// Compares `image` (named `what` on this tier) with the same image of the adapter's best
    /// tier, which is the first to call this for `what`.
    fn check_tiers(&mut self, gpu: &Gpu<'_>, what: &str, image: &RgbaImage) {
        let key = (gpu.label.to_string(), what.to_string());
        let name = gpu.name(what);
        match self.best.get(&key).cloned() {
            None => {
                self.best.insert(key, (name, image.clone()));
            }
            Some((best_name, best)) => self.check_close(
                &format!("{name} against {best_name}"),
                (&name, image),
                (&best_name, &best),
            ),
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

// ---------------------------------------------------------------- content

/// The demo registry, shared by every world and renderer here.
fn registry() -> &'static Arc<BlockRegistry> {
    static REGISTRY: OnceLock<Arc<BlockRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Arc::new(boxel_demo::blocks::registry()))
}

fn block(name: &str) -> BlockState {
    registry().by_name(name).expect(name)
}

/// The block textures: procedural, but stone comes from a generated pack (no third-party
/// content) as a strip of the three [`FRAMES`], 4 ticks each: an animated layer, at tick 0.
fn textures() -> &'static BlockTextureData {
    static TEXTURES: OnceLock<BlockTextureData> = OnceLock::new();
    TEXTURES.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_world_switch_pack");
        let _ = std::fs::remove_dir_all(&dir);
        let blocks = dir.join("assets/minecraft/textures/block");
        std::fs::create_dir_all(&blocks).expect("pack dirs");
        std::fs::write(
            dir.join("pack.mcmeta"),
            r#"{"pack":{"pack_format":34,"description":"boxel gpu test pack"}}"#,
        )
        .expect("pack.mcmeta");
        RgbaImage {
            width: 16,
            height: 48,
            data: (0..16 * 48).flat_map(|i| FRAMES[i / (16 * 16)]).collect(),
        }
        .save_png(&blocks.join("stone.png"))
        .expect("png");
        std::fs::write(
            blocks.join("stone.png.mcmeta"),
            r#"{"animation":{"frametime":4,"interpolate":true}}"#,
        )
        .expect("mcmeta");
        let source =
            TextureSource::Pack(PackSource::new(dir).with_recipes(boxel_demo::pack::recipes()));
        let (data, report) = load_block_textures(registry(), &source).expect("the test pack");
        assert!(report.animation_bytes > 0, "the stone is animated");
        data
    })
}

/// The renderer of the tests: large enough that nothing is refused.
fn config() -> RendererConfig {
    RendererConfig {
        chunk_memory: ChunkMemorySettings {
            initial_quads: 1 << 20,
            initial_slots: 2048,
            ..ChunkMemorySettings::default()
        },
        translucent_sort: TranslucentSort::Inline,
    }
}

fn new_renderer(gpu: &Gpu<'_>, config: &RendererConfig) -> Result<VoxelRenderer, String> {
    let mut renderer = VoxelRenderer::new(
        gpu.device,
        gpu.queue,
        gpu.tier,
        config,
        textures(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    renderer.set_model_boxes(
        gpu.queue,
        MaterialTable::from_registry(registry()).model_boxes(),
    );
    Ok(renderer)
}

/// Records one frame, submits it and waits.
fn draw(gpu: &Gpu<'_>, renderer: &mut VoxelRenderer, view: &ViewParams) -> Result<(), String> {
    run_in_error_scopes(gpu.device, || {
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gpu_world_switch"),
            });
        renderer.render(gpu.device, gpu.queue, &mut encoder, view);
        submit_and_wait(gpu.device, gpu.queue, encoder)
    })?;
    if renderer.take_lost_uploads() {
        return Err("a frame lost its uploads".into());
    }
    Ok(())
}

/// [`draw`], then reads the main color back.
fn render(
    gpu: &Gpu<'_>,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    draw(gpu, renderer, view)?;
    read_texture_rgba8(gpu.device, gpu.queue, renderer.main_color())
}

/// The view from `eye` toward `target` with the default camera, sky and distance fog.
fn look(eye: DVec3, target: DVec3) -> (Camera, Transform) {
    let d = (target - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    (
        Camera::default(),
        Transform::from_yaw_pitch(eye, yaw, pitch),
    )
}

fn view_params(camera: &Camera, transform: &Transform) -> ViewParams {
    let view = CameraView::compute(camera, transform, WIDTH as f32 / HEIGHT as f32);
    let settings = RenderSettings::default();
    ViewParams::from_camera(&view, camera, settings.clear_color, settings.distance_fog)
}

// ---------------------------------------------------------------- renderer-level scenes

/// The chunks `chunks` of the blocks `block` gives, meshed by the real mesher with their
/// neighbors (empty meshes left out).
fn mesh_region(
    block: &dyn Fn(IVec3) -> BlockState,
    chunks: &[IVec3],
) -> Vec<(IVec3, Arc<ChunkMesh>)> {
    let size = CHUNK_SIZE;
    let mut data: HashMap<IVec3, ChunkData> = HashMap::new();
    let mut dense = vec![BlockState::AIR; (size * size * size) as usize];
    for &c in chunks {
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let n = c + IVec3::new(dx, dy, dz);
                    if data.contains_key(&n) {
                        continue;
                    }
                    for x in 0..size {
                        for y in 0..size {
                            for z in 0..size {
                                dense[LocalPos::new(x as u32, y as u32, z as u32).index()] =
                                    block(n * size + IVec3::new(x, y, z));
                            }
                        }
                    }
                    data.insert(n, ChunkData::from_dense(&dense));
                }
            }
        }
    }
    let materials = MaterialTable::from_registry(registry());
    let mut mesher = Mesher::new();
    let mut out = Vec::new();
    for &c in chunks {
        let mut neighbors = [None; 26];
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let offset = IVec3::new(dx, dy, dz);
                    if let Some(i) = neighbor_index(offset) {
                        neighbors[i] = data.get(&(c + offset));
                    }
                }
            }
        }
        let input = MeshInput {
            center: &data[&c],
            neighbors,
            light: None,
        };
        let mesh = mesher.mesh_chunk(&input, &materials, MeshOptions::default());
        if !mesh.is_empty() {
            out.push((c, Arc::new(mesh)));
        }
    }
    out
}

/// A hash of a chunk and a seed.
fn hash(c: IVec3, seed: u64) -> u64 {
    let h = (c.x as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (c.y as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
        ^ (c.z as u64).wrapping_mul(0x1656_67b1_9e37_79f9)
        ^ seed.wrapping_mul(0x85eb_ca77_c2b2_ae63);
    let h = (h ^ (h >> 31)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    h ^ (h >> 29)
}

/// The sparse scene of joint case (b): one island per chunk at even chunk coordinates of `xs`,
/// `ys` and `zs` (no two chunks touch), shaped by a hash of the chunk and `seed`: a stone block
/// (the animated layer) with a grass half, a leaves box and a water pool on it.
fn sparse(
    seed: u64,
    xs: RangeInclusive<i32>,
    ys: RangeInclusive<i32>,
    zs: RangeInclusive<i32>,
) -> Vec<(IVec3, Arc<ChunkMesh>)> {
    let even = |c: IVec3| c.x % 2 == 0 && c.y % 2 == 0 && c.z % 2 == 0;
    let (stone, grass, leaves, water) = (
        block("boxel:stone"),
        block("boxel:grass"),
        block("boxel:leaves"),
        block("boxel:water"),
    );
    let inside = {
        let (xs, ys, zs) = (xs.clone(), ys.clone(), zs.clone());
        move |c: IVec3| even(c) && xs.contains(&c.x) && ys.contains(&c.y) && zs.contains(&c.z)
    };
    let island = |p: IVec3| {
        let c = p.div_euclid(IVec3::splat(CHUNK_SIZE));
        if !inside(c) {
            return BlockState::AIR;
        }
        let l = p - c * CHUNK_SIZE;
        let h = hash(c, seed);
        let top = 3 + (h % 7) as i32;
        let shift = ((h >> 8) % 5) as i32;
        let (x, z) = (l.x - shift, l.z + shift - 2);
        if !(3..27).contains(&x) || !(3..27).contains(&z) || l.y > top + 4 {
            return BlockState::AIR;
        }
        if l.y < top {
            return if l.y == top - 1 && x < 12 {
                grass
            } else {
                stone
            };
        }
        if l.y == top && (14..22).contains(&x) && (5..12).contains(&z) {
            return water;
        }
        if (top + 1..top + 4).contains(&l.y) && (4..9).contains(&x) && (16..22).contains(&z) {
            return leaves;
        }
        BlockState::AIR
    };
    let mut chunks = Vec::new();
    for x in xs.clone() {
        for y in ys.clone() {
            for z in zs.clone() {
                if even(IVec3::new(x, y, z)) {
                    chunks.push(IVec3::new(x, y, z));
                }
            }
        }
    }
    mesh_region(&island, &chunks)
}

/// The view of the sparse scenes moved by `shift`: from above one corner, at no position
/// equidistant from two chunk centers.
fn sparse_view(shift: I64Vec3) -> ViewParams {
    let s = shift.as_dvec3();
    let (camera, transform) = look(
        DVec3::new(-83.37, 61.13, 97.71) + s,
        DVec3::new(12.0, 4.0, -10.0) + s,
    );
    view_params(&camera, &transform)
}

/// The `gpu_tiers` heightfield (x and z in -64..64, y in 0..64) with its floating stone slab
/// and leaves canopy, and water up to y 26 over the low ground; `phase` moves the hills (another
/// world).
fn heightfield(phase: f32) -> Vec<(IVec3, Arc<ChunkMesh>)> {
    let (stone, dirt, grass, leaves, water) = (
        block("boxel:stone"),
        block("boxel:dirt"),
        block("boxel:grass"),
        block("boxel:leaves"),
        block("boxel:water"),
    );
    let at = |p: IVec3| {
        if p.y < 0 || p.y >= 64 || p.x < -64 || p.x >= 64 || p.z < -64 || p.z >= 64 {
            return BlockState::AIR;
        }
        if (60..63).contains(&p.y) && (-20..20).contains(&p.x) && (-10..10).contains(&p.z) {
            return stone;
        }
        if (46..48).contains(&p.y) && (8..40).contains(&p.x) && (-40..-8).contains(&p.z) {
            return leaves;
        }
        let (x, z) = (p.x as f32, p.z as f32);
        let h = (30.0
            + 10.0 * (x * 0.09 + phase).sin() * (z * 0.07).cos()
            + 5.0 * ((x + z) * 0.13).sin()) as i32;
        if p.y < h - 4 {
            stone
        } else if p.y < h - 1 {
            dirt
        } else if p.y < h {
            grass
        } else if p.y < 26 {
            water
        } else {
            BlockState::AIR
        }
    };
    let mut chunks = Vec::new();
    for y in 0..2 {
        for x in -2..2 {
            for z in -2..2 {
                chunks.push(IVec3::new(x, y, z));
            }
        }
    }
    mesh_region(&at, &chunks)
}

/// Over the heightfield from above one corner (the slab, the canopy and the water in view).
fn heightfield_view() -> ViewParams {
    let (camera, transform) = look(DVec3::new(-50.3, 70.6, -48.2), DVec3::new(6.0, 28.0, 8.0));
    view_params(&camera, &transform)
}

/// Uploads `chunks` moved by `shift` blocks under ids from `first_id` on, in batches of
/// `batch` with a frame drawn after each (a world streaming in).
fn stream_in(
    gpu: &Gpu<'_>,
    renderer: &mut VoxelRenderer,
    chunks: &[(IVec3, Arc<ChunkMesh>)],
    shift: I64Vec3,
    first_id: u64,
    batch: usize,
    view: &ViewParams,
) -> Result<(), String> {
    for (n, part) in chunks.chunks(batch.max(1)).enumerate() {
        let ops: Vec<ChunkOp> = part
            .iter()
            .enumerate()
            .map(|(i, (c, mesh))| ChunkOp::Upsert {
                id: first_id + (n * batch + i) as u64,
                origin: c.as_i64vec3() * 32 + shift,
                mesh: Arc::clone(mesh),
            })
            .collect();
        if let Some(e) = renderer.apply_ops(gpu.device, ops, Resync::None).first() {
            return Err(format!("batch {n}: {e}"));
        }
        draw(gpu, renderer, view)?;
    }
    Ok(())
}

/// A renderer's switch to the next world as `prepare` runs it on a `Resync::World` frame: the
/// old world's last refusals are taken and dropped (`extract_visibility`), then the frame's
/// ops with `Resync::World` (`clear_world` first; the next world's chunks come in later frames
/// here) and no cave culling set (a resync frame culls nothing).
fn switch_world(gpu: &Gpu<'_>, renderer: &mut VoxelRenderer) {
    renderer.take_refused(|_, _| {});
    renderer.take_undrawn(|_, _| {});
    renderer.apply_ops(gpu.device, Vec::new(), Resync::World);
    renderer.set_visibility(None);
}

/// Everything `take_refused`, then `take_undrawn`, report.
fn reports_refusals(renderer: &mut VoxelRenderer) -> Vec<(ChunkPos, bool)> {
    let mut out = Vec::new();
    renderer.take_refused(|pos, refused| out.push((pos, refused)));
    renderer.take_undrawn(|pos, undrawn| out.push((pos, undrawn)));
    out
}

// ---------------------------------------------------------------- the renderer's reset

/// Joint case (a), and test 24's renderer half: right after `clear_world`, a renderer that held
/// a world (resident chunks, refused ones and a cave culling set, a frame drawn) has a fresh
/// renderer's per-world state, says nothing of the old world's refusals, and its next frame is a
/// fresh renderer's empty frame. The control: `clear_chunks` (a resync within one world) reports
/// the refused chunks as shown again.
#[test]
fn clear_world_leaves_a_fresh_renderer() {
    let scene = sparse(1, -4..=4, 0..=0, -4..=4);
    let view = sparse_view(I64Vec3::ZERO);
    let small = RendererConfig {
        chunk_memory: ChunkMemorySettings {
            initial_slots: 16,
            grow: false,
            ..config().chunk_memory
        },
        ..config()
    };
    assert!(scene.len() > 16, "some chunks are refused");
    let searched = VisibilityView::new(view.render_origin, Some(view.frustum), 4, 1);
    let set = Arc::new(visible_chunks(&searched, |_| u16::MAX));
    for_every_tier(|gpu, report| {
        let run = |report: &mut Report| -> Result<(), String> {
            let fresh = new_renderer(gpu, &small)?;
            let empty = render(gpu, &mut new_renderer(gpu, &small)?, &view)?;
            for world in [true, false] {
                let mut renderer = new_renderer(gpu, &small)?;
                let ops: Vec<ChunkOp> = scene
                    .iter()
                    .enumerate()
                    .map(|(i, (c, mesh))| ChunkOp::Upsert {
                        id: i as u64,
                        origin: c.as_i64vec3() * 32,
                        mesh: Arc::clone(mesh),
                    })
                    .collect();
                let refused = renderer.apply_ops(gpu.device, ops, Resync::None).len();
                renderer.set_visibility(Some(Arc::clone(&set)));
                draw(gpu, &mut renderer, &view)?;
                let held = renderer.debug_reset_state();
                report.check(
                    held.chunks == 16 && held.refused == refused && held.occlusion_set,
                    || format!("{} {}: the world held {held:?}", gpu.label, gpu.tier),
                );
                if world {
                    renderer.apply_ops(gpu.device, Vec::new(), Resync::World);
                    let state = renderer.debug_reset_state();
                    report.check(state == fresh.debug_reset_state(), || {
                        format!(
                            "{} {}: after clear_world {state:?}, fresh {:?}",
                            gpu.label,
                            gpu.tier,
                            fresh.debug_reset_state()
                        )
                    });
                    let reported = reports_refusals(&mut renderer);
                    report.check(reported.is_empty(), || {
                        format!(
                            "{} {}: refusals after clear_world: {reported:?}",
                            gpu.label, gpu.tier
                        )
                    });
                    renderer.set_visibility(None);
                    let image = render(gpu, &mut renderer, &view)?;
                    report.check_identical(
                        &format!("{} {}: the frame after clear_world", gpu.label, gpu.tier),
                        (&gpu.name("after_clear_world"), &image),
                        (&gpu.name("fresh_empty"), &empty),
                    );
                    report.check(renderer.stats().chunks == 0, || {
                        format!("{} {}: chunks left after clear_world", gpu.label, gpu.tier)
                    });
                } else {
                    renderer.apply_ops(gpu.device, Vec::new(), Resync::Chunks);
                    let shown = reports_refusals(&mut renderer)
                        .iter()
                        .filter(|(_, refused)| !refused)
                        .count();
                    report.check(shown >= refused, || {
                        format!(
                            "{} {}: clear_chunks reported {shown} of {refused} refused chunks",
                            gpu.label, gpu.tier
                        )
                    });
                }
            }
            Ok(())
        };
        if let Err(e) = run(report) {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// Joint case (b), and test 28's GPU half: on the sparse scene (no two chunks touch, so the
/// order chunks are drawn in cannot change a pixel), B after A, its slots handed out in another
/// order, draws exactly what a fresh renderer streaming B in draws, and so does a fresh renderer
/// fed every chunk of B in one `Resync::Chunks` frame (the device lost after the switch: the
/// plugin's new renderer), at the origin and 2^40 away. Every tier draws it as the adapter's
/// best tier.
#[test]
fn a_sparse_world_after_another_draws_as_in_a_fresh_renderer() {
    let a = sparse(1, -6..=6, 0..=2, -6..=6);
    let b = sparse(2, -4..=4, 0..=0, -4..=4);
    let reversed: Vec<_> = b.iter().rev().cloned().collect();
    for_every_tier(|gpu, report| {
        for (place, shift) in [("origin", I64Vec3::ZERO), ("far", FAR)] {
            let view = sparse_view(shift);
            let run = |report: &mut Report| -> Result<(), String> {
                let mut switched = new_renderer(gpu, &config())?;
                stream_in(gpu, &mut switched, &a, shift, 0, 16, &view)?;
                switch_world(gpu, &mut switched);
                draw(gpu, &mut switched, &view)?;
                stream_in(gpu, &mut switched, &reversed, shift, 1 << 20, 7, &view)?;
                let after = render(gpu, &mut switched, &view)?;

                let mut fresh = new_renderer(gpu, &config())?;
                stream_in(gpu, &mut fresh, &b, shift, 0, 7, &view)?;
                let fresh_image = render(gpu, &mut fresh, &view)?;

                let mut resynced = new_renderer(gpu, &config())?;
                let ops: Vec<ChunkOp> = b
                    .iter()
                    .enumerate()
                    .map(|(i, (c, mesh))| ChunkOp::Upsert {
                        id: (1 << 20) + i as u64,
                        origin: c.as_i64vec3() * 32 + shift,
                        mesh: Arc::clone(mesh),
                    })
                    .collect();
                if let Some(e) = resynced.apply_ops(gpu.device, ops, Resync::Chunks).first() {
                    return Err(format!("B in one resync frame: {e}"));
                }
                let resynced_image = render(gpu, &mut resynced, &view)?;

                let what = format!("{} {} {place}", gpu.label, gpu.tier);
                let fresh_name = gpu.name(&format!("sparse_{place}_fresh"));
                report.keep(&[(&fresh_name, &fresh_image)]);
                report.check_identical(
                    &format!("{what}: B after A"),
                    (&gpu.name(&format!("sparse_{place}_after_a")), &after),
                    (&fresh_name, &fresh_image),
                );
                report.check_identical(
                    &format!("{what}: B in one resync frame"),
                    (
                        &gpu.name(&format!("sparse_{place}_resync")),
                        &resynced_image,
                    ),
                    (&fresh_name, &fresh_image),
                );
                report.check_tiers(gpu, &format!("sparse_{place}"), &fresh_image);
                Ok(())
            };
            if let Err(e) = run(report) {
                report.fail(format!("{} {} {place}: {e}", gpu.label, gpu.tier), &[]);
            }
        }
    })
    .finish();
}

/// Joint case (c): the heightfield of `gpu_tiers` with leaves and water (chunks that touch,
/// translucent quads across them), B after A within `MAX_TIER_DIFF` of a fresh B on its tier
/// and of the best tier's fresh B.
#[test]
fn a_heightfield_after_another_matches_a_fresh_one() {
    let a = heightfield(1.7);
    let b = heightfield(0.0);
    let reversed: Vec<_> = b.iter().rev().cloned().collect();
    let view = heightfield_view();
    for_every_tier(|gpu, report| {
        let run = |report: &mut Report| -> Result<(), String> {
            let mut switched = new_renderer(gpu, &config())?;
            stream_in(gpu, &mut switched, &a, I64Vec3::ZERO, 0, 5, &view)?;
            switch_world(gpu, &mut switched);
            draw(gpu, &mut switched, &view)?;
            stream_in(
                gpu,
                &mut switched,
                &reversed,
                I64Vec3::ZERO,
                1 << 20,
                5,
                &view,
            )?;
            let after = render(gpu, &mut switched, &view)?;

            let mut fresh = new_renderer(gpu, &config())?;
            stream_in(gpu, &mut fresh, &b, I64Vec3::ZERO, 0, 5, &view)?;
            let fresh_image = render(gpu, &mut fresh, &view)?;
            report.keep(&[(&gpu.name("heightfield_fresh"), &fresh_image)]);
            // The fresh B first: the best tier keeps it, and every tier's B after A is
            // compared with it.
            report.check_tiers(gpu, "heightfield", &fresh_image);
            report.check_tiers(gpu, "heightfield", &after);
            report.check_close(
                &format!("{} {}: heightfield B after A", gpu.label, gpu.tier),
                (&gpu.name("heightfield_after_a"), &after),
                (&gpu.name("heightfield_fresh"), &fresh_image),
            );
            Ok(())
        };
        if let Err(e) = run(report) {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// What one load of a scene leaves in the renderer: its reset state and the sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Loaded {
    state: ResetState,
    arena_capacity: u32,
    slot_capacity: u32,
    /// Chunk memory bytes: pages, table, High's lists.
    bytes: (u64, u64, u64),
}

/// Chunk memory that grows past its floor with the big scene of case (d): pages of 16 Ki quads
/// from a floor of two (one Translucent) and 64 slots.
fn growing_config() -> RendererConfig {
    RendererConfig {
        chunk_memory: ChunkMemorySettings {
            initial_quads: 1 << 15,
            initial_slots: 64,
            page_quads: 1 << 14,
            ..ChunkMemorySettings::default()
        },
        translucent_sort: TranslucentSort::Inline,
    }
}

/// Joint case (d) and test 29 (#57's G9): 20 switches between a small scene and a big one that
/// grows chunk memory past its floor (pages, the table, High's lists). After every switch the
/// renderer's per-world state is a fresh renderer's, chunk memory included; every load of a
/// scene leaves the same state and sizes (nothing accumulates), the small scene never more than
/// the big one, and the last image is a fresh renderer's.
#[test]
fn twenty_switches_leave_a_fresh_renderer_every_time() {
    let small = sparse(3, -2..=2, 0..=0, -2..=2);
    let mut big = heightfield(0.5);
    big.extend(sparse(4, -8..=8, 2..=4, -8..=8));
    let view = sparse_view(I64Vec3::ZERO);
    for_every_tier(|gpu, report| {
        let run = |report: &mut Report| -> Result<(), String> {
            let config = growing_config();
            let fresh = new_renderer(gpu, &config)?.debug_reset_state();
            let mut renderer = new_renderer(gpu, &config)?;
            let mut first: [Option<Loaded>; 2] = [None, None];
            let mut next_id = 0;
            for load in 0..21 {
                let scene = if load % 2 == 0 { &small } else { &big };
                if load > 0 {
                    switch_world(gpu, &mut renderer);
                    let state = renderer.debug_reset_state();
                    report.check(state == fresh, || {
                        format!(
                            "{} {}: switch {load}: {state:?}, fresh {fresh:?}",
                            gpu.label, gpu.tier
                        )
                    });
                    draw(gpu, &mut renderer, &view)?;
                }
                stream_in(gpu, &mut renderer, scene, I64Vec3::ZERO, next_id, 24, &view)?;
                next_id += 1 << 20;
                let stats = renderer.stats();
                let memory = stats.chunk_memory;
                let loaded = Loaded {
                    state: renderer.debug_reset_state(),
                    arena_capacity: stats.arena_capacity_quads,
                    slot_capacity: stats.chunk_slot_capacity,
                    bytes: (memory.page_bytes, memory.table_bytes, memory.list_bytes),
                };
                match first[load % 2] {
                    None => first[load % 2] = Some(loaded),
                    Some(expected) => report.check(loaded == expected, || {
                        format!(
                            "{} {}: load {load}: {loaded:?}, first {expected:?}",
                            gpu.label, gpu.tier
                        )
                    }),
                }
            }
            if let [Some(s), Some(b)] = first {
                let floor = new_renderer(gpu, &config)?.stats();
                report.check(
                    s.state.arena_used <= b.state.arena_used
                        && s.state.slots <= b.state.slots
                        && s.arena_capacity <= b.arena_capacity
                        && s.slot_capacity <= b.slot_capacity
                        && s.bytes.0 <= b.bytes.0
                        && s.bytes.1 <= b.bytes.1
                        && s.bytes.2 <= b.bytes.2,
                    || format!("{} {}: small {s:?}, big {b:?}", gpu.label, gpu.tier),
                );
                // the big scene grew past the floor, pages and table
                report.check(
                    b.state.pages > floor.chunk_memory.pages
                        && b.slot_capacity > floor.chunk_slot_capacity,
                    || {
                        format!(
                            "{} {}: the big scene did not grow: {b:?}",
                            gpu.label, gpu.tier
                        )
                    },
                );
            }
            let last = render(gpu, &mut renderer, &view)?;
            let mut fresh_small = new_renderer(gpu, &config)?;
            stream_in(gpu, &mut fresh_small, &small, I64Vec3::ZERO, 0, 24, &view)?;
            let expected = render(gpu, &mut fresh_small, &view)?;
            report.check_identical(
                &format!(
                    "{} {}: the small scene after 20 switches",
                    gpu.label, gpu.tier
                ),
                (&gpu.name("switch_20"), &last),
                (&gpu.name("switch_20_fresh"), &expected),
            );
            Ok(())
        };
        if let Err(e) = run(report) {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// Joint case (e): `clear_world` and `clear_chunks` (as `apply_ops` runs them for
/// `Resync::World` and `Resync::Chunks`) landing while chunk memory is busy: a page being
/// cleared, a page half drained into the others, a larger table being cleared ahead and one put
/// in use in the frame whose copy of the old table waits for the next render. After the world's
/// clear the renderer is a fresh one and B draws as a fresh B; after the chunks' clear every
/// chunk comes back and draws as a fresh renderer fed them in one frame.
#[test]
fn a_clear_in_the_middle_of_memory_work_is_clean() {
    let mut a = heightfield(0.5);
    a.extend(sparse(4, -8..=8, 2..=4, -8..=8));
    let b = sparse(2, -4..=4, 0..=0, -4..=4);
    let view = sparse_view(I64Vec3::ZERO);
    for_every_tier(|gpu, report| {
        for world in [true, false] {
            let run = |report: &mut Report| -> Result<(), String> {
                let config = growing_config();
                let fresh = new_renderer(gpu, &config)?.debug_reset_state();
                let mut renderer = new_renderer(gpu, &config)?;
                stream_in(gpu, &mut renderer, &a, I64Vec3::ZERO, 0, 24, &view)?;
                // everything slow: a slice of a clear and one copy a frame
                renderer.debug_set_budgets(Some((4096, 1 << 10, 1)));
                renderer.debug_force_page(false);
                renderer.grow_between_frames(gpu.device);
                let victim = renderer
                    .debug_pages()
                    .iter()
                    .find(|p| p.3 == 1 && !p.1)
                    .map(|p| p.0)
                    .ok_or("no Solid page")?;
                if !renderer.debug_evacuate_page(victim) {
                    return Err(format!("page {victim} would not drain"));
                }
                draw(gpu, &mut renderer, &view)?;
                // more new chunks than free slots: a table at once, its copy waiting
                let stats = renderer.stats();
                let free = (stats.chunk_slot_capacity - stats.chunks) as usize;
                let extra: Vec<ChunkOp> = (0..free + 1)
                    .map(|i| {
                        let (c, mesh) = &b[i % b.len()];
                        ChunkOp::Upsert {
                            id: (1 << 30) + i as u64,
                            origin: (c.as_i64vec3() + I64Vec3::new(0, 8 + 2 * i as i64, 0)) * 32,
                            mesh: Arc::clone(mesh),
                        }
                    })
                    .collect();
                if let Some(e) = renderer
                    .apply_ops(gpu.device, extra.clone(), Resync::None)
                    .first()
                {
                    return Err(format!("the extra chunks: {e}"));
                }
                // and a larger one ahead, being cleared
                for _ in 0..3 {
                    renderer.grow_between_frames(gpu.device);
                }
                let busy = renderer.chunk_memory();
                let pending = renderer.debug_reset_state().pending_pieces;
                let tables = if gpu.tier >= Tier::Mid { 2 } else { 0 };
                report.check(
                    busy.clearing_pages >= 1 && busy.draining_pages == 1 && pending > tables,
                    || {
                        format!(
                            "{} {}: not busy: {busy:?}, {pending} pieces",
                            gpu.label, gpu.tier
                        )
                    },
                );
                renderer.debug_set_budgets(None);
                if world {
                    switch_world(gpu, &mut renderer);
                    let state = renderer.debug_reset_state();
                    report.check(state == fresh, || {
                        format!(
                            "{} {}: busy, then clear_world: {state:?}, fresh {fresh:?}",
                            gpu.label, gpu.tier
                        )
                    });
                    draw(gpu, &mut renderer, &view)?;
                    stream_in(gpu, &mut renderer, &b, I64Vec3::ZERO, 1 << 20, 7, &view)?;
                    let after = render(gpu, &mut renderer, &view)?;
                    let mut fresh_b = new_renderer(gpu, &config)?;
                    stream_in(gpu, &mut fresh_b, &b, I64Vec3::ZERO, 0, 7, &view)?;
                    let expected = render(gpu, &mut fresh_b, &view)?;
                    report.check_identical(
                        &format!("{} {}: B after a busy A", gpu.label, gpu.tier),
                        (&gpu.name("busy_world_after"), &after),
                        (&gpu.name("busy_world_fresh"), &expected),
                    );
                } else {
                    let mut all: Vec<ChunkOp> = a
                        .iter()
                        .enumerate()
                        .map(|(i, (c, mesh))| ChunkOp::Upsert {
                            id: i as u64,
                            origin: c.as_i64vec3() * 32,
                            mesh: Arc::clone(mesh),
                        })
                        .collect();
                    all.extend(extra);
                    if let Some(e) = renderer
                        .apply_ops(gpu.device, all.clone(), Resync::Chunks)
                        .first()
                    {
                        return Err(format!("the resync: {e}"));
                    }
                    for _ in 0..4 {
                        draw(gpu, &mut renderer, &view)?;
                        renderer.grow_between_frames(gpu.device);
                    }
                    let after = render(gpu, &mut renderer, &view)?;
                    let mut fresh_a = new_renderer(gpu, &config)?;
                    if let Some(e) = fresh_a.apply_ops(gpu.device, all, Resync::Chunks).first() {
                        return Err(format!("the fresh resync: {e}"));
                    }
                    let expected = render(gpu, &mut fresh_a, &view)?;
                    report.check_close(
                        &format!("{} {}: A after a busy clear_chunks", gpu.label, gpu.tier),
                        (&gpu.name("busy_chunks_after"), &after),
                        (&gpu.name("busy_chunks_fresh"), &expected),
                    );
                }
                Ok(())
            };
            if let Err(e) = run(report) {
                report.fail(
                    format!("{} {} world {world}: {e}", gpu.label, gpu.tier),
                    &[],
                );
            }
        }
    })
    .finish();
}

// ---------------------------------------------------------------- the world's side

/// Rolling hills around `origin` (a whole chunk), y 0..64: stone (the animated layer) on the
/// high ground, grass, sand at the shore, water up to y 20 in the valleys and boxes of leaves
/// over some hills; `seed` moves all of it (another world).
struct Hills {
    seed: u64,
    origin: I64Vec3,
}

impl Hills {
    fn height(&self, x: i64, z: i64) -> i64 {
        let (x, z, s) = (x as f64, z as f64, (self.seed % 64) as f64);
        (24.0
            + 9.0 * (x * 0.071 + s).sin() * (z * 0.053 + 2.0 * s).cos()
            + 5.0 * ((x - z) * 0.11 + 0.7 * s).sin())
        .floor() as i64
    }
}

impl WorldGenerator for Hills {
    fn generate(&self, key: ChunkKey, _: &BlockRegistry) -> ChunkData {
        if !self.vertical_chunks().contains(&key.pos.y) {
            return ChunkData::uniform(BlockState::AIR);
        }
        let (stone, dirt, grass, sand, water, leaves) = (
            block("boxel:stone"),
            block("boxel:dirt"),
            block("boxel:grass"),
            block("boxel:sand"),
            block("boxel:water"),
            block("boxel:leaves"),
        );
        let o = key.pos.origin();
        let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
        for x in 0..32 {
            for z in 0..32 {
                let (lx, lz) = (o.x + x - self.origin.x, o.z + z - self.origin.z);
                let h = self.height(lx, lz);
                let cell = (lx.div_euclid(9) as u64)
                    .wrapping_mul(31)
                    .wrapping_add((lz.div_euclid(9) as u64).wrapping_mul(17))
                    .wrapping_add(self.seed);
                let tree = cell.is_multiple_of(5)
                    && h > 21
                    && (2..6).contains(&lx.rem_euclid(9))
                    && (3..7).contains(&lz.rem_euclid(9));
                for y in 0..32 {
                    let wy = o.y + y;
                    let b = if wy < h - 3 || (wy < h && h > 29) {
                        stone
                    } else if wy < h - 1 {
                        dirt
                    } else if wy < h {
                        if h <= 21 { sand } else { grass }
                    } else if wy < 20 {
                        water
                    } else if tree && (h + 2..h + 5).contains(&wy) {
                        leaves
                    } else {
                        BlockState::AIR
                    };
                    dense[LocalPos::new(x as u32, y as u32, z as u32).index()] = b;
                }
            }
        }
        ChunkData::from_dense(&dense)
    }

    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        0..=1
    }

    fn name(&self) -> &str {
        "hills"
    }
}

/// A chunk change for the renderer (the render plugin's `MeshOp`).
enum MeshOp {
    Upsert {
        id: u64,
        origin: I64Vec3,
        mesh: Arc<ChunkMesh>,
    },
    Remove {
        id: u64,
    },
}

/// One frame of the app as the render plugin's `Extract` leaves it for `Prepare`.
struct Frame {
    resync: Resync,
    meshes: Vec<MeshOp>,
    visibility: Option<Arc<VisibleSet>>,
    fog: Option<FogInput>,
    lod: Option<LodInput>,
    view: ViewParams,
    /// Read back once drawn, under this name.
    capture: Option<&'static str>,
}

/// The render plugin's `forget_world`, mirrored: the frame of a close is a `Resync::World`
/// frame.
#[derive(Resource, Default)]
struct Forgotten(bool);

fn forget_world(mut forgotten: ResMut<Forgotten>) {
    forgotten.0 = true;
}

/// Where the game puts its viewer when a world opens.
#[derive(Resource, Clone, Copy)]
struct ViewerAt(DVec3, ChunkViewer);

/// `WorldOpen`, `OpenSystems::Game`: the viewer of the world (it goes with the world).
fn spawn_viewer(mut commands: Commands, at: Res<ViewerAt>) {
    commands.spawn((Transform::from_translation(at.0), at.1, WorldScoped));
}

/// The far LOD of the LOD worlds: `R = 8`, `K = 2` (a horizon of 1,024 blocks), every job and
/// send at once.
fn lod_settings() -> LodSettings {
    LodSettings {
        ring_radius: 8,
        levels: 2,
        frame_budget: Duration::from_secs(1),
        publish_per_frame: 1 << 16,
        upload_bytes_per_frame: 1 << 30,
        ..LodSettings::default()
    }
}

/// The settings of every world: every job at once, inline.
fn world_settings() -> WorldSettings {
    WorldSettings {
        interest_budget: Duration::MAX,
        job_budget: Duration::MAX,
        max_jobs_in_flight: 4096,
        ..WorldSettings::default()
    }
}

/// The world side of an app without a window (no world at startup, inline jobs, the daylight
/// clock, saves on inline jobs so a close ends in its own frame, and the far LOD and the fog if
/// asked), recording every frame as the render plugin extracts it.
struct Recorder {
    app: App,
    camera: Camera,
    transform: Transform,
    lod: bool,
    fog: bool,
    frames: Vec<Frame>,
}

impl Recorder {
    fn new(
        camera: Camera,
        transform: Transform,
        viewer: ChunkViewer,
        lod: bool,
        fog: bool,
    ) -> Self {
        use boxel_world::jobs::InlineExecutor;
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            executor: Arc::new(InlineExecutor),
            ..WorldPlugin::closed(Arc::clone(registry()))
        })
        .add_plugins(DaylightPlugin)
        .add_plugins(PersistPlugin::without_store().with_executor(Arc::new(InlineExecutor)))
        .insert_resource(PersistSettings {
            autosave: None,
            ..PersistSettings::default()
        })
        .init_resource::<Forgotten>()
        .insert_resource(ViewerAt(transform.translation, viewer))
        .add_systems(WorldTeardown, forget_world.in_set(TeardownSystems::Render))
        .add_systems(WorldOpen, spawn_viewer.in_set(OpenSystems::Game));
        if lod {
            app.add_plugins(LodWorldPlugin {
                settings: lod_settings(),
                executor: Arc::new(InlineExecutor),
            });
        }
        if fog {
            app.add_plugins(FogPlugin::new(vec![boxel_demo::fog::valley()]));
        }
        app.finish();
        app.cleanup();
        Self {
            app,
            camera,
            transform,
            lod,
            fog,
            frames: Vec::new(),
        }
    }

    fn lifecycle(&mut self) -> Mut<'_, WorldLifecycle> {
        self.app.world_mut().resource_mut::<WorldLifecycle>()
    }

    fn status(&self) -> WorldStatus {
        self.app.world().resource::<WorldLifecycle>().status()
    }

    fn epoch(&self) -> u64 {
        self.app.world().resource::<WorldEpoch>().get()
    }

    /// One frame, recorded as the render plugin extracts it (`extract`, `extract_lod`,
    /// `extract_fog`, `extract_visibility` and the view of `prepare`).
    fn frame(&mut self, capture: Option<&'static str>) {
        self.app.update();
        let world = self.app.world_mut();
        let forgotten = std::mem::take(&mut world.resource_mut::<Forgotten>().0);
        let resync = if forgotten {
            Resync::World
        } else if self.frames.is_empty() {
            Resync::Chunks
        } else {
            Resync::None
        };
        let events: Vec<ChunkMeshEvent> = world
            .resource_mut::<Messages<ChunkMeshEvent>>()
            .drain()
            .collect();
        let meshes = if resync == Resync::None {
            events
                .iter()
                .map(|event| match event {
                    ChunkMeshEvent::Upsert { id, key, mesh } => MeshOp::Upsert {
                        id: id.to_bits(),
                        origin: key.pos.origin().as_i64vec3(),
                        mesh: Arc::clone(mesh),
                    },
                    ChunkMeshEvent::Remove { id, .. } => MeshOp::Remove { id: id.to_bits() },
                })
                .collect()
        } else {
            world.write_message(ChunkMeshResync);
            world
                .resource::<ChunkMap>()
                .ready_meshes()
                .map(|(id, key, mesh)| MeshOp::Upsert {
                    id: id.to_bits(),
                    origin: key.pos.origin().as_i64vec3(),
                    mesh: Arc::clone(mesh),
                })
                .collect()
        };
        let lod = self.lod.then(|| {
            let ops = world.resource_mut::<LodOps>().take();
            let stats = world.resource::<LodStats>();
            let horizon = stats.off.is_none().then_some(stats.horizon as f32);
            let mut input = LodInput {
                ops,
                ..LodInput::new(world.resource::<LodSettings>().clone(), horizon)
            };
            if resync != Resync::None {
                input.ops.clear();
                input.resync = true;
                world.write_message(LodResync);
            }
            input
        });
        let fog = if self.fog {
            let settings = world.resource::<FogSettings>();
            settings.validate().is_ok().then(|| FogInput {
                settings: Arc::new(settings.clone()),
                time: world.resource::<FogClock>().time(),
            })
        } else {
            None
        };
        let viewer = world
            .query::<&ChunkViewer>()
            .iter(world)
            .copied()
            .reduce(|a, b| ChunkViewer {
                radius: a.radius.max(b.radius),
                vertical_radius: a.vertical_radius.max(b.vertical_radius),
            });
        let far = lod.as_ref().and_then(LodInput::far);
        let (camera, transform) = (self.camera, self.transform);
        let visibility = world.resource_scope(|world, mut visibility: Mut<ChunkVisibility>| {
            let map = world.resource::<ChunkMap>();
            if resync != Resync::None {
                visibility.clear_unshown(|pos| map.visibility_links_at(pos));
            }
            if !world.resource::<WorldSettings>().cave_culling {
                return None;
            }
            boxel_render::frame_visibility_with(
                resync != Resync::None,
                &world.resource::<WorldContent>().registry,
                map,
                &mut visibility,
                &camera,
                &transform,
                viewer?,
                WIDTH as f32 / HEIGHT as f32,
                far,
            )
        });
        let settings = RenderSettings::default();
        let daylight = world
            .get_resource::<TimeOfDay>()
            .zip(world.get_resource::<DaylightCurve>())
            .map(|(time, curve)| curve.at(time));
        let light = SceneLight {
            sky: daylight.map_or([1.0; 3], |d| d.sky_light),
            ambient: daylight.map_or(SceneLight::DEFAULT_AMBIENT, |d| d.ambient),
            ao_strength: settings.ao_strength,
            block_gain: settings.block_light_gain,
            curve: light_curve(settings.light_gamma),
        };
        let sky = daylight.map_or(settings.clear_color, |d| d.sky_color);
        let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
        let view = ViewParams {
            light,
            ..ViewParams::from_camera(&view, &camera, sky, settings.distance_fog)
        };
        self.frames.push(Frame {
            resync,
            meshes,
            visibility,
            fog,
            lod,
            view,
            capture,
        });
    }

    fn settled(&self) -> bool {
        let world = self.app.world();
        let stats = world.resource::<WorldStats>();
        let lod = !self.lod || {
            let lod = world.resource::<LodStats>();
            lod.off.is_none() && lod.fill >= 1.0 && lod.sends_waiting == 0
        };
        self.status() == WorldStatus::Open && stats.chunks > 0 && stats.is_settled() && lod
    }

    /// Runs frames until the world settles, then three more, the last one captured.
    fn settle(&mut self, capture: &'static str) {
        for _ in 0..3000 {
            self.frame(None);
            if self.settled() {
                let stats = *self.app.world().resource::<WorldStats>();
                assert_eq!(
                    stats.data_ready, stats.chunks,
                    "every chunk generated: {stats:?}"
                );
                for _ in 0..2 {
                    self.frame(None);
                }
                self.frame(Some(capture));
                return;
            }
        }
        panic!(
            "the world did not settle: {:?}",
            self.app.world().resource::<WorldStats>()
        );
    }

    /// Opens `spec` (in the next frame's `PreUpdate`) and settles, capturing `capture`.
    fn open(&mut self, spec: WorldSpec, capture: &'static str) {
        self.lifecycle().request_open(spec);
        self.settle(capture);
    }

    /// Closes the world in the next frame, captured as `capture`: with saves on inline jobs it
    /// ends in that frame.
    fn close(&mut self, capture: &'static str) {
        self.lifecycle().request_close(CloseOptions::default());
        self.frame(Some(capture));
        assert_eq!(
            self.status(),
            WorldStatus::Closed,
            "the close ended in its frame"
        );
    }
}

/// What a replay leaves of a captured frame: the image, the renderer's per-world state, its
/// chunks and LOD nodes, the LOD nodes drawn and the fog passes.
struct Captured {
    image: RgbaImage,
    state: ResetState,
    chunks: u32,
    lod_nodes: u32,
    lod_drawn: u32,
    fog_passes: u32,
}

/// Replays `frames` on a fresh renderer as the render plugin's `extract_visibility` and
/// `prepare` run them, with the fog clock at `fog_time` if given. No chunk may be refused: the
/// recorded cave culling sets never heard of a refusal.
fn replay(
    gpu: &Gpu<'_>,
    frames: &[Frame],
    fog_time: Option<FogTime>,
) -> Result<HashMap<&'static str, Captured>, String> {
    let mut renderer = new_renderer(gpu, &config())?;
    let mut out = HashMap::new();
    for (i, frame) in frames.iter().enumerate() {
        if frame.resync == Resync::World {
            renderer.take_refused(|_, _| {});
            renderer.take_undrawn(|_, _| {});
        } else {
            let refusals = reports_refusals(&mut renderer);
            if !refusals.is_empty() {
                return Err(format!("frame {i}: refused chunks {refusals:?}"));
            }
        }
        // `prepare`'s order: the frame's ops (with the clear its resync asks for), then the
        // visible set, the fog and the far LOD
        let ops: Vec<ChunkOp> = frame
            .meshes
            .iter()
            .map(|op| match op {
                MeshOp::Upsert { id, origin, mesh } => ChunkOp::Upsert {
                    id: *id,
                    origin: *origin,
                    mesh: Arc::clone(mesh),
                },
                MeshOp::Remove { id } => ChunkOp::Remove { id: *id },
            })
            .collect();
        if let Some(e) = renderer.apply_ops(gpu.device, ops, frame.resync).first() {
            return Err(format!("frame {i}: {e}"));
        }
        renderer.set_visibility(frame.visibility.clone());
        let fog = frame.fog.clone().map(|fog| match fog_time {
            Some(time) => FogInput { time, ..fog },
            None => fog,
        });
        renderer.set_fog(gpu.device, fog);
        renderer.set_lod(gpu.device, frame.lod.clone());
        match frame.capture {
            None => draw(gpu, &mut renderer, &frame.view)?,
            Some(name) => {
                let image = render(gpu, &mut renderer, &frame.view)?;
                let stats = renderer.stats();
                let captured = Captured {
                    image,
                    state: renderer.debug_reset_state(),
                    chunks: stats.chunks,
                    lod_nodes: stats.lod_nodes,
                    lod_drawn: stats.lod_drawn,
                    fog_passes: stats.fog_passes,
                };
                out.insert(name, captured);
            }
        }
    }
    Ok(out)
}

/// The frame `name` of `frames`.
fn captured_frame<'a>(frames: &'a [Frame], name: &str) -> &'a Frame {
    frames
        .iter()
        .find(|f| f.capture == Some(name))
        .unwrap_or_else(|| panic!("no frame {name}"))
}

/// What a renderer that never had a world draws for `frame` (its view, fog and LOD input).
fn no_world(gpu: &Gpu<'_>, frame: &Frame) -> Result<(RgbaImage, ResetState), String> {
    let mut renderer = new_renderer(gpu, &config())?;
    let state = renderer.debug_reset_state();
    renderer.set_fog(gpu.device, frame.fog.clone());
    renderer.set_lod(gpu.device, frame.lod.clone());
    Ok((render(gpu, &mut renderer, &frame.view)?, state))
}

// ---------------------------------------------------------------- world switches

/// The clocks of the worlds (`TimeOfDay::tick`): A at dusk, B before noon.
fn clocks() -> (u64, u64) {
    (
        TimeOfDay::at_clock(18.6).tick,
        TimeOfDay::at_clock(10.5).tick,
    )
}

fn hills(seed: u64, shift: I64Vec3, clock: u64) -> WorldSpec {
    WorldSpec::new(
        Arc::new(Hills {
            seed,
            origin: shift,
        }),
        world_settings(),
    )
    .with_clock(clock)
}

/// The camera over the hills moved by `shift` (no position equidistant from two chunk centers),
/// and the viewer there.
fn hills_pose(shift: I64Vec3) -> (Camera, Transform, ChunkViewer) {
    let s = shift.as_dvec3();
    let (camera, transform) = look(
        DVec3::new(37.31, 58.7, 61.13) + s,
        DVec3::new(-14.0, 16.0, -9.0) + s,
    );
    (
        camera,
        transform,
        ChunkViewer::new(4).with_vertical_radius(2),
    )
}

/// The recordings of the hills at one place.
struct Plain {
    /// A, its close (`closed`, then `still_closed` frames later), B.
    switched: Vec<Frame>,
    /// B in a fresh app.
    fresh: Vec<Frame>,
    /// A, then a close and an open in one frame (`switch`), B.
    same_frame: Vec<Frame>,
}

/// The recordings at the origin (`place` 0) or 2^40 away (1), made once per test binary.
fn plain(place: usize) -> &'static Plain {
    static PLAIN: [OnceLock<Plain>; 2] = [const { OnceLock::new() }; 2];
    PLAIN[place].get_or_init(|| {
        let shift = [I64Vec3::ZERO, FAR][place];
        let (clock_a, clock_b) = clocks();
        let (camera, transform, viewer) = hills_pose(shift);
        let recorder = || Recorder::new(camera, transform, viewer, false, false);

        let mut switched = recorder();
        switched.open(hills(1, shift, clock_a), "a");
        switched.close("closed");
        for _ in 0..2 {
            switched.frame(None);
        }
        switched.frame(Some("still_closed"));
        switched.open(hills(2, shift, clock_b), "b");

        let mut fresh = recorder();
        fresh.open(hills(2, shift, clock_b), "b");
        let translucent = fresh.frames.iter().flat_map(|f| &f.meshes).any(|op| {
            matches!(op, MeshOp::Upsert { mesh, .. }
                if mesh.bucket_counts[boxel_mesh::TRANSLUCENT_BUCKET] > 0)
        });
        assert!(translucent, "B has water");

        let mut same = recorder();
        same.open(hills(1, shift, clock_a), "a");
        let epoch = same.epoch();
        same.lifecycle().request_close(CloseOptions::default());
        same.lifecycle().request_open(hills(2, shift, clock_b));
        same.frame(Some("switch"));
        assert_eq!(
            (same.status(), same.epoch()),
            (WorldStatus::Open, epoch + 2),
            "a close and an open in one frame"
        );
        assert_eq!(captured_frame(&same.frames, "switch").resync, Resync::World);
        same.settle("b");
        Plain {
            switched: switched.frames,
            fresh: fresh.frames,
            same_frame: same.frames,
        }
    })
}

/// Pixels of the animated stone's tick 0 frame (red) in `image`.
fn red_pixels(image: &RgbaImage) -> usize {
    image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > p[1].saturating_add(50) && p[0] > p[2].saturating_add(50))
        .count()
}

/// Test 24: the frame of a close, and the frames after it, are sky only, as a renderer that
/// never had a world draws them; no chunk is left and the renderer's per-world state is a fresh
/// renderer's (joint case (a) through the world's close).
#[test]
fn the_frame_after_a_close_is_sky_only() {
    let plain = plain(0);
    for_every_tier(|gpu, report| {
        let run = |report: &mut Report| -> Result<(), String> {
            let captured = replay(gpu, &plain.switched, None)?;
            let what = format!("{} {}", gpu.label, gpu.tier);
            for name in ["closed", "still_closed"] {
                let c = &captured[name];
                let (empty, fresh) = no_world(gpu, captured_frame(&plain.switched, name))?;
                report.check_identical(
                    &format!("{what}: {name}"),
                    (&gpu.name(name), &c.image),
                    (&gpu.name(&format!("{name}_no_world")), &empty),
                );
                report.check(c.chunks == 0 && c.state == fresh, || {
                    format!(
                        "{what}: {name}: {} chunks, {:?}, fresh {fresh:?}",
                        c.chunks, c.state
                    )
                });
            }
            let a = &captured["a"].image;
            let shown = a.compare(&captured["closed"].image, 2).fraction();
            report.check(shown > 0.2, || {
                format!("{what}: A covered only {:.1} % of the image", shown * 100.0)
            });
            Ok(())
        };
        if let Err(e) = run(report) {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// Checks that B after A (`after`) draws as a fresh B (`fresh`) on this tier, and that both
/// draw as the adapter's best tier's fresh B (`name`).
fn check_switch(
    gpu: &Gpu<'_>,
    report: &mut Report,
    name: &str,
    after: &Captured,
    fresh: &Captured,
) {
    let what = format!("{} {} {name}", gpu.label, gpu.tier);
    report.keep(&[(&gpu.name(&format!("{name}_fresh")), &fresh.image)]);
    report.check_close(
        &format!("{what}: B after A"),
        (&gpu.name(&format!("{name}_after_a")), &after.image),
        (&gpu.name(&format!("{name}_fresh")), &fresh.image),
    );
    report.check(
        after.chunks == fresh.chunks && after.lod_nodes == fresh.lod_nodes,
        || {
            format!(
                "{what}: B after A holds {} chunks and {} LOD nodes, a fresh B {} and {}",
                after.chunks, after.lod_nodes, fresh.chunks, fresh.lod_nodes
            )
        },
    );
    report.check_tiers(gpu, name, &fresh.image);
    report.check_tiers(gpu, name, &after.image);
}

/// Test 25: B after A, settled, draws as a fresh B on each tier and across tiers, with water and
/// the animated stone in view (at tick 0, as both sides pin it), at the origin and 2^40 away.
/// B's clock is its own: A's dusk is gone.
#[test]
fn a_world_after_another_draws_as_a_fresh_one() {
    for_every_tier(|gpu, report| {
        for (place, name) in [(0, "hills_origin"), (1, "hills_far")] {
            let run = |report: &mut Report| -> Result<(), String> {
                let plain = plain(place);
                let switched = replay(gpu, &plain.switched, None)?;
                let fresh = replay(gpu, &plain.fresh, None)?;
                check_switch(gpu, report, name, &switched["b"], &fresh["b"]);
                let what = format!("{} {} {name}", gpu.label, gpu.tier);
                let red = red_pixels(&fresh["b"].image);
                report.check(red > 100, || {
                    format!("{what}: the animated stone shows in {red} pixels")
                });
                let other = switched["a"].image.compare(&fresh["b"].image, 2).fraction();
                report.check(other > 0.2, || {
                    format!(
                        "{what}: A and B differ in {:.1} % of the pixels",
                        other * 100.0
                    )
                });
                Ok(())
            };
            if let Err(e) = run(report) {
                report.fail(format!("{} {} {name}: {e}", gpu.label, gpu.tier), &[]);
            }
        }
    })
    .finish();
}

/// Test 27: a close and an open in the same frame (saves on inline jobs: the close ends at once
/// and the open runs right after it, test 25's world B) give test 25's image. That frame is a
/// `Resync::World` frame whose upload is only B's meshes.
#[test]
fn a_close_and_an_open_in_one_frame_draw_as_a_fresh_world() {
    let plain = plain(0);
    for_every_tier(|gpu, report| {
        let run = |report: &mut Report| -> Result<(), String> {
            let switched = replay(gpu, &plain.same_frame, None)?;
            let fresh = replay(gpu, &plain.fresh, None)?;
            check_switch(gpu, report, "same_frame", &switched["b"], &fresh["b"]);
            let uploads = captured_frame(&plain.same_frame, "switch").meshes.len() as u32;
            let left = switched["switch"].chunks;
            report.check(left == uploads, || {
                format!(
                    "{} {}: the switch frame holds {left} chunks for {uploads} uploads of B",
                    gpu.label, gpu.tier
                )
            });
            Ok(())
        };
        if let Err(e) = run(report) {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}

/// World A's fog clock in the forests: well past B's start and between two ticks.
const A_FOG_TIME: FogTime = FogTime::new(5000, 0x8000);

/// The recordings of the forests with their far LOD and the demo's valley fog.
struct Forests {
    /// A (its fog clock moved on), its close (`closed`, `still_closed`), B.
    switched: Vec<Frame>,
    /// B in a fresh app.
    fresh: Vec<Frame>,
}

fn forests() -> &'static Forests {
    static FORESTS: OnceLock<Forests> = OnceLock::new();
    FORESTS.get_or_init(|| {
        use boxel_demo::worldgen::TerrainSettings;
        let (clock_a, clock_b) = clocks();
        let a = || ForestGenerator::new(7, TerrainSettings::default());
        let b = ForestGenerator::default;
        let (x, z) = (40.37, 24.21);
        let ground = f64::from(b().terrain.height_at(x as i64, z as i64)).max(46.0);
        let transform =
            Transform::from_yaw_pitch(DVec3::new(x, ground + 24.41, z), 90f32.to_radians(), -0.12);
        let viewer = ChunkViewer::new(4).with_vertical_radius(8);
        let recorder = || Recorder::new(Camera::default(), transform, viewer, true, true);
        let spec = |generator: ForestGenerator, clock: u64| {
            WorldSpec::new(Arc::new(generator), world_settings()).with_clock(clock)
        };

        let mut switched = recorder();
        switched.lifecycle().request_open(spec(a(), clock_a));
        switched.frame(None);
        // A's fog has blown for a while and stopped half a tick on, paused as a screenshot pauses
        // it (a running clock rewrites the subtick every frame, so only a paused one could carry
        // A's subtick into B).
        *switched.app.world_mut().resource_mut::<FogClock>() = FogClock {
            tick: A_FOG_TIME.tick,
            subtick: A_FOG_TIME.subtick,
            paused: true,
        };
        switched.settle("a");
        switched.close("closed");
        switched.frame(Some("still_closed"));
        switched.open(spec(b(), clock_b), "b");
        let fog_time = |name| {
            captured_frame(&switched.frames, name)
                .fog
                .as_ref()
                .expect("fog")
                .time
        };
        assert_eq!(
            (fog_time("a"), fog_time("b")),
            (A_FOG_TIME, FogTime::new(0, 0)),
            "the fog clock of each world"
        );

        let mut fresh = recorder();
        fresh.open(spec(b(), clock_b), "b");
        Forests {
            switched: switched.frames,
            fresh: fresh.frames,
        }
    })
}

/// Test 26, on every tier but GL (where the LOD stays off; Low draws it as Mid does): the LOD
/// arena is empty from the frame of a close on, as a renderer that never had a world draws it;
/// B after A then draws its LOD, with no horizon of A left, and the fog layers as a fresh B (the
/// fog clock, tick and subtick, is the open's).
#[test]
fn the_lod_and_the_fog_start_again_with_the_next_world() {
    let forests = forests();
    for_every_tier(|gpu, report| {
        if gpu.gl() {
            return;
        }
        let run = |report: &mut Report| -> Result<(), String> {
            let switched = replay(gpu, &forests.switched, None)?;
            let fresh = replay(gpu, &forests.fresh, None)?;
            let what = format!("{} {}", gpu.label, gpu.tier);
            report.check(switched["a"].lod_nodes > 0, || {
                format!("{what}: A has no LOD")
            });
            let b = &fresh["b"];
            report.check(b.lod_drawn > 0 && b.fog_passes > 0, || {
                format!(
                    "{what}: B draws {} LOD nodes and {} fog passes",
                    b.lod_drawn, b.fog_passes
                )
            });
            // The control: B with A's fog clock looks otherwise.
            let blown = replay(gpu, &forests.fresh, Some(A_FOG_TIME))?;
            let moved = blown["b"].image.compare(&b.image, 2).fraction();
            report.check(moved > 0.002, || {
                format!(
                    "{what}: A's fog clock moves {:.3} % of B's pixels",
                    moved * 100.0
                )
            });
            for name in ["closed", "still_closed"] {
                let c = &switched[name];
                let (empty, _) = no_world(gpu, captured_frame(&forests.switched, name))?;
                report.check(c.lod_nodes == 0 && c.chunks == 0, || {
                    format!(
                        "{what}: {name}: {} LOD nodes, {} chunks",
                        c.lod_nodes, c.chunks
                    )
                });
                report.check_identical(
                    &format!("{what}: {name} with LOD and fog"),
                    (&gpu.name(&format!("forest_{name}")), &c.image),
                    (&gpu.name(&format!("forest_{name}_no_world")), &empty),
                );
            }
            check_switch(gpu, report, "forest", &switched["b"], &fresh["b"]);
            Ok(())
        };
        if let Err(e) = run(report) {
            report.fail(format!("{} {}: {e}", gpu.label, gpu.tier), &[]);
        }
    })
    .finish();
}
