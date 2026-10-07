//! The per-block path of the non-cube blocks (ADR 0018): slabs, cross plants and turned cubes,
//! against brute-force rules written here, independent of the mesher.

mod common;

use boxel_mesh::{
    AoPolicy, BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, FaceShading, MaterialTable, MeshOptions,
    Mesher, PackedQuad, PaddedChunk, VoxelClass, layout, reference_face_shading_at,
    reference_visible_faces,
};
use boxel_voxel::{Axis, BlockModel, BlockRegistry, Face, LightValue, RenderShape, SlabHalf};
use common::{Fill, LIGHT_FILLS, random_light, random_padded, set_material};
use std::collections::BTreeSet;

/// Materials: 1..=2 opaque cubes, 3 cutout cube, 4 translucent cube, 5 bottom slab, 6 top
/// slab, 7 a cross (cutout), 8 a cube turned to X, 9 a cube turned to Z, 10 a translucent
/// bottom slab. Layer of (material, face) is `material * 6 + face`.
const BOTTOM: u16 = 5;
const TOP: u16 = 6;
const CROSS: u16 = 7;
const LOG_X: u16 = 8;
const LOG_Z: u16 = 9;
const GLASS_SLAB: u16 = 10;
const ALL: [u16; 10] = [1, 2, 3, 4, BOTTOM, TOP, CROSS, LOG_X, LOG_Z, GLASS_SLAB];

fn table() -> MaterialTable {
    let shapes = vec![
        RenderShape::None,
        RenderShape::Opaque,
        RenderShape::Opaque,
        RenderShape::Cutout,
        RenderShape::Translucent,
        RenderShape::Opaque,
        RenderShape::Opaque,
        RenderShape::Cutout,
        RenderShape::Opaque,
        RenderShape::Opaque,
        RenderShape::Translucent,
    ];
    let layers = (0..shapes.len() as u16)
        .map(|m| Face::ALL.map(|f| m * 6 + f.index() as u16))
        .collect();
    let mut t = MaterialTable::from_parts(shapes, layers);
    t.set_model(BOTTOM, BlockModel::Slab(SlabHalf::Bottom));
    t.set_model(TOP, BlockModel::Slab(SlabHalf::Top));
    t.set_model(CROSS, BlockModel::Cross);
    t.set_model(LOG_X, BlockModel::Cube { axis: Axis::X });
    t.set_model(LOG_Z, BlockModel::Cube { axis: Axis::Z });
    t.set_model(GLASS_SLAB, BlockModel::Slab(SlabHalf::Bottom));
    t
}

/// The slab half of a material, if it is a slab.
fn half_of(m: u16) -> Option<SlabHalf> {
    match m {
        BOTTOM | GLASS_SLAB => Some(SlabHalf::Bottom),
        TOP => Some(SlabHalf::Top),
        _ => None,
    }
}

/// Which part of the border of its cell `face` of a slab of `half` covers: `None` inside the
/// cell, `Some(None)` the whole face, `Some(Some(half))` that half of a side.
fn border(half: SlabHalf, face: Face) -> Option<Option<SlabHalf>> {
    match (half, face) {
        (SlabHalf::Bottom, Face::NegY) | (SlabHalf::Top, Face::PosY) => Some(None),
        (_, Face::PosY | Face::NegY) => None,
        (half, _) => Some(Some(half)),
    }
}

/// One expected or meshed quad of the per-block path.
type Special = ([u32; 3], Face, u8, u16, usize, FaceShading);

/// The quads the per-block path must give, from the rules of ADR 0018.
fn expected_specials(
    padded: &PaddedChunk,
    table: &MaterialTable,
    light: Option<&boxel_light::PaddedLight>,
    policy: AoPolicy,
    level: boxel_mesh::LightQuantization,
) -> Vec<Special> {
    let elide = policy == AoPolicy::BakedLitOnly;
    let mut out = Vec::new();
    for y in 0..32 {
        for x in 0..32 {
            for z in 0..32 {
                let m = padded.get(x, y, z);
                let p = [x as u32, y as u32, z as u32];
                if let Some(half) = half_of(m) {
                    for face in Face::ALL {
                        let n = face.normal();
                        let nm = padded.get(x + n.x, y + n.y, z + n.z);
                        let hidden = match border(half, face) {
                            None => false,
                            Some(mine) => {
                                table.class(nm) == VoxelClass::Opaque
                                    || half_of(nm).is_some_and(|nh| {
                                        match border(nh, face.opposite()) {
                                            Some(None) => true,
                                            Some(theirs) => mine.is_some() && theirs == mine,
                                            None => false,
                                        }
                                    })
                            }
                        };
                        if hidden {
                            continue;
                        }
                        let code = match half {
                            SlabHalf::Bottom => layout::SHAPE_LOWER_HALF,
                            SlabHalf::Top => layout::SHAPE_UPPER_HALF,
                        };
                        let shading = reference_face_shading_at(padded, light, policy, p, face)
                            .quantized(level, elide);
                        out.push((
                            p,
                            face,
                            code,
                            m * 6 + face.index() as u16,
                            table.bucket(m, face),
                            shading,
                        ));
                    }
                } else if m == CROSS {
                    let own = light.map_or(LightValue::FULL_SKY, |l| l.get(x, y, z));
                    for face in [Face::PosX, Face::PosZ] {
                        let shading = FaceShading {
                            ao: 0xFF,
                            light: [own; 4],
                        }
                        .quantized(level, false);
                        out.push((
                            p,
                            face,
                            layout::SHAPE_CROSS,
                            m * 6 + Face::PosX.index() as u16,
                            CUTOUT_BUCKET,
                            shading,
                        ));
                    }
                }
            }
        }
    }
    out.sort_by_key(|s| (s.0, s.1.index(), s.2));
    out
}

/// Splits a mesh into its greedy unit faces (checking the turned codes of the logs) and its
/// per-block quads.
fn split_mesh(mesh: &ChunkMesh) -> (BTreeSet<([u32; 3], Face)>, Vec<Special>) {
    let mut cubes = BTreeSet::new();
    let mut specials = Vec::new();
    for bucket in 0..BUCKET_COUNT {
        for &quad in mesh.bucket(bucket) {
            let m = quad.layer() / 6;
            let shading = FaceShading {
                ao: quad.ao_byte(),
                light: mesh.corner_light(quad),
            };
            match quad.shape() {
                layout::SHAPE_CUBE | layout::SHAPE_TURNED => {
                    let model = match m {
                        LOG_X => BlockModel::Cube { axis: Axis::X },
                        LOG_Z => BlockModel::Cube { axis: Axis::Z },
                        _ => BlockModel::CUBE,
                    };
                    let turned = quad.shape() == layout::SHAPE_TURNED;
                    assert_eq!(turned, model.face_turned(quad.face()), "{quad:?}");
                    for unit in quad.unit_faces() {
                        assert!(cubes.insert(unit), "overlap at {unit:?}");
                    }
                }
                code => {
                    assert_eq!((quad.w(), quad.h()), (1, 1), "{quad:?}");
                    specials.push((quad.pos(), quad.face(), code, quad.layer(), bucket, shading));
                }
            }
        }
    }
    specials.sort_by_key(|s| (s.0, s.1.index(), s.2));
    (cubes, specials)
}

fn mesh(
    padded: &PaddedChunk,
    table: &MaterialTable,
    light: Option<&boxel_light::PaddedLight>,
    ao: AoPolicy,
) -> ChunkMesh {
    let mut out = ChunkMesh::default();
    Mesher::new().mesh(padded, light, table, MeshOptions::with_ao(ao), &mut out);
    out
}

#[test]
fn random_chunks_with_shapes_match_the_rules() {
    let table = table();
    let fills = [Fill::Noise, Fill::Boxes, Fill::Terrain, Fill::Lattice];
    let policies = [AoPolicy::Off, AoPolicy::Baked, AoPolicy::BakedLitOnly];
    let mut specials_seen = 0;
    for seed in 0..48u64 {
        let fill = fills[seed as usize % fills.len()];
        // Shapes first in the palette: Terrain puts the first entries on top.
        let palette: Vec<u16> = if seed % 2 == 0 {
            vec![BOTTOM, CROSS, TOP, 1, LOG_X, 2, GLASS_SLAB, LOG_Z, 3, 4]
        } else {
            ALL.to_vec()
        };
        let padded = random_padded(&table, fill, seed, 20 + (seed as u32 * 7) % 60, &palette);
        let light_fill = LIGHT_FILLS[seed as usize % 5];
        let light = random_light(light_fill, seed);
        let policy = policies[seed as usize % 3];
        let mesh = mesh(&padded, &table, light.as_ref(), policy);
        let (cubes, specials) = split_mesh(&mesh);
        assert_eq!(cubes, reference_visible_faces(&padded), "seed {seed}");
        let expected = expected_specials(
            &padded,
            &table,
            light.as_ref(),
            policy,
            mesh.light_quantization,
        );
        assert_eq!(specials.len(), expected.len(), "seed {seed} ({fill:?})");
        for (got, want) in specials.iter().zip(&expected) {
            assert_eq!(
                got, want,
                "seed {seed} ({fill:?}, {light_fill:?}, {policy:?})"
            );
        }
        specials_seen += specials.len();
        assert_eq!(
            mesh.bucket_counts.iter().sum::<u32>(),
            mesh.total_quads(),
            "seed {seed}"
        );
    }
    assert!(
        specials_seen > 10_000,
        "the fills exercise the per-block path"
    );
}

#[test]
fn slabs_hide_and_show_the_faces_they_should() {
    let table = table();
    let mut padded = PaddedChunk::new();
    let put = |padded: &mut PaddedChunk, p: [i32; 3], m| set_material(padded, &table, p, m);
    // A stone floor at y = 0, two bottom slabs side by side on it, a top slab next to them and a
    // glass block next to the top slab.
    for x in 0..8 {
        for z in 0..8 {
            put(&mut padded, [x, 0, z], 1);
        }
    }
    put(&mut padded, [2, 1, 2], BOTTOM);
    put(&mut padded, [3, 1, 2], BOTTOM);
    put(&mut padded, [4, 1, 2], TOP);
    put(&mut padded, [5, 1, 2], 4);
    let mesh = mesh(&padded, &table, None, AoPolicy::Off);
    let (cubes, specials) = split_mesh(&mesh);
    let faces =
        |p: [u32; 3]| -> Vec<Face> { specials.iter().filter(|s| s.0 == p).map(|s| s.1).collect() };
    // The left slab: no bottom (the floor), no +X (the other bottom slab).
    assert_eq!(
        faces([2, 1, 2]),
        [Face::PosY, Face::NegX, Face::PosZ, Face::NegZ]
    );
    // The middle slab: no bottom, no -X; its +X faces the top slab's other half: drawn.
    assert_eq!(
        faces([3, 1, 2]),
        [Face::PosY, Face::PosX, Face::PosZ, Face::NegZ]
    );
    // The top slab: all six (its bottom is inside its cell, glass hides nothing).
    assert_eq!(faces([4, 1, 2]).len(), 6);
    // The floor under the slabs still draws its top (hidden by depth), and the glass its -X.
    assert!(cubes.contains(&([2, 0, 2], Face::PosY)));
    assert!(cubes.contains(&([5, 1, 2], Face::NegX)));
    // Codes and buckets: opaque slab faces go to their face bucket.
    for s in &specials {
        let want = if s.0 == [4, 1, 2] {
            layout::SHAPE_UPPER_HALF
        } else {
            layout::SHAPE_LOWER_HALF
        };
        assert_eq!(s.2, want);
        assert_eq!(s.4, s.1.index());
    }
}

#[test]
fn crosses_are_two_cutout_quads_lit_by_their_own_cell() {
    let table = table();
    let mut padded = PaddedChunk::new();
    set_material(&mut padded, &table, [7, 3, 9], CROSS);
    // Opaque blocks all around change nothing: a cross is never hidden.
    for face in Face::ALL {
        let n = face.normal();
        set_material(&mut padded, &table, [7 + n.x, 3 + n.y, 9 + n.z], 1);
    }
    let light = common::full_light(|x, y, z| {
        if [x, y, z] == [7, 3, 9] {
            0x9312
        } else {
            0xF000
        }
    });
    let mesh = mesh(&padded, &table, Some(&light), AoPolicy::Baked);
    let cross: Vec<PackedQuad> = mesh.bucket(CUTOUT_BUCKET).to_vec();
    assert_eq!(cross.len(), 2);
    for (quad, face) in cross.iter().zip([Face::PosX, Face::PosZ]) {
        assert_eq!(quad.shape(), layout::SHAPE_CROSS);
        assert_eq!(quad.face(), face);
        assert_eq!(quad.pos(), [7, 3, 9]);
        assert_eq!(quad.ao_byte(), 0xFF);
        assert_eq!(mesh.corner_light(*quad), [LightValue(0x9312); 4]);
    }
}

#[test]
fn the_registry_classes_follow_shape_and_model() {
    let registry = BlockRegistry::default_blocks();
    let table = MaterialTable::from_registry(&registry);
    let id = |name: &str| registry.parse_state(name).unwrap().0;
    let class = |name: &str| table.class(id(name));
    assert_eq!(class("boxel:stone"), VoxelClass::Opaque);
    assert_eq!(class("boxel:glass"), VoxelClass::Transparent);
    assert_eq!(class("boxel:water"), VoxelClass::Transparent);
    assert_eq!(class("boxel:stone_slab[type=bottom]"), VoxelClass::Special);
    assert_eq!(class("boxel:stone_slab[type=top]"), VoxelClass::Special);
    assert_eq!(class("boxel:stone_slab[type=double]"), VoxelClass::Opaque);
    assert_eq!(class("boxel:short_grass"), VoxelClass::Special);
    assert_eq!(class("boxel:poppy"), VoxelClass::Special);
    assert_eq!(class("boxel:log[axis=x]"), VoxelClass::Opaque);
    let log_x = id("boxel:log[axis=x]");
    assert_eq!(table.cube_code(log_x, Face::PosX), layout::SHAPE_CUBE);
    assert_eq!(table.cube_code(log_x, Face::PosY), layout::SHAPE_TURNED);
    assert_eq!(
        table.bucket(id("boxel:glass"), Face::PosY),
        boxel_mesh::TRANSLUCENT_BUCKET
    );
}

/// A double slab (ADR 0019) is a plain stone cube to the mesher: the greedy path merges it into
/// the same quads, with the same layers, as stone in its place.
#[test]
fn double_slabs_mesh_like_stone_cubes() {
    let registry = BlockRegistry::default_blocks();
    let table = MaterialTable::from_registry(&registry);
    let stone = registry.parse_state("boxel:stone").unwrap().0;
    let double = registry
        .parse_state("boxel:stone_slab[type=double]")
        .unwrap()
        .0;
    let bottom = registry.parse_state("boxel:stone_slab").unwrap().0;
    // A 4 x 3 x 5 box of `m` with a bottom slab on top of one corner.
    let build = |m: u16| {
        let mut padded = PaddedChunk::new();
        for x in 2..6 {
            for y in 1..4 {
                for z in 3..8 {
                    set_material(&mut padded, &table, [x, y, z], m);
                }
            }
        }
        set_material(&mut padded, &table, [2, 4, 3], bottom);
        padded
    };
    let ao = AoPolicy::Baked;
    let with_stone = mesh(&build(stone), &table, None, ao);
    let with_double = mesh(&build(double), &table, None, ao);
    for bucket in 0..BUCKET_COUNT {
        assert_eq!(
            with_double.bucket(bucket),
            with_stone.bucket(bucket),
            "bucket {bucket}"
        );
    }
    // The box is six greedy quads (a cube face is never hidden by a slab), and the slab five
    // (its bottom lies on the box).
    let quads: Vec<PackedQuad> = (0..BUCKET_COUNT)
        .flat_map(|b| with_double.bucket(b).iter().copied())
        .collect();
    let cubes = quads
        .iter()
        .filter(|q| q.shape() == layout::SHAPE_CUBE)
        .count();
    assert_eq!((cubes, quads.len() - cubes), (6, 5));
}
