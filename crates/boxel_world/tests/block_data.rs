//! Data per block cell (ADR 0048) in headless apps with inline jobs: it is set through the edit
//! queue with the blocks, follows its block (kept through a change of state, dropped when the
//! block becomes another, unless the batch keeps it), survives an unload and a restart near the
//! origin and far from it (±2^31, 2^40 and the world limit), makes a generated chunk count as
//! edited, and remeshes its chunk only when the block's mesh reads it.
use boxel_app::App;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{
    BlockData, BlockDef, BlockModel, BlockPos, BlockRegistry, BlockState, CHUNK_SIZE, CHUNK_VOLUME,
    ChunkData, ChunkKey, ChunkPos, DataValue, FaceTextures, ItemStack, LocalPos, MAX_BLOCK_COORD,
    OPAQUE_OPACITY, Property, RenderShape, TextureId, testing,
};
use boxel_world::jobs::InlineExecutor;
use boxel_world::persist::{ChunkStore, MemoryStore, PersistPlugin, PersistSettings, WorldStore};
use boxel_world::{ChunkMap, ChunkViewer, DataEdit, EditQueue, WorldPlugin, WorldStats};
use boxel_worldgen::WorldGenerator;
use std::collections::BTreeSet;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// `test:solid` below `level`, air from `level` up.
struct Ground {
    level: i64,
}

impl WorldGenerator for Ground {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let solid = registry.by_name("test:solid").unwrap();
        let o = key.pos.origin();
        if o.y >= self.level {
            return ChunkData::uniform(BlockState::AIR);
        }
        if o.y + i64::from(CHUNK_SIZE) <= self.level {
            return ChunkData::uniform(solid);
        }
        let dense: Vec<BlockState> = (0..CHUNK_VOLUME)
            .map(|i| {
                let y = o.y + i64::from(LocalPos::from_index(i).y());
                if y < self.level {
                    solid
                } else {
                    BlockState::AIR
                }
            })
            .collect();
        ChunkData::from_dense(&dense)
    }
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        i64::MIN..=(self.level - 1) >> 5
    }
    fn name(&self) -> &str {
        "ground"
    }
}

/// The test blocks and three more: `test:chest` (a cube with `open`), `test:barrel` and
/// `test:frame` (whose mesh reads its data).
fn registry() -> Arc<BlockRegistry> {
    let mut b = testing::builder();
    let cube = |name: &str| BlockDef {
        name: name.to_string(),
        shape: RenderShape::Opaque,
        model: BlockModel::CUBE,
        collision: boxel_voxel::BlockCollision::Full,
        emission: [0; 3],
        light_opacity: OPAQUE_OPACITY,
        textures: FaceTextures::all(TextureId(1)),
    };
    b.add_block_with_states(
        cube("test:chest"),
        vec![Property::boolean("open")],
        |_, _| {},
    )
    .unwrap();
    b.add_block(cube("test:barrel")).unwrap();
    b.add_block(cube("test:frame")).unwrap();
    let frame = b.block_by_name("test:frame").unwrap();
    b.set_meshes_data(frame).unwrap();
    Arc::new(b.build())
}

struct World {
    app: App,
    viewer: boxel_ecs::entity::Entity,
}

/// A world on `backend` with a viewer at `at`, settled.
fn start(backend: &Arc<dyn ChunkStore>, registry: &Arc<BlockRegistry>, at: BlockPos) -> World {
    let generator = Ground { level: at.y };
    let store = WorldStore::open(Arc::clone(backend), registry, generator.name()).unwrap();
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(generator),
        registry: Arc::clone(registry),
        executor: Arc::new(InlineExecutor),
        settings: boxel_world::WorldSettings {
            light: false,
            ..Default::default()
        },
    })
    .add_plugins(PersistPlugin::new(store).with_executor(Arc::new(InlineExecutor)));
    app.insert_resource(PersistSettings {
        autosave: None,
        retry_after: Duration::ZERO,
        ..PersistSettings::default()
    });
    let viewer = app
        .world_mut()
        .spawn((
            Transform::from_translation(center(at)),
            ChunkViewer::new(1).with_vertical_radius(1),
        ))
        .id();
    let mut world = World { app, viewer };
    world.settle();
    world
}

fn center(p: BlockPos) -> DVec3 {
    DVec3::new(p.x as f64 + 0.5, p.y as f64 + 0.5, p.z as f64 + 0.5)
}

impl World {
    fn settle(&mut self) {
        let start = Instant::now();
        loop {
            self.app.update();
            let stats = *self.app.world().resource::<WorldStats>();
            let pending = self.app.world().resource::<WorldStore>().stats().pending;
            if stats.chunks > 0 && stats.is_settled() && pending == 0 {
                self.app.update();
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "not settled");
        }
    }

    fn queue(&mut self) -> boxel_ecs::world::Mut<'_, EditQueue> {
        self.app.world_mut().resource_mut::<EditQueue>()
    }

    fn map(&self) -> &ChunkMap {
        self.app.world().resource::<ChunkMap>()
    }

    fn data(&self, p: BlockPos) -> Option<BlockData> {
        assert!(self.map().block_at(p).is_some(), "{p:?} is not loaded");
        self.map().block_data_at(p).cloned()
    }

    fn move_to(&mut self, to: BlockPos) {
        self.app
            .world_mut()
            .get_mut::<Transform>(self.viewer)
            .unwrap()
            .translation = center(to);
        self.settle();
    }

    fn epoch(&self, p: BlockPos) -> u32 {
        let map = self.map();
        let id = map.id_at(ChunkPos::containing(p)).unwrap();
        map.get(id).unwrap().epoch
    }
}

fn at(o: BlockPos, x: i64, y: i64, z: i64) -> BlockPos {
    BlockPos::new(o.x + x, o.y + y, o.z + z)
}

/// The test places, as in `tests/persist.rs`.
fn origins() -> [BlockPos; 4] {
    let edge = i64::from(i32::MAX) + 1;
    let limit = MAX_BLOCK_COORD - (1 << 14);
    [
        BlockPos::new(0, 0, 0),
        BlockPos::new(edge - 8, -edge + 8, -edge - 8),
        BlockPos::new(1 << 40, -(1 << 40), 1 << 40),
        BlockPos::new(limit, limit, -limit),
    ]
}

/// A furnace-like inventory.
fn furnace(lit: i64) -> BlockData {
    let items = vec![
        Some(ItemStack::new("game:raw_iron", 9)),
        Some(ItemStack::new("game:coal", 1)),
        None,
    ];
    BlockData::new()
        .with("items", DataValue::Items(items.into()))
        .with("lit", DataValue::Int(lit))
}

fn state(r: &BlockRegistry, name: &str) -> BlockState {
    r.parse_state(name).unwrap()
}

#[test]
fn data_survives_unload_and_restart_near_and_far() {
    let registry = registry();
    for o in origins() {
        let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
        let mut w = start(&backend, &registry, o);
        let chest = state(&registry, "test:chest[open=false]");
        // Placed with its data in one frame, across chunk borders (-1 and 0 on every axis); and
        // data alone on a generated block (its chunk becomes an edited one).
        let cells = [at(o, -1, 0, -1), at(o, 0, 0, 0), at(o, 3, -1, 2)];
        for (k, &p) in cells.iter().enumerate() {
            if k < 2 {
                w.queue().set_block(p, chest);
            }
            w.queue().set_data(p, Some(furnace(k as i64)));
        }
        w.settle();
        for (k, &p) in cells.iter().enumerate() {
            assert_eq!(w.data(p), Some(furnace(k as i64)), "{o:?} {p:?}");
        }
        let chunks: BTreeSet<ChunkPos> = cells.iter().map(|&p| ChunkPos::containing(p)).collect();
        // Away and back: saved with their chunks, loaded with them.
        w.move_to(at(o, 1 << 13, 0, 0));
        let saved: BTreeSet<ChunkPos> = backend.keys().unwrap().iter().map(|k| k.pos).collect();
        assert_eq!(saved, chunks, "{o:?}: the chunks with data, and only them");
        w.move_to(o);
        for (k, &p) in cells.iter().enumerate() {
            assert_eq!(w.data(p), Some(furnace(k as i64)), "{o:?} {p:?} back");
        }
        w.app.shutdown();
        drop(w);
        let w = start(&backend, &registry, o);
        for (k, &p) in cells.iter().enumerate() {
            assert_eq!(w.data(p), Some(furnace(k as i64)), "{o:?} {p:?} restarted");
        }
        assert_eq!(w.data(at(o, 1, 0, 0)), None);
        assert_eq!(
            w.app.world().resource::<WorldStore>().stats().load_errors,
            0
        );
    }
}

#[test]
fn data_follows_its_block() {
    let registry = registry();
    let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
    let o = origins()[2];
    let mut w = start(&backend, &registry, o);
    let closed = state(&registry, "test:chest[open=false]");
    let open = state(&registry, "test:chest[open=true]");
    let barrel = state(&registry, "test:barrel");
    let [a, b, c, d, e] = [0, 1, 2, 3, 4].map(|x| at(o, x, 0, 5));
    for p in [a, b, c, d, e] {
        w.queue().set_block(p, closed);
        w.queue().set_data(p, Some(furnace(5)));
    }
    // Data on air is never kept.
    let air = at(o, 0, 3, 0);
    w.queue().set_data(air, Some(furnace(1)));
    w.settle();
    assert_eq!(w.data(air), None);
    // Another state of the same block keeps it; another block drops it, unless the batch keeps
    // it or sets new data; broken, it goes.
    w.queue().set_block(a, open);
    w.queue().set_block(b, barrel);
    w.queue().set_block(c, barrel);
    w.queue().keep_data(c);
    w.queue().set_block(d, barrel);
    w.queue().set_data(d, Some(furnace(9)));
    w.queue().set_block(e, BlockState::AIR);
    w.queue().keep_data(e);
    // The queue tells what the next batch does.
    assert_eq!(
        w.app.world().resource::<EditQueue>().pending_data(d),
        Some(&DataEdit::Set(Some(furnace(9))))
    );
    assert_eq!(
        w.app.world().resource::<EditQueue>().pending_data(c),
        Some(&DataEdit::Keep)
    );
    w.settle();
    assert_eq!(w.data(a), Some(furnace(5)), "same block");
    assert_eq!(w.data(b), None, "another block");
    assert_eq!(w.data(c), Some(furnace(5)), "kept");
    assert_eq!(w.data(d), Some(furnace(9)), "new data");
    assert_eq!(w.data(e), None, "broken: no data on air even if kept");
    // Broken and placed again in one batch: the new block starts without data.
    w.queue().set_block(a, BlockState::AIR);
    w.queue().set_block(a, closed);
    // Removed.
    w.queue().set_data(c, None);
    w.settle();
    assert_eq!(w.data(a), None);
    assert_eq!(w.data(c), None);
    assert!(w.app.world().resource::<EditQueue>().is_empty());
}

#[test]
fn only_data_a_mesh_reads_remeshes() {
    let registry = registry();
    let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
    let o = origins()[1];
    let mut w = start(&backend, &registry, o);
    let frame = state(&registry, "test:frame");
    let barrel = state(&registry, "test:barrel");
    let (f, b) = (at(o, 2, 0, 2), at(o, 2, 0, 9));
    w.queue().set_block(f, frame);
    w.queue().set_block(b, barrel);
    w.settle();
    let (ef, eb) = (w.epoch(f), w.epoch(b));
    let version = w
        .map()
        .data_at(ChunkPos::containing(b))
        .map(|d| d.version)
        .unwrap();
    w.queue().set_data(b, Some(furnace(1)));
    w.settle();
    assert_eq!(w.epoch(b), eb, "a barrel's data is not drawn");
    let data = w.map().data_at(ChunkPos::containing(b)).unwrap();
    assert_eq!(data.version, version, "data is not the blocks");
    w.queue().set_data(f, Some(furnace(2)));
    w.settle();
    assert_ne!(
        w.epoch(f),
        ef,
        "a frame's data is drawn: its chunk remeshes"
    );
    // The same data again changes nothing.
    let ef = w.epoch(f);
    w.queue().set_data(f, Some(furnace(2)));
    w.settle();
    assert_eq!(w.epoch(f), ef);
}
