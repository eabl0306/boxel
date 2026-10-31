//! Cell quads (EW-ITEM, ADR 0050): the quads cells draw from their data (an item in a frame, the
//! items on a shelf), baked into the chunk meshes ([`ChunkMesh::cell_quads`]) and drawn here, in
//! the main pass right after the solid terrain, with one pipeline shared by every tier
//! (`cell.wgsl`: one uniform block, one texture array and its sampler, plain vertices).
//!
//! A quad's image is a layer of one of two texture arrays: the cell textures, which the content
//! gives ([`crate::CellTextures`]), or the block textures the terrain draws with (a block drawn
//! small in a frame, ADR 0053). The block texture array is almost full and Low binds one texture
//! per pipeline, so the cell images cannot share the block layers nor sit beside them in the
//! terrain pipelines; and the pipeline here binds one array too. So the quads are kept in two
//! lists, one per array, each drawn with the same pipeline and its own bind group: the shader and
//! its bindings are the same on every tier, only group 0 changes between the two draws. The
//! vertices of every chunk with cell quads live in one vertex buffer per list, rebuilt when one
//! of those chunks changes (cell quads are few: an item a frame); a camera move changes nothing,
//! since the shader subtracts the render origin as the terrain does. Each frame the chunks inside
//! the GPU window and the frustum are drawn, runs of consecutive ones as one draw. Without cell
//! quads nothing of this is built and nothing runs.
//!
//! The content can also give quads that are not in any mesh, for the frames it gives them
//! ([`MovingCellQuad`], ADR 0054): what moves (a lid that opens, a bell that swings, a beam that
//! turns). They are drawn by the same pipeline after the chunks' quads, in two more lists (one
//! per texture array) rebuilt and uploaded only on a frame whose list differs from the last one.

use crate::chunk_table::ChunkTable;
use crate::renderer::ViewParams;
use crate::textures::BlockTextureData;
use crate::{DEPTH_FORMAT, MAIN_COLOR_FORMAT};
use boxel_math::{Aabb, I64Vec3, Vec3, wrap_to_i32};
use boxel_mesh::{ChunkMesh, LitCellQuad};
use boxel_shaders::gpu::{CellVertex, bindings, cell};
use boxel_shaders::{ShaderModuleId, create_module};
use boxel_voxel::{BlockPos, CellImage, CellQuad, LightValue};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

/// Edge of a chunk in blocks, for the frustum test.
const CHUNK_EDGE: f32 = 32.0;
/// Corner order of a quad's two triangles (corner `k` at `(k & 1, k >> 1)`).
const TRIANGLES: [usize; 6] = [0, 1, 2, 2, 1, 3];

/// Cell quad counters of the last frame (all 0 before the first chunk with cell quads).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellStats {
    /// Resident chunks with cell quads.
    pub chunks: u32,
    /// Their cell quads.
    pub quads: u32,
    /// Those of them that show a block layer ([`CellImage::Block`], ADR 0053).
    pub block_quads: u32,
    /// Quads drawn this frame (inside the window and the frustum).
    pub drawn: u32,
    /// Draw calls this frame.
    pub draws: u32,
    /// Vertex buffer uploads so far (one per frame with a changed chunk).
    pub uploads: u64,
    /// Layers of the cell texture array (0 until the content gives it).
    pub layers: u32,
    /// Moving cell quads of the last frame ([`MovingCellQuad`], ADR 0054), all drawn.
    pub moving: u32,
    /// Uploads of their vertices so far (one per frame whose list changed).
    pub moving_uploads: u64,
}

/// A cell quad the content gives for one frame, not baked into a chunk mesh (ADR 0054): a part
/// that moves (a lid that opens, a bell that swings, a beam that turns), drawn like the chunks'
/// cell quads (the same pipeline, both texture arrays, lit by its cell's light without ambient
/// occlusion) but not culled: give only what may show. The render plugin draws the
/// [`crate::MovingCellQuads`] of each frame, lit at their cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovingCellQuad {
    /// The cell whose min corner the quad's corners start from: it may reach out of it (a beam
    /// rises hundreds of blocks), and its light is that cell's.
    pub cell: BlockPos,
    /// The quad, its corners in blocks from `cell`'s min corner.
    pub quad: CellQuad,
}

/// The chunks with cell quads, by id (a `BTreeMap`: the vertex order is the same every time).
#[derive(Default)]
pub(crate) struct CellStore {
    chunks: BTreeMap<u64, (I64Vec3, Arc<ChunkMesh>)>,
    /// A chunk came, went or changed since the last upload.
    dirty: bool,
}

impl CellStore {
    /// Records `mesh` as the mesh of chunk `id` at `origin` (the renderer's `upsert_chunk`):
    /// kept if it has cell quads, else the chunk leaves the store.
    pub(crate) fn upsert(&mut self, id: u64, origin: I64Vec3, mesh: &Arc<ChunkMesh>) {
        if mesh.cell_quads.is_empty() {
            self.remove(id);
            return;
        }
        let same = self
            .chunks
            .get(&id)
            .is_some_and(|(o, m)| *o == origin && (Arc::ptr_eq(m, mesh) || **m == **mesh));
        if !same {
            self.chunks.insert(id, (origin, Arc::clone(mesh)));
            self.dirty = true;
        }
    }

    pub(crate) fn remove(&mut self, id: u64) {
        self.dirty |= self.chunks.remove(&id).is_some();
    }

    pub(crate) fn clear(&mut self) {
        self.dirty |= !self.chunks.is_empty();
        self.chunks.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

/// A bind group of the cell pipeline's group 0: `view` (a `D2Array`) and `sampler`.
fn texture_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("boxel cell textures"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// The 6 vertices of `q` (two triangles) in a chunk whose GPU origin is `origin`.
pub(crate) fn quad_vertices(origin: [i32; 3], q: &LitCellQuad) -> [CellVertex; 6] {
    let c = &q.quad;
    let data0 = (u32::from(c.image) & cell::LAYER_MASK)
        | (c.face as u32) << cell::FACE_SHIFT
        | if c.full_bright { cell::FULL_BRIGHT } else { 0 }
        | u32::from(q.light.0) << cell::LIGHT_SHIFT;
    let [r, g, b] = c.tint.map(u32::from);
    let data = [data0, r << 16 | g << 8 | b];
    TRIANGLES.map(|k| CellVertex {
        origin,
        pos: c.corners[k],
        uv: c.uv[k],
        data,
    })
}

/// The cell quads whose images are layers of one texture array: their vertices, each chunk's
/// range of them and this frame's draws.
#[derive(Default)]
struct QuadList {
    vertex: Option<wgpu::Buffer>,
    /// Vertices `vertex` holds.
    vertex_capacity: usize,
    vertices: Vec<CellVertex>,
    /// Per chunk with quads in this list, in the store's order: its origin and its vertex range.
    ranges: Vec<(I64Vec3, Range<u32>)>,
    /// This frame's draws (merged vertex ranges).
    draws: Vec<Range<u32>>,
}

impl QuadList {
    /// This frame's draws: the chunks inside the GPU window (`chunks`) and the frustum of
    /// `view`, runs of consecutive ones merged.
    fn pick(&mut self, chunks: &ChunkTable, view: &ViewParams) {
        self.draws.clear();
        for (origin, range) in &self.ranges {
            if !chunks.in_window(*origin) {
                continue;
            }
            let min = origin.saturating_sub(view.render_origin).as_vec3();
            let max = min + Vec3::splat(CHUNK_EDGE);
            if !view.frustum.intersects_aabb(&Aabb::new(min, max)) {
                continue;
            }
            match self.draws.last_mut() {
                Some(last) if last.end == range.start => last.end = range.end,
                _ => self.draws.push(range.clone()),
            }
        }
    }

    /// Writes the vertices into the vertex buffer (grown to fit); true if it wrote any.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        if self.vertices.is_empty() {
            return false;
        }
        if self.vertex.is_none() || self.vertex_capacity < self.vertices.len() {
            let capacity = self.vertices.len().next_power_of_two().max(6 * 256);
            self.vertex = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel cell vertices"),
                size: (capacity * size_of::<CellVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.vertex_capacity = capacity;
        }
        let Some(buffer) = &self.vertex else {
            return false;
        };
        queue.write_buffer(buffer, 0, bytemuck::cast_slice(&self.vertices));
        true
    }

    /// Draws this frame's quads with `group` as group 0; false if there were none.
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, group: &wgpu::BindGroup) -> bool {
        let Some(vertex) = &self.vertex else {
            return false;
        };
        if self.draws.is_empty() {
            return false;
        }
        pass.set_bind_group(cell::GROUP_TEXTURES, group, &[]);
        pass.set_vertex_buffer(0, vertex.slice(..));
        for range in &self.draws {
            pass.draw(range.clone(), 0..1);
        }
        true
    }

    fn drawn(&self) -> u32 {
        self.draws.iter().map(|r| r.len() as u32).sum::<u32>() / 6
    }

    /// Draws every vertex this frame (the moving quads: not culled).
    fn pick_all(&mut self) {
        self.draws.clear();
        if !self.vertices.is_empty() {
            self.draws.push(0..self.vertices.len() as u32);
        }
    }
}

/// Index in [`CellPass::lists`] of the quads whose images are in `source`.
const fn list_of(source: CellImage) -> usize {
    match source {
        CellImage::Cell => 0,
        CellImage::Block => 1,
    }
}

/// The vertices of every chunk of `store` in `lists`, each quad in the list of its texture array
/// ([`list_of`]), each chunk's run of a list recorded with its origin.
fn fill(lists: &mut [QuadList; 2], store: &CellStore) {
    for list in lists.iter_mut() {
        list.vertices.clear();
        list.ranges.clear();
    }
    for (origin, mesh) in store.chunks.values() {
        let starts = lists.each_ref().map(|l| l.vertices.len() as u32);
        let gpu = wrap_to_i32(*origin).to_array();
        for q in &mesh.cell_quads {
            lists[list_of(q.quad.source)]
                .vertices
                .extend(quad_vertices(gpu, q));
        }
        for (list, start) in lists.iter_mut().zip(starts) {
            let end = list.vertices.len() as u32;
            if end > start {
                list.ranges.push((*origin, start..end));
            }
        }
    }
}

/// The vertices of the moving quads `quads` (each with its light) in `lists`, each quad in the
/// list of its texture array, placed from its cell.
fn fill_moving(lists: &mut [QuadList; 2], quads: &[(MovingCellQuad, LightValue)]) {
    for list in lists.iter_mut() {
        list.vertices.clear();
        list.ranges.clear();
    }
    for &(moving, light) in quads {
        let cell = moving.cell;
        let origin = wrap_to_i32(I64Vec3::new(cell.x, cell.y, cell.z)).to_array();
        let lit = LitCellQuad {
            quad: moving.quad,
            light,
        };
        lists[list_of(moving.quad.source)]
            .vertices
            .extend(quad_vertices(origin, &lit));
    }
}

/// A quad list for `benches/cells.rs` (`crate::bench::MovingBench`).
#[derive(Default)]
pub(crate) struct QuadListBench(QuadList);

/// [`fill_moving`] into bench lists: the vertices written.
pub(crate) fn fill_moving_bench(
    lists: &mut [QuadListBench; 2],
    quads: &[(MovingCellQuad, LightValue)],
) -> usize {
    let [a, b] = lists;
    let mut pair = [std::mem::take(&mut a.0), std::mem::take(&mut b.0)];
    fill_moving(&mut pair, quads);
    let n = pair[0].vertices.len() + pair[1].vertices.len();
    let [x, y] = pair;
    a.0 = x;
    b.0 = y;
    n
}

/// The pipeline, the cell textures and the vertices of the cell quads (one per renderer).
pub(crate) struct CellPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// The cell textures and their bind group, with the data they were made from.
    textures: Option<(Arc<BlockTextureData>, wgpu::Texture, wgpu::BindGroup)>,
    /// The block textures (the terrain's array, animated layers included) in this pipeline's
    /// layout.
    blocks: wgpu::BindGroup,
    /// The quads of each texture array: the cell textures', then the block textures'.
    lists: [QuadList; 2],
    /// The moving quads (ADR 0054) of each texture array, in the same order.
    moving: [QuadList; 2],
    /// The moving quads given last, with their light: a frame that gives the same uploads
    /// nothing.
    moving_input: Vec<(MovingCellQuad, LightValue)>,
    stats: CellStats,
}

impl CellPass {
    /// The pipeline for `device`; `view_layout` is the renderer's group 1 (the `View` uniform),
    /// `block_texture` the terrain's block texture array.
    pub(crate) fn new(
        device: &wgpu::Device,
        view_layout: &wgpu::BindGroupLayout,
        block_texture: &wgpu::Texture,
    ) -> Self {
        let module = create_module(device, ShaderModuleId::Cell);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel cell textures"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let mut groups: [Option<&wgpu::BindGroupLayout>; 2] = [None, None];
        groups[cell::GROUP_TEXTURES as usize] = Some(&layout);
        groups[bindings::GROUP_VIEW as usize] = Some(view_layout);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel cell"),
            bind_group_layouts: &groups,
            immediate_size: 0,
        });
        let [o0, o1, o2, o3] = cell::ATTRIBUTE_OFFSETS;
        let attribute = |format, offset, shader_location| wgpu::VertexAttribute {
            format,
            offset,
            shader_location,
        };
        let attributes = [
            attribute(wgpu::VertexFormat::Sint32x3, o0, 0),
            attribute(wgpu::VertexFormat::Float32x3, o1, 1),
            attribute(wgpu::VertexFormat::Float32x2, o2, 2),
            attribute(wgpu::VertexFormat::Uint32x2, o3, 3),
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("boxel cell"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_cell"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<CellVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                })],
            },
            // Both sides, as the terrain's cutout bucket.
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            // Tested and written like the terrain (reverse-Z).
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
                entry_point: Some("fs_cell"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: MAIN_COLOR_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let blocks = texture_group(
            device,
            &layout,
            &crate::renderer::array_view(block_texture),
            &crate::renderer::block_sampler(device),
        );
        Self {
            pipeline,
            layout,
            textures: None,
            blocks,
            lists: Default::default(),
            moving: Default::default(),
            moving_input: Vec::new(),
            stats: CellStats::default(),
        }
    }

    /// Uses `quads` as the moving quads from the next [`CellPass::prepare`] on (uploaded then if
    /// they differ from the last ones); gives back the previous list, to fill again.
    pub(crate) fn set_moving(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        quads: Vec<(MovingCellQuad, LightValue)>,
    ) -> Vec<(MovingCellQuad, LightValue)> {
        if quads == self.moving_input {
            return quads;
        }
        let old = std::mem::replace(&mut self.moving_input, quads);
        fill_moving(&mut self.moving, &self.moving_input);
        let mut wrote = false;
        for list in &mut self.moving {
            wrote |= list.upload(device, queue);
        }
        self.stats.moving_uploads += u64::from(wrote);
        old
    }

    /// Uses `data` as the cell textures (nothing to do if they are the ones in use). A texture
    /// this device cannot hold is refused with the reason, and the quads are not drawn.
    pub(crate) fn set_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &Arc<BlockTextureData>,
    ) -> Result<(), String> {
        if self
            .textures
            .as_ref()
            .is_some_and(|(d, ..)| Arc::ptr_eq(d, data))
        {
            return Ok(());
        }
        self.textures = None;
        self.stats.layers = 0;
        let (texture, view, sampler) =
            crate::renderer::block_textures(device, queue, data, &device.limits())?;
        let group = texture_group(device, &self.layout, &view, &sampler);
        self.stats.layers = data.layers;
        self.textures = Some((Arc::clone(data), texture, group));
        Ok(())
    }

    /// Uploads the vertices if the store changed, then picks this frame's draws: the chunks
    /// inside the GPU window (`chunks`) and the frustum of `view`.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        store: &mut CellStore,
        chunks: &ChunkTable,
        view: &ViewParams,
    ) {
        if store.dirty {
            store.dirty = false;
            fill(&mut self.lists, store);
            let mut wrote = false;
            for list in &mut self.lists {
                wrote |= list.upload(device, queue);
            }
            self.stats.uploads += u64::from(wrote);
        }
        self.stats.chunks = store.chunks.len() as u32;
        let quads = self.lists.each_ref().map(|l| l.vertices.len() as u32 / 6);
        self.stats.quads = quads[0] + quads[1];
        self.stats.block_quads = quads[1];
        for list in &mut self.lists {
            list.pick(chunks, view);
        }
        for list in &mut self.moving {
            list.pick_all();
        }
        self.stats.moving = self.moving_input.len() as u32;
        let all = || self.lists.iter().chain(&self.moving);
        self.stats.drawn = all().map(QuadList::drawn).sum();
        let cells = if self.textures.is_some() {
            (self.lists[0].draws.len() + self.moving[0].draws.len()) as u32
        } else {
            0
        };
        self.stats.draws = cells + (self.lists[1].draws.len() + self.moving[1].draws.len()) as u32;
    }

    /// Draws this frame's cell quads into `pass` (group 1 already holds the View): those of the
    /// cell textures (once the content gave them), then those of the block textures. Leaves group
    /// 0 bound to one of them. False if it drew nothing.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> bool {
        if self
            .lists
            .iter()
            .chain(&self.moving)
            .all(|l| l.draws.is_empty())
        {
            return false;
        }
        pass.set_pipeline(&self.pipeline);
        let mut drew = false;
        if let Some((_, _, group)) = &self.textures {
            drew |= self.lists[0].draw(pass, group);
            drew |= self.moving[0].draw(pass, group);
        }
        drew |= self.lists[1].draw(pass, &self.blocks);
        drew |= self.moving[1].draw(pass, &self.blocks);
        drew
    }

    pub(crate) fn stats(&self) -> CellStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_voxel::{CellQuad, Face, LightValue};

    /// A mesh of `cells` quads of cell images then `blocks` of block layers.
    fn mixed(cells: usize, blocks: usize) -> Arc<ChunkMesh> {
        let mut m = (*mesh(cells + blocks)).clone();
        for q in &mut m.cell_quads[cells..] {
            q.quad.source = CellImage::Block;
        }
        Arc::new(m)
    }

    /// Each quad goes to the list of its texture array, each chunk's run of a list recorded
    /// with its origin; a chunk with none in a list has no run there.
    #[test]
    fn quads_are_listed_by_their_texture_array() {
        let mut store = CellStore::default();
        let (a, b) = (I64Vec3::new(0, 0, 0), I64Vec3::new(32, 0, 0));
        store.upsert(1, a, &mixed(2, 3));
        store.upsert(2, b, &mixed(1, 0));
        let mut lists: [QuadList; 2] = Default::default();
        fill(&mut lists, &store);
        assert_eq!(lists[0].vertices.len(), 6 * 3);
        assert_eq!(lists[1].vertices.len(), 6 * 3);
        assert_eq!(lists[0].ranges, vec![(a, 0..12), (b, 12..18)]);
        assert_eq!(lists[1].ranges, vec![(a, 0..18)]);
        assert_eq!(list_of(CellImage::Block), 1);
    }

    /// Moving quads go to the list of their texture array, placed from their cell (wrapped to
    /// i32 as a chunk origin is) with their light, and all of them are drawn.
    #[test]
    fn moving_quads_are_listed_from_their_cell() {
        let quad = mesh(1).cell_quads[0].quad;
        let far = BlockPos::new((1 << 40) + 3, -7, 12);
        let quads = vec![
            (
                MovingCellQuad {
                    cell: BlockPos::new(1, 2, 3),
                    quad,
                },
                LightValue(0x1234),
            ),
            (
                MovingCellQuad {
                    cell: far,
                    quad: CellQuad {
                        source: CellImage::Block,
                        ..quad
                    },
                },
                LightValue::FULL_SKY,
            ),
        ];
        let mut lists: [QuadList; 2] = Default::default();
        fill_moving(&mut lists, &quads);
        assert_eq!((lists[0].vertices.len(), lists[1].vertices.len()), (6, 6));
        assert!(lists[0].vertices.iter().all(|v| v.origin == [1, 2, 3]));
        let wrapped = wrap_to_i32(I64Vec3::new(far.x, far.y, far.z)).to_array();
        assert!(lists[1].vertices.iter().all(|v| v.origin == wrapped));
        assert_eq!(lists[0].vertices[0].data[0] >> cell::LIGHT_SHIFT, 0x1234);
        assert_eq!(lists[0].vertices[0].pos, quad.corners[0]);
        for list in &mut lists {
            list.pick_all();
        }
        assert_eq!(lists[0].draws, vec![0..6]);
        assert_eq!(lists[0].drawn() + lists[1].drawn(), 2);
        fill_moving(&mut lists, &[]);
        lists[0].pick_all();
        assert!(lists[0].vertices.is_empty() && lists[0].draws.is_empty());
    }

    fn mesh(quads: usize) -> Arc<ChunkMesh> {
        let quad = CellQuad {
            corners: [[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
            uv: [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            image: 5,
            source: CellImage::Cell,
            face: Face::NegZ,
            full_bright: true,
            tint: [1, 2, 3],
        };
        Arc::new(ChunkMesh {
            cell_quads: vec![
                LitCellQuad {
                    quad,
                    light: LightValue(0xABCD),
                };
                quads
            ],
            ..ChunkMesh::default()
        })
    }

    #[test]
    fn a_quad_is_two_triangles_with_its_data_packed() {
        let m = mesh(1);
        let v = quad_vertices([1, -2, 3], &m.cell_quads[0]);
        assert_eq!(
            v.map(|v| v.pos),
            TRIANGLES.map(|k| m.cell_quads[0].quad.corners[k])
        );
        assert!(v.iter().all(|v| v.origin == [1, -2, 3]));
        let face = Face::NegZ as u32;
        assert_eq!(
            v[0].data,
            [5 | face << 12 | 0x8000 | 0xABCD << 16, 0x01_02_03]
        );
    }

    #[test]
    fn the_store_keeps_only_meshes_with_cell_quads_and_marks_changes() {
        let mut store = CellStore::default();
        let origin = I64Vec3::new(32, 0, -64);
        store.upsert(1, origin, &Arc::new(ChunkMesh::default()));
        assert!(store.is_empty() && !store.dirty);
        let m = mesh(2);
        store.upsert(1, origin, &m);
        assert!(!store.is_empty() && store.dirty);
        store.dirty = false;
        // The same mesh again (or an equal one) changes nothing.
        store.upsert(1, origin, &m);
        store.upsert(1, origin, &mesh(2));
        assert!(!store.dirty);
        store.upsert(1, origin, &mesh(3));
        assert!(store.dirty);
        store.dirty = false;
        store.upsert(1, origin, &Arc::new(ChunkMesh::default()));
        assert!(store.is_empty() && store.dirty);
        store.upsert(2, origin, &m);
        store.dirty = false;
        store.remove(3);
        assert!(!store.dirty);
        store.clear();
        assert!(store.is_empty() && store.dirty);
    }
}
