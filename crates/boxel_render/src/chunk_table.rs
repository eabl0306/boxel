//! Chunk slots: `ChunkGpu` storage table (High/Mid) mirrored on the CPU (all tiers, CPU culling).
//!
//! The GPU gets the low 32 bits of each chunk origin, and the shaders subtract them from the
//! (equally wrapped) render origin in `i32`: exact only for chunks nearer than `2^31` voxels. A
//! chunk still loaded farther away (the old area for a few frames after a teleport, the area at
//! the world limit seen from a camera past it) would wrap to a position near the camera. So only
//! chunks inside a window around the camera ([`GPU_WINDOW`]) are drawn, on every tier: they are
//! the ones flagged live in the GPU table (the High cull skips the rest) and the only ones the
//! CPU cull passes (Mid, Low). The window follows the render origin ([`ChunkTable::follow`]).
//!
//! The table also tracks which slots changed since the GPU copy was last brought up to date
//! ([`ChunkTable::take_stale_runs`]), so the renderer uploads them once per frame in a few
//! contiguous runs instead of one write per change.
use crate::arena::ArenaAlloc;
use boxel_math::{I64Vec3, IVec3, wrap_to_i32};
use boxel_mesh::BUCKET_COUNT;
use boxel_shaders::gpu::ChunkGpu;
use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

/// Half width of the GPU window, in voxels per axis: a chunk whose origin is nearer than this to
/// the window center on every axis can be drawn (flagged live in the GPU table, passed by the
/// CPU cull); the rest stay resident but hidden.
pub(crate) const GPU_WINDOW: u64 = 1 << 30;

/// How far (voxels, on any axis) the render origin may move from the window center before the
/// window follows it (and High uploads every live flag again).
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
///
/// The GPU copy of the table (High/Mid) matches [`ChunkTable::gpu_entry_at`] for every slot except
/// the stale ones: the slots changed since the last [`ChunkTable::take_stale_runs`], and every slot
/// below `stale_prefix`.
pub(crate) struct ChunkTable {
    records: Vec<Option<ChunkRecord>>,
    free: BTreeSet<u32>,
    by_id: HashMap<u64, u32, foldhash::fast::FixedState>,
    capacity: u32,
    /// Center of the GPU window (a recent render origin).
    window: I64Vec3,
    /// Records inside the window.
    drawable: usize,
    /// Slots changed since the last upload, each once, in no order.
    stale: Vec<u32>,
    /// One bit per slot, set while the slot is in `stale`.
    stale_bits: Vec<u64>,
    /// Slots `0..stale_prefix` are all stale (after `clear` or a window move).
    stale_prefix: u32,
}

impl ChunkTable {
    /// Empty table of `capacity` slots.
    pub fn new(capacity: u32) -> Self {
        Self {
            records: Vec::new(),
            free: BTreeSet::new(),
            by_id: HashMap::default(),
            capacity,
            window: I64Vec3::ZERO,
            drawable: 0,
            stale: Vec::new(),
            stale_bits: vec![0; capacity.div_ceil(64) as usize],
            stale_prefix: 0,
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

    /// Live chunks inside the GPU window: the ones a tier may draw. The rest stay resident
    /// (the old area for a few frames after a teleport) but take no draws.
    pub fn drawable_len(&self) -> usize {
        self.drawable
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
        let old = self.records[slot as usize].replace(record);
        if old.is_some_and(|r| self.in_window(r.origin)) {
            self.drawable -= 1;
        }
        if self.in_window(record.origin) {
            self.drawable += 1;
        }
        self.by_id.insert(record.id, slot);
        self.mark_stale(slot);
        Some(slot)
    }

    /// Removes `id`; returns its slot and record.
    pub fn remove(&mut self, id: u64) -> Option<(u32, ChunkRecord)> {
        let slot = self.by_id.remove(&id)?;
        let record = self.records[slot as usize].take()?;
        self.free.insert(slot);
        // Its GPU entry must go to zero even past the new `slot_count`: High's cull reads the
        // flags of every slot below `slot_count`, and a slot freed here can be below it again.
        self.mark_stale(slot);
        if self.in_window(record.origin) {
            self.drawable -= 1;
        }
        // keep `records.len()` = highest live slot + 1
        while self.records.last().is_some_and(Option::is_none) {
            self.records.pop();
            self.free.remove(&(self.records.len() as u32));
        }
        Some((slot, record))
    }

    /// Removes every record (the caller resets the arena). The GPU entries of the old slots go
    /// to zero with the next upload.
    pub fn clear(&mut self) {
        self.mark_all_stale();
        self.records.clear();
        self.free.clear();
        self.by_id.clear();
        self.drawable = 0;
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
    /// the center on some axis. Returns true if the window moved: live flags may have changed, so
    /// every slot is marked stale and the next upload (before the cull runs) sends them all.
    /// Called every frame before culling, so the render origin is never farther than the slack
    /// from the center.
    pub fn follow(&mut self, render_origin: I64Vec3) -> bool {
        let c = self.window;
        let moved = render_origin.x.abs_diff(c.x) > GPU_WINDOW_SLACK
            || render_origin.y.abs_diff(c.y) > GPU_WINDOW_SLACK
            || render_origin.z.abs_diff(c.z) > GPU_WINDOW_SLACK;
        if moved {
            self.window = render_origin;
            self.drawable = self
                .iter()
                .filter(|(_, r)| self.in_window(r.origin))
                .count();
            self.mark_all_stale();
        }
        moved
    }

    /// The GPU entry of `slot`: its record's ([`ChunkTable::gpu_entry`]), zeroed when the slot is
    /// free (also past `slot_count`).
    pub fn gpu_entry_at(&self, slot: u32) -> ChunkGpu {
        match self.records.get(slot as usize) {
            Some(Some(record)) => self.gpu_entry(record),
            _ => ChunkGpu::default(),
        }
    }

    /// GPU entries of slots `0..slot_count()` (free slots zeroed).
    #[cfg(test)]
    pub fn gpu_entries(&self) -> Vec<ChunkGpu> {
        (0..self.slot_count())
            .map(|s| self.gpu_entry_at(s))
            .collect()
    }

    /// Marks every slot below `slot_count` stale: the next upload sends them all.
    fn mark_all_stale(&mut self) {
        self.stale_prefix = self.stale_prefix.max(self.slot_count());
    }

    /// True if no slot is stale.
    pub fn is_synced(&self) -> bool {
        self.stale.is_empty() && self.stale_prefix == 0
    }

    /// Sets `runs` to the sorted slot ranges to upload ([`ChunkTable::gpu_entry_at`] of each
    /// slot) so the GPU table matches again, and forgets them: every stale slot, with runs at most
    /// `merge_gap` up-to-date slots apart joined into one (sending those slots again costs less
    /// than another copy).
    pub fn take_stale_runs(&mut self, merge_gap: u32, runs: &mut Vec<Range<u32>>) {
        runs.clear();
        if self.stale_prefix > 0 {
            runs.push(0..self.stale_prefix);
        }
        self.stale.sort_unstable();
        for &slot in &self.stale {
            self.stale_bits[slot as usize / 64] &= !(1 << (slot % 64));
            match runs.last_mut() {
                Some(run) if slot < run.end => {}
                Some(run) if slot - run.end <= merge_gap => run.end = slot + 1,
                _ => runs.push(slot..slot + 1),
            }
        }
        self.stale.clear();
        self.stale_prefix = 0;
    }

    /// Records that the GPU entry of `slot` changed.
    fn mark_stale(&mut self, slot: u32) {
        let (word, bit) = (slot as usize / 64, 1 << (slot % 64));
        if self.stale_bits[word] & bit == 0 {
            self.stale_bits[word] |= bit;
            self.stale.push(slot);
        }
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

    /// `drawable_len` against a count of the records inside the window.
    fn assert_drawable(table: &ChunkTable, expected: usize) {
        let counted = table
            .iter()
            .filter(|(_, r)| table.in_window(r.origin))
            .count();
        assert_eq!((table.drawable_len(), counted), (expected, expected));
    }

    #[test]
    fn drawable_len_counts_only_the_chunks_inside_the_window() {
        let mut arena = ArenaAllocator::new(1024, 64);
        let mut table = ChunkTable::new(8);
        let far = I64Vec3::new(1 << 32, 0, 0);
        let far_record = |arena: &mut ArenaAllocator, id: u64| {
            let mut r = record(arena, id);
            r.origin += far;
            r
        };
        for id in 0..3 {
            table.insert(record(&mut arena, id));
        }
        assert_drawable(&table, 3);
        // The old area after a teleport: resident, not drawable.
        for id in 3..6 {
            table.insert(far_record(&mut arena, id));
        }
        assert_eq!(table.len(), 6);
        assert_drawable(&table, 3);
        // A replaced record can move out of the window or into it.
        table.insert(far_record(&mut arena, 0));
        assert_drawable(&table, 2);
        table.insert(record(&mut arena, 4));
        assert_drawable(&table, 3);
        table.remove(1);
        table.remove(5);
        assert_drawable(&table, 2);
        table.insert(far_record(&mut arena, 6));
        assert_drawable(&table, 2);
        // The window follows the camera: now the far records are the drawable ones.
        assert!(table.follow(far));
        assert_drawable(&table, 3);
        assert!(!table.follow(far + I64Vec3::new(1000, 0, 0)));
        assert_drawable(&table, 3);
        table.clear();
        assert_drawable(&table, 0);
    }

    /// Applies `take_stale_runs` to `gpu` (a GPU copy of the table, `capacity` entries) like the
    /// renderer does, checks the runs and returns them as `(start, end)`.
    fn sync(table: &mut ChunkTable, gpu: &mut [ChunkGpu], merge_gap: u32) -> Vec<(u32, u32)> {
        // left over from a previous call: cleared
        let mut runs = vec![7..9, 30..31];
        table.take_stale_runs(merge_gap, &mut runs);
        for pair in runs.windows(2) {
            assert!(
                pair[1].start - pair[0].end > merge_gap,
                "{runs:?} not sorted, apart and merged (gap {merge_gap})"
            );
        }
        for run in &runs {
            assert!(
                run.start < run.end && run.end <= table.capacity(),
                "{runs:?}"
            );
            for slot in run.clone() {
                gpu[slot as usize] = table.gpu_entry_at(slot);
            }
        }
        assert!(table.is_synced());
        runs.iter().map(|run| (run.start, run.end)).collect()
    }

    /// Every entry of the GPU copy matches the table (zero past `slot_count`).
    fn assert_mirrored(table: &ChunkTable, gpu: &[ChunkGpu]) {
        for (slot, entry) in gpu.iter().enumerate() {
            assert_eq!(*entry, table.gpu_entry_at(slot as u32), "slot {slot}");
        }
        assert!(
            gpu[table.slot_count() as usize..]
                .iter()
                .all(|e| *e == ChunkGpu::default())
        );
    }

    #[test]
    fn stale_runs_cover_changed_slots_and_merge_small_gaps() {
        let mut arena = ArenaAllocator::new(1024, 64);
        let mut table = ChunkTable::new(64);
        let mut gpu = vec![ChunkGpu::default(); 64];
        for id in 0..10 {
            table.insert(record(&mut arena, id));
        }
        assert_eq!(sync(&mut table, &mut gpu, 0), [(0, 10)]);
        assert_eq!(sync(&mut table, &mut gpu, 0), [], "nothing changed");
        for (gap, expected) in [
            (0, vec![(2, 4), (7, 8)]),
            (2, vec![(2, 4), (7, 8)]),
            (3, vec![(2, 8)]),
        ] {
            table.remove(2);
            table.remove(3);
            table.remove(7);
            assert_eq!(sync(&mut table, &mut gpu, gap), expected, "gap {gap}");
            assert_mirrored(&table, &gpu);
            for id in [2, 3, 7] {
                table.insert(record(&mut arena, id));
            }
            sync(&mut table, &mut gpu, 0);
        }
        // The last slot goes: its entry is zeroed past the new `slot_count`.
        table.remove(9);
        assert_eq!(table.slot_count(), 9);
        assert_eq!(sync(&mut table, &mut gpu, 0), [(9, 10)]);
        assert_mirrored(&table, &gpu);
        // `clear` zeroes every old slot, also one that went just before it.
        table.remove(8);
        table.clear();
        assert_eq!(sync(&mut table, &mut gpu, 0), [(0, 9)]);
        assert_mirrored(&table, &gpu);
        // A window move sends every slot again (the live flags change).
        for id in 0..4 {
            table.insert(record(&mut arena, id));
        }
        sync(&mut table, &mut gpu, 0);
        assert!(table.follow(I64Vec3::new(1 << 32, 0, 0)));
        assert_eq!(sync(&mut table, &mut gpu, 0), [(0, 4)]);
        assert!(gpu.iter().all(|e| e.flags == 0));
        assert!(!table.follow(I64Vec3::new(1 << 32, 5, 0)));
        assert_eq!(sync(&mut table, &mut gpu, 0), []);
    }

    #[test]
    fn the_gpu_copy_follows_any_sequence_of_changes() {
        // Pseudo-random inserts, replacements, removals, clears and window moves, synced at
        // random points with several merge gaps: the GPU copy always ends up equal to the table.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        for merge_gap in [0, 1, 4, 64] {
            let mut arena = ArenaAllocator::new(1 << 16, 1024);
            let mut table = ChunkTable::new(48);
            let mut gpu = vec![ChunkGpu::default(); 48];
            for _ in 0..4000 {
                match next(100) {
                    0..45 => {
                        let id = next(60);
                        if table.slot_for(id).is_some() {
                            let mut r = record(&mut arena, id);
                            r.origin.x += next(3) as i64 * (1 << 31);
                            if let Some((_, old)) = table.get(id) {
                                arena.free(old.alloc);
                            }
                            table.insert(r);
                        }
                    }
                    45..85 => {
                        if let Some((_, r)) = table.remove(next(60)) {
                            arena.free(r.alloc);
                        }
                    }
                    85..87 => {
                        table.clear();
                        arena.reset();
                    }
                    87..92 => {
                        table.follow(I64Vec3::new(next(3) as i64 * (1 << 31), 0, 0));
                    }
                    _ => {
                        sync(&mut table, &mut gpu, merge_gap);
                        assert_mirrored(&table, &gpu);
                    }
                }
            }
        }
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
