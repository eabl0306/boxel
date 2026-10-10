//! The 3D noise against two oracles: its documented definition, written out with no shortcut
//! (bit-identical anywhere in `i64`), and Ken Perlin's reference improved noise on the same
//! lattice (equal up to the evaluation order).
//!
//! The 2D oracle (the former `f64` noise) moved to `boxel_demo` with `TerrainGenerator` (#48).
use boxel_worldgen::Frequency;
use boxel_worldgen::noise::{Fbm3, Frequency3, fbm_3d, fbm_3d_grid, gradient_noise_3d, hash3};

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
