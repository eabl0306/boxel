//! [`ChunkSource`]: the world's [`ChunkMap`] as a [`VoxelSource`].

use crate::core::{CollisionTable, Probe, VoxelSource};
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
/// It keeps the last two chunks it looked up: one hash lookup per chunk touched, then a few
/// nanoseconds per block, also when reads alternate between two chunks (a box across a chunk
/// border). Cheap to create; make one per query or per body and drop it (it borrows the map, so
/// the world cannot change while it lives).
///
/// [`VoxelSource::tall_free`]: a `Ready` chunk answers from its palette
/// ([`ChunkData::may_contain`], cached in the chunk), a uniform one from its state, and a chunk
/// that is not loaded is never free. The answer is kept with the cached chunk.
pub struct ChunkSource<'a> {
    map: &'a ChunkMap,
    bounds: Option<RangeInclusive<i64>>,
    /// The last chunk looked up (`slots[0]`, the one probes read) and the one before it.
    slots: [Slot<'a>; 2],
}

/// One cached chunk of a [`ChunkSource`].
#[derive(Clone, Copy)]
struct Slot<'a> {
    pos: ChunkPos,
    entry: Entry<'a>,
    /// The last `tall_free` answer: `key << 1 | free`, with the key of the table's tall states
    /// (0: not asked yet).
    tall: u64,
}

impl Slot<'_> {
    /// No block is in this chunk (block coordinates are i64, chunks reach -2^58): the first
    /// probe always looks up.
    const NOWHERE: Self = Slot {
        pos: ChunkPos::new(i64::MIN, i64::MIN, i64::MIN),
        entry: Entry::Unloaded,
        tall: 0,
    };
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
            slots: [Slot::NOWHERE; 2],
        }
    }

    /// Makes `slots[0]` hold `chunk` (it did not): the other slot if it has it, else a lookup
    /// (the old `slots[0]` becomes the other one).
    #[inline(never)]
    fn fetch(&mut self, chunk: ChunkPos) {
        if self.slots[1].pos == chunk {
            self.slots.swap(0, 1);
        } else {
            self.slots[1] = self.slots[0];
            self.slots[0] = Slot {
                pos: chunk,
                entry: self.lookup(chunk),
                tall: 0,
            };
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

impl Entry<'_> {
    /// The chunk is loaded and holds no tall block of `table`.
    fn tall_free(self, table: &CollisionTable) -> bool {
        match self {
            Entry::Uniform(state) => !table.is_tall(state),
            Entry::Data(data) => !data.may_contain(table.tall_states()),
            Entry::Unloaded => false,
        }
    }
}

impl VoxelSource for ChunkSource<'_> {
    #[inline]
    fn probe(&mut self, p: BlockPos) -> Probe {
        let (chunk, local) = p.split();
        if chunk != self.slots[0].pos {
            self.fetch(chunk);
        }
        match self.slots[0].entry {
            Entry::Uniform(state) => Probe::Block(state),
            Entry::Data(data) => Probe::Block(data.get(local)),
            Entry::Unloaded => Probe::Unloaded,
        }
    }

    #[inline]
    fn tall_free(&mut self, chunk: ChunkPos, table: &CollisionTable) -> bool {
        if chunk != self.slots[0].pos {
            self.fetch(chunk);
        }
        let slot = &mut self.slots[0];
        let key = table.tall_states().key().get();
        if slot.tall >> 1 == key {
            return slot.tall & 1 != 0;
        }
        let free = slot.entry.tall_free(table);
        slot.tall = key << 1 | u64::from(free);
        free
    }
}
