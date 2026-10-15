//! The leaves of a field: [`Const`] and [`NoiseField`] (a quantized fbm).

use super::{CellGrid, Cells, Coarsen, Field, Fine, Rect, Slope, Span, slope_cells};
use crate::exact::{Q16, round_f64};
use crate::noise::{Basis, Fbm, FbmSplit};

/// The same value everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Const(pub i64);

impl Field for Const {
    fn value(&self, _x: i64, _z: i64) -> i64 {
        self.0
    }
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        assert_eq!(out.len(), xs.len() * zs.len(), "Field::grid output size");
        out.fill(self.0);
    }
    fn range(&self) -> Span {
        Span::point(self.0)
    }
    fn slope(&self) -> Slope {
        Slope::ZERO
    }
    fn fine(&self, _rect: Rect) -> Fine {
        Fine::ZERO
    }
    fn cells(&self, grid: &CellGrid) -> Cells {
        Cells {
            corners: vec![self.0; (grid.nx + 1) * (grid.nz + 1)],
            bounds: vec![Span::point(self.0); grid.len()],
            fines: vec![Fine::ZERO; grid.len()],
        }
    }
}

/// An fbm of [`Basis`] noise quantized to integers: `round(amplitude * fbm)`, halves away from 0,
/// in `[-amplitude, amplitude]`.
///
/// Its slope is `amplitude` times the fbm's Lipschitz bound ([`FbmSplit::lipschitz`]), with a
/// jitter of 2: 1 for the rounding to an integer and 1 for the `f64` rounding of the fbm, far
/// under a unit while `amplitude <= 2^40`. Its bound over a rectangle is the trait's default,
/// local: the value at the center plus and minus that slope over the distance to the farthest
/// corner ([`bound_from_slope`](super::bound_from_slope)).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseField {
    seed: u64,
    split: FbmSplit,
    basis: Basis,
    amplitude: i64,
    slope: Slope,
}

impl NoiseField {
    /// The largest amplitude.
    pub const MAX_AMPLITUDE: i64 = 1 << 40;

    /// `round(amplitude * basis.fbm(seed, x, z, fbm))`. Panics if `amplitude` is not in
    /// `0..=MAX_AMPLITUDE`.
    pub fn new(seed: u64, fbm: Fbm, basis: Basis, amplitude: i64) -> Self {
        Self::from_split(seed, fbm.split_at(0), basis, amplitude)
    }

    fn from_split(seed: u64, split: FbmSplit, basis: Basis, amplitude: i64) -> Self {
        assert!(
            (0..=Self::MAX_AMPLITUDE).contains(&amplitude),
            "NoiseField: amplitude {amplitude} out of 0..=2^40"
        );
        let per_block = Q16::from_int(amplitude as u64).mul_up(split.lipschitz(basis));
        Self {
            seed,
            split,
            basis,
            amplitude,
            slope: Slope {
                per_block,
                jitter: 2,
            },
        }
    }

    /// This field with the octaves under `min_wavelength` dropped too: the octaves
    /// [`Fbm::split_at`] keeps at `max(min_wavelength, cut)` (in the full fbm's scale), `cut` the
    /// wavelength this field was already split at (0 for a whole one), so splitting a coarse
    /// field again never brings octaves back. With it, a bound of how far this field ever is
    /// from the result: `ceil(amplitude * distance) + 1` ([`FbmSplit::distance`], the 1 for the
    /// two roundings to integers), 0 when the same octaves are kept.
    pub fn split_at(&self, min_wavelength: u64) -> (Self, u64) {
        let split = self
            .split
            .fbm()
            .split_at(min_wavelength.max(self.split.cut()));
        let coarse = Self::from_split(self.seed, split, self.basis, self.amplitude);
        let distance = split.distance(&self.split);
        if distance == 0.0 {
            // The same octaves: the same values, bit for bit.
            return (coarse, 0);
        }
        let dropped = Q16::from_f64(distance).times(self.amplitude as u64);
        (coarse, dropped.saturating_add(1))
    }

    /// The noise's seed.
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// The fbm (whole, as given to [`NoiseField::new`]).
    pub const fn fbm(&self) -> Fbm {
        self.split.fbm()
    }

    /// The octaves this field sums (all of them unless it came from [`NoiseField::split_at`]).
    pub const fn kept(&self) -> Fbm {
        self.split.kept()
    }

    /// The basis.
    pub const fn basis(&self) -> Basis {
        self.basis
    }

    /// The largest magnitude.
    pub const fn amplitude(&self) -> i64 {
        self.amplitude
    }

    /// `f64` noise to the field's integers.
    #[inline]
    fn quantize(&self, v: f64) -> i64 {
        round_f64(self.amplitude as f64 * v)
    }
}

impl Field for NoiseField {
    fn value(&self, x: i64, z: i64) -> i64 {
        self.quantize(self.split.sample(self.basis, self.seed, x, z))
    }
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        assert_eq!(out.len(), xs.len() * zs.len(), "Field::grid output size");
        let mut noise = vec![0.0; out.len()];
        self.split.grid(self.basis, self.seed, xs, zs, &mut noise);
        for (v, &n) in out.iter_mut().zip(&noise) {
            *v = self.quantize(n);
        }
    }
    fn range(&self) -> Span {
        Span::new(-self.amplitude, self.amplitude)
    }
    fn slope(&self) -> Slope {
        self.slope
    }
    /// [`slope_cells`]: the per-cell defaults from a corner grid and a center grid.
    fn cells(&self, grid: &CellGrid) -> Cells {
        slope_cells(self, grid)
    }
}

impl Coarsen for Const {
    type Coarse = Self;

    fn coarse(&self, _min_wavelength: u64) -> (Self, u64) {
        (*self, 0)
    }
}

impl Coarsen for NoiseField {
    type Coarse = Self;

    /// [`NoiseField::split_at`].
    fn coarse(&self, min_wavelength: u64) -> (Self, u64) {
        self.split_at(min_wavelength)
    }
}
