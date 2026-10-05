//! Camera component, camera-relative view math ([`CameraView`]) and [`FlyCameraPlugin`].
// SKELETON: remove this attribute when the crate is fully implemented (owner: WP2).
#![allow(
    unused_variables,
    unused_mut,
    dead_code,
    clippy::todo,
    reason = "skeleton stubs"
)]

use boxel_app::{App, Plugin};
use boxel_ecs::prelude::*;
use boxel_math::{Frustum, IVec3, Mat4, Vec3};
use boxel_transform::Transform;

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
/// voxels; every GPU position is `world - render_origin`, so `f32` stays exact near the camera
/// at any world coordinate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraView {
    /// Integer origin of the camera-relative frame.
    pub render_origin: IVec3,
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
        let render_origin = t.floor().as_ivec3();
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
#[derive(Default)]
pub struct FlyCameraPlugin;

impl Plugin for FlyCameraPlugin {
    fn build(&self, app: &mut App) {
        todo!("WP2")
    }
}

/// Common items.
pub mod prelude {
    pub use crate::{Camera, CameraView, FlyCamera, FlyCameraPlugin};
}
