//! The far LOD's world side with the demo forest's own LOD source (ADR 0031): the plugin in an
//! `App` with the real generator, materials and mesher, on both executors. The nodes it sends
//! are checked against a fresh column job of the same source.
use boxel_app::App;
use boxel_demo::worldgen::ForestGenerator;
use boxel_math::DVec3;
use boxel_mesh::lod::{LodMaterials, mesh_node};
use boxel_mesh::{ChunkMesh, Mesher, PaddedChunk};
use boxel_transform::Transform;
use boxel_voxel::{ChunkKey, DimensionId};
use boxel_world::jobs::{InlineExecutor, JobExecutor, TaskPoolExecutor};
use boxel_world::lod::{LodOp, LodOps, LodSettings, LodStats, LodWorldPlugin};
use boxel_world::{ChunkViewer, WorldPlugin, WorldSettings};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{LodNode, LodRequest, LodScratch};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn forest_app(executor: Arc<dyn JobExecutor>) -> App {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(ForestGenerator::default()),
        registry: Arc::new(boxel_demo::blocks::registry()),
        settings: WorldSettings::default(),
        executor: Arc::new(InlineExecutor),
    });
    app.add_plugins(LodWorldPlugin {
        settings: LodSettings {
            ring_radius: 8,
            levels: 3,
            frame_budget: Duration::from_millis(50),
            ..LodSettings::default()
        },
        executor,
    });
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(248.0, 90.0, -256.0)),
        ChunkViewer::new(2).with_vertical_radius(2),
    ));
    app
}

/// Updates until the LOD is filled, replaying its ops as a renderer would.
fn fill(app: &mut App) -> BTreeMap<ChunkKey, (Arc<ChunkMesh>, u8)> {
    let mut nodes = BTreeMap::new();
    let start = Instant::now();
    loop {
        app.update();
        for op in app.world_mut().resource_mut::<LodOps>().take() {
            match op {
                LodOp::Upsert { key, mesh, mask } => {
                    nodes.insert(key, (mesh, mask));
                }
                LodOp::Mask { key, mask } => {
                    nodes.get_mut(&key).expect("a sent node").1 = mask;
                }
                LodOp::Remove { key } => {
                    nodes.remove(&key).expect("a sent node");
                }
            }
        }
        let stats = app.world().resource::<LodStats>();
        if stats.fill >= 1.0 && stats.in_flight == 0 && stats.sends_waiting == 0 {
            return nodes;
        }
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "not filled: {stats:?}"
        );
        std::thread::yield_now();
    }
}

/// The mesh a fresh column job of the forest's source gives node `key`.
fn fresh_mesh(key: ChunkKey) -> Option<ChunkMesh> {
    let registry = boxel_demo::blocks::registry();
    let forest = ForestGenerator::default();
    let source = forest.lod().expect("the forest has a LOD source");
    let materials = LodMaterials::from_registry(&registry);
    let (mut scratch, mut padded) = (LodScratch::default(), PaddedChunk::new());
    let (mut mesher, mut out) = (Mesher::new(), None);
    let req = LodRequest {
        dim: DimensionId(0),
        level: key.lod,
        column: [key.pos.x, key.pos.z],
        layers: key.pos.y..=key.pos.y,
    };
    source.column(&req, &registry, &mut scratch, &mut |y, node| {
        if let (true, LodNode::Cells(cells)) = (y == key.pos.y, node) {
            let mut mesh = ChunkMesh::default();
            materials.fill(&cells.0[..], &mut padded);
            mesh_node(&mut mesher, &padded, &materials, key.lod, &mut mesh);
            out = Some(mesh);
        }
    });
    out
}

#[test]
fn the_forest_fills_its_rings_with_the_meshes_of_its_source() {
    for executor in [
        Arc::new(InlineExecutor) as Arc<dyn JobExecutor>,
        Arc::new(TaskPoolExecutor),
    ] {
        let mut app = forest_app(executor);
        let nodes = fill(&mut app);
        let stats = app.world().resource::<LodStats>().clone();
        assert_eq!(stats.off, None);
        assert_eq!(stats.horizon, 2048.0);
        assert_eq!(nodes.len(), stats.nodes());
        for level in &stats.levels {
            assert!(level.nodes > 0, "{stats:?}");
            assert_eq!(
                level.floor_nodes, 0,
                "no node meshed for the floor: {stats:?}"
            );
            assert_eq!(level.failed, 0);
        }
        // Sample nodes of every level against a fresh job.
        for level in 1..=3u8 {
            for (key, (mesh, _)) in nodes.iter().filter(|(k, _)| k.lod == level).take(4) {
                let fresh = fresh_mesh(*key).expect("the node has cells");
                assert_eq!(mesh.quads, fresh.quads, "{key:?}");
                assert_eq!(mesh.bucket_counts, fresh.bucket_counts, "{key:?}");
            }
        }
    }
}
