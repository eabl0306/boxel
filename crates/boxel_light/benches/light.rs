//! Light benchmarks (ADR 0015, phase P1). Release, on an idle machine.
//!
//! | Bench | Target |
//! |---|---|
//! | `light/box_cave_dark` (deep overworld chunk, fast path) | 5 us or less |
//! | `light/box_surface` (overworld surface near the origin, trees and an entrance) | 150 us or less (median) |
//! | `light/box_cave_lamps_rgb` (cave, lamps on 1.5% of the floor, 3 colors) | 500 us or less |
//! | `light/box_cave_lamps_glass_filter` (the same cave, each lamp in glass of its color, ADR 0060) | 1.25x of `box_cave_lamps_glass_clear` or less |
//! | `light/box_cave_lamps_glass_clear` (the same glass voxels, clear) | as `box_cave_lamps_rgb` |
//! | `light/box_surface_cold_masks_filters` (`box_surface_cold_masks`, a registry with filters) | 1.05x of `box_surface_cold_masks` or less |
//! | `light/box_far` (the surface neighborhood at x = z = 2^40) | 1.15x of `box_surface` or less |
//! | `sky_tile/walk_generate` (overworld column, generated privately) | 400 us or less |
//! | `sky_tile/walk_loaded` (the same column loaded, masks not built yet) | 60 us or less |
//! | `light/box_cavern_ceiling` (ADR 0061: a 64-high cavern under a down source in every roof column, two pillars) | 100 us or less |
//! | `light/box_cavern_ledge` (the same with a ledge across the box: 30 000 cells in its shadow) | none (the flood of a large shadow) |
//! | `light/box_sunbeam` (a generated cave with one down source under its roof) | as `box_surface` |
//! | `beam_tile/build_loaded` (the cave's column, 19 layers loaded with masks, sources under every cave roof) | 20 to 60 us |
//! | `beam_tile/build_loaded_cold_masks` (the same, masks not built yet: each layer with a source builds them) | none |
//! | `beam_tile/build_generate` (the same column, every layer generated and decorated) | 22 to 60 us per layer, plus generation |
//! | `light/point_cave` (ADR 0063: a torch in the air nearest the center of `box_cave_lamps_rgb`, added to its light) | none (part of a mesh job near the light) |
//! | `light/point_surface` (a torch 2 blocks over the ground at the center of `box_surface`) | none |
//!
//! The box benches run with the chunk masks cached (each chunk's masks are built once and
//! shared by the 27 boxes that read it); `light/box_surface_cold_masks` builds them too.
#![expect(clippy::print_stderr, reason = "bench scene statistics")]

mod scenes;

use boxel_demo::worldgen::OverworldGenerator;
use boxel_light::box_light::{MARGIN, tile_index};
use boxel_light::sky_tile::{DEFAULT_WALK_LIMIT, GeneratorTop, walk, walk_floor};
use boxel_light::{
    BeamTile, LayerData, LightPath, LightScratch, PaddedLight, PointEmitter, PointScratch,
    add_point_lights, beam_tile, compute,
};
use boxel_voxel::{
    BeamDir, BlockPos, BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos,
    LightTables, LocalPos, SkySourceDef,
};
use boxel_worldgen::WorldGenerator;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use scenes::{
    Neighborhood, generated_tile, glass_cave, glass_registry, lamp_cave, lamp_registry,
    surface_origin, trees_and_entrance,
};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

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
    // The lamps in glass, colored and clear, read with the tables of a registry that has both.
    let (glass_blocks, glass_lamps, filter_glass, clear_glass) = glass_registry();
    let glass_tables = LightTables::from_registry(&glass_blocks);
    assert_eq!(glass_lamps, lamps);
    let (filtered, glass_placed) = glass_cave(&cave, lamps, filter_glass);
    let (clear, _) = glass_cave(&cave, lamps, [clear_glass; 3]);
    for (name, n) in [
        ("box_cave_lamps_glass_filter", &filtered),
        ("box_cave_lamps_glass_clear", &clear),
    ] {
        let path = compute(&n.input(&glass_tables), &mut scratch, &mut out);
        assert!(
            matches!(path, LightPath::Full { block_pops, .. } if block_pops > 0),
            "{name} took {path:?}"
        );
        eprintln!("{name}: {glass_placed} glass voxels, {path:?}");
    }

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
    for (name, n) in [
        ("box_cave_lamps_glass_filter", &filtered),
        ("box_cave_lamps_glass_clear", &clear),
    ] {
        let input = n.input(&glass_tables);
        g_light.bench_function(name, |b| {
            b.iter(|| compute(black_box(&input), &mut scratch, &mut out));
        });
    }
    // A torch (ADR 0063) in each: the flood a mesh job near it adds to the box's light.
    for (name, n, light) in [
        ("point_cave", &cave, nearest_air(&cave)),
        ("point_surface", &surface, over_ground(&surface)),
    ] {
        let input = n.input(&tables);
        compute(&input, &mut scratch, &mut out);
        let base = out.clone();
        let torch = [PointEmitter::new(light, [14, 11, 6])];
        let mut points = PointScratch::new();
        eprintln!("{name}: torch at {light:?}");
        g_light.bench_function(name, |b| {
            b.iter_batched_ref(
                || base.clone(),
                |lit| add_point_lights(black_box(&input), &torch, &mut points, lit),
                BatchSize::SmallInput,
            );
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
    // The same with the tables of a registry that has filters (as a game with colored glass):
    // the masks also scan each palette for a filter state.
    g_light.bench_function("box_surface_cold_masks_filters", |b| {
        b.iter_batched(
            || Neighborhood {
                origin: surface.origin,
                chunks: surface.chunks.clone(),
                tiles: surface.tiles.clone(),
            },
            |n| compute(&n.input(&glass_tables), &mut scratch, &mut out),
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

/// The demo blocks plus `test:sky_source` (ADR 0061), and its down state of range `range`.
/// The air voxel of the center chunk of `n` nearest its center.
fn nearest_air(n: &Neighborhood) -> BlockPos {
    let center = &n.chunks[13];
    let o = n.origin.origin();
    (0..CHUNK_VOLUME)
        .map(LocalPos::from_index)
        .filter(|&p| center.get(p).is_air())
        .min_by_key(|p| {
            let d = |c: u32| (i64::from(c) - 16).abs();
            d(p.x()) + d(p.y()) + d(p.z())
        })
        .map(|p| {
            let [x, y, z] = [p.x(), p.y(), p.z()].map(i64::from);
            BlockPos::new(o.x + x, o.y + y, o.z + z)
        })
        .expect("an air voxel")
}

/// Two blocks over the highest non-air voxel of the center column of `n`.
fn over_ground(n: &Neighborhood) -> BlockPos {
    let o = n.origin.origin();
    for dy in [1, 0, -1] {
        let chunk = &n.chunks[boxel_light::box_light::chunk_index(0, dy, 0)];
        if let Some(y) = (0..32)
            .rev()
            .find(|&y| !chunk.get(LocalPos::new(16, y, 16)).is_air())
        {
            return BlockPos::new(
                o.x + 16,
                o.y + i64::from(dy) * 32 + i64::from(y) + 2,
                o.z + 16,
            );
        }
    }
    BlockPos::new(o.x + 16, o.y + 16, o.z + 16)
}

fn source_registry(range: u16) -> (BlockRegistry, BlockState) {
    let mut b = boxel_demo::blocks::builder();
    boxel_voxel::testing::add_sky_source(&mut b).expect("test:sky_source");
    let registry = b.build();
    let want = SkySourceDef {
        dir: BeamDir::Down,
        range,
    };
    let state = (0..registry.len())
        .map(|i| BlockState(i as u16))
        .find(|&s| registry.sky_source(s) == Some(want))
        .expect("a source state");
    (registry, state)
}

/// Puts `source` in every air voxel of `chunk` right under a blocker (a lit cavern roof, as a
/// game's generator would): returns how many.
fn roof_sources(chunk: &mut ChunkData, tables: &LightTables, source: BlockState) -> usize {
    let mut placed = 0;
    for i in 0..CHUNK_VOLUME - 1024 {
        let p = LocalPos::from_index(i);
        if chunk.get(p).is_air() && tables.blocks_sky(chunk.get(LocalPos::from_index(i + 1024))) {
            chunk.set(p, source);
            placed += 1;
        }
    }
    placed
}

/// The beam tiles of a neighborhood's 9 columns (by `tile_index`) for its box rows, from its 27
/// chunks (every other layer air).
fn neighborhood_beams(n: &Neighborhood, tables: &LightTables) -> Vec<BeamTile> {
    let y0 = n.origin.origin().y - MARGIN;
    (0..9)
        .map(|i| {
            let (dx, dz) = (i as i64 / 3 - 1, i as i64 % 3 - 1);
            beam_tile::build(y0..=y0 + 63, &[], tables, |cy| {
                let dy = cy - n.origin.y;
                if (-1..=1).contains(&dy) {
                    LayerData::Ref(&n.chunks[((dy + 1) * 9 + (dx + 1) * 3 + dz + 1) as usize])
                } else {
                    LayerData::Air
                }
            })
        })
        .collect()
}

/// Beam scenes: a full cavern ceiling and a single sunbeam, then the beam tile of a cave column.
fn bench_beams(c: &mut Criterion) {
    let g = OverworldGenerator::default();
    let (registry, source) = source_registry(64);
    let tables = LightTables::from_registry(&registry);
    let stone = registry.by_name("boxel:stone").expect("stone");
    let mut scratch = LightScratch::new();
    let mut out = PaddedLight::default();

    // The deep chunk's tiles (tops far above), its chunks replaced by stone around a cavern 64
    // high (box rows -20..=43 from the chunk's origin), a source under the roof of every column,
    // and two 3x3 pillars; with `ledge`, a ledge at 10 across x in -8..8.
    let deep = Neighborhood::generate(&g, &registry, &tables, ChunkPos::new(0, -40, 0));
    let origin = deep.origin.origin();
    let cavern_of = |ledge: bool| {
        let mut chunks = Vec::with_capacity(27);
        for i in 0..27i64 {
            let (dy, dx, dz) = (i / 9 - 1, i / 3 % 3 - 1, i % 3 - 1);
            let mut chunk = ChunkData::uniform(stone);
            for v in 0..CHUNK_VOLUME {
                let p = LocalPos::from_index(v);
                let (x, y, z) = (
                    dx * 32 + i64::from(p.x()),
                    dy * 32 + i64::from(p.y()),
                    dz * 32 + i64::from(p.z()),
                );
                let pillar = |cx: i64, cz: i64| (x - cx).abs() <= 1 && (z - cz).abs() <= 1;
                let s = match y {
                    43 if !pillar(20, 20) && !pillar(-5, 25) => source,
                    10 if ledge && (-8..8).contains(&x) => stone,
                    -20..=42 if !pillar(20, 20) && !pillar(-5, 25) => BlockState::AIR,
                    _ => stone,
                };
                if s != stone {
                    chunk.set(p, s);
                }
            }
            chunks.push(chunk);
        }
        Neighborhood {
            origin: deep.origin,
            chunks,
            tiles: deep.tiles.clone(),
        }
    };
    let cavern = cavern_of(false);
    let ledge = cavern_of(true);
    let cavern_beams = neighborhood_beams(&cavern, &tables);
    let ledge_beams = neighborhood_beams(&ledge, &tables);
    eprintln!(
        "box_cavern_ceiling: chunk {:?} (origin y {}), {} beams in its 9 tiles",
        cavern.origin,
        origin.y,
        cavern_beams.iter().map(BeamTile::len).sum::<usize>()
    );

    // A generated cave (8% air) with one source under its roof, the nearest to the chunk's
    // center.
    let (cave, _) = lamp_cave(&g, &registry, &tables, [stone; 3]);
    let mut sunbeam = Neighborhood::generate(&g, &registry, &tables, cave.origin);
    let center = &mut sunbeam.chunks[13];
    let roof = (0..CHUNK_VOLUME - 1024)
        .filter(|&i| {
            let p = LocalPos::from_index(i);
            center.get(p).is_air() && tables.blocks_sky(center.get(LocalPos::from_index(i + 1024)))
        })
        .min_by_key(|&i| {
            let p = LocalPos::from_index(i);
            let d = |v: u32| (i64::from(v) - 16).abs();
            d(p.x()) + d(p.y()) + d(p.z())
        })
        .expect("a cave roof in the center chunk");
    center.set(LocalPos::from_index(roof), source);
    let sun_beams = neighborhood_beams(&sunbeam, &tables);
    let beam = sun_beams[tile_index(0, 0)]
        .iter()
        .next()
        .expect("the sunbeam");
    eprintln!(
        "box_sunbeam: chunk {:?}, beam {:?} cells {}..={}",
        sunbeam.origin, beam.2.def, beam.2.lo, beam.2.hi
    );

    let mut g_light = c.benchmark_group("light");
    g_light.measurement_time(Duration::from_secs(4));
    for (name, n, beams) in [
        ("box_cavern_ceiling", &cavern, &cavern_beams),
        ("box_cavern_ledge", &ledge, &ledge_beams),
        ("box_sunbeam", &sunbeam, &sun_beams),
    ] {
        let mut input = n.input(&tables);
        assert_eq!(compute(&input, &mut scratch, &mut out), LightPath::FastDark);
        input.beams = std::array::from_fn(|i| Some(&beams[i]));
        let path = compute(&input, &mut scratch, &mut out);
        assert!(
            matches!(path, LightPath::Full { .. }),
            "{name} took {path:?}"
        );
        eprintln!("{name}: {path:?}");
        g_light.bench_function(name, |b| {
            b.iter(|| compute(black_box(&input), &mut scratch, &mut out));
        });
    }
    g_light.finish();

    // The beam tile of the cave's column for the cave box: every layer it reads (the box rows
    // plus 255 on each side, 20 layers), with sources under every cave roof.
    let (cx, cz) = (cave.origin.x, cave.origin.z);
    let y0 = cave.origin.origin().y - MARGIN;
    let window = y0..=y0 + 63;
    let decorated = |cy: i64| {
        let mut chunk = g.generate(ChunkKey::new(ChunkPos::new(cx, cy, cz)), &registry);
        let placed = roof_sources(&mut chunk, &tables, source);
        (chunk, placed)
    };
    let layers: Vec<(i64, ChunkData)> = ((y0 - 255) >> 5..=(y0 + 63 + 255) >> 5)
        .rev()
        .map(|cy| (cy, decorated(cy).0))
        .collect();
    let placed: usize = layers
        .iter()
        .map(|(_, c)| c.sky_sources(&tables).len())
        .sum();
    let column = |chunks: &[(i64, ChunkData)]| {
        beam_tile::build(window.clone(), &[], &tables, |cy| {
            chunks
                .iter()
                .find(|(y, _)| *y == cy)
                .map_or(LayerData::Air, |(_, c)| LayerData::Ref(c))
        })
    };
    let fresh: Vec<(i64, ChunkData)> = layers.iter().map(|(y, c)| (*y, c.clone())).collect();
    let tile = column(&fresh);
    eprintln!(
        "beam_tile column ({cx}, {cz}): {} layers, {placed} sources, {} beams, {} B",
        layers.len(),
        tile.len(),
        tile.heap_bytes()
    );
    let mut g_beam = c.benchmark_group("beam_tile");
    g_beam.measurement_time(Duration::from_secs(4));
    // The masks were built by the first build, as by the light boxes of a loaded world.
    g_beam.bench_function("build_loaded", |b| {
        b.iter(|| column(black_box(&fresh)));
    });
    g_beam.bench_function("build_loaded_cold_masks", |b| {
        b.iter_batched(
            // A clone starts without masks.
            || {
                layers
                    .iter()
                    .map(|(y, c)| (*y, c.clone()))
                    .collect::<Vec<_>>()
            },
            |chunks| column(&chunks),
            BatchSize::LargeInput,
        );
    });
    g_beam.bench_function("build_generate", |b| {
        b.iter(|| {
            beam_tile::build(window.clone(), &[], &tables, |cy| {
                LayerData::Owned(decorated(black_box(cy)).0)
            })
        });
    });
    g_beam.finish();
}

criterion_group!(benches, bench, bench_beams);
criterion_main!(benches);
