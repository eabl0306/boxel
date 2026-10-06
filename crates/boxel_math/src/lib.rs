//! Math for boxel: a `glam` re-export plus the few geometric helpers the engine shares.
//!
//! Conventions (frozen):
//! - World space is right-handed, +Y up. Cameras look down their local -Z.
//! - Clip space is wgpu's: X right, Y up, Z in `[0, 1]` (DirectX convention).
//! - Depth is reverse-Z with an infinite far plane: near maps to 1, infinity to 0.
//! - Rendering is camera-relative: GPU positions are `world - render_origin` where
//!   `render_origin` is an integer voxel position near the camera.
//! - World integer positions are `i64` on the CPU; the GPU gets their low 32 bits
//!   ([`wrap_to_i32`]) and only ever uses differences of them.

pub use glam;
pub use glam::{
    DQuat, DVec2, DVec3, I64Vec3, IVec2, IVec3, Mat3, Mat4, Quat, UVec2, UVec3, Vec2, Vec3, Vec3A,
    Vec4,
};

mod aabb;
mod frustum;
pub mod projection;

pub use aabb::Aabb;
pub use frustum::Frustum;

/// Low 32 bits of each component (two's complement wrapping): how 64-bit world integer
/// positions (chunk origins, the render origin) are uploaded to 32-bit GPU fields.
///
/// The GPU only uses differences of such values, computed in `i32`, and WGSL integer arithmetic
/// wraps: `wrap(a) - wrap(b)` is `a - b` modulo 2^32, so it is exactly `a - b` whenever the
/// true difference fits in `i32` (`|a - b| < 2^31` voxels), however large `a` and `b` are.
/// `boxel_render` only draws chunks within about `2^30` voxels of the camera (in practice a few
/// thousand); resident chunks farther away (right after a teleport) are not drawn.
pub const fn wrap_to_i32(v: I64Vec3) -> IVec3 {
    IVec3::new(v.x as i32, v.y as i32, v.z as i32)
}

/// Commonly used math items.
pub mod prelude {
    pub use crate::{Aabb, Frustum};
    pub use glam::{
        DQuat, DVec3, I64Vec3, IVec2, IVec3, Mat4, Quat, UVec2, UVec3, Vec2, Vec3, Vec4,
    };
}
