//! Animated resource pack textures (ADR 0028) on every tier. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! A generated pack (no third-party content) whose stone is a strip of three solid frames, red,
//! green and blue, 4 ticks each and interpolated. A 32x32 stone floor, open sky, is drawn by one
//! `VoxelRenderer` per adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and tier (the
//! capability check of `gpu_tiers.rs`) at several `ViewParams::texture_tick`s, in this order:
//!
//! - tick 0 close up: red;
//! - tick 4: green, the next frame written over the layer;
//! - tick 6: halfway from green to blue (Minecraft's interpolation);
//! - tick 8 from far above, where the floor samples a small mip: blue, so every mip level of
//!   the layer was written;
//! - tick 12 close up: red again (the loop);
//! - a budget of one byte at tick 16 (green): one layer still goes through in the frame.
//!
//! Every tier draws the same pixels as the best tier of its adapter at every step (0.1 % of the
//! pixels, channel tolerance 2).
//!
//! A second test looks at the side of a water source, meshed by the real `Mesher`, whose flow
//! texture is red on top and blue below: with a still `water_flow` the side scrolls (ADR 0025)
//! and half a texture later the colors swap; with an animated one it moves by its frames only,
//! so the same tick draws the same side at any fluid time.
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_animated_textures/`.
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{
    BUCKET_COUNT, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher, PackedQuad,
};
use boxel_render::{
    BlockTextureData, DEFAULT_SKY_COLOR, RendererConfig, TextureSource, TranslucentSort,
    ViewParams, VoxelRenderer, load_block_textures,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, Face, LocalPos};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_DIFF: f64 = 0.001;
/// The three frames of the stone strip.
const FRAMES: [[u8; 4]; 3] = [[220, 30, 30, 255], [30, 220, 30, 255], [30, 30, 220, 255]];

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
        label: Some("gpu_animated_textures"),
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

/// The pack: stone is a 16x48 strip of the three [`FRAMES`], 4 ticks each, interpolated.
fn animated_pack() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_animated_textures_pack");
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
    dir
}

/// One 32x32 +Y stone quad, open sky (no palette).
fn floor(registry: &BlockRegistry) -> Arc<ChunkMesh> {
    let stone = registry
        .textures()
        .iter()
        .position(|t| t.name == "boxel:stone")
        .expect("stone layer") as u16;
    let mut counts = [0; BUCKET_COUNT];
    counts[Face::PosY.index()] = 1;
    let mut mesh = ChunkMesh::from_quads(
        vec![PackedQuad::new([0, 0, 0], 32, 32, Face::PosY, stone)],
        counts,
    );
    mesh.palette.clear();
    mesh.hash = mesh.compute_hash();
    Arc::new(mesh)
}

/// Straight down onto the middle of the floor from `height` voxels above it.
fn view(height: f64, tick: u64) -> ViewParams {
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(
        DVec3::new(16.0, height, 16.0),
        0.0,
        -std::f32::consts::FRAC_PI_2,
    );
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams {
        texture_tick: tick,
        ..ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, 0.6)
    }
}

/// The steps: (name, eye height, tick, upload budget).
const STEPS: [(&str, f64, u64, u64); 6] = [
    ("red", 4.0, 0, u64::MAX),
    ("green", 4.0, 4, u64::MAX),
    ("green_blue", 4.0, 6, u64::MAX),
    ("blue_far", 60.0, 8, u64::MAX),
    ("red_again", 4.0, 12, u64::MAX),
    ("green_tiny_budget", 4.0, 16, 1),
];

// ---------------------------------------------------------------- rendering

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_animated_textures"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// The image of every step on one tier, and the layers the renderer uploaded.
fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    textures: &BlockTextureData,
    mesh: &Arc<ChunkMesh>,
) -> Result<(Vec<RgbaImage>, u64), String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        arena_quads: 1 << 13,
        max_chunk_slots: 8,
        ..RendererConfig::default()
    };
    let mut renderer = VoxelRenderer::new(
        &device,
        &queue,
        tier,
        &config,
        textures,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    renderer
        .upsert_chunk(0, I64Vec3::ZERO, Arc::clone(mesh))
        .map_err(|e| e.to_string())?;
    let mut images = Vec::new();
    for (_, height, tick, budget) in STEPS {
        renderer.set_texture_upload_budget(budget);
        images.push(render(&device, &queue, &mut renderer, &view(height, tick))?);
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors {errors:?}"));
    }
    Ok((images, renderer.stats().texture_layer_uploads))
}

/// Mean color of the middle quarter of the image.
fn middle(image: &RgbaImage) -> [f64; 3] {
    let mut sum = [0.0; 3];
    let mut n = 0.0;
    for y in HEIGHT * 3 / 8..HEIGHT * 5 / 8 {
        for x in WIDTH * 3 / 8..WIDTH * 5 / 8 {
            let i = ((y * WIDTH + x) * 4) as usize;
            for (s, &v) in sum.iter_mut().zip(&image.data[i..i + 3]) {
                *s += f64::from(v);
            }
            n += 1.0;
        }
    }
    sum.map(|s| s / n)
}

/// Whether `color` is the step's expected frame: one channel far above the others, or (the
/// blend) green and blue alike, both far above red.
fn looks_right(step: &str, [r, g, b]: [f64; 3]) -> bool {
    match step {
        "red" | "red_again" => r > 3.0 * g && r > 3.0 * b,
        "green" | "green_tiny_budget" => g > 3.0 * r && g > 3.0 * b,
        "blue_far" => b > 3.0 * r && b > 3.0 * g,
        "green_blue" => g > 2.0 * r && b > 2.0 * r && (g - b).abs() < 0.2 * g.max(b),
        _ => false,
    }
}

/// Fraction of pixels whose channels differ by more than 2.
fn diff_fraction(a: &RgbaImage, b: &RgbaImage) -> f64 {
    let differing = a
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.data.as_chunks::<4>().0)
        .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 2))
        .count();
    differing as f64 / (a.data.len() / 4) as f64
}

fn save(image: &RgbaImage, name: &str) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_animated_textures");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.png"));
    match image.save_png(&path) {
        Ok(()) => eprintln!("saved {}", path.display()),
        Err(e) => eprintln!("could not save {}: {e}", path.display()),
    }
}

#[test]
fn animated_pack_textures_follow_the_tick_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let registry = BlockRegistry::default_blocks();
    let (textures, report) =
        load_block_textures(&registry, &TextureSource::Pack(animated_pack())).expect("pack");
    assert_eq!((report.from_pack, report.animated), (1, 1), "{report:?}");
    let mesh = floor(&registry);
    let mut failures = Vec::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let mut reference: Option<(Tier, Vec<RgbaImage>)> = None;
        for tier in allowed_tiers(&caps) {
            let (images, uploads) = match render_tier(adapter, &caps, tier, &textures, &mesh) {
                Ok(result) => result,
                Err(e) => {
                    failures.push(format!("{label} {tier}: {e}"));
                    continue;
                }
            };
            rendered += 1;
            eprintln!(
                "{label} {tier}: {} steps, {uploads} layer uploads",
                STEPS.len()
            );
            // every step shows another frame: the layer is written once in each (the first
            // one too, the renderer does not assume what the array was created with)
            if uploads != STEPS.len() as u64 {
                failures.push(format!("{label} {tier}: {uploads} layer uploads"));
            }
            for ((step, ..), image) in STEPS.iter().zip(&images) {
                let color = middle(image);
                if !looks_right(step, color) {
                    failures.push(format!("{label} {tier}: {step} shows {color:?}"));
                    save(image, &format!("{tier}_{step}"));
                }
            }
            match &reference {
                None => reference = Some((tier, images)),
                Some((best, best_images)) => {
                    for (((step, ..), image), best_image) in
                        STEPS.iter().zip(&images).zip(best_images)
                    {
                        let diff = diff_fraction(image, best_image);
                        if diff > MAX_DIFF {
                            failures.push(format!(
                                "{label}: {tier} differs from {best} at {step} in {:.2} % of \
                                 the pixels",
                                diff * 100.0
                            ));
                            save(image, &format!("{tier}_{step}_diff"));
                            save(best_image, &format!("{best}_{step}_reference"));
                        }
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        rendered > 0 || !gpu_tests_required(),
        "BOXEL_REQUIRE_GPU=1 but no tier could render"
    );
}

// ---------------------------------------------------------------- flow

/// A pack whose `water_flow` is 32 px frames (a face shows their top-left 16 px quarter: red
/// rows 0 to 7, blue rows 8 to 15), two frames and animated when `animated`, else one still
/// frame.
fn flow_pack(animated: bool) -> PathBuf {
    let name = if animated { "animated" } else { "still" };
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("gpu_flow_pack_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let blocks = dir.join("assets/minecraft/textures/block");
    std::fs::create_dir_all(&blocks).expect("pack dirs");
    std::fs::write(
        dir.join("pack.mcmeta"),
        r#"{"pack":{"pack_format":34,"description":"boxel gpu test pack"}}"#,
    )
    .expect("pack.mcmeta");
    let height = if animated { 64 } else { 32 };
    RgbaImage {
        width: 32,
        height,
        data: (0..32 * height)
            .flat_map(|i| {
                if (i / 32) % 32 % 16 < 8 {
                    [220, 30, 30, 255]
                } else {
                    [30, 30, 220, 255]
                }
            })
            .collect(),
    }
    .save_png(&blocks.join("water_flow.png"))
    .expect("png");
    // stone sets the common size to 16
    RgbaImage {
        width: 16,
        height: 16,
        data: [128; 16 * 16 * 4].to_vec(),
    }
    .save_png(&blocks.join("stone.png"))
    .expect("png");
    if animated {
        std::fs::write(
            blocks.join("water_flow.png.mcmeta"),
            r#"{"animation":{"frametime":5}}"#,
        )
        .expect("mcmeta");
    }
    dir
}

/// A water source alone at (8, 8, 8), meshed.
fn water_block(registry: &BlockRegistry) -> Arc<ChunkMesh> {
    let water = registry.by_name("boxel:water").expect("water");
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    dense[LocalPos::new(8, 8, 8).index()] = water;
    let chunk = ChunkData::from_dense(&dense);
    let input = MeshInput {
        center: &chunk,
        neighbors: [None; 26],
        light: None,
    };
    let materials = MaterialTable::from_registry(registry);
    Arc::new(Mesher::new().mesh_chunk(&input, &materials, MeshOptions::default()))
}

/// Close in front of the source's +Z side, at `fluid_time` seconds and tick 0.
fn side_view(fluid_time: f32) -> ViewParams {
    let eye = DVec3::new(8.5, 8.4, 10.4);
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(eye, 0.0, 0.0);
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams {
        fluid_time,
        ..ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, 0.6)
    }
}

/// The side at fluid times 0 and 1 (water scrolls half a texture in a second).
fn render_side(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    textures: &BlockTextureData,
    mesh: &Arc<ChunkMesh>,
) -> Result<[RgbaImage; 2], String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        arena_quads: 1 << 13,
        max_chunk_slots: 8,
        translucent_sort: TranslucentSort::Inline,
    };
    let mut renderer = VoxelRenderer::new(
        &device,
        &queue,
        tier,
        &config,
        textures,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    renderer
        .upsert_chunk(0, I64Vec3::ZERO, Arc::clone(mesh))
        .map_err(|e| e.to_string())?;
    let at_zero = render(&device, &queue, &mut renderer, &side_view(0.0))?;
    let at_one = render(&device, &queue, &mut renderer, &side_view(1.0))?;
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors {errors:?}"));
    }
    Ok([at_zero, at_one])
}

#[test]
fn an_animated_flow_texture_does_not_scroll_on_any_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let registry = BlockRegistry::default_blocks();
    let mesh = water_block(&registry);
    let packs = [false, true].map(|animated| {
        let (textures, report) =
            load_block_textures(&registry, &TextureSource::Pack(flow_pack(animated)))
                .expect("pack");
        assert_eq!(textures.animated_flow, [animated, false], "{report:?}");
        textures
    });
    let mut failures = Vec::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            for (textures, animated) in packs.iter().zip([false, true]) {
                let [at_zero, at_one] = match render_side(adapter, &caps, tier, textures, &mesh) {
                    Ok(images) => images,
                    Err(e) => {
                        failures.push(format!("{label} {tier}: {e}"));
                        continue;
                    }
                };
                rendered += 1;
                let name = if animated { "animated" } else { "still" };
                let diff = diff_fraction(&at_zero, &at_one);
                eprintln!(
                    "{label} {tier} {name}: {:.1} % of the pixels moved",
                    diff * 100.0
                );
                // the side covers the middle of the image: a scroll changes a good part of it
                let ok = if animated { diff == 0.0 } else { diff > 0.05 };
                if !ok {
                    failures.push(format!(
                        "{label} {tier}: {name} flow changed {:.2} % of the pixels",
                        diff * 100.0
                    ));
                    save(&at_zero, &format!("{tier}_flow_{name}_0"));
                    save(&at_one, &format!("{tier}_flow_{name}_1"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        rendered > 0 || !gpu_tests_required(),
        "BOXEL_REQUIRE_GPU=1 but no tier could render"
    );
}
