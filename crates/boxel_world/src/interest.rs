//! Interest math: which chunks each viewer wants, when a slot may unload, job priorities, and the
//! work lists that apply a viewer change incrementally. No ECS, no slot state (unit tested on its
//! own).
//!
//! A viewer wants a vertical cylinder around its chunk (`dx, dy, dz` are chunk offsets):
//! - **mesh**: columns with `dx^2 + dz^2 <= radius^2` and layers with `|dy| <= vertical`;
//! - **data**: one more ring and one more layer (columns within `radius + 1` plus every xz
//!   neighbor of a mesh column, layers with `|dy| <= vertical + 1`), so each mesh chunk has its 26
//!   neighbors loaded;
//! - **keep**: the data region grown by the unload hysteresis (columns within
//!   `radius + 1 + hysteresis`, layers with `|dy| <= vertical + 1 + hysteresis`). An unwanted slot
//!   unloads once it is outside the keep region of every viewer.
//!
//! The level of a position is the best of the viewers ([`Level`]). When viewers change, a
//! [`Planner`] lists only the positions whose level may have changed: a small move is diffed row by
//! row (cost O(rows + changed), not O(view)); a new viewer lists its data region nearest shell
//! first; a viewer that leaves or jumps far lists its old keep region. The caller re-evaluates
//! each listed position against the current viewers, within a time budget.
use crate::Interest;
use boxel_voxel::ChunkPos;
use std::collections::VecDeque;
use std::ops::RangeInclusive;
use std::sync::Arc;

/// Radii (horizontal and vertical) and the hysteresis are clamped to this many chunks (about
/// 131 km) so the column math can never overflow; real view distances are far smaller.
pub(crate) const MAX_RADIUS: u32 = 4096;

/// Viewer centers are clamped to `+-MAX_CENTER` chunks per axis, so every offset position fits in
/// `i32` (block positions only reach `+-2^26` chunks anyway).
const MAX_CENTER: i32 = 1 << 28;

/// Rows a [`Task`] scans per [`Task::fill`] call when they produce nothing (bounds one call).
const ROWS_PER_FILL: usize = 256;

/// Positions a [`Task::Positions`] list hands out per [`Task::fill`] call.
const POSITIONS_PER_FILL: usize = 1024;

/// Column tables kept by a [`Planner`] (one per distinct radius and hysteresis in use).
const COLUMN_CACHE: usize = 8;

/// Largest [`Planner::capacity_hint`] (slots reserved up front).
const MAX_CAPACITY_HINT: usize = 1 << 18;

/// Viewer moves up to this many chunks per axis (same radii) are diffed row by row; longer jumps
/// re-list the new data region nearest first and then the old keep region.
const SMALL_MOVE: i64 = 4;

/// One viewer as the world sees it: its chunk, horizontal mesh radius and vertical mesh radius
/// (in chunks, clamped to [`MAX_RADIUS`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Viewer {
    /// The chunk the viewer is in.
    pub(crate) center: ChunkPos,
    /// Mesh columns: `dx^2 + dz^2 <= radius^2`.
    pub(crate) radius: u32,
    /// Mesh layers: `|dy| <= vertical`.
    pub(crate) vertical: u32,
}

impl Viewer {
    /// A viewer with clamped radii and center.
    pub(crate) fn new(center: ChunkPos, radius: u32, vertical: u32) -> Self {
        let clamp = |v: i32| v.clamp(-MAX_CENTER, MAX_CENTER);
        Self {
            center: ChunkPos::new(clamp(center.x), clamp(center.y), clamp(center.z)),
            radius: radius.min(MAX_RADIUS),
            vertical: vertical.min(MAX_RADIUS),
        }
    }
}

/// How much the viewers want a position, from least to most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Level {
    /// Outside every keep region: an unwanted slot here unloads.
    Out,
    /// Inside a keep region only (hysteresis band): an existing slot stays, nothing is created.
    Keep,
    /// Data wanted.
    Data,
    /// Data and mesh wanted.
    Mesh,
}

impl Level {
    /// The interest flags of this level.
    pub(crate) fn interest(self) -> Interest {
        match self {
            Level::Mesh => Interest::DATA | Interest::MESH,
            Level::Data => Interest::DATA,
            Level::Keep | Level::Out => Interest::empty(),
        }
    }
}

/// Per-row half widths of the mesh, data and keep column sets of one radius and hysteresis. All
/// three are symmetric intervals around the viewer's column in every row, nested
/// (mesh within data within keep).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Columns {
    radius: u32,
    hysteresis: u32,
    /// Rows `dz` in `-extent..=extent` (`radius + 1 + hysteresis`).
    extent: i64,
    /// `[mesh, data, keep]` half widths in x of row `dz` (index `dz + extent`), `-1` = none.
    widths: Vec<[i64; 3]>,
    /// Columns at keep level or better.
    keep_columns: usize,
}

impl Columns {
    /// Tables of a viewer with mesh radius `radius` and unload `hysteresis` (both clamped).
    pub(crate) fn new(radius: u32, hysteresis: u32) -> Self {
        let (radius, hysteresis) = (radius.min(MAX_RADIUS), hysteresis.min(MAX_RADIUS));
        let r = i64::from(radius);
        let h = i64::from(hysteresis);
        let extent = r + 1 + h;
        // Half width of the circle of radius `c` in row `dz` (`-1` outside it).
        let circle = |c: i64, dz: i64| {
            if dz.abs() <= c {
                (c * c - dz * dz).isqrt()
            } else {
                -1
            }
        };
        let widths: Vec<[i64; 3]> = (-extent..=extent)
            .map(|dz| {
                let mesh = circle(r, dz);
                // xz neighbors of the mesh columns of rows dz - 1, dz and dz + 1.
                let near = (-1..=1).map(|a| circle(r, dz + a)).max().unwrap_or(-1);
                let near = if near >= 0 { near + 1 } else { -1 };
                let data = circle(r + 1, dz).max(near);
                let keep = circle(r + 1 + h, dz).max(data);
                [mesh, data, keep]
            })
            .collect();
        let keep_columns = widths
            .iter()
            .map(|w| usize::try_from(2 * w[2] + 1).unwrap_or(0))
            .sum();
        Self {
            radius,
            hysteresis,
            extent,
            widths,
            keep_columns,
        }
    }

    /// `[mesh, data, keep]` half widths of row `dz` (all `-1` outside the table).
    fn row(&self, dz: i64) -> [i64; 3] {
        if dz.abs() > self.extent {
            return [-1; 3];
        }
        self.widths[usize::try_from(dz + self.extent).expect("row inside the table")]
    }

    /// Level of the column at offset `(dx, dz)`.
    pub(crate) fn level(&self, dx: i64, dz: i64) -> Level {
        let [mesh, data, keep] = self.row(dz);
        let dx = dx.abs();
        if dx <= mesh {
            Level::Mesh
        } else if dx <= data {
            Level::Data
        } else if dx <= keep {
            Level::Keep
        } else {
            Level::Out
        }
    }
}

/// A viewer with its column tables: answers "what level does this viewer give `pos`".
#[derive(Clone, Debug)]
pub(crate) struct Shape {
    viewer: Viewer,
    columns: Arc<Columns>,
}

impl Shape {
    fn new(viewer: Viewer, columns: Arc<Columns>) -> Self {
        debug_assert_eq!(viewer.radius, columns.radius);
        Self { viewer, columns }
    }

    fn vertical(&self) -> i64 {
        i64::from(self.viewer.vertical)
    }

    fn hysteresis(&self) -> i64 {
        i64::from(self.columns.hysteresis)
    }

    /// Level of the layer `y`.
    fn layer(&self, y: i64) -> Level {
        let dy = (y - i64::from(self.viewer.center.y)).abs();
        let (v, h) = (self.vertical(), self.hysteresis());
        if dy <= v {
            Level::Mesh
        } else if dy <= v + 1 {
            Level::Data
        } else if dy <= v + 1 + h {
            Level::Keep
        } else {
            Level::Out
        }
    }

    /// Level of `pos` for this viewer alone (vertical bounds not applied).
    pub(crate) fn level(&self, pos: ChunkPos) -> Level {
        let c = self.viewer.center;
        let layer = self.layer(i64::from(pos.y));
        if layer == Level::Out {
            return Level::Out;
        }
        let column = self.columns.level(
            i64::from(pos.x) - i64::from(c.x),
            i64::from(pos.z) - i64::from(c.z),
        );
        layer.min(column)
    }

    /// Absolute x intervals (inclusive) of positions at mesh, data and keep level or better in
    /// row `(y, z)`; `None` = empty.
    fn row(&self, y: i64, z: i64) -> [Option<(i64, i64)>; 3] {
        let layer = self.layer(y);
        let widths = self.columns.row(z - i64::from(self.viewer.center.z));
        let cx = i64::from(self.viewer.center.x);
        let mut out = [None; 3];
        for (i, (slot, level)) in out
            .iter_mut()
            .zip([Level::Mesh, Level::Data, Level::Keep])
            .enumerate()
        {
            let w = widths[i];
            if layer >= level && w >= 0 {
                *slot = Some((cx - w, cx + w));
            }
        }
        out
    }

    /// Layers that may hold a keep position, clipped to `bounds`.
    fn keep_layers(&self, bounds: Option<&RangeInclusive<i32>>) -> (i64, i64) {
        let reach = self.vertical() + 1 + self.hysteresis();
        clip(i64::from(self.viewer.center.y), reach, bounds)
    }

    /// Rows `z` that may hold a keep position.
    fn keep_rows(&self) -> (i64, i64) {
        let cz = i64::from(self.viewer.center.z);
        (cz - self.columns.extent, cz + self.columns.extent)
    }
}

/// `center +- reach`, clipped to `bounds`; empty (`lo > hi`) if they do not meet.
fn clip(center: i64, reach: i64, bounds: Option<&RangeInclusive<i32>>) -> (i64, i64) {
    let (mut lo, mut hi) = (center - reach, center + reach);
    if let Some(b) = bounds {
        lo = lo.max(i64::from(*b.start()));
        hi = hi.min(i64::from(*b.end()));
    }
    (lo, hi)
}

/// `ChunkPos` from in-range `i64` coordinates (viewer centers are clamped, so offsets fit).
fn pos(x: i64, y: i64, z: i64) -> ChunkPos {
    let narrow = |v: i64| i32::try_from(v).expect("interest positions fit in i32");
    ChunkPos::new(narrow(x), narrow(y), narrow(z))
}

/// A cursor over the rows `(y, z)` of an inclusive box, `y` outer.
#[derive(Clone, Debug)]
struct Rows {
    y: i64,
    z: i64,
    y_end: i64,
    z_start: i64,
    z_end: i64,
}

impl Rows {
    fn new((y0, y1): (i64, i64), (z0, z1): (i64, i64)) -> Self {
        Self {
            y: y0,
            z: z0,
            y_end: y1,
            z_start: z0,
            z_end: z1,
        }
    }

    /// The next row, or `None` when done.
    fn next(&mut self) -> Option<(i64, i64)> {
        if self.y > self.y_end || self.z_start > self.z_end {
            return None;
        }
        let row = (self.y, self.z);
        self.z += 1;
        if self.z > self.z_end {
            self.z = self.z_start;
            self.y += 1;
        }
        Some(row)
    }
}

/// One piece of work of a [`Planner`]: a lazily enumerated list of positions to re-evaluate.
#[derive(Clone, Debug)]
enum Task {
    /// Positions whose level differs between two viewers with the same radii (a small move).
    Move { from: Shape, to: Shape, rows: Rows },
    /// The data region of a viewer, by Chebyshev shells around its chunk (nearest first).
    Add {
        shape: Shape,
        shell: i64,
        last_shell: i64,
        layers: (i64, i64),
    },
    /// The keep region of a viewer that left or jumped (every position that may hold a slot it
    /// kept).
    Remove { shape: Shape, rows: Rows },
    /// Explicit positions (every loaded slot after a settings change).
    Positions(Vec<ChunkPos>),
}

impl Task {
    fn add(shape: Shape, bounds: Option<&RangeInclusive<i32>>) -> Self {
        let cy = i64::from(shape.viewer.center.y);
        let layers = clip(cy, shape.vertical() + 1, bounds);
        let last_shell = if layers.0 > layers.1 {
            // The data layers are all outside the vertical bounds.
            -1
        } else {
            let reach = (layers.0 - cy).abs().max((layers.1 - cy).abs());
            (i64::from(shape.viewer.radius) + 1).max(reach)
        };
        Task::Add {
            last_shell,
            shape,
            shell: 0,
            layers,
        }
    }

    fn remove(shape: Shape, bounds: Option<&RangeInclusive<i32>>) -> Self {
        let rows = Rows::new(shape.keep_layers(bounds), shape.keep_rows());
        Task::Remove { shape, rows }
    }

    fn moved(from: Shape, to: Shape, bounds: Option<&RangeInclusive<i32>>) -> Self {
        let (a, b) = (from.keep_layers(bounds), to.keep_layers(bounds));
        let (c, d) = (from.keep_rows(), to.keep_rows());
        let rows = Rows::new((a.0.min(b.0), a.1.max(b.1)), (c.0.min(d.0), c.1.max(d.1)));
        Task::Move { from, to, rows }
    }

    /// Appends some positions to `out`. Returns true when the task is finished.
    fn fill(&mut self, out: &mut VecDeque<ChunkPos>) -> bool {
        match self {
            Task::Move { from, to, rows } => {
                for _ in 0..ROWS_PER_FILL {
                    let Some((y, z)) = rows.next() else {
                        return true;
                    };
                    let before = out.len();
                    move_row(from, to, y, z, out);
                    if out.len() > before {
                        break;
                    }
                }
                false
            }
            Task::Add {
                shape,
                shell,
                last_shell,
                layers,
            } => {
                if *shell > *last_shell {
                    return true;
                }
                add_shell(shape, *shell, *layers, out);
                *shell += 1;
                *shell > *last_shell
            }
            Task::Remove { shape, rows } => {
                for _ in 0..ROWS_PER_FILL {
                    let Some((y, z)) = rows.next() else {
                        return true;
                    };
                    if let Some((x0, x1)) = shape.row(y, z)[2] {
                        out.extend((x0..=x1).map(|x| pos(x, y, z)));
                        break;
                    }
                }
                false
            }
            Task::Positions(list) => {
                let n = list.len().min(POSITIONS_PER_FILL);
                out.extend(list.drain(list.len() - n..));
                list.is_empty()
            }
        }
    }
}

/// Positions of row `(y, z)` whose level differs between `from` and `to`, except the ones that
/// only enter a keep region (nothing exists there to keep).
fn move_row(from: &Shape, to: &Shape, y: i64, z: i64, out: &mut VecDeque<ChunkPos>) {
    let (a, b) = (from.row(y, z), to.row(y, z));
    // Levels are nested intervals, so the level differs exactly on the union of the symmetric
    // differences of the three interval pairs.
    let mut parts = [(0, -1); 6];
    let mut n = 0;
    for i in 0..3 {
        for part in symmetric_difference(a[i], b[i]).into_iter().flatten() {
            parts[n] = part;
            n += 1;
        }
    }
    let parts = &mut parts[..n];
    parts.sort_unstable();
    let mut x_next = i64::MIN;
    for &mut (lo, hi) in parts {
        for x in lo.max(x_next)..=hi {
            let p = pos(x, y, z);
            let (la, lb) = (from.level(p), to.level(p));
            if la != lb && !(la == Level::Out && lb == Level::Keep) {
                out.push_back(p);
            }
        }
        x_next = x_next.max(hi + 1);
    }
}

/// `a` minus `b` and `b` minus `a` for inclusive intervals (each part may be `None`).
fn symmetric_difference(a: Option<(i64, i64)>, b: Option<(i64, i64)>) -> [Option<(i64, i64)>; 2] {
    match (a, b) {
        (None, None) => [None, None],
        (Some(i), None) | (None, Some(i)) => [Some(i), None],
        (Some((a0, a1)), Some((b0, b1))) => {
            if a1 < b0 || b1 < a0 {
                return [Some((a0, a1)), Some((b0, b1))];
            }
            let left = (a0 != b0).then(|| (a0.min(b0), a0.max(b0) - 1));
            let right = (a1 != b1).then(|| (a1.min(b1) + 1, a1.max(b1)));
            [left, right]
        }
    }
}

/// Positions at data level or better on Chebyshev shell `d` around the viewer, within `layers`.
fn add_shell(shape: &Shape, d: i64, (y0, y1): (i64, i64), out: &mut VecDeque<ChunkPos>) {
    let c = shape.viewer.center;
    let (cx, cy, cz) = (i64::from(c.x), i64::from(c.y), i64::from(c.z));
    // Data columns never reach past radius + 1 in x or z.
    let ext = (i64::from(shape.viewer.radius) + 1).min(d);
    let mut push = |dx: i64, dy: i64, dz: i64| {
        let p = pos(cx + dx, cy + dy, cz + dz);
        if shape.level(p) >= Level::Data {
            out.push_back(p);
        }
    };
    for y in y0.max(cy - d)..=y1.min(cy + d) {
        let dy = y - cy;
        if dy.abs() == d {
            // Top or bottom face: the whole square.
            for dz in -ext..=ext {
                for dx in -ext..=ext {
                    push(dx, dy, dz);
                }
            }
        } else if d <= ext {
            // A side ring: the square's perimeter (empty past the data columns).
            for dx in -d..=d {
                push(dx, dy, -d);
                if d > 0 {
                    push(dx, dy, d);
                }
            }
            for dz in (1 - d)..d {
                push(-d, dy, dz);
                push(d, dy, dz);
            }
        }
    }
}

/// Plans the interest work of viewer changes and answers level and priority queries for the
/// current viewers.
#[derive(Debug, Default)]
pub(crate) struct Planner {
    /// Viewers of the last plan, by id (sorted by id).
    viewers: Vec<(u64, Viewer)>,
    /// Their shapes, same order.
    shapes: Vec<Shape>,
    /// A plan was made (the first one starts from an empty world).
    planned: bool,
    hysteresis: u32,
    bounds: Option<RangeInclusive<i32>>,
    columns: Vec<Arc<Columns>>,
    tasks: VecDeque<Task>,
    pending: VecDeque<ChunkPos>,
}

impl Planner {
    /// True if `viewers`, `hysteresis` or `bounds` differ from the last plan (or there was none).
    pub(crate) fn changed(
        &self,
        viewers: &[(u64, Viewer)],
        hysteresis: u32,
        bounds: Option<&RangeInclusive<i32>>,
    ) -> bool {
        !self.planned
            || self.viewers != viewers
            || self.hysteresis != hysteresis.min(MAX_RADIUS)
            || self.bounds.as_ref() != bounds
    }

    /// Plans the work for new viewers (`viewers` sorted by id) or settings. `loaded` lists every
    /// loaded slot; it is only called when the settings changed (everything is re-evaluated).
    /// Returns `None` if nothing changed. Otherwise queued job priorities are stale, and the
    /// value is how many chunks closer to its nearest viewer any position may have come (the
    /// longest viewer move, `u32::MAX` if a viewer was added).
    pub(crate) fn update(
        &mut self,
        viewers: &[(u64, Viewer)],
        hysteresis: u32,
        bounds: Option<&RangeInclusive<i32>>,
        loaded: impl FnOnce() -> Vec<ChunkPos>,
    ) -> Option<u32> {
        if !self.changed(viewers, hysteresis, bounds) {
            return None;
        }
        debug_assert!(viewers.is_sorted_by_key(|(id, _)| *id));
        let drift = drift(&self.viewers, viewers);
        let hysteresis = hysteresis.min(MAX_RADIUS);
        let settings_changed =
            self.planned && (self.hysteresis != hysteresis || self.bounds.as_ref() != bounds);
        self.hysteresis = hysteresis;
        self.bounds = bounds.cloned();
        let shapes: Vec<Shape> = viewers
            .iter()
            .map(|&(_, v)| Shape::new(v, self.columns_for(v.radius, hysteresis)))
            .collect();
        let bounds = self.bounds.clone();
        let bounds = bounds.as_ref();
        if settings_changed {
            // Everything may change: re-evaluate every slot and every new data region.
            let mut list = loaded();
            list.sort_unstable();
            self.tasks.push_back(Task::Positions(list));
            for shape in &shapes {
                self.tasks.push_back(Task::add(shape.clone(), bounds));
            }
        } else {
            // Pair old and new viewers by id. Growth (moves, new viewers) is queued before
            // shrinking, so a jump creates the new area before it unloads the old one.
            let mut grow = Vec::new();
            let mut shrink = Vec::new();
            let (mut i, mut j) = (0, 0);
            while i < self.viewers.len() || j < viewers.len() {
                let old = self.viewers.get(i).map(|&(id, _)| id);
                let new = viewers.get(j).map(|&(id, _)| id);
                match (old, new) {
                    (Some(a), Some(b)) if a == b => {
                        let (from, to) = (&self.shapes[i], &shapes[j]);
                        if from.viewer != to.viewer {
                            if small_move(from, to) {
                                grow.push(Task::moved(from.clone(), to.clone(), bounds));
                            } else {
                                grow.push(Task::add(to.clone(), bounds));
                                shrink.push(Task::remove(from.clone(), bounds));
                            }
                        }
                        i += 1;
                        j += 1;
                    }
                    (Some(a), Some(b)) if a < b => {
                        shrink.push(Task::remove(self.shapes[i].clone(), bounds));
                        i += 1;
                    }
                    (Some(_), None) => {
                        shrink.push(Task::remove(self.shapes[i].clone(), bounds));
                        i += 1;
                    }
                    (_, Some(_)) => {
                        grow.push(Task::add(shapes[j].clone(), bounds));
                        j += 1;
                    }
                    (None, None) => unreachable!("loop condition"),
                }
            }
            self.tasks.extend(grow);
            self.tasks.extend(shrink);
        }
        self.viewers = viewers.to_vec();
        self.shapes = shapes;
        self.planned = true;
        Some(drift)
    }

    /// Column tables for `radius` (cached).
    fn columns_for(&mut self, radius: u32, hysteresis: u32) -> Arc<Columns> {
        let radius = radius.min(MAX_RADIUS);
        if let Some(c) = self
            .columns
            .iter()
            .find(|c| c.radius == radius && c.hysteresis == hysteresis)
        {
            return Arc::clone(c);
        }
        let c = Arc::new(Columns::new(radius, hysteresis));
        if self.columns.len() >= COLUMN_CACHE {
            self.columns.remove(0);
        }
        self.columns.push(Arc::clone(&c));
        c
    }

    /// The next position to re-evaluate, or `None` when the interest is up to date.
    pub(crate) fn next(&mut self) -> Option<ChunkPos> {
        loop {
            if let Some(p) = self.pending.pop_front() {
                return Some(p);
            }
            let task = self.tasks.front_mut()?;
            if task.fill(&mut self.pending) {
                self.tasks.pop_front();
            }
        }
    }

    /// About how many slots the current viewers may hold: their keep regions, clipped to the
    /// vertical bounds, at most [`MAX_CAPACITY_HINT`] (the map reserves this much up front).
    pub(crate) fn capacity_hint(&self) -> usize {
        let bounds = self.bounds.as_ref();
        let total = self.shapes.iter().fold(0usize, |total, s| {
            let (lo, hi) = s.keep_layers(bounds);
            let layers = usize::try_from(hi - lo + 1).unwrap_or(0);
            total.saturating_add(s.columns.keep_columns.saturating_mul(layers))
        });
        total.min(MAX_CAPACITY_HINT)
    }

    /// Positions and work lists not processed yet (0 = the interest is up to date).
    pub(crate) fn backlog(&self) -> usize {
        self.pending.len() + self.tasks.len()
    }

    /// True once a plan was made.
    pub(crate) fn is_planned(&self) -> bool {
        self.planned
    }

    /// The vertical bounds of the last plan.
    pub(crate) fn bounds(&self) -> Option<&RangeInclusive<i32>> {
        self.bounds.as_ref()
    }

    /// True if layer `y` is inside the vertical bounds (always without bounds).
    pub(crate) fn in_bounds(&self, y: i32) -> bool {
        self.bounds.as_ref().is_none_or(|b| b.contains(&y))
    }

    /// Level of `pos` for the current viewers: the best of them, [`Level::Out`] outside the
    /// vertical bounds.
    pub(crate) fn level(&self, pos: ChunkPos) -> Level {
        if !self.in_bounds(pos.y) {
            return Level::Out;
        }
        self.shapes
            .iter()
            .map(|s| s.level(pos))
            .max()
            .unwrap_or(Level::Out)
    }

    /// Job priority of `pos`: squared 3D chunk distance to the nearest current viewer, capped to
    /// `u32` (`u32::MAX` without viewers).
    pub(crate) fn priority(&self, pos: ChunkPos) -> u32 {
        self.viewers
            .iter()
            .map(|(_, v)| pos.distance_sq(v.center))
            .min()
            .map_or(u32::MAX, |d| u32::try_from(d).unwrap_or(u32::MAX))
    }
}

/// How many chunks closer to its nearest viewer any position may be with `new` viewers than with
/// `old` ones (both sorted by id): the longest move of a viewer present in both, rounded up, or
/// `u32::MAX` if `new` has a viewer `old` does not. Viewers that left only move positions away.
fn drift(old: &[(u64, Viewer)], new: &[(u64, Viewer)]) -> u32 {
    let mut drift = 0;
    let mut old = old.iter().peekable();
    for (id, viewer) in new {
        while old.next_if(|(old_id, _)| old_id < id).is_some() {}
        let Some((_, before)) = old.next_if(|(old_id, _)| old_id == id) else {
            return u32::MAX;
        };
        drift = drift.max(distance_ceil(before.center, viewer.center));
    }
    drift
}

/// Euclidean distance between two chunks, rounded up (`u32::MAX` if it does not fit).
fn distance_ceil(a: ChunkPos, b: ChunkPos) -> u32 {
    let d = |p: i32, q: i32| u128::from(p.abs_diff(q)).pow(2);
    let sq = d(a.x, b.x) + d(a.y, b.y) + d(a.z, b.z);
    let root = sq.isqrt();
    let root = if root * root < sq { root + 1 } else { root };
    u32::try_from(root).unwrap_or(u32::MAX)
}

/// True if `b` is `a` moved by at most [`SMALL_MOVE`] chunks per axis, radii unchanged.
fn small_move(a: &Shape, b: &Shape) -> bool {
    let (ca, cb) = (a.viewer.center, b.viewer.center);
    a.viewer.radius == b.viewer.radius
        && a.viewer.vertical == b.viewer.vertical
        && (i64::from(ca.x) - i64::from(cb.x)).abs() <= SMALL_MOVE
        && (i64::from(ca.y) - i64::from(cb.y)).abs() <= SMALL_MOVE
        && (i64::from(ca.z) - i64::from(cb.z)).abs() <= SMALL_MOVE
}

#[cfg(test)]
pub(crate) mod tests;
