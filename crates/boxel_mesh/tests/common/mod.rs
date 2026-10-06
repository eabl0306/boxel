//! Helpers shared by the meshing test files (each test binary uses a different subset).
#![allow(dead_code, reason = "each test binary uses a subset of the helpers")]

use boxel_mesh::{
    BUCKET_COUNT, ChunkMesh, MaterialTable, PaddedChunk, VoxelClass, reference_visible_faces,
};
use boxel_voxel::{
    BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, Face, LocalPos, NEIGHBOR_OFFSETS, Paletted,
    RenderShape,
};
use std::collections::BTreeSet;

/// Materials of [`test_table`]: 0 air, 1..=3 opaque, 4..=5 cutout, 6..=7 translucent, 8 a
/// non-air state without geometry. Ids above 8 are outside the table (also no geometry).
pub const OPAQUE: [u16; 3] = [1, 2, 3];
/// Cutout materials of [`test_table`].
pub const CUTOUT: [u16; 2] = [4, 5];
/// Translucent materials of [`test_table`].
pub const TRANSLUCENT: [u16; 2] = [6, 7];
/// Non-air state with `RenderShape::None` in [`test_table`].
pub const NO_GEOMETRY: u16 = 8;

/// Material table where every (material, face) pair has its own texture layer
/// (`material * 6 + face`), so a quad's layer identifies the material it was built from.
pub fn test_table() -> MaterialTable {
    let shapes = vec![
        RenderShape::None,
        RenderShape::Opaque,
        RenderShape::Opaque,
        RenderShape::Opaque,
        RenderShape::Cutout,
        RenderShape::Cutout,
        RenderShape::Translucent,
        RenderShape::Translucent,
        RenderShape::None,
    ];
    let layers = (0..shapes.len() as u16)
        .map(|m| Face::ALL.map(|f| m * 6 + f.index() as u16))
        .collect();
    MaterialTable::from_parts(shapes, layers)
}

/// Small deterministic PRNG (`SplitMix64`) so property tests stay cheap to generate.
pub struct Rng(u64);

impl Rng {
    /// Seeded generator.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u64) as i32
    }
    /// Random element.
    pub fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize]
    }
}

/// Encodes dense states into `Paletted::Packed` with the frozen word layout read by
/// `Paletted::get`/`decode_into` (index `i` in word `i / (64 / bits)` at bit
/// `(i % (64 / bits)) * bits`). Independent of `boxel_voxel`'s encoder, so the decode paths of
/// the mesher are exercised with Packed data even if that encoder changes.
pub fn encode_packed(states: &[BlockState]) -> Paletted {
    assert_eq!(states.len(), CHUNK_VOLUME);
    let mut palette: Vec<BlockState> = Vec::new();
    let indices: Vec<u64> = states
        .iter()
        .map(|s| match palette.iter().position(|p| p == s) {
            Some(i) => i as u64,
            None => {
                palette.push(*s);
                (palette.len() - 1) as u64
            }
        })
        .collect();
    let bits: usize = match palette.len() {
        0..=2 => 1,
        3..=4 => 2,
        5..=16 => 4,
        17..=256 => 8,
        n => panic!("palette of {n} states does not fit Packed"),
    };
    let per_word = 64 / bits;
    let mut words = vec![0u64; CHUNK_VOLUME.div_ceil(per_word)];
    for (i, idx) in indices.into_iter().enumerate() {
        words[i / per_word] |= idx << ((i % per_word) * bits);
    }
    Paletted::Packed {
        palette,
        bits: bits as u8,
        words: words.into_boxed_slice(),
    }
}

/// Chunk with an explicit representation.
pub fn chunk_from(blocks: Paletted) -> ChunkData {
    ChunkData::from_paletted(blocks)
}

/// Representation used for a non-uniform test chunk.
#[derive(Clone, Copy, Debug)]
pub enum Repr {
    /// `Paletted::Direct`.
    Direct,
    /// `Paletted::Packed` (see [`encode_packed`]).
    Packed,
}

/// Chunk from dense states in the given representation (even when all states are equal).
pub fn chunk_repr(states: &[BlockState], repr: Repr) -> ChunkData {
    match repr {
        Repr::Direct => chunk_from(Paletted::Direct(states.into())),
        Repr::Packed => chunk_from(encode_packed(states)),
    }
}

/// Dense states from a function of local `(x, y, z)`.
pub fn dense_from(mut f: impl FnMut(i32, i32, i32) -> u16) -> Vec<BlockState> {
    (0..CHUNK_VOLUME)
        .map(|i| {
            let p = LocalPos::from_index(i);
            BlockState(f(p.x() as i32, p.y() as i32, p.z() as i32))
        })
        .collect()
}

/// Neighbor slot of a chunk offset (each component in -1..=1, not all zero).
pub fn neighbor_slot(dx: i32, dy: i32, dz: i32) -> usize {
    NEIGHBOR_OFFSETS
        .iter()
        .position(|o| (o.x, o.y, o.z) == (dx, dy, dz))
        .expect("offset in -1..=1 and not zero")
}

/// Splits a padded coordinate in -1..=32 into (chunk offset, local coordinate).
pub fn split(c: i32) -> (i32, u32) {
    let d = c.div_euclid(CHUNK_SIZE);
    (d, c.rem_euclid(CHUNK_SIZE) as u32)
}

/// Writes `material` with the class the table gives it (what `gather` does per voxel).
pub fn set_material(padded: &mut PaddedChunk, table: &MaterialTable, p: [i32; 3], material: u16) {
    padded.set(p[0], p[1], p[2], material, table.class(material));
}

/// Class read back from the column masks.
pub fn class_at(padded: &PaddedChunk, x: i32, y: i32, z: i32) -> VoxelClass {
    let row = (x + 1) as usize + (y + 1) as usize * boxel_mesh::PADDED;
    let bit = 1u64 << (z + 1);
    match (
        padded.opaque[row] & bit != 0,
        padded.transparent[row] & bit != 0,
    ) {
        (true, false) => VoxelClass::Opaque,
        (false, true) => VoxelClass::Transparent,
        (false, false) => VoxelClass::Empty,
        (true, true) => panic!("voxel {x},{y},{z} is both opaque and transparent"),
    }
}

/// Checks a mesh against its padded input: counts match the quads, opaque buckets hold their
/// own face, quads stay inside the chunk and never overlap, every unit face has the bucket and
/// texture layer of the material of its voxel, and the expanded unit faces equal the
/// brute-force oracle. Returns the number of unit faces.
pub fn verify_mesh(padded: &PaddedChunk, table: &MaterialTable, mesh: &ChunkMesh) -> usize {
    assert_eq!(
        mesh.bucket_counts.iter().sum::<u32>(),
        mesh.total_quads(),
        "bucket counts do not add up to the quad count"
    );
    let mut got = BTreeSet::new();
    for bucket in 0..BUCKET_COUNT {
        for quad in mesh.bucket(bucket) {
            if bucket < 6 {
                assert_eq!(
                    quad.face().index(),
                    bucket,
                    "opaque bucket holds another face"
                );
            }
            for (p, face) in quad.unit_faces() {
                assert!(
                    p.iter().all(|&c| c < 32),
                    "{quad:?} leaves the chunk at {p:?}"
                );
                let material = padded.get(p[0] as i32, p[1] as i32, p[2] as i32);
                assert_eq!(
                    table.bucket(material, face),
                    bucket,
                    "unit face {p:?} {face:?} of material {material} in the wrong bucket"
                );
                assert_eq!(
                    quad.layer(),
                    table.layer(material, face),
                    "unit face {p:?} {face:?} does not carry the layer of material {material}"
                );
                assert!(got.insert((p, face)), "overlapping quads at {p:?} {face:?}");
            }
        }
    }
    let expected = reference_visible_faces(padded);
    if got != expected {
        let missing: Vec<_> = expected.difference(&got).take(8).collect();
        let extra: Vec<_> = got.difference(&expected).take(8).collect();
        panic!(
            "mesh != oracle ({} vs {} unit faces); missing {missing:?}, extra {extra:?}",
            got.len(),
            expected.len()
        );
    }
    got.len()
}
