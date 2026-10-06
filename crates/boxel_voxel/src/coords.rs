//! Chunk and block coordinates. Everything here is implemented by the skeleton (frozen).
//!
//! World integer coordinates (`ChunkPos`, `BlockPos`) are `i64` on every axis, bounded by the
//! world limit [`MAX_CHUNK_COORD`]. Chunk-local data (`LocalPos`, `CHUNK_SIZE`, offsets) stays
//! small and 32-bit.
use glam::{DVec3, I64Vec3, IVec3};

/// log2 of the chunk edge.
pub const CHUNK_SHIFT: u32 = 5;
/// Chunk edge in voxels (32).
pub const CHUNK_SIZE: i32 = 1 << CHUNK_SHIFT;
/// `CHUNK_SIZE - 1`, for local coordinates of negative positions.
pub const CHUNK_MASK: i32 = CHUNK_SIZE - 1;
/// Voxels per chunk layer (1024).
pub const CHUNK_AREA: usize = (CHUNK_SIZE * CHUNK_SIZE) as usize;
/// Voxels per chunk (32768).
pub const CHUNK_VOLUME: usize = CHUNK_AREA * CHUNK_SIZE as usize;

/// The world limit: lod-0 chunk coordinates live in `-MAX_CHUNK_COORD..=MAX_CHUNK_COORD` on
/// every axis (2^56 chunks, so voxel coordinates reach `+-2^61`, about `2.3e18`). At lod `k` the
/// limit is `MAX_CHUNK_COORD >> k` (the same voxels).
///
/// It is the only bound of the world integer coordinates and it is chosen so that nothing
/// overflows `i64` inside it, with margin:
/// - every origin (`chunk << (5 + lod)`) fits: at most `2^61`;
/// - every neighbor, radius or hysteresis offset (at most 4096 chunks each in the world crate)
///   and every difference of two positions fits;
/// - squared distances are computed in `u128` ([`ChunkPos::distance_sq`]).
///
/// The world clamps its viewers to it (`boxel_world` interest), so it never streams a chunk
/// outside; positions built by hand are not checked except where noted (`debug_assert`).
///
/// It is not the practical limit: `Transform` positions are `f64`, whose step reaches 1 mm at
/// about `4.4e12` voxels (`2^42`), 3 cm at `2^47` and a whole voxel at `2^52`, where a camera can
/// no longer move smoothly. Integer coordinates stay exact up to the world limit.
pub const MAX_CHUNK_COORD: i64 = 1 << 56;

/// Voxel coordinate of the world limit: `MAX_CHUNK_COORD << CHUNK_SHIFT` (`2^61`). Derived from
/// [`MAX_CHUNK_COORD`], not a separate bound: positions in `-MAX_BLOCK_COORD..MAX_BLOCK_COORD + 32`
/// belong to chunks inside the limit.
pub const MAX_BLOCK_COORD: i64 = MAX_CHUNK_COORD << CHUNK_SHIFT;

/// `|a - b|^2` without overflow (`u128`).
const fn sq_diff(a: i64, b: i64) -> u128 {
    let d = a.abs_diff(b) as u128;
    d * d
}

/// Chunk coordinates (world voxel = chunk * 32 + local). Ordered (x, y, z) for deterministic sorts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkPos {
    /// X.
    pub x: i64,
    /// Y.
    pub y: i64,
    /// Z.
    pub z: i64,
}

impl ChunkPos {
    /// New position.
    pub const fn new(x: i64, y: i64, z: i64) -> Self {
        Self { x, y, z }
    }
    /// From an `IVec3` (a small offset, or a position from 32-bit code).
    pub const fn from_ivec3(v: IVec3) -> Self {
        Self::new(v.x as i64, v.y as i64, v.z as i64)
    }
    /// From an `I64Vec3`.
    pub const fn from_i64vec3(v: I64Vec3) -> Self {
        Self::new(v.x, v.y, v.z)
    }
    /// As an `I64Vec3`.
    pub const fn as_i64vec3(self) -> I64Vec3 {
        I64Vec3::new(self.x, self.y, self.z)
    }
    /// World voxel position of the chunk's min corner (lod 0; a lod-`k` chunk starts at
    /// `pos << (5 + k)`). Exact for every chunk in `-2^58..2^58` (4 times the world limit: every
    /// chunk that contains an `i64` block); further out the shift would drop bits
    /// (debug-asserted).
    pub const fn origin(self) -> BlockPos {
        const fn shift(c: i64) -> i64 {
            let o = c << CHUNK_SHIFT;
            debug_assert!(o >> CHUNK_SHIFT == c, "chunk origin outside i64");
            o
        }
        BlockPos::new(shift(self.x), shift(self.y), shift(self.z))
    }
    /// Chunk containing `p`.
    pub const fn containing(p: BlockPos) -> Self {
        Self::new(p.x >> CHUNK_SHIFT, p.y >> CHUNK_SHIFT, p.z >> CHUNK_SHIFT)
    }
    /// Offset by `d` chunks (a neighbor or face offset). Cannot overflow within the world limit
    /// ([`MAX_CHUNK_COORD`] + any `i32` is far from `i64::MAX`); past `i64` it panics in debug.
    pub const fn offset(self, d: IVec3) -> Self {
        Self::new(
            self.x + d.x as i64,
            self.y + d.y as i64,
            self.z + d.z as i64,
        )
    }
    /// The 26 neighbors in [`NEIGHBOR_OFFSETS`] order.
    pub fn neighbors26(self) -> [ChunkPos; 26] {
        NEIGHBOR_OFFSETS.map(|d| self.offset(d))
    }
    /// This position clamped to the world limit ([`MAX_CHUNK_COORD`]) on every axis.
    pub const fn clamp_to_world(self) -> Self {
        const fn clamp(v: i64) -> i64 {
            if v < -MAX_CHUNK_COORD {
                -MAX_CHUNK_COORD
            } else if v > MAX_CHUNK_COORD {
                MAX_CHUNK_COORD
            } else {
                v
            }
        }
        Self::new(clamp(self.x), clamp(self.y), clamp(self.z))
    }
    /// Squared distance in chunks. Exact for any two positions within the world limit
    /// (differences up to `2^57`); saturates at `u128::MAX` only far outside it.
    pub const fn distance_sq(self, other: ChunkPos) -> u128 {
        sq_diff(self.x, other.x)
            .saturating_add(sq_diff(self.y, other.y))
            .saturating_add(sq_diff(self.z, other.z))
    }
    /// Squared horizontal (xz) distance in chunks. Exact within the world limit; saturates at
    /// `u128::MAX` only far outside it.
    pub const fn distance_sq_xz(self, other: ChunkPos) -> u128 {
        sq_diff(self.x, other.x).saturating_add(sq_diff(self.z, other.z))
    }
}

/// World voxel position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockPos {
    /// X.
    pub x: i64,
    /// Y.
    pub y: i64,
    /// Z.
    pub z: i64,
}

impl BlockPos {
    /// New position.
    pub const fn new(x: i64, y: i64, z: i64) -> Self {
        Self { x, y, z }
    }
    /// From an `I64Vec3`.
    pub const fn from_i64vec3(v: I64Vec3) -> Self {
        Self::new(v.x, v.y, v.z)
    }
    /// As an `I64Vec3`.
    pub const fn as_i64vec3(self) -> I64Vec3 {
        I64Vec3::new(self.x, self.y, self.z)
    }
    /// Voxel containing a world-space point (floor). Exact for every finite point within `i64`
    /// (`f64` integers are exact `i64` values there); saturates beyond it, NaN maps to 0.
    pub fn from_world(p: DVec3) -> Self {
        Self::from_i64vec3(p.floor().as_i64vec3())
    }
    /// Splits into chunk and local position (correct for negative coordinates).
    pub const fn split(self) -> (ChunkPos, LocalPos) {
        let mask = CHUNK_MASK as i64;
        (
            ChunkPos::containing(self),
            LocalPos::new(
                (self.x & mask) as u32,
                (self.y & mask) as u32,
                (self.z & mask) as u32,
            ),
        )
    }
    /// Inverse of [`BlockPos::split`]. Cannot overflow for chunks within the world limit.
    pub const fn from_parts(chunk: ChunkPos, local: LocalPos) -> Self {
        let o = chunk.origin();
        Self::new(
            o.x + local.x() as i64,
            o.y + local.y() as i64,
            o.z + local.z() as i64,
        )
    }
}

/// Position inside a chunk; packed index `(y << 10) | (x << 5) | z`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct LocalPos(u16);

impl LocalPos {
    /// From local coordinates in `0..32` (debug-asserted).
    pub const fn new(x: u32, y: u32, z: u32) -> Self {
        debug_assert!(x < 32 && y < 32 && z < 32);
        Self(((y << 10) | (x << 5) | z) as u16)
    }
    /// From a linear index in `0..CHUNK_VOLUME`.
    pub const fn from_index(i: usize) -> Self {
        debug_assert!(i < CHUNK_VOLUME);
        Self(i as u16)
    }
    /// Linear index.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
    /// X.
    pub const fn x(self) -> u32 {
        ((self.0 >> 5) & 31) as u32
    }
    /// Y.
    pub const fn y(self) -> u32 {
        (self.0 >> 10) as u32
    }
    /// Z.
    pub const fn z(self) -> u32 {
        (self.0 & 31) as u32
    }
    /// Faces of the chunk this voxel touches (bit `f` set = touches the chunk border on face `f`).
    pub const fn border_faces(self) -> u8 {
        let (x, y, z) = (self.x(), self.y(), self.z());
        ((y == 31) as u8)
            | ((y == 0) as u8) << 1
            | ((x == 31) as u8) << 2
            | ((x == 0) as u8) << 3
            | ((z == 31) as u8) << 4
            | ((z == 0) as u8) << 5
    }
}

/// Cube face / direction. Order and numbering are frozen (`PackedQuad`, shaders, mesher buckets):
/// `0 = +Y, 1 = -Y, 2 = +X, 3 = -X, 4 = +Z, 5 = -Z` (binary-greedy-meshing's Up, Down, Right,
/// Left, Front, Back).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Face {
    /// +Y.
    PosY = 0,
    /// -Y.
    NegY = 1,
    /// +X.
    PosX = 2,
    /// -X.
    NegX = 3,
    /// +Z.
    PosZ = 4,
    /// -Z.
    NegZ = 5,
}

impl Face {
    /// All faces in index order.
    pub const ALL: [Face; 6] = [
        Face::PosY,
        Face::NegY,
        Face::PosX,
        Face::NegX,
        Face::PosZ,
        Face::NegZ,
    ];
    /// From index `0..6`.
    pub const fn from_index(i: u8) -> Option<Face> {
        match i {
            0 => Some(Face::PosY),
            1 => Some(Face::NegY),
            2 => Some(Face::PosX),
            3 => Some(Face::NegX),
            4 => Some(Face::PosZ),
            5 => Some(Face::NegZ),
            _ => None,
        }
    }
    /// Index `0..6`.
    pub const fn index(self) -> usize {
        self as usize
    }
    /// Outward normal.
    pub const fn normal(self) -> IVec3 {
        match self {
            Face::PosY => IVec3::Y,
            Face::NegY => IVec3::NEG_Y,
            Face::PosX => IVec3::X,
            Face::NegX => IVec3::NEG_X,
            Face::PosZ => IVec3::Z,
            Face::NegZ => IVec3::NEG_Z,
        }
    }
    /// Opposite face.
    pub const fn opposite(self) -> Face {
        match self {
            Face::PosY => Face::NegY,
            Face::NegY => Face::PosY,
            Face::PosX => Face::NegX,
            Face::NegX => Face::PosX,
            Face::PosZ => Face::NegZ,
            Face::NegZ => Face::PosZ,
        }
    }
    /// Canonical (normal axis, u axis, v axis), 0 = X, 1 = Y, 2 = Z (`PackedQuad` w runs along u,
    /// h along v): ±Y -> (1, 0, 2), ±X -> (0, 2, 1), ±Z -> (2, 0, 1).
    pub const fn axes(self) -> (usize, usize, usize) {
        match self {
            Face::PosY | Face::NegY => (1, 0, 2),
            Face::PosX | Face::NegX => (0, 2, 1),
            Face::PosZ | Face::NegZ => (2, 0, 1),
        }
    }
}

/// Offsets of the 26 neighbors: `dy` outer, `dx` middle, `dz` inner, each in -1..=1, skipping
/// (0, 0, 0). Index of an offset: [`neighbor_index`].
pub const NEIGHBOR_OFFSETS: [IVec3; 26] = {
    let mut out = [IVec3::ZERO; 26];
    let mut i = 0;
    let mut dy = -1;
    while dy <= 1 {
        let mut dx = -1;
        while dx <= 1 {
            let mut dz = -1;
            while dz <= 1 {
                if !(dx == 0 && dy == 0 && dz == 0) {
                    out[i] = IVec3::new(dx, dy, dz);
                    i += 1;
                }
                dz += 1;
            }
            dx += 1;
        }
        dy += 1;
    }
    out
};

/// Index into [`NEIGHBOR_OFFSETS`] for an offset with components in -1..=1 (None for zero/out of range).
pub const fn neighbor_index(d: IVec3) -> Option<usize> {
    if d.x < -1
        || d.x > 1
        || d.y < -1
        || d.y > 1
        || d.z < -1
        || d.z > 1
        || (d.x == 0 && d.y == 0 && d.z == 0)
    {
        return None;
    }
    let raw = ((d.y + 1) * 9 + (d.x + 1) * 3 + (d.z + 1)) as usize;
    Some(if raw > 13 { raw - 1 } else { raw })
}

/// Dimension (world) id. M0 uses `DimensionId(0)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DimensionId(pub u16);

/// Full chunk key (persistence and render identity). `lod = 0` in M0; at lod k a chunk covers
/// `(32 << k)^3` voxels, starts at voxel `pos << (5 + k)` ([`ChunkPos::origin`] is the lod-0
/// origin) and the world limit of `pos` is `MAX_CHUNK_COORD >> k`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkKey {
    /// Dimension.
    pub dim: DimensionId,
    /// Level of detail.
    pub lod: u8,
    /// Position (in chunks of this lod).
    pub pos: ChunkPos,
}

impl ChunkKey {
    /// Lod-0 key in dimension 0.
    pub const fn new(pos: ChunkPos) -> Self {
        Self {
            dim: DimensionId(0),
            lod: 0,
            pos,
        }
    }
}
