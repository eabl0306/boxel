//! Chunk slots: `ChunkGpu` storage table (High/Mid) mirrored on the CPU (all tiers, CPU culling).
//!
//! The GPU gets the low 32 bits of each chunk origin, and the shaders subtract them from the
//! (equally wrapped) render origin in `i32`: exact only for chunks nearer than `2^31` voxels. A
//! chunk still loaded farther away (the old area for a few frames after a teleport, the area at
//! the world limit seen from a camera past it) would wrap to a position near the camera. So only
//! chunks inside a window around the camera ([`GPU_WINDOW`]) are drawn, on every tier: they are
//! the ones flagged live in the GPU table (the High cull skips the rest) and the only ones the
//! CPU cull passes (Mid, Low). The window follows the render origin ([`ChunkTable::follow`]).
use crate::arena::ArenaAlloc;
use boxel_math::{I64Vec3, IVec3, wrap_to_i32};
use boxel_mesh::BUCKET_COUNT;
use boxel_shaders::gpu::ChunkGpu;
use std::collections::{BTreeSet, HashMap};

/// Half width of the GPU window, in voxels per axis: a chunk whose origin is nearer than this to
/// the window center on every axis can be drawn (flagged live in the GPU table, passed by the
/// CPU cull); the rest stay resident but hidden.
pub(crate) const GPU_WINDOW: u64 = 1 << 30;

/// How far (voxels, on any axis) the render origin may move from the window center before the
/// window follows it and every live flag is uploaded again.
pub(crate) const GPU_WINDOW_SLACK: u64 = 1 << 20;

// A chunk inside the window is less than `GPU_WINDOW + GPU_WINDOW_SLACK` voxels from the render
// origin on every axis, so the GPU's wrapping `i32` subtraction is exact for it.
const _: () = assert!(GPU_WINDOW + GPU_WINDOW_SLACK < 1 << 31);

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

    /// The GPU mirror of this record, flagged live if `live` ([`ChunkTable::gpu_entry`] decides).
    pub fn gpu(&self, live: bool) -> ChunkGpu {
        ChunkGpu {
            origin: self.gpu_origin().to_array(),
            quad_base: self.alloc.offset,
            counts: self.counts,
            flags: if live { ChunkGpu::FLAG_LIVE } else { 0 },
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
    /// Center of the GPU window (a recent render origin).
    window: I64Vec3,
}

impl ChunkTable {
    /// Empty table of `capacity` slots.
    pub fn new(capacity: u32) -> Self {
        Self {
            records: Vec::new(),
            free: BTreeSet::new(),
            by_id: HashMap::new(),
            capacity,
            window: I64Vec3::ZERO,
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

    /// True if a chunk at `origin` is inside the GPU window (nearer than [`GPU_WINDOW`] to its
    /// center on every axis). Exact for any `i64` positions.
    pub fn in_window(&self, origin: I64Vec3) -> bool {
        let c = self.window;
        origin.x.abs_diff(c.x) < GPU_WINDOW
            && origin.y.abs_diff(c.y) < GPU_WINDOW
            && origin.z.abs_diff(c.z) < GPU_WINDOW
    }

    /// The GPU entry of `record`: live only inside the window.
    pub fn gpu_entry(&self, record: &ChunkRecord) -> ChunkGpu {
        record.gpu(self.in_window(record.origin))
    }

    /// Re-centers the window on `render_origin` when that is more than [`GPU_WINDOW_SLACK`] from
    /// the center on some axis. Returns true if the window moved: live flags may have changed,
    /// and the caller uploads [`ChunkTable::gpu_entries`] before the High cull runs. Called every
    /// frame before culling, so the render origin is never farther than the slack from the
    /// center.
    pub fn follow(&mut self, render_origin: I64Vec3) -> bool {
        let c = self.window;
        let moved = render_origin.x.abs_diff(c.x) > GPU_WINDOW_SLACK
            || render_origin.y.abs_diff(c.y) > GPU_WINDOW_SLACK
            || render_origin.z.abs_diff(c.z) > GPU_WINDOW_SLACK;
        if moved {
            self.window = render_origin;
        }
        moved
    }

    /// GPU entries of slots `0..slot_count()` (free slots zeroed), for a full upload.
    pub fn gpu_entries(&self) -> Vec<ChunkGpu> {
        self.records
            .iter()
            .map(|r| {
                r.as_ref()
                    .map_or(ChunkGpu::default(), |r| self.gpu_entry(r))
            })
            .collect()
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
        let gpu = r.gpu(true);
        assert_eq!(gpu.origin, [96, 0, 0]);
        assert_eq!(gpu.quad_base, r.alloc.offset);
        assert_eq!(gpu.counts, r.counts);
        assert_eq!(gpu.flags, ChunkGpu::FLAG_LIVE);
        assert_eq!(gpu.pad, [0; 3]);
        assert_eq!(r.gpu(false), ChunkGpu { flags: 0, ..gpu });
    }

    #[test]
    fn far_origins_upload_their_low_32_bits() {
        let mut arena = ArenaAllocator::new(1024, 64);
        let mut r = record(&mut arena, 0);
        // 2^31 wraps to i32::MIN, -2^31 - 32 to i32::MAX - 31, 2^40 + 64 to 64.
        r.origin = I64Vec3::new(1 << 31, -(1 << 31) - 32, (1 << 40) + 64);
        assert_eq!(r.gpu(true).origin, [i32::MIN, i32::MAX - 31, 64]);
        assert_eq!(r.gpu_origin(), IVec3::from_array(r.gpu(true).origin));
    }

    /// Live flag of every slot, in slot order.
    fn live(table: &ChunkTable) -> Vec<bool> {
        table
            .gpu_entries()
            .iter()
            .map(|e| e.flags == ChunkGpu::FLAG_LIVE)
            .collect()
    }

    #[test]
    fn only_chunks_near_the_camera_are_live_on_the_gpu() {
        let mut arena = ArenaAllocator::new(1024, 64);
        let mut table = ChunkTable::new(8);
        let edge = GPU_WINDOW as i64;
        let origins = [
            I64Vec3::ZERO,
            I64Vec3::new(edge - 32, -(edge - 32), 0),
            I64Vec3::new(0, edge, 0),
            // The old area after a 2^32-voxel teleport, which wraps onto the new one.
            I64Vec3::new(1 << 32, 0, -(1 << 32)),
            I64Vec3::new(i64::MIN, 0, i64::MAX),
        ];
        for (id, origin) in origins.into_iter().enumerate() {
            let mut r = record(&mut arena, id as u64);
            r.origin = origin;
            table.insert(r);
        }
        assert_eq!(live(&table), [true, true, false, false, false]);
        let (_, r) = table.get(3).expect("record");
        assert_eq!(
            table.gpu_entry(r).origin,
            [0; 3],
            "wraps to the world origin"
        );

        // Moves within the slack keep the window (and the flags).
        assert!(!table.follow(I64Vec3::new(1000, -(1 << 20), 1 << 20)));
        assert!(table.follow(I64Vec3::new(0, 0, (1 << 20) + 1)));
        assert_eq!(live(&table), [true, true, false, false, false]);

        // A teleport: the window follows and the area left behind goes dark.
        assert!(table.follow(I64Vec3::new((1 << 32) + 7, 3, -(1 << 32) - 9)));
        assert_eq!(live(&table), [false, false, false, true, false]);
        // Coming back makes the old chunks live again.
        assert!(table.follow(I64Vec3::new(16, -16, 16)));
        assert_eq!(live(&table), [true, true, false, false, false]);
        // At the ends of i64 (a camera far past the world limit) nothing overflows.
        assert!(table.follow(I64Vec3::new(i64::MIN + 5, 0, i64::MAX)));
        assert_eq!(live(&table), [false, false, false, false, true]);

        // Free slots upload zeroed entries.
        table.remove(1);
        let entries = table.gpu_entries();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[1], ChunkGpu::default());
    }

    #[test]
    fn live_chunks_have_exact_wrapped_differences() {
        // Wherever the camera goes, a chunk flagged live has an exact wrapping `i32` difference
        // to the render origin (what `cull.wgsl` and the vertex shaders compute).
        let mut arena = ArenaAllocator::new(1 << 16, 1024);
        let mut table = ChunkTable::new(64);
        let base = I64Vec3::new((1 << 31) - 64, -(1 << 31), 1 << 40);
        let mut origins = Vec::new();
        for (id, k) in (-6i64..=6).enumerate() {
            let mut r = record(&mut arena, id as u64);
            r.origin = base + I64Vec3::new(k << 28, k * 32, -(k << 29));
            origins.push(r.origin);
            table.insert(r);
        }
        // Camera positions relative to `base` and the chunks (`k`) live there.
        for (camera, expected) in [
            (I64Vec3::ZERO, vec![-1, 0, 1]),
            // within the slack: the window stays
            (I64Vec3::new(1 << 20, -(1 << 20), 1 << 20), vec![-1, 0, 1]),
            (I64Vec3::new(1 << 30, 0, -(1 << 31)), vec![3, 4, 5]),
            (
                I64Vec3::new((1 << 30) + (1 << 20), 1 << 20, -(1 << 31) - (1 << 20)),
                vec![3, 4, 5],
            ),
            (
                I64Vec3::new(-(1 << 30) - (1 << 20), 0, 1 << 31),
                vec![-5, -4, -3],
            ),
            (I64Vec3::new(0, 1 << 31, 0), vec![]),
            (I64Vec3::ZERO, vec![-1, 0, 1]),
        ] {
            let camera = base + camera;
            table.follow(camera);
            let mut live = Vec::new();
            for ((entry, origin), k) in table.gpu_entries().iter().zip(&origins).zip(-6i64..) {
                if entry.flags == ChunkGpu::FLAG_LIVE {
                    live.push(k);
                    let wrapped = IVec3::from_array(entry.origin).wrapping_sub(wrap_to_i32(camera));
                    assert_eq!(
                        wrapped.as_i64vec3(),
                        *origin - camera,
                        "{origin} from {camera}"
                    );
                }
            }
            assert_eq!(live, expected, "camera at {camera}");
        }
    }
}
