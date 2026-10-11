//! Fog and cloud presets of the demo world (issue #47, ADR 0030): the look of the example game,
//! not the engine's. The terrain sits between y = 16 and 112 with the sea's top water block at
//! 46 ([`crate::worldgen`]), so the valley fog lies on the sea and the low ground and the clouds
//! float well above every hill.
//!
//! Every preset is soft: no face shading (fog has no faces), cells that fill and fade as the
//! wind moves the noise, and soft fog without cell edges near the camera.

use boxel_fog::{FogFade, FogLayer, FogNoise, NoiseDims, density_for_visibility};

/// Volumetric cumulus clouds: 8-block cells in a 32-block band at y 192..224 over 3D noise. The
/// threshold rises to the top, so each cloud has a flat, wide base and thinner, uneven tops; the
/// base is gray and the top lit. Cells fill and fade as they drift 2 blocks a second along +x,
/// the edges of a cloud thinner than its core; soft within 16 blocks when flying through; faded
/// out by 768 blocks.
pub fn clouds() -> FogLayer {
    FogLayer {
        cell: [8, 8, 8],
        noise: FogNoise {
            scale: 4,
            octaves: 2,
            dims: NoiseDims::Volume,
            seed: 0xc10d,
        },
        threshold: 0.55,
        threshold_rise: 0.3,
        softness: 0.2,
        density: 0.35,
        height_shade: [0.62, 1.0],
        soft_distance: 16.0,
        wind: [2.0, 0.0, 0.0],
        max_distance: 768.0,
        fade: FogFade::Range {
            start: 384.0,
            end: 768.0,
        },
        ..FogLayer::new(192, 224)
    }
}

/// Valley fog: banks of 2-block cubes from y 48 to 64, just over the sea (its water ends at 47,
/// so no fog lies under it) and on the low ground around it, seen about 40 blocks through where
/// thickest, thinner at the edges of a bank, soft within 12 blocks, taking a third of the sky's
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
        softness: 0.3,
        density: density_for_visibility(40.0),
        color: [0.92, 0.94, 1.0],
        sky_tint: 0.35,
        soft_distance: 12.0,
        wind: [0.6, 0.0, 0.3],
        max_distance: 384.0,
        ..FogLayer::new(48, 64)
    }
}

/// A blanket of 2-block cubes filling `[min_y, max_y)` (both even), seen about `visibility`
/// blocks through where it is thickest, soft within 12 blocks. Every cell has some fog, of a
/// density that varies with 3D noise, so no lattice step can be skipped and every ray inside
/// walks the most cells and the soft fog's pieces: the worst case. (A blanket of equal cells
/// would be cheap: the march takes it whole.)
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
        soft_distance: 12.0,
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
    use boxel_fog::{FogTime, LayerParams, noise};

    fn every_preset() -> Vec<Vec<FogLayer>> {
        vec![
            vec![clouds()],
            vec![valley()],
            vec![valley(), clouds()],
            vec![blanket(0, 256, 40.0)],
            sunset(),
        ]
    }

    #[test]
    fn the_presets_are_valid() {
        for layers in every_preset() {
            assert_eq!(
                boxel_fog::validate(&layers, boxel_fog::DEFAULT_MAX_STEPS),
                Ok(()),
                "{layers:?}"
            );
        }
    }

    /// No preset shades by faces, every one is soft near the camera and fills gradually.
    #[test]
    fn the_presets_are_soft() {
        for layer in every_preset().into_iter().flatten() {
            assert_eq!(layer.face_shade, [1.0; 4], "{layer:?}");
            assert!(layer.soft_distance >= 8.0, "{layer:?}");
            assert!(layer.softness >= 0.2, "{layer:?}");
        }
        let c = clouds();
        assert_eq!(c.noise.dims, NoiseDims::Volume, "volumetric clouds");
        assert!((24..=32).contains(&(c.max_y - c.min_y)));
        assert!(c.cell.iter().all(|e| (6..=8).contains(e)));
        assert!(c.threshold_rise > 0.0 && c.height_shade[0] < c.height_shade[1]);
    }

    /// As the wind moves the noise, no cell of any preset changes its coverage by more than
    /// 1.6 % in a tick (64 a second): nothing pops; between ticks the clock interpolates.
    #[test]
    fn no_preset_cell_jumps_in_a_tick() {
        for layer in every_preset().into_iter().flatten() {
            let rows = (layer.max_y - layer.min_y) / i64::from(layer.cell[1]);
            let coverages = |tick: u64| {
                let p = LayerParams::new(&layer, [0, layer.min_y, 0], FogTime::from(tick));
                let mut out = Vec::new();
                for x in -16..16 {
                    for y in 0..rows.min(8) as i32 {
                        for z in -16..16 {
                            out.push(noise::coverage(&p, [x, y, z]).coverage);
                        }
                    }
                }
                out
            };
            let mut worst = 0;
            let mut prev = coverages(1000);
            for tick in 1001..1300 {
                let now = coverages(tick);
                for (a, b) in prev.iter().zip(&now) {
                    worst = worst.max(a.abs_diff(*b));
                }
                prev = now;
            }
            assert!(worst <= 64, "{worst} of 4096 in a tick: {layer:?}");
        }
    }
}
