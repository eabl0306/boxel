//! Property tests: the mesher against the brute-force oracle on random padded chunks, and
//! `gather` against a per-voxel reference on random chunks with random neighbors.
mod common;

use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, Mesher, PaddedChunk, VoxelClass};
use boxel_voxel::{BlockState, CHUNK_VOLUME, ChunkData, LocalPos};
use common::{
    CUTOUT, NO_GEOMETRY, OPAQUE, Repr, Rng, TRANSLUCENT, chunk_repr, neighbor_slot, set_material,
    split, test_table, verify_mesh,
};
use proptest::prelude::*;

/// Every material the random fills draw from (air excluded: it is the background).
const MATERIALS: [u16; 8] = [
    OPAQUE[0],
    OPAQUE[1],
    OPAQUE[2],
    CUTOUT[0],
    CUTOUT[1],
    TRANSLUCENT[0],
    TRANSLUCENT[1],
    NO_GEOMETRY,
];

/// How a random padded chunk is filled.
#[derive(Clone, Copy, Debug)]
enum Fill {
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
fn random_padded(
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

fn fill_strategy() -> impl Strategy<Value = Fill> {
    prop_oneof![
        Just(Fill::Noise),
        Just(Fill::Boxes),
        Just(Fill::Terrain),
        Just(Fill::Lattice),
    ]
}

/// One random chunk of the gather test.
#[derive(Clone, Copy, Debug)]
enum ChunkKind {
    /// No chunk (neighbors only): read as air.
    Missing,
    /// `Paletted::Uniform`.
    Uniform,
    /// Random voxels, `Paletted::Direct`.
    Direct,
    /// Random voxels, `Paletted::Packed`.
    Packed,
}

fn kind_strategy() -> impl Strategy<Value = ChunkKind> {
    prop_oneof![
        Just(ChunkKind::Missing),
        Just(ChunkKind::Uniform),
        Just(ChunkKind::Direct),
        Just(ChunkKind::Packed),
    ]
}

/// States the gather test draws from: air, every test material and an id outside the table.
const STATES: [u16; 10] = [
    0,
    OPAQUE[0],
    OPAQUE[1],
    OPAQUE[2],
    CUTOUT[0],
    CUTOUT[1],
    TRANSLUCENT[0],
    TRANSLUCENT[1],
    NO_GEOMETRY,
    77,
];

fn random_states(rng: &mut Rng, density: u32) -> Vec<BlockState> {
    let palette: Vec<u16> = (0..rng.range(1, 6)).map(|_| rng.pick(&STATES)).collect();
    (0..CHUNK_VOLUME)
        .map(|_| {
            let solid = rng.below(100) < u64::from(density);
            BlockState(if solid { rng.pick(&palette) } else { 0 })
        })
        .collect()
}

fn random_chunk(kind: ChunkKind, rng: &mut Rng, density: u32) -> Option<ChunkData> {
    match kind {
        ChunkKind::Missing => None,
        ChunkKind::Uniform => Some(ChunkData::uniform(BlockState(rng.pick(&STATES)))),
        ChunkKind::Direct => Some(chunk_repr(&random_states(rng, density), Repr::Direct)),
        ChunkKind::Packed => Some(chunk_repr(&random_states(rng, density), Repr::Packed)),
    }
}

/// Per-voxel reference of `gather`: reads every padded voxel from the chunk that owns it.
fn reference_gather(input: &MeshInput<'_>, table: &MaterialTable) -> PaddedChunk {
    let mut padded = PaddedChunk::new();
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                let (dx, lx) = split(x);
                let (dy, ly) = split(y);
                let (dz, lz) = split(z);
                let chunk = if (dx, dy, dz) == (0, 0, 0) {
                    Some(input.center)
                } else {
                    input.neighbors[neighbor_slot(dx, dy, dz)]
                };
                let state = chunk.map_or(0, |c| c.get(LocalPos::new(lx, ly, lz)).0);
                let class = table.class(state);
                let material = if class == VoxelClass::Empty { 0 } else { state };
                padded.set(x, y, z, material, class);
            }
        }
    }
    padded
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The expanded quads equal the oracle; every unit face has its voxel's material (layer)
    /// and bucket; no overlaps.
    #[test]
    fn mesher_matches_oracle(
        fill in fill_strategy(),
        seed in any::<u64>(),
        density in 0u32..=100,
        palette in proptest::collection::vec(proptest::sample::select(MATERIALS.to_vec()), 1..=4),
    ) {
        let table = test_table();
        let padded = random_padded(&table, fill, seed, density, &palette);
        let mut out = ChunkMesh::default();
        Mesher::new().mesh(&padded, &table, &mut out);
        verify_mesh(&padded, &table, &out);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// `gather` equals the per-voxel reference for any mix of representations and missing
    /// neighbors, and `mesh_chunk` equals `gather` + `mesh` (and the oracle).
    #[test]
    fn gather_matches_reference(
        kinds in proptest::collection::vec(kind_strategy(), 27),
        seed in any::<u64>(),
        density in 0u32..=100,
    ) {
        let table = test_table();
        let mut rng = Rng::new(seed);
        let center_kind = match kinds[0] {
            ChunkKind::Missing => ChunkKind::Packed,
            kind => kind,
        };
        let center = random_chunk(center_kind, &mut rng, density).expect("center");
        let neighbors: Vec<Option<ChunkData>> =
            kinds[1..].iter().map(|&k| random_chunk(k, &mut rng, density)).collect();
        let input = MeshInput {
            center: &center,
            neighbors: std::array::from_fn(|i| neighbors[i].as_ref()),
        };
        let mut mesher = Mesher::new();
        let mut padded = PaddedChunk::new();
        mesher.gather(&input, &table, &mut padded);
        let expected = reference_gather(&input, &table);
        prop_assert!(padded.voxels == expected.voxels, "voxel ids differ");
        prop_assert!(padded.opaque == expected.opaque, "opaque masks differ");
        prop_assert!(padded.transparent == expected.transparent, "transparent masks differ");

        let mut out = ChunkMesh::default();
        mesher.mesh(&padded, &table, &mut out);
        verify_mesh(&padded, &table, &out);
        prop_assert_eq!(mesher.mesh_chunk(&input, &table), out);
    }
}
