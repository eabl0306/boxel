//! [`PersistPlugin`]: saving and loading a world while it streams (ADR 0024).
use super::entities::{EntityCodecs, SavedLog, close_snapshot, entity_step, entity_work};
use super::saver::{Moment, Persist};
use super::store::{MetaValue, WorldStore, clock_meta, ticks_meta};
use crate::jobs::{IoTaskPoolExecutor, JobExecutor};
use crate::lifecycle::{
    PendingOpen, SaveSystems, TeardownSystems, WorldEpoch, WorldLifecycle, WorldLifecycleSystems,
    WorldSave, WorldScopeApp, WorldTeardown,
};
use crate::streaming::WorldJobs;
use crate::{BlockTicks, ChunkMap, TimeOfDay, WorldContent, WorldSystems};
use boxel_app::{App, Last, Plugin, PreUpdate, Update};
use boxel_diagnostic::DiagnosticsBoard;
use boxel_ecs::prelude::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Knobs of saving. Read every frame.
#[derive(Resource, Clone, Debug)]
pub struct PersistSettings {
    /// While the game runs, how often the loaded chunks with edits, the pending block ticks and
    /// the world clock are saved (a crash loses at most this much). `None`: only when a chunk
    /// unloads, when the world closes and on exit. Default 60 s.
    pub autosave: Option<Duration>,
    /// Most chunk records per save job (one write transaction). Default 256. A job that also
    /// writes the pending ticks and the clock writes every chunk handed over with them, in the
    /// same transaction: what the disk holds is always one moment of the world.
    pub max_batch: usize,
    /// After a save fails, how long until the next try (its chunks stay in memory meanwhile).
    /// Default 1 s.
    pub retry_after: Duration,
    /// Saves the world clock ([`TimeOfDay`]) and sets it back when the world opens. Default
    /// true; a game that sets the clock itself (a fixed hour) turns it off before the first
    /// update.
    pub clock: bool,
    /// How long a closing world keeps trying to write its last save (a full disk): then it
    /// gives up, closes the file and says so (`CloseReport::gave_up`). Default 10 s.
    pub close_timeout: Duration,
}

impl Default for PersistSettings {
    fn default() -> Self {
        Self {
            autosave: Some(Duration::from_secs(60)),
            max_batch: 256,
            retry_after: Duration::from_secs(1),
            clock: true,
            close_timeout: crate::lifecycle::default_close_timeout(),
        }
    }
}

/// Where save jobs, the store's close and the drop of a closed world run.
#[derive(Resource, Clone)]
pub(crate) struct PersistExecutor(pub(crate) Arc<dyn JobExecutor>);

/// Saves and loads the worlds of [`WorldPlugin`](crate::WorldPlugin) (add it after that one)
/// (ADR 0024):
///
/// - A chunk with edits is saved when it unloads, by the autosave
///   ([`PersistSettings::autosave`]), when its world closes and on exit (the `Shutdown`
///   schedule, which the runners run). Chunks nobody edited are never saved: they come from the
///   generator again. The exit then closes the store
///   ([`WorldShutdownSystems`](crate::lifecycle::WorldShutdownSystems)).
/// - A saved chunk is loaded instead of generated, in the chunk's generation job. A sky walk
///   (the light's sky tops, ADR 0015) reads saved chunks too, so the light of a saved world is
///   the light it had.
/// - The pending block ticks ([`BlockTicks`], flowing water) and the world clock
///   ([`TimeOfDay`], unless [`PersistSettings::clock`] is off) are saved with the autosave, at
///   the close, on exit and with chunks that unload while ticks wait, always in one transaction
///   with every edited chunk (one moment of the world), and restored when the world opens.
/// - [`Saved`](super::Saved) entities, through the registered
///   [`EntityCodec`](super::EntityCodec)s: in `PreUpdate`, the ones whose chunk has no slot
///   (it unloaded) are saved and despawned, and the ones of the chunks that loaded come back,
///   before the frame's fixed steps; the autosave, the close and the exit save every one alive,
///   in the same moment and transaction as the edited chunks, the ticks and the clock. A job
///   that writes entity records writes every staged chunk with them.
///
/// [`PersistPlugin::new`] saves the world [`WorldPlugin::new`](crate::WorldPlugin::new) opens at
/// startup in its store; [`PersistPlugin::without_store`] only makes saved worlds possible: each
/// [`WorldSpec`](crate::lifecycle::WorldSpec) brings its own store.
///
/// Disk writes run in save jobs on the executor (the IO task pool by default), one at a time; a
/// frame only hands chunks over and starts a job. Loads read the store inside the generation
/// jobs. A failed save keeps its chunks in memory (loads read them) and tries again after
/// [`PersistSettings::retry_after`]. Adds the [`PersistSettings`] resource, the [`WorldStore`]
/// resource while a saved world is open, and a `"save"` line to `DiagnosticsBoard` when present.
pub struct PersistPlugin {
    /// The store of the startup world, taken out at build: the plugin keeps no handle of it.
    store: Mutex<Option<WorldStore>>,
    executor: Arc<dyn JobExecutor>,
}

impl PersistPlugin {
    /// Saves the world [`WorldPlugin::new`](crate::WorldPlugin::new) opens at startup in `store`
    /// (it must not have served a world), with save jobs on the IO task pool.
    pub fn new(store: WorldStore) -> Self {
        Self {
            store: Mutex::new(Some(store)),
            executor: Arc::new(IoTaskPoolExecutor),
        }
    }

    /// No startup store: the worlds opened later bring theirs
    /// ([`WorldSpec::store`](crate::lifecycle::WorldSpec::store)).
    pub fn without_store() -> Self {
        Self {
            store: Mutex::new(None),
            executor: Arc::new(IoTaskPoolExecutor),
        }
    }

    /// Runs the save jobs, the store's close and the drop of closed worlds on `executor`
    /// (tests use `jobs::InlineExecutor`).
    #[must_use]
    pub fn with_executor(mut self, executor: Arc<dyn JobExecutor>) -> Self {
        self.executor = executor;
        self
    }
}

impl Plugin for PersistPlugin {
    fn build(&self, app: &mut App) {
        let world = app.world_mut();
        let states = world
            .get_resource::<WorldContent>()
            .expect("PersistPlugin needs WorldPlugin first")
            .registry
            .len();
        assert!(
            !world.contains_resource::<PersistExecutor>(),
            "one PersistPlugin per app"
        );
        let store = self.store.lock().expect("store lock").take();
        if let Some(store) = store {
            assert_eq!(
                states,
                store.ids().registry_states(),
                "the WorldStore was opened with another registry than the WorldPlugin's"
            );
            let mut pending = world.get_resource_mut::<PendingOpen>().expect(
                "PersistPlugin::new saves the world WorldPlugin::new opens at startup: with \
                 WorldPlugin::closed use PersistPlugin::without_store and open each world with \
                 its store",
            );
            pending.store = Some(store);
        }
        app.insert_resource(PersistExecutor(Arc::clone(&self.executor)))
            .init_resource::<PersistSettings>()
            .init_resource::<EntityCodecs>()
            .init_resource::<SavedLog>()
            .world_resource::<SavedLog>(SavedLog::is_empty)
            // Exclusive (codecs read and spawn entities), and only when there is work. Its order
            // against the rest of `PreUpdate` (input) does not matter: it only has to come before
            // the frame's fixed steps, and after the lifecycle's requests.
            .add_systems(
                PreUpdate,
                entity_step
                    .run_if(entity_work)
                    .after(WorldLifecycleSystems)
                    .ambiguous_with_all(),
            )
            .add_systems(Update, save_step.after(WorldSystems::Visibility))
            // Like the other board lines: its order does not matter.
            .add_systems(Last, publish_save_stats.ambiguous_with_all())
            .add_systems(WorldSave, close_snapshot.in_set(SaveSystems::World))
            .add_systems(
                WorldTeardown,
                teardown_persist.in_set(TeardownSystems::Persist),
            );
    }
}

/// `WorldTeardown`, `TeardownSystems::Persist`: the save state and the store go to the close in
/// progress (its last job runs on; the store is closed off the main thread), the markers the
/// despawns logged are dropped, and the `"save"` line goes.
fn teardown_persist(world: &mut World) {
    let persist = world.resource_mut::<WorldJobs>().persist.take();
    let store = world.remove_resource::<WorldStore>();
    if let Some(closing) = world.resource_mut::<WorldLifecycle>().closing.as_mut() {
        closing.persist = persist;
        closing.store = store;
    } else {
        debug_assert!(persist.is_none(), "a teardown outside a close");
    }
    world.resource_mut::<SavedLog>().clear();
    if let Some(mut board) = world.get_resource_mut::<DiagnosticsBoard>() {
        board.remove("save");
    }
}

/// The game data staged for the next save moment ([`WorldLifecycle::stage_meta`]) joins it.
pub(super) fn stage_game_meta(p: &mut Persist, lifecycle: &mut WorldLifecycle) {
    for (name, value) in lifecycle.staged_meta.drain(..) {
        p.set_meta(name, MetaValue::Bytes(value));
    }
}

/// The metadata of a save: the pending ticks and the clock, when the app has them (the clock
/// only with [`PersistSettings::clock`]).
pub(super) fn save_meta(
    p: &mut Persist,
    settings: &PersistSettings,
    ticks: Option<&BlockTicks>,
    time: Option<&TimeOfDay>,
) {
    if let Some(ticks) = ticks {
        let (name, value) = ticks_meta(ticks.pending());
        p.set_meta(name, value);
    }
    if let Some(time) = time.filter(|_| settings.clock) {
        let (name, value) = clock_meta(time.tick);
        p.set_meta(name, value);
    }
}

/// Every frame, after the world's systems: the save job (one at a time; it writes what unloading
/// and the autosave handed over).
///
/// The disk always holds one moment of the world: the pending ticks and the clock go with every
/// edited chunk, loaded or handed over, in one transaction. That snapshot is taken in the frame
/// its job starts, by the autosave and whenever chunks with edits unloaded while block ticks
/// wait: their water must never be saved without its ticks (a crash before the next autosave
/// would leave it stuck half spread), nor the ticks without the water of the loaded chunks. A
/// failed snapshot is taken again when it is tried again, not written stale with newer chunks.
///
/// While saved entities are alive they are part of that moment, and only the entity step can
/// save them: the snapshot is asked from it ([`Moment`]), it takes the chunks, the ticks, the
/// clock and the entities in the next `PreUpdate` (no fixed step in between), and the job starts
/// in the next frame.
fn save_step(
    mut jobs: ResMut<WorldJobs>,
    mut map: ResMut<ChunkMap>,
    settings: Res<PersistSettings>,
    ticks: Option<Res<BlockTicks>>,
    time: Option<Res<TimeOfDay>>,
    mut log: ResMut<SavedLog>,
    mut lifecycle: ResMut<WorldLifecycle>,
) {
    // Saving changes nothing the readers of these resources see.
    let Some(p) = jobs.bypass_change_detection().persist.as_mut() else {
        // An unsaved world: no entity step follows the markers.
        if !log.is_empty() {
            log.clear();
        }
        return;
    };
    let now = Instant::now();
    p.poll(now, settings.retry_after);
    match p.moment {
        // The entity step takes it before the next fixed step; nothing starts before.
        Moment::Asked => return,
        Moment::Taken => {
            if p.can_start(now) {
                p.moment = Moment::None;
                p.start(now, settings.max_batch);
            }
            return;
        }
        Moment::None => {}
    }
    if p.can_start(now) {
        let autosave = std::mem::take(&mut p.save_requested)
            || settings
                .autosave
                .is_some_and(|every| now.saturating_duration_since(p.last_autosave) >= every);
        let ticking = ticks.as_deref().is_some_and(|t| {
            let stats = t.stats();
            stats.queued + stats.frozen > 0
        });
        if autosave || p.has_meta() || (ticking && p.queued() > 0) {
            p.last_autosave = now;
            if !p.entities.live.is_empty() {
                p.moment = Moment::Asked;
                return;
            }
            p.stage_dirty(map.bypass_change_detection());
            save_meta(p, &settings, ticks.as_deref(), time.as_deref());
            stage_game_meta(p, lifecycle.bypass_change_detection());
        }
    }
    p.start(now, settings.max_batch);
}

/// `Shutdown`, after the exit save: closes the store on this thread, so the file is closed
/// cleanly and released before the app returns (a relaunch opens it at once, without a repair),
/// whoever still holds a clone of it.
pub(crate) fn close_store_on_exit(store: Option<Res<WorldStore>>) {
    let Some(store) = store else {
        return;
    };
    match store.close() {
        Ok(()) => tracing::debug!(handles = store.handles(), "the world store is closed"),
        Err(e) => tracing::error!("the world store could not be closed: {e}"),
    }
}

/// The `"save"` line of the `DiagnosticsBoard`, when one is present, a saved world is open and
/// the counts changed (or the world did: the line comes back after a reopen with equal counts).
fn publish_save_stats(
    store: Option<Res<WorldStore>>,
    jobs: Res<WorldJobs>,
    epoch: Res<WorldEpoch>,
    board: Option<ResMut<DiagnosticsBoard>>,
    mut last: Local<Option<(WorldEpoch, super::PersistStats, usize)>>,
) {
    let (Some(mut board), Some(store)) = (board, store) else {
        return;
    };
    let now = (
        *epoch,
        store.stats(),
        jobs.persist.as_ref().map_or(0, Persist::queued),
    );
    if *last == Some(now) {
        return;
    }
    *last = Some(now);
    let (_, stats, queued) = now;
    board.set(
        "save",
        format!(
            "saved={} loaded={} queued={} pending={} load_errors={} save_errors={} \
             entities_saved={} entities_loaded={} entity_errors={}",
            stats.saved_chunks,
            stats.loaded_chunks,
            queued,
            stats.pending,
            stats.load_errors,
            stats.save_errors,
            stats.saved_entities,
            stats.loaded_entities,
            stats.entity_errors
        ),
    );
}
