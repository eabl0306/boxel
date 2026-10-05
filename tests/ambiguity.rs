//! The engine's systems are not ambiguous with each other.
//!
//! An app with the headless engine plugins, the fly camera and the world (inline jobs) runs with
//! `ambiguity_detection: LogLevel::Error` on `First`, `PreUpdate`, `Update`, `PostUpdate` and
//! `Last`: building those schedules on the first `app.update()` panics on any pair of systems
//! with conflicting access and no order between them.
//!
//! `First` holds `time_system` and the exclusive `message_update_system`; `TimePlugin` marks
//! them ambiguous with each other, as Bevy does (their order does not matter).
//!
//! Fly camera vs world: `FlyCameraSystems` writes the camera `Transform` and
//! `WorldSystems::Interest` reads viewer transforms, both in `Update`, from crates that do not
//! know each other. The world marks its viewer tracker `ambiguous_with_all` (interest lags one
//! frame at most), so the pair is not ambiguous without help from the app. Apps that want the
//! interest of a frame to follow the camera move of the same frame add
//! `configure_sets(Update, FlyCameraSystems.before(WorldSystems::Interest))` (recommended for
//! `voxel_world`); that order is tested here too.
//!
//! Needs the `window` and `voxel` features (both in the default set).
#![cfg(all(feature = "window", feature = "voxel"))]

use boxel::app::{App, First, Last, PostUpdate, PreUpdate, TaskPoolPlugin, TimePlugin, Update};
use boxel::camera::{FlyCamera, FlyCameraPlugin, FlyCameraSystems};
use boxel::diagnostic::FrameStatsPlugin;
use boxel::ecs::prelude::*;
use boxel::ecs::schedule::{LogLevel, ScheduleBuildSettings};
use boxel::math::DVec3;
use boxel::transform::Transform;
use boxel::voxel::{BlockPos, ChunkPos};
use boxel::window::{ButtonInput, InputPlugin, KeyCode};
use boxel::world::jobs::InlineExecutor;
use boxel::world::{ChunkMap, ChunkViewer, Interest, WorldPlugin, WorldStats, WorldSystems};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// Ambiguities between systems of this schedule are errors (the schedule fails to build).
fn strict(schedule: &mut Schedule) {
    schedule.set_build_settings(ScheduleBuildSettings {
        ambiguity_detection: LogLevel::Error,
        ..ScheduleBuildSettings::default()
    });
}

/// The app under test: engine plugins with strict ambiguity detection on the main schedules
/// that hold engine systems (`First` included).
fn strict_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        TimePlugin,
        FrameStatsPlugin,
        InputPlugin,
        FlyCameraPlugin,
        WorldPlugin {
            executor: Arc::new(InlineExecutor),
            ..WorldPlugin::default()
        },
    ));
    app.edit_schedule(First, strict)
        .edit_schedule(PreUpdate, strict)
        .edit_schedule(Update, strict)
        .edit_schedule(PostUpdate, strict)
        .edit_schedule(Last, strict);
    app
}

/// A fly camera that is also the world's viewer (as in `voxel_world`), at the corner of chunk
/// `(0, 2, 0)` looking down -Z: any forward move enters chunk `(0, 2, -1)`.
fn spawn_camera_viewer(app: &mut App) -> Entity {
    app.world_mut()
        .spawn((
            Transform::from_translation(DVec3::new(0.5, 80.0, 0.0)),
            FlyCamera::default(),
            ChunkViewer::new(0).with_vertical_radius(0),
        ))
        .id()
}

#[test]
fn engine_systems_are_not_ambiguous() {
    let mut app = strict_app();
    spawn_camera_viewer(&mut app);
    for _ in 0..3 {
        app.update();
    }
    // The systems did run: the world streamed chunks around the viewer.
    assert!(!app.world().resource::<ChunkMap>().is_empty());
    assert!(app.world().resource::<WorldStats>().chunks > 0);
}

#[test]
fn camera_before_interest_is_a_valid_order() {
    let mut app = strict_app();
    app.configure_sets(Update, FlyCameraSystems.before(WorldSystems::Interest));
    let camera = spawn_camera_viewer(&mut app);
    app.update();
    let start = ChunkPos::new(0, 2, 0);
    let mesh_at = |app: &App, pos: ChunkPos| {
        let map = app.world().resource::<ChunkMap>();
        map.id_at(pos)
            .and_then(|id| map.get(id))
            .is_some_and(|slot| slot.interest.contains(Interest::MESH))
    };
    assert!(mesh_at(&app, start), "radius 0 meshes the viewer column");

    // Hold W: the camera moves forward (-Z) by speed * delta, which is > 0 from the second
    // update on, so it crosses into the next chunk. With the order above, the world computes
    // interest from the new position in the same frame.
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::KeyW);
    app.update();
    let moved = app
        .world()
        .get::<Transform>(camera)
        .expect("camera")
        .translation;
    let now = ChunkPos::containing(BlockPos::from_world(moved));
    assert_eq!(now, ChunkPos::new(0, 2, -1), "camera moved to {moved:?}");
    assert!(
        mesh_at(&app, now),
        "interest follows the camera in the same frame"
    );
    assert!(
        !mesh_at(&app, start),
        "the old column lost its mesh interest"
    );
}

/// Control: detection is really on in each of the five schedules (an unordered pair of writers
/// of one resource fails the build).
#[test]
fn ambiguity_detection_is_active() {
    #[derive(Resource, Default)]
    struct Counter(u32);
    fn bump_a(mut counter: ResMut<Counter>) {
        counter.0 += 1;
    }
    fn bump_b(mut counter: ResMut<Counter>) {
        counter.0 += 2;
    }

    let cases: [(&str, fn(&mut App)); 5] = [
        ("First", |app| {
            app.add_systems(First, (bump_a, bump_b));
        }),
        ("PreUpdate", |app| {
            app.add_systems(PreUpdate, (bump_a, bump_b));
        }),
        ("Update", |app| {
            app.add_systems(Update, (bump_a, bump_b));
        }),
        ("PostUpdate", |app| {
            app.add_systems(PostUpdate, (bump_a, bump_b));
        }),
        ("Last", |app| {
            app.add_systems(Last, (bump_a, bump_b));
        }),
    ];
    for (name, add_pair) in cases {
        let mut app = strict_app();
        app.init_resource::<Counter>();
        add_pair(&mut app);
        let built = catch_unwind(AssertUnwindSafe(|| app.update()));
        assert!(
            built.is_err(),
            "{name}: the ambiguous pair was not reported"
        );
    }
}
