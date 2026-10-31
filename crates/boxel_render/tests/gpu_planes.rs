//! Planes inside a box (EW-PLANE, ADR 0046) on every tier. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! A stone floor meshed by the real `Mesher` with a row of test blocks made of planes: a small
//! cross, a tilted plane, a square plane spun 45 degrees, an opaque post with a flat plane
//! over it and the tilted plane turned a quarter (a facing). For every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier
//! a local capability check allows (the one of `gpu_tiers.rs`), a `VoxelRenderer` draws the
//! floor alone and with the row. Asserts:
//!
//! - each plane covers the pixel of its center, as `ModelPlane::corners` puts it;
//! - the small cross leaves the top corner of its cell clear, and the spun square shows where
//!   the turn takes its corners and not where they were;
//! - every tier draws the same pixels as the best tier of its adapter (0.1 % of the pixels,
//!   channel tolerance 2).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_planes/` (every image with
//! `GPU_PLANES_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::{DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, ViewParams, VoxelRenderer};
use boxel_transform::Transform;
use boxel_voxel::{
    Axis, BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockState, BoxModel, ChunkData,
    Face, FaceTextures, LocalPos, ModelBox, ModelPlane, PlaneKind, RenderShape,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_DIFF: f64 = 0.001;

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
        label: Some("gpu_planes"),
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

/// The row's blocks, their cells (y = 9, on the floor) and planes.
struct Row {
    registry: BlockRegistry,
    blocks: Vec<([u32; 3], BlockState, Vec<ModelPlane>)>,
}

/// The small cross: in the middle of its cell, three quarters high.
const CROSS: [[u8; 3]; 2] = [[8, 0, 8], [24, 24, 24]];
/// The spun square, across Z in the middle of its cell, 16 thirty-seconds wide.
const SQUARE: [[u8; 3]; 2] = [[8, 8, 16], [24, 24, 16]];

/// The demo blocks and the row's: every plane shows the planks (opaque, so it covers what it
/// is drawn over).
fn row() -> Row {
    let demo = boxel_demo::blocks::registry();
    let planks = demo.by_name("boxel:planks").expect("planks");
    let planks = demo.face_texture(planks, Face::PosX);
    let mut b = boxel_demo::blocks::builder();
    let def = |name: &str, shape: RenderShape, model: BlockModel| BlockDef {
        name: name.to_string(),
        shape,
        model,
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(planks),
    };
    let cross = ModelPlane::cross(CROSS[0], CROSS[1]).to_vec();
    let tilted = vec![ModelPlane::new(
        PlaneKind::Rising(Axis::X),
        [0, 0, 0],
        [32, 16, 32],
    )];
    let spun = vec![ModelPlane::new(PlaneKind::Mid(Axis::Z), SQUARE[0], SQUARE[1]).spun(2)];
    let flat = vec![ModelPlane::new(
        PlaneKind::Mid(Axis::Y),
        [0, 24, 0],
        [32, 24, 32],
    )];
    // The tilted plane turned a quarter (a facing: its quad's face says so).
    let turned =
        vec![ModelPlane::new(PlaneKind::Rising(Axis::X), [0, 0, 0], [32, 16, 32]).turned(1)];
    let post = vec![ModelBox::new([7, 0, 7], [9, 12, 9])];
    let mut blocks = Vec::new();
    for (i, (name, shape, boxes, planes)) in [
        ("test:small_cross", RenderShape::Cutout, Vec::new(), cross),
        ("test:tilted", RenderShape::Cutout, Vec::new(), tilted),
        ("test:spun", RenderShape::Cutout, Vec::new(), spun),
        ("test:post", RenderShape::Opaque, post, flat),
        ("test:turned", RenderShape::Cutout, Vec::new(), turned),
    ]
    .into_iter()
    .enumerate()
    {
        let model = BoxModel::from_boxes(boxes).with_planes(planes.clone());
        let id = b.add_box_model(model).expect("model");
        let s = b
            .add_block(def(name, shape, BlockModel::Boxes(id)))
            .expect("block");
        blocks.push(([12 + 2 * i as u32, 9, 16], s, planes));
    }
    Row {
        registry: b.build(),
        blocks,
    }
}

/// The stone floor (y = 8, x and z in 4..28), with the row on it when `with_row`, meshed.
fn scene(row: &Row, materials: &MaterialTable, with_row: bool) -> ChunkMesh {
    let stone = row.registry.by_name("boxel:stone").expect("stone");
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for x in 4..28 {
        for z in 4..28 {
            dense[LocalPos::new(x, 8, z).index()] = stone;
        }
    }
    if with_row {
        for &([x, y, z], s, _) in &row.blocks {
            dense[LocalPos::new(x, y, z).index()] = s;
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

fn view() -> ViewParams {
    let eye = DVec3::new(16.5, 11.0, 12.0);
    let transform = Transform::from_yaw_pitch(eye, std::f32::consts::PI, -0.35);
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

// ---------------------------------------------------------------- rendering

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_planes"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// The floor alone and with the row.
struct TierImages {
    floor: RgbaImage,
    row: RgbaImage,
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    row: &Row,
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let materials = MaterialTable::from_registry(&row.registry);
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
        &boxel_render::generate_block_textures(row.registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    renderer.set_model_boxes(&queue, materials.model_boxes());
    let view = view();
    renderer
        .upsert_chunk(1, I64Vec3::ZERO, Arc::new(scene(row, &materials, false)))
        .map_err(|e| e.to_string())?;
    let floor = render(&device, &queue, &mut renderer, &view)?;
    renderer
        .upsert_chunk(1, I64Vec3::ZERO, Arc::new(scene(row, &materials, true)))
        .map_err(|e| e.to_string())?;
    let with_row = render(&device, &queue, &mut renderer, &view)?;
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(TierImages {
        floor,
        row: with_row,
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
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_planes"),
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
                Ok(()) => eprintln!("gpu_planes: saved {}", path.display()),
                Err(e) => eprintln!("gpu_planes: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_planes: FAIL {message}");
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

/// The world point of the plane point at `(u, v)` of `plane` (corner weights) in `cell`.
fn on_plane(cell: [u32; 3], plane: &ModelPlane, (u, v): (f32, f32)) -> DVec3 {
    let c = plane.corners().map(|c| c.position);
    let w = [(1.0 - u) * (1.0 - v), u * (1.0 - v), (1.0 - u) * v, u * v];
    let p = (0..3).map(|a| (0..4).map(|k| c[k][a] * w[k]).sum::<f32>());
    let p: Vec<f32> = p.collect();
    DVec3::new(
        f64::from(cell[0]) + f64::from(p[0]),
        f64::from(cell[1]) + f64::from(p[1]),
        f64::from(cell[2]) + f64::from(p[2]),
    )
}

fn check_tier(report: &mut Report, what: &str, row: &Row, i: &TierImages) {
    let view = view();
    let images = [("floor", &i.floor), ("row", &i.row)];
    for (cell, _, planes) in &row.blocks {
        for plane in planes {
            // The center and a point near each corner.
            for uv in [(0.5, 0.5), (0.2, 0.2), (0.8, 0.8)] {
                let at = project(&view, on_plane(*cell, plane, uv));
                if !drawn(&i.floor, &i.row, at) {
                    report.fail(
                        format!("{what}: {plane:?} in {cell:?} not drawn at {uv:?} {at:?}"),
                        &images,
                    );
                }
            }
        }
    }
    // The small cross leaves its cell's top corner clear (the full cell's cross would not).
    let [cross_cell, ..] = [row.blocks[0].0];
    let corner = DVec3::new(
        f64::from(cross_cell[0]) + 0.97,
        f64::from(cross_cell[1]) + 0.95,
        f64::from(cross_cell[2]) + 0.03,
    );
    if drawn(&i.floor, &i.row, project(&view, corner)) {
        report.fail(
            format!("{what}: the small cross reaches its cell's corner"),
            &images,
        );
    }
    // The spun square: a diamond. Inside it past the square's side, outside it inside the
    // square's corner.
    let cell = row.blocks[2].0;
    let center = DVec3::new(
        f64::from(cell[0]) + 0.5,
        f64::from(cell[1]) + 0.5,
        f64::from(cell[2]) + 0.5,
    );
    let inside = center + DVec3::new(0.3, 0.0, 0.0);
    let outside = center + DVec3::new(0.22, 0.22, 0.0);
    if !drawn(&i.floor, &i.row, project(&view, inside)) {
        report.fail(
            format!("{what}: the spun square misses its turned corner"),
            &images,
        );
    }
    if drawn(&i.floor, &i.row, project(&view, outside)) {
        report.fail(
            format!("{what}: the spun square still covers its old corner"),
            &images,
        );
    }
}

#[test]
fn planes_are_drawn_alike_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let row = row();
    let mut report = Report::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let mut tiers = Vec::new();
        for tier in allowed_tiers(&caps) {
            match render_tier(adapter, &caps, tier, &row) {
                Ok(images) => tiers.push((tier, images)),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        for (tier, i) in &tiers {
            rendered += 1;
            let what = format!("{label}_{tier}");
            if std::env::var_os("GPU_PLANES_SAVE").is_some() {
                report.save(&[
                    (&format!("{what}_floor"), &i.floor),
                    (&format!("{what}_row"), &i.row),
                ]);
            }
            check_tier(&mut report, &what, &row, i);
        }
        let Some(((best, b), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, t) in rest {
            let diff = b.row.compare(&t.row, 2);
            eprintln!(
                "gpu_planes: {label}: {tier} vs {best}: {:.4} % of pixels differ, max channel {}",
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
                        (&format!("{label}_{best}_row"), &b.row),
                        (&format!("{label}_{tier}_row"), &t.row),
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
