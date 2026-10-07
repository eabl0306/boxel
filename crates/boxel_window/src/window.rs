use crate::runner::{FramePacingInfo, RunnerSettings, UpdateMode, winit_runner};
use boxel_app::{App, FrameLimit, Plugin};
use boxel_ecs::prelude::*;
use boxel_math::UVec2;
use std::sync::Arc;
use winit::event_loop::{EventLoop, OwnedDisplayHandle};
use winit::window::{Window, WindowId};

/// Fullscreen policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowMode {
    /// Normal window.
    #[default]
    Windowed,
    /// Borderless fullscreen on the current monitor.
    BorderlessFullscreen,
}

/// Primary window description (logical size).
#[derive(Resource, Clone, Debug)]
pub struct WindowSettings {
    /// Title.
    pub title: String,
    /// Logical width.
    pub width: u32,
    /// Logical height.
    pub height: u32,
    /// User resizable.
    pub resizable: bool,
    /// Fullscreen policy.
    pub mode: WindowMode,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            title: "boxel".to_string(),
            width: 1280,
            height: 720,
            resizable: true,
            mode: WindowMode::Windowed,
        }
    }
}

/// Creates the event loop, registers window messages and installs the winit runner.
/// Must be added from the main thread (winit refuses to create an event loop elsewhere on
/// Linux). Panics with a clear message if an event loop cannot be created (no display).
///
/// Also inserts the default [`FrameLimit`] (144 fps) unless the app already has one; insert
/// your own (before or after this plugin) to change it, at startup or at runtime.
#[derive(Clone, Debug)]
pub struct WindowPlugin {
    /// Primary window settings (inserted as a resource).
    pub primary: WindowSettings,
    /// Write `AppExit::Success` when the primary window is closed (default true).
    pub exit_on_close: bool,
    /// Update pacing.
    pub update_mode: UpdateMode,
}

impl Default for WindowPlugin {
    fn default() -> Self {
        Self {
            primary: WindowSettings::default(),
            exit_on_close: true,
            update_mode: UpdateMode::Continuous,
        }
    }
}

impl Plugin for WindowPlugin {
    fn build(&self, app: &mut App) {
        let event_loop = EventLoop::new().unwrap_or_else(|err| {
            panic!(
                "boxel_window: cannot create the winit event loop: {err}. WindowPlugin needs a \
                 display (Wayland or X11 on Linux), must be added from the main thread and only \
                 once per process"
            )
        });
        app.insert_resource(DisplayHandle(event_loop.owned_display_handle()))
            .insert_resource(self.primary.clone())
            .insert_resource(WindowState::default())
            .init_resource::<FramePacingInfo>()
            .insert_resource(CursorOptions {
                grab: CursorGrab::None,
                visible: true,
            })
            .insert_resource(RunnerSettings {
                exit_on_close: self.exit_on_close,
                update_mode: self.update_mode,
            })
            .init_resource::<FrameLimit>()
            .add_message::<WindowCreated>()
            .add_message::<WindowResized>()
            .add_message::<WindowCloseRequested>()
            .add_message::<WindowFocused>()
            .add_message::<WindowOccluded>()
            .set_runner(move |app| winit_runner(app, event_loop));
    }
}

/// The winit display handle, inserted by [`WindowPlugin::build`]. Clone it into
/// `wgpu::InstanceDescriptor::new_with_display_handle(Box::new(handle.0.clone()))`.
#[derive(Resource, Clone, Debug)]
pub struct DisplayHandle(pub OwnedDisplayHandle);

/// The primary window. A NON-SEND resource: the runner inserts it in `resumed` with
/// `App::insert_non_send` and removes it in `exiting`. Read it with `NonSend<PrimaryWindow>`;
/// the executor then runs that system on the main thread.
///
/// Why non-send although `Window` is `Send + Sync`: on macOS every winit window method called
/// off the main thread is a synchronous dispatch to the main queue, and on Windows several
/// (`set_cursor_grab`, `set_cursor_visible`, ...) wait for the event-loop thread. The main
/// thread is blocked inside `App::update` while worker threads run systems, so such a call from
/// a parallel system deadlocks. On macOS `window_handle()` is also unavailable off the main
/// thread, so surface creation must stay there. Engine code changes the window only from the
/// runner (`CursorOptions`) or from main-thread systems.
#[derive(Clone, Debug)]
pub struct PrimaryWindow {
    /// The winit window (`Surface<'static>` is created from this `Arc` on the main thread).
    pub window: Arc<Window>,
    /// Its id.
    pub id: WindowId,
}

/// Live window state, maintained by the runner.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowState {
    /// Size in physical pixels: the last non-zero size (a 0x0 size, reported while minimized
    /// on some platforms, is ignored, so this is never 0 once the window exists). On Wayland
    /// it also follows `Window::request_inner_size`, which winit applies without a `Resized`
    /// event (the runner polls `Window::inner_size` there).
    pub physical_size: UVec2,
    /// DPI scale factor.
    pub scale_factor: f64,
    /// Keyboard focus.
    pub focused: bool,
    /// Not visible. True when winit reported `Occluded(true)` (macOS; X11 when the window is
    /// fully covered), OR when the window is minimized on X11 (polled with
    /// `Window::is_minimized` at most every 100 ms: X11 sends no `Occluded` on minimize and
    /// delivers `RedrawRequested` at once), OR when a redraw was requested and no
    /// `RedrawRequested` arrived for more than 100 ms (Wayland and Windows, where winit 0.30
    /// never reports occlusion: a hidden Wayland surface gets no frame callbacks). The runner
    /// paces updates at 30 Hz while true.
    pub occluded: bool,
    /// Presentation gate. The runner sets it when winit delivers `RedrawRequested` (on Wayland
    /// only after the compositor's frame callback for the last presented frame). A renderer
    /// acquires and presents a surface texture ONLY while it is true: it calls
    /// `Window::pre_present_notify` right before presenting and then sets it to false; the
    /// runner requests the next redraw after the update. This keeps FIFO presentation from
    /// blocking the main thread (and freezing the simulation) on a hidden Wayland window.
    /// Apps without a renderer can ignore it.
    pub frame_ready: bool,
}

/// Desired cursor state; the runner applies changes before each update. `Locked` falls back to
/// `Confined` where unsupported (X11).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorOptions {
    /// Grab mode.
    pub grab: CursorGrab,
    /// Cursor visible.
    pub visible: bool,
}

/// Cursor grab mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorGrab {
    /// Free cursor.
    #[default]
    None,
    /// Locked in place (relative mouse via `MouseMotion`).
    Locked,
}

/// The primary window was created (once per process in M0).
#[derive(Message, Clone, Copy, Debug)]
pub struct WindowCreated;

/// The surface size changed (physical pixels, never 0x0: minimization is not reported here).
/// Written only when the size really differs from `WindowState::physical_size`; a size the
/// compositor overrides before the runner sees it (for example a `request_inner_size` during
/// the initial Wayland configure sequence) produces no message.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowResized {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
}

/// The user asked to close the window.
#[derive(Message, Clone, Copy, Debug)]
pub struct WindowCloseRequested;

/// Focus changed.
#[derive(Message, Clone, Copy, Debug)]
pub struct WindowFocused(pub bool);

/// `WindowState::occluded` changed (winit occlusion, the X11 minimized poll or the redraw
/// watchdog).
#[derive(Message, Clone, Copy, Debug)]
pub struct WindowOccluded(pub bool);
