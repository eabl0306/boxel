//! Posing a model's boxes into vertices on the CPU (ADR 0040): the box-UV layout of each face,
//! the frames of the parts at a pose, and Minecraft's entity shading.

use super::{Cuboid, Model, PartPose, Pose};
use boxel_math::glam::Affine3A;
use boxel_math::{Mat3, Vec3};
use boxel_shaders::gpu::model::{REALM_SHIFT, SHADE_SHIFT};
use boxel_shaders::gpu::{ModelVertex, REALM_ENVS};
use boxel_world::DEFAULT_FACE_SHADE;

/// The two light directions of Minecraft's entity shading (`DIFFUSE_LIGHT_0` and
/// `DIFFUSE_LIGHT_1`), not normalized, in the frame of the shaded normals: the world (+Y up,
/// north -Z) for [`super::ModelLayer::World`], the camera's frame for
/// [`super::ModelLayer::View`].
pub const ENTITY_LIGHT_DIRECTIONS: [Vec3; 2] =
    [Vec3::new(0.2, 1.0, -0.7), Vec3::new(-0.2, 1.0, 0.7)];

/// Minecraft's entity shading of a face whose outward unit normal is `normal` (in the frame of
/// [`ENTITY_LIGHT_DIRECTIONS`]): `0.4 + 0.6 * (max(n · l0, 0) + max(n · l1, 0))`, at most 1. A
/// top face gets 1, a bottom face 0.4, north and south faces about 0.74, east and west 0.5.
pub fn entity_shade(normal: Vec3) -> f32 {
    let [a, b] = ENTITY_LIGHT_DIRECTIONS.map(Vec3::normalize);
    (0.4 + 0.6 * (normal.dot(a).max(0.0) + normal.dot(b).max(0.0))).min(1.0)
}

/// [`entity_shade`] in a realm whose cube faces are shaded by `table` (the mesher's face order,
/// +Y, -Y, +X, -X, +Z, -Z) instead of [`DEFAULT_FACE_SHADE`]: Minecraft's entity shading with
/// its darkness scaled as the realm scales the darkness of the cube faces, blended over the
/// normal's axes (by the squares of its components). The default table gives `entity_shade`
/// exactly, a flat table (all 1) a flat model, and the result stays in `0..=1`. The darkness of
/// a top face is 0 in both tables, so a face that looks straight up keeps its shade of 1.
pub fn realm_shade(normal: Vec3, table: &[f32; 6]) -> f32 {
    let shade = entity_shade(normal);
    if *table == DEFAULT_FACE_SHADE {
        return shade;
    }
    let dark = |t: &[f32; 6]| {
        let pick = |c: f32, pos: usize| {
            if c >= 0.0 {
                1.0 - t[pos]
            } else {
                1.0 - t[pos + 1]
            }
        };
        normal.y * normal.y * pick(normal.y, 0)
            + normal.x * normal.x * pick(normal.x, 2)
            + normal.z * normal.z * pick(normal.z, 4)
    };
    let base = dark(&DEFAULT_FACE_SHADE);
    if base <= 0.0 {
        return shade;
    }
    (1.0 - (1.0 - shade) * (dark(table) / base)).clamp(0.0, 1.0)
}

/// The texture rectangle of a face, in the model's texels: `[min, max)` on each axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceTexels {
    /// Top-left corner.
    pub min: [f32; 2],
    /// Bottom-right corner (exclusive).
    pub max: [f32; 2],
}

/// One face of a box in its part's frame (model space): its corners counter-clockwise seen from
/// outside, each with its texel coordinates on the model's texture, its outward normal and its
/// texture rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PosedFace {
    /// Corners, counter-clockwise from outside (right-handed, as the world).
    pub corners: [Vec3; 4],
    /// Each corner's texel coordinates.
    pub uv: [[f32; 2]; 4],
    /// Outward unit normal.
    pub normal: Vec3,
    /// The texture rectangle.
    pub texels: FaceTexels,
}

/// The six faces of `c` as Minecraft builds them (`ModelPart.Cube`): the top (y = min, model
/// space is +Y down), the bottom, the side at x = min, the front (z = min), the side at x = max
/// and the back, in its box-UV layout ([`Cuboid`]). The box grows by its inflate without its
/// texture changing; a mirrored box swaps its x corners and reverses each face's corners (each
/// keeps its texel), so its texture shows mirrored and it still winds counter-clockwise.
pub fn cuboid_faces(c: &Cuboid) -> [PosedFace; 6] {
    let lo = c.origin - Vec3::splat(c.inflate);
    let hi = c.origin + c.size + Vec3::splat(c.inflate);
    let (mut x0, mut x1) = (lo.x, hi.x);
    if c.mirror {
        std::mem::swap(&mut x0, &mut x1);
    }
    let (y0, y1, z0, z1) = (lo.y, hi.y, lo.z, hi.z);
    let p = Vec3::new;
    let (v1, v2, v3, v4) = (p(x0, y0, z0), p(x1, y0, z0), p(x1, y1, z0), p(x0, y1, z0));
    let (v5, v6, v7, v8) = (p(x0, y0, z1), p(x1, y0, z1), p(x1, y1, z1), p(x0, y1, z1));
    let [u, v] = c.uv;
    let (w, h, d) = (c.size.x, c.size.y, c.size.z);
    let (j, k, l, m) = (u, u + d, u + d + w, u + d + w + w);
    let (n, o) = (u + d + w + d, u + d + w + d + w);
    let (pp, q, r) = (v, v + d, v + d + h);
    // Minecraft's polygons: corners, then (u1, v1, u2, v2) and the normal. Corner 0 takes
    // (u2, v1), 1 (u1, v1), 2 (u1, v2) and 3 (u2, v2).
    let faces = [
        ([v6, v5, v1, v2], [k, pp, l, q], Vec3::NEG_Y),
        ([v3, v4, v8, v7], [l, q, m, pp], Vec3::Y),
        ([v1, v5, v8, v4], [j, q, k, r], Vec3::NEG_X),
        ([v2, v1, v4, v3], [k, q, l, r], Vec3::NEG_Z),
        ([v6, v2, v3, v7], [l, q, n, r], Vec3::X),
        ([v5, v6, v7, v8], [n, q, o, r], Vec3::Z),
    ];
    faces.map(|(mut corners, [u1, v1, u2, v2], mut normal)| {
        let mut uv = [[u2, v1], [u1, v1], [u1, v2], [u2, v2]];
        if c.mirror {
            corners.reverse();
            uv.reverse();
            normal.x = -normal.x;
        }
        PosedFace {
            corners,
            uv,
            normal,
            texels: FaceTexels {
                min: [u1.min(u2), v1.min(v2)],
                max: [u1.max(u2), v1.max(v2)],
            },
        }
    })
}

/// The frame of a part in its parent's: moved to its offset, then turned by `Rz * Ry * Rx`.
fn part_frame(pose: &PartPose) -> Affine3A {
    let r = pose.rotation;
    let turn = Mat3::from_rotation_z(r.z) * Mat3::from_rotation_y(r.y) * Mat3::from_rotation_x(r.x);
    Affine3A::from_mat3_translation(turn, pose.offset)
}

/// The frame of every part of `model` at `pose`, in model space (pixels), and whether it shows
/// (it and all its parents visible), into `out` (one entry per part). A pose shorter than the
/// model takes the rest pose for the parts it lacks.
pub fn pose_parts(model: &Model, pose: &Pose, out: &mut Vec<(Affine3A, bool)>) {
    out.clear();
    for (i, part) in model.parts().iter().enumerate() {
        let p = pose.parts.get(i).unwrap_or(&part.rest);
        let (parent, shown) = part
            .parent
            .and_then(|id| out.get(id.index()).copied())
            .unwrap_or((Affine3A::IDENTITY, true));
        out.push((parent * part_frame(p), shown && p.visible));
    }
}

/// Model space to an instance's frame: the point `origin` to the origin, flipped to +Y up
/// (Minecraft's `scale(-1, -1, 1)`, a half turn about z: the model's front, -Z, stays the
/// front), one pixel `scale / 16` blocks.
pub(crate) fn model_to_local(origin: Vec3, scale: f32) -> Affine3A {
    let s = scale / 16.0;
    Affine3A::from_scale(Vec3::new(-s, -s, s)) * Affine3A::from_translation(-origin)
}

/// What [`pose_vertices`] needs of an instance besides its model and pose.
pub(crate) struct InstanceFrame {
    /// Model space to the space of the vertices (pixels to blocks).
    pub to_space: Affine3A,
    /// Texels of the skin per texel of the model's texture.
    pub texel_scale: [f32; 2],
    /// The instance's light (`LightValue` bits).
    pub light: u16,
    /// The realm it is lit in.
    pub realm: super::ModelRealm,
}

/// The box faces of `model` at `pose` as vertices (four per face, counter-clockwise from
/// outside) in the space of `frame`, appended to `out[0]` (opaque boxes) and `out[1]` (cutout
/// boxes) in part order; `parts` is scratch. Faces whose texture rectangle is empty on the skin
/// are left out. Each face is shaded by its normal in that space, relative to the realm's face
/// shade ([`realm_shade`]), and carries the realm the shader lights it with.
pub(crate) fn pose_vertices(
    model: &Model,
    pose: &Pose,
    frame: &InstanceFrame,
    parts: &mut Vec<(Affine3A, bool)>,
    out: &mut [Vec<ModelVertex>; 2],
) {
    pose_parts(model, pose, parts);
    let [su, sv] = frame.texel_scale;
    let texel = |x: f32| x.clamp(0.0, f32::from(u16::MAX)) as u16;
    let realm = u32::from(frame.realm.index).min(REALM_ENVS as u32 - 1) << REALM_SHIFT;
    for (part, &(at, shown)) in model.parts().iter().zip(parts.iter()) {
        if !shown {
            continue;
        }
        let to = frame.to_space * at;
        let turn = Mat3::from(to.matrix3);
        for c in &part.cuboids {
            let out = &mut out[c.material as usize];
            for face in cuboid_faces(c) {
                let rect = [
                    texel((face.texels.min[0] * su).floor()),
                    texel((face.texels.min[1] * sv).floor()),
                    texel((face.texels.max[0] * su).ceil()),
                    texel((face.texels.max[1] * sv).ceil()),
                ];
                if rect[2] <= rect[0] || rect[3] <= rect[1] {
                    continue;
                }
                let normal = (turn * face.normal).normalize_or_zero();
                let shade = (realm_shade(normal, &frame.realm.face_shade) * 255.0).round() as u32;
                let data = u32::from(frame.light) | (shade.min(255) << SHADE_SHIFT) | realm;
                for (corner, uv) in face.corners.iter().zip(face.uv) {
                    out.push(ModelVertex {
                        pos: to.transform_point3(*corner).to_array(),
                        uv: [uv[0] * su, uv[1] * sv],
                        rect,
                        data,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelMaterial, PartId};
    use boxel_math::Quat;
    use std::f32::consts::FRAC_PI_2;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).abs().max_element() < 1e-4
    }

    /// Minecraft's head: 8 x 8 x 8 from (-4, -8, -4), its layout at (0, 0).
    fn head() -> Cuboid {
        Cuboid::new([0.0, 0.0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0])
    }

    /// The face of `faces` with outward normal `n`.
    fn face(faces: &[PosedFace; 6], n: Vec3) -> PosedFace {
        *faces
            .iter()
            .find(|f| f.normal == n)
            .expect("a face with that normal")
    }

    /// The texel of the corner of `f` at `corner`.
    fn uv_at(f: &PosedFace, corner: Vec3) -> [f32; 2] {
        let i = f
            .corners
            .iter()
            .position(|c| close(*c, corner))
            .unwrap_or_else(|| panic!("no corner at {corner} in {:?}", f.corners));
        f.uv[i]
    }

    #[test]
    fn a_head_has_minecrafts_box_uv_layout() {
        let faces = cuboid_faces(&head());
        let rect = |n: Vec3| {
            let t = face(&faces, n).texels;
            [t.min, t.max]
        };
        // Model space is +Y down: the face at y = min (normal -Y) is the top.
        assert_eq!(rect(Vec3::NEG_Y), [[8.0, 0.0], [16.0, 8.0]], "top");
        assert_eq!(rect(Vec3::Y), [[16.0, 0.0], [24.0, 8.0]], "bottom");
        assert_eq!(rect(Vec3::NEG_X), [[0.0, 8.0], [8.0, 16.0]], "right side");
        assert_eq!(rect(Vec3::NEG_Z), [[8.0, 8.0], [16.0, 16.0]], "front");
        assert_eq!(rect(Vec3::X), [[16.0, 8.0], [24.0, 16.0]], "left side");
        assert_eq!(rect(Vec3::Z), [[24.0, 8.0], [32.0, 16.0]], "back");
        // The front as seen from outside, top up: its top-left texel at the top (y = -8) of the
        // entity's right side (x = -4 in model space, on the viewer's left).
        let front = face(&faces, Vec3::NEG_Z);
        assert_eq!(uv_at(&front, Vec3::new(-4.0, -8.0, -4.0)), [8.0, 8.0]);
        assert_eq!(uv_at(&front, Vec3::new(4.0, -8.0, -4.0)), [16.0, 8.0]);
        assert_eq!(uv_at(&front, Vec3::new(4.0, 0.0, -4.0)), [16.0, 16.0]);
        // The back: its top-left texel on the entity's left (x = 4), on the viewer's left from
        // behind.
        let back = face(&faces, Vec3::Z);
        assert_eq!(uv_at(&back, Vec3::new(4.0, -8.0, 4.0)), [24.0, 8.0]);
        // The top seen from above with the back up: its top-left texel at the back right.
        let top = face(&faces, Vec3::NEG_Y);
        assert_eq!(uv_at(&top, Vec3::new(-4.0, -8.0, 4.0)), [8.0, 0.0]);
        assert_eq!(uv_at(&top, Vec3::new(-4.0, -8.0, -4.0)), [8.0, 8.0]);
        // The right side seen from outside: the back on the viewer's left.
        let right = face(&faces, Vec3::NEG_X);
        assert_eq!(uv_at(&right, Vec3::new(-4.0, -8.0, 4.0)), [0.0, 8.0]);
        assert_eq!(uv_at(&right, Vec3::new(-4.0, -8.0, -4.0)), [8.0, 8.0]);
    }

    #[test]
    fn faces_wind_counter_clockwise_from_outside_mirrored_or_not() {
        for c in [head(), head().mirror(), head().inflate(0.5).mirror()] {
            for f in cuboid_faces(&c) {
                let [a, b, cc, d] = f.corners;
                for (p, q, r) in [(a, b, cc), (a, cc, d)] {
                    let n = (q - p).cross(r - p);
                    assert!(n.dot(f.normal) > 0.0, "{c:?}: {f:?}");
                }
                let center = f.corners.iter().copied().sum::<Vec3>() / 4.0;
                let middle = c.origin + c.size / 2.0;
                assert!(
                    (center - middle).dot(f.normal) > 0.0,
                    "the normal points out: {f:?}"
                );
            }
        }
    }

    #[test]
    fn a_mirrored_box_shows_its_texture_mirrored() {
        let faces = cuboid_faces(&head().mirror());
        // The front's top-left texel goes to the other side.
        let front = face(&faces, Vec3::NEG_Z);
        assert_eq!(uv_at(&front, Vec3::new(4.0, -8.0, -4.0)), [8.0, 8.0]);
        assert_eq!(front.texels.min, [8.0, 8.0]);
        // The right side's texture is now on the side at x = max.
        let x_max = face(&faces, Vec3::X);
        assert_eq!(x_max.texels.min, [0.0, 8.0]);
        assert_eq!(face(&faces, Vec3::NEG_X).texels.min, [16.0, 8.0]);
    }

    #[test]
    fn inflating_grows_the_box_and_keeps_its_texture() {
        let plain = cuboid_faces(&head());
        let hat = cuboid_faces(&head().inflate(0.5));
        for (a, b) in plain.iter().zip(&hat) {
            assert_eq!(a.uv, b.uv);
            assert_eq!(a.texels, b.texels);
            for (p, q) in a.corners.iter().zip(&b.corners) {
                assert!(close((*q - *p).abs(), Vec3::splat(0.5)), "{p} {q}");
            }
        }
    }

    /// A figure: a body at the root with a head and an arm under it.
    fn figure() -> Model {
        let mut b = Model::builder([64, 64]);
        let body = b.part("body", None, PartPose::default());
        b.cuboid(
            body,
            Cuboid::new([16.0, 16.0], [-4.0, 0.0, -2.0], [8.0, 12.0, 4.0]),
        );
        let head_part = b.part("head", Some(body), PartPose::default());
        b.cuboid(head_part, head());
        let arm = b.part("arm", Some(body), PartPose::at([-5.0, 2.0, 0.0]));
        b.cuboid(
            arm,
            Cuboid::new([40.0, 16.0], [-3.0, -2.0, -2.0], [4.0, 12.0, 4.0]),
        );
        b.cuboid(
            arm,
            Cuboid::new([40.0, 32.0], [-3.0, -2.0, -2.0], [4.0, 12.0, 4.0])
                .inflate(0.25)
                .cutout(),
        );
        b.build().expect("valid")
    }

    #[test]
    fn parts_follow_their_parents_and_turn_about_their_pivot() {
        let model = figure();
        let arm = model.part("arm").expect("arm");
        let body = model.part("body").expect("body");
        let mut pose = model.rest_pose();
        let mut parts = Vec::new();
        pose_parts(&model, &pose, &mut parts);
        // At rest the arm's hand end, 10 below its pivot (the box starts 2 above it), is at the
        // pivot plus (0, 10, 0).
        assert!(close(
            parts[arm.index()]
                .0
                .transform_point3(Vec3::new(0.0, 10.0, 0.0)),
            Vec3::new(-5.0, 12.0, 0.0)
        ));
        // Swung forward by a quarter turn about x (Minecraft's arm swing is a negative xRot):
        // the hand points to -Z, the front.
        pose[arm].rotation.x = -FRAC_PI_2;
        pose_parts(&model, &pose, &mut parts);
        assert!(close(
            parts[arm.index()]
                .0
                .transform_point3(Vec3::new(0.0, 10.0, 0.0)),
            Vec3::new(-5.0, 2.0, -10.0)
        ));
        // Turning the body (y) carries the arm along: a quarter turn puts its pivot at +Z.
        pose[body].rotation.y = FRAC_PI_2;
        pose[arm].rotation.x = 0.0;
        pose_parts(&model, &pose, &mut parts);
        assert!(close(
            parts[arm.index()].0.transform_point3(Vec3::ZERO),
            Vec3::new(0.0, 2.0, 5.0)
        ));
        // The rotation is Rz * Ry * Rx: x first, then y, about the parent's axes.
        pose[body].rotation = Vec3::ZERO;
        pose[arm].rotation = Vec3::new(-FRAC_PI_2, FRAC_PI_2, 0.0);
        pose_parts(&model, &pose, &mut parts);
        let hand = parts[arm.index()]
            .0
            .transform_point3(Vec3::new(0.0, 10.0, 0.0));
        let expected = Vec3::new(-5.0, 2.0, 0.0)
            + Quat::from_rotation_y(FRAC_PI_2)
                * (Quat::from_rotation_x(-FRAC_PI_2) * Vec3::Y * 10.0);
        assert!(close(hand, expected), "{hand} {expected}");
    }

    #[test]
    fn hiding_a_part_hides_its_children() {
        let model = figure();
        let body = model.part("body").expect("body");
        let mut pose = model.rest_pose();
        pose[body].visible = false;
        let mut parts = Vec::new();
        pose_parts(&model, &pose, &mut parts);
        assert!(parts.iter().all(|(_, shown)| !shown));
        // A short pose takes the rest for the missing parts.
        let mut short = model.rest_pose();
        short.parts.truncate(1);
        pose_parts(&model, &short, &mut parts);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|(_, shown)| *shown));
    }

    #[test]
    fn entity_shading_is_minecrafts() {
        assert_eq!(entity_shade(Vec3::Y), 1.0);
        assert!((entity_shade(Vec3::NEG_Y) - 0.4).abs() < 1e-6);
        let north = entity_shade(Vec3::NEG_Z);
        assert!((north - 0.7395).abs() < 1e-3, "{north}");
        assert!((entity_shade(Vec3::Z) - north).abs() < 1e-6);
        let east = entity_shade(Vec3::X);
        assert!((east - 0.4970).abs() < 1e-3, "{east}");
    }

    /// The default table keeps Minecraft's entity shading bit for bit; a flat table makes every
    /// face full bright; a table between keeps the order of the faces and stays in `0..=1`.
    #[test]
    fn realm_shading_scales_the_entity_shading() {
        let normals = [
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Z,
            Vec3::NEG_Z,
            Vec3::new(0.8, 0.6, 0.0),
            Vec3::new(-0.36, -0.48, 0.8),
            Vec3::ZERO,
        ];
        for n in normals {
            assert_eq!(
                realm_shade(n, &DEFAULT_FACE_SHADE).to_bits(),
                entity_shade(n).to_bits(),
                "{n}"
            );
            if n != Vec3::ZERO {
                assert_eq!(realm_shade(n, &[1.0; 6]), 1.0, "{n}");
            }
        }
        // Halfway to flat: every face but the top brighter than Minecraft's, never past 1.
        let half = DEFAULT_FACE_SHADE.map(|s| s + (1.0 - s) * 0.5);
        for n in &normals[1..8] {
            let s = realm_shade(*n, &half);
            assert!(s > entity_shade(*n) && s <= 1.0, "{n}: {s}");
        }
        assert_eq!(realm_shade(Vec3::Y, &half), 1.0);
        // A darker table: darker faces, never under 0.
        let dark = [1.0, 0.0, 0.2, 0.2, 0.1, 0.1];
        for n in &normals[1..8] {
            let s = realm_shade(*n, &dark);
            assert!(s < entity_shade(*n) && s >= 0.0, "{n}: {s}");
        }
    }

    /// An instance in another realm carries its realm in the vertex data and its faces take
    /// that realm's shade.
    #[test]
    fn an_instance_carries_its_realm() {
        let model = figure();
        let flat = [1.0; 6];
        let frame = InstanceFrame {
            to_space: model_to_local(Vec3::ZERO, 1.0),
            texel_scale: [1.0, 1.0],
            light: 0x0F00,
            realm: crate::model::ModelRealm {
                index: 5,
                face_shade: flat,
            },
        };
        let mut out = [Vec::new(), Vec::new()];
        pose_vertices(
            &model,
            &model.rest_pose(),
            &frame,
            &mut Vec::new(),
            &mut out,
        );
        assert!(!out[0].is_empty());
        for v in out.iter().flatten() {
            assert_eq!(v.data >> REALM_SHIFT, 5);
            assert_eq!((v.data >> SHADE_SHIFT) & 255, 255);
            assert_eq!(v.data & 0xFFFF, 0x0F00);
        }
    }

    #[test]
    fn an_instance_faces_its_forward_with_its_feet_at_the_origin() {
        // A figure posed into an instance frame turned a quarter turn left (yaw 90 degrees:
        // forward is -X), its point (0, 12, 0) (the body's bottom) at the origin, scale 16 so
        // one pixel is one block.
        let model = figure();
        let frame = InstanceFrame {
            to_space: Affine3A::from_quat(Quat::from_rotation_y(FRAC_PI_2))
                * model_to_local(Vec3::new(0.0, 12.0, 0.0), 16.0),
            texel_scale: [2.0, 2.0],
            light: 0xF000,
            realm: crate::model::ModelRealm::default(),
        };
        let mut out = [Vec::new(), Vec::new()];
        pose_vertices(
            &model,
            &model.rest_pose(),
            &frame,
            &mut Vec::new(),
            &mut out,
        );
        // Every box drawn: 2 opaque boxes and the arm, 1 cutout box.
        assert_eq!(out[ModelMaterial::Opaque as usize].len(), 3 * 6 * 4);
        assert_eq!(out[ModelMaterial::Cutout as usize].len(), 6 * 4);
        // The head is up (the top of the head 20 blocks above the origin) and the front faces
        // the instance's forward: the face with the front texture (min texel (16, 16) at two
        // texels per model texel) points to -X.
        let top = out[0].iter().map(|v| v.pos[1]).fold(f32::MIN, f32::max);
        assert!((top - 20.0).abs() < 1e-4, "{top}");
        let front: Vec<_> = out[0]
            .chunks(4)
            .filter(|q| q[0].rect == [16, 16, 32, 32])
            .collect();
        assert_eq!(front.len(), 1, "one head front");
        assert!(
            front[0].iter().all(|v| (v.pos[0] + 4.0).abs() < 1e-4),
            "{:?}",
            front[0]
        );
        // The front of the head is lit like a west face of the world, and carries the light.
        let shade = (front[0][0].data >> SHADE_SHIFT) & 255;
        assert_eq!(shade, (entity_shade(Vec3::NEG_X) * 255.0).round() as u32);
        assert_eq!(front[0][0].data & 0xFFFF, 0xF000);
        // Realm 0.
        assert_eq!(front[0][0].data >> REALM_SHIFT, 0);
        // The arm (pivot at x = -5 in model space) is on the entity's right: facing -X, its
        // right is forward x up = -Z.
        let arm_z: Vec<f32> = out[1].iter().map(|v| v.pos[2]).collect();
        assert!(arm_z.iter().all(|z| *z < 0.0), "{arm_z:?}");
        // The arm id is the third part.
        assert_eq!(model.part("arm"), Some(PartId(2)));
    }
}
