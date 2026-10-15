//! Waterlogged blocks (ADR 0025): the block's boxes as for the dry block, and its water as a
//! source cell of the fluid around it, drawn by itself with its faces inset and none of those the
//! block hides (ADR 0038: a cutout block hides none; an opaque or translucent one where its boxes
//! cover the border whole, and the top only inside or on its solid top); cutout cubes that hold
//! water (ADR 0038), drawn as the dry cube one face per block; and the flow texture on the top of
//! a sloping fluid cell.
mod common;

use boxel_light::PaddedLight;
use boxel_mesh::{
    AoPolicy, BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, FLUID_FLAT, FLUID_SURFACE_BIT, MaterialTable,
    MeshInput, MeshOptions, Mesher, PackedQuad, PaddedChunk, TRANSLUCENT_BUCKET, VoxelClass,
    fluid_flow, layout, reference_fluid_heights, reference_visible_faces,
};
use boxel_voxel::{
    Axis, BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockState, BoxModel, CHUNK_SIZE,
    ChunkData, Face, FaceTextures, LightValue, ModelBox, NEIGHBOR_OFFSETS, Property, RenderShape,
    SlabHalf, WATERLOGGED, testing,
};
use common::{Repr, chunk_repr, dense_from, full_light};
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
/// `test:thick_plate` (opaque, `[0, 13, 0]-[16, 16, 16]`) and `test:upper` (opaque, a top slab);
/// and full cubes with `waterlogged` (ADR 0038): `test:wet_cube` (cutout, as leaves),
/// `test:wet_log` (the same turned to X, its ends solid), `test:wet_glass` (translucent) and
/// `test:wet_solid` (opaque, opacity 15), of which only the cutout ones hold water. The test
/// blocks keep their ids.
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
    let log_faces = FaceTextures {
        top: solid,
        bottom: solid,
        side: leaves,
    };
    let x_log = BlockModel::Cube { axis: Axis::X };
    let all = FaceTextures::all;
    let blocks = blocks.map(|(name, shape, model, collision, texture)| {
        (name, shape, model, collision, all(texture), 1)
    });
    let (cutout, cube) = (RenderShape::Cutout, BlockModel::CUBE);
    let cubes = [
        ("test:wet_cube", cutout, cube, all(leaves), 1),
        ("test:wet_log", cutout, x_log, log_faces, 1),
        (
            "test:wet_glass",
            RenderShape::Translucent,
            cube,
            all(glass),
            0,
        ),
        ("test:wet_solid", RenderShape::Opaque, cube, all(solid), 15),
    ]
    .map(|(name, shape, model, textures, opacity)| {
        (name, shape, model, BlockCollision::Full, textures, opacity)
    });
    let defs = blocks.into_iter().chain(cubes);
    for (name, shape, model, collision, textures, light_opacity) in defs {
        let def = BlockDef {
            name: name.to_string(),
            shape,
            model,
            collision,
            emission: [0; 3],
            light_opacity,
            textures,
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
const MIX: [&str; 26] = [
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
    "test:wet_cube",
    "test:wet_cube[waterlogged=true]",
    "test:wet_cube[waterlogged=true]",
    "test:wet_log[waterlogged=true]",
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

/// The faces of the waterlogged cube `s` the mesher draws, its neighbors being `around` (by
/// face, air outside the scene), from the rule written out (ADR 0038): every face but those
/// toward an opaque cube or toward the same block without its water, dry or waterlogged, as the
/// greedy pass culls the dry cube.
fn wet_cube_faces(
    registry: &BlockRegistry,
    s: BlockState,
    around: [BlockState; 6],
) -> BTreeSet<Face> {
    let dry = |s: BlockState| registry.with_water(s, false).unwrap_or(s);
    Face::ALL
        .into_iter()
        .filter(|f| {
            let n = around[f.index()];
            !registry.occludes(n) && dry(n) != dry(s)
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Random scenes of water, solid cubes and waterlogged and dry blocks (cutout, translucent
    /// and opaque, with and without a solid top, boxes and cubes), in a corner of the chunk, its
    /// middle or across its borders: every waterlogged block's water draws the faces the greedy
    /// cull leaves (`reference_visible_faces`) but those the block hides ([`hidden_by_block`]),
    /// inset, at the oracle's corner heights (16/16 under water or an opaque cube); every
    /// waterlogged cube draws one unit quad per face [`wet_cube_faces`] keeps, and the greedy
    /// pass none of a waterlogged cell.
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
        // The cube faces at a waterlogged cell are the wet cube's own unit quads.
        let mut drawn: BTreeMap<[u32; 3], BTreeSet<Face>> = BTreeMap::new();
        for quad in &mesh.quads {
            if quad.shape() != layout::SHAPE_FLUID_SLOPE && quad.shape() != layout::SHAPE_BOX {
                for (p, face) in quad.unit_faces().filter(|(p, _)| wet.contains(p)) {
                    prop_assert!(quad.w() == 1 && quad.h() == 1, "{:?}", quad);
                    prop_assert!(drawn.entry(p).or_default().insert(face), "{:?}", quad);
                }
            }
        }
        let at = |p: [i32; 3]| cells.get(&p).copied().unwrap_or(BlockState::AIR);
        for &p in &wet {
            let [x, y, z] = p.map(|c| c as i32);
            let s = at([x, y, z]);
            let want = if registry.model(s).is_solid_cube() {
                let around = Face::ALL.map(|f| {
                    let n = f.normal();
                    at([x + n.x, y + n.y, z + n.z])
                });
                wet_cube_faces(&registry, s, around)
            } else {
                BTreeSet::new()
            };
            let got = drawn.remove(&p).unwrap_or_default();
            prop_assert_eq!(&got, &want, "{} at {:?}", registry.state_name(s), p);
        }
    }
}

// ---------------------------------------------------------------- cutout cubes that hold water (ADR 0038)

/// The chunk `chunk` (in chunks from the center) of the world `f` (block coordinates from the
/// center chunk's origin, `None` air), meshed with its 26 neighbors and `light`: its padded
/// input and its mesh.
fn mesh_world(
    table: &MaterialTable,
    f: &dyn Fn([i32; 3]) -> Option<BlockState>,
    chunk: [i32; 3],
    light: Option<&PaddedLight>,
    options: MeshOptions,
) -> (PaddedChunk, ChunkMesh) {
    let data = |d: [i32; 3]| -> ChunkData {
        let lo: [i32; 3] = std::array::from_fn(|k| (chunk[k] + d[k]) * CHUNK_SIZE);
        let at = |x, y, z| f([lo[0] + x, lo[1] + y, lo[2] + z]).map_or(0, |s| s.0);
        chunk_repr(&dense_from(at), Repr::Packed)
    };
    let center = data([0; 3]);
    let around: Vec<ChunkData> = NEIGHBOR_OFFSETS
        .iter()
        .map(|d| data([d.x, d.y, d.z]))
        .collect();
    let input = MeshInput {
        center: &center,
        neighbors: std::array::from_fn(|i| Some(&around[i])),
        light,
    };
    let mut padded = PaddedChunk::new();
    let mut mesher = Mesher::new();
    mesher.gather(&input, table, &mut padded);
    let mesh = mesher.mesh_chunk(&input, table, options);
    (padded, mesh)
}

/// A unit face as it is drawn: position, face, texture layer, shape code, AO byte, the 4 corner
/// lights and the emissive flag.
type DrawnFace = ([u32; 3], Face, u16, u8, u8, [u16; 4], bool);

/// Every unit face of the cutout bucket of `mesh`, each once.
fn cutout_faces(mesh: &ChunkMesh) -> BTreeSet<DrawnFace> {
    let mut out = BTreeSet::new();
    for &quad in mesh.bucket(CUTOUT_BUCKET) {
        let light = mesh.corner_light(quad).map(|l| l.0);
        for (p, face) in quad.unit_faces() {
            let drawn = (
                p,
                face,
                quad.layer(),
                quad.shape(),
                quad.ao_byte(),
                light,
                quad.is_emissive(),
            );
            assert!(out.insert(drawn), "{p:?} {face:?} twice");
        }
    }
    out
}

/// The cube faces (`SHAPE_CUBE` or `SHAPE_TURNED`) the mesh draws at `p`, by face.
fn cube_quads(mesh: &ChunkMesh, p: [u32; 3]) -> BTreeMap<Face, PackedQuad> {
    let mut out = quads_at(mesh, p, layout::SHAPE_CUBE);
    out.extend(quads_at(mesh, p, layout::SHAPE_TURNED));
    out
}

/// The materials of the cutout cubes of [`held_registry`]: they hold water, the others do not.
#[test]
fn the_table_meshes_a_wet_cutout_cube_as_its_water() {
    let (registry, table) = held_registry();
    let water = state(&registry, "test:fluid").0;
    for name in ["test:wet_cube", "test:wet_log"] {
        let wet = state(&registry, &format!("{name}[waterlogged=true]")).0;
        let dry = state(&registry, &format!("{name}[waterlogged=false]")).0;
        assert!(
            table.is_waterlogged(wet) && !table.is_waterlogged(dry),
            "{name}"
        );
        assert_eq!(table.class(wet), VoxelClass::Transparent, "{name}");
        assert_eq!(table.class(dry), VoxelClass::Transparent, "{name}");
        assert_eq!(table.fluid_level(wet), (water, 0), "{name}");
        assert_eq!(table.model(wet), table.model(dry), "{name}");
        for face in Face::ALL {
            assert_eq!(table.layer(wet, face), table.layer(dry, face), "{name}");
            assert_eq!(
                table.cube_code(wet, face),
                table.cube_code(dry, face),
                "{name}"
            );
            assert_eq!(table.bucket(wet, face), CUTOUT_BUCKET, "{name}");
        }
    }
    for (name, class) in [
        ("test:wet_glass", VoxelClass::Transparent),
        ("test:wet_solid", VoxelClass::Opaque),
    ] {
        let s = state(&registry, &format!("{name}[waterlogged=true]")).0;
        assert!(!table.is_waterlogged(s), "{name}");
        assert_eq!(table.class(s), class, "{name}");
        assert_eq!(table.fluid_level(s), (s, 0), "{name}");
    }
}

/// A waterlogged cutout cube is drawn exactly as the dry one: in every scene (lone cubes, a
/// turned log, a cluster on a floor beside other leaves, a cluster across a chunk border), with
/// no light and with a light that falls off, for every AO policy, the unit faces of the cutout
/// bucket are the same, one per block for the wet cubes where the greedy pass merges the dry
/// ones. A lone cube's quads are the same bytes. Their water is drawn apart (the translucent
/// bucket), at the cost of a canopy: the 16x16 canopy over a pool below.
#[test]
fn the_wet_cube_looks_like_the_dry_one() {
    let (registry, table) = held_registry();
    let s = |name: &str| state(&registry, name);
    let (solid, cutout, fluid) = (s("test:solid"), s("test:cutout"), s("test:fluid"));
    let cube = [false, true].map(|wet| s(&format!("test:wet_cube[waterlogged={wet}]")));
    let log = [false, true].map(|wet| s(&format!("test:wet_log[waterlogged={wet}]")));
    // A lone cube and a lone log; a 3x3x3 cluster on a floor, other leaves beside it; a row of
    // turned logs; a cluster across the border x = 31 / 0, under a solid.
    let world = |wet: usize| {
        move |[x, y, z]: [i32; 3]| match (x, y, z) {
            (5, 5, 5) | (12..=14, 10..=12, 12..=14) | (30..=33, 20..=21, 5..=6) => Some(cube[wet]),
            (9, 5, 5) | (5..=8, 20, 20) => Some(log[wet]),
            (12..=14, 9, 12..=14) | (30..=33, 22, 5) => Some(solid),
            (15, 10..=11, 13) => Some(cutout),
            _ => None,
        }
    };
    // Sky falling off around a point, and a red light around another.
    let light_of = |chunk: [i32; 3]| {
        full_light(move |x, y, z| {
            let [x, y, z] = [x + 32 * chunk[0], y + 32 * chunk[1], z + 32 * chunk[2]];
            let sky = 15 - ((x - 16).abs() + (z - 8).abs() + (y - 20).abs()).min(15);
            let red = 14 - ((x - 13).abs() + (y - 11).abs() + (z - 15).abs()).min(14);
            LightValue::new(sky as u8, red as u8, 0, 0).0
        })
    };
    for chunk in [[0; 3], [1, 0, 0]] {
        let light = light_of(chunk);
        for light in [None, Some(&light)] {
            for ao in [AoPolicy::Off, AoPolicy::Baked, AoPolicy::BakedLitOnly] {
                let options = MeshOptions::with_ao(ao);
                let what = format!("chunk {chunk:?}, light {}, {ao:?}", light.is_some());
                let (_, dry) = mesh_world(&table, &world(0), chunk, light, options);
                let (_, wet) = mesh_world(&table, &world(1), chunk, light, options);
                assert_eq!(cutout_faces(&wet), cutout_faces(&dry), "{what}");
                assert!(!cutout_faces(&dry).is_empty(), "{what}");
                if chunk == [0; 3] && light.is_none() {
                    for p in [[5, 5, 5], [9, 5, 5]] {
                        let quads = cube_quads(&dry, p);
                        assert_eq!(quads.len(), 6, "{what}");
                        assert_eq!(cube_quads(&wet, p), quads, "{what}");
                    }
                }
            }
        }
    }
    // A 16x16 canopy of cubes over a pool: dry, the greedy pass merges its 6 sides; wet, every
    // cube draws its own faces and its water (top, bottom and the rim's sides).
    let canopy = |wet: usize| {
        move |[x, y, z]: [i32; 3]| match (x, y, z) {
            (8..24, 12, 8..24) => Some(cube[wet]),
            (8..24, 4, 8..24) => Some(fluid),
            (8..24, 3, 8..24) => Some(solid),
            _ => None,
        }
    };
    let quads = |wet| {
        let (_, mesh) = mesh_world(&table, &canopy(wet), [0; 3], None, OPTIONS);
        [CUTOUT_BUCKET, TRANSLUCENT_BUCKET].map(|b| mesh.bucket(b).len())
    };
    // The pool is 125 translucent quads. Wet: 512 tops and bottoms and 64 rim sides of cubes,
    // as many of water.
    assert_eq!(quads(0), [6, 125]);
    assert_eq!(quads(1), [576, 125 + 576]);
}

/// The water of a wet cube shows whole (a cutout block hides none of it): alone, its 6 faces
/// inset at 12/16 and the cube's 6 faces; in the top layer of a pool two deep, only its top,
/// level with the pool, and still the cube's 6 faces (water all around it). The cube's faces go
/// to the cutout bucket, its water to the translucent one.
#[test]
fn the_water_of_a_wet_cube() {
    use Face::*;
    let (registry, table) = held_registry();
    let wet = state(&registry, "test:wet_cube[waterlogged=true]");
    let fluid = state(&registry, "test:fluid");
    let solid = state(&registry, "test:solid");
    let buckets = |mesh: &ChunkMesh, p: [u32; 3]| {
        for b in 0..BUCKET_COUNT {
            for q in mesh.bucket(b).iter().filter(|q| q.pos() == p) {
                let want = match q.shape() {
                    layout::SHAPE_FLUID_SLOPE => TRANSLUCENT_BUCKET,
                    _ => CUTOUT_BUCKET,
                };
                assert_eq!(b, want, "{q:?}");
            }
        }
    };
    let p = [5, 5, 5];
    let (_, mesh) = mesh_of(&table, |x, y, z| ((x, y, z) == (5, 5, 5)).then_some(wet));
    assert_eq!(water_at(&mesh, p, [12; 4]), BTreeSet::from(Face::ALL));
    assert_eq!(cube_quads(&mesh, p).len(), 6);
    buckets(&mesh, p);
    let (_, mesh) = mesh_of(&table, |x, y, z| match (x, y, z) {
        (5, 5, 5) => Some(wet),
        (3..=7, 4..=5, 3..=7) => Some(fluid),
        (3..=7, 3, 3..=7) => Some(solid),
        _ => None,
    });
    let flat = [FLUID_FLAT; 4];
    assert_eq!(water_at(&mesh, p, flat), BTreeSet::from([PosY]));
    assert_eq!(cube_quads(&mesh, p).len(), 6);
    buckets(&mesh, p);
}

/// A wet cube culls as the dry one and its water as water: no face between two wet cubes, cube
/// or water; between a wet and a dry cube of one block, the dry one's face (it sees water) and
/// the water's face toward it, but not the wet one's (it sees its dry twin); none of its own
/// against an opaque cube (whose face shows, as against a dry cube); both cube faces and the
/// water's against other leaves; and against a wet slab the water hides, the slab's box face
/// and the cube's face show.
#[test]
fn a_wet_cube_culls_as_the_dry_one() {
    use Face::*;
    let (registry, table) = held_registry();
    let s = |name: &str| state(&registry, name);
    let wet = s("test:wet_cube[waterlogged=true]");
    let dry = s("test:wet_cube[waterlogged=false]");
    let (solid, cutout) = (s("test:solid"), s("test:cutout"));
    let slab = s("test:slab[waterlogged=true]");
    // Each pair in its own row: the wet cube at x = 5, the other at x = 6.
    let pairs = [wet, dry, solid, cutout, slab];
    let (_, mesh) = mesh_of(&table, |x, y, z| match (x, z) {
        (5, 5) if y % 5 == 0 && y / 5 < 5 => Some(wet),
        (6, 5) if y % 5 == 0 && y / 5 < 5 => Some(pairs[(y / 5) as usize]),
        _ => None,
    });
    let row = |k: u32| ([5, 5 * k, 5], [6, 5 * k, 5]);
    let has = |faces: BTreeMap<Face, PackedQuad>, f: Face| faces.contains_key(&f);
    let water = |p: [u32; 3]| quads_at(&mesh, p, layout::SHAPE_FLUID_SLOPE);
    // Two wet cubes.
    let (a, b) = row(0);
    assert!(!has(cube_quads(&mesh, a), PosX) && !has(cube_quads(&mesh, b), NegX));
    assert!(!has(water(a), PosX) && !has(water(b), NegX));
    assert_eq!(cube_quads(&mesh, a).len(), 5);
    // A wet and a dry cube of the same block.
    let (a, b) = row(1);
    assert!(!has(cube_quads(&mesh, a), PosX));
    assert!(has(cube_quads(&mesh, b), NegX));
    assert!(has(water(a), PosX));
    // Against an opaque cube.
    let (a, b) = row(2);
    assert!(!has(cube_quads(&mesh, a), PosX) && !has(water(a), PosX));
    assert!(has(cube_quads(&mesh, b), NegX), "the solid's face shows");
    // Against other leaves.
    let (a, b) = row(3);
    assert!(has(cube_quads(&mesh, a), PosX) && has(cube_quads(&mesh, b), NegX));
    assert!(has(water(a), PosX));
    // Against a wet slab: the slab's water and the cube's hide each other.
    let (a, b) = row(4);
    assert!(has(cube_quads(&mesh, a), PosX));
    assert!(!has(water(a), PosX) && !has(water(b), NegX));
    let slab_faces: Vec<Face> = mesh
        .quads
        .iter()
        .filter(|q| q.pos() == b && q.shape() == layout::SHAPE_BOX)
        .map(|q| q.face())
        .collect();
    assert!(slab_faces.contains(&NegX), "{slab_faces:?}");
}
