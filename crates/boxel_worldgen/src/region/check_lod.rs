//! The exported harness of a realm's far LOD (ADR 0039 sec. 10, feature `test-support`):
//! [`check_lod`] builds a region generator's real terrain around some columns (its chunks without
//! carvers and features, which the far LOD leaves out) and compares its coarse cells with the cell
//! rule on them, and checks the runs of its feature stages, so a game's or a mod's kinds,
//! modifiers, surface and feature stages get their far LOD checked without bespoke tests.
//! [`lod_corners`] gives the pass's corner heights, for exactness tests against the chunk
//! columns.

use super::generator::RegionGenerator;
use super::kind::{Context, HEIGHT_SHIFT, HeightQ};
use super::lod::{CORNERS, EDGE, LodMemo};
use crate::WorldGenerator;
use crate::lod::{
    CellPartial, LodCells, LodClass, LodClasses, LodColumn, LodNode, LodRequest, LodScratch,
    LodSource,
};
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_SHIFT, ChunkData, ChunkKey, ChunkPos, DimensionId, LocalPos,
};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::RangeInclusive;

/// How far, in blocks, the highest corner of a cell may stand over the cell's real columns: the
/// far corners lie one diagonal step past them, and the terrain keeps 3:1 along any line, with a
/// block for the roundings.
pub const CORNER_STEP: i64 = 6;

/// At most this many failures are listed in the error.
const MAX_LISTED: usize = 24;

/// What [`check_lod`] compared.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LodReport {
    /// Cell columns compared (one per column and level).
    pub cells: usize,
    /// Of them, dry ones (a solid top cell in both, over the realm's fluid in the pass).
    pub dry: usize,
    /// Wet ones (a fluid top cell in both, or the pass's column under the fluid), their tops
    /// within a cell.
    pub wet: usize,
    /// Dry cell columns whose coarse top lies in the real top's cell.
    pub same_top: usize,
    /// Cell columns where the fluid disagreed, each within a cell of a fluid edge.
    pub fluid_edges: usize,
    /// The highest a dry coarse top stood over the real one, in blocks.
    pub max_over: i64,
    /// Pairs of border cells compared between neighbor nodes.
    pub borders: usize,
    /// Cell columns with a feature stage's run ([`FeatureStage::lod_runs`]), each checked.
    ///
    /// [`FeatureStage::lod_runs`]: super::FeatureStage::lod_runs
    pub runs: usize,
}

/// A node as the source emitted it.
#[derive(Clone, Debug)]
enum Node {
    Uniform(BlockState),
    Cells(Box<LodCells>),
}

/// A column of nodes of one level, with its band.
struct Coarse {
    band: LodColumn,
    nodes: HashMap<i64, Node>,
    shift: u32,
}

impl Coarse {
    /// The node column `column` of `level` as the pass draws its terrain alone (its feature runs
    /// left out), every layer of its band.
    fn terrain<Cx: Context>(
        generator: &RegionGenerator<Cx>,
        registry: &BlockRegistry,
        level: u8,
        column: [i64; 2],
    ) -> Self {
        let mut scratch = LodScratch::default();
        let mut memo = LodMemo::<Cx>::take(&mut scratch);
        let slots = memo.slots(generator.key, false);
        let mut req = LodRequest {
            dim: DimensionId(0),
            level,
            column,
            layers: RangeInclusive::new(1, 0),
        };
        generator.lod_prepare(&req, registry, slots);
        slots.without_canopies();
        let band = slots.band;
        req.layers = band.lo..=band.hi - 1;
        let mut nodes = HashMap::new();
        let band = generator.lod_emit(&req, slots, &mut scratch.cells, &mut |layer, node| {
            let node = match node {
                LodNode::Uniform(state) => Node::Uniform(state),
                LodNode::Cells(cells) => Node::Cells(Box::new(cells.clone())),
            };
            nodes.insert(layer, node);
        });
        Self {
            band,
            nodes,
            shift: 5 + u32::from(level),
        }
    }

    /// The node column `column` of `level`, every layer of its band.
    fn of(source: &dyn LodSource, registry: &BlockRegistry, level: u8, column: [i64; 2]) -> Self {
        let mut req = LodRequest {
            dim: DimensionId(0),
            level,
            column,
            layers: RangeInclusive::new(1, 0),
        };
        let mut scratch = LodScratch::default();
        let band = source.column(&req, registry, &mut scratch, &mut |_, _| {});
        req.layers = band.lo..=band.hi - 1;
        let mut nodes = HashMap::new();
        let again = source.column(&req, registry, &mut scratch, &mut |layer, node| {
            let node = match node {
                LodNode::Uniform(state) => Node::Uniform(state),
                LodNode::Cells(cells) => Node::Cells(Box::new(cells.clone())),
            };
            nodes.insert(layer, node);
        });
        assert_eq!(
            band, again,
            "the band of level {level} column {column:?} changed"
        );
        Self {
            band,
            nodes,
            shift: 5 + u32::from(level),
        }
    }

    /// The cell `(x, y, z)` (`-1..=32` each) of layer `layer`; `None` inside a node of the band
    /// emitted uniform but not air (it draws nothing: its cells are ground, their states unknown).
    fn get(&self, layer: i64, x: i32, y: i32, z: i32) -> Option<BlockState> {
        if let Some(state) = self.band.uniform_at(layer) {
            return Some(state);
        }
        match self.nodes.get(&layer)? {
            Node::Uniform(BlockState::AIR) => Some(BlockState::AIR),
            Node::Uniform(_) => None,
            Node::Cells(cells) => Some(cells.get(x, y, z)),
        }
    }

    /// The top of cell column `(x, z)` (`0..32`): the block over its highest non-air cell, and
    /// that cell's state (a node that draws nothing counts as the band's `below`).
    fn top(&self, x: i32, z: i32) -> (i64, BlockState) {
        let s = 1i64 << (self.shift - 5);
        for layer in (self.band.lo..self.band.hi).rev() {
            for y in (0..32).rev() {
                let state = self.get(layer, x, y, z).unwrap_or(self.band.below);
                if state != BlockState::AIR {
                    return ((layer << self.shift) + (i64::from(y) + 1) * s, state);
                }
            }
        }
        (self.band.lo << self.shift, self.band.below)
    }
}

/// The terrain chunks of a generator ([`RegionGenerator::generate_terrain`]), each generated
/// once.
struct Chunks<'a, Cx: Context> {
    generator: &'a RegionGenerator<Cx>,
    registry: &'a BlockRegistry,
    chunks: HashMap<[i64; 3], ChunkData>,
}

impl<Cx: Context> Chunks<'_, Cx> {
    fn chunk(&mut self, at: [i64; 3]) -> &ChunkData {
        let (generator, registry) = (self.generator, self.registry);
        self.chunks.entry(at).or_insert_with(|| {
            generator.generate_terrain(ChunkKey::new(ChunkPos::new(at[0], at[1], at[2])), registry)
        })
    }

    fn block(&mut self, [x, y, z]: [i64; 3]) -> BlockState {
        let chunk = self.chunk([x >> CHUNK_SHIFT, y >> CHUNK_SHIFT, z >> CHUNK_SHIFT]);
        chunk.get(LocalPos::new(
            (x & 31) as u32,
            (y & 31) as u32,
            (z & 31) as u32,
        ))
    }
}

/// The real terrain of a generator and the cell rule on it.
struct Real<'a, Cx: Context> {
    chunks: Chunks<'a, Cx>,
    classes: LodClasses,
    /// Per block column, its highest non-Empty block and that block's class.
    tops: HashMap<(i64, i64), (i64, LodClass)>,
    /// The highest chunk layer that may hold anything.
    top_layer: i64,
}

impl<'a, Cx: Context> Real<'a, Cx> {
    fn new(generator: &'a RegionGenerator<Cx>, registry: &'a BlockRegistry) -> Self {
        Self {
            chunks: Chunks {
                generator,
                registry,
                chunks: HashMap::new(),
            },
            classes: LodClasses::from_registry(registry),
            tops: HashMap::new(),
            top_layer: *generator.vertical_chunks().end(),
        }
    }

    /// The highest Solid block of column `(x, z)`, under its fluid if it has one.
    fn solid_top(&mut self, x: i64, z: i64) -> i64 {
        let (top, class) = self.column_top(x, z);
        if class == LodClass::Solid {
            return top;
        }
        for y in (top - (4096 << CHUNK_SHIFT)..top).rev() {
            let state = self.chunks.block([x, y, z]);
            if self.classes.class(state) == LodClass::Solid {
                return y;
            }
        }
        panic!("check_lod: no solid block under ({x}, {top}, {z})");
    }

    /// The highest non-Empty block of column `(x, z)` and its class.
    fn column_top(&mut self, x: i64, z: i64) -> (i64, LodClass) {
        if let Some(&top) = self.tops.get(&(x, z)) {
            return top;
        }
        let mut found = None;
        'chunks: for layer in (self.top_layer - 4096..=self.top_layer).rev() {
            let chunk = self
                .chunks
                .chunk([x >> CHUNK_SHIFT, layer, z >> CHUNK_SHIFT]);
            if chunk.as_uniform() == Some(BlockState::AIR) {
                continue;
            }
            for ly in (0..32u32).rev() {
                let state = chunk.get(LocalPos::new((x & 31) as u32, ly, (z & 31) as u32));
                let class = self.classes.class(state);
                if class != LodClass::Empty {
                    found = Some(((layer << CHUNK_SHIFT) + i64::from(ly), class));
                    break 'chunks;
                }
            }
        }
        let top = found.unwrap_or_else(|| panic!("check_lod: no ground in column ({x}, {z})"));
        self.tops.insert((x, z), top);
        top
    }

    /// The cell of `2^level` blocks whose min corner is `[x, y, z]`, by the cell rule.
    fn cell(&mut self, level: u8, [x, y, z]: [i64; 3]) -> BlockState {
        let Self {
            chunks, classes, ..
        } = self;
        CellPartial::of_cell(classes, [x, y, z], 1 << level, |dx, dy, dz| {
            chunks.block([x + i64::from(dx), y + i64::from(dy), z + i64::from(dz)])
        })
        .state()
    }

    /// The highest top block over the `s x s` columns from `[x, z]`, if every one is solid (no
    /// fluid on top): the cell column's real ground.
    fn ground(&mut self, s: i64, [x, z]: [i64; 2]) -> Option<i64> {
        let mut hi = i64::MIN;
        for dx in 0..s {
            for dz in 0..s {
                let (y, class) = self.column_top(x + dx, z + dz);
                if class != LodClass::Solid {
                    return None;
                }
                hi = hi.max(y);
            }
        }
        Some(hi)
    }

    /// The highest Solid block over the `s x s` columns from `[x, z]` (the ground under any
    /// fluid).
    fn solid(&mut self, s: i64, [x, z]: [i64; 2]) -> i64 {
        let mut hi = i64::MIN;
        for dx in 0..s {
            for dz in 0..s {
                hi = hi.max(self.solid_top(x + dx, z + dz));
            }
        }
        hi
    }

    /// The highest non-Empty block over the `s x s` columns from `[x, z]`, and the lowest one.
    fn span(&mut self, s: i64, [x, z]: [i64; 2]) -> (i64, i64) {
        let (mut hi, mut lo) = (i64::MIN, i64::MAX);
        for dx in 0..s {
            for dz in 0..s {
                let (y, _) = self.column_top(x + dx, z + dz);
                hi = hi.max(y);
                lo = lo.min(y);
            }
        }
        (hi, lo)
    }

    /// The top of the level-`level` cell column at block `[x, z]` (its min corner): the block
    /// over its highest non-air cell, and that cell's state.
    fn top(&mut self, level: u8, [x, z]: [i64; 2]) -> (i64, BlockState) {
        let s = 1i64 << level;
        let (hi, _) = self.span(s, [x, z]);
        let mut y = hi.div_euclid(s) * s;
        for _ in 0..1 << 12 {
            let state = self.cell(level, [x, y, z]);
            if state != BlockState::AIR {
                return (y + s, state);
            }
            y -= s;
        }
        panic!("check_lod: no non-air cell of level {level} under ({x}, {hi}, {z})");
    }

    /// Whether the columns of `[x0, x1) x [z0, z1)` hold both a fluid top and a solid one.
    fn fluid_edge(&mut self, [x0, x1]: [i64; 2], [z0, z1]: [i64; 2]) -> bool {
        let (mut wet, mut dry) = (false, false);
        for x in x0..x1 {
            for z in z0..z1 {
                match self.column_top(x, z).1 {
                    LodClass::Fluid => wet = true,
                    _ => dry = true,
                }
                if wet && dry {
                    return true;
                }
            }
        }
        false
    }
}

/// Checks the far LOD of `generator` at the block columns `columns`, at each level of `levels`
/// (ADR 0039 sec. 10; the plan asks for levels 1 to 4), against its real terrain (its chunks
/// without the carvers and the feature stages, [`RegionGenerator::generate_terrain`]: the far LOD
/// leaves the carvers out and draws the features as runs of their own) and the cell rule. For
/// each column, the cell column of each level that holds it, the pass's terrain alone (its
/// feature runs left out):
///
/// - dry (a solid top cell in both, and the pass's cell column over the realm's fluid: not all
///   its corners under the level): the coarse top is never under the real one, and stands over
///   it by at most the cell's declared margin (its coarse ground over its highest corner: the
///   kinds' margins, the modifiers' rises and the surface's rise), plus what the kinds and the
///   modifiers declare they leave out ([`TerrainKind::lod_left_out`],
///   [`ShapeModifier::lod_left_out`]), plus [`CORNER_STEP`], plus a cell for the rounding. Where
///   every column of the cell is dry, the same in blocks: the pass's coarse ground covers every
///   real ground (the margins are sound), and its highest corner stands over them by at most
///   [`CORNER_STEP`] and what is left out (the corners are right);
/// - fluid (the others: a fluid top cell in either, or the pass's cell column under the fluid,
///   whose ground is its highest corner's, so land between the corners or a floor under less
///   than half a cell of fluid may stand over it): where one top is fluid and the other is not,
///   or the tops lie more than a cell apart, a fluid edge lies within a cell (the columns around
///   hold both a fluid top and a solid one);
/// - the band is conservative there: the real cells over it are air, and the real cell at the
///   top row of the layer under it is not;
/// - the run of the feature stages, if the cell column has one ([`FeatureStage::lod_runs`]):
///   over the ground (its lowest block over the highest real Solid block of the cell's columns,
///   under any fluid: a run on a cell whose real land stands over its coarse ground, such as a wet
///   cell of a shallow coast, would be buried), under the height (at most the feature stages'
///   greatest [`FeatureReach::height`] blocks), inside the band, and drawn (the top cell of the
///   column with the runs lies at or over it);
/// - the node's border cells, runs included, equal its 4 neighbors' and the layers' over and
///   under it.
///
/// Returns what it compared, or every failure (the first few listed).
///
/// [`TerrainKind::lod_left_out`]: super::TerrainKind::lod_left_out
/// [`ShapeModifier::lod_left_out`]: super::ShapeModifier::lod_left_out
/// [`FeatureStage::lod_runs`]: super::FeatureStage::lod_runs
/// [`FeatureReach::height`]: super::FeatureReach::height
pub fn check_lod<Cx: Context>(
    generator: &RegionGenerator<Cx>,
    registry: &BlockRegistry,
    columns: &[(i64, i64)],
    levels: RangeInclusive<u8>,
) -> Result<LodReport, String> {
    let mut real = Real::new(generator, registry);
    let mut report = LodReport::default();
    let mut failures: Vec<String> = Vec::new();
    for level in levels {
        let (s, shift) = (1i64 << level, 5 + u32::from(level));
        let left_out = generator
            .kinds
            .iter()
            .map(|k| i64::from(k.kind.lod_left_out(&generator.cx, level)))
            .max()
            .unwrap_or(0)
            + generator
                .modifiers
                .iter()
                .map(|m| i64::from(m.modifier.lod_left_out(level)))
                .sum::<i64>();
        let left_out = (left_out + (1 << HEIGHT_SHIFT) - 1) >> HEIGHT_SHIFT;
        // As drawn (the runs included), and the terrain alone.
        let mut nodes: HashMap<[i64; 2], Coarse> = HashMap::new();
        let mut terrain: HashMap<[i64; 2], Coarse> = HashMap::new();
        let node_of = |nodes: &mut HashMap<[i64; 2], Coarse>, at: [i64; 2]| {
            nodes
                .entry(at)
                .or_insert_with(|| Coarse::of(generator, registry, level, at));
        };
        let mut seen_nodes = Vec::new();
        for &(x, z) in columns {
            let (cx, cz) = (x >> level << level, z >> level << level);
            let node = [x >> shift, z >> shift];
            node_of(&mut nodes, node);
            if !seen_nodes.contains(&node) {
                seen_nodes.push(node);
            }
            let coarse = terrain
                .entry(node)
                .or_insert_with(|| Coarse::terrain(generator, registry, level, node));
            let (cell_x, cell_z) = (((x >> level) & 31) as i32, ((z >> level) & 31) as i32);
            let (coarse_top, coarse_state) = coarse.top(cell_x, cell_z);
            let (real_top, real_state) = real.top(level, [cx, cz]);
            let at = format!("level {level} cell column ({cx}, {cz})");
            report.cells += 1;
            let class = |state| real.classes.class(state);
            let (coarse_wet, real_wet) = (
                class(coarse_state) == LodClass::Fluid,
                class(real_state) == LodClass::Fluid,
            );
            // The pass's own numbers for the cell column.
            let mut scratch = LodScratch::default();
            let mut memo = LodMemo::<Cx>::take(&mut scratch);
            let slots = memo.slots(generator.key, false);
            let req = LodRequest {
                dim: DimensionId(0),
                level,
                column: node,
                layers: RangeInclusive::new(1, 0),
            };
            generator.lod_prepare(&req, registry, slots);
            let col = slots.cols[(cell_x as usize + 1) * EDGE + cell_z as usize + 1];
            if !coarse_wet && !real_wet && !col.is_wet() {
                report.dry += 1;
                // In blocks, where every column is dry: the pass's ground covers each real
                // ground, and its corners stand over them by at most a step and what is left out.
                if let Some(ground) = real.ground(s, [cx, cz]) {
                    if col.t < ground {
                        failures.push(format!(
                            "{at}: the coarse ground {} is under the real one {ground}: a margin \
                             is under the heights",
                            col.t
                        ));
                    }
                    if col.h - ground > left_out + CORNER_STEP {
                        failures.push(format!(
                            "{at}: the highest corner's ground {} stands over the real one \
                             {ground} by more than {CORNER_STEP} and {left_out} left out",
                            col.h
                        ));
                    }
                }
                if coarse_top < real_top {
                    failures.push(format!(
                        "{at}: the coarse top {coarse_top} ({coarse_state:?}) is under the \
                             real one {real_top} ({real_state:?})"
                    ));
                }
                let over = coarse_top - real_top;
                let allowed = (col.t - col.h) + left_out + CORNER_STEP + s - 1;
                if over > allowed {
                    failures.push(format!(
                        "{at}: the coarse top {coarse_top} stands {over} blocks over the real \
                             one {real_top}, more than its margin {} plus {left_out} left out, \
                             {CORNER_STEP} and a cell allow",
                        col.t - col.h
                    ));
                }
                report.same_top += usize::from(over < s);
                report.max_over = report.max_over.max(over);
            } else if coarse_wet != real_wet || coarse_top.abs_diff(real_top) > s.unsigned_abs() {
                if real.fluid_edge([cx - s, cx + 2 * s], [cz - s, cz + 2 * s]) {
                    report.fluid_edges += 1;
                } else {
                    failures.push(format!(
                        "{at}: the coarse top {coarse_top} ({coarse_state:?}) and the real one \
                             {real_top} ({real_state:?}) disagree on the fluid, with no fluid \
                             edge within a cell"
                    ));
                }
            } else {
                report.wet += 1;
            }
            // The band, at this cell column.
            let band = coarse.band;
            let (real_hi, _) = real.span(s, [cx, cz]);
            if real_hi >= (band.hi << shift) - s {
                failures.push(format!(
                    "{at}: a real block at y {real_hi}, in the padded block of layer {} \
                         that the band says is air",
                    band.hi
                ));
            }
            let floor_row = band.lo << shift;
            if real.cell(level, [cx, floor_row, cz]) == BlockState::AIR {
                failures.push(format!(
                    "{at}: the real cell at y {floor_row}, the top border row of layer {} \
                         under the band, is air",
                    band.lo - 1
                ));
            }
            // The run of the feature stages.
            if col.canopy_top > col.t {
                report.runs += 1;
                let (from, to) = (col.t + 1, col.canopy_top);
                let ground = real.solid(s, [cx, cz]);
                if from <= ground {
                    failures.push(format!(
                        "{at}: the feature run {from}..={to} ({:?}) is under the real ground at \
                         y {ground}: the cell's coarse ground ({}, its highest corner's {}) lies \
                         under real land",
                        col.canopy, col.t, col.h
                    ));
                }
                let height = generator
                    .features
                    .iter()
                    .map(|f| i64::from(f.stage.reach().height))
                    .max()
                    .unwrap_or(0);
                if to - col.t > height {
                    failures.push(format!(
                        "{at}: the feature run {from}..={to} is {} blocks tall, over the feature \
                         stages' height {height}",
                        to - col.t
                    ));
                }
                if to >= (band.hi << shift) - s {
                    failures.push(format!(
                        "{at}: the feature run {from}..={to} reaches the padded block of layer {} \
                         that the band says is air",
                        band.hi
                    ));
                }
                let (drawn, _) = nodes[&node].top(cell_x, cell_z);
                if drawn <= to {
                    failures.push(format!(
                        "{at}: the feature run {from}..={to} is not drawn: the top of the cell \
                         column with the runs is {drawn}"
                    ));
                }
            }
        }
        // Neighbor borders, for each node the columns fall in.
        for node in seen_nodes {
            for d in [[1, 0], [0, 1], [-1, 0], [0, -1]] {
                node_of(&mut nodes, [node[0] + d[0], node[1] + d[1]]);
            }
            let a = &nodes[&node];
            for (d, axis) in [([1, 0], 0usize), ([0, 1], 1)] {
                let b = &nodes[&[node[0] + d[0], node[1] + d[1]]];
                report.borders += borders(a, b, axis, &mut |f| failures.push(f));
            }
            for (d, axis) in [([-1, 0], 0usize), ([0, -1], 1)] {
                let b = &nodes[&[node[0] + d[0], node[1] + d[1]]];
                report.borders += borders(b, a, axis, &mut |f| failures.push(f));
            }
            report.borders += vertical_borders(a, &mut |f| failures.push(f));
        }
    }
    if failures.is_empty() {
        return Ok(report);
    }
    let mut message = format!("check_lod: {} failures; the first ones:", failures.len());
    for f in failures.iter().take(MAX_LISTED) {
        let _ = write!(message, "\n- {f}");
    }
    Err(message)
}

/// Compares the shared cells of `a` and `b`, neighbors along `axis` (`b` one node further):
/// `a`'s border at 32 is `b`'s cell 0 and `b`'s border at -1 is `a`'s cell 31. Returns how many
/// cells it compared.
fn borders(a: &Coarse, b: &Coarse, axis: usize, fail: &mut dyn FnMut(String)) -> usize {
    let mut n = 0;
    let lo = a.band.lo.min(b.band.lo) - 1;
    let hi = a.band.hi.max(b.band.hi);
    for layer in lo..=hi {
        for y in -1..=32 {
            for w in -1..=32 {
                for (ia, ib) in [(32, 0), (31, -1)] {
                    let (pa, pb) = if axis == 0 {
                        ([ia, y, w], [ib, y, w])
                    } else {
                        ([w, y, ia], [w, y, ib])
                    };
                    let (Some(sa), Some(sb)) = (
                        a.get(layer, pa[0], pa[1], pa[2]),
                        b.get(layer, pb[0], pb[1], pb[2]),
                    ) else {
                        continue;
                    };
                    n += 1;
                    if sa != sb {
                        fail(format!(
                            "layer {layer}: border cell {pa:?} of one node is {sa:?}, cell {pb:?} \
                             of its neighbor along axis {axis} is {sb:?}"
                        ));
                    }
                }
            }
        }
    }
    n
}

/// Compares the shared cells of the layers of one node column: layer `l`'s row 32 is layer `l +
/// 1`'s row 0, and layer `l + 1`'s row -1 is layer `l`'s row 31.
fn vertical_borders(a: &Coarse, fail: &mut dyn FnMut(String)) -> usize {
    let mut n = 0;
    for layer in a.band.lo - 1..=a.band.hi {
        for x in -1..=32 {
            for z in -1..=32 {
                for (ra, rb) in [(32, 0), (31, -1)] {
                    let (Some(sa), Some(sb)) = (a.get(layer, x, ra, z), a.get(layer + 1, x, rb, z))
                    else {
                        continue;
                    };
                    n += 1;
                    if sa != sb {
                        fail(format!(
                            "layers {layer} and {}: cell ({x}, {ra}, {z}) is {sa:?}, cell ({x}, \
                             {rb}, {z}) over it is {sb:?}",
                            layer + 1
                        ));
                    }
                }
            }
        }
    }
    n
}

/// The far LOD pass's corner heights for the node column of `req` (blended and shaped, as the
/// coarse cells read them): every corner `(x, z)` of the padded node with its height, in
/// [`HeightQ`]. With exact kinds (the default
/// [`TerrainKind::lod_heights`](super::TerrainKind::lod_heights)) each is the chunk column's
/// height there, bit for bit.
pub fn lod_corners<Cx: Context>(
    generator: &RegionGenerator<Cx>,
    registry: &BlockRegistry,
    req: &LodRequest,
) -> Vec<((i64, i64), HeightQ)> {
    let mut scratch = LodScratch::default();
    let mut memo = LodMemo::<Cx>::take(&mut scratch);
    let slots = memo.slots(generator.key, false);
    generator.lod_prepare(req, registry, slots);
    let mut out = Vec::with_capacity(CORNERS * CORNERS);
    for (a, &x) in slots.xs.iter().enumerate() {
        for (b, &z) in slots.zs.iter().enumerate() {
            out.push(((x, z), slots.corners[a * CORNERS + b]));
        }
    }
    out
}

/// The bytes the far LOD memo of `generator` holds on the heap in `scratch` (the Context's
/// fields aside: they are the game's), `None` if `scratch` holds no memo of its type. The budget
/// is 512 KiB per LOD worker (ADR 0039 "Presupuestos y medición").
pub fn lod_memo_bytes<Cx: Context>(
    generator: &RegionGenerator<Cx>,
    scratch: &mut LodScratch,
) -> Option<usize> {
    let memo = scratch.memo.as_mut()?.downcast_mut::<LodMemo<Cx>>()?;
    Some(memo.heap_bytes(generator.key))
}
