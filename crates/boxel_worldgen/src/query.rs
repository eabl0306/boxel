//! The query side of a generator (ADR 0039 sec. 13): what lies at a point of the world, without
//! building chunks, for the spawn search, the F3 line, the fog and the environment swaps.

use crate::region::HeightQ;
use crate::registry::GenId;

/// A column of a realm, as its generator computes it: the same bits as its chunks.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ColumnInfo {
    /// The realm.
    pub realm: GenId,
    /// The height (the kinds' blended, then the realm's shape modifiers), in 1/256 block over the
    /// realm's datum.
    pub height: HeightQ,
    /// The `y` of the top solid block.
    pub ground: i64,
    /// The `y` of the highest fluid block over the ground, `None` if the column is dry.
    pub fluid_top: Option<i64>,
    /// The dominant terrain kind (the largest blend weight, ties to the smaller id), `None` for a
    /// generator without kinds.
    pub kind: Option<GenId>,
    /// A block at or under the lowest ground of the column's chunk column (the realm's
    /// `ground_lo`).
    pub ground_lo: i64,
    /// The highest block of the column's chunk column that may not be air.
    pub top: i64,
}

/// What lies at a point of a generator's world (ADR 0039 sec. 13), reached through
/// [`WorldGenerator::as_query`](crate::WorldGenerator::as_query). Pure, and about 2 us warm.
pub trait WorldQuery: Send + Sync {
    /// The `y` of the top solid block of column `(x, z)` in the realm the far LOD draws (the open
    /// surface), `None` if that realm has no such surface.
    fn height_at(&self, x: i64, z: i64) -> Option<i64>;

    /// The realm of `y`.
    fn realm_at(&self, y: i64) -> &GenId;

    /// The biome at `(x, y, z)`, `None` if the realm has none. Until the biome table exists
    /// (ADR 0039 sec. 9), a placeholder label: the dominant kind of the column in the realm of
    /// `y`, the table's input.
    fn biome_at(&self, x: i64, y: i64, z: i64) -> Option<GenId>;

    /// Column `(x, z)` of the realm the far LOD draws, `None` if that realm has no surface.
    fn column(&self, x: i64, z: i64) -> Option<ColumnInfo>;

    /// The environment at `(x, y, z)`: an id in the game's table of environments. Per realm for
    /// now (the realm of `y`'s), per position later.
    fn environment_at(&self, x: i64, y: i64, z: i64) -> &GenId;
}
