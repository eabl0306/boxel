//! The terrain's distance fog (issue #51): the last stretch before the edge of what is drawn
//! fades into the sky color, so chunks (and the far LOD) do not end in a hard line. Spherical:
//! the distance is 3D, since the world is infinite up and down as well.

/// The fog distances while the distance fog is off ([`DistanceFog::range`]): both far past
/// anything drawn, so the shaders' `(d - start) / max(end - start, 1)` stays 0 with finite
/// arithmetic (no infinity, no NaN).
pub const NO_FOG: [f32; 2] = [f32::MAX, f32::MAX];

/// How the terrain fades into the sky color at the edge of what is drawn
/// ([`crate::RenderSettings::distance_fog`]): only the end, over `min(fade_fraction * d,
/// max_fade)` blocks before the distance `d` (the view distance, or the far LOD's horizon while
/// it draws). The defaults are Minecraft's: the last 10 %, at most 64 blocks (two chunks).
/// Off, nothing fades: the terrain ends at the distance with a hard edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DistanceFog {
    /// False turns the fade off (with or without the far LOD). Default true.
    pub enabled: bool,
    /// Fraction of the distance the fade takes, `0..=1` (0: a hard edge). Default 0.1.
    pub fade_fraction: f32,
    /// The longest fade in blocks, finite and `>= 0`. Default 64.
    pub max_fade: f32,
}

impl DistanceFog {
    /// The default [`DistanceFog::fade_fraction`]: the last 10 % of the distance.
    pub const DEFAULT_FADE_FRACTION: f32 = 0.1;
    /// The default [`DistanceFog::max_fade`]: two chunks.
    pub const DEFAULT_MAX_FADE: f32 = 64.0;

    /// No fade at all ([`DistanceFog::enabled`] false, the default numbers kept).
    pub fn off() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Checks the ranges of the field docs.
    pub fn validate(&self) -> Result<(), DistanceFogError> {
        if !(0.0..=1.0).contains(&self.fade_fraction) {
            return Err(DistanceFogError(format!(
                "fade_fraction {} is not in 0..=1",
                self.fade_fraction
            )));
        }
        if !(self.max_fade.is_finite() && self.max_fade >= 0.0) {
            return Err(DistanceFogError(format!(
                "max_fade {} is not finite and >= 0",
                self.max_fade
            )));
        }
        Ok(())
    }

    /// Fog start and end for terrain drawn to `distance` blocks: from `distance - min(
    /// fade_fraction * distance, max_fade)` to `distance`. Off, [`NO_FOG`]. Invalid settings
    /// ([`DistanceFog::validate`]) give the default's range.
    pub fn range(&self, distance: f32) -> [f32; 2] {
        if !self.enabled {
            return NO_FOG;
        }
        let fog = if self.validate().is_ok() {
            *self
        } else {
            Self::default()
        };
        let fade = (fog.fade_fraction * distance).min(fog.max_fade);
        [distance - fade, distance]
    }
}

impl Default for DistanceFog {
    /// On, the last 10 % of the distance, at most 64 blocks.
    fn default() -> Self {
        Self {
            enabled: true,
            fade_fraction: Self::DEFAULT_FADE_FRACTION,
            max_fade: Self::DEFAULT_MAX_FADE,
        }
    }
}

/// An invalid [`DistanceFog`] field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DistanceFogError(pub String);

impl std::fmt::Display for DistanceFogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DistanceFogError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_end_fades() {
        let fog = DistanceFog::default();
        // r2: 10 % of 64 blocks.
        assert_eq!(fog.range(64.0), [64.0 - 6.4, 64.0]);
        assert!((fog.range(64.0)[0] - 57.6).abs() < 1e-4);
        // r16: 10 % of 512 blocks, under the cap.
        assert_eq!(fog.range(512.0), [460.8, 512.0]);
        // From 640 blocks (r20) on, the cap: two chunks.
        assert_eq!(fog.range(640.0), [576.0, 640.0]);
        assert_eq!(fog.range(1024.0), [960.0, 1024.0]);
        // The far LOD's horizon.
        assert_eq!(fog.range(12_288.0), [12_224.0, 12_288.0]);
        assert_eq!(fog.range(0.0), [0.0, 0.0]);
    }

    #[test]
    fn the_fraction_and_the_cap_are_the_game_s() {
        let longer = DistanceFog {
            fade_fraction: 0.4,
            max_fade: 1000.0,
            ..DistanceFog::default()
        };
        // The look before issue #51: from 60 % of the distance.
        assert_eq!(longer.range(512.0), [307.2, 512.0]);
        let capped = DistanceFog {
            max_fade: 16.0,
            ..DistanceFog::default()
        };
        assert_eq!(capped.range(512.0), [496.0, 512.0]);
        assert_eq!(capped.range(64.0), [64.0 - 6.4, 64.0]);
        let hard = DistanceFog {
            fade_fraction: 0.0,
            ..DistanceFog::default()
        };
        assert_eq!(hard.range(512.0), [512.0, 512.0]);
        let whole = DistanceFog {
            fade_fraction: 1.0,
            max_fade: f32::MAX,
            ..DistanceFog::default()
        };
        assert_eq!(whole.range(512.0), [0.0, 512.0]);
    }

    #[test]
    fn off_fades_nothing_with_finite_numbers() {
        let off = DistanceFog::off();
        for distance in [0.0, 64.0, 512.0, 12_288.0] {
            assert_eq!(off.range(distance), NO_FOG);
        }
        // The shaders' fog factor: 0 everywhere, never NaN.
        let [start, end] = NO_FOG;
        for d in [0.0f32, 512.0, 1e9, f32::MAX] {
            let fog = ((d - start) / (end - start).max(1.0)).clamp(0.0, 1.0);
            assert_eq!(fog, 0.0, "{d}");
        }
        // Off whatever the numbers.
        let off_invalid = DistanceFog {
            fade_fraction: f32::NAN,
            ..DistanceFog::off()
        };
        assert_eq!(off_invalid.range(512.0), NO_FOG);
    }

    #[test]
    fn validate_checks_the_ranges() {
        assert_eq!(DistanceFog::default().validate(), Ok(()));
        assert_eq!(DistanceFog::off().validate(), Ok(()));
        for (fade_fraction, max_fade) in [
            (-0.1, 64.0),
            (1.5, 64.0),
            (f32::NAN, 64.0),
            (0.1, -1.0),
            (0.1, f32::INFINITY),
            (0.1, f32::NAN),
        ] {
            let bad = DistanceFog {
                fade_fraction,
                max_fade,
                ..DistanceFog::default()
            };
            assert!(bad.validate().is_err(), "{bad:?}");
            // Invalid numbers give the default's range, never a NaN.
            assert_eq!(bad.range(1024.0), [960.0, 1024.0], "{bad:?}");
        }
        for (fade_fraction, max_fade) in [(0.0, 0.0), (1.0, 0.0), (0.5, 1e6)] {
            let good = DistanceFog {
                fade_fraction,
                max_fade,
                ..DistanceFog::default()
            };
            assert_eq!(good.validate(), Ok(()), "{good:?}");
        }
    }
}
