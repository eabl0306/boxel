//! [`PhysicsQuery`]: physics queries from any system.

use crate::contacts::{BodyFilter, BodyHit, BodyIndex};
use crate::core::{
    CollisionTable, LocalBox, Probe, RayResult, VoxelFilter, VoxelHit, VoxelSource, raycast_voxels,
};
use crate::plugin::PhysicsContent;
use crate::source::ChunkSource;
use boxel_ecs::prelude::*;
use boxel_ecs::system::SystemParam;
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, BlockState, WorldPos};
use boxel_world::{ChunkMap, WorldSettings};

/// Physics queries against the world, for any system: voxel raycasts (picking, line of sight),
/// rays that stop at blocks or bodies, box overlaps and the block placement check. Reads
/// `ChunkMap`, `WorldSettings`, [`PhysicsContent`] and [`BodyIndex`] (so it needs
/// [`crate::PhysicsPlugin`] and `WorldPlugin`).
///
/// Systems in `Update` see the world as `WorldSystems::Edits` left it in the previous frame, or
/// in this frame when they run after it, and the bodies as the latest fixed step left them
/// ([`BodyIndex`]). Queries never change anything.
#[derive(SystemParam)]
pub struct PhysicsQuery<'w> {
    map: Res<'w, ChunkMap>,
    settings: Res<'w, WorldSettings>,
    content: Res<'w, PhysicsContent>,
    index: Res<'w, BodyIndex>,
}

/// What stops a [`PhysicsQuery::raycast`].
#[derive(Debug)]
pub struct RayFilter<'a> {
    /// The blocks that stop the ray; `None` lets it through every block (default
    /// `Some(VoxelFilter::Collision)`).
    pub voxels: Option<VoxelFilter<'a>>,
    /// The bodies that stop the ray (default: solid bodies of every layer).
    pub bodies: BodyFilter<'a>,
}

impl Default for RayFilter<'_> {
    fn default() -> Self {
        Self {
            voxels: Some(VoxelFilter::Collision),
            bodies: BodyFilter::default(),
        }
    }
}

impl<'a> RayFilter<'a> {
    /// Stopped by the blocks `voxels` accepts and the bodies `bodies` accepts.
    pub fn new(voxels: VoxelFilter<'a>, bodies: BodyFilter<'a>) -> Self {
        Self {
            voxels: Some(voxels),
            bodies,
        }
    }

    /// Bodies only: the ray goes through blocks.
    pub fn bodies(bodies: BodyFilter<'a>) -> Self {
        Self {
            voxels: None,
            bodies,
        }
    }
}

/// What a [`PhysicsQuery::raycast`] hit first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RayHit {
    /// A block.
    Block(VoxelHit),
    /// A body.
    Body(BodyHit),
}

impl RayHit {
    /// Ray parameter of the hit.
    pub fn t(&self) -> f64 {
        match self {
            RayHit::Block(hit) => hit.t,
            RayHit::Body(hit) => hit.t,
        }
    }

    /// Where the ray hit.
    pub fn point(&self) -> WorldPos {
        match self {
            RayHit::Block(hit) => hit.point,
            RayHit::Body(hit) => hit.point,
        }
    }

    /// The body hit, if it was one.
    pub fn entity(&self) -> Option<Entity> {
        match self {
            RayHit::Block(_) => None,
            RayHit::Body(hit) => Some(hit.entity),
        }
    }

    /// The block hit, if it was one.
    pub fn block(&self) -> Option<&VoxelHit> {
        match self {
            RayHit::Block(hit) => Some(hit),
            RayHit::Body(_) => None,
        }
    }
}

impl PhysicsQuery<'_> {
    /// A [`VoxelSource`] over the world's chunks (for [`crate::move_box`] and the other core
    /// functions).
    pub fn voxels(&self) -> ChunkSource<'_> {
        ChunkSource::new(&self.map, &self.settings)
    }

    /// How each block state collides.
    pub fn collision(&self) -> &CollisionTable {
        &self.content.collision
    }

    /// The bodies as the latest fixed step left them.
    pub fn bodies(&self) -> &BodyIndex {
        &self.index
    }

    /// The block at `p`, or [`Probe::Unloaded`].
    pub fn probe(&self, p: BlockPos) -> Probe {
        self.voxels().probe(p)
    }

    /// Casts the ray `origin + dir * t`, `t` in `0..=max_t`, and returns the first block
    /// `filter` accepts (see [`raycast_voxels`]). For block picking from a camera:
    /// `raycast_voxels(WorldPos::from_dvec3(transform.translation), forward, reach,
    /// VoxelFilter::Outline { fluids: false })`.
    pub fn raycast_voxels(
        &self,
        origin: WorldPos,
        dir: DVec3,
        max_t: f64,
        filter: VoxelFilter<'_>,
    ) -> RayResult {
        raycast_voxels(
            &mut self.voxels(),
            &self.content.collision,
            origin,
            dir,
            max_t,
            filter,
        )
    }

    /// Casts the ray `origin + dir * t`, `t` in `0..=max_t`, through blocks and bodies: the
    /// voxel raycast first, then the bodies up to the block it hit ([`BodyIndex::raycast`]). A
    /// body at the same `t` as the block wins (it is in front of the block's face). A chunk
    /// that is not loaded ends the ray like a wall that reports nothing (use
    /// [`PhysicsQuery::raycast_voxels`] to tell it apart). A ray that starts inside a body hits
    /// it at `t = 0`: exclude the caster's own body.
    pub fn raycast(
        &self,
        origin: WorldPos,
        dir: DVec3,
        max_t: f64,
        filter: RayFilter<'_>,
    ) -> Option<RayHit> {
        let mut limit = max_t;
        let mut block = None;
        if let Some(voxels) = filter.voxels {
            match self.raycast_voxels(origin, dir, max_t, voxels) {
                RayResult::Hit(hit) => {
                    limit = hit.t;
                    block = Some(hit);
                }
                RayResult::Unloaded { t, .. } => limit = t,
                RayResult::Miss => {}
            }
        }
        match self.index.raycast(origin, dir, limit, &filter.bodies) {
            Some(body) => Some(RayHit::Body(body)),
            None => block.map(RayHit::Block),
        }
    }

    /// The bodies `filter` accepts that overlap the box `bx` (relative to `pos`), sorted by
    /// entity (see [`BodyIndex::overlap_box`]).
    pub fn overlap_box(&self, pos: WorldPos, bx: LocalBox, filter: &BodyFilter<'_>) -> Vec<Entity> {
        self.index.overlap_box(pos, bx, filter)
    }

    /// The placement check: no solid body overlaps block `block` (see
    /// [`BodyIndex::block_is_free`]). Check the block itself (air, a fluid) with
    /// [`PhysicsQuery::probe`].
    pub fn block_is_free(&self, block: BlockPos) -> bool {
        self.index.block_is_free(block)
    }

    /// The placement check of a block state: no solid body overlaps the collision shape `state`
    /// would have at `block` (a slab only checks its half; a plant is always free; see
    /// [`BodyIndex::shape_is_free`]).
    pub fn state_is_free(&self, block: BlockPos, state: BlockState) -> bool {
        self.index
            .shape_is_free(block, self.collision().shape(state))
    }
}
