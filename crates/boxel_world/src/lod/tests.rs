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
        assert!(
            self.panic_at != Some((req.level, req.column)),
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
    plan::plan(eye, th, &mut Plan::default(), th.levels())
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
    plan::plan(eye, th, &mut p, th.levels())
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
    let top = app.world().resource::<LodWorld>().tree.levels;
    model.check(top);
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

/// The incremental covered bits equal a from-scratch walk (bits of parents known to be uniform
/// are never read and may be stale).
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
                lod.tree.node(k).map(|n| (n.wanted, n.available())),
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
    // Without full detail every lod-1 node draws; level-2 nodes hide what level 1 draws.
    assert_eq!(stats.levels[0].drawn, stats.levels[0].nodes);
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
    assert_eq!(lod.jobs.in_flight, 0, "every dropped job reported");
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
    // Every node on the renderer is wanted, or departed and waiting for its parent.
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
    let mut renderer = Model::default();
    fill(&mut app, &mut renderer, 300);
    assert_eq!(renderer.nodes.len(), before);
    let levels: Vec<u8> = renderer.upserts.iter().map(|k| k.lod).collect();
    assert!(
        levels.windows(2).all(|w| w[0] >= w[1]),
        "coarse to fine: {levels:?}"
    );
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
    // A level-1 node whose parent is on the renderer.
    let key = *model
        .nodes
        .keys()
        .find(|k| k.lod == 1 && model.nodes.contains_key(&tree::parent(**k)))
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
    let key = *model
        .nodes
        .keys()
        .find(|k| k.lod == 1)
        .expect("a lod-1 node");
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
    assert_eq!(stats.levels[0].drawn, stats.levels[0].nodes - 1);
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
        let expect = if entry.column.level == 3 {
            0
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
    assert!(matches!(nodes[0], (0, jobs::Built::Mesh(_))));
}

#[test]
fn chunk_coverage_from_a_map_matches_the_published_meshes() {
    let mut app = app(Steps::new(20), small());
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    for _ in 0..20 {
        app.update();
    }
    let map = app.world().resource::<crate::ChunkMap>();
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
