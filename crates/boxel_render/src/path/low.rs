//! Low tier: CPU cull + instanced triangle strips + immediates.
use super::{
    BucketDraw, CpuCull, SlotVisibility, TierPath, VoxelPipelines, page_lists, page_switches,
};
use crate::chunk_table::ChunkTable;
use crate::memory::{PageGpu, PageSet};
use crate::renderer::ViewParams;
use boxel_shaders::gpu::ChunkImmediate;

/// No storage buffers in any stage: a page of quads is an instance-step vertex buffer, each
/// visible bucket is `draw(0..4, quad_start..quad_start + n)` as a triangle strip and the chunk
/// origin travels in immediates (one uniform block per pipeline, ADR 0011). Each quad's light
/// palette entry comes from a second instance-step buffer, the page's light page, at the same
/// offsets (ADR 0015): the same `first_instance` reaches both. Each page's run of solid draws
/// binds its two buffers first; the translucent list binds them when the page changes.
pub(crate) struct LowPath {
    voxel: VoxelPipelines,
    cull: CpuCull,
    /// Page changes in the translucent list.
    switches: u32,
}

impl LowPath {
    /// Wraps the instanced pipelines.
    pub fn new(voxel: VoxelPipelines) -> Self {
        Self {
            voxel,
            cull: CpuCull::default(),
            switches: 0,
        }
    }
}

impl TierPath for LowPath {
    fn prepare(
        &mut self,
        _queue: &wgpu::Queue,
        chunks: &ChunkTable,
        view: &ViewParams,
        visibility: Option<SlotVisibility<'_>>,
    ) -> u32 {
        self.cull.run(chunks, view, visibility);
        self.switches = page_switches(&self.cull.translucent);
        self.cull.visible_chunks
    }

    fn draw_solid(&self, pass: &mut wgpu::RenderPass<'_>, pages: &PageSet) -> u32 {
        let mut draws = 0;
        for (pipeline, lists) in [
            (&self.voxel.opaque, &self.cull.opaque),
            (&self.voxel.cutout, &self.cull.cutout),
        ] {
            let mut set = false;
            for (page, list) in page_lists(lists) {
                let Some(page) = pages.get(page) else {
                    continue;
                };
                if !set {
                    pass.set_pipeline(pipeline);
                    set = true;
                }
                bind_page(pass, page);
                draws += draw_run(pass, list);
            }
        }
        draws
    }

    fn draw_translucent(&self, pass: &mut wgpu::RenderPass<'_>, pages: &PageSet) -> u32 {
        let list = &self.cull.translucent;
        if list.is_empty() {
            return 0;
        }
        pass.set_pipeline(&self.voxel.translucent);
        let mut draws = 0;
        let mut start = 0;
        // One run per page change: its buffers bound, then its draws.
        while start < list.len() {
            let page = list[start].page;
            let end = list[start..]
                .iter()
                .position(|d| d.page != page)
                .map_or(list.len(), |n| start + n);
            if let Some(gpu) = pages.get(page) {
                bind_page(pass, gpu);
                draws += draw_run(pass, &list[start..end]);
            }
            start = end;
        }
        draws
    }

    fn translucent_page_switches(&self) -> u32 {
        self.switches
    }
}

/// Binds a page's quads (slot 0) and its light page (slot 1).
fn bind_page(pass: &mut wgpu::RenderPass<'_>, page: &PageGpu) {
    pass.set_vertex_buffer(0, page.quads.slice(..));
    if let Some(light) = &page.light {
        pass.set_vertex_buffer(1, light.slice(..));
    }
}

/// Draws `list` (one pipeline, its page bound), setting the chunk origin immediates for its first
/// draw and whenever the chunk changes (a pipeline change clears them; consecutive buckets of one
/// chunk share them).
fn draw_run(pass: &mut wgpu::RenderPass<'_>, list: &[BucketDraw]) -> u32 {
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
    }
    list.len() as u32
}
