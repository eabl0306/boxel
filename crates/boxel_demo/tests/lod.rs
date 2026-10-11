//! The far level of detail of the demo generators (ADR 0031): their coarse cells against the
//! exact cell rule on the chunks they generate, far from the origin, at the sea and the beaches,
//! at the world floor, and with fields set after `new`.
#![expect(clippy::print_stderr, reason = "how close the coarse tops come")]
use boxel_demo::worldgen::{
    ForestGenerator, Heightmap, OverworldGenerator, Sea, TerrainGenerator, TerrainSettings,
};
use boxel_voxel::lod::{CellPartial, LodClasses};
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_VOLUME, ChunkKey, ChunkPos, DimensionId, LocalPos,
};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{LodCells, LodColumn, LodNode, LodRequest, LodScratch, LodSource};
use std::collections::HashMap;

/// A fixed sequence of numbers (splitmix64).
struct Numbers(u64);

impl Numbers {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `lo..hi`.
    fn between(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo) as u64) as i64
    }
}

/// A node as a source emitted it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Uniform(BlockState),
    Cells(Box<LodCells>),
}

/// One column of nodes from `source`, with its band.
#[derive(Debug)]
struct Column {
    band: LodColumn,
    nodes: HashMap<i64, Node>,
    level: u8,
}

/// The layers every test asks for: the floor at -64 and the sky over the trees at every level.
const LAYERS: std::ops::RangeInclusive<i64> = -8..=8;

fn request(level: u8, column: [i64; 2]) -> LodRequest {
    LodRequest {
        dim: DimensionId(0),
        level,
        column,
        layers: LAYERS,
    }
}

fn column(source: &dyn LodSource, r: &BlockRegistry, level: u8, at: [i64; 2]) -> Column {
    let mut nodes = HashMap::new();
    let band = source.column(
        &request(level, at),
        r,
        &mut LodScratch::default(),
        &mut |layer, node| {
            let node = match node {
                LodNode::Uniform(state) => Node::Uniform(state),
                LodNode::Cells(cells) => Node::Cells(Box::new(cells.clone())),
            };
            assert!(nodes.insert(layer, node).is_none(), "layer {layer} twice");
        },
    );
    Column { band, nodes, level }
}

impl Column {
    /// The cell `(x, y, z)` (`-1..=32` each) of the node at `layer`; `None` in a node of the
    /// band emitted uniform but not air, a node that draws nothing (its cells are ground, its
    /// border may hold fluid).
    fn get(&self, layer: i64, x: i32, y: i32, z: i32) -> Option<BlockState> {
        if let Some(state) = self.band.uniform_at(layer) {
            return Some(state);
        }
        match &self.nodes[&layer] {
            Node::Uniform(BlockState::AIR) => Some(BlockState::AIR),
            Node::Uniform(_) => None,
            Node::Cells(cells) => Some(cells.get(x, y, z)),
        }
    }

    /// [`Column::get`], a node that draws nothing taken as its uniform state.
    fn cell(&self, layer: i64, x: i32, y: i32, z: i32) -> BlockState {
        self.get(layer, x, y, z).unwrap_or(self.band.below)
    }

    /// The top of cell column `(x, z)` (`0..32`): the block just over its highest non-air cell,
    /// and that cell's state.
    fn top(&self, x: i32, z: i32) -> (i64, BlockState) {
        let shift = 5 + u32::from(self.level);
        let s = 1i64 << self.level;
        for layer in (self.band.lo..self.band.hi).rev() {
            for y in (0..32).rev() {
                let state = self.cell(layer, x, y, z);
                if state != BlockState::AIR {
                    return ((layer << shift) + (i64::from(y) + 1) * s, state);
                }
            }
        }
        (self.band.lo << shift, self.band.below)
    }
}

/// The exact cell rule on the chunks a generator makes.
struct Exact<'a> {
    generator: &'a dyn WorldGenerator,
    registry: &'a BlockRegistry,
    classes: LodClasses,
    chunks: HashMap<[i64; 3], Vec<BlockState>>,
}

/// The block at `[x, y, z]`, its chunk generated once.
fn block(
    chunks: &mut HashMap<[i64; 3], Vec<BlockState>>,
    generator: &dyn WorldGenerator,
    registry: &BlockRegistry,
    [x, y, z]: [i64; 3],
) -> BlockState {
    let dense = chunks.entry([x >> 5, y >> 5, z >> 5]).or_insert_with(|| {
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        generator
            .generate(
                ChunkKey::new(ChunkPos::new(x >> 5, y >> 5, z >> 5)),
                registry,
            )
            .to_dense(&mut dense);
        dense
    });
    dense[LocalPos::new((x & 31) as u32, (y & 31) as u32, (z & 31) as u32).index()]
}

impl<'a> Exact<'a> {
    fn new(generator: &'a dyn WorldGenerator, registry: &'a BlockRegistry) -> Self {
        Self {
            generator,
            registry,
            classes: LodClasses::from_registry(registry),
            chunks: HashMap::new(),
        }
    }

    /// The level-`level` cell whose min corner is block `[x, y, z]`.
    fn cell(&mut self, level: u8, [x, y, z]: [i64; 3]) -> BlockState {
        let Self {
            generator,
            registry,
            classes,
            chunks,
        } = self;
        CellPartial::of_cell(classes, [x, y, z], 1 << level, |dx, dy, dz| {
            let p = [x + i64::from(dx), y + i64::from(dy), z + i64::from(dz)];
            block(chunks, *generator, registry, p)
        })
        .state()
    }

    /// The top of the level-`level` cell column at block `[x, z]` (its min corner): the block
    /// just over its highest non-air cell under y = 128, and that cell's state.
    fn top(&mut self, level: u8, [x, z]: [i64; 2]) -> (i64, BlockState) {
        let s = 1i64 << level;
        for y in (0..128 / s).rev().map(|i| i * s) {
            let state = self.cell(level, [x, y, z]);
            if state != BlockState::AIR {
                return (y + s, state);
            }
        }
        panic!("no ground under y = 128 at {x}, {z}");
    }
}

/// How the coarse tops of some cell columns compare with the exact rule's.
#[derive(Debug, Default)]
struct Tops {
    columns: usize,
    /// Columns whose coarse top lies under the exact one: `(x, z, coarse, exact)`.
    below: Vec<(i64, i64, i64, i64)>,
    /// Columns with the same top.
    same: usize,
    /// Columns at most one cell over the exact top.
    within_a_cell: usize,
    /// Columns whose exact top cell is a tree's (log or leaves), and those whose coarse top is a
    /// tree's too.
    trees: usize,
    trees_kept: usize,
}

impl Tops {
    /// Compares `per_chunk` random cell columns of `level` in each of `chunks` random chunk
    /// columns within `spread` blocks of block column `center`.
    fn of(
        world: &dyn WorldGenerator,
        r: &BlockRegistry,
        level: u8,
        center: [i64; 2],
        spread: i64,
        (chunks, per_chunk): (usize, usize),
        n: &mut Numbers,
    ) -> Self {
        let source = world.lod().expect("the demo generators have a LOD source");
        let tree = |state| {
            ["boxel:log", "boxel:leaves"]
                .map(|b| r.by_name(b))
                .contains(&Some(state))
        };
        let (s, shift) = (1i64 << level, 5 + u32::from(level));
        let mut exact = Exact::new(world, r);
        let mut nodes: HashMap<[i64; 2], Column> = HashMap::new();
        let mut tops = Self::default();
        for _ in 0..chunks {
            let chunk = center.map(|c| (c + n.between(-spread, spread)) >> 5 << 5);
            for _ in 0..per_chunk {
                let [x, z] = chunk.map(|c| c + n.between(0, 32 / s) * s);
                let node = [x >> shift, z >> shift];
                let coarse = nodes
                    .entry(node)
                    .or_insert_with(|| column(source, r, level, node));
                let cell = [x, z].map(|b| ((b >> level) & 31) as i32);
                let (top, state) = coarse.top(cell[0], cell[1]);
                let (want, want_state) = exact.top(level, [x, z]);
                tops.columns += 1;
                if top < want {
                    tops.below.push((x, z, top, want));
                }
                tops.same += usize::from(top == want);
                tops.within_a_cell += usize::from(top <= want + s);
                if top > want + s {
                    eprintln!(
                        "over: {x} {z} level {level}: coarse {top} {state:?} exact {want} {want_state:?}"
                    );
                }
                if tree(want_state) {
                    tops.trees += 1;
                    tops.trees_kept += usize::from(tree(state));
                }
            }
        }
        tops
    }

    /// The bounds of the plan: never under the exact top (minus a block), within one cell over
    /// it for 99 % of the columns, and the trees kept at levels 1 and 2.
    fn check(&self, at: &str, level: u8) {
        assert!(self.below.is_empty(), "{at}: under the exact top: {self:?}");
        assert!(
            self.within_a_cell * 100 >= self.columns * 99,
            "{at}: more than one cell over the exact top: {self:?}"
        );
        if level <= 2 {
            assert!(self.trees > 0, "{at}: no tree looked at: {self:?}");
            assert_eq!(self.trees_kept, self.trees, "{at}: trees lost: {self:?}");
        }
    }
}

#[test]
fn coarse_tops_follow_the_exact_rule() {
    let r = boxel_demo::blocks::registry();
    let forest = ForestGenerator::default();
    let overworld = OverworldGenerator::default();
    for (name, world) in [
        ("forest", &forest as &dyn WorldGenerator),
        ("overworld", &overworld),
    ] {
        for level in 1..=3u8 {
            let mut n = Numbers(u64::from(level) << 8 | name.len() as u64);
            // 512 cell columns: 8 in each of 64 chunk columns spread over 128k blocks.
            let tops = Tops::of(world, &r, level, [0, 0], 1 << 16, (64, 8), &mut n);
            eprintln!("{name} level {level}: {tops:?}");
            tops.check(&format!("{name} level {level}"), level);
        }
    }
}

/// Offsets where a repeating or wrapped lattice would show (see `far_coords.rs`).
const REPEAT_OFFSETS: [i64; 3] = [256, 1 << 36, 1 << 40];

/// Far from the origin the cells are the same function: every node's border cells are the cells
/// its neighbors compute for themselves (the four columns around it and the layers over and
/// under it), whatever request computed them, and the tops still follow the exact rule.
#[test]
fn shifted_columns_give_identical_cells() {
    let r = boxel_demo::blocks::registry();
    let forest = ForestGenerator::default();
    let overworld = OverworldGenerator::default();
    for (name, world) in [
        ("forest", &forest as &dyn WorldGenerator),
        ("overworld", &overworld),
    ] {
        let source = world.lod().expect("a LOD source");
        for offset in REPEAT_OFFSETS {
            // Around a column of the offset and one of the origin's lakes, shifted.
            let center = [offset + 248, -(offset / 3) - 256];
            for level in 1..=4u8 {
                let shift = 5 + u32::from(level);
                let at = center.map(|c| c >> shift);
                let here = column(source, &r, level, at);
                let east = column(source, &r, level, [at[0] + 1, at[1]]);
                let south = column(source, &r, level, [at[0], at[1] + 1]);
                let context = format!("{name} offset {offset} level {level}");
                let same = |a: Option<BlockState>, b: Option<BlockState>, what: &str| {
                    // A node that draws nothing is emitted uniform stone: no cell to compare.
                    if let (Some(a), Some(b)) = (a, b) {
                        assert_eq!(a, b, "{context}: {what}");
                    }
                };
                for layer in LAYERS {
                    for y in -1..=32 {
                        for i in -1..=32 {
                            let w = format!("layer {layer} y {y} i {i}");
                            same(here.get(layer, 32, y, i), east.get(layer, 0, y, i), &w);
                            same(here.get(layer, 31, y, i), east.get(layer, -1, y, i), &w);
                            same(here.get(layer, i, y, 32), south.get(layer, i, y, 0), &w);
                            same(here.get(layer, i, y, 31), south.get(layer, i, y, -1), &w);
                        }
                    }
                    if layer < *LAYERS.end() {
                        for x in -1..=32 {
                            for z in -1..=32 {
                                let w = format!("layer {layer} x {x} z {z}");
                                let up = here.get(layer + 1, x, 0, z);
                                same(here.get(layer, x, 32, z), up, &w);
                                let down = here.get(layer, x, 31, z);
                                same(down, here.get(layer + 1, x, -1, z), &w);
                            }
                        }
                    }
                }
                if level <= 3 {
                    let mut n = Numbers(offset as u64 ^ u64::from(level));
                    let tops = Tops::of(world, &r, level, center, 1 << 12, (64, 8), &mut n);
                    tops.check(&context, 3);
                }
            }
        }
    }
}

/// The heights of a square of block columns.
struct Window {
    corner: [i64; 2],
    size: i64,
    heights: Vec<i64>,
}

impl Window {
    fn new(seed: u64, corner: [i64; 2], size: i64) -> Self {
        let axis = |c: i64| (c..c + size).collect::<Vec<_>>();
        let mut heights = vec![0; (size * size) as usize];
        Heightmap::default().heights(seed, &axis(corner[0]), &axis(corner[1]), &mut heights);
        Self {
            corner,
            size,
            heights,
        }
    }

    /// The lowest and highest heights of the `(s + 1)^2` columns from `[x, z]`: the columns of a
    /// level-`level` cell and the far corners its coarse ground samples.
    fn span(&self, level: u8, [x, z]: [i64; 2]) -> (i64, i64) {
        let s = 1i64 << level;
        let (mut lo, mut hi) = (i64::MAX, i64::MIN);
        for i in x - self.corner[0]..=x - self.corner[0] + s {
            for j in z - self.corner[1]..=z - self.corner[1] + s {
                let h = self.heights[(i * self.size + j) as usize];
                (lo, hi) = (lo.min(h), hi.max(h));
            }
        }
        (lo, hi)
    }

    /// The min corners of the level-`level` cells inside the window whose span passes `keep`.
    fn cells(&self, level: u8, keep: impl Fn(i64, i64) -> bool) -> Vec<[i64; 2]> {
        let s = 1i64 << level;
        let first = |c: i64| (c + s - 1) >> level << level;
        let mut out = Vec::new();
        for x in (first(self.corner[0])..self.corner[0] + self.size - s).step_by(s as usize) {
            for z in (first(self.corner[1])..self.corner[1] + self.size - s).step_by(s as usize) {
                let (lo, hi) = self.span(level, [x, z]);
                if keep(lo, hi) {
                    out.push([x, z]);
                }
            }
        }
        out
    }
}

/// The coarse cell holding block `[x, y, z]` at `level`.
fn coarse_at(
    source: &dyn LodSource,
    r: &BlockRegistry,
    level: u8,
    [x, y, z]: [i64; 3],
) -> BlockState {
    let shift = 5 + u32::from(level);
    let column = column(source, r, level, [x >> shift, z >> shift]);
    let cell = |b: i64| ((b >> level) & 31) as i32;
    column.cell(y >> shift, cell(x), cell(y), cell(z))
}

/// A sea around `(-3392, 1280)`, its bed 2 to 13 blocks deep, with coasts in the window.
const SEA_WINDOW: ([i64; 2], i64) = ([-3392 - 512, 1280 - 512], 1024);

#[test]
fn seas_stay_water_and_beaches_sand() {
    let r = boxel_demo::blocks::registry();
    let water = r.by_name("boxel:water").expect("water");
    let sand = r.by_name("boxel:sand").expect("sand");
    let sea = Sea::default();
    let forest = ForestGenerator::default();
    let overworld = OverworldGenerator::default();
    let window = Window::new(forest.terrain.seed, SEA_WINDOW.0, SEA_WINDOW.1);
    for (name, world) in [
        ("forest", &forest as &dyn WorldGenerator),
        ("overworld", &overworld),
    ] {
        let source = world.lod().expect("a LOD source");
        let mut exact = Exact::new(world, &r);
        // Seas 3 and 10 blocks deep at their shallowest column: the cell holding the surface
        // is water, as by the exact rule.
        for level in 2..=4u8 {
            for depth in [3, 10] {
                let cells = window.cells(level, |_, hi| hi == sea.level - depth);
                let at = format!("{name} level {level} depth {depth}");
                assert!(cells.len() >= 4, "{at}: {} cells", cells.len());
                for &[x, z] in cells.iter().take(16) {
                    let p = [x, sea.level, z];
                    assert_eq!(coarse_at(source, &r, level, p), water, "{at} at {p:?}");
                    let cell = [x, sea.level >> level << level, z];
                    assert_eq!(exact.cell(level, cell), water, "{at} exact at {p:?}");
                }
            }
        }
        // Beaches: land at the sea level up to 2 blocks over it keeps its sand at a distance.
        for level in 1..=3u8 {
            let cells = window.cells(level, |lo, hi| lo >= 41 && (46..=47).contains(&hi));
            let at = format!("{name} level {level} beach");
            assert!(cells.len() >= 4, "{at}: {} cells", cells.len());
            let shift = 5 + u32::from(level);
            for &[x, z] in cells.iter().take(32) {
                let column = column(source, &r, level, [x >> shift, z >> shift]);
                let (_, state) = column.top(((x >> level) & 31) as i32, ((z >> level) & 31) as i32);
                assert_eq!(state, sand, "{at} at {x}, {z}");
            }
        }
    }
}

/// The examples build the forest with `ForestGenerator { sea: None, ..ForestGenerator::new(..) }`
/// (`--no-sea`): the coarse cells read the fields at every call, so that world has no water at
/// any level, and a sea raised after `new` shows.
#[test]
fn fields_set_after_new_reach_the_coarse_cells() {
    let r = boxel_demo::blocks::registry();
    let water = r.by_name("boxel:water").expect("water");
    let seed = boxel_demo::worldgen::DEFAULT_SEED;
    let dry = ForestGenerator {
        sea: None,
        ..ForestGenerator::new(seed, TerrainSettings::default())
    };
    let mut dry_overworld = OverworldGenerator::default();
    dry_overworld.settings.sea = None;
    for (name, world) in [
        ("forest", &dry as &dyn WorldGenerator),
        ("overworld", &dry_overworld),
    ] {
        let source = world.lod().expect("a LOD source");
        for level in 1..=4u8 {
            let shift = 5 + u32::from(level);
            let [x, z] = SEA_WINDOW.0.map(|c| (c + SEA_WINDOW.1 / 2) >> shift);
            for column in [[x, z], [x + 1, z], [x, z - 1]] {
                let column = self::column(source, &r, level, column);
                for node in column.nodes.values() {
                    if let Node::Cells(cells) = node {
                        assert!(!cells.0.contains(&water), "{name} level {level}");
                    }
                    assert_ne!(node, &Node::Uniform(water), "{name} level {level}");
                }
            }
        }
    }
    // A sea raised to y = 60 after `new` covers the sea window's bed at y = 58.
    let high = ForestGenerator {
        sea: Some(Sea {
            level: 60,
            beach: 2,
        }),
        ..ForestGenerator::new(seed, TerrainSettings::default())
    };
    let source = high.lod().expect("a LOD source");
    let at = [-3392 + 40, 58, 1280 + 16];
    for level in 1..=3 {
        assert_eq!(coarse_at(source, &r, level, at), water, "level {level}");
    }
}

/// The forest and the terrain stand on a bedrock floor at y = -64 with air under it: at a
/// distance it is stone all the way down, so no node is ever meshed for the floor.
#[test]
fn the_floor_is_solid_at_a_distance() {
    let r = boxel_demo::blocks::registry();
    let stone = r.by_name("boxel:stone").expect("stone");
    let forest = ForestGenerator::default();
    let terrain = TerrainGenerator::default();
    for (name, world) in [
        ("forest", &forest as &dyn WorldGenerator),
        ("terrain", &terrain),
    ] {
        let source = world.lod().expect("a LOD source");
        for level in 1..=6u8 {
            let shift = 5 + u32::from(level);
            let floor = -64 >> shift;
            for at in [[0, 0], [248, -256], [-3392, 1280], [1 << 30, -(1 << 33)]] {
                let column = column(source, &r, level, at.map(|c| c >> shift));
                let context = format!("{name} level {level} at {at:?}: {:?}", column.band);
                assert_eq!(column.band.below, stone, "{context}");
                // Under the band, or a node that draws nothing.
                match column.nodes.get(&floor) {
                    None => assert!(floor < column.band.lo, "{context}"),
                    Some(node) => assert_eq!(node, &Node::Uniform(stone), "{context}"),
                }
            }
        }
    }
}

/// The same request gives the same nodes, through the generators' hook or directly, from a
/// fresh worker or one that served other columns first, and with another instance.
#[test]
fn coarse_cells_are_deterministic() {
    let r = boxel_demo::blocks::registry();
    let worlds: [(Box<dyn WorldGenerator>, Box<dyn WorldGenerator>); 3] = [
        (
            Box::new(ForestGenerator::default()),
            Box::new(ForestGenerator::default()),
        ),
        (
            Box::new(OverworldGenerator::default()),
            Box::new(OverworldGenerator::default()),
        ),
        (
            Box::new(TerrainGenerator::default()),
            Box::new(TerrainGenerator::default()),
        ),
    ];
    for (a, b) in &worlds {
        let (a, b) = (
            a.lod().expect("a LOD source"),
            b.lod().expect("a LOD source"),
        );
        let mut warm = LodScratch::default();
        let run = |source: &dyn LodSource, scratch: &mut LodScratch, req: &LodRequest| {
            let mut nodes = Vec::new();
            let band = source.column(req, &r, scratch, &mut |layer, node| {
                nodes.push((
                    layer,
                    match node {
                        LodNode::Uniform(state) => Node::Uniform(state),
                        LodNode::Cells(cells) => Node::Cells(Box::new(cells.clone())),
                    },
                ));
            });
            (band, nodes)
        };
        let requests: Vec<LodRequest> = (1..=6u8)
            .flat_map(|level| {
                [[0, 0], [5, -7], [-(1 << 40) >> level, 3 << 20]].map(|c| request(level, c))
            })
            .collect();
        let first: Vec<_> = requests.iter().map(|q| run(a, &mut warm, q)).collect();
        for (q, want) in requests.iter().zip(&first).rev() {
            assert_eq!(&run(a, &mut warm, q), want, "{q:?}");
            assert_eq!(&run(b, &mut LodScratch::default(), q), want, "{q:?}");
        }
    }
}
