//! The scene light of a frame (ADR 0015): what the voxel shaders multiply each mesh's baked
//! corner light by. It is one uniform, so changing it (day and night, a brightness setting)
//! never remeshes or uploads anything.
//!
//! Each realm of the dimension (ADR 0039) has its own entry ([`SceneRealms`]): sky light,
//! ambient floor and face shade. The shaders find a voxel's realm from the planes in the same
//! uniform, so a realm costs no per-chunk data.

use crate::{DynamicLights, SceneSun};
use boxel_shaders::gpu::{REALM_ENVS, REALM_PLANES, RealmEnvGpu};
use boxel_world::DEFAULT_FACE_SHADE;

// The uniform holds every realm a dimension can have.
const _: () = assert!(REALM_ENVS == boxel_world::MAX_REALMS);

/// Light levels per channel (`0..=15`).
pub const LIGHT_LEVELS: usize = 16;

/// How one realm's voxels are lit this frame. [`RealmShading::default`] takes the
/// [`SceneLight`]'s sky light and ambient floor and the default face shade: the look of a world
/// without realms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RealmShading {
    /// Sky light color times the daylight intensity (linear RGB); `None`: [`SceneLight::sky`].
    pub sky: Option<[f32; 3]>,
    /// Ambient floor (linear RGB); `None`: [`SceneLight::ambient`] on every channel.
    pub ambient: Option<[f32; 3]>,
    /// Face shade in the mesher's face order (+Y, -Y, +X, -X, +Z, -Z):
    /// [`boxel_world::DEFAULT_FACE_SHADE`] by default.
    pub face_shade: [f32; 6],
}

impl Default for RealmShading {
    fn default() -> Self {
        Self {
            sky: None,
            ambient: None,
            face_shade: DEFAULT_FACE_SHADE,
        }
    }
}

/// The realms of a frame (ADR 0039 sec. 2): where they meet and how each is lit. The default
/// is one realm lit by the [`SceneLight`] itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneRealms {
    /// The planes between the realms (world `y`), ascending; unused ones `i64::MAX`. A voxel at
    /// `y` is in realm [`SceneRealms::realm_of`].
    pub planes: [i64; REALM_PLANES],
    /// The realm the far LOD draws (below [`REALM_ENVS`]).
    pub drawn: u8,
    /// Each realm's light, bottom realm first.
    pub realms: [RealmShading; REALM_ENVS],
}

impl Default for SceneRealms {
    fn default() -> Self {
        Self {
            planes: [i64::MAX; REALM_PLANES],
            drawn: 0,
            realms: [RealmShading::default(); REALM_ENVS],
        }
    }
}

impl SceneRealms {
    /// One realm per plane plus one, met at `planes` (ascending; at most [`REALM_PLANES`], the
    /// rest are dropped), each lit by default.
    pub fn with_planes(planes: &[i64]) -> Self {
        let mut realms = Self::default();
        for (slot, &plane) in realms.planes.iter_mut().zip(planes) {
            *slot = plane;
        }
        realms
    }

    /// The realm of the voxels at `y`: the number of planes at or under it (the shaders'
    /// `realm_at`, `boxel_worldgen::RealmStack::realm_of`).
    pub fn realm_of(&self, y: i64) -> usize {
        self.planes.partition_point(|&plane| plane <= y)
    }

    /// The uniform words: the planes relative to `origin_y` (the render origin's y), clamped to
    /// the `i32` range (exact for the realm of every voxel less than `2^31` from it), and each
    /// realm's entry, its `None`s filled from `light`.
    pub(crate) fn uniform(
        &self,
        light: &SceneLight,
        origin_y: i64,
    ) -> ([[i32; 4]; 2], [RealmEnvGpu; REALM_ENVS]) {
        let rel = |plane: i64| {
            plane
                .saturating_sub(origin_y)
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        };
        let planes = std::array::from_fn(|k| std::array::from_fn(|j| rel(self.planes[4 * k + j])));
        let env = self.realms.map(|r| {
            let [sr, sg, sb] = r.sky.unwrap_or(light.sky);
            let [ar, ag, ab] = r.ambient.unwrap_or([light.ambient; 3]);
            let [top, bottom, px, nx, pz, nz] = r.face_shade;
            RealmEnvGpu {
                sky: [sr, sg, sb, top],
                ambient: [ar, ag, ab, bottom],
                shade: [px, nx, pz, nz],
            }
        });
        (planes, env)
    }
}

/// The scene light of one frame. The fragment shader computes, per pixel,
/// `max(curve(sky) * sky, curve(rgb) * block_gain, ambient)` from the corner lights it
/// interpolates, and multiplies the albedo, the face shade and the AO factor
/// `mix(1 - ao_strength, 1, ao / 3)` by it.
///
/// [`SceneLight::default`] is a white noon sky: a voxel under open sky with AO open looks
/// exactly as it did before light existed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneLight {
    /// Sky light color times the daylight intensity (linear RGB): white 1.0 at noon.
    pub sky: [f32; 3],
    /// Ambient floor: no surface is darker than its albedo (times face shade and AO) times this.
    /// [`SceneLight::DEFAULT_AMBIENT`] keeps caves almost black.
    pub ambient: f32,
    /// What the most occluded corner loses: 0.55 keeps 45 % (the look before light).
    pub ao_strength: f32,
    /// Multiplier of block light (lamps, lava).
    pub block_gain: f32,
    /// Brightness of each light level, level 15 = 1 ([`light_curve`]). The shader interpolates
    /// between levels.
    pub curve: [f32; LIGHT_LEVELS],
    /// The realms (ADR 0039): where they meet and each one's sky light, ambient floor and face
    /// shade. [`SceneRealms::default`]: one realm, lit by `sky` and `ambient`.
    pub realms: SceneRealms,
    /// The dynamic point lights of the frame (ADR 0062): block light that moves, added in the
    /// shaders by the brightest per channel. Empty by default. The plugin leaves out the ones the
    /// world bakes into its meshes (ADR 0063).
    pub dynamic: DynamicLights,
    /// The sun for sun shadows (High only): where it is and each realm's shadow strength.
    /// `None` (the default) draws none; the plugin fills it while
    /// [`crate::RenderSettings::sun_shadows`] is on. It never reaches the `View` uniform.
    pub sun: Option<SceneSun>,
}

impl SceneLight {
    /// Ambient floor of the default look (caves almost black, not fully).
    pub const DEFAULT_AMBIENT: f32 = 0.02;
    /// AO strength of the default look (the former `mix(0.45, 1, ao)`).
    pub const DEFAULT_AO_STRENGTH: f32 = 0.55;
    /// Default gamma of [`light_curve`] (Minecraft's "moody").
    pub const DEFAULT_GAMMA: f32 = 0.0;

    /// A white noon sky with the default ambient, AO strength, block gain and curve.
    pub fn noon() -> Self {
        Self {
            sky: [1.0; 3],
            ambient: Self::DEFAULT_AMBIENT,
            ao_strength: Self::DEFAULT_AO_STRENGTH,
            block_gain: 1.0,
            curve: light_curve(Self::DEFAULT_GAMMA),
            realms: SceneRealms::default(),
            dynamic: DynamicLights::default(),
            sun: None,
        }
    }

    /// The uniform words: `sky_light` (rgb, ambient), `light_params` (AO strength, block gain,
    /// 0, the drawn realm) and `light_curve` (4 levels per vec4).
    pub(crate) fn uniform(&self) -> ([f32; 4], [f32; 4], [[f32; 4]; 4]) {
        let [r, g, b] = self.sky;
        let c = self.curve;
        let drawn = self.realms.drawn.min(REALM_ENVS as u8 - 1);
        (
            [r, g, b, self.ambient],
            [self.ao_strength, self.block_gain, 0.0, f32::from(drawn)],
            std::array::from_fn(|k| std::array::from_fn(|j| c[4 * k + j])),
        )
    }
}

impl Default for SceneLight {
    /// [`SceneLight::noon`].
    fn default() -> Self {
        Self::noon()
    }
}

/// Minecraft-like brightness of the light levels: `b = f / (4 - 3f)` with `f = level / 15`,
/// moved toward `1 - (1 - b)^4` by `gamma` (0 = dark and moody, 1 = bright; clamped to 0..=1).
/// Level 0 is 0 and level 15 is 1 at every gamma, and the curve always rises.
pub fn light_curve(gamma: f32) -> [f32; LIGHT_LEVELS] {
    let gamma = gamma.clamp(0.0, 1.0);
    std::array::from_fn(|level| {
        let f = level as f32 / 15.0;
        let b = f / (4.0 - 3.0 * f);
        let bright = 1.0 - (1.0 - b).powi(4);
        b + (bright - b) * gamma
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_ends_at_0_and_1_and_rises() {
        for gamma in [0.0, 0.25, 0.5, 1.0, -3.0, 7.0] {
            let c = light_curve(gamma);
            assert_eq!((c[0], c[15]), (0.0, 1.0), "gamma {gamma}");
            assert!(c.windows(2).all(|w| w[0] < w[1]), "gamma {gamma}: {c:?}");
        }
        // Minecraft's moody curve: level 7 is about 18 % and level 14 about 78 %.
        let moody = light_curve(0.0);
        assert!((moody[7] - 0.179_5).abs() < 1e-3, "{}", moody[7]);
        assert!((moody[14] - 0.777_8).abs() < 1e-3, "{}", moody[14]);
        // A brighter gamma lifts every middle level.
        let bright = light_curve(1.0);
        assert!((1..15).all(|l| bright[l] > moody[l]));
    }

    #[test]
    fn noon_uniform_packs_four_levels_per_vec4() {
        let light = SceneLight::noon();
        let (sky, params, curve) = light.uniform();
        assert_eq!(sky, [1.0, 1.0, 1.0, SceneLight::DEFAULT_AMBIENT]);
        assert_eq!(params, [SceneLight::DEFAULT_AO_STRENGTH, 1.0, 0.0, 0.0]);
        assert_eq!(curve[3][3], 1.0);
        assert_eq!(curve[1][2], light.curve[6]);
        assert_eq!(SceneLight::default(), light);
    }

    /// The default realm entry is today's look bit for bit: the scene's sky light and ambient
    /// (on every channel) and the face shades `common.wgsl` had as literals (1.0, 0.5, 0.8,
    /// 0.65), in every entry; every plane unused (`i32::MAX`), so every voxel is in realm 0.
    #[test]
    fn the_default_realm_entry_is_todays_light() {
        let light = SceneLight {
            sky: [0.25, 0.375, 0.6],
            ambient: 0.02,
            ..SceneLight::noon()
        };
        let (sky_light, ..) = light.uniform();
        let (planes, env) = light.realms.uniform(&light, -(1 << 40));
        assert_eq!(planes, [[i32::MAX; 4]; 2]);
        let bits = |v: [f32; 4]| v.map(f32::to_bits);
        for e in env {
            assert_eq!(
                bits([e.sky[0], e.sky[1], e.sky[2], sky_light[3]]),
                bits(sky_light)
            );
            assert_eq!(
                bits([e.ambient[0], e.ambient[1], e.ambient[2], 0.0])[..3],
                [0.02_f32.to_bits(); 3]
            );
            assert_eq!(
                bits([e.sky[3], e.ambient[3], e.shade[0], e.shade[1]]),
                bits([1.0, 0.5, 0.8, 0.8])
            );
            assert_eq!(
                bits([e.shade[2], e.shade[3], 0.0, 0.0])[..2],
                [0.65_f32.to_bits(); 2]
            );
        }
        // The drawn realm rides in light_params.w.
        assert_eq!(light.uniform().1[3], 0.0);
    }

    /// A realm's own sky, ambient color and face shade reach its entry, per channel and per
    /// face; the others keep the scene's. Planes are relative to the render origin, clamped to
    /// `i32` (a far plane keeps its side of every voxel near the camera).
    #[test]
    fn a_realms_light_reaches_the_uniform() {
        let mut light = SceneLight::noon();
        light.realms = SceneRealms::with_planes(&[-(1 << 40), 4096, 1 << 40]);
        light.realms.drawn = 2;
        light.realms.realms[2] = RealmShading {
            sky: Some([0.5, 0.25, 0.125]),
            ambient: Some([0.1, 0.06, 0.03]),
            face_shade: [0.9, 0.85, 0.8, 0.75, 0.7, 0.65],
        };
        let (planes, env) = light.realms.uniform(&light, 4000);
        assert_eq!(planes, [[i32::MIN, 96, i32::MAX, i32::MAX], [i32::MAX; 4]]);
        assert_eq!(env[2].sky, [0.5, 0.25, 0.125, 0.9]);
        assert_eq!(env[2].ambient, [0.1, 0.06, 0.03, 0.85]);
        assert_eq!(env[2].shade, [0.8, 0.75, 0.7, 0.65]);
        assert_eq!(env[1].sky, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(env[1].ambient, [0.02, 0.02, 0.02, 0.5]);
        assert_eq!(light.uniform().1[3], 2.0);
        // The CPU realm of a voxel agrees with the shader's count over the relative planes.
        for y in [-(1 << 41), -(1 << 40), 0, 4095, 4096, 1 << 40] {
            let rel = (y - 4000_i64).clamp(i64::from(i32::MIN) + 1, i64::from(i32::MAX) - 1);
            let shader = planes
                .as_flattened()
                .iter()
                .filter(|&&p| i64::from(p) <= rel)
                .count();
            if (y - 4000).abs() < 1 << 31 {
                assert_eq!(shader, light.realms.realm_of(y), "{y}");
            }
        }
    }
}
