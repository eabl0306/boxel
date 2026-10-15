//! 2D gradient noise ([`gradient_noise_2d`], [`gradient_fbm_2d`] and [`gradient_fbm_2d_grid`]),
//! an fbm cut for the far LOD ([`Fbm::split_at`], [`FbmSplit`]) and the Lipschitz bounds of the
//! two 2D bases ([`Basis`]).
//!
//! The value noise's contract: integer coordinates, an exact lattice split ([`Frequency`]),
//! lattice points hashed as [`hash2`](super::hash2) does at any `i64` cell (and past `i64` the
//! same way as the value noise, for octaves finer than a voxel), and `f64` only for the position
//! inside a cell, with `+ - * /` in a fixed order. The point and grid forms share their kernel, so
//! they agree bit for bit.

use super::d3::{fade, lerp};
use super::{
    Fbm, Frequency, Octave, Scale, cell_hash, fbm_2d, fbm_2d_grid, lattice_hash, normalize,
    splitmix64,
};
use crate::exact::Q16;

/// The 8 gradients of the 2D noise, picked by the low 3 bits of a lattice point's hash: the
/// diagonals `(+-1, +-1)` and the axes. With them `|noise| <= 1` (the clamp only removes
/// rounding), reached only at a cell center whose 4 gradients all point at it.
const GRADIENTS: [[f64; 2]; 8] = [
    [1.0, 1.0],
    [-1.0, 1.0],
    [1.0, -1.0],
    [-1.0, -1.0],
    [1.0, 0.0],
    [-1.0, 0.0],
    [0.0, 1.0],
    [0.0, -1.0],
];

/// Steepest slope of one octave of [`gradient_noise_2d`] per lattice cell, Euclidean, rounded up:
/// `sqrt(2) * 2.751`.
///
/// Per axis, `dn/dtx = (1 - F(tz)) dN/dtx + F(tz) dR/dtx` for the near and far rows `N` and `R`.
/// With `F` the fade and `A(t) = 1 - F(t) - t F'(t)`, the near row's slope is `g00x A(tx) + g10x
/// A(1 - tx) + F'(tx) tz (g10z - g00z)` and the far row's the same with `1 - tz`, so `|dn/dtx| <=
/// P(tx) = |A(tx)| + |A(1 - tx)| + F'(tx)` (the `tz` terms average to at most 1/2: `t + F(t) (1 -
/// 2t) <= 1/2`). `P` peaks at `t = 1/2` with `2.75`, reached by a real cell (a test samples it).
/// z is the same by symmetry, and the two axes give at most `sqrt(2)` times that.
const GRADIENT_SLOPE: f64 = 3.8906;

/// [`GRADIENT_SLOPE`] per axis, before the `sqrt(2)`.
#[cfg(test)]
const GRADIENT_AXIS_SLOPE: f64 = 2.751;

/// Steepest slope of one octave of the value noise per lattice cell, Euclidean, rounded up:
/// `sqrt(2) * 3`. The smoothstep moves the weight by at most `3 / 2` per cell between corners at
/// most 2 apart, on each axis.
const VALUE_SLOPE: f64 = 4.2427;

/// What [`FbmSplit::dropped`] adds for the `f64` rounding of the two sums it compares (each is
/// off by a few units in the last place, far under this for up to 64 octaves).
const ROUNDING: f64 = 1.0 / (1u64 << 40) as f64;

/// The 2D noise an [`Fbm`] sums.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Basis {
    /// Value noise ([`value_noise_2d`](super::value_noise_2d), [`fbm_2d`]).
    Value,
    /// Gradient noise ([`gradient_noise_2d`], [`gradient_fbm_2d`]).
    Gradient,
}

impl Basis {
    /// The fbm of this basis at `(x, z)`.
    pub fn fbm(self, seed: u64, x: i64, z: i64, fbm: Fbm) -> f64 {
        match self {
            Self::Value => fbm_2d(seed, x, z, fbm),
            Self::Gradient => gradient_fbm_2d(seed, x, z, fbm),
        }
    }

    /// The fbm of this basis over a grid, bit-identical to [`Basis::fbm`] per point (layout of
    /// [`fbm_2d_grid`]).
    pub fn fbm_grid(self, seed: u64, xs: &[i64], zs: &[i64], fbm: Fbm, out: &mut [f64]) {
        match self {
            Self::Value => fbm_2d_grid(seed, xs, zs, fbm, out),
            Self::Gradient => gradient_fbm_2d_grid(seed, xs, zs, fbm, out),
        }
    }

    /// Steepest slope of one octave per lattice cell (Euclidean, rounded up).
    fn per_cell(self) -> f64 {
        match self {
            Self::Value => VALUE_SLOPE,
            Self::Gradient => GRADIENT_SLOPE,
        }
    }
}

/// One coordinate on one octave's lattice.
#[derive(Clone, Copy, Debug)]
struct Axis {
    /// The cell: the low corner.
    cell: i128,
    /// Position inside the cell, `t` in `[0, 1)`: the offset from the low corner.
    near: f64,
    /// `t - 1`: the offset from the high corner.
    far: f64,
    /// [`fade`] of `t`: the weight of the high corner.
    fade: f64,
}

/// Salt of an octave's lattice phase along x, mixed into its seed.
const PHASE_X: u64 = 0x5048_4153_455f_0058;
/// Salt of an octave's lattice phase along z.
const PHASE_Z: u64 = 0x5048_4153_455f_005a;

/// The phase of an octave's lattice on one axis, in `[0, den)`: `floor(splitmix64(mixed_seed ^
/// salt) * den / 2^64)`, in units of `1 / den` of a cell.
#[inline]
fn phase(mixed_seed: u64, salt: u64, den: u32) -> u32 {
    ((u128::from(splitmix64(mixed_seed ^ salt)) * u128::from(den)) >> 64) as u32
}

/// `floor((c * num + q) / den)` and its remainder, exact: `c * num` is at most `2^127 - 2^63` in
/// magnitude and `q < 2^32`, so the sum fits in `i128` (the 64-bit path when it fits in `i64`).
#[inline]
fn split_shifted(s: Scale, c: i64, q: u32) -> (i128, u32) {
    let narrow = i64::try_from(s.num)
        .ok()
        .and_then(|num| c.checked_mul(num))
        .and_then(|p| p.checked_add(i64::from(q)));
    if let Some(p) = narrow {
        return match s.shift {
            Some(shift) => (i128::from(p >> shift), (p as u32) & ((1 << shift) - 1)),
            None => {
                let den = i64::from(s.den);
                (i128::from(p.div_euclid(den)), p.rem_euclid(den) as u32)
            }
        };
    }
    let p = i128::from(c) * i128::from(s.num) + i128::from(q);
    match s.shift {
        Some(shift) => (p >> shift, (p as u32) & ((1 << shift) - 1)),
        None => {
            let den = i128::from(s.den);
            (p.div_euclid(den), p.rem_euclid(den) as u32)
        }
    }
}

/// `c` on `scale`'s lattice shifted by the phase `q / den` of a cell: the exact cell and the
/// position inside it.
#[inline]
fn axis(scale: Scale, c: i64, q: u32) -> Axis {
    let (cell, rem) = split_shifted(scale, c, q);
    let t = scale.fraction(rem);
    Axis {
        cell,
        near: t,
        far: t - 1.0,
        fade: fade(t),
    }
}

/// Hashes `(h00, h10, h01, h11)` of the corners of cell `(x0, z0)` (`h10` is at `x0 + 1`), equal
/// to [`cell_hash`] of each. Always inlined: it is the inner step of every sample.
#[inline(always)]
fn corner_hashes(mixed_seed: u64, x0: i128, z0: i128) -> [u32; 4] {
    if let (Ok(x0), Ok(z0)) = (i32::try_from(x0), i32::try_from(z0))
        && x0 < i32::MAX
        && z0 < i32::MAX
    {
        // Every corner inside i32 (all cells near the origin): `hash2` without excess.
        let (x1, z1) = (x0 + 1, z0 + 1);
        return [
            lattice_hash(mixed_seed, x0, z0),
            lattice_hash(mixed_seed, x1, z0),
            lattice_hash(mixed_seed, x0, z1),
            lattice_hash(mixed_seed, x1, z1),
        ];
    }
    wide_corner_hashes(mixed_seed, x0, z0)
}

/// [`corner_hashes`] of a cell with a corner outside `i32`, corner by corner; kept out of line so
/// the common path stays small. `+ 1` cannot overflow: cells are below `2^127 - 2^63` in
/// magnitude ([`Scale::split`]).
#[inline(never)]
fn wide_corner_hashes(mixed_seed: u64, x0: i128, z0: i128) -> [u32; 4] {
    let (x1, z1) = (x0 + 1, z0 + 1);
    [
        cell_hash(mixed_seed, x0, z0),
        cell_hash(mixed_seed, x1, z0),
        cell_hash(mixed_seed, x0, z1),
        cell_hash(mixed_seed, x1, z1),
    ]
}

/// The gradient noise inside one cell from its corner hashes, clamped to `[-1, 1]` (only
/// rounding can pass it). Shared by the point and the grid paths so both give the same bits:
/// each corner's `g . (t - corner)`, interpolated in x with `fade(tx)`, then in z with
/// `fade(tz)`, `lerp(a, b, w) = a + (b - a) * w` throughout.
#[inline(always)]
fn kernel([h00, h10, h01, h11]: [u32; 4], x: &Axis, z: &Axis) -> f64 {
    let dot = |h: u32, dx: f64, dz: f64| {
        let [gx, gz] = GRADIENTS[(h & 7) as usize];
        gx * dx + gz * dz
    };
    let near = lerp(dot(h00, x.near, z.near), dot(h10, x.far, z.near), x.fade);
    let far = lerp(dot(h01, x.near, z.far), dot(h11, x.far, z.far), x.fade);
    lerp(near, far, z.fade).clamp(-1.0, 1.0)
}

impl Octave {
    /// The phases `(qx, qz)` of this octave's lattice ([`phase`]).
    #[inline]
    fn phases(self) -> (u32, u32) {
        let den = self.scale.den;
        (
            phase(self.mixed_seed, PHASE_X, den),
            phase(self.mixed_seed, PHASE_Z, den),
        )
    }

    /// The octave's gradient noise at `(x, z)` (without the amplitude).
    #[inline]
    fn gradient(self, x: i64, z: i64) -> f64 {
        let (qx, qz) = self.phases();
        let (x, z) = (axis(self.scale, x, qx), axis(self.scale, z, qz));
        kernel(corner_hashes(self.mixed_seed, x.cell, z.cell), &x, &z)
    }
}

/// 2D gradient noise (Perlin's, with a quintic fade) in `[-1, 1]` at integer `(x, z)`, with
/// `frequency = num / den` lattice cells per voxel, on a lattice shifted by a phase hashed from
/// the seed.
///
/// The phase is `(qx, qz) / den` of a cell, `q = floor(splitmix64(splitmix64(seed) ^ salt) * den
/// / 2^64)` with one salt per axis, so the cell of `x` is `floor((x * num + qx) / den)`, exact,
/// and `t = ((x * num + qx) mod den) / den` its position inside the cell, in `[0, 1)`. Lattice
/// point `c` has the gradient `G[hash2(seed, c) & 7]` (for cells inside `i64`; past it, the value
/// noise's extension of `hash2`), one of `(+-1, +-1)`, `(+-1, 0)` and `(0, +-1)`. The noise is
/// `lerp(lerp(d00, d10, fade(tx)), lerp(d01, d11, fade(tx)), fade(tz))`, `dij` the dot product
/// of corner `ij`'s gradient with the offset from that corner, `fade(t) = 6t^5 - 15t^4 + 10t^3`,
/// clamped to `[-1, 1]`.
///
/// Gradient noise is 0 on its own lattice points. Without the phase every octave of an fbm with
/// an integer lacunarity would hold octave 0's lattice, so the fbm would be pinned to 0 on it
/// (on a 131 km grid for a 2^17 wavelength); with it, each octave's zeros sit at its own hashed
/// offset. With an integer frequency (`den = 1`) every sample is a lattice point and the noise is
/// 0 everywhere.
pub fn gradient_noise_2d(seed: u64, x: i64, z: i64, frequency: Frequency) -> f64 {
    let octave = Octave {
        mixed_seed: splitmix64(seed),
        scale: Scale::new(frequency.num.into(), frequency.den),
        amplitude: 1.0,
    };
    octave.gradient(x, z)
}

/// Fractal Brownian motion of [`gradient_noise_2d`] in `[-1, 1]` at integer `(x, z)`: the octaves
/// of [`fbm_2d`] (octave `k` samples `gradient_noise_2d(seed + k, x, z, frequency *
/// lacunarity^k)` with weight `gain^k`), divided by the sum of the absolute weights. One octave
/// equals [`gradient_noise_2d`]; zero octaves give 0.
pub fn gradient_fbm_2d(seed: u64, x: i64, z: i64, fbm: Fbm) -> f64 {
    let mut sum = 0.0;
    let mut weight_sum = 0.0;
    for octave in fbm.octaves(seed) {
        sum += octave.amplitude * octave.gradient(x, z);
        weight_sum += f64::abs(octave.amplitude);
    }
    normalize(sum, weight_sum)
}

/// [`gradient_fbm_2d`] at every point of the grid `xs` x `zs` (`out[i * zs.len() + j]` gets the
/// value at `(xs[i], zs[j])`), bit-identical to calling [`gradient_fbm_2d`] per point. Each
/// octave splits each coordinate once and hashes every lattice corner the grid touches once; a
/// grid too sparse for its lattice is sampled point by point.
///
/// # Panics
///
/// If `out.len()` is not `xs.len() * zs.len()`.
pub fn gradient_fbm_2d_grid(seed: u64, xs: &[i64], zs: &[i64], fbm: Fbm, out: &mut [f64]) {
    assert_eq!(
        out.len(),
        xs.len() * zs.len(),
        "gradient_fbm_2d_grid output size"
    );
    if out.is_empty() {
        return;
    }
    out.fill(0.0);
    let mut table = HashTable::default();
    let mut x_axes = Vec::with_capacity(xs.len());
    let mut z_axes = Vec::with_capacity(zs.len());
    let points = out.len();
    let mut weight_sum = 0.0;
    for octave in fbm.octaves(seed) {
        let (qx, qz) = octave.phases();
        x_axes.clear();
        x_axes.extend(xs.iter().map(|&x| axis(octave.scale, x, qx)));
        z_axes.clear();
        z_axes.extend(zs.iter().map(|&z| axis(octave.scale, z, qz)));
        let amplitude = octave.amplitude;
        let rows = out.chunks_exact_mut(zs.len()).zip(&x_axes);
        if table.rebuild(octave.mixed_seed, &x_axes, &z_axes, points) {
            let (h, d) = (&table.hashes, table.depth);
            for ((row, x), &x_row) in rows.zip(&table.rows) {
                for ((sum, z), &z_col) in row.iter_mut().zip(&z_axes).zip(&table.cols) {
                    let i = x_row + z_col;
                    let corners = [h[i], h[i + d], h[i + 1], h[i + d + 1]];
                    *sum += amplitude * kernel(corners, x, z);
                }
            }
        } else {
            for (row, x) in rows {
                for (sum, z) in row.iter_mut().zip(&z_axes) {
                    let corners = corner_hashes(octave.mixed_seed, x.cell, z.cell);
                    *sum += amplitude * kernel(corners, x, z);
                }
            }
        }
        weight_sum += f64::abs(amplitude);
    }
    for v in out.iter_mut() {
        *v = normalize(*v, weight_sum);
    }
}

/// Lattice hashes of one octave over every corner a grid touches (the layout of the value
/// noise's `LatticeTable`).
#[derive(Default)]
struct HashTable {
    /// Row-major `[x][z]` hashes over the corners `x_min..=x_max + 1` by `z_min..=z_max + 1`.
    hashes: Vec<u32>,
    /// Corners along z per x row.
    depth: usize,
    /// Per grid x: start of its cell's row in `hashes`.
    rows: Vec<usize>,
    /// Per grid z: offset of its cell inside a row.
    cols: Vec<usize>,
}

impl HashTable {
    /// Hashes every corner of the `xs` x `zs` cells once. False, with nothing hashed, when the
    /// grid is so sparse (high frequency) that the table would cost more than hashing per point.
    fn rebuild(&mut self, mixed_seed: u64, xs: &[Axis], zs: &[Axis], points: usize) -> bool {
        let span = |axes: &[Axis]| {
            axes.iter().fold((i128::MAX, i128::MIN), |(lo, hi), a| {
                (lo.min(a.cell), hi.max(a.cell))
            })
        };
        let ((x_min, x_max), (z_min, z_max)) = (span(xs), span(zs));
        let width = x_max.abs_diff(x_min).saturating_add(2);
        let depth = z_max.abs_diff(z_min).saturating_add(2);
        if width.saturating_mul(depth) > 4 * points as u128 {
            return false;
        }
        // Both spans are at most `4 * points`, so every offset below fits in usize.
        let depth = depth as usize;
        self.depth = depth;
        self.hashes.clear();
        for x in x_min..=x_max + 1 {
            for z in z_min..=z_max + 1 {
                self.hashes.push(cell_hash(mixed_seed, x, z));
            }
        }
        self.rows.clear();
        self.rows
            .extend(xs.iter().map(|a| (a.cell - x_min) as usize * depth));
        self.cols.clear();
        self.cols
            .extend(zs.iter().map(|a| (a.cell - z_min) as usize));
        true
    }
}

/// An [`Fbm`] cut in two for the far LOD ([`Fbm::split_at`]): the long octaves it keeps and a
/// bound of what the short ones it drops can add.
///
/// The kept part is the first octaves' share of the full sum: [`FbmSplit::sample`] is the fbm of
/// the kept octaves times their share of the weight, `kept weight / full weight`, so it equals the
/// full fbm's first terms over the full weight (up to rounding), and the full fbm is never
/// farther from it than [`FbmSplit::dropped`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FbmSplit {
    /// The whole fbm.
    fbm: Fbm,
    /// Its kept octaves, as an fbm of their own.
    kept: Fbm,
    /// Sum of the absolute weights of every octave.
    weight: f64,
    /// Kept weight over [`FbmSplit::weight`]: 1 when nothing is dropped.
    share: f64,
    /// Bound of `|full - kept|` everywhere: 0 when nothing is dropped.
    dropped: f64,
}

impl Fbm {
    /// Splits the fbm before its first octave with a wavelength (voxels per lattice cell,
    /// `den / (num * lacunarity^k)`) under `min_wavelength`: that octave and every later one are
    /// dropped. A level-`k` LOD cell of `s = 2^k` voxels drops the octaves under `2s`
    /// (`split_at(2 << k)`). `split_at(0)` keeps everything.
    pub fn split_at(self, min_wavelength: u64) -> FbmSplit {
        let (mut kept, mut count, mut prefix) = (0, 0, true);
        // The weights in octave order, as `fbm_2d` sums them.
        let (mut weight, mut kept_weight, mut dropped_weight) = (0.0, 0.0, 0.0);
        for octave in self.octaves(0) {
            let a = f64::abs(octave.amplitude);
            prefix &= u128::from(octave.scale.den)
                >= u128::from(min_wavelength) * u128::from(octave.scale.num);
            if prefix {
                kept += 1;
                kept_weight += a;
            } else {
                dropped_weight += a;
            }
            weight += a;
            count += 1;
        }
        let all = kept == count;
        FbmSplit {
            fbm: self,
            kept: if all {
                self
            } else {
                Fbm {
                    octaves: kept,
                    ..self
                }
            },
            weight,
            share: if all {
                1.0
            } else {
                normalize(kept_weight, weight)
            },
            dropped: if all {
                0.0
            } else {
                normalize(dropped_weight, weight) + ROUNDING
            },
        }
    }

    /// Lipschitz bound of the fbm of `basis` (`|f(p) - f(q)| <= L |p - q|`, Euclidean, for
    /// integer points), in units per voxel: [`FbmSplit::lipschitz`] of the whole fbm. It bounds
    /// the fbm as a real function; its `f64` evaluation adds a rounding far under `2^-40`.
    pub fn lipschitz(self, basis: Basis) -> Q16 {
        self.split_at(0).lipschitz(basis)
    }
}

impl FbmSplit {
    /// The whole fbm.
    pub const fn fbm(&self) -> Fbm {
        self.fbm
    }

    /// The kept octaves as an fbm of their own, normalized by their own weight (the whole fbm
    /// when nothing is dropped).
    pub const fn kept(&self) -> Fbm {
        self.kept
    }

    /// The kept octaves' share of the full weight, in `[0, 1]`: 1 exactly when nothing is
    /// dropped.
    pub const fn share(&self) -> f64 {
        self.share
    }

    /// Bound of `|full - kept|` at every point, for either basis: the dropped octaves' share of
    /// the full weight, plus `2^-40` for the two sums' `f64` rounding. 0 exactly when nothing is
    /// dropped.
    pub const fn dropped(&self) -> f64 {
        self.dropped
    }

    /// The kept part at `(x, z)`: `basis.fbm(seed, x, z, kept()) * share()`. With nothing dropped
    /// it is the full fbm, bit for bit.
    pub fn sample(&self, basis: Basis, seed: u64, x: i64, z: i64) -> f64 {
        basis.fbm(seed, x, z, self.kept) * self.share
    }

    /// [`FbmSplit::sample`] over a grid (layout of [`fbm_2d_grid`]), bit-identical per point.
    pub fn grid(&self, basis: Basis, seed: u64, xs: &[i64], zs: &[i64], out: &mut [f64]) {
        basis.fbm_grid(seed, xs, zs, self.kept, out);
        for v in out.iter_mut() {
            *v *= self.share;
        }
    }

    /// Lipschitz bound of [`FbmSplit::sample`] (`|f(p) - f(q)| <= L |p - q|`, Euclidean, for
    /// integer points), in units per voxel, rounded up.
    ///
    /// Octave `k` moves by at most `C f_k` per voxel (`f_k` its cells per voxel, `C` the basis's
    /// slope per cell), and never by more than 2 (its range), which is at most `2 |p - q|` for two
    /// distinct integer points; the kept octaves' weighted sum of `min(C f_k, 2)`, over the full
    /// weight. Computed in `f64` and padded by `2^-30` relative for its own rounding, so it stays
    /// an upper bound.
    pub fn lipschitz(&self, basis: Basis) -> Q16 {
        let c = basis.per_cell();
        let mut sum = 0.0;
        for octave in self.kept.octaves(0) {
            let f = octave.scale.num as f64 / f64::from(octave.scale.den);
            sum += f64::abs(octave.amplitude) * f64::min(c * f, 2.0);
        }
        Q16::from_f64(normalize(sum, self.weight) * (1.0 + 1.0 / (1u64 << 30) as f64))
    }
}

#[cfg(test)]
mod tests;
