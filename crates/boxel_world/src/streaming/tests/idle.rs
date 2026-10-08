//! The world between frames (ADR 0022) on the state machine: an edit batch whose jobs land in
//! idle steps still publishes whole and ages in frames only, a member that unloads in a step
//! closes its batch, and random walks with idle steps between frames end in the same world as
//! frames alone, also far from the origin.
use super::tall::{Op, apply, check_bookkeeping, check_interest, op};
use super::*;
use boxel_voxel::MAX_CHUNK_COORD;
use proptest::prelude::*;

impl Harness {
    /// An idle step with all the time it needs.
    fn idle(&mut self) -> IdleWork {
        let until = Instant::now() + Duration::from_secs(10);
        self.with(|jobs, cx, settings| jobs.idle(cx, settings, until))
    }
}

#[test]
fn an_edit_batch_whose_jobs_land_between_frames_publishes_whole() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = Harness::new(LayerGen::new(-3..=1), hold.clone(), 64);
    let members = border_edit(&mut h, &hold);
    h.take_events();
    let age = |h: &Harness| h.jobs.batches.values().next().map(|b| b.age);
    let before = age(&h);
    // One remesh lands in a step, the other in a later one.
    hold.run_first(1);
    assert_eq!(h.idle(), IdleWork::Waiting);
    hold.run_all();
    assert_eq!(h.idle(), IdleWork::Done);
    assert!(
        h.take_events().is_empty(),
        "nothing published between frames"
    );
    assert_eq!(age(&h), before, "batches age in frames only");
    // The next frame publishes the batch whole: both meshes are applied (only the edited
    // chunk's changed; the other is the same mesh, not sent again).
    h.collect();
    for id in members {
        assert!(matches!(h.map.slots[id].mesh(), MeshState::Ready { .. }));
    }
    assert_eq!(upserted(&h.take_events()), vec![ChunkPos::new(0, -1, 0)]);
    assert!(h.jobs.batches.is_empty());
    assert_eq!(h.jobs.light_counts.edit_batches_split, 0);
    h.check_published();
}

#[test]
fn a_member_that_unloads_between_frames_closes_its_batch() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = Harness::new(LayerGen::new(-3..=1), hold.clone(), 64);
    let members = border_edit(&mut h, &hold);
    hold.run_first(1);
    h.collect();
    assert_eq!(
        h.jobs.batches.values().next().map(|b| b.held.len()),
        Some(1)
    );
    h.take_events();
    // The viewers jump away: the frame only plans it, the step applies it.
    let far = [(0, Viewer::new(ChunkPos::new(50, 0, 0), 0, MAX_RADIUS))];
    h.with(|jobs, cx, settings| jobs.plan_interest(cx, settings, &far));
    assert!(h.jobs.interest_backlog() > 0);
    h.idle();
    assert_eq!(h.jobs.interest_backlog(), 0);
    assert!(members.iter().all(|&id| h.map.get(id).is_none()));
    assert!(h.jobs.batches.is_empty(), "closed with its members");
    assert_eq!(h.jobs.light_counts.edit_batches_split, 0);
    let events = h.take_events();
    assert!(
        upserted(&events).is_empty(),
        "the held result's chunk is gone"
    );
    assert!(!removed(&events).is_empty(), "the old meshes go");
    h.check_published();
    h.check_stats();
}

/// What changes in one frame of a walk. No edits while walking: a chunk that one world unloads
/// and loads again while the other keeps it loses its edits (nothing is saved yet), with or
/// without idle steps.
#[derive(Clone, Debug)]
enum WalkOp {
    Viewer(Op),
    Bounds(Option<(i64, i64)>),
    Hysteresis(u32),
    Still,
}

fn walk_op() -> impl Strategy<Value = WalkOp> {
    prop_oneof![
        8 => op().prop_map(WalkOp::Viewer),
        1 => prop_oneof![Just(None), Just(Some((-2, 1))), Just(Some((-3, 3)))]
            .prop_map(WalkOp::Bounds),
        1 => (0u32..=2).prop_map(WalkOp::Hysteresis),
        2 => Just(WalkOp::Still),
    ]
}

/// Where the walks happen (chunk x and z of the origin of their viewers): around the origin,
/// across the former `i32` edges of voxels (chunk +-2^26), past 2^40 voxels (chunk 2^35) and at
/// the world limit, where the viewers are clamped.
const ORIGINS: [i64; 5] = [0, 1 << 26, -(1 << 26), 1 << 35, MAX_CHUNK_COORD - 4];

/// The viewers as the world sees them: moved to `origin` and clamped to the world limit.
fn placed(viewers: &[Option<Viewer>; 2], origin: i64) -> Vec<(u64, Viewer)> {
    let clamp = |c: i64| {
        c.saturating_add(origin)
            .clamp(-MAX_CHUNK_COORD, MAX_CHUNK_COORD)
    };
    (0u64..)
        .zip(viewers.iter())
        .filter_map(|(id, v)| {
            v.map(|v| {
                let c = v.center;
                let center = ChunkPos::new(clamp(c.x), c.y, clamp(c.z));
                (id, Viewer::new(center, v.radius, v.vertical))
            })
        })
        .collect()
}

/// One frame as the world plugin runs it, with a zero interest budget (64 positions), the job
/// step and `run` of the held jobs between them; with `idle`, then up to that many idle steps,
/// each after `run` more held jobs.
fn frame(
    h: &mut Harness,
    hold: &HoldExecutor,
    list: &[(u64, Viewer)],
    run: usize,
    idle: Option<usize>,
) {
    h.with(|jobs, cx, settings| jobs.update_interest(cx, settings, list, Duration::ZERO));
    h.dispatch();
    hold.run_first(run);
    h.collect();
    for _ in 0..idle.unwrap_or(0) {
        if h.idle() == IdleWork::Done {
            break;
        }
        hold.run_first(run);
    }
}

/// Frames (every job running) until the interest is applied and the jobs are done.
fn settle(h: &mut Harness, hold: &HoldExecutor, list: &[(u64, Viewer)], idle: bool) {
    for _ in 0..10_000 {
        frame(h, hold, list, usize::MAX, idle.then_some(1000));
        if h.jobs.interest_backlog() == 0
            && h.jobs.scheduler.stale() == 0
            && h.jobs.light_backlog.is_empty()
            && h.stats().is_settled()
        {
            return;
        }
    }
    panic!("not settled: {:?}", h.stats());
}

/// The part of a world its viewers decide: every wanted slot's interest and data, and every
/// mesh slot's mesh and kept light. (Unwanted slots in the keep bands, and the meshes or light
/// of slots without mesh interest, depend on the path.)
fn wanted(
    h: &Harness,
) -> BTreeMap<ChunkPos, (Interest, ChunkData, Option<(ChunkMesh, Option<ChunkLight>)>)> {
    h.map
        .iter()
        .filter(|(_, slot)| !(slot.interest - Interest::PINNED).is_empty())
        .map(|(_, slot)| {
            let DataState::Ready(data) = slot.data() else {
                panic!("{:?} wanted, data {:?}", slot.pos, slot.data());
            };
            let mesh = slot.interest.contains(Interest::MESH).then(|| {
                let mesh = match slot.mesh() {
                    MeshState::Ready { mesh, .. } => ChunkMesh::clone(mesh),
                    MeshState::Empty => ChunkMesh::default(),
                    other => panic!("{:?} mesh {other:?}", slot.pos),
                };
                (mesh, slot.light().cloned())
            });
            (slot.pos, (slot.interest, ChunkData::clone(data), mesh))
        })
        .collect()
}

/// A world without a height limit, with held jobs, lit or not.
fn walk_world(lit: bool) -> (Harness, Arc<HoldExecutor>) {
    let hold = Arc::new(HoldExecutor::default());
    let h = Harness::new(LayerGen::new(-2..=1), hold.clone(), 16).tall();
    (if lit { h.lit() } else { h }, hold)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Two worlds take the same changes, frame by frame (viewer moves, jumps, radii, viewers
    /// coming and going, vertical bounds, hysteresis), with the same held jobs running in each
    /// frame; one also runs idle steps between frames, with more jobs running between them.
    /// Its bookkeeping holds after every frame. Once both settle, the same edits go to chunks
    /// both have meshed, and both settle again: the wanted slots end with the same interest,
    /// data, meshes and light.
    ///
    /// Lit worlds keep their bounds: a bounds change in a lit world without a height limit can
    /// leave a light job without the sky columns around it, with or without idle steps (a
    /// debug assertion in `sky_for`; reported apart).
    #[test]
    fn idle_steps_between_frames_end_in_the_same_world(
        frames in proptest::collection::vec((walk_op(), 0usize..12, 0usize..6), 1..24),
        edits in proptest::collection::vec(
            (-40i64..40, -48i64..16, -40i64..40, any::<bool>()),
            0..8,
        ),
        origin in proptest::sample::select(ORIGINS.to_vec()),
        lit in any::<bool>(),
    ) {
        let (mut a, hold_a) = walk_world(lit);
        let (mut b, hold_b) = walk_world(lit);
        let mut viewers: [Option<Viewer>; 2] =
            [Some(Viewer::new(ChunkPos::new(0, 0, 0), 2, 1)), None];
        for (op, run, idle) in &frames {
            match op {
                WalkOp::Viewer(op) => apply(&mut viewers, op),
                WalkOp::Bounds(_) if lit => {}
                WalkOp::Bounds(bounds) => {
                    for h in [&mut a, &mut b] {
                        h.settings.vertical_bounds = bounds.map(|(lo, hi)| lo..=hi);
                    }
                }
                WalkOp::Hysteresis(n) => {
                    for h in [&mut a, &mut b] {
                        h.settings.unload_hysteresis = *n;
                    }
                }
                WalkOp::Still => {}
            }
            let list = placed(&viewers, origin);
            frame(&mut a, &hold_a, &list, *run, None);
            frame(&mut b, &hold_b, &list, *run, Some(*idle));
            check_bookkeeping(&b);
        }
        let list = placed(&viewers, origin);
        settle(&mut a, &hold_a, &list, false);
        settle(&mut b, &hold_b, &list, true);
        let current: Vec<Viewer> = list.iter().map(|&(_, v)| v).collect();
        check_interest(&a, &current);
        check_interest(&b, &current);
        prop_assert_eq!(wanted(&a), wanted(&b));

        // Edits to blocks of chunks both worlds have meshed, near the first viewer.
        let meshed = |h: &Harness, chunk: ChunkPos| {
            h.map.id_at(chunk).is_some_and(|id| {
                let slot = &h.map.slots[id];
                slot.interest.contains(Interest::MESH)
                    && matches!(slot.data(), DataState::Ready(_))
            })
        };
        let center = list.first().map_or(ChunkPos::new(0, 0, 0), |&(_, v)| v.center);
        let edits: Vec<BlockEdit> = edits
            .iter()
            .map(|&(x, y, z, stone)| BlockEdit {
                pos: BlockPos::new(center.x * 32 + x, y, center.z * 32 + z),
                state: if stone { STONE } else { BlockState::AIR },
            })
            .filter(|e| meshed(&a, e.pos.split().0) && meshed(&b, e.pos.split().0))
            .collect();
        if !edits.is_empty() {
            a.edits(edits.clone());
            b.edits(edits);
            settle(&mut a, &hold_a, &list, false);
            settle(&mut b, &hold_b, &list, true);
            prop_assert_eq!(wanted(&a), wanted(&b));
        }
        for h in [&a, &b] {
            check_bookkeeping(h);
            prop_assert_eq!(h.jobs.light_counts.edit_batches_split, 0);
        }
    }
}
