//! Cave culling (ADR 0022): which chunks the camera may see through open space.
//!
//! Every chunk knows which pairs of its 6 faces see each other through voxels that do not
//! occlude ([`boxel_voxel::ChunkFaceInfo`], computed by the generation job and again by the mesh
//! job after an edit). From the camera's chunk, a breadth-first search walks to the neighbors,
//! entering each chunk through a face and leaving only through the faces linked to it, as
//! Minecraft and Sodium do:
//!
//! - It never walks back toward the camera along an axis: a chunk on the `+x` side of the camera
//!   is only left through `+x`, never `-x` (the chunk sequence of a straight line of sight does
//!   the same). So every step moves one chunk farther in Manhattan distance, and the queue
//!   handles the chunks distance by distance: when a chunk is taken, every face it can be
//!   entered through is already known.
//! - It only walks chunks that touch the view frustum (a line of sight stays inside it), except
//!   the 26 around the camera's chunk.
//! - The camera's chunk sees its 6 neighbors whatever its links.
//! - A chunk that is not loaded, not generated or whose mesh is not settled yet counts as open
//!   ([`ChunkSlot::visibility_links`](crate::ChunkSlot::visibility_links)), so a chunk the
//!   renderer shows is never hidden by blocks it does not show yet.
//!
//! The search covers a box around the camera's chunk (the viewer's mesh radius); a chunk
//! outside it is always visible ([`VisibleSet::is_visible`]). A path cannot leave an
//! axis-aligned box and come back without walking back along an axis, so the box loses nothing.
//!
//! [`ChunkVisibility`] keeps the links of the box in a grid that follows the camera (only the
//! slabs that enter it are read from the map when the camera changes chunk) and is patched by
//! the map's changes every frame ([`WorldSystems::Visibility`](crate::WorldSystems)). The
//! renderer runs the search for its own camera in its extract step, every frame the camera, its
//! view or the links change, so a chunk that just became visible is never drawn late.
use boxel_math::{Aabb, Frustum, I64Vec3, Vec3};
use boxel_voxel::{ALL_LINKS, CHUNK_SHIFT, ChunkFaceInfo, ChunkPos, Face};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Largest box half extents (chunks) the search covers: 64 around and 32 up and down (a grid of
/// at most 256 x 128 x 256 links, 16 MiB). A larger view is clamped (chunks outside the box are
/// always visible).
pub const MAX_VISIBILITY_RADIUS: u32 = 64;
/// See [`MAX_VISIBILITY_RADIUS`].
pub const MAX_VISIBILITY_VERTICAL_RADIUS: u32 = 32;

/// Where the search starts and what it may see.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisibilityView {
    /// The chunk of the eye: the chunk of `render_origin` (the renderer's integer eye).
    pub center: ChunkPos,
    /// Integer camera position (voxels); `frustum` is relative to it, as in the renderer.
    pub render_origin: I64Vec3,
    /// The camera's frustum relative to `render_origin` (`None`: every direction).
    pub frustum: Option<Frustum>,
    /// Half extent of the searched box around `center`, in chunks, along x and z.
    pub radius: u32,
    /// Half extent of the searched box along y.
    pub vertical_radius: u32,
}

impl VisibilityView {
    /// The view of a camera at integer position `render_origin` with `frustum` (relative to it),
    /// covering `radius` chunks around and `vertical_radius` up and down.
    pub fn new(
        render_origin: I64Vec3,
        frustum: Option<Frustum>,
        radius: u32,
        vertical_radius: u32,
    ) -> Self {
        Self {
            center: ChunkPos::from_i64vec3(render_origin >> CHUNK_SHIFT),
            render_origin,
            frustum,
            radius,
            vertical_radius,
        }
    }

    /// The box half extents, clamped to [`MAX_VISIBILITY_RADIUS`] and
    /// [`MAX_VISIBILITY_VERTICAL_RADIUS`].
    fn half(&self) -> [i64; 3] {
        let r = i64::from(self.radius.min(MAX_VISIBILITY_RADIUS));
        let v = i64::from(self.vertical_radius.min(MAX_VISIBILITY_VERTICAL_RADIUS));
        [r, v, r]
    }
}

/// The result of a search: one bit per chunk of the box, set when the camera may see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleSet {
    /// Box min corner.
    min: ChunkPos,
    /// Box size in chunks (x, y, z).
    size: [u64; 3],
    /// Bit `(y * size_z + z) * size_x + x` of the box-local position.
    bits: Vec<u64>,
    /// Bits set.
    count: usize,
}

impl VisibleSet {
    /// True if the camera may see chunk `pos`: always for a chunk outside the searched box.
    #[inline]
    pub fn is_visible(&self, pos: ChunkPos) -> bool {
        let Some(i) = self.local_index(pos) else {
            return true;
        };
        self.bits[i >> 6] >> (i & 63) & 1 != 0
    }

    /// True if `pos` is inside the searched box.
    pub fn covers(&self, pos: ChunkPos) -> bool {
        self.local_index(pos).is_some()
    }

    /// Chunks of the box the camera may see.
    pub fn visible_count(&self) -> usize {
        self.count
    }

    /// Chunks in the box.
    pub fn box_len(&self) -> usize {
        (self.size[0] * self.size[1] * self.size[2]) as usize
    }

    /// The searched box: min and max corners (inclusive).
    pub fn bounds(&self) -> (ChunkPos, ChunkPos) {
        let [sx, sy, sz] = self.size.map(|s| s as i64);
        let max = ChunkPos::new(
            self.min.x + sx - 1,
            self.min.y + sy - 1,
            self.min.z + sz - 1,
        );
        (self.min, max)
    }

    #[inline]
    fn local_index(&self, pos: ChunkPos) -> Option<usize> {
        // Exact for any two positions inside the world limit (|coordinate| <= 2^56).
        let dx = pos.x.wrapping_sub(self.min.x) as u64;
        let dy = pos.y.wrapping_sub(self.min.y) as u64;
        let dz = pos.z.wrapping_sub(self.min.z) as u64;
        let [sx, sy, sz] = self.size;
        (dx < sx && dy < sy && dz < sz).then(|| ((dy * sz + dz) * sx + dx) as usize)
    }
}

/// Counters of the searches of a [`ChunkVisibility`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VisibilityStats {
    /// Searches run since start.
    pub searches: u64,
    /// Updates that reused the last result (nothing changed).
    pub reused: u64,
    /// Duration of the last search, grid refresh included.
    pub last_search: Duration,
    /// Chunks the last search reached (visible chunks of its box).
    pub reached: usize,
    /// Chunks in the box of the last search.
    pub box_chunks: usize,
    /// Positions read from the map by the last grid refresh (0 when the camera stayed in its
    /// chunk; the whole box after a jump).
    pub refilled: usize,
}

/// Bits of the search state of a cell.
const INCOMING: u8 = 0x3f;
const QUEUED: u8 = 1 << 6;
const OUTSIDE: u8 = 1 << 7;

/// The links of the box around the camera, in a grid that wraps around (power-of-two sides, a
/// position goes to its coordinates modulo the sides), so moving the box only rewrites the
/// slabs that enter it.
#[derive(Debug, Default)]
struct LinkGrid {
    /// Bits of the x, y and z sides.
    bits: [u32; 3],
    links: Vec<u16>,
    /// The box the cells hold: min corner and size. `None`: nothing valid.
    held: Option<(ChunkPos, [i64; 3])>,
}

impl LinkGrid {
    #[inline]
    fn cell(&self, x: i64, y: i64, z: i64) -> usize {
        let [bx, by, bz] = self.bits;
        let mx = (1u64 << bx) - 1;
        let my = (1u64 << by) - 1;
        let mz = (1u64 << bz) - 1;
        (((y as u64 & my) << (bx + bz)) | ((z as u64 & mz) << bx) | (x as u64 & mx)) as usize
    }

    /// Makes the grid hold box `(min, size)`: positions that were not in the held box are read
    /// with `links`. Returns how many were read.
    fn hold(&mut self, min: ChunkPos, size: [i64; 3], links: &impl Fn(ChunkPos) -> u16) -> usize {
        let need = size.map(|s| (s as u64).next_power_of_two().trailing_zeros());
        let old = match self.held {
            Some((old_min, old_size)) if old_size == size && need == self.bits => {
                if old_min == min {
                    return 0;
                }
                Some(old_min)
            }
            _ => None,
        };
        if old.is_none() {
            self.bits = need;
            let cells = 1usize << (need[0] + need[1] + need[2]);
            self.links.clear();
            self.links.resize(cells, ALL_LINKS);
        }
        self.held = Some((min, size));
        let (x0, x1) = (min.x, min.x + size[0]);
        let mut read = 0;
        for y in min.y..min.y + size[1] {
            for z in min.z..min.z + size[2] {
                // The x ranges of this row the old box did not hold.
                let kept = old.filter(|o| {
                    (o.y..o.y + size[1]).contains(&y) && (o.z..o.z + size[2]).contains(&z)
                });
                let ranges = match kept {
                    Some(o) => [x0..o.x.clamp(x0, x1), (o.x + size[0]).clamp(x0, x1)..x1],
                    None => [x0..x1, x1..x1],
                };
                for x in ranges.into_iter().flatten() {
                    let i = self.cell(x, y, z);
                    self.links[i] = links(ChunkPos::new(x, y, z));
                    read += 1;
                }
            }
        }
        read
    }

    /// Sets the links of `pos` if the grid holds it. Returns true if they changed.
    fn set(&mut self, pos: ChunkPos, links: u16) -> bool {
        let Some((min, size)) = self.held else {
            return false;
        };
        let inside = (pos.x.wrapping_sub(min.x) as u64) < size[0] as u64
            && (pos.y.wrapping_sub(min.y) as u64) < size[1] as u64
            && (pos.z.wrapping_sub(min.z) as u64) < size[2] as u64;
        if !inside {
            return false;
        }
        let i = self.cell(pos.x, pos.y, pos.z);
        let changed = self.links[i] != links;
        self.links[i] = links;
        changed
    }
}

/// The breadth-first search over a [`LinkGrid`] (scratch kept between runs).
#[derive(Debug, Default)]
struct Search {
    /// Per box cell: incoming faces, queued, outside the frustum (all zero between runs).
    state: Vec<u8>,
    queue: Vec<u32>,
    outside: Vec<u32>,
}

impl Search {
    /// Searches the box the grid holds (centered on `view.center`) and returns the visible set.
    fn run(&mut self, grid: &LinkGrid, view: &VisibilityView) -> VisibleSet {
        let Some((min, size)) = grid.held else {
            unreachable!("the grid holds the view's box");
        };
        let [sx, sy, sz] = size.map(|s| s as usize);
        let cells = sx * sy * sz;
        if self.state.len() < cells {
            self.state.resize(cells, 0);
        }
        let half = view.half();
        let center = [half[0] as usize, half[1] as usize, half[2] as usize];
        let local = |x: usize, y: usize, z: usize| (y * sz + z) * sx + x;
        // Queue entries are the box-local coordinates, 10 bits each (sides are at most 129).
        let pack = |x: usize, y: usize, z: usize| (x | y << 10 | z << 20) as u32;
        let unpack = |e: u32| {
            let e = e as usize;
            (e & 0x3ff, e >> 10 & 0x3ff, e >> 20)
        };
        // Chunk AABBs relative to the render origin, as the renderer culls them (in i64 first).
        let base = (min.as_i64vec3() << CHUNK_SHIFT).saturating_sub(view.render_origin);
        let start = local(center[0], center[1], center[2]);
        self.queue.clear();
        self.outside.clear();
        self.queue.push(pack(center[0], center[1], center[2]));
        self.state[start] = QUEUED;
        let mut head = 0;
        while head < self.queue.len() {
            let (x, y, z) = unpack(self.queue[head]);
            head += 1;
            let i = local(x, y, z);
            let out = if i == start {
                0x3f
            } else {
                let links =
                    grid.links[grid.cell(min.x + x as i64, min.y + y as i64, min.z + z as i64)];
                let incoming = self.state[i] & INCOMING;
                let mut out = 0;
                for f in Face::ALL {
                    if incoming & (1 << f.index()) != 0 {
                        out |= ChunkFaceInfo::linked_faces(links, f);
                    }
                }
                // Never back toward the camera along an axis.
                let side = |p: usize, c: usize, plus: usize, minus: usize| -> u8 {
                    match p.cmp(&c) {
                        std::cmp::Ordering::Greater => 1 << plus,
                        std::cmp::Ordering::Less => 1 << minus,
                        std::cmp::Ordering::Equal => 1 << plus | 1 << minus,
                    }
                };
                out & (side(y, center[1], 0, 1)
                    | side(x, center[0], 2, 3)
                    | side(z, center[2], 4, 5))
            };
            for f in Face::ALL {
                if out & (1 << f.index()) == 0 {
                    continue;
                }
                let (nx, ny, nz) = match f {
                    Face::PosY if y + 1 < sy => (x, y + 1, z),
                    Face::NegY if y > 0 => (x, y - 1, z),
                    Face::PosX if x + 1 < sx => (x + 1, y, z),
                    Face::NegX if x > 0 => (x - 1, y, z),
                    Face::PosZ if z + 1 < sz => (x, y, z + 1),
                    Face::NegZ if z > 0 => (x, y, z - 1),
                    _ => continue,
                };
                let n = local(nx, ny, nz);
                let state = self.state[n];
                if state & OUTSIDE != 0 {
                    continue;
                }
                if state & QUEUED == 0 {
                    let near = nx.abs_diff(center[0]) <= 1
                        && ny.abs_diff(center[1]) <= 1
                        && nz.abs_diff(center[2]) <= 1;
                    if !near && let Some(frustum) = &view.frustum {
                        let offset = I64Vec3::new(nx as i64, ny as i64, nz as i64) << CHUNK_SHIFT;
                        let lo = base.saturating_add(offset).as_vec3();
                        if !frustum.intersects_aabb(&Aabb::new(lo, lo + Vec3::splat(32.0))) {
                            self.state[n] = OUTSIDE;
                            self.outside.push(n as u32);
                            continue;
                        }
                    }
                    self.queue.push(pack(nx, ny, nz));
                }
                // Entered through the face toward this chunk.
                let entry = f.opposite().index();
                self.state[n] |= QUEUED | 1 << entry;
            }
        }
        let mut bits = vec![0u64; cells.div_ceil(64)];
        for &e in &self.queue {
            let (x, y, z) = unpack(e);
            let i = local(x, y, z);
            bits[i >> 6] |= 1 << (i & 63);
            self.state[i] = 0;
        }
        for &i in &self.outside {
            self.state[i as usize] = 0;
        }
        VisibleSet {
            min,
            size: size.map(|s| s as u64),
            bits,
            count: self.queue.len(),
        }
    }
}

/// The box of `view`: min corner and size.
fn view_box(view: &VisibilityView) -> (ChunkPos, [i64; 3]) {
    let half = view.half();
    let c = view.center;
    (
        ChunkPos::new(c.x - half[0], c.y - half[1], c.z - half[2]),
        half.map(|h| 2 * h + 1),
    )
}

/// One search from scratch: the chunks `view` may see, with the links of each chunk from
/// `links` (tests, tools; the world keeps a [`ChunkVisibility`] instead).
pub fn visible_chunks(view: &VisibilityView, links: impl Fn(ChunkPos) -> u16) -> VisibleSet {
    let mut grid = LinkGrid::default();
    let (min, size) = view_box(view);
    grid.hold(min, size, &links);
    Search::default().run(&grid, view)
}

/// Cave culling state of the world (a resource of [`WorldPlugin`](crate::WorldPlugin)): the
/// links around the last camera, patched every frame from the map's changes, and the last
/// visible set. The render plugin calls [`ChunkVisibility::update`] with its camera once per
/// frame when [`WorldSettings::cave_culling`](crate::WorldSettings) is on.
#[derive(boxel_ecs::prelude::Resource, Debug, Default)]
pub struct ChunkVisibility {
    grid: LinkGrid,
    search: Search,
    /// The view of the last search and its result.
    last: Option<(VisibilityView, Arc<VisibleSet>)>,
    /// The grid changed since the last search.
    dirty: bool,
    stats: VisibilityStats,
}

impl ChunkVisibility {
    /// Applies the map's changes of [`ChunkSlot::visibility_links`](crate::ChunkSlot) (in
    /// order) to the grid.
    pub(crate) fn apply_changes(&mut self, changes: &[(ChunkPos, u16)]) {
        for &(pos, links) in changes {
            self.dirty |= self.grid.set(pos, links);
        }
    }

    /// The chunks `view` may see, searching again only if the view or a link of its box
    /// changed since the last call. `links` gives the links of a position the grid does not
    /// hold yet (the map's [`ChunkMap::visibility_links_at`](crate::ChunkMap)).
    pub fn update(
        &mut self,
        view: &VisibilityView,
        links: impl Fn(ChunkPos) -> u16,
    ) -> Arc<VisibleSet> {
        let start = Instant::now();
        let (min, size) = view_box(view);
        let refilled = self.grid.hold(min, size, &links);
        if refilled == 0
            && !self.dirty
            && let Some((last, set)) = &self.last
            && last == view
        {
            self.stats.reused += 1;
            return Arc::clone(set);
        }
        let set = Arc::new(self.search.run(&self.grid, view));
        self.dirty = false;
        self.last = Some((*view, Arc::clone(&set)));
        self.stats.searches += 1;
        self.stats.last_search = start.elapsed();
        self.stats.reached = set.visible_count();
        self.stats.box_chunks = set.box_len();
        self.stats.refilled = refilled;
        set
    }

    /// The result of the last search, if any.
    pub fn last(&self) -> Option<&Arc<VisibleSet>> {
        self.last.as_ref().map(|(_, set)| set)
    }

    /// Search counters.
    pub fn stats(&self) -> VisibilityStats {
        self.stats
    }
}

#[cfg(test)]
mod tests;
