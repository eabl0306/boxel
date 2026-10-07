//! Regression against `binary-greedy-meshing` 0.5.2, the crate `src/greedy.rs` forks: with AO
//! off and no light (and whenever light and AO cannot split a merge: uniform light with AO off,
//! or dark uniform light with AO on lit faces only) the mesher's quads are byte for byte the
//! ones bgm gives, converted to canonical `PackedQuad`s the way the mesher did before the fork.
mod common;

use binary_greedy_meshing as bgm;
use boxel_light::PaddedLight;
use boxel_mesh::{
    AoPolicy, BUCKET_COUNT, ChunkMesh, MaterialTable, MeshOptions, Mesher, PackedQuad, PaddedChunk,
};
use boxel_voxel::{Face, LightValue};
use common::{CUTOUT, Fill, NO_GEOMETRY, OPAQUE, TRANSLUCENT, random_padded, test_table};
use proptest::prelude::*;

/// Every material the random fills draw from.
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

/// One bgm quad as a canonical [`PackedQuad`]: the min corner of its 4 vertices is the
/// position, the extents along the face's u and v axes are `w` and `h`, and positive faces
/// (which bgm puts on the `voxel + 1` plane) move back by one on the normal axis.
fn canonical(bgm_face: bgm::Face, quad: bgm::Quad, face: Face, layer: u16) -> PackedQuad {
    let mut min = [u32::MAX; 3];
    let mut max = [0u32; 3];
    for vertex in bgm_face.vertices_packed(quad) {
        for (axis, c) in vertex.xyz().into_iter().enumerate() {
            min[axis] = min[axis].min(c);
            max[axis] = max[axis].max(c);
        }
    }
    let (n, u, v) = face.axes();
    assert_eq!(min[n], max[n], "bgm quad is not planar");
    let mut pos = min;
    if face.index().is_multiple_of(2) {
        pos[n] -= 1;
    }
    PackedQuad::new(pos, max[u] - min[u], max[v] - min[v], face, layer)
}

/// The mesh the mesher gave before the fork: bgm `fast_mesh`, every quad converted with
/// [`canonical`] and scattered to its bucket in bgm's order.
fn bgm_mesh(padded: &PaddedChunk, table: &MaterialTable) -> (Vec<PackedQuad>, [u32; 8]) {
    let mut mesher = bgm::Mesher::<32>::new();
    mesher.fast_mesh(&padded.voxels, &padded.opaque, &padded.transparent);
    let mut buckets: [Vec<PackedQuad>; BUCKET_COUNT] = std::array::from_fn(|_| Vec::new());
    for (face, quads) in Face::ALL.into_iter().zip(&mesher.quads) {
        let bgm_face = bgm::Face::from(face.index() as u8);
        for &quad in quads {
            let material = quad.voxel_id() as u16;
            let layer = table.layer(material, face);
            buckets[table.bucket(material, face)].push(canonical(bgm_face, quad, face, layer));
        }
    }
    let counts = buckets.each_ref().map(|b| b.len() as u32);
    (buckets.concat(), counts)
}

fn fill_strategy() -> impl Strategy<Value = Fill> {
    prop_oneof![
        Just(Fill::Noise),
        Just(Fill::Boxes),
        Just(Fill::Terrain),
        Just(Fill::Lattice),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn quads_equal_bgm_without_ao_or_light(
        fill in fill_strategy(),
        seed in any::<u64>(),
        density in 0u32..=100,
        palette in proptest::collection::vec(proptest::sample::select(MATERIALS.to_vec()), 1..=4),
        uniform in any::<u16>(),
    ) {
        let table = test_table();
        let padded = random_padded(&table, fill, seed, density, &palette);
        let (quads, counts) = bgm_mesh(&padded, &table);
        let mut mesher = Mesher::new();
        let mut out = ChunkMesh::default();
        for (light, policy) in [
            (None, AoPolicy::Off),
            (Some(PaddedLight::Uniform(LightValue(uniform))), AoPolicy::Off),
            (Some(PaddedLight::Uniform(LightValue::DARK)), AoPolicy::BakedLitOnly),
        ] {
            mesher.mesh(&padded, light.as_ref(), &table, MeshOptions::with_ao(policy), &mut out);
            prop_assert!(
                bytemuck::cast_slice::<_, u8>(&out.quads) == bytemuck::cast_slice::<_, u8>(&quads),
                "quads differ from bgm ({} vs {}) with {:?} {:?}",
                out.quads.len(),
                quads.len(),
                light,
                policy
            );
            prop_assert_eq!(out.bucket_counts, counts);
            let entry = light.map_or(LightValue::FULL_SKY, |l| l.get(0, 0, 0));
            let expected = if quads.is_empty() {
                Vec::new()
            } else {
                vec![boxel_mesh::palette_entry([entry; 4])]
            };
            prop_assert_eq!(&out.palette, &expected);
        }
    }
}
