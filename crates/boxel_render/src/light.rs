//! The scene light of a frame (ADR 0015): what the voxel shaders multiply each mesh's baked
//! corner light by. It is one uniform, so changing it (day and night, a brightness setting)
//! never remeshes or uploads anything.

/// Light levels per channel (`0..=15`).
pub const LIGHT_LEVELS: usize = 16;

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
        }
    }

    /// The uniform words: `sky_light` (rgb, ambient), `light_params` (AO strength, block gain)
    /// and `light_curve` (4 levels per vec4).
    pub(crate) fn uniform(&self) -> ([f32; 4], [f32; 4], [[f32; 4]; 4]) {
        let [r, g, b] = self.sky;
        let c = self.curve;
        (
            [r, g, b, self.ambient],
            [self.ao_strength, self.block_gain, 0.0, 0.0],
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
}
