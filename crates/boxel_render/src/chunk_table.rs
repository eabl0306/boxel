//! Chunk slots: `ChunkGpu` storage table (High/Mid) mirrored on the CPU (all tiers, CPU culling).
use crate::arena::ArenaAlloc;
use boxel_math::{I64Vec3, IVec3, wrap_to_i32};
use boxel_mesh::BUCKET_COUNT;
use boxel_shaders::gpu::ChunkGpu;
use std::collections::{BTreeSet, HashMap};

/// CPU record of a resident chunk.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChunkRecord {
    /// Caller id (`ChunkId::to_bits`).
    pub id: u64,
    /// World voxel min corner (`i64`); the culling AABB is `[origin, origin + 32]`.
    pub origin: I64Vec3,
    /// Quad range of all buckets (bucket 0 first).
    pub alloc: ArenaAlloc,
    /// Quads per bucket.
    pub counts: [u32; BUCKET_COUNT],
}

impl ChunkRecord {
    /// The origin as the GPU stores it: its low 32 bits ([`wrap_to_i32`]). The shaders only use
    /// it minus the (equally wrapped) render origin, exact for chunks nearer than `2^31` voxels.
    pub fn gpu_origin(&self) -> IVec3 {
        wrap_to_i32(self.origin)
    }

    /// The GPU mirror of this record.
    pub fn gpu(&self) -> ChunkGpu {
        ChunkGpu {
            origin: self.gpu_origin().to_array(),
            quad_base: self.alloc.offset,
            counts: self.counts,
            flags: ChunkGpu::FLAG_LIVE,
            pad: [0; 3],
        }
    }
}

/// Slot allocator + records. A slot is the index into the GPU chunk table; slots are reused
/// lowest first so `slot_count` (highest live slot + 1, the High cull dispatch size) stays small.
pub(crate) struct ChunkTable {
    records: Vec<Option<ChunkRecord>>,
    free: BTreeSet<u32>,
    by_id: HashMap<u64, u32>,
    capacity: u32,
}

impl ChunkTable {
    /// Empty table of `capacity` slots.
    pub fn new(capacity: u32) -> Self {
        Self {
            records: Vec::new(),
            free: BTreeSet::new(),
            by_id: HashMap::new(),
            capacity,
        }
    }

    /// Slot capacity.
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Live chunks.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Highest live slot + 1.
    pub fn slot_count(&self) -> u32 {
        self.records.len() as u32
    }

    /// Record of `id`.
    pub fn get(&self, id: u64) -> Option<(u32, &ChunkRecord)> {
        let slot = *self.by_id.get(&id)?;
        self.records[slot as usize].as_ref().map(|r| (slot, r))
    }

    /// The slot `insert(id, ..)` would use: the chunk's own slot, else the lowest free slot,
    /// else the next index; `None` when the table is full.
    pub fn slot_for(&self, id: u64) -> Option<u32> {
        if let Some(&slot) = self.by_id.get(&id) {
            return Some(slot);
        }
        if let Some(&slot) = self.free.first() {
            return Some(slot);
        }
        let next = self.records.len() as u32;
        (next < self.capacity).then_some(next)
    }

    /// Inserts or replaces the record of `record.id`; returns its slot (`None` if full).
    pub fn insert(&mut self, record: ChunkRecord) -> Option<u32> {
        let slot = self.slot_for(record.id)?;
        self.free.remove(&slot);
        if slot as usize == self.records.len() {
            self.records.push(None);
        }
        self.records[slot as usize] = Some(record);
        self.by_id.insert(record.id, slot);
        Some(slot)
    }

    /// Removes `id`; returns its slot and record.
    pub fn remove(&mut self, id: u64) -> Option<(u32, ChunkRecord)> {
        let slot = self.by_id.remove(&id)?;
        let record = self.records[slot as usize].take()?;
        self.free.insert(slot);
        // keep `records.len()` = highest live slot + 1
        while self.records.last().is_some_and(Option::is_none) {
            self.records.pop();
            self.free.remove(&(self.records.len() as u32));
        }
        Some((slot, record))
    }

    /// Removes every record (the caller resets the arena).
    pub fn clear(&mut self) {
        self.records.clear();
        self.free.clear();
        self.by_id.clear();
    }

    /// Live records in slot order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &ChunkRecord)> {
        self.records
            .iter()
            .enumerate()
            .filter_map(|(slot, r)| r.as_ref().map(|r| (slot as u32, r)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::ArenaAllocator;

    fn record(arena: &mut ArenaAllocator, id: u64) -> ChunkRecord {
        let mut counts = [0; BUCKET_COUNT];
        counts[0] = 2;
        counts[6] = 1;
        ChunkRecord {
            id,
            origin: I64Vec3::new(id as i64 * 32, 0, 0),
            alloc: arena.allocate(3).expect("arena"),
            counts,
        }
    }

    #[test]
    fn slots_are_dense_and_reused_lowest_first() {
        let mut arena = ArenaAllocator::new(1024, 64);
        let mut table = ChunkTable::new(8);
        for id in 0..4 {
            assert_eq!(table.insert(record(&mut arena, id)), Some(id as u32));
        }
        assert_eq!((table.len(), table.slot_count()), (4, 4));
        assert_eq!(table.remove(2).map(|(s, _)| s), Some(2));
        assert_eq!(table.remove(1).map(|(s, _)| s), Some(1));
        assert_eq!(table.slot_count(), 4, "slot 3 is still live");
        assert_eq!(table.insert(record(&mut arena, 10)), Some(1));
        assert_eq!(table.insert(record(&mut arena, 11)), Some(2));
        assert_eq!(table.insert(record(&mut arena, 12)), Some(4));
        assert!(table.remove(99).is_none());
    }

    #[test]
    fn existing_ids_keep_their_slot() {
        let mut arena = ArenaAllocator::new(1024, 64);
        let mut table = ChunkTable::new(8);
        table.insert(record(&mut arena, 5));
        table.insert(record(&mut arena, 6));
        let mut updated = record(&mut arena, 5);
        updated.origin = I64Vec3::new(-64, 32, 96);
        assert_eq!(table.insert(updated), Some(0));
        assert_eq!(table.len(), 2);
        let (slot, r) = table.get(5).expect("record");
        assert_eq!((slot, r.origin), (0, I64Vec3::new(-64, 32, 96)));
    }

    #[test]
    fn slot_count_shrinks_when_the_last_slots_go() {
        let mut arena = ArenaAllocator::new(1024, 64);
        let mut table = ChunkTable::new(8);
        for id in 0..4 {
            table.insert(record(&mut arena, id));
        }
        table.remove(1);
        table.remove(3);
        assert_eq!(table.slot_count(), 3);
        table.remove(2);
        assert_eq!(table.slot_count(), 1, "trailing free slots are trimmed");
        assert_eq!(table.insert(record(&mut arena, 7)), Some(1));
        let slots: Vec<u32> = table.iter().map(|(s, _)| s).collect();
        assert_eq!(slots, [0, 1]);
        table.clear();
        assert_eq!((table.len(), table.slot_count()), (0, 0));
    }

    #[test]
    fn a_full_table_refuses_new_ids_but_not_existing_ones() {
        let mut arena = ArenaAllocator::new(1024, 64);
        let mut table = ChunkTable::new(2);
        table.insert(record(&mut arena, 0));
        table.insert(record(&mut arena, 1));
        assert_eq!(table.slot_for(2), None);
        assert_eq!(table.insert(record(&mut arena, 2)), None);
        assert_eq!(table.slot_for(1), Some(1));
        table.remove(0);
        assert_eq!(table.slot_for(2), Some(0));
    }

    #[test]
    fn records_mirror_to_chunk_gpu() {
        let mut arena = ArenaAllocator::new(1024, 64);
        let _gap = arena.allocate(10).expect("gap");
        let r = record(&mut arena, 3);
        let gpu = r.gpu();
        assert_eq!(gpu.origin, [96, 0, 0]);
        assert_eq!(gpu.quad_base, r.alloc.offset);
        assert_eq!(gpu.counts, r.counts);
        assert_eq!(gpu.flags, ChunkGpu::FLAG_LIVE);
        assert_eq!(gpu.pad, [0; 3]);
    }

    #[test]
    fn far_origins_upload_their_low_32_bits() {
        let mut arena = ArenaAllocator::new(1024, 64);
        let mut r = record(&mut arena, 0);
        // 2^31 wraps to i32::MIN, -2^31 - 32 to i32::MAX - 31, 2^40 + 64 to 64.
        r.origin = I64Vec3::new(1 << 31, -(1 << 31) - 32, (1 << 40) + 64);
        assert_eq!(r.gpu().origin, [i32::MIN, i32::MAX - 31, 64]);
        assert_eq!(r.gpu_origin(), IVec3::from_array(r.gpu().origin));
    }
}
