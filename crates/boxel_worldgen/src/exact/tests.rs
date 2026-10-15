//! The exact helpers against plain references: Q16 bounds that round up, the 256-bit product
//! and division, and the point-to-segment distance and circumcenter checked exactly, near the
//! origin and next to the world limit (translation invariance with small coordinates as the
//! reference).
use super::*;
use boxel_voxel::MAX_BLOCK_COORD;
use proptest::prelude::*;

#[test]
fn q16_rounds_up() {
    assert_eq!(Q16::ratio(1, 3).raw(), 21_846); // 65536 / 3 = 21845.33
    assert_eq!(Q16::ratio(6, 3), Q16::from_int(2));
    assert_eq!(Q16::ratio(0, 7), Q16::ZERO);
    assert_eq!(Q16::ratio(u128::MAX, 1), Q16::MAX);
    // A denominator past 2^112 takes the wide division.
    assert_eq!(Q16::ratio(1 << 120, 1 << 121).raw(), 32_768);
    assert_eq!(Q16::ratio((1 << 120) + 1, 1 << 121).raw(), 32_769);
    assert_eq!(Q16::from_int(3).mul_up(Q16::ratio(1, 2)), Q16::ratio(3, 2));
    assert_eq!(Q16::from_raw(1).mul_up(Q16::from_raw(1)).raw(), 1);
    assert_eq!(Q16::from_raw(3).times(1), 1);
    assert_eq!(Q16::from_int(5).times(7), 35);
    // Unbounded stays unbounded, and nothing times 0 is 0.
    assert_eq!(Q16::MAX.times(2), u64::MAX);
    assert_eq!(Q16::ONE.times(u64::MAX), u64::MAX);
    assert_eq!(Q16::MAX.mul_up(Q16::from_raw(1)), Q16::MAX);
    assert_eq!(Q16::MAX.scale(1, 1 << 40), Q16::MAX);
    assert_eq!(
        (Q16::MAX.times(0), Q16::MAX.mul_up(Q16::ZERO)),
        (0, Q16::ZERO)
    );
    assert_eq!(div_ceil_sticky(u64::MAX, 4), u64::MAX);
    assert_eq!(div_ceil_sticky(9, 4), 3);
    assert_eq!(mul_shift_ceil(u64::MAX, 3, 16), u64::MAX);
    assert_eq!(mul_shift_ceil(3, 5, 2), 4);
    assert_eq!(Q16::ONE.scale(1, 3).raw(), 21_846);
    assert!(Q16::from_f64(0.5).raw() >= 32_768);
    assert!(Q16::from_f64(1.0 / 3.0).to_f64() >= 1.0 / 3.0);
    assert_eq!(Q16::from_f64(f64::INFINITY), Q16::MAX);
    // sqrt(2) rounded up.
    assert!(u128::from(Q16::SQRT_2.raw()).pow(2) >= 2 << 32);
    assert!(u128::from(Q16::SQRT_2.raw() - 1).pow(2) < 2 << 32);
}

#[test]
fn round_f64_rounds_halves_away_from_zero() {
    for (v, want) in [
        (0.0, 0),
        (0.49, 0),
        (0.5, 1),
        (-0.5, -1),
        (-0.49, 0),
        (2.5, 3),
        (-2.5, -3),
        (1e12 + 0.5, 1_000_000_000_001),
        (-((1u64 << 51) as f64) - 0.5, -(1 << 51) - 1),
    ] {
        assert_eq!(round_f64(v), want, "{v}");
    }
}

#[test]
fn isqrt_ceil_is_the_ceiling() {
    for n in 0u128..2000 {
        let r = isqrt_ceil(n);
        assert!(r * r >= n && (r == 0 || (r - 1) * (r - 1) < n), "{n}");
    }
    assert_eq!(isqrt_ceil(u128::MAX), 1 << 64);
}

proptest! {
    #[test]
    fn mul_wide_matches_u128_and_splits_exactly(a in any::<u128>(), b in any::<u128>()) {
        let (hi, lo) = mul_wide(a, b);
        if let Some(p) = a.checked_mul(b) {
            prop_assert_eq!((hi, lo), (0, p));
        }
        // Low 128 bits are the wrapping product; the division gives the factors back.
        prop_assert_eq!(lo, a.wrapping_mul(b));
        if a != 0 {
            prop_assert_eq!(div_wide(hi, lo, a), (b, 0));
        }
    }

    #[test]
    fn div_wide_is_a_division(q in any::<u128>(), d in 1u128.., r_seed in any::<u128>()) {
        let r = r_seed % d;
        // n = q * d + r, as 256 bits.
        let (hi, lo) = mul_wide(q, d);
        let (lo, carry) = lo.overflowing_add(r);
        let hi = hi + u128::from(carry);
        prop_assert_eq!(div_wide(hi, lo, d), (q, r));
    }
}

/// The squared distance from `p` to the segment `a`-`b` as a reduced-free rational
/// `(num, den)`, straight from the definition with small coordinates (every product in `i128`).
fn reference_segment(p: (i64, i64), a: (i64, i64), b: (i64, i64)) -> (i128, i128) {
    let (ex, ez) = (i128::from(b.0 - a.0), i128::from(b.1 - a.1));
    let (dx, dz) = (i128::from(p.0 - a.0), i128::from(p.1 - a.1));
    let len = ex * ex + ez * ez;
    let point = |q: (i64, i64)| {
        let (x, z) = (i128::from(p.0 - q.0), i128::from(p.1 - q.1));
        (x * x + z * z, 1)
    };
    if len == 0 {
        return point(a);
    }
    let t = dx * ex + dz * ez;
    if t <= 0 {
        point(a)
    } else if t >= len {
        point(b)
    } else {
        // |d|^2 - t^2 / len, over len.
        (len * (dx * dx + dz * dz) - t * t, len)
    }
}

/// `value == num / den`, exactly (small values: the cross products fit in `u128`).
fn equals(value: DistSq, (num, den): (i128, i128)) -> bool {
    let (num, den) = (num as u128, den as u128);
    value.rem < value.den && (value.whole * value.den + value.rem) * den == num * value.den
}

fn small() -> impl Strategy<Value = (i64, i64)> {
    (-5000i64..5000, -5000i64..5000)
}

/// Origins: the origin, the edges of `i32` and `2^40`, and next to the world limit and past it.
fn origin() -> impl Strategy<Value = (i64, i64)> {
    let edges = [
        0,
        1 << 31,
        -(1 << 31),
        1 << 40,
        MAX_BLOCK_COORD - 10_000,
        -MAX_BLOCK_COORD + 10_000,
        (1 << 62) - 10_000,
        -(1 << 62) + 10_000,
    ];
    (
        prop::sample::select(edges.to_vec()),
        prop::sample::select(edges.to_vec()),
    )
}

proptest! {
    #[test]
    fn segment_distance_is_exact_anywhere(
        p in small(), a in small(), b in small(), o in origin()
    ) {
        let want = reference_segment(p, a, b);
        let near = point_segment_distance(p, a, b);
        prop_assert!(equals(near, want), "{near:?} vs {want:?}");
        // The same configuration next to the world limit: the same exact value.
        let at = |q: (i64, i64)| (q.0 + o.0, q.1 + o.1);
        let far = point_segment_distance(at(p), at(a), at(b));
        prop_assert!(equals(far, want), "far {far:?} vs {want:?}");
        prop_assert_eq!(far, near);
        prop_assert!(near.floor() as f64 <= near.to_f64() && near.to_f64() <= near.ceil() as f64);
        let d = near.floor_dist();
        prop_assert!(d * d <= near.floor() && (d + 1) * (d + 1) > near.floor());
        let c = near.ceil_dist();
        prop_assert!(c * c >= near.ceil());
    }

    #[test]
    fn distances_compare_exactly(
        p in small(), a in small(), b in small(), q in small(), c in small(), d in small()
    ) {
        let (x, y) = (point_segment_distance(p, a, b), point_segment_distance(q, c, d));
        let ((xn, xd), (yn, yd)) = (reference_segment(p, a, b), reference_segment(q, c, d));
        prop_assert_eq!(x.cmp(&y), (xn * yd).cmp(&(yn * xd)));
    }
}

#[test]
fn segment_distance_handles_huge_cross_products() {
    // A long segment and a point far off its middle: the cross product passes 2^64.
    let lim = (1i64 << 62) - 1;
    let (a, b) = ((-lim, -lim), (lim, -lim));
    let p = (3, lim);
    let got = point_segment_distance(p, a, b);
    // The foot is (3, -lim): the distance is 2 * lim exactly.
    let want = u128::from(2 * lim as u64).pow(2);
    assert_eq!(got, DistSq::from_int(want));
    assert_eq!(got.floor_dist(), u128::from(2 * lim as u64));
    // A slanted one: the exact value has a fraction; check q * den + r = cross^2.
    let (a, b, p) = ((-lim, -lim + 7), (lim - 3, lim), (lim, -lim));
    let got = point_segment_distance(p, a, b);
    let e = (
        i128::from(b.0) - i128::from(a.0),
        i128::from(b.1) - i128::from(a.1),
    );
    let d = (
        i128::from(p.0) - i128::from(a.0),
        i128::from(p.1) - i128::from(a.1),
    );
    let cross = (e.0 * d.1 - e.1 * d.0).unsigned_abs();
    let len = (e.0 * e.0 + e.1 * e.1) as u128;
    assert_eq!(got.den, len);
    let (hi, lo) = mul_wide(got.whole, len);
    let (lo, carry) = lo.overflowing_add(got.rem);
    assert_eq!((hi + u128::from(carry), lo), mul_wide(cross, cross));
    // The f64 estimate agrees to its precision.
    let approx = cross as f64 * cross as f64 / len as f64;
    assert!((got.to_f64() - approx).abs() <= approx * 1e-12);
}

#[test]
fn segment_distance_endpoints_and_points() {
    let (a, b) = ((0, 0), (10, 0));
    assert_eq!(point_segment_distance((-3, 4), a, b), DistSq::from_int(25));
    assert_eq!(point_segment_distance((13, 4), a, b), DistSq::from_int(25));
    assert_eq!(point_segment_distance((5, 4), a, b), DistSq::from_int(16));
    assert_eq!(point_segment_distance((5, 0), a, b), DistSq::from_int(0));
    assert_eq!(point_segment_distance((1, 1), a, a), DistSq::from_int(2));
    // Equal values with different denominators are equal.
    let x = DistSq {
        whole: 1,
        rem: 1,
        den: 2,
    };
    let y = DistSq {
        whole: 1,
        rem: 3,
        den: 6,
    };
    assert_eq!(x, y);
    assert!(x < DistSq::from_int(2) && x > DistSq::from_int(1));
}

/// `|v - p|^2 * den^2`, exact, for a circumcenter `v` relative to `o` (small numerators).
fn scaled_dist(v: ExactPoint, o: (i64, i64), p: (i64, i64)) -> i128 {
    let dx = v.nx - i128::from(o.0) * v.den - i128::from(p.0 - o.0) * v.den;
    let dz = v.nz - i128::from(o.1) * v.den - i128::from(p.1 - o.1) * v.den;
    dx * dx + dz * dz
}

proptest! {
    #[test]
    fn circumcenter_is_equidistant_anywhere(
        a in small(), b in small(), c in small(), o in origin()
    ) {
        let at = |q: (i64, i64)| (q.0 + o.0, q.1 + o.1);
        let near = circumcenter(a, b, c);
        let far = circumcenter(at(a), at(b), at(c));
        prop_assert_eq!(near.is_none(), orient(a, b, c) == 0);
        if let (Some(v), Some(w)) = (near, far) {
            prop_assert!(v.den > 0);
            let (ra, rb, rc) = (scaled_dist(v, (0, 0), a), scaled_dist(v, (0, 0), b), scaled_dist(v, (0, 0), c));
            prop_assert_eq!(ra, rb);
            prop_assert_eq!(ra, rc);
            // The far one is the near one moved by the origin, exactly.
            prop_assert_eq!(w.den, v.den);
            prop_assert_eq!(w.nx, v.nx + i128::from(o.0) * v.den);
            prop_assert_eq!(w.nz, v.nz + i128::from(o.1) * v.den);
            let (vx, vz) = v.rounded().unwrap();
            let (wx, wz) = w.rounded().unwrap();
            prop_assert_eq!((wx - o.0, wz - o.1), (vx, vz));
            // Rounded to the nearest block.
            prop_assert!((2 * (i128::from(vx) * v.den - v.nx)).abs() <= v.den);
            prop_assert!((2 * (i128::from(vz) * v.den - v.nz)).abs() <= v.den);
        }
    }

    #[test]
    fn incircle_agrees_with_the_circumcenter(
        a in small(), b in small(), c in small(), d in small()
    ) {
        let (a, b, c) = if orient(a, b, c) < 0 { (a, c, b) } else { (a, b, c) };
        if let Some(v) = circumcenter(a, b, c) {
            let r = scaled_dist(v, (0, 0), a);
            let t = scaled_dist(v, (0, 0), d);
            prop_assert_eq!(incircle(a, b, c, d).signum(), (r - t).signum());
        }
    }
}

#[test]
fn orient_and_incircle_signs() {
    assert!(orient((0, 0), (1, 0), (0, 1)) > 0);
    assert!(orient((0, 0), (0, 1), (1, 0)) < 0);
    assert_eq!(orient((0, 0), (1, 1), (2, 2)), 0);
    let (a, b, c) = ((0, 0), (2, 0), (0, 2));
    assert!(incircle(a, b, c, (1, 1)) > 0);
    assert_eq!(incircle(a, b, c, (2, 2)), 0);
    assert!(incircle(a, b, c, (3, 3)) < 0);
    assert_eq!(
        circumcenter(a, b, c).and_then(ExactPoint::rounded),
        Some((1, 1))
    );
    assert_eq!(circumcenter(a, (1, 1), (2, 2)), None);
    // Halves round up.
    let v = ExactPoint::new(3, -3, 2);
    assert_eq!(v.rounded(), Some((2, -1)));
    // Lowest terms with a positive denominator: one point, one value.
    assert_eq!(ExactPoint::new(-6, 9, -12), ExactPoint::new(2, -3, 4));
    assert_eq!(ExactPoint::new(0, 0, -5), ExactPoint::new(0, 0, 1));
    let square = [(0, 0), (2, 0), (2, 2), (0, 2)];
    assert_eq!(
        circumcenter(square[0], square[1], square[2]),
        circumcenter(square[0], square[2], square[3])
    );
    assert_eq!(gcd(0, 0), 0);
    assert_eq!(gcd(12, 18), 6);
    assert_eq!(gcd(1 << 100, 3 << 90), 1 << 90);
}

#[test]
fn rounding_a_far_circumcenter_gives_none() {
    // Nearly collinear points within 2^30 of each other: the center lies near 2^91.
    let m = MAX_SPREAD;
    let (a, b, c) = ((0, 0), (m, m - 1), (-(m - 1), -(m - 2)));
    assert_ne!(orient(a, b, c), 0);
    let v = circumcenter(a, b, c).unwrap();
    assert_eq!(v.rounded(), None);
    assert!(v.nx.unsigned_abs() > u128::from(u64::MAX) * v.den.unsigned_abs());
}

#[test]
#[should_panic(expected = "2^30")]
fn exact_geometry_refuses_points_too_far_apart() {
    let _ = orient((0, 0), (1 << 31, 0), (0, 1));
}
