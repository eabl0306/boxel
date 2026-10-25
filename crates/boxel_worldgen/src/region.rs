//! Terrain by regions (ADR 0039 sec. 4, 5, 6, 9, 13 and 14): a realm's region map gives every
//! point of a lattice one terrain kind, the kinds' heights are blended exactly at their borders,
//! and a shared decoration builds the chunks.
//!
//! - [`Context`]: a realm's shared fields and named parameters (the game's).
//! - [`TerrainKind`]: a kind's heights, bounds and limits (the game's; [`FlatKind`] is the
//!   engine's one).
//! - [`SelectorRule`] and [`RuleEntry`]: the region map (game data; mods add rules and entries).
//!   The selector is the only place a kind is chosen.
//! - [`blend`]: the exact integer weights and blend.
//! - [`ShapeModifier`], [`FillStage`], [`SurfaceStage`], [`CarverStage`] and [`FeatureStage`]:
//!   the decoration hook, with [`decorate`]. Kinds, modifiers, carvers and feature stages give
//!   bounds per chunk column ([`TerrainKind::column_bounds`], [`ShapeModifier::column_effect`],
//!   [`CarverStage::column_reach`], [`FeatureStage::column_height`]) and a stage may read the
//!   columns around its chunk as far as it declares ([`ChunkColumns::tops`]).
//! - [`RegionGenerator`]: one realm, built at world open from the registries, with the freeze
//!   checks of its rules and kinds, and its far LOD: the region pass, one [`LodSource`] for the
//!   whole realm (ADR 0039 sec. 10).
//! - [`DimensionGenerator`]: a dimension's realm stack, one generator per realm.
//! - With the `test-support` feature, `check_lod`: the exported harness that checks a realm's far
//!   LOD against its real terrain (the chunks without carvers and features,
//!   `RegionGenerator::generate_terrain`) and the runs of its feature stages.
//!
//! Every per-worker memo is keyed by its generator's [`BuildKey`](crate::registry::BuildKey), so a
//! world opened after another on the same workers never reads the first one's.
//!
//! Not built yet: core terms, volumes and the other realm forms (WG-VOLUME, WG-CAVERN), the biome
//! table and the rest of the decoration layer (structures, post steps).
//!
//! [`LodSource`]: crate::lod::LodSource

pub mod blend;
mod check;
#[cfg(feature = "test-support")]
mod check_lod;
mod decorate;
mod dimension;
mod generator;
mod kind;
mod lod;
mod memo;
mod modifier;
mod select;

pub use check::{border_relief, check_kinds, check_modifiers, slope_passes};
#[cfg(feature = "test-support")]
pub use check_lod::{CORNER_STEP, LodReport, check_lod, lod_corners, lod_memo_bytes};
pub use decorate::{
    CarveChunk, CarveMask, CarveReach, CarverStage, ChunkColumns, ColumnsAround, Decoration,
    FeatureReach, FeatureStage, FillStage, FloodMask, FloorFill, LOD_TOP_RUNS, LodColumns, LodTop,
    MAX_APRON, NoSurface, RealmCarver, RealmFeature, Rock, Stages, SurfaceReach, SurfaceStage,
    TopLayers, decorate,
};
pub use dimension::{DimensionGenerator, REALM_GENERATOR};
pub use generator::{LocalBounds, RegionGenerator, RegionSettings, RegionSpec, check_realm_blocks};
pub use kind::{
    Context, FlatKind, HEIGHT_ONE, HEIGHT_SHIFT, HeightQ, KindForm, KindGrid, KindLimits,
    TerrainKind, block_of,
};
pub use memo::clear_worker_memo;
pub use modifier::{
    CarveCorner, ShapeEffect, ShapeModifier, SlopeMode, divide_tops, lod_by_effect, warp_slack,
};
pub use select::{
    DEFAULT_CELL_SHIFT, Entry, Interval, MAX_WEIGHT, PARAM_REGISTRY, RuleEntry, Scope,
    SelectorRule, check_rule_entries,
};

/// The steepest terrain the blend keeps along any line, in blocks per block (the far LOD's
/// skirts cover up to 3:1, ADR 0031).
pub const MAX_SLOPE: u32 = 3;

/// The smallest region lattice: `2^5` blocks, so a chunk column lies in one lattice cell.
pub const MIN_LATTICE_SHIFT: u32 = 5;

/// The largest region lattice: `2^12` blocks, so the blend's sums stay far inside `i64`.
pub const MAX_LATTICE_SHIFT: u32 = 12;

/// The default region lattice: `2^8` (256) blocks, a blend band of 768.
pub const DEFAULT_LATTICE_SHIFT: u32 = 8;

/// The version of the region code (selection, blend, chunk path) in every realm's digest line
/// `region`: bumped when the same registries give other terrain.
pub const CODE_VERSION: u32 = 1;

/// The registry name of a region generator's line for its code and lattice ([`CODE_VERSION`]):
/// every region realm has one.
pub const REGION_LINE: &str = "region";

#[cfg(test)]
mod tests;
