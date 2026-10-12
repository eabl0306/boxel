//! `boxel_window`: the winit 0.30 runner, the primary window and input.
//!
//! - [`WindowPlugin`] creates the `EventLoop` in `build` (main thread), inserts [`DisplayHandle`]
//!   (needed by the renderer to create the wgpu `Instance`, mandatory for GL on Wayland), and
//!   installs [`winit_runner`].
//! - The runner creates the window in `resumed`, inserts the non-send [`PrimaryWindow`] and
//!   drives `App::update()` from `about_to_wait` (never from `RedrawRequested`, so the
//!   simulation does not freeze while the window is hidden on Wayland). `RedrawRequested` only
//!   opens the presentation gate [`WindowState::frame_ready`] (see [`winit_runner`]).
//! - Window and device events become messages; [`InputPlugin`] folds them into [`ButtonInput`],
//!   accumulated mouse resources and [`CursorPosition`] in `PreUpdate`. Typed text arrives as
//!   [`TextInput`] (layout and dead keys applied) and input method composition as [`ImeEvent`],
//!   enabled through [`ImeOptions`].
//! - Updates are paced by `boxel_app::FrameLimit` (default 144 fps, 0 = unlimited, changeable
//!   at runtime), see [`UpdateMode`]. On Wayland a limit at or above the monitor's refresh rate
//!   follows the display: updates run when the compositor asks for a frame (see
//!   [`winit_runner`]). [`FramePacingInfo`] tells how each update was paced.
mod input;
mod runner;
mod window;

pub use input::{
    AccumulatedMouseMotion, AccumulatedMouseScroll, ButtonInput, ButtonState, CursorLeft,
    CursorMoved, CursorPosition, ImeEvent, InputPlugin, InputSystems, KeyboardInput,
    MouseButtonInput, MouseMotion, MouseWheel, TextInput, accumulate_mouse_motion_system,
    accumulate_mouse_scroll_system, cursor_position_system, keyboard_input_system,
    mouse_button_input_system,
};
pub use runner::{FramePacingInfo, UpdateMode, winit_runner};
pub use window::{
    CursorGrab, CursorOptions, DisplayHandle, ImeCursorArea, ImeOptions, PrimaryWindow,
    WindowCloseRequested, WindowCreated, WindowFocused, WindowMode, WindowOccluded, WindowPlugin,
    WindowResized, WindowSettings, WindowState,
};

pub use winit;
/// Mouse buttons (re-exported from winit).
pub use winit::event::MouseButton;
/// Physical key codes (re-exported from winit).
pub use winit::keyboard::KeyCode;

/// Common items.
pub mod prelude {
    pub use crate::{
        AccumulatedMouseMotion, AccumulatedMouseScroll, ButtonInput, CursorGrab, CursorOptions,
        CursorPosition, InputPlugin, KeyCode, MouseButton, PrimaryWindow, WindowPlugin,
        WindowSettings, WindowState,
    };
}
