//! Far level of detail benchmark (ADR 0031): `lod/column_l{1,4}`, one column job of the forest's
//! coarse terrain as the engine runs it (the source call, then for every node with cells the
//! fill, the mesher and the post-passes), at levels 1 and 4, and `lod/source_l{1,4}`, the source
//! call alone. The column holds the lake near the origin (around (248, -256)): ground, trees, a
//! beach and the sea.
#![expect(clippy::print_stderr, reason = "bench scene statistics")]
use boxel_demo::worldgen::ForestGenerator;
use boxel_mesh::lod::{LodMaterials, mesh_node};
use boxel_mesh::{ChunkMesh, Mesher, PaddedChunk};
use boxel_voxel::DimensionId;
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{LodNode, LodRequest, LodScratch};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench(c: &mut Criterion) {
    let registry = boxel_demo::blocks::registry();
    let forest = ForestGenerator::default();
    let source = forest.lod().expect("the forest has a LOD source");
    let materials = LodMaterials::from_registry(&registry);
    let mut scratch = LodScratch::default();
    let mut padded = PaddedChunk::new();
    let mut mesher = Mesher::new();
    let mut mesh = ChunkMesh::default();
    let mut g = c.benchmark_group("lod");
    for level in [1u8, 4] {
        let shift = 5 + u32::from(level);
        let req = LodRequest {
            dim: DimensionId(0),
            level,
            column: [248 >> shift, -256 >> shift],
            layers: -8..=8,
        };
        // Returns the meshed nodes and their quads.
        let mut job = |req: &LodRequest| {
            let (mut nodes, mut quads) = (0, 0);
            source.column(req, &registry, &mut scratch, &mut |_, node| {
                if let LodNode::Cells(cells) = node {
                    materials.fill(&cells.0[..], &mut padded);
                    mesh_node(&mut mesher, &padded, &materials, level, &mut mesh);
                    nodes += 1;
                    quads += mesh.quads.len();
                }
            });
            (nodes, quads)
        };
        let (nodes, quads) = job(&req);
        assert!(nodes > 0, "level {level}: no node to mesh");
        eprintln!("lod/column_l{level}: {nodes} nodes meshed, {quads} quads");
        g.bench_function(format!("column_l{level}"), |b| {
            b.iter(|| job(black_box(&req)));
        });
        g.bench_function(format!("source_l{level}"), |b| {
            b.iter(|| {
                let mut cells = 0;
                source.column(black_box(&req), &registry, &mut scratch, &mut |_, node| {
                    cells += usize::from(matches!(node, LodNode::Cells(_)));
                });
                cells
            });
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
