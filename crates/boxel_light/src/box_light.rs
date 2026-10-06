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
//! **Steps** ([`compute`]):
//! 1. Fast paths, from the palettes and the tiles' bounds alone: no emitter and the box under
//!    every sky top is uniformly dark ([`LightPath::FastDark`]); no emitter and the padded
//!    chunk above every sky top is uniformly open sky ([`LightPath::FastOpen`]).
//! 2. Opaque (and, when some state has opacity 2..=14, partial) z-row masks of the box, from
//!    the 27 cached [`ChunkMasks`].
//! 3. Sky: the direct voxels (above the top of their column) as row masks, built from the 9
//!    tiles; seeds are the open non-direct voxels next to a direct one, at `15 - cost`; then a
//!    flood into open non-direct voxels.
//! 4. Block light: one flood per channel present, seeded by the emitters inside the box.
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
use crate::padded::{PADDED, PaddedLight, new_full};
use boxel_voxel::{ChunkData, ChunkMasks, ChunkPos, LightTables, LightValue, LocalPos, Occupancy};

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
    /// Opacity and emission of each state (built from the registry the chunks use).
    pub tables: &'a LightTables,
}

/// How a box was computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    },
}

/// Per-worker scratch of [`compute`] (about 0.7 MiB). Reused across calls: its light array is
/// all zero between calls.
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
    /// Rows with a non-zero light value (to reset them).
    touched: [u64; ROW_WORDS],
    /// Flood queue: voxels raised to level `l` (2 or more) wait in bucket `l`.
    buckets: [Vec<u32>; 16],
    /// Emitters inside the box: box index and emission.
    emitters: Vec<(u32, LightValue)>,
    /// A spare padded buffer, so a uniform result does not drop the allocation.
    spare: Option<Box<[u16; crate::PADDED_VOLUME]>>,
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
            touched: [0; ROW_WORDS],
            buckets: Default::default(),
            emitters: Vec::new(),
            spare: None,
        }
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

fn classify(sky: &SkyInput<'_>, origin_y: i64) -> Sky {
    let SkyInput::Tiles(tiles) = sky else {
        return Sky::Dark;
    };
    let min = tiles.iter().map(|t| t.min()).min().unwrap_or(i64::MIN);
    let max = tiles.iter().map(|t| t.max()).max().unwrap_or(i64::MIN);
    // Box top: origin + 31 + MARGIN. Padded bottom: origin - 1.
    if origin_y.saturating_add(31 + MARGIN) <= min {
        Sky::Dark
    } else if origin_y.saturating_sub(1) > max {
        Sky::Open
    } else {
        Sky::Mixed
    }
}

fn run(
    input: &LightInput<'_>,
    scratch: &mut LightScratch,
    out: &mut PaddedLight,
    force_full: bool,
) -> LightPath {
    let origin_y = input.origin.origin().y;
    let tables = input.tables;
    let sky = classify(&input.sky, origin_y);
    let emits = input.chunks.iter().flatten().any(|c| c.may_emit(tables));
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
    let masks: [&ChunkMasks; 27] =
        std::array::from_fn(|i| input.chunks[i].map_or(ChunkMasks::empty(), |c| c.masks(tables)));
    let heavy = tables.max_partial_opacity() > 1;
    gather_rows(&masks, |m| &m.opaque, &mut scratch.opaque);
    if heavy {
        gather_rows(&masks, |m| &m.partial, &mut scratch.partial);
    }
    let cost_of = |bi: usize| opacity_cost(input, bi);
    let sky = match (force_full, &input.sky) {
        (true, SkyInput::Tiles(_)) => Sky::Mixed,
        _ => sky,
    };
    let mut sky_pops = 0;
    match (sky, &input.sky) {
        (Sky::Mixed, SkyInput::Tiles(tiles)) => {
            build_direct(tiles, origin_y - MARGIN, &mut scratch.direct);
            sky_pops = if heavy {
                sky_flood::<true>(scratch, &cost_of)
            } else {
                sky_flood::<false>(scratch, &cost_of)
            };
        }
        (Sky::Open, _) => scratch.direct.fill(u64::MAX),
        _ => scratch.direct.fill(0),
    }
    let block_pops = match (emits, heavy) {
        (false, _) => 0,
        (true, true) => block_flood::<true>(input, &masks, scratch, &cost_of),
        (true, false) => block_flood::<false>(input, &masks, scratch, &cost_of),
    };
    extract(scratch, out);
    // Reset the touched rows.
    for (w, word) in scratch.touched.iter_mut().enumerate() {
        let mut bits = std::mem::take(word);
        while bits != 0 {
            let row = (w << 6) | bits.trailing_zeros() as usize;
            bits &= bits - 1;
            scratch.light[row << 6..(row + 1) << 6].fill(0);
        }
    }
    LightPath::Full {
        sky_pops,
        block_pops,
    }
}

/// `out = Uniform(v)`, keeping a `Full` buffer as the spare.
fn set_uniform(out: &mut PaddedLight, scratch: &mut LightScratch, v: LightValue) {
    if let PaddedLight::Full(values) = std::mem::replace(out, PaddedLight::Uniform(v))
        && scratch.spare.is_none()
    {
        scratch.spare = Some(values);
    }
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
fn gather_rows(
    masks: &[&ChunkMasks; 27],
    pick: impl Fn(&ChunkMasks) -> &Occupancy,
    rows: &mut BoxRows,
) {
    for by in 0..BOX {
        let (dy, ly) = split(by);
        for bx in 0..BOX {
            let (dx, lx) = split(bx);
            let w = ly * 32 + lx;
            let word = |dz| pick(masks[chunk_index(dx, dy, dz)])[w];
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

/// `max(1, opacity)` of box voxel `bi` (only asked for partial voxels when some state has
/// opacity 2..=14).
fn opacity_cost(input: &LightInput<'_>, bi: usize) -> u16 {
    let (dy, ly) = split(bi >> 12);
    let (dx, lx) = split((bi >> 6) & 63);
    let (dz, lz) = split(bi & 63);
    let opacity = input.chunks[chunk_index(dx, dy, dz)].map_or(0, |c| {
        input
            .tables
            .opacity(c.get(LocalPos::new(lx as u32, ly as u32, lz as u32)))
    });
    u16::from(opacity.max(1))
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
        cost_of,
    )
}

/// Seeds the emitters inside the box and floods each channel present.
fn block_flood<const HEAVY: bool>(
    input: &LightInput<'_>,
    masks: &[&ChunkMasks; 27],
    scratch: &mut LightScratch,
    cost_of: &impl Fn(usize) -> u16,
) -> u32 {
    let LightScratch {
        light,
        opaque,
        partial,
        touched,
        buckets,
        emitters,
        ..
    } = scratch;
    emitters.clear();
    let mut any = 0u16;
    for (ci, chunk) in input.chunks.iter().enumerate() {
        if !chunk.is_some_and(|c| c.may_emit(input.tables)) {
            continue;
        }
        let (dy, dx, dz) = (ci as i32 / 9 - 1, ci as i32 / 3 % 3 - 1, ci as i32 % 3 - 1);
        for &(p, e) in &masks[ci].emitters {
            let b = |d: i32, l: u32| d * 32 + l as i32 + MARGIN as i32;
            let (bx, by, bz) = (b(dx, p.x()), b(dy, p.y()), b(dz, p.z()));
            if [bx, by, bz].iter().all(|v| (0..BOX as i32).contains(v)) {
                emitters.push((((by << 12) | (bx << 6) | bz) as u32, e));
                any |= e.0;
            }
        }
    }
    let mut pops = 0;
    for shift in LightValue::BLOCK_SHIFTS {
        if (any >> shift) & 15 == 0 {
            continue;
        }
        for &(bi, e) in emitters.iter() {
            let level = (e.0 >> shift) & 15;
            if level > (light[bi as usize] >> shift) & 15 {
                raise(light, touched, buckets, bi as usize, shift, level);
            }
        }
        pops += flood::<HEAVY>(light, buckets, touched, opaque, partial, shift, cost_of);
    }
    pops
}

/// Floods one channel (`shift`) from the queued voxels, highest level first. `closed` voxels
/// are never entered (they may still be sources). Entering a voxel costs 1, or (`HEAVY`)
/// `cost_of` for the voxels of `partial`. Returns the voxels popped.
fn flood<const HEAVY: bool>(
    light: &mut BoxLight,
    buckets: &mut [Vec<u32>; 16],
    touched: &mut [u64; ROW_WORDS],
    closed: &BoxRows,
    partial: &BoxRows,
    shift: u32,
    cost_of: &impl Fn(usize) -> u16,
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
                let cost = if HEAVY && (partial[row] >> bit) & 1 != 0 {
                    cost_of(n)
                } else {
                    1
                };
                if level > cost && (light[n] >> shift) & 15 < level - cost {
                    raise(light, touched, buckets, n, shift, level - cost);
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
    let mut values = scratch.spare.take().unwrap_or_else(new_full);
    for py in 0..PADDED {
        let by = py + PADDED_START;
        for px in 0..PADDED {
            let bx = px + PADDED_START;
            let r = (by << 6) | bx;
            let src = (r << 6) + PADDED_START;
            let dst = &mut values[(py * PADDED + px) * PADDED..][..PADDED];
            dst.copy_from_slice(&scratch.light[src..src + PADDED]);
            let mut d = (scratch.direct[r] >> PADDED_START) & PADDED_ROW;
            if d == PADDED_ROW {
                for v in dst.iter_mut() {
                    *v |= SKY_BITS;
                }
            } else {
                while d != 0 {
                    dst[d.trailing_zeros() as usize] |= SKY_BITS;
                    d &= d - 1;
                }
            }
        }
    }
    let first = values[0];
    if values.iter().all(|&v| v == first) {
        scratch.spare = Some(values);
        set_uniform(out, scratch, LightValue(first));
    } else if let PaddedLight::Full(old) = std::mem::replace(out, PaddedLight::Full(values)) {
        scratch.spare = Some(old);
    }
}
