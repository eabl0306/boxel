//! Edge walls (ADR 0031, E-D): a chunk carries the faces the chunk across each border would draw
//! toward it, and every tier draws them only while that chunk is not resident. Self-skips
//! without an adapter unless `BOXEL_REQUIRE_GPU=1`.
//!
//! The scene is two chunks of stone side by side along x, the first with a cave that runs into
//! its +X border, both meshed by the real `Mesher` with light (sky falling off down the cave,
//! block light by a corner, so corner lights and AO vary), the first with walls. The eye stands in the
//! cave and looks at that border, near and at an angle. For every adapter (filtered by
//! `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier it can run:
//! - the cave alone (the chunk across not resident) shows its outer wall: the walls; without
//!   them (a mesh without walls) that wall is the sky;
//! - with the chunk across resident its own faces show and the walls are left out: the same
//!   image as the cave meshed without walls next to that chunk, pixel for pixel;
//! - the walls look like the faces of the chunk across: the two images match but for the
//!   1/512 block the walls sit inside the border (a few edge pixels);
//! - after the chunk across goes again, the walls show again (the neighbor bits follow).
//!
//! Every tier must match the best tier of its adapter within 0.1 % of the pixels. Failing images
//! are saved under `CARGO_TARGET_TMPDIR/gpu_walls/`.
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_light::PaddedLight;
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshOptions, Mesher, PaddedChunk};
use boxel_render::{
    DEFAULT_SKY_COLOR, DistanceFog, RendererConfig, TranslucentSort, ViewParams, VoxelRenderer,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockState};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_TIER_DIFF: f64 = 0.001;

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
        label: Some("gpu_walls"),
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

/// The block at world voxel `p`: stone, but for the cave (x 16 to 31, y 6 to 25, z 6 to 25,
/// open to the +X border of the first chunk); the second chunk (x 32 to 63) is stone and dirt in
/// stripes, so its border face is not one quad.
fn block(r: &BlockRegistry, p: [i32; 3]) -> BlockState {
    let get = |name: &str| r.by_name(name).expect(name);
    let cave = (16..32).contains(&p[0]) && (6..26).contains(&p[1]) && (6..26).contains(&p[2]);
    if cave {
        BlockState::AIR
    } else if p[0] >= 32 && (p[1] + p[2]).rem_euclid(5) < 2 {
        get("boxel:dirt")
    } else {
        get("boxel:stone")
    }
}

/// The light at world voxel `p`: sky falling off from the cave's top row, and warm block light
/// near one corner of the cave.
fn light(p: [i32; 3]) -> u16 {
    let sky = (15 - (p[1] - 25).unsigned_abs().min(15)) as u16;
    let d = (p[0] - 16).abs() + (p[1] - 6).abs() + (p[2] - 6).abs();
    let block = (12 - d.min(12)) as u16;
    sky << 12 | block << 8 | (block / 2) << 4
}

/// The mesh of chunk `cx` (0 or 1 along x) as the world meshes it, with or without walls.
fn chunk_mesh(r: &BlockRegistry, table: &MaterialTable, cx: i32, walls: bool) -> ChunkMesh {
    let mut padded = PaddedChunk::new();
    let mut values = vec![0u16; boxel_light::PADDED_VOLUME];
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                let p = [cx * 32 + x, y, z];
                let s = block(r, p);
                padded.set(x, y, z, s.0, table.class(s.0));
                values[PaddedLight::index(x, y, z)] = light(p);
            }
        }
    }
    let light = PaddedLight::Full(values.into_boxed_slice().try_into().expect("padded"));
    let options = if walls {
        MeshOptions::default().with_walls()
    } else {
        MeshOptions::default()
    };
    let mut out = ChunkMesh::default();
    Mesher::new().mesh(&padded, Some(&light), table, options, &mut out);
    out
}

fn view(eye: [f64; 3], at: [f64; 3]) -> ViewParams {
    let eye = DVec3::from_array(eye);
    let d = (DVec3::from_array(at) - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    let camera = Camera::default();
    let cv = CameraView::compute(
        &camera,
        &Transform::from_yaw_pitch(eye, yaw, pitch),
        WIDTH as f32 / HEIGHT as f32,
    );
    ViewParams::from_camera(&cv, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
}

/// The eyes: in the cave, straight at the border, and at an angle near a corner.
fn views() -> Vec<ViewParams> {
    vec![
        view([18.5, 15.5, 15.5], [32.0, 15.0, 15.0]),
        view([20.5, 22.5, 9.5], [32.0, 8.0, 22.0]),
    ]
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<RgbaImage, String> {
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_walls"),
        });
        renderer.render(device, queue, &mut encoder, view);
        submit_and_wait(device, queue, encoder)
    })?;
    read_texture_rgba8(device, queue, renderer.main_color())
}

/// Per view: the cave with walls alone, without walls alone, with walls and the chunk across,
/// without walls and the chunk across, and with walls alone again after the chunk across left.
struct Images {
    tier: Tier,
    walls: Vec<RgbaImage>,
    hole: Vec<RgbaImage>,
    across: Vec<RgbaImage>,
    plain_across: Vec<RgbaImage>,
    walls_again: Vec<RgbaImage>,
}

fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    registry: &BlockRegistry,
) -> Result<Images, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        chunk_memory: boxel_render::ChunkMemorySettings {
            initial_quads: 1 << 16,
            initial_slots: 16,
            ..boxel_render::ChunkMemorySettings::default()
        },
        translucent_sort: TranslucentSort::Inline,
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
    let table = MaterialTable::from_registry(registry);
    let walled = Arc::new(chunk_mesh(registry, &table, 0, true));
    let plain = Arc::new(chunk_mesh(registry, &table, 0, false));
    let across = Arc::new(chunk_mesh(registry, &table, 1, false));
    if walled.wall_counts()[3] == 0 {
        return Err("no walls on the +X border".into());
    }
    let views = views();
    let shots = |renderer: &mut VoxelRenderer| -> Result<Vec<RgbaImage>, String> {
        views
            .iter()
            .map(|v| render(&device, &queue, renderer, v))
            .collect()
    };
    let upsert = |renderer: &mut VoxelRenderer, id: u64, cx: i64, mesh: &Arc<ChunkMesh>| {
        renderer
            .upsert_chunk(id, I64Vec3::new(cx * 32, 0, 0), Arc::clone(mesh))
            .map_err(|e| format!("upsert {id}: {e}"))
    };
    upsert(&mut renderer, 1, 0, &walled)?;
    let walls = shots(&mut renderer)?;
    upsert(&mut renderer, 1, 0, &plain)?;
    let hole = shots(&mut renderer)?;
    upsert(&mut renderer, 2, 1, &across)?;
    let plain_across = shots(&mut renderer)?;
    upsert(&mut renderer, 1, 0, &walled)?;
    let with_across = shots(&mut renderer)?;
    if !renderer.remove_chunk(2) {
        return Err("the chunk across was not resident".into());
    }
    let walls_again = shots(&mut renderer)?;
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(Images {
        tier,
        walls,
        hole,
        across: with_across,
        plain_across,
        walls_again,
    })
}

struct Report {
    failures: Vec<String>,
    dir: PathBuf,
}

impl Report {
    /// `a` and `b` differ in at most `max` of their pixels (channel tolerance 2).
    fn close(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage), max: f64) {
        let diff = a.1.compare(b.1, 2);
        let fraction = diff.differing_pixels as f64 / diff.total_pixels as f64;
        if fraction <= max {
            return;
        }
        let message = format!(
            "{what}: {} of {} pixels differ (max channel {})",
            diff.differing_pixels, diff.total_pixels, diff.max_channel_diff
        );
        eprintln!("gpu_walls: FAIL {message}");
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
fn edge_walls_show_only_while_the_chunk_across_is_not_drawn() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let registry = boxel_demo::blocks::registry();
    let mut report = Report {
        failures: Vec::new(),
        dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_walls"),
    };
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let mut tiers = Vec::new();
        for tier in allowed_tiers(&caps) {
            match render_tier(adapter, &caps, tier, &registry) {
                Ok(i) => tiers.push(i),
                Err(e) => report.failures.push(format!("{label} {tier}: {e}")),
            }
        }
        for i in &tiers {
            rendered += 1;
            for v in 0..i.walls.len() {
                let name = |scene: &str| format!("{label}_{}_{scene}_{v}", i.tier);
                // The walls hide the sky through the cave's outer side.
                let diff = i.walls[v].compare(&i.hole[v], 2);
                if (diff.differing_pixels as f64) < 0.05 * diff.total_pixels as f64 {
                    report.failures.push(format!(
                        "{}: the walls hide only {} pixels of the sky",
                        name("walls"),
                        diff.differing_pixels
                    ));
                }
                // The chunk across drawn: its faces, the walls left out.
                report.close(
                    "walls left out",
                    (&name("across"), &i.across[v]),
                    (&name("plain_across"), &i.plain_across[v]),
                    0.0,
                );
                // The walls look like those faces (but for their inset).
                report.close(
                    "walls look like the faces across",
                    (&name("walls"), &i.walls[v]),
                    (&name("plain_across"), &i.plain_across[v]),
                    0.002,
                );
                report.close(
                    "walls again",
                    (&name("walls_again"), &i.walls_again[v]),
                    (&name("walls"), &i.walls[v]),
                    0.0,
                );
            }
        }
        // Every tier as the first (best) one.
        if let Some((best, rest)) = tiers.split_first() {
            for i in rest {
                for v in 0..best.walls.len() {
                    for (scene, a, b) in [
                        ("walls", &best.walls[v], &i.walls[v]),
                        ("across", &best.across[v], &i.across[v]),
                    ] {
                        report.close(
                            &format!("{label} {} vs {} {scene} {v}", i.tier, best.tier),
                            (&format!("{label}_{}_{scene}_{v}", best.tier), a),
                            (&format!("{label}_{}_{scene}_{v}", i.tier), b),
                            MAX_TIER_DIFF,
                        );
                    }
                }
            }
        }
    }
    assert!(report.failures.is_empty(), "{:#?}", report.failures);
    assert!(rendered > 0 || !gpu_tests_required(), "no tier rendered");
}
