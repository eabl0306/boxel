use crate::greedy::Greedy;
use crate::packed::{LightQuantization, layout, palette_entry};
use crate::shading::{
    AO_OPEN_BYTE, AO_TABLE, FaceKey, FaceShading, KeyTable, LightPalette, PaletteFull, corner_ao,
    face_corner_lights, neighborhood8,
};
use crate::{
    BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, PADDED, PADDED_AREA, PADDED_VOLUME, PackedQuad,
    PaddedChunk, TRANSLUCENT_BUCKET, VoxelClass,
};
use boxel_light::PaddedLight;
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, Face, LightValue, LocalPos,
    NEIGHBOR_OFFSETS, RenderShape, neighbor_index,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// Which faces get ambient occlusion (ADR 0015). AO is part of the greedy-merge key, so every
/// AO value baked splits merges: it costs quads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AoPolicy {
    /// No AO: every face is open, quads merge as by material alone.
    Off,
    /// AO on every face.
    Baked,
    /// AO on every face with some light at one of its corners. A face whose 4 corner lights are
    /// all 0 keeps AO open and merges as with `Off`: almost all cave geometry is dark, and AO
    /// there is invisible at the default ambient floor. To revisit when held lights arrive.
    #[default]
    BakedLitOnly,
}

/// Mesher knobs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MeshOptions {
    /// Ambient occlusion policy.
    pub ao: AoPolicy,
}

impl MeshOptions {
    /// Options with this AO policy.
    pub const fn with_ao(ao: AoPolicy) -> Self {
        Self { ao }
    }
}

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

/// Center chunk plus its 26 neighbors in `boxel_voxel::NEIGHBOR_OFFSETS` order, and the light of
/// the padded chunk. A `None` neighbor is treated as empty (air).
pub struct MeshInput<'a> {
    /// Chunk to mesh.
    pub center: &'a ChunkData,
    /// Neighbors.
    pub neighbors: [Option<&'a ChunkData>; 26],
    /// Light of the chunk and its 1-voxel border (`boxel_light::compute`). `None`: open sky
    /// (sky 15) everywhere.
    pub light: Option<&'a PaddedLight>,
}

/// Mesh scratch other than the padded input (borrowed apart from it by `mesh_chunk`).
struct Scratch {
    greedy: Greedy,
    /// Merge key per padded voxel, written per face at that face's visible voxels only.
    keys: Box<[u32]>,
    palette: LightPalette,
    key_table: KeyTable,
}

/// Reusable mesher (one per worker thread; about 0.6 MiB of scratch plus the quad lists).
pub struct Mesher {
    scratch: Scratch,
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
            scratch: Scratch {
                greedy: Greedy::new(),
                keys: vec![0; PADDED_VOLUME].into_boxed_slice(),
                palette: LightPalette::new(),
                key_table: KeyTable::new(),
            },
            padded: PaddedChunk::new(),
            dense: vec![BlockState::AIR; CHUNK_VOLUME].into_boxed_slice(),
        }
    }

    /// Fills `out` from the center chunk and the border voxels of its neighbors (face
    /// neighbors: 32x32 slabs, edges: 32 rows, corners: 1 voxel). Uniform chunks take a fast path.
    /// `input.light` is not read.
    pub fn gather(
        &mut self,
        input: &MeshInput<'_>,
        materials: &MaterialTable,
        out: &mut PaddedChunk,
    ) {
        gather_into(&mut self.dense, input, materials, out);
    }

    /// Meshes a padded chunk lit by `light` (`None`: open sky everywhere) and writes `out`
    /// bucket-contiguous with its light palette and hash.
    ///
    /// Face culling and greedy merging are binary greedy meshing (an in-repo fork, `greedy.rs`).
    /// Two unit faces merge only if they have the same material, the same AO byte and the same 4
    /// corner lights ([`reference_face_shading`] defines both, per `options.ao`). Each quad gets
    /// `MaterialTable::layer` of its material and face and is routed to `MaterialTable::bucket`.
    /// Corner lights go to the palette in first-seen order; past 4096 distinct entries the chunk
    /// is meshed again at the next [`LightQuantization`] level.
    pub fn mesh(
        &mut self,
        padded: &PaddedChunk,
        light: Option<&PaddedLight>,
        materials: &MaterialTable,
        options: MeshOptions,
        out: &mut ChunkMesh,
    ) {
        mesh_into(&mut self.scratch, padded, light, materials, options, out);
    }

    /// `gather` + `mesh`. Fast paths: center uniform air -> empty mesh; center uniform opaque with
    /// all 6 face neighbors uniform opaque -> empty mesh.
    pub fn mesh_chunk(
        &mut self,
        input: &MeshInput<'_>,
        materials: &MaterialTable,
        options: MeshOptions,
    ) -> ChunkMesh {
        let mut out = ChunkMesh::default();
        if trivially_empty(input, materials) {
            out.hash = out.compute_hash();
            return out;
        }
        gather_into(&mut self.dense, input, materials, &mut self.padded);
        mesh_into(
            &mut self.scratch,
            &self.padded,
            input.light,
            materials,
            options,
            &mut out,
        );
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

/// How the merge keys of a mesh decode back to a [`FaceKey`].
#[derive(Clone, Copy, Debug)]
enum KeyKind {
    /// The key is the material (AO open, palette index 0).
    Material,
    /// `material << 8 | ao` (palette index 0).
    MaterialAo,
    /// A [`KeyTable`] id.
    Table,
}

/// [`Mesher::mesh`] body. Borrows only the scratch, so `mesh_chunk` can pass the mesher's own
/// padded buffer.
fn mesh_into(
    s: &mut Scratch,
    padded: &PaddedChunk,
    light: Option<&PaddedLight>,
    materials: &MaterialTable,
    options: MeshOptions,
    out: &mut ChunkMesh,
) {
    out.clear();
    s.greedy
        .cull(&padded.voxels, &padded.opaque, &padded.transparent);
    let elide = options.ao == AoPolicy::BakedLitOnly;
    let full = match light {
        None => Err(LightValue::FULL_SKY),
        Some(PaddedLight::Uniform(v)) => Err(*v),
        Some(PaddedLight::Full(values)) => Ok(&**values),
    };
    let kind = match full {
        // Uniform light: one palette entry, the key is the material and maybe the AO.
        Err(v) => {
            s.greedy.clear();
            let ao = match options.ao {
                AoPolicy::Off => false,
                AoPolicy::Baked => true,
                AoPolicy::BakedLitOnly => !v.is_dark(),
            };
            for face in 0..6 {
                if ao {
                    let keys = &mut s.keys;
                    // Never fails: no palette here.
                    let _ = visit_face(face, &s.greedy, padded, |i, _, nb9| {
                        let ao = AO_TABLE[neighborhood8(nb9)];
                        keys[i] = u32::from(padded.voxels[i]) << 8 | u32::from(ao);
                        Ok(())
                    });
                    s.greedy.merge_face(face, &s.keys);
                } else {
                    s.greedy.merge_face(face, &padded.voxels);
                }
            }
            if s.greedy.quads.iter().any(|q| !q.is_empty()) {
                out.palette.push(palette_entry([v; 4]));
            }
            if ao {
                KeyKind::MaterialAo
            } else {
                KeyKind::Material
            }
        }
        Ok(values) => {
            // `Flat` always fits, so some level succeeds.
            for level in LightQuantization::ALL {
                s.greedy.clear();
                s.palette.clear();
                s.key_table.clear();
                if mesh_lit(s, padded, values, options.ao, elide, level).is_ok() {
                    out.light_quantization = level;
                    break;
                }
            }
            out.palette.extend_from_slice(s.palette.entries());
            KeyKind::Table
        }
    };
    match kind {
        KeyKind::Material => assemble(&s.greedy, materials, out, |k| FaceKey {
            material: k as u16,
            ao: AO_OPEN_BYTE,
            light: 0,
        }),
        KeyKind::MaterialAo => assemble(&s.greedy, materials, out, |k| FaceKey {
            material: (k >> 8) as u16,
            ao: k as u8,
            light: 0,
        }),
        KeyKind::Table => assemble(&s.greedy, materials, out, |k| s.key_table.key(k)),
    }
    out.hash = out.compute_hash();
}

/// Fills the keys and merges every face under full light at quantization `level`. `Err` when
/// the palette overflows (the caller tries the next level).
fn mesh_lit(
    s: &mut Scratch,
    padded: &PaddedChunk,
    light: &[u16; PADDED_VOLUME],
    ao: AoPolicy,
    elide: bool,
    level: LightQuantization,
) -> Result<(), PaletteFull> {
    for (face, &(su, sv)) in LIGHT_STRIDES.iter().enumerate() {
        let (keys, palette, table) = (&mut s.keys, &mut s.palette, &mut s.key_table);
        visit_face(face, &s.greedy, padded, |i, front, nb9| {
            let ao = match ao {
                AoPolicy::Off => AO_OPEN_BYTE,
                AoPolicy::Baked | AoPolicy::BakedLitOnly => AO_TABLE[neighborhood8(nb9)],
            };
            // Cell (du, dv) in -1..=1 is `cells[(dv + 1) * 3 + (du + 1)]`.
            let corner = front - sv - su;
            let cells: [u16; 9] =
                std::array::from_fn(|c| light[corner + (c / 3) * sv + (c % 3) * su]);
            // Equal cells (open sky, dark caves): every corner is that value.
            let corners = level.apply(if cells.iter().all(|&c| c == cells[4]) {
                [LightValue(cells[4]); 4]
            } else {
                face_corner_lights(&cells, nb9)
            });
            let ao = if elide && corners.iter().all(|c| c.is_dark()) {
                AO_OPEN_BYTE
            } else {
                ao
            };
            keys[i] = table.id(padded.voxels[i], ao, corners, palette)?;
            Ok(())
        })?;
        s.greedy.merge_face(face, &s.keys);
    }
    Ok(())
}

/// Padded-index strides along the u and v axes of each face ([`Face::axes`]: ±Y -> x, z;
/// ±X -> z, y; ±Z -> x, y).
const LIGHT_STRIDES: [(usize, usize); 6] = [
    (PADDED, 1),
    (PADDED, 1),
    (1, PADDED_AREA),
    (1, PADDED_AREA),
    (PADDED, PADDED_AREA),
    (PADDED, PADDED_AREA),
];

/// Calls `visit(voxel index, front index, nb9)` for every visible unit face of `face` (after
/// `Greedy::cull`): the padded indices of the voxel and of the voxel in front of the face, and
/// the opacity (`PaddedChunk::opaque`) of the 3x3 front-layer neighborhood (bit
/// `(dv + 1) * 3 + (du + 1)`).
#[inline]
fn visit_face(
    face: usize,
    greedy: &Greedy,
    padded: &PaddedChunk,
    visit: impl FnMut(usize, usize, u32) -> Result<(), PaletteFull>,
) -> Result<(), PaletteFull> {
    let rows = greedy.face_rows(face);
    let opaque = &padded.opaque;
    match face {
        0 => visit_rows::<0>(rows, opaque, visit),
        1 => visit_rows::<1>(rows, opaque, visit),
        2 => visit_rows::<2>(rows, opaque, visit),
        3 => visit_rows::<3>(rows, opaque, visit),
        4 => visit_rows::<4>(rows, opaque, visit),
        _ => visit_rows::<5>(rows, opaque, visit),
    }
}

/// [`visit_face`] for face `F`.
#[inline]
fn visit_rows<const F: usize>(
    rows: &[u64],
    opaque: &[u64],
    mut visit: impl FnMut(usize, usize, u32) -> Result<(), PaletteFull>,
) -> Result<(), PaletteFull> {
    // Padded index offset of the voxel in front.
    let normal: isize = match F {
        0 => PADDED_AREA as isize,
        1 => -(PADDED_AREA as isize),
        2 => PADDED as isize,
        3 => -(PADDED as isize),
        4 => 1,
        _ => -1,
    };
    for (r, &bits) in rows.iter().enumerate() {
        if bits == 0 {
            continue;
        }
        // Unpadded x and y of the row (see `Greedy::face_rows`).
        let (x, y) = if F == 2 || F == 3 {
            (r / 32, r % 32)
        } else {
            (r % 32, r / 32)
        };
        let mut bits = bits;
        while bits != 0 {
            let bit = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let z = if F >= 4 { bit - 1 } else { bit };
            let i = (z + 1) + (x + 1) * PADDED + (y + 1) * PADDED_AREA;
            let front = i.wrapping_add_signed(normal);
            visit(i, front, front_neighborhood::<F>(opaque, x, y, z))?;
        }
    }
    Ok(())
}

/// Opacity of the 3x3 front-layer neighborhood of the face `F` of unpadded voxel `(x, y, z)`,
/// bit `(dv + 1) * 3 + (du + 1)` (see `shading`).
#[inline]
fn front_neighborhood<const F: usize>(opaque: &[u64], x: usize, y: usize, z: usize) -> u32 {
    // Mask row of padded (px, py); bit pz.
    let row = |px: usize, py: usize| opaque[px + py * PADDED];
    // Bits j -> 3j (a column of three cells along v).
    let spread3 = |t: u64| ((t & 1) | (t & 2) << 2 | (t & 4) << 4) as u32;
    match F {
        // ±Y: u = x, v = z; the front layer is padded y + 2 (+Y) or y (-Y). The bits of padded
        // z..z + 2 are the cells dv = -1..=1.
        0 | 1 => {
            let py = if F == 0 { y + 2 } else { y };
            (0..3).fold(0, |nb, du| nb | spread3((row(x + du, py) >> z) & 7) << du)
        }
        // ±X: u = z, v = y; the front layer is padded x + 2 (+X) or x (-X).
        2 | 3 => {
            let px = if F == 2 { x + 2 } else { x };
            (0..3).fold(0, |nb, dv| {
                nb | (((row(px, y + dv) >> z) & 7) as u32) << (3 * dv)
            })
        }
        // ±Z: u = x, v = y; the front layer is padded z + 2 (+Z) or z (-Z).
        _ => {
            let pz = if F == 4 { z + 2 } else { z };
            let mut nb = 0;
            for dv in 0..3 {
                for du in 0..3 {
                    nb |= (((row(x + du, y + dv) >> pz) & 1) as u32) << (3 * dv + du);
                }
            }
            nb
        }
    }
}

/// Converts the merged quads to [`PackedQuad`]s and writes them bucket-contiguous into `out`
/// (its palette already set), decoding each quad's key with `decode`.
fn assemble(
    greedy: &Greedy,
    materials: &MaterialTable,
    out: &mut ChunkMesh,
    decode: impl Fn(u32) -> FaceKey,
) {
    // Pass 1: bucket sizes, so each bucket is written in place (no second buffer).
    let mut counts = [0u32; BUCKET_COUNT];
    for (face, quads) in Face::ALL.into_iter().zip(&greedy.quads) {
        for &quad in quads {
            counts[materials.bucket(decode((quad >> 32) as u32).material, face)] += 1;
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
    for (face, quads) in Face::ALL.into_iter().zip(&greedy.quads) {
        for &quad in quads {
            let key = decode((quad >> 32) as u32);
            let bucket = materials.bucket(key.material, face);
            let packed = to_packed(quad, face, materials.layer(key.material, face), key);
            if cfg!(debug_assertions) {
                debug_check_merged(packed, out.corner_light(packed));
            }
            out.quads[cursor[bucket]] = packed;
            cursor[bucket] += 1;
        }
    }
    out.bucket_counts = counts;
}

/// The [`PackedQuad`] of a merged quad (`Greedy::quads` format) on `face`, with its texture
/// layer, AO byte and light palette index.
#[inline]
fn to_packed(quad: u64, face: Face, layer: u16, key: FaceKey) -> PackedQuad {
    use layout::*;
    PackedQuad {
        geo: quad as u32 | (face as u32) << GEO_FACE_SHIFT,
        mat: (u32::from(layer) & MAT_LAYER_MASK)
            | u32::from(key.ao) << MAT_AO_SHIFT
            | (u32::from(key.light) & MAT_LIGHT_MASK) << MAT_LIGHT_SHIFT,
    }
}

/// Debug check that a merged quad's AO and corner lights are constant along every axis it
/// spans more than one voxel on, so interpolating its 4 corners gives exactly what its unit
/// faces would (holds because both only depend on the vertex).
fn debug_check_merged(quad: PackedQuad, light: [LightValue; 4]) {
    let ao = |k| quad.ao(k);
    if quad.w() > 1 {
        debug_assert!(
            light[0] == light[1] && light[2] == light[3] && ao(0) == ao(1) && ao(2) == ao(3),
            "{quad:?} varies along u: light {light:?}"
        );
    }
    if quad.h() > 1 {
        debug_assert!(
            light[0] == light[2] && light[1] == light[3] && ao(0) == ao(2) && ao(1) == ao(3),
            "{quad:?} varies along v: light {light:?}"
        );
    }
}

/// Mesher class of padded `(x, y, z)` from the column masks (what face culling reads).
fn class_from_masks(padded: &PaddedChunk, x: i32, y: i32, z: i32) -> VoxelClass {
    let row = mask_row(x, y);
    let bit = 1u64 << (z + 1);
    if padded.opaque[row] & bit != 0 {
        VoxelClass::Opaque
    } else if padded.transparent[row] & bit != 0 {
        VoxelClass::Transparent
    } else {
        VoxelClass::Empty
    }
}

/// Brute-force oracle: every visible unit face `(voxel, face)` of the inner 32^3 (a face is
/// visible if the voxel is non-empty and the neighbor is empty, or transparent with a different
/// material). The mesher's output, expanded with `PackedQuad::unit_faces`, must equal this set.
pub fn reference_visible_faces(padded: &PaddedChunk) -> BTreeSet<([u32; 3], Face)> {
    // Class from the column masks (what bgm reads), material from the voxel ids.
    let class = |x, y, z| class_from_masks(padded, x, y, z);
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

/// Brute-force oracle of the shading: for every visible unit face ([`reference_visible_faces`]),
/// its AO byte and the exact light of its 4 corners (no [`LightQuantization`]), straight from
/// the definitions (ADR 0015):
/// - The front layer is the layer of voxels the face looks at; the 4 front-layer voxels around
///   a corner are the one in front of the face, its side along u, its side along v and the
///   diagonal. Opaque means `PaddedChunk::opaque` (render-opaque).
/// - Corner light: the rounded per-channel mean `(sum + n / 2) / n` of the light of those 4
///   voxels, without the opaque ones, and without the diagonal when both sides are opaque.
///   `light` `None` is sky 15 everywhere.
/// - AO per corner: 0 when both sides are opaque, else 3 minus the opaque count of the sides
///   and the diagonal. Always open with [`AoPolicy::Off`]; open on a face whose 4 corner lights
///   are 0 with [`AoPolicy::BakedLitOnly`].
pub fn reference_face_shading(
    padded: &PaddedChunk,
    light: Option<&PaddedLight>,
    policy: AoPolicy,
) -> BTreeMap<([u32; 3], Face), FaceShading> {
    let opaque = |p: [i32; 3]| class_from_masks(padded, p[0], p[1], p[2]) == VoxelClass::Opaque;
    let light_at = |p: [i32; 3]| light.map_or(LightValue::FULL_SKY, |l| l.get(p[0], p[1], p[2]));
    let mut out = BTreeMap::new();
    for (p, face) in reference_visible_faces(padded) {
        let (_, ua, va) = face.axes();
        let n = face.normal().to_array();
        let front: [i32; 3] = std::array::from_fn(|a| p[a] as i32 + n[a]);
        let cell = |du: i32, dv: i32| {
            let mut c = front;
            c[ua] += du;
            c[va] += dv;
            c
        };
        let mut ao = 0u8;
        let mut corners = [LightValue::DARK; 4];
        for (k, corner) in corners.iter_mut().enumerate() {
            let su = if k & 1 == 1 { 1 } else { -1 };
            let sv = if k >> 1 == 1 { 1 } else { -1 };
            let (side_u, side_v, diagonal) = (cell(su, 0), cell(0, sv), cell(su, sv));
            let (ou, ov, od) = (opaque(side_u), opaque(side_v), opaque(diagonal));
            ao |= corner_ao(ou, ov, od) << (2 * k);
            let mut kept = vec![light_at(front)];
            if !ou {
                kept.push(light_at(side_u));
            }
            if !ov {
                kept.push(light_at(side_v));
            }
            if !od && !(ou && ov) {
                kept.push(light_at(diagonal));
            }
            let count = kept.len() as u16;
            let mut value = 0u16;
            for shift in [12, 8, 4, 0] {
                let sum: u16 = kept.iter().map(|l| (l.0 >> shift) & 15).sum();
                value |= ((sum + count / 2) / count) << shift;
            }
            *corner = LightValue(value);
        }
        let dark = corners.iter().all(|c| c.is_dark());
        let ao = match policy {
            AoPolicy::Off => AO_OPEN_BYTE,
            AoPolicy::BakedLitOnly if dark => AO_OPEN_BYTE,
            AoPolicy::Baked | AoPolicy::BakedLitOnly => ao,
        };
        out.insert((p, face), FaceShading { ao, light: corners });
    }
    out
}
