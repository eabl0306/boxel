//! Persistence boundary (trait only in M0; redb-backed store in M1).
use boxel_voxel::ChunkKey;

/// Chunk blob store. Implementations must be callable from an IO worker thread; batching is the
/// caller's job (redb allows one write transaction at a time).
pub trait ChunkStore: Send + Sync + 'static {
    /// Loads one chunk blob.
    fn load(&self, key: ChunkKey) -> std::io::Result<Option<Vec<u8>>>;
    /// Saves blobs atomically as one batch.
    fn save_batch(&self, items: &[(ChunkKey, Vec<u8>)]) -> std::io::Result<()>;
    /// Flushes pending writes.
    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Stores nothing (M0 default: worlds are regenerated from the seed).
#[derive(Clone, Copy, Debug, Default)]
pub struct NullChunkStore;

impl ChunkStore for NullChunkStore {
    fn load(&self, _key: ChunkKey) -> std::io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn save_batch(&self, _items: &[(ChunkKey, Vec<u8>)]) -> std::io::Result<()> {
        Ok(())
    }
}
