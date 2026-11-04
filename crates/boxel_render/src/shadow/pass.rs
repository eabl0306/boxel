//! The sun shadow maps on High (sun shadows plan sec. 4.2 to 4.4, ADR 0065, lanes S3 and S4):
//! the caster cull and one depth pass per cascade, before the main pass, and the group the
//! receivers read them through (`sun_shadow.wgsl`, [`ShadowPass::receivers`]).
//!
//! - The cull (`shadow_cull.wgsl`) runs one thread per chunk slot, like `cull.wgsl`, and writes
//!   per cascade and page an opaque list (the face buckets that look away from the sun) and a
//!   cutout list, each sized for [`PAGE_CASTERS`] chunks of a page. A full list drops casters,
//!   which leaves today's look; [`ShadowStats::dropped`] counts them.
//! - Each cascade is one depth-only render pass into its layer of a `Depth32Float` array,
//!   cleared to 1 (the far side), with 2 multi-draws per page holding chunks: the opaque buckets
//!   without a fragment stage, then the cutout bucket with its alpha test.
//! - The receivers bind the maps, a `LessEqual` comparison sampler (linear: one 2x2 PCF
//!   compare) and the whole uniform, whose entry 0 also holds the camera realm's strength `k`
//!   ([`receive_layout`]). Only a frame that drew its maps has them.
//!
//! The resources exist only while shadows are on ([`crate::VoxelRenderer::set_sun_shadows`]):
//! with shadows off High allocates nothing and records nothing. A frame without a shadow (night,
//! a realm without the sun, no sky-lit chunk in the camera's realm) records nothing either.
use super::{ShadowCascades, SunShadowSettings};
use crate::DEPTH_FORMAT;
use crate::memory::{ARGS_BYTES, PageSet};
use boxel_shaders::gpu::chunk_memory::MAX_PAGES;
use boxel_shaders::gpu::shadow::{
    CASCADE_STRIDE, CULL_WORKGROUP_SIZE, GROUP_CASCADE, MAX_CASCADES, RECEIVE_MAPS,
    RECEIVE_SAMPLER, RECEIVE_UNIFORM, STATE_DROPPED_OFFSET, STATE_SIZE, ShadowCascadeGpu,
    UNIFORM_SIZE,
};
use boxel_shaders::gpu::{DrawRef, bindings};
use boxel_shaders::{ShaderModuleId, create_module};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Chunks of one page one cascade's lists hold (fewer if a page holds fewer): 3 opaque draws and
/// 1 cutout draw each. A cascade box at the default 160 blocks meets a few hundred sky-lit
/// chunks, so a full list is rare; it drops casters (today's look), never draws out of range.
/// About 0.9 MiB of lists per cascade at 4 pages.
pub const PAGE_CASTERS: u32 = 2048;

/// Constant depth bias of the casters, in units of the depth format's precision.
const DEPTH_BIAS_CONSTANT: i32 = 1;
/// Slope-scaled depth bias of the casters. Back faces leave a block of margin behind every lit
/// face (plan sec. 4.2), so a small bias covers box faces down to 1/16 block.
const DEPTH_BIAS_SLOPE: f32 = 1.5;

/// What the shadow passes did in the last frame ([`crate::RenderStats::shadows`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShadowStats {
    /// Cascades drawn (0: shadows off, or nothing to shadow this frame).
    pub cascades: u32,
    /// Multi-draws recorded into the maps: 2 per cascade and page holding chunks.
    pub draws: u32,
    /// Buckets a full list left out, in the last frame read back (a frame or two behind).
    pub dropped: u32,
    /// Bytes of the shadow resources (maps, lists, state, uniform); 0 while shadows are off.
    pub bytes: u64,
}

/// The bind group layouts of the voxel pipelines the shadow draws share (groups 0 to 2).
pub(crate) struct VoxelGroups<'a> {
    /// Group 0: block textures and sampler.
    pub materials: &'a wgpu::BindGroupLayout,
    /// Group 1: `View`.
    pub view: &'a wgpu::BindGroupLayout,
    /// Group 2: a page and the chunk table.
    pub geometry: &'a wgpu::BindGroupLayout,
}

/// What one frame of shadows reads from the renderer.
pub(crate) struct ShadowFrame<'a> {
    /// The frame's cascades ([`super::shadow_cascades`]).
    pub cascades: &'a ShadowCascades,
    /// The camera realm's voxels `[below, above)` in `y` relative to the render origin.
    pub band: [i32; 2],
    /// The camera's realm.
    pub realm: u8,
    /// The camera realm's shadow strength `k` (above 0: a frame without shadow draws nothing).
    pub strength: f32,
    /// Pages with High draw lists (`L`): every page id is below it.
    pub list_pages: u32,
    /// Chunks a page holds at most.
    pub page_chunks: u32,
    /// Chunk slots to cull (`View::counts.x`).
    pub slots: u32,
    /// The `View` uniform (the cull's binding 0).
    pub view: &'a wgpu::Buffer,
    /// Group 1 of the voxel pipelines (the `View`).
    pub view_group: &'a wgpu::BindGroup,
    /// Group 0 of the voxel pipelines.
    pub materials: &'a wgpu::BindGroup,
    /// The GPU chunk table.
    pub table: &'a wgpu::Buffer,
    /// The shared quad index buffer.
    pub index: &'a wgpu::Buffer,
    /// The pages (group 2 of each).
    pub pages: &'a PageSet,
}

/// The lists of one size: `cascades * pages * 4 * casters` draws and their refs.
struct Lists {
    args: wgpu::Buffer,
    refs: wgpu::Buffer,
    /// (cascades, pages, chunks of a page) they were sized for.
    shape: (u32, u32, u32),
}

/// A read back of the dropped count: idle, copied this frame, mapping, or mapped.
const SAMPLE_IDLE: u8 = 0;
const SAMPLE_COPIED: u8 = 1;
const SAMPLE_MAPPING: u8 = 2;
const SAMPLE_READY: u8 = 3;

/// The sun shadow maps and their passes, while shadows are on.
pub(crate) struct ShadowPass {
    settings: SunShadowSettings,
    /// One `Depth32Float` layer per cascade, `resolution` square.
    maps: wgpu::Texture,
    /// The render attachment of each layer.
    layers: Vec<wgpu::TextureView>,
    /// [`MAX_CASCADES`] `ShadowCascadeGpu`s.
    uniform: wgpu::Buffer,
    /// The cull's draw counts and dropped count (cleared every frame).
    state: wgpu::Buffer,
    lists: Option<Lists>,
    cull: wgpu::ComputePipeline,
    cull_layout: wgpu::BindGroupLayout,
    /// The cull's group 0 and the chunk table it binds (rebuilt when the table or the lists
    /// change).
    cull_group: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    /// One cascade's entry of `uniform` (dynamic offset).
    cascade_group: wgpu::BindGroup,
    /// The receivers' group 3 ([`receive_layout`]), built again with the maps.
    receive_layout: wgpu::BindGroupLayout,
    receive_sampler: wgpu::Sampler,
    receive_group: wgpu::BindGroup,
    opaque: wgpu::RenderPipeline,
    cutout: wgpu::RenderPipeline,
    /// Texels per side of a block texture and its mip levels (the cutout's mip level).
    block_texels: u32,
    mip_levels: u32,
    /// The dropped count's read back.
    sample: wgpu::Buffer,
    sample_state: Arc<AtomicU8>,
    /// The last frame's cascades (`None`: nothing was drawn).
    last: Option<ShadowCascades>,
    stats: ShadowStats,
}

impl ShadowPass {
    /// Builds the pipelines, the uniform and the maps of `settings` (valid), and the receivers'
    /// group of `receive` ([`receive_layout`]). `block_texture` is the block texture array (the
    /// cutout's mip level comes from its size).
    pub fn new(
        device: &wgpu::Device,
        groups: &VoxelGroups<'_>,
        receive: &wgpu::BindGroupLayout,
        settings: &SunShadowSettings,
        block_texture: &wgpu::Texture,
    ) -> Self {
        let compute = wgpu::ShaderStages::COMPUTE;
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: compute,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let uniform = |binding, size, visibility, dynamic| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: wgpu::BufferSize::new(size),
            },
            count: None,
        };
        let cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel shadow cull"),
            entries: &[
                uniform(0, boxel_shaders::gpu::VIEW_BUFFER_SIZE, compute, false),
                storage(1, true),
                storage(2, false),
                storage(3, false),
                storage(4, false),
                uniform(5, UNIFORM_SIZE, compute, false),
            ],
        });
        let cull_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel shadow cull"),
            bind_group_layouts: &[Some(&cull_layout)],
            immediate_size: 0,
        });
        let cull_module = create_module(device, ShaderModuleId::ShadowCull);
        let cull = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("boxel shadow cull"),
            layout: Some(&cull_pipeline_layout),
            module: &cull_module,
            entry_point: Some("cull_shadow_casters"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let cascade_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel shadow cascade"),
            entries: &[uniform(
                0,
                CASCADE_STRIDE,
                wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                true,
            )],
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel shadow uniform"),
            size: UNIFORM_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cascade_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("boxel shadow cascade"),
            layout: &cascade_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(CASCADE_STRIDE),
                }),
            }],
        });
        let state = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel shadow cull state"),
            size: STATE_SIZE,
            // `COPY_SRC`: the dropped count is read back, and tests read the counts.
            usage: wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let sample = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel shadow dropped read"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel shadow"),
            bind_group_layouts: &[
                Some(groups.materials),
                Some(groups.view),
                Some(groups.geometry),
                Some(&cascade_layout),
            ],
            immediate_size: 0,
        });
        debug_assert_eq!(GROUP_CASCADE, 3);
        let module = create_module(device, ShaderModuleId::Shadow);
        let pipeline = |label, entry, fragment: Option<&str>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    // The DrawRef of each draw, instance-step, as `vs_pull`.
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<DrawRef>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Uint32x2,
                            offset: 0,
                            shader_location: 0,
                        }],
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    // The cull chose the faces; a back face seen from the sun is drawn.
                    cull_mode: None,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    // The nearest caster to the sun (depth 0 on its side).
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState {
                        constant: DEPTH_BIAS_CONSTANT,
                        slope_scale: DEPTH_BIAS_SLOPE,
                        clamp: 0.0,
                    },
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: fragment.map(|entry| wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque = pipeline("boxel shadow opaque", "vs_shadow", None);
        let cutout = pipeline(
            "boxel shadow cutout",
            "vs_shadow_cutout",
            Some("fs_shadow_cutout"),
        );
        let (maps, layers) = maps(device, settings);
        let receive_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("boxel sun shadow compare"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let receive_group =
            receive_group(device, receive, &maps, &receive_sampler, &uniform_buffer);
        Self {
            settings: *settings,
            maps,
            layers,
            receive_layout: receive.clone(),
            receive_sampler,
            receive_group,
            uniform: uniform_buffer,
            state,
            lists: None,
            cull,
            cull_layout,
            cull_group: None,
            cascade_group,
            opaque,
            cutout,
            block_texels: block_texture.width(),
            mip_levels: block_texture.mip_level_count(),
            sample,
            sample_state: Arc::new(AtomicU8::new(SAMPLE_IDLE)),
            last: None,
            stats: ShadowStats::default(),
        }
    }

    /// The settings in use.
    pub fn settings(&self) -> &SunShadowSettings {
        &self.settings
    }

    /// Takes `settings` (valid): new maps when their size or count changed.
    pub fn set_settings(&mut self, device: &wgpu::Device, settings: &SunShadowSettings) {
        let resized = settings.resolution != self.settings.resolution
            || settings.cascades != self.settings.cascades;
        self.settings = *settings;
        if resized {
            (self.maps, self.layers) = maps(device, settings);
            self.receive_group = receive_group(
                device,
                &self.receive_layout,
                &self.maps,
                &self.receive_sampler,
                &self.uniform,
            );
            self.lists = None;
            self.cull_group = None;
            self.last = None;
        }
    }

    /// Bytes of the shadow resources.
    pub fn bytes(&self) -> u64 {
        let texels = u64::from(self.settings.resolution).pow(2) * u64::from(self.settings.cascades);
        let lists = self
            .lists
            .as_ref()
            .map_or(0, |l| l.args.size() + l.refs.size());
        4 * texels + lists + self.state.size() + self.uniform.size() + self.sample.size()
    }

    /// The last frame's stats ([`ShadowStats::bytes`] included).
    pub fn stats(&self) -> ShadowStats {
        ShadowStats {
            bytes: self.bytes(),
            ..self.stats
        }
    }

    /// Testing: the last frame's cascades, `None` when it drew none.
    pub fn last_cascades(&self) -> Option<&ShadowCascades> {
        self.last.as_ref()
    }

    /// The receivers' group 3 when this frame drew its maps ([`ShadowPass::encode`]); `None`
    /// after [`ShadowPass::skip`], and then the main pass draws with today's pipelines.
    pub fn receivers(&self) -> Option<&wgpu::BindGroup> {
        self.last.is_some().then_some(&self.receive_group)
    }

    /// Testing: the maps (`Depth32Float`, one layer per cascade, `COPY_SRC`).
    pub fn maps(&self) -> &wgpu::Texture {
        &self.maps
    }

    /// Testing: the cull state (the draw counts of [`boxel_shaders::gpu::shadow::STATE_COUNTS`]
    /// then the dropped count).
    pub fn state(&self) -> &wgpu::Buffer {
        &self.state
    }

    /// A frame without shadows: nothing is recorded.
    pub fn skip(&mut self) {
        self.read_sample();
        self.last = None;
        self.stats = ShadowStats {
            dropped: self.stats.dropped,
            ..ShadowStats::default()
        };
    }

    /// Records the cull and the depth passes of `frame` (before the main pass).
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &ShadowFrame<'_>,
    ) {
        self.read_sample();
        // Set again once the maps are recorded: until then no receiver reads them.
        self.last = None;
        let cascades = frame.cascades.cascades();
        let count = (cascades.len() as u32).min(self.settings.cascades);
        let pages = frame.list_pages.clamp(1, MAX_PAGES);
        let casters = frame.page_chunks.clamp(1, PAGE_CASTERS);
        let shape = (self.settings.cascades, pages, casters);
        if self.lists.as_ref().is_none_or(|l| l.shape != shape) {
            self.lists = Some(lists(device, shape));
            self.cull_group = None;
        }
        let Some(lists) = &self.lists else {
            return;
        };
        if self
            .cull_group
            .as_ref()
            .is_none_or(|(table, _)| table != frame.table)
        {
            let group = cull_group(
                device,
                &self.cull_layout,
                [
                    frame.view,
                    frame.table,
                    &lists.args,
                    &lists.refs,
                    &self.state,
                    &self.uniform,
                ],
            );
            self.cull_group = Some((frame.table.clone(), group));
        }
        let Some((_, cull_group)) = &self.cull_group else {
            return;
        };
        // The uniform: every cascade, the frame's shared fields in each.
        let c = frame.cascades;
        let mut entries = [ShadowCascadeGpu::default(); MAX_CASCADES];
        for (entry, cascade) in entries.iter_mut().zip(cascades) {
            *entry = ShadowCascadeGpu {
                view_proj: cascade.view_proj.to_cols_array_2d(),
                planes: cascade.frustum.to_array(),
                params: [
                    cascade.texel,
                    self.cutout_mip(cascade.texel),
                    cascade.start,
                    cascade.end,
                ],
                light: c.light.extend(c.distance).to_array(),
                counts: [count, c.bucket_mask, casters, pages],
                band: [frame.band[0], frame.band[1], i32::from(frame.realm), 0],
                receive: [frame.strength, 0.0, 0.0, 0.0],
                pad: [0; 4],
            };
        }
        queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&entries));
        encoder.clear_buffer(&self.state, 0, None);
        if frame.slots > 0 {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("boxel shadow cull"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.cull);
            pass.set_bind_group(boxel_shaders::gpu::shadow::CULL_GROUP, cull_group, &[]);
            pass.dispatch_workgroups(frame.slots.div_ceil(CULL_WORKGROUP_SIZE), 1, 1);
        }
        if self.sample_state.load(Ordering::Acquire) == SAMPLE_IDLE {
            encoder.copy_buffer_to_buffer(&self.state, STATE_DROPPED_OFFSET, &self.sample, 0, 4);
            self.sample_state.store(SAMPLE_COPIED, Ordering::Release);
        }
        let live: Vec<(u64, &wgpu::BindGroup)> = frame
            .pages
            .holding_chunks()
            .filter(|(p, _)| u32::from(*p) < pages)
            .filter_map(|(p, page)| page.geometry.as_ref().map(|g| (u64::from(p), g)))
            .collect();
        let casters = u64::from(casters);
        let mut draws = 0;
        for (layer, view) in self.layers.iter().enumerate().take(count as usize) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("boxel sun shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if live.is_empty() {
                continue;
            }
            let c = layer as u64;
            pass.set_bind_group(bindings::GROUP_MATERIALS, frame.materials, &[]);
            pass.set_bind_group(bindings::GROUP_VIEW, frame.view_group, &[]);
            pass.set_bind_group(
                GROUP_CASCADE,
                &self.cascade_group,
                &[(CASCADE_STRIDE * c) as u32],
            );
            pass.set_vertex_buffer(0, lists.refs.slice(..));
            pass.set_index_buffer(frame.index.slice(..), wgpu::IndexFormat::Uint32);
            for (cutout, pipeline) in [(false, &self.opaque), (true, &self.cutout)] {
                pass.set_pipeline(pipeline);
                for &(p, geometry) in &live {
                    let base = (c * u64::from(pages) + p) * 4 * casters;
                    let (first, max, k) = if cutout {
                        (base + 3 * casters, casters, 2 * c + 1)
                    } else {
                        (base, 3 * casters, 2 * c)
                    };
                    pass.set_bind_group(bindings::GROUP_GEOMETRY, geometry, &[]);
                    pass.multi_draw_indexed_indirect_count(
                        &lists.args,
                        first * ARGS_BYTES,
                        &self.state,
                        4 * (k * u64::from(MAX_PAGES) + p),
                        max as u32,
                    );
                    draws += 1;
                }
            }
        }
        self.last = Some(*frame.cascades);
        self.stats = ShadowStats {
            cascades: count,
            draws,
            dropped: self.stats.dropped,
            bytes: 0,
        };
    }

    /// The mip level of the block textures whose texels are about the size of a shadow texel
    /// of `texel` blocks: the cutout alpha test reads it.
    fn cutout_mip(&self, texel: f32) -> f32 {
        let texels = texel * self.block_texels as f32;
        let last = self.mip_levels.saturating_sub(1) as f32;
        if texels > 1.0 {
            texels.log2().floor().min(last)
        } else {
            0.0
        }
    }

    /// Maps the dropped count copied last frame, or reads it once mapped.
    fn read_sample(&mut self) {
        match self.sample_state.load(Ordering::Acquire) {
            SAMPLE_COPIED => {
                self.sample_state.store(SAMPLE_MAPPING, Ordering::Release);
                let state = Arc::clone(&self.sample_state);
                self.sample
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| {
                        let next = if r.is_ok() { SAMPLE_READY } else { SAMPLE_IDLE };
                        state.store(next, Ordering::Release);
                    });
            }
            SAMPLE_READY => {
                if let Ok(mapped) = self.sample.slice(..).get_mapped_range() {
                    self.stats.dropped = bytemuck::pod_read_unaligned(&mapped[..4]);
                }
                self.sample.unmap();
                self.sample_state.store(SAMPLE_IDLE, Ordering::Release);
            }
            _ => {}
        }
    }
}

/// The maps of `settings` and a view of each layer.
fn maps(
    device: &wgpu::Device,
    settings: &SunShadowSettings,
) -> (wgpu::Texture, Vec<wgpu::TextureView>) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("boxel sun shadow maps"),
        size: wgpu::Extent3d {
            width: settings.resolution,
            height: settings.resolution,
            depth_or_array_layers: settings.cascades,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        // `TEXTURE_BINDING`: the receivers (lane S4); `COPY_SRC`: tests read them back.
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let layers = (0..settings.cascades)
        .map(|layer| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("boxel sun shadow layer"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                ..Default::default()
            })
        })
        .collect();
    (texture, layers)
}

/// The layout of the receivers' group 3 (`sun_shadow.wgsl`, `GROUP_RECEIVE`), for the fragment
/// stage: the maps as a depth 2D array, a comparison sampler and the whole shadow uniform. The
/// shadowed voxel pipelines are built with it.
pub(crate) fn receive_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let fragment = wgpu::ShaderStages::FRAGMENT;
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("boxel sun shadow receivers"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: RECEIVE_MAPS,
                visibility: fragment,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: RECEIVE_SAMPLER,
                visibility: fragment,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: RECEIVE_UNIFORM,
                visibility: fragment,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM_SIZE),
                },
                count: None,
            },
        ],
    })
}

/// The receivers' group of `maps` (every layer), `sampler` and `uniform`.
fn receive_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    maps: &wgpu::Texture,
    sampler: &wgpu::Sampler,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    let view = maps.create_view(&wgpu::TextureViewDescriptor {
        label: Some("boxel sun shadow maps"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        aspect: wgpu::TextureAspect::DepthOnly,
        ..Default::default()
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("boxel sun shadow receivers"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: RECEIVE_MAPS,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: RECEIVE_SAMPLER,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: RECEIVE_UNIFORM,
                resource: uniform.as_entire_binding(),
            },
        ],
    })
}

/// Lists of `shape` (cascades, pages, chunks of a page).
fn lists(device: &wgpu::Device, shape: (u32, u32, u32)) -> Lists {
    let (cascades, pages, casters) = shape;
    let draws = u64::from(cascades) * u64::from(pages) * 4 * u64::from(casters);
    let args = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel shadow draw args"),
        size: draws * ARGS_BYTES,
        usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let refs = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel shadow draw refs"),
        size: draws * size_of::<DrawRef>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    Lists { args, refs, shape }
}

/// The cull's group 0: view, table, args, refs, state, uniform.
fn cull_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffers: [&wgpu::Buffer; 6],
) -> wgpu::BindGroup {
    let entries: Vec<wgpu::BindGroupEntry<'_>> = buffers
        .iter()
        .enumerate()
        .map(|(binding, buffer)| wgpu::BindGroupEntry {
            binding: binding as u32,
            resource: buffer.as_entire_binding(),
        })
        .collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("boxel shadow cull"),
        layout,
        entries: &entries,
    })
}

/// Testing: the depths of layer `layer` of `maps` (`Depth32Float`, a width that is a multiple
/// of 64), row by row from the top (waits for the GPU).
pub(crate) fn read_depth_layer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    maps: &wgpu::Texture,
    layer: u32,
) -> Result<Vec<f32>, String> {
    if layer >= maps.depth_or_array_layers() {
        return Err(format!("no layer {layer}"));
    }
    let (width, height) = (maps.width(), maps.height());
    // A row of 4-byte texels, a multiple of 256 bytes from 64 texels on.
    let row = 4 * width;
    if row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT != 0 {
        return Err(format!("{width} texels a row cannot be copied"));
    }
    let bytes = u64::from(row) * u64::from(height);
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel shadow map read"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("boxel shadow map read"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: maps,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: wgpu::TextureAspect::DepthOnly,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &read,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
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
    let slice = read.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("poll: {e}"))?;
    rx.recv()
        .map_err(|e| format!("map: {e}"))?
        .map_err(|e| format!("map: {e}"))?;
    let mapped = slice
        .get_mapped_range()
        .map_err(|e| format!("mapped range: {e}"))?;
    let depths = bytemuck::pod_collect_to_vec::<u8, f32>(&mapped[..]);
    drop(mapped);
    read.unmap();
    Ok(depths)
}
