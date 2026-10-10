//! G3 of #48: what the mesher makes of the built-in content. The material table of the
//! registry (model boxes, class, fluid level, water, shape, layers, buckets, emission), the
//! mesh of a showcase chunk with every state of the registry on a grid, and the meshes of two
//! chunks each (surface and underground) of the flat, terrain, forest and overworld (with its
//! caves) worlds. Frozen before the content moved out of the engine: the literal never changes,
//! only the setup lines below may.

#[path = "../../boxel_voxel/tests/golden/mod.rs"]
mod golden;

use boxel_mesh::{MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_voxel::{
    BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, Face, LocalPos, NEIGHBOR_OFFSETS,
};
use boxel_worldgen::{
    FlatGenerator, ForestGenerator, OverworldGenerator, OverworldSettings, TerrainGenerator,
    TerrainSettings, WorldGenerator,
};
use std::fmt::Write as _;

/// The seed of the generated worlds (the examples' default).
const SEED: u64 = 0x00b0_7e1d;

/// The registry under test.
fn registry() -> BlockRegistry {
    BlockRegistry::default_blocks()
}

/// The generators: the examples' flat world, the terrain, the forest and the overworld with its
/// default caves.
fn generators() -> Vec<(&'static str, Box<dyn WorldGenerator>)> {
    vec![
        (
            "flat",
            Box::new(FlatGenerator::new(vec![
                ("boxel:bedrock".to_string(), 1),
                ("boxel:stone".to_string(), 59),
                ("boxel:dirt".to_string(), 3),
                ("boxel:grass".to_string(), 1),
            ])),
        ),
        (
            "terrain",
            Box::new(TerrainGenerator::new(SEED, TerrainSettings::default())),
        ),
        (
            "forest",
            Box::new(ForestGenerator::new(SEED, TerrainSettings::default())),
        ),
        (
            "overworld",
            Box::new(OverworldGenerator::new(SEED, OverworldSettings::default())),
        ),
    ]
}

/// Every state of `r` on a grid: a sparse one (every other cell on each axis, so each block
/// stands alone) in the lower half, and two dense layers (each state next to others, in two
/// orders) in the upper half.
pub fn showcase(r: &BlockRegistry) -> ChunkData {
    let mut chunk = ChunkData::uniform(BlockState::AIR);
    let n = r.len();
    for i in 0..n {
        let s = BlockState(i as u16);
        let (x, z, y) = ((i % 16) * 2, (i / 16 % 16) * 2, (i / 256) * 2);
        chunk.set(LocalPos::new(x as u32, y as u32, z as u32), s);
        let (x, z) = (i % 32, i / 32 % 32);
        chunk.set(LocalPos::new(x as u32, 20 + (i / 1024) as u32, z as u32), s);
        let j = (i * 7 + 3) % n;
        chunk.set(
            LocalPos::new(x as u32, 21 + (i / 1024) as u32, z as u32),
            BlockState(j as u16),
        );
    }
    chunk
}

fn mesh_line(
    d: &mut String,
    what: &str,
    mesher: &mut Mesher,
    input: &MeshInput<'_>,
    m: &MaterialTable,
) {
    let mesh = mesher.mesh_chunk(input, m, MeshOptions::default());
    let _ = writeln!(
        d,
        "{what} quads={} buckets={:?} palette={} hash={:#018x}",
        mesh.quads.len(),
        mesh.bucket_counts,
        mesh.palette.len(),
        mesh.compute_hash(),
    );
}

fn dump(r: &BlockRegistry) -> String {
    let mut d = String::new();
    let m = MaterialTable::from_registry(r);
    for (i, b) in m.model_boxes().iter().enumerate() {
        let _ = writeln!(
            d,
            "box {i} {:?}-{:?}@{:?}",
            b.bounds.min, b.bounds.max, b.texture_offset
        );
    }
    for i in 0..r.len() as u16 {
        let layers: Vec<u16> = Face::ALL.iter().map(|&f| m.layer(i, f)).collect();
        let buckets: Vec<usize> = Face::ALL.iter().map(|&f| m.bucket(i, f)).collect();
        let _ = writeln!(
            d,
            "material {i} class={:?} fluid={:?} wet={} shape={:?} layers={layers:?} \
             buckets={buckets:?} emissive={}",
            m.class(i),
            m.fluid_level(i),
            m.is_waterlogged(i),
            m.shape(i),
            m.emissive(i),
        );
    }
    let mut mesher = Mesher::new();
    let show = showcase(r);
    let alone = MeshInput {
        center: &show,
        neighbors: [None; 26],
        light: None,
    };
    mesh_line(&mut d, "showcase", &mut mesher, &alone, &m);
    for (name, g) in generators() {
        let surface = match name {
            "overworld" => {
                let o = OverworldGenerator::new(SEED, OverworldSettings::default());
                o.height_at(16, 16) >> 5
            }
            "flat" => 1,
            _ => 2,
        };
        for cy in [surface, -2] {
            let origin = ChunkPos::new(0, cy, 0);
            let center = g.generate(ChunkKey::new(origin), r);
            let around: Vec<ChunkData> = NEIGHBOR_OFFSETS
                .iter()
                .map(|o| {
                    let p = ChunkPos::new(
                        origin.x + i64::from(o.x),
                        origin.y + i64::from(o.y),
                        origin.z + i64::from(o.z),
                    );
                    g.generate(ChunkKey::new(p), r)
                })
                .collect();
            let mut neighbors = [None; 26];
            for (slot, c) in neighbors.iter_mut().zip(&around) {
                *slot = Some(c);
            }
            let input = MeshInput {
                center: &center,
                neighbors,
                light: None,
            };
            mesh_line(&mut d, &format!("{name} y={cy}"), &mut mesher, &input, &m);
        }
    }
    d
}

#[test]
fn the_meshes_of_the_content_are_frozen() {
    golden::check("mesh", &dump(&registry()), 0x5b12_bf92_b5fd_cace);
}
