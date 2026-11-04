//! The model pass (ADR 0040): two pipelines (opaque and cutout) shared by every tier, the skins
//! on the GPU, and one vertex buffer the CPU fills with the posed instances
//! ([`super::ModelScene`]) when they change.
//!
//! On High with sun shadows (ADR 0065, lane S5) the world instances also cast into the shadow
//! maps and receive them: four more pipelines of the same vertices, built the first time
//! shadows are on ([`ModelPass::set_shadowed`]).

use super::scene::ModelScene;
use super::{ModelDraw, ModelLayer, ModelMaterial, ModelStats};
use crate::renderer::ViewParams;
use crate::shadow::{CascadeTarget, caster_depth};
use crate::{DEPTH_FORMAT, MAIN_COLOR_FORMAT};
use boxel_math::{Aabb, Quat, Vec3};
use boxel_shaders::gpu::shadow::{GROUP_CASCADE, GROUP_RECEIVE};
use boxel_shaders::gpu::{ModelVertex, bindings, model};
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

/// High's model pipelines of sun shadows (ADR 0065, lane S5).
struct ShadowedModels {
    /// `ModelShadowed`: the twins of `opaque` and `cutout` that receive the shadows (group 3).
    opaque: wgpu::RenderPipeline,
    cutout: wgpu::RenderPipeline,
    /// `ModelShadow`: the boxes' depth in a cascade's map, opaque and cutout.
    cast_opaque: wgpu::RenderPipeline,
    cast_cutout: wgpu::RenderPipeline,
}

/// The pipelines, skins and buffers of the entity models (one per renderer).
pub(crate) struct ModelPass {
    opaque: wgpu::RenderPipeline,
    cutout: wgpu::RenderPipeline,
    /// Sun shadows' pipelines, from the first frame with shadows on (High only).
    shadowed: Option<ShadowedModels>,
    /// They could not be built: the models neither cast nor receive.
    shadowed_failed: bool,
    /// The box of each draw range's vertices (the casters' cascade test), and whether the
    /// vertices changed since it was made.
    bounds: Vec<Aabb>,
    bounds_stale: bool,
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
        groups[bindings::GROUP_VIEW as usize] = Some(view_layout);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel model"),
            bind_group_layouts: &groups,
            immediate_size: 0,
        });
        let build = |label: &str, entry: &str, cull_mode: Option<wgpu::Face>| {
            pipeline(
                device,
                label,
                &layout,
                (&module, "vs_model", Some(entry)),
                cull_mode,
                false,
            )
        };
        Self {
            opaque: build(
                "boxel model opaque",
                "fs_model_opaque",
                Some(wgpu::Face::Back),
            ),
            cutout: build("boxel model cutout", "fs_model_cutout", None),
            shadowed: None,
            shadowed_failed: false,
            bounds: Vec::new(),
            bounds_stale: true,
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
        self.bounds_stale |= changed;
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

    /// High (sun shadows, ADR 0065): builds the pipelines that cast into the maps and receive
    /// them once (the next calls keep them): `view` is the renderer's group 1, `cascade` the
    /// casters' group 3 ([`crate::shadow::ShadowPass::cascade_layout`]), `receive` the
    /// receivers' ([`crate::shadow::receive_layout`]). If they fail, an error is logged and the
    /// models neither cast nor receive on this renderer.
    pub(crate) fn set_shadowed(
        &mut self,
        device: &wgpu::Device,
        view: &wgpu::BindGroupLayout,
        cascade: &wgpu::BindGroupLayout,
        receive: &wgpu::BindGroupLayout,
    ) {
        if self.shadowed.is_some() || self.shadowed_failed {
            return;
        }
        let mut built = None;
        let result = boxel_gfx::run_in_error_scopes(device, || {
            built = Some(self.shadowed_pipelines(device, view, cascade, receive));
            Ok(())
        });
        match (result, built) {
            (Ok(()), Some(shadowed)) => self.shadowed = Some(shadowed),
            (result, _) => {
                tracing::error!(
                    "entity model shadow pipelines: {}; models cast and receive no sun shadow",
                    result.err().unwrap_or_default()
                );
                self.shadowed_failed = true;
            }
        }
    }

    fn shadowed_pipelines(
        &self,
        device: &wgpu::Device,
        view: &wgpu::BindGroupLayout,
        cascade: &wgpu::BindGroupLayout,
        receive: &wgpu::BindGroupLayout,
    ) -> ShadowedModels {
        let mut groups: [Option<&wgpu::BindGroupLayout>; 4] = [None; 4];
        groups[model::GROUP_SKIN as usize] = Some(&self.skin_layout);
        groups[bindings::GROUP_VIEW as usize] = Some(view);
        groups[GROUP_RECEIVE as usize] = Some(receive);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel model shadowed"),
            bind_group_layouts: &groups,
            immediate_size: 0,
        });
        let module = create_module(device, ShaderModuleId::ModelShadowed);
        let receiver = |label: &str, entry: &str, cull_mode| {
            pipeline(
                device,
                label,
                &layout,
                (&module, "vs_model", Some(entry)),
                cull_mode,
                false,
            )
        };
        // The casters read the skin and their cascade, nothing else.
        let mut groups: [Option<&wgpu::BindGroupLayout>; 4] = [None; 4];
        groups[model::GROUP_SKIN as usize] = Some(&self.skin_layout);
        groups[GROUP_CASCADE as usize] = Some(cascade);
        let cast_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel model shadow"),
            bind_group_layouts: &groups,
            immediate_size: 0,
        });
        let cast_module = create_module(device, ShaderModuleId::ModelShadow);
        // Faces seen from the sun are culled: a box casts with those that look away from it.
        let caster = |label: &str, entry: Option<&str>| {
            pipeline(
                device,
                label,
                &cast_layout,
                (&cast_module, "vs_model_shadow", entry),
                Some(wgpu::Face::Front),
                true,
            )
        };
        ShadowedModels {
            opaque: receiver(
                "boxel model shadowed opaque",
                "fs_model_opaque",
                Some(wgpu::Face::Back),
            ),
            cutout: receiver("boxel model shadowed cutout", "fs_model_cutout", None),
            cast_opaque: caster("boxel model shadow opaque", None),
            cast_cutout: caster("boxel model shadow cutout", Some("fs_model_shadow_cutout")),
        }
    }

    /// High with sun shadows: the box of each draw range again if the vertices changed (the
    /// casters' cascade test). Runs before the shadow passes of a frame.
    pub(crate) fn prepare_casters(&mut self) {
        if !self.bounds_stale || self.shadowed.is_none() {
            return;
        }
        self.bounds_stale = false;
        let vertices = self.scene.vertices();
        self.bounds.clear();
        self.bounds.extend(self.scene.ranges().iter().map(|r| {
            let first = r.first_quad as usize * 4;
            let quads = vertices
                .get(first..first + r.quads as usize * 4)
                .unwrap_or_default();
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for v in quads {
                let p = Vec3::from_array(v.pos);
                min = min.min(p);
                max = max.max(p);
            }
            Aabb::new(min, max)
        }));
    }

    /// Draws the world instances whose boxes touch `target`'s cascade into its depth pass, with
    /// the faces that look away from the sun; the number of draws. Nothing before
    /// [`ModelPass::set_shadowed`] and [`ModelPass::prepare_casters`].
    pub(crate) fn draw_casters(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        target: &CascadeTarget<'_>,
    ) -> u32 {
        let (Some(shadowed), Some(vertex), Some(index)) =
            (&self.shadowed, &self.vertex, &self.index)
        else {
            return 0;
        };
        let ranges = self.scene.ranges();
        if self.bounds_stale || self.bounds.len() != ranges.len() {
            return 0;
        }
        let mut draws = 0;
        let mut material = None;
        let mut skin = None;
        for (r, bounds) in ranges.iter().zip(&self.bounds) {
            if r.layer != ModelLayer::World || !target.frustum.intersects_aabb(bounds) {
                continue;
            }
            let Some(group) = self.skins.get(&r.texture) else {
                continue;
            };
            if draws == 0 {
                pass.set_vertex_buffer(0, vertex.slice(..));
                pass.set_index_buffer(index.slice(..), wgpu::IndexFormat::Uint32);
                pass.set_bind_group(GROUP_CASCADE, target.group, &[target.offset]);
            }
            if material != Some(r.material) {
                pass.set_pipeline(match r.material {
                    ModelMaterial::Opaque => &shadowed.cast_opaque,
                    ModelMaterial::Cutout => &shadowed.cast_cutout,
                });
                material = Some(r.material);
            }
            if skin != Some(r.texture) {
                pass.set_bind_group(model::GROUP_SKIN, &group.group, &[]);
                skin = Some(r.texture);
            }
            pass.draw_indexed(r.first_quad * 6..(r.first_quad + r.quads) * 6, 0, 0..1);
            draws += 1;
        }
        draws
    }

    /// Draws the instances of `layer` into `pass`, whose group 1 is the `View`. With
    /// `receivers` (High, a frame with sun shadows: the receivers' group 3) the world layer is
    /// drawn with the pipelines that receive them, if they exist. Leaves group 0 bound to a
    /// skin: a later pipeline that reads group 0 sets its own.
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        layer: ModelLayer,
        receivers: Option<&wgpu::BindGroup>,
    ) {
        let (Some(vertex), Some(index)) = (&self.vertex, &self.index) else {
            return;
        };
        let shadowed = receivers
            .zip(self.shadowed.as_ref())
            .filter(|_| layer == ModelLayer::World);
        let (opaque, cutout) = match shadowed {
            Some((_, s)) => (&s.opaque, &s.cutout),
            None => (&self.opaque, &self.cutout),
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
                if let Some((receivers, _)) = shadowed {
                    pass.set_bind_group(GROUP_RECEIVE, receivers, &[]);
                }
                bound = true;
            }
            if material != Some(r.material) {
                pass.set_pipeline(match r.material {
                    ModelMaterial::Opaque => opaque,
                    ModelMaterial::Cutout => cutout,
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

/// A model pipeline of `layout`: the [`ModelVertex`] buffer into `vertex` (module and entry),
/// then `fragment`'s entry of the same module, if any. A `caster` draws depth only into a shadow
/// map ([`caster_depth`]); the others draw into the main color target, tested and written like
/// the terrain (reverse-Z).
fn pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    (module, vertex, fragment): (&wgpu::ShaderModule, &str, Option<&str>),
    cull_mode: Option<wgpu::Face>,
    caster: bool,
) -> wgpu::RenderPipeline {
    let [o0, o1, o2, o3, o4] = model::ATTRIBUTE_OFFSETS;
    let attribute = |format, offset, shader_location| wgpu::VertexAttribute {
        format,
        offset,
        shader_location,
    };
    let attributes = [
        attribute(wgpu::VertexFormat::Float32x3, o0, 0),
        attribute(wgpu::VertexFormat::Float32x2, o1, 1),
        attribute(wgpu::VertexFormat::Uint16x4, o2, 2),
        attribute(wgpu::VertexFormat::Uint32, o3, 3),
        attribute(wgpu::VertexFormat::Uint32, o4, 4),
    ];
    let color = [Some(wgpu::ColorTargetState {
        format: MAIN_COLOR_FORMAT,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    })];
    let (depth, targets): (_, &[Option<wgpu::ColorTargetState>]) = if caster {
        (caster_depth(), &[])
    } else {
        (
            wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Greater),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            },
            &color,
        )
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(vertex),
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
        depth_stencil: Some(depth),
        multisample: wgpu::MultisampleState::default(),
        fragment: fragment.map(|entry| wgpu::FragmentState {
            module,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}
