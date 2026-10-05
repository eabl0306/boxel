//! State machine tests: neighbor bookkeeping, stale results, the job contract, event budget and
//! interest transitions (no ECS; fake mesher).
//!
//! Most tests use a fixed-height world (`vertical_bounds` = the generator's range, every layer
//! wanted), as boxel M0 streamed; the `tall_*` tests and the property test cover the unbounded
//! one.
use super::*;
use crate::interest::MAX_RADIUS;
use crate::jobs::InlineExecutor;
use crate::tests::support::{HoldExecutor, LayerGen, fake_mesh, panicking_mesh};
use crate::{BlockEdit, WorldStats};
use boxel_voxel::{BlockPos, CHUNK_VOLUME, neighbor_index};
use boxel_worldgen::WorldGenerator;

const STONE: BlockState = BlockState(1);

/// A viewer chunk and its horizontal radius (every layer of the vertical bounds is wanted).
type ViewerChunk = (ChunkPos, u32);

struct Harness {
    map: ChunkMap,
    jobs: WorldJobs,
    content: WorldContent,
    settings: WorldSettings,
    events: Vec<ChunkMeshEvent>,
}

impl Harness {
    fn new(
        generator: impl WorldGenerator,
        executor: Arc<dyn JobExecutor>,
        max_in_flight: usize,
    ) -> Self {
        let registry = Arc::new(BlockRegistry::default_blocks());
        let settings = WorldSettings {
            vertical_bounds: Some(generator.vertical_chunks()),
            interest_budget: Duration::MAX,
            job_budget: Duration::MAX,
            ..WorldSettings::default()
        };
        let content = WorldContent {
            generator: Arc::new(generator),
            materials: Arc::new(MaterialTable::from_registry(&registry)),
            registry,
        };
        let mut jobs = WorldJobs::new(max_in_flight, 1, executor);
        jobs.mesh_fn = fake_mesh;
        Self {
            map: ChunkMap::default(),
            jobs,
            content,
            settings,
            events: Vec::new(),
        }
    }

    /// The same world without a height limit.
    fn tall(mut self) -> Self {
        self.settings.vertical_bounds = None;
        self
    }

    fn with<R>(&mut self, f: impl FnOnce(&mut WorldJobs, &mut Ctx<'_>, &WorldSettings) -> R) -> R {
        let mut cx = Ctx::new(&mut self.map, &self.content, &mut self.events);
        f(&mut self.jobs, &mut cx, &self.settings)
    }

    /// Viewers with every layer of the bounds (ids by position in the list), applied fully.
    fn interest(&mut self, viewers: &[ViewerChunk]) {
        let viewers: Vec<(u64, Viewer)> = (0u64..)
            .zip(viewers)
            .map(|(id, &(c, r))| (id, Viewer::new(c, r, MAX_RADIUS)))
            .collect();
        self.viewers(&viewers);
    }

    /// Applies `viewers` (sorted by id) fully.
    fn viewers(&mut self, viewers: &[(u64, Viewer)]) {
        self.with(|jobs, cx, settings| jobs.update_interest(cx, settings, viewers));
        assert_eq!(self.jobs.interest_backlog(), 0, "unlimited budget");
        assert_eq!(self.jobs.scheduler.stale(), 0);
    }

    fn dispatch(&mut self) {
        self.with(|jobs, cx, _| jobs.dispatch(cx));
    }

    fn collect(&mut self) {
        self.with(WorldJobs::collect);
    }

    fn step(&mut self) {
        self.dispatch();
        self.collect();
    }

    fn edit(&mut self, pos: BlockPos, state: BlockState) {
        self.with(|jobs, cx, _| jobs.apply_edits(cx, vec![BlockEdit { pos, state }]));
    }

    fn id(&self, pos: ChunkPos) -> ChunkId {
        self.map.id_at(pos).expect("loaded")
    }

    fn slot(&self, pos: ChunkPos) -> &ChunkSlot {
        &self.map.slots[self.id(pos)]
    }

    fn stats(&self) -> WorldStats {
        WorldStats {
            queued_jobs: self.jobs.scheduler.queued(),
            in_flight: self.jobs.scheduler.in_flight(),
            ..self.map.stats()
        }
    }

    fn settle(&mut self, max_steps: usize) {
        for _ in 0..max_steps {
            self.step();
            if self.stats().is_settled() {
                return;
            }
        }
        panic!("not settled: {:?}", self.stats());
    }

    fn take_events(&mut self) -> Vec<ChunkMeshEvent> {
        std::mem::take(&mut self.events)
    }

    /// Published chunks are `Ready`, or have mesh interest and a remesh queued or running; every
    /// `Ready` mesh is published.
    fn check_published(&self) {
        for (id, slot) in self.map.iter() {
            let published = self.jobs.published.contains(&id);
            let remeshing = slot.interest.contains(Interest::MESH)
                && matches!(slot.mesh, MeshState::Queued | MeshState::InFlight { .. });
            match slot.mesh {
                MeshState::Ready { .. } => {
                    assert!(published, "{:?} Ready, not published", slot.key.pos);
                }
                _ => assert!(
                    !published || remeshing,
                    "{:?} published with {:?} and {:?}",
                    slot.key.pos,
                    slot.mesh,
                    slot.interest
                ),
            }
        }
        assert!(
            self.jobs
                .published
                .iter()
                .all(|&id| self.map.get(id).is_some())
        );
    }
}

fn origin(radius: u32) -> [ViewerChunk; 1] {
    [(ChunkPos::new(0, 0, 0), radius)]
}

fn upserted(events: &[ChunkMeshEvent]) -> Vec<ChunkPos> {
    events
        .iter()
        .filter_map(|e| match e {
            ChunkMeshEvent::Upsert { key, .. } => Some(key.pos),
            ChunkMeshEvent::Remove { .. } => None,
        })
        .collect()
}

fn mask(f: impl Fn(i32) -> bool) -> u32 {
    (0..26)
        .filter(|&i| f(NEIGHBOR_OFFSETS[i].y))
        .map(|i| 1 << i)
        .sum()
}

#[test]
fn opposite_neighbor_mirrors_offsets() {
    for (i, offset) in NEIGHBOR_OFFSETS.iter().enumerate() {
        assert_eq!(neighbor_index(-*offset), Some(opposite_neighbor(i)));
    }
}

#[test]
fn neighbor_bits_follow_data_states() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = Harness::new(LayerGen::new(-1..=1), hold.clone(), 64);
    h.interest(&origin(0));
    assert_eq!(h.map.len(), 27);
    // Out-of-range layers count as ready from the start.
    assert_eq!(
        h.slot(ChunkPos::new(0, 1, 0)).neighbors_ready,
        mask(|dy| dy == 1)
    );
    assert_eq!(
        h.slot(ChunkPos::new(0, -1, 0)).neighbors_ready,
        mask(|dy| dy == -1)
    );
    let center = h.id(ChunkPos::new(0, 0, 0));
    assert_eq!(h.map.slots[center].neighbors_ready, 0);

    // Neighbors become ready one by one (as `Generated` does), in offset order.
    let mut expected = 0;
    for (i, offset) in NEIGHBOR_OFFSETS.iter().enumerate() {
        let n = h.id(ChunkPos::from_ivec3(*offset));
        h.map.slots[n].data = DataState::Ready(Arc::new(ChunkData::uniform(STONE)));
        h.with(|jobs, cx, _| jobs.data_available(cx, n));
        expected |= 1 << i;
        assert_eq!(
            h.map.slots[center].neighbors_ready, expected,
            "after neighbor {i}"
        );
        assert_eq!(
            h.map.slots[n].neighbors_ready & (1 << opposite_neighbor(i)),
            0,
            "the center is not ready yet"
        );
    }
    assert_eq!(expected, ALL_NEIGHBORS);
    assert!(matches!(h.map.slots[center].mesh, MeshState::Queued));

    // The center becomes ready: every neighbor gets its bit; the enclosed stone center is then
    // meshed by the fast path at dispatch (no job).
    h.map.slots[center].data = DataState::Ready(Arc::new(ChunkData::uniform(STONE)));
    h.with(|jobs, cx, _| jobs.data_available(cx, center));
    for (i, offset) in NEIGHBOR_OFFSETS.iter().enumerate() {
        let n = h.slot(ChunkPos::from_ivec3(*offset));
        assert_ne!(n.neighbors_ready & (1 << opposite_neighbor(i)), 0);
    }
    h.dispatch();
    // Generation jobs are discarded (data already set); the center is enclosed (fast path, no
    // job); (0, 1, 0) and (0, -1, 0) border the empty out-of-range layers and get mesh jobs.
    assert_eq!(hold.held(), 2);
    assert_eq!(h.jobs.scheduler.in_flight(), 2);
    assert!(matches!(h.map.slots[center].mesh, MeshState::Empty));
    for y in [-1, 1] {
        assert!(matches!(
            h.slot(ChunkPos::new(0, y, 0)).mesh,
            MeshState::InFlight { .. }
        ));
    }

    // Unloading the +X neighbor clears its bit in the center and bumps the center's epoch.
    let east = h.id(ChunkPos::new(1, 0, 0));
    let epoch = h.map.slots[center].epoch;
    h.with(|jobs, cx, _| jobs.unload(cx, east));
    let bit = 1 << neighbor_index(Face::PosX.normal()).expect("face offset");
    assert_eq!(h.map.slots[center].neighbors_ready, ALL_NEIGHBORS & !bit);
    assert_eq!(h.map.slots[center].epoch, epoch + 1);
    assert!(h.map.id_at(ChunkPos::new(1, 0, 0)).is_none());
}

/// Viewer radius 0 over stone (y = -1) and air (y = 0): one exposed mesh chunk at (0, -1, 0).
fn surface_world(executor: Arc<dyn JobExecutor>) -> Harness {
    let mut h = Harness::new(LayerGen::new(-1..=0), executor, 64);
    h.interest(&origin(0));
    h
}

#[test]
fn stale_results_are_rejected_by_version() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = surface_world(hold.clone());
    h.dispatch();
    assert_eq!(hold.held(), 18);
    hold.run_all();
    h.collect();
    let center = ChunkPos::new(0, -1, 0);
    assert!(
        matches!(h.slot(ChunkPos::new(0, 0, 0)).mesh, MeshState::Empty),
        "air fast path"
    );
    h.dispatch();
    assert_eq!(hold.held(), 1, "the surface mesh job");
    assert!(matches!(
        h.slot(center).mesh,
        MeshState::InFlight {
            data_version: 0,
            epoch: 0
        }
    ));

    // An interior edit while the job runs: version 1, remesh pending.
    h.edit(BlockPos::new(10, -10, 10), BlockState::AIR);
    assert!(h.slot(center).remesh_pending);
    assert_eq!(h.stats().meshing, 1);
    hold.run_all();
    h.collect();
    assert!(h.take_events().is_empty(), "version 0 result rejected");
    assert!(matches!(h.slot(center).mesh, MeshState::Queued));
    assert!(!h.slot(center).remesh_pending);
    assert_eq!(h.jobs.scheduler.queued(), 1, "requeued");

    h.dispatch();
    hold.run_all();
    h.collect();
    assert_eq!(upserted(&h.take_events()), vec![center]);
    match &h.slot(center).mesh {
        MeshState::Ready {
            mesh, data_version, ..
        } => assert_eq!((mesh.total_quads(), *data_version), (2, 1)),
        other => panic!("{other:?}"),
    }
    assert!(h.stats().is_settled());
}

#[test]
fn stale_results_are_rejected_by_epoch() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = surface_world(hold.clone());
    h.dispatch();
    hold.run_all();
    h.collect();
    h.dispatch();
    let center = ChunkPos::new(0, -1, 0);
    let id = h.id(center);

    // A neighbor changed under the running job (same center version, new epoch).
    h.map.slots[id].epoch += 1;
    hold.run_all();
    h.collect();
    assert!(h.take_events().is_empty(), "epoch 0 result rejected");
    assert!(matches!(h.slot(center).mesh, MeshState::Queued));
    h.dispatch();
    hold.run_all();
    h.collect();
    assert_eq!(upserted(&h.take_events()), vec![center]);
    assert!(matches!(
        h.slot(center).mesh,
        MeshState::Ready {
            data_version: 0,
            epoch: 1,
            ..
        }
    ));

    // A neighbor unloaded under a running remesh: the result is rejected and the mesh waits for
    // the neighbor (incomplete bits: not queued).
    h.map.slots[id].epoch += 1;
    h.with(|jobs, cx, _| jobs.request_remesh(cx, id, 0));
    h.dispatch();
    assert_eq!(hold.held(), 1);
    let east = h.id(ChunkPos::new(1, -1, 0));
    h.with(|jobs, cx, _| jobs.unload(cx, east));
    hold.run_all();
    h.collect();
    assert!(h.take_events().is_empty());
    assert!(matches!(h.slot(center).mesh, MeshState::Queued));
    assert_eq!(h.jobs.scheduler.queued(), 0);
}

#[test]
fn neighbor_edits_bump_epoch_and_remesh() {
    let mut h = surface_world(Arc::new(InlineExecutor));
    h.settle(20);
    h.take_events();
    let center = ChunkPos::new(0, -1, 0);
    let epoch = h.slot(center).epoch;
    // Local x = 0 of chunk (1, -1, 0) touches its -X face, i.e. the center chunk.
    h.edit(BlockPos::new(32, -5, 7), BlockState::AIR);
    assert_eq!(h.slot(center).epoch, epoch + 1);
    assert_eq!(
        h.map
            .data_at(ChunkPos::new(1, -1, 0))
            .expect("data")
            .version,
        1
    );
    h.settle(20);
    // (1, -1, 0) has no mesh interest (radius 0): only the center is remeshed.
    assert_eq!(upserted(&h.take_events()), vec![center]);
    assert!(matches!(
        h.slot(ChunkPos::new(1, -1, 0)).mesh,
        MeshState::None
    ));
    assert!(matches!(
        h.slot(center).mesh,
        MeshState::Ready { data_version: 0, epoch: e, .. } if e == epoch + 1
    ));
}

#[test]
fn edits_to_chunks_that_are_not_ready_are_dropped() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = surface_world(hold.clone());
    h.edit(BlockPos::new(1, -1, 1), BlockState::AIR);
    assert_eq!(
        h.map.block_at(BlockPos::new(1, -1, 1)),
        None,
        "not generated yet"
    );
    h.dispatch();
    hold.run_all();
    h.collect();
    assert_eq!(h.map.block_at(BlockPos::new(1, -1, 1)), Some(STONE));
    assert_eq!(
        h.map
            .data_at(ChunkPos::new(0, -1, 0))
            .expect("data")
            .version,
        0
    );
}

#[test]
fn mesh_events_respect_the_per_frame_budget() {
    let mut h = Harness::new(LayerGen::new(-1..=0), Arc::new(InlineExecutor), 64);
    h.settings.max_mesh_events_per_frame = 2;
    h.interest(&origin(1));
    let mut per_step = Vec::new();
    let mut waited = false;
    for _ in 0..50 {
        h.step();
        let events = h.take_events();
        assert!(events.len() <= 2, "{} events in one step", events.len());
        per_step.push(events.len());
        if !h.jobs.pending.is_empty() {
            waited = true;
            assert!(
                !h.stats().is_settled(),
                "pending results keep the world busy"
            );
        }
        if h.stats().is_settled() {
            break;
        }
    }
    assert!(waited, "some results had to wait: {per_step:?}");
    assert_eq!(per_step.iter().sum::<usize>(), 5, "5 exposed mesh chunks");
    assert_eq!(h.map.ready_meshes().count(), 5);
}

#[test]
fn generator_panic_releases_its_slot() {
    let hold = Arc::new(HoldExecutor::default());
    let mut generator = LayerGen::new(0..=0);
    generator.panic_at = Some(ChunkPos::new(0, 0, 0));
    let mut h = Harness::new(generator, hold.clone(), 64);
    h.interest(&origin(0));
    h.dispatch();
    assert_eq!(h.jobs.scheduler.in_flight(), 9);
    hold.run_all();
    h.collect();
    assert_eq!(h.jobs.scheduler.in_flight(), 0);
    assert_eq!(h.jobs.failed, 1);
    assert!(matches!(
        h.slot(ChunkPos::new(0, 0, 0)).data,
        DataState::Missing
    ));
    assert!(matches!(
        h.slot(ChunkPos::new(0, 0, 0)).mesh,
        MeshState::Empty
    ));
    assert!(h.stats().is_settled());
}

#[test]
fn cancelled_jobs_release_their_slot() {
    let hold = Arc::new(HoldExecutor::default());
    let mut generator = LayerGen::new(0..=0);
    // Cancellation wins: the panicking key is never generated.
    generator.panic_at = Some(ChunkPos::new(0, 0, 0));
    let mut h = Harness::new(generator, hold.clone(), 64);
    h.interest(&origin(0));
    h.dispatch();
    let old: Vec<ChunkId> = h.map.slots.keys().collect();
    let cancels: Vec<Arc<AtomicBool>> = old
        .iter()
        .map(|&id| Arc::clone(&h.map.slots[id].cancel))
        .collect();
    // Viewer jumps away: the generating slots are unloaded and cancelled.
    h.interest(&[(ChunkPos::new(100, 0, 0), 0)]);
    assert!(cancels.iter().all(|c| c.load(Ordering::Relaxed)));
    assert!(old.iter().all(|&id| h.map.get(id).is_none()));
    hold.run_all();
    h.collect();
    assert_eq!(h.jobs.scheduler.in_flight(), 0);
    assert_eq!(h.jobs.failed, 0, "cancelled, not failed");
    h.dispatch();
    hold.run_all();
    h.collect();
    assert!(h.stats().is_settled());
    assert_eq!(h.map.len(), 9);
}

#[test]
fn dropped_jobs_report_failure() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = Harness::new(LayerGen::new(0..=0), hold.clone(), 64);
    h.interest(&origin(0));
    h.dispatch();
    hold.drop_all();
    h.collect();
    assert_eq!(h.jobs.scheduler.in_flight(), 0);
    assert_eq!(h.jobs.failed, 9);
    assert!(
        h.map
            .iter()
            .all(|(_, s)| matches!(s.data, DataState::Missing))
    );
    assert!(h.stats().is_settled());
}

#[test]
fn mesher_panic_marks_the_mesh_empty() {
    let mut h = surface_world(Arc::new(InlineExecutor));
    h.jobs.mesh_fn = panicking_mesh;
    h.settle(20);
    assert!(matches!(
        h.slot(ChunkPos::new(0, -1, 0)).mesh,
        MeshState::Empty
    ));
    assert_eq!(h.jobs.failed, 1);
    assert_eq!(h.jobs.scheduler.in_flight(), 0);
    assert!(h.take_events().is_empty());
}

#[test]
fn failed_remesh_with_pending_request_retries() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = surface_world(hold.clone());
    h.dispatch();
    hold.run_all();
    h.collect();
    h.dispatch();
    let center = ChunkPos::new(0, -1, 0);
    h.edit(BlockPos::new(10, -10, 10), BlockState::AIR);
    hold.drop_all();
    h.collect();
    assert_eq!(h.jobs.failed, 1);
    assert!(matches!(h.slot(center).mesh, MeshState::Queued), "retried");
    h.dispatch();
    hold.run_all();
    h.collect();
    assert_eq!(upserted(&h.take_events()), vec![center]);
}

#[test]
fn remesh_to_empty_and_unload_remove_published_meshes() {
    let mut h = surface_world(Arc::new(InlineExecutor));
    h.settle(20);
    let center = ChunkPos::new(0, -1, 0);
    let id = h.id(center);
    assert_eq!(upserted(&h.take_events()), vec![center]);
    // Carving the whole chunk to air remeshes to empty: Remove.
    let edits: Vec<BlockEdit> = (0..32 * 32 * 32)
        .map(|i| {
            let l = LocalPos::from_index(i);
            BlockEdit {
                pos: BlockPos::from_parts(center, l),
                state: BlockState::AIR,
            }
        })
        .collect();
    h.with(|jobs, cx, _| jobs.apply_edits(cx, edits));
    h.settle(20);
    let events = h.take_events();
    assert!(
        matches!(events.as_slice(), [ChunkMeshEvent::Remove { id: removed, .. }] if *removed == id),
        "{events:?}"
    );
    assert!(matches!(h.slot(center).mesh, MeshState::Empty));
    // Put one block back, then unload by moving away: Remove again.
    h.edit(BlockPos::new(3, -3, 3), STONE);
    h.settle(20);
    assert_eq!(upserted(&h.take_events()), vec![center]);
    h.interest(&[(ChunkPos::new(50, 0, 0), 0)]);
    let events = h.take_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChunkMeshEvent::Remove { id: removed, .. } if *removed == id)),
        "{events:?}"
    );
    assert!(h.map.get(id).is_none());
}

#[test]
fn interest_changes_drop_unstarted_work() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = Harness::new(LayerGen::new(0..=0), hold.clone(), 1);
    h.interest(&origin(0));
    h.dispatch();
    assert_eq!(hold.held(), 1, "cap 1");
    let first = h.id(ChunkPos::new(0, 0, 0));
    assert!(
        matches!(h.map.slots[first].data, DataState::Generating),
        "priority 0 first"
    );
    assert_eq!(
        h.map.block_at(BlockPos::new(-20, 5, 5)),
        None,
        "queued: no data yet"
    );

    // One chunk east: column x = -1 is unwanted; its data was never started, so it unloads at
    // once (inside the hysteresis band). The old center loses its mesh interest.
    h.interest(&[(ChunkPos::new(1, 0, 0), 0)]);
    for z in -1..=1 {
        assert!(h.map.id_at(ChunkPos::new(-1, 0, z)).is_none());
    }
    assert_eq!(h.map.len(), 9);
    assert!(matches!(h.map.slots[first].mesh, MeshState::None));
    assert_eq!(h.map.slots[first].interest, Interest::DATA);
    assert!(matches!(
        h.slot(ChunkPos::new(1, 0, 0)).mesh,
        MeshState::Queued
    ));
    // Queued generation jobs: the 8 slots that are still `Queued` (stale ones were dropped).
    assert_eq!(h.jobs.scheduler.queued(), 8);

    for _ in 0..20 {
        h.dispatch();
        hold.run_all();
        h.collect();
    }
    assert!(h.stats().is_settled());
    // Air everywhere: the one mesh chunk (radius 0) is the uniform-air fast path.
    assert_eq!(h.stats().mesh_empty, 1);
}

fn removed(events: &[ChunkMeshEvent]) -> Vec<ChunkId> {
    events
        .iter()
        .filter_map(|e| match e {
            ChunkMeshEvent::Remove { id, .. } => Some(*id),
            ChunkMeshEvent::Upsert { .. } => None,
        })
        .collect()
}

#[test]
fn losing_mesh_interest_mid_remesh_removes_the_old_mesh() {
    let mut h = Harness::new(LayerGen::new(-1..=0), Arc::new(InlineExecutor), 64);
    let away = [(ChunkPos::new(-3, 0, 0), 1)];
    h.interest(&origin(1));
    h.settle(50);
    let a = h.id(ChunkPos::new(1, -1, 0));
    assert!(matches!(h.map.slots[a].mesh, MeshState::Ready { .. }));
    // A stays in the hysteresis band with its mesh; its neighbor (2, -1, 0) unloads.
    h.interest(&away);
    h.settle(50);
    assert!(h.map.id_at(ChunkPos::new(2, -1, 0)).is_none());
    assert!(matches!(h.map.slots[a].mesh, MeshState::Ready { .. }));
    h.check_published();
    h.take_events();

    // Back: A is wanted again with a stale epoch, so its remesh waits for the neighbor. Away
    // again before that: the old mesh is removed at once.
    h.interest(&origin(1));
    assert!(matches!(h.map.slots[a].mesh, MeshState::Queued));
    h.check_published();
    let mut remeshing: Vec<ChunkId> = h
        .map
        .iter()
        .filter(|(id, s)| matches!(s.mesh, MeshState::Queued) && h.jobs.published.contains(id))
        .map(|(id, _)| id)
        .collect();
    assert!(remeshing.contains(&a));
    h.interest(&away);
    assert!(matches!(h.map.slots[a].mesh, MeshState::None));
    let mut events = removed(&h.take_events());
    events.sort_unstable();
    remeshing.sort_unstable();
    assert_eq!(events, remeshing);
    h.check_published();
    h.settle(50);

    // Wanted again: rebuilt and published.
    h.interest(&origin(1));
    h.settle(50);
    assert!(matches!(h.map.slots[a].mesh, MeshState::Ready { .. }));
    assert!(upserted(&h.take_events()).contains(&ChunkPos::new(1, -1, 0)));
    h.check_published();
}

#[test]
fn losing_mesh_interest_while_in_flight_removes_the_old_mesh() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = surface_world(hold.clone());
    let run = |h: &mut Harness| {
        for _ in 0..10 {
            h.dispatch();
            hold.run_all();
            h.collect();
        }
        assert!(h.stats().is_settled());
    };
    run(&mut h);
    let center = ChunkPos::new(0, -1, 0);
    let id = h.id(center);
    assert_eq!(upserted(&h.take_events()), vec![center]);

    // An edit starts a remesh; the viewer leaves while it runs (the center keeps only data).
    h.edit(BlockPos::new(10, -10, 10), BlockState::AIR);
    h.dispatch();
    assert!(matches!(h.slot(center).mesh, MeshState::InFlight { .. }));
    h.interest(&[(ChunkPos::new(1, 0, 0), 0)]);
    assert_eq!(h.slot(center).interest, Interest::DATA);
    assert!(matches!(h.slot(center).mesh, MeshState::InFlight { .. }));
    assert_eq!(removed(&h.take_events()), vec![id]);
    h.check_published();
    run(&mut h);
    assert!(matches!(h.slot(center).mesh, MeshState::None));
    let late = h.take_events().into_iter().find(|e| match e {
        ChunkMeshEvent::Upsert { id: other, .. } | ChunkMeshEvent::Remove { id: other, .. } => {
            *other == id
        }
    });
    assert!(late.is_none(), "the late result is dropped: {late:?}");

    // Back: rebuilt from the edited data.
    h.interest(&origin(0));
    run(&mut h);
    assert_eq!(upserted(&h.take_events()), vec![center]);
    assert!(matches!(
        h.slot(center).mesh,
        MeshState::Ready {
            data_version: 1,
            ..
        }
    ));
    h.check_published();
}

/// Stone, except the air column x = 2, z = 0.
struct HoleGen;

impl WorldGenerator for HoleGen {
    fn generate(&self, key: ChunkKey, _registry: &BlockRegistry) -> ChunkData {
        if (key.pos.x, key.pos.z) == (2, 0) {
            ChunkData::uniform(BlockState::AIR)
        } else {
            ChunkData::uniform(STONE)
        }
    }
    fn vertical_chunks(&self) -> RangeInclusive<i32> {
        -1..=1
    }
    fn name(&self) -> &str {
        "hole"
    }
}

/// Empty when the center and the 6 neighbor slabs that touch it are all solid (an absent
/// neighbor is air), like a real mesher; otherwise `fake_mesh`.
fn hidden_mesh(input: &MeshInput<'_>, materials: &MaterialTable) -> ChunkMesh {
    let all_solid = |d: &ChunkData, border: Option<Face>| {
        (0..CHUNK_VOLUME).map(LocalPos::from_index).all(|p| {
            border.is_some_and(|f| p.border_faces() & (1 << f.index()) == 0) || !d.get(p).is_air()
        })
    };
    let hidden = all_solid(input.center, None)
        && Face::ALL.iter().all(|&face| {
            let i = neighbor_index(face.normal()).expect("face offset");
            input.neighbors[i].is_some_and(|d| all_solid(d, Some(face.opposite())))
        });
    if hidden {
        ChunkMesh::default()
    } else {
        fake_mesh(input, materials)
    }
}

#[test]
fn empty_mesh_is_redone_after_a_neighbor_reloads() {
    let mut h = Harness::new(HoleGen, Arc::new(InlineExecutor), 64);
    h.jobs.mesh_fn = hidden_mesh;
    h.interest(&origin(1));
    h.settle(50);
    let a = h.id(ChunkPos::new(1, 0, 0));
    assert!(matches!(h.map.slots[a].mesh, MeshState::Ready { .. }));

    // Filling the slab of the air chunk B = (2, 0, 0) that touches A hides A: Empty.
    let fill: Vec<BlockEdit> = (0..32 * 32)
        .map(|i| BlockEdit {
            pos: BlockPos::new(64, i / 32, i % 32),
            state: STONE,
        })
        .collect();
    h.with(|jobs, cx, _| jobs.apply_edits(cx, fill));
    h.settle(50);
    assert!(matches!(h.map.slots[a].mesh, MeshState::Empty));
    assert_eq!(removed(&h.take_events()), vec![a]);

    // Away: A stays in the band, B unloads and its edit is lost.
    h.interest(&[(ChunkPos::new(-3, 0, 0), 1)]);
    h.settle(50);
    assert_eq!(h.map.id_at(ChunkPos::new(1, 0, 0)), Some(a));
    assert!(h.map.id_at(ChunkPos::new(2, 0, 0)).is_none());

    // Back: B is generated as air again, so A has an exposed face again.
    h.interest(&origin(1));
    h.settle(50);
    assert_eq!(
        h.map
            .data_at(ChunkPos::new(2, 0, 0))
            .map(|d| d.as_uniform()),
        Some(Some(BlockState::AIR))
    );
    assert!(
        matches!(h.map.slots[a].mesh, MeshState::Ready { .. }),
        "{:?}",
        h.map.slots[a].mesh
    );
    assert!(upserted(&h.take_events()).contains(&ChunkPos::new(1, 0, 0)));
    h.check_published();
}

#[test]
fn auto_cap_keeps_workers_busy_for_one_and_a_half_frames() {
    // 144 Hz, 60 us jobs, 6 workers: 6 x ceil(1.5 x 6.94 ms / 60 us) = 6 x 174.
    assert_eq!(auto_cap(6, 1.0 / 144.0, 60e-6), 6 * 174);
    // An uncapped loop (4000 updates/s).
    assert_eq!(auto_cap(6, 0.25e-3, 60e-6), 6 * 7);
    // At least 2 per worker, at most MAX_AUTO_IN_FLIGHT.
    assert_eq!(auto_cap(4, 1e-6, 1e-3), 8);
    assert_eq!(auto_cap(8, 1.0, 1e-6), MAX_AUTO_IN_FLIGHT);
    assert_eq!(auto_cap(0, 0.0, 0.0), 2);

    // The averages follow the measurements.
    let mut cap = AutoCap::new(4);
    let t0 = Instant::now();
    for i in 0..64 {
        cap.observe_frame(t0 + Duration::from_millis(10 * i));
        cap.observe_job(Duration::from_micros(100));
    }
    assert!((cap.frame - 0.010).abs() < 1e-4, "{}", cap.frame);
    assert!((cap.job - 100e-6).abs() < 20e-6, "{}", cap.job);
    assert_eq!(cap.cap(), auto_cap(4, cap.frame, cap.job));
    // A hitch counts as 33 ms at most.
    cap.observe_frame(t0 + Duration::from_secs(60));
    assert!(cap.frame < 0.0135, "{}", cap.frame);
}

#[test]
fn the_cap_follows_the_settings_every_frame() {
    let mut h = surface_world(Arc::new(InlineExecutor));
    h.settings.max_jobs_in_flight = 5;
    h.jobs.begin_frame(&h.settings, Instant::now());
    assert_eq!(h.jobs.scheduler.max_in_flight(), 5);
    h.settings.max_jobs_in_flight = 0;
    h.jobs.begin_frame(&h.settings, Instant::now());
    assert_eq!(h.jobs.scheduler.max_in_flight(), h.jobs.auto_cap.cap());
}

mod tall;
