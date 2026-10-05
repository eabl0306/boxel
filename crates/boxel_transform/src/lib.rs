//! [`Transform`]: position (f64, world units = voxels) and rotation of an entity. No hierarchy in M0.
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, Quat, Vec3};

/// World placement. Translation is `f64` so large worlds keep sub-voxel precision; rendering
/// converts to camera-relative `f32` (see `boxel_camera::CameraView`).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// Position in world units (1.0 = one voxel).
    pub translation: DVec3,
    /// Orientation; local -Z is forward, +Y up, +X right.
    pub rotation: Quat,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// Origin, no rotation.
    pub const IDENTITY: Self = Self {
        translation: DVec3::ZERO,
        rotation: Quat::IDENTITY,
    };

    /// At `translation`, no rotation.
    pub const fn from_translation(translation: DVec3) -> Self {
        Self {
            translation,
            rotation: Quat::IDENTITY,
        }
    }

    /// At `(x, y, z)`, no rotation.
    pub const fn from_xyz(x: f64, y: f64, z: f64) -> Self {
        Self::from_translation(DVec3::new(x, y, z))
    }

    /// Same translation, given rotation.
    pub const fn with_rotation(mut self, rotation: Quat) -> Self {
        self.rotation = rotation;
        self
    }

    /// Rotation from yaw (around +Y) then pitch (around local +X), radians.
    pub fn from_yaw_pitch(translation: DVec3, yaw: f32, pitch: f32) -> Self {
        Self {
            translation,
            rotation: Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch),
        }
    }

    /// Unit forward vector (local -Z).
    pub fn forward(&self) -> Vec3 {
        self.rotation * Vec3::NEG_Z
    }

    /// Unit right vector (local +X).
    pub fn right(&self) -> Vec3 {
        self.rotation * Vec3::X
    }

    /// Unit up vector (local +Y).
    pub fn up(&self) -> Vec3 {
        self.rotation * Vec3::Y
    }
}

/// Common items.
pub mod prelude {
    pub use crate::Transform;
}
