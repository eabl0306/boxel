//! [`LatticeField`]: a child field sampled at the points of a `2^k` lattice and interpolated
//! exactly with integer weights.

use super::{Field, Fine, Rect, Slope, Span, bound_from_slope, fine_from_slope};
use crate::exact::Q16;

/// Lattice points [`LatticeField::bound`] reads one by one; past them it takes its own
/// [`bound_from_slope`] (with the child's bound over the lattice rectangle, if tighter).
const MAX_BOUND_POINTS: u128 = 64;

/// A field known at the points of a lattice of spacing `S = 2^shift` and interpolated between
/// them exactly: the plan's `Lattice<C>`. The child is only ever evaluated at lattice points: a
/// point query reads up to 4 of them, and the grid form reads each lattice point the grid
/// touches once, so a costly child (a spline of an fbm) runs about once per lattice cell of a
/// chunk instead of once per block.
///
/// At `(x, z)`, with `i = x >> shift`, `fx = x & (S - 1)` and the same for z, the value is
/// `floor(((S - fx)(S - fz) v00 + fx (S - fz) v10 + (S - fx) fz v01 + fx fz v11) / S^2)`, `vab`
/// the child at lattice point `((i + a) S, (j + b) S)`. Integer weights and one floor, so a point
/// query, a chunk grid and a LOD corner give the same bits; at a lattice point it is the child's
/// value there.
///
/// Bounds:
/// - range: the child's (a weighted mean of lattice values, floored, stays between them);
/// - slope: neighbor lattice values differ by at most `D = min(child range width, child slope
///   over S)` along an axis, so the interpolant's gradient is at most `sqrt(2) D / S`; jitter 1
///   for the floor;
/// - fine: inside one lattice cell the field is its bilinear interpolant, floored: within 1 of
///   the interpolant of any sub-rectangle's corners, and never over it on a whole lattice cell
///   (its corner values are exact). A rectangle across lattice cells takes [`fine_from_slope`].
#[derive(Clone, Debug, PartialEq)]
pub struct LatticeField<C> {
    child: C,
    shift: u32,
    /// Every weighted sum fits in `i64` (the child's values are small enough).
    pub(super) narrow: bool,
    range: Span,
    slope: Slope,
}

impl<C: Field> LatticeField<C> {
    /// The coarsest lattice: `2^30` blocks (the weights' products stay inside `i128`).
    pub const MAX_SHIFT: u32 = 30;

    /// `child` on a lattice of `2^shift` blocks. Panics if `shift > MAX_SHIFT`.
    pub fn new(shift: u32, child: C) -> Self {
        assert!(shift <= Self::MAX_SHIFT, "LatticeField: 2^{shift} > 2^30");
        let range = child.range();
        let size = 1u64 << shift;
        let step = child.slope().over(size).min(range.width());
        Self {
            // 4 weights of at most S^2 times |v| < 2^(60 - 2 shift): under 2^62.
            narrow: range.magnitude() < 1 << (60 - 2 * shift),
            range,
            slope: Slope {
                per_block: Q16::SQRT_2.scale(step, size),
                jitter: 1,
            },
            child,
            shift,
        }
    }

    /// The child.
    pub const fn child(&self) -> &C {
        &self.child
    }

    /// `log2` of the lattice spacing.
    pub const fn shift(&self) -> u32 {
        self.shift
    }

    /// The interpolation of the corner values `[v00, v10, v01, v11]` at `(fx, fz)` inside a
    /// cell. Shared by the point and the grid paths.
    #[inline]
    fn interpolate(&self, [v00, v10, v01, v11]: [i64; 4], fx: i64, fz: i64) -> i64 {
        let s = 1i64 << self.shift;
        if self.narrow {
            let sum = (s - fx) * (s - fz) * v00
                + fx * (s - fz) * v10
                + (s - fx) * fz * v01
                + fx * fz * v11;
            sum >> (2 * self.shift)
        } else {
            let (s, fx, fz) = (i128::from(s), i128::from(fx), i128::from(fz));
            let sum = (s - fx) * (s - fz) * i128::from(v00)
                + fx * (s - fz) * i128::from(v10)
                + (s - fx) * fz * i128::from(v01)
                + fx * fz * i128::from(v11);
            // A weighted mean of i64 values: it fits.
            (sum >> (2 * self.shift)) as i64
        }
    }

    /// `(cell, position inside it)` of `c`.
    #[inline]
    const fn split(&self, c: i64) -> (i64, i64) {
        (c >> self.shift, c & ((1 << self.shift) - 1))
    }
}

impl<C: Field> Field for LatticeField<C> {
    fn value(&self, x: i64, z: i64) -> i64 {
        let ((i, fx), (j, fz)) = (self.split(x), self.split(z));
        let at = |i: i64, j: i64| self.child.value(i << self.shift, j << self.shift);
        let v00 = at(i, j);
        // A corner of weight 0 is not evaluated: its neighbor stands in (same result).
        let v10 = if fx != 0 { at(i + 1, j) } else { v00 };
        let v01 = if fz != 0 { at(i, j + 1) } else { v00 };
        let v11 = match (fx != 0, fz != 0) {
            (true, true) => at(i + 1, j + 1),
            (true, false) => v10,
            (false, _) => v01,
        };
        self.interpolate([v00, v10, v01, v11], fx, fz)
    }

    /// Splits every coordinate once, evaluates the child over the lattice points the grid needs
    /// (one child grid: at most 2 per coordinate), then interpolates per point.
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        assert_eq!(out.len(), xs.len() * zs.len(), "Field::grid output size");
        if out.is_empty() {
            return;
        }
        // Per axis: the lattice indices needed (sorted, unique) and each coordinate's split.
        let axis = |cs: &[i64]| {
            let splits: Vec<(i64, i64)> = cs.iter().map(|&c| self.split(c)).collect();
            let mut cells: Vec<i64> = Vec::with_capacity(2 * cs.len());
            for &(i, f) in &splits {
                cells.push(i);
                if f != 0 {
                    cells.push(i + 1);
                }
            }
            cells.sort_unstable();
            cells.dedup();
            let at: Vec<usize> = splits
                .iter()
                .map(|&(i, _)| cells.binary_search(&i).expect("every cell was pushed"))
                .collect();
            (splits, cells, at)
        };
        let (x_splits, x_cells, x_at) = axis(xs);
        let (z_splits, z_cells, z_at) = axis(zs);
        let to_blocks =
            |cells: &[i64]| -> Vec<i64> { cells.iter().map(|&i| i << self.shift).collect() };
        let depth = z_cells.len();
        let mut table = vec![0; x_cells.len() * depth];
        self.child
            .grid(&to_blocks(&x_cells), &to_blocks(&z_cells), &mut table);
        for ((row, &(_, fx)), &a) in out.chunks_exact_mut(zs.len()).zip(&x_splits).zip(&x_at) {
            for ((v, &(_, fz)), &b) in row.iter_mut().zip(&z_splits).zip(&z_at) {
                // Cell i + 1 is next to i in the sorted cells whenever its weight is not 0.
                let v00 = table[a * depth + b];
                let v10 = if fx != 0 {
                    table[(a + 1) * depth + b]
                } else {
                    v00
                };
                let v01 = if fz != 0 {
                    table[a * depth + b + 1]
                } else {
                    v00
                };
                let v11 = match (fx != 0, fz != 0) {
                    (true, true) => table[(a + 1) * depth + b + 1],
                    (true, false) => v10,
                    (false, _) => v01,
                };
                *v = self.interpolate([v00, v10, v01, v11], fx, fz);
            }
        }
    }

    fn range(&self) -> Span {
        self.range
    }

    /// The lattice values around `rect` (one by one when there are few, else the child's bound
    /// over them): every value inside lies between its cell's corners.
    fn bound(&self, rect: Rect) -> Span {
        let k = self.shift;
        let mask = (1 << k) - 1;
        let up = |c: i64| (c >> k) + i64::from(c & mask != 0);
        let (i0, i1, j0, j1) = (rect.x0 >> k, up(rect.x1), rect.z0 >> k, up(rect.z1));
        let points = u128::from(i1.abs_diff(i0) + 1) * u128::from(j1.abs_diff(j0) + 1);
        if points > MAX_BOUND_POINTS {
            let lattice = Rect::new(i0 << k, j0 << k, i1 << k, j1 << k);
            return bound_from_slope(self, rect).intersect(self.child.bound(lattice));
        }
        let xs: Vec<i64> = (i0..=i1).map(|i| i << k).collect();
        let zs: Vec<i64> = (j0..=j1).map(|j| j << k).collect();
        let mut values = vec![0; xs.len() * zs.len()];
        self.child.grid(&xs, &zs, &mut values);
        let lo = values.iter().copied().fold(i64::MAX, i64::min);
        let hi = values.iter().copied().fold(i64::MIN, i64::max);
        Span::new(lo, hi)
    }

    fn slope(&self) -> Slope {
        self.slope
    }

    fn fine(&self, rect: Rect) -> Fine {
        let k = self.shift;
        let mask = (1 << k) - 1;
        let (base_x, base_z) = (rect.x0 & !mask, rect.z0 & !mask);
        let inside = rect.x1 - base_x <= 1 << k && rect.z1 - base_z <= 1 << k;
        if !inside {
            return fine_from_slope(self, rect);
        }
        // On one lattice cell the field is floor(B), B bilinear: B equals its own interpolant
        // on any sub-rectangle, and the floors move each side by under 1. With the corners on
        // lattice points their values are B exactly, so the field never stands over it.
        let aligned = rect
            .corners()
            .iter()
            .all(|&(x, z)| x & mask == 0 && z & mask == 0);
        Fine {
            up: u64::from(!aligned),
            down: 1,
        }
    }
}
