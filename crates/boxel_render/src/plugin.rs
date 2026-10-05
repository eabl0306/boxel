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

use crate::{RendererConfig, RendererSmokeTest, ViewParams, VoxelRenderer};
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
use boxel_math::{IVec3, UVec2};
use boxel_mesh::ChunkMesh;
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, TextureSpec};
use boxel_window::{DisplayHandle, PrimaryWindow, WindowState};
use boxel_world::{ChunkMap, ChunkMeshEvent, WorldContent};
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
    /// Draw calls recorded last frame (`RenderStats::draws`: Mid/Low one per visible bucket;
    /// High its 2 indirect multi-draws, opaque + cutout).
    pub draws: u32,
    /// Device losses recovered from.
    pub recoveries: u32,
    /// `RenderStats::dropped_chunks` (arena or chunk table full; warn-logged).
    pub dropped_chunks: u32,
}

/// Surface and error counters of the render plugin, for automation and diagnostics (additive to
/// the M0 skeleton API; `voxel_world --resize-test` reads it). Updated in `Cleanup`.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceInfo {
    /// Size of the configured surface in physical pixels (zero before the first selection).
    pub size: UVec2,
    /// Successful reconfigurations since startup (resize, `Outdated`, `Suboptimal`).
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
/// Chunk meshes are then re-uploaded from `ChunkMap::ready_meshes()`.
#[derive(Clone, Debug, Default)]
pub struct RenderPlugin {
    /// Settings.
    pub settings: RenderSettings,
}

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.settings.clone())
            .init_resource::<RenderInfo>()
            .init_resource::<SurfaceInfo>()
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
            .add_message::<ChunkMeshEvent>()
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
                extract.in_set(RenderSystems::Extract),
                prepare.in_set(RenderSystems::Prepare),
                draw.in_set(RenderSystems::Draw),
                cleanup.in_set(RenderSystems::Cleanup),
            ),
        )
        .add_systems(Shutdown, shutdown);
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

/// Selected device with its surface and renderer. Field order is the drop order (renderer,
/// surface, context), see [`ReadyGpu::teardown`].
struct ReadyGpu {
    renderer: VoxelRenderer,
    /// `None` only between a failed `Lost` recreation and the next `Setup`.
    surface: Option<wgpu::Surface<'static>>,
    config: wgpu::SurfaceConfiguration,
    ctx: GpuContext,
    /// Window size the surface was last configured for (`config` may be clamped).
    configured_for: UVec2,
    present: PresentModePref,
    /// Next `Extract` uploads every ready mesh (`renderer.clear_chunks` first).
    resync: bool,
    /// LKG `confirm_good` was called (first presented frame of this selection).
    confirmed: bool,
    /// Frames rendered with this renderer (screenshots wait for the first one).
    frames: u64,
    /// Submissions of offscreen frames still possibly in flight.
    offscreen: VecDeque<wgpu::SubmissionIndex>,
    /// [`DebugDestroyDevice`] destroyed this device: the loss is ours, not the driver's.
    destroyed_on_purpose: bool,
}

impl ReadyGpu {
    /// Drops renderer, surface and context, in that order.
    fn teardown(self: Box<Self>) {
        let ReadyGpu {
            renderer,
            surface,
            config,
            ctx,
            ..
        } = *self;
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
struct GpuState {
    stage: GpuStage,
    counters: SurfaceCounters,
}

impl GpuState {
    fn ready(&self) -> Option<&ReadyGpu> {
        match &self.stage {
            GpuStage::Ready(ready) => Some(ready),
            _ => None,
        }
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

/// One chunk mesh change for the renderer.
#[derive(Clone, Debug)]
enum MeshOp {
    Upsert {
        id: u64,
        origin: IVec3,
        mesh: Arc<ChunkMesh>,
    },
    Remove {
        id: u64,
    },
}

/// The extract boundary: inputs copied from the main world by `Extract`, the view computed by
/// `Prepare`, and the frame results `Cleanup` writes back.
#[derive(Resource, Default)]
struct ExtractedFrame {
    camera: Option<(Camera, Transform)>,
    window: WindowState,
    clear_color: [f32; 4],
    fog_start_fraction: f32,
    render_when_hidden: bool,
    /// Full resync: `meshes` holds every ready mesh, `renderer.clear_chunks` runs first.
    resync: bool,
    meshes: Vec<MeshOp>,
    /// Pending screenshot paths (kept until a frame exists to read).
    screenshots: Vec<PathBuf>,
    destroy_device: bool,
    /// `Prepare` output.
    view: Option<ViewParams>,
    /// `Draw` results.
    rendered: bool,
    presented: bool,
    /// The presentation gate was used (presented or skipped): `Cleanup` closes it.
    gate_used: bool,
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
             RenderSettings::arena_quads/max_chunk_slots or lower the view distance",
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
    let GpuState { stage, counters } = &mut *p.gpu;
    if let GpuStage::Ready(ready) = stage {
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
    let default_registry;
    let textures: &[TextureSpec] = match p.content.as_deref() {
        Some(content) => content.registry.textures(),
        None => {
            default_registry = BlockRegistry::default_blocks();
            default_registry.textures()
        }
    };
    let config = RendererConfig {
        arena_quads: settings.arena_quads,
        max_chunk_slots: settings.max_chunk_slots,
        clear_color: settings.clear_color,
    };
    let present = prefs.present_mode.unwrap_or_default();
    let built = VoxelRenderer::new(
        &ctx.device,
        &ctx.queue,
        ctx.tier,
        &config,
        textures,
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
    let (renderer, surface_config) = match built {
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

    // 8. Success.
    let summary = ctx.summary_line();
    tracing::info!("boxel: {summary}");
    p.gpu_ready.write(GpuReady {
        summary: summary.clone(),
        tier: ctx.tier,
    });
    p.info.ready = true;
    p.info.tier = Some(ctx.tier);
    p.info.class = Some(ctx.class);
    p.info.summary.clone_from(&summary);
    if let Some(board) = p.board.as_deref_mut() {
        board.set("gpu", summary);
    }
    p.gpu.stage = GpuStage::Ready(Box::new(ReadyGpu {
        renderer,
        surface: Some(surface),
        config: surface_config,
        ctx,
        configured_for: size,
        present,
        resync: true,
        confirmed: false,
        frames: 0,
        offscreen: VecDeque::new(),
        destroyed_on_purpose: false,
    }));
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
        smoke: &RendererSmokeTest,
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

/// `RenderSystems::Extract`: the only reads of the main world for this frame's rendering.
fn extract(
    settings: Res<RenderSettings>,
    gpu: Res<GpuState>,
    mut frame: ResMut<ExtractedFrame>,
    window: Option<Res<WindowState>>,
    cameras: Query<(&Camera, &Transform)>,
    chunks: Option<Res<ChunkMap>>,
    mut mesh_events: MessageReader<ChunkMeshEvent>,
    mut screenshots: MessageReader<RequestScreenshot>,
    mut destroy: MessageReader<DebugDestroyDevice>,
) {
    let frame = &mut *frame;
    frame.camera = cameras
        .iter()
        .find(|(camera, _)| camera.active)
        .map(|(camera, transform)| (*camera, *transform));
    frame.window = window.as_deref().copied().unwrap_or_default();
    frame.clear_color = settings.clear_color;
    frame.fog_start_fraction = settings.fog_start_fraction;
    frame.render_when_hidden = settings.render_when_hidden;
    frame
        .screenshots
        .extend(screenshots.read().map(|r| r.path.clone()));
    frame.destroy_device = destroy.read().count() > 0;
    frame.view = None;
    frame.rendered = false;
    frame.presented = false;
    frame.gate_used = false;

    let ready = gpu.ready().filter(|r| !r.ctx.health.is_lost());
    frame.resync = ready.is_some_and(|r| r.resync);
    frame.meshes.clear();
    if frame.resync {
        // Everything currently published replaces whatever the events would say.
        mesh_events.clear();
        if let Some(map) = chunks.as_deref() {
            frame
                .meshes
                .extend(map.ready_meshes().map(|(id, key, mesh)| MeshOp::Upsert {
                    id: id.to_bits(),
                    origin: key.pos.origin().as_ivec3(),
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

fn mesh_op(event: &ChunkMeshEvent) -> MeshOp {
    match event {
        ChunkMeshEvent::Upsert { id, key, mesh } => MeshOp::Upsert {
            id: id.to_bits(),
            origin: key.pos.origin().as_ivec3(),
            mesh: Arc::clone(mesh),
        },
        ChunkMeshEvent::Remove { id, .. } => MeshOp::Remove { id: id.to_bits() },
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
    if frame.resync {
        ready.renderer.clear_chunks(queue);
        ready.resync = false;
    }
    for op in frame.meshes.drain(..) {
        let result = match op {
            MeshOp::Upsert { id, origin, mesh } => {
                ready.renderer.upsert_chunk(queue, id, origin, &mesh)
            }
            MeshOp::Remove { id } => {
                ready.renderer.remove_chunk(queue, id);
                Ok(())
            }
        };
        if let Err(e) = result {
            warnings.record(&e);
        }
    }
    warnings.flush(ready.renderer.stats().dropped_chunks);
    frame.view = frame.camera.map(|(camera, transform)| {
        let aspect = ready.config.width.max(1) as f32 / ready.config.height.max(1) as f32;
        let view = CameraView::compute(&camera, &transform, aspect);
        ViewParams::from_camera(&view, &camera, frame.clear_color, frame.fog_start_fraction)
    });
}

/// `RenderSystems::Draw`: present through the frame gate, else (automation) render offscreen.
fn draw(
    mut gpu: ResMut<GpuState>,
    mut frame: ResMut<ExtractedFrame>,
    window: Option<NonSend<PrimaryWindow>>,
) {
    let GpuState { stage, counters } = &mut *gpu;
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
        match acquire_frame(surface) {
            FrameAcquire::Frame {
                texture,
                suboptimal,
            } => {
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
                ready.renderer.blit(&mut encoder, &target);
                ready.ctx.queue.submit([encoder.finish()]);
                if let Some(window) = window.as_deref() {
                    window.window.pre_present_notify();
                }
                ready.ctx.queue.present(texture);
                ready.frames += 1;
                frame.rendered = true;
                frame.presented = true;
                frame.gate_used = true;
                if suboptimal {
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
        frame.rendered = true;
    }
}

/// Renders into the main color target only (no acquire, no present), keeping at most
/// `MAX_OFFSCREEN_IN_FLIGHT` such frames queued on the GPU.
fn render_offscreen(ready: &mut ReadyGpu, view: &ViewParams) {
    while ready.offscreen.len() >= MAX_OFFSCREEN_IN_FLIGHT {
        if let Some(oldest) = ready.offscreen.pop_front() {
            let _ = ready.ctx.device.poll(wgpu::PollType::Wait {
                submission_index: Some(oldest),
                timeout: Some(OFFSCREEN_WAIT),
            });
        }
    }
    let mut encoder = ready
        .ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("boxel offscreen frame"),
        });
    ready
        .renderer
        .render(&ready.ctx.device, &ready.ctx.queue, &mut encoder, view);
    let index = ready.ctx.queue.submit([encoder.finish()]);
    ready.offscreen.push_back(index);
    ready.frames += 1;
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
    let GpuState { stage, counters } = &mut *gpu;
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
            if ready.frames > 0 && !lost {
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
            }
            if frame.destroy_device && !lost {
                tracing::warn!("DebugDestroyDevice: destroying the GPU device");
                ready.destroyed_on_purpose = true;
                ready.ctx.device.destroy();
                let _ = ready.ctx.device.poll(wgpu::PollType::Poll);
            }
            let stats = ready.renderer.stats();
            info.chunks = stats.chunks;
            info.draws = stats.draws;
            info.dropped_chunks = stats.dropped_chunks;
            if let Some(mut board) = board {
                board.set("render", render_line(ready.ctx.tier, &stats));
            }
            gpu_errors = gpu_errors.saturating_add(ready.gpu_errors());
            surface_info.size = UVec2::new(ready.config.width, ready.config.height);
        }
        GpuStage::Failed | GpuStage::Stopped => {
            for path in frame.screenshots.drain(..) {
                saved.write(ScreenshotSaved {
                    path,
                    result: Err("no GPU renderer".to_string()),
                });
            }
        }
        GpuStage::WaitingForWindow => {}
    }
    surface_info.reconfigures = counters.reconfigures;
    surface_info.recreates = counters.recreates;
    surface_info.errors = counters.failures.saturating_add(gpu_errors);
}

/// The `"render"` diagnostics line.
fn render_line(tier: Tier, stats: &crate::RenderStats) -> String {
    format!(
        "tier={tier} chunks={} quads={}/{} draws={} visible={} dropped={}",
        stats.chunks,
        stats.arena_used_quads,
        stats.arena_capacity_quads,
        stats.draws,
        stats.visible_chunks,
        stats.dropped_chunks
    )
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
    fn a_deliberate_device_loss_never_adds_an_lkg_strike() {
        assert!(!adds_lkg_strike(false, false), "first loss: retry only");
        assert!(adds_lkg_strike(true, false), "repeated driver loss");
        assert!(!adds_lkg_strike(true, true), "DebugDestroyDevice");
    }

    #[test]
    fn render_line_format() {
        let stats = crate::RenderStats {
            chunks: 3,
            arena_used_quads: 10,
            arena_capacity_quads: 100,
            visible_chunks: 2,
            draws: 5,
            dropped_chunks: 1,
        };
        assert_eq!(
            render_line(Tier::Mid, &stats),
            "tier=Mid chunks=3 quads=10/100 draws=5 visible=2 dropped=1"
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
        let key = boxel_voxel::ChunkKey::new(boxel_voxel::ChunkPos::new(1, -2, 3));
        let mesh = Arc::new(ChunkMesh::default());
        match mesh_op(&ChunkMeshEvent::Upsert {
            id: chunk,
            key,
            mesh: Arc::clone(&mesh),
        }) {
            MeshOp::Upsert { id, origin, .. } => {
                assert_eq!(id, chunk.to_bits());
                assert_eq!(origin, IVec3::new(32, -64, 96));
            }
            MeshOp::Remove { .. } => panic!("expected an upsert"),
        }
        match mesh_op(&ChunkMeshEvent::Remove { id: chunk, key }) {
            MeshOp::Remove { id } => assert_eq!(id, chunk.to_bits()),
            MeshOp::Upsert { .. } => panic!("expected a remove"),
        }
    }
}
