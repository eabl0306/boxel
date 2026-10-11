//! [`LodPlugin`]: the far LOD of an app (ADR 0031). Not in any default set of plugins: a game
//! adds it only if it wants terrain past the view distance; without it nothing of the LOD exists
//! (no resource, no system, no job, no pipeline) and frames are what they were before.
//!
//! Per frame:
//! - `Update`: the active camera becomes [`LodFocus`], then the world side runs
//!   ([`LodSystems`]: coverage, plan, jobs, the frame's ops).
//! - `RenderSystems::Extract`, after the render plugin's own extract and before its cave culling
//!   search: the ops become the frame's [`LodInput`]. A frame whose ops cannot reach a renderer
//!   (none ready, a lost device, a resync, a new LOD arena) drops them and writes [`LodResync`]:
//!   the world side sends every node again, coarse first.
//! - The render plugin does the rest: `Prepare` hands the input to the renderer (unless the LOD
//!   failed its smoke check on this device), the cave culling search reaches the horizon while
//!   the LOD draws ([`lod_far`]), and the refusals of the main renderer and of the LOD pass go
//!   back through [`LodFeedback`](super::LodFeedback).

use super::{LodInput, LodOp, LodResync, LodSettings, LodStats};
use crate::plugin::{ExtractedFrame, Render, RenderSystems, extract, extract_visibility};
use boxel_app::{App, Plugin, Update};
use boxel_camera::{Camera, CameraView};
use boxel_ecs::prelude::*;
use boxel_transform::Transform;
use boxel_window::WindowState;
use boxel_world::jobs::{JobExecutor, TaskPoolExecutor};
use boxel_world::lod::{LodFocus, LodOps, LodSystems, LodWorldPlugin};
use std::sync::Arc;

/// Width over height of the focus when no window size is known.
const DEFAULT_ASPECT: f32 = 16.0 / 9.0;

/// Adds the far LOD to an app (module docs): [`LodSettings`] (from `settings`) and the world side
/// ([`LodWorldPlugin`], its jobs on `executor`), the focus system and the extract step. Add it
/// after `WorldPlugin` and `RenderPlugin`.
///
/// ```ignore
/// app.add_plugins(LodPlugin::new(LodSettings { ring_radius: 16, levels: 3, ..default() }));
/// ```
///
/// Change it at runtime through `ResMut<LodSettings>`: `enabled = false` drops every node and
/// gives back today's frames; another `arena_quads` rebuilds the LOD pass, which fills again
/// coarse first.
pub struct LodPlugin {
    /// The initial settings.
    pub settings: LodSettings,
    /// Where the column jobs run (the default: the async compute pool, like the world's).
    pub executor: Arc<dyn JobExecutor>,
}

impl LodPlugin {
    /// The LOD with these settings.
    pub fn new(settings: LodSettings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }
}

impl Default for LodPlugin {
    /// The default settings (`R = 24`, `K = 4`: a horizon of 12,288 blocks) on the async compute
    /// pool.
    fn default() -> Self {
        Self {
            settings: LodSettings::default(),
            executor: Arc::new(TaskPoolExecutor),
        }
    }
}

impl Plugin for LodPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(LodWorldPlugin {
            settings: self.settings.clone(),
            executor: Arc::clone(&self.executor),
        })
        // Its order against the systems that move the camera does not matter (one frame of lag
        // at most), as for the world's own viewer tracking.
        .add_systems(
            Update,
            lod_focus.ambiguous_with_all().before(LodSystems::Plan),
        )
        .add_systems(
            Render,
            extract_lod
                .in_set(RenderSystems::Extract)
                .after(extract)
                .before(extract_visibility),
        );
    }
}

/// `Update`, before the plan: the active camera as [`LodFocus`], at the window's aspect (removed
/// while no camera is active: the world side then plans around its first viewer).
fn lod_focus(
    mut commands: Commands,
    cameras: Query<(&Camera, &Transform)>,
    window: Option<Res<WindowState>>,
    focus: Option<ResMut<LodFocus>>,
) {
    let Some((camera, transform)) = cameras.iter().find(|(camera, _)| camera.active) else {
        if focus.is_some() {
            commands.remove_resource::<LodFocus>();
        }
        return;
    };
    let aspect = window
        .map(|w| w.physical_size)
        .filter(|s| s.x > 0 && s.y > 0)
        .map_or(DEFAULT_ASPECT, |s| s.x as f32 / s.y as f32);
    let new = focus_of(camera, transform, aspect);
    match focus {
        Some(mut focus) => {
            focus.set_if_neq(new);
        }
        None => commands.insert_resource(new),
    }
}

/// The focus of `camera` at `transform`: the render origin and eye of its view, as the renderer
/// computes them.
fn focus_of(camera: &Camera, transform: &Transform, aspect: f32) -> LodFocus {
    let view = CameraView::compute(camera, transform, aspect);
    LodFocus {
        origin: view.render_origin,
        eye_rel: view.eye_rel,
        forward: view.forward,
        fov_y: camera.fov_y_radians,
        aspect,
    }
}

/// What the extract step knows of the renderer this frame, copied by the render plugin's
/// `extract` before `Prepare` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LodRenderer {
    /// The LOD can draw on it: it passed its smoke check, the backend is not GL and its LOD pass
    /// did not fail to build.
    pub(crate) drawable: bool,
    /// The `arena_quads` its LOD pass was built with, `None` without one.
    pub(crate) arena_quads: Option<u32>,
}

/// What [`extract_lod`] keeps between frames.
#[derive(Default)]
pub(crate) struct Extracted {
    /// The last valid settings (the world side logs invalid ones).
    valid: Option<LodSettings>,
    /// Ops went to a renderer since the last [`LodResync`]: when that renderer goes away, the
    /// world side must hear of it.
    sent: bool,
}

/// `RenderSystems::Extract`, after the render plugin's `extract` and before its cave culling
/// search: the frame's [`LodInput`] ([`frame_input`]).
pub(crate) fn extract_lod(
    settings: Res<LodSettings>,
    stats: Res<LodStats>,
    mut ops: ResMut<LodOps>,
    frame: Option<ResMut<ExtractedFrame>>,
    mut resyncs: MessageWriter<LodResync>,
    mut state: Local<Extracted>,
) {
    if settings.is_changed() && settings.validate().is_ok() {
        state.valid = Some(settings.clone());
    }
    // No render plugin: the world side drops the ops nobody takes.
    let Some(mut frame) = frame else {
        return;
    };
    let horizon = stats.off.is_none().then_some(stats.horizon as f32);
    let state = &mut *state;
    let (input, resync) = frame_input(
        frame.lod_renderer,
        frame.resync,
        state.valid.as_ref(),
        horizon,
        ops.take(),
        &mut state.sent,
    );
    frame.lod = input;
    if resync {
        resyncs.write(LodResync);
    }
}

/// One frame of LOD for the renderer, and whether the world side must send every node again
/// ([`LodResync`]):
/// - no renderer (none yet, or its device is lost: the next one starts without LOD nodes), or no
///   valid settings: no input; the ops are dropped, and a resync is due if any op was dropped or
///   had reached a renderer since the last one (`sent`);
/// - a resync frame (the renderer drops every chunk: a new renderer, or a frame whose uploads
///   were lost, LOD copies included) or another `arena_quads` than the LOD pass was built with
///   (the renderer builds it again, empty): an input with no op that drops every node, and a
///   resync;
/// - else the ops, in order.
pub(crate) fn frame_input(
    renderer: Option<LodRenderer>,
    resync: bool,
    settings: Option<&LodSettings>,
    horizon: Option<f32>,
    ops: Vec<LodOp>,
    sent: &mut bool,
) -> (Option<LodInput>, bool) {
    let (Some(renderer), Some(settings)) = (renderer, settings) else {
        let due = *sent || !ops.is_empty();
        *sent = false;
        return (None, due);
    };
    let mut input = LodInput {
        ops,
        ..LodInput::new(settings.clone(), horizon)
    };
    let new_arena = renderer
        .arena_quads
        .is_some_and(|quads| quads != settings.arena_quads);
    if resync || new_arena {
        input.ops.clear();
        input.resync = true;
        *sent = false;
        return (Some(input), true);
    }
    *sent |= !input.ops.is_empty();
    (Some(input), false)
}

/// The horizon when `Prepare` draws the far LOD this frame (its input draws, [`LodInput::far`],
/// on a renderer where the LOD can draw), else `None`. The cave culling search reaches that far:
/// the renderer then draws every chunk inside it.
pub(crate) fn lod_far(frame: &ExtractedFrame) -> Option<f32> {
    frame.lod_renderer.filter(|r| r.drawable)?;
    frame.lod.as_ref()?.far()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{frame_visibility, frame_visibility_with};
    use crate::renderer::far_plane_at;
    use crate::{RenderPlugin, ViewParams, cull_counts};
    use boxel_app::App;
    use boxel_ecs::message::Messages;
    use boxel_ecs::schedule::Schedule;
    use boxel_math::{DVec3, I64Vec3, Quat, UVec2};
    use boxel_voxel::{ChunkKey, ChunkPos, DimensionId};
    use boxel_world::jobs::InlineExecutor;
    use boxel_world::{
        ChunkMap, ChunkViewer, ChunkVisibility, WorldContent, WorldPlugin, WorldSettings,
        WorldStats,
    };
    use boxel_worldgen::FlatGenerator;

    fn renderer(drawable: bool, arena_quads: Option<u32>) -> Option<LodRenderer> {
        Some(LodRenderer {
            drawable,
            arena_quads,
        })
    }

    fn remove(x: i64) -> LodOp {
        LodOp::Remove {
            key: ChunkKey {
                dim: DimensionId(0),
                lod: 2,
                pos: ChunkPos::new(x, 0, 0),
            },
        }
    }

    /// A world of flat ground (4 blocks of `test:solid` from y = 0) and nothing else.
    fn flat_world() -> WorldPlugin {
        WorldPlugin {
            generator: Arc::new(FlatGenerator::new(vec![("test:solid".into(), 4)])),
            registry: Arc::new(boxel_voxel::testing::registry()),
            settings: WorldSettings {
                interest_budget: std::time::Duration::MAX,
                job_budget: std::time::Duration::MAX,
                max_jobs_in_flight: 4096,
                ..WorldSettings::default()
            },
            executor: Arc::new(InlineExecutor),
        }
    }

    #[test]
    fn the_plugin_adds_the_world_side_and_no_renderer_means_no_input() {
        let mut app = App::new();
        app.add_plugins((RenderPlugin::default(), flat_world()));
        app.add_plugins(LodPlugin {
            executor: Arc::new(InlineExecutor),
            ..LodPlugin::default()
        });
        app.finish();
        app.cleanup();
        for _ in 0..3 {
            app.update();
        }
        let world = app.world();
        assert_eq!(world.resource::<LodSettings>(), &LodSettings::default());
        assert!(world.contains_resource::<LodStats>());
        assert!(world.contains_resource::<LodOps>());
        assert!(world.contains_resource::<super::super::LodFeedback>());
        // No window, so no renderer: nothing for one.
        assert!(world.resource::<ExtractedFrame>().lod.is_none());
        assert!(world.resource::<ExtractedFrame>().lod_renderer.is_none());
    }

    #[test]
    fn without_the_plugin_there_is_no_lod() {
        let mut app = App::new();
        app.add_plugins((RenderPlugin::default(), flat_world()));
        app.finish();
        app.cleanup();
        app.update();
        let world = app.world();
        assert!(world.get_resource::<LodSettings>().is_none());
        assert!(world.get_resource::<LodOps>().is_none());
        assert!(world.get_resource::<LodFocus>().is_none());
        assert!(world.resource::<ExtractedFrame>().lod.is_none());
        assert!(
            !crate::RendererSmokeTest::default().lod,
            "no LOD work in the smoke test"
        );
    }

    #[test]
    fn ops_reach_a_ready_renderer_in_order() {
        let settings = LodSettings::default();
        let mut sent = false;
        let (input, resync) = frame_input(
            renderer(true, Some(settings.arena_quads)),
            false,
            Some(&settings),
            Some(12288.0),
            vec![remove(1), remove(2)],
            &mut sent,
        );
        let input = input.expect("an input");
        assert!(!resync && !input.resync && sent);
        assert_eq!(input.horizon, Some(12288.0));
        assert_eq!(input.ops.len(), 2);
        assert!(matches!(input.ops[1], LodOp::Remove { key } if key.pos.x == 2));
        // The first input of a renderer (no LOD pass yet) is no arena change.
        let (input, resync) = frame_input(
            renderer(true, None),
            false,
            Some(&settings),
            None,
            vec![remove(3)],
            &mut sent,
        );
        assert!(!resync && input.is_some_and(|i| i.ops.len() == 1 && i.far().is_none()));
    }

    /// Every frame whose ops cannot reach the renderer's LOD nodes asks the world side for every
    /// node again: no renderer (a lost device is no renderer to `extract`), a resync frame and a
    /// new LOD arena.
    #[test]
    fn dropped_ops_and_dropped_nodes_ask_for_a_resync() {
        let settings = LodSettings::default();
        let mut sent = false;
        // No renderer and nothing ever sent: nothing to tell.
        let (input, resync) = frame_input(None, false, Some(&settings), None, vec![], &mut sent);
        assert!(input.is_none() && !resync);
        // Ops dropped with no renderer.
        let (input, resync) = frame_input(
            None,
            false,
            Some(&settings),
            None,
            vec![remove(1)],
            &mut sent,
        );
        assert!(input.is_none() && resync && !sent);
        // A renderer took ops, then went away (its device was lost) with no new op.
        let ready = renderer(true, Some(settings.arena_quads));
        frame_input(
            ready,
            false,
            Some(&settings),
            None,
            vec![remove(1)],
            &mut sent,
        );
        assert!(sent);
        let (input, resync) = frame_input(None, false, Some(&settings), None, vec![], &mut sent);
        assert!(input.is_none() && resync && !sent);
        // Once told, a renderer still away asks no more.
        let (_, resync) = frame_input(None, false, Some(&settings), None, vec![], &mut sent);
        assert!(!resync);
        // A resync frame: the renderer drops every node and gets no op.
        let (input, resync) = frame_input(
            ready,
            true,
            Some(&settings),
            Some(1.0),
            vec![remove(1)],
            &mut sent,
        );
        let input = input.expect("an input that drops the nodes");
        assert!(resync && input.resync && input.ops.is_empty() && !sent);
        // Another arena (F8 to a bigger preset): the pass is built again, empty.
        let bigger = LodSettings {
            arena_quads: 16 << 20,
            ..settings.clone()
        };
        let (input, resync) = frame_input(
            ready,
            false,
            Some(&bigger),
            Some(1.0),
            vec![remove(1)],
            &mut sent,
        );
        let input = input.expect("an input");
        assert!(resync && input.resync && input.ops.is_empty());
        assert_eq!(input.settings.arena_quads, 16 << 20);
        // No valid settings yet: no input.
        let (input, _) = frame_input(ready, false, None, Some(1.0), vec![], &mut sent);
        assert!(input.is_none());
    }

    /// The system around [`frame_input`]: it keeps the last valid settings and its `sent` state
    /// between frames, takes the outbox and writes the message.
    #[test]
    fn extract_lod_takes_the_ops_and_writes_the_resyncs() {
        let mut world = World::new();
        world.insert_resource(LodSettings::default());
        world.insert_resource(LodStats {
            horizon: 4096.0,
            ..LodStats::default()
        });
        world.insert_resource(LodOps {
            ops: vec![remove(1)],
        });
        let mut frame = ExtractedFrame::default();
        frame.lod_renderer = renderer(true, Some(LodSettings::default().arena_quads));
        world.insert_resource(frame);
        world.init_resource::<Messages<LodResync>>();
        let mut schedule = Schedule::default();
        schedule.add_systems(extract_lod);
        let resyncs =
            |world: &mut World| world.resource_mut::<Messages<LodResync>>().drain().count();

        schedule.run(&mut world);
        let input = world
            .resource::<ExtractedFrame>()
            .lod
            .clone()
            .expect("input");
        assert_eq!((input.ops.len(), input.far()), (1, Some(4096.0)));
        assert!(
            world.resource::<LodOps>().ops.is_empty(),
            "the outbox is taken"
        );
        assert_eq!(resyncs(&mut world), 0);

        // An invalid change keeps the last valid settings.
        world.resource_mut::<LodSettings>().ring_radius = 0;
        world.resource_mut::<LodStats>().off = Some(boxel_world::lod::LodOff::Guard);
        schedule.run(&mut world);
        let input = world
            .resource::<ExtractedFrame>()
            .lod
            .clone()
            .expect("input");
        assert_eq!(input.settings.ring_radius, 24);
        assert_eq!(input.far(), None, "no horizon while the world side is off");

        // The renderer goes away after ops reached it.
        world.resource_mut::<ExtractedFrame>().lod_renderer = None;
        schedule.run(&mut world);
        assert!(world.resource::<ExtractedFrame>().lod.is_none());
        assert_eq!(resyncs(&mut world), 1);

        // A resync frame.
        world.resource_mut::<LodOps>().ops.push(remove(2));
        let mut frame = world.resource_mut::<ExtractedFrame>();
        frame.lod_renderer = renderer(true, None);
        frame.resync = true;
        schedule.run(&mut world);
        let input = world
            .resource::<ExtractedFrame>()
            .lod
            .clone()
            .expect("input");
        assert!(input.resync && input.ops.is_empty());
        assert_eq!(resyncs(&mut world), 1);
    }

    /// `lod_far` is `Some` exactly when `prepare` draws the LOD: an input that draws (enabled,
    /// a horizon) and a renderer where the LOD can draw.
    #[test]
    fn lod_far_follows_what_prepare_draws() {
        let on = LodSettings::default();
        let off = LodSettings {
            enabled: false,
            ..LodSettings::default()
        };
        let frame = |settings: Option<&LodSettings>, horizon, renderer| {
            let mut frame = ExtractedFrame::default();
            frame.lod = settings.map(|s| LodInput::new(s.clone(), horizon));
            frame.lod_renderer = renderer;
            frame
        };
        let ready = renderer(true, None);
        assert_eq!(lod_far(&frame(Some(&on), Some(8.0), ready)), Some(8.0));
        assert_eq!(lod_far(&frame(None, Some(8.0), ready)), None, "no plugin");
        assert_eq!(lod_far(&frame(Some(&on), None, ready)), None, "the guard");
        assert_eq!(lod_far(&frame(Some(&off), Some(8.0), ready)), None);
        assert_eq!(
            lod_far(&frame(Some(&on), Some(8.0), None)),
            None,
            "no renderer"
        );
        assert_eq!(
            lod_far(&frame(Some(&on), Some(8.0), renderer(false, None))),
            None,
            "smoke failed, GL or a failed pass"
        );
    }

    #[test]
    fn the_focus_follows_the_active_camera() {
        let mut app = App::new();
        app.add_systems(Update, lod_focus);
        app.insert_resource(WindowState {
            physical_size: UVec2::new(1000, 500),
            ..WindowState::default()
        });
        let at = Transform::from_translation(DVec3::new(1e12 + 0.25, 70.5, -3.75))
            .with_rotation(Quat::from_rotation_y(0.5));
        let camera = app.world_mut().spawn((Camera::default(), at)).id();
        app.update();
        app.update();
        let focus = *app.world().resource::<LodFocus>();
        assert_eq!(focus, focus_of(&Camera::default(), &at, 2.0));
        assert_eq!(focus.origin, I64Vec3::new(1_000_000_000_000, 70, -4));
        assert_eq!(focus.aspect, 2.0);
        app.world_mut()
            .get_mut::<Camera>(camera)
            .expect("camera")
            .active = false;
        app.update();
        app.update();
        assert!(app.world().get_resource::<LodFocus>().is_none());
    }

    /// A flat world settled around a viewer (r8 v8) at `from`, then again after it moved to
    /// `eye`: the chunks meshed around `from` that lie in the keep band of `eye` stay published.
    fn settled(from: DVec3, eye: DVec3) -> App {
        let mut app = App::new();
        app.add_plugins(flat_world());
        let viewer = app
            .world_mut()
            .spawn((
                Transform::from_translation(from),
                ChunkViewer::new(RADIUS).with_vertical_radius(RADIUS),
            ))
            .id();
        for at in [from, eye] {
            app.world_mut()
                .get_mut::<Transform>(viewer)
                .expect("viewer")
                .translation = at;
            let mut frames = 0;
            loop {
                app.update();
                let stats = app.world().resource::<WorldStats>();
                if frames > 1 && stats.chunks > 0 && stats.is_settled() {
                    break;
                }
                frames += 1;
                assert!(frames < 200, "the flat world did not settle");
            }
        }
        app
    }

    const RADIUS: u32 = 8;
    const HORIZON: f32 = 12288.0;

    /// With the LOD on, the cave culling search walks out to the horizon: every published chunk
    /// the renderer draws with its far plane there (the cull view) stays visible, for the two
    /// poses that reach past the view distance inside the searched box: a diagonal look along
    /// the ground toward a corner of the box, over the keep band of a viewer that came back from
    /// there, and an eye 200 blocks up pitched 20 degrees down. With the camera's far plane those
    /// poses lose chunks (the bug the LOD's far plane fixes), and without a far plane the search
    /// is `frame_visibility`'s.
    #[test]
    fn the_search_reaches_the_horizon_with_the_lod() {
        let camera = Camera {
            view_distance: (RADIUS * 32) as f32,
            ..Camera::default()
        };
        let aspect = 16.0 / 9.0;
        for (name, from, eye, yaw, pitch) in [
            (
                "diagonal",
                DVec3::new(80.5, 6.5, -47.5),
                DVec3::new(16.5, 6.5, 16.5),
                -45f64,
                0f64,
            ),
            (
                "high",
                DVec3::new(16.5, 204.5, 16.5),
                DVec3::new(16.5, 204.5, 16.5),
                30.0,
                -20.0,
            ),
        ] {
            let mut app = settled(from, eye);
            let transform =
                Transform::from_yaw_pitch(eye, yaw.to_radians() as f32, pitch.to_radians() as f32);
            let viewer = ChunkViewer::new(RADIUS).with_vertical_radius(RADIUS);
            let world = app.world_mut();
            let registry = Arc::clone(&world.resource::<WorldContent>().registry);
            let map = world.resource::<ChunkMap>();
            let search = |far| {
                frame_visibility_with(
                    false,
                    &registry,
                    map,
                    &mut ChunkVisibility::default(),
                    &camera,
                    &transform,
                    viewer,
                    aspect,
                    far,
                )
                .expect("the eye sees from open space")
            };
            let with_lod = search(Some(HORIZON));
            let without = search(None);
            let plain = frame_visibility(
                false,
                &registry,
                map,
                &mut ChunkVisibility::default(),
                &camera,
                &transform,
                viewer,
                aspect,
            )
            .expect("a set");
            assert_eq!(*plain, *without, "{name}: no far plane is frame_visibility");

            // The renderer's cull view with the LOD: the far plane at the horizon.
            let cv = CameraView::compute(&camera, &transform, aspect);
            let mut view = ViewParams::from_camera(&cv, &camera, [0.0; 4], 0.6);
            far_plane_at(&mut view.frustum, view.eye_rel, HORIZON);
            let published: Vec<_> = map
                .ready_meshes()
                .map(|(_, key, mesh)| (key.pos.origin().as_i64vec3(), &**mesh))
                .collect();
            let drawn = |set: Option<&boxel_world::VisibleSet>| {
                cull_counts(&view, published.iter().map(|(o, m)| (*o, *m)), set).chunks
            };
            let all = drawn(None);
            assert!(all > 50, "{name}: {all} chunks in view");
            assert_eq!(
                drawn(Some(&with_lod)),
                all,
                "{name}: a chunk the LOD left is hidden"
            );
            let lost = all - drawn(Some(&without));
            assert!(
                lost > 0,
                "{name}: the camera's far plane loses nothing here"
            );
        }
    }
}
