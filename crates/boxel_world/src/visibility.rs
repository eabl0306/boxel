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
//! - A chunk the renderer does not show as its blocks are is seen through ([`XRAY`]), with every
//!   chunk past it: one whose mesh is not published yet (or not wanted), is being redone after
//!   an edit that changed what occludes in it or on its border, or whose neighbor unloaded
//!   ([`ChunkSlot::shown`](crate::ChunkSlot::shown)), and one whose last mesh the renderer
//!   refused ([`ChunkVisibility::set_unshown`]). A chunk not loaded or not generated yet counts
//!   as open.
//!
//! The search covers a box around the camera's chunk (the viewer's mesh radius); a chunk
//! outside it is always visible ([`VisibleSet::is_visible`]). A path cannot leave an
//! axis-aligned box and come back without walking back along an axis, so the box loses nothing.
//!
//! [`ChunkVisibility`] keeps the links of the box in a grid that follows the camera (only the
//! slabs that enter it are read from the map when the camera changes chunk) and is patched by
//! the map's changes every frame ([`WorldSystems::Visibility`](crate::WorldSystems)). The
//! renderer asks for the set of its own camera in its extract step, every frame, so a chunk that
//! just became visible is never drawn late. A set stays valid, and is reused, while nothing can
//! make more chunks visible:
//!
//! - The search tests the frustum with [`MOVE_SLACK`] voxels to spare, so the set holds for any
//!   eye within that distance in the same chunk, looking the same way: a camera that flies
//!   straight searches again every few voxels, not every frame. Any turn searches again.
//! - A link that opens (an edit, a chunk unloaded or remeshed after an edit) searches again at
//!   once. A link that closes (a chunk whose mesh just settled, which happens every frame while
//!   the world streams) can only hide more: it waits up to [`TIGHTEN_DELAY`] for the next search.
use boxel_math::{Frustum, I64Vec3, Vec3};
use boxel_voxel::{ALL_LINKS, BlockPos, CHUNK_SHIFT, ChunkFaceInfo, ChunkPos, Face};
use std::collections::HashSet;
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
    /// Frames culled nothing because the eye did not see from open space
    /// ([`ChunkVisibility::eye_in_open`]).
    pub blocked: u64,
}

/// Links bit (above the 15 links): the renderer does not show the chunk as its blocks are (no
/// mesh yet, a mesh built from other blocks, [`ChunkSlot::shown`](crate::ChunkSlot::shown), or
/// a mesh it refused, [`ChunkVisibility::set_unshown`]). Where the faces of its occluding cubes
/// are missing, a line of sight goes into the rock around it, where nothing is meshed, and on
/// until an open voxel shows a face: the search sees every chunk past such a chunk, as if all
/// were open. Such a chunk counts as open itself.
pub const XRAY: u16 = 1 << 15;

/// Bits of the search state of a cell.
const INCOMING: u8 = 0x3f;
const QUEUED: u8 = 1 << 6;
const OUTSIDE: u8 = 1 << 7;

/// The links of the box around the camera, in box order (the order of the search's state and of
/// [`VisibleSet`]). Moving the box shifts the cells it keeps and reads only the slabs that
/// enter it.
#[derive(Debug, Default)]
struct LinkGrid {
    links: Vec<u16>,
    /// Scratch for a move.
    spare: Vec<u16>,
    /// The box the cells hold: min corner and size. `None`: nothing valid.
    held: Option<(ChunkPos, [i64; 3])>,
}

impl LinkGrid {
    /// Makes the grid hold box `(min, size)`: positions that were not in the held box are read
    /// with `links`. Returns how many were read.
    fn hold(&mut self, min: ChunkPos, size: [i64; 3], links: &impl Fn(ChunkPos) -> u16) -> usize {
        let old = match self.held {
            Some((old_min, old_size)) if old_size == size => {
                if old_min == min {
                    return 0;
                }
                Some(old_min)
            }
            _ => None,
        };
        let [sx, sy, sz] = size;
        let cells = (sx * sy * sz) as usize;
        self.spare.clear();
        self.spare.resize(cells, ALL_LINKS);
        // New cell (x, y, z) is old cell (x + dx, y + dy, z + dz).
        let d = old.map(|o| [min.x - o.x, min.y - o.y, min.z - o.z]);
        let mut read = 0;
        let mut i = 0;
        for y in 0..sy {
            for z in 0..sz {
                let row = i;
                i += sx as usize;
                // The x range of this row the old box held, as new x coordinates.
                let kept = d.and_then(|[dx, dy, dz]| {
                    let (oy, oz) = (y + dy, z + dz);
                    let (x0, x1) = ((-dx).max(0), (sx - dx).min(sx));
                    ((0..sy).contains(&oy) && (0..sz).contains(&oz) && x0 < x1).then(|| {
                        let from = ((oy * sz + oz) * sx + x0 + dx) as usize;
                        (x0, x1, from)
                    })
                });
                let (x0, x1) = match kept {
                    Some((x0, x1, from)) => {
                        let len = (x1 - x0) as usize;
                        let to = row + x0 as usize;
                        self.spare[to..to + len].copy_from_slice(&self.links[from..from + len]);
                        (x0, x1)
                    }
                    None => (0, 0),
                };
                for x in (0..x0).chain(x1..sx) {
                    let pos = ChunkPos::new(min.x + x, min.y + y, min.z + z);
                    self.spare[row + x as usize] = links(pos);
                    read += 1;
                }
            }
        }
        std::mem::swap(&mut self.links, &mut self.spare);
        self.held = Some((min, size));
        read
    }

    /// Sets the links of `pos` if the grid holds it. Returns the links it had (`None`: not
    /// held).
    fn set(&mut self, pos: ChunkPos, links: u16) -> Option<u16> {
        let (min, [sx, sy, sz]) = self.held?;
        let (x, y, z) = (
            pos.x.wrapping_sub(min.x) as u64,
            pos.y.wrapping_sub(min.y) as u64,
            pos.z.wrapping_sub(min.z) as u64,
        );
        if x >= sx as u64 || y >= sy as u64 || z >= sz as u64 {
            return None;
        }
        let i = ((y * sz as u64 + z) * sx as u64 + x) as usize;
        Some(std::mem::replace(&mut self.links[i], links))
    }
}

/// `LINKED_LO[f][b]` and `LINKED_HI[f][b]`: the faces linked to face `f` by the low 8 and the
/// high 7 bits of a link set ([`ChunkFaceInfo::linked_faces`] in two lookups).
const LINKED_LO: [[u8; 256]; 6] = linked_table::<256>(0);
/// See [`LINKED_LO`].
const LINKED_HI: [[u8; 128]; 6] = linked_table::<128>(8);

const fn linked_table<const N: usize>(shift: u32) -> [[u8; N]; 6] {
    let mut out = [[0u8; N]; 6];
    let mut f = 0;
    while f < 6 {
        let Some(face) = Face::from_index(f as u8) else {
            panic!("a face index")
        };
        let mut b = 0;
        while b < N {
            out[f][b] = ChunkFaceInfo::linked_faces((b as u16) << shift, face);
            b += 1;
        }
        f += 1;
    }
    out
}

/// A plane of the frustum, prepared for the chunks of the box: chunk `l` (box-local) touches
/// the inside of the plane when `c + dot(n, l) >= 0`.
#[derive(Clone, Copy, Debug)]
struct ChunkPlane {
    n: Vec3,
    c: f32,
}

/// Slack of the search's frustum test, in voxels, against rounding: it must never reject a chunk
/// the renderer's test (the same planes, summed in another order) accepts.
const FRUSTUM_SLACK: f32 = 0.5;

/// How far (voxels) every plane of the searched frustum is pushed out, so the set holds while
/// the eye moves that much without turning (see [`ChunkVisibility::update`]).
pub const MOVE_SLACK: f32 = 4.0;

/// How long a link that closed (it can only hide more) may wait for the next search.
pub const TIGHTEN_DELAY: Duration = Duration::from_millis(50);

/// Slack of the searches of [`visible_chunks`] and [`ChunkVisibility`]: [`MOVE_SLACK`] plus
/// [`FRUSTUM_SLACK`] against rounding.
const SEARCH_SLACK: f32 = MOVE_SLACK + FRUSTUM_SLACK;

/// The frustum of `view` against the chunks of the box at `min`, or `None` without a frustum,
/// with every plane pushed out by `slack` voxels.
fn chunk_planes(view: &VisibilityView, min: ChunkPos, slack: f32) -> Option<[ChunkPlane; 6]> {
    // The chunk AABBs `[base + 32 l, + 32]` relative to the render origin (in i64 first, as the
    // renderer culls them): the p-vertex of plane n is base + 32 (l + s) with s = 1 where
    // n >= 0, so the test is linear in l.
    let base = (min.as_i64vec3() << CHUNK_SHIFT)
        .saturating_sub(view.render_origin)
        .as_vec3();
    view.frustum.map(|f| {
        f.planes.map(|p| {
            let n = p.truncate();
            let s = Vec3::select(n.cmpge(Vec3::ZERO), Vec3::splat(32.0), Vec3::ZERO);
            ChunkPlane {
                n: n * 32.0,
                c: n.dot(base + s) + p.w + slack,
            }
        })
    })
}

/// True if box-local chunk `l` touches the inside of every plane.
#[inline]
fn touches(planes: &[ChunkPlane; 6], l: Vec3) -> bool {
    planes.iter().all(|p| p.c + p.n.dot(l) >= 0.0)
}

/// The breadth-first search over a [`LinkGrid`] (scratch kept between runs).
#[derive(Debug, Default)]
struct Search {
    /// Per box cell: incoming faces, queued, outside the frustum (all zero between runs).
    state: Vec<u8>,
    /// One bit per box cell: entered from a chunk seen through ([`XRAY`]), so it is seen
    /// through too (all zero between runs).
    xray: Vec<u64>,
    queue: Vec<u32>,
    outside: Vec<u32>,
}

impl Search {
    /// Searches the box the grid holds (centered on `view.center`) with the frustum pushed out
    /// by `slack` voxels, and returns the visible set.
    fn run(&mut self, grid: &LinkGrid, view: &VisibilityView, slack: f32) -> VisibleSet {
        let Some((min, size)) = grid.held else {
            unreachable!("the grid holds the view's box");
        };
        let [sx, sy, sz] = size.map(|s| s as usize);
        let cells = sx * sy * sz;
        if self.state.len() < cells {
            self.state.resize(cells, 0);
            self.xray.resize(cells.div_ceil(64), 0);
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
        let planes = chunk_planes(view, min, slack);
        let start = local(center[0], center[1], center[2]);
        self.queue.clear();
        self.outside.clear();
        self.queue.push(pack(center[0], center[1], center[2]));
        self.state[start] = QUEUED;
        // Some chunk was seen through ([`XRAY`]): the bits of `xray` are in use.
        let mut xray_any = false;
        let mut head = 0;
        while head < self.queue.len() {
            let (x, y, z) = unpack(self.queue[head]);
            head += 1;
            let i = local(x, y, z);
            let links = grid.links[i];
            let xray = links & XRAY != 0 || (xray_any && self.xray[i >> 6] >> (i & 63) & 1 != 0);
            xray_any |= xray;
            let out = if i == start {
                0x3f
            } else {
                // Never back toward the camera along an axis.
                let side = |p: usize, c: usize, plus: usize, minus: usize| -> u8 {
                    match p.cmp(&c) {
                        std::cmp::Ordering::Greater => 1 << plus,
                        std::cmp::Ordering::Less => 1 << minus,
                        std::cmp::Ordering::Equal => 1 << plus | 1 << minus,
                    }
                };
                let away =
                    side(y, center[1], 0, 1) | side(x, center[0], 2, 3) | side(z, center[2], 4, 5);
                if xray {
                    away
                } else {
                    let (lo, hi) = (usize::from(links as u8), usize::from(links >> 8 & 0x7f));
                    let mut incoming = self.state[i] & INCOMING;
                    let mut out = 0;
                    while incoming != 0 {
                        let f = incoming.trailing_zeros() as usize;
                        incoming &= incoming - 1;
                        out |= LINKED_LO[f][lo] | LINKED_HI[f][hi];
                    }
                    out & away
                }
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
                    if !near && let Some(planes) = &planes {
                        let l = Vec3::new(nx as f32, ny as f32, nz as f32);
                        if !touches(planes, l) {
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
                if xray {
                    self.xray[n >> 6] |= 1 << (n & 63);
                }
            }
        }
        let mut bits = vec![0u64; cells.div_ceil(64)];
        for &e in &self.queue {
            let (x, y, z) = unpack(e);
            let i = local(x, y, z);
            bits[i >> 6] |= 1 << (i & 63);
            self.state[i] = 0;
            if xray_any {
                // Only queued cells have the bit.
                self.xray[i >> 6] = 0;
            }
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

/// Largest near-plane reach (voxels) [`eye_in_open`] looks around: past it, it says no.
pub const MAX_EYE_REACH: f32 = 4.0;

/// Slack added to the reach of [`eye_in_open`], against rounding.
const EYE_SLACK: f32 = 1.0 / 64.0;

/// True if the eye at `render_origin + eye_rel` sees from open space: every voxel within `reach`
/// of it (the camera's near-plane reach, `boxel_camera::Camera::near_reach`) is known and does
/// not occlude (`occludes` gives `None` for an unknown voxel). The search is only sound then, so
/// the renderer culls nothing on a frame where this is false.
///
/// The search follows lines of sight that start in an open voxel and walk open voxels. The
/// renderer draws nothing nearer than the near plane, so a pixel's view ray really starts on it,
/// up to `reach` from the eye. Inside an occluding cube, that ray crosses the rock unseen (faces
/// between two occluding cubes are not meshed, and the near wall of a cave faces away) and shows
/// the far walls of caves the search does not reach: with a camera that flies through rock, or
/// one less than the near plane from a wall it looks at. Minecraft turns its culling off for a
/// camera inside a solid block for the same reason.
pub fn eye_in_open(
    render_origin: I64Vec3,
    eye_rel: Vec3,
    reach: f32,
    occludes: impl Fn(BlockPos) -> Option<bool>,
) -> bool {
    if reach.is_nan() || reach >= MAX_EYE_REACH {
        return false;
    }
    let r = reach.max(0.0) + EYE_SLACK;
    let lo = (eye_rel - Vec3::splat(r)).floor().as_i64vec3();
    let hi = (eye_rel + Vec3::splat(r)).floor().as_i64vec3();
    for y in lo.y..=hi.y {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                let p = BlockPos::from_i64vec3(render_origin.saturating_add(I64Vec3::new(x, y, z)));
                if occludes(p) != Some(false) {
                    return false;
                }
            }
        }
    }
    true
}

/// One search from scratch with the frustum pushed out by `slack` voxels (tests).
#[cfg(test)]
fn visible_chunks_with_slack(
    view: &VisibilityView,
    links: impl Fn(ChunkPos) -> u16,
    slack: f32,
) -> VisibleSet {
    let mut grid = LinkGrid::default();
    let (min, size) = view_box(view);
    grid.hold(min, size, &links);
    Search::default().run(&grid, view, slack)
}

/// One search from scratch: the chunks `view` may see, with the links of each chunk from
/// `links` (tests, tools; the world keeps a [`ChunkVisibility`] instead). Like the world's, it
/// tests the frustum with [`MOVE_SLACK`] to spare.
pub fn visible_chunks(view: &VisibilityView, links: impl Fn(ChunkPos) -> u16) -> VisibleSet {
    let mut grid = LinkGrid::default();
    let (min, size) = view_box(view);
    grid.hold(min, size, &links);
    Search::default().run(&grid, view, SEARCH_SLACK)
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
    /// A link opened since the last search: the next update searches.
    opened: bool,
    /// When the first link that closed since the last search did.
    closed: Option<Instant>,
    /// Chunks the renderer does not show as the map has them ([`ChunkVisibility::set_unshown`]):
    /// [`XRAY`] in the grid whatever the map says.
    unshown: HashSet<ChunkPos>,
    stats: VisibilityStats,
}

impl ChunkVisibility {
    /// Nothing held, searched or unshown: as a new world has it.
    pub(crate) fn is_clear(&self) -> bool {
        self.last.is_none()
            && self.unshown.is_empty()
            && self.grid.held.is_none()
            && !self.opened
            && self.closed.is_none()
    }

    /// Applies the map's changes of [`ChunkSlot::visibility_links`](crate::ChunkSlot) (in
    /// order) to the grid.
    pub(crate) fn apply_changes(&mut self, changes: &[(ChunkPos, u16)]) {
        self.apply_changes_at(changes, Instant::now());
    }

    /// [`ChunkVisibility::apply_changes`] at time `now`.
    pub(crate) fn apply_changes_at(&mut self, changes: &[(ChunkPos, u16)], now: Instant) {
        for &(pos, links) in changes {
            if self.unshown.is_empty() || !self.unshown.contains(&pos) {
                self.set_links(pos, links, now);
            }
        }
    }

    /// Sets the grid's links of `pos` (if it holds it) and notes whether they opened or closed.
    fn set_links(&mut self, pos: ChunkPos, links: u16, now: Instant) {
        match self.grid.set(pos, links) {
            Some(old) if links & !old != 0 => self.opened = true,
            Some(old) if links != old => {
                self.closed.get_or_insert(now);
            }
            _ => {}
        }
    }

    /// Whether the renderer shows chunk `pos` as the map has it. `unshown`: it refused the
    /// chunk's last mesh (its chunk table or arena is full), so it shows an older mesh or
    /// nothing there, and the rock around may show what lies behind it: the search sees every
    /// chunk past it, at once. Shown again (`false`, with the map's
    /// [`ChunkSlot::visibility_links`](crate::ChunkSlot) of `pos` as `links`): its links count
    /// again, as a link that closed.
    pub fn set_unshown(&mut self, pos: ChunkPos, unshown: bool, links: u16) {
        self.set_unshown_at(pos, unshown, links, Instant::now());
    }

    /// [`ChunkVisibility::set_unshown`] at time `now`.
    pub(crate) fn set_unshown_at(
        &mut self,
        pos: ChunkPos,
        unshown: bool,
        links: u16,
        now: Instant,
    ) {
        if unshown && self.unshown.insert(pos) {
            self.set_links(pos, ALL_LINKS | XRAY, now);
        } else if !unshown && self.unshown.remove(&pos) {
            self.set_links(pos, links, now);
        }
    }

    /// Every chunk is shown again ([`ChunkVisibility::set_unshown`] with `false`): the renderer
    /// dropped every chunk (a new renderer, or one that uploads every mesh again).
    pub fn clear_unshown(&mut self, links: impl Fn(ChunkPos) -> u16) {
        let now = Instant::now();
        for pos in std::mem::take(&mut self.unshown) {
            self.set_links(pos, links(pos), now);
        }
    }

    /// The chunks `view` may see: the last set while it still holds (the same chunk and box,
    /// the eye less than [`MOVE_SLACK`] from where it was searched without turning, no link
    /// opened, no closed link older than [`TIGHTEN_DELAY`]), else a new search. A search that
    /// finds the same chunks returns the same `Arc` (the renderer rebuilds its bits only for a
    /// new one, [`Arc::ptr_eq`]). `links` gives the links of a position the grid does not hold
    /// yet (the map's [`ChunkMap::visibility_links_at`](crate::ChunkMap)).
    pub fn update(
        &mut self,
        view: &VisibilityView,
        links: impl Fn(ChunkPos) -> u16,
    ) -> Arc<VisibleSet> {
        self.update_at(view, links, Instant::now())
    }

    /// [`ChunkVisibility::update`] at time `now`.
    pub(crate) fn update_at(
        &mut self,
        view: &VisibilityView,
        links: impl Fn(ChunkPos) -> u16,
        now: Instant,
    ) -> Arc<VisibleSet> {
        let start = Instant::now();
        let (min, size) = view_box(view);
        let unshown = &self.unshown;
        let refilled = self.grid.hold(min, size, &|p| {
            if !unshown.is_empty() && unshown.contains(&p) {
                ALL_LINKS | XRAY
            } else {
                links(p)
            }
        });
        let tighten = self
            .closed
            .is_some_and(|t| now.saturating_duration_since(t) >= TIGHTEN_DELAY);
        if refilled == 0
            && !self.opened
            && !tighten
            && let Some((last, set)) = &self.last
            && still_holds(last, view)
        {
            self.stats.reused += 1;
            return Arc::clone(set);
        }
        let found = self.search.run(&self.grid, view, SEARCH_SLACK);
        // The same chunks as before (a link that closed out of sight, a turn that changed
        // nothing): the same set, so the renderer keeps its bits.
        let set = match &self.last {
            Some((_, last)) if **last == found => Arc::clone(last),
            _ => Arc::new(found),
        };
        self.opened = false;
        self.closed = None;
        self.last = Some((*view, Arc::clone(&set)));
        self.stats.searches += 1;
        self.stats.last_search = start.elapsed();
        self.stats.reached = set.visible_count();
        self.stats.box_chunks = set.box_len();
        self.stats.refilled = refilled;
        set
    }

    /// [`eye_in_open`] for this frame's eye, counting the frames where it is false
    /// ([`VisibilityStats::blocked`]): the renderer must cull nothing then. The last set stays
    /// valid for later frames whose eye is back in open space (a set depends on the eye's
    /// chunk, not on its voxel).
    pub fn eye_in_open(
        &mut self,
        render_origin: I64Vec3,
        eye_rel: Vec3,
        reach: f32,
        occludes: impl Fn(BlockPos) -> Option<bool>,
    ) -> bool {
        // The renderer may show other blocks than the map's in a chunk it does not show.
        let unshown = &self.unshown;
        let open = eye_in_open(render_origin, eye_rel, reach, |p| {
            if !unshown.is_empty() && unshown.contains(&ChunkPos::containing(p)) {
                None
            } else {
                occludes(p)
            }
        });
        self.stats.blocked += u64::from(!open);
        open
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

/// True if the set searched for `old` holds for `new`: the same chunk and box, and every plane of
/// the new frustum (in the old frame) at most [`MOVE_SLACK`] outside the old one with the same
/// normal, which the search's slack covers.
fn still_holds(old: &VisibilityView, new: &VisibilityView) -> bool {
    if old.center != new.center
        || old.radius != new.radius
        || old.vertical_radius != new.vertical_radius
    {
        return false;
    }
    match (&old.frustum, &new.frustum) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            // The new planes relative to the old render origin (a few voxels apart: exact).
            let shift = (new.render_origin - old.render_origin).as_vec3();
            a.planes.iter().zip(&b.planes).all(|(pa, pb)| {
                let n = pb.truncate();
                n == pa.truncate() && pb.w - n.dot(shift) - pa.w <= MOVE_SLACK
            })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
