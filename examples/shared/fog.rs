//! `--fog PRESET` and `--fog-res` of the examples (`stress`, `snapshot`, `voxel_world`): the
//! demo's fog presets (`boxel_demo::fog`, ADR 0030).
#![allow(dead_code, reason = "each example uses its own part")]

use boxel::render::fog::{FogLayer, FogResolution, FogSettings};

/// Help lines of `--fog` and `--fog-res`.
pub const USAGE: &str =
    "  --fog PRESET         fog and clouds of the demo: off (the default, no fog plugin), empty
                       (the plugin with no layers), valley, clouds, both (valley and
                       clouds), inside (a blanket from y 0 to 256, seen 40 blocks through),
                       sunset (valley and clouds tuned for dusk)
  --fog-res half|full  resolution of the fog march (default half)";

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

    /// The settings of the preset at `resolution`, `None` for [`FogPreset::Off`].
    pub fn settings(self, resolution: FogResolution) -> Option<FogSettings> {
        self.layers().map(|layers| FogSettings {
            resolution,
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

/// `valley half` and the like, for the run's summary.
pub fn label(preset: FogPreset, resolution: FogResolution) -> String {
    match (preset, resolution) {
        (FogPreset::Off, _) => "off".to_string(),
        (p, FogResolution::Half) => format!("{} half", p.name()),
        (p, FogResolution::Full) => format!("{} full", p.name()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_parse_and_validate() {
        for p in FogPreset::ALL {
            assert_eq!(parse("--fog", p.name()), Ok(p));
            if let Some(settings) = p.settings(FogResolution::Full) {
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
        assert_eq!(label(FogPreset::Valley, FogResolution::Half), "valley half");
    }
}
