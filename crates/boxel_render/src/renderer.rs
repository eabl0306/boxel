use crate::arena::QuadArena;
use crate::chunk_table::{ChunkRecord, ChunkTable};
use crate::path::{
    HighInputs, HighPath, LowPath, MidPath, TierPath, TierPipelines, geometry_bind_group,
    quad_index_buffer,
};
use crate::targets::{Blit, Targets};
use crate::textures::{BLOCK_TEXTURE_SIZE, generate_block_textures, layer_bytes};
use boxel_camera::{Camera, CameraView};
use boxel_gfx::Tier;
use boxel_math::{Frustum, IVec3, Mat4, UVec2, Vec3};
use boxel_mesh::{BUCKET_COUNT, ChunkMesh};
use boxel_shaders::gpu::{ChunkGpu, ViewUniform, bindings};
use boxel_voxel::TextureSpec;
use wgpu::util::DeviceExt as _;

/// Upper bound of chunk slots (keeps the allocator node table and the GPU table bounded).
const MAX_SLOTS: u32 = 1 << 20;
/// Bytes of one chunk table entry.
const CHUNK_GPU_BYTES: u64 = size_of::<ChunkGpu>() as u64;

/// Renderer construction parameters.
#[derive(Clone, Debug)]
pub struct RendererConfig {
    /// Quad arena capacity (clamped to `max_storage_buffer_binding_size / 8` on High/Mid and
    /// `max_buffer_size / 8` on Low).
    pub arena_quads: u32,
    /// Chunk slots. High clamps it to `64 * max_compute_workgroups_per_dimension`; its draw
    /// lists hold `min(6 * slots, 65535)` opaque and `min(slots, 65535)` cutout draws. When more
    /// buckets are visible, High skips arbitrary ones (not the farthest; not counted in
    /// [`RenderStats`]) and logs one warning; Mid and Low draw them all.
    pub max_chunk_slots: u32,
    /// Clear color (linear).
    pub clear_color: [f32; 4],
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            arena_quads: 8 << 20,
            max_chunk_slots: 32768,
            clear_color: [0.30, 0.52, 0.85, 1.0],
        }
    }
}

/// Per-frame camera input of the renderer (camera-relative).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewParams {
    /// Camera-relative view-projection (reverse-Z).
    pub view_proj: Mat4,
    /// Camera-relative frustum (far plane at view distance).
    pub frustum: Frustum,
    /// Integer origin.
    pub render_origin: IVec3,
    /// Eye relative to `render_origin`.
    pub eye_rel: Vec3,
    /// Fog color (linear RGBA).
    pub fog_color: [f32; 4],
    /// Fog start distance.
    pub fog_start: f32,
    /// Fog end distance.
    pub fog_end: f32,
}

impl ViewParams {
    /// From a camera view: fog from `camera.view_distance * fog_start_fraction` to the distance.
    pub fn from_camera(
        view: &CameraView,
        camera: &Camera,
        fog_color: [f32; 4],
        fog_start_fraction: f32,
    ) -> Self {
        Self {
            view_proj: view.view_proj,
            frustum: view.frustum,
            render_origin: view.render_origin,
            eye_rel: view.eye_rel,
            fog_color,
            fog_start: camera.view_distance * fog_start_fraction,
            fog_end: camera.view_distance,
        }
    }
}

/// Counters of the last rendered frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Resident chunks.
    pub chunks: u32,
    /// Arena quads in use.
    pub arena_used_quads: u32,
    /// Arena capacity.
    pub arena_capacity_quads: u32,
    /// Chunks passing CPU culling (Mid/Low; High: 0, culling is on the GPU).
    pub visible_chunks: u32,
    /// Draw calls recorded (High: 2 multi-draws, opaque + cutout).
    pub draws: u32,
    /// Chunks not resident because the arena or the chunk table was full (cumulative). Non-zero
    /// means holes in the world: raise `arena_quads`/`max_chunk_slots` or the view distance is
    /// too large for them (M1 adds arena growth). Buckets skipped by a full High draw list are
    /// not counted (see [`RendererConfig::max_chunk_slots`]).
    pub dropped_chunks: u32,
}

/// Renderer errors.
#[derive(Debug, thiserror::Error)]
pub enum RendererError {
    /// The arena cannot fit the mesh (M0: no growth; the chunk is skipped and logged once).
    #[error("quad arena full ({needed} quads requested)")]
    ArenaFull {
        /// Quads requested.
        needed: u32,
    },
    /// No free chunk slot.
    #[error("chunk table full ({0} slots)")]
    ChunkTableFull(u32),
    /// Pipeline/bind group creation failed (error scopes).
    #[error("pipeline creation failed: {0}")]
    Pipeline(String),
}

/// The ECS-free voxel renderer for one device and one tier.
pub struct VoxelRenderer {
    tier: Tier,
    clear_color: [f32; 4],
    /// Group 0: block texture array + sampler.
    materials: wgpu::BindGroup,
    view_buffer: wgpu::Buffer,
    /// Group 1: `View` uniform.
    view_bind_group: wgpu::BindGroup,
    arena: QuadArena,
    chunks: ChunkTable,
    /// GPU chunk table (High/Mid).
    chunk_buffer: Option<wgpu::Buffer>,
    path: Box<dyn TierPath + Send + Sync>,
    targets: Targets,
    blit: Blit,
    /// Opaque draw-list capacity (High; also written to `View::counts.y` on every tier).
    cap_opaque: u32,
    /// Cutout draw-list capacity.
    cap_cutout: u32,
    stats: RenderStats,
    warned_list_overflow: bool,
}

impl VoxelRenderer {
    /// Creates every GPU resource for `tier` inside Validation/Internal/OOM error scopes
    /// (`boxel_gfx::run_in_error_scopes`, nested scopes are fine): block texture array (from
    /// `textures`, procedural), sampler, view uniform, quad arena, chunk table (High/Mid) / CPU
    /// table (all), shared quad index buffer (`[0,1,2,2,1,3] + 4i` for `MAX_QUADS_PER_BUCKET`
    /// quads, u32; High/Mid), the tier's opaque and cutout pipelines (+ cull on High), main color
    /// (`MAIN_COLOR_FORMAT`, `RENDER_ATTACHMENT | COPY_SRC | TEXTURE_BINDING`) and depth targets
    /// of `size`, and the blit pipeline for `output_format` (`fs_blit` if sRGB, else
    /// `fs_blit_encode_srgb`).
    ///
    /// Takes the device parts (not a `GpuContext`) so `RendererSmokeTest` can build a real
    /// renderer during selection. Limits are read with `device.limits()`. Must not touch any
    /// surface: the render plugin builds the renderer BEFORE configuring the surface.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tier: Tier,
        config: &RendererConfig,
        textures: &[TextureSpec],
        output_format: wgpu::TextureFormat,
        size: UVec2,
    ) -> Result<Self, RendererError> {
        let mut built = None;
        boxel_gfx::run_in_error_scopes(device, || {
            built = Some(Self::build(
                device,
                queue,
                tier,
                config,
                textures,
                output_format,
                size,
            )?);
            Ok(())
        })
        .map_err(RendererError::Pipeline)?;
        built.ok_or_else(|| RendererError::Pipeline("renderer was not built".to_string()))
    }

    fn build(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tier: Tier,
        config: &RendererConfig,
        textures: &[TextureSpec],
        output_format: wgpu::TextureFormat,
        size: UVec2,
    ) -> Result<Self, String> {
        let limits = device.limits();
        let (materials_view, sampler) = block_textures(device, queue, textures, &limits)?;

        let view_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel view"),
            size: size_of::<ViewUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Slots and arena sized from the device limits.
        let storage_limit = limits
            .max_storage_buffer_binding_size
            .min(limits.max_buffer_size);
        let mut slots = config.max_chunk_slots.clamp(1, MAX_SLOTS);
        if tier == Tier::High {
            slots = slots.min(
                bindings::CULL_WORKGROUP_SIZE
                    .saturating_mul(limits.max_compute_workgroups_per_dimension),
            );
        }
        if tier >= Tier::Mid {
            slots = slots.min(u32::try_from(storage_limit / CHUNK_GPU_BYTES).unwrap_or(u32::MAX));
        }
        let slots = slots.max(1);
        let arena_limit = if tier >= Tier::Mid {
            storage_limit
        } else {
            limits.max_buffer_size
        };
        let arena_quads = u64::from(config.arena_quads)
            .min(arena_limit / crate::arena::QUAD_BYTES)
            .max(1) as u32;
        let arena = QuadArena::new(device, arena_quads, 2 * slots + 4);
        let chunk_buffer = (tier >= Tier::Mid).then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel chunk table"),
                size: u64::from(slots) * CHUNK_GPU_BYTES,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let cap_opaque = (6 * slots).min(bindings::MAX_DRAWS_PER_LIST);
        let cap_cutout = slots.min(bindings::MAX_DRAWS_PER_LIST);

        let TierPipelines {
            layouts,
            voxel,
            cull,
        } = TierPipelines::new(device, tier);
        let materials = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("boxel materials"),
            layout: &layouts.materials,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&materials_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let view_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("boxel view"),
            layout: &layouts.view,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view_buffer.as_entire_binding(),
            }],
        });

        let path: Box<dyn TierPath + Send + Sync> = if tier == Tier::Low {
            Box::new(LowPath::new(voxel))
        } else {
            let (Some(geometry_layout), Some(chunk_buffer)) =
                (layouts.geometry.as_ref(), chunk_buffer.as_ref())
            else {
                return Err("internal: missing geometry resources".to_string());
            };
            let geometry =
                geometry_bind_group(device, geometry_layout, arena.buffer(), chunk_buffer);
            let index = quad_index_buffer(device);
            match cull {
                Some(cull) => Box::new(HighPath::new(
                    device,
                    voxel,
                    cull,
                    HighInputs {
                        view: &view_buffer,
                        chunks: chunk_buffer,
                        geometry,
                        index,
                        cap_opaque,
                        cap_cutout,
                    },
                )),
                None => Box::new(MidPath::new(device, voxel, geometry, index, slots)),
            }
        };

        let targets = Targets::new(device, size);
        let blit = Blit::new(device, output_format, &targets.color_view);
        Ok(Self {
            tier,
            clear_color: config.clear_color,
            materials,
            view_buffer,
            view_bind_group,
            stats: RenderStats::default(),
            arena,
            chunks: ChunkTable::new(slots),
            chunk_buffer,
            path,
            targets,
            blit,
            cap_opaque,
            cap_cutout,
            warned_list_overflow: false,
        })
    }

    /// Active tier.
    pub fn tier(&self) -> Tier {
        self.tier
    }
    /// Recreates size-dependent targets (no-op for 0 sizes).
    pub fn resize(&mut self, device: &wgpu::Device, size: UVec2) {
        if size.x == 0 || size.y == 0 || size == self.targets.size {
            return;
        }
        self.targets = Targets::new(device, size);
        self.blit.rebind(device, &self.targets.color_view);
    }
    /// Uploads (or replaces) a chunk mesh. `id` is any stable `u64` (`ChunkId::to_bits`),
    /// `origin` the chunk's world voxel min corner. Empty meshes remove the chunk.
    pub fn upsert_chunk(
        &mut self,
        queue: &wgpu::Queue,
        id: u64,
        origin: IVec3,
        mesh: &ChunkMesh,
    ) -> Result<(), RendererError> {
        if mesh.is_empty() {
            self.remove_chunk(queue, id);
            return Ok(());
        }
        if self.chunks.slot_for(id).is_none() {
            self.stats.dropped_chunks += 1;
            return Err(RendererError::ChunkTableFull(self.chunks.capacity()));
        }
        let needed = mesh.total_quads();
        let old = self.chunks.get(id).map(|(_, r)| r.alloc);
        let Some(alloc) = self.arena.alloc.replace(old, needed) else {
            // Nothing changed: a resident chunk keeps its old mesh (range and slot untouched).
            self.stats.dropped_chunks += 1;
            return Err(RendererError::ArenaFull { needed });
        };
        self.arena.write(queue, &alloc, &mesh.quads);
        let record = ChunkRecord {
            id,
            origin,
            alloc,
            counts: bucket_counts(mesh),
        };
        let Some(slot) = self.chunks.insert(record) else {
            // unreachable: `slot_for` succeeded above, so this is a new chunk (no old range)
            self.arena.alloc.free(alloc);
            self.stats.dropped_chunks += 1;
            return Err(RendererError::ChunkTableFull(self.chunks.capacity()));
        };
        self.write_slot(queue, slot, &record.gpu());
        Ok(())
    }
    /// Removes a chunk; returns false if unknown.
    pub fn remove_chunk(&mut self, queue: &wgpu::Queue, id: u64) -> bool {
        let Some((slot, record)) = self.chunks.remove(id) else {
            return false;
        };
        self.arena.alloc.free(record.alloc);
        self.write_slot(queue, slot, &ChunkGpu::default());
        true
    }
    /// Removes every chunk.
    pub fn clear_chunks(&mut self, queue: &wgpu::Queue) {
        let slot_count = self.chunks.slot_count();
        self.chunks.clear();
        self.arena.alloc.reset();
        if let Some(buffer) = &self.chunk_buffer
            && slot_count > 0
        {
            let zeros = vec![0u8; slot_count as usize * CHUNK_GPU_BYTES as usize];
            queue.write_buffer(buffer, 0, &zeros);
        }
    }
    /// Resident chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
    /// Encodes the tier's passes into the main color/depth targets: clear, (High) cull, opaque
    /// buckets with the opaque pipeline, then the cutout bucket with the cutout pipeline.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &ViewParams,
    ) {
        // Every resource already exists; the device parameter is kept for M1 arena growth.
        let _ = device;
        let slot_count = self.chunks.slot_count();
        let uniform = ViewUniform {
            view_proj: view.view_proj.to_cols_array_2d(),
            frustum: view.frustum.to_array(),
            render_origin: view.render_origin.extend(0).to_array(),
            camera_rel: view.eye_rel.extend(0.0).to_array(),
            fog_color: view.fog_color,
            fog_params: [view.fog_start, view.fog_end, 0.0, 0.0],
            counts: [slot_count, self.cap_opaque, self.cap_cutout, 0],
        };
        queue.write_buffer(&self.view_buffer, 0, bytemuck::bytes_of(&uniform));
        let visible = self.path.prepare(queue, &self.chunks, view);
        self.path.encode_compute(encoder, slot_count);
        let draws = {
            let mut pass = self.targets.begin_main_pass(encoder, self.clear_color);
            pass.set_bind_group(bindings::GROUP_MATERIALS, &self.materials, &[]);
            pass.set_bind_group(bindings::GROUP_VIEW, &self.view_bind_group, &[]);
            self.path.draw(&mut pass, self.arena.buffer())
        };
        self.stats.visible_chunks = visible;
        self.stats.draws = draws;
        let live = self.chunks.len() as u64;
        if self.tier == Tier::High
            && 6 * live > u64::from(self.cap_opaque)
            && !self.warned_list_overflow
        {
            self.warned_list_overflow = true;
            tracing::warn!(
                "{live} chunks may need more than {} opaque indirect draws: past that, High \
                 skips arbitrary buckets, not the farthest (draw lists are not segmented until M3)",
                self.cap_opaque
            );
        }
    }
    /// Encodes the fullscreen blit of the main color target into `target`.
    pub fn blit(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        self.blit.encode(encoder, target);
    }
    /// Main color texture (`MAIN_COLOR_FORMAT`, has `COPY_SRC`).
    pub fn main_color(&self) -> &wgpu::Texture {
        &self.targets.color
    }
    /// Last frame counters.
    pub fn stats(&self) -> RenderStats {
        RenderStats {
            chunks: self.chunks.len() as u32,
            arena_used_quads: self.arena.alloc.used(),
            arena_capacity_quads: self.arena.alloc.capacity(),
            ..self.stats
        }
    }

    /// Writes one chunk table entry (High/Mid; Low keeps the table on the CPU only).
    fn write_slot(&self, queue: &wgpu::Queue, slot: u32, entry: &ChunkGpu) {
        if let Some(buffer) = &self.chunk_buffer {
            queue.write_buffer(
                buffer,
                u64::from(slot) * CHUNK_GPU_BYTES,
                bytemuck::bytes_of(entry),
            );
        }
    }
}

/// Bucket counts of `mesh` (see [`trim_counts`]).
fn bucket_counts(mesh: &ChunkMesh) -> [u32; BUCKET_COUNT] {
    debug_assert_eq!(
        mesh.bucket_counts.iter().sum::<u32>(),
        mesh.total_quads(),
        "bucket counts do not match the quads"
    );
    trim_counts(mesh.bucket_counts, mesh.total_quads())
}

/// Trims bucket counts from the last bucket so they never cover more than `total` quads (a
/// malformed mesh must not make draws read other chunks' quads).
fn trim_counts(mut counts: [u32; BUCKET_COUNT], total: u32) -> [u32; BUCKET_COUNT] {
    let mut left = total;
    for c in &mut counts {
        *c = (*c).min(left);
        left -= *c;
    }
    counts
}

/// Block texture array (layer-major upload, 5 mips), its `D2Array` view and the block sampler
/// (`Repeat`, mag `Nearest`, min and mip `Linear`).
fn block_textures(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    textures: &[TextureSpec],
    limits: &wgpu::Limits,
) -> Result<(wgpu::TextureView, wgpu::Sampler), String> {
    let generated = generate_block_textures(textures);
    let mut layers = generated.layers;
    let mut data = generated.data;
    // wgpu-hal GL picks the texture target from the layer count: 1 layer -> TEXTURE_2D and a
    // multiple of 6 -> cube map, neither viewable as D2Array. Pad with a copy of the last layer.
    if layers == 1 || layers.is_multiple_of(6) {
        data.extend_from_within(data.len() - layer_bytes()..);
        layers += 1;
    }
    if layers > limits.max_texture_array_layers {
        return Err(format!(
            "{layers} block texture layers exceed max_texture_array_layers {}",
            limits.max_texture_array_layers
        ));
    }
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("boxel block textures"),
            size: wgpu::Extent3d {
                width: BLOCK_TEXTURE_SIZE,
                height: BLOCK_TEXTURE_SIZE,
                depth_or_array_layers: layers,
            },
            mip_level_count: generated.mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &data,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("boxel block textures"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("boxel blocks"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });
    Ok((view, sampler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<VoxelRenderer>();
    }

    #[test]
    fn bucket_counts_never_exceed_the_quads() {
        let counts = [2, 0, 0, 0, 0, 0, 3, 0];
        assert_eq!(trim_counts(counts, 5), counts);
        assert_eq!(
            trim_counts([2, 0, 0, 0, 0, 0, 3, 4], 5),
            [2, 0, 0, 0, 0, 0, 3, 0]
        );
        assert_eq!(
            trim_counts([4, 4, 0, 0, 0, 0, 0, 0], 6),
            [4, 2, 0, 0, 0, 0, 0, 0]
        );
    }
}
