//! An integer domain warp ([`Warp`]): two gradient fbms give a point an offset, rounded to whole
//! units, with a stated Lipschitz bound. The unit is the caller's: blocks, or a fixed point such
//! as 1/16 of a block for river distances (the amplitude is given in that unit).

use super::{Basis, Fbm, gradient_fbm_2d, gradient_fbm_2d_grid, mix_seed};
use crate::exact::{Q16, Slope, round_f64};

/// Salt of the x offset's fbm.
const SALT_X: u64 = 0x5741_5250_5f58_0001;
/// Salt of the z offset's fbm.
const SALT_Z: u64 = 0x5741_5250_5f5a_0002;

/// A domain warp: the offset of `(x, z)` is `(round(A * gx), round(A * gz))`, `gx` and `gz` two
/// [`gradient_fbm_2d`] with the same [`Fbm`] and seeds mixed from one seed with two salts
/// ([`mix_seed`]), `A` the amplitude, halves rounded away from 0. Each component is an integer
/// in `[-A, A]`.
///
/// Bounds, for integer points `p` and `q` at Euclidean distance `d`:
/// - each component: `|o(p) - o(q)| <= A L d + 2` ([`Warp::component_slope`]), `L` the fbm's
///   Lipschitz bound ([`Fbm::lipschitz`]); 1 of the 2 is the rounding to an integer and 1 covers
///   the `f64` rounding of the fbm (far under it while `A <= 2^40`);
/// - the offset as a vector: `|o(p) - o(q)| <= sqrt(2) (A L d + 2)`
///   ([`Warp::displacement_slope`]);
/// - the warped point, with offsets in `1 / u` blocks: `|(u p + o(p)) - (u q + o(q))|` is at
///   most `(u + sqrt(2) A L) d + 2 sqrt(2)`. With `sqrt(2) A L < u` the map keeps its
///   orientation and stays one to one up to the rounding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Warp {
    /// The two fbm seeds, x then z.
    seeds: [u64; 2],
    fbm: Fbm,
    amplitude: i64,
    /// Bound of each component.
    slope: Slope,
}

impl Warp {
    /// The largest amplitude: the `f64` rounding of the fbm then stays far under one unit.
    pub const MAX_AMPLITUDE: i64 = 1 << 40;

    /// A warp of `amplitude` units from the gradient fbm `fbm`. Panics if `amplitude` is not in
    /// `0..=MAX_AMPLITUDE`.
    pub fn new(seed: u64, fbm: Fbm, amplitude: i64) -> Self {
        assert!(
            (0..=Self::MAX_AMPLITUDE).contains(&amplitude),
            "Warp: amplitude {amplitude} out of 0..=2^40"
        );
        let per_block = Q16::from_int(amplitude as u64).mul_up(fbm.lipschitz(Basis::Gradient));
        Self {
            seeds: [mix_seed(seed, SALT_X), mix_seed(seed, SALT_Z)],
            fbm,
            amplitude,
            slope: Slope {
                per_block,
                jitter: 2,
            },
        }
    }

    /// The fbm seeds of the x and z components (their octaves use `seed + k`).
    pub const fn seeds(&self) -> [u64; 2] {
        self.seeds
    }

    /// The fbm of both components.
    pub const fn fbm(&self) -> Fbm {
        self.fbm
    }

    /// The largest offset on either axis.
    pub const fn amplitude(&self) -> i64 {
        self.amplitude
    }

    /// The offset of `(x, z)`, `(dx, dz)`, each in `[-amplitude, amplitude]`.
    pub fn offset(&self, x: i64, z: i64) -> (i64, i64) {
        let a = self.amplitude as f64;
        let component = |seed: u64| round_f64(a * gradient_fbm_2d(seed, x, z, self.fbm));
        (component(self.seeds[0]), component(self.seeds[1]))
    }

    /// [`Warp::offset`] at every point of the grid `xs` x `zs` (`out[i * zs.len() + j]` is the
    /// offset of `(xs[i], zs[j])`), bit-identical per point.
    ///
    /// # Panics
    ///
    /// If `out.len()` is not `xs.len() * zs.len()`.
    pub fn offset_grid(&self, xs: &[i64], zs: &[i64], out: &mut [(i64, i64)]) {
        assert_eq!(
            out.len(),
            xs.len() * zs.len(),
            "Warp::offset_grid output size"
        );
        let a = self.amplitude as f64;
        let (mut gx, mut gz) = (vec![0.0; out.len()], vec![0.0; out.len()]);
        gradient_fbm_2d_grid(self.seeds[0], xs, zs, self.fbm, &mut gx);
        gradient_fbm_2d_grid(self.seeds[1], xs, zs, self.fbm, &mut gz);
        for ((o, &x), &z) in out.iter_mut().zip(&gx).zip(&gz) {
            *o = (round_f64(a * x), round_f64(a * z));
        }
    }

    /// Bound of each offset component: `|o(p) - o(q)| <= per_block * |p - q| + jitter`, in the
    /// offset's unit per block.
    pub const fn component_slope(&self) -> Slope {
        self.slope
    }

    /// Bound of the offset as a vector, `|o(p) - o(q)|` (Euclidean): `sqrt(2)` times
    /// [`Warp::component_slope`], rounded up.
    pub fn displacement_slope(&self) -> Slope {
        Slope {
            per_block: Q16::SQRT_2.mul_up(self.slope.per_block),
            jitter: Q16::SQRT_2.times(self.slope.jitter),
        }
    }
}

#[cfg(test)]
mod tests;
