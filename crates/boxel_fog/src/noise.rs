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
//!   high word per axis, folded into one word (`lo ^ hi * HI_MIX`), and the three words go
//!   through a chained `lowbias32` from the octave's seed; the value is the top 12 bits. Flat
//!   noise has y words 0, so every height of a column has the same corners.
//! - **Interpolation.** A fixed-point smoothstep of the weight, then seven integer lerps
//!   (trilinear; for [`NoiseDims::Flat`] the y weight is 0 and both y corners are the same, so
//!   it is bilinear in x and z), each product under `2^26`. Two octaves mix as
//!   `(2 * v1 + v2) / 3`.
//! - **Coverage.** A ramp `clamp(n + 1 - threshold, 0, 4095) * gain >> 8`, at most 4096 (one),
//!   through the fixed-point smoothstep: a cell whose noise crosses the threshold starts almost
//!   empty and fills in. The threshold rises with the cell's row in the band
//!   ([`threshold_at`]), and near the band's floor and ceiling it rises to never covered along
//!   their edges ([`edge_weight`], [`raise`]).
//!
//! A cell reads the noise at its min corner; the soft fog near the camera reads it at any point
//! in 1/4096 of a cell ([`noise_at`]), so a cell's value is the soft fog's half a cell before.
//! The edges follow the same rule: a cell row measures its distance to a plane from its face
//! nearest it ([`row_edge`]) and the soft fog from half a cell inside the point
//! ([`point_edge`]), so the middle of a cell has its row's edge.
//!
//! The GPU never sees an absolute coordinate: [`LayerParams`] splits the
//! cell of the render origin into a lattice point (low and high words) and an offset inside its
//! step, and the shader adds the small cell offset of the march with an explicit carry into the
//! high words ([`coverage`]). [`coverage_direct`] computes the same from the absolute cell in
//! `i128`, the reference both are tested against.

use crate::config::{FogLayer, NoiseDims};
use crate::layer::{FogTime, LayerParams, displacement, profile_rows};

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

/// Odd multiplier that folds a coordinate's high word into its low word.
pub const HI_MIX: u32 = 0x9e37_79b1;

/// One word per axis of a lattice coordinate: `lo ^ hi * HI_MIX`.
#[inline]
pub const fn fold(lo: u32, hi: u32) -> u32 {
    lo ^ hi.wrapping_mul(HI_MIX)
}

/// The 12-bit value of the lattice point with folded words `v`, from an octave's seed.
pub const fn lattice_value(seed32: u32, v: [u32; 3]) -> i32 {
    (lowbias32(lowbias32(lowbias32(seed32 ^ v[0]) ^ v[1]) ^ v[2]) >> 20) as i32
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

/// The linear part of the coverage of noise `n` (`0..4096`):
/// `clamp(n + 1 - threshold, 0, 4095) * gain >> 8`, at most [`COVERAGE_ONE`]. Never decreases
/// as the noise grows, and never overflows `u32` (`4095 * 2^20 < 2^32`).
#[inline]
pub const fn ramp(n: i32, threshold: u32, gain: u32) -> u32 {
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

/// The fixed-point smoothstep of a ramp value (`0..=4096`): `smooth` up to one, so a cell
/// starts and ends its fill slowly. 0 below 64.
#[inline]
pub const fn smooth_ramp(w: u32) -> u32 {
    (((w * w) >> 12) * (12288 - 2 * w)) >> 12
}

/// Coverage (`0..=4096`) from the noise `n` (`0..4096`): the [`smooth_ramp`] of its [`ramp`].
/// 0 exactly when the ramp is under 64, so the bound of an empty step holds
/// ([`StepBound::is_empty`]).
#[inline]
pub const fn coverage_from_noise(n: i32, threshold: u32, gain: u32) -> u32 {
    smooth_ramp(ramp(n, threshold, gain))
}

/// The threshold (`0..=8192`) at height `h12` of the band (0 at its bottom, 4096 at its top):
/// `threshold + rise * h^2`, so the base stays wide and flat and the tops thin out.
#[inline]
pub const fn threshold_at(threshold: u32, rise: u32, h12: u32) -> u32 {
    threshold + ((rise * ((h12 * h12) >> 12)) >> 12)
}

/// The height (1/4096 of the band, `0..4096`) of the middle of the cell row `row` rows above the
/// band's bottom, the row clamped into the band's `rows`.
pub fn row_height(row: i64, rows: u32) -> u32 {
    let rows = rows.max(1);
    let r = row.clamp(0, i64::from(rows) - 1) as u32;
    ((2 * r + 1) * 2048) / rows
}

/// The threshold of the cells `dy` rows above the origin's cell: its row's rise
/// ([`rise_threshold`]) raised by the edges ([`row_edge`]).
pub fn row_threshold(p: &LayerParams, dy: i32) -> u32 {
    let threshold = rise_threshold(p, dy);
    if p.edges == [0; 2] {
        return threshold;
    }
    raise(threshold, smooth_ramp(row_edge(p, dy)))
}

/// The threshold the rise gives the cells `dy` rows above the origin's cell (rows outside the
/// band take the nearest row's). Never falls as `dy` grows.
pub fn rise_threshold(p: &LayerParams, dy: i32) -> u32 {
    if p.rise == 0 {
        return p.threshold;
    }
    let row = i64::from(dy) - i64::from(p.band_cells[0]);
    threshold_at(p.threshold, p.rise, row_height(row, p.rows))
}

/// Rows and blocks past which an edge has no weight: deeper than [`MAX_EDGE`]. Distances are
/// clamped to it first, so every product stays under `2^31`.
///
/// [`MAX_EDGE`]: crate::config::MAX_EDGE
pub const EDGE_REACH: i32 = 2048;
const _: () = assert!(crate::config::MAX_EDGE < EDGE_REACH as u32);

/// The weight (`0..=4096`) of an edge `edge` blocks deep at `d12` (1/4096 blocks, at most
/// `2^28`) from its plane: 4096 at the plane and past it (`d12 <= 0`), falling linearly to 0 at
/// `edge` blocks. 0 without an edge.
#[inline]
pub const fn edge_weight(d12: i32, edge: u32) -> u32 {
    if edge == 0 {
        return 0;
    }
    if d12 <= 0 {
        return 4096;
    }
    let w = 4096 - d12 / edge as i32;
    if w < 0 { 0 } else { w as u32 }
}

/// `threshold` raised `s` (`0..=4096`) of the way to 4096, never covered:
/// `threshold + (4096 - threshold) * s`. Never falls as either grows; a threshold of 4096 or
/// more stays as it is.
#[inline]
pub const fn raise(threshold: u32, s: u32) -> u32 {
    threshold + ((4096u32.saturating_sub(threshold) * s) >> 12)
}

/// The edge weight of the cells `dy` rows above the origin's cell: the larger of the bottom's
/// and the top's, each by the distance from its plane to the row's face nearest it (the row
/// along a plane, and any row past it, has 4096). The band cells are exact or clamped far
/// enough that the weight is the same.
pub fn row_edge(p: &LayerParams, dy: i32) -> u32 {
    let [bottom, top] = p.edges;
    let cy = p.cell[1] as i32;
    let below = (dy - p.band_cells[0]).clamp(0, EDGE_REACH);
    let above = (p.band_cells[1] - 1 - dy).clamp(0, EDGE_REACH);
    edge_weight((below * cy) << 12, bottom).max(edge_weight((above * cy) << 12, top))
}

/// The edge weight the soft fog reads at height `y` (relative to the render origin): by the
/// distance to each plane less half a cell, in 1/4096 blocks, so the middle of a cell has its
/// row's [`row_edge`] exactly and the last half cell before a plane has 4096.
pub fn point_edge(p: &LayerParams, y: f32) -> u32 {
    let [bottom, top] = p.edges;
    if bottom == 0 && top == 0 {
        return 0;
    }
    let half = p.cell[1] as f32 * 0.5;
    let d12 = |d: f32| (d.clamp(-1.0, EDGE_REACH as f32) * 4096.0) as i32;
    edge_weight(d12(y - p.band[0] - half), bottom).max(edge_weight(d12(p.band[1] - y - half), top))
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
/// the next one: trilinear over [`corner_values`].
fn octave(axes: [Axis; 3], seed32: u32) -> i32 {
    let c = corner_values(axes, seed32);
    let (sx, sy, sz) = (
        smooth(axes[0].weight),
        smooth(axes[1].weight),
        smooth(axes[2].weight),
    );
    let a00 = lerp(c[0], c[1], sx);
    let a10 = lerp(c[2], c[3], sx);
    let a01 = lerp(c[4], c[5], sx);
    let a11 = lerp(c[6], c[7], sx);
    lerp(lerp(a00, a10, sy), lerp(a01, a11, sy), sz)
}

/// The coverage of the cell `d` cells from the render origin's cell, as the GPU computes it
/// from the split of [`LayerParams`]: the noise at its min corner and the threshold of its row.
/// `|d|` is at most 4096 cells along each axis (the march never walks farther).
pub fn coverage(p: &LayerParams, d: [i32; 3]) -> Coverage {
    let noise = noise_at(p, d, [0; 3]);
    Coverage {
        noise,
        coverage: coverage_from_noise(noise, row_threshold(p, d[1]), p.gain),
    }
}

/// The noise (`0..4096`) at `sub` (1/4096 of a cell per axis, `0..4096`) past the min corner of
/// the cell `d` cells from the origin's cell: what the soft fog reads at any point. Flat noise
/// ignores `sub[1]`.
pub fn noise_at(p: &LayerParams, d: [i32; 3], sub: [i32; 3]) -> i32 {
    let l = p.log2_scale;
    let volume = p.volume;
    let mut axes1 = [Axis {
        lo: [0; 2],
        hi: [0; 2],
        weight: 0,
    }; 3];
    let mut axes2 = axes1;
    for a in 0..3 {
        if a == 1 && !volume {
            continue;
        }
        let k = p.m0[a] + d[a];
        let num = (k << 12) - p.q[a] + sub[a];
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
    let v1 = octave(axes1, p.seed32);
    if p.octaves == 2 {
        let v2 = octave(axes2, p.seed32_2);
        mix_octaves(v1, v2)
    } else {
        v1
    }
}

/// The first octave's lattice step around a cell: every cell whose offset lies in `[lo, hi)`
/// along each axis (unbounded along y for flat noise) interpolates the same corners, and value
/// noise never leaves the range of its corners, so none of them has more noise than
/// `max_noise` (with two octaves the second one is bounded by 4095). The march skips a whole
/// step when even that leaves no coverage at the lowest threshold of the step's rows
/// ([`StepBound::is_empty`]). It bounds the cells' noise, not the soft fog's, which reads
/// points up to half a cell outside the step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepBound {
    /// First cell offset of the step per axis.
    pub lo: [i32; 3],
    /// One past its last cell offset per axis.
    pub hi: [i32; 3],
    /// The most noise a cell of the step can have.
    pub max_noise: i32,
}

impl StepBound {
    /// True if no cell of the step can have coverage at [`StepBound::threshold`].
    pub fn is_empty(&self, p: &LayerParams) -> bool {
        coverage_from_noise(self.max_noise, self.threshold(p), p.gain) == 0
    }

    /// A threshold no cell of the step is under ([`row_threshold`]). With flat noise the step
    /// has every row: the band's bottom threshold. With 3D noise, the rise of its lowest row
    /// raised by the least edge weight its rows can have (the bottom edge's at its highest row,
    /// the top edge's at its lowest), less one for the smoothstep's dips of 1. Edges and the
    /// rise only ever skip more steps, never one with coverage.
    pub fn threshold(&self, p: &LayerParams) -> u32 {
        if !p.volume {
            return p.threshold;
        }
        let (low, high) = (self.lo[1], self.hi[1] - 1);
        let threshold = rise_threshold(p, low);
        if p.edges == [0; 2] {
            return threshold;
        }
        let cy = p.cell[1] as i32;
        let below = (high - p.band_cells[0]).clamp(0, EDGE_REACH);
        let above = (p.band_cells[1] - 1 - low).clamp(0, EDGE_REACH);
        let w = edge_weight((below * cy) << 12, p.edges[0])
            .max(edge_weight((above * cy) << 12, p.edges[1]));
        raise(threshold, smooth_ramp(w).saturating_sub(1))
    }
}

/// The [`StepBound`] of the first octave around cell `d` (as `fog.wgsl` computes it from the
/// corners it caches).
pub fn step_bound(p: &LayerParams, d: [i32; 3]) -> StepBound {
    let l = p.log2_scale;
    let mut lo = [i32::MIN; 3];
    let mut hi = [i32::MAX; 3];
    let mut axes = [Axis {
        lo: [0; 2],
        hi: [0; 2],
        weight: 0,
    }; 3];
    for a in 0..3 {
        if a == 1 && !p.volume {
            continue;
        }
        let k = p.m0[a] + d[a];
        let num = (k << 12) - p.q[a];
        let li = num >> (12 + l);
        // Cells k with ((k << 12) - q) >> (12 + l) == li: from L * li (one more with a wind
        // fraction) for L cells.
        lo[a] = (li << l) + i32::from(p.q[a] > 0) - p.m0[a];
        hi[a] = lo[a] + (1 << l);
        let (lo0, hi0) = add_words(p.lo[a], p.hi[a], li);
        let (lo1, hi1) = add_words(p.lo[a], p.hi[a], li + 1);
        axes[a] = Axis {
            lo: [lo0, lo1],
            hi: [hi0, hi1],
            weight: 0,
        };
    }
    let max = corner_values(axes, p.seed32).into_iter().max().unwrap_or(0);
    StepBound {
        lo,
        hi,
        max_noise: if p.octaves == 2 {
            mix_octaves(max, 4095)
        } else {
            max
        },
    }
}

/// The 8 corner values of a lattice step, x fastest, then y, then z (flat noise: the two y
/// corners are the same point).
fn corner_values(axes: [Axis; 3], seed32: u32) -> [i32; 8] {
    let v: [[u32; 2]; 3] = axes.map(|a| [fold(a.lo[0], a.hi[0]), fold(a.lo[1], a.hi[1])]);
    std::array::from_fn(|i| lattice_value(seed32, [v[0][i & 1], v[1][(i >> 1) & 1], v[2][i >> 2]]))
}

/// The coverage of absolute cell `cell` at `time`, computed directly in `i128` from the cell,
/// its row in the band, the band's planes and the wind: the reference [`coverage`] must equal.
pub fn coverage_direct(layer: &FogLayer, cell: [i64; 3], time: impl Into<FogTime>) -> Coverage {
    let noise = noise_direct(layer, cell, [0; 3], time);
    let cy = i64::from(layer.cell[1].max(1));
    let (bottom, top) = (layer.min_y.div_euclid(cy), layer.max_y.div_euclid(cy));
    let row = cell[1].saturating_sub(bottom);
    let rise = threshold_at(
        threshold_units(layer.threshold),
        threshold_units(layer.threshold_rise),
        row_height(row, profile_rows(layer)),
    );
    // Whole rows from each plane to the row's nearer face, then blocks in 1/4096.
    let reach = |rows: i128| (rows.clamp(0, i128::from(EDGE_REACH)) * i128::from(cy)) << 12;
    let below = reach(i128::from(cell[1]) - i128::from(bottom));
    let above = reach(i128::from(top) - 1 - i128::from(cell[1]));
    let w =
        edge_weight(below as i32, layer.edge_bottom).max(edge_weight(above as i32, layer.edge_top));
    let threshold = raise(rise, smooth_ramp(w));
    Coverage {
        noise,
        coverage: coverage_from_noise(noise, threshold, gain_units(layer.softness)),
    }
}

/// The noise at `sub` (1/4096 of a cell) past the min corner of absolute cell `cell` at `time`,
/// directly in `i128`: the reference of [`noise_at`].
pub fn noise_direct(
    layer: &FogLayer,
    cell: [i64; 3],
    sub: [i32; 3],
    time: impl Into<FogTime>,
) -> i32 {
    let volume = layer.noise.dims == NoiseDims::Volume;
    let l = layer.noise.scale.trailing_zeros();
    let seed1 = seed32(layer.noise.seed);
    let seed2 = seed32_octave2(seed1);
    let disp = displacement(layer, time);
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
        let v =
            (i128::from(cell[a]) - i128::from(d_int)) * 4096 - i128::from(q) + i128::from(sub[a]);
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
    let v1 = octave(axes1, seed1);
    if layer.noise.octaves == 2 {
        let v2 = octave(axes2, seed2);
        mix_octaves(v1, v2)
    } else {
        v1
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
        // Softness 1: the ramp follows the noise, the coverage its smoothstep.
        assert_eq!(ramp(99, 0, 256), 100);
        assert_eq!(coverage_from_noise(2047, 0, 256), 2048);
        assert_eq!(coverage_from_noise(4095, 0, 512), COVERAGE_ONE);
        assert_eq!(gain_units(1.0), 256);
        assert_eq!(gain_units(0.0), MAX_GAIN);
        assert_eq!(threshold_units(0.5), 2048);
        assert_eq!(mix_octaves(4095, 4095), 4094);
    }

    /// The ramp never falls as the noise grows, the smoothstep stays in `0..=4096`, starts and
    /// ends flat, and is 0 exactly under 64 (what the empty-step bound relies on).
    #[test]
    fn the_coverage_ramp_is_smooth() {
        for gain in [256, 300, 1024, MAX_GAIN] {
            for threshold in [0, 100, 2048, 4096, 8192] {
                for n in 1..4096 {
                    assert!(ramp(n, threshold, gain) >= ramp(n - 1, threshold, gain));
                }
            }
        }
        assert_eq!(smooth_ramp(0), 0);
        assert_eq!(smooth_ramp(4096), COVERAGE_ONE);
        for w in 0..=4096u32 {
            assert!(smooth_ramp(w) <= COVERAGE_ONE);
            assert_eq!(smooth_ramp(w) == 0, w < 64, "{w}");
        }
        // Flat ends: the first and last 5 % of the ramp move the coverage by under 1 %.
        assert!(smooth_ramp(205) < 41);
        assert!(smooth_ramp(4096 - 205) > 4096 - 41);
    }

    /// The edge weight falls linearly from 4096 at the plane to 0 at the edge's depth; the
    /// raised threshold reaches never covered at 4096 and never falls.
    #[test]
    fn edges_raise_the_threshold_to_never_covered() {
        assert_eq!(edge_weight(0, 0), 0, "no edge");
        assert_eq!(edge_weight(-5, 8), 4096, "past the plane");
        assert_eq!(edge_weight(0, 8), 4096, "at the plane");
        assert_eq!(edge_weight(2 << 12, 8), 3072);
        assert_eq!(edge_weight(4 << 12, 8), 2048);
        assert_eq!(edge_weight(8 << 12, 8), 0);
        assert_eq!(edge_weight(1 << 28, 1), 0, "deep inside");
        for edge in [1, 3, 8, 100, 1024] {
            for d in 1..(4 << 12) {
                assert!(
                    edge_weight(d, edge) <= edge_weight(d - 1, edge),
                    "{edge} {d}"
                );
            }
        }
        assert_eq!(raise(1000, 0), 1000);
        assert_eq!(raise(1000, 4096), 4096);
        assert_eq!(raise(1000, 2048), 2548);
        assert_eq!(raise(5000, 4096), 5000, "already never covered");
        // Never falls as the threshold or the raise grows.
        for s in (0..=4096).step_by(7) {
            for t in 1..=8192 {
                assert!(raise(t, s) >= raise(t - 1, s), "{t} {s}");
            }
        }
        for t in (0..=8192).step_by(5) {
            for s in 1..=4096 {
                assert!(raise(t, s) >= raise(t, s - 1), "{t} {s}");
            }
        }
        // At the plane nothing is covered, whatever the noise and the softness.
        for gain in [256, 1024, MAX_GAIN] {
            for t in [0, 1000, 4000] {
                assert_eq!(
                    coverage_from_noise(4095, raise(t, smooth_ramp(4096)), gain),
                    0
                );
            }
        }
    }

    /// The smoothstep dips by at most one: a lower bound of the edge's raise is the smoothstep
    /// of the least weight less one (what the empty-step bound uses).
    #[test]
    fn the_smoothstep_dips_by_one_at_most() {
        let mut least_after = u32::MAX;
        for w in (0..=4096u32).rev() {
            least_after = least_after.min(smooth_ramp(w));
            assert!(least_after + 1 >= smooth_ramp(w), "{w}");
        }
    }

    #[test]
    fn the_threshold_rises_with_the_square_of_the_height() {
        assert_eq!(threshold_at(1000, 2048, 0), 1000);
        assert_eq!(threshold_at(1000, 2048, 4096), 3048);
        assert_eq!(
            threshold_at(1000, 2048, 2048),
            1512,
            "a quarter at half height"
        );
        assert_eq!(threshold_at(4096, 4096, 4096), 8192, "never overflows");
        // Four rows: the middles of each, clamped into the band.
        let rows: Vec<u32> = (-2..6).map(|r| row_height(r, 4)).collect();
        assert_eq!(rows, [512, 512, 512, 1536, 2560, 3584, 3584, 3584]);
        assert_eq!(row_height(65535, 65536), 4095);
        assert_eq!(row_height(0, 0), 2048, "no rows count as one");
    }
}
