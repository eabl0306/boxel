//! Fluid corner heights (ADR 0021): flowing and sloped water cells against the brute-force rule
//! (`reference_fluid_heights`), crack-free shared corners across chunk borders, and a calm lake
//! that costs the quads it did before.
mod common;

use boxel_mesh::{
    AoPolicy, ChunkMesh, FLUID_FLAT, FLUID_SURFACE_BIT, LightQuantization, MaterialTable,
    MeshInput, MeshOptions, Mesher, PackedQuad, PaddedChunk, TRANSLUCENT_BUCKET, layout,
    reference_fluid_heights,
};
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, Face, NEIGHBOR_OFFSETS};
use common::{LIGHT_FILLS, Repr, chunk_repr, dense_from, random_light, verify_mesh};
use std::collections::BTreeMap;

const OPTIONS: MeshOptions = MeshOptions::with_ao(AoPolicy::BakedLitOnly);

fn registry() -> (BlockRegistry, MaterialTable) {
    let registry = BlockRegistry::default_blocks();
    let table = MaterialTable::from_registry(&registry);
    (registry, table)
}

fn state(registry: &BlockRegistry, name: &str) -> BlockState {
    registry.parse_state(name).expect("built-in state")
}

/// SplitMix64 of a world cell and a seed.
fn hash(seed: u64, x: i64, y: i64, z: i64) -> u64 {
    let mut v = seed
        ^ (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (z as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    v = (v ^ (v >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    v ^ (v >> 31)
}

/// A random world of pools: a stone floor 2 to 4 high, then 3 layers of mostly water of random
/// levels with air, stone, glass and (odd seeds) plants among it, then a sparse cover of stone
/// and water.
fn world(registry: &BlockRegistry, seed: u64) -> impl Fn(i64, i64, i64) -> BlockState {
    let stone = state(registry, "boxel:stone");
    let glass = state(registry, "boxel:glass");
    let grass = if seed % 2 == 1 {
        state(registry, "boxel:short_grass")
    } else {
        BlockState::AIR
    };
    let water: Vec<BlockState> = (0..16)
        .map(|l| state(registry, &format!("boxel:water[level={l}]")))
        .collect();
    move |x, y, z| {
        let floor = 2 + (hash(seed, x, 0, z) % 3) as i64;
        let roll = hash(seed ^ 1, x, y, z) % 100;
        let level = || {
            let l = hash(seed ^ 2, x, y, z) % 10;
            water[match l {
                0..5 => 0,
                5..9 => (hash(seed ^ 3, x, y, z) % 7 + 1) as usize,
                _ => (hash(seed ^ 4, x, y, z) % 8 + 8) as usize,
            }]
        };
        if y < floor {
            stone
        } else if y < floor + 3 {
            match roll {
                0..70 => level(),
                70..85 => BlockState::AIR,
                85..92 => stone,
                92..96 => grass,
                _ => glass,
            }
        } else if y < floor + 5 {
            match roll {
                0..10 => stone,
                10..20 => level(),
                _ => BlockState::AIR,
            }
        } else {
            BlockState::AIR
        }
    }
}

/// The chunk at chunk position `c` of `f` (world cells).
fn chunk_of(f: &impl Fn(i64, i64, i64) -> BlockState, c: [i64; 3]) -> ChunkData {
    let dense = dense_from(|x, y, z| {
        f(
            c[0] * 32 + i64::from(x),
            c[1] * 32 + i64::from(y),
            c[2] * 32 + i64::from(z),
        )
        .0
    });
    chunk_repr(&dense, Repr::Packed)
}

/// The center chunk at `c` and its 26 neighbors.
fn neighborhood(
    f: &impl Fn(i64, i64, i64) -> BlockState,
    c: [i64; 3],
) -> (ChunkData, Vec<ChunkData>) {
    let neighbors = NEIGHBOR_OFFSETS
        .iter()
        .map(|o| {
            chunk_of(
                f,
                [
                    c[0] + i64::from(o.x),
                    c[1] + i64::from(o.y),
                    c[2] + i64::from(o.z),
                ],
            )
        })
        .collect();
    (chunk_of(f, c), neighbors)
}

fn input<'a>(center: &'a ChunkData, neighbors: &'a [ChunkData]) -> MeshInput<'a> {
    MeshInput {
        center,
        neighbors: std::array::from_fn(|i| Some(&neighbors[i])),
        light: None,
    }
}

/// The corner heights the mesh draws per surface cell: a sloped cell's quads carry them, a flat
/// cell's top is a `SHAPE_FLUID_SURFACE` greedy quad (all corners at 14). Checks that every quad
/// of one sloped cell carries the same heights and that its top is drawn.
fn drawn_heights(mesh: &ChunkMesh) -> BTreeMap<[u32; 3], [u8; 4]> {
    let mut out: BTreeMap<[u32; 3], [u8; 4]> = BTreeMap::new();
    let mut tops = Vec::new();
    for quad in mesh.bucket(TRANSLUCENT_BUCKET) {
        if let Some(heights) = quad.fluid_heights() {
            let p = quad.pos();
            if let Some(&seen) = out.get(&p) {
                assert_eq!(seen, heights, "{p:?}: quads of one cell with other heights");
            }
            out.insert(p, heights);
            if quad.face() == Face::PosY {
                tops.push(p);
            }
        } else if quad.shape() == layout::SHAPE_FLUID_SURFACE && quad.face() == Face::PosY {
            for (p, _) in quad.unit_faces() {
                assert!(
                    out.insert(p, [FLUID_FLAT; 4]).is_none(),
                    "{p:?} drawn twice"
                );
            }
        }
    }
    for (p, heights) in &out {
        if heights != &[FLUID_FLAT; 4] {
            assert!(tops.contains(p), "{p:?}: a sloped cell without its top");
        }
    }
    out
}

#[test]
fn corner_heights_follow_the_brute_force_rule() {
    let (registry, table) = registry();
    let mut sloped = 0;
    for seed in 0..4u64 {
        let f = world(&registry, seed);
        for c in [[0, 0, 0], [-1, 0, 2]] {
            let (center, neighbors) = neighborhood(&f, c);
            let input = input(&center, &neighbors);
            let mut mesher = Mesher::new();
            let mut padded = PaddedChunk::new();
            mesher.gather(&input, &table, &mut padded);
            let mesh = mesher.mesh_chunk(&input, &table, OPTIONS);
            if seed % 2 == 0 {
                // Cubes and water only (`verify_mesh` knows no plants).
                verify_mesh(&padded, &table, &mesh);
            }
            let want = reference_fluid_heights(&padded, &table);
            let got = drawn_heights(&mesh);
            // A surface cell under glass or a plant shows its top too; every surface cell has one.
            for (p, heights) in &want {
                assert_eq!(
                    got.get(p),
                    Some(heights),
                    "seed {seed} chunk {c:?} cell {p:?}"
                );
            }
            assert_eq!(
                got.len(),
                want.len(),
                "seed {seed}: cells drawn that are not surface"
            );
            sloped += want.values().filter(|h| **h != [FLUID_FLAT; 4]).count();
            // Light: each sloped quad shades like the cube face in its place.
            for (k, fill) in LIGHT_FILLS.into_iter().enumerate() {
                let light = random_light(fill, seed * 8 + k as u64);
                let lit = mesher.mesh_chunk(
                    &MeshInput {
                        light: light.as_ref(),
                        ..input
                    },
                    &table,
                    OPTIONS,
                );
                if seed % 2 == 0 {
                    common::verify_shading(&padded, light.as_ref(), AoPolicy::BakedLitOnly, &lit);
                }
                assert_eq!(drawn_heights(&lit), got, "the light changes no height");
            }
        }
    }
    assert!(sloped > 500, "the scenes slope too little ({sloped} cells)");
}

#[test]
fn neighbor_cells_meet_without_cracks_across_chunks() {
    let (registry, table) = registry();
    for seed in 10..13u64 {
        let f = world(&registry, seed);
        // World cell -> corner heights, from the meshes of 4 chunks around a chunk corner.
        let mut cells: BTreeMap<[i64; 3], [u8; 4]> = BTreeMap::new();
        for c in [[0, 0, 0], [1, 0, 0], [0, 0, 1], [1, 0, 1]] {
            let (center, neighbors) = neighborhood(&f, c);
            let mesh = Mesher::new().mesh_chunk(&input(&center, &neighbors), &table, OPTIONS);
            for (p, h) in drawn_heights(&mesh) {
                let world = std::array::from_fn(|a| c[a] * 32 + i64::from(p[a]));
                cells.insert(world, h);
            }
        }
        let mut shared = 0;
        for (&[x, y, z], h) in &cells {
            // +x neighbor: its corners 0 and 2 are this cell's 1 and 3; +z: 0 and 1 are 2 and 3.
            if let Some(n) = cells.get(&[x + 1, y, z]) {
                assert_eq!(
                    [h[1], h[3]],
                    [n[0], n[2]],
                    "seed {seed}: crack at x {x} y {y} z {z}"
                );
                shared += 1;
            }
            if let Some(n) = cells.get(&[x, y, z + 1]) {
                assert_eq!(
                    [h[2], h[3]],
                    [n[0], n[1]],
                    "seed {seed}: crack at x {x} y {y} z {z}"
                );
                shared += 1;
            }
        }
        assert!(shared > 1000, "seed {seed}: only {shared} shared edges");
    }
}

#[test]
fn a_calm_lake_stays_one_greedy_quad_and_a_waterfall_slopes_it() {
    let (registry, table) = registry();
    let stone = state(&registry, "boxel:stone");
    let source = state(&registry, "boxel:water");
    let flowing = state(&registry, "boxel:water[level=7]");
    let falling = state(&registry, "boxel:water[level=8]");
    let lake = |extra: &dyn Fn(i32, i32, i32) -> Option<BlockState>| {
        let dense = dense_from(|x, y, z| {
            extra(x, y, z)
                .unwrap_or(match (x, y, z) {
                    (1..30, 1..=4, 1..30) => source,
                    (0..31, 0..=4, 0..31) => stone,
                    _ => BlockState::AIR,
                })
                .0
        });
        chunk_repr(&dense, Repr::Packed)
    };
    let mesh_of = |chunk: &ChunkData| {
        let input = MeshInput {
            center: chunk,
            neighbors: [None; 26],
            light: None,
        };
        let mut padded = PaddedChunk::new();
        let mut mesher = Mesher::new();
        mesher.gather(&input, &table, &mut padded);
        (padded, mesher.mesh_chunk(&input, &table, OPTIONS))
    };
    let (_, calm) = mesh_of(&lake(&|_, _, _| None));
    assert_eq!(
        calm.bucket(TRANSLUCENT_BUCKET).len(),
        1,
        "the lake top is one quad"
    );
    // A flowing cell in the lake: the sources around weigh 10 to its 1, so its corners stay at
    // 14/16 and it merges with the rest (every level meshes as the source).
    let (padded, mesh) = mesh_of(&lake(&|x, y, z| {
        ((x, y, z) == (10, 4, 10)).then_some(flowing)
    }));
    assert_eq!(padded.get(10, 4, 10), source.0 | FLUID_SURFACE_BIT);
    assert_eq!(padded.fluid_levels.len(), 1);
    assert_eq!(mesh.bucket(TRANSLUCENT_BUCKET).len(), 1, "still one quad");
    // A waterfall: a source at y 6 over falling water at y 5 over the lake. The lake cell under
    // it is full height, so the 8 around it rise to it; the source on top is a surface cell
    // with air around, its corners drop.
    let (_, mesh) = mesh_of(&lake(&|x, y, z| match (x, y, z) {
        (10, 6, 10) => Some(source),
        (10, 5, 10) => Some(falling),
        _ => None,
    }));
    let heights = drawn_heights(&mesh);
    let sloped: BTreeMap<_, _> = heights
        .into_iter()
        .filter(|(_, h)| *h != [FLUID_FLAT; 4])
        .collect();
    assert_eq!(sloped.len(), 9, "{sloped:?}");
    assert_eq!(sloped[&[11, 4, 10]], [16, 14, 16, 14]);
    assert_eq!(sloped[&[11, 4, 11]], [16, 14, 14, 14]);
    assert_eq!(sloped[&[9, 4, 9]], [14, 14, 14, 16]);
    // Own 112 at weight 10 and two air sides at weight 1: 1120 / 12 = 93.3 -> 12/16.
    assert_eq!(sloped[&[10, 6, 10]], [12; 4]);
    assert_eq!(mesh.light_quantization, LightQuantization::Exact);
}

#[test]
fn fluid_heights_round_trip_through_the_quad() {
    let quad = PackedQuad::new([3, 4, 5], 1, 1, Face::PosX, 77)
        .with_light_index(901)
        .with_fluid_heights([1, 16, 9, 14]);
    assert_eq!(quad.shape(), layout::SHAPE_FLUID_SLOPE);
    assert_eq!(quad.fluid_heights(), Some([1, 16, 9, 14]));
    assert_eq!((quad.pos(), quad.w(), quad.h()), ([3, 4, 5], 1, 1));
    assert_eq!(
        (quad.face(), quad.layer(), quad.light_index()),
        (Face::PosX, 77, 901)
    );
    assert_eq!(quad.ao_byte(), 0xFF, "no AO on a sloped quad");
    assert!(!quad.is_emissive());
    assert_eq!(
        PackedQuad::new([0; 3], 1, 1, Face::PosY, 0).fluid_heights(),
        None
    );
}
