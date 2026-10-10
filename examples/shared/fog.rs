//! `--fog PRESET`, `--fog-res` and `--fog-steps` of the examples (`stress`, `snapshot`): the
//! demo's fog presets (`boxel_demo::fog`, ADR 0030).
#![allow(dead_code, reason = "each example uses its own part")]

use boxel::render::fog::{FogLayer, FogResolution, FogSettings};

/// Help lines of `--fog`, `--fog-res` and `--fog-steps`.
pub const USAGE: &str =
    "  --fog PRESET         fog and clouds of the demo: off (the default, no fog plugin), empty
                       (the plugin with no layers), valley, clouds, both (valley and
                       clouds), inside (a blanket from y 0 to 256, seen 40 blocks through),
                       sunset (valley and clouds tuned for dusk)
  --fog-res half|full  resolution of the fog march (default half)
  --fog-steps N        cells a fog ray visits at most, 1 to 256 (default 64)";

/// A fog preset of the examples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FogPreset {
    /// No fog plugin.
    #[default]
    Off,
    /// The plugin with no layers (prices only the sampled depth).
    Empty,
    /// Valley fog.
    Valley,
    /// Clouds.
    Clouds,
    /// Valley fog and clouds.
    Both,
    /// A blanket the camera is inside: the worst case.
    Inside,
    /// Valley fog and clouds for dusk.
    Sunset,
}

impl FogPreset {
    /// Every preset, in the order of the help.
    pub const ALL: [FogPreset; 7] = [
        FogPreset::Off,
        FogPreset::Empty,
        FogPreset::Valley,
        FogPreset::Clouds,
        FogPreset::Both,
        FogPreset::Inside,
        FogPreset::Sunset,
    ];

    /// Its name on the command line.
    pub fn name(self) -> &'static str {
        match self {
            FogPreset::Off => "off",
            FogPreset::Empty => "empty",
            FogPreset::Valley => "valley",
            FogPreset::Clouds => "clouds",
            FogPreset::Both => "both",
            FogPreset::Inside => "inside",
            FogPreset::Sunset => "sunset",
        }
    }

    /// The layers, `None` for [`FogPreset::Off`].
    pub fn layers(self) -> Option<Vec<FogLayer>> {
        use boxel_demo::fog;
        Some(match self {
            FogPreset::Off => return None,
            FogPreset::Empty => Vec::new(),
            FogPreset::Valley => vec![fog::valley()],
            FogPreset::Clouds => vec![fog::clouds()],
            FogPreset::Both => vec![fog::valley(), fog::clouds()],
            FogPreset::Inside => vec![fog::blanket(0, 256, 40.0)],
            FogPreset::Sunset => fog::sunset(),
        })
    }

    /// The settings of the preset at `resolution` with a budget of `steps` cells per ray,
    /// `None` for [`FogPreset::Off`].
    pub fn settings(self, resolution: FogResolution, steps: u32) -> Option<FogSettings> {
        self.layers().map(|layers| FogSettings {
            resolution,
            max_steps: steps,
            ..FogSettings::new(layers)
        })
    }
}

/// `--fog PRESET`.
pub fn parse(flag: &str, value: &str) -> Result<FogPreset, String> {
    FogPreset::ALL
        .into_iter()
        .find(|p| p.name() == value)
        .ok_or_else(|| {
            let names: Vec<&str> = FogPreset::ALL.iter().map(|p| p.name()).collect();
            format!("{flag}: {value:?} is not {}", names.join(", "))
        })
}

/// `--fog-res half|full`.
pub fn parse_resolution(flag: &str, value: &str) -> Result<FogResolution, String> {
    match value {
        "half" => Ok(FogResolution::Half),
        "full" => Ok(FogResolution::Full),
        _ => Err(format!("{flag}: {value:?} is not half or full")),
    }
}

/// `--fog-steps N`.
pub fn parse_steps(flag: &str, value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|n| (1..=256).contains(n))
        .ok_or_else(|| format!("{flag}: {value:?} is not in 1..=256"))
}

/// `valley half` and the like, for the run's summary.
pub fn label(preset: FogPreset, resolution: FogResolution, steps: u32) -> String {
    let res = match resolution {
        FogResolution::Half => "half",
        FogResolution::Full => "full",
    };
    match preset {
        FogPreset::Off => "off".to_string(),
        p if steps == 64 => format!("{} {res}", p.name()),
        p => format!("{} {res} {steps} steps", p.name()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_parse_and_validate() {
        for p in FogPreset::ALL {
            assert_eq!(parse("--fog", p.name()), Ok(p));
            if let Some(settings) = p.settings(FogResolution::Full, 64) {
                assert_eq!(settings.validate(), Ok(()), "{}", p.name());
                assert_eq!(settings.resolution, FogResolution::Full);
            }
        }
        assert_eq!(FogPreset::Off.layers(), None);
        assert_eq!(FogPreset::Empty.layers(), Some(Vec::new()));
        assert!(parse("--fog", "mist").is_err());
        assert_eq!(
            parse_resolution("--fog-res", "half"),
            Ok(FogResolution::Half)
        );
        assert!(parse_resolution("--fog-res", "quarter").is_err());
        assert_eq!(parse_steps("--fog-steps", "32"), Ok(32));
        assert!(parse_steps("--fog-steps", "0").is_err());
        assert!(parse_steps("--fog-steps", "300").is_err());
        assert_eq!(
            label(FogPreset::Valley, FogResolution::Half, 64),
            "valley half"
        );
        assert_eq!(
            label(FogPreset::Inside, FogResolution::Full, 32),
            "inside full 32 steps"
        );
    }
}
