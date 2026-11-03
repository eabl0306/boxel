//! The reference the tests compare [`compute`](crate::compute) against: a scalar global light
//! solver over an explicit voxel world, one channel at a time, with a plain FIFO queue (no
//! buckets, no masks, no box), and sky tops found by brute force. Light filters (ADR 0060) apply
//! to the block channels in [`OracleWorld::solve`], and to the sky's three channels too in
//! [`OracleWorld::solve_colored`] (ADR 0064). Sky sources (ADR 0061) light their beams, traced
//! cell by cell, as direct sky.
//!
//! Only with the `oracle` feature (and in this crate's unit tests).
use boxel_voxel::{
    BeamDir, BlockState, CHUNK_SIZE, ChunkData, ChunkPos, LightTables, LightValue, LocalPos,
    MAX_LIGHT, SkySourceDef, transmit,
};
use std::collections::VecDeque;

/// An explicit world: `dims` chunks per axis from chunk `min`, with each voxel's opacity,
/// emission and light filter, its sky sources, the realm planes and the sky top of each column.
///
/// Voxels are indexed `(y * size_x + x) * size_z + z` and columns `x * size_z + z`, in voxels
/// from the world's min corner. Light never leaves the world (there is nothing outside it).
#[derive(Clone, Debug)]
pub struct OracleWorld {
    /// Min chunk.
    pub min: ChunkPos,
    /// Chunks along x, y, z.
    pub dims: [usize; 3],
    /// Opacity per voxel.
    pub opacity: Vec<u8>,
    /// Emission per voxel.
    pub emission: Vec<LightValue>,
    /// Light filter per voxel (`[15; 3]`: none).
    pub filter: Vec<[u8; 3]>,
    /// Sky top per column (absolute y; `i64::MIN` for open forever).
    pub tops: Vec<i64>,
    /// Every sky source: voxel index and what it emits.
    pub sources: Vec<(usize, SkySourceDef)>,
    /// Realm planes (absolute y, ascending; plane `p` is the bottom of the realm above it). A
    /// beam never leaves its source's realm. Empty from [`OracleWorld::from_chunks`].
    pub planes: Vec<i64>,
}

impl OracleWorld {
    /// From chunks in `(y, x, z)` order (`chunks[(cy * dims[0] + cx) * dims[2] + cz]`), with
    /// sky tops `tops` (absolute, per column).
    pub fn from_chunks(
        min: ChunkPos,
        dims: [usize; 3],
        chunks: &[ChunkData],
        tables: &LightTables,
        tops: Vec<i64>,
    ) -> Self {
        assert_eq!(chunks.len(), dims[0] * dims[1] * dims[2]);
        let s = CHUNK_SIZE as usize;
        let size = dims.map(|d| d * s);
        assert_eq!(tops.len(), size[0] * size[2]);
        let volume = size[0] * size[1] * size[2];
        let mut opacity = vec![0u8; volume];
        let mut emission = vec![LightValue::DARK; volume];
        let mut filter = vec![[MAX_LIGHT; 3]; volume];
        let mut sources = Vec::new();
        for (ci, chunk) in chunks.iter().enumerate() {
            let (cy, cx, cz) = (
                ci / (dims[0] * dims[2]),
                ci / dims[2] % dims[0],
                ci % dims[2],
            );
            for i in 0..boxel_voxel::CHUNK_VOLUME {
                let p = LocalPos::from_index(i);
                let state: BlockState = chunk.get(p);
                let (x, y, z) = (
                    cx * s + p.x() as usize,
                    cy * s + p.y() as usize,
                    cz * s + p.z() as usize,
                );
                let v = (y * size[0] + x) * size[2] + z;
                opacity[v] = tables.opacity(state);
                emission[v] = tables.emission(state);
                filter[v] = tables.light_filter(state);
                if let Some(def) = tables.sky_source(state) {
                    sources.push((v, def));
                }
            }
        }
        Self {
            min,
            dims,
            opacity,
            emission,
            filter,
            tops,
            sources,
            planes: Vec::new(),
        }
    }

    /// Size in voxels along x, y, z.
    pub fn size(&self) -> [usize; 3] {
        self.dims.map(|d| d * CHUNK_SIZE as usize)
    }

    /// The highest voxel with opacity above 0 of each column, `i64::MIN` where there is none
    /// (absolute y).
    pub fn brute_force_tops(&self) -> Vec<i64> {
        let [sx, sy, sz] = self.size();
        let y0 = self.min.origin().y;
        let mut tops = vec![i64::MIN; sx * sz];
        for x in 0..sx {
            for z in 0..sz {
                if let Some(y) = (0..sy)
                    .rev()
                    .find(|&y| self.opacity[(y * sx + x) * sz + z] > 0)
                {
                    tops[x * sz + z] = y0 + y as i64;
                }
            }
        }
        tops
    }

    /// The beam cells of the sky sources: from each source, cell by cell in its direction, while
    /// the cell has opacity 0, at most `range` cells, inside the world and the source's realm.
    pub fn beam_cells(&self) -> Vec<bool> {
        let mut beam = vec![false; self.opacity.len()];
        self.trace_beams(|c, _| beam[c] = true);
        beam
    }

    /// Calls `f(cell, sky)` for each cell of each beam, in order from its source, with the
    /// beam's sky there: 15 per channel from the source, filtered past each filter cell (that
    /// cell included), with no cost.
    fn trace_beams(&self, mut f: impl FnMut(usize, [u8; 3])) {
        let [sx, sy, sz] = self.size();
        let y0 = self.min.origin().y;
        let realm = |y: i64| self.planes.partition_point(|&p| p <= y);
        for &(v, def) in &self.sources {
            let (y, column) = (v / (sx * sz), v % (sx * sz));
            let home = realm(y0 + y as i64);
            let mut sky = [MAX_LIGHT; 3];
            for k in 0..i64::from(def.range) {
                let cy = match def.dir {
                    BeamDir::Down => y as i64 - k,
                    BeamDir::Up => y as i64 + k,
                };
                if !(0..sy as i64).contains(&cy) || realm(y0 + cy) != home {
                    break;
                }
                let c = cy as usize * sx * sz + column;
                if self.opacity[c] > 0 {
                    break;
                }
                sky = filter(sky, self.filter[c]);
                f(c, sky);
            }
        }
    }

    /// The global light of every voxel.
    pub fn solve(&self) -> Vec<LightValue> {
        let [sx, sy, sz] = self.size();
        let y0 = self.min.origin().y;
        let volume = sx * sy * sz;
        let mut out = vec![0u16; volume];
        let beam = self.beam_cells();
        // Direct sky: above the top of the column, or in a beam.
        let direct = |v: usize| {
            let (y, x, z) = (v / (sx * sz), v / sz % sx, v % sz);
            beam[v] || y0 + y as i64 > self.tops[x * sz + z]
        };
        // Each channel with the index of its filter (`None`: sky, never filtered).
        for (shift, k) in [
            (LightValue::SKY_SHIFT, None),
            (LightValue::RED_SHIFT, Some(0)),
            (LightValue::GREEN_SHIFT, Some(1)),
            (LightValue::BLUE_SHIFT, Some(2)),
        ] {
            let mut level = vec![0u8; volume];
            let mut queue = VecDeque::new();
            for (v, l) in level.iter_mut().enumerate() {
                let source = if shift == LightValue::SKY_SHIFT {
                    if direct(v) { 15 } else { 0 }
                } else {
                    self.emission[v].channel(shift)
                };
                if source > 0 {
                    *l = source;
                    queue.push_back(v);
                }
            }
            while let Some(v) = queue.pop_front() {
                let (y, x, z) = (v / (sx * sz), v / sz % sx, v % sz);
                let mut neighbors = [None; 6];
                if x > 0 {
                    neighbors[0] = Some(v - sz);
                }
                if x + 1 < sx {
                    neighbors[1] = Some(v + sz);
                }
                if y > 0 {
                    neighbors[2] = Some(v - sx * sz);
                }
                if y + 1 < sy {
                    neighbors[3] = Some(v + sx * sz);
                }
                if z > 0 {
                    neighbors[4] = Some(v - 1);
                }
                if z + 1 < sz {
                    neighbors[5] = Some(v + 1);
                }
                for n in neighbors.into_iter().flatten() {
                    let opacity = self.opacity[n];
                    if opacity >= 15 {
                        continue;
                    }
                    let mut value = level[v].saturating_sub(opacity.max(1));
                    if let Some(k) = k {
                        value = transmit(u16::from(value), self.filter[n][k]) as u8;
                    }
                    if value > level[n] {
                        level[n] = value;
                        queue.push_back(n);
                    }
                }
            }
            for (o, &l) in out.iter_mut().zip(&level) {
                *o |= u16::from(l) << shift;
            }
        }
        out.into_iter().map(LightValue).collect()
    }

    /// The sky's red, green and blue of every voxel with colored sky (ADR 0064): each channel
    /// filtered as a block channel. Direct sky is 15 per channel above the top of its column
    /// and in the beams, filtered by every filter above it in the column (or in the beam since
    /// its source), with no cost, and mixed by max; a direct voxel below 15 stays open to its
    /// neighbors. The uncolored sky, what gameplay reads, is [`OracleWorld::solve`]'s.
    pub fn solve_colored(&self) -> Vec<[u8; 3]> {
        let [sx, sy, sz] = self.size();
        let y0 = self.min.origin().y;
        let volume = sx * sy * sz;
        let mut direct = vec![[0u8; 3]; volume];
        for x in 0..sx {
            for z in 0..sz {
                let top = self.tops[x * sz + z];
                let mut sky = [MAX_LIGHT; 3];
                for y in (0..sy).rev() {
                    if y0 + y as i64 <= top {
                        break;
                    }
                    let v = (y * sx + x) * sz + z;
                    sky = filter(sky, self.filter[v]);
                    direct[v] = sky;
                }
            }
        }
        self.trace_beams(|c, sky| {
            direct[c] = [0, 1, 2].map(|k| direct[c][k].max(sky[k]));
        });
        let mut out = vec![[0u8; 3]; volume];
        for k in 0..3 {
            let mut level: Vec<u8> = direct.iter().map(|d| d[k]).collect();
            let mut queue: VecDeque<usize> = (0..volume).filter(|&v| level[v] > 0).collect();
            while let Some(v) = queue.pop_front() {
                for n in self.neighbors(v) {
                    let opacity = self.opacity[n];
                    if opacity >= 15 {
                        continue;
                    }
                    let lower = level[v].saturating_sub(opacity.max(1));
                    let value = transmit(u16::from(lower), self.filter[n][k]) as u8;
                    if value > level[n] {
                        level[n] = value;
                        queue.push_back(n);
                    }
                }
            }
            for (o, &l) in out.iter_mut().zip(&level) {
                o[k] = l;
            }
        }
        out
    }

    /// The face neighbors of voxel `v` inside the world.
    fn neighbors(&self, v: usize) -> impl Iterator<Item = usize> {
        let [sx, sy, sz] = self.size();
        let (y, x, z) = (v / (sx * sz), v / sz % sx, v % sz);
        [
            (x > 0).then(|| v - sz),
            (x + 1 < sx).then(|| v + sz),
            (y > 0).then(|| v - sx * sz),
            (y + 1 < sy).then(|| v + sx * sz),
            (z > 0).then(|| v - 1),
            (z + 1 < sz).then(|| v + 1),
        ]
        .into_iter()
        .flatten()
    }

    /// The padded part (local `-1..=32`, in [`PaddedLight::index`](crate::PaddedLight::index)
    /// order) of `chunk` from a [`OracleWorld::solve`] or [`OracleWorld::solve_colored`] result.
    /// The chunk and its border must be inside the world.
    pub fn padded<T: Copy>(&self, solution: &[T], chunk: ChunkPos) -> Vec<T> {
        let [sx, _, sz] = self.size();
        let base = |c: i64, m: i64| (c - m) * i64::from(CHUNK_SIZE);
        let (bx, by, bz) = (
            base(chunk.x, self.min.x),
            base(chunk.y, self.min.y),
            base(chunk.z, self.min.z),
        );
        let mut out = Vec::with_capacity(crate::PADDED_VOLUME);
        for y in -1..=32i64 {
            for x in -1..=32i64 {
                for z in -1..=32i64 {
                    let (wx, wy, wz) = ((bx + x) as usize, (by + y) as usize, (bz + z) as usize);
                    out.push(solution[(wy * sx + wx) * sz + wz]);
                }
            }
        }
        out
    }
}

/// `sky` after `filter`, per channel, with no cost.
fn filter(sky: [u8; 3], filter: [u8; 3]) -> [u8; 3] {
    [0, 1, 2].map(|k| transmit(u16::from(sky[k]), filter[k]) as u8)
}
