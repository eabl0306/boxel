//! Light benchmarks (ADR 0015, phase P1). Release, on an idle machine.
//!
//! | Bench | Target |
//! |---|---|
//! | `light/box_cave_dark` (deep overworld chunk, fast path) | 5 us or less |
//! | `light/box_surface` (overworld surface near the origin, trees and an entrance) | 150 us or less (median) |
//! | `light/box_cave_lamps_rgb` (cave, lamps on 1.5% of the floor, 3 colors) | 500 us or less |
//! | `light/box_far` (the surface neighborhood at x = z = 2^40) | 1.15x of `box_surface` or less |
//! | `sky_tile/walk_generate` (overworld column, generated privately) | 400 us or less |
//! | `sky_tile/walk_loaded` (the same column loaded, masks not built yet) | 60 us or less |
//!
//! The box benches run with the chunk masks cached (each chunk's masks are built once and
//! shared by the 27 boxes that read it); `light/box_surface_cold_masks` builds them too.
#![expect(clippy::print_stderr, reason = "bench scene statistics")]

use boxel_light::box_light::tile_index;
use boxel_light::sky_tile::{DEFAULT_WALK_LIMIT, GeneratorTop, walk, walk_floor};
use boxel_light::{
    LayerData, LightInput, LightPath, LightScratch, PaddedLight, SkyInput, SkyTile, compute,
};
use boxel_voxel::{
    BlockDef, BlockRegistry, BlockRegistryBuilder, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey,
    ChunkPos, FaceTextures, LightTables, LocalPos, RenderShape, TextureId,
};
use boxel_worldgen::{OverworldGenerator, WorldGenerator};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

/// The overworld's sky tile of chunk column `(cx, cz)`, every layer generated privately.
fn generated_tile(
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
struct Neighborhood {
    origin: ChunkPos,
    chunks: Vec<ChunkData>,
    tiles: Vec<SkyTile>,
}

impl Neighborhood {
    fn generate(
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
    fn input<'a>(&'a self, tables: &'a LightTables) -> LightInput<'a> {
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
    fn translated(&self, by: ChunkPos) -> Self {
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
    fn air(&self) -> f64 {
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
fn trees_and_entrance(
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
fn surface_origin(
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
fn lamp_registry() -> (BlockRegistry, [BlockState; 3]) {
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
fn lamp_cave(
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

fn bench(c: &mut Criterion) {
    let g = OverworldGenerator::default();
    let (registry, lamps) = lamp_registry();
    let tables = LightTables::from_registry(&registry);
    let mut scratch = LightScratch::new();
    let mut out = PaddedLight::default();

    let deep = Neighborhood::generate(&g, &registry, &tables, ChunkPos::new(0, -40, 0));
    let surface = Neighborhood::generate(
        &g,
        &registry,
        &tables,
        surface_origin(&g, &registry, &tables),
    );
    let far = surface.translated(ChunkPos::new(1 << 35, 0, 1 << 35));
    let (cave, placed) = lamp_cave(&g, &registry, &tables, lamps);

    let mut report = |name: &str, n: &Neighborhood| {
        let path = compute(&n.input(&tables), &mut scratch, &mut out);
        eprintln!(
            "{name}: chunk {:?}, air {:.1}%, {path:?}",
            n.origin,
            100.0 * n.air()
        );
        path
    };
    // Each bench must time the path its target is for: a fast path that slips in after a
    // worldgen change would "meet" the full-path targets.
    assert_eq!(report("box_cave_dark", &deep), LightPath::FastDark);
    let center = &surface.tiles[tile_index(0, 0)];
    let (cx, cz) = (surface.origin.x, surface.origin.z);
    assert!(
        trees_and_entrance(&g, center, cx, cz).0,
        "box_surface without trees"
    );
    let surface_path = report("box_surface", &surface);
    assert!(
        matches!(surface_path, LightPath::Full { sky_pops, .. } if sky_pops > 0),
        "box_surface took {surface_path:?}"
    );
    assert_eq!(report("box_far", &far), surface_path);
    let lamps_path = report("box_cave_lamps_rgb", &cave);
    assert!(
        matches!(lamps_path, LightPath::Full { block_pops, .. } if block_pops > 0),
        "box_cave_lamps_rgb took {lamps_path:?}"
    );
    eprintln!("box_cave_lamps_rgb: {placed} lamps");

    let mut g_light = c.benchmark_group("light");
    g_light.measurement_time(Duration::from_secs(4));
    for (name, n) in [
        ("box_cave_dark", &deep),
        ("box_surface", &surface),
        ("box_far", &far),
        ("box_cave_lamps_rgb", &cave),
    ] {
        let input = n.input(&tables);
        g_light.bench_function(name, |b| {
            b.iter(|| compute(black_box(&input), &mut scratch, &mut out));
        });
    }
    g_light.bench_function("box_surface_cold_masks", |b| {
        b.iter_batched(
            // A clone starts without masks.
            || Neighborhood {
                origin: surface.origin,
                chunks: surface.chunks.clone(),
                tiles: surface.tiles.clone(),
            },
            |n| compute(&n.input(&tables), &mut scratch, &mut out),
            BatchSize::LargeInput,
        );
    });
    g_light.finish();

    // The walk of the surface chunk's column: privately generated, and from loaded chunks
    // whose masks are not built yet.
    let (cx, cz) = (surface.origin.x, surface.origin.z);
    let range = g.vertical_chunks();
    let top = GeneratorTop::of(&range).voxel().expect("bounded top");
    let floor = walk_floor(top, DEFAULT_WALK_LIMIT);
    let loaded: Vec<(i64, Arc<ChunkData>)> = (-64..=top >> 5)
        .rev()
        .map(|cy| {
            let key = ChunkKey::new(ChunkPos::new(cx, cy, cz));
            (cy, Arc::new(g.generate(key, &registry)))
        })
        .collect();
    let layer_of = |cy: i64| loaded.iter().find(|(y, _)| *y == cy).map(|(_, c)| c);
    let mut layers_read = 0;
    walk(top, floor, &tables, |cy| {
        layers_read += 1;
        layer_of(cy).map_or(LayerData::Air, |c| LayerData::Shared(Arc::clone(c)))
    });
    eprintln!("sky_tile walk of column ({cx}, {cz}): {layers_read} layers");
    let mut g_sky = c.benchmark_group("sky_tile");
    g_sky.measurement_time(Duration::from_secs(4));
    g_sky.bench_function("walk_generate", |b| {
        b.iter(|| generated_tile(&g, &registry, &tables, black_box(cx), cz));
    });
    g_sky.bench_function("walk_loaded", |b| {
        b.iter_batched(
            || {
                loaded
                    .iter()
                    .map(|(y, c)| (*y, ChunkData::clone(c)))
                    .collect::<Vec<_>>()
            },
            |chunks| {
                walk(top, floor, &tables, |cy| {
                    chunks
                        .iter()
                        .find(|(y, _)| *y == cy)
                        .map_or(LayerData::Air, |(_, c)| LayerData::Ref(c))
                })
            },
            BatchSize::LargeInput,
        );
    });
    g_sky.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
