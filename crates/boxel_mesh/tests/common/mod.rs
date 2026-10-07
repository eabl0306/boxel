//! Helpers shared by the meshing test files (each test binary uses a different subset).
#![allow(dead_code, reason = "each test binary uses a subset of the helpers")]

use boxel_light::{PADDED_VOLUME, PaddedLight};
use boxel_mesh::{
    AoPolicy, BUCKET_COUNT, ChunkMesh, FLUID_SURFACE_BIT, FaceShading, MAX_PALETTE_ENTRIES,
    MaterialTable, PaddedChunk, VoxelClass, layout, reference_face_shading,
    reference_visible_faces,
};
use boxel_voxel::{
    BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, Face, LightValue, LocalPos, NEIGHBOR_OFFSETS,
    Paletted, RenderShape,
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
/// own face, quads stay inside the chunk and never overlap, every unit face has the bucket,
/// texture layer and emissive flag of the material of its voxel and the shape code of its face
/// (`SHAPE_FLUID_SURFACE` on a marked fluid surface cell but for its -Y face, or
/// `SHAPE_FLUID_SLOPE` on every face of a sloped one), and the expanded
/// unit faces equal the brute-force oracle. Returns the number of unit faces.
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
                let id = padded.get(p[0] as i32, p[1] as i32, p[2] as i32);
                let marked = padded.cull_mask != u16::MAX;
                let surface = marked && id & FLUID_SURFACE_BIT != 0;
                let material = if marked { id & !FLUID_SURFACE_BIT } else { id };
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
                assert_eq!(
                    quad.is_emissive(),
                    table.emissive(material),
                    "unit face {p:?} {face:?}: emissive flag of material {material}"
                );
                let code = if surface && quad.shape() == layout::SHAPE_FLUID_SLOPE {
                    // A surface cell with its own corner heights (checked by the fluid tests).
                    layout::SHAPE_FLUID_SLOPE
                } else if surface && face != Face::NegY {
                    layout::SHAPE_FLUID_SURFACE
                } else {
                    table.cube_code(material, face)
                };
                assert_eq!(quad.shape(), code, "{quad:?} {face:?}: shape code");
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

/// How a random padded chunk is filled.
#[derive(Clone, Copy, Debug)]
pub enum Fill {
    /// Independent voxels with probability `density` percent.
    Noise,
    /// Overlapping random boxes (large greedy merges, partial overlaps).
    Boxes,
    /// Layered heightmap with a liquid level (terrain-like surfaces and borders).
    Terrain,
    /// Periodic lattice `(a x + b y + c z) mod m < k` (checkerboards, stripes, slopes).
    Lattice,
}

/// Fills the whole padded chunk (inner voxels and the 1-voxel border) from the parameters.
pub fn random_padded(
    table: &MaterialTable,
    fill: Fill,
    seed: u64,
    density: u32,
    palette: &[u16],
) -> PaddedChunk {
    let mut rng = Rng::new(seed);
    let mut padded = PaddedChunk::new();
    let all =
        || (-1..=32).flat_map(|y| (-1..=32).flat_map(move |x| (-1..=32).map(move |z| [x, y, z])));
    match fill {
        Fill::Noise => {
            for p in all() {
                if rng.below(100) < u64::from(density) {
                    let material = rng.pick(palette);
                    set_material(&mut padded, table, p, material);
                }
            }
        }
        Fill::Boxes => {
            for _ in 0..1 + density / 4 {
                let material = rng.pick(palette);
                let lo = [0; 3].map(|_| rng.range(-1, 32));
                let size = [0; 3].map(|_| rng.range(1, 20));
                for y in lo[1]..(lo[1] + size[1]).min(33) {
                    for x in lo[0]..(lo[0] + size[0]).min(33) {
                        for z in lo[2]..(lo[2] + size[2]).min(33) {
                            set_material(&mut padded, table, [x, y, z], material);
                        }
                    }
                }
            }
        }
        Fill::Terrain => {
            let base = density as i32 * 34 / 100 - 1;
            let (fx, fz) = (rng.range(1, 9) as f32 / 37.0, rng.range(1, 9) as f32 / 29.0);
            let amplitude = rng.range(0, 12) as f32;
            let sea = rng.range(-1, 32);
            let layer = |depth: i32| palette[(depth.clamp(0, 3) as usize).min(palette.len() - 1)];
            for x in -1..=32 {
                for z in -1..=32 {
                    let h =
                        base + (amplitude * ((x as f32 * fx).sin() + (z as f32 * fz).cos())) as i32;
                    for y in -1..=32 {
                        if y <= h {
                            set_material(&mut padded, table, [x, y, z], layer(h - y));
                        } else if y <= sea {
                            let liquid = palette[palette.len() - 1];
                            set_material(&mut padded, table, [x, y, z], liquid);
                        }
                    }
                }
            }
        }
        Fill::Lattice => {
            let (a, b, c) = (rng.range(0, 3), rng.range(0, 3), rng.range(0, 3));
            let m = rng.range(2, 5);
            let k = rng.range(1, m - 1);
            for p in all() {
                let phase = (a * p[0] + b * p[1] + c * p[2]).rem_euclid(m);
                if phase < k {
                    let material = palette[phase as usize % palette.len()];
                    set_material(&mut padded, table, p, material);
                }
            }
        }
    }
    padded
}

/// How a random padded light is made.
#[derive(Clone, Copy, Debug)]
pub enum LightFill {
    /// No light (`None`: open sky everywhere).
    Absent,
    /// One random value everywhere (dark and open sky included).
    Uniform,
    /// Sky falling off below a random height and a few colored point lights fading by 1 per
    /// voxel (Manhattan): what real light looks like, smooth with large equal areas.
    Smooth,
    /// A few random values in 2x2x2 blocks.
    Blocky,
    /// Block channels in a 4-level band (12 to 15) per voxel under constant sky: too many
    /// exact corner values for the palette, but few enough at 3 bits.
    Band,
    /// A random value per voxel: overflows the palette even at 3 bits.
    Noise,
}

/// Every [`LightFill`].
pub const LIGHT_FILLS: [LightFill; 6] = [
    LightFill::Absent,
    LightFill::Uniform,
    LightFill::Smooth,
    LightFill::Blocky,
    LightFill::Band,
    LightFill::Noise,
];

/// A `Full` padded light from a function of padded `(x, y, z)` (each in -1..=32).
pub fn full_light(mut f: impl FnMut(i32, i32, i32) -> u16) -> PaddedLight {
    let mut values = vec![0u16; PADDED_VOLUME];
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                values[PaddedLight::index(x, y, z)] = f(x, y, z);
            }
        }
    }
    PaddedLight::Full(
        values
            .into_boxed_slice()
            .try_into()
            .expect("PADDED_VOLUME values"),
    )
}

/// A random padded light of kind `fill`.
pub fn random_light(fill: LightFill, seed: u64) -> Option<PaddedLight> {
    let mut rng = Rng::new(seed ^ 0x5EED_0F11_8D00);
    let level = |rng: &mut Rng| rng.below(16) as u16;
    Some(match fill {
        LightFill::Absent => return None,
        LightFill::Uniform => PaddedLight::Uniform(LightValue(match rng.below(3) {
            0 => 0,
            1 => 0xF000,
            _ => rng.next_u64() as u16,
        })),
        LightFill::Smooth => {
            let sky_top = rng.range(-4, 36);
            let lights: Vec<([i32; 3], [u16; 3])> = (0..rng.range(0, 3))
                .map(|_| {
                    let p = [0; 3].map(|_| rng.range(-8, 40));
                    (p, [0; 3].map(|_| level(&mut rng)))
                })
                .collect();
            full_light(|x, y, z| {
                let sky = (15 - (sky_top - y).max(0)).max(0) as u16;
                let mut value = sky << 12;
                for (p, rgb) in &lights {
                    let d = ((x - p[0]).abs() + (y - p[1]).abs() + (z - p[2]).abs()) as u16;
                    let lit = LightValue::new(
                        0,
                        rgb[0].saturating_sub(d) as u8,
                        rgb[1].saturating_sub(d) as u8,
                        rgb[2].saturating_sub(d) as u8,
                    );
                    value = LightValue(value).max(lit).0;
                }
                value
            })
        }
        LightFill::Blocky => {
            let values: Vec<u16> = (0..rng.range(1, 6))
                .map(|_| rng.next_u64() as u16)
                .collect();
            let salt = rng.next_u64();
            full_light(|x, y, z| {
                let cell = Rng::new(
                    salt ^ ((x >> 1) as u64) << 40 ^ ((y >> 1) as u64) << 20 ^ (z >> 1) as u64,
                )
                .next_u64();
                values[(cell % values.len() as u64) as usize]
            })
        }
        LightFill::Band => full_light(|_, _, _| {
            0xF000
                | [0; 3]
                    .iter()
                    .fold(0, |v, _| v << 4 | (12 + rng.below(4) as u16))
        }),
        LightFill::Noise => full_light(|_, _, _| rng.next_u64() as u16),
    })
}

/// Checks the shading of a mesh against [`reference_face_shading`]: every unit face of every
/// quad has the quad's AO byte and palette corners, and those equal the reference at the mesh's
/// quantization level. Also checks that the palette fits, every quad indexes it and every entry
/// is used.
pub fn verify_shading(
    padded: &PaddedChunk,
    light: Option<&PaddedLight>,
    policy: AoPolicy,
    mesh: &ChunkMesh,
) {
    let reference = reference_face_shading(padded, light, policy);
    let elide = policy == AoPolicy::BakedLitOnly;
    assert!(
        mesh.palette.len() <= MAX_PALETTE_ENTRIES,
        "palette overflow"
    );
    let mut used = vec![false; mesh.palette.len()];
    let mut faces = 0;
    for quad in &mesh.quads {
        let index = quad.light_index() as usize;
        assert!(
            index < mesh.palette.len(),
            "{quad:?} indexes past the palette"
        );
        used[index] = true;
        let got = FaceShading {
            ao: quad.ao_byte(),
            light: mesh.corner_light(*quad),
        };
        for (p, face) in quad.unit_faces() {
            let mut want = reference
                .get(&(p, face))
                .unwrap_or_else(|| panic!("{p:?} {face:?} is not a visible face"))
                .quantized(mesh.light_quantization, elide);
            if quad.shape() == layout::SHAPE_FLUID_SLOPE {
                // Sloped fluid quads have no AO: their AO bits hold corner heights.
                want.ao = 0xFF;
            }
            assert_eq!(got, want, "{quad:?}: unit face {p:?} {face:?}");
            faces += 1;
        }
    }
    assert_eq!(faces, reference.len(), "unit faces");
    assert!(used.iter().all(|&u| u), "unused palette entries");
}
