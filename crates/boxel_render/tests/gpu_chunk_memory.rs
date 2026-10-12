//! Chunk memory in pages (ADR 0035) on every tier. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! For every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local
//! capability check allows (the one of `gpu_tiers.rs`), real `VoxelRenderer`s are fed the same
//! chunk ops through `VoxelRenderer::apply_ops`, as the render plugin feeds them:
//!
//! - a sparse scene (non-empty chunks only at even chunk coordinates on all three axes, so no two
//!   chunks share a face, an edge or a corner) of stone, leaves (cutout) and water (translucent),
//!   with frames of churn (remeshes that grow, shrink and gain or lose water, removals, chunks
//!   back under new ids), drawn by a renderer with one page and by one with many tiny pages of
//!   at most 4 chunks: every frame is the same image, exactly, on that tier. Water fills its
//!   Translucent page and spills into Solid ones, so the translucent pass switches pages. Also
//!   with the scene and the camera at `2^31 - 64`, `2^40` and near the world limit (ADR 0014);
//! - the same pages read back: every live chunk's range holds its quads and its palette, and on
//!   Low its light page each quad's light entry;
//! - a default 4 Mi page filled past quad 65,535: those ranges read back exact;
//! - High's draw lists past 65,535 draws: about 22,500 chunks of six visible face buckets each
//!   (and 2,000 with 64 chunks a page). The draws the cull wrote equal the CPU count, nothing is
//!   dropped, and High draws what Mid draws (0.1 % of the pixels, channel tolerance 2);
//! - the default floor's chunk places cover its 32,768 slots, and a page full of chunks with
//!   quads to spare refuses with `RendererError::PageChunksFull`, shown in the stats.
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_chunk_memory/`.
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, IVec3, UVec2};
use boxel_mesh::{
    BUCKET_COUNT, ChunkMesh, FULL_SKY_ENTRY, MaterialTable, MeshInput, MeshOptions, Mesher,
    PackedQuad, PaletteEntry,
};
use boxel_render::{
    ChunkMemorySettings, ChunkOp, DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, Resync,
    TranslucentSort, ViewParams, VoxelRenderer,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, Face, LocalPos, MAX_BLOCK_COORD};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_TIER_DIFF: f64 = 0.001;
/// Where the scene and the camera also go (multiples of 32): straddling `+2^31`, `2^40` away and
/// across the wrap again, and next to the world limit.
const SHIFTS: [I64Vec3; 4] = [
    I64Vec3::ZERO,
    I64Vec3::new((1 << 31) - 64, 0, -(1 << 31) + 64),
    I64Vec3::new((1 << 40) + (1 << 31) - 64, -(1 << 31), -(1 << 40)),
    I64Vec3::new(MAX_BLOCK_COORD - 512, 0, -(MAX_BLOCK_COORD - 512)),
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
        label: Some("gpu_chunk_memory"),
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

/// The uncaptured errors so far, as an error.
fn no_errors(errors: &Mutex<Vec<String>>) -> Result<(), String> {
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("uncaptured errors: {errors:?}"))
    }
}

// ---------------------------------------------------------------- the sparse scene

/// The chunk cells of the sparse scene, in chunk units: even coordinates only, so no two chunks
/// touch (not even at a corner) and their draw order never reaches a pixel.
fn cells() -> Vec<IVec3> {
    let mut cells = Vec::new();
    for y in [0, 2] {
        for z in [0, 2, 4] {
            for x in [0, 2, 4] {
                cells.push(IVec3::new(x, y, z));
            }
        }
    }
    cells
}

/// Every block of the box `min..max` (exclusive, clipped to the chunk) as `name`.
fn cuboid(min: [u32; 3], max: [u32; 3], name: &'static str) -> Vec<([u32; 3], &'static str)> {
    let mut out = Vec::new();
    for x in min[0]..max[0].min(32) {
        for y in min[1]..max[1].min(32) {
            for z in min[2]..max[2].min(32) {
                out.push(([x, y, z], name));
            }
        }
    }
    out
}

/// The blocks of `cell`'s chunk at `version`: a stone cuboid, a cuboid of leaves (cutout) and,
/// in two chunks of three, a pool of water (translucent), sized by both.
fn chunk_blocks(cell: IVec3, version: u32) -> Vec<([u32; 3], &'static str)> {
    let mut h = (cell.x as u32 * 73_856_093)
        ^ (cell.y as u32 * 19_349_663)
        ^ (cell.z as u32 * 83_492_791)
        ^ version.wrapping_mul(2_654_435_761);
    let mut next = |n: u32| {
        h ^= h << 13;
        h ^= h >> 17;
        h ^= h << 5;
        h % n
    };
    let mut blocks = Vec::new();
    let s = [1 + next(4), 1 + next(3), 1 + next(4)];
    blocks.extend(cuboid(
        s,
        [
            s[0] + 10 + next(14),
            s[1] + 6 + next(12),
            s[2] + 10 + next(14),
        ],
        "boxel:stone",
    ));
    let l = [14 + next(6), 8 + next(8), 2 + next(8)];
    blocks.extend(cuboid(
        l,
        [l[0] + 4 + next(10), l[1] + 4 + next(8), l[2] + 4 + next(14)],
        "boxel:leaves",
    ));
    if next(3) != 0 {
        let w = [2 + next(6), 22 + next(4), 14 + next(6)];
        blocks.extend(cuboid(
            w,
            [w[0] + 6 + next(14), w[1] + 2 + next(5), w[2] + 6 + next(12)],
            "boxel:water",
        ));
    }
    blocks
}

/// The real mesher's mesh of `blocks` in a chunk, air around it, no light.
fn mesh_blocks(registry: &BlockRegistry, blocks: &[([u32; 3], &str)]) -> ChunkMesh {
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for &([x, y, z], name) in blocks {
        dense[LocalPos::new(x, y, z).index()] = registry.by_name(name).expect(name);
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

/// A camera at `eye` looking at `target`, both near the origin, then moved with the scene by
/// `shift` (the render origin is an `i64`, so the view is exact at any distance).
fn look(eye: DVec3, target: DVec3, shift: I64Vec3) -> ViewParams {
    let d = (target - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    let mut view =
        ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default());
    view.render_origin += shift;
    view
}

/// The sparse scene's meshes by cell and version.
type Meshes = std::collections::HashMap<(IVec3, u32), Arc<ChunkMesh>>;

/// Every mesh [`sparse_frames`] uses, meshed once.
fn sparse_meshes(registry: &BlockRegistry) -> Meshes {
    let mut meshes = Meshes::new();
    for cell in cells() {
        for version in 0..4 {
            let mesh = mesh_blocks(registry, &chunk_blocks(cell, version));
            meshes.insert((cell, version), Arc::new(mesh));
        }
    }
    meshes
}

/// One frame's ops and whether it is a resync (every chunk again, after a clear).
struct Frame {
    ops: Vec<ChunkOp>,
    resync: bool,
}

/// The frames of the sparse scene at `shift`: everything, then churn (remeshes that grow,
/// shrink, gain or lose water; removals; chunks back under new ids), then a resync of what is
/// left, as after a frame that lost its uploads.
fn sparse_frames(meshes: &Meshes, shift: I64Vec3) -> Vec<Frame> {
    let cells = cells();
    let origin = |cell: IVec3| shift + cell.as_i64vec3() * 32;
    let mesh = |cell: IVec3, version: u32| Arc::clone(&meshes[&(cell, version)]);
    let upsert = |id: u64, cell: IVec3, version: u32| ChunkOp::Upsert {
        id,
        origin: origin(cell),
        mesh: mesh(cell, version),
    };
    // id -> (cell, version) of what is live
    let mut live: Vec<(u64, IVec3, u32)> = cells
        .iter()
        .enumerate()
        .map(|(i, &c)| (i as u64, c, 0))
        .collect();
    let mut frames = vec![Frame {
        ops: live.iter().map(|&(id, c, v)| upsert(id, c, v)).collect(),
        resync: true,
    }];
    // churn: remeshes, removals
    let mut ops = Vec::new();
    for entry in live.iter_mut().step_by(3) {
        entry.2 = 1;
        ops.push(upsert(entry.0, entry.1, 1));
    }
    for id in [4, 9, 13] {
        ops.push(ChunkOp::Remove { id });
    }
    live.retain(|(id, ..)| ![4, 9, 13].contains(id));
    frames.push(Frame { ops, resync: false });
    // back under new ids, more remeshes (water comes and goes)
    let mut ops = Vec::new();
    for (new, cell) in [(100, cells[4]), (101, cells[9])] {
        ops.push(upsert(new, cell, 2));
        live.push((new, cell, 2));
    }
    for entry in live.iter_mut().skip(1).step_by(4) {
        entry.2 = 3;
        ops.push(upsert(entry.0, entry.1, 3));
    }
    frames.push(Frame { ops, resync: false });
    // a resync of what is live
    frames.push(Frame {
        ops: live.iter().map(|&(id, c, v)| upsert(id, c, v)).collect(),
        resync: true,
    });
    frames
}

/// The chunk memory of the reference: one page.
fn one_page() -> ChunkMemorySettings {
    ChunkMemorySettings {
        initial_quads: 1 << 18,
        initial_slots: 64,
        grow: false,
        ..ChunkMemorySettings::default()
    }
}

/// Twelve tiny pages of at most 4 chunks (one of them Translucent).
fn tiny_pages() -> ChunkMemorySettings {
    ChunkMemorySettings {
        initial_quads: 12 * 4096,
        initial_slots: 64,
        page_quads: 4096,
        page_chunks: 4,
        grow: false,
        ..ChunkMemorySettings::default()
    }
}

fn renderer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tier: Tier,
    chunk_memory: ChunkMemorySettings,
    sort: TranslucentSort,
) -> Result<VoxelRenderer, String> {
    let registry = boxel_demo::blocks::registry();
    VoxelRenderer::new(
        device,
        queue,
        tier,
        &RendererConfig {
            chunk_memory,
            translucent_sort: sort,
        },
        &boxel_render::generate_block_textures(registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))
}

/// One frame as the render plugin runs it: the ops, then the render, submitted; the main
/// color read back.
fn frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    ops: Vec<ChunkOp>,
    resync: bool,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    let resync = if resync { Resync::Chunks } else { Resync::None };
    let refused = renderer.apply_ops(device, ops, resync);
    if let Some(e) = refused.first() {
        return Err(format!("{} refusal(s), first: {e}", refused.len()));
    }
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_chunk_memory"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// The bytes a chunk's range holds: its quads (the translucent bucket in the mesher's order),
/// then its palette (one open-sky entry without one); with `light`, what its Low light page
/// holds instead: each quad's palette entry (open sky past the palette), then the palette.
fn range_bytes(mesh: &ChunkMesh, light: bool) -> Vec<u8> {
    let palette: Vec<PaletteEntry> = if mesh.palette.is_empty() {
        vec![FULL_SKY_ENTRY]
    } else {
        mesh.palette.clone()
    };
    let mut out: Vec<u8> = if light {
        let per_quad: Vec<PaletteEntry> = mesh
            .quads
            .iter()
            .map(|q| {
                palette
                    .get(usize::from(q.light_index()))
                    .copied()
                    .unwrap_or(FULL_SKY_ENTRY)
            })
            .collect();
        bytemuck::cast_slice(&per_quad).to_vec()
    } else {
        bytemuck::cast_slice(&mesh.quads).to_vec()
    };
    out.extend_from_slice(bytemuck::cast_slice(&palette));
    out
}

/// Every live chunk of `ops`'s last state reads back as its mesh (and on Low its light).
fn check_ranges(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &VoxelRenderer,
    tier: Tier,
    live: &[(u64, Arc<ChunkMesh>)],
) -> Result<(), String> {
    for (id, mesh) in live {
        let (page, offset, size) = renderer
            .debug_chunk_range(*id)
            .ok_or(format!("chunk {id} not resident"))?;
        for light in [false, tier == Tier::Low] {
            let got =
                renderer.debug_read_page(device, queue, page, light, offset..offset + size)?;
            if got != range_bytes(mesh, light) {
                return Err(format!(
                    "chunk {id} (page {page}, quads {offset}..+{size}, light {light}) does not \
                     read back as its mesh"
                ));
            }
        }
    }
    Ok(())
}

/// The live meshes after `frames`, by id.
fn live_meshes(frames: &[Frame]) -> Vec<(u64, Arc<ChunkMesh>)> {
    let mut live: Vec<(u64, Arc<ChunkMesh>)> = Vec::new();
    for f in frames {
        if f.resync {
            live.clear();
        }
        for op in &f.ops {
            match op {
                ChunkOp::Upsert { id, mesh, .. } => {
                    live.retain(|(i, _)| i != id);
                    live.push((*id, Arc::clone(mesh)));
                }
                ChunkOp::Remove { id } => live.retain(|(i, _)| i != id),
            }
        }
    }
    live
}

// ---------------------------------------------------------------- report

/// Collects failures and saves the images involved.
struct Report {
    failures: Vec<String>,
    dir: PathBuf,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_chunk_memory"),
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_chunk_memory: FAIL {message}");
        let _ = std::fs::create_dir_all(&self.dir);
        for (name, image) in images {
            let file: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let path = self.dir.join(format!("{file}.png"));
            match image.save_png(&path) {
                Ok(()) => eprintln!("gpu_chunk_memory: saved {}", path.display()),
                Err(e) => eprintln!("gpu_chunk_memory: cannot save {}: {e}", path.display()),
            }
        }
        self.failures.push(message);
    }

    fn finish(self, rendered: usize) {
        assert!(
            rendered > 0 || !gpu_tests_required(),
            "BOXEL_REQUIRE_GPU=1 but no tier could render"
        );
        assert!(
            self.failures.is_empty(),
            "{} failure(s):\n{}",
            self.failures.len(),
            self.failures.join("\n")
        );
    }
}

/// Runs `test` on every tier of every adapter; `None` without an adapter (skipped).
fn each_tier(
    name: &str,
    mut test: impl FnMut(&wgpu::Adapter, &AdapterCaps, Tier, &mut Report) -> Result<(), String>,
) {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("{name}: skipping, no adapter");
        return;
    }
    let mut report = Report::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            eprintln!("{name}: {label} {tier}");
            match test(adapter, &caps, tier, &mut report) {
                Ok(()) => rendered += 1,
                Err(e) => report.fail(format!("{name}: {label} {tier}: {e}"), &[]),
            }
        }
    }
    report.finish(rendered);
}

// ---------------------------------------------------------------- tests

/// Many tiny pages draw exactly what one page draws, every frame, on every tier and far from
/// the origin; their ranges read back as their meshes (on Low their light pages too).
#[test]
fn pages_draw_exactly_what_one_page_draws() {
    let registry = boxel_demo::blocks::registry();
    let meshes = sparse_meshes(&registry);
    let eye = DVec3::new(-6.0, 104.0, -12.0);
    let target = DVec3::new(72.0, 28.0, 78.0);
    each_tier("pages", |adapter, caps, tier, report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let label = format!("{} {tier}", adapter_label(caps));
        let mut switches = 0;
        for sort in [TranslucentSort::Inline, TranslucentSort::Off] {
            for shift in SHIFTS {
                let frames = sparse_frames(&meshes, shift);
                let view = look(eye, target, shift);
                let mut reference = renderer(&device, &queue, tier, one_page(), sort)?;
                let mut paged = renderer(&device, &queue, tier, tiny_pages(), sort)?;
                assert_eq!(reference.stats().chunk_memory.pages, 1);
                assert_eq!(paged.stats().chunk_memory.pages, 12);
                assert_eq!(paged.stats().chunk_memory.translucent_pages, 1);
                for (k, f) in frames.iter().enumerate() {
                    let a = frame(
                        &device,
                        &queue,
                        &mut reference,
                        f.ops.clone(),
                        f.resync,
                        &view,
                    )?;
                    let b = frame(&device, &queue, &mut paged, f.ops.clone(), f.resync, &view)?;
                    let what = format!("{label} {sort:?} shift {shift} frame {k}");
                    if a.non_background_fraction() < 0.03 {
                        report.fail(
                            format!("{what}: the scene is not in view"),
                            &[("scene", &a)],
                        );
                    }
                    let diff = a.compare(&b, 0);
                    if diff.differing_pixels > 0 {
                        report.fail(
                            format!("{what}: {} pixels differ", diff.differing_pixels),
                            &[
                                (&format!("{what} one page"), &a),
                                (&format!("{what} pages"), &b),
                            ],
                        );
                    }
                    let stats = paged.stats();
                    if stats.chunks != reference.stats().chunks || stats.dropped_chunks != 0 {
                        return Err(format!("{what}: chunks or refusals differ"));
                    }
                    switches = switches.max(stats.chunk_memory.translucent_page_switches);
                }
                if sort == TranslucentSort::Off {
                    check_ranges(&device, &queue, &paged, tier, &live_meshes(&frames))
                        .map_err(|e| format!("shift {shift}: {e}"))?;
                }
            }
        }
        if switches == 0 {
            return Err("the water never spread over two pages".to_string());
        }
        no_errors(&errors)
    });
}

/// A default page of 4 Mi quads filled past quad 65,535: those ranges read back exact (an
/// allocator that kept `u16` offsets would cut them).
#[test]
fn offsets_past_u16_read_back_exact() {
    each_tier("u16", |adapter, caps, tier, _report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let memory = ChunkMemorySettings {
            initial_quads: 4 << 20,
            initial_slots: 128,
            grow: false,
            ..ChunkMemorySettings::default()
        };
        let mut renderer = renderer(&device, &queue, tier, memory, TranslucentSort::Off)?;
        let mut live = Vec::new();
        let mut ops = Vec::new();
        for id in 0..100u64 {
            let quads: Vec<PackedQuad> = (0..999u32)
                .map(|q| PackedQuad {
                    geo: (id as u32) << 16 | q,
                    mat: q.wrapping_mul(2_654_435_761) & 0x000F_FFFF,
                })
                .collect();
            let mut counts = [0; BUCKET_COUNT];
            counts[Face::PosY.index()] = 999;
            let mesh = Arc::new(ChunkMesh::from_quads(quads, counts));
            live.push((id, Arc::clone(&mesh)));
            ops.push(ChunkOp::Upsert {
                id,
                origin: I64Vec3::new(id as i64 * 64, 0, 0),
                mesh,
            });
        }
        let view = look(
            DVec3::new(0.0, 100.0, 0.0),
            DVec3::new(50.0, 0.0, 50.0),
            I64Vec3::ZERO,
        );
        frame(&device, &queue, &mut renderer, ops, true, &view)?;
        let past = live
            .iter()
            .filter_map(|(id, _)| renderer.debug_chunk_range(*id))
            .filter(|&(_, offset, _)| offset > u32::from(u16::MAX))
            .count();
        if past < 30 {
            return Err(format!("only {past} chunks past quad 65,535"));
        }
        check_ranges(&device, &queue, &renderer, tier, &live)?;
        no_errors(&errors)
    });
}

/// A 16-voxel cube in the middle of a chunk: one quad in each of the 6 face buckets.
fn cube_mesh() -> ChunkMesh {
    let mut by_bucket: Vec<(usize, PackedQuad)> = Face::ALL
        .into_iter()
        .map(|face| {
            let n = face.normal();
            // the quad's min corner; positive faces lie at pos + 1 along their axis
            let pos = [
                if n.x > 0 { 23 } else { 8 },
                if n.y > 0 { 23 } else { 8 },
                if n.z > 0 { 23 } else { 8 },
            ];
            (face.index(), PackedQuad::new(pos, 16, 16, face, 1))
        })
        .collect();
    by_bucket.sort_by_key(|(bucket, _)| *bucket);
    let mut counts = [0; BUCKET_COUNT];
    for (bucket, _) in &by_bucket {
        counts[*bucket] += 1;
    }
    ChunkMesh::from_quads(by_bucket.into_iter().map(|(_, q)| q).collect(), counts)
}

/// High's draw lists past 65,535 draws: a flat grid of `side * side` cubes seen from straight
/// above, every one in view with about three face buckets visible. The draws High's cull wrote
/// equal the CPU count (nothing dropped) and High draws what Mid draws.
fn lists_past_one_list(side: i64, memory: ChunkMemorySettings, min_draws: u32) {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("lists: skipping, no adapter");
        return;
    }
    let mesh = Arc::new(cube_mesh());
    let origins: Vec<I64Vec3> = (0..side)
        .flat_map(|z| (0..side).map(move |x| I64Vec3::new(32 * x, 0, 32 * z)))
        .collect();
    let center = 16.0 * side as f64;
    let camera = Camera {
        view_distance: 1e5,
        ..Camera::default()
    };
    let height = 1.2 * center / (camera.fov_y_radians as f64 / 2.0).tan();
    let transform = Transform::from_yaw_pitch(
        DVec3::new(center + 0.5, height, center + 0.5),
        0.0,
        -89.9f32.to_radians(),
    );
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    let view = ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::off());
    let counts = boxel_render::cull_counts(&view, origins.iter().map(|&o| (o, &*mesh)), None);
    assert_eq!(counts.chunks as usize, origins.len(), "every chunk in view");
    assert!(counts.buckets >= min_draws, "{} draws", counts.buckets);
    let ops: Vec<ChunkOp> = origins
        .iter()
        .enumerate()
        .map(|(id, &origin)| ChunkOp::Upsert {
            id: id as u64,
            origin,
            mesh: Arc::clone(&mesh),
        })
        .collect();
    let mut report = Report::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let mut images = Vec::new();
        for tier in allowed_tiers(&caps).into_iter().filter(|t| *t >= Tier::Mid) {
            let result = render_lists(
                adapter,
                &caps,
                tier,
                memory,
                &ops,
                &view,
                counts.buckets,
                origins.len(),
            );
            match result {
                Ok(image) => {
                    rendered += 1;
                    images.push((tier, image));
                }
                Err(e) => report.fail(format!("lists: {label} {tier}: {e}"), &[]),
            }
        }
        if let [(_, high), (_, mid)] = images.as_slice() {
            let diff = high.compare(mid, 2);
            if diff.fraction() > MAX_TIER_DIFF {
                report.fail(
                    format!(
                        "lists: {label}: High and Mid differ in {} pixels",
                        diff.differing_pixels
                    ),
                    &[("lists High", high), ("lists Mid", mid)],
                );
            }
            if high.non_background_fraction() < 0.05 {
                report.fail(
                    format!("lists: {label}: nothing drawn"),
                    &[("lists High", high)],
                );
            }
        }
    }
    report.finish(rendered);
}

/// One frame of [`lists_past_one_list`] on `tier`, checked: no chunk dropped, and on High the
/// cull's draws equal `buckets` over two page lists or more.
#[expect(clippy::too_many_arguments, reason = "a test helper")]
fn render_lists(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    memory: ChunkMemorySettings,
    ops: &[ChunkOp],
    view: &ViewParams,
    buckets: u32,
    chunks: usize,
) -> Result<RgbaImage, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let mut renderer = renderer(&device, &queue, tier, memory, TranslucentSort::Off)?;
    let image = frame(&device, &queue, &mut renderer, ops.to_vec(), true, view)?;
    let stats = renderer.stats();
    if stats.dropped_chunks != 0 || stats.chunks as usize != chunks {
        return Err(format!(
            "{} chunks, {} dropped",
            stats.chunks, stats.dropped_chunks
        ));
    }
    match renderer.debug_high_draw_counts(&device, &queue) {
        Some(drawn) => {
            let drawn = drawn?;
            let total: u32 = drawn.iter().map(|[o, c]| o + c).sum();
            if total != buckets {
                return Err(format!(
                    "High drew {total} buckets ({drawn:?}), the CPU counts {buckets}"
                ));
            }
            if drawn.len() < 2 {
                return Err(format!("{} page lists", drawn.len()));
            }
        }
        None if tier == Tier::High => return Err("no High draw counts".to_string()),
        None => {}
    }
    no_errors(&errors)?;
    Ok(image)
}

/// About 22,500 chunks at the default 10,922 chunks a page: 67,500 draws or more over three of
/// the floor's four pages (its 32,768 slots need four; one list held 65,535 before pages, and
/// High dropped the rest).
#[test]
fn high_lists_hold_every_bucket_past_65535_draws() {
    let memory = ChunkMemorySettings {
        initial_quads: 3 << 17,
        initial_slots: 32768,
        page_quads: 1 << 17,
        grow: false,
        ..ChunkMemorySettings::default()
    };
    lists_past_one_list(150, memory, 65_536);
}

/// 2,025 chunks at 64 chunks a page: 32 pages, each with its own short lists.
#[test]
fn high_lists_hold_every_bucket_with_small_pages() {
    let memory = ChunkMemorySettings {
        initial_quads: 32 * 4096,
        initial_slots: 4096,
        page_quads: 4096,
        page_chunks: 64,
        grow: false,
        ..ChunkMemorySettings::default()
    };
    lists_past_one_list(45, memory, 6000);
}

/// The default floor holds the 32,768 slots' chunks (four pages of 2 Mi quads), and a page full
/// of chunks refuses with its own error while it still has quads free, shown in the stats.
#[test]
fn pages_full_of_chunks_are_told_apart() {
    each_tier("chunk places", |adapter, caps, tier, _report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let default = renderer(
            &device,
            &queue,
            tier,
            ChunkMemorySettings::default(),
            TranslucentSort::Off,
        )?;
        let memory = default.stats().chunk_memory;
        if (memory.pages, memory.capacity_quads) != (4, 8 << 20) || memory.chunk_places < 32768 {
            return Err(format!("default floor: {memory:?}"));
        }
        drop(default);
        // one page of 4 chunk places (the ceiling allows no more), 8 slots
        let capped = ChunkMemorySettings {
            initial_quads: 1 << 16,
            initial_slots: 8,
            max_quads: boxel_render::MemoryCeiling::Quads(1 << 16),
            page_quads: 1 << 16,
            page_chunks: 4,
            grow: false,
            ..ChunkMemorySettings::default()
        };
        let mut renderer = renderer(&device, &queue, tier, capped, TranslucentSort::Off)?;
        let mesh = Arc::new(cube_mesh());
        let origin = |id: u64| I64Vec3::new(64 * id as i64, 0, 0);
        for id in 0..4 {
            renderer
                .upsert_chunk(id, origin(id), Arc::clone(&mesh))
                .map_err(|e| format!("chunk {id}: {e}"))?;
        }
        match renderer.upsert_chunk(4, origin(4), Arc::clone(&mesh)) {
            Err(boxel_render::RendererError::PageChunksFull {
                needed: 7,
                page_chunks: 4,
            }) => {}
            other => return Err(format!("a fifth chunk: {other:?}")),
        }
        let stats = renderer.stats();
        let memory = stats.chunk_memory;
        if (memory.pages, memory.chunk_places, memory.full_pages) != (1, 4, 1)
            || memory.largest_free_quads < 7
            || (stats.chunks, stats.dropped_chunks) != (4, 1)
        {
            return Err(format!("full of chunks: {stats:?}"));
        }
        let mut refused = Vec::new();
        renderer.take_refused(|pos, r| refused.push((pos, r)));
        if refused != [(boxel_voxel::ChunkPos::new(2 * 4, 0, 0), true)] {
            return Err(format!("refused: {refused:?}"));
        }
        // a removed chunk frees its place
        renderer.remove_chunk(0);
        renderer
            .upsert_chunk(4, origin(4), Arc::clone(&mesh))
            .map_err(|e| format!("after a removal: {e}"))?;
        if renderer.stats().chunk_memory.full_pages != 1 {
            return Err(format!("after a removal: {:?}", renderer.stats()));
        }
        let view = look(
            DVec3::new(160.0, 100.0, -60.0),
            DVec3::new(160.0, 16.0, 16.0),
            I64Vec3::ZERO,
        );
        frame(&device, &queue, &mut renderer, Vec::new(), false, &view)?;
        no_errors(&errors)
    });
}
