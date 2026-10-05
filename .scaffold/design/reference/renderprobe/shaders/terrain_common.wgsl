// boxel/crates/boxel_render/src/shaders/terrain_common.wgsl
// Shared by every terrain tier: bindings common to all tiers, PackedQuad decoding, fragment shader.
//
// PackedQuad (8 bytes) = vec2<u32>(geo, mat):
//   geo: x:5 | y:5 | z:5 | (w-1):5 | (h-1):5 | face:3 | ao_flip:1 | reserved:3
//        x,y,z = min voxel of the quad inside its 32^3 chunk; w spans face_u(face), h spans face_v(face)
//   mat: tex_layer:12 | ao:8 (2 bits per geometric corner 0..3, 3 = unoccluded) | sky:4 | block:4 | flags:4
// face: 0=+Y 1=-Y 2=+X 3=-X 4=+Z 5=-Z (binary-greedy-meshing order: Up, Down, Right, Left, Front, Back)
// Corner c in 0..3 is the geometric corner (c & 1 along u, c >> 1 along v).
// Triangulation: vertex slots [0,1,2, 2,1,3] (indexed list) or a 4-vertex triangle strip.

@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var block_textures: texture_2d_array<f32>;
@group(1) @binding(1) var block_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
    @location(2) shade: f32,
    @location(3) view_dist: f32,
};

struct QuadVertex {
    local_pos: vec3<f32>,
    uv: vec2<f32>,
    layer: u32,
    shade: f32,
};

fn face_normal(f: u32) -> vec3<f32> {
    switch f {
        case 0u: { return vec3<f32>(0.0, 1.0, 0.0); }
        case 1u: { return vec3<f32>(0.0, -1.0, 0.0); }
        case 2u: { return vec3<f32>(1.0, 0.0, 0.0); }
        case 3u: { return vec3<f32>(-1.0, 0.0, 0.0); }
        case 4u: { return vec3<f32>(0.0, 0.0, 1.0); }
        default: { return vec3<f32>(0.0, 0.0, -1.0); }
    }
}

// u axis: X for +-Y and +-Z faces, Z for +-X faces. v axis: Z for +-Y faces, Y otherwise.
fn face_u(f: u32) -> vec3<f32> {
    if f == 2u || f == 3u { return vec3<f32>(0.0, 0.0, 1.0); }
    return vec3<f32>(1.0, 0.0, 0.0);
}
fn face_v(f: u32) -> vec3<f32> {
    if f < 2u { return vec3<f32>(0.0, 0.0, 1.0); }
    return vec3<f32>(0.0, 1.0, 0.0);
}

// Fixed directional face shading (Minecraft-like), M0 has no dynamic lighting.
fn face_shade(f: u32) -> f32 {
    switch f {
        case 0u: { return 1.0; }
        case 1u: { return 0.5; }
        case 2u, 3u: { return 0.6; }
        default: { return 0.8; }
    }
}

fn decode_quad(q: vec2<u32>, slot: u32) -> QuadVertex {
    let geo = q.x;
    let mat = q.y;
    let p = vec3<f32>(f32(geo & 31u), f32((geo >> 5u) & 31u), f32((geo >> 10u) & 31u));
    let w = f32(((geo >> 15u) & 31u) + 1u);
    let h = f32(((geo >> 20u) & 31u) + 1u);
    let f = (geo >> 25u) & 7u;
    let ao_flip = (geo >> 28u) & 1u;
    var c = slot & 3u;
    // ao_flip: rotate the slot->corner mapping so the shared diagonal becomes corners 0-3.
    if ao_flip == 1u {
        c = select(select(select(2u, 0u, c == 2u), 3u, c == 1u), 1u, c == 0u);
    }
    // Keep counter-clockwise winding seen from outside: cross(u, v) points against the normal
    // for faces +Y, +X and -Z (bits 0, 2, 5 of 0x25), so mirror corners 1 and 2 there.
    if ((0x25u >> f) & 1u) == 1u {
        if c == 1u { c = 2u; } else if c == 2u { c = 1u; }
    }
    let cu = f32(c & 1u);
    let cv = f32(c >> 1u);
    let n = face_normal(f);
    var out: QuadVertex;
    out.local_pos = p + max(n, vec3<f32>(0.0)) + face_u(f) * (cu * w) + face_v(f) * (cv * h);
    // Side faces: texture v grows downwards in image space, world v (Y) grows upwards.
    out.uv = vec2<f32>(cu * w, select(cv * h, (1.0 - cv) * h, f >= 2u));
    out.layer = mat & 0xFFFu;
    let ao = f32((mat >> (12u + 2u * c)) & 3u) / 3.0;
    let light = f32(max((mat >> 20u) & 15u, (mat >> 24u) & 15u)) / 15.0;
    out.shade = face_shade(f) * mix(0.5, 1.0, ao) * mix(0.15, 1.0, light);
    return out;
}

fn finish_vertex(qv: QuadVertex, origin: vec3<i32>, lod: u32) -> VsOut {
    let scale = f32(1u << lod);
    let world = vec3<f32>(origin) + qv.local_pos * scale;
    var out: VsOut;
    out.clip = view.view_proj * vec4<f32>(world, 1.0);
    out.uv = qv.uv;
    out.layer = qv.layer;
    out.shade = qv.shade;
    out.view_dist = distance(world, view.camera_pos.xyz);
    return out;
}

@fragment
fn fs_terrain(in: VsOut) -> @location(0) vec4<f32> {
    let albedo = textureSample(block_textures, block_sampler, in.uv, in.layer);
    if albedo.a < 0.5 {
        discard;
    }
    let fog = clamp((in.view_dist - view.fog_start) / max(view.fog_end - view.fog_start, 0.001), 0.0, 1.0);
    return vec4<f32>(mix(albedo.rgb * in.shade, view.fog_color.rgb, fog), 1.0);
}
