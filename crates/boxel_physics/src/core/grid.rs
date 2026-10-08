//! The broadphase: a uniform grid of cubic cells `2^shift` blocks wide (4 by default) with `i64`
//! cell coordinates, so it works the same anywhere in the world (ADR 0016 phase P3).
//!
//! Each body is stored with its [`BlockBounds`] (the blocks its box reaches) and goes into every
//! cell those bounds touch, at most two per axis; a body wider than that goes to a short `large`
//! list that is tested against everything. Building the grid sorts the cell entries by a hash
//! of the cell with a counting sort (linear time, no allocation once the buffers have grown, and
//! stable, so the order only depends on the insertion order). Two bodies are a candidate pair
//! when their layers meet both ways, one of them is active and their bounds overlap; each pair
//! is reported only from its home cell, the one at `max(min cell of a, min cell of b)`, so a pair
//! that shares several cells is reported once. Pairs come out sorted by index: insert bodies in
//! entity order and the pairs are in entity order.
//!
//! The same cells answer box queries ([`Grid::query`]) and rays ([`Grid::cell_ray`] walks the
//! cells along a ray with an exact DDA, like the voxel raycast).

use super::ray::MAX_RAY_DISTANCE;
use super::shape::LocalBox;
use boxel_math::{DVec3, I64Vec3};
use boxel_voxel::{BlockPos, MAX_BLOCK_COORD, WorldPos};
use std::ops::Range;

/// Cells of the default grid are `2^2 = 4` blocks wide.
pub const DEFAULT_GRID_SHIFT: u32 = 2;

/// The largest cell size, `2^16` blocks: a larger shift is lowered to it.
pub const MAX_GRID_SHIFT: u32 = 16;

/// A body whose bounds span more cells than this on any axis goes to the large list.
pub const MAX_CELLS_PER_AXIS: i64 = 2;

/// Which bodies a collider meets: two colliders interact when each one's `member` bits share a
/// bit with the other's `mask`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CollisionLayers {
    /// The layers this collider is in.
    pub member: u32,
    /// The layers this collider meets.
    pub mask: u32,
}

impl Default for CollisionLayers {
    /// In layer 1, meets every layer.
    fn default() -> Self {
        Self {
            member: 1,
            mask: u32::MAX,
        }
    }
}

impl CollisionLayers {
    /// In `member`, meeting `mask`.
    pub const fn new(member: u32, mask: u32) -> Self {
        Self { member, mask }
    }

    /// `self` and `other` meet (checked both ways).
    pub const fn interacts(self, other: CollisionLayers) -> bool {
        self.member & other.mask != 0 && other.member & self.mask != 0
    }
}

/// The blocks a box reaches, inclusive on every axis: `min..=max`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlockBounds {
    /// The first block on each axis.
    pub min: I64Vec3,
    /// The last block on each axis.
    pub max: I64Vec3,
}

impl BlockBounds {
    /// The bounds `min..=max`.
    pub const fn new(min: I64Vec3, max: I64Vec3) -> Self {
        Self { min, max }
    }

    /// One block.
    pub const fn of_block(block: BlockPos) -> Self {
        let b = block.as_i64vec3();
        Self { min: b, max: b }
    }

    /// The blocks the box `bx` (relative to `pos`, which should be canonical) reaches:
    /// `floor(min)..=floor(max)` on each axis. A box that ends exactly on a block plane also
    /// reaches the next block (so two boxes touching there share it), one that starts on a plane
    /// does not reach the block before (a body resting on the ground stays in its cell). Two
    /// boxes that overlap by more than a rounding error always share a block. Computed in the
    /// frame of `pos.block`, so the same box gives the same bounds (shifted) anywhere. A
    /// non-finite corner counts as 0; coordinates saturate at the `i64` ends.
    pub fn of_box(pos: WorldPos, bx: LocalBox) -> Self {
        let lo = pos.frac + bx.min;
        let hi = pos.frac + bx.max;
        let block = pos.block.as_i64vec3();
        // `as` saturates and turns NaN into 0.
        let axis = |b: i64, v: f64| b.saturating_add(v.floor() as i64);
        Self {
            min: I64Vec3::new(
                axis(block.x, lo.x),
                axis(block.y, lo.y),
                axis(block.z, lo.z),
            ),
            max: I64Vec3::new(
                axis(block.x, hi.x),
                axis(block.y, hi.y),
                axis(block.z, hi.z),
            ),
        }
    }

    /// The two ranges share a block on every axis.
    #[inline]
    pub fn overlaps(&self, other: &BlockBounds) -> bool {
        self.min.cmple(other.max).all() && other.min.cmple(self.max).all()
    }

    /// The first and last cell of these bounds in a grid of `2^shift`-block cells.
    #[inline]
    pub fn cells(&self, shift: u32) -> (I64Vec3, I64Vec3) {
        (shr(self.min, shift), shr(self.max, shift))
    }
}

/// `v >> shift` on every component (an arithmetic shift: floor division by `2^shift`).
#[inline]
fn shr(v: I64Vec3, shift: u32) -> I64Vec3 {
    I64Vec3::new(v.x >> shift, v.y >> shift, v.z >> shift)
}

/// `GridBody::flags`: the body also covers the next cell on x (`<< 1`: y, `<< 2`: z).
const SPAN_X: u8 = 1;
/// `GridBody::flags`: pairs with this body are reported (one of the two must be active).
const ACTIVE: u8 = 1 << 3;
/// `GridBody::flags`: wider than [`MAX_CELLS_PER_AXIS`] cells, in the large list.
const LARGE: u8 = 1 << 4;
/// `GridBody::flags` (with `LARGE`): moved to other cells by [`Grid::set_bounds`] since the grid
/// was built. Its cell entries are stale, so it waits in the large list until the next build.
const DETACHED: u8 = 1 << 5;
/// `Entry::key`: the bits above this hold the entry's cell offset from its body's first cell
/// (x, y, z); the bits below, the cell's hash (masked to the bucket).
const OFFSET_SHIFT: u32 = 29;
/// The most buckets a grid uses (`2^29`).
const MAX_BUCKETS: usize = 1 << OFFSET_SHIFT;

/// A body in the grid, in one cache line: its first cell, its bounds relative to that cell's
/// first block (they fit `i32`: a body in cells spans at most two of them), layers and flags. A
/// large body keeps its full bounds apart (`Grid::large_bounds`).
///
/// [`Grid::slot`] places a body without changing the grid, so the arithmetic of
/// [`Grid::insert`] can run on any thread; [`Grid::push_slot`] and [`Grid::push_slots`] add
/// placed bodies.
#[derive(Clone, Copy, Debug, Default)]
pub struct GridSlot {
    cell_min: I64Vec3,
    rel_min: [i32; 3],
    rel_max: [i32; 3],
    layers: CollisionLayers,
    flags: u8,
}

/// The grid's own name for a placed body.
type GridBody = GridSlot;

impl GridSlot {
    /// The placement of a body with `bounds` in a grid of cells `2^shift` blocks wide
    /// ([`Grid::slot`] for a grid with that cell size; `shift` at most [`MAX_GRID_SHIFT`]).
    pub fn new(shift: u32, bounds: BlockBounds, layers: CollisionLayers, active: bool) -> Self {
        let shift = shift.min(MAX_GRID_SHIFT);
        let (min, max) = bounds.cells(shift);
        let span = max.saturating_sub(min);
        let mut body = GridBody {
            cell_min: min,
            layers,
            flags: if active { ACTIVE } else { 0 },
            ..GridBody::default()
        };
        if span.max_element() >= MAX_CELLS_PER_AXIS || span.min_element() < 0 {
            body.flags |= LARGE;
            return body;
        }
        for a in 0..3 {
            // The first block of the first cell: at most `bounds.min`, so no overflow.
            let base = min[a] << shift;
            // `min - base` is below `2^shift`, `max - base` below `2^(shift + 1)`.
            body.rel_min[a] = (bounds.min[a] - base) as i32;
            body.rel_max[a] = (bounds.max[a] - base) as i32;
            if span[a] == 1 {
                body.flags |= SPAN_X << a;
            }
        }
        body
    }

    /// Wider than [`MAX_CELLS_PER_AXIS`] cells on some axis: the grid keeps it in its large
    /// list, with its bounds.
    pub fn is_large(&self) -> bool {
        self.large()
    }

    fn active(&self) -> bool {
        self.flags & ACTIVE != 0
    }

    fn large(&self) -> bool {
        self.flags & LARGE != 0
    }

    fn detached(&self) -> bool {
        self.flags & DETACHED != 0
    }
}

/// A body in one cell, in 8 bytes: the cell is the body's first cell plus the offset in `key`.
#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    body: u32,
    /// The cell's hash (low bits, masked to the bucket) and its offset from the body's first
    /// cell (the bits from `OFFSET_SHIFT`: x, y, z).
    key: u32,
}

impl Entry {
    /// The offset bits (x, y, z) of the entry's cell from its body's first cell.
    fn offset(self) -> u32 {
        self.key >> OFFSET_SHIFT
    }
}

/// `cell_min` moved by the offset bits `offset` (x, y, z).
#[inline]
fn offset_cell(cell_min: I64Vec3, offset: u32) -> I64Vec3 {
    cell_min
        + I64Vec3::new(
            i64::from(offset & 1),
            i64::from(offset >> 1 & 1),
            i64::from(offset >> 2 & 1),
        )
}

/// The broadphase grid (see the module docs). Reuse one across steps: [`Grid::reset`] keeps the
/// buffers.
#[derive(Clone, Debug)]
pub struct Grid {
    shift: u32,
    bodies: Vec<GridBody>,
    large: Vec<u32>,
    /// The bounds of the large bodies, in the order of `large`.
    large_bounds: Vec<BlockBounds>,
    entries: Vec<Entry>,
    sorted: Vec<Entry>,
    /// Bucket `b` holds `sorted[starts[b]..starts[b + 1]]`.
    starts: Vec<u32>,
    cursor: Vec<u32>,
    bucket_mask: u32,
    built: bool,
    /// Bodies moved to other cells since the grid was built (see `DETACHED`).
    detached: usize,
}

impl Default for Grid {
    /// Cells of 4 blocks.
    fn default() -> Self {
        Self::new(DEFAULT_GRID_SHIFT)
    }
}

impl Grid {
    /// An empty grid with cells `2^shift` blocks wide (`shift` at most [`MAX_GRID_SHIFT`]).
    pub fn new(shift: u32) -> Self {
        Self {
            shift: shift.min(MAX_GRID_SHIFT),
            bodies: Vec::new(),
            large: Vec::new(),
            large_bounds: Vec::new(),
            entries: Vec::new(),
            sorted: Vec::new(),
            starts: Vec::new(),
            cursor: Vec::new(),
            bucket_mask: 0,
            built: false,
            detached: 0,
        }
    }

    /// The cell size is `2^shift` blocks.
    pub fn shift(&self) -> u32 {
        self.shift
    }

    /// Removes every body and sets the cell size (keeps the buffers).
    pub fn reset(&mut self, shift: u32) {
        self.shift = shift.min(MAX_GRID_SHIFT);
        self.bodies.clear();
        self.large.clear();
        self.large_bounds.clear();
        self.entries.clear();
        self.built = false;
        self.detached = 0;
    }

    /// Number of bodies.
    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    /// No bodies.
    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    /// Adds a body and returns its index (0, 1, 2... in insertion order). Pairs need one active
    /// body (pairs of two inactive bodies, like two walls, are never reported).
    pub fn insert(&mut self, bounds: BlockBounds, layers: CollisionLayers, active: bool) -> u32 {
        self.push_slot(self.slot(bounds, layers, active), bounds)
    }

    /// The placement of a body for [`Grid::push_slot`]: [`Grid::insert`] in two halves, so the
    /// arithmetic can run on other threads. Valid until the grid's cell size changes.
    pub fn slot(&self, bounds: BlockBounds, layers: CollisionLayers, active: bool) -> GridSlot {
        self.place(bounds, layers, active)
    }

    /// Adds a body placed by [`Grid::slot`] with `bounds` (the same ones) and returns its index:
    /// the same as [`Grid::insert`] with the slot's arguments.
    pub fn push_slot(&mut self, slot: GridSlot, bounds: BlockBounds) -> u32 {
        let index = u32::try_from(self.bodies.len()).expect("fewer than 2^32 bodies");
        if slot.large() {
            self.large.push(index);
            self.large_bounds.push(bounds);
        }
        self.bodies.push(slot);
        self.built = false;
        index
    }

    /// Adds `n` bodies at once: `fill` writes their slots ([`Grid::slot`] of each one, in index
    /// order; on any threads) and returns the large ones (`GridSlot::is_large`) with their
    /// bounds, as indices from the first new body, in any order. The same as [`Grid::insert`]
    /// for each, in order.
    pub fn push_slots(
        &mut self,
        n: usize,
        fill: impl FnOnce(&mut [GridSlot]) -> Vec<(u32, BlockBounds)>,
    ) {
        let first = self.bodies.len();
        u32::try_from(first + n).expect("fewer than 2^32 bodies");
        self.bodies.resize(first + n, GridSlot::default());
        let mut large = fill(&mut self.bodies[first..]);
        large.sort_unstable_by_key(|&(i, _)| i);
        for (i, bounds) in large {
            self.large.push(first as u32 + i);
            self.large_bounds.push(bounds);
        }
        self.built = false;
    }

    /// The bounds of body `index`.
    pub fn bounds(&self, index: u32) -> BlockBounds {
        let body = &self.bodies[index as usize];
        if body.large() {
            let at = self
                .large
                .binary_search(&index)
                .expect("a large body is in the large list");
            return self.large_bounds[at];
        }
        let shift = self.shift;
        let base = I64Vec3::new(
            body.cell_min.x << shift,
            body.cell_min.y << shift,
            body.cell_min.z << shift,
        );
        let rel = |r: [i32; 3]| I64Vec3::new(i64::from(r[0]), i64::from(r[1]), i64::from(r[2]));
        BlockBounds::new(base + rel(body.rel_min), base + rel(body.rel_max))
    }

    /// Moves body `index` to `bounds`. On a built grid, a body that changes cells waits in the
    /// large list (tested against everything, like a large body) until the next
    /// [`Grid::build`]: queries and pairs see it at its new place at once without a rebuild.
    pub fn set_bounds(&mut self, index: u32, bounds: BlockBounds) {
        let old = self.bodies[index as usize];
        let mut body = self.place(bounds, old.layers, old.active());
        if old.detached() {
            body.flags |= LARGE | DETACHED;
        } else if self.built && (body.cell_min, body.flags) != (old.cell_min, old.flags) {
            body.flags |= LARGE | DETACHED;
            self.detached += 1;
        }
        let at = self.large.partition_point(|&l| l < index);
        match (old.large(), body.large()) {
            (false, true) => {
                self.large.insert(at, index);
                self.large_bounds.insert(at, bounds);
            }
            (true, false) => {
                self.large.remove(at);
                self.large_bounds.remove(at);
            }
            (true, true) => self.large_bounds[at] = bounds,
            (false, false) => {}
        }
        self.bodies[index as usize] = body;
    }

    /// The grid body of `bounds`: its cells, whether it is large, and its relative bounds.
    fn place(&self, bounds: BlockBounds, layers: CollisionLayers, active: bool) -> GridBody {
        GridSlot::new(self.shift, bounds, layers, active)
    }

    /// The indices of the large bodies (wider than [`MAX_CELLS_PER_AXIS`] cells, and the ones
    /// [`Grid::set_bounds`] moved to other cells since the grid was built), ascending.
    pub fn large(&self) -> &[u32] {
        &self.large
    }

    /// The grid is built: queries use the cells (else they scan every body).
    pub fn is_built(&self) -> bool {
        self.built
    }

    /// Sorts the cell entries (a counting sort by cell hash). [`Grid::pairs`] builds the grid
    /// itself; queries need it built.
    pub fn build(&mut self) {
        if self.detached > 0 {
            self.reattach();
        }
        self.entries.clear();
        for (index, body) in self.bodies.iter().enumerate() {
            if body.large() {
                continue;
            }
            let span = body.flags & (SPAN_X | SPAN_X << 1 | SPAN_X << 2);
            for offset in 0..8u8 {
                // Only the offsets within the body's span.
                if offset & !span != 0 {
                    continue;
                }
                let offset = u32::from(offset);
                let cell = offset_cell(body.cell_min, offset);
                let hash = cell_hash(cell) as u32 & (MAX_BUCKETS as u32 - 1);
                self.entries.push(Entry {
                    body: index as u32,
                    key: hash | offset << OFFSET_SHIFT,
                });
            }
        }
        let buckets = self
            .entries
            .len()
            .next_power_of_two()
            .clamp(16, MAX_BUCKETS);
        let mask = buckets as u32 - 1;
        self.bucket_mask = mask;
        self.starts.clear();
        self.starts.resize(buckets + 1, 0);
        for entry in &self.entries {
            self.starts[(entry.key & mask) as usize + 1] += 1;
        }
        for b in 1..=buckets {
            self.starts[b] += self.starts[b - 1];
        }
        self.cursor.clear();
        self.cursor.extend_from_slice(&self.starts[..buckets]);
        // Every slot is written below: only the new ones need a value.
        self.sorted.truncate(self.entries.len());
        self.sorted.resize(self.entries.len(), Entry::default());
        for entry in &self.entries {
            let at = &mut self.cursor[(entry.key & mask) as usize];
            self.sorted[*at as usize] = *entry;
            *at += 1;
        }
        self.built = true;
    }

    /// Puts the bodies [`Grid::set_bounds`] moved since the last build back in their cells
    /// (out of the large list, unless they are large).
    fn reattach(&mut self) {
        let mut kept = 0;
        for k in 0..self.large.len() {
            let (index, bounds) = (self.large[k], self.large_bounds[k]);
            let body = self.bodies[index as usize];
            if body.detached() {
                let (layers, active) = (body.layers, body.active());
                let placed = self.place(bounds, layers, active);
                self.bodies[index as usize] = placed;
                if !placed.large() {
                    continue;
                }
            }
            self.large[kept] = index;
            self.large_bounds[kept] = bounds;
            kept += 1;
        }
        self.large.truncate(kept);
        self.large_bounds.truncate(kept);
        self.detached = 0;
    }

    /// `a` and `b` (indices) are a candidate pair: one active, layers meet, bounds overlap.
    #[inline]
    fn candidate(&self, a: u32, b: u32) -> bool {
        let (first, second) = (&self.bodies[a as usize], &self.bodies[b as usize]);
        (first.active() || second.active())
            && first.layers.interacts(second.layers)
            && self.bounds(a).overlaps(&self.bounds(b))
    }

    /// Every candidate pair `(a, b)`, `a < b`, sorted, into `out` (cleared first). Builds the
    /// grid when needed.
    pub fn pairs(&mut self, out: &mut Vec<(u32, u32)>) {
        out.clear();
        if !self.built || self.detached > 0 {
            self.build();
        }
        self.pairs_in(0..self.sorted.len(), out);
        self.large_pairs(out);
        out.sort_unstable();
    }

    /// Splits the built grid's cell entries into at most `parts` ranges of about the same size
    /// that start and end on bucket boundaries: [`Grid::pairs_in`] on each (on any thread, in
    /// any order) plus [`Grid::large_pairs`] gives every pair of [`Grid::pairs`] once. Empty
    /// when the grid is not built.
    pub fn pair_ranges(&self, parts: usize) -> Vec<Range<usize>> {
        let len = if self.built { self.sorted.len() } else { 0 };
        let parts = parts.max(1);
        let mut ranges = Vec::with_capacity(parts);
        let mut start = 0;
        while start < len {
            let mut end = (start + len.div_ceil(parts)).min(len);
            // Never split a bucket.
            while end < len
                && self.sorted[end].key & self.bucket_mask
                    == self.sorted[end - 1].key & self.bucket_mask
            {
                end += 1;
            }
            ranges.push(start..end);
            start = end;
        }
        ranges
    }

    /// The candidate pairs `(a, b)`, `a < b`, of the cells whose entries are in `range` (from
    /// [`Grid::pair_ranges`]), appended to `out` unsorted. Large bodies (and bodies moved since
    /// the build) are not in cells: see [`Grid::large_pairs`]. Nothing when the grid is not
    /// built.
    pub fn pairs_in(&self, range: Range<usize>, out: &mut Vec<(u32, u32)>) {
        if !self.built {
            return;
        }
        let (sorted, bodies, mask, shift) = (
            &self.sorted[range],
            &self.bodies,
            self.bucket_mask,
            self.shift,
        );
        // Bodies moved since the build are in the large list, not in their old cells.
        let moved = |body: &GridBody| self.detached > 0 && body.detached();
        let mut start = 0;
        while start < sorted.len() {
            let bucket = sorted[start].key & mask;
            let mut end = start + 1;
            while end < sorted.len() && sorted[end].key & mask == bucket {
                end += 1;
            }
            // Entries of one bucket are in insertion order (the counting sort is stable), so
            // in one cell `p < q` means body `a < b`.
            for p in start..end.saturating_sub(1) {
                let ep = sorted[p];
                let first = &bodies[ep.body as usize];
                let off_p = ep.offset();
                let cell = offset_cell(first.cell_min, off_p);
                for eq in &sorted[p + 1..end] {
                    let off_q = eq.offset();
                    // The home cell (the first cell both cover) is this one when, on every
                    // axis, one of the two starts here.
                    if off_p & off_q != 0 {
                        continue;
                    }
                    let second = &bodies[eq.body as usize];
                    if offset_cell(second.cell_min, off_q) == cell
                        && (first.active() || second.active())
                        && first.layers.interacts(second.layers)
                        && overlap_in_cell(first, off_p, second, off_q, shift)
                        && !moved(first)
                        && !moved(second)
                    {
                        out.push((ep.body, eq.body));
                    }
                }
            }
            start = end;
        }
    }

    /// The candidate pairs `(a, b)`, `a < b`, with a large body, appended to `out` unsorted.
    pub fn large_pairs(&self, out: &mut Vec<(u32, u32)>) {
        for &l in &self.large {
            for (i, body) in self.bodies.iter().enumerate() {
                let i = i as u32;
                // A pair of large bodies is tested once, from the lower index.
                if i == l || (body.large() && i < l) || !self.candidate(l, i) {
                    continue;
                }
                out.push((l.min(i), l.max(i)));
            }
        }
    }

    /// The bodies of one cell (none when the grid is not built; large bodies, and bodies moved
    /// since the build, are never in a cell).
    pub fn cell(&self, cell: I64Vec3) -> impl Iterator<Item = u32> + '_ {
        let range = if self.built {
            let bucket = (cell_hash(cell) as u32 & self.bucket_mask) as usize;
            self.starts[bucket] as usize..self.starts[bucket + 1] as usize
        } else {
            0..0
        };
        self.sorted[range]
            .iter()
            .filter(move |e| {
                let body = &self.bodies[e.body as usize];
                offset_cell(body.cell_min, e.offset()) == cell && !body.detached()
            })
            .map(|e| e.body)
    }

    /// Every body whose bounds overlap `query`, ascending, into `out` (appended). Uses the cells
    /// when the grid is built and the query covers few of them, else scans every body.
    pub fn query(&self, query: BlockBounds, out: &mut Vec<u32>) {
        let first = out.len();
        let (qmin, qmax) = query.cells(self.shift);
        let span = qmax.saturating_sub(qmin);
        // A huge query (an infinite box saturates at the `i64` ends) counts as `u128::MAX`
        // cells: it scans the bodies.
        let cells = if span.min_element() < 0 {
            u128::MAX
        } else {
            let axis = |s: i64| s as u128 + 1;
            axis(span.x)
                .checked_mul(axis(span.y))
                .and_then(|c| c.checked_mul(axis(span.z)))
                .unwrap_or(u128::MAX)
        };
        if !self.built || cells.saturating_mul(8) > self.bodies.len() as u128 {
            out.extend((0..self.bodies.len() as u32).filter(|&i| self.bounds(i).overlaps(&query)));
            return;
        }
        for z in qmin.z..=qmax.z {
            for y in qmin.y..=qmax.y {
                for x in qmin.x..=qmax.x {
                    let cell = I64Vec3::new(x, y, z);
                    for i in self.cell(cell) {
                        let body = &self.bodies[i as usize];
                        // Reported from one cell only: the first cell both cover.
                        if body.cell_min.max(qmin) == cell && self.bounds(i).overlaps(&query) {
                            out.push(i);
                        }
                    }
                }
            }
        }
        out.extend(
            self.large
                .iter()
                .zip(&self.large_bounds)
                .filter(|(_, bounds)| bounds.overlaps(&query))
                .map(|(&l, _)| l),
        );
        out[first..].sort_unstable();
    }

    /// The cells along the ray `origin + dir * t`, `t` in `0..=max_t`, in order, each with the
    /// `t` where the ray enters it (0 for the first). The same exact DDA as the voxel raycast,
    /// in cell units: the steps never accumulate, ties step x, then y, then z. The ray ends at
    /// `max_t` (NaN counts as 0), at `MAX_RAY_DISTANCE` blocks on its longest axis and at the
    /// world limit. A `dir` with a non-finite component counts as zero (only the first cell).
    pub fn cell_ray(&self, origin: WorldPos, dir: DVec3, max_t: f64) -> CellRay {
        CellRay::new(origin, dir, max_t, self.shift)
    }
}

/// Bodies `a` and `b`, both in the cell at offsets `off_a` and `off_b` from their first cells,
/// share a block on every axis (their bounds compared in that cell's frame, in small integers).
#[inline]
fn overlap_in_cell(a: &GridBody, off_a: u32, b: &GridBody, off_b: u32, shift: u32) -> bool {
    (0..3).all(|axis| {
        // The cell starts `2^shift` blocks after the body's first cell when its offset bit is
        // set: the body's bounds in the cell's frame move back by that much.
        let back = |off: u32| ((off >> axis & 1) << shift) as i32;
        let (da, db) = (back(off_a), back(off_b));
        a.rel_min[axis] - da <= b.rel_max[axis] - db && b.rel_min[axis] - db <= a.rel_max[axis] - da
    })
}

/// A fixed hash of a cell (the same on every run and platform).
#[inline]
fn cell_hash(c: I64Vec3) -> u64 {
    let mut h = (c.x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (c.y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (c.z as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^ (h >> 33)
}

/// The cells along a ray ([`Grid::cell_ray`]): `(cell, t where the ray enters it)`.
#[derive(Clone, Debug)]
pub struct CellRay {
    cell: [i64; 3],
    step: [i64; 3],
    /// Offset of each axis' next cell plane from the first cell.
    plane: [i64; 3],
    /// The origin inside its cell, in cell units (`[0, 1)`).
    frac: DVec3,
    inv: [f64; 3],
    next: [f64; 3],
    max_t: f64,
    started: bool,
    done: bool,
    limit: i64,
}

impl CellRay {
    fn new(origin: WorldPos, dir: DVec3, max_t: f64, shift: u32) -> Self {
        let origin = origin.normalize();
        let dir = if dir.is_finite() { dir } else { DVec3::ZERO };
        let max_t = if max_t.is_nan() {
            0.0
        } else {
            max_t.min(MAX_RAY_DISTANCE / dir.abs().max_element())
        };
        let block = origin.block.as_i64vec3();
        let cell = shr(block, shift);
        // Powers of two: the scaling is exact.
        let size = (1u64 << shift) as f64;
        let within =
            (block - I64Vec3::new(cell.x << shift, cell.y << shift, cell.z << shift)).as_dvec3();
        let frac = (within + origin.frac) / size;
        let dir_cells = dir / size;
        let mut step = [0i64; 3];
        let mut plane = [0i64; 3];
        let mut inv = [0.0; 3];
        let mut next = [f64::INFINITY; 3];
        for a in 0..3 {
            let recip = 1.0 / dir_cells[a];
            if !recip.is_finite() {
                continue;
            }
            if dir_cells[a] > 0.0 {
                step[a] = 1;
                plane[a] = 1;
            } else {
                step[a] = -1;
            }
            inv[a] = recip;
            next[a] = (plane[a] as f64 - frac[a]) * recip;
        }
        Self {
            cell: [cell.x, cell.y, cell.z],
            step,
            plane,
            frac,
            inv,
            next,
            max_t,
            started: false,
            done: false,
            limit: (MAX_BLOCK_COORD >> shift) + 64,
        }
    }
}

impl Iterator for CellRay {
    type Item = (I64Vec3, f64);

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let as_vec = |c: [i64; 3]| I64Vec3::new(c[0], c[1], c[2]);
        if !self.started {
            self.started = true;
            return Some((as_vec(self.cell), 0.0));
        }
        let a = if self.next[0] <= self.next[1] && self.next[0] <= self.next[2] {
            0
        } else if self.next[1] <= self.next[2] {
            1
        } else {
            2
        };
        let t = self.next[a];
        // `t` is never NaN (finite planes, finite reciprocals).
        if t > self.max_t || t == f64::INFINITY {
            self.done = true;
            return None;
        }
        self.cell[a] += self.step[a];
        self.plane[a] += self.step[a];
        self.next[a] = (self.plane[a] as f64 - self.frac[a]) * self.inv[a];
        if self.cell[a].abs() > self.limit {
            self.done = true;
            return None;
        }
        Some((as_vec(self.cell), t))
    }
}
