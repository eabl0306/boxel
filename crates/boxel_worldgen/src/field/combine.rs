//! The combinators: [`Splined`], [`Add`], [`Min`], [`Blend`] and [`Warped`]. Each derives its
//! bounds from its children's; the proofs are in the docs, the checks in the tests.

use super::{Field, Fine, Rect, Slope, Span, Spline, by_bound, corner_values, fine_from_slope};
use crate::exact::{Q16, div_ceil_sticky, mul_shift_ceil, saturate};
use crate::noise::Warp;

/// `spline(child)`: a fixed-point [`Spline`] applied to a field.
///
/// Bounds, with `S` the spline (`S = floor(S_real)`, `S_real` piecewise linear) and `c` the
/// child:
/// - range and bound: the spline's range over the child's;
/// - slope: `|S(c(p)) - S(c(q))| < m |c(p) - c(q)| + 1 <= m L d + m R + 1`, `m` the spline's
///   steepest slope, `(L, R)` the child's;
/// - fine over `rect`: with `m_lo..m_hi` the slopes of `S` over the child's bound there, `m =
///   max(|m_lo|, |m_hi|)`, `e` the child's fine and `w` the spread of its 4 corner values, the
///   deviation is under the smaller of `m e + (m_hi - m_lo) w / 4 + 1` and `m min(w / 2 + e,
///   child slope over half the diagonal) + 1`. In the first, `m e` moves the child to its
///   interpolant, and `(m_hi - m_lo) w / 4` is how far a mean of `S` can stand from `S` of the
///   mean when its slope varies by `m_hi - m_lo` over values `w / 2` from their mean on average.
///   The second bounds how far each corner value is from the child at the point. The floors
///   add under 1. Where the spline is linear over the cell's values the fine is `m e + 1`.
#[derive(Clone, Debug, PartialEq)]
pub struct Splined<C> {
    child: C,
    spline: Spline,
    range: Span,
    slope: Slope,
}

impl<C: Field> Splined<C> {
    /// `spline(child)`.
    pub fn new(child: C, spline: Spline) -> Self {
        let c = child.slope();
        let m = spline.max_slope();
        Self {
            range: spline.range_over(child.range()),
            slope: Slope {
                per_block: m.mul_up(c.per_block),
                jitter: m.times(c.jitter).saturating_add(1),
            },
            child,
            spline,
        }
    }

    /// The child.
    pub const fn child(&self) -> &C {
        &self.child
    }

    /// The spline.
    pub const fn spline(&self) -> &Spline {
        &self.spline
    }
}

impl<C: Field> Field for Splined<C> {
    fn value(&self, x: i64, z: i64) -> i64 {
        self.spline.eval(self.child.value(x, z))
    }
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        self.child.grid(xs, zs, out);
        for v in out.iter_mut() {
            *v = self.spline.eval(*v);
        }
    }
    fn range(&self) -> Span {
        self.range
    }
    fn bound(&self, rect: Rect) -> Span {
        self.spline.range_over(self.child.bound(rect))
    }
    fn slope(&self) -> Slope {
        self.slope
    }
    fn fine(&self, rect: Rect) -> Fine {
        let input = self.child.bound(rect);
        let (m_lo, m_hi) = self.spline.slopes_over(input);
        let m = Q16::from_raw(saturate(m_lo.unsigned_abs().max(m_hi.unsigned_abs())));
        let bend = Q16::from_raw(saturate((m_hi - m_lo).unsigned_abs()));
        let child_fine = self.child.fine(rect);
        let e = child_fine.up.max(child_fine.down);
        let corners = corner_values(&self.child, rect);
        let lo = corners.iter().copied().fold(i64::MAX, i64::min);
        let hi = corners.iter().copied().fold(i64::MIN, i64::max);
        let w = hi.abs_diff(lo);
        let jensen = m
            .times(e)
            .saturating_add(div_ceil_sticky(bend.times(w), 4))
            .saturating_add(1);
        let near =
            (w.div_ceil(2).saturating_add(e)).min(self.child.slope().over(rect.half_diagonal()));
        let mean = m.times(near).saturating_add(1);
        let outputs = corners.map(|c| self.spline.eval(c));
        by_bound(self.spline.range_over(input), outputs).min(Fine::both(jensen.min(mean)))
    }
}

/// `a + b`.
///
/// Bounds add: ranges, bounds, slopes and fines (the interpolant of a sum is the sum of the
/// interpolants, so the deviations add too).
#[derive(Clone, Debug, PartialEq)]
pub struct Add<A, B> {
    a: A,
    b: B,
    range: Span,
}

impl<A: Field, B: Field> Add<A, B> {
    /// `a + b`. Panics if the sum of the ranges leaves `i64` (then no value can overflow).
    pub fn new(a: A, b: B) -> Self {
        let (ra, rb) = (a.range(), b.range());
        let range = match (ra.lo.checked_add(rb.lo), ra.hi.checked_add(rb.hi)) {
            (Some(lo), Some(hi)) => Span::new(lo, hi),
            _ => panic!("Add: the ranges {ra:?} + {rb:?} leave i64"),
        };
        Self { a, b, range }
    }
}

impl<A: Field, B: Field> Field for Add<A, B> {
    fn value(&self, x: i64, z: i64) -> i64 {
        self.a.value(x, z) + self.b.value(x, z)
    }
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        self.a.grid(xs, zs, out);
        let mut b = vec![0; out.len()];
        self.b.grid(xs, zs, &mut b);
        for (v, b) in out.iter_mut().zip(b) {
            *v += b;
        }
    }
    fn range(&self) -> Span {
        self.range
    }
    fn bound(&self, rect: Rect) -> Span {
        let (a, b) = (self.a.bound(rect), self.b.bound(rect));
        Span::new(a.lo + b.lo, a.hi + b.hi)
    }
    fn slope(&self) -> Slope {
        self.a.slope().sum(self.b.slope())
    }
    fn fine(&self, rect: Rect) -> Fine {
        let (a, b) = (self.a.fine(rect), self.b.fine(rect));
        Fine {
            up: a.up.saturating_add(b.up),
            down: a.down.saturating_add(b.down),
        }
    }
}

/// `min(a, b)`.
///
/// Bounds: range and bound `[min lo, min hi]`; slope the larger of both. Fine, with `d_k = a_k -
/// b_k` at the 4 corners and `(ua, da)`, `(ub, db)` the children's fines:
/// - down: `max(da, db)`, since `min(I_a, I_b)` is never under the interpolant of the corner
///   minima;
/// - up: the smallest of `ua + max d_k^+`, `ub + max d_k^-` and `max(ua, ub) + ceil(max |d_k| /
///   2)`. The last one is the crossing term: where `a` and `b` swap order inside the cell the
///   minimum rises over the interpolant of the corner minima (`min(x, s - x)` on `[0, s]`: 0 at
///   both corners, `s / 2` in the middle), by at most the smaller of the two weighted gaps,
///   which is at most half their sum, `max |d_k| / 2`.
#[derive(Clone, Debug, PartialEq)]
pub struct Min<A, B> {
    a: A,
    b: B,
}

impl<A: Field, B: Field> Min<A, B> {
    /// `min(a, b)`.
    pub const fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A: Field, B: Field> Field for Min<A, B> {
    fn value(&self, x: i64, z: i64) -> i64 {
        self.a.value(x, z).min(self.b.value(x, z))
    }
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        self.a.grid(xs, zs, out);
        let mut b = vec![0; out.len()];
        self.b.grid(xs, zs, &mut b);
        for (v, b) in out.iter_mut().zip(b) {
            *v = (*v).min(b);
        }
    }
    fn range(&self) -> Span {
        let (a, b) = (self.a.range(), self.b.range());
        Span::new(a.lo.min(b.lo), a.hi.min(b.hi))
    }
    fn bound(&self, rect: Rect) -> Span {
        let (a, b) = (self.a.bound(rect), self.b.bound(rect));
        Span::new(a.lo.min(b.lo), a.hi.min(b.hi))
    }
    fn slope(&self) -> Slope {
        self.a.slope().max(self.b.slope())
    }
    fn fine(&self, rect: Rect) -> Fine {
        let (fa, fb) = (self.a.fine(rect), self.b.fine(rect));
        let (ca, cb) = (corner_values(&self.a, rect), corner_values(&self.b, rect));
        let (mut over, mut under, mut gap) = (0u64, 0u64, 0u64);
        for (a, b) in ca.into_iter().zip(cb) {
            let d = i128::from(a) - i128::from(b);
            let size = saturate(d.unsigned_abs());
            gap = gap.max(size);
            if d > 0 {
                over = over.max(size);
            } else {
                under = under.max(size);
            }
        }
        let up = fa
            .up
            .saturating_add(over)
            .min(fb.up.saturating_add(under))
            .min(fa.up.max(fb.up).saturating_add(gap.div_ceil(2)));
        Fine {
            up,
            down: fa.down.max(fb.down),
        }
    }
}

/// `floor((w a + (S - w) b) / S)`: `a` and `b` mixed by a weight field `w` in `[0, S]`, `S =
/// 2^shift` (all of `a` at `w = S`, all of `b` at 0).
///
/// Bounds, with `D` the largest gap `|a - b|` the ranges allow and `(Lw, Rw)` the weight's
/// slope:
/// - range and bound: the hull of `a`'s and `b`'s (only `a`'s where the weight is all `S`, only
///   `b`'s where it is 0);
/// - slope: `g(p) - g(q) = (w_p (a_p - a_q) + (S - w_p)(b_p - b_q) + (w_p - w_q)(a_q - b_q)) /
///   S`, so `max(La, Lb) + Lw D / S` per block, jitter `max(Ra, Rb) + Rw D / S + 1`;
/// - fine: the same split against the interpolants gives `max(fines) + T + 1`, where `T = D_r *
///   W_r / S` is the blend term: `D_r` the largest `|a - b|` over the rectangle and `W_r` how far
///   the weight moves there (at most its bound's width, and its slope over half the diagonal).
///   Over both axes that is `(D / L) * s` for a weight ramping across `L` blocks, not `s / 2`.
///   Where the weight is constant over the rectangle the term vanishes, so the margin stays
///   local.
#[derive(Clone, Debug, PartialEq)]
pub struct Blend<W, A, B> {
    weight: W,
    a: A,
    b: B,
    shift: u32,
    /// Every weighted sum fits in `i64`.
    narrow: bool,
    slope: Slope,
}

impl<W: Field, A: Field, B: Field> Blend<W, A, B> {
    /// The finest weight scale: `2^32`.
    pub const MAX_SHIFT: u32 = 32;

    /// `a` and `b` mixed by `weight` in `[0, 2^shift]`. Panics if the weight's range leaves
    /// that interval or `shift > MAX_SHIFT`.
    pub fn new(weight: W, a: A, b: B, shift: u32) -> Self {
        assert!(
            shift <= Self::MAX_SHIFT,
            "Blend: weights of 2^{shift} > 2^32"
        );
        let scale = 1i64 << shift;
        let wr = weight.range();
        assert!(
            wr.lo >= 0 && wr.hi <= scale,
            "Blend: the weight's range {wr:?} leaves 0..={scale}"
        );
        let (ra, rb) = (a.range(), b.range());
        let gap = gap(ra, rb);
        let (sa, sb, sw) = (a.slope(), b.slope(), weight.slope());
        let slope = Slope {
            per_block: sa
                .per_block
                .max(sb.per_block)
                .saturating_add(sw.per_block.scale(gap, scale as u64)),
            jitter: sa
                .jitter
                .max(sb.jitter)
                .saturating_add(mul_shift_ceil(sw.jitter, gap, shift))
                .saturating_add(1),
        };
        Self {
            // |w a + (S - w) b| <= S * max|v| < 2^62.
            narrow: ra.magnitude().max(rb.magnitude()) < 1 << (62 - shift),
            weight,
            a,
            b,
            shift,
            slope,
        }
    }

    /// The mix of `a` and `b` by `w`. Shared by the point and the grid paths.
    #[inline]
    fn mix(&self, w: i64, a: i64, b: i64) -> i64 {
        let s = 1i64 << self.shift;
        if self.narrow {
            (w * a + (s - w) * b) >> self.shift
        } else {
            let sum = i128::from(w) * i128::from(a) + i128::from(s - w) * i128::from(b);
            // Between a and b: it fits.
            (sum >> self.shift) as i64
        }
    }

    /// Which of `a` and `b` a weight bound reaches: `(a, b)`.
    fn reaches(&self, w: Span) -> (bool, bool) {
        let s = 1i64 << self.shift;
        (w.hi > 0, w.lo < s)
    }
}

/// The largest `|a - b|` for `a` in `ra` and `b` in `rb` (saturating).
fn gap(ra: Span, rb: Span) -> u64 {
    let d = (i128::from(ra.hi) - i128::from(rb.lo)).max(i128::from(rb.hi) - i128::from(ra.lo));
    saturate(d.max(0) as u128)
}

impl<W: Field, A: Field, B: Field> Field for Blend<W, A, B> {
    fn value(&self, x: i64, z: i64) -> i64 {
        self.mix(
            self.weight.value(x, z),
            self.a.value(x, z),
            self.b.value(x, z),
        )
    }
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        self.weight.grid(xs, zs, out);
        let (mut a, mut b) = (vec![0; out.len()], vec![0; out.len()]);
        self.a.grid(xs, zs, &mut a);
        self.b.grid(xs, zs, &mut b);
        for ((v, a), b) in out.iter_mut().zip(a).zip(b) {
            *v = self.mix(*v, a, b);
        }
    }
    fn range(&self) -> Span {
        match self.reaches(self.weight.range()) {
            (true, false) => self.a.range(),
            (false, true) => self.b.range(),
            _ => self.a.range().hull(self.b.range()),
        }
    }
    fn bound(&self, rect: Rect) -> Span {
        match self.reaches(self.weight.bound(rect)) {
            (true, false) => self.a.bound(rect),
            (false, true) => self.b.bound(rect),
            _ => self.a.bound(rect).hull(self.b.bound(rect)),
        }
    }
    fn slope(&self) -> Slope {
        self.slope
    }
    fn fine(&self, rect: Rect) -> Fine {
        let w = self.weight.bound(rect);
        match self.reaches(w) {
            // All of one side: exactly that field, no floor.
            (true, false) => return self.a.fine(rect),
            (false, true) => return self.b.fine(rect),
            _ => {}
        }
        let d = gap(self.a.bound(rect), self.b.bound(rect));
        let moved = w
            .width()
            .min(self.weight.slope().over(rect.half_diagonal()));
        let term = mul_shift_ceil(d, moved, self.shift);
        let (fa, fb) = (self.a.fine(rect), self.b.fine(rect));
        Fine {
            up: fa.up.max(fb.up),
            down: fa.down.max(fb.down),
        }
        .plus(term.saturating_add(1))
    }
}

/// `child(p + warp(p))`: a field sampled at warped points ([`Warp`] in whole blocks).
///
/// Bounds: range the child's; bound the child's over the rectangle grown by the warp's
/// amplitude; slope: the warped point moves by at most `(1 + sqrt(2) Lw) d + sqrt(2) Rw`
/// ([`Warp::displacement_slope`]), so `Lc (1 + sqrt(2) Lw)` per block and `Lc sqrt(2) Rw + Rc`
/// of jitter; fine from that slope ([`fine_from_slope`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Warped<C> {
    child: C,
    warp: Warp,
    slope: Slope,
}

impl<C: Field> Warped<C> {
    /// `child` sampled at `p + warp.offset(p)`; the warp's offsets are taken as blocks.
    pub fn new(warp: Warp, child: C) -> Self {
        let (c, w) = (child.slope(), warp.displacement_slope());
        let slope = Slope {
            per_block: c.per_block.mul_up(Q16::ONE.saturating_add(w.per_block)),
            jitter: c.per_block.times(w.jitter).saturating_add(c.jitter),
        };
        Self { child, warp, slope }
    }

    /// The child.
    pub const fn child(&self) -> &C {
        &self.child
    }

    /// The warp.
    pub const fn warp(&self) -> &Warp {
        &self.warp
    }
}

impl<C: Field> Field for Warped<C> {
    fn value(&self, x: i64, z: i64) -> i64 {
        let (dx, dz) = self.warp.offset(x, z);
        self.child.value(x + dx, z + dz)
    }
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        assert_eq!(out.len(), xs.len() * zs.len(), "Field::grid output size");
        if out.is_empty() {
            return;
        }
        let mut offsets = vec![(0, 0); out.len()];
        self.warp.offset_grid(xs, zs, &mut offsets);
        for ((row, &x), offsets) in out
            .chunks_exact_mut(zs.len())
            .zip(xs)
            .zip(offsets.chunks_exact(zs.len()))
        {
            for ((v, &z), &(dx, dz)) in row.iter_mut().zip(zs).zip(offsets) {
                *v = self.child.value(x + dx, z + dz);
            }
        }
    }
    fn range(&self) -> Span {
        self.child.range()
    }
    fn bound(&self, rect: Rect) -> Span {
        let a = self.warp.amplitude();
        self.child.bound(Rect::new(
            rect.x0 - a,
            rect.z0 - a,
            rect.x1 + a,
            rect.z1 + a,
        ))
    }
    fn slope(&self) -> Slope {
        self.slope
    }
    fn fine(&self, rect: Rect) -> Fine {
        fine_from_slope(self, rect)
    }
}
