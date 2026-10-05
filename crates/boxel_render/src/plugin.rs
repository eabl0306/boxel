use boxel_app::{App, Plugin};
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::ScheduleLabel;
use boxel_gfx::{DeviceClass, GpuPreferences, SmokeLevel, Tier, TierPolicy};
use std::path::PathBuf;

/// Render schedule, inserted into `MainScheduleOrder` right after `Last` (same world, M0).
#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Render;

/// Chained sets of the `Render` schedule (single-threaded executor, main thread).
///
/// Extract boundary (frozen, prepares the M3 render thread): only `Setup`, `Extract` and
/// `Cleanup` read or write the main world. `Extract` copies everything the frame needs into a
/// private `ExtractedFrame`; `Prepare` and `Draw` read only `ExtractedFrame`, the private GPU
/// state and `NonSend<PrimaryWindow>` (for `pre_present_notify`).
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderSystems {
    /// GPU bootstrap / device-loss recovery / surface resize.
    Setup,
    /// Copy the frame inputs (camera, window state, mesh events or a full resync from
    /// `ChunkMap`, screenshot and debug requests) into `ExtractedFrame`.
    Extract,
    /// `ExtractedFrame` -> `ViewParams`, chunk uploads.
    Prepare,
    /// Encode tier passes; acquire, blit, submit, present when the frame gate allows it.
    Draw,
    /// Write back: `WindowState::frame_ready`, screenshots, LKG `Good` after the first
    /// presented frame, `RenderInfo` and diagnostics.
    Cleanup,
}

/// Renderer configuration (inserted as a resource by [`RenderPlugin`]).
#[derive(Resource, Clone, Debug)]
pub struct RenderSettings {
    /// Namespace of the last-known-good file.
    pub app_name: String,
    /// CLI-level GPU preferences (highest precedence; env/config are merged under it).
    pub gpu: GpuPreferences,
    /// GPU config file (else `BOXEL_GPU_CONFIG`).
    pub gpu_config: Option<PathBuf>,
    /// Smoke test depth during selection (`Auto`: `Draw` on GL and software adapters).
    pub smoke_level: SmokeLevel,
    /// Tier classification policy.
    pub tier_policy: TierPolicy,
    /// Sky/clear color (linear RGBA); also the fog color.
    pub clear_color: [f32; 4],
    /// Fog starts at this fraction of `Camera::view_distance` and ends at the distance.
    pub fog_start_fraction: f32,
    /// Quad arena capacity (8 bytes per quad). Default 8 Mi quads (64 MiB).
    pub arena_quads: u32,
    /// Chunk table capacity (render chunk slots). Default 32768.
    pub max_chunk_slots: u32,
    /// Automation: while the window is hidden or no frame is ready, still render the main color
    /// target (no acquire/present) and count it in `RenderInfo::frames_rendered`, so scripted
    /// runs (`voxel_world --frames`) progress on a covered window. Default false.
    pub render_when_hidden: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            app_name: "boxel".to_string(),
            gpu: GpuPreferences::default(),
            gpu_config: None,
            smoke_level: SmokeLevel::Auto,
            tier_policy: TierPolicy::default(),
            clear_color: [0.30, 0.52, 0.85, 1.0],
            fog_start_fraction: 0.6,
            arena_quads: 8 << 20,
            max_chunk_slots: 32768,
            render_when_hidden: false,
        }
    }
}

/// Public renderer status (read by apps, examples, diagnostics).
#[derive(Resource, Clone, Debug, Default)]
pub struct RenderInfo {
    /// A device is selected and the renderer is alive.
    pub ready: bool,
    /// Selected tier.
    pub tier: Option<Tier>,
    /// Device class.
    pub class: Option<DeviceClass>,
    /// `GpuContext::summary_line()`.
    pub summary: String,
    /// Frames presented since startup.
    pub frames_presented: u64,
    /// Frames rendered into the main color target (presented or not, see
    /// `RenderSettings::render_when_hidden`).
    pub frames_rendered: u64,
    /// Chunks resident on the GPU.
    pub chunks: u32,
    /// CPU-side draws last frame (Mid/Low; High reports 1 indirect call).
    pub draws: u32,
    /// Device losses recovered from.
    pub recoveries: u32,
    /// `RenderStats::dropped_chunks` (arena or chunk table full; warn-logged).
    pub dropped_chunks: u32,
}

/// Sent once per successful (re)bootstrap.
#[derive(Message, Clone, Debug)]
pub struct GpuReady {
    /// `GpuContext::summary_line()`.
    pub summary: String,
    /// Tier.
    pub tier: Tier,
}

/// Sent when no GPU could be selected (the app decides whether to exit).
#[derive(Message, Clone, Debug)]
pub struct GpuFailed {
    /// Error text including the selection report.
    pub error: String,
}

/// Ask for a PNG of the main color target at the end of this frame.
#[derive(Message, Clone, Debug)]
pub struct RequestScreenshot {
    /// Output path.
    pub path: PathBuf,
}

/// Result of a [`RequestScreenshot`].
#[derive(Message, Clone, Debug)]
pub struct ScreenshotSaved {
    /// Output path.
    pub path: PathBuf,
    /// Outcome.
    pub result: Result<(), String>,
}

/// Debug/verification: destroys the current `wgpu::Device` at the end of this frame to exercise
/// the device-lost recovery path (`voxel_world --simulate-device-loss <frame>`).
#[derive(Message, Clone, Copy, Debug, Default)]
pub struct DebugDestroyDevice;

/// Adds the renderer. Requires `WindowPlugin` (for `DisplayHandle`/`PrimaryWindow`) and
/// `boxel_app` time. Optional integrations: `boxel_world::WorldPlugin` (mesh events),
/// `boxel_diagnostic` (`"gpu"`/`"render"` lines), `boxel_camera::Camera`. The `Render`
/// schedule is configured with `App::edit_schedule` + `SingleThreadedExecutor` (keeps systems
/// other plugins added earlier and keeps GPU/window calls on the main thread).
#[derive(Clone, Debug, Default)]
pub struct RenderPlugin {
    /// Settings.
    pub settings: RenderSettings,
}

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        todo!("WP7")
    }
}
