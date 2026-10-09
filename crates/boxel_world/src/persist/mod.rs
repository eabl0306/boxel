//! Saving and loading the world (ADR 0024).
//!
//! - [`ChunkStore`]: where the bytes live, by key ([`RedbStore`] on disk, [`MemoryStore`],
//!   [`NullChunkStore`]). Keys keep all 64 bits of every axis (ADR 0014).
//! - [`format`](mod@format): the bytes. Blocks are saved by the stable name of their state
//!   through the world's [`IdMap`] (ADR 0019), in versioned, checksummed records.
//! - [`WorldStore`]: an open world (its store, id map and the chunks waiting to be written),
//!   shared by the main thread and the jobs. [`WorldStore::open_dir`] opens a directory.
//! - [`PersistPlugin`]: saves the chunks with edits as they unload, with the autosave and on
//!   exit, loads saved chunks instead of generating them, and keeps the pending block ticks and
//!   the world clock. Chunks nobody edited are never saved: they come from the seed again.
//!
//! ```no_run
//! use boxel_app::App;
//! use boxel_voxel::BlockRegistry;
//! use boxel_world::WorldPlugin;
//! use boxel_world::persist::{PersistPlugin, WorldStore};
//! use boxel_worldgen::{TerrainGenerator, WorldGenerator};
//! use std::sync::Arc;
//!
//! let registry = Arc::new(BlockRegistry::default_blocks());
//! let generator = Arc::new(TerrainGenerator::default());
//! let store = WorldStore::open_dir("saves/world1", &registry, generator.name())
//!     .expect("the world opens");
//! let mut app = App::new();
//! app.add_plugins(WorldPlugin {
//!     generator,
//!     registry,
//!     ..WorldPlugin::default()
//! })
//! .add_plugins(PersistPlugin::new(store));
//! ```
mod backend;
pub mod format;
mod plugin;
mod saver;
mod store;
#[cfg(test)]
mod tests;

pub use backend::{
    ChunkStore, MemoryStore, NullChunkStore, REDB_CACHE_BYTES, RedbStore, SaveBatch,
};
pub use format::{FORMAT_VERSION, FormatError, IdMap};
pub use plugin::{PersistPlugin, PersistSettings};
pub(crate) use saver::Persist;
pub use store::{PersistError, PersistStats, WORLD_FILE, WorldStore};
