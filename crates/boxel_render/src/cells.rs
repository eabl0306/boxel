//! Cell quads (EW-ITEM, ADR 0050): the quads cells draw from their data (an item in a frame, the
//! items on a shelf), baked into the chunk meshes ([`ChunkMesh::cell_quads`]) and drawn here, in
//! the main pass right after the solid terrain, with one pipeline shared by every tier
//! (`cell.wgsl`: one uniform block, one texture array and its sampler, plain vertices).
//!
//! A quad's image is a layer of one of two texture arrays: the cell textures, which the content
//! gives ([`crate::CellTextures`]), or the block textures the terrain draws with (a block drawn
//! small in a frame, ADR 0053). The block texture array is almost full and Low binds one texture
//! per pipeline, so the cell images cannot share the block layers nor sit beside them in the
//! terrain pipelines; and the pipeline here binds one array too. So the quads are kept in two
//! lists, one per array, each drawn with the same pipeline and its own bind group: the shader and
//! its bindings are the same on every tier, only group 0 changes between the two draws. The
//! vertices of every chunk with cell quads live in one vertex buffer per list, rebuilt when one
//! of those chunks changes (cell quads are few: an item a frame); a camera move changes nothing,
//! since the shader subtracts the render origin as the terrain does. Each frame the chunks inside
//! the GPU window and the frustum are drawn, runs of consecutive ones as one draw. Without cell
//! quads nothing of this is built and nothing runs.
//!
//! The content can also give quads that are not in any mesh, for the frames it gives them
//! ([`MovingCellQuad`], ADR 0054): what moves (a lid that opens, a bell that swings, a beam that
//! turns). They are drawn by the same pipeline after the chunks' quads, in two more lists (one
//! per texture array) rebuilt and uploaded only on a frame whose list differs from the last one.
//!
//! Translucent cell quads ([`CellBlend::Translucent`], ADR 0055: a stained glass block in a
//! frame, a banner's pattern) are kept apart, in four more lists (the chunks' and the moving
//! ones, per texture array), and drawn last with a second pipeline of the same shader that
//! blends and writes depth, as Minecraft's translucent items: back to front by the distance from
//! the eye to each quad's nearest point, without the quads whose face looks away from the eye.
//! Glows ([`CellBlend::Glow`]: the beacon beam's outer layer) are sorted with them and drawn by
//! a third pipeline that writes no depth. The sort writes only an index buffer (6 indices a
//! quad); it runs on a frame whose eye, drawn chunks or quads changed. The pass draws before the
//! terrain's translucent quads, so water or glass in front of a translucent cell quad tints it,
//! and the fog and clouds see a translucent one as opaque (they read the depth) and paint over a
//! glow.
//!
//! Pattern quads ([`CellImage::Pattern`], ADR 0056: Minecraft's end portal and end gateway) show
//! a pattern fixed to the screen from a third texture array, the pattern textures the content
//! gives ([`crate::PatternTextures`]). They are kept in one more list per kind (the chunks' and
//! the moving ones) and drawn opaque after the cutout quads by a fourth pipeline of the same
//! shader (`vs_cell_pattern`, `fs_cell_pattern`), with the pattern array as group 0: one uniform
//! and one texture/sampler pair, as every other pipeline here.
//!
//! On High with sun shadows (ADR 0065, lane S5) the cutout quads cast into the shadow maps (a
//! sign's board and text, an item in a frame: the quads whose face looks away from the sun, the
//! chunks' ones inside the GPU window and the cascade's box, and every moving one), and the
//! cutout, translucent and glow quads receive them through `CellShadowed`, the same shader with
//! the shadow maps' sun factor. They are built the first time shadows are on
//! ([`CellPass::set_shadowed`]).

use crate::chunk_table::ChunkTable;
use crate::renderer::ViewParams;
use crate::shadow::{CascadeTarget, caster_depth};
use crate::textures::BlockTextureData;
use crate::{DEPTH_FORMAT, MAIN_COLOR_FORMAT};
use boxel_math::{Aabb, I64Vec3, Vec3, wrap_to_i32};
use boxel_mesh::{ChunkMesh, LitCellQuad};
use boxel_shaders::gpu::shadow::{GROUP_CASCADE, GROUP_RECEIVE};
use boxel_shaders::gpu::{CellVertex, bindings, cell};
use boxel_shaders::{ShaderModuleId, create_module};
use boxel_voxel::{BlockPos, CellBlend, CellImage, CellQuad, LightValue};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

/// Edge of a chunk in blocks, for the frustum test.
const CHUNK_EDGE: f32 = 32.0;
/// Corner order of a quad's two triangles (corner `k` at `(k & 1, k >> 1)`).
const TRIANGLES: [usize; 6] = [0, 1, 2, 2, 1, 3];

/// Cell quad counters of the last frame (all 0 before the first chunk with cell quads).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellStats {
    /// Resident chunks with cell quads.
    pub chunks: u32,
    /// Their cell quads.
    pub quads: u32,
    /// Those of them that show a block layer ([`CellImage::Block`], ADR 0053).
    pub block_quads: u32,
    /// Quads drawn this frame (inside the window and the frustum).
    pub drawn: u32,
    /// Draw calls this frame.
    pub draws: u32,
    /// Vertex buffer uploads so far (one per frame with a changed chunk).
    pub uploads: u64,
    /// Layers of the cell texture array (0 until the content gives it).
    pub layers: u32,
    /// Moving cell quads of the last frame ([`MovingCellQuad`], ADR 0054), all drawn.
    pub moving: u32,
    /// Uploads of their vertices so far (one per frame whose list changed).
    pub moving_uploads: u64,
    /// Translucent cell quads drawn this frame (ADR 0055), baked or moving, those facing the eye.
    pub translucent: u32,
    /// Sorts of the translucent quads so far (one per frame whose eye, drawn chunks or quads
    /// changed).
    pub sorts: u64,
    /// Pattern quads drawn this frame (ADR 0056), baked or moving (0 until the content gives
    /// the pattern textures).
    pub patterns: u32,
    /// Layers of the pattern texture array (0 until the content gives it).
    pub pattern_layers: u32,
}

/// A cell quad the content gives for one frame, not baked into a chunk mesh (ADR 0054): a part
/// that moves (a lid that opens, a bell that swings, a beam that turns), drawn like the chunks'
/// cell quads (the same pipeline, both texture arrays, lit by its cell's light without ambient
/// occlusion) but not culled: give only what may show. The render plugin draws the
/// [`crate::MovingCellQuads`] of each frame, lit at their cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovingCellQuad {
    /// The cell whose min corner the quad's corners start from: it may reach out of it (a beam
    /// rises hundreds of blocks), and its light is that cell's.
    pub cell: BlockPos,
    /// The quad, its corners in blocks from `cell`'s min corner.
    pub quad: CellQuad,
}

/// The chunks with cell quads, by id (a `BTreeMap`: the vertex order is the same every time).
#[derive(Default)]
pub(crate) struct CellStore {
    chunks: BTreeMap<u64, (I64Vec3, Arc<ChunkMesh>)>,
    /// A chunk came, went or changed since the last upload.
    dirty: bool,
}

impl CellStore {
    /// Records `mesh` as the mesh of chunk `id` at `origin` (the renderer's `upsert_chunk`):
    /// kept if it has cell quads, else the chunk leaves the store.
    pub(crate) fn upsert(&mut self, id: u64, origin: I64Vec3, mesh: &Arc<ChunkMesh>) {
        if mesh.cell_quads.is_empty() {
            self.remove(id);
            return;
        }
        let same = self
            .chunks
            .get(&id)
            .is_some_and(|(o, m)| *o == origin && (Arc::ptr_eq(m, mesh) || **m == **mesh));
        if !same {
            self.chunks.insert(id, (origin, Arc::clone(mesh)));
            self.dirty = true;
        }
    }

    pub(crate) fn remove(&mut self, id: u64) {
        self.dirty |= self.chunks.remove(&id).is_some();
    }

    pub(crate) fn clear(&mut self) {
        self.dirty |= !self.chunks.is_empty();
        self.chunks.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

/// A bind group of the cell pipeline's group 0: `view` (a `D2Array`) and `sampler`.
fn texture_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("boxel cell textures"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// The sky's red, green and blue of a cell quad without tint (`0x0RGB`): its light's sky level
/// in all three channels.
const fn gray_sky(light: LightValue) -> u16 {
    let s = light.sky() as u16;
    s << 8 | s << 4 | s
}

/// The 6 vertices of `q` (two triangles) in a chunk whose GPU origin is `origin`, its sky's
/// red, green and blue `sky` (`0x0RGB`, ADR 0064; [`gray_sky`] of its light without tint).
pub(crate) fn quad_vertices(origin: [i32; 3], q: &LitCellQuad, sky: u16) -> [CellVertex; 6] {
    let c = &q.quad;
    let data0 = (u32::from(c.image) & cell::LAYER_MASK)
        | (c.face as u32) << cell::FACE_SHIFT
        | if c.full_bright { cell::FULL_BRIGHT } else { 0 }
        | u32::from(q.light.0) << cell::LIGHT_SHIFT;
    let [r, g, b] = c.tint.map(u32::from);
    // A pattern quad keeps its layer count where a translucent one keeps its opacity.
    let high = match c.source {
        CellImage::Pattern(layers) => u32::from(layers).min(cell::PATTERN_LAYERS_MAX),
        _ => u32::from(c.blend.opacity()),
    };
    let data = [data0, high << cell::OPACITY_SHIFT | r << 16 | g << 8 | b];
    TRIANGLES.map(|k| CellVertex {
        origin,
        pos: c.corners[k],
        uv: c.uv[k],
        data,
        sky: u32::from(sky),
    })
}

/// A translucent quad as the sort sees it (ADR 0055): its corner 0 and its two edges from it, in
/// blocks from `origin` (its chunk's min corner, or its cell's), and the side it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SortQuad {
    origin: I64Vec3,
    corner: Vec3,
    u: Vec3,
    v: Vec3,
    /// The quad's normal toward the side its face looks to; zero when that face lies along the
    /// quad (a plane shaded as `PosY` across it): both sides show.
    facing: Vec3,
    /// A glow ([`CellBlend::Glow`]): drawn without writing depth.
    glow: bool,
}

impl SortQuad {
    fn new(origin: I64Vec3, q: &CellQuad) -> Self {
        let [c0, c1, c2, _] = q.corners.map(Vec3::from_array);
        let (u, v) = (c1 - c0, c2 - c0);
        let normal = u.cross(v);
        let face = q.face.normal().as_vec3();
        let along = normal.dot(face);
        let facing = if along.abs() > 0.5 * normal.length() {
            normal * along.signum()
        } else {
            Vec3::ZERO
        };
        Self {
            origin,
            corner: c0,
            u,
            v,
            facing,
            glow: matches!(q.blend, CellBlend::Glow(_)),
        }
    }

    /// The squared distance from `eye` (blocks from `origin`) to the quad's nearest point (its
    /// parallelogram clamped along each edge), or `None` if its face looks away from `eye`.
    fn distance(&self, eye: Vec3) -> Option<f32> {
        let d = eye - self.corner;
        if self.facing != Vec3::ZERO && self.facing.dot(d) <= 0.0 {
            return None;
        }
        let along = |e: Vec3| (d.dot(e) / e.length_squared().max(1e-12)).clamp(0.0, 1.0);
        let nearest = self.corner + self.u * along(self.u) + self.v * along(self.v);
        Some((eye - nearest).length_squared())
    }
}

/// The cell quads whose images are layers of one texture array: their vertices, each chunk's
/// range of them and this frame's draws.
#[derive(Default)]
struct QuadList {
    vertex: Option<wgpu::Buffer>,
    /// Vertices `vertex` holds.
    vertex_capacity: usize,
    vertices: Vec<CellVertex>,
    /// Per chunk with quads in this list, in the store's order: its origin and its vertex range.
    ranges: Vec<(I64Vec3, Range<u32>)>,
    /// This frame's draws (merged vertex ranges).
    draws: Vec<Range<u32>>,
    /// Per quad (6 vertices), a translucent list's: where the sort reads it.
    sort: Vec<SortQuad>,
}

impl QuadList {
    /// This frame's draws: the chunks inside the GPU window (`chunks`) and the frustum of
    /// `view`, runs of consecutive ones merged.
    fn pick(&mut self, chunks: &ChunkTable, view: &ViewParams) {
        self.draws.clear();
        for (origin, range) in &self.ranges {
            if !chunks.in_window(*origin) {
                continue;
            }
            let min = origin.saturating_sub(view.render_origin).as_vec3();
            let max = min + Vec3::splat(CHUNK_EDGE);
            if !view.frustum.intersects_aabb(&Aabb::new(min, max)) {
                continue;
            }
            match self.draws.last_mut() {
                Some(last) if last.end == range.start => last.end = range.end,
                _ => self.draws.push(range.clone()),
            }
        }
    }

    /// Writes the vertices into the vertex buffer (grown to fit); true if it wrote any.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        if self.vertices.is_empty() {
            return false;
        }
        if self.vertex.is_none() || self.vertex_capacity < self.vertices.len() {
            let capacity = self.vertices.len().next_power_of_two().max(6 * 256);
            self.vertex = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel cell vertices"),
                size: (capacity * size_of::<CellVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.vertex_capacity = capacity;
        }
        let Some(buffer) = &self.vertex else {
            return false;
        };
        queue.write_buffer(buffer, 0, bytemuck::cast_slice(&self.vertices));
        true
    }

    /// Draws this frame's quads with `group` as group 0; false if there were none.
    fn draw(&self, pass: &mut wgpu::RenderPass<'_>, group: &wgpu::BindGroup) -> bool {
        let Some(vertex) = &self.vertex else {
            return false;
        };
        if self.draws.is_empty() {
            return false;
        }
        pass.set_bind_group(cell::GROUP_TEXTURES, group, &[]);
        pass.set_vertex_buffer(0, vertex.slice(..));
        for range in &self.draws {
            pass.draw(range.clone(), 0..1);
        }
        true
    }

    /// Draws the quads of the chunks inside the GPU window, the camera realm and `target`'s
    /// cascade into its depth pass with `group` as group 0 (the caster pipeline set), runs of
    /// consecutive chunks as one draw; every quad with `all` (the moving ones). The number of
    /// draws.
    fn draw_casters(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        group: &wgpu::BindGroup,
        target: &CascadeTarget<'_>,
        all: bool,
    ) -> u32 {
        let Some(vertex) = &self.vertex else {
            return 0;
        };
        let mut draws = 0;
        let mut flush = |pass: &mut wgpu::RenderPass<'_>, run: Range<u32>| {
            if draws == 0 {
                pass.set_bind_group(cell::GROUP_TEXTURES, group, &[]);
                pass.set_vertex_buffer(0, vertex.slice(..));
            }
            pass.draw(run, 0..1);
            draws += 1;
        };
        if all {
            if !self.vertices.is_empty() {
                flush(pass, 0..self.vertices.len() as u32);
            }
            return draws;
        }
        let mut run: Option<Range<u32>> = None;
        for (origin, range) in &self.ranges {
            let min = origin.saturating_sub(target.render_origin);
            let in_band = (i64::from(target.band[0])..i64::from(target.band[1])).contains(&min.y);
            let min = min.as_vec3();
            let max = min + Vec3::splat(CHUNK_EDGE);
            if !in_band
                || !target.chunks.in_window(*origin)
                || !target.frustum.intersects_aabb(&Aabb::new(min, max))
            {
                continue;
            }
            match &mut run {
                Some(r) if r.end == range.start => r.end = range.end,
                _ => {
                    if let Some(r) = run.replace(range.clone()) {
                        flush(pass, r);
                    }
                }
            }
        }
        if let Some(r) = run {
            flush(pass, r);
        }
        draws
    }

    fn drawn(&self) -> u32 {
        self.draws.iter().map(|r| r.len() as u32).sum::<u32>() / 6
    }

    /// Draws every vertex this frame (the moving quads: not culled).
    fn pick_all(&mut self) {
        self.draws.clear();
        if !self.vertices.is_empty() {
            self.draws.push(0..self.vertices.len() as u32);
        }
    }

    fn clear(&mut self) {
        self.vertices.clear();
        self.ranges.clear();
        self.sort.clear();
    }

    /// Appends `q` (corners from `origin`, whose GPU origin is `gpu`, its sky `sky`), with its
    /// sort entry.
    fn push(&mut self, origin: I64Vec3, gpu: [i32; 3], q: &LitCellQuad, sky: u16) {
        self.vertices.extend(quad_vertices(gpu, q, sky));
        if q.quad.blend.is_translucent() && !is_pattern(&q.quad) {
            self.sort.push(SortQuad::new(origin, &q.quad));
        }
    }
}

/// True for a quad of the pattern textures (ADR 0056): it goes to the pattern list whatever its
/// blend.
const fn is_pattern(q: &CellQuad) -> bool {
    matches!(q.source, CellImage::Pattern(_))
}

/// The lists of one kind of quads (from the chunks, or moving): the cutout ones and the
/// translucent ones (ADR 0055) per texture array, `[cell, block]` each, and the pattern ones
/// (ADR 0056).
#[derive(Default)]
struct Lists {
    cutout: [QuadList; 2],
    translucent: [QuadList; 2],
    pattern: QuadList,
}

impl Lists {
    fn clear(&mut self) {
        for list in self.all_mut() {
            list.clear();
        }
    }

    fn list(&mut self, q: &CellQuad) -> &mut QuadList {
        if is_pattern(q) {
            return &mut self.pattern;
        }
        let lists = if q.blend.is_translucent() {
            &mut self.translucent
        } else {
            &mut self.cutout
        };
        &mut lists[list_of(q.source)]
    }

    fn all(&self) -> impl Iterator<Item = &QuadList> {
        self.cutout
            .iter()
            .chain(&self.translucent)
            .chain([&self.pattern])
    }

    fn all_mut(&mut self) -> impl Iterator<Item = &mut QuadList> {
        self.cutout
            .iter_mut()
            .chain(&mut self.translucent)
            .chain([&mut self.pattern])
    }

    /// Uploads every list's vertices; true if it wrote any.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        let mut wrote = false;
        for list in self.all_mut() {
            wrote |= list.upload(device, queue);
        }
        wrote
    }
}

/// Which translucent list a sorted quad is in: the chunks' (`0`, `1`) or the moving ones' (`2`,
/// `3`), by texture array (`list_of`).
type ListId = u8;

/// The translucent quads' order this frame (ADR 0055): an index buffer of the quads that face
/// the eye, back to front, drawn as runs of one list.
#[derive(Default)]
struct Sorted {
    index: Option<wgpu::Buffer>,
    /// Indices `index` holds.
    index_capacity: usize,
    indices: Vec<u32>,
    /// This frame's draws: each run's list, whether its quads are glows, and its range of
    /// `indices`.
    runs: Vec<(ListId, bool, Range<u32>)>,
    /// The quads that face the eye: squared distance, list and quad.
    order: Vec<(f32, ListId, u32)>,
    /// The eye of the last sort (its render origin and the bits of its offset from it); `None`
    /// after a change: sort again.
    eye: Option<(I64Vec3, [u32; 3])>,
}

impl Sorted {
    /// Sorts the drawn quads of `lists` (the chunks' translucent lists, then the moving ones) for
    /// the eye of `view`: far first, ties by list and quad (the same order every time).
    fn sort(&mut self, lists: [&QuadList; 4], view: &ViewParams) {
        self.order.clear();
        for (id, list) in lists.iter().enumerate() {
            for range in &list.draws {
                for quad in range.start / 6..range.end / 6 {
                    let q = &list.sort[quad as usize];
                    let offset = q.origin.saturating_sub(view.render_origin).as_vec3();
                    if let Some(d) = q.distance(view.eye_rel - offset) {
                        self.order.push((d, id as ListId, quad));
                    }
                }
            }
        }
        self.order
            .sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        self.indices.clear();
        self.runs.clear();
        for &(_, id, quad) in &self.order {
            let glow = lists[usize::from(id)].sort[quad as usize].glow;
            let at = self.indices.len() as u32;
            match self.runs.last_mut() {
                Some((last, g, range)) if *last == id && *g == glow => range.end = at + 6,
                _ => self.runs.push((id, glow, at..at + 6)),
            }
            self.indices.extend(quad * 6..quad * 6 + 6);
        }
    }

    /// Writes the indices into the index buffer (grown to fit).
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.indices.is_empty() {
            return;
        }
        if self.index.is_none() || self.index_capacity < self.indices.len() {
            let capacity = self.indices.len().next_power_of_two().max(6 * 256);
            self.index = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel translucent cell indices"),
                size: (capacity * size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.index_capacity = capacity;
        }
        if let Some(buffer) = &self.index {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&self.indices));
        }
    }
}

/// Index in a kind's cutout and translucent lists of the quads whose images are in `source`
/// (the pattern quads have a list of their own, `Lists::pattern`).
const fn list_of(source: CellImage) -> usize {
    match source {
        CellImage::Cell | CellImage::Pattern(_) => 0,
        CellImage::Block => 1,
    }
}

/// The vertices of every chunk of `store` in `lists`, each quad in the list of its blend and
/// texture array ([`Lists::list`]), each chunk's run of a list recorded with its origin. Each
/// quad takes its sky from [`ChunkMesh::cell_sky_rgb`] (ADR 0064), or without tint where that is
/// empty (or, malformed, shorter than the quads).
fn fill(lists: &mut Lists, store: &CellStore) {
    lists.clear();
    for (origin, mesh) in store.chunks.values() {
        let starts = lists
            .all()
            .map(|l| l.vertices.len() as u32)
            .collect::<Vec<_>>();
        let gpu = wrap_to_i32(*origin).to_array();
        for (k, q) in mesh.cell_quads.iter().enumerate() {
            let sky = mesh
                .cell_sky_rgb
                .get(k)
                .map_or_else(|| gray_sky(q.light), |&rgb| rgb & 0xFFF);
            lists.list(&q.quad).push(*origin, gpu, q, sky);
        }
        for (list, start) in lists.all_mut().zip(starts) {
            let end = list.vertices.len() as u32;
            if end > start {
                list.ranges.push((*origin, start..end));
            }
        }
    }
}

/// The vertices of the moving quads `quads` (each with its light) in `lists`, each quad in the
/// list of its blend and texture array, placed from its cell. Their sky has no tint (ADR 0064:
/// the moving quads take their cell's uncolored light).
fn fill_moving(lists: &mut Lists, quads: &[(MovingCellQuad, LightValue)]) {
    lists.clear();
    for &(moving, light) in quads {
        let cell = I64Vec3::new(moving.cell.x, moving.cell.y, moving.cell.z);
        let lit = LitCellQuad {
            quad: moving.quad,
            light,
        };
        lists
            .list(&moving.quad)
            .push(cell, wrap_to_i32(cell).to_array(), &lit, gray_sky(light));
    }
}

/// The lists of the moving quads for `benches/cells.rs` (`crate::bench::MovingBench`).
#[derive(Default)]
pub(crate) struct QuadListBench(Lists);

/// [`fill_moving`] into bench lists: the vertices written.
pub(crate) fn fill_moving_bench(
    lists: &mut QuadListBench,
    quads: &[(MovingCellQuad, LightValue)],
) -> usize {
    fill_moving(&mut lists.0, quads);
    lists.0.all().map(|l| l.vertices.len()).sum()
}

/// The translucent quads of one frame for `benches/cells.rs` (`crate::bench::SortBench`): the
/// moving lists filled once, sorted for each eye.
#[derive(Default)]
pub(crate) struct SortBench {
    lists: Lists,
    sorted: Sorted,
}

impl SortBench {
    pub(crate) fn new(quads: &[(MovingCellQuad, LightValue)]) -> Self {
        let mut bench = Self::default();
        fill_moving(&mut bench.lists, quads);
        for list in &mut bench.lists.translucent {
            list.pick_all();
        }
        bench
    }

    /// Sorts for `view`: the indices written.
    pub(crate) fn sort(&mut self, view: &ViewParams) -> usize {
        let empty = QuadList::default();
        let [a, b] = &self.lists.translucent;
        self.sorted.sort([&empty, &empty, a, b], view);
        self.sorted.indices.len()
    }
}

/// High's cell pipelines of sun shadows (ADR 0065, lane S5).
struct ShadowedCells {
    /// `CellShadowed`: the twins of `pipeline`, `translucent` and `glow` that receive the
    /// shadows (group 3).
    pipeline: wgpu::RenderPipeline,
    translucent: wgpu::RenderPipeline,
    glow: wgpu::RenderPipeline,
    /// `CellShadow`: the cutout quads' depth in a cascade's map.
    caster: wgpu::RenderPipeline,
}

/// The pipelines, the cell textures and the vertices of the cell quads (one per renderer).
pub(crate) struct CellPass {
    pipeline: wgpu::RenderPipeline,
    /// The same shader blending its translucent fragment (ADR 0055), writing depth; and the
    /// same without writing it, for the glows.
    translucent: wgpu::RenderPipeline,
    glow: wgpu::RenderPipeline,
    /// The pattern quads' pipeline (ADR 0056): `vs_cell_pattern` and `fs_cell_pattern`, opaque.
    pattern: wgpu::RenderPipeline,
    /// Sun shadows' pipelines, from the first frame with shadows on (High only).
    shadowed: Option<ShadowedCells>,
    /// They could not be built: the cell quads neither cast nor receive.
    shadowed_failed: bool,
    layout: wgpu::BindGroupLayout,
    /// The cell textures and their bind group, with the data they were made from.
    textures: Option<(Arc<BlockTextureData>, wgpu::Texture, wgpu::BindGroup)>,
    /// The pattern textures (ADR 0056) and their bind group, with the data they were made from.
    patterns: Option<(Arc<BlockTextureData>, wgpu::Texture, wgpu::BindGroup)>,
    /// The block textures (the terrain's array, animated layers included) in this pipeline's
    /// layout.
    blocks: wgpu::BindGroup,
    /// The quads of the chunks.
    lists: Lists,
    /// The moving quads (ADR 0054).
    moving: Lists,
    /// The moving quads given last, with their light: a frame that gives the same uploads
    /// nothing.
    moving_input: Vec<(MovingCellQuad, LightValue)>,
    /// This frame's order of the translucent quads.
    sorted: Sorted,
    stats: CellStats,
}

impl CellPass {
    /// The pipelines for `device`; `view_layout` is the renderer's group 1 (the `View` uniform),
    /// `block_texture` the terrain's block texture array.
    pub(crate) fn new(
        device: &wgpu::Device,
        view_layout: &wgpu::BindGroupLayout,
        block_texture: &wgpu::Texture,
    ) -> Self {
        let module = create_module(device, ShaderModuleId::Cell);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel cell textures"),
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
        let mut groups: [Option<&wgpu::BindGroupLayout>; 2] = [None, None];
        groups[cell::GROUP_TEXTURES as usize] = Some(&layout);
        groups[bindings::GROUP_VIEW as usize] = Some(view_layout);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel cell"),
            bind_group_layouts: &groups,
            immediate_size: 0,
        });
        let build =
            |label: &str, entries: (&str, &str), blend: Option<wgpu::BlendState>, depth: bool| {
                cell_pipeline(
                    device,
                    label,
                    &pipeline_layout,
                    &module,
                    (entries.0, Some(entries.1)),
                    CellTarget::Main { blend, depth },
                )
            };
        let pipeline = build("boxel cell", ("vs_cell", "fs_cell"), None, true);
        let blend = Some(wgpu::BlendState::ALPHA_BLENDING);
        let translucent_entries = ("vs_cell", "fs_cell_translucent");
        let translucent = build("boxel cell translucent", translucent_entries, blend, true);
        let glow = build("boxel cell glow", translucent_entries, blend, false);
        let pattern_entries = ("vs_cell_pattern", "fs_cell_pattern");
        let pattern = build("boxel cell pattern", pattern_entries, None, true);
        let blocks = texture_group(
            device,
            &layout,
            &crate::renderer::array_view(block_texture),
            &crate::renderer::block_sampler(device),
        );
        Self {
            pipeline,
            translucent,
            glow,
            pattern,
            shadowed: None,
            shadowed_failed: false,
            layout,
            textures: None,
            patterns: None,
            blocks,
            lists: Lists::default(),
            moving: Lists::default(),
            moving_input: Vec::new(),
            sorted: Sorted::default(),
            stats: CellStats::default(),
        }
    }

    /// High (sun shadows, ADR 0065): builds the pipelines that cast into the maps and receive
    /// them once (the next calls keep them): `view` is the renderer's group 1, `cascade` the
    /// casters' group 3 ([`crate::shadow::ShadowPass::cascade_layout`]), `receive` the
    /// receivers' ([`crate::shadow::receive_layout`]). If they fail, an error is logged and the
    /// cell quads neither cast nor receive on this renderer.
    pub(crate) fn set_shadowed(
        &mut self,
        device: &wgpu::Device,
        view: &wgpu::BindGroupLayout,
        cascade: &wgpu::BindGroupLayout,
        receive: &wgpu::BindGroupLayout,
    ) {
        if self.shadowed.is_some() || self.shadowed_failed {
            return;
        }
        let mut built = None;
        let result = boxel_gfx::run_in_error_scopes(device, || {
            built = Some(self.shadowed_pipelines(device, view, cascade, receive));
            Ok(())
        });
        match (result, built) {
            (Ok(()), Some(shadowed)) => self.shadowed = Some(shadowed),
            (result, _) => {
                tracing::error!(
                    "cell quad shadow pipelines: {}; cell quads cast and receive no sun shadow",
                    result.err().unwrap_or_default()
                );
                self.shadowed_failed = true;
            }
        }
    }

    fn shadowed_pipelines(
        &self,
        device: &wgpu::Device,
        view: &wgpu::BindGroupLayout,
        cascade: &wgpu::BindGroupLayout,
        receive: &wgpu::BindGroupLayout,
    ) -> ShadowedCells {
        let layout = |label, last: &wgpu::BindGroupLayout| {
            let mut groups: [Option<&wgpu::BindGroupLayout>; 4] = [None; 4];
            groups[cell::GROUP_TEXTURES as usize] = Some(&self.layout);
            groups[bindings::GROUP_VIEW as usize] = Some(view);
            groups[3] = Some(last);
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &groups,
                immediate_size: 0,
            })
        };
        debug_assert_eq!((GROUP_RECEIVE, GROUP_CASCADE), (3, 3));
        let receiving = layout("boxel cell shadowed", receive);
        let module = create_module(device, ShaderModuleId::CellShadowed);
        let build = |label: &str, blend: Option<wgpu::BlendState>, depth| {
            let fragment = if blend.is_some() {
                "fs_cell_translucent"
            } else {
                "fs_cell"
            };
            cell_pipeline(
                device,
                label,
                &receiving,
                &module,
                ("vs_cell", Some(fragment)),
                CellTarget::Main { blend, depth },
            )
        };
        let alpha = Some(wgpu::BlendState::ALPHA_BLENDING);
        let casting = layout("boxel cell shadow", cascade);
        let cast_module = create_module(device, ShaderModuleId::CellShadow);
        ShadowedCells {
            pipeline: build("boxel cell shadowed", None, true),
            translucent: build("boxel cell shadowed translucent", alpha, true),
            glow: build("boxel cell shadowed glow", alpha, false),
            caster: cell_pipeline(
                device,
                "boxel cell shadow",
                &casting,
                &cast_module,
                ("vs_cell_shadow", Some("fs_cell_shadow")),
                CellTarget::Caster,
            ),
        }
    }

    /// Draws the cutout quads into `target`'s cascade (its depth pass): the chunks' ones inside
    /// the GPU window, the camera realm and the cascade's box, and every moving one, of the
    /// cell textures (once the content gave them) and the block textures. The quads whose face
    /// does not look away from the sun are collapsed by the shader. The number of draws; none
    /// before [`CellPass::set_shadowed`].
    pub(crate) fn draw_casters(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        target: &CascadeTarget<'_>,
    ) -> u32 {
        let Some(shadowed) = &self.shadowed else {
            return 0;
        };
        let lists = [&self.lists.cutout, &self.moving.cutout];
        if lists
            .iter()
            .flat_map(|l| l.iter())
            .all(|l| l.vertices.is_empty())
        {
            return 0;
        }
        pass.set_pipeline(&shadowed.caster);
        pass.set_bind_group(GROUP_CASCADE, target.group, &[target.offset]);
        let mut draws = 0;
        for (all, [cells, blocks]) in [(false, lists[0]), (true, lists[1])] {
            if let Some((_, _, group)) = &self.textures {
                draws += cells.draw_casters(pass, group, target, all);
            }
            draws += blocks.draw_casters(pass, &self.blocks, target, all);
        }
        draws
    }

    /// Uses `quads` as the moving quads from the next [`CellPass::prepare`] on (uploaded then if
    /// they differ from the last ones); gives back the previous list, to fill again.
    pub(crate) fn set_moving(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        quads: Vec<(MovingCellQuad, LightValue)>,
    ) -> Vec<(MovingCellQuad, LightValue)> {
        if quads == self.moving_input {
            return quads;
        }
        let old = std::mem::replace(&mut self.moving_input, quads);
        fill_moving(&mut self.moving, &self.moving_input);
        let wrote = self.moving.upload(device, queue);
        self.stats.moving_uploads += u64::from(wrote);
        self.sorted.eye = None;
        old
    }

    /// Uses `data` as the cell textures (nothing to do if they are the ones in use). A texture
    /// this device cannot hold is refused with the reason, and the quads are not drawn.
    pub(crate) fn set_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &Arc<BlockTextureData>,
    ) -> Result<(), String> {
        if self
            .textures
            .as_ref()
            .is_some_and(|(d, ..)| Arc::ptr_eq(d, data))
        {
            return Ok(());
        }
        self.textures = None;
        self.stats.layers = 0;
        let (texture, view, sampler) =
            crate::renderer::block_textures(device, queue, data, &device.limits())?;
        let group = texture_group(device, &self.layout, &view, &sampler);
        self.stats.layers = data.layers;
        self.textures = Some((Arc::clone(data), texture, group));
        Ok(())
    }

    /// Uses `data` as the pattern textures (ADR 0056; nothing to do if they are the ones in
    /// use). A texture this device cannot hold is refused with the reason, and the pattern quads
    /// are not drawn.
    pub(crate) fn set_pattern_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &Arc<BlockTextureData>,
    ) -> Result<(), String> {
        if self
            .patterns
            .as_ref()
            .is_some_and(|(d, ..)| Arc::ptr_eq(d, data))
        {
            return Ok(());
        }
        self.patterns = None;
        self.stats.pattern_layers = 0;
        let (texture, view, sampler) =
            crate::renderer::block_textures(device, queue, data, &device.limits())?;
        let group = texture_group(device, &self.layout, &view, &sampler);
        self.stats.pattern_layers = data.layers;
        self.patterns = Some((Arc::clone(data), texture, group));
        Ok(())
    }

    /// Uploads the vertices if the store changed, then picks this frame's draws: the chunks
    /// inside the GPU window (`chunks`) and the frustum of `view`; and sorts the translucent
    /// quads for its eye if it, the drawn chunks or the quads changed.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        store: &mut CellStore,
        chunks: &ChunkTable,
        view: &ViewParams,
    ) {
        if store.dirty {
            store.dirty = false;
            fill(&mut self.lists, store);
            let wrote = self.lists.upload(device, queue);
            self.stats.uploads += u64::from(wrote);
            self.sorted.eye = None;
        }
        self.stats.chunks = store.chunks.len() as u32;
        let count = |l: &QuadList| l.vertices.len() as u32 / 6;
        let [cell, block] =
            [0, 1].map(|k| count(&self.lists.cutout[k]) + count(&self.lists.translucent[k]));
        self.stats.quads = cell + block + count(&self.lists.pattern);
        self.stats.block_quads = block;
        for list in self
            .lists
            .cutout
            .iter_mut()
            .chain([&mut self.lists.pattern])
        {
            list.pick(chunks, view);
        }
        let mut changed = false;
        for list in &mut self.lists.translucent {
            let before = std::mem::take(&mut list.draws);
            list.pick(chunks, view);
            changed |= before != list.draws;
        }
        for list in self.moving.all_mut() {
            list.pick_all();
        }
        let eye = (
            view.render_origin,
            view.eye_rel.to_array().map(f32::to_bits),
        );
        let none = self
            .lists
            .translucent
            .iter()
            .chain(&self.moving.translucent)
            .all(|l| l.vertices.is_empty());
        if none && self.sorted.order.is_empty() {
            self.sorted.runs.clear();
        } else if changed || self.sorted.eye != Some(eye) {
            let [a, b] = &self.lists.translucent;
            let [c, d] = &self.moving.translucent;
            self.sorted.sort([a, b, c, d], view);
            self.sorted.upload(device, queue);
            self.sorted.eye = Some(eye);
            self.stats.sorts += 1;
        }
        self.stats.moving = self.moving_input.len() as u32;
        self.stats.translucent = self.sorted.order.len() as u32;
        let patterns = [&self.lists.pattern, &self.moving.pattern];
        self.stats.patterns = if self.patterns.is_some() {
            patterns.iter().map(|l| l.drawn()).sum()
        } else {
            0
        };
        let cutout = || self.lists.cutout.iter().chain(&self.moving.cutout);
        self.stats.drawn = cutout().map(QuadList::drawn).sum::<u32>()
            + self.stats.translucent
            + self.stats.patterns;
        let cells = if self.textures.is_some() {
            (self.lists.cutout[0].draws.len() + self.moving.cutout[0].draws.len()) as u32
                + self.sorted.runs.iter().filter(|r| r.0 % 2 == 0).count() as u32
        } else {
            0
        };
        let blocks = self.lists.cutout[1].draws.len() + self.moving.cutout[1].draws.len();
        let pattern_draws = if self.patterns.is_some() {
            patterns.iter().map(|l| l.draws.len()).sum()
        } else {
            0
        };
        self.stats.draws = cells
            + (blocks + pattern_draws + self.sorted.runs.iter().filter(|r| r.0 % 2 == 1).count())
                as u32;
    }

    /// Draws this frame's cell quads into `pass` (group 1 already holds the View): the cutout
    /// ones of the cell textures (once the content gave them), then those of the block textures,
    /// then the pattern ones (once the content gave their textures), then the translucent ones
    /// back to front. With `receivers` (High, a frame with sun shadows: the receivers' group 3)
    /// the cutout, translucent and glow quads are drawn with the pipelines that receive them,
    /// if they exist. Leaves group 0 bound to one of the arrays. False if it drew nothing.
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        receivers: Option<&wgpu::BindGroup>,
    ) -> bool {
        let cutout = || self.lists.cutout.iter().chain(&self.moving.cutout);
        let patterns = [&self.lists.pattern, &self.moving.pattern];
        let any_pattern = self.patterns.is_some() && patterns.iter().any(|l| !l.draws.is_empty());
        if cutout().all(|l| l.draws.is_empty()) && !any_pattern && self.sorted.runs.is_empty() {
            return false;
        }
        let shadowed = receivers.zip(self.shadowed.as_ref());
        if let Some((receivers, _)) = shadowed {
            pass.set_bind_group(GROUP_RECEIVE, receivers, &[]);
        }
        let shadowed = shadowed.map(|(_, s)| s);
        let mut drew = false;
        if cutout().any(|l| !l.draws.is_empty()) {
            pass.set_pipeline(shadowed.map_or(&self.pipeline, |s| &s.pipeline));
            if let Some((_, _, group)) = &self.textures {
                drew |= self.lists.cutout[0].draw(pass, group);
                drew |= self.moving.cutout[0].draw(pass, group);
            }
            drew |= self.lists.cutout[1].draw(pass, &self.blocks);
            drew |= self.moving.cutout[1].draw(pass, &self.blocks);
        }
        if let Some((_, _, group)) = &self.patterns
            && any_pattern
        {
            pass.set_pipeline(&self.pattern);
            for list in patterns {
                drew |= list.draw(pass, group);
            }
        }
        drew | self.draw_translucent(pass, shadowed)
    }

    /// The translucent quads in this frame's order, a draw per run of one list, with
    /// `shadowed`'s pipelines if given.
    fn draw_translucent(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        shadowed: Option<&ShadowedCells>,
    ) -> bool {
        let (translucent, glows) = match shadowed {
            Some(s) => (&s.translucent, &s.glow),
            None => (&self.translucent, &self.glow),
        };
        let Some(index) = &self.sorted.index else {
            return false;
        };
        let lists = [
            &self.lists.translucent[0],
            &self.lists.translucent[1],
            &self.moving.translucent[0],
            &self.moving.translucent[1],
        ];
        let mut bound = None;
        let mut pipeline = None;
        let mut drew = false;
        for (id, glow, range) in &self.sorted.runs {
            let group = if id % 2 == 0 {
                match &self.textures {
                    Some((_, _, group)) => group,
                    None => continue,
                }
            } else {
                &self.blocks
            };
            let Some(vertex) = &lists[usize::from(*id)].vertex else {
                continue;
            };
            if !drew {
                pass.set_index_buffer(index.slice(..), wgpu::IndexFormat::Uint32);
            }
            if pipeline != Some(*glow) {
                pass.set_pipeline(if *glow { glows } else { translucent });
                pipeline = Some(*glow);
            }
            if bound != Some(*id) {
                pass.set_bind_group(cell::GROUP_TEXTURES, group, &[]);
                pass.set_vertex_buffer(0, vertex.slice(..));
                bound = Some(*id);
            }
            pass.draw_indexed(range.clone(), 0, 0..1);
            drew = true;
        }
        drew
    }

    pub(crate) fn stats(&self) -> CellStats {
        self.stats
    }
}

/// What a cell pipeline draws into.
#[derive(Clone, Copy)]
enum CellTarget {
    /// The main pass: the color target with `blend`, depth tested (reverse-Z) and written with
    /// `depth`.
    Main {
        blend: Option<wgpu::BlendState>,
        depth: bool,
    },
    /// A shadow map: depth only ([`caster_depth`]).
    Caster,
}

/// A cell pipeline of `layout`: the [`CellVertex`] buffer into `entries.0` of `module`, then
/// its `entries.1`, both sides of every quad (as the terrain's cutout bucket: the sort drops the
/// translucent quads that look away; `vs_cell_pattern` the pattern ones; the casters' vertex
/// stage the quads that do not look away from the sun).
fn cell_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    (vertex, fragment): (&str, Option<&str>),
    target: CellTarget,
) -> wgpu::RenderPipeline {
    let [o0, o1, o2, o3, o4] = cell::ATTRIBUTE_OFFSETS;
    let attribute = |format, offset, shader_location| wgpu::VertexAttribute {
        format,
        offset,
        shader_location,
    };
    let attributes = [
        attribute(wgpu::VertexFormat::Sint32x3, o0, 0),
        attribute(wgpu::VertexFormat::Float32x3, o1, 1),
        attribute(wgpu::VertexFormat::Float32x2, o2, 2),
        attribute(wgpu::VertexFormat::Uint32x2, o3, 3),
        attribute(wgpu::VertexFormat::Uint32, o4, 4),
    ];
    let color;
    let (depth_stencil, targets): (_, &[Option<wgpu::ColorTargetState>]) = match target {
        // Tested and written like the terrain (reverse-Z); the translucent quads too, as
        // Minecraft's translucent items: drawn back to front, the ones behind still show. A
        // glow is only tested.
        CellTarget::Main { blend, depth } => {
            color = [Some(wgpu::ColorTargetState {
                format: MAIN_COLOR_FORMAT,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })];
            (
                wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth),
                    depth_compare: Some(wgpu::CompareFunction::Greater),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                },
                &color,
            )
        }
        CellTarget::Caster => (caster_depth(), &[]),
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(vertex),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<CellVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(depth_stencil),
        multisample: wgpu::MultisampleState::default(),
        fragment: fragment.map(|entry| wgpu::FragmentState {
            module,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_voxel::{CellQuad, Face, LightValue};

    /// A mesh of `cells` quads of cell images then `blocks` of block layers.
    fn mixed(cells: usize, blocks: usize) -> Arc<ChunkMesh> {
        let mut m = (*mesh(cells + blocks)).clone();
        for q in &mut m.cell_quads[cells..] {
            q.quad.source = CellImage::Block;
        }
        Arc::new(m)
    }

    /// Each quad goes to the list of its texture array, each chunk's run of a list recorded
    /// with its origin; a chunk with none in a list has no run there.
    #[test]
    fn quads_are_listed_by_their_texture_array() {
        let mut store = CellStore::default();
        let (a, b) = (I64Vec3::new(0, 0, 0), I64Vec3::new(32, 0, 0));
        store.upsert(1, a, &mixed(2, 3));
        store.upsert(2, b, &mixed(1, 0));
        let mut lists = Lists::default();
        fill(&mut lists, &store);
        let lists = &lists.cutout;
        assert_eq!(lists[0].vertices.len(), 6 * 3);
        assert_eq!(lists[1].vertices.len(), 6 * 3);
        assert_eq!(lists[0].ranges, vec![(a, 0..12), (b, 12..18)]);
        assert_eq!(lists[1].ranges, vec![(a, 0..18)]);
        assert_eq!(list_of(CellImage::Block), 1);
        assert!(lists[0].sort.is_empty());
    }

    /// Moving quads go to the list of their texture array, placed from their cell (wrapped to
    /// i32 as a chunk origin is) with their light, and all of them are drawn.
    #[test]
    fn moving_quads_are_listed_from_their_cell() {
        let quad = mesh(1).cell_quads[0].quad;
        let far = BlockPos::new((1 << 40) + 3, -7, 12);
        let quads = vec![
            (
                MovingCellQuad {
                    cell: BlockPos::new(1, 2, 3),
                    quad,
                },
                LightValue(0x1234),
            ),
            (
                MovingCellQuad {
                    cell: far,
                    quad: CellQuad {
                        source: CellImage::Block,
                        ..quad
                    },
                },
                LightValue::FULL_SKY,
            ),
        ];
        let mut all = Lists::default();
        fill_moving(&mut all, &quads);
        let lists = &mut all.cutout;
        assert_eq!((lists[0].vertices.len(), lists[1].vertices.len()), (6, 6));
        assert!(lists[0].vertices.iter().all(|v| v.origin == [1, 2, 3]));
        let wrapped = wrap_to_i32(I64Vec3::new(far.x, far.y, far.z)).to_array();
        assert!(lists[1].vertices.iter().all(|v| v.origin == wrapped));
        assert_eq!(lists[0].vertices[0].data[0] >> cell::LIGHT_SHIFT, 0x1234);
        assert_eq!(lists[0].vertices[0].pos, quad.corners[0]);
        for list in lists.iter_mut() {
            list.pick_all();
        }
        assert_eq!(lists[0].draws, vec![0..6]);
        assert_eq!(lists[0].drawn() + lists[1].drawn(), 2);
        fill_moving(&mut all, &[]);
        all.cutout[0].pick_all();
        assert!(all.cutout[0].vertices.is_empty() && all.cutout[0].draws.is_empty());
    }

    fn mesh(quads: usize) -> Arc<ChunkMesh> {
        let quad = CellQuad {
            corners: [[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
            uv: [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            image: 5,
            source: CellImage::Cell,
            face: Face::NegZ,
            full_bright: true,
            tint: [1, 2, 3],
            blend: CellBlend::Cutout,
        };
        Arc::new(ChunkMesh {
            cell_quads: vec![
                LitCellQuad {
                    quad,
                    light: LightValue(0xABCD),
                };
                quads
            ],
            ..ChunkMesh::default()
        })
    }

    fn view_at(eye: boxel_math::DVec3) -> ViewParams {
        let camera = boxel_camera::Camera::default();
        let transform = boxel_transform::Transform::from_yaw_pitch(eye, 0.0, 0.0);
        let cv = boxel_camera::CameraView::compute(&camera, &transform, 16.0 / 9.0);
        ViewParams::from_camera(&cv, &camera, [0.0; 4], crate::DistanceFog::default())
    }

    /// A translucent quad across Z at depth `z` of cell (0, 0, 0), shaded as `face`.
    fn pane(z: f32, face: Face) -> MovingCellQuad {
        let quad = mesh(1).cell_quads[0].quad;
        MovingCellQuad {
            cell: BlockPos::new(0, 0, 0),
            quad: CellQuad {
                corners: quad.corners.map(|[x, y, _]| [x, y, z]),
                face,
                ..quad
            }
            .with_blend(CellBlend::Translucent(128)),
        }
    }

    /// Translucent quads go to the translucent lists with their opacity packed, and sort back to
    /// front by the distance to their nearest point; one whose face looks away is not drawn,
    /// nor one seen from behind its face; a plane shaded along it shows from both sides.
    #[test]
    fn translucent_quads_sort_back_to_front_facing_the_eye() {
        let mut glow = pane(0.1, Face::NegZ);
        glow.quad.blend = CellBlend::Glow(32);
        let quads: Vec<_> = [
            pane(0.2, Face::NegZ),
            pane(0.8, Face::NegZ),
            pane(0.5, Face::PosZ),
            pane(0.6, Face::PosY),
            glow,
        ]
        .into_iter()
        .map(|q| (q, LightValue::FULL_SKY))
        .collect();
        let mut sort = SortBench::new(&quads);
        let lists = &sort.lists;
        assert!(lists.cutout.iter().all(|l| l.vertices.is_empty()));
        assert_eq!(lists.translucent[0].sort.len(), 5);
        assert_eq!(
            lists.translucent[0].vertices[24].data[1] >> cell::OPACITY_SHIFT,
            32
        );
        let v = lists.translucent[0].vertices[0];
        assert_eq!(v.data[1] >> cell::OPACITY_SHIFT, 128);
        assert_eq!(v.data[1] & 0xFF_FFFF, 0x01_02_03);
        // The eye in front (-Z side): the PosZ one looks away; far first, the glow nearest, in a
        // run of its own (its pipeline writes no depth).
        assert_eq!(
            sort.sort(&view_at(boxel_math::DVec3::new(0.5, 0.5, -3.0))),
            24
        );
        let order: Vec<u32> = sort.sorted.order.iter().map(|o| o.2).collect();
        assert_eq!(order, vec![1, 3, 0, 4]);
        assert_eq!(sort.sorted.runs, vec![(2, false, 0..18), (2, true, 18..24)]);
        assert_eq!(sort.sorted.indices[..6], [6, 7, 8, 9, 10, 11]);
        // From behind (+Z side): only the PosZ one and the plane, nearest last.
        sort.sort(&view_at(boxel_math::DVec3::new(0.5, 0.5, 4.0)));
        let order: Vec<u32> = sort.sorted.order.iter().map(|o| o.2).collect();
        assert_eq!(order, vec![2, 3]);
        // A quad seen past its edge: its nearest point is on that edge.
        let q = SortQuad::new(I64Vec3::ZERO, &pane(0.0, Face::NegZ).quad);
        assert_eq!(q.distance(Vec3::new(3.0, 0.5, -4.0)), Some(4.0 + 16.0));
        assert_eq!(q.distance(Vec3::new(3.0, 0.5, 4.0)), None);
    }

    /// Pattern quads (ADR 0056) go to the pattern list whatever their blend, with their layer
    /// count (at most 16) where a translucent quad keeps its opacity, and no sort entry.
    #[test]
    fn pattern_quads_have_a_list_of_their_own() {
        let quad = mesh(1).cell_quads[0].quad;
        let pattern = |layers, blend| MovingCellQuad {
            cell: BlockPos::new(0, 0, 0),
            quad: CellQuad {
                source: CellImage::Pattern(layers),
                ..quad
            }
            .with_blend(blend),
        };
        let quads: Vec<_> = [
            pattern(15, CellBlend::Cutout),
            pattern(200, CellBlend::Translucent(9)),
        ]
        .into_iter()
        .map(|q| (q, LightValue::FULL_SKY))
        .collect();
        let mut lists = Lists::default();
        fill_moving(&mut lists, &quads);
        assert!(
            lists
                .cutout
                .iter()
                .chain(&lists.translucent)
                .all(|l| l.vertices.is_empty())
        );
        let v = &lists.pattern.vertices;
        assert_eq!(v.len(), 12);
        assert!(lists.pattern.sort.is_empty());
        assert_eq!(v[0].data[1] >> cell::OPACITY_SHIFT, 15);
        assert_eq!(v[6].data[1] >> cell::OPACITY_SHIFT, 16);
        assert_eq!(v[0].data[1] & 0xFF_FFFF, 0x01_02_03);
        assert_eq!(v[0].data[0] & cell::LAYER_MASK, 5);
    }

    #[test]
    fn a_quad_is_two_triangles_with_its_data_packed() {
        let m = mesh(1);
        let v = quad_vertices([1, -2, 3], &m.cell_quads[0], 0x123);
        assert_eq!(
            v.map(|v| v.pos),
            TRIANGLES.map(|k| m.cell_quads[0].quad.corners[k])
        );
        assert!(v.iter().all(|v| v.origin == [1, -2, 3] && v.sky == 0x123));
        let face = Face::NegZ as u32;
        assert_eq!(
            v[0].data,
            [5 | face << 12 | 0x8000 | 0xABCD << 16, 0x01_02_03]
        );
    }

    /// ADR 0064: a cell quad takes its sky's channels from the mesh, or its light's sky level
    /// three times where the mesh has none (no tint, or a moving quad).
    #[test]
    fn cell_quads_take_their_sky_from_the_mesh() {
        let sky = |lists: &Lists| -> Vec<u32> {
            lists
                .all()
                .flat_map(|l| l.vertices.iter().map(|v| v.sky))
                .collect()
        };
        let mut store = CellStore::default();
        store.upsert(1, I64Vec3::ZERO, &mesh(2));
        let mut lists = Lists::default();
        fill(&mut lists, &store);
        assert_eq!(
            sky(&lists),
            [0xAAA; 12],
            "no tint: sky 10 (0xABCD) three times"
        );
        let mut tinted = (*mesh(2)).clone();
        tinted.cell_sky_rgb = vec![0xA76, 0x5AA];
        store.upsert(1, I64Vec3::ZERO, &Arc::new(tinted));
        fill(&mut lists, &store);
        let want: Vec<u32> = [0xA76; 6].into_iter().chain([0x5AA; 6]).collect();
        assert_eq!(sky(&lists), want);
        let quad = mesh(1).cell_quads[0].quad;
        let moving = MovingCellQuad {
            cell: BlockPos::new(0, 0, 0),
            quad,
        };
        fill_moving(&mut lists, &[(moving, LightValue(0x7123))]);
        assert_eq!(sky(&lists), [0x777; 6]);
    }

    #[test]
    fn the_store_keeps_only_meshes_with_cell_quads_and_marks_changes() {
        let mut store = CellStore::default();
        let origin = I64Vec3::new(32, 0, -64);
        store.upsert(1, origin, &Arc::new(ChunkMesh::default()));
        assert!(store.is_empty() && !store.dirty);
        let m = mesh(2);
        store.upsert(1, origin, &m);
        assert!(!store.is_empty() && store.dirty);
        store.dirty = false;
        // The same mesh again (or an equal one) changes nothing.
        store.upsert(1, origin, &m);
        store.upsert(1, origin, &mesh(2));
        assert!(!store.dirty);
        store.upsert(1, origin, &mesh(3));
        assert!(store.dirty);
        store.dirty = false;
        store.upsert(1, origin, &Arc::new(ChunkMesh::default()));
        assert!(store.is_empty() && store.dirty);
        store.upsert(2, origin, &m);
        store.dirty = false;
        store.remove(3);
        assert!(!store.dirty);
        store.clear();
        assert!(store.is_empty() && store.dirty);
    }
}
