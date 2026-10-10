//! Cave culling (ADR 0022) changes no pixel. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! The scene is real overworld terrain with caves (`OverworldGenerator::default()`), 7 x 7 x 7
//! meshed chunks around the origin from 32 voxels under the surface's caves to the sky, with
//! the deep caves flooded with water and glass walls across some caves (both open to the
//! culling: they must let it see through). Each chunk's face links come from its data, as the
//! world computes them.
//!
//! For every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier it can
//! run, every view is rendered without cave culling and with the visible set of a search from
//! its camera ([`visible_chunks`]), or none when the eye does not see from open space
//! ([`eye_in_open`], as the render plugin): the two images must be identical, pixel for pixel.
//! The views cover the surface, the inside of caves, eyes a hair from chunk borders, edges and
//! corners, looks through water and through glass, and eyes that see through rock (inside a
//! stone voxel, and nearer a stone wall than the near plane: those must not be culled at all).
//! The scene, the views and the links are then moved far from the origin (the former `i32`
//! edge, `2^40` voxels) and every image with culling must equal the one near the origin. Most
//! culled views must hide chunks, or the test would prove nothing.
//!
//! A second scene comes from the world itself (`WorldPlugin`, jobs run when the test says): the
//! meshes it publishes go to the renderer as the render plugin sends them, and the set is the
//! plugin's own (`boxel_render::frame_visibility`), while chunks are not drawn as their blocks are
//! (no mesh yet, the meshes of an edit published apart, a renderer resync while a chunk is
//! remeshed).
//!
//! A third case loses the uploads of the frame that publishes two chunks ahead of the eye (its
//! staging buffer cannot be created): that frame must cull nothing.
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_cave_culling/`.
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_demo::worldgen::OverworldGenerator;
use boxel_ecs::prelude::Messages;
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{Aabb, DVec3, I64Vec3, UVec2, Vec3};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::{DEFAULT_SKY_COLOR, RendererConfig, TranslucentSort, ViewParams, VoxelRenderer};
use boxel_transform::Transform;
use boxel_voxel::{
    ALL_LINKS, BlockPos, BlockRegistry, BlockState, CHUNK_SHIFT, ChunkData, ChunkFaceInfo,
    ChunkKey, ChunkPos, LocalPos, NEIGHBOR_OFFSETS,
};
use boxel_world::visibility::{XRAY, eye_in_open, visible_chunks};
use boxel_world::{
    ChunkMap, ChunkMeshEvent, ChunkMeshResync, ChunkSlot, ChunkViewer, ChunkVisibility, EditQueue,
    VisibilityView, VisibleSet, WorldPlugin, WorldSettings, WorldStats,
};
use boxel_worldgen::WorldGenerator;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Meshed chunks: `-R..=R` in x and z, `Y0..=Y1` in y (data one chunk further everywhere).
const R: i64 = 3;
const Y0: i64 = -4;
const Y1: i64 = 2;

/// Far-from-origin translations (voxels, multiples of 32): straddling the former `i32` edge on
/// x and z, `2^40`, and straddling it on y. (The world limit itself is past the exact range of
/// the `f64` camera; the search is tested there on the CPU, in `boxel_world`.)
const FAR_SHIFTS: [I64Vec3; 3] = [
    I64Vec3::new((1 << 31) - 64, 0, -(1 << 31) + 96),
    I64Vec3::new((1 << 40) + (1 << 31) - 64, -(1 << 40), -(1 << 40) - 32),
    I64Vec3::new(-(1 << 35), (1 << 31) - 64, 1 << 35),
];

// ---------------------------------------------------------------- adapters (as gpu_terrain)

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

fn device_for(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
) -> Result<(wgpu::Device, wgpu::Queue, Arc<Mutex<Vec<String>>>), String> {
    let request = tier_request(tier, &caps.limits);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu_cave_culling"),
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

struct Scene {
    registry: BlockRegistry,
    /// Non-empty meshes by chunk.
    meshes: Vec<(ChunkPos, Arc<ChunkMesh>)>,
    /// Links of the meshed chunks (every other chunk is open, as in the world).
    links: HashMap<ChunkPos, u16>,
    /// Blocks of every generated chunk.
    data: HashMap<ChunkPos, ChunkData>,
}

impl Scene {
    fn block(&self, p: BlockPos) -> Option<BlockState> {
        let (chunk, local) = p.split();
        self.data.get(&chunk).map(|d| d.get(local))
    }
    fn open(&self, p: BlockPos) -> bool {
        self.block(p) == Some(BlockState::AIR)
    }
    fn occludes(&self, p: BlockPos) -> Option<bool> {
        self.block(p).map(|s| self.registry.occludes(s))
    }
}

/// Generates, floods the deep caves, puts glass walls in some caves and meshes.
fn cave_scene() -> Scene {
    let registry = boxel_demo::blocks::registry();
    let generator = OverworldGenerator::default();
    let water = registry.by_name("boxel:water").expect("water");
    let glass = registry.by_name("boxel:glass").expect("glass");
    let mut data = HashMap::new();
    for y in Y0 - 1..=Y1 + 1 {
        for z in -R - 1..=R + 1 {
            for x in -R - 1..=R + 1 {
                let pos = ChunkPos::new(x, y, z);
                let mut chunk = generator.generate(ChunkKey::new(pos), &registry);
                // Below y = -96 the caves are flooded; in layer -2, every 11th x plane of the
                // caves is glass.
                if chunk.as_uniform().is_none() {
                    for i in 0..boxel_voxel::CHUNK_VOLUME {
                        let l = LocalPos::from_index(i);
                        if chunk.get(l) != BlockState::AIR {
                            continue;
                        }
                        let world_y = (y << CHUNK_SHIFT) + i64::from(l.y());
                        let world_x = (x << CHUNK_SHIFT) + i64::from(l.x());
                        if world_y < -96 {
                            chunk.set(l, water);
                        } else if y == -2 && world_x.rem_euclid(11) == 0 {
                            chunk.set(l, glass);
                        }
                    }
                }
                data.insert(pos, chunk);
            }
        }
    }
    let materials = MaterialTable::from_registry(&registry);
    let mut mesher = Mesher::new();
    let mut meshes = Vec::new();
    let mut links = HashMap::new();
    for (&pos, center) in &data {
        if pos.x.abs() > R || pos.z.abs() > R || !(Y0..=Y1).contains(&pos.y) {
            continue;
        }
        links.insert(pos, ChunkFaceInfo::compute(center, &registry).links);
        let input = MeshInput {
            center,
            neighbors: std::array::from_fn(|i| data.get(&pos.offset(NEIGHBOR_OFFSETS[i]))),
            light: None,
        };
        let mesh = mesher.mesh_chunk(&input, &materials, MeshOptions::default());
        if !mesh.is_empty() {
            meshes.push((pos, Arc::new(mesh)));
        }
    }
    meshes.sort_by_key(|(p, _)| (p.y, p.z, p.x));
    Scene {
        registry,
        meshes,
        links,
        data,
    }
}

fn chunk_id(c: ChunkPos) -> u64 {
    let pack = |v: i64| (v + 512) as u64 & 0x3ff;
    pack(c.x) | pack(c.y) << 10 | pack(c.z) << 20
}

/// A named view: eye (multiples of 1/64) and yaw/pitch in degrees.
struct View {
    name: String,
    eye: DVec3,
    yaw: f32,
    pitch: f32,
}

/// Snaps to 1/64 (exact at every shift).
fn snap(v: f64) -> f64 {
    (v * 64.0).round() / 64.0
}

/// The views: the surface, cave interiors, chunk borders, edges and corners, through water and
/// through glass. Cave eyes are open voxels found in the scene.
fn views(scene: &Scene) -> Vec<View> {
    let generator = OverworldGenerator::default();
    let h = generator.height_at(0, 0) as f64;
    let mut out = vec![
        View {
            name: "surface_down".into(),
            eye: DVec3::new(-40.25, h + 30.5, -20.75),
            yaw: -60.0,
            pitch: -35.0,
        },
        View {
            name: "surface_level".into(),
            eye: DVec3::new(2.5, h + 2.25, 1.75),
            yaw: 30.0,
            pitch: -8.0,
        },
    ];
    // Open voxels deep in the rock (more than 20 voxels under the surface), spread out.
    let mut caves = Vec::new();
    'search: for y in (-90i64..-10).step_by(7) {
        for z in (-90i64..90).step_by(13) {
            for x in (-90i64..90).step_by(11) {
                let p = BlockPos::new(x, y, z);
                if scene.open(p) && (y as f64) < generator.height_at(x, z) as f64 - 20.0 {
                    caves.push(p);
                    if caves.len() == 6 {
                        break 'search;
                    }
                }
            }
        }
    }
    assert!(caves.len() >= 3, "too few cave eyes: {caves:?}");
    for (i, p) in caves.iter().enumerate() {
        for (j, (yaw, pitch)) in [(0.0, -10.0), (125.0, 15.0), (250.0, -45.0)]
            .iter()
            .enumerate()
        {
            out.push(View {
                name: format!("cave{i}_{j}"),
                eye: p.as_i64vec3().as_dvec3() + DVec3::new(0.5, 0.421_875, 0.5),
                yaw: *yaw,
                pitch: *pitch,
            });
        }
    }
    // Eyes a hair from a chunk border, an edge and a corner, in open voxels next to them.
    for (name, (dx, dy, dz)) in [
        ("border_x", (1, 0, 0)),
        ("edge_xz", (1, 0, 1)),
        ("corner", (1, 1, 1)),
    ] {
        // The open voxel at the (-, -, -) side of a chunk corner; the eye sits a hair from the
        // borders of the axes in use, in the middle of the voxel on the others.
        let found = (-3..=3)
            .flat_map(|cy| (-2..=2).map(move |c| (c, cy)))
            .map(|(c, cy)| {
                BlockPos::from_i64vec3(I64Vec3::new(c * 32 - 1, cy * 32 - 1, c * 32 - 1))
            })
            .find(|&p| scene.open(p));
        if let Some(p) = found {
            let near = |d: i32| if d == 1 { 1.0 - 1.0 / 64.0 } else { 0.5 };
            let eye = p.as_i64vec3().as_dvec3() + DVec3::new(near(dx), near(dy), near(dz));
            for (j, yaw) in [45.0, 225.0].iter().enumerate() {
                out.push(View {
                    name: format!("{name}{j}"),
                    eye,
                    yaw: *yaw,
                    pitch: -20.0,
                });
            }
        }
    }
    // Looking down into the flooded caves from the lowest open voxel above the water, and
    // across the glass walls of layer -2.
    let wet = (-96i64..-60)
        .flat_map(|y| {
            (-90i64..90)
                .step_by(3)
                .map(move |x| BlockPos::new(x, y, (x * 7) % 90))
        })
        .find(|&p| scene.open(p));
    if let Some(p) = wet {
        out.push(View {
            name: "into_water".into(),
            eye: p.as_i64vec3().as_dvec3() + DVec3::new(0.5, 0.5, 0.5),
            yaw: 10.0,
            pitch: -60.0,
        });
    }
    let behind_glass = (-60i64..-34)
        .flat_map(|y| (-90i64..90).step_by(5).map(move |z| BlockPos::new(1, y, z)))
        .find(|&p| scene.open(p));
    if let Some(p) = behind_glass {
        for (j, yaw) in [90.0, -90.0].iter().enumerate() {
            out.push(View {
                name: format!("through_glass{j}"),
                eye: p.as_i64vec3().as_dvec3() + DVec3::new(0.5, 0.5, 0.5),
                yaw: *yaw,
                pitch: 0.0,
            });
        }
    }
    out.extend(rock_views(scene));
    for v in &mut out {
        v.eye = DVec3::new(snap(v.eye.x), snap(v.eye.y), snap(v.eye.z));
    }
    out
}

/// Views where the renderer sees through rock: the eye inside a stone voxel (a camera that
/// flies through the ground), and in an open voxel a hair from a stone wall it looks at, closer
/// than the near plane (0.1). Faces between two stone voxels are not meshed and the near walls
/// of caves face away, so the far walls of caves behind the rock show. Cave culling must not
/// hide them.
fn rock_views(scene: &Scene) -> Vec<View> {
    let generator = OverworldGenerator::default();
    let deep = |p: BlockPos| (p.y as f64) < generator.height_at(p.x, p.z) as f64 - 20.0;
    let mut out = Vec::new();
    let in_rock = (-80i64..-20)
        .step_by(9)
        .flat_map(|y| {
            (-80i64..80)
                .step_by(23)
                .map(move |x| BlockPos::new(x, y, -x / 3))
        })
        .filter(|&p| deep(p) && scene.block(p).is_some_and(|s| scene.registry.occludes(s)))
        .take(3);
    for (i, p) in in_rock.enumerate() {
        for (j, (yaw, pitch)) in [(20.0, -15.0), (160.0, 10.0), (275.0, -40.0)]
            .iter()
            .enumerate()
        {
            out.push(View {
                name: format!("in_rock{i}_{j}"),
                eye: p.as_i64vec3().as_dvec3() + DVec3::new(0.5, 0.421_875, 0.5),
                yaw: *yaw,
                pitch: *pitch,
            });
        }
    }
    // Open voxels with stone on their -x side: the eye 1/64 from that face, looking at it.
    let at_wall = (-80i64..-20)
        .step_by(7)
        .flat_map(|y| (-80i64..80).step_by(17).map(move |z| (y, z)))
        .flat_map(|(y, z)| (-80i64..80).map(move |x| BlockPos::new(x, y, z)))
        .filter(|&p| {
            deep(p)
                && scene.open(p)
                && scene
                    .block(BlockPos::new(p.x - 1, p.y, p.z))
                    .is_some_and(|s| scene.registry.occludes(s))
        })
        .step_by(5)
        .take(3);
    for (i, p) in at_wall.enumerate() {
        for (j, (yaw, pitch)) in [(90.0, 0.0), (70.0, -20.0)].iter().enumerate() {
            out.push(View {
                name: format!("at_wall{i}_{j}"),
                eye: p.as_i64vec3().as_dvec3() + DVec3::new(1.0 / 64.0, 0.5, 0.5),
                yaw: *yaw,
                pitch: *pitch,
            });
        }
    }
    assert!(out.len() >= 13, "too few rock views: {}", out.len());
    out
}

// ---------------------------------------------------------------- rendering

fn view_transform(view: &View, shift: I64Vec3) -> Transform {
    Transform::from_yaw_pitch(
        view.eye + shift.as_dvec3(),
        view.yaw.to_radians(),
        view.pitch.to_radians(),
    )
}

fn view_params(view: &View, shift: I64Vec3) -> (ViewParams, CameraView) {
    let transform = view_transform(view, shift);
    let camera = Camera::default();
    let cv = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    (
        ViewParams::from_camera(&cv, &camera, DEFAULT_SKY_COLOR, 0.6),
        cv,
    )
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_cave_culling"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// Per view: the image without culling, the image with it, the chunks it hid, and those of
/// them inside the frustum (hidden by rock, not by the view direction).
struct Rendered {
    plain: RgbaImage,
    culled: RgbaImage,
    hidden: u32,
    occluded: usize,
    /// The eye saw from open space ([`eye_in_open`]): the frame was culled.
    open: bool,
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    scene: &Scene,
    views: &[View],
    shift: I64Vec3,
) -> Result<Vec<Rendered>, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        arena_quads: 1 << 21,
        max_chunk_slots: 1024,
        translucent_sort: TranslucentSort::Inline,
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
    let chunk_shift = shift >> CHUNK_SHIFT;
    for (pos, mesh) in &scene.meshes {
        renderer
            .upsert_chunk(
                chunk_id(*pos),
                pos.origin().as_i64vec3() + shift,
                Arc::clone(mesh),
            )
            .map_err(|e| format!("upsert {pos:?}: {e}"))?;
    }
    let links = |p: ChunkPos| {
        let local = ChunkPos::from_i64vec3(p.as_i64vec3() - chunk_shift);
        scene.links.get(&local).copied().unwrap_or(ALL_LINKS)
    };
    let mut out = Vec::new();
    for view in views {
        let (params, cv) = view_params(view, shift);
        renderer.set_visibility(None);
        render(&device, &queue, &mut renderer, &params)?; // warm-up (translucent sort)
        let plain = render(&device, &queue, &mut renderer, &params)?;
        // As the render plugin: no culling unless the eye sees from open space.
        let reach = Camera::default().near_reach(WIDTH as f32 / HEIGHT as f32);
        let open = eye_in_open(cv.render_origin, cv.eye_rel, reach, |p| {
            scene.occludes(BlockPos::from_i64vec3(p.as_i64vec3() - shift))
        });
        let search = VisibilityView::new(cv.render_origin, Some(cv.frustum), 8, 8);
        let set = open.then(|| visible_chunks(&search, links));
        let occluded = set.as_ref().map_or(0, |set| {
            scene
                .meshes
                .iter()
                .filter(|(pos, _)| {
                    let shifted = ChunkPos::from_i64vec3(pos.as_i64vec3() + chunk_shift);
                    let min = ((shifted.as_i64vec3() << CHUNK_SHIFT) - cv.render_origin).as_vec3();
                    let aabb = Aabb::new(min, min + Vec3::splat(32.0));
                    cv.frustum.intersects_aabb(&aabb) && !set.is_visible(shifted)
                })
                .count()
        });
        renderer.set_visibility(set.map(Arc::new));
        let culled = render(&device, &queue, &mut renderer, &params)?;
        let hidden = renderer.stats().occluded_chunks;
        out.push(Rendered {
            plain,
            culled,
            hidden,
            occluded,
            open,
        });
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(out)
}

struct Report {
    failures: Vec<String>,
    dir: PathBuf,
}

impl Report {
    fn identical(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        let diff = a.1.compare(b.1, 0);
        if diff.differing_pixels == 0 {
            return;
        }
        let message = format!(
            "{what}: {} of {} pixels differ (max channel {})",
            diff.differing_pixels, diff.total_pixels, diff.max_channel_diff
        );
        eprintln!("gpu_cave_culling: FAIL {message}");
        let _ = std::fs::create_dir_all(&self.dir);
        for (name, image) in [a, b] {
            let file: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let _ = image.save_png(&self.dir.join(format!("{file}.png")));
        }
        self.failures.push(message);
    }
}

#[test]
fn cave_culling_changes_no_pixel() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let scene = cave_scene();
    let views = views(&scene);
    eprintln!(
        "gpu_cave_culling: {} meshes, {} views",
        scene.meshes.len(),
        views.len()
    );
    let mut report = Report {
        failures: Vec::new(),
        dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_cave_culling"),
    };
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let near = match render_tier(adapter, &caps, tier, &scene, &views, I64Vec3::ZERO) {
                Ok(near) => near,
                Err(e) => {
                    report.failures.push(format!("{label} {tier}: {e}"));
                    continue;
                }
            };
            rendered += 1;
            let (mut hidden_views, mut culled_views) = (0, 0);
            for (view, r) in views.iter().zip(&near) {
                let name = format!("{label}_{tier}_{}", view.name);
                if view.name.starts_with("in_rock") || view.name.starts_with("at_wall") {
                    // Without the guard these views lose pixels on every tier. With it they are
                    // drawn twice with the same input; High on llvmpipe orders its draws
                    // differently from one frame to the next, which shows on a few pixels where
                    // the near plane cuts faces, so the images are not compared.
                    if r.open || r.hidden != 0 {
                        report.failures.push(format!(
                            "{label} {tier} {}: culled although the eye sees through rock",
                            view.name
                        ));
                    }
                    continue;
                }
                report.identical(
                    &format!(
                        "{label} {tier} {}: with and without cave culling",
                        view.name
                    ),
                    (&format!("{name}_plain"), &r.plain),
                    (&format!("{name}_culled"), &r.culled),
                );
                hidden_views += usize::from(r.occluded > 0);
                culled_views += usize::from(r.open);
            }
            let hidden: Vec<(u32, usize)> = near.iter().map(|r| (r.hidden, r.occluded)).collect();
            eprintln!(
                "gpu_cave_culling: {label} {tier}: chunks hidden per view (all, in the frustum) \
                 {hidden:?}"
            );
            if hidden_views < culled_views / 2 {
                report.failures.push(format!(
                    "{label} {tier}: only {hidden_views} of {culled_views} culled views hid a \
                     chunk in the frustum"
                ));
            }
            for shift in FAR_SHIFTS {
                let far = match render_tier(adapter, &caps, tier, &scene, &views, shift) {
                    Ok(far) => far,
                    Err(e) => {
                        report
                            .failures
                            .push(format!("{label} {tier} shift {shift}: {e}"));
                        continue;
                    }
                };
                for ((view, n), f) in views.iter().zip(&near).zip(&far) {
                    let name = format!("{label}_{tier}_{}", view.name);
                    if f.open != n.open {
                        report.failures.push(format!(
                            "{label} {tier} {} shift {shift}: open {} at the origin, {}",
                            view.name, n.open, f.open
                        ));
                    }
                    if !n.open {
                        continue;
                    }
                    report.identical(
                        &format!(
                            "{label} {tier} {} shift {shift}: culled vs origin",
                            view.name
                        ),
                        (&format!("{name}_plain"), &n.plain),
                        (&format!("{name}_culled_shift{shift}"), &f.culled),
                    );
                    if f.hidden != n.hidden {
                        report.failures.push(format!(
                            "{label} {tier} {} shift {shift}: {} chunks hidden, {} at the origin",
                            view.name, f.hidden, n.hidden
                        ));
                    }
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

// ---------------------------------------------------------------- refused uploads

/// Per view, with two chunks ahead of the eye refused by a full chunk table: the image without
/// culling, the one drawn right after the refusal with a set that trusts their links, the one
/// with the set of a world told about them, and those with a set that trusts their links or
/// only opens them (to show what goes wrong without the fix).
struct RefusedImages {
    plain: RgbaImage,
    first: RgbaImage,
    told: RgbaImage,
    trusted: RgbaImage,
    opened: RgbaImage,
}

/// Two chunks with meshes ahead of the eye of `view` along its main axis.
fn refused_chunks(scene: &Scene, view: &View) -> Vec<ChunkPos> {
    let (_, cv) = view_params(view, I64Vec3::ZERO);
    let eye = ChunkPos::from_i64vec3(cv.render_origin >> CHUNK_SHIFT);
    let f = cv.forward;
    let axis = if f.x.abs() >= f.y.abs() && f.x.abs() >= f.z.abs() {
        I64Vec3::new(f.x.signum() as i64, 0, 0)
    } else if f.y.abs() >= f.z.abs() {
        I64Vec3::new(0, f.y.signum() as i64, 0)
    } else {
        I64Vec3::new(0, 0, f.z.signum() as i64)
    };
    (1..=6)
        .map(|k| ChunkPos::from_i64vec3(eye.as_i64vec3() + axis * k))
        .filter(|p| scene.meshes.iter().any(|(m, _)| m == p))
        .take(2)
        .collect()
}

fn render_refused(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    scene: &Scene,
    views: &[(&View, Vec<ChunkPos>)],
) -> Result<Vec<RefusedImages>, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        arena_quads: 1 << 21,
        max_chunk_slots: scene.meshes.len() as u32 - 2,
        translucent_sort: TranslucentSort::Inline,
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
    let links = |p: ChunkPos| scene.links.get(&p).copied().unwrap_or(ALL_LINKS);
    let mut out = Vec::new();
    for (view, refused) in views {
        let (params, cv) = view_params(view, I64Vec3::ZERO);
        renderer.clear_chunks();
        renderer.take_refused(|_, _| {});
        renderer.set_visibility(None);
        for (pos, mesh) in scene.meshes.iter().filter(|(p, _)| !refused.contains(p)) {
            renderer
                .upsert_chunk(chunk_id(*pos), pos.origin().as_i64vec3(), Arc::clone(mesh))
                .map_err(|e| format!("upsert {pos:?}: {e}"))?;
        }
        render(&device, &queue, &mut renderer, &params)?; // warm-up (translucent sort)
        let plain = render(&device, &queue, &mut renderer, &params)?;
        for pos in refused {
            let mesh = scene.meshes.iter().find(|(p, _)| p == pos).expect("mesh");
            if renderer
                .upsert_chunk(
                    chunk_id(*pos),
                    pos.origin().as_i64vec3(),
                    Arc::clone(&mesh.1),
                )
                .is_ok()
            {
                return Err(format!("{pos:?} was not refused"));
            }
        }
        let search = VisibilityView::new(cv.render_origin, Some(cv.frustum), 8, 8);
        let trusting = visible_chunks(&search, links);
        // The frame of the refusal: its set did not know.
        renderer.set_visibility(Some(Arc::new(trusting.clone())));
        let first = render(&device, &queue, &mut renderer, &params)?;
        // The next frames: the world knows.
        let mut vis = ChunkVisibility::default();
        renderer.take_refused(|pos, refused| vis.set_unshown(pos, refused, links(pos)));
        renderer.set_visibility(Some(vis.update(&search, links)));
        let told = render(&device, &queue, &mut renderer, &params)?;
        // Without the fix: the refused chunks trusted, or only counted as open.
        renderer.set_visibility(Some(Arc::new(trusting)));
        let trusted = render(&device, &queue, &mut renderer, &params)?;
        let opening = visible_chunks(&search, |p| {
            if refused.contains(&p) {
                ALL_LINKS
            } else {
                links(p)
            }
        });
        renderer.set_visibility(Some(Arc::new(opening)));
        let opened = render(&device, &queue, &mut renderer, &params)?;
        out.push(RefusedImages {
            plain,
            first,
            told,
            trusted,
            opened,
        });
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(out)
}

/// Chunks whose upload the renderer refused (its chunk table is full) are not drawn, and where
/// their faces are missing the rock around them shows what lies behind it. Cave culling must
/// see through them: on the frame of the refusal (the renderer draws without the set when it
/// reached one) and once the world knows ([`VoxelRenderer::take_refused`],
/// [`ChunkVisibility::set_unshown`]). Trusting their links, or only counting them as open,
/// loses pixels.
#[test]
fn refused_uploads_hide_nothing() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let scene = cave_scene();
    let all = views(&scene);
    let views: Vec<(&View, Vec<ChunkPos>)> = all
        .iter()
        .filter(|v| v.name.starts_with("cave") || v.name.starts_with("surface"))
        .map(|v| (v, refused_chunks(&scene, v)))
        .filter(|(_, refused)| refused.len() == 2)
        .collect();
    assert!(views.len() >= 4, "too few views: {}", views.len());
    let mut report = Report {
        failures: Vec::new(),
        dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_cave_culling_refused"),
    };
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let images = match render_refused(adapter, &caps, tier, &scene, &views) {
                Ok(images) => images,
                Err(e) => {
                    report.failures.push(format!("{label} {tier}: {e}"));
                    continue;
                }
            };
            rendered += 1;
            let (mut trusted_lost, mut opened_lost) = (0, 0);
            for ((view, _), r) in views.iter().zip(&images) {
                let name = format!("{label}_{tier}_refused_{}", view.name);
                let plain = (format!("{name}_plain"), &r.plain);
                for (what, image) in [("first", &r.first), ("told", &r.told)] {
                    report.identical(
                        &format!("{label} {tier} {} refused, {what} frame", view.name),
                        (&plain.0, plain.1),
                        (&format!("{name}_{what}"), image),
                    );
                }
                trusted_lost += usize::from(r.plain.compare(&r.trusted, 0).differing_pixels > 0);
                opened_lost += usize::from(r.plain.compare(&r.opened, 0).differing_pixels > 0);
            }
            eprintln!(
                "gpu_cave_culling: {label} {tier}: of {} views with refused chunks, {trusted_lost} \
                 lose pixels trusting their links, {opened_lost} counting them only as open",
                views.len()
            );
            if trusted_lost == 0 {
                report.failures.push(format!(
                    "{label} {tier}: no view loses pixels trusting refused chunks (the test proves \
                     nothing)"
                ));
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

// ---------------------------------------------------------------- lost uploads

/// Per view, with two chunks ahead of the eye uploaded in a frame whose uploads are lost (its
/// staging buffer cannot be created, as when out of memory): that frame without culling, the
/// same frame with a set that trusts their links (the world shows a chunk from the frame its
/// mesh is published), and the next frame with that set, before any resync (what the lost frame
/// drew before the fix).
struct LostImages {
    plain: RgbaImage,
    culled: RgbaImage,
    after: RgbaImage,
}

/// One frame of `renderer` whose uploads are lost (`debug_fail_next_upload`): wgpu reports the
/// staging buffer (taken off `errors`) and the renderer says so (`take_lost_uploads`).
fn render_losing_uploads(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
    errors: &Mutex<Vec<String>>,
) -> Result<RgbaImage, String> {
    let before = errors.lock().map_err(|e| e.to_string())?.len();
    renderer.debug_fail_next_upload();
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu_cave_culling lost uploads"),
    });
    renderer.render(device, queue, &mut encoder, view);
    if !renderer.take_lost_uploads() {
        return Err("the uploads were not lost".to_string());
    }
    {
        let mut errors = errors.lock().map_err(|e| e.to_string())?;
        if errors.len() == before {
            return Err("no error for the staging buffer".to_string());
        }
        // expected: the tier fails on any other uncaptured error
        errors.truncate(before);
    }
    submit_and_wait(device, queue, encoder)?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

fn render_lost(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    scene: &Scene,
    views: &[(&View, Vec<ChunkPos>)],
) -> Result<Vec<LostImages>, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        arena_quads: 1 << 21,
        max_chunk_slots: 1024,
        translucent_sort: TranslucentSort::Inline,
    };
    let textures = boxel_render::generate_block_textures(scene.registry.textures());
    let links = |p: ChunkPos| scene.links.get(&p).copied().unwrap_or(ALL_LINKS);
    let mut out = Vec::new();
    for (view, lost) in views {
        let (params, cv) = view_params(view, I64Vec3::ZERO);
        let search = VisibilityView::new(cv.render_origin, Some(cv.frustum), 8, 8);
        let trusting = Arc::new(visible_chunks(&search, links));
        // A new renderer per lost frame: the arena and the table hold the same bytes where the
        // lost copies should have gone, so the two frames draw the same chunks.
        let lost_frame = |set: Option<Arc<VisibleSet>>| {
            let mut renderer = VoxelRenderer::new(
                &device,
                &queue,
                tier,
                &config,
                &textures,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                UVec2::new(WIDTH, HEIGHT),
            )
            .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
            for (pos, mesh) in scene.meshes.iter().filter(|(p, _)| !lost.contains(p)) {
                renderer
                    .upsert_chunk(chunk_id(*pos), pos.origin().as_i64vec3(), Arc::clone(mesh))
                    .map_err(|e| format!("upsert {pos:?}: {e}"))?;
            }
            render(&device, &queue, &mut renderer, &params)?; // warm-up (translucent sort)
            render(&device, &queue, &mut renderer, &params)?;
            for pos in lost {
                let mesh = scene.meshes.iter().find(|(p, _)| p == pos).expect("mesh");
                renderer
                    .upsert_chunk(
                        chunk_id(*pos),
                        pos.origin().as_i64vec3(),
                        Arc::clone(&mesh.1),
                    )
                    .map_err(|e| format!("upsert {pos:?}: {e}"))?;
            }
            renderer.set_visibility(set);
            let image = render_losing_uploads(&device, &queue, &mut renderer, &params, &errors)?;
            Ok::<_, String>((renderer, image))
        };
        let (_, plain) = lost_frame(None)?;
        let (mut renderer, culled) = lost_frame(Some(Arc::clone(&trusting)))?;
        let after = render(&device, &queue, &mut renderer, &params)?;
        out.push(LostImages {
            plain,
            culled,
            after,
        });
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(out)
}

/// A frame whose chunk uploads are lost (a staging buffer that cannot be created, as when out
/// of memory) culls nothing: the world already shows the chunks it published that frame and
/// trusts their links, but the GPU does not hold their meshes, so the rock around them shows
/// what lies behind it. The render plugin resyncs on the next frame (which culls nothing
/// either). Culling that frame with the set loses pixels.
#[test]
fn lost_uploads_hide_nothing() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let scene = cave_scene();
    let all = views(&scene);
    // The two chunks ahead of the eye of `refused_uploads_hide_nothing`, lost instead.
    let views: Vec<(&View, Vec<ChunkPos>)> = all
        .iter()
        .filter(|v| v.name.starts_with("cave") || v.name.starts_with("surface"))
        .map(|v| (v, refused_chunks(&scene, v)))
        .filter(|(_, lost)| lost.len() == 2)
        .collect();
    assert!(views.len() >= 4, "too few views: {}", views.len());
    let mut report = Report {
        failures: Vec::new(),
        dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_cave_culling_lost"),
    };
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let images = match render_lost(adapter, &caps, tier, &scene, &views) {
                Ok(images) => images,
                Err(e) => {
                    report.failures.push(format!("{label} {tier}: {e}"));
                    continue;
                }
            };
            rendered += 1;
            let mut after_lost = 0;
            for ((view, _), r) in views.iter().zip(&images) {
                let name = format!("{label}_{tier}_lost_{}", view.name);
                report.identical(
                    &format!("{label} {tier} {} lost uploads, culled frame", view.name),
                    (&format!("{name}_plain"), &r.plain),
                    (&format!("{name}_culled"), &r.culled),
                );
                after_lost += usize::from(r.plain.compare(&r.after, 0).differing_pixels > 0);
            }
            eprintln!(
                "gpu_cave_culling: {label} {tier}: of {} views with lost uploads, {after_lost} \
                 lose pixels culling with the set",
                views.len()
            );
            if after_lost == 0 {
                report.failures.push(format!(
                    "{label} {tier}: no view loses pixels culling with lost uploads (the test \
                     proves nothing)"
                ));
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

// ---------------------------------------------------------------- what the world publishes

/// Tunnel cross-section (y and z, world voxels) of [`Tunnel`].
const TUNNEL: std::ops::RangeInclusive<i64> = 12..=20;
/// The eye of the tunnel views: in the tunnel, in chunk (0, 0, 0), looking along `+x`.
const TUNNEL_EYE: DVec3 = DVec3::new(8.5, 16.5, 16.5);

/// Stone everywhere but a tunnel along `+x` (from x = -24 to `end`, [`TUNNEL`] in y and z) and
/// a closed pocket in chunk (4, 0, 0) (x 136..=150, y and z 4..=28). The pocket's far wall shows
/// from the eye only through rock whose faces are not drawn.
struct Tunnel {
    end: i64,
}

impl WorldGenerator for Tunnel {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let stone = registry.by_name("boxel:stone").expect("stone");
        let mut data = ChunkData::uniform(stone);
        let o = key.pos.origin();
        let mut carve = |x: (i64, i64), y: (i64, i64), z: (i64, i64)| {
            for wy in o.y.max(y.0)..=(o.y + 31).min(y.1) {
                for wz in o.z.max(z.0)..=(o.z + 31).min(z.1) {
                    for wx in o.x.max(x.0)..=(o.x + 31).min(x.1) {
                        let local =
                            LocalPos::new((wx - o.x) as u32, (wy - o.y) as u32, (wz - o.z) as u32);
                        data.set(local, BlockState::AIR);
                    }
                }
            }
        };
        carve((-24, self.end), (12, 20), (12, 20));
        carve((136, 150), (4, 28), (4, 28));
        data
    }

    fn name(&self) -> &str {
        "tunnel"
    }
}

/// Holds the world's jobs until the test runs them.
#[derive(Default)]
struct Steps {
    jobs: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl Steps {
    fn held(&self) -> usize {
        self.jobs.lock().map(|j| j.len()).unwrap_or(0)
    }
    /// Runs held job `i` (`None`: the newest).
    fn run(&self, i: Option<usize>) {
        let job = self.jobs.lock().ok().and_then(|mut jobs| match i {
            Some(i) if i < jobs.len() => Some(jobs.remove(i)),
            Some(_) => None,
            None => jobs.pop(),
        });
        if let Some(job) = job {
            job();
        }
    }
    fn run_all(&self) {
        let jobs = self
            .jobs
            .lock()
            .map(|mut j| std::mem::take(&mut *j))
            .unwrap_or_default();
        for job in jobs {
            job();
        }
    }
}

impl boxel_world::jobs::JobExecutor for Steps {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.push(job);
        }
    }
}

/// One state of the world to draw: the mesh events since the previous one, the set the render
/// plugin uses (`None`: it culls nothing), a fresh search (that set once the links that closed
/// are searched again, after `TIGHTEN_DELAY`; on a resync frame, the set without the plugin's
/// resync guard) and the set of a search that does not see through chunks the renderer does not
/// show as they are (no `XRAY`).
struct WorldFrame {
    /// The first state of a world (the renderer starts empty).
    first: bool,
    /// A renderer resync: the renderer drops every chunk, then `events` are every settled mesh.
    resync: bool,
    label: String,
    events: Vec<ChunkMeshEvent>,
    set: Option<Arc<VisibleSet>>,
    fresh: VisibleSet,
    without_xray: VisibleSet,
}

/// A world of [`Tunnel`] (its jobs held by [`Steps`]) and the frames recorded from it.
struct TunnelWorld {
    app: boxel_app::App,
    steps: Arc<Steps>,
    registry: BlockRegistry,
    /// Searched box: radius and vertical radius around the eye's chunk.
    radius: u32,
    frames: Vec<WorldFrame>,
    pending: Vec<ChunkMeshEvent>,
}

impl TunnelWorld {
    fn new(end: i64, edit_batch_hold: Duration, viewers: &[(DVec3, u32)], radius: u32) -> Self {
        let steps = Arc::new(Steps::default());
        let mut app = boxel_app::App::new();
        app.add_plugins(WorldPlugin {
            generator: Arc::new(Tunnel { end }),
            registry: Arc::new(boxel_demo::blocks::registry()),
            settings: WorldSettings {
                max_jobs_in_flight: 4096,
                interest_budget: Duration::MAX,
                job_budget: Duration::MAX,
                edit_batch_hold,
                ..WorldSettings::default()
            },
            executor: steps.clone(),
        });
        for &(at, r) in viewers {
            app.world_mut().spawn((
                Transform::from_translation(at),
                ChunkViewer::new(r).with_vertical_radius(1),
            ));
        }
        Self {
            app,
            steps,
            registry: boxel_demo::blocks::registry(),
            radius,
            frames: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// One frame; returns whether it published a mesh event.
    fn update(&mut self) -> bool {
        self.app.update();
        let events: Vec<ChunkMeshEvent> = self
            .app
            .world_mut()
            .resource_mut::<Messages<ChunkMeshEvent>>()
            .drain()
            .collect();
        let any = !events.is_empty();
        self.pending.extend(events);
        any
    }

    fn settled(&self) -> bool {
        let stats = self.app.world().resource::<WorldStats>();
        stats.chunks > 0 && stats.is_settled() && self.steps.held() == 0
    }

    fn settle(&mut self) {
        for _ in 0..500 {
            self.update();
            if self.settled() {
                return;
            }
            self.steps.run_all();
        }
        panic!("tunnel world not settled");
    }

    /// Records the current state, as the render plugin culls it this frame.
    fn record(&mut self, label: String) {
        self.record_frame(label, false);
    }

    /// A renderer resync frame, as the render plugin runs it: the mesh events of the frame are
    /// dropped, the renderer drops every chunk and gets every settled mesh
    /// (`ChunkMap::ready_meshes`), and the world hears of it next frame (`ChunkMeshResync`).
    fn resync(&mut self, label: String) {
        let map = self.app.world().resource::<ChunkMap>();
        self.pending = map
            .ready_meshes()
            .map(|(id, key, mesh)| ChunkMeshEvent::Upsert {
                id,
                key,
                mesh: Arc::clone(mesh),
            })
            .collect();
        self.record_frame(label, true);
        self.app.world_mut().write_message(ChunkMeshResync);
    }

    fn record_frame(&mut self, label: String, resync: bool) {
        let tunnel = tunnel_view();
        let (_, cv) = view_params(&tunnel, I64Vec3::ZERO);
        let view = VisibilityView::new(cv.render_origin, Some(cv.frustum), self.radius, 1);
        let transform = view_transform(&tunnel, I64Vec3::ZERO);
        let viewer = ChunkViewer::new(self.radius).with_vertical_radius(1);
        let registry = &self.registry;
        let world = self.app.world_mut();
        let set = world.resource_scope(|world, mut vis: boxel_ecs::world::Mut<ChunkVisibility>| {
            let map = world.resource::<ChunkMap>();
            boxel_render::frame_visibility(
                resync,
                registry,
                map,
                &mut vis,
                &Camera::default(),
                &transform,
                viewer,
                WIDTH as f32 / HEIGHT as f32,
            )
        });
        let map = self.app.world().resource::<ChunkMap>();
        let fresh = visible_chunks(&view, |p| map.visibility_links_at(p));
        let without_xray = visible_chunks(&view, |p| map.visibility_links_at(p) & !XRAY);
        self.frames.push(WorldFrame {
            first: self.frames.is_empty(),
            resync,
            label,
            events: std::mem::take(&mut self.pending),
            set,
            fresh,
            without_xray,
        });
    }
}

fn tunnel_view() -> View {
    View {
        name: "tunnel".into(),
        eye: TUNNEL_EYE,
        yaw: -90.0,
        pitch: 0.0,
    }
}

/// The recorded states of the tunnel world: two viewers with a chunk between them that has data
/// and no mesh (the tunnel ends in it); the meshes of an edit batch published apart, the newest
/// job first (the tunnel's last voxel layer closed: the chunk past it loses its end wall before
/// the edited chunk shows the new one); a renderer resync while the chunk of the tunnel's end
/// wall is remeshed for a change that keeps what occludes (a lamp in the wall: the map still
/// shows the chunk and trusts its links, but the renderer gets only the settled meshes); and the
/// world streaming in with its jobs run in a scrambled order (recorded when meshes were
/// published or the set changed).
fn tunnel_frames() -> Vec<WorldFrame> {
    let mut out = Vec::new();
    // A box that reaches the pocket from the eye.
    let mut gap = TunnelWorld::new(
        80,
        Duration::ZERO,
        &[
            (TUNNEL_EYE, 1),
            (TUNNEL_EYE + DVec3::new(128.0, 0.0, 0.0), 1),
        ],
        6,
    );
    gap.settle();
    gap.record("gap".into());
    out.extend(gap.frames);

    let mut batch = TunnelWorld::new(95, Duration::ZERO, &[(TUNNEL_EYE, 5)], 5);
    batch.settle();
    batch.record("batch settled".into());
    let stone = batch.registry.by_name("boxel:stone").expect("stone");
    {
        let mut queue = batch.app.world_mut().resource_mut::<EditQueue>();
        for y in TUNNEL {
            for z in TUNNEL {
                queue.set_block(BlockPos::new(95, y, z), stone);
            }
        }
    }
    batch.update();
    batch.record("batch edited".into());
    let mut step = 0;
    while !batch.settled() {
        batch.steps.run(None);
        batch.update();
        batch.record(format!("batch step {step}"));
        step += 1;
        assert!(step < 100, "edit batch not settled");
    }
    out.extend(batch.frames);

    let mut resync = TunnelWorld::new(95, Duration::ZERO, &[(TUNNEL_EYE, 5)], 5);
    resync.settle();
    resync.record("resync settled".into());
    let lamp = resync.registry.by_name("boxel:lamp_white").expect("lamp");
    assert!(
        resync.registry.occludes(lamp),
        "the lamp must occlude like stone"
    );
    // The middle of the end wall, in chunk (3, 0, 0).
    resync
        .app
        .world_mut()
        .resource_mut::<EditQueue>()
        .set_block(BlockPos::new(96, 16, 16), lamp);
    resync.update();
    {
        let map = resync.app.world().resource::<ChunkMap>();
        let wall = map.id_at(ChunkPos::new(3, 0, 0)).expect("wall chunk");
        assert!(
            map.get(wall).is_some_and(ChunkSlot::shown),
            "a remesh that keeps what occludes keeps the wall chunk shown"
        );
        assert!(
            map.ready_meshes().all(|(id, _, _)| id != wall),
            "the wall chunk is being remeshed"
        );
    }
    resync.resync("resync frame".into());
    // The world hears of the resync: the chunks being remeshed are no longer shown.
    resync.update();
    resync.record("resync next".into());
    let mut step = 0;
    while !resync.settled() {
        resync.steps.run(None);
        resync.update();
        resync.record(format!("resync step {step}"));
        step += 1;
        assert!(step < 100, "resync world not settled");
    }
    out.extend(resync.frames);

    let mut stream = TunnelWorld::new(80, Duration::ZERO, &[(TUNNEL_EYE, 5)], 5);
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut last: Option<Arc<VisibleSet>> = None;
    let mut frames = 0;
    while !stream.settled() {
        let held = stream.steps.held();
        if held > 0 {
            rng ^= rng >> 12;
            rng ^= rng << 25;
            rng ^= rng >> 27;
            let pick = rng.wrapping_mul(0x2545_f491_4f6c_dd1d) % held as u64;
            stream.steps.run(Some(pick as usize));
        }
        let published = stream.update();
        frames += 1;
        assert!(frames < 20_000, "streaming not settled");
        let n = stream.frames.len();
        stream.record(format!("stream frame {frames}"));
        let set = stream.frames[n].set.clone();
        let changed = match (&set, &last) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (a, b) => a.is_some() != b.is_some(),
        };
        if !published && !changed {
            // Nothing new to draw: keep its events for the next recorded frame.
            let frame = stream.frames.pop().expect("recorded");
            stream.pending = frame.events;
        }
        last = set;
    }
    out.extend(stream.frames);
    out
}

/// The images of one recorded state: without culling, with the culling sets that apply (the
/// render plugin's and a fresh search's, named), with the set without `XRAY` (when it differs
/// from the fresh one) and, on a resync frame, with the fresh search the plugin would use
/// without its resync guard.
struct WorldImages {
    plain: RgbaImage,
    culled: Vec<(&'static str, RgbaImage)>,
    without_xray: Option<RgbaImage>,
    unguarded: Option<RgbaImage>,
}

/// Renders every recorded state of the tunnel world with the meshes the world published.
fn render_world_frames(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    frames: &[WorldFrame],
) -> Result<Vec<WorldImages>, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        arena_quads: 1 << 21,
        max_chunk_slots: 4096,
        translucent_sort: TranslucentSort::Inline,
    };
    let registry = boxel_demo::blocks::registry();
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
    let (params, _) = view_params(&tunnel_view(), I64Vec3::ZERO);
    let mut out = Vec::new();
    for frame in frames {
        if frame.first || frame.resync {
            renderer.clear_chunks();
        }
        for event in &frame.events {
            match event {
                ChunkMeshEvent::Upsert { id, key, mesh } => renderer
                    .upsert_chunk(
                        id.to_bits(),
                        key.pos.origin().as_i64vec3(),
                        Arc::clone(mesh),
                    )
                    .map_err(|e| format!("upsert {key:?}: {e}"))?,
                ChunkMeshEvent::Remove { id, .. } => {
                    renderer.remove_chunk(id.to_bits());
                }
            }
        }
        renderer.take_refused(|pos, _| panic!("{pos:?} refused"));
        renderer.set_visibility(None);
        render(&device, &queue, &mut renderer, &params)?; // warm-up (translucent sort)
        let plain = render(&device, &queue, &mut renderer, &params)?;
        let mut culled = Vec::new();
        let mut without_xray = None;
        let mut unguarded = None;
        if frame.resync {
            renderer.set_visibility(Some(Arc::new(frame.fresh.clone())));
            unguarded = Some(render(&device, &queue, &mut renderer, &params)?);
        }
        if let Some(set) = &frame.set {
            renderer.set_visibility(Some(Arc::clone(set)));
            culled.push(("culled", render(&device, &queue, &mut renderer, &params)?));
            if **set != frame.fresh {
                renderer.set_visibility(Some(Arc::new(frame.fresh.clone())));
                culled.push(("fresh", render(&device, &queue, &mut renderer, &params)?));
            }
            if frame.fresh != frame.without_xray {
                renderer.set_visibility(Some(Arc::new(frame.without_xray.clone())));
                without_xray = Some(render(&device, &queue, &mut renderer, &params)?);
            }
        }
        out.push(WorldImages {
            plain,
            culled,
            without_xray,
            unguarded,
        });
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(out)
}

/// The render plugin's culling over the meshes the world really published, while some are
/// missing or stale: a chunk with data and no mesh between the eye and drawn chunks, the meshes
/// of an edit batch published apart, a renderer resync while a chunk is remeshed, and a world
/// streaming in. Where a chunk is not drawn as its blocks are, the rock around it shows the far
/// wall of a pocket the face links do not reach. With the world's links (`XRAY` on such chunks)
/// and the plugin's set (nothing culled on the resync frame) no pixel changes on any tier; a
/// search that only opens them, or one on the resync frame, loses pixels (the test proves
/// something).
#[test]
fn chunks_not_drawn_as_they_are_hide_nothing() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let frames = tunnel_frames();
    eprintln!("gpu_cave_culling: {} tunnel world states", frames.len());
    let mut report = Report {
        failures: Vec::new(),
        dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_cave_culling_world"),
    };
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let images = match render_world_frames(adapter, &caps, tier, &frames) {
                Ok(images) => images,
                Err(e) => {
                    report.failures.push(format!("{label} {tier}: {e}"));
                    continue;
                }
            };
            rendered += 1;
            let (mut culled, mut lost, mut unguarded_lost) = (0, 0, 0);
            for (frame, images) in frames.iter().zip(&images) {
                let name = format!("{label}_{tier}_{}", frame.label);
                culled += usize::from(!images.culled.is_empty());
                for (what, image) in &images.culled {
                    report.identical(
                        &format!(
                            "{label} {tier} {}: with and without cave culling ({what})",
                            frame.label
                        ),
                        (&format!("{name}_plain"), &images.plain),
                        (&format!("{name}_{what}"), image),
                    );
                }
                if let Some(without) = &images.without_xray {
                    lost += usize::from(images.plain.compare(without, 0).differing_pixels > 0);
                }
                if let Some(unguarded) = &images.unguarded {
                    unguarded_lost +=
                        usize::from(images.plain.compare(unguarded, 0).differing_pixels > 0);
                }
            }
            eprintln!(
                "gpu_cave_culling: {label} {tier}: {culled} culled tunnel states, {lost} lose \
                 pixels without XRAY, {unguarded_lost} culling on the resync frame"
            );
            if lost == 0 {
                report.failures.push(format!(
                    "{label} {tier}: no tunnel state loses pixels without XRAY (the test proves \
                     nothing)"
                ));
            }
            if unguarded_lost == 0 {
                report.failures.push(format!(
                    "{label} {tier}: culling on the resync frame loses no pixel (the test proves \
                     nothing)"
                ));
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
