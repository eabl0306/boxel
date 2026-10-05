//! `Transform` basis vectors and constructors.
use boxel_math::{DVec3, Quat, Vec3};
use boxel_transform::Transform;
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

fn assert_vec(actual: Vec3, expected: Vec3) {
    assert!(
        actual.abs_diff_eq(expected, 1e-6),
        "{actual:?} != {expected:?}"
    );
}

#[test]
fn identity_basis() {
    let t = Transform::IDENTITY;
    assert_vec(t.forward(), Vec3::NEG_Z);
    assert_vec(t.right(), Vec3::X);
    assert_vec(t.up(), Vec3::Y);
    assert_eq!(Transform::default(), Transform::IDENTITY);
}

#[test]
fn yaw_quarter_turn_looks_down_neg_x() {
    // Yaw is a counter-clockwise turn around +Y seen from above: -Z turns to -X.
    let t = Transform::from_yaw_pitch(DVec3::ZERO, FRAC_PI_2, 0.0);
    assert_vec(t.forward(), Vec3::NEG_X);
    assert_vec(t.right(), Vec3::NEG_Z);
    assert_vec(t.up(), Vec3::Y);
}

#[test]
fn pitch_up_looks_up() {
    let t = Transform::from_yaw_pitch(DVec3::ZERO, 0.0, FRAC_PI_2);
    assert_vec(t.forward(), Vec3::Y);
    assert_vec(t.right(), Vec3::X);
    assert_vec(t.up(), Vec3::Z);
}

#[test]
fn pitch_is_applied_in_the_yawed_frame() {
    let t = Transform::from_yaw_pitch(DVec3::ZERO, FRAC_PI_2, FRAC_PI_4);
    let h = FRAC_PI_4.cos();
    assert_vec(t.forward(), Vec3::new(-h, h, 0.0));
    // Pitch never rolls: right stays horizontal.
    assert_vec(t.right(), Vec3::NEG_Z);
}

#[test]
fn basis_is_orthonormal_and_right_handed() {
    for (yaw, pitch) in [(0.0, 0.0), (0.3, -0.7), (PI, 1.2), (-2.0, 0.5), (5.0, -1.5)] {
        let t = Transform::from_yaw_pitch(DVec3::new(1.0, 2.0, 3.0), yaw, pitch);
        let (f, r, u) = (t.forward(), t.right(), t.up());
        for v in [f, r, u] {
            assert!((v.length() - 1.0).abs() < 1e-5, "{v:?}");
        }
        assert!(f.dot(r).abs() < 1e-5 && f.dot(u).abs() < 1e-5 && r.dot(u).abs() < 1e-5);
        // +X right, +Y up, -Z forward: right x up = backward.
        assert_vec(r.cross(u), -f);
        assert!(r.y.abs() < 1e-6, "no roll: {r:?}");
    }
}

#[test]
fn constructors_keep_translation_and_rotation() {
    let t = Transform::from_xyz(1.5, -2.0, 1.0e9);
    assert_eq!(t.translation, DVec3::new(1.5, -2.0, 1.0e9));
    assert_eq!(t.rotation, Quat::IDENTITY);
    assert_eq!(Transform::from_translation(t.translation), t);
    let r = Quat::from_rotation_z(0.25);
    let rotated = t.with_rotation(r);
    assert_eq!(rotated.translation, t.translation);
    assert_eq!(rotated.rotation, r);
    let yp = Transform::from_yaw_pitch(DVec3::new(4.0, 5.0, 6.0), 0.1, 0.2);
    assert_eq!(yp.translation, DVec3::new(4.0, 5.0, 6.0));
    // Large worlds keep sub-voxel precision in f64.
    let far = Transform::from_xyz(16_777_217.25, 0.0, 0.0);
    assert_eq!(far.translation.x.fract(), 0.25);
}

#[test]
fn transform_is_a_component() {
    let mut world = boxel_ecs::world::World::new();
    let e = world.spawn(Transform::from_xyz(1.0, 2.0, 3.0)).id();
    assert_eq!(
        world.get::<Transform>(e).map(|t| t.translation),
        Some(DVec3::new(1.0, 2.0, 3.0))
    );
}
