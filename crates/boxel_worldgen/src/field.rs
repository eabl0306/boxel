//! Static field combinators: integer 2D fields that carry their own bounds, so the far LOD's
//! margins and a column's air bound come from the composition instead of hand work (worldgen
//! plan sec. 2.5 and 3.2).
//!
//! A [`Field`] is an integer function of `(x, z)` in some fixed point the caller picks (heights
//! in 1/256 block, a weight in `2^-16`...). Besides its value at a point and over a grid (the same
//! bits), it gives:
//! - [`Field::range`] and [`Field::bound`]: every value over the plane, or over a rectangle;
//! - [`Field::slope`]: a Lipschitz bound with a rounding allowance ([`Slope`]), along any line;
//! - [`Field::fine`]: how far the field can stand over or under the bilinear interpolant of its
//!   values at a rectangle's 4 corners ([`Fine`]). That is the margin a LOD cell needs: the
//!   interpolant's maximum is at a corner, so `max corner + fine.up` bounds the field inside.
//!
//! The fields are generic structs, monomorphized: [`LatticeField`] (a child sampled at the points
//! of a `2^k` lattice and interpolated exactly), [`Splined`] (a fixed-point [`Spline`] of a
//! child), [`Add`], [`Min`], [`Blend`], [`Warped`], and the leaves [`Const`] and [`NoiseField`]
//! (a quantized fbm). There is no interpreter and no runtime graph. Each bound is sound: it may be
//! loose, never under the truth, and the tests check every one against the definition.
//!
//! Three rules keep the algebra sound where a naive one is not (worldgen-judge-2 sec. 3):
//! `fine` is measured against the bilinear interpolant, not the corner maximum (so [`Add`] can add
//! them); [`Min`] adds a crossing term (`min(x, s - x)` is 0 at both ends of `[0, s]` and `s / 2`
//! in the middle, while both inputs are linear); and [`Blend`]'s weight term over both axes is
//! `D * (weight change) / scale`, with `D` the gap between its inputs, not half of it.
//!
//! Coordinates are of magnitude under `2^62` (the world is `2^61`), so a cell's far corner and a
//! warp's offset always fit.

use crate::exact::isqrt_ceil;
pub use crate::exact::{Q16, Slope};

mod combine;
mod lattice;
mod leaf;
mod spline;

pub use combine::{Add, Blend, Min, Splined, Warped};
pub use lattice::LatticeField;
pub use leaf::{Const, NoiseField};
pub use spline::Spline;

/// An inclusive range of values `lo..=hi`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    /// The smallest value.
    pub lo: i64,
    /// The largest value.
    pub hi: i64,
}

impl Span {
    /// `lo..=hi`. Panics if `lo > hi`.
    pub const fn new(lo: i64, hi: i64) -> Self {
        assert!(lo <= hi, "Span: lo > hi");
        Self { lo, hi }
    }

    /// The single value `v`.
    pub const fn point(v: i64) -> Self {
        Self { lo: v, hi: v }
    }

    /// The smallest span holding both.
    pub fn hull(self, other: Self) -> Self {
        Self {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }

    /// Whether `v` lies in the span.
    pub const fn contains(self, v: i64) -> bool {
        self.lo <= v && v <= self.hi
    }

    /// `hi - lo`.
    pub const fn width(self) -> u64 {
        self.hi.abs_diff(self.lo)
    }

    /// The largest magnitude, `max(|lo|, |hi|)`.
    pub const fn magnitude(self) -> u64 {
        let (lo, hi) = (self.lo.unsigned_abs(), self.hi.unsigned_abs());
        if lo > hi { lo } else { hi }
    }
}

/// A closed rectangle of blocks, `x0..=x1` by `z0..=z1`, with its 4 corners `(x0, z0)`, `(x1,
/// z0)`, `(x0, z1)` and `(x1, z1)` (a LOD cell of `s` blocks at `(x, z)` is
/// [`Rect::cell`]`(x, z, s)`: its corners are the next cells' corners too).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Lowest x.
    pub x0: i64,
    /// Lowest z.
    pub z0: i64,
    /// Highest x.
    pub x1: i64,
    /// Highest z.
    pub z1: i64,
}

impl Rect {
    /// `x0..=x1` by `z0..=z1`. Panics if a side is reversed.
    pub const fn new(x0: i64, z0: i64, x1: i64, z1: i64) -> Self {
        assert!(x0 <= x1 && z0 <= z1, "Rect: reversed sides");
        Self { x0, z0, x1, z1 }
    }

    /// The square cell `x..=x + size` by `z..=z + size`. Panics if `size` is negative.
    pub const fn cell(x: i64, z: i64, size: i64) -> Self {
        Self::new(x, z, x + size, z + size)
    }

    /// The 4 corners: `(x0, z0)`, `(x1, z0)`, `(x0, z1)`, `(x1, z1)`.
    pub const fn corners(self) -> [(i64, i64); 4] {
        [
            (self.x0, self.z0),
            (self.x1, self.z0),
            (self.x0, self.z1),
            (self.x1, self.z1),
        ]
    }

    /// Half the diagonal, rounded up: `ceil(sqrt(sx^2 + sz^2) / 2)`.
    ///
    /// The bilinear weights `w_c` of a point `p` of the rectangle average the distances to the
    /// corners to at most this: `sum w_c |p - c| <= sqrt(sum w_c |p - c|^2) = sqrt(tx (1 - tx)
    /// sx^2 + tz (1 - tz) sz^2) <= sqrt(sx^2 + sz^2) / 2`. So a field with slope `L` (and jitter
    /// `R`) stands at most `L * half_diagonal + R` from the interpolant of its corners.
    pub fn half_diagonal(self) -> u64 {
        let (sx, sz) = (
            u128::from(self.x1.abs_diff(self.x0)),
            u128::from(self.z1.abs_diff(self.z0)),
        );
        crate::exact::saturate(isqrt_ceil(sx * sx + sz * sz).div_ceil(2))
    }

    /// Whether `(x, z)` lies in the rectangle.
    pub const fn contains(self, x: i64, z: i64) -> bool {
        self.x0 <= x && x <= self.x1 && self.z0 <= z && z <= self.z1
    }
}

/// How far a field may stand from the bilinear interpolant `I` of its values at a rectangle's
/// corners: `I(p) - down <= f(p) <= I(p) + up` at every integer point `p` of the rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Fine {
    /// Value units the field may stand over the interpolant.
    pub up: u64,
    /// Value units the field may stand under it.
    pub down: u64,
}

impl Fine {
    /// No deviation: the field is its interpolant (a constant, or bilinear on the rectangle).
    pub const ZERO: Self = Self { up: 0, down: 0 };

    /// `up` and `down` both `v`.
    pub const fn both(v: u64) -> Self {
        Self { up: v, down: v }
    }

    /// The tighter of two bounds, side by side.
    pub fn min(self, other: Self) -> Self {
        Self {
            up: self.up.min(other.up),
            down: self.down.min(other.down),
        }
    }

    /// Both sides plus `v` (saturating).
    pub const fn plus(self, v: u64) -> Self {
        Self {
            up: self.up.saturating_add(v),
            down: self.down.saturating_add(v),
        }
    }
}

/// An integer 2D field with its bounds (see the [module docs](self)).
///
/// The contract every implementation keeps, for coordinates of magnitude under `2^62`:
/// - `grid` equals `value` at every point, bit for bit;
/// - every value lies in `range()`, and every value at a point of `rect` in `bound(rect)`;
/// - `|value(p) - value(q)| <= slope().per_block * |p - q| + slope().jitter`;
/// - `fine(rect)` bounds the deviation from the interpolant of the 4 corner values over `rect`.
///
/// Only `value`, `range` and `slope` are required: `grid` defaults to a loop, `bound` to the
/// range and `fine` to [`fine_from_slope`].
pub trait Field {
    /// The value at `(x, z)`.
    fn value(&self, x: i64, z: i64) -> i64;

    /// The value at every point of the grid `xs` x `zs` (`out[i * zs.len() + j]` is the value at
    /// `(xs[i], zs[j])`), bit-identical to [`Field::value`]. Fields with a cheaper whole-grid form
    /// (a lattice table, the noise grids) override it.
    ///
    /// # Panics
    ///
    /// If `out.len()` is not `xs.len() * zs.len()`.
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        assert_eq!(out.len(), xs.len() * zs.len(), "Field::grid output size");
        if out.is_empty() {
            return;
        }
        for (row, &x) in out.chunks_exact_mut(zs.len()).zip(xs) {
            for (v, &z) in row.iter_mut().zip(zs) {
                *v = self.value(x, z);
            }
        }
    }

    /// Every value the field takes.
    fn range(&self) -> Span;

    /// Every value the field takes at the integer points of `rect`, inside [`Field::range`].
    fn bound(&self, rect: Rect) -> Span {
        let _ = rect;
        self.range()
    }

    /// The field's Lipschitz bound with its rounding allowance.
    fn slope(&self) -> Slope;

    /// How far the field stands from the interpolant of its corners over `rect`.
    fn fine(&self, rect: Rect) -> Fine {
        fine_from_slope(self, rect)
    }
}

impl<F: Field + ?Sized> Field for &F {
    fn value(&self, x: i64, z: i64) -> i64 {
        (**self).value(x, z)
    }
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        (**self).grid(xs, zs, out);
    }
    fn range(&self) -> Span {
        (**self).range()
    }
    fn bound(&self, rect: Rect) -> Span {
        (**self).bound(rect)
    }
    fn slope(&self) -> Slope {
        (**self).slope()
    }
    fn fine(&self, rect: Rect) -> Fine {
        (**self).fine(rect)
    }
}

/// The 4 corner values of `rect`.
pub fn corner_values<F: Field + ?Sized>(f: &F, rect: Rect) -> [i64; 4] {
    rect.corners().map(|(x, z)| f.value(x, z))
}

/// A sound [`Field::fine`] for any field, from its slope and its bound over `rect`.
///
/// The interpolant is a weighted mean of the corner values, so the field stands at most
/// `slope * half_diagonal` ([`Rect::half_diagonal`]) from it, and never over `bound.hi -
/// min corner` or under `max corner - bound.lo`.
pub fn fine_from_slope<F: Field + ?Sized>(f: &F, rect: Rect) -> Fine {
    let by_slope = f.slope().over(rect.half_diagonal());
    let corners = corner_values(f, rect);
    by_bound(f.bound(rect), corners).min(Fine::both(by_slope))
}

/// What `bound` alone says of the deviation from the interpolant of `corners`: at most
/// `bound.hi - min corner` over it and `max corner - bound.lo` under it.
fn by_bound(bound: Span, corners: [i64; 4]) -> Fine {
    let min = corners.iter().copied().fold(i64::MAX, i64::min);
    let max = corners.iter().copied().fold(i64::MIN, i64::max);
    Fine {
        up: bound.hi.abs_diff(min.min(bound.hi)),
        down: max.max(bound.lo).abs_diff(bound.lo),
    }
}

#[cfg(test)]
mod tests;
