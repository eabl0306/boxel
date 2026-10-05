use crate::{
    BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, PADDED, PackedQuad, PaddedChunk, TRANSLUCENT_BUCKET,
    VoxelClass,
};
use binary_greedy_meshing as bgm;
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, Face, LocalPos,
    NEIGHBOR_OFFSETS, RenderShape, neighbor_index,
};
use std::collections::BTreeSet;
use std::ops::Range;

/// Mesher-side view of the registry: per material id, bgm class, render shape (bucket routing)
/// and the texture layer of each face. Built once per registry and shared (`Arc`) by mesh jobs.
#[derive(Clone, Debug, Default)]
pub struct MaterialTable {
    class: Vec<VoxelClass>,
    shape: Vec<RenderShape>,
    layers: Vec<[u16; 6]>,
}

impl MaterialTable {
    /// From the registry (material id = `BlockRegistry::material`).
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let n = registry.len();
        let mut class = vec![VoxelClass::Empty; n];
        let mut shape = vec![RenderShape::None; n];
        let mut layers = vec![[0u16; 6]; n];
        for i in 0..n {
            let s = BlockState(i as u16);
            let m = registry.material(s) as usize;
            if m == 0 {
                continue;
            }
            shape[m] = registry.shape(s);
            class[m] = match shape[m] {
                RenderShape::None => VoxelClass::Empty,
                RenderShape::Opaque => VoxelClass::Opaque,
                RenderShape::Cutout | RenderShape::Translucent => VoxelClass::Transparent,
            };
            layers[m] = Face::ALL.map(|f| registry.face_texture(s, f).0);
        }
        Self {
            class,
            shape,
            layers,
        }
    }
    /// Class of a material (out of range -> `Empty`).
    pub fn class(&self, material: u16) -> VoxelClass {
        self.class
            .get(material as usize)
            .copied()
            .unwrap_or(VoxelClass::Empty)
    }
    /// Render shape of a material (out of range -> `None`).
    pub fn shape(&self, material: u16) -> RenderShape {
        self.shape
            .get(material as usize)
            .copied()
            .unwrap_or(RenderShape::None)
    }
    /// Bucket of a quad of `material` facing `face`: `Opaque` -> `face.index()`, `Cutout` ->
    /// [`CUTOUT_BUCKET`], `Translucent` -> [`TRANSLUCENT_BUCKET`] (`None` never produces quads).
    pub fn bucket(&self, material: u16, face: Face) -> usize {
        match self.shape(material) {
            RenderShape::Cutout => CUTOUT_BUCKET,
            RenderShape::Translucent => TRANSLUCENT_BUCKET,
            RenderShape::Opaque | RenderShape::None => face.index(),
        }
    }
    /// Texture layer of `face`.
    pub fn layer(&self, material: u16, face: Face) -> u16 {
        self.layers
            .get(material as usize)
            .map_or(0, |l| l[face.index()])
    }
    /// Test helper: explicit table (`shape[i]` and `layers[i]` for material `i`; the bgm
    /// class is derived from the shape exactly as in [`MaterialTable::from_registry`]).
    pub fn from_parts(shape: Vec<RenderShape>, layers: Vec<[u16; 6]>) -> Self {
        assert!(shape.len() == layers.len());
        let class = shape
            .iter()
            .map(|s| match s {
                RenderShape::None => VoxelClass::Empty,
                RenderShape::Opaque => VoxelClass::Opaque,
                RenderShape::Cutout | RenderShape::Translucent => VoxelClass::Transparent,
            })
            .collect();
        Self {
            class,
            shape,
            layers,
        }
    }
    /// Mesher voxel value and class of a block state. M0 identity map (as
    /// `BlockRegistry::material`): the table is indexed by material id, and the material id of
    /// a state with geometry is the state id itself; states without geometry (and ids outside
    /// the table) become `0` / `Empty`.
    #[inline]
    fn resolve(&self, state: BlockState) -> (u16, VoxelClass) {
        match self.class(state.0) {
            VoxelClass::Empty => (0, VoxelClass::Empty),
            class => (state.0, class),
        }
    }
}

/// Center chunk plus its 26 neighbors in `boxel_voxel::NEIGHBOR_OFFSETS` order. A `None`
/// neighbor is treated as empty (air).
pub struct MeshInput<'a> {
    /// Chunk to mesh.
    pub center: &'a ChunkData,
    /// Neighbors.
    pub neighbors: [Option<&'a ChunkData>; 26],
}

/// Reusable mesher (one per worker thread; ~210 KiB of scratch plus the bgm quad lists).
pub struct Mesher {
    inner: bgm::Mesher<32>,
    padded: PaddedChunk,
    /// `ChunkData::to_dense` target for non-uniform center chunks (`CHUNK_VOLUME` states).
    dense: Box<[BlockState]>,
}

impl Default for Mesher {
    fn default() -> Self {
        Self::new()
    }
}

impl Mesher {
    /// New mesher.
    pub fn new() -> Self {
        Self {
            inner: bgm::Mesher::new(),
            padded: PaddedChunk::new(),
            dense: vec![BlockState::AIR; CHUNK_VOLUME].into_boxed_slice(),
        }
    }

    /// Fills `out` from the center chunk and the border voxels of its neighbors (face
    /// neighbors: 32x32 slabs, edges: 32 rows, corners: 1 voxel). Uniform chunks take a fast path.
    pub fn gather(
        &mut self,
        input: &MeshInput<'_>,
        materials: &MaterialTable,
        out: &mut PaddedChunk,
    ) {
        gather_into(&mut self.dense, input, materials, out);
    }

    /// Meshes a padded chunk: bgm `fast_mesh`, then converts every bgm quad to the canonical
    /// [`crate::PackedQuad`] (via `Face::vertices_packed` min/max; positive faces subtract 1 on
    /// the normal axis), assigns `MaterialTable::layer`, routes each quad to
    /// `MaterialTable::bucket` and writes `out` bucket-contiguous.
    pub fn mesh(&mut self, padded: &PaddedChunk, materials: &MaterialTable, out: &mut ChunkMesh) {
        mesh_into(&mut self.inner, padded, materials, out);
    }

    /// `gather` + `mesh`. Fast paths: center uniform air -> empty mesh; center uniform opaque with
    /// all 6 face neighbors uniform opaque -> empty mesh.
    pub fn mesh_chunk(&mut self, input: &MeshInput<'_>, materials: &MaterialTable) -> ChunkMesh {
        let mut out = ChunkMesh::default();
        if trivially_empty(input, materials) {
            return out;
        }
        gather_into(&mut self.dense, input, materials, &mut self.padded);
        mesh_into(&mut self.inner, &self.padded, materials, &mut out);
        out
    }
}

/// True when the mesh is empty without looking at any voxel: the center is uniform air, or it
/// is uniform opaque and all six face neighbors are uniform opaque too.
fn trivially_empty(input: &MeshInput<'_>, materials: &MaterialTable) -> bool {
    let uniform_class =
        |chunk: &ChunkData| chunk.as_uniform().map(|state| materials.resolve(state).1);
    match uniform_class(input.center) {
        Some(VoxelClass::Empty) => true,
        Some(VoxelClass::Opaque) => Face::ALL.iter().all(|face| {
            neighbor_index(face.normal())
                .and_then(|i| input.neighbors[i])
                .and_then(uniform_class)
                == Some(VoxelClass::Opaque)
        }),
        Some(VoxelClass::Transparent) | None => false,
    }
}

/// [`Mesher::gather`] body. Borrows only the dense scratch, so `mesh_chunk` can gather into the
/// mesher's own padded buffer.
fn gather_into(
    dense: &mut [BlockState],
    input: &MeshInput<'_>,
    materials: &MaterialTable,
    out: &mut PaddedChunk,
) {
    out.clear();
    gather_center(dense, input.center, materials, out);
    for (offset, neighbor) in NEIGHBOR_OFFSETS.iter().zip(input.neighbors) {
        if let Some(chunk) = neighbor {
            gather_border([offset.x, offset.y, offset.z], chunk, materials, out);
        }
    }
}

/// Copies the inner 32^3 voxels. A uniform chunk fills whole rows; any other is decoded once
/// with `ChunkData::to_dense` and copied row by row (the local index `(y << 10) | (x << 5) | z`
/// and the padded layout both run along z, so dense row `y * 32 + x` is one padded column).
fn gather_center(
    dense: &mut [BlockState],
    center: &ChunkData,
    materials: &MaterialTable,
    out: &mut PaddedChunk,
) {
    const INNER: Range<i32> = 0..CHUNK_SIZE;
    if let Some(state) = center.as_uniform() {
        let (material, class) = materials.resolve(state);
        fill_region(out, [INNER, INNER, INNER], material, class);
        return;
    }
    center.to_dense(dense);
    let row_len = CHUNK_SIZE as usize;
    for (r, row) in dense.chunks_exact(row_len).enumerate() {
        let (x, y) = ((r % row_len) as i32, (r / row_len) as i32);
        write_run(out, materials, x, y, 0, row.iter().copied());
    }
}

/// Copies the part of the neighbor at `offset` (components in -1..=1) that lies in the padding:
/// a 32x32 slab for a face neighbor, a row of 32 for an edge neighbor, one voxel for a corner.
fn gather_border(
    offset: [i32; 3],
    chunk: &ChunkData,
    materials: &MaterialTable,
    out: &mut PaddedChunk,
) {
    let spans = offset.map(border_span);
    if let Some(state) = chunk.as_uniform() {
        let (material, class) = materials.resolve(state);
        fill_region(out, spans.map(|(range, _)| range), material, class);
        return;
    }
    let [(xs, sx), (ys, sy), (zs, sz)] = spans;
    for y in ys {
        for x in xs.clone() {
            let (lx, ly) = ((x + sx) as u32, (y + sy) as u32);
            let states = zs
                .clone()
                .map(|z| chunk.get(LocalPos::new(lx, ly, (z + sz) as u32)));
            write_run(out, materials, x, y, zs.start, states);
        }
    }
}

/// Writes a run of states along z, starting at padded `(x, y, z0)`, into a clear `out`: material
/// ids through the table, mask bits gathered per run. Consecutive equal states (common along
/// terrain columns) reuse the previous table lookup.
#[inline]
fn write_run(
    out: &mut PaddedChunk,
    materials: &MaterialTable,
    x: i32,
    y: i32,
    z0: i32,
    states: impl ExactSizeIterator<Item = BlockState>,
) {
    let start = PaddedChunk::index(x, y, z0);
    let dst = &mut out.voxels[start..start + states.len()];
    let (mut opaque, mut transparent) = (0u64, 0u64);
    let mut last = BlockState(u16::MAX);
    let mut resolved = materials.resolve(last);
    for (z, (slot, state)) in dst.iter_mut().zip(states).enumerate() {
        if state != last {
            last = state;
            resolved = materials.resolve(state);
        }
        let (material, class) = resolved;
        *slot = material;
        opaque |= u64::from(class == VoxelClass::Opaque) << z;
        transparent |= u64::from(class == VoxelClass::Transparent) << z;
    }
    // Mask bit of padded z is z + 1.
    let row = mask_row(x, y);
    out.opaque[row] |= opaque << (z0 + 1);
    out.transparent[row] |= transparent << (z0 + 1);
}

/// Padded-space range covered by a neighbor at `d` (-1, 0 or 1) along one axis, and the shift
/// that turns a padded coordinate of that range into the neighbor's local coordinate.
fn border_span(d: i32) -> (Range<i32>, i32) {
    match d {
        -1 => (-1..0, CHUNK_SIZE),
        0 => (0..CHUNK_SIZE, 0),
        _ => (CHUNK_SIZE..CHUNK_SIZE + 1, -CHUNK_SIZE),
    }
}

/// Fills a padded-space box (`[xs, ys, zs]`, each within -1..=32, non-empty) with one material.
/// `out` must be clear inside the box (`gather` clears first), so `Empty` writes nothing.
fn fill_region(
    out: &mut PaddedChunk,
    [xs, ys, zs]: [Range<i32>; 3],
    material: u16,
    class: VoxelClass,
) {
    let masks = match class {
        VoxelClass::Empty => return,
        VoxelClass::Opaque => &mut out.opaque,
        VoxelClass::Transparent => &mut out.transparent,
    };
    let len = zs.len();
    // Mask bit of padded z is z + 1.
    let bits = (u64::MAX >> (64 - len)) << (zs.start + 1);
    for y in ys {
        for x in xs.clone() {
            masks[mask_row(x, y)] |= bits;
            let start = PaddedChunk::index(x, y, zs.start);
            out.voxels[start..start + len].fill(material);
        }
    }
}

/// Column-mask row of padded `(x, y)` (each in -1..=32).
#[inline]
const fn mask_row(x: i32, y: i32) -> usize {
    (x + 1) as usize + (y + 1) as usize * PADDED
}

/// [`Mesher::mesh`] body. Borrows only the bgm mesher.
fn mesh_into(
    inner: &mut bgm::Mesher<32>,
    padded: &PaddedChunk,
    materials: &MaterialTable,
    out: &mut ChunkMesh,
) {
    inner.clear();
    inner.fast_mesh(&padded.voxels, &padded.opaque, &padded.transparent);
    out.clear();

    // Pass 1: bucket sizes, so each bucket is written in place (no second buffer).
    let mut counts = [0u32; BUCKET_COUNT];
    for (face, quads) in Face::ALL.into_iter().zip(&inner.quads) {
        for quad in quads {
            counts[materials.bucket(quad.voxel_id() as u16, face)] += 1;
        }
    }
    // Bounds: an opaque face bucket holds at most `MAX_QUADS_PER_FACE_BUCKET`. The cutout and
    // translucent buckets hold at most `MAX_QUADS_PER_BUCKET` while each has a single material
    // (the default registry), but up to 6 * 32^3 when two materials of one bucket alternate
    // (every face between them is visible). The output is never truncated.
    let mut cursor = [0usize; BUCKET_COUNT];
    let mut total = 0usize;
    for (start, count) in cursor.iter_mut().zip(counts) {
        *start = total;
        total += count as usize;
    }

    // Pass 2: convert and scatter.
    out.quads.resize(total, PackedQuad::default());
    for (face, quads) in Face::ALL.into_iter().zip(&inner.quads) {
        let bgm_face = bgm::Face::from(face.index() as u8);
        for &quad in quads {
            let material = quad.voxel_id() as u16;
            let bucket = materials.bucket(material, face);
            out.quads[cursor[bucket]] =
                to_packed(bgm_face, quad, face, materials.layer(material, face));
            cursor[bucket] += 1;
        }
    }
    out.bucket_counts = counts;
}

/// Converts one bgm quad to the canonical [`PackedQuad`]: the min corner of its 4 vertices is
/// the position, the extents along the face's canonical u/v axes are `w`/`h`, and positive faces
/// (which bgm puts on the `voxel + 1` plane) move back by one on the normal axis.
#[inline]
fn to_packed(bgm_face: bgm::Face, quad: bgm::Quad, face: Face, layer: u16) -> PackedQuad {
    let mut min = [u32::MAX; 3];
    let mut max = [0u32; 3];
    for vertex in bgm_face.vertices_packed(quad) {
        for (axis, c) in vertex.xyz().into_iter().enumerate() {
            min[axis] = min[axis].min(c);
            max[axis] = max[axis].max(c);
        }
    }
    let (n, u, v) = face.axes();
    debug_assert_eq!(min[n], max[n], "bgm quad is not planar");
    let mut pos = min;
    if face.index().is_multiple_of(2) {
        pos[n] -= 1;
    }
    PackedQuad::new(pos, max[u] - min[u], max[v] - min[v], face, layer)
}

/// Brute-force oracle: every visible unit face `(voxel, face)` of the inner 32^3 (a face is
/// visible if the voxel is non-empty and the neighbor is empty, or transparent with a different
/// material). The mesher's output, expanded with `PackedQuad::unit_faces`, must equal this set.
pub fn reference_visible_faces(padded: &PaddedChunk) -> BTreeSet<([u32; 3], Face)> {
    // Class from the column masks (what bgm reads), material from the voxel ids.
    let class = |x: i32, y: i32, z: i32| {
        let row = mask_row(x, y);
        let bit = 1u64 << (z + 1);
        if padded.opaque[row] & bit != 0 {
            VoxelClass::Opaque
        } else if padded.transparent[row] & bit != 0 {
            VoxelClass::Transparent
        } else {
            VoxelClass::Empty
        }
    };
    let mut faces = BTreeSet::new();
    for y in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                if class(x, y, z) == VoxelClass::Empty {
                    continue;
                }
                let material = padded.get(x, y, z);
                for face in Face::ALL {
                    let n = face.normal();
                    let (nx, ny, nz) = (x + n.x, y + n.y, z + n.z);
                    let visible = match class(nx, ny, nz) {
                        VoxelClass::Empty => true,
                        VoxelClass::Transparent => padded.get(nx, ny, nz) != material,
                        VoxelClass::Opaque => false,
                    };
                    if visible {
                        faces.insert(([x as u32, y as u32, z as u32], face));
                    }
                }
            }
        }
    }
    faces
}
