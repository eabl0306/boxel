use boxel_camera::{Camera, CameraView};
use boxel_gfx::Tier;
use boxel_math::{Frustum, IVec3, Mat4, UVec2, Vec3};
use boxel_mesh::ChunkMesh;
use boxel_voxel::TextureSpec;

/// Renderer construction parameters.
#[derive(Clone, Debug)]
pub struct RendererConfig {
    /// Quad arena capacity (clamped to `max_storage_buffer_binding_size / 8` on High/Mid and
    /// `max_buffer_size / 8` on Low).
    pub arena_quads: u32,
    /// Chunk slots. High clamps it to `64 * max_compute_workgroups_per_dimension`; its draw
    /// lists hold `min(6 * slots, 65535)` opaque and `min(slots, 65535)` cutout draws.
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
    /// too large for them (M1 adds arena growth).
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
        todo!("WP7")
    }
    /// Active tier.
    pub fn tier(&self) -> Tier {
        self.tier
    }
    /// Recreates size-dependent targets (no-op for 0 sizes).
    pub fn resize(&mut self, device: &wgpu::Device, size: UVec2) {
        todo!("WP7")
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
        todo!("WP7")
    }
    /// Removes a chunk; returns false if unknown.
    pub fn remove_chunk(&mut self, queue: &wgpu::Queue, id: u64) -> bool {
        todo!("WP7")
    }
    /// Removes every chunk.
    pub fn clear_chunks(&mut self, queue: &wgpu::Queue) {
        todo!("WP7")
    }
    /// Resident chunks.
    pub fn chunk_count(&self) -> usize {
        todo!("WP7")
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
        todo!("WP7")
    }
    /// Encodes the fullscreen blit of the main color target into `target`.
    pub fn blit(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        todo!("WP7")
    }
    /// Main color texture (`MAIN_COLOR_FORMAT`, has `COPY_SRC`).
    pub fn main_color(&self) -> &wgpu::Texture {
        todo!("WP7")
    }
    /// Last frame counters.
    pub fn stats(&self) -> RenderStats {
        todo!("WP7")
    }
}
