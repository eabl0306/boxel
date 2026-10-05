use glam::Vec3;

/// Axis-aligned bounding box in `f32` (usually camera-relative coordinates).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Aabb {
    /// Minimum corner (inclusive).
    pub min: Vec3,
    /// Maximum corner (inclusive).
    pub max: Vec3,
}

impl Aabb {
    /// Creates a box from its corners. `min <= max` component-wise is the caller's job.
    pub const fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    /// Creates a box from a center and half extents.
    pub fn from_center_half_extents(center: Vec3, half_extents: Vec3) -> Self {
        Self {
            min: center - half_extents,
            max: center + half_extents,
        }
    }

    /// Center point.
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Half extents.
    pub fn half_extents(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    /// True if `p` lies inside or on the boundary.
    pub fn contains_point(&self, p: Vec3) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }

    /// True if the boxes overlap (touching counts as overlapping).
    pub fn intersects(&self, other: &Aabb) -> bool {
        self.min.cmple(other.max).all() && other.min.cmple(self.max).all()
    }
}
