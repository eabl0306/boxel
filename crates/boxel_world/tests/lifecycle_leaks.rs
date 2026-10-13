//! What a closed world leaves behind (ADR 0034, issue #58, tests 21 to 23), in its own binary
//! with a counting allocator: within 2 s of `WorldEvent::Closed` the world's generator and its
//! chunk data are freed and the test's store handle is the last one; 30 worlds opened, streamed
//! and closed in a row leave the live heap, the open files and the threads as they were after
//! the third, and the chunk map's slots at the first world's high-water mark. The worlds are
//! opened at runtime (the build-time world's generator stays in its plugin), with the task
//! pools, the far LOD and a store (in memory, or redb in a temporary folder), at 2^40.
//!
//! The tests share the process's heap, files and threads, so each one runs alone (a lock).
use boxel_app::App;
use boxel_demo::worldgen::{ForestGenerator, TerrainSettings};
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, ChunkData};
use boxel_world::jobs::{IoTaskPoolExecutor, TaskPoolExecutor};
use boxel_world::lifecycle::{
    CloseOptions, WorldLifecycle, WorldScoped, WorldSpec, WorldStatus, close_world, open_world,
};
use boxel_world::lod::{LodSettings, LodWorldPlugin};
use boxel_world::persist::{ChunkStore, MemoryStore, PersistPlugin, PersistSettings, WorldStore};
use boxel_world::{ChunkMap, ChunkViewer, EditQueue, WorldPlugin, WorldSettings, WorldStats};
use boxel_worldgen::WorldGenerator;
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

/// The system allocator, counting the bytes allocated and not freed yet.
struct Counting;

/// Live heap bytes of the process (allocated minus freed).
static LIVE: AtomicIsize = AtomicIsize::new(0);

#[expect(unsafe_code, reason = "a global allocator is an unsafe trait")]
// SAFETY: every call goes to `System` with the caller's arguments unchanged; the counter is
// only arithmetic on the sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc`, passed on.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc_zeroed`, passed on.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc`, passed on.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller's contract for `realloc`, passed on.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            LIVE.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
        }
        new
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// One test at a time: they measure the whole process.
static ALONE: Mutex<()> = Mutex::new(());

/// 2^40 blocks east and south: the worlds sit there.
const FAR: i64 = 1 << 40;

/// How long the closed world's memory and handles may take to go after `Closed`.
const RELEASE: Duration = Duration::from_secs(2);

/// Worlds opened and closed in a row (tests 22 and 23), and the one they are compared with.
const CYCLES: usize = 30;
const BASELINE: usize = 3;

/// The most the live heap may grow from the third world to the last (test 22).
const HEAP_SLACK: isize = 4 << 20;

/// A fresh directory for a redb world (under the target dir), removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("lifecycle-leaks-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Where a world is saved.
enum Saves {
    /// A memory store, opened again on the same backend for every world.
    Memory(Arc<MemoryStore>),
    /// A redb file in this folder, opened again for every world.
    Redb(TempDir),
}

impl Saves {
    fn open(&self, registry: &BlockRegistry) -> WorldStore {
        match self {
            Saves::Memory(backend) => WorldStore::open(
                Arc::clone(backend) as Arc<dyn ChunkStore>,
                registry,
                "forest",
            ),
            Saves::Redb(dir) => WorldStore::open_dir(&dir.0, registry, "forest"),
        }
        .expect("the world's store opens")
    }
}

/// A closed app: the world's jobs and the LOD's on the task pool, saves on the IO pool (no
/// autosave), updated once.
fn app(registry: &Arc<BlockRegistry>) -> App {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        executor: Arc::new(TaskPoolExecutor),
        ..WorldPlugin::closed(Arc::clone(registry))
    })
    .add_plugins(PersistPlugin::without_store().with_executor(Arc::new(IoTaskPoolExecutor)))
    .add_plugins(LodWorldPlugin {
        settings: LodSettings {
            ring_radius: 8,
            levels: 2,
            ..LodSettings::default()
        },
        executor: Arc::new(TaskPoolExecutor),
    })
    .insert_resource(PersistSettings {
        autosave: None,
        ..PersistSettings::default()
    });
    app.update();
    app
}

/// The middle of the chunk column `(cx, cz)` from [`FAR`], above the forest's ground.
fn spot(cx: i64, cz: i64) -> DVec3 {
    DVec3::new(
        (FAR + cx * 32 + 16) as f64,
        80.0,
        (-FAR + cz * 32 + 16) as f64,
    )
}

/// Opens a forest of `seed` saved in `saves`, its viewer (radius 4) at `(cx, cz)`, and waits
/// for it to settle. Returns the generator and the store the test keeps.
fn open(
    app: &mut App,
    registry: &BlockRegistry,
    saves: &Saves,
    seed: u64,
    cx: i64,
) -> (Arc<dyn WorldGenerator>, WorldStore) {
    let generator: Arc<dyn WorldGenerator> =
        Arc::new(ForestGenerator::new(seed, TerrainSettings::default()));
    let store = saves.open(registry);
    let spec =
        WorldSpec::new(Arc::clone(&generator), WorldSettings::default()).with_store(store.clone());
    open_world(app.world_mut(), spec).expect("the world opens");
    app.world_mut().spawn((
        Transform::from_translation(spot(cx, 0)),
        ChunkViewer::new(4).with_vertical_radius(2),
        WorldScoped,
    ));
    let began = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            break;
        }
        assert!(
            began.elapsed() < Duration::from_secs(120),
            "not settled: {stats:?}"
        );
        std::thread::yield_now();
    }
    (generator, store)
}

/// Puts stone (`stone`) or air at a block of the column of [`open`]'s viewer at `(cx, 0)`: one
/// edited chunk to save at the close (the same block at each place, so the store does not
/// grow).
fn edit(app: &mut App, cx: i64, stone: bool) {
    let content = app.world().resource::<boxel_world::WorldContent>();
    let registry = &content.registry;
    let state = if stone {
        registry.by_name("boxel:stone")
    } else {
        Some(boxel_voxel::BlockState::AIR)
    }
    .expect("stone");
    let at = BlockPos::new(FAR + cx * 32 + 16, 70, -FAR + 16);
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(at, state);
    app.update();
}

/// Closes the open world and updates until `WorldEvent::Closed`.
fn close(app: &mut App) {
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    let began = Instant::now();
    while app.world().resource::<WorldLifecycle>().status() != WorldStatus::Closed {
        app.update();
        assert!(began.elapsed() < Duration::from_secs(30), "not closed");
        std::thread::sleep(Duration::from_millis(1));
    }
    let report = app
        .world()
        .resource::<WorldLifecycle>()
        .last_report()
        .expect("a report");
    assert!(report.saved && report.errors.is_empty(), "{report:?}");
}

/// Waits up to [`RELEASE`] for `done`; true if it held in time.
fn within_release(mut done: impl FnMut() -> bool) -> bool {
    let began = Instant::now();
    loop {
        if done() {
            return true;
        }
        if began.elapsed() > RELEASE {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A chunk of the open world whose data is not one block state (uniform chunks share the
/// app-wide snapshots kept on purpose).
fn mixed_chunk(app: &App) -> Weak<ChunkData> {
    let map = app.world().resource::<ChunkMap>();
    map.iter()
        .find_map(|(_, slot)| {
            let data = map.data_at(slot.key().pos)?;
            data.as_uniform().is_none().then(|| Arc::downgrade(data))
        })
        .expect("a mixed chunk")
}

/// Entries of a `/proc/self` folder (open files, threads).
#[cfg(target_os = "linux")]
fn count(dir: &str) -> usize {
    std::fs::read_dir(dir).map_or(0, Iterator::count)
}

/// Test 21: within 2 s of `Closed`, the closed world's generator and a chunk's data are freed,
/// and the store the test kept is the last handle of it, with a memory store and with redb.
#[test]
fn a_closed_world_frees_its_generator_its_chunks_and_its_store() {
    let _alone = ALONE.lock().unwrap_or_else(PoisonError::into_inner);
    let registry = Arc::new(boxel_demo::blocks::registry());
    let tmp = TempDir::new("probes");
    for saves in [
        Saves::Memory(Arc::new(MemoryStore::new())),
        Saves::Redb(tmp),
    ] {
        let mut app = app(&registry);
        let (generator, store) = open(&mut app, &registry, &saves, 7, 0);
        edit(&mut app, 0, true);
        let generator = {
            let weak = Arc::downgrade(&generator);
            drop(generator);
            weak
        };
        let chunk = mixed_chunk(&app);
        close(&mut app);
        assert!(
            within_release(|| generator.strong_count() == 0),
            "the generator is still held"
        );
        assert!(
            within_release(|| chunk.strong_count() == 0),
            "a chunk's data is still held"
        );
        assert!(
            within_release(|| store.handles() == 1),
            "{} handles of the store",
            store.handles()
        );
        assert!(store.is_closed());
    }
}

/// What a closed app holds after one world.
#[derive(Clone, Copy, Debug)]
struct Held {
    heap: isize,
    files: usize,
    threads: usize,
    slots: usize,
}

/// Opens, streams (radius 4, an edit saved at each close) and closes [`CYCLES`] worlds in
/// `saves`, two in turn (another seed at another place), and returns what the app held after
/// the first, the third and the last.
fn cycles(saves: &Saves) -> [Held; 3] {
    let registry = Arc::new(boxel_demo::blocks::registry());
    let mut app = app(&registry);
    let mut held = Vec::new();
    for cycle in 1..=CYCLES {
        let cx = (cycle % 2) as i64 * 8;
        let (generator, store) = open(&mut app, &registry, saves, cycle as u64 % 2, cx);
        edit(&mut app, cx, (cycle / 2) % 2 == 0);
        let generator = {
            let weak = Arc::downgrade(&generator);
            drop(generator);
            weak
        };
        close(&mut app);
        assert!(
            within_release(|| generator.strong_count() == 0 && store.handles() == 1),
            "world {cycle} is still held"
        );
        drop(store);
        if [1, BASELINE, CYCLES].contains(&cycle) {
            #[cfg(target_os = "linux")]
            let (files, threads) = (count("/proc/self/fd"), count("/proc/self/task"));
            #[cfg(not(target_os = "linux"))]
            let (files, threads) = (0, 0);
            held.push(Held {
                heap: LIVE.load(Ordering::Relaxed),
                files,
                threads,
                slots: app.world().resource::<ChunkMap>().slot_capacity(),
            });
        }
    }
    held.try_into().expect("three samples")
}

/// Test 22: 30 worlds in a row leave the live heap within 4 MiB of where it was after the
/// third (the LOD's per-worker caches and the shared uniform chunks are full by then).
#[test]
fn thirty_worlds_leave_the_heap_as_it_was() {
    let _alone = ALONE.lock().unwrap_or_else(PoisonError::into_inner);
    let [_, third, last] = cycles(&Saves::Memory(Arc::new(MemoryStore::new())));
    let grown = last.heap - third.heap;
    assert!(
        grown <= HEAP_SLACK,
        "the heap grew by {grown} bytes from world {BASELINE} to world {CYCLES} ({third:?} -> \
         {last:?})"
    );
}

/// Test 23: 30 redb worlds in a row leave the open files and the threads as they were after
/// the third (the pools start their threads during the first ones), and the chunk map's slots
/// at the first world's high-water mark.
#[test]
fn thirty_worlds_leave_no_file_thread_or_slot_behind() {
    let _alone = ALONE.lock().unwrap_or_else(PoisonError::into_inner);
    let tmp = TempDir::new("handles");
    let [first, third, last] = cycles(&Saves::Redb(tmp));
    #[cfg(target_os = "linux")]
    {
        assert_eq!(last.files, third.files, "open files: {third:?} -> {last:?}");
        assert_eq!(
            last.threads, third.threads,
            "threads: {third:?} -> {last:?}"
        );
    }
    assert_eq!(last.slots, first.slots, "slots: {first:?} -> {last:?}");
}
