//! Box faces that carry their box's quarter turns (ADR 0052) on every tier. Self-skips without
//! an adapter unless `BOXEL_REQUIRE_GPU=1`.
//!
//! A stone floor meshed by the real `Mesher` with a grid of test blocks, each model in its four
//! facings (turned around Y): a step with uvlock texture offsets, a cutout post whose faces crop
//! another layer and hide one face, the step drawn full bright, and the step turned around X
//! first (a six-facing block, which shares nothing but still turns). The mesh is drawn twice:
//! as the mesher makes it (each turned box is its entry plus the quad's turns) and as the
//! engine drew it before (each turned copy its own table entry, no turns). For every adapter
//! (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local capability check
//! allows (the one of `gpu_tiers.rs`), asserts:
//!
//! - the two draw exactly the same pixels;
//! - every block is drawn, and the full bright step is brighter than the plain one on a side;
//! - every tier draws the same pixels as the best tier of its adapter (0.1 % of the pixels,
//!   channel tolerance 2).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_box_turns/` (every image with
//! `GPU_BOX_TURNS_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{
    ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher, PackedQuad, layout, quad_model_box,
};
use boxel_render::{DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, ViewParams, VoxelRenderer};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockState, BoxFaceTexture, BoxModel,
    ChunkData, Face, FaceTextures, LocalPos, ModelBox, RenderShape, SlabHalf, TableEntry,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_DIFF: f64 = 0.001;
/// The models, one row each (z = 13 + 2 * row), their four facings along x (11 + 2 * turns).
const MODELS: usize = 4;
/// The row of the plain step and of the full bright one.
const STEP: usize = 0;
const BRIGHT: usize = 2;

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
        label: Some("gpu_box_turns"),
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

/// The grid's blocks: `blocks[row][turns]`, on the floor (y = 9).
struct Grid {
    registry: BlockRegistry,
    blocks: Vec<[BlockState; 4]>,
    /// Each row's model, unturned.
    models: Vec<BoxModel>,
}

fn cell(row: usize, turns: usize) -> [u32; 3] {
    [11 + 2 * turns as u32, 9, 13 + 2 * row as u32]
}

/// The step: a bottom slab and a raised half along +X with uvlock offsets, and a small knob
/// off the middle (so no facing looks like another).
fn step() -> Vec<ModelBox> {
    vec![
        ModelBox::slab(SlabHalf::Bottom),
        ModelBox {
            texture_offset: [3, 0, -5],
            ..ModelBox::new([8, 8, 0], [16, 16, 16])
        },
        ModelBox::new([1, 8, 2], [4, 12, 7]),
    ]
}

/// The demo blocks and the grid's.
fn grid() -> Grid {
    let demo = boxel_demo::blocks::registry();
    let face = |name: &str| {
        let s = demo.by_name(name).expect(name);
        demo.face_texture(s, Face::PosX)
    };
    let (planks, stone) = (face("boxel:planks"), face("boxel:stone"));
    let mut b = boxel_demo::blocks::builder();
    let def = |name: String, shape: RenderShape, model: BlockModel| BlockDef {
        name,
        shape,
        model,
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(planks),
    };
    let steps = BoxModel::from_boxes(step());
    // A post off the middle whose sides crop the stone, its top another texel of it, its -X
    // side hidden.
    let mut faces = [BoxFaceTexture::layer(stone).at(1, 2); 6];
    faces[Face::PosY.index()] = BoxFaceTexture::layer(stone).at(5, 9);
    faces[Face::NegX.index()] = BoxFaceTexture::HIDDEN;
    let post =
        BoxModel::from_boxes(vec![ModelBox::new([2, 0, 3], [7, 11, 6])]).with_textures(vec![faces]);
    let models = [
        (RenderShape::Opaque, steps.clone()),
        (RenderShape::Cutout, post),
        (RenderShape::Opaque, steps.clone()),
        (RenderShape::Opaque, steps.turned_x(1)),
    ];
    let mut blocks = Vec::new();
    let mut unturned = Vec::new();
    for (row, (shape, model)) in models.into_iter().enumerate() {
        let facings = std::array::from_fn(|turns| {
            let id = b.add_box_model(model.turned(turns as u8)).expect("model");
            let name = format!("test:turned_{row}_{turns}");
            let s = b
                .add_block(def(name.clone(), shape, BlockModel::Boxes(id)))
                .expect("block");
            if row == BRIGHT {
                let block = b.block_by_name(&name).expect("block");
                b.set_full_bright(block, |_| true).expect("bright");
            }
            s
        });
        blocks.push(facings);
        unturned.push(model);
    }
    Grid {
        registry: b.build(),
        blocks,
        models: unturned,
    }
}

/// The stone floor (y = 8, x and z in 4..28), with the grid on it when `with_grid`, meshed.
fn scene(grid: &Grid, materials: &MaterialTable, with_grid: bool) -> ChunkMesh {
    let stone = grid.registry.by_name("boxel:stone").expect("stone");
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for x in 4..28 {
        for z in 4..28 {
            dense[LocalPos::new(x, 8, z).index()] = stone;
        }
    }
    if with_grid {
        for (row, facings) in grid.blocks.iter().enumerate() {
            for (turns, &s) in facings.iter().enumerate() {
                let [x, y, z] = cell(row, turns);
                dense[LocalPos::new(x, y, z).index()] = s;
            }
        }
    }
    let chunk = ChunkData::from_dense(&dense);
    let input = MeshInput {
        center: &chunk,
        neighbors: [None; 26],
        light: None,
    };
    Mesher::new().mesh_chunk(&input, materials, MeshOptions::default())
}

/// `mesh` as the engine drew it before ADR 0052: every box face indexes an entry of its own
/// turned box (appended to a copy of `table`) with no turns.
fn unshared(mesh: &ChunkMesh, table: &[TableEntry]) -> (ChunkMesh, Vec<TableEntry>) {
    let mut entries = table.to_vec();
    let mut ids: HashMap<TableEntry, u16> = entries
        .iter()
        .enumerate()
        .map(|(i, &e)| (e, i as u16))
        .collect();
    let mut out = mesh.clone();
    for q in &mut out.quads {
        let Some(b) = quad_model_box(*q, table) else {
            continue;
        };
        let entry = TableEntry::Box(b);
        let index = *ids.entry(entry).or_insert_with(|| {
            entries.push(entry);
            (entries.len() - 1) as u16
        });
        *q = PackedQuad::model_box_face(
            q.pos(),
            q.face(),
            q.layer(),
            index,
            0,
            q.is_emissive(),
            q.ao_byte(),
            q.light_index(),
        );
    }
    out.hash = out.compute_hash();
    (out, entries)
}

fn view() -> ViewParams {
    let eye = DVec3::new(14.5, 12.5, 9.5);
    let transform = Transform::from_yaw_pitch(eye, std::f32::consts::PI, -0.55);
    let camera = Camera::default();
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
}

/// Where the world point `p` lands on the image, in pixels.
fn project(view: &ViewParams, p: DVec3) -> (u32, u32) {
    let rel = (p - view.render_origin.as_dvec3()).as_vec3();
    let clip = view.view_proj * rel.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    let x = (ndc.x * 0.5 + 0.5) * WIDTH as f32;
    let y = (0.5 - ndc.y * 0.5) * HEIGHT as f32;
    (x as u32, y as u32)
}

/// The middle of the north (-Z) side of the slab of the step in `cell`, facing the camera.
fn slab_side(cell: [u32; 3]) -> DVec3 {
    DVec3::new(
        f64::from(cell[0]) + 0.5,
        f64::from(cell[1]) + 0.25,
        f64::from(cell[2]) + 0.001,
    )
}

// ---------------------------------------------------------------- rendering

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_box_turns"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// The floor alone, the grid as meshed and the grid as drawn before.
struct TierImages {
    floor: RgbaImage,
    shared: RgbaImage,
    unshared: RgbaImage,
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    grid: &Grid,
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let materials = MaterialTable::from_registry(&grid.registry);
    let config = RendererConfig {
        chunk_memory: boxel_render::ChunkMemorySettings {
            initial_quads: 1 << 16,
            initial_slots: 64,
            ..boxel_render::ChunkMemorySettings::default()
        },
        ..RendererConfig::default()
    };
    let mut renderer = VoxelRenderer::new(
        &device,
        &queue,
        tier,
        &config,
        &boxel_render::generate_block_textures(grid.registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    let view = view();
    renderer.set_model_boxes(&queue, materials.model_boxes());
    renderer
        .upsert_chunk(1, I64Vec3::ZERO, Arc::new(scene(grid, &materials, false)))
        .map_err(|e| e.to_string())?;
    let floor = render(&device, &queue, &mut renderer, &view)?;
    let mesh = scene(grid, &materials, true);
    let (old, old_table) = unshared(&mesh, materials.model_boxes());
    renderer
        .upsert_chunk(1, I64Vec3::ZERO, Arc::new(mesh))
        .map_err(|e| e.to_string())?;
    let shared = render(&device, &queue, &mut renderer, &view)?;
    renderer.set_model_boxes(&queue, &old_table);
    renderer
        .upsert_chunk(1, I64Vec3::ZERO, Arc::new(old))
        .map_err(|e| e.to_string())?;
    let unshared = render(&device, &queue, &mut renderer, &view)?;
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(TierImages {
        floor,
        shared,
        unshared,
    })
}

// ---------------------------------------------------------------- checks

struct Report {
    failures: Vec<String>,
    dir: PathBuf,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_box_turns"),
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
                Ok(()) => eprintln!("gpu_box_turns: saved {}", path.display()),
                Err(e) => eprintln!("gpu_box_turns: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_box_turns: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }
}

fn pixel(image: &RgbaImage, (x, y): (u32, u32)) -> [u8; 4] {
    let i = ((y * image.width + x) * 4) as usize;
    [
        image.data[i],
        image.data[i + 1],
        image.data[i + 2],
        image.data[i + 3],
    ]
}

/// True if the pixel at `at` differs between `a` and `b` by more than 8 in a channel.
fn drawn(a: &RgbaImage, b: &RgbaImage, at: (u32, u32)) -> bool {
    let (p, q) = (pixel(a, at), pixel(b, at));
    (0..3).any(|c| p[c].abs_diff(q[c]) > 8)
}

fn check_tier(report: &mut Report, what: &str, grid: &Grid, i: &TierImages) {
    let view = view();
    let images = [
        ("floor", &i.floor),
        ("shared", &i.shared),
        ("unshared", &i.unshared),
    ];
    let diff = i.shared.compare(&i.unshared, 0);
    if diff.fraction() > 0.0 {
        report.fail(
            format!(
                "{what}: shared turns vs turned copies: {:.4} % of pixels differ, max channel {}",
                diff.fraction() * 100.0,
                diff.max_channel_diff
            ),
            &images,
        );
    }
    for row in 0..MODELS {
        for turns in 0..4 {
            // The middle of the facing's first box.
            let [x, y, z] = cell(row, turns);
            let first = grid.models[row].turned(turns as u8).boxes[0].bounds;
            let mid = |a: usize| f64::from(first.min[a] + first.max[a]) / 32.0;
            let at = project(
                &view,
                DVec3::new(
                    f64::from(x) + mid(0),
                    f64::from(y) + mid(1),
                    f64::from(z) + mid(2),
                ),
            );
            if !drawn(&i.floor, &i.shared, at) {
                report.fail(
                    format!("{what}: block {row} turned {turns} not drawn at {at:?}"),
                    &images,
                );
            }
        }
    }
    let sum = |at| {
        pixel(&i.shared, at)[..3]
            .iter()
            .map(|&c| u32::from(c))
            .sum::<u32>()
    };
    for turns in 0..4 {
        let plain = sum(project(&view, slab_side(cell(STEP, turns))));
        let bright = sum(project(&view, slab_side(cell(BRIGHT, turns))));
        if bright <= plain + 24 {
            report.fail(
                format!(
                    "{what}: full bright step turned {turns} not brighter ({bright} vs {plain})"
                ),
                &images,
            );
        }
    }
}

#[test]
fn turned_boxes_draw_as_their_turned_copies_on_every_tier() {
    let grid = grid();
    // The mesh really shares: turned box faces, and fewer entries than the turned copies need.
    let materials = MaterialTable::from_registry(&grid.registry);
    let mesh = scene(&grid, &materials, true);
    let turned = mesh
        .quads
        .iter()
        .filter(|q| q.shape() == layout::SHAPE_BOX && q.box_turn() != 0)
        .count();
    let bright = mesh
        .quads
        .iter()
        .filter(|q| q.shape() == layout::SHAPE_BOX && q.is_emissive() && q.box_turn() != 0)
        .count();
    assert!(turned > 0 && bright > 0, "{turned} turned, {bright} bright");
    let (_, old_table) = unshared(&mesh, materials.model_boxes());
    assert!(
        old_table.len() > materials.model_boxes().len() + 8,
        "{} entries before, {} now",
        old_table.len(),
        materials.model_boxes().len()
    );
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let mut report = Report::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let mut tiers = Vec::new();
        for tier in allowed_tiers(&caps) {
            match render_tier(adapter, &caps, tier, &grid) {
                Ok(images) => tiers.push((tier, images)),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        for (tier, i) in &tiers {
            rendered += 1;
            let what = format!("{label}_{tier}");
            if std::env::var_os("GPU_BOX_TURNS_SAVE").is_some() {
                report.save(&[
                    (&format!("{what}_floor"), &i.floor),
                    (&format!("{what}_shared"), &i.shared),
                    (&format!("{what}_unshared"), &i.unshared),
                ]);
            }
            check_tier(&mut report, &what, &grid, i);
        }
        let Some(((best, b), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, t) in rest {
            let diff = b.shared.compare(&t.shared, 2);
            eprintln!(
                "gpu_box_turns: {label}: {tier} vs {best}: {:.4} % of pixels differ, max channel {}",
                diff.fraction() * 100.0,
                diff.max_channel_diff
            );
            if diff.fraction() > MAX_DIFF {
                report.fail(
                    format!(
                        "{label}: {tier} vs {best}: {:.4} % of pixels differ",
                        diff.fraction() * 100.0
                    ),
                    &[
                        (&format!("{label}_{best}_shared"), &b.shared),
                        (&format!("{label}_{tier}_shared"), &t.shared),
                    ],
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
