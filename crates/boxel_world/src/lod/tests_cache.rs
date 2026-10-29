//! The far LOD with the generation cache (ADR 0042): a column read back from the cache builds
//! exactly what its source builds, with edits laid over it as ever, at any level and far from
//! the origin; a world that comes back with a full cache draws the same nodes without asking
//! the source.
use super::edits::{Pyramid, nodes_touching};
use super::jobs::{self, Built, JobInput, LodMessage};
use super::tests::{Model, Steps, fill, small, spawn};
use super::*;
use crate::WorldPlugin;
use crate::jobs::InlineExecutor;
use crate::persist::{ChunkStore, GenCache, GenCacheSettings, MemoryStore, PersistPlugin};
use boxel_math::DVec3;
use boxel_mesh::lod::LodMaterials;
use boxel_voxel::lod::LodClasses;
use boxel_voxel::{BlockRegistry, ChunkData, MAX_CHUNK_COORD};
use boxel_worldgen::WorldGenerator;
use std::sync::atomic::AtomicBool;

fn input(
    generator: Arc<dyn WorldGenerator>,
    registry: &Arc<BlockRegistry>,
    req: LodRequest,
    overlays: Vec<(ChunkPos, Arc<Pyramid>)>,
    cache: Option<&GenCache>,
) -> JobInput {
    let edit_layers = overlays
        .iter()
        .map(|(c, _)| nodes_touching(c.y, req.level))
        .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)));
    JobInput {
        id: 1,
        cache: cache.cloned(),
        generator,
        registry: Arc::clone(registry),
        materials: Arc::new(LodMaterials::from_registry(registry)),
        request: req,
        only: None,
        cancel: Arc::new(AtomicBool::new(false)),
        overlays,
        edit_layers,
        fallback: None,
    }
}

/// The two messages are the same column built the same way.
fn assert_same(a: &LodMessage, b: &LodMessage, what: &str) {
    let (
        LodMessage::Built {
            column: ca,
            nodes: na,
            ..
        },
        LodMessage::Built {
            column: cb,
            nodes: nb,
            ..
        },
    ) = (a, b)
    else {
        panic!("{what}: not two built columns: {a:?} {b:?}");
    };
    assert_eq!(ca, cb, "{what}: band");
    assert_eq!(na.len(), nb.len(), "{what}: nodes");
    for ((la, built_a), (lb, built_b)) in na.iter().zip(nb) {
        assert_eq!(la, lb, "{what}: order");
        let same = match (built_a, built_b) {
            (Built::Uniform, Built::Uniform)
            | (Built::Blank, Built::Blank)
            | (Built::TooBig, Built::TooBig) => true,
            (Built::Mesh(ma, oa), Built::Mesh(mb, ob)) => ma == mb && oa == ob,
            _ => false,
        };
        assert!(same, "{what}: layer {la}: {built_a:?} against {built_b:?}");
    }
}

/// Runs `input` without the cache, then with it twice (a miss that records, a hit), and checks
/// all three build the same.
fn check_column(
    generator: Arc<dyn WorldGenerator>,
    registry: &Arc<BlockRegistry>,
    req: LodRequest,
    overlays: Vec<(ChunkPos, Arc<Pyramid>)>,
    cache: &GenCache,
) {
    let what = format!("{} {req:?}", generator.name());
    let plain = jobs::run_column(&input(
        Arc::clone(&generator),
        registry,
        req.clone(),
        overlays.clone(),
        None,
    ));
    let before = cache.stats();
    let first = jobs::run_column(&input(
        Arc::clone(&generator),
        registry,
        req.clone(),
        overlays.clone(),
        Some(cache),
    ));
    cache.flush().expect("flush");
    let second = jobs::run_column(&input(generator, registry, req, overlays, Some(cache)));
    let after = cache.stats();
    assert_eq!(after.lod_misses - before.lod_misses, 1, "{what}");
    assert_eq!(after.lod_hits - before.lod_hits, 1, "{what}");
    assert_same(&plain, &first, &format!("{what}, recording"));
    assert_same(&plain, &second, &format!("{what}, from the cache"));
}

#[test]
fn a_cached_column_builds_what_the_source_builds() {
    let registry = Arc::new(boxel_voxel::testing::registry());
    let cache = GenCache::in_memory(&registry, GenCacheSettings::default()).expect("cache");
    let steps: Arc<dyn WorldGenerator> = Arc::new(Steps::new(20));
    for level in 1..=4u8 {
        for &x in &[0, -3, 1 << 20, -(MAX_CHUNK_COORD >> level)] {
            let req = LodRequest {
                dim: DimensionId(0),
                level,
                column: [x, x / 2],
                layers: -3..=3,
            };
            check_column(Arc::clone(&steps), &registry, req, Vec::new(), &cache);
        }
    }
}

#[test]
fn edits_go_over_a_cached_column() {
    let registry = Arc::new(boxel_voxel::testing::registry());
    let classes = LodClasses::from_registry(&registry);
    let cache = GenCache::in_memory(&registry, GenCacheSettings::default()).expect("cache");
    let steps: Arc<dyn WorldGenerator> = Arc::new(Steps::new(20));
    let solid = registry.by_name("test:solid").expect("test:solid");
    // A block of stone high in the air and a hole dug in the ground, in the column's nodes.
    let tower = Arc::new(Pyramid::of_chunk(&classes, &ChunkData::uniform(solid)));
    let hole = Arc::new(Pyramid::of_chunk(
        &classes,
        &ChunkData::uniform(boxel_voxel::BlockState::AIR),
    ));
    let overlays = vec![
        (ChunkPos::new(1, 3, 0), Arc::clone(&tower)),
        (ChunkPos::new(0, 0, 1), Arc::clone(&hole)),
    ];
    for level in 1..=3u8 {
        let req = LodRequest {
            dim: DimensionId(0),
            level,
            column: [0, 0],
            layers: -3..=3,
        };
        check_column(Arc::clone(&steps), &registry, req, overlays.clone(), &cache);
    }
}

/// A world of the steps terrain saved in `backend`, with `cache` attached, its LOD on.
fn cached_app(backend: &Arc<MemoryStore>, cache: &GenCache, at: DVec3) -> App {
    let steps = Steps::new(20);
    let registry = Arc::new(boxel_voxel::testing::registry());
    let store = WorldStore::open(
        Arc::clone(backend) as Arc<dyn ChunkStore>,
        &registry,
        steps.name(),
    )
    .expect("the world opens");
    store.attach_cache(cache.clone()).expect("one cache");
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(steps),
        registry,
        settings: WorldSettings {
            vertical_bounds: Some(-2..=3),
            ..WorldSettings::default()
        },
        executor: Arc::new(InlineExecutor),
    })
    .add_plugins(PersistPlugin::new(store).with_executor(Arc::new(InlineExecutor)))
    .add_plugins(LodWorldPlugin {
        settings: small(),
        executor: Arc::new(InlineExecutor),
    });
    spawn(&mut app, at);
    app
}

/// Every node the tree drew, with its mesh.
fn drawn(app: &App) -> Vec<(ChunkKey, ChunkMesh)> {
    let lod = app.world().resource::<LodWorld>();
    let mut out: Vec<(ChunkKey, ChunkMesh)> = lod
        .tree
        .columns
        .keys()
        .flat_map(|c| {
            let column = &lod.tree.columns[c];
            column
                .nodes
                .keys()
                .copied()
                .collect::<Vec<_>>()
                .into_iter()
                .filter_map(move |y| {
                    let key = ChunkKey {
                        dim: lod.tree.dim,
                        lod: c.level,
                        pos: ChunkPos::new(c.x, y, c.z),
                    };
                    match &lod.tree.node(key)?.state {
                        tree::NodeState::Ready { mesh, .. } => Some((key, (**mesh).clone())),
                        _ => None,
                    }
                })
        })
        .collect();
    out.sort_by_key(|(k, _)| (k.lod, k.pos.x, k.pos.y, k.pos.z));
    out
}

#[test]
fn a_world_back_with_a_full_cache_draws_the_same_far_nodes() {
    let registry = boxel_voxel::testing::registry();
    let cache = GenCache::in_memory(&registry, GenCacheSettings::default()).expect("cache");
    for at in [
        DVec3::new(10.0, 60.0, 10.0),
        DVec3::new((1i64 << 40) as f64 + 10.0, 60.0, -((1i64 << 31) as f64)),
    ] {
        let first = {
            let backend = Arc::new(MemoryStore::new());
            let mut app = cached_app(&backend, &cache, at);
            fill(&mut app, &mut Model::default(), 400);
            drawn(&app)
        };
        cache.flush().expect("flush");
        let before = cache.stats();
        assert!(before.lod_misses > 0 && before.written > 0, "{before:?}");
        // A new world of the same terrain (the cache does not care which: its stamps do).
        let backend = Arc::new(MemoryStore::new());
        let mut app = cached_app(&backend, &cache, at);
        fill(&mut app, &mut Model::default(), 400);
        let after = cache.stats();
        assert!(after.lod_hits > before.lod_hits, "{after:?}");
        assert_eq!(
            after.lod_misses, before.lod_misses,
            "every column from the cache"
        );
        assert!(!first.is_empty());
        assert_eq!(drawn(&app), first, "the same nodes, the same meshes");
    }
}
