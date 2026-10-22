//! The decoration hook of a realm (ADR 0039 sec. 9): the shape modifiers
//! ([`ShapeModifier`](super::ShapeModifier), on the blended heights), the fill stage (the rock,
//! [`FillStage`]), the surface stage (the top of each column, [`SurfaceStage`], with its far LOD
//! form [`SurfaceStage::lod_top`]), the carvers (caves, [`CarverStage`]) and the features (trees
//! and plants, [`FeatureStage`], with their far LOD form [`FeatureStage::lod_runs`]), traits the
//! game implements, and [`decorate`], which builds a chunk from its columns' grounds with them.
//!
//! The rest of the decoration layer (biomes, structures and post steps, each a registry of stages
//! by `(phase, priority, id)`) comes with the steps that first need it; none of it has a slot here
//! yet.

use super::kind::{Context, HeightQ};
use super::modifier::ShapeModifier;
use crate::block;
use crate::field::Rect;
use crate::registry::{GenEntry, GenId, HashParams, ParamsHasher};
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME,
    ChunkData, Occupancy,
};
use std::sync::Arc;

/// The widest apron a carver may declare ([`CarveReach::apron`]): one chunk, so its columns lie
/// in the chunk columns next to its own.
pub const MAX_APRON: u32 = CHUNK_SIZE as u32;

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

/// The grounds and fluid tops of columns outside a chunk, computed on demand
/// ([`ChunkColumns::tops`]): the region generator computes them as their own chunks do (the same
/// Context, kinds, blend, shape modifiers and fluid rule), bit for bit.
pub trait ColumnsAround {
    /// Per column of the grid `xs` x `zs` (point `i * zs.len() + j` at `(xs[i], zs[j])`): the `y`
    /// of its top solid block into `ground` and of its highest fluid block (its ground if it is
    /// dry) into `fluid_top`.
    fn tops(&self, xs: &[i64], zs: &[i64], ground: &mut [i64], fluid_top: &mut [i64]);
}

/// The columns of a chunk, as the column stage leaves them for the decoration, and the columns
/// around it as far as the stage reading them declares ([`ChunkColumns::tops`]).
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
    /// The columns around the chunk.
    around: &'a dyn ColumnsAround,
    /// How far around the chunk the stage reading these may read columns, in blocks.
    apron: u32,
    /// A block at or under every ground of the chunk's column and of the carvers' apron
    /// ([`CarveChunk::apron_lo`]).
    apron_lo: i64,
    /// The stage reading these, for messages.
    reader: &'a str,
}

impl<'a, Cx: Context> ChunkColumns<'a, Cx> {
    /// The columns of the chunk at `origin`, with `around` for the columns outside it. Its apron
    /// is 0 until [`decorate`] hands it to a stage that declares one (a carver's
    /// [`CarveReach::apron`]), and the carvers' [`CarveChunk::apron_lo`] is `i64::MIN` (no bound)
    /// until the region generator gives its chunk column's.
    pub fn new(
        origin: BlockPos,
        ground: &'a [i64; CHUNK_AREA],
        fluid_top: &'a [i64; CHUNK_AREA],
        cx: &'a Cx,
        fields: &'a Cx::Grid,
        around: &'a dyn ColumnsAround,
    ) -> Self {
        Self {
            origin,
            ground,
            fluid_top,
            cx,
            fields,
            around,
            apron: 0,
            apron_lo: i64::MIN,
            reader: "a stage",
        }
    }

    /// The same columns with `apron_lo` for the carvers ([`CarveChunk::apron_lo`]).
    pub(super) const fn with_apron_lo(mut self, apron_lo: i64) -> Self {
        self.apron_lo = apron_lo;
        self
    }

    /// How far around the chunk the stage holding these may read columns, in blocks (Chebyshev):
    /// its declared apron.
    pub const fn apron(&self) -> u32 {
        self.apron
    }

    /// The same columns for `reader`, a stage of apron `apron`.
    fn read_by(&self, reader: &'a str, apron: u32) -> Self {
        Self {
            origin: self.origin,
            ground: self.ground,
            fluid_top: self.fluid_top,
            cx: self.cx,
            fields: self.fields,
            around: self.around,
            apron,
            apron_lo: self.apron_lo,
            reader,
        }
    }

    /// The grounds and fluid tops of the grid `xs` x `zs` ([`ColumnsAround::tops`]), every column
    /// within the stage's apron of the chunk ([`ChunkColumns::apron`]; the chunk's own columns
    /// too, with the bits of [`ChunkColumns::ground`]), computed on demand as their own chunks do.
    ///
    /// A stage reads the surface around its chunk only as far as it declares: the deep path
    /// ([`CarverStage::column_reach`]) counts the grounds of its apron. In debug builds a column
    /// past the apron panics.
    ///
    /// # Panics
    ///
    /// If `ground` or `fluid_top` is not one entry per column of the grid.
    pub fn tops(&self, xs: &[i64], zs: &[i64], ground: &mut [i64], fluid_top: &mut [i64]) {
        let n = xs.len() * zs.len();
        assert!(
            ground.len() == n && fluid_top.len() == n,
            "ChunkColumns::tops: one ground and one fluid top per column"
        );
        if cfg!(debug_assertions) {
            let (a, size) = (i64::from(self.apron), i64::from(CHUNK_SIZE));
            let out = |v: i64, o: i64| (o - v).max(v - (o + size - 1)).max(0);
            for &x in xs {
                for &z in zs {
                    let past = out(x, self.origin.x).max(out(z, self.origin.z));
                    assert!(
                        past <= a,
                        "span check: {} read the column ({x}, {z}), {past} blocks from the chunk \
                         at ({}, {}, {}), past its apron of {a}: the deep path does not count \
                         that column's ground",
                        self.reader,
                        self.origin.x,
                        self.origin.y,
                        self.origin.z,
                    );
                }
            }
        }
        self.around.tops(xs, zs, ground, fluid_top);
    }
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
/// ([`SurfaceStage::lod_top`]) and the feature stages ([`FeatureStage::lod_runs`]): the node's
/// padded `34 x 34` cell columns of `2^level` blocks, each with its ground, its fluid and the
/// corner it takes its look from (the highest one: a steep cell shows what its top shows).
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

/// How far a carver reads the surface ([`CarverStage::reach`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CarveReach {
    /// Blocks under the lowest ground of the columns it reads (its chunk's and its apron's) down
    /// to which its rules read the surface (fades by depth, entrances, a roof): deeper, its mask
    /// is the one it gives without columns, so a chunk at least this far under every such ground
    /// takes the deep path. The most over every chunk column: a chunk column may declare less
    /// ([`CarverStage::column_reach`]).
    pub surface: u32,
    /// How far around its chunk it reads columns ([`ChunkColumns::tops`]), in blocks
    /// (Chebyshev), at most [`MAX_APRON`].
    pub apron: u32,
}

/// Which voxels of a chunk a carver leaves: bit `z` of word `y * 32 + x` (the layout of
/// [`ChunkData::from_occupancy`]) is 1 where the voxel stays, 0 where it is carved.
///
/// A carver only turns solid voxels into air: the chunk path carves a voxel only where it holds
/// neither air nor the realm's fluid, so no mask adds a block or removes the fluid. Keeping the
/// fluid out of a cave (a sea floor opened under it, a surface stage's block inside it turned to
/// air) is the carver's own rule, a water shell: it reads the fluid tops of its columns and of its
/// apron ([`ChunkColumns::tops`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CarveMask {
    /// No voxel is carved.
    Solid,
    /// Every voxel is carved.
    Air,
    /// Some voxels are carved.
    Mixed(Box<Occupancy>),
}

impl CarveMask {
    /// The mask of `occupancy`: [`CarveMask::Solid`] or [`CarveMask::Air`] when all its bits are
    /// equal.
    pub fn of(occupancy: Box<Occupancy>) -> Self {
        if occupancy.iter().all(|&w| w == u32::MAX) {
            Self::Solid
        } else if occupancy.iter().all(|&w| w == 0) {
            Self::Air
        } else {
            Self::Mixed(occupancy)
        }
    }

    /// Both masks at once: a voxel stays where both leave it.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Air, _) | (_, Self::Air) => Self::Air,
            (Self::Solid, m) | (m, Self::Solid) => m,
            (Self::Mixed(mut a), Self::Mixed(b)) => {
                for (a, b) in a.iter_mut().zip(b.iter()) {
                    *a &= b;
                }
                Self::of(a)
            }
        }
    }
}

/// What a carver gets for a chunk ([`CarverStage::carve`]).
pub struct CarveChunk<'a, Cx: Context> {
    /// The realm's Context.
    pub cx: &'a Cx,
    /// The carver's seed: the realm's seed mixed with the carver's salt.
    pub seed: u64,
    /// The chunk's lowest corner.
    pub origin: BlockPos,
    /// The chunk's columns, with its apron's through [`ChunkColumns::tops`]; `None` in the deep
    /// path, for a chunk at least its chunk column's [`CarverStage::column_reach`] under every
    /// ground it may read.
    pub columns: Option<&'a ChunkColumns<'a, Cx>>,
    /// A block at or under every ground the carvers may read: the lowest ground of the chunk's
    /// column and of the realm's carvers' widest apron around it, from the hooks
    /// ([`LocalBounds::apron_lo`]), the same in the column stage and in the deep path. In the
    /// deep path the chunk lies the carvers' widest reach over its chunk column
    /// ([`CarverStage::column_reach`]) under it: that is what makes it deep.
    ///
    /// A carver reads it to skip its apron where no rule of it can reach, without guessing: a
    /// water shell needs no ring when it is at or over the realm's fluid level (no column there
    /// is wet), nor in a chunk wholly more than the shell under it. It is a bound, not the
    /// grounds, and it moves when the hooks tighten: a mask that depends on it beyond such
    /// shortcuts changes with them.
    ///
    /// [`LocalBounds::apron_lo`]: super::LocalBounds::apron_lo
    pub apron_lo: i64,
    /// The realm's floor state.
    pub floor: BlockState,
    /// The realm's fluid state (air for a realm without fluid): the carve never drains it.
    pub fluid: BlockState,
    /// The blocks, for the names the carver declares ([`CarverStage::block_names`]).
    pub registry: &'a BlockRegistry,
}

/// A carver of a realm (ADR 0039 sec. 9, CARVERS): caves, ravines. It runs after the surface
/// stage in every chunk that holds a solid voxel, the deep path's included, and turns solid
/// voxels into air. The far LOD leaves carvers out.
///
/// Its mask must be a pure function of its seed, the chunk, the Context and the columns it reads
/// (its chunk's and its apron's), so a chunk and its neighbors agree; and it may read the surface
/// only down to its reach over the chunk's column ([`CarverStage::column_reach`], at most its
/// declared [`CarveReach::surface`]): in debug builds a chunk that lies that far under every
/// ground it may read checks that its mask with columns is its mask without them.
pub trait CarverStage<Cx: Context>: GenEntry + Send + Sync {
    /// How far it reads the surface.
    fn reach(&self) -> CarveReach;

    /// How far its rules read the surface in the chunks of one chunk column, at most the
    /// declared [`CarveReach::surface`]: `area` is the chunk column with the carver's apron around
    /// it ([`CarveReach::apron`]), the columns it may read for those chunks. A chunk of the chunk
    /// column at least this far under every ground it may read takes the deep path, so a carver
    /// whose entrances (its deepest rules) lie only in some regions gives a smaller reach away
    /// from them, and the deep path starts higher there. It must count every rule that reads the
    /// surface in those chunks, one decided at a column of the apron included, and be a pure
    /// function of the Context, its seed and `area`.
    ///
    /// In debug builds a chunk of the column stage that lies this far under every ground it may
    /// read checks that its mask with columns is its mask without them, and a reach over the
    /// declared one panics. By default the declared [`CarveReach::surface`].
    fn column_reach(&self, cx: &Cx, seed: u64, area: Rect) -> u32 {
        let _ = (cx, seed, area);
        self.reach().surface
    }

    /// The voxels of `chunk` it leaves.
    fn carve(&self, chunk: &CarveChunk<'_, Cx>) -> CarveMask;

    /// Every block name the carver looks up in the registry it gets, so the world open checks
    /// them and a typo fails at load ([`RegionGenerator::build`](super::RegionGenerator::build)),
    /// not in a worker.
    fn block_names(&self) -> Vec<&str>;
}

/// A carver as a realm runs it: its id (for messages), its stage and its seed (the realm's seed
/// mixed with the carver's salt).
pub struct RealmCarver<Cx: Context> {
    /// The carver's id.
    pub id: GenId,
    /// The stage.
    pub stage: Arc<dyn CarverStage<Cx>>,
    /// Its seed.
    pub seed: u64,
}

impl<Cx: Context> RealmCarver<Cx> {
    /// Its [`CarverStage::column_reach`] in the chunks of chunk column `(x, z)` (in chunks): over
    /// the chunk column and its apron around it. In debug builds a reach over the declared
    /// [`CarveReach::surface`], the carver's worst case, panics.
    pub(super) fn column_reach(&self, cx: &Cx, (x, z): (i64, i64)) -> u32 {
        let reach = self.stage.reach();
        let (a, size) = (i64::from(reach.apron), i64::from(CHUNK_SIZE));
        let (x0, z0) = (x << CHUNK_SHIFT, z << CHUNK_SHIFT);
        let area = Rect::new(x0 - a, z0 - a, x0 + size - 1 + a, z0 + size - 1 + a);
        let r = self.stage.column_reach(cx, self.seed, area);
        if cfg!(debug_assertions) {
            assert!(
                r <= reach.surface,
                "span check: carver {} gives the columns {area:?} a surface reach of {r}, over \
                 its declared surface reach {}",
                self.id,
                reach.surface,
            );
        }
        r
    }
}

/// How far a feature stage reaches ([`FeatureStage::reach`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FeatureReach {
    /// Blocks over each column's ground it may write: it writes only `ground + 1..=ground +
    /// height` of each column, the column a block lands on (a canopy over a lower neighbor
    /// counts the slope). At least 1: the world open refuses 0.
    pub height: u32,
    /// How far around its chunk it reads columns ([`ChunkColumns::tops`]), in blocks
    /// (Chebyshev), at most [`MAX_APRON`]: the anchors outside its chunk whose features reach
    /// into it (a canopy across a chunk border).
    pub apron: u32,
}

/// A feature stage of a realm (ADR 0039 sec. 9, FEATURES): trees, plants, boulders. It runs
/// after the carvers in every chunk its reach touches, the features of other chunks included (a
/// tree across a layer border is written by both chunks, each its own blocks), and writes only
/// over each column's ground, as high as it declares. It never runs in the deep path: nothing it
/// writes lies under a ground.
///
/// Its anchors lie in cells aligned with the chunks (8 or 16 blocks), and an anchor's column (its
/// fields, its biome) decides its feature. Its blocks must be a pure function of its seed, the
/// Context and the columns it reads (its chunk's and its apron's), so every chunk a tree crosses
/// writes the same tree. Of the chunk's states it may read the one of each block it writes (a
/// leaf only over air): the chunk that holds the block decides it. Any other state (the surface
/// stage's top block under a trunk, a carver's cave there) only the chunk that holds it sees, so
/// a feature decided from it must lie wholly inside that chunk, its layer included (a plant over
/// a ground on the top block of a layer lies in the next one). A feature that may cross a chunk
/// border decides from its columns instead (the surface class of its anchor's column, the
/// carver's own rule there), as the demo's trees decide their grass from the ground's height.
pub trait FeatureStage<Cx: Context>: GenEntry + Send + Sync {
    /// How far it reaches over the ground and around its chunk.
    fn reach(&self) -> FeatureReach;

    /// The most blocks it writes over the ground at the columns of `area` (a chunk column), at
    /// most [`FeatureReach::height`]: 0 where nothing grows. The chunk column's local top
    /// ([`LocalBounds::top`](super::LocalBounds::top)) counts it, so the chunks over a bare plain
    /// stay `Uniform` air; it must count the features anchored outside `area` that reach into it.
    /// In debug builds a block over it fails the span check of the chunk. By default the height
    /// of [`FeatureStage::reach`].
    fn column_height(&self, cx: &Cx, seed: u64, area: Rect) -> u32 {
        let _ = (cx, seed, area);
        self.reach().height
    }

    /// Writes its features into `out`, the chunk's states (`y << 10 | x << 5 | z`, after the
    /// surface stage, the carvers and the features before it), inside each column's reach; `seed`
    /// is the realm's seed mixed with the stage's salt. Called only for chunks its reach touches;
    /// in debug builds [`decorate`] panics if it changed a block outside `ground + 1..=ground +
    /// height` of its column.
    fn apply(
        &self,
        columns: &ChunkColumns<'_, Cx>,
        seed: u64,
        registry: &BlockRegistry,
        out: &mut [BlockState],
    );

    /// Every block name the stage looks up in the registry it gets, so the world open checks
    /// them and a typo fails at load ([`RegionGenerator::build`](super::RegionGenerator::build)),
    /// not in a worker.
    fn block_names(&self) -> Vec<&str>;

    /// The far LOD form (ADR 0039 sec. 10): per cell column of a node, a run of `(state,
    /// blocks)` stacked over the cell column's coarse ground (a canopy), into `out` (one per cell
    /// column, in `columns`' order, all `(AIR, 0)` on entry). The far LOD draws it as cells
    /// over the ground, never as margin, and inside the band; a run of 0 blocks, or of a state
    /// that is not Solid far away, draws nothing. Of the runs the stages give a cell column, the
    /// tallest is drawn. It should be a pure function of the cell column (hashed per cell, not
    /// per node), so two nodes and two jobs of a column agree, and at most
    /// [`FeatureReach::height`] blocks (the exported harness checks both ends of each run).
    ///
    /// The default gives none: the stage is not seen far away.
    fn lod_runs(
        &self,
        columns: &LodColumns<'_, Cx>,
        seed: u64,
        registry: &BlockRegistry,
        out: &mut [(BlockState, u32)],
    ) {
        let _ = (columns, seed, registry, out);
    }
}

/// A feature stage as a realm runs it: its id (for messages), its stage and its seed (the realm's
/// seed mixed with the stage's salt).
pub struct RealmFeature<Cx: Context> {
    /// The stage's id.
    pub id: GenId,
    /// The stage.
    pub stage: Arc<dyn FeatureStage<Cx>>,
    /// Its seed.
    pub seed: u64,
}

/// A realm's decoration: its shape modifiers, fill stage, surface stage, carvers and feature
/// stages, each with the id of its digest line.
pub struct Decoration<Cx: Context> {
    /// The fill stage.
    pub fill: (GenId, Arc<dyn FillStage<Cx>>),
    /// The surface stage.
    pub surface: (GenId, Arc<dyn SurfaceStage<Cx>>),
    /// The shape modifiers, in any order: they run by `(priority, id)`, the higher priority
    /// first ([`GenEntry::order`]).
    pub modifiers: Vec<(GenId, Arc<dyn ShapeModifier<Cx>>)>,
    /// The carvers, in any order: they run by `(priority, id)` too.
    pub carvers: Vec<(GenId, Arc<dyn CarverStage<Cx>>)>,
    /// The feature stages, in any order: they run by `(priority, id)` too.
    pub features: Vec<(GenId, Arc<dyn FeatureStage<Cx>>)>,
}

impl<Cx: Context> Decoration<Cx> {
    /// The stages `fill` and `surface`, with their ids, and no shape modifier, carver or feature
    /// stage.
    pub fn new(
        fill: (GenId, Arc<dyn FillStage<Cx>>),
        surface: (GenId, Arc<dyn SurfaceStage<Cx>>),
    ) -> Self {
        Self {
            fill,
            surface,
            modifiers: Vec::new(),
            carvers: Vec::new(),
            features: Vec::new(),
        }
    }

    /// The decoration with the shape modifier `modifier` under `id` too.
    #[must_use]
    pub fn with_modifier(mut self, id: GenId, modifier: Arc<dyn ShapeModifier<Cx>>) -> Self {
        self.modifiers.push((id, modifier));
        self
    }

    /// The decoration with the carver `carver` under `id` too.
    #[must_use]
    pub fn with_carver(mut self, id: GenId, carver: Arc<dyn CarverStage<Cx>>) -> Self {
        self.carvers.push((id, carver));
        self
    }

    /// The decoration with the feature stage `feature` under `id` too.
    #[must_use]
    pub fn with_feature(mut self, id: GenId, feature: Arc<dyn FeatureStage<Cx>>) -> Self {
        self.features.push((id, feature));
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
            carvers: self
                .carvers
                .iter()
                .map(|(id, c)| (id.clone(), Arc::clone(c)))
                .collect(),
            features: self
                .features
                .iter()
                .map(|(id, f)| (id.clone(), Arc::clone(f)))
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
    /// The carvers, in the order they run.
    pub carvers: &'a [RealmCarver<Cx>],
    /// The feature stages, in the order they run.
    pub features: &'a [RealmFeature<Cx>],
    /// The realm's floor state, for the fill.
    pub floor: BlockState,
    /// The realm's fluid state, where a column's fluid top is over its ground (air for a realm
    /// without fluid): the carvers never carve it.
    pub fluid: BlockState,
}

/// The chunk of `columns`: the rock of the fill at and under each column's ground, the fluid up to
/// its fluid top, air above, the surface stage over it where its reach touches the chunk, the
/// carvers' masks over all of it (a voxel they carve turns to air where it is neither air nor the
/// fluid), and the feature stages over the grounds where their reach touches the chunk.
///
/// From the cheapest answer to the dearest: `Uniform` air over every column's reach and fluid;
/// only the fill's rock under every column's reach (`Uniform` when the rock is one state and no
/// carver carves the chunk, 1-bit when one does); a 1-bit chunk ([`ChunkData::from_occupancy`])
/// when the rock is one state and neither the fluid, the surface nor a feature stage touches the
/// chunk (the carvers' masks and-ed onto the ground's); else a dense chunk
/// ([`ChunkData::from_dense`]).
pub fn decorate<Cx: Context>(
    columns: &ChunkColumns<'_, Cx>,
    stages: &Stages<'_, Cx>,
    registry: &BlockRegistry,
) -> ChunkData {
    let y0 = columns.origin.y;
    let y1 = y0 + i64::from(CHUNK_SIZE) - 1;
    let reach = stages.surface.reach();
    let (depth, rise) = (i64::from(reach.depth), i64::from(reach.rise));
    // The highest any feature stage writes over a ground.
    let grow = stages
        .features
        .iter()
        .map(|f| i64::from(f.stage.reach().height))
        .max()
        .unwrap_or(0);
    let over = rise.max(grow);
    let (mut min_g, mut max_g, mut max_top) = (i64::MAX, i64::MIN, i64::MIN);
    let (mut touched, mut wet, mut grown) = (false, false, false);
    for (&g, &w) in columns.ground.iter().zip(columns.fluid_top) {
        min_g = min_g.min(g);
        max_g = max_g.max(g);
        max_top = max_top.max(g.saturating_add(over)).max(w);
        touched |= depth + rise > 0 && g - depth < y1 && g.saturating_add(rise) >= y0;
        wet |= w > g && g < y1 && w >= y0;
        grown |= grow > 0 && g < y1 && g.saturating_add(grow) >= y0;
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
    let carve = Carve {
        stages,
        cx: columns.cx,
        origin: columns.origin,
        columns: Some(columns),
        apron_lo: columns.apron_lo,
        registry,
    };
    match rock {
        // Under every column's reach: the rock alone, as the deep path gives it.
        Some(rock) if y1 <= min_g.saturating_sub(depth) => return carve.rock(rock, &mut rock_buf),
        Some(Rock::Uniform(state)) if !touched && !wet && !grown => {
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
            if carve.carves(state) {
                match carve.mask() {
                    CarveMask::Solid => {}
                    CarveMask::Air => occupancy = [0; CHUNK_AREA],
                    CarveMask::Mixed(keep) => {
                        for (word, keep) in occupancy.iter_mut().zip(keep.iter()) {
                            *word &= keep;
                        }
                    }
                }
            }
            return ChunkData::from_occupancy(&occupancy, state, BlockState::AIR);
        }
        // Over every ground with nothing reaching the chunk (not reached: `y0 <= max_top`).
        None if !touched && !wet && !grown => return ChunkData::uniform(BlockState::AIR),
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
        stages.surface.apply(
            &columns.read_by("the surface stage", 0),
            registry,
            &mut states,
        );
        if let Some(before) = before {
            check_reach(columns, reach, &before, &states);
        }
    }
    // Only the rock and the surface stage's blocks over the grounds can be carved.
    if !stages.carvers.is_empty() && (rock.is_some() || (touched && rise > 0)) {
        carve.states(&carve.mask(), &mut states);
    }
    if grown {
        for f in stages.features {
            let reach = f.stage.reach();
            let height = i64::from(reach.height);
            let reached = columns
                .ground
                .iter()
                .any(|&g| g < y1 && g.saturating_add(height) >= y0);
            if !reached {
                continue;
            }
            let before = cfg!(debug_assertions).then(|| states.clone());
            let read = columns.read_by(f.id.as_str(), reach.apron);
            f.stage.apply(&read, f.seed, registry, &mut states);
            if let Some(before) = before {
                check_feature_reach(columns, f, reach, &before, &states);
            }
        }
    }
    ChunkData::from_dense(&states)
}

/// The chunk at `origin` under every column's surface reach and every carver's (the deep path):
/// the fill's rock, carved by the carvers without columns.
pub(super) fn deep<Cx: Context>(
    cx: &Cx,
    origin: BlockPos,
    apron_lo: i64,
    stages: &Stages<'_, Cx>,
    registry: &BlockRegistry,
) -> ChunkData {
    let mut buf = Vec::new();
    let rock = stages
        .fill
        .rock(cx, origin, stages.floor, registry, &mut buf);
    let carve = Carve {
        stages,
        cx,
        origin,
        columns: None,
        apron_lo,
        registry,
    };
    carve.rock(rock, &mut buf)
}

/// The carvers of one chunk and what they read.
struct Carve<'s, 'c, Cx: Context> {
    stages: &'s Stages<'s, Cx>,
    cx: &'s Cx,
    origin: BlockPos,
    /// `None` in the deep path.
    columns: Option<&'c ChunkColumns<'c, Cx>>,
    apron_lo: i64,
    registry: &'s BlockRegistry,
}

impl<Cx: Context> Carve<'_, '_, Cx> {
    /// Whether a carver may carve `state`: there is one, and the state is neither air nor the
    /// realm's fluid.
    fn carves(&self, state: BlockState) -> bool {
        !self.stages.carvers.is_empty() && state != BlockState::AIR && state != self.stages.fluid
    }

    /// The carvers' masks, and-ed: [`CarveMask::Solid`] without carvers.
    fn mask(&self) -> CarveMask {
        let mut mask = CarveMask::Solid;
        for c in self.stages.carvers {
            let reach = c.stage.reach();
            let columns = self
                .columns
                .map(|cols| cols.read_by(c.id.as_str(), reach.apron));
            let chunk = CarveChunk {
                cx: self.cx,
                seed: c.seed,
                origin: self.origin,
                columns: columns.as_ref(),
                apron_lo: self.apron_lo,
                floor: self.stages.floor,
                fluid: self.stages.fluid,
                registry: self.registry,
            };
            let m = c.stage.carve(&chunk);
            if cfg!(debug_assertions)
                && let Some(columns) = &columns
            {
                check_carve_reach(c, reach, columns, &chunk, &m);
            }
            mask = mask.and(m);
        }
        mask
    }

    /// The rock alone, carved: the deep path, or a chunk under every column's surface reach.
    fn rock(&self, rock: Rock, buf: &mut [BlockState]) -> ChunkData {
        match rock {
            Rock::Uniform(state) if !self.carves(state) => ChunkData::uniform(state),
            Rock::Uniform(state) => match self.mask() {
                CarveMask::Solid => ChunkData::uniform(state),
                CarveMask::Air => ChunkData::uniform(BlockState::AIR),
                CarveMask::Mixed(keep) => ChunkData::from_occupancy(&keep, state, BlockState::AIR),
            },
            Rock::Dense => {
                if !self.stages.carvers.is_empty() {
                    self.states(&self.mask(), buf);
                }
                ChunkData::from_dense(buf)
            }
        }
    }

    /// `mask` over `states` (`y << 10 | x << 5 | z`): every voxel it carves turns to air, unless
    /// it holds air or the realm's fluid.
    fn states(&self, mask: &CarveMask, states: &mut [BlockState]) {
        let fluid = self.stages.fluid;
        let keep = match mask {
            CarveMask::Solid => return,
            CarveMask::Air => {
                for s in states.iter_mut().filter(|s| **s != fluid) {
                    *s = BlockState::AIR;
                }
                return;
            }
            CarveMask::Mixed(keep) => keep,
        };
        for (i, &word) in keep.iter().enumerate() {
            // Word `ly * 32 + lx`, bit z; the state of `(lx, ly, lz)` is at `ly << 10 | lx << 5 |
            // lz`.
            let base = ((i >> CHUNK_SHIFT) << (2 * CHUNK_SHIFT)) | ((i & 31) << CHUNK_SHIFT);
            let mut carved = !word;
            while carved != 0 {
                let lz = carved.trailing_zeros() as usize;
                carved &= carved - 1;
                let s = &mut states[base | lz];
                if *s != fluid {
                    *s = BlockState::AIR;
                }
            }
        }
    }
}

/// The debug check of a carver's surface reach: in a chunk at least its chunk column's
/// [`CarverStage::column_reach`] under every ground the carver may read (its chunk's and its
/// apron's), its mask with the columns is its mask without them, as the deep path gives it. A
/// carver whose rules read the surface deeper would carve such a chunk one way in the column stage
/// and another in the deep path.
fn check_carve_reach<Cx: Context>(
    carver: &RealmCarver<Cx>,
    reach: CarveReach,
    columns: &ChunkColumns<'_, Cx>,
    chunk: &CarveChunk<'_, Cx>,
    mask: &CarveMask,
) {
    let origin = columns.origin;
    let y1 = origin.y + i64::from(CHUNK_SIZE) - 1;
    let column = (origin.x >> CHUNK_SHIFT, origin.z >> CHUNK_SHIFT);
    let surface = i64::from(carver.column_reach(chunk.cx, column));
    let mut lowest = columns.ground.iter().copied().min().expect("1024 columns");
    if y1 > lowest.saturating_sub(surface) {
        return;
    }
    if reach.apron > 0 {
        let (a, size) = (i64::from(reach.apron), i64::from(CHUNK_SIZE));
        let xs: Vec<i64> = (origin.x - a..origin.x + size + a).collect();
        let zs: Vec<i64> = (origin.z - a..origin.z + size + a).collect();
        let (mut ground, mut fluid) = (vec![0; xs.len() * zs.len()], vec![0; xs.len() * zs.len()]);
        columns.tops(&xs, &zs, &mut ground, &mut fluid);
        lowest = lowest.min(ground.iter().copied().min().expect("columns"));
        if y1 > lowest.saturating_sub(surface) {
            return;
        }
    }
    let without = CarveChunk {
        cx: chunk.cx,
        seed: chunk.seed,
        origin: chunk.origin,
        columns: None,
        apron_lo: chunk.apron_lo,
        floor: chunk.floor,
        fluid: chunk.fluid,
        registry: chunk.registry,
    };
    assert!(
        same_voxels(&carver.stage.carve(&without), mask),
        "span check: carver {} carved the chunk at ({}, {}, {}), {surface} or more blocks under \
         every ground it may read (the lowest at y {lowest}), one way with its columns and \
         another without: its rules read the surface deeper than its reach there (column reach \
         {surface}, declared surface {}), so the deep path would carve it otherwise",
        carver.id,
        origin.x,
        origin.y,
        origin.z,
        reach.surface,
    );
}

/// Whether two masks leave the same voxels: a `Mixed` mask may hold every bit or none (one the
/// carver did not pass through [`CarveMask::of`]), and gives the same chunk as `Solid` or `Air`.
fn same_voxels(a: &CarveMask, b: &CarveMask) -> bool {
    let word = |m: &CarveMask, i: usize| match m {
        CarveMask::Solid => u32::MAX,
        CarveMask::Air => 0,
        CarveMask::Mixed(keep) => keep[i],
    };
    (0..CHUNK_AREA).all(|i| word(a, i) == word(b, i))
}

/// The debug check of a feature stage: it changed only blocks over each column's ground, as high
/// as it declares. It runs only in the chunks its reach touches and never in the deep path, so a
/// block it writes elsewhere would show in some chunks and not in others, and over the local top
/// it would show only through the column stage.
fn check_feature_reach<Cx: Context>(
    columns: &ChunkColumns<'_, Cx>,
    feature: &RealmFeature<Cx>,
    reach: FeatureReach,
    before: &[BlockState],
    after: &[BlockState],
) {
    let origin = columns.origin;
    for (c, &g) in columns.ground.iter().enumerate() {
        let (lo, hi) = (g.saturating_add(1), g.saturating_add(reach.height.into()));
        for ly in 0..CHUNK_SIZE as usize {
            let (i, y) = ((ly << (2 * CHUNK_SHIFT)) | c, origin.y + ly as i64);
            assert!(
                before[i] == after[i] || (lo..=hi).contains(&y),
                "span check: feature stage {} changed the block at ({}, {y}, {}), outside its \
                 reach {lo}..={hi} (height {} over the ground at y {g}): the chunks it does not \
                 reach would not show it",
                feature.id,
                origin.x + (c >> CHUNK_SHIFT) as i64,
                origin.z + (c & (CHUNK_SIZE as usize - 1)) as i64,
                reach.height,
            );
        }
    }
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
