//! Waterlogged blocks (ADR 0025): the block's boxes as for the dry block, and its water as a
//! source cell of the fluid around it, drawn by itself with its faces inset and none against
//! the block's sturdy faces; and the flow texture on the top of a sloping fluid cell.
mod common;

use boxel_mesh::{
    AoPolicy, BUCKET_COUNT, ChunkMesh, FLUID_FLAT, FLUID_SURFACE_BIT, MaterialTable, MeshInput,
    MeshOptions, Mesher, PackedQuad, PaddedChunk, TRANSLUCENT_BUCKET, VoxelClass, fluid_flow,
    layout,
};
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, Face};
use common::{Repr, chunk_repr, dense_from};
use std::collections::BTreeMap;

const OPTIONS: MeshOptions = MeshOptions::with_ao(AoPolicy::BakedLitOnly);

fn registry() -> (BlockRegistry, MaterialTable) {
    let registry = boxel_demo::blocks::registry();
    let table = MaterialTable::from_registry(&registry);
    (registry, table)
}

fn state(registry: &BlockRegistry, name: &str) -> BlockState {
    registry.parse_state(name).expect("built-in state")
}

/// The chunk of `f` (local cells, `None` air), meshed alone.
fn mesh_of(
    table: &MaterialTable,
    f: impl Fn(i32, i32, i32) -> Option<BlockState>,
) -> (PaddedChunk, ChunkMesh) {
    let chunk: ChunkData = chunk_repr(
        &dense_from(|x, y, z| f(x, y, z).map_or(0, |s| s.0)),
        Repr::Packed,
    );
    let input = MeshInput {
        center: &chunk,
        neighbors: [None; 26],
        light: None,
    };
    let mut padded = PaddedChunk::new();
    let mut mesher = Mesher::new();
    mesher.gather(&input, table, &mut padded);
    let mesh = mesher.mesh_chunk(&input, table, OPTIONS);
    (padded, mesh)
}

/// Every quad of the mesh at cell `p` with `shape`, by face (each face once).
fn quads_at(mesh: &ChunkMesh, p: [u32; 3], shape: u8) -> BTreeMap<Face, PackedQuad> {
    let mut out = BTreeMap::new();
    for bucket in 0..BUCKET_COUNT {
        for quad in mesh.bucket(bucket) {
            if quad.pos() == p && quad.shape() == shape {
                assert!(
                    out.insert(quad.face(), *quad).is_none() || shape == layout::SHAPE_BOX,
                    "{quad:?} twice"
                );
            }
        }
    }
    out
}

#[test]
fn the_table_meshes_a_waterlogged_block_as_its_water() {
    let (registry, table) = registry();
    let water = state(&registry, "boxel:water").0;
    for name in [
        "boxel:stone_slab[waterlogged=true]",
        "boxel:oak_stairs[waterlogged=true]",
        "boxel:oak_fence[waterlogged=true]",
    ] {
        let wet = state(&registry, name).0;
        let dry = registry.with_water(BlockState(wet), false).expect("dry").0;
        assert!(
            table.is_waterlogged(wet) && !table.is_waterlogged(dry),
            "{name}"
        );
        assert_eq!(table.class(wet), VoxelClass::Transparent, "{name}");
        assert_eq!(table.class(dry), VoxelClass::Special, "{name}");
        assert_eq!(table.fluid_level(wet), (water, 0), "{name}");
        assert_eq!(table.fluid_level(dry), (dry, 0), "{name}");
        assert_eq!(table.model(wet), table.model(dry), "{name}");
    }
    // A double slab holds no water.
    let double = state(&registry, "boxel:stone_slab[type=double,waterlogged=true]").0;
    assert!(!table.is_waterlogged(double));
    assert_eq!(table.class(double), VoxelClass::Opaque);
    // Fluids have the fluid cube code (their side faces scroll the flow texture).
    for face in Face::ALL {
        assert_eq!(table.cube_code(water, face), layout::SHAPE_FLUID_CUBE);
    }
}

#[test]
fn a_waterlogged_slab_in_a_lake_is_part_of_the_lake() {
    let (registry, table) = registry();
    let stone = state(&registry, "boxel:stone");
    let source = state(&registry, "boxel:water");
    let wet = state(&registry, "boxel:stone_slab[waterlogged=true]");
    let still = table.layer(source.0, Face::PosY);
    let (padded, mesh) = mesh_of(&table, |x, y, z| match (x, y, z) {
        (10, 4, 10) => Some(wet),
        (1..30, 1..=4, 1..30) => Some(source),
        (0..31, 0..=4, 0..31) => Some(stone),
        _ => None,
    });
    let i = PaddedChunk::index(10, 4, 10);
    assert_eq!(padded.waterlogged, vec![(i as u32, wet.0)]);
    assert_eq!(padded.voxels[i], source.0 | FLUID_SURFACE_BIT);
    assert_eq!(padded.waterlogged_at(i), Some(wet.0));
    // Its water: the top only (the lake hides the rest), level with the lake, inset, the still
    // texture.
    let water = quads_at(&mesh, [10, 4, 10], layout::SHAPE_FLUID_SLOPE);
    assert_eq!(water.keys().copied().collect::<Vec<_>>(), [Face::PosY]);
    let top = water[&Face::PosY];
    assert_eq!(top.fluid_heights(), Some([FLUID_FLAT; 4]));
    assert!(top.fluid_inset());
    assert_eq!(top.layer(), still);
    // The lake's top covers every other cell once, and its cells draw no face against the slab.
    let mut covered = BTreeMap::new();
    for quad in mesh.bucket(TRANSLUCENT_BUCKET) {
        for (p, face) in quad.unit_faces() {
            assert!(covered.insert((p, face), *quad).is_none(), "{p:?} {face:?}");
            if quad.shape() == layout::SHAPE_FLUID_SURFACE {
                assert_eq!(face, Face::PosY, "{quad:?}");
            }
        }
    }
    for x in 1..30 {
        for z in 1..30 {
            assert!(covered.contains_key(&([x, 4, z], Face::PosY)), "{x} {z}");
        }
    }
    assert_eq!(covered.len(), 29 * 29);
    // The slab: its top (inside the cell) and its 4 sides and bottom against water.
    let boxes = mesh
        .quads
        .iter()
        .filter(|q| q.pos() == [10, 4, 10] && q.shape() == layout::SHAPE_BOX)
        .count();
    assert_eq!(boxes, 6);
}

#[test]
fn sturdy_faces_hide_the_water_and_the_rest_is_inset() {
    let (registry, table) = registry();
    let slab = state(&registry, "boxel:stone_slab[waterlogged=true]");
    let stairs = state(&registry, "boxel:oak_stairs[facing=east,waterlogged=true]");
    let fence = state(&registry, "boxel:oak_fence[waterlogged=true]");
    let dry_slab = state(&registry, "boxel:stone_slab");
    let (padded, mesh) = mesh_of(&table, |x, y, z| match (x, y, z) {
        (5, 10, 5) => Some(slab),
        (6, 10, 5) => Some(dry_slab),
        (12, 10, 5) => Some(stairs),
        (20, 10, 5) => Some(fence),
        _ => None,
    });
    assert_eq!(padded.waterlogged.len(), 3);
    let faces = |p, heights| {
        let quads = quads_at(&mesh, p, layout::SHAPE_FLUID_SLOPE);
        for quad in quads.values() {
            assert!(quad.fluid_inset(), "{quad:?}");
            assert_eq!(quad.fluid_heights(), Some(heights), "{quad:?}");
        }
        quads.keys().copied().collect::<Vec<_>>()
    };
    use Face::*;
    // The slab's bottom is sturdy; the dry slab beside it is no water: its face shows. In the
    // air the corners drop to 12/16, but on the side of the dry slab, which does not count.
    assert_eq!(
        faces([5, 10, 5], [12, 13, 12, 13]),
        [PosY, PosX, NegX, PosZ, NegZ]
    );
    // Stairs facing east: their back (east) and bottom are sturdy.
    assert_eq!(faces([12, 10, 5], [12; 4]), [PosY, NegX, PosZ, NegZ]);
    // A fence covers no face whole.
    assert_eq!(faces([20, 10, 5], [12; 4]), Face::ALL);
    // Between the wet and the dry slab, the two box faces hide each other, as between dry
    // ones.
    let boxes = |p| {
        mesh.quads
            .iter()
            .filter(|q| q.pos() == p && q.shape() == layout::SHAPE_BOX)
            .map(|q| q.face())
            .collect::<Vec<_>>()
    };
    assert!(!boxes([5, 10, 5]).contains(&PosX));
    assert!(!boxes([6, 10, 5]).contains(&NegX));
    assert_eq!(boxes([5, 10, 5]).len(), 5);
    assert_eq!(boxes([6, 10, 5]).len(), 5);
}

#[test]
fn a_sloping_top_shows_the_flow_texture() {
    let (registry, table) = registry();
    let stone = state(&registry, "boxel:stone");
    let source = state(&registry, "boxel:water");
    let flowing = |l: u8| state(&registry, &format!("boxel:water[level={l}]"));
    let water = source.0;
    let (still, flow) = (
        table.layer(water, Face::PosY),
        table.layer(water, Face::PosX),
    );
    assert_ne!(still, flow);
    // A source and a run of flowing water east of it, on a stone floor.
    let (_, mesh) = mesh_of(&table, |x, y, z| match (x, y, z) {
        (_, 0, _) => Some(stone),
        (4, 1, 4) => Some(source),
        (5..=10, 1, 4) => Some(flowing((x - 4) as u8)),
        _ => None,
    });
    let mut tops = 0;
    for quad in mesh.bucket(TRANSLUCENT_BUCKET) {
        let Some(heights) = quad.fluid_heights() else {
            continue;
        };
        let flow_dir = fluid_flow(heights);
        match quad.face() {
            Face::PosY => {
                tops += 1;
                let want = if flow_dir == [0, 0] { still } else { flow };
                assert_eq!(quad.layer(), want, "{quad:?} {heights:?}");
                // The run slopes down to the east.
                if (5..=9).contains(&quad.pos()[0]) {
                    assert!(flow_dir[0] > 0, "{quad:?} {heights:?}");
                }
            }
            Face::NegY => assert_eq!(quad.layer(), table.layer(water, Face::NegY)),
            _ => assert_eq!(quad.layer(), flow, "{quad:?}"),
        }
    }
    assert_eq!(tops, 7);
    assert_eq!(fluid_flow([16, 14, 16, 14]), [4, 0]);
    assert_eq!(fluid_flow([14, 14, 10, 10]), [0, 8]);
    assert_eq!(fluid_flow([12; 4]), [0, 0]);
}
