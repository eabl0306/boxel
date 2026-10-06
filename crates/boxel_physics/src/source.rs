//! [`ChunkSource`]: the world's [`ChunkMap`] as a [`VoxelSource`].

use crate::core::{Probe, VoxelSource};
use boxel_voxel::{BlockPos, BlockState, ChunkData, ChunkPos};
use boxel_world::{ChunkMap, DataState, WorldSettings};
use std::ops::RangeInclusive;

/// Reads blocks from the world's [`ChunkMap`] for physics:
///
/// | Chunk | Probe |
/// |---|---|
/// | `Ready` | the block (a uniform chunk answers without decoding anything) |
/// | `Missing` (generation failed) | air |
/// | a layer outside `WorldSettings::vertical_bounds` | air (the world never loads it) |
/// | `Queued`, `Generating`, or no slot | [`Probe::Unloaded`] |
///
/// It keeps the last chunk it looked up: one hash lookup per chunk touched, then a few
/// nanoseconds per block. Cheap to create; make one per query or per body and drop it (it
/// borrows the map, so the world cannot change while it lives).
pub struct ChunkSource<'a> {
    map: &'a ChunkMap,
    bounds: Option<RangeInclusive<i64>>,
    cached: ChunkPos,
    entry: Entry<'a>,
}

/// What [`ChunkSource`] knows about its cached chunk.
#[derive(Clone, Copy)]
enum Entry<'a> {
    /// Every block is this state (uniform storage, missing data, outside the bounds).
    Uniform(BlockState),
    /// Mixed blocks.
    Data(&'a ChunkData),
    /// Not loaded.
    Unloaded,
}

impl<'a> ChunkSource<'a> {
    /// A source over `map` with the vertical bounds of `settings`.
    pub fn new(map: &'a ChunkMap, settings: &WorldSettings) -> Self {
        Self::with_bounds(map, settings.vertical_bounds.clone())
    }

    /// A source over `map`; chunk layers outside `bounds` (when set) read as air.
    pub fn with_bounds(map: &'a ChunkMap, bounds: Option<RangeInclusive<i64>>) -> Self {
        Self {
            map,
            bounds,
            // No block is in this chunk (block coordinates are i64, chunks reach -2^58): the
            // first probe always looks up.
            cached: ChunkPos::new(i64::MIN, i64::MIN, i64::MIN),
            entry: Entry::Unloaded,
        }
    }

    fn lookup(&self, pos: ChunkPos) -> Entry<'a> {
        if let Some(bounds) = &self.bounds
            && !bounds.contains(&pos.y)
        {
            return Entry::Uniform(BlockState::AIR);
        }
        let map: &'a ChunkMap = self.map;
        let Some(slot) = map.id_at(pos).and_then(|id| map.get(id)) else {
            return Entry::Unloaded;
        };
        match slot.data() {
            DataState::Ready(data) => match data.as_uniform() {
                Some(state) => Entry::Uniform(state),
                None => Entry::Data(data),
            },
            DataState::Missing => Entry::Uniform(BlockState::AIR),
            DataState::Queued | DataState::Generating => Entry::Unloaded,
        }
    }
}

impl VoxelSource for ChunkSource<'_> {
    #[inline]
    fn probe(&mut self, p: BlockPos) -> Probe {
        let (chunk, local) = p.split();
        if chunk != self.cached {
            self.entry = self.lookup(chunk);
            self.cached = chunk;
        }
        match self.entry {
            Entry::Uniform(state) => Probe::Block(state),
            Entry::Data(data) => Probe::Block(data.get(local)),
            Entry::Unloaded => Probe::Unloaded,
        }
    }
}
