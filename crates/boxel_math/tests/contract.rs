//! Contract tests for skeleton-implemented items (frozen behavior).
use boxel_math::projection::{look_to, perspective_infinite_reverse};
use boxel_math::{Aabb, Frustum, Vec3};

fn camera_frustum() -> Frustum {
    // Eye at the origin looking down -Z.
    let view = look_to(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
    let proj = perspective_infinite_reverse(70f32.to_radians(), 16.0 / 9.0, 0.1);
    Frustum::from_view_proj_reverse_z(proj * view)
}

fn unit_box_at(c: Vec3) -> Aabb {
    Aabb::from_center_half_extents(c, Vec3::splat(0.5))
}

#[test]
fn frustum_keeps_front_and_rejects_behind_and_sides() {
    let f = camera_frustum();
    assert!(f.intersects_aabb(&unit_box_at(Vec3::new(0.0, 0.0, -10.0))));
    assert!(
        f.intersects_aabb(&unit_box_at(Vec3::new(0.0, 0.0, -100_000.0))),
        "infinite far"
    );
    assert!(
        !f.intersects_aabb(&unit_box_at(Vec3::new(0.0, 0.0, 10.0))),
        "behind"
    );
    assert!(
        !f.intersects_aabb(&unit_box_at(Vec3::new(-100.0, 0.0, -10.0))),
        "left"
    );
    assert!(
        !f.intersects_aabb(&unit_box_at(Vec3::new(0.0, 100.0, -10.0))),
        "above"
    );
    assert!(f.planes.iter().all(|p| p.is_finite()));
}

#[test]
fn max_distance_replaces_the_far_plane() {
    let mut f = camera_frustum();
    f.set_max_distance(Vec3::ZERO, Vec3::NEG_Z, 50.0);
    assert!(f.intersects_aabb(&unit_box_at(Vec3::new(0.0, 0.0, -49.0))));
    assert!(!f.intersects_aabb(&unit_box_at(Vec3::new(0.0, 0.0, -100.0))));
}

#[test]
fn aabb_contains_and_intersects_are_inclusive() {
    let a = Aabb::new(Vec3::ZERO, Vec3::ONE);
    assert!(a.contains_point(Vec3::ONE));
    assert!(!a.contains_point(Vec3::new(1.01, 0.5, 0.5)));
    assert!(
        a.intersects(&Aabb::new(Vec3::ONE, Vec3::splat(2.0))),
        "touching counts"
    );
    assert!(!a.intersects(&Aabb::new(Vec3::splat(1.01), Vec3::splat(2.0))));
}
