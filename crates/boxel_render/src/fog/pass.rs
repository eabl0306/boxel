//! The fog passes (ADR 0030): the march into a small integer target and the composite over the
//! main color, both fullscreen triangles of `fog.wgsl`, the same on every tier.

use super::{FogInput, FogSettings};
use crate::MAIN_COLOR_FORMAT;
use crate::renderer::ViewParams;
use boxel_fog::march::MarchLayer;
use boxel_fog::{FogLayer, FogResolution, FogTime, HazeProfile, RayBasis, haze_profile};
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
/// drawn), each with its color under the frame's light and its fade. For
/// [`boxel_fog::FogFade::Scene`] that is the terrain's, or the view's distance fog at the
/// layer's own end if it ends first; a layer with haze takes the farthest terrain's instead:
/// `lod_fade` (the far LOD's fade at its horizon) while the LOD draws, else the view's.
/// `profiles` are the layers' haze profiles ([`haze_profile`]), in the order of
/// `settings.layers`.
pub fn frame_layers(
    settings: &FogSettings,
    profiles: &[HazeProfile],
    view: &ViewParams,
    lod_fade: Option<[f32; 2]>,
    time: impl Into<FogTime>,
) -> Vec<MarchLayer> {
    let time = time.into();
    let origin = view.render_origin.to_array();
    let eye_y = view.eye_rel.y;
    let mut order: Vec<usize> = (0..settings.layers.len().min(FOG_MAX_LAYERS)).collect();
    order.sort_by_key(|&i| settings.layers[i].min_y);
    order
        .into_iter()
        .filter_map(|i| {
            let layer = &settings.layers[i];
            let lit = lit_color(layer, view);
            // A layer with haze reaches as far as the terrain is drawn and fades with it.
            let fade = if layer.haze > 0.0 {
                lod_fade.unwrap_or([view.fog_start, view.fog_end])
            } else {
                scene_fade(layer, view)
            };
            let m = MarchLayer::new(
                layer,
                origin,
                time,
                lit,
                fade,
                profiles.get(i).copied().unwrap_or_default(),
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
            (!empty && gap < m.reach).then_some(m)
        })
        .collect()
}

/// The fade of `layer` without haze if it fades like the scene: the terrain's (`view.fog_start`
/// to `view.fog_end`), or for a layer that ends first (its `max_distance`), the view's distance
/// fog at that end. Like the terrain, then, it fades out over the last stretch it is drawn
/// instead of stopping in a hard line where the terrain is still clear (with the fog off, both
/// end at once). The layers keep it while the far LOD draws (ADR 0031).
fn scene_fade(layer: &FogLayer, view: &ViewParams) -> [f32; 2] {
    if layer.max_distance >= view.fog_end {
        [view.fog_start, view.fog_end]
    } else {
        view.distance_fog.range(layer.max_distance)
    }
}

/// A layer's color under the frame's light: its color times the sky light (never below the
/// ambient floor), then `sky_tint` of the way to the sky color (warm at dusk).
fn lit_color(layer: &FogLayer, view: &ViewParams) -> [f32; 3] {
    let light = &view.light;
    std::array::from_fn(|c| {
        let lit = layer.color[c] * light.sky[c].max(light.ambient);
        lit + (view.fog_color[c] - lit) * layer.sky_tint
    })
}

/// The uniform words of one layer.
fn layer_gpu(m: &MarchLayer) -> FogLayerGpu {
    let p = &m.params;
    let flags = if p.volume { fog::LAYER_VOLUME } else { 0 }
        | if m.uniform { fog::LAYER_UNIFORM } else { 0 };
    let [r, g, b] = m.lit;
    let k = m.profile.knots;
    FogLayerGpu {
        band: [p.band[0], p.band[1], m.reach, m.cells_end],
        fade: [m.fade[0], m.fade[1], p.density_log2e, m.blend_start],
        grid: [p.grid[0], p.grid[1], p.grid[2], m.haze],
        size: [
            p.cell[0] as f32,
            p.cell[1] as f32,
            p.cell[2] as f32,
            m.profile.spacing,
        ],
        lit: [r, g, b, m.haze_bottom()],
        face: m.face_shade,
        soft: [m.soft[0], m.soft[1], m.height_shade[0], m.height_shade[1]],
        lattice: [p.m0[0] as u32, p.m0[1] as u32, p.m0[2] as u32, p.log2_scale],
        base_lo: [p.lo[0], p.lo[1], p.lo[2], 0],
        base_hi: [p.hi[0], p.hi[1], p.hi[2], p.seed32],
        octave2: [0, p.seed32_2, p.octaves, flags],
        shape: [p.threshold, p.gain, p.rise, p.rows],
        wind: [p.q[0], p.q[1], p.q[2], 0],
        cells: [
            p.band_cells[0],
            p.band_cells[1],
            p.edges[0] as i32,
            p.edges[1] as i32,
        ],
        profile_a: [k[0], k[1], k[2], k[3]],
        profile_b: [k[4], k[5], k[6], k[7]],
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
    /// The haze profile of each layer of `profiles_for`.
    profiles: Vec<HazeProfile>,
    profiles_for: Option<Arc<FogSettings>>,
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
            profiles: Vec::new(),
            profiles_for: None,
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
    /// `view`, after the main pass; `lod_fade` is the far LOD's fade while it draws (layers
    /// with haze take it, [`frame_layers`]). Returns the passes recorded (0 or 2) and the
    /// layers marched: nothing is recorded while the fog is off, has no layer the camera can
    /// reach, or the view cannot be inverted.
    #[expect(
        clippy::too_many_arguments,
        reason = "the renderer's one call, its targets and the frame"
    )]
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &ViewParams,
        lod_fade: Option<[f32; 2]>,
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
            .profiles_for
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(s, &settings))
        {
            self.profiles = settings.layers.iter().map(haze_profile).collect();
            self.profiles_for = Some(Arc::clone(&settings));
        }
        let layers = frame_layers(&settings, &self.profiles, view, lod_fade, input.time);
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
/// `render_origin` and `time`, at `sub` (1/4096 cells per axis) past the min corner of the cells
/// `first + (x, 0, z)` for `x < size[0]` and `z < size[1]` (relative to the origin's cell), row
/// by row: `[coverage, noise, edge]` each (the coverage at the threshold of the cell's row; the
/// soft fog's edge weight at height `y0 + (x + 64 z) / 16`, relative to the render origin).
#[doc(hidden)]
#[expect(
    clippy::too_many_arguments,
    reason = "a test helper, one call site per test"
)]
pub fn debug_coverage(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layer: &FogLayer,
    render_origin: [i64; 3],
    time: impl Into<FogTime>,
    first: [i32; 3],
    sub: i32,
    y0: f32,
    size: [u32; 2],
) -> Result<Vec<[u32; 3]>, String> {
    let header = FogHeader {
        counts: [0, 0, 0, y0.to_bits()],
        debug: [first[0], first[1], first[2], sub],
        ..FogHeader::default()
    };
    let words = debug_draw(
        device,
        queue,
        "fs_fog_debug_coverage",
        layer,
        render_origin,
        time.into(),
        &[header],
        size,
    )?;
    Ok(words[0].iter().map(|w| [w[0], w[1], w[2]]).collect())
}

/// Testing: walks through one noise cache as the march takes them (`fs_fog_debug_walk`), in
/// layer `layer` at `render_origin` and `time`. Per walk `(first, code)`, row by row: pixel
/// `(x, y)` starts at cell `first` plus `y` cells along z (x when walking z) and steps `x` cells
/// along the axis and sign `code` (0 to 5: +x, -x, +y, -y, +z, -z); it gives `[coverage, noise,
/// step threshold, empty]` of its last cell (the coverage at its row's threshold; the threshold
/// and the emptiness of its first-octave lattice step, as `boxel_fog::noise::StepBound`).
#[doc(hidden)]
pub fn debug_walk(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layer: &FogLayer,
    render_origin: [i64; 3],
    time: impl Into<FogTime>,
    walks: &[([i32; 3], u32)],
    size: [u32; 2],
) -> Result<Vec<Vec<[u32; 4]>>, String> {
    let headers: Vec<FogHeader> = walks
        .iter()
        .map(|&(first, code)| FogHeader {
            debug: [first[0], first[1], first[2], code as i32],
            ..FogHeader::default()
        })
        .collect();
    debug_draw(
        device,
        queue,
        "fs_fog_debug_walk",
        layer,
        render_origin,
        time.into(),
        &headers,
        size,
    )
}

/// Draws the debug entry `entry` with layer `layer` once per header into an `Rgba32Uint` target
/// of `size` and reads each back, row by row.
#[expect(clippy::too_many_arguments, reason = "the debug entries' shared draw")]
fn debug_draw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    entry: &str,
    layer: &FogLayer,
    render_origin: [i64; 3],
    time: FogTime,
    headers: &[FogHeader],
    size: [u32; 2],
) -> Result<Vec<Vec<[u32; 4]>>, String> {
    let mut out = Vec::with_capacity(headers.len());
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
            entry,
            wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba32Uint,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            },
        );
        let m = MarchLayer::new(
            layer,
            render_origin,
            time,
            [1.0; 3],
            [0.0, 1.0],
            HazeProfile::default(),
        );
        let mut layers = [FogLayerGpu::default(); FOG_MAX_LAYERS];
        layers[0] = layer_gpu(&m);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel fog debug"),
            size: FOG_BUFFER_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
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
        for header in headers {
            queue.write_buffer(&uniform, 0, bytemuck::bytes_of(header));
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
            let words = read_words(device, queue, &texture)?;
            out.push(words.as_chunks::<4>().0.to_vec());
        }
        Ok(())
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DistanceFog;
    use crate::renderer::{DEFAULT_SKY_COLOR, ViewParams};
    use boxel_camera::{Camera, CameraView};
    use boxel_math::DVec3;
    use boxel_transform::Transform;

    fn view_at(y: f64) -> ViewParams {
        view_with(y, DistanceFog::default())
    }

    fn view_with(y: f64, fog: DistanceFog) -> ViewParams {
        view_to(y, Camera::default().view_distance, fog)
    }

    fn profiles(settings: &FogSettings) -> Vec<HazeProfile> {
        settings.layers.iter().map(haze_profile).collect()
    }

    fn view_to(y: f64, view_distance: f32, fog: DistanceFog) -> ViewParams {
        let camera = Camera {
            view_distance,
            ..Camera::default()
        };
        let transform = Transform::from_yaw_pitch(DVec3::new(0.5, y, 0.5), 0.0, 0.0);
        ViewParams::from_camera(
            &CameraView::compute(&camera, &transform, 1.0),
            &camera,
            DEFAULT_SKY_COLOR,
            fog,
        )
    }

    /// Layers faded like the scene take the terrain's distance fog at the view distance, and
    /// end there; with it off they end there at once, as the terrain does. Layers with their
    /// own range keep it either way.
    #[test]
    fn scene_layers_follow_the_distance_fog() {
        let scene = FogLayer::new(0, 8);
        let mut ranged = FogLayer::new(16, 24);
        ranged.fade = boxel_fog::FogFade::Range {
            start: 10.0,
            end: 20.0,
        };
        let settings = FogSettings::new(vec![scene, ranged]);
        let profiles = profiles(&settings);
        let distance = Camera::default().view_distance;
        let on = frame_layers(&settings, &profiles, &view_at(4.5), None, 0);
        assert_eq!(on[0].fade, DistanceFog::default().range(distance));
        assert_eq!(on[0].fade, [distance - 0.1 * distance, distance]);
        assert_eq!(on[0].reach, distance);
        assert_eq!(on[1].fade, [10.0, 20.0]);
        assert!((on[0].fade_at(distance - 0.05 * distance) - 0.5).abs() < 1e-5);
        let off_view = view_with(4.5, DistanceFog::off());
        let off = frame_layers(&settings, &profiles, &off_view, None, 0);
        assert_eq!(off[0].fade, [distance, distance]);
        assert_eq!(off[0].reach, distance);
        for d in [0.0, distance - 1.0, distance] {
            assert_eq!(off[0].fade_at(d), 0.0, "{d}");
        }
        assert_eq!(off[0].fade_at(distance + 1.0), 1.0);
        assert_eq!(off[1].fade, [10.0, 20.0]);
        assert_eq!(off[1].reach, 20.0);
        assert_eq!(layer_gpu(&off[0]).band[2], off[0].reach);
        // Without haze they keep it while the far LOD draws (ADR 0031).
        let lod = frame_layers(
            &settings,
            &profiles,
            &view_at(4.5),
            Some([12_224.0, 12_288.0]),
            0,
        );
        assert_eq!(lod, on);
    }

    /// A layer faded like the scene that ends before the terrain does fades at its own end by
    /// the distance fog's rule, instead of stopping in a hard line inside the clear stretch
    /// (a valley fog drawn to 384 blocks, at 16 chunks: the terrain fades from 460.8 to 512).
    #[test]
    fn scene_layers_that_end_first_fade_at_their_end() {
        let mut short = FogLayer::new(0, 8);
        short.max_distance = 384.0;
        let mut inside = FogLayer::new(16, 24);
        inside.max_distance = 500.0;
        let mut long = FogLayer::new(32, 40);
        long.max_distance = 600.0;
        let settings = FogSettings::new(vec![short, inside, long]);
        let profiles = profiles(&settings);
        let fog = DistanceFog::default();
        let layers = frame_layers(&settings, &profiles, &view_to(4.5, 512.0, fog), None, 0);
        assert_eq!(layers[0].fade, fog.range(384.0));
        assert_eq!(layers[1].fade, fog.range(500.0));
        assert_eq!(layers[2].fade, [460.8, 512.0], "the terrain's");
        for (layer, end) in layers.iter().zip([384.0, 500.0, 512.0]) {
            assert_eq!(layer.reach, end);
            assert_eq!(layer.fade_at(end), 1.0, "faded out at {end}");
        }
        // The game's rule, and off: a hard end at the layer's own end.
        let capped = DistanceFog {
            max_fade: 16.0,
            ..fog
        };
        let layers = frame_layers(&settings, &profiles, &view_to(4.5, 512.0, capped), None, 0);
        assert_eq!(layers[0].fade, [368.0, 384.0]);
        let off = DistanceFog::off();
        let layers = frame_layers(&settings, &profiles, &view_to(4.5, 512.0, off), None, 0);
        assert_eq!(layers[0].fade, [384.0, 384.0]);
        assert_eq!(layers[2].fade, [512.0, 512.0]);
    }

    /// A layer with haze reaches as far as the terrain is drawn and fades with it: the view's
    /// end without LOD (not its own end's fade), the LOD's horizon while the LOD draws; with the
    /// terrain's fog off it ends there at once. A layer with its own range keeps it.
    #[test]
    fn hazy_scene_layers_take_the_far_terrains_fade() {
        let mut valley = FogLayer::new(0, 8);
        (valley.max_distance, valley.haze) = (384.0, 1.0);
        let mut ranged = FogLayer::new(16, 24);
        ranged.haze = 1.0;
        ranged.fade = boxel_fog::FogFade::Range {
            start: 1000.0,
            end: 2000.0,
        };
        let settings = FogSettings::new(vec![valley, ranged]);
        let profiles = profiles(&settings);
        let fog = DistanceFog::default();
        let view = view_to(4.5, 512.0, fog);
        let far = frame_layers(&settings, &profiles, &view, None, 0);
        assert_eq!(far[0].fade, [460.8, 512.0], "not range(384)");
        assert_eq!((far[0].cells_end, far[0].reach), (384.0, 512.0));
        assert_eq!(far[0].blend_start, 288.0);
        let horizon = [12_224.0, 12_288.0];
        let lod = frame_layers(&settings, &profiles, &view, Some(horizon), 0);
        assert_eq!(lod[0].fade, horizon);
        assert_eq!((lod[0].cells_end, lod[0].reach), (384.0, 12_288.0));
        for layers in [&far, &lod] {
            assert_eq!(layers[1].fade, [1000.0, 2000.0], "a range ignores the LOD");
            assert_eq!(layers[1].reach, 2000.0);
        }
        // The terrain's fog off: a hard end at the same distance, as the terrain.
        let off = view_to(4.5, 512.0, DistanceFog::off());
        let ended = frame_layers(&settings, &profiles, &off, None, 0);
        assert_eq!((ended[0].fade, ended[0].reach), ([512.0, 512.0], 512.0));
        let ended = frame_layers(&settings, &profiles, &off, Some([12_288.0; 2]), 0);
        assert_eq!((ended[0].fade, ended[0].reach), ([12_288.0; 2], 12_288.0));
        // A view that ends before the cells: no haze, as without it.
        let short = frame_layers(&settings, &profiles, &view_to(4.5, 340.0, fog), None, 0);
        assert_eq!((short[0].cells_end, short[0].reach), (340.0, 340.0));
        // A band out of the camera's old reach is in the haze's.
        let high = view_to(500.0, 512.0, fog);
        assert_eq!(frame_layers(&settings, &profiles, &high, None, 0).len(), 2);
        let mut plain = settings.clone();
        plain.layers[0].haze = 0.0;
        assert_eq!(frame_layers(&plain, &profiles, &high, None, 0).len(), 1);
    }

    #[test]
    fn frame_layers_are_sorted_and_culled() {
        let mut high = FogLayer::new(200, 204);
        high.max_distance = 100.0;
        let low = FogLayer::new(0, 8);
        let mut empty = FogLayer::new(40, 48);
        empty.threshold = 1.0;
        let settings = FogSettings::new(vec![high, low, empty]);
        let profiles = profiles(&settings);
        let near_low = frame_layers(&settings, &profiles, &view_at(4.5), None, 0);
        assert_eq!(
            near_low.len(),
            1,
            "the high layer is out of reach, the empty one never shows"
        );
        assert_eq!(near_low[0].params.band, [-4.0, 4.0]);
        let between = frame_layers(&settings, &profiles, &view_at(150.5), None, 0);
        assert_eq!(between.len(), 2);
        assert!(
            between[0].params.band[0] < between[1].params.band[0],
            "sorted by height"
        );
        // Lit by the noon sky: white stays white.
        assert_eq!(between[0].lit, [1.0; 3]);
    }

    #[test]
    fn the_sky_tint_takes_the_sky_color() {
        let mut view = view_at(4.5);
        view.fog_color = [1.0, 0.5, 0.25, 1.0];
        let lit = |tint: f32| {
            let mut layer = FogLayer::new(0, 8);
            layer.color = [0.5; 3];
            layer.sky_tint = tint;
            let settings = FogSettings::new(vec![layer]);
            frame_layers(&settings, &profiles(&settings), &view, None, 0)[0].lit
        };
        assert_eq!(lit(0.0), [0.5; 3], "lit by the noon sky");
        assert_eq!(lit(1.0), [1.0, 0.5, 0.25]);
        assert_eq!(lit(0.5), [0.75, 0.5, 0.375]);
    }

    #[test]
    fn layer_words_carry_the_split() {
        let mut layer = FogLayer::new(-8, 8);
        layer.cell = [2, 2, 2];
        layer.noise.dims = boxel_fog::NoiseDims::Volume;
        layer.noise.octaves = 2;
        layer.noise.scale = 4;
        layer.threshold = 0.5;
        let origin = [-5, 3, 1 << 40];
        let level = HazeProfile::level(&layer, 0.25);
        let m = MarchLayer::new(&layer, origin, 99, [0.5; 3], [10.0, 20.0], level);
        let gpu = layer_gpu(&m);
        assert_eq!(gpu.octave2[2], 2);
        assert_eq!(gpu.octave2[3], fog::LAYER_VOLUME);
        // Every cell full (threshold 0, softness 0): the march takes the layer whole.
        layer.threshold = 0.0;
        let full = MarchLayer::new(&layer, origin, 99, [0.5; 3], [10.0, 20.0], level);
        assert_eq!(
            layer_gpu(&full).octave2[3],
            fog::LAYER_VOLUME | fog::LAYER_UNIFORM
        );
        assert_eq!(gpu.lattice[3], 2);
        assert_eq!(gpu.base_hi[0], u32::MAX, "a negative lattice point");
        assert_eq!(gpu.fade[..2], [10.0, 20.0]);
        // Without haze: reach, cells end and blend start are the same distance.
        assert_eq!(gpu.band[2..], [20.0, 20.0]);
        assert_eq!((gpu.fade[3], gpu.grid[3]), (20.0, 0.0));
        let d = m.params.density_log2e;
        assert_eq!(gpu.profile_a, [d * 0.25; 4]);
        assert_eq!(gpu.profile_b, [d * 0.25; 4]);
        assert_eq!(
            gpu.cells,
            [m.params.band_cells[0], m.params.band_cells[1], 0, 0]
        );
        // Edges: their depths, and the profile's span inside them; never uniform. With haze
        // past the cells: the reach, the cells' end, the blend's start and the haze.
        layer.edge_bottom = 4;
        layer.edge_top = 10;
        layer.haze = 0.5;
        layer.max_distance = 12.0;
        let profile = haze_profile(&layer);
        let edged = MarchLayer::new(&layer, origin, 99, [0.5; 3], [10.0, 20.0], profile);
        let gpu = layer_gpu(&edged);
        assert_eq!(gpu.cells[2..], [4, 10]);
        assert_eq!(gpu.octave2[3], fog::LAYER_VOLUME, "edges are never uniform");
        assert_eq!(gpu.band[2..], [20.0, 12.0]);
        assert_eq!((gpu.fade[3], gpu.grid[3]), (9.0, 0.5));
        assert_eq!((profile.bottom, profile.spacing), (2.0, 12.0 / 7.0));
        assert_eq!(gpu.size[3], profile.spacing);
        // The first knot two blocks over the band's bottom (-8, 3 under the origin).
        assert_eq!(gpu.lit[3], -11.0 + 2.0);
        let k = profile.knots;
        assert_eq!(gpu.profile_a, [k[0], k[1], k[2], k[3]]);
        assert_eq!(gpu.profile_b, [k[4], k[5], k[6], k[7]]);
    }
}
