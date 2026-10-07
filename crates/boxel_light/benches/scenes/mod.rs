//! Bench scenes shared by the light benches and the mesher's (`boxel_mesh/benches/mesh.rs`
//! includes this file): neighborhoods of 27 overworld chunks and 9 sky tiles, the surface chunk
//! near the origin with trees and a cave entrance, and a cave lit by colored lamps.
#![allow(dead_code, reason = "each bench uses a subset of the scenes")]

use boxel_light::box_light::tile_index;
use boxel_light::sky_tile::{DEFAULT_WALK_LIMIT, GeneratorTop, walk, walk_floor};
use boxel_light::{LayerData, LightInput, SkyInput, SkyTile};
use boxel_voxel::{
    BlockDef, BlockRegistry, BlockRegistryBuilder, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey,
    ChunkPos, FaceTextures, LightTables, LocalPos, RenderShape, TextureId,
};
use boxel_worldgen::{OverworldGenerator, WorldGenerator};

/// The overworld's sky tile of chunk column `(cx, cz)`, every layer generated privately.
pub fn generated_tile(
    g: &OverworldGenerator,
    registry: &BlockRegistry,
    tables: &LightTables,
    cx: i64,
    cz: i64,
) -> SkyTile {
    let range = g.vertical_chunks();
    let top = GeneratorTop::of(&range).voxel().expect("bounded top");
    walk(top, walk_floor(top, DEFAULT_WALK_LIMIT), tables, |cy| {
        if cy > *range.end() {
            LayerData::Air
        } else {
            LayerData::Owned(g.generate(ChunkKey::new(ChunkPos::new(cx, cy, cz)), registry))
        }
    })
}

/// 27 chunks and 9 tiles around a chunk.
pub struct Neighborhood {
    /// The center chunk.
    pub origin: ChunkPos,
    /// Chunks in `chunk_index` order.
    pub chunks: Vec<ChunkData>,
    /// Tiles by `(dx + 1) * 3 + (dz + 1)`.
    pub tiles: Vec<SkyTile>,
}

impl Neighborhood {
    pub fn generate(
        g: &OverworldGenerator,
        registry: &BlockRegistry,
        tables: &LightTables,
        origin: ChunkPos,
    ) -> Self {
        let mut chunks = Vec::with_capacity(27);
        for dy in -1..=1 {
            for dx in -1..=1 {
                for dz in -1..=1 {
                    let p = ChunkPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                    chunks.push(g.generate(ChunkKey::new(p), registry));
                }
            }
        }
        let mut tiles = Vec::with_capacity(9);
        for dx in -1..=1 {
            for dz in -1..=1 {
                tiles.push(generated_tile(
                    g,
                    registry,
                    tables,
                    origin.x + dx,
                    origin.z + dz,
                ));
            }
        }
        Self {
            origin,
            chunks,
            tiles,
        }
    }

    /// Chunks in `chunk_index` order (`dy`, `dx`, `dz` from -1), tiles in `tile_index` order.
    pub fn input<'a>(&'a self, tables: &'a LightTables) -> LightInput<'a> {
        let mut chunks = [None; 27];
        for (i, c) in chunks.iter_mut().enumerate() {
            *c = Some(&self.chunks[i]);
        }
        let mut tiles = [&self.tiles[0]; 9];
        for dx in -1..=1 {
            for dz in -1..=1 {
                tiles[tile_index(dx, dz)] = &self.tiles[((dx + 1) * 3 + dz + 1) as usize];
            }
        }
        LightInput {
            origin: self.origin,
            chunks,
            sky: SkyInput::Tiles(tiles),
            tables,
        }
    }

    /// The same content `by` chunks away (the tiles' tops move with it).
    pub fn translated(&self, by: ChunkPos) -> Self {
        let dy = by.y << 5;
        let tiles = self
            .tiles
            .iter()
            .map(|t| {
                let tops: Vec<i64> = t
                    .tops()
                    .iter()
                    .map(|&v| if v == i64::MIN { v } else { v + dy })
                    .collect();
                SkyTile::from_tops(tops.into_boxed_slice().try_into().unwrap(), t.floor() + dy)
            })
            .collect();
        Self {
            origin: ChunkPos::new(
                self.origin.x + by.x,
                self.origin.y + by.y,
                self.origin.z + by.z,
            ),
            chunks: self.chunks.clone(),
            tiles,
        }
    }

    /// Fraction of air in the 27 chunks.
    pub fn air(&self) -> f64 {
        let air: usize = self
            .chunks
            .iter()
            .map(|c| {
                (0..CHUNK_VOLUME)
                    .filter(|&i| c.get(LocalPos::from_index(i)).is_air())
                    .count()
            })
            .sum();
        air as f64 / (27 * CHUNK_VOLUME) as f64
    }
}

/// Trees and cave entrances of a chunk column's tile: whether some top is above the ground
/// (a tree), and the first column whose surface voxel is carved (top 3 or more below it).
pub fn trees_and_entrance(
    g: &OverworldGenerator,
    tile: &SkyTile,
    cx: i64,
    cz: i64,
) -> (bool, Option<i64>) {
    let (mut trees, mut entrance) = (false, None);
    for i in 0..1024 {
        let (x, z) = (cx * 32 + (i >> 5) as i64, cz * 32 + (i & 31) as i64);
        let h = g.height_at(x, z);
        trees |= tile.tops()[i] > h;
        if tile.tops()[i] < h - 2 && entrance.is_none() {
            entrance = Some(h);
        }
    }
    (trees, entrance)
}

/// A surface chunk near the origin whose box holds trees and a cave entrance: the first chunk
/// column (searching outwards) with both, at the layer of the entrance's surface.
pub fn surface_origin(
    g: &OverworldGenerator,
    registry: &BlockRegistry,
    tables: &LightTables,
) -> ChunkPos {
    for r in 0i64..48 {
        for (cx, cz) in [
            (r, 0),
            (0, r),
            (-r, 0),
            (0, -r),
            (r, r),
            (-r, -r),
            (r, -r),
            (-r, r),
        ] {
            let tile = generated_tile(g, registry, tables, cx, cz);
            if let (true, Some(h)) = trees_and_entrance(g, &tile, cx, cz) {
                return ChunkPos::new(cx, h >> 5, cz);
            }
        }
    }
    panic!("no chunk column with trees and a cave entrance near the origin");
}

/// The default blocks plus three opaque lamps: orange, red, blue.
pub fn lamp_registry() -> (BlockRegistry, [BlockState; 3]) {
    let mut b = BlockRegistryBuilder::with_default_blocks();
    let mut lamp = |name: &str, emission| {
        b.add_block(BlockDef {
            name: format!("bench:{name}"),
            shape: RenderShape::Opaque,
            solid: true,
            emission,
            light_opacity: 15,
            textures: FaceTextures::all(TextureId(0)),
        })
        .expect("lamp")
    };
    let lamps = [
        lamp("lamp_orange", [14, 11, 6]),
        lamp("lamp_red", [14, 3, 3]),
        lamp("lamp_blue", [4, 6, 14]),
    ];
    (b.build(), lamps)
}

/// A cave chunk below the surface with at least 8% air around it, with lamps on 1.5% of the
/// floor voxels (air over stone) of all 27 chunks, colors in turn.
pub fn lamp_cave(
    g: &OverworldGenerator,
    registry: &BlockRegistry,
    tables: &LightTables,
    lamps: [BlockState; 3],
) -> (Neighborhood, usize) {
    let mut found = None;
    'search: for cy in [-8i64, -12, -16, -20] {
        for cx in 0..16i64 {
            let n = Neighborhood::generate(g, registry, tables, ChunkPos::new(cx, cy, 0));
            if n.air() >= 0.08 {
                found = Some(n);
                break 'search;
            }
        }
    }
    let mut n = found.expect("a cave neighborhood with 8% air");
    let mut placed = 0;
    let mut floor_seen = 0usize;
    for chunk in &mut n.chunks {
        for i in CHUNK_VOLUME / 32..CHUNK_VOLUME {
            let p = LocalPos::from_index(i);
            let below = LocalPos::from_index(i - 1024);
            if chunk.get(p).is_air() && !chunk.get(below).is_air() {
                floor_seen += 1;
                // 1.5%: every 67th floor voxel (deterministic).
                if floor_seen.is_multiple_of(67) {
                    chunk.set(p, lamps[placed % 3]);
                    placed += 1;
                }
            }
        }
    }
    (n, placed)
}
