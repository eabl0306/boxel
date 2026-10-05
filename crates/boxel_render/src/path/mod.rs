//! Tier paths (internal). Each implements the same internal trait, [`TierPath`]:
//! `prepare` (CPU culling / uploads), `encode_compute` (High cull), `draw` (opaque buckets with
//! the opaque pipeline, then the cutout bucket with the cutout pipeline).
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
use boxel_mesh::{CUTOUT_BUCKET, MAX_QUADS_PER_BUCKET};
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
    /// buckets with the opaque pipeline, then the cutout bucket with the cutout pipeline.
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

/// The two voxel pipelines of a tier (ADR 0012).
pub(crate) struct VoxelPipelines {
    /// `fs_opaque`, back faces culled, face buckets 0..6.
    pub opaque: wgpu::RenderPipeline,
    /// `fs_cutout` (alpha test), no face culling, bucket 6.
    pub cutout: wgpu::RenderPipeline,
}

impl VoxelPipelines {
    /// Builds the opaque and cutout pipelines of `tier` (`VoxelPull` on High/Mid,
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
        // High/Mid: DrawRef per draw; Low: the PackedQuad itself. Both are instance-step Uint32x2.
        let attributes = [wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint32x2,
            offset: 0,
            shader_location: 0,
        }];
        let build = |label: &str, fragment_entry: &str, cull_mode: Option<wgpu::Face>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vertex_entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &attributes,
                    })],
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
                    depth_write_enabled: Some(true),
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
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            opaque: build("boxel voxel opaque", "fs_opaque", Some(wgpu::Face::Back)),
            cutout: build("boxel voxel cutout", "fs_cutout", None),
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
    /// Chunk world origin (Low passes it as immediates).
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
    /// Chunks inside the frustum.
    pub visible_chunks: u32,
}

impl CpuCull {
    /// Culls every live chunk exactly like `cull_chunks` in `cull.wgsl`: chunk AABB
    /// `[origin - render_origin, + 32]` against the frustum (p-vertex test), then the per-face
    /// bucket test with the camera-relative eye; the cutout bucket only needs the frustum.
    pub fn run(&mut self, chunks: &ChunkTable, view: &ViewParams) {
        self.opaque.clear();
        self.cutout.clear();
        self.visible_chunks = 0;
        for (slot, record) in chunks.iter() {
            let min = (record.origin - view.render_origin).as_vec3();
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
                        origin: record.origin,
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
                    origin: record.origin,
                    quad_start: start,
                    count: n.min(MAX_QUADS_PER_BUCKET),
                });
            }
        }
    }
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
    use boxel_math::DVec3;
    use boxel_transform::Transform;

    /// Camera at `eye` looking along `-Z` rotated by yaw/pitch (radians).
    fn camera_view(eye: DVec3, yaw: f32, pitch: f32) -> ViewParams {
        let camera = Camera::default();
        let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
        let cv = CameraView::compute(&camera, &transform, 16.0 / 9.0);
        ViewParams::from_camera(&cv, &camera, [0.0; 4], 0.6)
    }

    /// Chunks with one quad in every opaque face bucket, two cutout quads and one translucent.
    fn table_with(origins: &[IVec3]) -> ChunkTable {
        let mut arena = ArenaAllocator::new(1 << 16, 1024);
        let mut table = ChunkTable::new(64);
        for (i, origin) in origins.iter().enumerate() {
            table.insert(ChunkRecord {
                id: i as u64,
                origin: *origin,
                alloc: arena.allocate(9).expect("arena"),
                counts: [1, 1, 1, 1, 1, 1, 2, 1],
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
        let table = table_with(&[IVec3::new(0, 0, 0)]);
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
        let table = table_with(&[IVec3::new(0, 0, 0), IVec3::new(0, 0, 320)]);
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
        let table = table_with(&[IVec3::new(0, 0, -64), IVec3::new(0, 0, -1000)]);
        let view = camera_view(DVec3::new(16.5, 16.5, 16.5), 0.0, 0.0);
        let mut cull = CpuCull::default();
        cull.run(&table, &view);
        assert_eq!(cull.visible_chunks, 1);
        assert!(cull.opaque.iter().chain(&cull.cutout).all(|d| d.slot == 0));
        // integer subtraction first: the same result far from the world origin
        let far = 1 << 24;
        let table = table_with(&[IVec3::new(far, 0, far - 64)]);
        let eye = DVec3::new(f64::from(far) + 16.5, 16.5, f64::from(far) + 16.5);
        cull.run(&table, &camera_view(eye, 0.0, 0.0));
        assert_eq!(cull.visible_chunks, 1);
        assert_eq!(visible_faces(&cull, &table, 0), [0, 1, 2, 3, 4]);
    }
}
