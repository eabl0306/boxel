//! Mid tier: CPU cull + `draw_indexed` per visible bucket with `DrawRef`.
use super::{
    BucketDraw, CpuCull, SlotVisibility, TierPath, VoxelPipelines, page_lists, page_switches,
};
use crate::chunk_table::ChunkTable;
use crate::memory::PageSet;
use crate::renderer::ViewParams;
use boxel_shaders::gpu::{DrawRef, bindings};

/// Vertex pulling with CPU culling: the frame's `DrawRef`s (opaque buckets first, page by page,
/// then cutout buckets the same way, then translucent ones back to front) go to an instance-step
/// vertex buffer and draw `i` uses `first_instance = i`. Each page's run of solid draws binds its
/// page first; the translucent list binds a page when it changes.
pub(crate) struct MidPath {
    voxel: VoxelPipelines,
    index: wgpu::Buffer,
    draw_refs: wgpu::Buffer,
    /// `DrawRef` capacity (8 per slot: 6 face buckets, cutout and translucent).
    capacity: usize,
    cull: CpuCull,
    refs: Vec<DrawRef>,
    /// Page changes in the translucent list.
    switches: u32,
}

impl MidPath {
    /// Creates the per-frame `DrawRef` buffer (`8 * slots` entries).
    pub fn new(
        device: &wgpu::Device,
        voxel: VoxelPipelines,
        index: wgpu::Buffer,
        slots: u32,
    ) -> Self {
        let (draw_refs, capacity) = draw_refs(device, slots);
        Self {
            voxel,
            index,
            draw_refs,
            capacity,
            cull: CpuCull::default(),
            refs: Vec::new(),
            switches: 0,
        }
    }
}

/// The per-frame `DrawRef` buffer for `slots` chunk slots (8 a slot), bound by its written
/// range, so a new one is never zero-filled.
fn draw_refs(device: &wgpu::Device, slots: u32) -> (wgpu::Buffer, usize) {
    let capacity = 8 * slots.max(1) as usize;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel draw refs"),
        size: (capacity * size_of::<DrawRef>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    (buffer, capacity)
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
        let mut room = self.capacity;
        for list in self.cull.opaque.iter_mut().chain(&mut self.cull.cutout) {
            list.truncate(room);
            room -= list.len();
        }
        self.cull.translucent.truncate(room);
        self.switches = page_switches(&self.cull.translucent);
        self.refs.clear();
        self.refs.extend(
            self.cull
                .opaque
                .iter()
                .chain(&self.cull.cutout)
                .flatten()
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

    fn draw_solid(&self, pass: &mut wgpu::RenderPass<'_>, pages: &PageSet) -> u32 {
        if self.cull.solid_len() == 0 {
            return 0;
        }
        self.bind_refs(pass);
        let mut first = 0;
        let mut draws = 0;
        for (pipeline, lists) in [
            (&self.voxel.opaque, &self.cull.opaque),
            (&self.voxel.cutout, &self.cull.cutout),
        ] {
            let mut set = false;
            for (page, list) in page_lists(lists) {
                let Some(geometry) = pages.get(page).and_then(|p| p.geometry.as_ref()) else {
                    first += list.len() as u32;
                    continue;
                };
                if !set {
                    pass.set_pipeline(pipeline);
                    set = true;
                }
                pass.set_bind_group(bindings::GROUP_GEOMETRY, geometry, &[]);
                draws += draw_run(pass, list, first);
                first += list.len() as u32;
            }
        }
        draws
    }

    fn draw_translucent(&self, pass: &mut wgpu::RenderPass<'_>, pages: &PageSet) -> u32 {
        let list = &self.cull.translucent;
        if list.is_empty() {
            return 0;
        }
        self.bind_refs(pass);
        pass.set_pipeline(&self.voxel.translucent);
        // The refs of the translucent buckets come after the opaque and cutout ones.
        let first = self.cull.solid_len() as u32;
        let mut bound = None;
        let mut draws = 0;
        for (i, d) in list.iter().enumerate() {
            if bound != Some(d.page) {
                let Some(geometry) = pages.get(d.page).and_then(|p| p.geometry.as_ref()) else {
                    continue;
                };
                pass.set_bind_group(bindings::GROUP_GEOMETRY, geometry, &[]);
                bound = Some(d.page);
            }
            let instance = first + i as u32;
            pass.draw_indexed(0..d.count * 6, 0, instance..instance + 1);
            draws += 1;
        }
        draws
    }

    fn resize_table(&mut self, device: &wgpu::Device, _table: Option<&wgpu::Buffer>, slots: u32) {
        (self.draw_refs, self.capacity) = draw_refs(device, slots);
    }

    fn translucent_page_switches(&self) -> u32 {
        self.switches
    }
}

impl MidPath {
    /// Binds the refs written this frame (only that range: wgpu would zero-fill the rest of the
    /// buffer at its first use) and the index buffer.
    fn bind_refs(&self, pass: &mut wgpu::RenderPass<'_>) {
        let written = (self.refs.len() * size_of::<DrawRef>()) as u64;
        pass.set_vertex_buffer(0, self.draw_refs.slice(..written));
        pass.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
    }
}

/// Draws `list`, whose `DrawRef`s start at `first` in the ref buffer.
fn draw_run(pass: &mut wgpu::RenderPass<'_>, list: &[BucketDraw], first: u32) -> u32 {
    for (i, d) in list.iter().enumerate() {
        let instance = first + i as u32;
        // six indices per quad, base_vertex 0 (vertex_index must start at 0)
        pass.draw_indexed(0..d.count * 6, 0, instance..instance + 1);
    }
    list.len() as u32
}
