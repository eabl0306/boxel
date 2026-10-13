//! Opening and closing worlds while the app runs (ADR 0034, issue #58).
//!
//! A world is data, not plugins: a [`WorldSpec`] (generator, settings, store, clock). The
//! plugins, the block registry, the codecs, the executors and the GPU are the app's and stay for
//! the life of the process; one world is open at a time. Every engine resource stays inserted
//! for the whole app (only the [`WorldStore`] resource comes and goes, since it is the world):
//! each plugin puts its own per-world state back as a fresh process has it, in three schedules
//! the lifecycle runs:
//!
//! - [`WorldSave`] ([`SaveSystems`]): the world's last moment, written with the save of the
//!   close (and of the exit).
//! - [`WorldTeardown`] ([`TeardownSystems`]): every per-world resource back to empty, the saved
//!   and [`WorldScoped`] entities despawned, the big containers moved to [`WorldGarbage`] (they
//!   are dropped on the IO pool).
//! - [`WorldOpen`] ([`OpenSystems`]): the restored ticks and clock, the game's player and viewer.
//!
//! Closing is asynchronous: the frame of the close runs the save and the teardown, then the world
//! stays [`WorldStatus::Closing`] for a few frames while the final save job, the store's close
//! and the garbage drop run off the main thread. Then [`WorldEvent::Closed`]. An open asked for
//! meanwhile waits for it. A fresh process opens its first world through the same path, before
//! `Startup` ([`WorldPlugin::new`](crate::WorldPlugin::new)), so a world opened after another one
//! starts as it would in a new process.
//!
//! Requests go through [`WorldCommandsExt`] (applied in the next `PreUpdate`) or, from an
//! exclusive system ordered before [`WorldLifecycleSystems`], through [`open_world`] and
//! [`close_world`] directly.
//!
//! A per-world resource of a plugin or a game registers with [`WorldScopeApp::world_resource`]
//! (its teardown system lives in a [`TeardownSystems`] set) and a per-world message with
//! [`WorldScopeApp::world_message`] (the lifecycle clears it).
mod driver;
mod scope;
#[cfg(test)]
mod tests;

pub(crate) use driver::{
    PendingOpen, default_close_timeout, despawn_world_entities, drive_world_lifecycle,
    lifecycle_busy, lifecycle_startup, open_engine_state, save_world_on_exit,
};
pub use driver::{close_world, finish_closing, open_world};
pub use scope::{WorldScope, WorldScopeApp};

use crate::WorldSettings;
use crate::persist::WorldStore;
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::ScheduleLabel;
use boxel_voxel::BlockPos;
use boxel_worldgen::{FlatGenerator, WorldGenerator};
use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};
use std::time::Duration;

/// What a world is made of. The block registry is the app's ([`WorldContent`](crate::WorldContent)),
/// never one world's.
#[derive(Clone)]
pub struct WorldSpec {
    /// Its generator.
    pub generator: Arc<dyn WorldGenerator>,
    /// The whole [`WorldSettings`] resource while it is open, knobs included: a game that changes
    /// knobs live builds it from the live resource
    /// (`WorldSettings { dimension, vertical_bounds, ..live.clone() }`).
    pub settings: WorldSettings,
    /// Where it is saved (opened by the game off the main thread, [`WorldStore::open_dir`]); `None`
    /// for a world that is not saved. A saved world needs
    /// [`PersistPlugin`](crate::persist::PersistPlugin) in the app.
    pub store: Option<WorldStore>,
    /// `TimeOfDay::tick` when the store holds no clock (or there is no store). `None`: the tick
    /// the app started with.
    pub clock: Option<u64>,
}

impl WorldSpec {
    /// An unsaved world of `generator` with `settings`, at the clock the app started with.
    pub fn new(generator: Arc<dyn WorldGenerator>, settings: WorldSettings) -> Self {
        Self {
            generator,
            settings,
            store: None,
            clock: None,
        }
    }

    /// The same world, saved in `store`.
    pub fn with_store(self, store: WorldStore) -> Self {
        Self {
            store: Some(store),
            ..self
        }
    }

    /// The same world, starting at `TimeOfDay::tick` `clock` unless its store has a clock.
    pub fn with_clock(self, clock: u64) -> Self {
        Self {
            clock: Some(clock),
            ..self
        }
    }
}

impl fmt::Debug for WorldSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorldSpec")
            .field("generator", &self.generator.name())
            .field("settings", &self.settings)
            .field("store", &self.store)
            .field("clock", &self.clock)
            .finish()
    }
}

/// Where the world of the app stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WorldStatus {
    /// No world: every world system does nothing.
    #[default]
    Closed,
    /// A world is open.
    Open,
    /// The world was torn down; its last save, the store's close and the drop of its memory
    /// run off the main thread. An open waits for [`WorldStatus::Closed`].
    Closing,
}

/// How a world closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseOptions {
    /// Writes the world's last moment (the default). `false` only to delete the world
    /// afterwards: what waits to be saved is dropped (the batches already written stay), a save
    /// job running at the close ends on its own and what it fails to write is dropped too.
    pub save: bool,
}

impl Default for CloseOptions {
    fn default() -> Self {
        Self { save: true }
    }
}

/// Which world the app is on: +1 at every open and every close. Always present with
/// [`WorldPlugin`](crate::WorldPlugin). A cache keyed by `ChunkId` or anything else a world
/// hands out is dropped when it changes.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorldEpoch(u64);

impl WorldEpoch {
    /// The count of opens and closes so far.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// What happened to the app's world, written by the lifecycle in `PreUpdate`.
#[derive(Message, Clone, Debug)]
pub enum WorldEvent {
    /// A world opened.
    Opened {
        /// Its epoch.
        epoch: WorldEpoch,
    },
    /// The world finished closing: saved (or given up), its store closed and its memory freed.
    /// The same store folder can be opened again from now on.
    Closed {
        /// The epoch of the close.
        epoch: WorldEpoch,
        /// How it went.
        report: CloseReport,
    },
    /// A queued open was refused (the spec's store, if any, was closed).
    OpenFailed {
        /// Why.
        error: OpenWorldError,
    },
}

/// How a close went ([`WorldEvent::Closed`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CloseReport {
    /// The world had a store and its last moment was written (never for a close without its
    /// save, [`CloseOptions::save`]).
    pub saved: bool,
    /// The last save kept failing for `PersistSettings::close_timeout` (or until the exit):
    /// what was not written yet is lost (the batches written before stay). A slow save that
    /// does not fail is waited for.
    pub gave_up: bool,
    /// Chunk records written from the close to the end.
    pub chunks_written: usize,
    /// Entity records written from the close to the end.
    pub entities_written: usize,
    /// The errors of the last save (none for a close without its save) and of the store's
    /// close.
    pub errors: Vec<String>,
    /// Main-thread time of the close frame (save snapshot and teardown).
    pub teardown: Duration,
    /// From the close to [`WorldEvent::Closed`].
    pub took: Duration,
    /// Handles of the closed store still alive at the end (diagnostics: 0 unless something
    /// kept a clone).
    pub store_handles: usize,
}

/// How an open went ([`WorldLifecycle::last_open`]): the main-thread time of [`open_world`] and
/// its two parts that grow with the saved world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenReport {
    /// The whole open on the main thread ([`WorldOpen`] included).
    pub took: Duration,
    /// Of it, the save state built from the store: the index of its saved chunks and entities.
    pub saved_index: Duration,
    /// Of it, the far LOD's saved chunks seeded as edits (zero without the far LOD).
    pub lod_seed: Duration,
    /// Chunks saved in the store when it opened.
    pub saved_chunks: usize,
}

/// Why a world did not open ([`open_world`], [`WorldEvent::OpenFailed`]). The app's world is
/// left as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenWorldError {
    /// A world is open or closing.
    NotClosed,
    /// The store was opened with another block registry than the app's.
    OtherRegistry {
        /// Block states of the store's registry.
        store: usize,
        /// Block states of the app's registry.
        registry: usize,
    },
    /// The store (or a clone of it) already served a world: open it again
    /// ([`WorldStore::open`]).
    StoreServed,
    /// The store is closed.
    StoreClosed,
    /// The store was opened before the previous world on the same backend finished closing: it
    /// would not see that world's last save. Open it again.
    StoreOpenedEarly,
    /// A saved world needs [`PersistPlugin`](crate::persist::PersistPlugin) in the app.
    NoPersistPlugin,
    /// Called from an `Idle` step: worlds open and close in `PreUpdate`.
    InIdle,
}

impl fmt::Display for OpenWorldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotClosed => write!(f, "a world is still open or closing"),
            Self::OtherRegistry { store, registry } => write!(
                f,
                "the world store was opened with another registry ({store} block states, the \
                 app has {registry})"
            ),
            Self::StoreServed => write!(f, "the world store already served a world"),
            Self::StoreClosed => write!(f, "the world store is closed"),
            Self::StoreOpenedEarly => write!(
                f,
                "the world store was opened before the previous world on it finished closing"
            ),
            Self::NoPersistPlugin => write!(f, "a saved world needs PersistPlugin"),
            Self::InIdle => write!(f, "worlds do not open or close in an idle step"),
        }
    }
}

impl std::error::Error for OpenWorldError {}

/// Marker: despawned when its world closes (after the saved entities are put away). Give it to
/// every entity of the world that is not [`Saved`](crate::persist::Saved): a viewer, a camera
/// rig, a projectile. A [`ChunkViewer`](crate::ChunkViewer) without it streams in the next world
/// too.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct WorldScoped;

/// The schedule of a world's last moment: [`SaveSystems`], chained. Runs when a world closes
/// with [`CloseOptions::save`] and on exit, while the world is whole.
#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct WorldSave;

/// The schedule that puts every per-world resource back as a fresh process has it:
/// [`TeardownSystems`], chained. Runs once per close, right after [`WorldSave`].
#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct WorldTeardown;

/// The schedule that sets a world up once its content and store are in: [`OpenSystems`],
/// chained.
#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct WorldOpen;

/// [`WorldSave`]'s sets.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SaveSystems {
    /// The game stages its data ([`WorldLifecycle::stage_meta`], the player). The store is
    /// still a resource here; a game write must not outlive this set (after it the store is
    /// closed).
    Game,
    /// The engine: the saved entities, the edited chunks, the ticks, the clock and the staged
    /// game data, as one moment.
    World,
}

/// [`WorldTeardown`]'s sets, in order.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TeardownSystems {
    /// The saved entities (put away), then every `Saved` and [`WorldScoped`] entity. The game
    /// despawns its own world entities here.
    Despawn,
    /// Physics' indexes and contacts.
    Physics,
    /// The block ticks.
    Ticks,
    /// The save state and the store.
    Persist,
    /// The chunk map, the sky tops and the jobs.
    Streaming,
    /// The far LOD.
    Lod,
    /// The renderer forgets the world's chunks and nodes.
    Render,
    /// The game's caches.
    Game,
}

/// [`WorldOpen`]'s sets, in order.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpenSystems {
    /// The restored ticks and clock, the far LOD's store and saved chunks.
    Engine,
    /// The game's player, viewer and camera: the first streaming frame starts where they are.
    Game,
}

/// The lifecycle's systems: the driver in `PreUpdate` (a game's exclusive screen hook that opens
/// or closes worlds directly goes before it) and the build-time world's open in `PreStartup`.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorldLifecycleSystems;

/// The `Shutdown` sets of the world, chained: the exit save, then the store's close.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorldShutdownSystems {
    /// While a world is open: [`WorldSave`], then everything staged is written on this thread.
    /// While one is closing: its close is finished on this thread.
    Save,
    /// Closes the [`WorldStore`] ([`WorldStore::close`]): the file is closed cleanly and
    /// released before the app returns, whoever still holds a clone. An app's own `Shutdown`
    /// system that writes the store goes `.before(Close)`: after it, the write fails with the
    /// closed error.
    Close,
}

/// A request waiting for the driver.
pub(crate) enum Request {
    Open(Box<WorldSpec>),
    Close(CloseOptions),
    Save,
}

/// The app's world: its status, the requests waiting, the game data staged for the next save
/// and the close in progress. Always present with [`WorldPlugin`](crate::WorldPlugin).
#[derive(Resource)]
pub struct WorldLifecycle {
    pub(crate) status: WorldStatus,
    pub(crate) requests: VecDeque<Request>,
    pub(crate) closing: Option<driver::ClosingWorld>,
    /// Game data for the next save moment, newest value per name.
    pub(crate) staged_meta: Vec<(String, Vec<u8>)>,
    /// Backends of closed worlds and the store openings counted when they closed (dead ones
    /// are pruned at each close): a store opened on one of them before that is refused.
    pub(crate) closed_backends: Vec<(Weak<dyn crate::persist::ChunkStore>, u64)>,
    /// The report of the last close (tests and tools).
    pub(crate) last_report: Option<CloseReport>,
    /// The report of the last open (tests and tools).
    pub(crate) last_open: Option<OpenReport>,
}

impl Default for WorldLifecycle {
    fn default() -> Self {
        Self {
            status: WorldStatus::Closed,
            requests: VecDeque::new(),
            closing: None,
            staged_meta: Vec::new(),
            closed_backends: Vec::new(),
            last_report: None,
            last_open: None,
        }
    }
}

impl WorldLifecycle {
    /// Where the world stands.
    pub fn status(&self) -> WorldStatus {
        self.status
    }

    /// True while a world is open.
    pub fn is_open(&self) -> bool {
        self.status == WorldStatus::Open
    }

    /// Opens `spec` in the next `PreUpdate` once no world is open (after a close in progress
    /// ends). Refused with [`WorldEvent::OpenFailed`] while a world is open: a switch is a close,
    /// then an open, both queued in order.
    pub fn request_open(&mut self, spec: WorldSpec) {
        self.requests.push_back(Request::Open(Box::new(spec)));
    }

    /// Closes the open world in the next `PreUpdate` (nothing while none is open).
    pub fn request_close(&mut self, options: CloseOptions) {
        self.requests.push_back(Request::Close(options));
    }

    /// Saves the open world soon, as if the autosave were due (nothing without a store).
    pub fn request_save(&mut self) {
        self.requests.push_back(Request::Save);
    }

    /// Joins `value` to the next save moment of the open world (the autosave, the close, the
    /// exit, [`WorldLifecycle::request_save`]), in the same transaction as its chunks, ticks,
    /// clock and entities. The newest value per name wins. Read it back with
    /// [`WorldStore::meta`]. Names starting with `boxel:` belong to the engine and are ignored.
    pub fn stage_meta(&mut self, name: impl Into<String>, value: Vec<u8>) {
        let name = name.into();
        if name.starts_with("boxel:") {
            tracing::error!(
                name,
                "metadata names starting with \"boxel:\" belong to the engine"
            );
            return;
        }
        self.staged_meta.retain(|(n, _)| *n != name);
        self.staged_meta.push((name, value));
    }

    /// The report of the last close that ended.
    pub fn last_report(&self) -> Option<&CloseReport> {
        self.last_report.as_ref()
    }

    /// The report of the last open.
    pub fn last_open(&self) -> Option<OpenReport> {
        self.last_open
    }
}

/// Run condition: a world is open.
pub fn world_open(lifecycle: Option<Res<WorldLifecycle>>) -> bool {
    lifecycle.is_some_and(|l| l.status == WorldStatus::Open)
}

/// Requests through `Commands`, applied in order in the next `PreUpdate`.
pub trait WorldCommandsExt {
    /// [`WorldLifecycle::request_open`].
    fn open_world(&mut self, spec: WorldSpec);
    /// [`WorldLifecycle::request_close`].
    fn close_world(&mut self, options: CloseOptions);
    /// [`WorldLifecycle::request_save`].
    fn save_world(&mut self);
}

impl WorldCommandsExt for Commands<'_, '_> {
    fn open_world(&mut self, spec: WorldSpec) {
        self.queue(move |world: &mut World| {
            if let Some(mut l) = world.get_resource_mut::<WorldLifecycle>() {
                l.request_open(spec);
            }
        });
    }

    fn close_world(&mut self, options: CloseOptions) {
        self.queue(move |world: &mut World| {
            if let Some(mut l) = world.get_resource_mut::<WorldLifecycle>() {
                l.request_close(options);
            }
        });
    }

    fn save_world(&mut self) {
        self.queue(|world: &mut World| {
            if let Some(mut l) = world.get_resource_mut::<WorldLifecycle>() {
                l.request_save();
            }
        });
    }
}

/// What a teardown leaves behind (the chunk slots, the sky tops, old job state): dropped on the
/// IO pool after the teardown, and [`WorldStatus::Closing`] waits for that drop, so two worlds
/// never sit in memory together.
#[derive(Resource, Default)]
pub struct WorldGarbage(Mutex<Vec<Box<dyn Any + Send>>>);

impl WorldGarbage {
    /// Drops `value` off the main thread after the teardown.
    pub fn push(&mut self, value: impl Send + 'static) {
        self.items().push(Box::new(value));
    }

    /// Nothing waits to be dropped.
    pub fn is_empty(&mut self) -> bool {
        self.items().is_empty()
    }

    pub(crate) fn take(&mut self) -> Vec<Box<dyn Any + Send>> {
        std::mem::take(self.items())
    }

    fn items(&mut self) -> &mut Vec<Box<dyn Any + Send>> {
        self.0.get_mut().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The generator of the closed world: empty, never freed (its address is never reused).
pub(crate) fn placeholder_generator() -> &'static Arc<dyn WorldGenerator> {
    static PLACEHOLDER: LazyLock<Arc<dyn WorldGenerator>> =
        LazyLock::new(|| Arc::new(FlatGenerator::new(Vec::new())));
    &PLACEHOLDER
}

/// What [`WorldOpen`]'s systems read while it runs: the epoch, the spec's clock, the saved ticks
/// and clock.
#[derive(Resource)]
pub(crate) struct OpenContext {
    pub(crate) epoch: WorldEpoch,
    /// The saved clock (only with `PersistSettings::clock`), else the spec's.
    pub(crate) clock: Option<u64>,
    pub(crate) ticks: Option<Vec<(BlockPos, u64)>>,
    /// Written by the open: how long the far LOD's seed took.
    pub(crate) lod_seed: Duration,
}
