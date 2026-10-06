//! World streaming with the real mesher and `TerrainGenerator`: the plugin in an
//! `App`, real terrain, both executors.
//!
//! Besides the event counts, every settled world is checked against first principles: each
//! generated chunk equals a fresh `generate` call, each `Ready` or `Empty` mesh equals what
//! `boxel_mesh::Mesher` builds from the map's current data and neighbors, and replaying the
//! `ChunkMeshEvent`s (as a renderer does) gives exactly `ChunkMap::ready_meshes()`.
use boxel_app::App;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_mesh::{ChunkMesh, MeshInput, Mesher};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_SIZE, ChunkKey, ChunkPos, LocalPos,
    NEIGHBOR_OFFSETS, RenderShape,
};
use boxel_world::jobs::{InlineExecutor, JobExecutor, TaskPoolExecutor};
use boxel_world::{
    ChunkId, ChunkMap, ChunkMeshEvent, ChunkSlot, ChunkViewer, DataState, EditQueue, MeshState,
    WorldContent, WorldPlugin, WorldSettings, WorldStats,
};
use boxel_worldgen::{TerrainGenerator, WorldGenerator};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn terrain_app(executor: Arc<dyn JobExecutor>, max_jobs_in_flight: usize) -> App {
    terrain_app_with(
        executor,
        WorldSettings {
            max_jobs_in_flight,
            ..WorldSettings::default()
        },
    )
}

fn terrain_app_with(executor: Arc<dyn JobExecutor>, settings: WorldSettings) -> App {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(TerrainGenerator::default()),
        registry: Arc::new(BlockRegistry::default_blocks()),
        settings,
        executor,
    });
    app
}

/// A viewer at the center of chunk column `(cx, cz)`, at height 80 (terrain surface level, chunk
/// layer 2), with the default vertical radius (8).
fn spawn_viewer(app: &mut App, cx: i32, cz: i32, radius: u32) -> Entity {
    app.world_mut()
        .spawn((
            Transform::from_translation(column_center(cx, cz)),
            ChunkViewer::new(radius),
        ))
        .id()
}

fn column_center(cx: i32, cz: i32) -> DVec3 {
    let half = f64::from(CHUNK_SIZE) / 2.0;
    DVec3::new(
        f64::from(cx * CHUNK_SIZE) + half,
        80.0,
        f64::from(cz * CHUNK_SIZE) + half,
    )
}

/// Updates until settled (60 s max); returns the mesh events in order.
fn settle(app: &mut App) -> Vec<ChunkMeshEvent> {
    let start = Instant::now();
    let mut events = Vec::new();
    loop {
        app.update();
        events.extend(
            app.world_mut()
                .resource_mut::<Messages<ChunkMeshEvent>>()
                .drain(),
        );
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            return events;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "not settled in 60 s: {stats:?}"
        );
        std::thread::yield_now();
    }
}

/// Last mesh published per chunk.
fn last_meshes(events: &[ChunkMeshEvent]) -> BTreeMap<u64, Arc<ChunkMesh>> {
    let mut out = BTreeMap::new();
    apply_events(&mut out, events);
    out
}

/// Applies mesh events to a renderer-like view of the world (chunk id -> mesh).
fn apply_events(replica: &mut BTreeMap<u64, Arc<ChunkMesh>>, events: &[ChunkMeshEvent]) {
    for e in events {
        match e {
            ChunkMeshEvent::Upsert { id, mesh, .. } => {
                replica.insert(id.to_bits(), Arc::clone(mesh));
            }
            ChunkMeshEvent::Remove { id, .. } => {
                replica.remove(&id.to_bits());
            }
        }
    }
}

/// The replayed events agree with `ChunkMap::ready_meshes()` (same chunks, same mesh `Arc`s).
fn assert_replica_matches(app: &App, replica: &BTreeMap<u64, Arc<ChunkMesh>>) {
    let map = app.world().resource::<ChunkMap>();
    let ready: BTreeMap<u64, &Arc<ChunkMesh>> = map
        .ready_meshes()
        .map(|(id, _, mesh)| (id.to_bits(), mesh))
        .collect();
    assert_eq!(
        replica.keys().collect::<Vec<_>>(),
        ready.keys().collect::<Vec<_>>(),
        "published chunks == ready meshes"
    );
    for (bits, mesh) in replica {
        assert!(
            Arc::ptr_eq(mesh, ready[bits]),
            "chunk {bits:#x}: the last Upsert carries the ready mesh"
        );
    }
}

/// Generated data equals a fresh `generate` call (edited chunks excepted), and every `Ready` or
/// `Empty` mesh equals what `Mesher` builds now from the map's data and its 26 neighbors (not
/// loaded or not generated = air, as the world defines it).
fn assert_world_matches_direct(app: &App, edited: &[ChunkPos]) {
    let map = app.world().resource::<ChunkMap>();
    let content = app.world().resource::<WorldContent>();
    let mut mesher = Mesher::new();
    let (mut meshes, mut empties) = (0, 0);
    for (_, slot) in map.iter() {
        let pos = slot.pos;
        if let DataState::Ready(data) = slot.data()
            && !edited.contains(&pos)
        {
            assert_eq!(
                **data,
                content.generator.generate(slot.key(), &content.registry),
                "{pos:?}: data equals a fresh generate"
            );
        }
        let expected_empty = match slot.mesh() {
            MeshState::Ready { .. } => false,
            MeshState::Empty => true,
            MeshState::None | MeshState::Queued | MeshState::InFlight { .. } => continue,
        };
        let center = match slot.data() {
            DataState::Ready(data) => Arc::clone(data),
            // Missing data is air: it can only mesh to nothing.
            _ => {
                assert!(expected_empty, "{pos:?}: a mesh without data");
                continue;
            }
        };
        let input = MeshInput {
            center: &center,
            neighbors: std::array::from_fn(|i| {
                map.data_at(pos.offset(NEIGHBOR_OFFSETS[i]))
                    .map(Arc::as_ref)
            }),
        };
        let direct = mesher.mesh_chunk(&input, &content.materials);
        if let MeshState::Ready { mesh, .. } = slot.mesh() {
            assert_eq!(**mesh, direct, "{pos:?}: streamed mesh == direct mesh");
            assert!(!mesh.is_empty());
            meshes += 1;
        } else {
            assert!(
                direct.is_empty(),
                "{pos:?}: Empty mesh, direct mesh has quads"
            );
            empties += 1;
        }
    }
    assert!(
        meshes > 0 && empties > 0,
        "{meshes} meshes, {empties} empty"
    );
}

/// Number of columns with `dx^2 + dz^2 <= r^2` (Gauss circle numbers).
fn mesh_columns(radius: i32) -> usize {
    let mut n = 0;
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dz * dz <= radius * radius {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn real_terrain_publishes_meshes() {
    let mut app = terrain_app(Arc::new(TaskPoolExecutor), 0);
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(0.0, 80.0, 0.0)),
        ChunkViewer::new(3),
    ));
    let events = settle(&mut app);

    let upserts = events
        .iter()
        .filter(|e| matches!(e, ChunkMeshEvent::Upsert { .. }))
        .count();
    assert!(upserts > 0, "terrain produced meshes");
    for event in &events {
        if let ChunkMeshEvent::Upsert { mesh, .. } = event {
            assert!(!mesh.is_empty());
            assert_eq!(mesh.bucket_counts.iter().sum::<u32>(), mesh.total_quads());
        }
    }
    let map = app.world().resource::<ChunkMap>();
    let quads: u32 = map.ready_meshes().map(|(_, _, m)| m.total_quads()).sum();
    assert!(quads > 0);
    assert_eq!(
        map.ready_meshes().count(),
        upserts,
        "one Upsert per ready mesh"
    );
    let stats = *app.world().resource::<WorldStats>();
    assert_eq!(stats.data_ready, stats.chunks, "every chunk generated");
    assert!(stats.mesh_ready > 0 && stats.mesh_empty > 0);
    // Every chunk of the mesh cylinder (radius 3 circle x 17 layers around layer 2) is meshed.
    assert_eq!(stats.mesh_ready + stats.mesh_empty, mesh_columns(3) * 17);
    let map = app.world().resource::<ChunkMap>();
    assert!(
        map.iter()
            .all(|(_, slot)| (2 - 9..=2 + 9).contains(&slot.pos.y))
    );
    // The resource and the slot-based counters agree.
    let slots = map.stats();
    assert_eq!(
        (
            slots.chunks,
            slots.data_ready,
            slots.mesh_ready,
            slots.mesh_empty
        ),
        (
            stats.chunks,
            stats.data_ready,
            stats.mesh_ready,
            stats.mesh_empty
        )
    );

    assert_world_matches_direct(&app, &[]);
    assert_replica_matches(&app, &last_meshes(&events));
}

/// Inline (deterministic order) and task pool (parallel) jobs build the same meshes.
#[test]
fn both_executors_build_the_same_world() {
    let meshes_by_pos = |executor: Arc<dyn JobExecutor>| {
        let mut app = terrain_app(executor, 0);
        spawn_viewer(&mut app, 1, -2, 2);
        settle(&mut app);
        assert_world_matches_direct(&app, &[]);
        let map = app.world().resource::<ChunkMap>();
        map.ready_meshes()
            .map(|(_, key, mesh)| (key.pos, Arc::clone(mesh)))
            .collect::<BTreeMap<ChunkPos, Arc<ChunkMesh>>>()
    };
    let inline = meshes_by_pos(Arc::new(InlineExecutor));
    let pooled = meshes_by_pos(Arc::new(TaskPoolExecutor));
    assert!(!inline.is_empty());
    assert_eq!(inline, pooled);
}

/// A far move unloads the old area (every published mesh gets its `Remove`) and streams the new
/// one; coming back rebuilds the same meshes.
#[test]
fn moving_the_viewer_streams_and_unloads() {
    let mut app = terrain_app(Arc::new(TaskPoolExecutor), 0);
    let viewer = spawn_viewer(&mut app, 0, 0, 2);
    let mut replica = BTreeMap::new();
    apply_events(&mut replica, &settle(&mut app));
    assert_replica_matches(&app, &replica);
    let snapshot = |app: &App| {
        let map = app.world().resource::<ChunkMap>();
        map.ready_meshes()
            .map(|(_, key, mesh)| (key, (**mesh).clone()))
            .collect::<BTreeMap<ChunkKey, ChunkMesh>>()
    };
    let home = snapshot(&app);

    // 20 chunks east: far beyond radius + 1 + hysteresis.
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .expect("viewer")
        .translation = column_center(20, 0);
    apply_events(&mut replica, &settle(&mut app));
    assert_replica_matches(&app, &replica);
    assert_world_matches_direct(&app, &[]);
    let map = app.world().resource::<ChunkMap>();
    assert!(
        map.iter().all(|(_, slot)| slot.pos.x >= 20 - 6),
        "the old area was unloaded"
    );
    assert!(map.id_at(ChunkPos::new(20, 2, 0)).is_some());

    // Back home: the same meshes again (new ids).
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .expect("viewer")
        .translation = column_center(0, 0);
    apply_events(&mut replica, &settle(&mut app));
    assert_replica_matches(&app, &replica);
    assert_eq!(snapshot(&app), home);
}

#[test]
fn edits_remesh_neighbors() {
    let mut app = terrain_app(Arc::new(InlineExecutor), 64);
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(0.0, 80.0, 0.0)),
        ChunkViewer::new(2),
    ));
    let events = settle(&mut app);
    let before = last_meshes(&events);
    let registry = BlockRegistry::default_blocks();
    let solid = |s: Option<BlockState>| s.is_some_and(|s| registry.shape(s) == RenderShape::Opaque);

    // A border voxel of chunk A (local x = 31) whose +X neighbor voxel in chunk B is also solid,
    // with both chunks meshed: removing it exposes a face in B.
    let map = app.world().resource::<ChunkMap>();
    let ready: Vec<(ChunkId, ChunkPos)> = map
        .ready_meshes()
        .map(|(id, key, _)| (id, key.pos))
        .collect();
    let mut found = None;
    'search: for &(a, pos) in &ready {
        let east = ChunkPos::new(pos.x + 1, pos.y, pos.z);
        let Some(b) = map.id_at(east) else { continue };
        if !matches!(
            map.get(b).map(ChunkSlot::mesh),
            Some(MeshState::Ready { .. })
        ) {
            continue;
        }
        for y in 0..32 {
            for z in 0..32 {
                let voxel = BlockPos::from_parts(pos, LocalPos::new(31, y, z));
                let across = BlockPos::new(voxel.x + 1, voxel.y, voxel.z);
                if solid(map.block_at(voxel)) && solid(map.block_at(across)) {
                    found = Some((a, b, voxel));
                    break 'search;
                }
            }
        }
    }
    let (a, b, voxel) = found.expect("a solid voxel pair across a meshed chunk border");
    let chunk_a = voxel.split().0;

    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(voxel, BlockState::AIR);
    let events = settle(&mut app);
    let after = last_meshes(&events);
    for id in [a, b] {
        let bits = id.to_bits();
        assert!(after.contains_key(&bits), "{id:?} remeshed and published");
        assert_ne!(after[&bits], before[&bits], "{id:?} mesh changed");
    }
    let map = app.world().resource::<ChunkMap>();
    assert_eq!(map.block_at(voxel), Some(BlockState::AIR));
    let DataState::Ready(data) = map.get(a).expect("chunk A").data() else {
        panic!("chunk A data is ready");
    };
    assert_eq!(data.version, 1, "one edit, one version bump");

    // The remeshed chunks match a direct mesh of the edited data, and the events still replay
    // to the ready meshes.
    assert_world_matches_direct(&app, &[chunk_a]);
    let mut replica = before;
    apply_events(&mut replica, &events);
    assert_replica_matches(&app, &replica);
}

/// Flying up leaves the terrain behind (its meshes are removed) and loads sky; coming back down
/// rebuilds the same meshes.
#[test]
fn vertical_moves_stream_and_unload() {
    let mut app = terrain_app(Arc::new(TaskPoolExecutor), 0);
    let viewer = app
        .world_mut()
        .spawn((
            Transform::from_translation(column_center(0, 0)),
            ChunkViewer::new(2).with_vertical_radius(2),
        ))
        .id();
    let mut replica = BTreeMap::new();
    apply_events(&mut replica, &settle(&mut app));
    assert_replica_matches(&app, &replica);
    let snapshot = |app: &App| {
        let map = app.world().resource::<ChunkMap>();
        map.ready_meshes()
            .map(|(_, key, mesh)| (key, (**mesh).clone()))
            .collect::<BTreeMap<ChunkKey, ChunkMesh>>()
    };
    let home = snapshot(&app);
    assert!(!home.is_empty());
    let move_to = |app: &mut App, y: f64| {
        app.world_mut()
            .get_mut::<Transform>(viewer)
            .expect("viewer")
            .translation
            .y = y;
    };

    // Up to layer 12 (y = 400): the terrain (layers -2..=3) is far below the loaded layers
    // 9..=15 and beyond the hysteresis: everything there unloads.
    move_to(&mut app, 400.0);
    apply_events(&mut replica, &settle(&mut app));
    assert_replica_matches(&app, &replica);
    assert!(replica.is_empty(), "no terrain meshes in the sky");
    let map = app.world().resource::<ChunkMap>();
    assert!(map.iter().all(|(_, slot)| slot.pos.y >= 12 - 5));
    assert!(
        map.id_at(ChunkPos::new(0, 15, 0)).is_some(),
        "no height limit"
    );
    assert_world_matches_direct_or_empty(&app);

    // Back down: the same meshes again.
    move_to(&mut app, 80.0);
    apply_events(&mut replica, &settle(&mut app));
    assert_replica_matches(&app, &replica);
    assert_world_matches_direct(&app, &[]);
    assert_eq!(snapshot(&app), home);
}

/// Like `assert_world_matches_direct`, for worlds that may have no mesh at all.
fn assert_world_matches_direct_or_empty(app: &App) {
    let map = app.world().resource::<ChunkMap>();
    for (_, slot) in map.iter() {
        assert!(
            matches!(slot.mesh(), MeshState::None | MeshState::Empty),
            "{:?}: sky chunks mesh to nothing",
            slot.pos
        );
    }
}

/// With `vertical_bounds` set to the generator's layers and a viewer that wants every layer, the
/// world streams like boxel M0: every chunk of every mesh column of those layers, nothing outside.
#[test]
fn fixed_height_streams_every_layer_of_the_generator() {
    let generator = TerrainGenerator::default();
    let layers = generator.vertical_chunks();
    let mut app = terrain_app_with(
        Arc::new(TaskPoolExecutor),
        WorldSettings {
            vertical_bounds: Some(layers.clone()),
            ..WorldSettings::default()
        },
    );
    let viewer = app
        .world_mut()
        .spawn((
            Transform::from_translation(column_center(0, 0)),
            ChunkViewer::new(3).with_vertical_radius(u32::MAX),
        ))
        .id();
    let events = settle(&mut app);
    let stats = *app.world().resource::<WorldStats>();
    assert_eq!(
        stats.mesh_ready + stats.mesh_empty,
        mesh_columns(3) * layers.clone().count()
    );
    let map = app.world().resource::<ChunkMap>();
    assert!(map.iter().all(|(_, s)| layers.contains(&s.pos.y)));
    assert_world_matches_direct(&app, &[]);
    assert_replica_matches(&app, &last_meshes(&events));

    // Moving up changes nothing: every layer is already wanted.
    let chunks = stats.chunks;
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .expect("viewer")
        .translation
        .y = 2000.0;
    let events = settle(&mut app);
    assert!(events.is_empty(), "{} mesh events", events.len());
    assert_eq!(app.world().resource::<WorldStats>().chunks, chunks);
}
