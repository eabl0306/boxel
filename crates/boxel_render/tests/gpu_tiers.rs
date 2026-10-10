//! Offscreen cross-tier image comparison. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! For every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and every tier a local
//! capability check allows, a real `VoxelRenderer` draws the same scenes offscreen:
//!
//! - a heightfield (4x2x4 chunks, the reference probe's surface function) with a floating slab
//!   and a canopy of leaves (cutout bucket), meshed in the test with `PackedQuad::new`;
//! - a close-up of the canopy from above (mip 0 alpha test: terrain and the inner side of the
//!   leaves show through the holes);
//! - the same heightfield with one chunk removed (exercises zeroed chunk-table slots);
//! - the same picture again after one frame of churn: removes, re-inserts under new ids and
//!   replaced meshes, all uploaded together by the next render (freed slots and arena ranges
//!   are reused within the frame);
//! - the same picture after a lost frame: more churn rendered into a frame with one invalid
//!   command, which wgpu rejects whole (uploads included) and reports while finishing or
//!   submitting it, then a resync of every chunk, as the render plugin does;
//! - the same picture after lost uploads: more churn in a frame whose staging buffer cannot be
//!   created (as when out of memory), which the renderer must report, then a resync;
//! - the heightfield in a second renderer whose arena is full (one entry may stay free: a mesh
//!   takes at least two, a quad and its light palette): a chunk that no longer
//!   fits keeps its old mesh, a smaller mesh reuses the chunk's own range;
//! - a far scene: a 32x32 slab ~480 voxels away and a second slab of another texture 1 voxel
//!   behind it (hidden), compared with the front slab alone (depth precision on every backend,
//!   including GL where reverse-Z loses its precision gain);
//! - the blocks of ADR 0018, meshed by the real `Mesher` over two chunks: a water pool and a
//!   glass box (translucent bucket, blended back to front), bottom and top slabs, cross plants
//!   and logs along X and Z, and the blocks of ADR 0020 (stairs of every kind, a fence run,
//!   torches), compared with the same scene without its translucent quads (the translucent pass
//!   draws) and without its model box table (the box faces draw);
//! - a stone floor textured from a tiny generated resource pack (`TextureSource::Pack`, 32x32
//!   layers): the pack's pixels must show on every tier.
//!
//! Every tier must match the best tier of its adapter within 0.1 % of the pixels. Failing images
//! are saved under `CARGO_TARGET_TMPDIR/gpu_tiers/` (every image with `GPU_TIERS_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, SmokeContext, SmokeLevel, SmokeTest, Tier, floors,
    gpu_tests_required, read_texture_rgba8, run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, IVec3, UVec2};
use boxel_mesh::{
    BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher,
    PackedQuad, TRANSLUCENT_BUCKET, layout,
};
use boxel_render::{
    DEFAULT_SKY_COLOR, RendererConfig, RendererError, RendererSmokeTest, TranslucentSort,
    ViewParams, VoxelRenderer, arena_len,
};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockRegistry, BlockState, ChunkData, Face, LocalPos, RenderShape, neighbor_index,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
/// Pixels may differ between tiers in at most this fraction (channel tolerance 2).
const MAX_TIER_DIFF: f64 = 0.001;
/// Chunk removed in the "minus" variant of the heightfield (holds high terrain and part of the
/// floating slab).
const REMOVED_CHUNK: IVec3 = IVec3::new(0, 1, 0);

// ---------------------------------------------------------------- adapters

/// Headless instance (`InstanceFlags::empty()`, `WGPU_BACKEND` or Vulkan + GL) and its adapters
/// filtered by `WGPU_ADAPTER_NAME` (case-insensitive substring).
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

/// Tiers this adapter can run, from the `[S]` pieces (`classify` is not used here): High =
/// MDI-count + first-instance indirect + compute on Vulkan; Mid = vertex storage (GL: a context
/// that really has SSBOs); Low = immediates. Software adapters are not capped.
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

/// A device shaped exactly like the one selection would create for `tier`, with uncaptured
/// errors collected instead of panicking.
fn device_for(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
) -> Result<(wgpu::Device, wgpu::Queue, Arc<Mutex<Vec<String>>>), String> {
    let request = tier_request(tier, &caps.limits);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu_tiers"),
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

/// Blocks of the test world (`boxel_demo::blocks::registry` ids).
struct Blocks {
    stone: BlockState,
    dirt: BlockState,
    grass: BlockState,
    leaves: BlockState,
}

impl Blocks {
    fn new(registry: &BlockRegistry) -> Self {
        let get = |name: &str| registry.by_name(name).expect(name);
        Self {
            stone: get("boxel:stone"),
            dirt: get("boxel:dirt"),
            grass: get("boxel:grass"),
            leaves: get("boxel:leaves"),
        }
    }
}

/// The reference probe's surface, x and z in `-64..64`, y in `0..64`, plus a floating stone slab
/// (y 60..63) and a two-block canopy of leaves (y 46..48) above part of the terrain.
fn block_at(blocks: &Blocks, p: IVec3) -> BlockState {
    if p.y < 0 || p.y >= 64 || p.x < -64 || p.x >= 64 || p.z < -64 || p.z >= 64 {
        return BlockState::AIR;
    }
    if (60..63).contains(&p.y) && (-20..20).contains(&p.x) && (-10..10).contains(&p.z) {
        return blocks.stone;
    }
    if (46..48).contains(&p.y) && (8..40).contains(&p.x) && (-40..-8).contains(&p.z) {
        return blocks.leaves;
    }
    let (x, z) = (p.x as f32, p.z as f32);
    let h =
        (30.0 + 10.0 * (x * 0.09).sin() * (z * 0.07).cos() + 5.0 * ((x + z) * 0.13).sin()) as i32;
    if p.y < h - 4 {
        blocks.stone
    } else if p.y < h - 1 {
        blocks.dirt
    } else if p.y < h {
        blocks.grass
    } else {
        BlockState::AIR
    }
}

/// Greedy mesher for the test (no `boxel_mesh::Mesher`): per face and slice, merges equal
/// (layer, bucket) cells into `w x h` quads. A face is visible unless the neighbor is opaque or
/// it is the same cutout block. Opaque faces go to their face bucket, cutout faces to bucket 6.
fn mesh_chunk(
    registry: &BlockRegistry,
    origin: IVec3,
    block: &dyn Fn(IVec3) -> BlockState,
) -> ChunkMesh {
    let mut buckets: [Vec<PackedQuad>; BUCKET_COUNT] = Default::default();
    let mut mask: Vec<Option<(u16, usize)>> = vec![None; 32 * 32];
    for face in Face::ALL {
        let (n, u, v) = face.axes();
        for d in 0..32 {
            mask.fill(None);
            for j in 0..32 {
                for i in 0..32 {
                    let mut local = [0; 3];
                    (local[n], local[u], local[v]) = (d, i, j);
                    let p = origin + IVec3::from_array(local);
                    let b = block(p);
                    let shape = registry.shape(b);
                    if !matches!(shape, RenderShape::Opaque | RenderShape::Cutout) {
                        continue;
                    }
                    let neighbor = block(p + face.normal());
                    let hidden = registry.shape(neighbor) == RenderShape::Opaque
                        || (shape == RenderShape::Cutout && neighbor == b);
                    if !hidden {
                        let bucket = if shape == RenderShape::Cutout {
                            CUTOUT_BUCKET
                        } else {
                            face.index()
                        };
                        mask[(j * 32 + i) as usize] =
                            Some((registry.face_texture(b, face).0, bucket));
                    }
                }
            }
            for j in 0..32usize {
                let mut i = 0usize;
                while i < 32 {
                    let Some(key) = mask[j * 32 + i] else {
                        i += 1;
                        continue;
                    };
                    let mut w = 1;
                    while i + w < 32 && mask[j * 32 + i + w] == Some(key) {
                        w += 1;
                    }
                    let mut h = 1;
                    while j + h < 32 && (0..w).all(|k| mask[(j + h) * 32 + i + k] == Some(key)) {
                        h += 1;
                    }
                    for jj in j..j + h {
                        mask[jj * 32 + i..jj * 32 + i + w].fill(None);
                    }
                    let mut pos = [0u32; 3];
                    (pos[n], pos[u], pos[v]) = (d as u32, i as u32, j as u32);
                    buckets[key.1].push(PackedQuad::new(pos, w as u32, h as u32, face, key.0));
                    i += w;
                }
            }
        }
    }
    let counts = buckets.each_ref().map(|quads| quads.len() as u32);
    ChunkMesh::from_quads(buckets.concat(), counts)
}

/// Chunk id used by the tests (any stable u64).
fn chunk_id(c: IVec3) -> u64 {
    let pack = |v: i32| u64::from((v + 512) as u32 & 0x3ff);
    pack(c.x) | pack(c.y) << 10 | pack(c.z) << 20
}

/// Non-empty heightfield chunks: (chunk coordinate, mesh).
fn heightfield(registry: &BlockRegistry) -> Vec<(IVec3, ChunkMesh)> {
    let blocks = Blocks::new(registry);
    let block = |p: IVec3| block_at(&blocks, p);
    let mut chunks = Vec::new();
    for cy in 0..2 {
        for cx in -2..2 {
            for cz in -2..2 {
                let c = IVec3::new(cx, cy, cz);
                let mesh = mesh_chunk(registry, c * 32, &block);
                if !mesh.is_empty() {
                    chunks.push((c, mesh));
                }
            }
        }
    }
    chunks
}

/// The far scene in chunk (0, 0, 0): the front slab is the +Z face of voxels `z = 31` (stone),
/// the back slab a 30x30 snow face one voxel behind it (hidden by the front slab).
fn far_scene(registry: &BlockRegistry, front: bool, back: bool) -> ChunkMesh {
    let layer = |name: &str| {
        registry
            .textures()
            .iter()
            .position(|t| t.name == name)
            .expect(name) as u16
    };
    let mut quads = Vec::new();
    if front {
        quads.push(PackedQuad::new(
            [0, 0, 31],
            32,
            32,
            Face::PosZ,
            layer("boxel:stone"),
        ));
    }
    if back {
        quads.push(PackedQuad::new(
            [1, 1, 30],
            30,
            30,
            Face::PosZ,
            layer("boxel:snow"),
        ));
    }
    let mut bucket_counts = [0; BUCKET_COUNT];
    bucket_counts[Face::PosZ.index()] = quads.len() as u32;
    ChunkMesh::from_quads(quads, bucket_counts)
}

/// `n` copies of the bottom face of voxel (0, 0, 0): never visible from above the chunk (face
/// bucket culled, back face). Used as filler and as a hidden replacement mesh.
fn hidden_mesh(n: u32) -> ChunkMesh {
    let quad = PackedQuad::new([0, 0, 0], 1, 1, Face::NegY, 0);
    let mut bucket_counts = [0; BUCKET_COUNT];
    bucket_counts[Face::NegY.index()] = n;
    ChunkMesh::from_quads(vec![quad; n as usize], bucket_counts)
}

/// The blocks of ADR 0018 on a stone floor across chunks (0, 0, 0) and (1, 0, 0): a water pool
/// in a stone rim, a glass box around a lamp-less stone pillar, bottom and top slabs as steps,
/// short grass and poppies, and logs along X and Z; and flowing water (ADR 0021): a source with
/// its flowing levels around it (sloped tops, `SHAPE_FLUID_SLOPE`) and a source over a falling
/// column; and ADR 0025: a waterlogged slab, stairs and fence in the pool, and lava spread 3
/// blocks around a source; and ADR 0027: a cobblestone wall run with a corner, a post raised by
/// the wall above it, tall sides under a stone block and a gate dropped into it, an open gate
/// with a fence at its end, and a waterlogged wall in the pool. Meshed by the real mesher (no
/// light: open sky). Returns `(chunk, mesh)` for both chunks.
fn special_scene(registry: &BlockRegistry) -> Vec<(IVec3, ChunkMesh)> {
    let get = |name: &str| registry.parse_state(name).expect(name);
    let mut dense = [
        vec![BlockState::AIR; 32 * 32 * 32],
        vec![BlockState::AIR; 32 * 32 * 32],
    ];
    let mut put = |x: i32, y: i32, z: i32, s: BlockState| {
        let chunk = (x >> 5) as usize;
        let local = LocalPos::new((x & 31) as u32, y as u32, z as u32);
        dense[chunk][local.index()] = s;
    };
    for x in 0..64 {
        for z in 0..32 {
            put(x, 0, z, get("boxel:stone"));
        }
    }
    // The pool: a stone rim around 6x6 water, one deep, on the floor.
    for x in 3..11 {
        for z in 3..11 {
            let rim = x == 3 || x == 10 || z == 3 || z == 10;
            put(
                x,
                1,
                z,
                if rim {
                    get("boxel:stone")
                } else {
                    get("boxel:water")
                },
            );
        }
    }
    // ADR 0025: waterlogged blocks in the pool (the stairs' back against the rim).
    put(5, 1, 5, get("boxel:stone_slab[waterlogged=true]"));
    put(
        7,
        1,
        4,
        get("boxel:stone_stairs[facing=north,waterlogged=true]"),
    );
    put(8, 1, 8, get("boxel:oak_fence[waterlogged=true]"));
    put(5, 1, 8, get("boxel:cobblestone_wall[waterlogged=true]"));
    // Lava: a source at (44, 1, 24) and its spread, levels 2, 4 and 6.
    for x in 41..48 {
        for z in 21..28 {
            let d = (x - 44i32).abs() + (z - 24i32).abs();
            if d <= 3 {
                put(x, 1, z, get(&format!("boxel:lava[level={}]", 2 * d)));
            }
        }
    }
    // The glass box: a 4x4x4 shell around a stone pillar, across the chunk border.
    for x in 30..34 {
        for y in 1..5 {
            for z in 5..9 {
                let shell = x == 30 || x == 33 || y == 4 || z == 5 || z == 8;
                if shell {
                    put(x, y, z, get("boxel:glass"));
                } else if (x, z) == (31, 6) {
                    put(x, y, z, get("boxel:planks"));
                }
            }
        }
    }
    // Steps: bottom slabs, then stone, then top slabs over air.
    for z in 14..18 {
        put(14, 1, z, get("boxel:stone_slab"));
        put(15, 1, z, get("boxel:stone"));
        put(16, 2, z, get("boxel:stone_slab[type=top]"));
    }
    // ADR 0020: stairs (straight, corners, upside down), a fence run against a stone block, a
    // torch on the floor and a wall torch on a stone block.
    for (x, name) in [
        (10, "boxel:stone_stairs[facing=east]"),
        (11, "boxel:oak_stairs[facing=east,shape=outer_left]"),
        (12, "boxel:stone_stairs[facing=north,shape=inner_right]"),
        (13, "boxel:oak_stairs[facing=west,half=top]"),
    ] {
        put(x, 1, 26, get(name));
    }
    put(
        13,
        2,
        27,
        get("boxel:stone_stairs[facing=south,half=top,shape=outer_right]"),
    );
    put(15, 1, 24, get("boxel:stone"));
    put(15, 1, 25, get("boxel:oak_fence[north=true,south=true]"));
    put(15, 1, 26, get("boxel:oak_fence[north=true,east=true]"));
    for x in 16..19 {
        put(x, 1, 26, get("boxel:oak_fence[east=true,west=true]"));
    }
    put(19, 1, 26, get("boxel:oak_fence[west=true]"));
    put(17, 1, 28, get("boxel:torch"));
    // ADR 0027: the states the neighbor rules give this wall run (`boxel_voxel::rules`).
    for (x, y, z, name) in [
        (22, 1, 24, "boxel:cobblestone_wall[east=low]"),
        (23, 1, 24, "boxel:cobblestone_wall[east=low,west=low]"),
        (23, 2, 24, "boxel:cobblestone_wall"),
        (24, 1, 24, "boxel:oak_fence_gate[facing=south,in_wall=true]"),
        (
            25,
            1,
            24,
            "boxel:cobblestone_wall[east=tall,west=tall,up=false]",
        ),
        (25, 2, 24, "boxel:stone"),
        (26, 1, 24, "boxel:cobblestone_wall[south=low,west=low]"),
        (26, 1, 25, "boxel:cobblestone_wall[north=low]"),
        (28, 1, 24, "boxel:oak_fence_gate[facing=north,open=true]"),
        (29, 1, 24, "boxel:oak_fence[west=true]"),
    ] {
        put(x, y, z, get(name));
    }
    put(21, 1, 27, get("boxel:stone"));
    put(21, 1, 28, get("boxel:wall_torch[facing=south]"));
    // Plants and logs.
    for (i, x) in (20..28).enumerate() {
        let plant = if i % 3 == 0 {
            "boxel:poppy"
        } else {
            "boxel:short_grass"
        };
        put(x, 1, 14, get(plant));
        put(x, 1, 20, get("boxel:log[axis=x]"));
    }
    for z in 22..28 {
        put(40, 1, z, get("boxel:log[axis=z]"));
    }
    // Flowing water: a source at (35, 1, 16) and the levels of its spread around it (sloped
    // tops, across the chunk border), and a source over a falling column at (46, 1..=5, 12).
    for x in 29..42 {
        for z in 10..23 {
            let d = (x - 35i32).abs() + (z - 16i32).abs();
            if d <= 6 {
                put(x, 1, z, get(&format!("boxel:water[level={d}]")));
            }
        }
    }
    for y in 1..5 {
        put(46, y, 12, get("boxel:water[level=8]"));
    }
    put(46, 5, 12, get("boxel:water"));
    // The walls and gates are what the rules give (air outside the two chunks).
    for (x, y, z) in [
        (22, 1, 24),
        (23, 1, 24),
        (24, 1, 24),
        (25, 1, 24),
        (26, 1, 24),
    ] {
        let block = |x: i32, y: i32, z: i32| {
            let inside = (0..64).contains(&x) && (0..32).contains(&y) && (0..32).contains(&z);
            if !inside {
                return BlockState::AIR;
            }
            let local = LocalPos::new((x & 31) as u32, y as u32, z as u32);
            dense[(x >> 5) as usize][local.index()]
        };
        let state = block(x, y, z);
        let around = Face::ALL.map(|f| {
            let n = f.normal();
            block(x + n.x, y + n.y, z + n.z)
        });
        let rule = registry
            .block(state)
            .and_then(|b| registry.neighbor_rule(b))
            .expect("a rule");
        assert_eq!(
            rule.apply(registry, state, &around),
            state,
            "({x}, {y}, {z})"
        );
    }
    let chunks = dense.map(|d| ChunkData::from_dense(&d));
    let materials = MaterialTable::from_registry(registry);
    let mut mesher = Mesher::new();
    let east = neighbor_index(IVec3::X).expect("face neighbor");
    let west = neighbor_index(IVec3::NEG_X).expect("face neighbor");
    (0..2)
        .map(|i| {
            let mut neighbors = [None; 26];
            neighbors[if i == 0 { east } else { west }] = Some(&chunks[1 - i]);
            let input = MeshInput {
                center: &chunks[i],
                neighbors,
                light: None,
            };
            let mesh = mesher.mesh_chunk(&input, &materials, MeshOptions::default());
            (IVec3::new(i as i32, 0, 0), mesh)
        })
        .collect()
}

/// `mesh` without its translucent quads (the last bucket).
fn without_translucent(mesh: &ChunkMesh) -> ChunkMesh {
    let mut out = mesh.clone();
    let n = out.bucket_counts[TRANSLUCENT_BUCKET] as usize;
    out.quads.truncate(out.quads.len() - n);
    out.bucket_counts[TRANSLUCENT_BUCKET] = 0;
    out.hash = out.compute_hash();
    out
}

/// Looks at the special scene from the south-west, above the pool.
fn special_view() -> ViewParams {
    let eye = DVec3::new(6.0, 14.0, 34.0);
    let d = (DVec3::new(24.0, 1.0, 12.0) - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    view_params(
        &Camera::default(),
        &Transform::from_yaw_pitch(eye, yaw, pitch),
    )
}

fn view_params(camera: &Camera, transform: &Transform) -> ViewParams {
    let view = CameraView::compute(camera, transform, WIDTH as f32 / HEIGHT as f32);
    ViewParams::from_camera(&view, camera, DEFAULT_SKY_COLOR, 0.6)
}

/// Looks over the heightfield from above one corner (the slab and the canopy are in view).
fn heightfield_view() -> ViewParams {
    let eye = DVec3::new(-50.3, 70.6, -48.2);
    let d = (DVec3::new(6.0, 28.0, 8.0) - eye).as_vec3();
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.atan2(d.x.hypot(d.z));
    view_params(
        &Camera::default(),
        &Transform::from_yaw_pitch(eye, yaw, pitch),
    )
}

/// Straight down onto the canopy from 2.5 voxels: about 50 pixels per voxel, so the leaves
/// texture is magnified (mip 0) and its holes are wider than a pixel.
fn canopy_view() -> ViewParams {
    view_params(
        &Camera::default(),
        &Transform::from_yaw_pitch(DVec3::new(24.5, 50.5, -23.5), 0.0, -89f32.to_radians()),
    )
}

/// 480 voxels in front of the front slab, looking down -Z with a 5 degree field of view so the
/// slab fills a third of the image; near 0.1, view distance 1000.
fn far_view() -> ViewParams {
    let camera = Camera {
        fov_y_radians: 5f32.to_radians(),
        near: 0.1,
        view_distance: 1000.0,
        active: true,
    };
    view_params(
        &camera,
        &Transform::from_yaw_pitch(DVec3::new(16.0, 16.0, 32.0 + 480.0), 0.0, 0.0),
    )
}

// ---------------------------------------------------------------- rendering

/// Renders one frame inside error scopes and reads the main color back; also returns the CPU
/// time of `VoxelRenderer::render`.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    view: &ViewParams,
) -> Result<(RgbaImage, Duration), String> {
    let mut cpu = Duration::ZERO;
    run_in_error_scopes(device, || {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_tiers"),
        });
        let start = Instant::now();
        renderer.render(device, queue, &mut encoder, view);
        cpu = start.elapsed();
        submit_and_wait(device, queue, encoder)
    })?;
    let image = read_texture_rgba8(device, queue, renderer.main_color())?;
    Ok((image, cpu))
}

/// Images of one tier.
struct TierImages {
    tier: Tier,
    heightfield: RgbaImage,
    canopy: RgbaImage,
    minus_chunk: RgbaImage,
    /// `minus_chunk` again, after one frame of churn (see [`churn`]).
    churn: RgbaImage,
    /// `minus_chunk` again, after a lost frame and a resync (see [`lost_frame`]).
    lost_frame: RgbaImage,
    /// `minus_chunk` again, after lost uploads and a resync (see [`lost_uploads`]).
    lost_uploads: RgbaImage,
    far_both: RgbaImage,
    far_front: RgbaImage,
    far_back: RgbaImage,
    /// The blocks of ADR 0018 ([`special_scene`]), and without their translucent quads.
    special: RgbaImage,
    special_opaque: RgbaImage,
    /// `special` drawn with an empty model box table (ADR 0020): its box faces vanish.
    special_no_boxes: RgbaImage,
    /// `special` at another time of the fluid animation (ADR 0025): the flow textures moved.
    special_flowing: RgbaImage,
    /// Full arena: after a failed grow of `REMOVED_CHUNK` (expected: `heightfield`), after its
    /// in-place shrink to a hidden quad (`minus_chunk`) and after growing it back
    /// (`heightfield`).
    full_arena: [RgbaImage; 3],
}

/// Renders the heightfield in a renderer whose arena is full (terrain plus out-of-view filler
/// chunks, at most one entry left), then re-upserts `REMOVED_CHUNK`: two quads more than it has
/// (two entries more, while at most one is free) must fail with `ArenaFull` and change nothing;
/// one hidden quad must succeed in the chunk's own range (no other quad fits); its original mesh
/// must fit there again.
fn full_arena_scenes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tier: Tier,
    registry: &BlockRegistry,
    terrain: &[(IVec3, ChunkMesh)],
) -> Result<[RgbaImage; 3], String> {
    let quads: u32 = terrain.iter().map(|(_, m)| arena_len(m)).sum();
    let largest = terrain.iter().map(|(_, m)| arena_len(m)).max();
    let config = RendererConfig {
        // slack for the size bins of `offset_allocator`, filled below
        arena_quads: quads + largest.unwrap_or(0) / 4 + 8,
        max_chunk_slots: 256,
        translucent_sort: TranslucentSort::Inline,
    };
    let mut renderer = VoxelRenderer::new(
        device,
        queue,
        tier,
        &config,
        &boxel_render::generate_block_textures(registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        UVec2::new(WIDTH, HEIGHT),
    )
    .map_err(|e| format!("full arena: VoxelRenderer::new: {e}"))?;
    for (c, mesh) in terrain {
        renderer
            .upsert_chunk(chunk_id(*c), (*c * 32).as_i64vec3(), Arc::new(mesh.clone()))
            .map_err(|e| format!("full arena: upsert {c}: {e}"))?;
    }
    // Fillers behind the camera, the largest that fits first, until no filler fits. A filler of
    // n quads takes n + 1 entries (its light palette), so at most one entry can stay free: no
    // chunk mesh fits in it.
    let mut filler_id = 1 << 40;
    loop {
        let stats = renderer.stats();
        let free = stats.arena_capacity_quads - stats.arena_used_quads;
        let fits = (1..free).rev().any(|n| {
            renderer
                .upsert_chunk(filler_id, I64Vec3::splat(-2048), Arc::new(hidden_mesh(n)))
                .is_ok()
        });
        if !fits {
            break;
        }
        filler_id += 1;
    }
    let full = renderer.stats();
    if full.arena_capacity_quads - full.arena_used_quads > 1 {
        return Err(format!("full arena: not full after the fillers: {full:?}"));
    }

    let (_, original) = terrain
        .iter()
        .find(|(c, _)| *c == REMOVED_CHUNK)
        .ok_or("full arena: no REMOVED_CHUNK")?;
    let (id, origin) = (chunk_id(REMOVED_CHUNK), (REMOVED_CHUNK * 32).as_i64vec3());
    let view = heightfield_view();
    let grown = hidden_mesh(original.total_quads() + 2);
    match renderer.upsert_chunk(id, origin, Arc::new(grown)) {
        Err(RendererError::ArenaFull { .. }) => {}
        other => return Err(format!("full arena: growing a chunk returned {other:?}")),
    }
    let after = renderer.stats();
    if (after.chunks, after.arena_used_quads, after.dropped_chunks)
        != (full.chunks, full.arena_used_quads, full.dropped_chunks + 1)
    {
        return Err(format!(
            "full arena: failed grow changed {full:?} into {after:?}"
        ));
    }
    let (kept, _) = render(device, queue, &mut renderer, &view)?;

    renderer
        .upsert_chunk(id, origin, Arc::new(hidden_mesh(1)))
        .map_err(|e| format!("full arena: shrinking a chunk: {e}"))?;
    let (shrunk, _) = render(device, queue, &mut renderer, &view)?;
    renderer
        .upsert_chunk(id, origin, Arc::new(original.clone()))
        .map_err(|e| format!("full arena: growing a chunk back: {e}"))?;
    let (regrown, _) = render(device, queue, &mut renderer, &view)?;
    let end = renderer.stats();
    if (end.chunks, end.arena_used_quads) != (full.chunks, full.arena_used_quads) {
        return Err(format!(
            "full arena: in-place replaces changed {full:?} into {end:?}"
        ));
    }
    Ok([kept, shrunk, regrown])
}

/// The id of terrain chunk `c` in id generation `generation` (0: `chunk_id`).
fn generation_id(c: IVec3, generation: u64) -> u64 {
    chunk_id(c) | generation << 40
}

/// Changes the heightfield minus `REMOVED_CHUNK` in many ways that leave the same picture, with
/// no render in between: the renderer uploads them all with the next frame. Every terrain chunk
/// is removed and comes back under a new id (from id generation `from` to `to`);
/// `REMOVED_CHUNK` comes back and goes again; one chunk gets a hidden mesh first and its real one
/// after. Freed slots and arena ranges are reused within the frame, so an upload of an older
/// mesh or slot over a newer one shows.
fn churn(
    renderer: &mut VoxelRenderer,
    terrain: &[(IVec3, ChunkMesh)],
    from: u64,
    to: u64,
) -> Result<(), String> {
    let origin = |c: IVec3| (c * 32).as_i64vec3();
    for (c, mesh) in terrain {
        if *c == REMOVED_CHUNK {
            renderer
                .upsert_chunk(chunk_id(*c), origin(*c), Arc::new(mesh.clone()))
                .map_err(|e| format!("churn: re-add {c}: {e}"))?;
            renderer.remove_chunk(chunk_id(*c));
            continue;
        }
        if !renderer.remove_chunk(generation_id(*c, from)) {
            return Err(format!("churn: {c} was not resident"));
        }
        renderer
            .upsert_chunk(generation_id(*c, to), origin(*c), Arc::new(hidden_mesh(3)))
            .map_err(|e| format!("churn: hidden {c}: {e}"))?;
        renderer
            .upsert_chunk(generation_id(*c, to), origin(*c), Arc::new(mesh.clone()))
            .map_err(|e| format!("churn: upsert {c}: {e}"))?;
    }
    if renderer.chunk_count() != terrain.len() - 1 {
        return Err(format!("churn: {} chunks resident", renderer.chunk_count()));
    }
    Ok(())
}

/// The heightfield minus `REMOVED_CHUNK` after a lost frame. More churn (id generation 1 to 2)
/// goes into a frame that also records one invalid copy. wgpu rejects that frame whole, the
/// uploads `render` recorded at its start included, and must say so through the uncaptured
/// error handler (`errors`) while finishing or submitting it: that is how the render plugin knows
/// to resync. Then a resync like the plugin's (every chunk again) and a normal frame.
fn lost_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    terrain: &[(IVec3, ChunkMesh)],
    errors: &Mutex<Vec<String>>,
) -> Result<RgbaImage, String> {
    churn(renderer, terrain, 1, 2)?;
    let view = heightfield_view();
    let buffer = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu_tiers lost frame"),
            size: 16,
            usage,
            mapped_at_creation: false,
        })
    };
    let (src, dst) = (
        buffer(wgpu::BufferUsages::COPY_SRC),
        buffer(wgpu::BufferUsages::COPY_DST),
    );
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu_tiers lost frame"),
    });
    renderer.render(device, queue, &mut encoder, &view);
    // 16 bytes at offset 8 of a 16-byte buffer
    encoder.copy_buffer_to_buffer(&src, 0, &dst, 8, Some(16));
    if renderer.take_lost_uploads() {
        return Err("lost frame: render could not record its uploads".to_string());
    }
    let before = errors.lock().map_err(|e| e.to_string())?.len();
    queue.submit([encoder.finish()]);
    {
        let mut errors = errors.lock().map_err(|e| e.to_string())?;
        if errors.len() == before {
            return Err("lost frame: no error while finishing or submitting it".to_string());
        }
        // expected: the tier fails on any other uncaptured error
        errors.truncate(before);
    }
    resync(renderer, terrain, 3)?;
    let (image, _) = render(device, queue, renderer, &view)?;
    Ok(image)
}

/// Every terrain chunk but `REMOVED_CHUNK` again, under id generation `generation`, after
/// `clear_chunks`: the render plugin's resync.
fn resync(
    renderer: &mut VoxelRenderer,
    terrain: &[(IVec3, ChunkMesh)],
    generation: u64,
) -> Result<(), String> {
    renderer.clear_chunks();
    for (c, mesh) in terrain.iter().filter(|(c, _)| *c != REMOVED_CHUNK) {
        renderer
            .upsert_chunk(
                generation_id(*c, generation),
                (*c * 32).as_i64vec3(),
                Arc::new(mesh.clone()),
            )
            .map_err(|e| format!("resync {c}: {e}"))?;
    }
    Ok(())
}

/// The heightfield minus `REMOVED_CHUNK` after lost uploads. More churn (id generation 3 to 4)
/// goes into a frame whose staging buffer cannot be created (`debug_fail_next_upload`, as when
/// out of memory). wgpu reports that inside `render`, and the frame itself is valid and runs
/// without the uploads, so nothing fails while finishing or submitting it: the renderer must say
/// so itself (`take_lost_uploads`, once), which is how the render plugin knows to resync. Then a
/// resync and a normal frame.
fn lost_uploads(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut VoxelRenderer,
    terrain: &[(IVec3, ChunkMesh)],
    errors: &Mutex<Vec<String>>,
) -> Result<RgbaImage, String> {
    churn(renderer, terrain, 3, 4)?;
    let view = heightfield_view();
    let before = errors.lock().map_err(|e| e.to_string())?.len();
    renderer.debug_fail_next_upload();
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu_tiers lost uploads"),
    });
    renderer.render(device, queue, &mut encoder, &view);
    if !renderer.take_lost_uploads() {
        return Err("lost uploads: not reported by take_lost_uploads".to_string());
    }
    if renderer.take_lost_uploads() {
        return Err("lost uploads: reported twice".to_string());
    }
    {
        let mut errors = errors.lock().map_err(|e| e.to_string())?;
        if errors.len() == before {
            return Err("lost uploads: no error for the staging buffer".to_string());
        }
        // expected: the tier fails on any other uncaptured error
        errors.truncate(before);
    }
    submit_and_wait(device, queue, encoder)?;
    if errors.lock().map_err(|e| e.to_string())?.len() != before {
        return Err("lost uploads: the frame itself failed".to_string());
    }
    resync(renderer, terrain, 5)?;
    let (image, _) = render(device, queue, renderer, &view)?;
    if renderer.take_lost_uploads() {
        return Err("lost uploads: the resync lost its uploads too".to_string());
    }
    Ok(image)
}

/// Uploads and renders every scene on one tier.
fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    registry: &BlockRegistry,
    terrain: &[(IVec3, ChunkMesh)],
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let config = RendererConfig {
        arena_quads: 1 << 20,
        max_chunk_slots: 256,
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
    if renderer.tier() != tier {
        return Err(format!("renderer reports tier {}", renderer.tier()));
    }

    for (c, mesh) in terrain {
        renderer
            .upsert_chunk(chunk_id(*c), (*c * 32).as_i64vec3(), Arc::new(mesh.clone()))
            .map_err(|e| format!("upsert {c}: {e}"))?;
    }
    let quads: u32 = terrain.iter().map(|(_, m)| arena_len(m)).sum();
    let stats = renderer.stats();
    if stats.chunks as usize != terrain.len() || stats.arena_used_quads != quads {
        return Err(format!(
            "stats after upload: {stats:?}, expected {quads} quads"
        ));
    }
    let view = heightfield_view();
    render(&device, &queue, &mut renderer, &view)?; // warm-up
    let (heightfield, cpu) = render(&device, &queue, &mut renderer, &view)?;
    let stats = renderer.stats();
    eprintln!(
        "gpu_tiers: {} {tier}: render {:.3} ms cpu, chunks={} quads={} draws={} visible={} \
         non_background={:.3}",
        adapter_label(caps),
        cpu.as_secs_f64() * 1e3,
        stats.chunks,
        stats.arena_used_quads,
        stats.draws,
        stats.visible_chunks,
        heightfield.non_background_fraction(),
    );

    let (canopy, _) = render(&device, &queue, &mut renderer, &canopy_view())?;

    if !renderer.remove_chunk(chunk_id(REMOVED_CHUNK)) {
        return Err(format!("chunk {REMOVED_CHUNK} was not resident"));
    }
    let (minus_chunk, _) = render(&device, &queue, &mut renderer, &view)?;
    churn(&mut renderer, terrain, 0, 1)?;
    let (churn, _) = render(&device, &queue, &mut renderer, &view)?;
    let lost_frame = lost_frame(&device, &queue, &mut renderer, terrain, &errors)?;
    let lost_uploads = lost_uploads(&device, &queue, &mut renderer, terrain, &errors)?;
    let full_arena = full_arena_scenes(&device, &queue, tier, registry, terrain)?;

    renderer.clear_chunks();
    if renderer.chunk_count() != 0 || renderer.stats().arena_used_quads != 0 {
        return Err(format!("clear_chunks left {:?}", renderer.stats()));
    }
    let far = far_view();
    renderer
        .upsert_chunk(7, I64Vec3::ZERO, Arc::new(far_scene(registry, true, true)))
        .map_err(|e| e.to_string())?;
    let (far_both, _) = render(&device, &queue, &mut renderer, &far)?;
    // replace the same chunk id with one slab at a time
    renderer
        .upsert_chunk(7, I64Vec3::ZERO, Arc::new(far_scene(registry, true, false)))
        .map_err(|e| e.to_string())?;
    let (far_front, _) = render(&device, &queue, &mut renderer, &far)?;
    renderer
        .upsert_chunk(7, I64Vec3::ZERO, Arc::new(far_scene(registry, false, true)))
        .map_err(|e| e.to_string())?;
    let (far_back, _) = render(&device, &queue, &mut renderer, &far)?;

    // an empty mesh removes the chunk: nothing but sky is left
    renderer
        .upsert_chunk(7, I64Vec3::ZERO, Arc::new(ChunkMesh::default()))
        .map_err(|e| e.to_string())?;
    let (empty, _) = render(&device, &queue, &mut renderer, &far)?;
    if renderer.chunk_count() != 0 || empty.non_background_fraction() != 0.0 {
        return Err("an empty mesh did not remove the chunk".to_string());
    }

    let special_meshes = special_scene(registry);
    let sloped = special_meshes
        .iter()
        .flat_map(|(_, mesh)| &mesh.quads)
        .filter(|q| q.shape() == layout::SHAPE_FLUID_SLOPE)
        .count();
    if sloped < 40 {
        return Err(format!("special scene: only {sloped} sloped fluid quads"));
    }
    // The model boxes of the stairs, fences and torches (ADR 0020), walls and gates (ADR 0027).
    let materials = MaterialTable::from_registry(registry);
    renderer.set_model_boxes(&queue, materials.model_boxes());
    let upload = |renderer: &mut VoxelRenderer, opaque_only: bool| {
        for (c, mesh) in &special_meshes {
            let mesh = if opaque_only {
                without_translucent(mesh)
            } else {
                mesh.clone()
            };
            renderer
                .upsert_chunk(100 + chunk_id(*c), (*c * 32).as_i64vec3(), Arc::new(mesh))
                .map_err(|e| e.to_string())?;
        }
        Ok::<_, String>(())
    };
    upload(&mut renderer, false)?;
    let (special, _) = render(&device, &queue, &mut renderer, &special_view())?;
    let draws = renderer.stats().draws;
    upload(&mut renderer, true)?;
    let (special_opaque, _) = render(&device, &queue, &mut renderer, &special_view())?;
    let opaque_draws = renderer.stats().draws;
    // Control: without the box table the box faces collapse, the picture changes.
    upload(&mut renderer, false)?;
    renderer.set_model_boxes(&queue, &[]);
    let (special_no_boxes, _) = render(&device, &queue, &mut renderer, &special_view())?;
    renderer.set_model_boxes(&queue, materials.model_boxes());
    let flowing = ViewParams {
        fluid_time: 1.375,
        ..special_view()
    };
    let (special_flowing, _) = render(&device, &queue, &mut renderer, &flowing)?;
    // Both chunks have translucent quads: one more draw each on every tier.
    if draws != opaque_draws + 2 {
        return Err(format!(
            "special scene: {draws} draws with the translucent buckets, {opaque_draws} without"
        ));
    }

    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(TierImages {
        tier,
        heightfield,
        canopy,
        minus_chunk,
        churn,
        lost_frame,
        lost_uploads,
        far_both,
        far_front,
        far_back,
        special,
        special_opaque,
        special_no_boxes,
        special_flowing,
        full_arena,
    })
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
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_tiers"),
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_tiers: FAIL {message}");
        self.save(images);
        self.failures.push(message);
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
                Ok(()) => eprintln!("gpu_tiers: saved {}", path.display()),
                Err(e) => eprintln!("gpu_tiers: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn check_range(&mut self, what: &str, image: &RgbaImage, range: std::ops::RangeInclusive<f64>) {
        let f = image.non_background_fraction();
        if !range.contains(&f) {
            self.fail(
                format!("{what}: non-background fraction {f:.4} outside {range:?}"),
                &[(what, image)],
            );
        }
    }

    fn check_differs(
        &mut self,
        what: &str,
        a: (&str, &RgbaImage),
        b: (&str, &RgbaImage),
        min_fraction: f64,
    ) {
        let fraction = a.1.compare(b.1, 2).fraction();
        if fraction < min_fraction {
            self.fail(
                format!(
                    "{what}: only {fraction:.4} of the pixels changed (expected >= {min_fraction})"
                ),
                &[a, b],
            );
        }
    }

    fn check_same(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        let diff = a.1.compare(b.1, 2);
        eprintln!(
            "gpu_tiers: {what}: {:.4} % of pixels differ, max channel {}",
            diff.fraction() * 100.0,
            diff.max_channel_diff
        );
        if diff.fraction() > MAX_TIER_DIFF {
            self.fail(
                format!(
                    "{what}: {} of {} pixels differ ({:.4} %, max channel {})",
                    diff.differing_pixels,
                    diff.total_pixels,
                    diff.fraction() * 100.0,
                    diff.max_channel_diff
                ),
                &[(a.0, a.1), (b.0, b.1)],
            );
        }
    }
}

#[test]
fn tiers_render_identically() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let registry = boxel_demo::blocks::registry();
    let terrain = heightfield(&registry);
    assert!(terrain.len() >= 20, "{} terrain chunks", terrain.len());
    assert!(
        terrain
            .iter()
            .any(|(_, m)| m.bucket_counts[CUTOUT_BUCKET] > 0),
        "the canopy must produce cutout quads"
    );
    assert!(terrain.iter().any(|(c, _)| *c == REMOVED_CHUNK));
    let mut report = Report::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let tiers = allowed_tiers(&caps);
        eprintln!("gpu_tiers: {label}: tiers {tiers:?}");
        let mut images = Vec::new();
        for &tier in &tiers {
            match render_tier(adapter, &caps, tier, &registry, &terrain) {
                Ok(i) => images.push(i),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
        }
        for i in &images {
            rendered += 1;
            let name = |scene: &str| format!("{label}_{}_{scene}", i.tier);
            if std::env::var_os("GPU_TIERS_SAVE").is_some() {
                report.save(&[
                    (&name("heightfield"), &i.heightfield),
                    (&name("canopy"), &i.canopy),
                    (&name("minus_chunk"), &i.minus_chunk),
                    (&name("churn"), &i.churn),
                    (&name("lost_frame"), &i.lost_frame),
                    (&name("lost_uploads"), &i.lost_uploads),
                    (&name("far_both"), &i.far_both),
                    (&name("far_front"), &i.far_front),
                    (&name("far_back"), &i.far_back),
                    (&name("special"), &i.special),
                    (&name("special_opaque"), &i.special_opaque),
                    (&name("special_no_boxes"), &i.special_no_boxes),
                    (&name("special_flowing"), &i.special_flowing),
                    (&name("full_arena_kept"), &i.full_arena[0]),
                    (&name("full_arena_shrunk"), &i.full_arena[1]),
                    (&name("full_arena_regrown"), &i.full_arena[2]),
                ]);
            }
            report.check_range(&name("heightfield"), &i.heightfield, 0.05..=0.95);
            report.check_range(&name("far_front"), &i.far_front, 0.1..=0.9);
            report.check_same(
                &format!("{label} {}: far scene, both slabs vs front slab", i.tier),
                (&name("far_both"), &i.far_both),
                (&name("far_front"), &i.far_front),
            );
            // controls: the hidden slab is drawn when nothing covers it, and removing a chunk
            // really changes the picture
            report.check_differs(
                &format!("{label} {}: far scene, back slab alone", i.tier),
                (&name("far_front"), &i.far_front),
                (&name("far_back"), &i.far_back),
                0.1,
            );
            report.check_range(&name("special"), &i.special, 0.3..=1.0);
            report.check_differs(
                &format!(
                    "{label} {}: special scene without its translucent quads",
                    i.tier
                ),
                (&name("special"), &i.special),
                (&name("special_opaque"), &i.special_opaque),
                0.01,
            );
            report.check_differs(
                &format!("{label} {}: special scene without its model boxes", i.tier),
                (&name("special"), &i.special),
                (&name("special_no_boxes"), &i.special_no_boxes),
                0.01,
            );
            report.check_differs(
                &format!(
                    "{label} {}: special scene later in the fluid animation",
                    i.tier
                ),
                (&name("special"), &i.special),
                (&name("special_flowing"), &i.special_flowing),
                // The fluids are small at this size: a few dozen pixels.
                0.0001,
            );
            report.check_differs(
                &format!("{label} {}: heightfield minus one chunk", i.tier),
                (&name("heightfield"), &i.heightfield),
                (&name("minus_chunk"), &i.minus_chunk),
                0.002,
            );
            for (scene, image, expected, expected_name) in [
                ("churn", &i.churn, &i.minus_chunk, "minus_chunk"),
                ("lost_frame", &i.lost_frame, &i.minus_chunk, "minus_chunk"),
                (
                    "lost_uploads",
                    &i.lost_uploads,
                    &i.minus_chunk,
                    "minus_chunk",
                ),
                (
                    "full_arena_kept",
                    &i.full_arena[0],
                    &i.heightfield,
                    "heightfield",
                ),
                (
                    "full_arena_shrunk",
                    &i.full_arena[1],
                    &i.minus_chunk,
                    "minus_chunk",
                ),
                (
                    "full_arena_regrown",
                    &i.full_arena[2],
                    &i.heightfield,
                    "heightfield",
                ),
            ] {
                report.check_same(
                    &format!("{label} {}: {scene} vs {expected_name}", i.tier),
                    (&name(scene), image),
                    (&name(expected_name), expected),
                );
            }
        }
        let Some((best, rest)) = images.split_first() else {
            continue;
        };
        for i in rest {
            for (scene, a, b) in [
                ("heightfield", &best.heightfield, &i.heightfield),
                ("canopy", &best.canopy, &i.canopy),
                ("minus_chunk", &best.minus_chunk, &i.minus_chunk),
                ("far_both", &best.far_both, &i.far_both),
                ("special", &best.special, &i.special),
                ("special_opaque", &best.special_opaque, &i.special_opaque),
                (
                    "special_no_boxes",
                    &best.special_no_boxes,
                    &i.special_no_boxes,
                ),
                ("special_flowing", &best.special_flowing, &i.special_flowing),
            ] {
                report.check_same(
                    &format!("{label}: {} vs {} ({scene})", i.tier, best.tier),
                    (&format!("{label}_{}_{scene}", best.tier), a),
                    (&format!("{label}_{}_{scene}", i.tier), b),
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

#[test]
fn renderer_smoke_test_passes_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let mut failures = Vec::new();
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let (device, queue, errors) = match device_for(adapter, &caps, tier) {
                Ok(d) => d,
                Err(e) => {
                    failures.push(format!("{label} {tier}: {e}"));
                    continue;
                }
            };
            for level in [SmokeLevel::Compile, SmokeLevel::Draw] {
                for output_format in [
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                    wgpu::TextureFormat::Bgra8Unorm,
                ] {
                    let ctx = SmokeContext {
                        device: &device,
                        queue: &queue,
                        tier,
                        caps: &caps,
                        level,
                        output_format,
                    };
                    let start = Instant::now();
                    let result = RendererSmokeTest::default().run(&ctx);
                    eprintln!(
                        "gpu_tiers: smoke {label} {tier} {level:?} {output_format:?}: {} ({:.1} ms)",
                        if result.is_ok() { "ok" } else { "FAILED" },
                        start.elapsed().as_secs_f64() * 1e3
                    );
                    if let Err(e) = result {
                        failures.push(format!("{label} {tier} {level:?} {output_format:?}: {e}"));
                    }
                }
            }
            let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
            if !errors.is_empty() {
                failures.push(format!("{label} {tier}: uncaptured errors {errors:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A quad whose light index is past its chunk's palette (only a malformed mesh has one) is lit
/// as open sky on every tier, like the quad of the same chunk with a valid index: High and Mid
/// do not read the arena after the palette, which may hold another chunk's quads.
#[test]
fn light_indices_past_the_palette_read_open_sky_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let registry = boxel_demo::blocks::registry();
    let stone = registry
        .textures()
        .iter()
        .position(|t| t.name == "boxel:stone")
        .unwrap_or(0) as u16;
    // Two 16x32 +Y quads side by side, no palette (the renderer uploads one open sky entry, and
    // does not debug-assert the indices of a mesh without one): index 0 is the uploaded entry,
    // 4000 is past it.
    let mut counts = [0; BUCKET_COUNT];
    counts[Face::PosY.index()] = 2;
    let mut mesh = ChunkMesh::from_quads(
        vec![
            PackedQuad::new([0, 0, 0], 16, 32, Face::PosY, stone),
            PackedQuad::new([16, 0, 0], 16, 32, Face::PosY, stone).with_light_index(4000),
        ],
        counts,
    );
    mesh.palette.clear();
    mesh.hash = mesh.compute_hash();
    let mesh = Arc::new(mesh);
    // Straight down from 23 voxels: the quads' centers at about x = 115 and x = 205.
    let view = view_params(
        &Camera::default(),
        &Transform::from_yaw_pitch(
            DVec3::new(16.0, 24.0, 16.0),
            0.0,
            -std::f32::consts::FRAC_PI_2,
        ),
    );
    let mut failures = Vec::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let result = (|| {
                let (device, queue, errors) = device_for(adapter, &caps, tier)?;
                let config = RendererConfig {
                    arena_quads: 1 << 13,
                    max_chunk_slots: 8,
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
                renderer
                    .upsert_chunk(0, I64Vec3::ZERO, Arc::clone(&mesh))
                    .map_err(|e| e.to_string())?;
                let (image, _) = render(&device, &queue, &mut renderer, &view)?;
                let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
                if !errors.is_empty() {
                    return Err(format!("uncaptured errors {errors:?}"));
                }
                Ok(image)
            })();
            let image = match result {
                Ok(image) => image,
                Err(e) => {
                    failures.push(format!("{label} {tier}: {e}"));
                    continue;
                }
            };
            rendered += 1;
            let px = |x: u32| {
                let i = ((HEIGHT / 2 * WIDTH + x) * 4) as usize;
                [image.data[i], image.data[i + 1], image.data[i + 2]]
            };
            let (valid, past) = (px(115), px(205));
            let same = valid.iter().zip(past).all(|(a, b)| a.abs_diff(b) <= 2);
            // Stone under a white noon sky: far above the ambient floor.
            if !same || valid.iter().any(|&c| c < 60) {
                failures.push(format!(
                    "{label} {tier}: open sky {valid:?}, index past the palette {past:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        rendered > 0 || !gpu_tests_required(),
        "BOXEL_REQUIRE_GPU=1 but no tier could render"
    );
}

/// Writes a tiny resource pack of generated PNG files (no third-party content) under
/// `CARGO_TARGET_TMPDIR`: a 32x32 stone, red on the left half and blue on the right, plus a
/// 32x32 dirt so 32 is the pack's common size.
fn synthetic_pack() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_tiers_pack");
    let _ = std::fs::remove_dir_all(&dir);
    let blocks = dir.join("assets/minecraft/textures/block");
    std::fs::create_dir_all(&blocks).expect("pack dirs");
    std::fs::write(
        dir.join("pack.mcmeta"),
        r#"{"pack":{"pack_format":34,"description":"boxel gpu test pack"}}"#,
    )
    .expect("pack.mcmeta");
    let png = |name: &str, pixel: &dyn Fn(u32) -> [u8; 4]| {
        RgbaImage {
            width: 32,
            height: 32,
            data: (0..32 * 32).flat_map(|i| pixel(i % 32)).collect(),
        }
        .save_png(&blocks.join(format!("{name}.png")))
        .expect("png");
    };
    png("stone", &|x| {
        if x < 16 {
            [220, 30, 30, 255]
        } else {
            [30, 30, 220, 255]
        }
    });
    png("dirt", &|_| [90, 60, 30, 255]);
    dir
}

/// A pack texture (`TextureSource::Pack`) reaches the screen on every tier: a stone floor
/// magnified under the camera shows the pack's red and blue halves of each voxel instead of the
/// procedural gray, and every tier draws the same image.
#[test]
fn pack_textures_render_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let pack = synthetic_pack();
    let registry = boxel_demo::blocks::registry();
    let (textures, report) = boxel_render::load_block_textures(
        &registry,
        &boxel_render::TextureSource::Pack(
            boxel_render::PackSource::new(pack).with_recipes(boxel_demo::pack::recipes()),
        ),
    )
    .expect("synthetic pack");
    assert_eq!((textures.size, report.from_pack), (32, 2), "{report:?}");
    let stone = registry
        .textures()
        .iter()
        .position(|t| t.name == "boxel:stone")
        .unwrap_or(0) as u16;
    // One 16x16 +Y stone quad (open sky, no palette) seen straight down from 4 voxels above its
    // middle: it fills the image, one voxel is about 30 pixels wide.
    let mut counts = [0; BUCKET_COUNT];
    counts[Face::PosY.index()] = 1;
    let mut mesh = ChunkMesh::from_quads(
        vec![PackedQuad::new([0, 0, 0], 16, 16, Face::PosY, stone)],
        counts,
    );
    mesh.palette.clear();
    mesh.hash = mesh.compute_hash();
    let mesh = Arc::new(mesh);
    let view = view_params(
        &Camera::default(),
        &Transform::from_yaw_pitch(DVec3::new(8.0, 5.0, 8.0), 0.0, -std::f32::consts::FRAC_PI_2),
    );
    let mut failures = Vec::new();
    // (adapter, tier label, image)
    let mut images: Vec<(String, String, RgbaImage)> = Vec::new();
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        for tier in allowed_tiers(&caps) {
            let result = (|| {
                let (device, queue, errors) = device_for(adapter, &caps, tier)?;
                let config = RendererConfig {
                    arena_quads: 1 << 13,
                    max_chunk_slots: 8,
                    translucent_sort: TranslucentSort::Inline,
                };
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
                renderer
                    .upsert_chunk(0, I64Vec3::ZERO, Arc::clone(&mesh))
                    .map_err(|e| e.to_string())?;
                let (image, _) = render(&device, &queue, &mut renderer, &view)?;
                let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
                if !errors.is_empty() {
                    return Err(format!("uncaptured errors {errors:?}"));
                }
                Ok(image)
            })();
            match result {
                Ok(image) => images.push((label.clone(), format!("{label} {tier}"), image)),
                Err(e) => failures.push(format!("{label} {tier}: {e}")),
            }
        }
    }
    for (_, name, image) in &images {
        // Count clearly red and clearly blue pixels: both halves must cover a good share.
        let (mut red, mut blue) = (0, 0);
        for p in image.data.as_chunks::<4>().0 {
            let [r, g, b, _] = p.map(u32::from);
            if r > 3 * g.max(8) && r > 2 * b {
                red += 1;
            } else if b > 3 * g.max(8) && b > 2 * r {
                blue += 1;
            }
        }
        let total = WIDTH * HEIGHT;
        if red < total / 5 || blue < total / 5 {
            failures.push(format!(
                "{name}: {red} red and {blue} blue pixels of {total}"
            ));
        }
        if std::env::var_os("GPU_TIERS_SAVE").is_some() {
            let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_tiers");
            let _ = std::fs::create_dir_all(&dir);
            let file = name.replace([' ', '(', ')'], "_");
            let _ = image.save_png(&dir.join(format!("pack_{file}.png")));
        }
    }
    // Pixel-identical across the tiers of each adapter, as the procedural scenes (same
    // tolerance; the first tier rendered is the adapter's best).
    for (i, (adapter, name, image)) in images.iter().enumerate() {
        // The adapter's first image is the reference; skip it.
        let Some((_, first_name, first)) = images[..i].iter().find(|(a, _, _)| a == adapter) else {
            continue;
        };
        let differing = first
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .zip(image.data.as_chunks::<4>().0)
            .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2))
            .count();
        if differing as f64 > MAX_TIER_DIFF * f64::from(WIDTH * HEIGHT) {
            failures.push(format!(
                "{name}: {differing} pixels differ from {first_name}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        !images.is_empty() || !gpu_tests_required(),
        "BOXEL_REQUIRE_GPU=1 but no tier could render"
    );
}
