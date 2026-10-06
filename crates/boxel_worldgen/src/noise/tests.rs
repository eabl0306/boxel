//! The noise against its definitions: an exact lattice split, `hash2` unchanged inside `i32` and
//! extended past it without repetition, the point and grid paths bit-identical and equal to a
//! shortcut-free reference far from the origin, continuity at the lattice edges, and the octaves
//! the fbm drops. The 3D noise (at the end) the same way: `hash3` against its formula, the grid
//! bit-identical to the point function anywhere in `i64`, the Lipschitz bound, the dropped
//! octaves, and the same statistics far away as near the origin.
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
fn far_tiles_are_not_relabeled_copies_of_the_origin_tile() {
    // Folding the tag into the seed (`seed ^ mix(e)`) would make tile `e` hash every point
    // `(a, b)` exactly like point `(a ^ dx, b ^ dz)` of the origin tile, with `(dx, dz)` the
    // halves of `mix(e)`, for every seed (whole aligned blocks copied). The tag is mixed in
    // after the key is hashed instead, so those points are unrelated.
    let tile = 1i64 << 32;
    for seed in [0, 7, 0x00b0_7e1d] {
        for (tx, tz) in [(1, 0), (0, -1), (3, 5), (-(1 << 20), 1 << 30)] {
            let e = ((excess(tx * tile) as u64) << 32) | excess(tz * tile) as u64;
            let d = mix(e);
            let (dx, dz) = ((d >> 32) as u32, d as u32);
            let mut same = 0;
            for a in 0..8u32 {
                for b in 0..8u32 {
                    let far = hash2(seed, tx * tile + i64::from(a), tz * tile + i64::from(b));
                    let image = hash2(seed, i64::from((a ^ dx) as i32), i64::from((b ^ dz) as i32));
                    same += u32::from(far == image);
                }
            }
            assert_eq!(same, 0, "seed {seed}, tile ({tx}, {tz})");
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

/// Shorthand for an [`Fbm3`] with frequencies `xz.0 / xz.1` and `y.0 / y.1`.
fn fbm3(xz: (u32, u32), y: (u32, u32), octaves: u32, lacunarity: u32, gain: f64) -> Fbm3 {
    Fbm3 {
        frequency: Frequency3 {
            xz: Frequency::new(xz.0, xz.1),
            y: Frequency::new(y.0, y.1),
        },
        octaves,
        lacunarity,
        gain,
    }
}

/// The cheese caverns of the cave plan: 1/128 across, about 1/85 up, 3 octaves.
fn cheese() -> Fbm3 {
    fbm3((1, 128), (3, 256), 3, 2, 0.5)
}

/// Steepest slope of one octave of gradient noise along one axis, per lattice cell. Along x, the
/// fade moves the weight by at most 15/8 per cell from the x0 to the x1 corners, whose dot
/// products differ by at most 4 (each is at most 2: two unit components times offsets of at most
/// 1); and the weighted dot products themselves move by at most 1 (one unit component).
const SLOPE_3D: f64 = 15.0 / 8.0 * 4.0 + 1.0;

/// Bound of `|fbm_3d(p + step) - fbm_3d(p)|` for one voxel along x or z (`along_y` false) or y:
/// the weighted mean of the octaves' [`SLOPE_3D`] times their cells per voxel.
fn lipschitz(f: Fbm3, along_y: bool) -> f64 {
    let axis = if along_y {
        f.frequency.y
    } else {
        f.frequency.xz
    };
    let (xz, y) = (f.frequency.xz, f.frequency.y);
    let (mut xz_num, mut y_num) = (u64::from(xz.num()), u64::from(y.num()));
    let mut num = u64::from(axis.num());
    let (mut amplitude, mut slope, mut weight) = (1.0, 0.0, 0.0);
    for _ in 0..f.octaves {
        if xz_num > u64::from(xz.den()) || y_num > u64::from(y.den()) {
            break;
        }
        slope += f64::abs(amplitude) * SLOPE_3D * num as f64 / f64::from(axis.den());
        weight += f64::abs(amplitude);
        let lacunarity = u64::from(f.lacunarity);
        (xz_num, y_num, num) = (xz_num * lacunarity, y_num * lacunarity, num * lacunarity);
        amplitude *= f.gain;
    }
    if weight > 0.0 { slope / weight } else { 0.0 }
}

#[test]
fn hash3_matches_its_formula_and_recorded_values() {
    let formula = |seed: u64, x: i64, y: i64, z: i64| {
        let s = splitmix64;
        s(s(s(s(seed) ^ x as u64) ^ y as u64) ^ z as u64) as u32
    };
    let edges = [
        0,
        1,
        -1,
        7,
        i64::from(i32::MAX),
        i64::from(i32::MIN),
        1 << 32,
        1 << 40,
        -(1 << 60),
        1 << 61,
        i64::MAX,
        i64::MIN,
    ];
    for seed in [0, 7, u64::MAX] {
        for x in edges {
            for y in edges {
                for z in edges {
                    assert_eq!(
                        hash3(seed, x, y, z),
                        formula(seed, x, y, z),
                        "({x}, {y}, {z})"
                    );
                }
            }
        }
    }
    // Recorded from an independent implementation of the formula (Python integers).
    for (seed, x, y, z, h) in [
        (0x0, 0, 0, 0, 0xaac8_0268),
        (0x7, -3, 12, 5, 0xb611_c53b),
        (0x2a, 1 << 31, -(1 << 31) - 1, 1 << 32, 0xd5bd_474f),
        (0x3039, 1 << 40, -(1 << 60), -(1 << 61), 0x387f_eddb),
        (u64::MAX, i64::MAX, i64::MIN, -1, 0x75c0_5aa4),
        (0x63, 1, 2, 3, 0xf511_d0a5),
        (0x63, 3, 2, 1, 0x366b_0907),
    ] {
        assert_eq!(hash3(seed, x, y, z), h, "seed {seed:#x} ({x}, {y}, {z})");
    }
}

#[test]
fn hash3_depends_on_every_input_and_never_repeats() {
    assert_ne!(hash3(7, 1, 2, 3), hash3(8, 1, 2, 3));
    // Every permutation and sign flip is another lattice point.
    let points = [
        (1, 2, 3),
        (1, 3, 2),
        (2, 1, 3),
        (2, 3, 1),
        (3, 1, 2),
        (3, 2, 1),
        (-1, 0, 0),
        (0, -1, 0),
        (0, 0, -1),
        (0, 0, 0),
    ];
    let hashes: Vec<u32> = points.iter().map(|&(x, y, z)| hash3(7, x, y, z)).collect();
    for (i, a) in hashes.iter().enumerate() {
        for b in &hashes[i + 1..] {
            assert_ne!(a, b);
        }
    }
    // Points 2^32 apart on any axis (the period of a 32-bit key) hash differently.
    let step = 1i64 << 32;
    for (x, y, z) in [(0, 0, 0), (-3, 12, 5), (1 << 31, -(1 << 31), 1 << 40)] {
        let hashes = [
            hash3(7, x, y, z),
            hash3(7, x + step, y, z),
            hash3(7, x, y + step, z),
            hash3(7, x, y, z + step),
            hash3(7, x - step, y - step, z - step),
        ];
        for (i, a) in hashes.iter().enumerate() {
            for b in &hashes[i + 1..] {
                assert_ne!(a, b, "({x}, {y}, {z})");
            }
        }
    }
    // Each output bit is set about half of the time over a block of the lattice.
    let mut ones = [0u32; 32];
    for x in -8..8 {
        for y in -8..8 {
            for z in -8..8 {
                let h = hash3(42, x, y, z);
                for (bit, count) in ones.iter_mut().enumerate() {
                    *count += (h >> bit) & 1;
                }
            }
        }
    }
    for (bit, &count) in ones.iter().enumerate() {
        let ratio = f64::from(count) / 4096.0;
        assert!((0.45..0.55).contains(&ratio), "bit {bit}: {ratio}");
    }
}

#[test]
fn noise_3d_matches_an_independent_implementation() {
    // Recorded from the documented definition in Python (its floats are IEEE doubles, with the
    // same operations in the same order).
    for (seed, f, (x, y, z), bits) in [
        (0, cheese(), (3, 1, -2), 0x3f98_1433_0ed1_8e3f),
        (0, cheese(), (37, -5, 91), 0xbfc2_f354_f990_23a8),
        (0, cheese(), (-1000, 77, 5), 0x3fb2_512c_434b_8687),
        (
            12345,
            fbm3((1, 64), (1, 64), 1, 2, 0.5),
            ((1 << 40) + 7, -(1 << 40) + 21, (1 << 40) + 50),
            0x3fdb_cf88_3854_9d0c,
        ),
        (
            12345,
            fbm3((1, 64), (1, 64), 1, 2, 0.5),
            (5, -(1 << 60), -9),
            0x3fb7_cc7b_4a91_b560,
        ),
        (
            12345,
            fbm3((1, 64), (1, 64), 1, 2, 0.5),
            ((1 << 61) - 3, (1 << 61) - 3, -(1 << 61)),
            0xbfb7_b561_eddc_4338,
        ),
        (
            9,
            fbm3((3, 10), (1, 7), 2, 2, 0.5),
            (-(1 << 61), (1 << 61) - 1, 123),
            0x3fd0_a0e6_e04e_d2d1,
        ),
        (
            9,
            fbm3((3, 10), (1, 7), 2, 2, 0.5),
            (17, -4, 1 << 33),
            0x3fb1_451b_7586_edc0,
        ),
    ] {
        let v = fbm_3d(seed, x, y, z, f);
        assert_eq!(v.to_bits(), bits, "{f:?} at ({x}, {y}, {z}): {v}");
    }
}

#[test]
fn gradient_noise_is_zero_on_the_lattice_and_stays_in_range() {
    let f = Frequency3 {
        xz: Frequency::one_in(16),
        y: Frequency::one_in(8),
    };
    for (cx, cy, cz) in [(0, 0, 0), (-3, 7, 2), (1 << 40, -(1 << 50), 1 << 55)] {
        let v = gradient_noise_3d(3, cx * 16, cy * 8, cz * 16, f);
        assert_eq!(v, 0.0, "cell ({cx}, {cy}, {cz})");
    }
    let f = Frequency3 {
        xz: Frequency::new(37, 100),
        y: Frequency::new(3, 20),
    };
    let (mut min, mut max) = (f64::MAX, f64::MIN);
    for i in 0..20_000 {
        let (x, y, z) = (i * 3 - 700, i * -5 + 300, i * 7);
        let v = gradient_noise_3d(11, x, y, z, f);
        assert!((-1.0..=1.0).contains(&v), "{v} at ({x}, {y}, {z})");
        (min, max) = (min.min(v), max.max(v));
    }
    assert!(
        min < -0.5 && max > 0.5,
        "noise range too narrow: [{min}, {max}]"
    );
}

#[test]
fn noise_3d_is_continuous_at_any_distance() {
    // 2^20 voxels per cell: a voxel step moves the noise by at most SLOPE_3D / 2^20, across cell
    // borders too, wherever the cells are (2^41 cells out at the edge of the world).
    let cell = 1i64 << 20;
    let f = Frequency3::uniform(Frequency::one_in(1 << 20));
    let bound = SLOPE_3D / cell as f64 + 1e-12;
    for start in [
        0,
        -cell,
        (1 << 31) - 8,
        -(1 << 31) - 8,
        (1 << 32) - 8,
        (1 << 31) * cell - 8,
        1 << 40,
        -(1 << 60),
        (1 << 61) - 8,
        -(1 << 61),
        i64::MAX - 16,
        i64::MIN,
    ] {
        for i in 0..16 {
            let (x, y, z) = (start + i, start / 3 + i, start / 5 - i);
            let v = gradient_noise_3d(5, x, y, z, f);
            for (dx, dy, dz) in [(1, 0, 0), (0, 1, 0), (0, 0, 1)] {
                let next = gradient_noise_3d(5, x + dx, y + dy, z + dz, f);
                assert!(
                    (next - v).abs() <= bound,
                    "jump at ({x}, {y}, {z}) + ({dx}, {dy}, {dz}): {v} vs {next}"
                );
            }
        }
        // The noise is not constant there: it moves within a cell (off the lattice planes, where
        // it can be 0 along a whole edge).
        let (x, y, z) = (
            start.min(i64::MAX - 2 * cell),
            start / 3 + cell / 3,
            start / 5 + cell / 5,
        );
        let moved = (1..8).any(|i| {
            let x = x + i * cell / 8;
            gradient_noise_3d(5, x, y, z, f) != gradient_noise_3d(5, x + cell / 2, y, z, f)
        });
        assert!(moved, "constant noise around {start}");
    }
    // Fine lattices, every octave crossing many cells: the fbm stays under the weighted bound.
    for f in [
        cheese(),
        fbm3((3, 40), (1, 16), 4, 2, 0.5),
        fbm3((1, 4), (1, 2), 3, 3, 0.7),
    ] {
        let (bound_xz, bound_y) = (lipschitz(f, false) + 1e-12, lipschitz(f, true) + 1e-12);
        for origin in [0, 1 << 40, -(1 << 60)] {
            for i in 0..2000 {
                let (x, y, z) = (origin + i, origin / 3 - 2 * i, origin / 5 + 3 * i);
                let v = fbm_3d(8, x, y, z, f);
                for (dx, dy, dz, bound) in
                    [(1, 0, 0, bound_xz), (0, 1, 0, bound_y), (0, 0, 1, bound_xz)]
                {
                    let next = fbm_3d(8, x + dx, y + dy, z + dz, f);
                    assert!(
                        (next - v).abs() <= bound,
                        "{f:?} jump at ({x}, {y}, {z}) + ({dx}, {dy}, {dz}): {v} vs {next}"
                    );
                }
            }
        }
    }
}

#[test]
fn grid_3d_matches_point_evaluation_bit_for_bit() {
    let axis = |origin: i64, step: i64, n: i64| -> Vec<i64> {
        (0..n).map(|i| origin + i * step).collect()
    };
    let cases = [
        // (origin, step, fbm): a chunk's density lattice (4x8x4 voxels) near the origin.
        ((0, 0, 0), (4, 8, 4), cheese()),
        ((-1000, 77, 5), (1, 1, 1), fbm3((1, 16), (1, 16), 3, 2, 0.5)),
        // Frequencies that are not powers of two (a division per split).
        ((3, -9, 12), (1, 2, 3), fbm3((3, 10), (1, 7), 4, 2, 0.5)),
        // Sparse grid: the per-point fallback.
        ((7, -3, 11), (1000, 1000, 1000), cheese()),
        // Descending coordinates, odd lacunarity and gain.
        (
            (100, 50, -20),
            (-1, -1, -1),
            fbm3((1, 7), (2, 9), 6, 3, 0.6),
        ),
        // One octave, none, and every octave dropped.
        ((5, 5, 5), (1, 1, 1), fbm3((1, 100), (1, 100), 1, 2, 0.5)),
        ((5, 5, 5), (1, 1, 1), fbm3((1, 100), (1, 100), 0, 2, 0.5)),
        ((5, 5, 5), (1, 1, 1), fbm3((3, 2), (1, 8), 3, 2, 0.5)),
        // Up to one cell per voxel: a corner per voxel in the table.
        ((-7, 3, 9), (1, 1, 1), fbm3((1, 4), (1, 8), 5, 2, 0.5)),
        // Voxels across +-2^31 on every axis, and cells across +-2^31.
        (
            ((1 << 31) - 6, -(1 << 31) - 6, (1 << 31) - 6),
            (1, 1, 1),
            fbm3((1, 4), (1, 2), 2, 2, 0.5),
        ),
        (
            ((1 << 32) - 6, -(1 << 32) - 6, (1 << 32) - 6),
            (1, 1, 1),
            fbm3((1, 2), (1, 2), 1, 2, 0.5),
        ),
        // Far out: 2^40, y = -2^60, the edge of the world (2^61).
        ((1 << 40, -(1 << 40), 1 << 40), (4, 8, 4), cheese()),
        ((0, -(1 << 60), 0), (4, 8, 4), cheese()),
        (
            (-(1 << 61), (1 << 61) - 64, (1 << 61) - 64),
            (4, 8, 4),
            cheese(),
        ),
        // `c * num` past i64 (the i128 split).
        (
            ((1 << 61) - 16, -(1 << 61), (1 << 61) - 16),
            (1, 1, 1),
            fbm3((3, 10), (3, 10), 2, 2, 0.5),
        ),
        // The ends of i64 with one cell per voxel: the `+ 1` corners wrap (with weight 0).
        (
            (i64::MAX - 11, i64::MIN, i64::MAX - 11),
            (1, 1, 1),
            fbm3((1, 1), (1, 2), 1, 2, 0.5),
        ),
        (
            (i64::MIN, i64::MAX - 6, i64::MIN),
            (1, 1, 1),
            fbm3((1, 2), (1, 1), 1, 2, 0.5),
        ),
    ];
    for (seed, &((x0, y0, z0), (sx, sy, sz), f)) in (40u64..).zip(&cases) {
        let (xs, ys, zs) = (axis(x0, sx, 12), axis(y0, sy, 7), axis(z0, sz, 12));
        let mut grid = vec![f64::NAN; xs.len() * ys.len() * zs.len()];
        fbm_3d_grid(seed, &xs, &ys, &zs, f, &mut grid);
        let mut values = grid.iter();
        for &x in &xs {
            for &y in &ys {
                for &z in &zs {
                    let point = fbm_3d(seed, x, y, z, f);
                    assert_eq!(
                        values.next().map(|v| v.to_bits()),
                        Some(point.to_bits()),
                        "case {x0} {y0} {z0} at ({x}, {y}, {z})"
                    );
                }
            }
        }
    }
    // Unordered coordinates with repeats, and an empty grid.
    let (xs, ys, zs) = ([9, -40, 9, 3], [17, -2], [0, 100, -7, 33, 32]);
    let mut grid = vec![0.0; 4 * 2 * 5];
    fbm_3d_grid(1, &xs, &ys, &zs, cheese(), &mut grid);
    assert_eq!(grid[(2 * 2 + 1) * 5 + 3], fbm_3d(1, 9, -2, 33, cheese()));
    fbm_3d_grid(1, &xs, &[], &zs, cheese(), &mut []);
}

#[test]
#[should_panic(expected = "output size")]
fn grid_3d_rejects_a_wrong_output_size() {
    fbm_3d_grid(1, &[0, 1], &[0], &[0, 1], cheese(), &mut [0.0; 3]);
}

#[test]
fn fbm_3d_drops_octaves_finer_than_a_voxel() {
    // 1/4 doubles to 1/2 and to 1 cell per voxel; 2 per voxel and every octave after it are
    // dropped. The y axis alone drops them too (1/16 across, 1/2 up: two octaves).
    let pairs = [
        (
            fbm3((1, 4), (1, 4), 3, 2, 0.5),
            fbm3((1, 4), (1, 4), 1000, 2, 0.5),
        ),
        (
            fbm3((1, 16), (1, 2), 2, 2, 0.5),
            fbm3((1, 16), (1, 2), 40, 2, 0.5),
        ),
        (
            fbm3((1, 9), (1, 3), 2, 3, 0.7),
            fbm3((1, 9), (1, 3), 7, 3, 0.7),
        ),
    ];
    for (kept, asked) in pairs {
        for (x, y, z) in [
            (0, 0, 0),
            (-77, 5, 1 << 33),
            (1 << 40, -(1 << 60), 3),
            (i64::MAX, i64::MIN, 0),
        ] {
            assert_eq!(
                fbm_3d(3, x, y, z, kept).to_bits(),
                fbm_3d(3, x, y, z, asked).to_bits(),
                "{asked:?} at ({x}, {y}, {z})"
            );
        }
        // And the last kept octave counts: one fewer changes the value.
        let fewer = Fbm3 {
            octaves: kept.octaves - 1,
            ..kept
        };
        assert_ne!(fbm_3d(3, 5, 6, 7, kept), fbm_3d(3, 5, 6, 7, fewer));
    }
    // Above one cell per voxel from the first octave: nothing is summed.
    let (x, y, z) = (12, -5, 99);
    assert_eq!(fbm_3d(3, x, y, z, fbm3((3, 2), (1, 8), 4, 2, 0.5)), 0.0);
    assert_eq!(fbm_3d(3, x, y, z, fbm3((1, 8), (9, 8), 4, 2, 0.5)), 0.0);
    let dense = Frequency3 {
        xz: Frequency::one_in(8),
        y: Frequency::new(9, 8),
    };
    assert_eq!(gradient_noise_3d(3, x, y, z, dense), 0.0);
}

#[test]
fn fbm_3d_normalizes_and_degenerates_cleanly() {
    let f = fbm3((10, 170), (3, 170), 6, 2, 0.5);
    for i in 0..500 {
        let (x, y, z) = (i * 7 - 1000, i * -3 + 50, i * 11);
        let v = fbm_3d(9, x, y, z, f);
        assert!((-1.0..=1.0).contains(&v));
        let one = Fbm3 { octaves: 1, ..f };
        assert_eq!(
            fbm_3d(9, x, y, z, one).to_bits(),
            gradient_noise_3d(9, x, y, z, f.frequency).to_bits()
        );
    }
    assert_eq!(fbm_3d(9, 1, 2, 3, Fbm3 { octaves: 0, ..f }), 0.0);
    assert_eq!(
        Frequency3::uniform(Frequency::one_in(64)),
        Frequency3 {
            xz: Frequency::new(2, 128),
            y: Frequency::one_in(64),
        }
    );
}

/// Mean and variance of `n` samples of `f` spread over `2^24` voxels per axis around `center`,
/// far enough apart that their cells are unrelated.
fn sample_stats(f: Fbm3, center: (i64, i64, i64), n: u64) -> (f64, f64) {
    let spread = |r: u64| (r & ((1 << 24) - 1)) as i64 - (1 << 23);
    let (mut sum, mut squares) = (0.0, 0.0);
    for i in 0..n {
        let x = center.0 + spread(splitmix64(3 * i));
        let y = center.1 + spread(splitmix64(3 * i + 1));
        let z = center.2 + spread(splitmix64(3 * i + 2));
        let v = fbm_3d(21, x, y, z, f);
        sum += v;
        squares += v * v;
    }
    let mean = sum / n as f64;
    (mean, squares / n as f64 - mean * mean)
}

#[test]
fn noise_3d_is_as_varied_far_away_as_near_the_origin() {
    let n = 100_000;
    let edge = (1 << 61) - (1 << 24);
    for f in [cheese(), fbm3((1, 64), (1, 64), 1, 2, 0.5)] {
        let (mean, variance) = sample_stats(f, (0, 0, 0), n);
        let sd = variance.sqrt();
        // Noise, not a constant or a flattened copy.
        assert!(
            mean.abs() < 0.02 * sd && (0.1..0.5).contains(&sd),
            "{f:?}: {mean} {sd}"
        );
        for center in [
            (1 << 40, 0, -(1 << 40)),
            (0, -(1 << 60), 0),
            (edge, -edge, edge),
            (-edge, edge, -edge),
        ] {
            let (far_mean, far_variance) = sample_stats(f, center, n);
            assert!(
                (far_mean - mean).abs() <= 0.02 * sd,
                "{f:?} mean at {center:?}: {far_mean} vs {mean}"
            );
            assert!(
                (far_variance / variance - 1.0).abs() <= 0.02,
                "{f:?} variance at {center:?}: {far_variance} vs {variance}"
            );
        }
    }
}

#[test]
fn noise_3d_windows_2_pow_32_apart_differ() {
    let window = |(x, y, z): (i64, i64, i64), f: Fbm3| {
        let axis = |o: i64| (o..o + 64).collect::<Vec<i64>>();
        let mut out = vec![0.0; 64 * 64 * 64];
        fbm_3d_grid(3, &axis(x), &axis(y), &axis(z), f, &mut out);
        out
    };
    let tile = 1i64 << 32;
    // Voxels 2^32 apart (cells 2^25 apart for the cheese), and cells 2^32 apart (1/32 of a cell
    // per voxel, windows 2^37 apart).
    let fine = fbm3((1, 32), (1, 32), 1, 2, 0.5);
    for (f, step) in [(cheese(), tile), (fine, 32 * tile)] {
        let origin = window((0, 0, 0), f);
        let nonzero = origin.iter().filter(|&&v| v != 0.0).count();
        assert!(nonzero * 2 > origin.len(), "{f:?}: {nonzero} nonzero");
        for offset in [
            (step, 0, 0),
            (0, step, 0),
            (0, 0, step),
            (2 * step, 0, 0),
            (0, -2 * step, 0),
            (step, step, step),
            (-step, -step, -step),
        ] {
            // Equal values other than the zeros on the lattice points both windows share (a few
            // more can match by chance on the lattice planes, where few gradients mix).
            let other = window(offset, f);
            let same = origin
                .iter()
                .zip(&other)
                .filter(|&(a, b)| *a != 0.0 && a.to_bits() == b.to_bits())
                .count();
            assert!(
                same * 1000 < origin.len(),
                "{f:?} at {offset:?}: {same} equal"
            );
        }
    }
}
