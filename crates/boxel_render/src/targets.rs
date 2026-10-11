//! Main color + depth targets and the blit pipeline.
use crate::{DEPTH_FORMAT, MAIN_COLOR_FORMAT};
use boxel_math::UVec2;
use std::sync::atomic::{AtomicU64, Ordering};

/// Source of [`Targets::generation`].
static GENERATION: AtomicU64 = AtomicU64::new(1);
use boxel_shaders::{ShaderModuleId, create_module};

/// Main color (`MAIN_COLOR_FORMAT`) and depth (`DEPTH_FORMAT`) of one size.
pub(crate) struct Targets {
    /// Main color (`RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC`).
    pub color: wgpu::Texture,
    /// View of `color`.
    pub color_view: wgpu::TextureView,
    /// View of the depth target.
    pub depth_view: wgpu::TextureView,
    /// Usages of the depth target: `RENDER_ATTACHMENT`, plus `TEXTURE_BINDING` when the fog
    /// samples it.
    pub depth_usage: wgpu::TextureUsages,
    /// Size in pixels (never 0).
    pub size: UVec2,
    /// Unique per pair of targets created: what holds views of them (the far LOD's depth
    /// pyramid) knows when to make them again.
    pub generation: u64,
}

impl Targets {
    /// Creates both targets; each dimension is clamped to `1..=max_texture_dimension_2d`. With
    /// `sampled_depth` (only when the fog is on, ADR 0030) the depth can also be bound as a
    /// texture; without it its usage is exactly `RENDER_ATTACHMENT`.
    pub fn new(device: &wgpu::Device, size: UVec2, sampled_depth: bool) -> Self {
        let max = device.limits().max_texture_dimension_2d.max(1);
        let size = size.clamp(UVec2::ONE, UVec2::splat(max));
        let extent = wgpu::Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("boxel main color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: MAIN_COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth_usage = if sampled_depth {
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING
        } else {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        };
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("boxel depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: depth_usage,
            view_formats: &[],
        });
        Self {
            color_view: color.create_view(&wgpu::TextureViewDescriptor::default()),
            depth_view: depth.create_view(&wgpu::TextureViewDescriptor::default()),
            depth_usage,
            color,
            size,
            generation: GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Begins the main pass: clear color, clear depth to 0 (reverse-Z).
    pub fn begin_main_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        clear: [f32; 4],
    ) -> wgpu::RenderPass<'e> {
        self.main_pass(encoder, Some(clear))
    }

    /// Begins the rest of a main pass that was ended to read its depth (the far LOD's depth
    /// pyramid on High): color and depth are loaded as they are.
    pub fn continue_main_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
    ) -> wgpu::RenderPass<'e> {
        self.main_pass(encoder, None)
    }

    /// The main pass, cleared to `clear` (and depth 0), or loading both without it.
    fn main_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        clear: Option<[f32; 4]>,
    ) -> wgpu::RenderPass<'e> {
        let color = match clear {
            Some(c) => wgpu::LoadOp::Clear(wgpu::Color {
                r: f64::from(c[0]),
                g: f64::from(c[1]),
                b: f64::from(c[2]),
                a: f64::from(c[3]),
            }),
            None => wgpu::LoadOp::Load,
        };
        let depth = match clear {
            Some(_) => wgpu::LoadOp::Clear(0.0),
            None => wgpu::LoadOp::Load,
        };
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("boxel main"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: color,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: depth,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }
}

/// Fullscreen blit of the main color target into an output of one format.
pub(crate) struct Blit {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
}

impl Blit {
    /// Pipeline for `output_format` (`fs_blit` when it is sRGB, else `fs_blit_encode_srgb`;
    /// both write alpha 1), bound to `source`.
    pub fn new(
        device: &wgpu::Device,
        output_format: wgpu::TextureFormat,
        source: &wgpu::TextureView,
    ) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel blit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = create_module(device, ShaderModuleId::Blit);
        let fragment_entry = if output_format.is_srgb() {
            "fs_blit"
        } else {
            "fs_blit_encode_srgb"
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("boxel blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(fragment_entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: output_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("boxel blit"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = Self::bind(device, &layout, &sampler, source);
        Self {
            layout,
            pipeline,
            sampler,
            bind_group,
        }
    }

    /// Re-binds to a new source view (after a resize).
    pub fn rebind(&mut self, device: &wgpu::Device, source: &wgpu::TextureView) {
        self.bind_group = Self::bind(device, &self.layout, &self.sampler, source);
    }

    fn bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        source: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("boxel blit"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    /// Encodes the blit into `target` (cleared to black first, then one fullscreen triangle).
    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("boxel blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
