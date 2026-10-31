//! Cell quads (EW-ITEM, ADR 0050): the quads cells draw from their data (an item in a frame, the
//! items on a shelf), baked into the chunk meshes ([`ChunkMesh::cell_quads`]) and drawn here, in
//! the main pass right after the solid terrain, with one pipeline shared by every tier
//! (`cell.wgsl`: one uniform block, one texture array and its sampler, plain vertices).
//!
//! The images are a texture array of their own, the cell textures, which the content gives
//! ([`crate::CellTextures`]): the block texture array is almost full and Low binds one texture
//! per pipeline, so the images cannot share the block layers nor sit beside them in the terrain
//! pipelines. The vertices of every chunk with cell quads live in one vertex buffer, rebuilt
//! when one of those chunks changes (cell quads are few: an item a frame); a camera move
//! changes nothing, since the shader subtracts the render origin as the terrain does. Each
//! frame the chunks inside the GPU window and the frustum are drawn, runs of consecutive ones
//! as one draw. Without cell quads nothing of this is built and nothing runs.

use crate::chunk_table::ChunkTable;
use crate::renderer::ViewParams;
use crate::textures::BlockTextureData;
use crate::{DEPTH_FORMAT, MAIN_COLOR_FORMAT};
use boxel_math::{Aabb, I64Vec3, Vec3, wrap_to_i32};
use boxel_mesh::{ChunkMesh, LitCellQuad};
use boxel_shaders::gpu::{CellVertex, bindings, cell};
use boxel_shaders::{ShaderModuleId, create_module};
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
    /// Quads drawn this frame (inside the window and the frustum).
    pub drawn: u32,
    /// Draw calls this frame.
    pub draws: u32,
    /// Vertex buffer uploads so far (one per frame with a changed chunk).
    pub uploads: u64,
    /// Layers of the cell texture array (0 until the content gives it).
    pub layers: u32,
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

/// The pipeline, the cell textures and the vertices of the cell quads (one per renderer).
pub(crate) struct CellPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// The cell textures and their bind group, with the data they were made from.
    textures: Option<(Arc<BlockTextureData>, wgpu::Texture, wgpu::BindGroup)>,
    vertex: Option<wgpu::Buffer>,
    /// Vertices `vertex` holds.
    vertex_capacity: usize,
    vertices: Vec<CellVertex>,
    /// Per chunk in the store's order: its origin and its vertex range.
    ranges: Vec<(I64Vec3, Range<u32>)>,
    /// This frame's draws (merged vertex ranges).
    draws: Vec<Range<u32>>,
    stats: CellStats,
}

impl CellPass {
    /// The pipeline for `device`; `view_layout` is the renderer's group 1 (the `View` uniform).
    pub(crate) fn new(device: &wgpu::Device, view_layout: &wgpu::BindGroupLayout) -> Self {
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
        Self {
            pipeline,
            layout,
            textures: None,
            vertex: None,
            vertex_capacity: 0,
            vertices: Vec::new(),
            ranges: Vec::new(),
            draws: Vec::new(),
            stats: CellStats::default(),
        }
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
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("boxel cell textures"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
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
            self.vertices.clear();
            self.ranges.clear();
            for (origin, mesh) in store.chunks.values() {
                let start = self.vertices.len() as u32;
                let gpu = wrap_to_i32(*origin).to_array();
                for q in &mesh.cell_quads {
                    self.vertices.extend(quad_vertices(gpu, q));
                }
                self.ranges
                    .push((*origin, start..self.vertices.len() as u32));
            }
            self.upload(device, queue);
        }
        self.stats.chunks = self.ranges.len() as u32;
        self.stats.quads = self.vertices.len() as u32 / 6;
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
        self.stats.drawn = self.draws.iter().map(|r| r.len() as u32).sum::<u32>() / 6;
        self.stats.draws = if self.textures.is_some() {
            self.draws.len() as u32
        } else {
            0
        };
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.vertices.is_empty() {
            return;
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
        if let Some(buffer) = &self.vertex {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&self.vertices));
            self.stats.uploads += 1;
        }
    }

    /// Draws this frame's cell quads into `pass` (group 1 already holds the View); leaves group
    /// 0 bound to the cell textures. False if it drew nothing.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) -> bool {
        let (Some((_, _, group)), Some(vertex)) = (&self.textures, &self.vertex) else {
            return false;
        };
        if self.draws.is_empty() {
            return false;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(cell::GROUP_TEXTURES, group, &[]);
        pass.set_vertex_buffer(0, vertex.slice(..));
        for range in &self.draws {
            pass.draw(range.clone(), 0..1);
        }
        true
    }

    pub(crate) fn stats(&self) -> CellStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_voxel::{CellQuad, Face, LightValue};

    fn mesh(quads: usize) -> Arc<ChunkMesh> {
        let quad = CellQuad {
            corners: [[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
            uv: [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            image: 5,
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
