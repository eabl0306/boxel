//! The LOD pass: the far LOD's own arena, its node records, the CPU cull and the draws.
//!
//! Every node is one allocation of its face buckets in the LOD arena (a `VERTEX | COPY_DST`
//! buffer, sub-allocated like the main arena), so LOD can never take room from full detail. Its
//! quads reach the GPU at the start of the next frame (one staging buffer per frame, one copy per
//! node). Each frame the nodes inside the frustum (the node box, then the box-level back-face
//! test of each face bucket, as `cull.wgsl` does for chunks) and with an octant left to draw
//! become one instanced draw each: `draw(0..4, first..first + n)` over the arena, the node in the
//! immediates ([`LodImmediate`]). Level 1 is drawn first, then the coarser ones (roughly front to
//! back, for early-Z).
use super::{LodInput, LodReport, far};
use crate::arena::{ArenaAlloc, ArenaAllocator, QUAD_BYTES};
use crate::path::bucket_visible;
use crate::renderer::ViewParams;
use crate::{DEPTH_FORMAT, MAIN_COLOR_FORMAT};
use boxel_math::{Aabb, I64Vec3, Vec3, wrap_to_i32};
use boxel_mesh::ChunkMesh;
use boxel_shaders::gpu::{LodImmediate, lod as bits};
use boxel_shaders::{ShaderModuleId, create_module};
use boxel_voxel::ChunkKey;
use boxel_world::lod::LodOp;
use std::collections::HashMap;
use std::sync::Arc;

/// Nodes the pass holds at most (the `extreme` preset keeps about 16k).
pub(crate) const MAX_LOD_NODES: u32 = 1 << 16;
/// Levels a node can have (4 bits on the GPU).
const LEVELS: usize = bits::LEVEL_MASK as usize + 1;
/// Largest staging buffer of a frame's uploads (more go in several).
const STAGING_BYTES: u64 = 64 << 20;
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
    /// Hidden octants (bit `x | y << 1 | z << 2`).
    mask: u8,
    /// The mesh until its quads are uploaded.
    pending: Option<Arc<ChunkMesh>>,
    /// Its index in `LodPass::levels[level]`.
    at: u32,
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
    /// Draw calls of the last frame (the drawn nodes inside the frustum).
    pub draws: u32,
    /// Quads of those draws.
    pub quads: u64,
    /// Node meshes refused (cumulative).
    pub refused: u32,
}

/// The far LOD's arena, pipeline and nodes (module docs).
pub(crate) struct LodPass {
    pipeline: wgpu::RenderPipeline,
    arena: wgpu::Buffer,
    nodes: LodNodes,
}

impl LodPass {
    /// Builds the pipeline (layout `[materials, view]` and 16 bytes of immediates: the tier's own
    /// group 0 and 1 bind groups work with it) and an arena of `arena_quads` quads, cut to the
    /// device's `max_buffer_size`. `color_mip` is the 1x1 mip level of the block textures. Call
    /// it inside error scopes.
    pub fn new(
        device: &wgpu::Device,
        materials: &wgpu::BindGroupLayout,
        view: &wgpu::BindGroupLayout,
        arena_quads: u32,
        color_mip: u32,
    ) -> Self {
        let quads = u64::from(arena_quads)
            .min(device.limits().max_buffer_size / QUAD_BYTES)
            .max(1) as u32;
        let arena = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel lod arena"),
            size: u64::from(quads) * QUAD_BYTES,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline: pipeline(device, materials, view),
            arena,
            nodes: LodNodes::new(quads, arena_quads, color_mip),
        }
    }

    /// `LodSettings::arena_quads` the pass was built for.
    pub fn requested_quads(&self) -> u32 {
        self.nodes.requested_quads
    }

    /// The horizon while the last input draws LOD.
    pub fn far(&self) -> Option<f32> {
        self.nodes.far
    }

    /// The terrain fog of the View uniform while the LOD draws: from `fade_start` of the horizon
    /// to the horizon.
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
    /// into `encoder`: one staging buffer of at most [`STAGING_BYTES`], one copy per node), then
    /// the draw list for `view` (the renderer's cull view, its far plane at the horizon).
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        view: &ViewParams,
    ) {
        for (batch, bytes) in self.nodes.take_uploads(STAGING_BYTES) {
            copy(device, encoder, &self.arena, &batch, bytes);
        }
        self.nodes.cull(view);
    }

    /// Records the frame's draws into the main pass (groups 0 and 1 are bound): the LOD pipeline,
    /// vertex buffer 0 = the LOD arena, one instanced draw per node. Binds no group 2 and no index
    /// buffer. Returns the draw calls recorded.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> u32 {
        let draws = &self.nodes.draws;
        if draws.is_empty() {
            return 0;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.arena.slice(..));
        for d in draws {
            pass.set_immediates(0, bytemuck::bytes_of(&d.immediate));
            pass.draw(0..4, d.first..d.first + d.quads);
        }
        draws.len() as u32
    }
}

/// A node's quads waiting for their upload: the mesh, its first arena quad and its quads.
type Upload = (Arc<ChunkMesh>, u32, u32);

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
    /// Refused since the last report.
    refused: Vec<ChunkKey>,
    warned_full: bool,
    /// The last input's horizon when it draws, its fade start and tint.
    far: Option<f32>,
    fade_start: f32,
    tint: bool,
    draws: Vec<NodeDraw>,
    stats: LodPassStats,
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
            refused: Vec::new(),
            warned_full: false,
            far: None,
            fade_start: 0.0,
            tint: false,
            draws: Vec::new(),
            stats: LodPassStats::default(),
        }
    }

    /// See [`LodPass::fog_params`].
    fn fog_params(&self) -> Option<[f32; 4]> {
        self.far.map(|h| [self.fade_start * h, h, 0.0, 0.0])
    }

    /// Applies one frame's input: a resync drops every node first, then the ops in order. A mesh
    /// that does not fit (arena or node table full) is refused: the node is not on the renderer
    /// any more, its older mesh included, and the next [`LodNodes::report`] names it.
    fn apply(&mut self, input: LodInput) {
        self.far = far(&input.settings, input.horizon);
        self.fade_start = input.settings.fade_start.clamp(0.0, 1.0);
        self.tint = input.settings.debug_tint;
        if input.resync {
            self.clear();
        }
        for op in input.ops {
            match op {
                LodOp::Upsert { key, mesh, mask } => self.upsert(key, mesh, mask),
                LodOp::Mask { key, mask } => {
                    if let Some(node) = self.node_mut(key) {
                        node.mask = mask;
                    }
                }
                LodOp::Remove { key } => self.remove(key),
            }
        }
        self.stats.nodes = self.by_key.len() as u32;
    }

    /// See [`LodPass::report`].
    fn report(&mut self) -> LodReport {
        LodReport {
            refused: std::mem::take(&mut self.refused),
            arena_free_quads: u64::from(self.alloc.capacity() - self.alloc.used()),
        }
    }

    fn node_mut(&mut self, key: ChunkKey) -> Option<&mut LodDraw> {
        let slot = *self.by_key.get(&key)?;
        self.slots[slot as usize].as_mut()
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
        let quads: u32 = faces.iter().sum();
        if quads == 0 {
            self.remove(key);
            return;
        }
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
            node.mask = mask;
            node.pending = Some(mesh);
            self.pending.push(slot);
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
            mask,
            pending: Some(mesh),
            at,
        });
        self.by_key.insert(key, slot);
        self.pending.push(slot);
    }

    fn remove(&mut self, key: ChunkKey) {
        let Some(slot) = self.by_key.remove(&key) else {
            return;
        };
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
        self.alloc.reset();
        self.stats.nodes = 0;
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

impl LodNodes {
    /// Takes the waiting uploads in batches of at most `limit` bytes (a single larger node goes
    /// alone), with their bytes.
    fn take_uploads(&mut self, limit: u64) -> Vec<(Vec<Upload>, u64)> {
        let mut out = Vec::new();
        let (mut batch, mut bytes) = (Vec::new(), 0u64);
        for slot in std::mem::take(&mut self.pending) {
            let Some(node) = self.slots.get_mut(slot as usize).and_then(Option::as_mut) else {
                continue;
            };
            let Some(mesh) = node.pending.take() else {
                continue;
            };
            let size = u64::from(node.alloc.size) * QUAD_BYTES;
            if bytes > 0 && bytes + size > limit {
                out.push((std::mem::take(&mut batch), bytes));
                bytes = 0;
            }
            bytes += size;
            batch.push((mesh, node.alloc.offset, node.alloc.size));
        }
        if bytes > 0 {
            out.push((batch, bytes));
        }
        out
    }

    /// The draw list for `view` (module docs). Empty while the last input does not draw LOD.
    fn cull(&mut self, view: &ViewParams) {
        self.draws.clear();
        self.stats.drawn = 0;
        self.stats.draws = 0;
        self.stats.quads = 0;
        if self.far.is_none() {
            return;
        }
        let (lo, hi) = (I64Vec3::splat(-WINDOW), I64Vec3::splat(WINDOW));
        for list in &self.levels {
            for &slot in list {
                let Some(node) = self.slots[slot as usize].as_ref() else {
                    continue;
                };
                if node.mask == 0xFF {
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
                let seen = node
                    .faces
                    .iter()
                    .enumerate()
                    .any(|(face, &n)| n > 0 && bucket_visible(face, min, max, view.eye_rel));
                if !seen {
                    continue;
                }
                let origin = wrap_to_i32(node.origin);
                self.draws.push(NodeDraw {
                    immediate: LodImmediate::new(
                        [origin.x, origin.y, origin.z],
                        node.key.lod,
                        node.mask,
                        self.color_mip,
                        self.tint,
                    ),
                    first: node.alloc.offset,
                    quads: node.alloc.size,
                });
                self.stats.quads += u64::from(node.alloc.size);
            }
        }
        self.stats.draws = self.draws.len() as u32;
    }
}

/// Copies `batch` (meshes with their arena offset and quads) into `arena` through one staging
/// buffer of `bytes`.
fn copy(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    arena: &wgpu::Buffer,
    batch: &[Upload],
    bytes: u64,
) {
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel lod uploads"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: true,
    });
    {
        let mut view = match staging.slice(..).get_mapped_range_mut() {
            Ok(view) => view,
            Err(e) => {
                // wgpu reported why (out of memory, a lost device); those nodes show garbage or
                // nothing until they are sent again (the device loss path resyncs everything).
                tracing::error!("far LOD uploads lost: {e}");
                return;
            }
        };
        let mut at = 0;
        for (mesh, _, n) in batch {
            let data: &[u8] = bytemuck::cast_slice(&mesh.quads[..*n as usize]);
            view.slice(at..at + data.len()).copy_from_slice(data);
            at += data.len();
        }
    }
    staging.unmap();
    let mut src = 0;
    for (_, offset, n) in batch {
        let size = u64::from(*n) * QUAD_BYTES;
        encoder.copy_buffer_to_buffer(&staging, src, arena, u64::from(*offset) * QUAD_BYTES, size);
        src += size;
    }
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
    let attributes = [wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Uint32x2,
        offset: 0,
        shader_location: 0,
    }];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("boxel lod"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_lod"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: QUAD_BYTES,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &attributes,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
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
            module: &module,
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
        let mut view = ViewParams::from_camera(&cv, &camera, [0.0; 4], 0.6);
        crate::renderer::far_plane_at(&mut view.frustum, view.eye_rel, 12288.0);
        view
    }

    /// The level lists, the key map and the slots agree; the allocator holds exactly the nodes'
    /// quads.
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
        let uploads = nodes.take_uploads(u64::MAX);
        let sent: Vec<u32> = uploads[0].0.iter().map(|(_, _, n)| *n).collect();
        assert_eq!(sent, [2, 6]);
        assert!(nodes.take_uploads(u64::MAX).is_empty());
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
        let batches = nodes.take_uploads(25 * QUAD_BYTES);
        let sizes: Vec<(usize, u64)> = batches.iter().map(|(b, n)| (b.len(), *n)).collect();
        assert_eq!(sizes, [(2, 160), (2, 160), (1, 80)]);
        // A node larger than the limit goes alone.
        nodes.apply(input(vec![upsert(
            key(1, 9, 0, 0),
            mesh([40, 0, 0, 0, 0, 0]),
            0,
        )]));
        let batches = nodes.take_uploads(25 * QUAD_BYTES);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].1, 40 * QUAD_BYTES);
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
        let ahead1 = key(1, -1, 0, -2);
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
        nodes.cull(&view);
        let drawn: Vec<(i32, u32)> = nodes
            .draws
            .iter()
            .map(|d| (d.immediate.node[0] & 15, d.quads))
            .collect();
        assert_eq!(drawn, [(1, 4), (2, 2)], "level 1 first");
        assert_eq!(nodes.stats.draws, 2);
        assert_eq!(nodes.stats.quads, 6);
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
        nodes.cull(&view);
        assert!(nodes.draws.is_empty());
        assert_eq!(
            nodes.stats,
            LodPassStats {
                nodes: 6,
                ..LodPassStats::default()
            }
        );
        nodes.apply(LodInput::new(LodSettings::default(), None));
        nodes.cull(&view);
        assert!(nodes.draws.is_empty());
        assert_eq!(nodes.fog_params(), None);
        // The tint, and the fog from fade_start of the horizon to it.
        let mut tinted = input(Vec::new());
        tinted.settings.debug_tint = true;
        tinted.settings.fade_start = 0.25;
        nodes.apply(tinted);
        nodes.cull(&view);
        assert!(
            nodes
                .draws
                .iter()
                .all(|d| d.immediate.node[3] as u32 & bits::TINT_BIT != 0)
        );
        assert_eq!(nodes.fog_params(), Some([3072.0, 12288.0, 0.0, 0.0]));
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
            nodes.cull(&view);
            let drawn: Vec<u32> = nodes.draws.iter().map(|d| d.quads).collect();
            assert_eq!(drawn, [6, 12], "shift {shift}");
        }
    }
}
