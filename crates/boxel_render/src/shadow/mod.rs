//! Sun shadows on the High tier (sun shadows plan, ADR 0065): the settings, the frame's sun
//! and the cascades the shadow maps are drawn into.
//!
//! Shadows only darken the sky term of the light, by at most a strength `k` per realm
//! ([`SceneSun::strength`]), so every way they can fail gives today's image. Mid and Low never
//! draw them.
//!
//! - [`SunShadowSettings`]: on or off, the strength, the distance, the cascades and their size
//!   ([`crate::RenderSettings::sun_shadows`]).
//! - [`SceneSun`]: the sun of one frame, filled by the render extract from
//!   `boxel_world::SunPath` and the world clock ([`crate::SceneLight::sun`]).
//! - [`shadow_cascades`]: where the cascades of a frame are, camera-relative and snapped to their
//!   texel grid.
//! - The maps themselves (lane S3): on High, while shadows are on, a caster cull and one
//!   depth pass per cascade before the main pass ([`ShadowStats`]).
//! - The receivers (lane S4): in a frame that drew its maps, High's opaque, cutout and
//!   translucent voxels are drawn with the shadowed pipelines (`sun_shadow.wgsl`), which scale
//!   the sky term by `1 - k * (1 - v)`. Every other frame, and Mid and Low always, draw with
//!   today's pipelines.
//! - Entity models and cell quads (lane S5) cast into the same maps (their faces that look away
//!   from the sun; the cells' cutout quads only) and receive them like the terrain: the models'
//!   normal comes from the screen derivatives of their position (`sun_factor_clip`).

mod cascades;
mod pass;

pub use cascades::{
    CASCADE_BLEND, Cascade, MAX_CASCADES, SHADOW_ANCHOR, ShadowCascades, ShadowView, bucket_mask,
    cascade_splits, shadow_cascades,
};
pub(crate) use pass::{
    CascadeTarget, ShadowFrame, ShadowPass, VoxelGroups, caster_depth, read_depth_layer,
    receive_layout,
};
pub use pass::{PAGE_CASTERS, ShadowStats};

const _: () = assert!(MAX_CASCADES == boxel_shaders::gpu::shadow::MAX_CASCADES);
const _: () = assert!(CASCADE_BLEND == boxel_shaders::gpu::shadow::RECEIVE_BLEND);

use boxel_gfx::Tier;
use boxel_math::DVec3;
use boxel_shaders::gpu::REALM_ENVS;

/// The sun elevation (sine) from which shadows have their full strength: about 11.5 degrees.
/// Below it they fade to none at the horizon, so long sunset streaks fade out ([`sun_fade`]).
pub const FADE_ELEVATION: f64 = 0.2;

/// How the sun casts shadows on High ([`crate::RenderSettings::sun_shadows`]). Off by default:
/// the engine fixes no look, the game turns them on. Read every frame; an invalid change is
/// logged and the last valid settings stay ([`SunShadowSettings::validate`]). Ignored on Mid and
/// Low, and on integrated GPUs unless [`SunShadowSettings::on_integrated`]
/// ([`SunShadowSettings::applies`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunShadowSettings {
    /// Draw sun shadows. Default false.
    pub enabled: bool,
    /// Draw them on an integrated GPU too (the Radeon 780M pays about four times the RTX's
    /// cost). Default false.
    pub on_integrated: bool,
    /// How much of the sky light a shadow takes, `0..=1`: 0.4 keeps 60 %. Default 0.4.
    pub strength: f32,
    /// How far from the camera there are shadows, in blocks (`16..=1024`, and at most the view
    /// distance); they fade out over the last 10 %. Default 160.
    pub distance: f32,
    /// Cascades between the camera and the distance, `1..=MAX_CASCADES`. Default 3.
    pub cascades: u32,
    /// Texels per side of each cascade's map, a power of two in `256..=4096`. Default 2048.
    pub resolution: u32,
}

impl SunShadowSettings {
    /// The default [`SunShadowSettings::strength`].
    pub const DEFAULT_STRENGTH: f32 = 0.4;
    /// The default [`SunShadowSettings::distance`].
    pub const DEFAULT_DISTANCE: f32 = 160.0;
    /// The default [`SunShadowSettings::cascades`].
    pub const DEFAULT_CASCADES: u32 = 3;
    /// The default [`SunShadowSettings::resolution`].
    pub const DEFAULT_RESOLUTION: u32 = 2048;

    /// The default settings, turned on.
    pub fn on() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// Checks the ranges of the field docs.
    pub fn validate(&self) -> Result<(), SunShadowError> {
        let fail = |what: String| Err(SunShadowError(what));
        if !(0.0..=1.0).contains(&self.strength) {
            return fail(format!("strength {} is not in 0..=1", self.strength));
        }
        if !(16.0..=1024.0).contains(&self.distance) {
            return fail(format!("distance {} is not in 16..=1024", self.distance));
        }
        if !(1..=MAX_CASCADES as u32).contains(&self.cascades) {
            return fail(format!(
                "cascades {} is not in 1..={MAX_CASCADES}",
                self.cascades
            ));
        }
        if !(self.resolution.is_power_of_two() && (256..=4096).contains(&self.resolution)) {
            return fail(format!(
                "resolution {} is not a power of two in 256..=4096",
                self.resolution
            ));
        }
        Ok(())
    }

    /// Whether a renderer of `tier` on a GPU of `device_type` draws sun shadows: when enabled,
    /// on High only, and on an integrated GPU only with [`SunShadowSettings::on_integrated`].
    pub fn applies(&self, tier: Tier, device_type: wgpu::DeviceType) -> bool {
        self.enabled
            && tier == Tier::High
            && (self.on_integrated || device_type != wgpu::DeviceType::IntegratedGpu)
    }
}

impl Default for SunShadowSettings {
    /// Off; 40 % strength, 160 blocks, 3 cascades of 2048 x 2048 when turned on.
    fn default() -> Self {
        Self {
            enabled: false,
            on_integrated: false,
            strength: Self::DEFAULT_STRENGTH,
            distance: Self::DEFAULT_DISTANCE,
            cascades: Self::DEFAULT_CASCADES,
            resolution: Self::DEFAULT_RESOLUTION,
        }
    }
}

/// An invalid [`SunShadowSettings`] field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SunShadowError(pub String);

impl std::fmt::Display for SunShadowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SunShadowError {}

/// The sun of one frame ([`crate::SceneLight::sun`]): where it is and how strong a shadow is in
/// each realm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneSun {
    /// The unit direction toward the sun (world axes), on its step (`boxel_world::SunPath`).
    pub direction: DVec3,
    /// The shadow strength `k` of each realm, bottom first: how much of the sky term a full
    /// shadow takes (`strength * fade(elevation)` for a realm with the sun, 0 without it). 0 at
    /// night.
    pub strength: [f32; REALM_ENVS],
    /// The realm of the camera's eye (below [`REALM_ENVS`]). Only its chunks cast and only its
    /// voxels are shadowed: no realm shadows another (ADR 0039).
    pub camera_realm: u8,
}

impl SceneSun {
    /// The sun toward `direction` (unit) with shadow strength `strength` (`0..=1`) in every
    /// realm `has_sun` says has the sun, faded by the sun's elevation ([`sun_fade`]), seen from
    /// `camera_realm`.
    pub fn new(
        direction: DVec3,
        strength: f32,
        has_sun: impl Fn(usize) -> bool,
        camera_realm: usize,
    ) -> Self {
        let k = strength.clamp(0.0, 1.0) * sun_fade(direction.y);
        Self {
            direction,
            strength: std::array::from_fn(|realm| if has_sun(realm) { k } else { 0.0 }),
            camera_realm: camera_realm.min(REALM_ENVS - 1) as u8,
        }
    }

    /// The shadow strength of the camera's realm: 0 means no shadow anywhere this frame.
    pub fn camera_strength(&self) -> f32 {
        self.strength[usize::from(self.camera_realm)]
    }
}

/// How much of its strength a shadow has with the sun at `elevation` (sine): 0 at and below the
/// horizon, 1 from [`FADE_ELEVATION`], smooth in between.
pub fn sun_fade(elevation: f64) -> f32 {
    let t = (elevation / FADE_ELEVATION).clamp(0.0, 1.0) as f32;
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_owner_s() {
        let s = SunShadowSettings::default();
        assert!(!s.enabled && !s.on_integrated);
        assert_eq!(
            (s.strength, s.distance, s.cascades, s.resolution),
            (0.4, 160.0, 3, 2048)
        );
        assert_eq!(s.validate(), Ok(()));
        assert_eq!(SunShadowSettings::on().validate(), Ok(()));
    }

    #[test]
    fn invalid_settings_are_refused() {
        let base = SunShadowSettings::on();
        let bad = [
            SunShadowSettings {
                strength: 1.5,
                ..base
            },
            SunShadowSettings {
                strength: f32::NAN,
                ..base
            },
            SunShadowSettings {
                distance: 8.0,
                ..base
            },
            SunShadowSettings {
                distance: f32::INFINITY,
                ..base
            },
            SunShadowSettings {
                cascades: 0,
                ..base
            },
            SunShadowSettings {
                cascades: MAX_CASCADES as u32 + 1,
                ..base
            },
            SunShadowSettings {
                resolution: 1000,
                ..base
            },
            SunShadowSettings {
                resolution: 8192,
                ..base
            },
        ];
        for s in bad {
            assert!(s.validate().is_err(), "{s:?}");
        }
        for s in [
            SunShadowSettings {
                strength: 0.0,
                distance: 16.0,
                cascades: 1,
                resolution: 256,
                ..base
            },
            SunShadowSettings {
                strength: 1.0,
                distance: 1024.0,
                cascades: MAX_CASCADES as u32,
                resolution: 4096,
                ..base
            },
        ] {
            assert_eq!(s.validate(), Ok(()), "{s:?}");
        }
    }

    /// High only; integrated GPUs only when asked; never while off.
    #[test]
    fn shadows_apply_on_high_only() {
        use wgpu::DeviceType::{Cpu, DiscreteGpu, IntegratedGpu};
        let on = SunShadowSettings::on();
        assert!(on.applies(Tier::High, DiscreteGpu));
        assert!(!on.applies(Tier::High, IntegratedGpu));
        for tier in [Tier::Mid, Tier::Low] {
            assert!(!on.applies(tier, DiscreteGpu));
        }
        let everywhere = SunShadowSettings {
            on_integrated: true,
            ..on
        };
        assert!(everywhere.applies(Tier::High, IntegratedGpu));
        assert!(!everywhere.applies(Tier::Mid, IntegratedGpu));
        assert!(everywhere.applies(Tier::High, Cpu));
        assert!(!SunShadowSettings::default().applies(Tier::High, DiscreteGpu));
    }

    /// No shadow at or below the horizon, the full strength from 0.2, rising in between.
    #[test]
    fn shadows_fade_near_the_horizon() {
        assert_eq!(sun_fade(-1.0), 0.0);
        assert_eq!(sun_fade(0.0), 0.0);
        assert_eq!(sun_fade(FADE_ELEVATION), 1.0);
        assert_eq!(sun_fade(1.0), 1.0);
        assert!((sun_fade(0.1) - 0.5).abs() < 1e-6);
        let mut last = 0.0;
        for i in 0..=200 {
            let fade = sun_fade(f64::from(i) * 0.001);
            assert!(fade >= last && fade <= 1.0);
            last = fade;
        }
    }

    /// `k` is the strength times the fade in realms with the sun, 0 in the others and at night.
    #[test]
    fn each_realm_has_its_strength() {
        let up = DVec3::new(0.0, 1.0, 0.0);
        let sun = SceneSun::new(up, 0.4, |realm| realm != 1, 2);
        assert_eq!(sun.strength[0], 0.4);
        assert_eq!(sun.strength[1], 0.0);
        assert_eq!(sun.strength[REALM_ENVS - 1], 0.4);
        assert_eq!((sun.camera_realm, sun.camera_strength()), (2, 0.4));
        assert_eq!(SceneSun::new(up, 0.4, |_| true, 1).camera_strength(), 0.4);
        assert_eq!(SceneSun::new(up, 0.4, |r| r != 1, 1).camera_strength(), 0.0);
        let night = SceneSun::new(-up, 0.4, |_| true, 0);
        assert_eq!(night.strength, [0.0; REALM_ENVS]);
        let low = DVec3::new(0.995, 0.1, 0.0).normalize();
        let dusk = SceneSun::new(low, 0.4, |_| true, 0);
        assert!(dusk.camera_strength() > 0.0 && dusk.camera_strength() < 0.4);
        // Out-of-range input is clamped.
        let clamped = SceneSun::new(up, 2.0, |_| true, 100);
        assert_eq!(
            (clamped.camera_strength(), clamped.camera_realm),
            (1.0, REALM_ENVS as u8 - 1)
        );
    }
}
