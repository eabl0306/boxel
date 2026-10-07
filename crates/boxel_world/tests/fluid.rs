//! Flowing water (ADR 0021) in a headless app against a brute-force simulator written here from
//! Minecraft's rules: one cell at a time on a small grid, every tick of a game tick in schedule
//! order, no chunks, no budget, no overlay. The engine must match it cell for cell after every
//! game tick: spread distance, the way down, falling, new sources, draining. Also the tick
//! budget (it only delays), ticks frozen in unloaded chunks, worldgen water that stays still,
//! and the same scenes far from the origin.
use boxel_app::App;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos,
    LocalPos, MAX_BLOCK_COORD,
};
use boxel_world::jobs::InlineExecutor;
use boxel_world::{
    BlockTickSettings, BlockTicks, ChunkMap, ChunkViewer, EditQueue, FluidPlugin, WorldPlugin,
    WorldSettings, WorldStats,
};
use boxel_worldgen::WorldGenerator;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ the simulator

/// A cell of the simulator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Cell {
    Air,
    Stone,
    Plant,
    /// Water of this `level` (0 source, 1..=7 flowing, 8..=15 falling).
    Water(u8),
}

/// Neighbor offsets in the order a change notifies them (the block, then +y, -y, +x, -x, +z,
/// -z: `Face::ALL`).
const NOTIFY: [[i64; 3]; 7] = [
    [0, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [1, 0, 0],
    [-1, 0, 0],
    [0, 0, 1],
    [0, 0, -1],
];
/// Minecraft's horizontal order: north (-z), east (+x), south (+z), west (-x).
const SIDES: [[i64; 3]; 4] = [[0, 0, -1], [1, 0, 0], [0, 0, 1], [-1, 0, 0]];
/// The order spreads are written in (Minecraft's `EnumMap` of directions): north, south, west,
/// east.
const WRITE_ORDER: [[i64; 3]; 4] = [[0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]];
const DOWN: [i64; 3] = [0, -1, 0];
const UP: [i64; 3] = [0, 1, 0];

fn add(a: [i64; 3], b: [i64; 3]) -> [i64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn neg(a: [i64; 3]) -> [i64; 3] {
    [-a[0], -a[1], -a[2]]
}

/// What a water cell holds: (amount, source, falling).
fn water(cell: Cell) -> Option<(u8, bool, bool)> {
    match cell {
        Cell::Water(0) => Some((8, true, false)),
        Cell::Water(l @ 1..=7) => Some((8 - l, false, false)),
        Cell::Water(_) => Some((8, false, true)),
        _ => None,
    }
}

struct Sim {
    cells: HashMap<[i64; 3], Cell>,
    now: u64,
    order: u64,
    queue: BTreeMap<(u64, u64), [i64; 3]>,
    waiting: HashSet<[i64; 3]>,
}

impl Sim {
    /// Stone below y = 0, air above.
    fn new() -> Self {
        Self {
            cells: HashMap::new(),
            now: 0,
            order: 0,
            queue: BTreeMap::new(),
            waiting: HashSet::new(),
        }
    }

    fn get(&self, p: [i64; 3]) -> Cell {
        self.cells
            .get(&p)
            .copied()
            .unwrap_or(if p[1] < 0 { Cell::Stone } else { Cell::Air })
    }

    fn schedule(&mut self, p: [i64; 3], due: u64) {
        if self.waiting.insert(p) {
            self.queue.insert((due, self.order), p);
            self.order += 1;
        }
    }

    fn notify(&mut self, p: [i64; 3], at: u64) {
        for d in NOTIFY {
            let q = add(p, d);
            if matches!(self.get(q), Cell::Water(_)) {
                self.schedule(q, at + 5);
            }
        }
    }

    /// A tick's write.
    fn write(&mut self, p: [i64; 3], cell: Cell, at: u64) {
        if self.get(p) != cell {
            self.cells.insert(p, cell);
            self.notify(p, at);
        }
    }

    /// Edits from outside (a player), applied together, then notified at the current tick.
    fn edit(&mut self, edits: &[([i64; 3], Cell)]) {
        let mut changed = Vec::new();
        for &(p, cell) in edits {
            if self.get(p) != cell {
                changed.push(p);
            }
            self.cells.insert(p, cell);
        }
        for p in changed {
            self.notify(p, self.now);
        }
    }

    /// One game tick: every tick due at it, in order.
    fn step(&mut self) {
        self.now += 1;
        while let Some(entry) = self.queue.first_entry() {
            if entry.key().0 > self.now {
                break;
            }
            let ((due, _), p) = entry.remove_entry();
            self.waiting.remove(&p);
            if let Some(w) = water(self.get(p)) {
                self.tick(p, w, due);
            }
        }
    }

    fn holds(&self, cell: Cell) -> bool {
        matches!(cell, Cell::Air | Cell::Plant | Cell::Water(_))
    }

    fn source(&self, cell: Cell) -> bool {
        cell == Cell::Water(0)
    }

    /// `getNewLiquid`.
    fn new_liquid(&self, p: [i64; 3]) -> Option<Cell> {
        let mut most = 0;
        let mut sources = 0;
        for d in SIDES {
            if let Some((amount, source, _)) = water(self.get(add(p, d))) {
                most = most.max(amount);
                sources += u32::from(source);
            }
        }
        let below = self.get(add(p, DOWN));
        if sources >= 2 && matches!(below, Cell::Stone | Cell::Water(0)) {
            return Some(Cell::Water(0));
        }
        if matches!(self.get(add(p, UP)), Cell::Water(_)) {
            return Some(Cell::Water(8));
        }
        (most >= 2).then(|| Cell::Water(8 - (most - 1)))
    }

    fn hole(&self, below: Cell) -> bool {
        self.holds(below)
    }

    fn passes(&self, cell: Cell) -> bool {
        !self.source(cell) && self.holds(cell)
    }

    fn empty_and_holds(&self, cell: Cell) -> bool {
        matches!(cell, Cell::Air | Cell::Plant)
    }

    fn slope(&self, p: [i64; 3], depth: u32, from: [i64; 3]) -> u32 {
        let mut best = 1000;
        for d in SIDES {
            if d == from {
                continue;
            }
            let q = add(p, d);
            if !self.passes(self.get(q)) {
                continue;
            }
            if self.hole(self.get(add(q, DOWN))) {
                return depth;
            }
            if depth < 4 {
                best = best.min(self.slope(q, depth + 1, neg(d)));
            }
        }
        best
    }

    fn sides(&mut self, p: [i64; 3], amount: u8, falling: bool, due: u64) {
        let left = if falling { 7 } else { amount - 1 };
        if left == 0 {
            return;
        }
        let mut best = 1000;
        let mut targets: Vec<([i64; 3], Option<Cell>)> = Vec::new();
        for d in SIDES {
            let q = add(p, d);
            if !self.passes(self.get(q)) {
                continue;
            }
            let new = self.new_liquid(q);
            let distance = if self.hole(self.get(add(q, DOWN))) {
                0
            } else {
                self.slope(q, 1, neg(d))
            };
            if distance < best {
                targets.clear();
            }
            if distance <= best {
                targets.push((d, new));
                best = distance;
            }
        }
        for d in WRITE_ORDER {
            let Some(&(_, new)) = targets.iter().find(|(t, _)| *t == d) else {
                continue;
            };
            let q = add(p, d);
            if let Some(new) = new
                && self.empty_and_holds(self.get(q))
            {
                self.write(q, new, due);
            }
        }
    }

    fn tick(&mut self, p: [i64; 3], (mut amount, source, mut falling): (u8, bool, bool), due: u64) {
        if !source {
            match self.new_liquid(p) {
                None => {
                    self.write(p, Cell::Air, due);
                    return;
                }
                Some(new) => {
                    if new != self.get(p) {
                        self.write(p, new, due);
                    }
                    (amount, _, falling) = water(new).unwrap();
                }
            }
        }
        let source = self.get(p) == Cell::Water(0);
        let below = add(p, DOWN);
        if self.empty_and_holds(self.get(below)) {
            if let Some(new) = self.new_liquid(below) {
                self.write(below, new, due);
            }
            let sources = SIDES
                .iter()
                .filter(|&&d| self.source(self.get(add(p, d))))
                .count();
            if sources >= 3 {
                self.sides(p, amount, falling, due);
            }
        } else if source || !self.hole(self.get(below)) {
            self.sides(p, amount, falling, due);
        }
    }
}

// ------------------------------------------------------------------ the engine

/// Stone below `origin.y`, air above, and some blocks placed by the generator (worldgen water).
struct Ground {
    origin: BlockPos,
    placed: Vec<(BlockPos, BlockState)>,
}

impl WorldGenerator for Ground {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let stone = registry.by_name("boxel:stone").unwrap();
        let o = key.pos.origin();
        let size = i64::from(CHUNK_SIZE);
        let inside: Vec<_> = self
            .placed
            .iter()
            .filter(|(p, _)| ChunkPos::containing(*p) == key.pos)
            .collect();
        if inside.is_empty() {
            if o.y >= self.origin.y {
                return ChunkData::uniform(BlockState::AIR);
            }
            if o.y + size <= self.origin.y {
                return ChunkData::uniform(stone);
            }
        }
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        for (i, slot) in dense.iter_mut().enumerate() {
            if o.y + i64::from(LocalPos::from_index(i).y()) < self.origin.y {
                *slot = stone;
            }
        }
        for (p, state) in inside {
            dense[p.split().1.index()] = *state;
        }
        ChunkData::from_dense(&dense)
    }
    fn name(&self) -> &str {
        "ground"
    }
}

struct Engine {
    app: App,
    origin: BlockPos,
    registry: Arc<BlockRegistry>,
    viewer: Entity,
}

impl Engine {
    /// The world around `origin` (the ground's top is `origin.y - 1`) with the viewer there.
    fn new(origin: BlockPos, placed: &[([i64; 3], Cell)]) -> Self {
        let registry = Arc::new(BlockRegistry::default_blocks());
        let mut app = App::new();
        let placed = placed
            .iter()
            .map(|&(p, c)| (at(origin, p), state_of(&registry, c)))
            .collect();
        app.add_plugins(WorldPlugin {
            generator: Arc::new(Ground { origin, placed }),
            registry: Arc::clone(&registry),
            executor: Arc::new(InlineExecutor),
            settings: WorldSettings {
                light: false,
                ..WorldSettings::default()
            },
        })
        .add_plugins(FluidPlugin);
        let viewer = app
            .world_mut()
            .spawn((
                Transform::from_translation(DVec3::new(
                    origin.x as f64,
                    origin.y as f64,
                    origin.z as f64,
                )),
                ChunkViewer::new(1).with_vertical_radius(1),
            ))
            .id();
        let mut engine = Self {
            app,
            origin,
            registry,
            viewer,
        };
        engine.settle();
        engine
    }

    fn settle(&mut self) {
        let start = Instant::now();
        loop {
            self.app.update();
            let stats = *self.app.world().resource::<WorldStats>();
            if stats.chunks > 0 && stats.is_settled() {
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "not settled");
        }
    }

    fn edit(&mut self, edits: &[([i64; 3], Cell)]) {
        let mut queue = self.app.world_mut().resource_mut::<EditQueue>();
        for &(p, c) in edits {
            let state = state_of(&self.registry, c);
            queue.set_block(at(self.origin, p), state);
        }
        self.app.update();
    }

    /// One game tick and one frame.
    fn step(&mut self) {
        self.app.world_mut().resource_mut::<BlockTicks>().advance(1);
        self.app.update();
    }

    fn get(&self, p: [i64; 3]) -> Cell {
        let map = self.app.world().resource::<ChunkMap>();
        let state = map.block_at(at(self.origin, p)).expect("loaded");
        cell_of(&self.registry, state)
    }

    fn ticks(&self) -> &BlockTicks {
        self.app.world().resource::<BlockTicks>()
    }
}

fn at(origin: BlockPos, p: [i64; 3]) -> BlockPos {
    BlockPos::new(origin.x + p[0], origin.y + p[1], origin.z + p[2])
}

fn state_of(registry: &BlockRegistry, cell: Cell) -> BlockState {
    match cell {
        Cell::Air => BlockState::AIR,
        Cell::Stone => registry.by_name("boxel:stone").unwrap(),
        Cell::Plant => registry.by_name("boxel:short_grass").unwrap(),
        Cell::Water(l) => registry
            .parse_state(&format!("boxel:water[level={l}]"))
            .unwrap(),
    }
}

fn cell_of(registry: &BlockRegistry, state: BlockState) -> Cell {
    match registry.name(state) {
        "boxel:air" => Cell::Air,
        "boxel:stone" => Cell::Stone,
        "boxel:short_grass" => Cell::Plant,
        "boxel:water" => Cell::Water(registry.property(state, "level").unwrap().parse().unwrap()),
        other => panic!("unexpected {other}"),
    }
}

/// The cells compared: 12 blocks around the origin, 5 down and 4 up.
fn region() -> impl Iterator<Item = [i64; 3]> {
    (-5..=4).flat_map(|y| (-12..=12).flat_map(move |x| (-12..=12).map(move |z| [x, y, z])))
}

fn assert_same(engine: &Engine, sim: &Sim, what: &str) {
    for p in region() {
        assert_eq!(
            engine.get(p),
            sim.get(p),
            "{what}: cell {p:?} at game tick {}",
            sim.now
        );
    }
}

/// Runs `edits` at tick 0 (and `later` edits at their ticks) in both, comparing every game tick
/// for `ticks` ticks. Returns both.
fn run(
    origin: BlockPos,
    what: &str,
    edits: &[([i64; 3], Cell)],
    later: &[(u64, Vec<([i64; 3], Cell)>)],
    ticks: u64,
) -> (Engine, Sim) {
    let mut engine = Engine::new(origin, &[]);
    let mut sim = Sim::new();
    engine.edit(edits);
    sim.edit(edits);
    for t in 1..=ticks {
        engine.step();
        sim.step();
        if let Some((_, edits)) = later.iter().find(|(at, _)| *at == t) {
            engine.edit(edits);
            sim.edit(edits);
        }
        assert_same(&engine, &sim, what);
    }
    (engine, sim)
}

/// A pool in the ground, a step and a cave: pits east of the origin.
fn pits() -> Vec<([i64; 3], Cell)> {
    let mut edits = vec![([3, -1, 0], Cell::Air), ([3, -2, 0], Cell::Air)];
    // a cave at the pit's bottom running east
    for x in 3..=8 {
        edits.push(([x, -3, 0], Cell::Air));
    }
    // a plant the water washes away, west
    edits.push(([-2, 0, 1], Cell::Plant));
    edits
}

const ORIGIN: BlockPos = BlockPos::new(16, 40, -7);

#[test]
fn a_source_spreads_seven_blocks_on_flat_ground() {
    let (engine, _) = run(ORIGIN, "flat", &[([0, 0, 0], Cell::Water(0))], &[], 60);
    for p in region().filter(|p| p[1] == 0) {
        let d = p[0].abs() + p[2].abs();
        let want = if d <= 7 {
            Cell::Water(d as u8)
        } else {
            Cell::Air
        };
        assert_eq!(engine.get(p), want, "{p:?}");
    }
    assert_eq!(
        engine.ticks().stats().queued,
        0,
        "settled water schedules nothing"
    );
}

#[test]
fn water_takes_the_way_down_falls_and_spreads_again() {
    let mut edits = pits();
    edits.push(([0, 0, 0], Cell::Water(0)));
    let (engine, _) = run(ORIGIN, "pits", &edits, &[], 120);
    // only towards the pit: nothing flows west, north or south of the source
    for p in [[-1, 0, 0], [0, 0, 1], [0, 0, -1]] {
        assert_eq!(engine.get(p), Cell::Air, "{p:?}");
    }
    assert_eq!(engine.get([1, 0, 0]), Cell::Water(1));
    assert_eq!(engine.get([3, -1, 0]), Cell::Water(8), "falling");
    assert_eq!(
        engine.get([3, -3, 0]),
        Cell::Water(8),
        "falling at the bottom"
    );
    assert_eq!(engine.get([4, -3, 0]), Cell::Water(1), "spreads again");
    assert_eq!(engine.get([-2, 0, 1]), Cell::Plant, "the plant stays dry");
}

#[test]
fn two_sources_make_a_third_and_water_drains_without_its_source() {
    let sources = [([0, 0, 0], Cell::Water(0)), ([2, 0, 0], Cell::Water(0))];
    let (engine, _) = run(ORIGIN, "infinite", &sources, &[], 30);
    assert_eq!(engine.get([1, 0, 0]), Cell::Water(0), "a new source");
    // A source in a dry field, then gone: everything drains (and the plant it reached is gone).
    let drain = [(80, vec![([0, 0, 0], Cell::Air)])];
    let plant = [([0, 0, 0], Cell::Water(0)), ([2, 0, 1], Cell::Plant)];
    let (engine, _) = run(ORIGIN, "drain", &plant, &drain, 200);
    for p in region() {
        assert!(
            matches!(engine.get(p), Cell::Air | Cell::Stone),
            "{p:?} still holds {:?}",
            engine.get(p)
        );
    }
    assert_eq!(engine.ticks().stats().queued, 0);
}

#[test]
fn the_same_scenes_far_from_the_origin() {
    let mut edits = pits();
    edits.push(([0, 0, 0], Cell::Water(0)));
    edits.push(([-4, 1, -3], Cell::Water(0)));
    for origin in [
        BlockPos::new((1 << 31) - 2, -(1 << 31) + 3, -(1 << 31) + 1),
        BlockPos::new(1 << 40, 1 << 40, -(1 << 40)),
        BlockPos::new(
            MAX_BLOCK_COORD - 1024,
            -MAX_BLOCK_COORD + 1024,
            MAX_BLOCK_COORD - 1024,
        ),
    ] {
        run(origin, &format!("far {origin:?}"), &edits, &[], 70);
    }
}

#[test]
fn the_budget_only_delays_the_flood() {
    let mut edits = pits();
    edits.push(([0, 0, 0], Cell::Water(0)));
    edits.push(([-3, 2, 2], Cell::Water(0)));
    let mut engine = Engine::new(ORIGIN, &[]);
    *engine.app.world_mut().resource_mut::<BlockTickSettings>() = BlockTickSettings {
        budget: Duration::ZERO,
        min_ticks_per_frame: 2,
        paused: false,
    };
    let mut sim = Sim::new();
    engine.edit(&edits);
    sim.edit(&edits);
    for _ in 0..200 {
        sim.step();
    }
    assert!(sim.queue.is_empty(), "the simulator settles");
    let mut behind = 0;
    let mut frames = 0;
    while frames < 200 || engine.ticks().stats().queued > 0 {
        engine.step();
        behind = behind.max(engine.ticks().stats().behind);
        frames += 1;
        assert!(frames < 5000, "the flood never settles");
    }
    assert!(behind > 10, "the budget never held ticks back ({behind})");
    assert_same(&engine, &sim, "budget");
}

#[test]
fn ticks_wait_in_unloaded_chunks_and_resume() {
    // The viewer's data reaches chunk x = 2 (radius 1, data one further): x = 96 is not loaded.
    let origin = BlockPos::new(16, 40, 16);
    let mut engine = Engine::new(origin, &[]);
    let map = engine.app.world().resource::<ChunkMap>();
    assert!(
        map.data_at(ChunkPos::new(2, 1, 0)).is_some(),
        "chunk 2 loaded"
    );
    assert!(
        map.data_at(ChunkPos::new(3, 1, 0)).is_none(),
        "chunk 3 not loaded"
    );
    // A source 3 blocks from the unloaded chunk: its tick reads 5 blocks to the sides.
    let source = [96 - 3 - 16, 0, 0];
    engine.edit(&[(source, Cell::Water(0))]);
    for _ in 0..20 {
        engine.step();
    }
    assert_eq!(engine.ticks().stats().frozen, 1);
    assert_eq!(
        engine.get(add(source, [1, 0, 0])),
        Cell::Air,
        "nothing flowed"
    );
    // The viewer moves east: the chunk loads, the tick runs and the water spreads.
    engine
        .app
        .world_mut()
        .entity_mut(engine.viewer)
        .insert(Transform::from_translation(DVec3::new(48.0, 40.0, 16.0)));
    engine.settle();
    for _ in 0..60 {
        engine.step();
    }
    let stats = engine.ticks().stats();
    assert_eq!((stats.frozen, stats.queued), (0, 0), "{stats:?}");
    for d in 0..=7 {
        assert_eq!(
            engine.get(add(source, [d, 0, 0])),
            Cell::Water(d as u8),
            "{d}"
        );
    }
    assert_eq!(engine.get(add(source, [8, 0, 0])), Cell::Air);
}

#[test]
fn worldgen_water_is_still_until_an_edit_touches_it() {
    // A generated source on open ground: water next to air that never flows by itself.
    let mut engine = Engine::new(ORIGIN, &[([0, 0, 0], Cell::Water(0))]);
    for _ in 0..40 {
        engine.step();
    }
    assert_eq!(engine.ticks().stats().queued, 0);
    assert_eq!(engine.get([1, 0, 0]), Cell::Air);
    // A block placed next to it wakes it up.
    engine.edit(&[([0, 1, 0], Cell::Stone)]);
    for _ in 0..40 {
        engine.step();
    }
    assert_eq!(engine.get([1, 0, 0]), Cell::Water(1));
    assert_eq!(engine.get([0, 0, 7]), Cell::Water(7));
}

/// SplitMix64.
fn mix(mut v: u64) -> u64 {
    v = v.wrapping_add(0x9E37_79B9_7F4A_7C15);
    v = (v ^ (v >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    v ^ (v >> 31)
}

#[test]
fn random_terrain_with_edits_matches_the_simulator() {
    for seed in 0..6u64 {
        let mut rng = mix(seed);
        let mut roll = |n: u64| {
            rng = mix(rng);
            rng % n
        };
        let mut edits = Vec::new();
        for x in -9..=9 {
            for z in -9..=9 {
                match roll(100) {
                    0..12 => edits.push(([x, -1, z], Cell::Air)),
                    12..16 => {
                        edits.push(([x, -1, z], Cell::Air));
                        edits.push(([x, -2, z], Cell::Air));
                    }
                    16..24 => edits.push(([x, 0, z], Cell::Stone)),
                    24..28 => edits.push(([x, 0, z], Cell::Plant)),
                    _ => {}
                }
            }
        }
        let mut sources = Vec::new();
        for _ in 0..3 {
            let p = [roll(13) as i64 - 6, roll(3) as i64, roll(13) as i64 - 6];
            sources.push(p);
            edits.push((p, Cell::Water(0)));
        }
        // Later: a source goes, a wall goes up in the flow, a block next to the water breaks.
        let later = [
            (35, vec![(sources[0], Cell::Air)]),
            (50, vec![([1, 0, 1], Cell::Stone), ([2, 0, 1], Cell::Stone)]),
            (65, vec![([0, -1, 0], Cell::Air), ([-3, 0, -2], Cell::Air)]),
        ];
        run(ORIGIN, &format!("seed {seed}"), &edits, &later, 130);
    }
}
