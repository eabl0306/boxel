//! The block outline (ADR 0026) on every tier. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! A stone floor with a stone cube and a fence post on it, meshed by the real `Mesher`. For
//! every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local
//! capability check allows (the one of `gpu_tiers.rs`), a `VoxelRenderer` draws it without an
//! outline, with the cube outlined, with the fence outlined (its outline boxes: the post, a
//! block tall) and with a block under the floor outlined. Asserts:
//!
//! - an outline shows (enough pixels change) and only darkens the picture;
//! - with `BlockOutline`'s default color it darkens as much as Minecraft's: to 0.6 of each sRGB
//!   value (the median over the darkened channels);
//! - the outline of the hidden block changes nothing (depth tested against the terrain);
//! - setting the outline back to `None` gives the picture without it again, and the outline is
//!   not counted in the draw calls;
//! - every tier draws the same pixels as the best tier of its adapter (0.1 % of the pixels,
//!   channel tolerance 2), with and without the outline.
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_outline/` (every image with
//! `GPU_OUTLINE_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::{
    BlockOutline, DEFAULT_SKY_COLOR, Outline, RendererConfig, ViewParams, VoxelRenderer,
    block_outline_edges,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, LocalPos};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_DIFF: f64 = 0.001;
/// An outline changes at least this many pixels.
const MIN_OUTLINE_PIXELS: usize = 100;
/// Minecraft's outline, black at 40 % blended on sRGB values, leaves 0.6 of each one.
const MINECRAFT_DARKENING: f64 = 0.6;
/// How far the median darkening of an outline may be from [`MINECRAFT_DARKENING`].
const DARKENING_TOLERANCE: f64 = 0.04;

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
        label: Some("gpu_outline"),
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

/// Where the outlined blocks are, in chunk (0, 0, 0).
const CUBE: [i64; 3] = [12, 9, 12];
const FENCE: [i64; 3] = [9, 9, 12];
/// Under the floor: every edge is hidden.
const HIDDEN: [i64; 3] = [12, 6, 12];

/// The stone floor (y = 8, x and z in 4..20), the cube and the fence post, meshed.
fn scene(registry: &BlockRegistry, materials: &MaterialTable) -> ChunkMesh {
    let stone = registry.by_name("boxel:stone").expect("stone");
    let fence = registry.by_name("boxel:oak_fence").expect("fence");
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for x in 4..20 {
        for z in 4..20 {
            dense[LocalPos::new(x, 8, z).index()] = stone;
        }
    }
    let at = |p: [i64; 3]| LocalPos::new(p[0] as u32, p[1] as u32, p[2] as u32).index();
    dense[at(CUBE)] = stone;
    dense[at(FENCE)] = fence;
    let chunk = ChunkData::from_dense(&dense);
    let input = MeshInput {
        center: &chunk,
        neighbors: [None; 26],
        light: None,
    };
    Mesher::new().mesh_chunk(&input, materials, MeshOptions::default())
}

/// The camera, south east of the cube and above it, looking at it.
fn view() -> ViewParams {
    let eye = DVec3::new(13.9, 11.3, 15.1);
    let target = DVec3::new(11.2, 9.6, 12.5);
    let d = (target - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, 0.6)
}

fn outline(registry: &BlockRegistry, block: [i64; 3], state: &str) -> Outline {
    let state = registry.parse_state(state).expect(state);
    Outline {
        block: I64Vec3::from_array(block),
        edges: block_outline_edges(registry, state).into(),
        color: BlockOutline::default().color,
        width: 2.0,
    }
}

// ---------------------------------------------------------------- rendering

/// Renders one frame and reads the main color back, with the frame's draw calls.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<(RgbaImage, u32), String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_outline"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    let image = read_texture_rgba8(device, queue, renderer.main_color())?;
    Ok((image, renderer.stats().draws))
}

/// The images of one tier: no outline, the cube, the fence, the hidden block, none again.
struct TierImages {
    plain: RgbaImage,
    cube: RgbaImage,
    fence: RgbaImage,
    hidden: RgbaImage,
    again: RgbaImage,
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let registry = BlockRegistry::default_blocks();
    let materials = MaterialTable::from_registry(&registry);
    let config = RendererConfig {
        arena_quads: 1 << 16,
        max_chunk_slots: 64,
        ..RendererConfig::default()
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
    renderer.set_model_boxes(&queue, materials.model_boxes());
    renderer
        .upsert_chunk(1, I64Vec3::ZERO, Arc::new(scene(&registry, &materials)))
        .map_err(|e| e.to_string())?;
    let view = view();
    let (plain, draws) = render(&device, &queue, &mut renderer, &view)?;
    let mut with = |o: Option<Outline>| -> Result<RgbaImage, String> {
        renderer.set_outline(o);
        let (image, d) = render(&device, &queue, &mut renderer, &view)?;
        if d != draws {
            return Err(format!("{d} draws with an outline, {draws} without"));
        }
        Ok(image)
    };
    let images = TierImages {
        cube: with(Some(outline(&registry, CUBE, "boxel:stone")))?,
        fence: with(Some(outline(&registry, FENCE, "boxel:oak_fence")))?,
        hidden: with(Some(outline(&registry, HIDDEN, "boxel:stone")))?,
        again: with(None)?,
        plain,
    };
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(images)
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
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_outline"),
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
                Ok(()) => eprintln!("gpu_outline: saved {}", path.display()),
                Err(e) => eprintln!("gpu_outline: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_outline: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }

    fn check_same(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        let diff = a.1.compare(b.1, 2);
        eprintln!(
            "gpu_outline: {what}: {:.4} % of pixels differ, max channel {}",
            diff.fraction() * 100.0,
            diff.max_channel_diff
        );
        if diff.fraction() > MAX_DIFF {
            self.fail(
                format!(
                    "{what}: {} of {} pixels differ ({:.4} %)",
                    diff.differing_pixels,
                    diff.total_pixels,
                    diff.fraction() * 100.0
                ),
                &[a, b],
            );
        }
    }

    /// `lined` is `plain` with dark lines: at least [`MIN_OUTLINE_PIXELS`] pixels darker, none
    /// brighter, darkened as Minecraft's are ([`MINECRAFT_DARKENING`]).
    fn check_lines(&mut self, what: &str, plain: (&str, &RgbaImage), lined: (&str, &RgbaImage)) {
        let mut darker = 0;
        let mut brighter = 0;
        for (a, b) in plain.1.data.chunks(4).zip(lined.1.data.chunks(4)) {
            if (0..3).any(|c| b[c] > a[c].saturating_add(2)) {
                brighter += 1;
            } else if (0..3).any(|c| b[c].saturating_add(8) < a[c]) {
                darker += 1;
            }
        }
        eprintln!("gpu_outline: {what}: {darker} pixels darker, {brighter} brighter");
        if darker < MIN_OUTLINE_PIXELS || brighter > 0 {
            self.fail(
                format!("{what}: {darker} pixels darker, {brighter} brighter"),
                &[plain, lined],
            );
        }
        // How much the lines darken: the median of `lined / plain` over the channels of the
        // darkened pixels, from a plain value of 64 up (where 8 bits resolve the ratio). The
        // few pixels where two edges overlap are darker.
        let mut ratios = Vec::new();
        for (a, b) in plain.1.data.chunks(4).zip(lined.1.data.chunks(4)) {
            if (0..3).any(|c| b[c].saturating_add(8) < a[c]) {
                ratios.extend(
                    (0..3)
                        .filter(|&c| a[c] >= 64)
                        .map(|c| f64::from(b[c]) / f64::from(a[c])),
                );
            }
        }
        ratios.sort_by(f64::total_cmp);
        let median = ratios.get(ratios.len() / 2).copied().unwrap_or(f64::NAN);
        eprintln!(
            "gpu_outline: {what}: darkened to {median:.3} of the sRGB value ({} channels)",
            ratios.len()
        );
        if ratios.len() < MIN_OUTLINE_PIXELS
            || (median - MINECRAFT_DARKENING).abs() > DARKENING_TOLERANCE
        {
            self.fail(
                format!(
                    "{what}: darkened to {median:.3} of the sRGB value over {} channels, \
                     Minecraft's is {MINECRAFT_DARKENING}",
                    ratios.len()
                ),
                &[plain, lined],
            );
        }
    }
}

#[test]
fn the_block_outline_is_drawn_alike_on_every_tier() {
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
            match render_tier(adapter, &caps, tier) {
                Ok(images) => tiers.push((tier, images)),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        for (tier, i) in &tiers {
            rendered += 1;
            let name = |what: &str| format!("{label}_{tier}_{what}");
            if std::env::var_os("GPU_OUTLINE_SAVE").is_some() {
                report.save(&[
                    (&name("plain"), &i.plain),
                    (&name("cube"), &i.cube),
                    (&name("fence"), &i.fence),
                    (&name("hidden"), &i.hidden),
                ]);
            }
            let plain = (name("plain"), &i.plain);
            let plain = (plain.0.as_str(), plain.1);
            report.check_lines(
                &format!("{label} {tier}: the cube"),
                plain,
                (&name("cube"), &i.cube),
            );
            report.check_lines(
                &format!("{label} {tier}: the fence"),
                plain,
                (&name("fence"), &i.fence),
            );
            let hidden = i.plain.compare(&i.hidden, 0);
            if hidden.differing_pixels > 0 {
                report.fail(
                    format!(
                        "{label} {tier}: the hidden block's outline shows on {} pixels",
                        hidden.differing_pixels
                    ),
                    &[plain, (&name("hidden"), &i.hidden)],
                );
            }
            let again = i.plain.compare(&i.again, 0);
            if again.differing_pixels > 0 {
                report.fail(
                    format!(
                        "{label} {tier}: {} pixels differ after the outline went away",
                        again.differing_pixels
                    ),
                    &[plain, (&name("again"), &i.again)],
                );
            }
        }
        let Some(((best, b), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, t) in rest {
            for (what, x, y) in [
                ("plain", &b.plain, &t.plain),
                ("cube", &b.cube, &t.cube),
                ("fence", &b.fence, &t.fence),
            ] {
                report.check_same(
                    &format!("{label}: {tier} vs {best} ({what})"),
                    (&format!("{label}_{best}_{what}"), x),
                    (&format!("{label}_{tier}_{what}"), y),
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
