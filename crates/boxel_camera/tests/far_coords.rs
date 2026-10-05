//! The camera-relative frame far from the origin: `render_origin` is `i64`, `eye_rel` stays exact,
//! and the GPU's 32-bit wrapping subtraction of chunk and render origins gives the same image
//! input as near the origin.
use boxel_camera::{Camera, CameraView};
use boxel_math::{DVec3, I64Vec3, IVec3, Vec3, Vec4, wrap_to_i32};
use boxel_transform::Transform;

/// Integer camera positions (voxels) around the former `i32` edge and far beyond it.
const FAR: [f64; 12] = [
    2_147_483_647.0,      // 2^31 - 1
    2_147_483_648.0,      // 2^31
    2_147_483_649.0,      // 2^31 + 1
    2_147_484_648.0,      // 2^31 + 1000
    1_099_511_627_776.0,  // 2^40
    1e15,                 // f64 step 0.125
    -2_147_483_648.0,     // -2^31
    -2_147_483_649.0,     // -2^31 - 1
    -2_147_484_648.0,     // -2^31 - 1000
    -1_099_511_627_779.0, // -2^40 - 3
    -1e15,                // f64 step 0.125
    4_398_046_511_104.0,  // 2^42 (1 mm f64 steps)
];

/// Eye fraction inside the voxel: multiples of 1/8, exact at every `FAR` position.
const FRAC: DVec3 = DVec3::new(0.25, 0.75, 0.5);

fn view_at(eye: DVec3) -> CameraView {
    CameraView::compute(
        &Camera::default(),
        &Transform::from_yaw_pitch(eye, 0.4, -0.2),
        16.0 / 9.0,
    )
}

/// Where the GPU puts world point `block + local` (`local` in voxels inside that block's chunk):
/// the chunk origin and the render origin wrapped to `i32` and subtracted with wrapping, like
/// `finish_vertex` in `common.wgsl`. Returns NDC xy and the clip w.
fn gpu_project(view: &CameraView, block: I64Vec3, local: Vec3) -> Vec4 {
    let chunk_origin = block.div_euclid(I64Vec3::splat(32)) * 32;
    let in_chunk = (block - chunk_origin).as_vec3() + local;
    let rel = wrap_to_i32(chunk_origin)
        .wrapping_sub(view.gpu_render_origin())
        .as_vec3()
        + in_chunk;
    let clip = view.view_proj * rel.extend(1.0);
    Vec4::new(clip.x / clip.w, clip.y / clip.w, clip.z / clip.w, clip.w)
}

#[test]
fn render_origin_and_eye_rel_are_exact_far_away() {
    for d in FAR {
        for axis_signs in [DVec3::ONE, DVec3::new(1.0, -1.0, 1.0)] {
            let base = axis_signs * d;
            let view = view_at(base + FRAC);
            let expected = I64Vec3::new(base.x as i64, base.y as i64, base.z as i64);
            assert_eq!(view.render_origin, expected, "at {base}");
            assert_eq!(view.eye_rel, FRAC.as_vec3(), "at {base}");
            // The GPU value is the low 32 bits.
            assert_eq!(
                view.gpu_render_origin(),
                IVec3::new(expected.x as i32, expected.y as i32, expected.z as i32)
            );
        }
    }
}

#[test]
fn gpu_relative_positions_match_the_origin_far_away() {
    // Points around the camera, in voxels relative to its voxel: ahead, behind a chunk border,
    // and 1000 voxels away (several chunks across the wrapped edge).
    let offsets = [
        (I64Vec3::new(0, 0, -8), Vec3::new(0.01, 0.02, 0.0)),
        (I64Vec3::new(-40, 3, -70), Vec3::new(0.5, 0.5, 0.5)),
        (I64Vec3::new(31, -1, -1000), Vec3::ZERO),
        (I64Vec3::new(1000, 20, -999), Vec3::new(0.25, 0.0, 0.75)),
    ];
    for d in FAR {
        let base = I64Vec3::splat(d as i64);
        // The same place in the chunk grid near the origin: the GPU sees the same chunk-relative
        // integers and the same in-chunk floats, so the results are bit-identical.
        let aligned = base.rem_euclid(I64Vec3::splat(32));
        let near = view_at(aligned.as_dvec3() + FRAC);
        let far = view_at(DVec3::splat(d) + FRAC);
        assert_eq!(far.view_proj, near.view_proj, "at {d}");
        for (offset, local) in offsets {
            let a = gpu_project(&near, aligned + offset, local);
            let b = gpu_project(&far, base + offset, local);
            assert_eq!(a, b, "offset {offset} at {d}");
        }
    }
}

#[test]
fn wrapped_difference_is_exact_across_the_edge() {
    // Camera just below 2^31, chunk just above: the i32 values wrap to opposite signs but their
    // wrapping difference is the true distance.
    let view = view_at(DVec3::new(2_147_483_647.5, 70.5, -2_147_483_648.5));
    assert_eq!(
        view.render_origin,
        I64Vec3::new((1 << 31) - 1, 70, -(1 << 31) - 1)
    );
    let chunk = I64Vec3::new(1 << 31, 64, -(1 << 31) - 32);
    let rel = wrap_to_i32(chunk).wrapping_sub(view.gpu_render_origin());
    assert_eq!(rel, IVec3::new(1, -6, -31));
}
