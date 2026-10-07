//! Helpers shared by the light test files (each test binary uses a subset).
#![allow(dead_code, reason = "each test binary uses a subset of the helpers")]

use boxel_light::box_light::{chunk_index, tile_index};
use boxel_light::{LightInput, SkyInput, SkyTile};
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockRegistryBuilder, BlockState,
    CHUNK_AREA, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, ChunkPos, FaceTextures, LightTables, LocalPos,
    Occupancy, RenderShape, TextureId,
};

/// Small deterministic generator (`SplitMix64`).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
    /// True with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Blocks of [`test_registry`] (the built-in ones, then these).
pub struct Blocks {
    pub air: BlockState,
    pub stone: BlockState,
    pub leaves: BlockState,
    pub water: BlockState,
    pub glass: BlockState,
    /// Opaque lamps: orange, red, blue.
    pub lamps: [BlockState; 3],
    /// Emitters light passes through: glowing air (opacity 0), a lantern (opacity 1).
    pub glows: [BlockState; 2],
    /// Opacity 5, only in the heavy registry.
    pub smoke: Option<BlockState>,
}

/// The built-in blocks plus glass, three opaque lamps, two clear emitters and (with `heavy`)
/// smoke of opacity 5.
pub fn test_registry(heavy: bool) -> (BlockRegistry, Blocks) {
    let mut b = BlockRegistryBuilder::with_default_blocks();
    let tex = FaceTextures::all(TextureId(0));
    let mut add = |name: &str, shape, solid, emission, light_opacity| {
        b.add_block(BlockDef {
            name: format!("test:{name}"),
            shape,
            model: BlockModel::CUBE,
            collision: if solid {
                BlockCollision::Full
            } else {
                BlockCollision::None
            },
            emission,
            light_opacity,
            textures: tex,
        })
        .unwrap()
    };
    let glass = add("glass", RenderShape::Translucent, true, [0; 3], 0);
    let lamps = [
        add("lamp_orange", RenderShape::Opaque, true, [14, 11, 6], 15),
        add("lamp_red", RenderShape::Opaque, true, [15, 3, 2], 15),
        add("lamp_blue", RenderShape::Opaque, true, [3, 6, 15], 15),
    ];
    let glows = [
        add("glow", RenderShape::None, false, [9, 0, 12], 0),
        add("lantern", RenderShape::Cutout, true, [12, 12, 12], 1),
    ];
    let smoke = heavy.then(|| add("smoke", RenderShape::Translucent, false, [0; 3], 5));
    let r = b.build();
    let blocks = Blocks {
        air: BlockState::AIR,
        stone: r.by_name("boxel:stone").unwrap(),
        leaves: r.by_name("boxel:leaves").unwrap(),
        water: r.by_name("boxel:water").unwrap(),
        glass,
        lamps,
        glows,
        smoke,
    };
    (r, blocks)
}

/// A random chunk: uniform air, stone or leaves, a 1-bit cave chunk, a terrain-like layer, or
/// noise of every block (densities per chunk). `airy` favors air (top layers).
pub fn random_chunk(rng: &mut Rng, b: &Blocks, airy: bool) -> ChunkData {
    let kind = if airy && rng.chance(0.5) {
        0
    } else {
        rng.below(7)
    };
    match kind {
        0 => ChunkData::uniform(b.air),
        1 => ChunkData::uniform(b.stone),
        2 => ChunkData::uniform(b.leaves),
        3 => {
            // Cave-like: stone with blobs of air (1-bit).
            let density = 0.2 + 0.6 * rng.unit();
            let mask: Occupancy = std::array::from_fn(|_| {
                (0..32).fold(0u32, |w, z| w | (u32::from(rng.chance(density)) << z))
            });
            ChunkData::from_occupancy(&mask, b.stone, b.air)
        }
        4 => {
            // A ground plane with holes, leaves and lamps on top.
            let h = rng.below(32) as u32;
            let mut dense = vec![b.air; CHUNK_VOLUME];
            for (i, s) in dense.iter_mut().enumerate() {
                let p = LocalPos::from_index(i);
                *s = if p.y() < h && !rng.chance(0.03) {
                    b.stone
                } else if p.y() <= h + 4 && rng.chance(0.05) {
                    b.leaves
                } else if p.y() == h && rng.chance(0.01) {
                    b.lamps[rng.below(3) as usize]
                } else {
                    b.air
                };
            }
            ChunkData::from_dense(&dense)
        }
        _ => {
            let stone = 0.6 * rng.unit();
            let partial = 0.15 * rng.unit();
            let emit = 0.004 * rng.unit();
            let mut dense = vec![b.air; CHUNK_VOLUME];
            for s in &mut dense {
                let u = rng.unit();
                *s = if u < emit {
                    let all = [b.lamps[0], b.lamps[1], b.lamps[2], b.glows[0], b.glows[1]];
                    all[rng.below(5) as usize]
                } else if u < emit + stone {
                    b.stone
                } else if u < emit + stone + partial {
                    match (rng.below(4), b.smoke) {
                        (0, _) => b.leaves,
                        (1, _) => b.water,
                        (2, Some(smoke)) => smoke,
                        _ => b.glass,
                    }
                } else {
                    b.air
                };
            }
            ChunkData::from_dense(&dense)
        }
    }
}

/// A world of `n^3` chunks from `min` with sky tops per column (absolute y).
pub struct World {
    pub min: ChunkPos,
    pub n: usize,
    /// Chunks in `(y, x, z)` order.
    pub chunks: Vec<ChunkData>,
    /// Tops per column, `x * (32 n) + z` (voxels from the min corner).
    pub tops: Vec<i64>,
}

impl World {
    /// The chunk at offset `(cx, cy, cz)` chunks from `min`.
    pub fn chunk(&self, cx: usize, cy: usize, cz: usize) -> &ChunkData {
        &self.chunks[(cy * self.n + cx) * self.n + cz]
    }

    /// The tile of chunk column `(cx, cz)`.
    pub fn tile(&self, cx: usize, cz: usize) -> SkyTile {
        let s = CHUNK_SIZE as usize;
        let side = self.n * s;
        let tops: Vec<i64> = (0..CHUNK_AREA)
            .map(|i| self.tops[(cx * s + (i >> 5)) * side + cz * s + (i & 31)])
            .collect();
        SkyTile::from_tops(tops.into_boxed_slice().try_into().unwrap(), i64::MIN)
    }

    /// The light input of chunk `(cx, cy, cz)` (1..n-1 on each axis), with the tiles `tiles`
    /// (`n * n`, by `cx * n + cz`).
    pub fn input<'a>(
        &'a self,
        tiles: &'a [SkyTile],
        tables: &'a LightTables,
        (cx, cy, cz): (usize, usize, usize),
    ) -> LightInput<'a> {
        let mut chunks = [None; 27];
        for dy in -1..=1i32 {
            for dx in -1..=1i32 {
                for dz in -1..=1i32 {
                    let at = |c: usize, d: i32| (c as i32 + d) as usize;
                    chunks[chunk_index(dx, dy, dz)] =
                        Some(self.chunk(at(cx, dx), at(cy, dy), at(cz, dz)));
                }
            }
        }
        let tile = |dx: i32, dz: i32| {
            &tiles[(cx as i32 + dx) as usize * self.n + (cz as i32 + dz) as usize]
        };
        let mut picked = [tile(0, 0); 9];
        for dx in -1..=1 {
            for dz in -1..=1 {
                picked[tile_index(dx, dz)] = tile(dx, dz);
            }
        }
        LightInput {
            origin: ChunkPos::new(
                self.min.x + cx as i64,
                self.min.y + cy as i64,
                self.min.z + cz as i64,
            ),
            chunks,
            sky: SkyInput::Tiles(picked),
            tables,
        }
    }

    /// Every tile, by `cx * n + cz`.
    pub fn tiles(&self) -> Vec<SkyTile> {
        (0..self.n * self.n)
            .map(|i| self.tile(i / self.n, i % self.n))
            .collect()
    }
}

/// A random world of `n^3` chunks from `min`. Sky tops: the highest blocker of each column, or
/// in some chunk columns a roof above the world, a cap at a random height inside the world
/// (over air where the blockers are lower, as the walk floor), or (open columns) a top below
/// the world or `i64::MIN`. Some chunk columns are kept open (air and glass only).
pub fn random_world(seed: u64, n: usize, min: ChunkPos, b: &Blocks, tables: &LightTables) -> World {
    let mut rng = Rng::new(seed);
    let open_columns: Vec<bool> = (0..n * n).map(|_| rng.chance(0.15)).collect();
    let mut chunks = Vec::with_capacity(n * n * n);
    for cy in 0..n {
        for cx in 0..n {
            for cz in 0..n {
                let chunk = if open_columns[cx * n + cz] {
                    if rng.chance(0.3) {
                        let mut c = ChunkData::uniform(b.air);
                        for _ in 0..rng.below(200) {
                            let p = LocalPos::from_index(rng.below(CHUNK_VOLUME as u64) as usize);
                            c.set(p, b.glass);
                        }
                        c
                    } else {
                        ChunkData::uniform(b.air)
                    }
                } else {
                    random_chunk(&mut rng, b, cy + 2 >= n)
                };
                chunks.push(chunk);
            }
        }
    }
    let s = CHUNK_SIZE as usize;
    let side = n * s;
    let y0 = min.origin().y;
    let world_top = y0 + side as i64 - 1;
    let mut tops = vec![i64::MIN; side * side];
    for x in 0..side {
        for z in 0..side {
            let (cx, cz, lx, lz) = (x / s, z / s, (x % s) as u32, (z % s) as u32);
            let found = (0..side).rev().find(|&y| {
                let c = &chunks[((y / s) * n + cx) * n + cz];
                tables.blocks_sky(c.get(LocalPos::new(lx, (y % s) as u32, lz)))
            });
            let brute = found.map_or(i64::MIN, |y| y0 + y as i64);
            // Columns of a chunk column share the kind of extra, so tops vary smoothly.
            let mut column_rng = Rng::new(seed ^ ((cx * n + cz) as u64).wrapping_mul(0x51_7cc1));
            tops[x * side + z] = match column_rng.below(10) {
                0 => brute.max(world_top + 1 + rng.below(40) as i64),
                1 => brute.max(world_top + (1 << 40)),
                2 if brute == i64::MIN => y0 - 1 - rng.below(3) as i64,
                3 if brute == i64::MIN => y0 + rng.below(side as u64) as i64 - side as i64,
                // The same cap for the whole chunk column (its generator gives the same value).
                4 => brute.max(y0 + column_rng.below(side as u64) as i64),
                _ => brute,
            };
        }
    }
    World {
        min,
        n,
        chunks,
        tops,
    }
}
