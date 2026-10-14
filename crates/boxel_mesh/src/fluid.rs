//! Fluid corner heights (ADR 0021): the top of a fluid surface cell has a height per corner,
//! the average of the fluid around that corner, as in Minecraft's liquid renderer. A cell whose
//! 4 corners are all at the flat surface height ([`FLAT`], 14/16) stays in the greedy pass
//! (`SHAPE_FLUID_SURFACE`), so a calm lake costs what it did; every other surface cell (flowing
//! water, the edge of a slope) leaves the greedy output and is drawn one block at a time with
//! `SHAPE_FLUID_SLOPE`, its 4 heights in the quad.
//!
//! The rule, per corner of a surface cell, from the heights of the 4 cells around the corner
//! (in 1/128 of a block):
//! - A cell of the same fluid with that fluid or an opaque block above it is full (128); a
//!   source or falling fluid is [`SOURCE`] (112 = 14/16); flowing level `l` (1..=7) is
//!   `(8 - l) * 14`, Minecraft's `(8 - l) / 9` scaled so that a source sits at 14/16. Air and
//!   plants are 0; anything else (a solid block, glass) does not count.
//! - If either side cell is full, the corner is full. The diagonal cell counts only when a side
//!   cell has fluid (and makes the corner full if it is full).
//! - The corner is the weighted mean of the counted cells: weight 10 for source and full
//!   heights, 1 for the others. Rounded to sixteenths, at least 1/16.
//!
//! Two fluid cells that share a corner see the same 4 cells there and count the same ones (both
//! have fluid, so the diagonal counts for both), so their tops meet without cracks; the shared
//! edge of two cells has the same two corners. Both hold across chunk borders: the padding has
//! every cell a corner of the inner chunk reads (and the cell above each).
//!
//! The water of a waterlogged block (ADR 0025) is a source cell like any other for its
//! neighbors, but always drawn one block at a time: a surface cell with its corner heights, any
//! other one full (16/16 corners). Its faces against the block's sturdy faces (a slab's bottom,
//! the back of stairs) are not drawn and the others carry [`layout::GEO_FLUID_INSET_BIT`].
//!
//! Textures (ADR 0025): the side faces show the fluid's flow texture (its side texture), and
//! so does the top of a cell whose corners slope ([`fluid_flow`]); a level top shows the still
//! one. The shader scrolls the flow texture with the time.
use crate::greedy::Greedy;
use crate::mesher::{class_from_masks, front_nb9, mask_row, padded_pos};
use crate::packed::layout;
use crate::shading::PaletteFull;
use crate::{
    FLUID_SURFACE_BIT, MaterialTable, PADDED, PADDED_AREA, PADDED_VOLUME, PackedQuad, PaddedChunk,
    VoxelClass,
};
use boxel_voxel::{BlockModel, CHUNK_SIZE, Face};
use std::collections::BTreeMap;

/// Height of a full fluid cell (fluid or an opaque block above it), in 1/128 of a block.
const FULL: i32 = 128;
/// Height of a source (and of falling fluid) at the surface: 14/16.
const SOURCE: i32 = 112;
/// A cell that does not count in a corner's mean (solid blocks, glass).
const IGNORED: i32 = -1;
/// The corner height of a flat surface, in sixteenths (`SOURCE / 8`).
pub const FLAT: u8 = 14;
/// The level of a waterlogged block's cell in [`FluidScratch::levels`] while finding the sloped
/// cells: a source for its neighbors' heights, left to the waterlogged pass by the surface pass.
const WATERLOGGED: u8 = u8::MAX;

/// The direction the top of a fluid cell with these corner heights flows (sixteenths, corner
/// `k` at `(x, z) = (k & 1, k >> 1)`): downhill along x and along z, each the height of the low
/// side's two corners less the high side's. `[0, 0]` for a level top: it shows the still texture,
/// any other the flow texture scrolled this way (ADR 0025). The shaders compute the same.
pub const fn fluid_flow(h: [u8; 4]) -> [i32; 2] {
    let [a, b, c, d] = [h[0] as i32, h[1] as i32, h[2] as i32, h[3] as i32];
    [(a + c) - (b + d), (a + b) - (c + d)]
}

/// A surface cell drawn with its own corner heights.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SlopedCell {
    /// Inner chunk position.
    pos: [u32; 3],
    /// Its fluid's material.
    material: u16,
    /// Corner heights in sixteenths (`1..=16`), corner `k` at `(x, z) = (k & 1, k >> 1)`.
    heights: [u8; 4],
    /// The faces the greedy culling left visible (bit `Face::index`).
    faces: u8,
    /// The water of a waterlogged block ([`layout::GEO_FLUID_INSET_BIT`]).
    inset: bool,
}

/// Scratch of the fluid pass: the dense levels of the padded chunk (all 0 between meshes) and
/// the sloped cells of the last mesh.
pub(crate) struct FluidScratch {
    levels: Box<[u8]>,
    /// [`Heights::layer`].
    layer: Box<[i32]>,
    pub(crate) cells: Vec<SlopedCell>,
}

impl FluidScratch {
    pub(crate) fn new() -> Self {
        Self {
            levels: vec![0; PADDED_VOLUME].into_boxed_slice(),
            layer: vec![UNKNOWN; PADDED * PADDED].into_boxed_slice(),
            cells: Vec::new(),
        }
    }
}

/// Not computed yet (in [`Heights::layer`]).
const UNKNOWN: i32 = i32::MIN;

/// The heights of the cells of one padded chunk, memoized for one layer and fluid at a time (each
/// cell is a neighbor of up to 8 surface cells).
struct Heights<'a> {
    padded: &'a PaddedChunk,
    materials: &'a MaterialTable,
    levels: &'a [u8],
    /// The layer (y) and fluid of `layer`.
    key: Option<(i32, u16)>,
    /// Heights of the padded cells of that layer, index `(x + 1) + (z + 1) * PADDED`.
    layer: Box<[i32]>,
}

impl Heights<'_> {
    /// [`Heights::of`], memoized per layer and fluid.
    fn at(&mut self, fluid: u16, x: i32, y: i32, z: i32) -> i32 {
        if self.key != Some((y, fluid)) {
            self.key = Some((y, fluid));
            self.layer.fill(UNKNOWN);
        }
        let i = (x + 1) as usize + (z + 1) as usize * PADDED;
        if self.layer[i] == UNKNOWN {
            self.layer[i] = self.of(fluid, x, y, z);
        }
        self.layer[i]
    }

    /// Height of padded cell `(x, y, z)` (y in `0..32`) seen from fluid `fluid`.
    fn of(&self, fluid: u16, x: i32, y: i32, z: i32) -> i32 {
        let i = PaddedChunk::index(x, y, z);
        let id = self.padded.voxels[i] & !FLUID_SURFACE_BIT;
        if id == fluid {
            let above = self.padded.voxels[i + PADDED_AREA] & !FLUID_SURFACE_BIT;
            if above == fluid || class_from_masks(self.padded, x, y + 1, z) == VoxelClass::Opaque {
                return FULL;
            }
            return match self.levels[i] {
                0 | 8.. => SOURCE,
                level => (8 - i32::from(level)) * 14,
            };
        }
        match class_from_masks(self.padded, x, y, z) {
            VoxelClass::Empty => 0,
            VoxelClass::Special | VoxelClass::Transparent => match self.materials.model(id) {
                BlockModel::Cross | BlockModel::Fluid => 0,
                _ => IGNORED,
            },
            VoxelClass::Opaque => IGNORED,
        }
    }

    /// Corner `k` (`(x, z) = (k & 1, k >> 1)`) of the top of fluid cell `(x, y, z)`, whose own
    /// height is `own`, in sixteenths.
    fn corner(&mut self, fluid: u16, [x, y, z]: [i32; 3], own: i32, k: usize) -> u8 {
        let dx = if k & 1 == 1 { 1 } else { -1 };
        let dz = if k >> 1 == 1 { 1 } else { -1 };
        let side_x = self.at(fluid, x + dx, y, z);
        let side_z = self.at(fluid, x, y, z + dz);
        if side_x >= FULL || side_z >= FULL {
            return 16;
        }
        let (mut sum, mut weight) = (0, 0);
        let mut add = |h: i32| {
            if h >= SOURCE {
                sum += h * 10;
                weight += 10;
            } else if h >= 0 {
                sum += h;
                weight += 1;
            }
        };
        if side_x > 0 || side_z > 0 {
            let diagonal = self.at(fluid, x + dx, y, z + dz);
            if diagonal >= FULL {
                return 16;
            }
            add(diagonal);
        }
        add(own);
        add(side_z);
        add(side_x);
        // Rounded to sixteenths (8 units); `weight >= 1`: the own cell always counts.
        ((sum + 4 * weight) / (8 * weight)).clamp(1, 16) as u8
    }

    /// The 4 corner heights of surface cell `(x, y, z)` of fluid `fluid`.
    fn corners(&mut self, fluid: u16, p: [i32; 3]) -> [u8; 4] {
        let own = self.at(fluid, p[0], p[1], p[2]);
        std::array::from_fn(|k| self.corner(fluid, p, own, k))
    }
}

/// Finds the surface cells of `padded` whose top is not flat, takes their faces out of the
/// greedy culling (so the greedy pass does not draw them; their neighbors still cull against
/// them) and keeps them in `scratch.cells`. Needs the surface marks (`FLUID_SURFACE_BIT`); a
/// chunk without marks has no sloped cells.
pub(crate) fn find_sloped(
    padded: &PaddedChunk,
    materials: &MaterialTable,
    greedy: &mut Greedy,
    scratch: &mut FluidScratch,
) {
    scratch.cells.clear();
    if padded.cull_mask == u16::MAX {
        return;
    }
    for &(i, level) in &padded.fluid_levels {
        scratch.levels[i as usize] = level;
    }
    for &(i, _) in &padded.waterlogged {
        scratch.levels[i as usize] = WATERLOGGED;
    }
    let mut heights = Heights {
        padded,
        materials,
        levels: &scratch.levels,
        key: None,
        layer: std::mem::take(&mut scratch.layer),
    };
    for y in 0..CHUNK_SIZE {
        let Some(flat) = flat_cells(padded, materials, &scratch.levels, y) else {
            continue;
        };
        for x in 0..CHUNK_SIZE {
            // Inner voxels only (mask bit of padded z is z + 1), minus the surely flat ones.
            let row = (x + 1) as usize;
            let mut bits = (padded.transparent[mask_row(x, y)] & !flat[row]) >> 1;
            bits &= u64::from(u32::MAX);
            while bits != 0 {
                let z = bits.trailing_zeros() as i32;
                bits &= bits - 1;
                let i = PaddedChunk::index(x, y, z);
                let id = padded.voxels[i];
                if id & FLUID_SURFACE_BIT == 0 || scratch.levels[i] == WATERLOGGED {
                    continue;
                }
                let fluid = id & !FLUID_SURFACE_BIT;
                let corners = heights.corners(fluid, [x, y, z]);
                if corners == [FLAT; 4] {
                    continue;
                }
                let pos = [x as u32, y as u32, z as u32];
                scratch.cells.push(SlopedCell {
                    pos,
                    material: fluid,
                    heights: corners,
                    faces: greedy.take_faces(pos),
                    inset: false,
                });
            }
        }
    }
    // The water of the waterlogged blocks, every one (surface or not) by itself.
    for &(i, block) in &padded.waterlogged {
        let [x, y, z] = padded_pos(i as usize);
        if ![x, y, z].iter().all(|c| (0..CHUNK_SIZE).contains(c)) {
            continue;
        }
        let id = padded.voxels[i as usize];
        let fluid = id & !FLUID_SURFACE_BIT;
        let corners = if id & FLUID_SURFACE_BIT != 0 {
            heights.corners(fluid, [x, y, z])
        } else {
            [16; 4]
        };
        let pos = [x as u32, y as u32, z as u32];
        scratch.cells.push(SlopedCell {
            pos,
            material: fluid,
            heights: corners,
            faces: greedy.take_faces(pos) & !materials.sturdy_faces(block),
            inset: true,
        });
    }
    scratch.layer = heights.layer;
    for &(i, _) in &padded.fluid_levels {
        scratch.levels[i as usize] = 0;
    }
    for &(i, _) in &padded.waterlogged {
        scratch.levels[i as usize] = 0;
    }
}

/// The cells of layer `y` that are surely flat, as padded column masks per padded x (bit `z + 1`):
/// a source-height surface cell whose 8 neighbors are all source-height surface cells of the same
/// fluid or opaque. Every corner then averages sources only: exactly 14/16, with no need to look
/// at it. `None` when the layer has no surface cell. With two fluids in the layer nothing is surely
/// flat (all zero).
fn flat_cells(
    padded: &PaddedChunk,
    materials: &MaterialTable,
    levels: &[u8],
    y: i32,
) -> Option<[u64; PADDED]> {
    let mut good = [0u64; PADDED];
    let mut fluid = None;
    let mut any_surface = false;
    for x in -1..=CHUNK_SIZE {
        let row = mask_row(x, y);
        let mut bits = padded.transparent[row];
        let mut surface = 0u64;
        while bits != 0 {
            let bit = bits.trailing_zeros();
            bits &= bits - 1;
            let i = PaddedChunk::index(x, y, bit as i32 - 1);
            let id = padded.voxels[i];
            let m = id & !FLUID_SURFACE_BIT;
            any_surface |= id & FLUID_SURFACE_BIT != 0;
            if materials.model(m) != BlockModel::Fluid {
                continue;
            }
            match fluid {
                None => fluid = Some(m),
                Some(f) if f != m => return any_layer(padded, y).then_some([0; PADDED]),
                Some(_) => {}
            }
            let above = padded.voxels[i + PADDED_AREA] & !FLUID_SURFACE_BIT;
            let full = above == m || padded.opaque[mask_row(x, y + 1)] >> bit & 1 != 0;
            let source = matches!(levels[i], 0 | 8..);
            if !full && source {
                surface |= 1 << bit;
            }
        }
        good[(x + 1) as usize] = surface | padded.opaque[row];
    }
    if !any_surface {
        return None;
    }
    let mut flat = [0u64; PADDED];
    for x in 1..PADDED - 1 {
        let a = good[x - 1] & good[x] & good[x + 1];
        flat[x] = a & (a << 1) & (a >> 1) & !padded.opaque[mask_row(x as i32 - 1, y)];
    }
    Some(flat)
}

/// True if inner layer `y` has a fluid surface cell.
fn any_layer(padded: &PaddedChunk, y: i32) -> bool {
    (0..CHUNK_SIZE).any(|x| {
        let mut bits = (padded.transparent[mask_row(x, y)] >> 1) & u64::from(u32::MAX);
        while bits != 0 {
            let z = bits.trailing_zeros() as i32;
            bits &= bits - 1;
            if padded.voxels[PaddedChunk::index(x, y, z)] & FLUID_SURFACE_BIT != 0 {
                return true;
            }
        }
        false
    })
}

/// Appends the quads of the sloped cells to `out` with their buckets
/// ([`MaterialTable::sloped_bucket`]): one `SHAPE_FLUID_SLOPE` quad per visible face.
/// `shade(material, front, face, nb9)` gives the light palette index (and an AO byte, unused: the
/// heights take its bits) as for the cube face of the fluid's material in its place.
pub(crate) fn emit_sloped(
    cells: &[SlopedCell],
    padded: &PaddedChunk,
    materials: &MaterialTable,
    out: &mut Vec<(u8, PackedQuad)>,
    mut shade: impl FnMut(u16, usize, Face, Option<u32>) -> Result<(u8, u16), PaletteFull>,
) -> Result<(), PaletteFull> {
    for cell in cells {
        let [x, y, z] = cell.pos;
        let m = cell.material;
        let flowing = fluid_flow(cell.heights) != [0, 0];
        for face in Face::ALL {
            if cell.faces & (1 << face.index()) == 0 {
                continue;
            }
            let n = face.normal();
            let front = PaddedChunk::index(x as i32 + n.x, y as i32 + n.y, z as i32 + n.z);
            let nb9 = front_nb9(face, &padded.opaque, x as usize, y as usize, z as usize);
            let (_, light) = shade(m, front, face, Some(nb9))?;
            // A sloping top shows the flow texture (the fluid's side texture).
            let texture = if face == Face::PosY && flowing {
                Face::PosX
            } else {
                face
            };
            let mut quad = PackedQuad::new(cell.pos, 1, 1, face, materials.layer(m, texture))
                .with_light_index(light)
                .with_fluid_heights(cell.heights);
            if cell.inset {
                quad = quad.with_fluid_inset();
            }
            if materials.emissive(m) {
                quad.geo |= layout::FLAG_EMISSIVE << layout::GEO_FLAGS_SHIFT;
            }
            out.push((materials.sloped_bucket(m, face, cell.heights) as u8, quad));
        }
    }
    Ok(())
}

/// Brute-force oracle of the corner heights (ADR 0021): every fluid surface cell of the inner
/// 32^3 of a gathered chunk (marked with `FLUID_SURFACE_BIT`) and its 4 corner heights in
/// sixteenths, from Minecraft's rule written out in floating point (heights in blocks, a source
/// at 14/16, flowing level `l` at `(8 - l) / 8` of that; weights 10 at or above a source, 1
/// below; a solid neighbor is left out). The mesher draws a cell with `SHAPE_FLUID_SLOPE` and
/// these heights unless all 4 are 14 (then it stays a greedy `SHAPE_FLUID_SURFACE` cell).
pub fn reference_fluid_heights(
    padded: &PaddedChunk,
    materials: &MaterialTable,
) -> BTreeMap<[u32; 3], [u8; 4]> {
    let level_at = |x: i32, y: i32, z: i32| {
        let i = PaddedChunk::index(x, y, z) as u32;
        padded
            .fluid_levels
            .iter()
            .rev()
            .find(|&&(j, _)| j == i)
            .map_or(0, |&(_, l)| l)
    };
    // Height in blocks; `None` does not count.
    let height = |fluid: u16, x: i32, y: i32, z: i32| -> Option<f64> {
        let id = padded.get(x, y, z) & !FLUID_SURFACE_BIT;
        let class = |x, y, z| class_from_masks(padded, x, y, z);
        if id == fluid {
            let above = padded.get(x, y + 1, z) & !FLUID_SURFACE_BIT;
            if above == fluid || class(x, y + 1, z) == VoxelClass::Opaque {
                return Some(1.0);
            }
            let level = level_at(x, y, z);
            let amount = if level == 0 || level >= 8 {
                8
            } else {
                8 - level
            };
            return Some(f64::from(amount) / 8.0 * 0.875);
        }
        let model = materials.model(id);
        match class(x, y, z) {
            VoxelClass::Empty => Some(0.0),
            VoxelClass::Special | VoxelClass::Transparent
                if matches!(model, BlockModel::Cross | BlockModel::Fluid) =>
            {
                Some(0.0)
            }
            _ => None,
        }
    };
    let mut out = BTreeMap::new();
    for y in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                let id = padded.get(x, y, z);
                if padded.cull_mask == u16::MAX || id & FLUID_SURFACE_BIT == 0 {
                    continue;
                }
                let fluid = id & !FLUID_SURFACE_BIT;
                let own = height(fluid, x, y, z).expect("a fluid cell");
                let corners = std::array::from_fn(|k| {
                    let (dx, dz) = (
                        if k & 1 == 1 { 1 } else { -1 },
                        if k >> 1 == 1 { 1 } else { -1 },
                    );
                    let a = height(fluid, x + dx, y, z);
                    let b = height(fluid, x, y, z + dz);
                    if a == Some(1.0) || b == Some(1.0) {
                        return 16;
                    }
                    let mut counted = vec![own];
                    counted.extend(a);
                    counted.extend(b);
                    if a.unwrap_or(0.0) > 0.0 || b.unwrap_or(0.0) > 0.0 {
                        let d = height(fluid, x + dx, y, z + dz);
                        if d == Some(1.0) {
                            return 16;
                        }
                        counted.extend(d);
                    }
                    let weight = |h: f64| if h >= 0.875 { 10.0 } else { 1.0 };
                    let sum: f64 = counted.iter().map(|&h| h * weight(h)).sum();
                    let total: f64 = counted.iter().map(|&h| weight(h)).sum();
                    ((sum / total * 16.0).round() as u8).clamp(1, 16)
                });
                out.insert([x as u32, y as u32, z as u32], corners);
            }
        }
    }
    out
}
