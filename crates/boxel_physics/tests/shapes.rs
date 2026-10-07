//! Shapes: bounds against the voxel grid (a sphere uses its bounding box in v1), ray tests
//! against boxes and spheres, and the narrowphase (overlap and minimum translation vector of
//! box/box, sphere/sphere, sphere/box, and box/box with the motion of the step), checked
//! against analytic answers and by proptests (moving by the vector separates exactly).
mod common;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::core::{
    Contact, box_box, box_box_moving, box_sphere, shape_contact, sphere_box, sphere_sphere,
};
use boxel_physics::{LocalBox, Shape, ShapeHit, SweepConfig, TestWorld, move_box};
use boxel_voxel::{BlockPos, WorldPos};
use common::{STONE, table};
use proptest::prelude::*;

#[test]
fn bounds_are_centered_boxes() {
    let aabb = Shape::Aabb {
        half: DVec3::new(0.3, 0.9, 0.3),
    };
    assert_eq!(
        aabb.bounds(),
        LocalBox::new(DVec3::new(-0.3, -0.9, -0.3), DVec3::new(0.3, 0.9, 0.3))
    );
    let ball = Shape::Sphere { radius: 0.25 };
    assert_eq!(
        ball.bounds(),
        LocalBox::new(DVec3::splat(-0.25), DVec3::splat(0.25))
    );
    let b = LocalBox::new(DVec3::new(-1.0, 0.0, 2.0), DVec3::new(1.0, 4.0, 3.0));
    assert_eq!(b.size(), DVec3::new(2.0, 4.0, 1.0));
    assert_eq!(b.center(), DVec3::new(0.0, 2.0, 2.5));
    assert!(b.contains(DVec3::new(1.0, 4.0, 3.0)) && !b.contains(DVec3::new(1.5, 1.0, 2.5)));
    assert_eq!(b.translate(DVec3::ONE).min, DVec3::new(0.0, 1.0, 3.0));
    // Touching is not overlapping.
    let right = LocalBox::new(DVec3::new(1.0, 0.0, 2.0), DVec3::new(2.0, 1.0, 3.0));
    assert!(!b.overlaps(right, 1e-7));
    assert!(b.overlaps(right.translate(DVec3::new(-0.01, 0.0, 0.0)), 1e-7));
}

#[test]
fn a_sphere_collides_with_voxels_as_its_bounding_box() {
    // v1 rule (ADR 0016): a sphere resting on a block edge stays up on its bounding box.
    let mut world = TestWorld::new();
    world.set(BlockPos::new(0, -1, 0), STONE);
    let ball = Shape::Sphere { radius: 0.25 };
    // The sphere's origin is its center: offset its box up by the radius so the origin is the
    // bottom, as a collider offset does.
    let shape = ball.bounds().translate(DVec3::new(0.0, 0.25, 0.0));
    // Center over air 0.125 past the block's edge: the bounding box still lands on it.
    let start = WorldPos::from_dvec3(DVec3::new(1.125, 3.0, 0.5));
    let out = move_box(
        &mut world,
        &table(),
        start,
        shape,
        DVec3::new(0.0, -5.0, 0.0),
        &[],
        &SweepConfig::default(),
    )
    .expect("loaded");
    assert_eq!(out.end.to_dvec3(), DVec3::new(1.125, 0.0, 0.5));
    assert!(out.on_ground());
    assert_eq!(out.ground, Some(BlockPos::new(0, -1, 0)));
}

#[test]
fn rays_against_boxes() {
    let b = LocalBox::new(DVec3::new(1.0, -1.0, -1.0), DVec3::new(3.0, 1.0, 1.0));
    // Straight on: enters the -X face at t = 1.
    assert_eq!(
        b.ray(DVec3::ZERO, DVec3::X, 10.0),
        Some(ShapeHit {
            t: 1.0,
            normal: DVec3::NEG_X
        })
    );
    // From above, with a non-unit direction (t in multiples of dir).
    assert_eq!(
        b.ray(DVec3::new(2.0, 5.0, 0.0), DVec3::new(0.0, -2.0, 0.0), 10.0),
        Some(ShapeHit {
            t: 2.0,
            normal: DVec3::Y
        })
    );
    // Misses: parallel outside a slab, pointing away, beyond max_t.
    assert_eq!(b.ray(DVec3::new(0.0, 2.0, 0.0), DVec3::X, 10.0), None);
    assert_eq!(b.ray(DVec3::ZERO, DVec3::NEG_X, 10.0), None);
    assert_eq!(b.ray(DVec3::ZERO, DVec3::X, 0.5), None);
    // Starting inside: t = 0, no normal.
    assert_eq!(
        b.ray(DVec3::new(2.0, 0.0, 0.0), DVec3::Y, 10.0),
        Some(ShapeHit {
            t: 0.0,
            normal: DVec3::ZERO
        })
    );
    // Through the edge at x = 1, y = 1: the x slab is entered at the same t as the y slab; the
    // first axis (x) wins.
    let hit = b
        .ray(DVec3::new(0.0, 2.0, 0.0), DVec3::new(1.0, -1.0, 0.0), 10.0)
        .expect("edge");
    assert_eq!((hit.t, hit.normal), (1.0, DVec3::NEG_X));
}

#[test]
fn rays_against_spheres() {
    let ball = Shape::Sphere { radius: 2.0 };
    let c = DVec3::new(5.0, 0.0, 0.0);
    assert_eq!(
        ball.ray(c, DVec3::ZERO, DVec3::X, 10.0),
        Some(ShapeHit {
            t: 3.0,
            normal: DVec3::NEG_X
        })
    );
    // Tangent at y = 2 (the discriminant is 0): one touching point.
    let tangent = ball
        .ray(c, DVec3::new(0.0, 2.0, 0.0), DVec3::X, 10.0)
        .expect("tangent");
    assert_eq!((tangent.t, tangent.normal), (5.0, DVec3::Y));
    // Misses: above the sphere, pointing away, beyond max_t, zero direction.
    assert_eq!(ball.ray(c, DVec3::new(0.0, 2.5, 0.0), DVec3::X, 10.0), None);
    assert_eq!(ball.ray(c, DVec3::ZERO, DVec3::NEG_X, 10.0), None);
    assert_eq!(ball.ray(c, DVec3::ZERO, DVec3::X, 2.9), None);
    assert_eq!(ball.ray(c, DVec3::ZERO, DVec3::ZERO, 10.0), None);
    // Inside.
    assert_eq!(
        ball.ray(c, DVec3::new(4.0, 1.0, 0.0), DVec3::X, 10.0),
        Some(ShapeHit {
            t: 0.0,
            normal: DVec3::ZERO
        })
    );
    // A box shape goes through the box test.
    let crate_shape = Shape::Aabb { half: DVec3::ONE };
    assert_eq!(
        crate_shape.ray(c, DVec3::ZERO, DVec3::X, 10.0).map(|h| h.t),
        Some(4.0)
    );
}

proptest! {
    /// Random rays toward random spheres: the hit point is on the sphere, its normal is the
    /// unit radius there, and no earlier point of the ray is inside.
    #[test]
    fn sphere_hits_lie_on_the_surface(
        center in (-20.0..20.0f64, -20.0..20.0f64, -20.0..20.0f64),
        radius in 0.1..5.0f64,
        target in (-1.0..1.0f64, -1.0..1.0f64, -1.0..1.0f64),
        origin in (-30.0..30.0f64, -30.0..30.0f64, -30.0..30.0f64),
    ) {
        let center = DVec3::new(center.0, center.1, center.2);
        let origin = DVec3::new(origin.0, origin.1, origin.2);
        prop_assume!((origin - center).length() > radius + 1e-6);
        // Aim at a point inside the sphere: it must hit.
        let aim = center + DVec3::new(target.0, target.1, target.2) * (radius * 0.5);
        let dir = aim - origin;
        let hit = Shape::Sphere { radius }.ray(center, origin, dir, 1.0).expect("aimed inside");
        let point = origin + dir * hit.t;
        prop_assert!(((point - center).length() - radius).abs() < 1e-9 * (1.0 + radius));
        prop_assert!((hit.normal.length() - 1.0).abs() < 1e-9);
        prop_assert!(hit.normal.dot(dir) < 0.0, "the ray enters against the normal");
        let earlier = origin + dir * (hit.t * (1.0 - 1e-6));
        prop_assert!((earlier - center).length() >= radius - 1e-9);
    }

    /// Boxes: the hit point is on the box surface, on the face the normal names.
    #[test]
    fn box_hits_lie_on_the_named_face(
        min in (-10.0..10.0f64, -10.0..10.0f64, -10.0..10.0f64),
        size in (0.1..5.0f64, 0.1..5.0f64, 0.1..5.0f64),
        target in (0.0..1.0f64, 0.0..1.0f64, 0.0..1.0f64),
        origin in (-30.0..30.0f64, -30.0..30.0f64, -30.0..30.0f64),
    ) {
        let min = DVec3::new(min.0, min.1, min.2);
        let b = LocalBox::new(min, min + DVec3::new(size.0, size.1, size.2));
        let origin = DVec3::new(origin.0, origin.1, origin.2);
        prop_assume!(!b.contains(origin));
        let aim = b.min + b.size() * DVec3::new(target.0, target.1, target.2);
        let dir = aim - origin;
        let hit = b.ray(origin, dir, 1.0).expect("aimed inside");
        let point = origin + dir * hit.t;
        let grown = LocalBox::new(b.min - 1e-9, b.max + 1e-9);
        prop_assert!(grown.contains(point), "{:?} not on {:?}", point, b);
        let axis = hit.normal.abs().max_position();
        let face = if hit.normal[axis] > 0.0 { b.max[axis] } else { b.min[axis] };
        prop_assert!((point[axis] - face).abs() < 1e-9);
        prop_assert!(hit.normal.dot(dir) < 0.0);
    }
}

#[test]
fn the_shape_frame_is_local() {
    // The same ray far away, expressed relative to the shape, gives the same answer: shapes
    // never see absolute coordinates.
    let near =
        Shape::Sphere { radius: 1.5 }.ray(DVec3::new(3.0, 0.0, 0.0), DVec3::ZERO, DVec3::X, 10.0);
    let far_origin =
        WorldPos::from_block(BlockPos::from_i64vec3(I64Vec3::splat(1 << 52)), DVec3::ZERO);
    let far_center = far_origin.offset(DVec3::new(3.0, 0.0, 0.0));
    let rel = far_origin.delta_to(&far_center);
    let far = Shape::Sphere { radius: 1.5 }.ray(rel, DVec3::ZERO, DVec3::X, 10.0);
    assert_eq!(near, far);
}

fn cube(min: [f64; 3], size: f64) -> LocalBox {
    let min = DVec3::from_array(min);
    LocalBox::new(min, min + size)
}

fn contact(normal: DVec3, depth: f64) -> Option<Contact> {
    Some(Contact { normal, depth })
}

#[test]
fn box_box_separates_along_the_least_penetration() {
    let a = cube([0.0, 0.0, 0.0], 1.0);
    // 0.25 deep on +X, much deeper on Y and Z.
    let b = LocalBox::new(DVec3::new(0.75, 0.1, 0.2), DVec3::new(1.75, 0.9, 0.8));
    assert_eq!(box_box(a, b), contact(DVec3::X, 0.25));
    assert_eq!(box_box(b, a), contact(DVec3::NEG_X, 0.25));
    // Touching is not overlapping; neither is a NaN box.
    assert_eq!(box_box(a, cube([1.0, 0.0, 0.0], 1.0)), None);
    assert_eq!(box_box(a, cube([0.5, 1.0, 0.5], 1.0)), None);
    assert_eq!(box_box(a, cube([f64::NAN, 0.0, 0.0], 1.0)), None);
    // Identical boxes: every axis ties, Y wins and the second box goes up.
    assert_eq!(box_box(a, a), contact(DVec3::Y, 1.0));
    // X and Z tie (0.25 each): X wins.
    let corner = cube([0.75, 0.0, 0.75], 1.0);
    assert_eq!(box_box(a, corner), contact(DVec3::X, 0.25));
    // A small box inside a big one near its min corner leaves through the nearest side.
    let big = cube([0.0, 0.0, 0.0], 4.0);
    let small = LocalBox::new(DVec3::new(0.5, 1.0, 1.5), DVec3::new(1.0, 2.0, 2.0));
    assert_eq!(box_box(big, small), contact(DVec3::NEG_X, 1.0));
}

#[test]
fn box_box_moving_pushes_back_toward_the_side_it_came_from() {
    let a = cube([0.0, 0.0, 0.0], 1.0);
    // Sunk 1.25 into `a` from above in one step: plain least penetration says down (0.75),
    // the motion says up (1.25).
    let b = cube([0.0, -0.25, 0.0], 1.0);
    assert_eq!(box_box(a, b), contact(DVec3::NEG_Y, 0.75));
    let fell = DVec3::new(0.0, -1.5, 0.0);
    assert_eq!(box_box_moving(a, b, fell, 1e-7), contact(DVec3::Y, 1.25));
    // Resting on `a` (touching before the step) counts as apart.
    let rested = cube([0.0, 0.875, 0.0], 1.0);
    assert_eq!(
        box_box_moving(a, rested, DVec3::new(0.0, -0.125, 0.0), 1e-7),
        contact(DVec3::Y, 0.125)
    );
    // Coming diagonally over a corner: apart on X and Y before, the shorter way out wins.
    let diagonal = cube([0.75, 0.875, 0.0], 1.0);
    assert_eq!(
        box_box_moving(a, diagonal, DVec3::new(-0.25, -0.25, 0.0), 1e-7),
        contact(DVec3::Y, 0.125)
    );
    // Overlapping before the step too: the same as `box_box`.
    let inside = cube([0.5, 0.25, 0.0], 1.0);
    assert_eq!(
        box_box_moving(a, inside, DVec3::new(0.1, 0.0, 0.0), 1e-7),
        box_box(a, inside)
    );
    // Apart now, or grazing a side by less than `eps` while falling past it: nothing.
    assert_eq!(
        box_box_moving(a, cube([2.0, 0.0, 0.0], 1.0), fell, 1e-7),
        None
    );
    let grazing = cube([1.0 - 1e-9, 0.25, 0.0], 1.0);
    assert!(box_box(a, grazing).is_some());
    assert_eq!(box_box_moving(a, grazing, fell, 1e-7), None);
}

#[test]
fn spheres_separate_along_the_line_between_centers() {
    assert_eq!(
        sphere_sphere(DVec3::ZERO, 1.0, DVec3::new(1.5, 0.0, 0.0), 1.0),
        contact(DVec3::X, 0.5)
    );
    // Coincident centers: up.
    assert_eq!(
        sphere_sphere(DVec3::ONE, 0.5, DVec3::ONE, 0.25),
        contact(DVec3::Y, 0.75)
    );
    // Touching, apart, NaN or negative radii: nothing.
    assert_eq!(
        sphere_sphere(DVec3::ZERO, 1.0, DVec3::new(0.0, 2.0, 0.0), 1.0),
        None
    );
    assert_eq!(
        sphere_sphere(DVec3::ZERO, 1.0, DVec3::splat(3.0), 1.0),
        None
    );
    assert_eq!(sphere_sphere(DVec3::ZERO, f64::NAN, DVec3::X, 1.0), None);
    assert_eq!(sphere_sphere(DVec3::ZERO, -1.0, DVec3::ZERO, -1.0), None);
}

#[test]
fn sphere_box_uses_the_closest_point_or_the_nearest_face() {
    let b = cube([0.0, 0.0, 0.0], 2.0);
    // Center outside, facing the -X face: from the sphere toward the box, +X.
    assert_eq!(
        sphere_box(DVec3::new(-0.5, 1.0, 1.0), 1.0, b),
        contact(DVec3::X, 0.5)
    );
    // Over an edge: along the diagonal to the edge.
    let c = sphere_box(DVec3::new(-0.5, 2.5, 1.0), 1.0, b).expect("edge");
    let diag = DVec3::new(1.0, -1.0, 0.0).normalize();
    assert!((c.normal - diag).length() < 1e-15, "{c:?}");
    assert!((c.depth - (1.0 - 0.5f64.sqrt())).abs() < 1e-15);
    // Center inside, nearest to the max Z face: the box leaves toward -Z.
    assert_eq!(
        sphere_box(DVec3::new(1.0, 1.0, 1.75), 0.5, b),
        contact(DVec3::NEG_Z, 0.75)
    );
    // A point (radius 0) on the surface only touches; inside it overlaps.
    assert_eq!(sphere_box(DVec3::new(2.0, 1.0, 1.0), 0.0, b), None);
    assert_eq!(
        sphere_box(DVec3::new(1.0, 1.5, 1.0), 0.0, b),
        contact(DVec3::NEG_Y, 0.5)
    );
    // Touching from outside: nothing. Box against sphere is the same, reversed.
    assert_eq!(sphere_box(DVec3::new(-1.0, 1.0, 1.0), 1.0, b), None);
    assert_eq!(
        box_sphere(b, DVec3::new(-0.5, 1.0, 1.0), 1.0),
        contact(DVec3::NEG_X, 0.5)
    );
    // `shape_contact` dispatches on both shapes.
    let ball = Shape::Sphere { radius: 1.0 };
    let crate_shape = Shape::Aabb {
        half: DVec3::splat(1.0),
    };
    assert_eq!(
        shape_contact(ball, DVec3::new(-0.5, 1.0, 1.0), crate_shape, DVec3::ONE),
        contact(DVec3::X, 0.5)
    );
    assert_eq!(
        shape_contact(crate_shape, DVec3::ONE, ball, DVec3::new(-0.5, 1.0, 1.0)),
        contact(DVec3::NEG_X, 0.5)
    );
}

/// A random box: min corner and size.
fn arb_box() -> impl Strategy<Value = LocalBox> {
    (
        (-3.0..3.0f64, -3.0..3.0f64, -3.0..3.0f64),
        (0.05..3.0f64, 0.05..3.0f64, 0.05..3.0f64),
    )
        .prop_map(|(m, s)| {
            let min = DVec3::new(m.0, m.1, m.2);
            LocalBox::new(min, min + DVec3::new(s.0, s.1, s.2))
        })
}

proptest! {
    /// Moving the second box by the vector leaves the boxes touching (no overlap), and a
    /// slightly shorter move leaves them overlapping: the depth is exact along that axis, and no
    /// other axis has a shorter way out.
    #[test]
    fn box_box_vectors_separate_exactly(a in arb_box(), b in arb_box()) {
        let Some(c) = box_box(a, b) else {
            prop_assert!(!a.overlaps(b, 0.0));
            return Ok(());
        };
        prop_assert!(a.overlaps(b, 0.0));
        prop_assert!(c.depth > 0.0);
        prop_assert!((c.normal.length() - 1.0).abs() == 0.0);
        let moved = b.translate(c.normal * c.depth);
        prop_assert!(box_box(a, moved).is_none_or(|left| left.depth < 1e-12), "{:?}", box_box(a, moved));
        let short = b.translate(c.normal * (c.depth * (1.0 - 1e-6)));
        prop_assert!(box_box(a, short).is_some());
        for axis in 0..3 {
            let way_out = (a.max[axis] - b.min[axis]).min(b.max[axis] - a.min[axis]);
            prop_assert!(c.depth <= way_out);
        }
    }

    /// Spheres: moving the second one by the vector leaves them touching.
    #[test]
    fn sphere_sphere_vectors_separate_exactly(
        a in (-2.0..2.0f64, -2.0..2.0f64, -2.0..2.0f64),
        b in (-2.0..2.0f64, -2.0..2.0f64, -2.0..2.0f64),
        ra in 0.05..2.0f64,
        rb in 0.05..2.0f64,
    ) {
        let (a, b) = (DVec3::new(a.0, a.1, a.2), DVec3::new(b.0, b.1, b.2));
        let Some(c) = sphere_sphere(a, ra, b, rb) else {
            prop_assert!((b - a).length() >= ra + rb - 1e-12);
            return Ok(());
        };
        let moved = b + c.normal * c.depth;
        prop_assert!(((moved - a).length() - (ra + rb)).abs() < 1e-12 * (1.0 + ra + rb));
    }

    /// Sphere and box: moving the box by the vector leaves them at most touching.
    #[test]
    fn sphere_box_vectors_separate(
        center in (-4.0..4.0f64, -4.0..4.0f64, -4.0..4.0f64),
        r in 0.05..2.0f64,
        b in arb_box(),
    ) {
        let center = DVec3::new(center.0, center.1, center.2);
        let Some(c) = sphere_box(center, r, b) else {
            let closest = center.clamp(b.min, b.max);
            prop_assert!((closest - center).length() >= r - 1e-12);
            return Ok(());
        };
        prop_assert!((c.normal.length() - 1.0).abs() < 1e-12);
        let moved = b.translate(c.normal * c.depth);
        prop_assert!(
            sphere_box(center, r, moved).is_none_or(|left| left.depth < 1e-9),
            "{:?}",
            sphere_box(center, r, moved)
        );
    }
}
