//! The integer noise of the fog, bit for bit what `fog.wgsl` computes: the oracle the GPU tests
//! compare against.
//!
//! A cell's noise is value noise over its absolute index minus the wind displacement, on a
//! lattice of `L = 2^l` cells:
//!
//! - **Lattice.** In 1/4096 of a cell, the cell sits at `V = 4096 * C - D`, where `D` is the
//!   displacement in 1/4096 cells. Its lattice point is `V >> (12 + l)` and its weight inside
//!   the lattice step `(V >> l) & 4095`. The second octave has half the step.
//! - **Hash.** A lattice point's 64-bit coordinates (two's complement) split into a low and a
//!   high word per axis. The high words go into a seed mix, the low words through a chained
//!   `lowbias32`; the value is the top 12 bits.
//! - **Interpolation.** A fixed-point smoothstep of the weight, then integer lerps (bilinear in
//!   x and z for [`NoiseDims::Flat`], trilinear for `Volume`), each
//!   product under `2^26`. Two octaves mix as `(2 * v1 + v2) / 3`.
//! - **Coverage.** `clamp(n + 1 - threshold, 0, 4095) * gain >> 8`, at most 4096 (one).
//!
//! The GPU never sees an absolute coordinate: [`LayerParams`] splits the
//! cell of the render origin into a lattice point (low and high words) and an offset inside its
//! step, and the shader adds the small cell offset of the march with an explicit carry into the
//! high words ([`coverage`]). [`coverage_direct`] computes the same from the absolute cell in
//! `i128`, the reference both are tested against.

use crate::config::{FogLayer, NoiseDims};
use crate::layer::{LayerParams, displacement};

/// Coverage of a full cell.
pub const COVERAGE_ONE: u32 = 4096;
/// Mixed into the seed of the second octave.
pub const OCTAVE2_SALT: u32 = 0x9e37_79b9;
/// Gain of a cell with softness 0 (`q8`): one step of noise over the threshold fills it.
pub const MAX_GAIN: u32 = 1 << 20;

/// The `lowbias32` integer hash (Chris Wellons).
#[inline]
pub const fn lowbias32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

/// The 32-bit seed of a layer's first octave.
pub const fn seed32(seed: u64) -> u32 {
    lowbias32((seed as u32) ^ lowbias32((seed >> 32) as u32))
}

/// The 32-bit seed of the second octave from the first one's.
pub const fn seed32_octave2(seed32: u32) -> u32 {
    lowbias32(seed32 ^ OCTAVE2_SALT)
}

/// The seed mixed with the high words of a lattice point.
pub const fn seed_mix(seed32: u32, hi: [u32; 3]) -> u32 {
    lowbias32(lowbias32(lowbias32(seed32 ^ hi[0]) ^ hi[1]) ^ hi[2])
}

/// The 12-bit value of the lattice point with low words `lo` (`lo[1]` unused when flat).
pub const fn lattice_value(mix: u32, lo: [u32; 3], volume: bool) -> i32 {
    let hx = lowbias32(mix ^ lo[0]);
    let hxy = if volume { lowbias32(hx ^ lo[1]) } else { hx };
    (lowbias32(hxy ^ lo[2]) >> 20) as i32
}

/// Fixed-point smoothstep of a weight in `0..4096`: `t^2 (3 - 2t)` in 1/4096.
#[inline]
pub const fn smooth(w: i32) -> i32 {
    (((w * w) >> 12) * (12288 - 2 * w)) >> 12
}

/// Integer lerp from `a` to `b` by `s` (in 1/4096); never leaves `[min(a, b), max(a, b)]`.
#[inline]
pub const fn lerp(a: i32, b: i32, s: i32) -> i32 {
    a + (((b - a) * s) >> 12)
}

/// `(lo, hi) + li` as a 64-bit two's complement number, in words.
#[inline]
pub const fn add_words(lo: u32, hi: u32, li: i32) -> (u32, u32) {
    let n = lo.wrapping_add(li as u32);
    let hi = if li >= 0 {
        hi.wrapping_add((n < lo) as u32)
    } else {
        hi.wrapping_sub((n > lo) as u32)
    };
    (n, hi)
}

/// The words of `2 * (lo, hi)`: the base of the second octave's lattice.
#[inline]
pub const fn double_words(lo: u32, hi: u32) -> (u32, u32) {
    (lo << 1, (hi << 1) | (lo >> 31))
}

/// Coverage threshold (`0..=4096`) of a layer.
pub fn threshold_units(threshold: f32) -> u32 {
    (f64::from(threshold) * 4096.0).round().clamp(0.0, 4096.0) as u32
}

/// Coverage gain (`q8`) of a layer: `256 / softness`, [`MAX_GAIN`] for softness 0.
pub fn gain_units(softness: f32) -> u32 {
    if softness <= 0.0 {
        return MAX_GAIN;
    }
    (256.0 / f64::from(softness))
        .round()
        .clamp(256.0, f64::from(MAX_GAIN)) as u32
}

/// Coverage from the noise `n` (`0..4096`): `clamp(n + 1 - threshold, 0, 4095) * gain >> 8`,
/// at most [`COVERAGE_ONE`]. Never overflows `u32` (`4095 * 2^20 < 2^32`).
#[inline]
pub const fn coverage_from_noise(n: i32, threshold: u32, gain: u32) -> u32 {
    let d = n + 1 - threshold as i32;
    let d = if d < 0 {
        0
    } else if d > 4095 {
        4095
    } else {
        d as u32
    };
    let c = (d * gain) >> 8;
    if c > COVERAGE_ONE { COVERAGE_ONE } else { c }
}

/// Mixes two octaves: `(2 * v1 + v2) / 3` in fixed point.
#[inline]
pub const fn mix_octaves(v1: i32, v2: i32) -> i32 {
    ((2 * v1 + v2) * 21845) >> 16
}

/// The noise and coverage of one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coverage {
    /// Noise, `0..4096`.
    pub noise: i32,
    /// Coverage, `0..=4096`.
    pub coverage: u32,
}

/// One axis of a lattice point: its words and the weight of the cell inside its step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Axis {
    lo: [u32; 2],
    hi: [u32; 2],
    weight: i32,
}

/// The value noise of one octave from the words of each axis at the cell's lattice point and
/// the next one (`axes[1]` ignored when flat). `cached` is a seed mix and the high words it
/// mixes (the GPU's, for corners without a carry); without it every corner mixes its own.
fn octave(axes: [Axis; 3], seed32: u32, cached: Option<(u32, [u32; 3])>, volume: bool) -> i32 {
    let value = |cx: usize, cy: usize, cz: usize| {
        let hi = [axes[0].hi[cx], axes[1].hi[cy], axes[2].hi[cz]];
        let m = match cached {
            Some((mix, base)) if base == hi => mix,
            _ => seed_mix(seed32, hi),
        };
        lattice_value(m, [axes[0].lo[cx], axes[1].lo[cy], axes[2].lo[cz]], volume)
    };
    let (sx, sy, sz) = (
        smooth(axes[0].weight),
        smooth(axes[1].weight),
        smooth(axes[2].weight),
    );
    if volume {
        let a00 = lerp(value(0, 0, 0), value(1, 0, 0), sx);
        let a10 = lerp(value(0, 1, 0), value(1, 1, 0), sx);
        let a01 = lerp(value(0, 0, 1), value(1, 0, 1), sx);
        let a11 = lerp(value(0, 1, 1), value(1, 1, 1), sx);
        lerp(lerp(a00, a10, sy), lerp(a01, a11, sy), sz)
    } else {
        let a0 = lerp(value(0, 0, 0), value(1, 0, 0), sx);
        let a1 = lerp(value(0, 0, 1), value(1, 0, 1), sx);
        lerp(a0, a1, sz)
    }
}

/// The coverage of the cell `d` cells from the render origin's cell, as the GPU computes it
/// from the split of [`LayerParams`]. `|d|` is at most 4096 cells along each axis (the march
/// never walks farther).
pub fn coverage(p: &LayerParams, d: [i32; 3]) -> Coverage {
    let l = p.log2_scale;
    let volume = p.volume;
    let mut axes1 = [Axis {
        lo: [0; 2],
        hi: [0; 2],
        weight: 0,
    }; 3];
    let mut axes2 = axes1;
    let mut base_hi2 = [0; 3];
    for a in 0..3 {
        if a == 1 && !volume {
            continue;
        }
        let k = p.m0[a] + d[a];
        let num = (k << 12) - p.q[a];
        let li = num >> (12 + l);
        let (lo0, hi0) = add_words(p.lo[a], p.hi[a], li);
        let (lo1, hi1) = add_words(p.lo[a], p.hi[a], li + 1);
        axes1[a] = Axis {
            lo: [lo0, lo1],
            hi: [hi0, hi1],
            weight: (num >> l) & 4095,
        };
        if p.octaves == 2 {
            let (blo, bhi) = double_words(p.lo[a], p.hi[a]);
            base_hi2[a] = bhi;
            let li = num >> (11 + l);
            let (lo0, hi0) = add_words(blo, bhi, li);
            let (lo1, hi1) = add_words(blo, bhi, li + 1);
            axes2[a] = Axis {
                lo: [lo0, lo1],
                hi: [hi0, hi1],
                weight: (num >> (l - 1)) & 4095,
            };
        }
    }
    let v1 = octave(axes1, p.seed32, Some((p.seed_mix, p.hi)), volume);
    let noise = if p.octaves == 2 {
        let v2 = octave(axes2, p.seed32_2, Some((p.seed_mix2, base_hi2)), volume);
        mix_octaves(v1, v2)
    } else {
        v1
    };
    Coverage {
        noise,
        coverage: coverage_from_noise(noise, p.threshold, p.gain),
    }
}

/// The coverage of absolute cell `cell` at fixed tick `tick`, computed directly in `i128` from
/// the cell and the wind: the reference [`coverage`] must equal.
pub fn coverage_direct(layer: &FogLayer, cell: [i64; 3], tick: u64) -> Coverage {
    let volume = layer.noise.dims == NoiseDims::Volume;
    let l = layer.noise.scale.trailing_zeros();
    let seed1 = seed32(layer.noise.seed);
    let seed2 = seed32_octave2(seed1);
    let disp = displacement(layer, tick);
    let mut axes1 = [Axis {
        lo: [0; 2],
        hi: [0; 2],
        weight: 0,
    }; 3];
    let mut axes2 = axes1;
    let words = |v: i128| ((v as u64) as u32, ((v as u64) >> 32) as u32);
    for a in 0..3 {
        if a == 1 && !volume {
            continue;
        }
        let (d_int, q) = disp[a];
        let v = (i128::from(cell[a]) - i128::from(d_int)) * 4096 - i128::from(q);
        let li = v >> (12 + l);
        let ((lo0, hi0), (lo1, hi1)) = (words(li), words(li + 1));
        axes1[a] = Axis {
            lo: [lo0, lo1],
            hi: [hi0, hi1],
            weight: ((v >> l) & 4095) as i32,
        };
        if layer.noise.octaves == 2 {
            let li = v >> (11 + l);
            let ((lo0, hi0), (lo1, hi1)) = (words(li), words(li + 1));
            axes2[a] = Axis {
                lo: [lo0, lo1],
                hi: [hi0, hi1],
                weight: ((v >> (l - 1)) & 4095) as i32,
            };
        }
    }
    let v1 = octave(axes1, seed1, None, volume);
    let noise = if layer.noise.octaves == 2 {
        let v2 = octave(axes2, seed2, None, volume);
        mix_octaves(v1, v2)
    } else {
        v1
    };
    Coverage {
        noise,
        coverage: coverage_from_noise(
            noise,
            threshold_units(layer.threshold),
            gain_units(layer.softness),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowbias32_known_values() {
        // The published constants of lowbias32: 0 maps to 0, and the hash is a bijection on a
        // sample (no two of 1..=4096 collide).
        assert_eq!(lowbias32(0), 0);
        let mut seen: Vec<u32> = (1..=4096).map(lowbias32).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 4096);
    }

    #[test]
    fn smooth_and_lerp_stay_in_range() {
        assert_eq!(smooth(0), 0);
        assert_eq!(smooth(2048), 2048);
        assert!(smooth(4095) <= 4096 && smooth(4095) > 4090);
        // Truncation makes it dip by one here and there; it never leaves 0..4096.
        for w in 1..4096 {
            assert!(smooth(w) >= smooth(w - 1) - 1, "almost monotonic at {w}");
            assert!((0..4096).contains(&smooth(w)));
        }
        for (a, b) in [(0, 4095), (4095, 0), (100, 100), (3000, 17)] {
            for s in [0, 1, 2048, 4095, 4096] {
                let v = lerp(a, b, smooth(s.min(4095)));
                assert!(v >= a.min(b) && v <= a.max(b), "{a} {b} {s}: {v}");
            }
        }
    }

    #[test]
    fn carries_and_borrows_cross_the_high_word() {
        assert_eq!(add_words(u32::MAX, 5, 1), (0, 6));
        assert_eq!(add_words(0, 6, -1), (u32::MAX, 5));
        assert_eq!(add_words(10, 6, -3), (7, 6));
        assert_eq!(add_words(10, 6, 3), (13, 6));
        assert_eq!(add_words(u32::MAX, u32::MAX, 1), (0, 0), "-1 + 1");
        assert_eq!(double_words(0x8000_0001, 3), (2, 7));
    }

    #[test]
    fn coverage_from_noise_limits() {
        // Threshold 0 and softness 0: every cell is full.
        assert_eq!(coverage_from_noise(0, 0, MAX_GAIN), COVERAGE_ONE);
        assert_eq!(coverage_from_noise(4095, 0, MAX_GAIN), COVERAGE_ONE);
        // Threshold 1: none.
        assert_eq!(coverage_from_noise(4095, 4096, MAX_GAIN), 0);
        // Softness 1: the coverage follows the noise.
        assert_eq!(coverage_from_noise(99, 0, 256), 100);
        assert_eq!(gain_units(1.0), 256);
        assert_eq!(gain_units(0.0), MAX_GAIN);
        assert_eq!(threshold_units(0.5), 2048);
        assert_eq!(mix_octaves(4095, 4095), 4094);
    }
}
