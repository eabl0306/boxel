//! Back-to-front order of the translucent quads inside a chunk (ADR 0018, section 3).
//!
//! The translucent pass draws chunks back to front (`path::translucent_draws`), and inside a
//! chunk the quads in the order they sit in the arena. This module rewrites that order: it sorts
//! a chunk's translucent bucket for the camera on a worker thread and the renderer uploads the
//! permuted quads over the bucket's own arena range (and, on Low, their light over the same
//! range of the light buffer). The GPU draws them as before, so every tier gets the same order.
//!
//! ## The order
//!
//! Minecraft sorts by the distance from the camera to each quad's center. That works for its
//! one-block quads but not for greedy quads: a 32x32 water surface has its center far away
//! while its near part covers a glass block under the water next to the camera, which the
//! distance key then draws after the water. Instead, the order is a topological sort of the
//! "may cover" relation that the quads' planes give:
//!
//! - Almost every quad lies in an axis-aligned plane. If a quad `b` lies entirely on one side of the
//!   plane of quad `a` (touching allowed), the camera's side of that plane decides: when `b` is
//!   on the camera's side, `b` may cover `a` and `a` cannot cover `b`, so `a` goes first;
//!   otherwise `b` goes first. Both planes of a pair are tried; if they disagree, neither quad
//!   can cover the other and the pair adds no constraint.
//! - Two quads in the same plane facing opposite ways (water against glass) cover the same
//!   pixels at the same depth: the one facing away from the camera goes first.
//! - Pairs that no plane separates (only intersecting quads) and the order among unconstrained
//!   quads fall back to the distance key: farthest center first, ties by mesher index.
//! - The sloped top of a flowing water cell (`SHAPE_FLUID_SLOPE`, ADR 0021) has no plane: it
//!   lies in the horizontal slab between its lowest and highest corner, which orders it against
//!   what lies wholly above or below the slab (the slab's top or bottom acts as its plane). The
//!   slab's top and bottom bound the camera cell like planes, so crossing the slope re-sorts.
//! - A cycle of constraints (possible, rare) is broken by emitting the farthest remaining quad.
//!
//! The constraints only depend on which side of each plane the camera is, so the order stays
//! right while the camera stays inside the same cell of the chunk's planes ([`Validity`]): that
//! is the re-sort trigger. A far chunk's cell is huge (every plane on one side), so far chunks
//! re-sort only when the camera crosses one of their planes, and past [`EXACT_DISTANCE`] only
//! once the camera also moved an eighth of its distance away from where it was sorted (a quad
//! whose plane was just crossed is seen edge-on, so from far away its wrong order shows on next
//! to no pixels).
//!
//! The pair test is quadratic, so chunks with more than [`TOPO_LIMIT`] translucent quads use the
//! distance key alone, as Minecraft does, re-sorted when the camera enters another block (with
//! the same slack for far chunks).
//!
//! ## Cost
//!
//! Chunks without translucent quads have no state here. Each frame the renderer checks the
//! camera against the cell of every chunk with translucent quads (a few comparisons; skipped
//! while the camera does not move). Sorts run on the `AsyncComputeTaskPool` through the world's
//! [`JobExecutor`]s, nearest chunks first, with a cap on quads in flight; results are applied
//! with a cap on quads uploaded per frame, through the frame's single staging buffer.
use crate::chunk_table::ChunkTable;
use crate::path::CHUNK_EDGE;
use crate::renderer::ViewParams;
use boxel_math::{Aabb, DVec3, I64Vec3, Vec3};
use boxel_mesh::{ChunkMesh, PackedQuad, TRANSLUCENT_BUCKET, layout};
use boxel_voxel::{CollisionBox, Face, ModelBox};
use boxel_world::jobs::{InlineExecutor, JobExecutor, TaskPoolExecutor};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::Arc;

/// Where (and whether) the translucent quads inside a chunk are sorted back to front.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TranslucentSort {
    /// Not sorted: inside a chunk the quads keep the mesher's order (chunks are still drawn back
    /// to front). Switching to it keeps the last uploaded order.
    Off,
    /// Sorted on the calling thread inside [`VoxelRenderer::render`](crate::VoxelRenderer::render),
    /// applied in the same frame: deterministic (tests, offscreen snapshots).
    Inline,
    /// Sorted on the `AsyncComputeTaskPool` (the world's job pool), applied in a later frame.
    #[default]
    Background,
}

/// Sub-voxel steps of the plane coordinates: every quad plane of a mesh lies on a sixteenth of a
/// voxel (slabs at 8/16, fluid surfaces at 14/16, fluid corners on sixteenths), so planes are
/// exact integers.
const SUB: i32 = 16;

/// Most translucent quads a chunk sorts topologically. The pair test is quadratic: a worst case
/// (a 3D checkerboard of glass) takes about 0.3 ms at 512 quads and 2.4 ms at 1024 on a Ryzen 7
/// 250. Larger buckets use the distance key alone.
pub(crate) const TOPO_LIMIT: usize = 512;

/// Most translucent quads in sort jobs at once.
const MAX_IN_FLIGHT_QUADS: u32 = 64 * 1024;

/// One job sorts chunks until it holds about this many quads.
const JOB_QUADS: u32 = 4096;

/// Most re-sorted quads uploaded per frame (8 bytes each, twice on Low): at least one chunk.
const MAX_UPLOAD_QUADS: u32 = 32 * 1024;

/// Chunks whose center is nearer than this (voxels) re-sort at every plane crossing.
const EXACT_DISTANCE: f64 = 64.0;

/// A quad plane: axis, coordinate in sixteenths and the side its face looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Plane {
    /// 0 = x, 1 = y, 2 = z.
    pub axis: usize,
    /// Chunk-local coordinate in sixteenths of a voxel.
    pub at: i32,
    /// The face looks towards `+axis`.
    pub positive: bool,
}

/// The chunk-local box a quad covers, in sixteenths, flat along its plane's axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QuadBox {
    /// Min corner.
    pub min: [i32; 3],
    /// Max corner.
    pub max: [i32; 3],
    /// The quad's plane (`None` for a cross plant quad, which runs along a diagonal: its box is
    /// its cell, and for the sloped top of a fluid cell: its box spans its corner heights).
    pub plane: Option<Plane>,
}

impl QuadBox {
    /// The geometry `common.wgsl` (`decode_quad`) builds for `quad`, without the sub-pixel edge
    /// growth. `boxes` is the model box table a `SHAPE_BOX` quad indexes (a box index outside
    /// it counts as the cube face).
    pub fn of(quad: PackedQuad, boxes: &[ModelBox]) -> Self {
        let face = quad.face();
        let (n, u, v) = face.axes();
        let p = quad.pos().map(|c| c as i32 * SUB);
        let positive = face.index().is_multiple_of(2);
        let mut min = p;
        let mut max = p;
        let surface = (layout::FLUID_SURFACE * SUB as f32) as i32;
        if quad.shape() == layout::SHAPE_BOX {
            // One box face (ADR 0020): the box spans its own sixteenths of the cell.
            let b = boxes
                .get(usize::from(quad.box_index()))
                .map_or(CollisionBox::FULL, |b| b.bounds);
            for a in [u, v] {
                min[a] = p[a] + i32::from(b.min[a]);
                max[a] = p[a] + i32::from(b.max[a]);
            }
            let at = p[n] + i32::from(if positive { b.max[n] } else { b.min[n] });
            min[n] = at;
            max[n] = at;
            return Self {
                min,
                max,
                plane: Some(Plane {
                    axis: n,
                    at,
                    positive,
                }),
            };
        }
        max[u] += quad.w() as i32 * SUB;
        max[v] += quad.h() as i32 * SUB;
        let mut at = p[n] + if positive { SUB } else { 0 };
        match quad.shape() {
            layout::SHAPE_FLUID_SURFACE if n == 1 && positive => at = p[1] + surface,
            layout::SHAPE_FLUID_SURFACE if n != 1 => max[1] = p[1] + surface,
            layout::SHAPE_FLUID_SLOPE => {
                // Corner heights in sixteenths, corner k at (x, z) = (k & 1, k >> 1).
                let h = quad.fluid_heights().map_or([SUB; 4], |h| h.map(i32::from));
                let edge = |a: usize, b: usize| h[a].max(h[b]);
                let (lo, hi) = (
                    h[0].min(h[1]).min(h[2]).min(h[3]),
                    edge(0, 1).max(edge(2, 3)),
                );
                match face {
                    Face::PosY if lo != hi => {
                        min[1] = p[1] + lo;
                        max[1] = p[1] + hi;
                        return Self {
                            min,
                            max,
                            plane: None,
                        };
                    }
                    Face::PosY => at = p[1] + lo,
                    Face::NegY => {}
                    Face::PosX => max[1] = p[1] + edge(1, 3),
                    Face::NegX => max[1] = p[1] + edge(0, 2),
                    Face::PosZ => max[1] = p[1] + edge(2, 3),
                    Face::NegZ => max[1] = p[1] + edge(0, 1),
                }
            }
            layout::SHAPE_CROSS => {
                return Self {
                    min: p,
                    max: p.map(|c| c + SUB),
                    plane: None,
                };
            }
            _ => {}
        }
        min[n] = at;
        max[n] = at;
        Self {
            min,
            max,
            plane: Some(Plane {
                axis: n,
                at,
                positive,
            }),
        }
    }

    /// Squared distance from `eye` (sixteenths) to the box center: the fallback key.
    fn center_distance2(&self, eye: [f64; 3]) -> f64 {
        (0..3)
            .map(|a| {
                let c = f64::from(self.min[a] + self.max[a]) * 0.5;
                (c - eye[a]) * (c - eye[a])
            })
            .sum()
    }
}

/// Whether the owner of `plane` goes before `other` (`Some(true)`), after it (`Some(false)`) or
/// the plane says nothing (`other` crosses it, or lies in it facing the same way).
fn plane_order(plane: Plane, other: &QuadBox, eye: [f64; 3]) -> Option<bool> {
    let a = plane.axis;
    // A camera exactly on the plane sees its quads edge-on: either side works.
    let eye_above = eye[a] > f64::from(plane.at);
    let (lo, hi) = (other.min[a], other.max[a]);
    if lo == plane.at && hi == plane.at {
        // Same plane: only opposite faces overlap (a valid mesh never has two equal faces), and
        // the one facing away from the camera goes first.
        let o = other.plane?;
        if o.axis != a || o.positive == plane.positive {
            return None;
        }
        let owner_faces_eye = plane.positive == eye_above;
        Some(!owner_faces_eye)
    } else if lo >= plane.at {
        // `other` on the + side: on the camera's side it may cover the owner.
        Some(eye_above)
    } else if hi <= plane.at {
        Some(!eye_above)
    } else {
        None
    }
}

/// Whether the owner of a box without a plane (a sloped fluid top), which lies within the
/// horizontal slab `min[1]..=max[1]`, goes before `other` (`Some(true)`), after it, or the slab
/// says nothing (`other` reaches into it). Like [`plane_order`] with the slab's top for what
/// lies above it and its bottom for what lies below.
fn slab_order(owner: &QuadBox, other: &QuadBox, eye: [f64; 3]) -> Option<bool> {
    let (lo, hi) = (owner.min[1], owner.max[1]);
    if other.min[1] >= hi {
        Some(eye[1] > f64::from(hi))
    } else if other.max[1] <= lo {
        Some(eye[1] <= f64::from(lo))
    } else {
        None
    }
}

/// What the plane of `a` (or its slab, without a plane) says about `a` before `b`.
fn owner_order(a: &QuadBox, b: &QuadBox, eye: [f64; 3]) -> Option<bool> {
    match a.plane {
        Some(p) => plane_order(p, b, eye),
        None => slab_order(a, b, eye),
    }
}

/// Whether `a` must go before `b` (`Some(true)`), after it (`Some(false)`), or either way.
pub(crate) fn pair_order(a: &QuadBox, b: &QuadBox, eye: [f64; 3]) -> Option<bool> {
    let by_a = owner_order(a, b, eye);
    let by_b = owner_order(b, a, eye).map(|b_first| !b_first);
    match (by_a, by_b) {
        // One plane says `a` cannot cover `b`, the other that `b` cannot cover `a`.
        (Some(x), Some(y)) if x != y => None,
        (Some(x), _) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Where a sorted order is kept (chunk-local sixteenths): inside its camera cell (per axis,
/// `lo < eye <= hi`), and for a far chunk also within `slack` of the eye it was sorted for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Validity {
    /// Exclusive lower bounds.
    pub lo: [f64; 3],
    /// Inclusive upper bounds.
    pub hi: [f64; 3],
    /// The eye of the sort.
    pub eye: [f64; 3],
    /// Squared distance from `eye` the order is kept at, in any cell (0 near the chunk).
    pub slack2: f64,
}

impl Validity {
    /// True while the order sorted for this cell is still right at `eye` (sixteenths).
    pub fn contains(&self, eye: [f64; 3]) -> bool {
        (0..3).all(|a| self.lo[a] < eye[a] && eye[a] <= self.hi[a])
            || (0..3)
                .map(|a| (eye[a] - self.eye[a]) * (eye[a] - self.eye[a]))
                .sum::<f64>()
                <= self.slack2
    }
}

/// Distance in voxels from `eye` (sixteenths) to the chunk's center.
fn center_distance(eye: [f64; 3]) -> f64 {
    let half = f64::from(SUB) * f64::from(CHUNK_EDGE) * 0.5;
    (0..3)
        .map(|a| (eye[a] - half) * (eye[a] - half))
        .sum::<f64>()
        .sqrt()
        / f64::from(SUB)
}

/// How far (sixteenths) the camera may move before a chunk at `distance` voxels re-sorts on a
/// plane crossing: 0 up to [`EXACT_DISTANCE`], then an eighth of the distance past it. A quad
/// whose plane the camera just crossed is seen edge-on, and from far away it stays nearly
/// edge-on for a long way, so its wrong order covers next to no pixels; without this, a camera
/// flying past a lake re-sorts every chunk of its column at every block.
fn far_slack(distance: f64) -> f64 {
    (distance - EXACT_DISTANCE).max(0.0) / 8.0 * f64::from(SUB)
}

/// The cell of `eye` among the planes of `boxes`: the topological order only changes when the
/// camera crosses one of them.
fn plane_cell(boxes: &[QuadBox], eye: [f64; 3]) -> Validity {
    let mut lo = [f64::NEG_INFINITY; 3];
    let mut hi = [f64::INFINITY; 3];
    let mut bound = |axis: usize, at: f64| {
        if at < eye[axis] {
            lo[axis] = lo[axis].max(at);
        } else {
            hi[axis] = hi[axis].min(at);
        }
    };
    for b in boxes {
        match b.plane {
            Some(p) => bound(p.axis, f64::from(p.at)),
            // A sloped fluid top: the bottom and top of its box.
            None => {
                bound(1, f64::from(b.min[1]));
                bound(1, f64::from(b.max[1]));
            }
        }
    }
    let slack = far_slack(center_distance(eye));
    Validity {
        lo,
        hi,
        eye,
        slack2: slack * slack,
    }
}

/// The block cell of `eye` for the distance key, with the far slack of [`far_slack`] (the
/// order of far quads changes slowly).
fn block_cell(eye: [f64; 3]) -> Validity {
    let step = f64::from(SUB);
    let mut lo = [0.0; 3];
    let mut hi = [0.0; 3];
    for a in 0..3 {
        // `lo < eye <= hi`
        hi[a] = (eye[a] / step).ceil() * step;
        lo[a] = hi[a] - step;
    }
    let slack = far_slack(center_distance(eye));
    Validity {
        lo,
        hi,
        eye,
        slack2: slack * slack,
    }
}

/// The back-to-front order of `quads` (indices) for a camera at `eye` (chunk-local voxels), and
/// the cell where it stays right. See the module docs.
pub(crate) fn sort_order(
    quads: &[PackedQuad],
    model_boxes: &[ModelBox],
    eye: DVec3,
) -> (Vec<u32>, Validity) {
    let eye = (eye * f64::from(SUB)).to_array();
    let boxes: Vec<QuadBox> = quads.iter().map(|&q| QuadBox::of(q, model_boxes)).collect();
    // Squared distances are not negative: their bits order like their values.
    let keys: Vec<u64> = boxes
        .iter()
        .map(|b| b.center_distance2(eye).to_bits())
        .collect();
    if boxes.len() > TOPO_LIMIT {
        let mut order: Vec<u32> = (0..boxes.len() as u32).collect();
        order.sort_unstable_by_key(|&i| (Reverse(keys[i as usize]), i));
        return (order, block_cell(eye));
    }
    (topo_order(&boxes, &keys, eye), plane_cell(&boxes, eye))
}

/// Kahn's algorithm over [`pair_order`], farthest ready quad first (ties: lowest index); a cycle
/// emits the farthest quad left.
fn topo_order(boxes: &[QuadBox], keys: &[u64], eye: [f64; 3]) -> Vec<u32> {
    let n = boxes.len();
    let mut edges: Vec<(u32, u32)> = Vec::new();
    for i in 0..n {
        for j in i + 1..n {
            match pair_order(&boxes[i], &boxes[j], eye) {
                Some(true) => edges.push((i as u32, j as u32)),
                Some(false) => edges.push((j as u32, i as u32)),
                None => {}
            }
        }
    }
    // successors of each quad, contiguous
    let mut start = vec![0u32; n + 1];
    let mut indegree = vec![0u32; n];
    for &(from, to) in &edges {
        start[from as usize + 1] += 1;
        indegree[to as usize] += 1;
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut next = vec![0u32; edges.len()];
    for &(from, to) in &edges {
        next[fill[from as usize] as usize] = to;
        fill[from as usize] += 1;
    }
    let mut ready: BinaryHeap<(u64, Reverse<u32>)> = (0..n)
        .filter(|&i| indegree[i] == 0)
        .map(|i| (keys[i], Reverse(i as u32)))
        .collect();
    let mut done = vec![false; n];
    let mut order = Vec::with_capacity(n);
    while order.len() < n {
        let quad = match ready.pop() {
            Some((_, Reverse(i))) => i as usize,
            // A cycle: the farthest quad left goes next (ties: lowest index).
            None => (0..n)
                .filter(|&i| !done[i])
                .max_by_key(|&i| (keys[i], Reverse(i)))
                .unwrap_or(0),
        };
        if done[quad] {
            continue;
        }
        done[quad] = true;
        order.push(quad as u32);
        for &to in &next[start[quad] as usize..start[quad + 1] as usize] {
            let to = to as usize;
            indegree[to] -= 1;
            if indegree[to] == 0 && !done[to] {
                ready.push((keys[to], Reverse(to as u32)));
            }
        }
    }
    order
}

/// A chunk's translucent quads in back-to-front order for `eye` (chunk-local voxels), and the
/// cell where the order stays right; `boxes` is the model box table.
pub(crate) fn sort_bucket(
    mesh: &ChunkMesh,
    boxes: &[ModelBox],
    eye: DVec3,
) -> (Vec<PackedQuad>, Validity) {
    let quads = mesh.bucket(TRANSLUCENT_BUCKET);
    let (order, valid) = sort_order(quads, boxes, eye);
    (order.iter().map(|&i| quads[i as usize]).collect(), valid)
}

/// The camera relative to a chunk at `origin`, in chunk-local sixteenths. Exact near the chunk;
/// far away the difference is clamped (every plane stays on the same side).
fn local_eye(view: &ViewParams, origin: I64Vec3) -> DVec3 {
    const FAR: i64 = 1 << 24;
    let d = view
        .render_origin
        .saturating_sub(origin)
        .clamp(I64Vec3::splat(-FAR), I64Vec3::splat(FAR));
    d.as_dvec3() + view.eye_rel.as_dvec3()
}

/// A re-sorted bucket ready for upload: the renderer writes `quads` over the chunk's current
/// translucent range.
#[derive(Debug)]
pub(crate) struct SortedBucket {
    /// Chunk id.
    pub id: u64,
    /// The mesh it was sorted from (its palette lights the quads on Low).
    pub mesh: Arc<ChunkMesh>,
    /// The translucent quads, back to front.
    pub quads: Vec<PackedQuad>,
}

/// What the sorter knows of a chunk with translucent quads.
#[derive(Debug)]
struct ChunkSort {
    mesh: Arc<ChunkMesh>,
    origin: I64Vec3,
    /// Where the order on the GPU stays right (`None`: the mesher's order).
    valid: Option<Validity>,
    /// The job sorting it, until its result is applied or dropped.
    ticket: Option<u64>,
    /// A sort of this mesh panicked: no retries until the next mesh.
    failed: bool,
}

/// A job's answer for one chunk.
struct SortResult {
    id: u64,
    ticket: u64,
    mesh: Arc<ChunkMesh>,
    /// `None` when the sort panicked.
    sorted: Option<(Vec<PackedQuad>, Validity)>,
}

/// One chunk for a job.
struct SortTask {
    id: u64,
    ticket: u64,
    mesh: Arc<ChunkMesh>,
    boxes: Arc<[ModelBox]>,
    eye: DVec3,
}

/// Counters of the sorter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SortStats {
    /// Re-sorted buckets handed to the upload (cumulative).
    pub sorts: u32,
    /// Their quads (cumulative).
    pub quads: u64,
}

/// Keeps the translucent bucket of every chunk sorted for the camera (see the module docs).
pub(crate) struct TranslucentSorter {
    mode: TranslucentSort,
    chunks: HashMap<u64, ChunkSort, foldhash::fast::FixedState>,
    tx: crossbeam_channel::Sender<SortResult>,
    rx: crossbeam_channel::Receiver<SortResult>,
    /// Results waiting for the upload budget, in arrival order.
    ready: VecDeque<SortResult>,
    next_ticket: u64,
    /// Quads in jobs not answered yet.
    in_flight_quads: u32,
    /// Camera of the last scan.
    last_eye: Option<(I64Vec3, Vec3)>,
    /// Something besides the camera changed since the last scan (a mesh, a result, the budget).
    dirty: bool,
    stats: SortStats,
    /// The model box table the quads index (`SHAPE_BOX` quads).
    boxes: Arc<[ModelBox]>,
}

impl TranslucentSorter {
    /// An empty sorter.
    pub fn new(mode: TranslucentSort) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            mode,
            chunks: HashMap::default(),
            tx,
            rx,
            ready: VecDeque::new(),
            next_ticket: 0,
            in_flight_quads: 0,
            last_eye: None,
            dirty: false,
            stats: SortStats::default(),
            boxes: Arc::from([]),
        }
    }

    /// The model box table changed: every chunk sorts again with it.
    pub fn set_boxes(&mut self, boxes: Arc<[ModelBox]>) {
        self.boxes = boxes;
        for state in self.chunks.values_mut() {
            state.valid = None;
            state.ticket = None;
        }
        self.dirty = true;
    }

    /// The mode.
    pub fn mode(&self) -> TranslucentSort {
        self.mode
    }

    /// Changes the mode. Jobs in flight still answer; their results are applied only if the new
    /// mode sorts.
    pub fn set_mode(&mut self, mode: TranslucentSort) {
        if mode != self.mode {
            self.mode = mode;
            self.dirty = true;
        }
    }

    /// Counters.
    pub fn stats(&self) -> SortStats {
        self.stats
    }

    /// Chunks with translucent quads tracked.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// A chunk got `mesh`, uploaded in the mesher's order: it needs a sort if it has translucent
    /// quads. Any job for its previous mesh is ignored.
    pub fn upsert(&mut self, id: u64, origin: I64Vec3, mesh: Arc<ChunkMesh>) {
        if mesh.bucket_counts[TRANSLUCENT_BUCKET] == 0 {
            self.chunks.remove(&id);
            return;
        }
        self.chunks.insert(
            id,
            ChunkSort {
                mesh,
                origin,
                valid: None,
                ticket: None,
                failed: false,
            },
        );
        self.dirty = true;
    }

    /// A chunk was removed.
    pub fn remove(&mut self, id: u64) {
        self.chunks.remove(&id);
    }

    /// Every chunk was removed (or will be uploaded again in the mesher's order).
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.ready.clear();
    }

    /// Once per frame, before the uploads: starts sorts for the chunks whose camera cell
    /// changed (nearest and visible first, within the in-flight cap) and appends to `out` the
    /// sorted buckets to upload this frame (within the upload cap). With
    /// [`TranslucentSort::Inline`] the sorts started here are in `out` already.
    pub fn update(&mut self, chunks: &ChunkTable, view: &ViewParams, out: &mut Vec<SortedBucket>) {
        if self.mode == TranslucentSort::Off {
            // Drop whatever comes back: the chunks keep their uploaded order.
            self.receive();
            for result in self.ready.drain(..) {
                if let Some(state) = self.chunks.get_mut(&result.id)
                    && state.ticket == Some(result.ticket)
                {
                    state.ticket = None;
                }
            }
            return;
        }
        let eye = (view.render_origin, view.eye_rel);
        if self.dirty || self.last_eye != Some(eye) {
            self.last_eye = Some(eye);
            self.dirty = false;
            self.start_sorts(chunks, view);
        }
        self.receive();
        self.apply(out);
    }

    /// Starts jobs for the chunks whose order is not right for the camera any more.
    fn start_sorts(&mut self, chunks: &ChunkTable, view: &ViewParams) {
        let mut wanted: Vec<((bool, u64), u64)> = Vec::new();
        for (&id, state) in &self.chunks {
            if state.ticket.is_some() || state.failed || !chunks.in_window(state.origin) {
                continue;
            }
            let eye = local_eye(view, state.origin);
            if state
                .valid
                .is_some_and(|v| v.contains((eye * f64::from(SUB)).to_array()))
            {
                continue;
            }
            let min = state.origin.saturating_sub(view.render_origin).as_vec3();
            let max = min + Vec3::splat(CHUNK_EDGE);
            let hidden = !view.frustum.intersects_aabb(&Aabb::new(min, max));
            let center = eye - DVec3::splat(f64::from(CHUNK_EDGE) * 0.5);
            wanted.push(((hidden, center.length_squared().to_bits()), id));
        }
        if wanted.is_empty() {
            return;
        }
        wanted.sort_unstable();
        let mut job: Vec<SortTask> = Vec::new();
        let mut job_quads = 0;
        for (_, id) in wanted {
            if self.in_flight_quads >= MAX_IN_FLIGHT_QUADS {
                // the rest waits for answers
                self.dirty = true;
                break;
            }
            let Some(state) = self.chunks.get_mut(&id) else {
                continue;
            };
            let n = state.mesh.bucket_counts[TRANSLUCENT_BUCKET];
            let ticket = self.next_ticket;
            self.next_ticket += 1;
            state.ticket = Some(ticket);
            self.in_flight_quads += n;
            job_quads += n;
            job.push(SortTask {
                id,
                ticket,
                mesh: Arc::clone(&state.mesh),
                boxes: Arc::clone(&self.boxes),
                eye: local_eye(view, state.origin),
            });
            if job_quads >= JOB_QUADS {
                self.spawn(std::mem::take(&mut job));
                job_quads = 0;
            }
        }
        if !job.is_empty() {
            self.spawn(job);
        }
    }

    /// Runs one job (inline or on the pool). Each chunk answers exactly once, also when its sort
    /// panics.
    fn spawn(&self, tasks: Vec<SortTask>) {
        let tx = self.tx.clone();
        let job = Box::new(move || {
            for task in tasks {
                let sorted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    sort_bucket(&task.mesh, &task.boxes, task.eye)
                }))
                .ok();
                // The receiver lives as long as the renderer; a dropped one needs no answer.
                let _ = tx.send(SortResult {
                    id: task.id,
                    ticket: task.ticket,
                    mesh: task.mesh,
                    sorted,
                });
            }
        });
        match self.mode {
            TranslucentSort::Inline => InlineExecutor.spawn(job),
            _ => TaskPoolExecutor.spawn(job),
        }
    }

    /// Moves the answered jobs to `ready`.
    fn receive(&mut self) {
        for result in self.rx.try_iter() {
            let n = result.mesh.bucket_counts[TRANSLUCENT_BUCKET];
            self.in_flight_quads = self.in_flight_quads.saturating_sub(n);
            self.ready.push_back(result);
        }
    }

    /// Hands the ready results of current jobs to `out`, within the upload cap.
    fn apply(&mut self, out: &mut Vec<SortedBucket>) {
        let mut quads = 0;
        while let Some(result) = self.ready.pop_front() {
            let Some(state) = self.chunks.get_mut(&result.id) else {
                continue;
            };
            if state.ticket != Some(result.ticket) {
                // a newer mesh (or a resync) came after the job started
                continue;
            }
            let Some((sorted, valid)) = result.sorted else {
                state.ticket = None;
                state.failed = true;
                tracing::error!("translucent sort of chunk {} panicked", result.id);
                continue;
            };
            let n = sorted.len() as u32;
            if quads > 0 && quads + n > MAX_UPLOAD_QUADS {
                // next frame
                self.ready.push_front(SortResult {
                    sorted: Some((sorted, valid)),
                    ..result
                });
                break;
            }
            quads += n;
            state.ticket = None;
            state.valid = Some(valid);
            // the camera may have left the cell while the job ran
            self.dirty = true;
            self.stats.sorts = self.stats.sorts.saturating_add(1);
            self.stats.quads += u64::from(n);
            out.push(SortedBucket {
                id: result.id,
                mesh: result.mesh,
                quads: sorted,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::ArenaAllocator;
    use crate::chunk_table::ChunkRecord;
    use boxel_camera::{Camera, CameraView};
    use boxel_mesh::{BUCKET_COUNT, MaterialTable, MeshInput, MeshOptions, Mesher};
    use boxel_transform::Transform;
    use boxel_voxel::{BlockState, ChunkData, Face, LocalPos, ModelBox};

    fn quad(pos: [u32; 3], w: u32, h: u32, face: Face) -> PackedQuad {
        PackedQuad::new(pos, w, h, face, 0)
    }

    impl QuadBox {
        /// [`QuadBox::of`] without model boxes (greedy, fluid and cross quads).
        fn of_test(quad: PackedQuad) -> Self {
            Self::of(quad, &[])
        }
    }

    #[test]
    fn quad_boxes_follow_the_shader_geometry() {
        let b = QuadBox::of_test(quad([2, 3, 4], 5, 6, Face::PosY));
        assert_eq!((b.min, b.max), ([32, 64, 64], [112, 64, 160]));
        assert_eq!(
            b.plane,
            Some(Plane {
                axis: 1,
                at: 64,
                positive: true
            })
        );
        // -X: plane at x, u = z, v = y
        let b = QuadBox::of_test(quad([2, 3, 4], 5, 6, Face::NegX));
        assert_eq!((b.min, b.max), ([32, 48, 64], [32, 144, 144]));
        // fluid surface: the top drops to 14/16, the sides end there
        let top = QuadBox::of_test(
            quad([0, 3, 0], 4, 4, Face::PosY).with_shape(layout::SHAPE_FLUID_SURFACE),
        );
        assert_eq!(top.plane.map(|p| p.at), Some(3 * 16 + 14));
        let side = QuadBox::of_test(
            quad([0, 3, 0], 4, 1, Face::PosZ).with_shape(layout::SHAPE_FLUID_SURFACE),
        );
        assert_eq!((side.min[1], side.max[1], side.min[2]), (48, 62, 16));
        let bottom = QuadBox::of_test(
            quad([0, 3, 0], 4, 4, Face::NegY).with_shape(layout::SHAPE_FLUID_SURFACE),
        );
        assert_eq!(bottom.plane.map(|p| p.at), Some(48));
        // box faces (ADR 0020): the slab halves and a torch stick, from the box table
        let table = [
            ModelBox::new([0, 0, 0], [16, 8, 16]),
            ModelBox::new([0, 8, 0], [16, 16, 16]),
            ModelBox::new([7, 0, 7], [9, 10, 9]),
        ];
        let boxed = |f, index| {
            QuadBox::of(
                quad([0, 3, 0], 1, 1, f)
                    .with_shape(layout::SHAPE_BOX)
                    .with_box_index(index),
                &table,
            )
        };
        let (lower, upper) = (|f| boxed(f, 0), |f| boxed(f, 1));
        assert_eq!(lower(Face::PosY).plane.map(|p| p.at), Some(56));
        assert_eq!(lower(Face::NegY).plane.map(|p| p.at), Some(48));
        assert_eq!(upper(Face::PosY).plane.map(|p| p.at), Some(64));
        assert_eq!(upper(Face::NegY).plane.map(|p| p.at), Some(56));
        assert_eq!(
            (lower(Face::PosX).min[1], lower(Face::PosX).max[1]),
            (48, 56)
        );
        assert_eq!(
            (upper(Face::PosX).min[1], upper(Face::PosX).max[1]),
            (56, 64)
        );
        let stick = boxed(Face::NegX, 2);
        assert_eq!((stick.min, stick.max), ([7, 48, 7], [7, 58, 9]));
        assert_eq!(stick.plane.map(|p| (p.axis, p.positive)), Some((0, false)));
        // an index outside the table counts as the cube face
        assert_eq!(boxed(Face::PosY, 9).plane.map(|p| p.at), Some(64));
        // a cross covers its cell and has no plane
        let cross =
            QuadBox::of_test(quad([1, 1, 1], 1, 1, Face::PosX).with_shape(layout::SHAPE_CROSS));
        assert_eq!(
            (cross.min, cross.max, cross.plane),
            ([16; 3], [32; 3], None)
        );
    }

    #[test]
    fn sloped_fluid_quads_are_boxes_ordered_by_the_planes_around_them() {
        let sloped = |f| {
            QuadBox::of(
                quad([2, 3, 4], 1, 1, f).with_fluid_heights([16, 14, 9, 14]),
                &[],
            )
        };
        // the top: no plane, the cell between its lowest and highest corner
        let top = sloped(Face::PosY);
        assert_eq!(
            (top.min, top.max, top.plane),
            ([32, 57, 64], [48, 64, 80], None)
        );
        // a flat one is a plane again
        let flat = QuadBox::of(
            quad([2, 3, 4], 1, 1, Face::PosY).with_fluid_heights([13; 4]),
            &[],
        );
        assert_eq!(flat.plane.map(|p| p.at), Some(48 + 13));
        // the sides end at the higher corner of their edge, the bottom is the cube's
        assert_eq!(sloped(Face::PosX).max[1], 48 + 14);
        assert_eq!(sloped(Face::NegX).max[1], 48 + 16);
        assert_eq!(sloped(Face::PosZ).max[1], 48 + 14);
        assert_eq!(sloped(Face::NegZ).max[1], 48 + 16);
        assert_eq!(
            sloped(Face::NegZ).plane.map(|p| (p.axis, p.at)),
            Some((2, 64))
        );
        assert_eq!(sloped(Face::NegY).plane.map(|p| p.at), Some(48));
        // A sloped top over a glass top one block down: the glass first from above, last from
        // below; the camera cell stops at the slope's box.
        let quads = [
            quad([2, 3, 4], 1, 1, Face::PosY).with_fluid_heights([16, 14, 9, 14]),
            quad([2, 1, 4], 1, 1, Face::PosY),
        ];
        assert_eq!(order_of(&quads, DVec3::new(2.5, 9.0, 4.5)), [1, 0]);
        assert_eq!(order_of(&quads, DVec3::new(2.5, 1.5, 4.5)), [0, 1]);
        let (_, cell) = sort_order(&quads, &[], DVec3::new(2.5, 9.0, 4.5));
        assert_eq!(cell.lo[1], 64.0);
        let (_, cell) = sort_order(&quads, &[], DVec3::new(2.5, 3.2, 4.5));
        assert_eq!((cell.lo[1], cell.hi[1]), (32.0, 57.0));
    }

    fn order_of(quads: &[PackedQuad], eye: DVec3) -> Vec<u32> {
        sort_order(quads, &[], eye).0
    }

    #[test]
    fn parallel_quads_go_far_to_near_and_flip_when_the_camera_crosses() {
        // two horizontal quads, the camera above both, between them, below both
        let quads = [
            quad([0, 2, 0], 4, 4, Face::PosY),
            quad([0, 8, 0], 4, 4, Face::PosY),
        ];
        assert_eq!(order_of(&quads, DVec3::new(2.0, 20.0, 2.0)), [0, 1]);
        assert_eq!(order_of(&quads, DVec3::new(2.0, -5.0, 2.0)), [1, 0]);
        // far to the side too: the planes decide, not the distance
        assert_eq!(order_of(&quads, DVec3::new(100.0, 20.0, 2.0)), [0, 1]);
        assert_eq!(order_of(&quads, DVec3::new(100.0, 5.0, 2.0)), [1, 0]);
    }

    #[test]
    fn a_large_surface_goes_before_a_small_quad_under_it_near_the_camera() {
        // A 32x32 water surface at y = 10 and a small glass top under it, right below the camera
        // at one corner: the distance to the centers puts the surface farther (wrong), the planes
        // put the glass first.
        let water = quad([0, 9, 0], 32, 32, Face::PosY);
        let glass = quad([1, 7, 1], 1, 1, Face::PosY);
        let eye = DVec3::new(1.5, 11.0, 1.5);
        let boxes = [QuadBox::of_test(water), QuadBox::of_test(glass)];
        let e = (eye * 16.0).to_array();
        assert!(
            boxes[0].center_distance2(e) > boxes[1].center_distance2(e),
            "the distance key alone would draw the water first"
        );
        assert_eq!(order_of(&[water, glass], eye), [1, 0]);
    }

    #[test]
    fn coplanar_faces_go_away_facing_first() {
        // water's +X face and glass's -X face at x = 5
        let water = quad([4, 0, 0], 1, 1, Face::PosX);
        let glass = quad([5, 0, 0], 1, 1, Face::NegX);
        // camera on the water side (x < 5): the water face looks away
        assert_eq!(order_of(&[glass, water], DVec3::new(2.0, 0.5, 0.5)), [1, 0]);
        assert_eq!(order_of(&[glass, water], DVec3::new(8.0, 0.5, 0.5)), [0, 1]);
    }

    #[test]
    fn cycles_still_give_every_quad_once() {
        // A pinwheel: planes y = 2, x = 2 and z = 2, each quad on the camera's side of the
        // previous one's plane and across the next one's: 0 before 1 before 2 before 0.
        let quads = [
            quad([0, 1, 2], 4, 2, Face::PosY),
            quad([1, 2, 0], 4, 2, Face::PosX),
            quad([2, 0, 1], 2, 4, Face::PosZ),
            quad([8, 8, 8], 1, 1, Face::NegY),
        ];
        let eye = DVec3::new(10.0, 10.0, 10.0);
        let b = quads.map(QuadBox::of_test);
        let e = (eye * 16.0).to_array();
        assert_eq!(pair_order(&b[0], &b[1], e), Some(true));
        assert_eq!(pair_order(&b[1], &b[2], e), Some(true));
        assert_eq!(pair_order(&b[2], &b[0], e), Some(true));
        let mut order = order_of(&quads, eye);
        order.sort_unstable();
        assert_eq!(order, [0, 1, 2, 3]);
    }

    #[test]
    fn the_cell_bounds_the_order() {
        let quads = [
            quad([0, 2, 0], 4, 4, Face::PosY),
            quad([0, 8, 0], 4, 4, Face::PosY),
            quad([5, 0, 0], 1, 1, Face::PosX),
        ];
        let (_, cell) = sort_order(&quads, &[], DVec3::new(2.0, 5.0, 30.0));
        // between the two y planes (3 and 9), below the x plane (6), no z plane
        assert_eq!(cell.lo, [f64::NEG_INFINITY, 48.0, f64::NEG_INFINITY]);
        assert_eq!(cell.hi, [96.0, 144.0, f64::INFINITY]);
        assert!(cell.contains([-1e9, 100.0, -1e9]));
        assert!(!cell.contains([50.0, 150.0, 0.0]));
        // exactly on a plane counts as below it
        assert!(cell.contains([96.0, 144.0, 0.0]));
        assert!(!cell.contains([97.0, 100.0, 0.0]));
        assert!(!cell.contains([50.0, 48.0, 0.0]));
    }

    #[test]
    fn far_chunks_keep_their_order_for_an_eighth_of_the_distance() {
        let quads = [
            quad([2, 2, 2], 1, 1, Face::PosX),
            quad([6, 2, 2], 1, 1, Face::PosX),
        ];
        // near: every plane crossing counts
        let (_, near) = sort_order(&quads, &[], DVec3::new(5.0, 20.0, 40.0));
        assert_eq!(near.slack2, 0.0);
        assert!(!near.contains([7.5 * 16.0, 20.0 * 16.0, 40.0 * 16.0]));
        // 240 voxels away (176 past the exact distance): 22 voxels of slack, then the cell
        let eye = DVec3::new(5.0, 16.0, 256.0);
        let (_, far) = sort_order(&quads, &[], eye);
        let at = |x: f64| [x * 16.0, 16.0 * 16.0, 256.0 * 16.0];
        assert!(far.contains(at(26.9)), "crossed x = 7, within the slack");
        assert!(!far.contains(at(27.1)));
        assert!(far.contains(at(6.5)), "the cell");
    }

    #[test]
    fn large_buckets_use_the_distance_key_and_block_cells() {
        let n = TOPO_LIMIT + 1;
        let quads: Vec<PackedQuad> = (0..n)
            .map(|i| quad([(i % 32) as u32, (i / 32) as u32, 0], 1, 1, Face::PosZ))
            .collect();
        let eye = DVec3::new(0.5, 0.5, 10.0);
        let (order, cell) = sort_order(&quads, &[], eye);
        // farthest center first
        let d = |i: u32| {
            let b = QuadBox::of_test(quads[i as usize]);
            b.center_distance2((eye * 16.0).to_array())
        };
        assert!(order.windows(2).all(|w| d(w[0]) >= d(w[1])));
        assert_eq!(*order.last().expect("quads"), 0);
        // the block of the camera
        assert_eq!(cell.lo, [0.0, 0.0, 144.0]);
        assert_eq!(cell.hi, [16.0, 16.0, 160.0]);
        assert_eq!(cell.slack2, 0.0);
        // far away the order is kept for an eighth of the distance past 64 voxels
        let (_, far) = sort_order(&quads, &[], DVec3::new(336.0, 16.0, 16.0));
        assert_eq!(far.slack2.sqrt(), 32.0 * 16.0);
        assert!(far.contains([368.0 * 16.0, 256.0, 256.0]));
        assert!(!far.contains([368.5 * 16.0, 256.0, 256.0]));
    }

    // ------------------------------------------------------------ oracle

    /// Where the ray from `eye` along `dir` meets the quad (`t`), if it does (sixteenths).
    fn hit(b: &QuadBox, eye: [f64; 3], dir: [f64; 3]) -> Option<f64> {
        let p = b.plane?;
        let a = p.axis;
        if dir[a].abs() < 1e-12 {
            return None;
        }
        let t = (f64::from(p.at) - eye[a]) / dir[a];
        if t <= 1e-9 {
            return None;
        }
        (0..3)
            .filter(|&k| k != a)
            .all(|k| {
                let x = eye[k] + t * dir[k];
                f64::from(b.min[k]) < x && x < f64::from(b.max[k])
            })
            .then_some(t)
    }

    /// Checks `order` against rays from `eye` through a grid of points on every quad: along each
    /// ray, a quad hit farther must come earlier; two quads hit at the same depth (opposite faces
    /// of one plane) go away-facing first. Returns the pairs checked, or the first violation.
    fn check_with_rays(quads: &[PackedQuad], order: &[u32], eye: DVec3) -> Result<usize, String> {
        let eye = (eye * 16.0).to_array();
        let boxes: Vec<QuadBox> = quads.iter().map(|&q| QuadBox::of_test(q)).collect();
        let mut rank = vec![0; quads.len()];
        for (r, &i) in order.iter().enumerate() {
            rank[i as usize] = r;
        }
        let mut checked = 0;
        for target in &boxes {
            for s in 0..7 {
                for t in 0..7 {
                    // a point inside the quad, off the grid lines
                    let (fs, ft) = ((f64::from(s) + 0.37) / 7.0, (f64::from(t) + 0.61) / 7.0);
                    let mut free = (0..3).filter(|&k| target.min[k] != target.max[k]);
                    let (Some(ka), Some(kb)) = (free.next(), free.next()) else {
                        continue;
                    };
                    let mut p = target.min.map(f64::from);
                    p[ka] += fs * f64::from(target.max[ka] - target.min[ka]);
                    p[kb] += ft * f64::from(target.max[kb] - target.min[kb]);
                    let dir = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
                    let mut hits: Vec<(f64, usize)> = boxes
                        .iter()
                        .enumerate()
                        .filter_map(|(i, b)| hit(b, eye, dir).map(|t| (t, i)))
                        .collect();
                    hits.sort_by(|a, b| a.0.total_cmp(&b.0));
                    for (x, &(tx, i)) in hits.iter().enumerate() {
                        for &(ty, j) in &hits[x + 1..] {
                            checked += 1;
                            if ty - tx > 1e-9 {
                                // i in front of j
                                if rank[j] > rank[i] {
                                    return Err(format!(
                                        "quad {j} behind quad {i} drawn after it (eye {eye:?})"
                                    ));
                                }
                            } else {
                                let faces_eye = |q: usize| {
                                    let p = boxes[q].plane.expect("hit quads have planes");
                                    p.positive == (eye[p.axis] > f64::from(p.at))
                                };
                                let (first, second) = if faces_eye(i) { (j, i) } else { (i, j) };
                                if faces_eye(i) != faces_eye(j) && rank[first] > rank[second] {
                                    return Err(format!(
                                        "coplanar quads {first} and {second} (eye {eye:?})"
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(checked)
    }

    /// The translucent quads the real mesher makes for `blocks` in one chunk (no light).
    fn mesh_scene(blocks: &[((u32, u32, u32), &str)]) -> ChunkMesh {
        let registry = boxel_demo::blocks::registry();
        let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
        for &((x, y, z), name) in blocks {
            dense[LocalPos::new(x, y, z).index()] = registry.by_name(name).expect(name);
        }
        let chunk = ChunkData::from_dense(&dense);
        let materials = MaterialTable::from_registry(&registry);
        let input = MeshInput {
            center: &chunk,
            neighbors: [None; 26],
            light: None,
        };
        Mesher::new().mesh_chunk(&input, &materials, MeshOptions::default())
    }

    fn cuboid(
        min: (u32, u32, u32),
        max: (u32, u32, u32),
        name: &'static str,
    ) -> Vec<((u32, u32, u32), &'static str)> {
        let mut out = Vec::new();
        for x in min.0..max.0 {
            for y in min.1..max.1 {
                for z in min.2..max.2 {
                    out.push(((x, y, z), name));
                }
            }
        }
        out
    }

    /// Scenes of the GPU test: water and glass blobs, a glass box with a hollow inside, water in
    /// a glass pool and a glass block sunk in water.
    fn scenes() -> Vec<(&'static str, ChunkMesh)> {
        let mut pool = cuboid((8, 8, 8), (16, 11, 16), "boxel:glass");
        pool.retain(|&((x, y, z), _)| x == 8 || x == 15 || z == 8 || z == 15 || y == 8);
        pool.extend(cuboid((9, 9, 9), (15, 11, 15), "boxel:water"));
        let mut hollow = cuboid((10, 10, 10), (16, 16, 16), "boxel:glass");
        hollow.retain(|&((x, y, z), _)| [x, y, z].iter().any(|&c| c == 10 || c == 15));
        let mut sunk = cuboid((4, 4, 4), (20, 9, 20), "boxel:water");
        sunk.retain(|&((x, y, z), _)| (x, y, z) != (6, 6, 6));
        sunk.push(((6, 6, 6), "boxel:glass"));
        let mut blobs = cuboid((12, 8, 8), (16, 12, 12), "boxel:water");
        blobs.extend(cuboid((12, 8, 16), (16, 12, 20), "boxel:glass"));
        vec![
            ("blobs", mesh_scene(&blobs)),
            ("hollow box", mesh_scene(&hollow)),
            ("pool", mesh_scene(&pool)),
            ("sunk glass", mesh_scene(&sunk)),
        ]
    }

    #[test]
    fn sorted_scenes_match_ray_oracle_from_many_cameras() {
        let eyes = [
            DVec3::new(14.3, 20.7, 30.2),
            DVec3::new(-3.1, 9.4, -6.6),
            DVec3::new(13.1, 9.9, 13.7),
            DVec3::new(5.2, 12.3, 5.9),
            DVec3::new(40.5, 1.5, 12.25),
            DVec3::new(12.6, 10.6, 12.4),
            DVec3::new(6.5, 9.5, 6.5),
            DVec3::new(-60.0, 70.0, 80.0),
        ];
        let mut unsorted_failures = 0;
        for (name, mesh) in scenes() {
            let quads = mesh.bucket(TRANSLUCENT_BUCKET);
            assert!(
                quads.len() >= 6,
                "{name}: {} translucent quads",
                quads.len()
            );
            for eye in eyes {
                let (order, cell) = sort_order(quads, &[], eye);
                let checked =
                    check_with_rays(quads, &order, eye).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(checked > 0, "{name} {eye}: no overlaps checked");
                assert!(cell.contains((eye * 16.0).to_array()));
                // the mesher's order fails somewhere (the reason for the sort)
                let mesher: Vec<u32> = (0..quads.len() as u32).collect();
                if check_with_rays(quads, &mesher, eye).is_err() {
                    unsorted_failures += 1;
                }
            }
        }
        assert!(
            unsorted_failures > 4,
            "{unsorted_failures} mesher-order failures"
        );
    }

    #[test]
    fn the_order_is_the_same_anywhere_in_its_cell() {
        for (name, mesh) in scenes() {
            let quads = mesh.bucket(TRANSLUCENT_BUCKET);
            let eye = DVec3::new(13.3, 14.2, 25.1);
            let (_, cell) = sort_order(quads, &[], eye);
            // another point of the cell, then one across its +z bound if it has one
            let inside = DVec3::new(13.4, 14.25, 25.05);
            assert!(cell.contains((inside * 16.0).to_array()), "{name}");
            check_with_rays(quads, &sort_order(quads, &[], eye).0, inside)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    // ------------------------------------------------------------ sorter

    fn view_at(eye: DVec3, yaw: f32, pitch: f32) -> ViewParams {
        let camera = Camera::default();
        let transform = Transform::from_yaw_pitch(eye, yaw, pitch);
        let cv = CameraView::compute(&camera, &transform, 16.0 / 9.0);
        ViewParams::from_camera(&cv, &camera, [0.0; 4], 0.6)
    }

    /// Two translucent quads (y planes 3 and 9) and one opaque quad.
    fn small_mesh(layer: u16) -> Arc<ChunkMesh> {
        let quads = vec![
            PackedQuad::new([0, 0, 0], 1, 1, Face::PosY, 0),
            PackedQuad::new([0, 2, 0], 4, 4, Face::PosY, layer),
            PackedQuad::new([0, 8, 0], 4, 4, Face::PosY, layer),
        ];
        let mut counts = [0; BUCKET_COUNT];
        counts[0] = 1;
        counts[TRANSLUCENT_BUCKET] = 2;
        Arc::new(ChunkMesh::from_quads(quads, counts))
    }

    fn table(meshes: &[(u64, I64Vec3, &ChunkMesh)]) -> ChunkTable {
        let mut arena = ArenaAllocator::new(1 << 16, 1024);
        let mut table = ChunkTable::new(64);
        for &(id, origin, mesh) in meshes {
            let alloc = arena.allocate(mesh.total_quads() + 1).expect("arena");
            table.insert(ChunkRecord {
                id,
                origin,
                alloc,
                counts: mesh.bucket_counts,
                light_base: alloc.offset + mesh.total_quads(),
                light_len: 1,
            });
        }
        table
    }

    #[test]
    fn the_sorter_sorts_once_per_cell() {
        let mesh = small_mesh(3);
        let origin = I64Vec3::new(-64, 0, -64);
        let chunks = table(&[(1, origin, &mesh)]);
        let mut sorter = TranslucentSorter::new(TranslucentSort::Inline);
        sorter.upsert(1, origin, Arc::clone(&mesh));
        // an opaque-only chunk takes no state
        let mut opaque = (*mesh).clone();
        opaque.quads.truncate(1);
        opaque.bucket_counts[TRANSLUCENT_BUCKET] = 0;
        sorter.upsert(2, I64Vec3::ZERO, Arc::new(opaque));
        assert_eq!(sorter.len(), 1);
        let mut out = Vec::new();
        // above both planes (world y 20): the lower quad first
        let above = view_at(DVec3::new(-62.0, 20.0, -62.0), 0.0, -1.2);
        sorter.update(&chunks, &above, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, 1);
        assert_eq!(out[0].quads, mesh.bucket(TRANSLUCENT_BUCKET));
        out.clear();
        // moving inside the cell: nothing
        for y in [15.0, 12.0, 9.5] {
            sorter.update(
                &chunks,
                &view_at(DVec3::new(-61.0, y, -60.0), 0.3, -1.0),
                &mut out,
            );
            assert!(out.is_empty(), "y {y}");
        }
        // crossing y = 9: the upper quad goes first
        sorter.update(
            &chunks,
            &view_at(DVec3::new(-61.0, 5.0, -60.0), 0.3, -1.0),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        let reversed: Vec<_> = mesh
            .bucket(TRANSLUCENT_BUCKET)
            .iter()
            .rev()
            .copied()
            .collect();
        assert_eq!(out[0].quads, reversed);
        assert_eq!(sorter.stats(), SortStats { sorts: 2, quads: 4 });
        out.clear();
        // a new mesh is uploaded in the mesher's order: sorted again even in the same cell
        let other = small_mesh(4);
        sorter.upsert(1, origin, Arc::clone(&other));
        sorter.update(
            &chunks,
            &view_at(DVec3::new(-61.0, 5.0, -60.0), 0.3, -1.0),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert!(Arc::ptr_eq(&out[0].mesh, &other));
        out.clear();
        // removed: nothing more
        sorter.remove(1);
        sorter.update(&chunks, &above, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn results_of_replaced_meshes_are_dropped() {
        let mesh = small_mesh(3);
        let chunks = table(&[(1, I64Vec3::ZERO, &mesh)]);
        let mut sorter = TranslucentSorter::new(TranslucentSort::Inline);
        sorter.upsert(1, I64Vec3::ZERO, Arc::clone(&mesh));
        let view = view_at(DVec3::new(2.0, 20.0, 2.0), 0.0, -1.2);
        // the job answers, then a new mesh arrives before the answer is applied
        sorter.start_sorts(&chunks, &view);
        sorter.receive();
        assert_eq!(sorter.ready.len(), 1);
        let newer = small_mesh(5);
        sorter.upsert(1, I64Vec3::ZERO, Arc::clone(&newer));
        let mut out = Vec::new();
        sorter.apply(&mut out);
        assert!(out.is_empty(), "the old answer is dropped");
        sorter.update(&chunks, &view, &mut out);
        assert_eq!(out.len(), 1);
        assert!(Arc::ptr_eq(&out[0].mesh, &newer));
        // clear forgets everything
        sorter.clear();
        sorter.update(&chunks, &view, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn uploads_per_frame_are_capped_nearest_first() {
        // many chunks, each with a bucket of a quarter of the cap: four per frame
        let n = MAX_UPLOAD_QUADS / 4;
        let quads: Vec<PackedQuad> = (0..n)
            .map(|i| PackedQuad::new([i % 32, i / 32 % 32, 0], 1, 1, Face::PosZ, 1))
            .collect();
        let mut counts = [0; BUCKET_COUNT];
        counts[TRANSLUCENT_BUCKET] = n;
        let mesh = Arc::new(ChunkMesh::from_quads(quads, counts));
        let origins: Vec<(u64, I64Vec3)> = (0..6)
            .map(|i| (i, I64Vec3::new(32 * i as i64, 0, -64)))
            .collect();
        let chunks = table(
            &origins
                .iter()
                .map(|&(id, o)| (id, o, &*mesh))
                .collect::<Vec<_>>(),
        );
        let mut sorter = TranslucentSorter::new(TranslucentSort::Inline);
        for &(id, origin) in &origins {
            sorter.upsert(id, origin, Arc::clone(&mesh));
        }
        let view = view_at(DVec3::new(0.5, 16.0, 0.5), 0.0, 0.0);
        let mut out = Vec::new();
        sorter.update(&chunks, &view, &mut out);
        let ids: Vec<u64> = out.iter().map(|s| s.id).collect();
        assert_eq!(ids, [0, 1, 2, 3], "nearest first, four per frame");
        out.clear();
        sorter.update(&chunks, &view, &mut out);
        let ids: Vec<u64> = out.iter().map(|s| s.id).collect();
        assert_eq!(ids, [4, 5]);
        out.clear();
        sorter.update(&chunks, &view, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn off_and_background_modes() {
        let mesh = small_mesh(3);
        let chunks = table(&[(1, I64Vec3::ZERO, &mesh)]);
        let view = view_at(DVec3::new(2.0, 20.0, 2.0), 0.0, -1.2);
        let mut sorter = TranslucentSorter::new(TranslucentSort::Off);
        sorter.upsert(1, I64Vec3::ZERO, Arc::clone(&mesh));
        let mut out = Vec::new();
        sorter.update(&chunks, &view, &mut out);
        assert!(out.is_empty());
        // on the pool: the answer comes in a later frame
        sorter.set_mode(TranslucentSort::Background);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while out.is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "no answer from the pool"
            );
            sorter.update(&chunks, &view, &mut out);
            std::thread::yield_now();
        }
        assert_eq!(out.len(), 1);
        assert_eq!(sorter.in_flight_quads, 0);
    }
}
