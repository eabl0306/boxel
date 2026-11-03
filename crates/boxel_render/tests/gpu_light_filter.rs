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
//! A second test (`colored_sky_tints_the_picture_on_every_tier`, ADR 0064) lights a greenhouse
//! under red glass and a room behind a light blue window with colored sky, and checks the tint,
//! the night, the tiers (High draws flattened faces exactly, owner decision 1) and the far
//! coordinates; its own doc says how.
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
use boxel_math::{DVec3, I64Vec3, Quat, UVec2, Vec3};
use boxel_mesh::{ChunkMesh, LitCellQuad, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::model::{Cuboid, Model, ModelDraw, ModelLayer, ModelTexture, PartPose};
use boxel_render::{DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, ViewParams, VoxelRenderer};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockState, CHUNK_VOLUME, CellBlend,
    CellImage, CellQuad, ChunkData, Face, FaceTextures, LightTables, LightValue, LocalPos,
    RenderShape, TextureImage, TextureSpec,
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

// ================================================================ colored sky (ADR 0064)

/// The game's red and light blue glass filters (ADR 0064's table).
const DYE_RED: [u8; 3] = [10, 3, 2];
const DYE_LIGHT_BLUE: [u8; 3] = [3, 11, 13];

/// Far-from-origin translations of the colored sky scene and its views (those of
/// `gpu_terrain.rs`): across `+-2^31` in x, z and y, just past it, and `2^40` away.
const FAR_SHIFTS: [I64Vec3; 5] = [
    I64Vec3::new((1 << 31) - 64, 0, -(1 << 31) + 64),
    I64Vec3::new((1 << 31) + (1 << 20), 0, -(1 << 31) - (1 << 20)),
    I64Vec3::new((1 << 40) + (1 << 31) - 64, 0, -(1 << 40) - (1 << 31) + 64),
    I64Vec3::new(-(1 << 35), (1 << 31) - 64, 1 << 35),
    I64Vec3::new(1 << 31, -(1 << 31) - 64, 0),
];

/// The demo blocks plus `test:dye_red` and `test:dye_light_blue` (the game's filters) and
/// `test:clear_glass` (none), all drawn with the demo glass's texture.
fn sky_registry() -> BlockRegistry {
    let demo = boxel_demo::blocks::registry();
    let mut b = boxel_demo::blocks::builder();
    let glass = demo.by_name("boxel:glass").expect("glass");
    let texture = demo.face_texture(glass, Face::PosX);
    for (name, filter) in [
        ("test:dye_red", Some(DYE_RED)),
        ("test:dye_light_blue", Some(DYE_LIGHT_BLUE)),
        ("test:clear_glass", None),
    ] {
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
        if let Some(filter) = filter {
            let block = b.block_by_name(name).expect(name);
            b.set_light_filter(block, filter).expect("filter");
        }
    }
    b.build()
}

/// One variant of the colored sky scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SkyScene {
    /// Clear glass in the greenhouse roof and the window.
    Clear,
    /// Red glass in the roof, light blue in the window.
    Tinted,
    /// [`SkyScene::Tinted`] with a white lamp on the window room's floor, near the window: faces
    /// lit by tinted sky and lamp light that both vary across them.
    Lamp,
}

const SKY_SCENES: [SkyScene; 3] = [SkyScene::Clear, SkyScene::Tinted, SkyScene::Lamp];

/// The scene at `(x, y, z)` (blocks from the min corner of the center chunk, `-32..64`): stone
/// below y = 2; a greenhouse (stone walls at x = 2 and 12, z = 2 and 14, from y = 2 to 9) under
/// a glass roof at y = 10; a closed stone room (walls at x = 16 and 28, z = 2 and 14, from y =
/// 2 to 10, a stone roof at y = 11) lit only by a glass window in its +z wall (x 20 to 23, y 7
/// to 9), the open sky in front of it.
fn sky_block(registry: &BlockRegistry, scene: SkyScene, x: i64, y: i64, z: i64) -> BlockState {
    let s = |name: &str| registry.by_name(name).unwrap_or_else(|| panic!("{name}"));
    let (roof, window) = match scene {
        SkyScene::Clear => ("test:clear_glass", "test:clear_glass"),
        SkyScene::Tinted | SkyScene::Lamp => ("test:dye_red", "test:dye_light_blue"),
    };
    if y < 2 {
        return s("boxel:stone");
    }
    if (2..=12).contains(&x) && (2..=14).contains(&z) {
        return match y {
            10 => s(roof),
            2..=9 if x == 2 || x == 12 || z == 2 || z == 14 => s("boxel:stone"),
            _ => BlockState::AIR,
        };
    }
    if (16..=28).contains(&x) && (2..=14).contains(&z) && (2..=11).contains(&y) {
        let wall = x == 16 || x == 28 || z == 2 || z == 14 || y == 11;
        return match (x, y, z) {
            (20..=23, 7..=9, 14) => s(window),
            _ if wall => s("boxel:stone"),
            (22, 2, 11) if scene == SkyScene::Lamp => s("boxel:lamp_white"),
            _ => BlockState::AIR,
        };
    }
    BlockState::AIR
}

/// Where the rug (a cell quad on the greenhouse floor) lies, and the figure (a model) stands.
const RUG: [i64; 3] = [5, 2, 8];
const FIGURE: [f64; 3] = [9.5, 2.0, 9.5];

/// The center chunk of `scene` moved by `shift` (a multiple of 32), lit with colored sky as the
/// world lights it (sky tops and tints per column, `boxel_light::compute`), meshed with the
/// default options, with a white rug of cell quads on the greenhouse floor lit by its cell (its
/// sky's channels in `cell_sky_rgb`, as the mesher gives them). Also the padded light.
fn sky_mesh(registry: &BlockRegistry, scene: SkyScene, shift: I64Vec3) -> (ChunkMesh, PaddedLight) {
    use boxel_light::box_light::{chunk_index, tile_index};
    use boxel_light::{SkyTile, Tint};
    use boxel_voxel::ChunkPos;
    let tables = LightTables::from_registry(registry);
    let base = ChunkPos::new(shift.x >> 5, shift.y >> 5, shift.z >> 5);
    let chunks: Vec<ChunkData> = (0..27)
        .map(|i| {
            let (dy, rest) = (i as i64 / 9 - 1, i as i64 % 9);
            let (dx, dz) = (rest / 3 - 1, rest % 3 - 1);
            assert_eq!(chunk_index(dx as i32, dy as i32, dz as i32), i);
            let dense: Vec<BlockState> = (0..CHUNK_VOLUME)
                .map(|k| {
                    let p = LocalPos::from_index(k);
                    sky_block(
                        registry,
                        scene,
                        dx * 32 + i64::from(p.x()),
                        dy * 32 + i64::from(p.y()),
                        dz * 32 + i64::from(p.z()),
                    )
                })
                .collect();
            ChunkData::from_dense(&dense)
        })
        .collect();
    // The 9 tiles by brute force: the top of each column (open columns i64::MIN) and the
    // filters of opacity 0 above it.
    let tiles: Vec<SkyTile> = (0..9)
        .map(|ti| {
            let (dx, dz) = (ti as i64 / 3 - 1, ti as i64 % 3 - 1);
            assert_eq!(tile_index(dx as i32, dz as i32), ti);
            let mut tops = vec![i64::MIN; 1024];
            let mut tints = Vec::new();
            for x in 0..32u32 {
                for z in 0..32u32 {
                    for y in (-32..64i64).rev() {
                        let state = sky_block(
                            registry,
                            scene,
                            dx * 32 + i64::from(x),
                            y,
                            dz * 32 + i64::from(z),
                        );
                        if tables.blocks_sky(state) {
                            tops[((x << 5) | z) as usize] = y + shift.y;
                            break;
                        }
                        if tables.filters(state) {
                            let filter = tables.light_filter(state);
                            tints.push((
                                x,
                                z,
                                Tint {
                                    y: y + shift.y,
                                    filter,
                                },
                            ));
                        }
                    }
                }
            }
            SkyTile::from_tops(tops.into_boxed_slice().try_into().expect("1024"), i64::MIN)
                .with_tints(tints)
        })
        .collect();
    let input = LightInput {
        origin: base,
        chunks: std::array::from_fn(|i| Some(&chunks[i])),
        sky: SkyInput::Tiles(std::array::from_fn(|i| &tiles[i])),
        beams: [None; 9],
        tables: &tables,
        colored_sky: true,
    };
    let mut light = PaddedLight::default();
    boxel_light::compute(&input, &mut LightScratch::new(), &mut light);
    let center = &chunks[chunk_index(0, 0, 0)];
    let neighbors = std::array::from_fn(|i| {
        let o = boxel_voxel::NEIGHBOR_OFFSETS[i];
        Some(&chunks[chunk_index(o.x, o.y, o.z)])
    });
    let materials = MaterialTable::from_registry(registry);
    let input = MeshInput {
        center,
        neighbors,
        light: Some(&light),
    };
    let mut mesh = Mesher::new().mesh_chunk(&input, &materials, MeshOptions::default());
    // The rug: a white quad 1/64 above the floor, lit by its cell.
    let [x, y, z] = RUG.map(|c| c as i32);
    let rgb = light.sky_rgb(x, y, z);
    let block = light.get(x, y, z);
    let level = rgb.into_iter().max().unwrap_or(0);
    let (x0, y0, z0) = (x as f32, y as f32 + 1.0 / 64.0, z as f32);
    mesh.cell_quads.push(LitCellQuad {
        quad: CellQuad {
            corners: [
                [x0, y0, z0],
                [x0 + 1.0, y0, z0],
                [x0, y0, z0 + 1.0],
                [x0 + 1.0, y0, z0 + 1.0],
            ],
            uv: [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            image: 0,
            source: CellImage::Cell,
            face: Face::PosY,
            full_bright: false,
            tint: [255; 3],
            blend: CellBlend::Cutout,
        },
        light: LightValue::new(level, block.red(), block.green(), block.blue()),
    });
    if rgb != [level; 3] {
        mesh.cell_sky_rgb = vec![LightValue::from_rgb(rgb).0];
    }
    mesh.hash = mesh.compute_hash();
    (mesh, light)
}

/// The figure: one white box, a block wide and two tall, its feet at its origin.
fn figure() -> (Arc<Model>, ModelTexture) {
    let mut b = Model::builder([64, 64]);
    let body = b.part("body", None, PartPose::default());
    b.cuboid(
        body,
        Cuboid::new([0.0, 0.0], [-8.0, 0.0, -8.0], [16.0, 32.0, 16.0]),
    );
    let model = Arc::new(b.build().expect("valid figure"));
    let skin = ModelTexture::new(64, 64, vec![255; 64 * 64 * 4]).expect("valid skin");
    (model, skin)
}

/// The figure's draw at [`FIGURE`] moved by `shift`, lit by its cell as the render plugin lights
/// a model at a probe: the uncolored light and, where colored glass tinted it, the sky's
/// channels.
fn figure_draw(light: &PaddedLight, shift: I64Vec3) -> ModelDraw {
    let (model, texture) = figure();
    let cell = FIGURE.map(|c| c.floor() as i32);
    let value = light.get(cell[0], cell[1], cell[2]);
    let rgb = light.sky_rgb(cell[0], cell[1], cell[2]);
    ModelDraw {
        id: 1,
        pose: model.rest_pose(),
        model,
        texture,
        layer: ModelLayer::World,
        position: DVec3::from_array(FIGURE) + shift.as_dvec3(),
        rotation: Quat::IDENTITY,
        origin: Vec3::new(0.0, 32.0, 0.0),
        scale: 1.0,
        light: value,
        sky_rgb: (rgb != [value.sky(); 3]).then_some(rgb),
        realm: boxel_render::model::ModelRealm::default(),
    }
}

/// One view of the colored sky scene.
struct SkyView {
    name: &'static str,
    eye: [f64; 3],
    target: [f64; 3],
    /// Clock hour (12 noon, 0 midnight).
    hours: f64,
}

/// The views, rendered in this order: down at the greenhouse floor (the rug and the figure in
/// it) at noon and at midnight, and the window room's floor in front of its window at noon (the
/// window itself out of view, above it). Eye
/// coordinates are multiples of 1/64 (exact in `f64` at every shift).
const SKY_VIEWS: [SkyView; 3] = [
    SkyView {
        name: "greenhouse",
        eye: [7.5, 8.0, 3.5],
        target: [7.5, 2.0, 9.5],
        hours: 12.0,
    },
    SkyView {
        name: "greenhouse_midnight",
        eye: [7.5, 8.0, 3.5],
        target: [7.5, 2.0, 9.5],
        hours: 0.0,
    },
    SkyView {
        name: "window",
        eye: [22.0, 9.5, 8.0],
        target: [22.0, 2.0, 11.0],
        hours: 12.0,
    },
];

/// The view parameters of `view` moved by `shift`: the scene light and sky color of its hour from
/// the default `DaylightCurve`, as the render plugin computes them (`gpu_terrain.rs`).
fn sky_view_params(view: &SkyView, shift: I64Vec3) -> ViewParams {
    use boxel_render::SceneLight;
    use boxel_world::{DaylightCurve, TimeOfDay};
    let eye = DVec3::from_array(view.eye);
    let d = (DVec3::from_array(view.target) - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    let daylight = DaylightCurve::default().at(&TimeOfDay::at_clock(view.hours));
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(eye + shift.as_dvec3(), yaw, pitch);
    let cv = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams {
        light: SceneLight {
            sky: daylight.sky_light,
            ambient: daylight.ambient,
            ..SceneLight::noon()
        },
        ..ViewParams::from_camera(&cv, &camera, daylight.sky_color, DistanceFog::default())
    }
}

/// Where the world point `p` (scene coordinates) lands on the image of `view`, in pixels.
fn project(view: &ViewParams, p: DVec3) -> (u32, u32) {
    let rel = (p - view.render_origin.as_dvec3()).as_vec3();
    let clip = view.view_proj * rel.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    let x = (ndc.x * 0.5 + 0.5) * WIDTH as f32;
    let y = (0.5 - ndc.y * 0.5) * HEIGHT as f32;
    (x as u32, y as u32)
}

/// A white cell texture for the rug.
fn rug_textures() -> boxel_render::BlockTextureData {
    boxel_render::generate_block_textures(&[TextureSpec {
        name: "test:white".to_string(),
        image: TextureImage::solid(16, [255, 255, 255, 255]),
    }])
}

/// Renders every view of each of `meshes` (moved by `shift`, the figure with it) on one tier:
/// `images[mesh][view]`.
fn render_sky(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    registry: &BlockRegistry,
    meshes: &[(&ChunkMesh, Option<ModelDraw>)],
    shift: I64Vec3,
) -> Result<Vec<Vec<RgbaImage>>, String> {
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
    renderer.set_cell_textures(&device, &queue, &Arc::new(rug_textures()));
    let mut all = Vec::new();
    for (mesh, figure) in meshes {
        renderer
            .upsert_chunk(1, shift, Arc::new((*mesh).clone()))
            .map_err(|e| e.to_string())?;
        let mut images = Vec::new();
        for view in &SKY_VIEWS {
            let params = sky_view_params(view, shift);
            let rotation = Transform::from_yaw_pitch(DVec3::ZERO, 0.0, 0.0).rotation;
            renderer.set_models(&device, figure.iter().cloned().collect(), rotation);
            render(&device, &queue, &mut renderer, &params)?; // warm-up
            images.push(render(&device, &queue, &mut renderer, &params)?);
        }
        all.push(images);
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(all)
}

/// `mesh` with only the quads of its flattened tinted entries (the tinted entries whose exact
/// corners differ from what Low and Mid draw) and no cell quads: drawn, it covers every pixel
/// where High may differ from Mid.
fn flattened_only(mesh: &ChunkMesh) -> ChunkMesh {
    let t = mesh.tinted_entries as usize;
    let flattened = |i: usize| {
        i < t
            && boxel_mesh::tinted_corners(mesh.palette[i])
                != boxel_mesh::tinted_exact_corners(mesh.palette[i], mesh.tinted_exact[i])
    };
    let mut out = ChunkMesh {
        palette: mesh.palette.clone(),
        tinted_entries: mesh.tinted_entries,
        tinted_exact: mesh.tinted_exact.clone(),
        light_quantization: mesh.light_quantization,
        ..ChunkMesh::default()
    };
    for b in 0..boxel_mesh::BUCKET_COUNT {
        let kept: Vec<_> = mesh
            .bucket(b)
            .iter()
            .copied()
            .filter(|q| flattened(usize::from(q.light_index())))
            .collect();
        out.bucket_counts[b] = kept.len() as u32;
        out.quads.extend(kept);
    }
    out.hash = out.compute_hash();
    out
}

/// `mesh` with the exact part of every tinted entry replaced by its flat value at the 4 corners:
/// what Low and Mid draw. Drawn on High, it must give Mid's pixels.
fn exact_as_flat(mesh: &ChunkMesh) -> ChunkMesh {
    let mut out = mesh.clone();
    for (exact, entry) in out.tinted_exact.iter_mut().zip(&mesh.palette) {
        let flat = (entry[0] >> 24) << 4 | (entry[1] >> 24) & 15;
        let pair = flat | flat << 12;
        *exact = [pair, pair];
    }
    out.hash = out.compute_hash();
    out
}

/// Per pixel where `base` has every channel at least 40 (dimmer ones round too coarsely in 8
/// bits), the ratio of `image` to `base` in each
/// channel (sRGB values: the same face, texture, shade and AO, so only the light differs).
fn channel_ratios(image: &RgbaImage, base: &RgbaImage) -> Vec<[f64; 3]> {
    let (a, b) = (image.data.as_chunks::<4>().0, base.data.as_chunks::<4>().0);
    a.iter()
        .zip(b)
        .filter(|(_, b)| b[..3].iter().all(|&c| c >= 40))
        .map(|(a, b)| std::array::from_fn(|c| f64::from(a[c]) / f64::from(b[c])))
        .collect()
}

/// Of the pixels [`channel_ratios`] keeps, the fraction where `strong`'s ratio is more than 1.4
/// times each of `weak`'s, and the fraction where it is under 1.1 times one of them (untinted).
fn tinted_fractions(
    image: &RgbaImage,
    base: &RgbaImage,
    strong: &[usize],
    weak: &[usize],
) -> (f64, f64, usize) {
    let ratios = channel_ratios(image, base);
    let n = ratios.len().max(1) as f64;
    let min = |r: &[f64; 3], set: &[usize]| set.iter().map(|&c| r[c]).fold(f64::INFINITY, f64::min);
    let max = |r: &[f64; 3], set: &[usize]| set.iter().map(|&c| r[c]).fold(0.0, f64::max);
    let tinted = ratios
        .iter()
        .filter(|r| min(r, strong) > 1.4 * max(r, weak))
        .count();
    let untinted = ratios
        .iter()
        .filter(|r| min(r, strong) < 1.1 * max(r, weak))
        .count();
    (tinted as f64 / n, untinted as f64 / n, ratios.len())
}

/// The pixel at `at` of `image`.
fn pixel(image: &RgbaImage, (x, y): (u32, u32)) -> [u8; 4] {
    let i = 4 * (y * image.width + x) as usize;
    image.data[i..i + 4].try_into().expect("4 channels")
}

/// The checks of one tier's images (`images[scene][view]`, scenes in [`SKY_SCENES`] order).
fn check_sky_tier(report: &mut Report, what: &str, images: &[Vec<RgbaImage>]) {
    let (clear, tinted) = (&images[0], &images[1]);
    let named = |scene: &str, view: usize, image| {
        (
            format!("{what}_sky_{scene}_{}", SKY_VIEWS[view].name),
            image,
        )
    };
    // At noon, the greenhouse under red glass is red against the same greenhouse under clear
    // glass, none of it untinted: floor, walls, the rug (a cell) and the figure (a model).
    let (red, white, n) = tinted_fractions(&tinted[0], &clear[0], &[0], &[1, 2]);
    eprintln!("gpu_light_filter: {what}: greenhouse {n} pixels, {red:.3} red, {white:.3} untinted");
    if n < 10_000 || red < 0.95 || white > 0.0 {
        let images = [named("tinted", 0, &tinted[0]), named("clear", 0, &clear[0])];
        let refs: Vec<(&str, &RgbaImage)> = images.iter().map(|(n, i)| (n.as_str(), *i)).collect();
        report.fail(
            format!("{what}: greenhouse {n} pixels, {red:.3} red (< 0.95), {white:.3} untinted"),
            &refs,
        );
    }
    let params = sky_view_params(&SKY_VIEWS[0], I64Vec3::ZERO);
    let rug = project(
        &params,
        DVec3::new(RUG[0] as f64 + 0.5, RUG[1] as f64, RUG[2] as f64 + 0.5),
    );
    let body = project(
        &params,
        DVec3::new(FIGURE[0], FIGURE[1] + 1.0, FIGURE[2] - 0.5),
    );
    for (name, at) in [("rug", rug), ("figure", body)] {
        let (t, c) = (pixel(&tinted[0], at), pixel(&clear[0], at));
        let ratio = |k: usize| f64::from(t[k]) / f64::from(c[k].max(1));
        eprintln!("gpu_light_filter: {what}: {name} at {at:?}: {t:?} under red, {c:?} under clear");
        if ratio(0) <= 1.4 * ratio(1).max(ratio(2)) {
            report.fail(
                format!("{what}: the {name} is not red under red glass ({t:?} vs {c:?})"),
                &[],
            );
        }
    }
    // At midnight it follows the night sky: far darker than at noon, and nowhere brighter than
    // under clear glass (the tint is sky light, never block light).
    let (noon, night) = (mean_level(&tinted[0]), mean_level(&tinted[1]));
    let brighter = tinted[1]
        .data
        .iter()
        .zip(&clear[1].data)
        .filter(|&(t, c)| *t > c.saturating_add(1))
        .count();
    eprintln!(
        "gpu_light_filter: {what}: greenhouse mean level noon {noon:.1}, midnight {night:.1}"
    );
    if night > 0.5 * noon || brighter > 0 {
        let images = [named("tinted", 1, &tinted[1]), named("clear", 1, &clear[1])];
        let refs: Vec<(&str, &RgbaImage)> = images.iter().map(|(n, i)| (n.as_str(), *i)).collect();
        report.fail(
            format!("{what}: greenhouse at midnight {night:.1} vs noon {noon:.1}, {brighter} channels brighter than clear glass"),
            &refs,
        );
    }
    // A light blue window tints the closed room it lights.
    let (blue, white, n) = tinted_fractions(&tinted[2], &clear[2], &[1, 2], &[0]);
    eprintln!(
        "gpu_light_filter: {what}: window room {n} pixels, {blue:.3} blue, {white:.3} untinted"
    );
    if n < 2_000 || blue < 0.9 || white > 0.0 {
        let images = [named("tinted", 2, &tinted[2]), named("clear", 2, &clear[2])];
        let refs: Vec<(&str, &RgbaImage)> = images.iter().map(|(n, i)| (n.as_str(), *i)).collect();
        report.fail(
            format!(
                "{what}: window room {n} pixels, {blue:.3} light blue (< 0.9), {white:.3} untinted"
            ),
            &refs,
        );
    }
}

fn mean_level(image: &RgbaImage) -> f64 {
    let pixels = image.data.as_chunks::<4>().0;
    let sum: u64 = pixels
        .iter()
        .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
        .sum();
    sum as f64 / (3 * pixels.len().max(1)) as f64
}

/// `a` and `b` the same: exactly, or on a software adapter within the tolerance of the other
/// tier checks (llvmpipe rounds some interpolants differently between triangle lists and
/// strips, `gpu_terrain.rs`).
fn check_alike(
    report: &mut Report,
    what: &str,
    software: bool,
    a: (&str, &RgbaImage),
    b: (&str, &RgbaImage),
) {
    let diff = a.1.compare(b.1, if software { 2 } else { 0 });
    let ok = if software {
        diff.fraction() <= MAX_DIFF
    } else {
        diff.differing_pixels == 0
    };
    if !ok {
        report.fail(
            format!(
                "{what}: {} pixels differ (max channel {})",
                diff.differing_pixels, diff.max_channel_diff
            ),
            &[a, b],
        );
    }
}

/// ADR 0064, lane C4: the colored sky on every tier. A greenhouse under red glass and a closed
/// room lit by a light blue window, against the same scene with clear glass, lit as the world
/// lights them with colored sky (`colored_sky: true`) and meshed by the real `Mesher`, plus a
/// white rug of cell quads and a white figure (a model) in the greenhouse. For every adapter
/// (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local capability check
/// allows, asserts:
///
/// - at noon the greenhouse (floor, walls, rug and figure) is red against clear glass and none
///   of it untinted; at midnight it is far darker and nowhere brighter than under clear glass
///   (it follows the night sky); the window room is light blue;
/// - every tier draws these scenes alike (exactly; software adapters within the tolerance of
///   the other checks): no face is flattened in them;
/// - with a lamp near the window (faces lit by tinted sky and the lamp, both varying), Low and
///   Mid draw alike (both draw the flattened entries); High draws the exact faces (owner
///   decision 1): it differs from Mid somewhere and only where the flattened quads are drawn,
///   and with each exact part replaced by its flat value it draws Mid's pixels;
/// - every tier draws the same pixels at every [`FAR_SHIFTS`] position as near the origin, where
///   the meshes come out byte for byte the same.
///
/// Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_light_filter/` (every image with
/// `GPU_LIGHT_FILTER_SAVE=1`).
#[test]
fn colored_sky_tints_the_picture_on_every_tier() {
    let registry = sky_registry();
    let near: Vec<(ChunkMesh, PaddedLight)> = SKY_SCENES
        .iter()
        .map(|&scene| sky_mesh(&registry, scene, I64Vec3::ZERO))
        .collect();
    let [(clear, _), (tinted, tinted_light), (lamp, lamp_light)] = &near[..] else {
        unreachable!("three scenes")
    };
    assert_eq!(clear.tinted_entries, 0, "clear glass: no tinted entry");
    assert!(clear.cell_sky_rgb.is_empty());
    assert!(tinted.tinted_entries > 0 && !tinted.cell_sky_rgb.is_empty());
    assert_eq!(
        tinted_light.sky_rgb(7, 2, 8),
        [13, 7, 6],
        "under the red roof"
    );
    assert_eq!(
        tinted_light.get(7, 2, 8).sky(),
        15,
        "gameplay reads the uncolored sky"
    );
    assert_eq!(
        lamp_light.sky_rgb(22, 8, 14),
        [6, 13, 13],
        "in the light blue window"
    );
    let flattened = flattened_only(lamp);
    assert!(
        !flattened.quads.is_empty(),
        "the lamp scene has flattened faces"
    );
    assert!(
        flattened_only(tinted).quads.is_empty(),
        "without the lamp no face is flattened"
    );
    let figure = |light: &PaddedLight| Some(figure_draw(light, I64Vec3::ZERO));
    let exact_flat = exact_as_flat(lamp);
    let empty = ChunkMesh::from_quads(vec![], [0; boxel_mesh::BUCKET_COUNT]);
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let mut report = Report::new();
    let mut rendered = 0;
    let save_all = std::env::var_os("GPU_LIGHT_FILTER_SAVE").is_some();
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let software = caps.info.device_type == wgpu::DeviceType::Cpu;
        let mut results: Vec<(Tier, Vec<Vec<RgbaImage>>)> = Vec::new();
        for tier in allowed_tiers(&caps) {
            let scenes: Vec<(&ChunkMesh, Option<ModelDraw>)> = near
                .iter()
                .map(|(mesh, light)| (mesh, figure(light)))
                .collect();
            match render_sky(adapter, &caps, tier, &registry, &scenes, I64Vec3::ZERO) {
                Ok(images) => {
                    rendered += 1;
                    let what = format!("{label}_{tier}");
                    if save_all {
                        for (scene, views) in SKY_SCENES.iter().zip(&images) {
                            for (view, image) in SKY_VIEWS.iter().zip(views) {
                                report.save(&[(
                                    &format!("{what}_sky_{scene:?}_{}", view.name),
                                    image,
                                )]);
                            }
                        }
                    }
                    check_sky_tier(&mut report, &what, &images);
                    results.push((tier, images));
                }
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        // Without flattened faces every tier draws alike; with them Low and Mid do.
        let find = |t: Tier| results.iter().find(|(tier, _)| *tier == t).map(|(_, i)| i);
        let name = |tier: Tier, s: usize, v: usize| {
            format!(
                "{label}_{tier}_sky_{:?}_{}",
                SKY_SCENES[s], SKY_VIEWS[v].name
            )
        };
        if let Some(((best, b), rest)) = results.split_first() {
            for (tier, t) in rest {
                for s in 0..SKY_SCENES.len() {
                    if SKY_SCENES[s] == SkyScene::Lamp && *best == Tier::High {
                        continue;
                    }
                    for v in 0..SKY_VIEWS.len() {
                        check_alike(
                            &mut report,
                            &format!(
                                "{label}: sky {:?} {} {tier} vs {best}",
                                SKY_SCENES[s], SKY_VIEWS[v].name
                            ),
                            software,
                            (&name(*best, s, v), &b[s][v]),
                            (&name(*tier, s, v), &t[s][v]),
                        );
                    }
                }
            }
        }
        // High's own reference for the flattened faces.
        if let (Some(high), Some(mid)) = (find(Tier::High), find(Tier::Mid)) {
            let lamp_scene = SKY_SCENES
                .iter()
                .position(|&s| s == SkyScene::Lamp)
                .expect("lamp");
            let extra = [
                (&exact_flat, figure(lamp_light)),
                (&flattened, None),
                (&empty, None),
            ];
            let high_flat = render_sky(
                adapter,
                &caps,
                Tier::High,
                &registry,
                &extra[..1],
                I64Vec3::ZERO,
            );
            let mid_mask = render_sky(
                adapter,
                &caps,
                Tier::Mid,
                &registry,
                &extra[1..],
                I64Vec3::ZERO,
            );
            match (high_flat, mid_mask) {
                (Ok(high_flat), Ok(mask)) => {
                    let mut differ = 0;
                    for v in 0..SKY_VIEWS.len() {
                        let view = SKY_VIEWS[v].name;
                        check_alike(
                            &mut report,
                            &format!("{label}: High with flat exact parts vs Mid, lamp {view}"),
                            software,
                            (
                                &format!("{label}_High_sky_lamp_flat_{view}"),
                                &high_flat[0][v],
                            ),
                            (&name(Tier::Mid, lamp_scene, v), &mid[lamp_scene][v]),
                        );
                        let (h, m) = (&high[lamp_scene][v], &mid[lamp_scene][v]);
                        let (drawn, none) = (&mask[0][v], &mask[1][v]);
                        let tolerance = if software { 2 } else { 0 };
                        fn px(i: &RgbaImage, k: usize) -> &[u8] {
                            &i.data[4 * k..4 * k + 3]
                        }
                        let mut outside = 0;
                        for k in 0..(WIDTH * HEIGHT) as usize {
                            let d = px(h, k)
                                .iter()
                                .zip(px(m, k))
                                .any(|(a, b)| a.abs_diff(*b) > tolerance);
                            if d {
                                differ += 1;
                                if px(drawn, k) == px(none, k) {
                                    outside += 1;
                                }
                            }
                        }
                        eprintln!(
                            "gpu_light_filter: {label}: lamp {view}: High differs from Mid in {differ} pixels so far, {outside} outside the flattened quads"
                        );
                        let limit = if software {
                            (MAX_DIFF * f64::from(WIDTH * HEIGHT)) as usize
                        } else {
                            0
                        };
                        if outside > limit {
                            report.fail(
                                format!("{label}: lamp {view}: High differs from Mid in {outside} pixels outside the flattened quads"),
                                &[
                                    (&name(Tier::High, lamp_scene, v), h),
                                    (&name(Tier::Mid, lamp_scene, v), m),
                                    (&format!("{label}_Mid_sky_flattened_{view}"), drawn),
                                ],
                            );
                        }
                    }
                    if differ == 0 {
                        report.fail(
                            format!("{label}: High draws the flattened faces as Mid does"),
                            &[],
                        );
                    }
                }
                (Err(e), _) | (_, Err(e)) => {
                    report.fail(format!("{label} High reference: {e}"), &[]);
                }
            }
        }
        // The same pixels far from the origin, on every tier.
        for shift in FAR_SHIFTS {
            let far: Vec<(ChunkMesh, PaddedLight)> = SKY_SCENES
                .iter()
                .map(|&scene| sky_mesh(&registry, scene, shift))
                .collect();
            for ((a, _), (b, _)) in near.iter().zip(&far) {
                assert_eq!(a, b, "the meshes at shift {shift}");
            }
            let scenes: Vec<(&ChunkMesh, Option<ModelDraw>)> = far
                .iter()
                .map(|(mesh, light)| (mesh, Some(figure_draw(light, shift))))
                .collect();
            for (tier, images) in &results {
                match render_sky(adapter, &caps, *tier, &registry, &scenes, shift) {
                    Ok(moved) => {
                        for s in 0..SKY_SCENES.len() {
                            for v in 0..SKY_VIEWS.len() {
                                check_alike(
                                    &mut report,
                                    &format!(
                                        "{label}: sky {:?} {} {tier} shift {shift} vs origin",
                                        SKY_SCENES[s], SKY_VIEWS[v].name
                                    ),
                                    false,
                                    (&name(*tier, s, v), &images[s][v]),
                                    (&format!("{}_shift{shift}", name(*tier, s, v)), &moved[s][v]),
                                );
                            }
                        }
                    }
                    Err(e) => report.fail(format!("{label} {tier} shift {shift}: {e}"), &[]),
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
