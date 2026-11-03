//! Cell quads (EW-ITEM, ADR 0050): a block's cells with data draw the quads its rule gives for
//! that data, moved into the chunk and lit by the cell; cells without data, or of blocks without
//! a rule, draw nothing more, and their mesh is the one they had before the rule existed.

use boxel_light::PaddedLight;
use boxel_mesh::{ChunkMesh, LitCellQuad, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_voxel::{
    Axis, BlockCollision, BlockData, BlockDef, BlockModel, BlockRegistry, BlockState, BoxModel,
    CellQuad, CellQuadRule, ChunkData, DataValue, Face, FaceTextures, LightValue, LocalPos,
    ModelBox, ModelPlane, PlaneKind, Property, RenderShape, TextureImage, TextureSpec,
};
use std::sync::Arc;

/// The plane of a frame's item: across Z, a little in front of the back of the cell.
fn item_plane(spin: u8) -> ModelPlane {
    ModelPlane::new(PlaneKind::Mid(Axis::Z), [8, 8, 2], [24, 24, 2]).spun(spin)
}

/// The test blocks plus `test:frame`, a thin cutout box with a `glow` property, with or without
/// its cell quad rule: a cell with an `image` field draws that image on [`item_plane`], spun by
/// its `spin` field; a glowing state draws it full bright, shaded as `NegZ`.
fn registry(with_rule: bool) -> (BlockRegistry, [BlockState; 2]) {
    let mut b = boxel_voxel::testing::builder();
    let solid = b
        .add_texture(TextureSpec {
            name: "test:frame".to_string(),
            image: TextureImage::solid(boxel_voxel::testing::TEXTURE_SIZE, [120, 80, 40, 255]),
        })
        .expect("layer");
    let model = b
        .add_box_model(BoxModel::from_boxes(vec![ModelBox::new(
            [2, 2, 0],
            [14, 14, 1],
        )]))
        .expect("model");
    let def = BlockDef {
        name: "test:frame".to_string(),
        shape: RenderShape::Cutout,
        model: BlockModel::Boxes(model),
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(solid),
    };
    let first = b
        .add_block_with_states(
            def,
            vec![Property::new("glow", ["false", "true"])],
            |_, _| {},
        )
        .expect("frame");
    let block = b.block_by_name("test:frame").expect("block");
    if with_rule {
        let rule = CellQuadRule::new(|registry, state| {
            let glow = registry.property(state, "glow") == Some("true");
            Some(Arc::new(
                move |data: &BlockData, out: &mut Vec<CellQuad>| {
                    let Some(image) = data.int("image") else {
                        return;
                    };
                    let spin = data.int("spin").unwrap_or(0) as u8;
                    let quad = CellQuad::from_plane(&item_plane(spin), image as u16);
                    out.push(if glow {
                        quad.with_full_bright(true).shaded_as(Face::NegZ)
                    } else {
                        quad
                    });
                },
            ))
        });
        b.set_cell_quads(block, rule).expect("rule");
    }
    let registry = b.build();
    (registry, [first, BlockState(first.0 + 1)])
}

/// A stone floor at y = 0 and frames at (3, 4, 5) (data `image` 7, `spin` 2), (6, 4, 5)
/// glowing (`image` 9), (9, 4, 5) (no data) and (12, 4, 5) (data without an `image`).
fn chunk(registry: &BlockRegistry, frames: [BlockState; 2], data: bool) -> ChunkData {
    let stone = registry.by_name("test:solid").expect("stone");
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for x in 0..32 {
        for z in 0..32 {
            dense[LocalPos::new(x, 0, z).index()] = stone;
        }
    }
    let cells = [
        (3, frames[0]),
        (6, frames[1]),
        (9, frames[0]),
        (12, frames[0]),
    ];
    for (x, s) in cells {
        dense[LocalPos::new(x, 4, 5).index()] = s;
    }
    let mut chunk = ChunkData::from_dense(&dense);
    if data {
        let at = |x| LocalPos::new(x, 4, 5);
        let with = |image: i64, spin: i64| {
            BlockData::new()
                .with("image", DataValue::Int(image))
                .with("spin", DataValue::Int(spin))
        };
        chunk.set_block_data(at(3), Some(with(7, 2)));
        chunk.set_block_data(at(6), Some(with(9, 0)));
        chunk.set_block_data(
            at(12),
            Some(BlockData::new().with("spin", DataValue::Int(1))),
        );
    }
    chunk
}

fn mesh(registry: &BlockRegistry, chunk: &ChunkData, light: Option<&PaddedLight>) -> ChunkMesh {
    let table = MaterialTable::from_registry(registry);
    let input = MeshInput {
        center: chunk,
        neighbors: [None; 26],
        light,
    };
    Mesher::new().mesh_chunk(&input, &table, MeshOptions::default())
}

#[test]
fn cells_with_data_draw_their_rule_quads_in_the_chunk() {
    let (registry, frames) = registry(true);
    let chunk = chunk(&registry, frames, true);
    let mesh = mesh(&registry, &chunk, None);
    let want = [
        LitCellQuad {
            quad: CellQuad::from_plane(&item_plane(2), 7).moved([3.0, 4.0, 5.0]),
            light: LightValue::FULL_SKY,
        },
        LitCellQuad {
            quad: CellQuad::from_plane(&item_plane(0), 9)
                .with_full_bright(true)
                .shaded_as(Face::NegZ)
                .moved([6.0, 4.0, 5.0]),
            light: LightValue::FULL_SKY,
        },
    ];
    assert_eq!(mesh.cell_quads, want);
    // Spun 45 degrees: corners off the box, still on its plane, in the cell.
    let first = mesh.cell_quads[0].quad;
    assert!(first.corners.iter().all(|c| c[2] == 5.0625));
    assert!(first.corners.iter().all(|c| (3.0..=4.0).contains(&c[0])));
    // The block quads are the ones the frames draw without data.
    let bare = self::mesh(&registry, &self::chunk(&registry, frames, false), None);
    assert_eq!(
        (&mesh.quads, mesh.bucket_counts),
        (&bare.quads, bare.bucket_counts)
    );
    assert!(bare.cell_quads.is_empty());
    assert_ne!(mesh.hash, bare.hash, "the cell quads are part of the hash");
    assert_eq!(mesh.hash, mesh.compute_hash());
}

#[test]
fn a_cell_quad_takes_its_cell_light() {
    let (registry, frames) = registry(true);
    let chunk = chunk(&registry, frames, true);
    let lamp = LightValue(0x3A51);
    let mesh = mesh(&registry, &chunk, Some(&PaddedLight::Uniform(lamp)));
    assert_eq!(mesh.cell_quads.len(), 2);
    assert!(mesh.cell_quads.iter().all(|q| q.light == lamp));
}

/// Colored sky (ADR 0064): a cell quad under tinted sky has its cell's sky channels in
/// `cell_sky_rgb` and their brightest in its light; under gray sky, `cell_sky_rgb` stays empty
/// and the light holds the picture's sky level.
#[test]
fn a_cell_quad_under_colored_sky_takes_its_cell_sky() {
    let (registry, frames) = registry(true);
    let chunk = chunk(&registry, frames, true);
    let filled = |v: u16| -> Box<[u16; boxel_mesh::PADDED_VOLUME]> {
        vec![v; boxel_mesh::PADDED_VOLUME]
            .into_boxed_slice()
            .try_into()
            .expect("PADDED_VOLUME")
    };
    let colored = |sky: u16| PaddedLight::Colored {
        light: filled(0xF012),
        sky_rgb: filled(sky),
    };
    let red = mesh(&registry, &chunk, Some(&colored(0x0D76)));
    assert_eq!(red.cell_quads.len(), 2);
    assert!(red.cell_quads.iter().all(|q| q.light == LightValue(0xD012)));
    assert_eq!(red.cell_sky_rgb, [0x0D76; 2]);
    assert_eq!(red.hash, red.compute_hash());
    let black = mesh(&registry, &chunk, Some(&colored(0x0666)));
    assert!(black.cell_sky_rgb.is_empty());
    assert!(
        black
            .cell_quads
            .iter()
            .all(|q| q.light == LightValue(0x6012))
    );
    assert_ne!(red.hash, black.hash);
}

#[test]
fn without_a_rule_or_without_data_the_mesh_is_unchanged() {
    let (plain, frames) = registry(false);
    let (ruled, ruled_frames) = registry(true);
    assert_eq!(frames, ruled_frames);
    assert!(!MaterialTable::from_registry(&plain).has_cell_quads());
    assert!(MaterialTable::from_registry(&ruled).has_cell_quads());
    // Data on cells of a block without a rule draws nothing.
    let with_data = mesh(&plain, &chunk(&plain, frames, true), None);
    assert!(with_data.cell_quads.is_empty());
    // A registry with the rule meshes a chunk without data as one without the rule.
    let a = mesh(&plain, &chunk(&plain, frames, false), None);
    let b = mesh(&ruled, &chunk(&ruled, frames, false), None);
    assert_eq!(a, b);
    assert_eq!(a.hash, with_data.hash);
}

#[test]
fn the_rule_binds_each_state_once_with_the_registry() {
    let (registry, frames) = registry(true);
    let block = registry.block(frames[0]).expect("block");
    let rule = registry.cell_quad_rule(block).expect("rule");
    let glow = rule.bind(&registry, frames[1]).expect("glow state");
    let mut out = Vec::new();
    glow(&BlockData::new().with("image", DataValue::Int(3)), &mut out);
    assert!(out[0].full_bright);
    assert!(registry.meshes_data(frames[0]) && registry.meshes_data(frames[1]));
    let stone = registry.by_name("test:solid").expect("stone");
    assert!(
        registry
            .cell_quad_rule(registry.block(stone).expect("stone"))
            .is_none()
    );
}

/// A block that draws nothing as a block (a box model without boxes) and one quad from its data:
/// a chunk holding only it, with data, has a mesh of cell quads only, and that mesh is not empty
/// (ADR 0053: streaming and the renderer draw it). Without data the mesh is empty.
#[test]
fn a_mesh_of_cell_quads_only_is_not_empty() {
    let mut b = boxel_voxel::testing::builder();
    let model = b
        .add_box_model(BoxModel {
            outline: vec![boxel_voxel::CollisionBox::new([4, 0, 4], [12, 16, 12])],
            ..BoxModel::default()
        })
        .expect("model");
    let sign = b
        .add_block(BlockDef {
            name: "test:sign".to_string(),
            shape: RenderShape::Cutout,
            model: BlockModel::Boxes(model),
            collision: BlockCollision::None,
            emission: [0; 3],
            light_opacity: 0,
            textures: FaceTextures::all(boxel_voxel::TextureId(0)),
        })
        .expect("sign");
    let block = b.block_by_name("test:sign").expect("block");
    let rule = CellQuadRule::new(|_, _| {
        Some(Arc::new(|_: &BlockData, out: &mut Vec<CellQuad>| {
            out.push(CellQuad::from_plane(&item_plane(0), 1));
        }))
    });
    b.set_cell_quads(block, rule).expect("rule");
    let registry = b.build();
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    dense[LocalPos::new(7, 7, 7).index()] = sign;
    let mut chunk = ChunkData::from_dense(&dense);
    let bare = mesh(&registry, &chunk, None);
    assert!(bare.is_empty());
    chunk.set_block_data(LocalPos::new(7, 7, 7), Some(BlockData::new()));
    let drawn = mesh(&registry, &chunk, None);
    assert!(drawn.quads.is_empty());
    assert_eq!(drawn.cell_quads.len(), 1);
    assert!(!drawn.is_empty());
}

/// `model_quads` draws a block alone in its cell with as many quads as the mesher does there,
/// and on the same faces: the CPU twin leaves out the same faces.
#[test]
fn model_quads_draw_what_the_mesher_draws_for_a_lone_block() {
    let (registry, frames) = registry(false);
    let table = MaterialTable::from_registry(&registry);
    for name in [
        "test:solid",
        "test:slab",
        "test:boxes",
        "test:plant",
        "test:cutout",
        "test:frame",
    ] {
        let s = if name == "test:frame" {
            frames[0]
        } else {
            registry.by_name(name).expect(name)
        };
        let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
        dense[LocalPos::new(9, 9, 9).index()] = s;
        let chunk = ChunkData::from_dense(&dense);
        let input = MeshInput {
            center: &chunk,
            neighbors: [None; 26],
            light: None,
        };
        let terrain = Mesher::new().mesh_chunk(&input, &table, MeshOptions::default());
        let mut quads = Vec::new();
        boxel_voxel::model_quads(&registry, s, &mut quads);
        assert_eq!(quads.len(), terrain.quads.len(), "{name}");
        let mut faces: Vec<Face> = terrain.quads.iter().map(|q| q.face()).collect();
        let mut twin: Vec<Face> = quads.iter().map(|q| q.face).collect();
        if registry.model(s) == BlockModel::Cross {
            // A cross's quads are faces PosX and PosZ in the mesh, shaded as PosY.
            assert!(twin.iter().all(|&f| f == Face::PosY), "{name}");
            continue;
        }
        faces.sort_by_key(|f| f.index());
        twin.sort_by_key(|f| f.index());
        assert_eq!(faces, twin, "{name}");
    }
}
