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
//! - **Shade.** Off by default (every shade 1). A body of non-empty cells can take the shade of
//!   the face the ray entered it through (top, bottom, x or z); a ray that starts inside fog
//!   uses 1. The height in the band can shade it too (a gray base, a lit top), by the middle of
//!   each segment.
//! - **Fade.** Each segment's color fades to the sky color with the distance of its middle,
//!   like the terrain.
//! - **Soft fog.** A cell that starts within the layer's soft distance (and the blend past it)
//!   is cut in pieces of about a block, and each piece blends the cell's coverage into the
//!   coverage of the noise read at the piece's middle ([`MarchLayer::soft_coverage`]): no cell
//!   edges near the camera. Its pieces take no face shade.
//! - **Edges.** Near the band's floor and ceiling the threshold rises to never covered
//!   ([`noise::row_edge`] per cell row, [`noise::point_edge`] per point of the soft fog), so the
//!   band ends in an uneven skyline of cubes, not a plane.
//! - **Haze.** The cells end at [`MarchLayer::cells_end`]. A layer with
//!   [`FogLayer::haze`] goes on past them as a smooth haze of that fraction of its
//!   [`HazeProfile`] (the mean extinction at each height) up to [`MarchLayer::reach`], taken in
//!   pieces split at the knot heights the ray crosses and at the fade start, each of linear
//!   extinction, so every record inside one gets its exact transmittance. Over the last
//!   [`FAR_BLEND`] before the cells end, each cell blends into the haze, and at their end a cell
//!   is the haze.
//! - **Budget.** At most `max_steps` cells over all layers; past it the rest of each layer's
//!   cells takes its haze profile at full strength with shade 1 (no visible wall where the budget
//!   ends), thinned near the edges like the cells.
//! - **Early exit.** Once the optical depth passes 6 (`log2`: 8.66) nothing behind shows.
//! - **Skip.** A lattice step of the noise whose corners leave no coverage at the lowest
//!   threshold of its rows ([`noise::step_bound`]) is crossed in one step (never in the soft fog,
//!   which reads points outside the step, nor into the blend before the haze, where an empty
//!   cell still takes haze), and a layer whose every cell is full ([`MarchLayer::uniform`]) is
//!   taken in one step without walking its cells (its blend before the haze in pieces that
//!   blend as cells do).

use crate::config::{FogFade, FogLayer, MAX_FOG_DISTANCE};
use crate::layer::{FogTime, HAZE_KNOTS, HazeProfile, LayerParams};
use crate::noise;

/// Optical depth (in `log2` units, `exp2(-tau)`) after which the march stops: `6 / ln 2`.
pub const EXIT_TAU_LOG2: f32 = 8.66;
/// The march treats direction components smaller than this as this (no division by zero).
pub const MIN_DIR: f32 = 1e-30;
/// Pieces of a chord through a layer whose every cell is full ([`MarchLayer::uniform`]), and as
/// many for its blend before the haze: each one fades (and blends) by its own middle.
pub const UNIFORM_PIECES: u32 = 8;
/// The soft fog blends into the cells from the soft distance to this much farther (a fraction
/// of the soft distance).
pub const SOFT_BLEND: f32 = 0.5;
/// Length of a piece of the soft fog, in blocks.
pub const SOFT_PIECE: f32 = 1.0;
/// Most pieces of one cell in the soft fog.
pub const SOFT_MAX_PIECES: u32 = 16;
/// The cells blend into the haze over this last fraction of their distance
/// ([`MarchLayer::blend_start`]), the mirror of the soft fog's blend near the camera.
pub const FAR_BLEND: f32 = 0.25;
/// Most pieces of one chord of haze: one per knot height crossed and one at the fade start, plus
/// one.
pub const HAZE_PIECES: u32 = HAZE_KNOTS as u32 + 2;
/// A `t` past every chord (the shader's `FAR`).
const FAR: f32 = 1e30;

/// One layer as the march reads it (the uniform's per-layer data).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarchLayer {
    /// The integer part.
    pub params: LayerParams,
    /// Color of a lit cell (linear RGB).
    pub lit: [f32; 3],
    /// Shades of the faces: top, bottom, x, z.
    pub face_shade: [f32; 4],
    /// Brightness at the bottom and at the top of the band.
    pub height_shade: [f32; 2],
    /// Distances where the soft fog starts to blend into the cells and where it is all cells
    /// (`[0, 0]`: no soft fog).
    pub soft: [f32; 2],
    /// Fade start and end distance.
    pub fade: [f32; 2],
    /// Where the cells end (blocks along the ray): `min(max_distance, fade end)`.
    pub cells_end: f32,
    /// The farthest the layer is drawn: `cells_end`, or with haze its fade end (at most
    /// [`MAX_FOG_DISTANCE`]) when that is farther.
    pub reach: f32,
    /// Where the cells start to blend into the haze, [`FAR_BLEND`] of `cells_end` before it;
    /// `cells_end` when the layer ends with its cells (no blend).
    pub blend_start: f32,
    /// The fraction of the profile the haze takes ([`FogLayer::haze`]).
    pub haze: f32,
    /// The mean extinction at each height ([`crate::haze_profile`]): the haze past the cells,
    /// and the rest of the cells past the step budget.
    pub profile: HazeProfile,
    /// Every cell is full (threshold 0 without a rise or edges, softness 0): the march takes
    /// each chord whole.
    pub uniform: bool,
    /// `1 / max(band height, 1)`: the height in the band multiplies by it
    /// ([`MarchLayer::height_at`], the soft fog's rise).
    pub inv_height: f32,
    /// `1 / (soft[1] - soft[0])` (0 without soft fog): [`MarchLayer::soft_weight`] multiplies by
    /// it.
    pub inv_soft: f32,
}

impl MarchLayer {
    /// A layer for `render_origin` at `time`: `lit` is its color under this frame's light,
    /// `scene_fade` the fade start and end [`FogFade::Scene`] takes (the renderer gives the
    /// terrain's, or its distance fog at the layer's end if the layer ends first; for a layer
    /// with haze, the farthest terrain's), `profile` its [`crate::haze_profile`].
    pub fn new(
        layer: &FogLayer,
        render_origin: [i64; 3],
        time: impl Into<FogTime>,
        lit: [f32; 3],
        scene_fade: [f32; 2],
        profile: HazeProfile,
    ) -> Self {
        let params = LayerParams::new(layer, render_origin, time);
        let fade = match layer.fade {
            FogFade::Scene => scene_fade,
            FogFade::Range { start, end } => [start, end],
        };
        let end = fade[1].max(fade[0]);
        let cells_end = layer.max_distance.min(end);
        let reach = if layer.haze > 0.0 {
            cells_end.max(end.min(MAX_FOG_DISTANCE))
        } else {
            cells_end
        };
        let blend_start = if reach > cells_end {
            cells_end * (1.0 - FAR_BLEND)
        } else {
            cells_end
        };
        let soft = if layer.soft_distance > 0.0 {
            [
                layer.soft_distance,
                layer.soft_distance * (1.0 + SOFT_BLEND),
            ]
        } else {
            [0.0; 2]
        };
        Self {
            params,
            lit,
            face_shade: layer.face_shade,
            height_shade: layer.height_shade,
            soft,
            fade,
            cells_end,
            reach,
            blend_start,
            haze: layer.haze,
            profile,
            uniform: params.threshold == 0
                && params.rise == 0
                && params.edges == [0; 2]
                && params.gain == noise::MAX_GAIN,
            inv_height: 1.0 / (params.band[1] - params.band[0]).max(1.0),
            inv_soft: if soft[1] > soft[0] {
                1.0 / (soft[1] - soft[0])
            } else {
                0.0
            },
        }
    }

    /// Fade (0 to 1) at `dist` blocks.
    pub fn fade_at(&self, dist: f32) -> f32 {
        ((dist - self.fade[0]) / (self.fade[1] - self.fade[0]).max(1.0)).clamp(0.0, 1.0)
    }

    /// Brightness by the height `y` (relative to the render origin): linear from the band's
    /// bottom to its top.
    pub fn height_at(&self, y: f32) -> f32 {
        let h = ((y - self.params.band[0]) * self.inv_height).clamp(0.0, 1.0);
        self.height_shade[0] + (self.height_shade[1] - self.height_shade[0]) * h
    }

    /// The color of fog at view depth `mid` of a ray from `eye_y` along `dir_y` (`dir_len` its
    /// length): lit times `shade` and the brightness at that height, faded to `sky` by that
    /// distance (the shader's `segment_color` of a segment whose middle is `mid`).
    pub fn color_at(
        &self,
        mid: f32,
        shade: f32,
        eye_y: f32,
        dir_y: f32,
        dir_len: f32,
        sky: [f32; 3],
    ) -> [f32; 3] {
        let f = self.fade_at(mid * dir_len);
        let shade = shade * self.height_at(eye_y + mid * dir_y);
        std::array::from_fn(|i| self.lit[i] * shade + (sky[i] - self.lit[i] * shade) * f)
    }

    /// How soft the fog is (0 to 1) at `dist` blocks: 1 up to the soft distance, a smoothstep
    /// down to 0 at the end of the blend. Only for a layer with soft fog.
    pub fn soft_weight(&self, dist: f32) -> f32 {
        let x = ((self.soft[1] - dist) * self.inv_soft).clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    }

    /// The soft fog's coverage (0 to 1) at `pos` (relative to the render origin): the noise read
    /// at that point half a cell back (so the middle of a cell reads that cell's value) and the
    /// threshold at that height, raised by the edges ([`noise::point_edge`]).
    pub fn soft_coverage(&self, pos: [f32; 3]) -> f32 {
        let p = &self.params;
        let mut d = [0; 3];
        let mut sub = [0; 3];
        for a in 0..3 {
            let u = (pos[a] - p.grid[a]) / p.cell[a] as f32 - 0.5;
            let f = u.floor();
            d[a] = f as i32;
            sub[a] = ((u - f) * 4096.0) as i32;
        }
        let threshold = if p.rise == 0 {
            p.threshold
        } else {
            let h = ((pos[1] - p.band[0]) * self.inv_height).clamp(0.0, 1.0);
            noise::threshold_at(p.threshold, p.rise, (h * 4096.0) as u32)
        };
        let threshold = if p.edges == [0; 2] {
            threshold
        } else {
            noise::raise(threshold, noise::smooth_ramp(noise::point_edge(p, pos[1])))
        };
        let n = noise::noise_at(p, d, sub);
        noise::coverage_from_noise(n, threshold, p.gain) as f32 / 4096.0
    }

    /// Height of the profile's first knot relative to the render origin (the band's bottom,
    /// clamped as [`LayerParams::band`] is, plus [`HazeProfile::bottom`]).
    pub fn haze_bottom(&self) -> f32 {
        self.params.band[0] + self.profile.bottom
    }

    /// Extinction per block (`log2` units) of the profile at height `y` (relative to the render
    /// origin), at a haze of 1 ([`HazeProfile::at`]).
    pub fn haze_at(&self, y: f32) -> f32 {
        self.profile.at(self.haze_bottom(), y)
    }

    /// How much of a cell in the blend before the haze is haze (0 to 1) at `dist` blocks: a
    /// smoothstep from 0 at [`MarchLayer::blend_start`] to 1 at [`MarchLayer::cells_end`]. Only
    /// for a layer that goes on past its cells.
    pub fn blend_weight(&self, dist: f32) -> f32 {
        let x = ((dist - self.blend_start) / (self.cells_end - self.blend_start)).clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
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
    dir: [f32; 3],
    grid: [f32; 3],
    size: [f32; 3],
    band: [i32; 2],
}

impl Dda {
    /// The walk of a ray from `t0` through the cells of `params`, the y cell clamped into the
    /// rows it walks ([`LayerParams::walk_cells`]). With no row to walk, the y cell lands just
    /// under them, as the shader's `clamp` (a `max`, then a `min`) puts it: outside, so nothing
    /// is walked.
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
        cell[1] = cell[1]
            .max(params.walk_cells[0])
            .min(params.walk_cells[1] - 1);
        let mut dda = Self {
            cell,
            step,
            t_next: [0.0; 3],
            inv,
            eye,
            dir,
            grid: params.grid,
            size,
            band: params.walk_cells,
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

    /// The axis the ray leaves the lattice step `bound` through first (x, then y, then z on
    /// ties; never y when the step is unbounded along it) and its `t`.
    pub fn step_exit(&self, bound: &noise::StepBound) -> (usize, f32) {
        let mut best = (0, f32::INFINITY);
        for a in 0..3 {
            if bound.lo[a] == i32::MIN {
                continue;
            }
            let edge = if self.step[a] > 0 {
                bound.hi[a]
            } else {
                bound.lo[a]
            };
            let t = (self.grid[a] + edge as f32 * self.size[a] - self.eye[a]) * self.inv[a];
            if t < best.1 {
                best = (a, t);
            }
        }
        best
    }

    /// Moves to `t`, just past the lattice step `bound` along `axis`: that axis' first cell
    /// beyond it, the others where the ray is (kept inside the step).
    pub fn jump(&mut self, t: f32, axis: usize, bound: &noise::StepBound) {
        for a in 0..3 {
            self.cell[a] = if a == axis {
                if self.step[a] > 0 {
                    bound.hi[a]
                } else {
                    bound.lo[a] - 1
                }
            } else {
                let p = self.eye[a] + t * self.dir[a];
                (((p - self.grid[a]) / self.size[a]).floor() as i32)
                    .clamp(bound.lo[a], bound.hi[a].saturating_sub(1))
            };
        }
        if axis != 1 {
            self.cell[1] = self.cell[1].max(self.band[0]).min(self.band[1] - 1);
        }
        for a in 0..3 {
            self.t_next[a] = self.boundary(a);
        }
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

/// Every cell the ray crosses between `t0` and `t1` inside the rows `params` walks
/// ([`LayerParams::walk_cells`]), at most `max_cells`; none when no row is left to walk.
pub fn walk(
    eye: [f32; 3],
    dir: [f32; 3],
    params: &LayerParams,
    t0: f32,
    t1: f32,
    max_cells: usize,
) -> Vec<Visit> {
    let mut out = Vec::new();
    if params.walk_cells[0] >= params.walk_cells[1] {
        return out;
    }
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
        if dda.cell[1] < params.walk_cells[0] || dda.cell[1] >= params.walk_cells[1] {
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
    /// Pieces of soft fog integrated (outside the budget).
    pub soft_pieces: u32,
    /// Pieces of haze integrated: past the cells, and the rest of the cells past the budget.
    pub haze_pieces: u32,
    /// The step budget ran out before the cells did.
    pub capped: bool,
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
    soft_pieces: u32,
    haze_pieces: u32,
    capped: bool,
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

    /// Integrates `[s0, s1]` (view depth, `s1 > s0`) whose extinction (per unit `t`, `log2`)
    /// goes linearly from `sigma0` to `sigma1`, of color `col`: the exact optical depth of the
    /// piece and of each record inside it.
    fn segment_ramp(&mut self, s0: f32, s1: f32, sigma0: f32, sigma1: f32, col: [f32; 3]) {
        let len = s1 - s0;
        let slope = (sigma1 - sigma0) / len;
        while self.next < 4 && self.rec_t[self.next] <= s1 {
            let x = (self.rec_t[self.next] - s0).max(0.0);
            let tr = self.t * (-(sigma0 + 0.5 * slope * x) * x).exp2();
            let k = self.rec_order[self.next];
            self.out_t[k] = tr;
            self.out_c[k] = std::array::from_fn(|i| self.c[i] + (self.t - tr) * col[i]);
            self.next += 1;
        }
        let tau = 0.5 * (sigma0 + sigma1) * len;
        let t_new = self.t * (-tau).exp2();
        for (c, &k) in self.c.iter_mut().zip(&col) {
            *c += (self.t - t_new) * k;
        }
        self.t = t_new;
        self.tau += tau;
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
        soft_pieces: 0,
        haze_pieces: 0,
        capped: false,
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
        let Some((ta, tb)) = band_chord(ray.eye[1], dir[1], p.walk) else {
            continue;
        };
        let tb = tb.min(t_end).min(layer.reach / dir_len);
        if ta >= tb {
            continue;
        }
        s.flush(ta);
        march_layer(&mut s, layer, ray, ta, tb, sky, max_steps);
    }
    s.flush(f32::INFINITY);
    MarchResult {
        transmittance: s.out_t,
        color: s.out_c,
        steps: s.steps,
        soft_pieces: s.soft_pieces,
        haze_pieces: s.haze_pieces,
        capped: s.capped,
    }
}

/// One layer's chord `[ta, tb]` of a ray (the shader's `march_layer`, segment for segment): the
/// cell walk up to where the cells end, or the rest of the chord in [`UNIFORM_PIECES`] pieces
/// for a layer whose every cell is full (up to its ring, then the ring through
/// [`full_ring_span`]); past the step budget the rest of the cells through the profile; then the
/// haze past the cells. Each iteration of the walk takes one segment in
/// pieces: a cell (one piece, or about one per block in the soft fog) or the rest of the chord.
fn march_layer(
    s: &mut State,
    layer: &MarchLayer,
    ray: &Ray,
    ta: f32,
    tb: f32,
    sky: [f32; 3],
    max_steps: u32,
) {
    let p = &layer.params;
    let (eye, dir) = (ray.eye, ray.dir);
    let dir_len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    let up = dir[1] > 0.0;
    let t_cells = layer.cells_end / dir_len;
    if ta >= t_cells {
        // Past the cells: only the haze, no walk.
        haze_span(s, layer, eye, dir, ta, tb, layer.haze, sky);
        return;
    }
    // The cells' part of the chord, and where they start to blend into the haze (only when the
    // layer goes on past them: the ring).
    let tc = tb.min(t_cells);
    let ring = layer.reach > layer.cells_end;
    let t_blend = layer.blend_start / dir_len;
    let color = |a: f32, b: f32, shade: f32| -> [f32; 3] {
        layer.color_at(0.5 * (a + b), shade, eye[1], dir[1], dir_len, sky)
    };
    // A cell that starts before this is in the soft fog (0: no soft fog).
    let soft_t = layer.soft[1] / dir_len;
    let mut dda = Dda::new(eye, dir, p, ta);
    let mut s0 = ta;
    let mut shade = 1.0;
    let mut entry = if ta > 0.0 {
        layer.face_shade[entry_face(1, if up { 1 } else { -1 })]
    } else {
        1.0
    };
    let mut prev_full = false;
    // Where the ring of a layer whose every cell is full starts (it blends after its body).
    let mut full_ring = None;
    loop {
        let (a, b, pieces, last);
        // The extinction of the rest of the chord, or the cell's coverage.
        let mut rest = None;
        let mut cov = 0.0;
        let mut soft = false;
        // How much of the cell is haze (in the ring).
        let mut blend = 0.0;
        let mut axis = 2;
        if s.steps >= max_steps {
            // Past the budget: the rest of the cells through the profile, unshaded.
            s.capped = true;
            haze_span(s, layer, eye, dir, s0, tc, 1.0, sky);
            break;
        } else if layer.uniform {
            // One body without inner faces: one step. With a ring the body ends where it
            // starts, and the rest of the cells blends into the haze after it.
            s.steps += 1;
            let tm = if ring { t_blend.max(s0).min(tc) } else { tc };
            if tm < tc {
                full_ring = Some(tm);
            }
            let pieces_body = if tm > s0 { UNIFORM_PIECES } else { 0 };
            (a, b, pieces, last) = (s0, tm, pieces_body, true);
            rest = Some(p.density_log2e);
            shade = entry;
        } else {
            s.steps += 1;
            soft = s0 < soft_t;
            // A lattice step whose corners leave no coverage is skipped whole (one step), but
            // not into the ring, where an empty cell still takes haze.
            let bound = noise::step_bound(p, dda.cell);
            let skip = if !soft && bound.is_empty(p) {
                let (jump_axis, t) = dda.step_exit(&bound);
                (!ring || t <= t_blend).then_some((jump_axis, t))
            } else {
                None
            };
            if let Some((jump_axis, t)) = skip {
                if t >= tc {
                    break;
                }
                let sj = t.max(s0);
                dda.jump(sj, jump_axis, &bound);
                entry = layer.face_shade[entry_face(jump_axis, dda.step[jump_axis])];
                prev_full = false;
                s0 = sj;
                if dda.cell[1] < p.walk_cells[0] || dda.cell[1] >= p.walk_cells[1] {
                    break;
                }
                continue;
            }
            let (exit_axis, t) = dda.exit();
            axis = exit_axis;
            (a, b) = (s0, t.clamp(s0, tc));
            let c = noise::coverage(p, dda.cell).coverage;
            cov = c as f32 / 4096.0;
            if c > 0 && !prev_full {
                shade = entry;
            }
            prev_full = c > 0;
            last = t >= tc;
            if ring && 0.5 * (a + b) > t_blend {
                // In the ring: one piece, blending into the haze by its middle.
                soft = false;
                blend = layer.blend_weight(0.5 * (a + b) * dir_len);
            }
            pieces = if soft {
                (((b - a) * dir_len / SOFT_PIECE).ceil() as u32).clamp(1, SOFT_MAX_PIECES)
            } else {
                1
            };
        }
        let piece = (b - a) / pieces as f32;
        if soft {
            s.soft_pieces += pieces;
        }
        for i in 0..pieces {
            let pa = a + i as f32 * piece;
            let pb = if i + 1 == pieces { b } else { pa + piece };
            let (sigma, sh) = match rest {
                Some(sigma) => (sigma, shade),
                None => {
                    let (mut c, mut sh) = (cov, shade);
                    let mid = 0.5 * (pa + pb);
                    if soft {
                        let w = layer.soft_weight(mid * dir_len);
                        if w > 0.0 {
                            let pos = std::array::from_fn(|k| eye[k] + mid * dir[k]);
                            c += (layer.soft_coverage(pos) - c) * w;
                            sh += (1.0 - sh) * w;
                        }
                    }
                    let mut sigma = p.density_log2e * c;
                    if blend > 0.0 {
                        let haze = layer.haze * layer.haze_at(eye[1] + mid * dir[1]);
                        sigma += (haze - sigma) * blend;
                        sh += (1.0 - sh) * blend;
                    }
                    (sigma, sh)
                }
            };
            if sigma > 0.0 {
                s.segment(pa, pb, sigma * dir_len, color(pa, pb, sh));
                if s.done {
                    return;
                }
            }
        }
        if last {
            break;
        }
        dda.advance(axis);
        entry = layer.face_shade[entry_face(axis, dda.step[axis])];
        s0 = b;
        if dda.cell[1] < p.walk_cells[0] || dda.cell[1] >= p.walk_cells[1] {
            break;
        }
    }
    if let Some(tm) = full_ring {
        full_ring_span(s, layer, eye, dir, tm, tc, shade, sky);
    }
    if !s.done && tb > t_cells {
        haze_span(s, layer, eye, dir, ta.max(t_cells), tb, layer.haze, sky);
    }
}

/// The ring `[a, b]` (view depth) of a layer whose every cell is full (the shader's
/// `full_ring`): [`UNIFORM_PIECES`] pieces, each blending a full cell and the body's `shade` into
/// the haze and 1 by its middle, as a cell of the ring does, so at the end of the cells the layer
/// is its haze.
#[expect(
    clippy::too_many_arguments,
    reason = "the shader's arguments, one call each"
)]
fn full_ring_span(
    s: &mut State,
    layer: &MarchLayer,
    eye: [f32; 3],
    dir: [f32; 3],
    a: f32,
    b: f32,
    shade: f32,
    sky: [f32; 3],
) {
    let dir_len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    let density = layer.params.density_log2e;
    let piece = (b - a) / UNIFORM_PIECES as f32;
    for i in 0..UNIFORM_PIECES {
        let pa = a + i as f32 * piece;
        let pb = if i + 1 == UNIFORM_PIECES {
            b
        } else {
            pa + piece
        };
        let mid = 0.5 * (pa + pb);
        let w = layer.blend_weight(mid * dir_len);
        let haze = layer.haze * layer.haze_at(eye[1] + mid * dir[1]);
        let sigma = density + (haze - density) * w;
        let sh = shade + (1.0 - shade) * w;
        if sigma > 0.0 {
            let col = layer.color_at(mid, sh, eye[1], dir[1], dir_len, sky);
            s.segment(pa, pb, sigma * dir_len, col);
            if s.done {
                return;
            }
        }
    }
}

/// The haze of `layer` over `[a, b]` (view depth) at `factor` times its profile (the shader's
/// `haze_span`). The chord is cut where the ray crosses a knot height and at the fade start, at
/// most [`HAZE_PIECES`] pieces: inside each the extinction is linear in `t`, so the piece and
/// every record inside it get their exact optical depth ([`State::segment_ramp`]). A piece's
/// color is taken at the point its light comes from on average (exact for a constant
/// extinction), without shade: haze has no faces.
#[expect(
    clippy::too_many_arguments,
    reason = "the shader's arguments, one call each"
)]
fn haze_span(
    s: &mut State,
    layer: &MarchLayer,
    eye: [f32; 3],
    dir: [f32; 3],
    a: f32,
    b: f32,
    factor: f32,
    sky: [f32; 3],
) {
    let sp = layer.profile.spacing;
    if b <= a || factor <= 0.0 || sp <= 0.0 {
        return;
    }
    let dir_len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    let lo = layer.haze_bottom();
    let flat = dir[1].abs() < MIN_DIR;
    let up = dir[1] > 0.0;
    let last = (HAZE_KNOTS - 1) as f32;
    let u = (eye[1] + a * dir[1] - lo) / sp;
    // The next knot the ray crosses: the one above it going up, below it going down.
    let mut j = if up {
        (u.floor() + 1.0).clamp(0.0, last + 1.0)
    } else {
        (u.ceil() - 1.0).clamp(-1.0, last)
    };
    let inv = 1.0 / if flat { 1.0 } else { dir[1] };
    let mut t_fade = layer.fade[0] / dir_len;
    if t_fade <= a {
        t_fade = FAR;
    }
    let mut p = a;
    for _ in 0..HAZE_PIECES {
        if p >= b {
            break;
        }
        let t_knot = if !flat && j >= 0.0 && j <= last {
            (lo + j * sp - eye[1]) * inv
        } else {
            FAR
        };
        let q = t_knot.min(t_fade).min(b).max(p);
        // The interval of knots the piece lies in.
        let k = if flat {
            u.floor().clamp(-1.0, last)
        } else if up {
            j - 1.0
        } else {
            j
        };
        if q > p && k >= 0.0 && k < last {
            let k = k as usize;
            let s0 = factor * layer.profile.at_interval(lo, k, eye[1] + p * dir[1]) * dir_len;
            let s1 = factor * layer.profile.at_interval(lo, k, eye[1] + q * dir[1]) * dir_len;
            if s0 > 0.0 || s1 > 0.0 {
                // Where the piece's light comes from on average: e of its length in, with
                // e = 1 / tau - T / (1 - T) (natural units), its series when tau is small.
                let len = q - p;
                let tau2 = 0.5 * (s0 + s1) * len;
                let tau = tau2 * std::f32::consts::LN_2;
                let e = if tau >= 0.01 {
                    let tr = (-tau2).exp2();
                    len * (1.0 / tau - tr / (1.0 - tr))
                } else {
                    len * (0.5 - tau / 12.0)
                };
                let col = layer.color_at(p + e, 1.0, eye[1], dir[1], dir_len, sky);
                s.haze_pieces += 1;
                s.segment_ramp(p, q, s0, s1, col);
                if s.done {
                    return;
                }
            }
        }
        if t_knot <= q {
            j += if up { 1.0 } else { -1.0 };
        }
        if t_fade <= q {
            t_fade = FAR;
        }
        p = q;
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

    /// The layer with a profile of the same mean coverage at every height.
    fn march_layer(l: &FogLayer, mean: f32) -> MarchLayer {
        MarchLayer::new(
            l,
            [0; 3],
            0,
            l.color,
            [0.0, 1.0],
            HazeProfile::level(l, mean),
        )
    }

    /// The same layer walked cell by cell (as if it were not uniform).
    fn walked(l: &FogLayer, mean: f32) -> MarchLayer {
        MarchLayer {
            uniform: false,
            ..march_layer(l, mean)
        }
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
        let ml = walked(&layer, 1.0);
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
        let ml = walked(&layer, 1.0);
        let ray = Ray {
            eye: [0.5, 5.5, 0.5],
            dir: [0.0, 0.0, 1.0],
            depths: [100.0; 4],
        };
        // The profile of a full slab is its density at every height.
        assert_eq!(crate::haze_profile(&layer), ml.profile);
        let exact = march(&ray, &[ml], [0.0; 3], 256);
        let capped = march(&ray, &[ml], [0.0; 3], 10);
        let whole = march(&ray, &[march_layer(&layer, 1.0)], [0.0; 3], 10);
        assert_eq!(whole.steps, 1, "a uniform layer is one step");
        assert!((whole.transmittance[0] - exact.transmittance[0]).abs() < 1e-4);
        assert_eq!(capped.steps, 10);
        // The counters of the step table: only the capped ray takes the tail, in one piece (a
        // level ray crosses no knot).
        assert_eq!((exact.capped, exact.haze_pieces), (false, 0));
        assert_eq!((capped.capped, capped.haze_pieces), (true, 1));
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
        for ml in [march_layer(&layer, 1.0), walked(&layer, 1.0)] {
            check_shades(ml);
        }
    }

    fn check_shades(ml: MarchLayer) {
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

    /// The skip of empty lattice steps loses no fog: a sparse layer keeps exactly what the
    /// cells of a plain walk give.
    #[test]
    fn skipping_empty_steps_loses_no_fog() {
        let mut layer = slab(-16, 16, [2, 2, 2], 0.3);
        layer.noise.dims = NoiseDims::Volume;
        layer.noise.scale = 4;
        layer.threshold = 0.7;
        layer.max_distance = 200.0;
        let ml = march_layer(&layer, 0.1);
        let p = &ml.params;
        let eye = [0.5, 0.5, 0.5];
        let mut skipped = false;
        for dir in [[0.3f32, 0.05, 1.0], [-1.0, -0.2, 0.4], [0.7, 0.3, -1.0]] {
            let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
            let (t0, t1) = band_chord(eye[1], dir[1], p.band).expect("chord");
            let t1 = t1.min(ml.cells_end / len);
            let mut tau = 0.0;
            for v in walk(eye, dir, p, t0, t1, 100_000) {
                let cov = noise::coverage(p, v.cell).coverage as f32 / 4096.0;
                tau += p.density_log2e * cov * (v.t1 - v.t0) * len;
                skipped |= noise::step_bound(p, v.cell).is_empty(p);
            }
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
            let want = (-tau).exp2();
            assert!(
                (r.transmittance[0] - want).abs() < 2e-4
                    || (want < 0.003 && r.transmittance[0] < 0.003),
                "{dir:?}: {} vs {want}",
                r.transmittance[0]
            );
            assert!(r.steps < 256, "{dir:?}: {} steps", r.steps);
        }
        assert!(skipped, "some ray crosses an empty step");
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
            soft_pieces: 0,
            haze_pieces: 0,
            capped: false,
        };
        let [c, a] = texel(&r);
        assert_eq!(a & 0xff, 0);
        assert_eq!(a >> 24, 255);
        let color = crate::pack::decode_color(c);
        assert!((color[0] - 1.0).abs() < 1e-3);
    }

    /// One ray's transmittance and color per unit opacity, to the sky.
    fn look(ml: &MarchLayer, eye: [f32; 3], dir: [f32; 3]) -> (f32, f32) {
        let r = march(
            &Ray {
                eye,
                dir,
                depths: [f32::INFINITY; 4],
            },
            std::slice::from_ref(ml),
            [0.0; 3],
            256,
        );
        (
            r.transmittance[0],
            r.color[0][0] / (1.0 - r.transmittance[0]),
        )
    }

    /// A plain layer has no face shading: a slab looks the same from above, below and inside.
    #[test]
    fn fog_has_no_faces_by_default() {
        let layer = slab(8, 16, [1, 1, 1], 0.05);
        for ml in [march_layer(&layer, 1.0), walked(&layer, 1.0)] {
            for (eye, dir) in [
                ([0.5, 30.5, 0.5], [0.1, -1.0, 0.0]),
                ([0.5, 0.5, 0.5], [0.1, 1.0, 0.05]),
                ([-20.5, 12.5, 0.5], [1.0, 0.01, 0.3]),
            ] {
                let (t, shade) = look(&ml, eye, dir);
                assert!(t < 0.9, "{eye:?}: some fog");
                assert!((shade - 1.0).abs() < 1e-4, "{eye:?}: shade {shade}");
            }
        }
    }

    /// The height shade goes from the bottom's brightness to the top's: a horizontal ray along
    /// a row sees the brightness of its height.
    #[test]
    fn the_height_shades_the_band() {
        let mut layer = slab(8, 16, [1, 1, 1], 0.2);
        layer.height_shade = [0.5, 1.0];
        for ml in [march_layer(&layer, 1.0), walked(&layer, 1.0)] {
            for y in [8.5f32, 12.0, 15.5] {
                let (_, shade) = look(&ml, [0.5, y, 0.5], [0.0, 0.0, 1.0]);
                let want = 0.5 + 0.5 * (y - 8.0) / 8.0;
                assert!((shade - want).abs() < 1e-4, "y {y}: {shade} vs {want}");
            }
        }
    }

    /// A soft noisy layer for the soft fog tests.
    fn soft_bank(soft: f32) -> FogLayer {
        let mut l = slab(-16, 16, [2, 2, 2], 0.3);
        l.noise.dims = NoiseDims::Volume;
        l.noise.scale = 4;
        l.noise.octaves = 2;
        l.threshold = 0.3;
        l.softness = 0.5;
        l.soft_distance = soft;
        l
    }

    /// The middle of a cell reads the cell's own coverage: the soft fog is the cells' field
    /// read between their middles.
    #[test]
    fn a_cell_middle_reads_its_cell() {
        let ml = march_layer(&soft_bank(8.0), 0.5);
        let p = &ml.params;
        for x in -20..20 {
            for (y, z) in [(-3, 5), (0, -7), (2, 0)] {
                let d = [x, y, z];
                let pos = std::array::from_fn(|a| p.grid[a] + (d[a] as f32 + 0.5) * 2.0);
                assert_eq!(
                    ml.soft_coverage(pos),
                    noise::coverage(p, d).coverage as f32 / 4096.0,
                    "{d:?}"
                );
            }
        }
    }

    /// Inside the soft distance the march integrates the noise read along the ray: it matches
    /// a fine numerical integral of [`MarchLayer::soft_coverage`], cell edges and all.
    #[test]
    fn the_soft_fog_integrates_the_noise_along_the_ray() {
        let ml = march_layer(&soft_bank(16.0), 0.5);
        let eye = [0.3, 0.7, 0.2];
        let mut least = 1.0f32;
        for dir in [
            [0.3f32, 0.1, 1.0],
            [-1.0, -0.2, 0.4],
            [0.2, 0.9, -0.3],
            [0.7, -0.4, -0.6],
        ] {
            let dir = norm(dir);
            let depth = 12.0;
            let r = march(
                &Ray {
                    eye,
                    dir,
                    depths: [depth; 4],
                },
                &[ml],
                [0.0; 3],
                256,
            );
            let n = 12_000;
            let dt = depth / n as f32;
            let tau: f32 = (0..n)
                .map(|i| {
                    let t = (i as f32 + 0.5) * dt;
                    let pos = std::array::from_fn(|a| eye[a] + t * dir[a]);
                    ml.params.density_log2e * ml.soft_coverage(pos) * dt
                })
                .sum();
            let want = (-tau).exp2();
            assert!(
                (r.transmittance[0] - want).abs() < 3e-3,
                "{dir:?}: {} vs {want}",
                r.transmittance[0]
            );
            least = least.min(want);
        }
        assert!(least < 0.8, "some ray crosses fog ({least})");
    }

    /// Past the soft distance and its blend the fog is the cells' again, bit for bit.
    #[test]
    fn the_soft_fog_ends_past_its_blend() {
        let soft = march_layer(&soft_bank(8.0), 0.5);
        let cubic = march_layer(&soft_bank(0.0), 0.5);
        assert_eq!(soft.soft, [8.0, 12.0]);
        assert_eq!(cubic.soft, [0.0; 2]);
        // From 30 blocks above the band: it starts more than 12 blocks away.
        let ray = Ray {
            eye: [0.5, 46.0, 0.5],
            dir: [0.3, -1.0, 0.6],
            depths: [f32::INFINITY, 40.0, 50.0, 70.0],
        };
        let a = march(&ray, &[soft], [0.2, 0.3, 0.9], 256);
        let b = march(&ray, &[cubic], [0.2, 0.3, 0.9], 256);
        assert_eq!(a, b);
        // From inside, the soft fog changes what the near cells give.
        let near = Ray {
            eye: [0.5, 0.5, 0.5],
            ..ray
        };
        assert_ne!(
            march(&near, &[soft], [0.0; 3], 256),
            march(&near, &[cubic], [0.0; 3], 256)
        );
    }

    /// The soft fog's pieces are bounded: a ray that crosses the whole soft zone of a dense
    /// noisy layer costs at most its cells plus a piece per block.
    #[test]
    fn the_soft_fog_is_bounded() {
        let mut layer = soft_bank(32.0);
        layer.density = 0.001;
        let ml = march_layer(&layer, 0.5);
        let r = march(
            &Ray {
                eye: [0.5, 0.5, 0.5],
                dir: norm([0.6, 0.0, 0.8]),
                depths: [f32::INFINITY; 4],
            },
            &[ml],
            [0.0; 3],
            40,
        );
        assert_eq!(r.steps, 40, "the budget counts cells, not pieces");
        // Pieces of at most a block cover the soft fog and its blend (48 blocks), at most one
        // more per cell.
        assert!(
            (48..48 + 40 + SOFT_MAX_PIECES).contains(&r.soft_pieces),
            "{r:?}"
        );
    }

    /// A rising threshold is never a uniform layer, even with threshold and softness 0.
    #[test]
    fn a_rise_is_not_uniform() {
        let mut layer = slab(0, 16, [2, 2, 2], 0.3);
        assert!(march_layer(&layer, 1.0).uniform);
        layer.threshold_rise = 0.25;
        assert!(!march_layer(&layer, 1.0).uniform);
    }

    /// The layers and rays of `haze_0_draws_as_before`: every kind of layer the march walks,
    /// near the origin and far from it, rays that stay inside the budget.
    fn golden_scenes(
        make: &dyn Fn(&FogLayer, [i64; 3], [f32; 2]) -> MarchLayer,
    ) -> Vec<(&'static str, Vec<MarchLayer>, Vec<Ray>)> {
        let soft = soft_bank(16.0);
        let mut cumulus = slab(24, 60, [6, 6, 6], 0.35);
        cumulus.noise.dims = NoiseDims::Volume;
        cumulus.noise.scale = 4;
        cumulus.noise.octaves = 2;
        cumulus.noise.seed = 31;
        cumulus.threshold = 0.5;
        cumulus.threshold_rise = 0.35;
        (cumulus.edge_bottom, cumulus.edge_top) = (6, 12);
        cumulus.softness = 0.2;
        cumulus.height_shade = [0.6, 1.0];
        cumulus.soft_distance = 10.0;
        cumulus.wind = [2.0, 0.0, 0.0];
        cumulus.max_distance = 400.0;
        cumulus.fade = FogFade::Range {
            start: 250.0,
            end: 400.0,
        };
        let mut flat = slab(24, 28, [6, 4, 6], 0.6);
        flat.noise.scale = 2;
        flat.noise.seed = 5;
        flat.threshold = 0.5;
        flat.max_distance = 400.0;
        flat.fade = FogFade::Range {
            start: 250.0,
            end: 400.0,
        };
        let full = slab(-8, 2, [1, 1, 1], 0.2);
        let mut scene = soft_bank(0.0);
        scene.fade = FogFade::Scene;
        scene.max_distance = 120.0;
        let far = [(1 << 31) - 5, 70, -(1 << 31) + 3];
        let fan = |eye: [f32; 3], depths: [f32; 4]| -> Vec<Ray> {
            let mut out = Vec::new();
            for i in 0..24 {
                let a = i as f32 * 0.27;
                let e = -0.6 + (i % 7) as f32 * 0.2;
                out.push(Ray {
                    eye,
                    dir: [a.sin(), e, a.cos()],
                    depths: [
                        depths[i % 4],
                        depths[(i + 1) % 4],
                        depths[(i + 2) % 4],
                        depths[3],
                    ],
                });
            }
            out
        };
        let depths = [7.5, 23.0, 61.25, f32::INFINITY];
        vec![
            (
                "soft bank",
                vec![make(&soft, [0; 3], [0.0, 1.0])],
                fan([0.3, 0.7, 0.2], depths),
            ),
            (
                "soft bank far",
                vec![make(&soft, far, [0.0, 1.0])],
                fan([0.3, 0.7, 0.2], depths),
            ),
            (
                "cumulus",
                vec![make(&cumulus, [0; 3], [0.0, 1.0])],
                fan([3.3, 35.5, 2.7], depths)
                    .into_iter()
                    .chain(fan([0.5, 10.0, 0.5], [30.0, 90.0, 200.0, f32::INFINITY]))
                    .collect(),
            ),
            (
                "flat clouds and a full slab",
                vec![
                    make(&full, [0; 3], [0.0, 1.0]),
                    make(&flat, [0; 3], [0.0, 1.0]),
                ],
                fan([0.5, 10.0, 0.5], [30.0, 90.0, 200.0, f32::INFINITY])
                    .into_iter()
                    .chain(fan([0.5, -3.0, 0.5], depths))
                    .collect(),
            ),
            (
                "scene fade",
                vec![make(&scene, [0; 3], [100.0, 150.0])],
                fan([0.3, 0.7, 0.2], [20.0, 90.0, 140.0, f32::INFINITY]),
            ),
        ]
    }

    /// FNV-1a over the bits of a march's results.
    fn golden_hash(results: &[MarchResult]) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut eat = |w: u32| {
            for b in w.to_le_bytes() {
                h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
        };
        for r in results {
            eat(r.steps);
            for k in 0..4 {
                eat(r.transmittance[k].to_bits());
                for c in 0..3 {
                    eat(r.color[k][c].to_bits());
                }
            }
        }
        h
    }

    /// A layer without haze draws bit for bit what the march drew before the haze existed
    /// (feat/lod 0bd2849), on rays that stay inside the step budget: every kind of layer, near
    /// the origin and far from it. Except the walk of the edged cumulus, which since the fog's
    /// M2 skips the always empty rows along its planes (its hash was `0x3b9d_aed0_bfca_7613`).
    #[test]
    fn haze_0_draws_as_before() {
        let want = [
            ("soft bank", 0xbe13_1c0c_cc28_677d_u64),
            ("soft bank far", 0x1329_67d9_29c4_d600),
            ("cumulus", 0x3552_b81e_a192_c14e),
            ("flat clouds and a full slab", 0x7927_cb21_f71f_bb28),
            ("scene fade", 0xf7f9_683d_3bca_7304),
        ];
        // The profile only feeds the haze and the budget's tail: none of it shows here.
        let make = |l: &FogLayer, origin: [i64; 3], fade: [f32; 2]| {
            MarchLayer::new(
                l,
                origin,
                1234,
                [0.9, 0.92, 1.0],
                fade,
                HazeProfile::level(l, 0.5),
            )
        };
        for ((name, layers, rays), (want_name, hash)) in golden_scenes(&make).into_iter().zip(want)
        {
            assert_eq!(name, want_name);
            let results: Vec<MarchResult> = rays
                .iter()
                .map(|r| march(r, &layers, [0.2, 0.3, 0.9], 256))
                .collect();
            assert!(results.iter().all(|r| r.steps < 256), "{name}");
            assert!(results.iter().any(|r| r.transmittance[3] < 0.9), "{name}");
            assert_eq!(golden_hash(&results), hash, "{name}");
        }
    }

    /// A noisy bank over 0..24 of 2-block cells, with or without edges, its haze drawn far.
    fn hazy_bank(edges: (u32, u32), density: f32) -> FogLayer {
        let mut l = slab(0, 24, [2, 2, 2], density);
        l.noise.dims = NoiseDims::Volume;
        l.noise.scale = 4;
        l.noise.octaves = 2;
        l.noise.seed = 404;
        l.threshold = 0.4;
        l.softness = 0.3;
        (l.edge_bottom, l.edge_top) = edges;
        l.haze = 1.0;
        l.fade = FogFade::Range {
            start: 1500.0,
            end: 2000.0,
        };
        l
    }

    /// The layer's march data with its real profile.
    fn hazy(l: &FogLayer) -> MarchLayer {
        MarchLayer::new(l, [0; 3], 0, l.color, [0.0, 1.0], crate::haze_profile(l))
    }

    /// The optical depth (`log2`) of the haze along `[t0, t1]` of a ray, by fine midpoint
    /// pieces of the profile.
    fn dense_haze(ml: &MarchLayer, eye: [f32; 3], dir: [f32; 3], t0: f32, t1: f32) -> f64 {
        let len = f64::from((dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt());
        let n = 100_000;
        let dt = f64::from(t1 - t0) / f64::from(n);
        (0..n)
            .map(|i| {
                let t = f64::from(t0) + (f64::from(i) + 0.5) * dt;
                let y = (f64::from(eye[1]) + t * f64::from(dir[1])) as f32;
                f64::from(ml.haze * ml.haze_at(y)) * dt * len
            })
            .sum()
    }

    /// The haze of a chord matches a fine integral of its profile, on steep, grazing and
    /// horizontal rays, with and without edges, at haze 1 and less.
    #[test]
    fn haze_span_matches_dense_sampling() {
        for (edges, haze) in [((4, 8), 1.0), ((0, 0), 1.0), ((4, 8), 0.4)] {
            let mut layer = hazy_bank(edges, 0.003);
            layer.haze = haze;
            // Every chord is haze: the cells end at the camera.
            let ml = MarchLayer {
                cells_end: 0.0,
                blend_start: 0.0,
                ..hazy(&layer)
            };
            assert_eq!(ml.reach, 2000.0);
            for (eye, dir) in [
                ([0.3f32, 10.0, 0.7], [0.0f32, -1.0, 0.0]),
                ([0.3, 40.0, 0.7], [0.2, -1.0, 0.1]),
                ([0.3, -9.0, 0.7], [0.1, 1.0, -0.3]),
                ([0.3, 10.0, 0.7], [1.0, 0.02, 0.0]),
                ([0.3, 23.0, 0.7], [0.6, -0.013, 0.8]),
                ([0.3, 25.0, 0.7], [0.6, -0.03, 0.8]),
                ([0.3, 12.5, 0.7], [0.0, 0.0, 1.0]),
                ([0.3, 3.5, 0.7], [-0.7, 0.0, 0.7]),
            ] {
                let dir = norm(dir);
                let (t0, t1) = band_chord(eye[1], dir[1], ml.params.band).expect("chord");
                let t1 = t1.min(ml.reach);
                let want = dense_haze(&ml, eye, dir, t0, t1);
                let r = march(
                    &Ray {
                        eye,
                        dir,
                        depths: [f32::INFINITY; 4],
                    },
                    &[ml],
                    [0.0; 3],
                    64,
                );
                let got = -f64::from(r.transmittance[0]).log2();
                assert_eq!(r.steps, 0, "no cell");
                assert!(want > 1e-3, "{eye:?} {dir:?}: some haze ({want})");
                assert!(
                    (got - want).abs() <= 1e-3 * want,
                    "{edges:?} {eye:?} {dir:?}: {got} vs {want}"
                );
            }
        }
    }

    /// A record inside a piece of haze takes the transmittance of a march cut at its depth,
    /// though the extinction varies along the piece.
    #[test]
    fn haze_records_take_their_own_depth() {
        let mut layer = hazy_bank((4, 8), 0.05);
        layer.fade = FogFade::Range {
            start: 5000.0,
            end: 8000.0,
        };
        let ml = MarchLayer {
            cells_end: 0.0,
            blend_start: 0.0,
            reach: 2000.0,
            ..hazy(&layer)
        };
        let eye = [0.3, 30.0, 0.7];
        let dir = norm([0.4, -0.05, 1.0]);
        let depths = [190.0, 260.5, f32::INFINITY, 333.25];
        let all = march(&Ray { eye, dir, depths }, &[ml], [0.2, 0.3, 0.9], 64);
        for (k, &depth) in depths.iter().enumerate() {
            let one = march(
                &Ray {
                    eye,
                    dir,
                    depths: [depth; 4],
                },
                &[ml],
                [0.2, 0.3, 0.9],
                64,
            );
            assert!(
                (one.transmittance[0] - all.transmittance[k]).abs() < 1e-5,
                "{k}: {} vs {}",
                one.transmittance[0],
                all.transmittance[k]
            );
            for c in 0..3 {
                assert!((one.color[0][c] - all.color[k][c]).abs() < 1e-5, "{k}");
            }
        }
        let t = all.transmittance;
        assert!(t[1] < 0.99 && t[3] < t[1] && t[2] < t[3], "{t:?}");
    }

    /// Where the cells end, their extinction meets the haze's: the cells of the ring blend into
    /// it, so a thin slab across the end sees about the haze, not the last cell's own coverage
    /// (which it does without the ring).
    #[test]
    fn the_far_blend_is_continuous() {
        let mut layer = hazy_bank((4, 8), 0.03);
        layer.max_distance = 64.0;
        let ml = hazy(&layer);
        assert_eq!(
            (ml.cells_end, ml.blend_start, ml.reach),
            (64.0, 48.0, 2000.0)
        );
        let no_ring = MarchLayer {
            blend_start: ml.cells_end,
            ..ml
        };
        let d = ml.params.density_log2e;
        // Extinction per block across the end and the haze's there, over a fan of rays.
        let across = |m: &MarchLayer, i: u32| -> (f32, f32) {
            let a = i as f32 * 0.091;
            let eye = [0.3 + i as f32, 12.0 + (i % 5) as f32, 0.7];
            let dir = norm([a.sin(), 0.002 * (i % 3) as f32 - 0.002, a.cos()]);
            let (near, far) = (63.75, 64.25);
            let r = march(
                &Ray {
                    eye,
                    dir,
                    depths: [near, far, far, far],
                },
                std::slice::from_ref(m),
                [0.0; 3],
                256,
            );
            assert!(r.transmittance[1] > 0.05, "{i}: thin enough to see the end");
            let sigma = (r.transmittance[0] / r.transmittance[1]).log2() / (far - near);
            (sigma, m.haze * m.haze_at(eye[1] + 64.0 * dir[1]))
        };
        let (mut worst, mut worst_no_ring) = (0.0f32, 0.0f32);
        for i in 0..64 {
            let (sigma, haze) = across(&ml, i);
            worst = worst.max((sigma - haze).abs());
            let (sigma, haze) = across(&no_ring, i);
            worst_no_ring = worst_no_ring.max((sigma - haze).abs());
        }
        assert!(worst < 0.05 * d, "{worst} of {d}");
        assert!(
            worst_no_ring > 0.2 * d,
            "{worst_no_ring} of {d}: the test bites"
        );
    }

    /// A layer whose every cell is full blends into its haze too: across the end of its cells
    /// its extinction meets a haze of half its density, and a slab seen through a shaded face
    /// takes the haze's shade (1) there, not a step from the face's.
    #[test]
    fn a_full_layer_blends_into_its_haze_too() {
        let mut layer = slab(0, 24, [2, 2, 2], 0.03);
        layer.face_shade = [1.0, 0.5, 0.8, 0.65];
        layer.color = [1.0; 3];
        layer.max_distance = 64.0;
        layer.haze = 0.5;
        layer.fade = FogFade::Range {
            start: 1500.0,
            end: 2000.0,
        };
        let ml = hazy(&layer);
        assert!(ml.uniform);
        assert_eq!(
            (ml.cells_end, ml.blend_start, ml.reach),
            (64.0, 48.0, 2000.0)
        );
        let no_ring = MarchLayer {
            blend_start: ml.cells_end,
            ..ml
        };
        let d = ml.params.density_log2e;
        // Extinction per block and color per unit opacity across the end, and the haze's there:
        // level rays inside the band (shade 1) and rays rising into it from below (through the
        // bottom faces, shade 0.5).
        let across = |m: &MarchLayer, i: u32| -> (f32, f32, f32) {
            let a = i as f32 * 0.091;
            let below = i % 2 == 1;
            let eye = if below {
                [0.3 + i as f32, -2.0, 0.7]
            } else {
                [0.3 + i as f32, 12.0 + (i % 5) as f32, 0.7]
            };
            let rise = if below { 0.06 } else { 0.0 };
            let dir = norm([a.sin(), rise, a.cos()]);
            let (near, far) = (63.75, 64.25);
            let r = march(
                &Ray {
                    eye,
                    dir,
                    depths: [near, far, far, far],
                },
                std::slice::from_ref(m),
                [0.0; 3],
                256,
            );
            assert_eq!(r.steps, 1, "{i}: one step");
            assert!(r.transmittance[1] > 0.05, "{i}: thin enough to see the end");
            let sigma = (r.transmittance[0] / r.transmittance[1]).log2() / (far - near);
            let shade = (r.color[1][0] - r.color[0][0]) / (r.transmittance[0] - r.transmittance[1]);
            (sigma, m.haze * m.haze_at(eye[1] + 64.0 * dir[1]), shade)
        };
        let (mut worst, mut worst_shade) = (0.0f32, 0.0f32);
        let (mut worst_no_ring, mut worst_shade_no_ring) = (0.0f32, 0.0f32);
        for i in 0..64 {
            let (sigma, haze, shade) = across(&ml, i);
            assert!((haze - 0.5 * d).abs() < 1e-6 * d, "{i}: {haze}");
            worst = worst.max((sigma - haze).abs());
            worst_shade = worst_shade.max((shade - 1.0).abs());
            let (sigma, haze, shade) = across(&no_ring, i);
            worst_no_ring = worst_no_ring.max((sigma - haze).abs());
            worst_shade_no_ring = worst_shade_no_ring.max((shade - 1.0).abs());
        }
        assert!(worst < 0.05 * d, "{worst} of {d}");
        assert!(worst_shade < 0.02, "shade {worst_shade}");
        assert!(
            worst_no_ring > 0.2 * d && worst_shade_no_ring > 0.2,
            "{worst_no_ring} of {d}, shade {worst_shade_no_ring}: the test bites"
        );
        // Up to just past the end, the layer taken whole draws about what a walk of its cells
        // does (each cell of the ring blended by its middle), from inside the band and from a
        // ray that enters it in the ring (no body).
        for (eye, dir) in [
            ([0.3, 12.0, 0.7], [0.0, 0.0, 1.0]),
            ([0.3, 40.0, 0.7], norm([0.0, -0.3, 1.0])),
        ] {
            let ray = Ray {
                eye,
                dir,
                depths: [64.25; 4],
            };
            let whole = march(&ray, &[ml], [0.0; 3], 256);
            let walked = march(
                &ray,
                &[MarchLayer {
                    uniform: false,
                    ..ml
                }],
                [0.0; 3],
                256,
            );
            assert!(
                (whole.transmittance[0] - walked.transmittance[0]).abs() < 2e-3
                    && (whole.color[0][0] - walked.color[0][0]).abs() < 2e-3,
                "{eye:?}: {whole:?} vs {walked:?}"
            );
            assert!(whole.transmittance[0] < 0.995, "{eye:?}: some fog");
        }
    }

    /// The skip of empty lattice steps never jumps into the ring: there an empty cell still
    /// takes haze. The march keeps what a plain walk of the cells (blended in the ring) and the
    /// haze past them give.
    #[test]
    fn no_skip_into_the_ring() {
        let mut layer = slab(-16, 16, [2, 2, 2], 0.3);
        layer.noise.dims = NoiseDims::Volume;
        layer.noise.scale = 4;
        layer.threshold = 0.7;
        layer.max_distance = 200.0;
        layer.haze = 1.0;
        layer.fade = FogFade::Range {
            start: 1000.0,
            end: 4000.0,
        };
        let ml = hazy(&layer);
        let p = &ml.params;
        assert_eq!((ml.cells_end, ml.blend_start), (200.0, 150.0));
        let eye = [0.5, 0.5, 0.5];
        let (mut skipped, mut empty_in_ring) = (false, 0);
        for dir in [[0.3f32, 0.05, 1.0], [-1.0, -0.04, 0.4], [0.7, 0.02, -1.0]] {
            let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
            let (t0, t1) = band_chord(eye[1], dir[1], p.band).expect("chord");
            let t1 = t1.min(ml.reach / len);
            let (t_cells, t_blend) = (ml.cells_end / len, ml.blend_start / len);
            assert!(t1 > t_cells, "{dir:?}: the haze is in view");
            let mut tau = 0.0;
            for v in walk(eye, dir, p, t0, t1.min(t_cells), 100_000) {
                let cov = noise::coverage(p, v.cell).coverage as f32 / 4096.0;
                let mut sigma = p.density_log2e * cov;
                let mid = 0.5 * (v.t0 + v.t1);
                let empty = noise::step_bound(p, v.cell).is_empty(p);
                if mid > t_blend {
                    let haze = ml.haze * ml.haze_at(eye[1] + mid * dir[1]);
                    sigma += (haze - sigma) * ml.blend_weight(mid * len);
                    empty_in_ring += usize::from(empty);
                } else {
                    skipped |= empty;
                }
                tau += f64::from(sigma * (v.t1 - v.t0) * len);
            }
            tau += dense_haze(&ml, eye, dir, t_cells, t1);
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
            let want = (-tau).exp2() as f32;
            assert!(
                (r.transmittance[0] - want).abs() < 2e-4
                    || (want < 0.003 && r.transmittance[0] < 0.003),
                "{dir:?}: {} vs {want}",
                r.transmittance[0]
            );
            assert!(r.steps < 256, "{dir:?}: {} steps", r.steps);
        }
        assert!(skipped, "some ray skips an empty step before the ring");
        assert!(empty_in_ring > 0, "some empty step reaches into the ring");
    }

    /// The row along a plane with an edge is always empty, so the walk leaves it out: a ray
    /// along it takes no step and draws nothing, as the cells it would have walked had none.
    /// Without the edge the same ray walks that row.
    #[test]
    fn rows_along_edged_planes_are_not_walked() {
        let mut layer = hazy_bank((4, 8), 0.5);
        (layer.haze, layer.soft_distance) = (0.0, 0.0);
        let p = LayerParams::new(&layer, [0; 3], 0);
        assert_eq!((p.walk, p.walk_cells), ([2.0, 22.0], [1, 11]));
        assert_eq!((p.band, p.band_cells), ([0.0, 24.0], [0, 12]));
        let ray = |y: f32| Ray {
            eye: [0.5, y, 0.5],
            dir: norm([1.0, 0.0, 0.3]),
            depths: [300.0; 4],
        };
        for y in [0.5, 1.5, 22.5, 23.5] {
            let r = march(&ray(y), &[hazy(&layer)], [0.0; 3], 64);
            assert_eq!((r.steps, r.transmittance), (0, [1.0; 4]), "y {y}");
            // The rows' coverage the walk would have read: none.
            let row = (y / 2.0).floor() as i32;
            for x in 0..150 {
                let c = noise::coverage(&p, [x, row, x / 3]);
                assert_eq!(c.coverage, 0, "y {y} cell {x}");
            }
        }
        // A ray inside the walked rows still walks them; without edges the plane rows too.
        assert!(march(&ray(11.0), &[hazy(&layer)], [0.0; 3], 64).steps > 0);
        // Two rows, both along an edged plane: nothing left to walk.
        let mut thin = layer.clone();
        (thin.max_y, thin.edge_bottom, thin.edge_top) = (4, 2, 2);
        let p = LayerParams::new(&thin, [0; 3], 0);
        assert_eq!((p.walk, p.walk_cells), ([2.0, 2.0], [1, 1]));
        for y in [0.5, 2.0, 3.5] {
            assert_eq!(march(&ray(y), &[hazy(&thin)], [0.0; 3], 64).steps, 0);
        }
        (layer.edge_bottom, layer.edge_top) = (0, 0);
        let p = LayerParams::new(&layer, [0; 3], 0);
        assert_eq!((p.walk, p.walk_cells), (p.band, p.band_cells));
        assert!(march(&ray(1.5), &[hazy(&layer)], [0.0; 3], 64).steps > 0);
    }

    /// A layer with no row left to walk (one row along an edged plane, or a band clamped far
    /// from the origin) walks no cell, and a walk started on it puts the y cell just under it
    /// instead of panicking.
    #[test]
    fn an_empty_walk_visits_nothing() {
        let mut one_row = slab(0, 16, [16; 3], 0.3);
        one_row.edge_bottom = 4;
        assert_eq!(crate::config::validate(&[one_row.clone()], 64), Ok(()));
        let far = slab(0, 16, [16; 3], 0.3);
        let cases = [
            (LayerParams::new(&one_row, [0; 3], 0), [1, 1]),
            (LayerParams::new(&far, [0, 1 << 40, 0], 0), [-(1 << 30); 2]),
        ];
        for (p, cells) in cases {
            assert_eq!(p.walk_cells, cells);
            for dir in [
                [1.0, 0.0, 0.0],
                norm([0.3, 1.0, 0.2]),
                norm([0.3, -1.0, 0.2]),
            ] {
                assert_eq!(walk([0.5, 8.0, 0.5], dir, &p, 0.0, 10.0, 10), []);
                let dda = Dda::new([0.5, 8.0, 0.5], dir, &p, 0.0);
                assert_eq!(dda.cell[1], cells[0] - 1, "{dir:?}");
            }
        }
    }

    /// A chord that starts past the cells is haze only: no cell, no step.
    #[test]
    fn haze_only_chords_need_no_cells() {
        let mut layer = hazy_bank((4, 8), 0.05);
        (layer.min_y, layer.max_y) = (100, 124);
        layer.max_distance = 50.0;
        let ml = hazy(&layer);
        let r = march(
            &Ray {
                eye: [0.5, 0.5, 0.5],
                dir: norm([0.0, 0.5, 1.0]),
                depths: [f32::INFINITY; 4],
            },
            &[ml],
            [0.0; 3],
            64,
        );
        assert_eq!(r.steps, 0);
        assert!(!r.capped && r.haze_pieces >= 1, "{r:?}");
        assert!(r.transmittance[0] < 0.9, "{r:?}");
        // Without haze the layer ends with its cells, out of this ray's reach.
        layer.haze = 0.0;
        let r = march(
            &Ray {
                eye: [0.5, 0.5, 0.5],
                dir: norm([0.0, 0.5, 1.0]),
                depths: [f32::INFINITY; 4],
            },
            &[hazy(&layer)],
            [0.0; 3],
            64,
        );
        assert_eq!((r.steps, r.transmittance), (0, [1.0; 4]));
    }

    /// Where the cells end and how far the layer reaches, per fade and haze.
    #[test]
    fn cells_end_and_reach_per_fade_and_haze() {
        let ends = |max_distance: f32, fade: FogFade, scene: [f32; 2], haze: f32| {
            let mut l = FogLayer::new(0, 8);
            (l.max_distance, l.fade, l.haze) = (max_distance, fade, haze);
            let m = MarchLayer::new(&l, [0; 3], 0, [1.0; 3], scene, HazeProfile::level(&l, 1.0));
            (m.cells_end, m.reach, m.blend_start)
        };
        let scene = FogFade::Scene;
        let range = |start, end| FogFade::Range { start, end };
        // Like the terrain, at 16 chunks: the haze reaches the view's end.
        assert_eq!(
            ends(384.0, scene, [460.8, 512.0], 0.0),
            (384.0, 384.0, 384.0)
        );
        assert_eq!(
            ends(384.0, scene, [460.8, 512.0], 1.0),
            (384.0, 512.0, 288.0)
        );
        assert_eq!(
            ends(384.0, scene, [512.0, 512.0], 0.25),
            (384.0, 512.0, 288.0)
        );
        // The view ends first: no haze, no ring.
        assert_eq!(
            ends(384.0, scene, [306.0, 340.0], 1.0),
            (340.0, 340.0, 340.0)
        );
        // A range that ends with the cells, or past them.
        let cumulus = range(384.0, 768.0);
        assert_eq!(ends(768.0, cumulus, [0.0, 1.0], 1.0), (768.0, 768.0, 768.0));
        let far = range(6144.0, 12_288.0);
        assert_eq!(
            ends(3072.0, far, [0.0, 1.0], 1.0),
            (3072.0, 12_288.0, 2304.0)
        );
        assert_eq!(ends(3072.0, far, [0.0, 1.0], 0.0), (3072.0, 3072.0, 3072.0));
        // An extreme end: the haze stops at the cap.
        let extreme = range(1.0e5, 1.0e9);
        assert_eq!(
            ends(3072.0, extreme, [0.0, 1.0], 1.0),
            (3072.0, MAX_FOG_DISTANCE, 2304.0)
        );
    }

    /// Haze that has nowhere to go (its fade ends with the cells) changes nothing, not even
    /// on rays past the budget.
    #[test]
    fn haze_without_reach_changes_nothing() {
        let mut layer = hazy_bank((4, 8), 0.2);
        layer.max_distance = 300.0;
        layer.fade = FogFade::Range {
            start: 200.0,
            end: 300.0,
        };
        let with = hazy(&layer);
        layer.haze = 0.0;
        let without = hazy(&layer);
        assert_eq!(with.reach, without.reach);
        for i in 0..32 {
            let a = i as f32 * 0.3;
            let ray = Ray {
                eye: [0.3, 2.0 + (i % 20) as f32, 0.7],
                dir: [a.sin(), 0.05 * (i % 5) as f32 - 0.1, a.cos()],
                depths: [40.0, 150.0, f32::INFINITY, 280.0],
            };
            for budget in [256, 6] {
                assert_eq!(
                    march(&ray, &[with], [0.2, 0.3, 0.9], budget),
                    march(&ray, &[without], [0.2, 0.3, 0.9], budget),
                    "{i} {budget}"
                );
            }
        }
    }
}
