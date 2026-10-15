//! Exact integer helpers of the noise kit and the fields: [`Q16`] bounds that round up, a
//! Lipschitz bound with its rounding allowance ([`Slope`]), squared distances in `u128`
//! ([`dist_sq`]), the exact squared distance from a point to a segment
//! ([`point_segment_distance`], a [`DistSq`]), the exact circumcenter of three points
//! ([`circumcenter`], an [`ExactPoint`]) and the exact orientation and in-circle predicates
//! ([`orient`], [`incircle`]).
//!
//! Everything here is integer arithmetic, so it gives the same bits on every platform. Points are
//! `(x, z)` block coordinates of magnitude under `2^62`: twice the world limit
//! (`MAX_BLOCK_COORD = 2^61`), so the far LOD's padding and a warp fit with room to spare, and
//! every coordinate difference fits in `i64` with its square in `u128`.

use std::cmp::Ordering;

/// A non-negative real in Q16 fixed point (`raw / 65536`), kept as an upper bound: every
/// operation rounds up and saturates at [`Q16::MAX`], so a bound computed with it never falls
/// under the true value. [`Q16::MAX`] stands for "unbounded" and stays so: a product or quotient
/// of it (by anything but 0) is [`Q16::MAX`] again, or `u64::MAX` as an integer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Q16(u64);

impl Q16 {
    /// Zero.
    pub const ZERO: Self = Self(0);
    /// One.
    pub const ONE: Self = Self(1 << 16);
    /// `sqrt(2)` rounded up (`92682 / 65536`; `92682^2 > 2 * 2^32`).
    pub const SQRT_2: Self = Self(92_682);
    /// The largest value: a bound that promises nothing.
    pub const MAX: Self = Self(u64::MAX);

    /// The value `raw / 65536`.
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw value: `self = raw / 65536`.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// The integer `n` (saturating).
    pub const fn from_int(n: u64) -> Self {
        Self(n.saturating_mul(1 << 16))
    }

    /// `num / den` rounded up. Panics if `den` is 0.
    pub fn ratio(num: u128, den: u128) -> Self {
        assert!(den != 0, "Q16::ratio: the denominator must not be 0");
        // num * 2^16 as a 256-bit value, divided exactly.
        let (hi, lo) = (num >> 112, num << 16);
        if hi >= den {
            return Self::MAX;
        }
        let (q, r) = div_wide(hi, lo, den);
        Self(saturate(q + u128::from(r != 0)))
    }

    /// A Q16 value at or above `v`, at most `2^-16` over it (saturating). Panics if `v` is
    /// negative or NaN.
    ///
    /// Exact: the product by `65536` is exact in `f64`, and the cast truncates; the one added
    /// unit covers the truncation (one unit over when `v` is already a multiple of `2^-16`).
    pub fn from_f64(v: f64) -> Self {
        assert!(v >= 0.0, "Q16::from_f64 of a negative value or NaN: {v}");
        let scaled = v * 65536.0;
        Self((scaled as u64).saturating_add(1))
    }

    /// The value as an `f64` (for display and tests).
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / 65536.0
    }

    /// `self + other` (saturating).
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// `self * other`, rounded up (saturating).
    pub fn mul_up(self, other: Self) -> Self {
        if self.0.min(other.0) != 0 && (self == Self::MAX || other == Self::MAX) {
            return Self::MAX;
        }
        let p = u128::from(self.0) * u128::from(other.0);
        Self(saturate(p.div_ceil(1 << 16)))
    }

    /// `self * num / den`, rounded up (saturating). Panics if `den` is 0.
    pub fn scale(self, num: u64, den: u64) -> Self {
        assert!(den != 0, "Q16::scale: the denominator must not be 0");
        if num != 0 && (self == Self::MAX || num == u64::MAX) {
            return Self::MAX;
        }
        let p = u128::from(self.0) * u128::from(num);
        Self(saturate(p.div_ceil(u128::from(den))))
    }

    /// `self * n`, rounded up to an integer (saturating; `n = u64::MAX` counts as unbounded).
    pub fn times(self, n: u64) -> u64 {
        if self.0 != 0 && n != 0 && (self == Self::MAX || n == u64::MAX) {
            return u64::MAX;
        }
        let p = u128::from(self.0) * u128::from(n);
        saturate(p.div_ceil(1 << 16))
    }
}

/// `ceil(v / d)`, with `u64::MAX` (a saturated, unbounded value) kept as is. Panics if `d` is 0.
pub(crate) const fn div_ceil_sticky(v: u64, d: u64) -> u64 {
    if v == u64::MAX { v } else { v.div_ceil(d) }
}

/// `ceil(a * b / 2^shift)` (saturating; `u64::MAX` in `a` or `b` counts as unbounded).
pub(crate) fn mul_shift_ceil(a: u64, b: u64, shift: u32) -> u64 {
    if a != 0 && b != 0 && (a == u64::MAX || b == u64::MAX) {
        return u64::MAX;
    }
    saturate((u128::from(a) * u128::from(b)).div_ceil(1 << shift))
}

/// `v` as a `u64`, saturated.
#[inline]
pub(crate) fn saturate(v: u128) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// A Lipschitz bound with a rounding allowance: `|f(p) - f(q)| <= per_block * |p - q| + jitter`
/// for every two integer points, `|p - q|` the Euclidean distance (diagonals included). The
/// jitter covers the rounding of a function that is Lipschitz before it is rounded to integers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Slope {
    /// Value units per block.
    pub per_block: Q16,
    /// Value units added at any distance.
    pub jitter: u64,
}

impl Slope {
    /// A constant.
    pub const ZERO: Self = Self {
        per_block: Q16::ZERO,
        jitter: 0,
    };

    /// The bound of `|f(p) - f(q)|` for points at most `len` blocks apart (saturating).
    pub fn over(self, len: u64) -> u64 {
        self.per_block.times(len).saturating_add(self.jitter)
    }

    /// The bound of `|f(p) - f(q)|` for points whose squared distance is at most `dist_sq`.
    pub fn over_sq(self, dist_sq: u128) -> u64 {
        self.over(saturate(isqrt_ceil(dist_sq)))
    }

    /// `max(per_block)` and `max(jitter)`: a bound of both functions.
    pub fn max(self, other: Self) -> Self {
        Self {
            per_block: self.per_block.max(other.per_block),
            jitter: self.jitter.max(other.jitter),
        }
    }

    /// The bound of a sum: both parts added (saturating).
    pub fn sum(self, other: Self) -> Self {
        Self {
            per_block: self.per_block.saturating_add(other.per_block),
            jitter: self.jitter.saturating_add(other.jitter),
        }
    }
}

/// `v` rounded to the nearest integer, halves away from 0, exact for `|v| < 2^52`: the cast
/// truncates, and `v - trunc(v)` is exact there (no `round`, which may be a libm call).
#[inline]
pub(crate) fn round_f64(v: f64) -> i64 {
    let t = v as i64;
    let frac = v - t as f64;
    if frac >= 0.5 {
        t + 1
    } else if frac <= -0.5 {
        t - 1
    } else {
        t
    }
}

/// `ceil(sqrt(n))`.
pub fn isqrt_ceil(n: u128) -> u128 {
    let r = n.isqrt();
    r + u128::from(r * r != n)
}

/// `a * b` as a 256-bit value `(high, low)`.
pub(crate) const fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    const LOW: u128 = u64::MAX as u128;
    let (a1, a0) = (a >> 64, a & LOW);
    let (b1, b0) = (b >> 64, b & LOW);
    let (p00, p01, p10, p11) = (a0 * b0, a0 * b1, a1 * b0, a1 * b1);
    // Each partial product is under 2^128; the middle column sums three 64-bit halves.
    let mid = (p00 >> 64) + (p01 & LOW) + (p10 & LOW);
    let low = (p00 & LOW) | (mid << 64);
    let high = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);
    (high, low)
}

/// `(high * 2^128 + low) / d` and its remainder, for `d > 0` and `high < d` (so the quotient
/// fits in `u128`). Long division, one bit at a time: only the rare wide cases take it.
pub(crate) fn div_wide(high: u128, low: u128, d: u128) -> (u128, u128) {
    assert!(
        d != 0 && high < d,
        "div_wide: the quotient must fit in u128"
    );
    if high == 0 {
        return (low / d, low % d);
    }
    let (mut r, mut q) = (high, 0u128);
    for i in (0..128).rev() {
        // r < d before the shift, so 2r + 1 < 2d: one subtraction brings it back under d.
        let carry = r >> 127;
        r = (r << 1) | ((low >> i) & 1);
        q <<= 1;
        if carry == 1 || r >= d {
            // With the carry the true value is 2^128 + r, and the wrapped difference is exact.
            r = r.wrapping_sub(d);
            q |= 1;
        }
    }
    (q, r)
}

/// `(a * b) cmp (c * d)` for 128-bit factors, exact.
fn cmp_products(a: u128, b: u128, c: u128, d: u128) -> Ordering {
    mul_wide(a, b).cmp(&mul_wide(c, d))
}

/// Squared distance between `a` and `b` (coordinates of magnitude under `2^62`: each difference
/// is under `2^63`, so the sum of squares is under `2^127`).
#[inline]
pub fn dist_sq(a: (i64, i64), b: (i64, i64)) -> u128 {
    let dx = u128::from(a.0.abs_diff(b.0));
    let dz = u128::from(a.1.abs_diff(b.1));
    dx * dx + dz * dz
}

/// An exact squared distance, `whole + rem / den` with `rem < den`: a rational that keeps every
/// comparison exact. Equality and order are those of the value, whatever the representation.
#[derive(Clone, Copy, Debug)]
pub struct DistSq {
    whole: u128,
    rem: u128,
    den: u128,
}

impl DistSq {
    /// The integer squared distance `d2`.
    pub const fn from_int(d2: u128) -> Self {
        Self {
            whole: d2,
            rem: 0,
            den: 1,
        }
    }

    /// `floor` of the squared distance.
    pub const fn floor(self) -> u128 {
        self.whole
    }

    /// `ceil` of the squared distance.
    pub const fn ceil(self) -> u128 {
        self.whole + (self.rem != 0) as u128
    }

    /// `floor` of the distance: `isqrt(floor(d2)) = floor(sqrt(d2))` for any real `d2 >= 0`.
    pub fn floor_dist(self) -> u128 {
        self.whole.isqrt()
    }

    /// `ceil` of the distance: `ceil(sqrt(ceil(d2))) = ceil(sqrt(d2))` for any real `d2 >= 0`.
    pub fn ceil_dist(self) -> u128 {
        isqrt_ceil(self.ceil())
    }

    /// The squared distance as an `f64` (for display and tests).
    pub fn to_f64(self) -> f64 {
        self.whole as f64 + self.rem as f64 / self.den as f64
    }
}

impl PartialEq for DistSq {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for DistSq {}

impl PartialOrd for DistSq {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DistSq {
    fn cmp(&self, other: &Self) -> Ordering {
        // Both fractions are under 1, so the whole parts decide first.
        self.whole
            .cmp(&other.whole)
            .then_with(|| cmp_products(self.rem, other.den, other.rem, self.den))
    }
}

/// The exact squared distance from `p` to the segment `a`-`b` (a point when `a == b`).
///
/// With `e = b - a` and `d = p - a`: the nearest point is `a` when `d . e <= 0`, `b` when
/// `d . e >= |e|^2`, and otherwise the foot of the perpendicular, at squared distance
/// `cross(e, d)^2 / |e|^2`. Every step is exact for coordinates of magnitude under `2^62`
/// (anywhere in the world and well past its limit), with `u128` products and, when the cross
/// product passes `2^64`, a 256-bit square.
pub fn point_segment_distance(p: (i64, i64), a: (i64, i64), b: (i64, i64)) -> DistSq {
    let e = (
        i128::from(b.0) - i128::from(a.0),
        i128::from(b.1) - i128::from(a.1),
    );
    let d = (
        i128::from(p.0) - i128::from(a.0),
        i128::from(p.1) - i128::from(a.1),
    );
    // Each difference is under 2^63, so each product is under 2^126 and each sum under 2^127.
    let len_sq = (e.0 * e.0 + e.1 * e.1) as u128;
    if len_sq == 0 {
        return DistSq::from_int(dist_sq(p, a));
    }
    let dot = d.0 * e.0 + d.1 * e.1;
    if dot <= 0 {
        return DistSq::from_int(dist_sq(p, a));
    }
    if dot as u128 >= len_sq {
        return DistSq::from_int(dist_sq(p, b));
    }
    let cross = (e.0 * d.1 - e.1 * d.0).unsigned_abs();
    let (whole, rem) = match cross.checked_mul(cross) {
        Some(c2) => (c2 / len_sq, c2 % len_sq),
        None => {
            // The quotient is the squared distance, under |p - a|^2 < 2^127: it fits.
            let (high, low) = mul_wide(cross, cross);
            div_wide(high, low, len_sq)
        }
    };
    DistSq {
        whole,
        rem,
        den: len_sq,
    }
}

/// A point with exact rational coordinates `(nx / den, nz / den)`, `den > 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExactPoint {
    /// Numerator of x.
    pub nx: i128,
    /// Numerator of z.
    pub nz: i128,
    /// The shared denominator, positive.
    pub den: i128,
}

impl ExactPoint {
    /// The nearest block coordinates, halves rounded up (toward `+inf`).
    pub fn rounded(self) -> (i64, i64) {
        let round = |n: i128| -> i64 {
            let r = (2 * n + self.den).div_euclid(2 * self.den);
            i64::try_from(r).expect("a circumcenter of points in range fits in i64")
        };
        (round(self.nx), round(self.nz))
    }
}

/// Largest coordinate difference between the points of [`circumcenter`], [`orient`] and
/// [`incircle`]: their products stay inside `i128`.
pub const MAX_SPREAD: i64 = 1 << 30;

/// `p - o` per axis, checked against [`MAX_SPREAD`].
#[inline]
fn rel(p: (i64, i64), o: (i64, i64)) -> (i128, i128) {
    let (dx, dz) = (p.0 - o.0, p.1 - o.1);
    assert!(
        dx.abs() <= MAX_SPREAD && dz.abs() <= MAX_SPREAD,
        "exact geometry: points more than 2^30 apart"
    );
    (i128::from(dx), i128::from(dz))
}

/// Twice the signed area of the triangle `a b c`: positive when `c` is to the left of `a -> b`
/// (counterclockwise, with x to the right and z up), negative to the right, 0 when collinear.
/// Exact for points within `2^30` of each other per axis (panics otherwise).
pub fn orient(a: (i64, i64), b: (i64, i64), c: (i64, i64)) -> i128 {
    let (b, c) = (rel(b, a), rel(c, a));
    b.0 * c.1 - b.1 * c.0
}

/// Positive when `d` lies inside the circle through `a`, `b` and `c` (taken counterclockwise),
/// negative outside, 0 on it. Exact for points within `2^30` of each other per axis (panics
/// otherwise): every term stays under `2^124`.
pub fn incircle(a: (i64, i64), b: (i64, i64), c: (i64, i64), d: (i64, i64)) -> i128 {
    let (b, c, d) = (rel(b, a), rel(c, a), rel(d, a));
    let (b2, c2, d2) = (
        b.0 * b.0 + b.1 * b.1,
        c.0 * c.0 + c.1 * c.1,
        d.0 * d.0 + d.1 * d.1,
    );
    // The 4 x 4 lifted determinant with `a` moved to the origin is minus this 3 x 3 one.
    -(b.0 * (c.1 * d2 - d.1 * c2) - b.1 * (c.0 * d2 - d.0 * c2) + b2 * (c.0 * d.1 - d.0 * c.1))
}

/// The center of the circle through `a`, `b` and `c`, exact: `None` when they are collinear.
/// The points must lie within `2^30` of each other per axis (panics otherwise).
///
/// With `b' = b - a` and `c' = c - a`, the center is `a + (c'z |b'|^2 - b'z |c'|^2, b'x |c'|^2 -
/// c'x |b'|^2) / D`, `D = 2 cross(b', c')`.
pub fn circumcenter(a: (i64, i64), b: (i64, i64), c: (i64, i64)) -> Option<ExactPoint> {
    let (b, c) = (rel(b, a), rel(c, a));
    let mut den = 2 * (b.0 * c.1 - b.1 * c.0);
    if den == 0 {
        return None;
    }
    let (b2, c2) = (b.0 * b.0 + b.1 * b.1, c.0 * c.0 + c.1 * c.1);
    let (mut ux, mut uz) = (c.1 * b2 - b.1 * c2, b.0 * c2 - c.0 * b2);
    if den < 0 {
        (den, ux, uz) = (-den, -ux, -uz);
    }
    // |a| < 2^62 and den < 2^62: the products stay under 2^124.
    Some(ExactPoint {
        nx: i128::from(a.0) * den + ux,
        nz: i128::from(a.1) * den + uz,
        den,
    })
}

#[cfg(test)]
mod tests;
