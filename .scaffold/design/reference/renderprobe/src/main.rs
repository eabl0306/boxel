//! Architect probe: validates the boxel M0 render design end to end.
//! - bgm (Mesher<32>) -> canonical PackedQuad conversion, checked against a brute-force oracle
//! - three submission paths (High: GPU cull + MDIC, Mid: CPU cull + direct, Low: instanced + dyn uniform)
//!   using a DrawRef instance-step attribute (no instance_index / draw_index builtins)
//! - offscreen render on every adapter, image comparison across paths
//! - naga validation + GLSL output (330 / ES 300 / 430 / ES 310)
use binary_greedy_meshing as bgm;
use glam::{IVec3, Mat4, Vec3, Vec4};
use std::collections::HashSet;
use std::sync::Arc;
use wgpu::util::DeviceExt;
use wgpu::*;

const CS: usize = 32;
const CSP: usize = 34;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, Debug, PartialEq, Eq)]
struct PackedQuad {
    geo: u32,
    mat: u32,
}

impl PackedQuad {
    #[allow(clippy::too_many_arguments)]
    fn new(pos: [u32; 3], w: u32, h: u32, face: u32, layer: u32, ao: u32, sky: u32, block: u32) -> Self {
        assert!(pos.iter().all(|&c| c < 32) && (1..=32).contains(&w) && (1..=32).contains(&h) && face < 6);
        let geo = pos[0] | pos[1] << 5 | pos[2] << 10 | (w - 1) << 15 | (h - 1) << 20 | face << 25;
        let mat = (layer & 0xFFF) | (ao & 0xFF) << 12 | (sky & 15) << 20 | (block & 15) << 24;
        Self { geo, mat }
    }
    fn pos(self) -> [u32; 3] {
        [self.geo & 31, (self.geo >> 5) & 31, (self.geo >> 10) & 31]
    }
    fn w(self) -> u32 {
        ((self.geo >> 15) & 31) + 1
    }
    fn h(self) -> u32 {
        ((self.geo >> 20) & 31) + 1
    }
    fn face(self) -> u32 {
        (self.geo >> 25) & 7
    }
}

/// canonical (normal axis, u axis, v axis) per face; 0=X 1=Y 2=Z
const FACE_AXES: [(usize, usize, usize); 6] = [(1, 0, 2), (1, 0, 2), (0, 2, 1), (0, 2, 1), (2, 0, 1), (2, 0, 1)];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewUniform {
    view_proj: [[f32; 4]; 4],
    frustum: [[f32; 4]; 6],
    render_origin: [i32; 4],
    camera_rel: [f32; 4],
    fog_color: [f32; 4],
    fog_params: [f32; 4],
    counts: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChunkGpu {
    origin: [i32; 3],
    quad_base: u32,
    counts: [u32; 8],
}

// ---------------------------------------------------------------- world + mesher
fn block_at(p: IVec3) -> u16 {
    if p.y < 0 || p.y >= 96 || p.x < -64 || p.x >= 64 || p.z < -64 || p.z >= 64 {
        return 0;
    }
    let (x, z) = (p.x as f32, p.z as f32);
    let h = (30.0 + 10.0 * (x * 0.09).sin() * (z * 0.07).cos() + 5.0 * ((x + z) * 0.13).sin()) as i32;
    if p.y >= 60 && p.y < 63 && (-20..20).contains(&p.x) && (-10..10).contains(&p.z) {
        return 1; // floating slab: exercises -Y faces and chunk borders
    }
    if p.y < h - 4 {
        1
    } else if p.y < h - 1 {
        2
    } else if p.y < h {
        3
    } else {
        0
    }
}

fn layer_for(block: u16, face: u32) -> u32 {
    match (block, face) {
        (3, 0) => 0,
        (3, 1) => 2,
        (3, _) => 1,
        (2, _) => 2,
        _ => 3,
    }
}

struct ChunkMesh {
    origin: IVec3,
    quads: Vec<PackedQuad>,
    counts: [u32; 8],
}

fn mesh_chunk(cpos: IVec3, mesher: &mut bgm::Mesher<CS>) -> (ChunkMesh, usize) {
    let origin = cpos * 32;
    let mut voxels = vec![0u16; CSP * CSP * CSP];
    for y in 0..CSP {
        for x in 0..CSP {
            for z in 0..CSP {
                let p = origin + IVec3::new(x as i32 - 1, y as i32 - 1, z as i32 - 1);
                voxels[z + x * CSP + y * CSP * CSP] = block_at(p);
            }
        }
    }
    let mut opaque = vec![0u64; CSP * CSP];
    for (i, v) in voxels.iter().enumerate() {
        if *v != 0 {
            opaque[i / CSP] |= 1 << (i % CSP);
        }
    }
    let trans = vec![0u64; CSP * CSP];
    mesher.clear();
    mesher.fast_mesh(&voxels, &opaque, &trans);
    let mut quads = Vec::new();
    let mut counts = [0u32; 8];
    // oracle
    let local = |x: i32, y: i32, z: i32| voxels[(z + 1) as usize + (x + 1) as usize * CSP + (y + 1) as usize * CSP * CSP];
    let mut expected: HashSet<([i32; 3], u32)> = HashSet::new();
    for y in 0..32 {
        for x in 0..32 {
            for z in 0..32 {
                if local(x, y, z) == 0 {
                    continue;
                }
                for (f, d) in [[0, 1, 0], [0, -1, 0], [1, 0, 0], [-1, 0, 0], [0, 0, 1], [0, 0, -1]].iter().enumerate() {
                    if local(x + d[0], y + d[1], z + d[2]) == 0 {
                        expected.insert(([x, y, z], f as u32));
                    }
                }
            }
        }
    }
    let mut got: HashSet<([i32; 3], u32)> = HashSet::new();
    let mut area = 0usize;
    for face in 0..6usize {
        let (na, ua, va) = FACE_AXES[face];
        for q in &mesher.quads[face] {
            let verts = bgm::Face::from(face as u8).vertices_packed(*q);
            let mut mn = [u32::MAX; 3];
            let mut mx = [0u32; 3];
            for v in verts {
                for (a, c) in v.xyz().iter().enumerate() {
                    mn[a] = mn[a].min(*c);
                    mx[a] = mx[a].max(*c);
                }
            }
            let w = mx[ua] - mn[ua];
            let h = mx[va] - mn[va];
            assert_eq!(mx[na], mn[na], "quad not planar");
            let mut pos = mn;
            if face % 2 == 0 {
                pos[na] -= 1;
            }
            let block = q.voxel_id() as u16;
            let pq = PackedQuad::new(pos, w, h, face as u32, layer_for(block, face as u32), 0xFF, 15, 0);
            // expand for oracle
            for i in 0..w {
                for j in 0..h {
                    let mut p = [pos[0] as i32, pos[1] as i32, pos[2] as i32];
                    p[ua] += i as i32;
                    p[va] += j as i32;
                    assert_eq!(local(p[0], p[1], p[2]), block, "quad covers wrong block");
                    assert!(got.insert((p, face as u32)), "overlapping quads");
                    area += 1;
                }
            }
            assert_eq!(pq.pos(), pos);
            assert_eq!((pq.w(), pq.h(), pq.face()), (w, h, face as u32));
            quads.push(pq);
        }
        counts[face] = mesher.quads[face].len() as u32;
    }
    assert_eq!(got, expected, "mesher output != brute-force visible faces");
    (ChunkMesh { origin, quads, counts }, area)
}

// ---------------------------------------------------------------- camera
fn frustum_planes(m: Mat4) -> [[f32; 4]; 6] {
    let r = [m.row(0), m.row(1), m.row(2), m.row(3)];
    // reverse-Z infinite: near plane is z <= w (r3 - r2); far plane is at infinity -> always inside
    let ps = [r[3] + r[0], r[3] - r[0], r[3] + r[1], r[3] - r[1], r[3] - r[2]];
    let mut out = [[0.0, 0.0, 0.0, 1.0]; 6];
    for (i, p) in ps.iter().enumerate() {
        let l = p.truncate().length();
        out[i] = (*p / l).to_array();
    }
    out
}
fn aabb_in_frustum(pl: &[[f32; 4]; 6], mn: Vec3, mx: Vec3) -> bool {
    pl.iter().all(|p| {
        let n = Vec3::new(p[0], p[1], p[2]);
        let v = Vec3::select(n.cmpge(Vec3::ZERO), mx, mn);
        n.dot(v) + p[3] >= 0.0
    })
}
fn bucket_visible(f: usize, mn: Vec3, mx: Vec3, cam: Vec3) -> bool {
    match f {
        0 => cam.y > mn.y,
        1 => cam.y < mx.y,
        2 => cam.x > mn.x,
        3 => cam.x < mx.x,
        4 => cam.z > mn.z,
        _ => cam.z < mx.z,
    }
}

// ---------------------------------------------------------------- textures
fn make_texels() -> Vec<u8> {
    let mut out = Vec::new();
    let mut seed = 12345u32;
    let mut rnd = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 24) as i32
    };
    for layer in 0..4 {
        for y in 0..16 {
            for _x in 0..16 {
                let n = rnd() % 24 - 12;
                let base: [i32; 3] = match layer {
                    0 => [90, 160, 60],
                    1 => if y < 4 { [90, 160, 60] } else { [130, 90, 55] },
                    2 => [130, 90, 55],
                    _ => [128, 128, 128],
                };
                for c in base {
                    out.push((c + n).clamp(0, 255) as u8);
                }
                out.push(255);
            }
        }
    }
    out
}

// ---------------------------------------------------------------- shaders / naga
fn src(files: &[&str]) -> String {
    let mut s = files.iter().map(|f| std::fs::read_to_string(format!("shaders/{f}")).unwrap()).collect::<Vec<_>>().join("\n");
    if std::env::var("FLAT").is_ok() { s = s.replace("return shade_fragment(albedo, i, view);", "return vec4(1.0, 0.0, 0.0, 1.0);").replace("if (albedo.a < 0.5) { discard; }", ""); }
    if std::env::var("NOORIGIN").is_ok() { s = s.replace("finish_vertex(decode_quad(q, vi), chunk.origin.xyz, view)", "finish_vertex(decode_quad(q, vi), vec3<i32>(0), view)"); }
    if std::env::var("PROBEU").is_ok() { s = s.replace("return finish_vertex(decode_quad(q, vi), chunk.origin.xyz, view);", "var o: VsOut; o.clip = vec4(f32(vi & 1u) * 2.0 - 1.0, f32(vi >> 1u) * 2.0 - 1.0, 0.5, 1.0); o.uv = vec2(0.0); o.layer = 0u; o.view_dist = 0.0; o.shade = select(0.0, 1.0, chunk.origin.w == 777); if (bitcast<f32>(chunk.origin.x) == view.view_proj[0][0]) { o.shade = 0.5; } return o;").replace("return shade_fragment(albedo, i, view);", "return vec4(i.shade, 0.0, 0.0, 1.0);").replace("if (albedo.a < 0.5) { discard; }", ""); }
    if std::env::var("BIGTRI").is_ok() { s = s.replace("return finish_vertex(decode_quad(q, vi), chunk.origin.xyz, view);", "var o: VsOut; o.clip = vec4(f32(vi & 1u) * 2.0 - 1.0, f32(vi >> 1u) * 2.0 - 1.0, 0.5, 1.0); o.uv = vec2(0.0); o.layer = 0u; o.shade = 1.0; o.view_dist = 0.0; return o;"); }
    s
}

fn naga_checks() {
    use naga::back::glsl;
    use naga::valid::{Capabilities, ValidationFlags, Validator};
    let sets = [
        ("pull", src(&["common.wgsl", "voxel_pull.wgsl"]), vec![("vs_pull", naga::ShaderStage::Vertex), ("fs_opaque", naga::ShaderStage::Fragment)]),
        ("instanced", src(&["common.wgsl", "voxel_instanced.wgsl"]), vec![("vs_instanced", naga::ShaderStage::Vertex), ("fs_opaque", naga::ShaderStage::Fragment)]),
        ("cull", src(&["common.wgsl", "cull.wgsl"]), vec![("cull_chunks", naga::ShaderStage::Compute)]),
        ("blit", src(&["blit.wgsl"]), vec![("vs_fullscreen", naga::ShaderStage::Vertex), ("fs_blit", naga::ShaderStage::Fragment), ("fs_blit_encode_srgb", naga::ShaderStage::Fragment)]),
    ];
    for (name, s, eps) in sets {
        let m = match naga::front::wgsl::parse_str(&s) {
            Ok(m) => m,
            Err(e) => {
                println!("[naga {name}] PARSE ERROR {}", e.emit_to_string(&s));
                continue;
            }
        };
        let caps = if name == "instanced" { Capabilities::IMMEDIATES } else { Capabilities::empty() };
        let info = match Validator::new(ValidationFlags::all(), caps).validate(&m) {
            Ok(i) => i,
            Err(e) => {
                println!("[naga {name}] VALIDATION ERROR {}", e.emit_to_string(&s));
                continue;
            }
        };
        // forbidden builtins
        for ep in &m.entry_points {
            for arg in &ep.function.arguments {
                if let Some(naga::Binding::BuiltIn(b)) = &arg.binding {
                    let bad = matches!(b, naga::BuiltIn::InstanceIndex | naga::BuiltIn::NumWorkGroups | naga::BuiltIn::DrawIndex | naga::BuiltIn::BaseInstance | naga::BuiltIn::BaseVertex);
                    println!("[naga {name}] ep {} uses builtin {:?}{}", ep.name, b, if bad { "  <-- FORBIDDEN" } else { "" });
                }
            }
        }
        let versions: Vec<glsl::Version> = match name {
            "instanced" | "blit" => vec![glsl::Version::Desktop(330), glsl::Version::Embedded { version: 300, is_webgl: false }],
            _ => vec![glsl::Version::Desktop(430), glsl::Version::Embedded { version: 310, is_webgl: false }, glsl::Version::Desktop(330)],
        };
        for v in versions {
            for (ep, stage) in &eps {
                let opts = glsl::Options { version: v, ..Default::default() };
                let popts = glsl::PipelineOptions { shader_stage: *stage, entry_point: (*ep).into(), multiview: None };
                let mut out = String::new();
                let r = glsl::Writer::new(&mut out, &m, &info, &opts, &popts, naga::proc::BoundsCheckPolicies::default()).and_then(|mut w| w.write());
                println!("[naga {name}] GLSL {v:?} {ep}: {}", match r { Ok(_) => format!("OK ({} B)", out.len()), Err(e) => format!("ERROR {e:?}") });
            }
        }
    }
}

// ---------------------------------------------------------------- gpu
#[derive(Clone, Copy, PartialEq, Debug)]
enum Path {
    High,
    Mid,
    Low,
}


fn main() {
    env_logger::init();
    naga_checks();

    // mesh the scene
    let mut mesher = bgm::Mesher::<CS>::new();
    let mut chunks = Vec::new();
    let mut total_area = 0;
    let t = std::time::Instant::now();
    for cy in 0..3 {
        for cx in -2..2 {
            for cz in -2..2 {
                let (m, a) = mesh_chunk(IVec3::new(cx, cy, cz), &mut mesher);
                total_area += a;
                if !m.quads.is_empty() {
                    chunks.push(m);
                }
            }
        }
    }
    let nquads: usize = chunks.iter().map(|c| c.quads.len()).sum();
    println!("# mesher oracle OK: {} non-empty chunks, {} quads, {} unit faces, {:.1} ms", chunks.len(), nquads, total_area, t.elapsed().as_secs_f64() * 1e3);

    let mut arena: Vec<PackedQuad> = Vec::new();
    let mut table: Vec<ChunkGpu> = Vec::new();
    for c in &chunks {
        let mut counts = c.counts; counts[7] = 1; table.push(ChunkGpu { origin: c.origin.to_array(), quad_base: arena.len() as u32, counts });
        arena.extend_from_slice(&c.quads);
    }

    // camera
    let (w, h) = (640u32, 360u32);
    let cam = glam::DVec3::new(-75.3, 72.6, -71.2);
    let target = glam::DVec3::new(10.0, 25.0, 12.0);
    let render_origin = cam.floor().as_ivec3();
    let cam_rel = (cam - render_origin.as_dvec3()).as_vec3();
    let tgt_rel = (target - render_origin.as_dvec3()).as_vec3();
    let proj = glam::camera::rh::proj::directx::perspective_infinite_reverse(70f32.to_radians(), w as f32 / h as f32, 0.1);
    let viewm = glam::camera::rh::view::look_at_mat4(cam_rel, tgt_rel, Vec3::Y);
    let vp = proj * viewm;
    let planes = frustum_planes(vp);
    let max_draws = (table.len() * 6) as u32;
    let view_u = ViewUniform {
        view_proj: vp.to_cols_array_2d(),
        frustum: planes,
        render_origin: [render_origin.x, render_origin.y, render_origin.z, 0],
        camera_rel: Vec4::from((cam_rel, 0.0)).to_array(),
        fog_color: [0.55, 0.75, 0.95, 1.0],
        fog_params: [120.0, 260.0, 0.0, 0.0],
        counts: [table.len() as u32, max_draws, 0, 0],
    };
    // cpu cull (Mid + reference count for High)
    let mut cpu_refs: Vec<[u32; 2]> = Vec::new();
    let mut cpu_counts: Vec<u32> = Vec::new();
    let mut low_draws: Vec<(u32, u32, u32)> = Vec::new(); // (slot, first_quad, n)
    for (slot, c) in table.iter().enumerate() {
        let mn = (IVec3::from_array(c.origin) - render_origin).as_vec3();
        let mx = mn + Vec3::splat(32.0);
        if !aabb_in_frustum(&planes, mn, mx) {
            continue;
        }
        let mut start = c.quad_base;
        for f in 0..6 {
            let n = c.counts[f];
            if n > 0 && bucket_visible(f, mn, mx, cam_rel) {
                cpu_refs.push([slot as u32, start]);
                cpu_counts.push(n);
                low_draws.push((slot as u32, start, n));
            }
            start += n;
        }
    }
    println!("# scene: {} chunk slots, arena {} quads, cpu-visible draws {}", table.len(), arena.len(), cpu_refs.len());

    let mut flags = InstanceFlags::empty();
    if std::env::var("VALIDATE").is_ok() {
        flags = InstanceFlags::debugging() - InstanceFlags::VALIDATION_INDIRECT_CALL;
    }
    let mut desc = InstanceDescriptor::new_without_display_handle();
    desc.backends = Backends::VULKAN | Backends::GL;
    if let Ok(b) = std::env::var("WGPU_BACKEND") {
        let parsed = Backends::from_comma_list(&b);
        if !parsed.is_empty() {
            desc.backends = parsed;
        }
    }
    desc.flags = flags;
    let instance = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Instance::new(desc))).expect("instance");
    let adapters = pollster::block_on(instance.enumerate_adapters(Backends::all()));
    let texels = make_texels();
    let mut idx: Vec<u32> = Vec::with_capacity(16384 * 6);
    for q in 0..16384u32 {
        idx.extend_from_slice(&[4 * q, 4 * q + 1, 4 * q + 2, 4 * q + 2, 4 * q + 1, 4 * q + 3]);
    }
    let filter = std::env::var("ADAPTER").unwrap_or_default().to_lowercase();
    for adapter in adapters {
        let info = adapter.get_info();
        if !format!("{} {:?}", info.name, info.backend).to_lowercase().contains(&filter) {
            continue;
        }
        let feats = adapter.features();
        let dl = adapter.get_downlevel_capabilities();
        let lim = adapter.limits();
        println!("\n=== {} ({:?}, {:?}) driver_info={:?}", info.name, info.backend, info.device_type, info.driver_info);
        let want = feats & (Features::MULTI_DRAW_INDIRECT_COUNT | Features::INDIRECT_FIRST_INSTANCE | Features::IMMEDIATES);
        let dev = pollster::block_on(adapter.request_device(&DeviceDescriptor { label: Some("probe"), required_features: want, required_limits: lim.clone(), ..Default::default() }));
        let (device, queue) = match dev {
            Ok(d) => d,
            Err(e) => {
                println!("  request_device ERR {e}");
                continue;
            }
        };
        device.on_uncaptured_error(Arc::new(|e: Error| println!("  !! uncaptured: {}", e.to_string().replace('\n', " ").chars().take(300).collect::<String>())));
        device.set_device_lost_callback(|r, m| println!("  !! device lost {r:?}: {m}"));
        let mut paths = vec![Path::Low];
        if dl.flags.contains(DownlevelFlags::VERTEX_STORAGE) && lim.max_storage_buffers_per_shader_stage >= 4 {
            paths.insert(0, Path::Mid);
        }
        if want.contains(Features::MULTI_DRAW_INDIRECT_COUNT | Features::INDIRECT_FIRST_INSTANCE) && dl.flags.contains(DownlevelFlags::COMPUTE_SHADERS | DownlevelFlags::INDIRECT_EXECUTION) {
            paths.insert(0, Path::High);
        }
        if let Ok(only) = std::env::var("PATHS") {
            paths.retain(|p| only.to_lowercase().contains(&format!("{p:?}").to_lowercase()));
        }

        // shared resources
        let tex = device.create_texture_with_data(&queue, &TextureDescriptor { label: Some("blocks"), size: Extent3d { width: 16, height: 16, depth_or_array_layers: 4 }, mip_level_count: 1, sample_count: 1, dimension: TextureDimension::D2, format: TextureFormat::Rgba8UnormSrgb, usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST, view_formats: &[] }, util::TextureDataOrder::LayerMajor, &texels);
        let tex_view = tex.create_view(&TextureViewDescriptor { dimension: Some(TextureViewDimension::D2Array), ..Default::default() });
        let sampler = device.create_sampler(&SamplerDescriptor { address_mode_u: AddressMode::Repeat, address_mode_v: AddressMode::Repeat, ..Default::default() });
        let bgl_mat = device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: Some("materials"), entries: &[
            BindGroupLayoutEntry { binding: 0, visibility: ShaderStages::FRAGMENT, ty: BindingType::Texture { sample_type: TextureSampleType::Float { filterable: true }, view_dimension: TextureViewDimension::D2Array, multisampled: false }, count: None },
            BindGroupLayoutEntry { binding: 1, visibility: ShaderStages::FRAGMENT, ty: BindingType::Sampler(SamplerBindingType::Filtering), count: None },
        ] });
        let bg_mat = device.create_bind_group(&BindGroupDescriptor { label: Some("materials"), layout: &bgl_mat, entries: &[BindGroupEntry { binding: 0, resource: BindingResource::TextureView(&tex_view) }, BindGroupEntry { binding: 1, resource: BindingResource::Sampler(&sampler) }] });
        let view_buf = device.create_buffer_init(&util::BufferInitDescriptor { label: Some("view"), contents: bytemuck::bytes_of(&view_u), usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST });
        let bgl_view = device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: Some("view"), entries: &[BindGroupLayoutEntry { binding: 0, visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT, ty: BindingType::Buffer { ty: BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None }] });
        let bg_view = device.create_bind_group(&BindGroupDescriptor { label: Some("view"), layout: &bgl_view, entries: &[BindGroupEntry { binding: 0, resource: view_buf.as_entire_binding() }] });
        let arena_buf = device.create_buffer_init(&util::BufferInitDescriptor { label: Some("arena"), contents: bytemuck::cast_slice(&arena), usage: BufferUsages::STORAGE | BufferUsages::VERTEX | BufferUsages::COPY_DST });
        let idx_buf = device.create_buffer_init(&util::BufferInitDescriptor { label: Some("quad indices"), contents: bytemuck::cast_slice(&idx), usage: BufferUsages::INDEX });

        let color_tex = device.create_texture(&TextureDescriptor { label: Some("color"), size: Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: TextureDimension::D2, format: TextureFormat::Rgba8UnormSrgb, usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC, view_formats: &[] });
        let depth_tex = device.create_texture(&TextureDescriptor { label: Some("depth"), size: Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: TextureDimension::D2, format: TextureFormat::Depth32Float, usage: TextureUsages::RENDER_ATTACHMENT, view_formats: &[] });
        let color = color_tex.create_view(&Default::default());
        let depth = depth_tex.create_view(&Default::default());
        let readback = device.create_buffer(&BufferDescriptor { label: Some("readback"), size: (w * h * 4) as u64, usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST, mapped_at_creation: false });

        let mut images: Vec<(Path, Vec<u8>)> = Vec::new();
        for path in paths {
            let v = device.push_error_scope(ErrorFilter::Validation);
            let i = device.push_error_scope(ErrorFilter::Internal);
            let o = device.push_error_scope(ErrorFilter::OutOfMemory);
            let mut enc = device.create_command_encoder(&Default::default());
            let mut gpu_count_buf = None;
            let depth_state = Some(DepthStencilState { format: TextureFormat::Depth32Float, depth_write_enabled: Some(true), depth_compare: Some(if std::env::var("DEPTH_ALWAYS").is_ok() { CompareFunction::Always } else { CompareFunction::Greater }), stencil: Default::default(), bias: Default::default() });
            let prim = |topo: PrimitiveTopology| PrimitiveState { topology: topo, front_face: FrontFace::Ccw, cull_mode: if std::env::var("NOCULL").is_ok() { None } else { Some(Face::Back) }, ..Default::default() };
            let targets = [Some(ColorTargetState { format: TextureFormat::Rgba8UnormSrgb, blend: None, write_mask: ColorWrites::ALL })];
            match path {
                Path::High | Path::Mid => {
                    let sm = device.create_shader_module(ShaderModuleDescriptor { label: Some("pull"), source: ShaderSource::Wgsl(src(&["common.wgsl", "voxel_pull.wgsl"]).into()) });
                    let bgl_geo = device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: Some("geometry"), entries: &[
                        BindGroupLayoutEntry { binding: 0, visibility: ShaderStages::VERTEX, ty: BindingType::Buffer { ty: BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
                        BindGroupLayoutEntry { binding: 1, visibility: ShaderStages::VERTEX, ty: BindingType::Buffer { ty: BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
                    ] });
                    let table_buf = device.create_buffer_init(&util::BufferInitDescriptor { label: Some("chunk table"), contents: bytemuck::cast_slice(&table), usage: BufferUsages::STORAGE | BufferUsages::COPY_DST });
                    let bg_geo = device.create_bind_group(&BindGroupDescriptor { label: Some("geometry"), layout: &bgl_geo, entries: &[BindGroupEntry { binding: 0, resource: arena_buf.as_entire_binding() }, BindGroupEntry { binding: 1, resource: table_buf.as_entire_binding() }] });
                    let pl = device.create_pipeline_layout(&PipelineLayoutDescriptor { label: Some("pull"), bind_group_layouts: &[Some(&bgl_mat), Some(&bgl_view), Some(&bgl_geo)], immediate_size: 0 });
                    let attrs = [VertexAttribute { format: VertexFormat::Uint32x2, offset: 0, shader_location: 0 }];
                    let rp = device.create_render_pipeline(&RenderPipelineDescriptor { label: Some("pull"), layout: Some(&pl),
                        vertex: VertexState { module: &sm, entry_point: Some("vs_pull"), compilation_options: Default::default(), buffers: &[Some(VertexBufferLayout { array_stride: 8, step_mode: VertexStepMode::Instance, attributes: &attrs })] },
                        primitive: prim(PrimitiveTopology::TriangleList), depth_stencil: depth_state.clone(), multisample: Default::default(),
                        fragment: Some(FragmentState { module: &sm, entry_point: Some("fs_opaque"), compilation_options: Default::default(), targets: &targets }), multiview_mask: None, cache: None });
                    let refs_buf = device.create_buffer(&BufferDescriptor { label: Some("draw refs"), size: (max_draws as u64).max(1) * 8, usage: BufferUsages::VERTEX | BufferUsages::STORAGE | BufferUsages::COPY_DST, mapped_at_creation: false });
                    let args_buf = device.create_buffer(&BufferDescriptor { label: Some("draw args"), size: (max_draws as u64).max(1) * 20, usage: BufferUsages::INDIRECT | BufferUsages::STORAGE, mapped_at_creation: false });
                    let count_buf = device.create_buffer(&BufferDescriptor { label: Some("draw count"), size: 4, usage: BufferUsages::INDIRECT | BufferUsages::STORAGE | BufferUsages::COPY_DST | BufferUsages::COPY_SRC, mapped_at_creation: false });
                    if path == Path::High {
                        let cm = device.create_shader_module(ShaderModuleDescriptor { label: Some("cull"), source: ShaderSource::Wgsl(src(&["common.wgsl", "cull.wgsl"]).into()) });
                        let st = |b: u32, ro: bool| BindGroupLayoutEntry { binding: b, visibility: ShaderStages::COMPUTE, ty: BindingType::Buffer { ty: BufferBindingType::Storage { read_only: ro }, has_dynamic_offset: false, min_binding_size: None }, count: None };
                        let bgl_cull = device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: Some("cull"), entries: &[
                            BindGroupLayoutEntry { binding: 0, visibility: ShaderStages::COMPUTE, ty: BindingType::Buffer { ty: BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
                            st(1, true), st(2, false), st(3, false), st(4, false)] });
                        let cpl = device.create_pipeline_layout(&PipelineLayoutDescriptor { label: Some("cull"), bind_group_layouts: &[Some(&bgl_cull)], immediate_size: 0 });
                        let cp = device.create_compute_pipeline(&ComputePipelineDescriptor { label: Some("cull"), layout: Some(&cpl), module: &cm, entry_point: Some("cull_chunks"), compilation_options: Default::default(), cache: None });
                        let bg_cull = device.create_bind_group(&BindGroupDescriptor { label: Some("cull"), layout: &bgl_cull, entries: &[
                            BindGroupEntry { binding: 0, resource: view_buf.as_entire_binding() }, BindGroupEntry { binding: 1, resource: table_buf.as_entire_binding() },
                            BindGroupEntry { binding: 2, resource: args_buf.as_entire_binding() }, BindGroupEntry { binding: 3, resource: refs_buf.as_entire_binding() },
                            BindGroupEntry { binding: 4, resource: count_buf.as_entire_binding() }] });
                        enc.clear_buffer(&count_buf, 0, None);
                        let mut c = enc.begin_compute_pass(&ComputePassDescriptor { label: Some("cull"), timestamp_writes: None });
                        c.set_pipeline(&cp);
                        c.set_bind_group(0, &bg_cull, &[]);
                        c.dispatch_workgroups((table.len() as u32).div_ceil(64), 1, 1);
                    } else {
                        queue.write_buffer(&refs_buf, 0, bytemuck::cast_slice(&cpu_refs));
                    }
                    {
                        let mut p = begin(&mut enc, &color, &depth);
                        p.set_pipeline(&rp);
                        p.set_bind_group(0, &bg_mat, &[]);
                        p.set_bind_group(1, &bg_view, &[]);
                        p.set_bind_group(2, &bg_geo, &[]);
                        p.set_vertex_buffer(0, refs_buf.slice(..));
                        p.set_index_buffer(idx_buf.slice(..), IndexFormat::Uint32);
                        if path == Path::High {
                            p.multi_draw_indexed_indirect_count(&args_buf, 0, &count_buf, 0, max_draws);
                        } else {
                            for (i, n) in cpu_counts.iter().enumerate() {
                                p.draw_indexed(0..n * 6, 0, i as u32..i as u32 + 1);
                            }
                        }
                    }
                    if path == Path::High {
                        let rb = device.create_buffer(&BufferDescriptor { label: None, size: 4, usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST, mapped_at_creation: false });
                        enc.copy_buffer_to_buffer(&count_buf, 0, &rb, 0, 4);
                        gpu_count_buf = Some(rb);
                    }
                }
                Path::Low => {
                    let sm = device.create_shader_module(ShaderModuleDescriptor { label: Some("instanced"), source: ShaderSource::Wgsl(src(&["common.wgsl", "voxel_instanced.wgsl"]).into()) });
                    let bgl_chunk = device.create_bind_group_layout(&BindGroupLayoutDescriptor { label: Some("chunk"), entries: &[BindGroupLayoutEntry { binding: 0, visibility: ShaderStages::VERTEX, ty: BindingType::Buffer { ty: BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: BufferSize::new(16) }, count: None }] });
                    let stride = 256u64.max(lim.min_uniform_buffer_offset_alignment as u64);
                    let mut cu = vec![0u8; (table.len() as u64 * stride) as usize];
                    for (s, c) in table.iter().enumerate() {
                        let o = s * stride as usize;
                        cu[o..o + 16].copy_from_slice(bytemuck::cast_slice(&[c.origin[0], c.origin[1], c.origin[2], 777]));
                    }
                    let cu_buf = device.create_buffer_init(&util::BufferInitDescriptor { label: Some("chunk uniforms"), contents: &cu, usage: BufferUsages::UNIFORM });
                    let bg_chunk = device.create_bind_group(&BindGroupDescriptor { label: Some("chunk"), layout: &bgl_chunk, entries: &[BindGroupEntry { binding: 0, resource: BindingResource::Buffer(BufferBinding { buffer: &cu_buf, offset: 0, size: BufferSize::new(16) }) }] });
                    let pl = device.create_pipeline_layout(&PipelineLayoutDescriptor { label: Some("instanced"), bind_group_layouts: &[Some(&bgl_mat), Some(&bgl_view)], immediate_size: 16 });
                    let _ = (&bg_chunk, &bgl_chunk);
                    let attrs = [VertexAttribute { format: VertexFormat::Uint32x2, offset: 0, shader_location: 0 }];
                    let rp = device.create_render_pipeline(&RenderPipelineDescriptor { label: Some("instanced"), layout: Some(&pl),
                        vertex: VertexState { module: &sm, entry_point: Some("vs_instanced"), compilation_options: Default::default(), buffers: &[Some(VertexBufferLayout { array_stride: 8, step_mode: VertexStepMode::Instance, attributes: &attrs })] },
                        primitive: prim(PrimitiveTopology::TriangleStrip), depth_stencil: depth_state.clone(), multisample: Default::default(),
                        fragment: Some(FragmentState { module: &sm, entry_point: Some("fs_opaque"), compilation_options: Default::default(), targets: &targets }), multiview_mask: None, cache: None });
                    let mut p = begin(&mut enc, &color, &depth);
                    p.set_pipeline(&rp);
                    p.set_bind_group(0, &bg_mat, &[]);
                    p.set_bind_group(1, &bg_view, &[]);
                    p.set_vertex_buffer(0, arena_buf.slice(..));
                    for (slot, first, n) in &low_draws {
                        let o = table[*slot as usize].origin;
                        p.set_immediates(0, bytemuck::cast_slice(&[o[0], o[1], o[2], 0]));
                        if std::env::var("FIRST0").is_ok() { p.set_vertex_buffer(0, arena_buf.slice(*first as u64 * 8..)); p.draw(0..4, 0..*n); } else { p.draw(0..4, *first..*first + *n); }
                    }
                }
            }
            enc.copy_texture_to_buffer(TexelCopyTextureInfo { texture: &color_tex, mip_level: 0, origin: Origin3d::ZERO, aspect: TextureAspect::All },
                TexelCopyBufferInfo { buffer: &readback, layout: TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) } }, Extent3d { width: w, height: h, depth_or_array_layers: 1 });
            let t0 = std::time::Instant::now();
            queue.submit([enc.finish()]);
            let eo = pollster::block_on(o.pop());
            let ei = pollster::block_on(i.pop());
            let ev = pollster::block_on(v.pop());
            if ev.is_some() || ei.is_some() || eo.is_some() {
                println!("  [{path:?}] ERROR validation={:?} internal={:?} oom={:?}", ev.map(|e| e.to_string().replace('\n', " ").chars().take(400).collect::<String>()), ei.map(|e| e.to_string().replace('\n', " ").chars().take(400).collect::<String>()), eo.is_some());
                continue;
            }
            let s = readback.slice(..);
            s.map_async(MapMode::Read, |_| {});
            device.poll(PollType::wait_indefinitely()).unwrap();
            let data = s.get_mapped_range().unwrap().to_vec();
            readback.unmap();
            let mut gpu_count = String::new();
            if let Some(rb) = gpu_count_buf {
                let s = rb.slice(..);
                s.map_async(MapMode::Read, |_| {});
                device.poll(PollType::wait_indefinitely()).unwrap();
                gpu_count = format!(" gpu_draws={} (cpu {})", u32::from_le_bytes(s.get_mapped_range().unwrap()[..4].try_into().unwrap()), cpu_refs.len());
            }
            let sky = data.chunks(4).filter(|p| p[0] == data[0] && p[1] == data[1] && p[2] == data[2]).count();
            println!("  [{path:?}] OK {:.1} ms, non-clear px {:.1}%{gpu_count} px0={:?} pxmid={:?}", t0.elapsed().as_secs_f64() * 1e3, 100.0 * (1.0 - sky as f64 / (w * h) as f64), &data[0..4], &data[((h/2*w + w/2)*4) as usize..((h/2*w + w/2)*4 + 4) as usize]);
            let fname = format!("out_{}_{:?}_{:?}.png", info.name.split_whitespace().take(2).collect::<Vec<_>>().join("_").replace(['(', ')', '/'], ""), info.backend, path);
            let file = std::fs::File::create(&fname).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&data).unwrap();
            images.push((path, data));
        }
        for k in 1..images.len() {
            let (a, b) = (&images[0], &images[k]);
            let mut bad = 0usize;
            let mut maxd = 0u8;
            for (pa, pb) in a.1.chunks(4).zip(b.1.chunks(4)) {
                let d = pa.iter().zip(pb).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
                maxd = maxd.max(d);
                if d > 2 {
                    bad += 1;
                }
            }
            println!("  compare {:?} vs {:?}: {} px differ (>2) = {:.3}%, max diff {}", a.0, b.0, bad, 100.0 * bad as f64 / (w * h) as f64, maxd);
        }
    }
}

fn begin<'a>(enc: &'a mut CommandEncoder, color: &'a TextureView, depth: &'a TextureView) -> RenderPass<'a> {
    enc.begin_render_pass(&RenderPassDescriptor {
        label: Some("main"),
        color_attachments: &[Some(RenderPassColorAttachment { view: color, depth_slice: None, resolve_target: None, ops: Operations { load: LoadOp::Clear(Color { r: 0.55, g: 0.75, b: 0.95, a: 1.0 }), store: StoreOp::Store } })],
        depth_stencil_attachment: Some(RenderPassDepthStencilAttachment { view: depth, depth_ops: Some(Operations { load: LoadOp::Clear(0.0), store: StoreOp::Store }), stencil_ops: None }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}
