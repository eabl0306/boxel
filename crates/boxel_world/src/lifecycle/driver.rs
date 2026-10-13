//! The lifecycle's driver: opens and closes worlds, and polls a close until it ends.
use super::scope::after_teardown;
use super::{
    CloseOptions, CloseReport, OpenContext, OpenWorldError, Request, WorldEpoch, WorldEvent,
    WorldGarbage, WorldLifecycle, WorldOpen, WorldSave, WorldScoped, WorldSpec, WorldStatus,
    WorldTeardown,
};
use crate::jobs::{IoTaskPoolExecutor, JobExecutor};
use crate::lod::{LodFeedback, LodWorld};
use crate::persist::{
    ChunkStore, Persist, PersistExecutor, PersistSettings, PersistStats, Saved, WeakStore,
    WorldStore, openings, put_away_all,
};
use crate::sky::{SkyMap, SkyRule};
use crate::streaming::WorldJobs;
use crate::{BlockTicks, EditQueue, TimeOfDay, WorldContent, WorldSettings};
use boxel_app::{IdleStep, Time};
use boxel_ecs::prelude::*;
use crossbeam_channel::{Receiver, RecvTimeoutError, TryRecvError};
use std::any::Any;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// The world [`WorldPlugin::new`](crate::WorldPlugin::new) opens before `Startup`, with the store
/// [`PersistPlugin::new`](crate::persist::PersistPlugin::new) hands over (present from the build
/// to `PreStartup`).
#[derive(Resource, Default)]
pub(crate) struct PendingOpen {
    pub(crate) store: Option<WorldStore>,
}

/// `TimeOfDay::tick` when the app started (`PreStartup`): the clock of a world whose store and
/// spec have none.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub(crate) struct StartClock(pub(crate) u64);

/// How long a close waits for its last save by default (`PersistSettings::close_timeout`).
const DEFAULT_CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// A handle of the closed store still alive this long after the teardown is logged as a warning
/// (only a leaked clone explains it).
const HANDLE_WARN_AFTER: Duration = Duration::from_secs(2);

/// Where a close stands.
enum Phase {
    /// The last save job runs (or retries).
    Saving,
    /// The store's close and the drop of the save state run on the IO pool; the result of the
    /// close comes back.
    Releasing(Receiver<Result<(), String>>),
    /// Only the drop of the teardown's garbage may be left.
    Released,
}

/// A world being closed (status [`WorldStatus::Closing`]).
pub(crate) struct ClosingWorld {
    epoch: WorldEpoch,
    started: Instant,
    /// The last save gives up at this time.
    deadline: Instant,
    /// Handed over by the persistence's teardown.
    pub(crate) persist: Option<Persist>,
    pub(crate) store: Option<WorldStore>,
    stats_before: Option<PersistStats>,
    /// The closed store, without keeping it alive (its handles at the end).
    weak: Option<WeakStore>,
    backend: Option<Weak<dyn ChunkStore>>,
    phase: Phase,
    /// Reports once the teardown's garbage is dropped.
    garbage: Option<Receiver<()>>,
    report: CloseReport,
}

/// Run condition of the driver: requests wait, or a close is in progress.
pub(crate) fn lifecycle_busy(lifecycle: Option<Res<WorldLifecycle>>) -> bool {
    lifecycle.is_some_and(|l| !l.requests.is_empty() || l.status == WorldStatus::Closing)
}

/// Where the drops and the store's close run: the persistence's executor (the IO pool unless a
/// test passed its own), else the IO pool. Never the chunk workers.
fn io_executor(world: &World) -> Arc<dyn JobExecutor> {
    world
        .get_resource::<PersistExecutor>()
        .map_or_else(|| Arc::new(IoTaskPoolExecutor) as _, |e| Arc::clone(&e.0))
}

/// The stall of a close or an open: the fixed loop must not catch up on it.
fn declare_stall(world: &mut World, stall: Duration) {
    if let Some(mut time) = world.get_resource_mut::<Time>() {
        time.declare_stall(stall);
    }
}

/// The store of a refused spec: closed and dropped off the main thread (the last handle of an
/// open redb file writes and syncs it when dropped). Only dropped when it shares its backend
/// with the open world's store or the closing world's (a clone of it, or another opening of its
/// file): closing it would close the file that world still reads or writes its last save to.
fn refuse_store(world: &World, store: Option<WorldStore>) {
    let Some(store) = store else {
        return;
    };
    let backend = Arc::downgrade(store.backend());
    let shared = world
        .get_resource::<WorldStore>()
        .is_some_and(|open| Arc::ptr_eq(open.backend(), store.backend()))
        || world
            .get_resource::<WorldLifecycle>()
            .and_then(|l| l.closing.as_ref())
            .and_then(|c| c.backend.as_ref())
            .is_some_and(|closing| Weak::ptr_eq(closing, &backend));
    io_executor(world).spawn(Box::new(move || {
        if !shared {
            let _ = store.close();
        }
        drop(store);
    }));
}

/// `PreStartup`: records the clock the app starts with, then opens the world of
/// [`WorldPlugin::new`](crate::WorldPlugin::new) from the resources as they are now (a change an
/// app made after the build is kept).
///
/// # Panics
/// If that world does not open: a build-time mistake.
pub(crate) fn lifecycle_startup(world: &mut World) {
    let tick = world
        .get_resource::<TimeOfDay>()
        .map_or(TimeOfDay::default().tick, |t| t.tick);
    world.insert_resource(StartClock(tick));
    let Some(pending) = world.remove_resource::<PendingOpen>() else {
        return;
    };
    let spec = WorldSpec {
        generator: Arc::clone(&world.resource::<WorldContent>().generator),
        settings: world.resource::<WorldSettings>().clone(),
        store: pending.store,
        clock: None,
    };
    if let Err(e) = open_world(world, spec) {
        panic!("the world of WorldPlugin::new does not open: {e}");
    }
}

/// `PreUpdate` (exclusive, before the persistence's entity step): polls the close in progress,
/// then runs the requests in order. An open waits while a world closes.
pub(crate) fn drive_world_lifecycle(world: &mut World) {
    poll_closing(world);
    loop {
        let mut lifecycle = world.resource_mut::<WorldLifecycle>();
        let status = lifecycle.status;
        let Some(front) = lifecycle.requests.front() else {
            break;
        };
        if status == WorldStatus::Closing && matches!(front, Request::Open(_)) {
            break;
        }
        let Some(request) = lifecycle.requests.pop_front() else {
            break;
        };
        match (status, request) {
            (WorldStatus::Closed, Request::Open(spec)) => {
                if let Err(error) = open_world(world, *spec) {
                    tracing::warn!("a world did not open: {error}");
                    world.write_message(WorldEvent::OpenFailed { error });
                }
            }
            (WorldStatus::Open, Request::Open(spec)) => {
                refuse_store(world, spec.store);
                world.write_message(WorldEvent::OpenFailed {
                    error: OpenWorldError::NotClosed,
                });
            }
            (WorldStatus::Open, Request::Close(options)) => {
                close_now(world, options);
                poll_closing(world);
            }
            (WorldStatus::Open, Request::Save) => {
                if let Some(p) = world.resource_mut::<WorldJobs>().persist.as_mut() {
                    p.save_requested = true;
                }
            }
            (_, Request::Close(_) | Request::Save) => {
                tracing::debug!(?status, "a close or save request without an open world");
            }
            (WorldStatus::Closing, Request::Open(_)) => unreachable!("an open waits for Closed"),
        }
    }
}

/// Why `spec` cannot open now.
fn check_open(world: &World, spec: &WorldSpec) -> Result<(), OpenWorldError> {
    if world.contains_resource::<IdleStep>() {
        return Err(OpenWorldError::InIdle);
    }
    let lifecycle = world
        .get_resource::<WorldLifecycle>()
        .expect("worlds open with WorldPlugin");
    if lifecycle.status != WorldStatus::Closed {
        return Err(OpenWorldError::NotClosed);
    }
    let Some(store) = &spec.store else {
        return Ok(());
    };
    if !world.contains_resource::<PersistExecutor>() {
        return Err(OpenWorldError::NoPersistPlugin);
    }
    let registry = world.resource::<WorldContent>().registry.len();
    let states = store.ids().registry_states();
    if states != registry {
        return Err(OpenWorldError::OtherRegistry {
            store: states,
            registry,
        });
    }
    if !store.has_opened() {
        return Err(OpenWorldError::StoreServed);
    }
    if store.is_closed() {
        return Err(OpenWorldError::StoreClosed);
    }
    let backend = Arc::downgrade(store.backend());
    if lifecycle
        .closed_backends
        .iter()
        .any(|(b, closed_at)| Weak::ptr_eq(b, &backend) && store.opened_seq() < *closed_at)
    {
        return Err(OpenWorldError::StoreOpenedEarly);
    }
    Ok(())
}

/// Opens `spec` now (an exclusive caller: a test, or a game's screen hook in `PreUpdate` before
/// [`WorldLifecycleSystems`](super::WorldLifecycleSystems)): its content, settings and store are
/// installed, [`WorldOpen`] runs and [`WorldEvent::Opened`] is written. Refused when a world is
/// open or closing, in an `Idle` step, or for a store that cannot serve it ([`OpenWorldError`]);
/// a refused spec's store is closed off the main thread (only dropped when it shares the open or
/// closing world's backend), and the app's world is left as it was.
pub fn open_world(world: &mut World, spec: WorldSpec) -> Result<WorldEpoch, OpenWorldError> {
    let started = Instant::now();
    if let Err(e) = check_open(world, &spec) {
        refuse_store(world, spec.store);
        return Err(e);
    }
    let WorldSpec {
        generator,
        settings,
        store,
        clock,
    } = spec;
    let rule = SkyRule::new(
        settings.light,
        generator.vertical_chunks(),
        settings.sky_walk_limit,
    );
    if rule == SkyRule::Open {
        tracing::warn!(
            generator = generator.name(),
            "the generator does not declare its top (WorldGenerator::vertical_chunks): light runs \
             without sky tops, every column is open sky"
        );
    }
    world.resource_mut::<WorldContent>().generator = generator;
    world.resource_mut::<SkyMap>().rule = rule;
    {
        let mut jobs = world.resource_mut::<WorldJobs>();
        let now = jobs.clock_now();
        jobs.begin_frame(&settings, now);
    }
    *world.resource_mut::<WorldSettings>() = settings;
    let mut restored = None;
    if let Some(store) = store {
        // Logged here, where the app's log is up (a game may open its store before it is).
        if store.repaired() {
            tracing::warn!("the world file was not closed cleanly: redb repaired it");
        }
        let executor = Arc::clone(&world.resource::<PersistExecutor>().0);
        let mut persist = Persist::new(store.clone(), executor);
        restored = persist.restore.take();
        world.resource_mut::<WorldJobs>().persist = Some(persist);
        world.insert_resource(store);
    }
    let epoch = {
        let mut lifecycle = world.resource_mut::<WorldLifecycle>();
        lifecycle.status = WorldStatus::Open;
        // Values staged while no world was open belong to none.
        lifecycle.staged_meta.clear();
        let mut epoch = world.resource_mut::<WorldEpoch>();
        epoch.0 += 1;
        *epoch
    };
    let (ticks, saved_clock) = restored.unwrap_or_default();
    let keep_clock = world
        .get_resource::<PersistSettings>()
        .is_none_or(|s| s.clock);
    world.insert_resource(OpenContext {
        epoch,
        clock: saved_clock.filter(|_| keep_clock).or(clock),
        ticks,
    });
    let _ = world.try_run_schedule(WorldOpen);
    world.remove_resource::<OpenContext>();
    world.write_message(WorldEvent::Opened { epoch });
    tracing::debug!(epoch = epoch.get(), took = ?started.elapsed(), "world opened");
    declare_stall(world, started.elapsed());
    Ok(epoch)
}

/// `WorldOpen`, [`OpenSystems::Engine`](super::OpenSystems::Engine): the restored ticks, the
/// clock (saved, else the spec's, else the one the app started with), the far LOD's store and
/// saved chunks, and the leftovers a frame of the closed world may have written after its
/// teardown.
pub(crate) fn open_engine_state(
    mut ctx: ResMut<OpenContext>,
    start: Option<Res<StartClock>>,
    ticks: Option<ResMut<BlockTicks>>,
    time: Option<ResMut<TimeOfDay>>,
    lod: Option<ResMut<LodWorld>>,
    feedback: Option<ResMut<LodFeedback>>,
    mut edits: ResMut<EditQueue>,
    jobs: Res<WorldJobs>,
    store: Option<Res<WorldStore>>,
) {
    if let (Some(mut ticks), Some(saved)) = (ticks, ctx.ticks.take()) {
        ticks.restore(saved);
    }
    if let Some(mut time) = time {
        let start = start.map_or(TimeOfDay::default().tick, |s| s.0);
        time.tick = ctx.clock.unwrap_or(start);
    }
    if let Some(mut lod) = lod {
        lod.open_world(
            store.as_deref().cloned(),
            jobs.persist.as_ref(),
            ctx.epoch.get(),
        );
    }
    if let Some(mut feedback) = feedback {
        feedback.refused.clear();
        feedback.chunks.clear();
    }
    edits.edits.clear();
}

/// Closes the open world now (an exclusive caller, as [`open_world`]): [`WorldSave`] (unless
/// `options.save` is off), [`WorldTeardown`], then [`WorldStatus::Closing`] until the last save,
/// the store's close and the drop of the world's memory end off the main thread
/// ([`WorldEvent::Closed`], polled in `PreUpdate`; with inline executors it may end here).
/// False, doing nothing, when no world is open or in an `Idle` step.
pub fn close_world(world: &mut World, options: CloseOptions) -> bool {
    if world.contains_resource::<IdleStep>()
        || world
            .get_resource::<WorldLifecycle>()
            .is_none_or(|l| l.status != WorldStatus::Open)
    {
        return false;
    }
    close_now(world, options);
    poll_closing(world);
    true
}

/// The close frame: the save snapshot and the teardown (see [`close_world`]).
fn close_now(world: &mut World, options: CloseOptions) {
    let started = Instant::now();
    let settings = world
        .get_resource::<PersistSettings>()
        .cloned()
        .unwrap_or_default();
    let store = world.get_resource::<WorldStore>().cloned();
    let stats_before = store.as_ref().map(WorldStore::stats);
    if options.save {
        let _ = world.try_run_schedule(WorldSave);
    } else if let Some(p) = world.resource_mut::<WorldJobs>().persist.as_mut() {
        p.discard();
    }
    world.resource_mut::<WorldLifecycle>().staged_meta.clear();
    if let Some(p) = world.resource_mut::<WorldJobs>().persist.as_mut() {
        // The moment and what is staged go in one job, whatever its size.
        p.start(Instant::now(), settings.max_batch);
    }
    let epoch = {
        let mut epoch = world.resource_mut::<WorldEpoch>();
        epoch.0 += 1;
        *epoch
    };
    world.resource_mut::<WorldLifecycle>().closing = Some(ClosingWorld {
        epoch,
        started,
        deadline: started + settings.close_timeout,
        persist: None,
        store: None,
        stats_before,
        weak: store.as_ref().map(WorldStore::downgrade),
        backend: store.as_ref().map(|s| Arc::downgrade(s.backend())),
        phase: Phase::Saving,
        garbage: None,
        report: CloseReport::default(),
    });
    drop(store);
    let _ = world.try_run_schedule(WorldTeardown);
    after_teardown(world);
    let garbage = world.resource_mut::<WorldGarbage>().take();
    let done = drop_off_thread(world, garbage);
    let teardown = started.elapsed();
    let mut lifecycle = world.resource_mut::<WorldLifecycle>();
    lifecycle.status = WorldStatus::Closing;
    if let Some(closing) = lifecycle.closing.as_mut() {
        closing.garbage = Some(done);
        closing.report.teardown = teardown;
    }
    tracing::debug!(epoch = epoch.get(), ?teardown, "world torn down");
    declare_stall(world, teardown);
}

/// Drops `garbage` on the IO pool; the receiver hears when it is done.
fn drop_off_thread(world: &World, garbage: Vec<Box<dyn Any + Send>>) -> Receiver<()> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    io_executor(world).spawn(Box::new(move || {
        drop(garbage);
        let _ = tx.send(());
    }));
    rx
}

/// One poll of the close in progress. True when it ended now ([`WorldEvent::Closed`] written).
fn poll_closing(world: &mut World) -> bool {
    let Some(mut closing) = world.resource_mut::<WorldLifecycle>().closing.take() else {
        return false;
    };
    let settings = world
        .get_resource::<PersistSettings>()
        .cloned()
        .unwrap_or_default();
    loop {
        match &mut closing.phase {
            Phase::Saving => {
                let Some(p) = closing.persist.as_mut() else {
                    start_release(world, &mut closing);
                    continue;
                };
                let now = Instant::now();
                p.poll(now, settings.retry_after);
                p.start(now, settings.max_batch);
                // An inline executor ran it already.
                p.poll(now, settings.retry_after);
                if p.idle() {
                    start_release(world, &mut closing);
                } else if now >= closing.deadline {
                    tracing::error!(
                        "the world's last save did not succeed within {:?}: what was not written \
                         is lost",
                        settings.close_timeout
                    );
                    closing.report.gave_up = true;
                    start_release(world, &mut closing);
                } else {
                    break;
                }
            }
            Phase::Releasing(rx) => match rx.try_recv() {
                Ok(result) => closing.released(result),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    closing.released(Err("the store's close was dropped".to_string()));
                }
            },
            Phase::Released => {
                let garbage_done = closing
                    .garbage
                    .as_ref()
                    .is_none_or(|rx| !matches!(rx.try_recv(), Err(TryRecvError::Empty)));
                if garbage_done {
                    finish(world, closing);
                    return true;
                }
                break;
            }
        }
    }
    world.resource_mut::<WorldLifecycle>().closing = Some(closing);
    false
}

impl ClosingWorld {
    /// The store's close answered.
    fn released(&mut self, result: Result<(), String>) {
        if let Err(e) = result {
            self.report.errors.push(e);
        }
        self.phase = Phase::Released;
    }
}

/// Ends the save phase: the report's counts and errors, then the store's close and the drop of
/// the save state on the IO pool (an unsaved world has neither).
fn start_release(world: &World, closing: &mut ClosingWorld) {
    if let Some(p) = closing.persist.as_ref() {
        if let Some(e) = p.last_error() {
            closing.report.errors.push(e.to_string());
        }
        closing.report.saved = !closing.report.gave_up && p.idle();
    }
    if let (Some(store), Some(before)) = (closing.store.as_ref(), closing.stats_before) {
        let now = store.stats();
        closing.report.chunks_written = now.saved_chunks.saturating_sub(before.saved_chunks);
        closing.report.entities_written = now.saved_entities.saturating_sub(before.saved_entities);
    }
    let (persist, store) = (closing.persist.take(), closing.store.take());
    if persist.is_none() && store.is_none() {
        closing.phase = Phase::Released;
        return;
    }
    let (tx, rx) = crossbeam_channel::bounded(1);
    io_executor(world).spawn(Box::new(move || {
        let result = store
            .as_ref()
            .map_or(Ok(()), WorldStore::close)
            .map_err(|e| format!("the world store could not be closed: {e}"));
        drop(persist);
        drop(store);
        let _ = tx.send(result);
    }));
    closing.phase = Phase::Releasing(rx);
}

/// The close ended: [`WorldStatus::Closed`], the report, [`WorldEvent::Closed`].
fn finish(world: &mut World, closing: ClosingWorld) {
    let mut report = closing.report;
    report.took = closing.started.elapsed();
    report.store_handles = closing.weak.as_ref().map_or(0, WeakStore::handles);
    if report.store_handles > 0 {
        if report.took > HANDLE_WARN_AFTER {
            tracing::warn!(
                handles = report.store_handles,
                "a closed world's store is still held (a leaked clone)"
            );
        } else {
            tracing::debug!(
                handles = report.store_handles,
                "a closed world's store is still held"
            );
        }
    }
    let epoch = closing.epoch;
    let mut lifecycle = world.resource_mut::<WorldLifecycle>();
    lifecycle
        .closed_backends
        .retain(|(b, _)| b.strong_count() > 0);
    if let Some(backend) = closing.backend {
        lifecycle.closed_backends.push((backend, openings()));
    }
    lifecycle.status = WorldStatus::Closed;
    lifecycle.last_report = Some(report.clone());
    tracing::debug!(epoch = epoch.get(), took = ?report.took, saved = report.saved, "world closed");
    world.write_message(WorldEvent::Closed { epoch, report });
}

/// Finishes the close in progress on this thread, blocking for at most `wait` (exit, tests).
/// True when no close is in progress any more.
pub fn finish_closing(world: &mut World, wait: Duration) -> bool {
    let until = Instant::now() + wait;
    loop {
        if world
            .get_resource::<WorldLifecycle>()
            .is_none_or(|l| l.status != WorldStatus::Closing)
        {
            return true;
        }
        if poll_closing(world) {
            return true;
        }
        let now = Instant::now();
        if now >= until {
            return false;
        }
        // Block on what the close waits for, a short while at most.
        let slice = (until - now).min(Duration::from_millis(5));
        let mut lifecycle = world.resource_mut::<WorldLifecycle>();
        let Some(closing) = lifecycle.closing.as_mut() else {
            continue;
        };
        match &closing.phase {
            Phase::Saving => {
                if let Some(p) = closing.persist.as_mut() {
                    p.wait_report(slice);
                }
            }
            Phase::Releasing(rx) => match rx.recv_timeout(slice) {
                Ok(result) => closing.released(result),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    closing.released(Err("the store's close was dropped".to_string()));
                }
            },
            Phase::Released => {
                if let Some(garbage) = closing.garbage.as_ref()
                    && !matches!(garbage.recv_timeout(slice), Err(RecvTimeoutError::Timeout))
                {
                    closing.garbage = None;
                }
            }
        }
    }
}

/// `Shutdown`, [`WorldShutdownSystems::Save`](super::WorldShutdownSystems::Save): an open world
/// runs [`WorldSave`] and writes everything staged on this thread; a closing one finishes its
/// close here.
pub(crate) fn save_world_on_exit(world: &mut World) {
    let status = world
        .get_resource::<WorldLifecycle>()
        .map_or(WorldStatus::Closed, |l| l.status);
    let settings = world
        .get_resource::<PersistSettings>()
        .cloned()
        .unwrap_or_default();
    match status {
        WorldStatus::Open => {
            let _ = world.try_run_schedule(WorldSave);
            let mut jobs = world.resource_mut::<WorldJobs>();
            if let Some(p) = jobs.persist.as_mut() {
                match p.save_now(settings.max_batch) {
                    Ok(()) => tracing::info!(stats = ?p.store.stats(), "world saved"),
                    Err(e) => tracing::error!("the world could not be saved: {e}"),
                }
            }
        }
        WorldStatus::Closing => {
            if !finish_closing(world, settings.close_timeout + Duration::from_secs(1)) {
                tracing::error!("the world did not finish closing before the exit");
            }
        }
        WorldStatus::Closed => {}
    }
}

/// [`TeardownSystems::Despawn`](super::TeardownSystems::Despawn): the saved entities are put
/// away (their records were made by [`WorldSave`]), then every `Saved` and [`WorldScoped`]
/// entity still alive is despawned (a world without a store keeps no index of its saved
/// entities: `Saved` means of this world).
pub(crate) fn despawn_world_entities(world: &mut World) {
    put_away_all(world);
    let doomed: Vec<Entity> = world
        .query_filtered::<Entity, Or<(With<Saved>, With<WorldScoped>)>>()
        .iter(world)
        .collect();
    for entity in doomed {
        world.despawn(entity);
    }
}

/// The default close timeout, for the persistence's settings.
pub(crate) const fn default_close_timeout() -> Duration {
    DEFAULT_CLOSE_TIMEOUT
}
