//! Flowing water (ADR 0021) and lava (ADR 0025) in a headless app against a brute-force
//! simulator written here from Minecraft's rules: one cell at a time on a small grid, every tick
//! of a game tick in schedule order, no chunks, no budget, no overlay. The engine must match it
//! cell for cell after every game tick: spread distance, the way down, falling, new sources,
//! draining, lava's slower and shorter flow, lava meeting water (obsidian, cobblestone, stone),
//! and waterlogged slabs and fences (sources that flow out through their open faces, never
//! filled by flowing water, waterlogged by a new source between two others). Also the tick budget (it only delays), ticks frozen in unloaded
//! chunks, worldgen water that stays still, and the same scenes far from the origin.
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
    /// Lava of this `level`.
    Lava(u8),
    Cobble,
    Obsidian,
    /// A stone slab, its top half or its bottom one, maybe waterlogged (`wet`).
    Slab {
        top: bool,
        wet: bool,
    },
    /// An oak fence, maybe waterlogged.
    Fence {
        wet: bool,
    },
}

/// The fluids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Water,
    Lava,
}

impl Kind {
    fn delay(self) -> u64 {
        match self {
            Kind::Water => 5,
            Kind::Lava => 30,
        }
    }
    fn slope(self) -> u32 {
        match self {
            Kind::Water => 4,
            Kind::Lava => 2,
        }
    }
    fn drop_off(self) -> u8 {
        match self {
            Kind::Water => 1,
            Kind::Lava => 2,
        }
    }
    fn cell(self, level: u8) -> Cell {
        match self {
            Kind::Water => Cell::Water(level),
            Kind::Lava => Cell::Lava(level),
        }
    }
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

/// A fluid's (amount, source, falling) at `level`.
fn amounts(level: u8) -> (u8, bool, bool) {
    match level {
        0 => (8, true, false),
        l @ 1..=7 => (8 - l, false, false),
        _ => (8, false, true),
    }
}

/// The fluid a cell holds: its kind and (amount, source, falling). A waterlogged block holds a
/// water source.
fn content(cell: Cell) -> Option<(Kind, (u8, bool, bool))> {
    match cell {
        Cell::Water(l) => Some((Kind::Water, amounts(l))),
        Cell::Lava(l) => Some((Kind::Lava, amounts(l))),
        Cell::Slab { wet: true, .. } | Cell::Fence { wet: true } => Some((Kind::Water, amounts(0))),
        _ => None,
    }
}

/// What a cell's collision covers of its face toward `d`: (lower half, upper half), both for
/// a full face. Slabs cover their half of the sides and their whole bottom or top; a fence
/// post covers no whole half of any face.
fn cover(cell: Cell, d: [i64; 3]) -> (bool, bool) {
    match cell {
        Cell::Stone | Cell::Cobble | Cell::Obsidian => (true, true),
        Cell::Slab { top, .. } => match d[1] {
            0 => (!top, top),
            -1 => (!top, !top),
            _ => (top, top),
        },
        _ => (false, false),
    }
}

/// A dry block that takes a water source (`canPlaceLiquid`), as `kind`.
fn takes(kind: Kind, cell: Cell) -> bool {
    kind == Kind::Water
        && matches!(
            cell,
            Cell::Slab { wet: false, .. } | Cell::Fence { wet: false }
        )
}

/// The block with its water.
fn wet(cell: Cell) -> Cell {
    match cell {
        Cell::Slab { top, .. } => Cell::Slab { top, wet: true },
        Cell::Fence { .. } => Cell::Fence { wet: true },
        other => other,
    }
}

/// `canPassThroughWall` from `a` to its neighbor `b` across `d`.
fn wall_open(a: Cell, b: Cell, d: [i64; 3]) -> bool {
    let ((al, au), (bl, bu)) = (cover(a, d), cover(b, neg(d)));
    !((al || bl) && (au || bu))
}

struct Sim {
    cells: HashMap<[i64; 3], Cell>,
    now: u64,
    order: u64,
    queue: BTreeMap<(u64, u64), [i64; 3]>,
    waiting: HashSet<[i64; 3]>,
    /// Edits next to lava, for the reactions of the next step.
    reactions: Vec<([i64; 3], u64)>,
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
            reactions: Vec::new(),
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

    /// A tick's change at `p`: each fluid cell at or next to it reacts (lava) and, unless that
    /// changed it, is scheduled.
    fn notify(&mut self, p: [i64; 3], at: u64) {
        for d in NOTIFY {
            let q = add(p, d);
            let cell = self.get(q);
            let Some((kind, _)) = content(cell) else {
                continue;
            };
            if kind == Kind::Lava {
                self.react(q, at);
                if self.get(q) != cell {
                    continue;
                }
            }
            self.schedule(q, at + kind.delay());
        }
    }

    /// Lava with water above or beside hardens: obsidian from a source, cobblestone otherwise.
    fn react(&mut self, p: [i64; 3], at: u64) {
        let Cell::Lava(l) = self.get(p) else {
            return;
        };
        let wet = [UP, [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]]
            .iter()
            .any(|&d| content(self.get(add(p, d))).is_some_and(|(k, _)| k == Kind::Water));
        if wet {
            let product = if l == 0 { Cell::Obsidian } else { Cell::Cobble };
            self.write(p, product, at);
        }
    }

    /// A tick's write.
    fn write(&mut self, p: [i64; 3], cell: Cell, at: u64) {
        if self.get(p) != cell {
            self.cells.insert(p, cell);
            self.notify(p, at);
        }
    }

    /// Edits from outside (a player), applied together, then notified at the current tick; the
    /// lava around them reacts at the start of the next step.
    fn edit(&mut self, edits: &[([i64; 3], Cell)]) {
        let mut changed = Vec::new();
        for &(p, cell) in edits {
            if self.get(p) != cell {
                changed.push(p);
            }
            self.cells.insert(p, cell);
        }
        for p in changed {
            let mut lava = false;
            for d in NOTIFY {
                let q = add(p, d);
                if let Some((kind, _)) = content(self.get(q)) {
                    lava |= kind == Kind::Lava;
                    self.schedule(q, self.now + kind.delay());
                }
            }
            if lava {
                self.reactions.push((p, self.now));
            }
        }
    }

    /// One game tick: the reactions to the last edits, then every tick due at it, in order.
    fn step(&mut self) {
        self.now += 1;
        for (p, at) in std::mem::take(&mut self.reactions) {
            for d in NOTIFY {
                self.react(add(p, d), at);
            }
        }
        while let Some(entry) = self.queue.first_entry() {
            if entry.key().0 > self.now {
                break;
            }
            let ((due, _), p) = entry.remove_entry();
            self.waiting.remove(&p);
            if let Some((kind, fluid)) = content(self.get(p)) {
                self.tick(kind, p, fluid, due);
            }
        }
    }

    /// Nothing in it stops movement.
    fn holds(&self, cell: Cell) -> bool {
        matches!(
            cell,
            Cell::Air | Cell::Plant | Cell::Water(_) | Cell::Lava(_)
        )
    }

    fn fluid(&self, kind: Kind, cell: Cell) -> Option<(u8, bool, bool)> {
        content(cell).filter(|(k, _)| *k == kind).map(|(_, f)| f)
    }

    fn source(&self, kind: Kind, cell: Cell) -> bool {
        self.fluid(kind, cell).is_some_and(|f| f.1)
    }

    /// What `cell` holds gives way to `kind` flowing in (`down`: from above).
    fn replaceable(&self, kind: Kind, cell: Cell, down: bool) -> bool {
        match content(cell) {
            None => true,
            Some((Kind::Water, _)) => down && kind == Kind::Lava,
            Some((Kind::Lava, (amount, _, _))) => kind == Kind::Water && amount >= 4,
        }
    }

    /// `canHoldFluid` for `new` (flowing when `None`).
    fn holds_new(&self, kind: Kind, cell: Cell, new: Option<Cell>) -> bool {
        self.holds(cell) || takes(kind, cell) && new == Some(Cell::Water(0))
    }

    /// `canSpreadTo` from `from` into its neighbor `to` across `d`, as `new`.
    fn spreads_to(&self, kind: Kind, from: Cell, to: Cell, d: [i64; 3], new: Option<Cell>) -> bool {
        self.replaceable(kind, to, d == DOWN)
            && self.holds_new(kind, to, new)
            && wall_open(from, to, d)
    }

    /// `getNewLiquid`.
    fn new_liquid(&self, kind: Kind, p: [i64; 3]) -> Option<Cell> {
        let here = self.get(p);
        let mut most = 0;
        let mut sources = 0;
        for d in SIDES {
            let n = self.get(add(p, d));
            if let Some((amount, source, _)) = self.fluid(kind, n)
                && wall_open(here, n, d)
            {
                most = most.max(amount);
                sources += u32::from(source);
            }
        }
        let below = self.get(add(p, DOWN));
        let solid_below = matches!(
            below,
            Cell::Stone | Cell::Cobble | Cell::Obsidian | Cell::Slab { .. } | Cell::Fence { .. }
        );
        if kind == Kind::Water && sources >= 2 && (solid_below || self.source(kind, below)) {
            return Some(Cell::Water(0));
        }
        let above = self.get(add(p, UP));
        if self.fluid(kind, above).is_some() && wall_open(here, above, UP) {
            return Some(kind.cell(8));
        }
        let left = most.saturating_sub(kind.drop_off());
        (left > 0).then(|| kind.cell(8 - left))
    }

    fn hole(&self, kind: Kind, from: Cell, below: Cell) -> bool {
        (self.fluid(kind, below).is_some() || self.holds(below)) && wall_open(from, below, DOWN)
    }

    fn passes(&self, kind: Kind, from: Cell, to: Cell, d: [i64; 3], new: Option<Cell>) -> bool {
        !self.source(kind, to) && self.holds_new(kind, to, new) && wall_open(from, to, d)
    }

    fn slope(&self, kind: Kind, p: [i64; 3], depth: u32, from: [i64; 3]) -> u32 {
        let here = self.get(p);
        let mut best = 1000;
        for d in SIDES {
            if d == from {
                continue;
            }
            let q = add(p, d);
            let to = self.get(q);
            if !self.passes(kind, here, to, d, None) {
                continue;
            }
            if self.hole(kind, to, self.get(add(q, DOWN))) {
                return depth;
            }
            if depth < kind.slope() {
                best = best.min(self.slope(kind, q, depth + 1, neg(d)));
            }
        }
        best
    }

    /// `spreadTo`: lava falling into water makes stone; a dry block takes a water source.
    fn spread_to(&mut self, kind: Kind, q: [i64; 3], new: Option<Cell>, down: bool, due: u64) {
        let cell = self.get(q);
        if down && kind == Kind::Lava && matches!(cell, Cell::Water(_)) {
            self.write(q, Cell::Stone, due);
            return;
        }
        if takes(kind, cell) {
            if new == Some(Cell::Water(0)) {
                self.write(q, wet(cell), due);
            }
            return;
        }
        if let Some(new) = new {
            self.write(q, new, due);
        }
    }

    fn sides(&mut self, kind: Kind, p: [i64; 3], (amount, _, falling): (u8, bool, bool), due: u64) {
        let left = if falling {
            7
        } else {
            amount.saturating_sub(kind.drop_off())
        };
        if left == 0 {
            return;
        }
        let here = self.get(p);
        let mut best = 1000;
        let mut targets: Vec<([i64; 3], Option<Cell>)> = Vec::new();
        for d in SIDES {
            let q = add(p, d);
            let to = self.get(q);
            let new = self.new_liquid(kind, q);
            if !self.passes(kind, here, to, d, new) {
                continue;
            }
            let distance = if self.hole(kind, to, self.get(add(q, DOWN))) {
                0
            } else {
                self.slope(kind, q, 1, neg(d))
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
            if self.spreads_to(kind, self.get(p), self.get(q), d, new) {
                self.spread_to(kind, q, new, false, due);
            }
        }
    }

    fn tick(&mut self, kind: Kind, p: [i64; 3], mut fluid: (u8, bool, bool), due: u64) {
        if !fluid.1 {
            match self.new_liquid(kind, p) {
                None => {
                    self.write(p, Cell::Air, due);
                    return;
                }
                Some(new) => {
                    if new != self.get(p) {
                        self.write(p, new, due);
                    }
                    fluid = content(new).unwrap().1;
                }
            }
        }
        let here = self.get(p);
        let below = add(p, DOWN);
        let below_cell = self.get(below);
        let new = self.new_liquid(kind, below);
        if self.spreads_to(kind, here, below_cell, DOWN, new) {
            self.spread_to(kind, below, new, true, due);
            let sources = SIDES
                .iter()
                .filter(|&&d| self.source(kind, self.get(add(p, d))))
                .count();
            if sources >= 3 {
                self.sides(kind, p, fluid, due);
            }
        } else if fluid.1 || !self.hole(kind, here, below_cell) {
            self.sides(kind, p, fluid, due);
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
        // Every due tick runs in its frame, however slow the machine: the comparisons with the
        // simulator are per game tick (the budget has its own test).
        *app.world_mut().resource_mut::<BlockTickSettings>() = BlockTickSettings {
            min_ticks_per_frame: usize::MAX,
            ..BlockTickSettings::default()
        };
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
        Cell::Lava(l) => registry
            .parse_state(&format!("boxel:lava[level={l}]"))
            .unwrap(),
        Cell::Cobble => registry.by_name("boxel:cobblestone").unwrap(),
        Cell::Obsidian => registry.by_name("boxel:obsidian").unwrap(),
        Cell::Slab { top, wet } => registry
            .parse_state(&format!(
                "boxel:stone_slab[type={},waterlogged={wet}]",
                if top { "top" } else { "bottom" }
            ))
            .unwrap(),
        Cell::Fence { wet } => registry
            .parse_state(&format!("boxel:oak_fence[waterlogged={wet}]"))
            .unwrap(),
    }
}

fn cell_of(registry: &BlockRegistry, state: BlockState) -> Cell {
    let level = || registry.property(state, "level").unwrap().parse().unwrap();
    match registry.name(state) {
        "boxel:air" => Cell::Air,
        "boxel:stone" => Cell::Stone,
        "boxel:short_grass" => Cell::Plant,
        "boxel:water" => Cell::Water(level()),
        "boxel:lava" => Cell::Lava(level()),
        "boxel:cobblestone" => Cell::Cobble,
        "boxel:obsidian" => Cell::Obsidian,
        "boxel:stone_slab" => Cell::Slab {
            top: registry.property(state, "type") == Some("top"),
            wet: registry.is_waterlogged(state),
        },
        // Any sides: the world connects fences to their neighbors.
        "boxel:oak_fence" => Cell::Fence {
            wet: registry.is_waterlogged(state),
        },
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
    edits.push(([-6, 0, 5], Cell::Lava(0)));
    // A waterlogged top slab in the air and a dry fence between two sources.
    edits.push((
        [6, 2, -6],
        Cell::Slab {
            top: true,
            wet: true,
        },
    ));
    edits.push(([-7, 0, -8], Cell::Water(0)));
    edits.push(([-7, 0, -7], Cell::Fence { wet: false }));
    edits.push(([-7, 0, -6], Cell::Water(0)));
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
    edits.push(([-5, 0, -2], Cell::Lava(0)));
    let mut engine = Engine::new(ORIGIN, &[]);
    *engine.app.world_mut().resource_mut::<BlockTickSettings>() = BlockTickSettings {
        budget: Duration::ZERO,
        min_ticks_per_frame: 2,
        paused: false,
    };
    let mut sim = Sim::new();
    engine.edit(&edits);
    sim.edit(&edits);
    for _ in 0..300 {
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

/// `SplitMix64`.
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

#[test]
fn lava_spreads_three_blocks_with_a_step_of_two() {
    let (engine, _) = run(ORIGIN, "lava flat", &[([0, 0, 0], Cell::Lava(0))], &[], 160);
    for p in region().filter(|p| p[1] == 0) {
        let d = p[0].abs() + p[2].abs();
        let want = if d <= 3 {
            Cell::Lava(2 * d as u8)
        } else {
            Cell::Air
        };
        assert_eq!(engine.get(p), want, "{p:?}");
    }
    assert_eq!(
        engine.ticks().stats().queued,
        0,
        "settled lava schedules nothing"
    );
}

#[test]
fn lava_takes_the_way_down_and_drains_without_its_source() {
    let mut edits = pits();
    edits.push(([0, 0, 0], Cell::Lava(0)));
    let drain = [(200, vec![([0, 0, 0], Cell::Air)])];
    let (engine, _) = run(ORIGIN, "lava pits", &edits, &drain, 600);
    for p in region() {
        assert!(
            !matches!(engine.get(p), Cell::Lava(_)),
            "{p:?} still holds {:?}",
            engine.get(p)
        );
    }
}

#[test]
fn lava_meeting_water_makes_obsidian_cobblestone_and_stone() {
    // Water placed beside a lava source: obsidian at once (the next game tick).
    let later = [(3, vec![([1, 0, 0], Cell::Water(0))])];
    let (engine, _) = run(
        ORIGIN,
        "obsidian",
        &[([0, 0, 0], Cell::Lava(0))],
        &later,
        12,
    );
    assert_eq!(engine.get([0, 0, 0]), Cell::Obsidian);
    // Flowing water and flowing lava meet in a walled channel: cobblestone where flowing lava
    // touches water, obsidian where water reaches the lava source.
    let mut channel = Vec::new();
    for x in -8..=8 {
        channel.push(([x, 0, -1], Cell::Stone));
        channel.push(([x, 0, 1], Cell::Stone));
    }
    channel.push(([-4, 0, 0], Cell::Lava(0)));
    channel.push(([4, 0, 0], Cell::Water(0)));
    let (engine, _) = run(ORIGIN, "channel", &channel, &[], 160);
    let made: Vec<Cell> = (-8..=8).map(|x| engine.get([x, 0, 0])).collect();
    assert!(made.contains(&Cell::Cobble), "{made:?}");
    // Lava falling into a pool of water: stone.
    let pool = [([0, -1, 0], Cell::Water(0)), ([0, 3, 0], Cell::Lava(0))];
    let (engine, _) = run(ORIGIN, "fall", &pool, &[], 130);
    assert_eq!(engine.get([0, -1, 0]), Cell::Stone, "the pool hardened");
    // Water falling onto a lava source in a pit: obsidian.
    let pit = [([0, -1, 0], Cell::Lava(0)), ([0, 2, 0], Cell::Water(0))];
    let (engine, _) = run(ORIGIN, "pour", &pit, &[], 30);
    assert_eq!(engine.get([0, -1, 0]), Cell::Obsidian);
}

#[test]
fn random_terrain_with_water_and_lava_matches_the_simulator() {
    for seed in 0..4u64 {
        let mut rng = mix(seed ^ 0x6c61_7661);
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
        let mut place = |cell: Cell, edits: &mut Vec<_>| {
            let p = [roll(13) as i64 - 6, roll(3) as i64, roll(13) as i64 - 6];
            edits.push((p, cell));
            p
        };
        let lava = [
            place(Cell::Lava(0), &mut edits),
            place(Cell::Lava(0), &mut edits),
        ];
        place(Cell::Water(0), &mut edits);
        place(Cell::Water(0), &mut edits);
        // Later: a lava source goes, water is poured next to the other, a block breaks.
        let later = [
            (70, vec![(lava[0], Cell::Air)]),
            (100, vec![(add(lava[1], [0, 1, 0]), Cell::Water(0))]),
            (130, vec![([0, -1, 0], Cell::Air), ([-3, 0, -2], Cell::Air)]),
        ];
        run(ORIGIN, &format!("lava seed {seed}"), &edits, &later, 260);
    }
}

#[test]
fn waterlogged_blocks_are_sources_that_flow_out_of_their_open_faces() {
    let wet_bottom = Cell::Slab {
        top: false,
        wet: true,
    };
    let wet_top = Cell::Slab {
        top: true,
        wet: true,
    };
    // A waterlogged bottom slab on the ground: a source, 7 blocks to every side.
    let (engine, _) = run(ORIGIN, "slab", &[([0, 0, 0], wet_bottom)], &[], 60);
    assert_eq!(
        engine.get([0, 0, 0]),
        wet_bottom,
        "the slab keeps its water"
    );
    for d in 1..=7 {
        assert_eq!(engine.get([d, 0, 0]), Cell::Water(d as u8), "{d}");
    }
    assert_eq!(engine.get([8, 0, 0]), Cell::Air);
    // Hanging in the air: a bottom slab's water never goes through its closed bottom; a top
    // slab's falls first and, once the water under it stands, spreads to the sides like any
    // source over water.
    let hanging = [([-4, 3, 0], wet_bottom), ([4, 3, 0], wet_top)];
    let mut engine = Engine::new(ORIGIN, &[]);
    let mut sim = Sim::new();
    engine.edit(&hanging);
    sim.edit(&hanging);
    for t in 1..=40 {
        engine.step();
        sim.step();
        assert_same(&engine, &sim, "hanging");
        if t == 5 {
            assert_eq!(engine.get([-3, 3, 0]), Cell::Water(1), "sideways at once");
            assert_eq!(engine.get([4, 2, 0]), Cell::Water(8), "down at once");
            assert_eq!(engine.get([5, 3, 0]), Cell::Air, "not sideways yet");
        }
    }
    assert_eq!(
        engine.get([-4, 2, 0]),
        Cell::Air,
        "nothing under the bottom slab"
    );
    assert_eq!(engine.get([5, 3, 0]), Cell::Water(1));
    // A fence holds water like a slab; its open faces let it out everywhere.
    let (engine, _) = run(
        ORIGIN,
        "fence",
        &[([0, 2, 0], Cell::Fence { wet: true })],
        &[],
        30,
    );
    assert_eq!(engine.get([0, 1, 0]), Cell::Water(8));
}

#[test]
fn flowing_water_never_fills_a_block_and_lava_hardens_next_to_a_wet_one() {
    let dry = Cell::Slab {
        top: false,
        wet: false,
    };
    let edits = [
        ([0, 0, 0], Cell::Water(0)),
        ([2, 0, 0], dry),
        ([0, 0, 2], Cell::Fence { wet: false }),
        ([-6, 0, 0], Cell::Lava(0)),
        ([-5, 0, 0], Cell::Fence { wet: true }),
    ];
    let (engine, _) = run(ORIGIN, "dry", &edits, &[], 60);
    assert_eq!(engine.get([2, 0, 0]), dry);
    assert_eq!(engine.get([0, 0, 2]), Cell::Fence { wet: false });
    assert_eq!(engine.get([3, 0, 0]), Cell::Water(5), "around the slab");
    assert_eq!(engine.get([-6, 0, 0]), Cell::Obsidian);
}

#[test]
fn two_sources_waterlog_the_dry_block_between_them() {
    let dry = Cell::Slab {
        top: false,
        wet: false,
    };
    // Between two sources on the ground a dry slab or fence takes the new source; next to one
    // source it stays dry, with flowing water around it.
    let edits = [
        ([-1, 0, 0], Cell::Water(0)),
        ([0, 0, 0], dry),
        ([1, 0, 0], Cell::Water(0)),
        ([-1, 0, 4], Cell::Water(0)),
        ([0, 0, 4], Cell::Fence { wet: false }),
        ([1, 0, 4], Cell::Water(0)),
        ([0, 0, -6], dry),
        ([1, 0, -6], Cell::Water(0)),
    ];
    let (engine, _) = run(ORIGIN, "two sources", &edits, &[], 30);
    assert_eq!(
        engine.get([0, 0, 0]),
        Cell::Slab {
            top: false,
            wet: true
        }
    );
    assert_eq!(engine.get([0, 0, 4]), Cell::Fence { wet: true });
    assert_eq!(engine.get([0, 0, -6]), dry, "one source is not enough");
}

#[test]
fn random_terrain_with_waterlogged_blocks_matches_the_simulator() {
    for seed in 0..4u64 {
        let mut rng = mix(seed ^ 0x7765_74);
        let mut roll = |n: u64| {
            rng = mix(rng);
            rng % n
        };
        let mut edits = Vec::new();
        for x in -9..=9 {
            for z in -9..=9 {
                let y = roll(3) as i64 - 1;
                let wet = roll(3) == 0;
                match roll(100) {
                    0..10 => edits.push(([x, -1, z], Cell::Air)),
                    10..16 => edits.push(([x, y, z], Cell::Slab { top: false, wet })),
                    16..22 => edits.push(([x, y, z], Cell::Slab { top: true, wet })),
                    22..26 => edits.push(([x, y, z], Cell::Fence { wet })),
                    26..30 => edits.push(([x, 0, z], Cell::Stone)),
                    _ => {}
                }
            }
        }
        for _ in 0..2 {
            let p = [roll(13) as i64 - 6, roll(3) as i64, roll(13) as i64 - 6];
            edits.push((p, Cell::Water(0)));
        }
        let p = [roll(13) as i64 - 6, 1, roll(13) as i64 - 6];
        edits.push((p, Cell::Lava(0)));
        // Later: a waterlogged slab appears, a block breaks.
        let later = [
            (
                40,
                vec![(
                    [3, 1, -3],
                    Cell::Slab {
                        top: true,
                        wet: true,
                    },
                )],
            ),
            (70, vec![([0, -1, 0], Cell::Air), ([-3, 0, -2], Cell::Air)]),
        ];
        run(ORIGIN, &format!("wet seed {seed}"), &edits, &later, 140);
    }
}
