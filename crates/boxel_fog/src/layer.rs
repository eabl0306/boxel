//! What the GPU gets of a layer each frame: everything absolute reduced to small integers
//! relative to the render origin, computed here in `i64` and `i128` (ADR 0014).

use crate::config::{FogLayer, MAX_PROFILE_ROWS, NoiseDims};
use crate::noise::{self, COVERAGE_ONE, gain_units, seed32, seed32_octave2, threshold_units};

/// Fixed steps a second of the fog clock (the app's fixed rate).
pub const TICKS_PER_SECOND: u32 = 64;

/// A moment of the fog clock: whole fixed steps (`tick`, 64 a second) and the fraction of the
/// next one (`subtick`, in 1/65536), so the wind moves the noise every frame and not only once a
/// tick. Integers only: every machine and tier sees the same cells at the same moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FogTime {
    /// Whole ticks.
    pub tick: u64,
    /// Fraction of the next tick, in 1/65536.
    pub subtick: u16,
}

impl FogTime {
    /// `tick` and `subtick / 65536` of the next one.
    pub const fn new(tick: u64, subtick: u16) -> Self {
        Self { tick, subtick }
    }

    /// The moment in 1/65536 ticks.
    pub fn units(self) -> i128 {
        (i128::from(self.tick) << 16) | i128::from(self.subtick)
    }
}

impl From<u64> for FogTime {
    /// A whole tick.
    fn from(tick: u64) -> Self {
        Self { tick, subtick: 0 }
    }
}
/// Bands are clamped to this far from the render origin, where `f32` is still exact.
pub const BAND_LIMIT: i64 = 1 << 24;
/// Band cell indices are clamped to this far from the origin's cell.
pub const BAND_CELL_LIMIT: i64 = 1 << 30;

/// Wind in 1/65536 blocks per tick: `round(wind * 65536 / 64)`.
pub fn wind_units(wind: f32) -> i64 {
    (f64::from(wind) * 65536.0 / f64::from(TICKS_PER_SECOND)).round() as i64
}

/// The wind displacement of a layer at `time`, per axis: the whole cells (wrapping, `i64`) and
/// the fraction of a cell in 1/4096 (`0..4096`). Flat noise ignores the wind along y.
pub fn displacement(layer: &FogLayer, time: impl Into<FogTime>) -> [(i64, i32); 3] {
    let units = time.into().units();
    std::array::from_fn(|a| {
        if a == 1 && layer.noise.dims == NoiseDims::Flat {
            return (0, 0);
        }
        // 1/65536 cells, floored: 1/65536 blocks a tick times 1/65536 ticks, over the cell.
        let q16 = (i128::from(wind_units(layer.wind[a])) * units)
            .div_euclid(i128::from(layer.cell[a]) << 16);
        ((q16 >> 16) as i64, ((q16 & 0xffff) >> 4) as i32)
    })
}

/// Cell rows of a layer's band, as its threshold profile counts them: at least 1, at most
/// [`MAX_PROFILE_ROWS`] (more is only valid without a rise, which ignores the rows).
pub fn profile_rows(layer: &FogLayer) -> u32 {
    let cy = i128::from(layer.cell[1].max(1));
    ((i128::from(layer.max_y) - i128::from(layer.min_y)) / cy)
        .clamp(1, i128::from(MAX_PROFILE_ROWS)) as u32
}

/// One layer as the GPU marches it this frame, for a render origin and a tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerParams {
    /// The cell of the render origin (`floor(origin / cell)`).
    pub c0: [i64; 3],
    /// Min corner of cell `c0` relative to the render origin, in blocks: in `(-cell, 0]`.
    pub grid: [f32; 3],
    /// Cell size in blocks.
    pub cell: [u32; 3],
    /// 3D noise.
    pub volume: bool,
    /// 1 or 2.
    pub octaves: u32,
    /// `log2` of the noise scale.
    pub log2_scale: u32,
    /// Offset of the displaced origin cell inside its lattice step, `0..scale`.
    pub m0: [i32; 3],
    /// Fraction of a cell the wind moved past the whole cells, `0..4096`.
    pub q: [i32; 3],
    /// Low words of the displaced origin cell's lattice point.
    pub lo: [u32; 3],
    /// High words of it.
    pub hi: [u32; 3],
    /// Seed of the first octave.
    pub seed32: u32,
    /// Seed of the second octave.
    pub seed32_2: u32,
    /// Coverage threshold at the bottom of the band, `0..=4096`.
    pub threshold: u32,
    /// Rise of the threshold to the top of the band, `0..=4096` ([`noise::threshold_at`]).
    pub rise: u32,
    /// Cell rows of the band ([`profile_rows`]).
    pub rows: u32,
    /// Depth of the bottom and the top edge, in blocks ([`noise::row_edge`]).
    pub edges: [u32; 2],
    /// Coverage gain (`q8`).
    pub gain: u32,
    /// Band relative to the render origin's y (clamped to `+-2^24`).
    pub band: [f32; 2],
    /// Band in cells relative to `c0[1]`, `[first, end)` (clamped to `+-2^30`).
    pub band_cells: [i32; 2],
    /// Extinction per block of a full cell, times `log2(e)` (the GPU uses `exp2`).
    pub density_log2e: f32,
}

impl LayerParams {
    /// The layer for `render_origin` (world blocks) at `time`.
    pub fn new(layer: &FogLayer, render_origin: [i64; 3], time: impl Into<FogTime>) -> Self {
        let volume = layer.noise.dims == NoiseDims::Volume;
        let l = layer.noise.scale.max(1).trailing_zeros();
        let cell = layer.cell.map(|c| c.max(1));
        let c0: [i64; 3] = std::array::from_fn(|a| render_origin[a].div_euclid(i64::from(cell[a])));
        let grid = std::array::from_fn(|a| (c0[a] * i64::from(cell[a]) - render_origin[a]) as f32);
        let disp = displacement(layer, time);
        let mut m0 = [0; 3];
        let mut q = [0; 3];
        let mut lo = [0; 3];
        let mut hi = [0; 3];
        for a in 0..3 {
            if a == 1 && !volume {
                continue;
            }
            let (d_int, frac) = disp[a];
            let b = c0[a].wrapping_sub(d_int);
            let l0 = b >> l;
            m0[a] = (b & ((1 << l) - 1)) as i32;
            q[a] = frac;
            lo[a] = l0 as u64 as u32;
            hi[a] = ((l0 as u64) >> 32) as u32;
        }
        let s1 = seed32(layer.noise.seed);
        let s2 = seed32_octave2(s1);
        let band_rel = |y: i64| {
            (i128::from(y) - i128::from(render_origin[1]))
                .clamp(-i128::from(BAND_LIMIT), i128::from(BAND_LIMIT)) as f32
        };
        let cy = i64::from(cell[1]);
        let band_cell = |y: i64| {
            (i128::from(y.div_euclid(cy)) - i128::from(c0[1]))
                .clamp(-i128::from(BAND_CELL_LIMIT), i128::from(BAND_CELL_LIMIT)) as i32
        };
        Self {
            c0,
            grid,
            cell,
            volume,
            octaves: u32::from(layer.noise.octaves.clamp(1, 2)),
            log2_scale: l,
            m0,
            q,
            lo,
            hi,
            seed32: s1,
            seed32_2: s2,
            threshold: threshold_units(layer.threshold),
            rise: threshold_units(layer.threshold_rise),
            rows: profile_rows(layer),
            edges: [layer.edge_bottom, layer.edge_top],
            gain: gain_units(layer.softness),
            band: [band_rel(layer.min_y), band_rel(layer.max_y)],
            band_cells: [band_cell(layer.min_y), band_cell(layer.max_y)],
            density_log2e: layer.density * std::f32::consts::LOG2_E,
        }
    }
}

/// Knots of a [`HazeProfile`].
pub const HAZE_KNOTS: usize = 8;

/// The haze of a layer ([`FogLayer::haze`]): its mean extinction at 8 heights spread evenly over
/// the rows that can hold fog, linear between them and none outside. The row along a plane with
/// an edge is always empty ([`noise::row_edge`]), so with an edge the haze starts a cell inside,
/// like the cells; without one it is cut at the plane. Built on the CPU from the integer noise
/// ([`haze_profile`]): the rise and the edges thin it like they thin the cells.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HazeProfile {
    /// Height of the first knot over the band's bottom (`min_y`), in blocks.
    pub bottom: f32,
    /// Blocks between knots (0: no row can hold fog, no haze).
    pub spacing: f32,
    /// Mean extinction per block (`log2` units) at each knot, at a haze of 1.
    pub knots: [f32; HAZE_KNOTS],
}

impl HazeProfile {
    /// The rows of `layer` that can hold fog with the same mean coverage (0 to 1) everywhere.
    pub fn level(layer: &FogLayer, mean: f32) -> Self {
        let (bottom, spacing) = haze_span(layer);
        Self {
            bottom,
            spacing,
            knots: [layer.density * std::f32::consts::LOG2_E * mean; HAZE_KNOTS],
        }
    }

    /// Extinction per block (`log2` units, at a haze of 1) at height `y`, the first knot at
    /// height `lo` (both in the same frame): linear between knots, 0 below the first and from
    /// the last one up.
    pub fn at(&self, lo: f32, y: f32) -> f32 {
        if self.spacing <= 0.0 {
            return 0.0;
        }
        let u = (y - lo) / self.spacing;
        if !(0.0..(HAZE_KNOTS - 1) as f32).contains(&u) {
            return 0.0;
        }
        self.at_interval(lo, u.floor() as usize, y)
    }

    /// Extinction per block at height `y` by the knots `k` and `k + 1` (`k < 7`), the first knot
    /// at `lo`: their line, clamped to its ends.
    pub fn at_interval(&self, lo: f32, k: usize, y: f32) -> f32 {
        let y0 = lo + k as f32 * self.spacing;
        let w = ((y - y0) / self.spacing).clamp(0.0, 1.0);
        self.knots[k] + (self.knots[k + 1] - self.knots[k]) * w
    }
}

/// The rows of a layer that can hold fog, `[lo, hi)` in blocks: the band less the row along
/// each plane with an edge. `None` if no row can.
fn coverable(layer: &FogLayer) -> Option<(i128, i128)> {
    let cy = i128::from(layer.cell[1].max(1));
    let lo = i128::from(layer.min_y) + if layer.edge_bottom > 0 { cy } else { 0 };
    let hi = i128::from(layer.max_y) - if layer.edge_top > 0 { cy } else { 0 };
    (lo < hi).then_some((lo, hi))
}

/// [`HazeProfile::bottom`] and [`HazeProfile::spacing`] of a layer (0 and 0 if no row can hold
/// fog).
fn haze_span(layer: &FogLayer) -> (f32, f32) {
    coverable(layer).map_or((0.0, 0.0), |(lo, hi)| {
        let spacing = (hi - lo) as f64 / (HAZE_KNOTS - 1) as f64;
        ((lo - i128::from(layer.min_y)) as f32, spacing as f32)
    })
}

/// The [`HazeProfile`] of a layer: at each knot, its density times the mean coverage of the cell
/// row that holds it (the top knot takes the row under it), over a fixed sample of 1024 cells
/// spread over many lattice steps, read at that row's threshold. Every knot reads the same
/// noise, so a layer without edges and rise has the same mean at every knot. Deterministic; the
/// same for every origin and tick (on average).
pub fn haze_profile(layer: &FogLayer) -> HazeProfile {
    let (bottom, spacing) = haze_span(layer);
    let mut profile = HazeProfile {
        bottom,
        spacing,
        knots: [0.0; HAZE_KNOTS],
    };
    let Some((lo, hi)) = coverable(layer) else {
        return profile;
    };
    let params = LayerParams::new(layer, [0; 3], 0);
    let step = 3 * layer.noise.scale.max(1) as i32 + 1;
    let noise: Vec<i32> = (0..1024i32)
        .map(|i| {
            let d = [
                (i & 31) * step + (i * 7) % step,
                ((i * 5) % 16) * step + (i * 3) % step,
                (i >> 5) * step + (i * 11) % step,
            ];
            noise::noise_at(&params, d, [0; 3])
        })
        .collect();
    let cy = i128::from(layer.cell[1].max(1));
    let rows = (lo.div_euclid(cy), hi.div_euclid(cy));
    for (k, knot) in profile.knots.iter_mut().enumerate() {
        // The row that holds the knot's height (the top knot's: the one under it).
        let y = lo as f64 + k as f64 * (hi - lo) as f64 / (HAZE_KNOTS - 1) as f64;
        let row = (y.floor() as i128).div_euclid(cy).clamp(rows.0, rows.1 - 1);
        // The threshold of that row, from an origin in it (so its row index is small).
        let at_row = LayerParams::new(layer, [0, (row * cy) as i64, 0], 0);
        let threshold = noise::row_threshold(&at_row, 0);
        let sum: u64 = noise
            .iter()
            .map(|&n| u64::from(noise::coverage_from_noise(n, threshold, at_row.gain)))
            .sum();
        let mean = (sum as f64 / (1024.0 * f64::from(COVERAGE_ONE))) as f32;
        *knot = params.density_log2e * mean;
    }
    profile
}

/// The camera ray of every pixel, linear in normalized device coordinates: the direction from
/// the eye to the point at view depth 1 under NDC `(x, y)` is `ray0 + x * ray_x + y * ray_y`
/// (so `dot(dir, forward) = 1` and a ray's parameter is the view depth: `near / depth` for a
/// reverse-Z infinite projection).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayBasis {
    /// Direction at NDC `(0, 0)`.
    pub ray0: [f32; 3],
    /// Change of the direction per NDC unit along x.
    pub ray_x: [f32; 3],
    /// Change per NDC unit along y.
    pub ray_y: [f32; 3],
    /// Near plane distance (the depth buffer holds `near / view depth`).
    pub near: f32,
}

impl RayBasis {
    /// From a camera-relative, reverse-Z infinite view-projection (column-major, as
    /// `Mat4::to_cols_array_2d`) and the eye relative to the render origin, in `f64`.
    /// `None` if the matrix is singular.
    pub fn new(view_proj: [[f32; 4]; 4], eye: [f32; 3]) -> Option<Self> {
        let m: [[f64; 4]; 4] = view_proj.map(|c| c.map(f64::from));
        let inv = invert(m)?;
        // Clip z is `near` for every point (the matrix's z row is (0, 0, 0, near)), so NDC z
        // `near` is view depth 1.
        let near = m[3][2];
        let unproject = |x: f64, y: f64| {
            let clip = [x, y, near, 1.0];
            let p: [f64; 4] =
                std::array::from_fn(|r| (0..4).map(|c| inv[c][r] * clip[c]).sum::<f64>());
            std::array::from_fn::<f64, 3, _>(|a| p[a] / p[3] - f64::from(eye[a]))
        };
        let (o, x, y) = (
            unproject(0.0, 0.0),
            unproject(1.0, 0.0),
            unproject(0.0, 1.0),
        );
        Some(Self {
            ray0: o.map(|v| v as f32),
            ray_x: std::array::from_fn(|a| (x[a] - o[a]) as f32),
            ray_y: std::array::from_fn(|a| (y[a] - o[a]) as f32),
            near: near as f32,
        })
    }

    /// The ray direction at NDC `(x, y)`.
    pub fn dir(&self, x: f32, y: f32) -> [f32; 3] {
        std::array::from_fn(|a| self.ray0[a] + x * self.ray_x[a] + y * self.ray_y[a])
    }
}

/// Inverse of a column-major 4x4 matrix (Gauss-Jordan with partial pivoting).
fn invert(m: [[f64; 4]; 4]) -> Option<[[f64; 4]; 4]> {
    // Row-major working copy: a[r][c] = m[c][r].
    let mut a: [[f64; 8]; 4] = std::array::from_fn(|r| {
        std::array::from_fn(|c| {
            if c < 4 {
                m[c][r]
            } else {
                f64::from(u8::from(c - 4 == r))
            }
        })
    });
    for col in 0..4 {
        let pivot = (col..4).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-300 {
            return None;
        }
        a.swap(col, pivot);
        let p = a[col][col];
        for v in &mut a[col] {
            *v /= p;
        }
        for r in 0..4 {
            if r != col {
                let f = a[r][col];
                if f != 0.0 {
                    let row = a[col];
                    for (v, w) in a[r].iter_mut().zip(row) {
                        *v -= f * w;
                    }
                }
            }
        }
    }
    Some(std::array::from_fn(|c| {
        std::array::from_fn(|r| a[r][c + 4])
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FogLayer;

    #[test]
    fn grid_corner_is_in_the_cell_below_the_origin() {
        let mut layer = FogLayer::new(-12, 12);
        layer.cell = [3, 4, 32];
        for origin in [
            [0, 0, 0],
            [-1, -5, -33],
            [7, 9, 31],
            [i64::MAX / 4, -(1 << 61), (1 << 61) - 1],
        ] {
            let p = LayerParams::new(&layer, origin, 0);
            for a in 0..3 {
                let c = layer.cell[a] as f32;
                assert!(
                    p.grid[a] <= 0.0 && p.grid[a] > -c,
                    "{origin:?} {a}: {}",
                    p.grid[a]
                );
                assert_eq!(
                    i128::from(p.c0[a]) * i128::from(layer.cell[a]) - i128::from(origin[a]),
                    p.grid[a] as i128
                );
            }
        }
    }

    #[test]
    fn bands_are_relative_and_clamped() {
        let mut layer = FogLayer::new(64, 80);
        layer.cell = [2, 4, 2];
        let p = LayerParams::new(&layer, [0, 70, 0], 0);
        assert_eq!(p.band, [-6.0, 10.0]);
        // Cell 17 holds y 68..72: the band is cells 16..20.
        assert_eq!(p.c0[1], 17);
        assert_eq!(p.band_cells, [-1, 3]);
        let far = LayerParams::new(&layer, [0, -(1 << 60), 0], 0);
        assert_eq!(far.band, [BAND_LIMIT as f32, BAND_LIMIT as f32]);
        assert_eq!(far.band_cells, [1 << 30, 1 << 30]);
        let below = LayerParams::new(&layer, [0, 1 << 60, 0], 0);
        assert_eq!(below.band, [-(BAND_LIMIT as f32); 2]);
    }

    #[test]
    fn wind_units_and_displacement() {
        assert_eq!(wind_units(1.0), 1024);
        assert_eq!(wind_units(-2.5), -2560);
        let mut layer = FogLayer::new(0, 4);
        layer.cell = [4, 4, 4];
        layer.wind = [2.0, 1.0, -1.0];
        // 64 ticks = 1 s: 2 blocks = half a cell along x; flat noise ignores y.
        let d = displacement(&layer, 64);
        assert_eq!(d[0], (0, 2048));
        assert_eq!(d[1], (0, 0));
        assert_eq!(d[2], (-1, 3072));
        layer.noise.dims = NoiseDims::Volume;
        assert_eq!(displacement(&layer, 64)[1], (0, 1024));
    }

    /// A valley-like bank: 2-block cells over 46..70 with edges of 4 and 12 blocks.
    fn valley() -> FogLayer {
        let mut l = FogLayer::new(46, 70);
        l.cell = [2, 2, 2];
        l.noise.dims = NoiseDims::Volume;
        l.noise.scale = 8;
        l.noise.octaves = 2;
        l.noise.seed = 0xfa11e7;
        l.threshold = 0.45;
        (l.edge_bottom, l.edge_top) = (4, 12);
        l.softness = 0.3;
        l.density = 0.075;
        l
    }

    /// The haze spans the rows that can hold fog: none in the row along a plane with an edge
    /// (the valley's 46 to 48, under the sea's surface at 47), some in its core.
    #[test]
    fn the_haze_spans_the_rows_that_can_hold_fog() {
        let layer = valley();
        let p = haze_profile(&layer);
        assert_eq!((p.bottom, p.spacing), (2.0, 20.0 / 7.0));
        let lo = layer.min_y as f32 + p.bottom;
        for y in [30.0, 46.0, 47.0, 47.99, 68.0, 69.0, 70.0, 90.0] {
            assert_eq!(p.at(lo, y), 0.0, "y {y}");
        }
        for y in [48.0, 50.0, 55.0, 60.0] {
            assert!(p.at(lo, y) > 0.0, "y {y}: {p:?}");
        }
        assert!(p.knots.iter().all(|&k| k >= 0.0));
        // The top edge thins it toward the ceiling.
        assert!(p.knots[7] < p.knots[4], "{p:?}");
        // Between knots, linear.
        let mid = 0.5 * (p.knots[2] + p.knots[3]);
        assert!((p.at(lo, lo + 2.5 * p.spacing) - mid).abs() < 1e-6);
        // No row at all: no haze.
        let mut thin = valley();
        thin.max_y = 50;
        assert_eq!(haze_profile(&thin), HazeProfile::default());
    }

    /// Without an edge the haze goes up to the plane and stops there, like the cells.
    #[test]
    fn without_an_edge_the_haze_reaches_the_plane() {
        let mut layer = valley();
        (layer.edge_bottom, layer.edge_top) = (0, 0);
        let p = haze_profile(&layer);
        assert_eq!((p.bottom, p.spacing), (0.0, 24.0 / 7.0));
        let lo = layer.min_y as f32;
        assert!(p.knots[0] > 0.0 && p.knots[7] > 0.0, "{p:?}");
        assert_eq!(p.at(lo, lo), p.knots[0]);
        assert_eq!(p.at(lo, lo - 0.01), 0.0);
        assert_eq!(p.at(lo, 70.0), 0.0, "the band is [min_y, max_y)");
        assert!(p.at(lo, 69.99) > 0.0);
    }

    /// Without edges or a rise every knot has the same mean, from 0 (threshold 1) to the full
    /// density (threshold 0); a rise makes the base denser than the top.
    #[test]
    fn the_profile_follows_the_threshold_and_its_rise() {
        let mut layer = valley();
        (layer.edge_bottom, layer.edge_top) = (0, 0);
        let d = layer.density * std::f32::consts::LOG2_E;
        let level = haze_profile(&layer);
        assert!(
            level.knots.iter().all(|&k| k == level.knots[0]),
            "{level:?}"
        );
        assert!((0.25 * d..0.75 * d).contains(&level.knots[0]), "{level:?}");
        assert_eq!(
            HazeProfile::level(&layer, level.knots[0] / d).knots[3],
            level.knots[0] / d * d
        );
        (layer.threshold, layer.softness) = (0.0, 0.0);
        assert_eq!(
            haze_profile(&layer).knots,
            [d; HAZE_KNOTS],
            "every cell full"
        );
        layer.threshold = 1.0;
        assert_eq!(haze_profile(&layer).knots, [0.0; HAZE_KNOTS]);
        (layer.threshold, layer.softness) = (0.45, 0.3);
        layer.threshold_rise = 0.3;
        let rise = haze_profile(&layer);
        assert!(
            rise.knots.windows(2).all(|w| w[0] >= w[1]) && rise.knots[0] > rise.knots[7],
            "{rise:?}"
        );
    }

    /// The profile is deterministic and the same wherever the band is (the noise is read at
    /// the same cells, the thresholds by the rows of the band).
    #[test]
    fn the_profile_is_deterministic() {
        let layer = valley();
        let p = haze_profile(&layer);
        assert_eq!(haze_profile(&layer), p);
        for shift in [-(1i64 << 40), -1024, 2048, 1 << 50] {
            let mut moved = valley();
            (moved.min_y, moved.max_y) = (layer.min_y + shift, layer.max_y + shift);
            assert_eq!(haze_profile(&moved), p, "{shift}");
        }
    }

    fn mat_mul(a: [[f64; 4]; 4], b: [[f64; 4]; 4]) -> [[f64; 4]; 4] {
        std::array::from_fn(|c| std::array::from_fn(|r| (0..4).map(|k| a[k][r] * b[c][k]).sum()))
    }

    /// A reverse-Z infinite projection times a look-to view, column-major (glam's convention).
    fn view_proj(eye: [f64; 3], yaw: f64, pitch: f64, aspect: f64, near: f64) -> [[f64; 4]; 4] {
        let f = 1.0 / (0.5f64).tan();
        let proj = [
            [f / aspect, 0.0, 0.0, 0.0],
            [0.0, f, 0.0, 0.0],
            [0.0, 0.0, 0.0, -1.0],
            [0.0, 0.0, near, 0.0],
        ];
        let fwd = [
            -yaw.sin() * pitch.cos(),
            pitch.sin(),
            -yaw.cos() * pitch.cos(),
        ];
        let cross = |a: [f64; 3], b: [f64; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let norm = |v: [f64; 3]| {
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            v.map(|x| x / l)
        };
        let s = norm(cross(fwd, [0.0, 1.0, 0.0]));
        let u = cross(s, fwd);
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let view = [
            [s[0], u[0], -fwd[0], 0.0],
            [s[1], u[1], -fwd[1], 0.0],
            [s[2], u[2], -fwd[2], 0.0],
            [-dot(s, eye), -dot(u, eye), dot(fwd, eye), 1.0],
        ];
        mat_mul(proj, view)
    }

    #[test]
    fn ray_basis_reproduces_the_unprojection() {
        let eye = [0.25, 0.75, 0.5];
        let m = view_proj(eye, 0.7, -0.3, 16.0 / 9.0, 0.1);
        let basis =
            RayBasis::new(m.map(|c| c.map(|v| v as f32)), eye.map(|v| v as f32)).expect("basis");
        assert!((basis.near - 0.1).abs() < 1e-7);
        for (x, y) in [(0.0f32, 0.0f32), (1.0, 1.0), (-0.5, 0.25), (0.9, -0.9)] {
            let dir = basis.dir(x, y);
            let (x, y) = (f64::from(x), f64::from(y));
            let p: [f64; 3] = std::array::from_fn(|a| eye[a] + f64::from(dir[a]) * 37.0);
            // Project the point 37 units along the ray: it lands on (x, y) at view depth 37.
            let clip: [f64; 4] =
                std::array::from_fn(|r| (0..3).map(|c| m[c][r] * p[c]).sum::<f64>() + m[3][r]);
            assert!((clip[3] - 37.0).abs() < 1e-3, "view depth {}", clip[3]);
            assert!(
                (clip[0] / clip[3] - x).abs() < 1e-5,
                "x {}",
                clip[0] / clip[3]
            );
            assert!(
                (clip[1] / clip[3] - y).abs() < 1e-5,
                "y {}",
                clip[1] / clip[3]
            );
            assert!((clip[2] / clip[3] - 0.1 / 37.0).abs() < 1e-9, "depth");
        }
        assert_eq!(RayBasis::new([[0.0; 4]; 4], [0.0; 3]), None);
    }
}
