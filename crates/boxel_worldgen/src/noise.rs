//! Deterministic noise from integers (no dependencies): 2D value noise ([`value_noise_2d`],
//! [`fbm_2d`]) and 3D gradient noise ([`gradient_noise_3d`], [`fbm_3d`]). Same seed +
//! coordinates = same value on every platform (integer lattice and hashing, `f64`
//! interpolation).
//!
//! Samples are integer coordinates and frequencies exact fractions ([`Frequency`]), so the
//! lattice cell of a sample is an exact integer and its position inside the cell an exact
//! fraction: the noise is the same function at any `i64` coordinate, with no precision loss, no
//! saturated lattice and no repetition. Only the position inside the cell, in `[0, 1)`, becomes
//! an `f64`; an absolute coordinate never does.
//!
//! The seed goes through `SplitMix64` before it is combined with the lattice coordinates, so
//! nearby seeds (`fbm_2d` uses `seed + octave` per octave) give unrelated lattices.
//!
//! After the integer split the noise uses only `+`, `-`, `*` and `/` on `f64` (correctly rounded
//! by IEEE 754 everywhere), in a fixed order: no `mul_add` (Rust never fuses on its own), no
//! `powf` and no other libm call, whose results may differ between platforms.

/// A noise frequency as an exact fraction: `num` lattice cells every `den` voxels.
///
/// Kept in lowest terms, so equal frequencies compare equal
/// (`Frequency::new(2, 512) == Frequency::one_in(256)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Frequency {
    num: u32,
    den: u32,
}

impl Frequency {
    /// One lattice cell every `n` voxels (frequency `1 / n`). Panics if `n` is 0.
    pub const fn one_in(n: u32) -> Self {
        Self::new(1, n)
    }

    /// `num / den` lattice cells per voxel, reduced to lowest terms. Panics if `den` is 0.
    pub const fn new(num: u32, den: u32) -> Self {
        assert!(den != 0, "Frequency: the denominator must not be 0");
        let g = gcd(num, den);
        Self {
            num: num / g,
            den: den / g,
        }
    }

    /// Numerator (lowest terms).
    pub const fn num(self) -> u32 {
        self.num
    }

    /// Denominator (lowest terms, never 0).
    pub const fn den(self) -> u32 {
        self.den
    }

    /// The frequency in cells per voxel, rounded to `f64` (for display: the noise never
    /// samples with it).
    pub fn to_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }
}

/// Greatest common divisor (`gcd(0, b) = b`).
const fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Parameters of [`fbm_2d`]: octave `k` (from 0) has `frequency * lacunarity^k` lattice cells
/// per voxel and weight `gain^k`.
///
/// The lacunarity is an integer so every octave's frequency stays an exact fraction. The sum
/// stops before the first octave whose frequency numerator (`num * lacunarity^k`) overflows
/// `u64`: such an octave has more than `2^32` cells per voxel. With a lacunarity of 2 that
/// leaves at least 33 octaves (64 for a frequency `1 / n`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fbm {
    /// Frequency of octave 0.
    pub frequency: Frequency,
    /// Number of octaves (0 gives a constant 0).
    pub octaves: u32,
    /// Frequency factor from one octave to the next.
    pub lacunarity: u32,
    /// Weight factor from one octave to the next.
    pub gain: f64,
}

impl Fbm {
    /// The octaves that are sampled, in order (see [`Fbm`] for where they stop).
    fn octaves(self, seed: u64) -> impl Iterator<Item = Octave> {
        // Every octave shares the denominator.
        let base = Scale::new(self.frequency.num.into(), self.frequency.den);
        let mut num = Some(base.num);
        let mut amplitude = 1.0;
        (0..self.octaves).map_while(move |k| {
            let octave = Octave {
                mixed_seed: splitmix64(seed.wrapping_add(u64::from(k))),
                scale: Scale { num: num?, ..base },
                amplitude,
            };
            num = num?.checked_mul(u64::from(self.lacunarity));
            amplitude *= self.gain;
            Some(octave)
        })
    }
}

/// One octave of an [`Fbm`].
#[derive(Clone, Copy, Debug)]
struct Octave {
    /// `splitmix64(seed + k)`.
    mixed_seed: u64,
    scale: Scale,
    /// `gain^k`.
    amplitude: f64,
}

impl Octave {
    /// The octave's value noise at `(x, z)` (without the amplitude). Always inlined: it is the
    /// inner step of every sample, and the rare products past `i64` go out of line.
    #[inline(always)]
    fn sample(self, x: i64, z: i64) -> f64 {
        let s = self.scale;
        if let (Some((x0, rx)), Some((z0, rz))) = (s.split_narrow(x), s.split_narrow(z)) {
            let corners = corners(self.mixed_seed, x0, z0);
            return interpolate(corners, s.weight(rx), s.weight(rz));
        }
        self.sample_wide(x, z)
    }

    /// [`Octave::sample`] when a product `c * num` leaves `i64`.
    #[inline(never)]
    fn sample_wide(self, x: i64, z: i64) -> f64 {
        let ((x0, tx), (z0, tz)) = (self.scale.cell(x), self.scale.cell(z));
        interpolate(wide_corners(self.mixed_seed, x0, z0), tx, tz)
    }
}

/// The lattice of one octave: `num / den` cells per voxel (`num` includes the lacunarity, so it
/// can exceed `u32`).
#[derive(Clone, Copy, Debug)]
struct Scale {
    num: u64,
    den: u32,
    /// `log2(den)` when `den` is a power of two: cell and remainder by shift and mask.
    shift: Option<u32>,
    /// `1 / den`, used only when `den` is a power of two (then it is exact).
    inverse: f64,
}

impl Scale {
    fn new(num: u64, den: u32) -> Self {
        Self {
            num,
            den,
            shift: den.is_power_of_two().then(|| den.trailing_zeros()),
            inverse: 1.0 / f64::from(den),
        }
    }

    /// [`Scale::split`] in 64 bits, when `c * num` fits in `i64` (the common case: every octave
    /// of the terrain within `2^55` voxels of the origin).
    #[inline]
    fn split_narrow(self, c: i64) -> Option<(i64, u32)> {
        let p = c.checked_mul(i64::try_from(self.num).ok()?)?;
        Some(match self.shift {
            Some(shift) => (p >> shift, (p as u32) & ((1 << shift) - 1)),
            None => {
                let den = i64::from(self.den);
                (p.div_euclid(den), p.rem_euclid(den) as u32)
            }
        })
    }

    /// `floor(c * num / den)` and `(c * num) mod den`, exact: `|c * num| < 2^63 * 2^64` fits in
    /// `i128`, and so does the cell.
    #[inline]
    fn split(self, c: i64) -> (i128, u32) {
        let p = i128::from(c) * i128::from(self.num);
        match self.shift {
            // An arithmetic shift is a floor division; the low bits are the remainder.
            Some(shift) => (p >> shift, (p as u32) & ((1 << shift) - 1)),
            None => match i64::try_from(p) {
                // The common case: one 64-bit division.
                Ok(p) => {
                    let den = i64::from(self.den);
                    (p.div_euclid(den).into(), p.rem_euclid(den) as u32)
                }
                Err(_) => {
                    let den = i128::from(self.den);
                    (p.div_euclid(den), p.rem_euclid(den) as u32)
                }
            },
        }
    }

    /// The position `rem / den` inside a cell, in `[0, 1)`, with `rem` and `den` exact in `f64`
    /// (one correctly rounded division, exact when `den` is a power of two).
    #[inline]
    fn fraction(self, rem: u32) -> f64 {
        if self.shift.is_some() {
            // Times the exact inverse of a power of two: equal to the division, and cheaper.
            f64::from(rem) * self.inverse
        } else {
            f64::from(rem) / f64::from(self.den)
        }
    }

    /// Smoothstep weight of the position `rem / den` inside a cell ([`Scale::fraction`]).
    #[inline]
    fn weight(self, rem: u32) -> f64 {
        smoothstep(self.fraction(rem))
    }

    /// Lattice cell of coordinate `c` and the smoothstep weight of `c` inside it.
    #[inline]
    fn cell(self, c: i64) -> (i128, f64) {
        let (cell, rem) = match self.split_narrow(c) {
            Some((cell, rem)) => (cell.into(), rem),
            None => self.split(c),
        };
        (cell, self.weight(rem))
    }
}

/// `SplitMix64` output function (Steele, Lea and Flood 2014): a bijective 64-bit mix.
#[inline]
pub(crate) const fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The 64-bit key of `(x, z)`, injective: x in the high half, z in the low half (two's
/// complement bits).
#[inline]
const fn lattice_key(x: i32, z: i32) -> u64 {
    ((x as u32 as u64) << 32) | (z as u32 as u64)
}

/// [`hash2`] of a point inside `i32` with the seed already mixed (`splitmix64(seed)`).
#[inline]
const fn lattice_hash(mixed_seed: u64, x: i32, z: i32) -> u32 {
    splitmix64(mixed_seed ^ lattice_key(x, z)) as u32
}

/// [`hash2`] of a point outside `i32` with the seed already mixed: `(x, z)` are its low 32 bits
/// and `tag` stands for the bits above them. The tag is mixed in after the key is hashed, so
/// the values under two tags are a pseudo-random rearrangement of each other, never a shifted
/// or relabeled copy.
#[inline]
const fn tile_hash(mixed_seed: u64, tag: u64, x: i32, z: i32) -> u32 {
    splitmix64(splitmix64(mixed_seed ^ lattice_key(x, z)) ^ tag) as u32
}

/// The [`splitmix64`] finalizer without the increment: a bijective 64-bit mix with `mix(0) = 0`.
#[inline]
const fn mix(x: u64) -> u64 {
    let mut z = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// How many times `2^32` separate `v` from the `i32` its low 32 bits sign-extend to, as a 32-bit
/// pattern: 0 exactly when `v` fits in `i32`. With the low 32 bits it determines `v` (a
/// bijection of `i64` onto `(u32, u32)`).
#[inline]
const fn excess(v: i64) -> u32 {
    // `v - sext(low)` is a multiple of 2^32 (mod 2^64), so the shift is exact.
    (v.wrapping_sub(v as i32 as i64) >> 32) as u32
}

/// `v` as the `i64` its low 64 bits sign-extend to and the multiple of `2^64` left above it, as
/// a 64-bit pattern: `(v, 0)` exactly when `v` fits in `i64` (a bijection of `i128` onto
/// `(i64, u64)`, [`excess`] one level up).
#[inline]
const fn split_wide(v: i128) -> (i64, u64) {
    let low = v as i64;
    // `v - sext(low)` is a multiple of 2^64 (mod 2^128), so the shift is exact.
    (low, (v.wrapping_sub(low as i128) >> 64) as u64)
}

/// 32-bit hash of `(seed, x, z)` (e.g. a SplitMix64/xxhash-style mix).
///
/// With `key = x << 32 | z` from the low 32 bits of `x` and `z`, and `e = excess(x) << 32 |
/// excess(z)` (what is left of them above `i32`, 0 exactly inside it), the point maps
/// injectively to `(e, key)`. Inside `i32` the hash is `SplitMix64(SplitMix64(seed) ^ key)`
/// truncated to the low 32 bits: exactly the former `(seed, i32, i32)` version. Beyond, `mix(e)`
/// tags the point's `2^32 x 2^32` tile and is mixed in after the key is hashed:
/// `SplitMix64(SplitMix64(SplitMix64(seed) ^ key) ^ mix(e))` (`mix` is bijective with
/// `mix(0) = 0`, so tiles get distinct nonzero tags). No tile is a shifted or relabeled copy of
/// another, and nothing repeats with a period of `2^32`.
pub fn hash2(seed: u64, x: i64, z: i64) -> u32 {
    hash2_mixed(splitmix64(seed), x, z)
}

/// [`hash2`] with the seed already mixed.
#[inline]
const fn hash2_mixed(mixed_seed: u64, x: i64, z: i64) -> u32 {
    let e = ((excess(x) as u64) << 32) | excess(z) as u64;
    if e == 0 {
        lattice_hash(mixed_seed, x as i32, z as i32)
    } else {
        tile_hash(mixed_seed, mix(e), x as i32, z as i32)
    }
}

/// Hash of lattice cell `(x, z)` with the seed already mixed: [`hash2`] for cells inside `i64`.
/// Beyond (only octaves finer than a voxel reach them, far out, where every sample lands in
/// another cell and the octave is white noise anyway), the multiples of `2^64` above `i64`
/// (`xh`, `zh`) go into the tag of the cell's `2^32 x 2^32` tile: `tag ^ s(s(xh) ^ zh)` (`s` =
/// [`splitmix64`]). That pair hash has 64 bits for `2^128` tiles of `2^64 x 2^64` cells, so whole
/// tiles repeat: tile `(xh, zh)` has the values of tile `(xh', zh ^ s(xh) ^ s(xh'))` for every
/// `xh'` that does not give tile `(0, 0)` (the `i64` cells, without a pair hash).
#[inline]
fn cell_hash(mixed_seed: u64, x: i128, z: i128) -> u32 {
    if let (Ok(x), Ok(z)) = (i32::try_from(x), i32::try_from(z)) {
        // Inside i32 (every cell near the origin): `hash2_mixed` with `e = 0`.
        return lattice_hash(mixed_seed, x, z);
    }
    let ((x, x_high), (z, z_high)) = (split_wide(x), split_wide(z));
    if x_high | z_high == 0 {
        return hash2_mixed(mixed_seed, x, z);
    }
    let e = ((excess(x) as u64) << 32) | excess(z) as u64;
    let tag = mix(e) ^ splitmix64(splitmix64(x_high) ^ z_high);
    tile_hash(mixed_seed, tag, x as i32, z as i32)
}

/// A lattice hash as a value in `[-1, 1]`.
#[inline]
fn unit(h: u32) -> f64 {
    f64::from(h) / f64::from(u32::MAX) * 2.0 - 1.0
}

/// Lattice value in `[-1, 1]` of cell `(x, z)`.
#[inline]
fn lattice_value(mixed_seed: u64, x: i128, z: i128) -> f64 {
    unit(cell_hash(mixed_seed, x, z))
}

/// Cubic smoothstep `t * t * (3 - 2t)` for `t` in `[0, 1]`.
#[inline]
fn smoothstep(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// Corner values `(v00, v10, v01, v11)` of cell `(x0, z0)` (`v10` is at `x0 + 1`). Always
/// inlined: it is the inner step of every sample.
#[inline(always)]
fn corners(mixed_seed: u64, x0: i64, z0: i64) -> [f64; 4] {
    if let (Ok(x0), Ok(z0)) = (i32::try_from(x0), i32::try_from(z0))
        && x0 < i32::MAX
        && z0 < i32::MAX
    {
        // Every corner inside i32 (all cells near the origin): `hash2` without excess.
        return tile_corners(mixed_seed, x0, z0);
    }
    far_corners(mixed_seed, x0, z0)
}

/// Lattice values of the corners `(x0, z0)` to `(x0 + 1, z0 + 1)` with one seed (the callers
/// keep `x0` and `z0` below `i32::MAX`).
#[inline(always)]
fn tile_corners(seed: u64, x0: i32, z0: i32) -> [f64; 4] {
    let (x1, z1) = (x0 + 1, z0 + 1);
    [
        unit(lattice_hash(seed, x0, z0)),
        unit(lattice_hash(seed, x1, z0)),
        unit(lattice_hash(seed, x0, z1)),
        unit(lattice_hash(seed, x1, z1)),
    ]
}

/// [`corners`] of a cell with a corner outside `i32`. Usually the four corners still share
/// their bits above `i32` (they are in one `2^32 x 2^32` tile), and so the tag `hash2` makes of
/// those bits: one tag instead of four.
#[inline(never)]
fn far_corners(mixed_seed: u64, x0: i64, z0: i64) -> [f64; 4] {
    // `c + 1` exists and has the excess of `c` unless its low 32 bits wrap.
    let same_tile = |c: i64| c as i32 != i32::MAX && c != i64::MAX;
    if same_tile(x0) && same_tile(z0) {
        let e = ((excess(x0) as u64) << 32) | excess(z0) as u64;
        // `corners` takes the i32 path otherwise.
        debug_assert!(e != 0, "far_corners of a cell inside i32");
        let tag = mix(e);
        let (x0, z0) = (x0 as i32, z0 as i32);
        let (x1, z1) = (x0 + 1, z0 + 1);
        return [
            unit(tile_hash(mixed_seed, tag, x0, z0)),
            unit(tile_hash(mixed_seed, tag, x1, z0)),
            unit(tile_hash(mixed_seed, tag, x0, z1)),
            unit(tile_hash(mixed_seed, tag, x1, z1)),
        ];
    }
    wide_corners(mixed_seed, x0.into(), z0.into())
}

/// [`corners`] of any cell, corner by corner (for cells whose corners straddle a `2^32` tile
/// or leave `i64`); kept out of line so the common path stays small. `+ 1` cannot overflow:
/// cells are below `2^127 - 2^63` in magnitude ([`Scale::split`]).
#[inline(never)]
fn wide_corners(mixed_seed: u64, x0: i128, z0: i128) -> [f64; 4] {
    let (x1, z1) = (x0 + 1, z0 + 1);
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

/// 2D value noise in `[-1, 1]` at integer `(x, z)`: smoothstep-interpolated lattice values,
/// with `frequency` lattice cells per voxel.
///
/// The lattice value of cell `(cx, cz)` is `hash2(seed, cx, cz) / u32::MAX * 2 - 1` (for cells
/// inside `i64`), and the cell of `x` is `floor(x * frequency)`, exact. With a frequency of 1 the
/// noise is that value at every sample.
pub fn value_noise_2d(seed: u64, x: i64, z: i64, frequency: Frequency) -> f64 {
    let octave = Octave {
        mixed_seed: splitmix64(seed),
        scale: Scale::new(frequency.num.into(), frequency.den),
        amplitude: 1.0,
    };
    octave.sample(x, z)
}

/// Fractal Brownian motion of [`value_noise_2d`] in `[-1, 1]` at integer `(x, z)`.
///
/// Octave `k` (from 0) samples `value_noise_2d(seed + k, x, z, frequency * lacunarity^k)` with
/// weight `gain^k`; the sum is divided by the sum of the absolute weights, so the result stays
/// in `[-1, 1]`. One octave equals [`value_noise_2d`]; zero octaves give 0.
pub fn fbm_2d(seed: u64, x: i64, z: i64, fbm: Fbm) -> f64 {
    let mut sum = 0.0;
    let mut weight_sum = 0.0;
    for octave in fbm.octaves(seed) {
        sum += octave.amplitude * octave.sample(x, z);
        weight_sum += f64::abs(octave.amplitude);
    }
    normalize(sum, weight_sum)
}

/// `sum / weight_sum`, or 0 without weights.
#[inline]
fn normalize(sum: f64, weight_sum: f64) -> f64 {
    if weight_sum > 0.0 {
        sum / weight_sum
    } else {
        0.0
    }
}

/// [`fbm_2d`] at every point of the grid `xs` x `zs` (`out[i * zs.len() + j]` gets the value at
/// `(xs[i], zs[j])`), bit-identical to calling [`fbm_2d`] per point. Each octave splits each
/// coordinate into cell and weight once, and hashes every lattice corner the grid touches once
/// instead of four times per point.
///
/// The coordinates may come in any order and with any spacing (a stride is a coarser grid);
/// a grid too sparse for its lattice is sampled point by point.
///
/// # Panics
///
/// If `out.len()` is not `xs.len() * zs.len()`.
pub fn fbm_2d_grid(seed: u64, xs: &[i64], zs: &[i64], fbm: Fbm, out: &mut [f64]) {
    assert_eq!(out.len(), xs.len() * zs.len(), "fbm_2d_grid output size");
    if out.is_empty() {
        return;
    }
    out.fill(0.0);
    let mut table = LatticeTable::default();
    let mut x_cells = Vec::with_capacity(xs.len());
    let mut z_cells = Vec::with_capacity(zs.len());
    let points = out.len();
    let mut weight_sum = 0.0;
    for octave in fbm.octaves(seed) {
        x_cells.clear();
        x_cells.extend(xs.iter().map(|&x| octave.scale.cell(x)));
        z_cells.clear();
        z_cells.extend(zs.iter().map(|&z| octave.scale.cell(z)));
        let amplitude = octave.amplitude;
        let rows = out.chunks_exact_mut(zs.len()).zip(&x_cells);
        if table.rebuild(octave.mixed_seed, &x_cells, &z_cells, points) {
            let (values, d) = (&table.values, table.depth);
            for ((row, &(_, tx)), &x_row) in rows.zip(&table.rows) {
                for ((sum, &(_, tz)), &z_col) in row.iter_mut().zip(&z_cells).zip(&table.cols) {
                    let i = x_row + z_col;
                    let corners = [values[i], values[i + d], values[i + 1], values[i + d + 1]];
                    *sum += amplitude * interpolate(corners, tx, tz);
                }
            }
        } else {
            for (row, &(x0, tx)) in rows {
                for (sum, &(z0, tz)) in row.iter_mut().zip(&z_cells) {
                    let corners = wide_corners(octave.mixed_seed, x0, z0);
                    *sum += amplitude * interpolate(corners, tx, tz);
                }
            }
        }
        weight_sum += f64::abs(amplitude);
    }
    for v in out.iter_mut() {
        *v = normalize(*v, weight_sum);
    }
}

/// Lattice values of one octave over every corner a grid touches.
#[derive(Default)]
struct LatticeTable {
    /// Row-major `[x][z]` values over the corners `x_min..=x_max + 1` by `z_min..=z_max + 1`.
    values: Vec<f64>,
    /// Corners along z per x row.
    depth: usize,
    /// Per grid x: start of its cell's row in `values`.
    rows: Vec<usize>,
    /// Per grid z: offset of its cell inside a row.
    cols: Vec<usize>,
}

impl LatticeTable {
    /// Hashes every corner of the `xs` x `zs` cells once. False, with nothing hashed, when the
    /// grid is so sparse (high frequency) that the table would cost more than hashing per point.
    fn rebuild(
        &mut self,
        mixed_seed: u64,
        xs: &[(i128, f64)],
        zs: &[(i128, f64)],
        points: usize,
    ) -> bool {
        let span = |cells: &[(i128, f64)]| {
            cells
                .iter()
                .fold((i128::MAX, i128::MIN), |(lo, hi), &(c, _)| {
                    (lo.min(c), hi.max(c))
                })
        };
        let ((x_min, x_max), (z_min, z_max)) = (span(xs), span(zs));
        // Corners x_min..=x_max + 1 by z_min..=z_max + 1.
        let width = x_max.abs_diff(x_min).saturating_add(2);
        let depth = z_max.abs_diff(z_min).saturating_add(2);
        if width.saturating_mul(depth) > 4 * points as u128 {
            return false;
        }
        // Both spans are at most `4 * points`, so every offset below fits in usize.
        let depth = depth as usize;
        self.depth = depth;
        self.values.clear();
        for x in x_min..=x_max + 1 {
            for z in z_min..=z_max + 1 {
                self.values.push(lattice_value(mixed_seed, x, z));
            }
        }
        self.rows.clear();
        self.rows
            .extend(xs.iter().map(|&(c, _)| (c - x_min) as usize * depth));
        self.cols.clear();
        self.cols
            .extend(zs.iter().map(|&(c, _)| (c - z_min) as usize));
        true
    }
}

mod d3;

pub use d3::{Fbm3, Frequency3, fbm_3d, fbm_3d_grid, gradient_noise_3d, hash3};

#[cfg(test)]
mod tests;
