//! Camera tests: camera-relative view math and the fly controller driven by input resources and a
//! manually advanced `Time` (headless, no window).
use boxel_app::{App, PreUpdate, Time, Update};
use boxel_camera::{Camera, CameraView, FlyCamera, FlyCameraPlugin};
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::{LogLevel, ScheduleBuildSettings, ScheduleLabel};
use boxel_math::{Aabb, DVec3, Quat, Vec2, Vec3};
use boxel_transform::Transform;
use boxel_window::{
    ButtonState, CursorGrab, CursorOptions, InputPlugin, KeyCode, KeyboardInput, MouseButton,
    MouseButtonInput, MouseMotion, WindowFocused,
};
use std::f32::consts::FRAC_PI_2;
use std::time::{Duration, Instant};

const DT: Duration = Duration::from_millis(100);

// ---- CameraView ----

#[test]
fn eye_rel_stays_in_the_unit_cube_at_large_coordinates() {
    let camera = Camera::default();
    for (x, y, z) in [
        (1e7 + 0.25, 1e7, 1e7 + 0.999),
        (-1e7 + 0.75, -1e7, -1e7 - 0.5),
        (12_345_678.5, 64.125, -98_765_432.875),
    ] {
        let transform = Transform::from_yaw_pitch(DVec3::new(x, y, z), 0.7, -0.3);
        let view = CameraView::compute(&camera, &transform, 16.0 / 9.0);
        let rel = view.eye_rel;
        assert!(
            rel.cmpge(Vec3::ZERO).all() && rel.cmplt(Vec3::ONE).all(),
            "eye_rel {rel} out of [0, 1) at ({x}, {y}, {z})"
        );
        let back = view.render_origin.as_dvec3() + rel.as_dvec3();
        assert!(
            back.distance(transform.translation) < 1e-6,
            "{back} vs {}",
            transform.translation
        );
    }
}

#[test]
fn frustum_keeps_points_ahead_and_rejects_points_behind() {
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(DVec3::new(1e7 + 0.5, 70.25, -3e6), 1.1, 0.2);
    let view = CameraView::compute(&camera, &transform, 16.0 / 9.0);
    let point = |distance: f32| {
        Aabb::from_center_half_extents(view.eye_rel + view.forward * distance, Vec3::splat(0.01))
    };
    assert!(view.forward.is_normalized());
    assert!(view.frustum.intersects_aabb(&point(10.0)));
    assert!(view.frustum.intersects_aabb(&point(300.0)));
    assert!(!view.frustum.intersects_aabb(&point(-10.0)));
    // Beyond the view distance (384 by default).
    assert!(!view.frustum.intersects_aabb(&point(500.0)));
    // A point ahead also lands inside clip space with reverse-Z depth in (0, 1].
    let clip = view.view_proj * (view.eye_rel + view.forward * 10.0).extend(1.0);
    let ndc = clip.truncate() / clip.w;
    assert!(ndc.x.abs() < 1e-3 && ndc.y.abs() < 1e-3, "{ndc}");
    assert!(ndc.z > 0.0 && ndc.z <= 1.0, "{ndc}");
}

#[test]
fn near_reach_is_the_distance_to_the_near_plane_corners() {
    for (camera, aspect) in [
        (Camera::default(), 16.0 / 9.0),
        (Camera::default(), 0.5),
        (
            Camera {
                fov_y_radians: 100f32.to_radians(),
                near: 0.05,
                ..Camera::default()
            },
            21.0 / 9.0,
        ),
    ] {
        // The corners of the near rectangle: clip space (+-1, +-1) at depth 1 (reverse-Z).
        let inverse = camera.projection(aspect).inverse();
        let reach = camera.near_reach(aspect);
        for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let corner = inverse * boxel_math::Vec4::new(x, y, 1.0, 1.0);
            let distance = (corner.truncate() / corner.w).length();
            assert!(
                (distance - reach).abs() < reach * 1e-4,
                "{distance} vs {reach} at aspect {aspect}"
            );
        }
    }
    // The default camera at 16:9: about 0.17 voxels.
    let reach = Camera::default().near_reach(16.0 / 9.0);
    assert!((0.17..0.18).contains(&reach), "{reach}");
}

// ---- FlyCameraPlugin ----

struct Harness {
    app: App,
    camera: Entity,
    now: Instant,
}

impl Harness {
    fn new(fly: FlyCamera) -> Self {
        let mut app = App::new();
        let now = Instant::now();
        let mut time = Time::new(now);
        time.update_with_instant(now);
        app.add_plugins((InputPlugin, FlyCameraPlugin))
            .insert_resource(time);
        let transform = Transform::from_yaw_pitch(DVec3::new(10.0, 64.0, -5.0), fly.yaw, fly.pitch);
        let camera = app.world_mut().spawn((fly, transform)).id();
        Self { app, camera, now }
    }

    /// Advances `Time` by `DT` and runs one frame.
    fn step(&mut self) {
        self.now += DT;
        self.app
            .world_mut()
            .resource_mut::<Time>()
            .update_with_instant(self.now);
        self.app.update();
    }

    fn key(&mut self, key: KeyCode, state: ButtonState) -> &mut Self {
        self.app.world_mut().write_message(KeyboardInput {
            key,
            state,
            repeat: false,
        });
        self
    }

    fn hold(&mut self, keys: &[KeyCode]) -> &mut Self {
        for &key in keys {
            self.key(key, ButtonState::Pressed);
        }
        self
    }

    fn click(&mut self, button: MouseButton) -> &mut Self {
        self.app.world_mut().write_message(MouseButtonInput {
            button,
            state: ButtonState::Pressed,
        });
        self
    }

    fn mouse(&mut self, delta: Vec2) -> &mut Self {
        self.app.world_mut().write_message(MouseMotion { delta });
        self
    }

    fn lock_cursor(&mut self) -> &mut Self {
        *self.app.world_mut().resource_mut::<CursorOptions>() = CursorOptions {
            grab: CursorGrab::Locked,
            visible: false,
        };
        self
    }

    fn transform(&self) -> Transform {
        *self.app.world().get::<Transform>(self.camera).unwrap()
    }

    fn fly(&self) -> FlyCamera {
        *self.app.world().get::<FlyCamera>(self.camera).unwrap()
    }

    fn cursor(&self) -> CursorOptions {
        *self.app.world().resource::<CursorOptions>()
    }
}

fn assert_close(a: DVec3, b: DVec3) {
    assert!(a.distance(b) < 1e-9, "{a} != {b}");
}

/// Translation change after holding `keys` for one `DT` step.
fn moved(fly: FlyCamera, keys: &[KeyCode]) -> DVec3 {
    let mut h = Harness::new(fly);
    let start = h.transform().translation;
    h.hold(keys).step();
    h.transform().translation - start
}

#[test]
fn plugin_inserts_a_free_visible_cursor() {
    let h = Harness::new(FlyCamera::default());
    assert_eq!(
        h.cursor(),
        CursorOptions {
            grab: CursorGrab::None,
            visible: true
        }
    );
}

#[test]
fn wasd_moves_on_the_yaw_plane() {
    let fly = FlyCamera::default();
    // 20 units/s for 0.1 s; yaw 0 looks down -Z.
    assert_close(moved(fly, &[KeyCode::KeyW]), DVec3::new(0.0, 0.0, -2.0));
    assert_close(moved(fly, &[KeyCode::KeyS]), DVec3::new(0.0, 0.0, 2.0));
    assert_close(moved(fly, &[KeyCode::KeyD]), DVec3::new(2.0, 0.0, 0.0));
    assert_close(moved(fly, &[KeyCode::KeyA]), DVec3::new(-2.0, 0.0, 0.0));
    assert_close(moved(fly, &[KeyCode::Space]), DVec3::new(0.0, 2.0, 0.0));
    assert_close(
        moved(fly, &[KeyCode::ShiftLeft]),
        DVec3::new(0.0, -2.0, 0.0),
    );
    assert_close(moved(fly, &[KeyCode::KeyW, KeyCode::KeyS]), DVec3::ZERO);

    // Yaw +90 degrees turns left: forward becomes -X. Pitch never tilts the movement.
    let turned = FlyCamera {
        yaw: FRAC_PI_2,
        pitch: -1.0,
        ..fly
    };
    let delta = moved(turned, &[KeyCode::KeyW]);
    assert!(delta.distance(DVec3::new(-2.0, 0.0, 0.0)) < 1e-6, "{delta}");
}

#[test]
fn diagonals_are_normalized_and_boost_multiplies() {
    let fly = FlyCamera::default();
    let diagonal = moved(fly, &[KeyCode::KeyW, KeyCode::KeyD, KeyCode::Space]);
    assert!((diagonal.length() - 2.0).abs() < 1e-9, "{diagonal}");
    let boosted = moved(fly, &[KeyCode::KeyW, KeyCode::ControlLeft]);
    assert_close(boosted, DVec3::new(0.0, 0.0, -10.0));
}

#[test]
fn movement_keeps_going_while_held_and_uses_time() {
    let mut h = Harness::new(FlyCamera::default());
    let start = h.transform().translation;
    h.hold(&[KeyCode::KeyW]).step();
    h.step();
    h.step();
    assert_close(
        h.transform().translation - start,
        DVec3::new(0.0, 0.0, -6.0),
    );
    h.key(KeyCode::KeyW, ButtonState::Released).step();
    h.step();
    assert_close(
        h.transform().translation - start,
        DVec3::new(0.0, 0.0, -6.0),
    );
}

#[test]
fn mouse_look_only_while_locked() {
    let mut h = Harness::new(FlyCamera::default());
    let start = h.transform();
    h.mouse(Vec2::new(100.0, 50.0)).step();
    assert_eq!(h.transform(), start);
    assert_eq!(h.fly().yaw, 0.0);

    h.lock_cursor().mouse(Vec2::new(100.0, 50.0)).step();
    let fly = h.fly();
    // 0.002 rad per unit: yaw -= 0.2, pitch -= 0.1.
    assert!(
        (fly.yaw + 0.2).abs() < 1e-6 && (fly.pitch + 0.1).abs() < 1e-6,
        "{fly:?}"
    );
    let expected = Quat::from_rotation_y(fly.yaw) * Quat::from_rotation_x(fly.pitch);
    assert!(h.transform().rotation.abs_diff_eq(expected, 1e-6));
    assert_eq!(h.transform().translation, start.translation);
}

#[test]
fn pitch_is_clamped_and_yaw_wraps() {
    let mut h = Harness::new(FlyCamera::default());
    h.lock_cursor().mouse(Vec2::new(0.0, -10_000.0)).step();
    assert!((h.fly().pitch - 89f32.to_radians()).abs() < 1e-6);
    h.mouse(Vec2::new(0.0, 20_000.0)).step();
    assert!((h.fly().pitch + 89f32.to_radians()).abs() < 1e-6);

    // Ten full turns to the right keep yaw in [-PI, PI] with the same orientation.
    let turns = 10.0 * std::f32::consts::TAU / 0.002;
    h.mouse(Vec2::new(turns + 250.0, 0.0)).step();
    let yaw = h.fly().yaw;
    assert!(yaw.abs() <= std::f32::consts::PI, "{yaw}");
    assert!((yaw + 0.5).abs() < 1e-3, "{yaw}");
}

#[test]
fn click_grabs_and_escape_releases_the_cursor() {
    let mut h = Harness::new(FlyCamera::default());
    h.click(MouseButton::Left).step();
    assert_eq!(
        h.cursor(),
        CursorOptions {
            grab: CursorGrab::Locked,
            visible: false
        }
    );
    // Right click does nothing; Escape releases.
    h.click(MouseButton::Right).step();
    assert_eq!(h.cursor().grab, CursorGrab::Locked);
    h.key(KeyCode::Escape, ButtonState::Pressed).step();
    assert_eq!(
        h.cursor(),
        CursorOptions {
            grab: CursorGrab::None,
            visible: true
        }
    );
}

#[test]
fn losing_the_focus_releases_the_cursor() {
    let mut h = Harness::new(FlyCamera::default());
    h.click(MouseButton::Left).step();
    assert_eq!(h.cursor().grab, CursorGrab::Locked);
    // Alt-tab: the OS frees the cursor, and so does the controller.
    h.app.world_mut().write_message(WindowFocused(false));
    h.step();
    assert_eq!(
        h.cursor(),
        CursorOptions {
            grab: CursorGrab::None,
            visible: true
        }
    );
    // Focus back: nothing changes until a click grabs it again.
    h.app.world_mut().write_message(WindowFocused(true));
    h.step();
    assert_eq!(h.cursor().grab, CursorGrab::None);
    h.click(MouseButton::Left).step();
    assert_eq!(h.cursor().grab, CursorGrab::Locked);
}

#[test]
fn disabled_cameras_ignore_input() {
    let mut h = Harness::new(FlyCamera {
        enabled: false,
        ..FlyCamera::default()
    });
    let start = h.transform();
    h.click(MouseButton::Left).hold(&[KeyCode::KeyW]).step();
    assert_eq!(h.cursor().grab, CursorGrab::None);
    h.lock_cursor().mouse(Vec2::new(30.0, 30.0)).step();
    assert_eq!(h.transform(), start);
    assert_eq!(h.fly().yaw, 0.0);
}

#[test]
fn fly_camera_systems_are_not_ambiguous() {
    let mut h = Harness::new(FlyCamera::default());
    for label in [PreUpdate.intern(), Update.intern()] {
        h.app.edit_schedule(label, |schedule| {
            schedule.set_build_settings(ScheduleBuildSettings {
                ambiguity_detection: LogLevel::Error,
                ..ScheduleBuildSettings::default()
            });
        });
    }
    h.click(MouseButton::Left)
        .mouse(Vec2::ONE)
        .hold(&[KeyCode::KeyW])
        .step();
}
