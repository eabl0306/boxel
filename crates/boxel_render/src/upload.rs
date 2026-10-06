//! Per-frame uploads. Chunk meshes and chunk table entries changed since the last frame reach the
//! GPU together at the start of the next frame: one staging buffer, and one copy per contiguous
//! range recorded into the frame's encoder before any pass reads them. `Queue::write_buffer`
//! creates (and later frees) a staging buffer per call, a few microseconds each, and crossing a
//! chunk border changes a few hundred chunks.
use crate::arena::QUAD_BYTES;
use crate::chunk_table::ChunkTable;
use boxel_mesh::PackedQuad;
use boxel_shaders::gpu::ChunkGpu;
use std::collections::HashMap;
use std::ops::Range;

/// Bytes of one chunk table entry.
const ENTRY_BYTES: u64 = size_of::<ChunkGpu>() as u64;

/// Table runs at most this many up-to-date slots apart are sent as one: 16 entries more (1 KiB)
/// cost about as much as recording another copy.
const TABLE_MERGE_GAP: u32 = 16;

/// Largest staging buffer: a bigger frame (a full resync of a large world) is split into several.
/// A single range larger than this still goes in one buffer of its own size.
const STAGING_BYTES: u64 = 64 << 20;

/// A mesh waiting for its upload.
#[derive(Debug)]
struct PendingMesh {
    /// First arena quad (the chunk's current range).
    offset: u32,
    quads: Vec<PackedQuad>,
}

/// One copy into the arena or the chunk table.
#[derive(Clone, Debug)]
enum Piece {
    /// `pending[range]`, consecutive in the arena.
    Meshes(Range<usize>),
    /// Chunk table slots.
    Slots(Range<u32>),
}

/// The buffers the copies go to.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UploadTargets<'a> {
    /// The quad arena.
    pub arena: &'a wgpu::Buffer,
    /// The chunk table (High/Mid; Low has none).
    pub table: Option<&'a wgpu::Buffer>,
}

/// Changes waiting for the next frame, plus reused scratch space.
#[derive(Debug, Default)]
pub(crate) struct Uploads {
    /// The newest mesh of each chunk id changed since the last flush. A newer mesh or a removal
    /// replaces it: its arena range may belong to another chunk by then. So every pending range
    /// is the current, live range of its chunk, and no two of them overlap.
    meshes: HashMap<u64, PendingMesh, foldhash::fast::FixedState>,
    /// Scratch: the pending meshes in arena order.
    pending: Vec<PendingMesh>,
    /// Scratch: stale table runs.
    runs: Vec<Range<u32>>,
    /// Scratch: copies of the frame, with their bytes.
    pieces: Vec<(Piece, u64)>,
    /// Scratch: table entries of one run.
    entries: Vec<ChunkGpu>,
}

impl Uploads {
    /// Queues `quads` for the arena range starting at quad `offset`, the new range of chunk `id`.
    pub fn mesh(&mut self, id: u64, offset: u32, quads: &[PackedQuad]) {
        let pending = self.meshes.entry(id).or_insert_with(|| PendingMesh {
            offset,
            quads: Vec::new(),
        });
        pending.offset = offset;
        pending.quads.clear();
        pending.quads.extend_from_slice(quads);
    }

    /// Drops the pending mesh of a removed chunk.
    pub fn forget(&mut self, id: u64) {
        self.meshes.remove(&id);
    }

    /// Drops every pending mesh (the arena was reset).
    pub fn clear(&mut self) {
        self.meshes.clear();
    }

    /// Records into `encoder` the copies that bring the GPU up to date: every pending mesh into
    /// the arena, and every stale slot of `chunks` into the table (Low has no table: its stale
    /// slots are just forgotten). Nothing when nothing changed.
    pub fn flush(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        targets: UploadTargets<'_>,
        chunks: &mut ChunkTable,
    ) {
        chunks.take_stale_runs(TABLE_MERGE_GAP, &mut self.runs);
        if targets.table.is_none() {
            self.runs.clear();
        }
        if self.meshes.is_empty() && self.runs.is_empty() {
            return;
        }
        debug_assert!(
            self.meshes.iter().all(|(&id, mesh)| {
                chunks.get(id).is_some_and(|(_, r)| {
                    (r.alloc.offset, r.alloc.size as usize) == (mesh.offset, mesh.quads.len())
                })
            }),
            "a pending mesh is not the current range of a live chunk"
        );
        self.pending
            .extend(self.meshes.drain().map(|(_, mesh)| mesh));
        self.pending.sort_unstable_by_key(|mesh| mesh.offset);
        self.pieces.clear();
        mesh_pieces(&self.pending, &mut self.pieces);
        self.pieces.extend(self.runs.iter().map(|slots| {
            let bytes = u64::from(slots.end - slots.start) * ENTRY_BYTES;
            (Piece::Slots(slots.clone()), bytes)
        }));

        let limit = device.limits().max_buffer_size.min(STAGING_BYTES);
        let mut start = 0;
        while start < self.pieces.len() {
            let end = start + batch_len(&self.pieces[start..], limit);
            self.copy_batch(device, encoder, targets, chunks, start..end);
            start = end;
        }
        self.pending.clear();
    }

    /// One staging buffer with `pieces[batch]`, and their copies.
    fn copy_batch(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        targets: UploadTargets<'_>,
        chunks: &ChunkTable,
        batch: Range<usize>,
    ) {
        let bytes: u64 = self.pieces[batch.clone()]
            .iter()
            .map(|(_, bytes)| bytes)
            .sum();
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("boxel uploads"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        {
            let mut view = match staging.slice(..).get_mapped_range_mut() {
                Ok(view) => view,
                Err(e) => {
                    // Only with a lost device: the buffer was just created mapped. The render
                    // plugin then builds a new renderer and uploads every chunk again.
                    tracing::error!("chunk uploads skipped: {e}");
                    return;
                }
            };
            let mut at = 0;
            let mut put = |data: &[u8]| {
                view.slice(at..at + data.len()).copy_from_slice(data);
                at += data.len();
            };
            for (piece, _) in &self.pieces[batch.clone()] {
                match piece {
                    Piece::Meshes(range) => {
                        for mesh in &self.pending[range.clone()] {
                            put(bytemuck::cast_slice(&mesh.quads));
                        }
                    }
                    Piece::Slots(slots) => {
                        self.entries.clear();
                        self.entries
                            .extend(slots.clone().map(|slot| chunks.gpu_entry_at(slot)));
                        put(bytemuck::cast_slice(&self.entries));
                    }
                }
            }
        }
        staging.unmap();
        let mut src = 0;
        for (piece, size) in &self.pieces[batch] {
            let (dst, offset) = match piece {
                Piece::Meshes(range) => (
                    Some(targets.arena),
                    u64::from(self.pending[range.start].offset) * QUAD_BYTES,
                ),
                Piece::Slots(slots) => (targets.table, u64::from(slots.start) * ENTRY_BYTES),
            };
            if let Some(dst) = dst {
                encoder.copy_buffer_to_buffer(&staging, src, dst, offset, *size);
            }
            src += size;
        }
    }
}

/// Appends the copies of `pending` (sorted by offset) to `pieces`: one per run of meshes that
/// follow each other in the arena. Never across a gap: it may hold another chunk's quads.
fn mesh_pieces(pending: &[PendingMesh], pieces: &mut Vec<(Piece, u64)>) {
    let mut first = 0;
    let mut bytes = 0;
    for (i, mesh) in pending.iter().enumerate() {
        bytes += mesh.quads.len() as u64 * QUAD_BYTES;
        let joined = pending
            .get(i + 1)
            .is_some_and(|next| mesh.offset + mesh.quads.len() as u32 == next.offset);
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

    fn pending(offset: u32, quads: usize) -> PendingMesh {
        PendingMesh {
            offset,
            quads: vec![PackedQuad::default(); quads],
        }
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
        let mut pieces = Vec::new();
        mesh_pieces(&meshes, &mut pieces);
        let pieces: Vec<_> = pieces
            .into_iter()
            .map(|(piece, bytes)| match piece {
                Piece::Meshes(range) => (range, bytes / QUAD_BYTES),
                Piece::Slots(_) => unreachable!(),
            })
            .collect();
        // slot 5 is someone else's: no copy over it
        assert_eq!(pieces, [(0..2, 5), (2..4, 5), (4..5, 1)]);
        let mut none = Vec::new();
        mesh_pieces(&[], &mut none);
        assert!(none.is_empty());
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
