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
//!   the world clock. Chunks nobody edited are never saved: they come from the seed again. The
//!   exit closes the store after its save ([`WorldShutdownSystems`], [`WorldStore::close`]); so
//!   does closing the world while the app runs ([`crate::lifecycle`]).
//! - [`Saved`] entities: what the registered [`EntityCodec`]s write of them (physics saves its
//!   bodies) is saved with the chunk they are in, when it unloads (they are despawned then),
//!   with the autosave and on exit, and they come back when the chunk loads.
//!
//! ```no_run
//! use boxel_app::App;
//! use boxel_voxel::BlockRegistry;
//! use boxel_world::WorldPlugin;
//! use boxel_world::persist::{PersistPlugin, WorldStore};
//! use boxel_demo::worldgen::TerrainGenerator;
//! use boxel_worldgen::WorldGenerator;
//! use std::sync::Arc;
//!
//! let registry = Arc::new(boxel_demo::blocks::registry());
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
//! // Or open worlds while the app runs: `WorldPlugin::closed`, `PersistPlugin::without_store`
//! // and a `lifecycle::WorldSpec` with the store.
//! ```
mod backend;
mod entities;
pub mod format;
mod plugin;
mod saver;
mod store;
#[cfg(test)]
mod tests;

pub use backend::{
    ChunkStore, MemoryStore, NullChunkStore, REDB_CACHE_BYTES, RedbStore, SaveBatch,
};
#[cfg(test)]
pub(crate) use entities::SavedLog;
pub(crate) use entities::put_away_all;
pub use entities::{EntityCodec, EntityCodecs, Saved, SavedId, SavedIds, add_entity_codec};
pub use format::{EntityPart, FORMAT_VERSION, FormatError, IdMap};
pub(crate) use plugin::{PersistExecutor, close_store_on_exit};
pub use plugin::{PersistPlugin, PersistSettings};
#[cfg(test)]
pub(crate) use saver::saved_index;
pub(crate) use saver::{Persist, SavedIndex};
pub use store::{PersistError, PersistStats, WORLD_FILE, WorldStore};
pub(crate) use store::{WeakStore, openings};

/// The `Shutdown` sets, now the lifecycle's (kept here for apps that named them through
/// `persist`).
pub use crate::lifecycle::WorldShutdownSystems;
