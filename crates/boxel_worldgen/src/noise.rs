//! Deterministic value noise (no dependencies). Same seed + coordinates = same value on every
//! platform (integer hashing + f64 math).
//!
//! The seed goes through `SplitMix64` before it is combined with the lattice coordinates, so
//! nearby seeds (`fbm_2d` uses `seed + octave` per octave) give unrelated lattices.

/// `SplitMix64` output function (Steele, Lea and Flood 2014): a bijective 64-bit mix.
#[inline]
const fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// [`hash2`] with the seed already mixed (`splitmix64(seed)`).
#[inline]
const fn lattice_hash(mixed_seed: u64, x: i32, z: i32) -> u32 {
    // Injective in (x, z): x in the high half, z in the low half (two's complement bits).
    let key = ((x as u32 as u64) << 32) | (z as u32 as u64);
    splitmix64(mixed_seed ^ key) as u32
}

/// Lattice value in `[-1, 1]`.
#[inline]
fn lattice_value(mixed_seed: u64, x: i32, z: i32) -> f64 {
    f64::from(lattice_hash(mixed_seed, x, z)) / f64::from(u32::MAX) * 2.0 - 1.0
}

/// Cubic smoothstep `t * t * (3 - 2t)` for `t` in `[0, 1]`.
#[inline]
fn smoothstep(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// Lattice cell (floor) of a coordinate and the smoothstep weight inside it.
#[inline]
fn cell(c: f64) -> (i32, f64) {
    let f = c.floor();
    // Saturating cast: coordinates beyond i32 clamp to the last lattice cell (NaN maps to 0).
    (f as i32, smoothstep(c - f))
}

/// Corner values `(v00, v10, v01, v11)` of cell `(x0, z0)` (`v10` is at `x0 + 1`, wrapping).
#[inline]
fn corners(mixed_seed: u64, x0: i32, z0: i32) -> [f64; 4] {
    let (x1, z1) = (x0.wrapping_add(1), z0.wrapping_add(1));
    [
        lattice_value(mixed_seed, x0, z0),
        lattice_value(mixed_seed, x1, z0),
        lattice_value(mixed_seed, x0, z1),
        lattice_value(mixed_seed, x1, z1),
    ]
}

/// Bilinear interpolation of the corners with smoothstep weights. Shared by the point and the
/// grid paths so both produce bit-identical values.
#[inline]
fn interpolate([v00, v10, v01, v11]: [f64; 4], tx: f64, tz: f64) -> f64 {
    let near = v00 + (v10 - v00) * tx;
    let far = v01 + (v11 - v01) * tx;
    // The clamp only removes rounding overshoot of the interpolation.
    (near + (far - near) * tz).clamp(-1.0, 1.0)
}

/// [`value_noise_2d`] with the seed already mixed.
#[inline]
fn value_noise_mixed(mixed_seed: u64, x: f64, z: f64) -> f64 {
    let (x0, tx) = cell(x);
    let (z0, tz) = cell(z);
    interpolate(corners(mixed_seed, x0, z0), tx, tz)
}

/// 32-bit hash of `(seed, x, z)` (e.g. a SplitMix64/xxhash-style mix).
///
/// `SplitMix64(SplitMix64(seed) ^ (x << 32 | z))` truncated to the low 32 bits, with `x` and `z`
/// taken as their `u32` bit patterns.
pub fn hash2(seed: u64, x: i32, z: i32) -> u32 {
    lattice_hash(splitmix64(seed), x, z)
}

/// 2D value noise in `[-1, 1]`: smoothstep-interpolated lattice values.
///
/// Lattice values are `hash2(seed, x, z) / u32::MAX * 2 - 1` at integer corners, so the noise
/// equals that value at integer coordinates.
pub fn value_noise_2d(seed: u64, x: f64, z: f64) -> f64 {
    value_noise_mixed(splitmix64(seed), x, z)
}

/// Fractal Brownian motion of [`value_noise_2d`] in roughly `[-1, 1]`.
///
/// Octave `i` (from 0) samples `value_noise_2d(seed + i, x * f, z * f)` with `f = lacunarity^i`
/// and weight `gain^i`; the sum is divided by the sum of the absolute weights, so the result
/// stays in `[-1, 1]`. One octave equals [`value_noise_2d`]; zero octaves give 0.
pub fn fbm_2d(seed: u64, x: f64, z: f64, octaves: u32, lacunarity: f64, gain: f64) -> f64 {
    let mut sum = 0.0;
    let mut weight_sum = 0.0;
    let mut frequency = 1.0;
    let mut amplitude = 1.0;
    for octave in 0..octaves {
        let mixed_seed = splitmix64(seed.wrapping_add(u64::from(octave)));
        sum += amplitude * value_noise_mixed(mixed_seed, x * frequency, z * frequency);
        weight_sum += f64::abs(amplitude);
        frequency *= lacunarity;
        amplitude *= gain;
    }
    if weight_sum > 0.0 {
        sum / weight_sum
    } else {
        0.0
    }
}

/// [`fbm_2d`] at every point of the grid `xs` x `zs` (`out[i * zs.len() + j]` gets the value at
/// `(xs[i], zs[j])`), bit-identical to calling [`fbm_2d`] per point. Each octave hashes every
/// lattice corner the grid touches once instead of four times per point.
pub(crate) fn fbm_2d_grid(
    seed: u64,
    xs: &[f64],
    zs: &[f64],
    octaves: u32,
    lacunarity: f64,
    gain: f64,
    out: &mut [f64],
) {
    assert_eq!(out.len(), xs.len() * zs.len(), "fbm_2d_grid output size");
    if out.is_empty() {
        return;
    }
    out.fill(0.0);
    let mut table = LatticeTable::default();
    let mut weight_sum = 0.0;
    let mut frequency = 1.0;
    let mut amplitude = 1.0;
    for octave in 0..octaves {
        let mixed_seed = splitmix64(seed.wrapping_add(u64::from(octave)));
        let x_cells: Vec<(i32, f64)> = xs.iter().map(|&x| cell(x * frequency)).collect();
        let z_cells: Vec<(i32, f64)> = zs.iter().map(|&z| cell(z * frequency)).collect();
        table.rebuild(mixed_seed, &x_cells, &z_cells, out.len());
        for (row, &(x0, tx)) in out.chunks_exact_mut(zs.len()).zip(&x_cells) {
            for (sum, &(z0, tz)) in row.iter_mut().zip(&z_cells) {
                *sum += amplitude * interpolate(table.corners(x0, z0), tx, tz);
            }
        }
        weight_sum += f64::abs(amplitude);
        frequency *= lacunarity;
        amplitude *= gain;
    }
    for v in out.iter_mut() {
        *v = if weight_sum > 0.0 {
            *v / weight_sum
        } else {
            0.0
        };
    }
}

/// Lattice values of one octave over every corner a grid touches. Falls back to hashing per
/// point when the grid is so sparse (high frequency) that the table would cost more.
#[derive(Default)]
struct LatticeTable {
    mixed_seed: u64,
    x_min: i64,
    z_min: i64,
    /// Corners along z per x row.
    depth: usize,
    /// Row-major `[x][z]` values; empty when hashing per point.
    values: Vec<f64>,
}

impl LatticeTable {
    fn rebuild(&mut self, mixed_seed: u64, xs: &[(i32, f64)], zs: &[(i32, f64)], points: usize) {
        let span = |cells: &[(i32, f64)]| {
            cells
                .iter()
                .fold((i64::MAX, i64::MIN), |(lo, hi), &(c, _)| {
                    (lo.min(i64::from(c)), hi.max(i64::from(c)))
                })
        };
        let ((x_min, x_max), (z_min, z_max)) = (span(xs), span(zs));
        // Corners x_min..=x_max + 1 by z_min..=z_max + 1 (both spans fit in u32 + 2).
        let (width, depth) = ((x_max - x_min + 2) as u64, (z_max - z_min + 2) as u64);
        self.mixed_seed = mixed_seed;
        self.values.clear();
        if width.saturating_mul(depth) > 4 * points as u64 {
            return;
        }
        (self.x_min, self.z_min, self.depth) = (x_min, z_min, depth as usize);
        for x in x_min..=x_max + 1 {
            for z in z_min..=z_max + 1 {
                // `as i32` wraps exactly like `wrapping_add(1)` in `corners`.
                self.values
                    .push(lattice_value(mixed_seed, x as i32, z as i32));
            }
        }
    }

    #[inline]
    fn corners(&self, x0: i32, z0: i32) -> [f64; 4] {
        if self.values.is_empty() {
            return corners(self.mixed_seed, x0, z0);
        }
        let i = (i64::from(x0) - self.x_min) as usize * self.depth
            + (i64::from(z0) - self.z_min) as usize;
        let d = self.depth;
        [
            self.values[i],
            self.values[i + d],
            self.values[i + 1],
            self.values[i + d + 1],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix64_matches_reference_values() {
        // First two outputs of the reference SplitMix64 generator seeded with 0 (the state
        // advances by the gamma before each mix).
        assert_eq!(splitmix64(0), 0xe220_a839_7b1d_cdaf);
        assert_eq!(splitmix64(0x9e37_79b9_7f4a_7c15), 0x6e78_9e6a_a1b9_65f4);
    }

    #[test]
    fn hash2_is_deterministic_and_depends_on_every_input() {
        assert_eq!(hash2(7, -3, 12), hash2(7, -3, 12));
        assert_ne!(hash2(7, -3, 12), hash2(8, -3, 12));
        assert_ne!(hash2(7, -3, 12), hash2(7, -2, 12));
        assert_ne!(hash2(7, -3, 12), hash2(7, -3, 13));
        // Swapped and sign-flipped coordinates are different lattice points.
        assert_ne!(hash2(7, 1, 2), hash2(7, 2, 1));
        assert_ne!(hash2(7, -1, 0), hash2(7, 0, -1));
    }

    #[test]
    fn hash2_bits_are_balanced() {
        // Each output bit should be set about half of the time over a block of the lattice.
        let mut ones = [0u32; 32];
        let n = 64 * 64;
        for x in -32..32 {
            for z in -32..32 {
                let h = hash2(42, x, z);
                for (bit, count) in ones.iter_mut().enumerate() {
                    *count += (h >> bit) & 1;
                }
            }
        }
        for (bit, &count) in ones.iter().enumerate() {
            let ratio = f64::from(count) / f64::from(n);
            assert!((0.45..0.55).contains(&ratio), "bit {bit}: {ratio}");
        }
    }

    #[test]
    fn value_noise_hits_lattice_values_and_stays_in_range() {
        for (x, z) in [(0, 0), (-5, 9), (123, -456)] {
            let expected = f64::from(hash2(3, x, z)) / f64::from(u32::MAX) * 2.0 - 1.0;
            assert_eq!(value_noise_2d(3, f64::from(x), f64::from(z)), expected);
        }
        let mut min = f64::MAX;
        let mut max = f64::MIN;
        for i in 0..4000 {
            let x = f64::from(i) * 0.37 - 700.0;
            let z = f64::from(i) * -0.61 + 300.0;
            let v = value_noise_2d(11, x, z);
            assert!((-1.0..=1.0).contains(&v), "{v} at ({x}, {z})");
            min = min.min(v);
            max = max.max(v);
        }
        assert!(
            min < -0.5 && max > 0.5,
            "noise range too narrow: [{min}, {max}]"
        );
    }

    #[test]
    fn value_noise_is_continuous() {
        // Smoothstep interpolation: tiny steps give tiny changes, also across cell borders.
        let step = 1e-6;
        for i in 0..200 {
            let x = f64::from(i) * 0.25 - 25.0;
            let z = f64::from(i) * 0.5 - 50.0;
            let a = value_noise_2d(5, x, z);
            let b = value_noise_2d(5, x + step, z + step);
            assert!((a - b).abs() < 1e-4, "jump at ({x}, {z}): {a} vs {b}");
        }
    }

    #[test]
    fn grid_matches_point_evaluation_bit_for_bit() {
        let axis = |origin: f64, step: f64| -> Vec<f64> {
            (0..32).map(|i| origin + f64::from(i) * step).collect()
        };
        let cases = [
            // (x origin, z origin, step, octaves, lacunarity, gain)
            (0.0, 0.0, 1.0 / 256.0, 5, 2.0, 0.5),
            (-1000.0 / 256.0, 77.0 / 256.0, 1.0 / 256.0, 5, 2.0, 0.5),
            (3.5, -9.25, 0.3, 4, 2.0, 0.5),
            // Dense lattice (frequency above 1): the per-point fallback.
            (-40.0, 12.0, 7.3, 3, 2.0, 0.5),
            // Descending coordinates, odd lacunarity and gain, 1 and 0 octaves.
            (100.0, 50.0, -0.11, 6, 1.7, 0.6),
            (0.5, 0.5, 0.01, 1, 2.0, 0.5),
            (0.5, 0.5, 0.01, 0, 2.0, 0.5),
            // Near the i32 lattice limit (wrapping corners).
            (2_147_483_640.0, -2_147_483_640.0, 0.5, 2, 2.0, 0.5),
        ];
        for (seed, &(x0, z0, step, octaves, lacunarity, gain)) in (40u64..).zip(&cases) {
            let (xs, zs) = (axis(x0, step), axis(z0, step));
            let mut grid = vec![0.0; 32 * 32];
            fbm_2d_grid(seed, &xs, &zs, octaves, lacunarity, gain, &mut grid);
            for (i, &x) in xs.iter().enumerate() {
                for (j, &z) in zs.iter().enumerate() {
                    let point = fbm_2d(seed, x, z, octaves, lacunarity, gain);
                    assert_eq!(
                        grid[i * 32 + j].to_bits(),
                        point.to_bits(),
                        "case {x0} {z0} {step} at ({x}, {z})"
                    );
                }
            }
        }
        // Non-square grids.
        let (xs, zs) = (axis(1.0, 0.2), axis(-3.0, 0.7)[..5].to_vec());
        let mut grid = vec![0.0; 32 * 5];
        fbm_2d_grid(1, &xs, &zs, 3, 2.0, 0.5, &mut grid);
        assert_eq!(grid[7 * 5 + 3], fbm_2d(1, xs[7], zs[3], 3, 2.0, 0.5));
    }

    #[test]
    fn fbm_normalizes_and_degenerates_cleanly() {
        for i in 0..500 {
            let x = f64::from(i) * 1.7;
            let z = f64::from(i) * -0.3;
            let v = fbm_2d(9, x, z, 6, 2.0, 0.5);
            assert!((-1.0..=1.0).contains(&v));
            assert_eq!(fbm_2d(9, x, z, 1, 2.0, 0.5), value_noise_2d(9, x, z));
        }
        assert_eq!(fbm_2d(9, 1.5, 2.5, 0, 2.0, 0.5), 0.0);
    }
}
