//! The reference the tests compare [`compute`](crate::compute) against: a scalar global light
//! solver over an explicit voxel world, one channel at a time, with a plain FIFO queue (no
//! buckets, no masks, no box), and sky tops found by brute force.
//!
//! Only with the `oracle` feature (and in this crate's unit tests).
use boxel_voxel::{BlockState, CHUNK_SIZE, ChunkData, ChunkPos, LightTables, LightValue, LocalPos};
use std::collections::VecDeque;

/// An explicit world: `dims` chunks per axis from chunk `min`, with each voxel's opacity and
/// emission, and the sky top of each column.
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
    /// Sky top per column (absolute y; `i64::MIN` for open forever).
    pub tops: Vec<i64>,
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
            }
        }
        Self {
            min,
            dims,
            opacity,
            emission,
            tops,
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

    /// The global light of every voxel.
    pub fn solve(&self) -> Vec<LightValue> {
        let [sx, sy, sz] = self.size();
        let y0 = self.min.origin().y;
        let volume = sx * sy * sz;
        let mut out = vec![0u16; volume];
        // Direct sky: above the top of the column.
        let direct = |v: usize| {
            let (y, x, z) = (v / (sx * sz), v / sz % sx, v % sz);
            y0 + y as i64 > self.tops[x * sz + z]
        };
        for shift in [
            LightValue::SKY_SHIFT,
            LightValue::RED_SHIFT,
            LightValue::GREEN_SHIFT,
            LightValue::BLUE_SHIFT,
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
                    let value = level[v].saturating_sub(opacity.max(1));
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

    /// The padded light (local `-1..=32`, in [`PaddedLight::index`](crate::PaddedLight::index)
    /// order) of `chunk` from a [`OracleWorld::solve`] result. The chunk and its border must be
    /// inside the world.
    pub fn padded(&self, solution: &[LightValue], chunk: ChunkPos) -> Vec<LightValue> {
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
