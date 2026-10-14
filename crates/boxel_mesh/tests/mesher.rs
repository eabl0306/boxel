//! Mesher scenarios: unit quads, full chunks, worst-case checkerboards, bucket routing,
//! texture layers, gather and the `mesh_chunk` fast paths.
mod common;

use boxel_light::PaddedLight;
use boxel_mesh::{
    AoPolicy, BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, FLUID_SURFACE_BIT, FULL_SKY_ENTRY,
    LightQuantization, MAX_QUADS_PER_BUCKET, MAX_QUADS_PER_FACE_BUCKET, MaterialTable, MeshInput,
    MeshOptions, Mesher, PackedQuad, PaddedChunk, TRANSLUCENT_BUCKET, VoxelClass, layout,
};
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, Face, LightValue, LocalPos,
    NEIGHBOR_OFFSETS,
};
use common::{
    CUTOUT, NO_GEOMETRY, OPAQUE, Repr, TRANSLUCENT, chunk_repr, class_at, dense_from,
    neighbor_slot, set_material, test_table, verify_mesh,
};

/// The default options (AO baked on lit faces). Without light every face is lit, so these
/// scenes get AO wherever an opaque voxel touches a face's front layer.
const OPTIONS: MeshOptions = MeshOptions::with_ao(AoPolicy::BakedLitOnly);

fn registry() -> (BlockRegistry, MaterialTable) {
    let registry = boxel_demo::blocks::registry();
    let table = MaterialTable::from_registry(&registry);
    (registry, table)
}

fn block(registry: &BlockRegistry, name: &str) -> BlockState {
    registry
        .by_name(&format!("boxel:{name}"))
        .expect("built-in block")
}

fn isolated(center: &ChunkData) -> MeshInput<'_> {
    MeshInput {
        center,
        neighbors: [None; 26],
        light: None,
    }
}

fn surrounded<'a>(center: &'a ChunkData, neighbor: &'a ChunkData) -> MeshInput<'a> {
    MeshInput {
        center,
        neighbors: [Some(neighbor); 26],
        light: None,
    }
}

/// Meshes `padded` with a fresh mesher.
fn mesh_padded(padded: &PaddedChunk, table: &MaterialTable) -> ChunkMesh {
    let mut out = ChunkMesh::default();
    Mesher::new().mesh(padded, None, table, OPTIONS, &mut out);
    out
}

/// Gathers `input` with a fresh mesher.
fn gathered(input: &MeshInput<'_>, table: &MaterialTable) -> PaddedChunk {
    let mut padded = PaddedChunk::new();
    Mesher::new().gather(input, table, &mut padded);
    padded
}

fn single_voxel_chunk(state: BlockState, p: [u32; 3], repr: Repr) -> ChunkData {
    chunk_repr(
        &dense_from(|x, y, z| {
            if [x as u32, y as u32, z as u32] == p {
                state.0
            } else {
                0
            }
        }),
        repr,
    )
}

#[test]
fn single_voxel_gives_six_unit_quads() {
    let (registry, table) = registry();
    let stone = block(&registry, "stone");
    let mut mesher = Mesher::new();
    for (p, repr) in [
        ([5, 7, 9], Repr::Direct),
        ([0, 0, 0], Repr::Packed),
        ([31, 31, 31], Repr::Direct),
        ([0, 31, 17], Repr::Packed),
    ] {
        let center = single_voxel_chunk(stone, p, repr);
        let mesh = mesher.mesh_chunk(&isolated(&center), &table, OPTIONS);
        assert_eq!(
            mesh.bucket_counts,
            [1, 1, 1, 1, 1, 1, 0, 0],
            "voxel at {p:?}"
        );
        for face in Face::ALL {
            let quad = mesh.bucket(face.index())[0];
            let layer = registry.face_texture(stone, face).0;
            assert_eq!(
                quad,
                PackedQuad::new(p, 1, 1, face, layer),
                "{face:?} at {p:?}"
            );
        }
        verify_mesh(&gathered(&isolated(&center), &table), &table, &mesh);
    }
}

#[test]
fn full_chunk_with_empty_neighbors_gives_six_full_quads() {
    let (registry, table) = registry();
    let stone = block(&registry, "stone");
    let uniform = ChunkData::uniform(stone);
    let dense = chunk_repr(&dense_from(|_, _, _| stone.0), Repr::Direct);
    let mut mesher = Mesher::new();
    for center in [&uniform, &dense] {
        let mesh = mesher.mesh_chunk(&isolated(center), &table, OPTIONS);
        assert_eq!(mesh.bucket_counts, [1, 1, 1, 1, 1, 1, 0, 0]);
        let layer = registry.face_texture(stone, Face::PosY).0;
        let expected = [
            (Face::PosY, [0, 31, 0]),
            (Face::NegY, [0, 0, 0]),
            (Face::PosX, [31, 0, 0]),
            (Face::NegX, [0, 0, 0]),
            (Face::PosZ, [0, 0, 31]),
            (Face::NegZ, [0, 0, 0]),
        ];
        for (face, pos) in expected {
            assert_eq!(
                mesh.bucket(face.index()),
                &[PackedQuad::new(pos, 32, 32, face, layer)]
            );
        }
    }
}

#[test]
fn checkerboard_fills_every_opaque_face_bucket() {
    let (registry, table) = registry();
    let stone = block(&registry, "stone").0;
    let center = chunk_repr(
        &dense_from(|x, y, z| if (x + y + z) % 2 == 0 { stone } else { 0 }),
        Repr::Packed,
    );
    let mesh = Mesher::new().mesh_chunk(&isolated(&center), &table, OPTIONS);
    for face in Face::ALL {
        assert_eq!(mesh.bucket_counts[face.index()], MAX_QUADS_PER_FACE_BUCKET);
    }
    assert_eq!(mesh.bucket_counts[CUTOUT_BUCKET], 0);
    assert_eq!(mesh.bucket_counts[TRANSLUCENT_BUCKET], 0);
    assert!(mesh.quads.iter().all(|q| q.w() == 1 && q.h() == 1));
    verify_mesh(&gathered(&isolated(&center), &table), &table, &mesh);
}

#[test]
fn transparent_checkerboards_fill_their_shared_bucket() {
    let (registry, table) = registry();
    for (name, bucket) in [("leaves", CUTOUT_BUCKET), ("water", TRANSLUCENT_BUCKET)] {
        let state = block(&registry, name).0;
        let center = chunk_repr(
            &dense_from(|x, y, z| if (x + y + z) % 2 == 0 { state } else { 0 }),
            Repr::Direct,
        );
        let mesh = Mesher::new().mesh_chunk(&isolated(&center), &table, OPTIONS);
        let mut expected = [0; BUCKET_COUNT];
        expected[bucket] = MAX_QUADS_PER_BUCKET;
        assert_eq!(mesh.bucket_counts, expected, "{name}");
        verify_mesh(&gathered(&isolated(&center), &table), &table, &mesh);
    }
}

#[test]
fn default_registry_worst_case_fits_the_shared_index_buffer() {
    // One material per transparent bucket (leaves, water): a leaves/water checkerboard filling
    // the chunk and its padding shows every face, and each bucket reaches exactly the bound.
    let (registry, table) = registry();
    let [leaves, water] = ["leaves", "water"].map(|n| block(&registry, n).0);
    let mut padded = PaddedChunk::new();
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                let material = if (x + y + z) % 2 == 0 { leaves } else { water };
                set_material(&mut padded, &table, [x, y, z], material);
            }
        }
    }
    let mesh = mesh_padded(&padded, &table);
    let mut expected = [0; BUCKET_COUNT];
    expected[CUTOUT_BUCKET] = MAX_QUADS_PER_BUCKET;
    expected[TRANSLUCENT_BUCKET] = MAX_QUADS_PER_BUCKET;
    assert_eq!(mesh.bucket_counts, expected);
    verify_mesh(&padded, &table, &mesh);
}

#[test]
fn two_materials_in_one_transparent_bucket_show_every_face() {
    // Two alternating cutout (or translucent) materials hide nothing from each other: every
    // face of every voxel lands in one bucket, 6 * 32^3 quads (twice MAX_QUADS_PER_BUCKET).
    // The mesher keeps all of them; the renderer draws the first MAX_QUADS_PER_BUCKET (open
    // risk in docs/informe-m0.md).
    let table = test_table();
    for pair in [CUTOUT, TRANSLUCENT] {
        let mut padded = PaddedChunk::new();
        for y in -1..=32 {
            for x in -1..=32 {
                for z in -1..=32 {
                    let material = pair[((x + y + z) & 1) as usize];
                    set_material(&mut padded, &table, [x, y, z], material);
                }
            }
        }
        let mesh = mesh_padded(&padded, &table);
        let bucket = table.bucket(pair[0], Face::PosY);
        assert_eq!(mesh.bucket_counts[bucket] as usize, 6 * CHUNK_VOLUME);
        assert_eq!(mesh.total_quads() as usize, 6 * CHUNK_VOLUME);
        verify_mesh(&padded, &table, &mesh);
    }
}

#[test]
fn buckets_follow_the_render_shape() {
    let (registry, table) = registry();
    let [stone, leaves, water] = ["stone", "leaves", "water"].map(|n| block(&registry, n));
    let center = chunk_repr(
        &dense_from(|x, y, z| match (x, y, z) {
            (2, 2, 2) => stone.0,
            (10, 10, 10) => leaves.0,
            (20, 20, 20) => water.0,
            _ => 0,
        }),
        Repr::Packed,
    );
    let mesh = Mesher::new().mesh_chunk(&isolated(&center), &table, OPTIONS);
    assert_eq!(mesh.bucket_counts, [1, 1, 1, 1, 1, 1, 6, 6]);
    for face in Face::ALL {
        assert_eq!(mesh.bucket(face.index())[0].pos(), [2, 2, 2]);
    }
    for (bucket, pos, state) in [
        (CUTOUT_BUCKET, [10, 10, 10], leaves),
        (TRANSLUCENT_BUCKET, [20, 20, 20], water),
    ] {
        let mut faces: Vec<_> = mesh.bucket(bucket).iter().map(|q| q.face()).collect();
        faces.sort();
        assert_eq!(faces, Face::ALL);
        for quad in mesh.bucket(bucket) {
            assert_eq!(quad.pos(), pos);
            assert_eq!(quad.layer(), registry.face_texture(state, quad.face()).0);
        }
    }
    // Same routing straight from the table.
    for face in Face::ALL {
        assert_eq!(table.bucket(stone.0, face), face.index());
        assert_eq!(table.bucket(leaves.0, face), CUTOUT_BUCKET);
        assert_eq!(table.bucket(water.0, face), TRANSLUCENT_BUCKET);
    }
}

#[test]
fn quads_carry_the_texture_layer_of_their_face() {
    let (registry, table) = registry();
    let grass = block(&registry, "grass");
    let log = block(&registry, "log");
    // A 4x3x2 grass box and a log column: merged quads keep the per-face layer.
    let center = chunk_repr(
        &dense_from(|x, y, z| {
            if (4..8).contains(&x) && (3..6).contains(&y) && (10..12).contains(&z) {
                grass.0
            } else if x == 20 && z == 20 && (0..9).contains(&y) {
                log.0
            } else {
                0
            }
        }),
        Repr::Direct,
    );
    let mesh = Mesher::new().mesh_chunk(&isolated(&center), &table, OPTIONS);
    assert_eq!(mesh.total_quads(), 12, "two boxes, six merged quads each");
    for quad in &mesh.quads {
        let state = if quad.pos()[0] >= 20 { log } else { grass };
        assert_eq!(
            quad.layer(),
            registry.face_texture(state, quad.face()).0,
            "{quad:?}"
        );
    }
    let top = mesh.bucket(Face::PosY.index());
    let grass_top = top.iter().find(|q| q.pos()[0] < 20).expect("grass top");
    assert_eq!(
        (grass_top.pos(), grass_top.w(), grass_top.h()),
        ([4, 5, 10], 4, 2)
    );
    let side = mesh.bucket(Face::PosX.index());
    let grass_side = side.iter().find(|q| q.pos()[0] < 20).expect("grass +X");
    // +X: u = Z, v = Y.
    assert_eq!(
        (grass_side.pos(), grass_side.w(), grass_side.h()),
        ([7, 3, 10], 2, 3)
    );
    verify_mesh(&gathered(&isolated(&center), &table), &table, &mesh);
}

#[test]
fn emissive_blocks_flag_their_quads() {
    let (registry, table) = registry();
    let stone = block(&registry, "stone");
    let emitters: Vec<BlockState> = boxel_demo::blocks::LAMPS
        .iter()
        .map(|lamp| block(&registry, lamp.short_name()))
        .chain([block(&registry, "lava")])
        .collect();
    for &s in &emitters {
        assert!(table.emissive(s.0), "{s:?}");
    }
    for name in ["stone", "grass", "leaves", "water", "planks"] {
        assert!(!table.emissive(block(&registry, name).0), "{name}");
    }
    assert!(!table.emissive(0) && !table.emissive(u16::MAX));
    // The demo draws every emitter full bright: no lit emitter, so no chunk is looked at.
    assert!(!table.has_lit_emitters());
    // A stone floor with one emitter of each kind on it, lit and unlit: the emitters' quads,
    // and only theirs, carry the flag (verify_mesh checks every unit face).
    let center = chunk_repr(
        &dense_from(|x, y, z| match (x, y, z) {
            (_, 0, _) => stone.0,
            (x, 1, 8) if (2..2 + emitters.len() as i32 * 3).contains(&x) && x % 3 == 2 => {
                emitters[(x as usize - 2) / 3].0
            }
            _ => 0,
        }),
        Repr::Packed,
    );
    let padded = gathered(&isolated(&center), &table);
    assert!(!padded.lit_emitters);
    let light = common::full_light(|x, y, z| {
        let d = ((x - 8).abs() + (y - 1).abs() + (z - 8).abs()) as u8;
        LightValue::new(15, 14u8.saturating_sub(d), 0, 0).0
    });
    for light in [None, Some(&light)] {
        let mut mesh = ChunkMesh::default();
        Mesher::new().mesh(&padded, light, &table, OPTIONS, &mut mesh);
        verify_mesh(&padded, &table, &mesh);
        let flagged = mesh.quads.iter().filter(|q| q.is_emissive()).count();
        assert_eq!(
            flagged,
            5 * emitters.len(),
            "5 faces per emitter (the floor hides one)"
        );
        // Cube faces, but lava's: an opaque fluid whose top slopes down to the air around it
        // (ADR 0025), drawn in the opaque face buckets, its still texture on the level top
        // and its flow texture on the sides. A lone lava block's 4 corners are at one height,
        // so its top stays in the +Y bucket (a sloping one would go to the cutout bucket).
        let lava = block(&registry, "lava").0;
        let lava = [Face::PosY, Face::PosX].map(|f| table.layer(lava, f));
        assert_ne!(lava[0], lava[1]);
        for (b, q) in (0..BUCKET_COUNT).flat_map(|b| mesh.bucket(b).iter().map(move |q| (b, q))) {
            if lava.contains(&q.layer()) {
                assert_eq!(q.shape(), layout::SHAPE_FLUID_SLOPE, "{q:?}");
                assert_eq!(b, q.face().index(), "{q:?}");
                let h = q.fluid_heights().expect("a sloped cell");
                assert!(h.iter().all(|&c| c == h[0]), "{q:?}");
            } else {
                assert_eq!(q.shape(), layout::SHAPE_CUBE, "{q:?}");
            }
        }
    }
    // A table without emitters (from_parts) flags nothing until set_emissive.
    let mut parts = test_table();
    assert!(!parts.emissive(OPAQUE[0]));
    parts.set_emissive(OPAQUE[0], true);
    assert!(parts.emissive(OPAQUE[0]) && !parts.emissive(OPAQUE[1]));
}

/// The registry of [`common::glow_builder`] with the blocks `marked` drawn full bright, and its
/// table.
fn glow_registry(marked: &[&str]) -> (BlockRegistry, MaterialTable) {
    let mut b = common::glow_builder();
    for name in marked {
        let block = b.block_by_name(name).expect(name);
        b.set_full_bright(block, |_| true).expect(name);
    }
    let registry = b.build();
    let table = MaterialTable::from_registry(&registry);
    (registry, table)
}

#[test]
fn the_emissive_flag_follows_full_bright() {
    let mut b = common::glow_builder();
    let ore = b.block_by_name("test:ember_ore").expect("test:ember_ore");
    b.set_full_bright(ore, |v| v.get("lit") == Some("true"))
        .expect("test:ember_ore");
    let paint = b.block_by_name("test:paint").expect("test:paint");
    b.set_full_bright(paint, |_| true).expect("test:paint");
    let registry = b.build();
    let table = MaterialTable::from_registry(&registry);
    let mut glowing = 0;
    for s in (0..registry.len() as u16).map(BlockState) {
        let m = registry.material(s);
        if m != 0 {
            let name = registry.state_name(s);
            assert_eq!(table.emissive(m), registry.is_full_bright(s), "{name}");
            glowing += usize::from(table.emissive(m));
            // Every other emitter is lit by its own light.
            let lit = if table.emissive(m) {
                LightValue::DARK
            } else {
                registry.emission(s)
            };
            assert_eq!(table.lit_emission(m), lit, "{name}");
        }
    }
    assert!(table.has_lit_emitters());
    assert!(!table.emissive(0) && table.lit_emission(0).is_dark());
    // test:lamp, the 16 levels of test:hot, the lit ore and the paint; not the unmarked
    // emitters.
    assert_eq!(glowing, 1 + 16 + 1 + 1);
}

/// A quad decoded: its geometry and texture, AO and corner lights (palette order aside).
fn decoded(mesh: &ChunkMesh, q: PackedQuad) -> (u32, u16, u8, [LightValue; 4]) {
    (q.geo, q.layer(), q.ao_byte(), mesh.corner_light(q))
}

#[test]
fn a_lit_emitter_is_drawn_flat_at_its_own_light() {
    let (plain, plain_table) = glow_registry(&[]);
    let (marked, marked_table) =
        glow_registry(&["test:ember", "test:ember_plant", "test:ember_post"]);
    let by = |r: &BlockRegistry, name: &str| r.parse_state(name).expect(name);
    let floor = by(&plain, "test:solid");
    // A cave: no sky, a red light falling off around the block, no green; the floor under it
    // gives AO. The emission [3, 1, 0] is below the red around it and above the green.
    let light = common::full_light(|x, y, z| {
        let d = ((x - 8).abs() + (y - 1).abs() + (z - 8).abs()) as u8;
        LightValue::new(0, 13u8.saturating_sub(d), 0, 0).0
    });
    let light_at = |p: [i32; 3]| light.get(p[0], p[1], p[2]);
    let emission = LightValue::from_rgb(common::EMBER);
    let mesh = |table: &MaterialTable, s: BlockState| {
        let center = chunk_repr(
            &dense_from(|x, y, z| match (x, y, z) {
                (_, 0, _) => floor.0,
                (8, 1, 8) => s.0,
                _ => 0,
            }),
            Repr::Packed,
        );
        let padded = gathered(&isolated(&center), table);
        let mut out = ChunkMesh::default();
        Mesher::new().mesh(&padded, Some(&light), table, OPTIONS, &mut out);
        (padded, out)
    };
    let flag = layout::FLAG_EMISSIVE << layout::GEO_FLAGS_SHIFT;
    let own = |q: &PackedQuad| q.unit_faces().any(|(p, _)| p == [8, 1, 8]);
    for (looks, ember, special) in [
        ("test:solid", "test:ember", None),
        ("test:plant", "test:ember_plant", Some(layout::SHAPE_CROSS)),
        ("test:post", "test:ember_post", Some(layout::SHAPE_BOX)),
    ] {
        let (_, like) = mesh(&plain_table, by(&plain, looks));
        let (padded, lit) = mesh(&plain_table, by(&plain, ember));
        assert!(padded.lit_emitters, "{ember}");
        assert!(plain_table.has_lit_emitters());
        assert_eq!(
            plain_table.lit_emission(by(&plain, ember).0),
            emission,
            "{ember}"
        );
        // Unmarked: its faces are flat, AO open, at the light they read raised to its emission.
        let faces: Vec<&PackedQuad> = lit.quads.iter().filter(|q| own(q)).collect();
        assert!(!faces.is_empty(), "{ember}");
        for q in &faces {
            assert!(!q.is_emissive(), "{ember}: {q:?}");
            assert_eq!(q.ao_byte(), 0xFF, "{ember}: {q:?}");
            let n = q.face().normal().to_array();
            let front = [8 + n[0], 1 + n[1], 8 + n[2]];
            // A cross reads its own cell, a box face against the floor too; every other face
            // the voxel in front of it.
            let cell = match special {
                Some(layout::SHAPE_CROSS) => [8, 1, 8],
                Some(_) if q.face() == Face::NegY => [8, 1, 8],
                _ => front,
            };
            let want = light_at(cell).max(emission);
            assert_eq!(
                want.rgb()[1],
                common::EMBER[1],
                "the emission lifts the green"
            );
            assert!(want.red() > common::EMBER[0], "the red around wins");
            assert_eq!(lit.corner_light(**q), [want; 4], "{ember}: {q:?}");
        }
        match special {
            // The checks of the greedy path (verify_mesh only knows cubes).
            None => {
                verify_mesh(&padded, &plain_table, &lit);
                common::verify_shading(&padded, &plain_table, Some(&light), OPTIONS.ao, &lit);
                assert_eq!(faces.len(), 5, "{ember}: the floor hides one face");
            }
            Some(shape) => assert!(faces.iter().all(|q| q.shape() == shape), "{ember}"),
        }
        // The other quads (the floor) are the look-alike's.
        let rest = |m: &ChunkMesh| {
            let mut v: Vec<_> = m
                .quads
                .iter()
                .filter(|q| !own(q))
                .map(|q| decoded(m, *q))
                .collect();
            v.sort_unstable();
            v
        };
        assert_eq!(rest(&lit), rest(&like), "{ember}");
        // Marked, it is drawn full bright: the look-alike's quads in the same order, its own
        // with the flag bit and no other change.
        let (_, glowing) = mesh(&marked_table, by(&marked, ember));
        assert_eq!(
            marked_table.lit_emission(by(&marked, ember).0),
            LightValue::DARK
        );
        assert_eq!(glowing.palette, like.palette, "{ember}");
        assert_eq!(glowing.bucket_counts, like.bucket_counts, "{ember}");
        assert_eq!(glowing.quads.len(), like.quads.len(), "{ember}");
        for (a, b) in like.quads.iter().zip(&glowing.quads) {
            assert_eq!(a.mat, b.mat, "{ember}");
            let expected = if own(b) { flag } else { 0 };
            assert_eq!(a.geo ^ b.geo, expected, "{ember}: {a:?} {b:?}");
        }
    }
}

#[test]
fn a_lit_emitter_is_flat_under_a_uniform_light_too() {
    let (registry, table) = glow_registry(&[]);
    let by = |name: &str| registry.parse_state(name).expect(name);
    let (floor, ember) = (by("test:solid"), by("test:ember"));
    let center = chunk_repr(
        &dense_from(|x, y, z| match (x, y, z) {
            (_, 0, _) => floor.0,
            (8, 1, 8) | (20, 1, 20) => ember.0,
            _ => 0,
        }),
        Repr::Packed,
    );
    let padded = gathered(&isolated(&center), &table);
    let emission = LightValue::from_rgb(common::EMBER);
    let dark = PaddedLight::Uniform(LightValue::DARK);
    let reddish = PaddedLight::Uniform(LightValue::new(9, 7, 0, 0));
    for (light, v) in [
        (None, LightValue::FULL_SKY),
        (Some(&dark), LightValue::DARK),
        (Some(&reddish), LightValue::new(9, 7, 0, 0)),
    ] {
        for policy in [AoPolicy::Off, AoPolicy::Baked, AoPolicy::BakedLitOnly] {
            let mut out = ChunkMesh::default();
            let options = MeshOptions::with_ao(policy);
            Mesher::new().mesh(&padded, light, &table, options, &mut out);
            verify_mesh(&padded, &table, &out);
            common::verify_shading(&padded, &table, light, policy, &out);
            let at = |p| {
                out.quads
                    .iter()
                    .filter(move |q| q.unit_faces().any(|(u, _)| u == p))
            };
            for p in [[8, 1, 8], [20, 1, 20]] {
                assert_eq!(at(p).count(), 5, "{light:?} {policy:?}");
                for q in at(p) {
                    assert_eq!(q.ao_byte(), 0xFF, "{light:?} {policy:?}");
                    assert_eq!(out.corner_light(*q), [v.max(emission); 4], "{light:?}");
                }
            }
            // The floor keeps the uniform light.
            let floor = quad_at(&out, [0, 0, 0], Face::PosY);
            assert_eq!(out.corner_light(floor), [v; 4], "{light:?} {policy:?}");
        }
    }
}

#[test]
fn gather_with_uniform_neighbors() {
    let table = test_table();
    let center = chunk_repr(
        &dense_from(|x, y, z| if (x + 2 * y + 3 * z) % 5 == 0 { 2 } else { 0 }),
        Repr::Packed,
    );
    let uniforms: Vec<ChunkData> = (0..26)
        .map(|i| {
            // Cycle through opaque, cutout, translucent, air and a no-geometry state.
            let state = [OPAQUE[0], CUTOUT[1], TRANSLUCENT[0], 0, NO_GEOMETRY][i % 5];
            ChunkData::uniform(BlockState(state))
        })
        .collect();
    let mut neighbors = [None; 26];
    for (slot, chunk) in neighbors.iter_mut().zip(&uniforms) {
        *slot = Some(chunk);
    }
    let input = MeshInput {
        center: &center,
        neighbors,
        light: None,
    };
    let padded = gathered(&input, &table);
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                let inside = |c: i32| (0..32).contains(&c);
                let state = if inside(x) && inside(y) && inside(z) {
                    center.get(LocalPos::new(x as u32, y as u32, z as u32)).0
                } else {
                    let d = |c: i32| c.div_euclid(32);
                    uniforms[neighbor_slot(d(x), d(y), d(z))]
                        .as_uniform()
                        .expect("uniform")
                        .0
                };
                let class = table.class(state);
                let material = if class == VoxelClass::Empty { 0 } else { state };
                assert_eq!(padded.get(x, y, z), material, "voxel at {x},{y},{z}");
                assert_eq!(class_at(&padded, x, y, z), class, "class at {x},{y},{z}");
            }
        }
    }
    verify_mesh(&padded, &table, &mesh_padded(&padded, &table));
}

#[test]
fn gather_clears_the_output_first() {
    let table = test_table();
    let center = ChunkData::uniform(BlockState(TRANSLUCENT[1]));
    let input = isolated(&center);
    let mut dirty = PaddedChunk::new();
    for p in [
        [-1, -1, -1],
        [32, 5, 7],
        [3, 32, 0],
        [10, 10, 32],
        [16, 16, 16],
    ] {
        set_material(&mut dirty, &table, p, OPAQUE[2]);
    }
    let mut mesher = Mesher::new();
    mesher.gather(&input, &table, &mut dirty);
    assert_eq!(dirty.voxels, gathered(&input, &table).voxels);
    assert_eq!(dirty.opaque, gathered(&input, &table).opaque);
    assert_eq!(dirty.transparent, gathered(&input, &table).transparent);
}

#[test]
fn missing_and_air_neighbors_expose_borders_and_opaque_neighbors_hide_them() {
    let table = test_table();
    let stone = ChunkData::uniform(BlockState(OPAQUE[0]));
    let other_stone = ChunkData::uniform(BlockState(OPAQUE[1]));
    let leaves = ChunkData::uniform(BlockState(CUTOUT[0]));
    let air = ChunkData::uniform(BlockState::AIR);
    let mut mesher = Mesher::new();
    for (open, neighbor, expect_open) in [
        (Face::PosX, None, true),
        (Face::NegY, Some(&air), true),
        (Face::PosZ, Some(&leaves), true),
        (Face::NegX, Some(&other_stone), false),
    ] {
        let mut input = surrounded(&stone, &stone);
        input.neighbors[neighbor_slot(open.normal().x, open.normal().y, open.normal().z)] =
            neighbor;
        // AO off: the stone edge neighbors around the open face would shade (and split) its
        // border rows.
        let mesh = mesher.mesh_chunk(&input, &table, MeshOptions::with_ao(AoPolicy::Off));
        let mut expected = [0; BUCKET_COUNT];
        if expect_open {
            expected[open.index()] = 1;
        }
        assert_eq!(mesh.bucket_counts, expected, "{open:?} {neighbor:?}");
        verify_mesh(&gathered(&input, &table), &table, &mesh);
    }
}

#[test]
fn mesh_chunk_fast_paths_match_the_slow_path() {
    let table = test_table();
    let air = ChunkData::uniform(BlockState::AIR);
    let no_geometry = ChunkData::uniform(BlockState(NO_GEOMETRY));
    let out_of_table = ChunkData::uniform(BlockState(500));
    let stone = ChunkData::uniform(BlockState(OPAQUE[0]));
    let dirt = ChunkData::uniform(BlockState(OPAQUE[1]));
    let water = ChunkData::uniform(BlockState(TRANSLUCENT[0]));
    let mut mesher = Mesher::new();
    let slow = |input: &MeshInput<'_>| mesh_padded(&gathered(input, &table), &table);

    // Uniform air (and states without geometry) never produce quads.
    for center in [&air, &no_geometry, &out_of_table] {
        for input in [isolated(center), surrounded(center, &stone)] {
            assert!(mesher.mesh_chunk(&input, &table, OPTIONS).is_empty());
            assert!(slow(&input).is_empty());
        }
    }
    // Uniform opaque buried in uniform opaque face neighbors (edges and corners do not matter).
    let mut buried = MeshInput {
        center: &stone,
        neighbors: [None; 26],
        light: None,
    };
    for face in Face::ALL {
        let n = face.normal();
        buried.neighbors[neighbor_slot(n.x, n.y, n.z)] = Some(&dirt);
    }
    assert!(mesher.mesh_chunk(&buried, &table, OPTIONS).is_empty());
    assert!(slow(&buried).is_empty());
    // Uniform water in water: no fast path, still empty (bgm hides same-material faces).
    let flooded = surrounded(&water, &water);
    assert!(mesher.mesh_chunk(&flooded, &table, OPTIONS).is_empty());
    // Not buried: the fast path must not trigger.
    let isolated_stone = isolated(&stone);
    let mesh = mesher.mesh_chunk(&isolated_stone, &table, OPTIONS);
    assert_eq!(mesh.total_quads(), 6);
    assert_eq!(mesh, slow(&isolated_stone));
}

#[test]
fn mesher_and_output_are_reusable() {
    let table = test_table();
    let mut mesher = Mesher::new();
    let mut out = ChunkMesh::default();
    let mut padded = PaddedChunk::new();
    let chunks = [
        chunk_repr(
            &dense_from(|x, y, z| if (x ^ y ^ z) & 3 == 0 { 3 } else { 0 }),
            Repr::Packed,
        ),
        chunk_repr(
            &dense_from(|x, y, _| if y < x { 1 } else { 6 }),
            Repr::Direct,
        ),
        ChunkData::uniform(BlockState(CUTOUT[0])),
    ];
    for center in &chunks {
        let neighbors: [Option<&ChunkData>; 26] =
            std::array::from_fn(|i| (i % 3 != 0).then_some(&chunks[i % chunks.len()]));
        let input = MeshInput {
            center,
            neighbors,
            light: None,
        };
        mesher.gather(&input, &table, &mut padded);
        mesher.mesh(&padded, None, &table, OPTIONS, &mut out);
        let fresh = mesh_padded(&gathered(&input, &table), &table);
        assert_eq!(out, fresh);
        assert_eq!(mesher.mesh_chunk(&input, &table, OPTIONS), fresh);
        verify_mesh(&padded, &table, &out);
    }
}

#[test]
fn transparent_neighbors_of_different_materials_show_both_faces() {
    let table = test_table();
    let mut padded = PaddedChunk::new();
    // Two cutout materials, a translucent one and a same-material pair in a row along X.
    for (x, material) in [
        (3, CUTOUT[0]),
        (4, CUTOUT[1]),
        (5, TRANSLUCENT[0]),
        (6, TRANSLUCENT[0]),
        (7, OPAQUE[0]),
    ] {
        set_material(&mut padded, &table, [x, 9, 9], material);
    }
    let mesh = mesh_padded(&padded, &table);
    let faces = verify_mesh(&padded, &table, &mesh);
    // 5 voxels * 6 faces, minus the hidden pair between the two water voxels (2 faces)
    // and the translucent face against the opaque voxel (1 face).
    assert_eq!(faces, 30 - 2 - 1);
    // Opaque next to translucent keeps its face.
    assert!(
        mesh.bucket(Face::NegX.index())
            .iter()
            .any(|q| q.pos() == [7, 9, 9])
    );
}

#[test]
fn every_neighbor_slot_is_read_from_the_right_place() {
    // A marker voxel in each neighbor at the cell that touches the center chunk.
    let table = test_table();
    let neighbors: Vec<ChunkData> = NEIGHBOR_OFFSETS
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let target = |d: i32| if d < 0 { 31 } else { 0 };
            let (tx, ty, tz) = (target(o.x), target(o.y), target(o.z));
            let material = [OPAQUE[0], OPAQUE[1], OPAQUE[2], CUTOUT[0], TRANSLUCENT[1]][i % 5];
            chunk_repr(
                &dense_from(|x, y, z| {
                    let hit = |c: i32, d: i32, t: i32| d == 0 || c == t;
                    if hit(x, o.x, tx) && hit(y, o.y, ty) && hit(z, o.z, tz) && (x + y + z) % 3 == 0
                    {
                        material
                    } else {
                        0
                    }
                }),
                if i % 2 == 0 {
                    Repr::Direct
                } else {
                    Repr::Packed
                },
            )
        })
        .collect();
    let center = ChunkData::uniform(BlockState::AIR);
    let mut slots = [None; 26];
    for (slot, chunk) in slots.iter_mut().zip(&neighbors) {
        *slot = Some(chunk);
    }
    let padded = gathered(
        &MeshInput {
            center: &center,
            neighbors: slots,
            light: None,
        },
        &table,
    );
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                let (dx, lx) = common::split(x);
                let (dy, ly) = common::split(y);
                let (dz, lz) = common::split(z);
                let expected = if (dx, dy, dz) == (0, 0, 0) {
                    0
                } else {
                    neighbors[neighbor_slot(dx, dy, dz)]
                        .get(LocalPos::new(lx, ly, lz))
                        .0
                };
                assert_eq!(padded.get(x, y, z), expected, "voxel at {x},{y},{z}");
                assert_eq!(
                    class_at(&padded, x, y, z),
                    table.class(expected),
                    "class at {x},{y},{z}"
                );
            }
        }
    }
}

/// The face of `mesh` that covers unit face `(p, face)`.
fn quad_at(mesh: &ChunkMesh, p: [u32; 3], face: Face) -> PackedQuad {
    *mesh
        .quads
        .iter()
        .find(|q| q.unit_faces().any(|u| u == (p, face)))
        .unwrap_or_else(|| panic!("no quad covers {p:?} {face:?}"))
}

#[test]
fn ao_shades_the_floor_along_a_wall_and_follows_the_policy() {
    let table = test_table();
    // A stone floor at y = 0, a stone wall at x = 10 (y 1..=3) and a leaves wall at x = 20.
    let center = chunk_repr(
        &dense_from(|x, y, _| match (x, y) {
            (_, 0) => OPAQUE[0],
            (10, 1..=3) => OPAQUE[1],
            (20, 1..=3) => CUTOUT[0],
            _ => 0,
        }),
        Repr::Direct,
    );
    let padded = gathered(&isolated(&center), &table);
    let mesh_with = |light: Option<&PaddedLight>, policy| {
        let mut out = ChunkMesh::default();
        Mesher::new().mesh(
            &padded,
            light,
            &table,
            MeshOptions::with_ao(policy),
            &mut out,
        );
        verify_mesh(&padded, &table, &out);
        common::verify_shading(&padded, &table, light, policy, &out);
        out
    };
    let dark = PaddedLight::Uniform(LightValue::DARK);
    // +Y: u = x, v = z. The floor face at x = 9 touches the wall on its +u side: corners 1 and
    // 3 see the wall as a side and as the diagonal (AO 1); corners 0 and 2 are open.
    let beside_wall = ([9, 0, 5], Face::PosY);
    for (light, policy, ao) in [
        (None, AoPolicy::Baked, 0b01_11_01_11),
        (None, AoPolicy::BakedLitOnly, 0b01_11_01_11),
        (Some(&dark), AoPolicy::Baked, 0b01_11_01_11),
        (Some(&dark), AoPolicy::BakedLitOnly, 0xFF),
        (None, AoPolicy::Off, 0xFF),
    ] {
        let mesh = mesh_with(light, policy);
        let quad = quad_at(&mesh, beside_wall.0, beside_wall.1);
        assert_eq!(quad.ao_byte(), ao, "{light:?} {policy:?}");
        // Leaves cast no AO.
        assert_eq!(quad_at(&mesh, [19, 0, 5], Face::PosY).ao_byte(), 0xFF);
        if ao == 0xFF {
            // Nothing splits the floor but the wall itself: one quad on each side of it.
            assert_eq!(quad.w() * quad.h(), 10 * 32, "{light:?} {policy:?}");
        } else {
            // The shaded strip is its own quad, but for its two ends: past the chunk (no
            // neighbors) the wall is not a diagonal, so the end corners get AO 2.
            assert_eq!((quad.pos(), quad.w(), quad.h()), ([9, 0, 1], 1, 30));
            assert_eq!(
                quad_at(&mesh, [9, 0, 0], Face::PosY).ao_byte(),
                0b01_11_10_11
            );
        }
    }
}

#[test]
fn light_reaches_the_palette_and_splits_merges() {
    let table = test_table();
    let center = chunk_repr(
        &dense_from(|_, y, _| if y == 0 { OPAQUE[0] } else { 0 }),
        Repr::Packed,
    );
    let input = isolated(&center);
    let padded = gathered(&input, &table);
    // Sky 15 everywhere but a red light fading from (16, 1, 16).
    let light = common::full_light(|x, y, z| {
        let d = ((x - 16).abs() + (y - 1).abs() + (z - 16).abs()) as u8;
        LightValue::new(15, 14u8.saturating_sub(d), 0, 0).0
    });
    let input = MeshInput {
        light: Some(&light),
        ..input
    };
    let mut mesher = Mesher::new();
    let mesh = mesher.mesh_chunk(&input, &table, OPTIONS);
    verify_mesh(&padded, &table, &mesh);
    common::verify_shading(&padded, &table, Some(&light), AoPolicy::BakedLitOnly, &mesh);
    assert_eq!(mesh.light_quantization, LightQuantization::Exact);
    // The floor top splits where the red changes; the far corners are plain sky.
    let top = mesh.bucket(Face::PosY.index());
    assert!(top.len() > 1, "the light did not split the floor");
    let corner = quad_at(&mesh, [0, 0, 0], Face::PosY);
    assert_eq!(mesh.corner_light(corner), [LightValue::FULL_SKY; 4]);
    let lamp = quad_at(&mesh, [16, 0, 16], Face::PosY);
    assert!(mesh.corner_light(lamp).iter().all(|c| c.red() >= 12));
    // Same input, same mesh: palette order and hash are deterministic.
    let again = Mesher::new().mesh_chunk(&input, &table, OPTIONS);
    assert_eq!(again, mesh);
    assert_eq!(again.hash, mesh.compute_hash());
    // Without light the floor top is one quad again.
    let unlit = mesher.mesh_chunk(&isolated(&center), &table, OPTIONS);
    assert_eq!(unlit.bucket(Face::PosY.index()).len(), 1);
    assert_eq!(unlit.palette, vec![FULL_SKY_ENTRY]);
    assert_ne!(unlit.hash, mesh.hash);
}

/// A 4x4 pool two blocks deep in a stone basin with a stone block over one corner: the top
/// water layer is the surface (marked) except under the stone, where it stays full height; the
/// layer below is plain water, and no face lies between the two. The flat surface cells are
/// greedy quads whose top drops to `FLUID_SURFACE`; the three cells around the full one slope
/// up to it (ADR 0021).
#[test]
fn water_surface_cells_drop_their_top() {
    let (registry, table) = registry();
    let (stone, water) = (block(&registry, "stone"), block(&registry, "water"));
    let center = chunk_repr(
        &dense_from(|x, y, z| match (x, y, z) {
            (1..5, 1..=2, 1..5) => water.0,
            (0..6, 0..=2, 0..6) | (1, 3, 1) => stone.0,
            _ => 0,
        }),
        Repr::Packed,
    );
    let input = isolated(&center);
    let padded = gathered(&input, &table);
    for (x, z) in (1..5).flat_map(|x| (1..5).map(move |z| (x, z))) {
        let marked = |y| padded.get(x, y, z) & FLUID_SURFACE_BIT != 0;
        assert!(!marked(1), "({x}, 1, {z}) has water above");
        assert_eq!(marked(2), (x, z) != (1, 1), "({x}, 2, {z})");
    }
    let mesh = Mesher::new().mesh_chunk(&input, &table, OPTIONS);
    verify_mesh(&padded, &table, &mesh);
    let (mut flat, mut sloped) = (0, Vec::new());
    for quad in mesh.bucket(TRANSLUCENT_BUCKET) {
        for (p, face) in quad.unit_faces() {
            assert_eq!((p[1], face), (2, Face::PosY), "only the tops show");
            match quad.shape() {
                layout::SHAPE_FLUID_SURFACE => flat += 1,
                layout::SHAPE_FLUID_SLOPE => sloped.push((p, quad.fluid_heights().unwrap())),
                other => panic!("{p:?}: shape {other}"),
            }
        }
    }
    assert_eq!(flat, 12, "the open tops away from the full cell");
    sloped.sort();
    assert_eq!(
        sloped,
        [
            ([1, 2, 2], [16, 16, 14, 14]),
            ([2, 2, 1], [16, 14, 16, 14]),
            ([2, 2, 2], [16, 14, 14, 14]),
        ]
    );
}
