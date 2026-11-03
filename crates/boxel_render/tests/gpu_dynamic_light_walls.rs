//! Dynamic lights baked into the meshes near them (ADR 0063) on every tier. Self-skips without an
//! adapter unless `BOXEL_REQUIRE_GPU=1`.
//!
//! A closed stone room underground (no sky, no emitter: every face dark), split in two by a wall
//! one block thick from floor to ceiling. The camera sits in the stone above the wall and looks
//! down through the ceiling (its faces look into the room, so they are culled) at both halves; a
//! white light hangs in the left half. The meshes are built as the world builds the meshes near a
//! dynamic light: the light of the blocks (`boxel_light::compute`) plus the light's flood
//! (`boxel_light::add_point_lights`), meshed by the real `Mesher` with the world's default AO
//! policy (`AoPolicy::BakedLitOnly`). For every adapter and every tier a local capability check
//! allows (the one of `gpu_tiers.rs`), asserts:
//!
//! - the light lights the left half ([`MIN_LIT`] of the pixels 20 levels brighter than the dark
//!   room) and nothing behind the wall: the right part of the image ([`FAR_FROM`] on) is the dark
//!   room's, pixel for pixel; the same light in the shaders (ADR 0062) lights that part, the fault
//!   this fixes;
//! - an invisible clear emitter of the same light placed at the voxel (`test:light`, Minecraft's
//!   light block) gives the same pixels everywhere, and a visible white lamp there matches the lit
//!   half (mean channel difference at most [`MAX_MEAN_DIFF_VS_LAMP`] levels there);
//! - AO under the light is `AoPolicy::Baked`'s: meshed with `Baked` every pixel the light reaches
//!   is the same (the dark faces differ: `Baked` darkens them at the ambient floor, the world's
//!   default leaves them as every other dark face), and the AO shows (AO strength 0 changes at
//!   least 500 pixels);
//! - every tier draws the same pixels as the best tier of its adapter (0.1 % of the pixels,
//!   channel tolerance 2, as the other tier tests).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_dynamic_light_walls/` (every image with
//! `GPU_DYNAMIC_LIGHT_WALLS_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_light::{LightInput, LightScratch, PaddedLight, PointEmitter, PointScratch, SkyInput};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{AoPolicy, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::{
    DEFAULT_SKY_COLOR, DistanceFog, PointLight, RendererConfig, ViewParams, VoxelRenderer,
};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockPos, BlockRegistry, BlockState, CHUNK_VOLUME,
    ChunkData, FaceTextures, LightTables, LocalPos, RenderShape, TextureId,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_DIFF: f64 = 0.001;
/// The light: white, the demo's `boxel:lamp_white`.
const WHITE: [u8; 3] = [15, 15, 15];
/// The voxel the light hangs in: 4 blocks left of the wall, 2 over the floor.
const LIGHT_VOXEL: [i64; 3] = [12, 6, 16];
/// The wall's x.
const WALL_X: i64 = 16;
/// The image columns from this fraction of the width on show only the right half (behind the
/// wall) and its floor.
const FAR_FROM: f64 = 0.55;
/// The light reaches the left half: at least this fraction of the pixels is lit (measured
/// 0.147).
const MIN_LIT: f64 = 0.1;
/// Mean channel difference (0..255) allowed on the lit half between the dynamic light and a
/// visible lamp at its voxel (the lamp is a cube: it hides a few pixels and shades its
/// neighbors' AO).
const MAX_MEAN_DIFF_VS_LAMP: f64 = 1.0;

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
        label: Some("gpu_dynamic_light_walls"),
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

/// The demo blocks plus `test:light`, a clear emitter of [`WHITE`] that draws nothing.
fn registry() -> (BlockRegistry, BlockState) {
    let mut b = boxel_demo::blocks::builder();
    let light = b
        .add_block(BlockDef {
            name: "test:light".to_string(),
            shape: RenderShape::None,
            model: BlockModel::CUBE,
            collision: BlockCollision::None,
            emission: WHITE,
            light_opacity: 0,
            textures: FaceTextures::all(TextureId(0)),
        })
        .expect("light block");
    (b.build(), light)
}

/// What sits at [`LIGHT_VOXEL`].
#[derive(Clone, Copy, PartialEq)]
enum AtLight {
    Air,
    /// `test:light`.
    Invisible,
    /// `boxel:lamp_white`.
    Lamp,
}

/// The room's chunk (the origin chunk, alone): stone with a hollow x, z in 4..28, y in 4..12,
/// split by a stone wall at x = [`WALL_X`], and `at` at [`LIGHT_VOXEL`].
fn room(registry: &BlockRegistry, light: BlockState, at: AtLight) -> ChunkData {
    let s = |name: &str| registry.by_name(name).unwrap_or_else(|| panic!("{name}"));
    let stone = s("boxel:stone");
    let lamp = s("boxel:lamp_white");
    let dense: Vec<BlockState> = (0..CHUNK_VOLUME)
        .map(|i| {
            let p = LocalPos::from_index(i);
            let (x, y, z) = (i64::from(p.x()), i64::from(p.y()), i64::from(p.z()));
            let inside = (4..28).contains(&x) && (4..12).contains(&y) && (4..28).contains(&z);
            if [x, y, z] == LIGHT_VOXEL {
                return match at {
                    AtLight::Air => BlockState::AIR,
                    AtLight::Invisible => light,
                    AtLight::Lamp => lamp,
                };
            }
            if !inside || x == WALL_X {
                stone
            } else {
                BlockState::AIR
            }
        })
        .collect();
    ChunkData::from_dense(&dense)
}

/// The room's mesh with `at` at the light's voxel, `dynamic` the point lights added as the world
/// adds them near a dynamic light, AO policy `ao`; underground (no sky).
fn mesh(
    registry: &BlockRegistry,
    light: BlockState,
    at: AtLight,
    dynamic: &[PointEmitter],
    ao: AoPolicy,
) -> ChunkMesh {
    let chunk = room(registry, light, at);
    let tables = LightTables::from_registry(registry);
    let mut chunks = [None; 27];
    chunks[boxel_light::box_light::chunk_index(0, 0, 0)] = Some(&chunk);
    let input = LightInput {
        origin: boxel_voxel::ChunkPos::new(0, 0, 0),
        chunks,
        sky: SkyInput::BelowFloor,
        beams: [None; 9],
        tables: &tables,
        colored_sky: false,
    };
    let mut padded = PaddedLight::default();
    boxel_light::compute(&input, &mut LightScratch::new(), &mut padded);
    boxel_light::add_point_lights(&input, dynamic, &mut PointScratch::new(), &mut padded);
    let materials = MaterialTable::from_registry(registry);
    let input = MeshInput {
        center: &chunk,
        neighbors: [None; 26],
        light: Some(&padded),
    };
    Mesher::new().mesh_chunk(&input, &materials, MeshOptions::with_ao(ao))
}

/// From the stone over the wall, looking down at both halves (the left one, with the light, on
/// the left of the image).
fn view() -> ViewParams {
    let eye = DVec3::new(WALL_X as f64 + 0.5, 22.0, 16.0);
    let transform = Transform::from_yaw_pitch(eye, 0.0, -1.45);
    let camera = Camera::default();
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
}

/// [`view`] with AO strength `ao_strength` and, `shader`, the white light drawn in the shaders
/// (ADR 0062) instead of baked.
fn view_with(ao_strength: f32, shader: bool) -> ViewParams {
    let mut v = view();
    if shader {
        assert!(v.light.dynamic.push(PointLight {
            position: I64Vec3::from_array(LIGHT_VOXEL).as_dvec3() + DVec3::splat(0.5),
            emission: WHITE,
        }));
    }
    v.light.ao_strength = ao_strength;
    v
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_dynamic_light_walls"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// The meshes.
struct Scenes {
    registry: BlockRegistry,
    /// The dark room (the world's default AO policy).
    dark: ChunkMesh,
    /// The dark room with every face's AO (what the shader light of ADR 0062 needs).
    dark_baked: ChunkMesh,
    /// The light baked as the world bakes it.
    dynamic: ChunkMesh,
    /// The same with `AoPolicy::Baked`.
    dynamic_baked: ChunkMesh,
    /// An invisible clear emitter at the voxel.
    placed: ChunkMesh,
    /// A white lamp at the voxel.
    lamp: ChunkMesh,
}

/// The images of one tier.
struct TierImages {
    dark: RgbaImage,
    dynamic: RgbaImage,
    dynamic_flat: RgbaImage,
    dynamic_baked: RgbaImage,
    placed: RgbaImage,
    lamp: RgbaImage,
    /// The light in the shaders, over the dark room with every face's AO (ADR 0062).
    shader: RgbaImage,
    /// The dark room with every face's AO, no light.
    shader_dark: RgbaImage,
}

impl TierImages {
    fn named(&self) -> [(&'static str, &RgbaImage); 8] {
        [
            ("dark", &self.dark),
            ("dynamic", &self.dynamic),
            ("dynamic_flat", &self.dynamic_flat),
            ("dynamic_baked", &self.dynamic_baked),
            ("placed", &self.placed),
            ("lamp", &self.lamp),
            ("shader", &self.shader),
            ("shader_dark", &self.shader_dark),
        ]
    }
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    s: &Scenes,
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
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
        &boxel_render::generate_block_textures(s.registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    let mut shoot = |mesh: &ChunkMesh, views: &[ViewParams]| -> Result<Vec<RgbaImage>, String> {
        renderer
            .upsert_chunk(1, I64Vec3::ZERO, Arc::new(mesh.clone()))
            .map_err(|e| e.to_string())?;
        let mut images = Vec::new();
        for v in views {
            render(&device, &queue, &mut renderer, v)?; // warm-up
            images.push(render(&device, &queue, &mut renderer, v)?);
        }
        Ok(images)
    };
    let ao = boxel_render::SceneLight::DEFAULT_AO_STRENGTH;
    let plain = view_with(ao, false);
    let mut images = Vec::new();
    images.extend(shoot(&s.dark, std::slice::from_ref(&plain))?);
    images.extend(shoot(&s.dynamic, &[plain, view_with(0.0, false)])?);
    images.extend(shoot(&s.dynamic_baked, std::slice::from_ref(&plain))?);
    images.extend(shoot(&s.placed, std::slice::from_ref(&plain))?);
    images.extend(shoot(&s.lamp, std::slice::from_ref(&plain))?);
    images.extend(shoot(&s.dark_baked, &[view_with(ao, true), plain])?);
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    let [
        dark,
        dynamic,
        dynamic_flat,
        dynamic_baked,
        placed,
        lamp,
        shader,
        shader_dark,
    ]: [RgbaImage; 8] = images
        .try_into()
        .map_err(|_| "missing images".to_string())?;
    Ok(TierImages {
        dark,
        dynamic,
        dynamic_flat,
        dynamic_baked,
        placed,
        lamp,
        shader,
        shader_dark,
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
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_dynamic_light_walls"),
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
                Ok(()) => eprintln!("gpu_dynamic_light_walls: saved {}", path.display()),
                Err(e) => eprintln!(
                    "gpu_dynamic_light_walls: cannot save {}: {e}",
                    path.display()
                ),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_dynamic_light_walls: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }
}

/// The pixels of `image` as RGB, with their column.
fn rgb(image: &RgbaImage) -> impl Iterator<Item = (u32, [u8; 3])> + '_ {
    let width = image.width.max(1);
    image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .map(move |(i, p)| ((i as u32) % width, [p[0], p[1], p[2]]))
}

/// True for the pixels of a column that shows only the room behind the wall.
fn far(x: u32) -> bool {
    f64::from(x) >= FAR_FROM * f64::from(WIDTH)
}

/// Fraction of pixels of `a` whose brightest channel is at least `by` above `b`'s.
fn brighter_fraction(a: &RgbaImage, b: &RgbaImage, by: u8, keep: impl Fn(u32) -> bool) -> f64 {
    let n = rgb(a)
        .zip(rgb(b))
        .filter(|((x, _), _)| keep(*x))
        .filter(|((_, p), (_, q))| {
            let (p, q) = (p.iter().max().copied(), q.iter().max().copied());
            p.unwrap_or(0) >= q.unwrap_or(0).saturating_add(by)
        })
        .count();
    n as f64 / (a.width * a.height).max(1) as f64
}

/// Pixels of the far columns that differ.
fn far_differences(a: &RgbaImage, b: &RgbaImage) -> usize {
    rgb(a)
        .zip(rgb(b))
        .filter(|((x, p), (_, q))| far(*x) && p != q)
        .count()
}

/// Pixels that differ between `a` and `b` among those where `lit` is 3 levels brighter than
/// `dark` on some channel (the pixels a light reaches; a dark pixel may move a level where
/// quads merge differently).
fn lit_differences(a: &RgbaImage, b: &RgbaImage, lit: &RgbaImage, dark: &RgbaImage) -> usize {
    rgb(a)
        .zip(rgb(b))
        .zip(rgb(lit).zip(rgb(dark)))
        .filter(|(((_, p), (_, q)), ((_, l), (_, d)))| {
            (0..3).any(|c| l[c] >= d[c].saturating_add(3)) && p != q
        })
        .count()
}

/// Mean absolute channel difference over the columns `keep` takes.
fn mean_diff(a: &RgbaImage, b: &RgbaImage, keep: impl Fn(u32) -> bool) -> f64 {
    let (sum, n) = rgb(a).zip(rgb(b)).filter(|((x, _), _)| keep(*x)).fold(
        (0_u64, 0_u64),
        |(s, n), ((_, p), (_, q))| {
            let d: u64 = (0..3).map(|c| u64::from(p[c].abs_diff(q[c]))).sum();
            (s + d, n + 3)
        },
    );
    sum as f64 / n.max(1) as f64
}

fn check_tier(report: &mut Report, what: &str, i: &TierImages) {
    let named: Vec<(String, &RgbaImage)> = i
        .named()
        .into_iter()
        .map(|(n, image)| (format!("{what}_{n}"), image))
        .collect();
    let named: Vec<(&str, &RgbaImage)> = named.iter().map(|(n, i)| (n.as_str(), *i)).collect();
    let near = |x: u32| f64::from(x) < 0.45 * f64::from(WIDTH);
    let lit = brighter_fraction(&i.dynamic, &i.dark, 20, |_| true);
    let behind = far_differences(&i.dynamic, &i.dark);
    let leak = brighter_fraction(&i.shader, &i.shader_dark, 4, far);
    let placed = i.dynamic.compare(&i.placed, 0);
    let lamp = mean_diff(&i.dynamic, &i.lamp, near);
    let baked = lit_differences(&i.dynamic, &i.dynamic_baked, &i.dynamic, &i.dark);
    let baked_all = i.dynamic.compare(&i.dynamic_baked, 0).differing_pixels;
    let ao = i.dynamic.compare(&i.dynamic_flat, 1).differing_pixels;
    eprintln!(
        "gpu_dynamic_light_walls: {what}: lit {lit:.3}, behind the wall {behind} pixels changed \
         (shader light: {leak:.3} lit there), vs invisible emitter {} pixels, vs lamp mean \
         {lamp:.2}, vs Baked {baked} lit pixels ({baked_all} in all), AO pixels {ao}",
        placed.differing_pixels
    );
    if lit < MIN_LIT {
        report.fail(
            format!("{what}: only {lit:.3} of the pixels lit (< {MIN_LIT})"),
            &named,
        );
    }
    if behind > 0 {
        report.fail(
            format!("{what}: the light crossed the wall: {behind} pixels behind it changed"),
            &named,
        );
    }
    if leak < 0.005 {
        report.fail(
            format!("{what}: the shader light should cross the wall here (lit {leak:.3})"),
            &named,
        );
    }
    if placed.differing_pixels > 0 {
        report.fail(
            format!(
                "{what}: the baked light differs from an invisible emitter in {} pixels (max {})",
                placed.differing_pixels, placed.max_channel_diff
            ),
            &named,
        );
    }
    if lamp > MAX_MEAN_DIFF_VS_LAMP {
        report.fail(
            format!(
                "{what}: the lit half differs from a placed lamp by {lamp:.2} levels on average \
                 (> {MAX_MEAN_DIFF_VS_LAMP})"
            ),
            &named,
        );
    }
    if baked > 0 {
        report.fail(
            format!("{what}: AO under the light differs from AoPolicy::Baked in {baked} pixels"),
            &named,
        );
    }
    if ao < 500 {
        report.fail(
            format!("{what}: AO under the light changes only {ao} pixels (< 500)"),
            &named,
        );
    }
}

#[test]
fn a_baked_dynamic_light_stops_at_walls_with_ao_on_every_tier() {
    let (registry, light) = registry();
    let points = [PointEmitter::new(
        BlockPos::from_i64vec3(I64Vec3::from_array(LIGHT_VOXEL)),
        WHITE,
    )];
    let lit = AoPolicy::BakedLitOnly;
    let scenes = Scenes {
        dark: mesh(&registry, light, AtLight::Air, &[], lit),
        dark_baked: mesh(&registry, light, AtLight::Air, &[], AoPolicy::Baked),
        dynamic: mesh(&registry, light, AtLight::Air, &points, lit),
        dynamic_baked: mesh(&registry, light, AtLight::Air, &points, AoPolicy::Baked),
        placed: mesh(&registry, light, AtLight::Invisible, &[], lit),
        lamp: mesh(&registry, light, AtLight::Lamp, &[], lit),
        registry,
    };
    // The baked light is an invisible emitter's, quad for quad; dark faces still elide AO.
    assert_eq!(scenes.dynamic, scenes.placed);
    assert!(scenes.dark.total_quads() < scenes.dark_baked.total_quads());
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
            match render_tier(adapter, &caps, tier, &scenes) {
                Ok(images) => tiers.push((tier, images)),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        for (tier, i) in &tiers {
            rendered += 1;
            let what = format!("{label}_{tier}");
            if std::env::var_os("GPU_DYNAMIC_LIGHT_WALLS_SAVE").is_some() {
                let named: Vec<(String, &RgbaImage)> = i
                    .named()
                    .into_iter()
                    .map(|(n, image)| (format!("{what}_{n}"), image))
                    .collect();
                let named: Vec<(&str, &RgbaImage)> =
                    named.iter().map(|(n, i)| (n.as_str(), *i)).collect();
                report.save(&named);
            }
            check_tier(&mut report, &what, i);
        }
        let Some(((best, b), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, t) in rest {
            for ((name, x), (_, y)) in b.named().into_iter().zip(t.named()) {
                let diff = x.compare(y, 2);
                let exact = x.compare(y, 0);
                eprintln!(
                    "gpu_dynamic_light_walls: {label}: {name} {tier} vs {best}: {} pixels differ \
                     (max channel {}), {:.4} % over 2",
                    exact.differing_pixels,
                    exact.max_channel_diff,
                    diff.fraction() * 100.0,
                );
                if diff.fraction() > MAX_DIFF {
                    report.fail(
                        format!(
                            "{label}: {name} {tier} vs {best}: {:.4} % of pixels differ",
                            diff.fraction() * 100.0
                        ),
                        &[
                            (&format!("{label}_{best}_{name}"), x),
                            (&format!("{label}_{tier}_{name}"), y),
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
