//! The world's messages for systems that keep state derived from the blocks (physics, lighting):
//! `BlockChanged` for each edited block and `ChunkDataEvent` when a chunk's blocks become
//! readable or stop being readable.
use boxel_app::{App, Update};
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, ChunkPos};
use boxel_world::jobs::InlineExecutor;
use boxel_world::{
    BlockChanged, ChunkDataEvent, ChunkMap, ChunkViewer, DataState, EditQueue, WorldPlugin,
    WorldStats, WorldSystems,
};
use boxel_worldgen::FlatGenerator;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// `boxel:stone` in `BlockRegistry::default_blocks()`.
const STONE: BlockState = BlockState(1);

/// Every world message read in `Update` after the world's systems, in order.
#[derive(Resource, Default)]
struct Log {
    blocks: Vec<BlockChanged>,
    chunks: Vec<ChunkDataEvent>,
}

fn log(
    mut blocks: MessageReader<BlockChanged>,
    mut chunks: MessageReader<ChunkDataEvent>,
    mut log: ResMut<Log>,
) {
    log.blocks.extend(blocks.read().copied());
    log.chunks.extend(chunks.read().copied());
}

/// A flat world (stone y 0..=2, grass at y = 3) with inline jobs, a viewer of radius 1 at
/// `(x, 4, z)` and the message log.
fn app(x: f64, z: f64) -> (App, Entity) {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(FlatGenerator::new(vec![
            ("boxel:stone".to_string(), 3),
            ("boxel:grass".to_string(), 1),
        ])),
        registry: Arc::new(BlockRegistry::default_blocks()),
        executor: Arc::new(InlineExecutor),
        ..WorldPlugin::default()
    })
    .init_resource::<Log>()
    .add_systems(Update, log.after(WorldSystems::Collect));
    let viewer = app
        .world_mut()
        .spawn((
            Transform::from_translation(DVec3::new(x, 4.0, z)),
            ChunkViewer::new(1).with_vertical_radius(1),
        ))
        .id();
    settle(&mut app);
    (app, viewer)
}

fn settle(app: &mut App) {
    let start = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "not settled: {stats:?}"
        );
    }
}

/// The chunks whose blocks can be read now.
fn readable(app: &App) -> BTreeSet<ChunkPos> {
    app.world()
        .resource::<ChunkMap>()
        .iter()
        .filter(|(_, slot)| slot.data().is_readable())
        .map(|(_, slot)| slot.key().pos)
        .collect()
}

/// Replays the chunk messages: the set of readable chunks they describe (each `Loaded` for a
/// chunk not in the set, each `Unloaded` for one in it, with matching ids).
fn replay(events: &[ChunkDataEvent]) -> BTreeSet<ChunkPos> {
    let mut loaded = std::collections::BTreeMap::new();
    for event in events {
        match *event {
            ChunkDataEvent::Loaded { id, key } => {
                assert!(loaded.insert(key.pos, id).is_none(), "{key:?} loaded twice");
            }
            ChunkDataEvent::Unloaded { id, key } => {
                assert_eq!(loaded.remove(&key.pos), Some(id), "{key:?} unloaded");
            }
        }
    }
    loaded.into_keys().collect()
}

fn edit(app: &mut App, pos: BlockPos, state: BlockState) {
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(pos, state);
}

#[test]
fn edits_send_one_block_changed_per_changed_position() {
    let (mut app, _) = app(16.0, 16.0);
    app.world_mut().resource_mut::<Log>().blocks.clear();
    let (a, b, c) = (
        BlockPos::new(1, 4, 1),
        BlockPos::new(2, 3, 2),
        BlockPos::new(3, 2, 3),
    );
    // `a`: air to stone. `b`: grass to air and back (no change). `c`: stone to air to grass (one
    // message, stone to grass). A block in a chunk that is not loaded: dropped, no message.
    let grass = app
        .world()
        .resource::<ChunkMap>()
        .block_at(b)
        .expect("loaded");
    edit(&mut app, a, STONE);
    edit(&mut app, b, BlockState::AIR);
    edit(&mut app, c, BlockState::AIR);
    edit(&mut app, b, grass);
    edit(&mut app, c, grass);
    edit(&mut app, BlockPos::new(10_000, 4, 0), STONE);
    // Setting a block to what it holds is no change either.
    edit(&mut app, BlockPos::new(4, 0, 4), STONE);
    app.update();
    assert_eq!(
        app.world().resource::<Log>().blocks,
        [
            BlockChanged {
                pos: a,
                old: BlockState::AIR,
                new: STONE,
            },
            BlockChanged {
                pos: c,
                old: STONE,
                new: grass,
            },
        ]
    );
    let map = app.world().resource::<ChunkMap>();
    assert_eq!(
        (map.block_at(a), map.block_at(b), map.block_at(c)),
        (Some(STONE), Some(grass), Some(grass))
    );
    // A frame without edits sends nothing.
    app.world_mut().resource_mut::<Log>().blocks.clear();
    app.update();
    assert!(app.world().resource::<Log>().blocks.is_empty());
}

#[test]
fn chunk_messages_follow_what_can_be_read() {
    let (mut app, viewer) = app(16.0, 16.0);
    let first = app.world().resource::<Log>().chunks.clone();
    assert!(!first.is_empty());
    assert!(
        first
            .iter()
            .all(|e| matches!(e, ChunkDataEvent::Loaded { .. }))
    );
    assert_eq!(replay(&first), readable(&app));
    // The viewer moves far away: the old chunks unload, the new ones load.
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .expect("viewer")
        .translation = DVec3::new(16.0 + 32.0 * 10.0, 4.0, 16.0);
    settle(&mut app);
    let all = app.world().resource::<Log>().chunks.clone();
    assert!(
        all[first.len()..]
            .iter()
            .any(|e| matches!(e, ChunkDataEvent::Unloaded { .. }))
    );
    assert_eq!(replay(&all), readable(&app));
    // Back: the first chunks load again, with new ids.
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .expect("viewer")
        .translation = DVec3::new(16.0, 4.0, 16.0);
    settle(&mut app);
    let again = app.world().resource::<Log>().chunks.clone();
    assert_eq!(replay(&again), readable(&app));
    let home = ChunkPos::new(0, 0, 0);
    let ids: Vec<_> = again
        .iter()
        .filter(|e| matches!(e, ChunkDataEvent::Loaded { .. }) && e.pos() == home)
        .map(ChunkDataEvent::id)
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_ne!(ids[0], ids[1]);
    // Every readable chunk is Ready or Missing.
    let map = app.world().resource::<ChunkMap>();
    for pos in readable(&app) {
        let id = map.id_at(pos).expect("slot");
        assert!(matches!(
            map.get(id).expect("slot").data(),
            DataState::Ready(_) | DataState::Missing
        ));
    }
}
