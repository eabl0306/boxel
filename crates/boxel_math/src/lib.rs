//! Math for boxel: a `glam` re-export plus the few geometric helpers the engine shares.
//!
//! Conventions (frozen):
//! - World space is right-handed, +Y up. Cameras look down their local -Z.
//! - Clip space is wgpu's: X right, Y up, Z in `[0, 1]` (DirectX convention).
//! - Depth is reverse-Z with an infinite far plane: near maps to 1, infinity to 0.
//! - Rendering is camera-relative: GPU positions are `world - render_origin` where
//!   `render_origin` is an integer voxel position near the camera.

pub use glam;
pub use glam::{
    DQuat, DVec2, DVec3, IVec2, IVec3, Mat3, Mat4, Quat, UVec2, UVec3, Vec2, Vec3, Vec3A, Vec4,
};

mod aabb;
mod frustum;
pub mod projection;

pub use aabb::Aabb;
pub use frustum::Frustum;

/// Commonly used math items.
pub mod prelude {
    pub use crate::{Aabb, Frustum};
    pub use glam::{DQuat, DVec3, IVec2, IVec3, Mat4, Quat, UVec2, UVec3, Vec2, Vec3, Vec4};
}
