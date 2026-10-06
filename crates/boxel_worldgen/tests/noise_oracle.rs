//! The integer noise against a private copy of the former `f64` noise, kept here as an oracle:
//! inside the former range (`i32` columns, lattice cells below `2^31`) every terrain height is
//! bit-identical when the frequency denominator is a power of two (the default `1/256`), and
//! within rounding of the former coordinates otherwise.
//!
//! The 3D noise against two oracles: its documented definition, written out with no shortcut
//! (bit-identical anywhere in `i64`), and Ken Perlin's reference improved noise on the same
//! lattice (equal up to the evaluation order).
use boxel_worldgen::noise::{
    Fbm, Fbm3, Frequency3, fbm_2d, fbm_3d, fbm_3d_grid, gradient_noise_3d, hash3,
};
use boxel_worldgen::{Frequency, TerrainGenerator, TerrainSettings};

/// The former noise (`f64` coordinates, saturating `i32` lattice), copied verbatim.
mod former {
    const fn splitmix64(x: u64) -> u64 {
        let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    const fn lattice_hash(mixed_seed: u64, x: i32, z: i32) -> u32 {
        let key = ((x as u32 as u64) << 32) | (z as u32 as u64);
        splitmix64(mixed_seed ^ key) as u32
    }

    fn lattice_value(mixed_seed: u64, x: i32, z: i32) -> f64 {
        f64::from(lattice_hash(mixed_seed, x, z)) / f64::from(u32::MAX) * 2.0 - 1.0
    }

    fn smoothstep(t: f64) -> f64 {
        t * t * (3.0 - 2.0 * t)
    }

    fn cell(c: f64) -> (i32, f64) {
        let f = c.floor();
        (f as i32, smoothstep(c - f))
    }

    fn corners(mixed_seed: u64, x0: i32, z0: i32) -> [f64; 4] {
        let (x1, z1) = (x0.wrapping_add(1), z0.wrapping_add(1));
        [
            lattice_value(mixed_seed, x0, z0),
            lattice_value(mixed_seed, x1, z0),
            lattice_value(mixed_seed, x0, z1),
            lattice_value(mixed_seed, x1, z1),
        ]
    }

    fn interpolate([v00, v10, v01, v11]: [f64; 4], tx: f64, tz: f64) -> f64 {
        let near = v00 + (v10 - v00) * tx;
        let far = v01 + (v11 - v01) * tx;
        (near + (far - near) * tz).clamp(-1.0, 1.0)
    }

    fn value_noise_mixed(mixed_seed: u64, x: f64, z: f64) -> f64 {
        let (x0, tx) = cell(x);
        let (z0, tz) = cell(z);
        interpolate(corners(mixed_seed, x0, z0), tx, tz)
    }

    pub fn fbm_2d(seed: u64, x: f64, z: f64, octaves: u32, lacunarity: f64, gain: f64) -> f64 {
        let mut sum = 0.0;
        let mut weight_sum = 0.0;
        let mut frequency = 1.0;
        let mut amplitude = 1.0;
        for octave in 0..octaves {
            let mixed_seed = splitmix64(seed.wrapping_add(u64::from(octave)));
            sum += amplitude * value_noise_mixed(mixed_seed, x * frequency, z * frequency);
            weight_sum += f64::abs(amplitude);
            frequency *= lacunarity;
            amplitude *= gain;
        }
        if weight_sum > 0.0 {
            sum / weight_sum
        } else {
            0.0
        }
    }
}

/// The former `height_at`: the columns times the `f64` frequency, then the `f64` noise. The
/// frequencies the tests used (`1.0 / 256.0`, `1.0 / 7.0`, `1.9`...) are exactly `num / den`
/// rounded, which is [`Frequency::to_f64`].
fn former_noise(g: &TerrainGenerator, x: i64, z: i64) -> f64 {
    let s = &g.settings;
    let f = s.frequency.to_f64();
    former::fbm_2d(g.seed, x as f64 * f, z as f64 * f, s.octaves, 2.0, 0.5)
}

/// The terrain's noise through the public integer API.
fn noise(g: &TerrainGenerator, x: i64, z: i64) -> f64 {
    let s = &g.settings;
    let fbm = Fbm {
        frequency: s.frequency,
        octaves: s.octaves,
        lacunarity: 2,
        gain: 0.5,
    };
    fbm_2d(g.seed, x, z, fbm)
}

/// `base_height + round(amplitude * n)`, as the generator turns noise into a height.
fn height(s: &TerrainSettings, n: f64) -> i32 {
    s.base_height
        .saturating_add((f64::from(s.amplitude) * n).round() as i32)
}

/// `SplitMix64` steps: a deterministic sample of columns.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// `n` columns with `|x|, |z| <= limit`: uniform, log-uniform in magnitude (so small coordinates
/// are as common as large ones) and 32x32 windows at the origin and at both ends of the range.
fn columns(n: usize, limit: i64, mut state: u64) -> Vec<(i64, i64)> {
    let span = 2 * limit as u64 + 1;
    let mut out = Vec::with_capacity(n + 3 * 1024);
    for i in 0..n {
        let mut coordinate = || {
            let r = next(&mut state);
            if i % 2 == 0 {
                (r % span) as i64 - limit
            } else {
                // A magnitude below 2^bits, either sign.
                let bits = (r >> 58) as u32 % 32;
                let magnitude = (r & ((1 << bits) - 1)) as i64;
                let magnitude = magnitude.min(limit);
                if r & (1 << 57) == 0 {
                    magnitude
                } else {
                    -magnitude
                }
            }
        };
        out.push((coordinate(), coordinate()));
    }
    for (x0, z0) in [(-16, -16), (limit - 31, -limit), (-limit, limit - 31)] {
        for dx in 0..32 {
            out.extend((0..32).map(|dz| (x0 + dx, z0 + dz)));
        }
    }
    out
}

/// Largest `|x|` whose former lattice cells stay below `2^31 - 1` in every octave (the
/// former noise saturated past that), capped at the former `i32` columns.
fn former_limit(s: &TerrainSettings) -> i64 {
    let highest = s.frequency.to_f64() * 2f64.powi(s.octaves.max(1) as i32 - 1);
    (((1u64 << 31) - 2) as f64 / highest).min(f64::from(i32::MAX)) as i64
}

#[test]
fn power_of_two_frequencies_are_bit_identical_to_the_former_noise() {
    let generators = [
        (TerrainGenerator::default(), 60_000),
        (TerrainGenerator::new(3, TerrainSettings::default()), 10_000),
        // The settings of the generator tests.
        (
            TerrainGenerator::new(
                1,
                TerrainSettings {
                    base_height: 0,
                    amplitude: 30.5,
                    bottom: -1,
                    ..TerrainSettings::default()
                },
            ),
            10_000,
        ),
        (
            TerrainGenerator::new(
                99,
                TerrainSettings {
                    base_height: -20,
                    amplitude: 70.0,
                    bottom: -200,
                    ..TerrainSettings::default()
                },
            ),
            10_000,
        ),
        (
            TerrainGenerator::new(
                5,
                TerrainSettings {
                    base_height: 8,
                    amplitude: 20.0,
                    frequency: Frequency::one_in(16),
                    octaves: 3,
                    dirt_depth: 2,
                    sand_level: 2,
                    snow_level: 14,
                    bottom: -6,
                },
            ),
            10_000,
        ),
        (
            TerrainGenerator::new(
                6,
                TerrainSettings {
                    octaves: 0,
                    ..TerrainSettings::default()
                },
            ),
            1_000,
        ),
    ];
    for (g, n) in generators {
        let s = &g.settings;
        assert!(s.frequency.den().is_power_of_two(), "{s:?}");
        // These settings never saturated the former lattice inside i32.
        let limit = former_limit(s);
        assert_eq!(limit, i64::from(i32::MAX), "{s:?}");
        for (x, z) in columns(n, limit, g.seed) {
            let (new, old) = (noise(&g, x, z), former_noise(&g, x, z));
            assert_eq!(new.to_bits(), old.to_bits(), "{s:?} noise at ({x}, {z})");
            let h = g.height_at(x, z);
            assert_eq!(h, height(s, old), "{s:?} height at ({x}, {z})");
            assert_eq!(h, height(s, new), "{s:?} height at ({x}, {z})");
        }
    }
}

/// `1/7` and `19/10` are not exact in binary: the former code rounded the frequency and
/// `x * frequency`, so its lattice coordinates were off by up to about `|x * frequency| *
/// 2^-52`. The integer noise is exact, so it differs from the former one by the noise's slope
/// (at most 3 per lattice cell, per axis) times that error, a difference in the last bits that
/// grows with the distance. A height differs only where the former value was that close to a
/// rounding boundary.
#[test]
fn other_frequencies_match_the_former_noise_within_its_rounding() {
    let generators = [
        // The settings of the `column_heights` unit test.
        TerrainGenerator::new(
            4,
            TerrainSettings {
                frequency: Frequency::one_in(7),
                octaves: 7,
                amplitude: 12.5,
                ..TerrainSettings::default()
            },
        ),
        TerrainGenerator::new(
            5,
            TerrainSettings {
                frequency: Frequency::new(19, 10),
                octaves: 2,
                ..TerrainSettings::default()
            },
        ),
        TerrainGenerator::new(
            2,
            TerrainSettings {
                frequency: Frequency::new(3, 1000),
                ..TerrainSettings::default()
            },
        ),
    ];
    for g in generators {
        let s = &g.settings;
        assert!(!s.frequency.den().is_power_of_two(), "{s:?}");
        let limit = former_limit(s);
        let highest = s.frequency.to_f64() * 2f64.powi(s.octaves as i32 - 1);
        let amplitude = f64::from(s.amplitude);
        let mut heights_differ = 0;
        let samples = columns(20_000, limit, g.seed);
        for &(x, z) in &samples {
            let (new, old) = (noise(&g, x, z), former_noise(&g, x, z));
            let reach = (x.unsigned_abs() + z.unsigned_abs()) as f64 * highest;
            let tolerance = 3.0 * reach * 2f64.powi(-52) * 1.01 + 1e-14;
            assert!(
                (new - old).abs() <= tolerance,
                "{s:?} at ({x}, {z}): {new} vs {old} (tolerance {tolerance})"
            );
            let (h, former) = (g.height_at(x, z), height(s, old));
            if h != former {
                // Only next to a rounding boundary of `amplitude * n`.
                let scaled = amplitude * old;
                let boundary = (scaled - scaled.floor() - 0.5).abs();
                assert!(
                    (h - former).abs() == 1 && boundary <= amplitude * tolerance,
                    "{s:?} height at ({x}, {z}): {h} vs {former}"
                );
                heights_differ += 1;
            }
        }
        assert!(
            heights_differ * 1000 < samples.len(),
            "{s:?}: {heights_differ} heights differ"
        );
    }
}

/// The 3D noise as `gradient_noise_3d` and `Fbm3` document it: an `i128` split per axis,
/// `hash3` for each of the 8 corners, and the documented evaluation order.
mod spec3 {
    use super::{Fbm3, hash3};

    /// Perlin's 12 edge gradients as 16 entries.
    pub const GRADIENTS: [[f64; 3]; 16] = [
        [1.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
        [1.0, -1.0, 0.0],
        [-1.0, -1.0, 0.0],
        [1.0, 0.0, 1.0],
        [-1.0, 0.0, 1.0],
        [1.0, 0.0, -1.0],
        [-1.0, 0.0, -1.0],
        [0.0, 1.0, 1.0],
        [0.0, -1.0, 1.0],
        [0.0, 1.0, -1.0],
        [0.0, -1.0, -1.0],
        [1.0, 1.0, 0.0],
        [0.0, -1.0, 1.0],
        [-1.0, 1.0, 0.0],
        [0.0, -1.0, -1.0],
    ];

    pub fn fade(t: f64) -> f64 {
        t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
    }

    pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
        a + (b - a) * t
    }

    /// `floor(c * num / den)` (at most one cell per voxel: it fits in `i64`) and the position
    /// `rem / den` inside the cell.
    pub fn split(c: i64, num: u64, den: u32) -> (i64, f64) {
        let p = i128::from(c) * i128::from(num);
        let den = i128::from(den);
        let cell = i64::try_from(p.div_euclid(den)).unwrap();
        (cell, p.rem_euclid(den) as f64 / den as f64)
    }

    /// One octave with `num / den` cells per voxel along x and z (`xz`) and y (`y`).
    pub fn noise(seed: u64, (x, y, z): (i64, i64, i64), xz: (u64, u32), y_freq: (u64, u32)) -> f64 {
        let (cx, tx) = split(x, xz.0, xz.1);
        let (cy, ty) = split(y, y_freq.0, y_freq.1);
        let (cz, tz) = split(z, xz.0, xz.1);
        let (u, v, w) = (fade(tx), fade(ty), fade(tz));
        let g = |i: i64, j: i64, k: i64| {
            let h = hash3(
                seed,
                cx.wrapping_add(i),
                cy.wrapping_add(j),
                cz.wrapping_add(k),
            );
            GRADIENTS[(h & 15) as usize]
        };
        let layer = |k: i64| {
            let dot = |g: [f64; 3], dx: f64, dy: f64| g[0] * dx + g[1] * dy;
            let (g00, g10, g01, g11) = (g(0, 0, k), g(1, 0, k), g(0, 1, k), g(1, 1, k));
            let near = lerp(dot(g00, tx, ty), dot(g10, tx - 1.0, ty), u);
            let far = lerp(dot(g01, tx, ty - 1.0), dot(g11, tx - 1.0, ty - 1.0), u);
            let slope = lerp(lerp(g00[2], g10[2], u), lerp(g01[2], g11[2], u), v);
            (lerp(near, far, v), slope)
        };
        let ((base0, slope0), (base1, slope1)) = (layer(0), layer(1));
        lerp(base0 + slope0 * tz, base1 + slope1 * (tz - 1.0), w).clamp(-1.0, 1.0)
    }

    /// The octaves at most one cell per voxel on both axes, normalized by their weights.
    pub fn fbm(seed: u64, p: (i64, i64, i64), f: Fbm3) -> f64 {
        let (xz, y) = (f.frequency.xz, f.frequency.y);
        let (mut xz_num, mut y_num) = (u64::from(xz.num()), u64::from(y.num()));
        let (mut sum, mut weight_sum, mut amplitude) = (0.0, 0.0, 1.0);
        for k in 0..f.octaves {
            if xz_num > u64::from(xz.den()) || y_num > u64::from(y.den()) {
                break;
            }
            let n = noise(
                seed.wrapping_add(u64::from(k)),
                p,
                (xz_num, xz.den()),
                (y_num, y.den()),
            );
            sum += amplitude * n;
            weight_sum += f64::abs(amplitude);
            xz_num *= u64::from(f.lacunarity);
            y_num *= u64::from(f.lacunarity);
            amplitude *= f.gain;
        }
        if weight_sum > 0.0 {
            sum / weight_sum
        } else {
            0.0
        }
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

#[test]
fn noise_3d_matches_its_definition_far_from_the_origin() {
    let cheese = fbm3((1, 128), (3, 256), 3, 2, 0.5);
    let cases = [
        // (fbm, origin, step)
        (cheese, (0, 0, 0), (4, 8, 4)),
        (
            cheese,
            (-(1 << 31) - 16, (1 << 31) - 40, -(1 << 31) - 16),
            (4, 8, 4),
        ),
        (cheese, (1 << 40, -(1 << 40), -(1 << 40)), (4, 8, 4)),
        (cheese, (5, -(1 << 60), -9), (4, 8, 4)),
        (
            cheese,
            ((1 << 61) - 40, -(1 << 61), (1 << 61) - 40),
            (4, 8, 4),
        ),
        (
            fbm3((1, 2), (1, 4), 2, 2, 0.5),
            ((1 << 32) - 4, -(1 << 32) - 4, 1 << 33),
            (1, 1, 1),
        ),
        (
            fbm3((3, 10), (1, 7), 3, 2, 0.6),
            (-(1 << 61), (1 << 61) - 9, 1 << 61),
            (1, 1, 1),
        ),
        (
            fbm3((7, 1000), (19, 4096), 4, 3, 0.5),
            (i64::MIN, i64::MAX - 9, i64::MIN),
            (1, 1, 1),
        ),
        (
            fbm3((1, 1), (1, 2), 1, 2, 0.5),
            (i64::MAX - 9, i64::MIN, i64::MAX - 9),
            (1, 1, 1),
        ),
    ];
    for (seed, &(f, (x0, y0, z0), (sx, sy, sz))) in (60u64..).zip(&cases) {
        let xs: Vec<i64> = (0..10).map(|i| x0 + i * sx).collect();
        let ys: Vec<i64> = (0..10).map(|i| y0 + i * sy).collect();
        let zs: Vec<i64> = (0..10).map(|i| z0 + i * sz).collect();
        let mut grid = vec![0.0; 1000];
        fbm_3d_grid(seed, &xs, &ys, &zs, f, &mut grid);
        let mut values = grid.iter();
        for &x in &xs {
            for &y in &ys {
                for &z in &zs {
                    let want = spec3::fbm(seed, (x, y, z), f).to_bits();
                    let point = fbm_3d(seed, x, y, z, f).to_bits();
                    assert_eq!(point, want, "{f:?} at ({x}, {y}, {z})");
                    let grid = values.next().map(|v| v.to_bits());
                    assert_eq!(grid, Some(want), "{f:?} grid at ({x}, {y}, {z})");
                }
            }
        }
    }
}

/// Ken Perlin's reference improved noise (2002) on the `hash3` lattice: his `grad` bit trick for
/// the 16 gradients, and the 8 dot products interpolated in x, then y, then z.
fn perlin_reference(seed: u64, (x, y, z): (i64, i64, i64), f: Frequency3) -> f64 {
    use spec3::{fade, lerp, split};
    let grad = |h: u32, x: f64, y: f64, z: f64| {
        let h = h & 15;
        let u = if h < 8 { x } else { y };
        let v = if h < 4 {
            y
        } else if h == 12 || h == 14 {
            x
        } else {
            z
        };
        (if h & 1 == 0 { u } else { -u }) + (if h & 2 == 0 { v } else { -v })
    };
    let (cx, x) = split(x, f.xz.num().into(), f.xz.den());
    let (cy, y) = split(y, f.y.num().into(), f.y.den());
    let (cz, z) = split(z, f.xz.num().into(), f.xz.den());
    let (u, v, w) = (fade(x), fade(y), fade(z));
    let h = |i: i64, j: i64, k: i64| hash3(seed, cx + i, cy + j, cz + k);
    let n = lerp(
        lerp(
            lerp(
                grad(h(0, 0, 0), x, y, z),
                grad(h(1, 0, 0), x - 1.0, y, z),
                u,
            ),
            lerp(
                grad(h(0, 1, 0), x, y - 1.0, z),
                grad(h(1, 1, 0), x - 1.0, y - 1.0, z),
                u,
            ),
            v,
        ),
        lerp(
            lerp(
                grad(h(0, 0, 1), x, y, z - 1.0),
                grad(h(1, 0, 1), x - 1.0, y, z - 1.0),
                u,
            ),
            lerp(
                grad(h(0, 1, 1), x, y - 1.0, z - 1.0),
                grad(h(1, 1, 1), x - 1.0, y - 1.0, z - 1.0),
                u,
            ),
            v,
        ),
        w,
    );
    n.clamp(-1.0, 1.0)
}

#[test]
fn gradient_noise_3d_is_perlins_improved_noise() {
    // The gradient table and the reordered sum are Perlin's noise: equal up to rounding.
    let frequencies = [
        Frequency3::uniform(Frequency::one_in(16)),
        Frequency3 {
            xz: Frequency::one_in(128),
            y: Frequency::new(3, 256),
        },
        Frequency3 {
            xz: Frequency::new(3, 10),
            y: Frequency::new(1, 7),
        },
    ];
    let mut worst: f64 = 0.0;
    for (seed, f) in (5u64..).zip(frequencies) {
        for origin in [0, 1 << 40, -(1 << 60)] {
            for i in 0..4000 {
                let p = (
                    origin + i * 7 - 9000,
                    origin / 3 - i * 5,
                    origin / 7 + i * 3,
                );
                let ours = gradient_noise_3d(seed, p.0, p.1, p.2, f);
                let perlin = perlin_reference(seed, p, f);
                worst = worst.max((ours - perlin).abs());
            }
        }
    }
    assert!(worst <= 1e-14, "differs from Perlin's noise by {worst}");
}
