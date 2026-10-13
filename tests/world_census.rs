//! The census of the engine's resources (ADR 0034, issue #58): every resource of a full
//! headless app (world, saves, far LOD, physics, block ticks and fluids, the clock, the
//! renderer without a GPU, fog, the debug overlay and, with `ui`, the game UI), messages
//! included, belongs to one world (registered with `WorldScopeApp`, so a world's close empties
//! it) or to the app (listed in [`APP_WIDE`] below). A new resource that is neither fails here:
//! decide whether it is per world (register it and reset it in a `TeardownSystems` set) or the
//! app's (add it to the list).
//!
//! The resources are taken with no world, with a saved world open and after its close, so the
//! ones that exist only while a world is open are seen too.
#![cfg(all(feature = "render", feature = "voxel", feature = "physics"))]

use boxel::app::{App, TaskPoolPlugin, TimePlugin};
use boxel::debug_overlay::DebugOverlayPlugin;
use boxel::diagnostic::FrameStatsPlugin;
use boxel::ecs::prelude::*;
use boxel::math::DVec3;
use boxel::physics::PhysicsPlugin;
use boxel::render::RenderPlugin;
use boxel::render::fog::FogPlugin;
use boxel::render::lod::LodPlugin;
use boxel::transform::Transform;
use boxel::window::InputPlugin;
use boxel::world::jobs::InlineExecutor;
use boxel::world::lifecycle::{
    CloseOptions, WorldScope, WorldScoped, WorldSpec, close_world, finish_closing, open_world,
};
use boxel::world::persist::{MemoryStore, PersistPlugin, WorldStore};
use boxel::world::{ChunkViewer, DaylightPlugin, FluidPlugin, WorldPlugin, WorldSettings};
use std::any::TypeId;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// Per world, without a registration: the store is the world (inserted at the open, removed at
/// the close), and the open's context exists only while `WorldOpen` runs.
const PER_WORLD: &[&str] = &[
    "boxel_world::lifecycle::OpenContext",
    "boxel_world::persist::store::WorldStore",
];

/// The app's resources: settings and knobs, what the registry makes, the engine's machinery and
/// the per-app state of the renderer, input, time and UI. `WorldSettings`, `TimeOfDay` and
/// `FogClock` are not emptied at a close but set at every open (the spec's settings, the saved
/// or start clock, the fog's tick 0). Sorted, no duplicates.
const APP_WIDE: &[&str] = &[
    "boxel_app::idle::IdleWanted",
    "boxel_app::schedules::FixedMainScheduleOrder",
    "boxel_app::schedules::MainScheduleOrder",
    "boxel_app::time::FixedTime",
    "boxel_app::time::Time",
    "boxel_debug::Crosshair",
    "boxel_debug::DebugOverlay",
    "boxel_debug::FrameWindow",
    "boxel_debug::OverlayLines",
    "boxel_debug::PresentWindow",
    "boxel_diagnostic::DiagnosticsBoard",
    "boxel_diagnostic::FrameStats",
    "boxel_diagnostic::history::FrameTimeHistory",
    "boxel_ecs::entity_disabling::DefaultQueryFilters",
    "boxel_ecs::message::message_registry::MessageRegistry",
    "boxel_ecs::message::messages::Messages<boxel_app::app::AppExit>",
    "boxel_ecs::message::messages::Messages<boxel_render::plugin::DebugDestroyDevice>",
    "boxel_ecs::message::messages::Messages<boxel_render::plugin::GpuFailed>",
    "boxel_ecs::message::messages::Messages<boxel_render::plugin::GpuReady>",
    "boxel_ecs::message::messages::Messages<boxel_render::plugin::RequestScreenshot>",
    "boxel_ecs::message::messages::Messages<boxel_render::plugin::ScreenshotSaved>",
    "boxel_ecs::message::messages::Messages<boxel_window::input::CursorMoved>",
    "boxel_ecs::message::messages::Messages<boxel_window::input::KeyboardInput>",
    "boxel_ecs::message::messages::Messages<boxel_window::input::MouseButtonInput>",
    "boxel_ecs::message::messages::Messages<boxel_window::input::MouseMotion>",
    "boxel_ecs::message::messages::Messages<boxel_window::input::MouseWheel>",
    "boxel_ecs::message::messages::Messages<boxel_window::window::WindowFocused>",
    "boxel_ecs::message::messages::Messages<boxel_world::lifecycle::WorldEvent>",
    "boxel_ecs::schedule::schedule::Schedules",
    "boxel_physics::components::PhysicsSettings",
    "boxel_physics::plugin::PhysicsContent",
    "boxel_render::fog::FogSettings",
    "boxel_render::fog::plugin::FogClock",
    "boxel_render::overlay::OverlayDraw",
    "boxel_render::plugin::ExtractedFrame",
    "boxel_render::plugin::GpuState",
    "boxel_render::plugin::InstanceState",
    "boxel_render::plugin::LkgRes",
    "boxel_render::plugin::Losses",
    "boxel_render::plugin::PresentSettings",
    "boxel_render::plugin::RenderInfo",
    "boxel_render::plugin::RenderSettings",
    "boxel_render::plugin::SurfaceInfo",
    "boxel_ui::atlas::UiAtlas",
    "boxel_ui::font::BitmapFont",
    "boxel_ui::memory::UiState",
    "boxel_ui::nav::InputCapture",
    "boxel_ui::paint::UiDraw",
    "boxel_ui::scale::GuiScale",
    "boxel_ui::skin::UiSkin",
    "boxel_ui::skin::UiStyle",
    "boxel_window::input::AccumulatedMouseMotion",
    "boxel_window::input::AccumulatedMouseScroll",
    "boxel_window::input::ButtonInput<winit::event::MouseButton>",
    "boxel_window::input::ButtonInput<winit::keyboard::KeyCode>",
    "boxel_window::input::CursorPosition",
    "boxel_world::daylight::DaylightCurve",
    "boxel_world::daylight::TimeOfDay",
    "boxel_world::lifecycle::WorldEpoch",
    "boxel_world::lifecycle::WorldGarbage",
    "boxel_world::lifecycle::WorldLifecycle",
    "boxel_world::lifecycle::driver::StartClock",
    "boxel_world::lifecycle::scope::WorldScope",
    "boxel_world::lod::LodSettings",
    "boxel_world::neighbor_rules::BlockUses",
    "boxel_world::neighbor_rules::NeighborRules",
    "boxel_world::persist::entities::EntityCodecs",
    "boxel_world::persist::plugin::PersistExecutor",
    "boxel_world::persist::plugin::PersistSettings",
    "boxel_world::plugin::ResyncCursor",
    "boxel_world::plugin::WorldSettings",
    "boxel_world::ticks::BlockTickSettings",
];

/// Every resource of the app now, by type name, with whether the world registered it.
fn census(world: &World, seen: &mut BTreeSet<(String, bool)>) {
    let scope = world.resource::<WorldScope>();
    let registered: Vec<TypeId> = scope
        .resources()
        .chain(scope.messages())
        .map(|(id, _)| id)
        .collect();
    for (_, info, _) in world.iter_resources() {
        let per_world = info.type_id().is_some_and(|id| registered.contains(&id));
        seen.insert((info.name().to_string(), per_world));
    }
}

fn app() -> App {
    let registry = Arc::new(boxel_demo::blocks::registry());
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        TimePlugin,
        FrameStatsPlugin,
        InputPlugin,
    ))
    .add_plugins(WorldPlugin {
        executor: Arc::new(InlineExecutor),
        ..WorldPlugin::closed(Arc::clone(&registry))
    })
    .add_plugins((
        DaylightPlugin,
        FluidPlugin,
        PersistPlugin::without_store().with_executor(Arc::new(InlineExecutor)),
        PhysicsPlugin::default(),
        RenderPlugin::default(),
        LodPlugin {
            executor: Arc::new(InlineExecutor),
            ..LodPlugin::default()
        },
        FogPlugin::default(),
        DebugOverlayPlugin::default(),
    ));
    #[cfg(feature = "ui")]
    app.add_plugins(boxel::ui::UiPlugin);
    app.finish();
    app.cleanup();
    app
}

#[test]
fn every_resource_belongs_to_a_world_or_to_the_app() {
    let mut app = app();
    let mut seen = BTreeSet::new();
    app.update();
    census(app.world(), &mut seen);
    let store = WorldStore::open(
        Arc::new(MemoryStore::new()),
        &boxel_demo::blocks::registry(),
        "census",
    )
    .unwrap();
    let spec = WorldSpec::new(
        Arc::new(boxel_demo::worldgen::TerrainGenerator::default()),
        WorldSettings::default(),
    )
    .with_store(store);
    open_world(app.world_mut(), spec).unwrap();
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(0.0, 80.0, 0.0)),
        ChunkViewer::new(1).with_vertical_radius(1),
        WorldScoped,
    ));
    for _ in 0..5 {
        app.update();
    }
    census(app.world(), &mut seen);
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    assert!(finish_closing(app.world_mut(), Duration::from_secs(10)));
    app.update();
    census(app.world(), &mut seen);

    let unclassified: Vec<&str> = seen
        .iter()
        .filter(|(name, per_world)| {
            !per_world && !APP_WIDE.contains(&name.as_str()) && !PER_WORLD.contains(&name.as_str())
        })
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(
        unclassified.is_empty(),
        "resources neither per world nor on the app-wide list:\n{}",
        unclassified.join("\n")
    );
    // Registered per world and listed as the app's: one or the other.
    let both: Vec<&str> = seen
        .iter()
        .filter(|(name, per_world)| *per_world && APP_WIDE.contains(&name.as_str()))
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(both.is_empty(), "per world and app-wide: {both:?}");
}

#[test]
fn the_app_wide_list_is_sorted_without_duplicates() {
    assert!(APP_WIDE.windows(2).all(|w| w[0] < w[1]));
    assert!(PER_WORLD.windows(2).all(|w| w[0] < w[1]));
}
