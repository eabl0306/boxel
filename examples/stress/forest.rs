//! The `voxel_world` terrain: [`TerrainGenerator`] plus deterministic trees (log trunk, leaf
//! canopy). Same code as in `examples/voxel_world.rs`, so both examples stream the same world for
//! a given seed (keep them in sync until they share one module).

use boxel::prelude::*;
use boxel::voxel::{BlockRegistry, BlockState, CHUNK_SIZE, ChunkData, ChunkKey, LocalPos};
use boxel::worldgen::noise::hash2;
use std::ops::RangeInclusive;

/// Trees: at most one per `TREE_CELL`^2 columns, on grass only.
const TREE_CELL: i64 = 8;
/// Canopy reach around the trunk.
const CANOPY_RADIUS: i64 = 2;
/// Shortest trunk, and the tallest tree above the ground (trunk + 2 canopy layers above it).
const TRUNK_MIN: i64 = 4;
const TREE_HEIGHT_MAX: i64 = TRUNK_MIN + 2 + 2;
/// Salt so the tree lattice is not correlated with the height noise.
const TREE_SALT: u64 = 0x007e_e50f_b0e1;

/// [`TerrainGenerator`] plus deterministic trees. Each chunk places the parts of every tree
/// whose blocks reach into it (trunk column heights come from `height_at`), so trees crossing
/// chunk borders are consistent without neighbor data.
pub struct Forest {
    /// Height map and seed.
    pub terrain: TerrainGenerator,
}

/// World coordinates are `i64` (cells, trunks and blocks); inside the former `i32` range the
/// trees are the same as before (same cells, same `hash2` values).
#[derive(Clone, Copy, Debug)]
struct Tree {
    x: i64,
    z: i64,
    ground: i64,
    trunk: i64,
}

impl Forest {
    fn tree_in_cell(&self, cx: i64, cz: i64) -> Option<Tree> {
        let h = hash2(self.terrain.seed ^ TREE_SALT, cx, cz);
        if !h.is_multiple_of(3) {
            return None;
        }
        let span = (TREE_CELL - 2 * CANOPY_RADIUS) as u32;
        let x = cx * TREE_CELL + CANOPY_RADIUS + i64::from((h >> 8) % span);
        let z = cz * TREE_CELL + CANOPY_RADIUS + i64::from((h >> 16) % span);
        let ground = self.terrain.height_at(x, z);
        let s = &self.terrain.settings;
        if ground <= s.sand_level || ground > s.snow_level {
            return None;
        }
        Some(Tree {
            x,
            z,
            ground: ground.into(),
            trunk: TRUNK_MIN + i64::from((h >> 24) % 3),
        })
    }
}

impl Tree {
    /// Calls `put(x, y, z, is_log)` for every block of the tree.
    fn blocks(self, mut put: impl FnMut(i64, i64, i64, bool)) {
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
        let size = i64::from(CHUNK_SIZE);
        if o.y + size <= i64::from(s.sand_level) + 1
            || o.y > i64::from(s.snow_level) + TREE_HEIGHT_MAX
        {
            return data;
        }
        // Tree cells that may reach into the chunk (no overflow: origins stay within the world
        // limit, +-2^61).
        let cells = |lo: i64| {
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

    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        let inner = self.terrain.vertical_chunks();
        let tree_top = (i64::from(self.terrain.settings.snow_level) + TREE_HEIGHT_MAX) >> 5;
        *inner.start()..=(*inner.end()).max(tree_top)
    }

    fn name(&self) -> &str {
        "terrain+trees"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel::voxel::{CHUNK_VOLUME, ChunkPos};

    /// Fingerprint (log count, FNV-1a of every voxel state) of the 3x3 chunk columns from
    /// `(x, z)`, layers 1..=4.
    fn forest_fingerprint(
        forest: &Forest,
        registry: &BlockRegistry,
        x: i64,
        z: i64,
    ) -> (usize, u64) {
        let log = registry.by_name("boxel:log");
        let (mut logs, mut h) = (0, 0xcbf2_9ce4_8422_2325u64);
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        for dz in 0..3 {
            for dx in 0..3 {
                for y in 1..=4 {
                    let key = ChunkKey::new(ChunkPos::new(x + dx, y, z + dz));
                    forest.generate(key, registry).to_dense(&mut dense);
                    logs += dense.iter().filter(|s| Some(**s) == log).count();
                    for s in &dense {
                        for b in u64::from(s.0).to_le_bytes() {
                            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
                        }
                    }
                }
            }
        }
        (logs, h)
    }

    #[test]
    fn forest_inside_i32_is_unchanged_and_crosses_the_former_edge() {
        let forest = Forest {
            terrain: TerrainGenerator::default(),
        };
        let registry = BlockRegistry::default_blocks();
        // Recorded with the i32 coordinates (the last chunks before the old edge included).
        for ((x, z), expected) in [
            ((0, 0), (259, 0x0e4e_2255_79a0_f38d)),
            ((-3, 5), (284, 0xa713_c5ca_ff29_5b64)),
            ((7, -9), (6, 0x2418_de68_72ea_8247)),
            ((31_250, -31_250), (250, 0x9a87_d49f_8d57_45e4)),
            ((67_108_860, -67_108_863), (224, 0xdfd3_f57c_9c42_38a5)),
            ((-67_108_863, 67_108_860), (230, 0x96e8_7562_ea15_86c5)),
            ((12_345_678, -8_765_432), (245, 0x7995_606d_8843_dc2c)),
            ((-67_108_000, -67_100_000), (243, 0x97e4_a0c8_100c_a62c)),
        ] {
            assert_eq!(
                forest_fingerprint(&forest, &registry, x, z),
                expected,
                "({x}, {z})"
            );
        }
        // Across +-2^31 voxels (chunks +-2^26), where the i32 tree math overflowed: trees keep
        // growing on both sides.
        let edge = 1i64 << 26;
        for (x, z) in [(edge - 1, 0), (edge + 2, 5), (-edge - 2, -edge - 1)] {
            let (logs, _) = forest_fingerprint(&forest, &registry, x, z);
            assert!(logs > 0, "no trees at chunk ({x}, {z})");
        }
    }
}
