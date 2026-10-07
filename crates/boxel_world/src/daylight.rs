//! Day and night (ADR 0015, phase P5): the time of day and the sky light it gives.
//!
//! Only the renderer's scene light changes with the time: meshes keep their baked sky level, so
//! nothing is remeshed or uploaded. [`TimeOfDay`] advances one tick per fixed step
//! (`FixedUpdate`, 64 Hz), so the clock is the same at any frame rate. [`DaylightCurve`] maps the
//! sun's elevation to the sky light color and intensity, the ambient floor and the sky (clear
//! and fog) color; the render plugin reads both every frame.

use boxel_app::{App, FixedUpdate, Plugin};
use boxel_ecs::prelude::*;
use std::f64::consts::TAU;

/// Fixed steps per second (`boxel_app::FixedTime`'s default 64 Hz).
pub const TICKS_PER_SECOND: u64 = 64;

/// Ticks of the default day: 20 minutes of fixed steps (76 800).
pub const DEFAULT_TICKS_PER_DAY: u64 = 20 * 60 * TICKS_PER_SECOND;

/// Clock hour of tick 0 of every day (sunrise, as in Minecraft).
pub const SUNRISE_HOUR: f64 = 6.0;

/// The world clock, advanced by one [`TimeOfDay::speed`] per fixed step unless paused.
///
/// A day is [`TimeOfDay::ticks_per_day`] ticks. Within it, tick 0 is sunrise (06:00), a quarter
/// day is noon, half a day is sunset (18:00) and three quarters is midnight.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeOfDay {
    /// Ticks since the first sunrise.
    pub tick: u64,
    /// Ticks per day (at least 1; [`DEFAULT_TICKS_PER_DAY`]: 20 minutes).
    pub ticks_per_day: u64,
    /// Stops the clock (the scene light stays as it is).
    pub paused: bool,
    /// Ticks per fixed step: 1 is real time, 60 makes a 20-minute day last 20 seconds.
    pub speed: u64,
}

impl Default for TimeOfDay {
    /// Noon of the first day, 20-minute days, running at real time.
    fn default() -> Self {
        Self::at_clock(12.0)
    }
}

impl TimeOfDay {
    /// The first day at clock `hours` (0 to 24; 6 is sunrise, 12 noon), 20-minute days, running.
    pub fn at_clock(hours: f64) -> Self {
        let mut time = Self {
            tick: 0,
            ticks_per_day: DEFAULT_TICKS_PER_DAY,
            paused: false,
            speed: 1,
        };
        time.set_clock(hours);
        time
    }

    /// Ticks of one day (`ticks_per_day`, at least 1).
    fn day_len(&self) -> u64 {
        self.ticks_per_day.max(1)
    }

    /// Days since the first sunrise.
    pub fn day(&self) -> u64 {
        self.tick / self.day_len()
    }

    /// Ticks since this day's sunrise.
    pub fn tick_of_day(&self) -> u64 {
        self.tick % self.day_len()
    }

    /// Fraction of the day since sunrise, `0..1`: 0.25 is noon, 0.5 sunset, 0.75 midnight.
    pub fn day_fraction(&self) -> f64 {
        self.tick_of_day() as f64 / self.day_len() as f64
    }

    /// Clock hours, `0..24` (sunrise at 6, noon at 12).
    pub fn clock(&self) -> f64 {
        (self.day_fraction() * 24.0 + SUNRISE_HOUR).rem_euclid(24.0)
    }

    /// Moves to clock `hours` of the current day (wrapped to `0..24`): the first tick at or
    /// after it, so [`TimeOfDay::clock_text`] shows the minute that was set.
    pub fn set_clock(&mut self, hours: f64) {
        let since_sunrise = (hours - SUNRISE_HOUR).rem_euclid(24.0) / 24.0;
        let len = self.day_len();
        let tick = ((since_sunrise * len as f64).ceil() as u64).min(len - 1);
        self.tick = self.day() * len + tick;
    }

    /// Moves the time of day by `ticks` (negative goes back), wrapping around midnight within the
    /// current day: the day count never changes.
    pub fn scrub(&mut self, ticks: i64) {
        let len = self.day_len();
        let len_i = i128::from(len);
        let time = (i128::from(self.tick_of_day()) + i128::from(ticks)).rem_euclid(len_i);
        self.tick = self.day() * len + time as u64;
    }

    /// Ticks per clock hour.
    pub fn ticks_per_hour(&self) -> u64 {
        (self.day_len() / 24).max(1)
    }

    /// One fixed step: `tick += speed` unless paused.
    pub fn advance(&mut self) {
        if !self.paused {
            self.tick = self.tick.wrapping_add(self.speed);
        }
    }

    /// Sine of the sun's elevation: 0 at sunrise and sunset, 1 at noon, -1 at midnight.
    pub fn sun_elevation(&self) -> f64 {
        (self.day_fraction() * TAU).sin()
    }

    /// `HH:MM` of [`TimeOfDay::clock`], the minute rounded down (exact: integer ticks).
    pub fn clock_text(&self) -> String {
        const DAY_MINUTES: u128 = 24 * 60;
        let since_sunrise =
            u128::from(self.tick_of_day()) * DAY_MINUTES / u128::from(self.day_len());
        let minutes = (since_sunrise + SUNRISE_HOUR as u128 * 60) % DAY_MINUTES;
        format!("{:02}:{:02}", minutes / 60, minutes % 60)
    }
}

/// The sky at one point of the day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyKey {
    /// Color of the sky light on surfaces (linear RGB, brightest channel 1).
    pub tint: [f32; 3],
    /// Intensity of the sky light (1 at noon).
    pub intensity: f32,
    /// The sky itself: the clear color behind the terrain and the fog color (linear RGB).
    pub sky_color: [f32; 3],
}

impl SkyKey {
    /// `tint * intensity`.
    pub fn light(&self) -> [f32; 3] {
        self.tint.map(|c| c * self.intensity)
    }
}

/// What the renderer needs from the time of day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Daylight {
    /// Sky light on surfaces: tint times intensity (linear RGB). It multiplies the baked sky
    /// level; block light does not depend on it.
    pub sky_light: [f32; 3],
    /// Ambient floor (the same day and night: caves stay almost black).
    pub ambient: f32,
    /// Clear and fog color (linear RGBA).
    pub sky_color: [f32; 4],
}

/// Sun elevation to sky light, defaults from the owner's decisions (ADR 0015): a white day at
/// full intensity, a warm dusk at about 0.6 and a blue night at 0.2, with a cave ambient floor of
/// 0.02 at every hour.
///
/// The sky is [`DaylightCurve::day`] while the sun's elevation (its sine) is at least
/// `day_elevation`, [`DaylightCurve::night`] at or below `night_elevation`, and blends through
/// [`DaylightCurve::dusk`] at the horizon (smoothstep on each side). Dawn and dusk are the same.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct DaylightCurve {
    /// Full day.
    pub day: SkyKey,
    /// Sun on the horizon.
    pub dusk: SkyKey,
    /// Full night.
    pub night: SkyKey,
    /// Sun elevation (sine) from which it is full day.
    pub day_elevation: f32,
    /// Sun elevation (sine, negative) below which it is full night.
    pub night_elevation: f32,
    /// Ambient floor at every hour.
    pub ambient: f32,
}

impl Default for DaylightCurve {
    fn default() -> Self {
        Self {
            day: SkyKey {
                tint: [1.0, 1.0, 1.0],
                intensity: 1.0,
                sky_color: [0.30, 0.52, 0.85],
            },
            dusk: SkyKey {
                tint: [1.0, 0.75, 0.55],
                intensity: 0.6,
                sky_color: [0.72, 0.42, 0.26],
            },
            // The plan's night light (0.30, 0.35, 0.55), scaled so its brightest channel is 1,
            // at 20 % intensity.
            night: SkyKey {
                tint: [0.30 / 0.55, 0.35 / 0.55, 1.0],
                intensity: 0.2,
                sky_color: [0.012, 0.018, 0.045],
            },
            day_elevation: 0.25,
            night_elevation: -0.2,
            ambient: 0.02,
        }
    }
}

impl DaylightCurve {
    /// The sky for a sun elevation (sine, `-1..=1`).
    pub fn sample(&self, sun_elevation: f64) -> Daylight {
        let e = sun_elevation as f32;
        let (from, to, t) = if e >= 0.0 {
            (
                &self.dusk,
                &self.day,
                smoothstep(0.0, self.day_elevation, e),
            )
        } else {
            (
                &self.night,
                &self.dusk,
                smoothstep(self.night_elevation, 0.0, e),
            )
        };
        // Exact at both ends: full day is exactly the day key.
        let mix3 = |a: [f32; 3], b: [f32; 3]| std::array::from_fn(|i| a[i] * (1.0 - t) + b[i] * t);
        let [r, g, b] = mix3(from.sky_color, to.sky_color);
        Daylight {
            sky_light: mix3(from.light(), to.light()),
            ambient: self.ambient,
            sky_color: [r, g, b, 1.0],
        }
    }

    /// The sky at `time`.
    pub fn at(&self, time: &TimeOfDay) -> Daylight {
        self.sample(time.sun_elevation())
    }
}

/// 0 at `edge0`, 1 at `edge1`, smooth in between (clamped outside).
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Adds the world clock: [`TimeOfDay`] (noon of day one, unless the app inserted one) and
/// [`DaylightCurve`] (defaults, unless inserted), and advances the clock in `FixedUpdate`. The
/// render plugin turns them into the scene light; without this plugin the sky stays at noon.
#[derive(Clone, Copy, Debug, Default)]
pub struct DaylightPlugin;

impl Plugin for DaylightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TimeOfDay>()
            .init_resource::<DaylightCurve>()
            .add_systems(FixedUpdate, advance_time);
    }
}

/// One tick per fixed step.
fn advance_time(mut time: ResMut<TimeOfDay>) {
    if !time.paused && time.speed > 0 {
        time.advance();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_app::FixedMain;

    #[test]
    fn a_day_is_twenty_minutes_of_fixed_steps() {
        assert_eq!(DEFAULT_TICKS_PER_DAY, 76_800);
        let noon = TimeOfDay::default();
        assert_eq!(noon.tick, DEFAULT_TICKS_PER_DAY / 4);
        assert_eq!((noon.clock_text(), noon.day()), ("12:00".to_string(), 0));
        assert!((noon.sun_elevation() - 1.0).abs() < 1e-12);
        let midnight = TimeOfDay::at_clock(0.0);
        assert_eq!(midnight.tick, DEFAULT_TICKS_PER_DAY * 3 / 4);
        assert!((midnight.sun_elevation() + 1.0).abs() < 1e-12);
        for (hours, text) in [
            (6.0, "06:00"),
            (18.5, "18:30"),
            (23.99, "23:59"),
            (30.0, "06:00"),
        ] {
            assert_eq!(TimeOfDay::at_clock(hours).clock_text(), text, "{hours}");
        }
        assert_eq!(TimeOfDay::at_clock(6.0).tick, 0);
        assert_eq!(noon.ticks_per_hour(), 3200);
    }

    /// `--time HH:MM` shows `HH:MM`: every minute of the day.
    #[test]
    fn every_minute_shows_as_set() {
        for minute in 0..24 * 60_u32 {
            let (h, m) = (minute / 60, minute % 60);
            let time = TimeOfDay::at_clock(f64::from(h) + f64::from(m) / 60.0);
            assert_eq!(time.clock_text(), format!("{h:02}:{m:02}"));
            assert!(time.clock() * 60.0 >= f64::from(minute) - 1e-6, "{h}:{m}");
        }
    }

    #[test]
    fn scrubbing_wraps_within_the_day() {
        let mut time = TimeOfDay::at_clock(7.0);
        time.scrub(-2 * time.ticks_per_hour() as i64);
        assert_eq!((time.clock_text(), time.day()), ("05:00".to_string(), 0));
        time.tick += 3 * time.ticks_per_day;
        time.scrub(20 * time.ticks_per_hour() as i64);
        assert_eq!((time.clock_text(), time.day()), ("01:00".to_string(), 3));
        time.set_clock(12.0);
        assert_eq!((time.clock_text(), time.day()), ("12:00".to_string(), 3));
        // A degenerate day length never divides by zero.
        let mut odd = TimeOfDay {
            ticks_per_day: 0,
            ..TimeOfDay::default()
        };
        odd.scrub(-5);
        odd.advance();
        assert_eq!(odd.tick_of_day(), 0);
    }

    /// The clock only depends on the fixed steps run: the same steps give the same ticks, a
    /// paused clock stays, and `speed` ticks pass per step.
    #[test]
    fn ticks_are_deterministic() {
        let run = |steps: u32, setup: fn(&mut TimeOfDay)| {
            let mut app = App::new();
            app.add_plugins(DaylightPlugin);
            setup(&mut app.world_mut().resource_mut::<TimeOfDay>());
            for _ in 0..steps {
                app.world_mut().run_schedule(FixedMain);
            }
            *app.world().resource::<TimeOfDay>()
        };
        let start = TimeOfDay::default().tick;
        let a = run(640, |_| {});
        assert_eq!(a.tick, start + 640);
        assert_eq!(run(640, |_| {}), a);
        assert_eq!(run(640, |t| t.paused = true).tick, start);
        assert_eq!(run(10, |t| t.speed = 60).tick, start + 600);
        // A whole day of ticks comes back to the same time, one day later.
        let day = run((DEFAULT_TICKS_PER_DAY / 64) as u32, |t| t.speed = 64);
        assert_eq!((day.tick_of_day(), day.day()), (start, 1));
    }

    #[test]
    fn daylight_follows_the_sun() {
        let curve = DaylightCurve::default();
        let noon = curve.at(&TimeOfDay::at_clock(12.0));
        assert_eq!(noon.sky_light, [1.0; 3]);
        assert_eq!(noon.sky_color, [0.30, 0.52, 0.85, 1.0]);
        let night = curve.at(&TimeOfDay::at_clock(0.0));
        // 20 % with a blue tint
        let [r, g, b] = night.sky_light;
        assert!((b - 0.2).abs() < 1e-6 && r < g && g < b, "{night:?}");
        let dusk = curve.sample(0.0);
        assert!((dusk.sky_light[0] - 0.6).abs() < 1e-6, "{dusk:?}");
        assert!(dusk.sky_light[0] > dusk.sky_light[2], "warm: {dusk:?}");
        for d in [noon, night, dusk] {
            assert_eq!(d.ambient, 0.02);
        }
        // Brightness falls monotonically from noon to midnight.
        let mut last = f32::INFINITY;
        for hour in 12..=24 {
            let light = curve.at(&TimeOfDay::at_clock(f64::from(hour))).sky_light;
            let sum: f32 = light.iter().sum();
            assert!(sum <= last + 1e-6, "{hour}: {sum} after {last}");
            last = sum;
        }
        // Dawn mirrors dusk.
        let dawn = curve.at(&TimeOfDay::at_clock(6.5));
        let evening = curve.at(&TimeOfDay::at_clock(17.5));
        for i in 0..3 {
            assert!((dawn.sky_light[i] - evening.sky_light[i]).abs() < 1e-5);
        }
    }
}
