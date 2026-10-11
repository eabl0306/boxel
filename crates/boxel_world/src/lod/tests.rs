//! Tests of the LOD's world side (ADR 0031): thresholds, the wanted tree, the tree's covered
//! bits and ops, jobs, priorities, far from the origin and the plugin. Engine tests: a toy
//! generator of `test:` blocks ([`Steps`]) whose coarse cells follow the cell rule exactly.
use super::coverage::ChunkCoverage;
use super::plan::{self, ColumnKey, Eye, Plan, contains};
use super::*;
use crate::jobs::InlineExecutor;
use crate::{WorldPlugin, WorldSettings};
use boxel_app::{Last, Update};
use boxel_ecs::schedule::{LogLevel, ScheduleBuildSettings};
use boxel_math::DVec3;
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos, MAX_BLOCK_COORD,
    MAX_CHUNK_COORD,
};
use boxel_worldgen::lod::{LodCells, LodColumn, LodNode, LodScratch, LodSource};
use proptest::prelude::*;
use std::collections::BTreeSet;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A terrain of steps: the ground stands at `height(x, z)`, constant over 128 x 128 tiles (so a
/// coarse cell up to level 7 lies in one tile and the cell rule is exact), solid all the way
/// down. Chunks are `test:solid` under the ground, air over it, inside `vertical_chunks`.
#[derive(Clone, Default)]
pub(super) struct Steps {
    /// Lowest ground.
    pub(super) base: i64,
    /// A column whose LOD job panics.
    pub(super) panic_at: Option<(u8, [i64; 2])>,
    /// A column whose LOD jobs panic from when a test sets it.
    pub(super) panic_later: Arc<Mutex<Option<(u8, [i64; 2])>>>,
    /// No LOD source.
    pub(super) no_lod: bool,
    /// Every LOD column call, in order.
    pub(super) calls: Arc<Mutex<Vec<(u8, [i64; 2])>>>,
}

impl Steps {
    pub(super) fn new(base: i64) -> Self {
        Self {
            base,
            ..Self::default()
        }
    }

    pub(super) fn height(&self, x: i64, z: i64) -> i64 {
        self.base + 8 * ((x >> 7) + (z >> 7)).rem_euclid(5)
    }

    fn solid(registry: &BlockRegistry) -> BlockState {
        registry.by_name("test:solid").expect("test:solid")
    }
}

/// `ceil(a / b)` for `b > 0`.
fn div_ceil(a: i64, b: i64) -> i64 {
    -((-a).div_euclid(b))
}

impl boxel_worldgen::WorldGenerator for Steps {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let p = key.pos;
        let h = self.height(p.x << 5, p.z << 5);
        let y0 = p.y << 5;
        if y0 >= h {
            return ChunkData::uniform(BlockState::AIR);
        }
        let solid = Self::solid(registry);
        if y0 + 32 <= h {
            return ChunkData::uniform(solid);
        }
        let states: Vec<BlockState> = (0..CHUNK_VOLUME)
            .map(|i| {
                if y0 + (i / 1024) as i64 >= h {
                    BlockState::AIR
                } else {
                    solid
                }
            })
            .collect();
        ChunkData::from_dense(&states)
    }

    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        -2..=3
    }

    fn lod(&self) -> Option<&dyn LodSource> {
        (!self.no_lod).then_some(self as &dyn LodSource)
    }

    fn name(&self) -> &str {
        "steps"
    }
}

impl LodSource for Steps {
    fn column(
        &self,
        req: &LodRequest,
        registry: &BlockRegistry,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn {
        let later = *self.panic_later.lock().expect("lock");
        assert!(
            self.panic_at != Some((req.level, req.column))
                && later != Some((req.level, req.column)),
            "test source panics at {:?}",
            req.column
        );
        self.calls
            .lock()
            .expect("lock")
            .push((req.level, req.column));
        let s = req.cell_size();
        let n = req.node_size();
        let [x0, _, z0] = req.origin(0);
        // Heights of the padded footprint (tiles are 128 wide: sample every cell column).
        let heights: Vec<i64> = (-1..=32)
            .flat_map(|cx| (-1..=32).map(move |cz| (cx, cz)))
            .map(|(cx, cz)| self.height(x0 + cx * s, z0 + cz * s))
            .collect();
        let (lo_h, hi_h) = (
            *heights.iter().min().expect("cells"),
            *heights.iter().max().expect("cells"),
        );
        let solid = Self::solid(registry);
        // Layer L is all solid when its top border row lies under the lowest ground, all air
        // when its bottom border row lies over the highest.
        let band = LodColumn {
            lo: div_ceil(lo_h, n) - 1,
            hi: div_ceil(hi_h + s, n),
            below: solid,
        };
        for layer in req.layers.clone() {
            if band.uniform_at(layer).is_some() {
                continue;
            }
            let cells: &mut LodCells = &mut scratch.cells;
            let y0 = layer * n;
            for cy in -1..=32i32 {
                for cx in -1..=32i32 {
                    for cz in -1..=32i32 {
                        let h = heights[(cx + 1) as usize * 34 + (cz + 1) as usize];
                        let state = if y0 + i64::from(cy) * s < h {
                            solid
                        } else {
                            BlockState::AIR
                        };
                        cells.set(cx, cy, cz, state);
                    }
                }
            }
            match cells.uniform() {
                Some(state) => emit(layer, LodNode::Uniform(state)),
                None => emit(layer, LodNode::Cells(cells)),
            }
        }
        band
    }
}

// ---------------------------------------------------------------------------------------------
// Thresholds

#[test]
fn the_presets_horizons_at_r16_and_r64() {
    // r16 v8 h2 at ground level: T_1 is the preset's 64 R; the keep term is 875 blocks.
    let keep = KeepExtent::around(16, 8, 2, 2, None);
    assert_eq!(
        keep,
        KeepExtent {
            columns: 20,
            layers: 12
        }
    );
    for (r, k, horizon) in [(16, 3, 4096.0), (24, 4, 12_288.0), (24, 6, 49_152.0)] {
        let th = Thresholds::new(r, k, keep);
        assert_eq!(th.horizon(), horizon, "R {r} K {k}");
        assert_eq!(th.level(1), 64.0 * f64::from(r));
        assert!(!th.guard());
    }
    // r64: T_1 = 2,338 (the keep term plus two lod-1 nodes), the horizon stays 12,288.
    let th = Thresholds::new(24, 4, KeepExtent::around(64, 8, 2, 2, None));
    assert_eq!(th.level(1), 2338.0);
    assert_eq!(th.level(2), 3072.0);
    assert_eq!(th.level(3), 6144.0);
    assert_eq!(th.horizon(), 12_288.0);
}

#[test]
fn the_guard_turns_off_a_huge_vertical_radius_without_bounds() {
    let keep = KeepExtent::around(12, u32::MAX, 2, 2, None);
    assert!(Thresholds::new(24, 4, keep).guard());
    // With the forest's bounds the same viewer is fine at ground level.
    let bounds = -2..=3;
    let keep = KeepExtent::around(12, u32::MAX, 2, 2, Some(&bounds));
    assert_eq!(keep.layers, 5);
    assert!(!Thresholds::new(24, 4, keep).guard());
}

/// A plan with no history (no hysteresis).
fn fresh_plan(eye: &Eye, th: &Thresholds) -> Plan {
    plan::plan(eye, th, &mut Plan::default(), th.levels(), None)
}

/// Wanted nodes whose layer meets blocks `-64..128` (the toy terrain's band), over every level.
fn nodes_near_ground(p: &Plan) -> usize {
    p.levels
        .iter()
        .enumerate()
        .map(|(i, level)| {
            let k = i as u8 + 1;
            let e = node_edge(k);
            let band = Some(((-64i64).div_euclid(e), 127i64.div_euclid(e)));
            level
                .iter()
                .filter_map(|(_, c)| plan::intersect(c.wanted, band))
                .map(|(lo, hi)| (hi - lo + 1) as usize)
                .sum::<usize>()
        })
        .sum()
}

#[test]
fn a_high_eye_over_a_fixed_height_world_plans_at_most_twice_the_nodes() {
    let bounds = -2..=3;
    let at = |y: f64| {
        let eye = Eye::from_world(DVec3::new(0.5, y, 0.5));
        let keep = KeepExtent::around(12, u32::MAX, 2, eye.chunk().y, Some(&bounds));
        let th = Thresholds::new(24, 4, keep);
        assert!(!th.guard(), "y {y}");
        (th, fresh_plan(&eye, &th))
    };
    let (ground_th, ground) = at(80.0);
    let (high_th, high) = at(3000.0);
    assert_eq!(ground_th.level(1), 1536.0);
    // c_v = 96: T_1 = ceil(hypot(32 * 16, 32 * 96)) + 128 = 3,243.
    assert_eq!(high_th.level(1), 3243.0);
    assert_eq!(high_th.horizon(), 12_288.0);
    let (g, h) = (nodes_near_ground(&ground), nodes_near_ground(&high));
    assert!(
        h <= 2 * g,
        "{h} nodes near the ground at y 3000, {g} at y 80"
    );
}

/// Random keep regions: the viewer at the eye's chunk with radius `r`, vertical radius `v`
/// (`u32::MAX`: fixed height) and hysteresis `h`, maybe vertical bounds around, above or below
/// the eye.
fn keep_case() -> impl Strategy<Value = (u32, u32, u32, Option<(i64, i64)>, f64)> {
    (
        1u32..=64,
        prop_oneof![0u32..=64, Just(u32::MAX)],
        0u32..=4,
        prop_oneof![
            Just(None),
            (-8i64..=8, 0i64..=12).prop_map(|(lo, n)| Some((lo, lo + n))),
        ],
        -400.0f64..1200.0,
    )
        .prop_filter("a huge vertical radius needs bounds", |(_, v, _, b, _)| {
            *v != u32::MAX || b.is_some()
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Every chunk of the 3D keep region lies in a wanted lod-1 node (`T_1` covers it), and the
    /// rings are each at least two of their nodes wide.
    #[test]
    fn t1_covers_the_keep_region((r, v, h, bounds, y) in keep_case(), seed in 0u64..1000) {
        let bounds = bounds.map(|(lo, hi)| lo..=hi);
        let eye = Eye::from_world(DVec3::new(10.25, y, -3.5));
        let keep = KeepExtent::around(r, v, h, eye.chunk().y, bounds.as_ref());
        let th = Thresholds::new(24, 4, keep);
        prop_assume!(!th.guard());
        for k in 2..=4 {
            prop_assert!(th.level(k) >= th.level(k - 1) + 2.0 * node_edge(k) as f64);
        }
        let p = fresh_plan(&eye, &th);
        // The keep region: columns within r + 1 + h, layers within v + 1 + h, clipped.
        let c = eye.chunk();
        let reach = i64::from(r) + 1 + i64::from(h);
        let vreach = i64::from(v.min(4096)) + 1 + i64::from(h);
        let (mut lo, mut hi) = (c.y - vreach, c.y + vreach);
        if let Some(b) = &bounds {
            lo = lo.max(*b.start());
            hi = hi.min(*b.end());
        }
        prop_assume!(lo <= hi);
        let mut samples = Vec::new();
        for dy in [lo, hi, c.y.clamp(lo, hi)] {
            for a in 0..24 {
                let t = f64::from(a) * std::f64::consts::TAU / 24.0;
                let (dx, dz) = ((t.cos() * reach as f64) as i64, (t.sin() * reach as f64) as i64);
                if dx * dx + dz * dz <= reach * reach {
                    samples.push(ChunkPos::new(c.x + dx, dy, c.z + dz));
                }
            }
        }
        let mut rng = seed;
        for _ in 0..64 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let dx = (rng >> 33) as i64 % (2 * reach + 1) - reach;
            let dz = (rng >> 13) as i64 % (2 * reach + 1) - reach;
            let dy = lo + ((rng >> 45) as i64).rem_euclid(hi - lo + 1);
            if dx * dx + dz * dz <= reach * reach {
                samples.push(ChunkPos::new(c.x + dx, dy, c.z + dz));
            }
        }
        for chunk in samples {
            let node = ChunkPos::new(chunk.x >> 1, chunk.y >> 1, chunk.z >> 1);
            let col = p.get(ColumnKey { level: 1, x: node.x, z: node.z });
            prop_assert!(
                col.is_some_and(|c| contains(c.wanted, node.y)),
                "chunk {chunk:?} of the keep region is not in a wanted lod-1 node"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The wanted tree

/// The wanted nodes of a fresh plan by brute force: top nodes nearer than `H`, children of
/// nodes nearer than `T_(k-1)`.
fn brute_force(eye: &Eye, th: &Thresholds) -> BTreeSet<ChunkKey> {
    let top = th.levels();
    let e = node_edge(top);
    let h = th.horizon();
    let n = (h / e as f64).ceil() as i64 + 1;
    let [bx, by, bz] = eye.block.map(|v| v.div_euclid(e));
    let mut wanted = BTreeSet::new();
    let mut stack = Vec::new();
    for y in by - n..=by + n {
        for z in bz - n..=bz + n {
            for x in bx - n..=bx + n {
                let key = ChunkKey {
                    dim: DimensionId(0),
                    lod: top,
                    pos: ChunkPos::new(x, y, z),
                };
                if eye.distance(key) < h {
                    stack.push(key);
                }
            }
        }
    }
    while let Some(key) = stack.pop() {
        wanted.insert(key);
        if key.lod >= 2 && eye.distance(key) < th.level(key.lod - 1) {
            for o in 0..8 {
                let p = key.pos;
                stack.push(ChunkKey {
                    dim: key.dim,
                    lod: key.lod - 1,
                    pos: ChunkPos::new(
                        2 * p.x + (o & 1),
                        2 * p.y + ((o >> 1) & 1),
                        2 * p.z + (o >> 2),
                    ),
                });
            }
        }
    }
    wanted
}

/// Every wanted node of a plan.
fn wanted_nodes(p: &Plan) -> BTreeSet<ChunkKey> {
    let mut out = BTreeSet::new();
    for (i, level) in p.levels.iter().enumerate() {
        for ([x, z], c) in level.iter() {
            if let Some((lo, hi)) = c.wanted {
                for y in lo..=hi {
                    out.insert(ChunkKey {
                        dim: DimensionId(0),
                        lod: i as u8 + 1,
                        pos: ChunkPos::new(x, y, z),
                    });
                }
            }
        }
    }
    out
}

fn small_thresholds(eye: &Eye) -> Thresholds {
    Thresholds::new(8, 3, KeepExtent::around(4, 4, 2, eye.chunk().y, None))
}

#[test]
fn a_fresh_plan_is_the_brute_force_tree() {
    for at in [
        DVec3::new(0.5, 70.0, 0.5),
        DVec3::new(-1000.3, -77.7, 333.3),
        DVec3::new(5000.0, 4000.0, -9000.0),
    ] {
        let eye = Eye::from_world(at);
        let th = small_thresholds(&eye);
        let p = fresh_plan(&eye, &th);
        assert_eq!(wanted_nodes(&p), brute_force(&eye, &th), "eye at {at}");
    }
}

/// The plan after `p`, from `eye` (hysteresis from `p`).
fn replan(mut p: Plan, eye: &Eye, th: &Thresholds) -> Plan {
    plan::plan(eye, th, &mut p, th.levels(), None)
}

#[test]
fn moving_back_and_forth_inside_the_band_flips_nothing() {
    let a = Eye::from_world(DVec3::new(100.0, 70.0, 100.0));
    let b = Eye::from_world(DVec3::new(130.0, 75.0, 90.0));
    let th = small_thresholds(&a);
    let mut p = fresh_plan(&a, &th);
    // One round trip settles the hysteresis; after it, nothing changes.
    p = replan(p, &b, &th);
    p = replan(p, &a, &th);
    let at_a = wanted_nodes(&p);
    p = replan(p, &b, &th);
    let at_b = wanted_nodes(&p);
    for _ in 0..3 {
        p = replan(p, &a, &th);
        assert_eq!(wanted_nodes(&p), at_a);
        p = replan(p, &b, &th);
        assert_eq!(wanted_nodes(&p), at_b);
    }
}

#[test]
fn nothing_is_wanted_past_the_horizon_and_ancestors_stay() {
    let th0 = small_thresholds(&Eye::from_world(DVec3::ZERO));
    let limit = th0.horizon() + node_edge(th0.levels()) as f64;
    let mut eye = Eye::from_world(DVec3::new(3.0, 70.0, -5.0));
    let mut p = fresh_plan(&eye, &th0);
    // Walk away in steps; check after each replan.
    for step in 0..40 {
        let next = Eye::from_world(DVec3::new(3.0 + 37.0 * f64::from(step), 70.0, -5.0));
        p = replan(p, &next, &th0);
        eye = next;
        for key in wanted_nodes(&p) {
            assert!(eye.distance(key) < limit, "{key:?} past H + e_K");
            if key.lod < th0.levels() {
                let up = tree::parent(key);
                let col = p
                    .get(ColumnKey::of(up))
                    .expect("the parent column is planned");
                assert!(
                    contains(col.refined, up.pos.y),
                    "{key:?}: its parent is not refined"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Far from the origin (ADR 0014)

#[test]
fn plans_far_from_the_origin_are_the_same_tree_moved() {
    let base = DVec3::new(37.25, 70.5, -12.75);
    let reference = {
        let eye = Eye::from_world(base);
        wanted_nodes(&fresh_plan(&eye, &small_thresholds(&eye)))
    };
    let top = 3u8;
    // Offsets that keep the top level's grid: multiples of its node edge.
    for off in [1i64 << 31, -(1i64 << 31), 1 << 40, -(1 << 40)] {
        let shift = off.div_euclid(node_edge(top)) * node_edge(top);
        let eye = Eye {
            block: [base.x.floor() as i64 + shift, 70, -13 + shift],
            frac: [0.25, 0.5, 0.25],
        };
        let p = fresh_plan(&eye, &small_thresholds(&eye));
        let moved: BTreeSet<ChunkKey> = wanted_nodes(&p)
            .into_iter()
            .map(|k| {
                let d = shift >> (5 + k.lod);
                ChunkKey {
                    pos: ChunkPos::new(k.pos.x - d, k.pos.y, k.pos.z - d),
                    ..k
                }
            })
            .collect();
        assert_eq!(moved, reference, "offset {off}");
    }
}

#[test]
fn no_node_is_wanted_past_the_world_limit() {
    for k_top in [3u8, 6] {
        let th = Thresholds::new(8, k_top, KeepExtent::around(4, 4, 2, 0, None));
        for corner in [
            DVec3::splat(MAX_BLOCK_COORD as f64 - 6.0 * node_edge(k_top) as f64),
            DVec3::splat(-(MAX_BLOCK_COORD as f64) + 100.0),
            DVec3::splat(MAX_BLOCK_COORD as f64),
        ] {
            let eye = Eye::from_world(corner);
            let p = fresh_plan(&eye, &th);
            assert!(!wanted_nodes(&p).is_empty());
            for key in wanted_nodes(&p) {
                let limit = MAX_CHUNK_COORD >> key.lod;
                let q = key.pos;
                assert!(
                    q.x.abs() <= limit && q.y.abs() <= limit && q.z.abs() <= limit,
                    "{key:?} past the world limit"
                );
            }
        }
    }
}

#[test]
fn settings_validate_their_ranges() {
    assert_eq!(LodSettings::default().validate(), Ok(()));
    assert_eq!(LodSettings::default().preset_horizon(), 12_288.0);
    for bad in [
        LodSettings {
            ring_radius: 7,
            ..LodSettings::default()
        },
        LodSettings {
            levels: 0,
            ..LodSettings::default()
        },
        LodSettings {
            levels: 7,
            ..LodSettings::default()
        },
        LodSettings {
            fade_start: f32::NAN,
            ..LodSettings::default()
        },
        LodSettings {
            arena_quads: 1,
            ..LodSettings::default()
        },
        LodSettings {
            in_flight_busy: 0,
            ..LodSettings::default()
        },
        LodSettings {
            in_flight_settled: 9,
            ..LodSettings::default()
        },
        LodSettings {
            fallback_max_level: 2,
            ..LodSettings::default()
        },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// The plugin, the tree and the jobs

/// Small rings for fast tests: `R = 8`, `K = 2` (a horizon of 1,024 blocks), no frame budget.
fn small() -> LodSettings {
    LodSettings {
        ring_radius: 8,
        levels: 2,
        frame_budget: Duration::from_secs(10),
        ..LodSettings::default()
    }
}

/// A world of `steps` (bounds -2..=3, `test:` blocks) with the LOD on `executor`; the world's
/// own jobs on `world_executor`.
fn app_with(
    steps: Steps,
    settings: LodSettings,
    executor: Arc<dyn JobExecutor>,
    world_executor: Arc<dyn JobExecutor>,
) -> App {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(steps),
        registry: Arc::new(boxel_voxel::testing::registry()),
        settings: WorldSettings {
            vertical_bounds: Some(-2..=3),
            ..WorldSettings::default()
        },
        executor: world_executor,
    });
    app.add_plugins(LodWorldPlugin { settings, executor });
    app
}

fn app(steps: Steps, settings: LodSettings) -> App {
    app_with(
        steps,
        settings,
        Arc::new(InlineExecutor),
        Arc::new(InlineExecutor),
    )
}

fn spawn(app: &mut App, at: DVec3) -> Entity {
    app.world_mut()
        .spawn((
            Transform::from_translation(at),
            ChunkViewer::new(2).with_vertical_radius(1),
        ))
        .id()
}

/// What a renderer holds after applying the ops in order: each node's mask and quads.
#[derive(Default)]
struct Model {
    nodes: HashMap<ChunkKey, (u8, u32)>,
    upserts: Vec<ChunkKey>,
}

impl Model {
    fn apply(&mut self, ops: Vec<LodOp>) {
        for op in ops {
            match op {
                LodOp::Upsert { key, mesh, mask } => {
                    assert!(!mesh.is_empty(), "{key:?}: an empty mesh sent");
                    self.nodes.insert(key, (mask, mesh.total_quads()));
                    self.upserts.push(key);
                }
                LodOp::Mask { key, mask } => {
                    self.nodes
                        .get_mut(&key)
                        .unwrap_or_else(|| panic!("{key:?}: a mask for a node the renderer lacks"))
                        .0 = mask;
                }
                LodOp::Remove { key } => {
                    assert!(
                        self.nodes.remove(&key).is_some(),
                        "{key:?}: removed but never sent"
                    );
                }
            }
        }
    }

    /// No node is drawn under its parent's shown octant: a parent on the GPU hides the octant
    /// of every child on the GPU (the swap happens in one frame).
    fn check(&self, top: u8) {
        for &key in self.nodes.keys() {
            if key.lod < top
                && let Some(&(mask, _)) = self.nodes.get(&tree::parent(key))
            {
                assert!(
                    mask & tree::octant_bit(key) != 0,
                    "{key:?} drawn inside its parent's shown octant"
                );
            }
        }
    }
}

/// One frame: update, take the ops into `model`, check it.
fn frame(app: &mut App, model: &mut Model) {
    app.update();
    let ops = app.world_mut().resource_mut::<LodOps>().take();
    model.apply(ops);
    check_model(app, model);
}

/// The renderer model against the tree after a frame: a parent hides its children (the swap
/// is in one frame), every node there has its parent chain available (the chain rule), its mask
/// is the tree's mask for the coverage of this frame, and every octant it hides is drawn by
/// something else or holds nothing (module docs of `tree`).
fn check_model(app: &App, model: &Model) {
    let lod = app.world().resource::<LodWorld>();
    let cov = app.world().resource::<LodCoverage>().coverage();
    let tree = &lod.tree;
    model.check(tree.levels);
    for (&key, &(mask, _)) in &model.nodes {
        assert!(
            tree.chain_available(key),
            "{key:?} sent under an unavailable parent"
        );
        assert_eq!(
            mask,
            tree.mask(key, cov),
            "{key:?}: the mask on the renderer"
        );
        for o in 0..8u8 {
            if mask & (1 << o) == 0 {
                continue;
            }
            if key.lod == 1 {
                // Covered chunks: published (drawn) or drawing nothing.
                assert!(cov.covered(chunk_of_octant(key.pos, o)));
                continue;
            }
            let c = tree::child(key, o);
            let drawn = model.nodes.contains_key(&c);
            let nothing = tree.known_uniform(c)
                || tree
                    .node(c)
                    .is_some_and(|n| matches!(n.state, tree::NodeState::Blank));
            let touched = tree.is_touched(c, cov) && !tree.lost(c);
            assert!(
                drawn
                    || nothing
                    || full_detail_covers(cov, c)
                    || touched
                    || children_hidden(model, tree, cov, c),
                "{key:?} hides octant {o} ({c:?}) that nothing draws"
            );
        }
    }
}

/// True if every chunk inside node `key` is covered at full detail.
fn full_detail_covers(cov: &ChunkCoverage, key: ChunkKey) -> bool {
    let shift = key.lod - 1;
    let span = |v: i64| (v << shift)..=(((v + 1) << shift) - 1);
    span(key.pos.x).all(|x| {
        span(key.pos.y)
            .all(|y| span(key.pos.z).all(|z| cov.get(ChunkPos::new(x, y, z)).covered == 0xFF))
    })
}

/// True if node `key` is not on the renderer but covered through its children: each of them
/// drawn, holding nothing, covered at full detail, or itself covered through its children.
fn children_hidden(model: &Model, tree: &Tree, cov: &ChunkCoverage, key: ChunkKey) -> bool {
    if key.lod == 1 {
        return cov.get(key.pos).covered == 0xFF;
    }
    (0..8u8).all(|o| {
        let c = tree::child(key, o);
        model.nodes.contains_key(&c)
            || tree.known_uniform(c)
            || tree
                .node(c)
                .is_some_and(|n| matches!(n.state, tree::NodeState::Blank))
            || children_hidden(model, tree, cov, c)
    })
}

/// Frames until the LOD is filled (panics after `max` frames).
fn fill(app: &mut App, model: &mut Model, max: usize) -> usize {
    for i in 0..max {
        frame(app, model);
        let stats = app.world().resource::<LodStats>();
        if stats.fill >= 1.0 && stats.in_flight == 0 && stats.sends_waiting == 0 {
            return i + 1;
        }
    }
    panic!(
        "not filled after {max} frames: {:?}",
        app.world().resource::<LodStats>()
    );
}

/// The incremental covered bits equal a from-scratch walk.
fn assert_bits_from_scratch(app: &App) {
    let lod = app.world().resource::<LodWorld>();
    let cov = app.world().resource::<LodCoverage>().coverage().clone();
    let incremental: HashMap<ChunkKey, u8> = lod
        .tree
        .children()
        .iter()
        .filter(|(k, _)| !lod.tree.known_uniform(**k))
        .map(|(k, v)| (*k, *v))
        .collect();
    for (c, col) in &lod.tree.columns {
        assert_eq!(
            col.plan,
            lod.plan.get(*c).unwrap_or_default(),
            "{c:?}: the tree follows its plan"
        );
    }
    let (levels, known) = lod.tree.count_from_scratch();
    assert_eq!(lod.tree.counts(), &levels[..], "incremental counts");
    assert_eq!(
        lod.tree.columns_known().1,
        known,
        "columns with a known band"
    );
    let (scratch, kept) = lod.tree.lod1_octants_from_scratch();
    assert_eq!(kept, scratch, "lod-1 octants with quads");
    let touched: HashMap<ChunkKey, u8> = lod.tree.touched().clone();
    assert_eq!(
        touched,
        lod.tree.touched_from_scratch(&cov),
        "incremental touched bits"
    );
    let scratch = lod.tree.children_from_scratch(&cov);
    let mut keys: Vec<ChunkKey> = incremental.keys().chain(scratch.keys()).copied().collect();
    keys.sort_unstable();
    keys.dedup();
    let wrong: Vec<String> = keys
        .into_iter()
        .filter(|k| incremental.get(k) != scratch.get(k))
        .take(8)
        .map(|k| {
            let col = lod.tree.columns.get(&ColumnKey::of(k));
            format!(
                "{k:?}: incremental {:?} scratch {:?}, active {}, wanted {}, band {:?}, record {:?}",
                incremental.get(&k),
                scratch.get(&k),
                lod.tree.active(k),
                lod.tree.wanted(k),
                col.and_then(|c| c.band),
                lod.tree.node(k).map(tree::Node::available),
            )
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "covered bits differ:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn the_plugin_fills_the_rings_and_sends_every_node() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    let frames = fill(&mut app, &mut model, 300);
    let stats = app.world().resource::<LodStats>().clone();
    assert_eq!(stats.off, None);
    assert_eq!(stats.horizon, 1024.0);
    assert!(stats.nodes() > 0, "{stats:?}");
    assert_eq!(stats.levels.len(), 2);
    assert!(stats.filled_after.is_some());
    assert_eq!(
        stats
            .levels
            .iter()
            .map(|l| l.failed + l.pending)
            .sum::<usize>(),
        0
    );
    // Every node with a mesh is on the renderer, with its current mask.
    let lod = app.world().resource::<LodWorld>();
    let mut sent = 0;
    for (c, col) in &lod.tree.columns {
        for (&y, node) in &col.nodes {
            if node.sent() {
                sent += 1;
                let key = c.node(DimensionId(0), y);
                assert_eq!(model.nodes.get(&key).map(|n| n.0), Some(node.mask));
            }
        }
    }
    assert_eq!(sent, model.nodes.len());
    assert_eq!(sent, stats.nodes());
    // A lod-1 node is not drawn only where full detail covers every octant it has quads in.
    let cov = app.world().resource::<LodCoverage>().coverage();
    let hidden = model
        .nodes
        .iter()
        .filter(|(k, (mask, _))| k.lod == 1 && lod.tree.lod1_octants(k.pos) & !mask == 0)
        .count();
    assert_eq!(stats.levels[0].drawn, stats.levels[0].nodes - hidden);
    assert!(
        stats.covered_chunks > 0 && stats.published_chunks > 0,
        "{stats:?}"
    );
    assert!(cov.covered_chunks() >= cov.published_chunks());
    assert_bits_from_scratch(&app);
    assert!(frames < 300);
}

#[test]
fn fill_first_builds_the_whole_top_level_first() {
    let steps = Steps::new(20);
    let calls = Arc::clone(&steps.calls);
    let mut app = app(steps, small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let calls = calls.lock().expect("lock").clone();
    let top: BTreeSet<[i64; 2]> = calls
        .iter()
        .filter(|(k, _)| *k == 2)
        .map(|(_, c)| *c)
        .collect();
    assert!(top.len() > 10);
    assert!(
        calls[..top.len()].iter().all(|(k, _)| *k == 2),
        "the first {} jobs are the level-2 disc",
        top.len()
    );
}

/// Counts the jobs it runs (inline).
#[derive(Default)]
struct Counting(AtomicUsize);

impl JobExecutor for Counting {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        self.0.fetch_add(1, Ordering::Relaxed);
        job();
    }
}

/// Holds every job until the test runs them.
#[derive(Default)]
struct Hold(Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>);

impl Hold {
    fn run_all(&self) -> usize {
        let jobs = std::mem::take(&mut *self.0.lock().expect("lock"));
        let n = jobs.len();
        for job in jobs {
            job();
        }
        n
    }

    fn held(&self) -> usize {
        self.0.lock().expect("lock").len()
    }

    /// Runs the held jobs whose index has its bit set in `pick` (bit `i % 32`), keeps the rest.
    fn run_some(&self, pick: u32) {
        let jobs = std::mem::take(&mut *self.0.lock().expect("lock"));
        let mut kept = Vec::new();
        for (i, job) in jobs.into_iter().enumerate() {
            if pick & (1 << (i % 32)) != 0 {
                job();
            } else {
                kept.push(job);
            }
        }
        self.0.lock().expect("lock").extend(kept);
    }
}

impl JobExecutor for Hold {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        self.0.lock().expect("lock").push(job);
    }
}

/// Drops every job without running it.
struct Dropper;

impl JobExecutor for Dropper {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        drop(job);
    }
}

#[test]
fn jobs_run_on_the_plugins_own_executor() {
    let steps = Steps::new(20);
    let calls = Arc::clone(&steps.calls);
    let lod = Arc::new(Counting::default());
    let world = Arc::new(Counting::default());
    let mut app = app_with(steps, small(), lod.clone(), world.clone());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    fill(&mut app, &mut Model::default(), 300);
    let lod_jobs = lod.0.load(Ordering::Relaxed);
    assert!(lod_jobs > 0);
    assert_eq!(lod_jobs, calls.lock().expect("lock").len());
    assert!(
        world.0.load(Ordering::Relaxed) > 0,
        "the world's jobs ran on its own executor"
    );
}

#[test]
fn in_flight_caps_are_two_while_the_world_is_busy_and_five_once_settled() {
    let lod = Arc::new(Hold::default());
    // The world never finishes a job: it stays busy.
    let world = Arc::new(Hold::default());
    let mut app = app_with(Steps::new(20), small(), lod.clone(), world.clone());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(lod.held(), 2);
    assert_eq!(app.world().resource::<LodStats>().in_flight, 2);
    // A world that settles: five.
    let lod = Arc::new(Hold::default());
    let mut app = app_with(
        Steps::new(20),
        small(),
        lod.clone(),
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    for _ in 0..20 {
        app.update();
    }
    assert!(app.world().resource::<WorldStats>().is_settled());
    assert_eq!(lod.held(), 5);
}

/// A worker that ends a job starts the next handed-off one while under the cap, with no frame
/// of the main thread in between: the slots stay full between frames.
#[test]
fn workers_start_handed_off_jobs_between_frames() {
    let lod = Arc::new(Hold::default());
    let steps = Steps::new(20);
    let calls = Arc::clone(&steps.calls);
    let mut app = focus_app(steps, small(), lod.clone(), I64Vec3::new(0, 60, 0));
    app.update();
    app.update();
    let jobs = &app.world().resource::<LodWorld>().jobs;
    let line = jobs.handed_off();
    assert_eq!((lod.held(), jobs.running()), (5, 5));
    assert_eq!(line, 4 * 5, "four handed off per slot");
    // The held jobs run (the workers), with no update: each starts the first in line.
    assert_eq!(lod.run_all(), 5);
    let jobs = &app.world().resource::<LodWorld>().jobs;
    assert_eq!((lod.held(), jobs.running()), (5, 5));
    assert_eq!(jobs.handed_off(), line - 5);
    assert_eq!(calls.lock().expect("lock").len(), 5);
    // Every job still sends one message: nothing is left once all ran.
    let mut model = Model::default();
    fill_held(&mut app, &lod, &mut model);
    let state = app.world().resource::<LodWorld>();
    assert_eq!((state.jobs.in_flight, state.jobs.running()), (0, 0));
    assert!(state.jobs_of.is_empty());
}

/// While the world is busy only class-0 columns run (the top level first, then the nodes that
/// close holes): the rest waits for the world to settle, and handed-off jobs of a lower class
/// are taken back.
#[test]
fn a_busy_world_runs_only_class_0_columns() {
    let lod = Arc::new(Hold::default());
    let world = Arc::new(Hold::default());
    let steps = Steps::new(20);
    let calls = Arc::clone(&steps.calls);
    let mut app = app_with(steps, small(), lod.clone(), world.clone());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    for _ in 0..200 {
        lod.run_all();
        app.update();
    }
    let top = small().levels;
    let calls = calls.lock().expect("lock").clone();
    assert!(!calls.is_empty());
    assert!(calls.iter().all(|&(level, _)| level == top), "{calls:?}");
    let state = app.world().resource::<LodWorld>();
    assert!(
        state
            .jobs
            .handed_off_columns()
            .iter()
            .all(|&(_, class)| class == 0)
    );
    assert!(
        app.world().resource::<LodStats>().fill < 1.0,
        "level 1 waits"
    );
    // Handed-off jobs of other classes are taken back when the world turns busy.
    let mut state = app.world_mut().resource_mut::<LodWorld>();
    let c = ColumnKey {
        level: 1,
        x: 0,
        z: 0,
    };
    let job = JobInput {
        id: state.jobs.next_id(),
        generator: Arc::new(Steps::new(20)),
        registry: Arc::new(boxel_voxel::testing::registry()),
        materials: Arc::new(LodMaterials::from_registry(
            &boxel_voxel::testing::registry(),
        )),
        request: LodRequest {
            dim: DimensionId(0),
            level: c.level,
            column: [c.x, c.z],
            layers: 0..=0,
        },
        only: None,
        cancel: Arc::new(AtomicBool::new(false)),
    };
    state.jobs.hand_off(job, 2);
    assert!(state.jobs.handed_off_columns().contains(&(c, 2)));
    lod.run_all();
    app.update();
    let state = app.world().resource::<LodWorld>();
    assert!(!state.jobs.handed_off_columns().contains(&(c, 2)));
}

/// A replan goes level by level, coarse first, a budgeted run of columns a frame (with a budget
/// of a nanosecond, 16): a first plan or a jump never lands in one frame. Done, it is the plan
/// made at once. A jump while one is under way starts it over from the top, once the level
/// being applied is done.
#[test]
fn replans_go_level_by_level_coarse_first() {
    let settings = LodSettings {
        ring_radius: 8,
        levels: 3,
        frame_budget: Duration::from_nanos(1),
        ..LodSettings::default()
    };
    let mut app = focus_app(
        Steps::new(20),
        settings.clone(),
        Arc::new(InlineExecutor),
        I64Vec3::new(0, 60, 0),
    );
    let levels_planned = |app: &App| {
        let lod = app.world().resource::<LodWorld>();
        (1..=3u8)
            .filter(|&k| lod.tree.columns.keys().any(|c| c.level == k))
            .collect::<Vec<_>>()
    };
    let mut seen = Vec::new();
    for _ in 0..1000 {
        app.update();
        let levels = levels_planned(&app);
        if !levels.is_empty() && seen.last() != Some(&levels) {
            seen.push(levels);
        }
        if app.world().resource::<LodWorld>().replan.is_none() && !seen.is_empty() {
            break;
        }
    }
    assert_eq!(seen, [vec![3], vec![2, 3], vec![1, 2, 3]], "coarse first");
    let lod = app.world().resource::<LodWorld>();
    assert!(lod.replan.is_none());
    let eye = lod.planned.expect("planned").eye;
    let th = lod.planned.expect("planned").th;
    let mut empty = Plan::default();
    let once = plan::plan(&eye, &th, &mut empty, 3, lod.ceiling);
    for k in 1..=3u8 {
        let i = usize::from(k) - 1;
        let staged: Vec<_> = lod.plan.levels[i].iter().map(|(c, p)| (c, *p)).collect();
        let whole: Vec<_> = once.levels[i].iter().map(|(c, p)| (c, *p)).collect();
        assert_eq!(staged, whole, "level {k}");
    }
    assert_bits_from_scratch(&app);
    // Two jumps, the second while the first's replan is under way: the tree ends at the second.
    let jump = |app: &mut App, x: i64| {
        app.world_mut().resource_mut::<LodFocus>().origin = I64Vec3::new(x, 60, 0);
    };
    jump(&mut app, 1 << 20);
    app.update();
    assert!(app.world().resource::<LodWorld>().replan.is_some());
    jump(&mut app, 1 << 22);
    let mut frames = 0;
    while frames < 5000 {
        app.update();
        frames += 1;
        let lod = app.world().resource::<LodWorld>();
        if lod.replan.is_none() && lod.planned.expect("planned").eye.block[0] == 1 << 22 {
            break;
        }
    }
    assert!(frames > 3 && frames < 5000, "{frames} frames");
    let lod = app.world().resource::<LodWorld>();
    assert!(
        lod.tree
            .columns
            .keys()
            .all(|c| (c.x << (5 + c.level)) > (1 << 22) - 20_000),
        "nothing of the old places is left"
    );
    assert_bits_from_scratch(&app);
}

#[test]
fn a_viewer_far_from_the_eye_never_turns_the_guard_on() {
    let mut app = focus_app(
        Steps::new(20),
        small(),
        Arc::new(InlineExecutor),
        I64Vec3::new(0, 60, 0),
    );
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let far = spawn(&mut app, DVec3::new(1.0e6, 60.0, 0.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    for _ in 0..5 {
        frame(&mut app, &mut model);
        assert_eq!(app.world().resource::<LodStats>().off, None);
    }
    // Near the eye it counts again: T_1 reaches it.
    app.world_mut()
        .entity_mut(far)
        .get_mut::<Transform>()
        .expect("viewer")
        .translation
        .x = 600.0;
    frame(&mut app, &mut model);
    let keep = app.world().resource::<LodWorld>().keep.expect("keep");
    assert!(keep.columns >= 600 / 32, "{keep:?}");
    // Every viewer within the preset's horizon counts; past it, none.
    let eye = ChunkPos::new(0, 2, 0);
    let viewer = |x: i64| Viewer::new(ChunkPos::new(x, 2, 0), 4, 2);
    let at = |x: i64| KeepExtent::of_viewers(eye, [viewer(0), viewer(x)], 2, None, 100);
    assert_eq!(at(100).columns, 100 + 4 + 1 + 2 + 1);
    assert_eq!(at(101), at(0));
}

#[test]
fn a_panicking_source_fails_its_column_once() {
    let steps = Steps {
        panic_at: Some((2, [0, 0])),
        ..Steps::new(20)
    };
    let calls = Arc::clone(&steps.calls);
    let mut app = app(steps, small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    fill(&mut app, &mut Model::default(), 300);
    let stats = app.world().resource::<LodStats>();
    assert_eq!(stats.jobs_failed, 1);
    assert_eq!(stats.in_flight, 0);
    let lod = app.world().resource::<LodWorld>();
    let col = &lod.tree.columns[&ColumnKey {
        level: 2,
        x: 0,
        z: 0,
    }];
    assert!(col.failed && col.nodes.is_empty());
    // The panic happens before the call is recorded: no call for that column at all.
    assert!(!calls.lock().expect("lock").contains(&(2, [0, 0])));
}

#[test]
fn a_dropped_job_sends_failed() {
    let mut app = app_with(
        Steps::new(20),
        small(),
        Arc::new(Dropper),
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    for _ in 0..5 {
        app.update();
    }
    let lod = app.world().resource::<LodWorld>();
    assert_eq!(
        lod.jobs.in_flight,
        lod.jobs.handed_off() + lod.jobs.results_waiting(),
        "every dropped job reported (the handed-off ones never ran)"
    );
    assert_eq!(lod.jobs.running(), 0, "a dropped job is no longer running");
    assert!(lod.jobs.failed >= 2);
}

#[test]
fn cancelled_columns_drop_their_results_and_every_job_sends_one_message() {
    let lod = Arc::new(Hold::default());
    let mut app = app_with(
        Steps::new(20),
        small(),
        lod.clone(),
        Arc::new(InlineExecutor),
    );
    let viewer = spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    for _ in 0..30 {
        app.update();
    }
    // Some jobs ran to the end before the jump (their results wait in the channel), some will
    // see their cancel flag after it.
    let first = lod.run_all();
    app.update();
    let early = lod.held();
    assert!(first > 0 && early > 0);
    app.world_mut()
        .entity_mut(viewer)
        .get_mut::<Transform>()
        .expect("viewer")
        .translation
        .x += 1.0e6;
    app.update();
    lod.run_all();
    for _ in 0..3 {
        app.update();
    }
    let state = app.world().resource::<LodWorld>();
    assert!(state.jobs.cancelled > 0, "held jobs saw their cancel flag");
    // Nothing of the old area is left.
    for c in state.tree.columns.keys() {
        let x = (c.x << (5 + c.level)) as f64;
        assert!(x > 1.0e6 - 3000.0, "{c:?} left from before the jump");
    }
    // Every spawned job sent exactly one message: none in flight once all ran.
    while lod.run_all() > 0 {
        app.update();
    }
    app.update();
    let state = app.world().resource::<LodWorld>();
    assert_eq!(state.jobs.in_flight, 0);
    assert!(state.jobs_of.is_empty());
}

#[test]
fn the_tree_follows_a_moving_eye() {
    let mut app = app(Steps::new(20), small());
    let viewer = spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    for step in 0..12 {
        app.world_mut()
            .entity_mut(viewer)
            .get_mut::<Transform>()
            .expect("viewer")
            .translation += DVec3::new(45.0, if step % 2 == 0 { 9.0 } else { -7.0 }, -20.0);
        frame(&mut app, &mut model);
        assert_bits_from_scratch(&app);
    }
    fill(&mut app, &mut model, 300);
    assert_bits_from_scratch(&app);
    // Every node on the renderer is a sent record (a node that leaves the wanted tree goes at
    // once: its parent is on the renderer).
    let lod = app.world().resource::<LodWorld>();
    for &key in model.nodes.keys() {
        assert!(lod.tree.node(key).is_some_and(tree::Node::sent), "{key:?}");
    }
}

#[test]
fn a_resync_sends_every_node_again_the_coarsest_first() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let before = model.nodes.len();
    app.world_mut().write_message(LodResync);
    // A new renderer, small sends: the order shows.
    app.world_mut()
        .resource_mut::<LodSettings>()
        .publish_per_frame = 4;
    let mut renderer = Model::default();
    fill(&mut app, &mut renderer, 3000);
    assert_eq!(renderer.nodes.len(), before);
    // The lod-1 nodes that fill holes in full detail (published chunks next to uncovered ones)
    // with their ancestors first, then the rest; each coarse to fine.
    let lod = app.world().resource::<LodWorld>();
    let cov = app.world().resource::<LodCoverage>().coverage();
    let mut urgent = BTreeSet::new();
    for &k in &renderer.upserts {
        let o = cov.get(k.pos);
        if k.lod == 1 && o.published != 0 && o.covered != 0xFF {
            let mut a = k;
            while urgent.insert(a) && a.lod < lod.tree.levels {
                a = tree::parent(a);
            }
        }
    }
    assert!(
        urgent.iter().any(|k| k.lod == 1),
        "full detail touches lod-1 nodes"
    );
    let split = renderer
        .upserts
        .iter()
        .position(|k| !urgent.contains(k))
        .unwrap_or(renderer.upserts.len());
    let (first, rest) = renderer.upserts.split_at(split);
    assert!(
        rest.iter().all(|k| !urgent.contains(k)),
        "urgent nodes first"
    );
    for part in [first, rest] {
        let levels: Vec<u8> = part.iter().map(|k| k.lod).collect();
        assert!(
            levels.windows(2).all(|w| w[0] >= w[1]),
            "coarse to fine: {levels:?}"
        );
    }
    assert_bits_from_scratch(&app);
}

#[test]
fn ops_no_renderer_took_are_dropped() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut sent: BTreeSet<ChunkKey> = BTreeSet::new();
    for _ in 0..20 {
        app.update();
        let ops = &app.world().resource::<LodOps>().ops;
        for op in ops {
            if let LodOp::Upsert { key, .. } = op {
                assert!(sent.insert(*key), "{key:?} sent twice: old ops were kept");
            }
        }
    }
    assert!(!sent.is_empty());
}

#[test]
fn a_refused_node_waits_for_room_and_its_parent_draws_meanwhile() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    // A level-1 node away from full detail whose parent is on the renderer.
    let cov = app.world().resource::<LodCoverage>().coverage().clone();
    let key = *model
        .nodes
        .keys()
        .find(|k| {
            k.lod == 1
                && model.nodes.contains_key(&tree::parent(**k))
                && cov.get(k.pos) == Octants::default()
        })
        .expect("a lod-1 node under a sent parent");
    let up = tree::parent(key);
    assert!(model.nodes[&up].0 & tree::octant_bit(key) != 0);
    {
        let mut feedback = app.world_mut().resource_mut::<LodFeedback>();
        feedback.refused.push(key);
        feedback.arena_free_quads = Some(0);
    }
    // The renderer dropped it.
    model.nodes.remove(&key);
    frame(&mut app, &mut model);
    assert!(
        model.nodes[&up].0 & tree::octant_bit(key) == 0,
        "the parent shows the octant"
    );
    for _ in 0..60 {
        frame(&mut app, &mut model);
        assert!(
            !model.nodes.contains_key(&key),
            "sent again into a full arena"
        );
    }
    app.world_mut()
        .resource_mut::<LodFeedback>()
        .arena_free_quads = Some(1 << 30);
    // The back-off is over (as if a second went by).
    app.world_mut()
        .resource_mut::<LodWorld>()
        .tree
        .expire_refusals();
    frame(&mut app, &mut model);
    assert!(
        model.nodes.contains_key(&key),
        "sent again once there is room"
    );
    assert!(model.nodes[&up].0 & tree::octant_bit(key) != 0);
    assert_eq!(app.world().resource::<LodStats>().refused, 1);
}

#[test]
fn full_detail_coverage_hides_lod1_octants() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    // A lod-1 node away from the loaded chunks (no event will touch its coverage).
    let map = app.world().resource::<ChunkMap>();
    let key = *model
        .nodes
        .keys()
        .find(|k| {
            k.lod == 1
                && model.nodes[*k].0 == 0
                && (0..8).all(|o| map.id_at(chunk_of_octant(k.pos, o)).is_none())
        })
        .expect("a lod-1 node");
    let drawn = app.world().resource::<LodStats>().levels[0].drawn;
    let chunk = ChunkPos::new(key.pos.x * 2 + 1, key.pos.y * 2, key.pos.z * 2);
    app.world_mut()
        .resource_mut::<LodCoverage>()
        .set(chunk, false, true);
    frame(&mut app, &mut model);
    assert_eq!(model.nodes[&key].0, 0b10, "octant x = 1 hidden");
    // All 8 chunks: the node is covered, and its parent hides it too.
    let mut cov = app.world_mut().resource_mut::<LodCoverage>();
    for o in 0..8 {
        let p = key.pos;
        cov.set(
            ChunkPos::new(
                p.x * 2 + (o & 1),
                p.y * 2 + ((o >> 1) & 1),
                p.z * 2 + (o >> 2),
            ),
            true,
            false,
        );
    }
    frame(&mut app, &mut model);
    assert_eq!(model.nodes[&key].0, 0xFF);
    assert_bits_from_scratch(&app);
    let stats = app.world().resource::<LodStats>();
    assert_eq!(stats.levels[0].drawn, drawn - 1);
}

#[test]
fn disabling_removes_every_node_and_enabling_fills_again() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let n = model.nodes.len();
    app.world_mut().resource_mut::<LodSettings>().enabled = false;
    frame(&mut app, &mut model);
    assert!(model.nodes.is_empty());
    assert_eq!(
        app.world().resource::<LodStats>().off,
        Some(LodOff::Disabled)
    );
    app.world_mut().resource_mut::<LodSettings>().enabled = true;
    fill(&mut app, &mut model, 300);
    assert_eq!(model.nodes.len(), n);
}

#[test]
fn turning_off_with_jobs_queued_leaves_no_work_behind() {
    let lod = Arc::new(Hold::default());
    let mut app = app_with(
        Steps::new(20),
        small(),
        lod.clone(),
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    for _ in 0..3 {
        app.update();
    }
    let state = app.world().resource::<LodWorld>();
    assert!(lod.held() > 0 && !state.jobs.queue.is_empty());
    app.world_mut().resource_mut::<LodSettings>().enabled = false;
    app.update();
    lod.run_all();
    app.update();
    assert!(
        !app.world().resource::<LodWorld>().has_job_work(),
        "nothing left to ask idle steps for"
    );
}

#[test]
fn invalid_settings_keep_the_last_valid_ones() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    fill(&mut app, &mut Model::default(), 300);
    app.world_mut().resource_mut::<LodSettings>().ring_radius = 3;
    app.update();
    let stats = app.world().resource::<LodStats>();
    assert_eq!(stats.off, None);
    assert_eq!(stats.horizon, 1024.0);
    // Invalid from the start: no LOD at all.
    let mut app = app_with(
        Steps::new(20),
        LodSettings {
            levels: 9,
            ..small()
        },
        Arc::new(InlineExecutor),
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    for _ in 0..3 {
        app.update();
    }
    let stats = app.world().resource::<LodStats>();
    assert_eq!(stats.off, Some(LodOff::InvalidSettings));
    assert_eq!(stats.nodes(), 0);
}

/// Once the guard turned the LOD off, it comes back only when `T_1` is two lod-1 nodes under the
/// preset's horizon: an eye bobbing across the line does not clear and refill the LOD each time.
#[test]
fn the_guard_turns_on_again_only_past_a_margin() {
    let mut app = app(Steps::new(20), small());
    let viewer = app
        .world_mut()
        .spawn((
            Transform::from_translation(DVec3::new(10.0, 60.0, 10.0)),
            ChunkViewer::new(2).with_vertical_radius(u32::MAX),
        ))
        .id();
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    // Bounds -2..=3, radius 2, hysteresis 2: T_1 = ceil(hypot(192, 32 (layer + 3))) + 128
    // against a horizon of 1,024; the eye's chunk layer 25 trips it, 20 is past the margin.
    let mut at = |layer: f64| {
        app.world_mut()
            .entity_mut(viewer)
            .get_mut::<Transform>()
            .expect("viewer")
            .translation
            .y = (layer + 0.5) * 32.0;
        frame(&mut app, &mut model);
        (app.world().resource::<LodStats>().off, model.nodes.len())
    };
    assert_eq!(at(24.0).0, None);
    assert_eq!(at(25.0), (Some(LodOff::Guard), 0));
    for layer in [24.0, 21.0, 24.0, 25.0, 22.0] {
        assert_eq!(at(layer).0, Some(LodOff::Guard), "layer {layer}");
    }
    assert_eq!(at(20.0).0, None);
}

/// Over a fixed-height world the wanted layers stop at the generator's top: a high eye wants no
/// air column at its own height (a disc as wide as `T_1` there), about as many columns as at the
/// ground (the `far` preset's rings).
#[test]
fn a_high_eye_over_a_fixed_height_world_wants_no_air_layers() {
    let settings = LodSettings {
        ring_radius: 24,
        levels: 4,
        ..small()
    };
    let columns_at = |y: f64| {
        let mut app = app(Steps::new(20), settings.clone());
        app.world_mut().spawn((
            Transform::from_translation(DVec3::new(10.0, y, 10.0)),
            ChunkViewer::new(2).with_vertical_radius(u32::MAX),
        ));
        app.update();
        let lod = app.world().resource::<LodWorld>();
        assert_eq!(lod.off, None, "y {y}");
        // The generator's top is chunk layer 3.
        for (c, col) in &lod.tree.columns {
            if let Some((_, hi)) = col.plan.wanted {
                assert!(hi <= 3 >> c.level, "y {y}: {c:?} wants layers up to {hi}");
            }
        }
        app.world().resource::<LodStats>().columns()
    };
    let (ground, high) = (columns_at(80.0), columns_at(3000.0));
    assert!(
        2 * high <= 3 * ground,
        "{high} columns at y 3000, {ground} at y 80"
    );
}

/// Every chunk the main renderer draws.
fn published_chunks(app: &App) -> Vec<ChunkPos> {
    let cov = app.world().resource::<LodCoverage>().coverage();
    cov.positions()
        .flat_map(|p| (0..8).map(move |o| chunk_of_octant(p, o)))
        .filter(|&c| cov.published(c))
        .collect()
}

/// A smaller radius leaves published chunks past the new keep region until the world unloads
/// them, a budget at a time. Meanwhile `T_1` keeps its reach: every published chunk stays inside a
/// wanted lod-1 node (else its level-2 parent would hide the octant of a touched child nobody
/// builds, and the sky would show through the unpublished ground there).
#[test]
fn shrinking_the_radius_keeps_every_published_chunk_inside_a_wanted_lod1_node() {
    let mut app = app(Steps::new(20), small());
    let viewer = app
        .world_mut()
        .spawn((
            Transform::from_translation(DVec3::new(10.0, 60.0, 10.0)),
            ChunkViewer::new(20).with_vertical_radius(1),
        ))
        .id();
    let mut model = Model::default();
    for i in 0.. {
        frame(&mut app, &mut model);
        let stats = app.world().resource::<WorldStats>();
        if i > 1 && stats.chunks > 0 && stats.is_settled() {
            break;
        }
        assert!(i < 600, "the world did not settle");
    }
    fill(&mut app, &mut model, 600);
    let t1 = |app: &App| {
        app.world()
            .resource::<LodWorld>()
            .planned
            .expect("planned")
            .th
            .level(1)
    };
    assert!(t1(&app) > 576.0, "{}", t1(&app));
    // The unloads take many frames.
    app.world_mut()
        .resource_mut::<WorldSettings>()
        .interest_budget = Duration::from_nanos(1);
    app.world_mut()
        .entity_mut(viewer)
        .get_mut::<ChunkViewer>()
        .expect("viewer")
        .radius = 2;
    let (mut held, mut frames) = (0, 0);
    while t1(&app) > 512.0 {
        frame(&mut app, &mut model);
        let lod = app.world().resource::<LodWorld>();
        for c in published_chunks(&app) {
            let (key, _) = node_of_chunk(c, 1);
            assert!(
                lod.tree.wanted(key),
                "frame {frames}: {c:?} published outside the wanted lod-1 nodes (T_1 {})",
                t1(&app)
            );
        }
        assert_published_hidden(&app, &model);
        held += usize::from(t1(&app) > 512.0);
        frames += 1;
        assert!(frames < 400, "T_1 still {} after {frames} frames", t1(&app));
    }
    assert!(held > 10, "T_1 held for {held} frames");
    fill(&mut app, &mut model, 300);
    assert_bits_from_scratch(&app);
}

#[test]
fn a_generator_without_a_source_turns_the_lod_off() {
    let steps = Steps {
        no_lod: true,
        ..Steps::new(20)
    };
    let mut app = app(steps, small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    for _ in 0..3 {
        app.update();
    }
    let stats = app.world().resource::<LodStats>();
    assert_eq!(stats.off, Some(LodOff::NoSource));
    assert_eq!(stats.jobs_done, 0);
}

#[test]
fn the_queue_orders_by_class_then_screen_size() {
    let lod = Arc::new(Hold::default());
    let mut app = app_with(
        Steps::new(20),
        LodSettings {
            levels: 3,
            ..small()
        },
        lod.clone(),
        Arc::new(InlineExecutor),
    );
    app.world_mut().insert_resource(LodFocus {
        origin: I64Vec3::new(0, 60, 0),
        eye_rel: Vec3::new(0.5, 0.5, 0.5),
        forward: Vec3::X,
        fov_y: 70f32.to_radians(),
        aspect: 16.0 / 9.0,
    });
    spawn(&mut app, DVec3::new(0.5, 60.5, 0.5));
    app.update();
    // Let the top level through, then look at what is queued.
    for _ in 0..40 {
        lod.run_all();
        app.update();
    }
    let state = app.world().resource::<LodWorld>();
    let cov = app.world().resource::<LodCoverage>().coverage();
    let mut queue = state.jobs.queue.clone();
    let eye = state.planned.expect("planned").eye;
    let mut last: Option<(u8, f32)> = None;
    let mut classes = BTreeSet::new();
    while let Some(entry) = queue.pop() {
        let Some(col) = state.tree.columns.get(&entry.column) else {
            continue;
        };
        if col.ticket != Some(entry.ticket) {
            continue;
        }
        let work = col.work().expect("queued work");
        let key = entry.column.node(
            DimensionId(0),
            col.nearest(&work, 60i64.div_euclid(node_edge(entry.column.level))),
        );
        let node = |y: i64| entry.column.node(DimensionId(0), y);
        // A hole: a lod-1 node with published chunks that full detail does not cover whole, or
        // a coarser node with such a child (the chain rule needs it first).
        let hole = |y: i64| {
            let key = node(y);
            if key.lod == 1 {
                let o = cov.get(key.pos);
                o.published != 0 && o.covered != 0xFF
            } else {
                state.tree.touched_bits(key, cov) & !state.tree.children_bits(key, cov) != 0
            }
        };
        let expect = if entry.column.level == 3 || work.layers().any(hole) {
            0
        } else if entry.column.level == 1
            && work.layers().any(|y| cov.get(node(y).pos).published != 0)
        {
            3
        } else if state.in_view(&eye, key) {
            1
        } else {
            2
        };
        assert_eq!(entry.class, expect, "{entry:?}");
        if let Some((class, size)) = last {
            assert!(class < entry.class || (class == entry.class && size >= entry.size));
        }
        last = Some((entry.class, entry.size));
        classes.insert(entry.class);
    }
    assert!(classes.contains(&1) && classes.contains(&2), "{classes:?}");
}

#[test]
fn far_from_the_origin_the_rings_fill_the_same_way() {
    for at in [
        DVec3::new(
            (1u64 << 40) as f64 + 10.0,
            60.0,
            -((1u64 << 40) as f64) + 10.0,
        ),
        DVec3::new((1u64 << 31) as f64, 60.0, -((1u64 << 31) as f64)),
        DVec3::new(
            (MAX_BLOCK_COORD - 6 * node_edge(2)) as f64,
            60.0,
            -((MAX_BLOCK_COORD - 6 * node_edge(2)) as f64),
        ),
    ] {
        let mut app = app(Steps::new(20), small());
        spawn(&mut app, at);
        let mut model = Model::default();
        fill(&mut app, &mut model, 300);
        let stats = app.world().resource::<LodStats>();
        assert!(stats.nodes() > 0, "at {at}: {stats:?}");
        assert!(stats.published_chunks > 0, "at {at}: {stats:?}");
        assert_feed_matches_map(&app);
        assert_published_hidden(&app, &model);
        for key in model.nodes.keys() {
            let limit = MAX_CHUNK_COORD >> key.lod;
            assert!(
                key.pos.x.abs() <= limit && key.pos.z.abs() <= limit,
                "{key:?}"
            );
        }
        assert_bits_from_scratch(&app);
    }
}

#[test]
fn without_the_plugin_nothing_of_the_lod_exists() {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(Steps::new(20)),
        registry: Arc::new(boxel_voxel::testing::registry()),
        ..WorldPlugin::default()
    });
    app.update();
    let w = app.world();
    assert!(w.get_resource::<LodSettings>().is_none());
    assert!(w.get_resource::<LodStats>().is_none());
    assert!(w.get_resource::<LodOps>().is_none());
    assert!(w.get_resource::<LodWorld>().is_none());
}

/// A system that moves viewers is not ambiguous with the LOD (or the world).
#[test]
fn lod_systems_have_no_ambiguities() {
    fn move_viewers(mut q: Query<&mut Transform, With<ChunkViewer>>) {
        for mut t in &mut q {
            t.translation.x += 0.5;
        }
    }
    fn strict(schedule: &mut Schedule) {
        schedule.set_build_settings(ScheduleBuildSettings {
            ambiguity_detection: LogLevel::Error,
            ..ScheduleBuildSettings::default()
        });
    }
    let mut app = app(Steps::new(20), small());
    app.add_systems(Update, move_viewers);
    app.edit_schedule(Update, strict)
        .edit_schedule(Last, strict);
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    app.update();
    app.update();
    assert!(app.world().resource::<LodStats>().columns() > 0);
}

#[test]
fn lod_jobs_hand_the_mesher_cells_from_the_source() {
    // One column job of the toy source at level 1 against its exact cells.
    let steps = Steps::new(20);
    let registry = Arc::new(boxel_voxel::testing::registry());
    let materials = Arc::new(LodMaterials::from_registry(&registry));
    let input = JobInput {
        id: 7,
        generator: Arc::new(steps),
        registry,
        materials,
        request: LodRequest {
            dim: DimensionId(0),
            level: 1,
            column: [0, 0],
            layers: -3..=3,
        },
        only: None,
        cancel: Arc::new(AtomicBool::new(false)),
    };
    let LodMessage::Built { id, column, nodes } = jobs::run_column(&input) else {
        panic!("a built column");
    };
    assert_eq!(id, 7);
    // Heights 20..=52 over the padded node: layer 0 holds the ground, layer -1 is stone under
    // its top border row of ground, layer 1 is air over the highest step plus a cell.
    assert_eq!((column.lo, column.hi), (0, 1));
    assert_eq!(nodes.len(), 1);
    assert!(matches!(nodes[0], (0, jobs::Built::Mesh(..))));
}

#[test]
fn chunk_coverage_from_a_map_matches_the_published_meshes() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    for _ in 0..20 {
        app.update();
    }
    let map = app.world().resource::<ChunkMap>();
    let cov = ChunkCoverage::from_map(map);
    for (_, k, _) in map.ready_meshes() {
        let (p, bit) = octant_of_chunk(k.pos);
        assert!(cov.get(p).published & bit != 0 && cov.get(p).covered & bit != 0);
    }
    assert!(!cov.is_empty());
}

#[test]
fn each_level_is_planned_again_after_an_eighth_of_its_band() {
    let mut app = app(
        Steps::new(20),
        LodSettings {
            levels: 3,
            ..small()
        },
    );
    let viewer = spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 400);
    let eyes = |app: &App| app.world().resource::<LodWorld>().level_eyes;
    let start = eyes(&app);
    assert_eq!(plan::cadence(2), 8.0);
    assert_eq!(plan::cadence(3), 16.0);
    let walk = |app: &mut App, model: &mut Model, dx: f64| {
        app.world_mut()
            .entity_mut(viewer)
            .get_mut::<Transform>()
            .expect("viewer")
            .translation
            .x += dx;
        frame(app, model);
        assert_bits_from_scratch(app);
    };
    // 10 blocks: levels 1 and 2 again, level 3 (cadence 16) as it was.
    walk(&mut app, &mut model, 10.0);
    let now = eyes(&app);
    assert_ne!(now[2], start[2]);
    assert_eq!(now[1], now[2]);
    assert_eq!(now[3], start[3]);
    // 10 more: 20 from level 3's eye, so every level.
    walk(&mut app, &mut model, 10.0);
    let later = eyes(&app);
    assert_ne!(later[3], start[3]);
    assert!(later[1..=3].iter().all(|e| *e == later[3]));
}

// ---------------------------------------------------------------------------------------------
// Full-detail coverage: the feed, availability, the invariant

/// The level-`k` node holding chunk `c`, and the octant bit of `c` in it.
fn node_of_chunk(c: ChunkPos, k: u8) -> (ChunkKey, u8) {
    let at = |s: u8| ChunkPos::new(c.x >> s, c.y >> s, c.z >> s);
    let key = ChunkKey {
        dim: DimensionId(0),
        lod: k,
        pos: at(k),
    };
    let finer = ChunkKey {
        dim: DimensionId(0),
        lod: k - 1,
        pos: at(k - 1),
    };
    (key, tree::octant_bit(finer))
}

/// Who draws chunk `c` on the renderer model: full detail (published) and the LOD levels whose
/// node there shows `c`'s octant, those levels whose child toward `c` is lost apart (they may
/// draw over full detail).
fn drawers(app: &App, model: &Model, c: ChunkPos) -> (bool, Vec<u8>, Vec<u8>) {
    let lod = app.world().resource::<LodWorld>();
    let cov = app.world().resource::<LodCoverage>().coverage();
    let mut lods = Vec::new();
    let mut lost = Vec::new();
    for k in 1..=lod.tree.levels {
        let (key, bit) = node_of_chunk(c, k);
        if let Some(&(mask, _)) = model.nodes.get(&key)
            && mask & bit == 0
        {
            if k >= 2 && lod.tree.lost(node_of_chunk(c, k - 1).0) {
                lost.push(k);
            } else {
                lods.push(k);
            }
        }
    }
    (cov.published(c), lods, lost)
}

/// At most one of full detail and the LOD levels draws each chunk of `chunks` (a lost child's
/// parent may draw over full detail).
fn assert_at_most_one(app: &App, model: &Model, chunks: impl Iterator<Item = ChunkPos>) {
    for c in chunks {
        let (fd, lods, lost) = drawers(app, model, c);
        assert!(
            usize::from(fd) + lods.len() <= 1 && lods.len() + lost.len() <= 1,
            "{c:?} drawn by full detail {fd}, levels {lods:?} and over lost children {lost:?}"
        );
    }
}

/// Every published chunk is hidden from every LOD node on the renderer (but under a lost
/// child).
fn assert_published_hidden(app: &App, model: &Model) {
    let cov = app.world().resource::<LodCoverage>().coverage();
    let mut published = Vec::new();
    for p in cov.positions() {
        for o in 0..8 {
            let c = chunk_of_octant(p, o);
            if cov.published(c) {
                published.push(c);
            }
        }
    }
    for c in published {
        let (_, lods, _) = drawers(app, model, c);
        assert!(
            lods.is_empty(),
            "{c:?} published and drawn by levels {lods:?}"
        );
    }
}

/// The feed's coverage against a from-scratch one of the map: the same published chunks; covered
/// ones only where the map says so, and all of them in octants where a lod-1 node has quads
/// (elsewhere an empty mesh changes nothing drawn and is not looked for).
fn assert_feed_matches_map(app: &App) {
    let map = app.world().resource::<ChunkMap>();
    let lod = app.world().resource::<LodWorld>();
    let feed = app.world().resource::<LodCoverage>().coverage();
    let scratch = ChunkCoverage::from_map(map);
    let positions: foldhash::HashSet<ChunkPos> =
        feed.positions().chain(scratch.positions()).collect();
    for p in positions {
        let (a, b) = (feed.get(p), scratch.get(p));
        assert_eq!(a.published, b.published, "{p:?}: published");
        assert_eq!(a.covered & !b.covered, 0, "{p:?}: covered without a reason");
        let quads = lod.tree.lod1_octants(p);
        assert_eq!(
            a.covered & quads,
            b.covered & quads,
            "{p:?}: an empty chunk under LOD quads"
        );
    }
}

#[test]
fn the_coverage_follows_the_worlds_messages() {
    // Ground at 33 on one tile in five: the chunks under it are solid and enclosed (empty
    // meshes, no event), and the lod-1 skirts at the node's middle plane put quads in their
    // octant, so the poll has to find them.
    let mut app = app(Steps::new(33), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    for _ in 0..3 {
        frame(&mut app, &mut model);
    }
    assert_feed_matches_map(&app);
    assert_published_hidden(&app, &model);
    let map = app.world().resource::<ChunkMap>();
    let lod = app.world().resource::<LodWorld>();
    let cov = app.world().resource::<LodCoverage>().coverage();
    // Air over the ground: covered by its data.
    let air = ChunkPos::new(0, 2, 0);
    assert!(map.id_at(air).is_some() && cov.covered(air) && !cov.published(air));
    // Enclosed chunks with LOD quads in their octant: covered through the poll.
    let polled: Vec<ChunkPos> = map
        .iter()
        .filter(|(_, s)| {
            matches!(s.mesh(), crate::MeshState::Empty)
                && !matches!(s.data(), crate::DataState::Ready(d) if d.as_uniform() == Some(BlockState::AIR))
        })
        .map(|(_, s)| s.pos)
        .filter(|&c| {
            let (p, bit) = octant_of_chunk(c);
            lod.tree.lod1_octants(p) & bit != 0
        })
        .collect();
    assert!(
        !polled.is_empty(),
        "the scene has empty chunks under LOD quads"
    );
    for c in polled {
        assert!(cov.covered(c), "{c:?}");
    }
    // Still watched: loaded chunks without a mesh yet (the data ring), under LOD quads.
    let watched = app.world().resource::<LodCoverage>().watch_list();
    assert_eq!(
        watched.len(),
        app.world().resource::<LodStats>().watched_chunks
    );
    for &c in watched {
        let slot = map.get(map.id_at(c).expect("loaded")).expect("slot");
        assert!(
            !cov.covered(c) && matches!(slot.mesh(), crate::MeshState::None),
            "{c:?}"
        );
    }
    assert_bits_from_scratch(&app);
}

/// A lod-1 node on the renderer far from the loaded chunks, with no coverage.
fn free_lod1(app: &App, model: &Model) -> ChunkKey {
    let map = app.world().resource::<ChunkMap>();
    let cov = app.world().resource::<LodCoverage>().coverage();
    *model
        .nodes
        .keys()
        .filter(|k| {
            k.lod == 1
                && model.nodes.contains_key(&tree::parent(**k))
                && cov.get(k.pos) == Octants::default()
                && (0..8).all(|o| map.id_at(chunk_of_octant(k.pos, o)).is_none())
        })
        .min()
        .expect("a lod-1 node away from full detail")
}

fn upsert(c: ChunkPos) -> ChunkMeshEvent {
    ChunkMeshEvent::Upsert {
        id: crate::ChunkId::default(),
        key: ChunkKey {
            dim: DimensionId(0),
            lod: 0,
            pos: c,
        },
        mesh: Arc::new(ChunkMesh::default()),
    }
}

fn remove(c: ChunkPos) -> ChunkMeshEvent {
    ChunkMeshEvent::Remove {
        id: crate::ChunkId::default(),
        key: ChunkKey {
            dim: DimensionId(0),
            lod: 0,
            pos: c,
        },
    }
}

#[test]
fn upserts_written_between_frames_count_like_those_of_collect() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let key = free_lod1(&app, &model);
    let up = tree::parent(key);
    let c = chunk_of_octant(key.pos, 0b101);
    // Written outside `Update`, as the world's idle step does between frames.
    app.world_mut().write_message(upsert(c));
    frame(&mut app, &mut model);
    assert_eq!(model.nodes[&key].0, 1 << 0b101);
    let cov = app.world().resource::<LodCoverage>().coverage();
    assert!(cov.published(c));
    assert!(app.world().resource::<LodWorld>().tree.is_touched(key, cov));
    assert!(model.nodes[&up].0 & tree::octant_bit(key) != 0);
    app.world_mut().write_message(remove(c));
    frame(&mut app, &mut model);
    assert_eq!(model.nodes[&key].0, 0);
    assert_bits_from_scratch(&app);
}

#[test]
fn a_main_refusal_stops_counting_as_published_and_showing_again_restores_it() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let key = free_lod1(&app, &model);
    let c = chunk_of_octant(key.pos, 0b010);
    app.world_mut().write_message(upsert(c));
    frame(&mut app, &mut model);
    assert_eq!(model.nodes[&key].0, 0b100);
    // The refusal of that upload comes back two frames after its upsert.
    frame(&mut app, &mut model);
    app.world_mut()
        .resource_mut::<LodFeedback>()
        .chunks
        .push((c, true));
    frame(&mut app, &mut model);
    assert_eq!(model.nodes[&key].0, 0, "a refused chunk is not drawn");
    assert!(
        !app.world()
            .resource::<LodCoverage>()
            .coverage()
            .published(c)
    );
    app.world_mut()
        .resource_mut::<LodFeedback>()
        .chunks
        .push((c, false));
    frame(&mut app, &mut model);
    assert_eq!(model.nodes[&key].0, 0b100, "shown again");
    assert_bits_from_scratch(&app);
}

#[test]
fn a_chunk_mesh_resync_rebuilds_the_published_bits_from_the_ready_meshes() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    // A stale upsert of a chunk the world does not have, and a refused real one.
    let stale = chunk_of_octant(free_lod1(&app, &model).pos, 0);
    let (_, real, _) = app
        .world()
        .resource::<ChunkMap>()
        .ready_meshes()
        .next()
        .expect("a ready mesh");
    app.world_mut().write_message(upsert(stale));
    app.world_mut()
        .resource_mut::<LodFeedback>()
        .chunks
        .push((real.pos, true));
    frame(&mut app, &mut model);
    let cov = app.world().resource::<LodCoverage>().coverage();
    assert!(cov.published(stale) && !cov.published(real.pos));
    // The renderer dropped everything and uploaded the ready meshes again.
    app.world_mut().write_message(ChunkMeshResync);
    frame(&mut app, &mut model);
    let cov = app.world().resource::<LodCoverage>().coverage();
    assert!(!cov.published(stale) && cov.published(real.pos));
    assert_feed_matches_map(&app);
    assert_published_hidden(&app, &model);
    assert_bits_from_scratch(&app);
}

/// A world with no viewer (it loads nothing) and the LOD looking from `eye` through
/// [`LodFocus`], on `executor`.
fn focus_app(
    steps: Steps,
    settings: LodSettings,
    executor: Arc<dyn JobExecutor>,
    eye: I64Vec3,
) -> App {
    let mut app = app_with(steps, settings, executor, Arc::new(InlineExecutor));
    app.world_mut().insert_resource(LodFocus {
        origin: eye,
        eye_rel: Vec3::splat(0.5),
        forward: Vec3::X,
        fov_y: 70f32.to_radians(),
        aspect: 16.0 / 9.0,
    });
    app
}

#[test]
fn five_thousand_events_in_one_frame_are_applied_in_full() {
    let mut app = focus_app(
        Steps::new(20),
        small(),
        Arc::new(InlineExecutor),
        I64Vec3::new(0, 60, 0),
    );
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    // The budget only bounds the jobs; the events are all read and applied this frame.
    app.world_mut().resource_mut::<LodSettings>().frame_budget = Duration::from_nanos(1);
    let mut expect: HashMap<ChunkPos, bool> = HashMap::default();
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for _ in 0..5000 {
        let r = next();
        let c = ChunkPos::new(
            (r % 24) as i64 - 12,
            ((r >> 8) % 4) as i64,
            ((r >> 16) % 24) as i64 - 12,
        );
        let on = (r >> 32) % 3 != 0;
        app.world_mut()
            .write_message(if on { upsert(c) } else { remove(c) });
        expect.insert(c, on);
    }
    frame(&mut app, &mut model);
    let cov = app.world().resource::<LodCoverage>().coverage();
    for (&c, &on) in &expect {
        assert_eq!(cov.published(c), on, "{c:?}");
    }
    assert_eq!(
        cov.published_chunks(),
        expect.values().filter(|&&on| on).count()
    );
    assert_bits_from_scratch(&app);
    assert_published_hidden(&app, &model);
    assert_at_most_one(&app, &model, expect.keys().copied());
}

#[test]
fn a_lost_lod1_child_with_published_chunks_lets_its_parent_draw() {
    // Level-1 column (1, 0) fails; (3, 1) is refused later.
    let steps = Steps {
        panic_at: Some((1, [1, 0])),
        ..Steps::new(20)
    };
    let mut app = focus_app(
        steps,
        small(),
        Arc::new(InlineExecutor),
        I64Vec3::new(0, 60, 0),
    );
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let failed = ChunkKey {
        dim: DimensionId(0),
        lod: 1,
        pos: ChunkPos::new(1, 0, 0),
    };
    let up = tree::parent(failed);
    assert!(app.world().resource::<LodWorld>().tree.lost(failed));
    assert!(model.nodes.contains_key(&up));
    // A published chunk inside the failed node: its parent keeps drawing the octant.
    app.world_mut()
        .resource_mut::<LodCoverage>()
        .set(chunk_of_octant(failed.pos, 0), true, true);
    frame(&mut app, &mut model);
    assert_eq!(model.nodes[&up].0 & tree::octant_bit(failed), 0);
    // A sent lod-1 node with a published chunk, then refused: lost while it backs off.
    let refused = ChunkKey {
        dim: DimensionId(0),
        lod: 1,
        pos: ChunkPos::new(3, 0, 1),
    };
    assert!(model.nodes.contains_key(&refused), "{refused:?} sent");
    let up = tree::parent(refused);
    app.world_mut()
        .resource_mut::<LodCoverage>()
        .set(chunk_of_octant(refused.pos, 0), true, true);
    frame(&mut app, &mut model);
    assert!(model.nodes[&up].0 & tree::octant_bit(refused) != 0);
    {
        let mut feedback = app.world_mut().resource_mut::<LodFeedback>();
        feedback.refused.push(refused);
        feedback.arena_free_quads = Some(0);
    }
    model.nodes.remove(&refused);
    frame(&mut app, &mut model);
    assert_eq!(
        model.nodes[&up].0 & tree::octant_bit(refused),
        0,
        "the parent draws over the refused child"
    );
    let chunks: Vec<ChunkPos> = (0..8)
        .map(|o| chunk_of_octant(refused.pos, o))
        .chain((0..8).map(|o| chunk_of_octant(failed.pos, o)))
        .collect();
    assert_at_most_one(&app, &model, chunks.into_iter());
}

/// A column whose job fails after its band is known builds nothing more: a layer that enters its
/// wanted interval again is lost at once (its parent draws there), not pending for good under a
/// parent that hides it.
#[test]
fn layers_entering_a_failed_column_are_lost() {
    let steps = Steps::new(20);
    let panic_later = Arc::clone(&steps.panic_later);
    let mut app = focus_app(
        steps,
        small(),
        Arc::new(InlineExecutor),
        I64Vec3::new(0, 60, 0),
    );
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let key = ChunkKey {
        dim: DimensionId(0),
        lod: 1,
        pos: ChunkPos::new(1, 0, 0),
    };
    let up = tree::parent(key);
    assert!(model.nodes.contains_key(&key) && model.nodes.contains_key(&up));
    // A published chunk inside it: its parent hides the octant unless the node is lost.
    app.world_mut()
        .resource_mut::<LodCoverage>()
        .set(chunk_of_octant(key.pos, 0), true, true);
    frame(&mut app, &mut model);
    *panic_later.lock().expect("lock") = Some((1, [key.pos.x, key.pos.z]));
    let go = |app: &mut App, model: &mut Model, y: i64| {
        app.world_mut().resource_mut::<LodFocus>().origin.y = y;
        fill(app, model, 300);
    };
    // Down until the layer leaves the wanted tree (the column keeps the layers under it), and
    // back: its next job panics.
    go(&mut app, &mut model, -700);
    assert!(!app.world().resource::<LodWorld>().tree.wanted(key));
    go(&mut app, &mut model, 60);
    let tree = &app.world().resource::<LodWorld>().tree;
    let col = &tree.columns[&ColumnKey::of(key)];
    assert!(col.failed && col.band.is_some() && tree.lost(key));
    assert_eq!(model.nodes[&up].0 & tree::octant_bit(key), 0);
    // Once more: the layer comes back lost, not waiting for a job that never runs.
    go(&mut app, &mut model, -700);
    go(&mut app, &mut model, 60);
    let tree = &app.world().resource::<LodWorld>().tree;
    assert!(tree.lost(key), "{:?}", tree.node(key));
    assert_eq!(
        model.nodes[&up].0 & tree::octant_bit(key),
        0,
        "the parent draws over the failed child"
    );
    assert_eq!(app.world().resource::<LodStats>().levels[0].pending, 0);
    assert_bits_from_scratch(&app);
}

#[test]
fn a_touched_octant_waits_hidden_for_its_lod1_node_which_jumps_ahead() {
    let steps = Steps::new(20);
    let calls = Arc::clone(&steps.calls);
    let lod = Arc::new(Hold::default());
    let mut app = focus_app(steps, small(), lod.clone(), I64Vec3::new(0, 60, 0));
    let mut model = Model::default();
    // Until the top level is all sent and level 1 is under way.
    let mut ready = false;
    for _ in 0..400 {
        lod.run_all();
        frame(&mut app, &mut model);
        let stats = app.world().resource::<LodStats>();
        if stats.levels[1].pending == 0
            && stats.levels[1].nodes > 0
            && stats.sends_waiting == 0
            && stats.levels[0].nodes > 4
        {
            ready = true;
            break;
        }
    }
    assert!(ready, "{:?}", app.world().resource::<LodStats>());
    // A lod-1 column still waiting in the queue, under a sent parent.
    let state = app.world().resource::<LodWorld>();
    let (c, key) = state
        .tree
        .columns
        .iter()
        .filter(|(c, col)| c.level == 1 && col.ticket.is_some())
        .find_map(|(&c, col)| {
            let work = col.work()?;
            let y = work.layers().find(|&y| {
                let key = c.node(DimensionId(0), y);
                model.nodes.contains_key(&tree::parent(key)) && !state.tree.known_uniform(key)
            })?;
            Some((c, c.node(DimensionId(0), y)))
        })
        .expect("a queued lod-1 column under a sent parent");
    let up = tree::parent(key);
    assert_eq!(model.nodes[&up].0 & tree::octant_bit(key), 0);
    app.world_mut()
        .resource_mut::<LodCoverage>()
        .set(chunk_of_octant(key.pos, 3), true, true);
    frame(&mut app, &mut model);
    // A short hole rather than drawing over full detail.
    assert!(model.nodes[&up].0 & tree::octant_bit(key) != 0);
    assert_eq!(queued_class(&app, c), 0, "touched: the first class");
    // It goes next.
    let before = calls.lock().expect("lock").len();
    lod.run_all();
    frame(&mut app, &mut model);
    lod.run_all();
    let after = calls.lock().expect("lock")[before..].to_vec();
    assert!(after.contains(&(1, [c.x, c.z])), "{after:?}");
    fill_held(&mut app, &lod, &mut model);
    assert!(model.nodes.contains_key(&key));
    assert!(model.nodes[&up].0 & tree::octant_bit(key) != 0);
    assert_bits_from_scratch(&app);
}

/// The class of the queued entry of column `c`, or of its job handed off to the workers (a
/// class-0 column goes there at once; no job of a lower class is ahead of it then).
fn queued_class(app: &App, c: ColumnKey) -> u8 {
    let state = app.world().resource::<LodWorld>();
    let line = state.jobs.handed_off_columns();
    if let Some(at) = line.iter().position(|&(column, _)| column == c) {
        assert!(line[..at].iter().all(|&(_, class)| class == 0), "{line:?}");
        return line[at].1;
    }
    let ticket = state.tree.columns[&c].ticket.expect("queued");
    state
        .jobs
        .queue
        .iter()
        .find(|e| e.column == c && e.ticket == ticket)
        .expect("its entry")
        .class
}

/// A lod-1 column under full detail is a placeholder: queued last (class 3), however much full
/// detail it holds. One that fills a hole (published chunks next to uncovered ones, so its parent
/// hides that octant until it is sent) jumps to class 0.
#[test]
fn placeholders_under_full_detail_go_last_and_holes_first() {
    let lod = Arc::new(Hold::default());
    let mut app = focus_app(Steps::new(20), small(), lod.clone(), I64Vec3::new(0, 60, 0));
    let mut model = Model::default();
    // Until the top level is sent and level 1 is under way.
    for _ in 0..400 {
        lod.run_all();
        frame(&mut app, &mut model);
        let stats = app.world().resource::<LodStats>();
        if stats.levels[1].pending == 0 && stats.sends_waiting == 0 && stats.levels[0].nodes > 4 {
            break;
        }
    }
    // A queued lod-1 column that wants layer 0, its band not known yet.
    let state = app.world().resource::<LodWorld>();
    let c = *state
        .tree
        .columns
        .iter()
        .filter(|(c, col)| {
            c.level == 1
                && col.ticket.is_some()
                && col.band.is_none()
                && contains(col.plan.wanted, 0)
        })
        .map(|(c, _)| c)
        .min()
        .expect("a queued lod-1 column");
    let key = c.node(DimensionId(0), 0);
    let set = |app: &mut App, octants: u8| {
        let mut cov = app.world_mut().resource_mut::<LodCoverage>();
        for o in 0..8 {
            cov.set(
                chunk_of_octant(key.pos, o),
                octants & (1 << o) != 0,
                octants & (1 << o) != 0,
            );
        }
    };
    set(&mut app, 0xFF);
    frame(&mut app, &mut model);
    assert_eq!(queued_class(&app, c), 3, "full detail covers it");
    set(&mut app, 0x0F);
    frame(&mut app, &mut model);
    assert_eq!(queued_class(&app, c), 0, "a hole next to full detail");
}

/// The main renderer refused a chunk; the world sends its mesh again: the LOD counts it as drawn
/// in that frame, as any upsert (no frame of LOD over it while the refusal is taken back). A
/// refusal older than that upsert (of the upload before it) changes nothing; the refusal of the
/// new upload, a frame later, counts.
#[test]
fn a_refused_chunk_sent_again_counts_as_drawn_at_once() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let key = free_lod1(&app, &model);
    let c = chunk_of_octant(key.pos, 0b010);
    let refuse = |app: &mut App| {
        app.world_mut()
            .resource_mut::<LodFeedback>()
            .chunks
            .push((c, true));
    };
    app.world_mut().write_message(upsert(c));
    frame(&mut app, &mut model);
    frame(&mut app, &mut model);
    refuse(&mut app);
    frame(&mut app, &mut model);
    assert_eq!(model.nodes[&key].0, 0, "refused: the LOD draws it");
    app.world_mut().write_message(upsert(c));
    frame(&mut app, &mut model);
    assert_eq!(
        model.nodes[&key].0, 0b100,
        "sent again: drawn at full detail"
    );
    refuse(&mut app);
    frame(&mut app, &mut model);
    assert_eq!(
        model.nodes[&key].0, 0b100,
        "a refusal older than the upsert"
    );
    refuse(&mut app);
    frame(&mut app, &mut model);
    assert_eq!(model.nodes[&key].0, 0, "the new upload refused");
    assert_bits_from_scratch(&app);
}

/// A renderer that cannot draw the LOD (a GL backend, a failed smoke check or LOD pass) turns
/// the world side off: no node kept, no job; it comes back, from scratch, once one can.
#[test]
fn a_renderer_that_cannot_draw_turns_the_lod_off() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let n = model.nodes.len();
    app.world_mut().resource_mut::<LodFeedback>().cannot_draw = true;
    for _ in 0..5 {
        frame(&mut app, &mut model);
    }
    let stats = app.world().resource::<LodStats>();
    assert_eq!(stats.off, Some(LodOff::Unsupported));
    assert!(model.nodes.is_empty());
    assert_eq!(
        (stats.nodes(), stats.columns(), stats.queued_jobs),
        (0, 0, 0)
    );
    assert!(!app.world().resource::<LodWorld>().has_job_work());
    app.world_mut().resource_mut::<LodFeedback>().cannot_draw = false;
    fill(&mut app, &mut model, 300);
    assert_eq!(model.nodes.len(), n);
}

/// Frames running every held job until the LOD is filled.
fn fill_held(app: &mut App, lod: &Hold, model: &mut Model) {
    for _ in 0..1000 {
        lod.run_all();
        frame(app, model);
        let stats = app.world().resource::<LodStats>();
        if stats.fill >= 1.0 && stats.in_flight == 0 && stats.sends_waiting == 0 && lod.held() == 0
        {
            return;
        }
    }
    panic!("not filled: {:?}", app.world().resource::<LodStats>());
}

#[test]
fn a_refused_parent_takes_its_children_off_until_it_is_back() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let children = |model: &Model, key: ChunkKey| {
        model
            .nodes
            .keys()
            .filter(|k| k.lod == 1 && tree::parent(**k) == key)
            .count()
    };
    let key = *model
        .nodes
        .keys()
        .filter(|k| k.lod == 2)
        .max_by_key(|k| children(&model, **k))
        .expect("a level-2 node");
    let n = children(&model, key);
    assert!(n > 0);
    {
        let mut feedback = app.world_mut().resource_mut::<LodFeedback>();
        feedback.refused.push(key);
        feedback.arena_free_quads = Some(0);
    }
    model.nodes.remove(&key);
    frame(&mut app, &mut model);
    assert_eq!(
        children(&model, key),
        0,
        "nothing finer shows without its parent"
    );
    for _ in 0..10 {
        frame(&mut app, &mut model);
        assert!(!model.nodes.contains_key(&key));
    }
    app.world_mut()
        .resource_mut::<LodFeedback>()
        .arena_free_quads = Some(1 << 30);
    app.world_mut()
        .resource_mut::<LodWorld>()
        .tree
        .expire_refusals();
    frame(&mut app, &mut model);
    assert!(model.nodes.contains_key(&key));
    assert_eq!(children(&model, key), n, "the children come back with it");
    assert_bits_from_scratch(&app);
}

/// One step of the coverage invariant's random runs.
#[derive(Clone, Debug)]
enum Step {
    /// Publish chunk `n` of the near set.
    Publish(u16),
    /// Mark it as drawing nothing (an empty mesh).
    Empty(u16),
    /// Forget it (unloaded).
    Clear(u16),
    /// Run the held jobs picked by these bits.
    Jobs(u32),
    /// The renderer refuses node `n` on it.
    Refuse(u16),
    /// The arena has room again and the back-off is over.
    Room,
    /// The renderer lost every node.
    Resync,
    /// The eye moves by (16 x, 8 y, 16 z) blocks.
    Move(i8, i8, i8),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => any::<u16>().prop_map(Step::Publish),
        2 => any::<u16>().prop_map(Step::Empty),
        2 => any::<u16>().prop_map(Step::Clear),
        10 => any::<u32>().prop_map(Step::Jobs),
        1 => any::<u16>().prop_map(Step::Refuse),
        1 => Just(Step::Room),
        1 => Just(Step::Resync),
        2 => (-3i8..=3, -1i8..=1, -3i8..=3).prop_map(|(x, y, z)| Step::Move(x, y, z)),
    ]
}

/// The chunks a run publishes: 13 x 13 columns of layers 0 and 1 around the origin, inside
/// the wanted lod-1 nodes wherever the eye goes (it stays within 64 blocks of its start).
fn near_chunk(n: u16) -> ChunkPos {
    let n = i64::from(n) % (13 * 13 * 2);
    ChunkPos::new(n % 13 - 6, n / 169, (n / 13) % 13 - 6)
}

/// The chunks the invariant is checked on: around the near set and above and below it.
fn sample() -> impl Iterator<Item = ChunkPos> {
    (-1..=2).flat_map(|y| (-8..=8).flat_map(move |z| (-8..=8).map(move |x| ChunkPos::new(x, y, z))))
}

/// Once everything is built, sent and published: exactly one of full detail and the LOD draws
/// each chunk, or none where it draws nothing (covered and not published, or the finest wanted
/// node there holds nothing).
fn assert_exactly_one(app: &App, model: &Model) {
    let lod = app.world().resource::<LodWorld>();
    let cov = app.world().resource::<LodCoverage>().coverage();
    for c in sample() {
        let (fd, lods, lost) = drawers(app, model, c);
        assert!(lost.is_empty(), "{c:?}: lost children once all is built");
        if cov.covered(c) {
            assert!(
                lods.is_empty(),
                "{c:?} covered (published {fd}) and drawn by {lods:?}"
            );
            continue;
        }
        let finest = (1..=lod.tree.levels)
            .map(|k| node_of_chunk(c, k).0)
            .find(|&key| lod.tree.wanted(key));
        let draws = finest.is_some_and(|key| {
            lod.tree
                .node(key)
                .is_some_and(|n| matches!(n.state, tree::NodeState::Ready { .. }))
        });
        assert_eq!(
            lods.len(),
            usize::from(draws),
            "{c:?}: drawn by {lods:?}, finest wanted {finest:?}"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// The core property (ADR 0031): for random coverage changes, job completion orders,
    /// refusals, resyncs and eye moves, every chunk is drawn by at most one of full detail and
    /// the LOD levels, every frame (a lost child's parent may draw over full detail); with
    /// everything available, exactly one (or none where nothing is there).
    #[test]
    fn full_detail_and_the_lod_levels_never_draw_the_same_chunk(
        steps in proptest::collection::vec(step(), 1..60),
    ) {
        let lod = Arc::new(Hold::default());
        let start = I64Vec3::new(0, 60, 0);
        // Three levels: an orphaned lod-1 node would show under a level-3 grandparent. Eight
        // jobs in flight, the top level built first: the random part completes the finer
        // levels' jobs in any order.
        let settings = LodSettings {
            levels: 3,
            in_flight_busy: 8,
            in_flight_settled: 8,
            ..small()
        };
        let mut app = focus_app(Steps::new(20), settings, lod.clone(), start);
        let mut model = Model::default();
        let top_built = |app: &App| {
            let tree = &app.world().resource::<LodWorld>().tree;
            let mut top = tree.columns.iter().filter(|(c, _)| c.level == 3).peekable();
            top.peek().is_some() && top.all(|(_, col)| col.band.is_some())
        };
        for _ in 0..400 {
            lod.run_all();
            frame(&mut app, &mut model);
            if top_built(&app) {
                break;
            }
        }
        prop_assert!(top_built(&app));
        let mut eye = start;
        for step in steps {
            match step {
                Step::Publish(n) => app.world_mut().resource_mut::<LodCoverage>().set(near_chunk(n), true, true),
                Step::Empty(n) => app.world_mut().resource_mut::<LodCoverage>().set(near_chunk(n), true, false),
                Step::Clear(n) => app.world_mut().resource_mut::<LodCoverage>().set(near_chunk(n), false, false),
                Step::Jobs(pick) => lod.run_some(pick),
                Step::Refuse(n) => {
                    let mut keys: Vec<ChunkKey> = model.nodes.keys().copied().collect();
                    keys.sort_unstable();
                    if let Some(&key) = keys.get(usize::from(n) % keys.len().max(1)) {
                        model.nodes.remove(&key);
                        let mut feedback = app.world_mut().resource_mut::<LodFeedback>();
                        feedback.refused.push(key);
                        feedback.arena_free_quads = Some(0);
                    }
                }
                Step::Room => {
                    app.world_mut().resource_mut::<LodFeedback>().arena_free_quads = Some(1 << 30);
                    app.world_mut().resource_mut::<LodWorld>().tree.expire_refusals();
                }
                Step::Resync => {
                    app.world_mut().write_message(LodResync);
                    model = Model::default();
                }
                Step::Move(x, y, z) => {
                    let to = eye + I64Vec3::new(i64::from(x) * 16, i64::from(y) * 8, i64::from(z) * 16);
                    eye = I64Vec3::new(to.x.clamp(-64, 64), to.y.clamp(20, 100), to.z.clamp(-64, 64));
                    app.world_mut().resource_mut::<LodFocus>().origin = eye;
                }
            }
            frame(&mut app, &mut model);
            assert_at_most_one(&app, &model, sample());
        }
        app.world_mut().resource_mut::<LodFeedback>().arena_free_quads = Some(1 << 30);
        app.world_mut().resource_mut::<LodWorld>().tree.expire_refusals();
        fill_held(&mut app, &lod, &mut model);
        assert_exactly_one(&app, &model);
        assert_bits_from_scratch(&app);
    }
}

/// The coverage with real streaming: the demo forest, a viewer flying over it, the LOD on the
/// same inline executor. Every frame the renderer model (the world's mesh events and the LOD's
/// ops) is checked: every published chunk's octant is hidden at every level (no drawn level-2
/// and up octant contains a published chunk, no lod-1 octant draws over one), the feed's
/// published chunks are the events', and the masks and the chain rule hold.
fn fly_over_the_forest(viewer: ChunkViewer, bounds: bool) {
    let generator = boxel_demo::worldgen::ForestGenerator::default();
    let vertical = boxel_worldgen::WorldGenerator::vertical_chunks(&generator);
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(generator),
        registry: Arc::new(boxel_demo::blocks::registry()),
        settings: WorldSettings {
            vertical_bounds: bounds.then_some(vertical),
            ..WorldSettings::default()
        },
        executor: Arc::new(InlineExecutor),
    });
    app.add_plugins(LodWorldPlugin {
        settings: small(),
        executor: Arc::new(InlineExecutor),
    });
    let entity = app
        .world_mut()
        .spawn((
            Transform::from_translation(DVec3::new(248.0, 95.0, -256.0)),
            viewer,
        ))
        .id();
    let mut model = Model::default();
    let mut published: foldhash::HashSet<ChunkPos> = foldhash::HashSet::default();
    let (mut upserts, mut removes) = (0, 0);
    // Published chunks under a node on the renderer, per level (the checks are not vacuous).
    let mut under = [0usize; 3];
    let mut cursor = app
        .world()
        .resource::<Messages<ChunkMeshEvent>>()
        .get_cursor();
    for i in 0..70 {
        if i >= 20 {
            // 16 blocks a frame along x, a little up and down: new columns every other frame.
            let dy = if i % 10 < 5 { 2.0 } else { -2.0 };
            app.world_mut()
                .entity_mut(entity)
                .get_mut::<Transform>()
                .expect("viewer")
                .translation += DVec3::new(16.0, dy, 4.0);
        }
        frame(&mut app, &mut model);
        for event in cursor.read(app.world().resource::<Messages<ChunkMeshEvent>>()) {
            match event {
                ChunkMeshEvent::Upsert { key, .. } => {
                    upserts += 1;
                    published.insert(key.pos)
                }
                ChunkMeshEvent::Remove { key, .. } => {
                    removes += 1;
                    published.remove(&key.pos)
                }
            };
        }
        let cov = app.world().resource::<LodCoverage>().coverage();
        assert_eq!(cov.published_chunks(), published.len(), "frame {i}");
        for &c in &published {
            assert!(cov.published(c), "frame {i}: {c:?}");
            for k in 1..=2 {
                under[usize::from(k)] +=
                    usize::from(model.nodes.contains_key(&node_of_chunk(c, k).0));
            }
        }
        assert_published_hidden(&app, &model);
    }
    let stats = app.world().resource::<LodStats>();
    assert!(stats.off.is_none() && stats.drawn() > 0, "{stats:?}");
    assert!(
        upserts > 100 && removes > 10,
        "{upserts} upserts and {removes} removes"
    );
    assert!(under[1] > 100 && under[2] > 100, "{under:?}");
    assert_feed_matches_map(&app);
    assert_bits_from_scratch(&app);
}

#[test]
fn the_coverage_follows_real_streaming_over_the_forest() {
    fly_over_the_forest(ChunkViewer::new(3).with_vertical_radius(2), false);
}

#[test]
fn the_coverage_follows_real_streaming_with_a_fixed_height_world() {
    // `--fixed-height`: every layer of the generator, the vertical radius unbounded.
    fly_over_the_forest(ChunkViewer::new(3).with_vertical_radius(u32::MAX), true);
}

#[test]
fn a_parent_and_its_child_refused_in_one_frame_leave_cleanly() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 300);
    let child = *model
        .nodes
        .keys()
        .filter(|k| k.lod == 1 && model.nodes.contains_key(&tree::parent(**k)))
        .min()
        .expect("a lod-1 node under a sent parent");
    let up = tree::parent(child);
    {
        let mut feedback = app.world_mut().resource_mut::<LodFeedback>();
        feedback.refused.extend([up, child]);
        feedback.arena_free_quads = Some(0);
    }
    // The renderer took neither: no op may name them (the model panics on a removal of a node
    // it does not hold).
    model.nodes.remove(&up);
    model.nodes.remove(&child);
    frame(&mut app, &mut model);
    assert!(!model.nodes.keys().any(|k| tree::parent(*k) == up));
    app.world_mut()
        .resource_mut::<LodFeedback>()
        .arena_free_quads = Some(1 << 30);
    app.world_mut()
        .resource_mut::<LodWorld>()
        .tree
        .expire_refusals();
    fill(&mut app, &mut model, 50);
    assert!(model.nodes.contains_key(&up) && model.nodes.contains_key(&child));
    assert_eq!(app.world().resource::<LodStats>().refused, 2);
    assert_bits_from_scratch(&app);
}
