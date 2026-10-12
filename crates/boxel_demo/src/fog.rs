//! Fog and cloud presets of the demo world (issue #47, ADR 0030): the look of the example game,
//! not the engine's. The terrain sits between y = 16 and 112 with the sea's top water block at
//! 46 ([`crate::worldgen`]), so the valley fog lies on the sea and the low ground and the clouds
//! float well above every hill.
//!
//! Every preset is soft: no face shading (fog has no faces), cells that fill and fade as the
//! wind moves the noise, soft fog without cell edges near the camera, and edges that thin the
//! cells toward the band's floor and ceiling, so no layer ends in a flat plane.
//!
//! The far presets (ADR 0032) go on past their cells as a haze of their mean density, as far as
//! the terrain is drawn: [`hazy_valley`] and the big clouds ([`cumulus16`], [`cumulus20`],
//! [`cumulus24`], [`high_deck`] and [`decks`]), which take the far LOD's horizon.

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

/// The horizon the far cloud presets take without the far LOD: like [`clouds`], they fade out
/// from half of it and end there with their cells.
pub const NO_LOD_HORIZON: f32 = 768.0;

/// What the far cloud presets share: cubes that fill and fade with the wind, a gray base and a
/// lit top, soft within 32 blocks when flying through, and past their cells a haze of their mean
/// density at each height up to `horizon` (the far LOD's, or [`NO_LOD_HORIZON`]), faded into the
/// sky over its second half. Their cells end 192 cells away (counted on their smallest edge),
/// where a cell is some 4 pixels wide at 1080p and farther would only shimmer in the wind; the
/// last quarter of them blends into the haze.
fn far_clouds(min_y: i64, max_y: i64, horizon: f32) -> FogLayer {
    FogLayer {
        threshold: 0.55,
        softness: 0.25,
        height_shade: [0.55, 1.0],
        soft_distance: 32.0,
        haze: 1.0,
        fade: FogFade::Range {
            start: 0.5 * horizon,
            end: horizon,
        },
        ..FogLayer::new(min_y, max_y)
    }
}

/// Big cumulus for the far LOD: 16-block cubes in a band from y 176 to 320 that fill 208 to 288,
/// five rows, a few reaching down to 192 (twice today's cubes, cloud masses four times as wide
/// over a 128-block lattice). The row along each plane is always empty and the next one is
/// raised halfway (2 rows of edge at both ends), so the base is uneven but fairly flat; a
/// threshold that rises gently along the height thins the upper rows into domes over a wide
/// base. Dense enough that one cube crossed face to face is about 95 % opaque, so a ray that
/// meets a cloud stops after a few cubes. Cells to 3,072 blocks, then the haze to `horizon`;
/// drifting 4 blocks a second.
pub fn cumulus16(horizon: f32) -> FogLayer {
    FogLayer {
        cell: [16, 16, 16],
        noise: FogNoise {
            scale: 8,
            octaves: 2,
            dims: NoiseDims::Volume,
            seed: 0xc016,
        },
        threshold_rise: 0.15,
        edge_bottom: 32,
        edge_top: 32,
        density: 0.18,
        wind: [4.0, 0.0, 1.0],
        max_distance: 3072.0,
        ..far_clouds(176, 320, horizon)
    }
}

/// Like [`cumulus16`] with 20-block cubes in a band from y 160 to 340 that fill 200 to 300 (the
/// owner's range), five rows, over a 160-block lattice; cells to 3,840 blocks, drifting 5
/// blocks a second.
pub fn cumulus20(horizon: f32) -> FogLayer {
    FogLayer {
        cell: [20, 20, 20],
        noise: FogNoise {
            scale: 8,
            octaves: 2,
            dims: NoiseDims::Volume,
            seed: 0xc020,
        },
        threshold_rise: 0.15,
        edge_bottom: 40,
        edge_top: 40,
        density: 0.15,
        wind: [5.0, 0.0, 1.5],
        max_distance: 3840.0,
        ..far_clouds(160, 340, horizon)
    }
}

/// The biggest cubes and the cheapest steps: 24-block cubes in a band from y 144 to 336 that
/// fill 192 to 288, four rows, some reaching down to 168, over a 192-block lattice of one octave
/// (smoother, rounder masses); cells to 4,608 blocks, drifting 6 blocks a second.
pub fn cumulus24(horizon: f32) -> FogLayer {
    FogLayer {
        cell: [24, 24, 24],
        noise: FogNoise {
            scale: 8,
            octaves: 1,
            dims: NoiseDims::Volume,
            seed: 0xc024,
        },
        threshold_rise: 0.15,
        edge_bottom: 48,
        edge_top: 48,
        density: 0.12,
        wind: [6.0, 0.0, 2.0],
        max_distance: 4608.0,
        ..far_clouds(144, 336, horizon)
    }
}

/// A thin, pale high deck: flat cells 32 blocks wide and 8 tall from y 400 to 440 (filled
/// between 408 and 432), over columns of 2D noise on a 512-block lattice, light and faint;
/// drifting 8 blocks a second. Seen edge-on its 8-block rows are what shows, so its cells end
/// 192 rows away (1,536 blocks), then the haze to `horizon`; farther the rows were thinner than
/// a texel and broke into dots.
pub fn high_deck(horizon: f32) -> FogLayer {
    FogLayer {
        cell: [32, 8, 32],
        noise: FogNoise {
            scale: 16,
            octaves: 1,
            dims: NoiseDims::Flat,
            seed: 0xdec4,
        },
        threshold: 0.6,
        edge_bottom: 16,
        edge_top: 16,
        softness: 0.3,
        density: 0.05,
        color: [0.97, 0.97, 1.0],
        height_shade: [0.85, 1.0],
        wind: [8.0, 0.0, 3.0],
        max_distance: 1536.0,
        ..far_clouds(400, 440, horizon)
    }
}

/// Two cloud decks at their own heights and winds: [`cumulus16`] under [`high_deck`].
pub fn decks(horizon: f32) -> Vec<FogLayer> {
    vec![cumulus16(horizon), high_deck(horizon)]
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

    /// The far LOD's far horizon (`--lod far`).
    const FAR_HORIZON: f32 = 12_288.0;

    /// The far presets at `horizon`, as the examples put them together: the hazy valley under
    /// each set of far clouds.
    fn far_presets(horizon: f32) -> Vec<(&'static str, Vec<FogLayer>)> {
        let with_valley = |clouds: Vec<FogLayer>| [vec![hazy_valley()], clouds].concat();
        vec![
            ("cumulus16", with_valley(vec![cumulus16(horizon)])),
            ("cumulus20", with_valley(vec![cumulus20(horizon)])),
            ("cumulus24", with_valley(vec![cumulus24(horizon)])),
            ("decks", with_valley(decks(horizon))),
        ]
    }

    fn every_preset() -> Vec<Vec<FogLayer>> {
        let mut presets = vec![
            vec![clouds()],
            vec![valley()],
            vec![valley(), clouds()],
            vec![hazy_valley()],
            vec![hazy_valley(), clouds()],
            vec![blanket(0, 256, 40.0)],
            sunset(),
        ];
        for horizon in [NO_LOD_HORIZON, FAR_HORIZON] {
            presets.extend(far_presets(horizon).into_iter().map(|(_, layers)| layers));
        }
        presets
    }

    /// The far cloud layers at the far horizon.
    fn far_clouds_layers() -> Vec<FogLayer> {
        vec![
            cumulus16(FAR_HORIZON),
            cumulus20(FAR_HORIZON),
            cumulus24(FAR_HORIZON),
            high_deck(FAR_HORIZON),
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
        // The big cumulus: volumetric domes over a gray base, and dense, so a ray that meets a
        // cloud stops after a few cubes (one crossed face to face takes 2.5 nats or more).
        for c in [cumulus16, cumulus20, cumulus24].map(|f| f(FAR_HORIZON)) {
            assert_eq!(c.noise.dims, NoiseDims::Volume, "{c:?}");
            assert!(c.threshold_rise > 0.0, "{c:?}");
            assert!(c.height_shade[0] < c.height_shade[1], "{c:?}");
            assert!(c.density * c.cell[0] as f32 >= 2.5, "{c:?}");
            assert_eq!(c.cell[0], c.cell[1], "cubes: {c:?}");
        }
    }

    /// The far clouds end their cells 192 cells away (on their smallest edge, about 2
    /// half-resolution texels at 1080p) and their haze at the horizon, fading over its second
    /// half; without the LOD (768 blocks) they end like today's clouds.
    #[test]
    fn far_clouds_reach_the_horizon() {
        for c in far_clouds_layers() {
            let smallest = c.cell.iter().min().copied().unwrap_or(1);
            assert_eq!(c.max_distance, 192.0 * smallest as f32, "{c:?}");
            assert_eq!(c.haze, 1.0);
            assert_eq!(
                c.fade,
                FogFade::Range {
                    start: 6144.0,
                    end: FAR_HORIZON
                }
            );
        }
        for c in decks(NO_LOD_HORIZON) {
            assert_eq!(
                c.fade,
                FogFade::Range {
                    start: 384.0,
                    end: 768.0
                },
                "{c:?}"
            );
        }
        assert_eq!(decks(1000.0), vec![cumulus16(1000.0), high_deck(1000.0)]);
    }

    /// The big cumulus fill the heights their docs give (the owner asked for cubes that fill 200
    /// to 300): every knot of their haze profile (the mean coverage of the cell row at its
    /// height, the rise and the edges in it) inside that range holds at least a quarter of the
    /// densest knot, every knot outside it less, and the base is denser than the top (domes).
    #[test]
    fn the_cumulus_fill_their_range() {
        for (layer, range) in [
            (cumulus16(FAR_HORIZON), 208.0..288.0),
            (cumulus20(FAR_HORIZON), 200.0..300.0),
            (cumulus24(FAR_HORIZON), 192.0..288.0),
        ] {
            let p = boxel_fog::haze_profile(&layer);
            let peak = p.knots.iter().copied().fold(0.0, f32::max);
            let bottom = layer.min_y as f32 + p.bottom;
            let mut inside = Vec::new();
            for (k, &knot) in p.knots.iter().enumerate() {
                let y = bottom + k as f32 * p.spacing;
                if range.contains(&y) {
                    assert!(knot >= 0.25 * peak, "{range:?}: knot {k} at y {y}: {p:?}");
                    inside.push(knot);
                } else {
                    assert!(knot < 0.25 * peak, "{range:?}: knot {k} at y {y}: {p:?}");
                }
            }
            assert!(inside.len() >= 4, "{range:?}: {p:?}");
            assert!(inside[0] > inside[inside.len() - 1], "domes: {p:?}");
        }
    }

    /// Today's presets end with their cells; the hazy valley is the valley with its mean
    /// density past them, and none of that haze lies under the sea's surface (y 47).
    #[test]
    fn todays_presets_have_no_haze() {
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
        // Each layer once: the fade and the haze move no cell.
        let mut layers: Vec<FogLayer> = Vec::new();
        for layer in every_preset().into_iter().flatten() {
            let layer = FogLayer {
                fade: FogFade::Scene,
                haze: 0.0,
                ..layer
            };
            if !layers.contains(&layer) {
                layers.push(layer);
            }
        }
        for layer in layers {
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

    /// The steps the reference march takes per ray (the shader's, step for step) for today's
    /// `both`, the far presets and `inside`, over a fixed fan of rays from four poses: the
    /// stress default (y 140, looking 20 degrees down), the horizon (y 140, 8 up), above the
    /// clouds (y 420, 25 down) and in the cloud deck (y 250, level). Each ray goes 30 degrees
    /// below its pose's pitch to 40 above, 0 to 45 degrees to the side, over a flat floor at
    /// y 64 and the sky, with the far LOD's horizon (12,288 blocks) and a view of 512 (stress at
    /// radius 16). Deterministic, not a timing: it tells why a smoke moved. The ceilings are
    /// the first run plus 25 %; raise one only with a reason in the commit.
    /// `tools/quiet.sh cargo test -p boxel_demo fog_steps_per_ray -- --nocapture` prints the
    /// table.
    #[test]
    fn fog_steps_per_ray() {
        use boxel_fog::march::{MarchLayer, Ray, march};
        // The far LOD's fade at its horizon and the view's at 512 blocks: the distance fog's
        // last 10 %, at most 64 blocks.
        let range = |d: f32| [d - (0.1 * d).min(64.0), d];
        let (lod_fade, view) = (range(FAR_HORIZON), 512.0);
        // The fade a Scene layer takes (boxel_render's frame_layers).
        let scene_fade = |l: &FogLayer| {
            if l.haze > 0.0 {
                lod_fade
            } else {
                range(l.max_distance.min(view))
            }
        };
        let mut presets = vec![("both", vec![valley(), clouds()])];
        presets.extend(far_presets(FAR_HORIZON));
        presets.push(("inside", vec![blanket(0, 256, 40.0)]));
        let poses: [(&str, [i64; 3], f32); 4] = [
            ("stress", [16, 140, 16], -20.0),
            ("horizon", [16, 140, 16], 8.0),
            ("above", [16, 420, 16], -25.0),
            ("deck", [16, 250, 16], 0.0),
        ];
        // Mean steps and mean pieces (soft and haze) per ray, at most: the first run plus 25 %.
        let ceilings: [[(f32, f32); 4]; 6] = CEILINGS;
        let sky = [0.6, 0.75, 1.0];
        let eye = [0.5f32; 3];
        let mut table = String::from(
            "| preset | pose | mean steps | p90 | max | capped | soft pieces | haze pieces |\n\
             |---|---|---:|---:|---:|---:|---:|---:|\n",
        );
        let mut over = Vec::new();
        for ((name, layers), ceilings) in presets.iter().zip(ceilings) {
            let mut sorted = layers.clone();
            sorted.sort_by_key(|l| l.min_y);
            for ((pose, origin, pitch), (max_steps, max_pieces)) in poses.iter().zip(ceilings) {
                let march_layers: Vec<MarchLayer> = sorted
                    .iter()
                    .map(|l| {
                        let profile = boxel_fog::haze_profile(l);
                        MarchLayer::new(l, *origin, 1000u64, l.color, scene_fade(l), profile)
                    })
                    .collect();
                let (mut steps, mut soft, mut haze, mut capped) = (Vec::new(), 0u32, 0u32, 0u32);
                for elevation in (-30..=40).step_by(2) {
                    for azimuth in (0..=45).step_by(5) {
                        let yaw = (-90.0 + azimuth as f32).to_radians();
                        let pitch = (pitch + elevation as f32).clamp(-89.0, 89.0).to_radians();
                        let dir = [
                            -yaw.sin() * pitch.cos(),
                            pitch.sin(),
                            -yaw.cos() * pitch.cos(),
                        ];
                        // The ray is its own view axis: its depths are distances.
                        let floor = if dir[1] < 0.0 {
                            (64.0 - origin[1] as f32 - eye[1]) / dir[1]
                        } else {
                            f32::INFINITY
                        };
                        let ray = Ray {
                            eye,
                            dir,
                            depths: [floor; 4],
                        };
                        let r = march(&ray, &march_layers, sky, boxel_fog::DEFAULT_MAX_STEPS);
                        steps.push(r.steps);
                        soft += r.soft_pieces;
                        haze += r.haze_pieces;
                        capped += u32::from(r.capped);
                    }
                }
                let n = steps.len() as f32;
                steps.sort_unstable();
                let mean = steps.iter().sum::<u32>() as f32 / n;
                let p90 = steps[steps.len() * 9 / 10];
                let max = steps[steps.len() - 1];
                let (soft, haze) = (soft as f32 / n, haze as f32 / n);
                table.push_str(&format!(
                    "| {name} | {pose} | {mean:.1} | {p90} | {max} | {:.0} % | {soft:.1} | \
                     {haze:.1} |\n",
                    100.0 * capped as f32 / n
                ));
                if mean > max_steps || soft + haze > max_pieces {
                    over.push(format!(
                        "{name} {pose}: {mean:.1} steps (at most {max_steps}), {:.1} pieces (at \
                         most {max_pieces})",
                        soft + haze
                    ));
                }
            }
        }
        println!("{table}");
        assert!(over.is_empty(), "over the ceilings: {over:#?}\n{table}");
    }

    /// The ceilings of `fog_steps_per_ray`, per preset (both, cumulus16, cumulus20, cumulus24,
    /// decks, inside) and pose (stress, horizon, above, deck): mean steps and mean pieces per
    /// ray, the first run plus 25 %, at least half a piece. `both` and `inside` from 2026-10-11;
    /// the cumulus presets and `decks` from their retune to fill 200 to 300 (taller bands, the
    /// lowest of them nearer the poses at y 140).
    const CEILINGS: [[(f32, f32); 4]; 6] = [
        [(20.3, 0.5), (26.2, 0.5), (14.9, 0.5), (22.3, 0.8)],
        [(24.4, 0.5), (31.0, 0.9), (31.4, 1.9), (14.2, 66.3)],
        [(23.9, 0.5), (32.3, 8.9), (29.2, 1.9), (34.2, 61.7)],
        [(20.4, 9.4), (27.5, 28.4), (26.8, 1.4), (27.5, 62.3)],
        [(25.7, 1.2), (34.4, 1.5), (40.8, 52.2), (18.0, 66.3)],
        [(80.0, 36.2), (80.0, 36.2), (39.4, 1.7), (48.3, 31.2)],
    ];
}
