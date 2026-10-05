//! Offscreen cross-tier image comparison. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! For every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local
//! capability check allows, a real `VoxelRenderer` draws the same scenes offscreen:
//!
//! - a heightfield (4x2x4 chunks, the reference probe's surface function) with a floating slab
//!   and a canopy of leaves (cutout bucket), meshed in the test with `PackedQuad::new`;
//! - a close-up of the canopy from above (mip 0 alpha test: terrain and the inner side of the
//!   leaves show through the holes);
//! - the same heightfield with one chunk removed (exercises zeroed chunk-table slots);
//! - the heightfield in a second renderer whose arena is exactly full: a chunk that no longer
//!   fits keeps its old mesh, a smaller mesh reuses the chunk's own range;
//! - a far scene: a 32x32 slab ~480 voxels away and a second slab of another texture 1 voxel
//!   behind it (hidden), compared with the front slab alone (depth precision on every backend,
//!   including GL where reverse-Z loses its precision gain).
//!
//! Every tier must match the best tier of its adapter within 0.1 % of the pixels. Failing images
//! are saved under `CARGO_TARGET_TMPDIR/gpu_tiers/` (every image with `GPU_TIERS_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, SmokeContext, SmokeLevel, SmokeTest, Tier, floors,
    gpu_tests_required, read_texture_rgba8, run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, IVec3, UVec2};
use boxel_mesh::{BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, PackedQuad};
use boxel_render::{RendererConfig, RendererError, RendererSmokeTest, ViewParams, VoxelRenderer};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockState, Face, RenderShape};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_TIER_DIFF: f64 = 0.001;
/// Chunk removed in the "minus" variant of the heightfield (holds high terrain and part of the
/// floating slab).
const REMOVED_CHUNK: IVec3 = IVec3::new(0, 1, 0);

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

/// Tiers this adapter can run, from the `[S]` pieces (`classify` is not used here): High =
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

/// A device shaped exactly like the one selection would create for `tier`, with uncaptured
/// errors collected instead of panicking.
fn device_for(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
) -> Result<(wgpu::Device, wgpu::Queue, Arc<Mutex<Vec<String>>>), String> {
    let request = tier_request(tier, &caps.limits);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu_tiers"),
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

/// Blocks of the test world (`BlockRegistry::default_blocks` ids).
struct Blocks {
    stone: BlockState,
    dirt: BlockState,
    grass: BlockState,
    leaves: BlockState,
}

impl Blocks {
    fn new(registry: &BlockRegistry) -> Self {
        let get = |name: &str| registry.by_name(name).expect(name);
        Self {
            stone: get("boxel:stone"),
            dirt: get("boxel:dirt"),
            grass: get("boxel:grass"),
            leaves: get("boxel:leaves"),
        }
    }
}

/// The reference probe's surface, x and z in `-64..64`, y in `0..64`, plus a floating stone slab
/// (y 60..63) and a two-block canopy of leaves (y 46..48) above part of the terrain.
fn block_at(blocks: &Blocks, p: IVec3) -> BlockState {
    if p.y < 0 || p.y >= 64 || p.x < -64 || p.x >= 64 || p.z < -64 || p.z >= 64 {
        return BlockState::AIR;
    }
    if (60..63).contains(&p.y) && (-20..20).contains(&p.x) && (-10..10).contains(&p.z) {
        return blocks.stone;
    }
    if (46..48).contains(&p.y) && (8..40).contains(&p.x) && (-40..-8).contains(&p.z) {
        return blocks.leaves;
    }
    let (x, z) = (p.x as f32, p.z as f32);
    let h =
        (30.0 + 10.0 * (x * 0.09).sin() * (z * 0.07).cos() + 5.0 * ((x + z) * 0.13).sin()) as i32;
    if p.y < h - 4 {
        blocks.stone
    } else if p.y < h - 1 {
        blocks.dirt
    } else if p.y < h {
        blocks.grass
    } else {
        BlockState::AIR
    }
}

/// Greedy mesher for the test (no `boxel_mesh::Mesher`): per face and slice, merges equal
/// (layer, bucket) cells into `w x h` quads. A face is visible unless the neighbor is opaque or
/// it is the same cutout block. Opaque faces go to their face bucket, cutout faces to bucket 6.
fn mesh_chunk(
    registry: &BlockRegistry,
    origin: IVec3,
    block: &dyn Fn(IVec3) -> BlockState,
) -> ChunkMesh {
    let mut buckets: [Vec<PackedQuad>; BUCKET_COUNT] = Default::default();
    let mut mask: Vec<Option<(u16, usize)>> = vec![None; 32 * 32];
    for face in Face::ALL {
        let (n, u, v) = face.axes();
        for d in 0..32 {
            mask.fill(None);
            for j in 0..32 {
                for i in 0..32 {
                    let mut local = [0; 3];
                    (local[n], local[u], local[v]) = (d, i, j);
                    let p = origin + IVec3::from_array(local);
                    let b = block(p);
                    let shape = registry.shape(b);
                    if !matches!(shape, RenderShape::Opaque | RenderShape::Cutout) {
                        continue;
                    }
                    let neighbor = block(p + face.normal());
                    let hidden = registry.shape(neighbor) == RenderShape::Opaque
                        || (shape == RenderShape::Cutout && neighbor == b);
                    if !hidden {
                        let bucket = if shape == RenderShape::Cutout {
                            CUTOUT_BUCKET
                        } else {
                            face.index()
                        };
                        mask[(j * 32 + i) as usize] =
                            Some((registry.face_texture(b, face).0, bucket));
                    }
                }
            }
            for j in 0..32usize {
                let mut i = 0usize;
                while i < 32 {
                    let Some(key) = mask[j * 32 + i] else {
                        i += 1;
                        continue;
                    };
                    let mut w = 1;
                    while i + w < 32 && mask[j * 32 + i + w] == Some(key) {
                        w += 1;
                    }
                    let mut h = 1;
                    while j + h < 32 && (0..w).all(|k| mask[(j + h) * 32 + i + k] == Some(key)) {
                        h += 1;
                    }
                    for jj in j..j + h {
                        mask[jj * 32 + i..jj * 32 + i + w].fill(None);
                    }
                    let mut pos = [0u32; 3];
                    (pos[n], pos[u], pos[v]) = (d as u32, i as u32, j as u32);
                    buckets[key.1].push(PackedQuad::new(pos, w as u32, h as u32, face, key.0));
                    i += w;
                }
            }
        }
    }
    let mut mesh = ChunkMesh::default();
    for (b, quads) in buckets.iter().enumerate() {
        mesh.bucket_counts[b] = quads.len() as u32;
        mesh.quads.extend_from_slice(quads);
    }
    mesh
}

/// Chunk id used by the tests (any stable u64).
fn chunk_id(c: IVec3) -> u64 {
    let pack = |v: i32| u64::from((v + 512) as u32 & 0x3ff);
    pack(c.x) | pack(c.y) << 10 | pack(c.z) << 20
}

/// Non-empty heightfield chunks: (chunk coordinate, mesh).
fn heightfield(registry: &BlockRegistry) -> Vec<(IVec3, ChunkMesh)> {
    let blocks = Blocks::new(registry);
    let block = |p: IVec3| block_at(&blocks, p);
    let mut chunks = Vec::new();
    for cy in 0..2 {
        for cx in -2..2 {
            for cz in -2..2 {
                let c = IVec3::new(cx, cy, cz);
                let mesh = mesh_chunk(registry, c * 32, &block);
                if !mesh.is_empty() {
                    chunks.push((c, mesh));
                }
            }
        }
    }
    chunks
}

/// The far scene in chunk (0, 0, 0): the front slab is the +Z face of voxels `z = 31` (stone),
/// the back slab a 30x30 snow face one voxel behind it (hidden by the front slab).
fn far_scene(registry: &BlockRegistry, front: bool, back: bool) -> ChunkMesh {
    let layer = |name: &str| {
        registry
            .textures()
            .iter()
            .position(|t| t.name == name)
            .expect(name) as u16
    };
    let mut quads = Vec::new();
    if front {
        quads.push(PackedQuad::new(
            [0, 0, 31],
            32,
            32,
            Face::PosZ,
            layer("boxel:stone"),
        ));
    }
    if back {
        quads.push(PackedQuad::new(
            [1, 1, 30],
            30,
            30,
            Face::PosZ,
            layer("boxel:snow"),
        ));
    }
    let mut bucket_counts = [0; BUCKET_COUNT];
    bucket_counts[Face::PosZ.index()] = quads.len() as u32;
    ChunkMesh {
        quads,
        bucket_counts,
    }
}

/// `n` copies of the bottom face of voxel (0, 0, 0): never visible from above the chunk (face
/// bucket culled, back face). Used as filler and as a hidden replacement mesh.
fn hidden_mesh(n: u32) -> ChunkMesh {
    let quad = PackedQuad::new([0, 0, 0], 1, 1, Face::NegY, 0);
    let mut bucket_counts = [0; BUCKET_COUNT];
    bucket_counts[Face::NegY.index()] = n;
    ChunkMesh {
        quads: vec![quad; n as usize],
        bucket_counts,
    }
}

fn view_params(camera: &Camera, transform: &Transform) -> ViewParams {
    let view = CameraView::compute(camera, transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, camera, RendererConfig::default().clear_color, 0.6)
}

/// Looks over the heightfield from above one corner (the slab and the canopy are in view).
fn heightfield_view() -> ViewParams {
    let eye = DVec3::new(-50.3, 70.6, -48.2);
    let d = (DVec3::new(6.0, 28.0, 8.0) - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    view_params(
        &Camera::default(),
        &Transform::from_yaw_pitch(eye, yaw, pitch),
    )
}

/// Straight down onto the canopy from 2.5 voxels: about 50 pixels per voxel, so the leaves
/// texture is magnified (mip 0) and its holes are wider than a pixel.
fn canopy_view() -> ViewParams {
    view_params(
        &Camera::default(),
        &Transform::from_yaw_pitch(DVec3::new(24.5, 50.5, -23.5), 0.0, -89f32.to_radians()),
    )
}

/// 480 voxels in front of the front slab, looking down -Z with a 5 degree field of view so the
/// slab fills a third of the image; near 0.1, view distance 1000.
fn far_view() -> ViewParams {
    let camera = Camera {
        fov_y_radians: 5f32.to_radians(),
        near: 0.1,
        view_distance: 1000.0,
        active: true,
    };
    view_params(
        &camera,
        &Transform::from_yaw_pitch(DVec3::new(16.0, 16.0, 32.0 + 480.0), 0.0, 0.0),
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
            label: Some("gpu_tiers"),
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
    heightfield: RgbaImage,
    canopy: RgbaImage,
    minus_chunk: RgbaImage,
    far_both: RgbaImage,
    far_front: RgbaImage,
    far_back: RgbaImage,
    /// Full arena: after a failed grow of `REMOVED_CHUNK` (expected: `heightfield`), after its
    /// in-place shrink to a hidden quad (`minus_chunk`) and after growing it back
    /// (`heightfield`).
    full_arena: [RgbaImage; 3],
}

/// Renders the heightfield in a renderer whose arena is exactly full (terrain plus out-of-view
/// filler chunks), then re-upserts `REMOVED_CHUNK`: one quad more than it has must fail with
/// `ArenaFull` and change nothing; one hidden quad must succeed in the chunk's own range (no
/// other quad is free); its original mesh must fit there again.
fn full_arena_scenes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tier: Tier,
    registry: &BlockRegistry,
    terrain: &[(IVec3, ChunkMesh)],
) -> Result<[RgbaImage; 3], String> {
    let quads: u32 = terrain.iter().map(|(_, m)| m.total_quads()).sum();
    let largest = terrain.iter().map(|(_, m)| m.total_quads()).max();
    let config = RendererConfig {
        // slack for the size bins of `offset_allocator`, filled below
        arena_quads: quads + largest.unwrap_or(0) / 4 + 8,
        max_chunk_slots: 256,
        ..RendererConfig::default()
    };
    let mut renderer = VoxelRenderer::new(
        device,
        queue,
        tier,
        &config,
        registry.textures(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("full arena: VoxelRenderer::new: {e}"))?;
    for (c, mesh) in terrain {
        renderer
            .upsert_chunk(queue, chunk_id(*c), (*c * 32).as_i64vec3(), mesh)
            .map_err(|e| format!("full arena: upsert {c}: {e}"))?;
    }
    // fillers behind the camera, the largest that fits first, until not one quad is free
    let mut filler_id = 1 << 40;
    loop {
        let stats = renderer.stats();
        let free = stats.arena_capacity_quads - stats.arena_used_quads;
        let fits = (1..=free).rev().any(|n| {
            renderer
                .upsert_chunk(queue, filler_id, I64Vec3::splat(-2048), &hidden_mesh(n))
                .is_ok()
        });
        if !fits {
            break;
        }
        filler_id += 1;
    }
    let full = renderer.stats();
    if full.arena_used_quads != full.arena_capacity_quads {
        return Err(format!("full arena: not full after the fillers: {full:?}"));
    }

    let (_, original) = terrain
        .iter()
        .find(|(c, _)| *c == REMOVED_CHUNK)
        .ok_or("full arena: no REMOVED_CHUNK")?;
    let (id, origin) = (chunk_id(REMOVED_CHUNK), (REMOVED_CHUNK * 32).as_i64vec3());
    let view = heightfield_view();
    let grown = hidden_mesh(original.total_quads() + 1);
    match renderer.upsert_chunk(queue, id, origin, &grown) {
        Err(RendererError::ArenaFull { .. }) => {}
        other => return Err(format!("full arena: growing a chunk returned {other:?}")),
    }
    let after = renderer.stats();
    if (after.chunks, after.arena_used_quads, after.dropped_chunks)
        != (full.chunks, full.arena_used_quads, full.dropped_chunks + 1)
    {
        return Err(format!(
            "full arena: failed grow changed {full:?} into {after:?}"
        ));
    }
    let (kept, _) = render(device, queue, &mut renderer, &view)?;

    renderer
        .upsert_chunk(queue, id, origin, &hidden_mesh(1))
        .map_err(|e| format!("full arena: shrinking a chunk: {e}"))?;
    let (shrunk, _) = render(device, queue, &mut renderer, &view)?;
    renderer
        .upsert_chunk(queue, id, origin, original)
        .map_err(|e| format!("full arena: growing a chunk back: {e}"))?;
    let (regrown, _) = render(device, queue, &mut renderer, &view)?;
    let end = renderer.stats();
    if (end.chunks, end.arena_used_quads) != (full.chunks, full.arena_used_quads) {
        return Err(format!(
            "full arena: in-place replaces changed {full:?} into {end:?}"
        ));
    }
    Ok([kept, shrunk, regrown])
}

/// Uploads and renders every scene on one tier.
fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    registry: &BlockRegistry,
    terrain: &[(IVec3, ChunkMesh)],
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        arena_quads: 1 << 20,
        max_chunk_slots: 256,
        ..RendererConfig::default()
    };
    let mut renderer = VoxelRenderer::new(
        &device,
        &queue,
        tier,
        &config,
        registry.textures(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    if renderer.tier() != tier {
        return Err(format!("renderer reports tier {}", renderer.tier()));
    }

    for (c, mesh) in terrain {
        renderer
            .upsert_chunk(&queue, chunk_id(*c), (*c * 32).as_i64vec3(), mesh)
            .map_err(|e| format!("upsert {c}: {e}"))?;
    }
    let quads: u32 = terrain.iter().map(|(_, m)| m.total_quads()).sum();
    let stats = renderer.stats();
    if stats.chunks as usize != terrain.len() || stats.arena_used_quads != quads {
        return Err(format!(
            "stats after upload: {stats:?}, expected {quads} quads"
        ));
    }
    let view = heightfield_view();
    render(&device, &queue, &mut renderer, &view)?; // warm-up
    let (heightfield, cpu) = render(&device, &queue, &mut renderer, &view)?;
    let stats = renderer.stats();
    eprintln!(
        "gpu_tiers: {} {tier}: render {:.3} ms cpu, chunks={} quads={} draws={} visible={} \
         non_background={:.3}",
        adapter_label(caps),
        cpu.as_secs_f64() * 1e3,
        stats.chunks,
        stats.arena_used_quads,
        stats.draws,
        stats.visible_chunks,
        heightfield.non_background_fraction(),
    );

    let (canopy, _) = render(&device, &queue, &mut renderer, &canopy_view())?;

    if !renderer.remove_chunk(&queue, chunk_id(REMOVED_CHUNK)) {
        return Err(format!("chunk {REMOVED_CHUNK} was not resident"));
    }
    let (minus_chunk, _) = render(&device, &queue, &mut renderer, &view)?;
    let full_arena = full_arena_scenes(&device, &queue, tier, registry, terrain)?;

    renderer.clear_chunks(&queue);
    if renderer.chunk_count() != 0 || renderer.stats().arena_used_quads != 0 {
        return Err(format!("clear_chunks left {:?}", renderer.stats()));
    }
    let far = far_view();
    renderer
        .upsert_chunk(&queue, 7, I64Vec3::ZERO, &far_scene(registry, true, true))
        .map_err(|e| e.to_string())?;
    let (far_both, _) = render(&device, &queue, &mut renderer, &far)?;
    // replace the same chunk id with one slab at a time
    renderer
        .upsert_chunk(&queue, 7, I64Vec3::ZERO, &far_scene(registry, true, false))
        .map_err(|e| e.to_string())?;
    let (far_front, _) = render(&device, &queue, &mut renderer, &far)?;
    renderer
        .upsert_chunk(&queue, 7, I64Vec3::ZERO, &far_scene(registry, false, true))
        .map_err(|e| e.to_string())?;
    let (far_back, _) = render(&device, &queue, &mut renderer, &far)?;

    // an empty mesh removes the chunk: nothing but sky is left
    renderer
        .upsert_chunk(&queue, 7, I64Vec3::ZERO, &ChunkMesh::default())
        .map_err(|e| e.to_string())?;
    let (empty, _) = render(&device, &queue, &mut renderer, &far)?;
    if renderer.chunk_count() != 0 || empty.non_background_fraction() != 0.0 {
        return Err("an empty mesh did not remove the chunk".to_string());
    }

    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(TierImages {
        tier,
        heightfield,
        canopy,
        minus_chunk,
        far_both,
        far_front,
        far_back,
        full_arena,
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
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_tiers"),
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_tiers: FAIL {message}");
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
                Ok(()) => eprintln!("gpu_tiers: saved {}", path.display()),
                Err(e) => eprintln!("gpu_tiers: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn check_range(&mut self, what: &str, image: &RgbaImage, range: std::ops::RangeInclusive<f64>) {
        let f = image.non_background_fraction();
        if !range.contains(&f) {
            self.fail(
                format!("{what}: non-background fraction {f:.4} outside {range:?}"),
                &[(what, image)],
            );
        }
    }

    fn check_differs(
        &mut self,
        what: &str,
        a: (&str, &RgbaImage),
        b: (&str, &RgbaImage),
        min_fraction: f64,
    ) {
        let fraction = a.1.compare(b.1, 2).fraction();
        if fraction < min_fraction {
            self.fail(
                format!(
                    "{what}: only {fraction:.4} of the pixels changed (expected >= {min_fraction})"
                ),
                &[a, b],
            );
        }
    }

    fn check_same(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        let diff = a.1.compare(b.1, 2);
        eprintln!(
            "gpu_tiers: {what}: {:.4} % of pixels differ, max channel {}",
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
                &[(a.0, a.1), (b.0, b.1)],
            );
        }
    }
}

#[test]
fn tiers_render_identically() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let registry = BlockRegistry::default_blocks();
    let terrain = heightfield(&registry);
    assert!(terrain.len() >= 20, "{} terrain chunks", terrain.len());
    assert!(
        terrain
            .iter()
            .any(|(_, m)| m.bucket_counts[CUTOUT_BUCKET] > 0),
        "the canopy must produce cutout quads"
    );
    assert!(terrain.iter().any(|(c, _)| *c == REMOVED_CHUNK));
    let mut report = Report::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let tiers = allowed_tiers(&caps);
        eprintln!("gpu_tiers: {label}: tiers {tiers:?}");
        let mut images = Vec::new();
        for &tier in &tiers {
            match render_tier(adapter, &caps, tier, &registry, &terrain) {
                Ok(i) => images.push(i),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        for i in &images {
            rendered += 1;
            let name = |scene: &str| format!("{label}_{}_{scene}", i.tier);
            if std::env::var_os("GPU_TIERS_SAVE").is_some() {
                report.save(&[
                    (&name("heightfield"), &i.heightfield),
                    (&name("canopy"), &i.canopy),
                    (&name("minus_chunk"), &i.minus_chunk),
                    (&name("far_both"), &i.far_both),
                    (&name("far_front"), &i.far_front),
                    (&name("far_back"), &i.far_back),
                    (&name("full_arena_kept"), &i.full_arena[0]),
                    (&name("full_arena_shrunk"), &i.full_arena[1]),
                    (&name("full_arena_regrown"), &i.full_arena[2]),
                ]);
            }
            report.check_range(&name("heightfield"), &i.heightfield, 0.05..=0.95);
            report.check_range(&name("far_front"), &i.far_front, 0.1..=0.9);
            report.check_same(
                &format!("{label} {}: far scene, both slabs vs front slab", i.tier),
                (&name("far_both"), &i.far_both),
                (&name("far_front"), &i.far_front),
            );
            // controls: the hidden slab is drawn when nothing covers it, and removing a chunk
            // really changes the picture
            report.check_differs(
                &format!("{label} {}: far scene, back slab alone", i.tier),
                (&name("far_front"), &i.far_front),
                (&name("far_back"), &i.far_back),
                0.1,
            );
            report.check_differs(
                &format!("{label} {}: heightfield minus one chunk", i.tier),
                (&name("heightfield"), &i.heightfield),
                (&name("minus_chunk"), &i.minus_chunk),
                0.002,
            );
            for (scene, image, expected, expected_name) in [
                (
                    "full_arena_kept",
                    &i.full_arena[0],
                    &i.heightfield,
                    "heightfield",
                ),
                (
                    "full_arena_shrunk",
                    &i.full_arena[1],
                    &i.minus_chunk,
                    "minus_chunk",
                ),
                (
                    "full_arena_regrown",
                    &i.full_arena[2],
                    &i.heightfield,
                    "heightfield",
                ),
            ] {
                report.check_same(
                    &format!("{label} {}: {scene} vs {expected_name}", i.tier),
                    (&name(scene), image),
                    (&name(expected_name), expected),
                );
            }
        }
        let Some((best, rest)) = images.split_first() else {
            continue;
        };
        for i in rest {
            for (scene, a, b) in [
                ("heightfield", &best.heightfield, &i.heightfield),
                ("canopy", &best.canopy, &i.canopy),
                ("minus_chunk", &best.minus_chunk, &i.minus_chunk),
                ("far_both", &best.far_both, &i.far_both),
            ] {
                report.check_same(
                    &format!("{label}: {} vs {} ({scene})", i.tier, best.tier),
                    (&format!("{label}_{}_{scene}", best.tier), a),
                    (&format!("{label}_{}_{scene}", i.tier), b),
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

#[test]
fn renderer_smoke_test_passes_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let mut failures = Vec::new();
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let (device, queue, errors) = match device_for(adapter, &caps, tier) {
                Ok(d) => d,
                Err(e) => {
                    failures.push(format!("{label} {tier}: {e}"));
                    continue;
                }
            };
            for level in [SmokeLevel::Compile, SmokeLevel::Draw] {
                for output_format in [
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                    wgpu::TextureFormat::Bgra8Unorm,
                ] {
                    let ctx = SmokeContext {
                        device: &device,
                        queue: &queue,
                        tier,
                        caps: &caps,
                        level,
                        output_format,
                    };
                    let start = Instant::now();
                    let result = RendererSmokeTest.run(&ctx);
                    eprintln!(
                        "gpu_tiers: smoke {label} {tier} {level:?} {output_format:?}: {} ({:.1} ms)",
                        if result.is_ok() { "ok" } else { "FAILED" },
                        start.elapsed().as_secs_f64() * 1e3
                    );
                    if let Err(e) = result {
                        failures.push(format!("{label} {tier} {level:?} {output_format:?}: {e}"));
                    }
                }
            }
            let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
            if !errors.is_empty() {
                failures.push(format!("{label} {tier}: uncaptured errors {errors:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
