//! The `voxel_world` terrain: [`TerrainGenerator`] plus deterministic trees (log trunk, leaf
//! canopy). Same code as in `examples/voxel_world.rs`, so both examples stream the same world for
//! a given seed (keep them in sync until they share one module).

use boxel::prelude::*;
use boxel::voxel::{BlockRegistry, BlockState, CHUNK_SIZE, ChunkData, ChunkKey, LocalPos};
use boxel::worldgen::noise::hash2;
use std::ops::RangeInclusive;

/// Trees: at most one per `TREE_CELL`^2 columns, on grass only.
const TREE_CELL: i32 = 8;
/// Canopy reach around the trunk.
const CANOPY_RADIUS: i32 = 2;
/// Shortest trunk, and the tallest tree above the ground (trunk + 2 canopy layers above it).
const TRUNK_MIN: i32 = 4;
const TREE_HEIGHT_MAX: i32 = TRUNK_MIN + 2 + 2;
/// Salt so the tree lattice is not correlated with the height noise.
const TREE_SALT: u64 = 0x007e_e50f_b0e1;

/// [`TerrainGenerator`] plus deterministic trees. Each chunk places the parts of every tree
/// whose blocks reach into it (trunk column heights come from `height_at`), so trees crossing
/// chunk borders are consistent without neighbor data.
pub struct Forest {
    /// Height map and seed.
    pub terrain: TerrainGenerator,
}

#[derive(Clone, Copy, Debug)]
struct Tree {
    x: i32,
    z: i32,
    ground: i32,
    trunk: i32,
}

impl Forest {
    fn tree_in_cell(&self, cx: i32, cz: i32) -> Option<Tree> {
        let h = hash2(self.terrain.seed ^ TREE_SALT, cx, cz);
        if !h.is_multiple_of(3) {
            return None;
        }
        let span = (TREE_CELL - 2 * CANOPY_RADIUS) as u32;
        let x = cx * TREE_CELL + CANOPY_RADIUS + ((h >> 8) % span) as i32;
        let z = cz * TREE_CELL + CANOPY_RADIUS + ((h >> 16) % span) as i32;
        let ground = self.terrain.height_at(x, z);
        let s = &self.terrain.settings;
        if ground <= s.sand_level || ground > s.snow_level {
            return None;
        }
        Some(Tree {
            x,
            z,
            ground,
            trunk: TRUNK_MIN + ((h >> 24) % 3) as i32,
        })
    }
}

impl Tree {
    /// Calls `put(x, y, z, is_log)` for every block of the tree.
    fn blocks(self, mut put: impl FnMut(i32, i32, i32, bool)) {
        let top = self.ground + self.trunk;
        for y in self.ground + 1..=top {
            put(self.x, y, self.z, true);
        }
        for y in top - 1..=top + 2 {
            let (radius, round) = match y - top {
                -1 | 0 => (CANOPY_RADIUS, true),
                1 => (1, false),
                _ => (1, true),
            };
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    let corner = dx.abs() == radius && dz.abs() == radius;
                    if (round && corner) || (dx == 0 && dz == 0 && y <= top) {
                        continue;
                    }
                    put(self.x + dx, y, self.z + dz, false);
                }
            }
        }
    }
}

impl WorldGenerator for Forest {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let mut data = self.terrain.generate(key, registry);
        let (Some(log), Some(leaves)) = (
            registry.by_name("boxel:log"),
            registry.by_name("boxel:leaves"),
        ) else {
            return data;
        };
        let s = &self.terrain.settings;
        let o = key.pos.origin();
        let size = CHUNK_SIZE;
        if o.y + size <= s.sand_level + 1 || o.y > s.snow_level + TREE_HEIGHT_MAX {
            return data;
        }
        let cells = |lo: i32| {
            (lo - CANOPY_RADIUS).div_euclid(TREE_CELL)
                ..=(lo + size - 1 + CANOPY_RADIUS).div_euclid(TREE_CELL)
        };
        for cx in cells(o.x) {
            for cz in cells(o.z) {
                let Some(tree) = self.tree_in_cell(cx, cz) else {
                    continue;
                };
                tree.blocks(|x, y, z, is_log| {
                    let (lx, ly, lz) = (x - o.x, y - o.y, z - o.z);
                    if [lx, ly, lz].iter().all(|c| (0..size).contains(c)) {
                        let p = LocalPos::new(lx as u32, ly as u32, lz as u32);
                        if data.get(p) == BlockState::AIR {
                            data.set(p, if is_log { log } else { leaves });
                        }
                    }
                });
            }
        }
        data
    }

    fn vertical_chunks(&self) -> RangeInclusive<i32> {
        let inner = self.terrain.vertical_chunks();
        let tree_top = (self.terrain.settings.snow_level + TREE_HEIGHT_MAX) >> 5;
        *inner.start()..=(*inner.end()).max(tree_top)
    }

    fn name(&self) -> &str {
        "terrain+trees"
    }
}
