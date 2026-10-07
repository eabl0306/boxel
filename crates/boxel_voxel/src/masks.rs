//! [`LightMasks`]: per-chunk bit masks for the light computation, built from the blocks and the
//! [`LightTables`] and read through a borrowed view.
//!
//! Most chunks never build anything: a uniform chunk without emission reads static words, and
//! a 1-bit chunk without emission (the overworld's caves, nearly every chunk underground) reads
//! its own packed words, kept or flipped by the palette order. The other chunks build their
//! masks once ([`OwnedLightMasks`]: 4 KiB, 4 KiB more only when some voxel is partial, plus the
//! emitters) and cache them on the [`ChunkData`].
use crate::light::{CLASS_EMITS, CLASS_OPAQUE, CLASS_PARTIAL};
use crate::{
    BlockState, CHUNK_AREA, CHUNK_VOLUME, ChunkData, LightTables, LightValue, LocalPos, Occupancy,
    Paletted,
};

/// Words of one mask: word `k` holds [`Occupancy`] rows `2k` (low half) and `2k + 1` (high
/// half), the layout of 1-bit packed storage.
pub type MaskWords = [u64; CHUNK_AREA / 2];

/// The words behind the constant masks.
static ZERO_WORDS: MaskWords = [0; CHUNK_AREA / 2];

/// One bit mask of a chunk in the [`Occupancy`] layout (bit `z` of row `y * 32 + x`), read row
/// by row. Borrowed from a chunk's cached masks or from its 1-bit block storage, or constant.
#[derive(Clone, Copy)]
pub struct MaskRows<'a> {
    words: &'a MaskWords,
    /// `AND` with each word (0 for a constant mask).
    keep: u64,
    /// `XOR` after `keep` (all ones flips the words, or makes a constant mask full).
    flip: u64,
}

impl MaskRows<'static> {
    /// No voxel set.
    pub const EMPTY: Self = Self {
        words: &ZERO_WORDS,
        keep: 0,
        flip: 0,
    };
    /// Every voxel set.
    pub const FULL: Self = Self {
        words: &ZERO_WORDS,
        keep: 0,
        flip: u64::MAX,
    };
}

impl<'a> MaskRows<'a> {
    /// The mask stored in `words`.
    pub fn from_words(words: &'a MaskWords) -> Self {
        Self {
            words,
            keep: u64::MAX,
            flip: 0,
        }
    }
    /// The mask of a 1-bit chunk whose packed words are `words` (bit 1 is palette entry 1),
    /// when palette entry 0 is in the mask (`set0`) and entry 1 is (`set1`).
    fn one_bit(words: &'a MaskWords, set0: bool, set1: bool) -> Self {
        match (set0, set1) {
            (false, false) => MaskRows::EMPTY,
            (true, true) => MaskRows::FULL,
            (false, true) => Self::from_words(words),
            (true, false) => Self {
                words,
                keep: u64::MAX,
                flip: u64::MAX,
            },
        }
    }
    /// Row `i` (`y * 32 + x`): bit `z` is voxel `(x, y, z)`.
    #[inline]
    pub fn row(self, i: usize) -> u32 {
        let w = (self.words[(i >> 1) & (CHUNK_AREA / 2 - 1)] & self.keep) ^ self.flip;
        (w >> ((i & 1) << 5)) as u32
    }
    /// Every row, as an [`Occupancy`] (tests and one-off reads).
    pub fn to_occupancy(self) -> Box<Occupancy> {
        Box::new(std::array::from_fn(|i| self.row(i)))
    }
    /// Voxels set.
    pub fn count(self) -> u32 {
        self.words
            .iter()
            .map(|&w| ((w & self.keep) ^ self.flip).count_ones())
            .sum()
    }
}

impl std::fmt::Debug for MaskRows<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MaskRows({} set)", self.count())
    }
}

/// The light masks of one chunk: a `Copy` view, from [`ChunkData::light_masks`] (cached or
/// borrowed) or [`OwnedLightMasks::view`].
#[derive(Clone, Copy, Debug)]
pub struct LightMasks<'a> {
    /// Voxels with light opacity 15 (light stops). Not the render shape: a `Cutout` block may
    /// stop light too ([`ChunkData::opaque_occupancy`] is the mask of `Opaque` full cubes).
    pub opaque: MaskRows<'a>,
    /// Voxels with light opacity 1..=14 (leaves, water).
    pub partial: MaskRows<'a>,
    /// Every voxel that emits light, in increasing [`LocalPos`] index order, with its emission
    /// (sky 0).
    pub emitters: &'a [(LocalPos, LightValue)],
}

impl LightMasks<'static> {
    /// The masks of an all-air chunk (also what a missing chunk reads as).
    pub const EMPTY: Self = Self {
        opaque: MaskRows::EMPTY,
        partial: MaskRows::EMPTY,
        emitters: &[],
    };
}

impl<'a> LightMasks<'a> {
    /// The masks of `blocks` when they need no build: uniform or 1-bit storage without
    /// emission. `None` for every other chunk.
    pub(crate) fn borrowed(blocks: &'a Paletted, tables: &LightTables) -> Option<Self> {
        let bit = |c: u8, b: u8| c & b != 0;
        match blocks {
            Paletted::Uniform(s) => {
                let c = tables.class(*s);
                let fill = |b| {
                    if bit(c, b) {
                        MaskRows::FULL
                    } else {
                        MaskRows::EMPTY
                    }
                };
                (!bit(c, CLASS_EMITS)).then(|| LightMasks {
                    opaque: fill(CLASS_OPAQUE),
                    partial: fill(CLASS_PARTIAL),
                    emitters: &[],
                })
            }
            Paletted::Packed {
                palette,
                bits: 1,
                words,
            } => {
                let words: &MaskWords = (&**words).try_into().ok()?;
                let (c0, c1) = one_bit_classes(palette, tables);
                if bit(c0 | c1, CLASS_EMITS) {
                    return None;
                }
                let mask = |b| MaskRows::one_bit(words, bit(c0, b), bit(c1, b));
                Some(LightMasks {
                    opaque: mask(CLASS_OPAQUE),
                    partial: mask(CLASS_PARTIAL),
                    emitters: &[],
                })
            }
            Paletted::Packed { .. } | Paletted::Direct(_) => None,
        }
    }
}

/// Classes of the two palette entries of a 1-bit chunk (a 1-entry palette repeats entry 0).
fn one_bit_classes(palette: &[BlockState], tables: &LightTables) -> (u8, u8) {
    let a = palette.first().copied().unwrap_or(BlockState::AIR);
    let b = palette.get(1).copied().unwrap_or(a);
    (tables.class(a), tables.class(b))
}

/// Built light masks: what [`ChunkData::light_masks`] caches for the chunks that need a build
/// (`Packed` with 2 or more bits, `Direct`, and chunks with an emitter), and what
/// [`OwnedLightMasks::compute`] returns for any chunk.
#[derive(Clone)]
pub struct OwnedLightMasks {
    opaque: MaskWords,
    /// `None` when no voxel is partial.
    partial: Option<Box<MaskWords>>,
    emitters: Vec<(LocalPos, LightValue)>,
    /// [`LightTables::id`] of the tables the masks were built with.
    tables_id: u64,
}

impl std::fmt::Debug for OwnedLightMasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.view().fmt(f)
    }
}

/// Sets the bits `bits` of row `i` in `words`.
#[inline]
fn or_row(words: &mut MaskWords, i: usize, bits: u32) {
    words[i >> 1] |= u64::from(bits) << ((i & 1) << 5);
}

impl OwnedLightMasks {
    /// Builds the masks of `chunk`, any representation, without the cache (benches, tests and
    /// one-off reads; the world uses [`ChunkData::light_masks`]).
    ///
    /// - Uniform: every mask full or empty.
    /// - 1-bit `Packed`: the packed words are the masks, kept or flipped by the palette order.
    /// - Other `Packed`: one class lookup per palette entry; a word of equal indices (a run of
    ///   air or stone) fills its bits at once, the rest are decoded voxel by voxel.
    /// - `Direct`: one class lookup per voxel.
    pub fn compute(chunk: &ChunkData, tables: &LightTables) -> Box<OwnedLightMasks> {
        let mut opaque: MaskWords = [0; CHUNK_AREA / 2];
        let mut partial: MaskWords = [0; CHUNK_AREA / 2];
        let mut emitters = Vec::new();
        match chunk.blocks() {
            Paletted::Uniform(s) => {
                let class = tables.class(*s);
                let fill = |set: bool| if set { u64::MAX } else { 0 };
                opaque.fill(fill(class & CLASS_OPAQUE != 0));
                partial.fill(fill(class & CLASS_PARTIAL != 0));
                if class & CLASS_EMITS != 0 {
                    let e = tables.emission(*s);
                    emitters = (0..CHUNK_VOLUME)
                        .map(|i| (LocalPos::from_index(i), e))
                        .collect();
                }
            }
            Paletted::Packed {
                palette,
                bits: 1,
                words,
            } => one_bit(
                [&mut opaque, &mut partial],
                &mut emitters,
                tables,
                palette,
                words,
            ),
            Paletted::Packed {
                palette,
                bits,
                words,
            } => packed(
                [&mut opaque, &mut partial],
                &mut emitters,
                tables,
                palette,
                *bits,
                words,
            ),
            Paletted::Direct(states) => {
                for (row, voxels) in states.as_chunks::<32>().0.iter().enumerate() {
                    let (mut op, mut pa) = (0u32, 0u32);
                    for (z, &s) in voxels.iter().enumerate() {
                        let c = tables.class(s);
                        op |= u32::from(c & CLASS_OPAQUE != 0) << z;
                        pa |= u32::from(c & CLASS_PARTIAL != 0) << z;
                        if c & CLASS_EMITS != 0 {
                            emitters.push((LocalPos::from_index(row * 32 + z), tables.emission(s)));
                        }
                    }
                    or_row(&mut opaque, row, op);
                    or_row(&mut partial, row, pa);
                }
            }
        }
        Box::new(OwnedLightMasks {
            opaque,
            partial: partial.iter().any(|&w| w != 0).then(|| Box::new(partial)),
            emitters,
            tables_id: tables.id(),
        })
    }

    /// The masks as a view.
    pub fn view(&self) -> LightMasks<'_> {
        LightMasks {
            opaque: MaskRows::from_words(&self.opaque),
            partial: self
                .partial
                .as_deref()
                .map_or(MaskRows::EMPTY, MaskRows::from_words),
            emitters: &self.emitters,
        }
    }

    /// Heap bytes of these masks once boxed (what a chunk's cache holds).
    pub fn heap_bytes(&self) -> usize {
        size_of::<OwnedLightMasks>()
            + self.partial.as_ref().map_or(0, |_| size_of::<MaskWords>())
            + self.emitters.capacity() * size_of::<(LocalPos, LightValue)>()
    }

    /// True if these masks were built with `tables`.
    pub(crate) fn built_with(&self, tables: &LightTables) -> bool {
        self.tables_id == tables.id()
    }
}

/// 1-bit chunks: the packed words are the masks (bit 1 is palette entry 1).
fn one_bit(
    masks: [&mut MaskWords; 2],
    emitters: &mut Vec<(LocalPos, LightValue)>,
    tables: &LightTables,
    palette: &[BlockState],
    words: &[u64],
) {
    let (ca, cb) = one_bit_classes(palette, tables);
    for (bit, mask) in [CLASS_OPAQUE, CLASS_PARTIAL].into_iter().zip(masks) {
        let select = |w: u64| match (ca & bit != 0, cb & bit != 0) {
            (false, false) => 0,
            (true, true) => u64::MAX,
            (false, true) => w,
            (true, false) => !w,
        };
        for (out, &w) in mask.iter_mut().zip(words) {
            *out = select(w);
        }
    }
    if (ca | cb) & CLASS_EMITS != 0 {
        let a = palette.first().copied().unwrap_or(BlockState::AIR);
        let b = palette.get(1).copied().unwrap_or(a);
        let (ea, eb) = (tables.emission(a), tables.emission(b));
        for (k, &w) in words.iter().enumerate() {
            for j in 0..64 {
                let e = if (w >> j) & 1 == 1 { eb } else { ea };
                if !e.is_dark() {
                    emitters.push((LocalPos::from_index(k * 64 + j), e));
                }
            }
        }
    }
}

/// 2, 4 and 8-bit chunks: `64 / bits` voxels per packed word, so each packed word fills part
/// of one occupancy row.
fn packed(
    [opaque, partial]: [&mut MaskWords; 2],
    emitters: &mut Vec<(LocalPos, LightValue)>,
    tables: &LightTables,
    palette: &[BlockState],
    bits: u8,
    words: &[u64],
) {
    let mut lut = [0u8; 256];
    for (slot, &s) in lut.iter_mut().zip(palette) {
        *slot = tables.class(s);
    }
    let b = bits as usize;
    let per_word = 64 / b;
    let mask = (1u64 << b) - 1;
    // `idx * repeat` has `idx` in every slot of a word.
    let repeat = u64::MAX / mask;
    let span: u32 = if per_word == 32 {
        u32::MAX
    } else {
        (1u32 << per_word) - 1
    };
    for (wi, &w) in words.iter().enumerate() {
        let i0 = wi * per_word;
        let (row, shift) = (i0 >> 5, (i0 & 31) as u32);
        let first = (w & mask) as usize;
        let (op, pa, emits) = if w == first as u64 * repeat {
            let c = lut[first];
            let all = |bit: u8| if c & bit != 0 { span } else { 0 };
            (all(CLASS_OPAQUE), all(CLASS_PARTIAL), c & CLASS_EMITS != 0)
        } else {
            let (mut op, mut pa, mut any) = (0u32, 0u32, 0u8);
            for j in 0..per_word {
                let c = lut[((w >> (j * b)) & mask) as usize];
                op |= u32::from(c & CLASS_OPAQUE) << j;
                pa |= u32::from((c & CLASS_PARTIAL) >> 1) << j;
                any |= c;
            }
            (op, pa, any & CLASS_EMITS != 0)
        };
        or_row(opaque, row, op << shift);
        or_row(partial, row, pa << shift);
        if emits {
            for j in 0..per_word {
                let e = palette
                    .get(((w >> (j * b)) & mask) as usize)
                    .map_or(LightValue::DARK, |&s| tables.emission(s));
                if !e.is_dark() {
                    emitters.push((LocalPos::from_index(i0 + j), e));
                }
            }
        }
    }
}
