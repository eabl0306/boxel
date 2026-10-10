//! The reference march in `f32`: the algorithm of `fog.wgsl`, step for step, for the tests (the
//! GPU tests compare the renderer's pixels with it).
//!
//! A ray from the eye (camera-relative) along `dir`, parameterized by view depth `t` (so
//! `dot(dir, forward) = 1`), crosses the layers front to back: their bands are disjoint, so
//! walking them upwards when the ray rises (downwards otherwise) visits their chords in order.
//! In each chord an Amanatides-Woo walk visits the cells; a cell of extinction `sigma` (per
//! block) over a segment of length `len` blocks keeps `exp(-sigma * len)` of the light behind it
//! and adds `(T_before - T_after) * color` of its own. Up to four records (the depths of the
//! pixels of a 2x2 block) take the transmittance and color in front of their depth.
//!
//! - **Shade.** A body of non-empty cells takes the shade of the face the ray entered it
//!   through (top, bottom, x or z), so every cube looks flat and crisp; a ray that starts
//!   inside fog uses 1.
//! - **Fade.** Each cell's color fades to the sky color with the distance of the segment's
//!   middle, like the terrain.
//! - **Budget.** At most `max_steps` cells over all layers; past it the rest of each chord gets
//!   the layer's mean extinction with shade 1 (no visible wall where the budget ends).
//! - **Early exit.** Once the optical depth passes 6 (`log2`: 8.66) nothing behind shows.

use crate::config::{FogFade, FogLayer};
use crate::layer::LayerParams;
use crate::noise;

/// Optical depth (in `log2` units, `exp2(-tau)`) after which the march stops: `6 / ln 2`.
pub const EXIT_TAU_LOG2: f32 = 8.66;
/// The march treats direction components smaller than this as this (no division by zero).
pub const MIN_DIR: f32 = 1e-30;

/// One layer as the march reads it (the uniform's per-layer data).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarchLayer {
    /// The integer part.
    pub params: LayerParams,
    /// Color of a lit cell (linear RGB).
    pub lit: [f32; 3],
    /// Shades of the faces: top, bottom, x, z.
    pub face_shade: [f32; 4],
    /// Fade start and end distance.
    pub fade: [f32; 2],
    /// The farthest the layer is drawn (blocks along the ray).
    pub max_distance: f32,
    /// Mean extinction (per block, times `log2(e)`) for the rest of a ray past the budget.
    pub sigma_mean_log2e: f32,
}

impl MarchLayer {
    /// A layer for `render_origin` and `tick`: `lit` is its color under this frame's light,
    /// `scene_fade` the scene's fog start and end (used by [`FogFade::Scene`]), `mean_coverage`
    /// its [`crate::mean_coverage`].
    pub fn new(
        layer: &FogLayer,
        render_origin: [i64; 3],
        tick: u64,
        lit: [f32; 3],
        scene_fade: [f32; 2],
        mean_coverage: f32,
    ) -> Self {
        let params = LayerParams::new(layer, render_origin, tick);
        let fade = match layer.fade {
            FogFade::Scene => scene_fade,
            FogFade::Range { start, end } => [start, end],
        };
        Self {
            params,
            lit,
            face_shade: layer.face_shade,
            fade,
            max_distance: layer.max_distance,
            sigma_mean_log2e: params.density_log2e * mean_coverage,
        }
    }

    /// Fade (0 to 1) at `dist` blocks.
    pub fn fade_at(&self, dist: f32) -> f32 {
        ((dist - self.fade[0]) / (self.fade[1] - self.fade[0]).max(1.0)).clamp(0.0, 1.0)
    }

    /// The farthest distance drawn: `min(max_distance, fade end)`.
    pub fn reach(&self) -> f32 {
        self.max_distance.min(self.fade[1].max(self.fade[0]))
    }
}

/// Index of the face shade a ray takes entering a cell along `axis` in direction `step`: 0 top
/// (going down), 1 bottom (going up), 2 x, 3 z.
pub fn entry_face(axis: usize, step: i32) -> usize {
    match axis {
        0 => 2,
        1 if step > 0 => 1,
        1 => 0,
        _ => 3,
    }
}

/// `t` interval of the ray inside `[lo, hi)` along y, or `None`.
pub fn band_chord(eye_y: f32, dir_y: f32, band: [f32; 2]) -> Option<(f32, f32)> {
    if dir_y.abs() < MIN_DIR {
        return (eye_y >= band[0] && eye_y < band[1]).then_some((0.0, f32::INFINITY));
    }
    let inv = 1.0 / dir_y;
    let (a, b) = ((band[0] - eye_y) * inv, (band[1] - eye_y) * inv);
    let (t0, t1) = (a.min(b).max(0.0), a.max(b));
    (t0 < t1).then_some((t0, t1))
}

/// The cell walk of one chord.
#[derive(Clone, Copy, Debug)]
pub struct Dda {
    /// Current cell, relative to the origin's cell.
    pub cell: [i32; 3],
    /// Direction of each axis' steps (+1 or -1).
    pub step: [i32; 3],
    /// `t` where the ray leaves the current cell along each axis.
    pub t_next: [f32; 3],
    inv: [f32; 3],
    eye: [f32; 3],
    grid: [f32; 3],
    size: [f32; 3],
}

impl Dda {
    /// The walk of a ray from `t0` through the cells of `params`, the y cell clamped into its
    /// band.
    pub fn new(eye: [f32; 3], dir: [f32; 3], params: &LayerParams, t0: f32) -> Self {
        let size = params.cell.map(|c| c as f32);
        let mut cell = [0; 3];
        let mut step = [1; 3];
        let mut inv = [0.0; 3];
        for a in 0..3 {
            // `-0.0` counts as positive, like the shader's `select`.
            let d = if dir[a].abs() >= MIN_DIR {
                dir[a]
            } else if dir[a] < 0.0 {
                -MIN_DIR
            } else {
                MIN_DIR
            };
            inv[a] = 1.0 / d;
            step[a] = if d > 0.0 { 1 } else { -1 };
            let p = eye[a] + t0 * dir[a];
            cell[a] = ((p - params.grid[a]) / size[a]).floor() as i32;
        }
        cell[1] = cell[1].clamp(params.band_cells[0], params.band_cells[1] - 1);
        let mut dda = Self {
            cell,
            step,
            t_next: [0.0; 3],
            inv,
            eye,
            grid: params.grid,
            size,
        };
        for a in 0..3 {
            dda.t_next[a] = dda.boundary(a);
        }
        dda
    }

    /// `t` of the current cell's exit plane along `axis`.
    fn boundary(&self, a: usize) -> f32 {
        let edge = self.cell[a] + i32::from(self.step[a] > 0);
        (self.grid[a] + edge as f32 * self.size[a] - self.eye[a]) * self.inv[a]
    }

    /// The axis the ray leaves the current cell through first (x, then y, then z on ties) and
    /// its `t`.
    pub fn exit(&self) -> (usize, f32) {
        let t = self.t_next;
        if t[0] <= t[1] && t[0] <= t[2] {
            (0, t[0])
        } else if t[1] <= t[2] {
            (1, t[1])
        } else {
            (2, t[2])
        }
    }

    /// Steps into the next cell along `axis`.
    pub fn advance(&mut self, axis: usize) {
        self.cell[axis] += self.step[axis];
        self.t_next[axis] = self.boundary(axis);
    }
}

/// One cell of a walk ([`walk`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Visit {
    /// The cell, relative to the origin's cell.
    pub cell: [i32; 3],
    /// Where the ray enters it.
    pub t0: f32,
    /// Where it leaves it (or the chord ends).
    pub t1: f32,
    /// The face shade index of the face it entered through, `None` for the first cell.
    pub entered: Option<usize>,
}

/// Every cell the ray crosses between `t0` and `t1` inside the band of `params`, at most
/// `max_cells`.
pub fn walk(
    eye: [f32; 3],
    dir: [f32; 3],
    params: &LayerParams,
    t0: f32,
    t1: f32,
    max_cells: usize,
) -> Vec<Visit> {
    let mut out = Vec::new();
    let mut dda = Dda::new(eye, dir, params, t0);
    let mut s0 = t0;
    let mut entered = None;
    while out.len() < max_cells {
        let (axis, t) = dda.exit();
        let s1 = t.clamp(s0, t1);
        out.push(Visit {
            cell: dda.cell,
            t0: s0,
            t1: s1,
            entered,
        });
        if t >= t1 {
            break;
        }
        dda.advance(axis);
        entered = Some(entry_face(axis, dda.step[axis]));
        s0 = s1;
        if dda.cell[1] < params.band_cells[0] || dda.cell[1] >= params.band_cells[1] {
            break;
        }
    }
    out
}

/// One ray of the march.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    /// Eye relative to the render origin.
    pub eye: [f32; 3],
    /// Direction with `dot(dir, forward) = 1`.
    pub dir: [f32; 3],
    /// View depth of each record (`f32::INFINITY` for the sky).
    pub depths: [f32; 4],
}

/// What a ray gives each record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarchResult {
    /// Transmittance in front of each record's depth.
    pub transmittance: [f32; 4],
    /// Fog color in front of it (premultiplied: add it to `background * transmittance`).
    pub color: [[f32; 3]; 4],
    /// Cells visited.
    pub steps: u32,
}

/// The march state of one ray (the shader's variables).
struct State {
    t: f32,
    c: [f32; 3],
    tau: f32,
    rec_t: [f32; 4],
    rec_order: [usize; 4],
    next: usize,
    out_t: [f32; 4],
    out_c: [[f32; 3]; 4],
    steps: u32,
    done: bool,
}

impl State {
    /// Records whose depth is at most `t` take the current values.
    fn flush(&mut self, upto: f32) {
        while self.next < 4 && self.rec_t[self.next] <= upto {
            let k = self.rec_order[self.next];
            self.out_t[k] = self.t;
            self.out_c[k] = self.c;
            self.next += 1;
        }
    }

    /// Integrates `[s0, s1]` (view depth) of extinction `sigma_t` (per unit `t`, `log2`) and
    /// color `col`, taking the records inside it.
    fn segment(&mut self, s0: f32, s1: f32, sigma_t: f32, col: [f32; 3]) {
        while self.next < 4 && self.rec_t[self.next] <= s1 {
            let tr = self.t * (-sigma_t * (self.rec_t[self.next] - s0).max(0.0)).exp2();
            let k = self.rec_order[self.next];
            self.out_t[k] = tr;
            self.out_c[k] = std::array::from_fn(|i| self.c[i] + (self.t - tr) * col[i]);
            self.next += 1;
        }
        let len = s1 - s0;
        let t_new = self.t * (-sigma_t * len).exp2();
        for (c, &k) in self.c.iter_mut().zip(&col) {
            *c += (self.t - t_new) * k;
        }
        self.t = t_new;
        self.tau += sigma_t * len;
        if self.tau > EXIT_TAU_LOG2 {
            self.done = true;
        }
    }
}

/// Sorts four depths ascending with the shader's network (0-1, 2-3, 0-2, 1-3, 1-2), keeping
/// which record each came from.
pub fn sort_depths(depths: [f32; 4]) -> ([f32; 4], [usize; 4]) {
    let mut t = depths;
    let mut k = [0, 1, 2, 3];
    for (a, b) in [(0, 1), (2, 3), (0, 2), (1, 3), (1, 2)] {
        if t[b] < t[a] {
            t.swap(a, b);
            k.swap(a, b);
        }
    }
    (t, k)
}

/// Marches one ray through `layers` (sorted by height, bands disjoint) with the sky color
/// `sky` and a budget of `max_steps` cells.
pub fn march(ray: &Ray, layers: &[MarchLayer], sky: [f32; 3], max_steps: u32) -> MarchResult {
    let (rec_t, rec_order) = sort_depths(ray.depths);
    let mut s = State {
        t: 1.0,
        c: [0.0; 3],
        tau: 0.0,
        rec_t,
        rec_order,
        next: 0,
        out_t: [1.0; 4],
        out_c: [[0.0; 3]; 4],
        steps: 0,
        done: false,
    };
    let dir = ray.dir;
    let dir_len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    let t_end = rec_t[3];
    let up = dir[1] > 0.0;
    for n in 0..layers.len() {
        if s.done {
            break;
        }
        let layer = &layers[if up { n } else { layers.len() - 1 - n }];
        let p = &layer.params;
        let Some((ta, tb)) = band_chord(ray.eye[1], dir[1], p.band) else {
            continue;
        };
        let tb = tb.min(t_end).min(layer.reach() / dir_len);
        if ta >= tb {
            continue;
        }
        s.flush(ta);
        let fade_col = |s0: f32, s1: f32, shade: f32| -> [f32; 3] {
            let f = layer.fade_at(0.5 * (s0 + s1) * dir_len);
            std::array::from_fn(|i| layer.lit[i] * shade + (sky[i] - layer.lit[i] * shade) * f)
        };
        if s.steps >= max_steps {
            let col = fade_col(ta, tb, 1.0);
            s.segment(ta, tb, layer.sigma_mean_log2e * dir_len, col);
            continue;
        }
        let mut dda = Dda::new(ray.eye, dir, p, ta);
        let mut s0 = ta;
        let mut shade = 1.0;
        let mut entry = if ta > 0.0 {
            layer.face_shade[entry_face(1, if up { 1 } else { -1 })]
        } else {
            1.0
        };
        let mut prev_full = false;
        loop {
            if s.steps >= max_steps {
                let col = fade_col(s0, tb, 1.0);
                s.segment(s0, tb, layer.sigma_mean_log2e * dir_len, col);
                break;
            }
            s.steps += 1;
            let (axis, t) = dda.exit();
            let s1 = t.clamp(s0, tb);
            let cov = noise::coverage(p, dda.cell).coverage;
            let sigma = p.density_log2e * (cov as f32 / 4096.0);
            if sigma > 0.0 {
                if !prev_full {
                    shade = entry;
                }
                let col = fade_col(s0, s1, shade);
                s.segment(s0, s1, sigma * dir_len, col);
                if s.done {
                    break;
                }
            }
            prev_full = sigma > 0.0;
            if t >= tb {
                break;
            }
            dda.advance(axis);
            entry = layer.face_shade[entry_face(axis, dda.step[axis])];
            s0 = s1;
            if dda.cell[1] < p.band_cells[0] || dda.cell[1] >= p.band_cells[1] {
                break;
            }
        }
    }
    s.flush(f32::INFINITY);
    MarchResult {
        transmittance: s.out_t,
        color: s.out_c,
        steps: s.steps,
    }
}

/// The texel the march writes for a ray: the mean color per unit opacity and the four
/// opacities ([`crate::pack`]).
pub fn texel(result: &MarchResult) -> [u32; 2] {
    let alphas = result.transmittance.map(|t| 1.0 - t);
    let sum: f32 = alphas.iter().sum();
    let color = if sum > 0.0 {
        std::array::from_fn(|i| result.color.iter().map(|c| c[i]).sum::<f32>() / sum)
    } else {
        [0.0; 3]
    };
    [
        crate::pack::encode_color(color),
        crate::pack::encode_alphas(alphas),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FogLayer, NoiseDims};

    fn params(layer: &FogLayer) -> LayerParams {
        LayerParams::new(layer, [0; 3], 0)
    }

    fn slab(min: i64, max: i64, cell: [u32; 3], density: f32) -> FogLayer {
        let mut l = FogLayer::new(min, max);
        l.cell = cell;
        l.density = density;
        l.max_distance = 4096.0;
        l.fade = FogFade::Range {
            start: 4000.0,
            end: 4096.0,
        };
        l
    }

    fn march_layer(l: &FogLayer, mean: f32) -> MarchLayer {
        MarchLayer::new(l, [0; 3], 0, l.color, [0.0, 1.0], mean)
    }

    fn norm(v: [f32; 3]) -> [f32; 3] {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        v.map(|x| x / l)
    }

    #[test]
    fn walks_cover_the_chord_with_face_adjacent_cells() {
        let layer = slab(-64, 64, [2, 4, 3], 1.0);
        let p = params(&layer);
        let eye = [0.3, 0.6, 0.9];
        for dir in [
            [1.0, 0.2, 0.3],
            [-0.7, -0.4, 1.0],
            [0.0, 0.0, 1.0],
            [0.0, -1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.31, 0.77, -0.55],
        ] {
            let visits = walk(eye, dir, &p, 0.0, 40.0, 10_000);
            let total: f32 = visits.iter().map(|v| v.t1 - v.t0).sum();
            assert!((total - 40.0).abs() < 1e-3, "{dir:?}: {total}");
            for w in visits.windows(2) {
                let d: i32 = (0..3).map(|a| (w[1].cell[a] - w[0].cell[a]).abs()).sum();
                assert_eq!(d, 1, "{dir:?}: {:?} -> {:?}", w[0].cell, w[1].cell);
                assert_eq!(w[0].t1, w[1].t0);
                assert!(w[1].entered.is_some());
            }
            // Every visited cell holds the middle of its segment.
            for v in &visits {
                let tm = 0.5 * (v.t0 + v.t1);
                for a in 0..3 {
                    let pos = eye[a] + tm * dir[a];
                    let c = ((pos - p.grid[a]) / p.cell[a] as f32).floor() as i32;
                    if v.t1 - v.t0 > 1e-3 {
                        assert_eq!(c, v.cell[a], "{dir:?} axis {a} at {tm}");
                    }
                }
            }
        }
    }

    #[test]
    fn entry_faces_follow_the_step() {
        let layer = slab(-64, 64, [1, 1, 1], 1.0);
        let p = params(&layer);
        let up = walk([0.5, 0.5, 0.5], [0.0, 1.0, 0.0], &p, 0.0, 3.0, 10);
        assert_eq!(up[1].entered, Some(1), "going up enters through bottoms");
        let down = walk([0.5, 0.5, 0.5], [0.0, -1.0, 0.0], &p, 0.0, 3.0, 10);
        assert_eq!(down[1].entered, Some(0), "going down enters through tops");
        let x = walk([0.5, 0.5, 0.5], [-1.0, 0.0, 0.0], &p, 0.0, 3.0, 10);
        assert_eq!((x[1].entered, x[1].cell), (Some(2), [-1, 0, 0]));
        let z = walk([0.5, 0.5, 0.5], [0.0, 0.0, 1.0], &p, 0.0, 3.0, 10);
        assert_eq!((z[1].entered, z[1].cell), (Some(3), [0, 0, 1]));
    }

    #[test]
    fn rays_starting_on_a_band_plane_enter_its_first_cells() {
        // Band y 8..16 with 4-block cells, the eye below and above it.
        let layer = slab(8, 16, [4, 4, 4], 1.0);
        let p = params(&layer);
        for (eye, dir, first_y) in [
            ([0.5, 0.5, 0.5], [0.3, 1.0, 0.0], 2),
            ([0.5, 30.5, 0.5], [0.0, -1.0, 0.2], 3),
        ] {
            let (t0, t1) = band_chord(eye[1], dir[1], p.band).expect("chord");
            let visits = walk(eye, dir, &p, t0, t1, 100);
            assert_eq!(visits[0].cell[1], first_y, "{eye:?}");
            let total: f32 = visits.iter().map(|v| v.t1 - v.t0).sum();
            assert!((total - (t1 - t0)).abs() < 1e-4);
            assert!(visits.iter().all(|v| (2..4).contains(&v.cell[1])));
        }
        assert_eq!(band_chord(0.5, 0.0, p.band), None);
        assert_eq!(band_chord(9.0, 0.0, p.band), Some((0.0, f32::INFINITY)));
        assert_eq!(band_chord(9.0, -1.0, p.band), Some((0.0, 1.0)));
    }

    #[test]
    fn a_homogeneous_slab_keeps_exp_of_its_depth() {
        let layer = slab(0, 10, [1, 1, 1], 0.2);
        let ml = march_layer(&layer, 1.0);
        let eye = [0.5, 3.5, 0.5];
        let dir = norm([0.2, 0.0, 1.0]);
        // Normalized so t is the distance; the record at 7 blocks.
        let ray = Ray {
            eye,
            dir,
            depths: [7.0; 4],
        };
        let r = march(&ray, &[ml], [0.0; 3], 256);
        let want = (-0.2f32 * 7.0).exp();
        assert!((r.transmittance[0] - want).abs() < 1e-4, "{r:?}");
        // White fog: the color in front is 1 - T.
        assert!((r.color[0][0] - (1.0 - want)).abs() < 1e-4);
        assert_eq!(r.transmittance, [r.transmittance[0]; 4]);
    }

    #[test]
    fn records_equal_separate_marches() {
        let mut layer = slab(-8, 8, [2, 2, 2], 0.3);
        layer.noise.dims = NoiseDims::Volume;
        layer.noise.scale = 2;
        layer.threshold = 0.4;
        let ml = march_layer(&layer, 0.5);
        let eye = [0.5, 0.5, 0.5];
        let dir = [0.4, -0.1, 1.0];
        let depths = [9.0, 3.5, f32::INFINITY, 6.25];
        let all = march(&Ray { eye, dir, depths }, &[ml], [0.2, 0.3, 0.9], 256);
        for (k, &depth) in depths.iter().enumerate() {
            let one = march(
                &Ray {
                    eye,
                    dir,
                    depths: [depth; 4],
                },
                &[ml],
                [0.2, 0.3, 0.9],
                256,
            );
            assert!(
                (one.transmittance[0] - all.transmittance[k]).abs() < 1e-5,
                "{k}"
            );
            for c in 0..3 {
                assert!((one.color[0][c] - all.color[k][c]).abs() < 1e-5, "{k}");
            }
        }
    }

    #[test]
    fn the_march_stops_at_the_depth_and_exits_early() {
        let layer = slab(0, 10, [1, 1, 1], 0.5);
        let ml = march_layer(&layer, 1.0);
        let ray = |d: f32| Ray {
            eye: [0.5, 5.5, 0.5],
            dir: [0.0, 0.0, 1.0],
            depths: [d; 4],
        };
        let near = march(&ray(2.0), &[ml], [0.0; 3], 256);
        assert_eq!(near.steps, 3, "cells up to depth 2");
        let far = march(&ray(f32::INFINITY), &[ml], [0.0; 3], 256);
        // tau = 0.5 * log2(e) per block passes 8.66 after 13 blocks.
        assert_eq!(far.steps, 13);
        assert!(far.transmittance[0] < 0.003);
    }

    #[test]
    fn the_budget_tail_is_continuous() {
        // A full slab: the mean coverage is 1, so the tail continues the same extinction.
        let layer = slab(0, 10, [1, 1, 1], 0.02);
        let ml = march_layer(&layer, 1.0);
        let ray = Ray {
            eye: [0.5, 5.5, 0.5],
            dir: [0.0, 0.0, 1.0],
            depths: [100.0; 4],
        };
        let exact = march(&ray, &[ml], [0.0; 3], 256);
        let capped = march(&ray, &[ml], [0.0; 3], 10);
        assert_eq!(capped.steps, 10);
        assert!((exact.transmittance[0] - capped.transmittance[0]).abs() < 1e-4);
        assert!((exact.transmittance[0] - (-2.0f32).exp()).abs() < 1e-3);
    }

    #[test]
    fn one_shade_per_body_not_per_cell() {
        // Opaque-ish cells everywhere: a ray entering from above keeps the top shade through
        // every cell; from below the bottom shade; from the side the x shade.
        let mut layer = slab(8, 16, [1, 1, 1], 0.05);
        layer.face_shade = [1.0, 0.5, 0.8, 0.65];
        layer.color = [1.0; 3];
        let ml = march_layer(&layer, 1.0);
        let shade_of = |eye: [f32; 3], dir: [f32; 3]| {
            let r = march(
                &Ray {
                    eye,
                    dir,
                    depths: [f32::INFINITY; 4],
                },
                &[ml],
                [0.0; 3],
                256,
            );
            // Color per unit opacity is the shade.
            r.color[0][0] / (1.0 - r.transmittance[0])
        };
        assert!((shade_of([0.5, 30.5, 0.5], [0.1, -1.0, 0.0]) - 1.0).abs() < 1e-4);
        assert!((shade_of([0.5, 0.5, 0.5], [0.1, 1.0, 0.05]) - 0.5).abs() < 1e-4);
        assert!(
            (shade_of([0.5, 12.5, 0.5], [1.0, 0.0, 0.1]) - 1.0).abs() < 1e-4,
            "inside: 1"
        );
    }

    #[test]
    fn disjoint_layers_both_ways() {
        let low = slab(0, 4, [1, 1, 1], 0.1);
        let high = slab(20, 24, [1, 1, 1], 0.3);
        let layers = [march_layer(&low, 1.0), march_layer(&high, 1.0)];
        let up = march(
            &Ray {
                eye: [0.5, -10.0, 0.5],
                dir: [0.0, 1.0, 0.0],
                depths: [f32::INFINITY; 4],
            },
            &layers,
            [0.0; 3],
            256,
        );
        let down = march(
            &Ray {
                eye: [0.5, 40.0, 0.5],
                dir: [0.0, -1.0, 0.0],
                depths: [f32::INFINITY; 4],
            },
            &layers,
            [0.0; 3],
            256,
        );
        let want = (-0.4f32 - 1.2).exp();
        assert!((up.transmittance[0] - want).abs() < 1e-4);
        assert!((down.transmittance[0] - want).abs() < 1e-4);
        // The near layer weighs more in the color: white near, black far.
        let mut white_low = low.clone();
        white_low.color = [1.0; 3];
        white_low.face_shade = [1.0; 4];
        let mut black_high = high.clone();
        black_high.color = [0.0; 3];
        let layers = [march_layer(&white_low, 1.0), march_layer(&black_high, 1.0)];
        let r = march(
            &Ray {
                eye: [0.5, -10.0, 0.5],
                dir: [0.0, 1.0, 0.0],
                depths: [f32::INFINITY; 4],
            },
            &layers,
            [0.0; 3],
            256,
        );
        assert!((r.color[0][0] - (1.0 - (-0.4f32).exp())).abs() < 1e-4);
    }

    #[test]
    fn the_sort_network_sorts() {
        for d in [
            [4.0, 3.0, 2.0, 1.0],
            [1.0, 2.0, 3.0, 4.0],
            [2.0, 4.0, 1.0, 3.0],
            [f32::INFINITY, 0.5, 0.5, 2.0],
        ] {
            let (t, k) = sort_depths(d);
            assert!(t.windows(2).all(|w| w[0] <= w[1]), "{d:?}");
            for i in 0..4 {
                assert_eq!(d[k[i]], t[i]);
            }
        }
    }

    #[test]
    fn the_texel_mean_color() {
        let r = MarchResult {
            transmittance: [1.0, 0.5, 0.5, 0.0],
            color: [[0.0; 3], [0.5, 0.0, 0.0], [0.5, 0.0, 0.0], [1.0, 0.0, 0.0]],
            steps: 1,
        };
        let [c, a] = texel(&r);
        assert_eq!(a & 0xff, 0);
        assert_eq!(a >> 24, 255);
        let color = crate::pack::decode_color(c);
        assert!((color[0] - 1.0).abs() < 1e-3);
    }
}
