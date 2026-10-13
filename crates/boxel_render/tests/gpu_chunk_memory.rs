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
        // one page of 4 chunk places (its quads split no further), 8 slots
        let capped = ChunkMemorySettings {
            initial_quads: boxel_render::MIN_PAGE_QUADS,
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

// ---------------------------------------------------------------- growth and shrink (#57 M2, M3)

/// One frame as the render plugin runs it, growth included: the ops, the render (submitted),
/// then `grow_between_frames`; the main color read back when `read`. Returns the image and the
/// refusals (kept by the renderer and retried).
fn plugin_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    ops: Vec<ChunkOp>,
    resync: Resync,
    view: &ViewParams,
    read: bool,
) -> Result<(Option<RgbaImage>, usize), String> {
    let refused = renderer.apply_ops(device, ops, resync).len();
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_chunk_memory"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    let image = if read {
        Some(read_texture_rgba8(device, queue, renderer.main_color())?)
    } else {
        None
    };
    run_in_error_scopes(device, || {
        renderer.grow_between_frames(device);
        Ok(())
    })?;
    Ok((image, refused))
}

/// Chunk memory that starts at one page of 4,096 quads and 8 slots and grows: pages of 4,096
/// quads and at most 4 chunks.
fn growing() -> ChunkMemorySettings {
    ChunkMemorySettings {
        initial_quads: 4096,
        initial_slots: 8,
        page_quads: 4096,
        page_chunks: 4,
        grow: true,
        ..ChunkMemorySettings::default()
    }
}

/// `frames` with the first (a resync of every chunk) cut into one chunk a frame, so the pages
/// and the table grow between frames instead of all at once.
fn one_chunk_a_frame(frames: Vec<Frame>) -> Vec<Frame> {
    let mut frames = frames.into_iter();
    let Some(first) = frames.next() else {
        return Vec::new();
    };
    let mut out: Vec<Frame> = first
        .ops
        .into_iter()
        .enumerate()
        .map(|(i, op)| Frame {
            ops: vec![op],
            resync: i == 0,
        })
        .collect();
    out.extend(frames);
    out
}

/// The pages holding the live chunks of `live`, lowest first.
fn pages_holding(renderer: &VoxelRenderer, live: &[(u64, Arc<ChunkMesh>)]) -> Vec<u16> {
    let mut pages: Vec<u16> = live
        .iter()
        .filter_map(|(id, _)| renderer.debug_chunk_range(*id).map(|(page, ..)| page))
        .collect();
    pages.sort_unstable();
    pages.dedup();
    pages
}

/// How a growth run compares the two renderers' images.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Compare {
    /// Pixel for pixel (the sparse scene: no two chunks touch).
    Exact,
    /// Within the tier rule (chunks touch: the order they are drawn in may reach a sliver of
    /// pixels where coplanar faces meet).
    TierRule,
}

/// What a growth run saw, summed over runs.
#[derive(Debug, Default)]
struct Seen {
    pages_created: u32,
    pages_released: u32,
    emergency_tables: u32,
    ahead_tables: u32,
    relocated_bytes: u64,
    compared: u32,
    /// The largest share of scene pixels a compared frame had.
    in_view: f64,
}

/// The two renderers of a growth run and what it needs each frame.
struct Run<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    view: &'a ViewParams,
    reference: VoxelRenderer,
    paged: VoxelRenderer,
    evacuate: bool,
    compare: Compare,
    what: &'a str,
    /// The live chunks.
    live: Vec<(u64, Arc<ChunkMesh>)>,
    /// Frames run.
    k: usize,
}

impl Run<'_> {
    /// One frame of `ops` on both renderers, compared when nothing waits for room.
    fn step(
        &mut self,
        ops: Vec<ChunkOp>,
        resync: bool,
        report: &mut Report,
        seen: &mut Seen,
    ) -> Result<(), String> {
        let k = self.k;
        self.k += 1;
        if resync {
            self.live.clear();
        }
        for op in &ops {
            match op {
                ChunkOp::Upsert { id, mesh, .. } => {
                    self.live.retain(|(i, _)| i != id);
                    self.live.push((*id, Arc::clone(mesh)));
                }
                ChunkOp::Remove { id } => self.live.retain(|(i, _)| i != id),
            }
        }
        let resync = if resync { Resync::Chunks } else { Resync::None };
        let (device, queue, view) = (self.device, self.queue, self.view);
        let (a, refused) = plugin_frame(
            device,
            queue,
            &mut self.reference,
            ops.clone(),
            resync,
            view,
            true,
        )?;
        if refused > 0 {
            return Err(format!("frame {k}: the reference refused {refused} chunks"));
        }
        let (b, _) = plugin_frame(device, queue, &mut self.paged, ops, resync, view, true)?;
        if self.evacuate
            && let Some(&page) = pages_holding(&self.paged, &self.live).first()
        {
            self.paged.debug_evacuate_page(page);
        }
        self.paged
            .debug_advance_clock(std::time::Duration::from_secs(1));
        if self.paged.chunk_memory().refused_waiting > 0 {
            return Ok(());
        }
        let (Some(a), Some(b)) = (a, b) else {
            return Ok(());
        };
        seen.compared += 1;
        let what = format!("{} frame {k}", self.what);
        let diff = a.compare(&b, if self.compare == Compare::Exact { 0 } else { 2 });
        let bad = match self.compare {
            Compare::Exact => diff.differing_pixels > 0,
            Compare::TierRule => diff.fraction() > MAX_TIER_DIFF,
        };
        if bad {
            report.fail(
                format!("{what}: {} pixels differ", diff.differing_pixels),
                &[
                    (&format!("{what} one page"), &a),
                    (&format!("{what} growing"), &b),
                ],
            );
        }
        seen.in_view = seen.in_view.max(a.non_background_fraction());
        Ok(())
    }
}

/// Feeds `frames` to a one-page reference and to a renderer that grows from one tiny page, as
/// the render plugin does; with `evacuate`, every frame drains the lowest page holding chunks.
/// The clock moves a second a frame (shrink's holds pass). Every frame where nothing waits for
/// room is compared; then most chunks go and shrink runs until it releases pages, every frame
/// compared; last, every live range reads back as its mesh (on Low its light page too).
#[expect(clippy::too_many_arguments, reason = "a test helper")]
fn growth_run(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tier: Tier,
    frames: Vec<Frame>,
    view: &ViewParams,
    evacuate: bool,
    compare: Compare,
    what: &str,
    report: &mut Report,
    seen: &mut Seen,
) -> Result<(), String> {
    let mut run = Run {
        device,
        queue,
        view,
        reference: renderer(device, queue, tier, one_page(), TranslucentSort::Inline)?,
        paged: renderer(device, queue, tier, growing(), TranslucentSort::Inline)?,
        evacuate,
        compare,
        what,
        live: Vec::new(),
        k: 0,
    };
    for f in frames {
        run.step(f.ops, f.resync, report, seen)?;
    }
    // what waits for room comes back
    for _ in 0..30 {
        if run.paged.chunk_memory().refused_waiting == 0 {
            break;
        }
        run.step(Vec::new(), false, report, seen)?;
    }
    if run.paged.chunk_memory().refused_waiting > 0 {
        return Err(format!(
            "refused chunks never came back: {:?}, pages {:?}",
            run.paged.chunk_memory(),
            run.paged.debug_pages()
        ));
    }
    // shrink: all but three chunks go
    let gone: Vec<ChunkOp> = run
        .live
        .iter()
        .skip(3)
        .map(|(id, _)| ChunkOp::Remove { id: *id })
        .collect();
    run.step(gone, false, report, seen)?;
    let before = run.paged.chunk_memory();
    for _ in 0..40 {
        run.step(Vec::new(), false, report, seen)?;
    }
    let after = run.paged.chunk_memory();
    if after.pages_released <= before.pages_released || after.pages >= before.pages {
        return Err(format!(
            "shrink released nothing: before {before:?}, after {after:?}"
        ));
    }
    // the sorted translucent buckets are not in the mesher's order: those ranges are checked by
    // the images
    let solid: Vec<(u64, Arc<ChunkMesh>)> = run
        .live
        .iter()
        .filter(|(_, mesh)| mesh.bucket_counts[BUCKET_COUNT - 1] == 0)
        .cloned()
        .collect();
    check_ranges(device, queue, &run.paged, tier, &solid)?;
    seen.pages_created += after.pages_created;
    seen.pages_released += after.pages_released;
    seen.emergency_tables += after.emergency_tables;
    if after.emergency_tables == 0 && after.table_slots > 8 {
        seen.ahead_tables += 1;
    }
    seen.relocated_bytes += after.relocated_bytes;
    if run.paged.stats().chunks != run.reference.stats().chunks {
        return Err("the chunk counts differ".to_string());
    }
    Ok(())
}

/// G1 (exact, no neighbors): the sparse scene drawn by a renderer that grows from one tiny page
/// (pages created between frames and cleared first, or in a frame for a burst; the table grown
/// from 8 slots, ahead and at once) equals one page, pixel for pixel, every frame; also with a
/// page drained into the others every frame (shrink's evacuation and releases), and far from
/// the origin. The pages read back as their meshes after the moves (G8, G10).
#[test]
fn growing_pages_draw_exactly_what_one_page_draws() {
    let registry = boxel_demo::blocks::registry();
    let meshes = sparse_meshes(&registry);
    let eye = DVec3::new(-6.0, 104.0, -12.0);
    let target = DVec3::new(72.0, 28.0, 78.0);
    each_tier("growth", |adapter, caps, tier, report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let label = format!("{} {tier}", adapter_label(caps));
        let mut seen = Seen::default();
        for shift in SHIFTS {
            let view = look(eye, target, shift);
            for evacuate in [false, true] {
                let frames = sparse_frames(&meshes, shift);
                // (a) grows between frames; (b) takes every chunk in its first frame (the
                // table grows at once) and drains a page every frame
                let frames = if evacuate {
                    frames
                } else {
                    one_chunk_a_frame(frames)
                };
                let what = format!("{label} shift {shift} evacuate {evacuate}");
                growth_run(
                    &device,
                    &queue,
                    tier,
                    frames,
                    &view,
                    evacuate,
                    Compare::Exact,
                    &what,
                    report,
                    &mut seen,
                )
                .map_err(|e| format!("{what}: {e}"))?;
            }
        }
        // Low has no GPU table: its table grows on the CPU, never as an emergency
        if seen.pages_created == 0
            || seen.pages_released == 0
            || (seen.emergency_tables == 0) == (tier >= Tier::Mid)
            || seen.ahead_tables == 0
            || seen.relocated_bytes == 0
            || seen.compared < 40
            || seen.in_view < 0.03
        {
            return Err(format!("the runs did not grow, move and shrink: {seen:?}"));
        }
        no_errors(&errors)
    });
}

/// The dense scene of G2: a heightfield of stone over 3 x 3 chunks that touch, water in its
/// valleys and a box of leaves across a chunk border.
fn dense_meshes(registry: &BlockRegistry, phase: f32) -> Vec<(IVec3, Arc<ChunkMesh>)> {
    let height = |x: i32, z: i32| -> u32 {
        let (x, z) = (x as f32, z as f32);
        (12.0 + 5.0 * (x * 0.11 + phase).sin() + 4.0 * (z * 0.07 - phase).cos()) as u32
    };
    let mut out = Vec::new();
    for cz in 0..3 {
        for cx in 0..3 {
            let mut blocks = Vec::new();
            for z in 0..32u32 {
                for x in 0..32u32 {
                    let h = height(cx * 32 + x as i32, cz * 32 + z as i32);
                    for y in 0..h {
                        blocks.push(([x, y, z], "boxel:stone"));
                    }
                    for y in h..11 {
                        blocks.push(([x, y, z], "boxel:water"));
                    }
                }
            }
            // leaves across the border between cells x 0 and 1
            let lx = if cx == 0 {
                26..32
            } else if cx == 1 {
                0..6
            } else {
                0..0
            };
            if cz == 1 {
                for x in lx {
                    for y in 18..23 {
                        for z in 10..16 {
                            blocks.push(([x, y, z], "boxel:leaves"));
                        }
                    }
                }
            }
            let mesh = mesh_blocks(registry, &blocks);
            out.push((IVec3::new(cx, 0, cz), Arc::new(mesh)));
        }
    }
    out
}

/// G2 (dense, the tier rule): the heightfield with leaves across a chunk border and water,
/// growing from one tiny page as in G1, with and without a page drained every frame, within
/// `MAX_TIER_DIFF` of one page on each tier (chunks touch, so where coplanar faces meet the
/// order they are drawn in may reach a sliver of pixels, as on High it always could).
#[test]
fn growing_pages_keep_the_tier_rule_on_touching_chunks() {
    let registry = boxel_demo::blocks::registry();
    let versions = [dense_meshes(&registry, 0.0), dense_meshes(&registry, 1.3)];
    let view = look(
        DVec3::new(-20.0, 60.0, -24.0),
        DVec3::new(48.0, 8.0, 48.0),
        I64Vec3::ZERO,
    );
    each_tier("dense", |adapter, caps, tier, report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let label = format!("{} {tier}", adapter_label(caps));
        let mut seen = Seen::default();
        for evacuate in [false, true] {
            let upsert = |id: u64, (c, mesh): &(IVec3, Arc<ChunkMesh>)| ChunkOp::Upsert {
                id,
                origin: c.as_i64vec3() * 32,
                mesh: Arc::clone(mesh),
            };
            let mut frames: Vec<Frame> = versions[0]
                .iter()
                .enumerate()
                .map(|(i, m)| Frame {
                    ops: vec![upsert(i as u64, m)],
                    resync: i == 0,
                })
                .collect();
            // every chunk remeshed (the valleys move), two at a time
            for pair in (0..versions[1].len()).collect::<Vec<_>>().chunks(2) {
                frames.push(Frame {
                    ops: pair
                        .iter()
                        .map(|&i| upsert(i as u64, &versions[1][i]))
                        .collect(),
                    resync: false,
                });
            }
            let what = format!("{label} dense evacuate {evacuate}");
            growth_run(
                &device,
                &queue,
                tier,
                frames,
                &view,
                evacuate,
                Compare::TierRule,
                &what,
                report,
                &mut seen,
            )
            .map_err(|e| format!("{what}: {e}"))?;
        }
        if seen.pages_created == 0 || seen.relocated_bytes == 0 || seen.in_view < 0.03 {
            return Err(format!("the runs did not grow and move: {seen:?}"));
        }
        no_errors(&errors)
    });
}

/// A chunk of `n` blocks of `name`, two voxels apart so none touch: `6 n` quads.
fn pillars(registry: &BlockRegistry, n: u32, name: &'static str) -> Arc<ChunkMesh> {
    let blocks: Vec<([u32; 3], &str)> = (0..n.min(16 * 16 * 16))
        .map(|i| ([2 * (i % 16), 2 * ((i / 16) % 16), 2 * (i / 256)], name))
        .collect();
    Arc::new(mesh_blocks(registry, &blocks))
}

/// The cells of a sparse grid (even chunk coordinates, no two touch), `n` of them.
fn grid(n: usize) -> Vec<IVec3> {
    (0..n as i32)
        .map(|i| IVec3::new(2 * (i % 10), 2 * (i / 100), 2 * ((i / 10) % 10)))
        .collect()
}

/// Upserts of `mesh` at `cells`, ids from `first`.
fn upserts(cells: &[IVec3], first: u64, mesh: &Arc<ChunkMesh>) -> Vec<ChunkOp> {
    cells
        .iter()
        .enumerate()
        .map(|(i, c)| ChunkOp::Upsert {
            id: first + i as u64,
            origin: c.as_i64vec3() * 32,
            mesh: Arc::clone(mesh),
        })
        .collect()
}

/// A camera over the grid of [`grid`] (its near half inside the view distance).
fn grid_view() -> ViewParams {
    look(
        DVec3::new(100.0, 120.0, -60.0),
        DVec3::new(300.0, 0.0, 300.0),
        I64Vec3::ZERO,
    )
}

/// Every `(position, refused)` change the renderer reports.
fn refusals(renderer: &mut VoxelRenderer) -> Vec<(boxel_voxel::ChunkPos, bool)> {
    let mut out = Vec::new();
    renderer.take_refused(|pos, refused| out.push((pos, refused)));
    out
}

/// G5 (a burst): a frame's burst past the reserve but within a page takes exactly one page made
/// in that frame (an emergency), all in its class, nothing refused; a burst past one page while
/// the other class is full of chunks takes one page and refuses the rest, which wait and come
/// back once pages are made between frames. The image ends equal to one large page's.
#[test]
fn a_burst_takes_one_page_at_once_and_the_rest_waits() {
    let registry = boxel_demo::blocks::registry();
    let big = pillars(&registry, 100, "boxel:stone");
    let small = pillars(&registry, 25, "boxel:stone");
    let water = pillars(&registry, 1, "boxel:water");
    let cells = grid(100);
    let view = grid_view();
    let memory = ChunkMemorySettings {
        initial_quads: 8192,
        initial_slots: 256,
        page_quads: 4096,
        page_chunks: 16,
        grow: true,
        ..ChunkMemorySettings::default()
    };
    let wide = ChunkMemorySettings {
        initial_quads: 1 << 18,
        initial_slots: 256,
        grow: false,
        ..ChunkMemorySettings::default()
    };
    // 5 large chunks, a burst of 12 small ones, 16 tiny water chunks, a burst of 60 small ones
    let frames = [
        upserts(&cells[..5], 0, &big),
        upserts(&cells[5..17], 5, &small),
        upserts(&cells[17..33], 17, &water),
        upserts(&cells[33..93], 33, &small),
    ];
    each_tier("burst", |adapter, caps, tier, report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let mut reference = renderer(&device, &queue, tier, wide, TranslucentSort::Off)?;
        let mut paged = renderer(&device, &queue, tier, memory, TranslucentSort::Off)?;
        // the pages hold the floor's: Solid page 0, Translucent page 1
        let mut last = None;
        for (k, ops) in frames.iter().enumerate() {
            let resync = if k == 0 { Resync::Chunks } else { Resync::None };
            let refused = reference.apply_ops(&device, ops.clone(), resync);
            if !refused.is_empty() {
                return Err(format!("frame {k}: the reference refused"));
            }
            frame(&device, &queue, &mut reference, Vec::new(), false, &view)?;
            // no frame between: only the reserve of each frame grows
            let refused = paged.apply_ops(&device, ops.clone(), resync).len();
            last = Some(frame(
                &device,
                &queue,
                &mut paged,
                Vec::new(),
                false,
                &view,
            )?);
            let memory = paged.chunk_memory();
            match k {
                1 => {
                    if refused != 0 || memory.emergency_pages != 1 {
                        return Err(format!("burst 1: {refused} refused, {memory:?}"));
                    }
                    // all in Solid pages: the Translucent page took none
                    for id in 5..17 {
                        let (page, ..) = paged.debug_chunk_range(id).ok_or("not resident")?;
                        if paged.debug_pages().iter().any(|p| p.0 == page && p.1) {
                            return Err(format!("burst 1: chunk {id} spilled into page {page}"));
                        }
                    }
                }
                2 if refused != 0 || memory.emergency_pages != 1 => {
                    return Err(format!("the water: {refused} refused, {memory:?}"));
                }
                3 => {
                    // one more page at once, the rest refused and kept
                    if refused == 0
                        || memory.emergency_pages != 2
                        || memory.refused_waiting as usize != refused
                    {
                        return Err(format!("burst 2: {refused} refused, {memory:?}"));
                    }
                    let reported = refusals(&mut paged);
                    if reported.len() != refused || reported.iter().any(|(_, r)| !r) {
                        return Err(format!("burst 2 reported {reported:?}"));
                    }
                }
                _ => {}
            }
        }
        // between frames now: pages come, the waiting chunks with them
        let mut shown = 0;
        for _ in 0..30 {
            if paged.chunk_memory().refused_waiting == 0 {
                break;
            }
            let (image, _) = plugin_frame(
                &device,
                &queue,
                &mut paged,
                Vec::new(),
                Resync::None,
                &view,
                true,
            )?;
            last = image;
            shown += refusals(&mut paged).iter().filter(|(_, r)| !r).count();
        }
        let memory = paged.chunk_memory();
        if memory.refused_waiting != 0 || memory.emergency_pages != 2 || shown == 0 {
            return Err(format!("after the burst: {shown} shown, {memory:?}"));
        }
        let (image, _) = plugin_frame(
            &device,
            &queue,
            &mut paged,
            Vec::new(),
            Resync::None,
            &view,
            true,
        )?;
        let expected = frame(&device, &queue, &mut reference, Vec::new(), false, &view)?;
        let image = image.or(last).ok_or("no image")?;
        let diff = expected.compare(&image, 0);
        if diff.differing_pixels > 0 || expected.non_background_fraction() < 0.002 {
            report.fail(
                format!("burst {tier}: {} pixels differ", diff.differing_pixels),
                &[("burst wide", &expected), ("burst paged", &image)],
            );
        }
        no_errors(&errors)
    });
}

/// G6 (the ceiling): with the pages grown only by each frame's reserve, the same chunks are
/// refused on every tier; a refused remesh changes nothing; raising the ceiling brings the kept
/// chunks back nearest first; lowering it drops nothing resident.
#[test]
fn the_ceiling_refuses_the_same_chunks_everywhere_and_gives_them_back() {
    let registry = boxel_demo::blocks::registry();
    let small = pillars(&registry, 25, "boxel:stone");
    let large = pillars(&registry, 300, "boxel:stone");
    let cells = grid(100);
    let view = grid_view();
    let page = 4096;
    let memory = ChunkMemorySettings {
        initial_quads: page,
        initial_slots: 256,
        page_quads: page,
        page_chunks: 64,
        max_quads: boxel_render::MemoryCeiling::Quads(2 * page),
        grow: true,
        ..ChunkMemorySettings::default()
    };
    let refused_by_tier = Mutex::new(Vec::new());
    each_tier("ceiling", |adapter, caps, tier, _report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let mut paged = renderer(&device, &queue, tier, memory, TranslucentSort::Off)?;
        let refused = paged
            .apply_ops(&device, upserts(&cells, 0, &small), Resync::Chunks)
            .len();
        let before = frame(&device, &queue, &mut paged, Vec::new(), false, &view)?;
        let stats = paged.chunk_memory();
        if refused == 0 || stats.capacity_quads > 2 * u64::from(page) {
            return Err(format!("{refused} refused at the ceiling: {stats:?}"));
        }
        let mut refused_at: Vec<_> = refusals(&mut paged).into_iter().map(|(p, _)| p).collect();
        refused_at.sort_by_key(|p| (p.x, p.y, p.z));
        refused_by_tier
            .lock()
            .map_err(|e| e.to_string())?
            .push((tier, refused_at.clone()));
        // a refused remesh keeps the old mesh: range and image unchanged
        let kept = paged.debug_chunk_range(0);
        let ops = vec![ChunkOp::Upsert {
            id: 0,
            origin: cells[0].as_i64vec3() * 32,
            mesh: Arc::clone(&large),
        }];
        if paged.apply_ops(&device, ops, Resync::None).is_empty() {
            return Err("a remesh past the ceiling was not refused".to_string());
        }
        let same = frame(&device, &queue, &mut paged, Vec::new(), false, &view)?;
        if paged.debug_chunk_range(0) != kept || same.compare(&before, 0).differing_pixels > 0 {
            return Err("a refused remesh changed something".to_string());
        }
        // the remesh waits too; give the old mesh back so only the first refusals wait
        let ops = vec![ChunkOp::Upsert {
            id: 0,
            origin: cells[0].as_i64vec3() * 32,
            mesh: Arc::clone(&small),
        }];
        paged.apply_ops(&device, ops, Resync::None);
        refusals(&mut paged);
        // one more page under the ceiling: the nearest waiting chunks come back
        paged.set_memory_limits(&ChunkMemorySettings {
            max_quads: boxel_render::MemoryCeiling::Quads(3 * page),
            ..memory
        });
        for _ in 0..8 {
            frame(&device, &queue, &mut paged, Vec::new(), false, &view)?;
        }
        let back: Vec<_> = refusals(&mut paged)
            .into_iter()
            .filter(|(_, r)| !r)
            .map(|(p, _)| p)
            .collect();
        let waiting: Vec<_> = refused_at.iter().filter(|p| !back.contains(p)).collect();
        if back.is_empty() || waiting.is_empty() {
            return Err(format!(
                "{} back, {} waiting after one more page",
                back.len(),
                waiting.len()
            ));
        }
        let eye = view.render_origin.as_dvec3() + view.eye_rel.as_dvec3();
        let dist = |p: &boxel_voxel::ChunkPos| {
            let c = DVec3::new(p.x as f64, p.y as f64, p.z as f64) * 32.0 + 16.0;
            c.distance_squared(eye)
        };
        let far_back = back.iter().map(dist).fold(0.0, f64::max);
        let near_waiting = waiting.iter().map(|p| dist(p)).fold(f64::MAX, f64::min);
        if far_back > near_waiting {
            return Err("the waiting chunks did not come back nearest first".to_string());
        }
        // a lower ceiling drops nothing resident
        let chunks = paged.stats().chunks;
        paged.set_memory_limits(&ChunkMemorySettings {
            max_quads: boxel_render::MemoryCeiling::Quads(page),
            ..memory
        });
        frame(&device, &queue, &mut paged, Vec::new(), false, &view)?;
        if paged.stats().chunks != chunks {
            return Err("a lower ceiling dropped chunks".to_string());
        }
        no_errors(&errors)
    });
    let by_tier = refused_by_tier.into_inner().expect("lock");
    for pair in by_tier.windows(2) {
        assert_eq!(
            pair[0].1, pair[1].1,
            "{} and {} refused different chunks",
            pair[0].0, pair[1].0
        );
    }
}

/// The slot ceiling: chunks refused for want of a chunk slot make no page once the table cannot
/// grow (pages cannot help them; they used to count as quad demand, so growth made pages nothing
/// could use and shrink drained them again, for good). Raising the slot ceiling grows the table
/// and brings them back.
#[test]
fn chunks_refused_at_the_slot_ceiling_make_no_page() {
    let registry = boxel_demo::blocks::registry();
    let small = pillars(&registry, 25, "boxel:stone");
    let cells = grid(100);
    let view = grid_view();
    let memory = ChunkMemorySettings {
        initial_quads: 4 * 4096,
        initial_slots: 64,
        page_quads: 4096,
        max_slots: 64,
        grow: true,
        ..ChunkMemorySettings::default()
    };
    let wide = ChunkMemorySettings {
        initial_quads: 1 << 18,
        initial_slots: 256,
        grow: false,
        ..ChunkMemorySettings::default()
    };
    let all = upserts(&cells, 0, &small);
    each_tier("slot ceiling", |adapter, caps, tier, report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let mut reference = renderer(&device, &queue, tier, wide, TranslucentSort::Off)?;
        let expected = frame(&device, &queue, &mut reference, all.clone(), true, &view)?;
        let mut paged = renderer(&device, &queue, tier, memory, TranslucentSort::Off)?;
        let second = std::time::Duration::from_secs(1);
        let (_, refused) = plugin_frame(
            &device,
            &queue,
            &mut paged,
            all.clone(),
            Resync::Chunks,
            &view,
            false,
        )?;
        let after = paged.chunk_memory();
        if refused != 36 || after.refused_waiting != 36 {
            return Err(format!("{refused} refused at the slot ceiling: {after:?}"));
        }
        // past every hold and backoff: nothing grows for them
        for _ in 0..40 {
            paged.debug_advance_clock(second);
            plugin_frame(
                &device,
                &queue,
                &mut paged,
                Vec::new(),
                Resync::None,
                &view,
                false,
            )?;
        }
        let stats = paged.chunk_memory();
        if stats.pages_created != after.pages_created
            || stats.pages > after.pages
            || stats.refused_waiting != 36
        {
            return Err(format!(
                "pages for chunks without a slot: {after:?}, then {stats:?}"
            ));
        }
        // a higher slot ceiling: the table grows and they come back
        paged.set_memory_limits(&ChunkMemorySettings {
            max_slots: 256,
            ..memory
        });
        let mut image = None;
        for _ in 0..30 {
            paged.debug_advance_clock(second);
            let (i, _) = plugin_frame(
                &device,
                &queue,
                &mut paged,
                Vec::new(),
                Resync::None,
                &view,
                true,
            )?;
            image = i;
            if paged.chunk_memory().refused_waiting == 0 {
                break;
            }
        }
        let stats = paged.chunk_memory();
        if stats.refused_waiting != 0 || stats.table_slots <= 64 {
            return Err(format!("still waiting for a slot: {stats:?}"));
        }
        let image = image.ok_or("no image")?;
        let diff = expected.compare(&image, 0);
        if diff.differing_pixels > 0 {
            report.fail(
                format!(
                    "slot ceiling {tier}: {} pixels differ",
                    diff.differing_pixels
                ),
                &[
                    ("slot ceiling wide", &expected),
                    ("slot ceiling paged", &image),
                ],
            );
        }
        no_errors(&errors)
    });
}

/// A table asked for between frames that waits (the one creation of `grow_between_frames` went
/// to a page) while frames that are not drawn grow the table past it at once: the old request
/// is never swapped in. It used to be: the two-Prepare fallback built the smaller table, the
/// next frame swapped it in, and the flush wrote slots past its end (rejected frames, then a
/// device rebuild).
#[test]
fn a_stale_table_request_never_shrinks_the_table() {
    let registry = boxel_demo::blocks::registry();
    let mesh = pillars(&registry, 4, "boxel:stone");
    let cells = grid(100);
    let view = grid_view();
    let memory = ChunkMemorySettings {
        initial_quads: 1 << 14,
        initial_slots: 8,
        page_quads: 1 << 14,
        grow: true,
        ..ChunkMemorySettings::default()
    };
    let wide = ChunkMemorySettings {
        initial_quads: 1 << 18,
        initial_slots: 256,
        grow: false,
        ..ChunkMemorySettings::default()
    };
    let all = upserts(&cells, 0, &mesh);
    each_tier("stale table", |adapter, caps, tier, report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let mut reference = renderer(&device, &queue, tier, wide, TranslucentSort::Off)?;
        let expected = frame(&device, &queue, &mut reference, all.clone(), true, &view)?;
        let mut paged = renderer(&device, &queue, tier, memory, TranslucentSort::Off)?;
        // a drawn frame wants a page and a table of 16 slots; the page takes the creation
        paged.debug_force_page(false);
        plugin_frame(
            &device,
            &queue,
            &mut paged,
            all[..4].to_vec(),
            Resync::Chunks,
            &view,
            false,
        )?;
        let pages = paged.chunk_memory().pages_created;
        if pages != 1 {
            return Err(format!("{pages} pages made between frames"));
        }
        // three Prepares with no frame: bursts grow the table to 64, then 128, at once
        for ops in [all[4..40].to_vec(), all[40..].to_vec(), Vec::new()] {
            if let Some(e) = paged.apply_ops(&device, ops, Resync::None).first() {
                return Err(format!("refused: {e}"));
            }
        }
        let mut image = None;
        for _ in 0..3 {
            let (i, _) = plugin_frame(
                &device,
                &queue,
                &mut paged,
                Vec::new(),
                Resync::None,
                &view,
                true,
            )?;
            image = i;
        }
        let stats = paged.chunk_memory();
        if stats.table_slots < 128 {
            return Err(format!("the table shrank: {stats:?}"));
        }
        let image = image.ok_or("no image")?;
        let diff = expected.compare(&image, 0);
        if diff.differing_pixels > 0 {
            report.fail(
                format!(
                    "stale table {tier}: {} pixels differ",
                    diff.differing_pixels
                ),
                &[
                    ("stale table wide", &expected),
                    ("stale table paged", &image),
                ],
            );
        }
        no_errors(&errors)
    });
}

/// G7 (faults): uploads lost while a page drains lead to a resync and the right image; a page
/// that cannot be created leaves chunks waiting, and they come back after the 10 s backoff; a
/// fresh renderer fed a scene of many pages in one resync frame (what the plugin does after a
/// device loss) makes them all in that frame and draws it right.
#[test]
fn faults_resync_wait_or_grow_all_at_once() {
    let registry = boxel_demo::blocks::registry();
    let meshes = sparse_meshes(&registry);
    let eye = DVec3::new(-6.0, 104.0, -12.0);
    let target = DVec3::new(72.0, 28.0, 78.0);
    let view = look(eye, target, I64Vec3::ZERO);
    let frames = sparse_frames(&meshes, I64Vec3::ZERO);
    let all = frames[0].ops.clone();
    let stone = pillars(&registry, 25, "boxel:stone");
    each_tier("faults", |adapter, caps, tier, report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let mut reference = renderer(&device, &queue, tier, one_page(), TranslucentSort::Off)?;
        let expected = frame(&device, &queue, &mut reference, all.clone(), true, &view)?;
        let same = |image: &RgbaImage, what: &str, report: &mut Report| {
            let diff = expected.compare(image, 0);
            if diff.differing_pixels > 0 {
                report.fail(
                    format!(
                        "faults {tier} {what}: {} pixels differ",
                        diff.differing_pixels
                    ),
                    &[("faults expected", &expected), (what, image)],
                );
            }
        };
        // (3) every chunk in one resync frame: pages made at once, nothing refused
        let mut paged = renderer(&device, &queue, tier, growing(), TranslucentSort::Off)?;
        let (image, refused) = plugin_frame(
            &device,
            &queue,
            &mut paged,
            all.clone(),
            Resync::Chunks,
            &view,
            true,
        )?;
        if refused != 0 || paged.chunk_memory().pages < 5 {
            return Err(format!("one resync frame: {refused} refused"));
        }
        same(&image.ok_or("no image")?, "one resync frame", report);
        // (1) uploads lost while a page drains: a resync brings the right image back
        paged.debug_set_budgets(Some((1 << 20, 1 << 10, 1)));
        let page = paged.debug_chunk_range(0).ok_or("chunk 0")?.0;
        if !paged.debug_evacuate_page(page) {
            return Err(format!("page {page} would not drain"));
        }
        plugin_frame(
            &device,
            &queue,
            &mut paged,
            Vec::new(),
            Resync::None,
            &view,
            false,
        )?;
        // the staging buffer fails as when out of memory (wgpu reports it, uncaptured here)
        paged.debug_fail_next_upload();
        paged.apply_ops(&device, vec![all[1].clone()], Resync::None);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_chunk_memory lost"),
        });
        paged.render(&device, &queue, &mut encoder, &view);
        submit_and_wait(&device, &queue, encoder)?;
        if !paged.take_lost_uploads() {
            return Err("the uploads were not lost".to_string());
        }
        let lost = std::mem::take(&mut *errors.lock().map_err(|e| e.to_string())?);
        if lost.len() != 1 {
            return Err(format!("the lost uploads reported {lost:?}"));
        }
        let (image, refused) = plugin_frame(
            &device,
            &queue,
            &mut paged,
            all.clone(),
            Resync::Chunks,
            &view,
            true,
        )?;
        if refused != 0 {
            return Err(format!("the resync refused {refused}"));
        }
        same(&image.ok_or("no image")?, "after lost uploads", report);
        paged.debug_set_budgets(None);
        // (2) a failed page creation: refused, kept, back after the backoff (stone only: only
        // the Solid class grows, and it waits)
        let view = grid_view();
        let solid = upserts(&grid(30), 0, &stone);
        let mut reference = renderer(&device, &queue, tier, one_page(), TranslucentSort::Off)?;
        let expected = frame(&device, &queue, &mut reference, solid.clone(), true, &view)?;
        let mut paged = renderer(&device, &queue, tier, growing(), TranslucentSort::Off)?;
        let (first, rest) = solid.split_at(1);
        plugin_frame(
            &device,
            &queue,
            &mut paged,
            first.to_vec(),
            Resync::Chunks,
            &view,
            false,
        )?;
        paged.debug_fail_next_page();
        let refused = paged.apply_ops(&device, rest.to_vec(), Resync::None).len();
        let memory = paged.chunk_memory();
        if refused == 0 || memory.page_create_failures != 1 {
            return Err(format!("a failed creation: {refused} refused, {memory:?}"));
        }
        for _ in 0..5 {
            plugin_frame(
                &device,
                &queue,
                &mut paged,
                Vec::new(),
                Resync::None,
                &view,
                false,
            )?;
        }
        if paged.chunk_memory().pages_created != 0 {
            return Err(format!(
                "grew during the backoff: {:?}",
                paged.chunk_memory()
            ));
        }
        paged.debug_advance_clock(std::time::Duration::from_secs(10));
        let mut image = None;
        for _ in 0..40 {
            let (i, _) = plugin_frame(
                &device,
                &queue,
                &mut paged,
                Vec::new(),
                Resync::None,
                &view,
                true,
            )?;
            image = i;
            if paged.chunk_memory().refused_waiting == 0 {
                break;
            }
        }
        if paged.chunk_memory().refused_waiting != 0 {
            return Err(format!("still waiting: {:?}", paged.chunk_memory()));
        }
        let (image, _) = plugin_frame(
            &device,
            &queue,
            &mut paged,
            Vec::new(),
            Resync::None,
            &view,
            true,
        )
        .map(|(i, r)| (i.or(image), r))?;
        let image = image.ok_or("no image")?;
        let diff = expected.compare(&image, 0);
        if diff.differing_pixels > 0 {
            report.fail(
                format!(
                    "faults {tier} after the backoff: {} pixels differ",
                    diff.differing_pixels
                ),
                &[
                    ("faults solid expected", &expected),
                    ("after the backoff", &image),
                ],
            );
        }
        no_errors(&errors)
    });
}

/// G11 (a radius swing, the plugin's frame order): chunks of a radius 8, then 32, then 8
/// around the origin arrive and go at 512 a frame. Nothing is refused, the pages rise and come
/// back to the floor, no page is made inside a frame after the first 60 frames of each step,
/// and no frame moves more than the move budget.
#[test]
fn a_radius_swing_grows_and_shrinks_back_to_the_floor() {
    let registry = boxel_demo::blocks::registry();
    let meshes: Vec<Arc<ChunkMesh>> = (10..60)
        .map(|n| pillars(&registry, n, "boxel:stone"))
        .collect();
    let id = |x: i64, z: i64| ((x + 64) << 8 | (z + 64)) as u64;
    let mesh = |x: i64, z: i64| Arc::clone(&meshes[((x * 7 + z * 13).rem_euclid(50)) as usize]);
    let ring = |r: i64| -> Vec<(i64, i64)> {
        (-r..=r)
            .flat_map(|x| (-r..=r).map(move |z| (x, z)))
            .collect()
    };
    // a floor of three pages (two Solid, one Translucent): radius 8 fits it with the room shrink
    // keeps (a reserve of half a page plus half a page)
    let memory = ChunkMemorySettings {
        initial_quads: 3 << 18,
        initial_slots: 1024,
        page_quads: 1 << 18,
        grow: true,
        ..ChunkMemorySettings::default()
    };
    // looking up: nothing to rasterize, the frames stay cheap on every adapter
    let view = look(
        DVec3::new(0.0, 40.0, 0.0),
        DVec3::new(1.0, 400.0, 0.0),
        I64Vec3::ZERO,
    );
    each_tier("swing", |adapter, caps, tier, _report| {
        let (device, queue, errors) = device_for(adapter, caps, tier)?;
        let mut paged = renderer(&device, &queue, tier, memory, TranslucentSort::Off)?;
        let floor = paged.chunk_memory();
        let mut live: std::collections::BTreeSet<(i64, i64)> = Default::default();
        let mut peak = 0;
        let mut first = true;
        for (step, radius) in [8, 32, 8].into_iter().enumerate() {
            let want: std::collections::BTreeSet<(i64, i64)> = ring(radius).into_iter().collect();
            let mut ops: Vec<ChunkOp> = live
                .difference(&want)
                .map(|&(x, z)| ChunkOp::Remove { id: id(x, z) })
                .collect();
            ops.extend(want.difference(&live).map(|&(x, z)| ChunkOp::Upsert {
                id: id(x, z),
                origin: I64Vec3::new(x * 32, 0, z * 32),
                mesh: mesh(x, z),
            }));
            live = want;
            let frames = if step == 2 { 160 } else { 80 };
            let mut at_60 = None;
            let mut pending = ops.chunks(512).map(<[ChunkOp]>::to_vec).collect::<Vec<_>>();
            pending.reverse();
            for k in 0..frames {
                let ops = pending.pop().unwrap_or_default();
                let resync = if first { Resync::Chunks } else { Resync::None };
                first = false;
                let (_, refused) =
                    plugin_frame(&device, &queue, &mut paged, ops, resync, &view, false)?;
                if refused != 0 {
                    return Err(format!("step {step} frame {k}: {refused} refused"));
                }
                // a second a frame once the step down is in: shrink's holds pass
                paged.debug_advance_clock(std::time::Duration::from_millis(if step == 2 {
                    1000
                } else {
                    100
                }));
                peak = peak.max(paged.chunk_memory().pages);
                if k == 60 {
                    at_60 = Some(paged.chunk_memory().emergency_pages);
                }
            }
            let memory = paged.chunk_memory();
            if at_60 != Some(memory.emergency_pages) {
                return Err(format!(
                    "step {step}: pages made inside frames after frame 60: {at_60:?} then {memory:?}"
                ));
            }
        }
        let memory = paged.chunk_memory();
        let stats = paged.stats();
        if stats.dropped_chunks != 0
            || peak <= floor.pages
            || memory.pages != floor.pages
            || memory.capacity_quads != floor.capacity_quads
            || memory.pages_released == 0
            || memory.max_relocated_frame > paged.debug_move_budget()
        {
            return Err(format!(
                "the swing: peak {peak} pages, floor {floor:?}, now {memory:?}, dropped {}",
                stats.dropped_chunks
            ));
        }
        no_errors(&errors)
    });
}
