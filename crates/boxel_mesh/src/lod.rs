//! Far level of detail meshing (ADR 0031). A LOD node is a 32^3 grid of coarse cells, each the
//! state the cell rule gives it (`boxel_voxel::lod`); it is meshed by the ordinary [`Mesher`] on
//! a hand-filled [`PaddedChunk`] ([`LodMaterials::fill`]), with no light and no AO, then through
//! post-passes that let the renderer swap a node's octants for finer detail without cracks:
//!
//! 1. [`split_octants`]: quads are cut at cell 16 along u and v, so each lies in one octant (the
//!    renderer hides whole octants that finer detail draws, by the bits 4, 9 and 14 of a quad's
//!    `geo`, [`octant_of`]).
//! 2. [`add_underwater`]: the faces of every cell that is not see-through against a see-through
//!    fluid (water), lit darker the deeper they lie ([`dark_of`]): the bed under far water.
//! 3. [`add_skirts`]: on the 6 node faces and the 3 planes at cell 16, a face toward the plane on
//!    every ground cell near the surface whose cell across is ground too, and on the top cell of
//!    a see-through fluid whose cell across is not air. Where the octant across is drawn by
//!    finer, lower terrain, the skirt closes the step; where it is drawn by this node, the skirt
//!    lies inside solid cells (or inside the water, behind its surface) and is never seen.
//! 4. [`add_fluid_walls`]: the same planes, a face on every ground cell next to a plane that lies
//!    near a see-through fluid, so that a ray under the water (near or far) meets the ground's
//!    side instead of the sky, even where the cell rule rounded the cell across up to ground.
//!
//! Every visible state is a cube at a distance ([`LodMaterials`]): an opaque one, but for the
//! see-through fluids, whose faces against the air go into the translucent bucket. Inside the
//! node a see-through fluid still hides its neighbors' faces from the mesher, so the bed under it
//! comes from post-pass 2 alone. The quads of post-passes 3 and 4 come last in their bucket,
//! marked by their plane ([`Seam`]), so the renderer can leave out those nothing needs
//! ([`seam_counts`], [`water_seams`]).
use crate::packed::layout;
use crate::{
    AoPolicy, BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, FULL_SKY_ENTRY, MAX_QUADS_PER_BUCKET,
    MAX_QUADS_PER_FACE_BUCKET, MaterialTable, MeshOptions, Mesher, PADDED, PADDED_AREA,
    PADDED_VOLUME, PackedQuad, PaddedChunk, PaletteEntry, TRANSLUCENT_BUCKET, VoxelClass,
};
use boxel_voxel::lod::{LodClass, LodClasses};
use boxel_voxel::{BlockRegistry, BlockState, Face, RenderShape};
use std::collections::HashMap;

/// Cells along an octant edge: half a node.
pub const OCTANT_CELLS: u32 = 16;

/// Cells along a node edge.
const NODE: i32 = 32;

/// The planes post-passes 3 and 4 work on, per axis: the node's two faces and its middle.
const PLANES: [i32; 3] = [0, 16, 32];

/// What every registry state looks like at a distance: its canonical state (the material id of
/// LOD meshes) and the LOD [`MaterialTable`].
///
/// The canonical state of a state is the lowest state id with the same [`LodClass`], the same 6
/// face texture layers, the same emissive flag and the same see-through flag, so look-alike
/// variants (the levels of a fluid, a slab and its full block, a waterlogged state) merge into
/// one greedy quad. Fluid states first become their source. Empty states have material 0. In the
/// table every canonical state is a cube (`SHAPE_CUBE`) with its state's face layers and emissive
/// flag: an opaque one (`RenderShape::Opaque`), but for a fluid whose source is translucent (water,
/// ADR 0025), which is see-through at a distance too (`RenderShape::Translucent`: its faces go
/// into the translucent bucket). No fluid, box or cross shape reaches a LOD mesh.
#[derive(Clone, Debug)]
pub struct LodMaterials {
    classes: LodClasses,
    /// By state id: the canonical state's material (0 for Empty states).
    canonical: Box<[u16]>,
    /// By material id: the class of a canonical state (`Empty` for the others).
    material_class: Box<[LodClass]>,
    /// By material id: a see-through fluid's canonical state.
    see_through: Box<[bool]>,
    table: MaterialTable,
}

impl LodMaterials {
    /// The LOD materials of `registry` (type docs).
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let classes = LodClasses::from_registry(registry);
        let n = registry.len();
        let mut canonical = vec![0u16; n];
        let mut material_class = vec![LodClass::Empty; n];
        let mut see_through = vec![false; n];
        let mut shapes = vec![RenderShape::None; n];
        let mut layers = vec![[0u16; 6]; n];
        let mut emissive = Vec::new();
        let mut first: HashMap<(LodClass, [u16; 6], bool, bool), u16> = HashMap::new();
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
            let clear =
                class == LodClass::Fluid && registry.shape(looks) == RenderShape::Translucent;
            let key = (class, faces, glows, clear);
            let material = *first.entry(key).or_insert_with(|| {
                let m = i as u16;
                material_class[i] = class;
                see_through[i] = clear;
                shapes[i] = if clear {
                    RenderShape::Translucent
                } else {
                    RenderShape::Opaque
                };
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
            see_through: see_through.into(),
            table,
        }
    }

    /// The registry's LOD classes ([`LodClasses::from_registry`]).
    pub fn classes(&self) -> &LodClasses {
        &self.classes
    }

    /// The LOD material table: every canonical state a cube, opaque or (a see-through fluid)
    /// translucent.
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

    /// True if LOD material `material` is a see-through fluid (water): translucent in the
    /// table, with the bed under it drawn ([`add_underwater`]).
    pub fn see_through(&self, material: u16) -> bool {
        self.see_through
            .get(usize::from(material))
            .copied()
            .unwrap_or(false)
    }

    /// Fills `padded` from a node's 34^3 cells (`cells[PaddedChunk::index(x, y, z)]`, the order
    /// of `boxel_worldgen::lod::LodCells`): every non-Empty cell becomes its canonical material,
    /// an opaque voxel (a see-through fluid too: the mesher draws no face between it and its
    /// neighbors). Panics if `cells` is not 34^3 long.
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

/// Where a LOD quad lies: a face of the mesher or of the bed under water ([`add_underwater`]),
/// or a quad of post-pass 3 or 4 ([`add_skirts`], [`add_fluid_walls`]) on one of the node's 6
/// faces or on a middle plane (cell 16 of an axis). Those close the seam with other detail across
/// their plane, and only such a seam shows them: where this node or a neighbor of its level draws
/// the cells across, they lie between solid cells, behind the faces of the surface (or, for a
/// see-through fluid, inside it, behind its surface).
///
/// LOD meshes have no light, so a quad's light palette index holds its seam (bits 0 and 1) and
/// its darkness ([`dark_of`], from [`DARK_SHIFT`]): a LOD mesh's palette is [`LOD_PALETTE`] (open
/// sky for every index).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Seam {
    /// A face of the mesher or of the bed under water, always drawn.
    None = 0,
    /// On a node face (plane 0 or 32): needed where the node across is not of this level, or
    /// does not draw its cells by the plane.
    Face = 1,
    /// On a middle plane (16): needed where the octant across is hidden and this one is not.
    Middle = 2,
}

/// Bits of the [`Seam`] in a LOD quad's light palette index.
const SEAM_MASK: u16 = 3;

/// Where a LOD quad's darkness starts in its light palette index ([`dark_of`]).
pub const DARK_SHIFT: u16 = 2;

/// The darkest a LOD face gets: sky light 0, 15 blocks or more under water.
pub const MAX_DARK: u8 = 15;

/// The light palette of a non-empty LOD mesh: open sky for every light palette index a quad can
/// have ([`Seam`] and darkness).
pub const LOD_PALETTE: [PaletteEntry; 64] = [FULL_SKY_ENTRY; 64];

/// The [`Seam`] of a LOD quad (bits 0 and 1 of its light palette index).
pub const fn seam_of(quad: PackedQuad) -> Seam {
    match quad.light_index() & SEAM_MASK {
        0 => Seam::None,
        1 => Seam::Face,
        _ => Seam::Middle,
    }
}

/// How much darker than open sky a LOD quad is lit, 0 to [`MAX_DARK`] levels of sky light (the
/// bits from [`DARK_SHIFT`] of its light palette index): the blocks of see-through fluid above
/// it. A see-through fluid has light opacity 1 (ADR 0025), so a block of it takes one level of
/// sky light, and the bed of a lake 10 blocks deep has sky light 5 (`boxel_light`). The LOD has
/// no light, so [`add_underwater`] gives each face under water that darkness: the shader lights
/// it with sky light `15 - dark`.
pub const fn dark_of(quad: PackedQuad) -> u8 {
    ((quad.light_index() >> DARK_SHIFT) & MAX_DARK as u16) as u8
}

/// Per face bucket of a LOD mesh, `[on a node face, on a middle plane]`: the quads at the end of
/// the bucket marked [`Seam::Middle`], and the ones marked [`Seam::Face`] right before them, the
/// order [`mesh_node`] gives. The renderer may leave those out (each run, or the middle one
/// alone); anything else counts as a face of the mesher, always drawn.
pub fn seam_counts(mesh: &ChunkMesh) -> [[u32; 2]; 6] {
    std::array::from_fn(|b| bucket_seams(mesh.bucket(b)))
}

/// [`seam_counts`] of the translucent bucket of a LOD mesh: the see-through fluid's quads (its
/// faces against the air, then its skirts).
pub fn water_seams(mesh: &ChunkMesh) -> [u32; 2] {
    bucket_seams(mesh.bucket(TRANSLUCENT_BUCKET))
}

/// `[on a node face, on a middle plane]` at the end of `quads` (see [`seam_counts`]).
fn bucket_seams(quads: &[PackedQuad]) -> [u32; 2] {
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
}

/// Meshes a LOD node: `padded` filled by [`LodMaterials::fill`], meshed without light and AO,
/// then [`split_octants`], and the quads of [`add_underwater`], [`add_skirts`] and
/// [`add_fluid_walls`] (`level` sets the darkness under water). In each bucket the faces of the
/// mesher come first, then the bed under water, then the quads on a node face, then those on a
/// middle plane ([`seam_counts`], [`water_seams`]).
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
    let water = Water::of(padded, materials, level);
    let mut extra = Vec::new();
    if let Some(water) = &water {
        underwater(padded, materials, water, &mut extra);
    }
    skirts(padded, materials, water.as_ref(), &mut extra);
    if let Some(water) = &water {
        walls(padded, materials, water, &mut extra);
    }
    append(out, extra);
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

/// Post-pass 2: the bed under a see-through fluid. Every face of a cell that is neither air nor
/// see-through (ground, or an opaque fluid) toward a see-through fluid cell (the padded border
/// included), lit as deep as the fluid above the cell it faces ([`dark_of`]; a level-`level` cell
/// is `2^level` blocks). The mesher draws none of them, since every voxel of `padded` hides its
/// neighbors. They merge greedily per plane, material, darkness and octant, go into the bucket of
/// their face and count as faces of the mesher ([`Seam::None`]: they are surfaces, and always
/// drawn), after the mesher's own. Nothing for a node without a see-through fluid.
pub fn add_underwater(
    padded: &PaddedChunk,
    materials: &LodMaterials,
    level: u8,
    mesh: &mut ChunkMesh,
) {
    if let Some(water) = Water::of(padded, materials, level) {
        let mut extra = Vec::new();
        underwater(padded, materials, &water, &mut extra);
        append(mesh, extra);
    }
}

/// Post-pass 3 (every level): skirts. On the 6 node faces and the planes x, y, z = 16, a face
/// toward the plane on:
/// - every ground cell (not air, not see-through) next to the plane whose cell across is ground
///   too (a see-through fluid across already has the bed's face) and with an air cell within 2
///   cells (Chebyshev, inside the padded grid);
/// - every top cell of a see-through fluid (air right above it) next to the plane whose cell
///   across is not air: a coarser fluid top can stand above a finer one, inside its own top cell
///   (each level rounds the true top to its cells), and the skirt closes that step. Lower down,
///   under the finer fluid, it would be a second layer of fluid in front of the bed. (A
///   generator fills every cell under the fluid's level with it, so finer ground by a shore
///   never lies lower than that top cell either.)
///
/// Skirts merge greedily per plane, material, darkness and octant; each goes into the bucket of
/// its material and face (a see-through fluid's into the translucent one), a cube face of its
/// cell's octant, after the quads there before it and marked with its plane's [`Seam`]. A ground
/// skirt under a see-through fluid is lit as deep as the cell it faces. `padded` is the node as
/// [`LodMaterials::fill`] wrote it, of level `level`.
pub fn add_skirts(padded: &PaddedChunk, materials: &LodMaterials, level: u8, mesh: &mut ChunkMesh) {
    let water = Water::of(padded, materials, level);
    let mut extra = Vec::new();
    skirts(padded, materials, water.as_ref(), &mut extra);
    append(mesh, extra);
}

/// Post-pass 4 (every level): walls. On the planes of [`add_skirts`], every ground cell (not air,
/// not see-through) whose cell across is ground too, with a see-through fluid cell within 2 cells
/// (Chebyshev) and not a skirt (no air within 2 cells), gets a face toward the plane, lit as deep
/// as the cell it faces: the sides of the ground under the fluid. A ray that goes under the
/// surface of a finer fluid (the translucent full-detail one, or a finer LOD level's) and crosses
/// into this node below its bed meets a side there instead of the sky, even where the cell rule
/// rounded the cell across up to ground; where the node draws the cells across, the walls lie
/// inside them and are never seen. Ordered and marked like the skirts. Nothing for a node
/// without a see-through fluid.
pub fn add_fluid_walls(
    padded: &PaddedChunk,
    materials: &LodMaterials,
    level: u8,
    mesh: &mut ChunkMesh,
) {
    if let Some(water) = Water::of(padded, materials, level) {
        let mut extra = Vec::new();
        walls(padded, materials, &water, &mut extra);
        append(mesh, extra);
    }
}

/// A quad and its bucket, on their way into a mesh ([`append`]).
type Placed = (usize, PackedQuad);

/// The key of a face in the grids of the post-passes: its material, and its darkness from bit 16
/// (0: none).
fn face_key(material: u16, dark: u8) -> u32 {
    u32::from(material) | u32::from(dark) << 16
}

/// What the post-passes need of a node's see-through fluid: where it is and how deep each cell
/// lies under it.
struct Water {
    /// Per padded row (`(x + 1) + (y + 1) * 34`, bit `z + 1`): a see-through fluid cell.
    clear: Box<[u64]>,
    /// Per padded cell ([`PaddedChunk::index`]): the cells from it up to the highest cell of
    /// the see-through fluid over it, counted through non-air cells (0: none over it). Counted
    /// inside the padded grid: a fluid that goes on above the node counts from its top row.
    depth: Box<[u8]>,
    /// A cell's edge in blocks (`2^level`).
    cell: u32,
}

impl Water {
    /// The see-through fluid of `padded` (cells of level `level`), `None` if it has none.
    fn of(padded: &PaddedChunk, materials: &LodMaterials, level: u8) -> Option<Self> {
        let clear: Box<[u64]> = (0..PADDED_AREA)
            .map(|row| {
                let voxels = &padded.voxels[row * PADDED..(row + 1) * PADDED];
                voxels.iter().enumerate().fold(0, |bits, (z, &m)| {
                    bits | u64::from(materials.see_through(m)) << z
                })
            })
            .collect();
        if clear.iter().all(|&row| row == 0) {
            return None;
        }
        // Column by column from the top: a cell under a counted cell counts one more, the top
        // cell of a see-through fluid counts 1, air and anything else 0.
        let mut depth = vec![0u8; PADDED_VOLUME].into_boxed_slice();
        for x in -1..=NODE {
            for z in -1..=NODE {
                let mut above = 0u8;
                for y in (-1..=NODE).rev() {
                    let i = PaddedChunk::index(x, y, z);
                    let row = (x + 1) as usize + (y + 1) as usize * PADDED;
                    let d = if padded.voxels[i] == 0 {
                        0
                    } else if above > 0 {
                        above.saturating_add(1)
                    } else {
                        u8::from(clear[row] & 1 << (z + 1) != 0)
                    };
                    depth[i] = d;
                    above = d;
                }
            }
        }
        Some(Self {
            clear,
            depth,
            cell: 1 << level,
        })
    }

    /// The darkness ([`dark_of`]) of a face whose front cell is padded cell `c`: the blocks of
    /// fluid over that cell, `n s - s/2` for `n` cells of edge `s` (the true surface and bed lie
    /// inside their cells, half a cell on average), at most [`MAX_DARK`].
    fn dark(&self, c: [i32; 3]) -> u8 {
        let n = u32::from(self.depth[PaddedChunk::index(c[0], c[1], c[2])]);
        let blocks = (n * self.cell).saturating_sub(self.cell / 2);
        blocks.min(u32::from(MAX_DARK)) as u8
    }
}

/// [`add_underwater`]'s faces into `out`.
fn underwater(
    padded: &PaddedChunk,
    materials: &LodMaterials,
    water: &Water,
    out: &mut Vec<Placed>,
) {
    // The node's own cells (padded bits 1 to 32 of a row).
    const INNER: u64 = ((1 << NODE) - 1) << 1;
    let row = |x: i32, y: i32| (x + 1) as usize + (y + 1) as usize * PADDED;
    for face in Face::ALL {
        let (na, ua, va) = face.axes();
        let step = face_step(face);
        let mut planes: Vec<Option<Box<[[u32; 32]; 32]>>> = (0..NODE).map(|_| None).collect();
        for y in 0..NODE {
            for x in 0..NODE {
                let r = row(x, y);
                let ground = (padded.opaque[r] | padded.transparent[r] | padded.special[r])
                    & !water.clear[r];
                let across = match face {
                    Face::PosX => water.clear[row(x + 1, y)],
                    Face::NegX => water.clear[row(x - 1, y)],
                    Face::PosY => water.clear[row(x, y + 1)],
                    Face::NegY => water.clear[row(x, y - 1)],
                    Face::PosZ => water.clear[r] >> 1,
                    Face::NegZ => water.clear[r] << 1,
                };
                let mut bits = ground & across & INNER;
                while bits != 0 {
                    let z = bits.trailing_zeros() as i32 - 1;
                    bits &= bits - 1;
                    let c = [x, y, z];
                    let key = face_key(padded.get(x, y, z), water.dark(add(c, step)));
                    let grid =
                        planes[c[na] as usize].get_or_insert_with(|| Box::new([[0; 32]; 32]));
                    grid[c[va] as usize][c[ua] as usize] = key;
                }
            }
        }
        for (n, grid) in planes.iter().enumerate() {
            if let Some(grid) = grid {
                merge_grid(grid, face, n as i32, materials.table(), Seam::None, out);
            }
        }
    }
}

/// [`add_skirts`]'s quads into `out`.
fn skirts(
    padded: &PaddedChunk,
    materials: &LodMaterials,
    water: Option<&Water>,
    out: &mut Vec<Placed>,
) {
    let near_air = near_air(padded);
    for (face, n) in plane_faces() {
        let step = face_step(face);
        let grid = face_grid(face, n, padded, |c, across| {
            let m = padded.get(c[0], c[1], c[2]);
            if m == 0 || across == 0 {
                None
            } else if materials.see_through(m) {
                // The top cell of the fluid: air right above it.
                (padded.get(c[0], c[1] + 1, c[2]) == 0).then(|| face_key(m, 0))
            } else if !materials.see_through(across) && is_near(&near_air, c) {
                let dark = water.map_or(0, |w| w.dark(add(c, step)));
                Some(face_key(m, dark))
            } else {
                None
            }
        });
        merge_grid(&grid, face, n, materials.table(), seam(n), out);
    }
}

/// [`add_fluid_walls`]'s quads into `out`.
fn walls(padded: &PaddedChunk, materials: &LodMaterials, water: &Water, out: &mut Vec<Placed>) {
    let near_air = near_air(padded);
    let near_water = within_two(|row| water.clear[row]);
    for (face, n) in plane_faces() {
        let step = face_step(face);
        let grid = face_grid(face, n, padded, |c, across| {
            let m = padded.get(c[0], c[1], c[2]);
            let wall = m != 0
                && !materials.see_through(m)
                && across != 0
                && !materials.see_through(across)
                && is_near(&near_water, c)
                && !is_near(&near_air, c);
            wall.then(|| face_key(m, water.dark(add(c, step))))
        });
        merge_grid(&grid, face, n, materials.table(), seam(n), out);
    }
}

/// The faces post-passes 3 and 4 add, each with the cell coordinate along its normal: on plane
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

/// The 32x32 grid (`[v][u]`, by [`Face::axes`]) of the face keys ([`face_key`], 0 for none) that
/// `pick(cell, material across)` gives the cells of `face` at normal coordinate `n`.
fn face_grid(
    face: Face,
    n: i32,
    padded: &PaddedChunk,
    mut pick: impl FnMut([i32; 3], u16) -> Option<u32>,
) -> Box<[[u32; 32]; 32]> {
    let (na, ua, va) = face.axes();
    let step = face_step(face);
    let mut grid = Box::new([[0u32; 32]; 32]);
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

/// Greedy rectangles of equal face key in `grid` (0 = none), never across an octant border
/// (cell 16 of u or v), as quads of `face` at normal coordinate `n` marked `seam`, with their
/// material's layer and emissive flag and their key's darkness, each with its bucket (by its
/// material and face: a see-through fluid's is the translucent one).
fn merge_grid(
    grid: &[[u32; 32]; 32],
    face: Face,
    n: i32,
    table: &MaterialTable,
    seam: Seam,
    out: &mut Vec<Placed>,
) {
    let (na, ua, va) = face.axes();
    let mut used = [0u32; 32];
    for v0 in 0..32usize {
        let mut u0 = 0usize;
        while u0 < 32 {
            let key = grid[v0][u0];
            if key == 0 || used[v0] & (1 << u0) != 0 {
                u0 += 1;
                continue;
            }
            // Along u, inside the octant half of u0.
            let u_end = (u0 | 15) + 1;
            let mut u1 = u0 + 1;
            while u1 < u_end && grid[v0][u1] == key && used[v0] & (1 << u1) == 0 {
                u1 += 1;
            }
            let run = (((1u64 << (u1 - u0)) - 1) << u0) as u32;
            // Along v, while the whole run matches, inside the octant half of v0.
            let v_end = (v0 | 15) + 1;
            let mut v1 = v0 + 1;
            while v1 < v_end && used[v1] & run == 0 && grid[v1][u0..u1].iter().all(|&g| g == key) {
                v1 += 1;
            }
            for row in &mut used[v0..v1] {
                *row |= run;
            }
            let (m, dark) = (key as u16, (key >> 16) as u16);
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
            .with_light_index(seam as u16 | dark << DARK_SHIFT);
            out.push((table.bucket(m, face), quad));
            u0 = u1;
        }
    }
}

/// Per padded row (`(x + 1) + (y + 1) * 34`, bit `z + 1`): an air cell (no voxel) lies within 2
/// cells, Chebyshev, inside the padded grid.
fn near_air(padded: &PaddedChunk) -> Box<[u64]> {
    within_two(|row| !(padded.opaque[row] | padded.transparent[row] | padded.special[row]))
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

/// Adds `extra` cube quads (open sky, marked with their [`Seam`] and darkness) to `mesh`, each
/// in its bucket, every bucket in [`Seam`] order (stable: the quads of a seam keep theirs).
fn append(mesh: &mut ChunkMesh, extra: Vec<Placed>) {
    if extra.is_empty() {
        return;
    }
    let mut buckets: [Vec<PackedQuad>; BUCKET_COUNT] = Default::default();
    for (b, out) in buckets.iter_mut().enumerate() {
        out.extend_from_slice(mesh.bucket(b));
    }
    for (b, q) in extra {
        buckets[b].push(q);
    }
    for quads in &mut buckets {
        quads.sort_by_key(|&q| seam_of(q));
    }
    set_buckets(mesh, buckets);
}

/// Writes `buckets` into `mesh` bucket-contiguous, with the palette [`LOD_PALETTE`] (open sky:
/// what the mesher gives a mesh without light, for every [`Seam`] and darkness) and a new hash.
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
