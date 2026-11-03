//! From-scratch references of what the streamed world publishes, for tests and debugging.
//!
//! [`Reference`] recomputes sky tiles, beam tiles, light and meshes from the loaded chunk data
//! (which holds the edits) and the generator alone: no cached tile, light or mask, no
//! incremental update, no fast path of the world (every box gets its 9 tiles, even under the
//! walk floor, and its 9 beam tiles, without the gate). A settled world's meshes must equal
//! these, with the dynamic lights the world bakes given to [`Reference::with_points`] (ADR 0063).
//! Slow: every tile is a fresh walk, every beam tile a fresh build for one box.
use crate::beams::box_rows;
use crate::sky::{SkyRule, may_block};
use crate::{ChunkMap, ChunkSlot, DataState, SkyMap, WorldContent};
use boxel_light::sky_tile::walk;
use boxel_light::{
    BeamTile, LayerData, LightInput, LightScratch, PaddedLight, PointEmitter, PointScratch,
    SkyInput, SkyTile,
};
use boxel_mesh::{ChunkMesh, MeshInput, MeshOptions, Mesher};
use boxel_voxel::{CHUNK_SHIFT, ChunkData, ChunkKey, ChunkPos, LocalPos, NEIGHBOR_OFFSETS};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// From-scratch tiles, light and meshes of a world as it is loaded now (see the
/// [module docs](self)). Tiles are cached inside, so build a new one after the world changes.
pub struct Reference<'w> {
    map: &'w ChunkMap,
    sky: &'w SkyMap,
    content: &'w WorldContent,
    tiles: HashMap<(i64, i64), Arc<SkyTile>>,
    beams: HashMap<ChunkPos, Arc<BeamTile>>,
    generated: HashMap<ChunkPos, Arc<ChunkData>>,
    mesher: Mesher,
    scratch: LightScratch,
    points: Vec<PointEmitter>,
}

impl<'w> Reference<'w> {
    /// The reference of the world in `map`, `sky` and `content`.
    pub fn new(map: &'w ChunkMap, sky: &'w SkyMap, content: &'w WorldContent) -> Self {
        Self {
            map,
            sky,
            content,
            tiles: HashMap::new(),
            beams: HashMap::new(),
            generated: HashMap::new(),
            mesher: Mesher::new(),
            scratch: LightScratch::new(),
            points: Vec::new(),
        }
    }

    /// The same reference with the dynamic lights `points` baked into its meshes (ADR 0063):
    /// [`Reference::mesh`] shows their light, [`Reference::light`] stays the blocks' own.
    pub fn with_points(mut self, points: &[PointEmitter]) -> Self {
        self.points = points.to_vec();
        self
    }

    /// The sky tile of chunk column `(cx, cz)`, walked from scratch: from the generator's top
    /// or the highest blocker of a loaded chunk above it, through the loaded chunks (and the
    /// generator where none is loaded). `None` when the world has no sky tops (light off, or
    /// every column open).
    pub fn tile(&mut self, cx: i64, cz: i64) -> Option<Arc<SkyTile>> {
        let SkyRule::Walk(rule) = &self.sky.rule else {
            return None;
        };
        if let Some(tile) = self.tiles.get(&(cx, cz)) {
            return Some(Arc::clone(tile));
        }
        let tables = &self.content.light_tables;
        let loaded: BTreeMap<i64, &DataState> = self
            .map
            .iter()
            .filter(|(_, s)| s.pos.x == cx && s.pos.z == cz)
            .map(|(_, s)| (s.pos.y, s.data()))
            .collect();
        let ready = |data: &DataState| match data {
            DataState::Ready(d) => Some(Arc::clone(d)),
            _ => None,
        };
        // The highest blocker of a loaded chunk above the generator's range.
        let above = |cy: i64| rule.layers.is_empty() || cy > *rule.layers.end();
        let edit_top = loaded
            .iter()
            .rev()
            .filter(|(cy, _)| above(**cy))
            .find_map(|(&cy, data)| {
                let d = ready(data)?;
                let top = (0..32u32).rev().find(|&ly| {
                    (0..32)
                        .any(|x| (0..32).any(|z| tables.blocks_sky(d.get(LocalPos::new(x, ly, z)))))
                })?;
                Some((cy << CHUNK_SHIFT) + i64::from(top))
            })
            .unwrap_or(i64::MIN);
        let tile = match rule.start(edit_top) {
            None => SkyTile::uniform(i64::MIN, i64::MIN),
            Some(start) => {
                let (content, layers) = (self.content, &rule.layers);
                walk(start, rule.floor, tables, |cy| {
                    let here = loaded.get(&cy).and_then(|d| ready(d));
                    let in_range = layers.contains(&cy);
                    if in_range || (!layers.is_empty() && above(cy)) {
                        return match (here, loaded.get(&cy)) {
                            (Some(d), _) => LayerData::Shared(d),
                            (None, Some(DataState::Missing)) => LayerData::Air,
                            _ if in_range => {
                                let key = ChunkKey::new(ChunkPos::new(cx, cy, cz));
                                LayerData::Owned(content.generator.generate(key, &content.registry))
                            }
                            _ => LayerData::Air,
                        };
                    }
                    // Under the generator only content counts: a layer is void when no loaded
                    // chunk at or under it holds a blocker.
                    if let Some(d) = here.filter(|d| may_block(d, tables)) {
                        return LayerData::Shared(d);
                    }
                    let lower = loaded
                        .range(..cy)
                        .any(|(_, d)| ready(d).is_some_and(|d| may_block(&d, tables)));
                    if lower {
                        LayerData::Air
                    } else {
                        LayerData::Void
                    }
                })
            }
        };
        let tile = Arc::new(tile);
        self.tiles.insert((cx, cz), Arc::clone(&tile));
        Some(tile)
    }

    /// The beam tile of chunk column `(pos.x, pos.z)` for the light box of the chunk layer
    /// `pos.y` (ADR 0061), built from scratch: the loaded chunks as they are (`Ready` data; a
    /// `Missing` chunk is air), the generator where none is loaded. `None` when the world has no
    /// sky tops (light off, or every column open: beams add nothing).
    pub fn beam_tile(&mut self, pos: ChunkPos) -> Option<Arc<BeamTile>> {
        let SkyRule::Walk(rule) = &self.sky.rule else {
            return None;
        };
        if let Some(tile) = self.beams.get(&pos) {
            return Some(Arc::clone(tile));
        }
        let (lo, hi) = box_rows(pos.y);
        let content = self.content;
        let (map, layers, generated) = (self.map, &rule.layers, &mut self.generated);
        // Every layer the build reads, first: the closure only borrows.
        let (bottom, top) = crate::beams::BeamJob::read_range((lo, hi));
        let mut chunks: HashMap<i64, Option<Arc<ChunkData>>> = HashMap::new();
        if content.light_tables.any_sky_source() {
            for cy in bottom..=top {
                let at = ChunkPos::new(pos.x, cy, pos.z);
                let data = match map
                    .id_at(at)
                    .and_then(|id| map.get(id))
                    .map(ChunkSlot::data)
                {
                    Some(DataState::Ready(d)) => Some(Arc::clone(d)),
                    Some(DataState::Missing) => None,
                    _ if layers.contains(&cy) => {
                        Some(Arc::clone(generated.entry(at).or_insert_with(|| {
                            let key = ChunkKey::new(at);
                            Arc::new(content.generator.generate(key, &content.registry))
                        })))
                    }
                    _ => None,
                };
                chunks.insert(cy, data);
            }
        }
        let planes = content.generator.realms().planes();
        let tile = boxel_light::beam_tile::build(lo..=hi, planes, &content.light_tables, |cy| {
            chunks
                .get(&cy)
                .and_then(Option::as_ref)
                .map_or(LayerData::Air, |d| LayerData::Shared(Arc::clone(d)))
        });
        let tile = Arc::new(tile);
        self.beams.insert(pos, Arc::clone(&tile));
        Some(tile)
    }

    /// The light of the padded chunk at `pos`, from its 27 chunks as loaded (`Ready` data; any
    /// other chunk is air, as the world meshes it), its 9 tiles and 9 beam tiles from scratch.
    /// `None` with light off.
    pub fn light(&mut self, pos: ChunkPos) -> Option<PaddedLight> {
        self.light_with(pos, false)
    }

    /// [`Reference::light`], with the dynamic lights of [`Reference::with_points`] when
    /// `points`.
    fn light_with(&mut self, pos: ChunkPos, points: bool) -> Option<PaddedLight> {
        if self.sky.rule == SkyRule::Off {
            return None;
        }
        let tiles: [Arc<SkyTile>; 9] = std::array::from_fn(|i| {
            let (dx, dz) = (i as i64 / 3 - 1, i as i64 % 3 - 1);
            self.tile(pos.x + dx, pos.z + dz)
                .unwrap_or_else(|| Arc::clone(&self.sky.open))
        });
        let beams: [Option<Arc<BeamTile>>; 9] = std::array::from_fn(|i| {
            let (dx, dz) = (i as i64 / 3 - 1, i as i64 % 3 - 1);
            self.beam_tile(ChunkPos::new(pos.x + dx, pos.y, pos.z + dz))
        });
        let chunks = self.chunks(pos);
        let input = LightInput {
            origin: pos,
            chunks: std::array::from_fn(|i| chunks[i].as_deref()),
            sky: SkyInput::Tiles(std::array::from_fn(|i| &*tiles[i])),
            beams: std::array::from_fn(|i| beams[i].as_deref()),
            tables: &self.content.light_tables,
        };
        let mut out = PaddedLight::default();
        boxel_light::compute_full(&input, &mut self.scratch, &mut out);
        if points {
            let mut scratch = PointScratch::new();
            boxel_light::add_point_lights(&input, &self.points, &mut scratch, &mut out);
        }
        Some(out)
    }

    /// The mesh of the chunk at `pos` (its data must be `Ready`), with [`Reference::light`] and
    /// the dynamic lights of [`Reference::with_points`].
    pub fn mesh(&mut self, pos: ChunkPos, options: MeshOptions) -> ChunkMesh {
        let light = self.light_with(pos, true);
        let chunks = self.chunks(pos);
        let center = chunks[13].as_deref().expect("the chunk's data is Ready");
        let input = MeshInput {
            center,
            neighbors: std::array::from_fn(|i| chunks[if i < 13 { i } else { i + 1 }].as_deref()),
            light: light.as_ref(),
        };
        self.mesher
            .mesh_chunk(&input, &self.content.materials, options)
    }

    /// The `Ready` data of the 27 chunks around `pos` (center at 13).
    fn chunks(&self, pos: ChunkPos) -> [Option<Arc<ChunkData>>; 27] {
        std::array::from_fn(|i| {
            let offset = match i {
                13 => return self.map.data_at(pos).cloned(),
                i if i < 13 => NEIGHBOR_OFFSETS[i],
                i => NEIGHBOR_OFFSETS[i - 1],
            };
            self.map.data_at(pos.offset(offset)).cloned()
        })
    }
}
