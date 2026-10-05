//! Projection and view matrices (wgpu clip space, reverse-Z). Thin wrappers over `glam::camera`
//! (glam 0.34 removed the `Mat4::perspective_*`/`look_at_*` methods).
use glam::{Mat4, Vec3};

/// Reverse-Z infinite perspective, right-handed view space, wgpu clip space (Y up, Z in [0, 1]):
/// near maps to depth 1, infinity to depth 0. Use with `CompareFunction::Greater` and clear depth 0.
pub fn perspective_infinite_reverse(fov_y_radians: f32, aspect: f32, near: f32) -> Mat4 {
    glam::camera::rh::proj::directx::perspective_infinite_reverse(fov_y_radians, aspect, near)
}

/// Right-handed view matrix looking from `eye` along `dir` (`dir` need not be normalized).
pub fn look_to(eye: Vec3, dir: Vec3, up: Vec3) -> Mat4 {
    // glam expects a unit `dir` (a longer one scales the view's Y and Z axes).
    glam::camera::rh::view::look_to_mat4(eye, dir.normalize(), up)
}
