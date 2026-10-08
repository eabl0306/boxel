//! The world between frames (ADR 0022), through the `Idle` schedule as a paced runner runs it:
//! idle steps finish a border crossing in fewer frames and the world ends the same, streaming
//! meshes still wait for `Collect`, the step reports what is pending without touching the map,
//! and its data messages reach the next frame.
use super::support::{HoldExecutor, LayerGen};
use super::{drain_events, loaded, move_viewer, removes, spawn_tall_viewer, tall_world_app};
use crate::jobs::{InlineExecutor, JobExecutor};
use crate::streaming::{DISPATCH_CHECK, MIN_INTEREST_STEPS, WorldJobs};
use crate::{ChunkDataEvent, ChunkMap, ChunkMeshEvent, ChunkViewer, WorldSettings, WorldStats};
use boxel_app::{App, IdleWork, PreUpdate};
use boxel_ecs::prelude::*;
use boxel_light::ChunkLight;
use boxel_mesh::ChunkMesh;
use boxel_voxel::ChunkPos;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// An idle step with all the time it needs.
fn far() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn settled(app: &App) -> bool {
    let stats = *app.world().resource::<WorldStats>();
    stats.chunks > 0 && stats.is_settled()
}

/// The idle steps of one gap with held jobs: the jobs started so far run, then a step, until
/// the world is done or `rounds` steps ran.
fn idle_gap(app: &mut App, hold: &HoldExecutor, rounds: usize) {
    for _ in 0..rounds {
        hold.run_all();
        if app.run_idle(far()) == IdleWork::Done {
            break;
        }
    }
}

/// What a world ended with: loaded chunks, ready meshes and kept light by position.
#[derive(Debug, PartialEq)]
struct World {
    loaded: std::collections::BTreeSet<ChunkPos>,
    meshes: BTreeMap<ChunkPos, ChunkMesh>,
    light: BTreeMap<ChunkPos, Option<ChunkLight>>,
}

fn world(app: &App) -> World {
    let map = app.world().resource::<ChunkMap>();
    World {
        loaded: loaded(app),
        meshes: map
            .ready_meshes()
            .map(|(_, key, mesh)| (key.pos, ChunkMesh::clone(mesh)))
            .collect(),
        light: map
            .iter()
            .map(|(_, slot)| (slot.pos, slot.light().cloned()))
            .collect(),
    }
}

/// Positions of the `Upsert`s, sorted (a multiset), and of the `Remove`s.
fn events_of(events: &[ChunkMeshEvent]) -> (Vec<ChunkPos>, Vec<ChunkPos>) {
    let mut ups: Vec<ChunkPos> = events
        .iter()
        .filter_map(|e| match e {
            ChunkMeshEvent::Upsert { key, .. } => Some(key.pos),
            ChunkMeshEvent::Remove { .. } => None,
        })
        .collect();
    ups.sort_unstable();
    let mut rem = removes(events);
    rem.sort_unstable();
    (ups, rem)
}

/// What a crossing gave: frames until settled, the mesh events of each frame (with the idle
/// steps after it) and the world at the end.
struct Crossing {
    frames: usize,
    events: Vec<Vec<ChunkMeshEvent>>,
    world: World,
}

/// A tall world (stone under air, light as given) with a viewer of radius 3 settled at chunk
/// (0, 0, 0), then moved one chunk along x, and frames until it settles again: the jobs started
/// in a frame run before the next one, and with `idle` up to 64 idle steps (each after the jobs
/// started so far ran) follow each frame.
fn crossing(light: bool, idle_work: bool, idle: bool, inline: bool) -> Crossing {
    let hold = Arc::new(HoldExecutor::default());
    let executor: Arc<dyn JobExecutor> = if inline {
        Arc::new(InlineExecutor)
    } else {
        hold.clone()
    };
    let settings = WorldSettings {
        light,
        idle_work,
        // 64 positions per frame and a small cap: a frame alone makes slow progress.
        interest_budget: Duration::ZERO,
        max_jobs_in_flight: 8,
        ..WorldSettings::default()
    };
    let mut app = tall_world_app(LayerGen::new(-3..=1), settings, executor);
    let viewer = spawn_tall_viewer(&mut app, ChunkPos::new(0, 0, 0), 3, 2);
    for frame in 0.. {
        app.update();
        hold.run_all();
        if settled(&app) {
            break;
        }
        assert!(frame < 2000, "first load");
    }
    drain_events(&mut app);
    move_viewer(&mut app, viewer, ChunkPos::new(1, 0, 0));
    let mut events = Vec::new();
    for frames in 1.. {
        app.update();
        let mut frame = drain_events(&mut app);
        if settled(&app) {
            events.push(frame);
            return Crossing {
                frames,
                events,
                world: world(&app),
            };
        }
        if idle {
            idle_gap(&mut app, &hold, 64);
            frame.extend(drain_events(&mut app));
        } else {
            hold.run_all();
        }
        events.push(frame);
        assert!(frames < 2000, "crossing not settled");
    }
    unreachable!()
}

#[test]
fn idle_finishes_a_crossing_in_fewer_frames_with_the_same_world() {
    for light in [true, false] {
        let frames = crossing(light, true, false, false);
        let idle = crossing(light, true, true, false);
        assert!(
            idle.frames * 2 <= frames.frames,
            "light {light}: {} frames with idle steps, {} without",
            idle.frames,
            frames.frames
        );
        assert_eq!(idle.world, frames.world, "light {light}");
        let all = |c: &Crossing| events_of(&c.events.concat());
        assert_eq!(all(&idle), all(&frames), "light {light}");
        assert!(
            !all(&idle).0.is_empty(),
            "light {light}: the crossing meshes"
        );

        // Switched off, the idle steps do nothing: frame for frame the same.
        let off = crossing(light, false, true, false);
        assert_eq!(off.frames, frames.frames, "light {light}");
        let per_frame =
            |c: &Crossing| -> Vec<_> { c.events.iter().map(|e| events_of(e)).collect() };
        assert_eq!(per_frame(&off), per_frame(&frames), "light {light}");
        assert_eq!(off.world, frames.world, "light {light}");

        // Jobs that finish at once: no worse, and the same world.
        let inline = crossing(light, true, false, true);
        let inline_idle = crossing(light, true, true, true);
        assert!(inline_idle.frames <= inline.frames, "light {light}");
        assert_eq!(inline_idle.world, inline.world, "light {light}");
        assert_eq!(inline.world, frames.world, "light {light}");
    }
}

/// A tall world with held jobs and a viewer of radius 2 at the origin, after its first frame.
fn held_world(settings: WorldSettings) -> (App, Arc<HoldExecutor>) {
    let hold = Arc::new(HoldExecutor::default());
    let mut app = tall_world_app(LayerGen::new(-3..=1), settings, hold.clone());
    spawn_tall_viewer(&mut app, ChunkPos::new(0, 0, 0), 2, 1);
    app.update();
    (app, hold)
}

fn jobs(app: &App) -> &WorldJobs {
    app.world().resource::<WorldJobs>()
}

#[test]
fn idle_never_applies_streaming_results() {
    let settings = WorldSettings {
        max_mesh_events_per_frame: 4,
        ..WorldSettings::default()
    };
    let (mut app, hold) = held_world(settings);
    drain_events(&mut app);
    // Steps until every job is done: the mesh results wait, none is published by a step.
    idle_gap(&mut app, &hold, 1000);
    let waiting = jobs(&app).pending_meshes();
    assert!(waiting > 4, "{waiting} mesh results wait");
    let (ups, _) = events_of(&drain_events(&mut app));
    assert!(ups.is_empty(), "idle steps published {ups:?}");
    // The next frames publish them, 4 per frame.
    let mut published = 0;
    while !settled(&app) {
        app.update();
        let (ups, _) = events_of(&drain_events(&mut app));
        assert!(ups.len() <= 4, "{} upserts in a frame", ups.len());
        published += ups.len();
        idle_gap(&mut app, &hold, 64);
        let (ups, _) = events_of(&drain_events(&mut app));
        assert!(ups.is_empty(), "idle steps published {ups:?}");
    }
    assert!(published >= waiting);
}

#[test]
fn idle_status_matches_the_work() {
    let (mut app, hold) = held_world(WorldSettings::default());
    let map_changed = |app: &App| app.world().is_resource_changed::<ChunkMap>();
    // Only held jobs: waiting, and the map is not touched.
    assert_eq!(jobs(&app).interest_backlog(), 0);
    assert!(jobs(&app).scheduler.in_flight() > 0);
    assert_eq!(jobs(&app).idle_pending(), IdleWork::Waiting);
    assert_eq!(app.run_idle(far()), IdleWork::Waiting);
    assert!(!map_changed(&app));
    // Results to collect: ready, and collected.
    hold.run_all();
    assert_eq!(jobs(&app).idle_pending(), IdleWork::Ready);
    let left = app.run_idle(far());
    assert!(map_changed(&app));
    assert_eq!(left, jobs(&app).idle_pending());
    // Settled: done, and the map is not touched.
    idle_gap(&mut app, &hold, 1000);
    for _ in 0..3 {
        app.update();
        idle_gap(&mut app, &hold, 1000);
    }
    assert!(settled(&app));
    app.update();
    assert_eq!(jobs(&app).idle_pending(), IdleWork::Done);
    assert_eq!(app.run_idle(far()), IdleWork::Done);
    assert!(!map_changed(&app));
    // A jump with no interest budget leaves a backlog: ready.
    app.world_mut()
        .resource_mut::<WorldSettings>()
        .interest_budget = Duration::ZERO;
    let mut viewers = app
        .world_mut()
        .query_filtered::<Entity, With<ChunkViewer>>();
    let viewer = viewers.iter(app.world()).next().expect("one viewer");
    move_viewer(&mut app, viewer, ChunkPos::new(40, 0, 0));
    app.update();
    assert!(jobs(&app).interest_backlog() > 0);
    assert_eq!(jobs(&app).idle_pending(), IdleWork::Ready);
    app.run_idle(far());
    assert_eq!(jobs(&app).interest_backlog(), 0, "a step with time goes on");
}

#[test]
fn idle_with_no_time_does_nothing() {
    let settings = WorldSettings {
        interest_budget: Duration::ZERO,
        max_jobs_in_flight: 64,
        ..WorldSettings::default()
    };
    let hold = Arc::new(HoldExecutor::default());
    let mut app = tall_world_app(LayerGen::new(-3..=1), settings, hold.clone());
    spawn_tall_viewer(&mut app, ChunkPos::new(0, 0, 0), 8, 2);
    app.update();
    hold.run_all();
    drain_events(&mut app);
    // Results wait and the interest has a backlog.
    assert_eq!(jobs(&app).idle_pending(), IdleWork::Ready);
    assert!(jobs(&app).interest_backlog() > 0);
    let before = (hold.held(), jobs(&app).interest_steps);
    let past = Instant::now() - Duration::from_millis(1);
    assert_eq!(app.run_idle(past), IdleWork::Ready);
    assert_eq!((hold.held(), jobs(&app).interest_steps), before);
    assert!(drain_events(&mut app).is_empty());
    assert!(!app.world().is_resource_changed::<ChunkMap>());
    // A step that ends right away does at most one round, without interest work.
    app.run_idle(Instant::now() + Duration::from_nanos(1));
    assert!(hold.held() <= before.0 + DISPATCH_CHECK);
    assert!(jobs(&app).interest_steps <= before.1 + MIN_INTEREST_STEPS);
}

/// `ChunkDataEvent::Loaded`s read in `PreUpdate`, per frame.
#[derive(Resource, Default)]
struct Loads(Vec<usize>);

fn count_loads(mut reader: MessageReader<ChunkDataEvent>, mut loads: ResMut<Loads>) {
    let n = reader
        .read()
        .filter(|e| matches!(e, ChunkDataEvent::Loaded { .. }))
        .count();
    loads.0.push(n);
}

#[test]
fn data_events_from_idle_reach_the_next_frame() {
    let run = |idle: bool| {
        let (mut app, hold) = held_world(WorldSettings::default());
        app.init_resource::<Loads>()
            .add_systems(PreUpdate, count_loads);
        // The generations started in the first frame finish; with idle steps they are
        // collected right away, else by the next frame.
        hold.run_all();
        if idle {
            app.run_idle(far());
        }
        app.update();
        app.update();
        let loaded = app.world().resource::<WorldStats>().data_ready;
        (app.world().resource::<Loads>().0.clone(), loaded)
    };
    let (with, loaded) = run(true);
    let (without, same) = run(false);
    assert_eq!(loaded, same);
    // Generated chunks loaded in the step between frames 1 and 2: frame 2's readers get them
    // with the all-air chunks frame 1 created, every chunk.
    assert_eq!(with, vec![loaded, 0], "none lost");
    // Without idle steps frame 2 collects the generated ones, and frame 3 reads them.
    assert!(without[0] < loaded && without[1] > 0, "{without:?}");
    assert_eq!(without[0] + without[1], loaded);
}
