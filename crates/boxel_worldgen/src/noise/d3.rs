//! 3D gradient noise from integers: [`hash3`], improved Perlin noise ([`gradient_noise_3d`]) and
//! its fractal sum ([`fbm_3d`], and [`fbm_3d_grid`] over a whole grid at once).
//!
//! The same contract as the 2D noise: integer coordinates, a frequency per axis as an exact
//! fraction ([`Frequency3`]), an exact integer lattice cell, and an `f64` only for the position
//! inside the cell. Octaves with more than one lattice cell per voxel are dropped ([`Fbm3`]):
//! they would only alias into white noise, and without them every cell fits in `i64` (a cell is
//! never larger in magnitude than its coordinate), so [`hash3`] takes the cells as they are, with
//! no tiles and no `i128` cells.

use super::{Frequency, Scale, normalize, splitmix64};

/// A noise frequency per axis: `xz` along x and z, `y` along y (caverns taller than they are wide
/// take a lower `y` frequency). Both are exact fractions, so every axis splits exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Frequency3 {
    /// Lattice cells per voxel along x and z.
    pub xz: Frequency,
    /// Lattice cells per voxel along y.
    pub y: Frequency,
}

impl Frequency3 {
    /// The same frequency on every axis.
    pub const fn uniform(frequency: Frequency) -> Self {
        Self {
            xz: frequency,
            y: frequency,
        }
    }
}

/// Parameters of [`fbm_3d`]: octave `k` (from 0) has `frequency * lacunarity^k` lattice cells
/// per voxel on each axis and weight `gain^k`.
///
/// The lacunarity is an integer so every octave's frequencies stay exact fractions. The sum
/// stops before the first octave with more than one lattice cell per voxel on some axis: every
/// voxel of such an octave lands in another cell, which is aliased white noise and not worth its
/// cost. So a frequency of `1 / n` and a lacunarity of 2 give at most `1 + log2(n)` octaves,
/// whatever `octaves` asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fbm3 {
    /// Frequencies of octave 0.
    pub frequency: Frequency3,
    /// Number of octaves (0 gives a constant 0).
    pub octaves: u32,
    /// Frequency factor from one octave to the next.
    pub lacunarity: u32,
    /// Weight factor from one octave to the next.
    pub gain: f64,
}

impl Fbm3 {
    /// The octaves that are sampled, in order (see [`Fbm3`] for where they stop).
    fn octaves(self, seed: u64) -> impl Iterator<Item = Octave3> {
        let Frequency3 { xz, y } = self.frequency;
        let (xz_den, y_den) = (u64::from(xz.den), u64::from(y.den));
        let mut nums = (u64::from(xz.num), u64::from(y.num));
        let lacunarity = u64::from(self.lacunarity);
        let mut amplitude = 1.0;
        (0..self.octaves).map_while(move |k| {
            let (xz_num, y_num) = nums;
            if xz_num > xz_den || y_num > y_den {
                return None;
            }
            let octave = Octave3 {
                mixed_seed: splitmix64(seed.wrapping_add(u64::from(k))),
                xz: Scale::new(xz_num, xz.den),
                y: Scale::new(y_num, y.den),
                amplitude,
            };
            // Both numerators are at most their u32 denominator, so neither product overflows.
            nums = (xz_num * lacunarity, y_num * lacunarity);
            amplitude *= self.gain;
            Some(octave)
        })
    }
}

/// 32-bit hash of the lattice point `(x, y, z)`: a `SplitMix64` chain over the full 64-bit
/// coordinates (two's complement bits), `s(s(s(s(seed) ^ x) ^ y) ^ z)` truncated to its low 32
/// bits.
///
/// Every link is a bijective 64-bit mix and the last one comes after `z`, so neighbors along any
/// axis are unrelated, and nothing repeats with a period of `2^32` or any other. Unlike
/// [`hash2`](super::hash2), which keeps its former `i32` values, `hash3` has no legacy values to
/// keep, so it needs no tiles.
pub fn hash3(seed: u64, x: i64, y: i64, z: i64) -> u32 {
    link(link(link(splitmix64(seed), x), y), z) as u32
}

/// One link of the [`hash3`] chain: `splitmix64(h ^ c)`. Lattice points that share `x` (or `x`
/// and `y`) share the first links, so a table of corners hashes each prefix once.
#[inline(always)]
const fn link(h: u64, c: i64) -> u64 {
    splitmix64(h ^ c as u64)
}

/// The 12 edge gradients of improved Perlin noise as 16 entries, picked by the low 4 bits of a
/// hash: the last 4 repeat a tetrahedron of edges (Perlin 2002), so the pick is a mask.
const GRADIENTS: [[f64; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

/// The gradient a lattice point's [`hash3`] chain picks.
#[inline(always)]
fn gradient(h: u64) -> [f64; 3] {
    GRADIENTS[(h & 15) as usize]
}

/// Quintic fade `6t^5 - 15t^4 + 10t^3` for `t` in `[0, 1]`: 0 and 1 at the ends with zero first
/// and second derivatives, so the noise is C2 across cell borders.
#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// `a + (b - a) * t`.
#[inline(always)]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// One coordinate on one octave's lattice.
#[derive(Clone, Copy, Debug)]
struct Axis {
    /// The cell: the low corner.
    cell: i64,
    /// Position inside the cell, `t` in `[0, 1)`: the offset from the low corner.
    near: f64,
    /// `t - 1`: the offset from the high corner.
    far: f64,
    /// [`fade`] of `t`: the weight of the high corner.
    fade: f64,
}

/// `c` on a lattice of at most one cell per voxel (the [`Fbm3`] octaves): its cell is exact and
/// fits in `i64`.
#[inline]
fn axis(scale: Scale, c: i64) -> Axis {
    let (cell, rem) = scale.split_narrow(c).unwrap_or_else(|| {
        let (cell, rem) = scale.split(c);
        let cell = i64::try_from(cell).expect("a cell of at most one per voxel fits in i64");
        (cell, rem)
    });
    let t = scale.fraction(rem);
    Axis {
        cell,
        near: t,
        far: t - 1.0,
        fade: fade(t),
    }
}

/// One z layer of a cell's corners, interpolated in x and y.
#[derive(Clone, Copy, Debug)]
struct Layer {
    /// The corners' dot products without their z term, `gx * dx + gy * dy`.
    base: f64,
    /// The corners' z gradient components: the dot product grows by `slope * dz`.
    slope: f64,
}

/// The [`Layer`] of the corners `(x0, y0)`, `(x1, y0)`, `(x0, y1)` and `(x1, y1)` at one z.
/// Shared by the point and the grid paths, so both give bit-identical values.
#[inline(always)]
fn layer([g00, g10, g01, g11]: [[f64; 3]; 4], x: &Axis, y: &Axis) -> Layer {
    let dot = |g: [f64; 3], dx: f64, dy: f64| g[0] * dx + g[1] * dy;
    let near = lerp(dot(g00, x.near, y.near), dot(g10, x.far, y.near), x.fade);
    let far = lerp(dot(g01, x.near, y.far), dot(g11, x.far, y.far), x.fade);
    let slope_near = lerp(g00[2], g10[2], x.fade);
    let slope_far = lerp(g01[2], g11[2], x.fade);
    Layer {
        base: lerp(near, far, y.fade),
        slope: lerp(slope_near, slope_far, y.fade),
    }
}

/// The noise between the low and the high z layer of a cell, clamped to `[-1, 1]` (the 12 edge
/// gradients reach slightly past 1). Shared by the point and the grid paths.
#[inline(always)]
fn finish(low: Layer, high: Layer, z: &Axis) -> f64 {
    let low = low.base + low.slope * z.near;
    let high = high.base + high.slope * z.far;
    lerp(low, high, z.fade).clamp(-1.0, 1.0)
}

/// One octave of an [`Fbm3`].
#[derive(Clone, Copy, Debug)]
struct Octave3 {
    /// `splitmix64(seed + k)`.
    mixed_seed: u64,
    /// Lattice along x and z.
    xz: Scale,
    /// Lattice along y.
    y: Scale,
    /// `gain^k`.
    amplitude: f64,
}

impl Octave3 {
    /// The octave's noise at `(x, y, z)` (without the amplitude).
    #[inline]
    fn sample(self, x: i64, y: i64, z: i64) -> f64 {
        self.sample_axes(&axis(self.xz, x), &axis(self.y, y), &axis(self.xz, z))
    }

    /// [`Octave3::sample`] of coordinates already split. The `+ 1` corners wrap only at
    /// `i64::MAX` with a frequency of 1, where their weight is 0.
    #[inline]
    fn sample_axes(self, x: &Axis, y: &Axis, z: &Axis) -> f64 {
        let s = self.mixed_seed;
        let (x0, x1) = (link(s, x.cell), link(s, x.cell.wrapping_add(1)));
        let (y0, y1) = (y.cell, y.cell.wrapping_add(1));
        let columns = [link(x0, y0), link(x1, y0), link(x0, y1), link(x1, y1)];
        let corners = |z: i64| columns.map(|h| gradient(link(h, z)));
        let low = layer(corners(z.cell), x, y);
        let high = layer(corners(z.cell.wrapping_add(1)), x, y);
        finish(low, high, z)
    }
}

/// 3D gradient noise (Perlin's improved noise, 2002) in `[-1, 1]` at integer `(x, y, z)`, with
/// `frequency.xz` lattice cells per voxel along x and z and `frequency.y` along y.
///
/// The cell of the sample is `(floor(x * fxz), floor(y * fy), floor(z * fxz))`, exact, and `t`
/// is its position inside the cell, in `[0, 1)` per axis. Lattice point `c` has the gradient
/// `G[hash3(seed, c) & 15]`, one of the 12 cube edges (16 entries, 4 repeated, as in Perlin's
/// table). The noise is the sum of each corner's `G . (t - corner)` weighted per axis by the
/// quintic fade `6t^5 - 15t^4 + 10t^3`, clamped to `[-1, 1]`: smooth (C2), and 0 at every
/// lattice point.
///
/// The sum is evaluated in a fixed order, so every path gives the same bits: for each z layer of
/// corners, `gx * tx' + gy * ty'` and `gz` are interpolated in x, then in y (`base` and `slope`);
/// the noise is then `lerp(base0 + slope0 * tz, base1 + slope1 * (tz - 1), fade(tz))`, with
/// `lerp(a, b, w) = a + (b - a) * w` throughout.
///
/// A frequency above one cell per voxel on either axis gives 0: that octave is dropped, as in
/// [`fbm_3d`], whose one-octave sum this is.
pub fn gradient_noise_3d(seed: u64, x: i64, y: i64, z: i64, frequency: Frequency3) -> f64 {
    let one = Fbm3 {
        frequency,
        octaves: 1,
        lacunarity: 1,
        gain: 1.0,
    };
    one.octaves(seed)
        .next()
        .map_or(0.0, |octave| octave.sample(x, y, z))
}

/// Fractal Brownian motion of [`gradient_noise_3d`] in `[-1, 1]` at integer `(x, y, z)`.
///
/// Octave `k` (from 0) samples `gradient_noise_3d(seed + k, x, y, z, frequency * lacunarity^k)`
/// with weight `gain^k`, for the octaves [`Fbm3`] keeps; the sum is divided by the sum of the
/// absolute weights, so the result stays in `[-1, 1]`. Zero octaves give 0.
pub fn fbm_3d(seed: u64, x: i64, y: i64, z: i64, fbm: Fbm3) -> f64 {
    let mut sum = 0.0;
    let mut weight_sum = 0.0;
    for octave in fbm.octaves(seed) {
        sum += octave.amplitude * octave.sample(x, y, z);
        weight_sum += f64::abs(octave.amplitude);
    }
    normalize(sum, weight_sum)
}

/// [`fbm_3d`] at every point of the grid `xs` x `ys` x `zs` (`out[(i * ys.len() + j) *
/// zs.len() + k]` gets the value at `(xs[i], ys[j], zs[k])`), bit-identical to calling
/// [`fbm_3d`] per point.
///
/// Each octave splits each coordinate once and hashes every lattice corner the grid touches once
/// (one table per octave) instead of eight times per point, and the x-y half of the
/// interpolation is shared by the points of a z row that fall in the same cell. The coordinates
/// may come in any order and with any spacing (a stride is a coarser grid); a grid too sparse
/// for its lattice is sampled point by point.
///
/// # Panics
///
/// If `out.len()` is not `xs.len() * ys.len() * zs.len()`.
pub fn fbm_3d_grid(seed: u64, xs: &[i64], ys: &[i64], zs: &[i64], fbm: Fbm3, out: &mut [f64]) {
    let points = xs
        .len()
        .checked_mul(ys.len())
        .and_then(|n| n.checked_mul(zs.len()));
    assert_eq!(points, Some(out.len()), "fbm_3d_grid output size");
    if out.is_empty() {
        return;
    }
    out.fill(0.0);
    let mut table = LatticeTable3::default();
    let (mut x, mut y, mut z) = (
        Vec::with_capacity(xs.len()),
        Vec::with_capacity(ys.len()),
        Vec::with_capacity(zs.len()),
    );
    let mut weight_sum = 0.0;
    for octave in fbm.octaves(seed) {
        let split = |axes: &mut Vec<Axis>, coords: &[i64], scale: Scale| {
            axes.clear();
            axes.extend(coords.iter().map(|&c| axis(scale, c)));
        };
        split(&mut x, xs, octave.xz);
        split(&mut y, ys, octave.y);
        split(&mut z, zs, octave.xz);
        if table.rebuild(octave.mixed_seed, &x, &y, &z, out.len()) {
            table.accumulate(&x, &y, &z, octave.amplitude, out);
        } else {
            let mut sums = out.iter_mut();
            for xa in &x {
                for ya in &y {
                    // `z` first: `zip` stops on it without taking another sum.
                    for (za, sum) in z.iter().zip(sums.by_ref()) {
                        *sum += octave.amplitude * octave.sample_axes(xa, ya, za);
                    }
                }
            }
        }
        weight_sum += f64::abs(octave.amplitude);
    }
    for v in out.iter_mut() {
        *v = normalize(*v, weight_sum);
    }
}

/// Gradients of one octave over every lattice corner a grid touches.
#[derive(Default)]
struct LatticeTable3 {
    /// `[x][y][z]` over the corners `x_min..=x_max + 1` by `y_min..=y_max + 1` by
    /// `z_min..=z_max + 1`.
    gradients: Vec<[f64; 3]>,
    /// Corners per x slab (y by z): the stride of x.
    slab: usize,
    /// Corners along z: the stride of y.
    depth: usize,
    /// Per grid x: start of its cell's slab.
    xs: Vec<usize>,
    /// Per grid y: start of its cell's row inside a slab.
    ys: Vec<usize>,
    /// Per grid z: its cell inside a row.
    zs: Vec<usize>,
}

impl LatticeTable3 {
    /// Hashes every corner of the `xs` x `ys` x `zs` cells once. False, with nothing hashed, when
    /// the grid is so sparse (high frequency) that the table would cost more than hashing per
    /// point.
    fn rebuild(
        &mut self,
        mixed_seed: u64,
        xs: &[Axis],
        ys: &[Axis],
        zs: &[Axis],
        points: usize,
    ) -> bool {
        /// The lowest cell and the corners spanned along one axis (saturated).
        fn span(axes: &[Axis]) -> (i64, u64) {
            let (lo, hi) = axes.iter().fold((i64::MAX, i64::MIN), |(lo, hi), a| {
                (lo.min(a.cell), hi.max(a.cell))
            });
            (lo, hi.abs_diff(lo).saturating_add(2))
        }
        let ((x_min, width), (y_min, height), (z_min, depth)) = (span(xs), span(ys), span(zs));
        let corners = u128::from(width)
            .saturating_mul(u128::from(height))
            .saturating_mul(u128::from(depth));
        if corners > 4 * points as u128 {
            return false;
        }
        // Every span is at most `4 * points`, so every offset below fits in usize.
        let (width, height, depth) = (width as usize, height as usize, depth as usize);
        self.depth = depth;
        self.slab = height * depth;
        self.gradients.clear();
        for dx in 0..width {
            let hx = link(mixed_seed, x_min.wrapping_add(dx as i64));
            for dy in 0..height {
                let hxy = link(hx, y_min.wrapping_add(dy as i64));
                self.gradients.extend(
                    (0..depth).map(|dz| gradient(link(hxy, z_min.wrapping_add(dz as i64)))),
                );
            }
        }
        let offsets = |out: &mut Vec<usize>, axes: &[Axis], min: i64, stride: usize| {
            out.clear();
            out.extend(axes.iter().map(|a| a.cell.abs_diff(min) as usize * stride));
        };
        offsets(&mut self.xs, xs, x_min, self.slab);
        offsets(&mut self.ys, ys, y_min, depth);
        offsets(&mut self.zs, zs, z_min, 1);
        true
    }

    /// Adds `amplitude` times the octave's noise at every grid point to `out`.
    fn accumulate(&self, xs: &[Axis], ys: &[Axis], zs: &[Axis], amplitude: f64, out: &mut [f64]) {
        let (g, slab, depth) = (&self.gradients[..], self.slab, self.depth);
        let mut rows = out.chunks_exact_mut(zs.len());
        for (x, &x_start) in xs.iter().zip(&self.xs) {
            for ((y, &y_start), row) in ys.iter().zip(&self.ys).zip(rows.by_ref()) {
                // The four columns of corners along z: (x0, y0), (x1, y0), (x0, y1), (x1, y1).
                let start = x_start + y_start;
                let columns = [start, start + slab, start + depth, start + slab + depth];
                let corners = |k: usize| columns.map(|c| g[c + k]);
                // The last cell along z and its two layers: consecutive points share them.
                let mut last: Option<(usize, Layer, Layer)> = None;
                for ((sum, z), &k) in row.iter_mut().zip(zs).zip(&self.zs) {
                    let (low, high) = match last {
                        Some((cell, low, high)) if cell == k => (low, high),
                        // One cell up (ascending z) or down (descending z): one new layer.
                        Some((cell, _, high)) if cell + 1 == k => {
                            (high, layer(corners(k + 1), x, y))
                        }
                        Some((cell, low, _)) if k + 1 == cell => (layer(corners(k), x, y), low),
                        _ => (layer(corners(k), x, y), layer(corners(k + 1), x, y)),
                    };
                    last = Some((k, low, high));
                    *sum += amplitude * finish(low, high, z);
                }
            }
        }
    }
}
