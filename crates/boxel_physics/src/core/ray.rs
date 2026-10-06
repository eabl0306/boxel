//! Exact voxel raycast: Amanatides-Woo DDA from a [`WorldPos`], stepping blocks in `i64`.
//!
//! Each axis keeps the integer offset of its next block plane from the origin's block, and every
//! step computes the distance to it fresh as `(plane - frac) * (1 / dir)` instead of adding a
//! step length to a running total. Nothing accumulates, so a hit does not depend on how far the
//! ray traveled or on where in the world it started: the same ray shifted by any integer vector
//! visits the same blocks at bit-identical `t`.
//!
//! Ties (a ray exactly through an edge or a corner) step x, then y, then z: every block the ray
//! touches is visited, the edge-adjacent ones included.

use super::source::{CollisionTable, Probe, VoxelSource};
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, BlockState, Face, MAX_BLOCK_COORD, WorldPos};

/// Which blocks stop a ray.
pub enum VoxelFilter<'a> {
    /// Blocks that collide (full blocks): line of sight, projectiles.
    Collision,
    /// Blocks with geometry (render shape other than `None`): block picking. Fluids stop the
    /// ray only when `fluids` is true.
    Outline {
        /// Water and other fluids stop the ray too.
        fluids: bool,
    },
    /// Any test on the block's position and state.
    Custom(&'a mut dyn FnMut(BlockPos, BlockState) -> bool),
}

impl VoxelFilter<'_> {
    /// The ray stops at block `pos` holding `state`.
    #[inline]
    pub fn accepts(&mut self, table: &CollisionTable, pos: BlockPos, state: BlockState) -> bool {
        match self {
            VoxelFilter::Collision => table.is_full(state),
            VoxelFilter::Outline { fluids } => {
                table.has_outline(state) && (*fluids || !table.is_fluid(state))
            }
            VoxelFilter::Custom(test) => test(pos, state),
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
    /// The face the ray entered through (stepping +X enters the block's -X face). `None` when
    /// the ray started inside the block (`t = 0`).
    pub face: Option<Face>,
    /// Ray parameter where the ray entered the block (`origin + dir * t`; with a unit `dir`, the
    /// distance in blocks).
    pub t: f64,
    /// Where the ray entered the block, exactly on the entered face's plane.
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
        /// Where the ray entered it.
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
/// it); a zero component never steps that axis. A ray that starts inside an accepted block hits
/// it at `t = 0` with no face. `max_t` should be finite; the ray also stops at the world limit.
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
    let max_t = if max_t.is_nan() { 0.0 } else { max_t };
    let mut voxel = [origin.block.x, origin.block.y, origin.block.z];
    let frac = origin.frac;
    let mut step = [0i64; 3];
    // Offset of each axis' next block plane from the origin's block.
    let mut plane = [0i64; 3];
    let mut inv = [0.0f64; 3];
    let mut next = [f64::INFINITY; 3];
    for a in 0..3 {
        if dir[a] > 0.0 {
            step[a] = 1;
            plane[a] = 1;
        } else if dir[a] < 0.0 {
            step[a] = -1;
        } else {
            // Zero or NaN: this axis never steps.
            continue;
        }
        inv[a] = 1.0 / dir[a];
        next[a] = (plane[a] as f64 - frac[a]) * inv[a];
    }

    match src.probe(origin.block) {
        Probe::Unloaded => {
            return RayResult::Unloaded {
                block: origin.block,
                t: 0.0,
            };
        }
        Probe::Block(state) => {
            if filter.accepts(table, origin.block, state) {
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

    // Past this the ray left the world (a few chunks of margin around the limit).
    const LIMIT: i64 = MAX_BLOCK_COORD + 64;
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
        // An infinite t also ends a ray with no direction.
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
