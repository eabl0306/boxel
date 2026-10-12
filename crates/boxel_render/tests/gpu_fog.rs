//! The fog of ADR 0030 on every tier. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`; run with `--test-threads=1` like the other GPU tests.
//!
//! - **Zero cost:** without fog input the depth target is a plain attachment and frames are
//!   what they were; fog that is off, out of reach or fully transparent leaves every pixel
//!   untouched (the composite discards); dropping the fog gives the plain depth back.
//! - **Noise:** `fs_fog_debug_coverage` gives the CPU oracle's coverage and noise bit for bit,
//!   at origins 0, `2^31 - 5`, `-2^40` and near the world limit, at cell corners and points
//!   inside cells (the soft fog), across the rows of a rising threshold and of the edges next to
//!   the band's planes, between ticks; and the soft fog's edge weight at a column of heights.
//! - **March:** on scenes whose depth is known exactly (a wide floor and the sky), each texel of
//!   the march equals the reference march of `boxel_fog::march` (opacity bytes within 3, color
//!   within 0.02, 0.5 % of the texels may differ where a ray grazes a cell edge), and each pixel
//!   equals the reference composite of its texel over the fog-free pixel (2 / 255).
//! - **Tiers:** every tier draws the scenes (flat clouds and volumetric clouds with edges from
//!   below, just under them, inside and above, a camera inside a blanket and inside a soft bank,
//!   an edged bank seen from its top's height, a noisy bank over the floor, with the terrain's
//!   distance fog off too, fog over water, fog behind glass, dusk) like the best tier of its adapter, at half and full resolution (0.1 %
//!   of the pixels, tolerance 2).
//! - **Origins:** the same camera with render origins a few blocks apart draws the same image,
//!   with cubic and soft fog.
//! - **Half resolution:** thin posts in front of dense fog look the same at half and full
//!   resolution (each pixel of a 2x2 block keeps its own depth).
//! - **Shades:** a slab looks the same from above and below unless the layer asks for face
//!   shades; then from above it has the top shade, from below the bottom shade.
//! - **Lifecycle:** resizes, a resolution switch and a rebuilt renderer keep working.
//! - **Smoke:** `RendererSmokeTest { fog: true }` passes on every tier at both levels.
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_fog/` (every image with
//! `GPU_FOG_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_fog::march::{Ray, march, texel};
use boxel_fog::pack::{composite, decode_alpha, decode_color};
use boxel_fog::{FogFade, FogLayer, FogResolution, FogTime, LayerParams, NoiseDims, RayBasis};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, SmokeContext, SmokeLevel, SmokeTest, Tier, floors,
    gpu_tests_required, read_texture_rgba8, run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, UVec2, Vec3};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::fog::{FogInput, FogSettings, debug_coverage, frame_layers};
use boxel_render::{
    DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, RendererSmokeTest, TranslucentSort, ViewParams,
    VoxelRenderer,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, LocalPos, MAX_BLOCK_COORD};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_TIER_DIFF: f64 = 0.001;
/// Floor chunks along x and z (a 256-block floor, two blocks thick, top at y = 2).
const FLOOR_CHUNKS: i64 = 8;
/// Top of the floor.
const FLOOR_TOP: f32 = 2.0;

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

type Errors = Arc<Mutex<Vec<String>>>;

/// A device shaped like the one selection would create for `tier`, collecting uncaptured
/// errors.
fn device_for(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
) -> Result<(wgpu::Device, wgpu::Queue, Errors), String> {
    let request = tier_request(tier, &caps.limits);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu_fog"),
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

fn take_errors(errors: &Errors) -> Result<(), String> {
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("uncaptured errors: {errors:?}"))
    }
}

/// Runs `check` on every tier of every adapter; returns how many tiers ran. Panics with every
/// failure at the end.
fn for_every_tier(
    mut check: impl FnMut(&str, Tier, &wgpu::Device, &wgpu::Queue, &AdapterCaps, &mut Report),
) -> Report {
    let (_instance, adapters) = adapters();
    let mut report = Report::new();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return report;
    }
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let (device, queue, errors) = match device_for(adapter, &caps, tier) {
                Ok(d) => d,
                Err(e) => {
                    report.fail(format!("{label} {tier}: {e}"), &[]);
                    continue;
                }
            };
            check(&label, tier, &device, &queue, &caps, &mut report);
            if let Err(e) = take_errors(&errors) {
                report.fail(format!("{label} {tier}: {e}"), &[]);
            }
            report.tiers += 1;
        }
    }
    report
}

// ---------------------------------------------------------------- scenes

/// The real mesher's mesh of one chunk whose blocks `block(local)` gives (air around it).
fn mesh_chunk(
    registry: &BlockRegistry,
    block: impl Fn(u32, u32, u32) -> &'static str,
) -> ChunkMesh {
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for x in 0..32 {
        for y in 0..32 {
            for z in 0..32 {
                let name = block(x, y, z);
                if name != "air" {
                    dense[LocalPos::new(x, y, z).index()] = registry.by_name(name).expect(name);
                }
            }
        }
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

/// What a test world holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum World {
    /// The wide floor (top at y = 2) and the sky.
    Floor,
    /// The floor with thin stone posts 120 blocks along +z of the origin chunk.
    Posts,
    /// A water pool and a glass box on the floor of chunk (0, 0, 0).
    Pool,
}

/// The chunks of a world: `(chunk position, mesh)`.
fn world(registry: &BlockRegistry, kind: World) -> Vec<(I64Vec3, ChunkMesh)> {
    let floor = mesh_chunk(
        registry,
        |_, y, _| if y < 2 { "boxel:stone" } else { "air" },
    );
    let mut out = Vec::new();
    for cx in -FLOOR_CHUNKS / 2..FLOOR_CHUNKS / 2 {
        for cz in -FLOOR_CHUNKS / 2..FLOOR_CHUNKS / 2 {
            let special = match kind {
                World::Posts if (cx, cz) == (0, 3) => Some(mesh_chunk(registry, |x, y, z| {
                    let post = y < 14 && x % 4 == 1 && z == 8;
                    if y < 2 || post { "boxel:stone" } else { "air" }
                })),
                World::Pool if (cx, cz) == (0, 0) => Some(mesh_chunk(registry, |x, y, z| {
                    let pool = (8..16).contains(&x) && (8..16).contains(&z) && (2..5).contains(&y);
                    let rim = pool && (x == 8 || x == 15 || z == 8 || z == 15 || y == 2);
                    let glass =
                        (18..24).contains(&x) && (8..14).contains(&z) && (2..8).contains(&y);
                    if y < 2 || rim {
                        "boxel:stone"
                    } else if pool {
                        "boxel:water"
                    } else if glass {
                        "boxel:glass"
                    } else {
                        "air"
                    }
                })),
                _ => None,
            };
            out.push((
                I64Vec3::new(cx, 0, cz),
                special.unwrap_or_else(|| floor.clone()),
            ));
        }
    }
    out
}

/// A camera at `eye` looking at `target`, for a `WIDTH x HEIGHT` target.
fn look(eye: DVec3, target: DVec3) -> ViewParams {
    let d = (target - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
}

/// The view with the terrain's distance fog off: the terrain, and the layers faded like the
/// scene (`FogFade::Scene`), end at the view distance at once.
fn no_distance_fog(mut view: ViewParams) -> ViewParams {
    [view.fog_start, view.fog_end] = DistanceFog::off().range(Camera::default().view_distance);
    view
}

/// A dusk light and sky.
fn dusk(mut view: ViewParams) -> ViewParams {
    view.light.sky = [0.95, 0.55, 0.32];
    view.fog_color = [0.85, 0.45, 0.30, 1.0];
    view.clear_color = view.fog_color;
    view
}

/// High flat clouds: 6x4x6 cells, crisp.
fn clouds() -> FogLayer {
    let mut l = FogLayer::new(24, 28);
    l.cell = [6, 4, 6];
    l.noise.scale = 2;
    l.noise.seed = 5;
    l.threshold = 0.5;
    l.density = 0.6;
    l.max_distance = 400.0;
    l.fade = FogFade::Range {
        start: 250.0,
        end: 400.0,
    };
    l
}

/// A noisy bank of 2-block cells over the floor.
fn bank() -> FogLayer {
    let mut l = FogLayer::new(2, 14);
    l.cell = [2, 2, 2];
    l.noise.dims = NoiseDims::Volume;
    l.noise.scale = 4;
    l.noise.octaves = 2;
    l.noise.seed = 9;
    l.threshold = 0.45;
    l.softness = 0.3;
    l.density = 0.25;
    l.color = [0.9, 0.92, 1.0];
    l.wind = [1.0, 0.0, 0.5];
    l
}

/// The bank with soft fog within 12 blocks of the camera.
fn soft_bank() -> FogLayer {
    let mut l = bank();
    l.soft_distance = 12.0;
    l
}

/// Volumetric clouds: 6-block cells in a 36-block band over 3D noise, a rising threshold, uneven
/// bottoms and tops (edges of 6 and 12 blocks), a gray base and a lit top, soft within 10
/// blocks.
fn cumulus() -> FogLayer {
    let mut l = FogLayer::new(24, 60);
    l.cell = [6, 6, 6];
    l.noise.dims = NoiseDims::Volume;
    l.noise.scale = 4;
    l.noise.octaves = 2;
    l.noise.seed = 31;
    l.threshold = 0.5;
    l.threshold_rise = 0.35;
    (l.edge_bottom, l.edge_top) = (6, 12);
    l.softness = 0.2;
    l.density = 0.35;
    l.height_shade = [0.6, 1.0];
    l.soft_distance = 10.0;
    l.wind = [2.0, 0.0, 0.0];
    l.max_distance = 400.0;
    l.fade = FogFade::Range {
        start: 250.0,
        end: 400.0,
    };
    l
}

/// The soft bank from y 2 to 18 with a top edge of half its height and a bottom edge of a cell:
/// an uneven skyline of cubes instead of a plane at y 18.
fn edged_bank() -> FogLayer {
    let mut l = soft_bank();
    (l.min_y, l.max_y) = (2, 18);
    (l.edge_bottom, l.edge_top) = (2, 8);
    l
}

/// A blanket that fills every cell, seen about 20 blocks through.
fn blanket() -> FogLayer {
    let mut l = FogLayer::new(0, 32);
    l.cell = [2, 2, 2];
    l.density = boxel_fog::density_for_visibility(20.0);
    l
}

/// One shot: a world, a camera and the fog layers.
struct Shot {
    name: &'static str,
    world: World,
    view: ViewParams,
    layers: Vec<FogLayer>,
    tick: u64,
    /// The depth is known exactly (the floor and the sky): the march is compared texel by
    /// texel with the reference.
    analytic: bool,
}

fn shots() -> Vec<Shot> {
    let shot = |name, world, view, layers, analytic| Shot {
        name,
        world,
        view,
        layers,
        tick: 1234,
        analytic,
    };
    vec![
        shot(
            "clouds_below",
            World::Floor,
            look(DVec3::new(0.5, 10.0, 0.5), DVec3::new(30.0, 40.0, 80.0)),
            vec![clouds()],
            true,
        ),
        shot(
            "clouds_inside",
            World::Floor,
            look(DVec3::new(3.3, 26.5, 2.7), DVec3::new(60.0, 22.0, 70.0)),
            vec![clouds()],
            true,
        ),
        shot(
            "clouds_above",
            World::Floor,
            look(DVec3::new(-5.5, 45.0, -9.5), DVec3::new(40.0, 10.0, 60.0)),
            vec![clouds(), bank()],
            true,
        ),
        shot(
            "cumulus_below",
            World::Floor,
            look(DVec3::new(0.5, 10.0, 0.5), DVec3::new(30.0, 50.0, 80.0)),
            vec![cumulus()],
            true,
        ),
        shot(
            "cumulus_under",
            World::Floor,
            look(DVec3::new(0.5, 22.0, 0.5), DVec3::new(50.0, 30.0, 80.0)),
            vec![cumulus()],
            true,
        ),
        shot(
            "cumulus_inside",
            World::Floor,
            look(DVec3::new(3.3, 35.5, 2.7), DVec3::new(60.0, 30.0, 70.0)),
            vec![cumulus()],
            true,
        ),
        shot(
            "cumulus_above",
            World::Floor,
            look(DVec3::new(-5.5, 70.0, -9.5), DVec3::new(40.0, 10.0, 60.0)),
            vec![soft_bank(), cumulus()],
            true,
        ),
        shot(
            "soft_inside",
            World::Floor,
            look(DVec3::new(10.3, 7.5, 6.7), DVec3::new(40.0, 6.0, 40.0)),
            vec![soft_bank()],
            true,
        ),
        shot(
            "bank_top",
            World::Floor,
            look(DVec3::new(0.5, 18.0, 0.5), DVec3::new(60.0, 17.0, 80.0)),
            vec![edged_bank()],
            true,
        ),
        shot(
            "inside_blanket",
            World::Floor,
            look(DVec3::new(16.3, 8.5, 16.7), DVec3::new(40.0, 4.0, 40.0)),
            vec![blanket()],
            true,
        ),
        shot(
            "bank",
            World::Floor,
            look(DVec3::new(-20.5, 22.0, -20.5), DVec3::new(16.0, 2.0, 16.0)),
            vec![bank()],
            true,
        ),
        shot(
            "bank_no_distance_fog",
            World::Floor,
            no_distance_fog(look(
                DVec3::new(-20.5, 22.0, -20.5),
                DVec3::new(16.0, 2.0, 16.0),
            )),
            vec![bank()],
            true,
        ),
        shot(
            "pool",
            World::Pool,
            look(DVec3::new(30.0, 16.0, 30.0), DVec3::new(14.0, 3.0, 11.0)),
            vec![bank()],
            false,
        ),
        shot(
            "glass",
            World::Pool,
            look(DVec3::new(21.0, 5.5, 2.0), DVec3::new(21.0, 4.0, 40.0)),
            vec![bank()],
            false,
        ),
        shot(
            "dusk",
            World::Floor,
            dusk(look(
                DVec3::new(-20.5, 22.0, -20.5),
                DVec3::new(16.0, 2.0, 16.0),
            )),
            vec![bank(), clouds()],
            true,
        ),
        shot(
            "posts",
            World::Posts,
            look(DVec3::new(5.5, 8.0, -0.5), DVec3::new(5.5, 8.0, 200.0)),
            vec![FogLayer {
                density: boxel_fog::density_for_visibility(250.0),
                ..blanket()
            }],
            false,
        ),
    ]
}

// ---------------------------------------------------------------- rendering

fn new_renderer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tier: Tier,
    size: UVec2,
) -> Result<VoxelRenderer, String> {
    let registry = boxel_demo::blocks::registry();
    let config = RendererConfig {
        arena_quads: 1 << 18,
        max_chunk_slots: 256,
        translucent_sort: TranslucentSort::Inline,
    };
    VoxelRenderer::new(
        device,
        queue,
        tier,
        &config,
        &boxel_render::generate_block_textures(registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        size,
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))
}

fn load_world(renderer: &mut VoxelRenderer, chunks: &[(I64Vec3, ChunkMesh)]) -> Result<(), String> {
    renderer.clear_chunks();
    for (i, (pos, mesh)) in chunks.iter().enumerate() {
        renderer
            .upsert_chunk(i as u64, *pos * 32, Arc::new(mesh.clone()))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Renders one frame and reads the main color back.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_fog"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

fn fog_input(layers: Vec<FogLayer>, resolution: FogResolution, tick: u64) -> FogInput {
    FogInput {
        settings: Arc::new(FogSettings {
            resolution,
            ..FogSettings::new(layers)
        }),
        time: tick.into(),
    }
}

// ---------------------------------------------------------------- the reference

fn srgb_to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    255.0
        * if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
}

/// The view depth of the floor (or the sky, `INFINITY`) under the ray of pixel center `px`, and
/// whether it is safely away from the floor's edges.
fn floor_depth(basis: &RayBasis, eye: Vec3, origin: I64Vec3, px: [f32; 2]) -> (f32, bool) {
    let ndc = [
        px[0] / WIDTH as f32 * 2.0 - 1.0,
        1.0 - px[1] / HEIGHT as f32 * 2.0,
    ];
    let dir = basis.dir(ndc[0], ndc[1]);
    let top = FLOOR_TOP - origin.y as f32;
    if dir[1] >= -1e-6 || eye.y <= top {
        return (f32::INFINITY, true);
    }
    let t = (top - eye.y) / dir[1];
    let half = (FLOOR_CHUNKS * 16) as f32;
    let x = eye.x + t * dir[0] + origin.x as f32;
    let z = eye.z + t * dir[2] + origin.z as f32;
    let margin = 1.0;
    let inside = x > -half + margin && x < half - margin && z > -half + margin && z < half - margin;
    let outside =
        x < -half - margin || x > half + margin || z < -half - margin || z > half + margin;
    if inside {
        (t, true)
    } else {
        (f32::INFINITY, outside)
    }
}

/// Compares the march texels of an analytic shot with the reference march and the fog image
/// with the reference composite of those texels over the fog-free image.
fn check_against_reference(
    what: &str,
    shot: &Shot,
    texels: &(UVec2, Vec<[u32; 2]>),
    plain: &RgbaImage,
    fogged: &RgbaImage,
    report: &mut Report,
) {
    let settings = FogSettings::new(shot.layers.clone());
    let means: Vec<_> = settings
        .layers
        .iter()
        .map(boxel_fog::mean_coverage)
        .collect();
    let layers = frame_layers(&settings, &means, &shot.view, shot.tick);
    let basis = RayBasis::new(
        shot.view.view_proj.to_cols_array_2d(),
        shot.view.eye_rel.to_array(),
    )
    .expect("basis");
    let sky = [
        shot.view.fog_color[0],
        shot.view.fog_color[1],
        shot.view.fog_color[2],
    ];
    let (size, words) = texels;
    let (mut compared, mut off) = (0usize, 0usize);
    let mut worst = String::new();
    for ty in 0..size.y {
        for tx in 0..size.x {
            let mut depths = [0.0; 4];
            let mut clean = true;
            for k in 0..4u32 {
                let px = [
                    ((2 * tx + (k & 1)) as f32 + 0.5).min(WIDTH as f32 - 0.5),
                    ((2 * ty + (k >> 1)) as f32 + 0.5).min(HEIGHT as f32 - 0.5),
                ];
                let (t, ok) = floor_depth(&basis, shot.view.eye_rel, shot.view.render_origin, px);
                depths[k as usize] = t;
                clean &= ok;
            }
            if !clean {
                continue;
            }
            let center = [(2 * tx + 1) as f32, (2 * ty + 1) as f32];
            let ndc = [
                center[0] / WIDTH as f32 * 2.0 - 1.0,
                1.0 - center[1] / HEIGHT as f32 * 2.0,
            ];
            let ray = Ray {
                eye: shot.view.eye_rel.to_array(),
                dir: basis.dir(ndc[0], ndc[1]),
                depths,
            };
            let want = texel(&march(&ray, &layers, sky, settings.max_steps));
            let got = words[(ty * size.x + tx) as usize];
            compared += 1;
            let mut bad = false;
            for k in 0..4 {
                let (a, b) = ((want[1] >> (8 * k)) & 0xff, (got[1] >> (8 * k)) & 0xff);
                bad |= a.abs_diff(b) > 3;
            }
            let (cw, cg) = (decode_color(want[0]), decode_color(got[0]));
            let opaque = (0..4).any(|k| decode_alpha(((got[1] >> (8 * k)) & 0xff) as u8) > 0.01);
            bad |= opaque && (0..3).any(|c| (cw[c] - cg[c]).abs() > 0.02);
            if bad {
                off += 1;
                if worst.is_empty() {
                    worst = format!("texel ({tx}, {ty}): want {want:08x?}, got {got:08x?}");
                }
            }
            // The composite of the GPU's own texel over the fog-free pixel.
            for k in 0..4u32 {
                let (x, y) = (2 * tx + (k & 1), 2 * ty + (k >> 1));
                if x >= WIDTH || y >= HEIGHT {
                    continue;
                }
                let i = ((y * WIDTH + x) * 4) as usize;
                let dst = [0, 1, 2].map(|c| srgb_to_linear(plain.data[i + c]));
                let expected = composite(got, k as usize, dst).unwrap_or(dst);
                for (c, &linear) in expected.iter().enumerate() {
                    let e = linear_to_srgb(linear);
                    let g = f32::from(fogged.data[i + c]);
                    if (e - g).abs() > 2.6 {
                        report.fail(
                            format!(
                                "{what}: pixel ({x}, {y}) channel {c}: composite {e:.1}, drawn {g}"
                            ),
                            &[("plain", plain), ("fogged", fogged)],
                        );
                        return;
                    }
                }
            }
        }
    }
    eprintln!(
        "gpu_fog: {what}: {off} of {compared} texels differ from the reference march {worst}"
    );
    if compared < 1000 || off as f64 > 0.005 * compared as f64 {
        report.fail(
            format!("{what}: {off} of {compared} texels differ from the reference march: {worst}"),
            &[("fogged", fogged)],
        );
    }
}

// ---------------------------------------------------------------- reports

/// Collects failures and saves the images involved.
struct Report {
    failures: Vec<String>,
    dir: PathBuf,
    tiers: u32,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_fog"),
            tiers: 0,
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
                Ok(()) => eprintln!("gpu_fog: saved {}", path.display()),
                Err(e) => eprintln!("gpu_fog: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_fog: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }

    /// Fails if more than `max` of the pixels differ by more than `tolerance`.
    fn check_close(
        &mut self,
        what: &str,
        a: (&str, &RgbaImage),
        b: (&str, &RgbaImage),
        tolerance: u8,
        max: f64,
    ) {
        let diff = a.1.compare(b.1, tolerance);
        eprintln!(
            "gpu_fog: {what}: {:.4} % of pixels differ, max channel {}",
            diff.fraction() * 100.0,
            diff.max_channel_diff
        );
        if diff.fraction() > max {
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

    fn check_identical(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        if a.1.data != b.1.data {
            let diff = a.1.compare(b.1, 0);
            self.fail(
                format!(
                    "{what}: {} pixels changed (max channel {})",
                    diff.differing_pixels, diff.max_channel_diff
                ),
                &[a, b],
            );
        }
    }

    fn finish(self) {
        assert!(
            self.failures.is_empty(),
            "{} failures:\n{}",
            self.failures.len(),
            self.failures.join("\n")
        );
        assert!(
            self.tiers > 0 || !gpu_tests_required(),
            "BOXEL_REQUIRE_GPU=1 but no tier could run"
        );
    }
}

// ---------------------------------------------------------------- tests

/// Without fog input nothing of the fog exists; fog that is off, out of reach or transparent
/// changes no pixel; dropping the fog restores the plain depth target.
#[test]
fn fog_costs_nothing_when_absent_off_or_out_of_reach() {
    let registry = boxel_demo::blocks::registry();
    let chunks = world(&registry, World::Floor);
    let view = look(DVec3::new(-20.5, 22.0, -20.5), DVec3::new(16.0, 2.0, 16.0));
    for_every_tier(|label, tier, device, queue, _, report| {
        let mut run = || -> Result<(), String> {
            let mut renderer = new_renderer(device, queue, tier, UVec2::new(WIDTH, HEIGHT))?;
            load_world(&mut renderer, &chunks)?;
            let plain = render(device, queue, &mut renderer, &view)?;
            if renderer
                .depth_usage()
                .contains(wgpu::TextureUsages::TEXTURE_BINDING)
            {
                return Err("the depth target is sampled without fog".into());
            }
            let name = |what: &str| format!("{label}_{tier}_{what}");
            let mut off = fog_input(vec![bank()], FogResolution::Half, 0);
            Arc::make_mut(&mut off.settings).enabled = false;
            let mut far = clouds();
            (far.min_y, far.max_y, far.max_distance) = (900, 904, 100.0);
            let mut clear = bank();
            clear.density = 1e-9;
            for (what, input) in [
                ("off", off),
                ("out of reach", fog_input(vec![far], FogResolution::Half, 0)),
                (
                    "transparent",
                    fog_input(vec![clear], FogResolution::Full, 0),
                ),
            ] {
                renderer.set_fog(device, Some(input));
                let image = render(device, queue, &mut renderer, &view)?;
                let passes = renderer.stats().fog_passes;
                if what == "transparent" && passes != 2 {
                    return Err(format!(
                        "{what}: {passes} fog passes, want 2 (the discard path)"
                    ));
                }
                if what != "transparent" && passes != 0 {
                    return Err(format!("{what}: {passes} fog passes"));
                }
                report.check_identical(
                    &format!("{label} {tier}: fog {what}"),
                    (&name("plain"), &plain),
                    (&name(what), &image),
                );
            }
            renderer.set_fog(
                device,
                Some(fog_input(vec![bank()], FogResolution::Half, 0)),
            );
            let fogged = render(device, queue, &mut renderer, &view)?;
            if renderer.stats().fog_passes != 2 || fogged.data == plain.data {
                return Err("the bank changed nothing".into());
            }
            renderer.set_fog(device, None);
            if renderer
                .depth_usage()
                .contains(wgpu::TextureUsages::TEXTURE_BINDING)
            {
                return Err("the depth target stays sampled after the fog".into());
            }
            let again = render(device, queue, &mut renderer, &view)?;
            report.check_identical(
                &format!("{label} {tier}: fog dropped"),
                (&name("plain"), &plain),
                (&name("dropped"), &again),
            );
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{label} {tier}: {e}"), &[]);
        }
    })
    .finish();
}

/// `fs_fog_debug_coverage` equals the CPU oracle bit for bit, far from the origin too: at the
/// cells' corners and at points inside them (what the soft fog reads), on rows below, inside
/// and above a band whose threshold rises and on the rows of its edges, at whole ticks and
/// between them; and the soft fog's edge weight over a column of heights through the band.
#[test]
fn the_gpu_noise_is_the_oracle_bit_for_bit() {
    let mut layers = Vec::new();
    for (cell, scale, octaves, dims, rise, edges) in [
        ([1, 1, 1], 1, 1, NoiseDims::Flat, 0.0, (0, 0)),
        ([12, 4, 12], 4, 1, NoiseDims::Flat, 0.5, (6, 10)),
        ([2, 2, 2], 8, 2, NoiseDims::Volume, 0.0, (0, 3)),
        ([3, 5, 7], 64, 2, NoiseDims::Volume, 0.35, (5, 17)),
    ] {
        let mut l = FogLayer::new(0, i64::from(cell[1]) * 8);
        l.cell = cell;
        l.noise.scale = scale;
        l.noise.octaves = octaves;
        l.noise.dims = dims;
        l.noise.seed = 0x5eed_0000_0000_0001 + u64::from(scale);
        l.threshold = 0.4;
        l.threshold_rise = rise;
        (l.edge_bottom, l.edge_top) = edges;
        l.softness = 0.25;
        l.wind = [2.5, -0.75, -9.0];
        layers.push(l);
    }
    let origins = [
        [0i64, 0, 0],
        [(1 << 31) - 5, 70, -(1 << 31) + 3],
        [-(1 << 40), -3, 1 << 40],
        [MAX_BLOCK_COORD - 100, 12, -(MAX_BLOCK_COORD - 100)],
    ];
    let size = [64u32, 48];
    // (time, row of the grid relative to the origin's cell, point inside the cells)
    let probes = [
        (FogTime::from(0), 3, 0),
        (FogTime::from(0), -40, 1234),
        (FogTime::new(777_777, 40_000), 6, 4095),
        (FogTime::new(777_777, 40_000), 2, 0),
    ];
    for_every_tier(|label, _tier, device, queue, _, report| {
        for (li, layer) in layers.iter().enumerate() {
            for origin in origins {
                // The rows along the band's planes and the next ones in.
                let band = LayerParams::new(layer, origin, 0).band_cells;
                let edge_probes = [
                    (FogTime::new(9, 123), band[1] - 1, 0),
                    (FogTime::new(9, 123), band[1] - 2, 2048),
                    (FogTime::new(9, 123), band[0], 0),
                    (FogTime::new(9, 123), band[0] + 1, 4095),
                ];
                for (time, row, sub) in probes.into_iter().chain(edge_probes) {
                    let first = [-32, row, -24];
                    let params = LayerParams::new(layer, origin, time);
                    let y0 = (params.band[0] - 4.0).clamp(-1.0e6, 1.0e6);
                    let got = match debug_coverage(
                        device, queue, layer, origin, time, first, sub, y0, size,
                    ) {
                        Ok(g) => g,
                        Err(e) => {
                            report.fail(format!("{label}: debug coverage: {e}"), &[]);
                            return;
                        }
                    };
                    let mut bad = 0;
                    for (i, g) in got.iter().enumerate() {
                        let (x, z) = (i as u32 % size[0], i as u32 / size[0]);
                        let d = [first[0] + x as i32, first[1], first[2] + z as i32];
                        let n = boxel_fog::noise::noise_at(&params, d, [sub; 3]);
                        let y = y0 + (x + 64 * z) as f32 * 0.0625;
                        let want = [
                            boxel_fog::noise::coverage_from_noise(
                                n,
                                boxel_fog::noise::row_threshold(&params, d[1]),
                                params.gain,
                            ),
                            n as u32,
                            boxel_fog::noise::point_edge(&params, y),
                        ];
                        if *g != want {
                            if bad == 0 {
                                eprintln!(
                                    "gpu_fog: {label}: layer {li} origin {origin:?} {time:?} \
                                     cell {d:?} point {sub}: want {want:?}, got {g:?}"
                                );
                            }
                            bad += 1;
                        }
                    }
                    if bad > 0 {
                        report.fail(
                            format!(
                                "{label}: layer {li} origin {origin:?} {time:?} point {sub}: \
                                 {bad} cells differ from the oracle"
                            ),
                            &[],
                        );
                    }
                }
            }
        }
    })
    .finish();
}

/// Images of one tier: per shot, without fog, at half and at full resolution.
struct ShotImages {
    plain: RgbaImage,
    half: RgbaImage,
    full: RgbaImage,
}

/// Every shot on every tier: the march against the reference where the depth is analytic, and
/// every tier against the best one of its adapter.
#[test]
fn every_tier_draws_the_fog_of_the_reference() {
    let registry = boxel_demo::blocks::registry();
    let shots = shots();
    let worlds: Vec<(World, Vec<(I64Vec3, ChunkMesh)>)> = [World::Floor, World::Posts, World::Pool]
        .into_iter()
        .map(|w| (w, world(&registry, w)))
        .collect();
    let save = std::env::var_os("GPU_FOG_SAVE").is_some();
    let mut per_adapter: Vec<(String, Vec<(Tier, Vec<ShotImages>)>)> = Vec::new();
    let mut report = for_every_tier(|label, tier, device, queue, _, report| {
        let mut run = || -> Result<Vec<ShotImages>, String> {
            let mut renderer = new_renderer(device, queue, tier, UVec2::new(WIDTH, HEIGHT))?;
            let mut loaded = None;
            let mut out = Vec::new();
            for shot in &shots {
                if loaded != Some(shot.world) {
                    let chunks = &worlds
                        .iter()
                        .find(|(w, _)| *w == shot.world)
                        .expect("world")
                        .1;
                    load_world(&mut renderer, chunks)?;
                    loaded = Some(shot.world);
                }
                renderer.set_fog(device, None);
                let plain = render(device, queue, &mut renderer, &shot.view)?;
                let mut images = Vec::new();
                for resolution in [FogResolution::Half, FogResolution::Full] {
                    renderer.set_fog(
                        device,
                        Some(fog_input(shot.layers.clone(), resolution, shot.tick)),
                    );
                    let image = render(device, queue, &mut renderer, &shot.view)?;
                    if renderer.stats().fog_passes != 2 {
                        return Err(format!("{}: no fog passes", shot.name));
                    }
                    if resolution == FogResolution::Half && shot.analytic {
                        let texels = renderer
                            .fog_texels(device, queue)
                            .ok_or("no fog target")??;
                        check_against_reference(
                            &format!("{label} {tier} {}", shot.name),
                            shot,
                            &texels,
                            &plain,
                            &image,
                            report,
                        );
                    }
                    images.push(image);
                }
                let full = images.pop().expect("full");
                let half = images.pop().expect("half");
                if save {
                    report.save(&[
                        (&format!("{label}_{tier}_{}_plain", shot.name), &plain),
                        (&format!("{label}_{tier}_{}_half", shot.name), &half),
                        (&format!("{label}_{tier}_{}_full", shot.name), &full),
                    ]);
                }
                out.push(ShotImages { plain, half, full });
            }
            Ok(out)
        };
        match run() {
            Ok(images) => match per_adapter.iter_mut().find(|(l, _)| l == label) {
                Some((_, tiers)) => tiers.push((tier, images)),
                None => per_adapter.push((label.to_string(), vec![(tier, images)])),
            },
            Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
        }
    });
    for (label, tiers) in &per_adapter {
        // Thin posts in front of dense fog: the 2x2 blocks keep each pixel's depth.
        if let Some((tier, images)) = tiers.first() {
            let i = shots.iter().position(|s| s.name == "posts").expect("posts");
            // The posts (about 2 pixels wide) still show through the fog: darker than the fog
            // around them at half resolution too.
            let post: Vec<usize> = images[i]
                .plain
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .filter(|(_, p)| p[0] < 120 && p[1] < 120 && p[2] < 120)
                .map(|(px, _)| px)
                .collect();
            let shown = post
                .iter()
                .filter(|&&px| images[i].half.data[4 * px] < 240)
                .count();
            eprintln!(
                "gpu_fog: {label} {tier}: {shown} of {} post pixels show through",
                post.len()
            );
            if post.len() < 40 || shown * 10 < post.len() * 9 {
                report.fail(
                    format!("{label} {tier}: {shown} of {} post pixels show", post.len()),
                    &[
                        ("posts_plain", &images[i].plain),
                        ("posts_half", &images[i].half),
                    ],
                );
            }
            report.check_close(
                &format!("{label} {tier}: posts at half vs full resolution"),
                ("posts_half", &images[i].half),
                ("posts_full", &images[i].full),
                12,
                0.02,
            );
            for (s, img) in shots.iter().zip(images) {
                if img.half.data == img.plain.data {
                    report.fail(format!("{label} {tier}: {}: no fog drawn", s.name), &[]);
                }
            }
        }
        let Some(((best, best_images), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, images) in rest {
            for ((shot, a), b) in shots.iter().zip(best_images).zip(images) {
                for (res, x, y) in [("half", &a.half, &b.half), ("full", &a.full, &b.full)] {
                    report.check_close(
                        &format!("{label}: {tier} vs {best} ({} {res})", shot.name),
                        (&format!("{label}_{best}_{}_{res}", shot.name), x),
                        (&format!("{label}_{tier}_{}_{res}", shot.name), y),
                        2,
                        MAX_TIER_DIFF,
                    );
                }
            }
        }
    }
    report.finish();
}

/// The same camera described from render origins a few blocks apart draws the same fog.
#[test]
fn render_origins_a_few_blocks_apart_draw_the_same_fog() {
    let registry = boxel_demo::blocks::registry();
    let chunks = world(&registry, World::Floor);
    let (eye, target) = (DVec3::new(-20.5, 22.0, -20.5), DVec3::new(16.0, 2.0, 16.0));
    let shifted = |d: I64Vec3| {
        let dir = (target - eye).as_vec3();
        let camera = Camera::default();
        let transform =
            Transform::from_yaw_pitch(eye, (-dir.x).atan2(-dir.z), dir.y.atan2(dir.x.hypot(dir.z)));
        let mut cv = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
        cv.render_origin += d;
        cv.eye_rel -= d.as_vec3();
        cv.view = boxel_math::projection::look_to(cv.eye_rel, cv.forward, transform.up());
        cv.view_proj = cv.proj * cv.view;
        cv.frustum = boxel_math::Frustum::from_view_proj_reverse_z(cv.view_proj);
        cv.frustum
            .set_max_distance(cv.eye_rel, cv.forward, camera.view_distance);
        ViewParams::from_camera(&cv, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
    };
    let views = [
        shifted(I64Vec3::ZERO),
        shifted(I64Vec3::new(3, -2, 5)),
        shifted(I64Vec3::new(-7, 4, -1)),
    ];
    for_every_tier(|label, tier, device, queue, _, report| {
        let mut run = || -> Result<(), String> {
            let mut renderer = new_renderer(device, queue, tier, UVec2::new(WIDTH, HEIGHT))?;
            load_world(&mut renderer, &chunks)?;
            for (what, layers) in [
                ("cubic", vec![bank(), clouds()]),
                ("soft", vec![soft_bank(), cumulus()]),
            ] {
                renderer.set_fog(device, Some(fog_input(layers, FogResolution::Half, 99)));
                let first = render(device, queue, &mut renderer, &views[0])?;
                for (i, view) in views.iter().enumerate().skip(1) {
                    let image = render(device, queue, &mut renderer, view)?;
                    report.check_close(
                        &format!("{label} {tier}: {what} origin shift {i}"),
                        (&format!("{what}_origin0"), &first),
                        (&format!("{what}_origin{i}"), &image),
                        2,
                        0.002,
                    );
                }
            }
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{label} {tier}: {e}"), &[]);
        }
    })
    .finish();
}

/// A dense slab looks the same from above and below (fog has no faces), unless the layer asks
/// for the voxels' face shades: then from above it takes the top shade, from below the bottom
/// shade.
#[test]
fn a_slab_takes_the_shade_of_the_face_seen_only_when_asked() {
    let mut slab = FogLayer::new(40, 44);
    slab.density = 8.0;
    slab.color = [0.8, 0.8, 0.8];
    let shaded = FogLayer {
        face_shade: boxel_fog::VOXEL_FACE_SHADE,
        ..slab.clone()
    };
    for_every_tier(|label, tier, device, queue, _, report| {
        let run = |slab: &FogLayer, want_ratio: f32| -> Result<(), String> {
            let mut renderer = new_renderer(device, queue, tier, UVec2::new(WIDTH, HEIGHT))?;
            renderer.set_fog(
                device,
                Some(fog_input(vec![slab.clone()], FogResolution::Half, 0)),
            );
            let mut colors = Vec::new();
            for (eye, target) in [
                (DVec3::new(0.5, 60.0, 0.5), DVec3::new(10.0, 0.0, 12.0)),
                (DVec3::new(0.5, 20.0, 0.5), DVec3::new(10.0, 60.0, 12.0)),
            ] {
                render(device, queue, &mut renderer, &look(eye, target))?;
                let (size, texels) = renderer
                    .fog_texels(device, queue)
                    .ok_or("no fog target")??;
                let center = texels[((size.y / 2) * size.x + size.x / 2) as usize];
                if center[1] != u32::MAX {
                    return Err(format!("the slab is not opaque: {:08x}", center[1]));
                }
                colors.push(decode_color(center[0])[0]);
            }
            let ratio = colors[1] / colors[0];
            eprintln!(
                "gpu_fog: {label} {tier}: top {}, bottom {} ({ratio})",
                colors[0], colors[1]
            );
            if (colors[0] - 0.8).abs() > 0.01 || (ratio - want_ratio).abs() > 0.01 {
                return Err(format!("shades {colors:?}, want a ratio of {want_ratio}"));
            }
            Ok(())
        };
        for (slab, ratio) in [(&slab, 1.0), (&shaded, 0.5)] {
            if let Err(e) = run(slab, ratio) {
                report.fail(format!("{label} {tier}: {e}"), &[]);
            }
        }
    })
    .finish();
}

/// Resizes, a resolution switch and a rebuilt renderer keep the fog working.
#[test]
fn fog_survives_resizes_resolution_switches_and_new_renderers() {
    let registry = boxel_demo::blocks::registry();
    let chunks = world(&registry, World::Floor);
    let view = look(DVec3::new(-20.5, 22.0, -20.5), DVec3::new(16.0, 2.0, 16.0));
    for_every_tier(|label, tier, device, queue, _, report| {
        let run = || -> Result<(), String> {
            let mut renderer = new_renderer(device, queue, tier, UVec2::new(WIDTH, HEIGHT))?;
            load_world(&mut renderer, &chunks)?;
            for (size, resolution, texels) in [
                (
                    UVec2::new(WIDTH, HEIGHT),
                    FogResolution::Half,
                    UVec2::new(160, 90),
                ),
                (UVec2::new(97, 61), FogResolution::Half, UVec2::new(49, 31)),
                (UVec2::new(97, 61), FogResolution::Full, UVec2::new(97, 61)),
                (
                    UVec2::new(WIDTH, HEIGHT),
                    FogResolution::Full,
                    UVec2::new(WIDTH, HEIGHT),
                ),
            ] {
                renderer.resize(device, size);
                renderer.set_fog(device, Some(fog_input(vec![bank()], resolution, 5)));
                render(device, queue, &mut renderer, &view)?;
                let (got, _) = renderer
                    .fog_texels(device, queue)
                    .ok_or("no fog target")??;
                if got != texels || renderer.stats().fog_passes != 2 {
                    return Err(format!(
                        "{size} {resolution:?}: target {got}, want {texels}"
                    ));
                }
            }
            drop(renderer);
            let mut renderer = new_renderer(device, queue, tier, UVec2::new(WIDTH, HEIGHT))?;
            load_world(&mut renderer, &chunks)?;
            renderer.set_fog(
                device,
                Some(fog_input(vec![bank()], FogResolution::Half, 5)),
            );
            render(device, queue, &mut renderer, &view)?;
            if renderer.stats().fog_passes != 2 {
                return Err("a rebuilt renderer draws no fog".into());
            }
            Ok(())
        };
        if let Err(e) = run() {
            report.fail(format!("{label} {tier}: {e}"), &[]);
        }
    })
    .finish();
}

/// The smoke test's fog check passes on every tier, at both levels.
#[test]
fn the_fog_smoke_check_passes_on_every_tier() {
    for_every_tier(|label, tier, device, queue, caps, report| {
        for level in [SmokeLevel::Compile, SmokeLevel::Draw] {
            let smoke = RendererSmokeTest {
                fog: true,
                ..RendererSmokeTest::default()
            };
            let ctx = SmokeContext {
                device,
                queue,
                tier,
                caps,
                level,
                output_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            };
            if let Err(e) = smoke.run(&ctx) {
                report.fail(format!("{label} {tier} {level:?}: {e}"), &[]);
            }
            if !smoke.fog_ok.load(Ordering::Relaxed) {
                report.fail(
                    format!("{label} {tier} {level:?}: the fog check failed"),
                    &[],
                );
            }
        }
    })
    .finish();
}
