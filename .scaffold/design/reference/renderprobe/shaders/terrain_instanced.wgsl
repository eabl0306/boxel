// boxel/crates/boxel_render/src/shaders/terrain_instanced.wgsl
// Low tier: no storage buffers anywhere (NVIDIA desktop GL 3.3, GLES 3.0 class devices).
// The quad arena is bound as an instance-step vertex buffer (Uint32x2, stride 8); one instance = one quad.
// Per draw: set_immediates(0, DrawChunk), set_vertex_buffer(0, arena.slice(first*8 .. (first+n)*8)),
//           draw(0..4, 0..n) as a 4-vertex triangle strip.
// The chunk origin travels as an immediate (Features::IMMEDIATES, 16 bytes). A dynamic-offset uniform
// was measured to render nothing on NVIDIA GL 3.3 + wgpu 30.0.1 (see ADR 0006), immediates work there.

struct DrawChunk {
    origin: vec3<i32>,
    lod: u32,
};

var<immediate> draw_chunk: DrawChunk;

struct InstancedIn {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) quad: vec2<u32>,
};

@vertex
fn vs_instanced(in: InstancedIn) -> VsOut {
    return finish_vertex(decode_quad(in.quad, in.vertex_index), draw_chunk.origin, draw_chunk.lod);
}
