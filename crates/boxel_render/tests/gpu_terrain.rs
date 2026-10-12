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
//! pixels (channel tolerance 2). Then every tier renders the same scene and views moved far from
//! the origin ([`FAR_SHIFTS`]: straddling the former `i32` edge at `+-2^31` in x, z and y, past
//! it, and far beyond) and must give exactly the pixels it gave near the origin: chunk origins
//! are `i64` on the CPU and wrapped to 32 bits for the GPU, whose wrapping subtraction gives the
//! same camera-relative positions. Every renderer also teleports its camera `2^32` voxels away
//! on every axis with the chunks still resident and must draw nothing there, then the original
//! pixels again when the camera comes back. Their wrapped origins land exactly under the camera,
//! and seen in `i64` they lie straight ahead, inside a view distance of `2^34`: neither the
//! frustum nor the view distance hides them, only the GPU window does, on every tier.
//!
//! A second test renders a lit scene built by hand (ADR 0015): a tree, a roof over the ground, a
//! low wall beside sunlit ground (AO), a closed cave lit by red, blue and orange lamps, and an
//! overview at noon and at midnight. Its light is computed exactly as the world computes it
//! (sky tops per column, `boxel_light::compute`, the default AO policy), again at every
//! [`FAR_SHIFTS`] position, where the meshes must come out byte for byte the same. Every tier
//! must draw exactly the same pixels (software adapters: the tolerance above), the moved scene
//! the same pixels as near the origin, the cave the same at noon and at midnight (block light
//! does not follow the sun), the night overview at most half as bright as noon, the cave tinted
//! by its lamps, the shadowed views darker than without light and AO, and the face of a white
//! lamp glowing: at the full brightness of its texture, the same at noon, at midnight and
//! without light.
//!
//! A third test looks for cracks: views of the terrain at 1280x720 on a magenta background must
//! show no island of magenta inside the terrain (T-junctions between greedy quads, closed by the
//! edge growth of `common.wgsl`).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_terrain/` (every image with
//! `GPU_TERRAIN_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_demo::worldgen::TerrainGenerator;
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{Aabb, DVec3, I64Vec3, UVec2, Vec3};
use boxel_mesh::{AoPolicy, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::{
    DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, TranslucentSort, ViewParams, VoxelRenderer,
    arena_len,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, NEIGHBOR_OFFSETS};
use boxel_worldgen::WorldGenerator;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_TIER_DIFF: f64 = 0.001;
/// Meshed columns are `-MESH_RADIUS..MESH_RADIUS` in x and z (data goes one column further).
const MESH_RADIUS: i64 = 3;
/// Far-from-origin translations of the scene and the views (voxels, multiples of 32 so the chunk
/// grid lines up with the one near the origin).
const FAR_SHIFTS: [I64Vec3; 5] = [
    // The meshed area straddles +2^31 in x and -2^31 in z (where the wrapped origins jump).
    I64Vec3::new((1 << 31) - 64, 0, -(1 << 31) + 64),
    // Just past them.
    I64Vec3::new((1 << 31) + (1 << 20), 0, -(1 << 31) - (1 << 20)),
    // 2^40 away and across the wrap again (2^40 alone wraps to the same 32 bits as 0).
    I64Vec3::new((1 << 40) + (1 << 31) - 64, 0, -(1 << 40) - (1 << 31) + 64),
    // The surface (around y = 64) straddles +2^31 in y, then -2^31.
    I64Vec3::new(-(1 << 35), (1 << 31) - 64, 1 << 35),
    I64Vec3::new(1 << 31, -(1 << 31) - 64, 0),
];

/// How far the camera teleports with the chunks left resident: their wrapped origins are then
/// the same as near the camera. The chunks are left `2^32` voxels behind on every axis, which
/// from there is straight ahead of the overview (about 10 degrees off its center).
const TELEPORT: I64Vec3 = I64Vec3::new(-(1 << 32), 1 << 32, -(1 << 32));

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
    meshes: Vec<(ChunkPos, Arc<ChunkMesh>)>,
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
    let registry = boxel_demo::blocks::registry();
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
        // No light (open sky everywhere) and the default AO policy: AO on every face.
        let input = MeshInput {
            center,
            neighbors: std::array::from_fn(|i| data.get(&pos.offset(NEIGHBOR_OFFSETS[i]))),
            light: None,
        };
        let mesh = mesher.mesh_chunk(&input, &materials, MeshOptions::default());
        if !mesh.is_empty() {
            meshes.push((pos, Arc::new(mesh)));
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
    let pack = |v: i64| (v + 512) as u64 & 0x3ff;
    pack(c.x) | pack(c.y) << 10 | pack(c.z) << 20
}

/// `transform` moved by `shift`, seen with `camera`. Eye coordinates are multiples of 1/64,
/// exact in `f64` at every shift, so the camera-relative eye and the view matrix are the same as
/// near the origin.
fn view_params(camera: &Camera, transform: &Transform, shift: I64Vec3) -> ViewParams {
    let mut transform = *transform;
    transform.translation += shift.as_dvec3();
    let view = CameraView::compute(camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, camera, DEFAULT_SKY_COLOR, DistanceFog::default())
}

/// The default camera with a view distance of `2^34` voxels, past the chunks left behind by a
/// [`TELEPORT`]. Fog starts at 60 % of it, so it does not touch anything near the camera.
fn far_sighted() -> Camera {
    Camera {
        view_distance: 17_179_869_184.0,
        ..Camera::default()
    }
}

/// From above the (-x, -z) corner of the meshed area, looking at its center.
fn overview(camera: &Camera, scene: &Scene, shift: I64Vec3) -> ViewParams {
    let eye = DVec3::new(
        -110.296_875,
        f64::from(scene.max_height) + 40.593_75,
        -104.203_125,
    );
    let target = DVec3::new(0.0, f64::from(scene.max_height) - 30.0, 0.0);
    let d = (target - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    view_params(camera, &Transform::from_yaw_pitch(eye, yaw, pitch), shift)
}

/// Just above the highest column at the -x edge, looking across the surface toward +x (hills,
/// valleys and the horizon at the far edge, many chunks in depth).
fn across(scene: &Scene, shift: I64Vec3) -> ViewParams {
    let eye = DVec3::new(
        -94.593_75,
        f64::from(scene.max_height) + 3.406_25,
        8.703_125,
    );
    // yaw -90 degrees turns the -z forward vector toward +x
    view_params(
        &Camera::default(),
        &Transform::from_yaw_pitch(eye, -90f32.to_radians(), -12f32.to_radians()),
        shift,
    )
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

/// Images of one tier.
struct TierImages {
    tier: Tier,
    /// In the order of [`VIEWS`].
    images: Vec<RgbaImage>,
    /// The overview moved by [`TELEPORT`] with the scene left where it was, seen with
    /// [`far_sighted`]: right after the teleport, and after uploading every mesh again from there.
    teleported: [RgbaImage; 2],
    /// The overview again, back at the scene.
    back: RgbaImage,
}

/// View names, in the order `render_tier` renders them.
const VIEWS: [&str; 2] = ["overview", "across"];

/// Uploads the scene moved by `shift` and renders every view (moved the same way) on one tier,
/// then the overview after a [`TELEPORT`] and back.
fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    scene: &Scene,
    shift: I64Vec3,
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        chunk_memory: boxel_render::ChunkMemorySettings {
            initial_quads: 1 << 20,
            initial_slots: 1024,
            ..boxel_render::ChunkMemorySettings::default()
        },
        translucent_sort: TranslucentSort::Inline,
    };
    let mut renderer = VoxelRenderer::new(
        &device,
        &queue,
        tier,
        &config,
        &boxel_render::generate_block_textures(scene.registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    if renderer.tier() != tier {
        return Err(format!("renderer reports tier {}", renderer.tier()));
    }
    let upload = |renderer: &mut VoxelRenderer| -> Result<(), String> {
        for (pos, mesh) in &scene.meshes {
            let origin = pos.origin().as_i64vec3() + shift;
            renderer
                .upsert_chunk(chunk_id(*pos), origin, Arc::clone(mesh))
                .map_err(|e| format!("upsert {pos:?}: {e}"))?;
        }
        Ok(())
    };
    upload(&mut renderer)?;
    let stats = renderer.stats();
    let entries: u32 = scene.meshes.iter().map(|(_, m)| arena_len(m)).sum();
    if stats.chunks as usize != scene.meshes.len() || stats.arena_used_quads != entries {
        return Err(format!(
            "stats after upload: {stats:?}, expected {} chunks and {entries} arena entries",
            scene.meshes.len(),
        ));
    }

    let mut images = Vec::new();
    for (name, view) in VIEWS.iter().zip([
        overview(&Camera::default(), scene, shift),
        across(scene, shift),
    ]) {
        render(&device, &queue, &mut renderer, &view)?; // warm-up
        let (image, cpu) = render(&device, &queue, &mut renderer, &view)?;
        let stats = renderer.stats();
        eprintln!(
            "gpu_terrain: {} {tier} {name} shift {shift}: render {:.3} ms cpu, chunks={} quads={} \
             draws={} visible={} non_background={:.3}",
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

    // The old chunks are still resident after a teleport (the world removes them over a few
    // frames). Their wrapped origins equal the ones they had near the camera: only an i64 test
    // keeps them from being drawn under it. In i64 they are in the frustum of the far-sighted
    // view, so the CPU cull of Mid and Low cannot drop them by distance or direction either.
    let away = overview(&far_sighted(), scene, shift + TELEPORT);
    for (pos, _) in &scene.meshes {
        let min = (pos.origin().as_i64vec3() + shift - away.render_origin).as_vec3();
        if !away
            .frustum
            .intersects_aabb(&Aabb::new(min, min + Vec3::splat(32.0)))
        {
            return Err(format!(
                "teleported view: {pos:?} left behind is not in the frustum"
            ));
        }
    }
    let (teleported, _) = render(&device, &queue, &mut renderer, &away)?;
    upload(&mut renderer)?;
    let (uploaded, _) = render(&device, &queue, &mut renderer, &away)?;
    let (back, _) = render(
        &device,
        &queue,
        &mut renderer,
        &overview(&Camera::default(), scene, shift),
    )?;

    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(TierImages {
        tier,
        images,
        teleported: [teleported, uploaded],
        back,
    })
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

    /// After the teleport of `r` nothing is drawn; back at the scene, the overview is unchanged.
    fn check_teleport(&mut self, what: &str, r: &TierImages) {
        for (when, image) in ["at_once", "reuploaded"].iter().zip(&r.teleported) {
            let f = image.non_background_fraction();
            if f > 0.0 {
                let name = format!("{what}_teleported_{when}");
                self.fail(
                    format!("{what}: {f:.4} of the pixels drawn after a teleport ({when})"),
                    &[(&name, image)],
                );
            }
        }
        self.check_identical(
            &format!("{what}: back from a teleport"),
            (&format!("{what}_overview"), &r.images[0]),
            (&format!("{what}_back"), &r.back),
        );
    }

    fn check_identical(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        let diff = a.1.compare(b.1, 0);
        if diff.differing_pixels > 0 {
            self.fail(
                format!(
                    "{what}: {} of {} pixels differ (max channel {})",
                    diff.differing_pixels, diff.total_pixels, diff.max_channel_diff
                ),
                &[a, b],
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
            match render_tier(adapter, &caps, tier, &scene, I64Vec3::ZERO) {
                Ok(r) => results.push(r),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        // The same scene and views far from the origin: the same pixels on each tier.
        for near in &results {
            report.check_teleport(&format!("{label}_{}", near.tier), near);
            for shift in FAR_SHIFTS {
                let far = match render_tier(adapter, &caps, near.tier, &scene, shift) {
                    Ok(far) => far,
                    Err(e) => {
                        report.fail(format!("{label} {} shift {shift}: {e}", near.tier), &[]);
                        continue;
                    }
                };
                report.check_teleport(&format!("{label}_{}_shift{shift}", near.tier), &far);
                for (i, view) in VIEWS.iter().enumerate() {
                    report.check_identical(
                        &format!("{label}: {} shift {shift} vs origin ({view})", near.tier),
                        (&format!("{label}_{}_{view}", near.tier), &near.images[i]),
                        (
                            &format!("{label}_{}_{view}_shift{shift}", near.tier),
                            &far.images[i],
                        ),
                    );
                }
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

// ---------------------------------------------------------------- cracks

/// Size of the crack renders: a crack is a pixel here and there, so a big image finds them.
const CRACK_SIZE: UVec2 = UVec2::new(1280, 720);
/// Clear color of the crack renders (linear magenta, exactly (255, 0, 255) in sRGB): no terrain
/// pixel has it, so every pixel of it shows the background.
const CRACK_CLEAR: [f32; 4] = [1.0, 0.0, 1.0, 1.0];
/// [`CRACK_CLEAR`] read back (sRGB bytes).
const CRACK_CLEAR_SRGB: [u8; 3] = [255, 0, 255];
/// Largest island of background pixels that counts as a crack (a crack is a pixel or a short
/// sliver; real openings between hills are bigger or touch the sky).
const MAX_CRACK_PIXELS: usize = 8;

/// Where the y of a crack view's eye (and target) is measured from.
#[derive(Clone, Copy)]
enum Base {
    /// The highest column of the meshed area.
    Top,
    /// The top of the ground under the eye (a player standing on it).
    Ground,
}

/// Where a crack view looks.
#[derive(Clone, Copy)]
enum Aim {
    /// At a point, placed like the eye.
    At([f64; 3]),
    /// Yaw and pitch in degrees, as `Transform::from_yaw_pitch` takes them.
    Angles(f32, f32),
}

/// The crack views: an eye, relative to the center of the meshed area in x and z and to its
/// [`Base`] in y, and where it looks. Eye coordinates are multiples of 1/64.
const CRACK_VIEWS: [(&str, [f64; 3], Aim, Base); 6] = [
    // Straight down from above the center: only terrain fills the image.
    (
        "down",
        [0.015_625, 20.0, 0.031_25],
        Aim::At([0.0, -40.0, 0.0]),
        Base::Top,
    ),
    // Down at about 70 degrees near the center, toward +x: terrain only.
    (
        "steep",
        [-20.25, 16.5, 4.75],
        Aim::At([0.0, -48.0, 4.75]),
        Base::Top,
    ),
    // Low and grazing across the surface toward the far edge (the skyline is the edge of the
    // meshed area).
    (
        "across",
        [-94.593_75, 3.406_25, 8.703_125],
        Aim::At([0.0, -20.0, 8.703_125]),
        Base::Top,
    ),
    // Oblique from above one edge.
    (
        "oblique",
        [-20.5, 25.25, -90.75],
        Aim::At([10.0, -20.0, 20.0]),
        Base::Top,
    ),
    // A player's eye (1.6 above the ground) looking 4 degrees down, 43 degrees off the z axis:
    // the x and z axes of the ground look almost parallel on screen, so the growth has to close
    // the cracks across the edges, not along them (a growth of 1/64 of a pixel along each axis
    // left a crack here, and in 20 of 288 such views).
    (
        "standing43",
        [20.078_125, 1.593_75, -19.953_125],
        Aim::Angles(43.0, -4.0),
        Base::Ground,
    ),
    // The same, 13 degrees off the x axis.
    (
        "standing103",
        [-19.921_875, 1.593_75, -19.953_125],
        Aim::Angles(103.0, -4.0),
        Base::Ground,
    ),
];

/// Pixels of the background islands of `image`: 4-connected areas of exactly `clear` (sRGB
/// bytes) of at most [`MAX_CRACK_PIXELS`] that do not touch the image border, so terrain
/// surrounds them. The sky above the horizon is one big area and does not count.
fn crack_pixels(image: &RgbaImage, clear: [u8; 3]) -> Vec<(u32, u32)> {
    let (w, h) = (image.width as usize, image.height as usize);
    let pixels = image.data.as_chunks::<4>().0;
    let is_clear = |i: usize| pixels[i][..3] == clear;
    let mut seen = vec![false; w * h];
    let mut cracks = Vec::new();
    for start in 0..w * h {
        if seen[start] || !is_clear(start) {
            continue;
        }
        seen[start] = true;
        let (mut stack, mut area, mut border) = (vec![start], Vec::new(), false);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            area.push((x as u32, y as u32));
            border |= x == 0 || y == 0 || x == w - 1 || y == h - 1;
            let mut visit = |j: usize| {
                if !seen[j] && is_clear(j) {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        if !border && area.len() <= MAX_CRACK_PIXELS {
            cracks.extend(area);
        }
    }
    cracks
}

/// Renders every crack view of `scene` on one tier at [`CRACK_SIZE`] with the [`CRACK_CLEAR`]
/// background.
fn render_cracks(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    scene: &Scene,
) -> Result<Vec<RgbaImage>, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        chunk_memory: boxel_render::ChunkMemorySettings {
            initial_quads: 1 << 20,
            initial_slots: 1024,
            ..boxel_render::ChunkMemorySettings::default()
        },
        translucent_sort: TranslucentSort::Inline,
    };
    let mut renderer = VoxelRenderer::new(
        &device,
        &queue,
        tier,
        &config,
        &boxel_render::generate_block_textures(scene.registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        CRACK_SIZE,
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    for (pos, mesh) in &scene.meshes {
        renderer
            .upsert_chunk(chunk_id(*pos), pos.origin().as_i64vec3(), Arc::clone(mesh))
            .map_err(|e| format!("upsert {pos:?}: {e}"))?;
    }
    let generator = TerrainGenerator::default();
    let camera = Camera::default();
    let mut images = Vec::new();
    for (_, eye, aim, base) in CRACK_VIEWS {
        let y = match base {
            Base::Top => scene.max_height,
            // The eye's column: the top block's top face is one above its height.
            Base::Ground => generator.height_at(eye[0].floor() as i64, eye[2].floor() as i64) + 1,
        };
        let base = DVec3::new(0.0, f64::from(y), 0.0);
        let eye = base + DVec3::from_array(eye);
        let (yaw, pitch) = match aim {
            Aim::At(target) => {
                let d = (base + DVec3::from_array(target) - eye).as_vec3();
                ((-d.x).atan2(-d.z), d.y.atan2(d.x.hypot(d.z)))
            }
            Aim::Angles(yaw, pitch) => (yaw.to_radians(), pitch.to_radians()),
        };
        let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
        let aspect = CRACK_SIZE.x as f32 / CRACK_SIZE.y as f32;
        let cv = CameraView::compute(&camera, &transform, aspect);
        let params = ViewParams {
            clear_color: CRACK_CLEAR,
            ..ViewParams::from_camera(&cv, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
        };
        let (image, _) = render(&device, &queue, &mut renderer, &params)?;
        images.push(image);
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(images)
}

/// No cracks between faces: greedy quads meet in T-junctions, where the rasterizer could leave
/// single pixels uncovered that show the background through the hollow inside of the terrain
/// (without the edge growth of the shaders: 50 to 55 pixels in the first 4 views on every
/// adapter, about 1 in 50 000 terrain pixels, and 20 in the 2 standing views on the GPUs; growing
/// 1/64 of a pixel along each axis instead of across each edge still left 1 in each standing
/// view).
/// Every tier renders the terrain from [`CRACK_VIEWS`] at 1280x720, which do not see the sides of
/// the meshed area (where the staircase of its edge can enclose a background pixel for real),
/// and must show no island of the background color inside the terrain.
#[test]
fn terrain_has_no_cracks_on_any_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let scene = terrain_scene();
    let mut report = Report::new();
    let save_all = std::env::var_os("GPU_TERRAIN_SAVE").is_some();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let images = match render_cracks(adapter, &caps, tier, &scene) {
                Ok(images) => images,
                Err(e) => {
                    report.fail(format!("{label} {tier} cracks: {e}"), &[]);
                    continue;
                }
            };
            rendered += 1;
            for ((view, ..), image) in CRACK_VIEWS.iter().zip(&images) {
                let cracks = crack_pixels(image, CRACK_CLEAR_SRGB);
                let pixels = image.data.as_chunks::<4>().0;
                let background = pixels.iter().filter(|p| p[..3] == CRACK_CLEAR_SRGB).count();
                let terrain = 1.0 - background as f64 / pixels.len() as f64;
                eprintln!(
                    "gpu_terrain: {label} {tier} cracks {view}: {} pixels, terrain {terrain:.3}",
                    cracks.len()
                );
                let name = format!("{label}_{tier}_cracks_{view}");
                if save_all {
                    report.save(&[(&name, image)]);
                }
                if !cracks.is_empty() {
                    let first: Vec<_> = cracks.iter().take(8).collect();
                    report.fail(
                        format!(
                            "{label} {tier} {view}: {} crack pixels {first:?}",
                            cracks.len()
                        ),
                        &[(&name, image)],
                    );
                }
                // The views see mostly terrain (the down and steep views nothing else).
                let least = if matches!(*view, "down" | "steep") {
                    1.0
                } else {
                    0.4
                };
                if terrain < least {
                    report.fail(
                        format!("{label} {tier} {view}: terrain covers {terrain:.3} < {least}"),
                        &[(&name, image)],
                    );
                }
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

// ---------------------------------------------------------------- lit scenes

/// Chunk columns of the lit scene, in x and z (voxels -64..64).
const LIT_COLUMNS: std::ops::Range<i64> = -2..2;
/// Chunk layers of the lit scene (voxels -32..32; the ground is at y = 0).
const LIT_LAYERS: std::ops::RangeInclusive<i64> = -1..=0;

/// The blocks of the lit scene.
struct LitBlocks {
    grass: BlockState,
    dirt: BlockState,
    stone: BlockState,
    log: BlockState,
    leaves: BlockState,
    /// Red, blue, orange and white lamps.
    lamps: [BlockState; 4],
    lava: BlockState,
}

impl LitBlocks {
    fn new(registry: &BlockRegistry) -> Self {
        let s = |name: &str| registry.by_name(name).unwrap_or_else(|| panic!("{name}"));
        Self {
            grass: s("boxel:grass"),
            dirt: s("boxel:dirt"),
            stone: s("boxel:stone"),
            log: s("boxel:log"),
            leaves: s("boxel:leaves"),
            lamps: [
                s("boxel:lamp_red"),
                s("boxel:lamp_blue"),
                s("boxel:lamp_orange"),
                s("boxel:lamp_white"),
            ],
            lava: s("boxel:lava"),
        }
    }

    /// The scene at local voxel `(x, y, z)`: grass ground at y = 0 over dirt and stone; a tree
    /// at (-40, -40); a wall with an overhanging roof around x = 8..24, z = -48..-24; a low wall
    /// at z = 24..26 beside sunlit ground (AO) with a white lamp and a lava block in front of it;
    /// and under the ground a closed cave lit by a red, a blue and an orange lamp.
    fn at(&self, x: i64, y: i64, z: i64) -> BlockState {
        let air = BlockState::AIR;
        // The cave: closed on every side, deep under the ground.
        if (8..40).contains(&x) && (-24..-14).contains(&y) && (8..40).contains(&z) {
            return match (x, y, z) {
                (12, -24, 12) => self.lamps[0],
                (28, -24, 33) => self.lamps[1],
                (12, -24, 35) => self.lamps[2],
                _ => air,
            };
        }
        match (x, y, z) {
            (-30, 0, 12) => return self.lava,
            (-12, 1, 18) => return self.lamps[3],
            (_, ..-2, _) => return self.stone,
            (_, -2..=-1, _) => return self.dirt,
            (_, 0, _) => return self.grass,
            _ => {}
        }
        // The tree: a trunk to y = 6 and a 5x5 canopy from y = 4 to 7.
        let (tx, tz) = (x + 40, z + 40);
        if tx == 0 && tz == 0 && y <= 6 {
            return self.log;
        }
        if tx.abs() <= 2 && tz.abs() <= 2 && (4..=7).contains(&y) {
            let corner = tx.abs() == 2 && tz.abs() == 2;
            return if corner && y == 7 { air } else { self.leaves };
        }
        // The wall and its roof, open toward -x.
        if (-48..-24).contains(&z) {
            if (20..24).contains(&x) && y < 9 {
                return self.stone;
            }
            if (8..20).contains(&x) && y == 8 {
                return self.stone;
            }
        }
        // A low wall beside sunlit ground.
        if (-40..0).contains(&x) && (24..26).contains(&z) && y < 5 {
            return self.stone;
        }
        air
    }
}

/// `pos` moved by `by` chunks.
fn moved(pos: ChunkPos, by: I64Vec3) -> ChunkPos {
    ChunkPos::new(pos.x + by.x, pos.y + by.y, pos.z + by.z)
}

/// The lit scene as the world would mesh it: data, exact sky tops, light and meshes.
struct LitScene {
    registry: BlockRegistry,
    /// Non-empty meshes: (chunk position, mesh).
    meshes: Vec<(ChunkPos, Arc<ChunkMesh>)>,
}

/// Builds the lit scene moved by `shift` (multiples of 32 on every axis): its chunk data, the
/// sky tops of its columns (the scene is all there is, so a column without blockers is open
/// forever), the light of every chunk with `boxel_light::compute` and its mesh with the default
/// AO policy. Without `light`, every mesh gets open sky and no AO: the look before both.
fn lit_scene(shift: I64Vec3, light: bool) -> LitScene {
    use boxel_light::box_light::{chunk_index, tile_index};
    use boxel_light::{LightInput, LightScratch, PaddedLight, SkyInput, SkyTile};
    use boxel_voxel::{CHUNK_VOLUME, LightTables, LocalPos};

    let registry = boxel_demo::blocks::registry();
    let blocks = LitBlocks::new(&registry);
    let tables = LightTables::from_registry(&registry);
    let chunk_shift = shift / 32;
    let mut data: BTreeMap<ChunkPos, ChunkData> = BTreeMap::new();
    for cy in LIT_LAYERS {
        for cz in LIT_COLUMNS {
            for cx in LIT_COLUMNS {
                let dense: Vec<BlockState> = (0..CHUNK_VOLUME)
                    .map(|i| {
                        let l = LocalPos::from_index(i);
                        blocks.at(
                            cx * 32 + i64::from(l.x()),
                            cy * 32 + i64::from(l.y()),
                            cz * 32 + i64::from(l.z()),
                        )
                    })
                    .collect();
                let pos = moved(ChunkPos::new(cx, cy, cz), chunk_shift);
                data.insert(pos, ChunkData::from_dense(&dense));
            }
        }
    }
    // Sky tops of every chunk column around the scene (outside it: open forever).
    let mut tiles: BTreeMap<(i64, i64), SkyTile> = BTreeMap::new();
    for cz in LIT_COLUMNS.start - 1..=LIT_COLUMNS.end {
        for cx in LIT_COLUMNS.start - 1..=LIT_COLUMNS.end {
            let tops: Box<[i64; 1024]> = Box::new(std::array::from_fn(|i| {
                let (x, z) = (cx * 32 + (i >> 5) as i64, cz * 32 + (i & 31) as i64);
                let inside = LIT_COLUMNS.contains(&cx) && LIT_COLUMNS.contains(&cz);
                let top = (*LIT_LAYERS.start() * 32..(*LIT_LAYERS.end() + 1) * 32)
                    .rev()
                    .find(|&y| inside && tables.blocks_sky(blocks.at(x, y, z)));
                top.map_or(i64::MIN, |y| y + shift.y)
            }));
            tiles.insert(
                (cx + chunk_shift.x, cz + chunk_shift.z),
                SkyTile::from_tops(tops, i64::MIN),
            );
        }
    }

    let materials = MaterialTable::from_registry(&registry);
    let mut mesher = Mesher::new();
    let mut scratch = LightScratch::new();
    let mut padded = PaddedLight::default();
    let mut meshes = Vec::new();
    for (&pos, center) in &data {
        let near = |dx: i64, dy: i64, dz: i64| {
            data.get(&ChunkPos::new(pos.x + dx, pos.y + dy, pos.z + dz))
        };
        if light {
            let input = LightInput {
                origin: pos,
                chunks: std::array::from_fn(|i| {
                    let (dy, rest) = (i as i64 / 9 - 1, i as i64 % 9);
                    let (dx, dz) = (rest / 3 - 1, rest % 3 - 1);
                    debug_assert_eq!(chunk_index(dx as i32, dy as i32, dz as i32), i);
                    near(dx, dy, dz)
                }),
                sky: SkyInput::Tiles(std::array::from_fn(|i| {
                    let (dx, dz) = (i as i64 / 3 - 1, i as i64 % 3 - 1);
                    debug_assert_eq!(tile_index(dx as i32, dz as i32), i);
                    &tiles[&(pos.x + dx, pos.z + dz)]
                })),
                tables: &tables,
            };
            boxel_light::compute(&input, &mut scratch, &mut padded);
        }
        let input = MeshInput {
            center,
            neighbors: std::array::from_fn(|i| data.get(&pos.offset(NEIGHBOR_OFFSETS[i]))),
            light: light.then_some(&padded),
        };
        let options = if light {
            MeshOptions::default()
        } else {
            MeshOptions { ao: AoPolicy::Off }
        };
        let mesh = mesher.mesh_chunk(&input, &materials, options);
        if !mesh.is_empty() {
            meshes.push((pos, Arc::new(mesh)));
        }
    }
    LitScene { registry, meshes }
}

/// One view of the lit scene: an eye looking at a target, at a clock hour.
struct LitView {
    name: &'static str,
    eye: [f64; 3],
    target: [f64; 3],
    /// Clock hour (12 noon, 0 midnight).
    hours: f64,
}

/// The lit views, rendered in this order. Eye coordinates are multiples of 1/64 (exact in `f64`
/// at every shift).
const LIT_VIEWS: [LitView; 9] = [
    LitView {
        name: "tree",
        eye: [-43.75, 2.5, -43.5],
        target: [-40.5, 0.75, -40.5],
        hours: 12.0,
    },
    LitView {
        name: "overhang",
        eye: [1.5, 5.5, -36.25],
        target: [16.0, 0.5, -36.0],
        hours: 12.0,
    },
    LitView {
        name: "wall_ao",
        eye: [-20.25, 3.5, 21.25],
        target: [-20.0, 1.0, 24.0],
        hours: 12.0,
    },
    LitView {
        name: "cave",
        eye: [10.5, -16.25, 23.75],
        target: [26.0, -24.5, 24.0],
        hours: 12.0,
    },
    LitView {
        name: "cave_midnight",
        eye: [10.5, -16.25, 23.75],
        target: [26.0, -24.5, 24.0],
        hours: 0.0,
    },
    LitView {
        name: "overview",
        eye: [-70.5, 30.25, -68.75],
        target: [0.0, 0.0, 0.0],
        hours: 12.0,
    },
    LitView {
        name: "night",
        eye: [-70.5, 30.25, -68.75],
        target: [0.0, 0.0, 0.0],
        hours: 0.0,
    },
    // The -z face of the white lamp at (-12, 1, 18) fills the whole view.
    LitView {
        name: "lamp",
        eye: [-11.5, 1.5, 17.625],
        target: [-11.5, 1.5, 19.0],
        hours: 12.0,
    },
    LitView {
        name: "lamp_midnight",
        eye: [-11.5, 1.5, 17.625],
        target: [-11.5, 1.5, 19.0],
        hours: 0.0,
    },
];

/// The view parameters of `view` moved by `shift`: the scene light and sky color of its hour from
/// the default `DaylightCurve`, as the render plugin computes them.
fn lit_view_params(view: &LitView, shift: I64Vec3) -> ViewParams {
    use boxel_render::SceneLight;
    use boxel_world::{DaylightCurve, TimeOfDay};
    let eye = DVec3::from_array(view.eye);
    let d = (DVec3::from_array(view.target) - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    let daylight = DaylightCurve::default().at(&TimeOfDay::at_clock(view.hours));
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(eye + shift.as_dvec3(), yaw, pitch);
    let cv = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams {
        light: SceneLight {
            sky: daylight.sky_light,
            ambient: daylight.ambient,
            ..SceneLight::noon()
        },
        ..ViewParams::from_camera(&cv, &camera, daylight.sky_color, DistanceFog::default())
    }
}

/// Uploads `scene` (already moved by `shift`) on one tier and renders every lit view.
fn render_lit(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    scene: &LitScene,
    shift: I64Vec3,
) -> Result<Vec<RgbaImage>, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        chunk_memory: boxel_render::ChunkMemorySettings {
            initial_quads: 1 << 20,
            initial_slots: 256,
            ..boxel_render::ChunkMemorySettings::default()
        },
        translucent_sort: TranslucentSort::Inline,
    };
    let mut renderer = VoxelRenderer::new(
        &device,
        &queue,
        tier,
        &config,
        &boxel_render::generate_block_textures(scene.registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    for (pos, mesh) in &scene.meshes {
        renderer
            .upsert_chunk(chunk_id(*pos), pos.origin().as_i64vec3(), Arc::clone(mesh))
            .map_err(|e| format!("upsert {pos:?}: {e}"))?;
    }
    let entries: u32 = scene.meshes.iter().map(|(_, m)| arena_len(m)).sum();
    if renderer.stats().arena_used_quads != entries {
        return Err(format!(
            "stats after upload: {:?}, expected {entries} arena entries",
            renderer.stats()
        ));
    }
    let mut images = Vec::new();
    for view in &LIT_VIEWS {
        let params = lit_view_params(view, shift);
        render(&device, &queue, &mut renderer, &params)?; // warm-up
        let (image, _) = render(&device, &queue, &mut renderer, &params)?;
        images.push(image);
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(images)
}

/// Mean of the RGB channels over the image (0..255).
fn mean_level(image: &RgbaImage) -> f64 {
    let pixels = image.data.as_chunks::<4>().0;
    let sum: u64 = pixels
        .iter()
        .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
        .sum();
    sum as f64 / (3 * pixels.len().max(1)) as f64
}

/// Fraction of pixels where channel `c` is at least 60 and more than twice the other two.
fn dominant_fraction(image: &RgbaImage, c: usize) -> f64 {
    let pixels = image.data.as_chunks::<4>().0;
    let n = pixels
        .iter()
        .filter(|p| {
            let others = (0..3).filter(|&o| o != c).map(|o| u16::from(p[o])).max();
            u16::from(p[c]) >= 60 && others.is_some_and(|o| u16::from(p[c]) > 2 * o)
        })
        .count();
    n as f64 / pixels.len().max(1) as f64
}

/// Light checks on one tier's images (`unlit`: the same views without light and AO).
fn check_light(report: &mut Report, label: &str, lit: &[RgbaImage], unlit: &[RgbaImage]) {
    let image = |name: &str| {
        let i = LIT_VIEWS
            .iter()
            .position(|v| v.name == name)
            .unwrap_or_else(|| panic!("{name}"));
        (i, &lit[i])
    };
    // Block light does not depend on the time of day: the closed cave is the same at midnight.
    let (_, cave) = image("cave");
    let (_, cave_midnight) = image("cave_midnight");
    report.check_identical(
        &format!("{label}: cave at noon vs midnight"),
        (&format!("{label}_lit_cave"), cave),
        (&format!("{label}_lit_cave_midnight"), cave_midnight),
    );
    // The cave is dark except around its lamps, which tint it red and blue.
    let (red, blue) = (dominant_fraction(cave, 0), dominant_fraction(cave, 2));
    eprintln!("gpu_terrain: {label}: cave red {red:.4} blue {blue:.4}");
    if red < 0.01 || blue < 0.01 {
        report.fail(
            format!("{label}: cave lamps: red pixels {red:.4}, blue pixels {blue:.4} (< 0.01)"),
            &[(&format!("{label}_lit_cave"), cave)],
        );
    }
    // Night is much darker than noon.
    let (_, noon) = image("overview");
    let (_, night) = image("night");
    let (day, dark) = (mean_level(noon), mean_level(night));
    eprintln!("gpu_terrain: {label}: overview mean level noon {day:.1} night {dark:.1}");
    if dark > 0.5 * day {
        report.fail(
            format!("{label}: night mean level {dark:.1} vs noon {day:.1}"),
            &[
                (&format!("{label}_lit_overview"), noon),
                (&format!("{label}_lit_night"), night),
            ],
        );
    }
    // Emissive blocks glow: the white lamp's face is drawn at the full brightness of its
    // texture (base color 230, 230, 220 with white speckles; a lit face facing -z would keep
    // 0.65 of it, a mean level near 195), the same at noon, at midnight and without light.
    let (i, lamp) = image("lamp");
    let (_, lamp_midnight) = image("lamp_midnight");
    for (what, other, name) in [
        ("midnight", lamp_midnight, "lit_lamp_midnight"),
        ("without light", &unlit[i], "unlit_lamp"),
    ] {
        report.check_identical(
            &format!("{label}: emissive lamp at noon vs {what}"),
            (&format!("{label}_lit_lamp"), lamp),
            (&format!("{label}_{name}"), other),
        );
    }
    let glow = mean_level(lamp);
    eprintln!("gpu_terrain: {label}: lamp mean level {glow:.1}");
    if glow < 220.0 {
        report.fail(
            format!("{label}: lamp mean level {glow:.1} < 220: not glowing"),
            &[(&format!("{label}_lit_lamp"), lamp)],
        );
    }
    // Shadows: under the tree, the overhang and against the wall (AO) the lit image is darker
    // than the same view without light and AO.
    for name in ["tree", "overhang", "wall_ao"] {
        let (i, lit_image) = image(name);
        let (shaded, open) = (mean_level(lit_image), mean_level(&unlit[i]));
        eprintln!("gpu_terrain: {label}: {name} mean level lit {shaded:.1} unlit {open:.1}");
        if shaded >= open - 1.0 {
            report.fail(
                format!("{label}: {name}: lit mean level {shaded:.1} not below unlit {open:.1}"),
                &[
                    (&format!("{label}_lit_{name}"), lit_image),
                    (&format!("{label}_unlit_{name}"), &unlit[i]),
                ],
            );
        }
    }
}

/// The lit scenes (tree, overhang, wall, cave with lamps, night) render exactly the same pixels
/// on every tier of an adapter and far from the origin, where the light is computed again at the
/// moved position and must give the same meshes.
#[test]
fn lit_scenes_render_identically_on_every_tier_and_far_away() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let start = Instant::now();
    let near = lit_scene(I64Vec3::ZERO, true);
    let unlit = lit_scene(I64Vec3::ZERO, false);
    let far: Vec<(I64Vec3, LitScene)> = FAR_SHIFTS
        .iter()
        .map(|&shift| (shift, lit_scene(shift, true)))
        .collect();
    eprintln!(
        "gpu_terrain: lit scene: {} meshes, {} arena entries ({:.0} ms on the CPU for {} scenes)",
        near.meshes.len(),
        near.meshes.iter().map(|(_, m)| arena_len(m)).sum::<u32>(),
        start.elapsed().as_secs_f64() * 1e3,
        far.len() + 2,
    );
    for (shift, scene) in &far {
        assert_eq!(scene.meshes.len(), near.meshes.len(), "shift {shift}");
        for ((a, ma), (b, mb)) in near.meshes.iter().zip(&scene.meshes) {
            assert_eq!(*b, moved(*a, *shift / 32), "shift {shift}: chunk order");
            assert_eq!(ma, mb, "shift {shift}: the mesh of {a:?} changed");
        }
    }
    // The light shows: palettes with more than open sky, and some dark faces.
    let palettes: usize = near.meshes.iter().map(|(_, m)| m.palette.len()).sum();
    assert!(
        palettes > 2 * near.meshes.len(),
        "{palettes} palette entries"
    );

    let mut report = Report::new();
    let save_all = std::env::var_os("GPU_TERRAIN_SAVE").is_some();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let mut results: Vec<(Tier, Vec<RgbaImage>)> = Vec::new();
        for tier in allowed_tiers(&caps) {
            let images = match render_lit(adapter, &caps, tier, &near, I64Vec3::ZERO) {
                Ok(images) => images,
                Err(e) => {
                    report.fail(format!("{label} {tier} lit: {e}"), &[]);
                    continue;
                }
            };
            for (shift, scene) in &far {
                match render_lit(adapter, &caps, tier, scene, *shift) {
                    Ok(moved) => {
                        for (view, (a, b)) in LIT_VIEWS.iter().zip(images.iter().zip(&moved)) {
                            report.check_identical(
                                &format!("{label}: {tier} lit {} shift {shift}", view.name),
                                (&format!("{label}_{tier}_lit_{}", view.name), a),
                                (&format!("{label}_{tier}_lit_{}_shift{shift}", view.name), b),
                            );
                        }
                    }
                    Err(e) => report.fail(format!("{label} {tier} lit shift {shift}: {e}"), &[]),
                }
            }
            if save_all {
                for (view, image) in LIT_VIEWS.iter().zip(&images) {
                    report.save(&[(&format!("{label}_{tier}_lit_{}", view.name), image)]);
                }
            }
            rendered += 1;
            results.push((tier, images));
        }
        let Some(((best_tier, best), rest)) = results.split_first() else {
            continue;
        };
        // Every tier draws the same pixels. Software rasterizers (llvmpipe) round a few
        // interpolants differently between triangle lists and strips: they get the tolerance of
        // the terrain test.
        let software = caps.info.device_type == wgpu::DeviceType::Cpu;
        for (tier, images) in rest {
            for (view, (a, b)) in LIT_VIEWS.iter().zip(best.iter().zip(images)) {
                let what = format!("{label}: lit {} {tier} vs {best_tier}", view.name);
                let a = (&*format!("{label}_{best_tier}_lit_{}", view.name), a);
                let b = (&*format!("{label}_{tier}_lit_{}", view.name), b);
                if software {
                    report.check_same(&what, a, b);
                } else {
                    report.check_identical(&what, a, b);
                }
            }
        }
        match render_lit(adapter, &caps, *best_tier, &unlit, I64Vec3::ZERO) {
            Ok(unlit_images) => {
                if save_all {
                    for (view, image) in LIT_VIEWS.iter().zip(&unlit_images) {
                        report
                            .save(&[(&format!("{label}_{best_tier}_unlit_{}", view.name), image)]);
                    }
                }
                check_light(
                    &mut report,
                    &format!("{label}_{best_tier}"),
                    best,
                    &unlit_images,
                );
            }
            Err(e) => report.fail(format!("{label} {best_tier} unlit: {e}"), &[]),
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
