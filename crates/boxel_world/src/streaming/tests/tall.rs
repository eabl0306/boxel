//! Worlds without a height limit: the loaded volume follows the viewer up and down, layers the
//! generator declares all air need no job, one-chunk moves touch only the slabs that change, and
//! random walks (with interest work spread over frames and jobs running in between) always end
//! in the from-scratch wanted set.
use super::*;
use crate::interest::tests::reference_world_level;
use proptest::prelude::*;
use std::collections::BTreeSet;

/// Stone in chunk layers -2 and -1, air elsewhere (its `vertical_chunks()` is -2..=1).
fn tall_world(executor: Arc<dyn JobExecutor>) -> Harness {
    Harness::new(LayerGen::new(-2..=1), executor, 64).tall()
}

fn one(center: ChunkPos, radius: u32, vertical: u32) -> [(u64, Viewer); 1] {
    [(0, Viewer::new(center, radius, vertical))]
}

/// Every position the viewers want (data level or better), by brute force.
fn reference_wanted(h: &Harness, viewers: &[Viewer]) -> BTreeMap<ChunkPos, Interest> {
    let hysteresis = h.settings.unload_hysteresis;
    let bounds = h.settings.vertical_bounds.as_ref();
    let mut out = BTreeMap::new();
    for v in viewers {
        let reach = i32::try_from(v.radius).expect("small") + 2;
        let vreach = i32::try_from(v.vertical).expect("small") + 2;
        let c = v.center;
        for y in c.y - vreach..=c.y + vreach {
            for z in c.z - reach..=c.z + reach {
                for x in c.x - reach..=c.x + reach {
                    let p = ChunkPos::new(x, y, z);
                    let level = reference_world_level(viewers, hysteresis, bounds, p);
                    if level >= Level::Data {
                        out.insert(p, level.interest());
                    }
                }
            }
        }
    }
    out
}

/// The interest invariants of a world whose interest backlog is empty.
fn check_interest(h: &Harness, viewers: &[Viewer]) {
    let hysteresis = h.settings.unload_hysteresis;
    let bounds = h.settings.vertical_bounds.as_ref();
    assert_eq!(h.jobs.interest_backlog(), 0);
    for (_, slot) in h.map.iter() {
        let pos = slot.key.pos;
        let level = reference_world_level(viewers, hysteresis, bounds, pos);
        assert_eq!(
            slot.interest - Interest::PINNED,
            level.interest(),
            "{pos:?} at {level:?}"
        );
        if level.interest().is_empty() {
            assert_eq!(level, Level::Keep, "{pos:?} kept outside every keep region");
            assert!(
                !matches!(slot.data, DataState::Queued),
                "{pos:?} kept without being started"
            );
        }
        assert_eq!(
            slot.neighbors_ready,
            own_neighbor_bits(&h.map, &h.jobs.planner, pos),
            "{pos:?} neighbor bits"
        );
    }
    let loaded: BTreeSet<ChunkPos> = h.map.iter().map(|(_, s)| s.key.pos).collect();
    for pos in reference_wanted(h, viewers).keys() {
        assert!(loaded.contains(pos), "{pos:?} wanted, not loaded");
    }
    h.check_published();
}

/// Runs held jobs until settled.
fn run_all(h: &mut Harness, hold: &HoldExecutor) {
    for _ in 0..200 {
        h.dispatch();
        hold.run_all();
        h.collect();
        if h.stats().is_settled() {
            return;
        }
    }
    panic!("not settled: {:?}", h.stats());
}

#[test]
fn the_loaded_volume_follows_the_viewer_up_and_down() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = tall_world(hold.clone());
    let at = |y| one(ChunkPos::new(0, y, 0), 1, 1);
    h.viewers(&at(0));
    // Radius 1, vertical 1: 21 data columns x layers -2..=2.
    assert_eq!(h.map.len(), 21 * 5);
    let viewers = |y| vec![Viewer::new(ChunkPos::new(0, y, 0), 1, 1)];
    check_interest(&h, &viewers(0));
    run_all(&mut h, &hold);
    // The stone layer -1 under air is exposed in the 5 mesh columns.
    assert_eq!(h.map.ready_meshes().count(), 5);

    // Up 10 chunks: everything there is above the generator's layers, so it is air without a
    // job; the old volume is beyond the hysteresis and unloads (meshes removed).
    h.take_events();
    h.viewers(&at(10));
    check_interest(&h, &viewers(10));
    assert_eq!(hold.held(), 0);
    assert_eq!(h.jobs.scheduler.queued(), 0, "no generation job in the sky");
    assert!(h.map.iter().all(|(_, s)| (8..=12).contains(&s.key.pos.y)));
    assert!(h.stats().is_settled());
    assert_eq!(removed(&h.take_events()).len(), 5);
    assert_eq!(h.map.ready_meshes().count(), 0);

    // Down to y = -6: below the stone too (air), and nothing to mesh.
    h.viewers(&at(-6));
    check_interest(&h, &viewers(-6));
    run_all(&mut h, &hold);
    assert_eq!(h.map.ready_meshes().count(), 0);

    // Back to y = -2: the stone layers are generated and the bottom of the stone (layer -2 over
    // the air of layer -3) is exposed, like its top.
    h.viewers(&at(-2));
    check_interest(&h, &viewers(-2));
    run_all(&mut h, &hold);
    let mut exposed: Vec<i32> = h.map.ready_meshes().map(|(_, k, _)| k.pos.y).collect();
    exposed.sort_unstable();
    exposed.dedup();
    assert_eq!(exposed, vec![-2, -1]);
    check_interest(&h, &viewers(-2));
}

#[test]
fn chunks_above_the_generator_can_be_edited() {
    let mut h = tall_world(Arc::new(InlineExecutor));
    h.viewers(&one(ChunkPos::new(0, 20, 0), 1, 1));
    h.settle(20);
    let high = BlockPos::new(5, 20 * 32 + 3, 9);
    assert_eq!(h.map.block_at(high), Some(BlockState::AIR));
    h.edit(high, STONE);
    h.settle(20);
    assert_eq!(h.map.block_at(high), Some(STONE));
    assert_eq!(upserted(&h.take_events()), vec![ChunkPos::new(0, 20, 0)]);
    // The other sky chunks still share one air snapshot.
    let air: Vec<&Arc<ChunkData>> = h
        .map
        .iter()
        .filter_map(|(_, s)| match &s.data {
            DataState::Ready(d) if s.key.pos != ChunkPos::new(0, 20, 0) => Some(d),
            _ => None,
        })
        .collect();
    assert!(air.windows(2).all(|w| Arc::ptr_eq(w[0], w[1])));
}

#[test]
fn edge_meshes_wait_for_their_neighbors_above_and_below() {
    let hold = Arc::new(HoldExecutor::default());
    // Stone in layers -2 and -1, which is all the generator declares.
    let mut h = Harness::new(LayerGen::new(-2..=-1), hold.clone(), 64).tall();
    // Vertical radius 0 at layer -1: the mesh layer is -1, data layers -2..=0.
    h.viewers(&one(ChunkPos::new(0, -1, 0), 0, 0));
    assert_eq!(h.map.len(), 27);
    // Layer 0 is air at once; layers -2 and -1 need generation.
    assert_eq!(h.jobs.scheduler.queued(), 18);
    let center = ChunkPos::new(0, -1, 0);
    assert_eq!(
        h.slot(center).neighbors_ready,
        mask(|dy| dy == 1),
        "only the air layer above is ready; the unloaded or ungenerated ones are not"
    );
    h.dispatch();
    assert_eq!(hold.held(), 18);
    assert!(matches!(h.slot(center).mesh, MeshState::Queued));
    assert_eq!(
        h.jobs.scheduler.queued(),
        0,
        "no mesh job before the neighbors"
    );
    hold.run_all();
    h.collect();
    assert_eq!(h.slot(center).neighbors_ready, ALL_NEIGHBORS);
    h.dispatch();
    assert_eq!(hold.held(), 1, "the center's mesh job");
    hold.run_all();
    h.collect();
    assert!(matches!(h.slot(center).mesh, MeshState::Ready { .. }));
    assert!(h.stats().is_settled());
}

#[test]
fn small_moves_touch_only_the_changed_positions() {
    let mut h = tall_world(Arc::new(InlineExecutor));
    let hysteresis = h.settings.unload_hysteresis;
    let viewer = |x, y, z| Viewer::new(ChunkPos::new(x, y, z), 12, 4);
    let mut from = viewer(0, 0, 0);
    h.viewers(&[(0, from)]);
    h.settle(200);
    let view = h.map.len();
    for (step, to) in [viewer(1, 0, 0), viewer(1, 1, 0), viewer(0, 0, -1)]
        .into_iter()
        .enumerate()
    {
        // The positions whose level changes (out -> keep excepted), by brute force.
        let mut changed = 0;
        for y in -10..=10 {
            for z in -20..=20 {
                for x in -20..=20 {
                    let p = ChunkPos::new(x, y, z);
                    let a = reference_world_level(&[from], hysteresis, None, p);
                    let b = reference_world_level(&[to], hysteresis, None, p);
                    if a != b && !(a == Level::Out && b == Level::Keep) {
                        changed += 1;
                    }
                }
            }
        }
        let before = h.jobs.interest_steps;
        h.viewers(&[(0, to)]);
        assert_eq!(h.jobs.interest_steps - before, changed, "move {step}");
        if step == 0 {
            // A horizontal move: the slabs entering and leaving, a fraction of the view.
            assert!(
                changed * 3 < view,
                "{changed} positions for a view of {view}"
            );
        }
        check_interest(&h, &[to]);
        h.settle(200);
        from = to;
    }
}

/// Empty for an all-air center, else `1 + the number of loaded neighbors` quads: a mesh built
/// with other neighbors than the map has now shows a different count.
fn neighbor_count_mesh(input: &MeshInput<'_>, materials: &MaterialTable) -> ChunkMesh {
    let mut mesh = fake_mesh(input, materials);
    if mesh.is_empty() {
        return mesh;
    }
    let n = 1 + u32::try_from(input.neighbors.iter().flatten().count()).expect("26");
    mesh.quads = vec![boxel_mesh::PackedQuad::default(); n as usize];
    mesh.bucket_counts = [0; 8];
    mesh.bucket_counts[0] = n;
    mesh
}

/// Every `Ready` mesh was built from the neighbors the map has now.
fn check_meshes_match_neighbors(h: &Harness) {
    for (_, slot) in h.map.iter() {
        if let MeshState::Ready { mesh, .. } = &slot.mesh {
            let loaded = NEIGHBOR_OFFSETS
                .iter()
                .filter(|o| h.map.data_at(slot.key.pos.offset(**o)).is_some())
                .count();
            assert_eq!(
                mesh.total_quads() as usize,
                1 + loaded,
                "{:?} mesh built from other neighbors",
                slot.key.pos
            );
        }
    }
    h.check_published();
}

#[test]
fn vertical_bounds_can_change_at_runtime() {
    let hold = Arc::new(HoldExecutor::default());
    // Stone in layers -3..=-1, air above.
    let mut h = Harness::new(LayerGen::new(-3..=1), hold.clone(), 64);
    h.jobs.mesh_fn = neighbor_count_mesh;
    let viewer = [(0, Viewer::new(ChunkPos::new(0, 0, 0), 1, 3))];
    let current = [viewer[0].1];
    for bounds in [Some(-2..=1), None, Some(-1..=0), Some(-2..=1)] {
        h.settings.vertical_bounds = bounds.clone();
        h.viewers(&viewer);
        run_all(&mut h, &hold);
        check_interest(&h, &current);
        check_meshes_match_neighbors(&h);
        let layers: BTreeSet<i32> = h.map.iter().map(|(_, s)| s.key.pos.y).collect();
        match &bounds {
            Some(b) => assert!(layers.iter().all(|y| b.contains(y)), "{layers:?}"),
            None => assert_eq!(layers, (-4..=4).collect()),
        }
        // The stone layer under the air is meshed in every case.
        assert!(
            h.map
                .ready_meshes()
                .any(|(_, k, _)| k.pos == ChunkPos::new(0, -1, 0))
        );
    }
}

/// What happens between two checks of a random walk.
#[derive(Clone, Debug)]
enum Op {
    /// Viewer `i` moves by a small offset.
    Move(u8, i32, i32, i32),
    /// Viewer `i` jumps to a position.
    Jump(u8, i32, i32, i32),
    /// Viewer `i` gets new radii.
    Radius(u8, u32, u32),
    /// Viewer `i` appears or disappears.
    Toggle(u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0u8..2, -2i32..=2, -2i32..=2, -2i32..=2)
            .prop_map(|(i, x, y, z)| Op::Move(i, x, y, z)),
        1 => (0u8..2, -12i32..=12, -6i32..=6, -12i32..=12)
            .prop_map(|(i, x, y, z)| Op::Jump(i, x, y, z)),
        1 => (0u8..2, 0u32..=3, 0u32..=2).prop_map(|(i, r, v)| Op::Radius(i, r, v)),
        1 => (0u8..2).prop_map(Op::Toggle),
    ]
}

fn apply(viewers: &mut [Option<Viewer>; 2], op: &Op) {
    match *op {
        Op::Move(i, x, y, z) => {
            if let Some(v) = &mut viewers[usize::from(i)] {
                let c = v.center;
                *v = Viewer::new(
                    ChunkPos::new(c.x + x, c.y + y, c.z + z),
                    v.radius,
                    v.vertical,
                );
            }
        }
        Op::Jump(i, x, y, z) => {
            if let Some(v) = &mut viewers[usize::from(i)] {
                *v = Viewer::new(ChunkPos::new(x, y, z), v.radius, v.vertical);
            }
        }
        Op::Radius(i, r, vr) => {
            if let Some(v) = &mut viewers[usize::from(i)] {
                *v = Viewer::new(v.center, r, vr);
            }
        }
        Op::Toggle(i) => {
            let slot = &mut viewers[usize::from(i)];
            *slot = match slot {
                Some(_) => None,
                None => Some(Viewer::new(ChunkPos::new(2, 1, -3), 1, 1)),
            };
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Random viewer walks (small moves, vertical moves, jumps, radius changes, a second viewer
    /// coming and going) on the real state machine. Interest work is spread over frames (budget
    /// zero: 64 positions per frame) with jobs dispatched, run and collected in between; after
    /// each change has been fully applied, every slot has exactly the interest a from-scratch
    /// computation gives, every wanted chunk is loaded, unwanted slots sit in a keep band, and
    /// the neighbor bits and published meshes agree with the map.
    #[test]
    fn random_walks_match_the_from_scratch_wanted_set(
        ops in proptest::collection::vec(op(), 1..16),
        bounded in any::<bool>(),
        run_jobs in proptest::collection::vec(any::<bool>(), 16),
    ) {
        let hold = Arc::new(HoldExecutor::default());
        let mut h = Harness::new(LayerGen::new(-2..=1), hold.clone(), 16);
        if !bounded {
            h = h.tall();
        }
        let mut viewers: [Option<Viewer>; 2] = [Some(Viewer::new(ChunkPos::new(0, 0, 0), 2, 1)), None];
        for (op, run) in ops.iter().zip(run_jobs.iter().cycle()) {
            apply(&mut viewers, op);
            let list: Vec<(u64, Viewer)> = (0u64..)
                .zip(viewers.iter())
                .filter_map(|(id, v)| v.map(|v| (id, v)))
                .collect();
            // Frames with a zero interest budget, jobs in between.
            let mut frames = 0;
            loop {
                h.with(|jobs, cx, settings| {
                    jobs.plan_interest(cx, settings, &list);
                    jobs.process_interest(cx, settings, Duration::ZERO);
                });
                if *run {
                    h.dispatch();
                    hold.run_all();
                    h.collect();
                }
                if h.jobs.interest_backlog() == 0 {
                    break;
                }
                frames += 1;
                prop_assert!(frames < 10_000, "interest never caught up");
            }
            let current: Vec<Viewer> = list.iter().map(|&(_, v)| v).collect();
            check_interest(&h, &current);
        }
        // Everything settles, and every wanted mesh is built.
        run_all(&mut h, &hold);
        let current: Vec<Viewer> = viewers.iter().flatten().copied().collect();
        check_interest(&h, &current);
        for (_, slot) in h.map.iter() {
            if slot.interest.contains(Interest::MESH) {
                prop_assert!(
                    matches!(slot.mesh, MeshState::Ready { .. } | MeshState::Empty),
                    "{:?} mesh {:?}", slot.key.pos, slot.mesh
                );
            }
        }
    }
}
