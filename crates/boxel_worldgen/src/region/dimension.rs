//! [`DimensionGenerator`]: a dimension's realm stack, dispatching each chunk to its realm.

use super::REGION_LINE;
use crate::WorldGenerator;
use crate::lod::{LodColumn, LodNode, LodRequest, LodScratch, LodSource};
use crate::query::{ColumnInfo, WorldQuery};
use crate::realm::{DEFAULT_REALM, RealmStack};
use crate::registry::{
    DigestBuilder, DigestEntry, DigestScope, GenId, RegistryDigest, hash_params,
};
use boxel_voxel::{BlockRegistry, ChunkData, ChunkKey};
use std::borrow::Cow;
use std::ops::RangeInclusive;

/// The registry name of the digest line a realm gets when its generator brings no line of its
/// own ([`DimensionGenerator::new`]).
pub const REALM_GENERATOR: &str = "realm_generator";

/// The generator of a dimension (ADR 0039 sec. 1 and 2): its [`RealmStack`] and one generator
/// per realm (a [`RegionGenerator`](super::RegionGenerator), or any other).
///
/// [`WorldGenerator::generate`] finds the realm of the chunk (`realm_of` of its lowest voxel: the
/// planes are multiples of 4096 blocks, so a chunk is in one realm) and calls that realm's
/// generator: one virtual call per chunk. [`WorldGenerator::vertical_chunks`] is the union of
/// the realms' ranges, each inside its span. The far LOD is the drawn realm's source, reporting
/// the dimension's version.
pub struct DimensionGenerator {
    dimension: GenId,
    stack: RealmStack,
    realms: Box<[Box<dyn WorldGenerator>]>,
    digest: RegistryDigest,
    vertical: RangeInclusive<i64>,
}

impl DimensionGenerator {
    /// The generator of `dimension`: `realms[i]` generates realm `i` of `stack` (bottom first,
    /// [`RealmStack::iter`]'s order). `digest` holds the dimension's lines (each realm's, from
    /// its generator, and the dimension's own): the generator's version is the low 32 bits of
    /// its sub-digest of `dimension` ([`RegistryDigest::dimension`]).
    ///
    /// A realm without a region generator's lines in `digest` (its `region` line: its generator
    /// is another kind) gets one line for its generator: `realm_generator`, under the realm's
    /// id, with the generator's [`WorldGenerator::version`] and the hash of its
    /// [`WorldGenerator::name`]. So changing that generator changes the dimension's version and
    /// shows in the save record's diff.
    ///
    /// # Panics
    ///
    /// If `realms` and `stack` differ in length, or if a generator that reports one realm of its
    /// own (a [`RegionGenerator`](super::RegionGenerator)) sits in another realm's place.
    pub fn new(
        dimension: GenId,
        stack: RealmStack,
        realms: Vec<Box<dyn WorldGenerator>>,
        digest: &RegistryDigest,
    ) -> Self {
        assert_eq!(
            realms.len(),
            stack.len(),
            "DimensionGenerator: one generator per realm"
        );
        for (ix, generator) in realms.iter().enumerate() {
            // A generator without realms reports the default one: it fits any place.
            let own = generator.realms();
            let id = own.id(0);
            assert!(
                own.len() != 1 || id == stack.id(ix) || id.as_str() == DEFAULT_REALM,
                "DimensionGenerator: realm {ix} of the stack is {}, its generator's is {id} (the \
                 generators go bottom first, in the stack's order)",
                stack.id(ix),
            );
        }
        let vertical = realms
            .iter()
            .zip(stack.iter())
            .map(|(g, (_, spec))| {
                let (r, s) = (g.vertical_chunks(), spec.span.chunk_layers());
                (*r.start()).max(*s.start())..=(*r.end()).min(*s.end())
            })
            .filter(|r| !r.is_empty())
            .reduce(|a, b| (*a.start()).min(*b.start())..=(*a.end()).max(*b.end()))
            .unwrap_or(RangeInclusive::new(1, 0));
        let mut lines = DigestBuilder::new();
        lines.extend(digest);
        for ((realm, _), generator) in stack.iter().zip(&realms) {
            let scope = DigestScope::of_realm(&dimension, realm);
            if !digest
                .entries()
                .iter()
                .any(|e| e.scope == scope && e.registry == REGION_LINE)
            {
                lines.push(DigestEntry {
                    scope,
                    registry: Cow::Borrowed(REALM_GENERATOR),
                    id: realm.clone(),
                    version: generator.version(),
                    params: hash_params(generator.name()),
                });
            }
        }
        Self {
            digest: lines.finish().dimension(&dimension),
            dimension,
            stack,
            realms: realms.into(),
            vertical,
        }
    }

    /// The generator of realm `ix`. Panics if out of range.
    pub fn realm_generator(&self, ix: usize) -> &dyn WorldGenerator {
        &*self.realms[ix]
    }

    /// The dimension's digest: the lines it was built with, and a `realm_generator` line for
    /// every realm whose generator brought none.
    pub fn digest(&self) -> &RegistryDigest {
        &self.digest
    }

    /// The query side of realm `ix`'s generator.
    fn query(&self, ix: usize) -> Option<&dyn WorldQuery> {
        self.realms[ix].as_query()
    }
}

impl WorldGenerator for DimensionGenerator {
    #[inline]
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let realm = self.stack.realm_of(key.pos.origin().y);
        self.realms[realm].generate(key, registry)
    }

    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        self.vertical.clone()
    }

    /// The drawn realm's source, if it has one, reporting the dimension's version.
    fn lod(&self) -> Option<&dyn LodSource> {
        self.realms[self.stack.drawn()]
            .lod()
            .map(|_| self as &dyn LodSource)
    }

    fn name(&self) -> &str {
        self.dimension.as_str()
    }

    fn version(&self) -> u32 {
        self.digest.version()
    }

    fn realms(&self) -> &RealmStack {
        &self.stack
    }

    fn as_query(&self) -> Option<&dyn WorldQuery> {
        Some(self)
    }
}

/// The drawn realm's source. It draws that realm's band; clipping the band to the realm's span
/// comes with the region pass of the far LOD (WG-REGIONLOD).
impl LodSource for DimensionGenerator {
    fn column(
        &self,
        req: &LodRequest,
        registry: &BlockRegistry,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn {
        self.realms[self.stack.drawn()]
            .lod()
            .expect("lod() answers only when the drawn realm has a source")
            .column(req, registry, scratch, emit)
    }

    fn version(&self) -> u32 {
        WorldGenerator::version(self)
    }
}

impl WorldQuery for DimensionGenerator {
    fn height_at(&self, x: i64, z: i64) -> Option<i64> {
        self.query(self.stack.drawn())?.height_at(x, z)
    }

    fn realm_at(&self, y: i64) -> &GenId {
        self.stack.id(self.stack.realm_of(y))
    }

    fn biome_at(&self, x: i64, y: i64, z: i64) -> Option<GenId> {
        self.query(self.stack.realm_of(y))?.biome_at(x, y, z)
    }

    fn column(&self, x: i64, z: i64) -> Option<ColumnInfo> {
        self.query(self.stack.drawn())?.column(x, z)
    }

    fn environment_at(&self, _x: i64, y: i64, _z: i64) -> &GenId {
        &self.stack.spec(self.stack.realm_of(y)).env
    }
}
