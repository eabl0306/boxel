//! Mid tier: CPU cull + `draw_indexed` per visible bucket with `DrawRef`.
use super::{BucketDraw, CpuCull, SlotVisibility, TierPath, VoxelPipelines};
use crate::chunk_table::ChunkTable;
use crate::renderer::ViewParams;
use boxel_shaders::gpu::{DrawRef, bindings};

/// Vertex pulling with CPU culling: the frame's `DrawRef`s (opaque buckets first, then cutout
/// buckets, then translucent ones back to front) go to an instance-step vertex buffer and draw
/// `i` uses `first_instance = i`.
pub(crate) struct MidPath {
    voxel: VoxelPipelines,
    geometry: wgpu::BindGroup,
    index: wgpu::Buffer,
    draw_refs: wgpu::Buffer,
    /// `DrawRef` capacity (8 per slot: 6 face buckets, cutout and translucent).
    capacity: usize,
    cull: CpuCull,
    refs: Vec<DrawRef>,
}

impl MidPath {
    /// Creates the per-frame `DrawRef` buffer (`8 * slots` entries).
    pub fn new(
        device: &wgpu::Device,
        voxel: VoxelPipelines,
        geometry: wgpu::BindGroup,
        index: wgpu::Buffer,
        slots: u32,
    ) -> Self {
        let capacity = 8 * slots as usize;
        let draw_refs = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel draw refs"),
            size: (capacity * size_of::<DrawRef>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            voxel,
            geometry,
            index,
            draw_refs,
            capacity,
            cull: CpuCull::default(),
            refs: Vec::new(),
        }
    }
}

impl TierPath for MidPath {
    fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        chunks: &ChunkTable,
        view: &ViewParams,
        visibility: Option<SlotVisibility<'_>>,
    ) -> u32 {
        self.cull.run(chunks, view, visibility);
        // never more than 8 per live slot; the clamp only guards the buffer
        self.cull.opaque.truncate(self.capacity);
        self.cull
            .cutout
            .truncate(self.capacity - self.cull.opaque.len());
        self.cull
            .translucent
            .truncate(self.capacity - self.cull.opaque.len() - self.cull.cutout.len());
        self.refs.clear();
        self.refs.extend(
            self.cull
                .opaque
                .iter()
                .chain(&self.cull.cutout)
                .chain(&self.cull.translucent)
                .map(|d| DrawRef {
                    chunk_slot: d.slot,
                    quad_start: d.quad_start,
                }),
        );
        if !self.refs.is_empty() {
            queue.write_buffer(&self.draw_refs, 0, bytemuck::cast_slice(&self.refs));
        }
        self.cull.visible_chunks
    }

    fn draw_solid(&self, pass: &mut wgpu::RenderPass<'_>, _arena: &wgpu::Buffer) -> u32 {
        self.draw_lists(
            pass,
            0,
            [
                (&self.voxel.opaque, &self.cull.opaque),
                (&self.voxel.cutout, &self.cull.cutout),
            ],
        )
    }

    fn draw_translucent(&self, pass: &mut wgpu::RenderPass<'_>, _arena: &wgpu::Buffer) -> u32 {
        // The refs of the translucent buckets come after the opaque and cutout ones.
        let first = (self.cull.opaque.len() + self.cull.cutout.len()) as u32;
        self.draw_lists(
            pass,
            first,
            [(&self.voxel.translucent, &self.cull.translucent)],
        )
    }
}

impl MidPath {
    /// Draws `lists` (each with its pipeline), whose `DrawRef`s start at `first` in the ref
    /// buffer, in order. Binds the geometry, the refs and the index buffer first.
    fn draw_lists<const N: usize>(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        mut first: u32,
        lists: [(&wgpu::RenderPipeline, &Vec<BucketDraw>); N],
    ) -> u32 {
        if lists.iter().all(|(_, list)| list.is_empty()) {
            return 0;
        }
        pass.set_bind_group(bindings::GROUP_GEOMETRY, &self.geometry, &[]);
        pass.set_vertex_buffer(0, self.draw_refs.slice(..));
        pass.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
        let mut draws = 0;
        for (pipeline, list) in lists {
            if list.is_empty() {
                continue;
            }
            pass.set_pipeline(pipeline);
            for (i, d) in list.iter().enumerate() {
                let instance = first + i as u32;
                // six indices per quad, base_vertex 0 (vertex_index must start at 0)
                pass.draw_indexed(0..d.count * 6, 0, instance..instance + 1);
                draws += 1;
            }
            first += list.len() as u32;
        }
        draws
    }
}
