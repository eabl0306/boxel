//! `--fog PRESET`, `--fog-haze`, `--fog-res` and `--fog-steps` of the examples (`stress`,
//! `snapshot`, `voxel_world`): the demo's fog presets (`boxel_demo::fog`, ADR 0030 and 0032).
#![allow(dead_code, reason = "each example uses its own part")]

use boxel::fog::{DEFAULT_MAX_STEPS, FogFade};
use boxel::render::fog::{FogLayer, FogResolution, FogSettings};

/// Help lines of `--fog`, `--fog-haze`, `--fog-res` and `--fog-steps`.
pub const USAGE: &str =
    "  --fog PRESET         fog and clouds of the demo: off (the default, no fog plugin), empty
                       (the plugin with no layers), valley, clouds, both (valley and
                       clouds), cumulus16, cumulus20, cumulus24 (big clouds of 16, 20 or
                       24-block cubes over a valley that goes on as a haze to the horizon),
                       decks (cumulus16 under a thin high deck), inside (a blanket from y 0
                       to 256, seen 40 blocks through), sunset (valley and clouds tuned for
                       dusk)
  --fog-haze F         the haze past the cells, 0 to 1 times each layer's (default 1); at 0
                       the clouds fade out at their cells (Shift+F7 in voxel_world)
  --fog-res half|full  resolution of the fog march (default half)
  --fog-steps N        cells a fog ray visits at most, 1 to 256 (default 64, the engine's)";

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
    /// Valley fog and clouds: today's look, the reference of every measurement.
    Both,
    /// The hazy valley under big clouds of 16-block cubes.
    Cumulus16,
    /// The hazy valley under big clouds of 20-block cubes.
    Cumulus20,
    /// The hazy valley under big clouds of 24-block cubes.
    Cumulus24,
    /// The hazy valley under the 16-block clouds and a thin high deck.
    Decks,
    /// A blanket the camera is inside: the worst case.
    Inside,
    /// Valley fog and clouds for dusk.
    Sunset,
}

impl FogPreset {
    /// Every preset, in the order of the help (and of F7 in `voxel_world`).
    pub const ALL: [FogPreset; 11] = [
        FogPreset::Off,
        FogPreset::Empty,
        FogPreset::Valley,
        FogPreset::Clouds,
        FogPreset::Both,
        FogPreset::Cumulus16,
        FogPreset::Cumulus20,
        FogPreset::Cumulus24,
        FogPreset::Decks,
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
            FogPreset::Cumulus16 => "cumulus16",
            FogPreset::Cumulus20 => "cumulus20",
            FogPreset::Cumulus24 => "cumulus24",
            FogPreset::Decks => "decks",
            FogPreset::Inside => "inside",
            FogPreset::Sunset => "sunset",
        }
    }

    /// The layers, `None` for [`FogPreset::Off`]. The far presets end their clouds at `horizon`,
    /// the far LOD's (`None` without it: 768 blocks, where today's clouds end).
    pub fn layers(self, horizon: Option<f32>) -> Option<Vec<FogLayer>> {
        use boxel_demo::fog;
        let h = horizon.unwrap_or(fog::NO_LOD_HORIZON);
        let hazy = |clouds: Vec<FogLayer>| [vec![fog::hazy_valley()], clouds].concat();
        Some(match self {
            FogPreset::Off => return None,
            FogPreset::Empty => Vec::new(),
            FogPreset::Valley => vec![fog::valley()],
            FogPreset::Clouds => vec![fog::clouds()],
            FogPreset::Both => vec![fog::valley(), fog::clouds()],
            FogPreset::Cumulus16 => hazy(vec![fog::cumulus16(h)]),
            FogPreset::Cumulus20 => hazy(vec![fog::cumulus20(h)]),
            FogPreset::Cumulus24 => hazy(vec![fog::cumulus24(h)]),
            FogPreset::Decks => hazy(fog::decks(h)),
            FogPreset::Inside => vec![fog::blanket(0, 256, 40.0)],
            FogPreset::Sunset => fog::sunset(),
        })
    }

    /// The settings of the preset at `resolution` with a budget of `steps` cells per ray, its
    /// clouds ending at `horizon` and its haze times `haze` ([`with_haze_factor`]), `None` for
    /// [`FogPreset::Off`].
    pub fn settings(
        self,
        resolution: FogResolution,
        steps: u32,
        horizon: Option<f32>,
        haze: f32,
    ) -> Option<FogSettings> {
        self.layers(horizon).map(|layers| FogSettings {
            resolution,
            max_steps: steps,
            ..FogSettings::new(with_haze_factor(layers, haze))
        })
    }
}

/// `layers` with every layer's haze times `factor` (0 to 1). At 0 the layers end with their
/// cells, and a hazy layer's [`FogFade::Range`] that ends past them is refit to fade out over
/// the second half of where they end, so the clouds do not stop in a hard circle (a `Scene`
/// layer already does that without haze). A layer without haze is left as it is.
pub fn with_haze_factor(layers: Vec<FogLayer>, factor: f32) -> Vec<FogLayer> {
    layers
        .into_iter()
        .map(|layer| {
            let haze = layer.haze * factor;
            let fade = match layer.fade {
                FogFade::Range { end, .. }
                    if layer.haze > 0.0 && haze <= 0.0 && end > layer.max_distance =>
                {
                    FogFade::Range {
                        start: 0.5 * layer.max_distance,
                        end: layer.max_distance,
                    }
                }
                fade => fade,
            };
            FogLayer {
                haze,
                fade,
                ..layer
            }
        })
        .collect()
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

/// `--fog-haze F`: 0 to 1.
pub fn parse_haze(flag: &str, value: &str) -> Result<f32, String> {
    value
        .parse::<f32>()
        .ok()
        .filter(|f| (0.0..=1.0).contains(f))
        .ok_or_else(|| format!("{flag}: {value:?} is not in 0..=1"))
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

/// `valley half`, `cumulus16 half haze x0.25` and the like, for the run's summary.
pub fn label(preset: FogPreset, resolution: FogResolution, steps: u32, haze: f32) -> String {
    let res = match resolution {
        FogResolution::Half => "half",
        FogResolution::Full => "full",
    };
    let mut label = match preset {
        FogPreset::Off => return "off".to_string(),
        p if steps == DEFAULT_MAX_STEPS => format!("{} {res}", p.name()),
        p => format!("{} {res} {steps} steps", p.name()),
    };
    if haze != 1.0 {
        label.push_str(&format!(" haze x{haze}"));
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No LOD and the far LOD's three horizons.
    const HORIZONS: [Option<f32>; 4] = [None, Some(4096.0), Some(12_288.0), Some(49_152.0)];

    #[test]
    fn presets_parse_and_validate() {
        for p in FogPreset::ALL {
            assert_eq!(parse("--fog", p.name()), Ok(p));
            for horizon in HORIZONS {
                for haze in [1.0, 0.25, 0.0] {
                    let Some(settings) = p.settings(FogResolution::Full, 64, horizon, haze) else {
                        continue;
                    };
                    assert_eq!(settings.validate(), Ok(()), "{} {horizon:?}", p.name());
                    assert_eq!(settings.resolution, FogResolution::Full);
                }
            }
        }
        assert_eq!(FogPreset::Off.layers(None), None);
        assert_eq!(FogPreset::Empty.layers(None), Some(Vec::new()));
        assert!(parse("--fog", "mist").is_err());
        // The far presets come after today's `both`, which stays as it was.
        let names: Vec<&str> = FogPreset::ALL.iter().map(|p| p.name()).collect();
        assert_eq!(
            names[4..9],
            ["both", "cumulus16", "cumulus20", "cumulus24", "decks"]
        );
        assert_eq!(
            FogPreset::Both.layers(Some(12_288.0)),
            Some(vec![boxel_demo::fog::valley(), boxel_demo::fog::clouds()])
        );
        for name in ["cumulus16", "cumulus20", "cumulus24", "decks", "--fog-haze"] {
            assert!(USAGE.contains(name), "{name}");
        }
        assert_eq!(
            parse_resolution("--fog-res", "half"),
            Ok(FogResolution::Half)
        );
        assert!(parse_resolution("--fog-res", "quarter").is_err());
        assert_eq!(parse_steps("--fog-steps", "32"), Ok(32));
        assert!(parse_steps("--fog-steps", "0").is_err());
        assert!(parse_steps("--fog-steps", "300").is_err());
        assert_eq!(parse_haze("--fog-haze", "0.25"), Ok(0.25));
        assert_eq!(parse_haze("--fog-haze", "0"), Ok(0.0));
        assert_eq!(parse_haze("--fog-haze", "1"), Ok(1.0));
        for bad in ["-0.1", "1.5", "NaN", "half"] {
            assert!(parse_haze("--fog-haze", bad).is_err(), "{bad}");
        }
        assert_eq!(
            label(
                FogPreset::Valley,
                FogResolution::Half,
                DEFAULT_MAX_STEPS,
                1.0
            ),
            "valley half"
        );
        assert_eq!(
            label(FogPreset::Inside, FogResolution::Full, 32, 1.0),
            "inside full 32 steps"
        );
        assert_eq!(
            label(FogPreset::Cumulus16, FogResolution::Half, 64, 0.25),
            "cumulus16 half haze x0.25"
        );
    }

    /// The far presets end their clouds at the horizon they are given, 768 blocks without it.
    #[test]
    fn the_far_presets_take_the_horizon() {
        let ends = |horizon| {
            FogPreset::Decks
                .layers(horizon)
                .expect("layers")
                .into_iter()
                .map(|l| l.fade)
                .collect::<Vec<_>>()
        };
        let range = |start, end| FogFade::Range { start, end };
        assert_eq!(
            ends(Some(12_288.0)),
            [
                FogFade::Scene,
                range(6144.0, 12_288.0),
                range(6144.0, 12_288.0)
            ]
        );
        assert_eq!(
            ends(None),
            [FogFade::Scene, range(384.0, 768.0), range(384.0, 768.0)]
        );
        let layers = FogPreset::Cumulus20.layers(None).expect("layers");
        assert_eq!(layers[0], boxel_demo::fog::hazy_valley());
    }

    /// The haze factor scales every layer's haze; at 0 the far clouds fade out at their cells
    /// instead of stopping there, and every other layer keeps its fade.
    #[test]
    fn the_haze_factor_scales_every_layer() {
        let layers = FogPreset::Decks.layers(Some(12_288.0)).expect("layers");
        let quarter = with_haze_factor(layers.clone(), 0.25);
        for (a, b) in layers.iter().zip(&quarter) {
            assert_eq!(b.haze, 0.25 * a.haze);
            assert_eq!(b.fade, a.fade, "only the haze changes");
        }
        assert_eq!(with_haze_factor(layers.clone(), 1.0), layers);
        let none = with_haze_factor(layers.clone(), 0.0);
        assert!(none.iter().all(|l| l.haze == 0.0));
        assert_eq!(
            none[0].fade,
            FogFade::Scene,
            "the valley fades like the scene"
        );
        for l in &none[1..] {
            let e = l.max_distance;
            assert_eq!(
                l.fade,
                FogFade::Range {
                    start: 0.5 * e,
                    end: e
                },
                "{l:?}"
            );
        }
        // Without the LOD the clouds already end with their cells (768): nothing to refit.
        let near = FogPreset::Cumulus16.layers(None).expect("layers");
        let refit = with_haze_factor(near.clone(), 0.0);
        assert_eq!(refit[1].fade, near[1].fade);
        // Today's presets have no haze: the factor changes nothing.
        let both = FogPreset::Both.layers(None).expect("layers");
        assert_eq!(with_haze_factor(both.clone(), 0.0), both);
    }
}
