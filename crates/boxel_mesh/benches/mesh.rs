//! Meshing benchmarks: gather + mesh of terrain-like, caves and checkerboard chunks, and of
//! chunks of `OverworldGenerator`: a deep cave chunk (1-bit stone and air with its 26
//! neighbors), the same kind of cave lit by colored lamps, and the surface chunk near the origin
//! with trees and a cave entrance (the scenes of the `boxel_light` benches). Quad counts and
//! palette sizes are printed.
//!
//! Targets (release, idle machine; ADR 0015, phase P2):
//!
//! | Bench | Target |
//! |---|---|
//! | `mesh/cave` (AO off, no light: the bgm path) | 1.15x of 28.8 us or less |
//! | `gather_mesh/cave` (dark uniform light, AO on lit faces only) | 1.1x of 91.5 us or less |
//! | `gather_mesh/cave_lit` (lamps, light computed beforehand) | 160 us or less |
//! | `gather_mesh/surface` (sky light and AO keys, light computed beforehand) | 150 us or less |
//!
//! The other `mesh/*`, `gather/*` and `gather_mesh/terrain_*` benches keep the M0 setup (AO
//! off, no light) to compare with earlier runs; `mesh/terrain_baked` adds AO to every face.
//! `mesh/shapes` and `gather_mesh/shapes` put stairs, fences, torches and slabs on a quarter of
//! the terrain's surface cells (ADR 0020).
#![expect(clippy::print_stderr, reason = "bench scene statistics")]

use boxel_light::{LightScratch, PaddedLight, compute};
use boxel_mesh::{AoPolicy, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher, PaddedChunk};
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_SIZE, ChunkData, ChunkKey, ChunkPos, LightTables, LightValue,
    NEIGHBOR_OFFSETS,
};
use boxel_worldgen::{OverworldGenerator, WorldGenerator};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

#[path = "../tests/common/mod.rs"]
mod common;
#[path = "../../boxel_light/benches/scenes/mod.rs"]
mod scenes;

use common::{Repr, chunk_repr, dense_from};
use scenes::Neighborhood;

/// AO off: quads as by material alone (the M0 mesher).
const OFF: MeshOptions = MeshOptions::with_ao(AoPolicy::Off);
/// The default: AO on faces with some light.
const LIT_ONLY: MeshOptions = MeshOptions::with_ao(AoPolicy::BakedLitOnly);

/// Block ids of the default registry used by the scenes.
struct Blocks {
    stone: u16,
    dirt: u16,
    grass: u16,
    sand: u16,
    gravel: u16,
    log: u16,
    leaves: u16,
    water: u16,
}

impl Blocks {
    fn new(registry: &BlockRegistry) -> Self {
        let id = |name: &str| {
            registry
                .by_name(&format!("boxel:{name}"))
                .expect("built-in block")
                .0
        };
        Self {
            stone: id("stone"),
            dirt: id("dirt"),
            grass: id("grass"),
            sand: id("sand"),
            gravel: id("gravel"),
            log: id("log"),
            leaves: id("leaves"),
            water: id("water"),
        }
    }
}

/// Integer hash of a world position (deterministic scatter for trees and gravel).
fn hash(x: i32, y: i32, z: i32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ (z as u32).wrapping_mul(0xCB1A_B31F);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^ (h >> 12)
}

const SEA_LEVEL: i32 = 42;

fn height(x: i32, z: i32) -> i32 {
    let (fx, fz) = (x as f32, z as f32);
    (44.0
        + 6.0 * (fx * 0.11).sin() * (fz * 0.07).cos()
        + 3.0 * ((fx + fz) * 0.21).sin()
        + 1.5 * (fz * 0.37).cos()) as i32
}

fn is_tree(x: i32, z: i32) -> bool {
    hash(x, 0, z).is_multiple_of(61) && height(x, z) > SEA_LEVEL + 1
}

/// Rolling terrain around y = 44: stone with gravel specks, dirt, grass or sand, sea at y = 42
/// (translucent water), trees with log trunks and leaf crowns (cutout).
fn terrain(b: &Blocks, x: i32, y: i32, z: i32) -> u16 {
    let h = height(x, z);
    if y < h - 3 {
        return if hash(x, y, z).is_multiple_of(23) {
            b.gravel
        } else {
            b.stone
        };
    }
    if y < h {
        return b.dirt;
    }
    if y == h {
        return if h <= SEA_LEVEL + 1 { b.sand } else { b.grass };
    }
    for dx in -2..=2 {
        for dz in -2..=2 {
            if !is_tree(x + dx, z + dz) {
                continue;
            }
            let base = height(x + dx, z + dz);
            if dx == 0 && dz == 0 && (base + 1..=base + 4).contains(&y) {
                return b.log;
            }
            if (base + 3..=base + 6).contains(&y) && dx.abs() + dz.abs() <= 3 {
                return b.leaves;
            }
        }
    }
    if y <= SEA_LEVEL { b.water } else { 0 }
}

/// Stone carved by a 3D trigonometric cave field.
fn caves(b: &Blocks, x: i32, y: i32, z: i32) -> u16 {
    let (fx, fy, fz) = (x as f32, y as f32, z as f32);
    let field = (fx * 0.21).sin() * (fy * 0.27).cos() * (fz * 0.18).sin()
        + 0.5 * ((fx + fy) * 0.11).cos() * ((fz - fy) * 0.13).sin();
    if field > 0.25 { 0 } else { b.stone }
}

/// Center chunk plus its 26 neighbors, sampled from a world function.
struct Scene {
    center: ChunkData,
    neighbors: Vec<ChunkData>,
}

impl Scene {
    fn new(chunk: [i32; 3], repr: Repr, world: impl Fn(i32, i32, i32) -> u16) -> Self {
        let sample = |c: [i32; 3]| {
            let origin = c.map(|v| v * CHUNK_SIZE);
            let dense = dense_from(|x, y, z| world(origin[0] + x, origin[1] + y, origin[2] + z));
            if dense.iter().all(|s| *s == dense[0]) {
                ChunkData::uniform(dense[0])
            } else {
                chunk_repr(&dense, repr)
            }
        };
        let neighbors = NEIGHBOR_OFFSETS
            .iter()
            .map(|o| sample([chunk[0] + o.x, chunk[1] + o.y, chunk[2] + o.z]))
            .collect();
        Self {
            center: sample(chunk),
            neighbors,
        }
    }

    /// The chunk at `pos` and its neighbors from a generator.
    fn generated(g: &dyn WorldGenerator, registry: &BlockRegistry, pos: ChunkPos) -> Self {
        let generate = |p: ChunkPos| g.generate(ChunkKey::new(p), registry);
        let neighbors = NEIGHBOR_OFFSETS
            .iter()
            .map(|o| {
                generate(ChunkPos::new(
                    pos.x + i64::from(o.x),
                    pos.y + i64::from(o.y),
                    pos.z + i64::from(o.z),
                ))
            })
            .collect();
        Self {
            center: generate(pos),
            neighbors,
        }
    }

    /// The center and its 26 neighbors of a light neighborhood (27 chunks, center at 13).
    fn from_neighborhood(n: &Neighborhood) -> Self {
        let mut chunks = n.chunks.clone();
        let center = chunks.remove(13);
        Self {
            center,
            neighbors: chunks,
        }
    }

    fn input(&self) -> MeshInput<'_> {
        self.lit(None)
    }

    fn lit<'a>(&'a self, light: Option<&'a PaddedLight>) -> MeshInput<'a> {
        MeshInput {
            center: &self.center,
            neighbors: std::array::from_fn(|i| Some(&self.neighbors[i])),
            light,
        }
    }
}

/// The light of a neighborhood's center chunk, computed once.
fn light_of(n: &Neighborhood, tables: &LightTables) -> PaddedLight {
    let mut out = PaddedLight::default();
    let path = compute(&n.input(tables), &mut LightScratch::new(), &mut out);
    eprintln!("light of {:?}: {path:?}", n.origin);
    out
}

fn bench(c: &mut Criterion) {
    let registry = BlockRegistry::default_blocks();
    let table = MaterialTable::from_registry(&registry);
    let b = Blocks::new(&registry);
    let surface = [0, 1, 0];
    let terrain_packed = Scene::new(surface, Repr::Packed, |x, y, z| terrain(&b, x, y, z));
    let terrain_direct = Scene::new(surface, Repr::Direct, |x, y, z| terrain(&b, x, y, z));
    let cave = Scene::new([0, 0, 0], Repr::Packed, |x, y, z| caves(&b, x, y, z));
    let stone = b.stone;
    let checker = Scene::new([0, 0, 0], Repr::Direct, move |x, y, z| {
        if (x + y + z) % 2 == 0 { stone } else { 0 }
    });

    // A deep overworld chunk with caves: the first one along +x with at least 5% air.
    let overworld = OverworldGenerator::default();
    let air_share = |data: &ChunkData| {
        let mut d = vec![BlockState::AIR; boxel_voxel::CHUNK_VOLUME];
        data.to_dense(&mut d);
        d.iter().filter(|s| **s == BlockState::AIR).count() as f64 / d.len() as f64
    };
    let cave_pos = (0..4096)
        .map(|x| ChunkPos::new(x, -10, 0))
        .find(|&p| air_share(&overworld.generate(ChunkKey::new(p), &registry)) >= 0.05)
        .expect("a cave chunk");
    let generated = Scene::generated(&overworld, &registry, cave_pos);

    let mut mesher = Mesher::new();
    let mut padded = PaddedChunk::new();
    let mut out = ChunkMesh::default();
    for (name, scene, options) in [
        ("terrain", &terrain_packed, OFF),
        (
            "terrain_baked",
            &terrain_packed,
            MeshOptions::with_ao(AoPolicy::Baked),
        ),
        ("caves", &cave, OFF),
        ("checkerboard", &checker, OFF),
        ("cave", &generated, OFF),
    ] {
        mesher.gather(&scene.input(), &table, &mut padded);
        mesher.mesh(&padded, None, &table, options, &mut out);
        eprintln!(
            "scene {name}: {} quads, buckets {:?}",
            out.total_quads(),
            out.bucket_counts
        );
        c.bench_function(&format!("mesh/{name}"), |bench| {
            bench.iter(|| {
                mesher.mesh(black_box(&padded), None, &table, options, &mut out);
                black_box(out.total_quads())
            });
        });
    }
    eprintln!("scene cave: overworld chunk {cave_pos:?}");

    // Lit scenes: the surface near the origin (trees and a cave entrance) and a cave with
    // lamps, both with the real light of `boxel_light` (computed once, outside the timing).
    let (lamp_blocks, lamps) = scenes::lamp_registry();
    let lamp_table = MaterialTable::from_registry(&lamp_blocks);
    let tables = LightTables::from_registry(&lamp_blocks);
    let surface_n = Neighborhood::generate(
        &overworld,
        &lamp_blocks,
        &tables,
        scenes::surface_origin(&overworld, &lamp_blocks, &tables),
    );
    let (cave_n, placed) = scenes::lamp_cave(&overworld, &lamp_blocks, &tables, lamps);
    eprintln!("scene cave_lit: {placed} lamps around {:?}", cave_n.origin);
    let surface = Scene::from_neighborhood(&surface_n);
    let surface_light = light_of(&surface_n, &tables);
    let cave_lit = Scene::from_neighborhood(&cave_n);
    let cave_light = light_of(&cave_n, &tables);
    let dark = PaddedLight::Uniform(LightValue::DARK);

    for (name, scene, light, table, options) in [
        ("terrain_packed", &terrain_packed, None, &table, OFF),
        ("terrain_direct", &terrain_direct, None, &table, OFF),
        ("cave", &generated, Some(&dark), &table, LIT_ONLY),
        (
            "cave_lit",
            &cave_lit,
            Some(&cave_light),
            &lamp_table,
            LIT_ONLY,
        ),
        (
            "surface",
            &surface,
            Some(&surface_light),
            &lamp_table,
            LIT_ONLY,
        ),
    ] {
        let input = scene.lit(light);
        let mesh = mesher.mesh_chunk(&input, table, options);
        eprintln!(
            "scene {name}: {} quads, palette {} ({:?})",
            mesh.total_quads(),
            mesh.palette.len(),
            mesh.light_quantization
        );
        c.bench_function(&format!("gather/{name}"), |bench| {
            bench.iter(|| mesher.gather(black_box(&input), table, &mut padded));
        });
        c.bench_function(&format!("gather_mesh/{name}"), |bench| {
            bench.iter(|| black_box(mesher.mesh_chunk(black_box(&input), table, options)));
        });
    }

    // The mesh step alone on the surface chunk: without light and AO (the M0 path), with AO on
    // every face, and lit (the light keys).
    mesher.gather(&surface.input(), &lamp_table, &mut padded);
    for (name, light, options) in [
        ("surface_unlit", None, OFF),
        ("surface_baked", None, MeshOptions::with_ao(AoPolicy::Baked)),
        ("surface", Some(&surface_light), LIT_ONLY),
    ] {
        mesher.mesh(&padded, light, &lamp_table, options, &mut out);
        let faces: u32 = out.quads.iter().map(|q| q.w() * q.h()).sum();
        eprintln!(
            "scene {name}: {} quads, {faces} unit faces",
            out.total_quads()
        );
        c.bench_function(&format!("mesh/{name}"), |bench| {
            bench.iter(|| {
                mesher.mesh(black_box(&padded), light, &lamp_table, options, &mut out);
                black_box(out.total_quads())
            });
        });
    }

    // ADR 0020: the terrain with a block made of boxes on one surface cell in four (stairs of
    // every shape, fences, torches, slabs): the per-block path at its busiest.
    let shaped: Vec<u16> = (0..registry.len() as u16)
        .filter(|&s| !registry.model(BlockState(s)).boxes().is_empty())
        .collect();
    let shapes = Scene::new([0, 1, 0], Repr::Packed, |x, y, z| {
        let h = height(x, z);
        let k = hash(x, 7, z);
        if y == h + 1 && h > SEA_LEVEL && k.is_multiple_of(4) && terrain(&b, x, y, z) == 0 {
            shaped[(k / 4) as usize % shaped.len()]
        } else {
            terrain(&b, x, y, z)
        }
    });
    let input = shapes.input();
    let mesh = mesher.mesh_chunk(&input, &table, OFF);
    let specials = mesh
        .quads
        .iter()
        .filter(|q| q.shape() == boxel_mesh::layout::SHAPE_BOX)
        .count();
    eprintln!(
        "scene shapes: {} quads, {specials} box faces",
        mesh.total_quads()
    );
    mesher.gather(&input, &table, &mut padded);
    c.bench_function("mesh/shapes", |bench| {
        bench.iter(|| {
            mesher.mesh(black_box(&padded), None, &table, OFF, &mut out);
            black_box(out.total_quads())
        });
    });
    c.bench_function("gather_mesh/shapes", |bench| {
        bench.iter(|| black_box(mesher.mesh_chunk(black_box(&input), &table, OFF)));
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
