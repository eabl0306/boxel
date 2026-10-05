use crate::{AdapterCaps, DeviceClass, Tier};

/// How deep a smoke test goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SmokeLevel {
    /// Resolved per adapter by [`SmokeLevel::resolve`]: `Draw` on GL backends and on
    /// Software-class adapters, `Compile` elsewhere. Default.
    #[default]
    Auto,
    /// Create every pipeline/bind group layout the tier uses (catches shader translation errors,
    /// e.g. GLSL 330 without SSBOs), record one pass, `finish` and submit it.
    Compile,
    /// `Compile` + draw one quad offscreen (16x16) and read back the center pixel. Catches silent
    /// miscompiles such as the NVIDIA GL 3.3 uniform-block collision (ADR 0011), which produce no
    /// error at all.
    Draw,
}

impl SmokeLevel {
    /// The level actually run on `caps` (never `Auto`): `Draw` on `Backend::Gl` or
    /// `DeviceClass::Software` (a 16x16 readback costs a few ms), else `Compile`.
    pub fn resolve(self, caps: &AdapterCaps) -> SmokeLevel {
        match self {
            SmokeLevel::Auto
                if caps.info.backend == wgpu::Backend::Gl
                    || caps.class() == DeviceClass::Software =>
            {
                SmokeLevel::Draw
            }
            SmokeLevel::Auto => SmokeLevel::Compile,
            other => other,
        }
    }
}

/// Inputs of a smoke test.
pub struct SmokeContext<'a> {
    /// Fresh device for the candidate.
    pub device: &'a wgpu::Device,
    /// Its queue.
    pub queue: &'a wgpu::Queue,
    /// Tier being proven.
    pub tier: Tier,
    /// Adapter capabilities.
    pub caps: &'a AdapterCaps,
    /// Depth (already resolved: never `Auto`).
    pub level: SmokeLevel,
    /// Color format the device will present to: the chosen surface format when selecting with a
    /// surface ([`crate::choose_surface_format`]), else `Rgba8UnormSrgb`. The test must build
    /// its output pipeline (the renderer's blit) for exactly this format.
    pub output_format: wgpu::TextureFormat,
}

/// Proves a tier works on a device. `select_gpu` runs it inside [`run_in_error_scopes`].
/// The renderer implements it with its *real* pipelines (`boxel_render::RendererSmokeTest`);
/// tools without a renderer use [`BasicSmokeTest`].
///
/// Contract: at every level the test records at least one pass and calls [`submit_and_wait`]
/// inside the scopes (wgpu 30 reports pass validation errors at `CommandEncoder::finish`).
/// Everything runs on the calling thread: wgpu error scopes are thread-local
/// (`ErrorScopeGuard` is `!Send`).
pub trait SmokeTest: Send + Sync {
    /// Returns `Err(message)` on any failure the test detects itself.
    fn run(&self, ctx: &SmokeContext<'_>) -> Result<(), String>;
}

/// Self-contained smoke test with its own tiny WGSL modules exercising the same capability
/// classes as the renderer: Low = instance-step `Uint32x2` attribute + immediates + one uniform
/// block + `texture_2d_array`; Mid = Low + two read-only storage buffers in the vertex stage +
/// indexed draw; High = Mid + compute with `atomicAdd` into a STORAGE|INDIRECT buffer + a render
/// pass that records `multi_draw_indexed_indirect_count`. Output pipeline in
/// `ctx.output_format`.
#[derive(Clone, Copy, Debug, Default)]
pub struct BasicSmokeTest;

///
/// Every level draws one quad covering a 4x4 `Rgba8UnormSrgb` target with the tier pipeline,
/// then blits it into a 4x4 `ctx.output_format` texture (`fs_blit` for sRGB formats,
/// `fs_blit_encode_srgb` otherwise, like the renderer's blit). `Draw` reads both back and
/// checks that the center pixel is the uniform color (within 2 per channel).
impl SmokeTest for BasicSmokeTest {
    fn run(&self, ctx: &SmokeContext<'_>) -> Result<(), String> {
        let level = ctx.level.resolve(ctx.caps);
        let mut targets = None;
        // Own scopes so a pipeline error is reported as the cause, not as a wrong pixel.
        run_in_error_scopes(ctx.device, || {
            targets = Some(basic::record_and_submit(ctx)?);
            Ok(())
        })?;
        let Some(targets) = targets else {
            return Err("smoke test produced no targets".to_string());
        };
        if level == SmokeLevel::Draw {
            basic::check_pixels(ctx, &targets)?;
        }
        Ok(())
    }
}

/// Finishes `encoder`, submits it and blocks until the GPU is idle. Smoke tests call it inside
/// [`run_in_error_scopes`].
pub fn submit_and_wait(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: wgpu::CommandEncoder,
) -> Result<(), String> {
    queue.submit([encoder.finish()]);
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map(|_| ())
        .map_err(|e| format!("poll: {e}"))
}

/// Runs `f` with Validation, Internal and `OutOfMemory` error scopes pushed (popped in reverse
/// order, blocking with pollster). Returns the first error message from `f` or a scope.
/// (Internal is mandatory: shader translation failures on GL are Internal errors.)
///
/// Thread rule: wgpu 30 error scopes are thread-local and the guards are `!Send`, so `f` and
/// all GPU bring-up it covers (selection, smoke tests, `VoxelRenderer::new`) run on the calling
/// thread. Keep this in mind for async GPU init (M3).
pub fn run_in_error_scopes(
    device: &wgpu::Device,
    f: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let result = f();
    let oom = pollster::block_on(oom.pop());
    let internal = pollster::block_on(internal.pop());
    let validation = pollster::block_on(validation.pop());
    let one_line = |e: wgpu::Error| e.to_string().replace('\n', " ");
    result?;
    if let Some(e) = validation {
        return Err(format!("validation: {}", one_line(e)));
    }
    if let Some(e) = internal {
        return Err(format!("internal: {}", one_line(e)));
    }
    if let Some(e) = oom {
        return Err(format!("out of memory: {}", one_line(e)));
    }
    Ok(())
}

/// WGSL and GPU code of [`BasicSmokeTest`].
pub(crate) mod basic {
    use super::{SmokeContext, submit_and_wait};
    use crate::{Tier, read_texture_rgba8};
    use wgpu::util::DeviceExt;

    /// Bindings and fragment stage shared by every tier (one uniform block, one texture array +
    /// sampler, immediates: the Low-tier resource rules of ADR 0011).
    const SHARED_WGSL: &str = r"
@group(0) @binding(0) var<uniform> u: vec4<f32>;
@group(0) @binding(1) var t: texture_2d_array<f32>;
@group(0) @binding(2) var s: sampler;
var<immediate> imm: vec4<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@fragment
fn fs_main(i: VsOut) -> @location(0) vec4<f32> {
    return u * textureSample(t, s, i.uv, 0).a + imm * 0.0;
}
";

    /// Low: instance-step `Uint32x2` attribute, triangle strip, no storage buffers.
    const LOW_VS_WGSL: &str = r"
@vertex
fn vs_main(@builtin(vertex_index) vi: u32, @location(0) q: vec2<u32>) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
    var o: VsOut;
    o.pos = vec4<f32>((corner * 2.0 - 1.0) * f32(q.x), 0.0, 1.0);
    o.uv = corner;
    return o;
}
";

    /// Mid: corners and scale/offset pulled from two read-only storage buffers in the vertex
    /// stage (indexed draw); the instance attribute selects the records.
    const MID_VS_WGSL: &str = r"
@group(1) @binding(0) var<storage, read> a: array<vec2<u32>>;
@group(1) @binding(1) var<storage, read> b: array<vec4<i32>>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, @location(0) q: vec2<u32>) -> VsOut {
    let corner = vec2<f32>(a[q.y + vi]);
    let k = b[q.y];
    var o: VsOut;
    o.pos = vec4<f32>((corner * vec2<f32>(k.xy) + vec2<f32>(k.zw)) * f32(q.x), 0.0, 1.0);
    o.uv = corner;
    return o;
}
";

    /// High: GPU-written indirect arguments (`atomicAdd` slot + one `DrawArgs`).
    const HIGH_CS_WGSL: &str = r"
struct DrawArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
};

@group(0) @binding(3) var<storage, read_write> count: atomic<u32>;
@group(0) @binding(4) var<storage, read_write> args: array<DrawArgs>;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x == 0u {
        let slot = atomicAdd(&count, 1u);
        args[slot] = DrawArgs(6u, 1u, 0u, 0, 0u);
    }
}
";

    /// Output pipeline: fullscreen blit with the renderer's two fragment entry points.
    pub(crate) const BLIT_WGSL: &str = r"
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;

struct BlitOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) vi: u32) -> BlitOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: BlitOut;
    o.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs_blit(i: BlitOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(src_tex, src_sampler, i.uv).rgb, 1.0);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

@fragment
fn fs_blit_encode_srgb(i: BlitOut) -> @location(0) vec4<f32> {
    let c = textureSample(src_tex, src_sampler, i.uv);
    return vec4<f32>(linear_to_srgb(c.rgb), 1.0);
}
";

    /// Color the tier pass must produce (linear RGBA).
    pub(crate) const COLOR: [f32; 4] = [0.2, 0.5, 0.8, 1.0];
    /// Side of the offscreen targets.
    const SIZE: u32 = 4;
    /// Format of the tier pass target (the renderer's main color target).
    const MAIN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    /// Full WGSL module of `tier`.
    pub(crate) fn tier_wgsl(tier: Tier) -> String {
        match tier {
            Tier::Low => format!("{SHARED_WGSL}{LOW_VS_WGSL}"),
            Tier::Mid => format!("{SHARED_WGSL}{MID_VS_WGSL}"),
            Tier::High => format!("{SHARED_WGSL}{MID_VS_WGSL}{HIGH_CS_WGSL}"),
        }
    }

    /// Rendered targets (kept alive for the readback).
    pub(crate) struct Targets {
        main: wgpu::Texture,
        output: wgpu::Texture,
    }

    /// Creates every pipeline of the tier, records the tier pass (plus the High compute pass)
    /// and the blit pass, and waits for the GPU.
    pub(crate) fn record_and_submit(ctx: &SmokeContext<'_>) -> Result<Targets, String> {
        let device = ctx.device;
        let main = target(device, "boxel smoke main", MAIN_FORMAT, true);
        let output = target(device, "boxel smoke output", ctx.output_format, false);
        let main_view = main.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("boxel smoke"),
        });
        let tier = TierPass::new(ctx);
        tier.record(&mut encoder, &main_view);
        let blit = BlitPass::new(device, &main_view, ctx.output_format);
        blit.record(&mut encoder, &output_view);
        submit_and_wait(device, ctx.queue, encoder)?;
        Ok(Targets { main, output })
    }

    /// Reads both targets back and compares their center pixel with [`COLOR`].
    pub(crate) fn check_pixels(ctx: &SmokeContext<'_>, targets: &Targets) -> Result<(), String> {
        let expected = expected_rgb();
        let main = read_texture_rgba8(ctx.device, ctx.queue, &targets.main)?;
        check_center("tier pass", &main, expected)?;
        if readable(ctx.output_format) {
            let output = read_texture_rgba8(ctx.device, ctx.queue, &targets.output)?;
            check_center("blit", &output, expected)?;
        }
        Ok(())
    }

    /// sRGB-encoded bytes of [`COLOR`] as stored by an `*Srgb` target.
    pub(crate) fn expected_rgb() -> [u8; 3] {
        let encode = |c: f32| {
            let s = if c <= 0.003_130_8 {
                c * 12.92
            } else {
                1.055 * c.powf(1.0 / 2.4) - 0.055
            };
            // Clamped to [0, 255] before the cast.
            (s.clamp(0.0, 1.0) * 255.0).round() as u8
        };
        [encode(COLOR[0]), encode(COLOR[1]), encode(COLOR[2])]
    }

    fn readable(format: wgpu::TextureFormat) -> bool {
        use wgpu::TextureFormat as F;
        matches!(
            format,
            F::Rgba8Unorm | F::Rgba8UnormSrgb | F::Bgra8Unorm | F::Bgra8UnormSrgb
        )
    }

    fn check_center(what: &str, image: &crate::RgbaImage, expected: [u8; 3]) -> Result<(), String> {
        let (x, y) = (image.width / 2, image.height / 2);
        let i = ((y * image.width + x) * 4) as usize;
        let Some(px) = image.data.get(i..i + 4) else {
            return Err(format!("{what}: readback too small"));
        };
        let close = (0..3).all(|c| px[c].abs_diff(expected[c]) <= 2);
        if close {
            Ok(())
        } else {
            Err(format!(
                "{what}: center pixel {:?} != expected {:?} (within 2)",
                &px[..3],
                expected
            ))
        }
    }

    fn target(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
        sampled: bool,
    ) -> wgpu::Texture {
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        if sampled {
            usage |= wgpu::TextureUsages::TEXTURE_BINDING;
        }
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    }

    fn bytes_f32(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    fn bytes_u32(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    fn bytes_i32(values: &[i32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    fn buffer(
        device: &wgpu::Device,
        label: &str,
        contents: &[u8],
        usage: wgpu::BufferUsages,
    ) -> wgpu::Buffer {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents,
            usage,
        })
    }

    fn layout_entry(
        binding: u32,
        visibility: wgpu::ShaderStages,
        ty: wgpu::BindingType,
    ) -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty,
            count: None,
        }
    }

    fn storage(read_only: bool) -> wgpu::BindingType {
        wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        }
    }

    /// Everything the tier pass binds.
    struct TierPass {
        tier: Tier,
        pipeline: wgpu::RenderPipeline,
        material: wgpu::BindGroup,
        storage: Option<wgpu::BindGroup>,
        instances: wgpu::Buffer,
        indices: Option<wgpu::Buffer>,
        compute: Option<ComputePart>,
    }

    /// High only: the culling stand-in and its indirect buffers.
    struct ComputePart {
        pipeline: wgpu::ComputePipeline,
        bind_group: wgpu::BindGroup,
        args: wgpu::Buffer,
        count: wgpu::Buffer,
    }

    impl TierPass {
        fn new(ctx: &SmokeContext<'_>) -> TierPass {
            let device = ctx.device;
            let tier = ctx.tier;
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("boxel smoke tier"),
                source: wgpu::ShaderSource::Wgsl(tier_wgsl(tier).into()),
            });

            // Group 0: uniform + texture array + sampler (all tiers).
            let material_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("boxel smoke material"),
                    entries: &[
                        layout_entry(
                            0,
                            wgpu::ShaderStages::VERTEX_FRAGMENT,
                            wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: wgpu::BufferSize::new(16),
                            },
                        ),
                        layout_entry(
                            1,
                            wgpu::ShaderStages::FRAGMENT,
                            wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2Array,
                                multisampled: false,
                            },
                        ),
                        layout_entry(
                            2,
                            wgpu::ShaderStages::FRAGMENT,
                            wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        ),
                    ],
                });
            let uniform = buffer(
                device,
                "boxel smoke uniform",
                &bytes_f32(&COLOR),
                wgpu::BufferUsages::UNIFORM,
            );
            // Two opaque white layers (a 1-layer texture would be a plain 2D texture on GL).
            let texture = device.create_texture_with_data(
                ctx.queue,
                &wgpu::TextureDescriptor {
                    label: Some("boxel smoke texture"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 2,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &[255; 8],
            );
            let texture_view = texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
            let material = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("boxel smoke material"),
                layout: &material_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });

            // Group 1 (Mid/High): corners and scale/offset in read-only storage buffers.
            let storage_layout = (tier >= Tier::Mid).then(|| {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("boxel smoke storage"),
                    entries: &[
                        layout_entry(0, wgpu::ShaderStages::VERTEX, storage(true)),
                        layout_entry(1, wgpu::ShaderStages::VERTEX, storage(true)),
                    ],
                })
            });
            let storage_group = storage_layout.as_ref().map(|layout| {
                let corners = buffer(
                    device,
                    "boxel smoke corners",
                    &bytes_u32(&[0, 0, 1, 0, 0, 1, 1, 1]),
                    wgpu::BufferUsages::STORAGE,
                );
                let transform = buffer(
                    device,
                    "boxel smoke transform",
                    &bytes_i32(&[2, 2, -1, -1]),
                    wgpu::BufferUsages::STORAGE,
                );
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("boxel smoke storage"),
                    layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: corners.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: transform.as_entire_binding(),
                        },
                    ],
                })
            });

            let mut layouts = vec![Some(&material_layout)];
            if let Some(layout) = &storage_layout {
                layouts.push(Some(layout));
            }
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("boxel smoke tier"),
                bind_group_layouts: &layouts,
                immediate_size: 16,
            });
            let attributes = [wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32x2,
                offset: 0,
                shader_location: 0,
            }];
            let topology = if tier == Tier::Low {
                wgpu::PrimitiveTopology::TriangleStrip
            } else {
                wgpu::PrimitiveTopology::TriangleList
            };
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("boxel smoke tier"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &attributes,
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(MAIN_FORMAT.into())],
                }),
                multiview_mask: None,
                cache: None,
            });

            // DrawRef-like instance record: x = scale (1), y = first record (0).
            let instances = buffer(
                device,
                "boxel smoke instances",
                &bytes_u32(&[1, 0]),
                wgpu::BufferUsages::VERTEX,
            );
            let indices = (tier >= Tier::Mid).then(|| {
                buffer(
                    device,
                    "boxel smoke indices",
                    &bytes_u32(&[0, 1, 2, 2, 1, 3]),
                    wgpu::BufferUsages::INDEX,
                )
            });
            let compute = (tier == Tier::High).then(|| ComputePart::new(device, &module));
            TierPass {
                tier,
                pipeline,
                material,
                storage: storage_group,
                instances,
                indices,
                compute,
            }
        }

        fn record(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
            if let Some(compute) = &self.compute {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("boxel smoke cull"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&compute.pipeline);
                pass.set_bind_group(0, &compute.bind_group, &[]);
                pass.dispatch_workgroups(1, 1, 1);
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("boxel smoke tier"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
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
            pass.set_bind_group(0, &self.material, &[]);
            if let Some(storage) = &self.storage {
                pass.set_bind_group(1, storage, &[]);
            }
            pass.set_immediates(0, &[0; 16]);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            if let Some(indices) = &self.indices {
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            }
            match (self.tier, &self.compute) {
                (Tier::High, Some(compute)) => {
                    pass.multi_draw_indexed_indirect_count(&compute.args, 0, &compute.count, 0, 1);
                }
                (Tier::Low, _) => pass.draw(0..4, 0..1),
                _ => pass.draw_indexed(0..6, 0, 0..1),
            }
        }
    }

    impl ComputePart {
        fn new(device: &wgpu::Device, module: &wgpu::ShaderModule) -> ComputePart {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("boxel smoke cull"),
                entries: &[
                    layout_entry(3, wgpu::ShaderStages::COMPUTE, storage(false)),
                    layout_entry(4, wgpu::ShaderStages::COMPUTE, storage(false)),
                ],
            });
            let indirect = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT;
            // Zero-initialized: count 0, no arguments until the compute pass writes them.
            let count = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel smoke draw count"),
                size: 4,
                usage: indirect,
                mapped_at_creation: false,
            });
            let args = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel smoke draw args"),
                size: 20,
                usage: indirect,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("boxel smoke cull"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: count.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: args.as_entire_binding(),
                    },
                ],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("boxel smoke cull"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("boxel smoke cull"),
                layout: Some(&pipeline_layout),
                module,
                entry_point: Some("cs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
            ComputePart {
                pipeline,
                bind_group,
                args,
                count,
            }
        }
    }

    /// The output (blit) pipeline for `format` and its bind group.
    struct BlitPass {
        pipeline: wgpu::RenderPipeline,
        bind_group: wgpu::BindGroup,
    }

    impl BlitPass {
        fn new(
            device: &wgpu::Device,
            source: &wgpu::TextureView,
            format: wgpu::TextureFormat,
        ) -> BlitPass {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("boxel smoke blit"),
                source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
            });
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("boxel smoke blit"),
                entries: &[
                    layout_entry(
                        0,
                        wgpu::ShaderStages::FRAGMENT,
                        wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                    ),
                    layout_entry(
                        1,
                        wgpu::ShaderStages::FRAGMENT,
                        wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    ),
                ],
            });
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("boxel smoke blit"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("boxel smoke blit"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let fragment = if format.is_srgb() {
                "fs_blit"
            } else {
                "fs_blit_encode_srgb"
            };
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("boxel smoke blit"),
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
                    entry_point: Some(fragment),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(format.into())],
                }),
                multiview_mask: None,
                cache: None,
            });
            BlitPass {
                pipeline,
                bind_group,
            }
        }

        fn record(&self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("boxel smoke blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
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
}

#[cfg(test)]
mod tests {
    use super::basic::{BLIT_WGSL, COLOR, expected_rgb, tier_wgsl};
    use crate::Tier;
    use wgpu::naga;
    use wgpu::naga::valid::{Capabilities, ModuleInfo, ValidationFlags, Validator};

    fn load(label: &str, src: &str) -> (naga::Module, ModuleInfo) {
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("{label}: {}", e.emit_to_string(src)));
        let info = Validator::new(ValidationFlags::all(), Capabilities::IMMEDIATES)
            .validate(&module)
            .unwrap_or_else(|e| panic!("{label}: {}", e.emit_to_string(src)));
        (module, info)
    }

    fn entry_points(module: &naga::Module) -> Vec<(&str, naga::ShaderStage)> {
        module
            .entry_points
            .iter()
            .map(|e| (e.name.as_str(), e.stage))
            .collect()
    }

    #[test]
    fn basic_smoke_wgsl_validates_with_naga() {
        use naga::ShaderStage::{Compute, Fragment, Vertex};
        for tier in Tier::DESCENDING {
            let (module, _) = load(&format!("{tier}"), &tier_wgsl(tier));
            let mut expected = vec![("fs_main", Fragment), ("vs_main", Vertex)];
            if tier == Tier::High {
                expected.push(("cs_main", Compute));
            }
            let mut got = entry_points(&module);
            got.sort_unstable_by_key(|(name, _)| *name);
            expected.sort_unstable_by_key(|(name, _)| *name);
            assert_eq!(got, expected, "{tier}");
            let storage = module
                .global_variables
                .iter()
                .any(|(_, g)| matches!(g.space, naga::AddressSpace::Storage { .. }));
            assert_eq!(storage, tier >= Tier::Mid, "{tier}: storage buffers");
        }
        let (blit, _) = load("blit", BLIT_WGSL);
        let mut got = entry_points(&blit);
        got.sort_unstable_by_key(|(name, _)| *name);
        assert_eq!(
            got,
            vec![
                ("fs_blit", Fragment),
                ("fs_blit_encode_srgb", Fragment),
                ("vs_fullscreen", Vertex)
            ]
        );
    }

    /// GL translation: Low and the blit must work in GLSL 330 / ES 300 (NVIDIA's 3.3 context),
    /// Mid needs GLSL 430 / ES 310 and is rejected by 330 (why `classify` checks the version).
    #[cfg(any(
        windows,
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd"
    ))]
    #[test]
    fn basic_smoke_wgsl_translates_to_glsl() {
        use naga::back::glsl;
        fn write(
            module: &naga::Module,
            info: &ModuleInfo,
            version: glsl::Version,
            entry: &str,
            stage: naga::ShaderStage,
        ) -> Result<String, String> {
            let mut out = String::new();
            let options = glsl::Options {
                version,
                writer_flags: glsl::WriterFlags::ADJUST_COORDINATE_SPACE,
                ..Default::default()
            };
            let pipeline = glsl::PipelineOptions {
                shader_stage: stage,
                entry_point: entry.to_string(),
                multiview: None,
            };
            glsl::Writer::new(
                &mut out,
                module,
                info,
                &options,
                &pipeline,
                naga::proc::BoundsCheckPolicies::default(),
            )
            .and_then(|mut w| w.write())
            .map(|_| out)
            .map_err(|e| e.to_string())
        }
        let gl33 = glsl::Version::Desktop(330);
        let es30 = glsl::Version::Embedded {
            version: 300,
            is_webgl: false,
        };
        let gl43 = glsl::Version::Desktop(430);
        let es31 = glsl::Version::Embedded {
            version: 310,
            is_webgl: false,
        };
        let stages = [
            ("vs_main", naga::ShaderStage::Vertex),
            ("fs_main", naga::ShaderStage::Fragment),
        ];
        let (low, low_info) = load("low", &tier_wgsl(Tier::Low));
        let (mid, mid_info) = load("mid", &tier_wgsl(Tier::Mid));
        for (entry, stage) in stages {
            for version in [gl33, es30] {
                write(&low, &low_info, version, entry, stage)
                    .unwrap_or_else(|e| panic!("Low {entry} {version:?}: {e}"));
            }
            for version in [gl43, es31] {
                write(&mid, &mid_info, version, entry, stage)
                    .unwrap_or_else(|e| panic!("Mid {entry} {version:?}: {e}"));
            }
        }
        assert!(
            write(&mid, &mid_info, gl33, "vs_main", naga::ShaderStage::Vertex).is_err(),
            "GLSL 330 has no SSBOs"
        );
        let (blit, blit_info) = load("blit", BLIT_WGSL);
        for (entry, stage) in [
            ("vs_fullscreen", naga::ShaderStage::Vertex),
            ("fs_blit", naga::ShaderStage::Fragment),
            ("fs_blit_encode_srgb", naga::ShaderStage::Fragment),
        ] {
            for version in [gl33, es30] {
                write(&blit, &blit_info, version, entry, stage)
                    .unwrap_or_else(|e| panic!("blit {entry} {version:?}: {e}"));
            }
        }
    }

    #[test]
    fn expected_color_is_srgb_encoded() {
        // 0.2 -> 124, 0.5 -> 188, 0.8 -> 231 in sRGB.
        assert_eq!(COLOR, [0.2, 0.5, 0.8, 1.0]);
        assert_eq!(expected_rgb(), [124, 188, 231]);
    }
}
