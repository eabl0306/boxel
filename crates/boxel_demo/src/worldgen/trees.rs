//! Deterministic trees (log trunk, leaf canopy), placed per chunk without neighbor data. Used by
//! [`ForestGenerator`](super::ForestGenerator) and
//! [`OverworldGenerator`](super::OverworldGenerator).

use boxel_voxel::{BlockPos, BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, LocalPos};
use boxel_worldgen::noise::hash2;
use std::ops::RangeInclusive;

/// Columns per side of a tree cell: at most one tree per cell.
const CELL: i64 = 8;
/// Canopy reach around the trunk.
const CANOPY_RADIUS: i64 = 2;
/// Shortest trunk.
const TRUNK_MIN: i64 = 4;
/// Salt of the tree lattice, so it is not correlated with the height noise.
const TREE_SALT: u64 = 0x007e_e50f_b0e1;

/// Trees: at most one per 8x8 cell of columns (a third of the cells), trunk of 4 to 6 logs on
/// the column's ground, and a leaf canopy around the top.
///
/// A tree stays inside its cell (the trunk is at least [`Trees::CANOPY_RADIUS`] columns from the
/// cell border), and cells align with chunk borders, so a tree never crosses a chunk border
/// horizontally: a chunk only needs the trees of its own columns, plus the ones growing from
/// the chunk below. Each cell decides its tree from `hash2(seed ^ salt, cx, cz)` and from the
/// ground the caller reports for the trunk column, so the trees are the same function at any
/// `i64` column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trees {
    /// Salt of the tree lattice (mixed into the world seed with XOR). Explicit, so another stage never
    /// moves the trees, and the trees never move another stage.
    pub salt: u64,
}

impl Default for Trees {
    /// The salt of the former example `Forest`: the same trees for the same seed.
    fn default() -> Self {
        Self { salt: TREE_SALT }
    }
}

/// One tree: trunk column `(x, z)` growing from the ground block at height `ground`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tree {
    /// Trunk column x.
    pub x: i64,
    /// Trunk column z.
    pub z: i64,
    /// Height of the ground block the trunk stands on.
    pub ground: i64,
    /// Logs in the trunk.
    pub trunk: i64,
}

/// What a tree puts at a voxel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeBlock {
    /// Trunk.
    Log,
    /// Canopy.
    Leaves,
}

impl Trees {
    /// Columns per side of a tree cell.
    pub const CELL: i64 = CELL;
    /// Canopy reach around the trunk.
    pub const CANOPY_RADIUS: i64 = CANOPY_RADIUS;
    /// Highest tree block above the ground: the tallest trunk plus 2 canopy layers above it.
    pub const HEIGHT: i64 = TRUNK_MIN + 2 + 2;

    /// The tree of cell `(cx, cz)`, if it has one. `ground(x, z)` gives the ground height of the
    /// trunk column, or `None` where no tree may grow (not grass, or no solid block under the
    /// trunk); it is only called for the cells that hash to a tree.
    pub fn tree_in_cell(
        &self,
        seed: u64,
        cx: i64,
        cz: i64,
        ground: impl FnOnce(i64, i64) -> Option<i64>,
    ) -> Option<Tree> {
        let h = hash2(seed ^ self.salt, cx, cz);
        if !h.is_multiple_of(3) {
            return None;
        }
        let span = (CELL - 2 * CANOPY_RADIUS) as u32;
        // No overflow: columns stay within the world limit (+-2^61).
        let x = cx * CELL + CANOPY_RADIUS + i64::from((h >> 8) % span);
        let z = cz * CELL + CANOPY_RADIUS + i64::from((h >> 16) % span);
        Some(Tree {
            x,
            z,
            ground: ground(x, z)?,
            trunk: TRUNK_MIN + i64::from((h >> 24) % 3),
        })
    }

    /// The trees with a block inside the chunk whose lowest corner is `origin`. `ground` as in
    /// [`Trees::tree_in_cell`]; `grounds` bounds every height it can return (a chunk that no
    /// tree on such ground reaches asks for none).
    pub fn reaching(
        &self,
        seed: u64,
        origin: BlockPos,
        grounds: RangeInclusive<i64>,
        mut ground: impl FnMut(i64, i64) -> Option<i64>,
    ) -> Vec<Tree> {
        let size = i64::from(CHUNK_SIZE);
        // Tree blocks span ground + 1 ..= ground + HEIGHT.
        if origin.y + size - 1 <= *grounds.start() || origin.y > grounds.end() + Self::HEIGHT {
            return Vec::new();
        }
        // The chunk's own cells: trees never cross a chunk border horizontally.
        let cells = |lo: i64| lo.div_euclid(CELL)..=(lo + size - 1).div_euclid(CELL);
        let mut trees = Vec::new();
        for cx in cells(origin.x) {
            for cz in cells(origin.z) {
                trees.extend(self.tree_in_cell(seed, cx, cz, &mut ground));
            }
        }
        trees.retain(|t| t.ground + 1 < origin.y + size && t.top() + 2 >= origin.y);
        trees
    }

    /// Puts the blocks of `trees` that fall inside the chunk at `origin` into `data`, only over
    /// air.
    pub fn place(
        trees: &[Tree],
        origin: BlockPos,
        data: &mut ChunkData,
        log: BlockState,
        leaves: BlockState,
    ) {
        let size = i64::from(CHUNK_SIZE);
        for tree in trees {
            tree.blocks(|x, y, z, block| {
                let (lx, ly, lz) = (x - origin.x, y - origin.y, z - origin.z);
                if [lx, ly, lz].iter().all(|c| (0..size).contains(c)) {
                    let p = LocalPos::new(lx as u32, ly as u32, lz as u32);
                    if data.get(p) == BlockState::AIR {
                        let state = match block {
                            TreeBlock::Log => log,
                            TreeBlock::Leaves => leaves,
                        };
                        data.set(p, state);
                    }
                }
            });
        }
    }

    /// [`Trees::place`] into the dense voxels of a chunk (indexed like [`LocalPos::index`]):
    /// cheaper than [`ChunkData::set`], whose palette may grow and repack.
    ///
    /// # Panics
    ///
    /// If `dense.len()` is not `CHUNK_VOLUME`.
    pub fn place_dense(
        trees: &[Tree],
        origin: BlockPos,
        dense: &mut [BlockState],
        log: BlockState,
        leaves: BlockState,
    ) {
        assert_eq!(
            dense.len(),
            CHUNK_VOLUME,
            "place_dense needs CHUNK_VOLUME voxels"
        );
        let size = i64::from(CHUNK_SIZE);
        for tree in trees {
            tree.blocks(|x, y, z, block| {
                let (lx, ly, lz) = (x - origin.x, y - origin.y, z - origin.z);
                if [lx, ly, lz].iter().all(|c| (0..size).contains(c)) {
                    let slot = &mut dense[LocalPos::new(lx as u32, ly as u32, lz as u32).index()];
                    if *slot == BlockState::AIR {
                        *slot = match block {
                            TreeBlock::Log => log,
                            TreeBlock::Leaves => leaves,
                        };
                    }
                }
            });
        }
    }

    /// The tree block at `p`, if a tree puts one there (point form of [`Trees::reaching`] and
    /// [`Trees::place`], whatever is under it).
    pub fn block_at(
        &self,
        seed: u64,
        p: BlockPos,
        ground: impl FnOnce(i64, i64) -> Option<i64>,
    ) -> Option<TreeBlock> {
        // A tree stays inside its cell.
        let tree = self.tree_in_cell(seed, p.x.div_euclid(CELL), p.z.div_euclid(CELL), ground)?;
        let mut found = None;
        tree.blocks(|x, y, z, block| {
            if (x, y, z) == (p.x, p.y, p.z) {
                found = Some(block);
            }
        });
        found
    }
}

impl Tree {
    /// Height of the top log.
    pub fn top(&self) -> i64 {
        self.ground + self.trunk
    }

    /// Calls `put(x, y, z, block)` for every block of the tree (no position twice).
    pub fn blocks(&self, mut put: impl FnMut(i64, i64, i64, TreeBlock)) {
        let top = self.top();
        for y in self.ground + 1..=top {
            put(self.x, y, self.z, TreeBlock::Log);
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
                    put(self.x + dx, y, self.z + dz, TreeBlock::Leaves);
                }
            }
        }
    }
}
