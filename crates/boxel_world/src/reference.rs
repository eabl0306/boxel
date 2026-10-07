//! From-scratch references of what the streamed world publishes, for tests and debugging.
//!
//! [`Reference`] recomputes sky tiles, light and meshes from the loaded chunk data (which holds
//! the edits) and the generator alone: no cached tile, light or mask, no incremental update, no
//! fast path of the world (every box gets its 9 tiles, even under the walk floor). A settled
//! world's meshes must equal these. Slow: every tile is a fresh walk.
use crate::sky::{SkyRule, may_block};
use crate::{ChunkMap, DataState, SkyMap, WorldContent};
use boxel_light::sky_tile::walk;
use boxel_light::{LayerData, LightInput, LightScratch, PaddedLight, SkyInput, SkyTile};
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
    mesher: Mesher,
    scratch: LightScratch,
}

impl<'w> Reference<'w> {
    /// The reference of the world in `map`, `sky` and `content`.
    pub fn new(map: &'w ChunkMap, sky: &'w SkyMap, content: &'w WorldContent) -> Self {
        Self {
            map,
            sky,
            content,
            tiles: HashMap::new(),
            mesher: Mesher::new(),
            scratch: LightScratch::new(),
        }
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

    /// The light of the padded chunk at `pos`, from its 27 chunks as loaded (`Ready` data; any
    /// other chunk is air, as the world meshes it) and its 9 tiles from scratch. `None` with
    /// light off.
    pub fn light(&mut self, pos: ChunkPos) -> Option<PaddedLight> {
        if self.sky.rule == SkyRule::Off {
            return None;
        }
        let tiles: [Arc<SkyTile>; 9] = std::array::from_fn(|i| {
            let (dx, dz) = (i as i64 / 3 - 1, i as i64 % 3 - 1);
            self.tile(pos.x + dx, pos.z + dz)
                .unwrap_or_else(|| Arc::clone(&self.sky.open))
        });
        let chunks = self.chunks(pos);
        let input = LightInput {
            origin: pos,
            chunks: std::array::from_fn(|i| chunks[i].as_deref()),
            sky: SkyInput::Tiles(std::array::from_fn(|i| &*tiles[i])),
            tables: &self.content.light_tables,
        };
        let mut out = PaddedLight::default();
        boxel_light::compute_full(&input, &mut self.scratch, &mut out);
        Some(out)
    }

    /// The mesh of the chunk at `pos` (its data must be `Ready`), with [`Reference::light`].
    pub fn mesh(&mut self, pos: ChunkPos, options: MeshOptions) -> ChunkMesh {
        let light = self.light(pos);
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
