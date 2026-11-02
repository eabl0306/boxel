//! Entity models (ADR 0040) on every tier. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! A stone floor meshed by the real `Mesher` and a test figure (a head with an outer layer, a
//! body and two arms, in Minecraft's box-UV layout) whose head faces have one color each. For
//! every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local
//! capability check allows (the one of `gpu_tiers.rs`), a `VoxelRenderer` draws the scene
//! without models, with the figure posed in front of the camera, under the floor, in the dark,
//! with a fully clear outer layer (and without that box), and a first-person arm (the view
//! layer) over a figure right in front of the eye. Asserts:
//!
//! - the posed figure shows its front: the head's front color at the center of the head's front
//!   face (the faces are wound, culled and textured the right way round);
//! - under the floor nothing shows (depth tested with the terrain);
//! - in the dark it is much darker (lit by its light, as the terrain);
//! - a clear outer layer draws nothing (alpha tested): the same pixels as without its box;
//! - the arm of the view layer draws over the figure in front of it, the same pixels as over
//!   the plain scene;
//! - the models add no terrain draw call, and drawing the same frame again uploads nothing;
//! - every tier draws the same pixels as the best tier of its adapter (0.1 % of the pixels,
//!   channel tolerance 2).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_models/` (every image with
//! `GPU_MODELS_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, Quat, UVec2, Vec3};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::model::{Cuboid, Model, ModelDraw, ModelLayer, ModelTexture, PartPose};
use boxel_render::{DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, ViewParams, VoxelRenderer};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, LightValue, LocalPos};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_DIFF: f64 = 0.001;
/// A drawn figure changes at least this many pixels.
const MIN_FIGURE_PIXELS: usize = 500;

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
        label: Some("gpu_models"),
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

/// The colors of the head's faces on the test skin (sRGB).
const HEAD_FRONT: [u8; 4] = [230, 30, 30, 255];
const HEAD_BACK: [u8; 4] = [230, 230, 30, 255];
const HEAD_SIDES: [u8; 4] = [30, 200, 30, 255];
const HEAD_TOP: [u8; 4] = [240, 240, 240, 255];
/// The rest of the base layers.
const BASE: [u8; 4] = [90, 110, 200, 255];
/// The outer layer's band over the eyes.
const BAND: [u8; 4] = [30, 220, 220, 255];

/// The feet of the posed figure: in front of the camera, standing on the floor.
const FEET: DVec3 = DVec3::new(16.5, 9.0, 16.5);
/// The feet of the hidden figure: all of it under the floor's top.
const UNDER: DVec3 = DVec3::new(16.5, 6.0, 16.5);

/// The stone floor (y = 8, x and z in 4..28), meshed.
fn scene(registry: &BlockRegistry, materials: &MaterialTable) -> ChunkMesh {
    let stone = registry.by_name("boxel:stone").expect("stone");
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for x in 4..28 {
        for z in 4..28 {
            dense[LocalPos::new(x, 8, z).index()] = stone;
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

/// The test figure: body at the root, a head (with its outer layer when `hat`) and two arms
/// under it, Minecraft's biped proportions (2 blocks tall at scale 1, feet at y = 24).
fn figure(hat: bool) -> Arc<Model> {
    let mut b = Model::builder([64, 64]);
    let body = b.part("body", None, PartPose::default());
    b.cuboid(
        body,
        Cuboid::new([16.0, 16.0], [-4.0, 0.0, -2.0], [8.0, 12.0, 4.0]),
    );
    let head = b.part("head", Some(body), PartPose::default());
    b.cuboid(
        head,
        Cuboid::new([0.0, 0.0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0]),
    );
    if hat {
        b.cuboid(
            head,
            Cuboid::new([32.0, 0.0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0])
                .inflate(0.5)
                .cutout(),
        );
    }
    let right = b.part("right_arm", Some(body), PartPose::at([-5.0, 2.0, 0.0]));
    b.cuboid(
        right,
        Cuboid::new([40.0, 16.0], [-3.0, -2.0, -2.0], [4.0, 12.0, 4.0]),
    );
    let left = b.part("left_arm", Some(body), PartPose::at([5.0, 2.0, 0.0]));
    b.cuboid(
        left,
        Cuboid::new([40.0, 16.0], [-1.0, -2.0, -2.0], [4.0, 12.0, 4.0]).mirror(),
    );
    // Legs as one box, so the figure stands 2 blocks tall.
    let legs = b.part("legs", Some(body), PartPose::at([0.0, 12.0, 0.0]));
    b.cuboid(
        legs,
        Cuboid::new([0.0, 16.0], [-4.0, 0.0, -2.0], [8.0, 12.0, 4.0]),
    );
    Arc::new(b.build().expect("valid figure"))
}

/// The test skin; with `band`, the outer layer of the head has an opaque band over the eyes,
/// else it is all clear.
fn skin(band: bool) -> ModelTexture {
    let mut rgba = Vec::with_capacity(64 * 64 * 4);
    for y in 0..64u32 {
        for x in 0..64u32 {
            let texel = match (x, y) {
                (8..16, 0..8) => HEAD_TOP,
                (8..16, 8..16) => HEAD_FRONT,
                (24..32, 8..16) => HEAD_BACK,
                (0..8 | 16..24, 8..16) => HEAD_SIDES,
                (40..48, 10..12) if band => BAND,
                (32..64, 0..16) => [0, 0, 0, 0],
                _ => BASE,
            };
            rgba.extend_from_slice(&texel);
        }
    }
    ModelTexture::new(64, 64, rgba).expect("valid skin")
}

/// The figure as a world draw at `feet`, facing -Z (the camera), its right arm raised forward
/// and its head turned a little.
fn posed(model: &Arc<Model>, texture: &ModelTexture, feet: DVec3, light: LightValue) -> ModelDraw {
    let mut pose = model.rest_pose();
    pose[model.part("right_arm").expect("arm")].rotation.x = -1.2;
    pose[model.part("head").expect("head")].rotation.y = 0.15;
    ModelDraw {
        id: 1,
        model: Arc::clone(model),
        texture: texture.clone(),
        pose,
        layer: ModelLayer::World,
        position: feet,
        rotation: Quat::IDENTITY,
        origin: Vec3::new(0.0, 24.0, 0.0),
        scale: 1.0,
        light,
        realm: boxel_render::model::ModelRealm::default(),
    }
}

/// The figure's right arm alone as a first-person hand (the view layer): in the lower right of
/// the view, pointing forward and down.
fn hand(texture: &ModelTexture) -> ModelDraw {
    let mut b = Model::builder([64, 64]);
    let arm = b.part("right_arm", None, PartPose::at([-5.0, 2.0, 0.0]));
    b.cuboid(
        arm,
        Cuboid::new([40.0, 16.0], [-3.0, -2.0, -2.0], [4.0, 12.0, 4.0]),
    );
    let model = Arc::new(b.build().expect("valid arm"));
    ModelDraw {
        id: 9,
        pose: model.rest_pose(),
        model,
        texture: texture.clone(),
        layer: ModelLayer::View,
        position: DVec3::new(0.35, -0.3, -0.6),
        rotation: Quat::from_rotation_x(1.3),
        origin: Vec3::new(-5.0, 2.0, 0.0),
        scale: 1.0,
        light: LightValue::FULL_SKY,
        realm: boxel_render::model::ModelRealm::default(),
    }
}

/// The camera 3.5 blocks in front of the figure (on its -Z side), looking along +Z and a little
/// down.
fn camera() -> (Camera, Transform) {
    let eye = DVec3::new(16.5, 10.6, 13.0);
    (
        Camera::default(),
        Transform::from_yaw_pitch(eye, std::f32::consts::PI, -0.12),
    )
}

fn view() -> ViewParams {
    let (camera, transform) = camera();
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

/// Renders one frame with `models` and reads the main color back, with the frame's terrain draw
/// calls and the model uploads so far.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
    models: Vec<ModelDraw>,
) -> Result<(RgbaImage, u32, u64), String> {
    let (_, transform) = camera();
    renderer.set_models(device, models, transform.rotation);
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_models"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    let image = read_texture_rgba8(device, queue, renderer.main_color())?;
    let stats = renderer.stats();
    Ok((image, stats.draws, stats.models.uploads))
}

/// The images of one tier.
struct TierImages {
    plain: RgbaImage,
    posed: RgbaImage,
    hidden: RgbaImage,
    dark: RgbaImage,
    clear_hat: RgbaImage,
    no_hat: RgbaImage,
    near: RgbaImage,
    near_hand: RgbaImage,
    hand: RgbaImage,
    again: RgbaImage,
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let registry = boxel_demo::blocks::registry();
    let materials = MaterialTable::from_registry(&registry);
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
    let (with_hat, without_hat) = (figure(true), figure(false));
    let (banded, clear) = (skin(true), skin(false));
    let sky = LightValue::FULL_SKY;
    let (plain, draws, _) = render(&device, &queue, &mut renderer, &view, Vec::new())?;
    let mut frame = |models: Vec<ModelDraw>| -> Result<(RgbaImage, u64), String> {
        let (image, d, uploads) = render(&device, &queue, &mut renderer, &view, models)?;
        if d != draws {
            return Err(format!("{d} terrain draws with models, {draws} without"));
        }
        Ok((image, uploads))
    };
    let (posed_image, uploads) = frame(vec![posed(&with_hat, &banded, FEET, sky)])?;
    let (again, still) = frame(vec![posed(&with_hat, &banded, FEET, sky)])?;
    if still != uploads {
        return Err(format!(
            "the same frame again uploaded the models ({uploads} then {still} uploads)"
        ));
    }
    // Right in front of the eye, filling the lower right of the view.
    let near_feet = DVec3::new(16.0, 9.0, 14.2);
    let near = posed(&without_hat, &banded, near_feet, sky);
    let images = TierImages {
        posed: posed_image,
        again,
        hidden: frame(vec![posed(&with_hat, &banded, UNDER, sky)])?.0,
        dark: frame(vec![posed(&with_hat, &banded, FEET, LightValue::DARK)])?.0,
        clear_hat: frame(vec![posed(&with_hat, &clear, FEET, sky)])?.0,
        no_hat: frame(vec![posed(&without_hat, &clear, FEET, sky)])?.0,
        near: frame(vec![near.clone()])?.0,
        near_hand: frame(vec![near, hand(&banded)])?.0,
        hand: frame(vec![hand(&banded)])?.0,
        plain,
    };
    let stats = renderer.stats().models;
    if stats.instances != 1 || stats.draws == 0 {
        return Err(format!("model stats after the hand: {stats:?}"));
    }
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
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_models"),
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
                Ok(()) => eprintln!("gpu_models: saved {}", path.display()),
                Err(e) => eprintln!("gpu_models: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_models: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }

    fn check(&mut self, ok: bool, message: String, images: &[(&str, &RgbaImage)]) {
        if !ok {
            self.fail(message, images);
        }
    }

    fn check_same(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        let diff = a.1.compare(b.1, 2);
        eprintln!(
            "gpu_models: {what}: {:.4} % of pixels differ, max channel {}",
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

/// The pixels where `a` and `b` differ by more than 8 in a channel.
fn changed(a: &RgbaImage, b: &RgbaImage) -> Vec<usize> {
    a.data
        .chunks(4)
        .zip(b.data.chunks(4))
        .enumerate()
        .filter(|(_, (p, q))| (0..3).any(|c| p[c].abs_diff(q[c]) > 8))
        .map(|(i, _)| i)
        .collect()
}

/// Mean sRGB brightness of `image` over `pixels`.
fn mean(image: &RgbaImage, pixels: &[usize]) -> f64 {
    let sum: f64 = pixels
        .iter()
        .map(|&i| {
            let p = &image.data[i * 4..i * 4 + 3];
            p.iter().map(|&c| f64::from(c)).sum::<f64>() / 3.0
        })
        .sum();
    sum / pixels.len().max(1) as f64
}

fn check_tier(report: &mut Report, what: &str, i: &TierImages) {
    let named = |n: &str| format!("{what}_{n}");
    let (plain_name, posed_name) = (named("plain"), named("posed"));
    let plain = (plain_name.as_str(), &i.plain);
    let posed_ = (posed_name.as_str(), &i.posed);
    let figure = changed(&i.plain, &i.posed);
    report.check(
        figure.len() >= MIN_FIGURE_PIXELS,
        format!("{what}: the figure changes only {} pixels", figure.len()),
        &[plain, posed_],
    );
    // The head's front color at the center of the head's front face (turned 0.15 rad, so a
    // little to one side of its pivot: the face's center moves less than a pixel's worth of
    // its 8 pixels).
    let center = FEET + DVec3::new(0.0, 1.75, -0.25);
    let at = project(&view(), center);
    let p = pixel(&i.posed, at);
    report.check(
        p[0] > 120 && p[1] < 60 && p[2] < 60,
        format!("{what}: the head's front at {at:?} is {p:?}, not red"),
        &[posed_],
    );
    let again = i.posed.compare(&i.again, 0);
    report.check(
        again.differing_pixels == 0,
        format!(
            "{what}: the same frame again differs on {} pixels",
            again.differing_pixels
        ),
        &[posed_, (&named("again"), &i.again)],
    );
    let hidden = i.plain.compare(&i.hidden, 0);
    report.check(
        hidden.differing_pixels == 0,
        format!(
            "{what}: the figure under the floor shows on {} pixels",
            hidden.differing_pixels
        ),
        &[plain, (&named("hidden"), &i.hidden)],
    );
    let (bright, dark) = (mean(&i.posed, &figure), mean(&i.dark, &figure));
    eprintln!("gpu_models: {what}: the figure's brightness {bright:.1} lit, {dark:.1} dark");
    report.check(
        dark < bright * 0.35,
        format!("{what}: in the dark the figure is {dark:.1}, lit {bright:.1}"),
        &[posed_, (&named("dark"), &i.dark)],
    );
    let hat = i.clear_hat.compare(&i.no_hat, 0);
    report.check(
        hat.differing_pixels == 0,
        format!(
            "{what}: a clear outer layer changes {} pixels",
            hat.differing_pixels
        ),
        &[
            (&named("clear_hat"), &i.clear_hat),
            (&named("no_hat"), &i.no_hat),
        ],
    );
    let band = changed(&i.no_hat, &i.posed).len();
    report.check(
        band > 0,
        format!("{what}: the outer layer's band does not show"),
        &[posed_],
    );
    // The hand over the plain scene and over the figure in front of it: where the hand is,
    // the same pixels.
    let hand_pixels = changed(&i.plain, &i.hand);
    report.check(
        hand_pixels.len() >= MIN_FIGURE_PIXELS,
        format!("{what}: the hand changes only {} pixels", hand_pixels.len()),
        &[plain, (&named("hand"), &i.hand)],
    );
    let covered = changed(&i.plain, &i.near).len();
    report.check(
        covered >= MIN_FIGURE_PIXELS,
        format!("{what}: the near figure changes only {covered} pixels"),
        &[plain, (&named("near"), &i.near)],
    );
    let differ = hand_pixels
        .iter()
        .filter(|&&k| {
            let (a, b) = (
                &i.hand.data[k * 4..k * 4 + 3],
                &i.near_hand.data[k * 4..k * 4 + 3],
            );
            a.iter().zip(b).any(|(x, y)| x.abs_diff(*y) > 2)
        })
        .count();
    report.check(
        differ * 100 < hand_pixels.len(),
        format!(
            "{what}: the hand differs over the near figure on {differ} of its {} pixels",
            hand_pixels.len()
        ),
        &[
            (&named("hand"), &i.hand),
            (&named("near_hand"), &i.near_hand),
        ],
    );
}

#[test]
fn entity_models_are_drawn_alike_on_every_tier() {
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
            let what = format!("{label}_{tier}");
            if std::env::var_os("GPU_MODELS_SAVE").is_some() {
                let name = |n: &str| format!("{what}_{n}");
                report.save(&[
                    (&name("plain"), &i.plain),
                    (&name("posed"), &i.posed),
                    (&name("dark"), &i.dark),
                    (&name("near_hand"), &i.near_hand),
                    (&name("hand"), &i.hand),
                ]);
            }
            check_tier(&mut report, &what, i);
        }
        let Some(((best, b), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, t) in rest {
            for (what, x, y) in [
                ("posed", &b.posed, &t.posed),
                ("dark", &b.dark, &t.dark),
                ("near_hand", &b.near_hand, &t.near_hand),
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
