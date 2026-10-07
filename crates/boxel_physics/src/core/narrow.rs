//! The narrowphase: whether two shapes overlap, and the shortest way to pull them apart (the
//! minimum translation vector, ADR 0016 phase P3).
//!
//! Every function works in one local frame (usually the frame of the first body's block, with
//! the second body placed by `WorldPos::delta_to`), so the numbers stay small and a pair gives
//! bit-identical results anywhere in the world. Only `+ - * /`, `sqrt`, `min` and `max` are used.
//!
//! The normal of a [`Contact`] points from the first shape toward the second: moving the second
//! shape by `normal * depth` (or the first by `-normal * depth`, or any split of the two) leaves
//! them touching.
//!
//! | Pair | Separation |
//! |---|---|
//! | box / box | along the axis of least penetration (ties Y, X, Z) |
//! | sphere / sphere | along the line between the centers (`+Y` when they coincide) |
//! | sphere / box | from the box's closest point to the center (least penetration face when the center is inside the box) |

use super::shape::{LocalBox, Shape};
use boxel_math::DVec3;
use std::cmp::Ordering;

/// Two shapes overlap: how to pull them apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    /// Unit vector from the first shape toward the second: the direction that separates the
    /// second shape from the first.
    pub normal: DVec3,
    /// How far they overlap along `normal` (always positive).
    pub depth: f64,
}

impl Contact {
    /// The same contact seen from the other shape (the normal reversed).
    pub fn flip(self) -> Self {
        Self {
            normal: -self.normal,
            depth: self.depth,
        }
    }
}

/// The axes in the order that wins ties: Y, then X, then Z.
const TIE_ORDER: [usize; 3] = [1, 0, 2];

/// The unit vector along `axis`, positive or negative.
fn axis_normal(axis: usize, positive: bool) -> DVec3 {
    let mut n = DVec3::ZERO;
    n[axis] = if positive { 1.0 } else { -1.0 };
    n
}

/// Box against box. On each axis the second box can leave through its positive side
/// (`a.max - b.min`) or its negative side (`b.max - a.min`); the shorter one is that axis'
/// penetration (positive when the centers coincide on that axis). The contact is along the axis
/// with the least penetration (ties Y, X, Z). `None` when they do not overlap by a positive
/// amount on every axis (touching is not overlapping).
pub fn box_box(a: LocalBox, b: LocalBox) -> Option<Contact> {
    let mut best: Option<(f64, usize, bool)> = None;
    for axis in TIE_ORDER {
        let plus = a.max[axis] - b.min[axis];
        let minus = b.max[axis] - a.min[axis];
        // `!(x > 0)` also rejects NaN.
        if !(plus > 0.0 && minus > 0.0) {
            return None;
        }
        let (depth, positive) = if minus < plus {
            (minus, false)
        } else {
            (plus, true)
        };
        if best.is_none_or(|(d, _, _)| depth < d) {
            best = Some((depth, axis, positive));
        }
    }
    let (depth, axis, positive) = best?;
    Some(Contact {
        normal: axis_normal(axis, positive),
        depth,
    })
}

/// Box against box when `b` moved by `motion` relative to `a` during the step that made them
/// overlap: if `b` was apart from `a` on some axes before that motion (`b - motion` at least
/// `eps` clear of `a`, or touching), they are pushed back apart on the one of those axes with
/// the least penetration (ties Y, X, Z), toward the side `b` came from. So a body that sank
/// more than half its height into another in one step (a crate falling fast onto a crate) is
/// pushed back up, never through. Without such an axis (they overlapped already), the same as
/// [`box_box`]. `None` unless the boxes overlap by more than `eps` on every axis (a body that
/// only grazes another's side is not lifted onto it).
pub fn box_box_moving(a: LocalBox, b: LocalBox, motion: DVec3, eps: f64) -> Option<Contact> {
    let current = box_box(a, b).filter(|c| c.depth > eps)?;
    let before = b.translate(-motion);
    let mut best: Option<(f64, usize, bool)> = None;
    for axis in TIE_ORDER {
        let positive = if before.min[axis] >= a.max[axis] - eps {
            true
        } else if before.max[axis] <= a.min[axis] + eps {
            false
        } else {
            continue;
        };
        // Both are positive: the boxes overlap on every axis now.
        let depth = if positive {
            a.max[axis] - b.min[axis]
        } else {
            b.max[axis] - a.min[axis]
        };
        if best.is_none_or(|(d, _, _)| depth < d) {
            best = Some((depth, axis, positive));
        }
    }
    Some(match best {
        Some((depth, axis, positive)) => Contact {
            normal: axis_normal(axis, positive),
            depth,
        },
        None => current,
    })
}

/// Sphere against sphere: along the line from `a` to `b` (`+Y` when the centers coincide).
/// `None` when they do not overlap (touching is not overlapping).
pub fn sphere_sphere(a: DVec3, ra: f64, b: DVec3, rb: f64) -> Option<Contact> {
    let delta = b - a;
    let reach = ra + rb;
    let dist_sq = delta.dot(delta);
    if !(reach > 0.0 && dist_sq < reach * reach) {
        return None;
    }
    let dist = dist_sq.sqrt();
    if dist == 0.0 {
        return Some(Contact {
            normal: DVec3::Y,
            depth: reach,
        });
    }
    Some(Contact {
        normal: delta / dist,
        depth: reach - dist,
    })
}

/// Sphere (center `c`, radius `r`) against a box; the normal points from the sphere toward the
/// box. With the center outside the box, the contact runs from the center to the box's closest
/// point. With the center inside, the box leaves through the face nearest to the center (ties
/// Y, X, Z; min face before max face). `None` when they do not overlap.
pub fn sphere_box(c: DVec3, r: f64, b: LocalBox) -> Option<Contact> {
    let closest = c.clamp(b.min, b.max);
    let delta = closest - c;
    let dist_sq = delta.dot(delta);
    if dist_sq > 0.0 {
        // Not closer than the radius (NaN included): apart or touching.
        if dist_sq.partial_cmp(&(r * r)) != Some(Ordering::Less) {
            return None;
        }
        let dist = dist_sq.sqrt();
        return Some(Contact {
            normal: delta / dist,
            depth: r - dist,
        });
    }
    // The center is inside (or on the surface): the box moves away along the axis where the
    // center is nearest to a face. Nearest to the min face: the box moves + (its min face goes
    // past the sphere).
    let mut best: Option<(f64, usize, bool)> = None;
    for axis in TIE_ORDER {
        let to_min = c[axis] - b.min[axis];
        let to_max = b.max[axis] - c[axis];
        let (gap, positive) = if to_max < to_min {
            (to_max, false)
        } else {
            (to_min, true)
        };
        if best.is_none_or(|(g, _, _)| gap < g) {
            best = Some((gap, axis, positive));
        }
    }
    let (gap, axis, positive) = best?;
    let depth = gap + r;
    // A point on the surface only touches; NaN never overlaps.
    if depth.partial_cmp(&0.0) != Some(Ordering::Greater) {
        return None;
    }
    Some(Contact {
        normal: axis_normal(axis, positive),
        depth,
    })
}

/// Box against sphere; the normal points from the box toward the sphere.
pub fn box_sphere(a: LocalBox, c: DVec3, r: f64) -> Option<Contact> {
    sphere_box(c, r, a).map(Contact::flip)
}

/// Shape `a` centered at `ca` against shape `b` centered at `cb` (one local frame); the normal
/// points from `a` toward `b`.
pub fn shape_contact(a: Shape, ca: DVec3, b: Shape, cb: DVec3) -> Option<Contact> {
    match (a, b) {
        (Shape::Aabb { half: ha }, Shape::Aabb { half: hb }) => box_box(
            LocalBox::from_center_half(ca, ha),
            LocalBox::from_center_half(cb, hb),
        ),
        (Shape::Sphere { radius: ra }, Shape::Sphere { radius: rb }) => {
            sphere_sphere(ca, ra, cb, rb)
        }
        (Shape::Sphere { radius }, Shape::Aabb { half }) => {
            sphere_box(ca, radius, LocalBox::from_center_half(cb, half))
        }
        (Shape::Aabb { half }, Shape::Sphere { radius }) => {
            box_sphere(LocalBox::from_center_half(ca, half), cb, radius)
        }
    }
}

/// Shape `shape` centered at `center` against the box `bx` (one local frame); the normal points
/// from the shape toward the box.
pub fn shape_box(shape: Shape, center: DVec3, bx: LocalBox) -> Option<Contact> {
    match shape {
        Shape::Aabb { half } => box_box(LocalBox::from_center_half(center, half), bx),
        Shape::Sphere { radius } => sphere_box(center, radius, bx),
    }
}
