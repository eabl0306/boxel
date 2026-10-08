use boxel_ecs::prelude::*;
use std::time::{Duration, Instant};

/// Default [`FrameLimit::max_fps`].
pub const DEFAULT_MAX_FPS: u32 = 144;

/// Maximum number of app updates (frames) per second. The runners read it before every frame,
/// so a change made by any system (a settings menu, a debug key) paces the very next frame.
///
/// - `max_fps = 0` means unlimited: the runner starts the next frame as soon as the previous
///   one ends.
/// - The default is [`DEFAULT_MAX_FPS`] (144). `boxel_window::WindowPlugin` inserts it with
///   `init_resource`, so a value the app inserts (before or after the plugin) wins.
/// - The headless runner (`ScheduleRunnerPlugin` in `RunMode::Loop`) uses it instead of its
///   `wait` when the resource exists.
///
/// What it limits is the update rate: simulation, input and rendering all run once per update.
/// The frames shown on screen can be fewer:
/// - Vsync (`Fifo` presentation) never shows more frames than the display refresh rate. On X11
///   (and, not measured yet, Windows) it also blocks the update when the swapchain is full, so
///   the update rate drops to the refresh rate when the limit is higher.
/// - On Wayland the runner presents only after the compositor's frame callback (the
///   presentation gate, `WindowState::frame_ready`), with or without vsync, so shown frames
///   are at most the refresh rate.
/// - On Wayland a limit at or above 99 % of the monitor's refresh rate follows the display
///   (ADR 0017): updates run when the compositor asks for a frame, so the update rate is the
///   refresh rate (about 143.9 on a 143.88 Hz panel with a limit of 143, 144 or 240) and every
///   update presents unless the compositor skips a refresh. Above the refresh rate the limit
///   then changes nothing; only 0 (unlimited) runs more updates than the display shows.
/// - A lower limit keeps its own grid. A due update whose gate is still closed waits a little
///   for the callback and the pacing restarts there, so more updates present. The cost: with a
///   renderer the update rate can land up to about 2 % under the limit (measured on a 144 Hz
///   panel: 119 at 120, 127.5 at 130; 60 and 100 exact), and shown frames can be a few percent
///   under the limit (111 to 119 at 120).
/// - While the window is hidden the runner caps updates at 30 per second (a lower limit stays).
///
/// Pacing is deadline based ([`FramePacer`]): each frame is due one period after the previous
/// one was due, so wake-up jitter does not drift the rate, and a frame late by more than one
/// period restarts the grid instead of running a burst of catch-up frames.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameLimit {
    /// Frames per second; 0 = unlimited.
    pub max_fps: u32,
}

impl Default for FrameLimit {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_FPS)
    }
}

impl FrameLimit {
    /// No limit.
    pub const UNLIMITED: FrameLimit = FrameLimit { max_fps: 0 };

    /// At most `max_fps` frames per second (0 = unlimited).
    pub const fn new(max_fps: u32) -> Self {
        Self { max_fps }
    }

    /// True for `max_fps == 0`.
    pub const fn is_unlimited(self) -> bool {
        self.max_fps == 0
    }

    /// Time between frame starts, rounded to the nanosecond (`None` = unlimited). 144 fps gives
    /// 6 944 444 ns.
    pub fn period(self) -> Option<Duration> {
        if self.max_fps == 0 {
            return None;
        }
        let fps = u64::from(self.max_fps);
        let nanos = (1_000_000_000 + fps / 2) / fps;
        (nanos > 0).then(|| Duration::from_nanos(nanos))
    }
}

/// The pacing of the current frame, for diagnostics (overlays, benchmarks): when the next frame
/// is due and how long the previous update took. Both runners write it right before each update.
/// Without a limit (or a runner that paces) `next` is `None`. No engine system reads it: sizing
/// per-frame work to the time left at 144 Hz did not help the world catch up (issue #3).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameDeadline {
    /// When the next frame is due (`None`: unlimited, or no pacing runner).
    pub next: Option<Instant>,
    /// The period that deadline was set with: the [`FrameLimit`] period, the display's while
    /// the winit runner follows it, at least 33 ms while the window is hidden (`None` with
    /// `next`).
    pub period: Option<Duration>,
    /// How long the previous update took, start to end (zero before the first).
    pub last_update: Duration,
}

impl FrameDeadline {
    /// The deadline after a frame that `pacer` just started under `period`, following an
    /// update that took `last_update`.
    pub fn after_start(
        pacer: &FramePacer,
        period: Option<Duration>,
        last_update: Duration,
    ) -> Self {
        let next = pacer.next_frame(period);
        Self {
            next,
            period: next.and(period),
            last_update,
        }
    }
}

/// What a runner does before the next frame (see [`FramePacer::wait`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameWait {
    /// The frame is due: run it now.
    Ready,
    /// More than the spin margin is left: block (sleep, or `WaitUntil` in an event loop) until
    /// `wake`, which is the deadline minus the spin margin, then ask again.
    Sleep {
        /// When to wake up.
        wake: Instant,
    },
    /// Less than the spin margin is left: yield in a loop until `until` (the deadline), then run
    /// the frame ([`spin_until`]).
    Spin {
        /// The deadline.
        until: Instant,
    },
}

/// Deadline-based frame pacing, with the clock supplied by the caller (tests drive it with
/// synthetic instants).
///
/// A runner calls [`FramePacer::start_frame`] right before each update and asks
/// [`FramePacer::wait`] what to do until the next one. The pacer keeps the nominal start of the
/// current frame (its anchor):
/// - the next frame is due at `anchor + period`, with the period of that moment, so a limit
///   changed during a frame paces the very next one;
/// - a frame that starts at most one period after it was due keeps the grid (anchor = due
///   time): late wake-ups are absorbed and the average rate stays exact, with no drift;
/// - a frame that starts more than one period late re-anchors on its real start: no burst of
///   catch-up frames after a stall;
/// - an unlimited frame (`period = None`) anchors on its real start, so a limit set later
///   starts counting from there.
///
/// The spin margin splits the wait: the coarse part sleeps, the last `spin_margin` yields in a
/// loop, because OS timers wake up late by tens of microseconds (Linux timer slack is 50 us)
/// and more under load. The grid keeps the average rate exact either way; the spin keeps each
/// frame on time. The default, [`FramePacer::DEFAULT_SPIN_MARGIN`], costs 3 to 4 % of a core
/// at 144 fps (measured with `voxel_world`: 20 % of a core for the whole process against 16 to
/// 17 % without spinning).
#[derive(Clone, Debug)]
pub struct FramePacer {
    anchor: Option<Instant>,
    spin_margin: Duration,
}

impl Default for FramePacer {
    fn default() -> Self {
        Self::new()
    }
}

impl FramePacer {
    /// Default spin margin (300 us).
    pub const DEFAULT_SPIN_MARGIN: Duration = Duration::from_micros(300);

    /// A pacer with no frame yet (the first frame is always ready) and the default spin margin.
    pub const fn new() -> Self {
        Self::with_spin_margin(Self::DEFAULT_SPIN_MARGIN)
    }

    /// A pacer with a custom spin margin (zero: never spin).
    pub const fn with_spin_margin(spin_margin: Duration) -> Self {
        Self {
            anchor: None,
            spin_margin,
        }
    }

    /// The spin margin.
    pub fn spin_margin(&self) -> Duration {
        self.spin_margin
    }

    /// Nominal start of the latest frame (`None` before the first).
    pub fn anchor(&self) -> Option<Instant> {
        self.anchor
    }

    /// Forgets the grid: the next frame is ready at once.
    pub fn reset(&mut self) {
        self.anchor = None;
    }

    /// Moves the grid: the current frame counts as started at `at`, so the next one is due one
    /// period later. A runner uses it to follow an external clock, such as the compositor's
    /// frame callbacks, when a frame waited for one.
    pub fn reanchor(&mut self, at: Instant) {
        self.anchor = Some(at);
    }

    /// Records that a frame starts at `now` under `period` (`None` = unlimited). See the type
    /// docs for the anchoring rules.
    pub fn start_frame(&mut self, now: Instant, period: Option<Duration>) {
        let due = self.next_frame(period);
        self.anchor = Some(match (due, period) {
            (Some(due), Some(period)) if now >= due && now - due <= period => due,
            _ => now,
        });
    }

    /// When the next frame is due under `period` (`None`: no frame yet, unlimited, or not
    /// representable).
    pub fn next_frame(&self, period: Option<Duration>) -> Option<Instant> {
        self.anchor?.checked_add(period?)
    }

    /// What to do at `now` before the next frame, under `period`.
    pub fn wait(&self, now: Instant, period: Option<Duration>) -> FrameWait {
        match self.next_frame(period) {
            Some(due) if due > now => match due.checked_sub(self.spin_margin) {
                Some(wake) if wake > now => FrameWait::Sleep { wake },
                _ => FrameWait::Spin { until: due },
            },
            _ => FrameWait::Ready,
        }
    }

    /// Blocks the calling thread until the next frame is due under `period`: sleeps for all
    /// but the spin margin, then spins (the headless runner; an event loop uses
    /// [`FramePacer::wait`] instead, so it keeps handling events while it waits).
    pub fn block_until_due(&self, period: Option<Duration>) {
        if let Some(due) = self.next_frame(period) {
            if let Some(wake) = due.checked_sub(self.spin_margin) {
                sleep_until(wake);
            }
            spin_until(due);
        }
    }
}

/// Yields the thread in a loop until `deadline` (the last fraction of a millisecond of a frame
/// wait, where a sleep would wake up too late).
pub fn spin_until(deadline: Instant) {
    while Instant::now() < deadline {
        std::thread::yield_now();
    }
}

/// Sleeps until `deadline` (returns at once if it passed).
#[expect(clippy::disallowed_methods, reason = "runner frame pacing")]
fn sleep_until(deadline: Instant) {
    let left = deadline.saturating_duration_since(Instant::now());
    if !left.is_zero() {
        std::thread::sleep(left);
    }
}
