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

mod scenes;

use boxel_demo::worldgen::OverworldGenerator;
use boxel_light::box_light::tile_index;
use boxel_light::sky_tile::{DEFAULT_WALK_LIMIT, GeneratorTop, walk, walk_floor};
use boxel_light::{LayerData, LightPath, LightScratch, PaddedLight, compute};
use boxel_voxel::{ChunkData, ChunkKey, ChunkPos, LightTables};
use boxel_worldgen::WorldGenerator;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use scenes::{
    Neighborhood, generated_tile, lamp_cave, lamp_registry, surface_origin, trees_and_entrance,
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
