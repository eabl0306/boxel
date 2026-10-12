//! The band's edges (`FogLayer::edge_bottom`, `edge_top`): near a plane the threshold rises to
//! never covered, so no column of cells reaches it and the soft fog fades out before it, far
//! from the origin too (ADR 0014); the outline is an uneven skyline of cubes; the empty-step
//! skip and the tail past the budget lose no fog and invent none.
use boxel_fog::march::{MarchLayer, Ray, band_chord, march, walk};
use boxel_fog::noise::{self, coverage, point_edge, row_edge};
use boxel_fog::{FogFade, FogLayer, HazeProfile, LayerParams, NoiseDims, haze_profile};
use boxel_voxel::MAX_BLOCK_COORD;
use proptest::prelude::*;

/// A noisy bank over `[min_y, max_y)` with the given cells and edges, drawn far.
fn bank(min_y: i64, max_y: i64, cell: [u32; 3], edges: (u32, u32)) -> FogLayer {
    let mut l = FogLayer::new(min_y, max_y);
    l.cell = cell;
    l.noise.dims = NoiseDims::Volume;
    l.noise.scale = 4;
    l.noise.octaves = 2;
    l.noise.seed = 77;
    l.threshold = 0.3;
    l.softness = 0.3;
    l.density = 0.4;
    (l.edge_bottom, l.edge_top) = edges;
    l.max_distance = 1024.0;
    l.fade = FogFade::Range {
        start: 1000.0,
        end: 1024.0,
    };
    l
}

fn origin_strategy() -> impl Strategy<Value = i64> {
    let centers = [
        0i64,
        1 << 31,
        -(1 << 31),
        1 << 40,
        -(1 << 40),
        MAX_BLOCK_COORD - 5000,
        -MAX_BLOCK_COORD + 5000,
    ];
    (0..centers.len(), -5000i64..5000).prop_map(move |(i, off)| centers[i] + off)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// No column reaches a plane that has an edge: the cell row along it is never covered, the
    /// soft fog has nothing in the last half cell before it, and a ray that stays in that half
    /// cell (cubic or soft, inside its budget or past it) crosses no fog at all. With threshold
    /// 0 and the softest ramp, every other cell would be full.
    #[test]
    fn no_column_reaches_a_plane_with_an_edge(
        ox in origin_strategy(), oz in origin_strategy(),
        oy in -64i64..64,
        cell in (1u32..=8, 1u32..=8, 1u32..=8),
        rows in 2i64..12,
        edges in (1u32..=64, 1u32..=64),
        two in any::<bool>(), volume in any::<bool>(), seed in any::<u64>(),
        soft in prop_oneof![Just(0.0f32), 4.0f32..=32.0],
        budget in prop_oneof![Just(256u32), 1u32..8],
        tick in 0u64..1_000_000,
        yaw in 0.0f32..std::f32::consts::TAU,
        inset in 0.01f32..0.49,
    ) {
        let cell = [cell.0, cell.1, cell.2];
        let cy = i64::from(cell[1]);
        let mut layer = bank(0, rows * cy, cell, edges);
        layer.noise.dims = if volume { NoiseDims::Volume } else { NoiseDims::Flat };
        layer.noise.octaves = if two { 2 } else { 1 };
        layer.noise.seed = seed;
        layer.threshold = 0.0;
        layer.softness = 1.0;
        layer.soft_distance = soft;
        let origin = [ox, oy, oz];
        let p = LayerParams::new(&layer, origin, tick);
        for (x, z) in [(-7, 3), (0, 0), (40, -90), (1000, 2000)] {
            prop_assert_eq!(coverage(&p, [x, p.band_cells[0], z]).coverage, 0);
            prop_assert_eq!(coverage(&p, [x, p.band_cells[1] - 1, z]).coverage, 0);
        }
        let ml = MarchLayer::new(&layer, origin, tick, [1.0; 3], [0.0, 1.0], haze_profile(&layer));
        let half = cell[1] as f32 * 0.5;
        let [lo, hi] = p.band;
        let dir = [yaw.sin(), 0.0, yaw.cos()];
        for y in [hi - half * inset * 2.0, lo + half * inset * 2.0] {
            prop_assert_eq!(point_edge(&p, y), 4096);
            prop_assert_eq!(ml.haze_at(y), 0.0);
            let eye = [0.3, y, 0.7];
            prop_assert_eq!(ml.soft_coverage([5.1, y, -2.3]), 0.0);
            let r = march(&Ray { eye, dir, depths: [f32::INFINITY; 4] }, &[ml], [0.0; 3], budget);
            prop_assert_eq!(r.transmittance, [1.0; 4], "y {} of {:?}", y, p.band);
        }
        // Inside, past the edges, the same bank has fog.
        let mid = 0.5 * (lo + hi);
        let mid_row = ((mid - p.grid[1]) / cell[1] as f32).floor() as i32;
        if row_edge(&p, mid_row) == 0 {
            let r = march(
                &Ray { eye: [0.3, mid, 0.7], dir, depths: [f32::INFINITY; 4] },
                &[ml],
                [0.0; 3],
                256,
            );
            prop_assert!(r.transmittance[0] < 1.0);
        }
    }

    /// The edge of a cell row is the soft fog's at the row's middle, bit for bit, and the
    /// threshold the cells get is the one the soft fog reads there.
    #[test]
    fn a_row_middle_has_its_row_edge(
        oy in origin_strategy(),
        cy in 1u32..=32,
        rows in 1i64..40,
        edges in (0u32..=200, 0u32..=200),
        dy in -50i32..50,
    ) {
        let mut layer = bank(0, rows * i64::from(cy), [2, cy, 2], edges);
        let c0 = oy.div_euclid(i64::from(cy));
        (layer.min_y, layer.max_y) = ((c0 - rows / 2) * i64::from(cy), (c0 + rows - rows / 2) * i64::from(cy));
        let p = LayerParams::new(&layer, [0, oy, 0], 0);
        let middle = p.grid[1] + (dy as f32 + 0.5) * cy as f32;
        prop_assert_eq!(point_edge(&p, middle), row_edge(&p, dy), "row {}", dy);
    }
}

/// The highest covered row of a column takes many heights, and each row near the top is
/// thinner than the one below it: an uneven skyline of cubes, not a plane. The rows the edges
/// leave alone keep the plain layer's cells.
#[test]
fn the_edges_make_an_uneven_skyline() {
    let plain = bank(0, 32, [2, 2, 2], (0, 0));
    let edged = bank(0, 32, [2, 2, 2], (4, 12));
    let (pp, pe) = (
        LayerParams::new(&plain, [0; 3], 0),
        LayerParams::new(&edged, [0; 3], 0),
    );
    let mut tops = std::collections::BTreeMap::new();
    let mut per_row = [0u32; 16];
    for x in -150..150 {
        for z in -150..150 {
            let mut top = None;
            for (r, count) in per_row.iter_mut().enumerate() {
                let r = r as i32;
                let c = coverage(&pe, [x, r, z]).coverage;
                if (2..10).contains(&r) {
                    assert_eq!(c, coverage(&pp, [x, r, z]).coverage, "row {r} has no edge");
                }
                if c > 0 {
                    top = Some(r);
                    *count += 1;
                }
            }
            if let Some(t) = top {
                *tops.entry(t).or_insert(0u32) += 1;
            }
        }
    }
    assert_eq!(per_row[0], 0, "the floor's row");
    assert_eq!(per_row[15], 0, "the ceiling's row");
    assert!(
        per_row[9..15].windows(2).all(|w| w[0] > w[1]) && per_row[14] > 0,
        "thinner toward the ceiling: {per_row:?}"
    );
    assert!(
        per_row[1] < per_row[2],
        "thinner toward the floor: {per_row:?}"
    );
    // Columns end at many heights; the most common one holds under half of them.
    let columns: u32 = tops.values().sum();
    assert!(tops.len() >= 6, "{tops:?}");
    assert!(
        tops.values().all(|&n| n * 2 < columns),
        "one height dominates: {tops:?}"
    );
}

/// The skip of empty lattice steps still loses no fog with edges and a rise: the march keeps
/// what the cells of a plain walk give, and it skips steps the plain bound would walk.
#[test]
fn skipping_with_edges_loses_no_fog() {
    let mut layer = bank(-24, 24, [2, 2, 2], (8, 16));
    layer.threshold = 0.5;
    layer.threshold_rise = 0.3;
    layer.max_distance = 200.0;
    let ml = MarchLayer::new(
        &layer,
        [0; 3],
        0,
        layer.color,
        [0.0, 1.0],
        HazeProfile::level(&layer, 0.1),
    );
    let p = &ml.params;
    let mut raised = 0;
    for (eye, dir) in [
        ([0.5f32, 0.5, 0.5], [0.3f32, 0.05, 1.0]),
        ([0.5, 18.5, 0.5], [-1.0, -0.02, 0.4]),
        ([0.5, -20.5, 0.5], [0.7, 0.01, -1.0]),
        ([0.5, 23.0, 0.5], [0.2, -0.1, 1.0]),
    ] {
        let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        let (t0, t1) = band_chord(eye[1], dir[1], p.band).expect("chord");
        let t1 = t1.min(ml.cells_end / len);
        let mut tau = 0.0;
        for v in walk(eye, dir, p, t0, t1, 100_000) {
            let cov = coverage(p, v.cell).coverage as f32 / 4096.0;
            tau += p.density_log2e * cov * (v.t1 - v.t0) * len;
            let bound = noise::step_bound(p, v.cell);
            if bound.is_empty(p) && coverage_from_noise_at(&bound, p) {
                raised += 1;
            }
        }
        let r = march(
            &Ray {
                eye,
                dir,
                depths: [f32::INFINITY; 4],
            },
            &[ml],
            [0.0; 3],
            256,
        );
        let want = (-tau).exp2();
        assert!(
            (r.transmittance[0] - want).abs() < 2e-4
                || (want < 0.003 && r.transmittance[0] < 0.003),
            "{eye:?} {dir:?}: {} vs {want}",
            r.transmittance[0]
        );
        assert!(r.steps < 256, "{dir:?}: {} steps", r.steps);
    }
    assert!(
        raised > 0,
        "the raised threshold skips steps the band's bottom would not"
    );
}

/// Whether the band's bottom threshold alone would have walked the step.
fn coverage_from_noise_at(bound: &noise::StepBound, p: &LayerParams) -> bool {
    noise::coverage_from_noise(bound.max_noise, p.threshold, p.gain) > 0
}

/// Near the camera the soft fog integrates the edged noise along the ray (no plane where the
/// band ends), and the tail past the budget thins near the planes like the cells: a ray along
/// the top of the band sees about what a fine walk of the cells gives, not the band's mean.
#[test]
fn the_soft_fog_and_the_tail_follow_the_edges() {
    let mut layer = bank(0, 32, [2, 2, 2], (4, 16));
    layer.soft_distance = 16.0;
    let ml = MarchLayer::new(
        &layer,
        [0; 3],
        0,
        [1.0; 3],
        [0.0, 1.0],
        haze_profile(&layer),
    );
    // The soft fog from inside, looking up and out through the top edge: a fine integral.
    let eye = [0.3, 22.7, 0.2];
    for dir in [[0.3f32, 0.2, 1.0], [-1.0, 0.1, 0.4], [0.2, 0.9, -0.3]] {
        let l = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        let dir = dir.map(|v| v / l);
        let depth = 12.0;
        let r = march(
            &Ray {
                eye,
                dir,
                depths: [depth; 4],
            },
            &[ml],
            [0.0; 3],
            256,
        );
        let n = 12_000;
        let dt = depth / n as f32;
        let tau: f32 = (0..n)
            .map(|i| {
                let t = (i as f32 + 0.5) * dt;
                let pos = std::array::from_fn(|a| eye[a] + t * dir[a]);
                ml.params.density_log2e * ml.soft_coverage(pos) * dt
            })
            .sum();
        let want = (-tau).exp2();
        assert!(
            (r.transmittance[0] - want).abs() < 3e-3,
            "{dir:?}: {} vs {want}",
            r.transmittance[0]
        );
    }
    // A cell's middle reads its cell, edges and all.
    let p = &ml.params;
    for x in -10..10 {
        for y in 0..16 {
            let d = [x, y, 3 - x];
            let pos = std::array::from_fn(|a| p.grid[a] + (d[a] as f32 + 0.5) * 2.0);
            assert_eq!(
                ml.soft_coverage(pos),
                coverage(p, d).coverage as f32 / 4096.0,
                "{d:?}"
            );
        }
    }
    // The tail (the profile): the core's mean deep inside, thinner up the edge, nothing in the
    // row along the plane.
    let core = ml.profile.knots[1];
    assert_eq!(ml.profile.knots[1..4], [core; 3], "{:?}", ml.profile);
    assert_eq!(ml.haze_at(8.0), core);
    let up: Vec<f32> = [16.5f32, 20.0, 24.0, 28.0, 29.5]
        .map(|y| ml.haze_at(y))
        .to_vec();
    assert!(
        up.windows(2).all(|w| w[0] >= w[1]) && up[4] < up[0],
        "{up:?}"
    );
    assert_eq!(ml.haze_at(30.5), 0.0);
    assert_eq!(ml.haze_at(31.5), 0.0);
    // Rays along the top rows with a tiny budget: the tail gives about what the cells give,
    // far less than a tail without edges.
    let cubic = MarchLayer {
        soft: [0.0; 2],
        ..ml
    };
    let flat = MarchLayer {
        profile: HazeProfile {
            knots: [core; 8],
            ..ml.profile
        },
        ..cubic
    };
    // Optical depth along rows at height `y`, summed over 64 rays.
    let along = |layer: &MarchLayer, y: f32, budget| -> f32 {
        (0..64)
            .map(|i| {
                let a = i as f32 * 0.37;
                march(
                    &Ray {
                        eye: [0.3 + 17.0 * i as f32, y, 0.7 - 9.0 * i as f32],
                        dir: [a.sin(), -0.001, a.cos()],
                        depths: [300.0; 4],
                    },
                    std::slice::from_ref(layer),
                    [0.0; 3],
                    budget,
                )
                .transmittance[0]
                    .ln()
            })
            .sum()
    };
    for y in [23.5f32, 25.5, 27.5] {
        let (cells, tail, plain) = (
            along(&cubic, y, 256),
            along(&cubic, y, 4),
            along(&flat, y, 4),
        );
        eprintln!(
            "y {y}: optical depth of the cells {cells}, the tail {tail}, without edges {plain}"
        );
        assert!(
            tail > 2.0 * cells - 2.0 && tail < 0.5 * cells + 2.0,
            "y {y}: the tail {tail} is not near the cells' {cells}"
        );
        assert!(plain < 3.0 * tail - 1.0, "y {y}: the edges thin the tail");
    }
}
