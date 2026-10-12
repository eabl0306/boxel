//! [`PersistPlugin`]: saving and loading a world while it streams (ADR 0024).
use super::entities::{EntityCodecs, SavedLog, entity_step, entity_work, save_entities_on_exit};
use super::saver::{Moment, Persist};
use super::store::{WorldStore, clock_meta, ticks_meta};
use crate::jobs::{IoTaskPoolExecutor, JobExecutor};
use crate::streaming::WorldJobs;
use crate::{BlockTicks, ChunkMap, TimeOfDay, WorldContent, WorldSystems};
use boxel_app::{App, Last, Plugin, PreUpdate, Shutdown, Startup, Update};
use boxel_diagnostic::DiagnosticsBoard;
use boxel_ecs::prelude::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Knobs of saving. Read every frame.
#[derive(Resource, Clone, Debug)]
pub struct PersistSettings {
    /// While the game runs, how often the loaded chunks with edits, the pending block ticks and
    /// the world clock are saved (a crash loses at most this much). `None`: only when a chunk
    /// unloads and on exit. Default 60 s.
    pub autosave: Option<Duration>,
    /// Most chunk records per save job (one write transaction). Default 256. A job that also
    /// writes the pending ticks and the clock writes every chunk handed over with them, in the
    /// same transaction: what the disk holds is always one moment of the world.
    pub max_batch: usize,
    /// After a save fails, how long until the next try (its chunks stay in memory meanwhile).
    /// Default 1 s.
    pub retry_after: Duration,
    /// Saves the world clock ([`TimeOfDay`]) and sets it back at startup. Default true; a game
    /// that sets the clock itself (a fixed hour) turns it off before the first update.
    pub clock: bool,
}

impl Default for PersistSettings {
    fn default() -> Self {
        Self {
            autosave: Some(Duration::from_secs(60)),
            max_batch: 256,
            retry_after: Duration::from_secs(1),
            clock: true,
        }
    }
}

/// The `Shutdown` sets of [`PersistPlugin`], chained: the exit save, then the store's close.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorldShutdownSystems {
    /// Waits for the running save, then writes the saved entities, every loaded chunk with
    /// edits, what is still staged, the pending ticks and the clock, on this thread.
    Save,
    /// Closes the [`WorldStore`] ([`WorldStore::close`]): the file is closed cleanly and
    /// released before the app returns, whoever still holds a clone. An app's own `Shutdown`
    /// system that writes the store ([`WorldStore::put_meta`]) goes `.before(Close)`: after it,
    /// the write fails with the closed error.
    Close,
}

/// Saves and loads the world of [`WorldPlugin`](crate::WorldPlugin) (add it after that one) in
/// `store` (ADR 0024):
///
/// - A chunk with edits is saved when it unloads, by the autosave
///   ([`PersistSettings::autosave`]) and on exit (the `Shutdown` schedule, which the runners
///   run). Chunks nobody edited are never saved: they come from the generator again. The exit
///   then closes the store ([`WorldShutdownSystems`]).
/// - A saved chunk is loaded instead of generated, in the chunk's generation job. A sky walk
///   (the light's sky tops, ADR 0015) reads saved chunks too, so the light of a saved world is
///   the light it had.
/// - The pending block ticks ([`BlockTicks`], flowing water) and the world clock
///   ([`TimeOfDay`], unless [`PersistSettings::clock`] is off) are saved with the autosave, on
///   exit and with chunks that unload while ticks wait, always in one transaction with every
///   edited chunk (one moment of the world), and restored at startup.
/// - [`Saved`](super::Saved) entities, through the registered
///   [`EntityCodec`](super::EntityCodec)s: in `PreUpdate`, the ones whose chunk has no slot
///   (it unloaded) are saved and despawned, and the ones of the chunks that loaded come back,
///   before the frame's fixed steps; the autosave and the exit save every one alive, in the
///   same moment and transaction as the edited chunks, the ticks and the clock. A job that
///   writes entity records writes every staged chunk with them.
///
/// A world whose file was repaired when it opened (it was not closed cleanly: a crash) is
/// reported with a warning when the plugin is added ([`WorldStore::repaired`]).
///
/// Disk writes run in save jobs on `executor` (the IO task pool by default), one at a time; a
/// frame only hands chunks over and starts a job. Loads read the store inside the generation
/// jobs. A failed save keeps its chunks in memory (loads read them) and tries again after
/// [`PersistSettings::retry_after`]. Adds the [`WorldStore`] and [`PersistSettings`] resources,
/// and a `"save"` line to `DiagnosticsBoard` when present.
pub struct PersistPlugin {
    /// The open world, opened for this app: a [`WorldStore`] serves one `PersistPlugin` (a
    /// second one panics).
    pub store: WorldStore,
    /// Where save jobs run (tests use `jobs::InlineExecutor`).
    pub executor: Arc<dyn JobExecutor>,
}

impl PersistPlugin {
    /// Saves to `store`, with save jobs on the IO task pool.
    pub fn new(store: WorldStore) -> Self {
        Self {
            store,
            executor: Arc::new(IoTaskPoolExecutor),
        }
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
        assert_eq!(
            states,
            self.store.ids().registry_states(),
            "the WorldStore was opened with another registry than the WorldPlugin's"
        );
        // Logged here, not while the file opened: a game may open its world before `LogPlugin`
        // installs the subscriber (`voxel_world` does), and that line would be lost.
        if self.store.repaired() {
            tracing::warn!("the world file was not closed cleanly: redb repaired it");
        }
        let mut jobs = world
            .get_resource_mut::<WorldJobs>()
            .expect("PersistPlugin needs WorldPlugin first");
        assert!(jobs.persist.is_none(), "one PersistPlugin per world");
        jobs.persist = Some(Persist::new(self.store.clone(), Arc::clone(&self.executor)));
        app.insert_resource(self.store.clone())
            .init_resource::<PersistSettings>()
            .init_resource::<EntityCodecs>()
            .init_resource::<SavedLog>()
            .add_systems(Startup, restore_saved)
            // Exclusive (codecs read and spawn entities), and only when there is work. Its order
            // against the rest of `PreUpdate` (input) does not matter: it only has to come before
            // the frame's fixed steps.
            .add_systems(
                PreUpdate,
                entity_step.run_if(entity_work).ambiguous_with_all(),
            )
            .add_systems(Update, save_step.after(WorldSystems::Visibility))
            // Like the other board lines: its order does not matter.
            .add_systems(Last, publish_save_stats.ambiguous_with_all())
            .configure_sets(
                Shutdown,
                (WorldShutdownSystems::Save, WorldShutdownSystems::Close).chain(),
            )
            .add_systems(
                Shutdown,
                (
                    (save_entities_on_exit, save_on_exit)
                        .chain()
                        .in_set(WorldShutdownSystems::Save),
                    close_store_on_exit.in_set(WorldShutdownSystems::Close),
                ),
            );
    }
}

/// Startup: the pending block ticks and the world clock of a saved world.
fn restore_saved(
    mut jobs: ResMut<WorldJobs>,
    settings: Res<PersistSettings>,
    ticks: Option<ResMut<BlockTicks>>,
    time: Option<ResMut<TimeOfDay>>,
) {
    let Some((saved_ticks, clock)) = jobs.persist.as_mut().and_then(|p| p.restore.take()) else {
        return;
    };
    if let (Some(mut ticks), Some(saved)) = (ticks, saved_ticks) {
        ticks.restore(saved);
    }
    if settings.clock
        && let (Some(mut time), Some(tick)) = (time, clock)
    {
        time.tick = tick;
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
) {
    // Saving changes nothing the readers of these resources see.
    let Some(p) = jobs.bypass_change_detection().persist.as_mut() else {
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
        let autosave = settings
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
        }
    }
    p.start(now, settings.max_batch);
}

/// `Shutdown`: waits for the running save, then writes every loaded chunk with edits, what is
/// still staged, the pending ticks and the clock, on this thread.
fn save_on_exit(
    mut jobs: ResMut<WorldJobs>,
    mut map: ResMut<ChunkMap>,
    settings: Res<PersistSettings>,
    ticks: Option<Res<BlockTicks>>,
    time: Option<Res<TimeOfDay>>,
) {
    let Some(p) = jobs.persist.as_mut() else {
        return;
    };
    p.wait();
    p.stage_dirty(&mut map);
    save_meta(p, &settings, ticks.as_deref(), time.as_deref());
    match p.save_now(settings.max_batch) {
        Ok(()) => tracing::info!(stats = ?p.store.stats(), "world saved"),
        Err(e) => tracing::error!("the world could not be saved: {e}"),
    }
}

/// `Shutdown`, after the exit save: closes the store on this thread, so the file is closed
/// cleanly and released before the app returns (a relaunch opens it at once, without a repair),
/// whoever still holds a clone of it.
fn close_store_on_exit(store: Res<WorldStore>) {
    match store.close() {
        Ok(()) => tracing::debug!(handles = store.handles(), "the world store is closed"),
        Err(e) => tracing::error!("the world store could not be closed: {e}"),
    }
}

/// The `"save"` line of the `DiagnosticsBoard`, when one is present and the counts changed.
fn publish_save_stats(
    store: Res<WorldStore>,
    jobs: Res<WorldJobs>,
    board: Option<ResMut<DiagnosticsBoard>>,
    mut last: Local<Option<(super::PersistStats, usize)>>,
) {
    let Some(mut board) = board else {
        return;
    };
    let now = (
        store.stats(),
        jobs.persist.as_ref().map_or(0, Persist::queued),
    );
    if *last == Some(now) {
        return;
    }
    *last = Some(now);
    let (stats, queued) = now;
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
