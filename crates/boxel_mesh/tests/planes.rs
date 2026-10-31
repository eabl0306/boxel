//! Planes inside a box (EW-PLANE, ADR 0046): a model's planes mesh to plane quads (a cross
//! quad of face +Y) that index their entry in the model box table, carry their kind and spin,
//! go to a bucket drawn from both sides and cover nothing.

mod common;

use boxel_mesh::{
    AoPolicy, BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, MaterialTable, MeshOptions, Mesher,
    PackedQuad, PaddedChunk, layout, pack_plane_entry, unpack_plane_entry,
};
use boxel_voxel::{
    Axis, BlockModel, BoxModelId, Face, ModelBox, ModelPlane, PlaneEntry, PlaneKind, RenderShape,
    TableEntry, TextureId,
};
use common::set_material;

const NAMED: u16 = 100;

/// Material 1: an opaque cube. 2: a cutout small cross (planes only). 3: an opaque post with a
/// tilted plane of a named layer, spun a sixteenth. 4: a cutout chain, two planes with their own
/// rectangles.
fn table() -> MaterialTable {
    let shapes = vec![
        RenderShape::None,
        RenderShape::Opaque,
        RenderShape::Cutout,
        RenderShape::Opaque,
        RenderShape::Cutout,
    ];
    let layers = (0..shapes.len() as u16)
        .map(|m| Face::ALL.map(|f| m * 6 + f.index() as u16))
        .collect();
    let mut t = MaterialTable::from_parts(shapes, layers);
    let cross = ModelPlane::cross([6, 0, 6], [26, 24, 26]);
    t.set_model_parts(2, BlockModel::Boxes(BoxModelId(2)), &[], &[], &cross);
    let post = ModelBox::new([7, 0, 7], [9, 8, 9]);
    let tilted = ModelPlane::new(PlaneKind::Rising(Axis::X), [0, 16, 4], [32, 24, 28])
        .with_texture(TextureId(NAMED))
        .spun(1);
    t.set_model_parts(3, BlockModel::Boxes(BoxModelId(3)), &[post], &[], &[tilted]);
    let chain = [
        ModelPlane::new(PlaneKind::Rising(Axis::Y), [14, 0, 14], [18, 32, 18])
            .with_uv([0, 0, 3, 16]),
        ModelPlane::new(PlaneKind::Falling(Axis::Y), [14, 0, 14], [18, 32, 18])
            .with_uv([3, 0, 6, 16]),
    ];
    t.set_model_parts(4, BlockModel::Boxes(BoxModelId(4)), &[], &[], &chain);
    t
}

fn mesh(padded: &PaddedChunk, table: &MaterialTable) -> ChunkMesh {
    let mut out = ChunkMesh::default();
    Mesher::new().mesh(
        padded,
        None,
        table,
        MeshOptions::with_ao(AoPolicy::Baked),
        &mut out,
    );
    out
}

/// The plane quads at `p` with their buckets.
fn plane_quads(mesh: &ChunkMesh, p: [u32; 3]) -> Vec<(usize, PackedQuad)> {
    (0..BUCKET_COUNT)
        .flat_map(|b| mesh.bucket(b).iter().map(move |q| (b, *q)))
        .filter(|(_, q)| q.is_plane() && q.pos() == p)
        .collect()
}

fn entry(table: &MaterialTable, q: PackedQuad) -> PlaneEntry {
    match table.model_boxes()[usize::from(q.box_index())] {
        TableEntry::Plane(p) => p,
        TableEntry::Box(b) => panic!("a plane quad indexes a box: {b:?}"),
    }
}

#[test]
fn a_cross_in_a_box_is_two_plane_quads_sharing_an_entry() {
    let table = table();
    let mut padded = PaddedChunk::new();
    set_material(&mut padded, &table, [5, 5, 5], 2);
    let m = mesh(&padded, &table);
    let quads = plane_quads(&m, [5, 5, 5]);
    assert_eq!(quads.len(), 2, "{quads:?}");
    assert_eq!(m.total_quads(), 2, "planes only: no box faces");
    let [rising, falling] = ModelPlane::cross([6, 0, 6], [26, 24, 26]);
    for ((bucket, q), want) in quads.iter().zip([rising, falling]) {
        assert_eq!(*bucket, CUTOUT_BUCKET);
        assert_eq!(q.shape(), layout::SHAPE_CROSS);
        assert_eq!(q.face(), Face::PosY);
        assert_eq!(q.plane(), Some((want.kind, 0)));
        assert_eq!(
            q.layer(),
            2 * 6 + Face::PosX.index() as u16,
            "the block's side"
        );
        assert_eq!(q.ao_byte(), 0xFF, "no AO");
        assert_eq!(entry(&table, *q), want.entry());
    }
    assert_eq!(quads[0].1.box_index(), quads[1].1.box_index());
}

#[test]
fn an_opaque_blocks_planes_are_cutout_and_keep_their_spin_and_layer() {
    let table = table();
    let mut padded = PaddedChunk::new();
    set_material(&mut padded, &table, [5, 5, 5], 3);
    let m = mesh(&padded, &table);
    let quads = plane_quads(&m, [5, 5, 5]);
    assert_eq!(quads.len(), 1);
    let (bucket, q) = quads[0];
    assert_eq!(bucket, CUTOUT_BUCKET, "drawn from both sides");
    assert_eq!(q.plane(), Some((PlaneKind::Rising(Axis::X), 1)));
    assert_eq!(q.layer(), NAMED);
    // The post's faces stay in the opaque face buckets.
    let boxes = (0..CUTOUT_BUCKET)
        .flat_map(|b| m.bucket(b).iter())
        .filter(|q| q.shape() == layout::SHAPE_BOX)
        .count();
    assert_eq!(boxes, 6, "the post's faces, over air");
}

#[test]
fn planes_with_their_own_rectangles_take_their_own_entries() {
    let table = table();
    let mut padded = PaddedChunk::new();
    set_material(&mut padded, &table, [5, 5, 5], 4);
    let m = mesh(&padded, &table);
    let quads = plane_quads(&m, [5, 5, 5]);
    assert_eq!(quads.len(), 2);
    assert_ne!(quads[0].1.box_index(), quads[1].1.box_index());
    assert_eq!(entry(&table, quads[0].1).uv, [0, 0, 3, 16]);
    assert_eq!(entry(&table, quads[1].1).uv, [3, 0, 6, 16]);
    // Cross (one entry), tilted plane, post, two chain planes.
    let planes = table
        .model_boxes()
        .iter()
        .filter(|e| matches!(e, TableEntry::Plane(_)))
        .count();
    assert_eq!(planes, 4);
}

#[test]
fn planes_cover_nothing() {
    let table = table();
    let mut padded = PaddedChunk::new();
    // A cube with the cross on each side: all 6 of its faces are drawn.
    set_material(&mut padded, &table, [5, 5, 5], 1);
    for face in Face::ALL {
        let n = face.normal();
        set_material(&mut padded, &table, [5 + n.x, 5 + n.y, 5 + n.z], 2);
    }
    let m = mesh(&padded, &table);
    let cube = m
        .quads
        .iter()
        .filter(|q| q.shape() == layout::SHAPE_CUBE)
        .flat_map(|q| q.unit_faces())
        .filter(|(p, _)| *p == [5, 5, 5])
        .count();
    assert_eq!(cube, 6);
    assert_eq!(m.quads.iter().filter(|q| q.is_plane()).count(), 12);
}

#[test]
fn plane_entries_pack_into_two_words_and_back() {
    for p in [
        PlaneEntry {
            min: [0; 3],
            max: [32; 3],
            uv: [0, 0, 16, 16],
            turn: 0,
        },
        PlaneEntry {
            min: [31, 17, 3],
            max: [32, 32, 29],
            uv: [16, 3, 0, 15],
            turn: 3,
        },
    ] {
        assert_eq!(unpack_plane_entry(pack_plane_entry(p)), p);
    }
    // The plane byte rides in the AO byte, the entry index where w - 1 and h - 1 are.
    for code in 0..9 {
        let kind = PlaneKind::from_code(code).expect("kind");
        let plane = ModelPlane::new(kind, [0; 3], [32; 3]).spun(15);
        let q = PackedQuad::new([31, 2, 30], 1, 1, Face::PosY, 4095)
            .with_shape(layout::SHAPE_CROSS)
            .with_box_index(1023)
            .with_ao_byte(plane.quad_byte())
            .with_light_index(4095);
        assert_eq!(q.plane(), Some((kind, 15)));
        assert_eq!(
            (q.box_index(), q.layer(), q.light_index()),
            (1023, 4095, 4095)
        );
        assert_eq!(q.pos(), [31, 2, 30]);
    }
    // A cross plant quad is no plane.
    let cross = PackedQuad::new([1, 1, 1], 1, 1, Face::PosX, 3).with_shape(layout::SHAPE_CROSS);
    assert_eq!(cross.plane(), None);
}
