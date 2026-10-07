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
/// binary-greedy-meshing needs and a third one of the non-cube blocks. Layout = bgm `pad_linearize`: index `(z+1) + (x+1)*34 + (y+1)*34^2`;
/// mask row `(x+1) + (y+1)*34`, bit `z+1`.
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
        }
    }
    /// Resets to empty.
    pub fn clear(&mut self) {
        self.voxels.fill(0);
        self.opaque.fill(0);
        self.transparent.fill(0);
        self.special.fill(0);
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
        match class {
            VoxelClass::Empty => self.voxels[i] = 0,
            VoxelClass::Opaque => self.opaque[row] |= bit,
            VoxelClass::Transparent => self.transparent[row] |= bit,
            VoxelClass::Special => self.special[row] |= bit,
        }
    }
    /// Material at `(x, y, z)` in `-1..=32`.
    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> u16 {
        self.voxels[Self::index(x, y, z)]
    }
}
