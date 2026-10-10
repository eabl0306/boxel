//! The fog passes (ADR 0030): the march into a small integer target and the composite over the
//! main color, both fullscreen triangles of `fog.wgsl`, the same on every tier.

use super::{FogInput, FogSettings};
use crate::MAIN_COLOR_FORMAT;
use crate::renderer::ViewParams;
use boxel_fog::march::MarchLayer;
use boxel_fog::{FogLayer, FogResolution, RayBasis, mean_coverage};
use boxel_math::UVec2;
use boxel_shaders::gpu::{
    FOG_BUFFER_SIZE, FOG_LAYERS_OFFSET, FOG_MAX_LAYERS, FogHeader, FogLayerGpu, bindings, fog,
};
use boxel_shaders::{ShaderModuleId, create_module};
use std::sync::Arc;

/// Format of the fog target: x = color, y = four opacity bytes (`boxel_fog::pack`).
pub(crate) const FOG_TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Uint;

/// The layers one frame marches, as the reference march takes them: sorted by height, without
/// the ones the camera cannot reach (their band is farther above or below the eye than they are
/// drawn), each with its color under the frame's light. `means` are the layers' mean coverages
/// ([`mean_coverage`]), in the order of `settings.layers`.
pub fn frame_layers(
    settings: &FogSettings,
    means: &[f32],
    view: &ViewParams,
    tick: u64,
) -> Vec<MarchLayer> {
    let origin = view.render_origin.to_array();
    let eye_y = view.eye_rel.y;
    let mut order: Vec<usize> = (0..settings.layers.len().min(FOG_MAX_LAYERS)).collect();
    order.sort_by_key(|&i| settings.layers[i].min_y);
    order
        .into_iter()
        .filter_map(|i| {
            let layer = &settings.layers[i];
            let lit = lit_color(layer, view);
            let m = MarchLayer::new(
                layer,
                origin,
                tick,
                lit,
                [view.fog_start, view.fog_end],
                means.get(i).copied().unwrap_or(0.0),
            );
            let [lo, hi] = m.params.band;
            let gap = if eye_y < lo {
                lo - eye_y
            } else if eye_y >= hi {
                eye_y - hi
            } else {
                0.0
            };
            let empty = layer.density <= 0.0 || m.params.threshold >= 4096;
            (!empty && gap < m.reach()).then_some(m)
        })
        .collect()
}

/// A layer's color under the frame's light: its color times the sky light (never below the
/// ambient floor).
fn lit_color(layer: &FogLayer, view: &ViewParams) -> [f32; 3] {
    let light = &view.light;
    std::array::from_fn(|c| layer.color[c] * light.sky[c].max(light.ambient))
}

/// The uniform words of one layer.
fn layer_gpu(m: &MarchLayer) -> FogLayerGpu {
    let p = &m.params;
    let flags = if p.volume { fog::LAYER_VOLUME } else { 0 }
        | if m.uniform { fog::LAYER_UNIFORM } else { 0 };
    let [r, g, b] = m.lit;
    FogLayerGpu {
        band: [p.band[0], p.band[1], m.reach(), m.sigma_mean_log2e],
        fade: [m.fade[0], m.fade[1], p.density_log2e, 0.0],
        grid: [p.grid[0], p.grid[1], p.grid[2], 0.0],
        size: [p.cell[0] as f32, p.cell[1] as f32, p.cell[2] as f32, 0.0],
        lit: [r, g, b, 0.0],
        face: m.face_shade,
        lattice: [p.m0[0] as u32, p.m0[1] as u32, p.m0[2] as u32, p.log2_scale],
        base_lo: [p.lo[0], p.lo[1], p.lo[2], 0],
        base_hi: [p.hi[0], p.hi[1], p.hi[2], p.seed32],
        octave2: [0, p.seed32_2, p.octaves, flags],
        shape: [p.threshold, p.gain, 0, 0],
        wind: [p.q[0], p.q[1], p.q[2], 0],
        cells: [p.band_cells[0], p.band_cells[1], 0, 0],
    }
}

/// The march target of one size.
struct Target {
    texture: wgpu::Texture,
    /// Composite group 0: the target as `texture_2d<u32>`.
    group: wgpu::BindGroup,
    /// Full resolution it serves.
    size: UVec2,
    resolution: FogResolution,
}

/// The fog passes of a renderer. Created by the first [`crate::VoxelRenderer::set_fog`].
pub(crate) struct FogPass {
    march: wgpu::RenderPipeline,
    composite_half: wgpu::RenderPipeline,
    composite_full: wgpu::RenderPipeline,
    depth_layout: wgpu::BindGroupLayout,
    target_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    uniform_group: wgpu::BindGroup,
    /// March group 0: the main depth.
    depth_group: wgpu::BindGroup,
    target: Option<Target>,
    input: Option<FogInput>,
    /// Mean coverage of each layer of `means_for`.
    means: Vec<f32>,
    means_for: Option<Arc<FogSettings>>,
}

impl FogPass {
    /// Pipelines, layouts and the uniform; the march reads `depth` (the main depth target,
    /// created with `TEXTURE_BINDING`).
    pub fn new(device: &wgpu::Device, depth: &wgpu::TextureView) -> Self {
        let module = create_module(device, ShaderModuleId::Fog);
        let texture_layout = |label, binding, sample_type| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }],
            })
        };
        let depth_layout = texture_layout(
            "boxel fog depth",
            bindings::FOG_BINDING_DEPTH,
            wgpu::TextureSampleType::Float { filterable: false },
        );
        let target_layout = texture_layout(
            "boxel fog target",
            bindings::FOG_BINDING_TARGET,
            wgpu::TextureSampleType::Uint,
        );
        let uniform_layout = uniform_layout(device);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel fog"),
            size: FOG_BUFFER_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("boxel fog"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: bindings::FOG_BINDING_UNIFORM,
                resource: uniform.as_entire_binding(),
            }],
        });
        let march_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel fog march"),
            bind_group_layouts: &[Some(&depth_layout), Some(&uniform_layout)],
            immediate_size: 0,
        });
        let composite_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel fog composite"),
            bind_group_layouts: &[Some(&target_layout)],
            immediate_size: 0,
        });
        let march = pipeline(
            device,
            &module,
            &march_layout,
            "fs_fog_march",
            wgpu::ColorTargetState {
                format: FOG_TARGET_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            },
        );
        // dst * (1 - alpha) + color * alpha in linear; alpha stays 1.
        let blend = wgpu::ColorTargetState {
            format: MAIN_COLOR_FORMAT,
            blend: Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::REPLACE,
            }),
            write_mask: wgpu::ColorWrites::COLOR,
        };
        let composite_half = pipeline(
            device,
            &module,
            &composite_layout,
            "fs_fog_composite_half",
            blend.clone(),
        );
        let composite_full = pipeline(
            device,
            &module,
            &composite_layout,
            "fs_fog_composite_full",
            blend,
        );
        let depth_group = depth_group(device, &depth_layout, depth);
        Self {
            march,
            composite_half,
            composite_full,
            depth_layout,
            target_layout,
            uniform,
            uniform_group,
            depth_group,
            target: None,
            input: None,
            means: Vec::new(),
            means_for: None,
        }
    }

    /// The main depth target changed (a resize).
    pub fn rebind_depth(&mut self, device: &wgpu::Device, depth: &wgpu::TextureView) {
        self.depth_group = depth_group(device, &self.depth_layout, depth);
    }

    /// The fog of the next frames.
    pub fn set_input(&mut self, input: FogInput) {
        self.input = Some(input);
    }

    /// The march target texture, if a frame created one.
    pub fn target(&self) -> Option<&wgpu::Texture> {
        self.target.as_ref().map(|t| &t.texture)
    }

    /// Encodes the march and the composite into `color` (the main color target of `size`) for
    /// `view`, after the main pass. Returns the passes recorded (0 or 2) and the layers
    /// marched: nothing is recorded while the fog is off, has no layer the camera can reach, or
    /// the view cannot be inverted.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &ViewParams,
        color: &wgpu::TextureView,
        size: UVec2,
    ) -> (u32, u32) {
        let Some(input) = &self.input else {
            return (0, 0);
        };
        let settings = Arc::clone(&input.settings);
        if !settings.enabled || settings.layers.is_empty() {
            return (0, 0);
        }
        if !self
            .means_for
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(s, &settings))
        {
            self.means = settings.layers.iter().map(mean_coverage).collect();
            self.means_for = Some(Arc::clone(&settings));
        }
        let layers = frame_layers(&settings, &self.means, view, input.tick);
        if layers.is_empty() {
            return (0, 0);
        }
        let Some(basis) = RayBasis::new(view.view_proj.to_cols_array_2d(), view.eye_rel.to_array())
        else {
            return (0, 0);
        };
        let half = settings.resolution == FogResolution::Half;
        let header = FogHeader {
            ray0: extend(basis.ray0, basis.near),
            ray_x: extend(basis.ray_x, size.x as f32),
            ray_y: extend(basis.ray_y, size.y as f32),
            eye: extend(view.eye_rel.to_array(), 0.0),
            counts: [
                layers.len() as u32,
                if half { fog::FLAG_HALF } else { 0 },
                settings.max_steps.clamp(1, boxel_fog::config::MAX_STEPS),
                0,
            ],
            sky: [view.fog_color[0], view.fog_color[1], view.fog_color[2], 0.0],
            debug: [0; 4],
        };
        let mut gpu_layers = [FogLayerGpu::default(); FOG_MAX_LAYERS];
        for (slot, m) in gpu_layers.iter_mut().zip(&layers) {
            *slot = layer_gpu(m);
        }
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&header));
        queue.write_buffer(
            &self.uniform,
            FOG_LAYERS_OFFSET,
            bytemuck::cast_slice(&gpu_layers),
        );
        let target = self.target_for(device, size, settings.resolution);
        let target_view = target
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("boxel fog march"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.march);
            pass.set_bind_group(bindings::FOG_GROUP_DEPTH, &self.depth_group, &[]);
            pass.set_bind_group(bindings::FOG_GROUP_UNIFORM, &self.uniform_group, &[]);
            pass.draw(0..3, 0..1);
        }
        let target = self.target.as_ref().map(|t| &t.group);
        if let Some(group) = target {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("boxel fog composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(if half {
                &self.composite_half
            } else {
                &self.composite_full
            });
            pass.set_bind_group(bindings::FOG_GROUP_TARGET, group, &[]);
            pass.draw(0..3, 0..1);
        }
        (2, layers.len() as u32)
    }

    /// The target for a frame of `size` at `resolution`, created when either changed.
    fn target_for(
        &mut self,
        device: &wgpu::Device,
        size: UVec2,
        resolution: FogResolution,
    ) -> &Target {
        let stale = self
            .target
            .as_ref()
            .is_none_or(|t| t.size != size || t.resolution != resolution);
        if stale {
            let extent = match resolution {
                FogResolution::Half => UVec2::new(size.x.div_ceil(2), size.y.div_ceil(2)),
                FogResolution::Full => size,
            };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("boxel fog target"),
                size: wgpu::Extent3d {
                    width: extent.x.max(1),
                    height: extent.y.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FOG_TARGET_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("boxel fog target"),
                layout: &self.target_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: bindings::FOG_BINDING_TARGET,
                    resource: wgpu::BindingResource::TextureView(
                        &texture.create_view(&wgpu::TextureViewDescriptor::default()),
                    ),
                }],
            });
            self.target = Some(Target {
                texture,
                group,
                size,
                resolution,
            });
        }
        self.target.as_ref().expect("target created above")
    }
}

fn extend(v: [f32; 3], w: f32) -> [f32; 4] {
    [v[0], v[1], v[2], w]
}

fn uniform_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("boxel fog uniform"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: bindings::FOG_BINDING_UNIFORM,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(FOG_BUFFER_SIZE),
            },
            count: None,
        }],
    })
}

fn depth_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    depth: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("boxel fog depth"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: bindings::FOG_BINDING_DEPTH,
            resource: wgpu::BindingResource::TextureView(depth),
        }],
    })
}

fn pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    fragment: &str,
    target: wgpu::ColorTargetState,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fragment),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_fullscreen"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(target)],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Reads a texture of 8-byte texels (`Rg32Uint`) or 16-byte texels (`Rgba32Uint`) back as
/// words, row by row.
pub(crate) fn read_words(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u32>, String> {
    let texel = match texture.format() {
        wgpu::TextureFormat::Rg32Uint => 8,
        wgpu::TextureFormat::Rgba32Uint => 16,
        other => return Err(format!("read_words: unsupported format {other:?}")),
    };
    let (width, height) = (texture.width(), texture.height());
    let unpadded = width * texel;
    let padded =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel fog readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("boxel fog readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| e.to_string())?;
    rx.recv()
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let mapped = slice.get_mapped_range().map_err(|e| e.to_string())?;
    let mut words = Vec::with_capacity((unpadded / 4 * height) as usize);
    for row in mapped.chunks_exact(padded as usize) {
        words.extend(
            row[..unpadded as usize]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&b| u32::from_le_bytes(b)),
        );
    }
    drop(mapped);
    buffer.unmap();
    Ok(words)
}

/// Testing: the noise and coverage `fs_fog_debug_coverage` computes for layer `layer` at
/// `render_origin` and `tick`, over the cells `first + (x, 0, z)` for `x < size[0]` and
/// `z < size[1]` (relative to the origin's cell), row by row: `[coverage, noise]` each.
#[doc(hidden)]
pub fn debug_coverage(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layer: &FogLayer,
    render_origin: [i64; 3],
    tick: u64,
    first: [i32; 3],
    size: [u32; 2],
) -> Result<Vec<[u32; 2]>, String> {
    let mut words = Vec::new();
    boxel_gfx::run_in_error_scopes(device, || {
        let module = create_module(device, ShaderModuleId::Fog);
        let uniform_layout = uniform_layout(device);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel fog debug"),
            bind_group_layouts: &[None, Some(&uniform_layout)],
            immediate_size: 0,
        });
        let pipeline = pipeline(
            device,
            &module,
            &layout,
            "fs_fog_debug_coverage",
            wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba32Uint,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            },
        );
        let m = MarchLayer::new(layer, render_origin, tick, [1.0; 3], [0.0, 1.0], 0.0);
        let header = FogHeader {
            debug: [first[0], first[1], first[2], 0],
            ..FogHeader::default()
        };
        let mut layers = [FogLayerGpu::default(); FOG_MAX_LAYERS];
        layers[0] = layer_gpu(&m);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel fog debug"),
            size: FOG_BUFFER_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&header));
        queue.write_buffer(&uniform, FOG_LAYERS_OFFSET, bytemuck::cast_slice(&layers));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("boxel fog debug"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: bindings::FOG_BINDING_UNIFORM,
                resource: uniform.as_entire_binding(),
            }],
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("boxel fog debug"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Uint,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("boxel fog debug"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("boxel fog debug"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(bindings::FOG_GROUP_UNIFORM, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
        words = read_words(device, queue, &texture)?;
        Ok(())
    })?;
    Ok(words
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| [w[0], w[1]])
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{DEFAULT_SKY_COLOR, ViewParams};
    use boxel_camera::{Camera, CameraView};
    use boxel_math::DVec3;
    use boxel_transform::Transform;

    fn view_at(y: f64) -> ViewParams {
        let camera = Camera::default();
        let transform = Transform::from_yaw_pitch(DVec3::new(0.5, y, 0.5), 0.0, 0.0);
        ViewParams::from_camera(
            &CameraView::compute(&camera, &transform, 1.0),
            &camera,
            DEFAULT_SKY_COLOR,
            0.6,
        )
    }

    #[test]
    fn frame_layers_are_sorted_and_culled() {
        let mut high = FogLayer::new(200, 204);
        high.max_distance = 100.0;
        let low = FogLayer::new(0, 8);
        let mut empty = FogLayer::new(40, 48);
        empty.threshold = 1.0;
        let settings = FogSettings::new(vec![high, low, empty]);
        let means = [1.0, 1.0, 0.0];
        let near_low = frame_layers(&settings, &means, &view_at(4.5), 0);
        assert_eq!(
            near_low.len(),
            1,
            "the high layer is out of reach, the empty one never shows"
        );
        assert_eq!(near_low[0].params.band, [-4.0, 4.0]);
        let between = frame_layers(&settings, &means, &view_at(150.5), 0);
        assert_eq!(between.len(), 2);
        assert!(
            between[0].params.band[0] < between[1].params.band[0],
            "sorted by height"
        );
        // Lit by the noon sky: white stays white.
        assert_eq!(between[0].lit, [1.0; 3]);
    }

    #[test]
    fn layer_words_carry_the_split() {
        let mut layer = FogLayer::new(-8, 8);
        layer.cell = [2, 2, 2];
        layer.noise.dims = boxel_fog::NoiseDims::Volume;
        layer.noise.octaves = 2;
        layer.noise.scale = 4;
        layer.threshold = 0.5;
        let m = MarchLayer::new(&layer, [-5, 3, 1 << 40], 99, [0.5; 3], [10.0, 20.0], 0.25);
        let gpu = layer_gpu(&m);
        assert_eq!(gpu.octave2[2], 2);
        assert_eq!(gpu.octave2[3], fog::LAYER_VOLUME);
        // Every cell full (threshold 0, softness 0): the march takes the layer whole.
        layer.threshold = 0.0;
        let full = MarchLayer::new(&layer, [-5, 3, 1 << 40], 99, [0.5; 3], [10.0, 20.0], 1.0);
        assert_eq!(
            layer_gpu(&full).octave2[3],
            fog::LAYER_VOLUME | fog::LAYER_UNIFORM
        );
        assert_eq!(gpu.lattice[3], 2);
        assert_eq!(gpu.base_hi[0], u32::MAX, "a negative lattice point");
        assert_eq!(gpu.fade[..2], [10.0, 20.0]);
        assert_eq!(gpu.band[3], m.params.density_log2e * 0.25);
        assert_eq!(gpu.cells[..2], m.params.band_cells);
    }
}
