//! [`PhysicsQuery`]: physics queries from any system.

use crate::core::{CollisionTable, Probe, RayResult, VoxelFilter, VoxelSource, raycast_voxels};
use crate::plugin::PhysicsContent;
use crate::source::ChunkSource;
use boxel_ecs::prelude::*;
use boxel_ecs::system::SystemParam;
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, WorldPos};
use boxel_world::{ChunkMap, WorldSettings};

/// Physics queries against the world, for any system: voxel raycasts today (picking, line of
/// sight), shape and entity queries in later phases. Reads `ChunkMap`, `WorldSettings` and
/// [`PhysicsContent`] (so it needs [`crate::PhysicsPlugin`] and `WorldPlugin`).
///
/// Systems in `Update` see the world as `WorldSystems::Edits` left it in the previous frame, or
/// in this frame when they run after it. Queries never change anything.
#[derive(SystemParam)]
pub struct PhysicsQuery<'w> {
    map: Res<'w, ChunkMap>,
    settings: Res<'w, WorldSettings>,
    content: Res<'w, PhysicsContent>,
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
}
