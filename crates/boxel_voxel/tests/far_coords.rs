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

// ---------------------------------------------------------------- WorldPos

/// Integer shifts of the far-coords scenes: the former `i32` edge both ways, `2^40`, `2^52`
/// (where an `f64` stops holding sub-voxel positions) and the world limit both ways.
const SHIFTS: [i64; 7] = [
    0,
    1 << 31,
    -(1 << 31),
    1 << 40,
    1 << 52,
    MAX_BLOCK_COORD - 64,
    -(MAX_BLOCK_COORD - 64),
];

/// Offsets inside a voxel, exact in `f64`.
const FRACS: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0 - f64::EPSILON / 2.0];

#[test]
fn world_pos_splits_exactly_far_away() {
    // Below 2^52 an f64 still holds a quarter voxel: the split gives the block and frac back.
    for v in BLOCKS.into_iter().filter(|v| v.unsigned_abs() < 1 << 50) {
        for f in FRACS.into_iter().take(4) {
            let p = WorldPos::from_dvec3(DVec3::new(v as f64 + f, -(v as f64) + f, v as f64));
            assert_eq!(p.block, BlockPos::new(v, -v, v), "{v} + {f}");
            assert_eq!(p.frac, DVec3::new(f, f, 0.0), "{v} + {f}");
            assert_eq!(
                p.to_dvec3(),
                DVec3::new(v as f64 + f, -(v as f64) + f, v as f64)
            );
        }
    }
    // Anywhere, up to the limit: from_block keeps the block and the frac exactly (an f64
    // cannot hold these positions), and to_dvec3 rounds them only on the way out.
    for v in BLOCKS.into_iter().filter(|v| v.abs() <= MAX_BLOCK_COORD) {
        for f in FRACS {
            let p = WorldPos::from_block(BlockPos::new(v, v, -v), DVec3::splat(f));
            assert_eq!(
                (p.block, p.frac),
                (BlockPos::new(v, v, -v), DVec3::splat(f))
            );
            assert_eq!(WorldPos::from(p.block).frac, DVec3::ZERO);
        }
    }
    // The last block of the last chunk inside the limit too.
    let last = BlockPos::new(MAX_BLOCK_COORD + 31, 0, 0);
    for f in FRACS {
        let p = WorldPos::from_block(last, DVec3::splat(f));
        assert_eq!((p.block, p.frac), (last, DVec3::splat(f)));
    }
    assert_eq!(
        WorldPos::from_block(BlockPos::new(MAX_BLOCK_COORD, 0, 0), DVec3::splat(0.5))
            .to_dvec3()
            .x,
        MAX_BLOCK_COORD as f64,
        "2^61 + 0.5 rounds to 2^61 in f64"
    );
}

#[test]
fn normalize_carries_a_frac_that_rounds_up_to_one() {
    // -2^-60 - floor(-2^-60) = 1 - 2^-60, which rounds to 1.0: it must carry, never stay 1.
    let tiny = -(2f64.powi(-60));
    let p = WorldPos {
        block: BlockPos::new(5, -7, 1 << 40),
        frac: DVec3::new(tiny, tiny, tiny),
    }
    .normalize();
    assert_eq!(p.block, BlockPos::new(5, -7, 1 << 40));
    assert_eq!(p.frac, DVec3::ZERO);
    let p = WorldPos::from_dvec3(DVec3::new(tiny, 0.0, -0.0));
    assert_eq!((p.block, p.frac), (BlockPos::new(0, 0, 0), DVec3::ZERO));
    assert!(p.frac.x.is_sign_positive() && p.frac.z.is_sign_positive());
    // A move by a tiny negative amount from an integer position.
    let q = WorldPos::from(BlockPos::new(3, 3, 3)).offset(DVec3::splat(tiny));
    assert_eq!((q.block, q.frac), (BlockPos::new(3, 3, 3), DVec3::ZERO));
    // Larger negative fracs borrow from the block, exactly.
    let r = WorldPos::from_block(BlockPos::new(0, 0, 0), DVec3::new(-0.25, -1.5, -3.0));
    assert_eq!(r.block, BlockPos::new(-1, -2, -3));
    assert_eq!(r.frac, DVec3::new(0.75, 0.5, 0.0));
}

#[test]
fn world_pos_clamps_to_the_world_limit_and_never_panics() {
    // The world's blocks: those of the chunks inside the limit, -2^61 ..= 2^61 + 31. A position
    // past them stops on the limit: the min corner on the negative side, just below the max
    // corner (frac 1 - 2^-53) on the positive side.
    let last = MAX_BLOCK_COORD + 31;
    let top = 1.0 - f64::EPSILON / 2.0;
    let p = WorldPos::from_dvec3(DVec3::new(1e300, -1e300, f64::NAN));
    assert_eq!(p.block, BlockPos::new(last, -MAX_BLOCK_COORD, 0));
    assert_eq!(p.frac, DVec3::new(top, 0.0, 0.0));
    let p = WorldPos::from_dvec3(DVec3::new(f64::INFINITY, f64::NEG_INFINITY, 0.5));
    assert_eq!(p.block, BlockPos::new(last, -MAX_BLOCK_COORD, 0));
    assert_eq!(p.frac, DVec3::new(top, 0.0, 0.5));
    // Every block of the last chunk is reachable.
    let inside = WorldPos::from(BlockPos::new(MAX_BLOCK_COORD, 0, 0)).offset(DVec3::X * 31.25);
    assert_eq!((inside.block.x, inside.frac.x), (last, 0.25));
    // A move past the limit stops on it and never wraps back inside the last block: before the
    // fix, 0.9 + 0.2 gave frac 0.1 in the same block (0.8 backward).
    let edge = WorldPos::from_block(
        BlockPos::new(last, -MAX_BLOCK_COORD, 0),
        DVec3::new(0.9, 0.1, 0.0),
    );
    for d in [0.2, 10.5] {
        let out = edge.offset(DVec3::new(d, -d, 0.0));
        assert_eq!(out.block, edge.block, "clamped");
        assert_eq!(out.frac, DVec3::new(top, 0.0, 0.0));
        let moved = edge.delta_to(&out);
        assert!(moved.x > 0.0 && moved.x <= d, "{moved:?}");
        assert!(moved.y < 0.0 && moved.y >= -d, "{moved:?}");
    }
    // Any i64 block: no overflow anywhere.
    let wild = WorldPos {
        block: BlockPos::new(i64::MAX, i64::MIN, 0),
        frac: DVec3::new(5.0, -5.0, 0.0),
    };
    let n = wild.normalize();
    assert_eq!(n.block, BlockPos::new(last, -MAX_BLOCK_COORD, 0));
    let d = wild.delta_to(&WorldPos::default());
    assert!(d.is_finite(), "{d:?}");
}

#[test]
fn delta_to_is_exact_between_far_positions() {
    for shift in SHIFTS {
        // 2^40 toward the origin on z, so the far shifts stay inside the world.
        let far = if shift > 0 { -(1i64 << 40) } else { 1 << 40 };
        let a = WorldPos::from_block(BlockPos::new(shift, -shift, shift), DVec3::splat(0.25));
        let b = WorldPos::from_block(
            BlockPos::new(shift + 3, -shift - 5, shift + far),
            DVec3::new(0.5, 0.75, 0.125),
        );
        assert_eq!(a.delta_to(&b), DVec3::new(3.25, -4.5, far as f64 - 0.125));
        assert_eq!(b.delta_to(&a), -a.delta_to(&b));
        assert_eq!(a.delta_to(&a), DVec3::ZERO);
    }
    // Across the whole world the integer part is 2^62: still exact (a power of two).
    let west = WorldPos::from(BlockPos::new(-MAX_BLOCK_COORD, 0, 0));
    let east = WorldPos::from(BlockPos::new(MAX_BLOCK_COORD, 0, 0));
    assert_eq!(west.delta_to(&east).x, 2f64.powi(62));
}

proptest! {
    /// The same moves from the same frac give the same frac and the same block delta at every
    /// shift: `WorldPos` arithmetic never looks at the size of the block. (At most 32 moves of
    /// less than 2 voxels: the scene stays inside the world at the `MAX_BLOCK_COORD - 64`
    /// shift.)
    #[test]
    fn offsets_are_translation_invariant(
        fx in 0.0..1.0f64, fy in 0.0..1.0f64, fz in 0.0..1.0f64,
        moves in proptest::collection::vec(
            (-1.9..1.9f64, -1.9..1.9f64, -1.9..1.9f64), 1..32),
    ) {
        let frac = DVec3::new(fx, fy, fz);
        let mut reference: Option<Vec<(I64Vec3, DVec3)>> = None;
        for shift in SHIFTS {
            let base = BlockPos::new(shift, -shift, shift);
            let mut p = WorldPos::from_block(base, frac);
            let mut trace = Vec::new();
            for &(x, y, z) in &moves {
                let before = p;
                p = p.offset(DVec3::new(x, y, z));
                prop_assert!(p.frac.cmpge(DVec3::ZERO).all() && p.frac.cmplt(DVec3::ONE).all());
                // The move is the requested one, up to the f64 step of the move itself.
                prop_assert!((before.delta_to(&p) - DVec3::new(x, y, z)).abs().max_element() < 1e-12);
                trace.push((p.block.as_i64vec3() - base.as_i64vec3(), p.frac));
            }
            match &reference {
                None => reference = Some(trace),
                Some(r) => prop_assert_eq!(r, &trace, "shift {}", shift),
            }
        }
    }

    /// `from_dvec3` and `to_dvec3` round-trip every f64 position whose frac is exact.
    #[test]
    fn dvec3_round_trip(x in -1e12..1e12f64, y in -1e12..1e12f64, z in 0.0..1e15f64) {
        let p = DVec3::new(x, y, z);
        let w = WorldPos::from_dvec3(p);
        prop_assert!(w.frac.cmpge(DVec3::ZERO).all() && w.frac.cmplt(DVec3::ONE).all());
        // |p| >= 0.5 for all but a vanishing set of draws; the split is then exact.
        if p.abs().cmpge(DVec3::splat(0.5)).all() {
            prop_assert_eq!(w.to_dvec3(), p);
            prop_assert_eq!(w.block, BlockPos::from_world(p));
        }
    }
}
