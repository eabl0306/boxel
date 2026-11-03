//! Beam tiles of the loaded chunk columns (ADR 0061).
//!
//! Every [`SkyColumn`](crate::sky::SkyColumn) holds a [`SourceColumn`], with the column's
//! lifecycle: the loaded layers that may hold a sky source, and the column's [`BeamTile`] once a
//! mesh needed one. A tile is exact for a window of rows and is built by a
//! [`JobKind::BeamTile`](crate::scheduler::JobKind) job ([`BeamJob`]) from the column's content:
//! loaded snapshots, saved chunks and private generations, never from what happens to be loaded,
//! so a chunk that arrives never relights anything.
//!
//! **The gate.** A mesh needs the tile of one of its 9 columns only if a layer of that column
//! within [`GATE_LAYERS`] of its own may hold a source: the generator's band
//! (`WorldGenerator::sky_source_layers`), a loaded chunk whose palette holds a source
//! ([`SourceColumn::layers`]), or a saved chunk that is not readable. Otherwise it passes `None`,
//! which gives the same light as a tile with no beam in its box. A registry without sources never
//! looks.
//!
//! **Windows.** A tile covers the box rows (chunk origin `- 16 ..= + 47`) of every mesh that used
//! it. A mesh outside asks for a wider build ([`widen`]) and waits; the tile it has stays usable
//! meanwhile, since its content did not change. Edits retrace beams in place
//! ([`BeamTile::edit`]); one that cannot be traced from loaded data sends the column to a new
//! build, compared with the tile before it ([`changed_band`]) to remesh what changed.
use crate::ChunkId;
use crate::persist::WorldStore;
use boxel_light::beam_tile::MAX_WINDOW;
use boxel_light::{BeamTile, LayerData};
use boxel_voxel::{
    BeamDir, CHUNK_AREA, CHUNK_SHIFT, ChunkData, ChunkKey, ChunkPos, DimensionId, LightTables,
    MAX_BEAM, SkySourceDef,
};
use boxel_worldgen::WorldGenerator;
use std::ops::RangeInclusive;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// Chunk layers on each side of a mesh's layer where a source may light its box: a beam spans
/// at most [`MAX_BEAM`] cells (8 layers), and the box reaches 16 rows past the chunk.
pub(crate) const GATE_LAYERS: i64 = (MAX_BEAM as u64).div_ceil(1 << CHUNK_SHIFT) as i64 + 1;

/// How far a beam reaches past its source, in cells.
pub(crate) const REACH: i64 = MAX_BEAM as i64 - 1;

/// The rows of the light box of a chunk at layer `cy`: the chunk plus 16 rows on each side.
pub(crate) fn box_rows(cy: i64) -> (i64, i64) {
    let y0 = cy << CHUNK_SHIFT;
    (y0 - 16, y0 + 47)
}

/// A window covering `cur` and `add`, or `add` alone when the union is taller than
/// [`MAX_WINDOW`] (viewers thousands of layers apart in one column).
pub(crate) fn widen(cur: Option<(i64, i64)>, add: (i64, i64)) -> (i64, i64) {
    let Some((lo, hi)) = cur else {
        return add;
    };
    let (lo, hi) = (lo.min(add.0), hi.max(add.1));
    if hi - lo < MAX_WINDOW { (lo, hi) } else { add }
}

/// The rows a new source `def` at `y` may change the light of: its longest beam, plus 14.
pub(crate) fn source_rows(def: SkySourceDef, y: i64) -> (i64, i64) {
    let r = i64::from(def.range) - 1;
    let (lo, hi) = match def.dir {
        BeamDir::Down => (y - r, y),
        BeamDir::Up => (y, y + r),
    };
    (lo - 14, hi + 14)
}

/// The beam tile slot of one chunk column ([module docs](self)).
#[derive(Debug, Default)]
pub(crate) struct SourceColumn {
    /// Loaded layers whose snapshot may hold a source (`ChunkData::may_source`), ascending. Set
    /// when the data arrives and on edits that place a source, dropped on unload.
    pub(crate) layers: Vec<i64>,
    /// The tile: exact for its window and the current content. `None` before the first build,
    /// and from an edit it could not retrace (or a lost edit) until the next build lands.
    pub(crate) tile: Option<Arc<BeamTile>>,
    /// The tile before the content change that took `tile` away (or an empty one when nothing
    /// lit the column yet): the next tile is compared with it to remesh what changed.
    pub(crate) old: Option<Arc<BeamTile>>,
    /// Rows the next build covers: `Some` while a build is wanted.
    pub(crate) want: Option<(i64, i64)>,
    /// A build of this version and window is running.
    pub(crate) in_flight: Option<(u32, (i64, i64))>,
    /// Bumped by every change of the column's content that may move a beam (a running build
    /// read the old content: its result is discarded).
    pub(crate) version: u32,
    /// A build job is in the scheduler.
    pub(crate) queued: bool,
    /// Mesh and light slots waiting for this tile (may hold stale or repeated ids).
    pub(crate) waiters: Vec<ChunkId>,
    /// The build waits for this slot's data (a layer it would otherwise generate privately).
    pub(crate) waiting_for: Option<ChunkId>,
    /// The edit batch whose remeshes the next tile's changes join.
    pub(crate) batch: Option<u32>,
    /// A build replaced a window it could not widen: meshes outside the window may hold an
    /// earlier tile, so edits there remesh around themselves.
    pub(crate) replaced: bool,
}

impl SourceColumn {
    /// True if some loaded layer in `lo..=hi` may hold a source.
    pub(crate) fn has_layer_in(&self, lo: i64, hi: i64) -> bool {
        has_in(&self.layers, lo, hi)
    }
    /// Layer `y` is loaded and may hold a source.
    pub(crate) fn add_layer(&mut self, y: i64) {
        if let Err(i) = self.layers.binary_search(&y) {
            self.layers.insert(i, y);
        }
    }
    /// Layer `y` is no longer loaded.
    pub(crate) fn remove_layer(&mut self, y: i64) {
        if let Ok(i) = self.layers.binary_search(&y) {
            self.layers.remove(i);
        }
    }
    /// The window of the tile or of the build that will replace it.
    pub(crate) fn window(&self) -> Option<(i64, i64)> {
        let tile = self.tile.as_ref().map(|t| t.window().into_inner());
        let running = self.in_flight.map(|(_, w)| w);
        [tile, running, self.want]
            .into_iter()
            .flatten()
            .reduce(|a, b| widen(Some(a), b))
    }
    /// Heap bytes of the tile (shared with the meshes that hold it) and the layer list.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.tile.as_ref().map_or(0, |t| t.heap_bytes()) + self.layers.capacity() * size_of::<i64>()
    }
}

/// True if `sorted` (ascending) has a value in `lo..=hi`.
pub(crate) fn has_in(sorted: &[i64], lo: i64, hi: i64) -> bool {
    let i = sorted.partition_point(|&y| y < lo);
    sorted.get(i).is_some_and(|&y| y <= hi)
}

/// The beam cells of local column `col` (`(x << 5) | z`) of `tile` in rows `lo..=hi`, as
/// disjoint intervals, ascending.
fn cells(tile: &BeamTile, col: usize, lo: i64, hi: i64) -> Vec<(i64, i64)> {
    let (x, z) = ((col >> CHUNK_SHIFT) as u32, (col & 31) as u32);
    let mut out: Vec<(i64, i64)> = Vec::new();
    // Beams are sorted by their lowest cell.
    for b in tile.column(x, z) {
        let (a, b) = (b.lo.max(lo), b.hi.min(hi));
        if a > b {
            continue;
        }
        match out.last_mut() {
            Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The voxels whose light a change of beams from `old` to `new` (chunk column `(x, z)`) can
/// reach: the columns whose beam cells differ, inside `new`'s window, +-14 horizontally, from 14
/// under the lowest changed cell to 14 over the highest. Rows of `new`'s window outside `old`'s
/// count as empty in `old`: a mesh there had no tile, so no beam reached it. With colored sky
/// (ADR 0064), a column whose filter cells differ counts every beam cell it has in either
/// tile: a filter tints the cells past it. `None` if nothing changed.
pub(crate) fn changed_band(
    old: &BeamTile,
    new: &BeamTile,
    x: i64,
    z: i64,
) -> Option<[RangeInclusive<i64>; 3]> {
    let (n_lo, n_hi) = new.window().into_inner();
    let (o_lo, o_hi) = old.window().into_inner();
    let (o_lo, o_hi) = (o_lo.max(n_lo), o_hi.min(n_hi));
    let mut band: Option<[(i64, i64); 3]> = None;
    for col in 0..CHUNK_AREA {
        let a = if o_lo <= o_hi {
            cells(old, col, o_lo, o_hi)
        } else {
            Vec::new()
        };
        let b = cells(new, col, n_lo, n_hi);
        let (cx, cz) = ((col >> CHUNK_SHIFT) as u32, (col & 31) as u32);
        let retinted = (old.colored_sky() || new.colored_sky())
            && !old.column_tints(cx, cz).eq(new.column_tints(cx, cz));
        if a == b && !retinted {
            continue;
        }
        // The symmetric difference: the toggles of both, pairs of equal ones cancelled.
        let mut t: Vec<i64> = a.iter().chain(&b).flat_map(|&(l, h)| [l, h + 1]).collect();
        t.sort_unstable();
        let mut odd: Vec<i64> = Vec::with_capacity(t.len());
        for v in t {
            if odd.last() == Some(&v) {
                odd.pop();
            } else {
                odd.push(v);
            }
        }
        if retinted {
            // Every beam cell of either tile in the window.
            for &(l, h) in a.iter().chain(&b) {
                odd.extend([l, h + 1]);
            }
            odd.sort_unstable();
        }
        let (Some(&lo), Some(&end)) = (odd.first(), odd.last()) else {
            continue;
        };
        let wx = (x << CHUNK_SHIFT) + (col >> CHUNK_SHIFT) as i64;
        let wz = (z << CHUNK_SHIFT) + (col & 31) as i64;
        let r = band.get_or_insert([(wx, wx), (lo, end - 1), (wz, wz)]);
        for (axis, (l, h)) in r.iter_mut().zip([(wx, wx), (lo, end - 1), (wz, wz)]) {
            axis.0 = axis.0.min(l);
            axis.1 = axis.1.max(h);
        }
    }
    band.map(|axes| axes.map(|(l, h)| l.saturating_sub(14)..=h.saturating_add(14)))
}

/// Layer `cy` of column `(x, z)` as it is after an edit, for [`BeamTile::edit`]: loaded data, or
/// air outside the generator's range when no saved copy exists (without persistence an unloaded
/// chunk lost its edits). `None` otherwise: a build reads or generates it.
pub(crate) fn edit_layer<'m>(
    map: &'m crate::ChunkMap,
    (x, z): (i64, i64),
    cy: i64,
    saved: &[i64],
    layers: &RangeInclusive<i64>,
) -> Option<LayerData<'m>> {
    if let Some(id) = map.id_at(ChunkPos::new(x, cy, z)) {
        return match map.slots[id].data() {
            crate::DataState::Ready(data) => Some(LayerData::Ref(data)),
            crate::DataState::Missing => Some(LayerData::Air),
            crate::DataState::Queued | crate::DataState::Generating => None,
        };
    }
    if saved.binary_search(&cy).is_ok() || layers.contains(&cy) {
        return None;
    }
    Some(LayerData::Air)
}

/// A beam tile build, captured on the main thread and run on a worker.
pub(crate) struct BeamJob {
    pub(crate) x: i64,
    pub(crate) z: i64,
    pub(crate) dim: DimensionId,
    /// The rows the tile is exact for.
    pub(crate) window: (i64, i64),
    /// The generator's realm planes, ascending.
    pub(crate) planes: Vec<i64>,
    /// The generator's chunk layers: layers outside read as air unless loaded or saved.
    pub(crate) layers: RangeInclusive<i64>,
    /// The loaded layers of the read range, highest first: their data, or `None` for a chunk
    /// whose generation failed (air).
    pub(crate) captured: Vec<(i64, Option<Arc<ChunkData>>)>,
    /// Layers not captured that read as air without being generated: those above `air_above`
    /// and below `air_below`. No source can be there, so whatever they hold changes no beam
    /// cell of the window (only the ends of beams leaving it).
    pub(crate) air_above: i64,
    pub(crate) air_below: i64,
    /// Saved layers of the read range that are not loaded (ADR 0024): read from `store` first,
    /// like `captured` ones.
    pub(crate) saved: Vec<i64>,
    /// The world's store, when `saved` is not empty.
    pub(crate) store: Option<WorldStore>,
    /// Colored sky (ADR 0064): the tile keeps the filter cells of its beams.
    pub(crate) colored: bool,
}

impl BeamJob {
    /// The layers a build of `window` reads: `(w_lo - 255) >> 5 ..= (w_hi + 255) >> 5`.
    pub(crate) fn read_range((lo, hi): (i64, i64)) -> (i64, i64) {
        ((lo - REACH) >> CHUNK_SHIFT, (hi + REACH) >> CHUNK_SHIFT)
    }

    /// Builds the tile: loaded layers from `captured`, saved layers from the store, the rest
    /// generated privately (a generation that panics reads as air, as a `Missing` chunk does)
    /// or air. Returns the tile and how many chunks were generated.
    pub(crate) fn run(
        mut self,
        generator: &dyn WorldGenerator,
        registry: &boxel_voxel::BlockRegistry,
        tables: &LightTables,
    ) -> (BeamTile, u32) {
        let mut generated = 0;
        if let Some(store) = self.store.take() {
            for cy in std::mem::take(&mut self.saved) {
                let pos = ChunkPos::new(self.x, cy, self.z);
                let data = store.read_chunk(self.key(pos)).or_else(|| {
                    generated += 1;
                    self.generate(generator, registry, pos).map(Arc::new)
                });
                self.captured.push((cy, data));
            }
            self.captured
                .sort_unstable_by_key(|(cy, _)| std::cmp::Reverse(*cy));
        }
        let window = self.window.0..=self.window.1;
        let build = if self.colored {
            boxel_light::beam_tile::build_colored
        } else {
            boxel_light::beam_tile::build
        };
        let tile = build(window, &self.planes, tables, |cy| {
            let i = self.captured.partition_point(|(c, _)| *c > cy);
            if let Some((_, data)) = self.captured.get(i).filter(|(c, _)| *c == cy) {
                return data.as_deref().map_or(LayerData::Air, LayerData::Ref);
            }
            if !self.layers.contains(&cy) || cy > self.air_above || cy < self.air_below {
                return LayerData::Air;
            }
            generated += 1;
            self.generate(generator, registry, ChunkPos::new(self.x, cy, self.z))
                .map_or(LayerData::Air, LayerData::Owned)
        });
        (tile, generated)
    }

    fn key(&self, pos: ChunkPos) -> ChunkKey {
        ChunkKey {
            dim: self.dim,
            lod: 0,
            pos,
        }
    }

    /// A private generation of the chunk at `pos` (`None` if the generator panics).
    fn generate(
        &self,
        generator: &dyn WorldGenerator,
        registry: &boxel_voxel::BlockRegistry,
        pos: ChunkPos,
    ) -> Option<ChunkData> {
        let key = self.key(pos);
        catch_unwind(AssertUnwindSafe(|| generator.generate(key, registry))).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_voxel::testing::{SKY_SOURCE_RANGES, add_sky_source};
    use boxel_voxel::{BlockState, LocalPos};

    /// Every source whose longest beam meets a box is within [`GATE_LAYERS`] layers of the
    /// box's chunk, and the bound is tight.
    #[test]
    fn the_gate_covers_every_layer_a_beam_can_light_a_box_from() {
        for cy in [-3, 0, 5] {
            let (lo, hi) = box_rows(cy);
            let mut farthest = 0;
            for y in lo - 400..hi + 400 {
                let down = y >= lo && y - REACH <= hi;
                let up = y <= hi && y + REACH >= lo;
                if down || up {
                    let d = ((y >> CHUNK_SHIFT) - cy).abs();
                    assert!(d <= GATE_LAYERS, "source at {y}, box of layer {cy}");
                    farthest = farthest.max(d);
                }
            }
            assert_eq!(farthest, GATE_LAYERS);
        }
    }

    #[test]
    fn windows_widen_unless_too_tall() {
        assert_eq!(widen(None, (0, 63)), (0, 63));
        assert_eq!(widen(Some((0, 63)), (-100, 10)), (-100, 63));
        assert_eq!(widen(Some((0, 63)), (MAX_WINDOW, MAX_WINDOW + 63)), {
            (MAX_WINDOW, MAX_WINDOW + 63)
        });
        assert_eq!(
            widen(Some((0, 63)), (MAX_WINDOW - 64, MAX_WINDOW - 1)),
            (0, MAX_WINDOW - 1)
        );
    }

    /// Two tiles of a column with one source each: the band holds the changed beam cells,
    /// clipped to the new window, +-14.
    #[test]
    fn changed_bands_hold_the_cells_that_differ() {
        let mut b = boxel_voxel::testing::builder();
        let first = add_sky_source(&mut b).expect("sources").0 - 1;
        let registry = b.build();
        let tables = LightTables::from_registry(&registry);
        let r64 =
            BlockState(first + SKY_SOURCE_RANGES.iter().position(|&r| r == 64).unwrap() as u16);
        let tile = |source: Option<(u32, u32, u32)>, window: (i64, i64)| {
            let mut chunk = ChunkData::uniform(BlockState::AIR);
            if let Some((x, y, z)) = source {
                chunk.set(LocalPos::new(x, y, z), r64);
            }
            boxel_light::beam_tile::build(window.0..=window.1, &[], &tables, |cy| {
                if cy == 2 {
                    LayerData::Ref(&chunk)
                } else {
                    LayerData::Air
                }
            })
        };
        let none = tile(None, (0, 127));
        let one = tile(Some((3, 20, 5)), (0, 127));
        assert_eq!(changed_band(&none, &none, 1, -1), None);
        assert_eq!(changed_band(&one, &one, 1, -1), None);
        // The source at y = 84 lights 21..=84.
        let band = changed_band(&none, &one, 1, -1).expect("changed");
        assert_eq!(
            band,
            [35 - 14..=35 + 14, 21 - 14..=84 + 14, -27 - 14..=-27 + 14]
        );
        assert_eq!(changed_band(&one, &none, 1, -1), Some(band.clone()));
        // A new window above the old one: the old tile counts as empty there.
        let high = tile(Some((3, 20, 5)), (60, 127));
        let low = tile(None, (0, 50));
        let band = changed_band(&low, &high, 1, -1).expect("changed");
        assert_eq!(band[1], 60 - 14..=84 + 14);
        // Inside both windows nothing changed.
        let both = tile(Some((3, 20, 5)), (0, 127));
        assert_eq!(changed_band(&both, &high, 1, -1), None);
    }

    /// Colored sky (ADR 0064): the same beam with and without a pane in it changes the colors of
    /// its cells, so the band holds the whole beam in the window; equal panes change nothing.
    #[test]
    fn changed_bands_hold_the_beams_whose_filter_cells_differ() {
        let mut b = boxel_voxel::testing::builder();
        let first = add_sky_source(&mut b).expect("sources").0 - 1;
        let pane = b
            .add_block(boxel_voxel::BlockDef {
                name: "test:red_pane".into(),
                shape: boxel_voxel::RenderShape::Translucent,
                model: boxel_voxel::BlockModel::CUBE,
                collision: boxel_voxel::BlockCollision::Full,
                emission: [0; 3],
                light_opacity: 0,
                textures: boxel_voxel::FaceTextures::all(boxel_voxel::TextureId(0)),
            })
            .expect("pane");
        let block = b.block_by_name("test:red_pane").expect("pane");
        b.set_light_filter(block, [10, 3, 2]).expect("filter");
        let registry = b.build();
        let tables = LightTables::from_registry(&registry);
        let r64 =
            BlockState(first + SKY_SOURCE_RANGES.iter().position(|&r| r == 64).unwrap() as u16);
        let tile = |pane_at: Option<u32>| {
            let mut chunk = ChunkData::uniform(BlockState::AIR);
            chunk.set(LocalPos::new(3, 20, 5), r64);
            if let Some(y) = pane_at {
                chunk.set(LocalPos::new(3, y, 5), pane);
            }
            boxel_light::beam_tile::build_colored(0..=127, &[], &tables, |cy| {
                if cy == 2 {
                    LayerData::Ref(&chunk)
                } else {
                    LayerData::Air
                }
            })
        };
        let (clear, red) = (tile(None), tile(Some(10)));
        assert!(red.has_tints() && !clear.has_tints());
        assert_eq!(changed_band(&red, &tile(Some(10)), 1, -1), None);
        // The beam lights 21..=84 in both tiles.
        let band = changed_band(&clear, &red, 1, -1).expect("retinted");
        assert_eq!(
            band,
            [35 - 14..=35 + 14, 21 - 14..=84 + 14, -27 - 14..=-27 + 14]
        );
        assert_eq!(changed_band(&red, &clear, 1, -1), Some(band.clone()));
        assert_eq!(changed_band(&red, &tile(Some(4)), 1, -1), Some(band));
    }
}
