//! `boxel_debug`: the F3 debug overlay (Minecraft style). [`DebugOverlayPlugin`] toggles it
//! with F3 (hidden by default) and, while visible, writes its text as rectangles into
//! `boxel_render::OverlayDraw`, which the render plugin draws on top of every frame (and so in
//! screenshots too).
//!
//! Shown: frames presented per second and app updates per second (they differ: see
//! `boxel_app::FrameLimit`), the average and worst frame time of the last second, the frame
//! limit with the vsync state and present mode (and `paced by the display` when the runner
//! follows the monitor, `boxel_window::FramePacingInfo`), the GPU (adapter, backend, tier, device class,
//! driver), the camera (position, chunk, facing), the voxel light (sky and RGB levels) at the
//! eye and at the [`CrosshairTarget`] (at the eye also the sky's color when a light filter
//! tinted it, ADR 0064), the time of day (`boxel_world::TimeOfDay`) and the chunk
//! counters of `boxel_world::WorldStats` plus the GPU-side chunk and draw call counts of
//! `boxel_render::RenderInfo`. Missing resources (no world, no renderer yet) just hide their
//! lines.
//!
//! Sun shadows (ADR 0065): while `RenderSettings::sun_shadows` is on, a `Shadows` line gives
//! what the last frame drew into the shadow maps (`RenderInfo::shadows`) and the sun's elevation
//! (`boxel_world::SunPath` at the clock), or says they are off where the renderer does not draw
//! them (Mid, Low, an integrated GPU). With shadows off the line is hidden ([`ShadowProbe`]).
//!
//! Sky sources and realms (ADR 0061): a `Realm` line gives the realm at the eye with its daylight
//! curve, and in a world whose registry has sky sources a `Source` line gives the source whose
//! beam lights the [`CrosshairTarget`] ([`SourceProbe`]). [`SkySourceMarkers`] (off by default)
//! marks the sources and their beams near the camera, with or without the text.
//!
//! Optional debug keys ([`FrameKeys`], off by default; F5 and F6 when on) cycle the frame limit
//! and toggle vsync at runtime; the overlay then shows a hint line for them.
//!
//! Apps add their own lines through [`OverlayLines`] (shown last, in key order), and can turn on
//! a [`Crosshair`] at the center of the frame, drawn whether the text is shown or not (for
//! aiming from the screen center, as block picking does).
//!
//! Facing convention: north is -Z, east is +X; yaw is a compass heading in degrees (0 north,
//! 90 east), pitch is positive looking up.

mod font;
mod sky;

pub use font::{GLYPH_SIZE, TextStyle, glyph, layout_text};
pub use sky::{MAX_MARKS, RealmProbe, SkySourceMarkers, SourceProbe, project};

use boxel_app::{App, FrameLimit, Last, Plugin, Time, Update};
use boxel_camera::{Camera, CameraView};
use boxel_ecs::prelude::*;
use boxel_ecs::system::SystemParam;
use boxel_math::{DVec3, UVec2};
use boxel_render::{
    OverlayDraw, OverlayRect, PresentSettings, RenderInfo, RenderSettings, ShadowStats, SurfaceInfo,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, LightValue};
use boxel_window::{ButtonInput, FramePacingInfo, KeyCode};
use boxel_world::lifecycle::{TeardownSystems, WorldScopeApp, WorldTeardown};
use boxel_world::{
    ChunkMap, DaylightCurve, RealmLighting, RealmPlanes, SkyMap, SunPath, TimeOfDay, WorldContent,
    WorldStats,
};
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write;
use std::time::{Duration, Instant};

/// Adds the F3 debug overlay. Requires `TimePlugin` and `RenderPlugin` (for `OverlayDraw`;
/// the plugin inits it when missing so it also runs headless). Input comes from
/// `ButtonInput<KeyCode>` when present.
#[derive(Clone, Debug, Default)]
pub struct DebugOverlayPlugin {
    /// Start visible (screenshots, `voxel_world --debug-overlay`).
    pub visible: bool,
    /// Debug keys for the frame limit and vsync (`None`, the default: no keys, so games keep
    /// F5 and F6 for themselves).
    pub frame_keys: Option<FrameKeys>,
}

/// State of the overlay. Flip `visible` from code, or press [`DebugOverlay::toggle_key`].
#[derive(Resource, Clone, Debug)]
pub struct DebugOverlay {
    /// Drawn this frame.
    pub visible: bool,
    /// Key that toggles it (F3).
    pub toggle_key: KeyCode,
    /// Text placement and colors.
    pub style: TextStyle,
    /// Frame-settings debug keys (`None`: off); the overlay shows a hint line for them.
    pub frame_keys: Option<FrameKeys>,
}

impl Default for DebugOverlay {
    fn default() -> Self {
        Self {
            visible: false,
            toggle_key: KeyCode::F3,
            style: TextStyle::default(),
            frame_keys: None,
        }
    }
}

/// Debug keys that change the frame settings at runtime: one cycles `boxel_app::FrameLimit`
/// through [`FRAME_LIMIT_STEPS`], the other toggles vsync in `boxel_render::PresentSettings`.
/// Both log the new value at info level. A game's settings menu writes the same two resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameKeys {
    /// Cycles the frame limit (default F5).
    pub cycle_limit: KeyCode,
    /// Toggles vsync (default F6).
    pub toggle_vsync: KeyCode,
}

impl Default for FrameKeys {
    fn default() -> Self {
        Self {
            cycle_limit: KeyCode::F5,
            toggle_vsync: KeyCode::F6,
        }
    }
}

/// Extra overlay lines from apps and other crates (a picked block, game state), shown after the
/// engine's lines in key order. Inserted by [`DebugOverlayPlugin`]; write it from any system.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct OverlayLines {
    lines: BTreeMap<&'static str, String>,
}

impl OverlayLines {
    /// Sets (replaces) the line for `key`.
    pub fn set(&mut self, key: &'static str, line: impl Into<String>) {
        self.lines.insert(key, line.into());
    }
    /// Removes the line for `key`.
    pub fn remove(&mut self, key: &str) {
        self.lines.remove(key);
    }
    /// The line for `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.get(key).map(String::as_str)
    }
    /// The lines, in key order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.lines.values().map(String::as_str)
    }
}

/// The voxel the crosshair points into: apps that pick blocks set it to the cell in front of the
/// picked face (the light there is what shades that face), `None` when nothing is picked. The
/// overlay shows its light next to the eye's. Inserted (empty) by [`DebugOverlayPlugin`].
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CrosshairTarget(pub Option<BlockPos>);

/// The light the overlay shows: at the eye's voxel and at the [`CrosshairTarget`]. `None` light
/// means the chunk is not loaded or keeps no light: outside `WorldSettings::light_cache_radius`,
/// or within it before the chunk's mesh or light job finished (all-air chunks included).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightProbe {
    /// Light of the voxel holding the eye.
    pub eye: Option<LightValue>,
    /// The sky `[red, green, blue]` of the voxel holding the eye (`ChunkMap::sky_rgb_at`):
    /// tinted by the light filters it went through with colored sky (ADR 0064), else its sky
    /// level three times. Shown only when tinted.
    pub eye_sky_rgb: Option<[u8; 3]>,
    /// The crosshair target and its light.
    pub target: Option<(BlockPos, Option<LightValue>)>,
}

impl LightProbe {
    /// The light of `map` at the voxel holding `eye` and at `target`.
    pub fn at(map: &ChunkMap, eye: Option<DVec3>, target: Option<BlockPos>) -> Self {
        let eye = eye.map(BlockPos::from_world);
        Self {
            eye: eye.and_then(|p| map.light_at(p)),
            eye_sky_rgb: eye.and_then(|p| map.sky_rgb_at(p)),
            target: target.map(|pos| (pos, map.light_at(pos))),
        }
    }
}

/// `sky 15 rgb 14 11 6`, or `-` when unknown; with `tinted sky 13 7 6` after it when the sky
/// `sky_rgb` is not the sky level three times.
fn light_text(light: Option<LightValue>, sky_rgb: Option<[u8; 3]>) -> String {
    match light {
        Some(l) => {
            let [r, g, b] = l.rgb();
            let mut text = format!("sky {} rgb {r} {g} {b}", l.sky());
            if let Some([r, g, b]) = sky_rgb.filter(|&c| c != [l.sky(); 3]) {
                let _ = write!(text, "  tinted sky {r} {g} {b}");
            }
            text
        }
        None => "-".to_string(),
    }
}

/// The sun shadows the overlay shows (ADR 0065), while `RenderSettings::sun_shadows` is on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShadowProbe {
    /// What the last frame drew into the maps (`RenderInfo::shadows`). No bytes: the renderer
    /// draws no shadows here (Mid, Low, or an integrated GPU without `on_integrated`).
    pub stats: ShadowStats,
    /// The sun's elevation in degrees, on its step (negative at night).
    pub sun_degrees: f64,
}

impl ShadowProbe {
    /// The shadows of `stats` with the sun of `path` (the default one without it) at `time`
    /// (noon without a clock), as the render extract places it.
    pub fn at(stats: ShadowStats, path: Option<&SunPath>, time: Option<&TimeOfDay>) -> Self {
        let noon = TimeOfDay::default();
        let sun = path
            .copied()
            .unwrap_or_default()
            .direction(time.unwrap_or(&noon));
        Self {
            stats,
            sun_degrees: sun.y.clamp(-1.0, 1.0).asin().to_degrees(),
        }
    }

    /// The probe of the overlay: [`ShadowProbe::at`] with the renderer's stats while `settings`
    /// turn sun shadows on, `None` (no line) while they are off or without a renderer.
    pub fn when_on(
        settings: Option<&RenderSettings>,
        render: Option<&RenderInfo>,
        path: Option<&SunPath>,
        time: Option<&TimeOfDay>,
    ) -> Option<Self> {
        let on = settings.is_some_and(|s| s.sun_shadows.enabled);
        render
            .filter(|_| on)
            .map(|info| Self::at(info.shadows, path, time))
    }

    /// `Shadows 3 cascades  24 draws  sun 34 deg  53 MiB` (with `6 entity draws` after the draws
    /// when models or cell quads cast, and `dropped N` when a list was full), `Shadows none drawn ...` in a frame with nothing to shadow (night, no sky above, a
    /// realm without the sun), or `Shadows off here ...` where the renderer does not draw them.
    pub fn line(&self) -> String {
        let s = self.stats;
        if s.bytes == 0 {
            return "Shadows off here (High only, integrated GPUs on request)".to_string();
        }
        let mut line = if s.cascades == 0 {
            "Shadows none drawn".to_string()
        } else {
            format!("Shadows {} cascades  {} draws", s.cascades, s.draws)
        };
        if s.cascades > 0 && s.entity_draws > 0 {
            let _ = write!(line, "  {} entity draws", s.entity_draws);
        }
        let _ = write!(
            line,
            "  sun {:.0} deg  {:.0} MiB",
            self.sun_degrees,
            s.bytes as f64 / f64::from(1 << 20)
        );
        if s.dropped > 0 {
            let _ = write!(line, "  dropped {}", s.dropped);
        }
        line
    }
}

/// A cross at the center of the frame. Off by default; [`DebugOverlayPlugin`] draws it while
/// `visible`, with or without the F3 text (it needs the renderer's `SurfaceInfo` for the frame
/// size).
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct Crosshair {
    /// Drawn.
    pub visible: bool,
    /// Arm length from the center, in physical pixels.
    pub size: f32,
    /// Line thickness, in physical pixels.
    pub thickness: f32,
    /// Linear RGBA.
    pub color: [f32; 4],
}

impl Default for Crosshair {
    fn default() -> Self {
        Self {
            visible: false,
            size: 10.0,
            thickness: 2.0,
            color: [1.0, 1.0, 1.0, 0.8],
        }
    }
}

impl Crosshair {
    /// The two bars of the cross for a frame of `size` physical pixels (horizontal first).
    pub fn rects(&self, size: UVec2) -> [OverlayRect; 2] {
        let center = [size.x as f32 / 2.0, size.y as f32 / 2.0];
        let (arm, half) = (self.size, self.thickness / 2.0);
        [
            OverlayRect {
                min: [center[0] - arm, center[1] - half],
                max: [center[0] + arm, center[1] + half],
                color: self.color,
            },
            OverlayRect {
                min: [center[0] - half, center[1] - arm],
                max: [center[0] + half, center[1] + arm],
                color: self.color,
            },
        ]
    }
}

/// The limits [`FrameKeys::cycle_limit`] steps through, in order (0 = unlimited).
pub const FRAME_LIMIT_STEPS: [u32; 5] = [30, 60, 144, 240, 0];

/// The limit after `max_fps` in [`FRAME_LIMIT_STEPS`]: the next larger step, unlimited after
/// the largest one (or after any value above it), and the first step after unlimited.
pub fn next_frame_limit(max_fps: u32) -> u32 {
    if max_fps == 0 {
        return FRAME_LIMIT_STEPS[0];
    }
    FRAME_LIMIT_STEPS
        .into_iter()
        .find(|&step| step > max_fps)
        .unwrap_or(0)
}

/// Frame times of the last second (wall clock, `Time::real_delta`): one entry per app update.
#[derive(Resource, Clone, Debug, Default)]
pub struct FrameWindow {
    frames: VecDeque<Duration>,
    sum: Duration,
}

/// Length of the [`FrameWindow`] and [`PresentWindow`].
const WINDOW: Duration = Duration::from_secs(1);

impl FrameWindow {
    /// Adds a frame time and forgets frames older than one second (zero deltas are ignored).
    pub fn push(&mut self, frame: Duration) {
        if frame.is_zero() {
            return;
        }
        self.frames.push_back(frame);
        self.sum += frame;
        while self.frames.len() > 1
            && let Some(&oldest) = self.frames.front()
            && self.sum - oldest >= WINDOW
        {
            self.frames.pop_front();
            self.sum -= oldest;
        }
    }
    /// Average updates per second over the window (0 when empty).
    pub fn fps(&self) -> f64 {
        if self.sum.is_zero() {
            0.0
        } else {
            self.frames.len() as f64 / self.sum.as_secs_f64()
        }
    }
    /// Average frame time over the window.
    pub fn average(&self) -> Duration {
        u32::try_from(self.frames.len())
            .ok()
            .filter(|&n| n > 0)
            .map_or(Duration::ZERO, |n| self.sum / n)
    }
    /// Worst frame time of the window.
    pub fn worst(&self) -> Duration {
        self.frames.iter().copied().max().unwrap_or_default()
    }
}

/// Presented frames of the last second, from `RenderInfo::frames_presented` sampled every
/// update (frames shown on screen, as opposed to the app updates of [`FrameWindow`]).
#[derive(Resource, Clone, Debug, Default)]
pub struct PresentWindow {
    samples: VecDeque<(Instant, u64)>,
}

impl PresentWindow {
    /// Adds the presented-frame count seen at `now`, keeping the newest sample that is at
    /// least one second old as the start of the window.
    pub fn push(&mut self, now: Instant, presented: u64) {
        self.samples.push_back((now, presented));
        while self.samples.len() > 2
            && self
                .samples
                .get(1)
                .is_some_and(|&(at, _)| now.saturating_duration_since(at) >= WINDOW)
        {
            self.samples.pop_front();
        }
    }
    /// Presented frames per second over the window (0 before two samples).
    pub fn fps(&self) -> f64 {
        let (Some(&(t0, c0)), Some(&(t1, c1))) = (self.samples.front(), self.samples.back()) else {
            return 0.0;
        };
        let span = t1.saturating_duration_since(t0);
        if span.is_zero() {
            0.0
        } else {
            c1.saturating_sub(c0) as f64 / span.as_secs_f64()
        }
    }
}

impl Plugin for DebugOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DebugOverlay {
            visible: self.visible,
            frame_keys: self.frame_keys,
            ..DebugOverlay::default()
        })
        .init_resource::<FrameWindow>()
        .init_resource::<PresentWindow>()
        .init_resource::<OverlayDraw>()
        .init_resource::<OverlayLines>()
        .init_resource::<Crosshair>()
        .init_resource::<CrosshairTarget>()
        .init_resource::<SkySourceMarkers>()
        .world_resource::<CrosshairTarget>(|t| t.0.is_none())
        .add_systems(Update, (toggle_overlay, frame_keys))
        .add_systems(Last, (track_frames, build_overlay).chain())
        .add_systems(WorldTeardown, forget_target.in_set(TeardownSystems::Render));
    }
}

/// `WorldTeardown`: the crosshair target was a block of the closed world.
fn forget_target(mut target: ResMut<CrosshairTarget>) {
    *target = CrosshairTarget::default();
}

/// Flips [`DebugOverlay::visible`] on a press of its toggle key.
fn toggle_overlay(keys: Option<Res<ButtonInput<KeyCode>>>, mut overlay: ResMut<DebugOverlay>) {
    if keys.is_some_and(|k| k.just_pressed(overlay.toggle_key)) {
        overlay.visible = !overlay.visible;
    }
}

/// [`FrameKeys`]: cycles the frame limit and toggles vsync (when the keys are on and the
/// resources exist).
fn frame_keys(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    overlay: Res<DebugOverlay>,
    limit: Option<ResMut<FrameLimit>>,
    present: Option<ResMut<PresentSettings>>,
) {
    let (Some(keys), Some(bind)) = (keys, overlay.frame_keys) else {
        return;
    };
    if keys.just_pressed(bind.cycle_limit)
        && let Some(mut limit) = limit
    {
        limit.max_fps = next_frame_limit(limit.max_fps);
        tracing::info!("frame limit: {}", limit_text(Some(*limit)));
    }
    if keys.just_pressed(bind.toggle_vsync)
        && let Some(mut present) = present
    {
        let on = !present.is_vsync();
        present.set_vsync(on);
        tracing::info!("vsync: {}", on_off(on));
    }
}

fn track_frames(
    time: Res<Time>,
    render: Option<Res<RenderInfo>>,
    mut updates: ResMut<FrameWindow>,
    mut presented: ResMut<PresentWindow>,
) {
    updates.push(time.real_delta());
    if let (Some(render), Some(now)) = (render, time.last_update()) {
        presented.push(now, render.frames_presented);
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// `144 fps`, or `none` (unlimited, or no limit resource).
fn limit_text(limit: Option<FrameLimit>) -> String {
    match limit {
        Some(limit) if !limit.is_unlimited() => format!("{} fps", limit.max_fps),
        _ => "none".to_string(),
    }
}

/// Everything the text is made from (missing parts hide their lines).
#[derive(Clone, Debug, Default)]
pub struct OverlayInputs {
    /// App updates per second, average frame time and worst frame of the last second.
    pub frames: (f64, Duration, Duration),
    /// Frames presented per second over the last second (`None`: no renderer).
    pub presented_fps: Option<f64>,
    /// The app's frame limit (`None`: no resource, so no limit).
    pub frame_limit: Option<FrameLimit>,
    /// Wanted presentation (`None`: no renderer).
    pub present: Option<PresentSettings>,
    /// How the runner paced the frame (`None`: no winit runner).
    pub pacing: Option<FramePacingInfo>,
    /// Frame-settings keys to show a hint for.
    pub frame_keys: Option<FrameKeys>,
    /// Renderer status.
    pub render: Option<RenderInfo>,
    /// Active camera: position and forward direction.
    pub camera: Option<(DVec3, boxel_math::Vec3)>,
    /// Chunk counters.
    pub world: Option<WorldStats>,
    /// Voxel light at the eye and the crosshair (`None`: no world).
    pub light: Option<LightProbe>,
    /// World clock (`None`: no `DaylightPlugin`).
    pub time: Option<TimeOfDay>,
    /// The realm at the eye (`None`: no world or no camera).
    pub realm: Option<RealmProbe>,
    /// The sky source lighting the crosshair target (`None`: no target, or a registry without
    /// sky sources).
    pub source: Option<SourceProbe>,
    /// The sun shadows (`None`: off in `RenderSettings`, or no renderer).
    pub shadows: Option<ShadowProbe>,
    /// Lines added by apps ([`OverlayLines`]), shown last.
    pub extra: Vec<String>,
}

/// Compass name of a heading in degrees (0 north, 90 east). Rounded to the 0.1 degree the
/// overlay prints, so a printed 315.0 always says north.
pub fn cardinal(yaw_degrees: f64) -> &'static str {
    const NAMES: [&str; 4] = ["north", "east", "south", "west"];
    let yaw = (yaw_degrees * 10.0).round() / 10.0;
    let index = ((yaw + 45.0).rem_euclid(360.0) / 90.0) as usize % 4;
    NAMES[index]
}

/// Longest adapter or driver text shown (the rest is cut with `...`).
const MAX_NAME: usize = 52;

fn shorten(text: &str) -> String {
    if text.chars().count() <= MAX_NAME {
        text.to_string()
    } else {
        let cut: String = text.chars().take(MAX_NAME - 3).collect();
        format!("{cut}...")
    }
}

/// `Limit 144 fps  vsync off (Mailbox)`: the vsync state is the configured present mode when a
/// GPU is ready (plus the wanted state when it differs, as when the surface cannot turn vsync
/// off), else the wanted one.
fn frame_settings_line(inputs: &OverlayInputs) -> String {
    let mut line = format!("Limit {}", limit_text(inputs.frame_limit));
    let configured = inputs
        .render
        .as_ref()
        .filter(|info| info.ready)
        .and_then(|info| Some((info.present_mode?, info.vsync()?)));
    let wanted = inputs.present.map(PresentSettings::is_vsync);
    match (configured, wanted) {
        (Some((mode, on)), wanted) => {
            let _ = write!(line, "  vsync {} ({mode:?})", on_off(on));
            if let Some(wanted) = wanted.filter(|&w| w != on) {
                let _ = write!(line, "  wanted {}", on_off(wanted));
            }
        }
        (None, Some(wanted)) => {
            let _ = write!(line, "  vsync {}", on_off(wanted));
        }
        (None, None) => {}
    }
    if let Some(pacing) = inputs.pacing.filter(|p| p.display_locked) {
        line.push_str("  paced by the display");
        if let Some(mhz) = pacing.refresh_millihertz {
            let _ = write!(line, " ({:.1} Hz)", f64::from(mhz) / 1000.0);
        }
    }
    line
}

/// The overlay text, one string per line (ASCII).
pub fn overlay_lines(inputs: &OverlayInputs) -> Vec<String> {
    let mut lines = vec!["boxel debug (F3)".to_string()];
    let (updates, average, worst) = inputs.frames;
    lines.push(match inputs.presented_fps {
        Some(shown) => format!("FPS {shown:.0} shown  {updates:.0} updates/s"),
        None => format!("Updates {updates:.0}/s"),
    });
    lines.push(format!(
        "Frame {:.2} ms avg  worst {:.2} ms (1 s)",
        average.as_secs_f64() * 1e3,
        worst.as_secs_f64() * 1e3
    ));
    lines.push(frame_settings_line(inputs));
    if let Some(keys) = inputs.frame_keys {
        lines.push(format!(
            "Keys {:?} fps limit  {:?} vsync",
            keys.cycle_limit, keys.toggle_vsync
        ));
    }
    match &inputs.render {
        Some(info) if info.ready => {
            lines.push(format!("GPU {}", shorten(&info.adapter)));
            let tier = info.tier.map_or_else(|| "?".to_string(), |t| t.to_string());
            let class = info
                .class
                .map_or_else(|| "?".to_string(), |c| format!("{c:?}"));
            lines.push(format!("    {}  tier {tier}  class {class}", info.backend));
            if !info.driver.is_empty() {
                lines.push(format!("    driver {}", shorten(&info.driver)));
            }
        }
        Some(_) => lines.push("GPU not ready".to_string()),
        None => {}
    }
    if let Some((position, forward)) = inputs.camera {
        lines.push(format!(
            "XYZ {:.2} / {:.2} / {:.2}",
            position.x, position.y, position.z
        ));
        let (chunk, local) = BlockPos::from_world(position).split();
        lines.push(format!(
            "Chunk {} {} {}  in chunk {} {} {}",
            chunk.x,
            chunk.y,
            chunk.z,
            local.x(),
            local.y(),
            local.z()
        ));
        let f = forward.as_dvec3();
        let yaw = f.x.atan2(-f.z).to_degrees().rem_euclid(360.0);
        let pitch = f.y.clamp(-1.0, 1.0).asin().to_degrees();
        lines.push(format!(
            "Facing {} (yaw {yaw:.1}, pitch {pitch:.1})",
            cardinal(yaw)
        ));
    }
    if let Some(light) = &inputs.light {
        lines.push(format!(
            "Light eye {}",
            light_text(light.eye, light.eye_sky_rgb)
        ));
        if let Some((pos, value)) = light.target {
            lines.push(format!(
                "      target {} {} {}  {}",
                pos.x,
                pos.y,
                pos.z,
                light_text(value, None)
            ));
        }
    }
    if let Some(realm) = &inputs.realm {
        lines.push(realm.line());
    }
    if let Some(source) = &inputs.source {
        lines.push(source.line());
    }
    if let Some(time) = &inputs.time {
        let state = if time.paused {
            "  paused".to_string()
        } else if time.speed != 1 {
            format!("  x{}", time.speed)
        } else {
            String::new()
        };
        lines.push(format!(
            "Time {}  day {}{state}",
            time.clock_text(),
            time.day()
        ));
    }
    if let Some(world) = &inputs.world {
        lines.push(format!(
            "Chunks {} loaded  {} data ready",
            world.chunks, world.data_ready
        ));
        lines.push(format!(
            "       {} meshed  {} empty",
            world.mesh_ready, world.mesh_empty
        ));
        lines.push(format!(
            "       {} generating  {} meshing",
            world.generating, world.meshing
        ));
        lines.push(format!(
            "Jobs {} queued  {} in flight",
            world.queued_jobs, world.in_flight
        ));
    }
    if let Some(info) = inputs.render.as_ref().filter(|i| i.ready) {
        let mut line = String::new();
        let _ = write!(
            line,
            "GPU chunks {}  draw calls {}",
            info.chunks, info.draws
        );
        lines.push(line);
        if let Some(shadows) = &inputs.shadows {
            lines.push(shadows.line());
        }
    }
    lines.extend(inputs.extra.iter().cloned());
    lines
}

/// What the overlay reads of the sky sources and realms (ADR 0061).
#[derive(SystemParam)]
struct SkyView<'w> {
    markers: Option<Res<'w, SkySourceMarkers>>,
    sky: Option<Res<'w, SkyMap>>,
    content: Option<Res<'w, WorldContent>>,
    planes: Option<Res<'w, RealmPlanes>>,
    lighting: Option<Res<'w, RealmLighting>>,
    curve: Option<Res<'w, DaylightCurve>>,
    sun: Option<Res<'w, SunPath>>,
    settings: Option<Res<'w, RenderSettings>>,
}

/// Rebuilds `OverlayDraw` from the current state. While hidden it holds only the crosshair and
/// the [`SkySourceMarkers`] (or nothing), and is written only when that changes.
fn build_overlay(
    overlay: Res<DebugOverlay>,
    window: Res<FrameWindow>,
    presented: Res<PresentWindow>,
    limit: Option<Res<FrameLimit>>,
    present: Option<Res<PresentSettings>>,
    pacing: Option<Res<FramePacingInfo>>,
    render: Option<Res<RenderInfo>>,
    world: Option<Res<WorldStats>>,
    extra: Option<Res<OverlayLines>>,
    crosshair: Option<Res<Crosshair>>,
    surface: Option<Res<SurfaceInfo>>,
    cameras: Query<(&Camera, &Transform)>,
    sky_view: SkyView,
    world_view: (
        Option<Res<ChunkMap>>,
        Option<Res<CrosshairTarget>>,
        Option<Res<TimeOfDay>>,
    ),
    mut draw: ResMut<OverlayDraw>,
) {
    let frame = surface.map(|s| s.size).filter(|s| s.x > 0 && s.y > 0);
    let cross = match (crosshair.as_deref(), frame) {
        (Some(c), Some(frame)) if c.visible => Some(c.rects(frame)),
        _ => None,
    };
    let active = cameras.iter().find(|(camera, _)| camera.active);
    let mut marks = Vec::new();
    if let (Some(markers), Some(sky), Some(frame), Some((cam, transform))) = (
        sky_view.markers.as_deref().filter(|m| m.visible),
        sky_view.sky.as_deref(),
        frame,
        active,
    ) {
        let view = CameraView::compute(cam, transform, frame.x as f32 / frame.y as f32);
        markers.marks_in(sky, &view, transform.translation, frame, &mut marks);
    }
    if !overlay.visible {
        if let Some(cross) = &cross {
            marks.extend_from_slice(cross);
        }
        if draw.rects != marks {
            draw.rects = marks;
        }
        return;
    }
    let camera = active.map(|(_, transform)| (transform.translation, transform.forward()));
    let (map, target, time) = world_view;
    let target = target.as_deref().and_then(|t| t.0);
    let realm = sky_view
        .planes
        .as_deref()
        .zip(camera)
        .map(|(planes, (eye, _))| {
            RealmProbe::at(
                BlockPos::from_world(eye).y,
                planes,
                sky_view.lighting.as_deref(),
                sky_view.curve.as_deref().zip(time.as_deref()),
            )
        });
    let sources = sky_view
        .content
        .as_deref()
        .is_some_and(|c| c.light_tables.any_sky_source());
    let source = match (sky_view.sky.as_deref(), target) {
        (Some(sky), Some(pos)) if sources => Some(SourceProbe::at(sky, pos)),
        _ => None,
    };
    let light = map
        .as_deref()
        .map(|map| LightProbe::at(map, camera.map(|(eye, _)| eye), target));
    let shadows = ShadowProbe::when_on(
        sky_view.settings.as_deref(),
        render.as_deref(),
        sky_view.sun.as_deref(),
        time.as_deref(),
    );
    let inputs = OverlayInputs {
        frames: (window.fps(), window.average(), window.worst()),
        presented_fps: render.is_some().then(|| presented.fps()),
        frame_limit: limit.as_deref().copied(),
        present: present.as_deref().copied(),
        pacing: pacing.as_deref().copied(),
        frame_keys: overlay.frame_keys,
        render: render.as_deref().cloned(),
        camera,
        world: world.as_deref().copied(),
        light,
        time: time.as_deref().copied(),
        realm,
        source,
        shadows,
        extra: extra
            .as_deref()
            .map(|e| e.iter().map(str::to_string).collect())
            .unwrap_or_default(),
    };
    let lines = overlay_lines(&inputs);
    draw.rects.clear();
    draw.rects.append(&mut marks);
    layout_text(&lines, &overlay.style, &mut draw.rects);
    if let Some(cross) = cross {
        draw.rects.extend_from_slice(&cross);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_app::TimePlugin;

    #[test]
    fn f3_toggles_the_overlay_and_hidden_draws_nothing() {
        let mut app = App::new();
        app.add_plugins((TimePlugin, DebugOverlayPlugin::default()))
            .init_resource::<ButtonInput<KeyCode>>();
        app.update();
        assert!(!app.world().resource::<DebugOverlay>().visible);
        assert!(app.world().resource::<OverlayDraw>().rects.is_empty());

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::F3);
        app.update();
        assert!(app.world().resource::<DebugOverlay>().visible);
        assert!(!app.world().resource::<OverlayDraw>().rects.is_empty());

        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release(KeyCode::F3);
        keys.clear_just();
        keys.press(KeyCode::F3);
        app.update();
        assert!(!app.world().resource::<DebugOverlay>().visible);
        assert!(app.world().resource::<OverlayDraw>().rects.is_empty());
    }

    #[test]
    fn app_lines_come_last_and_the_crosshair_shows_with_or_without_text() {
        let mut app = App::new();
        app.add_plugins((TimePlugin, DebugOverlayPlugin::default()))
            .init_resource::<ButtonInput<KeyCode>>();
        app.update();
        // Off by default: a hidden overlay draws nothing, even with a frame size.
        app.insert_resource(SurfaceInfo {
            size: UVec2::new(800, 600),
            ..SurfaceInfo::default()
        });
        app.update();
        assert!(app.world().resource::<OverlayDraw>().rects.is_empty());
        // Crosshair on, text hidden: only the two bars, centered.
        app.world_mut().resource_mut::<Crosshair>().visible = true;
        app.update();
        let rects = app.world().resource::<OverlayDraw>().rects.clone();
        assert_eq!(rects.len(), 2);
        assert_eq!(
            (rects[0].min, rects[0].max),
            ([390.0, 299.0], [410.0, 301.0])
        );
        assert_eq!(
            (rects[1].min, rects[1].max),
            ([399.0, 290.0], [401.0, 310.0])
        );
        // Unchanged frames do not touch the draw list.
        let tick = app.world().resource_ref::<OverlayDraw>().last_changed();
        app.update();
        assert_eq!(
            app.world().resource_ref::<OverlayDraw>().last_changed(),
            tick
        );
        // Text visible: the crosshair stays on top (last).
        app.world_mut().resource_mut::<DebugOverlay>().visible = true;
        app.update();
        let rects = &app.world().resource::<OverlayDraw>().rects;
        assert!(rects.len() > 2);
        assert_eq!(
            rects[rects.len() - 2..],
            Crosshair {
                visible: true,
                ..Crosshair::default()
            }
            .rects(UVec2::new(800, 600))
        );

        let mut extra = OverlayLines::default();
        extra.set("z", "last");
        extra.set("a", "first");
        let inputs = OverlayInputs {
            extra: extra.iter().map(str::to_string).collect(),
            ..OverlayInputs::default()
        };
        let lines = overlay_lines(&inputs);
        assert_eq!(lines[lines.len() - 2..], ["first", "last"]);
        extra.remove("z");
        assert_eq!((extra.get("a"), extra.get("z")), (Some("first"), None));
    }

    #[test]
    fn frame_window_keeps_one_second() {
        let mut window = FrameWindow::default();
        for _ in 0..100 {
            window.push(Duration::from_millis(10));
        }
        window.push(Duration::from_millis(50));
        assert_eq!(window.worst(), Duration::from_millis(50));
        assert!((window.fps() - 100.0).abs() < 6.0, "{}", window.fps());
        for _ in 0..200 {
            window.push(Duration::from_millis(10));
        }
        assert_eq!(window.worst(), Duration::from_millis(10));
        assert_eq!(window.average(), Duration::from_millis(10));
    }

    #[test]
    fn lines_show_position_chunk_and_facing() {
        let inputs = OverlayInputs {
            camera: Some((
                DVec3::new(-1.5, 70.25, 33.0),
                boxel_math::Vec3::new(1.0, 0.0, 0.0),
            )),
            world: Some(WorldStats::default()),
            ..OverlayInputs::default()
        };
        let lines = overlay_lines(&inputs);
        assert!(
            lines.contains(&"XYZ -1.50 / 70.25 / 33.00".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("Chunk -1 2 1")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("Facing east (yaw 90.0")),
            "{lines:?}"
        );
        assert_eq!(cardinal(0.0), "north");
        assert_eq!(cardinal(359.0), "north");
        assert_eq!(cardinal(180.0), "south");
        assert_eq!(cardinal(270.0), "west");
        assert_eq!(cardinal(314.999_999), "north");
        assert_eq!(shorten(&"x".repeat(80)).len(), MAX_NAME);
    }

    #[test]
    fn lines_show_the_light_at_the_eye_and_the_crosshair_and_the_time() {
        let mut time = TimeOfDay::at_clock(18.5);
        let inputs = OverlayInputs {
            light: Some(LightProbe {
                eye: Some(LightValue::new(15, 0, 0, 0)),
                // Not tinted: the sky level three times shows nothing more.
                eye_sky_rgb: Some([15; 3]),
                target: Some((
                    BlockPos::new(-3, 64, 1 << 40),
                    Some(LightValue::new(9, 14, 11, 6)),
                )),
            }),
            time: Some(time),
            ..OverlayInputs::default()
        };
        let lines = overlay_lines(&inputs);
        for want in [
            "Light eye sky 15 rgb 0 0 0",
            "      target -3 64 1099511627776  sky 9 rgb 14 11 6",
            "Time 18:30  day 0",
        ] {
            assert!(lines.contains(&want.to_string()), "{want}: {lines:?}");
        }
        // Unknown light (not loaded or not cached), no target, a paused or fast clock.
        time.paused = true;
        let inputs = OverlayInputs {
            light: Some(LightProbe::default()),
            time: Some(time),
            ..OverlayInputs::default()
        };
        let lines = overlay_lines(&inputs);
        assert!(lines.contains(&"Light eye -".to_string()), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("target")), "{lines:?}");
        assert!(lines.contains(&"Time 18:30  day 0  paused".to_string()));
        // Under red glass with colored sky (ADR 0064): gameplay's sky, then the tinted one.
        let inputs = OverlayInputs {
            light: Some(LightProbe {
                eye: Some(LightValue::new(15, 2, 0, 0)),
                eye_sky_rgb: Some([13, 7, 6]),
                target: None,
            }),
            ..OverlayInputs::default()
        };
        let lines = overlay_lines(&inputs);
        assert!(
            lines.contains(&"Light eye sky 15 rgb 2 0 0  tinted sky 13 7 6".to_string()),
            "{lines:?}"
        );
        time.paused = false;
        time.speed = 60;
        let inputs = OverlayInputs {
            time: Some(time),
            ..OverlayInputs::default()
        };
        assert!(overlay_lines(&inputs).contains(&"Time 18:30  day 0  x60".to_string()));
        // No world, no light lines.
        let lines = overlay_lines(&OverlayInputs::default());
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("Light") || l.starts_with("Time"))
        );
    }

    #[test]
    fn lines_show_the_source_at_the_crosshair_and_the_realm_at_the_eye() {
        use boxel_light::Beam;
        use boxel_voxel::{BeamDir, SkySourceDef};
        // Two realms meeting at y = 0; the upper one has its own curve.
        let planes = RealmPlanes {
            planes: vec![0],
            drawn: 0,
        };
        let lighting = RealmLighting {
            realms: vec![
                boxel_world::RealmLight::default(),
                boxel_world::RealmLight {
                    curve: Some(DaylightCurve::default()),
                    ..boxel_world::RealmLight::default()
                },
            ],
        };
        let (curve, noon) = (DaylightCurve::default(), TimeOfDay::default());
        let below = RealmProbe::at(-1, &planes, Some(&lighting), Some((&curve, &noon)));
        let above = RealmProbe::at(1 << 40, &planes, Some(&lighting), None);
        assert_eq!((below.realm, below.realms, below.own_curve), (0, 2, false));
        assert_eq!(
            (above.realm, above.own_curve, above.sky_light),
            (1, true, None)
        );
        let [r, g, b] = lighting.at(0, &curve, &noon).sky_light;
        let beam = Beam {
            source: 74,
            lo: 11,
            hi: 74,
            def: SkySourceDef {
                dir: BeamDir::Down,
                range: 64,
            },
        };
        let inputs = OverlayInputs {
            light: Some(LightProbe {
                target: Some((BlockPos::new(3, 20, -2), None)),
                ..LightProbe::default()
            }),
            realm: Some(below),
            source: Some(SourceProbe::Beam(BlockPos::new(3, 74, -2), beam)),
            ..OverlayInputs::default()
        };
        let lines = overlay_lines(&inputs);
        let at = |prefix: &str| lines.iter().position(|l| l.starts_with(prefix));
        assert_eq!(
            lines[at("Realm").expect("realm line")],
            format!("Realm 0 of 2  curve global  sky {r:.2} {g:.2} {b:.2}")
        );
        assert_eq!(
            lines[at("Source").expect("source line")],
            "Source 3 74 -2  down range 64  beam 11..74"
        );
        // The realm after the light, the source after the realm.
        assert!(at("      target") < at("Realm") && at("Realm") < at("Source"));
        assert_eq!(above.line(), "Realm 1 of 2  curve own");
        assert_eq!(SourceProbe::None.line(), "Source none");
        assert_eq!(SourceProbe::Unknown.line(), "Source -");
        let up = Beam {
            def: SkySourceDef {
                dir: BeamDir::Up,
                range: 16,
            },
            ..beam
        };
        assert!(
            SourceProbe::Beam(BlockPos::new(0, 74, 0), up)
                .line()
                .contains("up range 16")
        );
        // Neither line without a world (or without sources in it).
        let lines = overlay_lines(&OverlayInputs::default());
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("Realm") || l.starts_with("Source"))
        );
    }

    /// A camera at `eye` looking along -X (yaw 90 degrees), for a frame of 800 x 600.
    fn west_view(eye: DVec3) -> (CameraView, UVec2) {
        let size = UVec2::new(800, 600);
        let transform = Transform::from_yaw_pitch(eye, std::f32::consts::FRAC_PI_2, 0.0);
        assert!((transform.forward() - boxel_math::Vec3::NEG_X).length() < 1e-6);
        let view = CameraView::compute(&Camera::default(), &transform, 800.0 / 600.0);
        (view, size)
    }

    #[test]
    fn markers_project_inside_the_screen() {
        use boxel_light::Beam;
        use boxel_voxel::{BeamDir, SkySourceDef};
        // The same scene at the origin, past the former i32 edge and at 2^40 (ADR 0014).
        for base in [0i64, 1 << 31, -(1 << 40)] {
            let eye = DVec3::new(base as f64 + 8.5, 0.5, base as f64 + 3.5);
            let (view, size) = west_view(eye);
            let at = |x: i64, y: i64, z: i64| BlockPos::new(base + x, y, base + z);
            // Straight ahead: the center; behind, nearer than the near plane or off the sides of
            // the frame: nothing.
            assert_eq!(project(&view, size, at(3, 0, 3)), Some([400.0, 300.0]));
            assert_eq!(project(&view, size, at(14, 0, 3)), None, "behind");
            assert_eq!(project(&view, size, at(8, 0, 3)), None, "at the eye");
            assert_eq!(project(&view, size, at(3, 0, 40)), None, "off the side");
            assert_eq!(project(&view, size, at(3, 40, 3)), None, "over the top");
            // A down beam from the center: the source's square over the dots under it.
            let beam = |source: i64, lo: i64| Beam {
                source,
                lo,
                hi: source,
                def: SkySourceDef {
                    dir: BeamDir::Down,
                    range: 64,
                },
            };
            let markers = SkySourceMarkers {
                visible: true,
                ..SkySourceMarkers::default()
            };
            let mut out = Vec::new();
            let beams = [
                (base + 3, base + 3, beam(0, -40)),
                // Behind the camera, and farther than the radius: left out.
                (base + 20, base + 3, beam(0, -10)),
                (base - 60, base + 3, beam(0, -10)),
            ];
            markers.marks(&view, eye, size, beams, &mut out);
            let source = out.last().expect("the source");
            assert_eq!((source.min, source.max), ([396.0, 296.0], [404.0, 304.0]));
            assert_eq!(source.color, markers.source_color);
            let dots = &out[..out.len() - 1];
            // Every other cell from the bottom (-40) to the cell under the source, within the
            // frame (the lowest ones leave it under the bottom edge).
            assert!(!dots.is_empty() && dots.len() <= 20, "{}", dots.len());
            for dot in dots {
                assert_eq!(dot.color, markers.beam_color);
                let center = (dot.min[0] + dot.max[0]) / 2.0;
                assert!((center - 400.0).abs() < 1e-3, "{dot:?}");
                assert!(dot.min[1] > 300.0, "under the source: {dot:?}");
            }
            for rect in &out {
                assert!(rect.min[0] >= 0.0 && rect.min[1] >= 0.0, "{rect:?}");
                assert!(rect.max[0] <= 800.0 && rect.max[1] <= 600.0, "{rect:?}");
            }
            // The first beam is not seen looking the other way.
            let east = Transform::from_yaw_pitch(eye, -std::f32::consts::FRAC_PI_2, 0.0);
            let back = CameraView::compute(&Camera::default(), &east, 800.0 / 600.0);
            let mut none = Vec::new();
            markers.marks(&back, eye, size, [beams[0]], &mut none);
            assert!(none.is_empty(), "{none:?}");
        }
    }

    /// Stone at chunk layers 0 and -3, air between, and one sky source under the roof at
    /// `(3, -1, 3)` (`test:sky_source`, down, range 16), declared in layer -1.
    struct SunbeamHall;

    impl boxel_worldgen::WorldGenerator for SunbeamHall {
        fn generate(
            &self,
            key: boxel_voxel::ChunkKey,
            registry: &boxel_voxel::BlockRegistry,
        ) -> boxel_voxel::ChunkData {
            use boxel_voxel::{BlockState, ChunkData, LocalPos};
            let solid = registry.by_name("test:solid").expect("solid");
            match key.pos.y {
                0 | -3 => ChunkData::uniform(solid),
                -1 if (key.pos.x, key.pos.z) == (0, 0) => {
                    let mut chunk = ChunkData::uniform(BlockState::AIR);
                    let source = registry.by_name("test:sky_source").expect("source");
                    chunk.set(LocalPos::new(3, 31, 3), source);
                    chunk
                }
                _ => ChunkData::uniform(BlockState::AIR),
            }
        }
        fn vertical_chunks(&self) -> std::ops::RangeInclusive<i64> {
            -3..=0
        }
        fn sky_source_layers(&self) -> std::ops::RangeInclusive<i64> {
            -1..=-1
        }
        fn name(&self) -> &str {
            "sunbeam hall"
        }
    }

    /// The overlay in a world with a sky source: the source at the crosshair from the world's
    /// beam tile, and its marks drawn with the text hidden.
    #[test]
    fn a_world_with_a_source_shows_it_at_the_crosshair_and_marks_it() {
        use boxel_world::jobs::InlineExecutor;
        use boxel_world::{ChunkViewer, WorldPlugin};
        let mut b = boxel_voxel::testing::builder();
        boxel_voxel::testing::add_sky_source(&mut b).expect("sources");
        let mut app = App::new();
        app.add_plugins((
            TimePlugin,
            DebugOverlayPlugin::default(),
            WorldPlugin {
                generator: std::sync::Arc::new(SunbeamHall),
                registry: std::sync::Arc::new(b.build()),
                executor: std::sync::Arc::new(InlineExecutor),
                ..WorldPlugin::default()
            },
        ))
        .insert_resource(SurfaceInfo {
            size: UVec2::new(800, 600),
            ..SurfaceInfo::default()
        });
        // Level with the source, 5 blocks east of its column, looking west at it.
        let eye = DVec3::new(8.5, -0.5, 3.5);
        app.world_mut().spawn((
            Camera::default(),
            Transform::from_yaw_pitch(eye, std::f32::consts::FRAC_PI_2, 0.0),
            ChunkViewer {
                radius: 1,
                vertical_radius: 2,
            },
        ));
        let start = Instant::now();
        loop {
            app.update();
            let stats = *app.world().resource::<WorldStats>();
            if stats.chunks > 0 && stats.is_settled() {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "{stats:?}");
        }
        let sky = app.world().resource::<SkyMap>();
        let SourceProbe::Beam(source, beam) = SourceProbe::at(sky, BlockPos::new(3, -5, 3)) else {
            panic!("a beam through (3, -5, 3)");
        };
        assert_eq!(
            (source, beam.lo, beam.hi),
            (BlockPos::new(3, -1, 3), -16, -1)
        );
        assert_eq!(
            SourceProbe::at(sky, BlockPos::new(3, -20, 3)),
            SourceProbe::None
        );
        assert_eq!(
            SourceProbe::at(sky, BlockPos::new(4, -5, 3)),
            SourceProbe::None
        );
        assert_eq!(
            SourceProbe::at(sky, BlockPos::new(3, -5, 1000)),
            SourceProbe::Unknown,
            "not loaded"
        );
        // Off by default; on, the marks alone while the text is hidden.
        assert!(app.world().resource::<OverlayDraw>().rects.is_empty());
        app.world_mut().resource_mut::<SkySourceMarkers>().visible = true;
        app.update();
        let rects = app.world().resource::<OverlayDraw>().rects.clone();
        let markers = *app.world().resource::<SkySourceMarkers>();
        let source = rects.last().expect("the source's square");
        assert_eq!((source.min, source.max), ([396.0, 296.0], [404.0, 304.0]));
        assert!(rects.len() > 1, "and its beam");
        assert!(
            rects[..rects.len() - 1]
                .iter()
                .all(|r| r.color == markers.beam_color && r.min[1] > 300.0)
        );
        // With the text the marks go under it.
        app.world_mut().resource_mut::<DebugOverlay>().visible = true;
        app.update();
        let with_text = &app.world().resource::<OverlayDraw>().rects;
        assert_eq!(with_text[..rects.len()], rects[..]);
        assert!(with_text.len() > rects.len());
    }

    #[test]
    fn far_positions_show_their_true_chunk() {
        // Past the former i32 edge (2^31 voxels = chunk 2^26) and at 2^40, negatives too.
        for (position, chunk) in [
            (
                DVec3::new(2_147_483_648.5, 64.0, -2_147_483_649.5),
                "Chunk 67108864 2 -67108865  in chunk 0 0 30",
            ),
            (
                DVec3::new(1_099_511_627_776.25, -1.0, -1_099_511_627_807.0),
                "Chunk 34359738368 -1 -34359738369  in chunk 0 31 1",
            ),
        ] {
            let inputs = OverlayInputs {
                camera: Some((position, boxel_math::Vec3::new(0.0, 0.0, -1.0))),
                ..OverlayInputs::default()
            };
            let lines = overlay_lines(&inputs);
            assert!(lines.contains(&chunk.to_string()), "{lines:?}");
        }
    }

    #[test]
    fn present_window_measures_shown_frames_over_one_second() {
        let mut window = PresentWindow::default();
        assert_eq!(window.fps(), 0.0);
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        // 144 updates per second, a frame shown on 5 of every 6 (120 per second), for 3 s.
        let mut presented = 0;
        for i in 0..432u32 {
            if i % 6 != 0 {
                presented += 1;
            }
            window.push(t0 + Duration::from_nanos(6_944_444) * i, presented);
        }
        assert!((window.fps() - 120.0).abs() < 1.0, "{}", window.fps());
        // Only about one second of samples is kept.
        assert!(window.samples.len() <= 146, "{}", window.samples.len());
        // Nothing shown for a while (hidden window): the rate decays to 0.
        let end = t0 + ms(3000);
        for i in 0..200u32 {
            window.push(end + ms(10) * i, presented);
        }
        assert_eq!(window.fps(), 0.0);
    }

    #[test]
    fn frame_limit_steps_cycle() {
        let mut limit = 144;
        let mut seen = Vec::new();
        for _ in 0..6 {
            limit = next_frame_limit(limit);
            seen.push(limit);
        }
        assert_eq!(seen, [240, 0, 30, 60, 144, 240]);
        assert_eq!(
            next_frame_limit(100),
            144,
            "a custom limit goes to the next step"
        );
        assert_eq!(next_frame_limit(1000), 0);
    }

    #[test]
    fn frame_keys_change_the_limit_and_vsync_only_when_enabled() {
        for enabled in [false, true] {
            let mut app = App::new();
            app.add_plugins((
                TimePlugin,
                DebugOverlayPlugin {
                    frame_keys: enabled.then(FrameKeys::default),
                    ..DebugOverlayPlugin::default()
                },
            ))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<FrameLimit>()
            .init_resource::<PresentSettings>();
            app.update();
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.press(KeyCode::F5);
            keys.press(KeyCode::F6);
            app.update();
            let limit = app.world().resource::<FrameLimit>().max_fps;
            let vsync = app.world().resource::<PresentSettings>().is_vsync();
            if enabled {
                assert_eq!((limit, vsync), (240, true));
            } else {
                assert_eq!((limit, vsync), (144, false));
            }
        }
    }

    #[test]
    fn lines_show_the_sun_shadows_only_while_they_are_on() {
        let drawn = ShadowStats {
            cascades: 3,
            draws: 24,
            entity_draws: 0,
            dropped: 0,
            bytes: 53 << 20,
        };
        let render = RenderInfo {
            ready: true,
            shadows: drawn,
            ..RenderInfo::default()
        };
        let mut settings = RenderSettings::default();
        let fifteen = TimeOfDay::at_clock(15.0);
        let probe = |settings: &RenderSettings, render: &RenderInfo, time| {
            ShadowProbe::when_on(Some(settings), Some(render), None, time)
        };
        // Off (the engine's default): no line, whatever the renderer says.
        assert_eq!(probe(&settings, &render, Some(&fifteen)), None);
        settings.sun_shadows.enabled = true;
        assert_eq!(
            ShadowProbe::when_on(Some(&settings), None, None, None),
            None,
            "no renderer, no line"
        );
        // On: the maps of the last frame and the sun of the default path at the clock (at 15:00
        // the sun is 37.8 degrees up; at noon 60, the 30 degree tilt).
        let line = |probe: Option<ShadowProbe>| {
            let inputs = OverlayInputs {
                render: Some(render.clone()),
                shadows: probe,
                ..OverlayInputs::default()
            };
            let lines = overlay_lines(&inputs);
            let shadows: Vec<_> = lines.iter().filter(|l| l.starts_with("Shadows")).collect();
            assert!(shadows.len() <= 1, "{lines:?}");
            // Right after the GPU chunks line.
            if let Some(at) = lines.iter().position(|l| l.starts_with("Shadows")) {
                assert!(lines[at - 1].starts_with("GPU chunks"), "{lines:?}");
            }
            shadows.first().map(ToString::to_string)
        };
        assert_eq!(
            line(probe(&settings, &render, Some(&fifteen))).as_deref(),
            Some("Shadows 3 cascades  24 draws  sun 38 deg  53 MiB")
        );
        assert_eq!(
            line(probe(&settings, &render, None)).as_deref(),
            Some("Shadows 3 cascades  24 draws  sun 60 deg  53 MiB"),
            "noon without a clock, as the render extract"
        );
        let flat = SunPath { tilt_degrees: 0.0 };
        let straight = ShadowProbe::when_on(Some(&settings), Some(&render), Some(&flat), None);
        assert!(line(straight).unwrap().contains("sun 90 deg"));
        // Full lists, nothing to draw at night, and a renderer that draws none (Mid, Low, an
        // integrated GPU without on_integrated).
        let full = ShadowProbe {
            stats: ShadowStats {
                dropped: 5,
                ..drawn
            },
            sun_degrees: 37.8,
        };
        assert!(line(Some(full)).unwrap().ends_with("  dropped 5"));
        let figures = ShadowProbe {
            stats: ShadowStats {
                entity_draws: 6,
                ..drawn
            },
            sun_degrees: 37.8,
        };
        assert_eq!(
            line(Some(figures)).as_deref(),
            Some("Shadows 3 cascades  24 draws  6 entity draws  sun 38 deg  53 MiB")
        );
        let night = ShadowProbe {
            stats: ShadowStats {
                cascades: 0,
                draws: 0,
                ..drawn
            },
            sun_degrees: -12.2,
        };
        assert_eq!(
            line(Some(night)).as_deref(),
            Some("Shadows none drawn  sun -12 deg  53 MiB")
        );
        let midnight = probe(&settings, &render, Some(&TimeOfDay::at_clock(0.0))).unwrap();
        assert!(midnight.sun_degrees < -59.0, "{midnight:?}");
        let elsewhere = ShadowProbe {
            stats: ShadowStats::default(),
            sun_degrees: 37.8,
        };
        assert_eq!(
            line(Some(elsewhere)).as_deref(),
            Some("Shadows off here (High only, integrated GPUs on request)")
        );
        // The overlay hides it with the GPU line while the renderer is not ready.
        let lines = overlay_lines(&OverlayInputs {
            render: Some(RenderInfo::default()),
            shadows: Some(full),
            ..OverlayInputs::default()
        });
        assert!(!lines.iter().any(|l| l.starts_with("Shadows")), "{lines:?}");
    }

    #[test]
    fn the_overlay_reads_the_shadow_settings_and_the_sun() {
        let mut app = App::new();
        app.add_plugins((
            TimePlugin,
            DebugOverlayPlugin {
                visible: true,
                ..DebugOverlayPlugin::default()
            },
        ))
        .insert_resource(RenderInfo {
            ready: true,
            shadows: ShadowStats {
                cascades: 3,
                draws: 24,
                entity_draws: 0,
                dropped: 0,
                bytes: 53 << 20,
            },
            ..RenderInfo::default()
        })
        .insert_resource(RenderSettings::default());
        app.update();
        let off = app.world().resource::<OverlayDraw>().rects.len();
        app.world_mut()
            .resource_mut::<RenderSettings>()
            .sun_shadows
            .enabled = true;
        app.update();
        let on = app.world().resource::<OverlayDraw>().rects.len();
        // One more line, with its glyphs.
        assert!(on > off, "{off} rects off, {on} on");
    }

    #[test]
    fn lines_show_shown_and_update_rates_limit_vsync_and_keys() {
        let mut render = RenderInfo {
            ready: true,
            present_mode: Some(wgpu::PresentMode::Mailbox),
            ..RenderInfo::default()
        };
        let inputs = OverlayInputs {
            frames: (
                144.2,
                Duration::from_micros(6944),
                Duration::from_micros(7100),
            ),
            presented_fps: Some(120.4),
            frame_limit: Some(FrameLimit::default()),
            present: Some(PresentSettings::default()),
            frame_keys: Some(FrameKeys::default()),
            render: Some(render.clone()),
            ..OverlayInputs::default()
        };
        let lines = overlay_lines(&inputs);
        assert_eq!(lines[1], "FPS 120 shown  144 updates/s");
        assert_eq!(lines[2], "Frame 6.94 ms avg  worst 7.10 ms (1 s)");
        assert_eq!(lines[3], "Limit 144 fps  vsync off (Mailbox)");
        // The runner follows the monitor (ADR 0017).
        let mut pacing = FramePacingInfo {
            display_locked: true,
            refresh_millihertz: Some(144_002),
            ..FramePacingInfo::default()
        };
        let paced = OverlayInputs {
            pacing: Some(pacing),
            ..inputs.clone()
        };
        assert_eq!(
            overlay_lines(&paced)[3],
            "Limit 144 fps  vsync off (Mailbox)  paced by the display (144.0 Hz)"
        );
        pacing.display_locked = false;
        let grid = OverlayInputs {
            pacing: Some(pacing),
            ..inputs.clone()
        };
        assert_eq!(
            overlay_lines(&grid)[3],
            "Limit 144 fps  vsync off (Mailbox)"
        );
        assert_eq!(lines[4], "Keys F5 fps limit  F6 vsync");

        // Vsync off wanted, but the surface only offers Fifo.
        render.present_mode = Some(wgpu::PresentMode::Fifo);
        let inputs = OverlayInputs {
            frame_limit: Some(FrameLimit::UNLIMITED),
            render: Some(render),
            frame_keys: None,
            ..inputs
        };
        let lines = overlay_lines(&inputs);
        assert_eq!(lines[3], "Limit none  vsync on (Fifo)  wanted off");
        assert!(!lines.iter().any(|l| l.starts_with("Keys")));

        // No renderer: update rate only, wanted vsync state.
        let lines = overlay_lines(&OverlayInputs {
            frames: (60.0, Duration::ZERO, Duration::ZERO),
            frame_limit: Some(FrameLimit::new(60)),
            ..OverlayInputs::default()
        });
        assert_eq!(lines[1], "Updates 60/s");
        assert_eq!(lines[3], "Limit 60 fps");
    }
}
