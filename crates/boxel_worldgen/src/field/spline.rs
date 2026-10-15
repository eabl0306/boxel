//! [`Spline`]: a piecewise linear curve in fixed point that knows its slopes.

use super::Span;
use crate::exact::{Q16, saturate};

/// A piecewise linear curve through integer knots `(x, y)` with strictly increasing `x`,
/// constant before the first knot and after the last.
///
/// Between knots `(x0, y0)` and `(x1, y1)` it is `y0 + floor((y1 - y0) (x - x0) / (x1 - x0))`:
/// one exact `i128` product and floor division, so every platform gets the same bits and the
/// knots are hit exactly. Inputs and outputs are in whatever fixed points the caller picks (Q12
/// noise in, heights in 1/256 block out...); the slopes are output units per input unit.
///
/// It keeps its steepest slope ([`Spline::max_slope`], the Lipschitz bound before the floor; the
/// floor adds less than 1 at any distance) and answers the range and the slopes over any input
/// interval, for the margins of [`Splined`](super::Splined).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Spline {
    xs: Box<[i64]>,
    ys: Box<[i64]>,
    /// Per segment, its slope in Q16 rounded down and up (signed).
    slopes: Box<[(i128, i128)]>,
    max_slope: Q16,
    range: Span,
}

impl Spline {
    /// The largest knot coordinate: products of differences stay inside `i128`.
    pub const MAX_KNOT: i64 = 1 << 62;

    /// The curve through `knots`. Panics if there is no knot, if the `x` are not strictly
    /// increasing, or if a coordinate is past [`Spline::MAX_KNOT`] in magnitude.
    pub fn new(knots: &[(i64, i64)]) -> Self {
        assert!(!knots.is_empty(), "Spline: no knot");
        assert!(
            knots
                .iter()
                .all(|&(x, y)| x.unsigned_abs() <= Self::MAX_KNOT as u64
                    && y.unsigned_abs() <= Self::MAX_KNOT as u64),
            "Spline: a knot past 2^62"
        );
        assert!(
            knots.windows(2).all(|w| w[0].0 < w[1].0),
            "Spline: knots must have strictly increasing x"
        );
        let slopes: Box<[(i128, i128)]> = knots
            .windows(2)
            .map(|w| {
                let (dx, dy) = (
                    i128::from(w[1].0) - i128::from(w[0].0),
                    i128::from(w[1].1) - i128::from(w[0].1),
                );
                let scaled = dy << 16;
                (scaled.div_euclid(dx), -(-scaled).div_euclid(dx))
            })
            .collect();
        let max_slope = slopes
            .iter()
            .map(|&(lo, hi)| lo.unsigned_abs().max(hi.unsigned_abs()))
            .max()
            .unwrap_or(0);
        let ys: Box<[i64]> = knots.iter().map(|k| k.1).collect();
        let range = Span::new(
            ys.iter().copied().fold(i64::MAX, i64::min),
            ys.iter().copied().fold(i64::MIN, i64::max),
        );
        Self {
            xs: knots.iter().map(|k| k.0).collect(),
            ys,
            slopes,
            max_slope: Q16::from_raw(saturate(max_slope)),
            range,
        }
    }

    /// The curve at `x`.
    pub fn eval(&self, x: i64) -> i64 {
        // The first knot past x.
        let k = self.xs.partition_point(|&kx| kx <= x);
        if k == 0 {
            return self.ys[0];
        }
        if k == self.xs.len() {
            return self.ys[k - 1];
        }
        let (x0, y0) = (self.xs[k - 1], self.ys[k - 1]);
        let (x1, y1) = (self.xs[k], self.ys[k]);
        // Differences in i128: knots 2^62 apart on both sides of 0 are 2^63 apart.
        let step = (i128::from(y1) - i128::from(y0)) * (i128::from(x) - i128::from(x0));
        // Between y0 and y1: it fits.
        (i128::from(y0) + step.div_euclid(i128::from(x1) - i128::from(x0))) as i64
    }

    /// The knots.
    pub fn knots(&self) -> impl Iterator<Item = (i64, i64)> + '_ {
        self.xs.iter().copied().zip(self.ys.iter().copied())
    }

    /// The steepest slope (absolute, output units per input unit), rounded up: `|S(u) - S(v)| <
    /// max_slope * |u - v| + 1` for any integers.
    pub const fn max_slope(&self) -> Q16 {
        self.max_slope
    }

    /// Every value the curve takes.
    pub const fn range(&self) -> Span {
        self.range
    }

    /// Every value the curve takes over the inputs `input`: the ends and the knots between
    /// (the curve is monotone between knots).
    pub fn range_over(&self, input: Span) -> Span {
        let ends = Span::point(self.eval(input.lo)).hull(Span::point(self.eval(input.hi)));
        // The knots strictly inside the interval.
        let (a, b) = (
            self.xs.partition_point(|&x| x <= input.lo),
            self.xs.partition_point(|&x| x < input.hi),
        );
        self.ys[a..b.max(a)]
            .iter()
            .fold(ends, |s, &y| s.hull(Span::point(y)))
    }

    /// The lowest and highest slope over the inputs `input`, in signed Q16 (`raw / 65536`),
    /// rounded outward: every segment that touches the interval, and 0 for a constant end it
    /// reaches.
    pub fn slopes_over(&self, input: Span) -> (i128, i128) {
        let n = self.xs.len();
        let mut lo_hi: Option<(i128, i128)> = None;
        let mut take = |(lo, hi): (i128, i128)| {
            lo_hi = Some(lo_hi.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        };
        if input.lo < self.xs[0] || input.hi > self.xs[n - 1] || n == 1 {
            take((0, 0));
        }
        for (k, &s) in self.slopes.iter().enumerate() {
            if self.xs[k] <= input.hi && self.xs[k + 1] >= input.lo {
                take(s);
            }
        }
        lo_hi.unwrap_or((0, 0))
    }
}
