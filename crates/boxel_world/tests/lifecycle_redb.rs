//! Closing a redb world (issue #58, E-P): a closed world's file opens again at once and without a
//! repair while clones of its store live; a file that was not closed cleanly (a crash) is
//! repaired, and the app of the world logs one warning, also when the world opened before
//! anything logged; the exit (`Shutdown`) saves everything, lets a game system ordered before
//! `WorldShutdownSystems::Close` write its value and then closes the file, far from the origin
//! and with the task pools, as in the game.
use boxel_app::{App, Shutdown};
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos,
    LocalPos,
};
use boxel_world::jobs::{InlineExecutor, IoTaskPoolExecutor, TaskPoolExecutor};
use boxel_world::persist::{
    ChunkStore, PersistPlugin, PersistSettings, RedbStore, WORLD_FILE, WorldShutdownSystems,
    WorldStore,
};
use boxel_world::{ChunkMap, ChunkViewer, EditQueue, WorldPlugin, WorldStats};
use boxel_worldgen::WorldGenerator;
use std::io;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Stone below y = 0, air from there up.
struct Ground;

impl WorldGenerator for Ground {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let stone = registry.by_name("boxel:stone").unwrap();
        let o = key.pos.origin();
        if o.y >= 0 {
            return ChunkData::uniform(BlockState::AIR);
        }
        if o.y + i64::from(CHUNK_SIZE) <= 0 {
            return ChunkData::uniform(stone);
        }
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        for (i, slot) in dense.iter_mut().enumerate() {
            if o.y + i64::from(LocalPos::from_index(i).y()) < 0 {
                *slot = stone;
            }
        }
        ChunkData::from_dense(&dense)
    }
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        i64::MIN..=-1
    }
    fn name(&self) -> &str {
        "ground"
    }
}

/// A fresh directory for a redb world (under the target dir), removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("lifecycle-redb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Opens `file` with redb directly, panicking if it has to be repaired: it was closed cleanly.
/// Fails while another handle has it open.
fn open_clean(file: &Path) -> Result<redb::Database, redb::DatabaseError> {
    redb::Database::builder()
        .set_repair_callback(|_| panic!("the world file was not closed cleanly"))
        .open(file)
}

/// Keeps the message of every event at `INFO` or above logged on the thread it is the
/// subscriber of (`tracing::subscriber::with_default`).
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<String>>>);

impl Logged {
    /// The lines that contain `text`.
    fn with(&self, text: &str) -> Vec<String> {
        let lines = self.0.lock().unwrap();
        lines.iter().filter(|l| l.contains(text)).cloned().collect()
    }
}

/// The `message` field of an event.
struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for Logged {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0.lock().unwrap().push(message.0);
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

const REPAIR: &str = "not closed cleanly";

#[test]
fn a_closed_world_file_opens_again_at_once_without_a_repair() {
    let tmp = TempDir::new("clean");
    let registry = boxel_demo::blocks::registry();
    let store = WorldStore::open_dir(&tmp.0, &registry, "ground").unwrap();
    assert!(!store.repaired(), "a new file is not a repair");
    store.put_meta("game:player", b"far".to_vec()).unwrap();
    // A job still holds a clone.
    let job = store.clone();
    let file = tmp.0.join(WORLD_FILE);
    assert!(
        RedbStore::open(&file).is_err(),
        "locked while the world is open"
    );
    store.close().unwrap();
    assert!(job.is_closed());
    drop(open_clean(&file).expect("released and clean"));
    let again = WorldStore::open_dir(&tmp.0, &registry, "ground").expect("opens at once");
    assert!(!again.repaired());
    assert_eq!(again.meta("game:player").unwrap(), Some(b"far".to_vec()));
    assert_eq!(
        job.meta("game:player").unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
}

#[test]
fn a_file_not_closed_cleanly_is_repaired_and_says_so() {
    let tmp = TempDir::new("crash");
    let registry = boxel_demo::blocks::registry();
    let copy = crashed_worlds(&tmp.0, &registry, 1)[0].join(WORLD_FILE);

    // The open itself logs nothing: its app does (below).
    let logged = Logged::default();
    let repaired =
        tracing::subscriber::with_default(logged.clone(), || RedbStore::open(&copy)).unwrap();
    assert!(repaired.repaired());
    assert!(logged.with(REPAIR).is_empty());
    assert_eq!(
        repaired.load_meta("game:player").unwrap(),
        Some(b"far".to_vec())
    );
    // Closed cleanly now: no repair.
    repaired.close().unwrap();
    assert!(!RedbStore::open(&copy).unwrap().repaired());
    // Nor for the world closed by its store.
    let live = RedbStore::open(tmp.0.join("live").join(WORLD_FILE)).unwrap();
    assert!(!live.repaired());
}

/// `count` world directories whose file was copied while its store was open: files that were
/// never closed cleanly, as after a crash.
fn crashed_worlds(dir: &Path, registry: &BlockRegistry, count: usize) -> Vec<PathBuf> {
    let live = dir.join("live");
    let store = WorldStore::open_dir(&live, registry, "ground").unwrap();
    store.put_meta("game:player", b"far".to_vec()).unwrap();
    let copies = (0..count)
        .map(|i| {
            let copy = dir.join(format!("crash-{i}"));
            std::fs::create_dir_all(&copy).unwrap();
            std::fs::copy(live.join(WORLD_FILE), copy.join(WORLD_FILE)).unwrap();
            copy
        })
        .collect();
    store.close().unwrap();
    copies
}

/// An app with the world of `store` (built, never updated).
fn app_with(store: WorldStore, registry: &Arc<BlockRegistry>) -> App {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(Ground),
        registry: Arc::clone(registry),
        executor: Arc::new(InlineExecutor),
        ..WorldPlugin::default()
    })
    .add_plugins(PersistPlugin {
        store,
        executor: Arc::new(InlineExecutor),
    });
    app
}

/// A world opened before anything logs (`voxel_world` and `snapshot` open theirs before
/// `LogPlugin` installs the subscriber) still reports its repair, once, when its `PersistPlugin`
/// is added; one opened while the app logs reports it once too, not twice; a clean one or a new
/// one never (redb runs its repair on a file it creates too).
#[test]
fn a_repair_is_logged_once_by_the_app_of_the_world() {
    let tmp = TempDir::new("repair-log");
    let registry = Arc::new(boxel_demo::blocks::registry());
    let crashed = crashed_worlds(&tmp.0, &registry, 2);

    // Nothing listens while it opens.
    let store = WorldStore::open_dir(&crashed[0], &registry, "ground").unwrap();
    assert!(store.repaired());
    let logged = Logged::default();
    let app = tracing::subscriber::with_default(logged.clone(), || app_with(store, &registry));
    assert_eq!(logged.with(REPAIR).len(), 1, "reported by the app");
    drop(app);

    // Opened while the app logs.
    let logged = Logged::default();
    let app = tracing::subscriber::with_default(logged.clone(), || {
        let store = WorldStore::open_dir(&crashed[1], &registry, "ground").unwrap();
        app_with(store, &registry)
    });
    assert_eq!(logged.with(REPAIR).len(), 1, "one warning, not two");
    drop(app);

    // Dropping the apps closed both cleanly; a new world was never repaired.
    for dir in crashed.iter().chain([&tmp.0.join("new")]) {
        let logged = Logged::default();
        let app = tracing::subscriber::with_default(logged.clone(), || {
            let store = WorldStore::open_dir(dir, &registry, "ground").unwrap();
            assert!(!store.repaired());
            app_with(store, &registry)
        });
        assert!(logged.with(REPAIR).is_empty());
        drop(app);
    }
}

// ------------------------------------------------------------------ the exit

/// What a game system that writes the store after the close got.
#[derive(Resource, Default)]
struct Late(Option<io::ErrorKind>);

/// A game value written on exit, ordered before the close.
fn save_game(store: Res<WorldStore>) {
    store
        .put_meta("game:exit", b"before the close".to_vec())
        .expect("written before the close");
}

/// The same after the close: the closed error.
fn write_late(store: Res<WorldStore>, mut late: ResMut<Late>) {
    late.0 = store
        .put_meta("game:late", b"after the close".to_vec())
        .err()
        .map(|e| e.kind());
}

fn center(p: BlockPos) -> DVec3 {
    DVec3::new(p.x as f64 + 0.5, p.y as f64 + 0.5, p.z as f64 + 0.5)
}

fn at(o: BlockPos, x: i64, y: i64, z: i64) -> BlockPos {
    BlockPos::new(o.x + x, o.y + y, o.z + z)
}

/// The world in `dir` with chunk jobs and save jobs on the task pools (as in the game), no
/// autosave and the viewer at `p`, settled.
fn start(dir: &Path, registry: &Arc<BlockRegistry>, p: BlockPos) -> (App, Entity) {
    let store = WorldStore::open_dir(dir, registry, "ground").expect("opens");
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(Ground),
        registry: Arc::clone(registry),
        executor: Arc::new(TaskPoolExecutor),
        ..WorldPlugin::default()
    })
    .add_plugins(PersistPlugin {
        store,
        executor: Arc::new(IoTaskPoolExecutor),
    })
    .insert_resource(PersistSettings {
        autosave: None,
        ..PersistSettings::default()
    });
    let viewer = app
        .world_mut()
        .spawn((
            Transform::from_translation(center(p)),
            ChunkViewer::new(1).with_vertical_radius(1),
        ))
        .id();
    settle(&mut app);
    (app, viewer)
}

/// Updates until the world is settled (60 s max).
fn settle(app: &mut App) {
    let start = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "not settled");
    }
}

fn block(app: &App, p: BlockPos) -> Option<BlockState> {
    app.world().resource::<ChunkMap>().block_at(p)
}

/// Sets `p` to `state` and updates until it is in the map.
fn edit(app: &mut App, p: BlockPos, state: BlockState) {
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(p, state);
    let start = Instant::now();
    while block(app, p) != Some(state) {
        app.update();
        assert!(start.elapsed() < Duration::from_secs(60), "not edited");
    }
}

/// The exit while the world is open: an edit whose chunk just unloaded (its save may still be
/// running on the IO pool) and an edit in a loaded chunk are both written, a game system
/// ordered before the close writes its value, one ordered after it gets the closed error, and
/// the file is closed cleanly while the app and its handles of the store still live.
#[test]
fn the_exit_saves_everything_then_closes_the_file() {
    let tmp = TempDir::new("exit");
    let registry = Arc::new(boxel_demo::blocks::registry());
    let stone = registry.by_name("boxel:stone").unwrap();
    let o = BlockPos::new(1 << 40, 2, -(1 << 40));
    let away = at(o, 1 << 13, 0, 0);
    let (a, b) = (at(o, 1, 0, 1), at(away, 1, 0, 1));

    let (mut app, viewer) = start(&tmp.0, &registry, o);
    app.init_resource::<Late>().add_systems(
        Shutdown,
        (
            save_game.before(WorldShutdownSystems::Close),
            write_late.after(WorldShutdownSystems::Close),
        ),
    );
    edit(&mut app, a, stone);
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .unwrap()
        .translation = center(away);
    let began = Instant::now();
    while app
        .world()
        .resource::<ChunkMap>()
        .id_at(ChunkPos::containing(a))
        .is_some()
        || block(&app, b).is_none()
    {
        app.update();
        assert!(began.elapsed() < Duration::from_secs(60), "not moved");
    }
    edit(&mut app, b, stone);
    app.shutdown();

    let store = app.world().resource::<WorldStore>().clone();
    assert!(store.is_closed());
    assert!(store.handles() > 2, "the app's handles live");
    assert_eq!(
        app.world().resource::<Late>().0,
        Some(io::ErrorKind::NotConnected)
    );
    drop(open_clean(&tmp.0.join(WORLD_FILE)).expect("released and clean"));
    drop(store);
    drop(app);

    let (mut app, viewer) = start(&tmp.0, &registry, o);
    assert_eq!(block(&app, a), Some(stone), "saved when it unloaded");
    {
        let store = app.world().resource::<WorldStore>();
        assert_eq!(
            store.meta("game:exit").unwrap(),
            Some(b"before the close".to_vec())
        );
        assert_eq!(store.meta("game:late").unwrap(), None);
    }
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .unwrap()
        .translation = center(away);
    let began = Instant::now();
    while block(&app, b).is_none() {
        app.update();
        assert!(began.elapsed() < Duration::from_secs(60), "not loaded");
    }
    assert_eq!(block(&app, b), Some(stone), "saved by the exit");
}
