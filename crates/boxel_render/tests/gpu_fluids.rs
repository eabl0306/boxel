//! Fluid faces against the renderer's culling and depth test (ADR 0025) on every tier.
//! Self-skips without an adapter unless `BOXEL_REQUIRE_GPU=1`.
//!
//! Scenes meshed by the real `Mesher`, no light:
//!
//! - lava falling onto a ledge, its sloping top in the bottom layer of a chunk, seen from below
//!   that chunk on the downhill side: the renderer skips a chunk's +Y bucket for such an eye, so
//!   the top must not be in it. The pixels around the screen center (the top's middle) differ
//!   from the same view without the lava (the sky behind it).
//! - depth ties: leaves beside lava (the leaves' far side and the lava's face share a plane),
//!   and a waterlogged fence and stairs between full water (their water's top at 16/16 shares
//!   the plane of the post's or step's top). The solid's quads drawn alone, on two clear colors,
//!   cover the pixels that do not change; inside them (one pixel in from their edge) the full
//!   scene shows exactly those quads: the fluid never wins a tie.
//! - waterlogged roots (`test:roots`, a cutout block of one full box, ADR 0038): in full water
//!   their water's top at 16/16 ties with the roots' top and loses, as above; in a pool their
//!   water shows through the holes of their texture (the square around the screen center
//!   differs from the same pool with dry roots).
//! - waterlogged leaves (`test:wet_leaves`, a cutout cube with the same texture, ADR 0038): in
//!   full water their top wins the tie with their water's; in a pool and alone in the air their
//!   water shows through the holes (against the same scenes with dry leaves).
//!
//! For every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local
//! capability check allows (the one of `gpu_tiers.rs`); every tier also draws the full scenes as
//! the best tier of its adapter does (0.1 % of the pixels, channel tolerance 2). Failing images
//! are saved under `CARGO_TARGET_TMPDIR/gpu_fluids/` (every image with `GPU_FLUIDS_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, IVec3, UVec2};
use boxel_mesh::{
    BUCKET_COUNT, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher, PackedQuad, layout,
};
use boxel_render::{
    DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, TranslucentSort, ViewParams, VoxelRenderer,
};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockState, BoxModel, ChunkData, Face,
    FaceTextures, LocalPos, ModelBox, NEIGHBOR_OFFSETS, Property, RenderShape, TextureImage,
    TextureSpec, WATERLOGGED,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Tiers may differ in at most this fraction of the pixels (channel tolerance 2).
const MAX_TIER_DIFF: f64 = 0.001;
/// The two clear colors that tell the pixels a set of quads covers.
const CLEARS: [[f32; 4]; 2] = [[1.0, 0.0, 1.0, 1.0], [0.0, 1.0, 0.0, 1.0]];
/// Half the side of the square around the screen center a `Drawn` check looks at.
const CENTER: u32 = 2;
/// A `Wins` check needs at least this many pixels covered by its quads (after the erosion).
const MIN_COVERED: usize = 50;

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
        label: Some("gpu_fluids"),
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

/// The demo blocks plus `test:roots`, waterloggable roots: cutout, one full box, a frame
/// texture (3 opaque texels around a clear middle, so the eye sees into the cell), and
/// `test:wet_leaves`, the same as a cutout cube (leaves that hold water). The demo keeps its
/// ids.
fn registry() -> BlockRegistry {
    let mut b = boxel_demo::blocks::builder();
    let edge = 16;
    let texels: Vec<[u8; 4]> = (0..edge * edge)
        .map(|i| {
            let (x, y) = (i % edge, i / edge);
            let frame = x.min(y).min(edge - 1 - x).min(edge - 1 - y) < 3;
            if frame { [120, 80, 50, 255] } else { [0; 4] }
        })
        .collect();
    let frame = b
        .add_texture(TextureSpec {
            name: "test:roots".to_string(),
            image: TextureImage {
                size: edge as u32,
                texels: texels.into(),
                binary_alpha: true,
            },
        })
        .expect("test:roots texture");
    let full = b
        .add_box_model(BoxModel::from_boxes(vec![ModelBox::new([0; 3], [16; 3])]))
        .expect("full box");
    let roots = BlockDef {
        name: "test:roots".to_string(),
        shape: RenderShape::Cutout,
        model: BlockModel::Boxes(full),
        collision: BlockCollision::Full,
        emission: [0; 3],
        light_opacity: 1,
        textures: FaceTextures::all(frame),
    };
    let leaves = BlockDef {
        name: "test:wet_leaves".to_string(),
        model: BlockModel::CUBE,
        ..roots.clone()
    };
    for def in [roots, leaves] {
        let name = def.name.clone();
        b.add_block_with_states(def, vec![Property::boolean(WATERLOGGED)], |_, _| {})
            .expect(&name);
    }
    b.build()
}

/// Chunks of a scene: (world voxel origin, mesh).
type Chunks = Vec<(I64Vec3, ChunkMesh)>;

/// The real mesher's meshes of chunks `chunks` (chunk coordinates) of the world `block`, with
/// all their neighbors, no light.
fn mesh_world(
    registry: &BlockRegistry,
    block: &dyn Fn(IVec3) -> BlockState,
    chunks: &[IVec3],
) -> Chunks {
    let data = |c: IVec3| {
        let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
        for (i, s) in dense.iter_mut().enumerate() {
            let p = LocalPos::from_index(i);
            *s = block(c * 32 + IVec3::new(p.x() as i32, p.y() as i32, p.z() as i32));
        }
        ChunkData::from_dense(&dense)
    };
    let materials = MaterialTable::from_registry(registry);
    let mut mesher = Mesher::new();
    chunks
        .iter()
        .map(|&c| {
            let center = data(c);
            let around: Vec<ChunkData> = NEIGHBOR_OFFSETS.iter().map(|&d| data(c + d)).collect();
            let input = MeshInput {
                center: &center,
                neighbors: std::array::from_fn(|i| Some(&around[i])),
                light: None,
            };
            let mesh = mesher.mesh_chunk(&input, &materials, MeshOptions::default());
            (c.as_i64vec3() * 32, mesh)
        })
        .collect()
}

/// `mesh` with only the quads `keep` keeps (same buckets and palette).
fn only(mesh: &ChunkMesh, keep: &dyn Fn(PackedQuad) -> bool) -> ChunkMesh {
    let mut quads = Vec::new();
    let mut counts = [0; BUCKET_COUNT];
    for (b, count) in counts.iter_mut().enumerate() {
        for &q in mesh.bucket(b) {
            if keep(q) {
                quads.push(q);
                *count += 1;
            }
        }
    }
    let mut out = ChunkMesh {
        quads,
        bucket_counts: counts,
        palette: mesh.palette.clone(),
        ..ChunkMesh::default()
    };
    out.hash = out.compute_hash();
    out
}

/// What a scene checks against its full image.
enum Check {
    /// The square around the screen center differs from these chunks' image (the scene without
    /// a quad that must show there).
    Drawn(Chunks),
    /// Every pixel these chunks' quads cover (drawn alone) shows the same in the full image.
    Wins(Chunks),
}

struct Scene {
    name: &'static str,
    chunks: Chunks,
    check: Check,
    eye: DVec3,
    target: DVec3,
}

fn scenes(registry: &BlockRegistry) -> Vec<Scene> {
    let get = |name: &str| registry.parse_state(name).expect(name);
    let air = BlockState::AIR;
    let stone = get("boxel:stone");
    let leaves = get("boxel:leaves");
    let water = get("boxel:water");
    let lava = |l: u8| get(&format!("boxel:lava[level={l}]"));
    let materials = MaterialTable::from_registry(registry);

    // Lava falls down a column at (8, 32..=34, 8) onto a stone ledge (y 31, x up to 11) and
    // runs one block east: the cell (9, 32, 8) is the bottom layer of chunk (0, 1, 0), its top
    // slopes from 16/16 at the column down to 4/16 (0.75 a block). From (14, 31.8, 8.5), below
    // that chunk and past the ledge, the eye is above the top's plane and sees its middle.
    let ledge = move |p: IVec3, with_lava: bool| match (p.x, p.y, p.z) {
        (0..=11, 31, 4..=12) => stone,
        (8, 32 | 33, 8) if with_lava => lava(8),
        (8, 34, 8) if with_lava => lava(0),
        (9, 32, 8) if with_lava => lava(2),
        _ => air,
    };
    let ledge_chunks = [IVec3::ZERO, IVec3::Y];
    let slope = mesh_world(registry, &|p| ledge(p, true), &ledge_chunks);
    let bare = mesh_world(registry, &|p| ledge(p, false), &ledge_chunks);

    // Leaves next to lava sources: the leaves' far side (x = 12) is in the lava's plane.
    let wall = mesh_world(
        registry,
        &|p| match (p.x, p.y, p.z) {
            (12, 8..=10, 8..=10) => leaves,
            (11, 8..=10, 8..=10) => lava(0),
            _ => air,
        },
        &[IVec3::ZERO],
    );
    let leaves_layer = registry.face_texture(leaves, Face::PosX).0;
    let leaves_only = |chunks: &Chunks| -> Chunks {
        chunks
            .iter()
            .map(|(o, m)| (*o, only(m, &|q| q.layer() == leaves_layer)))
            .collect()
    };

    // A waterlogged block at (8, 8, 8) between two columns of water two high: the water on
    // both sides is full, so its own water's 4 corners are at 16/16, in the plane of the tops of
    // its boxes that reach the cell's top.
    let pool = |block: BlockState| {
        mesh_world(
            registry,
            &move |p| match (p.x, p.y, p.z) {
                (8, 8, 8) => block,
                (7 | 9, 8 | 9, 8) => water,
                _ => air,
            },
            &[IVec3::ZERO],
        )
    };
    let tops = move |chunks: &Chunks| -> Chunks {
        let boxes = materials.model_boxes().to_vec();
        chunks
            .iter()
            .map(|(o, m)| {
                let keep = |q: PackedQuad| {
                    q.shape() == layout::SHAPE_BOX
                        && q.face() == Face::PosY
                        && boxes[usize::from(q.box_index())].bounds.max[1] == 16
                };
                (*o, only(m, &keep))
            })
            .collect()
    };
    let fence = pool(get("boxel:oak_fence[waterlogged=true]"));
    let stairs = pool(get("boxel:stone_stairs[facing=north,waterlogged=true]"));
    let wet_roots = get("test:roots[waterlogged=true]");
    let full_roots = pool(wet_roots);
    let above = DVec3::new(8.5, 11.5, 8.6);
    let middle = DVec3::new(8.5, 9.0, 8.5);

    // Roots at (8, 8, 8) in a 3x3 pool on a stone floor: their water's top (14/16, level with
    // the pool) shows through the middle of the roots' top. Dry, the floor shows there.
    let roots_pool = |roots: BlockState| {
        mesh_world(
            registry,
            &move |p| match (p.x, p.y, p.z) {
                (8, 8, 8) => roots,
                (7..=9, 8, 7..=9) => water,
                (7..=9, 7, 7..=9) => stone,
                _ => air,
            },
            &[IVec3::ZERO],
        )
    };
    let dry_roots = registry.with_water(wet_roots, false).expect("dry roots");
    // Leaves at (8, 8, 8), the same scenes, and alone in the air: their water's top (14/16 in
    // the pool, 12/16 alone) shows through the middle of their top. Dry, the floor or the sky
    // shows there.
    let wet_leaves = get("test:wet_leaves[waterlogged=true]");
    let dry_leaves = registry.with_water(wet_leaves, false).expect("dry leaves");
    let full_leaves = pool(wet_leaves);
    let leaves_top = |chunks: &Chunks| -> Chunks {
        let keep = |q: PackedQuad| {
            q.shape() == layout::SHAPE_CUBE && q.face() == Face::PosY && q.pos() == [8, 8, 8]
        };
        chunks.iter().map(|(o, m)| (*o, only(m, &keep))).collect()
    };
    let alone = |block: BlockState| {
        let at = move |p: IVec3| if p == IVec3::splat(8) { block } else { air };
        mesh_world(registry, &at, &[IVec3::ZERO])
    };
    vec![
        Scene {
            name: "sloping lava top from below its chunk",
            check: Check::Drawn(bare),
            chunks: slope,
            eye: DVec3::new(14.0, 31.8, 8.5),
            target: DVec3::new(9.5, 32.625, 8.5),
        },
        Scene {
            name: "leaves beside lava",
            check: Check::Wins(leaves_only(&wall)),
            chunks: wall,
            eye: DVec3::new(17.5, 9.6, 9.7),
            target: DVec3::new(12.5, 9.5, 9.5),
        },
        Scene {
            name: "waterlogged fence in full water",
            check: Check::Wins(tops(&fence)),
            chunks: fence,
            eye: above,
            target: middle,
        },
        Scene {
            name: "waterlogged stairs in full water",
            check: Check::Wins(tops(&stairs)),
            chunks: stairs,
            eye: above,
            target: middle,
        },
        Scene {
            name: "waterlogged roots in full water",
            check: Check::Wins(tops(&full_roots)),
            chunks: full_roots,
            eye: above,
            target: middle,
        },
        Scene {
            name: "waterlogged roots in a pool",
            check: Check::Drawn(roots_pool(dry_roots)),
            chunks: roots_pool(wet_roots),
            eye: above,
            target: middle,
        },
        Scene {
            name: "waterlogged leaves in full water",
            check: Check::Wins(leaves_top(&full_leaves)),
            chunks: full_leaves,
            eye: above,
            target: middle,
        },
        Scene {
            name: "waterlogged leaves in a pool",
            check: Check::Drawn(roots_pool(dry_leaves)),
            chunks: roots_pool(wet_leaves),
            eye: above,
            target: middle,
        },
        Scene {
            name: "waterlogged leaves alone",
            check: Check::Drawn(alone(dry_leaves)),
            chunks: alone(wet_leaves),
            eye: above,
            target: middle,
        },
    ]
}

/// A camera at `eye` looking at `target`, the sky cleared to `clear`.
fn look(eye: DVec3, target: DVec3, clear: [f32; 4]) -> ViewParams {
    let d = (target - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    let mut view =
        ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default());
    view.clear_color = clear;
    view
}

// ---------------------------------------------------------------- rendering

/// Shows `chunks` alone and renders one frame of `view`, reading the main color back.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    chunks: &Chunks,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    renderer.clear_chunks();
    for (i, (origin, mesh)) in chunks.iter().enumerate() {
        renderer
            .upsert_chunk(i as u64, *origin, Arc::new(mesh.clone()))
            .map_err(|e| e.to_string())?;
    }
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_fluids"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// Images of one scene on one tier: the full scene, then its check's chunks on each clear
/// color (`Drawn`: the first only).
struct SceneImages {
    full: RgbaImage,
    check: Vec<RgbaImage>,
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    registry: &BlockRegistry,
    scenes: &[Scene],
) -> Result<Vec<SceneImages>, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        chunk_memory: boxel_render::ChunkMemorySettings {
            initial_quads: 1 << 16,
            initial_slots: 64,
            ..boxel_render::ChunkMemorySettings::default()
        },
        translucent_sort: TranslucentSort::Inline,
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
    renderer.set_model_boxes(&queue, MaterialTable::from_registry(registry).model_boxes());
    let mut out = Vec::new();
    for scene in scenes {
        let view = |clear| look(scene.eye, scene.target, clear);
        let full = render(
            &device,
            &queue,
            &mut renderer,
            &scene.chunks,
            &view(CLEARS[0]),
        )?;
        let (chunks, clears) = match &scene.check {
            Check::Drawn(chunks) => (chunks, &CLEARS[..1]),
            Check::Wins(chunks) => (chunks, &CLEARS[..]),
        };
        let check = clears
            .iter()
            .map(|&clear| render(&device, &queue, &mut renderer, chunks, &view(clear)))
            .collect::<Result<_, _>>()?;
        out.push(SceneImages { full, check });
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(out)
}

// ---------------------------------------------------------------- checks

/// Largest channel difference of pixel `(x, y)` between `a` and `b`.
fn pixel_diff(a: &RgbaImage, b: &RgbaImage, x: u32, y: u32) -> u8 {
    let i = ((y * a.width + x) * 4) as usize;
    (0..3)
        .map(|c| a.data[i + c].abs_diff(b.data[i + c]))
        .max()
        .unwrap_or(0)
}

/// The pixels the quads of `on` (one image per clear color) cover, one pixel in from their
/// edge: the pixel and its 4 neighbors keep their color on both clears.
fn covered(on: &[RgbaImage]) -> Vec<(u32, u32)> {
    let same = |x: u32, y: u32| pixel_diff(&on[0], &on[1], x, y) <= 2;
    let mut out = Vec::new();
    for y in 1..HEIGHT - 1 {
        for x in 1..WIDTH - 1 {
            if same(x, y) && same(x - 1, y) && same(x + 1, y) && same(x, y - 1) && same(x, y + 1) {
                out.push((x, y));
            }
        }
    }
    out
}

/// Collects failures and saves the images involved.
struct Report {
    failures: Vec<String>,
    dir: PathBuf,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_fluids"),
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
                Ok(()) => eprintln!("gpu_fluids: saved {}", path.display()),
                Err(e) => eprintln!("gpu_fluids: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_fluids: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }

    /// The scene's check on one tier.
    fn check(&mut self, what: &str, scene: &Scene, images: &SceneImages) {
        let names = [0, 1].map(|k| format!("{what}_check{k}"));
        let full_name = format!("{what}_full");
        let full = (full_name.as_str(), &images.full);
        let reference = (names[0].as_str(), &images.check[0]);
        match scene.check {
            Check::Drawn(_) => {
                let (cx, cy) = (WIDTH / 2, HEIGHT / 2);
                let mut same = 0;
                for y in cy - CENTER..=cy + CENTER {
                    for x in cx - CENTER..=cx + CENTER {
                        same += usize::from(pixel_diff(full.1, reference.1, x, y) <= 16);
                    }
                }
                eprintln!("gpu_fluids: {what}: {same} center pixels show what is behind");
                if same > 0 {
                    self.fail(
                        format!("{what}: {same} pixels at the center miss the quad"),
                        &[full, reference],
                    );
                }
            }
            Check::Wins(_) => {
                let pixels = covered(&images.check);
                let lost = pixels
                    .iter()
                    .filter(|&&(x, y)| pixel_diff(full.1, reference.1, x, y) > 2)
                    .count();
                eprintln!(
                    "gpu_fluids: {what}: the fluid covers {lost} of {} pixels",
                    pixels.len()
                );
                if pixels.len() < MIN_COVERED || lost > 0 {
                    let other = (names[1].as_str(), &images.check[1]);
                    self.fail(
                        format!(
                            "{what}: {lost} of {} covered pixels changed (at least \
                             {MIN_COVERED} covered)",
                            pixels.len()
                        ),
                        &[full, reference, other],
                    );
                }
            }
        }
    }
}

#[test]
fn fluids_lose_depth_ties_and_their_sloping_tops_show_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let registry = registry();
    let scenes = scenes(&registry);
    let mut report = Report::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let mut tiers = Vec::new();
        for tier in allowed_tiers(&caps) {
            match render_tier(adapter, &caps, tier, &registry, &scenes) {
                Ok(images) => tiers.push((tier, images)),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        for (tier, images) in &tiers {
            rendered += 1;
            for (scene, i) in scenes.iter().zip(images) {
                let what = format!("{label}_{tier}_{}", scene.name);
                if std::env::var_os("GPU_FLUIDS_SAVE").is_some() {
                    report.save(&[(&format!("{what}_full"), &i.full)]);
                    for (k, image) in i.check.iter().enumerate() {
                        report.save(&[(&format!("{what}_check{k}"), image)]);
                    }
                }
                report.check(&what, scene, i);
            }
        }
        let Some(((best, best_images), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, images) in rest {
            for ((scene, a), b) in scenes.iter().zip(best_images).zip(images) {
                let diff = a.full.compare(&b.full, 2);
                if diff.fraction() > MAX_TIER_DIFF {
                    report.fail(
                        format!(
                            "{label}: {tier} vs {best} ({}): {} of {} pixels differ",
                            scene.name, diff.differing_pixels, diff.total_pixels
                        ),
                        &[
                            (&format!("{label}_{best}_{}", scene.name), &a.full),
                            (&format!("{label}_{tier}_{}", scene.name), &b.full),
                        ],
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
