use crate::input::{
    ButtonState, CursorMoved, KeyboardInput, MouseButtonInput, MouseMotion, MouseWheel,
};
use crate::window::{
    CursorGrab, CursorOptions, PrimaryWindow, WindowCloseRequested, WindowCreated, WindowFocused,
    WindowMode, WindowOccluded, WindowResized, WindowSettings, WindowState,
};
use boxel_app::{
    App, AppExit, FrameDeadline, FrameLimit, FramePacer, FrameWait, IDLE_MARGIN, IDLE_POLL,
    IdleInfo, IdleWork, MIN_IDLE_STEP, PluginsState, idle_step_end, spin_until,
};
use boxel_ecs::prelude::*;
use boxel_math::{UVec2, Vec2};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

/// Update pacing of the winit runner. Both modes respect [`FrameLimit`] (the app's resource,
/// read before every frame; `WindowPlugin` inserts the default of 144 fps).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateMode {
    /// Update continuously at the [`FrameLimit`] rate (games): `ControlFlow::WaitUntil` for the
    /// coarse part of each wait, a short spin for the last fraction of a millisecond, and
    /// `ControlFlow::Poll` when the limit is 0 (unlimited). On Wayland a limit at or above the
    /// refresh rate follows the display instead: each update runs when its frame callback
    /// arrives (see [`winit_runner`]).
    #[default]
    Continuous,
    /// Update when an event arrives or after `wait` (tools), never faster than [`FrameLimit`].
    Reactive {
        /// Max time between updates.
        wait: Duration,
    },
}

/// How the winit runner paced the current update, for diagnostics (overlays, benchmarks). The
/// runner writes it right before each update; `WindowPlugin` inserts it.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FramePacingInfo {
    /// How long this update waited past its deadline for the presentation gate (zero: it did
    /// not wait). See [`winit_runner`].
    pub gate_wait: Duration,
    /// This update stopped waiting with the gate still closed, so it runs without presenting.
    pub gate_missed: bool,
    /// When the presentation gate last opened (`RedrawRequested`; on Wayland the compositor's
    /// frame callback for the last presented frame).
    pub gate_opened_at: Option<Instant>,
    /// Refresh rate of the window's monitor in millihertz, as winit reports it (`None`:
    /// unknown). Polled about once per second where the gate follows frame callbacks (Wayland),
    /// once elsewhere.
    pub refresh_millihertz: Option<u32>,
    /// This update follows the display: the gate opens with the compositor's frame callbacks
    /// and the [`FrameLimit`] is at or above the refresh rate, so updates run when the
    /// compositor asks for a frame (see [`winit_runner`]).
    pub display_locked: bool,
    /// Updates that waited for the gate, since the window was created.
    pub gate_waits: u64,
    /// Updates that gave up waiting for the gate, since the window was created.
    pub gate_misses: u64,
}

/// A requested redraw that has not arrived after this long marks the window occluded.
const REDRAW_TIMEOUT: Duration = Duration::from_millis(100);
/// Time between updates while the window is occluded (30 Hz).
const OCCLUDED_WAIT: Duration = Duration::from_millis(33);
/// Longest wait past a frame deadline for the presentation gate (see `RunnerCore::plan`).
const GATE_WAIT_MAX: Duration = Duration::from_millis(2);
/// A gate that opens sooner than this after its redraw was requested did not wait for the
/// compositor: no frame callback was pending (X11, or a window nothing presents to).
const QUICK_GATE: Duration = Duration::from_micros(500);
/// After this many quick gate openings in a row the gate no longer follows the display.
const QUICK_GATES_MAX: u8 = 8;
/// Gate openings that waited for the compositor needed in a row before the gate follows the
/// display again (one slow opening can be a busy compositor answering a plain redraw).
const SLOW_GATES_TO_LOCK: u8 = 2;
/// Frame callback intervals kept to check the reported refresh rate (`CallbackCadence`).
const CADENCE_SAMPLES: usize = 16;
/// The callback cadence is judged only with at least this many intervals.
const CADENCE_MIN_SAMPLES: usize = 8;
/// A median callback interval off the reported refresh period by more than this share, in per
/// mille, means the reported rate is not the one the compositor follows (another monitor, VRR):
/// updates stop following the display.
const CADENCE_TOLERANCE_PER_MILLE: u32 = 100;
/// A frame limit at or above this share of the display refresh rate, in per mille, follows the
/// display (ADR 0017): within 1 % the limit and the panel are the same rate, and above it the
/// extra updates could never be shown.
const DISPLAY_MATCH_PER_MILLE: u64 = 990;
/// Time between `Plugin::ready` polls while plugins are still being set up.
const PLUGINS_POLL: Duration = Duration::from_millis(1);
/// Time between `Window::is_minimized` polls (X11 only; each poll is a server round trip).
const MINIMIZED_POLL: Duration = Duration::from_millis(100);
/// Time between `Window::current_monitor` polls (the refresh rate of the window's display; the
/// window can move to another monitor). Only where the gate follows frame callbacks (the only
/// use of the rate); elsewhere the monitor is polled once.
const MONITOR_POLL: Duration = Duration::from_secs(1);
/// Pixel scroll deltas are converted to lines with this factor.
const PIXELS_PER_LINE: f64 = 32.0;

/// Runner knobs from [`WindowPlugin`](crate::WindowPlugin), read once when the runner starts.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunnerSettings {
    /// Write `AppExit::Success` on `CloseRequested`.
    pub(crate) exit_on_close: bool,
    /// Update pacing.
    pub(crate) update_mode: UpdateMode,
}

impl Default for RunnerSettings {
    fn default() -> Self {
        Self {
            exit_on_close: true,
            update_mode: UpdateMode::Continuous,
        }
    }
}

/// The winit runner (installed by `WindowPlugin`). Contract (frozen):
/// 1. Waits for `App::plugins_state() == Ready`, then `finish()` + `cleanup()`.
/// 2. `resumed`: creates the primary window once from `WindowSettings`, inserts the non-send
///    `PrimaryWindow` (`App::insert_non_send`) and `WindowState`, writes `WindowCreated`, and
///    starts the redraw protocol (the first `RedrawRequested` is pending; request one if the
///    platform does not deliver it on its own).
/// 3. `window_event`: translates to messages and `WindowState`; `CloseRequested` writes
///    `WindowCloseRequested` (+ `AppExit::Success` if `exit_on_close`); `RedrawRequested` sets
///    `WindowState::frame_ready = true` and clears the pending-redraw timestamp. Messages are
///    written only if their `Messages<T>` resource exists.
/// 4. `device_event`: `MouseMotion` while focused.
/// 5. `about_to_wait`: applies `CursorOptions` changes; computes
///    `occluded = winit_occluded || (redraw pending for > 100 ms)` and writes `WindowOccluded` on
///    change; runs `app.update()`; if `!frame_ready` and no redraw is pending, calls
///    `window.request_redraw()` and records the time; exits the loop on `app.should_exit()`.
///    The update still runs in `about_to_wait`; idle steps ([`App::run_idle`], ADR 0022) run
///    in the later iterations of the wait, never in the iteration of the update.
///    Control flow: `Continuous` waits for the next [`FrameLimit`] deadline (`WaitUntil` the
///    deadline minus the spin margin, then a spin; `Poll` when unlimited); `Reactive` waits
///    `WaitUntil(now + wait)` but never runs an update before the limit allows it; while
///    occluded the period is at least 33 ms. Updates always run in `about_to_wait`, never inside
///    `RedrawRequested`, and never wait for it longer than a refresh plus a quarter of one (at
///    most 2 ms) past their deadline or past the redraw request, whichever is later (ADR 0017).
/// 6. `exiting`: `app.shutdown()` (renderer releases renderer, surface and device in that
///    order), then removes `PrimaryWindow`, all while the event loop is still alive.
/// 7. Returns the stored `AppExit` (`AppExit::error()` if `run_app` failed).
///
/// Details of this implementation:
/// - Updates start once the window exists and plugins are finished (desktop platforms deliver
///   `resumed` before the first `about_to_wait`).
/// - `WindowResized` is written only when the size actually changes (never for 0x0). On Wayland,
///   `Window::request_inner_size` is applied client-side and winit sends no `Resized` event, so
///   the runner compares `Window::inner_size` with `WindowState` before every update there.
/// - On X11, winit sends no `Occluded` when the window is minimized and delivers
///   `RedrawRequested` at once, so neither source above fires. The runner also polls
///   `Window::is_minimized` there (at most every 100 ms) and ORs it into `occluded`.
/// - In `Reactive` mode an occluded window waits `max(wait, 33 ms)`.
/// - Pacing ([`FramePacer`]) is deadline based: the next update is due one period after the
///   previous one was due (no drift from late wake-ups), and an update late by more than one
///   period restarts the grid (no catch-up burst). Events that wake the loop before the
///   deadline are translated into messages at once and seen by the next update.
/// - When a frame is due but its presentation gate is still closed (a redraw is pending), the
///   runner waits up to a quarter of the period (at most 2 ms) for `RedrawRequested`, so on
///   Wayland a frame callback that lands just after the deadline is not missed (that would
///   show only every other frame when the limit equals the refresh rate).
/// - Display lock (ADR 0017): when the gate follows the compositor's frame callbacks (Wayland,
///   and the redraws really wait for the compositor) and [`FrameLimit`] is at or above 99 % of
///   the monitor's refresh rate (`MonitorHandle::refresh_rate_millihertz`, polled every
///   second), updates follow the display: each one runs as soon as its frame callback arrives
///   (not before half a refresh period after the previous deadline), and a closed gate is
///   waited for up to a refresh plus a quarter of one (at most 2 ms) past the deadline, or past
///   the redraw request when the update ended later (a compositor that skipped a refresh sends
///   the callback one refresh late). The lock needs two callbacks in a row to start, and it
///   stops while the median interval between frame callbacks is more than 10 % off the
///   reported refresh period (the window is on another monitor than winit says, or VRR). An
///   open gate that no update consumed (nothing presented) does not count as a callback: the
///   next update waits for its deadline. Without a callback by then the update runs
///   without presenting, as before; a hidden window falls to 30 Hz through the redraw
///   watchdog. So a limit of 143 no longer drifts against a 143.88 Hz panel, a skipped refresh
///   is one long frame instead of an update that is never shown, a limit above the refresh
///   rate no longer runs updates that cannot be shown, and each frame gets the most time
///   before the compositor's deadline.
/// - Idle steps (ADR 0022): in `Continuous` mode, when the app has `Idle` systems and an update
///   ends with time to wait, the runner opens an idle slot that ends `IDLE_MARGIN` before the
///   earlier of the deadline's spin margin and the wait's own wake-up (a refresh plus a quarter
///   when the gate is closed and updates follow the display, the earliest start when a frame
///   callback came early). The iteration of the update sets `ControlFlow::Poll` instead of its
///   wait, so winit flushes the connection and delivers a redraw that answers at once (X11, or a
///   Wayland window nothing presents to) before the first step, stamped as quick as before. Each
///   later iteration of the wait runs one step (`App::run_idle`, at most `IDLE_STEP`): `Poll`
///   again while the systems are `Ready`, a `WaitUntil` of at most `IDLE_POLL` while they are
///   `Waiting`, the plain wait once they are `Done` or the slot is over. Events, the gate and
///   the deadline are checked between steps, so an update is never held by one; an event that
///   arrives during a step (a frame callback more than 1 ms early) is handled when it returns.
///   `IdleInfo` reports the steps of each gap. Without `Idle` systems, unlimited, in `Reactive`
///   mode, or when the update ran late, nothing changes.
pub fn winit_runner(mut app: App, event_loop: EventLoop<()>) -> AppExit {
    let settings = app
        .world()
        .get_resource::<RunnerSettings>()
        .copied()
        .unwrap_or_default();
    app.world_mut().init_resource::<WindowState>();
    app.world_mut().init_resource::<FramePacingInfo>();
    app.world_mut().init_resource::<FrameDeadline>();
    app.world_mut().init_resource::<IdleInfo>();
    let mut handler = WinitApp::new(app, settings);
    let result = event_loop.run_app(&mut handler);
    // `exiting` normally did this already; it is a no-op then.
    handler.shutdown();
    match result {
        Ok(()) => handler.exit.unwrap_or_default(),
        Err(err) => {
            tracing::error!("the winit event loop failed: {err}");
            AppExit::error()
        }
    }
}

/// The `ApplicationHandler`: owns the app, the window and the [`RunnerCore`].
struct WinitApp {
    app: App,
    core: RunnerCore,
    window: Option<Arc<Window>>,
    /// The platform applies `request_inner_size` without a `Resized` event (Wayland).
    poll_size: bool,
    /// The platform reports no occlusion and no stalled redraw for a minimized window (X11).
    poll_minimized: bool,
    /// Set once the loop was asked to exit (by the app or by a fatal runner error).
    exit: Option<AppExit>,
    shut_down: bool,
}

impl WinitApp {
    fn new(app: App, settings: RunnerSettings) -> Self {
        Self {
            app,
            core: RunnerCore::new(settings),
            window: None,
            poll_size: false,
            poll_minimized: false,
            exit: None,
            shut_down: false,
        }
    }

    /// Finishes plugins once they are ready; true when the app may update.
    fn plugins_finished(&mut self) -> bool {
        match self.app.plugins_state() {
            PluginsState::Adding => false,
            PluginsState::Ready => {
                self.app.finish();
                self.app.cleanup();
                true
            }
            PluginsState::Finished => {
                self.app.cleanup();
                true
            }
            PluginsState::Cleaned => true,
        }
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) {
        let settings = self
            .app
            .world()
            .get_resource::<WindowSettings>()
            .cloned()
            .unwrap_or_default();
        let mut attributes = Window::default_attributes()
            .with_title(settings.title.clone())
            .with_inner_size(LogicalSize::new(settings.width, settings.height))
            .with_resizable(settings.resizable);
        if settings.mode == WindowMode::BorderlessFullscreen {
            attributes = attributes.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                tracing::error!("cannot create the window {:?}: {err}", settings.title);
                self.exit = Some(AppExit::error());
                event_loop.exit();
                return;
            }
        };
        let handle = window.window_handle().map(|handle| handle.as_raw());
        self.poll_size = matches!(handle, Ok(RawWindowHandle::Wayland(_)));
        self.poll_minimized = matches!(
            handle,
            Ok(RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_))
        );
        let size = size_of(window.inner_size());
        let scale_factor = window.scale_factor();
        tracing::debug!("window created: {}x{} scale={scale_factor}", size.x, size.y);
        self.app.insert_non_send(PrimaryWindow {
            window: window.clone(),
            id: window.id(),
        });
        self.core
            .window_created(self.app.world_mut(), size, scale_factor, Instant::now());
        // Wayland holds `RedrawRequested` until the compositor's frame callback.
        self.core.callback_gate = matches!(handle, Ok(RawWindowHandle::Wayland(_)));
        // Wayland delivers the first `RedrawRequested` after the initial configure on its own;
        // other platforms may need the request.
        window.request_redraw();
        self.window = Some(window);
    }

    /// `Shutdown` schedule, then drop the window, exactly once.
    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        self.app.shutdown();
        self.app.world_mut().remove_non_send::<PrimaryWindow>();
        self.window = None;
    }
}

impl ApplicationHandler for WinitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() && self.exit.is_none() {
            self.create_window(event_loop);
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window.as_ref().is_some_and(|w| w.id() == window_id) {
            self.core.window_event(self.app.world_mut(), event);
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if self.window.is_some() {
            self.core.device_event(self.app.world_mut(), &event);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit.is_some() {
            return;
        }
        if !self.plugins_finished() {
            event_loop.set_control_flow(wait_until(Instant::now(), PLUGINS_POLL));
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        loop {
            match self.core.pace(self.app.world(), Instant::now()) {
                FrameWait::Ready => break,
                // Then ask again: the frame may still wait for its presentation gate.
                FrameWait::Spin { until } => spin_until(until),
                FrameWait::Sleep { wake } => {
                    // Woken early (an event, an idle poll), or waiting for the gate: an idle
                    // step if the slot has room for one, then keep waiting.
                    let now = Instant::now();
                    let flow = match self.core.idle_step(now, wake) {
                        Some(until) => {
                            let work = self.app.run_idle(until);
                            self.core.after_idle(now, Instant::now(), work, wake)
                        }
                        None => ControlFlow::WaitUntil(wake),
                    };
                    event_loop.set_control_flow(flow);
                    return;
                }
            }
        }
        if self.poll_size {
            self.core
                .resized(self.app.world_mut(), size_of(window.inner_size()));
        }
        if let Some(cursor) = self.core.cursor_change(self.app.world()) {
            apply_cursor(&window, cursor);
        }
        let now = Instant::now();
        if self.poll_minimized {
            self.core.poll_minimized(now, || window.is_minimized());
        }
        self.core.poll_monitor(now, || {
            window
                .current_monitor()
                .and_then(|monitor| monitor.refresh_rate_millihertz())
        });
        self.core.begin_update(self.app.world_mut(), now);
        self.app.update();
        if let Some(exit) = self.app.should_exit() {
            tracing::debug!("exit requested: {exit:?}");
            self.exit = Some(exit);
            event_loop.exit();
            return;
        }
        self.core.idle_systems = self.app.has_idle_systems();
        let end = self.core.end_update(self.app.world(), Instant::now());
        if end.request_redraw {
            window.request_redraw();
        }
        event_loop.set_control_flow(end.control_flow);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.shutdown();
    }
}

/// What the runner does after an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EndOfUpdate {
    /// Call `Window::request_redraw` (the presentation gate is closed and nothing is pending).
    request_redraw: bool,
    /// Pacing until the next update.
    control_flow: ControlFlow,
}

/// Runner state that does not need the winit window or event loop: event translation, the
/// redraw protocol, the occlusion watchdog and frame pacing. Kept apart so it is testable
/// without a display.
#[derive(Debug)]
struct RunnerCore {
    settings: RunnerSettings,
    /// Keyboard focus as last reported by winit (gates raw mouse motion).
    focused: bool,
    /// Last `WindowEvent::Occluded` (never sent on Wayland and Windows).
    winit_occluded: bool,
    /// Last polled `Window::is_minimized` (X11 only, where minimizing sends no `Occluded`).
    minimized: bool,
    /// When `minimized` was last polled (`None`: never).
    minimized_polled_at: Option<Instant>,
    /// The published `WindowState::occluded`.
    occluded: bool,
    /// When the outstanding redraw was requested (`None`: nothing pending).
    redraw_pending_since: Option<Instant>,
    /// When the latest `RedrawRequested` opened the presentation gate.
    gate_opened_at: Option<Instant>,
    /// The cursor state last applied to the window (the OS default at creation).
    applied_cursor: CursorOptions,
    /// Frame deadlines ([`FrameLimit`]).
    pacer: FramePacer,
    /// The deadline of a due frame held for its presentation gate (see `gate_wait`).
    gate_hold: Option<Instant>,
    /// Refresh rate of the window's monitor in millihertz (`None`: unknown).
    refresh_millihertz: Option<u32>,
    /// When the monitor was last polled (`None`: never).
    monitor_polled_at: Option<Instant>,
    /// What the runner publishes as [`FramePacingInfo`] (its counters live here).
    pacing: FramePacingInfo,
    /// The platform's presentation gate follows the compositor's frame callbacks (Wayland).
    callback_gate: bool,
    /// The latest gate opening waited for the compositor (`QUICK_GATE` or more after its
    /// request).
    gate_was_callback: bool,
    /// Quick gate openings in a row, up to `QUICK_GATES_MAX` (where it starts: the gate does not
    /// follow the display until callbacks are seen).
    quick_gates: u8,
    /// Gate openings that waited for the compositor in a row (see `SLOW_GATES_TO_LOCK`).
    slow_gates: u8,
    /// The gate opened after the latest update started (cleared by `begin_update`): only such
    /// an opening is a frame callback the next update may run at.
    gate_fresh: bool,
    /// When the redraw answered by the latest gate opening was requested.
    gate_requested_at: Option<Instant>,
    /// Recent frame callback intervals.
    cadence: CallbackCadence,
    /// The callback cadence does not match the reported refresh rate (see
    /// `CADENCE_TOLERANCE_PER_MILLE`).
    cadence_mismatch: bool,
    /// When the current update started (`None`: none yet).
    update_started: Option<Instant>,
    /// How long the previous update took (for [`FrameDeadline`]).
    last_update: Duration,
    /// The app has `Idle` systems (refreshed after every update).
    idle_systems: bool,
    /// When the open idle slot ends (`None`: no slot).
    idle_end: Option<Instant>,
    /// What the `Idle` systems wanted after the latest step.
    idle_work: IdleWork,
    /// Idle steps of the current gap, published as [`IdleInfo`] by `begin_update`.
    idle: IdleInfo,
}

impl RunnerCore {
    fn new(settings: RunnerSettings) -> Self {
        Self {
            settings,
            focused: true,
            winit_occluded: false,
            minimized: false,
            minimized_polled_at: None,
            occluded: false,
            redraw_pending_since: None,
            gate_opened_at: None,
            applied_cursor: CursorOptions {
                grab: CursorGrab::None,
                visible: true,
            },
            pacer: FramePacer::new(),
            gate_hold: None,
            refresh_millihertz: None,
            monitor_polled_at: None,
            pacing: FramePacingInfo::default(),
            callback_gate: false,
            gate_was_callback: false,
            quick_gates: QUICK_GATES_MAX,
            slow_gates: 0,
            gate_fresh: false,
            gate_requested_at: None,
            cadence: CallbackCadence::default(),
            cadence_mismatch: false,
            update_started: None,
            last_update: Duration::ZERO,
            idle_systems: false,
            idle_end: None,
            idle_work: IdleWork::Done,
            idle: IdleInfo::default(),
        }
    }

    /// The period between updates: the display's while updates follow it
    /// ([`RunnerCore::display_period`]), else [`FrameLimit`] (`None` = unlimited or no
    /// resource), at least `OCCLUDED_WAIT` while the window is occluded.
    fn frame_period(&self, world: &World) -> Option<Duration> {
        if let Some(display) = self.display_period(world) {
            return Some(display);
        }
        let limit = world
            .get_resource::<FrameLimit>()
            .and_then(|limit| limit.period());
        if self.occluded {
            Some(limit.map_or(OCCLUDED_WAIT, |period| period.max(OCCLUDED_WAIT)))
        } else {
            limit
        }
    }

    /// The refresh period when updates follow the display (ADR 0017): the gate follows the
    /// compositor's frame callbacks, the window is visible, the refresh rate is known, the frame
    /// callbacks come at that rate and the [`FrameLimit`] is at or above 99 % of it. `None`
    /// otherwise (the limit's grid).
    fn display_period(&self, world: &World) -> Option<Duration> {
        if self.occluded
            || !self.callback_gate
            || self.quick_gates >= QUICK_GATES_MAX
            || self.cadence_mismatch
        {
            return None;
        }
        let limit = u64::from(world.get_resource::<FrameLimit>()?.max_fps);
        let refresh = u64::from(self.refresh_millihertz?);
        // fps * 1000 is millihertz; compare per mille.
        if limit > 0 && limit * 1_000_000 >= refresh * DISPLAY_MATCH_PER_MILLE {
            self.refresh_period()
        } else {
            None
        }
    }

    /// The reported refresh period (`None`: unknown).
    fn refresh_period(&self) -> Option<Duration> {
        let refresh = u64::from(self.refresh_millihertz?);
        (refresh > 0).then(|| Duration::from_nanos(1_000_000_000_000 / refresh))
    }

    /// The presentation gate is closed and its redraw is pending (a renderer presented and the
    /// runner asked for the next frame callback).
    fn gate_closed(&self, world: &World) -> bool {
        !self.occluded && self.redraw_pending_since.is_some() && !frame_ready(world)
    }

    /// Before an update: whether the frame deadline allows it at `now` (see
    /// [`RunnerCore::plan`]). Remembers the deadline of a frame held for its gate.
    fn pace(&mut self, world: &World, now: Instant) -> FrameWait {
        let (wait, held) = self.plan(world, now);
        // `Ready` without a hold ends an earlier hold; a plain deadline wait drops it (the limit
        // changed while the frame waited for its gate).
        if held.is_some() || wait != FrameWait::Ready {
            self.gate_hold = held;
        }
        wait
    }

    /// What to do at `now` before the next update, and the deadline of a frame held for its
    /// presentation gate (with `Sleep` only).
    ///
    /// On Wayland the gate opens with the compositor's frame callback, which comes on the
    /// compositor's clock. With a limit below the refresh rate the time grid governs: a due
    /// frame with a closed gate waits up to [`RunnerCore::gate_window`] for the callback, and
    /// `begin_update` restarts the grid at the callback when it came in that window, so a
    /// callback just after the deadline is not missed (measured on a 144 Hz panel at 144 fps
    /// with `Mailbox`: without it 113 to 126 frames shown per second, with it 136 to 143).
    ///
    /// When updates follow the display ([`RunnerCore::display_period`]) the frame callback is
    /// the clock: an update runs as soon as a frame callback opens its gate, from half a period
    /// before its deadline on ([`display_window`]; the deadlines use the refresh period), and
    /// a closed gate is waited for up to a refresh plus [`gate_window`] past the deadline, or
    /// past the redraw request if the previous update ended later, since a compositor that
    /// skipped a refresh (for example because the GPU finished the previous frame late) sends
    /// the callback one refresh late and an update run in the meantime would not be shown. A
    /// gate that opened before the latest update started and was not consumed (nothing
    /// presented: no camera, a menu, a surface being rebuilt) is no callback: the update waits
    /// for its deadline, so the rate stays at the refresh rate. There is no spin: the callback
    /// wakes the loop. Starting right at the
    /// callback gives the frame the most time before the compositor's next deadline (measured:
    /// about a third fewer refreshes skipped than a regular grid at the refresh period), at the
    /// cost of frame times that follow the callbacks' jitter (0.25 ms standard deviation).
    ///
    /// No wait while unlimited, occluded, or when no redraw is pending (no renderer, or X11,
    /// where the redraw arrives at once).
    fn plan(&self, world: &World, now: Instant) -> (FrameWait, Option<Instant>) {
        let period = self.frame_period(world);
        let (Some(period), Some(due)) = (period, self.pacer.next_frame(period)) else {
            return (FrameWait::Ready, None);
        };
        let hold = |until: Option<Instant>| match until {
            Some(until) if now < until => (FrameWait::Sleep { wake: until }, Some(due)),
            _ => (FrameWait::Ready, None),
        };
        if self.display_period(world).is_some() {
            let (earliest, latest) = display_window(due, period, self.redraw_pending_since);
            if self.gate_closed(world) {
                return hold(latest);
            }
            if self.gate_was_callback && self.gate_fresh && frame_ready(world) {
                return match earliest {
                    Some(earliest) if now < earliest => (FrameWait::Sleep { wake: earliest }, None),
                    _ => (FrameWait::Ready, None),
                };
            }
            return (self.pacer.wait(now, Some(period)), None);
        }
        match self.pacer.wait(now, Some(period)) {
            FrameWait::Ready if self.gate_closed(world) => {
                hold(due.checked_add(gate_window(period)))
            }
            wait => (wait, None),
        }
    }

    /// The window exists: fill `WindowState`, write `WindowCreated`, start the redraw protocol
    /// (the first redraw counts as pending from `now`).
    fn window_created(&mut self, world: &mut World, size: UVec2, scale_factor: f64, now: Instant) {
        self.focused = true;
        self.winit_occluded = false;
        self.minimized = false;
        self.minimized_polled_at = None;
        self.occluded = false;
        self.redraw_pending_since = Some(now);
        update_state(world, |state| {
            *state = WindowState {
                physical_size: size,
                scale_factor,
                focused: true,
                occluded: false,
                frame_ready: false,
            };
        });
        write(world, WindowCreated);
    }

    fn window_event(&mut self, world: &mut World, event: WindowEvent) {
        match event {
            WindowEvent::Resized(size) => self.resized(world, size_of(size)),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                update_state(world, |state| state.scale_factor = scale_factor);
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                update_state(world, |state| state.focused = focused);
                write(world, WindowFocused(focused));
            }
            WindowEvent::Occluded(occluded) => self.winit_occluded = occluded,
            WindowEvent::CloseRequested => {
                write(world, WindowCloseRequested);
                if self.settings.exit_on_close {
                    write(world, AppExit::Success);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(message) = key_message(event.physical_key, event.state, event.repeat) {
                    write(world, message);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                write(
                    world,
                    MouseButtonInput {
                        button,
                        state: button_state(state),
                    },
                );
            }
            WindowEvent::MouseWheel { delta, .. } => {
                write(
                    world,
                    MouseWheel {
                        delta: wheel_delta(delta),
                    },
                );
            }
            WindowEvent::CursorMoved { position, .. } => {
                write(
                    world,
                    CursorMoved {
                        position: Vec2::new(position.x as f32, position.y as f32),
                    },
                );
            }
            WindowEvent::RedrawRequested => self.gate_opened(world, Instant::now()),
            _ => {}
        }
    }

    /// `RedrawRequested` at `now`: opens the presentation gate and notes whether the opening
    /// waited for the compositor (a frame callback) or came at once. The first opening after
    /// the window was created comes with the platform's first configure, not after a present:
    /// it says nothing either way.
    ///
    /// A callback that answers a redraw requested less than half a refresh after the previous
    /// callback came one refresh after it (the commit made that refresh), so their interval is
    /// a sample of the compositor's real cadence (`CallbackCadence`). Later requests are
    /// skipped: with a low limit the interval would measure the limit, not the display.
    fn gate_opened(&mut self, world: &mut World, now: Instant) {
        let requested = self.redraw_pending_since.take();
        if let Some(since) = requested
            && let Some(previous) = self.gate_opened_at
        {
            let callback = now.saturating_duration_since(since) >= QUICK_GATE;
            if callback
                && self.gate_was_callback
                && let Some(refresh) = self.refresh_period()
                && since.saturating_duration_since(previous) <= refresh / 2
            {
                self.cadence.push(now.saturating_duration_since(previous));
                self.cadence_mismatch = self.cadence.median().is_some_and(|median| {
                    median.abs_diff(refresh) * 1000 > refresh * CADENCE_TOLERANCE_PER_MILLE
                });
            }
            self.gate_was_callback = callback;
            if callback {
                self.slow_gates = self.slow_gates.saturating_add(1);
                if self.slow_gates >= SLOW_GATES_TO_LOCK {
                    self.quick_gates = 0;
                }
            } else {
                self.slow_gates = 0;
                self.quick_gates = self.quick_gates.saturating_add(1).min(QUICK_GATES_MAX);
            }
        }
        self.gate_requested_at = requested;
        self.gate_opened_at = Some(now);
        self.gate_fresh = true;
        update_state(world, |state| state.frame_ready = true);
    }

    fn device_event(&mut self, world: &mut World, event: &DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (x, y) } = *event
            && self.focused
        {
            write(
                world,
                MouseMotion {
                    delta: Vec2::new(x as f32, y as f32),
                },
            );
        }
    }

    /// A new surface size: updates `WindowState` and writes `WindowResized` if it changed.
    /// 0x0 (minimized on some platforms) is ignored: the last real size stays.
    fn resized(&mut self, world: &mut World, size: UVec2) {
        if size.x == 0 || size.y == 0 {
            return;
        }
        let Some(mut state) = world.get_resource_mut::<WindowState>() else {
            return;
        };
        if state.physical_size == size {
            return;
        }
        state.physical_size = size;
        write(
            world,
            WindowResized {
                width: size.x,
                height: size.y,
            },
        );
    }

    /// The desired cursor state if it differs from the one last applied (and marks it applied).
    fn cursor_change(&mut self, world: &World) -> Option<CursorOptions> {
        let wanted = *world.get_resource::<CursorOptions>()?;
        (wanted != self.applied_cursor).then(|| {
            self.applied_cursor = wanted;
            wanted
        })
    }

    /// Refreshes `minimized` from `is_minimized` (`None`: unknown, counts as not minimized),
    /// at most every `MINIMIZED_POLL`.
    fn poll_minimized(&mut self, now: Instant, is_minimized: impl FnOnce() -> Option<bool>) {
        let due = self
            .minimized_polled_at
            .is_none_or(|at| now.saturating_duration_since(at) >= MINIMIZED_POLL);
        if due {
            self.minimized_polled_at = Some(now);
            self.minimized = is_minimized().unwrap_or(false);
        }
    }

    /// Refreshes the monitor's refresh rate (`None`: unknown) at most every `MONITOR_POLL`
    /// where the gate follows frame callbacks, and once elsewhere (only the display lock uses
    /// it, and the query is not free: a round trip on macOS). A new rate drops the callback
    /// cadence measured for the old one.
    fn poll_monitor(&mut self, now: Instant, refresh_millihertz: impl FnOnce() -> Option<u32>) {
        let due = self.monitor_polled_at.is_none_or(|at| {
            self.callback_gate && now.saturating_duration_since(at) >= MONITOR_POLL
        });
        if due {
            self.monitor_polled_at = Some(now);
            let refresh = refresh_millihertz().filter(|&mhz| mhz > 0);
            if refresh != self.refresh_millihertz {
                tracing::debug!("display refresh rate: {refresh:?} mHz");
                self.refresh_millihertz = refresh;
                self.cadence = CallbackCadence::default();
                self.cadence_mismatch = false;
            }
        }
    }

    /// Before an update: close the idle slot, publish occlusion (winit's report, the polled
    /// minimized state or the redraw watchdog), [`FramePacingInfo`], [`FrameDeadline`] and
    /// [`IdleInfo`].
    fn begin_update(&mut self, world: &mut World, now: Instant) {
        self.idle_end = None;
        self.idle_work = IdleWork::Done;
        let idle = self.idle.take_gap();
        if let Some(mut info) = world.get_resource_mut::<IdleInfo>() {
            *info = idle;
        }
        let stalled = self
            .redraw_pending_since
            .is_some_and(|since| now.saturating_duration_since(since) > REDRAW_TIMEOUT);
        let occluded = self.winit_occluded || self.minimized || stalled;
        if occluded != self.occluded {
            self.occluded = occluded;
            tracing::debug!(
                "window occluded: {occluded} (minimized: {}, redraw stalled: {stalled})",
                self.minimized
            );
            update_state(world, |state| state.occluded = occluded);
            write(world, WindowOccluded(occluded));
        }
        let period = self.frame_period(world);
        let display = self.display_period(world).is_some();
        let due = self.pacer.next_frame(period);
        let fresh = std::mem::take(&mut self.gate_fresh);
        self.publish_pacing(world, now, display);
        self.pacer.start_frame(now, period);
        // Phase lock (see `plan`): the grid restarts at the frame callback that opened this
        // update's gate since the previous update. Following the display, a callback inside the
        // display window; else one that came after the deadline, within the wait window (this
        // update waited for it).
        if let (Some(due), Some(period), Some(opened)) = (due, period, self.gate_opened_at)
            && fresh
            && frame_ready(world)
        {
            let lock = if display {
                let (earliest, latest) = display_window(due, period, self.gate_requested_at);
                self.gate_was_callback
                    && earliest.is_none_or(|earliest| opened >= earliest)
                    && latest.is_some_and(|latest| opened <= latest)
            } else {
                opened > due && opened - due <= gate_window(period)
            };
            if lock {
                self.pacer.reanchor(opened);
            }
        }
        self.update_started = Some(now);
        if let Some(mut deadline) = world.get_resource_mut::<FrameDeadline>() {
            *deadline = FrameDeadline::after_start(&self.pacer, period, self.last_update);
        }
    }

    /// Fills and publishes [`FramePacingInfo`] for the update that starts at `now`.
    fn publish_pacing(&mut self, world: &mut World, now: Instant, display_locked: bool) {
        let (gate_wait, gate_missed) = match self.gate_hold.take() {
            Some(due) => (now.saturating_duration_since(due), !frame_ready(world)),
            None => (Duration::ZERO, false),
        };
        let p = &mut self.pacing;
        p.gate_wait = gate_wait;
        p.gate_missed = gate_missed;
        p.gate_waits += u64::from(!gate_wait.is_zero());
        p.gate_misses += u64::from(gate_missed);
        p.gate_opened_at = self.gate_opened_at;
        p.refresh_millihertz = self.refresh_millihertz;
        p.display_locked = display_locked;
        if let Some(mut info) = world.get_resource_mut::<FramePacingInfo>() {
            *info = *p;
        }
    }

    /// After an update: request the next redraw if the presentation gate was consumed, and pick
    /// the pacing (with the [`FrameLimit`] of now: a change made during the update paces the
    /// next one). A wait with room for idle steps opens an idle slot and polls instead
    /// ([`RunnerCore::open_idle`]).
    fn end_update(&mut self, world: &World, now: Instant) -> EndOfUpdate {
        if let Some(started) = self.update_started.take() {
            self.last_update = now.saturating_duration_since(started);
        }
        let request_redraw = !frame_ready(world) && self.redraw_pending_since.is_none();
        if request_redraw {
            self.redraw_pending_since = Some(now);
        }
        let (pace, held) = self.plan(world, now);
        if held.is_some() {
            // The loop may sleep through the whole wait without another `pace`.
            self.gate_hold = held;
        }
        self.idle_end = None;
        self.idle_work = IdleWork::Done;
        let control_flow = match self.settings.update_mode {
            UpdateMode::Continuous => match pace {
                FrameWait::Sleep { wake } if self.open_idle(world, now, wake) => ControlFlow::Poll,
                FrameWait::Sleep { wake } => ControlFlow::WaitUntil(wake),
                FrameWait::Ready | FrameWait::Spin { .. } => ControlFlow::Poll,
            },
            UpdateMode::Reactive { wait } => {
                let wait = if self.occluded {
                    wait.max(OCCLUDED_WAIT)
                } else {
                    wait
                };
                match (wait_until(now, wait), pace) {
                    (ControlFlow::WaitUntil(timeout), FrameWait::Sleep { wake }) => {
                        ControlFlow::WaitUntil(timeout.max(wake))
                    }
                    (control_flow, _) => control_flow,
                }
            }
        };
        EndOfUpdate {
            request_redraw,
            control_flow,
        }
    }

    /// Opens an idle slot after an update that ended at `now` and waits until `wake`, if the app
    /// has `Idle` systems, a [`FrameLimit`] (the 33 ms wait of a hidden window alone is no
    /// limit) and the slot has room for a step: it ends `IDLE_MARGIN` before the earlier of the
    /// next deadline's spin margin and `wake`. Following the display, `wake` is the latest wait
    /// for a closed gate (later than the deadline, so the deadline decides) or the earliest
    /// start for a callback that came early.
    fn open_idle(&mut self, world: &World, now: Instant, wake: Instant) -> bool {
        let limited = world
            .get_resource::<FrameLimit>()
            .is_some_and(|limit| limit.period().is_some());
        if !self.idle_systems || !limited {
            return false;
        }
        let end = self
            .pacer
            .idle_end(self.frame_period(world))
            .zip(wake.checked_sub(IDLE_MARGIN))
            .map(|(due, wake)| due.min(wake));
        let room = |end: Instant| now.checked_add(MIN_IDLE_STEP).is_some_and(|t| t <= end);
        let Some(end) = end.filter(|&end| room(end)) else {
            return false;
        };
        self.idle_end = Some(end);
        self.idle_work = IdleWork::Ready;
        true
    }

    /// The end of the idle slot for a wait until `wake` (it may have moved since the slot
    /// opened: a frame callback that came early): `None` without a slot.
    fn idle_slot_end(&self, wake: Instant) -> Option<Instant> {
        Some(self.idle_end?.min(wake.checked_sub(IDLE_MARGIN)?))
    }

    /// The end of an idle step at `now` while the loop waits until `wake` (`None`: no slot,
    /// the systems are done, or too little of the slot is left).
    fn idle_step(&self, now: Instant, wake: Instant) -> Option<Instant> {
        if self.idle_work == IdleWork::Done {
            return None;
        }
        idle_step_end(now, self.idle_slot_end(wake)?)
    }

    /// After an idle step that ran from `started` to `now`: records it and picks the control
    /// flow: `Poll` for the next step at once, a short wait while the systems wait for work in
    /// flight (at most `IDLE_POLL`, never past the slot or `wake`), the plain wait once they
    /// are done.
    fn after_idle(
        &mut self,
        started: Instant,
        now: Instant,
        work: IdleWork,
        wake: Instant,
    ) -> ControlFlow {
        let end = self.idle_slot_end(wake).unwrap_or(now);
        self.idle.record(started, now, end);
        self.idle_work = work;
        match work {
            IdleWork::Ready => ControlFlow::Poll,
            IdleWork::Waiting => {
                let poll = now.checked_add(IDLE_POLL).map_or(end, |poll| poll.min(end));
                ControlFlow::WaitUntil(if poll > now { poll } else { wake })
            }
            IdleWork::Done => {
                self.idle_end = None;
                ControlFlow::WaitUntil(wake)
            }
        }
    }
}

/// How long a due frame may wait for its presentation gate on the limit's grid: a quarter of
/// the period, at most `GATE_WAIT_MAX`.
fn gate_window(period: Duration) -> Duration {
    (period / 4).min(GATE_WAIT_MAX)
}

/// While updates follow the display: the earliest an update may run once its frame callback
/// came (half a period before its deadline) and the latest it waits for one: a refresh plus
/// [`gate_window`] (1.25 refresh at 144 Hz, 1.12 at 60 Hz) after the deadline, or after the
/// redraw request (`requested`) when the previous update ended past its deadline, since the
/// callback comes about a refresh after the present. `None` where an instant is not
/// representable.
fn display_window(
    due: Instant,
    period: Duration,
    requested: Option<Instant>,
) -> (Option<Instant>, Option<Instant>) {
    let from = requested.map_or(due, |requested| requested.max(due));
    (
        due.checked_sub(period / 2),
        from.checked_add(period + gate_window(period)),
    )
}

/// The last `CADENCE_SAMPLES` intervals between frame callbacks (see `RunnerCore::gate_opened`).
#[derive(Clone, Copy, Debug, Default)]
struct CallbackCadence {
    samples: [Duration; CADENCE_SAMPLES],
    len: usize,
    next: usize,
}

impl CallbackCadence {
    fn push(&mut self, interval: Duration) {
        self.samples[self.next] = interval;
        self.next = (self.next + 1) % CADENCE_SAMPLES;
        self.len = (self.len + 1).min(CADENCE_SAMPLES);
    }

    /// The median interval (`None`: fewer than `CADENCE_MIN_SAMPLES`). A skipped refresh now
    /// and then (a doubled interval) does not move it.
    fn median(&self) -> Option<Duration> {
        if self.len < CADENCE_MIN_SAMPLES {
            return None;
        }
        let mut samples = self.samples;
        let samples = &mut samples[..self.len];
        samples.sort_unstable();
        Some(samples[self.len / 2])
    }
}

/// The presentation gate (`WindowState::frame_ready`) is open.
fn frame_ready(world: &World) -> bool {
    world
        .get_resource::<WindowState>()
        .is_some_and(|state| state.frame_ready)
}

/// `WaitUntil(now + wait)`, or `Wait` if that instant is not representable.
fn wait_until(now: Instant, wait: Duration) -> ControlFlow {
    now.checked_add(wait)
        .map_or(ControlFlow::Wait, ControlFlow::WaitUntil)
}

/// Applies a cursor state: `Locked` falls back to `Confined` (X11 has no pointer lock).
fn apply_cursor(window: &Window, options: CursorOptions) {
    let grab = match options.grab {
        CursorGrab::None => window.set_cursor_grab(CursorGrabMode::None),
        CursorGrab::Locked => window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined)),
    };
    if let Err(err) = grab {
        tracing::warn!("cannot set the cursor grab to {:?}: {err}", options.grab);
    }
    window.set_cursor_visible(options.visible);
}

/// Writes `message` if its `Messages<M>` resource exists (the plugin that owns it was added).
fn write<M: Message>(world: &mut World, message: M) {
    if let Some(mut messages) = world.get_resource_mut::<Messages<M>>() {
        messages.write(message);
    }
}

/// Edits `WindowState` (if present), marking it changed only if a field changed.
fn update_state(world: &mut World, edit: impl FnOnce(&mut WindowState)) {
    if let Some(mut state) = world.get_resource_mut::<WindowState>() {
        let mut next = *state;
        edit(&mut next);
        state.set_if_neq(next);
    }
}

fn size_of(size: PhysicalSize<u32>) -> UVec2 {
    UVec2::new(size.width, size.height)
}

fn button_state(state: ElementState) -> ButtonState {
    match state {
        ElementState::Pressed => ButtonState::Pressed,
        ElementState::Released => ButtonState::Released,
    }
}

/// Keyboard message for a physical key (keys winit cannot identify are dropped).
fn key_message(key: PhysicalKey, state: ElementState, repeat: bool) -> Option<KeyboardInput> {
    let PhysicalKey::Code(key) = key else {
        return None;
    };
    Some(KeyboardInput {
        key,
        state: button_state(state),
        repeat,
    })
}

/// Scroll in lines: line deltas as is, pixel deltas divided by 32.
fn wheel_delta(delta: MouseScrollDelta) -> Vec2 {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => Vec2::new(x, y),
        MouseScrollDelta::PixelDelta(p) => Vec2::new(
            (p.x / PIXELS_PER_LINE) as f32,
            (p.y / PIXELS_PER_LINE) as f32,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ButtonInput, InputPlugin, MouseButton};
    use winit::dpi::PhysicalPosition;
    use winit::event::TouchPhase;
    use winit::keyboard::{KeyCode, NativeKeyCode};

    /// An app with the input plugin and the window messages/resources `WindowPlugin` would
    /// register (the plugin itself needs a display).
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(InputPlugin)
            .insert_resource(WindowState::default())
            .init_resource::<FramePacingInfo>()
            .init_resource::<FrameDeadline>()
            .init_resource::<IdleInfo>()
            .insert_resource(CursorOptions {
                grab: CursorGrab::None,
                visible: true,
            })
            .add_message::<WindowCreated>()
            .add_message::<WindowResized>()
            .add_message::<WindowCloseRequested>()
            .add_message::<WindowFocused>()
            .add_message::<WindowOccluded>();
        app
    }

    fn core() -> RunnerCore {
        RunnerCore::new(RunnerSettings::default())
    }

    fn messages<M: Message + Clone>(world: &World) -> Vec<M> {
        let messages = world.resource::<Messages<M>>();
        messages.get_cursor().read(messages).cloned().collect()
    }

    fn state(world: &World) -> WindowState {
        *world.resource::<WindowState>()
    }

    fn created(core: &mut RunnerCore, world: &mut World, now: Instant) {
        core.window_created(world, UVec2::new(800, 600), 1.5, now);
    }

    #[test]
    fn window_created_fills_state_and_writes_the_message() {
        let mut app = app();
        let world = app.world_mut();
        let mut core = core();
        created(&mut core, world, Instant::now());
        assert_eq!(
            state(world),
            WindowState {
                physical_size: UVec2::new(800, 600),
                scale_factor: 1.5,
                focused: true,
                occluded: false,
                frame_ready: false,
            }
        );
        assert_eq!(messages::<WindowCreated>(world).len(), 1);
        assert!(core.redraw_pending_since.is_some());
    }

    #[test]
    fn resized_writes_only_real_changes() {
        let mut app = app();
        let world = app.world_mut();
        let mut core = core();
        created(&mut core, world, Instant::now());
        core.window_event(world, WindowEvent::Resized(PhysicalSize::new(800, 600)));
        core.window_event(world, WindowEvent::Resized(PhysicalSize::new(0, 0)));
        core.window_event(world, WindowEvent::Resized(PhysicalSize::new(0, 300)));
        assert!(messages::<WindowResized>(world).is_empty());
        assert_eq!(state(world).physical_size, UVec2::new(800, 600));

        core.window_event(world, WindowEvent::Resized(PhysicalSize::new(900, 650)));
        assert_eq!(
            messages::<WindowResized>(world),
            vec![WindowResized {
                width: 900,
                height: 650
            }]
        );
        assert_eq!(state(world).physical_size, UVec2::new(900, 650));
        // The Wayland size poll goes through the same path.
        core.resized(world, UVec2::new(900, 650));
        core.resized(world, UVec2::new(1000, 700));
        assert_eq!(messages::<WindowResized>(world).len(), 2);
    }

    #[test]
    fn close_requested_exits_only_when_configured() {
        for exit_on_close in [true, false] {
            let mut app = app();
            let mut core = RunnerCore::new(RunnerSettings {
                exit_on_close,
                ..RunnerSettings::default()
            });
            core.window_event(app.world_mut(), WindowEvent::CloseRequested);
            assert_eq!(messages::<WindowCloseRequested>(app.world()).len(), 1);
            let expected = exit_on_close.then_some(AppExit::Success);
            assert_eq!(app.should_exit(), expected);
        }
    }

    #[test]
    fn focus_updates_state_and_gates_mouse_motion() {
        let mut app = app();
        let world = app.world_mut();
        let mut core = core();
        created(&mut core, world, Instant::now());
        let motion = DeviceEvent::MouseMotion { delta: (3.0, -2.0) };
        core.device_event(world, &motion);
        core.window_event(world, WindowEvent::Focused(false));
        core.device_event(world, &motion);
        assert!(!state(world).focused);
        assert_eq!(messages::<WindowFocused>(world).len(), 1);
        assert_eq!(
            messages::<MouseMotion>(world),
            vec![MouseMotion {
                delta: Vec2::new(3.0, -2.0)
            }]
        );
        core.window_event(world, WindowEvent::Focused(true));
        core.device_event(world, &motion);
        assert!(state(world).focused);
        assert_eq!(messages::<MouseMotion>(world).len(), 2);
    }

    #[test]
    fn pointer_events_become_messages() {
        let mut app = app();
        let world = app.world_mut();
        let mut core = core();
        let device_id = DeviceId::dummy();
        core.window_event(
            world,
            WindowEvent::MouseInput {
                device_id,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        core.window_event(
            world,
            WindowEvent::MouseWheel {
                device_id,
                delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(64.0, -16.0)),
                phase: TouchPhase::Moved,
            },
        );
        core.window_event(
            world,
            WindowEvent::CursorMoved {
                device_id,
                position: PhysicalPosition::new(10.5, 20.0),
            },
        );
        assert_eq!(
            messages::<MouseButtonInput>(world),
            vec![MouseButtonInput {
                button: MouseButton::Left,
                state: ButtonState::Pressed
            }]
        );
        assert_eq!(
            messages::<MouseWheel>(world),
            vec![MouseWheel {
                delta: Vec2::new(2.0, -0.5)
            }]
        );
        assert_eq!(
            messages::<CursorMoved>(world),
            vec![CursorMoved {
                position: Vec2::new(10.5, 20.0)
            }]
        );
    }

    #[test]
    fn unregistered_messages_are_skipped() {
        let mut world = World::new();
        world.insert_resource(WindowState::default());
        let mut core = core();
        core.window_event(&mut world, WindowEvent::CloseRequested);
        core.window_event(&mut world, WindowEvent::Focused(false));
        core.device_event(&mut world, &DeviceEvent::MouseMotion { delta: (1.0, 1.0) });
        assert!(!state(&world).focused);
        assert!(!world.contains_resource::<Messages<AppExit>>());
    }

    #[test]
    fn redraw_protocol_requests_one_redraw_per_consumed_frame() {
        let mut app = app();
        let world = app.world_mut();
        let mut core = core();
        let t0 = Instant::now();
        created(&mut core, world, t0);
        // First redraw pending: nothing new is requested.
        assert!(!core.end_update(world, t0).request_redraw);

        core.window_event(world, WindowEvent::RedrawRequested);
        assert!(state(world).frame_ready);
        assert!(core.redraw_pending_since.is_none());
        // Gate open and not consumed (no renderer): no request.
        assert!(!core.end_update(world, t0).request_redraw);

        // A renderer presented and closed the gate: exactly one request.
        world.resource_mut::<WindowState>().frame_ready = false;
        assert!(core.end_update(world, t0).request_redraw);
        assert!(!core.end_update(world, t0).request_redraw);
        assert_eq!(core.redraw_pending_since, Some(t0));
    }

    #[test]
    fn stalled_redraw_marks_the_window_occluded() {
        let mut app = app();
        let world = app.world_mut();
        let mut core = core();
        let t0 = Instant::now();
        created(&mut core, world, t0);
        core.begin_update(world, t0 + Duration::from_millis(100));
        assert!(!state(world).occluded);

        let late = t0 + Duration::from_millis(101);
        core.begin_update(world, late);
        assert!(state(world).occluded);
        let end = core.end_update(world, late);
        assert!(!end.request_redraw);
        // 30 Hz while occluded: the deadline minus the spin margin.
        assert_eq!(
            end.control_flow,
            ControlFlow::WaitUntil(late + OCCLUDED_WAIT - FramePacer::DEFAULT_SPIN_MARGIN)
        );

        core.window_event(world, WindowEvent::RedrawRequested);
        core.begin_update(world, late);
        assert!(!state(world).occluded);
        let changes: Vec<bool> = messages::<WindowOccluded>(world)
            .iter()
            .map(|m| m.0)
            .collect();
        assert_eq!(changes, vec![true, false]);
        assert_eq!(core.end_update(world, late).control_flow, ControlFlow::Poll);
    }

    #[test]
    fn winit_occlusion_is_published() {
        let mut app = app();
        let world = app.world_mut();
        let mut core = core();
        let now = Instant::now();
        created(&mut core, world, now);
        core.window_event(world, WindowEvent::RedrawRequested);
        core.window_event(world, WindowEvent::Occluded(true));
        core.begin_update(world, now);
        assert!(state(world).occluded);
        core.window_event(world, WindowEvent::Occluded(false));
        core.begin_update(world, now);
        assert!(!state(world).occluded);
        assert_eq!(messages::<WindowOccluded>(world).len(), 2);
    }

    #[test]
    fn polled_minimized_state_marks_the_window_occluded() {
        let mut app = app();
        let world = app.world_mut();
        let mut core = core();
        let t0 = Instant::now();
        created(&mut core, world, t0);
        // X11: redraws keep arriving while minimized, so the watchdog never fires.
        core.window_event(world, WindowEvent::RedrawRequested);
        let mut polls = 0;
        core.poll_minimized(t0, || {
            polls += 1;
            Some(true)
        });
        core.begin_update(world, t0);
        assert!(state(world).occluded);
        let end = core.end_update(world, t0);
        assert_eq!(
            end.control_flow,
            ControlFlow::WaitUntil(t0 + OCCLUDED_WAIT - FramePacer::DEFAULT_SPIN_MARGIN)
        );

        // Rate limited: no new query before `MINIMIZED_POLL`.
        let early = t0 + MINIMIZED_POLL - Duration::from_millis(1);
        core.poll_minimized(early, || {
            polls += 1;
            Some(false)
        });
        core.begin_update(world, early);
        assert!(state(world).occluded);

        // Restored. `None` (unknown) counts as not minimized.
        let later = t0 + MINIMIZED_POLL;
        core.poll_minimized(later, || {
            polls += 1;
            None
        });
        core.begin_update(world, later);
        assert!(!state(world).occluded);
        assert_eq!(polls, 2);
        let changes: Vec<bool> = messages::<WindowOccluded>(world)
            .iter()
            .map(|m| m.0)
            .collect();
        assert_eq!(changes, vec![true, false]);
    }

    #[test]
    fn control_flow_follows_the_update_mode() {
        let world = World::new();
        let now = Instant::now();
        let wait = Duration::from_millis(50);
        let mut reactive = RunnerCore::new(RunnerSettings {
            update_mode: UpdateMode::Reactive { wait },
            ..RunnerSettings::default()
        });
        assert_eq!(
            reactive.end_update(&world, now).control_flow,
            ControlFlow::WaitUntil(now + wait)
        );
        // Occluded: never faster than 30 Hz, never slower than asked.
        reactive.occluded = true;
        assert_eq!(
            reactive.end_update(&world, now).control_flow,
            ControlFlow::WaitUntil(now + wait)
        );
        let mut fast = RunnerCore::new(RunnerSettings {
            update_mode: UpdateMode::Reactive {
                wait: Duration::from_millis(5),
            },
            ..RunnerSettings::default()
        });
        fast.occluded = true;
        assert_eq!(
            fast.end_update(&world, now).control_flow,
            ControlFlow::WaitUntil(now + OCCLUDED_WAIT)
        );
        let mut forever = RunnerCore::new(RunnerSettings {
            update_mode: UpdateMode::Reactive {
                wait: Duration::MAX,
            },
            ..RunnerSettings::default()
        });
        assert_eq!(
            forever.end_update(&world, now).control_flow,
            ControlFlow::Wait
        );
        assert_eq!(
            core().end_update(&world, now).control_flow,
            ControlFlow::Poll
        );
    }

    /// A visible window with a presented first frame and the given limit.
    fn paced(max_fps: u32) -> (App, RunnerCore, Instant) {
        let mut app = app();
        app.insert_resource(FrameLimit::new(max_fps));
        let mut core = core();
        let t0 = Instant::now();
        created(&mut core, app.world_mut(), t0);
        core.window_event(app.world_mut(), WindowEvent::RedrawRequested);
        (app, core, t0)
    }

    #[test]
    fn frame_limit_paces_continuous_updates() {
        let (mut app, mut core, t0) = paced(100);
        let ms = Duration::from_millis;
        let margin = FramePacer::DEFAULT_SPIN_MARGIN;
        assert_eq!(core.pace(app.world(), t0), FrameWait::Ready, "first frame");
        core.begin_update(app.world_mut(), t0);
        let end = core.end_update(app.world(), t0 + ms(2));
        assert_eq!(
            end.control_flow,
            ControlFlow::WaitUntil(t0 + ms(10) - margin)
        );
        // An event wakes the loop early: keep waiting, then spin, then run.
        assert_eq!(
            core.pace(app.world(), t0 + ms(5)),
            FrameWait::Sleep {
                wake: t0 + ms(10) - margin
            }
        );
        assert_eq!(
            core.pace(app.world(), t0 + ms(10) - margin / 2),
            FrameWait::Spin { until: t0 + ms(10) }
        );
        assert_eq!(core.pace(app.world(), t0 + ms(10)), FrameWait::Ready);

        // Deadlines accumulate: a frame started 1 ms late is still due 10 ms after the first.
        core.begin_update(app.world_mut(), t0 + ms(11));
        let end = core.end_update(app.world(), t0 + ms(13));
        assert_eq!(
            end.control_flow,
            ControlFlow::WaitUntil(t0 + ms(20) - margin)
        );

        // A limit changed during the update paces the next frame.
        app.world_mut().resource_mut::<FrameLimit>().max_fps = 50;
        let end = core.end_update(app.world(), t0 + ms(13));
        assert_eq!(
            end.control_flow,
            ControlFlow::WaitUntil(t0 + ms(30) - margin)
        );

        // 0 = unlimited: poll.
        app.insert_resource(FrameLimit::UNLIMITED);
        assert_eq!(
            core.end_update(app.world(), t0 + ms(13)).control_flow,
            ControlFlow::Poll
        );
        assert_eq!(core.pace(app.world(), t0 + ms(13)), FrameWait::Ready);
    }

    #[test]
    fn a_due_frame_waits_briefly_for_the_presentation_gate() {
        let ms = Duration::from_millis;
        let period = Duration::from_nanos(6_944_444);
        let (mut app, mut core, t0) = paced(144);
        // A renderer presents (closes the gate): the runner requests the next redraw.
        core.begin_update(app.world_mut(), t0);
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        assert!(core.end_update(app.world(), t0).request_redraw);
        // Due, but the frame callback has not come: wait up to a quarter of the period.
        let due = t0 + period;
        let until = due + period / 4;
        assert_eq!(
            core.pace(app.world(), due),
            FrameWait::Sleep { wake: until }
        );
        // It comes 0.3 ms later: run now (the runner stamps the gate with the real clock; the
        // test sets the synthetic instant).
        core.window_event(app.world_mut(), WindowEvent::RedrawRequested);
        let callback = due + Duration::from_micros(300);
        core.gate_opened_at = Some(callback);
        assert_eq!(core.pace(app.world(), callback), FrameWait::Ready);
        // Phase lock: the grid restarts at the callback.
        core.begin_update(app.world_mut(), callback);
        assert_eq!(core.pacer.next_frame(Some(period)), Some(callback + period));

        // The next callback comes before the deadline: no wait, and the grid is kept.
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        assert!(core.end_update(app.world(), callback).request_redraw);
        let due = callback + period;
        core.window_event(app.world_mut(), WindowEvent::RedrawRequested);
        core.gate_opened_at = Some(due - ms(1));
        assert_eq!(core.pace(app.world(), due), FrameWait::Ready);
        core.begin_update(app.world_mut(), due);
        assert_eq!(core.pacer.next_frame(Some(period)), Some(due + period));

        // A gate that opens after the wait window (the update was late anyway): no lock.
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        assert!(core.end_update(app.world(), due).request_redraw);
        let next = due + period;
        core.window_event(app.world_mut(), WindowEvent::RedrawRequested);
        core.gate_opened_at = Some(next + ms(3));
        core.begin_update(app.world_mut(), next + ms(3));
        assert_eq!(core.pacer.next_frame(Some(period)), Some(next + period));

        // No callback at all (hidden window): give up at the end of the wait.
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        assert!(core.end_update(app.world(), next + ms(3)).request_redraw);
        let due = next + period;
        assert!(matches!(
            core.pace(app.world(), due),
            FrameWait::Sleep { .. }
        ));
        assert_eq!(core.pace(app.world(), due + period / 4), FrameWait::Ready);

        // The wait is at most 2 ms (60 fps: a quarter period would be 4.2 ms).
        app.insert_resource(FrameLimit::new(60));
        let due = core.pacer.next_frame(FrameLimit::new(60).period()).unwrap();
        assert_eq!(
            core.pace(app.world(), due),
            FrameWait::Sleep {
                wake: due + GATE_WAIT_MAX
            }
        );
        // Never while unlimited or occluded.
        app.insert_resource(FrameLimit::UNLIMITED);
        assert_eq!(core.pace(app.world(), due), FrameWait::Ready);
        app.insert_resource(FrameLimit::new(144));
        core.winit_occluded = true;
        core.begin_update(app.world_mut(), due + ms(200));
        let due = core.pacer.next_frame(Some(OCCLUDED_WAIT)).unwrap();
        assert_eq!(core.pace(app.world(), due), FrameWait::Ready);
    }

    #[test]
    fn pacing_info_reports_gate_waits_and_misses() {
        let period = Duration::from_nanos(6_944_444);
        let pacing = |app: &App| *app.world().resource::<FramePacingInfo>();
        let (mut app, mut core, t0) = paced(144);
        core.begin_update(app.world_mut(), t0);
        assert_eq!(pacing(&app).gate_wait, Duration::ZERO);
        assert!(!pacing(&app).gate_missed);
        // Presented: the callback comes 0.3 ms after the deadline.
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        assert!(core.end_update(app.world(), t0).request_redraw);
        let due = t0 + period;
        assert!(matches!(
            core.pace(app.world(), due),
            FrameWait::Sleep { .. }
        ));
        core.window_event(app.world_mut(), WindowEvent::RedrawRequested);
        let callback = due + Duration::from_micros(300);
        core.gate_opened_at = Some(callback);
        assert_eq!(core.pace(app.world(), callback), FrameWait::Ready);
        core.begin_update(app.world_mut(), callback);
        let info = pacing(&app);
        assert_eq!(info.gate_wait, Duration::from_micros(300));
        assert!(!info.gate_missed);
        assert_eq!((info.gate_waits, info.gate_misses), (1, 0));
        assert_eq!(info.gate_opened_at, Some(callback));

        // No callback: the update gives up at the end of the window.
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        assert!(core.end_update(app.world(), callback).request_redraw);
        let due = callback + period;
        assert!(matches!(
            core.pace(app.world(), due),
            FrameWait::Sleep { .. }
        ));
        let late = due + period / 4;
        assert_eq!(core.pace(app.world(), late), FrameWait::Ready);
        core.begin_update(app.world_mut(), late);
        let info = pacing(&app);
        assert_eq!(info.gate_wait, period / 4);
        assert!(info.gate_missed);
        assert_eq!((info.gate_waits, info.gate_misses), (2, 1));

        // The callback arrives before the next deadline: no wait.
        core.window_event(app.world_mut(), WindowEvent::RedrawRequested);
        let due = due + period;
        assert_eq!(core.pace(app.world(), due), FrameWait::Ready);
        core.begin_update(app.world_mut(), due);
        let info = pacing(&app);
        assert_eq!((info.gate_wait, info.gate_missed), (Duration::ZERO, false));
        assert_eq!((info.gate_waits, info.gate_misses), (2, 1));
    }

    #[test]
    fn monitor_refresh_is_polled_once_without_frame_callbacks() {
        // X11, Windows, macOS: nothing uses the rate after the first poll.
        let mut core = core();
        let t0 = Instant::now();
        let mut polls = 0;
        for i in 0..3 {
            core.poll_monitor(t0 + MONITOR_POLL * i, || {
                polls += 1;
                Some(60_000 + i * 1000)
            });
        }
        assert_eq!((polls, core.refresh_millihertz), (1, Some(60_000)));
    }

    #[test]
    fn monitor_refresh_is_polled_at_most_every_second() {
        let mut app = app();
        let mut core = core();
        core.callback_gate = true;
        let t0 = Instant::now();
        let mut polls = 0;
        core.poll_monitor(t0, || {
            polls += 1;
            Some(144_002)
        });
        core.poll_monitor(t0 + MONITOR_POLL - Duration::from_millis(1), || {
            polls += 1;
            Some(60_000)
        });
        core.begin_update(app.world_mut(), t0);
        assert_eq!(
            app.world().resource::<FramePacingInfo>().refresh_millihertz,
            Some(144_002)
        );
        // 0 (and `None`) mean unknown.
        core.poll_monitor(t0 + MONITOR_POLL, || {
            polls += 1;
            Some(0)
        });
        core.begin_update(app.world_mut(), t0 + MONITOR_POLL);
        assert_eq!(
            app.world().resource::<FramePacingInfo>().refresh_millihertz,
            None
        );
        assert_eq!(polls, 2);
    }

    /// 144.002 Hz, the panel of the development machine as winit reports it.
    const PANEL_MHZ: u32 = 144_002;

    /// A visible Wayland-like window (the gate follows frame callbacks) on a 144.002 Hz panel
    /// with the given limit, after its first frame callback.
    fn display(max_fps: u32) -> (App, RunnerCore, Instant) {
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let mut app = app();
        app.insert_resource(FrameLimit::new(max_fps));
        let mut core = core();
        core.callback_gate = true;
        let t0 = Instant::now();
        created(&mut core, app.world_mut(), t0);
        core.poll_monitor(t0, || Some(PANEL_MHZ));
        // The first gate comes with the first configure: no lock yet.
        let first = t0 + Duration::from_millis(5);
        core.gate_opened(app.world_mut(), first);
        assert_eq!(core.display_period(app.world()), None);
        // The frame callbacks of the first presents: after two in a row the gate follows the
        // display.
        let mut callback = first;
        for _ in 0..SLOW_GATES_TO_LOCK {
            assert_eq!(core.display_period(app.world()), None);
            present(&mut app, &mut core, callback);
            callback += panel;
            core.gate_opened(app.world_mut(), callback);
        }
        assert_eq!(core.display_period(app.world()), Some(panel));
        (app, core, callback)
    }

    /// Follows the pacing from `now`, with no events, until an update may run.
    fn ready_at(app: &App, core: &mut RunnerCore, mut now: Instant) -> Instant {
        loop {
            match core.pace(app.world(), now) {
                FrameWait::Ready => return now,
                FrameWait::Sleep { wake } => now = wake,
                FrameWait::Spin { until } => now = until,
            }
        }
    }

    /// A renderer presents during the update at `at`: the gate closes and the runner asks for
    /// the next redraw.
    fn present(app: &mut App, core: &mut RunnerCore, at: Instant) -> EndOfUpdate {
        core.begin_update(app.world_mut(), at);
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        let end = core.end_update(app.world(), at + Duration::from_micros(800));
        assert!(end.request_redraw);
        end
    }

    #[test]
    fn display_lock_needs_frame_callbacks_and_a_limit_at_the_refresh_rate() {
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let (mut app, mut core, t) = display(144);
        assert_eq!(core.display_period(app.world()), Some(panel));
        // Within 1 % below the panel (143), or above it (240): the display. 142 is the grid.
        for (max_fps, locked) in [(143, true), (240, true), (142, false), (0, false)] {
            app.insert_resource(FrameLimit::new(max_fps));
            assert_eq!(
                core.display_period(app.world()).is_some(),
                locked,
                "max_fps={max_fps}"
            );
        }
        app.insert_resource(FrameLimit::new(144));
        // Gates that open at once (no frame callback: nothing presents) stop it after 8.
        for i in 0..QUICK_GATES_MAX {
            assert!(core.display_period(app.world()).is_some(), "quick gate {i}");
            let at = t + Duration::from_millis(u64::from(i) + 1);
            core.redraw_pending_since = Some(at);
            core.gate_opened(app.world_mut(), at + Duration::from_micros(20));
        }
        assert_eq!(core.display_period(app.world()), None);
        // Two callbacks in a row bring it back; one is not enough.
        for (i, at) in [20, 30].into_iter().enumerate() {
            assert_eq!(core.display_period(app.world()), None, "callback {i}");
            let at = t + Duration::from_millis(at);
            core.redraw_pending_since = Some(at);
            core.gate_opened(app.world_mut(), at + Duration::from_millis(3));
        }
        assert_eq!(core.display_period(app.world()), Some(panel));
        // Not while occluded, without a refresh rate or without frame callbacks (X11).
        core.occluded = true;
        assert_eq!(core.display_period(app.world()), None);
        core.occluded = false;
        core.refresh_millihertz = None;
        assert_eq!(core.display_period(app.world()), None);
        core.refresh_millihertz = Some(PANEL_MHZ);
        core.callback_gate = false;
        assert_eq!(core.display_period(app.world()), None);
    }

    #[test]
    fn display_locked_updates_follow_the_frame_callbacks() {
        let us = Duration::from_micros;
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let pacing = |app: &App| *app.world().resource::<FramePacingInfo>();
        let (mut app, mut core, t0) = display(144);
        let end = present(&mut app, &mut core, t0);
        assert!(pacing(&app).display_locked);
        // The next callback wakes the loop: no deadline wake-up, no spin.
        let due = t0 + panel;
        let latest = due + panel + panel / 4;
        assert_eq!(end.control_flow, ControlFlow::WaitUntil(latest));
        assert_eq!(
            core.pace(app.world(), due - us(400)),
            FrameWait::Sleep { wake: latest }
        );

        // The callback comes 0.4 ms before the deadline: run at once, the grid follows it.
        let callback = due - us(400);
        core.gate_opened(app.world_mut(), callback);
        assert_eq!(core.pace(app.world(), callback), FrameWait::Ready);
        present(&mut app, &mut core, callback);
        assert_eq!(pacing(&app).gate_wait, Duration::ZERO);
        let due = callback + panel;
        assert_eq!(core.pacer.next_frame(Some(panel)), Some(due));

        // 0.3 ms after the deadline: it waited 0.3 ms, presents, and the grid follows.
        assert!(matches!(
            core.pace(app.world(), due),
            FrameWait::Sleep { .. }
        ));
        let callback = due + us(300);
        core.gate_opened(app.world_mut(), callback);
        assert_eq!(core.pace(app.world(), callback), FrameWait::Ready);
        present(&mut app, &mut core, callback);
        assert_eq!(pacing(&app).gate_wait, us(300));
        assert!(!pacing(&app).gate_missed);
        let due = callback + panel;

        // The compositor skipped a refresh: the callback comes a whole refresh late and the
        // update waits for it instead of running without presenting.
        assert!(matches!(
            core.pace(app.world(), due + panel / 4),
            FrameWait::Sleep { .. }
        ));
        let callback = due + panel + us(100);
        core.gate_opened(app.world_mut(), callback);
        assert_eq!(core.pace(app.world(), callback), FrameWait::Ready);
        present(&mut app, &mut core, callback);
        assert_eq!(pacing(&app).gate_wait, panel + us(100));
        assert!(!pacing(&app).gate_missed);
        let due = callback + panel;

        // A callback far too early (after the redraw was requested) waits until half a period
        // before the deadline.
        let early = due - panel + us(1500);
        core.gate_opened(app.world_mut(), early);
        assert_eq!(
            core.pace(app.world(), early),
            FrameWait::Sleep {
                wake: due - panel / 2
            }
        );
        assert_eq!(core.pace(app.world(), due - panel / 2), FrameWait::Ready);
        present(&mut app, &mut core, due - panel / 2);
        let due = due - panel / 2 + panel;

        // No callback at all (hidden window): give up a refresh and a quarter late.
        let latest = due + panel + panel / 4;
        assert_eq!(
            core.pace(app.world(), due),
            FrameWait::Sleep { wake: latest }
        );
        assert_eq!(core.pace(app.world(), latest), FrameWait::Ready);
        core.begin_update(app.world_mut(), latest);
        let info = pacing(&app);
        assert!(info.gate_missed);
        assert_eq!(info.gate_wait, panel + panel / 4);
        // The grid restarts there (more than a period late).
        assert_eq!(core.pacer.next_frame(Some(panel)), Some(latest + panel));
        // Still no callback: the loop sleeps through the next wait without asking again, and
        // the update still reports it.
        let end = core.end_update(app.world(), latest + us(800));
        let due = latest + panel;
        let latest = due + panel + panel / 4;
        assert_eq!(end.control_flow, ControlFlow::WaitUntil(latest));
        core.begin_update(app.world_mut(), latest);
        let info = pacing(&app);
        assert!(info.gate_missed);
        assert_eq!(info.gate_wait, panel + panel / 4);

        // A limit under 99 % of the panel keeps its own grid and the short wait.
        app.insert_resource(FrameLimit::new(120));
        assert!(core.display_period(app.world()).is_none());
    }

    #[test]
    fn an_open_gate_that_nothing_consumes_keeps_the_display_rate() {
        // Following the display, the updates stop presenting (no camera, a menu, a surface
        // being rebuilt): the open gate is no new frame callback, so updates keep one refresh
        // apart instead of running every half refresh.
        let us = Duration::from_micros;
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let (mut app, mut core, t0) = display(144);
        let updates = 144;
        let mut now = t0;
        let mut starts = Vec::new();
        for _ in 0..updates {
            now = ready_at(&app, &mut core, now);
            starts.push(now);
            core.begin_update(app.world_mut(), now);
            assert!(app.world().resource::<FramePacingInfo>().display_locked);
            assert!(!core.end_update(app.world(), now + us(500)).request_redraw);
            now += us(500);
        }
        assert_eq!(starts[0], t0, "the callback's own update runs at once");
        assert_eq!(starts[updates - 1] - t0, panel * (updates as u32 - 1));
        let info = *app.world().resource::<FramePacingInfo>();
        assert_eq!((info.gate_waits, info.gate_misses), (0, 0));

        // Presenting again: the next callback is the clock again.
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        assert!(core.end_update(app.world(), now).request_redraw);
        let callback = now + panel;
        core.gate_opened(app.world_mut(), callback);
        assert_eq!(ready_at(&app, &mut core, callback), callback);
    }

    #[test]
    fn a_slow_update_still_waits_a_refresh_for_its_callback() {
        // An update that ends after its deadline plus a refresh and a quarter (a game under
        // about 64 fps on a 144 Hz panel) waits for the callback of its own present.
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let pacing = |app: &App| *app.world().resource::<FramePacingInfo>();
        let (mut app, mut core, t0) = display(144);
        core.begin_update(app.world_mut(), t0);
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        let end_at = t0 + panel * 5 / 2;
        let end = core.end_update(app.world(), end_at);
        assert!(end.request_redraw);
        let latest = end_at + panel + panel / 4;
        assert_eq!(end.control_flow, ControlFlow::WaitUntil(latest));
        assert!(matches!(
            core.pace(app.world(), end_at + panel / 2),
            FrameWait::Sleep { .. }
        ));
        // The callback comes a refresh after the present: the update presents, and the grid
        // follows the callback.
        let callback = end_at + panel;
        core.gate_opened(app.world_mut(), callback);
        assert_eq!(core.pace(app.world(), callback), FrameWait::Ready);
        present(&mut app, &mut core, callback);
        assert!(!pacing(&app).gate_missed);
        assert_eq!(core.pacer.next_frame(Some(panel)), Some(callback + panel));
    }

    #[test]
    fn a_callback_cadence_off_the_reported_rate_drops_the_display_lock() {
        let ms = Duration::from_millis;
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        // The callbacks of a quick frame come one refresh apart.
        let run = |cadence: &[Duration]| {
            let (mut app, mut core, mut t) = display(144);
            for &interval in cadence {
                present(&mut app, &mut core, t);
                t += interval;
                core.gate_opened(app.world_mut(), t);
            }
            (app, core, t)
        };
        // The panel's own rate, measured 143.88 Hz against 144.002 reported (ADR 0017), with a
        // skipped refresh now and then: the lock stays.
        let measured = Duration::from_micros(6950);
        let mut cadence = vec![measured; CADENCE_SAMPLES * 2];
        for skipped in cadence.iter_mut().step_by(5) {
            *skipped = measured * 2;
        }
        let (app, core, _) = run(&cadence);
        assert_eq!(core.cadence.len, CADENCE_SAMPLES);
        assert_eq!(core.display_period(app.world()), Some(panel));

        // The compositor runs the window at 60 Hz (another monitor than winit reports, or
        // VRR): the grid of the limit governs once enough intervals say so.
        let sixty = Duration::from_nanos(16_666_667);
        let (mut app, mut core, t) = run(&[sixty; CADENCE_MIN_SAMPLES - 2]);
        assert_eq!(
            core.display_period(app.world()),
            Some(panel),
            "too few samples"
        );
        present(&mut app, &mut core, t);
        core.gate_opened(app.world_mut(), t + sixty);
        assert_eq!(core.display_period(app.world()), None);
        present(&mut app, &mut core, t + sixty);
        assert!(!app.world().resource::<FramePacingInfo>().display_locked);

        // A redraw requested late after the previous callback (a low limit) says nothing about
        // the display.
        let samples = core.cadence.len;
        let t = t + sixty * 2;
        core.redraw_pending_since = Some(t - ms(10));
        core.gate_opened(app.world_mut(), t);
        assert_eq!(core.cadence.len, samples);

        // The window moves to a 60 Hz monitor: the old samples go, and a limit of 144 follows
        // that display.
        core.poll_monitor(t + MONITOR_POLL, || Some(60_000));
        assert_eq!((core.cadence.len, core.cadence_mismatch), (0, false));
        assert_eq!(
            core.display_period(app.world()),
            Some(Duration::from_nanos(16_666_666))
        );
    }

    #[test]
    fn a_limit_changed_while_waiting_for_the_gate_paces_at_once() {
        let us = Duration::from_micros;
        let margin = FramePacer::DEFAULT_SPIN_MARGIN;
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let (mut app, mut core, t0) = display(144);
        present(&mut app, &mut core, t0);
        let due = t0 + panel;
        assert!(matches!(
            core.pace(app.world(), due),
            FrameWait::Sleep { .. }
        ));
        assert!(core.gate_hold.is_some());
        // 60 fps: the limit's grid from the same anchor, and the old hold is gone.
        app.insert_resource(FrameLimit::new(60));
        let due60 = t0 + FrameLimit::new(60).period().expect("limited");
        assert_eq!(
            core.pace(app.world(), due + us(100)),
            FrameWait::Sleep {
                wake: due60 - margin
            }
        );
        assert_eq!(core.gate_hold, None);
        // Its gate wait counts from its own deadline.
        assert!(matches!(
            core.pace(app.world(), due60),
            FrameWait::Sleep { .. }
        ));
        let callback = due60 + us(500);
        core.gate_opened(app.world_mut(), callback);
        assert_eq!(core.pace(app.world(), callback), FrameWait::Ready);
        core.begin_update(app.world_mut(), callback);
        let info = *app.world().resource::<FramePacingInfo>();
        assert_eq!((info.gate_wait, info.gate_missed), (us(500), false));
        assert!(!info.display_locked);

        // Unlimited while waiting: run at once.
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        app.insert_resource(FrameLimit::new(144));
        assert!(core.end_update(app.world(), callback).request_redraw);
        let due = core.pacer.next_frame(Some(panel)).expect("due");
        assert!(matches!(
            core.pace(app.world(), due),
            FrameWait::Sleep { .. }
        ));
        app.insert_resource(FrameLimit::UNLIMITED);
        assert_eq!(core.pace(app.world(), due + us(100)), FrameWait::Ready);
    }

    #[test]
    fn reactive_updates_follow_the_display_but_never_faster() {
        let us = Duration::from_micros;
        let ms = Duration::from_millis;
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let (mut app, mut core, t0) = display(144);
        core.settings.update_mode = UpdateMode::Reactive { wait: ms(50) };
        // The timeout is later than the wait for the callback: it wins, and the callback (an
        // event) wakes the loop.
        let end = present(&mut app, &mut core, t0);
        assert_eq!(
            end.control_flow,
            ControlFlow::WaitUntil(t0 + us(800) + ms(50))
        );
        let callback = t0 + panel;
        core.gate_opened(app.world_mut(), callback);
        assert_eq!(core.pace(app.world(), callback), FrameWait::Ready);
        core.begin_update(app.world_mut(), callback);
        assert!(app.world().resource::<FramePacingInfo>().display_locked);
        // Nothing presents: an event right after the update waits for the next deadline, not
        // for half a refresh.
        let end = core.end_update(app.world(), callback + us(500));
        assert!(!end.request_redraw);
        assert_eq!(
            end.control_flow,
            ControlFlow::WaitUntil(callback + us(500) + ms(50))
        );
        assert!(matches!(
            core.pace(app.world(), callback + panel / 2),
            FrameWait::Sleep { .. }
        ));
        assert_eq!(
            ready_at(&app, &mut core, callback + us(600)),
            callback + panel
        );
    }

    #[test]
    fn the_frame_deadline_follows_the_pacing() {
        let us = Duration::from_micros;
        let deadline = |app: &App| *app.world().resource::<FrameDeadline>();
        let period = FrameLimit::new(144).period().expect("limited");
        let (mut app, mut core, t0) = paced(144);
        core.begin_update(app.world_mut(), t0);
        assert_eq!(
            deadline(&app),
            FrameDeadline {
                next: Some(t0 + period),
                period: Some(period),
                last_update: Duration::ZERO,
            }
        );
        core.end_update(app.world(), t0 + us(1500));
        let t1 = t0 + period;
        core.begin_update(app.world_mut(), t1);
        assert_eq!(deadline(&app).next, Some(t1 + period));
        assert_eq!(deadline(&app).last_update, us(1500));
        // Unlimited: no deadline.
        app.insert_resource(FrameLimit::UNLIMITED);
        core.end_update(app.world(), t1 + us(700));
        core.begin_update(app.world_mut(), t1 + us(800));
        assert_eq!(
            deadline(&app),
            FrameDeadline {
                next: None,
                period: None,
                last_update: us(700),
            }
        );
        // Following the display: the refresh period from the frame callback.
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let (mut app, mut core, t0) = display(144);
        core.begin_update(app.world_mut(), t0);
        assert_eq!(deadline(&app).next, Some(t0 + panel));
        assert_eq!(deadline(&app).period, Some(panel));
    }

    #[test]
    fn gates_that_open_at_once_keep_the_limit_grid() {
        // A window nothing presents to on Wayland (as `just test-window`): the redraw arrives
        // at once, so the limit's grid governs even at the panel's rate.
        let period = FrameLimit::new(144).period().expect("limited");
        let mut app = app();
        app.insert_resource(FrameLimit::new(144));
        let mut core = core();
        core.callback_gate = true;
        let t0 = Instant::now();
        created(&mut core, app.world_mut(), t0);
        core.poll_monitor(t0, || Some(PANEL_MHZ));
        // The first configure takes a while; it does not count as a frame callback.
        core.gate_opened(app.world_mut(), t0 + Duration::from_millis(5));
        let mut now = t0 + Duration::from_millis(5);
        for _ in 0..20 {
            core.begin_update(app.world_mut(), now);
            app.world_mut().resource_mut::<WindowState>().frame_ready = false;
            assert!(core.end_update(app.world(), now).request_redraw);
            core.gate_opened(app.world_mut(), now + Duration::from_micros(30));
            now = core.pacer.next_frame(Some(period)).expect("due");
        }
        assert!(!app.world().resource::<FramePacingInfo>().display_locked);
        assert_eq!(core.display_period(app.world()), None);
        assert_eq!(now, t0 + Duration::from_millis(5) + period * 20);
    }

    #[test]
    fn frame_limit_keeps_the_occluded_cap_and_lower_limits() {
        let ms = Duration::from_millis;
        let margin = FramePacer::DEFAULT_SPIN_MARGIN;
        for (max_fps, period) in [(144, OCCLUDED_WAIT), (10, ms(100)), (0, OCCLUDED_WAIT)] {
            let (mut app, mut core, t0) = paced(max_fps);
            core.winit_occluded = true;
            core.begin_update(app.world_mut(), t0);
            assert!(state(app.world()).occluded);
            assert_eq!(
                core.end_update(app.world(), t0).control_flow,
                ControlFlow::WaitUntil(t0 + period - margin),
                "max_fps={max_fps}"
            );
        }
    }

    #[test]
    fn reactive_updates_never_run_faster_than_the_limit() {
        let ms = Duration::from_millis;
        let margin = FramePacer::DEFAULT_SPIN_MARGIN;
        let reactive = RunnerSettings {
            update_mode: UpdateMode::Reactive { wait: ms(50) },
            ..RunnerSettings::default()
        };
        // The timeout is later than the frame deadline: it wins.
        let (mut app, _, t0) = paced(144);
        let mut core = RunnerCore::new(reactive);
        core.begin_update(app.world_mut(), t0);
        assert_eq!(
            core.end_update(app.world(), t0).control_flow,
            ControlFlow::WaitUntil(t0 + ms(50))
        );
        // A 10 fps limit pushes the timeout to the deadline, and an early event waits for it.
        app.insert_resource(FrameLimit::new(10));
        assert_eq!(
            core.end_update(app.world(), t0).control_flow,
            ControlFlow::WaitUntil(t0 + ms(100) - margin)
        );
        assert_eq!(
            core.pace(app.world(), t0 + ms(60)),
            FrameWait::Sleep {
                wake: t0 + ms(100) - margin
            }
        );
    }

    const US: Duration = Duration::from_micros(1);

    /// The slot end the runner keeps before a deadline `due`: the spin margin plus
    /// `IDLE_MARGIN` (1 ms at the default spin margin).
    fn slot_end(due: Instant) -> Instant {
        due - FramePacer::DEFAULT_SPIN_MARGIN - IDLE_MARGIN
    }

    #[test]
    fn end_update_opens_an_idle_slot_on_the_grid() {
        let period = FrameLimit::new(144).period().expect("limited");
        let (mut app, mut core, t0) = paced(144);
        core.idle_systems = true;
        core.begin_update(app.world_mut(), t0);
        app.world_mut().resource_mut::<WindowState>().frame_ready = false;
        let end = core.end_update(app.world(), t0 + 1000 * US);
        // Poll first: the redraw is requested as before, and the next iteration runs a step.
        assert!(end.request_redraw);
        assert_eq!(end.control_flow, ControlFlow::Poll);
        let due = t0 + period;
        assert_eq!(core.idle_end, Some(slot_end(due)));
        assert_eq!(slot_end(due), due - Duration::from_millis(1));
        let now = t0 + 1100 * US;
        let FrameWait::Sleep { wake } = core.pace(app.world(), now) else {
            panic!("the frame is not due yet");
        };
        assert_eq!(core.idle_step(now, wake), Some(now + boxel_app::IDLE_STEP));
        // Near the end of the slot the step stops there; past it there is none.
        let late = slot_end(due) - 300 * US;
        assert_eq!(core.idle_step(late, wake), Some(slot_end(due)));
        assert_eq!(core.idle_step(slot_end(due) - 100 * US, wake), None);
    }

    #[test]
    fn the_display_locked_slot_uses_the_callback_due() {
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let (mut app, mut core, t0) = display(144);
        core.idle_systems = true;
        let end = present(&mut app, &mut core, t0);
        assert_eq!(end.control_flow, ControlFlow::Poll);
        // The closed gate waits up to a refresh and a quarter past the deadline; the slot ends
        // 1 ms before the deadline anyway.
        let due = t0 + panel;
        assert_eq!(core.idle_end, Some(slot_end(due)));
        let now = t0 + 900 * US;
        let wake = due + panel + panel / 4;
        assert_eq!(core.pace(app.world(), now), FrameWait::Sleep { wake });
        assert!(core.idle_step(now, wake).is_some());
    }

    #[test]
    fn the_slot_never_reaches_the_runners_own_wake() {
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let (mut app, mut core, t0) = display(144);
        core.idle_systems = true;
        present(&mut app, &mut core, t0);
        let due = t0 + panel;
        // A callback far too early: the update waits for half a period before the deadline,
        // and the slot ends `IDLE_MARGIN` before that.
        let early = t0 + 1500 * US;
        core.gate_opened(app.world_mut(), early);
        let earliest = due - panel / 2;
        assert_eq!(
            core.pace(app.world(), early),
            FrameWait::Sleep { wake: earliest }
        );
        let until = core.idle_step(early, earliest).expect("room for a step");
        assert!(until <= earliest - IDLE_MARGIN);
        let mut now = early;
        while let Some(until) = core.idle_step(now, earliest) {
            assert!(until <= earliest - IDLE_MARGIN);
            let flow = core.after_idle(now, until, IdleWork::Ready, earliest);
            assert_eq!(flow, ControlFlow::Poll);
            now = until;
        }
        assert!(now <= earliest - IDLE_MARGIN);
        assert_eq!(core.idle.overruns, 0);
        assert_eq!(core.pace(app.world(), earliest), FrameWait::Ready);
    }

    #[test]
    fn no_slot_without_idle_systems_a_limit_or_room() {
        let period = FrameLimit::new(144).period().expect("limited");
        let open = |core: &mut RunnerCore, app: &App, at: Instant| {
            let flow = core.end_update(app.world(), at).control_flow;
            (core.idle_end.is_some(), flow)
        };
        // No `Idle` systems: the plain wait.
        let (mut app, mut core, t0) = paced(144);
        core.begin_update(app.world_mut(), t0);
        let at = t0 + 1000 * US;
        let wake = t0 + period - FramePacer::DEFAULT_SPIN_MARGIN;
        assert_eq!(
            open(&mut core, &app, at),
            (false, ControlFlow::WaitUntil(wake))
        );
        core.idle_systems = true;
        assert_eq!(open(&mut core, &app, at), (true, ControlFlow::Poll));
        // An update that ended too close to the slot end, in the spin margin or late.
        let due = t0 + period;
        for at in [slot_end(due) - 100 * US, due - 100 * US, due + 100 * US] {
            assert!(!open(&mut core, &app, at).0, "{:?}", at - t0);
        }
        // Unlimited.
        app.insert_resource(FrameLimit::UNLIMITED);
        assert_eq!(open(&mut core, &app, at), (false, ControlFlow::Poll));
        // Reactive mode.
        let (mut app, _, t0) = paced(144);
        let mut core = RunnerCore::new(RunnerSettings {
            update_mode: UpdateMode::Reactive {
                wait: Duration::from_millis(50),
            },
            ..RunnerSettings::default()
        });
        core.idle_systems = true;
        core.begin_update(app.world_mut(), t0);
        assert!(!open(&mut core, &app, t0 + 1000 * US).0);
        assert_eq!(core.idle_step(t0 + 1100 * US, t0 + period), None);
    }

    #[test]
    fn after_idle_picks_the_control_flow() {
        let period = FrameLimit::new(144).period().expect("limited");
        let (mut app, mut core, t0) = paced(144);
        core.idle_systems = true;
        core.begin_update(app.world_mut(), t0);
        core.end_update(app.world(), t0 + 500 * US);
        let due = t0 + period;
        let wake = due - FramePacer::DEFAULT_SPIN_MARGIN;
        let end = slot_end(due);
        let now = t0 + 1000 * US;
        assert_eq!(
            core.after_idle(t0 + 600 * US, now, IdleWork::Ready, wake),
            ControlFlow::Poll
        );
        assert_eq!(
            core.after_idle(now, now + 200 * US, IdleWork::Waiting, wake),
            ControlFlow::WaitUntil(now + 200 * US + IDLE_POLL)
        );
        // Near the end the poll stops at the end; past it, the plain wait.
        assert_eq!(
            core.after_idle(end - 400 * US, end - 100 * US, IdleWork::Waiting, wake),
            ControlFlow::WaitUntil(end)
        );
        assert_eq!(
            core.after_idle(end - 400 * US, end + 50 * US, IdleWork::Waiting, wake),
            ControlFlow::WaitUntil(wake)
        );
        // Done closes the slot until the next update.
        assert!(
            core.idle_step(end - Duration::from_millis(2), wake)
                .is_some()
        );
        assert_eq!(
            core.after_idle(end - 1500 * US, end - 1400 * US, IdleWork::Done, wake),
            ControlFlow::WaitUntil(wake)
        );
        assert_eq!(core.idle_step(end - Duration::from_millis(1), wake), None);
        // The next update publishes the gap: 5 steps, one past the slot end by 50 us.
        core.begin_update(app.world_mut(), due);
        let info = *app.world().resource::<IdleInfo>();
        assert_eq!(info.steps, 5);
        assert_eq!(info.tail, (400 + 200 + 300 + 450 + 100) * US);
        assert_eq!(
            (info.late, info.overruns, info.overrun_max),
            (50 * US, 1, 50 * US)
        );
        // The update after it starts a new gap; overruns add up.
        core.end_update(app.world(), due + 500 * US);
        core.begin_update(app.world_mut(), due + period);
        let info = *app.world().resource::<IdleInfo>();
        assert_eq!(
            (info.steps, info.tail, info.overruns),
            (0, Duration::ZERO, 1)
        );
    }

    #[test]
    fn a_quick_redraw_is_stamped_before_the_first_idle_step() {
        // A Wayland window nothing presents to: its redraw answers at once. The update's
        // iteration polls, so the answer is handled (and stamped) before any step.
        let (mut app, mut core, t0) = display(144);
        core.idle_systems = true;
        let quick = |core: &RunnerCore| (core.quick_gates, core.gate_was_callback);
        let mut now = t0;
        for i in 0..QUICK_GATES_MAX {
            assert!(core.display_period(app.world()).is_some(), "quick gate {i}");
            now = ready_at(&app, &mut core, now);
            let end = present(&mut app, &mut core, now);
            assert_eq!(end.control_flow, ControlFlow::Poll, "quick gate {i}");
            let requested = core.redraw_pending_since.expect("requested");
            core.gate_opened(app.world_mut(), requested + 50 * US);
            assert_eq!(quick(&core), (i + 1, false), "quick gate {i}");
            // Then idle steps until the systems are done.
            now = requested + 50 * US;
            while let FrameWait::Sleep { wake } = core.pace(app.world(), now)
                && let Some(until) = core.idle_step(now, wake)
            {
                core.after_idle(now, until, IdleWork::Ready, wake);
                now = until;
            }
        }
        assert_eq!(core.display_period(app.world()), None);
    }

    #[test]
    fn a_callback_during_a_poll_runs_the_update_at_once() {
        let panel = Duration::from_nanos(1_000_000_000_000 / u64::from(PANEL_MHZ));
        let (mut app, mut core, t0) = display(144);
        core.idle_systems = true;
        present(&mut app, &mut core, t0);
        let due = t0 + panel;
        let latest = due + panel + panel / 4;
        let now = slot_end(due) - 300 * US;
        let flow = core.after_idle(now - 200 * US, now, IdleWork::Waiting, latest);
        assert_eq!(flow, ControlFlow::WaitUntil(now + IDLE_POLL));
        // The frame callback wakes the loop before the poll ends: the update runs.
        let callback = due - 200 * US;
        core.gate_opened(app.world_mut(), callback);
        assert_eq!(core.pace(app.world(), callback), FrameWait::Ready);
        core.begin_update(app.world_mut(), callback);
        assert_eq!(core.idle_end, None);
        assert_eq!(app.world().resource::<IdleInfo>().steps, 1);
    }

    #[test]
    fn the_occluded_slot_uses_the_occluded_period() {
        let (mut app, mut core, t0) = paced(144);
        core.idle_systems = true;
        core.winit_occluded = true;
        core.begin_update(app.world_mut(), t0);
        assert_eq!(
            core.end_update(app.world(), t0 + 500 * US).control_flow,
            ControlFlow::Poll
        );
        assert_eq!(core.idle_end, Some(slot_end(t0 + OCCLUDED_WAIT)));
    }

    #[test]
    fn an_occluded_window_without_a_limit_never_idles() {
        // Hidden updates still wait on the 33 ms grid, but without a limit that wait is not a
        // frame limit: no slot, the plain wait as before. Minimized, occluded by winit, or a
        // stalled redraw (the watchdog).
        for hide in 0..3 {
            let (mut app, mut core, t0) = paced(144);
            app.insert_resource(FrameLimit::UNLIMITED);
            core.idle_systems = true;
            match hide {
                0 => core.minimized = true,
                1 => core.winit_occluded = true,
                _ => core.redraw_pending_since = Some(t0 - REDRAW_TIMEOUT - 1000 * US),
            }
            core.begin_update(app.world_mut(), t0);
            assert!(core.occluded, "case {hide}");
            let flow = core.end_update(app.world(), t0 + 500 * US).control_flow;
            assert_eq!(
                flow,
                ControlFlow::WaitUntil(t0 + OCCLUDED_WAIT - FramePacer::DEFAULT_SPIN_MARGIN),
                "case {hide}"
            );
            assert_eq!(core.idle_end, None, "case {hide}");
            assert_eq!(core.idle_step(t0 + 600 * US, t0 + OCCLUDED_WAIT), None);
        }
    }

    /// What a run of [`simulate`] saw, with instants as times since its start.
    #[derive(Debug, Default, PartialEq)]
    struct Trace {
        /// Update starts.
        starts: Vec<Duration>,
        /// `FramePacingInfo` of each update: gate wait, gate missed, display locked, the
        /// counters of waits and misses, and the latest gate opening.
        pacing: Vec<(Duration, bool, bool, u64, u64, Option<Duration>)>,
        /// Gate classification at the end.
        gates: (u8, u8, bool),
        /// Median frame callback interval at the end.
        cadence: Option<Duration>,
    }

    /// Runs `updates` updates on a synthetic event loop from `now`: each update takes 0.8 ms
    /// and presents, and the compositor answers a redraw requested at `r` at `callback(r)`.
    /// Events are handled between loop iterations, so one that comes during an idle step is
    /// stamped when the step returns. With `idle`, the `Idle` systems take up to 0.4 ms a step
    /// and answer `Ready`, then `Waiting` three times, then `Done`. Returns the trace and the
    /// idle steps run.
    fn simulate(
        app: &mut App,
        core: &mut RunnerCore,
        mut now: Instant,
        updates: usize,
        idle: bool,
        callback: impl Fn(Instant) -> Instant,
    ) -> (Trace, u32) {
        let mut trace = Trace::default();
        let base = now;
        let (mut flow, mut pending, mut gap, mut steps) = (ControlFlow::Poll, None, 0, 0);
        core.idle_systems = idle;
        while trace.starts.len() < updates {
            let wake = match flow {
                ControlFlow::Poll => now,
                ControlFlow::WaitUntil(at) => at.max(now),
                ControlFlow::Wait => unreachable!("continuous updates always wake"),
            };
            match pending {
                Some(at) if at <= wake => {
                    now = now.max(at);
                    pending = None;
                    core.gate_opened(app.world_mut(), now);
                }
                _ => now = wake,
            }
            let waited = loop {
                match core.pace(app.world(), now) {
                    FrameWait::Ready => break None,
                    FrameWait::Spin { until } => now = until,
                    FrameWait::Sleep { wake } => {
                        break Some(match core.idle_step(now, wake) {
                            Some(until) => {
                                let started = now;
                                now = (now + 400 * US).min(until);
                                gap += 1;
                                steps += 1;
                                let work = match gap {
                                    1 => IdleWork::Ready,
                                    2..=4 => IdleWork::Waiting,
                                    _ => IdleWork::Done,
                                };
                                core.after_idle(started, now, work, wake)
                            }
                            None => ControlFlow::WaitUntil(wake),
                        });
                    }
                }
            };
            if let Some(waited) = waited {
                flow = waited;
                continue;
            }
            core.begin_update(app.world_mut(), now);
            trace.starts.push(now - base);
            let p = *app.world().resource::<FramePacingInfo>();
            trace.pacing.push((
                p.gate_wait,
                p.gate_missed,
                p.display_locked,
                p.gate_waits,
                p.gate_misses,
                p.gate_opened_at
                    .map(|at| at.saturating_duration_since(base)),
            ));
            app.world_mut().resource_mut::<WindowState>().frame_ready = false;
            now += 800 * US;
            let end = core.end_update(app.world(), now);
            if end.request_redraw {
                pending = Some(callback(now));
            }
            flow = end.control_flow;
            gap = 0;
        }
        trace.gates = (core.quick_gates, core.slow_gates, core.gate_was_callback);
        trace.cadence = core.cadence.median();
        (trace, steps)
    }

    /// The compositor's frame callbacks from `base`: one every 6.95 ms (the measured cadence
    /// of the 144.002 Hz panel, ADR 0017) with up to 0.25 ms of jitter, answering the redraws
    /// committed 0.1 ms before; every 13th refresh is skipped (the callback comes one later).
    fn compositor(base: Instant) -> impl Fn(Instant) -> Instant {
        let refresh = Duration::from_micros(6950);
        move |requested: Instant| {
            let mut n: u32 = 1;
            loop {
                let jitter = i64::from(n * 7919 % 501) - 250;
                let vblank = base + refresh * n;
                let vblank = if jitter >= 0 {
                    vblank + US * jitter.unsigned_abs() as u32
                } else {
                    vblank - US * jitter.unsigned_abs() as u32
                };
                if vblank > requested + 100 * US && !n.is_multiple_of(13) {
                    return vblank;
                }
                n += 1;
            }
        }
    }

    #[test]
    fn idle_steps_leave_display_lock_and_cadence_unchanged() {
        // Following the display: the callbacks come within 0.5 ms of each deadline, after the
        // slot ends, so every update, wait, lock decision and cadence sample is the same.
        let run = |idle: bool| {
            let (mut app, mut core, t0) = display(144);
            simulate(&mut app, &mut core, t0, 300, idle, compositor(t0))
        };
        let (without, none) = run(false);
        let (with, steps) = run(true);
        assert_eq!(none, 0);
        assert!(steps >= 4 * 290, "{steps} idle steps");
        assert!(with.pacing.iter().all(|p| p.2), "display locked");
        assert!(
            with.pacing.iter().any(|p| p.0 > Duration::ZERO),
            "gate waits"
        );
        assert_eq!(with, without);

        // A limit under the refresh rate keeps its grid: callbacks may come during a step and
        // are stamped late, but the updates and their gate waits are the same.
        let run = |idle: bool| {
            let (mut app, mut core, t0) = display(144);
            app.insert_resource(FrameLimit::new(120));
            simulate(&mut app, &mut core, t0, 300, idle, compositor(t0))
        };
        let (without, _) = run(false);
        let (with, steps) = run(true);
        assert!(steps > 0);
        assert_eq!(with.starts, without.starts);
        let waits = |t: &Trace| -> Vec<_> { t.pacing.iter().map(|p| (p.0, p.1, p.2)).collect() };
        assert_eq!(waits(&with), waits(&without));

        // X11: the redraw answers at once, before any step.
        let run = |idle: bool| {
            let (mut app, mut core, t0) = paced(144);
            // `paced` stamps its first gate with the real clock.
            core.gate_opened_at = Some(t0);
            simulate(&mut app, &mut core, t0, 100, idle, |requested| requested)
        };
        let (without, _) = run(false);
        let (with, steps) = run(true);
        assert!(steps > 0);
        assert_eq!(with, without);
    }

    #[test]
    fn cursor_changes_are_reported_once() {
        let mut app = app();
        let mut core = core();
        assert_eq!(core.cursor_change(app.world()), None);
        let locked = CursorOptions {
            grab: CursorGrab::Locked,
            visible: false,
        };
        *app.world_mut().resource_mut::<CursorOptions>() = locked;
        assert_eq!(core.cursor_change(app.world()), Some(locked));
        assert_eq!(core.cursor_change(app.world()), None);
    }

    #[test]
    fn key_and_wheel_conversions() {
        assert_eq!(
            key_message(
                PhysicalKey::Code(KeyCode::KeyW),
                ElementState::Released,
                true
            ),
            Some(KeyboardInput {
                key: KeyCode::KeyW,
                state: ButtonState::Released,
                repeat: true
            })
        );
        assert_eq!(
            key_message(
                PhysicalKey::Unidentified(NativeKeyCode::Xkb(999)),
                ElementState::Pressed,
                false
            ),
            None
        );
        assert_eq!(
            wheel_delta(MouseScrollDelta::LineDelta(0.0, -3.0)),
            Vec2::new(0.0, -3.0)
        );
    }

    #[test]
    fn window_events_reach_the_input_resources() {
        let mut app = app();
        let mut core = core();
        core.window_event(
            app.world_mut(),
            WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Right,
            },
        );
        app.update();
        let buttons = app.world().resource::<ButtonInput<MouseButton>>();
        assert!(buttons.just_pressed(MouseButton::Right));
    }
}
