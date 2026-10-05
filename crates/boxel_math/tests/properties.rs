//! Property tests and edge cases for `Frustum`, `Aabb` and the projection helpers.
use boxel_math::projection::{look_to, perspective_infinite_reverse};
use boxel_math::{Aabb, Frustum, Mat4, Vec3, Vec4};
use proptest::prelude::*;

/// Direction from yaw (around +Y) and pitch, both in radians (yaw 0, pitch 0 = -Z).
fn direction(yaw: f32, pitch: f32) -> Vec3 {
    Vec3::new(
        -yaw.sin() * pitch.cos(),
        pitch.sin(),
        -yaw.cos() * pitch.cos(),
    )
}

/// Up vector for `dir` that is never parallel to it.
fn up_for(dir: Vec3) -> Vec3 {
    if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y }
}

fn view_proj(eye: Vec3, dir: Vec3, fov_y: f32, aspect: f32, near: f32) -> Mat4 {
    perspective_infinite_reverse(fov_y, aspect, near) * look_to(eye, dir, up_for(dir))
}

fn corners(aabb: &Aabb) -> [Vec3; 8] {
    let (a, b) = (aabb.min, aabb.max);
    [
        Vec3::new(a.x, a.y, a.z),
        Vec3::new(b.x, a.y, a.z),
        Vec3::new(a.x, b.y, a.z),
        Vec3::new(b.x, b.y, a.z),
        Vec3::new(a.x, a.y, b.z),
        Vec3::new(b.x, a.y, b.z),
        Vec3::new(a.x, b.y, b.z),
        Vec3::new(b.x, b.y, b.z),
    ]
}

fn inside_all_planes(frustum: &Frustum, p: Vec3) -> bool {
    frustum
        .planes
        .iter()
        .all(|pl| pl.truncate().dot(p) + pl.w >= 0.0)
}

prop_compose! {
    fn camera()(
        eye in prop::array::uniform3(-500.0f32..500.0),
        yaw in -std::f32::consts::PI..std::f32::consts::PI,
        pitch in -1.5f32..1.5,
        fov_deg in 30.0f32..110.0,
        aspect in 0.5f32..2.5,
        near in 0.1f32..2.0,
    ) -> (Vec3, Vec3, Mat4) {
        let eye = Vec3::from_array(eye);
        let dir = direction(yaw, pitch);
        (eye, dir, view_proj(eye, dir, fov_deg.to_radians(), aspect, near))
    }
}

prop_compose! {
    /// A box near the camera (offset in [-200, 200]) with half extents in [0, 50].
    fn aabb_near(eye: Vec3)(
        offset in prop::array::uniform3(-200.0f32..200.0),
        half in prop::array::uniform3(0.0f32..50.0),
    ) -> Aabb {
        Aabb::from_center_half_extents(eye + Vec3::from_array(offset), Vec3::from_array(half))
    }
}

fn camera_and_box() -> impl Strategy<Value = (Vec3, Vec3, Mat4, Aabb)> {
    camera()
        .prop_flat_map(|(eye, dir, vp)| aabb_near(eye).prop_map(move |aabb| (eye, dir, vp, aabb)))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// The p-vertex test is conservative: a box with any corner inside every plane is kept.
    #[test]
    fn a_corner_inside_all_planes_means_intersection((_eye, _dir, vp, aabb) in camera_and_box()) {
        let frustum = Frustum::from_view_proj_reverse_z(vp);
        if corners(&aabb).iter().any(|&c| inside_all_planes(&frustum, c)) {
            prop_assert!(frustum.intersects_aabb(&aabb));
        }
    }

    /// Same with a finite view distance.
    #[test]
    fn a_corner_inside_all_planes_means_intersection_with_max_distance(
        (eye, dir, vp, aabb) in camera_and_box(),
        distance in 1.0f32..300.0,
    ) {
        let mut frustum = Frustum::from_view_proj_reverse_z(vp);
        frustum.set_max_distance(eye, dir, distance);
        if corners(&aabb).iter().any(|&c| inside_all_planes(&frustum, c)) {
            prop_assert!(frustum.intersects_aabb(&aabb));
        }
    }

    /// Points that project inside the clip volume (wgpu: |x|, |y| <= w, 0 < z <= w) are inside
    /// every extracted plane, and so is any box that contains them.
    #[test]
    fn points_inside_clip_space_are_inside_the_planes(
        (_eye, _dir, vp, aabb) in camera_and_box(),
    ) {
        let frustum = Frustum::from_view_proj_reverse_z(vp);
        let c = aabb.center();
        let clip = vp * c.extend(1.0);
        let margin = 1e-3 * clip.w.abs();
        let strictly_inside = clip.w > 0.0
            && clip.x.abs() < clip.w - margin
            && clip.y.abs() < clip.w - margin
            && clip.z > margin
            && clip.z < clip.w - margin;
        if strictly_inside {
            for plane in frustum.planes {
                prop_assert!(plane.truncate().dot(c) + plane.w >= -1e-3, "{plane:?} {c:?}");
            }
            prop_assert!(frustum.intersects_aabb(&aabb));
        }
    }

    /// A box entirely behind the camera (beyond the near plane on the wrong side) is rejected.
    #[test]
    fn boxes_behind_the_camera_are_rejected(
        (eye, dir, vp) in camera(),
        back in 3.0f32..300.0,
        half in 0.0f32..1.0,
    ) {
        let frustum = Frustum::from_view_proj_reverse_z(vp);
        let aabb = Aabb::from_center_half_extents(eye - dir * back, Vec3::splat(half));
        prop_assert!(!frustum.intersects_aabb(&aabb));
    }

    /// Planes are normalized (unit normals), so `dot(n, p) + d` is a signed distance.
    #[test]
    fn planes_are_normalized((_eye, _dir, vp) in camera()) {
        let frustum = Frustum::from_view_proj_reverse_z(vp);
        for plane in &frustum.planes[..5] {
            prop_assert!((plane.truncate().length() - 1.0).abs() < 1e-4, "{plane:?}");
        }
        prop_assert_eq!(frustum.planes[5], Vec4::new(0.0, 0.0, 0.0, 1.0));
    }

    /// `from_center_half_extents` and `center`/`half_extents` round-trip; boxes contain their
    /// center and corners and intersect themselves.
    #[test]
    fn aabb_round_trips(
        c in prop::array::uniform3(-1000.0f32..1000.0),
        h in prop::array::uniform3(0.0f32..100.0),
    ) {
        let (c, h) = (Vec3::from_array(c), Vec3::from_array(h));
        let aabb = Aabb::from_center_half_extents(c, h);
        prop_assert!((aabb.center() - c).abs().max_element() < 1e-3);
        prop_assert!((aabb.half_extents() - h).abs().max_element() < 1e-3);
        prop_assert!(aabb.contains_point(aabb.center()));
        for corner in corners(&aabb) {
            prop_assert!(aabb.contains_point(corner));
        }
        prop_assert!(aabb.intersects(&aabb));
    }

    /// `intersects` is symmetric.
    #[test]
    fn aabb_intersects_is_symmetric(
        a in prop::array::uniform3(-10.0f32..10.0),
        ha in prop::array::uniform3(0.0f32..5.0),
        b in prop::array::uniform3(-10.0f32..10.0),
        hb in prop::array::uniform3(0.0f32..5.0),
    ) {
        let a = Aabb::from_center_half_extents(Vec3::from_array(a), Vec3::from_array(ha));
        let b = Aabb::from_center_half_extents(Vec3::from_array(b), Vec3::from_array(hb));
        prop_assert_eq!(a.intersects(&b), b.intersects(&a));
    }
}

fn origin_frustum() -> Frustum {
    Frustum::from_view_proj_reverse_z(view_proj(
        Vec3::ZERO,
        Vec3::NEG_Z,
        70f32.to_radians(),
        16.0 / 9.0,
        0.1,
    ))
}

#[test]
fn degenerate_point_boxes_follow_the_point() {
    let f = origin_frustum();
    let point = |p: Vec3| Aabb::new(p, p);
    assert!(f.intersects_aabb(&point(Vec3::new(0.0, 0.0, -5.0))));
    assert!(!f.intersects_aabb(&point(Vec3::new(0.0, 0.0, 5.0))));
    assert!(
        !f.intersects_aabb(&point(Vec3::new(0.0, 0.0, -0.05))),
        "before near"
    );
}

#[test]
fn boxes_around_the_eye_and_enclosing_boxes_are_kept() {
    let f = origin_frustum();
    assert!(f.intersects_aabb(&Aabb::from_center_half_extents(Vec3::ZERO, Vec3::ONE)));
    assert!(f.intersects_aabb(&Aabb::from_center_half_extents(
        Vec3::ZERO,
        Vec3::splat(1.0e6)
    )));
}

#[test]
fn a_box_between_the_eye_and_the_near_plane_is_rejected() {
    let f = origin_frustum();
    let tiny = Aabb::new(Vec3::new(-0.01, -0.01, -0.05), Vec3::new(0.01, 0.01, -0.01));
    assert!(!f.intersects_aabb(&tiny));
    // Straddling the near plane is kept.
    let straddle = Aabb::new(Vec3::new(-0.01, -0.01, -0.5), Vec3::new(0.01, 0.01, -0.01));
    assert!(f.intersects_aabb(&straddle));
}

#[test]
fn boxes_touching_a_plane_are_kept() {
    let mut f = origin_frustum();
    f.set_max_distance(Vec3::ZERO, Vec3::NEG_Z, 50.0);
    // The far plane is z = -50; a box whose nearest face lies exactly on it touches it.
    let touching = Aabb::new(Vec3::new(-1.0, -1.0, -60.0), Vec3::new(1.0, 1.0, -50.0));
    assert!(f.intersects_aabb(&touching));
    let beyond = Aabb::new(Vec3::new(-1.0, -1.0, -60.0), Vec3::new(1.0, 1.0, -50.01));
    assert!(!f.intersects_aabb(&beyond));
}

#[test]
fn max_distance_follows_the_eye_and_direction() {
    let eye = Vec3::new(100.0, 20.0, -30.0);
    let mut f =
        Frustum::from_view_proj_reverse_z(view_proj(eye, Vec3::X, 70f32.to_radians(), 1.0, 0.1));
    f.set_max_distance(eye, Vec3::X, 10.0);
    let at = |x: f32| Aabb::from_center_half_extents(eye + Vec3::X * x, Vec3::splat(0.25));
    assert!(f.intersects_aabb(&at(5.0)));
    assert!(f.intersects_aabb(&at(10.0)));
    assert!(!f.intersects_aabb(&at(11.0)));
    assert!(!f.intersects_aabb(&at(-5.0)));
}

#[test]
fn to_array_keeps_the_plane_order() {
    let f = origin_frustum();
    let arrays = f.to_array();
    for (array, plane) in arrays.iter().zip(f.planes) {
        assert_eq!(*array, plane.to_array());
    }
    // Left plane faces +X (inside is to its right), right plane faces -X, bottom +Y, top -Y,
    // near -Z, far is the always-inside (0, 0, 0, 1).
    assert!(arrays[0][0] > 0.0);
    assert!(arrays[1][0] < 0.0);
    assert!(arrays[2][1] > 0.0);
    assert!(arrays[3][1] < 0.0);
    assert!(arrays[4][2] < 0.0);
    assert_eq!(arrays[5], [0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn reverse_z_maps_near_to_one_and_infinity_to_zero() {
    let near = 0.1;
    let proj = perspective_infinite_reverse(70f32.to_radians(), 1.5, near);
    let depth = |z: f32| {
        let clip = proj * Vec4::new(0.0, 0.0, z, 1.0);
        clip.z / clip.w
    };
    assert!((depth(-near) - 1.0).abs() < 1e-6);
    assert!(depth(-1.0e7) < 1e-6);
    assert!(depth(-1.0) > depth(-2.0), "depth decreases with distance");
}

#[test]
fn look_to_moves_the_eye_to_the_origin_looking_down_neg_z() {
    let eye = Vec3::new(1.0, 2.0, 3.0);
    let view = look_to(eye, Vec3::X, Vec3::Y);
    let ahead = view * Vec4::new(2.0, 2.0, 3.0, 1.0);
    assert!((ahead - Vec4::new(0.0, 0.0, -1.0, 1.0)).abs().max_element() < 1e-6);
    let above = view * Vec4::new(1.0, 3.0, 3.0, 1.0);
    assert!((above - Vec4::new(0.0, 1.0, 0.0, 1.0)).abs().max_element() < 1e-6);
    let right = view * Vec4::new(1.0, 2.0, 4.0, 1.0);
    assert!((right - Vec4::new(1.0, 0.0, 0.0, 1.0)).abs().max_element() < 1e-6);
}

#[test]
fn look_to_accepts_unnormalized_directions() {
    let eye = Vec3::new(-7.0, 2.5, 30.0);
    let dir = Vec3::new(0.3, -0.2, -0.9);
    let unit = look_to(eye, dir.normalize(), Vec3::Y);
    for scale in [0.01, 4.0, 1000.0] {
        let scaled = look_to(eye, dir * scale, Vec3::Y * 3.0);
        assert!(unit.abs_diff_eq(scaled, 1e-5), "scale {scale}: {scaled:?}");
    }
}
