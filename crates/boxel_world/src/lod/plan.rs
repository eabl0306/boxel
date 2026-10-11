//! Selection (ADR 0031): the distances that decide which LOD nodes are wanted, and the wanted
//! tree as one layer interval per column.
//!
//! A level-`k` node is `e_k = 32 * 2^k` blocks wide. The wanted tree is an octree refined by the
//! 3D distance from the eye to each node's box:
//! - top nodes: level-`K` nodes nearer than the horizon `H = T_K`;
//! - a wanted level-`k` node (`k >= 2`) is refined (its 8 children are wanted) while it is nearer
//!   than `T_(k-1)`.
//!
//! Both have a hysteresis of one node edge of the level below them (`e_K` for the top): a node
//! enters under the threshold and leaves at the threshold plus that edge, so an eye moving back
//! and forth inside the band changes nothing.
//!
//! Thresholds ([`Thresholds`]): `T_1` reaches past the 3D keep region of the viewers
//! ([`KeepExtent`]) by two lod-1 nodes, so every chunk the main renderer may still draw lies
//! inside a wanted lod-1 node, and `T_k = max(32 R 2^k, T_(k-1) + 2 e_k)`: every ring is at least
//! two of its nodes wide, and the horizon stays the preset's `32 R 2^K` unless the keep region
//! reaches it. When the keep region shrinks (a smaller radius), the chunks it left stay published
//! until the world unloads them, a budget at a time: `T_1` keeps its reach until it covers them
//! all again ([`reaches_published`]). When `T_1` itself passes the preset's horizon (a huge
//! vertical radius without vertical bounds, an eye kilometers above a fixed-height world), the
//! guard turns the LOD off.
//!
//! The set of wanted layers of a column is an interval (the distance to a box grows with its
//! vertical gap), so the walk is per column: [`plan_level`] keeps one wanted interval per column
//! and level, and for `k >= 2` the refined part of it, whose children columns want twice its
//! layers. A replan makes the levels one after another, coarse first, and the LOD applies each
//! a budgeted run of columns a frame ([`LevelPlan::two_windows`]).

use super::coverage::ChunkCoverage;
use crate::interest::Viewer;
use boxel_math::{DVec3, I64Vec3, Vec3};
use boxel_voxel::{BlockPos, ChunkKey, ChunkPos, DimensionId, MAX_CHUNK_COORD};
use std::ops::RangeInclusive;
use std::time::Instant;

/// Most LOD levels the settings allow (`k = 1..=6`). The GPU side keeps 4 bits, room for 15.
pub const MAX_LEVELS: u8 = 6;

/// The edge of a level-`k` node in blocks, `32 * 2^k`.
pub const fn node_edge(k: u8) -> i64 {
    32 << k
}

/// The world limit of a level-`k` node coordinate: `MAX_CHUNK_COORD >> k`.
pub const fn world_limit(k: u8) -> i64 {
    MAX_CHUNK_COORD >> k
}

/// How far the full-detail chunks the viewers keep reach from the eye, in chunks: what `T_1`
/// must cover.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeepExtent {
    /// Horizontal: the keep columns (`radius + 1 + hysteresis` around each viewer) plus the
    /// eye's chunk.
    pub columns: u64,
    /// Vertical: the largest `|dy|` between the eye's layer and a keep layer (`vertical + 1 +
    /// hysteresis` around each viewer, clipped to the vertical bounds), plus the eye's chunk.
    pub layers: u64,
}

impl KeepExtent {
    /// The keep extent of one viewer at the eye's chunk (layer `eye_layer`): mesh `radius`,
    /// `vertical` radius, unload `hysteresis`, `bounds` the world's vertical bounds.
    pub fn around(
        radius: u32,
        vertical: u32,
        hysteresis: u32,
        eye_layer: i64,
        bounds: Option<&RangeInclusive<i64>>,
    ) -> Self {
        let eye = ChunkPos::new(0, eye_layer, 0);
        Self::of_viewers(
            eye,
            [Viewer::new(eye, radius, vertical)],
            hysteresis,
            bounds,
            u64::MAX,
        )
    }

    /// The keep extent of `viewers` seen from the eye in chunk `eye`. Viewers' radii are already
    /// clamped ([`Viewer::new`]); the hysteresis is clamped the same way. A viewer whose keep
    /// layers miss the bounds keeps nothing, and so does one more than `away` columns from the
    /// eye: it is somewhere else (another viewer, or the world's record of a viewer that just
    /// jumped, a frame late), and the LOD refines around the eye only.
    pub(crate) fn of_viewers(
        eye: ChunkPos,
        viewers: impl IntoIterator<Item = Viewer>,
        hysteresis: u32,
        bounds: Option<&RangeInclusive<i64>>,
        away: u64,
    ) -> Self {
        let h = u64::from(hysteresis.min(crate::interest::MAX_RADIUS));
        let mut out = Self::default();
        for v in viewers {
            let reach = i64::from(v.vertical) + 1 + h as i64;
            let (mut lo, mut hi) = (v.center.y - reach, v.center.y + reach);
            if let Some(b) = bounds {
                lo = lo.max(*b.start());
                hi = hi.min(*b.end());
            }
            if lo > hi {
                continue;
            }
            let dy = eye.y.abs_diff(lo).max(eye.y.abs_diff(hi));
            let dx = eye.x.abs_diff(v.center.x) as f64;
            let dz = eye.z.abs_diff(v.center.z) as f64;
            let off = dx.hypot(dz).ceil() as u64;
            if off > away || eye.y.abs_diff(v.center.y) > away {
                continue;
            }
            let columns = off.saturating_add(u64::from(v.radius) + 1 + h + 1);
            out.columns = out.columns.max(columns);
            out.layers = out.layers.max(dy.saturating_add(1));
        }
        out
    }
}

/// The selection thresholds of one plan (module docs), in blocks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    levels: u8,
    /// `t[k]` = `T_k` for `k` in `1..=levels`.
    t: [f64; MAX_LEVELS as usize + 1],
    /// The preset's horizon, `32 R 2^K`.
    preset: f64,
}

impl Thresholds {
    /// The thresholds of ring radius `ring_radius` (`R`, in node edges) and `levels` (`K`,
    /// clamped to `1..=MAX_LEVELS`) around a keep region of `keep`.
    pub fn new(ring_radius: u32, levels: u8, keep: KeepExtent) -> Self {
        let levels = levels.clamp(1, MAX_LEVELS);
        let r = f64::from(ring_radius);
        let edge = |k: u8| node_edge(k) as f64;
        // Exact integers far past any sane extent; a huge one only trips the guard.
        let reach = (32.0 * keep.columns as f64)
            .hypot(32.0 * keep.layers as f64)
            .ceil();
        let mut t = [0.0; MAX_LEVELS as usize + 1];
        t[1] = (64.0 * r).max(reach + 2.0 * edge(1));
        for k in 2..=levels {
            t[usize::from(k)] = (r * edge(k)).max(t[usize::from(k) - 1] + 2.0 * edge(k));
        }
        Self {
            levels,
            t,
            preset: r * edge(levels),
        }
    }

    /// `K`.
    pub fn levels(&self) -> u8 {
        self.levels
    }

    /// `T_k` for `k` in `1..=K` (refinement of level `k + 1`; `T_K` is the horizon).
    pub fn level(&self, k: u8) -> f64 {
        self.t[usize::from(k.clamp(1, self.levels))]
    }

    /// The horizon `H = T_K`: the LOD draws nothing past it.
    pub fn horizon(&self) -> f64 {
        self.t[usize::from(self.levels)]
    }

    /// The preset's horizon `32 R 2^K`, what [`Thresholds::horizon`] is unless the keep region
    /// reaches it.
    pub fn preset_horizon(&self) -> f64 {
        self.preset
    }

    /// True if the keep region reaches the preset's horizon (`T_1 > 32 R 2^K`): the LOD draws
    /// nothing and starts no jobs until it drops back ([`Thresholds::guard_holds`]).
    pub fn guard(&self) -> bool {
        self.t[1] > self.preset
    }

    /// True if a LOD the guard turned off stays off: until `T_1` is two lod-1 node edges (128
    /// blocks) under the preset's horizon. An eye bobbing across the line (one chunk layer with
    /// vertical bounds) then does not clear and refill the LOD each time.
    pub fn guard_holds(&self) -> bool {
        self.t[1] > self.preset - 2.0 * node_edge(1) as f64
    }
}

/// True if every published chunk of `cov` lies in a lod-1 node that a plan under `th` from `eye`
/// wants: its level-2 parent is nearer than `T_1` (refined whatever the hysteresis; its own
/// ancestors are nearer still than their thresholds), or with one level the node itself is
/// nearer than the horizon. Chunks above `ceiling >> 1` lie in nodes no plan wants (all air at
/// the generator's level). A walk over the coverage: called only while `T_1` would shrink.
pub(crate) fn reaches_published(
    eye: &Eye,
    th: &Thresholds,
    ceiling: Option<i64>,
    cov: &ChunkCoverage,
) -> bool {
    let t1 = th.level(1);
    cov.positions().all(|p| {
        if cov.get(p).published == 0 || ceiling.is_some_and(|c| p.y > c >> 1) {
            return true;
        }
        let lod1 = ChunkKey {
            dim: DimensionId(0),
            lod: 1,
            pos: p,
        };
        let key = if th.levels() >= 2 {
            ChunkKey {
                lod: 2,
                pos: ChunkPos::new(p.x >> 1, p.y >> 1, p.z >> 1),
                ..lod1
            }
        } else {
            lod1
        };
        eye.distance(key) < t1
    })
}

/// The eye of the LOD in blocks: an integer block and the fraction inside it, so distances to
/// node boxes are exact at any `i64` position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Eye {
    pub(crate) block: [i64; 3],
    /// In `[0, 1)` per axis.
    pub(crate) frac: [f64; 3],
}

impl Eye {
    /// The eye at world position `p` (clamped to the world limit's blocks).
    pub(crate) fn from_world(p: DVec3) -> Self {
        let block = BlockPos::from_world(p);
        let limit = boxel_voxel::MAX_BLOCK_COORD;
        let b = [block.x, block.y, block.z].map(|v| v.clamp(-limit, limit));
        let f = |v: f64, b: i64| (v - b as f64).clamp(0.0, 1.0 - f64::EPSILON);
        Self {
            block: b,
            frac: [f(p.x, b[0]), f(p.y, b[1]), f(p.z, b[2])],
        }
    }

    /// The eye at `origin + rel` (a render origin and an `f32` offset from it).
    pub(crate) fn from_origin(origin: I64Vec3, rel: Vec3) -> Self {
        let floor = rel.floor();
        let fr = rel - floor;
        let f = |v: f32| f64::from(v).clamp(0.0, 1.0 - f64::EPSILON);
        let limit = boxel_voxel::MAX_BLOCK_COORD;
        Self {
            block: [
                origin.x.saturating_add(floor.x as i64).clamp(-limit, limit),
                origin.y.saturating_add(floor.y as i64).clamp(-limit, limit),
                origin.z.saturating_add(floor.z as i64).clamp(-limit, limit),
            ],
            frac: [f(fr.x), f(fr.y), f(fr.z)],
        }
    }

    /// The chunk the eye is in.
    pub(crate) fn chunk(&self) -> ChunkPos {
        ChunkPos::containing(BlockPos::new(self.block[0], self.block[1], self.block[2]))
    }

    /// The offset from the eye to block `b` along `axis`, in blocks (`b - eye`). Exact while
    /// the difference is below 2^53, which every node near the eye is.
    fn offset(&self, axis: usize, b: i64) -> f64 {
        b.wrapping_sub(self.block[axis]) as f64 - self.frac[axis]
    }

    /// The gap from the eye to `[lo, lo + size)` along `axis` (0 inside).
    fn gap(&self, axis: usize, lo: i64, size: i64) -> f64 {
        let a = self.offset(axis, lo);
        let b = a + size as f64;
        if a > 0.0 {
            a
        } else if b < 0.0 {
            -b
        } else {
            0.0
        }
    }

    /// The squared horizontal gap from the eye to column `column` of level `k`.
    pub(crate) fn column_gap2(&self, k: u8, column: [i64; 2]) -> f64 {
        let e = node_edge(k);
        let gx = self.gap(0, column[0] << (5 + k), e);
        let gz = self.gap(2, column[1] << (5 + k), e);
        gx * gx + gz * gz
    }

    /// The distance from the eye to the box of node `key` (0 inside).
    pub(crate) fn distance(&self, key: ChunkKey) -> f64 {
        let (k, p) = (key.lod, key.pos);
        let e = node_edge(k);
        let s = 5 + u32::from(k);
        let g = [
            self.gap(0, p.x << s, e),
            self.gap(1, p.y << s, e),
            self.gap(2, p.z << s, e),
        ];
        (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt()
    }

    /// The offset from the eye to the center of node `key` (for the frustum classes).
    pub(crate) fn center_offset(&self, key: ChunkKey) -> DVec3 {
        let (k, p) = (key.lod, key.pos);
        let half = node_edge(k) as f64 / 2.0;
        let s = 5 + u32::from(k);
        DVec3::new(
            self.offset(0, p.x << s) + half,
            self.offset(1, p.y << s) + half,
            self.offset(2, p.z << s) + half,
        )
    }

    /// How far the eye is from `other`, in blocks.
    pub(crate) fn distance_to(&self, other: &Eye) -> f64 {
        let d = |a: usize| {
            other.block[a].wrapping_sub(self.block[a]) as f64 + other.frac[a] - self.frac[a]
        };
        DVec3::new(d(0), d(1), d(2)).length()
    }
}

/// The eye seen from the layers of one level: its layer and its height inside it, computed once
/// per level of a plan.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EyeLayer {
    layer: i64,
    /// In `[0, e)`.
    r: f64,
    e: f64,
    limit: i64,
}

impl EyeLayer {
    pub(crate) fn new(eye: &Eye, k: u8) -> Self {
        let e = node_edge(k);
        Self {
            layer: eye.block[1].div_euclid(e),
            r: eye.block[1].rem_euclid(e) as f64 + eye.frac[1],
            e: e as f64,
            limit: world_limit(k),
        }
    }

    /// The layers of a column with squared horizontal gap `gap2` whose nodes lie nearer than
    /// `t`, clipped to the world limit.
    pub(crate) fn span(&self, gap2: f64, t: f64) -> Option<Span> {
        let rest = t * t - gap2;
        if rest <= 0.0 {
            return None;
        }
        let g = rest.sqrt();
        // Layer `layer + j` spans [j e - r, (j + 1) e - r) from the eye: nearer than `g` when
        // `j e - r < g` and `(j + 1) e - r > -g`.
        let lo = ((self.r - g) / self.e).floor() as i64;
        let hi = ((self.r + g) / self.e).ceil() as i64 - 1;
        let span = (
            self.layer.saturating_add(lo).max(-self.limit),
            self.layer.saturating_add(hi).min(self.limit),
        );
        (span.0 <= span.1).then_some(span)
    }
}

/// An inclusive interval of node layers, `lo <= hi`.
pub(crate) type Span = (i64, i64);

/// `a` and `b` together (`None` if both are empty).
pub(crate) fn intersect(a: Option<Span>, b: Option<Span>) -> Option<Span> {
    let (a, b) = (a?, b?);
    let s = (a.0.max(b.0), a.1.min(b.1));
    (s.0 <= s.1).then_some(s)
}

/// The smallest interval holding `a` and `b`.
pub(crate) fn hull(a: Option<Span>, b: Option<Span>) -> Option<Span> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
        (a, None) => a,
        (None, b) => b,
    }
}

/// True if `s` holds `y`.
pub(crate) fn contains(s: Option<Span>, y: i64) -> bool {
    s.is_some_and(|(lo, hi)| lo <= y && y <= hi)
}

/// Hysteresis of one column: what stays of `old` inside `stay`, grown to hold `enter` (which
/// lies inside `stay`). The hull keeps one interval; everything in it lies inside `stay`.
pub(crate) fn hysteresis(
    old: Option<Span>,
    enter: Option<Span>,
    stay: Option<Span>,
) -> Option<Span> {
    hull(intersect(old, stay), enter)
}

/// A column of the wanted tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ColumnPlan {
    /// The wanted layers.
    pub(crate) wanted: Option<Span>,
    /// `k >= 2`: the refined layers (inside `wanted`), whose children are wanted.
    pub(crate) refined: Option<Span>,
}

/// A column of the wanted tree: level and `[x, z]` in level-`k` nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ColumnKey {
    pub(crate) level: u8,
    pub(crate) x: i64,
    pub(crate) z: i64,
}

impl ColumnKey {
    /// The key of this column's node at `layer`.
    pub(crate) fn node(self, dim: DimensionId, layer: i64) -> ChunkKey {
        ChunkKey {
            dim,
            lod: self.level,
            pos: ChunkPos::new(self.x, layer, self.z),
        }
    }

    /// The column of node `key`.
    pub(crate) fn of(key: ChunkKey) -> Self {
        Self {
            level: key.lod,
            x: key.pos.x,
            z: key.pos.z,
        }
    }
}

/// The columns of one level of the wanted tree: a dense window of column plans (unwanted
/// columns have no wanted layers), so lookups are an index and diffs a walk over two windows.
#[derive(Clone, Debug, Default)]
pub(crate) struct LevelPlan {
    /// The window's lowest column `[x, z]`.
    origin: [i64; 2],
    /// Columns along x and z.
    size: [usize; 2],
    cells: Vec<ColumnPlan>,
}

impl LevelPlan {
    /// A window over columns `min..=max` (empty if `min` passes `max` on an axis).
    fn new(min: [i64; 2], max: [i64; 2]) -> Self {
        if min[0] > max[0] || min[1] > max[1] {
            return Self::default();
        }
        let size = [
            (max[0] - min[0] + 1) as usize,
            (max[1] - min[1] + 1) as usize,
        ];
        Self {
            origin: min,
            size,
            cells: vec![ColumnPlan::default(); size[0] * size[1]],
        }
    }

    fn index(&self, x: i64, z: i64) -> Option<usize> {
        let dx = usize::try_from(x.checked_sub(self.origin[0])?).ok()?;
        let dz = usize::try_from(z.checked_sub(self.origin[1])?).ok()?;
        (dx < self.size[0] && dz < self.size[1]).then_some(dz * self.size[0] + dx)
    }

    /// The plan of column `[x, z]` if it is wanted.
    pub(crate) fn get(&self, x: i64, z: i64) -> Option<&ColumnPlan> {
        self.index(x, z)
            .map(|i| &self.cells[i])
            .filter(|c| c.wanted.is_some())
    }

    /// Sets column `[x, z]`, which lies in the window.
    fn set(&mut self, x: i64, z: i64, plan: ColumnPlan) {
        let i = self
            .index(x, z)
            .expect("a column inside the level's window");
        self.cells[i] = plan;
    }

    /// The lowest and highest columns of the window (`None`: empty).
    fn bounds(&self) -> Option<([i64; 2], [i64; 2])> {
        (!self.cells.is_empty()).then(|| {
            let o = self.origin;
            (
                o,
                [
                    o[0] + self.size[0] as i64 - 1,
                    o[1] + self.size[1] as i64 - 1,
                ],
            )
        })
    }

    /// The columns of the windows of `after` and `before`, row by row, `after`'s first:
    /// [`TwoWindows::column`] `0..len` (a column in both comes twice).
    pub(crate) fn two_windows(after: &Self, before: &Self) -> TwoWindows {
        let window = |l: &Self| {
            l.bounds()
                .map_or(([0, 0], 0, 0), |(lo, _)| (lo, l.size[0], l.cells.len()))
        };
        let (a, b) = (window(after), window(before));
        TwoWindows {
            windows: [a, b],
            len: a.2 + b.2,
        }
    }

    /// Every wanted column with its plan.
    pub(crate) fn iter(&self) -> impl Iterator<Item = ([i64; 2], &ColumnPlan)> + '_ {
        let (o, w) = (self.origin, self.size[0].max(1));
        self.cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.wanted.is_some())
            .map(move |(i, c)| ([o[0] + (i % w) as i64, o[1] + (i / w) as i64], c))
    }
}

/// The columns of two level windows ([`LevelPlan::two_windows`]), by index, so walking them can
/// stop and go on later.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TwoWindows {
    /// Each window's lowest column, width and columns.
    windows: [([i64; 2], usize, usize); 2],
    /// Columns in both.
    pub(crate) len: usize,
}

impl TwoWindows {
    /// Column `i` (below `len`).
    pub(crate) fn column(&self, i: usize) -> [i64; 2] {
        let [first, second] = self.windows;
        let ((origin, width, _), i) = if i < first.2 {
            (first, i)
        } else {
            (second, i - first.2)
        };
        [
            origin[0] + (i % width) as i64,
            origin[1] + (i / width) as i64,
        ]
    }
}

/// The wanted tree: one window of columns per level (`levels[k - 1]`).
#[derive(Clone, Debug, Default)]
pub(crate) struct Plan {
    pub(crate) levels: Vec<LevelPlan>,
}

impl Plan {
    /// The plan of column `c` (`None`: unwanted).
    #[cfg(test)]
    pub(crate) fn get(&self, c: ColumnKey) -> Option<ColumnPlan> {
        self.levels
            .get(usize::from(c.level).wrapping_sub(1))?
            .get(c.x, c.z)
            .copied()
    }
}

/// How far the eye moves before level `k`'s plan is made again: an eighth of the hysteresis
/// band of its own thresholds (`e_(k-1)` for the refinement, which is narrower than the top
/// level's `e_K`), at least [`super::REPLAN_DISTANCE`]. Level 1 has no threshold of its own: it
/// follows level 2's refinement.
pub(crate) fn cadence(k: u8) -> f64 {
    let band = node_edge(k.max(2) - 1) as f64;
    (band / 8.0).max(super::REPLAN_DISTANCE)
}

/// Plans the wanted tree for `eye` under `th`, keeping levels above `redo` as they are in `old`
/// (moved out of it: the eye has not crossed an eighth of their bands since they were made) and
/// making levels `redo` and below again, with the hysteresis of their spans in `old`. `redo` is
/// clamped to `1..=K`; an `old` with another number of levels counts as empty. The same as
/// [`plan_level`] level after level, coarse first, at once (tests; the LOD goes a budgeted part
/// a frame).
///
/// `ceiling` is the generator's highest chunk layer (`vertical_chunks().end()`, `None` without
/// one): a level-`k` layer above `ceiling >> k` holds only air, so it is never wanted (no job, no
/// record). Over a fixed-height world a high eye then wants the columns that reach the ground,
/// not a disc as wide as `T_1` at its own height.
#[cfg(test)]
pub(crate) fn plan(
    eye: &Eye,
    th: &Thresholds,
    old: &mut Plan,
    redo: u8,
    ceiling: Option<i64>,
) -> Plan {
    let top = th.levels();
    if old.levels.len() != usize::from(top) {
        old.levels = vec![LevelPlan::default(); usize::from(top)];
    }
    let redo = redo.clamp(1, top);
    let mut levels: Vec<LevelPlan> = (1..=top)
        .map(|k| {
            if k > redo {
                std::mem::take(&mut old.levels[usize::from(k) - 1])
            } else {
                LevelPlan::default()
            }
        })
        .collect();
    for k in (1..=redo).rev() {
        let (finer, coarser) = levels.split_at_mut(usize::from(k));
        let parents = (k < top).then(|| &coarser[0]);
        finer[usize::from(k) - 1] = plan_level(
            eye,
            th,
            k,
            &old.levels[usize::from(k) - 1],
            parents,
            ceiling,
        );
    }
    Plan { levels }
}

/// Plans level `k` of the wanted tree for `eye` under `th` (see [`plan`]): the top level from
/// the horizon, a finer one from the refined layers of `parents` (level `k + 1`, `None` for the
/// top), with the hysteresis of `prev` (level `k` as planned before). At once: the LOD goes a
/// few rows a frame ([`LevelPlanner`]).
#[cfg(test)]
pub(crate) fn plan_level(
    eye: &Eye,
    th: &Thresholds,
    k: u8,
    prev: &LevelPlan,
    parents: Option<&LevelPlan>,
    ceiling: Option<i64>,
) -> LevelPlan {
    let mut planner = LevelPlanner::new(*eye, *th, k, parents, ceiling);
    while !planner.step(prev, parents, None) {}
    planner.finish()
}

/// [`plan_level`] a row at a time, so a frame can stop between rows and go on in the next one.
/// `prev` and `parents` must be the same at every step (the replan keeps them until the level
/// is applied).
#[derive(Debug)]
pub(crate) struct LevelPlanner {
    eye: Eye,
    th: Thresholds,
    k: u8,
    ceiling: Option<i64>,
    /// The level's new window.
    map: LevelPlan,
    /// The top level: the window's rows (z); a finer level: the rows of the parents' window.
    rows: std::ops::Range<i64>,
    top: bool,
}

impl LevelPlanner {
    /// Starts planning level `k` (see [`plan_level`]).
    pub(crate) fn new(
        eye: Eye,
        th: Thresholds,
        k: u8,
        parents: Option<&LevelPlan>,
        ceiling: Option<i64>,
    ) -> Self {
        let top = th.levels();
        let Some(parents) = parents.filter(|_| k < top) else {
            // Top level: every column nearer than the horizon (plus its hysteresis).
            let e = node_edge(top);
            let stay = th.horizon() + e as f64;
            let reach = (stay / e as f64).ceil() as i64 + 1;
            let limit = world_limit(top);
            let (cx, cz) = (eye.block[0].div_euclid(e), eye.block[2].div_euclid(e));
            let min = [(cx - reach).max(-limit), (cz - reach).max(-limit)];
            let max = [(cx + reach).min(limit), (cz + reach).min(limit)];
            return Self {
                eye,
                th,
                k: top,
                ceiling,
                map: LevelPlan::new(min, max),
                rows: min[1]..max[1] + 1,
                top: true,
            };
        };
        // A finer level: the children of the refined layers. The window: twice the refined
        // parents' bounds.
        let limit = world_limit(k);
        let mut bounds: Option<([i64; 2], [i64; 2])> = None;
        for ([px, pz], parent) in parents.iter() {
            if parent.refined.is_some() {
                let (lo, hi) = bounds.get_or_insert(([px, pz], [px, pz]));
                *lo = [lo[0].min(px), lo[1].min(pz)];
                *hi = [hi[0].max(px), hi[1].max(pz)];
            }
        }
        let clip = |v: i64| v.clamp(-limit, limit);
        let map = bounds.map_or_else(LevelPlan::default, |(lo, hi)| {
            LevelPlan::new(
                [clip(lo[0] * 2), clip(lo[1] * 2)],
                [clip(hi[0] * 2 + 1), clip(hi[1] * 2 + 1)],
            )
        });
        let rows = if bounds.is_some() {
            0..parents.size[1] as i64
        } else {
            0..0
        };
        Self {
            eye,
            th,
            k,
            ceiling,
            map,
            rows,
            top: false,
        }
    }

    /// The layers of level `k` that may hold more than air, inside the world limit.
    fn layers(&self, k: u8) -> Option<Span> {
        let limit = world_limit(k);
        Some((-limit, self.ceiling.map_or(limit, |c| (c >> k).min(limit))))
    }

    /// The refined layers of a level-`k` column (`k >= 2`) at `gap2` from the eye: nearer than
    /// `T_(k-1)`, kept until past it by `e_(k-1)`.
    fn refine(
        &self,
        at: &EyeLayer,
        k: u8,
        gap2: f64,
        wanted: Span,
        old: Option<&ColumnPlan>,
    ) -> Option<Span> {
        let below = k - 1;
        let t = self.th.level(below);
        let enter = at.span(gap2, t);
        let stay = at.span(gap2, t + node_edge(below) as f64);
        intersect(
            hysteresis(old.and_then(|o| o.refined), enter, stay),
            Some(wanted),
        )
    }

    /// Plans rows until they are all done (true) or `until` passed (checked after each row).
    pub(crate) fn step(
        &mut self,
        prev: &LevelPlan,
        parents: Option<&LevelPlan>,
        until: Option<Instant>,
    ) -> bool {
        while let Some(row) = self.rows.next() {
            if self.top {
                self.top_row(row, prev);
            } else if let Some(parents) = parents {
                self.finer_row(row, prev, parents);
            }
            if until.is_some_and(|t| Instant::now() >= t) {
                break;
            }
        }
        self.rows.is_empty()
    }

    /// The new plan (every row planned).
    pub(crate) fn finish(self) -> LevelPlan {
        debug_assert!(self.rows.is_empty(), "a level planned in part");
        self.map
    }

    /// Row `z` of the top level's window.
    fn top_row(&mut self, z: i64, prev: &LevelPlan) {
        let top = self.k;
        let at = EyeLayer::new(&self.eye, top);
        let h = self.th.horizon();
        let stay = h + node_edge(top) as f64;
        let Some((min, max)) = self.map.bounds() else {
            return;
        };
        for x in min[0]..=max[0] {
            let gap2 = self.eye.column_gap2(top, [x, z]);
            if gap2 >= stay * stay {
                continue;
            }
            let before = prev.get(x, z);
            let wanted = intersect(
                hysteresis(
                    before.and_then(|o| o.wanted),
                    at.span(gap2, h),
                    at.span(gap2, stay),
                ),
                self.layers(top),
            );
            let Some(span) = wanted else {
                continue;
            };
            let refined = if top >= 2 {
                self.refine(&at, top, gap2, span, before)
            } else {
                None
            };
            self.map.set(x, z, ColumnPlan { wanted, refined });
        }
    }

    /// The children of row `row` of the parents' window.
    fn finer_row(&mut self, row: i64, prev: &LevelPlan, parents: &LevelPlan) {
        let k = self.k;
        let limit = world_limit(k);
        let at = EyeLayer::new(&self.eye, k);
        let w = parents.size[0];
        let pz = parents.origin[1] + row;
        for dx in 0..w {
            let px = parents.origin[0] + dx as i64;
            let Some(parent) = parents.get(px, pz) else {
                continue;
            };
            let Some((a, b)) = parent.refined else {
                continue;
            };
            let Some(wanted) = intersect(Some((a * 2, b * 2 + 1)), self.layers(k)) else {
                continue;
            };
            for (cx, cz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, z) = (px * 2 + cx, pz * 2 + cz);
                if x.abs() > limit || z.abs() > limit {
                    continue;
                }
                let refined = if k >= 2 {
                    let gap2 = self.eye.column_gap2(k, [x, z]);
                    self.refine(&at, k, gap2, wanted, prev.get(x, z))
                } else {
                    None
                };
                self.map.set(
                    x,
                    z,
                    ColumnPlan {
                        wanted: Some(wanted),
                        refined,
                    },
                );
            }
        }
    }
}
