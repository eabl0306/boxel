//! Far level of detail (ADR 0031): a generator's coarse terrain, sampled level by level.
//!
//! The engine draws terrain far beyond the full-detail view distance as LOD nodes: a level-`k`
//! node is a 32^3 grid of coarse cells, each standing for `s^3` blocks (`s = 2^k`), so a node is
//! `32 s` blocks wide. Its key is a [`ChunkKey`] with `lod = k`: node `pos` starts at block
//! `pos << (5 + k)`, and the world limit of `pos` is `MAX_CHUNK_COORD >> k`.
//!
//! A generator that can answer for a coarse cell without building the blocks under it implements
//! [`LodSource`] and returns it from [`WorldGenerator::lod`](crate::WorldGenerator::lod). The
//! engine asks for one column of one level at a time ([`LodRequest`]), so a heightfield is
//! sampled once for every layer of the column. Every cell follows the cell rule of
//! [`boxel_voxel::lod`] (re-exported here):
//!
//! 1. If any block of the cell is Solid: when the top-most Fluid block lies above the top-most
//!    Solid block and the cell holds at least `s^2 / 2` Fluid blocks, the cell is that fluid.
//!    Otherwise the cell is Solid, with the state of its Solid block of greatest `y`, ties to the
//!    smallest `z`, then the smallest `x`.
//! 2. Else, if at least half of the cell's blocks are Fluid, the cell is that fluid.
//! 3. Else air.
//!
//! "That fluid" is the source with the most blocks in the cell (ties to the smallest id), and the
//! classes come from [`LodClasses::from_registry`]: plants, glass and thin models are Empty,
//! cubes (cutout ones too), slabs and models filling at least half the cell are Solid. A source
//! may approximate the rule (a heightfield cannot afford to look at every block of a 16-block
//! cell), within bounds its own tests check; what the engine builds from real chunks (player
//! edits) follows it exactly.
//!
//! Below a column's band the source reports one state ([`LodColumn::below`]); a world floor with
//! air under it may count as solid there, so no node is meshed for faces nobody sees from above.
//!
//! [`FlatGenerator`] has an exact source: its cells are the rule applied to its own blocks, which
//! makes it the oracle of the engine's tests.

use crate::FlatGenerator;
use boxel_voxel::{BlockRegistry, BlockState, ChunkKey, ChunkPos, DimensionId};
use std::any::Any;
use std::ops::RangeInclusive;

pub use boxel_voxel::lod::{CellPartial, HALF_CELL_VOLUME, LodClass, LodClasses};

/// Edge of a node's cell grid with its 1-cell border: 32 + 2.
pub const LOD_PADDED: usize = 34;
/// `LOD_PADDED^3`: the cells of [`LodCells`].
pub const LOD_PADDED_VOLUME: usize = LOD_PADDED * LOD_PADDED * LOD_PADDED;
/// Highest LOD level the engine asks for (4 bits on the GPU).
pub const MAX_LOD_LEVEL: u8 = 15;

/// What the engine asks a [`LodSource`] for: some layers of one column of level-`level` nodes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LodRequest {
    /// Dimension.
    pub dim: DimensionId,
    /// LOD level `k`, `1..=MAX_LOD_LEVEL`: a cell is `2^k` blocks.
    pub level: u8,
    /// The column `[x, z]`, in level-`k` nodes.
    pub column: [i64; 2],
    /// The node layers the engine wants (`y`, in level-`k` nodes).
    pub layers: RangeInclusive<i64>,
}

impl LodRequest {
    /// The edge of a cell, in blocks (`2^level`).
    pub const fn cell_size(&self) -> i64 {
        1 << self.level
    }

    /// The edge of a node, in blocks (`32 * 2^level`).
    pub const fn node_size(&self) -> i64 {
        32 << self.level
    }

    /// The key of this column's node at `layer`.
    pub const fn key(&self, layer: i64) -> ChunkKey {
        ChunkKey {
            dim: self.dim,
            lod: self.level,
            pos: ChunkPos::new(self.column[0], layer, self.column[1]),
        }
    }

    /// The block position `[x, y, z]` of the min corner of this column's node at `layer`: the
    /// node's cell `(0, 0, 0)` spans `origin..origin + cell_size()` on each axis.
    pub const fn origin(&self, layer: i64) -> [i64; 3] {
        let shift = 5 + self.level as u32;
        [
            self.column[0] << shift,
            layer << shift,
            self.column[1] << shift,
        ]
    }
}

/// A node's 34^3 cells: its 32^3 plus a 1-cell border, `(x, y, z)` each in `-1..=32`, in the
/// mesher's padded order (`LodCells::index`). Each cell holds the state the cell rule gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LodCells(pub Box<[BlockState; LOD_PADDED_VOLUME]>);

impl Default for LodCells {
    fn default() -> Self {
        Self::new()
    }
}

impl LodCells {
    /// All air.
    pub fn new() -> Self {
        let cells: Box<[BlockState]> = vec![BlockState::AIR; LOD_PADDED_VOLUME].into();
        Self(cells.try_into().expect("LOD_PADDED_VOLUME cells"))
    }

    /// Index of cell `(x, y, z)`, each in `-1..=32`: `(z + 1) + (x + 1) * 34 + (y + 1) * 34^2`.
    #[inline]
    pub const fn index(x: i32, y: i32, z: i32) -> usize {
        (z + 1) as usize
            + (x + 1) as usize * LOD_PADDED
            + (y + 1) as usize * LOD_PADDED * LOD_PADDED
    }

    /// The cell at `(x, y, z)`, each in `-1..=32`.
    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> BlockState {
        self.0[Self::index(x, y, z)]
    }

    /// Sets the cell at `(x, y, z)`, each in `-1..=32`.
    #[inline]
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        self.0[Self::index(x, y, z)] = state;
    }

    /// Sets every cell to `state`.
    pub fn fill(&mut self, state: BlockState) {
        self.0.fill(state);
    }

    /// The state of every cell, border included, when they are all equal.
    pub fn uniform(&self) -> Option<BlockState> {
        let first = self.0[0];
        self.0.iter().all(|&s| s == first).then_some(first)
    }
}

/// One node of a column, as a [`LodSource`] emits it.
#[derive(Clone, Copy, Debug)]
pub enum LodNode<'a> {
    /// Every cell, border included, is this state.
    Uniform(BlockState),
    /// The node's cells.
    Cells(&'a LodCells),
}

/// The vertical extent of a column ([`LodSource::column`]): layers below `lo` are all `below`,
/// layers at or above `hi` are air.
///
/// Every layer outside `lo..hi` is uniform over its whole 34^3 padded block, the border cells of
/// the four neighboring columns and of the layers above and below included, so a uniform layer
/// never needs a face (a cliff wall toward a lower neighbor lies inside `lo..hi`). In particular,
/// when `below` is not air, the bottom cell row of layer `lo` (border included) is all non-air.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LodColumn {
    /// The first layer that may not be uniform.
    pub lo: i64,
    /// The first layer above `lo` that is air again, with every layer above it.
    pub hi: i64,
    /// The state of every layer below `lo`.
    pub below: BlockState,
}

impl LodColumn {
    /// A column of air at every layer.
    pub const AIR: Self = Self {
        lo: 0,
        hi: 0,
        below: BlockState::AIR,
    };

    /// The state of `layer` if it lies outside the band (`None` inside `lo..hi`).
    pub const fn uniform_at(&self, layer: i64) -> Option<BlockState> {
        if layer < self.lo {
            Some(self.below)
        } else if layer >= self.hi {
            Some(BlockState::AIR)
        } else {
            None
        }
    }
}

/// A worker's scratch for [`LodSource::column`], reused from call to call on one thread.
#[derive(Default)]
pub struct LodScratch {
    /// The cells a source fills and emits.
    pub cells: LodCells,
    /// Whatever the source keeps between calls on this worker (sample tiles, buffers). The engine
    /// never reads it; a source downcasts it to its own type.
    pub memo: Option<Box<dyn Any + Send>>,
}

impl std::fmt::Debug for LodScratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LodScratch")
            .field("memo", &self.memo.is_some())
            .finish_non_exhaustive()
    }
}

/// A generator's coarse terrain (module docs).
pub trait LodSource: Send + Sync {
    /// Emits every layer of `req.layers` that lies inside the column's band `lo..hi`, in any
    /// order, each as [`LodNode::Uniform`] or [`LodNode::Cells`] following the cell rule
    /// (approximations allowed, bounded by the source's tests), and returns the band. Layers of
    /// `req.layers` outside the band are not emitted: [`LodColumn`] says what they are.
    /// Deterministic; bounded time at any height and at any column inside the world limit.
    fn column(
        &self,
        req: &LodRequest,
        registry: &BlockRegistry,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn;

    /// Bumped when the coarse output changes (documentation, and a future persisted table).
    fn version(&self) -> u32 {
        0
    }
}

/// [`FlatGenerator`]'s classes, kept between calls on one worker for one registry.
struct FlatMemo {
    /// The registry they were made from (its address and size).
    registry: (usize, usize),
    classes: LodClasses,
}

impl FlatGenerator {
    /// The state of the cell whose rows of blocks start at `y0` and are `s` high: the cell rule
    /// applied to the layers (every row of blocks is one state over its `s^2` blocks).
    fn cell_at(
        &self,
        layers: &[(BlockState, i64)],
        classes: &LodClasses,
        y0: i64,
        s: i64,
    ) -> BlockState {
        let area = (s * s) as u64;
        let mut p = CellPartial::empty(area * s as u64);
        let mut kinds: Vec<(BlockState, u64)> = Vec::new();
        let (mut top, mut at) = (0i64, 0usize);
        for y in y0..y0 + s {
            // The layer holding block row `y` (air below 0 and above the layers).
            let state = if y < 0 {
                BlockState::AIR
            } else {
                while at < layers.len() && y >= top + layers[at].1 {
                    top += layers[at].1;
                    at += 1;
                }
                layers.get(at).map_or(BlockState::AIR, |l| l.0)
            };
            match classes.class(state) {
                LodClass::Empty => {}
                // Rows go up: the last Solid row is the top one (one state across the row).
                LodClass::Solid => p.solid = Some(([0, y, 0], state)),
                LodClass::Fluid => {
                    let source = classes.fluid(state).unwrap_or(state);
                    p.fluid_top = Some(y);
                    p.fluid_blocks += area;
                    match kinds.iter_mut().find(|k| k.0 == source) {
                        Some(k) => k.1 += area,
                        None => kinds.push((source, area)),
                    }
                }
            }
        }
        // The fluid with the most blocks, ties to the smaller id (as `CellPartial::of_cell`).
        p.fluid = kinds.into_iter().reduce(|a, b| {
            if b.1 > a.1 || (b.1 == a.1 && b.0 < a.0) {
                b
            } else {
                a
            }
        });
        p.state()
    }
}

/// Exact: every cell is the cell rule applied to the generator's own blocks (the test oracle of
/// the engine's LOD). Layers hold the same state over a whole row of blocks, so a cell only looks
/// at its `s` rows.
impl LodSource for FlatGenerator {
    fn column(
        &self,
        req: &LodRequest,
        registry: &BlockRegistry,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn {
        let height = self.total_height();
        if height <= 0 {
            return LodColumn::AIR;
        }
        let (s, e) = (req.cell_size(), req.node_size());
        // Layer `L`'s cells span blocks `L e - s .. L e + 33 s`: layer -1 holds the bottom layer
        // in its top border row, and everything outside the band is air.
        let band = LodColumn {
            lo: -1,
            hi: -(-(height + s)).div_euclid(e),
            below: BlockState::AIR,
        };
        let key = (std::ptr::from_ref(registry) as usize, registry.len());
        let fresh = !matches!(
            scratch.memo.as_ref().and_then(|m| m.downcast_ref::<FlatMemo>()),
            Some(m) if m.registry == key
        );
        if fresh {
            scratch.memo = Some(Box::new(FlatMemo {
                registry: key,
                classes: LodClasses::from_registry(registry),
            }));
        }
        let memo = scratch.memo.take().expect("set above");
        let classes = &memo.downcast_ref::<FlatMemo>().expect("set above").classes;
        let layers: Vec<(BlockState, i64)> = self
            .layers
            .iter()
            .map(|(name, n)| (crate::block(registry, name, "FlatGenerator"), i64::from(*n)))
            .collect();
        let first = (*req.layers.start()).max(band.lo);
        let last = (*req.layers.end()).min(band.hi - 1);
        for layer in first..=last {
            let y0 = req.origin(layer)[1];
            let rows: Vec<BlockState> = (-1..=32i64)
                .map(|cy| self.cell_at(&layers, classes, y0 + cy * s, s))
                .collect();
            if rows.iter().all(|&r| r == rows[0]) {
                emit(layer, LodNode::Uniform(rows[0]));
                continue;
            }
            for (cy, &state) in (-1..=32i32).zip(&rows) {
                for x in -1..=32 {
                    for z in -1..=32 {
                        scratch.cells.set(x, cy, z, state);
                    }
                }
            }
            emit(layer, LodNode::Cells(&scratch.cells));
        }
        scratch.memo = Some(memo);
        band
    }
}
