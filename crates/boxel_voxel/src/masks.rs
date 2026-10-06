//! [`ChunkMasks`]: per-chunk bit masks for light, physics and raycasts, built from the blocks
//! and the [`LightTables`] and cached on [`ChunkData`].
use crate::light::{CLASS_EMITS, CLASS_OPAQUE, CLASS_PARTIAL, CLASS_SOLID};
use crate::{
    CHUNK_AREA, CHUNK_VOLUME, ChunkData, LightTables, LightValue, LocalPos, Occupancy, Paletted,
};

/// Bit masks of one chunk. Each mask is an [`Occupancy`]: bit `z` of word `y * 32 + x`.
///
/// Built by [`ChunkData::masks`] (cached) or [`ChunkMasks::compute`] (not cached).
#[derive(Clone, PartialEq, Eq)]
pub struct ChunkMasks {
    /// Voxels with opacity 15 (light stops).
    pub opaque: Occupancy,
    /// Voxels with opacity 1..=14 (leaves, water).
    pub partial: Occupancy,
    /// Voxels that collide (`BlockDef::solid`), for physics and raycasts.
    pub solid: Occupancy,
    /// Every voxel that emits light, in increasing [`LocalPos`] index order, with its emission
    /// (sky 0).
    pub emitters: Vec<(LocalPos, LightValue)>,
    /// [`LightTables::id`] of the tables the masks were built with (0 for the shared uniform
    /// masks, which do not depend on them).
    tables_id: u64,
}

impl std::fmt::Debug for ChunkMasks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = |m: &Occupancy| m.iter().map(|w| w.count_ones()).sum::<u32>();
        f.debug_struct("ChunkMasks")
            .field("opaque", &count(&self.opaque))
            .field("partial", &count(&self.partial))
            .field("solid", &count(&self.solid))
            .field("emitters", &self.emitters.len())
            .finish()
    }
}

/// Mask classes of the shared uniform masks (`CLASS_OPAQUE | CLASS_PARTIAL | CLASS_SOLID`).
const UNIFORM_CLASSES: u8 = CLASS_OPAQUE | CLASS_PARTIAL | CLASS_SOLID;

/// Masks of a chunk where every voxel has class `class` and nothing emits.
const fn uniform(class: u8) -> ChunkMasks {
    const fn fill(set: bool) -> Occupancy {
        if set {
            [u32::MAX; CHUNK_AREA]
        } else {
            [0; CHUNK_AREA]
        }
    }
    ChunkMasks {
        opaque: fill(class & CLASS_OPAQUE != 0),
        partial: fill(class & CLASS_PARTIAL != 0),
        solid: fill(class & CLASS_SOLID != 0),
        emitters: Vec::new(),
        tables_id: 0,
    }
}

/// The masks of every uniform chunk without emission, indexed by its class bits.
static UNIFORM: [ChunkMasks; 8] = [
    uniform(0),
    uniform(1),
    uniform(2),
    uniform(3),
    uniform(4),
    uniform(5),
    uniform(6),
    uniform(7),
];

impl ChunkMasks {
    /// The masks of an all-air chunk (also what a missing chunk reads as).
    pub fn empty() -> &'static ChunkMasks {
        &UNIFORM[0]
    }

    /// The shared masks of a uniform chunk of class `class`, if it does not emit.
    pub(crate) fn shared_uniform(class: u8) -> Option<&'static ChunkMasks> {
        (class & CLASS_EMITS == 0).then(|| &UNIFORM[(class & UNIFORM_CLASSES) as usize])
    }

    /// True if these masks were built with `tables` (or do not depend on them).
    pub(crate) fn built_with(&self, tables: &LightTables) -> bool {
        self.tables_id == 0 || self.tables_id == tables.id()
    }

    /// Builds the masks of `chunk` without the cache (benches, tests and one-off reads; the
    /// world uses [`ChunkData::masks`]).
    ///
    /// - Uniform: every mask full or empty.
    /// - 1-bit `Packed`: the packed words are the masks, kept or flipped by the palette order.
    /// - Other `Packed`: one class lookup per palette entry; a word of equal indices (a run of
    ///   air or stone) fills its bits at once, the rest are decoded voxel by voxel.
    /// - `Direct`: one class lookup per voxel.
    pub fn compute(chunk: &ChunkData, tables: &LightTables) -> Box<ChunkMasks> {
        let mut m = Box::new(uniform(0));
        m.tables_id = tables.id();
        match chunk.blocks() {
            Paletted::Uniform(s) => {
                let class = tables.class(*s);
                let fill = |set: bool| if set { u32::MAX } else { 0 };
                m.opaque.fill(fill(class & CLASS_OPAQUE != 0));
                m.partial.fill(fill(class & CLASS_PARTIAL != 0));
                m.solid.fill(fill(class & CLASS_SOLID != 0));
                if class & CLASS_EMITS != 0 {
                    let e = tables.emission(*s);
                    m.emitters = (0..CHUNK_VOLUME)
                        .map(|i| (LocalPos::from_index(i), e))
                        .collect();
                }
            }
            Paletted::Packed {
                palette,
                bits: 1,
                words,
            } => one_bit(&mut m, tables, palette, words),
            Paletted::Packed {
                palette,
                bits,
                words,
            } => packed(&mut m, tables, palette, *bits, words),
            Paletted::Direct(states) => {
                for (word, row) in states.as_chunks::<32>().0.iter().enumerate() {
                    let (mut op, mut pa, mut so) = (0u32, 0u32, 0u32);
                    for (z, &s) in row.iter().enumerate() {
                        let c = tables.class(s);
                        op |= u32::from(c & CLASS_OPAQUE != 0) << z;
                        pa |= u32::from(c & CLASS_PARTIAL != 0) << z;
                        so |= u32::from(c & CLASS_SOLID != 0) << z;
                        if c & CLASS_EMITS != 0 {
                            m.emitters
                                .push((LocalPos::from_index(word * 32 + z), tables.emission(s)));
                        }
                    }
                    m.opaque[word] = op;
                    m.partial[word] = pa;
                    m.solid[word] = so;
                }
            }
        }
        m
    }
}

/// 1-bit chunks: packed word `k` holds voxels `64k .. 64k + 64`, which are occupancy words
/// `2k` (low half) and `2k + 1` (high half). Bit 1 is palette entry 1.
fn one_bit(m: &mut ChunkMasks, tables: &LightTables, palette: &[crate::BlockState], words: &[u64]) {
    let a = palette[0];
    let b = palette.get(1).copied().unwrap_or(a);
    let (ca, cb) = (tables.class(a), tables.class(b));
    for (bit, mask) in [
        (CLASS_OPAQUE, &mut m.opaque),
        (CLASS_PARTIAL, &mut m.partial),
        (CLASS_SOLID, &mut m.solid),
    ] {
        let select = |w: u64| match (ca & bit != 0, cb & bit != 0) {
            (false, false) => 0,
            (true, true) => u64::MAX,
            (false, true) => w,
            (true, false) => !w,
        };
        if ca & bit == 0 && cb & bit == 0 {
            continue;
        }
        for (pair, &w) in mask.as_chunks_mut::<2>().0.iter_mut().zip(words) {
            let v = select(w);
            *pair = [v as u32, (v >> 32) as u32];
        }
    }
    if (ca | cb) & CLASS_EMITS != 0 {
        let (ea, eb) = (tables.emission(a), tables.emission(b));
        for (k, &w) in words.iter().enumerate() {
            for j in 0..64 {
                let e = if (w >> j) & 1 == 1 { eb } else { ea };
                if !e.is_dark() {
                    m.emitters.push((LocalPos::from_index(k * 64 + j), e));
                }
            }
        }
    }
}

/// 2, 4 and 8-bit chunks: `64 / bits` voxels per packed word, so each packed word fills part
/// of one occupancy word.
fn packed(
    m: &mut ChunkMasks,
    tables: &LightTables,
    palette: &[crate::BlockState],
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
        let (o, shift) = (i0 >> 5, (i0 & 31) as u32);
        let first = (w & mask) as usize;
        let (op, pa, so, emits) = if w == first as u64 * repeat {
            let c = lut[first];
            let all = |bit: u8| if c & bit != 0 { span } else { 0 };
            (
                all(CLASS_OPAQUE),
                all(CLASS_PARTIAL),
                all(CLASS_SOLID),
                c & CLASS_EMITS != 0,
            )
        } else {
            let (mut op, mut pa, mut so, mut any) = (0u32, 0u32, 0u32, 0u8);
            for j in 0..per_word {
                let c = lut[((w >> (j * b)) & mask) as usize];
                op |= u32::from(c & CLASS_OPAQUE) << j;
                pa |= u32::from((c & CLASS_PARTIAL) >> 1) << j;
                so |= u32::from((c & CLASS_SOLID) >> 2) << j;
                any |= c;
            }
            (op, pa, so, any & CLASS_EMITS != 0)
        };
        m.opaque[o] |= op << shift;
        m.partial[o] |= pa << shift;
        m.solid[o] |= so << shift;
        if emits {
            for j in 0..per_word {
                let e = palette
                    .get(((w >> (j * b)) & mask) as usize)
                    .map_or(LightValue::DARK, |&s| tables.emission(s));
                if !e.is_dark() {
                    m.emitters.push((LocalPos::from_index(i0 + j), e));
                }
            }
        }
    }
}
