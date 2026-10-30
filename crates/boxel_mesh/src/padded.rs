/// Padded edge: 32 + 2 border voxels (fits one `u64` column mask).
pub const PADDED: usize = 34;
/// `PADDED^2`.
pub const PADDED_AREA: usize = PADDED * PADDED;
/// `PADDED^3`.
pub const PADDED_VOLUME: usize = PADDED_AREA * PADDED;

/// Mesher classification of a voxel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoxelClass {
    /// No geometry (air).
    Empty,
    /// Hides neighbor faces.
    Opaque,
    /// Drawn, does not hide neighbors (cutout and translucent).
    Transparent,
    /// A block that is not a cube (slabs, plants, ADR 0018): meshed one block at a time after
    /// the greedy pass, which sees it as empty (its neighbors draw their faces against it). Its
    /// material id stays in `voxels` and it has a bit in the `special` masks.
    Special,
}

/// The mesher's padded input: material ids for x, y, z in `-1..=32` plus the two column masks
/// binary-greedy-meshing needs, a third one of the non-cube blocks and a fourth one of the fluid
/// surface cells. Layout = bgm `pad_linearize`: index `(z+1) + (x+1)*34 + (y+1)*34^2`;
/// mask row `(x+1) + (y+1)*34`, bit `z+1` (so the row of index `i` is `i / 34`, its bit `i % 34`).
#[derive(Clone, Debug)]
pub struct PaddedChunk {
    /// Material ids (`0` = empty).
    pub voxels: Box<[u16]>,
    /// Opaque column masks.
    pub opaque: Box<[u64]>,
    /// Transparent column masks.
    pub transparent: Box<[u64]>,
    /// Special (non-cube) column masks.
    pub special: Box<[u64]>,
    /// Fluid surface cell column masks: a fluid (`BlockModel::Fluid`) of the inner 32^3 under
    /// neither its own fluid nor an opaque block, whose top sits lower
    /// (`layout::SHAPE_FLUID_SURFACE`). Set by the mesher's gather (ADR 0043; before, the top bit
    /// of the voxel id). Culling ignores it (a surface cell and the rest of its fluid hide the
    /// faces between them), merging does not.
    pub surface: Box<[u64]>,
    /// The fluid cells whose level is not 0 (flowing or falling water, ADR 0021): padded index
    /// and level. Every level of a fluid has the material of its source in `voxels` (so they
    /// cull and merge as one fluid); the mesher reads the levels here for the corner heights.
    pub fluid_levels: Vec<(u32, u8)>,
    /// The waterlogged blocks (ADR 0025): padded index and the block's material, sorted by
    /// index. Their cell in `voxels` holds their water (a transparent fluid source), so it culls
    /// and slopes as water; the mesher draws the block's boxes, or the faces of a cutout cube
    /// (ADR 0038), from here.
    pub waterlogged: Vec<(u32, u16)>,
    /// The inner 32^3 may hold a lit emitter (`MaterialTable::lit_emission`, ADR 0036), whose
    /// faces the mesher draws flat. Gather sets it from the center chunk's palette; `new` and
    /// `clear` set it, so a chunk filled by hand is always looked at. When it is false the
    /// mesher never asks a voxel whether it is one.
    pub lit_emitters: bool,
    /// Per side border (-X, +X, -Z, +Z) and column of its padded layer (along z for the x
    /// borders, along x for the z ones): the material of the lowest opaque cube over the padded
    /// row on top, in the chunk over the neighbor across (its rows 1 to 31; 0: none). The edge
    /// walls' caps read it (`MeshOptions::walls`): a cave across the border whose ceiling stands
    /// over the padding. Gather sets it; `new` and `clear` zero it.
    pub above: [[u16; 32]; 4],
}

impl Default for PaddedChunk {
    fn default() -> Self {
        Self::new()
    }
}

impl PaddedChunk {
    /// All empty.
    pub fn new() -> Self {
        Self {
            voxels: vec![0; PADDED_VOLUME].into_boxed_slice(),
            opaque: vec![0; PADDED_AREA].into_boxed_slice(),
            transparent: vec![0; PADDED_AREA].into_boxed_slice(),
            special: vec![0; PADDED_AREA].into_boxed_slice(),
            surface: vec![0; PADDED_AREA].into_boxed_slice(),
            fluid_levels: Vec::new(),
            waterlogged: Vec::new(),
            lit_emitters: true,
            above: [[0; 32]; 4],
        }
    }
    /// Resets to empty.
    pub fn clear(&mut self) {
        self.voxels.fill(0);
        self.opaque.fill(0);
        self.transparent.fill(0);
        self.special.fill(0);
        self.surface.fill(0);
        self.fluid_levels.clear();
        self.waterlogged.clear();
        self.lit_emitters = true;
        self.above = [[0; 32]; 4];
    }
    /// Linear index of chunk-local `(x, y, z)`, each in `-1..=32`.
    #[inline]
    pub const fn index(x: i32, y: i32, z: i32) -> usize {
        (z + 1) as usize + (x + 1) as usize * PADDED + (y + 1) as usize * PADDED_AREA
    }
    /// Writes one voxel and its mask bits.
    #[inline]
    pub fn set(&mut self, x: i32, y: i32, z: i32, material: u16, class: VoxelClass) {
        let i = Self::index(x, y, z);
        self.voxels[i] = material;
        let row = (x + 1) as usize + (y + 1) as usize * PADDED;
        let bit = 1u64 << (z + 1);
        self.opaque[row] &= !bit;
        self.transparent[row] &= !bit;
        self.special[row] &= !bit;
        self.surface[row] &= !bit;
        match class {
            VoxelClass::Empty => self.voxels[i] = 0,
            VoxelClass::Opaque => self.opaque[row] |= bit,
            VoxelClass::Transparent => self.transparent[row] |= bit,
            VoxelClass::Special => self.special[row] |= bit,
        }
    }
    /// Writes a fluid voxel of `level` (`0` = source): `material` is the fluid's material (the
    /// one of its source) with its class, and a level other than 0 goes to
    /// [`PaddedChunk::fluid_levels`]. What `gather` does for a fluid state.
    pub fn set_fluid(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        material: u16,
        class: VoxelClass,
        level: u8,
    ) {
        self.set(x, y, z, material, class);
        let i = Self::index(x, y, z) as u32;
        self.fluid_levels.retain(|&(j, _)| j != i);
        if level != 0 {
            self.fluid_levels.push((i, level));
        }
    }
    /// Writes a waterlogged block (ADR 0025): `water` (a transparent fluid source material) in
    /// the cell and `block` in [`PaddedChunk::waterlogged`]. What `gather` does for a
    /// waterlogged state.
    pub fn set_waterlogged(&mut self, x: i32, y: i32, z: i32, block: u16, water: u16) {
        self.set(x, y, z, water, VoxelClass::Transparent);
        let i = Self::index(x, y, z) as u32;
        match self.waterlogged.binary_search_by_key(&i, |&(j, _)| j) {
            Ok(k) => self.waterlogged[k].1 = block,
            Err(k) => self.waterlogged.insert(k, (i, block)),
        }
    }
    /// The waterlogged block at padded index `i`, if any.
    #[inline]
    pub fn waterlogged_at(&self, i: usize) -> Option<u16> {
        let k = self
            .waterlogged
            .binary_search_by_key(&(i as u32), |&(j, _)| j)
            .ok()?;
        Some(self.waterlogged[k].1)
    }
    /// Material at `(x, y, z)` in `-1..=32`.
    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> u16 {
        self.voxels[Self::index(x, y, z)]
    }
    /// True if padded index `i` is a fluid surface cell ([`PaddedChunk::surface`]).
    #[inline]
    pub fn is_surface(&self, i: usize) -> bool {
        self.surface[i / PADDED] >> (i % PADDED) & 1 != 0
    }
    /// True if some cell is a fluid surface cell ([`PaddedChunk::surface`]).
    pub fn has_surface(&self) -> bool {
        self.surface.iter().any(|&row| row != 0)
    }
}
