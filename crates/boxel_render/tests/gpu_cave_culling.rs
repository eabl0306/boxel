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
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_cave_culling/`.
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
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
use boxel_world::VisibilityView;
use boxel_world::visibility::{eye_in_open, visible_chunks};
use boxel_worldgen::{OverworldGenerator, WorldGenerator};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    let registry = BlockRegistry::default_blocks();
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

fn view_params(view: &View, shift: I64Vec3) -> (ViewParams, CameraView) {
    let transform = Transform::from_yaw_pitch(
        view.eye + shift.as_dvec3(),
        view.yaw.to_radians(),
        view.pitch.to_radians(),
    );
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
