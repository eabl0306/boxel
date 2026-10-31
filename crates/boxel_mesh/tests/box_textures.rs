//! A texture per box face (EW-BOXTEX, ADR 0045): a box face shows the layer its model names, or
//! the block's; a face that crops its texture reads its own model box table entry; a hidden
//! face is not drawn and hides no neighbor face.

mod common;

use boxel_mesh::{
    AoPolicy, BUCKET_COUNT, ChunkMesh, MaterialTable, MeshOptions, Mesher, PaddedChunk, layout,
    quad_model_box,
};
use boxel_voxel::{BlockModel, BoxFaceTexture, BoxModelId, Face, ModelBox, RenderShape, TextureId};
use common::set_material;

/// The block's layer of (material, face) is `material * 6 + face`; the box layers start at 100.
const BLOCK_LAYERS: u16 = 6;
const NAMED: u16 = 100;

/// Material 1: an opaque cube. 2: a candle-like post with a named layer on its sides and a
/// crop on its top. 3: a cutout full box whose -X face is hidden. 4: the same full box, every
/// face the block's.
fn table() -> MaterialTable {
    let shapes = vec![
        RenderShape::None,
        RenderShape::Opaque,
        RenderShape::Cutout,
        RenderShape::Cutout,
        RenderShape::Cutout,
    ];
    let layers = (0..shapes.len() as u16)
        .map(|m| Face::ALL.map(|f| m * BLOCK_LAYERS + f.index() as u16))
        .collect();
    let mut t = MaterialTable::from_parts(shapes, layers);
    let post = ModelBox::new([7, 0, 7], [9, 6, 9]);
    let mut sides = [BoxFaceTexture::layer(TextureId(NAMED)).at(0, 8); 6];
    sides[Face::PosY.index()] = BoxFaceTexture::layer(TextureId(NAMED + 1)).at(0, 6);
    sides[Face::NegY.index()] = BoxFaceTexture::BLOCK;
    t.set_textured_model(2, BlockModel::Boxes(BoxModelId(2)), &[post], &[sides]);
    let full = ModelBox::new([0; 3], [16; 3]);
    let mut open = [BoxFaceTexture::BLOCK; 6];
    open[Face::NegX.index()] = BoxFaceTexture::HIDDEN;
    t.set_textured_model(3, BlockModel::Boxes(BoxModelId(3)), &[full], &[open]);
    t.set_model(4, BlockModel::Boxes(BoxModelId(4)), &[full]);
    t
}

fn mesh(padded: &PaddedChunk, table: &MaterialTable) -> ChunkMesh {
    let mut out = ChunkMesh::default();
    let options = MeshOptions::with_ao(AoPolicy::Off);
    Mesher::new().mesh(padded, None, table, options, &mut out);
    out
}

/// The box quads at `p`: face, layer and model box.
fn box_quads(mesh: &ChunkMesh, table: &MaterialTable, p: [u32; 3]) -> Vec<(Face, u16, ModelBox)> {
    let mut quads: Vec<_> = (0..BUCKET_COUNT)
        .flat_map(|b| mesh.bucket(b).iter())
        .filter(|q| q.shape() == layout::SHAPE_BOX && q.pos() == p)
        .map(|q| {
            // The entry turned by the quad's turns (ADR 0052).
            let Some(b) = quad_model_box(*q, table.model_boxes()) else {
                panic!("a box face indexes a plane: {q:?}");
            };
            (q.face(), q.layer(), b)
        })
        .collect();
    quads.sort_by_key(|q| q.0.index());
    quads
}

#[test]
fn box_faces_show_their_own_layers_and_crops() {
    let table = table();
    let mut padded = PaddedChunk::new();
    set_material(&mut padded, &table, [5, 5, 5], 2);
    let quads = box_quads(&mesh(&padded, &table), &table, [5, 5, 5]);
    assert_eq!(quads.len(), 6);
    let post = ModelBox::new([7, 0, 7], [9, 6, 9]);
    for (face, layer, b) in quads {
        assert_eq!(b.bounds, post.bounds, "{face:?}");
        match face {
            // The block's bottom, under the box.
            Face::NegY => assert_eq!((layer, b), (2 * BLOCK_LAYERS + 1, post)),
            // The top crops texel (0, 6): u along x from 7, v along z from 7.
            Face::PosY => {
                assert_eq!(layer, NAMED + 1);
                assert_eq!((b.texture_offset[0], b.texture_offset[2]), (9, 15));
            }
            // The sides crop (0, 8): u from 7 (x or z), the top row 8 at y = 6.
            _ => {
                assert_eq!(layer, NAMED, "{face:?}");
                let (_, u, _) = face.axes();
                assert_eq!(b.texture_offset[u], 9, "{face:?}");
                assert_eq!(b.texture_offset[1], 2, "{face:?}");
            }
        }
    }
}

#[test]
fn crops_take_their_own_table_entries_and_share_them() {
    let table = table();
    // The post's top and its ±Z sides share an entry (the same x offset), its bottom reads the
    // plain box and its ±X sides a third (their z offset differs from the top's); the two full
    // boxes share theirs.
    let boxes = table.model_boxes();
    assert_eq!(boxes.len(), 4, "{boxes:?}");
}

#[test]
fn hidden_faces_are_not_drawn_and_hide_nothing() {
    let table = table();
    let mut padded = PaddedChunk::new();
    // The open box with a plain one to its -X: the open box draws no -X face, and the plain
    // box's +X face stays, since nothing covers it.
    set_material(&mut padded, &table, [5, 5, 5], 3);
    set_material(&mut padded, &table, [4, 5, 5], 4);
    let m = mesh(&padded, &table);
    let open: Vec<Face> = box_quads(&m, &table, [5, 5, 5])
        .iter()
        .map(|q| q.0)
        .collect();
    assert!(!open.contains(&Face::NegX), "{open:?}");
    assert_eq!(open.len(), 5);
    let plain: Vec<Face> = box_quads(&m, &table, [4, 5, 5])
        .iter()
        .map(|q| q.0)
        .collect();
    assert!(plain.contains(&Face::PosX), "{plain:?}");
    // Two plain boxes side by side hide their shared faces (the same block).
    let mut padded = PaddedChunk::new();
    set_material(&mut padded, &table, [5, 5, 5], 4);
    set_material(&mut padded, &table, [4, 5, 5], 4);
    let m = mesh(&padded, &table);
    let plain: Vec<Face> = box_quads(&m, &table, [4, 5, 5])
        .iter()
        .map(|q| q.0)
        .collect();
    assert!(!plain.contains(&Face::PosX), "{plain:?}");
}

/// ADR 0052: the four facings of a model share its table entries, and each facing's quads draw
/// its own turned boxes (the entry turned by the quad's turns), full bright or not.
#[test]
fn turned_models_share_their_table_entries() {
    let step = [
        ModelBox::new([0, 0, 0], [16, 8, 16]),
        ModelBox {
            texture_offset: [3, 0, -5],
            ..ModelBox::new([8, 8, 0], [16, 16, 16])
        },
        ModelBox::new([1, 8, 2], [4, 12, 7]),
    ];
    let shapes = vec![RenderShape::Cutout; 9];
    let layers = (0..9u16)
        .map(|m| Face::ALL.map(|f| m * BLOCK_LAYERS + f.index() as u16))
        .collect();
    let mut t = MaterialTable::from_parts(shapes, layers);
    for m in 1..9u16 {
        let turns = (m - 1) % 4;
        let boxes = step.map(|b| b.turned(turns as u8));
        t.set_model(m, BlockModel::Boxes(BoxModelId(m)), &boxes);
        t.set_emissive(m, m >= 5);
    }
    assert_eq!(t.model_boxes().len(), step.len(), "{:?}", t.model_boxes());
    for m in 1..9u16 {
        let turns = ((m - 1) % 4) as u8;
        let mut padded = PaddedChunk::new();
        set_material(&mut padded, &t, [5, 5, 5], m);
        let mesh = mesh(&padded, &t);
        let quads = box_quads(&mesh, &t, [5, 5, 5]);
        assert!(!quads.is_empty());
        for (face, _, b) in &quads {
            assert!(
                step.iter().any(|s| s.turned(turns) == *b),
                "material {m}: {face:?} draws {b:?}"
            );
        }
        let all = (0..BUCKET_COUNT).flat_map(|b| mesh.bucket(b).iter());
        for q in all.filter(|q| q.shape() == layout::SHAPE_BOX) {
            assert_eq!(q.is_emissive(), m >= 5, "{q:?}");
        }
    }
}
