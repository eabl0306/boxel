//! Waterlogged blocks (ADR 0025): the block's boxes as for the dry block, and its water as a
//! source cell of the fluid around it, drawn by itself with its faces inset and none of those the
//! block hides (ADR 0038: a cutout block hides none; an opaque or translucent one where its boxes
//! cover the border whole, and the top only inside or on its solid top); and the flow texture on
//! the top of a sloping fluid cell.
mod common;

use boxel_mesh::{
    AoPolicy, BUCKET_COUNT, ChunkMesh, FLUID_FLAT, FLUID_SURFACE_BIT, MaterialTable, MeshInput,
    MeshOptions, Mesher, PackedQuad, PaddedChunk, TRANSLUCENT_BUCKET, VoxelClass, fluid_flow,
    layout, reference_fluid_heights, reference_visible_faces,
};
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockState, BoxModel, CHUNK_SIZE,
    ChunkData, Face, FaceTextures, ModelBox, NEIGHBOR_OFFSETS, Property, RenderShape, SlabHalf,
    WATERLOGGED, testing,
};
use common::{Repr, chunk_repr, dense_from};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

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
fn opaque_blocks_hide_the_water_they_cover() {
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

// ---------------------------------------------------------------- what a block hides (ADR 0038)

/// [`testing::builder`] plus waterloggable blocks (`waterlogged`, opacity 1) for the held water
/// rule: `test:roots` (cutout, one full box, as mangrove roots), `test:cage` (the same box,
/// opaque), `test:glass_box` (the same box, translucent), `test:glass_slab` (translucent, a
/// bottom slab), `test:plate` (opaque, a plate under the top, `[0, 15, 0]-[16, 16, 16]`),
/// `test:thick_plate` (opaque, `[0, 13, 0]-[16, 16, 16]`) and `test:upper` (opaque, a top slab).
/// The test blocks keep their ids.
fn held_registry() -> (BlockRegistry, MaterialTable) {
    let base = testing::registry();
    let texture = |name: &str| base.face_texture(state(&base, name), Face::PosY);
    let (solid, leaves, glass) = (
        texture("test:solid"),
        texture("test:cutout"),
        texture("test:glass"),
    );
    let mut b = testing::builder();
    let full = ModelBox::new([0; 3], [16; 3]);
    let plate = ModelBox::new([0, 15, 0], [16; 3]);
    let thick = ModelBox::new([0, 13, 0], [16; 3]);
    let full_model = b
        .add_box_model(BoxModel::from_boxes(vec![full]))
        .expect("full box");
    let plate_model = b
        .add_box_model(BoxModel::from_boxes(vec![plate]))
        .expect("plate");
    let thick_model = b
        .add_box_model(BoxModel::from_boxes(vec![thick]))
        .expect("thick plate");
    let upper = BlockModel::Slab(SlabHalf::Top);
    let lower = BlockModel::Slab(SlabHalf::Bottom);
    let blocks = [
        (
            "test:roots",
            RenderShape::Cutout,
            BlockModel::Boxes(full_model),
            BlockCollision::Full,
            leaves,
        ),
        (
            "test:cage",
            RenderShape::Opaque,
            BlockModel::Boxes(full_model),
            BlockCollision::Full,
            solid,
        ),
        (
            "test:glass_box",
            RenderShape::Translucent,
            BlockModel::Boxes(full_model),
            BlockCollision::Full,
            glass,
        ),
        (
            "test:glass_slab",
            RenderShape::Translucent,
            lower,
            lower.default_collision(),
            glass,
        ),
        (
            "test:plate",
            RenderShape::Opaque,
            BlockModel::Boxes(plate_model),
            BlockCollision::Boxes(vec![plate.bounds]),
            solid,
        ),
        (
            "test:thick_plate",
            RenderShape::Opaque,
            BlockModel::Boxes(thick_model),
            BlockCollision::Boxes(vec![thick.bounds]),
            solid,
        ),
        (
            "test:upper",
            RenderShape::Opaque,
            upper,
            upper.default_collision(),
            solid,
        ),
    ];
    for (name, shape, model, collision, texture) in blocks {
        let def = BlockDef {
            name: name.to_string(),
            shape,
            model,
            collision,
            emission: [0; 3],
            light_opacity: 1,
            textures: FaceTextures::all(texture),
        };
        b.add_block_with_states(def, vec![Property::boolean(WATERLOGGED)], |_, _| {})
            .expect(name);
    }
    let registry = b.build();
    for name in ["test:solid", "test:fluid", "test:slab", "test:boxes"] {
        assert_eq!(state(&registry, name), state(&base, name), "{name}");
    }
    let table = MaterialTable::from_registry(&registry);
    (registry, table)
}

/// The faces of the water quads at `p` (all inset, at `heights`).
fn water_at(mesh: &ChunkMesh, p: [u32; 3], heights: [u8; 4]) -> BTreeSet<Face> {
    let quads = quads_at(mesh, p, layout::SHAPE_FLUID_SLOPE);
    for quad in quads.values() {
        assert!(quad.fluid_inset(), "{quad:?}");
        assert_eq!(quad.fluid_heights(), Some(heights), "{quad:?}");
    }
    quads.keys().copied().collect()
}

/// How many box faces the mesh draws at `p`.
fn box_faces(mesh: &ChunkMesh, p: [u32; 3]) -> usize {
    mesh.quads
        .iter()
        .filter(|q| q.pos() == p && q.shape() == layout::SHAPE_BOX)
        .count()
}

#[test]
fn a_cutout_block_never_hides_its_water() {
    use Face::*;
    let (registry, table) = held_registry();
    let roots = state(&registry, "test:roots[waterlogged=true]");
    let fluid = state(&registry, "test:fluid");
    let solid = state(&registry, "test:solid");
    // Alone in the air: all 6 faces of its water, at 12/16 (air around), and its 6 box faces.
    let (_, mesh) = mesh_of(&table, |x, y, z| ((x, y, z) == (5, 5, 5)).then_some(roots));
    assert_eq!(
        water_at(&mesh, [5, 5, 5], [12; 4]),
        BTreeSet::from(Face::ALL)
    );
    assert_eq!(box_faces(&mesh, [5, 5, 5]), 6);
    // In the middle of a 3x3 pool on a floor: only its top, level with the pool (no water side
    // toward the pool, no bottom on the floor). Its boxes as for dry roots: the bottom on the
    // floor is hidden.
    let (_, mesh) = mesh_of(&table, |x, y, z| match (x, y, z) {
        (5, 5, 5) => Some(roots),
        (4..=6, 5, 4..=6) => Some(fluid),
        (4..=6, 4, 4..=6) => Some(solid),
        _ => None,
    });
    let flat = [FLUID_FLAT; 4];
    assert_eq!(water_at(&mesh, [5, 5, 5], flat), BTreeSet::from([PosY]));
    assert_eq!(box_faces(&mesh, [5, 5, 5]), 5);
}

#[test]
fn the_top_hides_only_inside_the_solid_top() {
    use Face::*;
    let (registry, table) = held_registry();
    let wet = |name: &str| state(&registry, &format!("{name}[waterlogged=true]"));
    let cell = PaddedChunk::index(5, 5, 5) as u32;
    let alone = |s: BlockState| {
        let (padded, mesh) = mesh_of(&table, |x, y, z| ((x, y, z) == (5, 5, 5)).then_some(s));
        // The cell holds water, whatever of it shows.
        assert_eq!(padded.waterlogged, vec![(cell, s.0)]);
        mesh
    };
    let sides = [PosX, NegX, PosZ, NegZ];
    // A top slab: its solid top starts at 8, under the corners (12/16): no top. Its sides are
    // half covered and its bottom open.
    let mesh = alone(wet("test:upper"));
    let want = BTreeSet::from_iter(sides.into_iter().chain([NegY]));
    assert_eq!(water_at(&mesh, [5, 5, 5], [12; 4]), want);
    // A plate from 15: the corners (12/16) lie below it, every face shows.
    let mesh = alone(wet("test:plate"));
    assert_eq!(
        water_at(&mesh, [5, 5, 5], [12; 4]),
        BTreeSet::from(Face::ALL)
    );
    // An opaque full box hides all of its water.
    let mesh = alone(wet("test:cage"));
    assert_eq!(water_at(&mesh, [5, 5, 5], [0; 4]), BTreeSet::new());
    assert_eq!(box_faces(&mesh, [5, 5, 5]), 6);
    // The plate with full water on its 4 sides (water above them): its corners are at 16/16, on
    // the plate's top. No top, no side (water beside), the bottom over the air.
    let fluid = state(&registry, "test:fluid");
    let plate = wet("test:plate");
    let (_, mesh) = mesh_of(&table, |x, y, z| match (x, y, z) {
        (5, 5, 5) => Some(plate),
        (4 | 6, 5 | 6, 5) | (5, 5 | 6, 4 | 6) => Some(fluid),
        _ => None,
    });
    assert_eq!(water_at(&mesh, [5, 5, 5], [16; 4]), BTreeSet::from([NegY]));
    // A thick plate from 13, a solid cube on both of its x sides: every corner at 13/16 (air
    // on the z side), on the plate's floor. Its top is hidden (a corner at the floor counts as
    // inside), its z sides and bottom show.
    let solid = state(&registry, "test:solid");
    let thick = wet("test:thick_plate");
    let (_, mesh) = mesh_of(&table, |x, y, z| match (x, y, z) {
        (5, 5, 5) => Some(thick),
        (4 | 6, 5, 5) => Some(solid),
        _ => None,
    });
    let want = BTreeSet::from([PosZ, NegZ, NegY]);
    assert_eq!(water_at(&mesh, [5, 5, 5], [13; 4]), want);
    // A solid cube on +z too: the corners there at 14/16, the others at 13/16.
    let (_, mesh) = mesh_of(&table, |x, y, z| match (x, y, z) {
        (5, 5, 5) => Some(thick),
        (4 | 6, 5, 5) | (5, 5, 6) => Some(solid),
        _ => None,
    });
    let want = BTreeSet::from([NegZ, NegY]);
    assert_eq!(water_at(&mesh, [5, 5, 5], [13, 13, 14, 14]), want);
}

#[test]
fn a_translucent_block_hides_its_water_like_an_opaque_one() {
    use Face::*;
    let (registry, table) = held_registry();
    let wet = |name: &str| state(&registry, &format!("{name}[waterlogged=true]"));
    // Alone in the air. A full glass box covers all of its water: none shows, though the cell
    // holds it, and its 6 box faces show.
    let cell = PaddedChunk::index(5, 5, 5) as u32;
    let glass = wet("test:glass_box");
    let (padded, mesh) = mesh_of(&table, |x, y, z| ((x, y, z) == (5, 5, 5)).then_some(glass));
    assert_eq!(padded.waterlogged, vec![(cell, glass.0)]);
    assert_eq!(water_at(&mesh, [5, 5, 5], [12; 4]), BTreeSet::new());
    assert_eq!(box_faces(&mesh, [5, 5, 5]), 6);
    // A glass slab covers its bottom whole: the 4 sides and the top show.
    let slab = wet("test:glass_slab");
    let (padded, mesh) = mesh_of(&table, |x, y, z| ((x, y, z) == (5, 5, 5)).then_some(slab));
    assert_eq!(padded.waterlogged, vec![(cell, slab.0)]);
    let want = BTreeSet::from([PosY, PosX, NegX, PosZ, NegZ]);
    assert_eq!(water_at(&mesh, [5, 5, 5], [12; 4]), want);
    // Its boxes are drawn translucent.
    let translucent = mesh.bucket(TRANSLUCENT_BUCKET).iter();
    let boxes = translucent.filter(|q| q.pos() == [5, 5, 5] && q.shape() == layout::SHAPE_BOX);
    assert_eq!(boxes.count(), box_faces(&mesh, [5, 5, 5]));
}

/// The blocks of the random scenes of [`held_water_faces_match_minecraft`], water first: more
/// often water than anything else.
const MIX: [&str; 22] = [
    "test:fluid",
    "test:fluid",
    "test:fluid",
    "test:fluid[level=2]",
    "test:fluid[level=5]",
    "test:fluid[level=9]",
    "test:solid",
    "test:roots",
    "test:roots[waterlogged=true]",
    "test:plate",
    "test:plate[waterlogged=true]",
    "test:upper",
    "test:upper[waterlogged=true]",
    "test:slab",
    "test:slab[waterlogged=true]",
    "test:boxes",
    "test:boxes[waterlogged=true]",
    "test:cage[waterlogged=true]",
    "test:glass_box",
    "test:glass_box[waterlogged=true]",
    "test:glass_slab[waterlogged=true]",
    "test:thick_plate[waterlogged=true]",
];

/// The faces of the water of waterlogged `s` that the block hides, its water's corners at
/// `heights`, from the rule written out in floating point (Minecraft's `LiquidBlockRenderer`,
/// ADR 0038): none for a cutout block (Minecraft's `canOcclude`); an opaque or translucent one
/// (the translucent sort could not order its box faces against water faces on their plane)
/// hides a side or the bottom where its boxes cover the whole border face, and the top where
/// every point from the lowest corner up to the top of the cell is inside a box and the top
/// border is covered whole.
fn hidden_by_block(registry: &BlockRegistry, s: BlockState, heights: [u8; 4]) -> BTreeSet<Face> {
    if !matches!(
        registry.shape(s),
        RenderShape::Opaque | RenderShape::Translucent
    ) {
        return BTreeSet::new();
    }
    let boxes: Vec<([f64; 3], [f64; 3])> = registry
        .model_boxes(s)
        .iter()
        .map(|b| {
            let f = |c: [u8; 3]| c.map(|v| f64::from(v) / 16.0);
            (f(b.bounds.min), f(b.bounds.max))
        })
        .collect();
    // The centers of the sixteenths of a block edge.
    let centers = || (0..16).map(|i| (f64::from(i) + 0.5) / 16.0);
    let covered = |face: Face| {
        let (n, u, v) = face.axes();
        let normal = face.normal();
        let positive = [normal.x, normal.y, normal.z][n] > 0;
        let border = if positive { 1.0 } else { 0.0 };
        centers().all(|a| {
            centers().all(|b| {
                boxes.iter().any(|(min, max)| {
                    let plane = if positive { max[n] } else { min[n] };
                    plane == border && min[u] < a && a < max[u] && min[v] < b && b < max[v]
                })
            })
        })
    };
    let low = f64::from(*heights.iter().min().expect("4 corners")) / 16.0;
    let solid_above = centers().filter(|&y| y > low).all(|y| {
        centers().all(|x| {
            centers().all(|z| {
                boxes.iter().any(|(min, max)| {
                    (0..3).all(|k| min[k] < [x, y, z][k] && [x, y, z][k] < max[k])
                })
            })
        })
    });
    Face::ALL
        .into_iter()
        .filter(|&face| match face {
            Face::PosY => covered(face) && solid_above,
            _ => covered(face),
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Random scenes of water, solid cubes and waterlogged and dry blocks (cutout, translucent
    /// and opaque, with and without a solid top), in a corner of the chunk, its middle or across
    /// its borders: every waterlogged block's water draws the faces the greedy cull leaves
    /// (`reference_visible_faces`) but those the block hides ([`hidden_by_block`]), inset, at
    /// the oracle's corner heights (16/16 under water or an opaque cube).
    #[test]
    fn held_water_faces_match_minecraft(
        seed in any::<u64>(),
        corner in 0usize..3,
        fill in 20u64..=100,
    ) {
        let (registry, table) = held_registry();
        let mix: Vec<BlockState> = MIX.iter().map(|name| state(&registry, name)).collect();
        // A 6^3 window of random blocks (padded coordinates), the rest air.
        let origin = [-1, 12, 27][corner];
        let window = origin..origin + 6;
        let mut rng = common::Rng::new(seed);
        let mut cells = BTreeMap::new();
        for x in window.clone() {
            for y in window.clone() {
                for z in window.clone() {
                    if rng.below(100) < fill {
                        cells.insert([x, y, z], rng.pick(&mix));
                    }
                }
            }
        }
        // The chunk at offset `d` from the center, if the window reaches into it.
        let chunk_of = |d: [i32; 3]| -> Option<ChunkData> {
            let lo = d.map(|c| c * CHUNK_SIZE);
            let touches = lo.iter().all(|&l| window.start < l + CHUNK_SIZE && l < window.end);
            touches.then(|| {
                let at =
                    |x, y, z| cells.get(&[lo[0] + x, lo[1] + y, lo[2] + z]).map_or(0, |s| s.0);
                chunk_repr(&dense_from(at), Repr::Packed)
            })
        };
        let center = chunk_of([0; 3]).expect("every window reaches into the center");
        let around: Vec<Option<ChunkData>> =
            NEIGHBOR_OFFSETS.iter().map(|d| chunk_of([d.x, d.y, d.z])).collect();
        let input = MeshInput {
            center: &center,
            neighbors: std::array::from_fn(|i| around[i].as_ref()),
            light: None,
        };
        let mut padded = PaddedChunk::new();
        let mut mesher = Mesher::new();
        mesher.gather(&input, &table, &mut padded);
        let mesh = mesher.mesh_chunk(&input, &table, OPTIONS);
        let visible = reference_visible_faces(&padded);
        let heights = reference_fluid_heights(&padded, &table);
        let mut wet = BTreeSet::new();
        for (&[x, y, z], &s) in &cells {
            let inner = [x, y, z].iter().all(|c| (0..CHUNK_SIZE).contains(c));
            if !inner || !registry.is_waterlogged(s) {
                continue;
            }
            let p = [x as u32, y as u32, z as u32];
            wet.insert(p);
            let m = registry.material(s);
            prop_assert_eq!(padded.waterlogged_at(PaddedChunk::index(x, y, z)), Some(m));
            let corners = heights.get(&p).copied().unwrap_or([16; 4]);
            let hidden = hidden_by_block(&registry, s, corners);
            let want: BTreeSet<Face> = Face::ALL
                .into_iter()
                .filter(|&f| visible.contains(&(p, f)) && !hidden.contains(&f))
                .collect();
            let got = water_at(&mesh, p, corners);
            let name = registry.state_name(s);
            prop_assert_eq!(&got, &want, "{} at {:?}, corners {:?}", name, p, corners);
        }
        // The greedy pass draws no face of a waterlogged cell.
        for quad in &mesh.quads {
            if quad.shape() != layout::SHAPE_FLUID_SLOPE && quad.shape() != layout::SHAPE_BOX {
                prop_assert!(quad.unit_faces().all(|(p, _)| !wet.contains(&p)), "{:?}", quad);
            }
        }
    }
}
