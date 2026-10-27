//! Edge walls (`MeshOptions::walls`, `layout::SHAPE_WALL`; ADR 0031, E-D): on each of its 6
//! borders a chunk carries the faces the chunk across would draw toward it, so a cave of the
//! chunk keeps its outer wall while that neighbor is not drawn.
//! - they are exactly the neighbor's own faces on that border (cell, face, layer, AO and corner
//!   lights), with any content and light, on every border; on the side borders, where neither
//!   voxel is an opaque cube and one stands over the neighbor's in its layer (a cave going on
//!   across), a cap of that rock's material;
//! - they come last in their face bucket ([`ChunkMesh::wall_counts`]), and the rest of the mesh
//!   is the mesh without them;
//! - off by default; a chunk of air has none.
mod common;

use boxel_light::PaddedLight;
use boxel_mesh::{
    AoPolicy, ChunkMesh, MaterialTable, MeshOptions, Mesher, PackedQuad, PaddedChunk, layout,
};
use boxel_voxel::{Face, LightValue};
use common::{Rng, full_light, set_material, test_table};
use std::collections::{BTreeMap, BTreeSet};

/// The material at world voxel `p` of a world drawn from `seed`: ground (opaque materials 1 to
/// 3) with holes, see-through cubes (cutout 4 and 5, translucent 6 and 7) and air.
fn world(seed: u64, p: [i32; 3]) -> u16 {
    let mut rng = Rng::new(
        seed ^ (p[0] as u64).wrapping_mul(0x9E37_79B9)
            ^ (p[1] as u64).wrapping_mul(0x85EB_CA6B) << 1
            ^ (p[2] as u64).wrapping_mul(0xC2B2_AE35) << 2,
    );
    match rng.below(20) {
        0..=10 => 1 + rng.below(3) as u16,
        11..=13 => 4 + rng.below(4) as u16,
        _ => 0,
    }
}

/// The sky and block light at world voxel `p`: varied, so corner lights differ.
fn light_at(seed: u64, p: [i32; 3]) -> u16 {
    let mut rng = Rng::new(seed.rotate_left(17) ^ (p[0] * 31 + p[1] * 7 + p[2]) as u64);
    (rng.below(16) as u16) << 12 | rng.below(4) as u16
}

/// The padded chunk at chunk offset `at` (in chunks) of the world, and its light.
fn chunk(seed: u64, table: &MaterialTable, at: [i32; 3]) -> (PaddedChunk, PaddedLight) {
    let mut padded = PaddedChunk::new();
    let world_of = |x: i32, y: i32, z: i32| [at[0] * 32 + x, at[1] * 32 + y, at[2] * 32 + z];
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                set_material(
                    &mut padded,
                    table,
                    [x, y, z],
                    world(seed, world_of(x, y, z)),
                );
            }
        }
    }
    let light = full_light(|x, y, z| light_at(seed, world_of(x, y, z)));
    (padded, light)
}

fn mesh(
    padded: &PaddedChunk,
    light: &PaddedLight,
    table: &MaterialTable,
    o: MeshOptions,
) -> ChunkMesh {
    let mut out = ChunkMesh::default();
    Mesher::new().mesh(padded, Some(light), table, o, &mut out);
    out
}

/// A unit face and how it looks: world cell of the face's block, face, layer, AO byte and corner
/// lights.
type Look = ([i32; 3], Face, u16, u8, [LightValue; 4]);

/// The unit faces of `quads` of `mesh` (chunk at `origin`, in voxels), each at the world cell of
/// the block it is a face of: a wall's is the cell across its border.
fn looks(mesh: &ChunkMesh, quads: &[PackedQuad], origin: [i32; 3]) -> BTreeSet<Look> {
    let mut out = BTreeSet::new();
    for &q in quads {
        let n = q.face().normal();
        let across = if q.shape() == layout::SHAPE_WALL {
            [-n.x, -n.y, -n.z]
        } else {
            [0; 3]
        };
        let light = mesh.corner_light(q);
        for (p, f) in q.unit_faces() {
            let cell = [0, 1, 2].map(|a| origin[a] + p[a] as i32 + across[a]);
            out.insert((cell, f, q.layer(), q.ao_byte(), light));
        }
    }
    out
}

#[test]
fn walls_are_the_faces_the_chunk_across_draws() {
    let table = test_table();
    for seed in 0..6u64 {
        for ao in [AoPolicy::Baked, AoPolicy::BakedLitOnly, AoPolicy::Off] {
            let options = MeshOptions::with_ao(ao);
            let (padded, light) = chunk(seed, &table, [0; 3]);
            let walled = mesh(&padded, &light, &table, options.with_walls());
            let counts = walled.wall_counts();
            for face in Face::ALL {
                let f = face.index();
                let bucket = walled.bucket(f);
                let walls = &bucket[bucket.len() - counts[f] as usize..];
                // The chunk across the border these walls close, and its faces of `face` on
                // its own border voxels toward this chunk.
                let n = face.normal();
                let at = [-n.x, -n.y, -n.z];
                let (np, nl) = chunk(seed, &table, at);
                let across = mesh(&np, &nl, &table, options);
                let (na, _, _) = face.axes();
                let border = if n.x + n.y + n.z > 0 { 31 } else { 0 };
                let theirs: Vec<PackedQuad> = across
                    .bucket(f)
                    .iter()
                    .copied()
                    .filter(|q| q.shape() == layout::SHAPE_CUBE && q.pos()[na] == border)
                    .collect();
                let origin = at.map(|c| c * 32);
                let opaque = |p: [i32; 3]| (1..=3).contains(&world(seed, p));
                let (faces, caps): (BTreeSet<Look>, BTreeSet<Look>) = looks(&walled, walls, [0; 3])
                    .into_iter()
                    .partition(|l| opaque(l.0));
                assert_eq!(
                    faces,
                    looks(&across, &theirs, origin),
                    "seed {seed}, {ao:?}, {face:?}"
                );
                // The caps: on a side border, every cell of the neighbor's border layer that is
                // no opaque cube, in front of one that is none either, under an opaque cube of
                // the layer up to the padded row over the chunk, with that cube's material.
                let side = na != 1;
                let mut expected = BTreeSet::new();
                for (cell, _, _, _, _) in &caps {
                    assert!(side, "{face:?}: a cap on a top or bottom border");
                    let over = (cell[1] + 1..=32)
                        .map(|y| world(seed, [cell[0], y, cell[2]]))
                        .find(|&m| (1..=3).contains(&m));
                    let layer = over.map(|m| table.layer(m, face));
                    assert!(
                        caps.iter().any(|l| l.0 == *cell && Some(l.2) == layer),
                        "{face:?} {cell:?}: a cap of {over:?}"
                    );
                }
                if side {
                    let (_, ua, va) = face.axes();
                    for v in 0..32 {
                        for u in 0..32 {
                            let mut c = [0; 3];
                            c[na] = if n.x + n.y + n.z > 0 { 0 } else { 31 };
                            (c[ua], c[va]) = (u, v);
                            let cell = [c[0] - n.x, c[1] - n.y, c[2] - n.z];
                            let covered = (cell[1] + 1..=32).any(|y| opaque([cell[0], y, cell[2]]));
                            if !opaque(cell) && !opaque(c) && covered {
                                expected.insert(cell);
                            }
                        }
                    }
                }
                let capped: BTreeSet<[i32; 3]> = caps.iter().map(|l| l.0).collect();
                assert_eq!(capped, expected, "seed {seed}, {ao:?}, {face:?}");
                assert!(
                    seed > 0 || !walls.is_empty(),
                    "{face:?}: a border with walls"
                );
                for q in walls {
                    assert_eq!(q.shape(), layout::SHAPE_WALL);
                    assert_eq!(q.face(), face);
                    assert_eq!(q.pos()[na], 31 - border, "{q:?}");
                }
            }
        }
    }
}

#[test]
fn walls_come_last_and_leave_the_rest_of_the_mesh_as_it_was() {
    let table = test_table();
    for seed in 10..14u64 {
        let (padded, light) = chunk(seed, &table, [0; 3]);
        let plain = mesh(&padded, &light, &table, MeshOptions::default());
        let walled = mesh(&padded, &light, &table, MeshOptions::default().with_walls());
        assert_eq!(plain.wall_counts(), [0; 6]);
        assert!(plain.quads.iter().all(|q| q.shape() != layout::SHAPE_WALL));
        let counts = walled.wall_counts();
        assert!(counts.iter().all(|&n| n > 0), "{counts:?}");
        for b in 0..8 {
            let bucket = walled.bucket(b);
            let walls = counts.get(b).map_or(0, |&n| n as usize);
            let (rest, end) = bucket.split_at(bucket.len() - walls);
            assert!(end.iter().all(|q| q.shape() == layout::SHAPE_WALL));
            assert!(rest.iter().all(|q| q.shape() != layout::SHAPE_WALL));
            assert_eq!(rest, plain.bucket(b), "bucket {b}");
        }
        // The walls' corner lights come after the others in the palette.
        if walled.light_quantization == plain.light_quantization {
            assert_eq!(&walled.palette[..plain.palette.len()], &plain.palette[..]);
        }
        assert_eq!(walled.hash, walled.compute_hash());
    }
}

#[test]
fn walls_merge_by_look_and_cover_each_border_face_once() {
    // Ground across every border, and a cave of air in this chunk touching each of them: a
    // wall per border, one quad each where the look is the same.
    let table = test_table();
    let mut padded = PaddedChunk::new();
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                let inside = (0..32).contains(&x) && (0..32).contains(&y) && (0..32).contains(&z);
                let m = if inside { 0 } else { 2 };
                set_material(&mut padded, &table, [x, y, z], m);
            }
        }
    }
    let light = PaddedLight::Uniform(LightValue(0x7000));
    let walled = mesh(
        &padded,
        &light,
        &table,
        MeshOptions::with_ao(AoPolicy::Off).with_walls(),
    );
    assert_eq!(walled.wall_counts(), [1; 6]);
    let mut seen: BTreeMap<Face, u32> = BTreeMap::new();
    for &q in &walled.quads {
        assert_eq!(q.shape(), layout::SHAPE_WALL);
        assert_eq!((q.w(), q.h()), (32, 32));
        assert_eq!(q.layer(), table.layer(2, q.face()));
        *seen.entry(q.face()).or_default() += 1;
    }
    assert_eq!(seen.len(), 6);
    // With AO the corners of each border split it.
    let ao = mesh(
        &padded,
        &light,
        &table,
        MeshOptions::with_ao(AoPolicy::Baked).with_walls(),
    );
    for f in 0..6 {
        let area: u32 = ao.bucket(f).iter().map(|q| q.w() * q.h()).sum();
        assert_eq!(area, 32 * 32, "face bucket {f}");
    }
}

#[test]
fn a_chunk_of_air_has_no_walls() {
    // The mesher's fast path: a chunk of air draws nothing, walls neither (the chunk across
    // draws its own faces when it is drawn).
    use boxel_mesh::MeshInput;
    use boxel_voxel::{BlockState, ChunkData};
    let r = boxel_voxel::testing::registry();
    let table = MaterialTable::from_registry(&r);
    let solid = r.parse_state("test:solid").expect("test:solid");
    let (air, ground) = (
        ChunkData::uniform(BlockState::AIR),
        ChunkData::uniform(solid),
    );
    let input = MeshInput {
        center: &air,
        neighbors: [Some(&ground); 26],
        light: None,
    };
    let out = Mesher::new().mesh_chunk(&input, &table, MeshOptions::default().with_walls());
    assert!(out.is_empty());
}

#[test]
fn a_cave_going_on_across_a_side_border_ends_in_a_cap() {
    // A tunnel along x through stone, rows 20 to 31 of this chunk and of the chunk across +X,
    // its ceiling in the chunks over them (rows 32 to 36): the padding sees no rock over the
    // tunnel across the border, the chunk over the neighbor does. The tunnel ends in a cap of
    // the ceiling's rock on the +X border; with the chunk over the neighbor missing (air), it
    // stays open.
    use boxel_mesh::MeshInput;
    use boxel_voxel::{BlockState, ChunkData, LocalPos, NEIGHBOR_OFFSETS};
    let r = boxel_voxel::testing::registry();
    let table = MaterialTable::from_registry(&r);
    let (stone, lamp) = (
        r.parse_state("test:solid").expect("test:solid"),
        r.parse_state("test:lamp").expect("test:lamp"),
    );
    // World voxel `p` of the scene: the tunnel (y 20 to 36, z 10 to 14) and stone; the rock
    // over y 36 is lamp, so a cap shows whose rock it took.
    let block = |p: [i32; 3]| {
        if (20..37).contains(&p[1]) && (10..15).contains(&p[2]) && p[1] < 37 && p[0] >= 0 {
            BlockState::AIR
        } else if p[1] >= 37 {
            lamp
        } else {
            stone
        }
    };
    let chunk = |c: [i32; 3]| {
        let mut data = ChunkData::uniform(stone);
        for i in 0..32 * 32 * 32 {
            let p = LocalPos::from_index(i);
            let w = [
                c[0] * 32 + p.x() as i32,
                c[1] * 32 + p.y() as i32,
                c[2] * 32 + p.z() as i32,
            ];
            data.set(p, block(w));
        }
        data
    };
    let center = chunk([0, 0, 0]);
    let around: Vec<ChunkData> = NEIGHBOR_OFFSETS
        .iter()
        .map(|o| chunk([o.x, o.y, o.z]))
        .collect();
    let air = ChunkData::uniform(BlockState::AIR);
    let over_across = NEIGHBOR_OFFSETS
        .iter()
        .position(|o| (o.x, o.y, o.z) == (1, 1, 0))
        .expect("an edge neighbor");
    let caps = |missing: bool| {
        let input = MeshInput {
            center: &center,
            neighbors: std::array::from_fn(|i| {
                Some(if missing && i == over_across {
                    &air
                } else {
                    &around[i]
                })
            }),
            light: None,
        };
        let mesh = Mesher::new().mesh_chunk(&input, &table, MeshOptions::default().with_walls());
        let f = Face::NegX.index();
        let bucket = mesh.bucket(f);
        let walls = &bucket[bucket.len() - mesh.wall_counts()[f] as usize..];
        walls
            .iter()
            .flat_map(|q| q.unit_faces().map(move |(p, _)| (p, q.layer())))
            .filter(|(p, _)| p[0] == 31 && (10..15).contains(&p[2]) && p[1] >= 20)
            .collect::<Vec<_>>()
    };
    let capped = caps(false);
    // Rows 20 to 31 of the tunnel's 5 columns, each a cap of the lamp rock.
    assert_eq!(capped.len(), 12 * 5, "{capped:?}");
    let layer = table.layer(lamp.0, Face::NegX);
    assert!(capped.iter().all(|&(_, l)| l == layer), "{capped:?}");
    assert!(caps(true).is_empty());
}
