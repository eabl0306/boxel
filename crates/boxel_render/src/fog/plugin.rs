//! [`FogPlugin`]: the fog of an app (ADR 0030). Not in any default set of plugins: a game adds
//! it only if it wants fog; without it the renderer never creates a fog resource.

use super::{FogInput, FogLayer, FogSettings, FogTime};
use crate::plugin::{ExtractedFrame, Render, RenderSystems};
use boxel_app::{App, FixedTime, FixedUpdate, Plugin, RunFixedMainLoop, RunFixedMainLoopSystems};
use boxel_ecs::prelude::*;
use boxel_world::lifecycle::{OpenSystems, WorldOpen};
use std::sync::Arc;

/// The fog clock: fixed steps since the fog started (64 a second), advanced in `FixedUpdate`
/// unless paused, and the fraction of the next step the frame is past it, so the wind moves the
/// noise every frame and not in 64 steps a second. Integers: every machine, tier and frame rate
/// sees the same cells at the same moment. Pause it and set `tick` for a fixed look (tests,
/// screenshots).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FogClock {
    /// Ticks so far.
    pub tick: u64,
    /// Fraction of the next tick (1/65536): the fixed step's overstep, set every frame after the
    /// steps unless paused.
    pub subtick: u16,
    /// Stops the wind.
    pub paused: bool,
}

impl FogClock {
    /// The moment the fog is drawn at.
    pub fn time(&self) -> FogTime {
        FogTime::new(self.tick, self.subtick)
    }
}

/// Adds fog to an app: inserts [`FogSettings`] (from `settings`) and [`FogClock`], advances the
/// clock in `FixedUpdate` and hands each frame's fog to the renderer in
/// `RenderSystems::Extract`. Needs [`crate::RenderPlugin`] to draw anything.
///
/// `extract_fog` checks the settings only when they changed: an invalid change is logged once
/// and the last valid settings stay (invalid settings from the start mean no fog).
///
/// ```ignore
/// app.add_plugins(FogPlugin::new(vec![my_valley_fog(), my_clouds()]));
/// ```
#[derive(Clone, Debug, Default)]
pub struct FogPlugin {
    /// The initial settings.
    pub settings: FogSettings,
}

impl FogPlugin {
    /// Fog with these layers and the default settings.
    pub fn new(layers: Vec<FogLayer>) -> Self {
        Self {
            settings: FogSettings::new(layers),
        }
    }
}

impl Plugin for FogPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.settings.clone())
            .init_resource::<FogClock>()
            .add_systems(FixedUpdate, advance_fog_clock)
            .add_systems(
                RunFixedMainLoop,
                interpolate_fog_clock.in_set(RunFixedMainLoopSystems::AfterFixedMainLoop),
            )
            .add_systems(Render, extract_fog.in_set(RenderSystems::Extract))
            .add_systems(WorldOpen, start_fog_clock.in_set(OpenSystems::Engine));
    }
}

/// `WorldOpen`: every world starts its fog clock at 0, as a fresh process does (a paused clock
/// stays paused).
fn start_fog_clock(mut clock: ResMut<FogClock>) {
    clock.tick = 0;
    clock.subtick = 0;
}

/// `FixedUpdate`: one tick per fixed step.
fn advance_fog_clock(mut clock: ResMut<FogClock>) {
    if !clock.paused {
        clock.tick = clock.tick.wrapping_add(1);
    }
}

/// After the frame's fixed steps: the fraction of the next tick the frame is past the last one.
fn interpolate_fog_clock(mut clock: ResMut<FogClock>, fixed: Option<Res<FixedTime>>) {
    if let (false, Some(fixed)) = (clock.paused, fixed) {
        clock.subtick = subtick(fixed.overstep_fraction_f64());
    }
}

/// An overstep fraction (`[0, 1)`) in 1/65536 of a tick.
fn subtick(fraction: f64) -> u16 {
    (fraction * 65536.0).floor().clamp(0.0, 65535.0) as u16
}

/// The last valid settings and the error last logged.
#[derive(Default)]
struct Checked {
    valid: Option<Arc<FogSettings>>,
    error: Option<boxel_fog::FogError>,
}

/// `RenderSystems::Extract`: the frame's fog from the settings (validated when they changed)
/// and the clock.
fn extract_fog(
    settings: Res<FogSettings>,
    clock: Res<FogClock>,
    frame: Option<ResMut<ExtractedFrame>>,
    mut checked: Local<Checked>,
) {
    if settings.is_changed() {
        match settings.validate() {
            Ok(()) => {
                checked.valid = Some(Arc::new(settings.clone()));
                checked.error = None;
            }
            Err(e) => {
                if checked.error.as_ref() != Some(&e) {
                    let kept = if checked.valid.is_some() {
                        "keeping the last valid fog"
                    } else {
                        "no fog"
                    };
                    tracing::warn!("fog settings: {e}; {kept}");
                    checked.error = Some(e);
                }
            }
        }
    }
    if let Some(mut frame) = frame {
        frame.fog = checked.valid.as_ref().map(|s| FogInput {
            settings: Arc::clone(s),
            time: clock.time(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RenderPlugin;
    use boxel_app::FixedMain;

    fn app(fog: Option<FogPlugin>) -> App {
        let mut app = App::new();
        app.add_plugins(RenderPlugin::default());
        if let Some(fog) = fog {
            app.add_plugins(fog);
        }
        app.finish();
        app.cleanup();
        app
    }

    fn frame_fog(app: &App) -> Option<FogInput> {
        app.world().resource::<ExtractedFrame>().fog.clone()
    }

    #[test]
    fn the_plugin_inserts_its_resources_and_extracts_each_frame() {
        let mut app = app(Some(FogPlugin::new(vec![FogLayer::new(0, 8)])));
        assert_eq!(app.world().resource::<FogClock>(), &FogClock::default());
        assert_eq!(app.world().resource::<FogSettings>().layers.len(), 1);
        app.update();
        let fog = frame_fog(&app).expect("fog this frame");
        assert_eq!(fog.settings.layers, vec![FogLayer::new(0, 8)]);
        assert_eq!(fog.time, FogTime::default());
    }

    #[test]
    fn without_the_plugin_there_is_no_fog() {
        let mut app = app(None);
        app.update();
        assert!(frame_fog(&app).is_none());
        assert!(app.world().get_resource::<FogSettings>().is_none());
        assert!(
            !crate::RendererSmokeTest::default().fog,
            "no fog work in the smoke test"
        );
    }

    #[test]
    fn the_clock_ticks_once_per_fixed_step_unless_paused() {
        let mut app = app(Some(FogPlugin::default()));
        for _ in 0..5 {
            app.world_mut().run_schedule(FixedMain);
        }
        assert_eq!(app.world().resource::<FogClock>().tick, 5);
        app.world_mut().resource_mut::<FogClock>().paused = true;
        app.world_mut().run_schedule(FixedMain);
        assert_eq!(app.world().resource::<FogClock>().tick, 5);
        app.update();
        assert_eq!(frame_fog(&app).expect("fog").time.tick, 5);
    }

    /// Between ticks the clock reads the fixed step's overstep, so the wind moves every frame;
    /// paused, the fraction stays where it was.
    #[test]
    fn the_clock_interpolates_between_ticks() {
        use std::time::Duration;
        let mut app = app(Some(FogPlugin::default()));
        let mut fixed = FixedTime::default();
        fixed.accumulate(fixed.timestep() * 3 / 4);
        app.insert_resource(fixed);
        app.world_mut().run_schedule(RunFixedMainLoop);
        let clock = *app.world().resource::<FogClock>();
        assert!(
            (clock.subtick as i32 - 49152).abs() <= 1,
            "three quarters of a tick: {}",
            clock.subtick
        );
        app.update();
        assert_eq!(frame_fog(&app).expect("fog").time, clock.time());
        let mut paused = app.world_mut().resource_mut::<FogClock>();
        paused.paused = true;
        app.world_mut()
            .resource_mut::<FixedTime>()
            .accumulate(Duration::from_millis(1));
        app.world_mut().run_schedule(RunFixedMainLoop);
        assert_eq!(app.world().resource::<FogClock>().subtick, clock.subtick);
        assert_eq!(subtick(0.0), 0);
        assert_eq!(subtick(0.5), 32768);
        assert_eq!(
            subtick(1.5),
            65535,
            "a late frame stays before the next tick"
        );
    }

    #[test]
    fn a_settings_change_reaches_the_next_frame_and_an_invalid_one_is_ignored() {
        let mut app = app(Some(FogPlugin::new(vec![FogLayer::new(0, 8)])));
        app.update();
        app.world_mut().resource_mut::<FogSettings>().layers[0].max_y = 16;
        app.update();
        let fog = frame_fog(&app).expect("fog");
        assert_eq!(fog.settings.layers[0].max_y, 16);
        // Overlapping layers: the last valid settings stay.
        app.world_mut()
            .resource_mut::<FogSettings>()
            .layers
            .push(FogLayer::new(4, 12));
        app.update();
        let kept = frame_fog(&app).expect("fog");
        assert!(Arc::ptr_eq(&kept.settings, &fog.settings));
        // Fixed again: the new settings.
        app.world_mut().resource_mut::<FogSettings>().layers[1] = FogLayer::new(20, 24);
        app.update();
        assert_eq!(frame_fog(&app).expect("fog").settings.layers.len(), 2);
    }

    #[test]
    fn invalid_settings_from_the_start_mean_no_fog() {
        let mut app = app(Some(FogPlugin::new(vec![FogLayer::new(8, 0)])));
        app.update();
        assert!(frame_fog(&app).is_none());
    }
}
