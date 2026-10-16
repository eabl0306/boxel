//! The decoration hook of a realm (ADR 0039 sec. 9), in its minimal form: the fill stage (the
//! rock, [`FillStage`]) and the surface stage (the top of each column, [`SurfaceStage`]), traits
//! the game implements, and [`decorate`], which builds a chunk from its columns' grounds with them.
//!
//! The rest of the decoration layer (shape modifiers, biomes, carvers, features, structures and
//! post steps, each a registry of stages by `(phase, priority, id)`) comes with the steps that
//! first need it; none of it has a slot here yet.

use super::kind::Context;
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

/// The top of each column of a realm (ADR 0039 sec. 9, SURFACE): soil, sand, snow.
pub trait SurfaceStage<Cx: Context>: GenEntry + Send + Sync {
    /// How far it reaches around each column's ground.
    fn reach(&self) -> SurfaceReach;

    /// Rewrites `out`, the chunk's states (`y << 10 | x << 5 | z`, with the rock, the fluid and
    /// the air in place), inside each column's reach. Called only for chunks its reach touches.
    fn apply(
        &self,
        columns: &ChunkColumns<'_, Cx>,
        registry: &BlockRegistry,
        out: &mut [BlockState],
    );
}

/// A realm's decoration: its fill and surface stages, each with the id of its digest line.
pub struct Decoration<Cx: Context> {
    /// The fill stage.
    pub fill: (GenId, Arc<dyn FillStage<Cx>>),
    /// The surface stage.
    pub surface: (GenId, Arc<dyn SurfaceStage<Cx>>),
}

impl<Cx: Context> Decoration<Cx> {
    /// The stages `fill` and `surface`, with their ids.
    pub fn new(
        fill: (GenId, Arc<dyn FillStage<Cx>>),
        surface: (GenId, Arc<dyn SurfaceStage<Cx>>),
    ) -> Self {
        Self { fill, surface }
    }
}

impl<Cx: Context> Clone for Decoration<Cx> {
    fn clone(&self) -> Self {
        Self {
            fill: (self.fill.0.clone(), Arc::clone(&self.fill.1)),
            surface: (self.surface.0.clone(), Arc::clone(&self.surface.1)),
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
        stages.surface.apply(columns, registry, &mut states);
    }
    ChunkData::from_dense(&states)
}
