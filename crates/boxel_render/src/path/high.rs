//! High tier: GPU cull + `multi_draw_indexed_indirect_count`.
use super::{CullPipeline, TierPath, VoxelPipelines};
use crate::chunk_table::ChunkTable;
use crate::renderer::ViewParams;
use boxel_shaders::gpu::{DrawIndexedIndirect, DrawRef, bindings};

/// Bytes of one indirect draw.
const ARGS_BYTES: u64 = size_of::<DrawIndexedIndirect>() as u64;

/// Buffers the High path needs from the renderer.
pub(crate) struct HighInputs<'a> {
    /// `View` uniform.
    pub view: &'a wgpu::Buffer,
    /// Chunk table (storage).
    pub chunks: &'a wgpu::Buffer,
    /// Group 2 (quad arena + chunk table).
    pub geometry: wgpu::BindGroup,
    /// Shared quad index buffer.
    pub index: wgpu::Buffer,
    /// Opaque list capacity (also the first index of the cutout list).
    pub cap_opaque: u32,
    /// Cutout list capacity.
    pub cap_cutout: u32,
}

/// GPU-driven path: `cull_chunks` writes two compacted indirect lists (opaque face buckets at
/// `[0, cap_opaque)`, cutout buckets at `[cap_opaque, cap_opaque + cap_cutout)`) with matching
/// `DrawRef`s and atomic counts; each list is one `multi_draw_indexed_indirect_count`.
pub(crate) struct HighPath {
    voxel: VoxelPipelines,
    cull: wgpu::ComputePipeline,
    cull_bind_group: wgpu::BindGroup,
    geometry: wgpu::BindGroup,
    index: wgpu::Buffer,
    draw_args: wgpu::Buffer,
    draw_refs: wgpu::Buffer,
    draw_counts: wgpu::Buffer,
    cap_opaque: u32,
    cap_cutout: u32,
}

impl HighPath {
    /// Creates the draw lists and the cull bind group.
    pub fn new(
        device: &wgpu::Device,
        voxel: VoxelPipelines,
        cull: CullPipeline,
        inputs: HighInputs<'_>,
    ) -> Self {
        let draws = u64::from(inputs.cap_opaque + inputs.cap_cutout);
        let draw_args = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel draw args"),
            size: draws * ARGS_BYTES,
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let draw_refs = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel draw refs"),
            size: draws * size_of::<DrawRef>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let draw_counts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel draw counts"),
            size: 8,
            usage: wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cull_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("boxel cull"),
            layout: &cull.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: inputs.view.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: inputs.chunks.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: draw_args.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: draw_refs.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: draw_counts.as_entire_binding(),
                },
            ],
        });
        Self {
            voxel,
            cull: cull.pipeline,
            cull_bind_group,
            geometry: inputs.geometry,
            index: inputs.index,
            draw_args,
            draw_refs,
            draw_counts,
            cap_opaque: inputs.cap_opaque,
            cap_cutout: inputs.cap_cutout,
        }
    }
}

impl TierPath for HighPath {
    fn prepare(&mut self, _queue: &wgpu::Queue, _chunks: &ChunkTable, _view: &ViewParams) -> u32 {
        0
    }

    fn encode_compute(&self, encoder: &mut wgpu::CommandEncoder, slot_count: u32) {
        encoder.clear_buffer(&self.draw_counts, 0, None);
        if slot_count == 0 {
            return;
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("boxel cull"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.cull);
        pass.set_bind_group(bindings::CULL_GROUP, &self.cull_bind_group, &[]);
        pass.dispatch_workgroups(slot_count.div_ceil(bindings::CULL_WORKGROUP_SIZE), 1, 1);
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, _arena: &wgpu::Buffer) -> u32 {
        pass.set_bind_group(bindings::GROUP_GEOMETRY, &self.geometry, &[]);
        pass.set_vertex_buffer(0, self.draw_refs.slice(..));
        pass.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
        pass.set_pipeline(&self.voxel.opaque);
        pass.multi_draw_indexed_indirect_count(
            &self.draw_args,
            0,
            &self.draw_counts,
            0,
            self.cap_opaque,
        );
        pass.set_pipeline(&self.voxel.cutout);
        pass.multi_draw_indexed_indirect_count(
            &self.draw_args,
            u64::from(self.cap_opaque) * ARGS_BYTES,
            &self.draw_counts,
            4,
            self.cap_cutout,
        );
        2
    }
}
