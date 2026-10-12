//! `--lod PRESET` of the examples: the far LOD's presets (ADR 0031). They are distances and
//! device choices, not content, so they live here (the demo's part of the LOD is its generators'
//! `LodSource`).
#![allow(dead_code, reason = "each example uses its own part")]

use boxel::world::lod::{LodSettings, LodStats};

/// Help lines of `--lod`.
pub const USAGE: &str =
    "  --lod PRESET         far terrain past the view distance (ADR 0031): off (the default, no
                       LOD plugin), near (4,096 blocks, for the 780M), far (12,288) or
                       extreme (49,152)";

/// A LOD preset of the examples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LodPreset {
    /// No LOD plugin.
    #[default]
    Off,
    /// `R = 16`, `K = 3`: a horizon of 4,096 blocks.
    Near,
    /// `R = 24`, `K = 4`: a horizon of 12,288 blocks.
    Far,
    /// `R = 24`, `K = 6`: a horizon of 49,152 blocks (edits do not show at level 6).
    Extreme,
}

impl LodPreset {
    /// Every preset, in the order of the help.
    pub const ALL: [LodPreset; 4] = [
        LodPreset::Off,
        LodPreset::Near,
        LodPreset::Far,
        LodPreset::Extreme,
    ];

    /// Its name on the command line.
    pub fn name(self) -> &'static str {
        match self {
            LodPreset::Off => "off",
            LodPreset::Near => "near",
            LodPreset::Far => "far",
            LodPreset::Extreme => "extreme",
        }
    }

    /// The settings, `None` for [`LodPreset::Off`]. Each preset sizes its own arena: while a
    /// level refines, parent and children are both resident.
    pub fn settings(self) -> Option<LodSettings> {
        let (ring_radius, levels, arena_quads) = match self {
            LodPreset::Off => return None,
            LodPreset::Near => (16, 3, 4 << 20),
            LodPreset::Far => (24, 4, 12 << 20),
            LodPreset::Extreme => (24, 6, 16 << 20),
        };
        Some(LodSettings {
            ring_radius,
            levels,
            arena_quads,
            ..LodSettings::default()
        })
    }

    /// Its horizon `32 R 2^K` in blocks, `None` for [`LodPreset::Off`]: where the far fog
    /// presets end their clouds (`fog::FogPreset::layers`).
    pub fn horizon(self) -> Option<f32> {
        self.settings().map(|s| s.preset_horizon() as f32)
    }

    /// The horizon the LOD draws to: [`LodPreset::horizon`] while it draws, `None` while its
    /// stats say it draws nothing ([`LodStats::off`]: disabled, a GL backend or a failed LOD
    /// pass, its guard...). Without stats (no LOD plugin yet), the preset's.
    pub fn drawn_horizon(self, stats: Option<&LodStats>) -> Option<f32> {
        self.horizon()
            .filter(|_| stats.is_none_or(|s| s.off.is_none()))
    }
}

/// `--lod PRESET`.
pub fn parse(flag: &str, value: &str) -> Result<LodPreset, String> {
    LodPreset::ALL
        .into_iter()
        .find(|p| p.name() == value)
        .ok_or_else(|| {
            let names: Vec<&str> = LodPreset::ALL.iter().map(|p| p.name()).collect();
            format!("{flag}: {value:?} is not {}", names.join(", "))
        })
}

/// `far (R 24, K 4, 12288 blocks)` and the like, for the run's summary.
pub fn label(preset: LodPreset) -> String {
    match preset.settings() {
        None => preset.name().to_string(),
        Some(s) => format!(
            "{} (R {}, K {}, {} blocks)",
            preset.name(),
            s.ring_radius,
            s.levels,
            s.preset_horizon()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel::world::lod::KeepExtent;

    #[test]
    fn presets_validate_and_reach_their_horizons() {
        // A viewer at r16 v8 at ground level (hysteresis 2).
        let keep = KeepExtent::around(16, 8, 2, 2, None);
        for (preset, horizon) in [
            (LodPreset::Near, 4096.0),
            (LodPreset::Far, 12_288.0),
            (LodPreset::Extreme, 49_152.0),
        ] {
            let s = preset.settings().expect("a preset with settings");
            assert_eq!(s.validate(), Ok(()), "{preset:?}");
            assert_eq!(s.horizon(keep), horizon, "{preset:?}");
            assert_eq!(preset.horizon(), Some(horizon as f32), "{preset:?}");
        }
        assert!(LodPreset::Off.settings().is_none());
        assert_eq!(LodPreset::Off.horizon(), None);
    }

    #[test]
    fn the_drawn_horizon_is_none_while_the_lod_draws_nothing() {
        use boxel::world::lod::LodOff;
        let far = LodPreset::Far;
        assert_eq!(far.drawn_horizon(None), Some(12_288.0));
        let mut stats = LodStats::default();
        assert_eq!(far.drawn_horizon(Some(&stats)), Some(12_288.0));
        for off in [LodOff::Unsupported, LodOff::Guard, LodOff::Disabled] {
            stats.off = Some(off);
            assert_eq!(far.drawn_horizon(Some(&stats)), None, "{off:?}");
        }
        assert_eq!(LodPreset::Off.drawn_horizon(None), None);
    }

    #[test]
    fn names_parse_back() {
        for preset in LodPreset::ALL {
            assert_eq!(parse("--lod", preset.name()), Ok(preset));
        }
        assert!(parse("--lod", "huge").is_err());
        assert_eq!(label(LodPreset::Far), "far (R 24, K 4, 12288 blocks)");
    }
}
