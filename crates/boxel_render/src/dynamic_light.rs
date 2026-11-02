//! Dynamic point lights (ADR 0062): light that moves with an entity (a torch in the hand) and is
//! not a block. The fragment shaders add it to the baked block light, so a light that moves
//! never relights or remeshes a chunk.
//!
//! A light has a level per block light channel (`0..=15`, as a block's emission) and gives
//! `level - d` at Manhattan distance `d`, as a block emitter's light falls off in open air; it
//! mixes with the baked light by the brightest per channel, as block light does. So a torch in
//! the hand lights the faces around it as the same torch placed there would, but through walls
//! as far as they face it (no flood fill on the GPU).
//!
//! [`DynamicLight`] on an entity with a `Transform` makes it shine; the render plugin keeps the
//! [`MAX_DYNAMIC_LIGHTS`] nearest the camera each frame ([`SceneLight::dynamic`]). Models lit at
//! a probe (ADR 0040) take it on the CPU, at their probe.
//!
//! [`SceneLight::dynamic`]: crate::SceneLight::dynamic

use crate::plugin::ExtractedFrame;
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec3};
use boxel_shaders::gpu::DynamicLightGpu;
use boxel_transform::Transform;
use boxel_voxel::LightValue;

/// Dynamic lights a frame draws (`boxel_shaders::gpu::MAX_DYNAMIC_LIGHTS`): the nearest the
/// camera.
pub const MAX_DYNAMIC_LIGHTS: usize = boxel_shaders::gpu::MAX_DYNAMIC_LIGHTS;

/// Light levels per channel go up to this (a block's emission).
const MAX_LEVEL: u8 = 15;

/// Makes its entity a moving light (ADR 0062): it shines from the entity's `Transform`
/// translation plus `offset`, with `emission` per block light channel. An emission of 0 on every
/// channel shines nothing (and costs nothing), so a game can keep the component and change it.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
#[require(Transform)]
pub struct DynamicLight {
    /// Level per block light channel (red, green, blue), `0..=15` (more is 15): a block's
    /// emission (a torch is `[14, 11, 6]` in the game's colors).
    pub emission: [u8; 3],
    /// Where it shines from, relative to the translation (not turned with the entity): the eye
    /// of a player whose translation is its feet.
    pub offset: Vec3,
}

/// One dynamic light of a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointLight {
    /// Where it is (world blocks).
    pub position: DVec3,
    /// Level per block light channel (red, green, blue), `0..=15`.
    pub emission: [u8; 3],
}

impl PointLight {
    /// Its block light levels at `at`: `emission - d` per channel, `d` the Manhattan distance,
    /// at least 0 (the shaders' falloff, without a face).
    pub fn levels_at(&self, at: DVec3) -> [f32; 3] {
        let d = (at - self.position).abs();
        let d = (d.x + d.y + d.z) as f32;
        self.emission.map(|e| (f32::from(e) - d).max(0.0))
    }
}

/// The dynamic lights of a frame (ADR 0062), at most [`MAX_DYNAMIC_LIGHTS`]. Empty by default:
/// the frame draws exactly what it drew before dynamic lights.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DynamicLights {
    lights: [PointLight; MAX_DYNAMIC_LIGHTS],
    len: usize,
}

impl DynamicLights {
    /// Adds `light` (its emission clamped to 15) and returns true, or returns false when it is
    /// full. A light dark on every channel is not kept (true: nothing to draw).
    pub fn push(&mut self, light: PointLight) -> bool {
        let emission = light.emission.map(|e| e.min(MAX_LEVEL));
        if emission == [0; 3] || !light.position.is_finite() {
            return true;
        }
        let Some(slot) = self.lights.get_mut(self.len) else {
            return false;
        };
        *slot = PointLight { emission, ..light };
        self.len += 1;
        true
    }

    /// The lights, in the order pushed.
    pub fn as_slice(&self) -> &[PointLight] {
        &self.lights[..self.len]
    }

    /// Lights kept.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True without lights.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Drops every light.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The block light levels the lights give at `at`, the brightest per channel.
    pub fn levels_at(&self, at: DVec3) -> [f32; 3] {
        self.as_slice().iter().fold([0.0; 3], |acc, l| {
            let levels = l.levels_at(at);
            std::array::from_fn(|c| acc[c].max(levels[c]))
        })
    }

    /// `light` with its block channels raised to the lights' levels at `at`, rounded to the
    /// nearest level: how a model lit at a probe takes them (its light is one level per
    /// instance, as the baked light it reads).
    pub fn lit(&self, light: LightValue, at: DVec3) -> LightValue {
        if self.is_empty() || !at.is_finite() {
            return light;
        }
        let levels = self.levels_at(at);
        let baked = light.rgb();
        let [r, g, b] = std::array::from_fn(|c| {
            let level = (levels[c] + 0.5).floor().clamp(0.0, 15.0) as u8;
            baked[c].max(level)
        });
        LightValue::new(light.sky(), r, g, b)
    }

    /// The uniform words: the count and each light relative to `origin` (the render origin;
    /// exact to `f32` for lights near it), its reach its brightest level.
    pub(crate) fn uniform(
        &self,
        origin: I64Vec3,
    ) -> ([u32; 4], [DynamicLightGpu; MAX_DYNAMIC_LIGHTS]) {
        let origin = origin.as_dvec3();
        let mut gpu = [DynamicLightGpu::default(); MAX_DYNAMIC_LIGHTS];
        for (g, l) in gpu.iter_mut().zip(self.as_slice()) {
            let rel = (l.position - origin).as_vec3();
            let [r, gr, b] = l.emission.map(f32::from);
            *g = DynamicLightGpu {
                pos: [rel.x, rel.y, rel.z, r.max(gr).max(b)],
                level: [r, gr, b, 0.0],
            };
        }
        ([self.len as u32, 0, 0, 0], gpu)
    }
}

/// The [`MAX_DYNAMIC_LIGHTS`] of `lights` nearest `eye` (all of them if fewer), nearest first,
/// ties by entity; without an eye, the first by entity. Dark lights are dropped first.
pub(crate) fn nearest(
    lights: impl IntoIterator<Item = (u64, PointLight)>,
    eye: Option<DVec3>,
) -> DynamicLights {
    let mut all: Vec<(f64, u64, PointLight)> = lights
        .into_iter()
        .filter(|(_, l)| l.emission.iter().any(|&e| e > 0) && l.position.is_finite())
        .map(|(id, l)| (eye.map_or(0.0, |e| e.distance_squared(l.position)), id, l))
        .collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut out = DynamicLights::default();
    for (_, _, light) in all {
        if !out.push(light) {
            break;
        }
    }
    out
}

/// `RenderSystems::Extract`, after `extract` (which found the camera and wrote the frame's
/// scene light) and before the models' extract (which lights the models with them): the
/// frame's dynamic lights.
pub(crate) fn extract_dynamic_lights(
    mut frame: ResMut<ExtractedFrame>,
    lights: Query<(Entity, &DynamicLight, &Transform)>,
) {
    let eye = frame.camera_eye();
    frame.light.dynamic = nearest(
        lights.iter().map(|(entity, light, transform)| {
            (
                entity.to_bits(),
                PointLight {
                    position: transform.translation + light.offset.as_dvec3(),
                    emission: light.emission,
                },
            )
        }),
        eye,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(x: f64, emission: [u8; 3]) -> PointLight {
        PointLight {
            position: DVec3::new(x, 0.0, 0.0),
            emission,
        }
    }

    /// The uniform holds the count and each light relative to the render origin, its levels
    /// as floats and its reach its brightest channel; the rest of the array is zero. Far from
    /// the world origin the relative position is exact.
    #[test]
    fn the_uniform_packs_relative_positions_levels_and_reach() {
        let far = 1_i64 << 40;
        let mut lights = DynamicLights::default();
        assert!(lights.push(PointLight {
            position: DVec3::new(far as f64 + 2.5, -3.25, far as f64 - 7.75),
            emission: [14, 11, 6],
        }));
        assert!(lights.push(PointLight {
            position: DVec3::new(far as f64, 1.0, far as f64),
            emission: [3, 6, 20],
        }));
        let (count, gpu) = lights.uniform(I64Vec3::new(far, 0, far));
        assert_eq!(count, [2, 0, 0, 0]);
        assert_eq!(gpu[0].pos, [2.5, -3.25, -7.75, 14.0]);
        assert_eq!(gpu[0].level, [14.0, 11.0, 6.0, 0.0]);
        // Emission clamps to 15, and the reach follows it.
        assert_eq!(gpu[1].pos, [0.0, 1.0, 0.0, 15.0]);
        assert_eq!(gpu[1].level, [3.0, 6.0, 15.0, 0.0]);
        assert!(gpu[2..].iter().all(|g| *g == DynamicLightGpu::default()));
        // No lights: count 0 and an all-zero array, the uniform of a frame before dynamic lights.
        let (count, gpu) = DynamicLights::default().uniform(I64Vec3::ZERO);
        assert_eq!(count, [0; 4]);
        assert!(gpu.iter().all(|g| *g == DynamicLightGpu::default()));
    }

    /// Dark or non-finite lights are not kept; the 17th light is refused.
    #[test]
    fn push_keeps_lit_lights_up_to_the_capacity() {
        let mut lights = DynamicLights::default();
        assert!(lights.push(light(0.0, [0; 3])));
        assert!(lights.push(PointLight {
            position: DVec3::NAN,
            emission: [15; 3],
        }));
        assert!(lights.is_empty());
        for k in 0..MAX_DYNAMIC_LIGHTS {
            assert!(lights.push(light(k as f64, [1, 0, 0])));
        }
        assert!(!lights.push(light(99.0, [1, 0, 0])));
        assert_eq!(lights.len(), MAX_DYNAMIC_LIGHTS);
        lights.clear();
        assert_eq!(lights, DynamicLights::default());
    }

    /// The nearest lights to the eye are kept, nearest first, ties by entity.
    #[test]
    fn the_nearest_lights_are_kept() {
        let all = (0..40_u64).map(|id| (id, light(100.0 - id as f64, [10, 10, 10])));
        let kept = nearest(all, Some(DVec3::ZERO));
        assert_eq!(kept.len(), MAX_DYNAMIC_LIGHTS);
        let xs: Vec<f64> = kept.as_slice().iter().map(|l| l.position.x).collect();
        assert_eq!(xs, (0..16).map(|k| 61.0 + f64::from(k)).collect::<Vec<_>>());
        // Ties: by entity. Dark lights first leave.
        let ties = [
            (7, light(1.0, [1, 2, 3])),
            (3, light(-1.0, [4, 5, 6])),
            (1, light(0.5, [0; 3])),
        ];
        let kept = nearest(ties, Some(DVec3::ZERO));
        assert_eq!(
            kept.as_slice(),
            [light(-1.0, [4, 5, 6]), light(1.0, [1, 2, 3])]
        );
    }

    /// Falloff: level minus the Manhattan distance, the brightest per channel; a model takes it
    /// rounded to the nearest level, over its baked light by channel, its sky untouched.
    #[test]
    fn levels_fall_off_by_manhattan_distance_and_mix_by_max() {
        let mut lights = DynamicLights::default();
        lights.push(light(0.0, [14, 11, 6]));
        lights.push(PointLight {
            position: DVec3::new(0.0, 0.0, 4.0),
            emission: [0, 0, 15],
        });
        // 3 blocks from the first, 7 from the second.
        assert_eq!(
            lights.levels_at(DVec3::new(1.0, 2.0, 0.0)),
            [11.0, 8.0, 8.0]
        );
        assert_eq!(lights.levels_at(DVec3::new(0.0, 0.0, 40.0)), [0.0; 3]);
        let baked = LightValue::new(9, 12, 0, 0);
        let lit = lights.lit(baked, DVec3::new(0.25, 0.25, 0.0));
        // 13.5, 10.5 and 5.5 from the first, blue 10.5 from the second: rounded up.
        assert_eq!(
            (lit.sky(), lit.red(), lit.green(), lit.blue()),
            (9, 14, 11, 11)
        );
        assert_eq!(DynamicLights::default().lit(baked, DVec3::ZERO), baked);
    }
}
