//! Far level of detail (ADR 0031): [`ForestGenerator`], [`TerrainGenerator`] and
//! [`OverworldGenerator`] answer for coarse cells themselves ([`LodSource`]), straight from their
//! heightmap, sea and trees, without building the blocks under a cell.
//!
//! One call builds a column of level-`k` nodes (cells `s = 2^k` blocks wide), every layer from
//! the same samples:
//!
//! - Heights: one grid of the heightmap at the 35 x 35 cell corners of the padded node. A cell
//!   column's height `h` is the highest of its 4 corners, and its ground `t` stands a margin
//!   over it above level 1 (the corners miss up to a block of the finest octave inside a cell at
//!   levels 2 to 4 on the default terrain, about `s / 10` above), so the coarse ground stands on
//!   or above the real one.
//! - Trees: every tree of the 8 x 8 tree cells the padded node touches (one in four at level 5,
//!   one in sixteen at 6, and so on, so the cost stays bounded), on the ground of its trunk
//!   column ([`Heightmap::height_at`] at levels 1 and 2, the `t` of the trunk's cell above, if
//!   its `h` is a ground trees grow on). Per cell column a tree adds its log and leaf runs, the
//!   hull of the runs of every tree there: any cell they cross is solid, so forests keep their
//!   bumps at every level.
//! - Cells, by the cell rule of [`boxel_worldgen::lod`] on those columns: a cell holding ground
//!   (up to `t`) or tree is water if the sea lies on its ground (`t` under the sea level and
//!   below the cell's top: the water then covers every column of the cell); else, in the cell
//!   holding `h` or over it, a tree block as high as `h` (leaves over a log), and otherwise the
//!   surface block of a column of height `h` at `min(top of the cell, h)`, with the sand of the
//!   beaches. A cell without either is water if the sea fills half of it, else air.
//!
//! Left out: caves (the ground is solid down to `i64::MIN`), cave openings and plants (Empty at
//! a distance anyway). The world floor of the forest and the terrain (bedrock with air under it)
//! counts as solid too: [`LodColumn::below`] is stone, so no node is meshed for faces nobody
//! sees from above. A node with no air cell, border included, and no fluid cell inside draws
//! nothing either and is emitted uniform stone; its cells' states are never seen.
//!
//! Every call reads the generator's current fields (callers change them after `new`, like the
//! examples' forest without a sea), and every cell is a pure function of its world position, so
//! the borders of neighboring nodes agree.

use super::{ForestGenerator, Heightmap, OverworldGenerator, Sea, TerrainGenerator, Tree, Trees};
use crate::worldgen::overworld::{SurfaceBlocks, SurfaceRules};
use boxel_voxel::{BlockRegistry, BlockState};
use boxel_worldgen::lod::{
    LOD_PADDED, LodCells, LodColumn, LodNode, LodRequest, LodScratch, LodSource,
};
use std::ops::RangeInclusive;

/// Cell columns along a padded node edge.
const EDGE: usize = LOD_PADDED;
/// Cell corners along a padded node edge.
const CORNERS: usize = EDGE + 1;
/// Cells in one horizontal layer of a padded node: the stride of `y` in [`LodCells`].
const LAYER: usize = EDGE * EDGE;
/// A run with no block.
const NO_RUN: (i64, i64) = (i64::MAX, i64::MIN);

/// One cell column of a padded node: its ground and the blocks trees put there.
#[derive(Clone, Copy, Debug)]
struct Column {
    /// The highest of the cell column's corners: the height its surface blocks follow.
    height: i64,
    /// The top of the ground: `height` plus the level's margin.
    ground: i64,
    /// The lowest and highest leaf blocks of every tree there (hull), or [`NO_RUN`].
    leaves: (i64, i64),
    /// The same for logs.
    logs: (i64, i64),
}

/// A worker's buffers, kept in [`LodScratch::memo`] between calls.
#[derive(Default)]
struct Memo {
    xs: Vec<i64>,
    zs: Vec<i64>,
    noise: Vec<f64>,
    corners: Vec<i64>,
    /// By `x * EDGE + z`, the padded cell column `(x - 1, z - 1)`.
    columns: Vec<Column>,
}

/// The blocks a demo LOD column writes.
#[derive(Clone, Copy, Debug)]
struct Blocks {
    surface: SurfaceBlocks,
    water: BlockState,
    log: BlockState,
    leaves: BlockState,
}

/// What a demo world looks like from far away: the parts of a generator a LOD column reads,
/// taken from its fields on every call.
struct Surface<'a> {
    seed: u64,
    heightmap: Heightmap,
    /// The surface rules, sand raised to the beaches.
    rules: SurfaceRules,
    /// The trees and the grounds they grow on, `None` without trees or their blocks.
    trees: Option<(&'a Trees, RangeInclusive<i64>)>,
    /// The sea, `None` without one or without its water.
    sea: Option<Sea>,
    blocks: Blocks,
}

/// The extra blocks of a cell column's ground over its corner maximum at level `k` (module docs).
fn margin(k: u32) -> i64 {
    if k <= 1 {
        0
    } else {
        ((1i64 << k) >> 4).clamp(1, 8)
    }
}

/// The top of `run` inside `y0..=y1`, if the run reaches into it.
fn run_top((lo, hi): (i64, i64), y0: i64, y1: i64) -> Option<i64> {
    (lo <= y1 && hi >= y0).then(|| hi.min(y1))
}

/// Widens `run` to `lo..=hi`.
fn widen(run: &mut (i64, i64), lo: i64, hi: i64) {
    run.0 = run.0.min(lo);
    run.1 = run.1.max(hi);
}

impl Surface<'_> {
    /// [`LodSource::column`] for this surface.
    fn column(
        &self,
        req: &LodRequest,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn {
        let mut memo = scratch
            .memo
            .take()
            .and_then(|m| m.downcast::<Memo>().ok())
            .unwrap_or_default();
        let k = u32::from(req.level);
        let (s, shift) = (req.cell_size(), 5 + k);
        let [x0, _, z0] = req.origin(0);
        self.columns(k, [x0 - s, z0 - s], &mut memo);
        // Layers whose top border row lies under the dirt of every padded column are stone
        // (`lo`); layers whose bottom border row lies above everything are air (`hi`).
        let dirt = self.rules.dirt_depth.max(0);
        let lowest = memo.columns.iter().map(|c| c.height).min().unwrap_or(0) - dirt;
        let highest = memo.columns.iter().map(|c| self.top(c)).max().unwrap_or(0);
        let band = LodColumn {
            lo: (lowest - s) >> shift,
            hi: ((highest + s) >> shift) + 1,
            below: self.blocks.surface.stone,
        };
        let first = (*req.layers.start()).max(band.lo);
        let last = (*req.layers.end()).min(band.hi - 1);
        for layer in first..=last {
            let node = match self.fill(&memo.columns, layer << shift, s, &mut scratch.cells) {
                Fill::Cells => LodNode::Cells(&scratch.cells),
                Fill::Ground => LodNode::Uniform(band.below),
                Fill::Air => LodNode::Uniform(BlockState::AIR),
            };
            emit(layer, node);
        }
        scratch.memo = Some(memo);
        band
    }

    /// Fills `memo.columns` for the padded node of level `k` whose padded min corner is block
    /// `[x, z]`: the corner heights, then the trees.
    fn columns(&self, k: u32, [x, z]: [i64; 2], memo: &mut Memo) {
        let s = 1i64 << k;
        let Memo {
            xs,
            zs,
            noise,
            corners,
            columns,
        } = memo;
        xs.clear();
        xs.extend((0..CORNERS as i64).map(|i| x + i * s));
        zs.clear();
        zs.extend((0..CORNERS as i64).map(|i| z + i * s));
        noise.resize(CORNERS * CORNERS, 0.0);
        corners.resize(CORNERS * CORNERS, 0);
        self.heightmap
            .heights_with(self.seed, xs, zs, noise, corners);
        let margin = margin(k);
        columns.clear();
        columns.extend(
            (0..EDGE)
                .flat_map(|i| (0..EDGE).map(move |j| (i, j)))
                .map(|(i, j)| {
                    let at = |i: usize, j: usize| corners[i * CORNERS + j];
                    let top = at(i, j)
                        .max(at(i + 1, j))
                        .max(at(i, j + 1))
                        .max(at(i + 1, j + 1));
                    Column {
                        height: top,
                        ground: top + margin,
                        leaves: NO_RUN,
                        logs: NO_RUN,
                    }
                }),
        );
        if let Some((trees, grounds)) = &self.trees {
            self.plant(trees, grounds, k, [x, z], columns);
        }
    }

    /// The height of the level-`k` cell column holding block column `(x, z)`, as
    /// [`Surface::columns`] computes it: a pure function of the cell.
    fn cell_height(&self, k: u32, x: i64, z: i64) -> i64 {
        let s = 1i64 << k;
        let (cx, cz) = (x >> k << k, z >> k << k);
        let h = |x, z| self.heightmap.height_at(self.seed, x, z);
        h(cx, cz)
            .max(h(cx + s, cz))
            .max(h(cx, cz + s))
            .max(h(cx + s, cz + s))
    }

    /// Adds the trees that reach into the padded node at `[x, z]` (block min corner) to
    /// `columns`.
    fn plant(
        &self,
        trees: &Trees,
        grounds: &RangeInclusive<i64>,
        k: u32,
        [x, z]: [i64; 2],
        columns: &mut [Column],
    ) {
        let span = (EDGE as i64) << k;
        let margin = margin(k);
        // One tree cell in `stride^2` at level 5 and up: about (34 * 16 / 8)^2 cells per call.
        let stride = ((1i64 << k) >> 4).max(1);
        let cells = |lo: i64| {
            let first = lo.div_euclid(Trees::CELL);
            let first = first + (stride - first.rem_euclid(stride)) % stride;
            (first..=(lo + span - 1).div_euclid(Trees::CELL)).step_by(stride as usize)
        };
        let column_of = |bx: i64, bz: i64| {
            let (i, j) = ((bx - x) >> k, (bz - z) >> k);
            ((0..EDGE as i64).contains(&i) && (0..EDGE as i64).contains(&j))
                .then(|| i as usize * EDGE + j as usize)
        };
        for cx in cells(x) {
            for cz in cells(z) {
                let tree = trees.tree_in_cell(self.seed, cx, cz, |tx, tz| {
                    if k <= 2 {
                        let ground = self.heightmap.height_at(self.seed, tx, tz);
                        return grounds.contains(&ground).then_some(ground);
                    }
                    // The corners decide whether it grows (they overshoot the trunk column, so
                    // a beach under a grass corner may grow one); the margin keeps its top on
                    // or over the real one.
                    let height = match column_of(tx, tz) {
                        Some(c) => columns[c].height,
                        None => self.cell_height(k, tx, tz),
                    };
                    grounds.contains(&height).then_some(height + margin)
                });
                if let Some(tree) = tree {
                    add_tree(&tree, columns, column_of);
                }
            }
        }
    }

    /// The highest block of a cell column: ground, trees or sea.
    fn top(&self, c: &Column) -> i64 {
        let sea = self
            .sea
            .filter(|sea| sea.covers(c.ground))
            .map_or(i64::MIN, |sea| sea.level);
        c.ground.max(c.leaves.1).max(c.logs.1).max(sea)
    }
}

/// Adds the log and leaf runs of `tree` to the cell columns `column_of` finds (see
/// [`Tree::blocks`]: the trunk, then the canopy around its top two logs and over it).
fn add_tree(tree: &Tree, columns: &mut [Column], column_of: impl Fn(i64, i64) -> Option<usize>) {
    let top = tree.top();
    let r = Trees::CANOPY_RADIUS;
    for dx in -r..=r {
        for dz in -r..=r {
            let (ax, az) = (dx.abs(), dz.abs());
            let Some(c) = column_of(tree.x + dx, tree.z + dz) else {
                continue;
            };
            let c = &mut columns[c];
            match (ax.max(az), ax == az) {
                // The trunk, and two leaves over it.
                (0, _) => {
                    widen(&mut c.logs, tree.ground + 1, top);
                    widen(&mut c.leaves, top + 1, top + 2);
                }
                // Around the trunk: four layers, three at the corners.
                (1, true) => widen(&mut c.leaves, top - 1, top + 1),
                (1, false) => widen(&mut c.leaves, top - 1, top + 2),
                // The outer ring of the two lower layers, without its corners.
                (_, false) => widen(&mut c.leaves, top - 1, top),
                (_, true) => {}
            }
        }
    }
}

/// What [`Surface::fill`] found in a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    /// The cells.
    Cells,
    /// No air anywhere and no fluid inside the node (none in its border either at level 1, for
    /// the fluid walls): no face, no skirt, no wall, nothing to draw.
    Ground,
    /// All air.
    Air,
}

impl Surface<'_> {
    /// The state of the cell `y0..y0 + s` of cell column `c` (module docs).
    fn cell(&self, c: &Column, y0: i64, s: i64) -> BlockState {
        let b = &self.blocks;
        let y1 = y0 + s - 1;
        let t = c.ground;
        let sea = self.sea.filter(|sea| sea.covers(t));
        let ground = (y0 <= t).then(|| y1.min(t));
        let (leaves, logs) = (run_top(c.leaves, y0, y1), run_top(c.logs, y0, y1));
        let tree = leaves.max(logs);
        if ground.is_none() && tree.is_none() {
            // No ground: water if the sea fills half of the cell.
            return match sea {
                Some(sea) if 2 * (sea.level.min(y1) - y0.max(t + 1) + 1) >= s => b.water,
                _ => BlockState::AIR,
            };
        }
        // The sea over the ground, one block deep over every column at least.
        if sea.is_some() && t < y1 {
            return b.water;
        }
        // The surface block follows the corners' height. In the cell holding that height or
        // over it, a tree block as high wins (a canopy over the ground of its cell), leaves over
        // a log; under it the ground fills the cell's top layer.
        match ground {
            Some(_) if y1 < c.height || tree.is_none_or(|top| top < c.height) => {
                self.rules.block_at(&b.surface, c.height, y1.min(c.height))
            }
            _ if leaves >= logs => b.leaves,
            _ => b.log,
        }
    }

    /// Fills `cells` with the node whose cell row 0 starts at block `y` (the padded rows -1 to
    /// 32 included) from the cell columns, cells `s` blocks high.
    fn fill(&self, columns: &[Column], y: i64, s: i64, cells: &mut LodCells) -> Fill {
        let dirt = self.rules.dirt_depth.max(0);
        let (mut air, mut fluid, mut fluid_inside, mut solid) = (false, false, false, false);
        // Padded row `r` (0..34) starts at block `bottom + r * s`.
        let bottom = y - s;
        let row = |at: i64| (at - bottom).div_euclid(s).clamp(0, EDGE as i64) as usize;
        for (i, c) in columns.iter().enumerate() {
            let inner = (1..=32).contains(&(i / EDGE)) && (1..=32).contains(&(i % EDGE));
            // Rows wholly under the dirt are stone; rows over everything are air.
            let stone = row(c.height - dirt);
            let open = row(self.top(c) + s).max(stone);
            for r in 0..stone {
                cells.0[i + r * LAYER] = self.blocks.surface.stone;
            }
            for r in stone..open {
                let state = self.cell(c, bottom + r as i64 * s, s);
                cells.0[i + r * LAYER] = state;
                if state == BlockState::AIR {
                    air = true;
                } else if self.sea.is_some() && state == self.blocks.water {
                    fluid = true;
                    fluid_inside |= inner && (1..=32).contains(&r);
                } else {
                    solid = true;
                }
            }
            for r in open..EDGE {
                cells.0[i + r * LAYER] = BlockState::AIR;
            }
            solid |= stone > 0;
            air |= open < EDGE;
        }
        if !air && !fluid_inside && (s > 2 || !fluid) {
            Fill::Ground
        } else if !fluid && !solid {
            Fill::Air
        } else {
            Fill::Cells
        }
    }
}

impl Blocks {
    /// The blocks of `registry`; the surface ones must be there (named after `generator` if
    /// not), water, log and leaves may be missing (no sea, no trees).
    fn new(registry: &BlockRegistry, generator: &str) -> Self {
        let or_air = |name| registry.by_name(name).unwrap_or(BlockState::AIR);
        Self {
            surface: SurfaceBlocks::new(registry, generator),
            water: or_air("boxel:water"),
            log: or_air("boxel:log"),
            leaves: or_air("boxel:leaves"),
        }
    }

    fn has_water(&self) -> bool {
        self.water != BlockState::AIR
    }

    fn has_trees(&self) -> bool {
        self.log != BlockState::AIR && self.leaves != BlockState::AIR
    }
}

/// `rules` with sand up to the beaches of `sea`.
fn beaches(mut rules: SurfaceRules, sea: Option<Sea>) -> SurfaceRules {
    if let Some(sea) = sea {
        rules.sand_level = rules.sand_level.max(sea.level.saturating_add(sea.beach));
    }
    rules
}

impl LodSource for TerrainGenerator {
    /// The heightmap and its surface; the bedrock floor and the air under it count as stone.
    fn column(
        &self,
        req: &LodRequest,
        registry: &BlockRegistry,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn {
        Surface {
            seed: self.seed,
            heightmap: self.heightmap(),
            rules: SurfaceRules::from_terrain(&self.settings),
            trees: None,
            sea: None,
            blocks: Blocks::new(registry, "TerrainGenerator"),
        }
        .column(req, scratch, emit)
    }
}

impl LodSource for ForestGenerator {
    /// The terrain with its trees and sea as
    /// [`generate`](boxel_worldgen::WorldGenerator::generate) places them (water only with
    /// `boxel:water` in the registry, trees only with `boxel:log` and `boxel:leaves`); the floor
    /// counts as stone.
    fn column(
        &self,
        req: &LodRequest,
        registry: &BlockRegistry,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn {
        let blocks = Blocks::new(registry, "ForestGenerator");
        let sea = self.sea.filter(|_| blocks.has_water());
        let rules = SurfaceRules::from_terrain(&self.terrain.settings);
        Surface {
            seed: self.terrain.seed,
            heightmap: self.terrain.heightmap(),
            rules: beaches(rules, sea),
            trees: blocks.has_trees().then(|| (&self.trees, self.grounds())),
            sea,
            blocks,
        }
        .column(req, scratch, emit)
    }
}

impl LodSource for OverworldGenerator {
    /// The heightmap, surface, sea and trees, without the caves: stone down to `i64::MIN`.
    fn column(
        &self,
        req: &LodRequest,
        registry: &BlockRegistry,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn {
        let blocks = Blocks::new(registry, "OverworldGenerator");
        let trees = self.settings.trees.as_ref().filter(|_| blocks.has_trees());
        Surface {
            seed: self.seed,
            heightmap: self.settings.heightmap.clone(),
            rules: self.rules(),
            trees: trees.map(|t| (t, self.tree_grounds())),
            sea: self.settings.sea.filter(|_| blocks.has_water()),
            blocks,
        }
        .column(req, scratch, emit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_voxel::lod::{LodClass, LodClasses};
    use boxel_worldgen::lod::LOD_PADDED_VOLUME;

    /// The surfaces of the three generators with their defaults.
    fn surfaces<'a>(
        forest: &'a ForestGenerator,
        overworld: &'a OverworldGenerator,
        terrain: &'a TerrainGenerator,
        registry: &BlockRegistry,
    ) -> [Surface<'a>; 3] {
        let blocks = Blocks::new(registry, "test");
        let forest_sea = forest.sea.filter(|_| blocks.has_water());
        [
            Surface {
                seed: forest.terrain.seed,
                heightmap: forest.terrain.heightmap(),
                rules: beaches(
                    SurfaceRules::from_terrain(&forest.terrain.settings),
                    forest_sea,
                ),
                trees: Some((&forest.trees, forest.grounds())),
                sea: forest_sea,
                blocks,
            },
            Surface {
                seed: overworld.seed,
                heightmap: overworld.settings.heightmap.clone(),
                rules: overworld.rules(),
                trees: overworld
                    .settings
                    .trees
                    .as_ref()
                    .map(|t| (t, overworld.tree_grounds())),
                sea: overworld.settings.sea,
                blocks,
            },
            Surface {
                seed: terrain.seed,
                heightmap: terrain.heightmap(),
                rules: SurfaceRules::from_terrain(&terrain.settings),
                trees: None,
                sea: None,
                blocks,
            },
        ]
    }

    /// Columns to look at: the origin, a lake (around (248, -256)), hills, and far out.
    fn requests() -> Vec<LodRequest> {
        let mut out = Vec::new();
        for level in 1..=6u8 {
            for [x, z] in [
                [0i64, 0],
                [248, -256],
                [-3392, 1280],
                [1 << 20, -(1 << 21)],
                [(1 << 40) + 77, -(1 << 36)],
            ] {
                let shift = 5 + u32::from(level);
                out.push(LodRequest {
                    dim: boxel_voxel::DimensionId(0),
                    level,
                    column: [x >> shift, z >> shift],
                    layers: -8..=8,
                });
            }
        }
        out
    }

    /// The bulk fill gives the cell rule of each cell, and the layers outside the band (and the
    /// uniform ones inside it) are uniform over their whole padded block.
    #[test]
    fn fill_and_band_follow_the_cells() {
        let registry = crate::blocks::registry();
        let classes = LodClasses::from_registry(&registry);
        let (forest, overworld, terrain) = (
            ForestGenerator::default(),
            OverworldGenerator::default(),
            TerrainGenerator::default(),
        );
        for (g, surface) in surfaces(&forest, &overworld, &terrain, &registry)
            .iter()
            .enumerate()
        {
            for req in requests() {
                let k = u32::from(req.level);
                let (s, shift) = (req.cell_size(), 5 + k);
                let [x0, _, z0] = req.origin(0);
                let mut scratch = LodScratch::default();
                let mut emitted = Vec::new();
                let band = surface.column(&req, &mut scratch, &mut |layer, node| {
                    emitted.push((
                        layer,
                        match node {
                            LodNode::Uniform(state) => Err(state),
                            LodNode::Cells(cells) => Ok(cells.clone()),
                        },
                    ));
                });
                let at = format!("generator {g} level {k} column {:?}", req.column);
                assert!(band.lo < band.hi, "{at}: {band:?}");
                assert_eq!(band.below, surface.blocks.surface.stone, "{at}");
                // Exactly the band's layers, in order.
                let layers: Vec<i64> = emitted.iter().map(|e| e.0).collect();
                let want: Vec<i64> = (band.lo.max(-8)..band.hi.min(9)).collect();
                assert_eq!(layers, want, "{at}: {band:?}");
                let mut memo = Memo::default();
                surface.columns(k, [x0 - s, z0 - s], &mut memo);
                let mut cells = LodCells::new();
                for layer in band.lo - 3..band.hi + 3 {
                    let fill = surface.fill(&memo.columns, layer << shift, s, &mut cells);
                    // Every cell is the rule's, the shortcuts included.
                    for (i, c) in memo.columns.iter().enumerate() {
                        for r in 0..EDGE {
                            let y0 = (layer << shift) + (r as i64 - 1) * s;
                            assert_eq!(
                                cells.0[i + r * LAYER],
                                surface.cell(c, y0, s),
                                "{at} layer {layer} column {i} row {r}"
                            );
                        }
                    }
                    let class = |state| classes.class(state);
                    match band.uniform_at(layer) {
                        Some(state) => assert_eq!(cells.uniform(), Some(state), "{at} {layer}"),
                        None => {
                            let emitted = &emitted.iter().find(|e| e.0 == layer);
                            match emitted.map(|e| &e.1) {
                                Some(Ok(c)) => assert_eq!(c, &cells, "{at} {layer}"),
                                Some(Err(state)) if *state == BlockState::AIR => {
                                    assert_eq!(fill, Fill::Air);
                                    assert_eq!(cells.uniform(), Some(BlockState::AIR));
                                }
                                // No air and ground inside, whatever its states: nothing to
                                // draw.
                                Some(Err(state)) => {
                                    assert_eq!((fill, *state), (Fill::Ground, band.below));
                                    for y in -1..=32 {
                                        for x in -1..=32 {
                                            for z in -1..=32 {
                                                let c = class(cells.get(x, y, z));
                                                let inside =
                                                    [x, y, z].iter().all(|a| (0..32).contains(a));
                                                assert_ne!(c, LodClass::Empty, "{at} {layer}");
                                                if inside || k == 1 {
                                                    assert_eq!(c, LodClass::Solid, "{at} {layer}");
                                                }
                                            }
                                        }
                                    }
                                }
                                None => assert!(!(-8..=8).contains(&layer), "{at} {layer}"),
                            }
                        }
                    }
                }
                assert_eq!(cells.0.len(), LOD_PADDED_VOLUME);
            }
        }
    }

    /// The sampled grounds and trees do not depend on the buffers a worker kept.
    #[test]
    fn memo_reuse_changes_nothing() {
        let registry = crate::blocks::registry();
        let forest = ForestGenerator::default();
        let mut warm = LodScratch::default();
        for req in requests() {
            let run = |scratch: &mut LodScratch| {
                let mut out = Vec::new();
                let band = forest.column(&req, &registry, scratch, &mut |layer, node| {
                    out.push(match node {
                        LodNode::Uniform(state) => (layer, Err(state)),
                        LodNode::Cells(cells) => (layer, Ok(cells.clone())),
                    });
                });
                (band, out)
            };
            assert_eq!(run(&mut warm), run(&mut LodScratch::default()), "{req:?}");
        }
    }
}
