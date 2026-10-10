//! Exact voxel raycast: Amanatides-Woo DDA from a [`WorldPos`], stepping blocks in `i64`.
//!
//! Each axis keeps the integer offset of its next block plane from the origin's block, and every
//! step computes the distance to it fresh as `(plane - frac) * (1 / dir)` instead of adding a
//! step length to a running total. Nothing accumulates, so a hit does not depend on how far the
//! ray traveled or on where in the world it started: the same ray shifted by any integer vector
//! visits the same blocks at bit-identical `t`.
//!
//! Ties (a ray exactly through an edge or a corner) step x, then y, then z. Of the blocks the ray
//! only touches along that edge or at that corner, it visits the ones on that staircase and skips
//! the rest: one of the two blocks beside an edge, two of the six around a corner.
//!
//! A collision box may reach into the cell above its block (a fence collides 1.5 blocks tall,
//! ADR 0026). A collision ray then also tests, in each cell it crosses low enough, the tall
//! block under that cell, and keeps the nearest hit until no later cell can be nearer. It skips
//! that test while the block under the cell is in a loaded chunk without tall blocks
//! ([`VoxelSource::tall_free`]): the result is the same, and the cost is paid only near them.

use super::shape::LocalBox;
use super::source::{CollisionShape, CollisionTable, Probe, VoxelSource};
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, BlockState, CHUNK_SHIFT, ChunkPos, Face, MAX_BLOCK_COORD, WorldPos};

/// The farthest [`raycast_voxels`] follows a ray on any axis, in blocks: a larger `max_t` is
/// shortened to `MAX_RAY_DISTANCE / max(|dir.x|, |dir.y|, |dir.z|)`. A guard against runaway
/// rays (an infinite `max_t` through the air above the world would walk on for `2^61` blocks):
/// the cost is one probe per block crossed, so the longest ray costs some tens of microseconds.
/// 4096 blocks is 128 chunks, more than any view distance.
pub const MAX_RAY_DISTANCE: f64 = 4096.0;

/// Which blocks stop a ray, and where: a block made of boxes (a slab, a plant's outline) stops
/// it only if the ray meets one of the boxes (ADR 0018).
pub enum VoxelFilter<'a> {
    /// Blocks that collide, at their collision shape: line of sight, projectiles.
    Collision,
    /// Blocks with geometry (render shape other than `None`), at their outline
    /// ([`CollisionTable::outline`]): block picking. Fluids stop the ray only when `fluids` is
    /// true.
    Outline {
        /// Water and other fluids stop the ray too.
        fluids: bool,
    },
    /// Any test on the block's position and state (the whole cell stops the ray).
    Custom(&'a mut dyn FnMut(BlockPos, BlockState) -> bool),
}

impl VoxelFilter<'_> {
    /// The ray stops at block `pos` holding `state`.
    #[inline]
    pub fn accepts(&mut self, table: &CollisionTable, pos: BlockPos, state: BlockState) -> bool {
        match self {
            VoxelFilter::Collision => table.collides(state),
            VoxelFilter::Outline { fluids } => {
                table.has_outline(state) && (*fluids || !table.is_fluid(state))
            }
            VoxelFilter::Custom(test) => test(pos, state),
        }
    }
}

impl VoxelFilter<'_> {
    /// Where in an accepted block the ray stops.
    #[inline]
    fn shape<'t>(&self, table: &'t CollisionTable, state: BlockState) -> CollisionShape<'t> {
        match self {
            VoxelFilter::Collision => table.shape(state),
            VoxelFilter::Outline { .. } => table.outline(state),
            VoxelFilter::Custom(_) => CollisionShape::Full,
        }
    }
}

impl std::fmt::Debug for VoxelFilter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VoxelFilter::Collision => f.write_str("Collision"),
            VoxelFilter::Outline { fluids } => {
                f.debug_struct("Outline").field("fluids", fluids).finish()
            }
            VoxelFilter::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

/// A block a ray stopped at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoxelHit {
    /// The block.
    pub block: BlockPos,
    /// The face the ray entered through (stepping +X enters the block's -X face; for a block
    /// made of boxes, the face of the box it met: the top of a bottom slab is `PosY`). `None`
    /// when the ray started inside the block (or inside one of its boxes, `t = 0`).
    pub face: Option<Face>,
    /// Ray parameter where the ray entered the block (`origin + dir * t`; with a unit `dir`, the
    /// distance in blocks).
    pub t: f64,
    /// Where the ray entered the block (or its box), exactly on the entered face's plane.
    pub point: WorldPos,
    /// The block's state.
    pub state: BlockState,
}

impl VoxelHit {
    /// The block in front of the entered face: where a placed block goes. `None` when the ray
    /// started inside the block.
    pub fn adjacent(&self) -> Option<BlockPos> {
        let n = self.face?.normal();
        Some(BlockPos::new(
            self.block.x + i64::from(n.x),
            self.block.y + i64::from(n.y),
            self.block.z + i64::from(n.z),
        ))
    }
}

/// The result of a voxel raycast.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RayResult {
    /// The ray stopped at a block.
    Hit(VoxelHit),
    /// Nothing within `max_t`.
    Miss,
    /// The ray reached a block of an unloaded chunk at `t` before any hit: what lies beyond is
    /// unknown (a caller can tell this apart from a miss and retry later).
    Unloaded {
        /// The first unloaded block on the ray.
        block: BlockPos,
        /// Where the ray entered it; for a collision ray in a table with tall blocks, where it
        /// came within their reach of it (up to half a block above it, ADR 0026).
        t: f64,
    },
}

impl RayResult {
    /// The hit, if any.
    pub fn hit(&self) -> Option<&VoxelHit> {
        match self {
            RayResult::Hit(hit) => Some(hit),
            RayResult::Miss | RayResult::Unloaded { .. } => None,
        }
    }
}

/// Casts the ray `origin + dir * t`, `t` in `0..=max_t`, through the voxels of `src` and returns
/// the first block `filter` accepts. `dir` need not be a unit vector (`t` is in multiples of
/// it); a zero component never steps that axis, and neither does one so small that its
/// reciprocal overflows. A `dir` with a NaN or infinite component has no direction: only the
/// origin block is tested, as with a zero `dir`. A ray that starts inside an accepted block hits
/// it at `t = 0` with no face.
///
/// The ray ends at `max_t` (NaN counts as 0), at [`MAX_RAY_DISTANCE`] blocks on its longest
/// axis, or at the world limit, whichever comes first.
///
/// Cost: one probe per block crossed (about `max_t * (|dir.x| + |dir.y| + |dir.z|)` of them),
/// with one chunk lookup per chunk crossed when `src` caches its last chunk
/// ([`crate::ChunkSource`] does).
pub fn raycast_voxels<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    origin: WorldPos,
    dir: DVec3,
    max_t: f64,
    mut filter: VoxelFilter<'_>,
) -> RayResult {
    // A hand-built origin may be outside the canonical form (NaN frac would never step).
    let origin = origin.normalize();
    // An infinite component would make every step distance 0 (the ray would never advance).
    let dir = if dir.is_finite() { dir } else { DVec3::ZERO };
    // Every ray is finite: at most MAX_RAY_DISTANCE blocks on its longest axis (a zero `dir`
    // divides to infinity and leaves `max_t` as it is).
    let max_t = if max_t.is_nan() {
        0.0
    } else {
        max_t.min(MAX_RAY_DISTANCE / dir.abs().max_element())
    };
    let dda = Dda::new(origin, dir);
    // Collision rays also meet the boxes that tall blocks raise into the cell above them.
    if matches!(filter, VoxelFilter::Collision) && table.reach() > 0.0 {
        return raycast_reaching(src, table, origin, dir, max_t, dda);
    }
    let Dda {
        mut voxel,
        frac,
        step,
        mut plane,
        inv,
        mut next,
    } = dda;

    match src.probe(origin.block) {
        Probe::Unloaded => {
            return RayResult::Unloaded {
                block: origin.block,
                t: 0.0,
            };
        }
        Probe::Block(state) => {
            if filter.accepts(table, origin.block, state) {
                match filter.shape(table, state) {
                    CollisionShape::Boxes(boxes) => {
                        let hit = boxes_hit(boxes, origin, dir, max_t, origin.block, None);
                        if let Some(hit) = hit {
                            return RayResult::Hit(VoxelHit { state, ..hit });
                        }
                    }
                    _ => {
                        return RayResult::Hit(VoxelHit {
                            block: origin.block,
                            face: None,
                            t: 0.0,
                            point: origin,
                            state,
                        });
                    }
                }
            }
        }
    }

    loop {
        // Smallest t; ties step x, then y, then z.
        let a = if next[0] <= next[1] && next[0] <= next[2] {
            0
        } else if next[1] <= next[2] {
            1
        } else {
            2
        };
        let t = next[a];
        // An infinite t ends a ray with no direction (no axis steps).
        if t > max_t || t == f64::INFINITY {
            return RayResult::Miss;
        }
        // The plane crossed now, in world coordinates (the entered face's plane).
        let crossed = origin.block.as_i64vec3()[a] + plane[a];
        voxel[a] += step[a];
        plane[a] += step[a];
        next[a] = (plane[a] as f64 - frac[a]) * inv[a];
        if voxel[a].abs() > LIMIT {
            return RayResult::Miss;
        }
        let block = BlockPos::new(voxel[0], voxel[1], voxel[2]);
        match src.probe(block) {
            Probe::Unloaded => return RayResult::Unloaded { block, t },
            Probe::Block(state) => {
                if filter.accepts(table, block, state) {
                    let face = entered_face(a, step[a]);
                    match filter.shape(table, state) {
                        CollisionShape::Boxes(boxes) => {
                            let hit = boxes_hit(boxes, origin, dir, max_t, block, Some(face));
                            if let Some(hit) = hit {
                                return RayResult::Hit(VoxelHit { state, ..hit });
                            }
                        }
                        _ => {
                            return RayResult::Hit(VoxelHit {
                                block,
                                face: Some(face),
                                t,
                                point: point_on_plane(origin, dir, t, a, crossed),
                                state,
                            });
                        }
                    }
                }
            }
        }
    }
}

/// The first of `boxes` (in the frame of `block`, `0..=1`) the ray meets within `max_t`, as a
/// hit on `block` (its `state` is left as air for the caller): the ray parameter of the box's
/// entry, the face of the box it entered and the point exactly on that face's plane. A ray
/// that starts inside a box hits it at `t = 0` with no face; a ray that starts on the border of
/// a box of a block it is only entering takes `entered` as its face. The frame of the block is
/// relative to the origin's block, so the test is exact at any distance from the world origin.
fn boxes_hit(
    boxes: &[LocalBox],
    origin: WorldPos,
    dir: DVec3,
    max_t: f64,
    block: BlockPos,
    entered: Option<Face>,
) -> Option<VoxelHit> {
    // The ray's origin in the block's frame (the difference is small: at most a ray length).
    let offset = block.as_i64vec3() - origin.block.as_i64vec3();
    let local = origin.frac - DVec3::new(offset.x as f64, offset.y as f64, offset.z as f64);
    let mut best: Option<(f64, Option<(usize, f64)>)> = None;
    for b in boxes {
        let Some(hit) = b.ray(local, dir, max_t) else {
            continue;
        };
        if best.is_some_and(|(t, _)| t <= hit.t) {
            continue;
        }
        // The box face it entered: its axis and its plane in the block's frame.
        let plane = (0..3).find(|&a| hit.normal[a] != 0.0).map(|a| {
            let at = if hit.normal[a] < 0.0 {
                b.min[a]
            } else {
                b.max[a]
            };
            (a, at)
        });
        best = Some((hit.t, plane));
    }
    let (t, plane) = best?;
    let (face, point) = match plane {
        Some((a, at)) => {
            let face = face_of_normal(a, dir[a] > 0.0);
            let mut point = origin.offset(dir * t);
            // On the plane exactly: the block coordinate and the fraction of the box face (the
            // top of a tall box is in the cell above, ADR 0026).
            let base = block.as_i64vec3()[a];
            let whole = at.floor();
            let (coord, frac) = (base + whole as i64, at - whole);
            match a {
                0 => point.block.x = coord,
                1 => point.block.y = coord,
                _ => point.block.z = coord,
            }
            point.frac[a] = frac;
            (Some(face), point)
        }
        // Inside a box from the start: the origin block, or the border of an entered one.
        None => (entered, origin.offset(dir * t)),
    };
    Some(VoxelHit {
        block,
        face,
        t,
        point,
        state: BlockState::AIR,
    })
}

/// Past this the ray left the world (the world's blocks end at `MAX_BLOCK_COORD + 31`).
const LIMIT: i64 = MAX_BLOCK_COORD + 64;

/// The stepping state of a ray (see the module docs): the current block, the step and the next
/// block plane of each axis (an offset from the origin's block) and the ray parameter there.
struct Dda {
    voxel: [i64; 3],
    frac: DVec3,
    step: [i64; 3],
    plane: [i64; 3],
    inv: [f64; 3],
    next: [f64; 3],
}

impl Dda {
    /// At `origin` (canonical), going along `dir` (finite).
    #[inline]
    fn new(origin: WorldPos, dir: DVec3) -> Self {
        let mut dda = Dda {
            voxel: [origin.block.x, origin.block.y, origin.block.z],
            frac: origin.frac,
            step: [0; 3],
            plane: [0; 3],
            inv: [0.0; 3],
            next: [f64::INFINITY; 3],
        };
        for a in 0..3 {
            let recip = 1.0 / dir[a];
            if !recip.is_finite() {
                // Zero, or so small that the reciprocal overflows (a step distance of `0 * inf`
                // would be NaN): this axis never steps.
                continue;
            }
            if dir[a] > 0.0 {
                dda.step[a] = 1;
                dda.plane[a] = 1;
            } else {
                dda.step[a] = -1;
            }
            dda.inv[a] = recip;
            dda.next[a] = (dda.plane[a] as f64 - dda.frac[a]) * recip;
        }
        dda
    }

    /// The axis of the next step: smallest t, ties x, then y, then z.
    #[inline]
    fn axis(&self) -> usize {
        let next = &self.next;
        if next[0] <= next[1] && next[0] <= next[2] {
            0
        } else if next[1] <= next[2] {
            1
        } else {
            2
        }
    }
}

/// [`raycast_voxels`] with [`VoxelFilter::Collision`] in a table with tall blocks (ADR 0026):
/// in every cell it crosses, the ray tests the cell's block and, when it passes within the
/// table's reach above the cell's bottom, the tall block under the cell (whose boxes rise into
/// it). A hit is kept until the next cell starts past it: a later cell can only hold a nearer
/// hit through a tall block's top. With the same blocks and no tall ones in the way, it returns
/// what the plain loop returns.
///
/// The block under the cell is not read while its chunk is loaded and holds no tall block
/// ([`VoxelSource::tall_free`], asked again only when the ray takes it into another chunk):
/// it would read as a block that is not tall, which changes nothing.
#[inline(never)]
fn raycast_reaching<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    origin: WorldPos,
    dir: DVec3,
    max_t: f64,
    mut dda: Dda,
) -> RayResult {
    let reach = table.reach();
    let mut best: Option<VoxelHit> = None;
    // The current cell: where the ray entered it, and through which face (with the plane's axis
    // and world coordinate), `None` in the origin's cell.
    let mut cell = origin.block;
    let mut entry = 0.0;
    let mut entered: Option<(Face, usize, i64)> = None;
    // The block under the cell is in a loaded chunk without tall blocks.
    let under_chunk =
        |cell: BlockPos| ChunkPos::containing(BlockPos::new(cell.x, cell.y - 1, cell.z));
    let mut under_free = src.tall_free(under_chunk(cell), table);
    // The first of the nearest hits.
    let keep = |hit: VoxelHit, best: &mut Option<VoxelHit>| {
        if best.is_none_or(|b| hit.t < b.t) {
            *best = Some(hit);
        }
    };
    loop {
        let a = dda.axis();
        let exit = dda.next[a];
        match src.probe(cell) {
            Probe::Unloaded => {
                return RayResult::Unloaded {
                    block: cell,
                    t: entry,
                };
            }
            Probe::Block(state) if table.collides(state) => match table.shape(state) {
                CollisionShape::Boxes(boxes) => {
                    let face = entered.map(|(face, ..)| face);
                    if let Some(hit) = boxes_hit(boxes, origin, dir, max_t, cell, face) {
                        keep(VoxelHit { state, ..hit }, &mut best);
                    }
                }
                _ => keep(
                    match entered {
                        Some((face, a, crossed)) => VoxelHit {
                            block: cell,
                            face: Some(face),
                            t: entry,
                            point: point_on_plane(origin, dir, entry, a, crossed),
                            state,
                        },
                        None => VoxelHit {
                            block: cell,
                            face: None,
                            t: 0.0,
                            point: origin,
                            state,
                        },
                    },
                    &mut best,
                ),
            },
            Probe::Block(_) => {}
        }
        // Nothing in this cell or later can be nearer than its entry (a full block's face).
        if let Some(hit) = best
            && hit.t <= entry
        {
            return RayResult::Hit(hit);
        }
        // The tall block under the cell, unless its chunk has none.
        if !under_free {
            // The lowest the ray gets in this cell, above the cell's bottom (`entry` and `exit`
            // are its ends; a ray that does not step on Y keeps its height).
            let bottom = (cell.y - origin.block.y) as f64;
            let low = if dir.y < 0.0 { exit.min(max_t) } else { entry };
            let low = dda.frac.y + dir.y * low - bottom;
            // A small margin: a ray that only grazes the top of a tall box still tests it.
            if low <= reach + 1e-9 {
                let under = BlockPos::new(cell.x, cell.y - 1, cell.z);
                match src.probe(under) {
                    Probe::Unloaded => {
                        // Where the ray comes within reach of it: what lies past is unknown,
                        // what the ray met before is not.
                        let t = if dir.y < 0.0 {
                            entry.max((bottom + reach - dda.frac.y) / dir.y)
                        } else {
                            entry
                        };
                        return match best {
                            Some(hit) if hit.t <= t => RayResult::Hit(hit),
                            _ => RayResult::Unloaded { block: under, t },
                        };
                    }
                    Probe::Block(state) if table.is_tall(state) => {
                        if let CollisionShape::Boxes(boxes) = table.shape(state)
                            && let Some(hit) = boxes_hit(boxes, origin, dir, max_t, under, None)
                        {
                            keep(VoxelHit { state, ..hit }, &mut best);
                        }
                    }
                    Probe::Block(_) => {}
                }
            }
        }
        // Every later cell starts at `exit` or further.
        if let Some(hit) = best
            && hit.t <= exit
        {
            return RayResult::Hit(hit);
        }
        if exit > max_t || exit == f64::INFINITY {
            return best.map_or(RayResult::Miss, RayResult::Hit);
        }
        let crossed = origin.block.as_i64vec3()[a] + dda.plane[a];
        // The coordinate on `a` of the block under the cell, before and after the step.
        let under = dda.voxel[a] - i64::from(a == 1);
        dda.voxel[a] += dda.step[a];
        dda.plane[a] += dda.step[a];
        dda.next[a] = (dda.plane[a] as f64 - dda.frac[a]) * dda.inv[a];
        if dda.voxel[a].abs() > LIMIT {
            return best.map_or(RayResult::Miss, RayResult::Hit);
        }
        cell = BlockPos::new(dda.voxel[0], dda.voxel[1], dda.voxel[2]);
        entry = exit;
        entered = Some((entered_face(a, dda.step[a]), a, crossed));
        if under >> CHUNK_SHIFT != (under + dda.step[a]) >> CHUNK_SHIFT {
            under_free = src.tall_free(under_chunk(cell), table);
        }
    }
}

/// The face of a box a ray moving along `axis` (toward `+` if `positive`) enters through.
fn face_of_normal(axis: usize, positive: bool) -> Face {
    entered_face(axis, if positive { 1 } else { -1 })
}

/// The face a step along `axis` in direction `step` enters through.
fn entered_face(axis: usize, step: i64) -> Face {
    match (axis, step > 0) {
        (0, true) => Face::NegX,
        (0, false) => Face::PosX,
        (1, true) => Face::NegY,
        (1, false) => Face::PosY,
        (_, true) => Face::NegZ,
        (_, false) => Face::PosZ,
    }
}

/// `origin + dir * t`, with the `axis` coordinate exactly on the integer plane `crossed`.
fn point_on_plane(origin: WorldPos, dir: DVec3, t: f64, axis: usize, crossed: i64) -> WorldPos {
    let mut point = origin.offset(dir * t);
    match axis {
        0 => point.block.x = crossed,
        1 => point.block.y = crossed,
        _ => point.block.z = crossed,
    }
    point.frac[axis] = 0.0;
    point
}
