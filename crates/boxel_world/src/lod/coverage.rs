//! Full-detail coverage (ADR 0031): which lod-0 chunks the main renderer draws, as 8 bits per
//! lod-1 node position (one bit per octant: a lod-1 octant is exactly one chunk).
//!
//! - `published`: the chunk's last `ChunkMeshEvent` was an `Upsert` (written in `Collect` or
//!   between frames: the next `Update` reads both), and the main renderer does not hold it as
//!   refused ([`LodFeedback::chunks`](super::LodFeedback::chunks)).
//! - `covered`: published, or the chunk draws nothing at full detail either (an `Empty` mesh, or
//!   data that is all air or `Missing`), so the LOD must not draw its coarse rounding there.
//!   Never [`ChunkSlot::shown`](crate::ChunkSlot::shown): it goes false after an edit while the
//!   old mesh is still drawn, and the LOD would flash over the player's own builds.
//!
//! The LOD's `Coverage` step feeds it every frame, in full ([`LodCoverage`]): the mesh events,
//! the data events, the renderer's resyncs and refusals. An `Empty` mesh sends no event, so a
//! watch list of uncovered loaded chunks that lie in octants where a lod-1 node has quads is
//! polled, a bounded number per frame, round robin. Whatever changes records its lod-1 position,
//! and the LOD refreshes the masks there (the node tree takes the bits up the levels).
//! [`ChunkCoverage::from_map`] computes it from scratch from a [`ChunkMap`] (the reference the
//! feed is checked against).
use crate::{ChunkMap, ChunkSlot, DataState, MeshState};
use boxel_ecs::prelude::*;
use boxel_voxel::{BlockState, ChunkPos};
use foldhash::{HashMap, HashSet};

/// Watched chunks polled per frame at most. A quarter of the list goes per frame (at least 64),
/// so each one is looked at every 4 frames: the chunks without a mesh yet (the data ring around
/// the viewers) wait there for long.
pub const POLL_PER_FRAME: usize = 1024;

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

/// The chunk in octant `o` (`x | y << 1 | z << 2`) of lod-1 position `p`.
pub fn chunk_of_octant(p: ChunkPos, o: u8) -> ChunkPos {
    ChunkPos::new(
        p.x * 2 + i64::from(o & 1),
        p.y * 2 + i64::from((o >> 1) & 1),
        p.z * 2 + i64::from(o >> 2),
    )
}

/// What is known of the chunks of one lod-1 position, a bit per octant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Bits {
    /// The last mesh event was an `Upsert`.
    upserted: u8,
    /// Nothing to draw: an `Empty` mesh, all air or `Missing`.
    empty: u8,
    /// The main renderer refused the chunk's last upload.
    refused: u8,
}

impl Bits {
    fn octants(self) -> Octants {
        let published = self.upserted & !self.refused;
        Octants {
            covered: published | self.empty,
            published,
        }
    }
}

/// Sets or clears `bit` in `v`.
fn put(v: &mut u8, bit: u8, on: bool) {
    if on {
        *v |= bit;
    } else {
        *v &= !bit;
    }
}

/// True if chunk slot `slot` draws nothing at full detail: an `Empty` mesh, or data that is all
/// air or `Missing`.
fn draws_nothing(slot: &ChunkSlot) -> bool {
    matches!(slot.mesh(), MeshState::Empty)
        || matches!(slot.data(), DataState::Missing)
        || matches!(slot.data(), DataState::Ready(d) if d.as_uniform() == Some(BlockState::AIR))
}

/// The slot of chunk `c`, if loaded.
fn slot(map: &ChunkMap, c: ChunkPos) -> Option<&ChunkSlot> {
    map.get(map.id_at(c)?)
}

/// Lod-0 coverage per lod-1 position (module docs). Positions with no bit set are not stored.
#[derive(Clone, Debug, Default)]
pub struct ChunkCoverage {
    bits: HashMap<ChunkPos, Bits>,
    /// Published and covered chunks, kept as bits change.
    published: usize,
    covered: usize,
}

impl ChunkCoverage {
    /// The bits of lod-1 position `p` (zero when nothing there is covered).
    pub fn get(&self, p: ChunkPos) -> Octants {
        self.bits.get(&p).copied().unwrap_or_default().octants()
    }

    /// Changes the bits of lod-1 position `p` through `f`; returns true if its octants changed.
    fn update_at(&mut self, p: ChunkPos, f: impl FnOnce(&mut Bits)) -> bool {
        let old = self.bits.get(&p).copied().unwrap_or_default();
        let mut b = old;
        f(&mut b);
        if b == old {
            return false;
        }
        let (before, after) = (old.octants(), b.octants());
        let n = |v: u8| v.count_ones() as usize;
        self.published = self.published + n(after.published) - n(before.published);
        self.covered = self.covered + n(after.covered) - n(before.covered);
        if b == Bits::default() {
            self.bits.remove(&p);
        } else {
            self.bits.insert(p, b);
        }
        after != before
    }

    /// Changes the bits of chunk `c` through `f` (its bit given); returns true if its octants
    /// changed.
    fn update(&mut self, c: ChunkPos, f: impl FnOnce(&mut Bits, u8)) -> bool {
        let (p, bit) = octant_of_chunk(c);
        self.update_at(p, |b| f(b, bit))
    }

    /// Sets chunk `c` as published (or not) and as drawing nothing (`covered` and not
    /// published), with no refusal; returns true if its octants changed.
    pub fn set(&mut self, c: ChunkPos, covered: bool, published: bool) -> bool {
        self.update(c, |b, bit| {
            put(&mut b.upserted, bit, published);
            put(&mut b.empty, bit, covered && !published);
            put(&mut b.refused, bit, false);
        })
    }

    /// True if chunk `c` is covered.
    pub fn covered(&self, c: ChunkPos) -> bool {
        let (p, bit) = octant_of_chunk(c);
        self.get(p).covered & bit != 0
    }

    /// True if chunk `c` is published.
    pub fn published(&self, c: ChunkPos) -> bool {
        let (p, bit) = octant_of_chunk(c);
        self.get(p).published & bit != 0
    }

    /// Chunks published.
    pub fn published_chunks(&self) -> usize {
        self.published
    }

    /// Chunks covered (published ones included).
    pub fn covered_chunks(&self) -> usize {
        self.covered
    }

    /// Lod-1 positions with something known (a bit set, maybe none covered: a refusal).
    pub fn positions(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.bits.keys().copied()
    }

    /// Lod-1 positions with something known.
    pub fn len(&self) -> usize {
        self.bits.len()
    }

    /// Nothing known.
    pub fn is_empty(&self) -> bool {
        self.bits.is_empty()
    }

    /// The coverage of every chunk of `map` from scratch: published = a `Ready` mesh; covered =
    /// published, an `Empty` mesh, or data that is uniform air or `Missing`.
    pub fn from_map(map: &ChunkMap) -> Self {
        let mut out = Self::default();
        for (_, slot) in map.iter() {
            let published = matches!(slot.mesh(), MeshState::Ready { .. });
            let empty = draws_nothing(slot);
            if published || empty {
                out.set(slot.pos, empty, published);
            }
        }
        out
    }
}

/// The LOD's view of the full detail (module docs): the coverage, the lod-1 positions changed
/// since the LOD last read them, and the watch list of chunks polled for an `Empty` mesh.
#[derive(Resource, Clone, Debug, Default)]
pub struct LodCoverage {
    coverage: ChunkCoverage,
    changed: HashSet<ChunkPos>,
    /// Watched chunks, polled round robin from `cursor`.
    watch: Vec<ChunkPos>,
    watched: HashSet<ChunkPos>,
    cursor: usize,
}

impl LodCoverage {
    /// The coverage.
    pub fn coverage(&self) -> &ChunkCoverage {
        &self.coverage
    }

    /// Chunks waiting on the watch list.
    pub fn watched(&self) -> usize {
        self.watch.len()
    }

    /// The watch list.
    #[cfg(test)]
    pub(crate) fn watch_list(&self) -> &[ChunkPos] {
        &self.watch
    }

    /// Changes chunk `c` through `f` and records its lod-1 position if its octants changed.
    fn update(&mut self, c: ChunkPos, f: impl FnOnce(&mut Bits, u8)) {
        if self.coverage.update(c, f) {
            self.changed.insert(octant_of_chunk(c).0);
        }
    }

    /// Sets chunk `c`'s bits by hand ([`ChunkCoverage::set`]; tests).
    #[cfg(test)]
    pub(crate) fn set(&mut self, c: ChunkPos, covered: bool, published: bool) {
        if self.coverage.set(c, covered, published) {
            self.changed.insert(octant_of_chunk(c).0);
        }
    }

    /// An `Upsert` of chunk `c`.
    pub(crate) fn upserted(&mut self, c: ChunkPos) {
        self.update(c, |b, bit| put(&mut b.upserted, bit, true));
    }

    /// A `Remove` of chunk `c`: no longer published; it may draw nothing now (an `Empty` mesh).
    pub(crate) fn removed(&mut self, c: ChunkPos, map: &ChunkMap) {
        let empty = slot(map, c).is_some_and(draws_nothing);
        self.update(c, |b, bit| {
            put(&mut b.upserted, bit, false);
            put(&mut b.empty, bit, empty);
        });
    }

    /// Chunk `c` loaded (`loaded`) or unloaded: whether it draws nothing, from its slot as it
    /// is now (so the order of the data events against the mesh events does not matter). A
    /// loaded chunk that may still mesh to nothing is watched if `quads` says its lod-1 node has
    /// quads in its octant.
    pub(crate) fn data_changed(
        &mut self,
        c: ChunkPos,
        loaded: bool,
        map: &ChunkMap,
        quads: impl Fn(ChunkPos) -> u8,
    ) {
        let (p, bit) = octant_of_chunk(c);
        let b = self.coverage.bits.get(&p).copied().unwrap_or_default();
        if !loaded && b.empty & bit == 0 {
            // Nothing to clear; if it loaded again since, its `Loaded` event follows.
            return;
        }
        let Some(slot) = slot(map, c) else {
            if b.empty & bit != 0 {
                self.update(c, |b, bit| put(&mut b.empty, bit, false));
            }
            return;
        };
        let empty = draws_nothing(slot);
        if (b.empty & bit != 0) != empty {
            self.update(c, |b, bit| put(&mut b.empty, bit, empty));
        }
        if !empty && b.octants().covered & bit == 0 && quads(p) & bit != 0 {
            self.watch(c);
        }
    }

    /// The main renderer refused chunk `c`'s upload (`true`) or shows what the world sent it
    /// again (`false`).
    pub(crate) fn refused(&mut self, c: ChunkPos, refused: bool) {
        self.update(c, |b, bit| put(&mut b.refused, bit, refused));
    }

    /// The main renderer dropped every chunk and uploaded [`ChunkMap::ready_meshes`] again
    /// (`ChunkMeshResync`), after the mesh events of this frame were applied. Published from now
    /// on: the `Ready` meshes, and the published chunks whose remesh started after that upload
    /// (they keep [`ChunkSlot::shown`]; the world forgot those whose remesh was under way then,
    /// and they lost it). No refusal is left: a resynced renderer starts without them.
    pub(crate) fn renderer_resynced(&mut self, map: &ChunkMap) {
        let mut upserted: HashMap<ChunkPos, u8> = HashMap::default();
        for (_, key, _) in map.ready_meshes() {
            let (p, bit) = octant_of_chunk(key.pos);
            *upserted.entry(p).or_insert(0) |= bit;
        }
        for (&p, b) in &self.coverage.bits {
            for o in 0..8 {
                let held = b.upserted & (1 << o) != 0
                    && slot(map, chunk_of_octant(p, o)).is_some_and(|s| {
                        s.shown()
                            && matches!(s.mesh(), MeshState::Queued | MeshState::InFlight { .. })
                    });
                if held {
                    *upserted.entry(p).or_insert(0) |= 1 << o;
                }
            }
        }
        let positions: HashSet<ChunkPos> = self
            .coverage
            .bits
            .keys()
            .chain(upserted.keys())
            .copied()
            .collect();
        for p in positions {
            let now = upserted.get(&p).copied().unwrap_or(0);
            if self.coverage.update_at(p, |b| {
                b.upserted = now;
                b.refused = 0;
            }) {
                self.changed.insert(p);
            }
        }
    }

    /// Watches chunk `c` (polled until it is covered, unloads, or its octant has no LOD quads).
    pub(crate) fn watch(&mut self, c: ChunkPos) {
        if self.watched.insert(c) {
            self.watch.push(c);
        }
    }

    /// A lod-1 node at `p` got quads in `octants`: its loaded uncovered chunks there are watched.
    pub(crate) fn lod1_quads(&mut self, p: ChunkPos, octants: u8, map: &ChunkMap) {
        let covered = self.coverage.get(p).covered;
        for o in 0..8 {
            let bit = 1u8 << o;
            if octants & bit == 0 || covered & bit != 0 {
                continue;
            }
            let c = chunk_of_octant(p, o);
            if map.id_at(c).is_some() {
                self.watch(c);
            }
        }
    }

    /// Polls a quarter of the watched chunks (at least 64, at most `max`), round robin: one that
    /// draws nothing now is covered; one that is covered, unloaded, or whose octant has no LOD
    /// quads (`quads`) leaves the list.
    pub(crate) fn poll(&mut self, map: &ChunkMap, max: usize, quads: impl Fn(ChunkPos) -> u8) {
        let n = self.watch.len().div_ceil(4).max(64).min(max);
        for _ in 0..n.min(self.watch.len()) {
            if self.cursor >= self.watch.len() {
                self.cursor = 0;
            }
            let c = self.watch[self.cursor];
            let (p, bit) = octant_of_chunk(c);
            let keep = if self.coverage.covered(c) || quads(p) & bit == 0 {
                false
            } else {
                match slot(map, c) {
                    None => false,
                    Some(s) if draws_nothing(s) => {
                        self.update(c, |b, bit| put(&mut b.empty, bit, true));
                        false
                    }
                    Some(_) => true,
                }
            };
            if keep {
                self.cursor += 1;
            } else {
                self.watched.remove(&c);
                self.watch.swap_remove(self.cursor);
            }
        }
    }

    /// The lod-1 positions changed since the last call.
    pub(crate) fn take_changed(&mut self) -> HashSet<ChunkPos> {
        std::mem::take(&mut self.changed)
    }
}
