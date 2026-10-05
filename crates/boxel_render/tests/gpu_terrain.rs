//! Real terrain on every tier. Self-skips without an adapter unless `BOXEL_REQUIRE_GPU=1`.
//!
//! Same procedure as `gpu_tiers.rs`, but the scene comes from the engine instead of hand-made
//! quads: `TerrainGenerator::default()` generates every vertical chunk of 8x8 columns around the
//! origin, and `boxel_mesh::Mesher` meshes the inner 6x6 columns with their 26 neighbors (missing
//! neighbors outside the generator's vertical range are air), exactly as the world streams them.
//!
//! For every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local
//! capability check allows (the one of `gpu_tiers.rs`), a `VoxelRenderer` uploads the meshes and
//! draws two fixed views offscreen: an overview from above one corner and a low view across the
//! surface. Asserts: the upload stats match the scene, every image has a non-background fraction
//! in [0.05, 0.95], and every tier matches the best tier of its adapter within 0.1 % of the
//! pixels (channel tolerance 2). Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_terrain/`
//! (every image with `GPU_TERRAIN_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, IVec3, UVec2};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, Mesher};
use boxel_render::{RendererConfig, ViewParams, VoxelRenderer};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, ChunkData, ChunkKey, ChunkPos, NEIGHBOR_OFFSETS};
use boxel_worldgen::{TerrainGenerator, WorldGenerator};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_TIER_DIFF: f64 = 0.001;
/// Meshed columns are `-MESH_RADIUS..MESH_RADIUS` in x and z (data goes one column further).
const MESH_RADIUS: i32 = 3;

// ---------------------------------------------------------------- adapters

/// Headless instance (`InstanceFlags::empty()`, `WGPU_BACKEND` or Vulkan + GL) and its adapters
/// filtered by `WGPU_ADAPTER_NAME` (case-insensitive substring).
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

/// Tiers this adapter can run, from the `[S]` pieces (same check as `gpu_tiers.rs`): High =
/// MDI-count + first-instance indirect + compute on Vulkan; Mid = vertex storage (GL: a context
/// that really has SSBOs); Low = immediates. Software adapters are not capped.
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

/// A device shaped like the one selection would create for `tier`, with uncaptured errors
/// collected instead of panicking.
fn device_for(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
) -> Result<(wgpu::Device, wgpu::Queue, Arc<Mutex<Vec<String>>>), String> {
    let request = tier_request(tier, &caps.limits);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu_terrain"),
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

// ---------------------------------------------------------------- scene

/// The meshed terrain and what the views need to know about it.
struct Scene {
    registry: BlockRegistry,
    /// Non-empty meshes: (chunk position, mesh).
    meshes: Vec<(ChunkPos, ChunkMesh)>,
    /// Highest terrain column in the meshed area.
    max_height: i32,
}

impl Scene {
    fn quads(&self) -> u32 {
        self.meshes.iter().map(|(_, m)| m.total_quads()).sum()
    }
}

/// Generates the 8x8 data columns and meshes the inner 6x6 with `boxel_mesh::Mesher`.
fn terrain_scene() -> Scene {
    let registry = BlockRegistry::default_blocks();
    let generator = TerrainGenerator::default();
    let vertical = generator.vertical_chunks();
    let mut data: BTreeMap<ChunkPos, ChunkData> = BTreeMap::new();
    for y in vertical.clone() {
        for z in -MESH_RADIUS - 1..=MESH_RADIUS {
            for x in -MESH_RADIUS - 1..=MESH_RADIUS {
                let pos = ChunkPos::new(x, y, z);
                data.insert(pos, generator.generate(ChunkKey::new(pos), &registry));
            }
        }
    }

    let materials = MaterialTable::from_registry(&registry);
    let mut mesher = Mesher::new();
    let mut meshes = Vec::new();
    for (&pos, center) in &data {
        if !(-MESH_RADIUS..MESH_RADIUS).contains(&pos.x)
            || !(-MESH_RADIUS..MESH_RADIUS).contains(&pos.z)
        {
            continue;
        }
        let input = MeshInput {
            center,
            neighbors: std::array::from_fn(|i| data.get(&pos.offset(NEIGHBOR_OFFSETS[i]))),
        };
        let mesh = mesher.mesh_chunk(&input, &materials);
        if !mesh.is_empty() {
            meshes.push((pos, mesh));
        }
    }

    let extent = MESH_RADIUS * 32;
    let mut max_height = i32::MIN;
    for z in -extent..extent {
        for x in -extent..extent {
            max_height = max_height.max(generator.height_at(x, z));
        }
    }
    Scene {
        registry,
        meshes,
        max_height,
    }
}

/// Chunk id used by the test (any stable u64).
fn chunk_id(c: ChunkPos) -> u64 {
    let pack = |v: i32| u64::from((v + 512) as u32 & 0x3ff);
    pack(c.x) | pack(c.y) << 10 | pack(c.z) << 20
}

fn view_params(transform: &Transform) -> ViewParams {
    let camera = Camera::default();
    let view = CameraView::compute(&camera, transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, &camera, RendererConfig::default().clear_color, 0.6)
}

/// From above the (-x, -z) corner of the meshed area, looking at its center.
fn overview(scene: &Scene) -> ViewParams {
    let eye = DVec3::new(-110.3, f64::from(scene.max_height) + 40.6, -104.2);
    let target = DVec3::new(0.0, f64::from(scene.max_height) - 30.0, 0.0);
    let d = (target - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    view_params(&Transform::from_yaw_pitch(eye, yaw, pitch))
}

/// Just above the highest column at the -x edge, looking across the surface toward +x (hills,
/// valleys and the horizon at the far edge, many chunks in depth).
fn across(scene: &Scene) -> ViewParams {
    let eye = DVec3::new(-94.6, f64::from(scene.max_height) + 3.4, 8.7);
    // yaw -90 degrees turns the -z forward vector toward +x
    view_params(&Transform::from_yaw_pitch(
        eye,
        -90f32.to_radians(),
        -12f32.to_radians(),
    ))
}

// ---------------------------------------------------------------- rendering

/// Renders one frame inside error scopes and reads the main color back; also returns the CPU
/// time of `VoxelRenderer::render`.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<(RgbaImage, Duration), String> {
    let mut cpu = Duration::ZERO;
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_terrain"),
        });
        let start = Instant::now();
        renderer.render(device, queue, &mut encoder, view);
        cpu = start.elapsed();
        submit_and_wait(device, queue, encoder)
    })?;
    let image = read_texture_rgba8(device, queue, renderer.main_color())?;
    Ok((image, cpu))
}

/// Images of one tier, in the order of [`VIEWS`].
struct TierImages {
    tier: Tier,
    images: Vec<RgbaImage>,
}

/// View names, in the order `render_tier` renders them.
const VIEWS: [&str; 2] = ["overview", "across"];

/// Uploads the scene and renders every view on one tier.
fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    scene: &Scene,
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        arena_quads: 1 << 20,
        max_chunk_slots: 1024,
        ..RendererConfig::default()
    };
    let mut renderer = VoxelRenderer::new(
        &device,
        &queue,
        tier,
        &config,
        scene.registry.textures(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    if renderer.tier() != tier {
        return Err(format!("renderer reports tier {}", renderer.tier()));
    }
    for (pos, mesh) in &scene.meshes {
        let origin = IVec3::new(pos.x, pos.y, pos.z) * 32;
        renderer
            .upsert_chunk(&queue, chunk_id(*pos), origin, mesh)
            .map_err(|e| format!("upsert {pos:?}: {e}"))?;
    }
    let stats = renderer.stats();
    if stats.chunks as usize != scene.meshes.len() || stats.arena_used_quads != scene.quads() {
        return Err(format!(
            "stats after upload: {stats:?}, expected {} chunks and {} quads",
            scene.meshes.len(),
            scene.quads()
        ));
    }

    let mut images = Vec::new();
    for (name, view) in VIEWS.iter().zip([overview(scene), across(scene)]) {
        render(&device, &queue, &mut renderer, &view)?; // warm-up
        let (image, cpu) = render(&device, &queue, &mut renderer, &view)?;
        let stats = renderer.stats();
        eprintln!(
            "gpu_terrain: {} {tier} {name}: render {:.3} ms cpu, chunks={} quads={} draws={} \
             visible={} non_background={:.3}",
            adapter_label(caps),
            cpu.as_secs_f64() * 1e3,
            stats.chunks,
            stats.arena_used_quads,
            stats.draws,
            stats.visible_chunks,
            image.non_background_fraction(),
        );
        images.push(image);
    }

    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(TierImages { tier, images })
}

// ---------------------------------------------------------------- checks

/// Collects failures and saves the images involved.
struct Report {
    failures: Vec<String>,
    dir: PathBuf,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_terrain"),
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_terrain: FAIL {message}");
        self.save(images);
        self.failures.push(message);
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
                Ok(()) => eprintln!("gpu_terrain: saved {}", path.display()),
                Err(e) => eprintln!("gpu_terrain: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn check_range(&mut self, what: &str, image: &RgbaImage) {
        let range = 0.05..=0.95;
        let f = image.non_background_fraction();
        if !range.contains(&f) {
            self.fail(
                format!("{what}: non-background fraction {f:.4} outside {range:?}"),
                &[(what, image)],
            );
        }
    }

    fn check_same(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        let diff = a.1.compare(b.1, 2);
        eprintln!(
            "gpu_terrain: {what}: {:.4} % of pixels differ, max channel {}",
            diff.fraction() * 100.0,
            diff.max_channel_diff
        );
        if diff.fraction() > MAX_TIER_DIFF {
            self.fail(
                format!(
                    "{what}: {} of {} pixels differ ({:.4} %, max channel {})",
                    diff.differing_pixels,
                    diff.total_pixels,
                    diff.fraction() * 100.0,
                    diff.max_channel_diff
                ),
                &[a, b],
            );
        }
    }
}

#[test]
fn real_terrain_renders_identically_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let start = Instant::now();
    let scene = terrain_scene();
    eprintln!(
        "gpu_terrain: scene: {} non-empty chunks, {} quads, max height {} ({:.0} ms on the CPU)",
        scene.meshes.len(),
        scene.quads(),
        scene.max_height,
        start.elapsed().as_secs_f64() * 1e3
    );
    assert!(
        scene.meshes.len() >= 36,
        "at least one surface chunk per meshed column"
    );

    let mut report = Report::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let tiers = allowed_tiers(&caps);
        eprintln!("gpu_terrain: {label}: tiers {tiers:?}");
        let mut results = Vec::new();
        for &tier in &tiers {
            match render_tier(adapter, &caps, tier, &scene) {
                Ok(r) => results.push(r),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        let save_all = std::env::var_os("GPU_TERRAIN_SAVE").is_some();
        for r in &results {
            rendered += 1;
            for (view, image) in VIEWS.iter().zip(&r.images) {
                let name = format!("{label}_{}_{view}", r.tier);
                if save_all {
                    report.save(&[(&name, image)]);
                }
                report.check_range(&name, image);
            }
        }
        let Some((best, rest)) = results.split_first() else {
            continue;
        };
        for r in rest {
            for (i, view) in VIEWS.iter().enumerate() {
                report.check_same(
                    &format!("{label}: {} vs {} ({view})", r.tier, best.tier),
                    (&format!("{label}_{}_{view}", best.tier), &best.images[i]),
                    (&format!("{label}_{}_{view}", r.tier), &r.images[i]),
                );
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
        rendered > 0 || !gpu_tests_required(),
        "BOXEL_REQUIRE_GPU=1 but no tier could render"
    );
}
