//! Far level of detail meshing (ADR 0031). A LOD node is a 32^3 grid of coarse cells, each the
//! state the cell rule gives it (`boxel_voxel::lod`); it is meshed by the ordinary [`Mesher`] on
//! a hand-filled [`PaddedChunk`] ([`LodMaterials::fill`]), with no light and no AO, then through
//! three post-passes that let the renderer swap a node's octants for finer detail without cracks:
//!
//! 1. [`split_octants`]: quads are cut at cell 16 along u and v, so each lies in one octant (the
//!    renderer hides whole octants that finer detail draws, by the bits 4, 9 and 14 of a quad's
//!    `geo`, [`octant_of`]).
//! 2. [`add_skirts`]: on the 6 node faces and the 3 planes at cell 16, a face toward the plane on
//!    every non-air cell near the surface whose cell across is not air either. Where the octant
//!    across is drawn by finer, lower terrain, the skirt closes the step; where it is drawn by
//!    this node, the skirt lies inside solid cells and is never seen.
//! 3. [`add_fluid_walls`] (level 1 only): the same planes, a face on every cell next to a plane
//!    that lies near a fluid, so that a ray under the translucent near fluid meets the coarse
//!    fluid's or ground's side instead of the sky.
//!
//! Every visible state is an opaque cube at a distance, fluids too ([`LodMaterials`]): a node's
//! quads land in the 6 opaque face buckets, cubes only. The quads of post-passes 2 and 3 come
//! last in their bucket, marked by their plane ([`Seam`]), so the renderer can leave out those
//! nothing needs ([`seam_counts`]).
use crate::packed::layout;
use crate::{
    AoPolicy, BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, FULL_SKY_ENTRY, MAX_QUADS_PER_BUCKET,
    MAX_QUADS_PER_FACE_BUCKET, MaterialTable, MeshOptions, Mesher, PADDED, PADDED_AREA,
    PADDED_VOLUME, PackedQuad, PaddedChunk, PaletteEntry, VoxelClass,
};
use boxel_voxel::lod::{LodClass, LodClasses};
use boxel_voxel::{BlockRegistry, BlockState, Face, RenderShape};
use std::collections::HashMap;

/// Cells along an octant edge: half a node.
pub const OCTANT_CELLS: u32 = 16;

/// Cells along a node edge.
const NODE: i32 = 32;

/// The planes post-passes 2 and 3 work on, per axis: the node's two faces and its middle.
const PLANES: [i32; 3] = [0, 16, 32];

/// What every registry state looks like at a distance: its canonical state (the material id of
/// LOD meshes) and the LOD [`MaterialTable`].
///
/// The canonical state of a state is the lowest state id with the same [`LodClass`], the same 6
/// face texture layers and the same emissive flag, so look-alike variants (the levels of a
/// fluid, a slab and its full block, a waterlogged state) merge into one greedy quad. Fluid
/// states first become their source. Empty states have material 0. In the table every canonical
/// state is an opaque cube (`RenderShape::Opaque`, `SHAPE_CUBE`) with its state's face layers and
/// emissive flag: no fluid, box or cross shape reaches a LOD mesh.
#[derive(Clone, Debug)]
pub struct LodMaterials {
    classes: LodClasses,
    /// By state id: the canonical state's material (0 for Empty states).
    canonical: Box<[u16]>,
    /// By material id: the class of a canonical state (`Empty` for the others).
    material_class: Box<[LodClass]>,
    table: MaterialTable,
}

impl LodMaterials {
    /// The LOD materials of `registry` (type docs).
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let classes = LodClasses::from_registry(registry);
        let n = registry.len();
        let mut canonical = vec![0u16; n];
        let mut material_class = vec![LodClass::Empty; n];
        let mut shapes = vec![RenderShape::None; n];
        let mut layers = vec![[0u16; 6]; n];
        let mut emissive = Vec::new();
        let mut first: HashMap<(LodClass, [u16; 6], bool), u16> = HashMap::new();
        for (i, slot) in canonical.iter_mut().enumerate() {
            let s = BlockState(i as u16);
            let class = classes.class(s);
            let looks = match class {
                LodClass::Empty => continue,
                LodClass::Solid => s,
                LodClass::Fluid => classes.fluid(s).unwrap_or(s),
            };
            let faces = Face::ALL.map(|f| registry.face_texture(looks, f).0);
            let glows = !registry.emission(looks).is_dark();
            let material = *first.entry((class, faces, glows)).or_insert_with(|| {
                let m = i as u16;
                material_class[i] = class;
                shapes[i] = RenderShape::Opaque;
                layers[i] = faces;
                if glows {
                    emissive.push(m);
                }
                m
            });
            *slot = material;
        }
        let mut table = MaterialTable::from_parts(shapes, layers);
        for m in emissive {
            table.set_emissive(m, true);
        }
        Self {
            classes,
            canonical: canonical.into(),
            material_class: material_class.into(),
            table,
        }
    }

    /// The registry's LOD classes ([`LodClasses::from_registry`]).
    pub fn classes(&self) -> &LodClasses {
        &self.classes
    }

    /// The LOD material table: every canonical state an opaque cube.
    pub fn table(&self) -> &MaterialTable {
        &self.table
    }

    /// The material of state `s` in a LOD mesh: its canonical state's id, 0 for an Empty state
    /// (and out of range).
    pub fn canonical(&self, s: BlockState) -> u16 {
        self.canonical.get(usize::from(s.0)).copied().unwrap_or(0)
    }

    /// The class of LOD material `material` (a canonical state; `Empty` for any other id).
    pub fn class(&self, material: u16) -> LodClass {
        self.material_class
            .get(usize::from(material))
            .copied()
            .unwrap_or(LodClass::Empty)
    }

    /// Fills `padded` from a node's 34^3 cells (`cells[PaddedChunk::index(x, y, z)]`, the order
    /// of `boxel_worldgen::lod::LodCells`): every non-Empty cell becomes its canonical material,
    /// an opaque voxel. Panics if `cells` is not 34^3 long.
    pub fn fill(&self, cells: &[BlockState], padded: &mut PaddedChunk) {
        assert_eq!(cells.len(), PADDED_VOLUME, "a node's padded cells");
        padded.clear();
        let mut i = 0;
        for y in -1..=NODE {
            for x in -1..=NODE {
                for z in -1..=NODE {
                    let m = self.canonical(cells[i]);
                    if m != 0 {
                        padded.set(x, y, z, m, VoxelClass::Opaque);
                    }
                    i += 1;
                }
            }
        }
    }
}

/// The octant of a LOD quad in its node, `x | y << 1 | z << 2` (each 0 or 1): bits 4, 9 and 14 of
/// its `geo`, the octant of its min cell. After [`split_octants`] every cell under the quad lies
/// in it.
pub const fn octant_of(quad: PackedQuad) -> u8 {
    let g = quad.geo;
    (((g >> 4) & 1) | (((g >> 9) & 1) << 1) | (((g >> 14) & 1) << 2)) as u8
}

/// Where a LOD quad lies: a face the mesher found, or a quad of post-pass 2 or 3
/// ([`add_skirts`], [`add_fluid_walls`]) on one of the node's 6 faces or on a middle plane (cell
/// 16 of an axis). Those close the seam with other detail across their plane, and only such a
/// seam shows them: where this node or a neighbor of its level draws the cells across, they lie
/// between solid cells, behind the faces of the surface.
///
/// LOD meshes have no light, so a quad's light palette index holds its seam: a LOD mesh's palette
/// is [`LOD_PALETTE`] (open sky three times) and a quad's entry is `seam as u16`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Seam {
    /// A face of the mesher, always drawn.
    None = 0,
    /// On a node face (plane 0 or 32): needed where the node across is not of this level, or
    /// does not draw its cells by the plane.
    Face = 1,
    /// On a middle plane (16): needed where the octant across is hidden and this one is not.
    Middle = 2,
}

/// The light palette of a non-empty LOD mesh: one open-sky entry per [`Seam`].
pub const LOD_PALETTE: [PaletteEntry; 3] = [FULL_SKY_ENTRY; 3];

/// The [`Seam`] of a LOD quad (its light palette index).
pub const fn seam_of(quad: PackedQuad) -> Seam {
    match quad.light_index() {
        0 => Seam::None,
        1 => Seam::Face,
        _ => Seam::Middle,
    }
}

/// Per face bucket of a LOD mesh, `[on a node face, on a middle plane]`: the quads at the end of
/// the bucket marked [`Seam::Middle`], and the ones marked [`Seam::Face`] right before them, the
/// order [`mesh_node`] gives. The renderer may leave those out (each run, or the middle one
/// alone); anything else counts as a face of the mesher, always drawn.
pub fn seam_counts(mesh: &ChunkMesh) -> [[u32; 2]; 6] {
    std::array::from_fn(|b| {
        let quads = mesh.bucket(b);
        let middle = quads
            .iter()
            .rev()
            .take_while(|&&q| seam_of(q) == Seam::Middle)
            .count();
        let face = quads[..quads.len() - middle]
            .iter()
            .rev()
            .take_while(|&&q| seam_of(q) == Seam::Face)
            .count();
        [face as u32, middle as u32]
    })
}

/// Meshes a LOD node: `padded` filled by [`LodMaterials::fill`], meshed without light and AO,
/// then [`split_octants`], [`add_skirts`] and, at `level` 1, [`add_fluid_walls`]. In each bucket
/// the mesher's faces come first, then the quads on a node face, then those on a middle plane
/// ([`seam_counts`]).
pub fn mesh_node(
    mesher: &mut Mesher,
    padded: &PaddedChunk,
    materials: &LodMaterials,
    level: u8,
    out: &mut ChunkMesh,
) {
    mesher.mesh(
        padded,
        None,
        materials.table(),
        MeshOptions::with_ao(AoPolicy::Off),
        out,
    );
    split_octants(out);
    add_skirts(padded, materials, out);
    add_fluid_walls(padded, materials, level, out);
}

/// True if every bucket of `mesh` is within the renderer's limits
/// ([`MAX_QUADS_PER_FACE_BUCKET`] for a face bucket, [`MAX_QUADS_PER_BUCKET`] for the others).
pub fn fits(mesh: &ChunkMesh) -> bool {
    mesh.bucket_counts.iter().enumerate().all(|(b, &n)| {
        n <= if b < CUTOUT_BUCKET {
            MAX_QUADS_PER_FACE_BUCKET
        } else {
            MAX_QUADS_PER_BUCKET
        }
    })
}

/// Post-pass 1: cuts every greedy quad of `mesh` at cell 16 along its u and v axes, so all the
/// cells under each quad lie in one octant ([`octant_of`]). The covered faces do not change.
pub fn split_octants(mesh: &mut ChunkMesh) {
    let mut buckets: [Vec<PackedQuad>; BUCKET_COUNT] = Default::default();
    for (b, out) in buckets.iter_mut().enumerate() {
        for &q in mesh.bucket(b) {
            split_quad(q, out);
        }
    }
    set_buckets(mesh, buckets);
}

/// Post-pass 2 (every level): skirts. On the 6 node faces and the planes x, y, z = 16, every
/// non-air cell next to the plane whose cell across is non-air too and with an air cell within 2
/// cells (Chebyshev, inside the padded grid) gets a face toward the plane, in its own material
/// (fluid cells too: a coarser fluid top stands up to a cell above a finer one). Skirts merge
/// greedily per plane, material and octant; each goes into the bucket of its face, a cube face
/// of its cell's octant, after the quads there before it and marked with its plane's [`Seam`].
/// `padded` is the node as [`LodMaterials::fill`] wrote it.
pub fn add_skirts(padded: &PaddedChunk, materials: &LodMaterials, mesh: &mut ChunkMesh) {
    let near_air = near_air(padded);
    let mut extra = Vec::new();
    for (face, n) in plane_faces() {
        let grid = face_grid(
            face,
            n,
            |c, across| {
                let m = padded.get(c[0], c[1], c[2]);
                (m != 0 && across != 0 && is_near(&near_air, c)).then_some(m)
            },
            padded,
        );
        merge_grid(&grid, face, n, materials.table(), seam(n), &mut extra);
    }
    append(mesh, extra);
}

/// Post-pass 3 (`level` 1 only; nothing at other levels): fluid walls. On the planes of
/// [`add_skirts`], every non-air cell whose cell across is non-air too and that has a fluid cell
/// within 2 cells (Chebyshev, itself included) gets a face toward the plane: the whole depth of
/// a fluid next to a plane and the sides of the ground around it. Skirts are left out (and faces
/// with air across, which the mesher drew). A ray that enters the translucent full-detail fluid
/// next to a level-1 node and crosses under its surface into the node meets a side there
/// instead of the sky, even where the cell rule rounded the cell across up to ground; where the
/// node draws the cells across, the walls lie inside them and are never seen. Ordered and
/// marked like the skirts.
pub fn add_fluid_walls(
    padded: &PaddedChunk,
    materials: &LodMaterials,
    level: u8,
    mesh: &mut ChunkMesh,
) {
    if level != 1 {
        return;
    }
    let near_air = near_air(padded);
    let near_fluid = near_fluid(padded, materials);
    let mut extra = Vec::new();
    for (face, n) in plane_faces() {
        let grid = face_grid(
            face,
            n,
            |c, across| {
                let m = padded.get(c[0], c[1], c[2]);
                let wall = m != 0 && across != 0 && is_near(&near_fluid, c);
                (wall && !is_near(&near_air, c)).then_some(m)
            },
            padded,
        );
        merge_grid(&grid, face, n, materials.table(), seam(n), &mut extra);
    }
    append(mesh, extra);
}

/// The faces post-passes 2 and 3 add, each with the cell coordinate along its normal: on plane
/// `p` of an axis, the cell at `p - 1` faces the positive way (planes 16 and 32) and the cell at
/// `p` the negative way (planes 0 and 16).
fn plane_faces() -> impl Iterator<Item = (Face, i32)> {
    [
        (Face::PosX, Face::NegX),
        (Face::PosY, Face::NegY),
        (Face::PosZ, Face::NegZ),
    ]
    .into_iter()
    .flat_map(|(pos, neg)| {
        PLANES.into_iter().flat_map(move |p| {
            let up = (p > 0).then_some((pos, p - 1));
            let down = (p < NODE).then_some((neg, p));
            up.into_iter().chain(down)
        })
    })
}

/// The [`Seam`] of the quads of [`plane_faces`] at normal coordinate `n`: the cells by a middle
/// plane are 15 and 16.
fn seam(n: i32) -> Seam {
    if n == OCTANT_CELLS as i32 - 1 || n == OCTANT_CELLS as i32 {
        Seam::Middle
    } else {
        Seam::Face
    }
}

/// The cell offset across `face`.
fn face_step(face: Face) -> [i32; 3] {
    let d = face.normal();
    [d.x, d.y, d.z]
}

fn add(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// The 32x32 grid (`[v][u]`, by [`Face::axes`]) of the materials of `face` at normal coordinate
/// `n` that `pick(cell, material across)` keeps.
fn face_grid(
    face: Face,
    n: i32,
    mut pick: impl FnMut([i32; 3], u16) -> Option<u16>,
    padded: &PaddedChunk,
) -> Box<[[u16; 32]; 32]> {
    let (na, ua, va) = face.axes();
    let step = face_step(face);
    let mut grid = Box::new([[0u16; 32]; 32]);
    for (v, row) in grid.iter_mut().enumerate() {
        for (u, slot) in row.iter_mut().enumerate() {
            let mut c = [0; 3];
            c[na] = n;
            c[ua] = u as i32;
            c[va] = v as i32;
            let a = add(c, step);
            *slot = pick(c, padded.get(a[0], a[1], a[2])).unwrap_or(0);
        }
    }
    grid
}

/// Greedy rectangles of equal material in `grid` (0 = none), never across an octant border
/// (cell 16 of u or v), as quads of `face` at normal coordinate `n` marked `seam`.
fn merge_grid(
    grid: &[[u16; 32]; 32],
    face: Face,
    n: i32,
    table: &MaterialTable,
    seam: Seam,
    out: &mut Vec<PackedQuad>,
) {
    let (na, ua, va) = face.axes();
    let mut used = [0u32; 32];
    for v0 in 0..32usize {
        let mut u0 = 0usize;
        while u0 < 32 {
            let m = grid[v0][u0];
            if m == 0 || used[v0] & (1 << u0) != 0 {
                u0 += 1;
                continue;
            }
            // Along u, inside the octant half of u0.
            let u_end = (u0 | 15) + 1;
            let mut u1 = u0 + 1;
            while u1 < u_end && grid[v0][u1] == m && used[v0] & (1 << u1) == 0 {
                u1 += 1;
            }
            let run = (((1u64 << (u1 - u0)) - 1) << u0) as u32;
            // Along v, while the whole run matches, inside the octant half of v0.
            let v_end = (v0 | 15) + 1;
            let mut v1 = v0 + 1;
            while v1 < v_end && used[v1] & run == 0 && grid[v1][u0..u1].iter().all(|&g| g == m) {
                v1 += 1;
            }
            for row in &mut used[v0..v1] {
                *row |= run;
            }
            let mut pos = [0u32; 3];
            pos[na] = n as u32;
            pos[ua] = u0 as u32;
            pos[va] = v0 as u32;
            let flags = if table.emissive(m) {
                layout::FLAG_EMISSIVE as u8
            } else {
                0
            };
            let quad = PackedQuad::new(
                pos,
                (u1 - u0) as u32,
                (v1 - v0) as u32,
                face,
                table.layer(m, face),
            )
            .with_flags(flags | layout::SHAPE_CUBE << 1)
            .with_light_index(seam as u16);
            out.push(quad);
            u0 = u1;
        }
    }
}

/// Per padded row (`(x + 1) + (y + 1) * 34`, bit `z + 1`): an air cell (no voxel) lies within 2
/// cells, Chebyshev, inside the padded grid.
fn near_air(padded: &PaddedChunk) -> Box<[u64]> {
    within_two(|row| !(padded.opaque[row] | padded.transparent[row] | padded.special[row]))
}

/// Per padded row, as [`near_air`]: a fluid cell lies within 2 cells (the cell itself included).
fn near_fluid(padded: &PaddedChunk, materials: &LodMaterials) -> Box<[u64]> {
    within_two(|row| {
        let voxels = &padded.voxels[row * PADDED..(row + 1) * PADDED];
        voxels.iter().enumerate().fold(0, |bits, (z, &m)| {
            if materials.class(m) == LodClass::Fluid {
                bits | 1 << z
            } else {
                bits
            }
        })
    })
}

/// The padded row masks `cells(row)` (bit `z + 1`) dilated by 2 cells on every axis (Chebyshev),
/// inside the padded grid.
fn within_two(cells: impl Fn(usize) -> u64) -> Box<[u64]> {
    const ALL: u64 = (1 << PADDED) - 1;
    let p = PADDED;
    // Along z, then along x (rows of one y), then along y.
    let along_z: Vec<u64> = (0..PADDED_AREA)
        .map(|row| {
            let c = cells(row) & ALL;
            (c | c << 1 | c >> 1 | c << 2 | c >> 2) & ALL
        })
        .collect();
    let dilate = |src: &[u64], stride: usize| -> Vec<u64> {
        (0..PADDED_AREA)
            .map(|row| {
                let (x, y) = (row % p, row / p);
                let at = if stride == 1 { x } else { y };
                let lo = at.saturating_sub(2);
                let hi = (at + 2).min(p - 1);
                (lo..=hi).fold(0, |acc, k| {
                    let r = if stride == 1 { k + y * p } else { x + k * p };
                    acc | src[r]
                })
            })
            .collect()
    };
    let along_x = dilate(&along_z, 1);
    dilate(&along_x, p).into()
}

/// True if bit `c` of a [`within_two`] mask is set.
fn is_near(mask: &[u64], c: [i32; 3]) -> bool {
    let row = (c[0] + 1) as usize + (c[1] + 1) as usize * PADDED;
    mask[row] & (1 << (c[2] + 1)) != 0
}

/// The quad cut at cell 16 along u and v (greedy shapes; the per-block shapes have `w = h = 1`).
fn split_quad(q: PackedQuad, out: &mut Vec<PackedQuad>) {
    use layout::*;
    let greedy = matches!(
        q.shape(),
        SHAPE_CUBE | SHAPE_TURNED | SHAPE_FLUID_CUBE | SHAPE_FLUID_SURFACE
    );
    if !greedy {
        out.push(q);
        return;
    }
    let (_, ua, va) = q.face().axes();
    let pos = q.pos();
    for (u0, u1) in halves(pos[ua], q.w()) {
        for (v0, v1) in halves(pos[va], q.h()) {
            let mut p = pos;
            p[ua] = u0;
            p[va] = v0;
            out.push(reshaped(q, p, u1 - u0, v1 - v0));
        }
    }
}

/// `start..start + len` cut at [`OCTANT_CELLS`] (one or two ranges).
fn halves(start: u32, len: u32) -> impl Iterator<Item = (u32, u32)> {
    let end = start + len;
    let cut = start < OCTANT_CELLS && end > OCTANT_CELLS;
    let first = (start, if cut { OCTANT_CELLS } else { end });
    let second = cut.then_some((OCTANT_CELLS, end));
    std::iter::once(first).chain(second)
}

/// `q` moved to `pos` with extents `w` and `h`; face, flags and material word kept.
fn reshaped(q: PackedQuad, pos: [u32; 3], w: u32, h: u32) -> PackedQuad {
    use layout::*;
    let keep = q.geo & !((1 << GEO_FACE_SHIFT) - 1);
    let geo = pos[0]
        | pos[1] << GEO_Y_SHIFT
        | pos[2] << GEO_Z_SHIFT
        | (w - 1) << GEO_W_SHIFT
        | (h - 1) << GEO_H_SHIFT;
    PackedQuad {
        geo: keep | geo,
        mat: q.mat,
    }
}

/// Adds `extra` cube quads (open sky, marked with their [`Seam`]) to `mesh`, each in the bucket
/// of its face, every bucket in [`Seam`] order (stable: the quads of a seam keep theirs).
fn append(mesh: &mut ChunkMesh, extra: Vec<PackedQuad>) {
    if extra.is_empty() {
        return;
    }
    let mut buckets: [Vec<PackedQuad>; BUCKET_COUNT] = Default::default();
    for (b, out) in buckets.iter_mut().enumerate() {
        out.extend_from_slice(mesh.bucket(b));
    }
    for q in extra {
        buckets[q.face().index()].push(q);
    }
    for quads in &mut buckets {
        quads.sort_by_key(|&q| seam_of(q));
    }
    set_buckets(mesh, buckets);
}

/// Writes `buckets` into `mesh` bucket-contiguous, with the palette [`LOD_PALETTE`] (open sky:
/// what the mesher gives a mesh without light, once per [`Seam`]) and a new hash.
fn set_buckets(mesh: &mut ChunkMesh, buckets: [Vec<PackedQuad>; BUCKET_COUNT]) {
    debug_assert!(
        mesh.palette.iter().all(|&e| e == FULL_SKY_ENTRY),
        "LOD meshes have no light"
    );
    mesh.quads.clear();
    for (b, quads) in buckets.iter().enumerate() {
        mesh.bucket_counts[b] = quads.len() as u32;
        mesh.quads.extend_from_slice(quads);
    }
    mesh.palette.clear();
    if !mesh.quads.is_empty() {
        mesh.palette.extend_from_slice(&LOD_PALETTE);
    }
    mesh.hash = mesh.compute_hash();
}
