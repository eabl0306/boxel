//! The noise against its definitions: an exact lattice split, `hash2` unchanged inside `i32` and
//! extended past it without repetition, the point and grid paths bit-identical and equal to a
//! shortcut-free reference far from the origin, continuity at the lattice edges, and the octaves
//! the fbm drops.
use super::*;

/// Shorthand for an [`Fbm`].
fn fbm(num: u32, den: u32, octaves: u32, lacunarity: u32, gain: f64) -> Fbm {
    Fbm {
        frequency: Frequency::new(num, den),
        octaves,
        lacunarity,
        gain,
    }
}

/// The terrain's fbm (1/256, 5 octaves, lacunarity 2, gain 0.5).
fn terrain() -> Fbm {
    fbm(1, 256, 5, 2, 0.5)
}

/// [`fbm_2d`] straight from its definition: an `i128` division for every split and [`hash2`] for
/// every corner (so the cells must fit in `i64`), with no table and no shortcut.
fn reference_fbm(seed: u64, x: i64, z: i64, f: Fbm) -> f64 {
    let den = i128::from(f.frequency.den());
    let mut num = i128::from(f.frequency.num());
    let (mut sum, mut weight_sum, mut amplitude) = (0.0, 0.0, 1.0);
    for k in 0..f.octaves {
        let split = |c: i64| {
            let p = i128::from(c) * num;
            let t = p.rem_euclid(den) as f64 / den as f64;
            (i64::try_from(p.div_euclid(den)).unwrap(), smoothstep(t))
        };
        let ((x0, tx), (z0, tz)) = (split(x), split(z));
        let value = |cx: i64, cz: i64| unit(hash2(seed.wrapping_add(u64::from(k)), cx, cz));
        let corners = [
            value(x0, z0),
            value(x0 + 1, z0),
            value(x0, z0 + 1),
            value(x0 + 1, z0 + 1),
        ];
        sum += amplitude * interpolate(corners, tx, tz);
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
fn splitmix64_matches_reference_values() {
    // First two outputs of the reference SplitMix64 generator seeded with 0 (the state
    // advances by the gamma before each mix).
    assert_eq!(splitmix64(0), 0xe220_a839_7b1d_cdaf);
    assert_eq!(splitmix64(0x9e37_79b9_7f4a_7c15), 0x6e78_9e6a_a1b9_65f4);
}

#[test]
fn hash2_is_deterministic_and_depends_on_every_input() {
    assert_eq!(hash2(7, -3, 12), hash2(7, -3, 12));
    assert_ne!(hash2(7, -3, 12), hash2(8, -3, 12));
    assert_ne!(hash2(7, -3, 12), hash2(7, -2, 12));
    assert_ne!(hash2(7, -3, 12), hash2(7, -3, 13));
    // Swapped and sign-flipped coordinates are different lattice points.
    assert_ne!(hash2(7, 1, 2), hash2(7, 2, 1));
    assert_ne!(hash2(7, -1, 0), hash2(7, 0, -1));
}

#[test]
fn hash2_is_unchanged_inside_i32() {
    // The former `hash2(seed, x: i32, z: i32)`, spelled out.
    let old = |seed: u64, x: i32, z: i32| {
        splitmix64(splitmix64(seed) ^ ((x as u32 as u64) << 32 | z as u32 as u64)) as u32
    };
    let edges = [
        0,
        1,
        -1,
        7,
        -3,
        1 << 30,
        -(1 << 30),
        i32::MAX,
        i32::MIN,
        i32::MAX - 1,
    ];
    for seed in [0, 7, 42, 0x00b0_7e1d ^ 0x007e_e50f_b0e1, u64::MAX] {
        for x in edges {
            for z in edges {
                let h = hash2(seed, i64::from(x), i64::from(z));
                assert_eq!(h, old(seed, x, z), "seed {seed:#x} ({x}, {z})");
            }
        }
    }
    // Recorded with the i32 API before the change.
    for (seed, x, z, h) in [
        (0x0, 0, 0, 0x4d19_7e6f),
        (0x7, -3, 12, 0x92b2_2bbf),
        (0x2a, 2_147_483_647, -2_147_483_648, 0xf266_2151),
        (0x7e_e5bf_cefc, -268_435_456, 268_435_455, 0xdba8_76dd),
        (u64::MAX, -1, -1, 0x67a4_7936),
        (0x3039, 1_073_741_824, -1_073_741_824, 0xe7b7_b5e4),
        (0x63, -2_147_483_648, 2_147_483_647, 0x76a0_c215),
    ] {
        assert_eq!(hash2(seed, x, z), h, "seed {seed:#x} ({x}, {z})");
    }
}

#[test]
fn hash2_key_is_injective_past_i32() {
    // (low 32 bits, excess) determines the value: the excess is 0 exactly inside i32 and
    // differs for values 2^32 apart.
    let values = [
        0,
        -1,
        i64::from(i32::MAX),
        i64::from(i32::MIN),
        1 << 31,
        -(1 << 31) - 1,
        1 << 32,
        -(1 << 32),
        (1 << 32) + 5,
        1 << 40,
        -(1 << 40),
        1 << 61,
        i64::MAX,
        i64::MIN,
    ];
    let mut seen = std::collections::HashSet::new();
    for v in values {
        let low = v as i32;
        let e = excess(v);
        assert_eq!(e == 0, i32::try_from(v).is_ok(), "{v}");
        // Inverse: sext(low) + e * 2^32 (mod 2^64).
        let back = i64::from(low).wrapping_add((i64::from(e as i32)) << 32);
        assert_eq!(back, v);
        assert!(seen.insert((low, e)), "{v} collides");
    }
    // Points 2^32 apart (the period the low bits alone would have) hash differently, and so
    // do the four points that share their low bits around the origin.
    for (x, z) in [
        (0i64, 0i64),
        (-3, 12),
        (1 << 31, -(1 << 31)),
        (1 << 40, 1 << 40),
    ] {
        let hashes = [
            hash2(7, x, z),
            hash2(7, x + (1 << 32), z),
            hash2(7, x, z + (1 << 32)),
            hash2(7, x - (1 << 32), z - (1 << 32)),
        ];
        for (i, a) in hashes.iter().enumerate() {
            for b in &hashes[i + 1..] {
                assert_ne!(a, b, "({x}, {z})");
            }
        }
    }
}

#[test]
fn hash2_bits_are_balanced() {
    // Each output bit should be set about half of the time over a block of the lattice.
    let mut ones = [0u32; 32];
    let n = 64 * 64;
    for x in -32..32 {
        for z in -32..32 {
            let h = hash2(42, x, z);
            for (bit, count) in ones.iter_mut().enumerate() {
                *count += (h >> bit) & 1;
            }
        }
    }
    for (bit, &count) in ones.iter().enumerate() {
        let ratio = f64::from(count) / f64::from(n);
        assert!((0.45..0.55).contains(&ratio), "bit {bit}: {ratio}");
    }
}

#[test]
fn wide_cells_extend_hash2_without_repeating() {
    let seed = 42;
    let mixed = splitmix64(seed);
    // Cells inside i64 hash exactly as hash2 (the i32 shortcut included).
    for (x, z) in [
        (0, 0),
        (-1, 5),
        (i64::from(i32::MAX), i64::from(i32::MIN)),
        (1 << 31, 3),
        (1 << 40, -(1 << 40)),
        (i64::MAX, i64::MIN),
    ] {
        assert_eq!(cell_hash(mixed, x.into(), z.into()), hash2(seed, x, z));
    }
    // (low 64 bits, high part) determines the cell, and the high part is 0 exactly inside i64.
    for v in [
        0,
        -1,
        i128::from(i64::MAX),
        i128::from(i64::MIN),
        1 << 63,
        -(1 << 63) - 1,
        1 << 64,
        -(1 << 64),
        (1 << 64) + 5,
        1 << 100,
        -(1 << 126),
        i128::MAX,
        i128::MIN,
    ] {
        let (low, high) = split_wide(v);
        assert_eq!(high == 0, i64::try_from(v).is_ok(), "{v}");
        assert_eq!(i128::from(low).wrapping_add(i128::from(high) << 64), v);
    }
    // Cells 2^64 apart (the period the low bits alone would have) hash differently.
    for (x, z) in [
        (0i128, 0i128),
        (-3, 12),
        (1 << 63, -(1 << 63)),
        (1 << 100, 1 << 100),
    ] {
        let step = 1i128 << 64;
        let hashes = [
            cell_hash(mixed, x, z),
            cell_hash(mixed, x + step, z),
            cell_hash(mixed, x, z + step),
            cell_hash(mixed, x - step, z - step),
            cell_hash(mixed, x + step, z + step),
        ];
        for (i, a) in hashes.iter().enumerate() {
            for b in &hashes[i + 1..] {
                assert_ne!(a, b, "({x}, {z})");
            }
        }
    }
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
            let fast = corners(mixed, x, z).map(f64::to_bits);
            let each = wide_corners(mixed, x.into(), z.into()).map(f64::to_bits);
            assert_eq!(fast, each, "cell ({x}, {z})");
        }
    }
}

#[test]
fn frequency_is_an_exact_fraction_in_lowest_terms() {
    assert_eq!(Frequency::new(2, 512), Frequency::one_in(256));
    let f = Frequency::new(38, 20);
    assert_eq!((f.num(), f.den()), (19, 10));
    assert_eq!(f.to_f64(), 1.9);
    assert_eq!(Frequency::one_in(7).to_f64(), 1.0 / 7.0);
    // Zero is a valid (constant) frequency.
    let zero = Frequency::new(0, 9);
    assert_eq!((zero.num(), zero.den()), (0, 1));
    assert_eq!(
        value_noise_2d(5, 123, -7, zero),
        value_noise_2d(5, -9, 4, zero)
    );
}

#[test]
#[should_panic(expected = "denominator")]
fn frequency_rejects_a_zero_denominator() {
    let _ = Frequency::new(1, 0);
}

#[test]
fn lattice_split_is_exact() {
    let mut coords = vec![
        0,
        1,
        -1,
        255,
        -255,
        256,
        -256,
        1 << 31,
        -(1 << 31),
        (1 << 53) + 1,
        -(1 << 53) - 1,
        1 << 62,
        i64::MAX,
        i64::MIN,
        i64::MIN + 1,
    ];
    coords.extend((0..64u64).map(|i| splitmix64(i) as i64 >> (i % 64)));
    let nums = [
        0,
        1,
        2,
        3,
        19,
        1 << 20,
        u64::from(u32::MAX),
        1 << 40,
        (1 << 63) + 12_345,
        u64::MAX,
    ];
    let dens = [1, 2, 7, 10, 256, 1 << 31, u32::MAX - 4, u32::MAX];
    for &c in &coords {
        for num in nums {
            for den in dens {
                let scale = Scale::new(num, den);
                let (cell, rem) = scale.split(c);
                let p = i128::from(c) * i128::from(num);
                assert_eq!(
                    cell * i128::from(den) + i128::from(rem),
                    p,
                    "{c} * {num} / {den}"
                );
                assert!(rem < den, "{c} * {num} / {den}");
                // The shift path agrees with the division, and the 64-bit path with both
                // wherever it applies.
                let divided = Scale {
                    shift: None,
                    ..scale
                };
                assert_eq!(divided.split(c), (cell, rem), "{c} * {num} / {den}");
                let narrow = i64::try_from(p).is_ok() && i64::try_from(num).is_ok();
                for s in [scale, divided] {
                    let got = s.split_narrow(c).map(|(cell, rem)| (i128::from(cell), rem));
                    assert_eq!(got, narrow.then_some((cell, rem)), "{c} * {num} / {den}");
                }
            }
        }
    }
}

#[test]
fn value_noise_hits_lattice_values_and_stays_in_range() {
    // Frequency 1: every sample is a lattice corner, near and far.
    let one = Frequency::new(1, 1);
    for (x, z) in [
        (0, 0),
        (-5, 9),
        (123, -456),
        (1 << 31, -(1 << 31) - 1),
        (1 << 40, 7),
        (i64::MAX, i64::MIN),
    ] {
        assert_eq!(
            value_noise_2d(3, x, z, one),
            unit(hash2(3, x, z)),
            "({x}, {z})"
        );
    }
    // A coarser lattice hits its corners at multiples of the cell size.
    let sixteenth = Frequency::one_in(16);
    for (cx, cz) in [(0, 0), (-3, 7), (1 << 40, -(1 << 50))] {
        let v = value_noise_2d(3, cx * 16, cz * 16, sixteenth);
        assert_eq!(v, unit(hash2(3, cx, cz)), "cell ({cx}, {cz})");
    }
    let frequency = Frequency::new(37, 100);
    let mut min = f64::MAX;
    let mut max = f64::MIN;
    for i in 0..4000 {
        let (x, z) = (i * 3 - 700, i * -5 + 300);
        let v = value_noise_2d(11, x, z, frequency);
        assert!((-1.0..=1.0).contains(&v), "{v} at ({x}, {z})");
        min = min.min(v);
        max = max.max(v);
    }
    assert!(
        min < -0.5 && max > 0.5,
        "noise range too narrow: [{min}, {max}]"
    );
}

#[test]
fn value_noise_is_continuous_at_any_distance() {
    // 2^20 voxels per cell: one voxel moves the noise by at most 2 (corner difference) * 1.5
    // (steepest smoothstep) / 2^20, also across cell borders and wherever the cells leave i32
    // (cell 2^31 is voxel 2^51) or the voxels leave the former i32 range.
    let cell = 1i64 << 20;
    let frequency = Frequency::one_in(1 << 20);
    let bound = 3.0 / (1u64 << 20) as f64 + 1e-12;
    for start in [
        0,
        -cell,
        (1 << 31) - 8,
        -(1 << 31) - 8,
        (1 << 32) - 8,
        (1 << 31) * cell - 8,
        -(1 << 31) * cell - 8,
        (1 << 32) * cell - 8,
        -(1 << 32) * cell - 8,
        (1 << 42) * cell - 8,
        i64::MAX - 16,
        i64::MIN,
    ] {
        for i in 0..16 {
            let (x, z) = (start + i, start / 3 + i);
            let v = value_noise_2d(5, x, z, frequency);
            for (dx, dz) in [(1, 0), (0, 1)] {
                let next = value_noise_2d(5, x + dx, z + dz, frequency);
                assert!(
                    (next - v).abs() <= bound,
                    "jump at ({x}, {z}): {v} vs {next}"
                );
            }
        }
        // The noise is not constant there: half a cell further it has moved.
        let (x, z) = (start.min(i64::MAX - 2 * cell), start / 3);
        let moved = (0..8).any(|i| {
            let x = x + i * cell / 8;
            value_noise_2d(5, x, z, frequency) != value_noise_2d(5, x + cell / 2, z, frequency)
        });
        assert!(moved, "constant noise around {start}");
    }
}

#[test]
fn grid_matches_point_evaluation_bit_for_bit() {
    let axis =
        |origin: i64, step: i64| -> Vec<i64> { (0..32).map(|i| origin + i * step).collect() };
    let cases = [
        // (x origin, z origin, step, fbm)
        (0, 0, 1, terrain()),
        (-1000, 77, 1, terrain()),
        (3, -9, 1, fbm(3, 10, 4, 2, 0.5)),
        // Dense lattice (frequency above 1): the per-point fallback.
        (-40, 12, 1, fbm(73, 10, 3, 2, 0.5)),
        // Sparse grid: the per-point fallback too.
        (7, -3, 1000, terrain()),
        // Descending coordinates, odd lacunarity and gain, 1 and 0 octaves.
        (100, 50, -1, fbm(1, 7, 6, 3, 0.6)),
        (5, 5, 1, fbm(1, 100, 1, 2, 0.5)),
        (5, 5, 1, fbm(1, 100, 0, 2, 0.5)),
        // Lattice corners crossing i32::MAX (table and per-point paths).
        (2_147_483_640, -2_147_483_640, 1, fbm(1, 1, 2, 2, 0.5)),
        // Cells past i32, cells past i64, and the ends of i64.
        (1 << 40, -(1 << 40), 1, terrain()),
        (-(1 << 62), (1 << 62) - 100, 1, fbm(19, 10, 2, 2, 0.5)),
        (i64::MAX - 31, i64::MIN, 1, terrain()),
    ];
    for (seed, &(x0, z0, step, f)) in (40u64..).zip(&cases) {
        let (xs, zs) = (axis(x0, step), axis(z0, step));
        let mut grid = vec![0.0; 32 * 32];
        fbm_2d_grid(seed, &xs, &zs, f, &mut grid);
        for (i, &x) in xs.iter().enumerate() {
            for (j, &z) in zs.iter().enumerate() {
                let point = fbm_2d(seed, x, z, f);
                assert_eq!(
                    grid[i * 32 + j].to_bits(),
                    point.to_bits(),
                    "case {x0} {z0} {step} at ({x}, {z})"
                );
            }
        }
    }
    // Non-square grids.
    let (xs, zs) = (axis(1, 2), axis(-3, 7)[..5].to_vec());
    let f = fbm(1, 10, 3, 2, 0.5);
    let mut grid = vec![0.0; 32 * 5];
    fbm_2d_grid(1, &xs, &zs, f, &mut grid);
    assert_eq!(grid[7 * 5 + 3], fbm_2d(1, xs[7], zs[3], f));
}

#[test]
fn noise_matches_its_definition_far_from_the_origin() {
    // 12 below where the low 32 bits of a cell wrap from i32::MAX to i32::MIN.
    const TILE_EDGE: i64 = (1 << 31) - 12;
    // Every cell fits in i64 here, so the shortcut-free reference applies.
    let cases = [
        (terrain(), 0, 0),
        (terrain(), 1 << 35, -(1 << 39)),
        (terrain(), -(1 << 50), 1 << 50),
        (terrain(), 1 << 62, -(1 << 62)),
        (terrain(), i64::MAX - 40, i64::MIN),
        (fbm(1, 7, 7, 2, 0.5), 1 << 50, -(1 << 55)),
        (fbm(19, 10, 2, 2, 0.5), -(1 << 60), (1 << 60) - 7),
        (fbm(5, 3, 4, 3, 0.7), -(1 << 40), 1 << 41),
        // Cells crossing a 2^32 tile of the hash (the low 32 bits wrap) on both axes.
        (fbm(1, 1, 2, 2, 0.5), (1 << 40) + TILE_EDGE, -(3 << 31) - 12),
    ];
    for (seed, &(f, x0, z0)) in (9u64..).zip(&cases) {
        let xs: Vec<i64> = (0..24).map(|i| x0 + i).collect();
        let zs: Vec<i64> = (0..24).map(|i| z0 + 3 * i).collect();
        let mut grid = vec![0.0; 24 * 24];
        fbm_2d_grid(seed, &xs, &zs, f, &mut grid);
        for (i, &x) in xs.iter().enumerate() {
            for (j, &z) in zs.iter().enumerate() {
                let want = reference_fbm(seed, x, z, f).to_bits();
                assert_eq!(fbm_2d(seed, x, z, f).to_bits(), want, "{f:?} at ({x}, {z})");
                assert_eq!(grid[i * 24 + j].to_bits(), want, "{f:?} grid at ({x}, {z})");
            }
        }
    }
}

#[test]
fn fbm_normalizes_and_degenerates_cleanly() {
    let f = fbm(10, 17, 6, 2, 0.5);
    for i in 0..500 {
        let (x, z) = (i * 7 - 1000, i * -3 + 50);
        let v = fbm_2d(9, x, z, f);
        assert!((-1.0..=1.0).contains(&v));
        let one = Fbm { octaves: 1, ..f };
        assert_eq!(fbm_2d(9, x, z, one), value_noise_2d(9, x, z, f.frequency));
    }
    assert_eq!(fbm_2d(9, 1, 2, Fbm { octaves: 0, ..f }), 0.0);
}

#[test]
fn fbm_stops_before_the_frequency_overflows() {
    let count = |f: Fbm| f.octaves(0).count();
    // Numerators 2^0..=2^63, 3 * 2^0..=3 * 2^62 and (2^32 - 1) * 2^0..=2^32.
    assert_eq!(count(fbm(1, 256, 1000, 2, 0.5)), 64);
    assert_eq!(count(fbm(3, 256, 1000, 2, 0.5)), 63);
    assert_eq!(count(fbm(u32::MAX, 1, 1000, 2, 0.5)), 33);
    assert_eq!(count(fbm(1, 256, 5, 2, 0.5)), 5);
    // Lacunarity 0 or 1 never overflows.
    assert_eq!(count(fbm(1, 256, 1000, 1, 0.5)), 1000);
    assert_eq!(count(fbm(1, 256, 1000, 0, 0.5)), 1000);
    // Asking for more octaves than exist changes nothing.
    let (all, many) = (fbm(1, 256, 64, 2, 0.5), fbm(1, 256, 1000, 2, 0.5));
    for (x, z) in [(0, 0), (-77, 1 << 33), (i64::MAX, 5)] {
        assert_eq!(
            fbm_2d(3, x, z, all).to_bits(),
            fbm_2d(3, x, z, many).to_bits()
        );
    }
}
