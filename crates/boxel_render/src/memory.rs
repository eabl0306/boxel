//! Chunk memory in pages (ADR 0035). Chunk quads live in several GPU buffers of a fixed size
//! (floor pages may be smaller), the pages, each sub-allocated by its own [`ArenaAllocator`]. A
//! chunk's whole range (its 8 buckets, then its light palette) sits in one page, so
//! `ChunkGpu::quad_base` and `light_base` are page-local and every draw binds its chunk's page.
//! On Low every page has a light page of the same size, parallel to it (ADR 0015).
//!
//! A page has a class: chunks with translucent quads go to Translucent pages, so the back to front
//! translucent pass, one order on every tier, seldom switches buffers. Placement is deterministic
//! and the same on every tier ([`ChunkMemory::place`]): the chunk's own page first, then the pages
//! of its class lowest id first, then its own range in place, then the other class. One page is
//! one High draw-list segment: a page holds at most `page_chunks` chunks, so its 6 face buckets
//! each fit one indirect list of [`PAGE_DRAWS_MAX`] draws.
//!
//! The renderer creates the floor pages at GPU start ([`MemoryLayout`]), enough of them for the
//! initial table's chunks (the floor's quads then go over smaller pages). The memory work emits
//! what it writes as [`MemOp`]s through a [`CopySink`]: the GPU sink stages them and records the
//! copies, a test sink replays them over byte models of the pages.
use crate::arena::{ArenaAlloc, ArenaAllocator, QUAD_BYTES};
use crate::chunk_table::{ChunkRecord, ChunkTable};
use crate::upload::{Uploads, arena_len, light_entries};
use boxel_gfx::Tier;
use boxel_math::I64Vec3;
use boxel_mesh::{BUCKET_COUNT, ChunkMesh, TRANSLUCENT_BUCKET};
use boxel_shaders::gpu::chunk_memory::{MAX_PAGES, PAGE_CHUNKS_MAX, PAGE_DRAWS_MAX};
use boxel_shaders::gpu::{ChunkGpu, DrawIndexedIndirect, DrawRef, bindings};
use std::sync::Arc;

/// Default page size in quads: 4 Mi quads, 32 MiB.
pub const DEFAULT_PAGE_QUADS: u32 = 4 << 20;
/// Smallest page size in quads.
pub const MIN_PAGE_QUADS: u32 = 4096;
/// Upper bound of chunk slots (keeps the allocator node table and the GPU table bounded).
pub(crate) const MAX_SLOTS: u32 = 1 << 20;
/// Bytes of one chunk table entry.
pub(crate) const CHUNK_GPU_BYTES: u64 = size_of::<ChunkGpu>() as u64;
/// Bytes of one indirect draw.
pub(crate) const ARGS_BYTES: u64 = size_of::<DrawIndexedIndirect>() as u64;
/// Bytes of one allocator node (a `u32` node and its free-list entry).
const NODE_BYTES: u64 = 32;

/// The most quads chunk memory may take ([`ChunkMemorySettings::max_quads`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MemoryCeiling {
    /// By adapter class: 192 Mi quads (1.5 GiB of pages) on a discrete GPU, 96 Mi (768 MiB) on
    /// an integrated one, 32 Mi (256 MiB) otherwise.
    #[default]
    Auto,
    /// This many quads.
    Quads(u32),
}

impl MemoryCeiling {
    /// Quads on an adapter of `class`.
    pub fn quads(self, class: wgpu::DeviceType) -> u32 {
        match self {
            Self::Quads(quads) => quads,
            Self::Auto => match class {
                wgpu::DeviceType::DiscreteGpu => 192 << 20,
                wgpu::DeviceType::IntegratedGpu => 96 << 20,
                _ => 32 << 20,
            },
        }
    }
}

/// How the renderer keeps chunk quads on the GPU (ADR 0035): pages of `page_quads` quads, the
/// floor of `initial_quads` created at GPU start, a chunk table of `initial_slots` slots. The
/// ceiling (`max_quads`, `max_slots`) is counted in quads and slots, the same numbers on every
/// tier; bytes are only reported ([`ChunkMemoryStats`]).
///
/// Growth past the floor is not there yet (#57 M2): until then every renderer keeps the floor
/// and the initial table, whatever `grow` says, and refuses the chunks that do not fit
/// ([`crate::RendererError`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkMemorySettings {
    /// Quads of the pages created at GPU start, the floor (default 8 Mi, 64 MiB: four pages of
    /// 2 Mi). Below `page_quads` the floor is one page of exactly this many quads (small apps
    /// and tests keep a single arena); above, whole pages. When `initial_slots` needs more pages
    /// than that (`page_chunks` chunks a page), the same quads go over that many smaller pages,
    /// each of `MIN_PAGE_QUADS` at least. With two pages or more the last one is Translucent.
    pub initial_quads: u32,
    /// Chunk slots of the table created at GPU start (default 32,768); the floor has chunk
    /// places for all of them when its quads allow (`initial_quads`). High also clamps them to
    /// `64 * max_compute_workgroups_per_dimension`, High and Mid to the storage binding size.
    pub initial_slots: u32,
    /// The most quads the pages may hold.
    pub max_quads: MemoryCeiling,
    /// The most chunk slots (default and at most `1 << 20`).
    pub max_slots: u32,
    /// Quads of one page (default 4 Mi, 32 MiB; at least 4,096). Clamped to the device's buffer
    /// limits, the same on every tier the device runs.
    pub page_quads: u32,
    /// Chunks one page holds at most (default and at most 10,922: 6 draws each fit one High
    /// indirect list of 65,532). Tests lower it to get many pages in small scenes.
    pub page_chunks: u32,
    /// Grow and shrink the pages and the table at runtime (#57 M2); `false` keeps the floor and
    /// the initial table.
    pub grow: bool,
}

impl Default for ChunkMemorySettings {
    fn default() -> Self {
        Self {
            initial_quads: 8 << 20,
            initial_slots: 32768,
            max_quads: MemoryCeiling::Auto,
            max_slots: MAX_SLOTS,
            page_quads: DEFAULT_PAGE_QUADS,
            page_chunks: PAGE_CHUNKS_MAX,
            grow: true,
        }
    }
}

/// An invalid [`ChunkMemorySettings`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChunkMemoryError {
    /// `page_quads` below [`MIN_PAGE_QUADS`].
    #[error("page_quads {0} is below {MIN_PAGE_QUADS}")]
    PageQuads(u32),
    /// `page_chunks` outside `1..=10922`.
    #[error("page_chunks {0} is outside 1..={PAGE_CHUNKS_MAX}")]
    PageChunks(u32),
    /// `initial_quads` is 0 or above the ceiling.
    #[error("initial_quads {0} is 0 or above max_quads")]
    InitialQuads(u32),
    /// `initial_slots` is 0 or above `max_slots`.
    #[error("initial_slots {0} is 0 or above max_slots")]
    InitialSlots(u32),
    /// `max_slots` is 0 or above `1 << 20`.
    #[error("max_slots {0} is outside 1..={MAX_SLOTS}")]
    MaxSlots(u32),
}

impl ChunkMemorySettings {
    /// Checks what does not depend on the device (which clamps the rest).
    pub fn validate(&self) -> Result<(), ChunkMemoryError> {
        if self.page_quads < MIN_PAGE_QUADS {
            return Err(ChunkMemoryError::PageQuads(self.page_quads));
        }
        if !(1..=PAGE_CHUNKS_MAX).contains(&self.page_chunks) {
            return Err(ChunkMemoryError::PageChunks(self.page_chunks));
        }
        if !(1..=MAX_SLOTS).contains(&self.max_slots) {
            return Err(ChunkMemoryError::MaxSlots(self.max_slots));
        }
        let ceiling = match self.max_quads {
            MemoryCeiling::Quads(quads) => quads,
            MemoryCeiling::Auto => u32::MAX,
        };
        if self.initial_quads == 0 || self.initial_quads > ceiling {
            return Err(ChunkMemoryError::InitialQuads(self.initial_quads));
        }
        if self.initial_slots == 0 || self.initial_slots > self.max_slots {
            return Err(ChunkMemoryError::InitialSlots(self.initial_slots));
        }
        Ok(())
    }
}

/// Pages whose High draw lists still fit the storage binding floor of every Mid/High device
/// (`boxel_gfx::floors::STORAGE_BINDING_SIZE`) with `page_chunks` chunks a page, as a multiple
/// of 4, at most [`MAX_PAGES`]. Applied on every tier, so the ceiling is the same everywhere.
pub(crate) fn list_pages_limit(page_chunks: u32) -> u32 {
    let per_page = u64::from(page_chunks) * 7 * ARGS_BYTES;
    let pages = (boxel_gfx::floors::STORAGE_BINDING_SIZE / per_page.max(1)) as u32;
    (pages / 4 * 4).clamp(1, MAX_PAGES)
}

/// What the device allows chunk memory ([`MemoryLayout::resolve`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeviceFacts {
    /// The largest page in quads: the storage binding size on High and Mid, the buffer size on
    /// Low, over 8 bytes a quad.
    pub page_limit: u64,
    /// The most chunk slots: High's dispatch size and High/Mid's storage binding size.
    pub slot_limit: u32,
    /// The adapter class (the automatic ceiling).
    pub class: wgpu::DeviceType,
}

impl DeviceFacts {
    /// The facts of `device` for `tier`.
    pub fn of(device: &wgpu::Device, tier: Tier) -> Self {
        let limits = device.limits();
        let storage_limit = limits
            .max_storage_buffer_binding_size
            .min(limits.max_buffer_size);
        let mut slot_limit = MAX_SLOTS;
        if tier == Tier::High {
            slot_limit = slot_limit.min(
                bindings::CULL_WORKGROUP_SIZE
                    .saturating_mul(limits.max_compute_workgroups_per_dimension),
            );
        }
        if tier >= Tier::Mid {
            slot_limit =
                slot_limit.min(u32::try_from(storage_limit / CHUNK_GPU_BYTES).unwrap_or(u32::MAX));
        }
        let buffer_limit = if tier >= Tier::Mid {
            storage_limit
        } else {
            limits.max_buffer_size
        };
        Self {
            page_limit: buffer_limit / QUAD_BYTES,
            slot_limit: slot_limit.max(1),
            class: device.adapter_info().device_type,
        }
    }
}

/// The resolved chunk memory of one renderer: page size, chunks a page, the floor pages, the
/// initial table and the ceiling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemoryLayout {
    /// Quads of a growth page.
    pub page_quads: u32,
    /// Chunks a page holds at most.
    pub page_chunks: u32,
    /// The floor: each page's class and quads, in id order.
    pub floor: Vec<(PageClass, u32)>,
    /// Chunk slots of the initial table.
    pub slots: u32,
    /// The most pages: the ceiling in quads over `page_quads`, at most [`list_pages_limit`].
    pub max_pages: u32,
    /// The most quads.
    pub ceiling_quads: u64,
    /// The most slots.
    pub ceiling_slots: u32,
}

impl MemoryLayout {
    /// `settings` on a device with `facts` (out-of-range values are clamped: the plugin
    /// validates before, [`ChunkMemorySettings::validate`]).
    pub fn resolve(settings: &ChunkMemorySettings, facts: DeviceFacts) -> Self {
        let page_quads = u64::from(settings.page_quads.max(MIN_PAGE_QUADS))
            .min(facts.page_limit)
            .max(1) as u32;
        let page_chunks = settings.page_chunks.clamp(1, PAGE_CHUNKS_MAX);
        let ceiling = u64::from(settings.max_quads.quads(facts.class).max(1));
        let max_pages = (ceiling.div_ceil(u64::from(page_quads)) as u32)
            .clamp(1, list_pages_limit(page_chunks));
        let ceiling_quads = ceiling.min(u64::from(max_pages) * u64::from(page_quads));
        let ceiling_slots = settings.max_slots.clamp(1, facts.slot_limit);
        let slots = settings.initial_slots.clamp(1, ceiling_slots);
        let initial = u64::from(settings.initial_quads.max(1)).min(ceiling_quads);
        let mut sizes = if initial < u64::from(page_quads) {
            vec![initial]
        } else {
            let pages = initial
                .div_ceil(u64::from(page_quads))
                .min(u64::from(max_pages)) as usize;
            vec![u64::from(page_quads); pages]
        };
        // Enough pages for the initial table's chunks, `page_chunks` each: nothing adds pages
        // later yet, and a page full of chunks refuses one with quads to spare. The same quads
        // go over more, smaller pages (at least `MIN_PAGE_QUADS` each, at most `max_pages`).
        let total: u64 = sizes.iter().sum();
        let for_slots = u64::from(slots.div_ceil(page_chunks))
            .min(u64::from(max_pages))
            .min(total / u64::from(MIN_PAGE_QUADS));
        if for_slots > sizes.len() as u64 {
            let (each, extra) = (total / for_slots, total % for_slots);
            sizes = (0..for_slots)
                .map(|i| each + u64::from(i < extra))
                .collect();
        }
        let pages = sizes.len();
        let floor = sizes
            .into_iter()
            .enumerate()
            .map(|(i, quads)| {
                // with two pages or more the last one is Translucent
                let class = if pages >= 2 && i + 1 == pages {
                    PageClass::Translucent
                } else {
                    PageClass::Solid
                };
                (class, quads as u32)
            })
            .collect();
        Self {
            page_quads,
            page_chunks,
            floor,
            slots,
            max_pages,
            ceiling_quads,
            ceiling_slots,
        }
    }

    /// Opaque draws of one page's High list.
    pub fn stride(&self) -> u32 {
        6 * self.page_chunks
    }

    /// Quads of the floor pages.
    pub fn floor_quads(&self) -> u64 {
        self.floor.iter().map(|&(_, quads)| u64::from(quads)).sum()
    }

    /// Chunks the floor pages hold at most.
    pub fn floor_chunk_places(&self) -> u64 {
        self.floor.len() as u64 * u64::from(self.page_chunks)
    }
}

const _: () = assert!(6 * PAGE_CHUNKS_MAX == PAGE_DRAWS_MAX);

/// What a page holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PageClass {
    /// Chunks without translucent quads.
    Solid,
    /// Chunks with translucent quads (the back to front pass draws them).
    Translucent,
}

impl PageClass {
    /// The class of a chunk with these (trimmed) bucket counts.
    pub fn of(counts: &[u32; BUCKET_COUNT]) -> Self {
        if counts[TRANSLUCENT_BUCKET] > 0 {
            Self::Translucent
        } else {
            Self::Solid
        }
    }
}

/// A chunk's range: a page and a range inside it (page-local quads).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PagedAlloc {
    /// The page id.
    pub page: u16,
    /// The range inside the page.
    pub alloc: ArenaAlloc,
}

/// The CPU side of one page.
#[derive(Debug)]
pub(crate) struct Page {
    /// What it holds.
    pub class: PageClass,
    /// Its ranges, in quads.
    pub alloc: ArenaAllocator,
    /// Chunks resident in it.
    pub chunks: u32,
    /// Allocator nodes (CPU bytes).
    nodes: u32,
}

impl Page {
    /// An empty page of `quads` quads that holds at most `page_chunks` chunks.
    fn new(class: PageClass, quads: u32, page_chunks: u32) -> Self {
        // `n` live ranges and `n + 1` free ones between them, one more while `replace` holds a
        // chunk's old range, and slack (`ArenaAllocator::new`). A page cannot hold more chunks
        // than half its quads (a mesh takes a quad and a palette entry at least).
        let nodes = 2 * page_chunks.min(quads / 2 + 1) + 4;
        Self {
            class,
            alloc: ArenaAllocator::new(quads, nodes),
            chunks: 0,
            nodes,
        }
    }

    /// True if a range of `size` quads fits, and with `new` (a chunk not resident here yet)
    /// a chunk place too.
    fn fits(&self, size: u32, new: bool, page_chunks: u32) -> bool {
        self.alloc.largest_free() >= size && (!new || self.chunks < page_chunks)
    }
}

/// The pages of one renderer, CPU side: placement, use and the chunk counts.
#[derive(Debug)]
pub(crate) struct ChunkMemory {
    /// By id; `None`: a free id.
    pages: Vec<Option<Page>>,
    /// Chunks one page holds at most.
    page_chunks: u32,
}

impl ChunkMemory {
    /// No page yet.
    pub fn new(page_chunks: u32) -> Self {
        Self {
            pages: Vec::new(),
            page_chunks: page_chunks.clamp(1, PAGE_CHUNKS_MAX),
        }
    }

    /// The pages of `layout`'s floor, in id order.
    pub fn with_floor(layout: &MemoryLayout) -> Self {
        let mut memory = Self::new(layout.page_chunks);
        for &(class, quads) in &layout.floor {
            memory.add_page(class, quads);
        }
        memory
    }

    /// Adds an empty page with the lowest free id, returned.
    pub fn add_page(&mut self, class: PageClass, quads: u32) -> u16 {
        let page = Page::new(class, quads, self.page_chunks);
        let id = match self.pages.iter().position(Option::is_none) {
            Some(free) => {
                self.pages[free] = Some(page);
                free
            }
            None => {
                self.pages.push(Some(page));
                self.pages.len() - 1
            }
        };
        debug_assert!(id < MAX_PAGES as usize, "page id {id} past MAX_PAGES");
        id as u16
    }

    /// The page `id`.
    pub fn page(&self, id: u16) -> Option<&Page> {
        self.pages.get(usize::from(id)).and_then(Option::as_ref)
    }

    /// Every page with its id, in id order.
    pub fn pages(&self) -> impl Iterator<Item = (u16, &Page)> {
        self.pages
            .iter()
            .enumerate()
            .filter_map(|(id, p)| p.as_ref().map(|p| (id as u16, p)))
    }

    /// Highest page id + 1: the pages the High draw lists cover.
    pub fn id_span(&self) -> u32 {
        self.pages.len() as u32
    }

    /// A range of `size` quads for a chunk of `class`, replacing its range `old` (if resident),
    /// in this order:
    /// 1. `old`'s page, when of `class` (a remeshed chunk stays where it is);
    /// 2. the pages of `class`, lowest id first, skipping those that hold `page_chunks` chunks
    ///    (a chunk moving in counts as new there) and those it does not fit in;
    /// 3. `old`'s range in place, when `size` fits in what it reserves and its page is of
    ///    `class`;
    /// 4. the pages of the other class (same order and skips), then `old`'s range in place
    ///    whatever its class: the image is the same, only the binds cost more, and the next
    ///    remesh moves the chunk back to its class (rule 1 needs the class).
    ///
    /// The new range is allocated while `old` is held and `old` is freed only on success, so
    /// `None` means nothing changed: `old` is still allocated and its GPU data untouched.
    /// Deterministic: the same calls give the same ranges on every tier, and adding an empty
    /// page moves nothing (it changes only placements that would have spilled or failed).
    pub fn place(
        &mut self,
        old: Option<PagedAlloc>,
        size: u32,
        class: PageClass,
    ) -> Option<PagedAlloc> {
        if size == 0 {
            return None;
        }
        let old_page = old.map(|o| o.page);
        let class_of = |memory: &Self, id: u16| memory.page(id).map(|p| p.class);
        // 1. the old page, of the class
        if let Some(o) = old
            && class_of(self, o.page) == Some(class)
            && let Some(new) = self.allocate_in(o.page, size, false)
        {
            self.release(o, Some(o.page));
            return Some(new);
        }
        // 2. the class
        if let Some(new) = self.allocate_in_class(class, size, old_page) {
            if let Some(o) = old {
                self.release(o, Some(new.page));
            }
            return Some(new);
        }
        // 3. in place, of the class
        if let Some(o) = old
            && class_of(self, o.page) == Some(class)
            && let Some(alloc) = o.alloc.with_size(size)
        {
            return Some(PagedAlloc { alloc, ..o });
        }
        // 4. the other class, then in place
        let other = match class {
            PageClass::Solid => PageClass::Translucent,
            PageClass::Translucent => PageClass::Solid,
        };
        if let Some(new) = self.allocate_in_class(other, size, old_page) {
            if let Some(o) = old {
                self.release(o, Some(new.page));
            }
            return Some(new);
        }
        let o = old?;
        o.alloc
            .with_size(size)
            .map(|alloc| PagedAlloc { alloc, ..o })
    }

    /// Allocates `size` in the first page of `class` it fits in (`old_page`: where the chunk is
    /// resident, where it does not count as new).
    fn allocate_in_class(
        &mut self,
        class: PageClass,
        size: u32,
        old_page: Option<u16>,
    ) -> Option<PagedAlloc> {
        let page_chunks = self.page_chunks;
        let id = self.pages().find_map(|(id, page)| {
            let new = old_page != Some(id);
            (page.class == class && page.fits(size, new, page_chunks)).then_some(id)
        })?;
        self.allocate_in(id, size, old_page != Some(id))
    }

    /// Allocates `size` in page `id`; `new`: a chunk not resident there yet (it takes one of its
    /// chunk places).
    fn allocate_in(&mut self, id: u16, size: u32, new: bool) -> Option<PagedAlloc> {
        let page_chunks = self.page_chunks;
        let page = self.pages.get_mut(usize::from(id))?.as_mut()?;
        if !page.fits(size, new, page_chunks) {
            return None;
        }
        let alloc = page.alloc.allocate(size)?;
        if new {
            page.chunks += 1;
        }
        Some(PagedAlloc { page: id, alloc })
    }

    /// Frees `old` after the chunk moved to page `to` (its chunk place goes too unless it stayed
    /// in the same page).
    fn release(&mut self, old: PagedAlloc, to: Option<u16>) {
        if let Some(page) = self
            .pages
            .get_mut(usize::from(old.page))
            .and_then(Option::as_mut)
        {
            page.alloc.free(old.alloc);
            if to != Some(old.page) {
                page.chunks -= 1;
            }
        }
    }

    /// Frees a removed chunk's range.
    pub fn free(&mut self, alloc: PagedAlloc) {
        self.release(alloc, None);
    }

    /// Frees every range (every chunk was removed); the pages stay.
    pub fn reset(&mut self) {
        for page in self.pages.iter_mut().flatten() {
            page.alloc.reset();
            page.chunks = 0;
        }
    }

    /// Quads in use (reserved ranges), all pages.
    pub fn used(&self) -> u64 {
        self.pages().map(|(_, p)| u64::from(p.alloc.used())).sum()
    }

    /// Quads the pages hold.
    pub fn capacity(&self) -> u64 {
        self.pages()
            .map(|(_, p)| u64::from(p.alloc.capacity()))
            .sum()
    }

    /// The largest range any page can give right now.
    pub fn largest_free(&self) -> u32 {
        self.pages()
            .map(|(_, p)| p.alloc.largest_free())
            .max()
            .unwrap_or(0)
    }

    /// Chunks the pages hold at most.
    pub fn chunk_places(&self) -> u32 {
        self.pages().count() as u32 * self.page_chunks
    }

    /// Pages holding `page_chunks` chunks: they take no new chunk, whatever quads they have
    /// free.
    pub fn full_pages(&self) -> u32 {
        self.pages()
            .filter(|(_, p)| p.chunks >= self.page_chunks)
            .count() as u32
    }

    /// True if some page has a free range of `size` quads (whether or not it has a chunk
    /// place left).
    fn has_range(&self, size: u32) -> bool {
        self.pages().any(|(_, p)| p.alloc.largest_free() >= size)
    }

    /// Bytes of the allocator nodes.
    pub fn cpu_bytes(&self) -> u64 {
        self.pages()
            .map(|(_, p)| u64::from(p.nodes) * NODE_BYTES)
            .sum()
    }
}

/// Bucket counts of `mesh` (see [`trim_counts`]).
pub(crate) fn bucket_counts(mesh: &ChunkMesh) -> [u32; BUCKET_COUNT] {
    debug_assert_eq!(
        mesh.bucket_counts.iter().sum::<u32>(),
        mesh.total_quads(),
        "bucket counts do not match the quads"
    );
    trim_counts(mesh.bucket_counts, mesh.total_quads())
}

/// Trims bucket counts from the last bucket so they never cover more than `total` quads (a
/// malformed mesh must not make draws read other chunks' quads).
pub(crate) fn trim_counts(mut counts: [u32; BUCKET_COUNT], total: u32) -> [u32; BUCKET_COUNT] {
    let mut left = total;
    for c in &mut counts {
        *c = (*c).min(left);
        left -= *c;
    }
    counts
}

/// Why [`upsert_resident`] left a chunk as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No free chunk slot.
    Table,
    /// No page can take `needed` quads; `resident`: the chunk's older mesh stays.
    Pages { needed: u32, resident: bool },
    /// A page has `needed` quads free, but every such page holds `page_chunks` chunks.
    PageChunks { needed: u32, resident: bool },
}

/// Places chunk `id`'s non-empty `mesh` at `origin`: a slot, a range in a page, its record, and
/// its upload queued. The CPU core of `VoxelRenderer::upsert_chunk` (the memory tests drive it
/// too). A refusal changes nothing.
pub(crate) fn upsert_resident(
    memory: &mut ChunkMemory,
    chunks: &mut ChunkTable,
    uploads: &mut Uploads,
    id: u64,
    origin: I64Vec3,
    mesh: &Arc<ChunkMesh>,
) -> Result<(), Refusal> {
    debug_assert!(!mesh.is_empty());
    if chunks.slot_for(id).is_none() {
        return Err(Refusal::Table);
    }
    let needed = arena_len(mesh);
    let old = chunks.get(id).map(|(_, r)| r.alloc);
    let counts = bucket_counts(mesh);
    let Some(alloc) = memory.place(old, needed, PageClass::of(&counts)) else {
        let resident = old.is_some();
        // `place` takes any page with the range and a chunk place (the old page needs no
        // place), so a page that has the range is full of chunks
        return Err(if memory.has_range(needed) {
            Refusal::PageChunks { needed, resident }
        } else {
            Refusal::Pages { needed, resident }
        });
    };
    let record = ChunkRecord {
        id,
        origin,
        alloc,
        counts,
        light_base: alloc.alloc.offset + mesh.total_quads(),
        light_len: light_entries(mesh),
    };
    let inserted = chunks.insert(record);
    // `slot_for` succeeded above, so this cannot fail
    debug_assert!(inserted.is_some());
    uploads.mesh(id, alloc.page, alloc.alloc.offset, Arc::clone(mesh));
    Ok(())
}

/// Removes chunk `id` (its range, slot and pending upload); false if unknown.
pub(crate) fn remove_resident(
    memory: &mut ChunkMemory,
    chunks: &mut ChunkTable,
    uploads: &mut Uploads,
    id: u64,
) -> bool {
    let Some((_, record)) = chunks.remove(id) else {
        return false;
    };
    memory.free(record.alloc);
    uploads.forget(id);
    true
}

/// Where a [`MemOp`] goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MemTarget {
    /// A page of chunk quads.
    Page(u16),
    /// Low: the light page parallel to page `id`.
    LightPage(u16),
    /// The chunk table (High/Mid).
    Table,
}

/// One write of the memory work, in bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MemOp<'a> {
    /// `data` at byte `offset` of `target`.
    Write {
        /// The buffer.
        target: MemTarget,
        /// Byte offset.
        offset: u64,
        /// The bytes.
        data: &'a [u8],
    },
}

/// Where the memory work's writes go: the GPU (staging buffers and copies recorded into the
/// frame's encoder) or, in tests, byte models of the buffers.
pub(crate) trait CopySink {
    /// The most bytes one batch of [`CopySink::writes`] should hold (a single larger write
    /// still goes in a batch of its own).
    fn max_batch(&self) -> u64;

    /// One batch of `bytes` of writes, all of them made by `fill` through the function it gets.
    /// False if the batch could not be staged: nothing of it reaches its targets. `fail`
    /// (testing) makes the batch fail as when out of memory.
    fn writes(
        &mut self,
        bytes: u64,
        fail: bool,
        fill: &mut dyn FnMut(&mut dyn FnMut(MemOp<'_>)),
    ) -> bool;
}

/// The GPU side of one page.
#[derive(Debug)]
pub(crate) struct PageGpu {
    /// The quads and palettes: `STORAGE` on High and Mid (vertex pulling), `VERTEX` on Low
    /// (instance-step), with `COPY_DST` (uploads) and `COPY_SRC` (moves and readback).
    pub quads: wgpu::Buffer,
    /// Low: each quad's light palette entry at the quad's own offset, parallel to `quads`.
    pub light: Option<wgpu::Buffer>,
    /// High/Mid: group 2, this page and the chunk table.
    pub geometry: Option<wgpu::BindGroup>,
    /// Chunks resident in it ([`PageSet::sync`]); the draw loops skip empty pages.
    pub chunks: u32,
}

/// The GPU pages by id: what the tier paths draw from.
#[derive(Debug, Default)]
pub(crate) struct PageSet {
    pages: Vec<Option<PageGpu>>,
}

impl PageSet {
    /// Page `id`.
    pub fn get(&self, id: u16) -> Option<&PageGpu> {
        self.pages.get(usize::from(id)).and_then(Option::as_ref)
    }

    /// The pages holding chunks, in id order.
    pub fn holding_chunks(&self) -> impl Iterator<Item = (u16, &PageGpu)> {
        self.pages
            .iter()
            .enumerate()
            .filter_map(|(id, p)| p.as_ref().filter(|p| p.chunks > 0).map(|p| (id as u16, p)))
    }

    /// Adds page `id`.
    pub fn insert(&mut self, id: u16, page: PageGpu) {
        let i = usize::from(id);
        if self.pages.len() <= i {
            self.pages.resize_with(i + 1, || None);
        }
        self.pages[i] = Some(page);
    }

    /// Takes the chunk counts of `memory`'s pages.
    pub fn sync(&mut self, memory: &ChunkMemory) {
        for (id, page) in self.pages.iter_mut().enumerate() {
            if let Some(page) = page {
                page.chunks = memory.page(id as u16).map_or(0, |p| p.chunks);
            }
        }
    }

    /// Bytes of the pages (Low's light pages included).
    pub fn bytes(&self) -> u64 {
        self.pages
            .iter()
            .flatten()
            .map(|p| p.quads.size() + p.light.as_ref().map_or(0, wgpu::Buffer::size))
            .sum()
    }
}

/// Creates page `id` of `quads` quads for `tier`: its buffer, Low's light page and, on High and
/// Mid, its group 2 with `geometry`'s chunk table. wgpu zero-fills it at its first use.
pub(crate) fn create_page(
    device: &wgpu::Device,
    tier: Tier,
    id: u16,
    quads: u32,
    geometry: Option<(&wgpu::BindGroupLayout, &wgpu::Buffer)>,
) -> PageGpu {
    let _span = frame_span!("render.page_create");
    let size = u64::from(quads) * QUAD_BYTES;
    let copies = wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
    let usage = if tier >= Tier::Mid {
        wgpu::BufferUsages::STORAGE | copies
    } else {
        wgpu::BufferUsages::VERTEX | copies
    };
    let quads = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(&format!("boxel page {id}")),
        size,
        usage,
        mapped_at_creation: false,
    });
    let light = (tier == Tier::Low).then(|| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("boxel light page {id}")),
            size,
            usage: wgpu::BufferUsages::VERTEX | copies,
            mapped_at_creation: false,
        })
    });
    let geometry = geometry
        .map(|(layout, table)| crate::path::geometry_bind_group(device, layout, &quads, table));
    PageGpu {
        quads,
        light,
        geometry,
        chunks: 0,
    }
}

/// Bytes of High's draw lists for `pages` page lists of `stride` opaque draws each (draw args,
/// draw refs).
pub(crate) fn list_bytes(pages: u32, stride: u32) -> u64 {
    let draws = u64::from(pages) * u64::from(stride) * 7 / 6;
    draws * (ARGS_BYTES + size_of::<DrawRef>() as u64)
}

/// Chunk memory counters ([`crate::RenderStats::chunk_memory`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChunkMemoryStats {
    /// Pages.
    pub pages: u32,
    /// Of them, Translucent pages.
    pub translucent_pages: u32,
    /// Quads in use in the pages (every resident chunk's quads and light palette).
    pub used_quads: u64,
    /// Quads the pages hold.
    pub capacity_quads: u64,
    /// The most quads the pages may hold ([`ChunkMemorySettings::max_quads`], cut to what the
    /// draw lists and [`boxel_shaders::gpu::chunk_memory::MAX_PAGES`] allow).
    pub ceiling_quads: u64,
    /// The most chunk slots.
    pub ceiling_slots: u32,
    /// Quads a page could give one chunk right now, at most.
    pub largest_free_quads: u32,
    /// Bytes of the pages, Low's light pages included.
    pub page_bytes: u64,
    /// Bytes of the GPU chunk table (High/Mid).
    pub table_bytes: u64,
    /// Bytes of High's draw lists.
    pub list_bytes: u64,
    /// Chunks the pages hold at most: [`ChunkMemorySettings::page_chunks`] a page.
    pub chunk_places: u32,
    /// Pages holding `page_chunks` chunks: they take no new chunk, whatever quads they have free
    /// ([`crate::RendererError::PageChunksFull`]).
    pub full_pages: u32,
    /// Extra binds of the last frame's translucent pass: one each time two buckets drawn one
    /// after the other sit in different pages.
    pub translucent_page_switches: u32,
    /// Bytes of the page allocators on the CPU.
    pub cpu_bytes: u64,
}

/// Testing: `bytes` of `buffer` from byte `offset` (it needs `COPY_SRC`; waits for the GPU).
pub(crate) fn read_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    offset: u64,
    bytes: u64,
) -> Result<Vec<u8>, String> {
    if bytes == 0 {
        return Ok(Vec::new());
    }
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("boxel readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("boxel readback"),
    });
    encoder.copy_buffer_to_buffer(buffer, offset, &read, 0, bytes);
    queue.submit([encoder.finish()]);
    let slice = read.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("poll: {e}"))?;
    rx.recv()
        .map_err(|e| format!("map: {e}"))?
        .map_err(|e| format!("map: {e}"))?;
    let data = slice
        .get_mapped_range()
        .map_err(|e| format!("mapped range: {e}"))?
        .to_vec();
    read.unmap();
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use PageClass::{Solid, Translucent};

    /// Memory with pages of `quads` quads of these classes, `page_chunks` chunks each.
    fn memory(classes: &[PageClass], quads: u32, page_chunks: u32) -> ChunkMemory {
        let mut memory = ChunkMemory::new(page_chunks);
        for &class in classes {
            memory.add_page(class, quads);
        }
        memory
    }

    fn page_of(a: Option<PagedAlloc>) -> Option<u16> {
        a.map(|a| a.page)
    }

    // Sizes on `offset_allocator`'s size bins (powers of two here): a range of `n` quads holds
    // a request of `n` only when `n` is on a bin, so these tests stay exact.

    #[test]
    fn chunks_go_to_their_class_lowest_page_first() {
        let mut m = memory(&[Solid, Translucent, Solid, Translucent], 1024, 100);
        assert_eq!(page_of(m.place(None, 256, Solid)), Some(0));
        assert_eq!(page_of(m.place(None, 256, Translucent)), Some(1));
        assert_eq!(page_of(m.place(None, 512, Solid)), Some(0));
        // page 0 has 256 left
        assert_eq!(page_of(m.place(None, 512, Solid)), Some(2));
        assert_eq!(page_of(m.place(None, 1024, Translucent)), Some(3));
        assert_eq!(page_of(m.place(None, 512, Translucent)), Some(1));
        assert_eq!(page_of(m.place(None, 512, Solid)), Some(2));
        assert_eq!(page_of(m.place(None, 256, Solid)), Some(0));
        // the other class only once every page of the own class failed
        assert_eq!(page_of(m.place(None, 256, Solid)), Some(1), "spilled");
        assert_eq!(page_of(m.place(None, 2048, Solid)), None);
        let used: Vec<u32> = m.pages().map(|(_, p)| p.alloc.used()).collect();
        assert_eq!(used, [1024; 4]);
        assert_eq!((m.used(), m.capacity(), m.largest_free()), (4096, 4096, 0));
        let chunks: Vec<u32> = m.pages().map(|(_, p)| p.chunks).collect();
        assert_eq!(chunks, [3, 3, 2, 1]);
    }

    #[test]
    fn a_remeshed_chunk_stays_in_its_page_or_reuses_its_range() {
        let mut m = memory(&[Solid, Solid], 1024, 100);
        let a = m.place(None, 256, Solid).expect("a");
        let _b = m.place(None, 512, Solid).expect("b");
        let _c = m.place(None, 512, Solid).expect("c");
        assert_eq!(a.page, 0);
        // rule 1: a smaller mesh stays in page 0 (allocated before the old range is freed)
        let a = m.place(Some(a), 128, Solid).expect("stays");
        assert_eq!((a.page, a.alloc.offset), (0, 768));
        assert_eq!(
            m.page(0).map(|p| (p.chunks, p.alloc.used())),
            Some((2, 640))
        );
        // rule 2: one that no longer fits in page 0 moves to page 1, its chunk place with it
        let a = m.place(Some(a), 512, Solid).expect("moves");
        assert_eq!(a.page, 1);
        assert_eq!(m.page(0).map(|p| p.chunks), Some(1));
        assert_eq!(m.page(1).map(|p| p.chunks), Some(2));
        // rule 3: nothing fits anywhere, but the old range holds it
        let a = m.place(Some(a), 511, Solid).expect("in place");
        assert_eq!((a.page, a.alloc.size, a.alloc.reserved()), (1, 511, 512));
        // too large for anything: nothing changes
        let used = m.used();
        assert!(m.place(Some(a), 600, Solid).is_none());
        assert_eq!(m.used(), used);
    }

    /// A first translucent chunk with no Translucent page (a one-page floor) spills into a
    /// Solid page, and its next remesh moves it to its class once a Translucent page exists.
    #[test]
    fn a_spilled_chunk_goes_back_to_its_class() {
        let mut m = memory(&[Solid], 1024, 100);
        let water = m.place(None, 128, Translucent).expect("spilled");
        assert_eq!(water.page, 0);
        // in place whatever the class (rule 4) when nothing else fits
        let mut full = memory(&[Solid], 128, 100);
        let w = full.place(None, 128, Translucent).expect("all of it");
        assert_eq!(
            full.place(Some(w), 64, Translucent).map(|a| a.alloc.size),
            Some(64)
        );
        // a Translucent page appears: the next remesh moves there
        let id = m.add_page(Translucent, 1024);
        assert_eq!(id, 1);
        let water = m.place(Some(water), 128, Translucent).expect("moved");
        assert_eq!(water.page, 1);
        assert_eq!(m.page(0).map(|p| (p.chunks, p.alloc.used())), Some((0, 0)));
        // a chunk that drains its water moves back to a Solid page
        let dry = m.place(Some(water), 64, Solid).expect("dry");
        assert_eq!(dry.page, 0);
    }

    #[test]
    fn a_page_holds_at_most_page_chunks_chunks() {
        let mut m = memory(&[Solid, Solid], 1024, 3);
        let placed: Vec<u16> = (0..7)
            .filter_map(|_| m.place(None, 16, Solid).map(|a| a.page))
            .collect();
        assert_eq!(placed, [0, 0, 0, 1, 1, 1]);
        // a remesh inside a full page is no new chunk there
        let mut m = memory(&[Solid], 1024, 2);
        let a = m.place(None, 16, Solid).expect("a");
        let _b = m.place(None, 16, Solid).expect("b");
        assert!(m.place(None, 16, Solid).is_none(), "full of chunks");
        assert_eq!(m.place(Some(a), 32, Solid).map(|a| a.page), Some(0));
    }

    #[test]
    fn freed_and_reset_pages_take_chunks_again() {
        let mut m = memory(&[Solid], 128, 4);
        let a = m.place(None, 96, Solid).expect("a");
        assert!(m.place(None, 64, Solid).is_none());
        m.free(a);
        assert_eq!(m.page(0).map(|p| (p.chunks, p.alloc.used())), Some((0, 0)));
        let _b = m.place(None, 128, Solid).expect("whole page");
        m.reset();
        assert_eq!((m.used(), m.largest_free()), (0, 128));
        assert_eq!(m.page(0).map(|p| p.chunks), Some(0));
    }

    /// Every range stays inside its page, and offsets past 65,535 come back exact (the `u16`
    /// node mode of `offset_allocator` would cut them).
    #[test]
    fn ranges_stay_inside_their_page_with_exact_offsets() {
        let quads = 1 << 20;
        let mut m = memory(&[Solid, Solid], quads, 64);
        let mut end = [0u32; 2];
        let mut past_u16 = false;
        for _ in 0..60 {
            let a = m.place(None, 30_000, Solid).expect("fits");
            let page = usize::from(a.page);
            assert_eq!(a.alloc.offset, end[page], "sequential in a fresh page");
            end[page] += 30_000;
            assert!(a.alloc.offset + a.alloc.size <= quads);
            past_u16 |= a.alloc.offset > u32::from(u16::MAX);
        }
        assert!(past_u16 && end[1] > 0);
    }

    /// `n` live ranges between `n + 1` free ones, each chunk remeshed in place: the nodes of a
    /// page are enough for `page_chunks` chunks (`replace_has_enough_allocator_nodes` per page).
    #[test]
    fn a_page_has_enough_allocator_nodes() {
        let n = 8;
        let mut m = memory(&[Solid], 1 << 16, n);
        let page = m.pages[0].as_mut().expect("page");
        let all: Vec<ArenaAlloc> = (0..2 * n + 1)
            .map(|_| page.alloc.allocate(64).expect("setup"))
            .collect();
        let mut live = Vec::new();
        for (i, alloc) in all.into_iter().enumerate() {
            if i % 2 == 0 {
                page.alloc.free(alloc);
            } else {
                live.push(PagedAlloc { page: 0, alloc });
            }
        }
        page.chunks = n;
        assert_eq!(live.len() as u32, n);
        // `n` live ranges between `n + 1` free ones, each remeshed in its page
        for a in &mut live {
            *a = m.place(Some(*a), 32, Solid).expect("replace");
            assert_eq!(a.page, 0);
        }
        for a in live {
            m.free(a);
        }
        assert_eq!((m.used(), m.page(0).map(|p| p.chunks)), (0, Some(0)));
    }

    /// Adding an empty page moves no resident chunk, and changes only placements that would
    /// otherwise have spilled to the other class or failed.
    #[test]
    fn adding_an_empty_page_moves_nothing() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % u64::from(n)) as u32
        };
        for _ in 0..50 {
            let mut a = memory(&[Solid, Translucent], 500, 6);
            let mut live: Vec<(PagedAlloc, PageClass)> = Vec::new();
            for _ in 0..12 {
                let class = if next(3) == 0 { Translucent } else { Solid };
                if let Some(p) = a.place(None, 1 + next(120), class) {
                    live.push((p, class));
                }
            }
            let mut b = memory(&[Solid, Translucent], 500, 6);
            for (p, class) in &live {
                let again = b.place(None, p.alloc.size, *class).expect("same history");
                assert_eq!((again.page, again.alloc.offset), (p.page, p.alloc.offset));
            }
            let extra = b.add_page(if next(2) == 0 { Solid } else { Translucent }, 500);
            for _ in 0..8 {
                let class = if next(3) == 0 { Translucent } else { Solid };
                let size = 1 + next(200);
                let (pa, pb) = (a.place(None, size, class), b.place(None, size, class));
                match (pa, pb) {
                    (Some(x), Some(y)) if x.page == y.page => {
                        assert_eq!(x.alloc.offset, y.alloc.offset);
                    }
                    (x, Some(y)) => {
                        // `b` used its new page: `a` spilled or failed there
                        assert_eq!(y.page, extra);
                        let spilled =
                            x.is_some_and(|x| a.page(x.page).map(|p| p.class) != Some(class));
                        assert!(x.is_none() || spilled, "{x:?} -> {y:?}");
                        // keep the two in step for the next placements
                        if let Some(x) = x {
                            a.free(x);
                        }
                        b.free(y);
                    }
                    (Some(x), None) => panic!("a page more refused {x:?}"),
                    (None, None) => {}
                }
            }
            // what was resident is where it was
            for (p, _) in &live {
                assert!(b.page(p.page).is_some());
            }
        }
    }

    fn facts(class: wgpu::DeviceType) -> DeviceFacts {
        DeviceFacts {
            page_limit: (128 << 20) / QUAD_BYTES,
            slot_limit: MAX_SLOTS,
            class,
        }
    }

    #[test]
    fn the_floor_is_whole_pages_with_one_translucent() {
        let discrete = facts(wgpu::DeviceType::DiscreteGpu);
        let default = MemoryLayout::resolve(&ChunkMemorySettings::default(), discrete);
        assert_eq!(default.page_quads, 4 << 20);
        // 8 Mi quads in pages of 2 Mi: four pages hold the 32,768 slots' chunks
        assert_eq!(
            default.floor,
            [
                (Solid, 2 << 20),
                (Solid, 2 << 20),
                (Solid, 2 << 20),
                (Translucent, 2 << 20)
            ]
        );
        assert_eq!((default.slots, default.page_chunks), (32768, 10922));
        assert_eq!(default.ceiling_quads, 192 << 20);
        assert_eq!(default.max_pages, 48);
        assert_eq!(default.stride(), 65532);
        // below a page: one page of exactly that size
        let small = ChunkMemorySettings {
            initial_quads: 1 << 16,
            initial_slots: 64,
            ..ChunkMemorySettings::default()
        };
        let layout = MemoryLayout::resolve(&small, discrete);
        assert_eq!(layout.floor, [(Solid, 1 << 16)]);
        // above: rounded up to whole pages (48 Mi: 12 pages)
        let big = ChunkMemorySettings {
            initial_quads: 48 << 20,
            initial_slots: 98304,
            ..ChunkMemorySettings::default()
        };
        let layout = MemoryLayout::resolve(&big, discrete);
        assert_eq!(layout.floor.len(), 12);
        assert_eq!(
            layout
                .floor
                .iter()
                .filter(|(c, _)| *c == Translucent)
                .count(),
            1
        );
        let odd = ChunkMemorySettings {
            initial_quads: (4 << 20) + 1,
            initial_slots: 64,
            ..ChunkMemorySettings::default()
        };
        assert_eq!(
            MemoryLayout::resolve(&odd, discrete).floor,
            [(Solid, 4 << 20), (Translucent, 4 << 20)]
        );
        // the device cuts the page size; the ceiling by adapter class
        let tiny = DeviceFacts {
            page_limit: 1 << 20,
            ..discrete
        };
        assert_eq!(
            MemoryLayout::resolve(&ChunkMemorySettings::default(), tiny).page_quads,
            1 << 20
        );
        for (class, quads) in [
            (wgpu::DeviceType::IntegratedGpu, 96 << 20),
            (wgpu::DeviceType::Cpu, 32 << 20),
        ] {
            let layout = MemoryLayout::resolve(&ChunkMemorySettings::default(), facts(class));
            assert_eq!(layout.ceiling_quads, quads);
        }
    }

    /// No page id ever reaches `MAX_PAGES`, and no High list pair outgrows the storage binding
    /// floor, whatever the settings ask for.
    #[test]
    fn the_ceiling_keeps_page_ids_and_lists_bounded() {
        assert_eq!(list_pages_limit(PAGE_CHUNKS_MAX), 84);
        assert_eq!(list_pages_limit(4), MAX_PAGES);
        for page_chunks in [1, 4, 64, 1000, PAGE_CHUNKS_MAX] {
            let limit = list_pages_limit(page_chunks);
            assert!(limit <= MAX_PAGES && limit.is_multiple_of(4));
            let args = u64::from(limit) * u64::from(page_chunks) * 7 * ARGS_BYTES;
            assert!(args <= boxel_gfx::floors::STORAGE_BINDING_SIZE);
        }
        let huge = ChunkMemorySettings {
            initial_quads: u32::MAX,
            max_quads: MemoryCeiling::Quads(u32::MAX),
            page_quads: MIN_PAGE_QUADS,
            page_chunks: 4,
            ..ChunkMemorySettings::default()
        };
        let layout = MemoryLayout::resolve(&huge, facts(wgpu::DeviceType::DiscreteGpu));
        assert_eq!(layout.max_pages, MAX_PAGES);
        assert_eq!(layout.floor.len(), MAX_PAGES as usize);
        assert_eq!(
            layout.ceiling_quads,
            u64::from(MAX_PAGES) * u64::from(MIN_PAGE_QUADS)
        );
        let memory = ChunkMemory::with_floor(&layout);
        assert!(memory.pages().all(|(id, _)| u32::from(id) < MAX_PAGES));
    }

    /// A non-empty mesh of `quads` opaque quads (`quads + 1` entries with its palette).
    fn solid_mesh(quads: u32) -> Arc<ChunkMesh> {
        let mut counts = [0; BUCKET_COUNT];
        counts[0] = quads;
        let quads = vec![PackedQuad { geo: 0, mat: 0 }; quads as usize];
        Arc::new(ChunkMesh::from_quads(quads, counts))
    }

    /// The floor holds every chunk the initial table does, as main's single arena did whenever
    /// the quads fit (growth, which would add pages for chunk places, is not there yet): the
    /// table refuses first, never a page full of chunks with quads to spare. The quads are the
    /// ones asked for.
    #[test]
    fn the_floor_holds_every_initial_slot() {
        let discrete = facts(wgpu::DeviceType::DiscreteGpu);
        // the defaults, the overworld of the examples, a floor below one page
        for (quads, slots) in [(8 << 20, 32768), (24 << 20, 65536), (2 << 20, 32768)] {
            let settings = ChunkMemorySettings {
                initial_quads: quads,
                initial_slots: slots,
                ..ChunkMemorySettings::default()
            };
            let layout = MemoryLayout::resolve(&settings, discrete);
            let what = format!("{quads} quads, {slots} slots: {:?}", layout.floor);
            assert_eq!(layout.floor_quads(), u64::from(quads), "{what}");
            assert!(layout.floor_chunk_places() >= u64::from(slots), "{what}");
            assert!(
                layout
                    .floor
                    .iter()
                    .all(|&(_, q)| (MIN_PAGE_QUADS..=layout.page_quads).contains(&q)),
                "{what}"
            );
            let mut memory = ChunkMemory::with_floor(&layout);
            let mut chunks = ChunkTable::new(layout.slots);
            let mut uploads = Uploads::default();
            let mesh = solid_mesh(1);
            for id in 0..u64::from(slots) {
                upsert_resident(
                    &mut memory,
                    &mut chunks,
                    &mut uploads,
                    id,
                    I64Vec3::ZERO,
                    &mesh,
                )
                .unwrap_or_else(|e| panic!("{what}: chunk {id} refused: {e:?}"));
            }
            let next = u64::from(slots);
            let refusal = upsert_resident(
                &mut memory,
                &mut chunks,
                &mut uploads,
                next,
                I64Vec3::ZERO,
                &mesh,
            );
            assert_eq!(refusal, Err(Refusal::Table), "{what}");
        }
        // the floors of the existing tests and of the examples' big worlds keep their pages
        let small = ChunkMemorySettings {
            initial_quads: 1 << 18,
            initial_slots: 4096,
            ..ChunkMemorySettings::default()
        };
        assert_eq!(
            MemoryLayout::resolve(&small, discrete).floor,
            [(Solid, 1 << 18)]
        );
        let big = ChunkMemorySettings {
            initial_quads: 48 << 20,
            initial_slots: 98304,
            ..ChunkMemorySettings::default()
        };
        let layout = MemoryLayout::resolve(&big, discrete);
        assert_eq!(layout.floor.len(), 12);
        assert!(layout.floor.iter().all(|&(_, q)| q == 4 << 20));
        // no split page below `MIN_PAGE_QUADS`: tiny pages of few chunks stay as asked
        let tiny = ChunkMemorySettings {
            initial_quads: 32 * 4096,
            initial_slots: 4096,
            page_quads: 4096,
            page_chunks: 64,
            ..ChunkMemorySettings::default()
        };
        let layout = MemoryLayout::resolve(&tiny, discrete);
        assert_eq!(layout.floor.len(), 32);
        assert!(
            layout.floor_chunk_places() < 4096,
            "the quads cap the pages"
        );
    }

    /// A refusal tells quads that ran out from pages full of chunks.
    #[test]
    fn a_refusal_tells_quads_from_chunk_places() {
        let mut m = memory(&[Solid, Translucent], 1024, 2);
        let mut chunks = ChunkTable::new(16);
        let mut uploads = Uploads::default();
        let small = solid_mesh(1);
        for id in 0..4 {
            let placed =
                upsert_resident(&mut m, &mut chunks, &mut uploads, id, I64Vec3::ZERO, &small);
            assert_eq!(placed, Ok(()));
        }
        assert_eq!((m.full_pages(), m.chunk_places()), (2, 4));
        // quads to spare in both pages, no chunk place
        assert_eq!(
            upsert_resident(&mut m, &mut chunks, &mut uploads, 4, I64Vec3::ZERO, &small),
            Err(Refusal::PageChunks {
                needed: 2,
                resident: false
            })
        );
        // a resident chunk larger than any free range: the quads ran out
        let large = solid_mesh(1500);
        assert_eq!(
            upsert_resident(&mut m, &mut chunks, &mut uploads, 0, I64Vec3::ZERO, &large),
            Err(Refusal::Pages {
                needed: 1501,
                resident: true
            })
        );
        // a removed chunk frees its place
        assert!(remove_resident(&mut m, &mut chunks, &mut uploads, 1));
        assert_eq!((m.full_pages(), m.chunk_places()), (1, 4));
        let placed = upsert_resident(&mut m, &mut chunks, &mut uploads, 4, I64Vec3::ZERO, &small);
        assert_eq!(placed, Ok(()));
    }

    #[test]
    fn settings_are_validated() {
        assert_eq!(ChunkMemorySettings::default().validate(), Ok(()));
        let bad = |s: ChunkMemorySettings| s.validate().expect_err("invalid");
        let d = ChunkMemorySettings::default();
        assert_eq!(
            bad(ChunkMemorySettings {
                page_quads: 100,
                ..d
            }),
            ChunkMemoryError::PageQuads(100)
        );
        assert_eq!(
            bad(ChunkMemorySettings {
                page_chunks: 0,
                ..d
            }),
            ChunkMemoryError::PageChunks(0)
        );
        assert_eq!(
            bad(ChunkMemorySettings {
                page_chunks: 20000,
                ..d
            }),
            ChunkMemoryError::PageChunks(20000)
        );
        assert_eq!(
            bad(ChunkMemorySettings {
                initial_quads: 10 << 20,
                max_quads: MemoryCeiling::Quads(8 << 20),
                ..d
            }),
            ChunkMemoryError::InitialQuads(10 << 20)
        );
        assert_eq!(
            bad(ChunkMemorySettings {
                initial_slots: 70_000,
                max_slots: 65_536,
                ..d
            }),
            ChunkMemoryError::InitialSlots(70_000)
        );
        assert_eq!(
            bad(ChunkMemorySettings {
                max_slots: MAX_SLOTS + 1,
                ..d
            }),
            ChunkMemoryError::MaxSlots(MAX_SLOTS + 1)
        );
        // a floor below one page is fine
        let small = ChunkMemorySettings {
            initial_quads: 1024,
            initial_slots: 8,
            ..d
        };
        assert_eq!(small.validate(), Ok(()));
    }

    // ---------------------------------------------------------------- MemOp replay

    use crate::sort::SortedBucket;
    use crate::upload::{UploadKinds, expand_light, quad_light};
    use boxel_mesh::{PackedQuad, PaletteEntry};
    use proptest::prelude::*;
    use std::collections::HashMap;

    /// A buffer as the GPU would hold it: its bytes and which of them were ever written (wgpu's
    /// init tracker zero-fills the others at the first use that needs them).
    #[derive(Clone, Debug)]
    struct Model {
        bytes: Vec<u8>,
        written: Vec<bool>,
    }

    impl Model {
        fn new(len: usize) -> Self {
            Self {
                bytes: vec![0xAB; len],
                written: vec![false; len],
            }
        }
    }

    /// The test sink: every write lands in a byte model of its buffer; `fail` batches land
    /// nowhere.
    struct ModelSink {
        buffers: HashMap<MemTarget, Model>,
        writes: usize,
    }

    impl CopySink for ModelSink {
        fn max_batch(&self) -> u64 {
            // small batches: runs get cut, several batches a frame
            4096
        }

        fn writes(
            &mut self,
            _bytes: u64,
            fail: bool,
            fill: &mut dyn FnMut(&mut dyn FnMut(MemOp<'_>)),
        ) -> bool {
            if fail {
                return false;
            }
            let buffers = &mut self.buffers;
            let writes = &mut self.writes;
            fill(&mut |op| match op {
                MemOp::Write {
                    target,
                    offset,
                    data,
                } => {
                    let model = buffers
                        .get_mut(&target)
                        .expect("a write to a missing buffer");
                    let range = offset as usize..offset as usize + data.len();
                    assert!(
                        range.end <= model.bytes.len(),
                        "{target:?} {range:?} out of bounds"
                    );
                    model.bytes[range.clone()].copy_from_slice(data);
                    model.written[range].iter_mut().for_each(|w| *w = true);
                    *writes += 1;
                }
            });
            true
        }
    }

    /// What the test expects of a live chunk.
    #[derive(Clone, Debug)]
    struct Expected {
        origin: I64Vec3,
        mesh: Arc<ChunkMesh>,
        /// The translucent bucket in its last uploaded order.
        sorted: Option<Vec<PackedQuad>>,
    }

    /// xorshift64.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n.max(1)
        }
    }

    /// A random mesh of 1..=`max` quads, maybe translucent, with a palette of 0..4 entries.
    fn random_mesh(rng: &mut Rng, max: u32) -> Arc<ChunkMesh> {
        let total = 1 + rng.next(u64::from(max.max(1))) as u32;
        let palette: Vec<PaletteEntry> = (0..rng.next(4))
            .map(|_| [rng.next(1 << 32) as u32, rng.next(1 << 32) as u32])
            .collect();
        let quads: Vec<PackedQuad> = (0..total)
            .map(|_| {
                let light = rng.next(palette.len() as u64 + 1) as u16;
                PackedQuad {
                    geo: rng.next(1 << 32) as u32,
                    mat: rng.next(1 << 20) as u32,
                }
                .with_light_index(light)
            })
            .collect();
        let mut counts = [0u32; BUCKET_COUNT];
        let mut left = total;
        let translucent = rng.next(3) == 0;
        let last = if translucent {
            BUCKET_COUNT
        } else {
            TRANSLUCENT_BUCKET
        };
        for (i, c) in counts.iter_mut().enumerate().take(last) {
            let n = if i + 1 == last {
                left
            } else {
                rng.next(u64::from(left) + 1) as u32
            };
            *c = n;
            left -= n;
        }
        Arc::new(ChunkMesh {
            palette,
            ..ChunkMesh::from_quads(quads, counts)
        })
    }

    /// Checks every model against the CPU state: each live chunk's range holds its quads (the
    /// translucent bucket in its last sorted order) and then its palette, its light page range
    /// the expanded light, the table every slot's entry; every range a draw reads was written.
    fn check(
        sink: &ModelSink,
        chunks: &ChunkTable,
        expected: &HashMap<u64, Expected>,
        kinds: UploadKinds,
        memory: &ChunkMemory,
    ) -> Result<(), String> {
        let mut light = Vec::new();
        for (&id, e) in expected {
            let (_, r) = chunks.get(id).ok_or(format!("chunk {id} not resident"))?;
            if r.origin != e.origin {
                return Err(format!("chunk {id} origin"));
            }
            let page = memory.page(r.alloc.page).ok_or("no page")?;
            if r.alloc.alloc.offset + r.alloc.alloc.size > page.alloc.capacity() {
                return Err(format!("chunk {id} crosses its page"));
            }
            let mut quads = e.mesh.quads.clone();
            if let Some(sorted) = &e.sorted {
                let start: u32 = r.counts[..TRANSLUCENT_BUCKET].iter().sum();
                quads[start as usize..start as usize + sorted.len()].copy_from_slice(sorted);
            }
            let mut want: Vec<u8> = bytemuck::cast_slice(&quads).to_vec();
            want.extend_from_slice(bytemuck::cast_slice(crate::upload::uploaded_palette(
                &e.mesh,
            )));
            let at = r.alloc.alloc.offset as usize * QUAD_BYTES as usize;
            let range = at..at + want.len();
            let model = &sink.buffers[&MemTarget::Page(r.alloc.page)];
            if !model.written[range.clone()].iter().all(|&w| w) {
                return Err(format!("chunk {id}: its range was never written"));
            }
            if model.bytes[range.clone()] != want[..] {
                return Err(format!("chunk {id}: wrong bytes in page {}", r.alloc.page));
            }
            if kinds.low_light {
                expand_light(&e.mesh, &mut light);
                if let Some(sorted) = &e.sorted {
                    let start: usize = r.counts[..TRANSLUCENT_BUCKET].iter().sum::<u32>() as usize;
                    let mut sorted_light = Vec::new();
                    quad_light(&e.mesh, sorted, &mut sorted_light);
                    light[start..start + sorted.len()].copy_from_slice(&sorted_light);
                }
                let model = &sink.buffers[&MemTarget::LightPage(r.alloc.page)];
                if model.bytes[range.clone()] != bytemuck::cast_slice::<_, u8>(&light)[..] {
                    return Err(format!("chunk {id}: wrong light"));
                }
            }
        }
        if expected.len() != chunks.len() {
            return Err("resident chunks the test does not know".to_string());
        }
        if kinds.table {
            let table = &sink.buffers[&MemTarget::Table];
            for slot in 0..chunks.capacity() {
                let at = slot as usize * CHUNK_GPU_BYTES as usize;
                let range = at..at + CHUNK_GPU_BYTES as usize;
                let want = chunks.gpu_entry_at(slot);
                let live = chunks.record_at(slot).is_some();
                // never-written entries read as zero on the GPU (wgpu's zero-fill)
                let got: &[u8] = if table.written[range.clone()].iter().all(|&w| w) {
                    &table.bytes[range]
                } else if table.written[range.clone()].iter().any(|&w| w) {
                    return Err(format!("slot {slot}: half written"));
                } else if live {
                    return Err(format!("live slot {slot} never written"));
                } else {
                    continue;
                };
                if got != bytemuck::bytes_of(&want) {
                    let got: ChunkGpu = *bytemuck::from_bytes(got);
                    return Err(format!(
                        "slot {slot}: stale entry {got:?}, want {want:?} (slot count {})",
                        chunks.slot_count()
                    ));
                }
            }
        }
        Ok(())
    }

    /// One random run: frames of upserts, removes, re-sorted buckets, window moves, frames that
    /// draw nothing, lost frames (then a resync, as the render plugin does) and clears, over
    /// tiny pages. Every flushed frame must leave the models equal to the CPU state.
    fn replay(
        seed: u64,
        page_quads: u32,
        page_chunks: u32,
        floor: usize,
        slots: u32,
        kinds: UploadKinds,
    ) -> Result<(), String> {
        let mut rng = Rng(seed | 1);
        let mut memory = ChunkMemory::new(page_chunks);
        let mut sink = ModelSink {
            buffers: HashMap::new(),
            writes: 0,
        };
        for k in 0..floor {
            let class = if k + 1 == floor && floor > 1 {
                Translucent
            } else {
                Solid
            };
            let id = memory.add_page(class, page_quads);
            let len = page_quads as usize * QUAD_BYTES as usize;
            sink.buffers.insert(MemTarget::Page(id), Model::new(len));
            if kinds.low_light {
                sink.buffers
                    .insert(MemTarget::LightPage(id), Model::new(len));
            }
        }
        if kinds.table {
            let len = slots as usize * CHUNK_GPU_BYTES as usize;
            sink.buffers.insert(MemTarget::Table, Model::new(len));
        }
        let mut chunks = ChunkTable::new(slots);
        let mut uploads = Uploads::default();
        let mut expected: HashMap<u64, Expected> = HashMap::new();
        let ids = u64::from(slots) + 6;
        let max_quads = (page_quads / 3).max(2);
        for frame in 0..60 {
            for _ in 0..rng.next(12) {
                let id = rng.next(ids);
                match rng.next(10) {
                    0..6 => {
                        let mesh = random_mesh(&mut rng, max_quads);
                        let origin = I64Vec3::new(id as i64 * 32, (rng.next(3) as i64) << 31, 0);
                        // a refusal changes nothing: the old mesh stays
                        let placed = upsert_resident(
                            &mut memory,
                            &mut chunks,
                            &mut uploads,
                            id,
                            origin,
                            &mesh,
                        );
                        if placed.is_ok() {
                            expected.insert(
                                id,
                                Expected {
                                    origin,
                                    mesh,
                                    sorted: None,
                                },
                            );
                        }
                    }
                    6..8 => {
                        if remove_resident(&mut memory, &mut chunks, &mut uploads, id) {
                            expected.remove(&id);
                        }
                    }
                    8 => {
                        // a re-sorted translucent bucket: its quads reversed
                        if let Some(e) = expected.get_mut(&id)
                            && let Some((_, r)) = chunks.get(id)
                            && r.counts[TRANSLUCENT_BUCKET] > 0
                        {
                            let start: u32 = r.counts[..TRANSLUCENT_BUCKET].iter().sum();
                            let n = r.counts[TRANSLUCENT_BUCKET] as usize;
                            let mut quads =
                                e.mesh.quads[start as usize..start as usize + n].to_vec();
                            quads.rotate_left(rng.next(n as u64) as usize);
                            quads.reverse();
                            uploads.sorted_bucket(
                                &chunks,
                                SortedBucket {
                                    id,
                                    mesh: Arc::clone(&e.mesh),
                                    quads: quads.clone(),
                                },
                            );
                            e.sorted = Some(quads);
                        }
                    }
                    _ => {
                        let x = (rng.next(3) as i64) << 31;
                        chunks.follow(I64Vec3::new(x, 0, 0));
                    }
                }
            }
            match rng.next(12) {
                // a frame that draws nothing: the changes wait
                0 => continue,
                // a lost frame: its first batch never lands (it may wait for a frame with
                // something to write)
                1 => uploads.fail_next_batch(),
                // every chunk removed
                2 => {
                    chunks.clear();
                    memory.reset();
                    uploads.clear();
                    expected.clear();
                }
                _ => {}
            }
            uploads.flush(&mut sink, &mut chunks, kinds);
            if uploads.take_lost() {
                // as the render plugin does: every chunk again, from a cleared table
                chunks.clear();
                memory.reset();
                uploads.clear();
                let mut again: Vec<(u64, Expected)> = expected.drain().collect();
                again.sort_by_key(|(id, _)| *id);
                for (id, e) in again {
                    let placed = upsert_resident(
                        &mut memory,
                        &mut chunks,
                        &mut uploads,
                        id,
                        e.origin,
                        &e.mesh,
                    );
                    if placed.is_ok() {
                        expected.insert(id, Expected { sorted: None, ..e });
                    }
                }
                uploads.flush(&mut sink, &mut chunks, kinds);
                if uploads.take_lost() {
                    return Err(format!("frame {frame}: the resync was lost"));
                }
            }
            check(&sink, &chunks, &expected, kinds, &memory)
                .map_err(|e| format!("frame {frame}: {e}"))?;
            let pages_used: u64 = memory.pages().map(|(_, p)| u64::from(p.chunks)).sum();
            if pages_used != chunks.len() as u64 {
                return Err(format!(
                    "frame {frame}: page chunk counts {pages_used} vs {}",
                    chunks.len()
                ));
            }
            if memory.pages().any(|(_, p)| p.chunks > page_chunks) {
                return Err(format!("frame {frame}: a page holds too many chunks"));
            }
        }
        if sink.writes == 0 {
            return Err("nothing was written".to_string());
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        /// The memory work's writes, replayed over byte models of the pages, the light pages and
        /// the table, always leave the GPU what the CPU state says (ADR 0035).
        #[test]
        fn mem_ops_replay_to_the_cpu_state(
            seed in any::<u64>(),
            page_quads in 64u32..4096,
            page_chunks in 2u32..16,
            floor in 1usize..4,
            slots in 4u32..48,
            low in any::<bool>(),
        ) {
            let kinds = UploadKinds { table: !low, low_light: low };
            let result = replay(seed, page_quads, page_chunks, floor, slots, kinds);
            prop_assert!(result.is_ok(), "{}", result.unwrap_err());
        }
    }
}
