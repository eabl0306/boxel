//! What the sweep and the raycast read: a [`VoxelSource`] answers one block at a time, and the
//! [`CollisionTable`] says how each block state collides.

use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, ChunkData, ChunkPos, RenderShape, StateFlags,
};
use std::collections::{HashMap, HashSet};

/// The answer of a [`VoxelSource`] for one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The block. Space with no data that is known to be empty (a chunk whose generation failed,
    /// layers outside the world's vertical bounds) reads as air.
    Block(BlockState),
    /// The block's chunk is not loaded yet: its content is unknown. Physics never guesses: a
    /// sweep that needs this block stops ([`crate::Unloaded`]) and a ray reports it.
    Unloaded,
}

/// Read access to voxels, one block at a time. `&mut self` so implementations can cache (the
/// last chunk read: consecutive probes are almost always in the same chunk).
pub trait VoxelSource {
    /// The block at `p`.
    fn probe(&mut self, p: BlockPos) -> Probe;
}

impl<S: VoxelSource + ?Sized> VoxelSource for &mut S {
    #[inline]
    fn probe(&mut self, p: BlockPos) -> Probe {
        (**self).probe(p)
    }
}

/// How a block collides. Today every block is a full cube or nothing; shapes made of boxes
/// (slabs, stairs, fences) are reserved for later, hence `non_exhaustive`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CollisionShape {
    /// No collision (air, water, flowers).
    #[default]
    Empty,
    /// The whole unit cube.
    Full,
}

/// `CollisionTable` bit: the block collides as a full cube.
const FULL: u8 = 1;
/// `CollisionTable` bit: the block is a fluid.
const FLUID: u8 = 1 << 1;
/// `CollisionTable` bit: the block has geometry (picking outline).
const OUTLINE: u8 = 1 << 2;

/// Per block state, what physics needs to know: the [`CollisionShape`] and two flags (fluid,
/// has an outline). Built from a [`BlockRegistry`]: `BlockDef::solid` gives `Full`, the
/// registry's `FLUID` flag gives the fluid bit and a render shape other than `None` gives the
/// outline bit. States outside the registry are empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollisionTable {
    bits: Vec<u8>,
}

impl CollisionTable {
    /// The table of every state of `registry`.
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let bits = (0..registry.len())
            .map(|i| {
                let state = BlockState(i as u16);
                let flags = registry.flags(state);
                let mut bits = 0;
                if flags.contains(StateFlags::SOLID) {
                    bits |= FULL;
                }
                if flags.contains(StateFlags::FLUID) {
                    bits |= FLUID;
                }
                if registry.shape(state) != RenderShape::None {
                    bits |= OUTLINE;
                }
                bits
            })
            .collect();
        Self { bits }
    }

    #[inline]
    fn bits(&self, state: BlockState) -> u8 {
        self.bits.get(state.0 as usize).copied().unwrap_or(0)
    }

    /// How `state` collides.
    pub fn shape(&self, state: BlockState) -> CollisionShape {
        if self.is_full(state) {
            CollisionShape::Full
        } else {
            CollisionShape::Empty
        }
    }

    /// `state` collides as a full cube.
    #[inline]
    pub fn is_full(&self, state: BlockState) -> bool {
        self.bits(state) & FULL != 0
    }

    /// `state` is a fluid (water).
    #[inline]
    pub fn is_fluid(&self, state: BlockState) -> bool {
        self.bits(state) & FLUID != 0
    }

    /// `state` has geometry, so it can be picked (everything but air and invisible blocks).
    #[inline]
    pub fn has_outline(&self, state: BlockState) -> bool {
        self.bits(state) & OUTLINE != 0
    }

    /// Number of states.
    pub fn len(&self) -> usize {
        self.bits.len()
    }

    /// No states at all (not even air).
    pub fn is_empty(&self) -> bool {
        self.bits.is_empty()
    }
}

/// A small in-memory world for tests, benches and tools: chunks in a hash map (a chunk that is
/// not there is air) plus a set of chunks that read as [`Probe::Unloaded`]. No cache: every
/// probe is one hash lookup. The ECS layer reads the real world through
/// [`crate::ChunkSource`].
#[derive(Clone, Debug, Default)]
pub struct TestWorld {
    chunks: HashMap<ChunkPos, ChunkData>,
    unloaded: HashSet<ChunkPos>,
}

impl TestWorld {
    /// All air, all loaded.
    pub fn new() -> Self {
        Self::default()
    }

    /// The block at `p` (air in a chunk that was never written; also for unloaded chunks).
    pub fn get(&self, p: BlockPos) -> BlockState {
        let (chunk, local) = p.split();
        self.chunks
            .get(&chunk)
            .map_or(BlockState::AIR, |data| data.get(local))
    }

    /// Sets one block.
    pub fn set(&mut self, p: BlockPos, state: BlockState) {
        let (chunk, local) = p.split();
        self.chunks
            .entry(chunk)
            .or_insert_with(|| ChunkData::uniform(BlockState::AIR))
            .set(local, state);
    }

    /// Sets every block of the box `min..=max` (inclusive on every axis).
    pub fn fill(&mut self, min: BlockPos, max: BlockPos, state: BlockState) {
        for y in min.y..=max.y {
            for x in min.x..=max.x {
                for z in min.z..=max.z {
                    self.set(BlockPos::new(x, y, z), state);
                }
            }
        }
    }

    /// Replaces a whole chunk (any storage: uniform, packed, direct).
    pub fn set_chunk(&mut self, pos: ChunkPos, data: ChunkData) {
        self.chunks.insert(pos, data);
    }

    /// The stored chunk at `pos`, if any was written.
    pub fn chunk(&self, pos: ChunkPos) -> Option<&ChunkData> {
        self.chunks.get(&pos)
    }

    /// Marks a chunk as unloaded (`true`) or loaded again (`false`); its blocks are kept.
    pub fn set_unloaded(&mut self, pos: ChunkPos, unloaded: bool) {
        if unloaded {
            self.unloaded.insert(pos);
        } else {
            self.unloaded.remove(&pos);
        }
    }
}

impl VoxelSource for TestWorld {
    fn probe(&mut self, p: BlockPos) -> Probe {
        let mut view: &TestWorld = self;
        view.probe(p)
    }
}

impl VoxelSource for &TestWorld {
    fn probe(&mut self, p: BlockPos) -> Probe {
        let (chunk, local) = p.split();
        if self.unloaded.contains(&chunk) {
            return Probe::Unloaded;
        }
        Probe::Block(
            self.chunks
                .get(&chunk)
                .map_or(BlockState::AIR, |data| data.get(local)),
        )
    }
}
