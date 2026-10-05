//! Low tier: CPU cull + instanced triangle strips + immediates.
use super::{CpuCull, TierPath, VoxelPipelines};
use crate::chunk_table::ChunkTable;
use crate::renderer::ViewParams;
use boxel_shaders::gpu::ChunkImmediate;

/// No storage buffers in any stage: the quad arena is an instance-step vertex buffer, each
/// visible bucket is `draw(0..4, quad_start..quad_start + n)` as a triangle strip and the chunk
/// origin travels in immediates (one uniform block per pipeline, ADR 0011).
pub(crate) struct LowPath {
    voxel: VoxelPipelines,
    cull: CpuCull,
}

impl LowPath {
    /// Wraps the instanced pipelines.
    pub fn new(voxel: VoxelPipelines) -> Self {
        Self {
            voxel,
            cull: CpuCull::default(),
        }
    }
}

impl TierPath for LowPath {
    fn prepare(&mut self, _queue: &wgpu::Queue, chunks: &ChunkTable, view: &ViewParams) -> u32 {
        self.cull.run(chunks, view);
        self.cull.visible_chunks
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, arena: &wgpu::Buffer) -> u32 {
        if self.cull.opaque.is_empty() && self.cull.cutout.is_empty() {
            return 0;
        }
        pass.set_vertex_buffer(0, arena.slice(..));
        let mut draws = 0;
        for (pipeline, list) in [
            (&self.voxel.opaque, &self.cull.opaque),
            (&self.voxel.cutout, &self.cull.cutout),
        ] {
            if list.is_empty() {
                continue;
            }
            pass.set_pipeline(pipeline);
            // consecutive buckets of one chunk share the immediates
            let mut current = None;
            for d in list {
                if current != Some(d.slot) {
                    let origin = ChunkImmediate {
                        origin: [d.origin.x, d.origin.y, d.origin.z, 0],
                    };
                    pass.set_immediates(0, bytemuck::bytes_of(&origin));
                    current = Some(d.slot);
                }
                pass.draw(0..4, d.quad_start..d.quad_start + d.count);
                draws += 1;
            }
        }
        draws
    }
}
