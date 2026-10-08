//! Which faces of a chunk see each other (cave culling, ADR 0022).
//!
//! Two faces of a chunk are linked when a path of face-adjacent voxels that do not occlude
//! ([`BlockRegistry::occludes`]: an `Opaque` full cube) goes from one to the other inside the
//! chunk. Water, glass, leaves, slabs, stairs, fences, plants and air are all open: a ray can
//! cross them. The world's visibility search walks from the camera's chunk through these links
//! (Minecraft and Sodium do the same); a chunk it never reaches cannot be seen.
//!
//! [`ChunkFaceInfo::compute`] is a bit-parallel flood fill over the 1024 rows of the chunk's
//! occlusion mask ([`Occupancy`] layout, `u32` along z): one flood per connected open region that
//! touches the border, each one a worklist of `y` layers flooded in 2D (whole z runs at once).
//! Uniform chunks and chunks without any occluding voxel take a constant-time path.
use crate::{BlockRegistry, BlockState, CHUNK_AREA, ChunkData, Face, Occupancy, Paletted};

/// Bit of the link between faces `a` and `b` (`a != b`), in [`ChunkFaceInfo::links`]: the 15
/// unordered pairs of the 6 faces in `Face` order, `(0, 1)` first and `(4, 5)` last.
pub const fn link_bit(a: Face, b: Face) -> u16 {
    LINK_BITS[a.index()][b.index()]
}

/// Every link set.
pub const ALL_LINKS: u16 = (1 << 15) - 1;

/// `LINK_BITS[a][b]`: the bit of pair `(a, b)`; zero on the diagonal.
const LINK_BITS: [[u16; 6]; 6] = {
    let mut out = [[0u16; 6]; 6];
    let mut bit = 0;
    let mut a = 0;
    while a < 6 {
        let mut b = a + 1;
        while b < 6 {
            out[a][b] = 1 << bit;
            out[b][a] = 1 << bit;
            bit += 1;
            b += 1;
        }
        a += 1;
    }
    out
};

/// What a chunk lets through, face to face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChunkFaceInfo {
    /// Bit [`link_bit`]`(a, b)` set when faces `a` and `b` are linked through open voxels.
    pub links: u16,
    /// Bit `f` set when every voxel on face `f` occludes (the chunk shows a closed wall there).
    pub opaque_faces: u8,
    /// Every voxel occludes.
    pub all_opaque: bool,
}

impl ChunkFaceInfo {
    /// Nothing occludes: every pair of faces is linked (air, or a chunk whose blocks are not
    /// known yet, which the visibility search must treat as open).
    pub const OPEN: Self = Self {
        links: ALL_LINKS,
        opaque_faces: 0,
        all_opaque: false,
    };

    /// Every voxel occludes: no links, every face closed.
    pub const SOLID: Self = Self {
        links: 0,
        opaque_faces: 0x3f,
        all_opaque: true,
    };

    /// The face info of `chunk` with `registry`'s occlusion ([`BlockRegistry::occludes`]).
    /// Constant time for uniform chunks; a few microseconds for a cave chunk.
    pub fn compute(chunk: &ChunkData, registry: &BlockRegistry) -> Self {
        if let Some(state) = chunk.as_uniform() {
            return Self::uniform(registry.occludes(state));
        }
        let mut occupancy = [0u32; CHUNK_AREA];
        occlusion_mask(chunk.blocks(), registry, &mut occupancy);
        Self::from_occupancy(&occupancy)
    }

    /// The info of a chunk filled with one state: [`ChunkFaceInfo::SOLID`] if it occludes, else
    /// [`ChunkFaceInfo::OPEN`].
    pub const fn uniform(occludes: bool) -> Self {
        if occludes { Self::SOLID } else { Self::OPEN }
    }

    /// The info of a chunk whose occluding voxels are the set bits of `occupancy` (bit `z` of
    /// word `y * 32 + x`, the layout of [`ChunkData::opaque_occupancy`]).
    pub fn from_occupancy(occupancy: &Occupancy) -> Self {
        let all = occupancy.iter().fold(u32::MAX, |acc, &w| acc & w);
        if all == u32::MAX {
            return Self::SOLID;
        }
        let opaque_faces = opaque_faces(occupancy);
        if occupancy.iter().all(|&w| w == 0) {
            return Self::OPEN;
        }
        let mut open = [0u32; CHUNK_AREA];
        for (o, &w) in open.iter_mut().zip(occupancy) {
            *o = !w;
        }
        let mut flood = Flood::new(&open);
        let mut links = 0;
        // One flood per open region touching the border, seeded from any border voxel no
        // earlier flood reached.
        while let Some(seed) = flood.next_seed() {
            let touched = flood.fill(seed);
            links |= pairs(touched);
            if links == ALL_LINKS {
                break;
            }
        }
        Self {
            links,
            opaque_faces,
            all_opaque: false,
        }
    }

    /// True if faces `a` and `b` are linked (`a == b`: false).
    pub const fn linked(self, a: Face, b: Face) -> bool {
        self.links & link_bit(a, b) != 0
    }

    /// The faces linked to face `f` by `links`, as a 6-bit mask in `Face` order.
    #[inline]
    pub const fn linked_faces(links: u16, f: Face) -> u8 {
        let row = &LINK_BITS[f.index()];
        let mut out = 0;
        let mut g = 0;
        while g < 6 {
            if links & row[g] != 0 {
                out |= 1 << g;
            }
            g += 1;
        }
        out
    }
}

/// Every pair of the faces in `touched` (6-bit mask) as link bits.
fn pairs(touched: u8) -> u16 {
    let mut links = 0;
    for (a, row) in LINK_BITS.iter().enumerate() {
        if touched & (1 << a) == 0 {
            continue;
        }
        for (b, &bit) in row.iter().enumerate().skip(a + 1) {
            if touched & (1 << b) != 0 {
                links |= bit;
            }
        }
    }
    links
}

/// Faces whose 32x32 border layer is fully set in `occupancy`, as a 6-bit mask in `Face` order.
fn opaque_faces(occupancy: &Occupancy) -> u8 {
    let layer = |y: usize| {
        occupancy[y * 32..y * 32 + 32]
            .iter()
            .all(|&w| w == u32::MAX)
    };
    let column = |x: usize| (0..32).all(|y| occupancy[y * 32 + x] == u32::MAX);
    let all = occupancy.iter().fold(u32::MAX, |acc, &w| acc & w);
    u8::from(layer(31))
        | u8::from(layer(0)) << 1
        | u8::from(column(31)) << 2
        | u8::from(column(0)) << 3
        | u8::from(all >> 31 == 1) << 4
        | u8::from(all & 1 == 1) << 5
}

/// The run-fill of `seed` inside `mask` along one `u32` row: every bit of a run of set bits of
/// `mask` that holds a bit of `seed` (`seed` must be inside `mask`).
#[inline]
fn fill_runs(seed: u32, mask: u32) -> u32 {
    // Adding a seed bit carries through the rest of its run above it.
    let up = |s: u32, m: u32| ((m.wrapping_add(s) ^ m) & m) | s;
    up(seed, mask) | up(seed.reverse_bits(), mask.reverse_bits()).reverse_bits()
}

/// Flood fill state over the open mask of one chunk.
struct Flood<'a> {
    open: &'a Occupancy,
    /// Open voxels some flood already reached.
    visited: Occupancy,
    /// The current flood.
    region: Occupancy,
}

impl<'a> Flood<'a> {
    fn new(open: &'a Occupancy) -> Self {
        Self {
            open,
            visited: [0; CHUNK_AREA],
            region: [0; CHUNK_AREA],
        }
    }

    /// An open border voxel no flood reached yet, as (row, bit), or `None`.
    fn next_seed(&self) -> Option<(usize, u32)> {
        let free = |i: usize| self.open[i] & !self.visited[i];
        // -Z and +Z: bit 0 and bit 31 of every row; then the four side layers and columns.
        for i in 0..CHUNK_AREA {
            let f = free(i) & 0x8000_0001;
            if f != 0 {
                return Some((i, f.isolate_lowest_one()));
            }
        }
        // Layers y = 0 and y = 31, then columns x = 0 and x = 31.
        let rows = (0..32)
            .chain(31 * 32..32 * 32)
            .chain((0..32).map(|y| y * 32))
            .chain((0..32).map(|y| y * 32 + 31));
        for i in rows {
            let f = free(i);
            if f != 0 {
                return Some((i, f.isolate_lowest_one()));
            }
        }
        None
    }

    /// Floods the open region holding `seed` (row, bit), adds it to `visited` and returns the
    /// faces it touches (6-bit mask in `Face` order).
    fn fill(&mut self, (row, bit): (usize, u32)) -> u8 {
        self.region.fill(0);
        self.region[row] = fill_runs(bit, self.open[row]);
        // Worklist of y layers to flood again (a neighbor layer changed): the seed's layer and
        // the two it feeds first.
        let y = row / 32;
        let mut dirty: u32 = (0b111u64 << y >> 1) as u32;
        while dirty != 0 {
            let y = dirty.trailing_zeros() as usize;
            dirty &= dirty - 1;
            if self.flood_layer(y) {
                if y > 0 {
                    dirty |= 1 << (y - 1);
                }
                if y < 31 {
                    dirty |= 1 << (y + 1);
                }
            }
        }
        let mut touched = 0u8;
        let mut any_z = 0u32;
        for y in 0..32 {
            let layer = &self.region[y * 32..y * 32 + 32];
            let mut any = 0;
            for &w in layer {
                any |= w;
            }
            if any != 0 {
                if y == 31 {
                    touched |= 1 << Face::PosY.index();
                }
                if y == 0 {
                    touched |= 1 << Face::NegY.index();
                }
                if layer[31] != 0 {
                    touched |= 1 << Face::PosX.index();
                }
                if layer[0] != 0 {
                    touched |= 1 << Face::NegX.index();
                }
                any_z |= any;
            }
        }
        if any_z >> 31 != 0 {
            touched |= 1 << Face::PosZ.index();
        }
        if any_z & 1 != 0 {
            touched |= 1 << Face::NegZ.index();
        }
        for (v, &r) in self.visited.iter_mut().zip(&self.region) {
            *v |= r;
        }
        touched
    }

    /// Floods layer `y` of the region from itself and the layers above and below (2D, whole z
    /// runs at once). Returns true if the layer grew.
    fn flood_layer(&mut self, y: usize) -> bool {
        let base = y * 32;
        let mut grew = false;
        // The layers above and below feed this one.
        for x in 0..32 {
            let i = base + x;
            let mut s = self.region[i];
            if y > 0 {
                s |= self.region[i - 32];
            }
            if y < 31 {
                s |= self.region[i + 32];
            }
            s &= self.open[i];
            if s != self.region[i] {
                self.region[i] = fill_runs(s, self.open[i]);
                grew = true;
            }
        }
        // Then x neighbors, sweeping both ways until stable.
        loop {
            let mut changed = false;
            for x in (0..32).chain((0..32).rev()) {
                let i = base + x;
                let o = self.open[i];
                if o == 0 {
                    continue;
                }
                let mut s = self.region[i];
                if x > 0 {
                    s |= self.region[i - 1];
                }
                if x < 31 {
                    s |= self.region[i + 1];
                }
                s &= o;
                if s != self.region[i] {
                    self.region[i] = fill_runs(s, o);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
            grew = true;
        }
        grew
    }
}

/// Writes into `out` the occluding voxels of `blocks` (the layout of
/// [`ChunkData::opaque_occupancy`], same result), with a lookup per byte of packed indices
/// instead of one per voxel.
fn occlusion_mask(blocks: &Paletted, registry: &BlockRegistry, out: &mut Occupancy) {
    let occludes = |s: BlockState| registry.occludes(s);
    match blocks {
        Paletted::Uniform(s) => out.fill(if occludes(*s) { u32::MAX } else { 0 }),
        Paletted::Packed {
            palette,
            bits,
            words,
        } => {
            let b = u32::from(*bits);
            let lanes = 8 / b; // voxels per byte
            let mut flags = [false; 256];
            for (f, &s) in flags.iter_mut().zip(palette.iter()) {
                *f = occludes(s);
            }
            // Per byte value: the occluding voxels among its lanes.
            let mut lut = [0u8; 256];
            let mask = (1u32 << b) - 1;
            for (v, entry) in lut.iter_mut().enumerate() {
                for lane in 0..lanes {
                    if flags[((v as u32 >> (lane * b)) & mask) as usize] {
                        *entry |= 1 << lane;
                    }
                }
            }
            // Each word holds 64 / b voxels: whole rows (b = 1: two rows) or a part of one.
            let per_word = 64 / b;
            for (wi, &w) in words.iter().enumerate() {
                let mut acc = 0u64;
                for byte in 0..8 {
                    let v = ((w >> (byte * 8)) & 0xff) as usize;
                    acc |= u64::from(lut[v]) << (byte * lanes);
                }
                let first = wi as u64 * u64::from(per_word);
                let row = (first >> 5) as usize;
                let shift = first & 31;
                if per_word == 64 {
                    out[row] = acc as u32;
                    out[row + 1] = (acc >> 32) as u32;
                } else {
                    // 32, 16 or 8 voxels: inside one row.
                    if shift == 0 {
                        out[row] = 0;
                    }
                    out[row] |= (acc as u32) << shift;
                }
            }
        }
        Paletted::Direct(states) => {
            for (word, row) in out.iter_mut().zip(states.as_chunks::<32>().0) {
                let mut w = 0;
                for (z, &s) in row.iter().enumerate() {
                    w |= u32::from(occludes(s)) << z;
                }
                *word = w;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_bits_cover_the_15_pairs_once() {
        let mut seen = 0u16;
        for a in Face::ALL {
            for b in Face::ALL {
                let bit = link_bit(a, b);
                assert_eq!(bit, link_bit(b, a));
                if a == b {
                    assert_eq!(bit, 0);
                } else {
                    assert_eq!(bit.count_ones(), 1);
                    seen |= bit;
                }
            }
        }
        assert_eq!(seen, ALL_LINKS);
        for f in Face::ALL {
            assert_eq!(
                ChunkFaceInfo::linked_faces(ALL_LINKS, f),
                0x3f & !(1 << f.index())
            );
            assert_eq!(ChunkFaceInfo::linked_faces(0, f), 0);
        }
    }

    #[test]
    fn fill_runs_fills_whole_runs_both_ways() {
        let m = 0b0111_0110_1110u32;
        assert_eq!(fill_runs(0b0000_0010_0000, m), 0b0000_0110_0000);
        assert_eq!(fill_runs(0b0010_0000_0000, m), 0b0111_0000_0000);
        assert_eq!(fill_runs(0b0010_0000_1000, m), 0b0111_0000_1110);
        assert_eq!(fill_runs(1 << 31, u32::MAX), u32::MAX);
        assert_eq!(fill_runs(1, u32::MAX), u32::MAX);
        assert_eq!(fill_runs(0, m), 0);
    }
}
