//! Per-frame uploads. Chunk meshes and chunk table entries changed since the last frame reach the
//! GPU together at the start of the next frame: one staging buffer, and one copy per contiguous
//! range recorded into the frame's encoder before any pass reads them. `Queue::write_buffer`
//! creates (and later frees) a staging buffer per call, a few microseconds each, and crossing a
//! chunk border changes a few hundred chunks.
//!
//! A chunk's range in its page (`crate::memory`) holds its quads and then its light palette
//! (ADR 0015), which High and Mid read at `ChunkGpu::light_base`. Low has no storage buffers:
//! there each quad's palette entry is written, expanded, at the quad's own offset in a light page
//! parallel to the page (its second instance attribute).
//!
//! A re-sorted translucent bucket (`crate::sort`) goes the same way: its permuted quads over the
//! bucket's own range, after the meshes of the frame (a mesh uploaded in the same frame is
//! written first), and on Low their expanded light over the same range of the light page.
//!
//! The writes go through a [`CopySink`]: [`GpuSink`] stages them and records the copies, the
//! memory tests replay them over byte models of the buffers.
use crate::arena::QUAD_BYTES;
use crate::chunk_table::ChunkTable;
use crate::memory::{CopySink, MemOp, MemTarget, PageSet, PagedAlloc};
use crate::sort::SortedBucket;
use boxel_mesh::{ChunkMesh, FULL_SKY_ENTRY, PackedQuad, PaletteEntry, TRANSLUCENT_BUCKET};
use boxel_shaders::gpu::ChunkGpu;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

/// Bytes of one chunk table entry.
const ENTRY_BYTES: u64 = size_of::<ChunkGpu>() as u64;

/// Table runs at most this many up-to-date slots apart are sent as one: 16 entries more (1 KiB)
/// cost about as much as recording another copy.
const TABLE_MERGE_GAP: u32 = 16;

/// Largest staging buffer: a bigger frame (a full resync of a large world) is split into several,
/// and so is a run of meshes that follow each other in the arena. Only a single mesh larger than
/// this (far more than a chunk can hold) would go in one buffer of its own size.
const STAGING_BYTES: u64 = 64 << 20;

/// A mesh waiting for its upload.
#[derive(Debug)]
struct PendingMesh {
    /// The chunk's page.
    page: u16,
    /// First quad in the page (the chunk's current range).
    offset: u32,
    /// The mesh, shared with the world: its quads are copied once, into the staging buffer.
    mesh: Arc<ChunkMesh>,
}

/// Light palette entries a mesh takes in the arena: its palette, or one open-sky entry when it
/// has none (a hand-made mesh), so every quad's index 0 reads its own chunk's data.
pub(crate) fn light_entries(mesh: &ChunkMesh) -> u32 {
    mesh.palette.len().max(1) as u32
}

/// The palette a mesh is uploaded with (see [`light_entries`]).
pub(crate) fn uploaded_palette(mesh: &ChunkMesh) -> &[PaletteEntry] {
    if mesh.palette.is_empty() {
        std::slice::from_ref(&FULL_SKY_ENTRY)
    } else {
        &mesh.palette
    }
}

/// Page entries (8 bytes each) a chunk mesh takes: its quads plus its light palette (at least
/// one entry). [`RenderStats::arena_used_quads`](crate::RenderStats) counts these.
pub fn arena_len(mesh: &ChunkMesh) -> u32 {
    mesh.total_quads() + light_entries(mesh)
}

impl PendingMesh {
    fn quads(&self) -> &[PackedQuad] {
        &self.mesh.quads
    }

    /// Arena entries: the quads, then the light palette.
    fn len(&self) -> u32 {
        arena_len(&self.mesh)
    }

    fn bytes(&self) -> u64 {
        u64::from(self.len()) * QUAD_BYTES
    }
}

/// A re-sorted translucent bucket waiting for its upload.
#[derive(Debug)]
struct PendingSorted {
    /// The chunk's page.
    page: u16,
    /// First quad of the chunk's translucent bucket in the page.
    offset: u32,
    /// The mesh the quads come from (its palette lights them on Low).
    mesh: Arc<ChunkMesh>,
    /// The bucket's quads, back to front.
    quads: Vec<PackedQuad>,
}

/// One copy into a page, a Low light page or the chunk table.
#[derive(Clone, Debug)]
enum Piece {
    /// `pending[range]`, consecutive in one page: quads and palettes.
    Meshes(Range<usize>),
    /// The same range in the Low light page: each quad's palette entry, expanded.
    LowLight(Range<usize>),
    /// Chunk table slots.
    Slots(Range<u32>),
    /// `sorted_pending[i]`: a translucent bucket in its new order.
    Sorted(usize),
    /// The light of `sorted_pending[i]` in the Low light page.
    SortedLowLight(usize),
}

/// What the tier has to write to.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UploadKinds {
    /// A GPU chunk table (High/Mid; Low has none).
    pub table: bool,
    /// Light pages parallel to the pages (Low only).
    pub low_light: bool,
}

/// Changes waiting for the next frame, plus reused scratch space.
#[derive(Debug, Default)]
pub(crate) struct Uploads {
    /// The newest mesh of each chunk id changed since the last flush. A newer mesh or a removal
    /// replaces it: its arena range may belong to another chunk by then. So every pending range
    /// is the current, live range of its chunk, and no two of them overlap.
    meshes: HashMap<u64, PendingMesh, foldhash::fast::FixedState>,
    /// The newest re-sorted translucent bucket of each chunk id (the same rules as `meshes`).
    sorted: HashMap<u64, PendingSorted, foldhash::fast::FixedState>,
    /// Scratch: the pending meshes in arena order.
    pending: Vec<PendingMesh>,
    /// Scratch: the pending sorted buckets in arena order.
    sorted_pending: Vec<PendingSorted>,
    /// Scratch: stale table runs.
    runs: Vec<Range<u32>>,
    /// Scratch: copies of the frame, with their bytes.
    pieces: Vec<(Piece, u64)>,
    /// Scratch: table entries of one run.
    entries: Vec<ChunkGpu>,
    /// Scratch: the expanded light of one mesh (Low).
    low_words: Vec<PaletteEntry>,
    /// A flush could not record all its copies since [`Uploads::take_lost`] was last called.
    lost: bool,
    /// The next staging buffer is created invalid (tests, see [`Uploads::fail_next_batch`]).
    fail_next: bool,
}

impl Uploads {
    /// Queues the quads of `mesh` for the range starting at quad `offset` of `page`, the new
    /// range of chunk `id`. A sorted bucket of the chunk's previous mesh no longer applies.
    pub fn mesh(&mut self, id: u64, page: u16, offset: u32, mesh: Arc<ChunkMesh>) {
        self.meshes.insert(id, PendingMesh { page, offset, mesh });
        self.sorted.remove(&id);
    }

    /// Queues `quads`, the translucent bucket of chunk `id`'s current `mesh` in a new order, for
    /// the bucket's range starting at quad `offset` of `page`. Written after any pending mesh.
    pub fn sorted(
        &mut self,
        id: u64,
        page: u16,
        offset: u32,
        mesh: Arc<ChunkMesh>,
        quads: Vec<PackedQuad>,
    ) {
        self.sorted.insert(
            id,
            PendingSorted {
                page,
                offset,
                mesh,
                quads,
            },
        );
    }

    /// Queues `sorted` over its chunk's translucent bucket, if the chunk is still resident in
    /// `chunks` with that bucket (a trimmed bucket, from a malformed mesh, keeps the mesher's
    /// order).
    pub fn sorted_bucket(&mut self, chunks: &ChunkTable, sorted: SortedBucket) {
        let Some((_, record)) = chunks.get(sorted.id) else {
            return;
        };
        if record.counts[TRANSLUCENT_BUCKET] as usize != sorted.quads.len() {
            return;
        }
        let start: u32 = record.counts[..TRANSLUCENT_BUCKET].iter().sum();
        self.sorted(
            sorted.id,
            record.alloc.page,
            record.alloc.alloc.offset + start,
            sorted.mesh,
            sorted.quads,
        );
    }

    /// Drops the pending mesh of a removed chunk.
    pub fn forget(&mut self, id: u64) {
        self.meshes.remove(&id);
        self.sorted.remove(&id);
    }

    /// Chunk `id` moved from `old` to `new` (shrink, ADR 0035): its pending mesh and sorted
    /// bucket go to the new range. True if a mesh was pending (it writes the whole new range, so
    /// nothing has to be copied there).
    pub fn relocate(&mut self, id: u64, old: PagedAlloc, new: PagedAlloc) -> bool {
        if let Some(sorted) = self.sorted.get_mut(&id) {
            sorted.page = new.page;
            sorted.offset = sorted.offset - old.alloc.offset + new.alloc.offset;
        }
        match self.meshes.get_mut(&id) {
            Some(mesh) => {
                mesh.page = new.page;
                mesh.offset = new.alloc.offset;
                true
            }
            None => false,
        }
    }

    /// Meshes and sorted buckets waiting for the next flush.
    pub fn pending_len(&self) -> usize {
        self.meshes.len() + self.sorted.len()
    }

    /// Drops every pending mesh (every page was reset).
    pub fn clear(&mut self) {
        self.meshes.clear();
        self.sorted.clear();
    }

    /// True once after a flush that could not record all its copies (a staging buffer could not
    /// be created). Their changes were taken from the chunk table all the same: the GPU no
    /// longer matches it until every chunk is uploaded again.
    pub fn take_lost(&mut self) -> bool {
        std::mem::take(&mut self.lost)
    }

    /// True if a flush lost copies since [`Uploads::take_lost`] was last called (kept).
    pub fn lost(&self) -> bool {
        self.lost
    }

    /// Makes the next staging buffer fail like one that cannot be created (too large or out of
    /// memory): wgpu reports the error and its copies are lost.
    pub fn fail_next_batch(&mut self) {
        self.fail_next = true;
    }

    /// Writes through `sink` what brings the GPU up to date: every pending mesh into its page
    /// (and on Low its expanded light into the light page), and every stale slot of `chunks`
    /// into the table (Low has no table: its stale slots are just forgotten). Nothing when
    /// nothing changed.
    pub fn flush<S: CopySink>(
        &mut self,
        sink: &mut S,
        chunks: &mut ChunkTable,
        kinds: UploadKinds,
    ) {
        chunks.take_stale_runs(TABLE_MERGE_GAP, &mut self.runs);
        if !kinds.table {
            self.runs.clear();
        }
        if self.meshes.is_empty() && self.sorted.is_empty() && self.runs.is_empty() {
            return;
        }
        debug_assert!(
            self.meshes.iter().all(|(&id, mesh)| {
                chunks.get(id).is_some_and(|(_, r)| {
                    (r.alloc.page, r.alloc.alloc.offset, r.alloc.alloc.size)
                        == (mesh.page, mesh.offset, mesh.len())
                        && r.light_base == mesh.offset + mesh.mesh.total_quads()
                })
            }),
            "a pending mesh is not the current range of a live chunk"
        );
        debug_assert!(
            self.sorted.iter().all(|(&id, sorted)| {
                chunks.get(id).is_some_and(|(_, r)| {
                    let start: u32 = r.counts[..TRANSLUCENT_BUCKET].iter().sum();
                    r.alloc.page == sorted.page
                        && r.alloc.alloc.offset + start == sorted.offset
                        && r.counts[TRANSLUCENT_BUCKET] as usize == sorted.quads.len()
                })
            }),
            "a pending sorted bucket is not the current translucent range of a live chunk"
        );
        self.pending
            .extend(self.meshes.drain().map(|(_, mesh)| mesh));
        self.pending
            .sort_unstable_by_key(|mesh| (mesh.page, mesh.offset));
        let limit = sink.max_batch();
        self.pieces.clear();
        mesh_pieces(&self.pending, limit, &mut self.pieces);
        if kinds.low_light {
            let low: Vec<(Piece, u64)> = self
                .pieces
                .iter()
                .filter_map(|(piece, bytes)| match piece {
                    Piece::Meshes(range) => Some((Piece::LowLight(range.clone()), *bytes)),
                    _ => None,
                })
                .collect();
            self.pieces.extend(low);
        }
        self.pieces.extend(self.runs.iter().map(|slots| {
            let bytes = u64::from(slots.end - slots.start) * ENTRY_BYTES;
            (Piece::Slots(slots.clone()), bytes)
        }));
        // After the meshes: a bucket sorted from a mesh uploaded this frame overwrites it.
        self.sorted_pending
            .extend(self.sorted.drain().map(|(_, sorted)| sorted));
        self.sorted_pending
            .sort_unstable_by_key(|sorted| (sorted.page, sorted.offset));
        for (i, sorted) in self.sorted_pending.iter().enumerate() {
            let bytes = sorted.quads.len() as u64 * QUAD_BYTES;
            self.pieces.push((Piece::Sorted(i), bytes));
            if kinds.low_light {
                self.pieces.push((Piece::SortedLowLight(i), bytes));
            }
        }

        let mut start = 0;
        while start < self.pieces.len() {
            let end = start + batch_len(&self.pieces[start..], limit);
            self.write_batch(sink, chunks, start..end);
            start = end;
        }
        self.pending.clear();
        self.sorted_pending.clear();
    }

    /// The writes of `pieces[batch]`, in one batch of `sink`.
    fn write_batch<S: CopySink>(&mut self, sink: &mut S, chunks: &ChunkTable, batch: Range<usize>) {
        let bytes: u64 = self.pieces[batch.clone()]
            .iter()
            .map(|(_, bytes)| bytes)
            .sum();
        let fail = std::mem::take(&mut self.fail_next);
        let Self {
            pieces,
            pending,
            sorted_pending,
            entries,
            low_words,
            ..
        } = self;
        let written = sink.writes(bytes, fail, &mut |put| {
            for (piece, _) in &pieces[batch.clone()] {
                match piece {
                    Piece::Meshes(range) => {
                        for mesh in &pending[range.clone()] {
                            let target = MemTarget::Page(mesh.page);
                            let at = u64::from(mesh.offset) * QUAD_BYTES;
                            let quads: &[u8] = bytemuck::cast_slice(mesh.quads());
                            put(MemOp::Write {
                                target,
                                offset: at,
                                data: quads,
                            });
                            put(MemOp::Write {
                                target,
                                offset: at + quads.len() as u64,
                                data: bytemuck::cast_slice(uploaded_palette(&mesh.mesh)),
                            });
                        }
                    }
                    Piece::LowLight(range) => {
                        for mesh in &pending[range.clone()] {
                            expand_light(&mesh.mesh, low_words);
                            put(MemOp::Write {
                                target: MemTarget::LightPage(mesh.page),
                                offset: u64::from(mesh.offset) * QUAD_BYTES,
                                data: bytemuck::cast_slice(low_words),
                            });
                        }
                    }
                    Piece::Slots(slots) => {
                        entries.clear();
                        entries.extend(slots.clone().map(|slot| chunks.gpu_entry_at(slot)));
                        put(MemOp::Write {
                            target: MemTarget::Table,
                            offset: u64::from(slots.start) * ENTRY_BYTES,
                            data: bytemuck::cast_slice(entries),
                        });
                    }
                    Piece::Sorted(i) => {
                        let sorted = &sorted_pending[*i];
                        put(MemOp::Write {
                            target: MemTarget::Page(sorted.page),
                            offset: u64::from(sorted.offset) * QUAD_BYTES,
                            data: bytemuck::cast_slice(&sorted.quads),
                        });
                    }
                    Piece::SortedLowLight(i) => {
                        let sorted = &sorted_pending[*i];
                        quad_light(&sorted.mesh, &sorted.quads, low_words);
                        put(MemOp::Write {
                            target: MemTarget::LightPage(sorted.page),
                            offset: u64::from(sorted.offset) * QUAD_BYTES,
                            data: bytemuck::cast_slice(low_words),
                        });
                    }
                }
            }
        });
        if !written {
            // The batch could not be staged (out of memory, a lost device): these copies are
            // lost, but their meshes and stale slots were already taken: `take_lost` tells the
            // caller to upload every chunk again (the render plugin resyncs).
            self.lost = true;
        }
    }
}

/// The GPU's [`CopySink`]: each batch is one `MAP_WRITE` staging buffer, and its writes are
/// copies recorded into the frame's encoder (writes that follow each other in one buffer share
/// one copy).
pub(crate) struct GpuSink<'a> {
    /// The device (staging buffers).
    pub device: &'a wgpu::Device,
    /// The frame's encoder.
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// The pages.
    pub pages: &'a PageSet,
    /// The chunk table (High/Mid).
    pub table: Option<&'a wgpu::Buffer>,
    /// Scratch: the copies of a batch (target, staging offset, target offset, bytes).
    pub copies: &'a mut Vec<(MemTarget, u64, u64, u64)>,
}

impl GpuSink<'_> {
    /// The buffer `target` names.
    fn buffer(&self, target: MemTarget) -> Option<&wgpu::Buffer> {
        match target {
            MemTarget::Page(id) => self.pages.get(id).map(|p| &p.quads),
            MemTarget::LightPage(id) => self.pages.get(id).and_then(|p| p.light.as_ref()),
            MemTarget::Table => self.table,
        }
    }
}

impl CopySink for GpuSink<'_> {
    fn max_batch(&self) -> u64 {
        self.device.limits().max_buffer_size.min(STAGING_BYTES)
    }

    fn writes(
        &mut self,
        bytes: u64,
        fail: bool,
        fill: &mut dyn FnMut(&mut dyn FnMut(MemOp<'_>)),
    ) -> bool {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel uploads"),
            // Over every `max_buffer_size` (and unaligned): `Uploads::fail_next_batch`.
            size: if fail { u64::MAX } else { bytes },
            usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        let copies = &mut *self.copies;
        copies.clear();
        {
            let mut view = match staging.slice(..).get_mapped_range_mut() {
                Ok(view) => view,
                Err(e) => {
                    // The buffer could not be created (out of memory, a lost device): wgpu
                    // reported why, and the device stays usable.
                    tracing::error!("chunk uploads lost: {e}");
                    return false;
                }
            };
            let mut at = 0u64;
            fill(&mut |op| match op {
                MemOp::Write {
                    target,
                    offset,
                    data,
                } => {
                    let len = data.len() as u64;
                    view.slice(at as usize..at as usize + data.len())
                        .copy_from_slice(data);
                    match copies.last_mut() {
                        Some((t, src, dst, size))
                            if *t == target && *src + *size == at && *dst + *size == offset =>
                        {
                            *size += len;
                        }
                        _ => copies.push((target, at, offset, len)),
                    }
                    at += len;
                }
            });
        }
        staging.unmap();
        for &(target, src, dst, size) in self.copies.iter() {
            if size > 0
                && let Some(buffer) = self.buffer(target)
            {
                let buffer = buffer.clone();
                self.encoder
                    .copy_buffer_to_buffer(&staging, src, &buffer, dst, size);
            }
        }
        true
    }

    fn copy(
        &mut self,
        from: MemTarget,
        from_offset: u64,
        to: MemTarget,
        to_offset: u64,
        bytes: u64,
    ) {
        if bytes == 0 {
            return;
        }
        if let (Some(src), Some(dst)) = (self.buffer(from), self.buffer(to)) {
            let (src, dst) = (src.clone(), dst.clone());
            self.encoder
                .copy_buffer_to_buffer(&src, from_offset, &dst, to_offset, bytes);
        }
    }

    fn clear(&mut self, target: MemTarget, offset: u64, bytes: u64) {
        if bytes == 0 {
            return;
        }
        if let Some(buffer) = self.buffer(target) {
            let buffer = buffer.clone();
            self.encoder.clear_buffer(&buffer, offset, Some(bytes));
        }
    }
}

/// Sets `out` to what the Low light page holds over `mesh`'s range: the palette entry of each
/// quad (open sky for an index outside the palette), then the palette itself (never drawn; it
/// only keeps the range the same size as in the page).
pub(crate) fn expand_light(mesh: &ChunkMesh, out: &mut Vec<PaletteEntry>) {
    quad_light(mesh, &mesh.quads, out);
    out.extend_from_slice(uploaded_palette(mesh));
}

/// Sets `out` to the palette entry of each of `quads` (from `mesh`), open sky for an index
/// outside the palette.
pub(crate) fn quad_light(mesh: &ChunkMesh, quads: &[PackedQuad], out: &mut Vec<PaletteEntry>) {
    let palette = uploaded_palette(mesh);
    out.clear();
    out.extend(quads.iter().map(|q| {
        palette
            .get(usize::from(q.light_index()))
            .copied()
            .unwrap_or(FULL_SKY_ENTRY)
    }));
}

/// Appends the copies of `pending` (sorted by page, then offset) to `pieces`: one per run of
/// meshes that follow each other in one page, cut where it would pass `limit` bytes. Never
/// across a gap (it may hold another chunk's quads) nor across pages.
fn mesh_pieces(pending: &[PendingMesh], limit: u64, pieces: &mut Vec<(Piece, u64)>) {
    let mut first = 0;
    let mut bytes = 0;
    for (i, mesh) in pending.iter().enumerate() {
        bytes += mesh.bytes();
        let joined = pending.get(i + 1).is_some_and(|next| {
            mesh.page == next.page
                && mesh.offset + mesh.len() == next.offset
                && bytes + next.bytes() <= limit
        });
        if !joined {
            pieces.push((Piece::Meshes(first..i + 1), bytes));
            first = i + 1;
            bytes = 0;
        }
    }
}

/// How many of `pieces`, from the first, go in one staging buffer of at most `limit` bytes; at
/// least one, even when it alone is larger.
fn batch_len(pieces: &[(Piece, u64)], limit: u64) -> usize {
    let mut bytes = 0;
    pieces
        .iter()
        .position(|(_, size)| {
            bytes += size;
            bytes > limit
        })
        .map_or(pieces.len(), |over| over.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pending mesh of `len` page entries in page 0: `len - 1` quads and one palette entry.
    fn pending(offset: u32, len: usize) -> PendingMesh {
        in_page(0, offset, len)
    }

    fn in_page(page: u16, offset: u32, len: usize) -> PendingMesh {
        PendingMesh {
            page,
            offset,
            mesh: Arc::new(ChunkMesh::from_quads(
                vec![PackedQuad::default(); len - 1],
                [0; boxel_mesh::BUCKET_COUNT],
            )),
        }
    }

    fn mesh_ranges(meshes: &[PendingMesh], limit: u64) -> Vec<(Range<usize>, u64)> {
        let mut pieces = Vec::new();
        mesh_pieces(meshes, limit, &mut pieces);
        pieces
            .into_iter()
            .map(|(piece, bytes)| match piece {
                Piece::Meshes(range) => (range, bytes / QUAD_BYTES),
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn meshes_that_follow_each_other_share_one_copy() {
        let meshes = [
            pending(0, 3),
            pending(3, 2),
            pending(6, 1),
            pending(7, 4),
            pending(20, 1),
        ];
        // slot 5 is someone else's: no copy over it
        assert_eq!(
            mesh_ranges(&meshes, u64::MAX),
            [(0..2, 5), (2..4, 5), (4..5, 1)]
        );
        assert!(mesh_ranges(&[], u64::MAX).is_empty());
    }

    #[test]
    fn runs_of_meshes_never_join_across_pages() {
        // Page 0 ends at 5 and page 1 starts at 5: the same offsets, other buffers.
        let meshes = [
            in_page(0, 0, 3),
            in_page(0, 3, 2),
            in_page(1, 5, 2),
            in_page(1, 7, 1),
            in_page(3, 8, 1),
        ];
        assert_eq!(
            mesh_ranges(&meshes, u64::MAX),
            [(0..2, 5), (2..4, 3), (4..5, 1)]
        );
    }

    #[test]
    fn runs_of_meshes_are_cut_at_the_staging_limit() {
        // A resync: every mesh right after the previous one.
        let meshes: Vec<_> = (0..6).map(|i| pending(4 * i, 4)).collect();
        let limit = 10 * QUAD_BYTES;
        let ranges = mesh_ranges(&meshes, limit);
        assert_eq!(ranges, [(0..2, 8), (2..4, 8), (4..6, 8)]);
        assert!(ranges.iter().all(|(_, quads)| quads * QUAD_BYTES <= limit));
        // A mesh larger than the limit still goes alone.
        let big = [pending(0, 2), pending(2, 20), pending(22, 2)];
        assert_eq!(mesh_ranges(&big, limit), [(0..1, 2), (1..2, 20), (2..3, 2)]);
    }

    #[test]
    fn meshes_take_their_quads_and_their_palette() {
        let quads = vec![
            PackedQuad::default().with_light_index(1),
            PackedQuad::default(),
            PackedQuad::default().with_light_index(7),
        ];
        let lit = [3, 5];
        let mesh = ChunkMesh {
            quads: quads.clone(),
            palette: vec![FULL_SKY_ENTRY, lit],
            ..ChunkMesh::default()
        };
        assert_eq!((light_entries(&mesh), arena_len(&mesh)), (2, 5));
        let mut low = Vec::new();
        expand_light(&mesh, &mut low);
        // per quad, then the palette; an index past the palette reads open sky
        assert_eq!(
            low,
            [lit, FULL_SKY_ENTRY, FULL_SKY_ENTRY, FULL_SKY_ENTRY, lit]
        );
        // A hand-made mesh without a palette still gets one open-sky entry.
        let bare = ChunkMesh {
            quads,
            ..ChunkMesh::default()
        };
        assert_eq!((light_entries(&bare), arena_len(&bare)), (1, 4));
        expand_light(&bare, &mut low);
        assert_eq!(low, [FULL_SKY_ENTRY; 4]);
    }

    #[test]
    fn batches_stay_under_the_limit_unless_one_piece_is_larger() {
        let pieces: Vec<_> = [10, 20, 30, 100, 5, 45]
            .into_iter()
            .map(|bytes| (Piece::Slots(0..1), bytes))
            .collect();
        let mut batches = Vec::new();
        let mut start = 0;
        while start < pieces.len() {
            let end = start + batch_len(&pieces[start..], 50);
            batches.push(start..end);
            start = end;
        }
        assert_eq!(batches, [0..2, 2..3, 3..4, 4..6]);
        assert_eq!(batch_len(&pieces, 1000), pieces.len());
    }
}
