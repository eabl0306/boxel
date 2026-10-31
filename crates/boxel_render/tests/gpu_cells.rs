//! Cell quads (EW-ITEM, ADR 0050) on every tier. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! A stone wall meshed by the real `Mesher` with a row of test frames in front of it whose cells
//! hold data: a red item, a green one clear on its right half, the red one spun 45 degrees, a
//! white one, and the white one drawn full bright with a blue tint. For every adapter (filtered
//! by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local capability check allows (the
//! one of `gpu_tiers.rs`), a `VoxelRenderer` with the cell textures draws the wall and frames
//! without data, and with it. Asserts:
//!
//! - each item covers the pixel of its center, where `CellQuad::from_plane` puts it, and the
//!   clear half of the green one leaves the wall seen;
//! - the spun item shows where the turn takes its corners and not where they were;
//! - the full bright item is its tint exactly, the plain white one is shaded;
//! - every tier draws the same pixels as the best tier of its adapter (0.1 % of the pixels,
//!   channel tolerance 2).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_cells/` (every image with
//! `GPU_CELLS_SAVE=1`).
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
    Axis, BlockCollision, BlockData, BlockDef, BlockModel, BlockRegistry, BlockState, BoxModel,
    CellQuad, CellQuadRule, ChunkData, DataValue, Face, FaceTextures, LocalPos, ModelBox,
    ModelPlane, PlaneKind, RenderShape, TextureImage, TextureSpec,
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
        label: Some("gpu_cells"),
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

/// The cell textures: red, green clear on its right half, white.
const RED: u16 = 0;
const GREEN_HALF: u16 = 1;
const WHITE: u16 = 2;
const BLUE_TINT: [u8; 3] = [0, 0, 255];

fn cell_textures() -> boxel_render::BlockTextureData {
    let solid = |name: &str, color| TextureSpec {
        name: name.to_string(),
        image: TextureImage::solid(16, color),
    };
    let half: Vec<[u8; 4]> = (0..256)
        .map(|i| {
            if i % 16 < 8 {
                [40, 200, 60, 255]
            } else {
                [0; 4]
            }
        })
        .collect();
    let half = TextureSpec {
        name: "test:half".to_string(),
        image: TextureImage {
            size: 16,
            texels: half.into(),
            binary_alpha: true,
        },
    };
    boxel_render::generate_block_textures(&[
        solid("test:red", [220, 30, 30, 255]),
        half,
        solid("test:white", [255, 255, 255, 255]),
    ])
}

/// The item plane of a frame: across Z, in front of the board on the cell's +Z side (the
/// wall), half a block wide.
fn item_plane(spin: u8) -> ModelPlane {
    ModelPlane::new(PlaneKind::Mid(Axis::Z), [8, 8, 28], [24, 24, 28]).spun(spin)
}

/// The row: cell (x, 10, 19) and its data `(image, spin, full bright)`.
const ROW: [(u32, (u16, u8, bool)); 5] = [
    (10, (RED, 0, false)),
    (12, (GREEN_HALF, 0, false)),
    (14, (RED, 2, false)),
    (16, (WHITE, 0, false)),
    (18, (WHITE, 0, true)),
];

struct Scene {
    registry: BlockRegistry,
    frame: BlockState,
}

/// The demo blocks and `test:frame`: a thin board against the wall whose cells draw their item.
fn scene() -> Scene {
    let demo = boxel_demo::blocks::registry();
    let planks = demo.by_name("boxel:planks").expect("planks");
    let planks = demo.face_texture(planks, Face::PosX);
    let mut b = boxel_demo::blocks::builder();
    let model = b
        .add_box_model(BoxModel::from_boxes(vec![ModelBox::new(
            [2, 2, 15],
            [14, 14, 16],
        )]))
        .expect("model");
    let frame = b
        .add_block(BlockDef {
            name: "test:frame".to_string(),
            shape: RenderShape::Cutout,
            model: BlockModel::Boxes(model),
            collision: BlockCollision::None,
            emission: [0; 3],
            light_opacity: 0,
            textures: FaceTextures::all(planks),
        })
        .expect("frame");
    let id = b.block_by_name("test:frame").expect("id");
    let rule = CellQuadRule::new(|_, _| {
        Some(Arc::new(|data: &BlockData, out: &mut Vec<CellQuad>| {
            let int = |name| data.int(name).unwrap_or(0);
            let quad = CellQuad::from_plane(&item_plane(int("spin") as u8), int("image") as u16)
                .shaded_as(Face::NegZ);
            out.push(if int("bright") == 1 {
                quad.with_full_bright(true).with_tint(BLUE_TINT)
            } else {
                quad
            });
        }))
    });
    b.set_cell_quads(id, rule).expect("rule");
    Scene {
        registry: b.build(),
        frame,
    }
}

/// The stone wall (z = 20, x in 6..26, y in 6..16) and the frames, with their data when
/// `with_items`, meshed.
fn mesh(scene: &Scene, materials: &MaterialTable, with_items: bool) -> ChunkMesh {
    let stone = scene.registry.by_name("boxel:stone").expect("stone");
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for x in 6..26 {
        for y in 6..16 {
            dense[LocalPos::new(x, y, 20).index()] = stone;
        }
    }
    for &(x, _) in &ROW {
        dense[LocalPos::new(x, 10, 19).index()] = scene.frame;
    }
    let mut chunk = ChunkData::from_dense(&dense);
    if with_items {
        for &(x, (image, spin, bright)) in &ROW {
            let data = BlockData::new()
                .with("image", DataValue::Int(i64::from(image)))
                .with("spin", DataValue::Int(i64::from(spin)))
                .with("bright", DataValue::Int(i64::from(bright)));
            chunk.set_block_data(LocalPos::new(x, 10, 19), Some(data));
        }
    }
    let input = MeshInput {
        center: &chunk,
        neighbors: [None; 26],
        light: None,
    };
    let mesh = Mesher::new().mesh_chunk(&input, materials, MeshOptions::default());
    assert_eq!(
        mesh.cell_quads.len(),
        if with_items { ROW.len() } else { 0 }
    );
    mesh
}

fn view() -> ViewParams {
    let eye = DVec3::new(14.5, 10.5, 12.0);
    let transform = Transform::from_yaw_pitch(eye, std::f32::consts::PI, 0.0);
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
            label: Some("gpu_cells"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// The frames without data, then with it.
struct TierImages {
    bare: RgbaImage,
    items: RgbaImage,
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    scene: &Scene,
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let materials = MaterialTable::from_registry(&scene.registry);
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
        &boxel_render::generate_block_textures(scene.registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    renderer.set_model_boxes(&queue, materials.model_boxes());
    renderer.set_cell_textures(&device, &queue, &Arc::new(cell_textures()));
    let view = view();
    renderer
        .upsert_chunk(1, I64Vec3::ZERO, Arc::new(mesh(scene, &materials, false)))
        .map_err(|e| e.to_string())?;
    let bare = render(&device, &queue, &mut renderer, &view)?;
    renderer
        .upsert_chunk(1, I64Vec3::ZERO, Arc::new(mesh(scene, &materials, true)))
        .map_err(|e| e.to_string())?;
    let items = render(&device, &queue, &mut renderer, &view)?;
    let stats = renderer.stats().cells;
    if (stats.chunks, stats.quads, stats.drawn, stats.layers)
        != (1, ROW.len() as u32, ROW.len() as u32, 3)
    {
        return Err(format!("cell stats {stats:?}"));
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(TierImages { bare, items })
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
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_cells"),
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
                Ok(()) => eprintln!("gpu_cells: saved {}", path.display()),
                Err(e) => eprintln!("gpu_cells: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_cells: FAIL {message}");
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

/// The world point at `(u, v)` (corner weights) of the item of the frame at `x`.
fn on_item(x: u32, spin: u8, (u, v): (f32, f32)) -> DVec3 {
    let q = CellQuad::from_plane(&item_plane(spin), 0);
    let w = [(1.0 - u) * (1.0 - v), u * (1.0 - v), (1.0 - u) * v, u * v];
    let p: Vec<f64> = (0..3)
        .map(|a| f64::from((0..4).map(|k| q.corners[k][a] * w[k]).sum::<f32>()))
        .collect();
    DVec3::new(f64::from(x) + p[0], 10.0 + p[1], 19.0 + p[2])
}

fn check_tier(report: &mut Report, what: &str, i: &TierImages) {
    let view = view();
    let images = [("bare", &i.bare), ("items", &i.items)];
    for &(x, (image, spin, _)) in &ROW {
        let points: &[(f32, f32)] = if image == GREEN_HALF {
            &[(0.25, 0.5)]
        } else {
            &[(0.5, 0.5), (0.2, 0.2), (0.8, 0.8)]
        };
        for &uv in points {
            let at = project(&view, on_item(x, spin, uv));
            if !drawn(&i.bare, &i.items, at) {
                report.fail(
                    format!("{what}: item at x {x} not drawn at {uv:?} {at:?}"),
                    &images,
                );
            }
        }
    }
    // The clear half of the green item: the board behind it shows.
    let clear = project(&view, on_item(12, 0, (0.75, 0.5)));
    if drawn(&i.bare, &i.items, clear) {
        report.fail(
            format!("{what}: the clear half of an item is drawn"),
            &images,
        );
    }
    // The spun item: a diamond. Inside it past the square's side, outside it inside its corner.
    let center = DVec3::new(14.5, 10.5, 19.875);
    for (offset, inside) in [
        (DVec3::new(0.3, 0.0, 0.0), true),
        (DVec3::new(0.22, 0.22, 0.0), false),
    ] {
        if drawn(&i.bare, &i.items, project(&view, center + offset)) != inside {
            report.fail(
                format!("{what}: the spun item at {offset:?} inside: {}", !inside),
                &images,
            );
        }
    }
    // Full bright: the tint itself. Plain white under the noon sky: shaded as -Z.
    let bright = pixel(&i.items, project(&view, on_item(18, 0, (0.5, 0.5))));
    if bright[2] < 250 || bright[0] > 4 || bright[1] > 4 {
        report.fail(
            format!("{what}: the full bright item is {bright:?}"),
            &images,
        );
    }
    let white = pixel(&i.items, project(&view, on_item(16, 0, (0.5, 0.5))));
    if white[..3].iter().any(|c| !(120..=240).contains(c)) {
        report.fail(format!("{what}: the white item is {white:?}"), &images);
    }
}

#[test]
fn cell_quads_are_drawn_alike_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let scene = scene();
    let mut report = Report::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let mut tiers = Vec::new();
        for tier in allowed_tiers(&caps) {
            match render_tier(adapter, &caps, tier, &scene) {
                Ok(images) => tiers.push((tier, images)),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        for (tier, i) in &tiers {
            rendered += 1;
            let what = format!("{label}_{tier}");
            if std::env::var_os("GPU_CELLS_SAVE").is_some() {
                report.save(&[
                    (&format!("{what}_bare"), &i.bare),
                    (&format!("{what}_items"), &i.items),
                ]);
            }
            check_tier(&mut report, &what, i);
        }
        let Some(((best, b), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, t) in rest {
            let diff = b.items.compare(&t.items, 2);
            eprintln!(
                "gpu_cells: {label}: {tier} vs {best}: {:.4} % of pixels differ, max channel {}",
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
                        (&format!("{label}_{best}_items"), &b.items),
                        (&format!("{label}_{tier}_items"), &t.items),
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
