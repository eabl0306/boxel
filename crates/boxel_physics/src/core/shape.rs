//! Collider shapes, local boxes and ray tests against them.
//!
//! Every coordinate here is local: relative to a body's position or to a block, never an
//! absolute world coordinate (see [`boxel_voxel::WorldPos`]), so the numbers stay small and the
//! results do not depend on where in the world they are computed.

use boxel_math::DVec3;

/// An axis-aligned box in a local frame, `min <= max` on every axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocalBox {
    /// Min corner.
    pub min: DVec3,
    /// Max corner.
    pub max: DVec3,
}

impl LocalBox {
    /// A box from its corners.
    pub const fn new(min: DVec3, max: DVec3) -> Self {
        Self { min, max }
    }

    /// The box `center - half ..= center + half`.
    pub fn from_center_half(center: DVec3, half: DVec3) -> Self {
        Self {
            min: center - half,
            max: center + half,
        }
    }

    /// The box moved by `d`.
    pub fn translate(self, d: DVec3) -> Self {
        Self {
            min: self.min + d,
            max: self.max + d,
        }
    }

    /// Edge lengths.
    pub fn size(self) -> DVec3 {
        self.max - self.min
    }

    /// Center.
    pub fn center(self) -> DVec3 {
        (self.min + self.max) * 0.5
    }

    /// `p` is inside or on the boundary.
    pub fn contains(self, p: DVec3) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }

    /// The two boxes overlap by more than `eps` on every axis (touching is not overlapping).
    pub fn overlaps(self, other: LocalBox, eps: f64) -> bool {
        (self.min + eps).cmplt(other.max).all() && (other.min + eps).cmplt(self.max).all()
    }

    /// The first point where the ray `origin + dir * t` meets the box, for `t` in
    /// `0..=max_t` (slab test). A ray that starts inside hits at `t = 0` with a zero normal.
    /// `dir` need not be a unit vector: `t` is in multiples of it.
    pub fn ray(self, origin: DVec3, dir: DVec3, max_t: f64) -> Option<ShapeHit> {
        let mut near = f64::NEG_INFINITY;
        let mut far = f64::INFINITY;
        let mut axis = 0;
        for a in 0..3 {
            if dir[a] == 0.0 {
                // Parallel to the slab: inside it or never.
                if origin[a] < self.min[a] || origin[a] > self.max[a] {
                    return None;
                }
                continue;
            }
            let t1 = (self.min[a] - origin[a]) / dir[a];
            let t2 = (self.max[a] - origin[a]) / dir[a];
            let (enter, exit) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
            // Ties keep the first axis (x, then y, then z).
            if enter > near {
                near = enter;
                axis = a;
            }
            far = far.min(exit);
        }
        if near > far || far < 0.0 {
            return None;
        }
        if near <= 0.0 {
            return Some(ShapeHit {
                t: 0.0,
                normal: DVec3::ZERO,
            });
        }
        if near > max_t {
            return None;
        }
        let mut normal = DVec3::ZERO;
        normal[axis] = if dir[axis] > 0.0 { -1.0 } else { 1.0 };
        Some(ShapeHit { t: near, normal })
    }
}

/// Where a ray meets a shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeHit {
    /// Ray parameter of the hit (`origin + dir * t`).
    pub t: f64,
    /// Outward unit normal of the surface hit; zero when the ray started inside.
    pub normal: DVec3,
}

/// A collider shape, centered on its own origin (a collider places it with an offset from the
/// body's position: a player is `Aabb { half: (0.3, 0.9, 0.3) }` at offset `(0, 0.9, 0)`, so
/// the body's position is at its feet).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// An axis-aligned box with these half extents.
    Aabb {
        /// Half extents (each larger than the sweep's `eps`).
        half: DVec3,
    },
    /// A sphere.
    Sphere {
        /// Radius (larger than the sweep's `eps`).
        radius: f64,
    },
}

impl Shape {
    /// The box this shape uses against the voxel grid, centered on the shape's origin: the box
    /// itself, or a sphere's bounding box. Spheres collide with voxels as their bounding box in
    /// v1 (ADR 0016): exact sphere-vs-voxel contact is deferred until rolling balls need it.
    pub fn bounds(self) -> LocalBox {
        match self {
            Shape::Aabb { half } => LocalBox::from_center_half(DVec3::ZERO, half),
            Shape::Sphere { radius } => {
                LocalBox::from_center_half(DVec3::ZERO, DVec3::splat(radius))
            }
        }
    }

    /// The first point where the ray `origin + dir * t` meets this shape centered at `center`
    /// (all in one local frame), for `t` in `0..=max_t`. Exact for both shapes (only rays
    /// against voxels use the sphere's bounding box). A ray that starts inside hits at `t = 0`
    /// with a zero normal.
    pub fn ray(self, center: DVec3, origin: DVec3, dir: DVec3, max_t: f64) -> Option<ShapeHit> {
        match self {
            Shape::Aabb { half } => {
                LocalBox::from_center_half(center, half).ray(origin, dir, max_t)
            }
            Shape::Sphere { radius } => ray_sphere(center, radius, origin, dir, max_t),
        }
    }
}

/// Ray against a sphere, with the numerically stable form of the quadratic (no cancellation
/// between `-b` and the square root).
fn ray_sphere(
    center: DVec3,
    radius: f64,
    origin: DVec3,
    dir: DVec3,
    max_t: f64,
) -> Option<ShapeHit> {
    let f = origin - center;
    let c = f.dot(f) - radius * radius;
    if c <= 0.0 {
        return Some(ShapeHit {
            t: 0.0,
            normal: DVec3::ZERO,
        });
    }
    let a = dir.dot(dir);
    let b = f.dot(dir);
    if a == 0.0 || b >= 0.0 {
        // Outside and not moving, or moving away.
        return None;
    }
    let disc = b * b - a * c;
    if disc < 0.0 {
        return None;
    }
    // b < 0: q = -(b - sqrt) = sqrt - b > 0, and the roots are q / a and c / q.
    let q = disc.sqrt() - b;
    let t = (c / q).min(q / a);
    if t > max_t {
        return None;
    }
    let normal = (f + dir * t) / radius;
    Some(ShapeHit { t, normal })
}
