//! Tier paths (internal). Each implements the same internal trait, [`TierPath`]:
//! `prepare` (CPU culling / uploads), `encode_compute` (High cull), `draw` (opaque buckets with
//! the opaque pipeline, then the cutout bucket with the cutout pipeline, then the translucent
//! buckets back to front with the translucent pipeline).
//!
//! This module also owns what every tier shares: the frozen bind group layouts (group 0
//! materials, group 1 view, group 2 geometry on High/Mid, the cull group), the voxel pipeline
//! builder and the CPU culling used by Mid and Low (the same tests as `cull.wgsl`).
mod high;
mod low;
mod mid;

pub(crate) use high::{HighInputs, HighPath};
pub(crate) use low::LowPath;
pub(crate) use mid::MidPath;

use crate::chunk_table::ChunkTable;
use crate::renderer::ViewParams;
use crate::{DEPTH_FORMAT, MAIN_COLOR_FORMAT};
use boxel_gfx::Tier;
use boxel_math::{Aabb, IVec3, Vec3};
use boxel_mesh::{CUTOUT_BUCKET, MAX_QUADS_PER_BUCKET, TRANSLUCENT_BUCKET};
use boxel_shaders::gpu::{ViewUniform, bindings};
use boxel_shaders::{ShaderModuleId, create_module};

/// Edge of a render chunk in voxels (`CHUNK_SIZE` in `common.wgsl`).
pub(crate) const CHUNK_EDGE: f32 = 32.0;

/// What a tier does each frame.
pub(crate) trait TierPath {
    /// CPU work before encoding (culling, per-frame uploads). Returns the chunks that passed
    /// CPU culling (High culls on the GPU and returns 0).
    fn prepare(&mut self, queue: &wgpu::Queue, chunks: &ChunkTable, view: &ViewParams) -> u32;

    /// Compute work recorded before the main pass (High: draw count reset + GPU culling).
    fn encode_compute(&self, encoder: &mut wgpu::CommandEncoder, slot_count: u32) {
        let _ = (encoder, slot_count);
    }

    /// Records the draws into the main pass (groups 0 and 1 are already bound): the opaque
    /// buckets with the opaque pipeline, the cutout bucket with the cutout pipeline, then the
    /// translucent buckets ([`translucent_draws`], back to front) with the translucent pipeline.
    /// Returns the draw calls recorded.
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, arena: &wgpu::Buffer) -> u32;
}

/// Bind group layouts shared by the voxel pipelines of a tier.
pub(crate) struct VoxelLayouts {
    /// Group 0: block texture array + sampler (fragment).
    pub materials: wgpu::BindGroupLayout,
    /// Group 1: `View` uniform (vertex + fragment).
    pub view: wgpu::BindGroupLayout,
    /// Group 2 (High/Mid): quad arena + chunk table, read-only storage (vertex).
    pub geometry: Option<wgpu::BindGroupLayout>,
}

impl VoxelLayouts {
    /// Creates the layouts `tier` uses.
    pub fn new(device: &wgpu::Device, tier: Tier) -> Self {
        let materials = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel materials"),
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
        let view = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel view"),
            entries: &[uniform_entry(
                0,
                wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            )],
        });
        let geometry = (tier >= Tier::Mid).then(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("boxel geometry"),
                entries: &[
                    storage_entry(0, wgpu::ShaderStages::VERTEX, true),
                    storage_entry(1, wgpu::ShaderStages::VERTEX, true),
                ],
            })
        });
        Self {
            materials,
            view,
            geometry,
        }
    }
}

/// The three voxel pipelines of a tier (ADR 0012, ADR 0018).
pub(crate) struct VoxelPipelines {
    /// `fs_opaque`, back faces culled, face buckets 0..6.
    pub opaque: wgpu::RenderPipeline,
    /// `fs_cutout` (alpha test), no face culling, bucket 6.
    pub cutout: wgpu::RenderPipeline,
    /// `fs_translucent`: alpha blending, depth test without depth write, no face culling (a
    /// water surface or a glass pane shows from both sides), bucket 7.
    pub translucent: wgpu::RenderPipeline,
}

impl VoxelPipelines {
    /// Builds the opaque, cutout and translucent pipelines of `tier` (`VoxelPull` on High/Mid,
    /// `VoxelInstanced` on Low).
    pub fn new(device: &wgpu::Device, tier: Tier, layouts: &VoxelLayouts) -> Self {
        let (module_id, vertex_entry, topology, immediate_size) = match tier {
            Tier::High | Tier::Mid => (
                ShaderModuleId::VoxelPull,
                "vs_pull",
                wgpu::PrimitiveTopology::TriangleList,
                0,
            ),
            Tier::Low => (
                ShaderModuleId::VoxelInstanced,
                "vs_instanced",
                wgpu::PrimitiveTopology::TriangleStrip,
                bindings::LOW_IMMEDIATE_SIZE,
            ),
        };
        let module = create_module(device, module_id);
        let groups: Vec<Option<&wgpu::BindGroupLayout>> = [
            Some(&layouts.materials),
            Some(&layouts.view),
            layouts.geometry.as_ref(),
        ]
        .into_iter()
        .filter(Option::is_some)
        .collect();
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(module_id.label()),
            bind_group_layouts: &groups,
            immediate_size,
        });
        // High/Mid: DrawRef per draw; Low: the PackedQuad itself (slot 0) and its light palette
        // entry (slot 1, a buffer parallel to the arena, ADR 0015). All instance-step Uint32x2.
        let attribute = |shader_location| {
            [wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32x2,
                offset: 0,
                shader_location,
            }]
        };
        let (slot0, slot1) = (attribute(0), attribute(1));
        let instance_step = |attributes| wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes,
        };
        let buffers: Vec<Option<wgpu::VertexBufferLayout<'_>>> = match tier {
            Tier::High | Tier::Mid => vec![Some(instance_step(&slot0))],
            Tier::Low => vec![Some(instance_step(&slot0)), Some(instance_step(&slot1))],
        };
        let build = |label: &str,
                     fragment_entry: &str,
                     cull_mode: Option<wgpu::Face>,
                     blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vertex_entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    // Blended quads test depth but do not write it: the ones behind still show.
                    depth_write_enabled: Some(blend.is_none()),
                    depth_compare: Some(wgpu::CompareFunction::Greater),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fragment_entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: MAIN_COLOR_FORMAT,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            opaque: build(
                "boxel voxel opaque",
                "fs_opaque",
                Some(wgpu::Face::Back),
                None,
            ),
            cutout: build("boxel voxel cutout", "fs_cutout", None, None),
            translucent: build(
                "boxel voxel translucent",
                "fs_translucent",
                None,
                Some(wgpu::BlendState::ALPHA_BLENDING),
            ),
        }
    }
}

/// The High tier cull pipeline and its layout.
pub(crate) struct CullPipeline {
    /// Group 0 of `cull.wgsl`.
    pub layout: wgpu::BindGroupLayout,
    /// `cull_chunks`.
    pub pipeline: wgpu::ComputePipeline,
}

impl CullPipeline {
    /// Builds `cull_chunks` with its layout: view uniform (0), chunk table (1, read-only), draw
    /// args (2), draw refs (3) and the two draw counts (4).
    pub fn new(device: &wgpu::Device) -> Self {
        let compute = wgpu::ShaderStages::COMPUTE;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel cull"),
            entries: &[
                uniform_entry(0, compute),
                storage_entry(1, compute, true),
                storage_entry(2, compute, false),
                storage_entry(3, compute, false),
                storage_entry(4, compute, false),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel cull"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = create_module(device, ShaderModuleId::Cull);
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("boxel cull"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("cull_chunks"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self { layout, pipeline }
    }
}

/// Every pipeline a tier uses (the smoke test compiles exactly these).
pub(crate) struct TierPipelines {
    /// Voxel layouts.
    pub layouts: VoxelLayouts,
    /// Opaque + cutout.
    pub voxel: VoxelPipelines,
    /// High only.
    pub cull: Option<CullPipeline>,
}

impl TierPipelines {
    /// Builds the layouts and pipelines of `tier`.
    pub fn new(device: &wgpu::Device, tier: Tier) -> Self {
        let layouts = VoxelLayouts::new(device, tier);
        let voxel = VoxelPipelines::new(device, tier, &layouts);
        let cull = (tier == Tier::High).then(|| CullPipeline::new(device));
        Self {
            layouts,
            voxel,
            cull,
        }
    }
}

/// Shared quad index buffer of High/Mid: `[0, 1, 2, 2, 1, 3] + 4q` for
/// `q in 0..MAX_QUADS_PER_BUCKET` (`vertex_index >> 2` is the quad, `& 3` the corner).
pub(crate) fn quad_index_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    use wgpu::util::DeviceExt as _;
    let indices: Vec<u32> = (0..MAX_QUADS_PER_BUCKET)
        .flat_map(|q| [0, 1, 2, 2, 1, 3].map(|c| 4 * q + c))
        .collect();
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("boxel quad indices"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    })
}

/// Group 2 of the High/Mid voxel pipelines: quad arena (binding 0) + chunk table (binding 1).
pub(crate) fn geometry_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    arena: &wgpu::Buffer,
    chunks: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("boxel geometry"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: arena.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: chunks.as_entire_binding(),
            },
        ],
    })
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(size_of::<ViewUniform>() as u64),
        },
        count: None,
    }
}

fn storage_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// One visible bucket: draw `count` quads starting at arena quad `quad_start` of chunk `slot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BucketDraw {
    /// Chunk table slot.
    pub slot: u32,
    /// Chunk origin as the GPU gets it, the low 32 bits of the world origin (Low passes it as
    /// immediates).
    pub origin: IVec3,
    /// First quad in the arena.
    pub quad_start: u32,
    /// Quads (never 0, never above `MAX_QUADS_PER_BUCKET`).
    pub count: u32,
}

/// Result of the CPU culling (reused every frame).
#[derive(Debug, Default)]
pub(crate) struct CpuCull {
    /// Visible non-empty opaque face buckets, in slot then face order.
    pub opaque: Vec<BucketDraw>,
    /// Visible non-empty cutout buckets.
    pub cutout: Vec<BucketDraw>,
    /// Visible non-empty translucent buckets, back to front ([`translucent_draws`]).
    pub translucent: Vec<BucketDraw>,
    /// Chunks inside the frustum.
    pub visible_chunks: u32,
}

impl CpuCull {
    /// Culls every chunk inside the GPU window (the live ones of `cull.wgsl`, about `2^30` voxels
    /// around the camera, see `chunk_table`) with the tests of `cull_chunks`: chunk AABB
    /// `[origin - render_origin, + 32]` against the frustum (p-vertex test), then the per-face
    /// bucket test with the camera-relative eye; the cutout bucket only needs the frustum. The
    /// translucent buckets come from [`translucent_draws`].
    ///
    /// Chunks outside the window are skipped at any view distance: Mid and Low draw with the
    /// wrapped origins too, whose difference is exact only inside it.
    pub fn run(&mut self, chunks: &ChunkTable, view: &ViewParams) {
        self.opaque.clear();
        self.cutout.clear();
        self.visible_chunks = 0;
        translucent_draws(chunks, view, &mut self.translucent);
        for (slot, record) in chunks.iter() {
            if !chunks.in_window(record.origin) {
                continue;
            }
            // In i64 first, then to f32 (exact for every chunk near the camera). Within the world
            // limit (|v| <= 2^61) nothing saturates; a camera past i64 sees every chunk as far
            // (culled) instead of overflowing.
            let min = record.origin.saturating_sub(view.render_origin).as_vec3();
            let max = min + Vec3::splat(CHUNK_EDGE);
            if !view.frustum.intersects_aabb(&Aabb::new(min, max)) {
                continue;
            }
            self.visible_chunks += 1;
            let mut start = record.alloc.offset;
            for (face, &n) in record.counts[..6].iter().enumerate() {
                if n > 0 && bucket_visible(face, min, max, view.eye_rel) {
                    self.opaque.push(BucketDraw {
                        slot,
                        origin: record.gpu_origin(),
                        quad_start: start,
                        count: n.min(MAX_QUADS_PER_BUCKET),
                    });
                }
                start += n;
            }
            let n = record.counts[CUTOUT_BUCKET];
            if n > 0 {
                self.cutout.push(BucketDraw {
                    slot,
                    origin: record.gpu_origin(),
                    quad_start: start,
                    count: n.min(MAX_QUADS_PER_BUCKET),
                });
            }
        }
    }
}

/// The visible non-empty translucent buckets, back to front: every tier draws them from this
/// list, in this order, so blending gives the same pixels on all of them. Same window and
/// frustum tests as [`CpuCull::run`]; sorted by the squared distance from the eye to the chunk's
/// center, farthest first, ties by slot. Only the chunks are sorted: inside a bucket the quads
/// keep the mesher's order (face buckets, then greedy order), so two translucent surfaces of
/// one chunk can blend in the wrong order (ADR 0018).
pub(crate) fn translucent_draws(chunks: &ChunkTable, view: &ViewParams, out: &mut Vec<BucketDraw>) {
    let mut keyed: Vec<(f32, BucketDraw)> = Vec::new();
    for (slot, record) in chunks.iter_translucent() {
        if !chunks.in_window(record.origin) {
            continue;
        }
        let min = record.origin.saturating_sub(view.render_origin).as_vec3();
        let max = min + Vec3::splat(CHUNK_EDGE);
        if !view.frustum.intersects_aabb(&Aabb::new(min, max)) {
            continue;
        }
        let start = record.alloc.offset + record.counts[..TRANSLUCENT_BUCKET].iter().sum::<u32>();
        let center = min + Vec3::splat(CHUNK_EDGE / 2.0);
        keyed.push((
            (center - view.eye_rel).length_squared(),
            BucketDraw {
                slot,
                origin: record.gpu_origin(),
                quad_start: start,
                count: record.counts[TRANSLUCENT_BUCKET].min(MAX_QUADS_PER_BUCKET),
            },
        ));
    }
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.slot.cmp(&b.1.slot)));
    out.clear();
    out.extend(keyed.into_iter().map(|(_, d)| d));
}

/// Chunk-level backface test of `cull.wgsl` (`bucket_visible`): a +Y bucket can only be seen
/// from above the chunk's min y, and so on. `face` in `Face` order.
pub(crate) fn bucket_visible(face: usize, min: Vec3, max: Vec3, eye: Vec3) -> bool {
    match face {
        0 => eye.y > min.y,
        1 => eye.y < max.y,
        2 => eye.x > min.x,
        3 => eye.x < max.x,
        4 => eye.z > min.z,
        _ => eye.z < max.z,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::ArenaAllocator;
    use crate::chunk_table::ChunkRecord;
    use boxel_camera::{Camera, CameraView};
    use boxel_math::{DVec3, I64Vec3, wrap_to_i32};
    use boxel_transform::Transform;

    /// Camera at `eye` looking along `-Z` rotated by yaw/pitch (radians).
    fn camera_view(eye: DVec3, yaw: f32, pitch: f32) -> ViewParams {
        let camera = Camera::default();
        let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
        let cv = CameraView::compute(&camera, &transform, 16.0 / 9.0);
        ViewParams::from_camera(&cv, &camera, [0.0; 4], 0.6)
    }

    /// Chunks with one quad in every opaque face bucket, two cutout quads and one translucent.
    fn table_with(origins: &[I64Vec3]) -> ChunkTable {
        let mut arena = ArenaAllocator::new(1 << 16, 1024);
        let mut table = ChunkTable::new(64);
        for (i, origin) in origins.iter().enumerate() {
            let alloc = arena.allocate(10).expect("arena");
            table.insert(ChunkRecord {
                id: i as u64,
                origin: *origin,
                alloc,
                counts: [1, 1, 1, 1, 1, 1, 2, 1],
                light_base: alloc.offset + 9,
                light_len: 1,
            });
        }
        table
    }

    /// Visible opaque buckets of `slot` as offsets inside its allocation (= face index here).
    fn visible_faces(cull: &CpuCull, table: &ChunkTable, slot: u32) -> Vec<u32> {
        let (_, record) = table.iter().find(|(s, _)| *s == slot).expect("slot");
        cull.opaque
            .iter()
            .filter(|d| d.slot == slot)
            .map(|d| d.quad_start - record.alloc.offset)
            .collect()
    }

    #[test]
    fn bucket_rules_match_cull_wgsl() {
        // The WGSL source spells out the same six rules (keeps both sides in sync).
        for rule in [
            "case 0u: { return cam.y > mn.y; }",
            "case 1u: { return cam.y < mx.y; }",
            "case 2u: { return cam.x > mn.x; }",
            "case 3u: { return cam.x < mx.x; }",
            "case 4u: { return cam.z > mn.z; }",
            "default: { return cam.z < mx.z; }",
        ] {
            assert!(boxel_shaders::CULL_WGSL.contains(rule), "{rule}");
        }
        let (min, max) = (Vec3::ZERO, Vec3::splat(32.0));
        let above = Vec3::new(16.0, 40.0, 16.0);
        let inside = Vec3::splat(16.0);
        for (face, expected) in [true, false, true, true, true, true]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                bucket_visible(face, min, max, above),
                expected,
                "face {face}"
            );
            assert!(
                bucket_visible(face, min, max, inside),
                "inside sees every face"
            );
        }
        let corner = Vec3::new(-1.0, -1.0, -1.0);
        let visible: Vec<usize> = (0..6)
            .filter(|&f| bucket_visible(f, min, max, corner))
            .collect();
        assert_eq!(visible, [1, 3, 5]);
    }

    #[test]
    fn cpu_cull_drops_hidden_face_buckets_and_keeps_the_cutout_bucket() {
        // Camera above the chunk looking down: the -Y bucket faces away from it.
        let table = table_with(&[I64Vec3::new(0, 0, 0)]);
        let view = camera_view(DVec3::new(16.5, 80.5, 16.5), 0.0, -89f32.to_radians());
        let mut cull = CpuCull::default();
        cull.run(&table, &view);
        assert_eq!(cull.visible_chunks, 1);
        assert_eq!(visible_faces(&cull, &table, 0), [0, 2, 3, 4, 5]);
        assert_eq!(cull.cutout.len(), 1);
        let (_, record) = table.iter().next().expect("record");
        assert_eq!(cull.cutout[0].quad_start, record.alloc.offset + 6);
        assert_eq!(cull.cutout[0].count, 2);
        assert!(
            cull.opaque
                .iter()
                .all(|d| d.count == 1 && d.origin == IVec3::ZERO)
        );
    }

    #[test]
    fn cutout_bucket_only_needs_the_frustum() {
        // Camera inside the first chunk: every face bucket passes. The second chunk is behind the
        // camera and is culled entirely, cutout bucket included.
        let table = table_with(&[I64Vec3::new(0, 0, 0), I64Vec3::new(0, 0, 320)]);
        let view = camera_view(DVec3::new(16.5, 16.5, 16.5), 0.0, 0.0);
        let mut cull = CpuCull::default();
        cull.run(&table, &view);
        assert_eq!(cull.visible_chunks, 1);
        assert_eq!(visible_faces(&cull, &table, 0), [0, 1, 2, 3, 4, 5]);
        assert_eq!(cull.cutout.iter().map(|d| d.slot).collect::<Vec<_>>(), [0]);
        assert!(cull.opaque.iter().all(|d| d.slot == 0));
    }

    #[test]
    fn far_chunks_are_culled_by_the_view_distance() {
        // Camera::default().view_distance = 384: a chunk 1000 voxels ahead is past the far plane.
        let table = table_with(&[I64Vec3::new(0, 0, -64), I64Vec3::new(0, 0, -1000)]);
        let view = camera_view(DVec3::new(16.5, 16.5, 16.5), 0.0, 0.0);
        let mut cull = CpuCull::default();
        cull.run(&table, &view);
        assert_eq!(cull.visible_chunks, 1);
        assert!(cull.opaque.iter().chain(&cull.cutout).all(|d| d.slot == 0));
        // integer subtraction first: the same result far from the world origin
        let far = 1i64 << 24;
        let table = table_with(&[I64Vec3::new(far, 0, far - 64)]);
        let eye = DVec3::new(far as f64 + 16.5, 16.5, far as f64 + 16.5);
        cull.run(&table, &camera_view(eye, 0.0, 0.0));
        assert_eq!(cull.visible_chunks, 1);
        assert_eq!(visible_faces(&cull, &table, 0), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn cpu_cull_is_exact_across_the_former_i32_edge() {
        // Chunks around a camera in chunk (0, 0, 0): ahead, beside, behind and past the view
        // distance. Shifted so the camera and the chunks straddle +-2^31 (where i32 origins used
        // to overflow) and far beyond, the same buckets must pass in the same order.
        let layout = [
            I64Vec3::new(0, 0, 0),
            I64Vec3::new(0, 0, -64),
            I64Vec3::new(-32, 0, -32),
            I64Vec3::new(32, -32, -96),
            I64Vec3::new(0, 0, 320),
            I64Vec3::new(0, 0, -1024),
        ];
        let run = |shift: I64Vec3| {
            let origins: Vec<I64Vec3> = layout.iter().map(|o| *o + shift).collect();
            let mut table = table_with(&origins);
            let eye = shift.as_dvec3() + DVec3::new(16.5, 20.25, 16.5);
            let view = camera_view(eye, 0.3, -0.2);
            // As the renderer does every frame before culling.
            table.follow(view.render_origin);
            let mut cull = CpuCull::default();
            cull.run(&table, &view);
            let buckets = |list: &[BucketDraw]| -> Vec<(u32, u32, u32)> {
                list.iter()
                    .map(|d| (d.slot, d.quad_start, d.count))
                    .collect()
            };
            // Low draws with the wrapped origin of each chunk.
            for d in cull.opaque.iter().chain(&cull.cutout) {
                assert_eq!(d.origin, wrap_to_i32(origins[d.slot as usize]));
            }
            (
                cull.visible_chunks,
                buckets(&cull.opaque),
                buckets(&cull.cutout),
            )
        };
        let near = run(I64Vec3::ZERO);
        assert_eq!(
            near.0, 4,
            "the chunks behind and past the far plane are culled"
        );
        for shift in [
            I64Vec3::new((1 << 31) - 32, 0, -(1 << 31)),
            I64Vec3::new(-(1 << 31), 64, (1 << 31) - 64),
            I64Vec3::new(1 << 40, -(1 << 31), -(1 << 40)),
            // The eye is still exact in f64 here (steps of 1/4); far beyond, f64 rounds the
            // camera itself (the documented next limit), not the integer math.
            I64Vec3::new(1 << 50, 0, -(1 << 50)),
        ] {
            assert_eq!(run(shift), near, "shift {shift}");
        }
    }

    #[test]
    fn chunks_outside_the_gpu_window_are_culled_at_any_view_distance() {
        // Two chunks 2^32 voxels apart straight ahead of the camera: the old area after a
        // teleport, still resident. With an unlimited view distance both are in the frustum, but
        // their wrapped origins are the same, so drawing the far one (Mid and Low draw with
        // wrapped origins) would put it on top of the near one: the window keeps it out.
        let teleport = I64Vec3::new(0, 0, -(1 << 32));
        let ahead = I64Vec3::new(0, 0, -64);
        let mut table = table_with(&[ahead, ahead + teleport]);
        let camera = Camera {
            view_distance: f32::INFINITY,
            ..Camera::default()
        };
        let view_at = |eye: DVec3| {
            let transform = Transform::from_yaw_pitch(eye, 0.0, 0.0);
            let cv = CameraView::compute(&camera, &transform, 16.0 / 9.0);
            ViewParams::from_camera(&cv, &camera, [0.0; 4], 0.6)
        };
        let eye = DVec3::new(16.5, 16.5, 16.5);
        let mut cull = CpuCull::default();
        for (eye, visible) in [
            (eye, vec![0]),
            // The first chunk is behind the camera now.
            (eye + teleport.as_dvec3(), vec![1]),
            // Both ahead, both outside the window.
            (eye - teleport.as_dvec3(), vec![]),
            (eye, vec![0]),
        ] {
            let view = view_at(eye);
            // As the renderer does every frame before culling.
            table.follow(view.render_origin);
            for (_, record) in table.iter() {
                let min = record.origin.saturating_sub(view.render_origin).as_vec3();
                let aabb = Aabb::new(min, min + Vec3::splat(CHUNK_EDGE));
                let in_front = (record.origin - view.render_origin).z < 0;
                assert_eq!(view.frustum.intersects_aabb(&aabb), in_front, "{eye}");
            }
            cull.run(&table, &view);
            let mut slots: Vec<u32> = cull.opaque.iter().map(|d| d.slot).collect();
            slots.dedup();
            assert_eq!(slots, visible, "camera at {eye}");
            assert_eq!(
                cull.visible_chunks as usize,
                visible.len(),
                "camera at {eye}"
            );
        }
    }
}
