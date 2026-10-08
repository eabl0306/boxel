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
use boxel_voxel::{BlockPos, CHUNK_VOLUME, ChunkKey, LightValue, neighbor_index};
use boxel_worldgen::WorldGenerator;
use std::collections::HashMap;

const STONE: BlockState = BlockState(1);

/// A viewer chunk and its horizontal radius (every layer of the vertical bounds is wanted).
type ViewerChunk = (ChunkPos, u32);

struct Harness {
    map: ChunkMap,
    sky: SkyMap,
    jobs: WorldJobs,
    content: WorldContent,
    settings: WorldSettings,
    events: Vec<ChunkMeshEvent>,
    /// How far the world's frame time moves on every [`Harness::collect`] (each one is a frame).
    frame: Duration,
}

/// Default [`Harness::frame`]: 8 ms, so the default edit batch hold (16 ms) ends after the 2
/// frames of its floor.
const HARNESS_FRAME: Duration = Duration::from_millis(8);

impl Harness {
    /// A world without light (these tests check the streaming rules; [`Harness::lit`] turns it
    /// on).
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
            light: false,
            ..WorldSettings::default()
        };
        let content = WorldContent::new(Arc::new(generator), registry);
        let mut jobs = WorldJobs::new(max_in_flight, 1, executor);
        jobs.mesh_fn = fake_mesh;
        Self {
            map: ChunkMap::default(),
            sky: SkyMap::new(SkyRule::Off),
            jobs,
            content,
            settings,
            events: Vec::new(),
            frame: HARNESS_FRAME,
        }
    }

    /// The same world without a height limit.
    fn tall(mut self) -> Self {
        self.settings.vertical_bounds = None;
        self
    }

    /// The same world with light (before anything is loaded).
    fn lit(mut self) -> Self {
        assert!(
            self.map.is_empty(),
            "light is chosen before streaming starts"
        );
        self.settings.light = true;
        self.sky = SkyMap::new(SkyRule::new(
            true,
            self.content.generator.vertical_chunks(),
            self.settings.sky_walk_limit,
        ));
        self
    }

    fn with<R>(&mut self, f: impl FnOnce(&mut WorldJobs, &mut Ctx<'_>, &WorldSettings) -> R) -> R {
        let mut cx = Ctx::new(
            &mut self.map,
            &mut self.sky,
            &self.content,
            &mut self.events,
        );
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
        self.with(|jobs, cx, settings| {
            jobs.update_interest(cx, settings, viewers, settings.interest_budget);
        });
        assert_eq!(self.jobs.interest_backlog(), 0, "unlimited budget");
        assert_eq!(self.jobs.scheduler.stale(), 0);
    }

    fn dispatch(&mut self) {
        self.with(|jobs, cx, _| jobs.dispatch(cx));
    }

    /// A frame's collect step, [`Harness::frame`] after the last one: the edit batch hold runs
    /// on that clock as in the world plugin (the in-flight cap stays the harness's).
    fn collect(&mut self) {
        self.jobs.hold = self.settings.edit_batch_hold;
        self.jobs.now += self.frame;
        self.with(WorldJobs::collect);
    }

    fn step(&mut self) {
        self.dispatch();
        self.collect();
    }

    /// One edit, checked as in [`Harness::edits`].
    fn edit(&mut self, pos: BlockPos, state: BlockState) -> Vec<BlockChanged> {
        self.edits(vec![BlockEdit { pos, state }])
    }

    /// Applies `edits` in one call and checks the `BlockChanged` it returns against a diff of
    /// the map: per position edited, in the order of its first edit, the block before and after
    /// the call, for the positions whose chunk was `Ready` and whose block changed. Those hold
    /// their last edit; the others (no slot, `Queued`, `Generating`, `Missing`) are not touched.
    fn edits(&mut self, edits: Vec<BlockEdit>) -> Vec<BlockChanged> {
        let ready = |map: &ChunkMap, pos: BlockPos| {
            let (chunk, local) = pos.split();
            map.data_at(chunk).map(|data| data.get(local))
        };
        // Per position: the block before (if its chunk is `Ready`) and its last edit.
        let mut order: Vec<(BlockPos, Option<BlockState>, BlockState)> = Vec::new();
        let mut index: HashMap<BlockPos, usize> = HashMap::new();
        for edit in &edits {
            match index.get(&edit.pos) {
                Some(&i) => order[i].2 = edit.state,
                None => {
                    index.insert(edit.pos, order.len());
                    order.push((edit.pos, ready(&self.map, edit.pos), edit.state));
                }
            }
        }
        let changed = self.with(|jobs, cx, _| jobs.apply_edits(cx, edits));
        let mut expected = Vec::new();
        for (pos, before, last) in order {
            let after = ready(&self.map, pos);
            let Some(old) = before else {
                assert_eq!(after, None, "{pos:?}: a dropped edit");
                continue;
            };
            assert_eq!(after, Some(last), "{pos:?} holds its last edit");
            if old != last {
                expected.push(BlockChanged {
                    pos,
                    old,
                    new: last,
                });
            }
        }
        assert_eq!(changed, expected);
        changed
    }

    fn id(&self, pos: ChunkPos) -> ChunkId {
        self.map.id_at(pos).expect("loaded")
    }

    fn slot(&self, pos: ChunkPos) -> &ChunkSlot {
        &self.map.slots[self.id(pos)]
    }

    /// The slot counters (checked against a walk of every slot) and the scheduler counts.
    fn stats(&self) -> WorldStats {
        self.check_stats();
        WorldStats {
            queued_jobs: self.jobs.scheduler.queued(),
            in_flight: self.jobs.scheduler.in_flight(),
            ..self.map.stats()
        }
    }

    /// The incremental slot counters match a walk of every slot.
    fn check_stats(&self) {
        assert_eq!(
            self.map.stats(),
            self.map.recount(),
            "incremental slot counts"
        );
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
            let published = self.jobs.published.get(&id);
            if let (Some(published), MeshState::Ready { mesh, .. }) = (published, slot.mesh()) {
                assert!(
                    Arc::ptr_eq(published, mesh),
                    "{:?}: published mesh",
                    slot.pos
                );
            }
            let published = published.is_some();
            let remeshing = slot.interest.contains(Interest::MESH)
                && matches!(slot.mesh(), MeshState::Queued | MeshState::InFlight { .. });
            match slot.mesh() {
                MeshState::Ready { .. } => {
                    assert!(published, "{:?} Ready, not published", slot.pos);
                }
                _ => assert!(
                    !published || remeshing,
                    "{:?} published with {:?} and {:?}",
                    slot.pos,
                    slot.mesh(),
                    slot.interest
                ),
            }
        }
        assert!(
            self.jobs
                .published
                .keys()
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
    // Out-of-range layers count as ready (a slot gets its bits when its data arrives).
    let planner = &h.jobs.planner;
    assert_eq!(
        own_neighbor_bits(&h.map, planner, ChunkPos::new(0, 1, 0)),
        mask(|dy| dy == 1)
    );
    assert_eq!(
        own_neighbor_bits(&h.map, planner, ChunkPos::new(0, -1, 0)),
        mask(|dy| dy == -1)
    );
    let center = h.id(ChunkPos::new(0, 0, 0));
    assert_eq!(h.map.slots[center].neighbors_ready, 0);

    // Neighbors become ready one by one (as `Generated` does), in offset order.
    let mut expected = 0;
    for (i, offset) in NEIGHBOR_OFFSETS.iter().enumerate() {
        let n = h.id(ChunkPos::from_ivec3(*offset));
        h.map
            .set_data(n, DataState::Ready(Arc::new(ChunkData::uniform(STONE))));
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
    assert!(matches!(h.map.slots[center].mesh(), MeshState::Queued));

    // The center becomes ready: every neighbor gets its bit; the enclosed stone center is then
    // meshed by the fast path at dispatch (no job).
    h.map.set_data(
        center,
        DataState::Ready(Arc::new(ChunkData::uniform(STONE))),
    );
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
    assert!(matches!(h.map.slots[center].mesh(), MeshState::Empty));
    for y in [-1, 1] {
        assert!(matches!(
            h.slot(ChunkPos::new(0, y, 0)).mesh(),
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
        matches!(h.slot(ChunkPos::new(0, 0, 0)).mesh(), MeshState::Empty),
        "air fast path"
    );
    h.dispatch();
    assert_eq!(hold.held(), 1, "the surface mesh job");
    assert!(matches!(
        h.slot(center).mesh(),
        MeshState::InFlight {
            data_version: 0,
            epoch: 0
        }
    ));

    // An interior edit while the job runs: version 1, remesh pending.
    h.edit(BlockPos::new(10, -10, 10), BlockState::AIR);
    assert!(h.slot(center).remesh_pending());
    assert_eq!(h.stats().meshing, 1);
    hold.run_all();
    h.collect();
    assert!(h.take_events().is_empty(), "version 0 result rejected");
    assert!(matches!(h.slot(center).mesh(), MeshState::Queued));
    assert!(!h.slot(center).remesh_pending());
    assert_eq!(h.jobs.scheduler.queued(), 1, "requeued");

    h.dispatch();
    hold.run_all();
    h.collect();
    assert_eq!(upserted(&h.take_events()), vec![center]);
    match h.slot(center).mesh() {
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
    assert!(matches!(h.slot(center).mesh(), MeshState::Queued));
    h.dispatch();
    hold.run_all();
    h.collect();
    assert_eq!(upserted(&h.take_events()), vec![center]);
    assert!(matches!(
        h.slot(center).mesh(),
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
    assert!(matches!(h.slot(center).mesh(), MeshState::Queued));
    assert_eq!(h.jobs.scheduler.queued(), 0);
}

#[test]
fn padded_neighbors_are_the_chunks_whose_mesh_reads_the_voxel() {
    let offsets = |p: LocalPos| -> Vec<(i32, i32, i32)> {
        NEIGHBOR_OFFSETS
            .iter()
            .enumerate()
            .filter(|(i, _)| padded_neighbors(p) & (1 << i) != 0)
            .map(|(_, o)| (o.x, o.y, o.z))
            .collect()
    };
    // `box_chunks(p, 0)`, used by the edits, is the chunk and these neighbors.
    for p in [
        LocalPos::new(5, 1, 30),
        LocalPos::new(5, 31, 7),
        LocalPos::new(31, 5, 0),
        LocalPos::new(0, 0, 0),
        LocalPos::new(31, 31, 31),
    ] {
        let chunk = ChunkPos::new(-3, 7, 2);
        let mut got = BTreeSet::new();
        box_chunks(BlockPos::from_parts(chunk, p), 0, &mut got);
        let mut want: BTreeSet<ChunkPos> = offsets(p)
            .into_iter()
            .map(|(x, y, z)| chunk.offset(boxel_math::IVec3::new(x, y, z)))
            .collect();
        want.insert(chunk);
        assert_eq!(got, want, "{p:?}");
    }
    assert!(offsets(LocalPos::new(5, 1, 30)).is_empty());
    assert_eq!(offsets(LocalPos::new(5, 31, 7)), [(0, 1, 0)]);
    // An edge voxel: two face neighbors and the edge neighbor between them.
    let mut edge = offsets(LocalPos::new(31, 5, 0));
    edge.sort_unstable();
    assert_eq!(edge, [(0, 0, -1), (1, 0, -1), (1, 0, 0)]);
    // A corner voxel: 7 neighbors, each offset in -1..=0 per axis.
    let corner = offsets(LocalPos::new(0, 0, 0));
    assert_eq!(corner.len(), 7);
    assert!(corner.iter().all(|&(x, y, z)| x <= 0 && y <= 0 && z <= 0));
    // Brute force: neighbor `o` reads p when p lies in its padded box (-1..=32 in its frame).
    for i in 0..32 * 32 * 32 {
        let p = LocalPos::from_index(i);
        for (bit, o) in NEIGHBOR_OFFSETS.iter().enumerate() {
            let inside = |c: u32, d: i32| (-1..=32).contains(&(c as i32 - 32 * d));
            let reads = inside(p.x(), o.x) && inside(p.y(), o.y) && inside(p.z(), o.z);
            assert_eq!(padded_neighbors(p) & (1 << bit) != 0, reads, "{p:?} {o:?}");
        }
    }
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
    // (1, -1, 0) has no mesh interest (radius 0): only the center is remeshed, and its mesh
    // (the fake mesher's, from its own unchanged data) is the same: nothing is sent again.
    assert!(h.take_events().is_empty());
    assert!(matches!(
        h.slot(ChunkPos::new(1, -1, 0)).mesh(),
        MeshState::None
    ));
    assert!(matches!(
        h.slot(center).mesh(),
        MeshState::Ready { data_version: 0, epoch: e, .. } if *e == epoch + 1
    ));
}

#[test]
fn edits_to_chunks_that_are_not_ready_are_dropped() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = surface_world(hold.clone());
    let pos = BlockPos::new(1, -1, 1);
    let chunk = ChunkPos::new(0, -1, 0);
    // The slot exists but its generation has not started: dropped, and no message.
    assert!(matches!(h.slot(chunk).data(), DataState::Queued));
    assert_eq!(h.edit(pos, BlockState::AIR), []);
    assert_eq!(h.map.block_at(pos), None, "not generated yet");
    // Generating: the same.
    h.dispatch();
    assert!(matches!(h.slot(chunk).data(), DataState::Generating));
    assert_eq!(h.edit(pos, BlockState::AIR), []);
    hold.run_all();
    h.collect();
    assert_eq!(h.map.block_at(pos), Some(STONE));
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
        h.slot(ChunkPos::new(0, 0, 0)).data(),
        DataState::Missing
    ));
    assert!(matches!(
        h.slot(ChunkPos::new(0, 0, 0)).mesh(),
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
            .all(|(_, s)| matches!(s.data(), DataState::Missing))
    );
    assert!(h.stats().is_settled());
}

#[test]
fn mesher_panic_marks_the_mesh_empty() {
    let mut h = surface_world(Arc::new(InlineExecutor));
    h.jobs.mesh_fn = panicking_mesh;
    h.settle(20);
    assert!(matches!(
        h.slot(ChunkPos::new(0, -1, 0)).mesh(),
        MeshState::Empty
    ));
    assert_eq!(h.jobs.failed, 1);
    assert_eq!(h.jobs.scheduler.in_flight(), 0);
    assert!(h.take_events().is_empty());
}

/// A mesh job that fails leaves an `Empty` mesh. The chunk's light from before the edit stays
/// for queries but is not current (an edit next to it would size its remesh from it), and a
/// light job brings it up to date.
#[test]
fn a_failed_remesh_does_not_leave_old_light_current() {
    let mut h = Harness::new(LayerGen::new(-3..=1), Arc::new(InlineExecutor), 256).lit();
    h.interest(&origin(1));
    h.settle(40);
    let chunk = ChunkPos::new(0, -1, 0);
    assert!(h.slot(chunk).current_light().is_some());
    let dug = BlockPos::new(5, -1, 6);
    assert_eq!(h.map.light_at(dug), Some(LightValue::DARK), "stone");
    h.jobs.mesh_fn = panicking_mesh;
    h.edit(dug, BlockState::AIR);
    h.step();
    let slot = h.slot(chunk);
    assert!(matches!(slot.mesh(), MeshState::Empty));
    assert!(slot.light().is_some(), "the old light stays for queries");
    assert!(slot.current_light().is_none(), "but it is not current");
    h.jobs.mesh_fn = fake_mesh;
    h.settle(20);
    assert!(h.slot(chunk).current_light().is_some(), "a light job");
    assert_eq!(h.map.light_at(dug), Some(LightValue::FULL_SKY), "dug open");
}

/// Remeshes of an edit batch, and a walk an edit sent back, keep priority 0 when a viewer move
/// refreshes the queued priorities.
#[test]
fn edit_jobs_stay_first_when_priorities_are_refreshed() {
    let mut h = Harness::new(LayerGen::new(-3..=1), Arc::new(InlineExecutor), 256).lit();
    h.interest(&origin(2));
    h.settle(40);
    // Not dispatched, so the remeshes stay queued: a hole deep in the stone at a chunk border
    // remeshes the two chunks around it (no light or sky change).
    h.edit(BlockPos::new(31, -40, 5), BlockState::AIR);
    let queued: Vec<ChunkId> = h
        .map
        .iter()
        .filter(|(_, s)| matches!(s.mesh(), MeshState::Queued))
        .map(|(id, _)| id)
        .collect();
    assert!(queued.len() >= 2, "{queued:?}");
    h.interest(&[(ChunkPos::new(2, 0, 0), 2)]);
    for &id in &queued {
        let slot = &h.map.slots[id];
        assert!(slot.batch.is_some());
        let priority = current_priority(
            &h.map,
            &h.sky,
            &h.jobs.planner,
            JobId::Chunk(id),
            JobKind::Mesh,
        );
        assert_eq!(priority, Some(0), "{:?}", slot.pos);
    }
    // A column an edit sent back to a walk.
    let col = h.sky.id_at(0, 0).expect("column");
    let c = &mut h.sky.columns[col];
    c.queued = true;
    c.batch = Some(0);
    let priority = |h: &Harness| {
        current_priority(
            &h.map,
            &h.sky,
            &h.jobs.planner,
            JobId::Column(col),
            JobKind::SkyTile,
        )
    };
    assert_eq!(priority(&h), Some(0));
    h.sky.columns[col].batch = None;
    assert_eq!(priority(&h), Some(4), "its distance");
    h.sky.columns[col].queued = false;
    h.settle(40);
    assert!(h.map.iter().all(|(_, s)| s.batch.is_none()));
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
    assert!(
        matches!(h.slot(center).mesh(), MeshState::Queued),
        "retried"
    );
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
    h.edits(edits);
    h.settle(20);
    let events = h.take_events();
    assert!(
        matches!(events.as_slice(), [ChunkMeshEvent::Remove { id: removed, .. }] if *removed == id),
        "{events:?}"
    );
    assert!(matches!(h.slot(center).mesh(), MeshState::Empty));
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
        matches!(h.map.slots[first].data(), DataState::Generating),
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
    assert!(matches!(h.map.slots[first].mesh(), MeshState::None));
    assert_eq!(h.map.slots[first].interest, Interest::DATA);
    assert!(matches!(
        h.slot(ChunkPos::new(1, 0, 0)).mesh(),
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

/// Every slot sits at its own position in the index: no second slot was created at an occupied
/// position (it would be left out of the index, with its own job and counts).
fn check_one_slot_per_position(h: &Harness) {
    for (id, slot) in h.map.iter() {
        assert_eq!(
            h.map.id_at(slot.pos),
            Some(id),
            "{:?} has two slots",
            slot.pos
        );
    }
    h.check_stats();
}

/// Two viewers that want the same 27 chunks list the same positions one after the other, so the
/// first batch of interest lookups holds all of the first viewer's positions and the start of
/// the second one's, each after the step that created (later: unloaded) its slot. Those are
/// looked up again: no position gets a second slot and no step reads a removed one. Release
/// builds rely on this alone (the stale lookup checks are `debug_assert`s).
#[test]
fn a_position_listed_twice_in_one_batch_is_looked_up_again() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = Harness::new(LayerGen::new(-1..=1), hold.clone(), 64);
    // Keep regions are the data regions: both viewers list the same 27 positions when they leave.
    h.settings.unload_hysteresis = 0;
    let center = ChunkPos::new(0, 0, 0);
    // Radius 0 and 3 layers: 3 x 3 columns x 3 layers each.
    h.interest(&[(center, 0), (center, 0)]);
    assert!(
        h.jobs.relookups > 0,
        "no position was listed twice in a batch"
    );
    assert_eq!(h.map.len(), 27);
    check_one_slot_per_position(&h);
    assert_eq!(h.jobs.scheduler.queued(), 27, "one generation job per slot");

    let created = h.jobs.relookups;
    h.interest(&[]);
    assert!(
        h.jobs.relookups > created,
        "no unloaded position listed again"
    );
    assert!(h.map.is_empty());
    check_one_slot_per_position(&h);
    assert_eq!(h.jobs.scheduler.queued(), 0);
    assert_eq!(hold.held(), 0);
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

/// A renderer that resyncs (lost uploads, a new device or tier) drops every mesh and uploads
/// [`ChunkMap::ready_meshes`] again, which misses a chunk being remeshed. Its next mesh must
/// reach the renderer even when it is identical to the one published before.
#[test]
fn a_renderer_resync_gets_every_published_mesh_back() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = surface_world(hold.clone());
    let mut renderer: HashMap<ChunkId, Arc<ChunkMesh>> = HashMap::new();
    let apply = |renderer: &mut HashMap<ChunkId, Arc<ChunkMesh>>, events: Vec<ChunkMeshEvent>| {
        for event in events {
            match event {
                ChunkMeshEvent::Upsert { id, mesh, .. } => {
                    renderer.insert(id, mesh);
                }
                ChunkMeshEvent::Remove { id, .. } => {
                    renderer.remove(&id);
                }
            }
        }
    };
    for _ in 0..50 {
        h.dispatch();
        hold.run_all();
        h.collect();
        if h.stats().is_settled() {
            break;
        }
    }
    assert!(h.stats().is_settled());
    let events = h.take_events();
    apply(&mut renderer, events);
    let center = h.id(ChunkPos::new(0, -1, 0));
    assert!(renderer.contains_key(&center));
    // A remesh that will give the same mesh (nothing changed), running during the resync.
    h.with(|jobs, cx, _| jobs.request_remesh(cx, center, 0));
    h.dispatch();
    assert_eq!(hold.held(), 1);
    renderer = h
        .map
        .ready_meshes()
        .map(|(id, _, mesh)| (id, Arc::clone(mesh)))
        .collect();
    assert!(!renderer.contains_key(&center));
    h.jobs.renderer_resynced(&h.map);
    h.check_published();
    hold.run_all();
    h.collect();
    let events = h.take_events();
    apply(&mut renderer, events);
    h.check_published();
    // The renderer holds every published mesh again.
    for (id, slot) in h.map.iter() {
        if let MeshState::Ready { mesh, .. } = slot.mesh() {
            assert!(
                renderer.get(&id).is_some_and(|m| Arc::ptr_eq(m, mesh)),
                "{:?}",
                slot.pos
            );
        }
    }
    assert_eq!(renderer.len(), h.jobs.published.len());
}

#[test]
fn losing_mesh_interest_mid_remesh_removes_the_old_mesh() {
    let mut h = Harness::new(LayerGen::new(-1..=0), Arc::new(InlineExecutor), 64);
    let away = [(ChunkPos::new(-3, 0, 0), 1)];
    h.interest(&origin(1));
    h.settle(50);
    let a = h.id(ChunkPos::new(1, -1, 0));
    assert!(matches!(h.map.slots[a].mesh(), MeshState::Ready { .. }));
    // A stays in the hysteresis band with its mesh; its neighbor (2, -1, 0) unloads.
    h.interest(&away);
    h.settle(50);
    assert!(h.map.id_at(ChunkPos::new(2, -1, 0)).is_none());
    assert!(matches!(h.map.slots[a].mesh(), MeshState::Ready { .. }));
    h.check_published();
    h.take_events();

    // Back: A is wanted again with a stale epoch, so its remesh waits for the neighbor. Away
    // again before that: the old mesh is removed at once.
    h.interest(&origin(1));
    assert!(matches!(h.map.slots[a].mesh(), MeshState::Queued));
    h.check_published();
    let mut remeshing: Vec<ChunkId> = h
        .map
        .iter()
        .filter(|(id, s)| {
            matches!(s.mesh(), MeshState::Queued) && h.jobs.published.contains_key(id)
        })
        .map(|(id, _)| id)
        .collect();
    assert!(remeshing.contains(&a));
    h.interest(&away);
    assert!(matches!(h.map.slots[a].mesh(), MeshState::None));
    let mut events = removed(&h.take_events());
    events.sort_unstable();
    remeshing.sort_unstable();
    assert_eq!(events, remeshing);
    h.check_published();
    h.settle(50);

    // Wanted again: rebuilt and published.
    h.interest(&origin(1));
    h.settle(50);
    assert!(matches!(h.map.slots[a].mesh(), MeshState::Ready { .. }));
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
    assert!(matches!(h.slot(center).mesh(), MeshState::InFlight { .. }));
    h.interest(&[(ChunkPos::new(1, 0, 0), 0)]);
    assert_eq!(h.slot(center).interest, Interest::DATA);
    assert!(matches!(h.slot(center).mesh(), MeshState::InFlight { .. }));
    assert_eq!(removed(&h.take_events()), vec![id]);
    h.check_published();
    run(&mut h);
    assert!(matches!(h.slot(center).mesh(), MeshState::None));
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
        h.slot(center).mesh(),
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
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        -1..=1
    }
    fn name(&self) -> &str {
        "hole"
    }
}

/// Empty when the center and the 6 neighbor slabs that touch it are all solid (an absent
/// neighbor is air), like a real mesher; otherwise `fake_mesh`.
fn hidden_mesh(
    input: &MeshInput<'_>,
    materials: &MaterialTable,
    options: MeshOptions,
) -> ChunkMesh {
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
        fake_mesh(input, materials, options)
    }
}

#[test]
fn empty_mesh_is_redone_after_a_neighbor_reloads() {
    let mut h = Harness::new(HoleGen, Arc::new(InlineExecutor), 64);
    h.jobs.mesh_fn = hidden_mesh;
    h.interest(&origin(1));
    h.settle(50);
    let a = h.id(ChunkPos::new(1, 0, 0));
    assert!(matches!(h.map.slots[a].mesh(), MeshState::Ready { .. }));

    // Filling the slab of the air chunk B = (2, 0, 0) that touches A hides A: Empty.
    let fill: Vec<BlockEdit> = (0..32 * 32)
        .map(|i| BlockEdit {
            pos: BlockPos::new(64, i / 32, i % 32),
            state: STONE,
        })
        .collect();
    h.edits(fill);
    h.settle(50);
    assert!(matches!(h.map.slots[a].mesh(), MeshState::Empty));
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
        matches!(h.map.slots[a].mesh(), MeshState::Ready { .. }),
        "{:?}",
        h.map.slots[a].mesh()
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
        for _ in 0..40 {
            cap.observe_job(Duration::from_micros(100));
        }
    }
    cap.observe_frame(t0 + Duration::from_millis(640));
    assert!((cap.frame - 0.010).abs() < 1e-4, "{}", cap.frame);
    assert!((cap.job - 100e-6).abs() < 1e-6, "{}", cap.job);
    assert_eq!(cap.cap(), auto_cap(4, cap.frame, cap.job));
    // A hitch counts as 33 ms at most.
    cap.observe_frame(t0 + Duration::from_secs(60));
    assert!(cap.frame < 0.0135, "{}", cap.frame);
    // A frame with a single slow job (10 times the average) barely moves the job cost.
    let job = cap.job;
    cap.observe_job(Duration::from_millis(1));
    cap.observe_frame(t0 + Duration::from_secs(61));
    assert!(
        cap.job > job && cap.job < job * 1.05,
        "{} after {job}",
        cap.job
    );
}

/// Jobs come back in no useful order: cheap generations and expensive meshes mixed, some frames
/// ending with a run of one kind. The cap depends on what the frames finished, not on that order,
/// and it stays steady while the mix does.
#[test]
fn auto_cap_is_steady_under_a_mixed_job_stream() {
    let t0 = Instant::now();
    let cheap = Duration::from_micros(5);
    let costly = Duration::from_micros(150);
    let mut caps = Vec::new();
    let mut cap = AutoCap::new(6);
    for frame in 0..200u32 {
        cap.observe_frame(t0 + Duration::from_micros(6944 * u64::from(frame)));
        caps.push(cap.cap());
        // 300 of each per frame; every other frame ends with all the cheap ones.
        let (first, last) = if frame % 2 == 0 {
            (costly, cheap)
        } else {
            (cheap, costly)
        };
        for busy in std::iter::repeat_n(first, 300).chain(std::iter::repeat_n(last, 300)) {
            cap.observe_job(busy);
        }
    }
    // After the first frames, consecutive caps differ by a few percent at most.
    let settled = &caps[64..];
    let (lo, hi) = (
        *settled.iter().min().expect("caps"),
        *settled.iter().max().expect("caps"),
    );
    assert!(hi * 100 <= lo * 103, "cap between {lo} and {hi}");
    // 6 workers x 1.5 frames / 77.5 us per job.
    let expected = auto_cap(6, 6.944e-3, 77.5e-6);
    assert!(
        lo.abs_diff(expected) * 100 <= expected * 3,
        "{lo} vs {expected}"
    );

    // The same jobs in another order give the same cap.
    let mut a = AutoCap::new(6);
    let mut b = AutoCap::new(6);
    for frame in 0..10u32 {
        let now = t0 + Duration::from_millis(7 * u64::from(frame));
        a.observe_frame(now);
        b.observe_frame(now);
        for i in 0..100u64 {
            a.observe_job(Duration::from_micros(i));
            b.observe_job(Duration::from_micros(99 - i));
        }
    }
    a.observe_frame(t0 + Duration::from_millis(70));
    b.observe_frame(t0 + Duration::from_millis(70));
    assert!((a.job - b.job).abs() < 1e-12, "{} vs {}", a.job, b.job);
    assert_eq!(a.cap(), b.cap());
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

/// The slot counters behind `WorldStats` are kept on every transition, not recounted: after
/// each step of a run through every kind of change (create, generate, fail, mesh, remesh,
/// edits under running jobs, dropped and cancelled jobs, unload, reload, moves inside the
/// hysteresis band) they match a walk of every slot.
#[test]
fn slot_counters_follow_every_transition() {
    let hold = Arc::new(HoldExecutor::default());
    let mut generator = LayerGen::new(-2..=1);
    generator.panic_at = Some(ChunkPos::new(1, -1, 1));
    let mut h = Harness::new(generator, hold.clone(), 8);
    h.settings.unload_hysteresis = 1;
    let round = |h: &mut Harness| {
        h.dispatch();
        h.check_stats();
        hold.run_all();
        h.collect();
        h.check_stats();
    };
    let settle = |h: &mut Harness| {
        for _ in 0..100 {
            if h.stats().is_settled() {
                return;
            }
            round(h);
        }
        panic!("not settled: {:?}", h.stats());
    };

    h.interest(&origin(2));
    h.check_stats();
    // A few rounds at 8 jobs in flight: generations, then meshes, some still queued.
    for _ in 0..3 {
        round(&mut h);
    }
    // Edits under running mesh jobs and on settled chunks; border voxels touch neighbors.
    h.dispatch();
    for x in [0, 31, 32, 63] {
        h.edit(BlockPos::new(x, -1, 0), BlockState::AIR);
        h.check_stats();
        h.edit(BlockPos::new(x, -33, 5), BlockState::AIR);
        h.check_stats();
    }
    hold.run_all();
    h.collect();
    h.check_stats();
    // Failing meshers and dropped jobs.
    h.jobs.mesh_fn = panicking_mesh;
    round(&mut h);
    h.jobs.mesh_fn = fake_mesh;
    h.dispatch();
    hold.drop_all();
    h.collect();
    h.check_stats();
    // A jump unloads everything (jobs still running are cancelled), then the way back reloads.
    h.dispatch();
    h.interest(&[(ChunkPos::new(40, 0, 0), 1)]);
    h.check_stats();
    round(&mut h);
    h.interest(&origin(2));
    h.check_stats();
    settle(&mut h);
    // Small moves: slots enter and leave the hysteresis band, mesh interest comes and goes.
    for x in [1, 2, 3, 2, 0, -1] {
        h.interest(&[(ChunkPos::new(x, 0, 0), 2)]);
        h.check_stats();
        round(&mut h);
        h.edit(BlockPos::new(32 * x, -1, 0), STONE);
        h.check_stats();
    }
    settle(&mut h);
    let stats = h.stats();
    assert!(stats.is_settled(), "{stats:?}");
    assert!(stats.mesh_ready > 0 && stats.mesh_empty > 0, "{stats:?}");
}

/// A settled world of stone under air seen from chunk (0, 0, 0) and (1, 0, 0) (radius 0), then
/// a hole at the border between their columns: an edit batch of 2 remeshes, (0, -1, 0) and
/// (1, -1, 0), whose jobs are held.
fn border_edit(h: &mut Harness, hold: &HoldExecutor) -> [ChunkId; 2] {
    h.interest(&[(ChunkPos::new(0, 0, 0), 0), (ChunkPos::new(1, 0, 0), 0)]);
    for _ in 0..20 {
        h.dispatch();
        hold.run_all();
        h.collect();
    }
    assert!(h.stats().is_settled(), "{:?}", h.stats());
    h.edit(BlockPos::new(31, -5, 5), BlockState::AIR);
    h.dispatch();
    assert_eq!(hold.held(), 2, "the 2 remeshes");
    assert_eq!(h.jobs.batches.len(), 1);
    [h.id(ChunkPos::new(0, -1, 0)), h.id(ChunkPos::new(1, -1, 0))]
}

/// A batch whose only held result leaves (its chunk unloads, as when a viewer moves away from a
/// member far from the edit) waits for its other members again without a clock: it is not
/// published in parts when the hold of the result that left runs out.
#[test]
fn a_held_result_that_leaves_takes_the_hold_clock_with_it() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = Harness::new(LayerGen::new(-3..=1), hold.clone(), 64);
    border_edit(&mut h, &hold);
    hold.run_first(1);
    h.collect();
    let batch = h.jobs.batches.values().next().expect("open batch");
    assert_eq!((batch.held.len(), batch.members.len()), (1, 1));
    assert!(batch.hold.is_some());
    let (left, stays) = (batch.held[0].id, batch.members[0]);
    h.with(|jobs, cx, _| jobs.unload(cx, left));
    h.check_stats();
    let batch = h.jobs.batches.values().next().expect("still open");
    assert!(batch.held.is_empty() && batch.hold.is_none(), "{batch:?}");
    // Well past the hold of the result that left: nothing is published in parts.
    for _ in 0..10 {
        h.collect();
    }
    assert_eq!(h.jobs.light_counts.edit_batches_split, 0);
    assert!(h.map.slots[stays].batch.is_some(), "still waiting");
    // Its result arrives stale (the unload bumped its neighbors' epoch): it is remeshed, still
    // in the batch, once the unloaded neighbor is back.
    hold.run_all();
    h.collect();
    assert_eq!(h.jobs.light_counts.edit_batches_split, 0);
    assert!(h.map.slots[stays].batch.is_some(), "still waiting");
}

/// A batch that never gets a result closes after 120 frames and a second, both, so short frames
/// do not close it sooner; it counts as split (its meshes go out on their own).
#[test]
fn a_batch_without_results_closes_after_frames_and_time() {
    let ms = Duration::from_millis;
    // (frame interval, frames still open, frames until closed)
    for (frame, open, closed) in [(ms(1), 999, 1000), (ms(17), 120, 121)] {
        let hold = Arc::new(HoldExecutor::default());
        let mut h = Harness::new(LayerGen::new(-3..=1), hold.clone(), 64);
        let members = border_edit(&mut h, &hold);
        h.frame = frame;
        for _ in 0..open {
            h.collect();
        }
        assert_eq!(
            h.jobs.batches.len(),
            1,
            "{frame:?}: open after {open} frames"
        );
        for _ in open..closed {
            h.collect();
        }
        assert!(
            h.jobs.batches.is_empty(),
            "{frame:?}: closed after {closed}"
        );
        assert_eq!(h.jobs.light_counts.edit_batches_split, 1, "{frame:?}");
        assert_eq!(h.jobs.light_counts.edit_batches_held, 0, "{frame:?}");
        // The meshes still arrive, each on its own.
        hold.run_all();
        h.collect();
        for id in members {
            let slot = &h.map.slots[id];
            assert!(slot.batch.is_none(), "{frame:?}");
            assert!(matches!(slot.mesh(), MeshState::Ready { .. }), "{frame:?}");
        }
    }
}

mod sky;
mod tall;
