//! The exact integer blend at kind borders (ADR 0039 sec. 5).
//!
//! A column `p` lies in one cell of the region lattice (spacing `L = 2^shift`), with corners
//! `q00`, `q10`, `q01` and `q11`. `n_k(q)` counts kind `k` in the 3 x 3 box of lattice points
//! around `q`, so the 4 boxes read the 4 x 4 window of lattice points around the cell
//! ([`Window`]). With `fx = x & (L - 1)` and `fz = z & (L - 1)`:
//!
//! ```text
//! c_k(p) = n_k(q00) (L - fx)(L - fz) + n_k(q10) fx (L - fz) + n_k(q01) (L - fx) fz + n_k(q11) fx fz
//! sum_k c_k(p) = 9 L^2                        (exact, no rounding)
//! H(p) = floor(sum_k c_k(p) H_k(p) / (9 L^2))
//! ```
//!
//! `|sum_k c_k H_k| <= 9 L^2 2^31`, under `2^59` for the largest lattice
//! ([`MAX_LATTICE_SHIFT`](super::MAX_LATTICE_SHIFT)): `i64` holds it with any number of kinds. The
//! same formula serves a point, a chunk grid and a LOD corner, so they agree bit for bit.

use super::HeightQ;

/// Lattice points in the box around a lattice point: `(2r + 1)^2` with `r = 1`.
pub const BOX: i64 = 9;

/// The lattice points of a [`Window`].
pub const WINDOW_POINTS: usize = 16;

/// The window points at the cell's corners `q00`, `q10`, `q01` and `q11`, as `(a, b)`.
const CORNERS: [(usize, usize); 4] = [(1, 1), (2, 1), (1, 2), (2, 2)];

/// The counts of one kind in the boxes of the 4 corners: `[n00, n10, n01, n11]`, each in `0..=9`.
pub type Counts = [i64; 4];

/// The kinds of the 4 x 4 lattice points around a lattice cell `(i, j)`: point `a * 4 + b` is
/// lattice point `(i - 1 + a, j - 1 + b)`. A kind is its index among the realm's kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Window(pub [u32; WINDOW_POINTS]);

impl Window {
    /// The kind of every point, if they are all one: the interior fast path.
    pub fn single(&self) -> Option<u32> {
        let first = self.0[0];
        self.0.iter().all(|&k| k == first).then_some(first)
    }

    /// The counts of `kind` in the boxes of the 4 corners.
    pub fn counts(&self, kind: u32) -> Counts {
        CORNERS.map(|(ca, cb)| {
            let mut n = 0;
            for a in ca - 1..=ca + 1 {
                for b in cb - 1..=cb + 1 {
                    n += i64::from(self.0[a * 4 + b] == kind);
                }
            }
            n
        })
    }

    /// The kinds present, each once with its counts, by kind index (so the realm's order).
    /// Every point lies in some corner's box, so each has a count above 0 somewhere.
    pub fn present(&self) -> Vec<(u32, Counts)> {
        let mut kinds: Vec<u32> = self.0.to_vec();
        kinds.sort_unstable();
        kinds.dedup();
        kinds.into_iter().map(|k| (k, self.counts(k))).collect()
    }
}

/// The bilinear factors of a column at `(fx, fz)` inside its cell of `2^shift` blocks:
/// `[(L - fx)(L - fz), fx (L - fz), (L - fx) fz, fx fz]`, summing to `L^2`.
#[inline]
pub const fn factors(fx: i64, fz: i64, shift: u32) -> [i64; 4] {
    let l = 1 << shift;
    let (gx, gz) = (l - fx, l - fz);
    [gx * gz, fx * gz, gx * fz, fx * fz]
}

/// `c_k`: a kind's counts weighted by a column's [`factors`].
#[inline]
pub const fn weight(n: Counts, f: [i64; 4]) -> i64 {
    n[0] * f[0] + n[1] * f[1] + n[2] * f[2] + n[3] * f[3]
}

/// The sum of every kind's weight at any column: `9 L^2`.
#[inline]
pub const fn total(shift: u32) -> i64 {
    BOX << (2 * shift)
}

/// The blended height of `sum = sum_k c_k H_k`: `floor(sum / (9 L^2))`, as `floor(floor(sum /
/// L^2) / 9)` (the same for a positive divisor): a shift and a division by a constant.
#[inline]
pub const fn blend(sum: i64, shift: u32) -> HeightQ {
    // A convex combination of `HeightQ`s, floored: it fits.
    (sum >> (2 * shift)).div_euclid(BOX) as HeightQ
}

/// The dominant kind of a column (the biome table's input, ADR 0039 sec. 9): the largest
/// `c_k`, ties to the kind first in `present` (the smaller index).
pub fn dominant(present: &[(u32, Counts)], f: [i64; 4]) -> u32 {
    let mut best = (i64::MIN, u32::MAX);
    for &(kind, n) in present {
        let c = weight(n, f);
        if c > best.0 {
            best = (c, kind);
        }
    }
    best.1
}
