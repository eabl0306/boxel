//! Hooks for `benches/chunk_memory.rs` (ADR 0035): the CPU halves of chunk memory without a GPU.
//! Not API.
use crate::ChunkMemorySettings;
use crate::arena::ArenaAllocator;
use crate::chunk_table::{ChunkRecord, ChunkTable};
use crate::memory::{
    ChunkMemory, CopySink, MemOp, MemoryLayout, PageClass, PagedAlloc, bucket_counts,
    upsert_resident,
};
use crate::path::CpuCull;
use crate::renderer::ViewParams;
use crate::upload::{UploadKinds, Uploads, arena_len, light_entries};
use boxel_math::I64Vec3;
use boxel_mesh::ChunkMesh;
use std::sync::Arc;

/// The CPU cull of Mid and Low over a table of resident chunks.
pub struct CullBench {
    table: ChunkTable,
    cull: CpuCull,
    /// Pages holding chunks.
    pub pages: usize,
}

impl CullBench {
    /// `chunks` placed as the renderer places them, in pages of `settings`, all of the
    /// floor created up front.
    pub fn placed(chunks: &[(I64Vec3, Arc<ChunkMesh>)], settings: &ChunkMemorySettings) -> Self {
        let layout = MemoryLayout::resolve(settings, facts());
        let mut memory = ChunkMemory::with_floor(&layout);
        let mut table = ChunkTable::new(layout.slots);
        let mut uploads = Uploads::default();
        for (id, (origin, mesh)) in chunks.iter().enumerate() {
            let _ = upsert_resident(
                &mut memory,
                &mut table,
                &mut uploads,
                id as u64,
                *origin,
                mesh,
            );
        }
        let pages = memory.pages().filter(|(_, p)| p.chunks > 0).count();
        Self {
            table,
            cull: CpuCull::default(),
            pages,
        }
    }

    /// The same chunks with slot `s` in page `s % pages`: neighbors never share a page (the
    /// worst case for the per-page lists).
    pub fn scattered(chunks: &[(I64Vec3, Arc<ChunkMesh>)], pages: u16) -> Self {
        let mut arenas: Vec<ArenaAllocator> = (0..pages)
            .map(|_| ArenaAllocator::new(u32::MAX / 2, 2 * chunks.len() as u32 + 4))
            .collect();
        let mut table = ChunkTable::new(chunks.len() as u32 + 1);
        for (id, (origin, mesh)) in chunks.iter().enumerate() {
            let page = (id % usize::from(pages)) as u16;
            let Some(alloc) = arenas[usize::from(page)].allocate(arena_len(mesh)) else {
                continue;
            };
            table.insert(ChunkRecord {
                id: id as u64,
                origin: *origin,
                alloc: PagedAlloc { page, alloc },
                counts: bucket_counts(mesh),
                light_base: alloc.offset + mesh.total_quads(),
                light_len: light_entries(mesh),
            });
        }
        Self {
            table,
            cull: CpuCull::default(),
            pages: usize::from(pages),
        }
    }

    /// Resident chunks.
    pub fn chunks(&self) -> usize {
        self.table.len()
    }

    /// One frame's cull; the draws it made.
    pub fn run(&mut self, view: &ViewParams) -> usize {
        self.table.follow(view.render_origin);
        self.cull.run(&self.table, view, None);
        self.cull.solid_len() + self.cull.translucent.len()
    }
}

/// Placement in the pages: chunks placed, remeshed and removed.
pub struct PlaceBench {
    memory: ChunkMemory,
    live: Vec<Option<PagedAlloc>>,
}

impl PlaceBench {
    /// The floor of `settings`, `slots` chunk ids.
    pub fn new(settings: &ChunkMemorySettings, slots: usize) -> Self {
        let layout = MemoryLayout::resolve(settings, facts());
        Self {
            memory: ChunkMemory::with_floor(&layout),
            live: vec![None; slots],
        }
    }

    /// Places (or remeshes) chunk `id` with `size` quads; false when refused.
    pub fn place(&mut self, id: usize, size: u32, translucent: bool) -> bool {
        let class = if translucent {
            PageClass::Translucent
        } else {
            PageClass::Solid
        };
        let old = self.live[id];
        match self.memory.place(old, size, class) {
            Some(new) => {
                self.live[id] = Some(new);
                true
            }
            None => false,
        }
    }

    /// Removes chunk `id`.
    pub fn remove(&mut self, id: usize) {
        if let Some(a) = self.live[id].take() {
            self.memory.free(a);
        }
    }

    /// Quads in use.
    pub fn used(&self) -> u64 {
        self.memory.used()
    }
}

/// A frame's upload work up to the copies: placement, then the writes `Uploads::flush` makes
/// (into a sink that only counts them).
pub struct FlushBench {
    memory: ChunkMemory,
    table: ChunkTable,
    uploads: Uploads,
    sink: CountSink,
}

/// Counts the writes and their bytes.
struct CountSink {
    writes: usize,
    bytes: u64,
}

impl CopySink for CountSink {
    fn max_batch(&self) -> u64 {
        64 << 20
    }

    fn writes(
        &mut self,
        _bytes: u64,
        _fail: bool,
        fill: &mut dyn FnMut(&mut dyn FnMut(MemOp<'_>)),
    ) -> bool {
        let (writes, bytes) = (&mut self.writes, &mut self.bytes);
        fill(&mut |op| match op {
            MemOp::Write { data, .. } => {
                *writes += 1;
                *bytes += data.len() as u64;
            }
        });
        true
    }
}

impl FlushBench {
    /// The floor of `settings`.
    pub fn new(settings: &ChunkMemorySettings) -> Self {
        let layout = MemoryLayout::resolve(settings, facts());
        Self {
            memory: ChunkMemory::with_floor(&layout),
            table: ChunkTable::new(layout.slots),
            uploads: Uploads::default(),
            sink: CountSink {
                writes: 0,
                bytes: 0,
            },
        }
    }

    /// Upserts `meshes` (ids from `first`), then flushes them as Mid would; the writes made.
    pub fn frame(&mut self, first: u64, meshes: &[(I64Vec3, Arc<ChunkMesh>)]) -> usize {
        for (i, (origin, mesh)) in meshes.iter().enumerate() {
            let _ = upsert_resident(
                &mut self.memory,
                &mut self.table,
                &mut self.uploads,
                first + i as u64,
                *origin,
                mesh,
            );
        }
        let before = self.sink.writes;
        let kinds = UploadKinds {
            table: true,
            low_light: false,
        };
        self.uploads.flush(&mut self.sink, &mut self.table, kinds);
        self.sink.writes - before
    }

    /// Removes every chunk.
    pub fn clear(&mut self) {
        self.table.clear();
        self.memory.reset();
        self.uploads.clear();
    }
}

/// A discrete GPU with large buffers.
fn facts() -> crate::memory::DeviceFacts {
    crate::memory::DeviceFacts {
        page_limit: (128 << 20) / 8,
        slot_limit: crate::memory::MAX_SLOTS,
        class: wgpu::DeviceType::DiscreteGpu,
    }
}
