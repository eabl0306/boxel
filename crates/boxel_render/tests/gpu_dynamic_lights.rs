//! Dynamic point lights (ADR 0062) on every tier. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! A closed stone room underground (no sky, no emitter: every face dark) with a pillar and a
//! step on its floor, meshed by the real `Mesher` with its light computed as the world computes
//! it (`boxel_light::compute`). The camera looks down at the floor, the pillar and the step; a
//! white light hangs behind it. For every adapter (filtered by `WGPU_BACKEND` /
//! `WGPU_ADAPTER_NAME`) and every tier a local capability check allows (the one of
//! `gpu_tiers.rs`), asserts:
//!
//! - the dynamic light lights the dark room (at least [`MIN_LIT`] of the pixels 20 levels
//!   brighter than without it: the near half of the room, the far half is out of its reach);
//! - a light out of reach of every quad gives exactly the pixels of no light at all;
//! - AO shows under the light: with the AO the dynamic-light worlds mesh with
//!   (`AoPolicy::Baked`) the image differs from the same light with AO strength 0, and is never
//!   brighter; with AO elided on dark faces (`AoPolicy::BakedLitOnly`) it would not show at all
//!   (the two images are equal), which is why worlds with dynamic lights mesh with `Baked`;
//! - the dynamic light looks like a lamp placed at the same voxel (baked light): mean channel
//!   difference at most [`MAX_MEAN_DIFF_VS_PLACED`] levels;
//! - every tier draws the same pixels as the best tier of its adapter (0.1 % of the pixels,
//!   channel tolerance 2, as the other tier tests).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_dynamic_lights/` (every image with
//! `GPU_DYNAMIC_LIGHTS_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_light::{LightInput, LightScratch, PaddedLight, SkyInput};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{AoPolicy, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::{
    DEFAULT_SKY_COLOR, DistanceFog, PointLight, RendererConfig, ViewParams, VoxelRenderer,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, LightTables, LocalPos};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_DIFF: f64 = 0.001;
/// The dynamic light and the placed lamp: white, the demo's `boxel:lamp_white`.
const WHITE: [u8; 3] = [15, 15, 15];
/// The voxel the light hangs in (behind and above the camera).
const LIGHT_VOXEL: [i64; 3] = [16, 7, 19];
/// Mean channel difference (0..255) allowed between the dynamic light and the placed lamp
/// (measured 0.27 on the RTX 5050, the 780M and llvmpipe).
const MAX_MEAN_DIFF_VS_PLACED: f64 = 1.0;
/// The light reaches the near half of the room: at least this fraction of the pixels is lit
/// (measured 0.30).
const MIN_LIT: f64 = 0.25;

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
        label: Some("gpu_dynamic_lights"),
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

/// The room's chunk (the origin chunk, alone): stone with a hollow x, z in 8..24, y in 4..10, a
/// pillar 3 blocks tall at (16, 4..7, 14), an L-shaped step at x 11..14, z 13 (2 high at
/// x = 11), and (`lamp`) a white lamp at [`LIGHT_VOXEL`].
fn room(registry: &BlockRegistry, lamp: bool) -> ChunkData {
    let s = |name: &str| registry.by_name(name).unwrap_or_else(|| panic!("{name}"));
    let stone = s("boxel:stone");
    let white = s("boxel:lamp_white");
    let dense: Vec<BlockState> = (0..CHUNK_VOLUME)
        .map(|i| {
            let p = LocalPos::from_index(i);
            let (x, y, z) = (i64::from(p.x()), i64::from(p.y()), i64::from(p.z()));
            let inside = (8..24).contains(&x) && (4..10).contains(&y) && (8..24).contains(&z);
            match (x, y, z) {
                _ if !inside => stone,
                _ if lamp && [x, y, z] == LIGHT_VOXEL => white,
                (16, 4..7, 14) | (11..14, 4, 13) | (11, 5, 13) => stone,
                _ => BlockState::AIR,
            }
        })
        .collect();
    ChunkData::from_dense(&dense)
}

/// The room's mesh with AO policy `ao`, lit as the world lights it: no sky (it is underground),
/// dark without the lamp.
fn mesh(registry: &BlockRegistry, lamp: bool, ao: AoPolicy) -> ChunkMesh {
    let chunk = room(registry, lamp);
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
    let mut light = PaddedLight::default();
    boxel_light::compute(&input, &mut LightScratch::new(), &mut light);
    let materials = MaterialTable::from_registry(registry);
    let input = MeshInput {
        center: &chunk,
        neighbors: [None; 26],
        light: Some(&light),
    };
    Mesher::new().mesh_chunk(&input, &materials, MeshOptions::with_ao(ao))
}

/// Looking toward -z and down at the floor, the pillar and the step, the light behind.
fn view() -> ViewParams {
    let eye = DVec3::new(16.5, 8.5, 18.0);
    let transform = Transform::from_yaw_pitch(eye, 0.0, -0.6);
    let camera = Camera::default();
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
}

/// [`view`] with a white dynamic light at `at` and AO strength `ao_strength`.
fn lit_view(at: DVec3, ao_strength: f32) -> ViewParams {
    let mut v = view();
    assert!(v.light.dynamic.push(PointLight {
        position: at,
        emission: WHITE,
    }));
    v.light.ao_strength = ao_strength;
    v
}

/// The center of [`LIGHT_VOXEL`].
fn light_center() -> DVec3 {
    I64Vec3::from_array(LIGHT_VOXEL).as_dvec3() + DVec3::splat(0.5)
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_dynamic_lights"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// The meshes: the dark room with AO on every face and with AO elided on dark faces, and the
/// room with the lamp.
struct Scenes {
    registry: BlockRegistry,
    baked: ChunkMesh,
    elided: ChunkMesh,
    placed: ChunkMesh,
}

/// The images of one tier.
struct TierImages {
    /// Baked AO, no light.
    dark: RgbaImage,
    /// Baked AO, the light.
    lit: RgbaImage,
    /// Baked AO, the light, AO strength 0.
    lit_flat: RgbaImage,
    /// Baked AO, the light out of reach of every quad.
    far: RgbaImage,
    /// AO elided on dark faces, the light.
    elided: RgbaImage,
    /// AO elided on dark faces, the light, AO strength 0.
    elided_flat: RgbaImage,
    /// The lamp placed at the light's voxel, no dynamic light.
    placed: RgbaImage,
}

impl TierImages {
    fn named(&self) -> [(&'static str, &RgbaImage); 7] {
        [
            ("dark", &self.dark),
            ("lit", &self.lit),
            ("lit_flat", &self.lit_flat),
            ("far", &self.far),
            ("elided", &self.elided),
            ("elided_flat", &self.elided_flat),
            ("placed", &self.placed),
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
    let at = light_center();
    let ao = boxel_render::SceneLight::DEFAULT_AO_STRENGTH;
    let far = at + DVec3::new(0.0, 0.0, 100.0);
    let mut baked = shoot(
        &s.baked,
        &[
            view(),
            lit_view(at, ao),
            lit_view(at, 0.0),
            lit_view(far, ao),
        ],
    )?
    .into_iter();
    let mut elided = shoot(&s.elided, &[lit_view(at, ao), lit_view(at, 0.0)])?.into_iter();
    let mut placed = shoot(&s.placed, &[view()])?.into_iter();
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    let next = |images: &mut std::vec::IntoIter<RgbaImage>| {
        images.next().ok_or_else(|| "missing image".to_string())
    };
    Ok(TierImages {
        dark: next(&mut baked)?,
        lit: next(&mut baked)?,
        lit_flat: next(&mut baked)?,
        far: next(&mut baked)?,
        elided: next(&mut elided)?,
        elided_flat: next(&mut elided)?,
        placed: next(&mut placed)?,
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
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_dynamic_lights"),
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
                Ok(()) => eprintln!("gpu_dynamic_lights: saved {}", path.display()),
                Err(e) => eprintln!("gpu_dynamic_lights: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_dynamic_lights: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }
}

fn rgb(image: &RgbaImage) -> impl Iterator<Item = [u8; 3]> + '_ {
    image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| [p[0], p[1], p[2]])
}

/// Fraction of pixels of `a` whose brightest channel is at least `by` above `b`'s.
fn brighter_fraction(a: &RgbaImage, b: &RgbaImage, by: u8) -> f64 {
    let n = rgb(a)
        .zip(rgb(b))
        .filter(|(x, y)| {
            let (x, y) = (x.iter().max().copied(), y.iter().max().copied());
            x.unwrap_or(0) >= y.unwrap_or(0).saturating_add(by)
        })
        .count();
    n as f64 / (a.width * a.height).max(1) as f64
}

/// Pixels where some channel of `a` is more than `tolerance` above `b`'s.
fn brighter_pixels(a: &RgbaImage, b: &RgbaImage, tolerance: u8) -> usize {
    rgb(a)
        .zip(rgb(b))
        .filter(|(x, y)| (0..3).any(|c| x[c] > y[c].saturating_add(tolerance)))
        .count()
}

/// Mean absolute channel difference.
fn mean_diff(a: &RgbaImage, b: &RgbaImage) -> f64 {
    let (sum, n) = rgb(a).zip(rgb(b)).fold((0_u64, 0_u64), |(s, n), (x, y)| {
        let d: u64 = (0..3).map(|c| u64::from(x[c].abs_diff(y[c]))).sum();
        (s + d, n + 3)
    });
    sum as f64 / n.max(1) as f64
}

fn check_tier(report: &mut Report, what: &str, i: &TierImages) {
    let named: Vec<(String, &RgbaImage)> = i
        .named()
        .into_iter()
        .map(|(n, image)| (format!("{what}_{n}"), image))
        .collect();
    let named: Vec<(&str, &RgbaImage)> = named.iter().map(|(n, i)| (n.as_str(), *i)).collect();
    let lit = brighter_fraction(&i.lit, &i.dark, 20);
    let ao_pixels = i.lit.compare(&i.lit_flat, 1).differing_pixels;
    let ao_brighter = brighter_pixels(&i.lit, &i.lit_flat, 1);
    let elided = i.elided.compare(&i.elided_flat, 0).differing_pixels;
    let placed = mean_diff(&i.lit, &i.placed);
    eprintln!(
        "gpu_dynamic_lights: {what}: lit {lit:.3}, AO pixels under the light {ao_pixels} \
         (elided {elided}), vs placed lamp mean diff {placed:.2}"
    );
    if lit < MIN_LIT {
        report.fail(
            format!("{what}: only {lit:.3} of the pixels lit by the dynamic light (< {MIN_LIT})"),
            &named,
        );
    }
    let far = i.far.compare(&i.dark, 0);
    if far.differing_pixels > 0 {
        report.fail(
            format!(
                "{what}: a light out of reach changed {} pixels (max {})",
                far.differing_pixels, far.max_channel_diff
            ),
            &named,
        );
    }
    if ao_pixels < 500 || ao_brighter > 0 {
        report.fail(
            format!(
                "{what}: AO under the light: {ao_pixels} pixels darkened by AO (< 500), \
                 {ao_brighter} brighter with AO"
            ),
            &named,
        );
    }
    if elided > 0 {
        report.fail(
            format!("{what}: AO elided on dark faces still showed in {elided} pixels"),
            &named,
        );
    }
    if placed > MAX_MEAN_DIFF_VS_PLACED {
        report.fail(
            format!(
                "{what}: the dynamic light differs from the placed lamp by {placed:.2} levels \
                 on average (> {MAX_MEAN_DIFF_VS_PLACED})"
            ),
            &named,
        );
    }
}

#[test]
fn a_dynamic_light_lights_a_dark_room_with_ao_on_every_tier() {
    let registry = boxel_demo::blocks::registry();
    let scenes = Scenes {
        baked: mesh(&registry, false, AoPolicy::Baked),
        elided: mesh(&registry, false, AoPolicy::BakedLitOnly),
        placed: mesh(&registry, true, AoPolicy::Baked),
        registry,
    };
    // The room is dark: the elided mesh has no AO at all and merges more.
    assert!(scenes.elided.total_quads() < scenes.baked.total_quads());
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
            if std::env::var_os("GPU_DYNAMIC_LIGHTS_SAVE").is_some() {
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
                    "gpu_dynamic_lights: {label}: {name} {tier} vs {best}: {} pixels differ \
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
