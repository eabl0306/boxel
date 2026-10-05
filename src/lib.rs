//! # boxel
//!
//! A Bevy-style voxel engine. This facade re-exports the engine crates and provides
//! [`DefaultPlugins`], [`MinimalPlugins`] and a [`prelude`].
//!
//! Derive macros (`Component`, `Resource`, `Message`, `SystemSet`, `ScheduleLabel`...) resolve
//! `::boxel::ecs` when your crate depends on `boxel` (under that exact name).
//!
//! ECS notes:
//! - In `ParamSet`, spell query lifetimes out:
//!   `ParamSet<(Query<&'static mut A>, Query<&'static B>)>`. The elided form fails with
//!   error E0477 ("the type `&mut A` does not fulfill the required lifetime"): rustc's
//!   compatibility hack only recognizes a crate named `bevy_ecs` (see docs/architecture.md).
//! - Resources are entities (`bevy_ecs` 0.20): `World::entities` counts and `Query<Entity>` see
//!   them too.
//! - The window is a non-send resource: use `NonSend<PrimaryWindow>` (main thread). Calling
//!   winit window methods from parallel systems deadlocks on macOS and Windows.
//!
//! Frame rate: [`app::FrameLimit`] (in the prelude) caps updates per second, 144 by default and
//! 0 for unlimited; `render::PresentSettings` (feature `render`, also in the prelude) switches
//! vsync, off by default. Both are resources that any system can change at runtime, for
//! example from a settings menu.
//!
//! Features: `window`, `render`, `voxel`, `png` and `debug` (ECS diagnostics with system and
//! component names) are on by default; `trace`/`trace_tracy` add spans; `hal` adds the Vulkan
//! escape hatch (Windows/Linux).

pub use boxel_app as app;
pub use boxel_diagnostic as diagnostic;
pub use boxel_ecs as ecs;
pub use boxel_log as log;
pub use boxel_math as math;
pub use boxel_tasks as tasks;
pub use boxel_transform as transform;

#[cfg(feature = "window")]
pub use boxel_camera as camera;
#[cfg(feature = "window")]
pub use boxel_window as window;

#[cfg(feature = "render")]
pub use boxel_debug as debug_overlay;
#[cfg(feature = "render")]
pub use boxel_gfx as gfx;
#[cfg(feature = "render")]
pub use boxel_render as render;
#[cfg(feature = "render")]
pub use boxel_shaders as shaders;

#[cfg(feature = "voxel")]
pub use boxel_mesh as mesh;
#[cfg(feature = "voxel")]
pub use boxel_voxel as voxel;
#[cfg(feature = "voxel")]
pub use boxel_world as world;
#[cfg(feature = "voxel")]
pub use boxel_worldgen as worldgen;

use boxel_app::{PluginGroup, PluginGroupBuilder};

/// Everything a typical app needs.
pub mod prelude {
    pub use crate::{DefaultPlugins, MinimalPlugins};
    pub use boxel_app::prelude::*;
    pub use boxel_diagnostic::{
        DiagnosticsBoard, FrameStats, FrameStatsPlugin, LogDiagnosticsPlugin,
    };
    pub use boxel_ecs::prelude::*;
    pub use boxel_log::LogPlugin;
    pub use boxel_math::prelude::*;
    pub use boxel_transform::prelude::*;

    #[cfg(feature = "window")]
    pub use boxel_camera::prelude::*;
    #[cfg(feature = "window")]
    pub use boxel_window::prelude::*;

    #[cfg(feature = "render")]
    pub use boxel_debug::{DebugOverlay, DebugOverlayPlugin, FrameKeys};
    #[cfg(feature = "render")]
    pub use boxel_gfx::{GpuPreferences, PresentModePref, Tier};
    #[cfg(feature = "render")]
    pub use boxel_render::prelude::*;

    #[cfg(feature = "voxel")]
    pub use boxel_voxel::prelude::*;
    #[cfg(feature = "voxel")]
    pub use boxel_world::prelude::*;
    #[cfg(feature = "voxel")]
    pub use boxel_worldgen::{TerrainGenerator, WorldGenerator};
}

/// Log, task pools, time, frame stats, input, window + winit runner, voxel world, renderer and
/// the F3 debug overlay (each behind its cargo feature). Configure with `.set(...)`, remove with `.disable::<T>()`.
pub struct DefaultPlugins;

impl PluginGroup for DefaultPlugins {
    fn build(self) -> PluginGroupBuilder {
        let group = PluginGroupBuilder::start()
            .add(boxel_log::LogPlugin::default())
            .add(boxel_app::TaskPoolPlugin::default())
            .add(boxel_app::TimePlugin)
            .add(boxel_diagnostic::FrameStatsPlugin);
        #[cfg(feature = "window")]
        let group = group
            .add(boxel_window::InputPlugin)
            .add(boxel_window::WindowPlugin::default());
        #[cfg(feature = "voxel")]
        let group = group.add(boxel_world::WorldPlugin::default());
        #[cfg(feature = "render")]
        let group = group
            .add(boxel_render::RenderPlugin::default())
            .add(boxel_debug::DebugOverlayPlugin::default());
        group
    }
}

/// Headless: task pools, time and the loop runner (16 ms). For servers, tools and tests.
pub struct MinimalPlugins;

impl PluginGroup for MinimalPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start()
            .add(boxel_app::TaskPoolPlugin::default())
            .add(boxel_app::TimePlugin)
            .add(boxel_app::ScheduleRunnerPlugin::run_loop(
                std::time::Duration::from_millis(16),
            ))
    }
}
