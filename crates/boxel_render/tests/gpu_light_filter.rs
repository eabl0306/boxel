//! Light filters (ADR 0060) on every tier. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! A closed stone room underground (no sky) with a white lamp on its floor and, between the
//! lamp and the camera, a wall of glass across the room: red glass with a light filter, the same
//! glass without one, or no wall. The light is computed as the world computes it
//! (`boxel_light::compute`) and the room meshed by the real `Mesher`. The camera looks down at
//! the floor on its side of the wall. For every adapter (filtered by `WGPU_BACKEND` /
//! `WGPU_ADAPTER_NAME`) and every tier a local capability check allows (the one of
//! `gpu_tiers.rs`), asserts:
//!
//! - behind red glass the floor is red (most pixels red, none green or blue), behind clear glass
//!   it is not;
//! - the room without the wall gives the same meshes and exactly the same pixels with the filter
//!   blocks registered as with the demo registry alone (filters change nothing where there are
//!   none);
//! - every tier draws the same pixels as the best tier of its adapter (0.1 % of the pixels,
//!   channel tolerance 2).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_light_filter/` (every image with
//! `GPU_LIGHT_FILTER_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_light::{LightInput, LightScratch, PaddedLight, SkyInput};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::{DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, ViewParams, VoxelRenderer};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, Face,
    FaceTextures, LightTables, LocalPos, RenderShape,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_DIFF: f64 = 0.001;
/// The filter of the red glass: all the red, little green and blue.
const RED: [u8; 3] = [15, 2, 2];

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
        label: Some("gpu_light_filter"),
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

/// What stands across the room at x = 12.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wall {
    /// Glass with the [`RED`] filter.
    Red,
    /// The same glass without a filter.
    Clear,
    /// Nothing.
    None,
}

/// The demo blocks plus `test:red_glass` (filtered) and `test:clear_glass`, both drawn with
/// the demo glass's texture; or (`filters` false) the demo blocks alone.
fn registry(filters: bool) -> BlockRegistry {
    let demo = boxel_demo::blocks::registry();
    let mut b = boxel_demo::blocks::builder();
    if filters {
        let glass = demo.by_name("boxel:glass").expect("glass");
        let texture = demo.face_texture(glass, Face::PosX);
        for name in ["test:red_glass", "test:clear_glass"] {
            b.add_block(BlockDef {
                name: name.to_string(),
                shape: RenderShape::Translucent,
                model: BlockModel::CUBE,
                collision: BlockCollision::Full,
                emission: [0; 3],
                light_opacity: 0,
                textures: FaceTextures::all(texture),
            })
            .expect(name);
        }
        let red = b.block_by_name("test:red_glass").expect("red glass");
        b.set_light_filter(red, RED).expect("filter");
    }
    b.build()
}

/// The room's chunk (the origin chunk, alone): stone with a hollow x, z in 4..28, y in 4..12, a
/// white lamp on the floor at (8, 4, 16) and `wall` across the room at x = 12.
fn room(registry: &BlockRegistry, wall: Wall) -> ChunkData {
    let s = |name: &str| registry.by_name(name).unwrap_or_else(|| panic!("{name}"));
    let stone = s("boxel:stone");
    let lamp = s("boxel:lamp_white");
    let glass = match wall {
        Wall::Red => Some(s("test:red_glass")),
        Wall::Clear => Some(s("test:clear_glass")),
        Wall::None => None,
    };
    let dense: Vec<BlockState> = (0..CHUNK_VOLUME)
        .map(|i| {
            let p = LocalPos::from_index(i);
            let (x, y, z) = (p.x(), p.y(), p.z());
            let inside = (4..28).contains(&x) && (4..12).contains(&y) && (4..28).contains(&z);
            match (x, y, z) {
                _ if !inside => stone,
                (8, 4, 16) => lamp,
                (12, _, _) => glass.unwrap_or(BlockState::AIR),
                _ => BlockState::AIR,
            }
        })
        .collect();
    ChunkData::from_dense(&dense)
}

/// The room's mesh, lit as the world lights it: no sky (it is underground), the lamp's light
/// through the wall.
fn mesh(registry: &BlockRegistry, wall: Wall) -> ChunkMesh {
    let chunk = room(registry, wall);
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
    Mesher::new().mesh_chunk(&input, &materials, MeshOptions::default())
}

/// Looking down at the floor on the camera's side of the wall (x 13 to 17.5).
fn view() -> ViewParams {
    let eye = DVec3::new(16.5, 7.0, 16.5);
    // Toward -x, down 76 degrees.
    let transform = Transform::from_yaw_pitch(eye, std::f32::consts::FRAC_PI_2, -1.33);
    let camera = Camera::default();
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_light_filter"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// The images of one tier: behind red glass, behind clear glass, without a wall, and without a
/// wall from the demo registry alone.
struct TierImages {
    red: RgbaImage,
    clear: RgbaImage,
    none: RgbaImage,
    none_plain: RgbaImage,
}

/// The four meshes and the registries they come from.
struct Scenes {
    filters: BlockRegistry,
    plain: BlockRegistry,
    red: ChunkMesh,
    clear: ChunkMesh,
    none: ChunkMesh,
    none_plain: ChunkMesh,
}

fn render_registry(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    registry: &BlockRegistry,
    meshes: &[&ChunkMesh],
) -> Result<Vec<RgbaImage>, String> {
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
        &boxel_render::generate_block_textures(registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    let view = view();
    let mut images = Vec::new();
    for mesh in meshes {
        renderer
            .upsert_chunk(1, I64Vec3::ZERO, Arc::new((*mesh).clone()))
            .map_err(|e| e.to_string())?;
        render(&device, &queue, &mut renderer, &view)?; // warm-up
        images.push(render(&device, &queue, &mut renderer, &view)?);
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(images)
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    s: &Scenes,
) -> Result<TierImages, String> {
    let mut with = render_registry(
        adapter,
        caps,
        tier,
        &s.filters,
        &[&s.red, &s.clear, &s.none],
    )?
    .into_iter();
    let mut plain = render_registry(adapter, caps, tier, &s.plain, &[&s.none_plain])?.into_iter();
    let next = |images: &mut std::vec::IntoIter<RgbaImage>| {
        images.next().ok_or_else(|| "missing image".to_string())
    };
    Ok(TierImages {
        red: next(&mut with)?,
        clear: next(&mut with)?,
        none: next(&mut with)?,
        none_plain: next(&mut plain)?,
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
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_light_filter"),
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
                Ok(()) => eprintln!("gpu_light_filter: saved {}", path.display()),
                Err(e) => eprintln!("gpu_light_filter: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_light_filter: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }
}

/// Fraction of pixels where channel `c` is at least 40 and more than twice the other two.
fn dominant_fraction(image: &RgbaImage, c: usize) -> f64 {
    let pixels = image.data.as_chunks::<4>().0;
    let n = pixels
        .iter()
        .filter(|p| {
            let others = (0..3).filter(|&o| o != c).map(|o| u16::from(p[o])).max();
            u16::from(p[c]) >= 40 && others.is_some_and(|o| u16::from(p[c]) > 2 * o)
        })
        .count();
    n as f64 / pixels.len().max(1) as f64
}

fn check_tier(report: &mut Report, what: &str, i: &TierImages) {
    let images = [
        (format!("{what}_red"), &i.red),
        (format!("{what}_clear"), &i.clear),
        (format!("{what}_none"), &i.none),
        (format!("{what}_none_plain"), &i.none_plain),
    ];
    let named: Vec<(&str, &RgbaImage)> = images.iter().map(|(n, i)| (n.as_str(), *i)).collect();
    let red = dominant_fraction(&i.red, 0);
    let others = dominant_fraction(&i.red, 1).max(dominant_fraction(&i.red, 2));
    let clear = dominant_fraction(&i.clear, 0);
    eprintln!("gpu_light_filter: {what}: red pixels {red:.3} (clear glass {clear:.3})");
    if red < 0.5 || others > 0.0 {
        report.fail(
            format!(
                "{what}: behind red glass {red:.3} red pixels (< 0.5), {others:.3} green or blue"
            ),
            &named,
        );
    }
    if clear > 0.01 {
        report.fail(
            format!("{what}: behind clear glass {clear:.3} red pixels (> 0.01)"),
            &named,
        );
    }
    let diff = i.none.compare(&i.none_plain, 0);
    if diff.differing_pixels > 0 {
        report.fail(
            format!(
                "{what}: no wall, with and without filter blocks: {} pixels differ (max {})",
                diff.differing_pixels, diff.max_channel_diff
            ),
            &named,
        );
    }
}

#[test]
fn a_lamp_behind_red_glass_lights_the_floor_red_on_every_tier() {
    let filters = registry(true);
    let plain = registry(false);
    let scenes = Scenes {
        red: mesh(&filters, Wall::Red),
        clear: mesh(&filters, Wall::Clear),
        none: mesh(&filters, Wall::None),
        none_plain: mesh(&plain, Wall::None),
        filters,
        plain,
    };
    // Where there is no filter, nothing changes: the same mesh, light palette included.
    assert_eq!(scenes.none, scenes.none_plain);
    // The wall changes the light only through its filter.
    assert_ne!(scenes.red.palette, scenes.clear.palette);
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
            if std::env::var_os("GPU_LIGHT_FILTER_SAVE").is_some() {
                report.save(&[
                    (&format!("{what}_red"), &i.red),
                    (&format!("{what}_clear"), &i.clear),
                    (&format!("{what}_none"), &i.none),
                ]);
            }
            check_tier(&mut report, &what, i);
        }
        let Some(((best, b), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, t) in rest {
            for (name, x, y) in [
                ("red", &b.red, &t.red),
                ("clear", &b.clear, &t.clear),
                ("none", &b.none, &t.none),
            ] {
                let diff = x.compare(y, 2);
                eprintln!(
                    "gpu_light_filter: {label}: {name} {tier} vs {best}: {:.4} % of pixels \
                     differ, max channel {}",
                    diff.fraction() * 100.0,
                    diff.max_channel_diff
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
