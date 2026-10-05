//! Meshing benchmarks: gather + mesh of terrain-like, caves and checkerboard chunks.
//!
//! Targets (informational in M0): `mesh` of a terrain-like padded chunk < 60 us,
//! gather + mesh < 150 us (release).
#![expect(clippy::print_stderr, reason = "bench scene statistics")]

use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, Mesher, PaddedChunk};
use boxel_voxel::{BlockRegistry, CHUNK_SIZE, ChunkData, NEIGHBOR_OFFSETS};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

#[path = "../tests/common/mod.rs"]
mod common;

use common::{Repr, chunk_repr, dense_from};

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

    fn input(&self) -> MeshInput<'_> {
        MeshInput {
            center: &self.center,
            neighbors: std::array::from_fn(|i| Some(&self.neighbors[i])),
        }
    }
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

    let mut mesher = Mesher::new();
    let mut padded = PaddedChunk::new();
    let mut out = ChunkMesh::default();
    for (name, scene) in [
        ("terrain", &terrain_packed),
        ("caves", &cave),
        ("checkerboard", &checker),
    ] {
        mesher.gather(&scene.input(), &table, &mut padded);
        mesher.mesh(&padded, &table, &mut out);
        eprintln!(
            "scene {name}: {} quads, buckets {:?}",
            out.total_quads(),
            out.bucket_counts
        );
        c.bench_function(&format!("mesh/{name}"), |bench| {
            bench.iter(|| {
                mesher.mesh(black_box(&padded), &table, &mut out);
                black_box(out.total_quads())
            });
        });
    }
    for (name, scene) in [
        ("terrain_packed", &terrain_packed),
        ("terrain_direct", &terrain_direct),
    ] {
        let input = scene.input();
        c.bench_function(&format!("gather/{name}"), |bench| {
            bench.iter(|| mesher.gather(black_box(&input), &table, &mut padded));
        });
        c.bench_function(&format!("gather_mesh/{name}"), |bench| {
            bench.iter(|| black_box(mesher.mesh_chunk(black_box(&input), &table)));
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
