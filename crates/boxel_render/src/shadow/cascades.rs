//! The sun shadow cascades of a frame, on the CPU (sun shadows plan sec. 4.1).
//!
//! Each cascade serves a range of view distances. Its box is the bounding sphere of that slice
//! of the camera frustum, so its size never changes as the camera turns, squared in the plane
//! across the sun and stretched toward the sun up to the top of the casters. The box center is
//! snapped to whole texels of a grid fixed to the world: as the camera moves, a world point
//! keeps its place inside its texel, so shadow edges stand still.
//!
//! Far from the origin (ADR 0014) the grid hangs from an anchor, the render origin rounded down
//! to a multiple of [`SHADOW_ANCHOR`] in `i64`; the light-space math runs in `f64` relative to it,
//! and the matrices and planes come out camera-relative, like `ViewParams::view_proj`. Two
//! frames whose render origins differ by a multiple of the anchor get the same cascades, bit for
//! bit. Crossing an anchor line shifts the grid by a fraction of a texel once.

use super::{SceneSun, SunShadowSettings};
use boxel_camera::{Camera, CameraView};
use boxel_math::glam::{DMat4, DVec4};
use boxel_math::{DVec3, Frustum, I64Vec3, Mat4, Vec3, Vec4};

/// The most cascades ([`SunShadowSettings::cascades`]).
pub const MAX_CASCADES: usize = 4;

/// The snap grid of the cascades hangs from the render origin rounded down to a multiple of this
/// many voxels on each axis.
pub const SHADOW_ANCHOR: i64 = 1024;

/// The blend band between two cascades, as a fraction of the view distance where the nearer one
/// ends: the farther one starts that much before. The last cascade fades its shadows out over
/// the same fraction of the shadow distance.
pub const CASCADE_BLEND: f32 = BLEND as f32;

/// [`CASCADE_BLEND`] in `f64`, for the splits.
const BLEND: f64 = 0.1;

/// Where each cascade ends, as a fraction of the shadow distance, by cascade count. Three are
/// the plan's `[0, 20)`, `[20, 56)`, `[56, 160]` at 160 blocks.
const SPLITS: [&[f64]; MAX_CASCADES] = [
    &[1.0],
    &[0.25, 1.0],
    &[0.125, 0.35, 1.0],
    &[0.0625, 0.175, 0.45, 1.0],
];

/// Texels of margin around each cascade's sphere: half a texel of snap, one of 2x2 filtering
/// and one and a half of normal offset in the receivers, rounded up.
const PAD_TEXELS: f64 = 4.0;

/// Blocks of margin below the sphere and above the casters along the sun direction.
const DEPTH_MARGIN: f64 = 2.0;

/// The depth range is snapped to multiples of this many blocks (anchor-relative), so a box that
/// did not move keeps the same depths.
const DEPTH_SNAP: f64 = 8.0;

/// The longest stretch toward the sun past the sphere, in blocks. A low sun with tall terrain
/// would reach very far; casters beyond it are left out (the receivers keep today's look, and
/// such a low sun is faded out almost entirely).
const MAX_EXTRUSION: f64 = 512.0;

/// A face whose normal is closer to edge-on than this (`|dot(n, sun)|`) projects to a line: its
/// bucket casts nothing.
const EDGE_ON: f64 = 1e-4;

/// The face bucket normals, in the mesher's face order (+Y, -Y, +X, -X, +Z, -Z).
const BUCKET_NORMALS: [DVec3; 6] = [
    DVec3::Y,
    DVec3::NEG_Y,
    DVec3::X,
    DVec3::NEG_X,
    DVec3::Z,
    DVec3::NEG_Z,
];

/// What the cascades need from the camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowView {
    /// The integer origin the matrices are relative to (`ViewParams::render_origin`).
    pub render_origin: I64Vec3,
    /// The eye relative to `render_origin`.
    pub eye_rel: Vec3,
    /// Unit forward.
    pub forward: Vec3,
    /// Vertical field of view, radians.
    pub fov_y_radians: f32,
    /// Width over height.
    pub aspect: f32,
    /// The camera's view distance: shadows end at it if it is nearer than theirs.
    pub view_distance: f32,
    /// The highest `y` anything that casts may reach, relative to `render_origin` (the top of
    /// the loaded chunks): the boxes reach toward the sun up to it.
    pub casters_top: f32,
}

impl ShadowView {
    /// The view of `camera` at `view` with `aspect`, its casters reaching up to `casters_top`
    /// (relative to `view.render_origin`).
    pub fn new(view: &CameraView, camera: &Camera, aspect: f32, casters_top: f32) -> Self {
        Self {
            render_origin: view.render_origin,
            eye_rel: view.eye_rel,
            forward: view.forward,
            fov_y_radians: camera.fov_y_radians,
            aspect,
            view_distance: camera.view_distance,
            casters_top,
        }
    }
}

/// One cascade: the box its shadow map covers, camera-relative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cascade {
    /// Camera-relative position to the map: `x` and `y` in `-1..=1` across the sun (clip space,
    /// `y` up), depth `0` on the sun's side of the box and `1` on the far side (`w` is 1).
    pub view_proj: Mat4,
    /// The box as six planes facing inward, camera-relative, in [`Frustum`]'s order: left,
    /// right, bottom, top, then the sun's side ("near") and the far side.
    pub frustum: Frustum,
    /// The view distance from which this cascade serves (the previous one's blend band starts
    /// here; 0 for the first).
    pub start: f32,
    /// The view distance where it ends; from `end * (1 - CASCADE_BLEND)` the next one blends in
    /// (or, for the last, the shadows fade out).
    pub end: f32,
    /// The size of one texel in blocks.
    pub texel: f32,
    /// The center of the slice's bounding sphere, camera-relative.
    pub center: Vec3,
    /// Its radius in blocks.
    pub radius: f32,
    /// The box in whole steps of the anchor's grid: its center in texels across the sun, then
    /// the sun's side and the far side of its depth range in steps of 8 blocks. With the
    /// anchor and the sun direction it fixes the box in the world: a cascade whose key did not
    /// change covers the same voxels.
    pub key: [i64; 4],
}

impl Cascade {
    const EMPTY: Self = Self {
        view_proj: Mat4::ZERO,
        frustum: Frustum {
            planes: [Vec4::ZERO; 6],
        },
        start: 0.0,
        end: 0.0,
        texel: 0.0,
        center: Vec3::ZERO,
        radius: 0.0,
        key: [0; 4],
    };
}

/// The cascades of one frame ([`shadow_cascades`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowCascades {
    /// The anchor of the snap grid: the render origin rounded down to a multiple of
    /// [`SHADOW_ANCHOR`].
    pub anchor: I64Vec3,
    /// The unit direction toward the sun.
    pub light: Vec3,
    /// The face buckets that cast (bit `f` for bucket `f`, [`bucket_mask`]).
    pub bucket_mask: u32,
    /// Where the shadows end: the shadow distance, or the view distance if nearer. They fade out
    /// from `distance * (1 - CASCADE_BLEND)`.
    pub distance: f32,
    /// Texels per side of each map.
    pub resolution: u32,
    cascades: [Cascade; MAX_CASCADES],
    count: usize,
}

impl ShadowCascades {
    /// The cascades, nearest first.
    pub fn cascades(&self) -> &[Cascade] {
        &self.cascades[..self.count]
    }
}

/// The opaque face buckets that cast toward `light` (unit, toward the sun): bit `f` is set when
/// bucket `f`'s normal looks away from the sun, so its faces are the far side of their block
/// seen from the sun (sun shadows plan sec. 4.2). Edge-on buckets cast nothing.
pub fn bucket_mask(light: DVec3) -> u32 {
    BUCKET_NORMALS
        .iter()
        .enumerate()
        .filter(|(_, n)| n.dot(light) < -EDGE_ON)
        .fold(0, |mask, (f, _)| mask | 1 << f)
}

/// The view distances `[start, end)` each of `count` cascades serves, for shadows out to
/// `distance` (`count` clamped to `1..=MAX_CASCADES`).
pub fn cascade_splits(count: u32, distance: f32) -> impl Iterator<Item = [f32; 2]> {
    let splits = SPLITS[(count.clamp(1, MAX_CASCADES as u32) - 1) as usize];
    let d = f64::from(distance);
    let blend = 1.0 - BLEND;
    (0..splits.len()).map(move |i| {
        let start = if i == 0 {
            0.0
        } else {
            splits[i - 1] * d * blend
        };
        [start as f32, (splits[i] * d) as f32]
    })
}

/// The cascades of `view` for `sun` with `settings` (validated by the caller), or `None` when
/// there is nothing to draw: no shadow in the camera's realm (night, a realm without the sun,
/// strength 0) or no distance to draw them in.
pub fn shadow_cascades(
    view: &ShadowView,
    sun: &SceneSun,
    settings: &SunShadowSettings,
) -> Option<ShadowCascades> {
    let distance = settings.distance.min(view.view_distance);
    if sun.camera_strength() <= 0.0 || distance.is_nan() || distance < 1.0 {
        return None;
    }
    let light = sun.direction.normalize();
    let [u, v, w] = light_basis(light);
    let anchor = I64Vec3::new(
        view.render_origin.x.div_euclid(SHADOW_ANCHOR),
        view.render_origin.y.div_euclid(SHADOW_ANCHOR),
        view.render_origin.z.div_euclid(SHADOW_ANCHOR),
    ) * SHADOW_ANCHOR;
    // At most 1023 on each axis: exact.
    let offset = (view.render_origin - anchor).as_dvec3();
    let (ou, ov, ow) = (offset.dot(u), offset.dot(v), offset.dot(w));
    let eye = view.eye_rel.as_dvec3();
    let forward = view.forward.as_dvec3().normalize();
    let tan_y = (f64::from(view.fov_y_radians) * 0.5).tan();
    let tan_x = tan_y * f64::from(view.aspect);
    // The angle between the view axis and the frustum's corner rays.
    let cos_corner = 1.0 / (1.0 + tan_x * tan_x + tan_y * tan_y).sqrt();
    let res = f64::from(settings.resolution);
    let mut cascades = [Cascade::EMPTY; MAX_CASCADES];
    let mut count = 0;
    for ([start, end], cascade) in cascade_splits(settings.cascades, distance).zip(&mut cascades) {
        let (along, radius) = slice_sphere(f64::from(start), f64::from(end), cos_corner);
        let center = eye + forward * along;
        let half = radius / (1.0 - 2.0 * PAD_TEXELS / res);
        let texel = 2.0 * half / res;
        // The sphere's center in the anchor's light space, snapped to whole texels across the
        // sun.
        let c = center + offset;
        let (cu, cv, cw) = (c.dot(u), c.dot(v), c.dot(w));
        let (iu, iv) = ((cu / texel).round(), (cv / texel).round());
        // Along the sun: from the far side of the sphere to the highest caster over it. A ray
        // from a point `p` of the sphere reaches `casters_top` after `(top - p.y) / light.y`; its
        // largest `w` over the sphere is the center's plus `radius * sqrt(1 / y^2 - 1)`.
        let rise = f64::from(view.casters_top) - center.y;
        let reach = if light.y > 1e-6 {
            (rise + radius * (1.0 - light.y * light.y).sqrt()) / light.y
        } else {
            f64::INFINITY
        };
        let reach = reach.clamp(radius, radius + MAX_EXTRUSION);
        let near = ((cw + reach + DEPTH_MARGIN) / DEPTH_SNAP).ceil();
        let far = ((cw - radius - DEPTH_MARGIN) / DEPTH_SNAP).floor();
        // Back to camera-relative.
        let bu = iu * texel - ou;
        let bv = iv * texel - ov;
        let near_w = near * DEPTH_SNAP - ow;
        let far_w = far * DEPTH_SNAP - ow;
        let depth = near_w - far_w;
        let rows = [
            (u / half).extend(-bu / half),
            (v / half).extend(-bv / half),
            (-w / depth).extend(near_w / depth),
            DVec4::W,
        ];
        let view_proj = DMat4::from_cols(rows[0], rows[1], rows[2], rows[3]).transpose();
        let planes = [
            u.extend(half - bu),
            (-u).extend(half + bu),
            v.extend(half - bv),
            (-v).extend(half + bv),
            (-w).extend(near_w),
            w.extend(-far_w),
        ];
        *cascade = Cascade {
            view_proj: view_proj.as_mat4(),
            frustum: Frustum {
                planes: planes.map(DVec4::as_vec4),
            },
            start,
            end,
            texel: texel as f32,
            center: center.as_vec3(),
            radius: radius as f32,
            key: [iu as i64, iv as i64, near as i64, far as i64],
        };
        count += 1;
    }
    Some(ShadowCascades {
        anchor,
        light: light.as_vec3(),
        bucket_mask: bucket_mask(light),
        distance,
        resolution: settings.resolution,
        cascades,
        count,
    })
}

/// A right-handed light basis `[u, v, w]` with `w = light` (unit). `u` is the normal of the
/// plane the light and the X axis span: for any [`boxel_world::SunPath`] it is the same all
/// day, so the maps never turn as the sun moves.
fn light_basis(light: DVec3) -> [DVec3; 3] {
    let across = DVec3::new(0.0, -light.z, light.y);
    let u = if across.length_squared() > 1e-12 {
        across.normalize()
    } else {
        // The sun on the X axis (sunrise, sunset: no shadows then).
        DVec3::Z
    };
    [u, light.cross(u), light]
}

/// The bounding sphere of the points inside a symmetric frustum whose distance from the eye is
/// in `[start, end]`, with `cos_corner` the cosine of the angle between its axis and its corner
/// rays: its center's distance along the axis and its radius. The farthest points from a center
/// on the axis are on the corner rays at `start` or `end`; the center balances the two, or sits
/// where the `end` ones are nearest.
fn slice_sphere(start: f64, end: f64, cos_corner: f64) -> (f64, f64) {
    let along = ((start + end) / (2.0 * cos_corner)).min(end * cos_corner);
    let reach = |d: f64| d * d + along * along - 2.0 * d * along * cos_corner;
    (along, reach(start).max(reach(end)).max(0.0).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_math::Quat;
    use boxel_shaders::gpu::REALM_ENVS;
    use boxel_voxel::MAX_BLOCK_COORD;
    use proptest::prelude::*;

    /// A sun of strength 0.4 everywhere toward `direction`, seen from realm 0.
    fn sun(direction: DVec3) -> SceneSun {
        SceneSun::new(direction.normalize(), 0.4, |_| true, 0)
    }

    /// The sun at 15:00 with the default 30 degree tilt.
    fn afternoon() -> SceneSun {
        let theta = 0.375 * std::f64::consts::TAU;
        let (s, c) = 30f64.to_radians().sin_cos();
        sun(DVec3::new(theta.cos(), theta.sin() * c, theta.sin() * s))
    }

    /// A camera at `origin + eye_rel` looking along `rotation`'s -Z, 70 degrees, 16:9.
    fn view(origin: I64Vec3, eye_rel: Vec3, rotation: Quat) -> ShadowView {
        ShadowView {
            render_origin: origin,
            eye_rel,
            forward: rotation * Vec3::NEG_Z,
            fov_y_radians: 70f32.to_radians(),
            aspect: 16.0 / 9.0,
            view_distance: 512.0,
            casters_top: 96.0,
        }
    }

    /// The map position of the camera-relative point `p`.
    fn project(c: &Cascade, p: Vec3) -> Vec3 {
        let q = c.view_proj * p.extend(1.0);
        q.truncate() / q.w
    }

    /// `p` is inside the map with room for the receivers' filter (`margin` in clip units).
    fn inside(c: &Cascade, p: Vec3, margin: f32) -> bool {
        let q = project(c, p);
        let lim = 1.0 - margin;
        q.x.abs() <= lim
            && q.y.abs() <= lim
            && (0.0..=1.0).contains(&q.z)
            && c.frustum
                .planes
                .iter()
                .all(|pl| pl.truncate().dot(p) + pl.w >= -1e-3)
    }

    /// Camera-relative points of the frustum at view distance `d`: the corner rays, the axis and
    /// points between, for a camera with forward `f` and up `up`.
    fn slice_points(v: &ShadowView, rotation: Quat, d: f32) -> Vec<Vec3> {
        let tan_y = (v.fov_y_radians * 0.5).tan();
        let tan_x = tan_y * v.aspect;
        let (right, up) = (rotation * Vec3::X, rotation * Vec3::Y);
        let mut points = Vec::new();
        for sx in [-1.0, -0.5, 0.0, 0.5, 1.0] {
            for sy in [-1.0, -0.3, 0.0, 0.7, 1.0] {
                let dir = (v.forward + right * (sx * tan_x) + up * (sy * tan_y)).normalize();
                points.push(v.eye_rel + dir * d);
            }
        }
        points
    }

    #[test]
    fn three_cascades_split_at_20_and_56_blocks() {
        let splits: Vec<_> = cascade_splits(3, 160.0).collect();
        assert_eq!(splits, [[0.0, 20.0], [18.0, 56.0], [50.4, 160.0]]);
        assert_eq!(cascade_splits(1, 100.0).collect::<Vec<_>>(), [[0.0, 100.0]]);
        assert_eq!(cascade_splits(9, 160.0).count(), MAX_CASCADES);
        assert_eq!(cascade_splits(0, 160.0).count(), 1);
        for n in 1..=MAX_CASCADES as u32 {
            let splits: Vec<_> = cascade_splits(n, 200.0).collect();
            assert_eq!(splits.last().unwrap()[1], 200.0);
            for pair in splits.windows(2) {
                // Each one starts inside the previous one, in its blend band.
                assert!(pair[1][0] < pair[0][1] && pair[1][0] > pair[0][0]);
                assert!((pair[1][0] - pair[0][1] * 0.9).abs() < 1e-3);
            }
        }
    }

    /// At 160 blocks and 2048 texels the texels are about 1/60, 1/20 and 1/8 block (plan sec.
    /// 3.1), the same whichever way the camera looks.
    #[test]
    fn texel_sizes_follow_the_plan() {
        let settings = SunShadowSettings::on();
        let v = view(I64Vec3::ZERO, Vec3::splat(0.5), Quat::IDENTITY);
        let c = shadow_cascades(&v, &afternoon(), &settings).unwrap();
        let texels: Vec<f32> = c.cascades().iter().map(|c| 1.0 / c.texel).collect();
        assert_eq!(texels.len(), 3);
        assert!((50.0..70.0).contains(&texels[0]), "{texels:?}");
        assert!((17.0..25.0).contains(&texels[1]), "{texels:?}");
        assert!((7.0..9.0).contains(&texels[2]), "{texels:?}");
        assert_eq!(c.distance, 160.0);
        // A near view distance ends the shadows there.
        let near = ShadowView {
            view_distance: 96.0,
            ..v
        };
        let c = shadow_cascades(&near, &afternoon(), &settings).unwrap();
        assert_eq!((c.distance, c.cascades()[2].end), (96.0, 96.0));
    }

    #[test]
    fn nothing_to_draw_without_a_shadow() {
        let settings = SunShadowSettings::on();
        let v = view(I64Vec3::ZERO, Vec3::ZERO, Quat::IDENTITY);
        let night = sun(DVec3::new(0.0, -1.0, 0.2));
        assert!(shadow_cascades(&v, &night, &settings).is_none());
        let sunless = SceneSun::new(DVec3::Y, 0.4, |realm| realm != 0, 0);
        assert!(shadow_cascades(&v, &sunless, &settings).is_none());
        let zero = SceneSun::new(DVec3::Y, 0.0, |_| true, 0);
        assert!(shadow_cascades(&v, &zero, &settings).is_none());
        let blind = ShadowView {
            view_distance: 0.0,
            ..v
        };
        assert!(shadow_cascades(&blind, &afternoon(), &settings).is_none());
        // The sun in a realm the camera is not in: no shadows here.
        let above = SceneSun::new(DVec3::Y, 0.4, |realm| realm == 1, 0);
        assert!(shadow_cascades(&v, &above, &settings).is_none());
        assert_eq!(REALM_ENVS, 9);
    }

    /// The buckets that cast look away from the sun; edge-on ones do not cast.
    #[test]
    fn only_faces_away_from_the_sun_cast() {
        // +Y 0, -Y 1, +X 2, -X 3, +Z 4, -Z 5.
        assert_eq!(bucket_mask(DVec3::Y), 1 << 1);
        assert_eq!(
            bucket_mask(DVec3::new(1.0, 1.0, 1.0).normalize()),
            0b10_1010
        );
        assert_eq!(
            bucket_mask(DVec3::new(-1.0, 1.0, -1.0).normalize()),
            0b01_0110
        );
        // Noon with the 30 degree tilt: the sun is edge-on to +X and -X.
        let (s, c) = 30f64.to_radians().sin_cos();
        let noon = DVec3::new((0.25 * std::f64::consts::TAU).cos(), c, s);
        assert_eq!(bucket_mask(noon), (1 << 1) | (1 << 5));
        let light = afternoon().direction;
        for (f, normal) in BUCKET_NORMALS.iter().enumerate() {
            let casts = bucket_mask(light) & (1 << f) != 0;
            assert_eq!(casts, normal.dot(light) < 0.0);
        }
    }

    /// The light basis is orthonormal and right-handed, and across the sun it keeps one axis all
    /// day.
    #[test]
    fn the_maps_do_not_turn_with_the_sun() {
        let (s, c) = 30f64.to_radians().sin_cos();
        let mut first = None;
        for step in 1..360 {
            let theta = f64::from(step) / 720.0 * std::f64::consts::TAU;
            let light = DVec3::new(theta.cos(), theta.sin() * c, theta.sin() * s);
            let [u, v, w] = light_basis(light);
            assert!((u.cross(v) - w).length() < 1e-12);
            assert!(u.dot(v).abs() < 1e-12 && u.dot(w).abs() < 1e-12);
            let first = *first.get_or_insert(u);
            assert!((u - first).length() < 1e-12, "{step}");
        }
        let [u, v, _] = light_basis(DVec3::X);
        assert!(u.dot(DVec3::X).abs() < 1e-12 && v.dot(DVec3::X).abs() < 1e-12);
    }

    /// The cascades take microseconds a frame (plan, lane S2: a unit timing, no bench).
    #[test]
    fn cascades_take_microseconds() {
        let settings = SunShadowSettings {
            cascades: MAX_CASCADES as u32,
            ..SunShadowSettings::on()
        };
        let sun = afternoon();
        let rounds = 2000;
        let start = std::time::Instant::now();
        let mut sum = 0.0;
        for i in 0..rounds {
            let eye = Vec3::new(i as f32 * 1e-3, 0.5, 0.25);
            let v = view(
                I64Vec3::new(i, 64, -i),
                eye,
                Quat::from_rotation_y(i as f32),
            );
            let c = shadow_cascades(std::hint::black_box(&v), &sun, &settings).unwrap();
            sum += c.cascades()[0].texel;
        }
        let each = start.elapsed() / rounds as u32;
        assert!(sum > 0.0);
        // About a microsecond in a release build; generous for a debug build under load.
        assert!(each.as_micros() < 200, "{each:?} per frame");
    }

    fn rotation() -> impl Strategy<Value = Quat> {
        (-3.2f32..3.2, -1.55f32..1.55, -3.2f32..3.2).prop_map(|(yaw, pitch, roll)| {
            Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch) * Quat::from_rotation_z(roll)
        })
    }

    fn far_origin() -> impl Strategy<Value = I64Vec3> {
        let axis = prop_oneof![
            -4096i64..4096,
            Just((1i64 << 31) - 700),
            Just(-(1i64 << 31) + 300),
            Just((1i64 << 40) + 12_345),
            Just(-(1i64 << 35) - 999),
            Just((MAX_BLOCK_COORD & !(SHADOW_ANCHOR - 1)) - 5000),
            Just(-(MAX_BLOCK_COORD & !(SHADOW_ANCHOR - 1)) + 77),
        ];
        (axis.clone(), axis.clone(), axis).prop_map(|(x, y, z)| I64Vec3::new(x, y, z))
    }

    /// A sun above the horizon, from just over it to straight up.
    fn day_sun() -> impl Strategy<Value = SceneSun> {
        (0.05f64..std::f64::consts::PI - 0.05, 0.0f64..60.0).prop_map(|(theta, tilt)| {
            let (s, c) = tilt.to_radians().sin_cos();
            sun(DVec3::new(theta.cos(), theta.sin() * c, theta.sin() * s))
        })
    }

    fn eye_rel() -> impl Strategy<Value = Vec3> {
        (0.0f32..1.0, 0.0f32..1.0, 0.0f32..1.0).prop_map(|(x, y, z)| Vec3::new(x, y, z))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Every point of a cascade's slice of the frustum is in its map with room for the
        /// filter, for any orientation, field of view, aspect, sun and place; and so is every
        /// caster over it, up to the casters' top.
        #[test]
        fn a_cascade_holds_its_slice(
            rotation in rotation(),
            origin in far_origin(),
            eye in eye_rel(),
            fov in 30f32..110.0,
            aspect in 0.5f32..3.0,
            sun in day_sun(),
            cascades in 1u32..=MAX_CASCADES as u32,
            distance in 16f32..400.0,
        ) {
            let settings = SunShadowSettings { cascades, distance, ..SunShadowSettings::on() };
            let v = ShadowView {
                fov_y_radians: fov.to_radians(),
                aspect,
                ..view(origin, eye, rotation)
            };
            let c = shadow_cascades(&v, &sun, &settings).unwrap();
            prop_assert_eq!(c.cascades().len(), cascades as usize);
            let light = sun.direction.as_vec3();
            for cascade in c.cascades() {
                // Two texels of room: the filter and the receivers' offset.
                let margin = 2.0 * 2.0 / c.resolution as f32;
                for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                    let d = cascade.start + (cascade.end - cascade.start) * t;
                    for p in slice_points(&v, rotation, d) {
                        prop_assert!(inside(cascade, p, margin), "{p} at {d}: {cascade:?}");
                        // The casters above it, while the box reaches that far.
                        let top = v.casters_top.max(p.y);
                        let s = (top - p.y) / light.y;
                        if s < MAX_EXTRUSION as f32 {
                            let q = p + light * s;
                            prop_assert!(inside(cascade, q, margin), "{q} over {p}");
                        }
                    }
                }
            }
        }

        /// The size of a cascade never changes as the camera turns or moves.
        #[test]
        fn turning_keeps_the_size(
            a in rotation(),
            b in rotation(),
            origin in far_origin(),
            eye in eye_rel(),
            sun in day_sun(),
        ) {
            let settings = SunShadowSettings::on();
            let one = shadow_cascades(&view(origin, eye, a), &sun, &settings).unwrap();
            let two = shadow_cascades(&view(origin + 77, Vec3::ZERO, b), &sun, &settings).unwrap();
            for (x, y) in one.cascades().iter().zip(two.cascades()) {
                prop_assert_eq!(x.texel.to_bits(), y.texel.to_bits());
                prop_assert_eq!(x.radius.to_bits(), y.radius.to_bits());
            }
        }

        /// As the camera moves (within one anchor cell), a world point keeps its place inside
        /// its texel: the boxes move by whole texels only.
        #[test]
        fn moving_moves_the_boxes_by_whole_texels(
            rotation in rotation(),
            cell in far_origin(),
            start in (20i64..1000, 20i64..1000, 20i64..1000),
            step in (-20i64..20, -20i64..20, -20i64..20),
            eyes in (eye_rel(), eye_rel()),
            sun in day_sun(),
        ) {
            let settings = SunShadowSettings::on();
            let anchor = cell - cell.rem_euclid(I64Vec3::splat(SHADOW_ANCHOR));
            let a = anchor + I64Vec3::new(start.0, start.1, start.2);
            let b = a + I64Vec3::new(step.0, step.1, step.2);
            let one = shadow_cascades(&view(a, eyes.0, rotation), &sun, &settings).unwrap();
            let two = shadow_cascades(&view(b, eyes.1, rotation), &sun, &settings).unwrap();
            prop_assert_eq!(one.anchor, two.anchor);
            let half = settings.resolution as f32 * 0.5;
            // A world point near both cameras, camera-relative to each.
            let point = a + I64Vec3::new(3, -2, 5);
            let rel = |origin: I64Vec3| (point - origin).as_vec3() + Vec3::splat(0.375);
            for (x, y) in one.cascades().iter().zip(two.cascades()) {
                let p = project(x, rel(a));
                let q = project(y, rel(b));
                for (pa, qa) in [(p.x, q.x), (p.y, q.y)] {
                    let texels = (pa - qa) * half;
                    prop_assert!((texels - texels.round()).abs() < 0.02, "{texels} texels");
                }
                // The same depth range when the box did not move along the sun.
                if x.key[2..] == y.key[2..] {
                    prop_assert!((p.z - q.z).abs() < 1e-4);
                }
            }
        }

        /// Frames whose render origins differ by a multiple of the anchor get the same
        /// cascades, bit for bit, up to the world's limit.
        #[test]
        fn far_frames_get_the_same_cascades(
            rotation in rotation(),
            near in (-2048i64..2048, -2048i64..2048, -2048i64..2048),
            eye in eye_rel(),
            sun in day_sun(),
            shift in prop_oneof![
                Just(I64Vec3::new(1 << 31, 0, -(1 << 31))),
                Just(I64Vec3::new(-(1 << 31), 1 << 31, 1 << 40)),
                Just(I64Vec3::new(1 << 40, -(1 << 35), 1 << 35)),
                Just(I64Vec3::splat(MAX_BLOCK_COORD & !(SHADOW_ANCHOR - 1))),
                Just(I64Vec3::splat(-(MAX_BLOCK_COORD & !(SHADOW_ANCHOR - 1)))),
            ],
        ) {
            let settings = SunShadowSettings::on();
            let origin = I64Vec3::new(near.0, near.1, near.2);
            let here = shadow_cascades(&view(origin, eye, rotation), &sun, &settings).unwrap();
            let there =
                shadow_cascades(&view(origin + shift, eye, rotation), &sun, &settings).unwrap();
            prop_assert_eq!(there.anchor - here.anchor, shift);
            prop_assert_eq!(
                ShadowCascades { anchor: I64Vec3::ZERO, ..here },
                ShadowCascades { anchor: I64Vec3::ZERO, ..there }
            );
        }
    }
}
