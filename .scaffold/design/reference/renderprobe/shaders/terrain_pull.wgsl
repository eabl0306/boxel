// boxel/crates/boxel_render/src/shaders/terrain_pull.wgsl
// High and Mid tiers: vertex pulling from storage buffers (needs DownlevelFlags::VERTEX_STORAGE).
//
// Draw identity never comes from builtin offsets. Each draw reads ONE element of the instance-step
// vertex buffer `draw_meta` (location 0), selected by first_instance:
//   High: multi_draw_indexed_indirect_count, args = (6n, 1, 0, 0, s), draw_meta[s] written by cs_cull
//   Mid:  draw_indexed(0..6n, 0, s..s+1), draw_meta[s] written by the CPU each frame
// vertex_index is always local to the draw (first_index = 0, base_vertex = 0) and is read from the
// shared static index buffer [4i+0, 4i+1, 4i+2, 4i+2, 4i+1, 4i+3]. So no backend-specific builtin
// semantics are involved (DX12 count path, GL first_instance emulation, missing draw_index).

@group(0) @binding(1) var<storage, read> chunks: array<ChunkGpu>;
@group(0) @binding(2) var<storage, read> quads: array<vec2<u32>>;

struct PullIn {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) draw: vec2<u32>,      // x = absolute index of the first quad, y = chunk slot
};

@vertex
fn vs_pull(in: PullIn) -> VsOut {
    let ch = chunks[in.draw.y];
    let q = quads[in.draw.x + (in.vertex_index >> 2u)];
    return finish_vertex(decode_quad(q, in.vertex_index & 3u), ch.origin, ch.lod);
}
