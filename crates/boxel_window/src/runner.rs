use crate::input::{
    ButtonState, CursorMoved, KeyboardInput, MouseButtonInput, MouseMotion, MouseWheel,
};
use crate::window::{
    CursorGrab, CursorOptions, PrimaryWindow, WindowCloseRequested, WindowCreated, WindowFocused,
    WindowMode, WindowOccluded, WindowResized, WindowSettings, WindowState,
};
use boxel_app::{App, AppExit, PluginsState};
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

/// Update pacing of the winit runner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateMode {
    /// `ControlFlow::Poll`: update as fast as presentation allows (games).
    #[default]
    Continuous,
    /// `ControlFlow::WaitUntil(now + wait)` between updates (tools).
    Reactive {
        /// Max time between updates.
        wait: Duration,
    },
}

/// A requested redraw that has not arrived after this long marks the window occluded.
const REDRAW_TIMEOUT: Duration = Duration::from_millis(100);
/// Time between updates while the window is occluded (30 Hz).
const OCCLUDED_WAIT: Duration = Duration::from_millis(33);
/// Time between `Plugin::ready` polls while plugins are still being set up.
const PLUGINS_POLL: Duration = Duration::from_millis(1);
/// Time between `Window::is_minimized` polls (X11 only; each poll is a server round trip).
const MINIMIZED_POLL: Duration = Duration::from_millis(100);
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
///    Control flow: `Poll` (`Continuous`), `WaitUntil(now + wait)` (`Reactive`), and
///    `WaitUntil(now + 33 ms)` while occluded. Updates are never driven by `RedrawRequested`.
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
pub fn winit_runner(mut app: App, event_loop: EventLoop<()>) -> AppExit {
    let settings = app
        .world()
        .get_resource::<RunnerSettings>()
        .copied()
        .unwrap_or_default();
    app.world_mut().init_resource::<WindowState>();
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
        self.core.begin_update(self.app.world_mut(), now);
        self.app.update();
        if let Some(exit) = self.app.should_exit() {
            tracing::debug!("exit requested: {exit:?}");
            self.exit = Some(exit);
            event_loop.exit();
            return;
        }
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
/// redraw protocol and the occlusion watchdog. Kept apart so it is testable without a display.
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
    /// The cursor state last applied to the window (the OS default at creation).
    applied_cursor: CursorOptions,
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
            applied_cursor: CursorOptions {
                grab: CursorGrab::None,
                visible: true,
            },
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
            WindowEvent::RedrawRequested => {
                self.redraw_pending_since = None;
                update_state(world, |state| state.frame_ready = true);
            }
            _ => {}
        }
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

    /// Before an update: publish occlusion (winit's report, the polled minimized state or the
    /// redraw watchdog).
    fn begin_update(&mut self, world: &mut World, now: Instant) {
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
    }

    /// After an update: request the next redraw if the presentation gate was consumed, and pick
    /// the pacing.
    fn end_update(&mut self, world: &World, now: Instant) -> EndOfUpdate {
        let frame_ready = world
            .get_resource::<WindowState>()
            .is_some_and(|state| state.frame_ready);
        let request_redraw = !frame_ready && self.redraw_pending_since.is_none();
        if request_redraw {
            self.redraw_pending_since = Some(now);
        }
        let control_flow = match (self.settings.update_mode, self.occluded) {
            (UpdateMode::Continuous, false) => ControlFlow::Poll,
            (UpdateMode::Continuous, true) => wait_until(now, OCCLUDED_WAIT),
            (UpdateMode::Reactive { wait }, false) => wait_until(now, wait),
            (UpdateMode::Reactive { wait }, true) => wait_until(now, wait.max(OCCLUDED_WAIT)),
        };
        EndOfUpdate {
            request_redraw,
            control_flow,
        }
    }
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
        assert_eq!(
            end.control_flow,
            ControlFlow::WaitUntil(late + OCCLUDED_WAIT)
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
        assert_eq!(end.control_flow, ControlFlow::WaitUntil(t0 + OCCLUDED_WAIT));

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
