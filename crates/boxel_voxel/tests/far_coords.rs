//! World coordinates far from the origin: `BlockPos`/`ChunkPos` are `i64`, exact across the
//! former `i32` edge (`+-2^31` voxels, `+-2^26` chunks) and up to the world limit.
use boxel_voxel::*;
use glam::{DVec3, I64Vec3, IVec3};
use proptest::prelude::*;

/// Voxel coordinates around every former and current edge, negatives included.
const BLOCKS: [i64; 19] = [
    0,
    1,
    -1,
    31,
    -32,
    (1 << 31) - 1,
    1 << 31,
    (1 << 31) + 1,
    -(1 << 31) + 1,
    -(1 << 31),
    -(1 << 31) - 1,
    1 << 40,
    -(1 << 40),
    (1 << 40) + 17,
    -(1 << 40) - 17,
    MAX_BLOCK_COORD,
    MAX_BLOCK_COORD + 31,
    -MAX_BLOCK_COORD,
    -MAX_BLOCK_COORD - 1,
];

#[test]
fn split_and_from_parts_round_trip_at_the_edges() {
    for v in BLOCKS {
        for p in [
            BlockPos::new(v, 0, 0),
            BlockPos::new(0, v, 0),
            BlockPos::new(0, 0, v),
            BlockPos::new(v, -v, v),
        ] {
            let (c, l) = p.split();
            // Floor division and euclidean remainder, computed independently.
            let expect = |v: i64| (v.div_euclid(32), v.rem_euclid(32) as u32);
            assert_eq!((c.x, l.x()), expect(p.x), "{p:?}");
            assert_eq!((c.y, l.y()), expect(p.y), "{p:?}");
            assert_eq!((c.z, l.z()), expect(p.z), "{p:?}");
            assert_eq!(ChunkPos::containing(p), c);
            assert_eq!(BlockPos::from_parts(c, l), p, "{p:?}");
            // The origin is the chunk's min corner, 32 voxels per chunk.
            let o = c.origin();
            assert_eq!(o.as_i64vec3(), c.as_i64vec3() * 32);
            assert_eq!(
                p.as_i64vec3() - o.as_i64vec3(),
                I64Vec3::new(i64::from(l.x()), i64::from(l.y()), i64::from(l.z()),)
            );
        }
    }
}

#[test]
fn chunks_past_the_former_edge_do_not_alias() {
    // Chunk 2^26 starts at voxel 2^31 (it used to wrap to -2^31, the origin of chunk -2^26).
    let east = ChunkPos::new(1 << 26, 0, 0);
    let west = ChunkPos::new(-(1 << 26), 0, 0);
    assert_eq!(east.origin().x, 1 << 31);
    assert_eq!(west.origin().x, -(1 << 31));
    let local = LocalPos::new(5, 0, 0);
    assert_ne!(
        BlockPos::from_parts(east, local),
        BlockPos::from_parts(west, local)
    );
    // The last voxel of chunk 2^26 - 1 is followed by the first of chunk 2^26.
    let last = BlockPos::from_parts(ChunkPos::new((1 << 26) - 1, 0, 0), LocalPos::new(31, 0, 0));
    assert_eq!(last.x, i64::from(i32::MAX));
    assert_eq!(BlockPos::new(last.x + 1, 0, 0).split().0, east);
}

#[test]
fn from_world_floors_exactly_far_away() {
    for d in [
        0.0,
        2_147_483_647.0,
        2_147_483_648.0,
        2_147_483_680.0,
        3e9,
        1e12,
        -2_147_483_648.0,
        -2_147_483_649.0,
        -3e9,
        -1e12,
    ] {
        // `d + 0.5` is exact for these (below 2^52).
        let p = BlockPos::from_world(DVec3::new(d + 0.5, -d - 0.25, d + 0.75));
        assert_eq!(p, BlockPos::new(d as i64, -(d as i64) - 1, d as i64), "{d}");
    }
    // Integers far beyond 2^53 are exact f64 values too: 1e15, 2^60 and the world limit.
    for v in [
        1_000_000_000_000_000i64,
        1 << 60,
        MAX_BLOCK_COORD,
        -MAX_BLOCK_COORD,
    ] {
        let p = BlockPos::from_world(DVec3::splat(v as f64));
        assert_eq!(p, BlockPos::new(v, v, v));
    }
    // Saturating beyond i64, NaN to 0 (never a panic).
    let p = BlockPos::from_world(DVec3::new(f64::INFINITY, -1e300, f64::NAN));
    assert_eq!(p, BlockPos::new(i64::MAX, i64::MIN, 0));
}

#[test]
fn offsets_and_distances_at_the_world_limit() {
    let m = MAX_CHUNK_COORD;
    let corner = ChunkPos::new(m, -m, m);
    assert_eq!(corner.origin().x, MAX_BLOCK_COORD);
    assert_eq!(
        corner.offset(IVec3::new(1, -1, 1)),
        ChunkPos::new(m + 1, -m - 1, m + 1)
    );
    for (n, d) in corner.neighbors26().iter().zip(NEIGHBOR_OFFSETS) {
        assert_eq!(n.as_i64vec3() - corner.as_i64vec3(), d.as_i64vec3());
    }
    // Opposite corners of the world: (2^57)^2 per axis, exact in u128.
    let far = ChunkPos::new(-m, m, -m);
    let axis = 1u128 << 114;
    assert_eq!(corner.distance_sq(far), 3 * axis);
    assert_eq!(corner.distance_sq_xz(far), 2 * axis);
    assert_eq!(far.distance_sq(corner), 3 * axis);
    // Small distances stay small and exact, across the former edge too.
    let a = ChunkPos::new((1 << 26) - 1, 3, -(1 << 26));
    let b = ChunkPos::new((1 << 26) + 2, -1, -(1 << 26) - 12);
    assert_eq!(a.distance_sq(b), 9 + 16 + 144);
    assert_eq!(a.distance_sq_xz(b), 9 + 144);
    // Any i64 pair: saturates instead of overflowing.
    let min = ChunkPos::new(i64::MIN, i64::MIN, i64::MIN);
    let max = ChunkPos::new(i64::MAX, i64::MAX, i64::MAX);
    assert_eq!(min.distance_sq(max), u128::MAX);
}

#[test]
fn clamp_to_world_bounds_every_axis() {
    let p = ChunkPos::new(i64::MAX, i64::MIN, 12);
    assert_eq!(
        p.clamp_to_world(),
        ChunkPos::new(MAX_CHUNK_COORD, -MAX_CHUNK_COORD, 12)
    );
    let inside = ChunkPos::new(MAX_CHUNK_COORD, -(1 << 26), -MAX_CHUNK_COORD);
    assert_eq!(inside.clamp_to_world(), inside);
    // A position clamped to the limit plus the largest radius offset still has an origin.
    let edge = p.clamp_to_world().offset(IVec3::splat(8193));
    assert_eq!(edge.origin().x, (MAX_CHUNK_COORD + 8193) * 32);
}

proptest! {
    #[test]
    fn round_trip_anywhere_inside_the_world(
        x in -MAX_BLOCK_COORD..MAX_BLOCK_COORD + 32,
        y in -MAX_BLOCK_COORD..MAX_BLOCK_COORD + 32,
        z in -MAX_BLOCK_COORD..MAX_BLOCK_COORD + 32,
    ) {
        let p = BlockPos::new(x, y, z);
        let (c, l) = p.split();
        prop_assert!(c.x.abs() <= MAX_CHUNK_COORD && c.y.abs() <= MAX_CHUNK_COORD);
        prop_assert_eq!(BlockPos::from_parts(c, l), p);
        prop_assert_eq!(c.origin().x, x.div_euclid(32) * 32);
    }
}
