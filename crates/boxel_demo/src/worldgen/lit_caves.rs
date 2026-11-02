//! [`LitCaves`]: any world with its caves lit by sky sources (ADR 0061), the lit cavern of the
//! demos and the `lit-cavern` perf case.

use crate::blocks::SKY_SOURCE;
use boxel_voxel::{BlockRegistry, ChunkData, ChunkKey, LocalPos};
use boxel_worldgen::{LodSource, RealmStack, WorldGenerator, WorldQuery};
use std::ops::RangeInclusive;
use std::sync::Arc;

/// `inner` with a sky source (the default state of [`SKY_SOURCE`]: down, range 64) in the first
/// air cell under every roof of the columns with `x + z` even, in the chunk layers `layers`. A
/// roof is an opaque cube over air inside one chunk (a roof on a chunk's bottom row is skipped).
/// The generator declares `layers` as its [`WorldGenerator::sky_source_layers`], so the world
/// builds the beams of every column within reach of them.
///
/// It is what a game's cave step would do, at the density of the plan's "every column or every
/// other one". The registry must hold [`SKY_SOURCE`]
/// ([`registry_with_sky_source`](crate::blocks::registry_with_sky_source)); without it the
/// chunks are `inner`'s. Everything else (the LOD, the realms, the queries) is `inner`'s: the
/// sources are invisible and only change the light.
pub struct LitCaves {
    /// The world whose caves are lit.
    pub inner: Arc<dyn WorldGenerator>,
    /// The chunk layers that get sources.
    pub layers: RangeInclusive<i64>,
}

impl LitCaves {
    /// `inner` with sources in the chunk layers within `around` layers of `layer` (a viewer's
    /// chunk y).
    pub fn around(inner: Arc<dyn WorldGenerator>, layer: i64, around: i64) -> Self {
        Self {
            inner,
            layers: layer.saturating_sub(around)..=layer.saturating_add(around),
        }
    }
}

impl WorldGenerator for LitCaves {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let mut chunk = self.inner.generate(key, registry);
        if !self.layers.contains(&key.pos.y) || chunk.as_uniform().is_some() {
            return chunk;
        }
        let Some(source) = registry.by_name(SKY_SOURCE) else {
            return chunk;
        };
        let o = key.pos.origin();
        let parity = (o.x + o.z).rem_euclid(2) as u32;
        for x in 0..32u32 {
            for z in 0..32u32 {
                // The world column's `x + z` is even.
                if !(x + z + parity).is_multiple_of(2) {
                    continue;
                }
                for y in (0..31u32).rev() {
                    let (here, above) = (LocalPos::new(x, y, z), LocalPos::new(x, y + 1, z));
                    if chunk.get(here).is_air() && registry.occludes(chunk.get(above)) {
                        chunk.set(here, source);
                    }
                }
            }
        }
        chunk
    }
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        self.inner.vertical_chunks()
    }
    fn sky_source_layers(&self) -> RangeInclusive<i64> {
        self.layers.clone()
    }
    fn lod(&self) -> Option<&dyn LodSource> {
        self.inner.lod()
    }
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn version(&self) -> u32 {
        self.inner.version()
    }
    fn realms(&self) -> &RealmStack {
        self.inner.realms()
    }
    fn as_query(&self) -> Option<&dyn WorldQuery> {
        self.inner.as_query()
    }
}
