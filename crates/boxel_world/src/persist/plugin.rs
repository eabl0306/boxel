//! [`PersistPlugin`]: saving and loading a world while it streams (ADR 0024).
use super::saver::Persist;
use super::store::{WorldStore, clock_meta, ticks_meta};
use crate::jobs::{IoTaskPoolExecutor, JobExecutor};
use crate::streaming::WorldJobs;
use crate::{BlockTicks, ChunkMap, TimeOfDay, WorldContent, WorldSystems};
use boxel_app::{App, Last, Plugin, Shutdown, Startup, Update};
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
    /// Most chunk records per save job (one write transaction). Default 256.
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

/// Saves and loads the world of [`WorldPlugin`](crate::WorldPlugin) (add it after that one) in
/// `store` (ADR 0024):
///
/// - A chunk with edits is saved when it unloads, by the autosave
///   ([`PersistSettings::autosave`]) and on exit (the `Shutdown` schedule, which the runners
///   run). Chunks nobody edited are never saved: they come from the generator again.
/// - A saved chunk is loaded instead of generated, in the chunk's generation job. A sky walk
///   (the light's sky tops, ADR 0015) reads saved chunks too, so the light of a saved world is
///   the light it had.
/// - The pending block ticks ([`BlockTicks`], flowing water) and the world clock
///   ([`TimeOfDay`], unless [`PersistSettings::clock`] is off) are saved with the autosave and
///   on exit, and restored at startup.
///
/// Disk writes run in save jobs on `executor` (the IO task pool by default), one at a time; a
/// frame only hands chunks over and starts a job. Loads read the store inside the generation
/// jobs. A failed save keeps its chunks in memory (loads read them) and tries again after
/// [`PersistSettings::retry_after`]. Adds the [`WorldStore`] and [`PersistSettings`] resources,
/// and a `"save"` line to `DiagnosticsBoard` when present.
pub struct PersistPlugin {
    /// The open world.
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
        let mut jobs = world
            .get_resource_mut::<WorldJobs>()
            .expect("PersistPlugin needs WorldPlugin first");
        assert!(jobs.persist.is_none(), "one PersistPlugin per world");
        jobs.persist = Some(Persist::new(self.store.clone(), Arc::clone(&self.executor)));
        app.insert_resource(self.store.clone())
            .init_resource::<PersistSettings>()
            .add_systems(Startup, restore_saved)
            .add_systems(Update, save_step.after(WorldSystems::Visibility))
            // Like the other board lines: its order does not matter.
            .add_systems(Last, publish_save_stats.ambiguous_with_all())
            .add_systems(Shutdown, save_on_exit);
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
fn save_meta(
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

/// Every frame, after the world's systems: the autosave when due, then the save job (one at a
/// time; it writes what unloading and the autosave handed over).
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
    if let Some(every) = settings.autosave
        && now.saturating_duration_since(p.last_autosave) >= every
    {
        p.last_autosave = now;
        p.stage_dirty(map.bypass_change_detection());
        save_meta(p, &settings, ticks.as_deref(), time.as_deref());
    }
    p.frame(now, settings.max_batch, settings.retry_after);
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
            "saved={} loaded={} queued={} pending={} load_errors={} save_errors={}",
            stats.saved_chunks,
            stats.loaded_chunks,
            queued,
            stats.pending,
            stats.load_errors,
            stats.save_errors
        ),
    );
}
