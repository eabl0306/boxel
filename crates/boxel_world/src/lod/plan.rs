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
//! reaches it. When `T_1` itself passes the preset's horizon (a huge vertical radius without
//! vertical bounds, an eye kilometers above a fixed-height world), the guard turns the LOD off.
//!
//! The set of wanted layers of a column is an interval (the distance to a box grows with its
//! vertical gap), so the walk is per column: [`plan`] keeps one wanted interval per column and
//! level, and for `k >= 2` the refined part of it, whose children columns want twice its layers.

use crate::interest::Viewer;
use boxel_math::{DVec3, I64Vec3, Vec3};
use boxel_voxel::{BlockPos, ChunkKey, ChunkPos, DimensionId, MAX_CHUNK_COORD};
use foldhash::HashMap;
use std::ops::RangeInclusive;

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
        )
    }

    /// The keep extent of `viewers` seen from the eye in chunk `eye`. Viewers' radii are already
    /// clamped ([`Viewer::new`]); the hysteresis is clamped the same way. A viewer whose keep
    /// layers miss the bounds keeps nothing.
    pub(crate) fn of_viewers(
        eye: ChunkPos,
        viewers: impl IntoIterator<Item = Viewer>,
        hysteresis: u32,
        bounds: Option<&RangeInclusive<i64>>,
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
    /// nothing and starts no jobs until it drops back.
    pub fn guard(&self) -> bool {
        self.t[1] > self.preset
    }
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

    /// The layers of a level-`k` column with squared horizontal gap `gap2` whose nodes lie
    /// nearer than `t`, clipped to the world limit.
    pub(crate) fn layers_within(&self, k: u8, gap2: f64, t: f64) -> Option<Span> {
        let rest = t * t - gap2;
        if rest <= 0.0 {
            return None;
        }
        let g = rest.sqrt();
        let e = node_edge(k);
        // The eye's layer and its height inside it, in [0, e).
        let layer = self.block[1].div_euclid(e);
        let r = self.block[1].rem_euclid(e) as f64 + self.frac[1];
        let ef = e as f64;
        // Layer `layer + j` spans [j e - r, (j + 1) e - r) from the eye: nearer than `g` when
        // `j e - r < g` and `(j + 1) e - r > -g`.
        let lo = ((r - g) / ef).floor() as i64;
        let hi = ((r + g) / ef).ceil() as i64 - 1;
        let limit = world_limit(k);
        let span = (
            layer.saturating_add(lo).max(-limit),
            layer.saturating_add(hi).min(limit),
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

/// The wanted tree: one map of columns per level (`levels[k - 1]`).
#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub(crate) levels: Vec<HashMap<[i64; 2], ColumnPlan>>,
}

impl Plan {
    /// The plan of column `c` (`None`: unwanted).
    pub(crate) fn get(&self, c: ColumnKey) -> Option<ColumnPlan> {
        self.levels
            .get(usize::from(c.level).wrapping_sub(1))?
            .get(&[c.x, c.z])
            .copied()
    }
}

/// Plans the wanted tree for `eye` under `th`; `old` gives each column's previous plan (for the
/// hysteresis; `None` for a fresh plan).
pub(crate) fn plan(
    eye: &Eye,
    th: &Thresholds,
    old: impl Fn(ColumnKey) -> Option<ColumnPlan>,
) -> Plan {
    let top = th.levels();
    let mut levels: Vec<HashMap<[i64; 2], ColumnPlan>> =
        (0..top).map(|_| HashMap::default()).collect();
    let refine = |key: ColumnKey, gap2: f64, wanted: Option<Span>, old: Option<ColumnPlan>| {
        if key.level < 2 {
            return None;
        }
        let below = key.level - 1;
        let t = th.level(below);
        let enter = eye.layers_within(key.level, gap2, t);
        let stay = eye.layers_within(key.level, gap2, t + node_edge(below) as f64);
        intersect(hysteresis(old.and_then(|o| o.refined), enter, stay), wanted)
    };
    // Top level: every column nearer than the horizon (plus its hysteresis).
    let e = node_edge(top);
    let h = th.horizon();
    let stay = h + e as f64;
    let reach = (stay / e as f64).ceil() as i64 + 1;
    let limit = world_limit(top);
    let (cx, cz) = (eye.block[0].div_euclid(e), eye.block[2].div_euclid(e));
    let top_map = &mut levels[usize::from(top) - 1];
    for z in (cz - reach).max(-limit)..=(cz + reach).min(limit) {
        for x in (cx - reach).max(-limit)..=(cx + reach).min(limit) {
            let key = ColumnKey { level: top, x, z };
            let gap2 = eye.column_gap2(top, [x, z]);
            if gap2 >= stay * stay {
                continue;
            }
            let prev = old(key);
            let wanted = hysteresis(
                prev.and_then(|o| o.wanted),
                eye.layers_within(top, gap2, h),
                eye.layers_within(top, gap2, stay),
            );
            if wanted.is_none() {
                continue;
            }
            let refined = refine(key, gap2, wanted, prev);
            top_map.insert([x, z], ColumnPlan { wanted, refined });
        }
    }
    // Finer levels: the children of the refined layers.
    for k in (1..top).rev() {
        let (finer, coarser) = levels.split_at_mut(usize::from(k));
        let parents = &coarser[0];
        let map = &mut finer[usize::from(k) - 1];
        let limit = world_limit(k);
        for (&[px, pz], parent) in parents {
            let Some((a, b)) = parent.refined else {
                continue;
            };
            let wanted = intersect(Some((a * 2, b * 2 + 1)), Some((-limit, limit)));
            for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let key = ColumnKey {
                    level: k,
                    x: px * 2 + dx,
                    z: pz * 2 + dz,
                };
                if key.x.abs() > limit || key.z.abs() > limit || wanted.is_none() {
                    continue;
                }
                let gap2 = eye.column_gap2(k, [key.x, key.z]);
                let refined = refine(key, gap2, wanted, old(key));
                map.insert([key.x, key.z], ColumnPlan { wanted, refined });
            }
        }
    }
    Plan { levels }
}
