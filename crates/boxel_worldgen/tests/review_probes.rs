//! The adversarial probes of the WG1 review (#67), kept as regression tests: public API only,
//! each `p*` test states what it probes. P1, P2, P3b and P4 failed before the review fixes; P3
//! takes `P3_SEEDS` (default 400) and `P3_LARGE`, P5 `P5_SEEDS`, for longer runs.
use boxel_voxel::MAX_BLOCK_COORD;
use boxel_worldgen::exact::{ExactPoint, MAX_SPREAD, circumcenter, dist_sq, orient};
use boxel_worldgen::field::{
    Add, Blend, Const, Field, LatticeField, Min, NoiseField, Q16, Rect, Slope, Spline, Splined,
    Warped, corner_values,
};
use boxel_worldgen::noise::{
    Basis, Cellular, Fbm, Frequency, Site, SiteId, Warp, gradient_fbm_2d, splitmix64,
};
use std::panic::{AssertUnwindSafe, catch_unwind};

fn fbm(den: u32, octaves: u32, gain: f64) -> Fbm {
    Fbm {
        frequency: Frequency::one_in(den),
        octaves,
        lacunarity: 2,
        gain,
    }
}

// ---------------------------------------------------------------------------------------------
// P1. Spline::new accepts knots at +-MAX_KNOT (doc: "past 2^62" refused), but `x1 - x0` is an
// i64 subtraction: -2^62 .. 2^62 is 2^63 and overflows (panic in debug, wrap in release).
#[test]
fn p1_spline_accepts_knots_it_cannot_subtract() {
    let k = Spline::MAX_KNOT;
    let built = catch_unwind(|| Spline::new(&[(-k, 0), (k, 1)]));
    assert!(
        built.is_ok(),
        "Spline::new panicked on knots within MAX_KNOT"
    );
    let s = built.unwrap();
    let eval = catch_unwind(AssertUnwindSafe(|| s.eval(0)));
    assert!(
        eval.is_ok(),
        "Spline::eval panicked between knots within MAX_KNOT"
    );
}

// ---------------------------------------------------------------------------------------------
// P2. circumcenter() accepts any three points within MAX_SPREAD of `a`, but ExactPoint::rounded
// `expect`s that the center fits in i64 ("a circumcenter of points in range fits in i64").
// Nearly collinear points in range have centers near 2^91.
#[test]
fn p2_rounded_circumcenter_of_points_in_range() {
    let m = MAX_SPREAD;
    let (a, b, c) = ((0, 0), (m, m - 1), (-(m - 1), -(m - 2)));
    assert_ne!(orient(a, b, c), 0);
    let v: ExactPoint = circumcenter(a, b, c).expect("not collinear");
    let r = catch_unwind(|| v.rounded());
    assert!(r.is_ok(), "rounded() panics on {v:?}");
}

// ---------------------------------------------------------------------------------------------
// P3. voronoi_cell's window radius: window_radius bounds a Voronoi vertex's distance to its
// nearest site by sqrt(2) (S - 1 - m), the integer-point bound, but vertices are real points
// (up to sqrt(2) (S - m)). For these (shift, margin) the code's window is one smaller than the
// real-point argument needs. Check every vertex against a wide window by exact rationals.
fn vertex_is_empty(v: ExactPoint, a: Site, others: &[Site]) -> Option<Site> {
    // |v - p|^2 * den^2 for p near the origin (small numbers: cells near the origin).
    let d = |p: (i64, i64)| {
        let dx = v.nx - i128::from(p.0) * v.den;
        let dz = v.nz - i128::from(p.1) * v.den;
        dx * dx + dz * dz
    };
    let r = d(a.pos());
    others.iter().copied().find(|s| d(s.pos()) < r)
}

#[test]
fn p3_voronoi_window_on_small_cells() {
    let small = [
        (0u32, 0i64),
        (1, 0),
        (2, 0),
        (2, 1),
        (3, 1),
        (4, 2),
        (5, 5),
        (6, 24),
        (7, 48),
    ];
    let large = [
        (10u32, 175i64),
        (10, 387),
        (11, 351),
        (12, 702),
        (14, 6204),
        (15, 12408),
    ];
    let cases: Vec<(u32, i64)> = if std::env::var("P3_LARGE").is_ok() {
        large.to_vec()
    } else {
        small.to_vec()
    };
    let mut failures = Vec::new();
    for (shift, margin) in cases {
        let seeds: u64 = std::env::var("P3_SEEDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(400);
        for seed in 0..seeds {
            let c = Cellular::new(splitmix64(seed), shift, margin);
            for i in -3..3 {
                for j in -3..3 {
                    let id = SiteId { i, j };
                    let Ok(cell) = catch_unwind(AssertUnwindSafe(|| c.voronoi_cell(id))) else {
                        failures.push(format!(
                            "shift {shift} m {margin} seed {seed} {id:?}: panic"
                        ));
                        continue;
                    };
                    let mut wide = Vec::new();
                    for di in -8..=8 {
                        for dj in -8..=8 {
                            if (di, dj) != (0, 0) {
                                wide.push(c.site(SiteId {
                                    i: i + di,
                                    j: j + dj,
                                }));
                            }
                        }
                    }
                    for e in &cell.edges {
                        if let Some(s) = vertex_is_empty(e.end, cell.site, &wide) {
                            failures.push(format!(
                                "shift {shift} m {margin} seed {seed} {id:?}: vertex {:?} nearer to {s:?}",
                                e.end
                            ));
                        }
                    }
                }
            }
        }
    }
    let mut by_config = std::collections::BTreeMap::new();
    for f in &failures {
        let key: String = f.split(" seed").next().unwrap().to_string();
        *by_config.entry(key).or_insert(0) += 1;
    }
    assert!(
        failures.is_empty(),
        "{} failures {by_config:?}, first: {:?}",
        failures.len(),
        &failures[..failures.len().min(3)]
    );
}

// ---------------------------------------------------------------------------------------------
// P4. A gradient fbm with a power-of-two wavelength and integer lacunarity is exactly 0 at every
// point of its coarsest lattice (every octave's lattice contains it, and the noise is 0 on its
// lattice). For W0 that pins e.g. the basin field C1 (2^17 to 2^14) to 0 on a 131 km grid: if
// 0 is the coast threshold, every grid point is on a coast.
#[test]
fn p4_gradient_fbm_is_pinned_to_zero_on_its_coarsest_lattice() {
    let f = Fbm {
        frequency: Frequency::one_in(1 << 17),
        octaves: 4,
        lacunarity: 2,
        gain: 0.4,
    };
    let mut zeros = 0;
    for i in -20i64..20 {
        for j in -20i64..20 {
            if gradient_fbm_2d(0xb0_7e1d, i << 17, j << 17, f) == 0.0 {
                zeros += 1;
            }
        }
    }
    // Also through the fields: a tier S lattice over a 2^13 fbm is 0 on every 32nd lattice point.
    let climate = LatticeField::new(
        8,
        NoiseField::new(7, fbm(1 << 13, 3, 0.5), Basis::Gradient, 4096),
    );
    let pinned = (-20i64..20)
        .flat_map(|i| (-20i64..20).map(move |j| (i << 13, j << 13)))
        .filter(|&(x, z)| climate.value(x, z) == 0)
        .count();
    assert!(
        zeros < 16 && pinned < 16,
        "{zeros} of 1600 coarsest lattice points are exactly 0; {pinned} of 1600 for the tier S field"
    );
}

// ---------------------------------------------------------------------------------------------
// P5. Every point (not a sample) of small rectangles against range, bound and fine, and every
// pair of points of a window against the slope, for trees with fast inputs that cross often.
fn slope_holds(delta: u64, slope: Slope, d2: u128) -> bool {
    if slope.per_block == Q16::MAX {
        return true;
    }
    let Some(over) = delta.checked_sub(slope.jitter) else {
        return true;
    };
    let lhs = u128::from(over) << 16;
    let p = u128::from(slope.per_block.raw());
    // Both sides fit: lhs < 2^60, p < 2^60 for the probes' amplitudes.
    lhs.checked_mul(lhs).unwrap() <= (p * p).checked_mul(d2).unwrap()
}

fn check_every_point<F: Field>(name: &str, f: &F, rect: Rect) -> Result<(), String> {
    let (range, bound, fine) = (f.range(), f.bound(rect), f.fine(rect));
    let [v00, v10, v01, v11] = corner_values(f, rect);
    let (sx, sz) = (i128::from(rect.x1 - rect.x0), i128::from(rect.z1 - rect.z0));
    for x in rect.x0..=rect.x1 {
        for z in rect.z0..=rect.z1 {
            let v = f.value(x, z);
            if !range.contains(v) || !bound.contains(v) {
                return Err(format!(
                    "{name}: {v} at ({x},{z}) out of {range:?} / {bound:?} on {rect:?}"
                ));
            }
            let (wx1, wz1) = (i128::from(x - rect.x0), i128::from(z - rect.z0));
            let (wx0, wz0) = (sx - wx1, sz - wz1);
            let (sx1, sz1) = (sx.max(1), sz.max(1));
            let (wx0, wz0) = (if sx == 0 { 1 } else { wx0 }, if sz == 0 { 1 } else { wz0 });
            let num = wx0 * wz0 * i128::from(v00)
                + wx1 * wz0 * i128::from(v10)
                + wx0 * wz1 * i128::from(v01)
                + wx1 * wz1 * i128::from(v11);
            let den = sx1 * sz1;
            let scaled = i128::from(v) * den;
            if scaled > num + i128::from(fine.up) * den
                || scaled < num - i128::from(fine.down) * den
            {
                return Err(format!(
                    "{name}: {v} at ({x},{z}) vs interpolant {} with {fine:?} on {rect:?}",
                    num as f64 / den as f64
                ));
            }
        }
    }
    // Slope: every pair of a 9 x 9 window at the rectangle's corner.
    let slope = f.slope();
    let pts: Vec<(i64, i64, i64)> = (0..9)
        .flat_map(|a| (0..9).map(move |b| (rect.x0 + a, rect.z0 + b)))
        .map(|(x, z)| (x, z, f.value(x, z)))
        .collect();
    for &(x, z, v) in &pts {
        for &(x2, z2, w) in &pts {
            let d2 = dist_sq((x, z), (x2, z2));
            if !slope_holds(v.abs_diff(w), slope, d2) {
                return Err(format!(
                    "{name}: {v} -> {w} from ({x},{z}) to ({x2},{z2}) with {slope:?}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn p5_fields_hold_at_every_point() {
    let centers = [
        0i64,
        -(1 << 31) - 5,
        1 << 40,
        MAX_BLOCK_COORD - 4000,
        -MAX_BLOCK_COORD + 7,
    ];
    let mut failures = Vec::new();
    let seeds: u64 = std::env::var("P5_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    for seed in 0..seeds {
        // Fast inputs: wavelength 8 and 16, so they cross inside small rectangles.
        let fast = NoiseField::new(seed, fbm(8, 3, 0.7), Basis::Gradient, 4000);
        let other = NoiseField::new(seed ^ 9, fbm(16, 2, 0.6), Basis::Value, 3000);
        let lat2 = LatticeField::new(2, fast);
        let lat3 = LatticeField::new(3, other);
        let bendy = Spline::new(&[
            (-4000, 900),
            (-1000, -500),
            (-10, 3000),
            (10, -3000),
            (900, 2500),
            (4000, -100),
        ]);
        let splined = Splined::new(&lat2, bendy.clone());
        let splined_raw = Splined::new(&fast, bendy);
        let min = Min::new(&lat3, &splined);
        let min_raw = Min::new(&fast, &other);
        let add = Add::new(&min, &lat2);
        let weight = LatticeField::new(
            2,
            Splined::new(&other, Spline::new(&[(-500, 0), (500, 1 << 12)])),
        );
        let blend = Blend::new(&weight, &add, &min_raw, 12);
        let blend_raw = Blend::new(
            Splined::new(&fast, Spline::new(&[(-300, 0), (300, 1 << 8)])),
            &other,
            Const(-2500),
            8,
        );
        let warped = Warped::new(Warp::new(seed, fbm(32, 2, 0.5), 12), &lat3);
        let warped_min = Min::new(
            Warped::new(Warp::new(seed ^ 3, fbm(16, 1, 0.5), 6), &fast),
            &other,
        );
        for &c in &centers {
            for (k, side) in [
                (0i64, 4i64),
                (1, 8),
                (2, 7),
                (3, 16),
                (5, 12),
                (6, 32),
                (7, 33),
                (8, 64),
            ] {
                let x0 = c + (splitmix64(seed * 31 + k as u64) % 64) as i64 - 32;
                let z0 = -c + (splitmix64(seed * 37 + k as u64) % 64) as i64 - 32;
                for rect in [
                    Rect::cell(x0, z0, side),
                    Rect::cell(x0 & !7, z0 & !7, side),
                    Rect::new(x0, z0, x0 + side, z0 + 3),
                ] {
                    let all: [(&str, &dyn Fn() -> Result<(), String>); 12] = [
                        ("fast", &|| check_every_point("fast", &fast, rect)),
                        ("lat2", &|| check_every_point("lat2", &lat2, rect)),
                        ("lat3", &|| check_every_point("lat3", &lat3, rect)),
                        ("splined", &|| check_every_point("splined", &splined, rect)),
                        ("splined_raw", &|| {
                            check_every_point("splined_raw", &splined_raw, rect)
                        }),
                        ("min", &|| check_every_point("min", &min, rect)),
                        ("min_raw", &|| check_every_point("min_raw", &min_raw, rect)),
                        ("add", &|| check_every_point("add", &add, rect)),
                        ("blend", &|| check_every_point("blend", &blend, rect)),
                        ("blend_raw", &|| {
                            check_every_point("blend_raw", &blend_raw, rect)
                        }),
                        ("warped", &|| check_every_point("warped", &warped, rect)),
                        ("warped_min", &|| {
                            check_every_point("warped_min", &warped_min, rect)
                        }),
                    ];
                    for (_, check) in all {
                        if let Err(e) = check() {
                            failures.push(e);
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures, first: {:#?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

// ---------------------------------------------------------------------------------------------
// P6. The Lipschitz bound of one octave of each basis, searched over every unit and diagonal
// step of a region (not random lines): the steepest steps of short wavelengths.
#[test]
fn p6_lipschitz_against_every_step() {
    for basis in [Basis::Gradient, Basis::Value] {
        for den in [3u32, 4, 5, 8, 16, 37] {
            let f = fbm(den, 1, 0.5);
            let l = f.split_at(0).lipschitz(basis).to_f64();
            let mut worst: f64 = 0.0;
            for seed in 0..40u64 {
                for x in -60i64..60 {
                    for z in -60i64..60 {
                        let v = basis.fbm(seed, x, z, f);
                        for (dx, dz) in [(1i64, 0i64), (0, 1), (1, 1), (1, -1)] {
                            let w = basis.fbm(seed, x + dx, z + dz, f);
                            let d = ((dx * dx + dz * dz) as f64).sqrt();
                            worst = worst.max((v - w).abs() / d);
                        }
                    }
                }
            }
            assert!(
                worst <= l + 1e-12,
                "{basis:?} 1/{den}: steepest {worst} over L {l}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// P7. Grid against point with grids that break the usual shape: duplicated and descending
// coordinates, one grid spanning both ends of the world, grids crossing 0 with odd sizes.
#[test]
fn p7_grid_equals_point_on_odd_grids() {
    let fast = NoiseField::new(3, fbm(8, 3, 0.7), Basis::Gradient, 4000);
    let lat = LatticeField::new(
        5,
        NoiseField::new(4, fbm(1 << 10, 4, 0.5), Basis::Gradient, 1 << 30),
    );
    let tree = Blend::new(
        LatticeField::new(
            8,
            Splined::new(fast, Spline::new(&[(-4000, 0), (4000, 1 << 16)])),
        ),
        Min::new(&lat, Add::new(LatticeField::new(2, fast), Const(5))),
        Warped::new(Warp::new(1, fbm(64, 2, 0.5), 30), &lat),
        16,
    );
    let w = MAX_BLOCK_COORD;
    let grids: [(Vec<i64>, Vec<i64>); 5] = [
        (
            vec![5, 5, -3, -3, 0, 31, 32, 33],
            vec![-1, 0, 1, 1, -64, 63],
        ),
        (vec![-w, 0, w], vec![w, -w, 17]),
        ((-17..=17).rev().collect(), (-9..=9).step_by(3).collect()),
        (
            vec![(1 << 31) - 1, 1 << 31, (1 << 31) + 1],
            vec![-(1 << 31), -(1 << 31) - 1],
        ),
        (vec![w - 1, w, w + 31], vec![-w, -w + 1]),
    ];
    for (xs, zs) in &grids {
        let mut out = vec![0; xs.len() * zs.len()];
        tree.grid(xs, zs, &mut out);
        for (i, &x) in xs.iter().enumerate() {
            for (j, &z) in zs.iter().enumerate() {
                assert_eq!(out[i * zs.len() + j], tree.value(x, z), "({x}, {z})");
            }
        }
        let c = Cellular::new(9, 3, 0);
        let mut near = vec![c.nearest(0, 0); xs.len() * zs.len()];
        c.nearest_grid(xs, zs, &mut near);
        for (i, &x) in xs.iter().enumerate() {
            for (j, &z) in zs.iter().enumerate() {
                assert_eq!(
                    near[i * zs.len() + j],
                    c.nearest(x, z),
                    "cellular ({x}, {z})"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// P8. F1, F2 and ids at every point of patches for the tie-heavy small cells the author's patch
// test skips (shift 0 and 2, every margin), against brute force.
#[test]
fn p8_nearest_on_tie_heavy_cells() {
    for (shift, margin) in [(0u32, 0i64), (2, 0), (2, 1), (4, 0), (4, 7)] {
        for seed in 0..8u64 {
            let c = Cellular::new(seed, shift, margin);
            let side = 6i64 << shift;
            for x in -side..side {
                for z in -side..side {
                    let n = c.nearest(x, z);
                    let home = c.cell_of(x, z);
                    let mut all: Vec<(u128, SiteId, Site)> = Vec::new();
                    for di in -5..=5 {
                        for dj in -5..=5 {
                            let s = c.site(SiteId {
                                i: home.i + di,
                                j: home.j + dj,
                            });
                            all.push((dist_sq((x, z), s.pos()), s.id, s));
                        }
                    }
                    all.sort_by_key(|&(d, id, _)| (d, id));
                    assert_eq!(
                        (n.f1, n.d1, n.f2, n.d2),
                        (all[0].2, all[0].0, all[1].2, all[1].0),
                        "shift {shift} margin {margin} seed {seed} at ({x}, {z})"
                    );
                }
            }
        }
    }
}

// P3b. The smallest counterexample P3 found: cells of 4 blocks, no margin. Sites (11, -1) in
// cell (2, -1) and (10, 8) in cell (2, 2) are Voronoi neighbors 3 cells apart, outside the
// window of 2; voronoi_cell(2, -1) returns a vertex nearer to (10, 8) than to its own site.
#[test]
fn p3b_voronoi_neighbor_three_cells_away() {
    let c = Cellular::new(splitmix64(605), 2, 0);
    let a = c.site(SiteId { i: 2, j: -1 });
    let far = c.site(SiteId { i: 2, j: 2 });
    assert_eq!((a.pos(), far.pos()), ((11, -1), (10, 8)));
    let cell = c.voronoi_cell(a.id);
    assert!(
        cell.edges.iter().any(|e| e.neighbor.id == far.id),
        "neighbors {:?}",
        cell.edges.iter().map(|e| e.neighbor.id).collect::<Vec<_>>()
    );
}

// P9. NoiseField::split_at with many octaves and gains at or past 1 (the f64 rounding allowance of
// 2^-40 and the dropped share), at points near the origin and the world limit.
#[test]
fn p9_split_bound_with_heavy_octaves() {
    for (gain, octaves) in [
        (1.0f64, 40u32),
        (1.5, 30),
        (-0.9, 25),
        (2.0, 50),
        (0.999, 64),
    ] {
        let f = Fbm {
            frequency: Frequency::one_in(1 << 20),
            octaves,
            lacunarity: 2,
            gain,
        };
        for basis in [Basis::Gradient, Basis::Value] {
            let full = NoiseField::new(11, f, basis, 1 << 40);
            for cut in [2u64, 8, 1 << 10, 1 << 15, 1 << 19] {
                let (coarse, dropped) = full.split_at(cut);
                for k in 0..400i64 {
                    let (x, z) = (
                        k * 7919 - MAX_BLOCK_COORD + k * k,
                        MAX_BLOCK_COORD - k * 104_729,
                    );
                    let d = full.value(x, z).abs_diff(coarse.value(x, z));
                    assert!(
                        d <= dropped,
                        "{gain} {octaves} {basis:?} cut {cut}: {d} > {dropped}"
                    );
                }
            }
        }
    }
}

// P11. Warp components against their slope over every unit and diagonal step of a region.
#[test]
fn p11_warp_component_slope_every_step() {
    for (den, amp) in [(16u32, 40i64), (64, 1 << 20), (512, 48), (8, 3)] {
        let w = Warp::new(5, fbm(den, 3, 0.5), amp);
        let s = w.component_slope();
        for x in -80i64..80 {
            for z in -80i64..80 {
                let (ax, az) = w.offset(x, z);
                for (dx, dz) in [(1i64, 0i64), (0, 1), (1, 1), (1, -1)] {
                    let (bx, bz) = w.offset(x + dx, z + dz);
                    let d2 = (dx * dx + dz * dz) as u128;
                    assert!(
                        slope_holds(ax.abs_diff(bx), s, d2) && slope_holds(az.abs_diff(bz), s, d2),
                        "den {den} amp {amp} at ({x},{z})"
                    );
                }
            }
        }
    }
}
