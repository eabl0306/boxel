//! Full-detail coverage (ADR 0031): which lod-0 chunks the main renderer draws, as 8 bits per
//! lod-1 node position (one bit per octant: a lod-1 octant is exactly one chunk).
//!
//! - `published`: the chunk's last `ChunkMeshEvent` was an `Upsert` and the main renderer does
//!   not hold it as refused.
//! - `covered`: published, or the chunk draws nothing at full detail either (an `Empty` mesh, or
//!   data that is all air or `Missing`), so the LOD must not draw its coarse rounding there.
//!
//! A lod-1 node hides the octants whose chunk is covered, and the bits go up the levels through
//! the node tree (a node whose 8 children are covered is covered). [`LodCoverage`] is the
//! resource; whoever changes it ([`LodCoverage::set`], [`LodCoverage::replace`]) records the lod-1
//! positions it touched, and the LOD refreshes their masks in its next `Coverage` step.
//! [`ChunkCoverage::from_map`] computes it from scratch from a [`ChunkMap`] (the reference the
//! incremental feed is checked against, and what tools use on a settled world).
use crate::{ChunkMap, DataState, MeshState};
use boxel_ecs::prelude::*;
use boxel_voxel::{BlockState, ChunkPos};
use foldhash::{HashMap, HashSet};

/// The octant bits of one lod-1 node position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Octants {
    /// Chunks the LOD must not draw over (published, empty or air).
    pub covered: u8,
    /// Chunks the main renderer draws.
    pub published: u8,
}

/// The lod-1 position of chunk `c` and its octant bit there.
pub fn octant_of_chunk(c: ChunkPos) -> (ChunkPos, u8) {
    let p = ChunkPos::new(c.x >> 1, c.y >> 1, c.z >> 1);
    let o = (c.x & 1) | ((c.y & 1) << 1) | ((c.z & 1) << 2);
    (p, 1 << o)
}

/// Lod-0 coverage per lod-1 position (module docs). Positions with no bit set are not stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkCoverage {
    bits: HashMap<ChunkPos, Octants>,
}

impl ChunkCoverage {
    /// The bits of lod-1 position `p` (zero when nothing there is covered).
    pub fn get(&self, p: ChunkPos) -> Octants {
        self.bits.get(&p).copied().unwrap_or_default()
    }

    /// Sets chunk `c`'s bits; returns true if they changed.
    pub fn set(&mut self, c: ChunkPos, covered: bool, published: bool) -> bool {
        let (p, bit) = octant_of_chunk(c);
        let mut o = self.get(p);
        let before = o;
        let put = |v: &mut u8, on: bool| {
            if on {
                *v |= bit;
            } else {
                *v &= !bit;
            }
        };
        // A published chunk is drawn, so it is covered too.
        put(&mut o.covered, covered || published);
        put(&mut o.published, published);
        if o == before {
            return false;
        }
        if o == Octants::default() {
            self.bits.remove(&p);
        } else {
            self.bits.insert(p, o);
        }
        true
    }

    /// Lod-1 positions with a bit set.
    pub fn positions(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.bits.keys().copied()
    }

    /// Lod-1 positions with a bit set.
    pub fn len(&self) -> usize {
        self.bits.len()
    }

    /// No bit set.
    pub fn is_empty(&self) -> bool {
        self.bits.is_empty()
    }

    /// The coverage of every chunk of `map` from scratch: published = a `Ready` mesh; covered =
    /// published, an `Empty` mesh, or data that is uniform air or `Missing`.
    pub fn from_map(map: &ChunkMap) -> Self {
        let mut out = Self::default();
        for (_, slot) in map.iter() {
            let published = matches!(slot.mesh(), MeshState::Ready { .. });
            let empty = matches!(slot.mesh(), MeshState::Empty)
                || matches!(slot.data(), DataState::Missing)
                || matches!(slot.data(), DataState::Ready(d) if d.as_uniform() == Some(BlockState::AIR));
            if published || empty {
                out.set(slot.pos, empty, published);
            }
        }
        out
    }
}

/// The LOD's view of the full detail (module docs), and the lod-1 positions changed since the
/// LOD last read it.
#[derive(Resource, Clone, Debug, Default)]
pub struct LodCoverage {
    coverage: ChunkCoverage,
    changed: HashSet<ChunkPos>,
}

impl LodCoverage {
    /// The coverage.
    pub fn coverage(&self) -> &ChunkCoverage {
        &self.coverage
    }

    /// Sets chunk `c`'s bits ([`ChunkCoverage::set`]) and records its lod-1 position if they
    /// changed.
    pub fn set(&mut self, c: ChunkPos, covered: bool, published: bool) {
        if self.coverage.set(c, covered, published) {
            self.changed.insert(octant_of_chunk(c).0);
        }
    }

    /// Replaces the whole coverage (a resync, or a tool) and records every position of the old
    /// and the new one.
    pub fn replace(&mut self, coverage: ChunkCoverage) {
        self.changed.extend(self.coverage.positions());
        self.changed.extend(coverage.positions());
        self.coverage = coverage;
    }

    /// The lod-1 positions changed since the last call.
    pub(crate) fn take_changed(&mut self) -> HashSet<ChunkPos> {
        std::mem::take(&mut self.changed)
    }
}
