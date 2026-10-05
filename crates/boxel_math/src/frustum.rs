use crate::Aabb;
use glam::{Mat4, Vec3, Vec4};

/// A view frustum as 6 normalized planes. Each plane is `(n.x, n.y, n.z, d)` with the normal
/// pointing *inside*: a point `p` is inside when `dot(n, p) + d >= 0` for all planes.
///
/// Plane order (frozen, mirrored by `cull.wgsl`): left, right, bottom, top, near, far.
/// With an infinite projection the far plane is `(0, 0, 0, 1)` (always inside) unless
/// [`Frustum::set_max_distance`] replaces it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frustum {
    /// Planes in the frozen order.
    pub planes: [Vec4; 6],
}

impl Frustum {
    /// Gribb-Hartmann extraction from a reverse-Z, infinite-far, wgpu clip-space matrix
    /// (see [`crate::projection::perspective_infinite_reverse`]). Planes are normalized.
    /// Left = r3 + r0, right = r3 - r0, bottom = r3 + r1, top = r3 - r1, near = r3 - r2,
    /// far = (0, 0, 0, 1).
    pub fn from_view_proj_reverse_z(view_proj: Mat4) -> Self {
        let r0 = view_proj.row(0);
        let r1 = view_proj.row(1);
        let r2 = view_proj.row(2);
        let r3 = view_proj.row(3);
        let normalize = |p: Vec4| {
            let len = p.truncate().length();
            if len > 0.0 { p / len } else { p }
        };
        Self {
            planes: [
                normalize(r3 + r0),
                normalize(r3 - r0),
                normalize(r3 + r1),
                normalize(r3 - r1),
                normalize(r3 - r2),
                Vec4::new(0.0, 0.0, 0.0, 1.0),
            ],
        }
    }

    /// Replaces the far plane with a plane at `distance` along `forward` from `eye`
    /// (`forward` must be normalized). Used to cull beyond the view distance.
    pub fn set_max_distance(&mut self, eye: Vec3, forward: Vec3, distance: f32) {
        self.planes[5] = (-forward).extend(forward.dot(eye) + distance);
    }

    /// Conservative box test (p-vertex). False only if the box is fully outside a plane.
    pub fn intersects_aabb(&self, aabb: &Aabb) -> bool {
        self.planes.iter().all(|p| {
            let n = p.truncate();
            let v = Vec3::select(n.cmpge(Vec3::ZERO), aabb.max, aabb.min);
            n.dot(v) + p.w >= 0.0
        })
    }

    /// Planes as plain arrays, in the frozen order (GPU upload).
    pub fn to_array(self) -> [[f32; 4]; 6] {
        self.planes.map(|p| p.to_array())
    }
}
