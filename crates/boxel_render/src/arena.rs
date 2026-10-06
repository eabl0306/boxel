//! Quad arena: one `STORAGE | VERTEX | COPY_DST` buffer of `PackedQuads`, sub-allocated in quad
//! units with `offset_allocator` (one allocation per chunk, all 7 buckets contiguous).
use boxel_mesh::PackedQuad;
use offset_allocator::{Allocation, Allocator};

/// Bytes per quad.
pub(crate) const QUAD_BYTES: u64 = size_of::<PackedQuad>() as u64;

/// One chunk's quad range.
#[derive(Clone, Copy)]
pub(crate) struct ArenaAlloc {
    /// First quad.
    pub offset: u32,
    /// Quads of the mesh stored here.
    pub size: u32,
    /// Quads held in the arena (`>= size`; larger only after an in-place `replace`).
    reserved: u32,
    handle: Allocation,
}

impl std::fmt::Debug for ArenaAlloc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArenaAlloc")
            .field("offset", &self.offset)
            .field("size", &self.size)
            .field("reserved", &self.reserved)
            .finish_non_exhaustive()
    }
}

/// CPU side of the arena (no GPU, unit-tested).
pub(crate) struct ArenaAllocator {
    allocator: Allocator,
    capacity: u32,
    used: u32,
}

impl ArenaAllocator {
    /// `capacity` quads. `max_allocs` bounds the allocator nodes: live allocations and free
    /// fragments both use one, and one is kept in reserve. For `n` resident chunks pass
    /// `2 * n + 4`: `replace` briefly holds one extra allocation.
    pub fn new(capacity: u32, max_allocs: u32) -> Self {
        Self {
            allocator: Allocator::with_max_allocs(capacity, max_allocs.max(2)),
            capacity,
            used: 0,
        }
    }

    /// Capacity in quads.
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Quads held by live allocations (reserved ranges, see `replace`).
    pub fn used(&self) -> u32 {
        self.used
    }

    /// Allocates `size > 0` quads.
    pub fn allocate(&mut self, size: u32) -> Option<ArenaAlloc> {
        if size == 0 || size > self.capacity - self.used {
            return None;
        }
        let handle = self.allocator.allocate(size)?;
        self.used += size;
        Some(ArenaAlloc {
            offset: handle.offset,
            size,
            reserved: size,
            handle,
        })
    }

    /// Releases an allocation (each allocation exactly once: the allocator asserts it).
    pub fn free(&mut self, alloc: ArenaAlloc) {
        self.allocator.free(alloc.handle);
        self.used -= alloc.reserved;
    }

    /// Replaces `old` (if any) with a range of `size > 0` quads. The new range is allocated
    /// while `old` is still held and `old` is freed only on success, so a failure never loses
    /// it. When no other range fits but `size` fits in `old`'s reserved range, that range is
    /// reused in place (the slack stays reserved until the next `replace` or `free`).
    ///
    /// `None` means nothing changed: `old` (if any) is still allocated and its GPU data is
    /// untouched, so the caller keeps the old mesh.
    ///
    /// Freeing first and re-allocating on failure does not work: `offset_allocator` rounds a
    /// request up to a size bin but files a freed range under the bin below, so a freed range
    /// with live neighbors is usually not found again at its own offset.
    pub fn replace(&mut self, old: Option<ArenaAlloc>, size: u32) -> Option<ArenaAlloc> {
        if let Some(new) = self.allocate(size) {
            if let Some(old) = old {
                self.free(old);
            }
            return Some(new);
        }
        let old = old?;
        (size > 0 && size <= old.reserved).then_some(ArenaAlloc { size, ..old })
    }

    /// Frees everything.
    pub fn reset(&mut self) {
        self.allocator.reset();
        self.used = 0;
    }
}

/// The GPU buffer plus its allocator.
pub(crate) struct QuadArena {
    buffer: wgpu::Buffer,
    /// Allocator over the buffer, in quads.
    pub alloc: ArenaAllocator,
}

impl QuadArena {
    /// Creates the buffer (`capacity * 8` bytes).
    pub fn new(device: &wgpu::Device, capacity: u32, max_allocs: u32) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel quad arena"),
            size: u64::from(capacity) * QUAD_BYTES,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            alloc: ArenaAllocator::new(capacity, max_allocs),
        }
    }

    /// The arena buffer.
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_and_free_track_usage() {
        let mut arena = ArenaAllocator::new(1024, 16);
        assert_eq!((arena.capacity(), arena.used()), (1024, 0));
        let a = arena.allocate(100).expect("a");
        let b = arena.allocate(200).expect("b");
        assert_eq!(arena.used(), 300);
        assert!(a.offset + a.size <= b.offset || b.offset + b.size <= a.offset);
        arena.free(a);
        assert_eq!(arena.used(), 200);
        arena.free(b);
        assert_eq!(arena.used(), 0);
        assert!(arena.allocate(0).is_none(), "empty allocations are refused");
    }

    #[test]
    fn freed_space_is_reused() {
        let mut arena = ArenaAllocator::new(1024, 16);
        let a = arena.allocate(512).expect("a");
        let b = arena.allocate(512).expect("b");
        assert!(arena.allocate(1).is_none(), "full");
        arena.free(a);
        let c = arena.allocate(512).expect("reuses a's range");
        assert_eq!(c.offset, a.offset);
        arena.free(b);
        arena.free(c);
        assert_eq!(arena.allocate(1024).expect("whole arena").offset, 0);
    }

    #[test]
    fn full_arena_and_node_limit_refuse_allocations() {
        let mut arena = ArenaAllocator::new(256, 64);
        assert!(arena.allocate(257).is_none());
        let all = arena.allocate(256).expect("exact fit");
        assert!(arena.allocate(1).is_none());
        arena.free(all);
        // node limit: with max_allocs = 4 only a few allocations fit despite free space
        let mut few = ArenaAllocator::new(1 << 20, 4);
        let live: Vec<_> = std::iter::from_fn(|| few.allocate(16)).take(10).collect();
        assert!(
            !live.is_empty() && live.len() < 4,
            "{} allocations",
            live.len()
        );
    }

    /// Allocates whatever is left until nothing fits.
    fn fill(arena: &mut ArenaAllocator) -> Vec<ArenaAlloc> {
        let mut fillers = Vec::new();
        while let Some(alloc) = (1..=arena.capacity() - arena.used())
            .rev()
            .find_map(|n| arena.allocate(n))
        {
            fillers.push(alloc);
        }
        fillers
    }

    #[test]
    fn replace_moves_to_a_new_range_and_frees_the_old_one() {
        let mut arena = ArenaAllocator::new(1024, 16);
        let old = arena.allocate(256).expect("old");
        let other = arena.allocate(256).expect("other");
        assert_eq!((old.offset, other.offset), (0, 256));
        let grown = arena.replace(Some(old), 300).expect("grown");
        assert_eq!((grown.offset, grown.size), (512, 300));
        assert_eq!(arena.used(), 556);
        // the old range is free again
        let reuse = arena.allocate(256).expect("old range");
        assert_eq!(reuse.offset, 0);
        // a new chunk that does not fit changes nothing
        assert!(arena.replace(None, 1000).is_none());
        assert_eq!(arena.used(), 812);
        for alloc in [grown, other, reuse] {
            arena.free(alloc);
        }
        assert_eq!(arena.used(), 0);
        assert_eq!(arena.allocate(1024).expect("whole arena").offset, 0);
    }

    #[test]
    fn replace_prefers_a_tight_range_over_reusing_the_old_one() {
        let mut arena = ArenaAllocator::new(2048, 16);
        let old = arena.allocate(1000).expect("old");
        let shrunk = arena.replace(Some(old), 10).expect("shrunk");
        assert_eq!(shrunk.size, 10);
        assert_eq!(
            arena.used(),
            10,
            "the old range was freed, not kept as slack"
        );
        arena.free(shrunk);
        assert_eq!(arena.used(), 0);
    }

    /// Full arena, both neighbors of the old range live: growing fails and leaves the old
    /// range allocated, shrinking reuses it in place. Most of these sizes are not size-bin
    /// boundaries, where freeing first and re-allocating lost the old range.
    #[test]
    fn replace_keeps_the_old_range_when_the_arena_is_full() {
        for size in [8, 17, 100, 256, 300, 1000, 1500, 5000] {
            let mut arena = ArenaAllocator::new(5 * size - 1, 64);
            let live: Vec<_> = (0..4)
                .map(|_| arena.allocate(size).expect("setup"))
                .collect();
            let fillers = fill(&mut arena);
            let old = live[1];
            let used = arena.used();
            assert!(arena.replace(Some(old), 2 * size).is_none(), "size {size}");
            assert_eq!(arena.used(), used, "size {size}: nothing changed");
            // `old` is still held, so shrinking reuses it in place
            let shrunk = arena
                .replace(Some(old), size - 3)
                .unwrap_or_else(|| panic!("size {size}: shrink in place"));
            assert_eq!((shrunk.offset, shrunk.size), (old.offset, size - 3));
            assert_eq!(arena.used(), used, "size {size}: the slack stays reserved");
            // and grows back up to the reserved size, still in place
            let regrown = arena.replace(Some(shrunk), size).expect("regrow");
            assert_eq!((regrown.offset, regrown.size), (old.offset, size));
            assert!(arena.replace(Some(regrown), size + 1).is_none());
            // freeing it releases the whole reserved range
            arena.free(regrown);
            assert_eq!(arena.used(), used - size);
            for alloc in [live[0], live[2], live[3]].into_iter().chain(fillers) {
                arena.free(alloc);
            }
            assert_eq!(arena.used(), 0);
        }
    }

    /// `n` live ranges between `n + 1` free ones is the most fragmented layout (`2n + 1`
    /// nodes); `replace` adds a temporary allocation on top, which `2n + 4` nodes allow.
    #[test]
    fn replace_has_enough_allocator_nodes() {
        let n = 8;
        let mut arena = ArenaAllocator::new(1 << 16, 2 * n + 4);
        let all: Vec<_> = (0..2 * n + 1)
            .map(|_| arena.allocate(64).expect("setup"))
            .collect();
        let mut live = Vec::new();
        for (i, alloc) in all.into_iter().enumerate() {
            if i % 2 == 0 {
                arena.free(alloc);
            } else {
                live.push(alloc);
            }
        }
        assert_eq!(live.len() as u32, n);
        for alloc in &mut live {
            *alloc = arena.replace(Some(*alloc), 32).expect("replace");
        }
        for alloc in live {
            arena.free(alloc);
        }
        assert_eq!(arena.used(), 0);
    }

    #[test]
    fn reset_frees_everything() {
        let mut arena = ArenaAllocator::new(1024, 16);
        let _a = arena.allocate(1000).expect("a");
        arena.reset();
        assert_eq!(arena.used(), 0);
        assert!(arena.allocate(1024).is_some());
    }
}
