//! The 2D gradient noise against its definition (written out with an `i128` split and `hash2`
//! per corner), the point and grid forms bit-identical anywhere in `i64`, its range and zeros,
//! the Lipschitz bounds of both bases along random lines (diagonals included), and the fbm split:
//! nothing dropped is the full fbm bit for bit, and the dropped bound covers the full fbm.
use super::*;
use crate::noise::{hash2, value_noise_2d};
use boxel_voxel::MAX_BLOCK_COORD;
use proptest::prelude::*;

/// Shorthand for an [`Fbm`].
fn fbm(num: u32, den: u32, octaves: u32, lacunarity: u32, gain: f64) -> Fbm {
    Fbm {
        frequency: Frequency::new(num, den),
        octaves,
        lacunarity,
        gain,
    }
}

/// A terrain-like fbm: 1/256, 5 octaves, lacunarity 2, gain 0.5.
fn terrain() -> Fbm {
    fbm(1, 256, 5, 2, 0.5)
}

/// [`gradient_fbm_2d`] as documented: the hashed phase of each octave, an `i128` division for
/// every split, [`hash2`] for every corner (so the cells must fit in `i64`), the documented
/// evaluation order, no table.
fn reference(seed: u64, x: i64, z: i64, f: Fbm) -> f64 {
    let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let gradients = [
        [1.0, 1.0],
        [-1.0, 1.0],
        [1.0, -1.0],
        [-1.0, -1.0],
        [1.0, 0.0],
        [-1.0, 0.0],
        [0.0, 1.0],
        [0.0, -1.0],
    ];
    let den = i128::from(f.frequency.den());
    let mut num = i128::from(f.frequency.num());
    let (mut sum, mut weight_sum, mut amplitude) = (0.0, 0.0, 1.0);
    for k in 0..f.octaves {
        let mixed = splitmix64(seed.wrapping_add(u64::from(k)));
        let q = |salt: u64| ((u128::from(splitmix64(mixed ^ salt)) * den as u128) >> 64) as i128;
        let (qx, qz) = (q(0x5048_4153_455f_0058), q(0x5048_4153_455f_005a));
        let split = |c: i64, q: i128| {
            let p = i128::from(c) * num + q;
            let t = p.rem_euclid(den) as f64 / den as f64;
            (i64::try_from(p.div_euclid(den)).unwrap(), t)
        };
        let ((x0, tx), (z0, tz)) = (split(x, qx), split(z, qz));
        let dot = |cx: i64, cz: i64, dx: f64, dz: f64| {
            let g: [f64; 2] =
                gradients[(hash2(seed.wrapping_add(u64::from(k)), cx, cz) & 7) as usize];
            g[0] * dx + g[1] * dz
        };
        let near = lerp(dot(x0, z0, tx, tz), dot(x0 + 1, z0, tx - 1.0, tz), fade(tx));
        let far = lerp(
            dot(x0, z0 + 1, tx, tz - 1.0),
            dot(x0 + 1, z0 + 1, tx - 1.0, tz - 1.0),
            fade(tx),
        );
        sum += amplitude * lerp(near, far, fade(tz)).clamp(-1.0, 1.0);
        weight_sum += f64::abs(amplitude);
        num *= i128::from(f.lacunarity);
        amplitude *= f.gain;
    }
    if weight_sum > 0.0 {
        sum / weight_sum
    } else {
        0.0
    }
}

#[test]
fn gradient_noise_is_zero_on_its_shifted_lattice_and_stays_in_range() {
    // The lattice of 1/16 is shifted by (qx, qz) sixteenths: its points are at 16 c - q.
    let f = Frequency::one_in(16);
    let mixed = splitmix64(3);
    let (qx, qz) = (
        i64::from(phase(mixed, PHASE_X, 16)),
        i64::from(phase(mixed, PHASE_Z, 16)),
    );
    for (cx, cz) in [(0, 0), (-3, 7), (1 << 27, -(1 << 27)), (1 << 40, 5)] {
        let (x, z) = (cx * 16 - qx, cz * 16 - qz);
        assert_eq!(gradient_noise_2d(3, x, z, f), 0.0, "({cx}, {cz})");
    }
    let frequency = Frequency::new(37, 1000);
    let (mut min, mut max) = (f64::MAX, f64::MIN);
    for i in 0..20_000 {
        let (x, z) = (i * 3 - 7000, i * -5 + 300);
        let v = gradient_noise_2d(11, x, z, frequency);
        assert!((-1.0..=1.0).contains(&v), "{v} at ({x}, {z})");
        (min, max) = (min.min(v), max.max(v));
    }
    assert!(min < -0.5 && max > 0.5, "range too narrow: [{min}, {max}]");
    // One octave is the noise; zero octaves are 0.
    let one = fbm(37, 1000, 1, 2, 0.5);
    assert_eq!(
        gradient_fbm_2d(11, 5, -9, one),
        gradient_noise_2d(11, 5, -9, frequency)
    );
    assert_eq!(gradient_fbm_2d(11, 5, -9, Fbm { octaves: 0, ..one }), 0.0);
}

#[test]
fn gradient_fbm_is_not_pinned_on_its_coarsest_lattice() {
    // Integer lacunarity: every octave's lattice holds octave 0's. Without the phases the fbm
    // was exactly 0 at every multiple of the wavelength (and so was a warp's offset).
    let f = fbm(1, 1 << 17, 4, 2, 0.4);
    let zeros = (-20i64..20)
        .flat_map(|i| (-20i64..20).map(move |j| (i << 17, j << 17)))
        .filter(|&(x, z)| gradient_fbm_2d(0x00b0_7e1d, x, z, f) == 0.0)
        .count();
    assert_eq!(zeros, 0);
    let w = super::super::Warp::new(7, fbm(1, 1 << 9, 3, 2, 0.5), 48);
    let still = (-20i64..20)
        .flat_map(|i| (-20i64..20).map(move |j| (i << 9, j << 9)))
        .filter(|&(x, z)| w.offset(x, z) == (0, 0))
        .count();
    assert!(
        still < 8,
        "{still} of 1600 lattice points without an offset"
    );
    // Each octave's phase comes from its own seed: two octaves rarely share one.
    let phases: std::collections::HashSet<(u32, u32)> = (0..64u64)
        .map(|k| {
            let m = splitmix64(9 + k);
            (phase(m, PHASE_X, 1 << 20), phase(m, PHASE_Z, 1 << 20))
        })
        .collect();
    assert_eq!(phases.len(), 64);
}

#[test]
fn corner_shortcuts_match_hashing_each_corner() {
    let mixed = splitmix64(77);
    let tile = 1i64 << 32;
    // Inside i32, at its edges, inside and across 2^32 tiles of the hash, at the ends of i64.
    let edges = [
        0,
        -1,
        i64::from(i32::MAX) - 1,
        i64::from(i32::MAX),
        i64::from(i32::MIN),
        i64::from(i32::MIN) - 1,
        tile,
        tile + i64::from(i32::MAX),
        -3 * tile + 12_345,
        (1 << 40) + i64::from(i32::MAX),
        i64::MAX - 1,
        i64::MAX,
        i64::MIN,
    ];
    for x in edges {
        for z in edges {
            let fast = corner_hashes(mixed, x.into(), z.into());
            assert_eq!(
                fast,
                wide_corner_hashes(mixed, x.into(), z.into()),
                "cell ({x}, {z})"
            );
        }
    }
    // And past i64 (octaves finer than a voxel far out).
    let far = i128::from(i64::MAX) + 5;
    assert_eq!(
        corner_hashes(mixed, far, -far),
        wide_corner_hashes(mixed, far, -far)
    );
}

#[test]
fn shifted_split_is_exact() {
    for c in [0, 1, -1, 1 << 40, -(1 << 62), i64::MAX, i64::MIN] {
        for (num, den) in [
            (1u64, 16u32),
            (3, 10),
            (u64::MAX, 7),
            (1 << 63, 1 << 31),
            (5, 1),
        ] {
            for q in [0, 1, den - 1] {
                let (cell, rem) = split_shifted(Scale::new(num, den), c, q);
                let p = i128::from(c) * i128::from(num) + i128::from(q);
                assert_eq!(
                    cell * i128::from(den) + i128::from(rem),
                    p,
                    "{c} {num} {den} {q}"
                );
                assert!(rem < den);
            }
        }
    }
}

#[test]
fn gradient_noise_matches_its_definition_far_from_the_origin() {
    let cases = [
        (terrain(), 0, 0),
        (terrain(), 1 << 31, -(1 << 31)),
        (terrain(), 1 << 40, -(1 << 39)),
        (terrain(), MAX_BLOCK_COORD - 40, -MAX_BLOCK_COORD),
        (terrain(), i64::MAX - 40, i64::MIN),
        (fbm(1, 7, 7, 2, 0.5), 1 << 50, -(1 << 55)),
        (fbm(19, 10, 2, 2, 0.5), -(1 << 60), (1 << 60) - 7),
        (fbm(5, 3, 4, 3, 0.7), -(1 << 40), 1 << 41),
        // Cells crossing a 2^32 tile of the hash (the low 32 bits wrap) on both axes.
        (fbm(1, 2, 1, 2, 0.5), (1 << 32) - 24, -(3 << 32) - 24),
    ];
    for (seed, &(f, x0, z0)) in (9u64..).zip(&cases) {
        let xs: Vec<i64> = (0..24).map(|i| x0 + i).collect();
        let zs: Vec<i64> = (0..24).map(|i| z0 + 3 * i).collect();
        let mut grid = vec![0.0; 24 * 24];
        gradient_fbm_2d_grid(seed, &xs, &zs, f, &mut grid);
        for (i, &x) in xs.iter().enumerate() {
            for (j, &z) in zs.iter().enumerate() {
                let want = reference(seed, x, z, f).to_bits();
                let point = gradient_fbm_2d(seed, x, z, f).to_bits();
                assert_eq!(point, want, "{f:?} at ({x}, {z})");
                assert_eq!(grid[i * 24 + j].to_bits(), want, "{f:?} grid at ({x}, {z})");
            }
        }
    }
}

#[test]
fn gradient_grid_matches_point_evaluation_bit_for_bit() {
    let axis =
        |origin: i64, step: i64| -> Vec<i64> { (0..32).map(|i| origin + i * step).collect() };
    let cases = [
        (0, 0, 1, terrain()),
        (3, -9, 1, fbm(3, 10, 4, 2, 0.5)),
        // Dense lattice (frequency above 1): the per-point fallback, cells past i64 far out.
        (-40, 12, 1, fbm(73, 10, 3, 2, 0.5)),
        (-(1 << 62), (1 << 62) - 100, 1, fbm(19, 10, 2, 2, 0.5)),
        // Sparse grid, descending coordinates, odd lacunarity.
        (7, -3, 1000, terrain()),
        (100, 50, -1, fbm(1, 7, 6, 3, 0.6)),
        (5, 5, 1, fbm(1, 100, 0, 2, 0.5)),
        // Lattice corners crossing i32::MAX, cells past i32, the ends of i64.
        (4_294_967_280, -4_294_967_280, 1, fbm(1, 2, 1, 2, 0.5)),
        (1 << 40, -(1 << 40), 1, terrain()),
        (i64::MAX - 31, i64::MIN, 1, terrain()),
    ];
    for (seed, &(x0, z0, step, f)) in (40u64..).zip(&cases) {
        let (xs, zs) = (axis(x0, step), axis(z0, step));
        let mut grid = vec![0.0; 32 * 32];
        gradient_fbm_2d_grid(seed, &xs, &zs, f, &mut grid);
        for (i, &x) in xs.iter().enumerate() {
            for (j, &z) in zs.iter().enumerate() {
                assert_eq!(
                    grid[i * 32 + j].to_bits(),
                    gradient_fbm_2d(seed, x, z, f).to_bits(),
                    "case {x0} {z0} {step} at ({x}, {z})"
                );
            }
        }
    }
    // Empty and non-square grids.
    gradient_fbm_2d_grid(1, &[], &[1, 2], terrain(), &mut []);
    let (xs, zs) = (axis(1, 2), axis(-3, 7)[..5].to_vec());
    let mut grid = vec![0.0; 32 * 5];
    gradient_fbm_2d_grid(1, &xs, &zs, terrain(), &mut grid);
    assert_eq!(grid[7 * 5 + 3], gradient_fbm_2d(1, xs[7], zs[3], terrain()));
}

/// Coordinates near the places the tests care about: the origin, the edges of `i32`, `2^40`,
/// and the world limit.
fn place() -> impl Strategy<Value = i64> {
    let centers = [
        0,
        1 << 31,
        -(1 << 31),
        1 << 40,
        MAX_BLOCK_COORD - (1 << 20),
        -MAX_BLOCK_COORD + (1 << 20),
    ];
    (prop::sample::select(centers.to_vec()), -100_000i64..100_000).prop_map(|(c, d)| c + d)
}

/// Random fbm parameters: wavelengths from 2 to 2^14, up to 6 octaves.
fn any_fbm() -> impl Strategy<Value = Fbm> {
    (1u32..=14, 1u32..=6, 2u32..=3, 0.2f64..0.8).prop_map(|(shift, octaves, lacunarity, gain)| {
        fbm(1, 1 << shift, octaves, lacunarity, gain)
    })
}

fn any_basis() -> impl Strategy<Value = Basis> {
    prop::sample::select(vec![Basis::Value, Basis::Gradient])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn grid_equals_point_anywhere(
        seed in any::<u64>(), x0 in place(), z0 in place(), f in any_fbm(),
        steps in (1i64..40, 1i64..40), basis in any_basis(), cut in 0u64..64,
    ) {
        let xs: Vec<i64> = (0..9).map(|i| x0 + i * steps.0).collect();
        let zs: Vec<i64> = (0..7).map(|i| z0 - i * steps.1).collect();
        let split = f.split_at(cut);
        let mut grid = vec![0.0; 9 * 7];
        let mut split_grid = vec![0.0; 9 * 7];
        basis.fbm_grid(seed, &xs, &zs, f, &mut grid);
        split.grid(basis, seed, &xs, &zs, &mut split_grid);
        for (i, &x) in xs.iter().enumerate() {
            for (j, &z) in zs.iter().enumerate() {
                prop_assert_eq!(grid[i * 7 + j].to_bits(), basis.fbm(seed, x, z, f).to_bits());
                prop_assert_eq!(
                    split_grid[i * 7 + j].to_bits(),
                    split.sample(basis, seed, x, z).to_bits()
                );
            }
        }
    }

    #[test]
    fn lipschitz_bound_holds_along_any_line(
        seed in any::<u64>(), x in place(), z in place(), f in any_fbm(), basis in any_basis(),
        d in (-3000i64..3000, -3000i64..3000), cut in 0u64..20_000,
    ) {
        let split = f.split_at(cut);
        let l = split.lipschitz(basis).to_f64();
        let dist = ((d.0 * d.0 + d.1 * d.1) as f64).sqrt();
        let (a, b) = (split.sample(basis, seed, x, z), split.sample(basis, seed, x + d.0, z + d.1));
        prop_assert!((a - b).abs() <= l * dist + 1e-12, "{a} {b} over {dist}: L {l}");
        // A step of one voxel along each axis and each diagonal.
        for (dx, dz) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
            let n = split.sample(basis, seed, x + dx, z + dz);
            let step = ((dx * dx + dz * dz) as f64).sqrt();
            prop_assert!((a - n).abs() <= l * step + 1e-12, "step ({dx}, {dz}): {a} {n}, L {l}");
        }
    }

    #[test]
    fn dropped_bound_covers_the_full_fbm(
        seed in any::<u64>(), x in place(), z in place(), f in any_fbm(), basis in any_basis(),
        cut in 0u64..40_000,
    ) {
        let split = f.split_at(cut);
        let (full, kept) = (basis.fbm(seed, x, z, f), split.sample(basis, seed, x, z));
        prop_assert!((full - kept).abs() <= split.dropped(), "{full} {kept} {}", split.dropped());
    }
}

#[test]
fn split_keeps_the_long_octaves() {
    // Wavelengths 256, 128, 64, 32, 16.
    let f = terrain();
    let kept = |cut: u64| f.split_at(cut).kept().octaves;
    assert_eq!(kept(0), 5);
    assert_eq!(kept(16), 5);
    assert_eq!(kept(17), 4);
    assert_eq!(kept(64), 3);
    assert_eq!(kept(256), 1);
    assert_eq!(kept(257), 0);
    // Level 4 (cells of 16) drops the octaves under 32.
    assert_eq!(kept(2 << 4), 4);
    // Shares and bounds: 1 + 1/2 + 1/4 + 1/8 + 1/16 = 31/16.
    let s = f.split_at(64);
    assert_eq!(s.share(), 1.75 / 1.9375);
    assert!(s.dropped() >= 0.1875 / 1.9375 && s.dropped() < 0.1875 / 1.9375 + 1e-9);
    // Nothing dropped: the full fbm, bit for bit, in both bases.
    let all = f.split_at(0);
    assert_eq!((all.share(), all.dropped()), (1.0, 0.0));
    for (x, z) in [(0, 0), (-77, 1 << 33), (MAX_BLOCK_COORD, -MAX_BLOCK_COORD)] {
        for basis in [Basis::Value, Basis::Gradient] {
            assert_eq!(
                all.sample(basis, 3, x, z).to_bits(),
                basis.fbm(3, x, z, f).to_bits()
            );
        }
        assert_eq!(Basis::Value.fbm(3, x, z, f), fbm_2d(3, x, z, f));
    }
    // Everything dropped: 0, within the whole weight.
    let none = f.split_at(1 << 20);
    assert_eq!(none.sample(Basis::Gradient, 3, 5, 5), 0.0);
    assert!(none.dropped() >= 1.0);
    // Octaves past the u64 frequency (lacunarity 2 from 1/256 stops at 64) are counted once.
    let many = fbm(1, 256, 1000, 2, 0.5).split_at(0);
    assert_eq!(many.kept().octaves, 1000);
    assert_eq!(many.share(), 1.0);
}

#[test]
fn lipschitz_bounds_scale_with_frequency() {
    let one = |den: u32, basis: Basis| fbm(1, den, 1, 2, 0.5).lipschitz(basis).to_f64();
    // One octave: the per-cell slope times the cells per voxel, rounded up.
    assert!((one(64, Basis::Gradient) - 3.8906 / 64.0).abs() < 1e-4);
    assert!((one(64, Basis::Value) - 4.2427 / 64.0).abs() < 1e-4);
    // Finer than a voxel: capped by the range (2 per voxel).
    assert!((fbm(4, 1, 1, 2, 0.5).lipschitz(Basis::Gradient).to_f64() - 2.0).abs() < 1e-4);
    // Dropping octaves lowers the bound.
    let f = terrain();
    for basis in [Basis::Value, Basis::Gradient] {
        assert!(f.split_at(64).lipschitz(basis) < f.lipschitz(basis));
    }
    // The value noise's slope per cell is reached: a smoothstep between corners 2 apart.
    let v = |x: i64| value_noise_2d(1, x, 0, Frequency::one_in(1 << 16));
    let steepest = (0..(1 << 16))
        .step_by(97)
        .map(|x| (v(x + 1) - v(x)).abs() * f64::from(1u32 << 16))
        .fold(0.0, f64::max);
    assert!(steepest <= 3.0 + 1e-6, "{steepest}");
}

/// Mean and variance of `n` gradient fbm samples spread over `2^24` voxels around `center`.
fn sample_stats(f: Fbm, center: (i64, i64), n: u64) -> (f64, f64) {
    let spread = |r: u64| (r & ((1 << 24) - 1)) as i64 - (1 << 23);
    let (mut sum, mut squares) = (0.0, 0.0);
    for i in 0..n {
        let x = center.0 + spread(splitmix64(2 * i));
        let z = center.1 + spread(splitmix64(2 * i + 1));
        let v = gradient_fbm_2d(21, x, z, f);
        sum += v;
        squares += v * v;
    }
    let mean = sum / n as f64;
    (mean, squares / n as f64 - mean * mean)
}

#[test]
fn gradient_noise_is_as_varied_far_away_as_near_the_origin() {
    let n = 50_000;
    let edge = MAX_BLOCK_COORD - (1 << 24);
    let f = fbm(1, 64, 3, 2, 0.5);
    let (mean, variance) = sample_stats(f, (0, 0), n);
    let sd = variance.sqrt();
    assert!(
        mean.abs() < 0.03 * sd && (0.1..0.5).contains(&sd),
        "{mean} {sd}"
    );
    for center in [(1 << 40, -(1 << 40)), (edge, -edge), (-edge, edge)] {
        let (far_mean, far_variance) = sample_stats(f, center, n);
        assert!(
            (far_mean - mean).abs() <= 0.03 * sd,
            "mean at {center:?}: {far_mean}"
        );
        assert!(
            (far_variance / variance - 1.0).abs() <= 0.03,
            "variance at {center:?}: {far_variance} vs {variance}"
        );
    }
}

#[test]
fn gradient_slope_per_axis_is_bounded_by_its_peak() {
    // P(t) = |A(t)| + |A(1 - t)| + F'(t), A(t) = 1 - F(t) - t F'(t): sampled 2^20 times, and
    // |P'| < 16 on [0, 1], so between samples it stays within 16 / 2^21 of them.
    let f = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let df = |t: f64| 30.0 * t * t * (t - 1.0) * (t - 1.0);
    let a = |t: f64| 1.0 - f(t) - t * df(t);
    let p = |t: f64| a(t).abs() + a(1.0 - t).abs() + df(t);
    let n = 1u32 << 20;
    let peak = (0..=n)
        .map(|i| p(f64::from(i) / f64::from(n)))
        .fold(0.0, f64::max);
    assert!((peak - 2.75).abs() < 1e-9, "{peak}");
    assert!(peak + 16.0 / f64::from(2 * n) <= GRADIENT_AXIS_SLOPE);
    const { assert!(GRADIENT_AXIS_SLOPE * std::f64::consts::SQRT_2 <= GRADIENT_SLOPE) };
    // The steepest real cell: gradients (-1, -1), (1, -1), (1, -1), (-1, -1) around its center
    // give a slope of 2.75 per cell along x there.
    let steepest = (0..(1 << 12))
        .flat_map(|seed| {
            let n = |x: i64, z: i64| gradient_noise_2d(seed, x, z, Frequency::one_in(1 << 16));
            // The center of the shifted cell (0, 0).
            let mixed = splitmix64(seed);
            let (cx, cz) = (
                (1i64 << 15) - i64::from(phase(mixed, PHASE_X, 1 << 16)),
                (1i64 << 15) - i64::from(phase(mixed, PHASE_Z, 1 << 16)),
            );
            [
                (n(cx + 1, cz) - n(cx, cz)).abs(),
                (n(cx, cz + 1) - n(cx, cz)).abs(),
            ]
        })
        .fold(0.0, f64::max)
        * f64::from(1u32 << 16);
    assert!(steepest <= 2.751 && steepest > 2.7, "{steepest}");
}
