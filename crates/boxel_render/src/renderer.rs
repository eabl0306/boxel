use crate::arena::{QUAD_BYTES, QuadArena};
use crate::chunk_table::{ChunkRecord, ChunkTable};
use crate::light::SceneLight;
use crate::path::{
    HighInputs, HighPath, LowPath, MidPath, SlotVisibility, TierPath, TierPipelines,
    geometry_bind_group, quad_index_buffer,
};
use crate::sort::{SortedBucket, TranslucentSort, TranslucentSorter};
use crate::targets::{Blit, Targets};
use crate::textures::BlockTextureData;
use crate::upload::{UploadTargets, Uploads, arena_len, light_entries};
use boxel_camera::{Camera, CameraView};
use boxel_gfx::Tier;
use boxel_math::{Frustum, I64Vec3, Mat4, UVec2, Vec3, wrap_to_i32};
use boxel_mesh::{BUCKET_COUNT, ChunkMesh, TRANSLUCENT_BUCKET, pack_model_box};
use boxel_shaders::gpu::{
    ChunkGpu, VIEW_BOX_CAPACITY, VIEW_BOXES_OFFSET, VIEW_BUFFER_SIZE, ViewUniform, bindings,
};
use boxel_voxel::{CHUNK_SHIFT, ChunkPos, ModelBox};
use boxel_world::VisibleSet;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt as _;

/// Upper bound of chunk slots (keeps the allocator node table and the GPU table bounded).
const MAX_SLOTS: u32 = 1 << 20;
/// Bytes of one chunk table entry.
const CHUNK_GPU_BYTES: u64 = size_of::<ChunkGpu>() as u64;

/// The default sky: the clear color and fog color of a noon view (linear RGBA).
pub const DEFAULT_SKY_COLOR: [f32; 4] = [0.30, 0.52, 0.85, 1.0];

/// Renderer construction parameters.
#[derive(Clone, Debug)]
pub struct RendererConfig {
    /// Quad arena capacity (clamped to `max_storage_buffer_binding_size / 8` on High/Mid and
    /// `max_buffer_size / 8` on Low). Each chunk takes its quads plus its light palette (one
    /// entry of 8 bytes per distinct corner light set, usually a few dozen). Low also allocates
    /// a second buffer of the same size (each quad's light, ADR 0015).
    pub arena_quads: u32,
    /// Chunk slots. High clamps it to `64 * max_compute_workgroups_per_dimension`; its draw
    /// lists hold `min(6 * slots, 65535)` opaque and `min(slots, 65535)` cutout draws. When more
    /// buckets are visible, High skips arbitrary ones (not the farthest; not counted in
    /// [`RenderStats`]) and logs one warning; Mid and Low draw them all.
    pub max_chunk_slots: u32,
    /// How the translucent quads inside each chunk are sorted back to front (ADR 0018). Default
    /// [`TranslucentSort::Background`]; [`VoxelRenderer::set_translucent_sort`] changes it later.
    pub translucent_sort: TranslucentSort,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            arena_quads: 8 << 20,
            max_chunk_slots: 32768,
            translucent_sort: TranslucentSort::default(),
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
    /// Integer origin (`i64` world voxels; the GPU gets its low 32 bits).
    pub render_origin: I64Vec3,
    /// Eye relative to `render_origin`.
    pub eye_rel: Vec3,
    /// Fog color (linear RGBA).
    pub fog_color: [f32; 4],
    /// Fog start distance.
    pub fog_start: f32,
    /// Fog end distance.
    pub fog_end: f32,
    /// Clear color of the main pass, the sky behind the terrain (linear RGBA).
    pub clear_color: [f32; 4],
    /// The scene light the baked corner light is multiplied by (sky color, ambient floor, AO
    /// strength, brightness curve).
    pub light: SceneLight,
}

impl ViewParams {
    /// From a camera view: fog from `camera.view_distance * fog_start_fraction` to the distance,
    /// `sky_color` as both the fog and the clear color, and a noon [`SceneLight`] (set
    /// [`ViewParams::light`] for another time of day).
    pub fn from_camera(
        view: &CameraView,
        camera: &Camera,
        sky_color: [f32; 4],
        fog_start_fraction: f32,
    ) -> Self {
        Self {
            view_proj: view.view_proj,
            frustum: view.frustum,
            render_origin: view.render_origin,
            eye_rel: view.eye_rel,
            fog_color: sky_color,
            fog_start: camera.view_distance * fog_start_fraction,
            fog_end: camera.view_distance,
            clear_color: sky_color,
            light: SceneLight::default(),
        }
    }
}

/// Counters of the last rendered frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Resident chunks.
    pub chunks: u32,
    /// Arena entries in use: the quads of every resident chunk plus their light palettes.
    pub arena_used_quads: u32,
    /// Arena capacity: `RendererConfig::arena_quads`, cut to the device's buffer limits.
    pub arena_capacity_quads: u32,
    /// Chunk table capacity: `RendererConfig::max_chunk_slots`, cut to the device's limits.
    pub chunk_slot_capacity: u32,
    /// Chunks passing CPU culling (Mid/Low; High: 0, culling is on the GPU).
    pub visible_chunks: u32,
    /// Draw calls recorded (High: 2 multi-draws, opaque + cutout, plus one draw per visible
    /// chunk with translucent quads).
    pub draws: u32,
    /// Chunks not resident because the arena or the chunk table was full (cumulative). Non-zero
    /// means holes in the world: raise `arena_quads`/`max_chunk_slots` or the view distance is
    /// too large for them (M1 adds arena growth). Buckets skipped by a full High draw list are
    /// not counted (see [`RendererConfig::max_chunk_slots`]).
    pub dropped_chunks: u32,
    /// Resident chunks inside the GPU window that cave culling hides this frame (ADR 0022): the
    /// world's visibility search did not reach them, because they are behind closed rock or
    /// outside the frustum. 0 without a visible set ([`VoxelRenderer::set_visibility`]).
    pub occluded_chunks: u32,
    /// Time the last frame spent bringing the cave culling bits up to date (the
    /// `render.occlusion` span): a walk of the whole chunk table after a new visible set, a
    /// cleared table or a moved GPU window, else only the slots that changed. Not part of the
    /// world's search time (`boxel_world::VisibilityStats::last_search`).
    pub occlusion_time: Duration,
    /// Translucent buckets re-sorted for the camera and uploaded (cumulative, see
    /// [`TranslucentSort`]).
    pub translucent_sorts: u32,
    /// Their quads (cumulative).
    pub translucent_sorted_quads: u64,
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
    /// Group 0: block texture array + sampler.
    materials: wgpu::BindGroup,
    view_buffer: wgpu::Buffer,
    /// Group 1: `View` uniform.
    view_bind_group: wgpu::BindGroup,
    arena: QuadArena,
    chunks: ChunkTable,
    /// GPU chunk table (High/Mid).
    chunk_buffer: Option<wgpu::Buffer>,
    /// Low: the light palette entry of each arena quad, at the quad's offset (vertex slot 1).
    low_light: Option<wgpu::Buffer>,
    /// Meshes (and, through `chunks`, table entries) waiting for the next `render`.
    uploads: Uploads,
    /// Keeps each chunk's translucent bucket in back-to-front order for the camera.
    sorter: TranslucentSorter,
    /// Scratch: the sorted buckets of a frame.
    sorted: Vec<SortedBucket>,
    path: Box<dyn TierPath + Send + Sync>,
    targets: Targets,
    blit: Blit,
    /// Opaque draw-list capacity (High; also written to `View::counts.y` on every tier).
    cap_opaque: u32,
    /// Cutout draw-list capacity.
    cap_cutout: u32,
    stats: RenderStats,
    warned_list_overflow: bool,
    /// Cave culling: the visible set of the frame and its bits per slot.
    occlusion: Occlusion,
    /// Chunks whose last upload was refused (cave culling must see through them).
    refused: Refused,
}

/// The chunks whose last upload [`VoxelRenderer::upsert_chunk`] refused (chunk table or arena
/// full). The world takes them for drawn with that mesh, but the renderer shows an older mesh or
/// nothing there, and where their faces are missing the rock around them shows what lies behind
/// it: cave culling must see through them (ADR 0022). Their changes go to the world
/// ([`VoxelRenderer::take_refused`]); until the world searched with them, a frame whose visible
/// set reached one is drawn without the set.
#[derive(Debug, Default)]
struct Refused {
    /// Refused chunks by id, with their position.
    by_id: HashMap<u64, ChunkPos>,
    /// Changes for [`VoxelRenderer::take_refused`], in order: `true` refused, `false` shown as
    /// the world has it again (or removed).
    changes: Vec<(ChunkPos, bool)>,
    /// Positions refused since the last [`VoxelRenderer::render`].
    fresh: Vec<ChunkPos>,
}

impl Refused {
    /// The upload of chunk `id` at `pos` was refused.
    fn refuse(&mut self, id: u64, pos: ChunkPos) {
        if self.by_id.insert(id, pos).is_none() {
            self.changes.push((pos, true));
            self.fresh.push(pos);
        }
    }

    /// Chunk `id` shows what the world sent last: a mesh was uploaded, or it was removed.
    fn shown(&mut self, id: u64) {
        if let Some(pos) = self.by_id.remove(&id) {
            self.changes.push((pos, false));
        }
    }

    /// Every chunk was removed.
    fn clear(&mut self) {
        self.changes
            .extend(self.by_id.drain().map(|(_, pos)| (pos, false)));
    }

    /// True if `set` reached a chunk refused since the last call: it may hide what shows
    /// through that chunk. Forgets them.
    fn take_fresh(&mut self, set: Option<&VisibleSet>) -> bool {
        let reached = set.is_some_and(|set| {
            self.fresh
                .iter()
                .any(|&pos| set.covers(pos) && set.is_visible(pos))
        });
        self.fresh.clear();
        reached
    }
}

/// The cave culling input of the renderer (ADR 0022): the world's visible set and one bit per
/// chunk slot derived from it. A new set (or a cleared table, or a moved window) rebuilds every
/// bit; otherwise only the slots inserted or removed since the last frame are looked at.
#[derive(Default)]
struct Occlusion {
    set: Option<Arc<VisibleSet>>,
    /// Bit `slot & 31` of word `slot >> 5`: the slot's chunk may be seen.
    bits: Vec<u32>,
    /// Same layout: the slot is live, inside the GPU window, and its bit is 0 (counted in
    /// `hidden`).
    hidden_bits: Vec<u32>,
    /// `bits` match `set` and this table version.
    built: Option<u64>,
    /// The bits changed since the last frame.
    changed: bool,
    /// Live slots inside the GPU window whose bit is 0.
    hidden: u32,
    /// Scratch: the slots touched since the last frame.
    touched: Vec<u32>,
}

impl Occlusion {
    /// Replaces the visible set (a different set rebuilds the bits).
    fn set(&mut self, set: Option<Arc<VisibleSet>>) {
        let same = match (&self.set, &set) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.set = set;
            self.built = None;
        }
    }

    /// Brings the bits up to date with `chunks`; returns them, `None` without a set.
    fn refresh(&mut self, chunks: &mut ChunkTable) -> Option<SlotVisibility<'_>> {
        let Some(set) = &self.set else {
            chunks.take_touched(|_| {});
            self.hidden = 0;
            self.built = None;
            return None;
        };
        let words = chunks.slot_count().div_ceil(32) as usize;
        let version = chunks.version();
        self.changed = false;
        if self.built != Some(version) {
            chunks.take_touched(|_| {});
            self.built = Some(version);
            self.changed = true;
            self.bits.clear();
            self.bits.resize(words, 0);
            self.hidden_bits.clear();
            self.hidden_bits.resize(words, 0);
            self.hidden = 0;
            for (slot, record) in chunks.iter() {
                let (word, bit) = (slot as usize >> 5, 1 << (slot & 31));
                let pos = ChunkPos::from_i64vec3(record.origin >> CHUNK_SHIFT);
                if set.is_visible(pos) {
                    self.bits[word] |= bit;
                } else if chunks.in_window(record.origin) {
                    self.hidden_bits[word] |= bit;
                    self.hidden += 1;
                }
            }
        } else {
            if self.bits.len() < words {
                self.bits.resize(words, 0);
                self.hidden_bits.resize(words, 0);
            }
            let (bits, hidden_bits, hidden) =
                (&mut self.bits, &mut self.hidden_bits, &mut self.hidden);
            let mut changed = false;
            let touched = &mut self.touched;
            touched.clear();
            chunks.take_touched(|slot| touched.push(slot));
            for &slot in touched.iter() {
                let (word, bit) = (slot as usize >> 5, 1 << (slot & 31));
                let Some(w) = bits.get_mut(word) else {
                    continue;
                };
                let (visible, counted) = match chunks.record_at(slot) {
                    Some(record) => {
                        let pos = ChunkPos::from_i64vec3(record.origin >> CHUNK_SHIFT);
                        let visible = set.is_visible(pos);
                        (visible, !visible && chunks.in_window(record.origin))
                    }
                    // A free slot is never drawn; its bit only has to be 0.
                    None => (false, false),
                };
                let before = *w;
                *w = if visible { *w | bit } else { *w & !bit };
                changed |= *w != before;
                let h = &mut hidden_bits[word];
                if (*h & bit != 0) != counted {
                    *h ^= bit;
                    if counted {
                        *hidden += 1;
                    } else {
                        *hidden -= 1;
                    }
                }
            }
            self.changed = changed;
        }
        Some(SlotVisibility {
            bits: &self.bits,
            changed: self.changed,
        })
    }
}

impl VoxelRenderer {
    /// Creates every GPU resource for `tier` inside Validation/Internal/OOM error scopes
    /// (`boxel_gfx::run_in_error_scopes`, nested scopes are fine): block texture array (from
    /// `textures`: [`crate::generate_block_textures`] or [`crate::load_block_textures`]),
    /// sampler, view uniform, quad arena, chunk table (High/Mid) / CPU table (all), shared quad index buffer (`[0,1,2,2,1,3] + 4i` for `MAX_QUADS_PER_BUCKET`
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
        textures: &BlockTextureData,
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
        textures: &BlockTextureData,
        output_format: wgpu::TextureFormat,
        size: UVec2,
    ) -> Result<Self, String> {
        let limits = device.limits();
        let (materials_view, sampler) = block_textures(device, queue, textures, &limits)?;

        let view_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel view"),
            size: VIEW_BUFFER_SIZE,
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
            .min(arena_limit / QUAD_BYTES)
            .max(1) as u32;
        let arena = QuadArena::new(device, arena_quads, 2 * slots + 4);
        let low_light = (tier == Tier::Low).then(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel low light"),
                size: u64::from(arena_quads) * QUAD_BYTES,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
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

        let path: Box<dyn TierPath + Send + Sync> = if let Some(light) = &low_light {
            Box::new(LowPath::new(voxel, light.clone()))
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
                        slots,
                    },
                )),
                None => Box::new(MidPath::new(device, voxel, geometry, index, slots)),
            }
        };

        let targets = Targets::new(device, size);
        let blit = Blit::new(device, output_format, &targets.color_view);
        Ok(Self {
            tier,
            materials,
            view_buffer,
            view_bind_group,
            stats: RenderStats::default(),
            arena,
            chunks: ChunkTable::new(slots),
            chunk_buffer,
            low_light,
            uploads: Uploads::default(),
            sorter: TranslucentSorter::new(config.translucent_sort),
            sorted: Vec::new(),
            path,
            targets,
            blit,
            cap_opaque,
            cap_cutout,
            warned_list_overflow: false,
            occlusion: Occlusion::default(),
            refused: Refused::default(),
        })
    }

    /// Sets the chunks the camera may see (cave culling, ADR 0022): from the next
    /// [`VoxelRenderer::render`] on, every tier skips a resident chunk whose position the set
    /// does not hold as visible (a chunk outside the set's box is visible). `None` (the default)
    /// draws every chunk the frustum passes. The set must come from a search for the camera of
    /// the frames it is used in ([`boxel_world::ChunkVisibility::update`]), told about the
    /// chunks this renderer refused ([`VoxelRenderer::take_refused`]): a stale set can hide a
    /// chunk that became visible. A `render` after an upload was refused, whose set reached
    /// that chunk, draws without the set, and so does one that lost uploads
    /// ([`VoxelRenderer::take_lost_uploads`]), until they are taken.
    pub fn set_visibility(&mut self, set: Option<Arc<VisibleSet>>) {
        self.occlusion.set(set);
    }

    /// Calls `f` with every change since the last call, in order, of the chunks whose last
    /// upload was refused ([`RendererError::ChunkTableFull`] or [`RendererError::ArenaFull`]):
    /// `(position, true)` when refused, `(position, false)` when the chunk shows what was sent
    /// last again (a later upload, a removal, [`VoxelRenderer::clear_chunks`]). Cave culling
    /// sees through a refused chunk ([`boxel_world::ChunkVisibility::set_unshown`]).
    pub fn take_refused(&mut self, mut f: impl FnMut(ChunkPos, bool)) {
        for (pos, refused) in self.refused.changes.drain(..) {
            f(pos, refused);
        }
    }

    /// Sets the model box table the `SHAPE_BOX` quads of the meshes index (ADR 0020):
    /// `boxel_mesh::MaterialTable::model_boxes` of the material table the meshes come from. It
    /// goes into the view uniform (after the per-frame part), written here once, and to the
    /// translucent sorter. Until it is set the table is empty: box faces collapse to nothing
    /// (and sort as cube faces). Boxes past [`VIEW_BOX_CAPACITY`] are dropped with an error
    /// log (the registry never makes that many).
    pub fn set_model_boxes(&mut self, queue: &wgpu::Queue, boxes: &[ModelBox]) {
        let boxes = if boxes.len() > VIEW_BOX_CAPACITY {
            tracing::error!(
                "{} model boxes, the view uniform holds {VIEW_BOX_CAPACITY}",
                boxes.len()
            );
            &boxes[..VIEW_BOX_CAPACITY]
        } else {
            boxes
        };
        let mut words = vec![[0u32; 2]; VIEW_BOX_CAPACITY];
        for (slot, &b) in words.iter_mut().zip(boxes) {
            *slot = pack_model_box(b);
        }
        queue.write_buffer(
            &self.view_buffer,
            VIEW_BOXES_OFFSET,
            bytemuck::cast_slice(&words),
        );
        self.sorter.set_boxes(Arc::from(boxes));
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
    /// `origin` the chunk's world voxel min corner (`i64`; the chunk table on the GPU keeps its
    /// low 32 bits, see [`boxel_math::wrap_to_i32`]). While the camera is about `2^30` voxels or
    /// more away from it on some axis, the chunk stays resident but no tier draws it (its wrapped
    /// origin difference may not be exact). Empty meshes remove the chunk.
    ///
    /// The chunk's arena range holds its quads and then its light palette (High and Mid read it
    /// there; Low gets each quad's entry in a parallel buffer). Every quad's light index should
    /// be inside the palette (the mesher guarantees it; debug-asserted). A mesh without a
    /// palette, or a quad whose index is past it, is lit as open sky on every tier.
    ///
    /// Chunk changes stay on the CPU until the next [`VoxelRenderer::render`], which records all
    /// of them at the start of its encoder (one staging buffer for the whole frame). Until then
    /// the renderer holds `mesh` (no copy of its quads).
    pub fn upsert_chunk(
        &mut self,
        id: u64,
        origin: I64Vec3,
        mesh: Arc<ChunkMesh>,
    ) -> Result<(), RendererError> {
        if mesh.is_empty() {
            self.remove_chunk(id);
            return Ok(());
        }
        let pos = ChunkPos::from_i64vec3(origin >> CHUNK_SHIFT);
        if self.chunks.slot_for(id).is_none() {
            self.stats.dropped_chunks += 1;
            self.refused.refuse(id, pos);
            return Err(RendererError::ChunkTableFull(self.chunks.capacity()));
        }
        debug_assert!(
            mesh.palette.is_empty()
                || mesh
                    .quads
                    .iter()
                    .all(|q| usize::from(q.light_index()) < mesh.palette.len()),
            "a quad's light index is outside its palette"
        );
        let needed = arena_len(&mesh);
        let old = self.chunks.get(id).map(|(_, r)| r.alloc);
        let Some(alloc) = self.arena.alloc.replace(old, needed) else {
            // Nothing changed: a resident chunk keeps its old mesh (range and slot untouched).
            self.stats.dropped_chunks += 1;
            self.refused.refuse(id, pos);
            return Err(RendererError::ArenaFull { needed });
        };
        let record = ChunkRecord {
            id,
            origin,
            alloc,
            counts: bucket_counts(&mesh),
            light_base: alloc.offset + mesh.total_quads(),
            light_len: light_entries(&mesh),
        };
        if self.chunks.insert(record).is_none() {
            // unreachable: `slot_for` succeeded above, so this is a new chunk (no old range)
            self.arena.alloc.free(alloc);
            self.stats.dropped_chunks += 1;
            self.refused.refuse(id, pos);
            return Err(RendererError::ChunkTableFull(self.chunks.capacity()));
        }
        self.refused.shown(id);
        self.sorter.upsert(id, origin, Arc::clone(&mesh));
        self.uploads.mesh(id, alloc.offset, mesh);
        Ok(())
    }
    /// Removes a chunk; returns false if unknown. Reaches the GPU with the next `render`.
    pub fn remove_chunk(&mut self, id: u64) -> bool {
        self.refused.shown(id);
        let Some((_, record)) = self.chunks.remove(id) else {
            return false;
        };
        self.arena.alloc.free(record.alloc);
        self.uploads.forget(id);
        self.sorter.remove(id);
        true
    }
    /// Removes every chunk. Reaches the GPU with the next `render`.
    pub fn clear_chunks(&mut self) {
        self.refused.clear();
        self.chunks.clear();
        self.arena.alloc.reset();
        self.uploads.clear();
        self.sorter.clear();
    }
    /// How the translucent quads inside each chunk are sorted ([`RendererConfig::translucent_sort`]).
    pub fn translucent_sort(&self) -> TranslucentSort {
        self.sorter.mode()
    }
    /// Changes how the translucent quads inside each chunk are sorted. Turning it off keeps the
    /// order each chunk has on the GPU until its next mesh (the mesher's order).
    pub fn set_translucent_sort(&mut self, mode: TranslucentSort) {
        self.sorter.set_mode(mode);
    }
    /// Resident chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
    /// True once after a [`VoxelRenderer::render`] that could not record all its chunk uploads
    /// (a staging buffer could not be created, as when out of memory). The frame itself is valid
    /// and runs without them, so nothing else reports it: the GPU no longer matches the resident
    /// chunks until every chunk is uploaded again ([`VoxelRenderer::clear_chunks`], then
    /// [`VoxelRenderer::upsert_chunk`] each). Until this is called, frames draw without the
    /// visible set ([`VoxelRenderer::set_visibility`]).
    pub fn take_lost_uploads(&mut self) -> bool {
        self.uploads.take_lost()
    }
    /// Testing: the next [`VoxelRenderer::render`] with chunk changes cannot create its first
    /// staging buffer, as when out of memory (wgpu reports an error), so those uploads are lost.
    #[doc(hidden)]
    pub fn debug_fail_next_upload(&mut self) {
        self.uploads.fail_next_batch();
    }
    /// Encodes the tier's passes into the main color/depth targets: the chunk changes since the
    /// last call and the translucent buckets re-sorted for this camera (uploads), clear, (High) cull, opaque buckets with the opaque pipeline, then the
    /// cutout bucket with the cutout pipeline, then the translucent buckets back to front with
    /// the translucent pipeline. Submit `encoder` before the next `render`.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &ViewParams,
    ) {
        // The GPU window follows the camera before any cull: only chunks whose wrapped origin
        // difference is exact are drawn, on every tier (see `chunk_table`). High reads the
        // window from the live flags: a move marks every slot stale, so the upload below sends
        // them all again. Mid and Low test the window in `CpuCull`.
        self.chunks.follow(view.render_origin);
        {
            let _span = frame_span!("render.sort");
            self.sorter.update(&self.chunks, view, &mut self.sorted);
            for sorted in self.sorted.drain(..) {
                let Some((_, record)) = self.chunks.get(sorted.id) else {
                    continue;
                };
                // A trimmed bucket (malformed mesh) keeps the mesher's order.
                if record.counts[TRANSLUCENT_BUCKET] as usize != sorted.quads.len() {
                    continue;
                }
                let start: u32 = record.counts[..TRANSLUCENT_BUCKET].iter().sum();
                self.uploads.sorted(
                    sorted.id,
                    record.alloc.offset + start,
                    sorted.mesh,
                    sorted.quads,
                );
            }
        }
        {
            let _span = frame_span!("render.upload");
            let targets = UploadTargets {
                arena: self.arena.buffer(),
                table: self.chunk_buffer.as_ref(),
                low_light: self.low_light.as_ref(),
            };
            self.uploads
                .flush(device, encoder, targets, &mut self.chunks);
        }
        debug_assert!(self.chunks.is_synced());
        let slot_count = self.chunks.slot_count();
        // Cave culling, unless this frame's set reached a chunk whose upload was just refused
        // (the set trusted its links, but the renderer does not show it as the world has it), or
        // uploads were lost since the caller last asked: the set may trust the links of chunks
        // whose meshes never reached the GPU (the world shows a chunk from the frame its mesh is
        // published), and the caller resyncs only after this frame.
        let reached_refused = self.refused.take_fresh(self.occlusion.set.as_deref());
        let culled = self.occlusion.set.is_some() && !reached_refused && !self.uploads.lost();
        let (sky_light, light_params, light_curve) = view.light.uniform();
        let uniform = ViewUniform {
            view_proj: view.view_proj.to_cols_array_2d(),
            frustum: view.frustum.to_array(),
            // Low 32 bits, like the chunk origins (the shaders subtract them with wrapping).
            render_origin: wrap_to_i32(view.render_origin).extend(0).to_array(),
            camera_rel: view.eye_rel.extend(0.0).to_array(),
            fog_color: view.fog_color,
            fog_params: [view.fog_start, view.fog_end, 0.0, 0.0],
            counts: [
                slot_count,
                self.cap_opaque,
                self.cap_cutout,
                u32::from(culled),
            ],
            sky_light,
            light_params,
            light_curve,
            viewport: [
                self.targets.size.x as f32,
                self.targets.size.y as f32,
                0.0,
                0.0,
            ],
        };
        queue.write_buffer(&self.view_buffer, 0, bytemuck::bytes_of(&uniform));
        // Without the set this frame, the bits stay as they are (High keeps the uploaded ones).
        let refresh_start = Instant::now();
        let visibility = if culled || self.occlusion.set.is_none() {
            let _span = frame_span!("render.occlusion");
            self.occlusion.refresh(&mut self.chunks)
        } else {
            None
        };
        self.stats.occlusion_time = refresh_start.elapsed();
        let used = visibility.is_some();
        let visible = self.path.prepare(queue, &self.chunks, view, visibility);
        let hidden = if used { self.occlusion.hidden } else { 0 };
        self.path.encode_compute(encoder, slot_count);
        let draws = {
            let mut pass = self.targets.begin_main_pass(encoder, view.clear_color);
            pass.set_bind_group(bindings::GROUP_MATERIALS, &self.materials, &[]);
            pass.set_bind_group(bindings::GROUP_VIEW, &self.view_bind_group, &[]);
            self.path.draw(&mut pass, self.arena.buffer())
        };
        self.stats.visible_chunks = visible;
        self.stats.draws = draws;
        self.stats.occluded_chunks = hidden;
        // Only chunks inside the window take draws: the old area still resident after a
        // teleport does not count (it would warn falsely, and only once), nor do the chunks cave
        // culling hides.
        let drawable = (self.chunks.drawable_len() as u64).saturating_sub(u64::from(hidden));
        if self.tier == Tier::High
            && 6 * drawable > u64::from(self.cap_opaque)
            && !self.warned_list_overflow
        {
            self.warned_list_overflow = true;
            tracing::warn!(
                "{drawable} chunks may need more than {} opaque indirect draws: past that, High \
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
    /// View of the main color texture (overlays draw into it before the blit).
    pub fn main_color_view(&self) -> &wgpu::TextureView {
        &self.targets.color_view
    }
    /// Last frame counters.
    pub fn stats(&self) -> RenderStats {
        RenderStats {
            chunks: self.chunks.len() as u32,
            arena_used_quads: self.arena.alloc.used(),
            arena_capacity_quads: self.arena.alloc.capacity(),
            chunk_slot_capacity: self.chunks.capacity(),
            translucent_sorts: self.sorter.stats().sorts,
            translucent_sorted_quads: self.sorter.stats().quads,
            ..self.stats
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

/// Block texture array (layer-major upload, full mip chain), its `D2Array` view and the block
/// sampler (`Repeat`, mag `Nearest`, min and mip `Linear`).
fn block_textures(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    textures: &BlockTextureData,
    limits: &wgpu::Limits,
) -> Result<(wgpu::TextureView, wgpu::Sampler), String> {
    let mut layers = textures.layers;
    let layer_bytes = textures.layer_bytes();
    if textures.data.len() != layers as usize * layer_bytes {
        return Err(format!(
            "block texture data has {} bytes, expected {layers} layers of {layer_bytes}",
            textures.data.len()
        ));
    }
    if textures.size > limits.max_texture_dimension_2d {
        return Err(format!(
            "block texture size {} exceeds max_texture_dimension_2d {}",
            textures.size, limits.max_texture_dimension_2d
        ));
    }
    let mut padded = None;
    // wgpu-hal GL picks the texture target from the layer count: 1 layer -> TEXTURE_2D and a
    // multiple of 6 -> cube map, neither viewable as D2Array. Pad with a copy of the last layer.
    if layers == 1 || layers.is_multiple_of(6) {
        let mut data = textures.data.clone();
        data.extend_from_within(data.len() - layer_bytes..);
        padded = Some(data);
        layers += 1;
    }
    let data = padded.as_deref().unwrap_or(&textures.data);
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
                width: textures.size,
                height: textures.size,
                depth_or_array_layers: layers,
            },
            mip_level_count: textures.mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        data,
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
    fn occlusion_bits_follow_the_set_and_the_slots() {
        use crate::arena::ArenaAllocator;
        use boxel_math::I64Vec3;
        use boxel_world::VisibilityView;
        use boxel_world::visibility::visible_chunks;
        let mut arena = ArenaAllocator::new(1 << 12, 64);
        let mut table = ChunkTable::new(64);
        let mut insert = |table: &mut ChunkTable, id: u64, chunk: I64Vec3| {
            let alloc = arena.allocate(2).expect("arena");
            table.insert(ChunkRecord {
                id,
                origin: chunk << 5,
                alloc,
                counts: [1, 0, 0, 0, 0, 0, 0, 0],
                light_base: alloc.offset + 1,
                light_len: 1,
            });
        };
        // A closed world seen from chunk (0, 0, 0): its 6 neighbors are visible, the rest of
        // the box (radius 3) is not, and chunks outside the box are.
        insert(&mut table, 1, I64Vec3::new(0, 0, -1));
        insert(&mut table, 2, I64Vec3::new(0, 0, -2));
        insert(&mut table, 3, I64Vec3::new(9, 0, 0));
        let view = VisibilityView::new(I64Vec3::new(16, 16, 16), None, 3, 3);
        let set = Arc::new(visible_chunks(&view, |_| 0));
        let mut occlusion = Occlusion::default();
        assert!(occlusion.refresh(&mut table).is_none());
        occlusion.set(Some(Arc::clone(&set)));
        let v = occlusion.refresh(&mut table).expect("bits");
        assert!(v.changed);
        assert_eq!(
            (v.visible(0), v.visible(1), v.visible(2)),
            (true, false, true)
        );
        assert_eq!(occlusion.hidden, 1);
        assert!(!occlusion.refresh(&mut table).expect("bits").changed);
        // The same set again changes nothing; a new slot only sets its own bit.
        occlusion.set(Some(Arc::clone(&set)));
        assert!(!occlusion.refresh(&mut table).expect("bits").changed);
        insert(&mut table, 4, I64Vec3::new(0, 1, 0));
        let v = occlusion.refresh(&mut table).expect("bits");
        assert!(v.changed && v.visible(3));
        // A hidden chunk comes and goes: the count follows.
        insert(&mut table, 5, I64Vec3::new(0, -2, 0));
        occlusion.refresh(&mut table);
        assert_eq!(occlusion.hidden, 2);
        let (slot, _) = table.remove(2).expect("hidden chunk");
        let v = occlusion.refresh(&mut table).expect("bits");
        assert!(!v.changed, "a removed slot was hidden already");
        assert_eq!(occlusion.hidden, 1);
        insert(&mut table, 6, I64Vec3::new(0, -1, 0));
        let v = occlusion.refresh(&mut table).expect("bits");
        assert!(
            v.changed && v.visible(slot),
            "the freed slot holds a visible chunk now"
        );
        assert_eq!(occlusion.hidden, 1);
        // The incremental bits equal a rebuild.
        let incremental = occlusion.bits.clone();
        occlusion.built = None;
        occlusion.refresh(&mut table);
        assert_eq!(occlusion.bits, incremental);
        assert_eq!(occlusion.hidden, 1);
        occlusion.set(None);
        assert!(occlusion.refresh(&mut table).is_none());
        assert_eq!(occlusion.hidden, 0);
    }

    #[test]
    fn refused_uploads_are_reported_once_and_released() {
        use boxel_math::I64Vec3;
        use boxel_world::VisibilityView;
        use boxel_world::visibility::visible_chunks;
        let (a, b) = (ChunkPos::new(0, 0, -1), ChunkPos::new(9, 0, 0));
        let mut refused = Refused::default();
        let take = |r: &mut Refused| std::mem::take(&mut r.changes);
        refused.refuse(1, a);
        refused.refuse(1, a);
        refused.refuse(2, b);
        assert_eq!(take(&mut refused), [(a, true), (b, true)]);
        // A closed world seen from chunk (0, 0, 0): its neighbor `a` is reached, `b` is outside
        // the box (never searched).
        let view = VisibilityView::new(I64Vec3::new(16, 16, 16), None, 3, 3);
        let set = visible_chunks(&view, |_| 0);
        assert!(refused.take_fresh(Some(&set)));
        assert!(!refused.take_fresh(Some(&set)), "only once");
        refused.refuse(3, ChunkPos::new(0, 0, -2));
        assert!(
            !refused.take_fresh(Some(&set)),
            "not reached: its links were not used"
        );
        refused.refuse(4, b);
        assert!(!refused.take_fresh(Some(&set)), "outside the box");
        refused.refuse(5, ChunkPos::new(1, 0, 0));
        assert!(!refused.take_fresh(None));
        // Shown again by an upload or a removal; unknown ids change nothing.
        refused.shown(1);
        refused.shown(1);
        refused.shown(7);
        assert_eq!(take(&mut refused)[3..], [(a, false)]);
        refused.clear();
        let mut released = take(&mut refused);
        released.sort_by_key(|(p, _)| (p.x, p.y, p.z));
        assert_eq!(
            released,
            [
                (ChunkPos::new(0, 0, -2), false),
                (ChunkPos::new(1, 0, 0), false),
                (b, false),
                (b, false),
            ]
        );
        assert!(refused.by_id.is_empty());
    }

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
