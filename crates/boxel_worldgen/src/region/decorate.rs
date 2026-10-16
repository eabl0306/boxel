//! The decoration hook of a realm (ADR 0039 sec. 9), in its minimal form: the shape modifiers
//! ([`ShapeModifier`](super::ShapeModifier), on the blended heights), the fill stage (the rock,
//! [`FillStage`]) and the surface stage (the top of each column, [`SurfaceStage`], with its far
//! LOD form [`SurfaceStage::lod_top`]), traits the game implements, and [`decorate`], which builds
//! a chunk from its columns' grounds with them.
//!
//! The rest of the decoration layer (biomes, carvers, features, structures and post steps, each a
//! registry of stages by `(phase, priority, id)`) comes with the steps that first need it; none of
//! it has a slot here yet.

use super::kind::{Context, HeightQ};
use super::modifier::ShapeModifier;
use crate::block;
use crate::registry::{GenEntry, GenId, HashParams, ParamsHasher};
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME,
    ChunkData, Occupancy,
};
use std::sync::Arc;

/// What a fill stage gives a chunk ([`FillStage::rock`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rock {
    /// One state through the whole chunk (most chunks): the chunk stays `Uniform` or 1-bit.
    Uniform(BlockState),
    /// A state per voxel, written to the stage's output.
    Dense,
}

/// The rock of a realm (ADR 0039 sec. 9, FILL): the state of every voxel at or under its
/// column's ground, before the surface stage rewrites the top.
pub trait FillStage<Cx: Context>: GenEntry + Send + Sync {
    /// The rock of the chunk at `origin`, for every voxel of the chunk (the chunk keeps it at or
    /// under each column's ground): [`Rock::Uniform`], or [`Rock::Dense`] with `out` (empty on
    /// entry) resized to [`CHUNK_VOLUME`] states indexed `y << 10 | x << 5 | z`. `floor` is the
    /// realm's floor state.
    ///
    /// A pure function of the voxel and the realm, never of the surface, so a chunk under every
    /// surface (the deep path, which reads no height) and a chunk the surface crosses agree.
    fn rock(
        &self,
        cx: &Cx,
        origin: BlockPos,
        floor: BlockState,
        registry: &BlockRegistry,
        out: &mut Vec<BlockState>,
    ) -> Rock;

    /// Every block name the stage looks up in the registry it gets (besides the realm's floor,
    /// which it is handed), so the world open checks them and a typo fails at load
    /// ([`RegionGenerator::build`](super::RegionGenerator::build)), not in a worker.
    fn block_names(&self) -> Vec<&str>;
}

/// How far a surface stage reaches around a column's top solid block `g`: it may rewrite the
/// blocks from `g - depth + 1` to `g + rise`, both included, and nothing else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SurfaceReach {
    /// Blocks at and under the top solid block.
    pub depth: u32,
    /// Blocks over it.
    pub rise: u32,
}

/// The columns of a chunk, as the column stage leaves them for the decoration.
pub struct ChunkColumns<'a, Cx: Context> {
    /// The chunk's lowest corner.
    pub origin: BlockPos,
    /// Per column `x << 5 | z`: the `y` of its top solid block.
    pub ground: &'a [i64; CHUNK_AREA],
    /// Per column: the `y` of its highest fluid block, or its ground if it is dry.
    pub fluid_top: &'a [i64; CHUNK_AREA],
    /// The realm's Context.
    pub cx: &'a Cx,
    /// Its fields over the chunk's 32 x 32 columns.
    pub fields: &'a Cx::Grid,
}

/// The most runs a surface stage gives a far LOD cell column ([`LodTop`]).
pub const LOD_TOP_RUNS: usize = 4;

/// The top of a far LOD cell column as a surface stage gives it ([`SurfaceStage::lod_top`]):
/// runs of `(state, blocks)` from the column's ground down, top first; a run of 0 blocks ends
/// them. Under the runs lies the realm's floor (the far LOD draws no fill stage: its strata are
/// not seen from far away). The default, no run, is the floor up to the ground.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LodTop {
    /// The runs, top first.
    pub runs: [(BlockState, u32); LOD_TOP_RUNS],
}

impl LodTop {
    /// The runs in use, top first.
    pub fn used(&self) -> impl Iterator<Item = (BlockState, u32)> + '_ {
        self.runs.iter().copied().take_while(|&(_, n)| n > 0)
    }
}

/// The cell columns of a far LOD node, as the region pass leaves them for the surface stage
/// ([`SurfaceStage::lod_top`]): the node's padded `34 x 34` cell columns of `2^level` blocks, each
/// with its ground, its fluid and the corner it takes its look from (the highest one: a steep
/// cell shows what its top shows).
pub struct LodColumns<'a, Cx: Context> {
    /// The LOD level: cells are `2^level` blocks.
    pub level: u8,
    /// The realm's Context.
    pub cx: &'a Cx,
    /// Its fields at the cells' corners ([`Context::lod_grid`]).
    pub fields: &'a Cx::Grid,
    /// The corners' x, in blocks.
    pub xs: &'a [i64],
    /// The corners' z: corner `i * zs.len() + j` is `(xs[i], zs[j])`.
    pub zs: &'a [i64],
    /// The corners' heights (the kinds' blended, then the shape modifiers), in [`HeightQ`] over
    /// the realm's datum: a cell column's relief is its 4 corners' (steep cells show rock), the
    /// same from any node.
    pub heights: &'a [HeightQ],
    /// Per cell column (`i * (zs.len() - 1) + j`, between corners `i..=i + 1` and `j..=j + 1`):
    /// the index of its highest corner.
    pub corner: &'a [u32],
    /// Per cell column: the `y` of its top solid block (its highest corner's, without margin).
    pub ground: &'a [i64],
    /// Per cell column: the `y` of its highest fluid block, or its ground if it is dry (the
    /// realm's fluid rule on its corners).
    pub fluid_top: &'a [i64],
}

/// The top of each column of a realm (ADR 0039 sec. 9, SURFACE): soil, sand, snow.
pub trait SurfaceStage<Cx: Context>: GenEntry + Send + Sync {
    /// How far it reaches around each column's ground.
    fn reach(&self) -> SurfaceReach;

    /// Rewrites `out`, the chunk's states (`y << 10 | x << 5 | z`, with the rock, the fluid and
    /// the air in place), inside each column's reach. Called only for chunks its reach touches;
    /// in debug builds [`decorate`] panics if it changed a block outside the reach.
    fn apply(
        &self,
        columns: &ChunkColumns<'_, Cx>,
        registry: &BlockRegistry,
        out: &mut [BlockState],
    );

    /// Every block name the stage looks up in the registry it gets, so the world open checks
    /// them and a typo fails at load ([`RegionGenerator::build`](super::RegionGenerator::build)),
    /// not in a worker.
    fn block_names(&self) -> Vec<&str>;

    /// The far LOD form (ADR 0039 sec. 10): the top of each cell column of a node, from its
    /// ground down, into `out` (one per cell column, in `columns`' order, all without runs on
    /// entry). A cell column's ground may stand over its highest corner by its margin and the
    /// reach's rise: the far LOD slides the runs up with it, so a coarse cell is Solid wherever a
    /// real one may hold a Solid block (a non-Solid top run included).
    ///
    /// The default leaves every column without runs: the realm's floor up to the ground.
    fn lod_top(&self, columns: &LodColumns<'_, Cx>, registry: &BlockRegistry, out: &mut [LodTop]) {
        let _ = (columns, registry, out);
    }
}

/// A realm's decoration: its shape modifiers, fill stage and surface stage, each with the id of
/// its digest line.
pub struct Decoration<Cx: Context> {
    /// The fill stage.
    pub fill: (GenId, Arc<dyn FillStage<Cx>>),
    /// The surface stage.
    pub surface: (GenId, Arc<dyn SurfaceStage<Cx>>),
    /// The shape modifiers, in any order: they run by `(priority, id)`, the higher priority
    /// first ([`GenEntry::order`]).
    pub modifiers: Vec<(GenId, Arc<dyn ShapeModifier<Cx>>)>,
}

impl<Cx: Context> Decoration<Cx> {
    /// The stages `fill` and `surface`, with their ids, and no shape modifier.
    pub fn new(
        fill: (GenId, Arc<dyn FillStage<Cx>>),
        surface: (GenId, Arc<dyn SurfaceStage<Cx>>),
    ) -> Self {
        Self {
            fill,
            surface,
            modifiers: Vec::new(),
        }
    }

    /// The decoration with the shape modifier `modifier` under `id` too.
    #[must_use]
    pub fn with_modifier(mut self, id: GenId, modifier: Arc<dyn ShapeModifier<Cx>>) -> Self {
        self.modifiers.push((id, modifier));
        self
    }
}

impl<Cx: Context> Clone for Decoration<Cx> {
    fn clone(&self) -> Self {
        Self {
            fill: (self.fill.0.clone(), Arc::clone(&self.fill.1)),
            surface: (self.surface.0.clone(), Arc::clone(&self.surface.1)),
            modifiers: self
                .modifiers
                .iter()
                .map(|(id, m)| (id.clone(), Arc::clone(m)))
                .collect(),
        }
    }
}

/// The realm's floor state everywhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FloorFill;

impl HashParams for FloorFill {
    fn hash_params(&self, _: &mut ParamsHasher) {}
}

impl GenEntry for FloorFill {
    fn version(&self) -> u32 {
        1
    }
}

impl<Cx: Context> FillStage<Cx> for FloorFill {
    fn rock(
        &self,
        _cx: &Cx,
        _origin: BlockPos,
        floor: BlockState,
        _registry: &BlockRegistry,
        _out: &mut Vec<BlockState>,
    ) -> Rock {
        Rock::Uniform(floor)
    }

    fn block_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

/// No surface: the rock reaches the top.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoSurface;

impl HashParams for NoSurface {
    fn hash_params(&self, _: &mut ParamsHasher) {}
}

impl GenEntry for NoSurface {
    fn version(&self) -> u32 {
        1
    }
}

impl<Cx: Context> SurfaceStage<Cx> for NoSurface {
    fn reach(&self) -> SurfaceReach {
        SurfaceReach::default()
    }

    fn apply(&self, _: &ChunkColumns<'_, Cx>, _: &BlockRegistry, _: &mut [BlockState]) {}

    fn block_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

/// Layers of blocks on top of every column, top first: `[("game:grass", 1), ("game:dirt", 3)]`
/// makes the top solid block grass and the 3 under it dirt. Names are resolved against the
/// registry passed to the generator; an unknown one panics there with its name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TopLayers {
    /// `(block name, thickness)`, top first.
    pub layers: Vec<(String, u32)>,
}

impl TopLayers {
    /// The layers `layers`, top first.
    pub fn new(layers: Vec<(String, u32)>) -> Self {
        Self { layers }
    }
}

impl HashParams for TopLayers {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.layers.len());
        for (name, thickness) in &self.layers {
            h.write(name).write(thickness);
        }
    }
}

impl GenEntry for TopLayers {
    fn version(&self) -> u32 {
        1
    }
}

impl<Cx: Context> SurfaceStage<Cx> for TopLayers {
    fn reach(&self) -> SurfaceReach {
        SurfaceReach {
            depth: self.layers.iter().map(|&(_, t)| t).sum(),
            rise: 0,
        }
    }

    fn apply(
        &self,
        columns: &ChunkColumns<'_, Cx>,
        registry: &BlockRegistry,
        out: &mut [BlockState],
    ) {
        let layers: Vec<(BlockState, i64)> = self
            .layers
            .iter()
            .map(|(name, t)| (block(registry, name, "TopLayers"), i64::from(*t)))
            .collect();
        let y0 = columns.origin.y;
        let size = i64::from(CHUNK_SIZE);
        for (c, &g) in columns.ground.iter().enumerate() {
            // From the top solid block down, each layer over its thickness.
            let mut top = g;
            for &(state, t) in &layers {
                let lo = (top - t + 1 - y0).max(0);
                let hi = (top - y0 + 1).min(size);
                for ly in lo..hi {
                    out[((ly as usize) << (2 * CHUNK_SHIFT)) | c] = state;
                }
                top -= t;
            }
        }
    }

    fn block_names(&self) -> Vec<&str> {
        self.layers.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// The same layers on every cell column: exact, as long as there are at most
    /// [`LOD_TOP_RUNS`] (the floor stands in for the ones after).
    fn lod_top(&self, _columns: &LodColumns<'_, Cx>, registry: &BlockRegistry, out: &mut [LodTop]) {
        let mut top = LodTop::default();
        let layers = self.layers.iter().filter(|(_, t)| *t > 0);
        for (run, (name, t)) in top.runs.iter_mut().zip(layers) {
            *run = (block(registry, name, "TopLayers"), *t);
        }
        out.fill(top);
    }
}

/// The stages and states [`decorate`] builds a chunk with.
pub struct Stages<'a, Cx: Context> {
    /// The rock.
    pub fill: &'a dyn FillStage<Cx>,
    /// The top of each column.
    pub surface: &'a dyn SurfaceStage<Cx>,
    /// The realm's floor state, for the fill.
    pub floor: BlockState,
    /// The realm's fluid state, where a column's fluid top is over its ground.
    pub fluid: BlockState,
}

/// The chunk of `columns`: the rock of the fill at and under each column's ground, the fluid up to
/// its fluid top, air above, and the surface stage over it where its reach touches the chunk.
///
/// From the cheapest answer to the dearest: `Uniform` air over every column's reach and fluid;
/// only the fill's rock under every column's reach (`Uniform` when the rock is one state); a
/// 1-bit chunk ([`ChunkData::from_occupancy`]) when the rock is one state and neither the fluid
/// nor the surface touches the chunk; else a dense chunk ([`ChunkData::from_dense`]).
pub fn decorate<Cx: Context>(
    columns: &ChunkColumns<'_, Cx>,
    stages: &Stages<'_, Cx>,
    registry: &BlockRegistry,
) -> ChunkData {
    let y0 = columns.origin.y;
    let y1 = y0 + i64::from(CHUNK_SIZE) - 1;
    let reach = stages.surface.reach();
    let (depth, rise) = (i64::from(reach.depth), i64::from(reach.rise));
    let (mut min_g, mut max_g, mut max_top) = (i64::MAX, i64::MIN, i64::MIN);
    let (mut touched, mut wet) = (false, false);
    for (&g, &w) in columns.ground.iter().zip(columns.fluid_top) {
        min_g = min_g.min(g);
        max_g = max_g.max(g);
        max_top = max_top.max(g.saturating_add(rise)).max(w);
        touched |= depth + rise > 0 && g - depth < y1 && g.saturating_add(rise) >= y0;
        wet |= w > g && g < y1 && w >= y0;
    }
    if y0 > max_top {
        return ChunkData::uniform(BlockState::AIR);
    }
    let mut rock_buf = Vec::new();
    let rock = (y0 <= max_g).then(|| {
        stages.fill.rock(
            columns.cx,
            columns.origin,
            stages.floor,
            registry,
            &mut rock_buf,
        )
    });
    let rock_chunk = |rock: Rock, buf: &[BlockState]| match rock {
        Rock::Uniform(state) => ChunkData::uniform(state),
        Rock::Dense => ChunkData::from_dense(buf),
    };
    match rock {
        // Under every column's reach: the rock alone, as the deep path gives it.
        Some(rock) if y1 <= min_g.saturating_sub(depth) => return rock_chunk(rock, &rock_buf),
        Some(Rock::Uniform(state)) if !touched && !wet => {
            let mut occupancy: Occupancy = [0; CHUNK_AREA];
            for (c, &g) in columns.ground.iter().enumerate() {
                let (lx, lz) = (c >> CHUNK_SHIFT, c & (CHUNK_SIZE as usize - 1));
                let layers = (g - y0 + 1).clamp(0, i64::from(CHUNK_SIZE)) as usize;
                for word in occupancy[lx..]
                    .iter_mut()
                    .step_by(CHUNK_SIZE as usize)
                    .take(layers)
                {
                    *word |= 1 << lz;
                }
            }
            return ChunkData::from_occupancy(&occupancy, state, BlockState::AIR);
        }
        // Over every ground with nothing reaching the chunk (not reached: `y0 <= max_top`).
        None if !touched && !wet => return ChunkData::uniform(BlockState::AIR),
        _ => {}
    }
    let mut states = vec![BlockState::AIR; CHUNK_VOLUME];
    for (c, (&g, &w)) in columns.ground.iter().zip(columns.fluid_top).enumerate() {
        let top = (g.max(w) - y0 + 1).clamp(0, i64::from(CHUNK_SIZE)) as usize;
        for ly in 0..top {
            let i = (ly << (2 * CHUNK_SHIFT)) | c;
            states[i] = if y0 + ly as i64 <= g {
                match rock {
                    Some(Rock::Uniform(state)) => state,
                    Some(Rock::Dense) => rock_buf[i],
                    None => unreachable!("a voxel at or under a ground has rock"),
                }
            } else {
                stages.fluid
            };
        }
    }
    if touched {
        let before = cfg!(debug_assertions).then(|| states.clone());
        stages.surface.apply(columns, registry, &mut states);
        if let Some(before) = before {
            check_reach(columns, reach, &before, &states);
        }
    }
    ChunkData::from_dense(&states)
}

/// The debug check of the surface stage: it changed only blocks inside each column's reach. A
/// stage runs only in the chunks its declared reach touches, so a block it writes outside would
/// show in some chunks and not in their neighbors (and never in the deep path).
fn check_reach<Cx: Context>(
    columns: &ChunkColumns<'_, Cx>,
    reach: SurfaceReach,
    before: &[BlockState],
    after: &[BlockState],
) {
    let origin = columns.origin;
    for (c, &g) in columns.ground.iter().enumerate() {
        let lo = g.saturating_sub(reach.depth.into()).saturating_add(1);
        let hi = g.saturating_add(reach.rise.into());
        for ly in 0..CHUNK_SIZE as usize {
            let (i, y) = ((ly << (2 * CHUNK_SHIFT)) | c, origin.y + ly as i64);
            assert!(
                before[i] == after[i] || (lo..=hi).contains(&y),
                "span check: the surface stage changed the block at ({}, {y}, {}), outside its \
                 reach {lo}..={hi} (depth {}, rise {} around the ground at y {g}): the chunks it \
                 does not reach would not show it",
                origin.x + (c >> CHUNK_SHIFT) as i64,
                origin.z + (c & (CHUNK_SIZE as usize - 1)) as i64,
                reach.depth,
                reach.rise,
            );
        }
    }
}
