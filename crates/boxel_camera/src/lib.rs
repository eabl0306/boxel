//! Camera component, camera-relative view math ([`CameraView`]) and [`FlyCameraPlugin`].

use boxel_app::{App, Plugin, Time, Update};
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, Frustum, I64Vec3, IVec3, Mat4, Quat, Vec2, Vec3};
use boxel_transform::Transform;
use boxel_window::{
    AccumulatedMouseMotion, ButtonInput, CursorGrab, CursorOptions, KeyCode, MouseButton,
};
use std::f32::consts::{PI, TAU};

/// Perspective camera. Uses `Transform` for placement. The renderer draws from the first
/// active camera (M0 supports one).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Vertical field of view, radians.
    pub fov_y_radians: f32,
    /// Near plane distance (reverse-Z, infinite far).
    pub near: f32,
    /// Culling and fog distance in world units.
    pub view_distance: f32,
    /// Only active cameras render.
    pub active: bool,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            fov_y_radians: 70f32.to_radians(),
            near: 0.1,
            view_distance: 384.0,
            active: true,
        }
    }
}

impl Camera {
    /// Reverse-Z infinite projection for `aspect` (width / height).
    pub fn projection(&self, aspect: f32) -> Mat4 {
        boxel_math::projection::perspective_infinite_reverse(self.fov_y_radians, aspect, self.near)
    }
}

/// Camera-relative view for one frame. `render_origin` is the camera position floored to whole
/// voxels (`i64`, like world block positions); every GPU position is `world - render_origin`,
/// so `f32` stays exact near the camera at any world coordinate. The only limit left is the
/// `f64` camera position itself (1 mm steps at about `4.4e12` voxels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraView {
    /// Integer origin of the camera-relative frame (saturates past `i64`, far outside the world).
    pub render_origin: I64Vec3,
    /// Eye position relative to `render_origin` (each component in [0, 1)).
    pub eye_rel: Vec3,
    /// Unit forward.
    pub forward: Vec3,
    /// View matrix (camera-relative).
    pub view: Mat4,
    /// Projection matrix (reverse-Z infinite).
    pub proj: Mat4,
    /// `proj * view`.
    pub view_proj: Mat4,
    /// Frustum in camera-relative space; far plane at `Camera::view_distance`.
    pub frustum: Frustum,
}

impl CameraView {
    /// Builds the view for `aspect` (width / height).
    pub fn compute(camera: &Camera, transform: &Transform, aspect: f32) -> Self {
        let t = transform.translation;
        // `t - floor(t)` is exact in f64, and so is the i64 round trip of an f64 integer.
        let render_origin = t.floor().as_i64vec3();
        let eye_rel = (t - render_origin.as_dvec3()).as_vec3();
        let forward = transform.forward();
        let view = boxel_math::projection::look_to(eye_rel, forward, transform.up());
        let proj = camera.projection(aspect);
        let view_proj = proj * view;
        let mut frustum = Frustum::from_view_proj_reverse_z(view_proj);
        frustum.set_max_distance(eye_rel, forward, camera.view_distance);
        Self {
            render_origin,
            eye_rel,
            forward,
            view,
            proj,
            view_proj,
            frustum,
        }
    }

    /// The render origin as the GPU gets it: its low 32 bits ([`boxel_math::wrap_to_i32`]). The
    /// shaders subtract it from chunk origins wrapped the same way, which gives the exact
    /// relative position of every chunk less than `2^31` voxels away.
    pub const fn gpu_render_origin(&self) -> IVec3 {
        boxel_math::wrap_to_i32(self.render_origin)
    }
}

/// Free-fly controller: mouse look while the cursor is grabbed, WASD move, Space/ShiftLeft
/// up/down, `ControlLeft` boost. Left click grabs the cursor, Escape releases it.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct FlyCamera {
    /// Speed in world units per second.
    pub speed: f32,
    /// Multiplier while boosting.
    pub boost: f32,
    /// Radians per mouse unit.
    pub sensitivity: f32,
    /// Current yaw (radians), owned by the controller.
    pub yaw: f32,
    /// Current pitch (radians, clamped to +-89 degrees).
    pub pitch: f32,
    /// Input enabled (automation turns it off).
    pub enabled: bool,
}

impl Default for FlyCamera {
    fn default() -> Self {
        Self {
            speed: 20.0,
            boost: 5.0,
            sensitivity: 0.002,
            yaw: 0.0,
            pitch: 0.0,
            enabled: true,
        }
    }
}

/// Systems of the fly camera.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct FlyCameraSystems;

/// Adds the fly camera systems to `Update` (in [`FlyCameraSystems`]). Requires `InputPlugin`.
///
/// Also needs `Time` (`TimePlugin`). Inserts the default `CursorOptions` (free, visible) if no
/// `WindowPlugin` did, so the controller also runs headless. The systems write `Transform`:
/// order systems of other plugins that read the camera's `Transform` in `Update` against
/// [`FlyCameraSystems`] (for example `FlyCameraSystems.before(WorldSystems::Interest)`).
#[derive(Default)]
pub struct FlyCameraPlugin;

impl Plugin for FlyCameraPlugin {
    fn build(&self, app: &mut App) {
        if !app.world().contains_resource::<CursorOptions>() {
            app.insert_resource(CursorOptions {
                grab: CursorGrab::None,
                visible: true,
            });
        }
        app.add_systems(
            Update,
            (
                fly_camera_grab_system,
                fly_camera_look_system,
                fly_camera_move_system,
            )
                .chain()
                .in_set(FlyCameraSystems),
        );
    }
}

/// Pitch limit (89 degrees): looking straight up or down would flip the view.
const MAX_PITCH: f32 = 89.0_f32.to_radians();

/// Left click grabs the cursor (locked, hidden), Escape releases it. Only while an enabled
/// fly camera exists.
fn fly_camera_grab_system(
    cameras: Query<&FlyCamera, With<Transform>>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut cursor: ResMut<CursorOptions>,
) {
    if !cameras.iter().any(|camera| camera.enabled) {
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        cursor.set_if_neq(CursorOptions {
            grab: CursorGrab::None,
            visible: true,
        });
    } else if buttons.just_pressed(MouseButton::Left) {
        cursor.set_if_neq(CursorOptions {
            grab: CursorGrab::Locked,
            visible: false,
        });
    }
}

/// Mouse look while the cursor is locked: yaw around +Y, pitch around the local +X, clamped.
fn fly_camera_look_system(
    cursor: Res<CursorOptions>,
    motion: Res<AccumulatedMouseMotion>,
    mut cameras: Query<(&mut FlyCamera, &mut Transform)>,
) {
    if cursor.grab != CursorGrab::Locked || motion.delta == Vec2::ZERO {
        return;
    }
    for (mut camera, mut transform) in &mut cameras {
        if !camera.enabled {
            continue;
        }
        let yaw = wrap_angle(camera.yaw - motion.delta.x * camera.sensitivity);
        let pitch =
            (camera.pitch - motion.delta.y * camera.sensitivity).clamp(-MAX_PITCH, MAX_PITCH);
        camera.yaw = yaw;
        camera.pitch = pitch;
        transform.rotation = Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch);
    }
}

/// WASD on the horizontal plane of the yaw, Space up, `ShiftLeft` down, `ControlLeft` boost.
fn fly_camera_move_system(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut cameras: Query<(&FlyCamera, &mut Transform)>,
) {
    let dt = time.delta().as_secs_f64();
    let forward = axis(&keys, KeyCode::KeyW, KeyCode::KeyS);
    let right = axis(&keys, KeyCode::KeyD, KeyCode::KeyA);
    let up = axis(&keys, KeyCode::Space, KeyCode::ShiftLeft);
    if dt == 0.0 || (forward == 0.0 && right == 0.0 && up == 0.0) {
        return;
    }
    let boosting = keys.pressed(KeyCode::ControlLeft);
    for (camera, mut transform) in &mut cameras {
        if !camera.enabled {
            continue;
        }
        let (sin, cos) = f64::from(camera.yaw).sin_cos();
        // Rotation of -Z and +X around +Y by the yaw.
        let forward_dir = DVec3::new(-sin, 0.0, -cos);
        let right_dir = DVec3::new(cos, 0.0, -sin);
        let dir = forward_dir * forward + right_dir * right + DVec3::Y * up;
        let mut speed = f64::from(camera.speed);
        if boosting {
            speed *= f64::from(camera.boost);
        }
        transform.translation += dir.normalize_or_zero() * speed * dt;
    }
}

/// +1 if only `positive` is held, -1 if only `negative` is, else 0.
fn axis(keys: &ButtonInput<KeyCode>, positive: KeyCode, negative: KeyCode) -> f64 {
    match (keys.pressed(positive), keys.pressed(negative)) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    }
}

/// Keeps an angle in [-PI, PI] so `f32` precision does not degrade after many turns (values
/// already in range are returned unchanged).
fn wrap_angle(angle: f32) -> f32 {
    if (-PI..=PI).contains(&angle) {
        angle
    } else {
        (angle + PI).rem_euclid(TAU) - PI
    }
}

/// Common items.
pub mod prelude {
    pub use crate::{Camera, CameraView, FlyCamera, FlyCameraPlugin};
}
