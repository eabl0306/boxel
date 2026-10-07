//! Translucent quads sorted inside a chunk (ADR 0018, section 3) on every tier. Self-skips
//! without an adapter unless `BOXEL_REQUIRE_GPU=1`.
//!
//! Scenes of water and glass meshed by the real `Mesher`, each in one chunk, from two opposite
//! cameras:
//!
//! - glass in front of water (a water block and a glass block one behind the other);
//! - a glass box (its far walls behind its near ones);
//! - water in a glass pool (water and glass faces in the same planes) and a glass block under
//!   water, without a reference image.
//!
//! For every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local
//! capability check allows (the one of `gpu_tiers.rs`), a `VoxelRenderer` draws each view with
//! the mesher's order (`TranslucentSort::Off`) and sorted (`TranslucentSort::Inline`). The first
//! two scenes also get a reference built without the sorter: their quads split into chunks at
//! the same origin, one per group (the far block's faces looking away from the camera, its faces
//! looking at it, then the near block's), which the translucent pass draws in slot order (equal
//! distances). Asserts: the sorted image matches the reference, the mesher's order does not for
//! at least one camera of each scene (the bug this fixes), a second frame from the same camera
//! re-sorts nothing and draws the same pixels, and every tier draws the same sorted pixels as the
//! best tier of its adapter (0.1 % of the pixels, channel tolerance 2).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_translucent/` (every image with
//! `GPU_TRANSLUCENT_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{
    BUCKET_COUNT, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher, PackedQuad,
    TRANSLUCENT_BUCKET, layout,
};
use boxel_render::{DEFAULT_SKY_COLOR, RendererConfig, TranslucentSort, ViewParams, VoxelRenderer};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, LocalPos};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ in at most this fraction (channel tolerance 2).
const MAX_DIFF: f64 = 0.001;
/// The mesher's order must change at least this fraction of the pixels somewhere.
const MIN_MESHER_DIFF: f64 = 0.002;

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
        label: Some("gpu_translucent"),
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

// ---------------------------------------------------------------- scenes

/// Every block of the box `min..max` (exclusive) as `name`.
fn cuboid(min: [u32; 3], max: [u32; 3], name: &'static str) -> Vec<([u32; 3], &'static str)> {
    let mut out = Vec::new();
    for x in min[0]..max[0] {
        for y in min[1]..max[1] {
            for z in min[2]..max[2] {
                out.push(([x, y, z], name));
            }
        }
    }
    out
}

/// The real mesher's mesh of `blocks` in chunk (0, 0, 0), air around it, no light.
fn mesh_blocks(registry: &BlockRegistry, blocks: &[([u32; 3], &str)]) -> ChunkMesh {
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for &([x, y, z], name) in blocks {
        dense[LocalPos::new(x, y, z).index()] = registry.by_name(name).expect(name);
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

/// A camera at `eye` looking at `target`.
fn look(eye: DVec3, target: DVec3) -> ViewParams {
    let d = (target - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
    let view = CameraView::compute(&camera, &transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, 0.6)
}

/// One scene: its mesh, two cameras and whether its translucent quads form separate convex
/// blocks of one material each (then the reference image exists).
struct Scene {
    name: &'static str,
    mesh: ChunkMesh,
    eyes: [DVec3; 2],
    target: DVec3,
    blocks: bool,
}

fn scenes(registry: &BlockRegistry) -> Vec<Scene> {
    let mut front = cuboid([12, 8, 8], [16, 12, 12], "boxel:water");
    front.extend(cuboid([12, 8, 16], [16, 12, 20], "boxel:glass"));
    let mut pool = cuboid([8, 8, 8], [16, 11, 16], "boxel:glass");
    pool.retain(|&([x, y, z], _)| x == 8 || x == 15 || z == 8 || z == 15 || y == 8);
    pool.extend(cuboid([9, 9, 9], [15, 11, 15], "boxel:water"));
    let mut sunk = cuboid([6, 6, 6], [18, 10, 18], "boxel:water");
    sunk.retain(|&(p, _)| p != [10, 7, 11]);
    sunk.push(([10, 7, 11], "boxel:glass"));
    vec![
        Scene {
            name: "glass in front of water",
            mesh: mesh_blocks(registry, &front),
            eyes: [DVec3::new(18.5, 15.5, 30.0), DVec3::new(9.5, 15.0, -4.0)],
            target: DVec3::new(14.0, 10.0, 14.0),
            blocks: true,
        },
        Scene {
            name: "glass box",
            mesh: mesh_blocks(registry, &cuboid([10, 6, 10], [16, 12, 16], "boxel:glass")),
            eyes: [DVec3::new(22.0, 16.0, 24.0), DVec3::new(4.0, 3.0, 2.0)],
            target: DVec3::new(13.0, 9.0, 13.0),
            blocks: true,
        },
        Scene {
            name: "glass pool",
            mesh: mesh_blocks(registry, &pool),
            eyes: [DVec3::new(12.3, 17.0, 24.0), DVec3::new(25.0, 10.6, 10.0)],
            target: DVec3::new(12.0, 9.5, 12.0),
            blocks: false,
        },
        Scene {
            name: "glass under water",
            mesh: mesh_blocks(registry, &sunk),
            eyes: [DVec3::new(9.6, 12.0, 9.2), DVec3::new(24.0, 13.0, 25.0)],
            target: DVec3::new(10.5, 7.5, 11.5),
            blocks: false,
        },
    ]
}

/// A mesh with only `quads` in its translucent bucket (the palette of `of`).
fn translucent_mesh(of: &ChunkMesh, quads: Vec<PackedQuad>) -> ChunkMesh {
    let mut counts = [0; BUCKET_COUNT];
    counts[TRANSLUCENT_BUCKET] = quads.len() as u32;
    let mut mesh = ChunkMesh {
        quads,
        bucket_counts: counts,
        palette: of.palette.clone(),
        ..ChunkMesh::default()
    };
    mesh.hash = mesh.compute_hash();
    mesh
}

/// True if `quad` looks away from `eye` (its plane per `common.wgsl`).
fn faces_away(quad: PackedQuad, eye: DVec3) -> bool {
    let face = quad.face();
    let (n, _, _) = face.axes();
    let positive = face.index() % 2 == 0;
    let mut plane = f64::from(quad.pos()[n]) + if positive { 1.0 } else { 0.0 };
    if quad.shape() == layout::SHAPE_FLUID_SURFACE && n == 1 && positive {
        plane = f64::from(quad.pos()[1]) + f64::from(layout::FLUID_SURFACE);
    }
    positive != (eye.to_array()[n] > plane)
}

/// The reference groups of a scene of convex blocks (one material each), drawn in this order:
/// per block, farthest first, its faces looking away from `eye`, then the ones looking at it.
/// Faces of one group never overlap on screen.
fn reference_groups(mesh: &ChunkMesh, eye: DVec3) -> Vec<ChunkMesh> {
    let quads = mesh.bucket(TRANSLUCENT_BUCKET);
    let mut layers: Vec<u16> = quads.iter().map(|q| q.layer()).collect();
    layers.sort_unstable();
    layers.dedup();
    // distance from the eye to each block's center (the mean of its quads' min corners is
    // enough to tell two separate blocks apart)
    let center = |layer: u16| {
        let corners: Vec<DVec3> = quads
            .iter()
            .filter(|q| q.layer() == layer)
            .map(|q| DVec3::from_array(q.pos().map(f64::from)))
            .collect();
        corners.iter().sum::<DVec3>() / corners.len() as f64
    };
    layers.sort_by(|&a, &b| {
        let (da, db) = (center(a).distance(eye), center(b).distance(eye));
        db.total_cmp(&da)
    });
    let mut groups = Vec::new();
    for layer in layers {
        for away in [true, false] {
            let group: Vec<PackedQuad> = quads
                .iter()
                .copied()
                .filter(|&q| q.layer() == layer && faces_away(q, eye) == away)
                .collect();
            if !group.is_empty() {
                groups.push(translucent_mesh(mesh, group));
            }
        }
    }
    groups
}

// ---------------------------------------------------------------- rendering

/// Renders one frame and reads the main color back.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_translucent"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// Images of one scene from its two cameras on one tier.
struct SceneImages {
    mesher: Vec<RgbaImage>,
    sorted: Vec<RgbaImage>,
    /// The second frame from the same camera.
    again: Vec<RgbaImage>,
    reference: Vec<RgbaImage>,
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    scenes: &[Scene],
) -> Result<Vec<SceneImages>, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let registry = BlockRegistry::default_blocks();
    let config = RendererConfig {
        arena_quads: 1 << 16,
        max_chunk_slots: 64,
        translucent_sort: TranslucentSort::Off,
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
    let mut out = Vec::new();
    for scene in scenes {
        let mesh = Arc::new(scene.mesh.clone());
        let mut images = SceneImages {
            mesher: Vec::new(),
            sorted: Vec::new(),
            again: Vec::new(),
            reference: Vec::new(),
        };
        for eye in scene.eyes {
            let view = look(eye, scene.target);
            // the mesher's order: a fresh upload, not sorted
            renderer.clear_chunks();
            renderer.set_translucent_sort(TranslucentSort::Off);
            renderer
                .upsert_chunk(1, I64Vec3::ZERO, Arc::clone(&mesh))
                .map_err(|e| e.to_string())?;
            images
                .mesher
                .push(render(&device, &queue, &mut renderer, &view)?);
            // sorted in the same frame
            renderer.set_translucent_sort(TranslucentSort::Inline);
            let before = renderer.stats().translucent_sorts;
            images
                .sorted
                .push(render(&device, &queue, &mut renderer, &view)?);
            let sorts = renderer.stats().translucent_sorts;
            if sorts != before + 1 {
                return Err(format!(
                    "{}: {} sorts in one frame",
                    scene.name,
                    sorts - before
                ));
            }
            // same camera: nothing to re-sort
            images
                .again
                .push(render(&device, &queue, &mut renderer, &view)?);
            if renderer.stats().translucent_sorts != sorts {
                return Err(format!("{}: re-sorted for the same camera", scene.name));
            }
            if scene.blocks {
                renderer.clear_chunks();
                renderer.set_translucent_sort(TranslucentSort::Off);
                for (i, group) in reference_groups(&mesh, eye).into_iter().enumerate() {
                    renderer
                        .upsert_chunk(10 + i as u64, I64Vec3::ZERO, Arc::new(group))
                        .map_err(|e| e.to_string())?;
                }
                images
                    .reference
                    .push(render(&device, &queue, &mut renderer, &view)?);
            }
        }
        out.push(images);
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(out)
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
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_translucent"),
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
                Ok(()) => eprintln!("gpu_translucent: saved {}", path.display()),
                Err(e) => eprintln!("gpu_translucent: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_translucent: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }

    fn check_same(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        let diff = a.1.compare(b.1, 2);
        eprintln!(
            "gpu_translucent: {what}: {:.4} % of pixels differ, max channel {}",
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

#[test]
fn translucent_quads_are_sorted_inside_chunks_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let registry = BlockRegistry::default_blocks();
    let scenes = scenes(&registry);
    for scene in &scenes {
        let n = scene.mesh.bucket_counts[TRANSLUCENT_BUCKET];
        assert!(n >= 6, "{}: {n} translucent quads", scene.name);
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
        for (tier, images) in &tiers {
            rendered += 1;
            for (scene, i) in scenes.iter().zip(images) {
                let name =
                    |what: &str, k: usize| format!("{label}_{tier}_{}_{what}{k}", scene.name);
                if std::env::var_os("GPU_TRANSLUCENT_SAVE").is_some() {
                    for k in 0..2 {
                        report.save(&[
                            (&name("mesher", k), &i.mesher[k]),
                            (&name("sorted", k), &i.sorted[k]),
                        ]);
                        if let Some(r) = i.reference.get(k) {
                            report.save(&[(&name("reference", k), r)]);
                        }
                    }
                }
                let mut mesher_wrong = false;
                for k in 0..2 {
                    report.check_same(
                        &format!("{label} {tier}: {} camera {k}, second frame", scene.name),
                        (&name("sorted", k), &i.sorted[k]),
                        (&name("again", k), &i.again[k]),
                    );
                    let expected = i.reference.get(k).unwrap_or(&i.sorted[k]);
                    if let Some(reference) = i.reference.get(k) {
                        report.check_same(
                            &format!(
                                "{label} {tier}: {} camera {k}, sorted vs reference",
                                scene.name
                            ),
                            (&name("sorted", k), &i.sorted[k]),
                            (&name("reference", k), reference),
                        );
                    }
                    let changed = i.mesher[k].compare(expected, 2).fraction();
                    eprintln!(
                        "gpu_translucent: {label} {tier}: {} camera {k}: the mesher's order \
                         changes {:.3} % of the pixels",
                        scene.name,
                        changed * 100.0
                    );
                    mesher_wrong |= changed >= MIN_MESHER_DIFF;
                }
                if !mesher_wrong {
                    report.fail(
                        format!(
                            "{label} {tier}: {}: the mesher's order looks right from both \
                             cameras (the scene does not test the sort)",
                            scene.name
                        ),
                        &[
                            (&name("mesher", 0), &i.mesher[0]),
                            (&name("sorted", 0), &i.sorted[0]),
                        ],
                    );
                }
            }
        }
        let Some(((best, best_images), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, images) in rest {
            for ((scene, a), b) in scenes.iter().zip(best_images).zip(images) {
                for k in 0..2 {
                    report.check_same(
                        &format!("{label}: {tier} vs {best} ({} camera {k})", scene.name),
                        (
                            &format!("{label}_{best}_{}_sorted{k}", scene.name),
                            &a.sorted[k],
                        ),
                        (
                            &format!("{label}_{tier}_{}_sorted{k}", scene.name),
                            &b.sorted[k],
                        ),
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
