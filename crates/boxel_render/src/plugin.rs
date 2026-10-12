//! [`RenderPlugin`]: GPU bootstrap, the extract boundary, mesh extraction, the frame-callback
//! presentation gate, screenshots, device-loss recovery and last-known-good confirmation
//! (ADR 0002 and 0008, `docs/architecture.md` sections 5.8 and 5.9).
//!
//! Private state, all main-world resources touched only by `Render` systems:
//! - `GpuState`: `WaitingForWindow` -> `Ready` (context, surface, renderer) or `Failed`;
//!   `Stopped` after `Shutdown`.
//! - `InstanceState`: resolved preferences and the process' single `CreatedInstance`.
//! - `LkgRes`: the last-known-good store (when enabled).
//! - `Losses`: device losses per adapter key and the session exclusions they produced.
//! - `ExtractedFrame`: everything `Prepare` and `Draw` may read, copied by `Extract`, plus the
//!   frame results `Cleanup` writes back.

use crate::fog::{FogInput, FogSettings};
use crate::lod::{LodFeedback, LodInput, LodRenderer, LodSettings, lod_far};
use crate::outline::{BlockOutline, Outline, OutlineCache};
use crate::overlay::{OverlayDraw, OverlayPass, OverlayRect};
use crate::{
    AllocProbeReport, BlockTextureData, ChunkMemorySettings, ChunkMemoryStats, ChunkOp,
    DEFAULT_SKY_COLOR, DistanceFog, FLUID_TIME_PERIOD, RendererConfig, RendererSmokeTest,
    SceneLight, TextureReport, TextureSource, TranslucentSort, ViewParams, VoxelRenderer,
    generate_block_textures, light_curve, load_block_textures, texture_tick,
};
use boxel_app::{App, Last, MainScheduleOrder, Plugin, Shutdown, Time};
use boxel_camera::{Camera, CameraView};
use boxel_diagnostic::DiagnosticsBoard;
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::{ScheduleLabel, SingleThreadedExecutor};
use boxel_ecs::system::SystemParam;
use boxel_gfx::{
    CreatedInstance, DeviceClass, Exclusion, FrameAcquire, GpuContext, GpuPreferences, LkgStore,
    PresentModePref, SelectError, SelectRequest, SmokeLevel, Tier, TierPolicy, acquire_frame,
    build_fingerprint, configure_surface, create_gl_instance, create_instance,
    install_lkg_panic_hook, read_texture_rgba8, select_gpu,
};
use boxel_math::UVec2;
use boxel_mesh::MaterialTable;
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, BlockRegistryBuilder, ChunkPos};
use boxel_window::{DisplayHandle, PrimaryWindow, WindowState};
use boxel_world::lifecycle::{TeardownSystems, WorldScopeApp, WorldTeardown};
use boxel_world::{
    ChunkMap, ChunkMeshEvent, ChunkMeshResync, ChunkViewer, ChunkVisibility, DaylightCurve,
    TimeOfDay, VisibilityView, VisibleSet, WorldContent, WorldSettings,
};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    /// Sky/clear color (linear RGBA), also the fog color, when the app has no world clock. With
    /// `boxel_world::DaylightPlugin` ([`TimeOfDay`] and [`DaylightCurve`]) the curve gives the sky
    /// color, the sky light and the ambient floor of the current time instead.
    pub clear_color: [f32; 4],
    /// How the terrain fades into the fog color at `Camera::view_distance` (and at the far
    /// LOD's horizon while it draws): only the last 10 %, at most 64 blocks, by default;
    /// `enabled: false` for a hard edge. Read every frame; an invalid change is logged and the
    /// last valid one stays ([`DistanceFog::validate`]).
    pub distance_fog: DistanceFog,
    /// What the most occluded corner loses to ambient occlusion (0.55: it keeps 45 %).
    pub ao_strength: f32,
    /// Multiplier of block light (lamps, lava). Default 1.
    pub block_light_gain: f32,
    /// Brightness curve of the light levels ([`light_curve`]): 0 (the default) is Minecraft's
    /// dark "moody" curve, 1 the brightest.
    pub light_gamma: f32,
    /// How chunk quads are kept on the GPU (ADR 0035): pages of 4 Mi quads (32 MiB), a floor of
    /// 8 Mi quads and 32,768 chunk slots by default. Read when a renderer is built (the first
    /// GPU, a recovered device loss); an invalid value is logged and the defaults are used
    /// ([`ChunkMemorySettings::validate`]).
    pub chunk_memory: ChunkMemorySettings,
    /// Automation: while the window is hidden or no frame is ready, still render the main color
    /// target (no acquire/present) and count it in `RenderInfo::frames_rendered`, so scripted
    /// runs (`voxel_world --frames`) progress on a covered window. Default false.
    pub render_when_hidden: bool,
    /// Where block texture pixels come from: procedural (the default) or a Minecraft resource
    /// pack directory, read once when the first GPU is selected ([`RenderInfo::textures`]).
    pub textures: TextureSource,
    /// How the translucent quads inside each chunk are sorted back to front (ADR 0018). Default
    /// [`TranslucentSort::Background`]: on the async compute pool, applied a frame or so later.
    /// Read every frame.
    pub translucent_sort: TranslucentSort,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            app_name: "boxel".to_string(),
            gpu: GpuPreferences::default(),
            gpu_config: None,
            smoke_level: SmokeLevel::Auto,
            tier_policy: TierPolicy::default(),
            clear_color: DEFAULT_SKY_COLOR,
            distance_fog: DistanceFog::default(),
            ao_strength: SceneLight::DEFAULT_AO_STRENGTH,
            block_light_gain: 1.0,
            light_gamma: SceneLight::DEFAULT_GAMMA,
            chunk_memory: ChunkMemorySettings::default(),
            render_when_hidden: false,
            textures: TextureSource::Procedural,
            translucent_sort: TranslucentSort::default(),
        }
    }
}

/// Presentation setting (vsync), read by the renderer every frame. A change reconfigures the
/// surface on the same device in the next `Render` (`RenderSystems::Setup`): no new device, no
/// renderer rebuild, no chunk re-upload. Inserted by [`RenderPlugin`] with `init_resource`
/// (insert your own to start with another mode).
///
/// The default is vsync off ([`PresentModePref::NoVsync`]: `Mailbox`, else `Immediate`, else
/// `Fifo` with a logged note), so the app's `boxel_app::FrameLimit` governs the frame rate.
/// The wanted mode is mapped to what the surface supports with
/// [`boxel_gfx::choose_present_mode`]; [`RenderInfo::present_mode`] shows the result. A
/// `--present`, `BOXEL_PRESENT` or config file preference (`GpuPreferences::present_mode`)
/// replaces this value once, when the first GPU is selected; runtime changes win after that.
///
/// Frames shown on screen with each choice:
/// - Vsync on (`Fifo`): at most the display refresh rate. Where presenting blocks (X11;
///   Windows is expected to behave the same, not measured) the update rate also drops to the
///   refresh rate when the frame limit is higher.
/// - Vsync off: up to the frame limit (`Mailbox` drops frames instead of tearing, `Immediate`
///   may tear). On Wayland the presentation gate (`WindowState::frame_ready`, opened by the
///   compositor's frame callback) still allows at most one present per refresh in both modes;
///   the extra updates run the simulation without presenting.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentSettings {
    /// Wanted present mode.
    pub mode: PresentModePref,
}

impl PresentSettings {
    /// Vsync on ([`PresentModePref::Vsync`]) or off ([`PresentModePref::NoVsync`]).
    pub fn vsync(on: bool) -> Self {
        Self {
            mode: PresentModePref::from_vsync(on),
        }
    }

    /// The wanted mode is vsync ([`PresentModePref::Vsync`]).
    pub fn is_vsync(self) -> bool {
        self.mode.is_vsync()
    }

    /// Switches vsync on or off (see [`PresentSettings::vsync`]).
    pub fn set_vsync(&mut self, on: bool) {
        self.mode = PresentModePref::from_vsync(on);
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
    /// Adapter name.
    pub adapter: String,
    /// Backend (`Vulkan`, `Gl`, ...).
    pub backend: String,
    /// Driver name and version (as reported by wgpu; may be empty).
    pub driver: String,
    /// Present mode the surface is configured with (`None` while no GPU is selected); see
    /// [`PresentSettings`].
    pub present_mode: Option<wgpu::PresentMode>,
    /// Frames presented since startup.
    pub frames_presented: u64,
    /// Frames rendered into the main color target (presented or not, see
    /// `RenderSettings::render_when_hidden`).
    pub frames_rendered: u64,
    /// Chunks resident on the GPU.
    pub chunks: u32,
    /// Draw calls recorded last frame (`RenderStats::draws`: Mid/Low one per visible bucket;
    /// High 2 indirect multi-draws, opaque + cutout, per page holding chunks).
    pub draws: u32,
    /// Chunks that passed the CPU cull last frame (`RenderStats::visible_chunks`; Mid/Low).
    pub visible_chunks: u32,
    /// Resident chunks cave culling hid last frame (`RenderStats::occluded_chunks`).
    pub occluded_chunks: u32,
    /// Time last frame took to update the cave culling bits (`RenderStats::occlusion_time`).
    pub occlusion_time: Duration,
    /// Translucent buckets re-sorted for the camera and uploaded since the renderer was built
    /// (`RenderStats::translucent_sorts`).
    pub translucent_sorts: u32,
    /// Device losses recovered from.
    pub recoveries: u32,
    /// `RenderStats::dropped_chunks` (no page or chunk slot free; warn-logged).
    pub dropped_chunks: u32,
    /// Page entries in use (`RenderStats::arena_used_quads`): the quads of every resident chunk
    /// plus their light palettes. 0 until a renderer exists.
    pub arena_used_quads: u32,
    /// Quads the pages hold (`RenderStats::arena_capacity_quads`). 0 until a renderer exists.
    pub arena_capacity_quads: u32,
    /// Chunk table slots (`RenderStats::chunk_slot_capacity`): the requested
    /// `ChunkMemorySettings::initial_slots`, cut to the device's limits. 0 until a renderer
    /// exists.
    pub chunk_slot_capacity: u32,
    /// Chunk memory: pages, use and bytes (`RenderStats::chunk_memory`, ADR 0035).
    pub chunk_memory: ChunkMemoryStats,
    /// Testing: what the last [`DebugAllocProbe`] saw, with how many probes ran so far.
    #[doc(hidden)]
    pub alloc_probe: Option<(u64, AllocProbeReport)>,
    /// How the block texture array was built (source, size, layers from a pack, load time,
    /// warnings). Default until the first GPU selection.
    pub textures: TextureReport,
    /// Far LOD nodes on the GPU (`RenderStats::lod_nodes`; 0 without `crate::lod::LodPlugin`).
    pub lod_nodes: u32,
    /// LOD draw calls last frame (`RenderStats::lod_draws`, not in `draws`).
    pub lod_draws: u32,
    /// Quads of those draws (`RenderStats::lod_quads`).
    pub lod_quads: u64,
    /// Quads copied into the LOD arena so far (`RenderStats::lod_upload_quads`).
    pub lod_upload_quads: u64,
    /// LOD arena quads in use (`RenderStats::lod_arena_used_quads`).
    pub lod_arena_used_quads: u32,
    /// LOD arena size in quads (`RenderStats::lod_arena_capacity_quads`).
    pub lod_arena_capacity_quads: u32,
    /// Quads the UI pass draws each frame (feature `ui`, ADR 0033; 0 without it, before the
    /// first non-empty paint list and while the list is empty).
    pub ui_quads: u32,
    /// UI paint list uploads since the device's UI pass was created (feature `ui`): they happen
    /// only when the list or the frame size changes, so an idle UI adds none.
    pub ui_uploads: u64,
    /// UI atlas page uploads since the device's UI pass was created (feature `ui`): the first
    /// one, then one per change of the atlas.
    pub ui_atlas_uploads: u64,
}

impl RenderInfo {
    /// Whether the configured present mode waits for the display (`Fifo`, `FifoRelaxed`);
    /// `None` while no GPU is selected.
    pub fn vsync(&self) -> Option<bool> {
        self.present_mode.map(vsync_of)
    }
}

/// Surface and error counters of the render plugin, for automation and diagnostics (additive to
/// the M0 skeleton API; `voxel_world --resize-test` reads it). Updated in `Cleanup`.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceInfo {
    /// Size of the configured surface in physical pixels (zero before the first selection).
    pub size: UVec2,
    /// Successful reconfigurations since startup (resize, `Outdated`, `Suboptimal`, present
    /// mode changes).
    pub reconfigures: u32,
    /// Surfaces recreated after `Lost` (same device).
    pub recreates: u32,
    /// Surface configuration or creation failures plus uncaptured GPU errors, summed over every
    /// device of the session.
    pub errors: u32,
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

/// Measurement (the M0 probe of ADR 0035, `stress --alloc-probe`): right after this frame's
/// present the renderer creates a buffer of `mib` MiB with the usages of a page of chunk quads,
/// where pages will be created, and later frames clear it as a new page will be; `mib = 0` drops
/// the probe's buffers. The outcome lands in `RenderInfo::alloc_probe`.
#[doc(hidden)]
#[derive(Message, Clone, Copy, Debug, Default)]
pub struct DebugAllocProbe {
    /// MiB to create, 0 to drop every probe buffer.
    pub mib: u32,
}

/// Adds the renderer. Requires `WindowPlugin` (for `DisplayHandle`/`PrimaryWindow`) and
/// `boxel_app` time. Optional integrations: `boxel_world::WorldPlugin` (mesh events),
/// `boxel_diagnostic` (`"gpu"`/`"render"` lines), `boxel_camera::Camera`. The `Render`
/// schedule is configured with `App::edit_schedule` + `SingleThreadedExecutor` (keeps systems
/// other plugins added earlier and keeps GPU/window calls on the main thread).
///
/// Without a window (headless apps, tests) the plugin stays idle: `Setup` waits for
/// `PrimaryWindow` and `DisplayHandle`. On the first frame with both it selects a GPU
/// (`boxel_gfx::select_gpu` with [`crate::RendererSmokeTest`]), builds the
/// [`VoxelRenderer`] BEFORE configuring the surface and writes [`GpuReady`] (or
/// [`GpuFailed`], after which it stays idle). A lost device is dropped (renderer, surface,
/// device) and the next frame re-selects with a new surface; a second loss of the same adapter
/// excludes its tier and adds an LKG strike (none when [`DebugDestroyDevice`] caused the loss).
/// Chunk meshes are then re-uploaded from `ChunkMap::ready_meshes()`, as they are after a frame
/// that lost its chunk uploads (rejected by the GPU, or a staging buffer that could not be
/// created); a few such frames close together count as a device loss.
#[derive(Clone, Debug, Default)]
pub struct RenderPlugin {
    /// Settings.
    pub settings: RenderSettings,
}

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.settings.clone())
            .init_resource::<PresentSettings>()
            .init_resource::<RenderInfo>()
            .init_resource::<SurfaceInfo>()
            .init_resource::<OverlayDraw>()
            .init_resource::<BlockOutline>()
            .init_resource::<GpuState>()
            .init_resource::<InstanceState>()
            .init_resource::<LkgRes>()
            .init_resource::<Losses>()
            .init_resource::<ExtractedFrame>()
            .add_message::<GpuReady>()
            .add_message::<GpuFailed>()
            .add_message::<RequestScreenshot>()
            .add_message::<ScreenshotSaved>()
            .add_message::<DebugDestroyDevice>()
            .add_message::<DebugAllocProbe>()
            .add_message::<ChunkMeshEvent>()
            .add_message::<ChunkMeshResync>()
            // `edit_schedule` keeps systems other plugins already added to `Render`.
            .edit_schedule(Render, |schedule| {
                schedule.set_executor(SingleThreadedExecutor::new());
            });
        {
            let mut order = app.world_mut().get_resource_or_init::<MainScheduleOrder>();
            let render = Render.intern();
            if !order.labels.contains(&render) {
                if order.labels.contains(&Last.intern()) {
                    order.insert_after(Last, Render);
                } else {
                    order.labels.push(render);
                }
            }
        }
        app.configure_sets(
            Render,
            (
                RenderSystems::Setup,
                RenderSystems::Extract,
                RenderSystems::Prepare,
                RenderSystems::Draw,
                RenderSystems::Cleanup,
            )
                .chain(),
        )
        .add_systems(
            Render,
            (
                setup.in_set(RenderSystems::Setup),
                (extract, extract_visibility)
                    .chain()
                    .in_set(RenderSystems::Extract),
                prepare.in_set(RenderSystems::Prepare),
                draw.in_set(RenderSystems::Draw),
                cleanup.in_set(RenderSystems::Cleanup),
            ),
        )
        .add_systems(Shutdown, shutdown)
        .add_systems(WorldTeardown, forget_world.in_set(TeardownSystems::Render))
        .world_resource::<BlockOutline>(|o| o.block.is_none());
        #[cfg(feature = "ui")]
        app.add_systems(Render, crate::ui::extract_ui.in_set(RenderSystems::Extract));
    }
}

// ---------------------------------------------------------------------------------------------
// Private state

/// Offscreen frames (`render_when_hidden`) allowed in flight before `Draw` waits for the GPU.
const MAX_OFFSCREEN_IN_FLIGHT: usize = 2;
/// Upper bound of that wait (a hung GPU must not freeze the loop forever).
const OFFSCREEN_WAIT: Duration = Duration::from_secs(2);
/// Upper bound of the GPU idle wait in `Shutdown`.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
/// Chunk upload failures are warn-logged at most this often.
const UPLOAD_WARN_INTERVAL: Duration = Duration::from_secs(5);
/// Frames close together that lose their chunk uploads and count as a device loss: the earlier
/// ones resync every chunk, the last one rebuilds the device (a second time also excludes its
/// tier, see [`RejectedFrames`]).
const MAX_REJECTED_FRAMES: u32 = 3;
/// Accepted frames in a row after which earlier rejected frames no longer count.
const REJECTED_FRAMES_FORGOTTEN_AFTER: u32 = 120;

/// Lifecycle of the GPU side.
#[derive(Default)]
enum GpuStage {
    /// No device: select one as soon as the window and display handle exist.
    #[default]
    WaitingForWindow,
    /// Device, surface and renderer alive.
    Ready(Box<ReadyGpu>),
    /// Selection failed (`GpuFailed` was written); no retries.
    Failed,
    /// `Shutdown` ran; everything was released.
    Stopped,
}

/// What the renderer drops and uploads again before its next frame, ordered: writers only raise
/// it, the stronger one wins (one flag for every reset, shared with the chunk memory of #57).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Resync {
    /// Nothing.
    #[default]
    None,
    /// Every chunk is dropped ([`VoxelRenderer::clear_chunks`](crate::VoxelRenderer::clear_chunks))
    /// and every ready mesh uploaded again: a new device, or a frame whose uploads were lost.
    Chunks,
    /// The world closed: everything of it goes
    /// ([`VoxelRenderer::clear_world`](crate::VoxelRenderer::clear_world)), and its last refusals
    /// never reach the next world.
    World,
}

impl Resync {
    /// Raises it to `to` if that is stronger.
    pub fn raise(&mut self, to: Resync) {
        *self = (*self).max(to);
    }
}

/// `WorldTeardown`, `TeardownSystems::Render`: the renderer forgets the closed world in its
/// next frame (every chunk, refusal, cull set and far LOD node of it, [`Resync::World`]), and the
/// block outline goes. Without a ready device nothing is needed: every new device starts with a
/// resync.
fn forget_world(mut gpu: ResMut<GpuState>, mut outline: ResMut<BlockOutline>) {
    if let GpuStage::Ready(ready) = &mut gpu.stage {
        ready.resync.raise(Resync::World);
    }
    outline.block = None;
}

/// Selected device with its surface and renderer. Field order is the drop order (renderer,
/// surface, context), see [`ReadyGpu::teardown`].
struct ReadyGpu {
    renderer: VoxelRenderer,
    /// Overlay rectangles drawn on top of the frame.
    overlay: OverlayPass,
    /// The game UI drawn over the overlay, created on the first non-empty paint list.
    #[cfg(feature = "ui")]
    ui: Option<crate::ui::UiPass>,
    /// `None` only between a failed `Lost` recreation and the next `Setup`.
    surface: Option<wgpu::Surface<'static>>,
    config: wgpu::SurfaceConfiguration,
    ctx: GpuContext,
    /// Window size the surface was last configured for (`config` may be clamped).
    configured_for: UVec2,
    /// Present preference the surface is configured with (follows [`PresentSettings`]).
    present: PresentModePref,
    /// What the next `Extract` resyncs (writers only raise it): [`Resync::Chunks`] for a new
    /// renderer or a frame whose chunk uploads were lost (see [`after_frame`]),
    /// [`Resync::World`] when the world closed (`forget_world`).
    resync: Resync,
    /// Recent frames with lost chunk uploads.
    rejected: RejectedFrames,
    /// LKG `confirm_good` was called (first presented frame of this selection).
    confirmed: bool,
    /// Frames rendered with this renderer (screenshots wait for the first one).
    frames: u64,
    /// Submissions of offscreen frames still possibly in flight.
    offscreen: VecDeque<wgpu::SubmissionIndex>,
    /// [`DebugDestroyDevice`] destroyed this device: the loss is ours, not the driver's.
    destroyed_on_purpose: bool,
    /// The fog passes passed the smoke test on this device (else the fog stays off on it).
    fog_ok: bool,
    /// The fog was refused on this device and that was logged.
    fog_warned: bool,
    /// The far LOD passed the smoke test on this device (else it stays off on it).
    lod_ok: bool,
    /// The LOD was refused on this device and that was logged.
    lod_warned: bool,
}

impl ReadyGpu {
    /// Drops renderer, surface and context, in that order.
    fn teardown(self: Box<Self>) {
        let ReadyGpu {
            renderer,
            overlay,
            #[cfg(feature = "ui")]
            ui,
            surface,
            config,
            ctx,
            ..
        } = *self;
        #[cfg(feature = "ui")]
        drop(ui);
        drop(overlay);
        drop(renderer);
        drop(surface);
        drop(config);
        drop(ctx);
    }

    /// Uncaptured errors of this device so far.
    fn gpu_errors(&self) -> u32 {
        self.ctx.health.error_count()
    }
}

/// Counters that survive re-selections (published as [`SurfaceInfo`]).
#[derive(Clone, Copy, Debug, Default)]
struct SurfaceCounters {
    reconfigures: u32,
    recreates: u32,
    /// Surface creation/configuration failures.
    failures: u32,
    /// Uncaptured errors of devices that were already dropped.
    retired_gpu_errors: u32,
}

/// See [`GpuStage`].
#[derive(Resource, Default)]
pub(crate) struct GpuState {
    stage: GpuStage,
    counters: SurfaceCounters,
    /// Block texture array, built once from `RenderSettings::textures` (re-selections reuse it).
    block_textures: Option<Arc<BlockTextureData>>,
}

impl GpuState {
    fn ready(&self) -> Option<&ReadyGpu> {
        match &self.stage {
            GpuStage::Ready(ready) => Some(ready),
            _ => None,
        }
    }

    /// The UI pass of the selected device: `None` without a usable device, `Some(None)` before
    /// the first non-empty paint list.
    #[cfg(feature = "ui")]
    pub(crate) fn ui_pass(&self) -> Option<Option<&crate::ui::UiPass>> {
        self.ready()
            .filter(|r| !r.ctx.health.is_lost())
            .map(|r| r.ui.as_ref())
    }

    /// Leaves `Ready` (dropping renderer, surface, context) and enters `next`. Returns whether a
    /// selection was dropped and whether it had been confirmed.
    fn drop_ready(&mut self, next: GpuStage) -> Option<bool> {
        match std::mem::replace(&mut self.stage, next) {
            GpuStage::Ready(ready) => {
                self.counters.retired_gpu_errors = self
                    .counters
                    .retired_gpu_errors
                    .saturating_add(ready.gpu_errors());
                let confirmed = ready.confirmed;
                ready.teardown();
                Some(confirmed)
            }
            _ => None,
        }
    }
}

/// Resolved preferences and the process instance (created once; a second, GL-only instance only
/// when `gl_deferred` asks for it).
#[derive(Resource, Default)]
struct InstanceState {
    prefs: Option<GpuPreferences>,
    created: Option<CreatedInstance>,
    /// The `gl_deferred` second stage was tried.
    gl_stage_tried: bool,
}

/// Last-known-good store (`None` when disabled).
#[derive(Resource, Default)]
struct LkgRes {
    opened: bool,
    store: Option<LkgStore>,
}

/// Device losses of this session.
#[derive(Resource, Default)]
struct Losses {
    exclusions: Vec<Exclusion>,
    per_key: HashMap<String, u32>,
}

impl Losses {
    /// Counts a loss of `adapter_key` at `tier`. The first loss of an adapter retries the same
    /// pair (transient TDR); from the second one on, the tier is excluded (`tier.lower()`, or the
    /// whole adapter for Low) and `true` is returned (the caller adds an LKG strike, see
    /// [`adds_lkg_strike`]).
    fn record(&mut self, adapter_key: &str, tier: Tier) -> bool {
        let count = self.per_key.entry(adapter_key.to_string()).or_default();
        *count += 1;
        if *count < 2 {
            return false;
        }
        self.exclude(adapter_key, tier.lower());
        true
    }

    /// Adds (or tightens) an exclusion for `adapter_key`.
    fn exclude(&mut self, adapter_key: &str, max_tier: Option<Tier>) {
        match self
            .exclusions
            .iter_mut()
            .find(|e| e.adapter_key == adapter_key)
        {
            Some(existing) => {
                existing.max_tier = match (existing.max_tier, max_tier) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    _ => None,
                };
            }
            None => self.exclusions.push(Exclusion {
                adapter_key: adapter_key.to_string(),
                max_tier,
            }),
        }
    }
}

/// The extract boundary: inputs copied from the main world by `Extract`, the view computed by
/// `Prepare`, and the frame results `Cleanup` writes back.
#[derive(Resource, Default)]
pub(crate) struct ExtractedFrame {
    camera: Option<(Camera, Transform)>,
    window: WindowState,
    /// Clear and fog color of this frame.
    sky_color: [f32; 4],
    /// Scene light of this frame.
    light: SceneLight,
    /// Seconds of the fluid animation ([`ViewParams::fluid_time`]).
    fluid_time: f32,
    /// Game ticks of the texture animations ([`ViewParams::texture_tick`]).
    texture_tick: u64,
    /// The distance fog of this frame: the last valid `RenderSettings::distance_fog`.
    pub(crate) distance_fog: DistanceFog,
    render_when_hidden: bool,
    translucent_sort: TranslucentSort,
    /// Full resync: `meshes` holds every ready mesh, and `renderer.clear_chunks` (or, for a new
    /// world, `renderer.clear_world`) runs first.
    pub(crate) resync: Resync,
    meshes: Vec<ChunkOp>,
    /// Pending screenshot paths (kept until a frame exists to read).
    screenshots: Vec<PathBuf>,
    /// Copy of `OverlayDraw::rects`.
    overlay: Vec<OverlayRect>,
    /// The UI's paint list and atlas page when the UI pass lacks them (`crate::ui`).
    #[cfg(feature = "ui")]
    pub(crate) ui: crate::ui::UiExtract,
    /// The block outline of `BlockOutline`, as the world has the block this frame.
    outline: Option<Outline>,
    /// The chunks the camera may see this frame (cave culling), `None` when off.
    visibility: Option<Arc<VisibleSet>>,
    /// This frame's fog (`crate::fog::FogPlugin`'s extract), `None` without the plugin.
    pub(crate) fog: Option<FogInput>,
    /// This frame's far LOD (`crate::lod::LodPlugin`'s extract), `None` without the plugin.
    pub(crate) lod: Option<LodInput>,
    /// The renderer as the LOD's extract sees it, `None` without a ready renderer (none yet, or
    /// its device is lost).
    pub(crate) lod_renderer: Option<LodRenderer>,
    destroy_device: bool,
    /// [`DebugAllocProbe`] requests not run yet, in MiB.
    alloc_probe: Vec<u32>,
    /// What they saw (`Draw` output).
    alloc_probe_done: Vec<AllocProbeReport>,
    /// `Prepare` output.
    view: Option<ViewParams>,
    /// `Draw` results.
    rendered: bool,
    presented: bool,
    /// The presentation gate was used (presented or skipped): `Cleanup` closes it.
    gate_used: bool,
}

/// What to do after a frame (see [`RejectedFrames::record`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameVerdict {
    /// The chunk uploads reached the GPU.
    Accepted,
    /// They were lost: upload every chunk again next frame.
    Resync,
    /// Lost again, [`MAX_REJECTED_FRAMES`] recent times: treat the device as lost.
    Lost,
}

/// Counts recent frames whose chunk uploads were lost. A device that keeps losing them would
/// resync (and warn) every frame, and a full resync allocates more than the frame it replaces,
/// so memory pressure tends to repeat: past [`MAX_REJECTED_FRAMES`] the device is rebuilt
/// instead, like after a device loss (a second time excludes its tier).
#[derive(Debug, Default)]
struct RejectedFrames {
    /// Rejected frames that still count.
    recent: u32,
    /// Accepted frames since the last rejected one.
    accepted: u32,
}

impl RejectedFrames {
    /// Records one frame (`rejected`: its chunk uploads were lost).
    fn record(&mut self, rejected: bool) -> FrameVerdict {
        if !rejected {
            self.accepted = self.accepted.saturating_add(1);
            if self.accepted >= REJECTED_FRAMES_FORGOTTEN_AFTER {
                self.recent = 0;
            }
            return FrameVerdict::Accepted;
        }
        self.accepted = 0;
        self.recent += 1;
        if self.recent >= MAX_REJECTED_FRAMES {
            FrameVerdict::Lost
        } else {
            FrameVerdict::Resync
        }
    }
}

/// Rate limiter of the chunk upload warning.
#[derive(Default)]
struct UploadWarnings {
    last: Option<Instant>,
    failures: u32,
    last_error: String,
}

impl UploadWarnings {
    fn record(&mut self, error: &crate::RendererError) {
        self.failures += 1;
        self.last_error = error.to_string();
    }

    fn flush(&mut self, dropped_total: u32) {
        if self.failures == 0 {
            return;
        }
        let now = Instant::now();
        if self
            .last
            .is_some_and(|t| now.duration_since(t) < UPLOAD_WARN_INTERVAL)
        {
            return;
        }
        tracing::warn!(
            "{} chunk upload(s) failed (last: {}); {dropped_total} chunks dropped so far: raise \
             RenderSettings::chunk_memory (initial_quads, initial_slots) or lower the view \
             distance",
            self.failures,
            self.last_error
        );
        self.last = Some(now);
        self.failures = 0;
    }
}

// ---------------------------------------------------------------------------------------------
// Setup

/// Main-world access of `Setup` (bootstrap, device-loss recovery, surface resize).
#[derive(SystemParam)]
struct SetupParams<'w> {
    settings: Res<'w, RenderSettings>,
    present: ResMut<'w, PresentSettings>,
    gpu: ResMut<'w, GpuState>,
    instance: ResMut<'w, InstanceState>,
    lkg: ResMut<'w, LkgRes>,
    losses: ResMut<'w, Losses>,
    display: Option<Res<'w, DisplayHandle>>,
    window: Option<NonSend<'w, PrimaryWindow>>,
    window_state: Option<Res<'w, WindowState>>,
    content: Option<Res<'w, WorldContent>>,
    gpu_ready: MessageWriter<'w, GpuReady>,
    gpu_failed: MessageWriter<'w, GpuFailed>,
    info: ResMut<'w, RenderInfo>,
    board: Option<ResMut<'w, DiagnosticsBoard>>,
    time: Option<ResMut<'w, Time>>,
    /// Present with `crate::fog::FogPlugin`: the smoke test also checks the fog passes.
    fog: Option<Res<'w, FogSettings>>,
    /// Present with `crate::lod::LodPlugin`: the smoke test also checks the LOD pass.
    lod: Option<Res<'w, LodSettings>>,
}

/// `RenderSystems::Setup`. GPU selection and renderer creation are declared as a stall
/// (`Time::declare_stall`): the simulation does not try to catch up on them.
fn setup(mut p: SetupParams) {
    let started = Instant::now();
    let lost = p.gpu.ready().is_some_and(|r| r.ctx.health.is_lost());
    if lost {
        recover_from_loss(&mut p);
    }
    let bring_up = lost || matches!(p.gpu.stage, GpuStage::WaitingForWindow);
    if matches!(p.gpu.stage, GpuStage::WaitingForWindow) {
        bootstrap(&mut p);
    }
    if bring_up
        && !matches!(p.gpu.stage, GpuStage::WaitingForWindow)
        && let Some(time) = p.time.as_deref_mut()
    {
        time.declare_stall(started.elapsed());
    }
    let size = p.window_state.as_deref().map(|s| s.physical_size);
    let window = p.window.as_deref().map(|w| Arc::clone(&w.window));
    let wanted_present = p.present.mode;
    let GpuState {
        stage, counters, ..
    } = &mut *p.gpu;
    if let GpuStage::Ready(ready) = stage {
        if ready.present != wanted_present {
            change_present_mode(ready, counters, wanted_present);
        }
        if ready.surface.is_none()
            && let Some(window) = window
        {
            recreate_surface(
                ready,
                counters,
                &window,
                size.unwrap_or(ready.configured_for),
            );
        }
        if let Some(size) = size
            && size.x > 0
            && size.y > 0
            && size != ready.configured_for
        {
            ready.renderer.resize(&ready.ctx.device, size);
            reconfigure(ready, counters, size);
        }
    }
    if p.gpu.ready().is_some_and(|r| r.surface.is_none()) {
        // Could not recreate the surface: start over with a full selection (no exclusion, the
        // device is healthy).
        tracing::warn!("no usable surface: re-selecting the GPU");
        if let Some(false) = p.gpu.drop_ready(GpuStage::WaitingForWindow)
            && let Some(store) = p.lkg.store.as_mut()
        {
            store.cancel_pending();
        }
        p.info.ready = false;
    }
}

/// Device lost: log, count, drop renderer/surface/context, exclude on a repeated loss.
fn recover_from_loss(p: &mut SetupParams) {
    let Some(ready) = p.gpu.ready() else {
        return;
    };
    let reason = ready
        .ctx
        .health
        .lost_reason()
        .unwrap_or_else(|| "no reason given".to_string());
    let summary = ready.ctx.caps.summary();
    let tier = ready.ctx.tier;
    let key = summary.key();
    let on_purpose = ready.destroyed_on_purpose;
    let confirmed = p.gpu.drop_ready(GpuStage::WaitingForWindow);
    if confirmed == Some(false)
        && let Some(store) = p.lkg.store.as_mut()
    {
        store.cancel_pending();
    }
    let repeated = p.losses.record(&key, tier);
    let n = p.losses.per_key.get(&key).copied().unwrap_or(1);
    if repeated {
        tracing::error!(
            "GPU device lost ({reason}) on {} {tier} (loss {n} of this adapter): excluding \
             {tier} and re-selecting with a new surface",
            summary.name
        );
        if !adds_lkg_strike(repeated, on_purpose) {
            tracing::info!("the loss came from DebugDestroyDevice: no LKG strike");
        } else if let Some(store) = p.lkg.store.as_mut() {
            store.add_strike(&summary, tier, format!("device lost: {reason}"));
        }
    } else {
        tracing::warn!(
            "GPU device lost ({reason}) on {} {tier}: re-selecting with a new surface",
            summary.name
        );
    }
    p.info.recoveries += 1;
    p.info.ready = false;
}

/// A repeated loss adds an LKG strike, except when [`DebugDestroyDevice`] caused it: the LKG
/// only records driver failures, and a test run must not change the next normal selection.
fn adds_lkg_strike(repeated: bool, destroyed_on_purpose: bool) -> bool {
    repeated && !destroyed_on_purpose
}

/// First selection or re-selection after a loss (`WaitingForWindow` with window + display).
fn bootstrap(p: &mut SetupParams) {
    let (Some(window), Some(display)) = (p.window.as_deref(), p.display.as_deref()) else {
        return;
    };
    let window = Arc::clone(&window.window);
    let display = display.0.clone();
    let settings = p.settings.clone();

    // 1. Preferences (once; warnings logged once).
    let prefs = match &p.instance.prefs {
        Some(prefs) => prefs.clone(),
        None => {
            let (prefs, warnings) =
                GpuPreferences::resolve(settings.gpu.clone(), settings.gpu_config.as_deref());
            for warning in warnings {
                tracing::warn!("GPU preferences: {warning}");
            }
            // A user override (CLI, env, config file) replaces the app's present setting once.
            if let Some(mode) = prefs.present_mode {
                p.present.set_if_neq(PresentSettings { mode });
            }
            p.instance.prefs = Some(prefs.clone());
            prefs
        }
    };
    // 2. Instance (once per process).
    if p.instance.created.is_none() {
        match create_instance(Some(display.clone()), &prefs) {
            Ok(created) => p.instance.created = Some(created),
            Err(e) => {
                fail(p, e.to_string());
                return;
            }
        }
    }
    // 3. Last-known-good (once).
    if !p.lkg.opened {
        p.lkg.opened = true;
        if prefs.lkg_enabled() {
            let path = LkgStore::default_path(&settings.app_name);
            let (store, error) = LkgStore::open(path.clone(), build_fingerprint());
            if let Some(error) = error {
                tracing::warn!("{error}");
            }
            if let Some(path) = path {
                install_lkg_panic_hook(path);
            }
            p.lkg.store = Some(store);
        }
    }

    // 4-5. New surface + selection (+ the GL second stage of `panic = "abort"` builds).
    let smoke = RendererSmokeTest {
        fog: p.fog.is_some(),
        lod: p.lod.is_some(),
        ..RendererSmokeTest::default()
    };
    let mut selected = {
        let Some(created) = p.instance.created.as_ref() else {
            return;
        };
        select(
            created,
            &window,
            &prefs,
            &settings,
            p.lkg.store.as_mut(),
            &p.losses.exclusions,
            &smoke,
        )
    };
    if let Err(Selection::NoAdapter(_)) = &selected
        && p.instance
            .created
            .as_ref()
            .is_some_and(|created| created.gl_deferred)
        && !p.instance.gl_stage_tried
    {
        p.instance.gl_stage_tried = true;
        tracing::info!("no adapter without GL; trying the deferred GL instance");
        match create_gl_instance(Some(display), &prefs) {
            Ok(gl) => {
                selected = select(
                    &gl,
                    &window,
                    &prefs,
                    &settings,
                    p.lkg.store.as_mut(),
                    &p.losses.exclusions,
                    &smoke,
                );
                p.instance.created = Some(gl);
            }
            Err(e) => selected = Err(Selection::Failed(e.to_string())),
        }
    }
    let (ctx, surface) = match selected {
        Ok(pair) => pair,
        Err(Selection::NoAdapter(error) | Selection::Failed(error)) => {
            fail(p, error);
            return;
        }
    };

    // 6. Renderer BEFORE the surface is configured (it only needs `ctx.output_format`).
    let size = p
        .window_state
        .as_deref()
        .map(|s| s.physical_size)
        .filter(|s| s.x > 0 && s.y > 0)
        .unwrap_or_else(|| {
            let s = window.inner_size();
            UVec2::new(s.width.max(1), s.height.max(1))
        });
    let textures = block_texture_data(p);
    let config = RendererConfig {
        chunk_memory: valid_chunk_memory(settings.chunk_memory),
        translucent_sort: settings.translucent_sort,
    };
    let present = p.present.mode;
    let built = VoxelRenderer::new(
        &ctx.device,
        &ctx.queue,
        ctx.tier,
        &config,
        &textures,
        ctx.output_format,
        size,
    )
    .map_err(|e| format!("renderer: {e}"))
    .and_then(|renderer| {
        // 7. Configure the NEW surface with this device.
        configure_surface(&surface, &ctx, size.x, size.y, present)
            .map(|surface_config| (renderer, surface_config))
            .map_err(|e| format!("surface configuration: {e}"))
    });
    let (mut renderer, surface_config) = match built {
        Ok(parts) => parts,
        Err(error) => {
            let key = ctx.caps.summary().key();
            let max_tier = ctx.tier.lower();
            tracing::error!(
                "{} {}: {error}; excluding the pair (max tier {}) and re-selecting next frame",
                ctx.caps.info.name,
                ctx.tier,
                max_tier.map_or("none", Tier::as_str)
            );
            p.losses.exclude(&key, max_tier);
            if let Some(store) = p.lkg.store.as_mut() {
                store.cancel_pending();
            }
            p.gpu.counters.failures += 1;
            p.gpu.counters.retired_gpu_errors = p
                .gpu
                .counters
                .retired_gpu_errors
                .saturating_add(ctx.health.error_count());
            // Drop order: surface, then the context (no renderer exists).
            drop(surface);
            drop(ctx);
            return;
        }
    };
    // The model box table of the world's meshes (ADR 0020). Without a world: the air-only
    // registry's, empty (the engine has no content, #48).
    match p.content.as_deref() {
        Some(content) => renderer.set_model_boxes(&ctx.queue, content.materials.model_boxes()),
        None => {
            let materials = MaterialTable::from_registry(&BlockRegistryBuilder::new().build());
            renderer.set_model_boxes(&ctx.queue, materials.model_boxes());
        }
    }

    // 8. Success.
    let summary = ctx.summary_line();
    tracing::info!("boxel: {summary}");
    log_present_mode(&surface, &ctx, present, surface_config.present_mode);
    p.gpu_ready.write(GpuReady {
        summary: summary.clone(),
        tier: ctx.tier,
    });
    p.info.ready = true;
    p.info.tier = Some(ctx.tier);
    p.info.class = Some(ctx.class);
    p.info.summary.clone_from(&summary);
    p.info.adapter.clone_from(&ctx.caps.info.name);
    p.info.backend = format!("{:?}", ctx.caps.info.backend);
    p.info.driver = [&ctx.caps.info.driver, &ctx.caps.info.driver_info]
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(board) = p.board.as_deref_mut() {
        board.set("gpu", summary);
    }
    let overlay = OverlayPass::new(&ctx.device);
    p.gpu.stage = GpuStage::Ready(Box::new(ReadyGpu {
        renderer,
        overlay,
        #[cfg(feature = "ui")]
        ui: None,
        surface: Some(surface),
        config: surface_config,
        ctx,
        configured_for: size,
        present,
        resync: Resync::Chunks,
        rejected: RejectedFrames::default(),
        confirmed: false,
        frames: 0,
        offscreen: VecDeque::new(),
        destroyed_on_purpose: false,
        fog_ok: smoke.fog_ok.load(std::sync::atomic::Ordering::Relaxed),
        fog_warned: false,
        lod_ok: smoke.lod_ok.load(std::sync::atomic::Ordering::Relaxed),
        lod_warned: false,
    }));
}

/// The block texture array, built on the first bootstrap from `RenderSettings::textures` and the
/// world's registry (re-selections after a device loss reuse it). A pack that fails to open falls
/// back to the procedural textures with an error log; the outcome lands in `RenderInfo::textures`.
fn block_texture_data(p: &mut SetupParams) -> Arc<BlockTextureData> {
    if let Some(data) = &p.gpu.block_textures {
        return Arc::clone(data);
    }
    let default_registry;
    let registry: &BlockRegistry = match p.content.as_deref() {
        Some(content) => &content.registry,
        // Without a world: the air-only registry, one magenta layer (the engine has no
        // content, #48).
        None => {
            default_registry = BlockRegistryBuilder::new().build();
            &default_registry
        }
    };
    let started = Instant::now();
    let (data, report) = load_block_textures(registry, &p.settings.textures).unwrap_or_else(|e| {
        tracing::error!("block textures: {e}; using the procedural textures");
        let data = generate_block_textures(registry.textures());
        let report = TextureReport {
            source: TextureSource::Procedural.label(),
            size: data.size,
            layers: data.layers,
            load_time: started.elapsed(),
            warnings: vec![e],
            ..TextureReport::default()
        };
        (data, report)
    });
    for warning in &report.warnings {
        tracing::warn!("block textures: {warning}");
    }
    tracing::info!(
        "block textures: {} {}px, {} of {} layers from the pack, {} pack warnings, {:.1} ms",
        report.source,
        report.size,
        report.from_pack,
        report.layers,
        report.pack_warnings,
        report.load_time.as_secs_f64() * 1000.0
    );
    p.info.textures = report;
    let data = Arc::new(data);
    p.gpu.block_textures = Some(Arc::clone(&data));
    data
}

/// Why a selection attempt gave no device.
enum Selection {
    /// `select_gpu` found nothing usable (the GL second stage may still help).
    NoAdapter(String),
    /// Anything else (surface creation).
    Failed(String),
}

/// New surface from the window, then `select_gpu` against it.
fn select(
    created: &CreatedInstance,
    window: &Arc<boxel_window::winit::window::Window>,
    prefs: &GpuPreferences,
    settings: &RenderSettings,
    lkg: Option<&mut LkgStore>,
    exclusions: &[Exclusion],
    smoke: &RendererSmokeTest,
) -> Result<(GpuContext, wgpu::Surface<'static>), Selection> {
    let surface = created
        .instance
        .create_surface(Arc::clone(window))
        .map_err(|e| Selection::Failed(format!("cannot create a window surface: {e}")))?;
    let ctx = select_gpu(SelectRequest {
        instance: created,
        surface: Some(&surface),
        prefs,
        policy: settings.tier_policy,
        lkg,
        exclusions,
        smoke,
        smoke_level: settings.smoke_level,
        create_devices: true,
    })
    .map_err(|e| match e {
        SelectError::NoUsableAdapter(_) => Selection::NoAdapter(e.to_string()),
        SelectError::NoInstance(_) => Selection::Failed(e.to_string()),
    })?;
    Ok((ctx, surface))
}

/// Selection failure: `GpuFailed`, error log, state `Failed` (no retries).
fn fail(p: &mut SetupParams, error: String) {
    tracing::error!("GPU selection failed: {error}");
    p.gpu_failed.write(GpuFailed { error });
    p.gpu.stage = GpuStage::Failed;
    p.info.ready = false;
}

/// Reconfigures the current surface (same device) for `size`.
fn reconfigure(ready: &mut ReadyGpu, counters: &mut SurfaceCounters, size: UVec2) {
    let Some(surface) = ready.surface.as_ref() else {
        return;
    };
    match configure_surface(surface, &ready.ctx, size.x, size.y, ready.present) {
        Ok(config) => {
            ready.config = config;
            counters.reconfigures += 1;
        }
        Err(e) => {
            tracing::error!("surface reconfiguration failed: {e}");
            counters.failures += 1;
        }
    }
    // Do not retry every frame on failure: the next real size change tries again.
    ready.configured_for = size;
}

/// [`PresentSettings`] changed: reconfigure the current surface (same device, same size) with
/// the new preference.
fn change_present_mode(
    ready: &mut ReadyGpu,
    counters: &mut SurfaceCounters,
    wanted: PresentModePref,
) {
    ready.present = wanted;
    let size = ready.configured_for;
    reconfigure(ready, counters, size);
    if let Some(surface) = ready.surface.as_ref() {
        log_present_mode(surface, &ready.ctx, wanted, ready.config.present_mode);
    }
}

/// Logs the present mode a configuration picked, with a note when vsync off was wanted but the
/// surface only offers `Fifo`.
fn log_present_mode(
    surface: &wgpu::Surface<'_>,
    ctx: &GpuContext,
    wanted: PresentModePref,
    chosen: wgpu::PresentMode,
) {
    tracing::info!(
        "present mode {chosen:?} (wanted {}, vsync {})",
        wanted.as_str(),
        if vsync_of(chosen) { "on" } else { "off" }
    );
    if !wanted.is_vsync() && chosen == wgpu::PresentMode::Fifo {
        let offered = surface.get_capabilities(&ctx.adapter).present_modes;
        tracing::info!(
            "vsync off is not available on this surface (it offers {offered:?}): presenting \
             with Fifo, so shown frames follow the display refresh rate"
        );
    }
}

/// `Fifo` and `FifoRelaxed` wait for the display (vsync).
pub(crate) fn vsync_of(mode: wgpu::PresentMode) -> bool {
    matches!(
        mode,
        wgpu::PresentMode::Fifo | wgpu::PresentMode::FifoRelaxed
    )
}

/// `Lost` surface on a healthy device: drop the old surface FIRST, then create a new one from the
/// same window and instance and configure it with the same device.
fn recreate_surface(
    ready: &mut ReadyGpu,
    counters: &mut SurfaceCounters,
    window: &Arc<boxel_window::winit::window::Window>,
    size: UVec2,
) {
    drop(ready.surface.take());
    let size = UVec2::new(size.x.max(1), size.y.max(1));
    let surface = match ready.ctx.instance.create_surface(Arc::clone(window)) {
        Ok(surface) => surface,
        Err(e) => {
            tracing::error!("cannot recreate the window surface: {e}");
            counters.failures += 1;
            return;
        }
    };
    match configure_surface(&surface, &ready.ctx, size.x, size.y, ready.present) {
        Ok(config) => {
            ready.config = config;
            ready.configured_for = size;
            ready.surface = Some(surface);
            counters.recreates += 1;
            tracing::info!("surface recreated ({}x{})", size.x, size.y);
        }
        Err(e) => {
            tracing::error!("recreated surface cannot be configured: {e}");
            counters.failures += 1;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Extract

/// The sky color and scene light of a frame: from the world clock and its curve when the app has
/// them, else a noon sky of `settings.clear_color`.
fn scene_light(
    settings: &RenderSettings,
    clock: Option<(&TimeOfDay, &DaylightCurve)>,
) -> ([f32; 4], SceneLight) {
    let daylight = clock.map(|(time, curve)| curve.at(time));
    let light = SceneLight {
        sky: daylight.map_or([1.0; 3], |d| d.sky_light),
        ambient: daylight.map_or(SceneLight::DEFAULT_AMBIENT, |d| d.ambient),
        ao_strength: settings.ao_strength,
        block_gain: settings.block_light_gain,
        curve: light_curve(settings.light_gamma),
    };
    (
        daylight.map_or(settings.clear_color, |d| d.sky_color),
        light,
    )
}

/// The distance fog of the next frames: `fog` if valid, else (logged) the `last` valid one.
fn valid_distance_fog(fog: DistanceFog, last: DistanceFog) -> DistanceFog {
    match fog.validate() {
        Ok(()) => fog,
        Err(e) => {
            tracing::warn!("RenderSettings::distance_fog: {e}; the last valid one stays");
            last
        }
    }
}

/// `RenderSystems::Extract`: the only reads of the main world for this frame's rendering.
pub(crate) fn extract(
    settings: Res<RenderSettings>,
    gpu: Res<GpuState>,
    mut frame: ResMut<ExtractedFrame>,
    window: Option<Res<WindowState>>,
    cameras: Query<(&Camera, &Transform)>,
    clock: Option<(Res<TimeOfDay>, Res<DaylightCurve>)>,
    chunks: Option<Res<ChunkMap>>,
    mut mesh_events: MessageReader<ChunkMeshEvent>,
    mut resyncs: MessageWriter<ChunkMeshResync>,
    mut screenshots: MessageReader<RequestScreenshot>,
    (mut destroy, mut probes): (
        MessageReader<DebugDestroyDevice>,
        MessageReader<DebugAllocProbe>,
    ),
    overlay: Res<OverlayDraw>,
    time: Option<Res<Time>>,
    (outline, content, mut outline_cache): (
        Option<Res<BlockOutline>>,
        Option<Res<WorldContent>>,
        Local<OutlineCache>,
    ),
) {
    let frame = &mut *frame;
    frame.overlay.clear();
    frame.overlay.extend_from_slice(&overlay.rects);
    frame.outline = match (outline.as_deref(), chunks.as_deref(), content.as_deref()) {
        (Some(outline), Some(map), Some(content)) => {
            crate::outline::resolve(outline, map, &content.registry, &mut outline_cache)
        }
        _ => None,
    };
    frame.camera = cameras
        .iter()
        .find(|(camera, _)| camera.active)
        .map(|(camera, transform)| (*camera, *transform));
    frame.window = window.as_deref().copied().unwrap_or_default();
    (frame.sky_color, frame.light) = scene_light(
        &settings,
        clock.as_ref().map(|(time, curve)| (&**time, &**curve)),
    );
    if settings.is_changed() {
        frame.distance_fog = valid_distance_fog(settings.distance_fog, frame.distance_fog);
    }
    frame.fluid_time = time
        .as_ref()
        .map_or(0.0, |t| (t.elapsed_secs_f64() % FLUID_TIME_PERIOD) as f32);
    frame.texture_tick = time.map_or(0, |t| texture_tick(t.elapsed_secs_f64()));
    frame.render_when_hidden = settings.render_when_hidden;
    frame.translucent_sort = settings.translucent_sort;
    frame
        .screenshots
        .extend(screenshots.read().map(|r| r.path.clone()));
    frame.destroy_device = destroy.read().count() > 0;
    // Kept until a frame is drawn (the probe runs after a submit).
    frame.alloc_probe.extend(probes.read().map(|p| p.mib));
    frame.view = None;
    frame.rendered = false;
    frame.presented = false;
    frame.gate_used = false;

    let ready = gpu.ready().filter(|r| !r.ctx.health.is_lost());
    frame.resync = ready.map_or(Resync::None, |r| r.resync);
    frame.lod_renderer = ready.map(|r| LodRenderer {
        drawable: r.lod_ok && crate::lod::supported(&r.ctx.caps.info) && !r.renderer.lod_failed(),
        arena_quads: r.renderer.lod_arena_quads(),
    });
    frame.meshes.clear();
    if frame.resync != Resync::None {
        // Everything currently published replaces whatever the events would say. The chunks
        // being remeshed are missing: the world sends their next mesh even if identical.
        mesh_events.clear();
        resyncs.write(ChunkMeshResync);
        if let Some(map) = chunks.as_deref() {
            frame
                .meshes
                .extend(map.ready_meshes().map(|(id, key, mesh)| ChunkOp::Upsert {
                    id: id.to_bits(),
                    origin: key.pos.origin().as_i64vec3(),
                    mesh: Arc::clone(mesh),
                }));
        }
    } else if ready.is_some() {
        frame.meshes.extend(mesh_events.read().map(mesh_op));
    } else {
        // No renderer: the next selection resyncs from `ChunkMap`.
        mesh_events.clear();
    }
}

/// `RenderSystems::Extract`, after [`extract`] (and the far LOD's extract): the cave culling
/// search for this frame's camera (ADR 0022), when the world has it on
/// ([`WorldSettings::cave_culling`]). It runs here, with the exact camera and aspect `Prepare`
/// renders with, so a chunk that just became visible is never drawn late. The searched box is
/// the mesh region of the camera's own [`ChunkViewer`], or of the largest viewer; without viewers
/// nothing is culled. The set is [`frame_visibility_with`]'s, with the far plane at the LOD's
/// horizon while the far LOD draws this frame (ADR 0031: the renderer then draws every chunk out
/// to it).
///
/// First, whatever the setting, the world learns which chunks the renderer refused or shows
/// again since the last frame ([`VoxelRenderer::take_refused`]): the search sees through them
/// ([`forward_refused`]); and the far LOD draws over those that left nothing drawn
/// ([`VoxelRenderer::take_undrawn`]).
///
/// [`VoxelRenderer::take_undrawn`]: crate::VoxelRenderer::take_undrawn
pub(crate) fn extract_visibility(
    mut gpu: ResMut<GpuState>,
    mut frame: ResMut<ExtractedFrame>,
    settings: Option<Res<WorldSettings>>,
    content: Option<Res<WorldContent>>,
    chunks: Option<Res<ChunkMap>>,
    visibility: Option<ResMut<ChunkVisibility>>,
    mut lod_feedback: Option<ResMut<LodFeedback>>,
    cameras: Query<(&Camera, Option<&ChunkViewer>)>,
    viewers: Query<&ChunkViewer>,
) {
    frame.visibility = None;
    let (Some(settings), Some(content), Some(map), Some(mut visibility)) =
        (settings, content, chunks, visibility)
    else {
        return;
    };
    if let GpuStage::Ready(ready) = &mut gpu.stage {
        if frame.resync == Resync::World {
            // The closed world's last refusals: the next world never hears of them.
            ready.renderer.take_refused(|_, _| {});
            ready.renderer.take_undrawn(|_, _| {});
        } else {
            ready.renderer.take_refused(|pos, refused| {
                forward_refused(pos, refused, &mut visibility, &map);
            });
            // Taken whatever the LOD: the renderer keeps them until then.
            ready.renderer.take_undrawn(|pos, undrawn| {
                if let Some(lod) = lod_feedback.as_deref_mut() {
                    lod.chunks.push((pos, undrawn));
                }
            });
        }
        if frame.resync != Resync::None {
            // `prepare` drops every chunk first (a new renderer may not know the old refusals).
            visibility.clear_unshown(|pos| map.visibility_links_at(pos));
        }
    }
    if !settings.cave_culling {
        return;
    }
    let (Some(ready), Some((camera, transform))) = (gpu.ready(), frame.camera) else {
        return;
    };
    let own = cameras
        .iter()
        .find(|(camera, _)| camera.active)
        .and_then(|(_, viewer)| viewer.copied());
    let Some(viewer) = own.or_else(|| {
        viewers.iter().copied().reduce(|a, b| ChunkViewer {
            radius: a.radius.max(b.radius),
            vertical_radius: a.vertical_radius.max(b.vertical_radius),
        })
    }) else {
        return;
    };
    // The aspect `prepare` uses: the same view, so the same frustum.
    let aspect = ready.config.width.max(1) as f32 / ready.config.height.max(1) as f32;
    frame.visibility = frame_visibility_with(
        frame.resync != Resync::None,
        &content.registry,
        &map,
        &mut visibility,
        &camera,
        &transform,
        viewer,
        aspect,
        lod_far(&frame),
    );
}

/// One change of the main renderer's refusals ([`VoxelRenderer::take_refused`]) to the cave
/// culling: a refused chunk shows an older mesh or nothing, so the search sees through it.
/// `ChunkVisibility` keeps its refused set to itself. The far LOD hears of the refusals that
/// leave nothing drawn apart ([`VoxelRenderer::take_undrawn`] into [`LodFeedback::chunks`]): a
/// refused chunk that keeps its older mesh is still drawn there.
///
/// [`VoxelRenderer::take_undrawn`]: crate::VoxelRenderer::take_undrawn
fn forward_refused(pos: ChunkPos, refused: bool, visibility: &mut ChunkVisibility, map: &ChunkMap) {
    visibility.set_unshown(pos, refused, map.visibility_links_at(pos));
}

/// The cave culling set (ADR 0022) of a frame with cave culling on, as the render plugin
/// computes it: what [`ChunkVisibility::update`] gives for the camera's view at `aspect`, over
/// `viewer`'s mesh region. `None`, culling nothing, on a renderer resync frame (`resync`), and
/// while a voxel within the near plane's reach of the eye occludes or is not loaded
/// ([`ChunkVisibility::eye_in_open`]): the renderer then shows what lies behind the rock.
///
/// A resync frame uploads only the settled meshes ([`ChunkMap::ready_meshes`]): a chunk being
/// remeshed draws nothing, although the map still says the renderer shows it (a remesh for
/// light alone keeps it shown) and trusts its links. The world hears of the resync only next
/// frame ([`ChunkMeshResync`]), when it stops showing those chunks.
///
/// Public for the GPU tests, which cull with it exactly as the plugin does.
#[doc(hidden)]
pub fn frame_visibility(
    resync: bool,
    registry: &BlockRegistry,
    map: &ChunkMap,
    visibility: &mut ChunkVisibility,
    camera: &Camera,
    transform: &Transform,
    viewer: ChunkViewer,
    aspect: f32,
) -> Option<Arc<VisibleSet>> {
    frame_visibility_with(
        resync, registry, map, visibility, camera, transform, viewer, aspect, None,
    )
}

/// [`frame_visibility`] with the search's far plane at `far` blocks along the view (the camera's
/// view distance with `None`, as there). While the far LOD draws (ADR 0031) the renderer culls
/// chunks with its far plane at the LOD's horizon, and the LOD hides its octants over every chunk
/// the world publishes, so the search must walk out there too: published chunks reach past the
/// view distance (the keep band, and the corners of the searched box), and a chunk the search
/// never walks is hidden on every tier, which would leave a hole where the LOD stepped aside.
///
/// Public for the GPU tests, which cull with it exactly as the plugin does.
#[doc(hidden)]
pub fn frame_visibility_with(
    resync: bool,
    registry: &BlockRegistry,
    map: &ChunkMap,
    visibility: &mut ChunkVisibility,
    camera: &Camera,
    transform: &Transform,
    viewer: ChunkViewer,
    aspect: f32,
    far: Option<f32>,
) -> Option<Arc<VisibleSet>> {
    if resync {
        return None;
    }
    let _span = frame_span!("render.visibility");
    let mut view = CameraView::compute(camera, transform, aspect);
    if let Some(far) = far {
        crate::renderer::far_plane_at(&mut view.frustum, view.eye_rel, far);
    }
    let open = visibility.eye_in_open(
        view.render_origin,
        view.eye_rel,
        camera.near_reach(aspect),
        |p| map.block_at(p).map(|state| registry.occludes(state)),
    );
    if !open {
        return None;
    }
    let search = VisibilityView::new(
        view.render_origin,
        Some(view.frustum),
        viewer.radius,
        viewer.vertical_radius,
    );
    Some(visibility.update(&search, |p| map.visibility_links_at(p)))
}

fn mesh_op(event: &ChunkMeshEvent) -> ChunkOp {
    match event {
        ChunkMeshEvent::Upsert { id, key, mesh } => ChunkOp::Upsert {
            id: id.to_bits(),
            origin: key.pos.origin().as_i64vec3(),
            mesh: Arc::clone(mesh),
        },
        ChunkMeshEvent::Remove { id, .. } => ChunkOp::Remove { id: id.to_bits() },
    }
}

// ---------------------------------------------------------------------------------------------
// Prepare and Draw (no main-world access)

/// `RenderSystems::Prepare`: chunk uploads and the view.
fn prepare(
    mut gpu: ResMut<GpuState>,
    mut frame: ResMut<ExtractedFrame>,
    mut warnings: Local<UploadWarnings>,
) {
    let GpuStage::Ready(ready) = &mut gpu.stage else {
        return;
    };
    if ready.ctx.health.is_lost() {
        return;
    }
    let frame = &mut *frame;
    let queue = &ready.ctx.queue;
    ready.renderer.set_translucent_sort(frame.translucent_sort);
    // The frame's chunk changes; a resync frame first runs the clear it asks for (`meshes` then
    // holds every ready mesh), before this frame's visible set is taken.
    let errors = ready
        .renderer
        .apply_ops(&ready.ctx.device, frame.meshes.drain(..), frame.resync);
    if frame.resync != Resync::None {
        ready.resync = Resync::None;
    }
    for e in &errors {
        warnings.record(e);
    }
    ready.renderer.set_outline(frame.outline.take());
    ready.renderer.set_visibility(frame.visibility.clone());
    let fog = frame.fog.take();
    if fog.is_some() && !ready.fog_ok {
        if !ready.fog_warned {
            ready.fog_warned = true;
            tracing::warn!("the fog failed its smoke test on this device: it stays off");
        }
    } else {
        ready.renderer.set_fog(&ready.ctx.device, fog);
    }
    let lod = frame.lod.take();
    if lod.is_some() && !ready.lod_ok {
        if !ready.lod_warned {
            ready.lod_warned = true;
            tracing::warn!("the far LOD failed its smoke test on this device: it stays off");
        }
        ready.renderer.set_lod(&ready.ctx.device, None);
    } else {
        ready.renderer.set_lod(&ready.ctx.device, lod);
    }
    warnings.flush(ready.renderer.stats().dropped_chunks);
    let color = ready.renderer.main_color().size();
    ready.overlay.prepare(
        &ready.ctx.device,
        queue,
        &frame.overlay,
        UVec2::new(color.width, color.height),
    );
    #[cfg(feature = "ui")]
    crate::ui::prepare_ui(
        &mut ready.ui,
        &ready.ctx.device,
        queue,
        &mut frame.ui,
        UVec2::new(color.width, color.height),
    );
    frame.view = frame.camera.map(|(camera, transform)| {
        let aspect = ready.config.width.max(1) as f32 / ready.config.height.max(1) as f32;
        let view = CameraView::compute(&camera, &transform, aspect);
        ViewParams {
            light: frame.light,
            fluid_time: frame.fluid_time,
            texture_tick: frame.texture_tick,
            ..ViewParams::from_camera(&view, &camera, frame.sky_color, frame.distance_fog)
        }
    });
}

/// `RenderSystems::Draw`: present through the frame gate, else (automation) render offscreen.
fn draw(
    mut gpu: ResMut<GpuState>,
    mut frame: ResMut<ExtractedFrame>,
    window: Option<NonSend<PrimaryWindow>>,
) {
    let GpuState {
        stage, counters, ..
    } = &mut *gpu;
    let GpuStage::Ready(ready) = stage else {
        return;
    };
    if ready.ctx.health.is_lost() {
        return;
    }
    let frame = &mut *frame;
    let Some(view) = frame.view else {
        return;
    };
    let gate_open = frame.window.frame_ready && !frame.window.occluded;
    if gate_open && let Some(surface) = ready.surface.as_ref() {
        let acquired = {
            let _span = frame_span!("render.acquire");
            acquire_frame(surface)
        };
        match acquired {
            FrameAcquire::Frame {
                texture,
                suboptimal,
            } => {
                let (commands, errors) = {
                    let _span = frame_span!("render.encode");
                    let target = texture
                        .texture
                        .create_view(&wgpu::TextureViewDescriptor::default());
                    let mut encoder =
                        ready
                            .ctx
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("boxel frame"),
                            });
                    ready
                        .renderer
                        .render(&ready.ctx.device, &ready.ctx.queue, &mut encoder, &view);
                    ready
                        .overlay
                        .encode(&mut encoder, ready.renderer.main_color_view());
                    #[cfg(feature = "ui")]
                    if let Some(ui) = &ready.ui {
                        ui.encode(&mut encoder, ready.renderer.main_color_view());
                    }
                    ready.renderer.blit(&mut encoder, &target);
                    // wgpu validates and encodes the recorded commands here
                    let _span = frame_span!("render.finish");
                    let errors = ready.ctx.health.error_count();
                    (encoder.finish(), errors)
                };
                {
                    let _span = frame_span!("render.submit");
                    ready.ctx.queue.submit([commands]);
                }
                after_frame(ready, errors);
                {
                    let _span = frame_span!("render.present");
                    if let Some(window) = window.as_deref() {
                        window.window.pre_present_notify();
                    }
                    ready.ctx.queue.present(texture);
                }
                after_present(ready, frame);
                ready.frames += 1;
                frame.rendered = true;
                frame.presented = true;
                frame.gate_used = true;
                if suboptimal {
                    let _span = frame_span!("render.reconfigure");
                    reconfigure(ready, counters, frame.window.physical_size);
                }
                return;
            }
            FrameAcquire::Skip => frame.gate_used = true,
            FrameAcquire::Reconfigure => {
                reconfigure(ready, counters, frame.window.physical_size);
            }
            FrameAcquire::Recreate => {
                if let Some(window) = window.as_deref() {
                    recreate_surface(ready, counters, &window.window, frame.window.physical_size);
                } else {
                    drop(ready.surface.take());
                }
            }
        }
    }
    if frame.render_when_hidden {
        render_offscreen(ready, &view);
        after_present(ready, frame);
        frame.rendered = true;
    }
}

/// After a frame was submitted (and presented): the work that waits for the frame to be out,
/// today the allocation probe ([`DebugAllocProbe`]). Not when the frame marked the device lost.
fn after_present(ready: &mut ReadyGpu, frame: &mut ExtractedFrame) {
    if ready.ctx.health.is_lost() {
        return;
    }
    for mib in frame.alloc_probe.drain(..) {
        let report = ready.renderer.debug_alloc_probe(&ready.ctx.device, mib);
        frame.alloc_probe_done.push(report);
    }
}

/// Renders into the main color target only (no acquire, no present), keeping at most
/// `MAX_OFFSCREEN_IN_FLIGHT` such frames queued on the GPU.
fn render_offscreen(ready: &mut ReadyGpu, view: &ViewParams) {
    {
        let _span = frame_span!("render.offscreen_wait");
        while ready.offscreen.len() >= MAX_OFFSCREEN_IN_FLIGHT {
            if let Some(oldest) = ready.offscreen.pop_front() {
                let _ = ready.ctx.device.poll(wgpu::PollType::Wait {
                    submission_index: Some(oldest),
                    timeout: Some(OFFSCREEN_WAIT),
                });
            }
        }
    }
    let (commands, errors) = {
        let _span = frame_span!("render.encode");
        let mut encoder =
            ready
                .ctx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("boxel offscreen frame"),
                });
        ready
            .renderer
            .render(&ready.ctx.device, &ready.ctx.queue, &mut encoder, view);
        ready
            .overlay
            .encode(&mut encoder, ready.renderer.main_color_view());
        #[cfg(feature = "ui")]
        if let Some(ui) = &ready.ui {
            ui.encode(&mut encoder, ready.renderer.main_color_view());
        }
        let _span = frame_span!("render.finish");
        let errors = ready.ctx.health.error_count();
        (encoder.finish(), errors)
    };
    let index = {
        let _span = frame_span!("render.submit");
        ready.ctx.queue.submit([commands])
    };
    after_frame(ready, errors);
    ready.offscreen.push_back(index);
    ready.frames += 1;
}

/// Called after a frame's submit with the device's error count from just before its `finish`.
/// The frame's chunk uploads were lost if
/// - an error came since then: none of the frame's commands ran (one invalid command buffer
///   fails the whole submit), the uploads `VoxelRenderer::render` recorded at its start
///   included. wgpu reports these errors on this thread, inside `finish` or `submit` (an error
///   from another thread at the same time only costs a needless resync);
/// - or `render` could not record them (`take_lost_uploads`: a staging buffer could not be
///   created, which wgpu reports before `finish`, and the frame itself runs).
///
/// Either way the renderer's chunk table and pages no longer match the GPU: the next frame
/// uploads every chunk again. Past [`MAX_REJECTED_FRAMES`] recent losses the device is marked
/// lost instead, and `Setup` rebuilds it.
fn after_frame(ready: &mut ReadyGpu, errors_before_finish: u32) {
    let submit_failed = ready.ctx.health.error_count() != errors_before_finish;
    let uploads_lost = ready.renderer.take_lost_uploads();
    match ready.rejected.record(submit_failed || uploads_lost) {
        FrameVerdict::Accepted => {}
        FrameVerdict::Resync => {
            let what = if submit_failed {
                "the GPU rejected a frame"
            } else {
                "a frame lost its chunk uploads"
            };
            tracing::warn!(
                "{what} ({} of {MAX_REJECTED_FRAMES} before the device is rebuilt); uploading \
                 every chunk again",
                ready.rejected.recent
            );
            ready.resync.raise(Resync::Chunks);
        }
        FrameVerdict::Lost => {
            ready.ctx.health.mark_lost(format!(
                "{MAX_REJECTED_FRAMES} recent frames lost their chunk uploads"
            ));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Cleanup and Shutdown

/// `RenderSystems::Cleanup`: write the frame results back to the main world.
fn cleanup(
    mut gpu: ResMut<GpuState>,
    mut frame: ResMut<ExtractedFrame>,
    window_state: Option<ResMut<WindowState>>,
    mut lkg: ResMut<LkgRes>,
    mut info: ResMut<RenderInfo>,
    mut surface_info: ResMut<SurfaceInfo>,
    board: Option<ResMut<DiagnosticsBoard>>,
    mut saved: MessageWriter<ScreenshotSaved>,
    time: Option<ResMut<Time>>,
    lod_feedback: Option<ResMut<LodFeedback>>,
) {
    let frame = &mut *frame;
    if frame.gate_used
        && let Some(mut state) = window_state
    {
        // The runner requests the next redraw after this update.
        state.frame_ready = false;
    }
    if frame.presented {
        info.frames_presented += 1;
    }
    if frame.rendered {
        info.frames_rendered += 1;
    }
    let GpuState {
        stage, counters, ..
    } = &mut *gpu;
    let mut gpu_errors = counters.retired_gpu_errors;
    match stage {
        GpuStage::Ready(ready) => {
            if frame.presented && !ready.confirmed {
                ready.confirmed = true;
                if let Some(store) = lkg.store.as_mut() {
                    store.confirm_good(&ready.ctx.caps.summary(), ready.ctx.tier);
                }
            }
            let lost = ready.ctx.health.is_lost();
            if ready.frames > 0 && !lost && !frame.screenshots.is_empty() {
                // The readback waits for the GPU and writes a file: a known stall.
                let started = Instant::now();
                for path in frame.screenshots.drain(..) {
                    let result = save_screenshot(
                        &ready.ctx.device,
                        &ready.ctx.queue,
                        ready.renderer.main_color(),
                        &path,
                    );
                    match &result {
                        Ok(()) => tracing::info!("screenshot saved to {}", path.display()),
                        Err(e) => tracing::error!("screenshot {}: {e}", path.display()),
                    }
                    saved.write(ScreenshotSaved { path, result });
                }
                if let Some(mut time) = time {
                    time.declare_stall(started.elapsed());
                }
            }
            if frame.destroy_device && !lost {
                tracing::warn!("DebugDestroyDevice: destroying the GPU device");
                ready.destroyed_on_purpose = true;
                ready.ctx.device.destroy();
                let _ = ready.ctx.device.poll(wgpu::PollType::Poll);
            }
            // The LOD pass's refusals reach the world side before its next `Update`.
            if let Some(mut feedback) = lod_feedback
                && let Some(report) = ready.renderer.take_lod_report()
            {
                feedback.refused.extend(report.refused);
                feedback.arena_free_quads = Some(report.arena_free_quads);
            }
            let stats = ready.renderer.stats();
            info.chunks = stats.chunks;
            info.draws = stats.draws;
            info.visible_chunks = stats.visible_chunks;
            info.occluded_chunks = stats.occluded_chunks;
            info.occlusion_time = stats.occlusion_time;
            info.translucent_sorts = stats.translucent_sorts;
            info.dropped_chunks = stats.dropped_chunks;
            info.arena_used_quads = stats.arena_used_quads;
            info.arena_capacity_quads = stats.arena_capacity_quads;
            info.chunk_slot_capacity = stats.chunk_slot_capacity;
            info.chunk_memory = stats.chunk_memory;
            for report in frame.alloc_probe_done.drain(..) {
                let seq = info.alloc_probe.map_or(1, |(seq, _)| seq + 1);
                info.alloc_probe = Some((seq, report));
            }
            info.lod_nodes = stats.lod_nodes;
            info.lod_draws = stats.lod_draws;
            info.lod_quads = stats.lod_quads;
            info.lod_upload_quads = stats.lod_upload_quads;
            info.lod_arena_used_quads = stats.lod_arena_used_quads;
            info.lod_arena_capacity_quads = stats.lod_arena_capacity_quads;
            #[cfg(feature = "ui")]
            {
                let ui = ready.ui.as_ref().map(crate::ui::UiPass::stats);
                let ui = ui.unwrap_or_default();
                info.ui_quads = ui.quads;
                info.ui_uploads = ui.uploads;
                info.ui_atlas_uploads = ui.atlas_uploads;
            }
            if let Some(mut board) = board {
                board.set("render", render_line(ready.ctx.tier, &stats));
            }
            gpu_errors = gpu_errors.saturating_add(ready.gpu_errors());
            surface_info.size = UVec2::new(ready.config.width, ready.config.height);
            info.present_mode = Some(ready.config.present_mode);
        }
        GpuStage::Failed | GpuStage::Stopped => {
            info.present_mode = None;
            for path in frame.screenshots.drain(..) {
                saved.write(ScreenshotSaved {
                    path,
                    result: Err("no GPU renderer".to_string()),
                });
            }
        }
        GpuStage::WaitingForWindow => info.present_mode = None,
    }
    surface_info.reconfigures = counters.reconfigures;
    surface_info.recreates = counters.recreates;
    surface_info.errors = counters.failures.saturating_add(gpu_errors);
}

/// The `"render"` diagnostics line.
fn render_line(tier: Tier, stats: &crate::RenderStats) -> String {
    let memory = &stats.chunk_memory;
    let mib = |quads: u64| (quads * 8) >> 20;
    format!(
        "tier={tier} chunks={} quads={}/{} draws={} visible={} dropped={} occluded={} \
         pages={} {}/{}MiB",
        stats.chunks,
        stats.arena_used_quads,
        stats.arena_capacity_quads,
        stats.draws,
        stats.visible_chunks,
        stats.dropped_chunks,
        stats.occluded_chunks,
        memory.pages,
        mib(memory.used_quads),
        mib(memory.capacity_quads),
    )
}

/// `settings` if valid, else the defaults (logged once a renderer is built).
fn valid_chunk_memory(settings: ChunkMemorySettings) -> ChunkMemorySettings {
    match settings.validate() {
        Ok(()) => settings,
        Err(e) => {
            tracing::warn!("RenderSettings::chunk_memory: {e}; using the defaults");
            ChunkMemorySettings::default()
        }
    }
}

/// Reads the main color target and writes a PNG (parent directories are created).
fn save_screenshot(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    path: &Path,
) -> Result<(), String> {
    let image = read_texture_rgba8(device, queue, texture)?;
    write_png(&image, path)
}

#[cfg(feature = "png")]
fn write_png(image: &boxel_gfx::RgbaImage, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    image
        .save_png(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(feature = "png"))]
fn write_png(_image: &boxel_gfx::RgbaImage, _path: &Path) -> Result<(), String> {
    Err("png feature disabled".to_string())
}

/// `Shutdown` schedule: wait for the GPU (bounded), drop renderer, surface and context in that
/// order, and restore unconfirmed LKG attempts.
fn shutdown(mut gpu: ResMut<GpuState>, mut lkg: ResMut<LkgRes>, mut info: ResMut<RenderInfo>) {
    if let Some(ready) = gpu.ready()
        && !ready.ctx.health.is_lost()
    {
        let _ = ready.ctx.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(SHUTDOWN_WAIT),
        });
    }
    gpu.drop_ready(GpuStage::Stopped);
    if let Some(store) = lkg.store.as_mut() {
        // No-op when the selection was confirmed (nothing of this process is `Pending`).
        store.cancel_pending();
    }
    info.ready = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_math::I64Vec3;
    use boxel_mesh::ChunkMesh;

    #[test]
    fn first_loss_retries_and_the_second_excludes_the_tier() {
        let mut losses = Losses::default();
        assert!(!losses.record("nv", Tier::High));
        assert!(losses.exclusions.is_empty());
        assert!(losses.record("nv", Tier::High));
        assert_eq!(
            losses.exclusions,
            vec![Exclusion {
                adapter_key: "nv".to_string(),
                max_tier: Some(Tier::Mid),
            }]
        );
        // Losses are counted per adapter.
        assert!(!losses.record("amd", Tier::Mid));
        // A third loss at Mid tightens the same exclusion.
        assert!(losses.record("nv", Tier::Mid));
        assert_eq!(losses.exclusions.len(), 1);
        assert_eq!(losses.exclusions[0].max_tier, Some(Tier::Low));
        // A repeated loss at Low excludes the adapter entirely.
        assert!(losses.record("nv", Tier::Low));
        assert_eq!(losses.exclusions[0].max_tier, None);
        losses.exclude("nv", Some(Tier::High));
        assert_eq!(losses.exclusions[0].max_tier, None, "never loosened");
    }

    #[test]
    fn an_invalid_distance_fog_keeps_the_last_valid_one() {
        let off = DistanceFog::off();
        let short = DistanceFog {
            max_fade: 16.0,
            ..DistanceFog::default()
        };
        assert_eq!(valid_distance_fog(short, off), short);
        assert_eq!(valid_distance_fog(off, short), off);
        let bad = DistanceFog {
            fade_fraction: 2.0,
            ..DistanceFog::default()
        };
        assert_eq!(valid_distance_fog(bad, short), short);
        assert_eq!(
            ExtractedFrame::default().distance_fog,
            DistanceFog::default()
        );
    }

    #[test]
    fn repeated_rejected_frames_escalate_to_a_device_loss() {
        let mut frames = RejectedFrames::default();
        assert_eq!(frames.record(false), FrameVerdict::Accepted);
        assert_eq!(frames.record(true), FrameVerdict::Resync);
        assert_eq!(frames.record(true), FrameVerdict::Resync);
        assert_eq!(frames.record(true), FrameVerdict::Lost);

        // Rejected frames between accepted ones still add up...
        let mut frames = RejectedFrames::default();
        for _ in 0..MAX_REJECTED_FRAMES - 1 {
            assert_eq!(frames.record(true), FrameVerdict::Resync);
            for _ in 0..REJECTED_FRAMES_FORGOTTEN_AFTER - 1 {
                assert_eq!(frames.record(false), FrameVerdict::Accepted);
            }
        }
        assert_eq!(frames.record(true), FrameVerdict::Lost);

        // ...until enough accepted frames in a row forget them.
        let mut frames = RejectedFrames::default();
        for _ in 0..3 * MAX_REJECTED_FRAMES {
            assert_eq!(frames.record(true), FrameVerdict::Resync);
            for _ in 0..REJECTED_FRAMES_FORGOTTEN_AFTER {
                assert_eq!(frames.record(false), FrameVerdict::Accepted);
            }
        }
    }

    #[test]
    fn a_deliberate_device_loss_never_adds_an_lkg_strike() {
        assert!(!adds_lkg_strike(false, false), "first loss: retry only");
        assert!(adds_lkg_strike(true, false), "repeated driver loss");
        assert!(!adds_lkg_strike(true, true), "DebugDestroyDevice");
    }

    #[test]
    fn present_settings_switch_vsync() {
        let mut settings = PresentSettings::default();
        assert_eq!(
            settings.mode,
            PresentModePref::NoVsync,
            "vsync off by default"
        );
        assert!(!settings.is_vsync());
        settings.set_vsync(true);
        assert_eq!(settings, PresentSettings::vsync(true));
        assert_eq!(settings.mode, PresentModePref::Vsync);
        settings.set_vsync(false);
        assert_eq!(settings.mode, PresentModePref::NoVsync);

        let mut info = RenderInfo::default();
        assert_eq!(info.vsync(), None);
        for (mode, vsync) in [
            (wgpu::PresentMode::Fifo, true),
            (wgpu::PresentMode::FifoRelaxed, true),
            (wgpu::PresentMode::Mailbox, false),
            (wgpu::PresentMode::Immediate, false),
        ] {
            info.present_mode = Some(mode);
            assert_eq!(info.vsync(), Some(vsync), "{mode:?}");
        }
    }

    #[test]
    fn render_line_format() {
        let stats = crate::RenderStats {
            chunks: 3,
            arena_used_quads: 10,
            arena_capacity_quads: 100,
            chunk_slot_capacity: 64,
            visible_chunks: 2,
            draws: 5,
            dropped_chunks: 1,
            occluded_chunks: 4,
            chunk_memory: ChunkMemoryStats {
                pages: 2,
                used_quads: 3 << 20,
                capacity_quads: 8 << 20,
                ..ChunkMemoryStats::default()
            },
            ..crate::RenderStats::default()
        };
        assert_eq!(
            render_line(Tier::Mid, &stats),
            "tier=Mid chunks=3 quads=10/100 draws=5 visible=2 dropped=1 occluded=4 pages=2 \
             24/64MiB"
        );
    }

    #[test]
    fn upload_warnings_are_rate_limited() {
        let mut w = UploadWarnings::default();
        w.flush(0);
        assert!(w.last.is_none(), "nothing to report");
        w.record(&crate::RendererError::ArenaFull { needed: 7 });
        w.flush(1);
        assert!(w.last.is_some());
        assert_eq!(w.failures, 0);
        let first = w.last;
        w.record(&crate::RendererError::ChunkTableFull(4));
        w.flush(2);
        assert_eq!(w.last, first, "second warning within 5 s is held back");
        assert_eq!(w.failures, 1);
    }

    #[test]
    fn mesh_events_become_ops_with_world_origins() {
        // `ChunkId` is a slotmap key; without a slotmap dependency the null key is the only one
        // this crate can build, which is enough to check the id and origin conversion.
        let chunk = boxel_world::ChunkId::default();
        let key = boxel_voxel::ChunkKey::new(ChunkPos::new(1, -2, 3));
        let mesh = Arc::new(ChunkMesh::default());
        match mesh_op(&ChunkMeshEvent::Upsert {
            id: chunk,
            key,
            mesh: Arc::clone(&mesh),
        }) {
            ChunkOp::Upsert { id, origin, .. } => {
                assert_eq!(id, chunk.to_bits());
                assert_eq!(origin, I64Vec3::new(32, -64, 96));
            }
            ChunkOp::Remove { .. } => panic!("expected an upsert"),
        }
        match mesh_op(&ChunkMeshEvent::Remove { id: chunk, key }) {
            ChunkOp::Remove { id } => assert_eq!(id, chunk.to_bits()),
            ChunkOp::Upsert { .. } => panic!("expected a remove"),
        }
    }

    #[test]
    fn the_scene_light_follows_the_world_clock() {
        let settings = RenderSettings {
            clear_color: [0.1, 0.2, 0.3, 1.0],
            ao_strength: 0.4,
            block_light_gain: 1.5,
            light_gamma: 1.0,
            ..RenderSettings::default()
        };
        // Without a clock: a white noon sky of the configured color.
        let (sky, light) = scene_light(&settings, None);
        assert_eq!(sky, settings.clear_color);
        assert_eq!(light.sky, [1.0; 3]);
        assert_eq!(light.ambient, SceneLight::DEFAULT_AMBIENT);
        assert_eq!((light.ao_strength, light.block_gain), (0.4, 1.5));
        assert_eq!(light.curve, light_curve(1.0));
        // With one: the curve's sky at that time; the shading settings stay.
        let curve = DaylightCurve::default();
        let midnight = TimeOfDay::at_clock(0.0);
        let (sky, night) = scene_light(&settings, Some((&midnight, &curve)));
        let expected = curve.at(&midnight);
        assert_eq!((sky, night.sky), (expected.sky_color, expected.sky_light));
        assert_eq!(night.ambient, expected.ambient);
        assert_eq!((night.ao_strength, night.block_gain), (0.4, 1.5));
        let noon = TimeOfDay::at_clock(12.0);
        let (sky, day) = scene_light(&RenderSettings::default(), Some((&noon, &curve)));
        assert_eq!((sky, day), (DEFAULT_SKY_COLOR, SceneLight::noon()));
    }

    /// A refusal change of the main renderer reaches the cave culling: its next update searches
    /// again, the refused chunk now see-through.
    #[test]
    fn refusals_reach_the_cave_culling() {
        let map = ChunkMap::default();
        let view = VisibilityView::new(I64Vec3::ZERO, None, 2, 2);
        let mut visibility = ChunkVisibility::default();
        visibility.update(&view, |_| 0);
        visibility.update(&view, |_| 0);
        assert_eq!(
            visibility.stats().searches,
            1,
            "the second update reuses the first"
        );
        let pos = ChunkPos::new(1, 0, 0);
        forward_refused(pos, true, &mut visibility, &map);
        visibility.update(&view, |_| 0);
        assert_eq!(
            visibility.stats().searches,
            2,
            "a refused chunk opens its links"
        );
        forward_refused(pos, false, &mut visibility, &map);
    }
}
