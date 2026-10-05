//! `boxel_window`: the winit 0.30 runner, the primary window and input.
//!
//! - [`WindowPlugin`] creates the `EventLoop` in `build` (main thread), inserts [`DisplayHandle`]
//!   (needed by the renderer to create the wgpu `Instance`, mandatory for GL on Wayland), and
//!   installs [`winit_runner`].
//! - The runner creates the window in `resumed`, inserts the non-send [`PrimaryWindow`] and
//!   drives `App::update()` from `about_to_wait` (never from `RedrawRequested`, so the
//!   simulation does not freeze while the window is hidden on Wayland). `RedrawRequested` only
//!   opens the presentation gate [`WindowState::frame_ready`] (see [`winit_runner`]).
//! - Window and device events become messages; [`InputPlugin`] folds them into [`ButtonInput`]
//!   and accumulated mouse resources in `PreUpdate`.
// SKELETON: remove this attribute when the crate is fully implemented (owner: WP2).
#![allow(
    unused_variables,
    unused_mut,
    dead_code,
    clippy::todo,
    reason = "skeleton stubs"
)]

mod input;
mod runner;
mod window;

pub use input::{
    AccumulatedMouseMotion, AccumulatedMouseScroll, ButtonInput, ButtonState, CursorMoved,
    InputPlugin, InputSystems, KeyboardInput, MouseButtonInput, MouseMotion, MouseWheel,
    accumulate_mouse_motion_system, accumulate_mouse_scroll_system, keyboard_input_system,
    mouse_button_input_system,
};
pub use runner::{UpdateMode, winit_runner};
pub use window::{
    CursorGrab, CursorOptions, DisplayHandle, PrimaryWindow, WindowCloseRequested, WindowCreated,
    WindowFocused, WindowMode, WindowOccluded, WindowPlugin, WindowResized, WindowSettings,
    WindowState,
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
        InputPlugin, KeyCode, MouseButton, PrimaryWindow, WindowPlugin, WindowSettings,
        WindowState,
    };
}
