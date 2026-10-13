//! Chunk memory at runtime (ADR 0035, #57 M2 and M3). Pages are created between frames (right
//! after present, [`VoxelRenderer::grow_between_frames`]) and cleared a slice a frame before their
//! first use; before a frame's meshes are placed, [`VoxelRenderer::reserve`] makes room for them
//! (a Clearing page put in use, or one page a class created at once); the chunk table grows ahead
//! or at once, and a larger one starts as a GPU copy of the old one; refused chunks keep their
//! mesh and are retried nearest first once room rose. Shrink drains the emptiest page of a class
//! into the others a few MiB a frame and releases it. `crate::memory::MemoryPolicy` decides when.
use super::{ChunkOp, Resync, VoxelRenderer};
use crate::chunk_table::ChunkTable;
use crate::memory::{
    CHUNK_GPU_BYTES, ChunkMemorySettings, ChunkMemoryStats, DeviceFacts, MemoryPolicy, MoveBudget,
    PageClass, PageGpu, PageState, SLOW_CREATE, Sample, bucket_counts, create_page, evacuate,
};
use crate::path::ListPair;
use crate::upload::{GpuSink, arena_len};
use boxel_gfx::Tier;
use boxel_math::I64Vec3;
use boxel_shaders::gpu::chunk_memory::MAX_PAGES;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Quads of refused chunks retried at most in one frame.
pub(super) const RETRY_QUADS: u64 = 131_072;

/// How [`VoxelRenderer::reserve`] may create pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReserveMode {
    /// A normal frame: at most one page a class is created inside it (an emergency: the
    /// reserve kept between frames should make it rare).
    Normal,
    /// A resync (a new device, lost uploads, a new world): a recovery frame that uploads every
    /// chunk, so as many pages as the meshes need, up to the ceiling.
    Resync,
}

/// What a frame's chunk ops need beyond what their chunks hold ([`VoxelRenderer::demand`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryDemand {
    /// Quads, Solid then Translucent: a new chunk (or one that changes class) counts its whole
    /// range, a remesh its growth past the range it holds plus, once, the largest remesh
    /// (`replace` holds both ranges for a moment). Removes are not subtracted.
    pub quads: [u64; 2],
    /// Chunk places, Solid then Translucent: chunks new to a page.
    pub chunks: [u32; 2],
    /// Slots: chunks new to the table.
    pub slots: u32,
}

/// What [`VoxelRenderer::reserve`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryReserve {
    /// Pages put in use (Clearing pages promoted and pages created).
    pub pages: u32,
    /// Chunk tables grown.
    pub tables: u32,
    /// Creations that failed.
    pub failures: u32,
}

/// A larger chunk table being cleared before it replaces the one in use (High/Mid).
pub(super) struct NextTable {
    buffer: wgpu::Buffer,
    /// Its slots.
    capacity: u32,
    /// Bytes from here on are cleared (below it the copy of the old table lands).
    from: u64,
    /// Bytes cleared from `from`.
    cleared: u64,
}

/// The counters of [`ChunkMemoryStats`] that the runtime keeps.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Counters {
    pub pages_created: u32,
    pub pages_released: u32,
    pub page_create_failures: u32,
    pub emergency_pages: u32,
    pub emergency_tables: u32,
    pub relocated_bytes: u64,
    pub max_relocated_frame: u64,
    pub last_page_create: Duration,
    pub max_page_create: Duration,
}

/// The runtime state of chunk memory (see the module docs).
pub(super) struct Growth {
    /// [`ChunkMemorySettings::grow`], read at GPU start.
    pub grow: bool,
    /// What the device allows (the live ceiling is resolved with it).
    pub facts: DeviceFacts,
    /// Group 2's layout (High/Mid): pages are bound again with it when the table changes.
    pub geometry_layout: Option<wgpu::BindGroupLayout>,
    /// The page ids High's lists covered at GPU start.
    pub initial_lists: u32,
    pub policy: MemoryPolicy,
    /// Added to the clock ([`VoxelRenderer::debug_advance_clock`]).
    clock: Duration,
    /// Pages the policy (or a test) asked for, per class.
    wanted: [bool; 2],
    /// Pages a test asked for ([`VoxelRenderer::debug_force_page`]), whatever the policy says.
    forced: [bool; 2],
    /// A table of this many slots the policy asked for.
    wanted_table: Option<u32>,
    /// Prepares in a row with something wanted and no `grow_between_frames` in between.
    waited: u32,
    /// `grow_between_frames` ran since the last Prepare.
    between: bool,
    next_table: Option<NextTable>,
    /// A copy of the old table into the one in use, for the next `render`: the old table and
    /// the bytes its uploads wrote.
    table_copy: Option<(wgpu::Buffer, u64)>,
    /// High: a list pair being cleared before it replaces the one in use.
    next_lists: Option<ListPair>,
    /// High: shrink left the lists twice as large as needed.
    shrink_lists: bool,
    /// Per class: the Draining page and the chunks it held when it started draining.
    victims: [Option<(u16, Vec<u64>)>; 2],
    /// The retries of the refused chunks.
    retry: RetryPass,
    /// Quads freed by removes since the last retry.
    freed: u64,
    /// The last frame's eye in world voxels (retries go nearest first).
    eye: I64Vec3,
    pub counters: Counters,
    /// Testing: the next creation fails.
    fail_next: bool,
    /// Testing: what every creation counts as having taken, instead of its measured time.
    create_time: Option<Duration>,
    /// Testing: the clear budget and the move budget of a frame instead of the adapter's.
    budgets: Option<(u64, MoveBudget)>,
    /// The slow-creation warning was given.
    warned_slow: bool,
}

impl Growth {
    pub fn new(
        grow: bool,
        facts: DeviceFacts,
        geometry_layout: Option<wgpu::BindGroupLayout>,
        initial_lists: u32,
    ) -> Self {
        Self {
            grow,
            facts,
            geometry_layout,
            initial_lists,
            policy: MemoryPolicy::default(),
            clock: Duration::ZERO,
            wanted: [false; 2],
            forced: [false; 2],
            wanted_table: None,
            waited: 0,
            between: false,
            next_table: None,
            table_copy: None,
            next_lists: None,
            shrink_lists: false,
            victims: [None, None],
            retry: RetryPass::default(),
            freed: 0,
            eye: I64Vec3::ZERO,
            counters: Counters::default(),
            fail_next: false,
            create_time: None,
            budgets: None,
            warned_slow: false,
        }
    }

    /// The renderer's clock: now, plus what the tests advanced it by.
    pub fn now(&self) -> Instant {
        Instant::now() + self.clock
    }

    /// Room rose: the refused chunks are retried (a new pass, from the nearest).
    pub fn room_rose(&mut self) {
        self.retry.arm();
        self.freed = 0;
    }

    /// A removed chunk freed `quads`: a quarter page of them retries the refused chunks.
    pub fn freed(&mut self, quads: u64, page_quads: u32) {
        self.freed += quads;
        if self.freed >= u64::from(page_quads / 4).max(1) {
            self.room_rose();
        }
    }

    /// Every chunk was dropped (lost uploads, a new device's first frame): shrink stops (its
    /// pages are Active again), a table copy is not needed (every written slot goes up again),
    /// what is being cleared goes on clearing.
    pub fn clear_chunks(&mut self) {
        self.victims = [None, None];
        self.table_copy = None;
        self.retry.stop();
        self.freed = 0;
    }

    /// Bits of what is wanted: 1 Solid page, 2 Translucent page, 4 a table.
    pub fn wanted_bits(&self) -> u32 {
        u32::from(self.wanted[0])
            | u32::from(self.wanted[1]) << 1
            | u32::from(self.wanted_table.is_some()) << 2
    }

    /// Pieces in progress: a table being cleared or waiting to be copied, a list pair being
    /// cleared, Draining pages.
    pub fn pending_pieces(&self) -> u32 {
        u32::from(self.next_table.is_some())
            + u32::from(self.table_copy.is_some())
            + u32::from(self.next_lists.is_some())
            + self.victims.iter().flatten().count() as u32
    }
}

/// True for a Clearing page whose last byte was cleared (it waits for nothing, or for lists).
fn is_cleared(page: &crate::memory::Page) -> bool {
    matches!(page.state, PageState::Clearing { cleared }
        if cleared >= u64::from(page.alloc.capacity()) * crate::arena::QUAD_BYTES)
}

/// High's lists cover page ids in steps of 4.
fn list_span(id_span: u32) -> u32 {
    id_span.max(1).next_multiple_of(4).min(MAX_PAGES)
}

impl VoxelRenderer {
    /// Takes the live part of the chunk memory settings (ADR 0035): the ceiling
    /// ([`ChunkMemorySettings::max_quads`], `max_slots`), read every frame by the render plugin.
    /// A lower ceiling stops growth and lets shrink work toward it as chunks unload; nothing
    /// resident is dropped. The page size, the floor and `grow` are read at GPU start only.
    pub fn set_memory_limits(&mut self, settings: &ChunkMemorySettings) {
        self.layout.set_ceiling(settings, self.growth.facts);
    }

    /// What `ops` need from chunk memory (see [`MemoryDemand`]), for
    /// [`VoxelRenderer::reserve`].
    pub fn demand(&self, ops: &[ChunkOp]) -> MemoryDemand {
        let mut demand = MemoryDemand::default();
        let mut largest = [0u64; 2];
        for op in ops {
            let ChunkOp::Upsert { id, mesh, .. } = op else {
                continue;
            };
            if mesh.is_empty() {
                continue;
            }
            let class = PageClass::of(&bucket_counts(mesh));
            let i = class.index();
            let size = u64::from(arena_len(mesh));
            match self.chunks.get(*id) {
                Some((_, record)) if PageClass::of(&record.counts) == class => {
                    let held = u64::from(record.alloc.alloc.size);
                    demand.quads[i] += size.saturating_sub(held);
                    largest[i] = largest[i].max(size);
                }
                Some(_) => {
                    demand.quads[i] += size;
                    demand.chunks[i] += 1;
                }
                None => {
                    demand.quads[i] += size;
                    demand.chunks[i] += 1;
                    demand.slots += 1;
                }
            }
        }
        for (quads, largest) in demand.quads.iter_mut().zip(largest) {
            *quads += largest;
        }
        demand
    }

    /// Makes room for `demand` before the frame's meshes are placed (the render plugin calls it
    /// through [`VoxelRenderer::apply_ops`]): per class, while the Active pages lack its quads
    /// (plus a sixteenth of a page for fragmentation) or chunk places, a Clearing page of the
    /// class is put in use (wgpu zero-fills what was not cleared yet, in this frame), else a page
    /// is created now (at most one a class in [`ReserveMode::Normal`]); the chunk table grows to
    /// hold the new chunks. Nothing with growth off ([`ChunkMemorySettings::grow`]), past the
    /// ceiling or for 10 s after a failed creation: those chunks are refused, kept, and retried
    /// once room rises. A piece the policy asked for that found no frame to be created after
    /// (no camera, a closed gate) for two Prepares is created here, to be cleared as usual.
    pub fn reserve(
        &mut self,
        device: &wgpu::Device,
        demand: &MemoryDemand,
        mode: ReserveMode,
    ) -> MemoryReserve {
        let mut done = MemoryReserve::default();
        if !self.growth.grow {
            return done;
        }
        // the fallback for pieces wanted between frames
        let wanting = self.growth.wanted_bits() != 0;
        if wanting && !self.growth.between {
            self.growth.waited += 1;
        } else {
            self.growth.waited = 0;
        }
        self.growth.between = false;
        if self.growth.waited >= 2 {
            self.growth.waited = 0;
            self.create_wanted(device);
        }
        // the table
        let need = self.chunks.len() as u32 + demand.slots;
        if need > self.chunks.capacity() {
            match self.grow_table_now(device, need) {
                Some(true) => done.tables += 1,
                Some(false) => done.failures += 1,
                None => {}
            }
        }
        // the pages
        let margin = u64::from(self.layout.page_quads / 16);
        for class in PageClass::ALL {
            let i = class.index();
            if demand.quads[i] == 0 && demand.chunks[i] == 0 {
                continue;
            }
            let mut created = 0;
            loop {
                let (free, places) = self.memory.active_room(class);
                if free >= demand.quads[i] + margin && places >= u64::from(demand.chunks[i]) {
                    break;
                }
                if let Some(id) = self.memory.clearing_page(class) {
                    self.activate(device, id);
                    self.growth.counters.emergency_pages += 1;
                    done.pages += 1;
                    continue;
                }
                if mode == ReserveMode::Normal && created >= 1 {
                    break;
                }
                let now = self.growth.now();
                if !self.page_fits_ceiling() || !self.growth.policy.can_create(class, now) {
                    break;
                }
                match self.create_page_now(device, class, PageState::Active) {
                    Ok(_) => {
                        created += 1;
                        done.pages += 1;
                        if mode == ReserveMode::Normal {
                            self.growth.counters.emergency_pages += 1;
                        }
                        self.growth.room_rose();
                    }
                    Err(()) => {
                        done.failures += 1;
                        break;
                    }
                }
            }
        }
        done
    }

    /// True if one more page fits under the ceiling (quads and pages).
    fn page_fits_ceiling(&self) -> bool {
        let pages = self.memory.pages().count() as u32;
        pages < self.layout.max_pages
            && (self.memory.capacity() + u64::from(self.layout.page_quads))
                <= self.layout.ceiling_quads
            && self.memory.next_id() < MAX_PAGES as u16
    }

    /// Creates a page of `class` in `state` with the lowest free id, inside error scopes: its
    /// buffer, Low's light page, High/Mid's group 2 and, on High when its id is past the lists,
    /// a larger list pair (put in use at once for an Active page, cleared first for a Clearing
    /// one). On failure everything made is dropped unused, the class backs off 10 s and the
    /// error is logged. Returns the id and the time it took.
    pub(super) fn create_page_now(
        &mut self,
        device: &wgpu::Device,
        class: PageClass,
        state: PageState,
    ) -> Result<(u16, Duration), ()> {
        let id = self.memory.next_id();
        let quads = self.layout.page_quads;
        let lists_needed = self.path.list_pages().is_some_and(|l| u32::from(id) >= l)
            && self
                .growth
                .next_lists
                .as_ref()
                .is_none_or(|n| u32::from(id) >= n.pages);
        let fail = std::mem::take(&mut self.growth.fail_next);
        let start = Instant::now();
        let mut made: Option<(PageGpu, Option<ListPair>)> = None;
        let result = {
            let geometry = self
                .growth
                .geometry_layout
                .as_ref()
                .zip(self.chunk_buffer.as_ref());
            let (tier, path) = (self.tier, &self.path);
            boxel_gfx::run_in_error_scopes(device, || {
                if fail {
                    return Err("testing: the page creation fails".to_string());
                }
                let page = create_page(device, tier, id, quads, geometry);
                let lists = if lists_needed {
                    path.create_lists(device, list_span(u32::from(id) + 1))
                } else {
                    None
                };
                made = Some((page, lists));
                Ok(())
            })
        };
        let took = self.growth.create_time.unwrap_or_else(|| start.elapsed());
        let now = self.growth.now();
        let (page, lists) = match (result, made) {
            (Ok(()), Some(made)) => made,
            (result, _) => {
                let e = result.err().unwrap_or_default();
                tracing::warn!(
                    "chunk memory: a page of {quads} quads could not be created ({e}); growth of \
                     its class waits 10 s and its chunks wait"
                );
                self.growth.policy.failed(class, now);
                self.growth.counters.page_create_failures += 1;
                return Err(());
            }
        };
        let added = self.memory.add_page_in(class, state, quads);
        debug_assert_eq!(added, id);
        self.pages.insert(id, page);
        if let Some(lists) = lists {
            self.growth.next_lists = Some(lists);
            self.growth.shrink_lists = false;
        }
        if state == PageState::Active {
            self.put_lists_in_use(device, id);
        }
        self.growth.counters.pages_created += 1;
        if self.growth.policy.created(class, now, took) && !self.growth.warned_slow {
            self.growth.warned_slow = true;
            tracing::warn!(
                "chunk memory: a page creation took {took:?} (over {SLOW_CREATE:?}: the driver \
                 needed a new memory block); pages are now asked for earlier (ADR 0035, decision 7)"
            );
        }
        Ok((id, took))
    }

    /// High: when page `id` is past the lists in use, the larger pair goes in use now (wgpu
    /// zero-fills what was not cleared yet), or one is created for it.
    fn put_lists_in_use(&mut self, device: &wgpu::Device, id: u16) {
        let Some(pages) = self.path.list_pages() else {
            return;
        };
        if u32::from(id) < pages {
            return;
        }
        let lists = match self.growth.next_lists.take() {
            Some(lists) if u32::from(id) < lists.pages => lists,
            _ => match self.path.create_lists(device, list_span(u32::from(id) + 1)) {
                Some(lists) => lists,
                None => return,
            },
        };
        if let Some(table) = &self.chunk_buffer {
            self.path.swap_lists(device, lists, table);
        }
    }

    /// Puts Clearing page `id` in use (and the lists it needs).
    fn activate(&mut self, device: &wgpu::Device, id: u16) {
        self.memory.set_state(id, PageState::Active);
        self.put_lists_in_use(device, id);
        self.growth.room_rose();
    }

    /// Grows the chunk table to hold `need` chunks now: a table being cleared goes in use if it
    /// is large enough, else one of the next power of two (at most the ceiling) is created.
    /// `None`: nothing to do (the ceiling), `Some(false)`: the creation failed.
    fn grow_table_now(&mut self, device: &wgpu::Device, need: u32) -> Option<bool> {
        let capacity = self.chunks.capacity();
        let ceiling = self.layout.ceiling_slots;
        if capacity >= ceiling {
            return None;
        }
        let now = self.growth.now();
        if self.chunk_buffer.is_none() {
            // Low: no GPU table, the CPU part at once
            self.chunks
                .grow(need.next_power_of_two().clamp(capacity, ceiling));
            self.growth.room_rose();
            return Some(true);
        }
        // a larger table being cleared goes in use now if it is large enough (else dropped)
        if let Some(next) = self.growth.next_table.take()
            && next.capacity >= need
        {
            self.swap_table(device, next.buffer, next.capacity);
            self.growth.counters.emergency_tables += 1;
            return Some(true);
        }
        if !self.growth.policy.can_grow_table(now) {
            return None;
        }
        let slots = need.next_power_of_two().clamp(capacity, ceiling);
        match self.create_table(device, slots) {
            Some(buffer) => {
                self.swap_table(device, buffer, slots);
                self.growth.counters.emergency_tables += 1;
                Some(true)
            }
            None => Some(false),
        }
    }

    /// A chunk table of `slots` slots, in error scopes (a failure backs table growth off 10 s).
    fn create_table(&mut self, device: &wgpu::Device, slots: u32) -> Option<wgpu::Buffer> {
        let mut made = None;
        let result = boxel_gfx::run_in_error_scopes(device, || {
            made = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel chunk table"),
                size: u64::from(slots) * CHUNK_GPU_BYTES,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            Ok(())
        });
        match (result, made) {
            (Ok(()), Some(buffer)) => Some(buffer),
            (result, _) => {
                tracing::warn!(
                    "chunk memory: a chunk table of {slots} slots could not be created ({}); \
                     table growth waits 10 s and new chunks wait",
                    result.err().unwrap_or_default()
                );
                let now = self.growth.now();
                self.growth.policy.table_failed(now);
                self.growth.counters.page_create_failures += 1;
                None
            }
        }
    }

    /// Puts `buffer` (`capacity` slots, more than the table in use) in use as the chunk table:
    /// the old one is copied into it at the start of the next `render` (only the slots uploads
    /// wrote; a copy still pending keeps its source, the last table a frame wrote), every page's
    /// group 2 and the tier's table-sized buffers are built again, and the cave culling bits are
    /// written again.
    fn swap_table(&mut self, device: &wgpu::Device, buffer: wgpu::Buffer, capacity: u32) {
        debug_assert!(capacity > self.chunks.capacity(), "a table never shrinks");
        let old = self.chunk_buffer.replace(buffer);
        if self.growth.table_copy.is_none()
            && let Some(old) = old
        {
            let bytes = u64::from(self.chunks.written_slots()) * CHUNK_GPU_BYTES;
            self.growth.table_copy = Some((old, bytes));
        }
        self.chunks.grow(capacity);
        if let (Some(layout), Some(table)) = (&self.growth.geometry_layout, &self.chunk_buffer) {
            self.pages.rebind(device, layout, table);
        }
        self.path
            .resize_table(device, self.chunk_buffer.as_ref(), capacity);
        // High's cull state is new: its bits go up with the next frame that has a set
        self.occlusion.built = None;
        self.growth.room_rose();
    }

    /// Creates one piece the policy asked for, as between frames.
    fn create_wanted(&mut self, device: &wgpu::Device) -> Option<Duration> {
        for class in PageClass::ALL {
            if !self.growth.wanted[class.index()] {
                continue;
            }
            self.growth.wanted[class.index()] = false;
            self.growth.forced[class.index()] = false;
            if !self.page_fits_ceiling() {
                continue;
            }
            let start = Instant::now();
            let state = PageState::Clearing { cleared: 0 };
            if self.create_page_now(device, class, state).is_ok() {
                let took = start.elapsed();
                let counters = &mut self.growth.counters;
                counters.last_page_create = took;
                counters.max_page_create = counters.max_page_create.max(took);
                return Some(took);
            }
            return None;
        }
        // (one asked for before the table grew past it at once is not wanted any more: built and
        // swapped in, the flush would write past its end)
        if let Some(slots) = self.growth.wanted_table.take()
            && slots > self.chunks.capacity()
        {
            let start = Instant::now();
            if self.chunk_buffer.is_none() {
                self.chunks.grow(slots);
                self.growth.room_rose();
            } else if self.growth.next_table.is_none()
                && let Some(buffer) = self.create_table(device, slots)
            {
                let from = u64::from(self.chunks.written_slots()) * CHUNK_GPU_BYTES;
                self.growth.next_table = Some(NextTable {
                    buffer,
                    capacity: slots,
                    from,
                    cleared: 0,
                });
            }
            return Some(start.elapsed());
        }
        if std::mem::take(&mut self.growth.shrink_lists)
            && self.growth.next_lists.is_none()
            && let Some(pages) = self.path.list_pages()
        {
            let want = list_span(self.memory.id_span()).max(self.growth.initial_lists);
            if want < pages {
                let start = Instant::now();
                self.growth.next_lists = self.path.create_lists(device, want);
                return Some(start.elapsed());
            }
        }
        None
    }

    /// Between frames (the render plugin calls it right after present, and after an offscreen
    /// frame's submit; not when the frame lost the device): creates at most one piece the
    /// policy asked for after the last frame, a page in state Clearing (with High's larger
    /// lists when it needs them) or a larger chunk table, timed (a creation over 2 ms doubles
    /// the class's reserve once and warns). Returns the time it took, `None` if nothing was
    /// created. A page is cleared by the next frames and takes chunks once clear.
    pub fn grow_between_frames(&mut self, device: &wgpu::Device) -> Option<Duration> {
        self.growth.between = true;
        if !self.growth.grow {
            return None;
        }
        let _span = frame_span!("render.page_create");
        self.create_wanted(device)
    }

    /// Retries the refused chunks kept with their mesh once room rose (a page went in use, the
    /// table grew, removes freed a quarter page): one pass over all of them, nearest to the last
    /// frame's eye first, at most 131,072 quads a frame (a chunk refused again counts too), then
    /// nothing until room rises again. A chunk placed now is reported shown
    /// ([`VoxelRenderer::take_refused`]); one refused again changes nothing and is not counted
    /// again in [`crate::RenderStats::dropped_chunks`]. [`VoxelRenderer::apply_ops`] calls it.
    pub fn retry_refused(&mut self) {
        if !self.growth.retry.pending() {
            return;
        }
        if self.refused.retained.is_empty() {
            self.growth.retry.stop();
            return;
        }
        let ids = self
            .growth
            .retry
            .batch(&self.refused.retained, self.growth.eye, RETRY_QUADS);
        for id in ids {
            let Some(kept) = self.refused.retained.get(&id) else {
                continue;
            };
            let (origin, mesh) = (kept.origin, Arc::clone(&kept.mesh));
            let _ = self.upsert_inner(id, origin, mesh, false);
        }
    }

    /// Chunk memory's work between frames, at the start of `render` (span `render.pages`): the
    /// pending table copy, the clears of Clearing pages, list pairs and a larger table within the
    /// clear budget (and the swaps of whatever is clear), the evacuation of Draining pages within
    /// the move budget, and the release of emptied ones.
    pub(super) fn maintain(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        let mut budget = self
            .growth
            .budgets
            .map_or(self.clear_budget, |(clear, _)| clear);
        // a larger list pair, before the pages that wait for it
        if let Some(lists) = &mut self.growth.next_lists {
            budget -= lists.clear_some(encoder, budget);
        }
        // a larger table ahead: cleared above what the copy brings, then put in use (dropped if
        // the table grew past it at once meanwhile)
        if self
            .growth
            .next_table
            .as_ref()
            .is_some_and(|n| n.capacity <= self.chunks.capacity())
        {
            self.growth.next_table = None;
        }
        if let Some(next) = &mut self.growth.next_table {
            let total = next.buffer.size() - next.from;
            let n = (total - next.cleared).min(budget);
            if n > 0 {
                encoder.clear_buffer(&next.buffer, next.from + next.cleared, Some(n));
                next.cleared += n;
                budget -= n;
            }
        }
        if self
            .growth
            .next_table
            .as_ref()
            .is_some_and(|n| n.cleared >= n.buffer.size() - n.from)
            && let Some(next) = self.growth.next_table.take()
        {
            self.swap_table(device, next.buffer, next.capacity);
        }
        // the old table's slots into the one in use, before this frame's uploads
        if let Some((old, bytes)) = self.growth.table_copy.take()
            && let Some(table) = &self.chunk_buffer
            && bytes > 0
        {
            encoder.copy_buffer_to_buffer(&old, 0, table, 0, bytes.min(old.size()));
        }
        let low_light = self.tier == Tier::Low;
        let mut moves = self
            .growth
            .budgets
            .map_or(MoveBudget::of(self.growth.facts.class), |(_, moves)| moves);
        let now = self.growth.now();
        let relocated = {
            let mut sink = GpuSink {
                device,
                encoder,
                pages: &self.pages,
                table: self.chunk_buffer.as_ref(),
                copies: &mut self.copies,
            };
            crate::memory::clear_pages(&mut self.memory, &mut sink, &mut budget, low_light);
            // evacuation (the releases follow)
            let mut relocated = 0;
            for class in PageClass::ALL {
                let Some((victim, ids)) = &mut self.growth.victims[class.index()] else {
                    continue;
                };
                let before = self.memory.page(*victim).map_or(0, |p| p.chunks);
                relocated += evacuate(
                    &mut self.memory,
                    &mut self.chunks,
                    &mut self.uploads,
                    &mut sink,
                    *victim,
                    ids,
                    &mut moves,
                    low_light,
                );
                if self.memory.page(*victim).map_or(0, |p| p.chunks) < before {
                    self.growth.policy.progressed(class, now);
                }
            }
            relocated
        };
        let counters = &mut self.growth.counters;
        counters.relocated_bytes += relocated;
        counters.max_relocated_frame = counters.max_relocated_frame.max(relocated);
        for class in PageClass::ALL {
            let i = class.index();
            let Some((victim, _)) = &self.growth.victims[i] else {
                continue;
            };
            let victim = *victim;
            if self.memory.page(victim).is_some_and(|p| p.chunks == 0) {
                self.memory.remove_page(victim);
                drop(self.pages.take(victim));
                self.growth.victims[i] = None;
                self.growth.counters.pages_released += 1;
                if let Some(pages) = self.path.list_pages()
                    && list_span(self.memory.id_span()).max(self.growth.initial_lists) <= pages / 2
                {
                    self.growth.shrink_lists = true;
                }
            }
        }
        // a list pair that is clear goes in use, then every cleared page the lists cover
        self.swap_clear_lists(device);
        let lists = self.path.list_pages();
        let ready: Vec<u16> = self
            .memory
            .pages()
            .filter(|&(id, p)| is_cleared(p) && lists.is_none_or(|l| u32::from(id) < l))
            .map(|(id, _)| id)
            .collect();
        for id in ready {
            self.activate(device, id);
        }
        self.pages.sync(&self.memory);
    }

    /// High: a list pair that is clear goes in use: a larger one at once, a smaller one (shrink
    /// asked for it) only while every page id still fits it.
    fn swap_clear_lists(&mut self, device: &wgpu::Device) {
        let Some(pages) = self.path.list_pages() else {
            return;
        };
        if !self
            .growth
            .next_lists
            .as_ref()
            .is_some_and(ListPair::is_clear)
        {
            return;
        }
        let (Some(lists), Some(table)) = (self.growth.next_lists.take(), &self.chunk_buffer) else {
            return;
        };
        if lists.pages >= pages || self.memory.id_span() <= lists.pages {
            self.path.swap_lists(device, lists, table);
        }
    }

    /// After a frame (end of `render`): the policy's decision for the next ones (pages and a
    /// table to create between frames, victims to drain, victims to keep).
    pub(super) fn decide_growth(&mut self, eye: I64Vec3) {
        self.growth.eye = eye;
        let now = self.growth.now();
        let sample = self.memory_sample();
        let decision = self.growth.policy.decide(&sample, now, self.growth.grow);
        for class in PageClass::ALL {
            let i = class.index();
            self.growth.wanted[i] = decision.want[i] || self.growth.forced[i];
            if decision.cancel[i]
                && let Some((victim, _)) = self.growth.victims[i].take()
            {
                self.memory.set_state(victim, PageState::Active);
                self.growth.room_rose();
            }
            if let Some(victim) = decision.drain[i] {
                self.start_draining(class, victim);
            }
        }
        self.growth.wanted_table = decision.want_table;
    }

    /// Page `victim` of `class` starts draining.
    fn start_draining(&mut self, class: PageClass, victim: u16) {
        if self.growth.victims[class.index()].is_some()
            || self
                .memory
                .page(victim)
                .is_none_or(|p| p.state != PageState::Active)
        {
            return;
        }
        self.memory.set_state(victim, PageState::Draining);
        let ids: Vec<u64> = self
            .chunks
            .iter()
            .filter(|(_, r)| r.alloc.page == victim)
            .map(|(_, r)| r.id)
            .collect();
        self.growth.victims[class.index()] = Some((victim, ids));
        let now = self.growth.now();
        self.growth.policy.progressed(class, now);
    }

    /// The policy's view of chunk memory now.
    fn memory_sample(&self) -> Sample {
        let mut classes = self.memory.sample_classes(self.layout.page_quads);
        let largest = self.memory.largest_free();
        // chunks refused for a slot wait for pages only while the table can still grow: at the
        // slot ceiling no page helps them
        let table_grows = self.chunks.capacity() < self.layout.ceiling_slots
            && self.growth.policy.can_grow_table(self.growth.now());
        for (i, class) in classes.iter_mut().enumerate() {
            let (pages, slots) = (&self.refused.waiting[i], &self.refused.for_slots[i]);
            let (quads, size) = if table_grows {
                (
                    pages.quads + slots.quads,
                    pages.largest().max(slots.largest()),
                )
            } else {
                (pages.quads, pages.largest())
            };
            class.waiting = quads;
            // a waiting chunk larger than every free range: growth, not the free quads, helps
            class.blocked = size > largest;
        }
        Sample {
            classes,
            page_quads: self.layout.page_quads,
            page_chunks: self.layout.page_chunks,
            pages: self.memory.pages().count() as u32,
            capacity: self.memory.capacity(),
            ceiling_quads: self.layout.ceiling_quads,
            max_pages: self.layout.max_pages.min(MAX_PAGES),
            floor_quads: self.layout.floor_quads(),
            floor_pages: self.layout.floor.len() as u32,
            slots: self.chunks.capacity(),
            used_slots: self.chunks.len() as u32,
            max_slots: self.layout.ceiling_slots,
            table_pending: self.growth.next_table.is_some(),
        }
    }

    /// The chunk memory part of [`VoxelRenderer::clear_world`]: what a fresh renderer starts
    /// with. The pages go back to the floor (the floor's pages kept where a page of that size
    /// exists, renumbered as a fresh renderer numbers them; the others dropped, never
    /// `destroy()`ed), the table, High's lists, the cull state and the refs back to their
    /// initial sizes (wgpu zero-fills those few MiB in the next frame), everything wanted or in
    /// progress dropped, the counters reset. What the device taught stays: the failure
    /// backoffs, a doubled reserve, the slowest creation.
    pub(super) fn reset_memory(&mut self, device: &wgpu::Device) {
        let g = &mut self.growth;
        g.wanted = [false; 2];
        g.forced = [false; 2];
        g.wanted_table = None;
        g.waited = 0;
        g.next_table = None;
        g.table_copy = None;
        g.next_lists = None;
        g.shrink_lists = false;
        g.victims = [None, None];
        g.retry.stop();
        g.freed = 0;
        g.policy.forget_world();
        g.counters = Counters {
            max_page_create: g.counters.max_page_create,
            ..Counters::default()
        };
        // the pages: the floor's sizes, in its order
        let mut old: Vec<PageGpu> = (0..self.memory.id_span())
            .filter_map(|id| self.pages.take(id as u16))
            .collect();
        self.memory = crate::memory::ChunkMemory::with_floor(&self.layout);
        let geometry = self
            .growth
            .geometry_layout
            .as_ref()
            .zip(self.chunk_buffer.as_ref());
        let floor: Vec<(u16, u32)> = self
            .memory
            .pages()
            .map(|(id, p)| (id, p.alloc.capacity()))
            .collect();
        for (id, quads) in floor {
            let bytes = u64::from(quads) * crate::arena::QUAD_BYTES;
            let page = match old.iter().position(|p| p.quads.size() == bytes) {
                Some(i) => old.swap_remove(i),
                None => create_page(device, self.tier, id, quads, geometry),
            };
            self.pages.insert(id, page);
        }
        drop(old);
        // the table and what follows it
        if self.chunks.capacity() != self.layout.slots {
            self.chunks = ChunkTable::new(self.layout.slots);
            if self.chunk_buffer.is_some() {
                self.chunk_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("boxel chunk table"),
                    size: u64::from(self.layout.slots) * CHUNK_GPU_BYTES,
                    usage: wgpu::BufferUsages::STORAGE
                        | wgpu::BufferUsages::COPY_DST
                        | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }));
                if let (Some(layout), Some(table)) =
                    (&self.growth.geometry_layout, &self.chunk_buffer)
                {
                    self.pages.rebind(device, layout, table);
                }
            }
            self.path
                .resize_table(device, self.chunk_buffer.as_ref(), self.layout.slots);
        }
        if self
            .path
            .list_pages()
            .is_some_and(|l| l != self.growth.initial_lists)
            && let Some(lists) = self.path.create_lists(device, self.growth.initial_lists)
            && let Some(table) = &self.chunk_buffer
        {
            self.path.swap_lists(device, lists, table);
        }
        self.pages.sync(&self.memory);
    }

    /// The runtime part of [`ChunkMemoryStats`].
    pub(super) fn growth_stats(&self, stats: ChunkMemoryStats) -> ChunkMemoryStats {
        let c = &self.growth.counters;
        let (mut clearing, mut draining) = (0, 0);
        for (_, page) in self.memory.pages() {
            match page.state {
                PageState::Clearing { .. } => clearing += 1,
                PageState::Draining => draining += 1,
                PageState::Active => {}
            }
        }
        ChunkMemoryStats {
            clearing_pages: clearing,
            draining_pages: draining,
            refused_waiting: self.refused.retained.len() as u32,
            table_slots: self.chunks.capacity(),
            pages_created: c.pages_created,
            pages_released: c.pages_released,
            page_create_failures: c.page_create_failures,
            emergency_pages: c.emergency_pages,
            emergency_tables: c.emergency_tables,
            relocated_bytes: c.relocated_bytes,
            max_relocated_frame: c.max_relocated_frame,
            last_page_create: c.last_page_create,
            max_page_create: c.max_page_create,
            ..stats
        }
    }

    /// Testing: asks for a page of the class (`translucent`) between frames, whatever the
    /// policy says (the next [`VoxelRenderer::grow_between_frames`] creates it).
    #[doc(hidden)]
    pub fn debug_force_page(&mut self, translucent: bool) {
        let class = if translucent {
            PageClass::Translucent
        } else {
            PageClass::Solid
        };
        self.growth.wanted[class.index()] = true;
        self.growth.forced[class.index()] = true;
    }

    /// Testing: page `id` starts draining now (its chunks move to the other pages over the next
    /// frames, then it is released), whatever the policy says. False if it is not an Active
    /// page or its class already drains one.
    #[doc(hidden)]
    pub fn debug_evacuate_page(&mut self, id: u16) -> bool {
        let Some(class) = self
            .memory
            .page(id)
            .filter(|p| p.state == PageState::Active)
            .map(|p| p.class)
        else {
            return false;
        };
        if self.growth.victims[class.index()].is_some() {
            return false;
        }
        self.start_draining(class, id);
        true
    }

    /// Testing: the next page creation (with its lists) fails as when out of memory.
    #[doc(hidden)]
    pub fn debug_fail_next_page(&mut self) {
        self.growth.fail_next = true;
    }

    /// Testing: every page creation counts as taking `took` instead of the time it took, so the
    /// slow-creation doubling of the reserve (ADR 0035, decision 7) follows the test and not how
    /// busy the machine is; `None` measures again.
    #[doc(hidden)]
    pub fn debug_set_page_create_time(&mut self, took: Option<Duration>) {
        self.growth.create_time = took;
    }

    /// Testing: what one frame may clear (bytes, a multiple of 8) and move (bytes and copy
    /// commands) instead of the adapter's budgets, so tests see pieces half cleared and pages
    /// half evacuated; `None` gives the adapter's back.
    #[doc(hidden)]
    pub fn debug_set_budgets(&mut self, budgets: Option<(u64, u64, u32)>) {
        self.growth.budgets =
            budgets.map(|(clear, bytes, copies)| (clear & !7, MoveBudget::new(bytes, copies)));
    }

    /// Testing: the adapter's move budget of one frame in bytes (Low counts its light copies).
    #[doc(hidden)]
    pub fn debug_move_budget(&self) -> u64 {
        MoveBudget::of(self.growth.facts.class).bytes
    }

    /// Testing: moves the renderer's clock (shrink holds, failure backoffs) forward by `by`.
    #[doc(hidden)]
    pub fn debug_advance_clock(&mut self, by: Duration) {
        self.growth.clock += by;
    }

    /// Testing: the pages, `(id, translucent, quads, state)` with state 0 Clearing, 1 Active,
    /// 2 Draining, in id order.
    #[doc(hidden)]
    pub fn debug_pages(&self) -> Vec<(u16, bool, u32, u8)> {
        self.memory
            .pages()
            .map(|(id, p)| {
                let state = match p.state {
                    PageState::Clearing { .. } => 0,
                    PageState::Active => 1,
                    PageState::Draining => 2,
                };
                (
                    id,
                    p.class == PageClass::Translucent,
                    p.alloc.capacity(),
                    state,
                )
            })
            .collect()
    }

    /// One frame's chunk changes, as the render plugin applies them before each frame (GPU tests
    /// call it to run the plugin's exact sequence): first the clear `resync` asks for
    /// ([`Resync::Chunks`]: [`VoxelRenderer::clear_chunks`], for a new renderer, lost uploads or
    /// a rejected frame; [`Resync::World`]: [`VoxelRenderer::clear_world`], the world closed),
    /// after which `ops` holds every chunk again; then the room the ops need
    /// ([`VoxelRenderer::demand`], [`VoxelRenderer::reserve`]: [`ReserveMode::Resync`] after a
    /// clear), each op in order, and the refused chunks' retries
    /// ([`VoxelRenderer::retry_refused`], not on a resync frame). Returns the refusals (each also
    /// counted in [`crate::RenderStats::dropped_chunks`] and reported by
    /// [`VoxelRenderer::take_refused`]).
    pub fn apply_ops(
        &mut self,
        device: &wgpu::Device,
        ops: impl IntoIterator<Item = ChunkOp>,
        resync: Resync,
    ) -> Vec<crate::RendererError> {
        match resync {
            Resync::None => {}
            Resync::Chunks => self.clear_chunks(),
            Resync::World => self.clear_world(device),
        }
        let ops: Vec<ChunkOp> = ops.into_iter().collect();
        let mode = if resync == Resync::None {
            ReserveMode::Normal
        } else {
            ReserveMode::Resync
        };
        let demand = self.demand(&ops);
        self.reserve(device, &demand, mode);
        let mut errors = Vec::new();
        for op in ops {
            let result = match op {
                ChunkOp::Upsert { id, origin, mesh } => {
                    let _span = frame_span!("render.upsert");
                    self.upsert_chunk(id, origin, mesh)
                }
                ChunkOp::Remove { id } => {
                    let _span = frame_span!("render.remove");
                    self.remove_chunk(id);
                    Ok(())
                }
            };
            if let Err(e) = result {
                errors.push(e);
            }
        }
        if mode == ReserveMode::Normal {
            self.retry_refused();
        }
        errors
    }
}

/// A refused chunk kept for its retry (see `Refused::retained`).
#[derive(Clone, Debug)]
pub(super) struct Kept {
    pub origin: I64Vec3,
    /// The `Arc` the world holds, not a copy.
    pub mesh: Arc<boxel_mesh::ChunkMesh>,
    /// Its class (`PageClass::index`).
    pub class: usize,
    /// The quads it takes in a page.
    pub quads: u32,
    /// Its last refusal was for want of a chunk slot (else of pages).
    pub table: bool,
}

impl Kept {
    pub fn new(origin: I64Vec3, mesh: Arc<boxel_mesh::ChunkMesh>, table: bool) -> Self {
        Self {
            origin,
            class: PageClass::of(&bucket_counts(&mesh)).index(),
            quads: arena_len(&mesh),
            mesh,
            table,
        }
    }
}

/// The refused chunks kept by id.
pub(super) type Retained = HashMap<u64, Kept>;

/// The refused chunks of one class waiting for room: their quads (growth makes room for them)
/// and their sizes (one larger than every free range asks for a page).
#[derive(Debug, Default)]
pub(super) struct Waiting {
    pub quads: u64,
    /// How many wait, by size.
    sizes: BTreeMap<u32, u32>,
}

impl Waiting {
    pub fn add(&mut self, size: u32) {
        self.quads += u64::from(size);
        *self.sizes.entry(size).or_default() += 1;
    }

    pub fn remove(&mut self, size: u32) {
        self.quads -= u64::from(size);
        if let std::collections::btree_map::Entry::Occupied(mut n) = self.sizes.entry(size) {
            *n.get_mut() -= 1;
            if *n.get() == 0 {
                n.remove();
            }
        }
    }

    /// The largest waiting chunk's quads (0: none waits).
    pub fn largest(&self) -> u32 {
        self.sizes.last_key_value().map_or(0, |(&size, _)| size)
    }
}

/// The retries of the refused chunks (ADR 0035): once room rose, one pass over all of them,
/// nearest to the eye first, a budget of quads a frame (a chunk refused again spends it too),
/// and nothing more until room rises again. Room that rises during a pass starts a new one from
/// the nearest. A pass is finite, so chunks waiting past the ceiling cost nothing while nothing
/// frees, and a far chunk that fits a hole the nearer ones do not is reached.
#[derive(Debug, Default)]
pub(super) struct RetryPass {
    /// Room rose: the next batch starts a pass.
    armed: bool,
    /// The pass in progress: the ids nearest first, and the next one to try.
    order: Vec<u64>,
    at: usize,
}

impl RetryPass {
    /// Room rose.
    pub fn arm(&mut self) {
        self.armed = true;
    }

    /// Nothing waits any more.
    pub fn stop(&mut self) {
        *self = Self::default();
    }

    /// A pass waits to start or is in progress.
    pub fn pending(&self) -> bool {
        self.armed || self.at < self.order.len()
    }

    /// This frame's chunks to try: the next ones of the pass (a new pass of `kept` sorted
    /// nearest to `eye` first if room rose) until they take `budget` quads. Ids no longer kept
    /// are skipped; chunks kept after the pass started wait for the next one.
    pub fn batch(&mut self, kept: &Retained, eye: I64Vec3, budget: u64) -> Vec<u64> {
        if std::mem::take(&mut self.armed) {
            let mut order: Vec<(i128, u64)> = kept
                .iter()
                .map(|(&id, k)| {
                    let d = k.origin + I64Vec3::splat(16) - eye;
                    let (x, y, z) = (i128::from(d.x), i128::from(d.y), i128::from(d.z));
                    (x * x + y * y + z * z, id)
                })
                .collect();
            order.sort_unstable();
            self.order = order.into_iter().map(|(_, id)| id).collect();
            self.at = 0;
        }
        let mut ids = Vec::new();
        let mut left = budget;
        while left > 0
            && let Some(&id) = self.order.get(self.at)
        {
            self.at += 1;
            if let Some(k) = kept.get(&id) {
                left = left.saturating_sub(u64::from(k.quads));
                ids.push(id);
            }
        }
        if self.at >= self.order.len() {
            self.order = Vec::new();
            self.at = 0;
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_cover_page_ids_in_steps_of_four() {
        assert_eq!(list_span(0), 4);
        assert_eq!(list_span(1), 4);
        assert_eq!(list_span(4), 4);
        assert_eq!(list_span(5), 8);
        assert_eq!(list_span(MAX_PAGES), MAX_PAGES);
        assert_eq!(list_span(MAX_PAGES + 3), MAX_PAGES);
    }

    /// `n` kept chunks of `quads` each along +x, id `i` at chunk `i` (nearer for a lower id).
    fn kept(n: u64, quads: u32) -> Retained {
        (0..n)
            .map(|i| {
                let k = Kept {
                    origin: I64Vec3::new(i as i64 * 32, 0, 0),
                    mesh: Arc::default(),
                    class: 0,
                    quads,
                    table: false,
                };
                (i, k)
            })
            .collect()
    }

    /// Past the ceiling, far more quads wait than one frame retries: a pass tries each chunk
    /// once, nearest first, over several frames, then nothing until room rises again (it used
    /// to retry the same nearest chunks every frame, forever, and never reach the far ones).
    #[test]
    fn a_retry_pass_tries_every_chunk_once_then_waits_for_room() {
        let kept = kept(1000, 1000);
        let mut pass = RetryPass::default();
        assert!(!pass.pending());
        assert!(pass.batch(&kept, I64Vec3::ZERO, RETRY_QUADS).is_empty());
        pass.arm();
        let mut tried = Vec::new();
        let mut frames = 0;
        while pass.pending() {
            let batch = pass.batch(&kept, I64Vec3::ZERO, RETRY_QUADS);
            let quads: u64 = batch.iter().map(|id| u64::from(kept[id].quads)).sum();
            assert!(quads < RETRY_QUADS + 1000, "a frame's budget");
            tried.extend(batch);
            frames += 1;
            assert!(frames < 100, "the pass never ends");
        }
        assert_eq!(
            tried,
            (0..1000).collect::<Vec<_>>(),
            "each once, nearest first"
        );
        assert_eq!(frames, 8);
        // nothing until room rises again, then a new pass from the nearest
        assert!(pass.batch(&kept, I64Vec3::ZERO, RETRY_QUADS).is_empty());
        pass.arm();
        assert_eq!(pass.batch(&kept, I64Vec3::ZERO, 3000), [0, 1, 2]);
    }

    /// Room that rises during a pass starts a new one from the nearest (to the new eye); chunks
    /// shown or removed during a pass are skipped, chunks refused during it wait for the next.
    #[test]
    fn room_rising_during_a_pass_starts_over_from_the_nearest() {
        let mut kept = kept(10, 100);
        let mut pass = RetryPass::default();
        pass.arm();
        assert_eq!(pass.batch(&kept, I64Vec3::ZERO, 300), [0, 1, 2]);
        kept.remove(&3);
        kept.insert(
            10,
            Kept {
                origin: I64Vec3::new(-64, 0, 0),
                mesh: Arc::default(),
                class: 0,
                quads: 100,
                table: false,
            },
        );
        assert_eq!(pass.batch(&kept, I64Vec3::ZERO, 200), [4, 5]);
        pass.arm();
        let eye = I64Vec3::new(9 * 32 + 16, 16, 16);
        assert_eq!(pass.batch(&kept, eye, 300), [9, 8, 7]);
        pass.stop();
        assert!(!pass.pending());
    }

    #[test]
    fn waiting_chunks_keep_their_quads_and_largest_size() {
        let mut waiting = Waiting::default();
        assert_eq!((waiting.quads, waiting.largest()), (0, 0));
        for size in [10, 30, 30, 20] {
            waiting.add(size);
        }
        assert_eq!((waiting.quads, waiting.largest()), (90, 30));
        waiting.remove(30);
        assert_eq!((waiting.quads, waiting.largest()), (60, 30));
        waiting.remove(30);
        assert_eq!((waiting.quads, waiting.largest()), (30, 20));
        waiting.remove(10);
        waiting.remove(20);
        assert_eq!((waiting.quads, waiting.largest()), (0, 0));
    }
}
