//! The LOD pass: the far LOD's own arena, its node records, the cull and the draws.
//!
//! Every node is one allocation of its face buckets in the LOD arena (a `VERTEX | COPY_DST`
//! buffer, `STORAGE` too on High, sub-allocated like the main arena), so LOD can never take room
//! from full detail. Its quads reach the GPU at the start of the next frame drawn (one staging
//! buffer per frame, one copy per node), at most [`UPLOAD_FRAMES`] frames of the world side's
//! sends a frame: after frames that were not drawn (a hidden window) the backlog goes up over a
//! few frames, not in one hitch. A node is drawn only once its quads are up; uploads that could
//! not be recorded (no staging buffer: out of memory) wait for the next frame.
//!
//! Two ways to draw, with the same vertex and fragment functions (`lod_common.wgsl`), so the same
//! pixels:
//! - Mid and Low (M1): each frame the CPU keeps the nodes inside the frustum (the node box, then
//!   the box-level back-face test of each face bucket, as `cull.wgsl` does for chunks) with an
//!   octant left to draw, and draws the buckets that can face the camera, one instanced draw
//!   per run of them in the arena: `draw(0..4, first..first + n)`, the node in the immediates
//!   ([`LodImmediate`]). Level 1 is drawn first, then the coarser ones (roughly front to back,
//!   for early-Z).
//! - High (M3, GPU-driven): the nodes live in a GPU table ([`LodNodeGpu`], only the entries that
//!   changed are written), `lod_cull.wgsl` keeps the face buckets that can face the camera of the
//!   nodes inside the frustum and writes them into indirect lists, one per segment of
//!   [`bits::SEGMENT_SLOTS`] slots, and each list is one `multi_draw_indexed_indirect_count`
//!   with vertex pulling (`lod_pull.wgsl`) and the main path's shared quad index buffer. No
//!   per-node CPU work, and only the buckets that can be seen go through the vertex stage. The
//!   cull runs after the opaque and cutout terrain, against a depth pyramid of what it drew
//!   (`lod_hiz.wgsl`): a node behind full detail over its whole screen rectangle is not drawn
//!   (the renderer ends the main pass there and goes on in a second one). The pyramid and the
//!   split pass cost about 0.03 ms on the surface, where they hide almost nothing: the cull
//!   counts what they hide, and while that is under 1 node in [`OCCLUSION_RATIO`] the frames
//!   cull without them, before the main pass ([`Occlusion`]; the same pixels either way).
//!
//! Skirts and fluid walls (`boxel_mesh::lod::Seam`, last in each face bucket) are drawn only
//! where a seam can show them: by a hidden octant, or on a node face whose neighbor of the same
//! level does not draw its cells there (kept as nodes change; bits [`bits::ACROSS_SHIFT`] of
//! the node on High). Both paths cut a bucket's draw by the same rule ([`bits::bucket_quads`]),
//! so they draw the same pixels.
//!
//! Underground ([`LodPass::prepare`]'s `sealed`: the cave culling search reached no chunk on the
//! border of its box, so no line of sight leaves it) nothing is drawn.
use super::{LodInput, LodReport, far};
use crate::arena::{ArenaAlloc, ArenaAllocator, QUAD_BYTES};
use crate::chunk_table::{GPU_WINDOW, GPU_WINDOW_SLACK};
use crate::path::bucket_visible;
use crate::renderer::ViewParams;
use crate::targets::Targets;
use crate::{DEPTH_FORMAT, DistanceFog, MAIN_COLOR_FORMAT};
use boxel_math::{Aabb, I64Vec3, Vec3, wrap_to_i32};
use boxel_mesh::lod::seam_counts;
use boxel_mesh::{ChunkMesh, MAX_QUADS_PER_FACE_BUCKET};
use boxel_shaders::gpu::{DrawIndexedIndirect, DrawRef, LodImmediate, LodNodeGpu, lod as bits};
use boxel_shaders::{ShaderModuleId, create_module};
use boxel_voxel::{ChunkKey, Face};
use boxel_world::lod::LodOp;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Nodes the pass holds at most (the `extreme` preset keeps about 16k).
pub(crate) const MAX_LOD_NODES: u32 = 1 << 16;
/// Levels a node can have (4 bits on the GPU).
const LEVELS: usize = bits::LEVEL_MASK as usize + 1;
/// Largest staging buffer of a frame's uploads (more go in several).
const STAGING_BYTES: u64 = 64 << 20;
/// A frame uploads at most this many frames of the world side's sends
/// (`LodSettings::upload_bytes_per_frame`), and at least one node: more than it sends while
/// every frame is drawn.
pub(crate) const UPLOAD_FRAMES: u64 = 4;
/// A node is drawn while its origin is less than this many blocks from the render origin on every
/// axis: its wrapped `i32` origin difference is exact there (the GPU window of `chunk_table`).
const WINDOW: i64 = 1 << 30;

/// One resident node.
#[derive(Debug)]
struct LodDraw {
    key: ChunkKey,
    /// World block origin of the node (`pos << (5 + level)`).
    origin: I64Vec3,
    /// Its quads in the arena: the 6 face buckets, contiguous.
    alloc: ArenaAlloc,
    /// Quads per face bucket.
    faces: [u32; 6],
    /// Per face bucket, its seam quads at the end: `[on a node face, on a middle plane]`.
    seams: [[u32; 2]; 6],
    /// Bit `f`: the node of this level across face bucket `f` draws its cells by that face
    /// ([`LodNodes::across`]), kept as nodes change.
    across: u8,
    /// In [`LodNodes::stale`].
    stale: bool,
    /// Hidden octants (bit `x | y << 1 | z << 2`).
    mask: u8,
    /// The mesh until its quads are uploaded: the node is not drawn until then.
    pending: Option<Arc<ChunkMesh>>,
    /// Its index in `LodPass::levels[level]`.
    at: u32,
    /// Its quads counted in [`LodNodes::live`] (0: not counted): uploaded with an octant left.
    counted: u32,
}

impl LodDraw {
    fn level(&self) -> usize {
        usize::from(self.key.lod)
    }
}

/// One draw of a frame.
#[derive(Clone, Copy, Debug)]
struct NodeDraw {
    immediate: LodImmediate,
    first: u32,
    quads: u32,
}

/// Counters of the pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LodPassStats {
    /// Resident nodes.
    pub nodes: u32,
    /// Nodes with an octant left to draw (in view or not), last frame.
    pub drawn: u32,
    /// Draw calls of the last frame: the runs of face buckets of the drawn nodes inside the
    /// frustum (one instanced draw each), or on High the indirect lists drawn (one per segment).
    pub draws: u32,
    /// Quads of those draws; on High (culled on the GPU) the quads of the drawn nodes.
    pub quads: u64,
    /// Node meshes refused (cumulative).
    pub refused: u32,
    /// Quads copied into the arena (cumulative).
    pub uploaded_quads: u64,
    /// Arena quads in use and the arena's size.
    pub arena_used: u32,
    /// See `arena_used`.
    pub arena_capacity: u32,
}

/// What the GPU-driven path of High needs from the renderer.
pub(crate) struct GpuInputs {
    /// The `View` uniform (the cull reads its frustum, render origin and eye).
    pub view: wgpu::Buffer,
    /// The main path's shared quad index buffer (`[0, 1, 2, 2, 1, 3] + 4q`): drawing the LOD
    /// leaves it bound for the translucent buckets.
    pub index: wgpu::Buffer,
}

/// The far LOD's arena, pipeline and nodes (module docs).
pub(crate) struct LodPass {
    pipeline: wgpu::RenderPipeline,
    arena: wgpu::Buffer,
    nodes: LodNodes,
    /// High: the GPU-driven path (module docs).
    gpu: Option<GpuDriven>,
    /// The next staging buffer is created invalid (tests: [`LodPass::fail_next_upload`]).
    fail_next: bool,
    /// Uploads were lost since the last recorded one (logged once).
    failing: bool,
}

impl LodPass {
    /// Builds the pipeline (layout `[materials, view]` and 16 bytes of immediates: the tier's own
    /// group 0 and 1 bind groups work with it) and an arena of `arena_quads` quads, cut to the
    /// device's `max_buffer_size`. `color_mip` is the 1x1 mip level of the block textures. With
    /// `gpu` (High) it draws through the GPU-driven path, its arena also cut to
    /// `max_storage_buffer_binding_size`. Call it inside error scopes.
    pub fn new(
        device: &wgpu::Device,
        materials: &wgpu::BindGroupLayout,
        view: &wgpu::BindGroupLayout,
        arena_quads: u32,
        color_mip: u32,
        gpu: Option<GpuInputs>,
    ) -> Self {
        let limits = device.limits();
        let mut bytes = limits.max_buffer_size;
        let mut usage = wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST;
        if gpu.is_some() {
            bytes = bytes.min(limits.max_storage_buffer_binding_size);
            usage |= wgpu::BufferUsages::STORAGE;
        }
        let quads = u64::from(arena_quads).min(bytes / QUAD_BYTES).max(1) as u32;
        let arena = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel lod arena"),
            size: u64::from(quads) * QUAD_BYTES,
            usage,
            mapped_at_creation: false,
        });
        let mut nodes = LodNodes::new(quads, arena_quads, color_mip);
        let gpu = gpu.map(|inputs| {
            nodes.table = Some(GpuTable::default());
            GpuDriven::new(device, materials, view, inputs)
        });
        Self {
            pipeline: pipeline(device, materials, view),
            arena,
            nodes,
            gpu,
            fail_next: false,
            failing: false,
        }
    }

    /// Testing: the next staging buffer of the LOD uploads cannot be created, as when out of
    /// memory (wgpu reports an error): those nodes are not drawn and go up the next frame.
    pub fn fail_next_upload(&mut self) {
        self.fail_next = true;
    }

    /// `LodSettings::arena_quads` the pass was built for.
    pub fn requested_quads(&self) -> u32 {
        self.nodes.requested_quads
    }

    /// True if it draws through the GPU-driven path (High).
    pub fn gpu_driven(&self) -> bool {
        self.gpu.is_some()
    }

    /// The horizon while the last input draws LOD.
    pub fn far(&self) -> Option<f32> {
        self.nodes.far
    }

    /// The terrain fog of the View uniform while the LOD draws: the last input's distance fog at
    /// the horizon ([`crate::DistanceFog::range`]).
    pub fn fog_params(&self) -> Option<[f32; 4]> {
        self.nodes.fog_params()
    }

    /// Applies one frame's input ([`LodNodes::apply`]).
    pub fn apply(&mut self, input: LodInput) {
        self.nodes.apply(input);
    }

    /// Takes the refusals since the last call, with the arena's free quads.
    pub fn report(&mut self) -> LodReport {
        self.nodes.report()
    }

    /// Counters of the last frame.
    pub fn stats(&self) -> LodPassStats {
        self.nodes.stats
    }

    /// The frame's work before the main pass: the waiting quads copied into the arena (recorded
    /// into `encoder`: staging buffers of at most [`STAGING_BYTES`], one copy per node; at most
    /// the frame's upload budget, module docs), then the draw list for `view` (the renderer's
    /// cull view, its far plane at the horizon; the View uniform holds it on High): on the CPU,
    /// or on High the changed node table entries (written with `queue`) and the cull, recorded
    /// here unless it waits for the solid terrain drawn into `targets` this frame
    /// ([`LodPass::culls_after_solid`]). A batch whose staging buffer cannot be created waits for
    /// the next frame, its nodes not drawn meanwhile. With `sealed` (no line of sight leaves the
    /// cave culling box) nothing is drawn.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        targets: &Targets,
        view: &ViewParams,
        sealed: bool,
    ) {
        let budget = self.nodes.upload_budget;
        for (batch, bytes) in self.nodes.take_uploads(STAGING_BYTES, budget) {
            let fail = std::mem::take(&mut self.fail_next);
            match copy(device, encoder, &self.arena, &batch, bytes, fail) {
                Ok(()) => {
                    self.failing = false;
                    self.nodes.stats.uploaded_quads += bytes / QUAD_BYTES;
                }
                Err(e) => {
                    // wgpu reported why (out of memory); the device stays usable.
                    if !std::mem::replace(&mut self.failing, true) {
                        tracing::error!("far LOD uploads delayed: {e}; tried again next frame");
                    }
                    self.nodes.restore(batch);
                }
            }
        }
        match &mut self.gpu {
            None => self.nodes.cull(view, sealed),
            Some(gpu) => {
                let segments = self.nodes.gpu_frame(view.render_origin, sealed);
                gpu.prepare(
                    device,
                    queue,
                    encoder,
                    targets,
                    &self.arena,
                    &mut self.nodes,
                    segments,
                );
            }
        }
    }

    /// Testing (High): the draws the last frame's GPU cull wrote, every segment's together
    /// (waits for the GPU); `None` on the instanced path or before any cull.
    pub fn read_gpu_draws(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<u32> {
        let gpu = self.gpu.as_ref()?;
        let lists = gpu.lists.as_ref()?;
        if gpu.segments == 0 {
            return Some(0);
        }
        let bytes = 4 * u64::from(gpu.segments);
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel lod draw counts read"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("boxel lod draw counts read"),
        });
        encoder.copy_buffer_to_buffer(&lists.counts, 0, &read, 0, bytes);
        queue.submit([encoder.finish()]);
        let slice = read.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;
        let mapped = slice.get_mapped_range().ok()?;
        let draws = bytemuck::cast_slice::<u8, u32>(&mapped).iter().sum();
        drop(mapped);
        read.unmap();
        Some(draws)
    }

    /// True if this frame's LOD is culled on the GPU after the opaque and cutout terrain (High,
    /// with something to draw, using the depth pyramid this frame): the renderer then ends the
    /// main pass after them, calls [`LodPass::cull_after_solid`] and draws the LOD and the rest
    /// in a second pass.
    pub fn culls_after_solid(&self) -> bool {
        self.gpu.as_ref().is_some_and(GpuDriven::culls_after_solid)
    }

    /// Testing (High): true if this frame's cull uses the depth pyramid.
    pub fn occlusion_culling(&self) -> Option<bool> {
        self.gpu.as_ref().map(|g| g.occlusion.on)
    }

    /// Records the cull of [`LodPass::culls_after_solid`]: the depth pyramid of `targets`
    /// (holding the opaque and cutout terrain) and the node cull against it.
    pub fn cull_after_solid(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        targets: &Targets,
    ) {
        if let Some(gpu) = &mut self.gpu {
            gpu.cull_after_solid(device, encoder, targets);
        }
    }

    /// Records the frame's draws into the main pass (groups 0 and 1 are bound): the LOD pipeline,
    /// vertex buffer 0 = the LOD arena, one instanced draw per run of a node's face buckets (its
    /// immediates set once); on High the pull pipeline,
    /// its geometry in group [`bits::GROUP_GEOMETRY`], vertex buffer 0 = its draw refs, the
    /// shared quad index buffer, one indirect draw per segment. Binds no group 2, and no index
    /// buffer but the main path's own. Returns the draw calls recorded.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        if let Some(gpu) = &self.gpu {
            return gpu.draw(pass);
        }
        let draws = &self.nodes.draws;
        if draws.is_empty() {
            return 0;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.arena.slice(..));
        let mut node = None;
        for d in draws {
            // A node's runs follow each other: its immediates once.
            if node != Some(d.immediate) {
                pass.set_immediates(0, bytemuck::bytes_of(&d.immediate));
                node = Some(d.immediate);
            }
            pass.draw(0..4, d.first..d.first + d.quads);
        }
        draws.len() as u32
    }
}

/// A node's quads on their way up.
struct Upload {
    /// The node's slot.
    slot: u32,
    mesh: Arc<ChunkMesh>,
    /// Its first arena quad and its quads.
    offset: u32,
    quads: u32,
}

/// The CPU side of the pass (no GPU, unit-tested): the arena allocator, the node records, the
/// waiting uploads and the draw list.
struct LodNodes {
    alloc: ArenaAllocator,
    /// `LodSettings::arena_quads` the pass was built for (the arena may be smaller: the device).
    requested_quads: u32,
    /// The mip of every texture layer that is 1x1: the layer's mean color.
    color_mip: u32,
    by_key: HashMap<ChunkKey, u32>,
    slots: Vec<Option<LodDraw>>,
    free: Vec<u32>,
    /// Slots by level, for the draw order.
    levels: [Vec<u32>; LEVELS],
    /// Slots whose quads wait for the next upload.
    pending: Vec<u32>,
    /// Bytes of quads a frame uploads at most (module docs; at least one node).
    upload_budget: u64,
    /// Refused since the last report.
    refused: Vec<ChunkKey>,
    warned_full: bool,
    /// The last input's horizon when it draws, its distance fog and tint.
    far: Option<f32>,
    fog: DistanceFog,
    tint: bool,
    draws: Vec<NodeDraw>,
    stats: LodPassStats,
    /// Uploaded nodes with an octant left to draw and their quads, kept as nodes change (the
    /// High frame reads them instead of walking every slot).
    live: (u32, u64),
    /// High: the GPU node table's CPU side (which entries to write).
    table: Option<GpuTable>,
    /// Slots whose [`LodDraw::across`] may have changed (a neighbor did).
    stale: Vec<u32>,
}

impl LodNodes {
    /// An arena of `quads` quads, built for `requested` (`LodSettings::arena_quads`).
    fn new(quads: u32, requested: u32, color_mip: u32) -> Self {
        Self {
            alloc: ArenaAllocator::new(quads, 2 * MAX_LOD_NODES + 4),
            requested_quads: requested,
            color_mip,
            by_key: HashMap::new(),
            slots: Vec::new(),
            free: Vec::new(),
            levels: Default::default(),
            pending: Vec::new(),
            upload_budget: u64::MAX,
            refused: Vec::new(),
            warned_full: false,
            far: None,
            fog: DistanceFog::default(),
            tint: false,
            draws: Vec::new(),
            stats: LodPassStats::default(),
            live: (0, 0),
            table: None,
            stale: Vec::new(),
        }
    }

    /// Brings `slot`'s share of [`LodNodes::live`] up to date after it changed.
    fn recount(&mut self, slot: u32) {
        let Some(node) = self.slots.get_mut(slot as usize).and_then(Option::as_mut) else {
            return;
        };
        let now = if node.mask != 0xFF && node.pending.is_none() {
            node.alloc.size.max(1)
        } else {
            0
        };
        if now != node.counted {
            let (n, q) = &mut self.live;
            if node.counted > 0 {
                *n -= 1;
                *q -= u64::from(node.counted);
            }
            if now > 0 {
                *n += 1;
                *q += u64::from(now);
            }
            node.counted = now;
        }
    }

    /// See [`LodPass::fog_params`].
    fn fog_params(&self) -> Option<[f32; 4]> {
        self.far.map(|h| {
            let [start, end] = self.fog.range(h);
            [start, end, 0.0, 0.0]
        })
    }

    /// Applies one frame's input: a resync drops every node first, then the ops in order. A mesh
    /// that does not fit (arena or node table full) is refused: the node is not on the renderer
    /// any more, its older mesh included, and the next [`LodNodes::report`] names it.
    fn apply(&mut self, input: LodInput) {
        self.far = far(&input.settings, input.horizon);
        self.fog = input.fog;
        if self.tint != input.settings.debug_tint {
            self.tint = input.settings.debug_tint;
            self.touch_all();
        }
        self.upload_budget =
            (input.settings.upload_bytes_per_frame as u64).saturating_mul(UPLOAD_FRAMES);
        if input.resync {
            self.clear();
        }
        for op in input.ops {
            match op {
                LodOp::Upsert { key, mesh, mask } => self.upsert(key, mesh, mask),
                LodOp::Mask { key, mask } => {
                    if let Some(&slot) = self.by_key.get(&key)
                        && let Some(node) = self.slots[slot as usize].as_mut()
                    {
                        node.mask = mask;
                        self.recount(slot);
                        self.touch(slot);
                        self.touch_around(key);
                    }
                }
                LodOp::Remove { key } => self.remove(key),
            }
        }
        self.stats.nodes = self.by_key.len() as u32;
        self.stats.arena_used = self.alloc.used();
        self.stats.arena_capacity = self.alloc.capacity();
    }

    /// See [`LodPass::report`].
    fn report(&mut self) -> LodReport {
        LodReport {
            refused: std::mem::take(&mut self.refused),
            arena_free_quads: u64::from(self.alloc.capacity() - self.alloc.used()),
        }
    }

    fn upsert(&mut self, key: ChunkKey, mesh: Arc<ChunkMesh>, mask: u8) {
        let level = usize::from(key.lod);
        let origin = node_origin(key);
        let (Some(origin), true) = (origin, (1..LEVELS).contains(&level)) else {
            tracing::error!("far LOD: {key:?} is no LOD node; refused");
            self.remove(key);
            self.refuse(key);
            return;
        };
        let faces = face_counts(&mesh);
        if faces.iter().any(|&n| n > MAX_QUADS_PER_FACE_BUCKET) {
            tracing::error!("far LOD: {key:?} has a face bucket over the limit; refused");
            self.remove(key);
            self.refuse(key);
            return;
        }
        let quads: u32 = faces.iter().sum();
        if quads == 0 {
            self.remove(key);
            return;
        }
        let seams = node_seams(&mesh, faces);
        let old = self.by_key.get(&key).copied();
        let old_alloc = old.and_then(|s| self.slots[s as usize].as_ref().map(|n| n.alloc));
        if old.is_none() && self.by_key.len() as u32 >= MAX_LOD_NODES {
            self.refuse(key);
            return;
        }
        let Some(alloc) = self.alloc.replace(old_alloc, quads) else {
            // Not on the renderer any more: the world side shows its parent there.
            self.remove(key);
            self.refuse(key);
            return;
        };
        if let Some(slot) = old {
            let node = self.slots[slot as usize].as_mut().expect("a resident node");
            node.alloc = alloc;
            node.faces = faces;
            node.seams = seams;
            node.mask = mask;
            node.pending = Some(mesh);
            self.pending.push(slot);
            self.recount(slot);
            self.touch(slot);
            self.touch_around(key);
            return;
        }
        let slot = self.free.pop().unwrap_or_else(|| {
            self.slots.push(None);
            self.slots.len() as u32 - 1
        });
        let at = self.levels[level].len() as u32;
        self.levels[level].push(slot);
        self.slots[slot as usize] = Some(LodDraw {
            key,
            origin,
            alloc,
            faces,
            seams,
            across: 0,
            stale: true,
            mask,
            pending: Some(mesh),
            at,
            counted: 0,
        });
        self.stale.push(slot);
        self.by_key.insert(key, slot);
        self.pending.push(slot);
        self.touch(slot);
        self.touch_around(key);
    }

    fn remove(&mut self, key: ChunkKey) {
        let Some(slot) = self.by_key.remove(&key) else {
            return;
        };
        if let Some(node) = self.slots[slot as usize].as_mut() {
            // Not drawn any more: out of the live count.
            node.mask = 0xFF;
        }
        self.recount(slot);
        let Some(node) = self.slots[slot as usize].take() else {
            return;
        };
        self.alloc.free(node.alloc);
        let list = &mut self.levels[node.level()];
        list.swap_remove(node.at as usize);
        if let Some(&moved) = list.get(node.at as usize)
            && let Some(m) = self.slots[moved as usize].as_mut()
        {
            m.at = node.at;
        }
        self.free.push(slot);
        self.touch(slot);
        self.touch_around(key);
    }

    fn refuse(&mut self, key: ChunkKey) {
        self.refused.push(key);
        self.stats.refused += 1;
        if !self.warned_full {
            self.warned_full = true;
            tracing::warn!(
                "far LOD: a node mesh was refused (arena of {} quads, {} used, {} nodes): its \
                 parent is drawn there instead; a larger arena_quads avoids it",
                self.alloc.capacity(),
                self.alloc.used(),
                self.by_key.len()
            );
        }
    }

    /// Drops every node.
    fn clear(&mut self) {
        self.by_key.clear();
        self.slots.clear();
        self.free.clear();
        for list in &mut self.levels {
            list.clear();
        }
        self.pending.clear();
        self.stale.clear();
        self.alloc.reset();
        self.stats.nodes = 0;
        self.live = (0, 0);
        self.touch_all();
    }

    /// The GPU table entry of `slot` changed (High).
    fn touch(&mut self, slot: u32) {
        if let Some(table) = &mut self.table {
            table.touch(slot);
        }
    }

    /// Node `key` changed in a way its 6 face neighbors see: whether it is drawn by each of
    /// them decides which of their seam quads are left out ([`LodNodes::across`]).
    fn touch_around(&mut self, key: ChunkKey) {
        for face in Face::ALL {
            let pos = key.pos.offset(face.normal());
            let Some(&slot) = self.by_key.get(&ChunkKey { pos, ..key }) else {
                continue;
            };
            if let Some(node) = self.slots[slot as usize].as_mut()
                && !std::mem::replace(&mut node.stale, true)
            {
                self.stale.push(slot);
            }
        }
    }

    /// Brings [`LodDraw::across`] of the slots touched since the last call up to date (a
    /// changed one changes the GPU table entry too).
    fn refresh_across(&mut self) {
        for slot in std::mem::take(&mut self.stale) {
            let Some(node) = self.slots[slot as usize].as_ref() else {
                continue;
            };
            let across = self.across(node);
            let node = self.slots[slot as usize].as_mut().expect("a resident node");
            node.stale = false;
            if node.across != across {
                node.across = across;
                self.touch(slot);
            }
        }
    }

    /// Bit `f` set if the node of `node`'s level across face bucket `f` is drawn with its
    /// octants by that face (its quads up): the seam quads of `node` on that face lie between
    /// solid cells, behind that node's surface. (Past the GPU window, 2^30 blocks from the
    /// camera, a node is not drawn but still counts: nothing that far is seen.)
    fn across(&self, node: &LodDraw) -> u8 {
        let mut bits = 0;
        for (f, face) in Face::ALL.into_iter().enumerate() {
            let pos = node.key.pos.offset(face.normal());
            let Some(other) = self
                .by_key
                .get(&ChunkKey { pos, ..node.key })
                .and_then(|&s| self.slots[s as usize].as_ref())
            else {
                continue;
            };
            // Its octants on the side facing `node`: across face `f`, the ones that hold the
            // opposite face's cells.
            if other.pending.is_none() && other.mask & OCTANTS_BY_FACE[f ^ 1] == 0 {
                bits |= 1 << f;
            }
        }
        bits
    }

    /// Every GPU table entry changed (High).
    fn touch_all(&mut self) {
        if let Some(table) = &mut self.table {
            table.all = true;
        }
    }
}

/// The world block origin of node `key`, `pos << (5 + lod)`, `None` past `i64` (the world limit
/// keeps every real node far inside).
fn node_origin(key: ChunkKey) -> Option<I64Vec3> {
    let shift = 5 + u32::from(key.lod);
    let shl = |c: i64| {
        let o = c.checked_shl(shift)?;
        (o >> shift == c).then_some(o)
    };
    Some(I64Vec3::new(
        shl(key.pos.x)?,
        shl(key.pos.y)?,
        shl(key.pos.z)?,
    ))
}

/// The 6 face bucket counts of a node mesh, cut so they never cover more than its quads. LOD
/// meshes have nothing in the cutout and translucent buckets.
fn face_counts(mesh: &ChunkMesh) -> [u32; 6] {
    debug_assert!(
        mesh.bucket_counts[6..].iter().all(|&n| n == 0),
        "a LOD mesh with cutout or translucent quads"
    );
    let mut left = mesh.total_quads().min(mesh.quads.len() as u32);
    let mut faces = [0; 6];
    for (f, &n) in faces.iter_mut().zip(&mesh.bucket_counts[..6]) {
        *f = n.min(left);
        left -= *f;
    }
    faces
}

/// The octants (bit `x | y << 1 | z << 2`) that hold the cells by each node face, in face bucket
/// order (+Y, -Y, +X, -X, +Z, -Z).
const OCTANTS_BY_FACE: [u8; 6] = [0xCC, 0x33, 0xAA, 0x55, 0xF0, 0x0F];

/// The seam quads of each face bucket of a node mesh with `faces` (`boxel_mesh::lod::seam_counts`),
/// none for a mesh whose counts do not match its quads (never left out, then).
fn node_seams(mesh: &ChunkMesh, faces: [u32; 6]) -> [[u32; 2]; 6] {
    if mesh.total_quads() as usize != mesh.quads.len() {
        return [[0; 2]; 6];
    }
    let mut seams = seam_counts(mesh);
    for (s, &n) in seams.iter_mut().zip(&faces) {
        if s[0] + s[1] > n {
            *s = [0; 2];
        }
    }
    seams
}

impl LodNodes {
    /// Takes the waiting uploads, oldest first, up to `budget` bytes (at least one node; the
    /// rest wait for the next frame), in batches of at most `limit` bytes (a single larger node
    /// goes alone), with their bytes.
    fn take_uploads(&mut self, limit: u64, budget: u64) -> Vec<(Vec<Upload>, u64)> {
        let mut out = Vec::new();
        let (mut batch, mut bytes, mut total) = (Vec::new(), 0u64, 0u64);
        let waiting = std::mem::take(&mut self.pending);
        for (i, &slot) in waiting.iter().enumerate() {
            let Some(node) = self.slots.get_mut(slot as usize).and_then(Option::as_mut) else {
                continue;
            };
            if node.pending.is_none() {
                continue;
            }
            let size = u64::from(node.alloc.size) * QUAD_BYTES;
            if total > 0 && total + size > budget {
                self.pending.extend_from_slice(&waiting[i..]);
                break;
            }
            let Some(mesh) = node.pending.take() else {
                continue;
            };
            if bytes > 0 && bytes + size > limit {
                out.push((std::mem::take(&mut batch), bytes));
                bytes = 0;
            }
            bytes += size;
            total += size;
            if let Some(table) = &mut self.table {
                table.touch(slot);
            }
            batch.push(Upload {
                slot,
                mesh,
                offset: node.alloc.offset,
                quads: node.alloc.size,
            });
        }
        if bytes > 0 {
            out.push((batch, bytes));
        }
        let uploaded: Vec<u32> = out
            .iter()
            .flat_map(|(batch, _)| batch.iter().map(|up| up.slot))
            .collect();
        for slot in uploaded {
            self.recount(slot);
            // Drawn from this frame: its neighbors may leave their seams on that side out.
            if let Some(key) = self.slots[slot as usize].as_ref().map(|n| n.key) {
                self.touch_around(key);
            }
        }
        out
    }

    /// Uploads that could not be recorded wait for the next frame (their nodes, if still the
    /// same allocation, are not drawn until then).
    fn restore(&mut self, batch: Vec<Upload>) {
        for up in batch {
            let Some(node) = self
                .slots
                .get_mut(up.slot as usize)
                .and_then(Option::as_mut)
            else {
                continue;
            };
            if node.pending.is_none() && node.alloc.offset == up.offset {
                node.pending = Some(up.mesh);
                let key = node.key;
                self.pending.push(up.slot);
                self.recount(up.slot);
                if let Some(table) = &mut self.table {
                    table.touch(up.slot);
                }
                self.touch_around(key);
            }
        }
    }

    /// The immediates (and the GPU table's node) of `node`.
    fn immediate(&self, node: &LodDraw) -> LodImmediate {
        let origin = wrap_to_i32(node.origin);
        LodImmediate::new(
            [origin.x, origin.y, origin.z],
            node.key.lod,
            node.mask,
            self.color_mip,
            self.tint,
        )
    }

    /// The draw list for `view` (module docs). Empty while the last input does not draw LOD, or
    /// `sealed`.
    fn cull(&mut self, view: &ViewParams, sealed: bool) {
        self.refresh_across();
        self.draws.clear();
        self.stats.drawn = 0;
        self.stats.draws = 0;
        self.stats.quads = 0;
        if self.far.is_none() || sealed {
            return;
        }
        let (lo, hi) = (I64Vec3::splat(-WINDOW), I64Vec3::splat(WINDOW));
        for list in &self.levels {
            for &slot in list {
                let Some(node) = self.slots[slot as usize].as_ref() else {
                    continue;
                };
                // Fully hidden, or its quads are not in the arena yet.
                if node.mask == 0xFF || node.pending.is_some() {
                    continue;
                }
                self.stats.drawn += 1;
                // In i64 first, then to f32 (exact near the camera), as for chunks.
                let rel = node.origin.saturating_sub(view.render_origin);
                if !(rel.cmpgt(lo) & rel.cmplt(hi)).all() {
                    continue;
                }
                let min = rel.as_vec3();
                let max = min + Vec3::splat((32i64 << node.key.lod) as f32);
                if !view.frustum.intersects_aabb(&Aabb::new(min, max)) {
                    continue;
                }
                // The face buckets that can face the camera, without the seam quads nothing
                // shows (what High's cull keeps), in as few runs of the arena as they allow.
                let immediate = self.immediate(node);
                let mut start = node.alloc.offset;
                let mut run: Option<(u32, u32)> = None;
                for (face, &n) in node.faces.iter().enumerate() {
                    let quads = if n > 0 && bucket_visible(face, min, max, view.eye_rel) {
                        let across = node.across >> face & 1 != 0;
                        bits::bucket_quads(n, node.seams[face], face, node.mask, across)
                    } else {
                        0
                    };
                    if quads > 0 {
                        run = match run {
                            Some((first, len)) if first + len == start => {
                                Some((first, len + quads))
                            }
                            Some((first, len)) => {
                                self.draws.push(NodeDraw {
                                    immediate,
                                    first,
                                    quads: len,
                                });
                                Some((start, quads))
                            }
                            None => Some((start, quads)),
                        };
                        self.stats.quads += u64::from(quads);
                    }
                    start += n;
                }
                if let Some((first, quads)) = run {
                    self.draws.push(NodeDraw {
                        immediate,
                        first,
                        quads,
                    });
                }
            }
        }
        self.stats.draws = self.draws.len() as u32;
    }

    /// High: the frame's bookkeeping instead of [`LodNodes::cull`]. The GPU window follows
    /// `render_origin` (every entry is written again when it moves), and the stats count the
    /// drawn nodes and their quads (the GPU culls them). Returns the segments to draw, 0 while
    /// the last input does not draw LOD, or `sealed`.
    fn gpu_frame(&mut self, render_origin: I64Vec3, sealed: bool) -> u32 {
        if let Some(table) = &mut self.table {
            table.follow(render_origin);
        }
        self.draws.clear();
        self.stats.drawn = 0;
        self.stats.draws = 0;
        self.stats.quads = 0;
        if self.far.is_none() || sealed {
            return 0;
        }
        (self.stats.drawn, self.stats.quads) = self.live;
        if self.stats.drawn == 0 {
            return 0;
        }
        let segments = segments_for(self.slots.len());
        self.stats.draws = segments;
        segments
    }

    /// The GPU table entry of `slot`: live once its quads are up and while it lies inside the
    /// GPU window; zeroed when free.
    fn gpu_entry(&self, slot: u32, table: &GpuTable) -> LodNodeGpu {
        let Some(node) = self.slots.get(slot as usize).and_then(Option::as_ref) else {
            return LodNodeGpu::default();
        };
        let live = node.pending.is_none() && table.in_window(node.origin);
        LodNodeGpu::new(self.immediate(node), node.alloc.offset, node.faces, live)
            .with_seams(node.seams, node.across)
    }

    /// High: the table entries to write since the last call, as runs `(first slot, entries)`,
    /// over `capacity` slots (a new table of that size gets every entry).
    fn take_table_writes(&mut self, capacity: u32, fresh: bool) -> Vec<(u32, Vec<LodNodeGpu>)> {
        self.refresh_across();
        let Some(mut table) = self.table.take() else {
            return Vec::new();
        };
        let mut runs = Vec::new();
        if fresh || table.all {
            // Every entry the table may hold: the slots now and those dropped since (zeroed).
            let end = (self.slots.len() as u32).max(table.written).min(capacity);
            if end > 0 {
                runs.push(0..end);
            }
            table.written = self.slots.len() as u32;
        } else {
            table.dirty.sort_unstable();
            for &slot in &table.dirty {
                if slot >= capacity {
                    continue;
                }
                match runs.last_mut() {
                    Some(run) if slot < run.end => {}
                    Some(run) if slot - run.end <= TABLE_MERGE_GAP => run.end = slot + 1,
                    _ => runs.push(slot..slot + 1),
                }
            }
            table.written = table.written.max(self.slots.len() as u32);
        }
        table.clear();
        let writes = runs
            .into_iter()
            .map(|run| {
                let entries = run
                    .clone()
                    .map(|slot| self.gpu_entry(slot, &table))
                    .collect();
                (run.start, entries)
            })
            .collect();
        self.table = Some(table);
        writes
    }
}

/// Free slots between two changed ones that still go in one write of the GPU table.
const TABLE_MERGE_GAP: u32 = 16;

/// The segments of the High draw lists `slots` slots need (at least 1).
fn segments_for(slots: usize) -> u32 {
    (slots.max(1) as u32).div_ceil(bits::SEGMENT_SLOTS)
}

/// The CPU side of the GPU node table (High): which entries changed, and the GPU window (the
/// rule of `chunk_table`: only nodes whose wrapped origin difference is exact are live).
#[derive(Debug, Default)]
struct GpuTable {
    /// Changed slots since the last write (each once).
    dirty: Vec<u32>,
    /// Bit per slot: in `dirty`.
    dirty_bits: Vec<u64>,
    /// Every entry changed (a cleared pass, a new tint, a moved window).
    all: bool,
    /// The window's center.
    window: I64Vec3,
    /// Slots the GPU table holds entries for (the high-water mark of the writes).
    written: u32,
}

impl GpuTable {
    fn touch(&mut self, slot: u32) {
        let (word, bit) = (slot as usize / 64, 1u64 << (slot % 64));
        if self.dirty_bits.len() <= word {
            self.dirty_bits.resize(word + 1, 0);
        }
        if self.dirty_bits[word] & bit == 0 {
            self.dirty_bits[word] |= bit;
            self.dirty.push(slot);
        }
    }

    fn clear(&mut self) {
        for &slot in &self.dirty {
            self.dirty_bits[slot as usize / 64] = 0;
        }
        self.dirty.clear();
        self.all = false;
    }

    fn in_window(&self, origin: I64Vec3) -> bool {
        let c = self.window;
        origin.x.abs_diff(c.x) < GPU_WINDOW
            && origin.y.abs_diff(c.y) < GPU_WINDOW
            && origin.z.abs_diff(c.z) < GPU_WINDOW
    }

    /// Re-centers the window on `render_origin` past the slack: every entry is written again.
    fn follow(&mut self, render_origin: I64Vec3) {
        let c = self.window;
        if render_origin.x.abs_diff(c.x) > GPU_WINDOW_SLACK
            || render_origin.y.abs_diff(c.y) > GPU_WINDOW_SLACK
            || render_origin.z.abs_diff(c.z) > GPU_WINDOW_SLACK
        {
            self.window = render_origin;
            self.all = true;
        }
    }
}

/// Copies `batch` into `arena` through one staging buffer of `bytes`. `Err` if the staging buffer
/// could not be created (out of memory, a lost device: wgpu reported it; `fail` makes it so,
/// for tests): nothing is recorded.
fn copy(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    arena: &wgpu::Buffer,
    batch: &[Upload],
    bytes: u64,
    fail: bool,
) -> Result<(), wgpu::MapRangeError> {
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel lod uploads"),
        // Over every `max_buffer_size` (and unaligned): `fail`.
        size: if fail { u64::MAX } else { bytes },
        usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: true,
    });
    {
        let mut view = staging.slice(..).get_mapped_range_mut()?;
        let mut at = 0;
        for up in batch {
            let data: &[u8] = bytemuck::cast_slice(&up.mesh.quads[..up.quads as usize]);
            view.slice(at..at + data.len()).copy_from_slice(data);
            at += data.len();
        }
    }
    staging.unmap();
    let mut src = 0;
    for up in batch {
        let size = u64::from(up.quads) * QUAD_BYTES;
        encoder.copy_buffer_to_buffer(
            &staging,
            src,
            arena,
            u64::from(up.offset) * QUAD_BYTES,
            size,
        );
        src += size;
    }
    Ok(())
}

/// A bind group entry of all of `buffer`.
fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

/// Bytes of one indirect draw.
const ARGS_BYTES: u64 = size_of::<DrawIndexedIndirect>() as u64;

/// The GPU-driven path of High (module docs): the cull and pull pipelines and the lists.
struct GpuDriven {
    cull: wgpu::ComputePipeline,
    cull_layout: wgpu::BindGroupLayout,
    pull: wgpu::RenderPipeline,
    geometry_layout: wgpu::BindGroupLayout,
    hiz_pipeline: wgpu::ComputePipeline,
    hiz_layout: wgpu::BindGroupLayout,
    inputs: GpuInputs,
    /// Sized for some segments, rebuilt larger when the slots pass them.
    lists: Option<GpuLists>,
    /// The depth pyramid of the main targets it was made for.
    hiz: Option<HiZ>,
    /// The cull's bind group over `lists` and `hiz` (made again when either is).
    cull_group: Option<wgpu::BindGroup>,
    /// Segments to draw this frame (0: none); their cull is recorded after the solid terrain.
    segments: u32,
    /// Node slots the cull looks at this frame.
    slots: u32,
    /// Whether the cull uses the depth pyramid this frame, and what it hid lately.
    occlusion: Occlusion,
}

/// Frames that cull without the depth pyramid after a sample in which it hid under 1 in
/// [`OCCLUSION_RATIO`] of the nodes in the frustum; then the pyramid is sampled again.
pub(crate) const OCCLUSION_SKIP_FRAMES: u32 = 30;
/// See [`OCCLUSION_SKIP_FRAMES`].
pub(crate) const OCCLUSION_RATIO: u32 = 16;
/// Readbacks of the cull's occlusion counts in flight at most.
const OCCLUSION_SAMPLES: usize = 3;

/// The depth pyramid's use (module docs). A frame that culls with it counts the nodes in the
/// frustum and those it hid (`lod_cull.wgsl`) and copies the counts to a free readback buffer,
/// mapped after that frame is submitted and read a frame or two later, never waited for.
struct Occlusion {
    /// `[nodes in the frustum, hidden by the pyramid, cull with the pyramid (written by the
    /// CPU), unused]`.
    counters: wgpu::Buffer,
    samples: Vec<OcclusionSample>,
    /// Frames left that cull without the pyramid.
    skip: u32,
    /// This frame culls with the pyramid.
    on: bool,
}

/// One readback buffer of [`Occlusion`].
struct OcclusionSample {
    buffer: wgpu::Buffer,
    /// [`SAMPLE_IDLE`], [`SAMPLE_COPIED`], [`SAMPLE_MAPPING`] or [`SAMPLE_READY`].
    state: Arc<AtomicU8>,
}

/// Free.
const SAMPLE_IDLE: u8 = 0;
/// The counts are copied into it by a frame not known to be submitted yet.
const SAMPLE_COPIED: u8 = 1;
/// Mapping (the frame was submitted).
const SAMPLE_MAPPING: u8 = 2;
/// Mapped: the counts can be read.
const SAMPLE_READY: u8 = 3;

/// Frames to cull without the depth pyramid after a sample of `in_frustum` nodes of which it hid
/// `hidden` (module docs).
fn occlusion_skip(in_frustum: u32, hidden: u32) -> u32 {
    if u64::from(hidden) * u64::from(OCCLUSION_RATIO) <= u64::from(in_frustum) {
        OCCLUSION_SKIP_FRAMES
    } else {
        0
    }
}

impl Occlusion {
    fn new(device: &wgpu::Device) -> Self {
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        Self {
            counters: buffer(
                "boxel lod occlusion",
                16,
                wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
            ),
            samples: (0..OCCLUSION_SAMPLES)
                .map(|_| OcclusionSample {
                    buffer: buffer(
                        "boxel lod occlusion read",
                        8,
                        wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    ),
                    state: Arc::new(AtomicU8::new(SAMPLE_IDLE)),
                })
                .collect(),
            skip: 0,
            on: true,
        }
    }

    /// At the start of a frame (the last one is submitted): maps the samples copied since,
    /// reads the ones mapped, then decides whether this frame culls with the pyramid and
    /// records the reset of the counts.
    fn begin_frame(&mut self, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder) {
        for sample in &self.samples {
            match sample.state.load(Ordering::Acquire) {
                SAMPLE_COPIED => {
                    sample.state.store(SAMPLE_MAPPING, Ordering::Release);
                    let state = Arc::clone(&sample.state);
                    sample
                        .buffer
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |r| {
                            let next = if r.is_ok() { SAMPLE_READY } else { SAMPLE_IDLE };
                            state.store(next, Ordering::Release);
                        });
                }
                SAMPLE_READY => {
                    let counts = sample.buffer.slice(..).get_mapped_range().ok().map(|m| {
                        let c: &[u32] = bytemuck::cast_slice(&m);
                        (c[0], c[1])
                    });
                    sample.buffer.unmap();
                    sample.state.store(SAMPLE_IDLE, Ordering::Release);
                    if let Some((in_frustum, hidden)) = counts {
                        self.skip = occlusion_skip(in_frustum, hidden);
                    }
                }
                _ => {}
            }
        }
        self.on = self.skip == 0;
        self.skip = self.skip.saturating_sub(1);
        queue.write_buffer(&self.counters, 8, bytemuck::bytes_of(&u32::from(self.on)));
        encoder.clear_buffer(&self.counters, 0, Some(8));
    }

    /// After a cull with the pyramid: its counts go to a free sample, if any.
    fn copy_counts(&self, encoder: &mut wgpu::CommandEncoder) {
        let Some(sample) = self
            .samples
            .iter()
            .find(|s| s.state.load(Ordering::Acquire) == SAMPLE_IDLE)
        else {
            return;
        };
        encoder.copy_buffer_to_buffer(&self.counters, 0, &sample.buffer, 0, 8);
        sample.state.store(SAMPLE_COPIED, Ordering::Release);
    }
}

/// The depth pyramid (`lod_hiz.wgsl`) of one pair of main targets.
struct HiZ {
    /// [`crate::targets::Targets::generation`] of the targets.
    generation: u64,
    /// Every level, for the cull.
    view: wgpu::TextureView,
    /// Level `m` from level `m - 1` (the depth for level 0), and its size.
    levels: Vec<(wgpu::BindGroup, [u32; 2])>,
}

/// The node table and the draw lists, for `segments` segments.
struct GpuLists {
    segments: u32,
    table: wgpu::Buffer,
    args: wgpu::Buffer,
    refs: wgpu::Buffer,
    /// One draw count per segment (cleared before every cull).
    counts: wgpu::Buffer,
    /// Group [`bits::GROUP_GEOMETRY`] of the pull pipeline: the LOD arena and the table.
    geometry: wgpu::BindGroup,
}

impl GpuDriven {
    fn new(
        device: &wgpu::Device,
        materials: &wgpu::BindGroupLayout,
        view: &wgpu::BindGroupLayout,
        inputs: GpuInputs,
    ) -> Self {
        let texture = |binding, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel lod cull"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(1, true),
                storage(2, false),
                storage(3, false),
                storage(4, false),
                texture(5, wgpu::ShaderStages::COMPUTE),
                storage(6, false),
            ],
        });
        let module = create_module(device, ShaderModuleId::LodCull);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(ShaderModuleId::LodCull.label()),
            bind_group_layouts: &[Some(&cull_layout)],
            immediate_size: 0,
        });
        let cull = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("boxel lod cull"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("cull_lod"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let vertex_storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let geometry_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel lod geometry"),
            entries: &[vertex_storage(0), vertex_storage(1)],
        });
        let hiz_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel lod hiz"),
            entries: &[
                texture(0, wgpu::ShaderStages::COMPUTE),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let module = create_module(device, ShaderModuleId::LodHiz);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(ShaderModuleId::LodHiz.label()),
            bind_group_layouts: &[Some(&hiz_layout)],
            immediate_size: 0,
        });
        let hiz_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("boxel lod hiz"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("hiz_reduce"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self {
            cull,
            cull_layout,
            pull: pull_pipeline(device, materials, view, &geometry_layout),
            geometry_layout,
            hiz_pipeline,
            hiz_layout,
            inputs,
            lists: None,
            hiz: None,
            cull_group: None,
            segments: 0,
            slots: 0,
            occlusion: Occlusion::new(device),
        }
    }

    /// The depth pyramid of `targets`: level 0 half their size, each level half the one below
    /// (rounded down, as the texture's mips), until the longer side is 1.
    fn hiz(&self, device: &wgpu::Device, targets: &Targets) -> HiZ {
        let base = [(targets.size.x / 2).max(1), (targets.size.y / 2).max(1)];
        let count = base[0].max(base[1]).ilog2() + 1;
        let sizes: Vec<[u32; 2]> = (0..count)
            .map(|m| [(base[0] >> m).max(1), (base[1] >> m).max(1)])
            .collect();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("boxel lod hiz"),
            size: wgpu::Extent3d {
                width: sizes[0][0],
                height: sizes[0][1],
                depth_or_array_layers: 1,
            },
            mip_level_count: sizes.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let level = |m: usize| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: m as u32,
                mip_level_count: Some(1),
                ..wgpu::TextureViewDescriptor::default()
            })
        };
        let levels = (0..sizes.len())
            .map(|m| {
                let source = if m == 0 {
                    targets.depth_view.clone()
                } else {
                    level(m - 1)
                };
                let target = level(m);
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("boxel lod hiz"),
                    layout: &self.hiz_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&source),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&target),
                        },
                    ],
                });
                (group, sizes[m])
            })
            .collect();
        HiZ {
            generation: targets.generation,
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            levels,
        }
    }

    /// The lists for `segments` segments.
    fn lists(&self, device: &wgpu::Device, arena: &wgpu::Buffer, segments: u32) -> GpuLists {
        let buffer = |label, size: u64, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let slots = u64::from(segments * bits::SEGMENT_SLOTS);
        let draws = u64::from(segments * bits::SEGMENT_DRAWS);
        let table = buffer(
            "boxel lod nodes",
            slots * size_of::<LodNodeGpu>() as u64,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let args = buffer(
            "boxel lod draw args",
            draws * ARGS_BYTES,
            wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::STORAGE,
        );
        let refs = buffer(
            "boxel lod draw refs",
            draws * size_of::<DrawRef>() as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::STORAGE,
        );
        let counts = buffer(
            "boxel lod draw counts",
            4 * u64::from(segments),
            wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
        );
        let geometry = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("boxel lod geometry"),
            layout: &self.geometry_layout,
            entries: &[buffer_entry(0, arena), buffer_entry(1, &table)],
        });
        GpuLists {
            segments,
            table,
            args,
            refs,
            counts,
            geometry,
        }
    }

    /// Writes the changed table entries and clears the draw counts of `segments` segments (none:
    /// no cull, nothing drawn). The cull is recorded here when it goes without the depth
    /// pyramid this frame ([`Occlusion`]), else by [`GpuDriven::cull_after_solid`].
    #[expect(
        clippy::too_many_arguments,
        reason = "the frame's GPU state, passed down"
    )]
    fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        targets: &Targets,
        arena: &wgpu::Buffer,
        nodes: &mut LodNodes,
        segments: u32,
    ) {
        self.occlusion.begin_frame(queue, encoder);
        let needed = segments_for(nodes.slots.len());
        let fresh = self.lists.as_ref().is_none_or(|l| l.segments < needed);
        if fresh {
            self.lists = Some(self.lists(device, arena, needed));
            self.cull_group = None;
        }
        let Some(lists) = &self.lists else {
            return;
        };
        let capacity = lists.segments * bits::SEGMENT_SLOTS;
        for (first, entries) in nodes.take_table_writes(capacity, fresh) {
            queue.write_buffer(
                &lists.table,
                u64::from(first) * size_of::<LodNodeGpu>() as u64,
                bytemuck::cast_slice(&entries),
            );
        }
        self.segments = segments.min(lists.segments);
        self.slots = (nodes.slots.len() as u32).min(capacity);
        if self.segments > 0 {
            encoder.clear_buffer(&lists.counts, 0, None);
            if !self.occlusion.on {
                self.cull(device, encoder, targets);
            }
        }
    }

    /// True if this frame's cull waits for the solid terrain ([`LodPass::culls_after_solid`]).
    fn culls_after_solid(&self) -> bool {
        self.segments > 0 && self.occlusion.on
    }

    /// Records, after the opaque and cutout terrain is drawn into `targets`, the depth pyramid
    /// of their depth and the cull of the frame's segments against it (none: nothing), and
    /// samples what it hid.
    fn cull_after_solid(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        targets: &Targets,
    ) {
        if self.culls_after_solid() {
            self.cull(device, encoder, targets);
            self.occlusion.copy_counts(encoder);
        }
    }

    /// Records the cull of the frame's segments, after the depth pyramid of `targets` when it
    /// is used this frame.
    fn cull(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        targets: &Targets,
    ) {
        if self
            .hiz
            .as_ref()
            .is_none_or(|h| h.generation != targets.generation)
        {
            self.hiz = Some(self.hiz(device, targets));
            self.cull_group = None;
        }
        let (Some(lists), Some(hiz)) = (&self.lists, &self.hiz) else {
            return;
        };
        let cull_group = self.cull_group.get_or_insert_with(|| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("boxel lod cull"),
                layout: &self.cull_layout,
                entries: &[
                    buffer_entry(0, &self.inputs.view),
                    buffer_entry(1, &lists.table),
                    buffer_entry(2, &lists.args),
                    buffer_entry(3, &lists.refs),
                    buffer_entry(4, &lists.counts),
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&hiz.view),
                    },
                    buffer_entry(6, &self.occlusion.counters),
                ],
            })
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("boxel lod cull"),
            timestamp_writes: None,
        });
        if self.occlusion.on {
            pass.set_pipeline(&self.hiz_pipeline);
            for (group, [w, h]) in &hiz.levels {
                pass.set_bind_group(0, group, &[]);
                pass.dispatch_workgroups(
                    w.div_ceil(bits::HIZ_WORKGROUP_EDGE),
                    h.div_ceil(bits::HIZ_WORKGROUP_EDGE),
                    1,
                );
            }
        }
        pass.set_pipeline(&self.cull);
        pass.set_bind_group(0, &*cull_group, &[]);
        pass.dispatch_workgroups(self.slots.div_ceil(bits::CULL_WORKGROUP_SIZE), 1, 1);
    }

    /// See [`LodPass::draw`].
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        let Some(lists) = self.lists.as_ref().filter(|_| self.segments > 0) else {
            return 0;
        };
        pass.set_pipeline(&self.pull);
        pass.set_bind_group(bits::GROUP_GEOMETRY, &lists.geometry, &[]);
        pass.set_vertex_buffer(0, lists.refs.slice(..));
        pass.set_index_buffer(self.inputs.index.slice(..), wgpu::IndexFormat::Uint32);
        for segment in 0..self.segments {
            pass.multi_draw_indexed_indirect_count(
                &lists.args,
                u64::from(segment * bits::SEGMENT_DRAWS) * ARGS_BYTES,
                &lists.counts,
                4 * u64::from(segment),
                bits::SEGMENT_DRAWS,
            );
        }
        self.segments
    }
}

/// The pull pipeline of High: `vs_lod_pull` and `fs_lod` (module `LodPull`), layout
/// `[materials, view, -, lod geometry]`, the `DrawRef` as one instance-step `Uint32x2` attribute,
/// indexed triangle lists; otherwise the state of [`pipeline`].
fn pull_pipeline(
    device: &wgpu::Device,
    materials: &wgpu::BindGroupLayout,
    view: &wgpu::BindGroupLayout,
    geometry: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let module = create_module(device, ShaderModuleId::LodPull);
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(ShaderModuleId::LodPull.label()),
        bind_group_layouts: &[Some(materials), Some(view), None, Some(geometry)],
        immediate_size: 0,
    });
    render_pipeline(
        device,
        &layout,
        &module,
        "vs_lod_pull",
        size_of::<DrawRef>() as u64,
        wgpu::PrimitiveTopology::TriangleList,
    )
}

/// The LOD pipeline: `vs_lod` and `fs_lod` (module `Lod`), layout `[materials, view]` and 16
/// bytes of immediates, the quad as one instance-step `Uint32x2` attribute, triangle strips,
/// back faces culled, depth `Greater` with writes, no blending.
fn pipeline(
    device: &wgpu::Device,
    materials: &wgpu::BindGroupLayout,
    view: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let module = create_module(device, ShaderModuleId::Lod);
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(ShaderModuleId::Lod.label()),
        bind_group_layouts: &[Some(materials), Some(view)],
        immediate_size: bits::IMMEDIATE_SIZE,
    });
    render_pipeline(
        device,
        &layout,
        &module,
        "vs_lod",
        QUAD_BYTES,
        wgpu::PrimitiveTopology::TriangleStrip,
    )
}

/// A LOD render pipeline: vertex entry `entry` with one instance-step `Uint32x2` attribute of
/// `stride` bytes, `fs_lod`, `topology`, back faces culled, depth `Greater` with writes, no
/// blending.
fn render_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    entry: &str,
    stride: u64,
    topology: wgpu::PrimitiveTopology,
) -> wgpu::RenderPipeline {
    let attributes = [wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Uint32x2,
        offset: 0,
        shader_location: 0,
    }];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("boxel lod"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: stride,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &attributes,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_lod"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: MAIN_COLOR_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_camera::{Camera, CameraView};
    use boxel_math::DVec3;
    use boxel_mesh::lod::Seam;
    use boxel_mesh::{BUCKET_COUNT, PackedQuad};
    use boxel_transform::Transform;
    use boxel_voxel::{ChunkPos, DimensionId, Face};
    use boxel_world::lod::LodSettings;

    fn key(lod: u8, x: i64, y: i64, z: i64) -> ChunkKey {
        ChunkKey {
            dim: DimensionId(0),
            lod,
            pos: ChunkPos::new(x, y, z),
        }
    }

    /// A node mesh with `faces[f]` quads in face bucket `f`.
    fn mesh(faces: [u32; 6]) -> Arc<ChunkMesh> {
        let mut quads = Vec::new();
        let mut counts = [0; BUCKET_COUNT];
        for (f, &n) in faces.iter().enumerate() {
            for i in 0..n {
                quads.push(PackedQuad::new([i % 32, 3, 5], 1, 1, Face::ALL[f], 1));
            }
            counts[f] = n;
        }
        Arc::new(ChunkMesh::from_quads(quads, counts))
    }

    fn input(ops: Vec<LodOp>) -> LodInput {
        LodInput {
            ops,
            ..LodInput::new(LodSettings::default(), Some(12288.0))
        }
    }

    fn upsert(key: ChunkKey, mesh: Arc<ChunkMesh>, mask: u8) -> LodOp {
        LodOp::Upsert { key, mesh, mask }
    }

    /// Camera at `eye` looking along `-Z` rotated by yaw/pitch (radians).
    fn camera_view(eye: DVec3, yaw: f32, pitch: f32) -> ViewParams {
        let camera = Camera::default();
        let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
        let cv = CameraView::compute(&camera, &transform, 16.0 / 9.0);
        let mut view = ViewParams::from_camera(&cv, &camera, [0.0; 4], DistanceFog::default());
        crate::renderer::far_plane_at(&mut view.frustum, view.eye_rel, 12288.0);
        view
    }

    /// The level lists, the key map and the slots agree; the allocator holds exactly the nodes'
    /// quads, and the stats say so.
    fn check(nodes: &LodNodes) {
        let mut listed = 0;
        for (level, list) in nodes.levels.iter().enumerate() {
            for (at, &slot) in list.iter().enumerate() {
                let node = nodes.slots[slot as usize].as_ref().expect("a listed node");
                assert_eq!((node.level(), node.at as usize), (level, at));
                assert_eq!(nodes.by_key.get(&node.key), Some(&slot));
                listed += 1;
            }
        }
        assert_eq!(listed, nodes.by_key.len());
        let held: u32 = nodes.slots.iter().flatten().map(|n| n.alloc.size).sum();
        assert_eq!(nodes.alloc.used(), held);
        assert_eq!(nodes.stats.nodes as usize, nodes.by_key.len());
        assert_eq!(nodes.stats.arena_used, held);
        assert_eq!(nodes.stats.arena_capacity, nodes.alloc.capacity());
        // The live count kept as nodes change is the walk over every slot.
        let live = nodes
            .slots
            .iter()
            .flatten()
            .filter(|n| n.mask != 0xFF && n.pending.is_none())
            .fold((0, 0), |(n, q), node| {
                (n + 1, q + u64::from(node.alloc.size))
            });
        assert_eq!(nodes.live, live);
    }

    #[test]
    fn ops_keep_the_records_and_the_arena_in_step() {
        let mut nodes = LodNodes::new(1 << 12, 1 << 12, 4);
        let (a, b, c) = (key(1, 0, 0, 0), key(2, -3, 1, 7), key(1, 5, -1, 2));
        nodes.apply(input(vec![
            upsert(a, mesh([4, 0, 2, 0, 0, 1]), 0),
            upsert(b, mesh([1; 6]), 0b11),
            upsert(c, mesh([3, 0, 0, 0, 0, 0]), 0),
        ]));
        check(&nodes);
        assert_eq!(nodes.by_key.len(), 3);
        assert_eq!(nodes.alloc.used(), 7 + 6 + 3);
        // A new mesh replaces the old one; a mask changes only the mask; unknown keys do nothing.
        nodes.apply(input(vec![
            upsert(a, mesh([1, 1, 0, 0, 0, 0]), 0),
            LodOp::Mask { key: b, mask: 0xF0 },
            LodOp::Mask {
                key: key(3, 0, 0, 0),
                mask: 1,
            },
            LodOp::Remove {
                key: key(3, 0, 0, 0),
            },
            LodOp::Remove { key: c },
        ]));
        check(&nodes);
        assert_eq!(nodes.alloc.used(), 2 + 6);
        let b_node = nodes.by_key[&b];
        assert_eq!(
            nodes.slots[b_node as usize].as_ref().map(|n| n.mask),
            Some(0xF0)
        );
        // The waiting uploads hold each node's newest mesh once, never a removed node's.
        let uploads = nodes.take_uploads(u64::MAX, u64::MAX);
        let sent: Vec<u32> = uploads[0].0.iter().map(|u| u.quads).collect();
        assert_eq!(sent, [2, 6]);
        assert!(nodes.take_uploads(u64::MAX, u64::MAX).is_empty());
        // An empty mesh removes the node; a resync drops everything before its ops.
        nodes.apply(input(vec![upsert(a, mesh([0; 6]), 0)]));
        check(&nodes);
        assert_eq!(nodes.by_key.len(), 1);
        nodes.apply(LodInput {
            resync: true,
            ..input(vec![upsert(c, mesh([2; 6]), 0)])
        });
        check(&nodes);
        assert_eq!(nodes.by_key.keys().collect::<Vec<_>>(), [&c]);
        assert_eq!(nodes.alloc.used(), 12);
    }

    #[test]
    fn uploads_come_in_batches_under_the_limit() {
        let mut nodes = LodNodes::new(1 << 12, 1 << 12, 4);
        let ops = (0..5)
            .map(|i| upsert(key(1, i, 0, 0), mesh([10, 0, 0, 0, 0, 0]), 0))
            .collect();
        nodes.apply(input(ops));
        let batches = nodes.take_uploads(25 * QUAD_BYTES, u64::MAX);
        let sizes: Vec<(usize, u64)> = batches.iter().map(|(b, n)| (b.len(), *n)).collect();
        assert_eq!(sizes, [(2, 160), (2, 160), (1, 80)]);
        // A node larger than the limit goes alone.
        nodes.apply(input(vec![upsert(
            key(1, 9, 0, 0),
            mesh([40, 0, 0, 0, 0, 0]),
            0,
        )]));
        let batches = nodes.take_uploads(25 * QUAD_BYTES, u64::MAX);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].1, 40 * QUAD_BYTES);
    }

    /// After frames that drew nothing (a hidden window: `Prepare` applied their ops, no
    /// `render` uploaded), a frame uploads at most its budget, oldest first; a node is drawn
    /// only once its quads are up.
    #[test]
    fn a_backlog_of_uploads_goes_up_over_frames() {
        let mut nodes = LodNodes::new(1 << 12, 1 << 12, 4);
        let view = camera_view(DVec3::new(0.0, 100.0, 0.0), 0.0, -0.3);
        let mut settings = input(Vec::new());
        // 10 quads a frame from the world side: 40 quads (4 nodes) uploaded a frame.
        settings.settings.upload_bytes_per_frame = 10 * QUAD_BYTES as usize;
        for i in 0..10 {
            let mut frame = settings.clone();
            frame.ops = vec![upsert(key(1, 0, 0, -3 - i), mesh([10, 0, 0, 0, 0, 0]), 0)];
            nodes.apply(frame);
        }
        assert_eq!(nodes.upload_budget, 40 * QUAD_BYTES);
        for (up, drawn) in [(40, 4), (40, 8), (20, 10)] {
            let batches = nodes.take_uploads(STAGING_BYTES, nodes.upload_budget);
            let quads: u32 = batches.iter().flat_map(|(b, _)| b).map(|u| u.quads).sum();
            assert_eq!(quads, up, "a frame's uploads");
            nodes.cull(&view, false);
            assert_eq!(nodes.draws.len(), drawn, "the nodes up are drawn");
        }
        assert!(
            nodes
                .take_uploads(STAGING_BYTES, nodes.upload_budget)
                .is_empty()
        );
        // One node over the budget still goes up, alone.
        nodes.apply(LodInput {
            ops: vec![upsert(key(1, 1, 0, -3), mesh([200, 0, 0, 0, 0, 0]), 0)],
            ..settings
        });
        let batches = nodes.take_uploads(STAGING_BYTES, nodes.upload_budget);
        assert_eq!(batches.iter().map(|(b, _)| b.len()).sum::<usize>(), 1);
        check(&nodes);
    }

    /// Uploads whose staging buffer could not be created wait for the next frame: their nodes are
    /// not drawn meanwhile (their range holds nothing yet, or another node's old quads), and go
    /// up again as they are. A node removed or replaced meanwhile is not restored.
    #[test]
    fn lost_uploads_go_up_the_next_frame() {
        let mut nodes = LodNodes::new(1 << 12, 1 << 12, 4);
        let view = camera_view(DVec3::new(0.0, 100.0, 0.0), 0.0, -0.3);
        let (a, b, c) = (key(1, -1, 0, -3), key(1, 0, 0, -3), key(1, 1, 0, -3));
        nodes.apply(input(vec![
            upsert(a, mesh([4, 0, 0, 0, 0, 0]), 0),
            upsert(b, mesh([5, 0, 0, 0, 0, 0]), 0),
            upsert(c, mesh([6, 0, 0, 0, 0, 0]), 0),
        ]));
        let lost = nodes.take_uploads(STAGING_BYTES, u64::MAX);
        // Meanwhile `b` is removed and `c` gets a new mesh.
        nodes.apply(input(vec![
            LodOp::Remove { key: b },
            upsert(c, mesh([7, 0, 0, 0, 0, 0]), 0),
        ]));
        for (batch, _) in lost {
            nodes.restore(batch);
        }
        nodes.cull(&view, false);
        assert!(nodes.draws.is_empty(), "nothing is up yet");
        let again = nodes.take_uploads(STAGING_BYTES, u64::MAX);
        let quads: Vec<u32> = again.iter().flat_map(|(b, _)| b).map(|u| u.quads).collect();
        assert_eq!(quads, [7, 4], "c's new mesh, a again; b is gone");
        nodes.cull(&view, false);
        assert_eq!(nodes.draws.len(), 2);
        check(&nodes);
    }

    #[test]
    fn a_full_arena_refuses_and_drops_the_older_mesh() {
        let mut nodes = LodNodes::new(100, 100, 4);
        let (a, b) = (key(1, 0, 0, 0), key(1, 1, 0, 0));
        nodes.apply(input(vec![
            upsert(a, mesh([60, 0, 0, 0, 0, 0]), 0),
            upsert(b, mesh([60, 0, 0, 0, 0, 0]), 0),
            // The renderer refused `b`: the mask that follows its upsert is ignored.
            LodOp::Mask { key: b, mask: 3 },
        ]));
        check(&nodes);
        assert_eq!(
            nodes.report(),
            LodReport {
                refused: vec![b],
                arena_free_quads: 40
            }
        );
        assert!(!nodes.by_key.contains_key(&b));
        assert!(nodes.report().refused.is_empty(), "reported once");
        // A larger mesh for a resident node that does not fit takes the node off the renderer.
        nodes.apply(input(vec![upsert(a, mesh([90, 30, 0, 0, 0, 0]), 0)]));
        check(&nodes);
        assert!(nodes.by_key.is_empty());
        assert_eq!(nodes.stats.refused, 2);
        assert_eq!(nodes.report().arena_free_quads, 100);
        // Room again: it fits.
        nodes.apply(input(vec![upsert(b, mesh([60, 0, 0, 0, 0, 0]), 0)]));
        assert!(nodes.by_key.contains_key(&b));
        assert!(nodes.report().refused.is_empty());
    }

    #[test]
    fn keys_that_are_no_lod_node_are_refused() {
        let mut nodes = LodNodes::new(100, 100, 4);
        let bad = [key(0, 0, 0, 0), key(16, 0, 0, 0), key(1, 1 << 58, 0, 0)];
        nodes.apply(input(
            bad.iter()
                .map(|&k| upsert(k, mesh([1, 0, 0, 0, 0, 0]), 0))
                .collect(),
        ));
        assert!(nodes.by_key.is_empty());
        assert_eq!(nodes.report().refused, bad);
        // Far from the origin (ADR 0014): exact origins up to the world limit.
        for (pos, lod) in [(1i64 << 25, 6u8), (-(1i64 << 40) >> 6, 1), (-1, 15)] {
            let k = key(lod, pos, -pos, 3);
            assert_eq!(
                node_origin(k),
                Some(I64Vec3::new(pos, -pos, 3) * (32i64 << lod))
            );
        }
    }

    #[test]
    fn the_cull_keeps_what_the_camera_may_see_level_1_first() {
        let mut nodes = LodNodes::new(1 << 12, 1 << 12, 4);
        // Looking along -Z from (0, 100, 0), level about horizontal.
        let view = camera_view(DVec3::new(0.0, 100.0, 0.0), 0.0, 0.0);
        let ahead2 = key(2, 0, 0, -3);
        let ahead1 = key(1, -1, 0, -2); // its -Z faces look away: not drawn
        let behind = key(1, 0, 0, 4);
        let tops_from_below = key(1, 0, 3, -3); // y 192..256: above the eye, +Y faces only
        let hidden = key(1, 1, 0, -2);
        let far_away = key(1, 1 << 30, 0, -2);
        nodes.apply(input(vec![
            upsert(ahead2, mesh([2, 0, 0, 0, 0, 0]), 0),
            upsert(ahead1, mesh([3, 0, 0, 0, 0, 1]), 0b0000_0001),
            upsert(behind, mesh([1; 6]), 0),
            upsert(tops_from_below, mesh([5, 0, 0, 0, 0, 0]), 0),
            upsert(hidden, mesh([1; 6]), 0xFF),
            upsert(far_away, mesh([1; 6]), 0),
        ]));
        nodes.take_uploads(STAGING_BYTES, u64::MAX);
        nodes.cull(&view, false);
        let drawn: Vec<(i32, u32)> = nodes
            .draws
            .iter()
            .map(|d| (d.immediate.node[0] & 15, d.quads))
            .collect();
        assert_eq!(drawn, [(1, 3), (2, 2)], "level 1 first");
        assert_eq!(nodes.stats.draws, 2);
        assert_eq!(nodes.stats.quads, 5);
        // Every node but the fully hidden one has an octant to draw.
        assert_eq!(nodes.stats.drawn, 5);
        let d = nodes.draws[0];
        assert_eq!(
            d.immediate,
            LodImmediate::new([-64, 0, -128], 1, 0b0000_0001, 4, false)
        );
        // Nothing while the input does not draw LOD (disabled, or no horizon).
        let mut off = input(Vec::new());
        off.settings.enabled = false;
        nodes.apply(off);
        nodes.cull(&view, false);
        assert!(nodes.draws.is_empty());
        assert_eq!(
            nodes.stats,
            LodPassStats {
                nodes: 6,
                arena_used: nodes.alloc.used(),
                arena_capacity: nodes.alloc.capacity(),
                ..LodPassStats::default()
            }
        );
        nodes.apply(LodInput::new(LodSettings::default(), None));
        nodes.cull(&view, false);
        assert!(nodes.draws.is_empty());
        assert_eq!(nodes.fog_params(), None);
        // The tint, and the distance fog's rule at the horizon: the last 10 %, at most 64
        // blocks.
        let mut tinted = input(Vec::new());
        tinted.settings.debug_tint = true;
        nodes.apply(tinted);
        nodes.cull(&view, false);
        assert!(
            nodes
                .draws
                .iter()
                .all(|d| d.immediate.node[3] as u32 & bits::TINT_BIT != 0)
        );
        assert_eq!(nodes.fog_params(), Some([12224.0, 12288.0, 0.0, 0.0]));
        // The game's numbers, and the fog off: no fade at the horizon either.
        let longer = LodInput {
            fog: DistanceFog {
                fade_fraction: 0.75,
                max_fade: 1e6,
                ..DistanceFog::default()
            },
            ..input(Vec::new())
        };
        nodes.apply(longer);
        assert_eq!(nodes.fog_params(), Some([3072.0, 12288.0, 0.0, 0.0]));
        nodes.apply(LodInput {
            fog: DistanceFog::off(),
            ..input(Vec::new())
        });
        assert_eq!(nodes.fog_params(), Some([12288.0, 12288.0, 0.0, 0.0]));
    }

    #[test]
    fn the_cull_is_exact_far_from_the_origin() {
        // The same scene shifted by 2^40 blocks (render origin with it) draws the same nodes.
        for shift in [0i64, 1 << 31, -(1 << 40), (1 << 40) + 512] {
            let mut nodes = LodNodes::new(1 << 12, 1 << 12, 4);
            let mut view = camera_view(DVec3::new(0.0, 100.0, 0.0), 0.3, -0.2);
            view.render_origin += I64Vec3::new(shift, 0, -shift);
            let k = |lod: u8, x: i64, z: i64| {
                key(lod, x + (shift >> (5 + lod)), 0, z - (shift >> (5 + lod)))
            };
            nodes.apply(input(vec![
                upsert(k(1, -2, -3), mesh([1; 6]), 0),
                upsert(k(2, -1, -2), mesh([2; 6]), 0),
                upsert(k(1, 3, 3), mesh([1; 6]), 0),
            ]));
            nodes.take_uploads(STAGING_BYTES, u64::MAX);
            nodes.cull(&view, false);
            // The +Y, +X and +Z buckets of the two nodes ahead, one draw each, but the eye is
            // inside the level-2 node's height: its -Y bucket too, one run from +Y to +X.
            let drawn: Vec<u32> = nodes.draws.iter().map(|d| d.quads).collect();
            assert_eq!(drawn, [1, 1, 1, 6, 2], "shift {shift}");
        }
    }

    /// Underground (`sealed`: the cave culling search leaves no line of sight out of its box)
    /// nothing is drawn, on both paths.
    #[test]
    fn a_sealed_search_draws_nothing() {
        let view = camera_view(DVec3::new(0.0, 100.0, 0.0), 0.0, -0.3);
        for gpu in [false, true] {
            let mut nodes = LodNodes::new(1 << 12, 1 << 12, 4);
            if gpu {
                nodes.table = Some(GpuTable::default());
            }
            nodes.apply(input(vec![upsert(key(1, 0, 0, -3), mesh([3; 6]), 0)]));
            nodes.take_uploads(STAGING_BYTES, u64::MAX);
            let frame = |nodes: &mut LodNodes, sealed| {
                if gpu {
                    nodes.gpu_frame(view.render_origin, sealed)
                } else {
                    nodes.cull(&view, sealed);
                    nodes.draws.len() as u32
                }
            };
            // Instanced: the +Y bucket, then -X and +Z in one run; High: one segment, every
            // quad of the drawn node counted (its cull is on the GPU).
            let (draws, quads) = if gpu { (1, 18) } else { (2, 9) };
            assert_eq!(frame(&mut nodes, false), draws, "gpu {gpu}");
            assert_eq!(nodes.stats.quads, quads);
            assert_eq!(frame(&mut nodes, true), 0, "gpu {gpu}");
            assert_eq!((nodes.stats.draws, nodes.stats.quads), (0, 0));
        }
    }

    /// The GPU table (High): each changed entry is written once, in runs; a node is live only
    /// once its quads are up and inside the GPU window; a removed node's entry is zeroed; a new
    /// tint, a cleared pass or a moved window write every entry; a fresh table gets them all.
    #[test]
    fn the_gpu_table_writes_what_changed() {
        let mut nodes = LodNodes::new(1 << 12, 1 << 12, 4);
        nodes.table = Some(GpuTable::default());
        let (a, b, c) = (key(1, 0, 0, -3), key(2, 1, 0, -2), key(1, 5, 0, 5));
        nodes.apply(input(vec![
            upsert(a, mesh([1, 2, 3, 0, 0, 4]), 0),
            upsert(b, mesh([5; 6]), 0b11),
            upsert(c, mesh([1; 6]), 0),
        ]));
        let writes = nodes.take_table_writes(64, true);
        assert_eq!(writes.len(), 1);
        let (first, entries) = &writes[0];
        assert_eq!((*first, entries.len()), (0, 3));
        // Not up yet: nothing is live.
        assert!(
            entries
                .iter()
                .all(|e| e.node[3] as u32 & bits::LIVE_BIT == 0)
        );
        assert!(
            nodes.take_table_writes(64, false).is_empty(),
            "written once"
        );
        nodes.take_uploads(STAGING_BYTES, u64::MAX);
        let writes = nodes.take_table_writes(64, false);
        let entries: Vec<LodNodeGpu> = writes.iter().flat_map(|(_, e)| e.clone()).collect();
        assert_eq!(entries.len(), 3);
        let sa = nodes.by_key[&a];
        let ea = entries[sa as usize];
        let node = nodes.slots[sa as usize].as_ref().expect("a");
        assert_eq!(
            ea,
            LodNodeGpu::new(
                nodes.immediate(node),
                node.alloc.offset,
                [1, 2, 3, 0, 0, 4],
                true
            )
        );
        // A mask and a removal: those two entries, the removed one zeroed.
        let (sb, sc) = (nodes.by_key[&b], nodes.by_key[&c]);
        nodes.apply(input(vec![
            LodOp::Mask { key: b, mask: 0xFF },
            LodOp::Remove { key: c },
        ]));
        let writes = nodes.take_table_writes(64, false);
        let mut written: Vec<(u32, LodNodeGpu)> = writes
            .iter()
            .flat_map(|(first, e)| {
                e.iter()
                    .enumerate()
                    .map(move |(i, e)| (first + i as u32, *e))
            })
            .collect();
        written.sort_by_key(|(slot, _)| *slot);
        let slots: Vec<u32> = written.iter().map(|(slot, _)| *slot).collect();
        let mut want = vec![sb, sc];
        want.sort_unstable();
        assert_eq!(slots, want);
        let entry = |slot| written.iter().find(|(s, _)| *s == slot).expect("written").1;
        assert_eq!(entry(sc), LodNodeGpu::default());
        assert_eq!(entry(sb).node[3] as u32 & 0xFF, 0xFF);
        // The segments to draw: none while every node is hidden or the input does not draw.
        let origin = I64Vec3::ZERO;
        assert_eq!(nodes.gpu_frame(origin, false), 1);
        assert_eq!((nodes.stats.drawn, nodes.stats.quads), (1, 10));
        nodes.apply(input(vec![LodOp::Mask { key: a, mask: 0xFF }]));
        assert_eq!(nodes.gpu_frame(origin, false), 0);
        // A new tint writes every entry; so does a window that moves.
        let mut tinted = input(Vec::new());
        tinted.settings.debug_tint = true;
        nodes.apply(tinted.clone());
        let n: usize = nodes
            .take_table_writes(64, false)
            .iter()
            .map(|(_, e)| e.len())
            .sum();
        assert_eq!(n, 3, "every slot, the free one zeroed");
        nodes.apply(tinted);
        assert!(nodes.take_table_writes(64, false).is_empty());
        let far = I64Vec3::new(1 << 40, 0, 0);
        nodes.gpu_frame(far, false);
        let writes = nodes.take_table_writes(64, false);
        assert_eq!(writes.iter().map(|(_, e)| e.len()).sum::<usize>(), 3);
        // The nodes near the origin are outside the window around 2^40: not live.
        assert!(
            writes
                .iter()
                .flat_map(|(_, e)| e)
                .all(|e| e.node[3] as u32 & bits::LIVE_BIT == 0)
        );
        // A resync drops every node: their entries are zeroed.
        nodes.apply(LodInput {
            resync: true,
            ..input(Vec::new())
        });
        let writes = nodes.take_table_writes(64, false);
        assert!(
            writes
                .iter()
                .flat_map(|(_, e)| e)
                .all(|e| *e == LodNodeGpu::default())
        );
        assert_eq!(writes.iter().map(|(_, e)| e.len()).sum::<usize>(), 3);
    }

    /// A node mesh with `faces[f]` quads in face bucket `f`, the last `seams[f]` of them seam
    /// quads (`[on a node face, on a middle plane]`, marked as `boxel_mesh::lod` marks them).
    fn seamed_mesh(faces: [u32; 6], seams: [[u32; 2]; 6]) -> Arc<ChunkMesh> {
        let mut quads = Vec::new();
        let mut counts = [0; BUCKET_COUNT];
        for (f, &n) in faces.iter().enumerate() {
            let [on_face, middle] = seams[f];
            for i in 0..n {
                let seam = if i + middle >= n {
                    Seam::Middle
                } else if i + middle + on_face >= n {
                    Seam::Face
                } else {
                    Seam::None
                };
                quads.push(
                    PackedQuad::new([i % 32, 3, 5], 1, 1, Face::ALL[f], 1)
                        .with_light_index(seam as u16),
                );
            }
            counts[f] = n;
        }
        let mut mesh = ChunkMesh::from_quads(quads, counts);
        mesh.palette = boxel_mesh::lod::LOD_PALETTE.to_vec();
        Arc::new(mesh)
    }

    #[test]
    fn seams_are_left_out_only_against_a_drawn_neighbor() {
        let mut nodes = LodNodes::new(1 << 12, 1 << 12, 4);
        nodes.table = Some(GpuTable::default());
        let seams = [[1, 2], [0, 0], [3, 1], [2, 2], [0, 4], [1, 0]];
        let m = seamed_mesh([10; 6], seams);
        // Two level-2 nodes side by side along x, one above the first, one of level 1 beside it.
        let (a, b, up, fine) = (
            key(2, 0, 0, 0),
            key(2, 1, 0, 0),
            key(2, 0, 1, 0),
            key(1, -1, 0, 0),
        );
        nodes.apply(input(
            [a, b, up, fine]
                .into_iter()
                .map(|k| upsert(k, m.clone(), 0))
                .collect(),
        ));
        let mut entries = HashMap::new();
        let take = |nodes: &mut LodNodes, entries: &mut HashMap<u32, LodNodeGpu>| {
            for (first, run) in nodes.take_table_writes(64, false) {
                for (i, e) in run.into_iter().enumerate() {
                    entries.insert(first + i as u32, e);
                }
            }
        };
        take(&mut nodes, &mut entries);
        let across = |entries: &HashMap<u32, LodNodeGpu>, nodes: &LodNodes, k| {
            ((entries[&nodes.by_key[&k]].node[3] as u32) >> bits::ACROSS_SHIFT & 0x3F) as u8
        };
        // Not up yet: no neighbor is drawn.
        assert_eq!(across(&entries, &nodes, a), 0);
        nodes.take_uploads(STAGING_BYTES, u64::MAX);
        take(&mut nodes, &mut entries);
        // Face bucket order +Y, -Y, +X, -X, +Z, -Z; the level-1 node does not count.
        assert_eq!(across(&entries, &nodes, a), 0b00_0101);
        assert_eq!(across(&entries, &nodes, b), 0b00_1000);
        assert_eq!(across(&entries, &nodes, up), 0b00_0010);
        let e = entries[&nodes.by_key[&a]];
        for (f, [on_face, middle]) in seams.into_iter().enumerate() {
            assert_eq!(e.seams[f], on_face | middle << 16);
        }
        // b hides an octant by a: a draws its +X seams again. One on b's far side: it does not.
        nodes.apply(input(vec![LodOp::Mask {
            key: b,
            mask: 0b0100,
        }]));
        take(&mut nodes, &mut entries);
        assert_eq!(across(&entries, &nodes, a), 0b00_0001);
        nodes.apply(input(vec![LodOp::Mask {
            key: b,
            mask: 0b1000,
        }]));
        take(&mut nodes, &mut entries);
        assert_eq!(across(&entries, &nodes, a), 0b00_0101);
        // A new mesh for b: not drawn until it is up.
        nodes.apply(input(vec![upsert(b, m.clone(), 0)]));
        take(&mut nodes, &mut entries);
        assert_eq!(across(&entries, &nodes, a), 0b00_0001);
        nodes.take_uploads(STAGING_BYTES, u64::MAX);
        take(&mut nodes, &mut entries);
        assert_eq!(across(&entries, &nodes, a), 0b00_0101);
        // Gone: the seams show again on both sides of the removed nodes.
        nodes.apply(input(vec![
            LodOp::Remove { key: b },
            LodOp::Remove { key: up },
        ]));
        take(&mut nodes, &mut entries);
        assert_eq!(across(&entries, &nodes, a), 0);
        // A mesh without seam marks (or with counts that do not add up) has none to leave out.
        let fine_seams = nodes.slots[nodes.by_key[&fine] as usize]
            .as_ref()
            .map(|n| n.seams);
        assert_eq!(fine_seams, Some(seams));
        assert_eq!(node_seams(&mesh([4; 6]), [4; 6]), [[0; 2]; 6]);
        assert_eq!(node_seams(&m, [10, 10, 3, 10, 10, 10])[2], [0, 0]);
    }

    /// The instanced draws leave out the seam quads nothing shows, as High's cull does: on a
    /// node face with a drawn neighbor of the same level across, and on the middle planes while
    /// no octant is hidden; a run of whole buckets is one draw.
    #[test]
    fn the_instanced_draws_leave_out_covered_seams() {
        let mut nodes = LodNodes::new(1 << 12, 1 << 12, 4);
        let seams = [[1, 2], [0, 0], [3, 1], [2, 2], [0, 4], [1, 0]];
        let m = seamed_mesh([10; 6], seams);
        let (a, b) = (key(1, -1, 0, -3), key(1, 0, 0, -3));
        nodes.apply(input(vec![upsert(a, m.clone(), 0), upsert(b, m, 0)]));
        nodes.take_uploads(STAGING_BYTES, u64::MAX);
        // Ahead and below: a shows +Y, +X (toward b) and +Z; b shows +Y, -X (toward a), +Z.
        let view = camera_view(DVec3::new(0.0, 100.0, 0.0), 0.0, -0.3);
        nodes.cull(&view, false);
        let drawn =
            |nodes: &LodNodes| -> Vec<u32> { nodes.draws.iter().map(|d| d.quads).collect() };
        // +Y: 10 without its 2 middle quads; toward the other node: without its middle and node
        // face quads; +Z: without its 4 middle ones.
        assert_eq!(drawn(&nodes), [8, 6, 6, 8, 6, 6]);
        assert_eq!(nodes.stats.quads, 40);
        // b hides its octant by a: a's +X seam shows again, and b's -X bucket is drawn whole
        // (its octant 1 is drawn while octant 0 across its middle plane is hidden), one run
        // with its +Z bucket.
        nodes.apply(input(vec![LodOp::Mask {
            key: b,
            mask: 0b0001,
        }]));
        nodes.cull(&view, false);
        assert_eq!(drawn(&nodes), [8, 9, 6, 8, 16]);
        assert_eq!(nodes.stats.quads, 47);
    }

    /// The depth pyramid is skipped for a while once a sample shows it hides under 1 node in
    /// [`OCCLUSION_RATIO`] of those in the frustum (nothing in the frustum: nothing to hide).
    #[test]
    fn the_depth_pyramid_is_skipped_while_it_hides_little() {
        assert_eq!(occlusion_skip(0, 0), OCCLUSION_SKIP_FRAMES);
        assert_eq!(occlusion_skip(3200, 0), OCCLUSION_SKIP_FRAMES);
        assert_eq!(occlusion_skip(3200, 200), OCCLUSION_SKIP_FRAMES);
        assert_eq!(occlusion_skip(3200, 201), 0);
        assert_eq!(occlusion_skip(10, 10), 0, "underground: everything hidden");
        assert_eq!(occlusion_skip(u32::MAX, u32::MAX), 0);
    }

    #[test]
    fn segments_cover_every_slot() {
        assert_eq!(segments_for(0), 1);
        assert_eq!(segments_for(1), 1);
        let per = bits::SEGMENT_SLOTS as usize;
        assert_eq!(segments_for(per), 1);
        assert_eq!(segments_for(per + 1), 2);
        assert_eq!(segments_for(MAX_LOD_NODES as usize), 7);
    }
}
