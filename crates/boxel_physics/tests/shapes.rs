//! Shapes: bounds against the voxel grid (a sphere uses its bounding box in v1) and ray tests
//! against boxes and spheres, checked against analytic answers.
mod common;

use boxel_math::{DVec3, I64Vec3};
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
