//! Fog and cloud presets of the demo world (issue #47, ADR 0030): the look of the example game,
//! not the engine's. The terrain sits between y = 16 and 112 with the sea's top water block at
//! 46 ([`crate::worldgen`]), so the valley fog lies on the sea and the low ground and the clouds
//! float well above every hill.
//!
//! Every preset is soft: no face shading (fog has no faces), cells that fill and fade as the
//! wind moves the noise, soft fog without cell edges near the camera, and edges that thin the
//! cells toward the band's floor and ceiling, so no layer ends in a flat plane.

use boxel_fog::{FogFade, FogLayer, FogNoise, NoiseDims, density_for_visibility};

/// Volumetric cumulus clouds: 8-block cubes in a 64-block band at y 176..240 over 3D noise,
/// most of them between 192 and 224. A 28-block top edge gives uneven, rounded tops and a
/// 20-block bottom edge uneven undersides, a bit flatter than the tops (with a gentle rise of
/// the threshold the base stays the widest part); the base is gray and the top lit. Cells fill
/// and fade as they drift 2 blocks a second along +x, the edges of a cloud thinner than its
/// core; soft within 16 blocks when flying through; faded out by 768 blocks.
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
        threshold_rise: 0.2,
        edge_bottom: 20,
        edge_top: 28,
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
        ..FogLayer::new(176, 240)
    }
}

/// Valley fog: banks of 2-block cubes from y 46 to 70, on the sea and the low ground around it.
/// Thickest from 50 to 58 (seen about 40 blocks through), thinning over a 12-block top edge into
/// an uneven skyline of cubes and over a 4-block bottom edge. The row along the band's floor
/// (46 to 48) is always empty and the soft fog ends half a cell above it, so no fog lies under
/// the sea's surface (its water ends at 47). Thinner at the edges of a bank, soft within 12
/// blocks, taking a third of the sky's color, drifting slowly.
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
        edge_bottom: 4,
        edge_top: 12,
        softness: 0.3,
        density: density_for_visibility(40.0),
        color: [0.92, 0.94, 1.0],
        sky_tint: 0.35,
        soft_distance: 12.0,
        wind: [0.6, 0.0, 0.3],
        max_distance: 384.0,
        ..FogLayer::new(46, 70)
    }
}

/// The valley fog going on past its cubes: from 384 blocks on, a haze of its mean density at
/// each height lies on the sea and the low ground as far as the terrain is drawn (the far LOD's
/// horizon while it draws), fading with it. The cubes blend into it from 288 blocks. Seen from
/// a hill, the far lowlands become a white sheet with the hills standing out of it.
pub fn hazy_valley() -> FogLayer {
    FogLayer {
        haze: 1.0,
        ..valley()
    }
}

/// A blanket of 2-block cubes filling `[min_y, max_y)` (both even), seen about `visibility`
/// blocks through where it is thickest, soft within 12 blocks, with edges like the valley's: a
/// quarter of its height at the top and an eighth at the bottom (at most 16 and 4 blocks).
/// Every cell away from the edges has some fog, of a density that varies with 3D noise, so no
/// lattice step there can be skipped and every ray inside walks the most cells and the soft
/// fog's pieces: the worst case. (A blanket of equal cells would be cheap: the march takes it
/// whole.)
pub fn blanket(min_y: i64, max_y: i64, visibility: f32) -> FogLayer {
    let height = u32::try_from(max_y.saturating_sub(min_y)).unwrap_or(u32::MAX);
    FogLayer {
        cell: [2, 2, 2],
        noise: FogNoise {
            scale: 4,
            octaves: 1,
            dims: NoiseDims::Volume,
            seed: 0xb1a2,
        },
        threshold: 0.0,
        edge_bottom: (height / 8).min(4),
        edge_top: (height / 4).min(16),
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
            vec![hazy_valley()],
            vec![hazy_valley(), clouds()],
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
        assert!(c.cell.iter().all(|e| (6..=8).contains(e)));
        assert!(c.threshold_rise > 0.0 && c.height_shade[0] < c.height_shade[1]);
    }

    /// Today's presets end with their cells; the hazy valley is the valley with its mean
    /// density past them, and none of that haze lies under the sea's surface (y 47).
    #[test]
    fn only_the_hazy_valley_has_haze() {
        for layer in [clouds(), valley(), blanket(0, 256, 40.0)]
            .into_iter()
            .chain(sunset())
        {
            assert_eq!(layer.haze, 0.0, "{layer:?}");
        }
        let hazy = hazy_valley();
        assert_eq!(hazy.haze, 1.0);
        assert_eq!(FogLayer { haze: 0.0, ..hazy }, valley());
        let profile = boxel_fog::haze_profile(&hazy_valley());
        let lo = hazy.min_y as f32 + profile.bottom;
        assert_eq!(lo, 48.0);
        for y in [40.0, 46.0, 47.0, 47.9] {
            assert_eq!(profile.at(lo, y), 0.0, "haze at y {y}");
        }
        assert!(profile.at(lo, 52.0) > 0.0);
    }

    /// No preset ends in a plane: every band has edges of at least two cells at both ends, the
    /// bottom one no deeper than the top one, and a core that the edges leave alone. The valley's
    /// top edge is half its band and no fog lies under the sea's surface (y 47); the clouds keep
    /// a core of two rows of cubes.
    #[test]
    fn no_preset_ends_in_a_plane() {
        for layer in every_preset().into_iter().flatten() {
            let cy = layer.cell[1];
            let height = (layer.max_y - layer.min_y) as u32;
            assert!(
                layer.edge_bottom >= 2 * cy && layer.edge_top >= 2 * cy,
                "{layer:?}"
            );
            assert!(layer.edge_bottom <= layer.edge_top, "{layer:?}");
            assert!(
                layer.edge_bottom + layer.edge_top + cy <= height,
                "a core: {layer:?}"
            );
        }
        let v = valley();
        assert_eq!(2 * v.edge_top, (v.max_y - v.min_y) as u32);
        let p = LayerParams::new(&v, [0; 3], 0);
        for y in [40.0f32, 46.0, 46.9, 47.0] {
            assert_eq!(noise::point_edge(&p, y), 4096, "soft fog at y {y}");
        }
        let floor_row = p.band_cells[0];
        assert_eq!(
            noise::row_threshold(&p, floor_row),
            4096,
            "the row 46 to 48"
        );
        let c = clouds();
        assert!(c.edge_bottom < c.edge_top, "a flatter base than top");
        assert!((c.max_y - c.min_y) as u32 - c.edge_bottom - c.edge_top >= 2 * c.cell[1]);
        assert_eq!(sunset()[0].edge_top, v.edge_top);
        let b = blanket(0, 256, 40.0);
        assert_eq!((b.edge_bottom, b.edge_top), (4, 16));
        let thin = blanket(0, 8, 40.0);
        assert_eq!((thin.edge_bottom, thin.edge_top), (1, 2));
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
