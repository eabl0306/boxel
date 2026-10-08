//! [`Sea`]: water up to a sea level over the low columns, with sand beaches (ADR 0021).

/// Water up to a sea level: every column whose surface is below `level` gets source water from
/// the block above its top up to `level` (lakes and seas wherever the terrain dips), and the
/// columns up to `level + beach` get sand on top instead of grass. Per column, from its height
/// alone: no neighbor data, exact at any distance, nearly free (one more run in the column
/// fill). The water is all sources and schedules no tick: it stays still until an edit touches
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sea {
    /// The y of the highest water block.
    pub level: i64,
    /// Sand on top of the columns up to `level + beach` high (the beaches).
    pub beach: i64,
}

impl Default for Sea {
    /// Water up to y = 46 (about 8 % of the default terrain's columns lie below it), sand up to
    /// 2 blocks above it.
    fn default() -> Self {
        Self {
            level: 46,
            beach: 2,
        }
    }
}

impl Sea {
    /// Ground blocks under the water of a column, cut by no cave (in the overworld): caves stay
    /// closed under the sea, so its water never sits over air.
    pub const FLOOR: i64 = 4;

    /// True if a column of height `h` is under water.
    pub const fn covers(&self, h: i64) -> bool {
        h < self.level
    }

    /// True if a column of height `h` has sand on top for the sea (under it or on the beach).
    pub const fn sandy(&self, h: i64) -> bool {
        h <= self.level.saturating_add(self.beach)
    }
}
