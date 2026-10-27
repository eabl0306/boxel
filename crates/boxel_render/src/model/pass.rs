//! The model pass (ADR 0040): two pipelines (opaque and cutout) shared by every tier, the skins
//! on the GPU, and one vertex buffer the CPU fills with the posed instances
//! ([`super::ModelScene`]) when they change.

use super::scene::ModelScene;
use super::{ModelDraw, ModelLayer, ModelMaterial, ModelStats};
use crate::renderer::ViewParams;
use crate::{DEPTH_FORMAT, MAIN_COLOR_FORMAT};
use boxel_math::Quat;
use boxel_shaders::gpu::{ModelVertex, model};
use boxel_shaders::{ShaderModuleId, create_module};
use std::collections::HashMap;
use wgpu::util::DeviceExt as _;

/// The fewest quads the index buffer is made for.
const MIN_QUADS: u32 = 1024;

/// A skin on the GPU.
struct Skin {
    /// Kept alive by the bind group; held for clarity.
    _texture: wgpu::Texture,
    group: wgpu::BindGroup,
}

/// The pipelines, skins and buffers of the entity models (one per renderer).
pub(crate) struct ModelPass {
    opaque: wgpu::RenderPipeline,
    cutout: wgpu::RenderPipeline,
    skin_layout: wgpu::BindGroupLayout,
    skins: HashMap<u64, Skin, foldhash::fast::FixedState>,
    scene: ModelScene,
    draws: Vec<ModelDraw>,
    camera: Quat,
    vertex: Option<wgpu::Buffer>,
    /// Vertices `vertex` holds.
    vertex_capacity: usize,
    index: Option<wgpu::Buffer>,
    /// Quads `index` covers.
    index_quads: u32,
    stats: ModelStats,
    /// A skin was refused (logged once).
    warned: bool,
}

impl ModelPass {
    /// The pipelines for `device`; `view_layout` is the renderer's group 1 (the `View`
    /// uniform). Nothing is uploaded until [`ModelPass::prepare`].
    pub(crate) fn new(device: &wgpu::Device, view_layout: &wgpu::BindGroupLayout) -> Self {
        let module = create_module(device, ShaderModuleId::Model);
        let skin_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel model skin"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let mut groups: [Option<&wgpu::BindGroupLayout>; 2] = [None, None];
        groups[model::GROUP_SKIN as usize] = Some(&skin_layout);
        groups[boxel_shaders::gpu::bindings::GROUP_VIEW as usize] = Some(view_layout);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel model"),
            bind_group_layouts: &groups,
            immediate_size: 0,
        });
        let [o0, o1, o2, o3] = model::ATTRIBUTE_OFFSETS;
        let attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: o0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: o1,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint16x4,
                offset: o2,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: o3,
                shader_location: 3,
            },
        ];
        let build = |label: &str, entry: &str, cull_mode: Option<wgpu::Face>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_model"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<ModelVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode,
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
                    entry_point: Some(entry),
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
        };
        Self {
            opaque: build(
                "boxel model opaque",
                "fs_model_opaque",
                Some(wgpu::Face::Back),
            ),
            cutout: build("boxel model cutout", "fs_model_cutout", None),
            skin_layout,
            skins: HashMap::default(),
            scene: ModelScene::default(),
            draws: Vec::new(),
            camera: Quat::IDENTITY,
            vertex: None,
            vertex_capacity: 0,
            index: None,
            index_quads: 0,
            stats: ModelStats::default(),
            warned: false,
        }
    }

    /// The instances of the next frames and the camera's rotation (for the view layer); gives
    /// back the previous list, emptied, to fill the next frame without allocating.
    pub(crate) fn set(&mut self, draws: Vec<ModelDraw>, camera: Quat) -> Vec<ModelDraw> {
        self.camera = camera;
        let mut old = std::mem::replace(&mut self.draws, draws);
        old.clear();
        old
    }

    /// Uploads the skins the instances use that the GPU lacks (and drops the unused ones), then,
    /// if anything changed, the frame's vertices.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &ViewParams,
    ) {
        let max = device.limits().max_texture_dimension_2d;
        let before = self.draws.len();
        self.draws
            .retain(|d| d.texture.width() <= max && d.texture.height() <= max);
        let refused = before - self.draws.len();
        if refused > 0 && !self.warned {
            self.warned = true;
            tracing::warn!("entity models: a skin is bigger than this device's {max} texels");
        }
        for draw in &self.draws {
            let t = &draw.texture;
            if self.skins.contains_key(&t.id()) {
                continue;
            }
            let size = wgpu::Extent3d {
                width: t.width(),
                height: t.height(),
                depth_or_array_layers: 1,
            };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("boxel model skin"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                t.rgba(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(t.width() * 4),
                    rows_per_image: Some(t.height()),
                },
                size,
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("boxel model skin"),
                layout: &self.skin_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
            self.skins.insert(
                t.id(),
                Skin {
                    _texture: texture,
                    group,
                },
            );
        }
        let draws = &self.draws;
        self.skins
            .retain(|id, _| draws.iter().any(|d| d.texture.id() == *id));
        let changed = self
            .scene
            .update(&self.draws, view.render_origin, view.eye_rel, self.camera);
        self.stats.instances = self.scene.instances();
        self.stats.posed = self.scene.last_posed();
        self.stats.skipped += (refused as u64) + u64::from(self.scene.last_skipped());
        self.stats.textures = self.skins.len() as u32;
        self.stats.quads = (self.scene.vertices().len() / 4) as u32;
        self.stats.draws = self.scene.ranges().len() as u32;
        if !changed || self.scene.vertices().is_empty() {
            return;
        }
        let vertices = self.scene.vertices();
        let quads = (vertices.len() / 4) as u32;
        if self.index.is_none() || self.index_quads < quads {
            let n = quads.next_power_of_two().max(MIN_QUADS);
            let indices: Vec<u32> = (0..n)
                .flat_map(|q| [0, 1, 2, 0, 2, 3].map(|i| 4 * q + i))
                .collect();
            self.index = Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("boxel model indices"),
                    contents: bytemuck::cast_slice(&indices),
                    usage: wgpu::BufferUsages::INDEX,
                }),
            );
            self.index_quads = n;
        }
        if self.vertex.is_none() || self.vertex_capacity < vertices.len() {
            let capacity = vertices
                .len()
                .next_power_of_two()
                .max(4 * MIN_QUADS as usize);
            self.vertex = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel model vertices"),
                size: (capacity * size_of::<ModelVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.vertex_capacity = capacity;
        }
        if let Some(buffer) = &self.vertex {
            let bytes: &[u8] = bytemuck::cast_slice(vertices);
            queue.write_buffer(buffer, 0, bytes);
            self.stats.uploads += 1;
            self.stats.upload_bytes += bytes.len() as u64;
        }
    }

    /// Counters of the last frame.
    pub(crate) fn stats(&self) -> ModelStats {
        self.stats
    }

    /// Whether the frame draws anything in `layer`.
    pub(crate) fn draws_in(&self, layer: ModelLayer) -> bool {
        self.scene.ranges().iter().any(|r| r.layer == layer)
    }

    /// Draws the instances of `layer` into `pass`, whose group 1 is the `View`. Leaves group 0
    /// bound to a skin: a later pipeline that reads group 0 sets its own.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, layer: ModelLayer) {
        let (Some(vertex), Some(index)) = (&self.vertex, &self.index) else {
            return;
        };
        let mut bound = false;
        let mut material = None;
        let mut skin = None;
        for r in self.scene.ranges().iter().filter(|r| r.layer == layer) {
            let Some(group) = self.skins.get(&r.texture) else {
                continue;
            };
            if !bound {
                pass.set_vertex_buffer(0, vertex.slice(..));
                pass.set_index_buffer(index.slice(..), wgpu::IndexFormat::Uint32);
                bound = true;
            }
            if material != Some(r.material) {
                pass.set_pipeline(match r.material {
                    ModelMaterial::Opaque => &self.opaque,
                    ModelMaterial::Cutout => &self.cutout,
                });
                material = Some(r.material);
            }
            if skin != Some(r.texture) {
                pass.set_bind_group(model::GROUP_SKIN, &group.group, &[]);
                skin = Some(r.texture);
            }
            pass.draw_indexed(r.first_quad * 6..(r.first_quad + r.quads) * 6, 0, 0..1);
        }
    }
}
