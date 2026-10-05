//! `boxel_debug`: the F3 debug overlay (Minecraft style). [`DebugOverlayPlugin`] toggles it
//! with F3 (hidden by default) and, while visible, writes its text as rectangles into
//! `boxel_render::OverlayDraw`, which the render plugin draws on top of every frame (and so in
//! screenshots too).
//!
//! Shown: average FPS and frame time with the worst frame of the last second, the GPU (adapter,
//! backend, tier, device class, driver), the camera (position, chunk, facing) and the chunk
//! counters of `boxel_world::WorldStats` plus the GPU-side chunk and draw call counts of
//! `boxel_render::RenderInfo`. Missing resources (no world, no renderer yet) just hide their
//! lines.
//!
//! Facing convention: north is -Z, east is +X; yaw is a compass heading in degrees (0 north,
//! 90 east), pitch is positive looking up.

mod font;

pub use font::{GLYPH_SIZE, TextStyle, glyph, layout_text};

use boxel_app::{App, Last, Plugin, Time, Update};
use boxel_camera::Camera;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_render::{OverlayDraw, RenderInfo};
use boxel_transform::Transform;
use boxel_voxel::BlockPos;
use boxel_window::{ButtonInput, KeyCode};
use boxel_world::WorldStats;
use std::collections::VecDeque;
use std::fmt::Write;
use std::time::Duration;

/// Adds the F3 debug overlay. Requires `TimePlugin` and `RenderPlugin` (for `OverlayDraw`;
/// the plugin inits it when missing so it also runs headless). Input comes from
/// `ButtonInput<KeyCode>` when present.
#[derive(Clone, Debug, Default)]
pub struct DebugOverlayPlugin {
    /// Start visible (screenshots, `voxel_world --debug-overlay`).
    pub visible: bool,
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
}

impl Default for DebugOverlay {
    fn default() -> Self {
        Self {
            visible: false,
            toggle_key: KeyCode::F3,
            style: TextStyle::default(),
        }
    }
}

/// Frame times of the last second (wall clock, `Time::real_delta`).
#[derive(Resource, Clone, Debug, Default)]
pub struct FrameWindow {
    frames: VecDeque<Duration>,
    sum: Duration,
}

/// Length of the [`FrameWindow`].
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
    /// Average frames per second over the window (0 when empty).
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

impl Plugin for DebugOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DebugOverlay {
            visible: self.visible,
            ..DebugOverlay::default()
        })
        .init_resource::<FrameWindow>()
        .init_resource::<OverlayDraw>()
        .add_systems(Update, toggle_overlay)
        .add_systems(Last, (track_frames, build_overlay).chain());
    }
}

/// Flips [`DebugOverlay::visible`] on a press of its toggle key.
fn toggle_overlay(keys: Option<Res<ButtonInput<KeyCode>>>, mut overlay: ResMut<DebugOverlay>) {
    if keys.is_some_and(|k| k.just_pressed(overlay.toggle_key)) {
        overlay.visible = !overlay.visible;
    }
}

fn track_frames(time: Res<Time>, mut window: ResMut<FrameWindow>) {
    window.push(time.real_delta());
}

/// Everything the text is made from (missing parts hide their lines).
#[derive(Clone, Debug, Default)]
pub struct OverlayInputs {
    /// Average FPS, average frame time and worst frame of the last second.
    pub frames: (f64, Duration, Duration),
    /// Renderer status.
    pub render: Option<RenderInfo>,
    /// Active camera: position and forward direction.
    pub camera: Option<(DVec3, boxel_math::Vec3)>,
    /// Chunk counters.
    pub world: Option<WorldStats>,
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

/// The overlay text, one string per line (ASCII).
pub fn overlay_lines(inputs: &OverlayInputs) -> Vec<String> {
    let mut lines = vec!["boxel debug (F3)".to_string()];
    let (fps, average, worst) = inputs.frames;
    lines.push(format!(
        "FPS {fps:.0}  {:.2} ms avg  worst {:.2} ms (1 s)",
        average.as_secs_f64() * 1e3,
        worst.as_secs_f64() * 1e3
    ));
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
    lines
}

/// Rebuilds `OverlayDraw` from the current state (clears it while hidden).
fn build_overlay(
    overlay: Res<DebugOverlay>,
    window: Res<FrameWindow>,
    render: Option<Res<RenderInfo>>,
    world: Option<Res<WorldStats>>,
    cameras: Query<(&Camera, &Transform)>,
    mut draw: ResMut<OverlayDraw>,
) {
    if !overlay.visible {
        if !draw.rects.is_empty() {
            draw.rects.clear();
        }
        return;
    }
    let inputs = OverlayInputs {
        frames: (window.fps(), window.average(), window.worst()),
        render: render.as_deref().cloned(),
        camera: cameras
            .iter()
            .find(|(camera, _)| camera.active)
            .map(|(_, transform)| (transform.translation, transform.forward())),
        world: world.as_deref().copied(),
    };
    let lines = overlay_lines(&inputs);
    draw.rects.clear();
    layout_text(&lines, &overlay.style, &mut draw.rects);
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
}
