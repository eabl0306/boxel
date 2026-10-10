//! Fog and cloud presets of the demo world (issue #47, ADR 0030): the look of the example game,
//! not the engine's. The terrain sits between y = 16 and 112 with the sea at 46
//! ([`crate::worldgen`]), so the valley fog pools around sea level and the clouds float well
//! above every hill.

use boxel_fog::{FogFade, FogGround, FogLayer, FogNoise, NoiseDims, density_for_visibility};

/// Minecraft-like clouds: flat 12x4x12 blocks at y 192..196, crisp, white, drifting 2 blocks a
/// second along +x, faded out by 768 blocks.
pub fn clouds() -> FogLayer {
    FogLayer {
        cell: [12, 4, 12],
        noise: FogNoise {
            scale: 4,
            octaves: 1,
            dims: NoiseDims::Flat,
            seed: 0xc10d,
        },
        threshold: 0.55,
        softness: 0.0,
        density: 2.0,
        wind: [2.0, 0.0, 0.0],
        max_distance: 768.0,
        fade: FogFade::Range {
            start: 384.0,
            end: 768.0,
        },
        ..FogLayer::new(192, 196)
    }
}

/// Valley fog: banks of 2-block cubes from y 40 to 64 that pool in the low ground (6 blocks over
/// the lowest ground within 8 texels), seen about 40 blocks through, taking a third of the sky's
/// color, drifting slowly.
pub fn valley() -> FogLayer {
    FogLayer {
        cell: [2, 2, 2],
        noise: FogNoise {
            scale: 8,
            octaves: 2,
            dims: NoiseDims::Volume,
            seed: 0xfa11e7,
        },
        threshold: 0.45,
        softness: 0.25,
        density: density_for_visibility(40.0),
        color: [0.92, 0.94, 1.0],
        sky_tint: 0.35,
        wind: [0.6, 0.0, 0.3],
        ground: FogGround::Pooled {
            depth: 6,
            radius: 8,
        },
        max_distance: 384.0,
        ..FogLayer::new(40, 64)
    }
}

/// A blanket of 2-block cubes filling `[min_y, max_y)` (both even), seen about `visibility`
/// blocks through where it is thickest. Every cell has some fog, of a density that varies with
/// 3D noise, so no lattice step can be skipped and every ray inside walks the most cells: the
/// worst case. (A blanket of equal cells would be cheap: the march takes it whole.)
pub fn blanket(min_y: i64, max_y: i64, visibility: f32) -> FogLayer {
    FogLayer {
        cell: [2, 2, 2],
        noise: FogNoise {
            scale: 4,
            octaves: 1,
            dims: NoiseDims::Volume,
            seed: 0xb1a2,
        },
        threshold: 0.0,
        softness: 1.0,
        density: density_for_visibility(visibility),
        color: [0.9, 0.9, 0.92],
        ..FogLayer::new(min_y, max_y)
    }
}

/// Valley fog and clouds tuned for dusk: the fog takes more of the warm sky, the clouds a bit.
pub fn sunset() -> Vec<FogLayer> {
    let valley = FogLayer {
        sky_tint: 0.6,
        color: [1.0, 0.95, 0.9],
        ..valley()
    };
    let clouds = FogLayer {
        sky_tint: 0.3,
        ..clouds()
    };
    vec![valley, clouds]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_presets_are_valid() {
        for layers in [
            vec![clouds()],
            vec![valley()],
            vec![valley(), clouds()],
            vec![blanket(0, 256, 40.0)],
            sunset(),
        ] {
            assert_eq!(boxel_fog::validate(&layers, 4, 64), Ok(()), "{layers:?}");
        }
    }
}
