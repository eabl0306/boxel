use boxel_app::{App, AppExit};
use std::time::Duration;
use winit::event_loop::EventLoop;

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
pub fn winit_runner(app: App, event_loop: EventLoop<()>) -> AppExit {
    todo!("WP2")
}
