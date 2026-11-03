//! Point lights baked as block light (ADR 0063): a light that is not a block (a torch in the
//! hand, `boxel_world`'s dynamic lights) shining from a voxel, added to the light of the
//! chunks around it when they are meshed.
//!
//! [`add_point_lights`] floods each light from its voxel through the blocks of a light box with
//! the block light rules ([`boxel_voxel::light`]: opacity 15 stops, entering costs
//! `max(1, opacity)`, filters scale each channel) and raises the box's padded light to it by the
//! brightest per channel. Block light mixes by the brightest per channel and its rules are
//! monotone, so the light of every emitter together is the brightest of the light of each one
//! alone: a point light at an air voxel gives, bit for bit, the light of an emitter of its
//! emission placed there (one that lets light in as air does). It goes around walls as a placed
//! torch does, and never through them.
//!
//! Each light floods only the cube of voxels it can light (its voxel +-14) that lies in the box:
//! a path from it to the padded chunk is at most 14 voxels long, so it stays in the box (the
//! argument of [`crate::box_light`]). The flood reads the box's chunks voxel by voxel, only
//! where the light goes: a few thousand voxels in open air, fewer in a cave.
use crate::PaddedLight;
use crate::box_light::{LightInput, MARGIN, chunk_index};
use crate::padded::new_full;
use boxel_voxel::{BlockPos, CHUNK_SIZE, ChunkPos, LightValue, LocalPos, MAX_LIGHT, transmit};

/// Farthest voxel (along each axis) a level-15 light reaches with level 1 or more.
pub const MAX_REACH: i64 = MAX_LIGHT as i64 - 1;
/// Edge of the cube a light floods: its voxel +-[`MAX_REACH`].
const CUBE: usize = 2 * MAX_REACH as usize + 1;
/// Voxels of the cube.
const CUBE_VOLUME: usize = CUBE * CUBE * CUBE;

/// A point light baked into the light of the chunks around it: an emitter at `pos` that is not
/// a block. Ordered by position first, so a sorted list is deterministic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PointEmitter {
    /// The voxel it shines from.
    pub pos: BlockPos,
    /// Level per block light channel (red, green, blue), `0..=15` (more reads as 15).
    pub emission: [u8; 3],
}

impl PointEmitter {
    /// The light at `pos` with `emission` (clamped to 15 per channel).
    pub fn new(pos: BlockPos, emission: [u8; 3]) -> Self {
        Self {
            pos,
            emission: emission.map(|e| e.min(MAX_LIGHT)),
        }
    }

    /// True when it lights nothing (every channel 0).
    pub fn is_dark(&self) -> bool {
        self.emission == [0; 3]
    }

    /// How far it reaches along each axis with level 1 or more: its brightest channel minus 1
    /// (0 for a dark light, which lights nothing).
    pub fn reach(&self) -> u8 {
        self.emission
            .iter()
            .map(|&e| e.min(MAX_LIGHT))
            .max()
            .unwrap_or(0)
            .saturating_sub(1)
    }

    /// True if it may light a voxel of the padded chunk at `chunk` (local `-1..=32`): its cube
    /// of reach meets it. Exact `i64` at any position.
    pub fn reaches_chunk(&self, chunk: ChunkPos) -> bool {
        if self.is_dark() {
            return false;
        }
        let r = i64::from(self.reach());
        let o = chunk.origin();
        [(self.pos.x, o.x), (self.pos.y, o.y), (self.pos.z, o.z)]
            .iter()
            .all(|&(p, o)| {
                // The padded chunk is o - 1 ..= o + 32; the cube p - r ..= p + r.
                let d = p.saturating_sub(o);
                d >= -1 - r && d <= CHUNK_SIZE as i64 + r
            })
    }
}

/// Scratch of [`add_point_lights`] (about 72 KiB), reused across calls.
pub struct PointScratch {
    /// Level of the channel being flooded, per cube voxel.
    level: Box<[u8; CUBE_VOLUME]>,
    /// The state of each cube voxel plus 1, as it was read (0: not read yet; `u16::MAX`: outside
    /// the box, never entered).
    state: Box<[u16; CUBE_VOLUME]>,
    /// Cube voxels with a level (to reset them and to merge them).
    touched: Vec<u32>,
    /// Flood queue: voxels raised to level `l` (2 or more) wait in bucket `l`.
    buckets: [Vec<u32>; 16],
}

impl Default for PointScratch {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PointScratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PointScratch").finish_non_exhaustive()
    }
}

impl PointScratch {
    /// Fresh scratch.
    pub fn new() -> Self {
        Self {
            level: vec![0; CUBE_VOLUME]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!("CUBE_VOLUME entries")),
            state: vec![0; CUBE_VOLUME]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!("CUBE_VOLUME entries")),
            touched: Vec::new(),
            buckets: Default::default(),
        }
    }
}

/// Index of cube offset `(dx, dy, dz)`, each in `-14..=14`.
#[inline]
fn cube_index(d: [i64; 3]) -> usize {
    let c = |v: i64| (v + MAX_REACH) as usize;
    (c(d[1]) * CUBE + c(d[0])) * CUBE + c(d[2])
}

/// The cube offset of index `i`.
#[inline]
fn cube_offset(i: usize) -> [i64; 3] {
    let (z, x, y) = (i % CUBE, i / CUBE % CUBE, i / (CUBE * CUBE));
    [x, y, z].map(|v| v as i64 - MAX_REACH)
}

/// Raises the block channels of `light`, the padded light of `input.origin` as
/// [`crate::compute`] gives it for `input`, to the light of `lights`: per channel, the
/// brightest of its own value and each light's flood (see the [module docs](self)). The sky
/// channel never changes. Lights that cannot reach the padded chunk are skipped. Returns how
/// many lights reached it.
///
/// A light at a voxel that lets light in as air does (opacity 0 or 1, no filter) gives exactly
/// the light of an emitter of its emission placed there. Inside a block, it still shines out as
/// an emitter does, even an opaque one.
pub fn add_point_lights(
    input: &LightInput<'_>,
    lights: &[PointEmitter],
    scratch: &mut PointScratch,
    light: &mut PaddedLight,
) -> usize {
    let origin = input.origin.origin();
    let mut reached = 0;
    for l in lights.iter().filter(|l| l.reaches_chunk(input.origin)) {
        reached += 1;
        // The light's voxel relative to the chunk's origin: small, as it reaches the chunk.
        let rel = [l.pos.x - origin.x, l.pos.y - origin.y, l.pos.z - origin.z];
        scratch.state.fill(0);
        for (k, shift) in LightValue::BLOCK_SHIFTS.into_iter().enumerate() {
            let emission = l.emission[k].min(MAX_LIGHT);
            if emission == 0 {
                continue;
            }
            flood(input, rel, k, emission, scratch);
            merge(rel, shift, scratch, light);
        }
    }
    reached
}

/// Floods channel `k` from the light at chunk-local `rel` with level `emission` into
/// `scratch.level` (its voxels listed in `scratch.touched`), highest level first, as
/// `box_light` floods a block channel.
fn flood(
    input: &LightInput<'_>,
    rel: [i64; 3],
    k: usize,
    emission: u8,
    scratch: &mut PointScratch,
) {
    let PointScratch {
        level,
        state,
        touched,
        buckets,
    } = scratch;
    let tables = input.tables;
    let filters = tables.any_filter();
    let center = cube_index([0; 3]);
    level[center] = emission;
    touched.push(center as u32);
    if emission >= 2 {
        buckets[usize::from(emission)].push(center as u32);
    }
    // The state of cube voxel `i` (offset `d`) plus 1, read once; `u16::MAX` outside the box.
    let mut state_of = |i: usize, d: [i64; 3]| -> u16 {
        if state[i] == 0 {
            let local = [rel[0] + d[0], rel[1] + d[1], rel[2] + d[2]];
            let inside = local
                .iter()
                .all(|&c| (-MARGIN..CHUNK_SIZE as i64 + MARGIN).contains(&c));
            state[i] = if inside {
                let [cx, cy, cz] = local.map(|c| c.div_euclid(CHUNK_SIZE as i64) as i32);
                let [x, y, z] = local.map(|c| c.rem_euclid(CHUNK_SIZE as i64) as u32);
                let s = input.chunks[chunk_index(cx, cy, cz)]
                    .map_or(boxel_voxel::BlockState::AIR, |c| {
                        c.get(LocalPos::new(x, y, z))
                    });
                s.0.saturating_add(1).min(u16::MAX - 1)
            } else {
                u16::MAX
            };
        }
        state[i]
    };
    for l in (2..=emission).rev() {
        let mut bucket = std::mem::take(&mut buckets[usize::from(l)]);
        for &v in &bucket {
            let v = v as usize;
            // Raised again from a higher level: already flooded.
            if level[v] != l {
                continue;
            }
            let d = cube_offset(v);
            for (axis, step) in [(0, -1), (0, 1), (1, -1), (1, 1), (2, -1), (2, 1)] {
                let mut n = d;
                n[axis] += step;
                if n[axis].abs() > MAX_REACH {
                    continue;
                }
                let ni = cube_index(n);
                let s = state_of(ni, n);
                if s == u16::MAX {
                    continue;
                }
                let s = boxel_voxel::BlockState(s - 1);
                if tables.is_opaque(s) {
                    continue;
                }
                let cost = tables.opacity(s).max(1);
                let mut value = u16::from(l.saturating_sub(cost));
                if filters {
                    value = transmit(value, tables.light_filter(s)[k]);
                }
                let value = value as u8;
                if level[ni] < value {
                    if level[ni] == 0 {
                        touched.push(ni as u32);
                    }
                    level[ni] = value;
                    if value >= 2 {
                        buckets[usize::from(value)].push(ni as u32);
                    }
                }
            }
        }
        bucket.clear();
        buckets[usize::from(l)] = bucket;
    }
}

/// Raises channel `shift` of the padded voxels of `light` to the flood in `scratch` (from the
/// light at chunk-local `rel`), then clears the flood.
fn merge(rel: [i64; 3], shift: u32, scratch: &mut PointScratch, light: &mut PaddedLight) {
    let PointScratch { level, touched, .. } = scratch;
    for &v in touched.iter() {
        let v = v as usize;
        let value = u16::from(std::mem::take(&mut level[v]));
        let d = cube_offset(v);
        let [x, y, z] = [rel[0] + d[0], rel[1] + d[1], rel[2] + d[2]];
        let padded = -1..=CHUNK_SIZE as i64;
        if !(padded.contains(&x) && padded.contains(&y) && padded.contains(&z)) {
            continue;
        }
        let (x, y, z) = (x as i32, y as i32, z as i32);
        if u16::from(light.get(x, y, z).channel(shift)) >= value {
            continue;
        }
        let values = full_mut(light);
        let i = PaddedLight::index(x, y, z);
        values[i] = (values[i] & !(15 << shift)) | (value << shift);
    }
    touched.clear();
}

/// The values of `light`, made `Full` first if it was uniform.
fn full_mut(light: &mut PaddedLight) -> &mut [u16; crate::PADDED_VOLUME] {
    if let PaddedLight::Uniform(v) = *light {
        let mut values = new_full();
        values.fill(v.0);
        *light = PaddedLight::Full(values);
    }
    match light {
        PaddedLight::Full(values) => values,
        PaddedLight::Uniform(_) => unreachable!("made full above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cube index round-trips its offset.
    #[test]
    fn cube_indices_round_trip() {
        for i in [0, 1, CUBE, CUBE * CUBE, CUBE_VOLUME / 2, CUBE_VOLUME - 1] {
            assert_eq!(cube_index(cube_offset(i)), i);
        }
        assert_eq!(cube_offset(cube_index([0; 3])), [0; 3]);
        assert_eq!(cube_offset(cube_index([-14, 3, 14])), [-14, 3, 14]);
    }

    /// A light reaches a chunk exactly when its cube meets the padded chunk, on every side and
    /// next to the world limit.
    #[test]
    fn reach_is_the_brightest_channel_minus_one() {
        let l = PointEmitter::new(BlockPos::new(0, 0, 0), [14, 11, 20]);
        assert_eq!(l.emission, [14, 11, 15]);
        assert_eq!(l.reach(), 14);
        assert_eq!(PointEmitter::new(BlockPos::default(), [1, 0, 0]).reach(), 0);
        let dark = PointEmitter::new(BlockPos::default(), [0; 3]);
        assert!(dark.is_dark() && !dark.reaches_chunk(ChunkPos::default()));
        // Reach 13: x -27..=-1 and y 34..=60.
        let torch = PointEmitter::new(BlockPos::new(-14, 47, 5), [14, 11, 6]);
        assert!(torch.reaches_chunk(ChunkPos::new(0, 1, 0)));
        assert!(torch.reaches_chunk(ChunkPos::new(-1, 1, 0)));
        assert!(!torch.reaches_chunk(ChunkPos::new(1, 1, 0)));
        // The padded chunks of layers 0 and 2 end at 32 and start at 63.
        assert!(!torch.reaches_chunk(ChunkPos::new(0, 0, 0)));
        assert!(!torch.reaches_chunk(ChunkPos::new(0, 2, 0)));
        let past = PointEmitter::new(BlockPos::new(-15, 47, 5), [14, 11, 6]);
        assert!(!past.reaches_chunk(ChunkPos::new(0, 1, 0)));
        let far = boxel_voxel::MAX_BLOCK_COORD;
        let edge = PointEmitter::new(BlockPos::new(far, -far, far), [15, 0, 0]);
        let chunk = ChunkPos::containing(edge.pos);
        assert!(edge.reaches_chunk(chunk));
        assert!(!edge.reaches_chunk(ChunkPos::new(chunk.x - 2, chunk.y, chunk.z)));
    }
}
