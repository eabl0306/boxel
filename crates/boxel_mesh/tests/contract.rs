//! Contract tests for skeleton-implemented items (frozen behavior).
use boxel_mesh::*;
use boxel_voxel::Face;

#[test]
fn packed_quad_roundtrip() {
    let q = PackedQuad::new([31, 0, 17], 32, 5, Face::NegX, 4095);
    assert_eq!(q.pos(), [31, 0, 17]);
    assert_eq!((q.w(), q.h()), (32, 5));
    assert_eq!(q.face(), Face::NegX);
    assert_eq!(q.layer(), 4095);
    assert_eq!(q.sky(), 15);
    assert_eq!(q.with_light(3, 9).block_light(), 9);
    assert_eq!(size_of::<PackedQuad>(), 8);
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
    };
    assert_eq!(m.bucket_range(2), 1..3);
    assert_eq!(m.bucket(TRANSLUCENT_BUCKET).len(), 1);
}
