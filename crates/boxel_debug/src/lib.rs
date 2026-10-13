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
//! eye and at the [`CrosshairTarget`], the time of day (`boxel_world::TimeOfDay`) and the chunk
//! counters of `boxel_world::WorldStats` plus the GPU-side chunk and draw call counts of
//! `boxel_render::RenderInfo`. Missing resources (no world, no renderer yet) just hide their
//! lines.
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

pub use font::{GLYPH_SIZE, TextStyle, glyph, layout_text};

use boxel_app::{App, FrameLimit, Last, Plugin, Time, Update};
use boxel_camera::Camera;
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, UVec2};
use boxel_render::{OverlayDraw, OverlayRect, PresentSettings, RenderInfo, SurfaceInfo};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, LightValue};
use boxel_window::{ButtonInput, FramePacingInfo, KeyCode};
use boxel_world::lifecycle::{TeardownSystems, WorldScopeApp, WorldTeardown};
use boxel_world::{ChunkMap, TimeOfDay, WorldStats};
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
    /// The crosshair target and its light.
    pub target: Option<(BlockPos, Option<LightValue>)>,
}

/// `sky 15 rgb 14 11 6`, or `-` when unknown.
fn light_text(light: Option<LightValue>) -> String {
    match light {
        Some(l) => {
            let [r, g, b] = l.rgb();
            format!("sky {} rgb {r} {g} {b}", l.sky())
        }
        None => "-".to_string(),
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
        lines.push(format!("Light eye {}", light_text(light.eye)));
        if let Some((pos, value)) = light.target {
            lines.push(format!(
                "      target {} {} {}  {}",
                pos.x,
                pos.y,
                pos.z,
                light_text(value)
            ));
        }
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
    }
    lines.extend(inputs.extra.iter().cloned());
    lines
}

/// Rebuilds `OverlayDraw` from the current state. While hidden it holds only the crosshair (or
/// nothing), and is written only when that changes.
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
    if !overlay.visible {
        let want: &[OverlayRect] = cross.as_ref().map_or(&[], |r| r.as_slice());
        if draw.rects.as_slice() != want {
            draw.rects.clear();
            draw.rects.extend_from_slice(want);
        }
        return;
    }
    let camera = cameras
        .iter()
        .find(|(camera, _)| camera.active)
        .map(|(_, transform)| (transform.translation, transform.forward()));
    let (map, target, time) = world_view;
    let light = map.as_deref().map(|map| LightProbe {
        eye: camera.and_then(|(eye, _)| map.light_at(BlockPos::from_world(eye))),
        target: target
            .as_deref()
            .and_then(|t| t.0)
            .map(|pos| (pos, map.light_at(pos))),
    });
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
        extra: extra
            .as_deref()
            .map(|e| e.iter().map(str::to_string).collect())
            .unwrap_or_default(),
    };
    let lines = overlay_lines(&inputs);
    draw.rects.clear();
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
