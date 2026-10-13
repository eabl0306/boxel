//! High tier: GPU cull + `multi_draw_indexed_indirect_count`, one pair of lists per page.
use super::{
    BucketDraw, CullPipeline, SlotVisibility, TierPath, VoxelPipelines, page_switches,
    translucent_draws,
};
use crate::chunk_table::ChunkTable;
use crate::memory::{ARGS_BYTES, PageSet};
use crate::renderer::ViewParams;
use boxel_shaders::gpu::chunk_memory::MAX_PAGES;
use boxel_shaders::gpu::{DrawRef, bindings};

/// Buffers the High path needs from the renderer.
pub(crate) struct HighInputs<'a> {
    /// `View` uniform.
    pub view: &'a wgpu::Buffer,
    /// Chunk table (storage).
    pub chunks: &'a wgpu::Buffer,
    /// Shared quad index buffer.
    pub index: wgpu::Buffer,
    /// Pages with draw lists (`L`): page ids are below it.
    pub pages: u32,
    /// Opaque draws of one page's list (6 per chunk a page holds); its cutout list holds a sixth.
    pub stride: u32,
    /// Chunk slots (the cave culling bits hold one per slot, the translucent refs one per slot).
    pub slots: u32,
}

/// GPU-driven path: `cull_chunks` writes two compacted indirect lists per page (the opaque face
/// buckets of page `p` at `[p * stride, (p + 1) * stride)`, its cutout buckets at
/// `L * stride + p * stride / 6`, a sixth as long) with matching `DrawRef`s and atomic counts;
/// each list is one `multi_draw_indexed_indirect_count` with its page bound (2 binds and 2
/// multi-draws per page holding chunks). A page holds at most `stride / 6` chunks, so no list
/// ever drops a bucket. The translucent buckets must be drawn back to front, so they come from
/// the CPU as on Mid ([`translucent_draws`]): one `draw_indexed` per visible chunk with
/// translucent quads, with its `DrawRef` in a small vertex buffer written every frame.
pub(crate) struct HighPath {
    voxel: VoxelPipelines,
    cull: wgpu::ComputePipeline,
    /// Group 0 of the cull, built again whenever one of its buffers changes.
    cull_layout: wgpu::BindGroupLayout,
    cull_bind_group: wgpu::BindGroup,
    /// The `View` uniform (the cull's binding 0).
    view: wgpu::Buffer,
    index: wgpu::Buffer,
    /// The draw lists in use (`L` pages).
    lists: ListPair,
    /// The cull state of `cull.wgsl`: two draw counts per page id (cleared every frame), then one
    /// cave culling bit per slot from `bindings::CULL_VISIBLE_OFFSET` (written when they change).
    draw_counts: wgpu::Buffer,
    /// Opaque draws of a page's list.
    stride: u32,
    /// `DrawRef`s of the translucent draws (one per slot at most).
    translucent_refs: wgpu::Buffer,
    /// This frame's translucent buckets, back to front.
    translucent: Vec<BucketDraw>,
    /// `translucent_refs` capacity.
    translucent_capacity: usize,
    /// Page changes in `translucent`.
    switches: u32,
}

/// High's two draw lists for `pages` page ids (ADR 0035): the indirect args and their
/// `DrawRef`s. A larger pair is cleared before it replaces the one in use (`cleared` bytes of
/// `args` then `refs` so far), so wgpu never zero-fills it inside a frame.
#[derive(Debug)]
pub(crate) struct ListPair {
    /// `DrawIndexedIndirect`s.
    pub args: wgpu::Buffer,
    /// Their `DrawRef`s.
    pub refs: wgpu::Buffer,
    /// Page ids the lists cover (`L`).
    pub pages: u32,
    /// Bytes cleared, `args` first.
    pub cleared: u64,
}

impl ListPair {
    /// Lists for `pages` page ids (at most `MAX_PAGES`) of `stride` opaque draws each.
    pub fn new(device: &wgpu::Device, pages: u32, stride: u32) -> Self {
        let pages = pages.clamp(1, MAX_PAGES);
        let draws = u64::from(pages) * u64::from(stride.max(6)) * 7 / 6;
        // `COPY_DST`: a larger pair is cleared before its first use (ADR 0035).
        let args = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel draw args"),
            size: draws * ARGS_BYTES,
            usage: wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let refs = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel draw refs"),
            size: draws * size_of::<DrawRef>() as u64,
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            args,
            refs,
            pages,
            cleared: 0,
        }
    }

    /// Bytes of both lists.
    pub fn bytes(&self) -> u64 {
        self.args.size() + self.refs.size()
    }

    /// True once every byte was cleared.
    pub fn is_clear(&self) -> bool {
        self.cleared >= self.bytes()
    }

    /// Records clears of at most `budget` bytes of what is still uncleared; returns the bytes
    /// cleared.
    pub fn clear_some(&mut self, encoder: &mut wgpu::CommandEncoder, budget: u64) -> u64 {
        let mut done = 0;
        for (buffer, start) in [(&self.args, 0), (&self.refs, self.args.size())] {
            let end = start + buffer.size();
            let from = self.cleared.max(start);
            if from >= end || done >= budget {
                continue;
            }
            let n = (end - from).min(budget - done);
            encoder.clear_buffer(buffer, from - start, Some(n));
            done += n;
            self.cleared = from + n;
        }
        done
    }
}

/// The cull state for `slots` chunk slots: the draw counts, then one visibility bit a slot.
fn cull_state(device: &wgpu::Device, slots: u32) -> wgpu::Buffer {
    let visible_words = u64::from(slots.div_ceil(32).max(1));
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel cull state"),
        size: bindings::CULL_VISIBLE_OFFSET + 4 * visible_words,
        // `COPY_SRC`: tests read the counts back.
        usage: wgpu::BufferUsages::INDIRECT
            | wgpu::BufferUsages::STORAGE
            | wgpu::BufferUsages::COPY_DST
            | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

/// The translucent draws' `DrawRef`s for `slots` chunk slots (bound by their written range).
fn translucent_refs(device: &wgpu::Device, slots: u32) -> (wgpu::Buffer, usize) {
    let capacity = slots.max(1) as usize;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel translucent draw refs"),
        size: (capacity * size_of::<DrawRef>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    (buffer, capacity)
}

/// Group 0 of the cull: view, chunk table, lists, cull state.
fn cull_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::Buffer,
    chunks: &wgpu::Buffer,
    lists: &ListPair,
    state: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("boxel cull"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: view.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: chunks.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: lists.args.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: lists.refs.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: state.as_entire_binding(),
            },
        ],
    })
}

impl HighPath {
    /// Creates the draw lists and the cull bind group.
    pub fn new(
        device: &wgpu::Device,
        voxel: VoxelPipelines,
        cull: CullPipeline,
        inputs: HighInputs<'_>,
    ) -> Self {
        let stride = inputs.stride.max(6);
        let lists = ListPair::new(device, inputs.pages, stride);
        let draw_counts = cull_state(device, inputs.slots);
        let cull_bind_group = cull_bind_group(
            device,
            &cull.layout,
            inputs.view,
            inputs.chunks,
            &lists,
            &draw_counts,
        );
        let (translucent_refs, translucent_capacity) = translucent_refs(device, inputs.slots);
        Self {
            voxel,
            cull: cull.pipeline,
            cull_layout: cull.layout,
            cull_bind_group,
            view: inputs.view.clone(),
            index: inputs.index,
            lists,
            draw_counts,
            stride,
            translucent_refs,
            translucent: Vec::new(),
            translucent_capacity,
            switches: 0,
        }
    }

    /// Testing: the draw counts the last frame's cull wrote, opaque and cutout per page
    /// (waits for the GPU).
    pub fn read_draw_counts(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Vec<[u32; 2]>, String> {
        let bytes = crate::memory::read_buffer(
            device,
            queue,
            &self.draw_counts,
            0,
            bindings::CULL_VISIBLE_OFFSET,
        )?;
        let words: &[u32] = bytemuck::cast_slice(&bytes);
        let max = MAX_PAGES as usize;
        Ok((0..self.lists.pages as usize)
            .map(|p| [words[p], words[max + p]])
            .collect())
    }
}

impl TierPath for HighPath {
    fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        chunks: &ChunkTable,
        view: &ViewParams,
        visibility: Option<SlotVisibility<'_>>,
    ) -> u32 {
        // The cull reads the bits only while `View::counts.w` says so (set by the renderer).
        if let Some(v) = visibility
            && v.changed
            && !v.bits.is_empty()
        {
            queue.write_buffer(
                &self.draw_counts,
                bindings::CULL_VISIBLE_OFFSET,
                bytemuck::cast_slice(v.bits),
            );
        }
        translucent_draws(chunks, view, visibility, &mut self.translucent);
        // never more than one per live slot; the clamp only guards the buffer
        self.translucent.truncate(self.translucent_capacity);
        self.switches = page_switches(&self.translucent);
        if !self.translucent.is_empty() {
            let refs: Vec<DrawRef> = self
                .translucent
                .iter()
                .map(|d| DrawRef {
                    chunk_slot: d.slot,
                    quad_start: d.quad_start,
                })
                .collect();
            queue.write_buffer(&self.translucent_refs, 0, bytemuck::cast_slice(&refs));
        }
        0
    }

    fn encode_compute(&self, encoder: &mut wgpu::CommandEncoder, slot_count: u32) {
        encoder.clear_buffer(&self.draw_counts, 0, Some(bindings::CULL_VISIBLE_OFFSET));
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

    fn draw_solid(&self, pass: &mut wgpu::RenderPass<'_>, pages: &PageSet) -> u32 {
        let live = || {
            pages
                .holding_chunks()
                .filter(|(p, _)| u32::from(*p) < self.lists.pages)
                .filter_map(|(p, page)| page.geometry.as_ref().map(|g| (u64::from(p), g)))
        };
        if live().next().is_none() {
            return 0;
        }
        pass.set_vertex_buffer(0, self.lists.refs.slice(..));
        pass.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
        let stride = u64::from(self.stride);
        let cut = stride / 6;
        let mut draws = 0;
        pass.set_pipeline(&self.voxel.opaque);
        for (p, geometry) in live() {
            pass.set_bind_group(bindings::GROUP_GEOMETRY, geometry, &[]);
            pass.multi_draw_indexed_indirect_count(
                &self.lists.args,
                p * stride * ARGS_BYTES,
                &self.draw_counts,
                4 * p,
                self.stride,
            );
            draws += 1;
        }
        pass.set_pipeline(&self.voxel.cutout);
        let cutout_base = u64::from(self.lists.pages) * stride;
        for (p, geometry) in live() {
            pass.set_bind_group(bindings::GROUP_GEOMETRY, geometry, &[]);
            pass.multi_draw_indexed_indirect_count(
                &self.lists.args,
                (cutout_base + p * cut) * ARGS_BYTES,
                &self.draw_counts,
                4 * (u64::from(MAX_PAGES) + p),
                cut as u32,
            );
            draws += 1;
        }
        draws
    }

    fn draw_translucent(&self, pass: &mut wgpu::RenderPass<'_>, pages: &PageSet) -> u32 {
        if self.translucent.is_empty() {
            return 0;
        }
        pass.set_index_buffer(self.index.slice(..), wgpu::IndexFormat::Uint32);
        pass.set_pipeline(&self.voxel.translucent);
        let written = (self.translucent.len() * size_of::<DrawRef>()) as u64;
        pass.set_vertex_buffer(0, self.translucent_refs.slice(..written));
        // Group 2 is set at the first draw: the pass may be a new one (the far LOD's pyramid ends
        // the first after the solid buckets), or the LOD bound its own geometry group.
        let mut bound = None;
        let mut draws = 0;
        for (i, d) in self.translucent.iter().enumerate() {
            if bound != Some(d.page) {
                let Some(geometry) = pages.get(d.page).and_then(|p| p.geometry.as_ref()) else {
                    continue;
                };
                pass.set_bind_group(bindings::GROUP_GEOMETRY, geometry, &[]);
                bound = Some(d.page);
            }
            let instance = i as u32;
            pass.draw_indexed(0..d.count * 6, 0, instance..instance + 1);
            draws += 1;
        }
        draws
    }

    fn translucent_page_switches(&self) -> u32 {
        self.switches
    }

    fn list_pages(&self) -> Option<u32> {
        Some(self.lists.pages)
    }

    fn create_lists(&self, device: &wgpu::Device, pages: u32) -> Option<ListPair> {
        Some(ListPair::new(device, pages, self.stride))
    }

    fn swap_lists(&mut self, device: &wgpu::Device, lists: ListPair, table: &wgpu::Buffer) {
        self.lists = lists;
        self.cull_bind_group = cull_bind_group(
            device,
            &self.cull_layout,
            &self.view,
            table,
            &self.lists,
            &self.draw_counts,
        );
    }

    fn resize_table(&mut self, device: &wgpu::Device, table: Option<&wgpu::Buffer>, slots: u32) {
        let Some(table) = table else {
            return;
        };
        self.draw_counts = cull_state(device, slots);
        (self.translucent_refs, self.translucent_capacity) = translucent_refs(device, slots);
        self.cull_bind_group = cull_bind_group(
            device,
            &self.cull_layout,
            &self.view,
            table,
            &self.lists,
            &self.draw_counts,
        );
    }

    fn list_bytes(&self) -> u64 {
        self.lists.bytes()
    }

    fn debug_draw_counts(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Option<Result<Vec<[u32; 2]>, String>> {
        Some(self.read_draw_counts(device, queue))
    }
}
