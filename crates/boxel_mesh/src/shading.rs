//! Per-face shading of the mesher (ADR 0015): ambient occlusion and the light of the 4 corners
//! of a unit face, the per-chunk light palette and the merge-key table.
//!
//! Both values only depend on the vertex (the 4 front-layer voxels around it), never on which
//! face asks: two visible coplanar faces that share a vertex see the same AO and the same light
//! there. So when two adjacent faces have the same key, the value is constant along the axis
//! they share, and a merged quad interpolates exactly what its unit faces would.
//!
//! The front-layer neighborhood of a face is the 3x3 block of voxels in the layer in front of
//! it, centered on the voxel the face looks at. A cell is `(du, dv)` in `-1..=1` along the face's
//! canonical u and v axes ([`boxel_voxel::Face::axes`]); its bit in a 9-bit neighborhood is
//! `(dv + 1) * 3 + (du + 1)` (the center, bit 4, is never opaque for a visible face).

#[cfg(test)]
use crate::packed::palette_corners;
use crate::packed::{LightQuantization, MAX_PALETTE_ENTRIES, PaletteEntry, palette_entry};
use boxel_voxel::LightValue;

/// AO of a corner that nothing occludes.
pub const AO_OPEN: u8 = 3;

/// The AO byte of a face that nothing occludes (all 4 corners [`AO_OPEN`]).
pub const AO_OPEN_BYTE: u8 = 0xFF;

/// Per corner `k` (`(cu, cv) = (k & 1, k >> 1)`): the bits of its side along u, its side along
/// v and its diagonal cell in a 9-bit neighborhood.
pub const CORNER_CELLS: [[u32; 3]; 4] = {
    let mut out = [[0; 3]; 4];
    let mut k = 0;
    while k < 4 {
        let su = if k & 1 == 1 { 1 } else { -1 };
        let sv = if k >> 1 == 1 { 1 } else { -1 };
        out[k] = [cell_bit(su, 0), cell_bit(0, sv), cell_bit(su, sv)];
        k += 1;
    }
    out
};

/// Bit of front-layer cell `(du, dv)` in a 9-bit neighborhood.
pub const fn cell_bit(du: i32, dv: i32) -> u32 {
    ((dv + 1) * 3 + (du + 1)) as u32
}

/// The 8-bit form of a 9-bit neighborhood (the center bit removed): the [`AO_TABLE`] index.
#[inline]
pub const fn neighborhood8(nb9: u32) -> usize {
    ((nb9 & 0xF) | ((nb9 >> 5) << 4)) as usize
}

/// AO of one corner from the opacity of its two sides and its diagonal: both sides opaque give
/// 0, otherwise 3 minus the opaque count.
pub const fn corner_ao(side_u: bool, side_v: bool, diagonal: bool) -> u8 {
    if side_u && side_v {
        0
    } else {
        AO_OPEN - side_u as u8 - side_v as u8 - diagonal as u8
    }
}

/// The AO byte (corner `k` at bits `2k`) of every 8-bit front-layer neighborhood
/// ([`neighborhood8`]; bit set = opaque).
pub static AO_TABLE: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        // Back to 9 bits: the center (bit 4) is clear.
        let nb9 = (i as u32 & 0xF) | ((i as u32 >> 4) << 5);
        let mut byte = 0u8;
        let mut k = 0;
        while k < 4 {
            let [su, sv, d] = CORNER_CELLS[k];
            let ao = corner_ao(nb9 >> su & 1 == 1, nb9 >> sv & 1 == 1, nb9 >> d & 1 == 1);
            byte |= ao << (2 * k);
            k += 1;
        }
        table[i] = byte;
        i += 1;
    }
    table
};

/// One lane of 16 bits per channel (sky, red, green, blue from the top), so up to four levels
/// add without carries.
#[inline]
const fn spread(v: u16) -> u64 {
    let v = v as u64;
    (v & 0xF) | ((v & 0xF0) << 12) | ((v & 0xF00) << 24) | ((v & 0xF000) << 36)
}

/// [`spread`] back to a [`LightValue`] (each lane below 16).
#[inline]
const fn gather(lanes: u64) -> u16 {
    ((lanes & 0xF) | ((lanes >> 12) & 0xF0) | ((lanes >> 24) & 0xF00) | ((lanes >> 36) & 0xF000))
        as u16
}

/// Per-lane `(sum + n / 2) / n` for `n` in 1..=4: `(x + HALF[n]) * MUL[n] >> 9` is exact for
/// every lane value below 256 (the largest here is 61).
const HALF: [u64; 5] = [
    0,
    0,
    0x0001_0001_0001_0001,
    0x0001_0001_0001_0001,
    0x0002_0002_0002_0002,
];
const MUL: [u64; 5] = [0, 512, 256, 171, 128];
const LANES: u64 = 0x007F_007F_007F_007F;

/// Light of corner `k` from the 9 front-layer cells (`cells[cell_bit]`) and their opacity
/// (`nb9`, bit set = opaque): the rounded per-channel mean of the front cell, of each side that
/// is not opaque, and of the diagonal unless it is opaque or both sides are.
#[inline]
pub fn corner_light(cells: &[u16; 9], nb9: u32, k: usize) -> LightValue {
    let [su, sv, d] = CORNER_CELLS[k];
    let opaque = |b: u32| nb9 >> b & 1 == 1;
    let (ou, ov) = (opaque(su), opaque(sv));
    let mut sum = spread(cells[4]);
    let mut n = 1;
    if !ou {
        sum += spread(cells[su as usize]);
        n += 1;
    }
    if !ov {
        sum += spread(cells[sv as usize]);
        n += 1;
    }
    if !opaque(d) && !(ou && ov) {
        sum += spread(cells[d as usize]);
        n += 1;
    }
    LightValue(gather((((sum + HALF[n]) * MUL[n]) >> 9) & LANES))
}

/// The 4 corner lights of a face (each one [`corner_light`]), with every cell spread once. With
/// no opaque cell every corner is the mean of its 2x2 block of cells.
#[inline]
pub fn face_corner_lights(cells: &[u16; 9], nb9: u32) -> [LightValue; 4] {
    let s = cells.map(spread);
    if nb9 == 0 {
        // (sum + 2) / 4 per lane: the shift moves 2 bits of each lane into the top of the lane
        // below, which the mask clears (every mean is below 16).
        let mean4 = |a: usize, b: usize, c: usize, d: usize| {
            LightValue(gather(((s[a] + s[b] + s[c] + s[d] + HALF[4]) >> 2) & LANES))
        };
        return [
            mean4(0, 1, 3, 4),
            mean4(1, 2, 4, 5),
            mean4(3, 4, 6, 7),
            mean4(4, 5, 7, 8),
        ];
    }
    let opaque = |b: u32| nb9 >> b & 1 == 1;
    CORNER_CELLS.map(|[su, sv, d]| {
        let (ou, ov) = (opaque(su), opaque(sv));
        let mut sum = s[4];
        let mut n = 1;
        if !ou {
            sum += s[su as usize];
            n += 1;
        }
        if !ov {
            sum += s[sv as usize];
            n += 1;
        }
        if !opaque(d) && !(ou && ov) {
            sum += s[d as usize];
            n += 1;
        }
        LightValue(gather((((sum + HALF[n]) * MUL[n]) >> 9) & LANES))
    })
}

/// The palette was full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaletteFull;

/// The 4 corner lights as one `u64` (corner `k` at bits `16k`).
#[inline]
const fn pack_corners(corners: [LightValue; 4]) -> u64 {
    corners[0].0 as u64
        | (corners[1].0 as u64) << 16
        | (corners[2].0 as u64) << 32
        | (corners[3].0 as u64) << 48
}

/// One slot of a [`StampMap`]: key, value and the generation that wrote it, in one place so a
/// probe reads one cache line.
#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    key: [u64; 2],
    value: u32,
    stamp: u32,
}

/// Open addressing map from a 128-bit key to a `u32`, whose clear is one counter increment
/// (slots carry the generation that wrote them). Keys are never removed.
#[derive(Clone, Debug)]
struct StampMap {
    slots: Vec<Slot>,
    stamp: u32,
    len: usize,
    /// `64 - log2(capacity)`.
    shift: u32,
}

impl StampMap {
    fn with_capacity_pow2(capacity: usize) -> Self {
        debug_assert!(capacity.is_power_of_two() && capacity >= 2);
        Self {
            slots: vec![Slot::default(); capacity],
            stamp: 1,
            len: 0,
            shift: 64 - capacity.trailing_zeros(),
        }
    }

    fn clear(&mut self) {
        self.len = 0;
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            // Every 2^32 clears: no slot may look current.
            self.slots.fill(Slot::default());
            self.stamp = 1;
        }
    }

    #[inline]
    fn slot(&self, key: [u64; 2]) -> usize {
        let mixed = key[0] ^ key[1].wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        (mixed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> self.shift) as usize
    }

    /// The value of `key`, or inserts `make(len)` (unless it fails) and returns it.
    #[inline]
    fn get_or_insert<E>(
        &mut self,
        key: [u64; 2],
        make: impl FnOnce(usize) -> Result<u32, E>,
    ) -> Result<u32, E> {
        let mask = self.slots.len() - 1;
        let mut i = self.slot(key);
        loop {
            let slot = self.slots[i];
            if slot.stamp != self.stamp {
                break;
            }
            if slot.key == key {
                return Ok(slot.value);
            }
            i = (i + 1) & mask;
        }
        let value = make(self.len)?;
        self.slots[i] = Slot {
            key,
            value,
            stamp: self.stamp,
        };
        self.len += 1;
        if self.len * 2 > self.slots.len() {
            self.grow();
        }
        Ok(value)
    }

    fn grow(&mut self) {
        let old = std::mem::replace(self, Self::with_capacity_pow2(self.slots.len() * 2));
        let mask = self.slots.len() - 1;
        for slot in old.slots.iter().filter(|s| s.stamp == old.stamp) {
            let mut j = self.slot(slot.key);
            while self.slots[j].stamp == self.stamp {
                j = (j + 1) & mask;
            }
            self.slots[j] = Slot {
                stamp: self.stamp,
                ..*slot
            };
        }
        self.len = old.len;
    }
}

/// A chunk's light palette under construction: distinct 4-corner lights in first-seen order, at
/// most [`MAX_PALETTE_ENTRIES`].
#[derive(Clone, Debug)]
pub struct LightPalette {
    map: StampMap,
    entries: Vec<PaletteEntry>,
}

impl Default for LightPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl LightPalette {
    /// Empty palette (its table is sized for the most entries, so it never grows).
    pub fn new() -> Self {
        Self {
            map: StampMap::with_capacity_pow2(2 * MAX_PALETTE_ENTRIES),
            entries: Vec::new(),
        }
    }
    /// Empties it, keeping its memory.
    pub fn clear(&mut self) {
        self.map.clear();
        self.entries.clear();
    }
    /// The index of `corners`, added if new; `Err` when it is new and the palette is full.
    #[inline]
    pub fn index(&mut self, corners: [LightValue; 4]) -> Result<u16, PaletteFull> {
        let entries = &mut self.entries;
        let index = self.map.get_or_insert([pack_corners(corners), 0], |len| {
            if len < MAX_PALETTE_ENTRIES {
                entries.push(palette_entry(corners));
                Ok(len as u32)
            } else {
                Err(PaletteFull)
            }
        })?;
        Ok(index as u16)
    }
    /// Entries in index order.
    pub fn entries(&self) -> &[PaletteEntry] {
        &self.entries
    }
    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// No entry yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// What a merge key stands for: the material, the AO byte and the light palette index of a
/// unit face. Two unit faces merge only if these are all equal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FaceKey {
    /// Material id.
    pub material: u16,
    /// AO byte (corner `k` at bits `2k`).
    pub ao: u8,
    /// Light palette index.
    pub light: u16,
}

/// Dense `u32` ids of the [`FaceKey`]s of a chunk, in first-seen order. A face is looked up by
/// its material, AO byte and corner lights in one probe; the palette is only asked for the
/// index of corner lights the table has not seen with that material and AO.
#[derive(Clone, Debug)]
pub struct KeyTable {
    map: StampMap,
    keys: Vec<FaceKey>,
    /// The last face looked up and its id (runs of equal faces are common).
    last: Option<([u64; 2], u32)>,
}

impl Default for KeyTable {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyTable {
    /// Empty table.
    pub fn new() -> Self {
        Self {
            map: StampMap::with_capacity_pow2(4096),
            keys: Vec::new(),
            last: None,
        }
    }
    /// Empties it, keeping its memory (clear `palette` with it).
    pub fn clear(&mut self) {
        self.map.clear();
        self.keys.clear();
        self.last = None;
    }
    /// The id of the face (`material`, `ao`, `corners`), added if new, with its corners added
    /// to `palette` (the palette this table was filled with since both were cleared). `Err`
    /// when the palette is full.
    #[inline]
    pub fn id(
        &mut self,
        material: u16,
        ao: u8,
        corners: [LightValue; 4],
        palette: &mut LightPalette,
    ) -> Result<u32, PaletteFull> {
        let key = [
            pack_corners(corners),
            u64::from(material) | u64::from(ao) << 16,
        ];
        if let Some((last, id)) = self.last
            && last == key
        {
            return Ok(id);
        }
        let keys = &mut self.keys;
        let id = self.map.get_or_insert(key, |len| {
            let light = palette.index(corners)?;
            keys.push(FaceKey {
                material,
                ao,
                light,
            });
            Ok(len as u32)
        })?;
        self.last = Some((key, id));
        Ok(id)
    }
    /// The key of `id`.
    #[inline]
    pub fn key(&self, id: u32) -> FaceKey {
        self.keys[id as usize]
    }
    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    /// No key yet.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// The shading of one unit face: its AO byte and the light of its 4 corners (corner order of
/// [`crate::PackedQuad::with_ao`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceShading {
    /// AO byte (corner `k` at bits `2k`, 3 = open).
    pub ao: u8,
    /// Corner lights.
    pub light: [LightValue; 4],
}

impl FaceShading {
    /// This shading with its light at `level`; with `elide`, a face whose quantized corners
    /// are all dark gets AO open (what [`crate::AoPolicy::BakedLitOnly`] stores).
    pub fn quantized(self, level: LightQuantization, elide: bool) -> Self {
        let light = level.apply(self.light);
        let dark = light.iter().all(|c| c.is_dark());
        Self {
            ao: if elide && dark { AO_OPEN_BYTE } else { self.ao },
            light,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ao_table_follows_the_corner_rule() {
        assert_eq!(AO_TABLE[0], AO_OPEN_BYTE);
        // Only the (-1, -1) diagonal: corner 0 drops to 2.
        assert_eq!(
            AO_TABLE[neighborhood8(1 << cell_bit(-1, -1))],
            0b11_11_11_10
        );
        // Both sides of corner 3: it is 0 whatever its diagonal; corners 1 and 2 see one side.
        let sides = 1 << cell_bit(1, 0) | 1 << cell_bit(0, 1);
        assert_eq!(AO_TABLE[neighborhood8(sides)], 0b00_10_10_11);
        // Everything opaque.
        assert_eq!(AO_TABLE[255], 0);
    }

    #[test]
    fn corner_light_is_the_rounded_mean_of_the_kept_cells() {
        let mut cells = [0u16; 9];
        cells[4] = 0xF000;
        cells[cell_bit(-1, 0) as usize] = 0xE100;
        cells[cell_bit(0, -1) as usize] = 0xD020;
        cells[cell_bit(-1, -1) as usize] = 0x0003;
        // Corner 0 keeps all four: sky (15 + 14 + 13 + 0 + 2) / 4 = 11, red (1 + 2) / 4 = 0,
        // green (2 + 2) / 4 = 1, blue (3 + 2) / 4 = 1.
        assert_eq!(corner_light(&cells, 0, 0), LightValue::new(11, 0, 1, 1));
        // An opaque diagonal is dropped: (15 + 14 + 13 + 1) / 3 = 14, red (1 + 1) / 3 = 0,
        // green (2 + 1) / 3 = 1.
        let nb = 1 << cell_bit(-1, -1);
        assert_eq!(corner_light(&cells, nb, 0), LightValue::new(14, 0, 1, 0));
        // Both sides opaque: only the front cell is left.
        let nb = 1 << cell_bit(-1, 0) | 1 << cell_bit(0, -1);
        assert_eq!(corner_light(&cells, nb, 0), LightValue(0xF000));
        // Corners away from the lit cells average with darkness: (15 + 1) / 4 = 4 (sky).
        assert_eq!(corner_light(&cells, 0, 3), LightValue::new(4, 0, 0, 0));
    }

    #[test]
    fn corner_light_matches_a_scalar_reference_for_every_n() {
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let cells: [u16; 9] = std::array::from_fn(|_| next() as u16);
            // A quarter of the cases without opaque cells (the 2x2 fast path).
            let nb9 = if next() % 4 == 0 {
                0
            } else {
                next() as u32 & 0x1EF
            };
            for (k, &[su, sv, d]) in CORNER_CELLS.iter().enumerate() {
                let op = |b: u32| nb9 >> b & 1 == 1;
                let mut kept = vec![cells[4]];
                if !op(su) {
                    kept.push(cells[su as usize]);
                }
                if !op(sv) {
                    kept.push(cells[sv as usize]);
                }
                if !op(d) && !(op(su) && op(sv)) {
                    kept.push(cells[d as usize]);
                }
                let n = kept.len() as u16;
                let mut want = 0u16;
                for shift in [12, 8, 4, 0] {
                    let sum: u16 = kept.iter().map(|v| (v >> shift) & 15).sum();
                    want |= ((sum + n / 2) / n) << shift;
                }
                assert_eq!(corner_light(&cells, nb9, k), LightValue(want));
                assert_eq!(face_corner_lights(&cells, nb9)[k], LightValue(want));
            }
        }
    }

    #[test]
    fn palette_keeps_first_seen_order_and_refuses_past_the_limit() {
        let mut p = LightPalette::new();
        let c = |v: u16| [LightValue(v); 4];
        assert_eq!(p.index(c(7)), Ok(0));
        assert_eq!(p.index(c(9)), Ok(1));
        assert_eq!(p.index(c(7)), Ok(0));
        for v in 10..(10 + MAX_PALETTE_ENTRIES as u16 - 2) {
            assert!(p.index(c(v)).is_ok());
        }
        assert_eq!(p.len(), MAX_PALETTE_ENTRIES);
        assert_eq!(p.index(c(60_000)), Err(PaletteFull));
        assert_eq!(p.index(c(9)), Ok(1));
        p.clear();
        assert!(p.is_empty());
        assert_eq!(p.index(c(9)), Ok(0));
        assert_eq!(p.entries(), &[palette_entry(c(9))]);
    }

    #[test]
    fn key_table_grows_and_clears() {
        let mut t = KeyTable::new();
        let mut p = LightPalette::new();
        let face = |i: u32| {
            (
                i as u16,
                (i >> 16) as u8,
                [LightValue((i % 4096) as u16); 4],
            )
        };
        for i in 0..100_000 {
            let (material, ao, corners) = face(i);
            assert_eq!(t.id(material, ao, corners, &mut p), Ok(i));
        }
        assert_eq!(p.len(), 4096);
        for i in (0..100_000).step_by(7) {
            let (material, ao, corners) = face(i);
            assert_eq!(t.id(material, ao, corners, &mut p), Ok(i));
            let key = t.key(i);
            assert_eq!((key.material, key.ao), (material, ao));
            assert_eq!(
                palette_corners(p.entries()[usize::from(key.light)]),
                corners
            );
        }
        // A new face whose corners are not in the full palette is refused.
        assert_eq!(
            t.id(1, 0, [LightValue(0xFFFF); 4], &mut p),
            Err(PaletteFull)
        );
        t.clear();
        p.clear();
        let (material, ao, corners) = face(99_999);
        assert_eq!(t.id(material, ao, corners, &mut p), Ok(0));
        assert_eq!((t.len(), p.len()), (1, 1));
    }
}
