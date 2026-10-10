//! What the GPU gets of a layer each frame: everything absolute reduced to small integers
//! relative to the render origin, computed here in `i64` and `i128` (ADR 0014).

use crate::config::{FogLayer, NoiseDims};
use crate::noise::{self, COVERAGE_ONE, gain_units, seed32, seed32_octave2, threshold_units};

/// Fixed steps a second of the fog clock (the app's fixed rate): the wind moves once a tick.
pub const TICKS_PER_SECOND: u32 = 64;
/// Bands are clamped to this far from the render origin, where `f32` is still exact.
pub const BAND_LIMIT: i64 = 1 << 24;
/// Band cell indices are clamped to this far from the origin's cell.
pub const BAND_CELL_LIMIT: i64 = 1 << 30;

/// Wind in 1/65536 blocks per tick: `round(wind * 65536 / 64)`.
pub fn wind_units(wind: f32) -> i64 {
    (f64::from(wind) * 65536.0 / f64::from(TICKS_PER_SECOND)).round() as i64
}

/// The wind displacement of a layer after `tick` ticks, per axis: the whole cells (wrapping,
/// `i64`) and the fraction of a cell in 1/4096 (`0..4096`). Flat noise ignores the wind along y.
pub fn displacement(layer: &FogLayer, tick: u64) -> [(i64, i32); 3] {
    std::array::from_fn(|a| {
        if a == 1 && layer.noise.dims == NoiseDims::Flat {
            return (0, 0);
        }
        // 1/65536 cells, floored.
        let q16 = (i128::from(wind_units(layer.wind[a])) * i128::from(tick))
            .div_euclid(i128::from(layer.cell[a]));
        ((q16 >> 16) as i64, ((q16 & 0xffff) >> 4) as i32)
    })
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
    /// Coverage threshold, `0..=4096`.
    pub threshold: u32,
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
    /// The layer for `render_origin` (world blocks) at `tick`.
    pub fn new(layer: &FogLayer, render_origin: [i64; 3], tick: u64) -> Self {
        let volume = layer.noise.dims == NoiseDims::Volume;
        let l = layer.noise.scale.max(1).trailing_zeros();
        let cell = layer.cell.map(|c| c.max(1));
        let c0: [i64; 3] = std::array::from_fn(|a| render_origin[a].div_euclid(i64::from(cell[a])));
        let grid = std::array::from_fn(|a| (c0[a] * i64::from(cell[a]) - render_origin[a]) as f32);
        let disp = displacement(layer, tick);
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
            gain: gain_units(layer.softness),
            band: [band_rel(layer.min_y), band_rel(layer.max_y)],
            band_cells: [band_cell(layer.min_y), band_cell(layer.max_y)],
            density_log2e: layer.density * std::f32::consts::LOG2_E,
        }
    }
}

/// The mean coverage (0 to 1) of a layer's cells, from a fixed sample of 1024 cells spread over
/// many lattice steps: what the march gives the rest of a ray once its step budget runs out.
/// Deterministic; the same for every origin and tick (on average).
pub fn mean_coverage(layer: &FogLayer) -> f32 {
    let params = LayerParams::new(layer, [0; 3], 0);
    let step = 3 * layer.noise.scale.max(1) as i32 + 1;
    let mut sum = 0u64;
    for i in 0..1024i32 {
        let d = [
            (i & 31) * step + (i * 7) % step,
            ((i * 5) % 16) * step,
            (i >> 5) * step + (i * 11) % step,
        ];
        sum += u64::from(noise::coverage(&params, d).coverage);
    }
    (sum as f64 / (1024.0 * f64::from(COVERAGE_ONE))) as f32
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

    #[test]
    fn mean_coverage_follows_the_threshold() {
        let mut layer = FogLayer::new(0, 8);
        layer.noise.scale = 4;
        assert_eq!(mean_coverage(&layer), 1.0, "threshold 0 fills every cell");
        layer.threshold = 1.0;
        assert_eq!(mean_coverage(&layer), 0.0);
        layer.threshold = 0.5;
        let half = mean_coverage(&layer);
        assert!((0.3..0.7).contains(&half), "{half}");
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
