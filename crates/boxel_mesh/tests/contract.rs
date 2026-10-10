//! Contract tests for skeleton-implemented items (frozen behavior).
use boxel_mesh::*;
use boxel_voxel::{Face, LightValue};

#[test]
fn packed_quad_roundtrip() {
    let q = PackedQuad::new([31, 0, 17], 32, 5, Face::NegX, 4095);
    assert_eq!(q.pos(), [31, 0, 17]);
    assert_eq!((q.w(), q.h()), (32, 5));
    assert_eq!(q.face(), Face::NegX);
    assert_eq!(q.layer(), 4095);
    assert_eq!((q.ao_byte(), q.light_index(), q.flags()), (0xFF, 0, 0));
    let lit = q.with_light_index(4095).with_ao([0, 1, 2, 3]).with_flags(9);
    assert_eq!((lit.light_index(), lit.flags()), (4095, 9));
    assert_eq!([0, 1, 2, 3].map(|k| lit.ao(k)), [0, 1, 2, 3]);
    assert_eq!(lit.ao_byte(), 0b11_10_01_00);
    // Geometry and layer are untouched by the other fields.
    assert_eq!(
        (lit.pos(), lit.w(), lit.h(), lit.face()),
        (q.pos(), 32, 5, Face::NegX)
    );
    assert_eq!(lit.layer(), 4095);
    assert_eq!(lit.with_light_index(7).with_flags(0).light_index(), 7);
    assert_eq!(size_of::<PackedQuad>(), 8);
}

#[test]
fn packed_quad_v2_bit_layout() {
    use layout::*;
    let q = PackedQuad::new([1, 2, 3], 4, 5, Face::PosZ, 6)
        .with_ao_byte(0xA5)
        .with_light_index(0xABC)
        .with_flags(0xD);
    assert_eq!(q.geo >> GEO_FLAGS_SHIFT, 0xD);
    assert_eq!((q.geo >> GEO_FACE_SHIFT) & 7, Face::PosZ as u32);
    assert_eq!(q.mat & MAT_LAYER_MASK, 6);
    assert_eq!((q.mat >> MAT_AO_SHIFT) & 0xFF, 0xA5);
    assert_eq!((q.mat >> MAT_LIGHT_SHIFT) & MAT_LIGHT_MASK, 0xABC);
    assert_eq!(
        (GEO_FLAGS_SHIFT, MAT_LIGHT_SHIFT, MAT_LIGHT_MASK),
        (28, 20, 0xFFF)
    );
}

#[test]
fn palette_entries_pack_four_corners() {
    let corners = [0xF000, 0x0123, 0x4567, 0x89AB].map(LightValue);
    let entry = palette_entry(corners);
    assert_eq!(entry, [0x0123_F000, 0x89AB_4567]);
    assert_eq!(palette_corners(entry), corners);
    assert_eq!(FULL_SKY_ENTRY, [0xF000_F000, 0xF000_F000]);
}

#[test]
fn light_quantization_levels() {
    let corners = [0xF123, 0x7FFF, 0x0001, 0x8888].map(LightValue);
    assert_eq!(LightQuantization::Exact.apply(corners), corners);
    assert_eq!(
        LightQuantization::Block3.apply(corners),
        [0xF022, 0x7EEE, 0x0000, 0x8888].map(LightValue)
    );
    // Rounded means: sky (15 + 7 + 0 + 8 + 2) / 4 = 8, red (1 + 15 + 0 + 8 + 2) / 4 = 6 -> 4,
    // green (2 + 15 + 0 + 8 + 2) / 4 = 6 -> 4, blue (3 + 15 + 1 + 8 + 2) / 4 = 7 -> 4.
    assert_eq!(
        LightQuantization::Flat.apply(corners),
        [LightValue(0x8444); 4]
    );
}

#[test]
fn mesh_hash_covers_quads_counts_and_palette() {
    let quads = vec![PackedQuad::new([0, 0, 0], 1, 1, Face::PosY, 3)];
    let mut counts = [0; BUCKET_COUNT];
    counts[0] = 1;
    let a = ChunkMesh::from_quads(quads.clone(), counts);
    assert_eq!(a.palette, vec![FULL_SKY_ENTRY]);
    assert_eq!(a.hash, a.compute_hash());
    let mut b = a.clone();
    b.palette[0][1] ^= 1;
    assert_ne!(b.compute_hash(), a.hash);
    let mut c = a.clone();
    c.quads[0] = c.quads[0].with_ao_byte(0);
    assert_ne!(c.compute_hash(), a.hash);
    assert_eq!(ChunkMesh::from_quads(quads, counts), a);
    assert!(
        ChunkMesh::from_quads(Vec::new(), [0; BUCKET_COUNT])
            .palette
            .is_empty()
    );
}

#[test]
fn unit_faces_follow_canonical_axes() {
    // +X face: u = Z, v = Y
    let q = PackedQuad::new([4, 10, 2], 3, 2, Face::PosX, 0);
    let faces: Vec<_> = q.unit_faces().collect();
    assert_eq!(faces.len(), 6);
    assert!(faces.contains(&([4, 11, 4], Face::PosX)));
}

#[test]
fn padded_chunk_matches_bgm_layout() {
    let mut p = PaddedChunk::new();
    p.set(0, 0, 0, 7, VoxelClass::Opaque);
    assert_eq!(PaddedChunk::index(0, 0, 0), binary_index(0, 0, 0));
    assert_eq!(p.get(0, 0, 0), 7);
    // row = (x+1) + (y+1)*34, bit = z+1
    assert_eq!(p.opaque[1 + PADDED], 1 << 1);
    p.set(0, 0, 0, 0, VoxelClass::Empty);
    assert_eq!(p.opaque[1 + PADDED], 0);
}

fn binary_index(x: usize, y: usize, z: usize) -> usize {
    (z + 1) + (x + 1) * PADDED + (y + 1) * PADDED_AREA
}

#[test]
fn chunk_mesh_buckets_are_contiguous() {
    let m = ChunkMesh {
        quads: vec![PackedQuad::default(); 4],
        bucket_counts: [1, 0, 2, 0, 0, 0, 0, 1],
        ..ChunkMesh::default()
    };
    assert_eq!(m.bucket_range(2), 1..3);
    assert_eq!(m.bucket(TRANSLUCENT_BUCKET).len(), 1);
}

/// Model boxes (ADR 0020) pack into the two words the shaders read and back, every coordinate
/// and texture offset in range; a box face quad keeps its index in the `w - 1`, `h - 1` bits.
#[test]
fn model_boxes_pack_and_box_quads_keep_their_index() {
    use boxel_mesh::{PackedQuad, layout, pack_model_box, unpack_model_box};
    use boxel_voxel::{Face, ModelBox};
    let registry = boxel_demo::blocks::registry();
    let table = MaterialTable::from_registry(&registry);
    let mut boxes: Vec<ModelBox> = table.model_boxes().to_vec();
    assert!(boxes.len() > 20, "slabs, stairs, fences and torches");
    let mut extreme = ModelBox::new([0, 0, 0], [16, 16, 16]);
    extreme.texture_offset = [-32, 31, 0];
    boxes.push(extreme);
    for b in boxes {
        assert_eq!(unpack_model_box(pack_model_box(b)), b);
    }
    for index in [0, 1, 517, 1023] {
        let q = PackedQuad::new([31, 0, 7], 1, 1, Face::NegZ, 9)
            .with_shape(layout::SHAPE_BOX)
            .with_box_index(index);
        assert_eq!(q.box_index(), index);
        assert_eq!((q.pos(), q.face(), q.layer()), ([31, 0, 7], Face::NegZ, 9));
        assert_eq!(q.shape(), layout::SHAPE_BOX);
    }
}
