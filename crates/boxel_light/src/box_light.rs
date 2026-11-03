//! The light of one padded chunk (local `-1..=32`), computed exactly from a box around it.
//!
//! **Why a box is exact.** A light level falls by at least 1 per voxel and starts at 15 (direct
//! sky or an emitter), so any light of 1 or more in the padded chunk comes from a source at
//! most 14 voxels away, along a path that stays within 14 voxels of it. The box, the chunk
//! plus 16 voxels on each side (64^3, so a z row is one `u64`), holds every such source and
//! path. A flood restricted to it therefore gives the global solution on the padded chunk, bit
//! for bit. The box lies inside the 27 chunks around the chunk, which the mesh job already
//! holds, so no chunk ever reads another chunk's light.
//!
//! Light filters (ADR 0060, colored glass) keep the argument: a filter voxel scales the block
//! light entering it after its cost ([`boxel_voxel::transmit`]), which never raises a level, so
//! light still falls by at least 1 per voxel and still starts at 15 at most. The rule stays
//! monotone (more light in, never less out), so the fixpoint is still unique and the
//! highest-first floods still pop each voxel at its final level.
//!
//! Sky sources (ADR 0061) keep it too: a beam cell is direct sky, a level-15 source like a voxel
//! above `T`, and the beam set is exact because its tiles are built from the world content
//! ([`crate::beam_tile`]). A beam passes filters of opacity 0 and stays white, as the sky does.
//!
//! **Colored sky** (ADR 0064, [`LightInput::colored_sky`]): sky light is filtered exactly as
//! block light, per channel. A direct voxel under filters (the tints of its column above `T`,
//! or the filter cells of a beam) is a source at the filtered level of each channel, and stays
//! open so a brighter neighbor can raise it; spread sky entering a filter voxel is filtered
//! after its cost. `transmit` never raises a level, so every light still comes from a source
//! at most 14 voxels away and the fixpoint is still unique. Only the boxes where a filter can
//! tint the sky take this path (`colored_floods`): three sky floods, one per channel, next to
//! the uncolored one, which gameplay reads (ADR 0064). Every other box runs the code of
//! before.
//!
//! **Steps** ([`compute`]):
//! 1. Emitters inside the box, from the chunks whose palette may emit (the others are skipped
//!    without reading a voxel). Fast paths, from that and the tiles' bounds alone: no emitter
//!    and the box under every sky top is uniformly dark ([`LightPath::FastDark`]); no emitter
//!    and the padded chunk above every sky top is uniformly open sky ([`LightPath::FastOpen`]).
//!    A lamp that is gone but still in a palette, or one outside the box, does not block them.
//!    A beam that crosses the box rules out `FastDark`; it never changes `FastOpen`.
//! 2. Opaque (and, when some state has opacity 2..=14, partial) z-row masks of the box, from
//!    the 27 chunks' [`LightMasks`].
//! 3. Sky: the direct voxels (above the top of their column, and the beam cells of the sky
//!    sources, ADR 0061) as row masks, built from the 9 tiles; seeds are the open non-direct
//!    voxels next to a direct one, at `15 - cost`; then a flood into open non-direct voxels.
//! 4. Block light: one flood per channel present, seeded by the emitters inside the box. When a
//!    chunk of the box holds a filter voxel, the floods price the filter voxels (and the partial
//!    ones) one by one; the sky flood never does.
//! 5. The padded region is copied out of the box (direct voxels get sky 15), and the rows the
//!    floods wrote are reset (no 512 KiB clear).
//!
//! Each flood is a bucket queue by level, highest first, with a single channel: values are
//! raised in decreasing order, so every voxel is popped once, at its final level (an emitter
//! outshone by a brighter neighbor leaves a stale entry, skipped). Level-1 voxels are written but
//! never queued: they light nothing. Coordinates inside the box are small; the only absolute comparisons
//! (sky tops against the box) are `i64` differences relative to the box, saturated, so the
//! result is the same at any position up to the world limit.
use crate::SkyTile;
use crate::beam_tile::BeamTile;
use crate::padded::{
    PADDED, PADDED_VOLUME, PaddedLight, WHITE_SKY, filter_sky, new_full, sky_is_gray,
};
use boxel_voxel::{
    BlockState, ChunkData, ChunkPos, LightMasks, LightTables, LightValue, LocalPos, MaskRows,
    transmit,
};

/// Box edge in voxels: the chunk plus [`MARGIN`] on each side.
pub const BOX: usize = 64;
/// Voxels of the box around the chunk on each side.
pub const MARGIN: i64 = 16;
/// Z rows of the box (`BOX^2`), indexed `(by << 6) | bx`.
const ROWS: usize = BOX * BOX;
/// Voxels of the box (`BOX^3`), indexed `(by << 12) | (bx << 6) | bz`.
const VOLUME: usize = ROWS * BOX;
/// The 34 bits of a row that belong to the padded chunk (box `15..49`).
const PADDED_ROW: u64 = (1 << PADDED) - 1;
/// First box coordinate of the padded chunk (local -1).
const PADDED_START: usize = MARGIN as usize - 1;
/// Sky 15, the value of a direct voxel.
const SKY_BITS: u16 = 0xF000;
/// Words of the touched-rows bitset (one bit per z row).
const ROW_WORDS: usize = ROWS / 64;

/// Light of the box, one [`LightValue`] (as `u16`) per voxel.
type BoxLight = [u16; VOLUME];
/// One bit per voxel, a `u64` per z row.
type BoxRows = [u64; ROWS];

/// Index of chunk offset `(dx, dy, dz)` (each in `-1..=1`) in [`LightInput::chunks`]:
/// `(dy + 1) * 9 + (dx + 1) * 3 + (dz + 1)`; the center is 13.
pub const fn chunk_index(dx: i32, dy: i32, dz: i32) -> usize {
    ((dy + 1) * 9 + (dx + 1) * 3 + (dz + 1)) as usize
}

/// Index of column offset `(dx, dz)` (each in `-1..=1`) in [`SkyInput::Tiles`]:
/// `(dx + 1) * 3 + (dz + 1)`; the center is 4.
pub const fn tile_index(dx: i32, dz: i32) -> usize {
    ((dx + 1) * 3 + (dz + 1)) as usize
}

/// Where the sky light of a box comes from.
#[derive(Clone, Copy, Debug)]
pub enum SkyInput<'a> {
    /// The caller knows that no voxel of the box is direct sky: its top is at or below the sky
    /// top of every column around it (for example a box under the walk floor of a world whose
    /// columns never reach the void; see [`crate::sky_tile`]). The box gets no sky light.
    BelowFloor,
    /// The sky tiles of the 3x3 chunk columns around the chunk, by [`tile_index`].
    Tiles([&'a SkyTile; 9]),
}

/// Inputs of one light box.
#[derive(Clone, Copy, Debug)]
pub struct LightInput<'a> {
    /// The chunk whose padded light is computed.
    pub origin: ChunkPos,
    /// The 27 chunks around it, center included, by [`chunk_index`]. `None` reads as air
    /// (a missing chunk).
    pub chunks: [Option<&'a ChunkData>; 27],
    /// Sky tops.
    pub sky: SkyInput<'a>,
    /// Beams of sky sources (ADR 0061) of the 3x3 chunk columns, by [`tile_index`]. A tile must
    /// cover the box's rows (`origin.y * 32 - 16 ..= origin.y * 32 + 47`, checked in debug
    /// builds). `None`: no beam reaches this column's part of the box (every world without sky
    /// sources), at no cost.
    pub beams: [Option<&'a BeamTile>; 9],
    /// Opacity and emission of each state (built from the registry the chunks use).
    pub tables: &'a LightTables,
    /// Colored sky (ADR 0064, `WorldSettings::colored_sky`): light filters tint the sky light
    /// too, and a box where one can gives [`PaddedLight::Colored`]. Off: the sky stays white
    /// and every result is bit for bit the one of before.
    pub colored_sky: bool,
}

/// How a box was computed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LightPath {
    /// No emitter and no direct sky in the box: uniformly dark, no flood.
    FastDark,
    /// No emitter and the whole padded chunk is direct sky: uniformly 15, no flood.
    FastOpen,
    /// The floods ran.
    Full {
        /// Voxels popped by the sky flood.
        sky_pops: u32,
        /// Voxels popped by the block floods (all channels).
        block_pops: u32,
        /// `Some(pops)` if the box took the colored sky path (ADR 0064): the voxels popped by
        /// its three sky floods.
        colored_pops: Option<u32>,
    },
}

/// `colored_pops` only when the box took the colored path, so the output of every other box
/// stays as before (goldens and logs print it).
impl std::fmt::Debug for LightPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LightPath::FastDark => f.write_str("FastDark"),
            LightPath::FastOpen => f.write_str("FastOpen"),
            LightPath::Full {
                sky_pops,
                block_pops,
                colored_pops,
            } => {
                let mut d = f.debug_struct("Full");
                d.field("sky_pops", sky_pops)
                    .field("block_pops", block_pops);
                if let Some(pops) = colored_pops {
                    d.field("colored_pops", pops);
                }
                d.finish()
            }
        }
    }
}

/// Per-worker scratch of [`compute`] (about 0.7 MiB, and 0.7 MiB more from the first colored
/// box). Reused across calls: its light arrays are all zero between calls.
pub struct LightScratch {
    /// Light of the box (zero outside a call).
    light: Box<BoxLight>,
    /// Opaque voxels.
    opaque: Box<BoxRows>,
    /// Partial voxels (opacity 1..=14); filled only when some state has opacity 2..=14.
    partial: Box<BoxRows>,
    /// Direct sky voxels.
    direct: Box<BoxRows>,
    /// Voxels the sky flood may not enter (opaque or direct).
    closed: Box<BoxRows>,
    /// Voxels the block floods price one by one when the box holds a filter: filter voxels,
    /// and partial ones when some state has opacity 2..=14.
    special: Box<BoxRows>,
    /// Rows with a non-zero light value (to reset them).
    touched: [u64; ROW_WORDS],
    /// Flood queue: voxels raised to level `l` (2 or more) wait in bucket `l`.
    buckets: [Vec<u32>; 16],
    /// Emitters inside the box: box index and emission.
    emitters: Vec<(u32, LightValue)>,
    /// Spare padded buffers (at most [`MAX_SPARES`]: the one `out` gives back and the one a
    /// uniform result did not keep), so alternating uniform and full results never allocate.
    spares: Vec<Box<[u16; PADDED_VOLUME]>>,
    /// A spare `sky_rgb` buffer of [`PaddedLight::Colored`].
    rgb_spare: Option<Box<[u16; PADDED_VOLUME]>>,
    /// The colored sky's scratch, made on the first colored box.
    color: Option<Box<ColorScratch>>,
    /// Padded buffers allocated so far.
    #[cfg(test)]
    allocations: usize,
}

/// Spare padded buffers kept by [`LightScratch`].
const MAX_SPARES: usize = 2;

/// The colored sky's part of [`LightScratch`] (ADR 0064), about 0.7 MiB.
struct ColorScratch {
    /// Sky red, green and blue of the box in the block nibbles (zero outside a call): the
    /// filtered level of the tinted direct voxels, then what the floods raise.
    rgb: Box<BoxLight>,
    /// Direct voxels with some sky channel below 15: sources at their filtered levels, open to
    /// the three floods.
    tinted: Box<BoxRows>,
    /// Voxels the three floods may not enter: opaque, or direct and not tinted (white).
    closed: Box<BoxRows>,
    /// Tinted voxels a flood must start from: next to an open voxel that is not direct, or in
    /// a column whose neighbors may be tinted otherwise.
    boundary: Box<BoxRows>,
    /// Rows of `rgb` with a non-zero value (to reset them).
    touched: [u64; ROW_WORDS],
    /// Per box column (`(bx << 6) | bz`), its tint going down the sweep of `tint_columns`.
    cur: Box<[u16; ROWS]>,
    /// Per `bx`, the columns (bit `bz`) whose tints the boundary must include.
    edge: [u64; BOX],
    /// Tints met inside the box: `(by << 24) | (column << 12) | tint`.
    events: Vec<u32>,
}

impl ColorScratch {
    fn new() -> Box<Self> {
        Box::new(Self {
            rgb: zeroed(),
            tinted: zeroed(),
            closed: zeroed(),
            boundary: zeroed(),
            touched: [0; ROW_WORDS],
            cur: zeroed(),
            edge: [0; BOX],
            events: Vec::new(),
        })
    }
}

impl Default for LightScratch {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for LightScratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LightScratch").finish_non_exhaustive()
    }
}

/// A zeroed fixed-size array on the heap.
fn zeroed<T: Clone + Default, const N: usize>() -> Box<[T; N]> {
    vec![T::default(); N]
        .into_boxed_slice()
        .try_into()
        .unwrap_or_else(|_| unreachable!("N entries"))
}

impl LightScratch {
    /// Fresh scratch.
    pub fn new() -> Self {
        Self {
            light: zeroed(),
            opaque: zeroed(),
            partial: zeroed(),
            direct: zeroed(),
            closed: zeroed(),
            special: zeroed(),
            touched: [0; ROW_WORDS],
            buckets: Default::default(),
            emitters: Vec::new(),
            spares: Vec::with_capacity(MAX_SPARES),
            rgb_spare: None,
            color: None,
            #[cfg(test)]
            allocations: 0,
        }
    }

    /// Keeps `values` as a spare if there is room.
    fn keep_spare(&mut self, values: Box<[u16; PADDED_VOLUME]>) {
        if self.spares.len() < MAX_SPARES {
            self.spares.push(values);
        }
    }

    /// A spare padded buffer, or a new one.
    fn take_spare(&mut self) -> Box<[u16; PADDED_VOLUME]> {
        self.spares.pop().unwrap_or_else(|| {
            #[cfg(test)]
            {
                self.allocations += 1;
            }
            new_full()
        })
    }
}

/// Computes the padded light of `input.origin` into `out`, taking the fast paths when they
/// apply.
pub fn compute(
    input: &LightInput<'_>,
    scratch: &mut LightScratch,
    out: &mut PaddedLight,
) -> LightPath {
    run(input, scratch, out, false)
}

/// [`compute`] without the fast paths: the floods always run, over the direct voxels found
/// from the tiles. Gives the same light (a reference for tests and benches).
pub fn compute_full(
    input: &LightInput<'_>,
    scratch: &mut LightScratch,
    out: &mut PaddedLight,
) -> LightPath {
    run(input, scratch, out, true)
}

/// Sky of a box from the tiles' bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sky {
    /// No direct voxel in the box.
    Dark,
    /// Every voxel of the padded chunk is direct.
    Open,
    /// Some of each.
    Mixed,
}

fn classify(input: &LightInput<'_>, origin_y: i64) -> Sky {
    let sky = match &input.sky {
        SkyInput::BelowFloor => Sky::Dark,
        SkyInput::Tiles(tiles) => {
            let min = tiles.iter().map(|t| t.min()).min().unwrap_or(i64::MIN);
            let max = tiles.iter().map(|t| t.max()).max().unwrap_or(i64::MIN);
            // Box top: origin + 31 + MARGIN. Padded bottom: origin - 1.
            if origin_y.saturating_add(31 + MARGIN) <= min {
                Sky::Dark
            } else if origin_y.saturating_sub(1) > max
                && !(input.colored_sky && tints_reach(tiles, origin_y.saturating_sub(1)))
            {
                Sky::Open
            } else {
                // A padded chunk above every top but under a tint is direct, not white.
                Sky::Mixed
            }
        }
    };
    // Beams add direct voxels: a dark box with one is mixed. An open one is already all 15.
    if sky == Sky::Dark && any_beams(&input.beams) && beams_cross(&input.beams, origin_y - MARGIN) {
        Sky::Mixed
    } else {
        sky
    }
}

/// True if some tint of `tiles` is at or above `y`: it filters direct sky at `y` or above.
#[inline]
fn tints_reach(tiles: &[&SkyTile; 9], y: i64) -> bool {
    tiles.iter().any(|t| t.tint_max() >= y)
}

/// The box columns (`bx` or `bz`) in the chunk column at tile offset `d + 1` (`d` in `-1..=1`).
fn box_span(d1: usize) -> std::ops::Range<usize> {
    match d1 {
        0 => 0..MARGIN as usize,
        1 => MARGIN as usize..BOX - MARGIN as usize,
        _ => BOX - MARGIN as usize..BOX,
    }
}

/// True if some column has a beam tile. Branch-free, as the dark fast path asks it of every box.
#[inline]
fn any_beams(beams: &[Option<&BeamTile>; 9]) -> bool {
    beams.iter().fold(false, |any, t| any | t.is_some())
}

/// True if some beam of `beams` has a cell in the box whose bottom row is `box_y0`.
#[inline(never)]
fn beams_cross(beams: &[Option<&BeamTile>; 9], box_y0: i64) -> bool {
    let top = BOX as i64 - 1;
    beams.iter().enumerate().any(|(ti, tile)| {
        debug_assert!(
            tile.is_none_or(|t| t.covers(box_y0, box_y0 + top)),
            "a beam tile of window {:?} for the box rows {box_y0}..={}",
            tile.map(BeamTile::window),
            box_y0 + top
        );
        let Some(tile) = tile.filter(|t| t.may_cross(box_y0, box_y0 + top)) else {
            return false;
        };
        let shift = tile.base() - box_y0;
        box_span(ti / 3).any(|bx| {
            box_span(ti % 3).any(|bz| {
                let col = (split(bx).1 << 5) | split(bz).1;
                tile.packed(col)
                    .iter()
                    .any(|b| i64::from(b.lo) + shift <= top && i64::from(b.hi) + shift >= 0)
            })
        })
    })
}

/// ORs the beam cells of `beams` in the box (bottom row `box_y0`) into `direct`, through
/// `toggles` (overwritten): per box column, the beams merged into disjoint runs, each run
/// toggling its first row and the row past its last, then one XOR sweep upwards. With
/// `skip_tinted`, the beams that cross a filter are left out (`add_tinted_beams` writes them).
/// Returns true if some cell was added; `toggles` then holds the cells added.
fn add_beams(
    beams: &[Option<&BeamTile>; 9],
    box_y0: i64,
    direct: &mut BoxRows,
    toggles: &mut BoxRows,
    skip_tinted: bool,
) -> bool {
    let top = BOX as i64 - 1;
    toggles.fill(0);
    let mut any = false;
    for (ti, tile) in beams.iter().enumerate() {
        debug_assert!(
            tile.is_none_or(|t| t.covers(box_y0, box_y0 + top)),
            "a beam tile of window {:?} for the box rows {box_y0}..={}",
            tile.map(BeamTile::window),
            box_y0 + top
        );
        let Some(tile) = tile.filter(|t| t.may_cross(box_y0, box_y0 + top)) else {
            continue;
        };
        // Box row of a beam offset: exact at any height (a difference of nearby values).
        let shift = tile.base() - box_y0;
        for bx in box_span(ti / 3) {
            for bz in box_span(ti % 3) {
                let col = (split(bx).1 << 5) | split(bz).1;
                let mut toggle = |lo: i64, hi: i64| {
                    toggles[((lo as usize) << 6) | bx] ^= 1 << bz;
                    if hi < top {
                        toggles[(((hi + 1) as usize) << 6) | bx] ^= 1 << bz;
                    }
                    any = true;
                };
                let mut run: Option<(i64, i64)> = None;
                // Sorted by `lo`, so clipped runs come in order and merge in one pass.
                for b in tile.packed(col) {
                    if skip_tinted && b.tinted() {
                        continue;
                    }
                    let lo = (i64::from(b.lo) + shift).max(0);
                    if lo > top {
                        break;
                    }
                    let hi = (i64::from(b.hi) + shift).min(top);
                    if hi < lo {
                        continue;
                    }
                    match run {
                        Some((s, e)) if lo <= e + 1 => run = Some((s, e.max(hi))),
                        _ => {
                            if let Some((s, e)) = run {
                                toggle(s, e);
                            }
                            run = Some((lo, hi));
                        }
                    }
                }
                if let Some((s, e)) = run {
                    toggle(s, e);
                }
            }
        }
    }
    if any {
        for r in BOX..ROWS {
            toggles[r] ^= toggles[r - BOX];
        }
        for (d, &t) in direct.iter_mut().zip(toggles.iter()) {
            *d |= t;
        }
    }
    any
}

fn run(
    input: &LightInput<'_>,
    scratch: &mut LightScratch,
    out: &mut PaddedLight,
    force_full: bool,
) -> LightPath {
    let origin_y = input.origin.origin().y;
    let sky = classify(input, origin_y);
    let any_emission = box_emitters(input, &mut scratch.emitters);
    let emits = !scratch.emitters.is_empty();
    if !force_full && !emits {
        match sky {
            Sky::Dark => {
                set_uniform(out, scratch, LightValue::DARK);
                return LightPath::FastDark;
            }
            Sky::Open => {
                set_uniform(out, scratch, LightValue::FULL_SKY);
                return LightPath::FastOpen;
            }
            Sky::Mixed => {}
        }
    }
    floods(input, scratch, out, force_full, sky, any_emission)
}

/// The floods of [`run`] when no fast path applies. Kept out of line, so the fast paths stay a
/// small function with a small stack frame (no 27 chunks' masks).
#[inline(never)]
fn floods(
    input: &LightInput<'_>,
    scratch: &mut LightScratch,
    out: &mut PaddedLight,
    force_full: bool,
    sky: Sky,
    any_emission: u16,
) -> LightPath {
    let origin_y = input.origin.origin().y;
    let box_y0 = origin_y - MARGIN;
    let tables = input.tables;
    let emits = !scratch.emitters.is_empty();
    let masks: [LightMasks<'_>; 27] = std::array::from_fn(|i| {
        input.chunks[i].map_or(LightMasks::EMPTY, |c| c.light_masks(tables))
    });
    let heavy = tables.max_partial_opacity() > 1;
    gather_rows(&masks.map(|m| m.opaque), &mut scratch.opaque);
    if heavy {
        gather_rows(&masks.map(|m| m.partial), &mut scratch.partial);
    }
    let cost_of = |bi: usize| u16::from(tables.opacity(state_at(input, bi)).max(1));
    let any_beams = any_beams(&input.beams);
    let sky = match (force_full, &input.sky) {
        (true, SkyInput::Tiles(_)) => Sky::Mixed,
        (true, SkyInput::BelowFloor) if any_beams => Sky::Mixed,
        _ => sky,
    };
    let filter_voxels = tables.any_filter() && masks.iter().any(|m| !m.filter.is_empty());
    // Colored sky: only where a filter can tint some direct or spread sky of the box.
    let colored = input.colored_sky
        && tables.any_filter()
        && sky == Sky::Mixed
        && (filter_voxels
            || matches!(&input.sky, SkyInput::Tiles(t) if tints_reach(t, box_y0))
            || (any_beams && tinted_beams_cross(&input.beams, box_y0)));
    if colored && scratch.color.is_none() {
        scratch.color = Some(ColorScratch::new());
    }
    let mut sky_pops = 0;
    match sky {
        Sky::Mixed => {
            match &input.sky {
                SkyInput::Tiles(tiles) => {
                    build_direct(tiles, box_y0, &mut scratch.direct);
                }
                SkyInput::BelowFloor => scratch.direct.fill(0),
            }
            if let (true, Some(color)) = (colored, scratch.color.as_deref_mut()) {
                match &input.sky {
                    SkyInput::Tiles(tiles) => tint_columns(tiles, box_y0, &scratch.direct, color),
                    SkyInput::BelowFloor => color.tinted.fill(0),
                }
                color.edge = [0; BOX];
            }
            if any_beams {
                // `closed` is free until the sky flood sets it.
                let LightScratch {
                    direct,
                    closed,
                    color,
                    ..
                } = scratch;
                let white = add_beams(&input.beams, box_y0, direct, closed, colored);
                if let (true, Some(color)) = (colored, color.as_deref_mut()) {
                    if white {
                        // A white beam cell is white whatever the roof above it.
                        for (t, &w) in color.tinted.iter_mut().zip(closed.iter()) {
                            *t &= !w;
                        }
                    }
                    add_tinted_beams(&input.beams, box_y0, direct, color);
                }
            }
            sky_pops = if heavy {
                sky_flood::<true>(scratch, &cost_of)
            } else {
                sky_flood::<false>(scratch, &cost_of)
            };
        }
        Sky::Open => scratch.direct.fill(u64::MAX),
        Sky::Dark => scratch.direct.fill(0),
    }
    let filters = emits && filter_voxels;
    if filters || (colored && filter_voxels) {
        gather_rows(&masks.map(|m| m.filter), &mut scratch.special);
        if heavy {
            let LightScratch {
                special, partial, ..
            } = scratch;
            for (s, &p) in special.iter_mut().zip(partial.iter()) {
                *s |= p;
            }
        }
    }
    let colored_pops = colored.then(|| colored_floods(input, scratch, heavy, filter_voxels));
    let block_pops = match (emits, filters, heavy) {
        (false, _, _) => 0,
        (true, true, _) => block_flood::<true>(any_emission, scratch, Special::Filter(input)),
        (true, false, true) => block_flood::<true>(any_emission, scratch, Special::Partial(input)),
        (true, false, false) => {
            block_flood::<false>(any_emission, scratch, Special::Partial(input))
        }
    };
    if colored {
        extract_colored(scratch, out);
    } else {
        extract(scratch, out);
    }
    // Reset the touched rows.
    reset_rows(&mut scratch.light, &mut scratch.touched);
    if let (true, Some(color)) = (colored, scratch.color.as_deref_mut()) {
        reset_rows(&mut color.rgb, &mut color.touched);
    }
    LightPath::Full {
        sky_pops,
        block_pops,
        colored_pops,
    }
}

/// Zeroes the rows of `light` marked in `touched`, and clears `touched`.
fn reset_rows(light: &mut BoxLight, touched: &mut [u64; ROW_WORDS]) {
    for (w, word) in touched.iter_mut().enumerate() {
        let mut bits = std::mem::take(word);
        while bits != 0 {
            let row = (w << 6) | bits.trailing_zeros() as usize;
            bits &= bits - 1;
            light[row << 6..(row + 1) << 6].fill(0);
        }
    }
}

/// `out = Uniform(v)`, keeping its buffers as spares.
fn set_uniform(out: &mut PaddedLight, scratch: &mut LightScratch, v: LightValue) {
    match std::mem::replace(out, PaddedLight::Uniform(v)) {
        PaddedLight::Uniform(_) => {}
        PaddedLight::Full(values) => scratch.keep_spare(values),
        PaddedLight::Colored { light, sky_rgb } => {
            scratch.keep_spare(light);
            scratch.rgb_spare = Some(sky_rgb);
        }
    }
}

/// `out = Full(values)`, keeping its buffers as spares.
fn set_full(out: &mut PaddedLight, scratch: &mut LightScratch, values: Box<[u16; PADDED_VOLUME]>) {
    match std::mem::replace(out, PaddedLight::Full(values)) {
        PaddedLight::Uniform(_) => {}
        PaddedLight::Full(old) => scratch.keep_spare(old),
        PaddedLight::Colored { light, sky_rgb } => {
            scratch.keep_spare(light);
            scratch.rgb_spare = Some(sky_rgb);
        }
    }
}

/// Collects the emitters inside the box into `emitters` (box index and emission) from the
/// chunks whose palette may emit; returns the OR of their emissions.
fn box_emitters(input: &LightInput<'_>, emitters: &mut Vec<(u32, LightValue)>) -> u16 {
    emitters.clear();
    let mut any = 0u16;
    for (ci, chunk) in input.chunks.iter().enumerate() {
        let Some(chunk) = chunk.filter(|c| c.may_emit(input.tables)) else {
            continue;
        };
        let (dy, dx, dz) = (ci as i32 / 9 - 1, ci as i32 / 3 % 3 - 1, ci as i32 % 3 - 1);
        for &(p, e) in chunk.light_masks(input.tables).emitters {
            let b = |d: i32, l: u32| d * 32 + l as i32 + MARGIN as i32;
            let (bx, by, bz) = (b(dx, p.x()), b(dy, p.y()), b(dz, p.z()));
            if [bx, by, bz].iter().all(|v| (0..BOX as i32).contains(v)) {
                emitters.push((((by << 12) | (bx << 6) | bz) as u32, e));
                any |= e.0;
            }
        }
    }
    any
}

/// Box coordinate (`0..64`) to chunk offset (`-1..=1`) and local coordinate (`0..32`).
#[inline]
fn split(b: usize) -> (i32, usize) {
    let v = b as i32 - MARGIN as i32;
    (v.div_euclid(32), v.rem_euclid(32) as usize)
}

/// Fills the z-row masks of the box from one mask of each chunk: box z `0..16` is the
/// `dz = -1` chunk's local `16..32`, `16..48` the center's `0..32`, `48..64` the `dz = 1`
/// chunk's `0..16`.
fn gather_rows(masks: &[MaskRows<'_>; 27], rows: &mut BoxRows) {
    for by in 0..BOX {
        let (dy, ly) = split(by);
        for bx in 0..BOX {
            let (dx, lx) = split(bx);
            let w = ly * 32 + lx;
            let word = |dz| masks[chunk_index(dx, dy, dz)].row(w);
            rows[(by << 6) | bx] = u64::from(word(-1) >> 16)
                | (u64::from(word(0)) << 16)
                | (u64::from(word(1) & 0xFFFF) << 48);
        }
    }
}

/// Direct voxels per z row: in each box column, every voxel above the column's sky top.
fn build_direct(tiles: &[&SkyTile; 9], box_y0: i64, direct: &mut BoxRows) {
    direct.fill(0);
    for bx in 0..BOX {
        let (dx, lx) = split(bx);
        for bz in 0..BOX {
            let (dz, lz) = split(bz);
            let t = tiles[tile_index(dx, dz)].top(lx as u32, lz as u32);
            // First direct box y: t - box_y0 + 1, relative and saturated (exact at any height).
            let first = t
                .saturating_sub(box_y0)
                .saturating_add(1)
                .clamp(0, BOX as i64) as usize;
            if first < BOX {
                direct[(first << 6) | bx] |= 1 << bz;
            }
        }
    }
    // Direct from the first direct y upwards.
    for r in BOX..ROWS {
        direct[r] |= direct[r - BOX];
    }
}

/// The state of box voxel `bi` (air in a missing chunk). Only asked for the voxels the floods
/// price one by one.
fn state_at(input: &LightInput<'_>, bi: usize) -> BlockState {
    let (dy, ly) = split(bi >> 12);
    let (dx, lx) = split((bi >> 6) & 63);
    let (dz, lz) = split(bi & 63);
    input.chunks[chunk_index(dx, dy, dz)].map_or(BlockState::AIR, |c| {
        c.get(LocalPos::new(lx as u32, ly as u32, lz as u32))
    })
}

/// Which voxels the block floods price one by one, and how.
#[derive(Clone, Copy)]
enum Special<'a, 'b> {
    /// The partial voxels: `max(1, opacity)` (when some state has opacity 2..=14).
    Partial(&'a LightInput<'b>),
    /// The filter voxels, and the partial ones when some state has opacity 2..=14: the filter
    /// after the cost.
    Filter(&'a LightInput<'b>),
}

/// Writes `value` at nibble `shift` of voxel `n` (non-zero), marks its row and queues it if it
/// can still light a neighbor.
#[inline(always)]
fn raise(
    light: &mut BoxLight,
    touched: &mut [u64; ROW_WORDS],
    buckets: &mut [Vec<u32>; 16],
    n: usize,
    shift: u32,
    value: u16,
) {
    let n = n & (VOLUME - 1);
    light[n] = (light[n] & !(15 << shift)) | (value << shift);
    let row = n >> 6;
    touched[row >> 6] |= 1 << (row & 63);
    if value > 1 {
        buckets[value as usize].push(n as u32);
    }
}

/// Seeds the sky flood next to the direct voxels and runs it.
fn sky_flood<const HEAVY: bool>(
    scratch: &mut LightScratch,
    cost_of: &impl Fn(usize) -> u16,
) -> u32 {
    let step = |n: usize, level: u16| level.saturating_sub(cost_of(n));
    let LightScratch {
        light,
        opaque,
        partial,
        direct,
        closed,
        touched,
        buckets,
        ..
    } = scratch;
    for ((c, &o), &d) in closed.iter_mut().zip(opaque.iter()).zip(direct.iter()) {
        *c = o | d;
    }
    for by in 0..BOX {
        for bx in 0..BOX {
            let r = (by << 6) | bx;
            let open = !closed[r];
            if open == 0 {
                continue;
            }
            let d = direct[r];
            let mut near = (d << 1) | (d >> 1);
            if by > 0 {
                near |= direct[r - BOX];
            }
            if by + 1 < BOX {
                near |= direct[r + BOX];
            }
            if bx > 0 {
                near |= direct[r - 1];
            }
            if bx + 1 < BOX {
                near |= direct[r + 1];
            }
            let mut seeds = open & near;
            while seeds != 0 {
                let bz = seeds.trailing_zeros() as usize;
                seeds &= seeds - 1;
                let bi = (r << 6) | bz;
                let cost = if HEAVY && (partial[r] >> bz) & 1 != 0 {
                    cost_of(bi)
                } else {
                    1
                };
                // An open voxel has opacity 14 or less: the seed is at least 1.
                let level = 15 - cost.min(14);
                raise(light, touched, buckets, bi, LightValue::SKY_SHIFT, level);
            }
        }
    }
    flood::<HEAVY>(
        light,
        buckets,
        touched,
        closed,
        partial,
        LightValue::SKY_SHIFT,
        &step,
    )
}

/// Seeds the emitters inside the box (collected by [`box_emitters`]; `any` is the OR of their
/// emissions) and floods each channel present. With `SPECIAL`, the voxels of `special` are
/// priced one by one.
fn block_flood<const SPECIAL: bool>(
    any: u16,
    scratch: &mut LightScratch,
    special: Special<'_, '_>,
) -> u32 {
    let LightScratch {
        light,
        opaque,
        partial,
        special: filter_rows,
        touched,
        buckets,
        emitters,
        ..
    } = scratch;
    let mut pops = 0;
    for (k, shift) in LightValue::BLOCK_SHIFTS.into_iter().enumerate() {
        if (any >> shift) & 15 == 0 {
            continue;
        }
        for &(bi, e) in emitters.iter() {
            let level = (e.0 >> shift) & 15;
            if level > (light[bi as usize] >> shift) & 15 {
                raise(light, touched, buckets, bi as usize, shift, level);
            }
        }
        pops += match special {
            Special::Partial(input) => {
                let step = |n: usize, level: u16| {
                    let opacity = input.tables.opacity(state_at(input, n));
                    level.saturating_sub(u16::from(opacity.max(1)))
                };
                flood::<SPECIAL>(light, buckets, touched, opaque, partial, shift, &step)
            }
            Special::Filter(input) => {
                let step = |n: usize, level: u16| {
                    let s = state_at(input, n);
                    let cost = u16::from(input.tables.opacity(s).max(1));
                    transmit(level.saturating_sub(cost), input.tables.light_filter(s)[k])
                };
                flood::<SPECIAL>(light, buckets, touched, opaque, filter_rows, shift, &step)
            }
        };
    }
    pops
}

/// Floods one channel (`shift`) from the queued voxels, highest level first. `closed` voxels
/// are never entered (they may still be sources). Entering a voxel from one at `level` gives it
/// `level - 1`, or (`SPECIAL`) `step(voxel, level)` for the voxels of `special`; `step` must
/// give less than `level` and never less for a higher `level`. Returns the voxels popped.
fn flood<const SPECIAL: bool>(
    light: &mut BoxLight,
    buckets: &mut [Vec<u32>; 16],
    touched: &mut [u64; ROW_WORDS],
    closed: &BoxRows,
    special: &BoxRows,
    shift: u32,
    step: &impl Fn(usize, u16) -> u16,
) -> u32 {
    let mut pops = 0;
    for level in (2..16u16).rev() {
        let mut bucket = std::mem::take(&mut buckets[level as usize]);
        for &bi in &bucket {
            let bi = bi as usize & (VOLUME - 1);
            // Raised again from a higher level (block light only): already flooded.
            if (light[bi] >> shift) & 15 != level {
                continue;
            }
            pops += 1;
            let mut relax = |n: usize| {
                let n = n & (VOLUME - 1);
                let (row, bit) = (n >> 6, n & 63);
                if (closed[row] >> bit) & 1 != 0 {
                    return;
                }
                let value = if SPECIAL && (special[row] >> bit) & 1 != 0 {
                    step(n, level)
                } else {
                    level - 1
                };
                if (light[n] >> shift) & 15 < value {
                    raise(light, touched, buckets, n, shift, value);
                }
            };
            let (bz, bx, by) = (bi & 63, (bi >> 6) & 63, bi >> 12);
            if bz > 0 {
                relax(bi - 1);
            }
            if bz + 1 < BOX {
                relax(bi + 1);
            }
            if bx > 0 {
                relax(bi - BOX);
            }
            if bx + 1 < BOX {
                relax(bi + BOX);
            }
            if by > 0 {
                relax(bi - ROWS);
            }
            if by + 1 < BOX {
                relax(bi + ROWS);
            }
        }
        bucket.clear();
        buckets[level as usize] = bucket;
    }
    pops
}

/// Copies the padded region out of the box into `out` (direct voxels get sky 15); a uniform
/// result becomes `Uniform`.
fn extract(scratch: &mut LightScratch, out: &mut PaddedLight) {
    let mut values = scratch.take_spare();
    fill_padded(scratch, &mut values);
    finish(scratch, out, values);
}

/// The padded region of the box's light (direct voxels get sky 15).
fn fill_padded(scratch: &LightScratch, values: &mut [u16; PADDED_VOLUME]) {
    for py in 0..PADDED {
        let by = py + PADDED_START;
        for px in 0..PADDED {
            let bx = px + PADDED_START;
            let r = (by << 6) | bx;
            let src = (r << 6) + PADDED_START;
            let dst = &mut values[(py * PADDED + px) * PADDED..][..PADDED];
            dst.copy_from_slice(&scratch.light[src..src + PADDED]);
            or_rows(
                dst,
                (scratch.direct[r] >> PADDED_START) & PADDED_ROW,
                SKY_BITS,
            );
        }
    }
}

/// `dst[i] |= value` for every bit `i` of `bits`.
#[inline]
fn or_rows(dst: &mut [u16], mut bits: u64, value: u16) {
    if bits == PADDED_ROW {
        for v in dst.iter_mut() {
            *v |= value;
        }
    } else {
        while bits != 0 {
            dst[bits.trailing_zeros() as usize] |= value;
            bits &= bits - 1;
        }
    }
}

/// `out` from the padded values: `Uniform` if they are all equal, `Full` otherwise.
fn finish(scratch: &mut LightScratch, out: &mut PaddedLight, values: Box<[u16; PADDED_VOLUME]>) {
    let first = values[0];
    if values.iter().all(|&v| v == first) {
        scratch.keep_spare(values);
        set_uniform(out, scratch, LightValue(first));
    } else {
        set_full(out, scratch, values);
    }
}

/// [`extract`] of a colored box: the light as there, and the sky's three channels (white direct
/// voxels get 15 in each). `Colored` only if some padded voxel's sky channels differ from its
/// uncolored sky; otherwise exactly what [`extract`] gives.
fn extract_colored(scratch: &mut LightScratch, out: &mut PaddedLight) {
    let mut values = scratch.take_spare();
    fill_padded(scratch, &mut values);
    let mut rgb = scratch.rgb_spare.take().unwrap_or_else(new_full);
    let color = scratch.color.as_deref().expect("colored scratch");
    for py in 0..PADDED {
        let by = py + PADDED_START;
        for px in 0..PADDED {
            let bx = px + PADDED_START;
            let r = (by << 6) | bx;
            let src = (r << 6) + PADDED_START;
            let dst = &mut rgb[(py * PADDED + px) * PADDED..][..PADDED];
            dst.copy_from_slice(&color.rgb[src..src + PADDED]);
            let white = scratch.direct[r] & !color.tinted[r];
            or_rows(dst, (white >> PADDED_START) & PADDED_ROW, WHITE_SKY);
        }
    }
    if sky_is_gray(&values[..], &rgb[..]) {
        scratch.rgb_spare = Some(rgb);
        finish(scratch, out, values);
    } else {
        match std::mem::replace(
            out,
            PaddedLight::Colored {
                light: values,
                sky_rgb: rgb,
            },
        ) {
            PaddedLight::Uniform(_) => {}
            PaddedLight::Full(old) => scratch.keep_spare(old),
            PaddedLight::Colored { light, sky_rgb } => {
                scratch.keep_spare(light);
                scratch.rgb_spare = Some(sky_rgb);
            }
        }
    }
}

/// Per-channel max of two colors.
#[inline]
fn max_color(a: u16, b: u16) -> u16 {
    LightValue(a).max(LightValue(b)).0
}

/// The tinted direct voxels of the columns (ADR 0064): every column-direct voxel of the box
/// under some tint of its column gets the filtered sky of its height in `color.rgb`, and its bit
/// in `color.tinted` (every row of which is written). One sweep down the box: per column its
/// tint above the box, then the tints met inside it, in height order.
fn tint_columns(tiles: &[&SkyTile; 9], box_y0: i64, direct: &BoxRows, color: &mut ColorScratch) {
    let top = box_y0 + BOX as i64 - 1;
    // Columns (bit `bz` of word `bx`) whose current tint is not white.
    let mut tinted_cols = [0u64; BOX];
    color.events.clear();
    for (bx, cols) in tinted_cols.iter_mut().enumerate() {
        let (dx, lx) = split(bx);
        for bz in 0..BOX {
            let (dz, lz) = split(bz);
            let col = (bx << 6) | bz;
            let tints = tiles[tile_index(dx, dz)].tints_at((lx << 5) | lz);
            color.cur[col] = WHITE_SKY;
            if tints.is_empty() {
                continue;
            }
            let mut c = WHITE_SKY;
            let mut rest = tints;
            // Highest first: those above the box tint the whole column inside it.
            while let [t, tail @ ..] = rest
                && t.y > top
            {
                c = filter_sky(c, t.filter);
                rest = tail;
            }
            color.cur[col] = c;
            if c != WHITE_SKY {
                *cols |= 1 << bz;
            }
            for t in rest {
                if t.y < box_y0 {
                    break;
                }
                c = filter_sky(c, t.filter);
                let by = (t.y - box_y0) as u32;
                color
                    .events
                    .push((by << 24) | ((col as u32) << 12) | u32::from(c));
            }
        }
    }
    // Highest row first; one tint per voxel, so the order inside a row does not matter.
    color.events.sort_unstable_by(|a, b| b.cmp(a));
    let mut e = 0;
    for by in (0..BOX).rev() {
        while let Some(&ev) = color.events.get(e)
            && (ev >> 24) as usize == by
        {
            let col = ((ev >> 12) & 0xFFF) as usize;
            let c = (ev & 0xFFF) as u16;
            color.cur[col] = c;
            let (bx, bz) = (col >> 6, col & 63);
            if c == WHITE_SKY {
                tinted_cols[bx] &= !(1 << bz);
            } else {
                tinted_cols[bx] |= 1 << bz;
            }
            e += 1;
        }
        for (bx, &cols) in tinted_cols.iter().enumerate() {
            let r = (by << 6) | bx;
            let mut t = direct[r] & cols;
            color.tinted[r] = t;
            if t == 0 {
                continue;
            }
            color.touched[r >> 6] |= 1 << (r & 63);
            let cur = &color.cur[bx << 6..(bx + 1) << 6];
            let dst = &mut color.rgb[r << 6..(r + 1) << 6];
            while t != 0 {
                let bz = t.trailing_zeros() as usize;
                t &= t - 1;
                dst[bz] = cur[bz];
            }
        }
    }
}

/// True if some beam that crosses a filter has a cell in the box whose bottom row is `box_y0`
/// (from the tiles' bounds).
fn tinted_beams_cross(beams: &[Option<&BeamTile>; 9], box_y0: i64) -> bool {
    let top = box_y0 + BOX as i64 - 1;
    beams
        .iter()
        .flatten()
        .any(|t| t.has_tints() && t.may_cross(box_y0, top))
}

/// Writes the cells of the beams that cross a filter (left out by `add_beams`) into `direct`,
/// with their filtered sky (ADR 0064): a cell already white stays white, one already tinted
/// takes the per-channel max. Their columns and the columns beside them go into `color.edge`.
fn add_tinted_beams(
    beams: &[Option<&BeamTile>; 9],
    box_y0: i64,
    direct: &mut BoxRows,
    color: &mut ColorScratch,
) {
    let top = BOX as i64 - 1;
    for (ti, tile) in beams.iter().enumerate() {
        let Some(tile) = tile.filter(|t| t.has_tints() && t.may_cross(box_y0, box_y0 + top)) else {
            continue;
        };
        let shift = tile.base() - box_y0;
        for bx in box_span(ti / 3) {
            for bz in box_span(ti % 3) {
                let col = (split(bx).1 << 5) | split(bz).1;
                let mut any = false;
                for &b in tile.packed(col).iter().filter(|b| b.tinted()) {
                    let (lo, hi) = (i64::from(b.lo) + shift, i64::from(b.hi) + shift);
                    if lo > top || hi < 0 {
                        continue;
                    }
                    tile.beam_colors(col, b, |cell, c| {
                        let by = cell + shift;
                        if !(0..=top).contains(&by) {
                            return;
                        }
                        let r = ((by as usize) << 6) | bx;
                        let bit = 1u64 << bz;
                        let v = (r << 6) | bz;
                        if direct[r] & bit != 0 && color.tinted[r] & bit == 0 {
                            return;
                        }
                        let c = if color.tinted[r] & bit != 0 {
                            max_color(color.rgb[v], c)
                        } else {
                            c
                        };
                        direct[r] |= bit;
                        if c == WHITE_SKY {
                            color.tinted[r] &= !bit;
                        } else {
                            color.tinted[r] |= bit;
                            color.rgb[v] = c;
                            color.touched[r >> 6] |= 1 << (r & 63);
                        }
                        any = true;
                    });
                }
                if any {
                    let bit = 1u64 << bz;
                    let side = (bit << 1) | (bit >> 1) | bit;
                    color.edge[bx] |= side;
                    if bx > 0 {
                        color.edge[bx - 1] |= bit;
                    }
                    if bx + 1 < BOX {
                        color.edge[bx + 1] |= bit;
                    }
                }
            }
        }
    }
}

/// Marks in `color.edge` the box columns next to a column with other tints: there two tinted
/// voxels side by side may differ, so the floods start from them. A column without tints needs
/// no mark: its voxels are white (closed), opaque, or not direct, and a tinted voxel next to one
/// that is not direct is in the boundary anyway.
fn mark_tint_edges(tiles: &[&SkyTile; 9], color: &mut ColorScratch) {
    let tints_of = |bx: usize, bz: usize| {
        let (dx, lx) = split(bx);
        let (dz, lz) = split(bz);
        tiles[tile_index(dx, dz)].tints_at((lx << 5) | lz)
    };
    for bx in 0..BOX {
        for bz in 0..BOX {
            let here = tints_of(bx, bz);
            if here.is_empty() {
                continue;
            }
            let bit = 1u64 << bz;
            let differs = |there: &[crate::Tint]| !there.is_empty() && there != here;
            if bz + 1 < BOX && differs(tints_of(bx, bz + 1)) {
                color.edge[bx] |= bit | (bit << 1);
            }
            if bx + 1 < BOX && differs(tints_of(bx + 1, bz)) {
                color.edge[bx] |= bit;
                color.edge[bx + 1] |= bit;
            }
        }
    }
}

/// The three sky floods of a colored box (ADR 0064), on `color.rgb`, after the uncolored one.
/// Sources: the white direct voxels (closed, 15) seed their open neighbors; the tinted ones hold
/// their filtered level and are queued where a neighbor may need it (`boundary`). Filter voxels
/// (and partial ones, with `heavy`) are priced one by one: the filter after the cost. Returns
/// the voxels popped.
fn colored_floods(
    input: &LightInput<'_>,
    scratch: &mut LightScratch,
    heavy: bool,
    filter_voxels: bool,
) -> u32 {
    let LightScratch {
        opaque,
        partial,
        direct,
        special,
        buckets,
        color,
        ..
    } = scratch;
    let color = color.as_deref_mut().expect("colored scratch");
    if let SkyInput::Tiles(tiles) = &input.sky {
        mark_tint_edges(tiles, color);
    }
    let ColorScratch {
        rgb,
        tinted,
        closed,
        boundary,
        touched,
        edge,
        ..
    } = color;
    for r in 0..ROWS {
        closed[r] = opaque[r] | (direct[r] & !tinted[r]);
    }
    // Tinted voxels next to an open non-direct voxel, or in an edge column.
    let not_direct = |r: usize| !opaque[r] & !direct[r];
    for by in 0..BOX {
        for (bx, &edge_x) in edge.iter().enumerate() {
            let r = (by << 6) | bx;
            let t = tinted[r];
            if t == 0 {
                boundary[r] = 0;
                continue;
            }
            let n = not_direct(r);
            let mut near = (n << 1) | (n >> 1) | edge_x;
            if by > 0 {
                near |= not_direct(r - BOX);
            }
            if by + 1 < BOX {
                near |= not_direct(r + BOX);
            }
            if bx > 0 {
                near |= not_direct(r - 1);
            }
            if bx + 1 < BOX {
                near |= not_direct(r + 1);
            }
            boundary[r] = t & near;
        }
    }
    // The rows each flood prices one by one.
    let special_rows: &BoxRows = if filter_voxels { special } else { partial };
    let priced = filter_voxels || heavy;
    let mut pops = 0;
    for (k, shift) in LightValue::BLOCK_SHIFTS.into_iter().enumerate() {
        let step = |n: usize, level: u16| {
            let s = state_at(input, n);
            let cost = u16::from(input.tables.opacity(s).max(1));
            transmit(level.saturating_sub(cost), input.tables.light_filter(s)[k])
        };
        // The tinted sources, at their filtered level.
        for (r, &b) in boundary.iter().enumerate() {
            let mut bits = b;
            while bits != 0 {
                let v = (r << 6) | bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let level = (rgb[v] >> shift) & 15;
                if level > 1 {
                    buckets[level as usize].push(v as u32);
                }
            }
        }
        // The open voxels next to a white direct one, from 15.
        for by in 0..BOX {
            for bx in 0..BOX {
                let r = (by << 6) | bx;
                let open = !closed[r];
                if open == 0 {
                    continue;
                }
                let white = |r: usize| direct[r] & !tinted[r];
                let w = white(r);
                let mut near = (w << 1) | (w >> 1);
                if by > 0 {
                    near |= white(r - BOX);
                }
                if by + 1 < BOX {
                    near |= white(r + BOX);
                }
                if bx > 0 {
                    near |= white(r - 1);
                }
                if bx + 1 < BOX {
                    near |= white(r + 1);
                }
                let mut seeds = open & near;
                while seeds != 0 {
                    let bz = seeds.trailing_zeros() as usize;
                    seeds &= seeds - 1;
                    let v = (r << 6) | bz;
                    let level = if priced && (special_rows[r] >> bz) & 1 != 0 {
                        step(v, 15)
                    } else {
                        14
                    };
                    if level > (rgb[v] >> shift) & 15 {
                        raise(rgb, touched, buckets, v, shift, level);
                    }
                }
            }
        }
        pops += if priced {
            flood::<true>(rgb, buckets, touched, closed, special_rows, shift, &step)
        } else {
            flood::<false>(rgb, buckets, touched, closed, special_rows, shift, &step)
        };
    }
    pops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(air: &'a ChunkData, tile: &'a SkyTile, tables: &'a LightTables) -> LightInput<'a> {
        LightInput {
            origin: ChunkPos::new(0, 0, 0),
            chunks: [Some(air); 27],
            sky: SkyInput::Tiles([tile; 9]),
            beams: [None; 9],
            tables,
            colored_sky: false,
        }
    }

    #[test]
    fn alternating_results_reuse_two_padded_buffers() {
        let tables = LightTables::from_registry(&boxel_demo::blocks::registry());
        let air = ChunkData::uniform(BlockState::AIR);
        // Sky tops 10 voxels into the chunk (a full result) and far above it (uniform dark).
        let mixed = SkyTile::uniform(10, i64::MIN);
        let roof = SkyTile::uniform(1000, i64::MIN);
        let mut scratch = LightScratch::new();
        let mut out = PaddedLight::default();
        for i in 0..6 {
            compute_full(&input(&air, &mixed, &tables), &mut scratch, &mut out);
            assert!(out.as_uniform().is_none());
            // Uniform from a flood (extract) and from the fast path (set_uniform), in turn.
            let dark = input(&air, &roof, &tables);
            let path = if i % 2 == 0 {
                compute_full(&dark, &mut scratch, &mut out)
            } else {
                compute(&dark, &mut scratch, &mut out)
            };
            assert_eq!(path == LightPath::FastDark, i % 2 == 1);
            assert_eq!(out.as_uniform(), Some(LightValue::DARK));
        }
        assert_eq!(scratch.allocations, 2);
        assert!(scratch.spares.len() <= MAX_SPARES);
    }
}
