//! Every field and combinator against the contract of [`Field`], checked exactly: values inside
//! the range and the bound of any rectangle, the fine bound against the exact bilinear
//! interpolant of the corners (a rational, no float), the slope along random lines and the
//! unit steps of both axes and diagonals, and the grid bit-identical to the point form. Fields
//! sit at the origin, the edges of `i32`, `2^40` and next to the world limit. Then the cases the
//! naive algebra gets wrong (Min's crossing, the blend term over both axes), the lattice's exact
//! interpolation, the spline and the noise split.
use super::*;
use crate::exact::mul_wide;
use crate::noise::{Basis, Fbm, Frequency, Warp};
use boxel_voxel::MAX_BLOCK_COORD;
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

/// A test field: `clamp(a x + b z + c, lo, hi)`, linear (so exactly its own interpolant) while
/// the clamp does not bite.
#[derive(Clone, Copy, Debug)]
struct Plane {
    a: i64,
    b: i64,
    c: i64,
    lo: i64,
    hi: i64,
}

impl Plane {
    fn raw(&self, x: i64, z: i64) -> i128 {
        i128::from(self.a) * i128::from(x) + i128::from(self.b) * i128::from(z) + i128::from(self.c)
    }
}

impl Field for Plane {
    fn value(&self, x: i64, z: i64) -> i64 {
        self.raw(x, z).clamp(self.lo.into(), self.hi.into()) as i64
    }
    fn range(&self) -> Span {
        Span::new(self.lo, self.hi)
    }
    /// Exact: a clamped linear function takes its extremes at the corners.
    fn bound(&self, rect: Rect) -> Span {
        let v = corner_values(self, rect);
        Span::new(*v.iter().min().unwrap(), *v.iter().max().unwrap())
    }
    fn slope(&self) -> Slope {
        let n = (self.a * self.a + self.b * self.b) as u128;
        Slope {
            per_block: Q16::from_raw(isqrt_ceil(n << 32) as u64),
            jitter: 0,
        }
    }
    fn fine(&self, rect: Rect) -> Fine {
        let inside = rect.corners().iter().all(|&(x, z)| {
            let v = self.raw(x, z);
            i128::from(self.lo) <= v && v <= i128::from(self.hi)
        });
        if inside {
            Fine::ZERO
        } else {
            fine_from_slope(self, rect)
        }
    }
}

/// The bilinear interpolant of `corners` over `rect` at `(x, z)`, as `num / den` exactly.
fn interpolant(rect: Rect, [v00, v10, v01, v11]: [i64; 4], x: i64, z: i64) -> (i128, i128) {
    let weights = |lo: i64, hi: i64, c: i64| -> (i128, i128, i128) {
        if hi == lo {
            (1, 0, 1)
        } else {
            (i128::from(hi - c), i128::from(c - lo), i128::from(hi - lo))
        }
    };
    let (x0, x1, sx) = weights(rect.x0, rect.x1, x);
    let (z0, z1, sz) = weights(rect.z0, rect.z1, z);
    let num = x0 * z0 * i128::from(v00)
        + x1 * z0 * i128::from(v10)
        + x0 * z1 * i128::from(v01)
        + x1 * z1 * i128::from(v11);
    (num, sx * sz)
}

/// `delta <= slope.per_block * sqrt(d2) + slope.jitter`, exactly.
fn slope_holds(delta: u64, slope: Slope, d2: u128) -> bool {
    let Some(over) = delta.checked_sub(slope.jitter) else {
        return true;
    };
    let lhs = u128::from(over) << 16;
    let p = u128::from(slope.per_block.raw());
    mul_wide(lhs, lhs) <= mul_wide(p * p, d2)
}

/// One case of the contract: a rectangle, points inside it, a line from one of them, a grid.
#[derive(Clone, Debug)]
struct Case {
    rect: Rect,
    points: Vec<(i64, i64)>,
    line: (i64, i64),
    xs: Vec<i64>,
    zs: Vec<i64>,
}

/// Checks `f` against its contract on `case`.
fn check<F: Field>(name: &str, f: &F, case: &Case) -> Result<(), TestCaseError> {
    let (range, bound) = (f.range(), f.bound(case.rect));
    let corners = corner_values(f, case.rect);
    let fine = f.fine(case.rect);
    let slope = f.slope();
    for &(x, z) in &case.points {
        let v = f.value(x, z);
        prop_assert!(range.contains(v), "{name}: {v} out of {range:?}");
        prop_assert!(
            bound.contains(v),
            "{name}: {v} out of {bound:?} on {:?}",
            case.rect
        );
        let (num, den) = interpolant(case.rect, corners, x, z);
        let scaled = i128::from(v) * den;
        prop_assert!(
            scaled <= num + i128::from(fine.up) * den
                && scaled >= num - i128::from(fine.down) * den,
            "{name}: {v} at ({x}, {z}) vs interpolant {} with {fine:?} on {:?}",
            num as f64 / den as f64,
            case.rect
        );
        // The slope along the case's line and the 8 unit steps.
        let (dx, dz) = case.line;
        for (dx, dz) in [
            (dx, dz),
            (1, 0),
            (0, 1),
            (1, 1),
            (1, -1),
            (-1, 0),
            (0, -1),
            (-1, -1),
            (-1, 1),
        ] {
            let w = f.value(x + dx, z + dz);
            let d2 = (dx * dx + dz * dz) as u128;
            prop_assert!(
                slope_holds(v.abs_diff(w), slope, d2),
                "{name}: {v} -> {w} over ({dx}, {dz}) with {slope:?}"
            );
        }
    }
    let mut grid = vec![0; case.xs.len() * case.zs.len()];
    f.grid(&case.xs, &case.zs, &mut grid);
    for (i, &x) in case.xs.iter().enumerate() {
        for (j, &z) in case.zs.iter().enumerate() {
            prop_assert_eq!(
                grid[i * case.zs.len() + j],
                f.value(x, z),
                "{} grid at ({}, {})",
                name,
                x,
                z
            );
        }
    }
    Ok(())
}

/// Checks `f.cells(grid)` against the contract: corners equal to the grid form, and in every
/// cell the bound and the fine against the field at `points` (fractions of the cell) and at its
/// corners and center.
fn check_cells<F: Field>(
    name: &str,
    f: &F,
    grid: &CellGrid,
    points: &[(f64, f64)],
) -> Result<(), TestCaseError> {
    let cells = f.cells(grid);
    let mut corners = vec![0; (grid.nx + 1) * (grid.nz + 1)];
    f.grid(&grid.corner_xs(), &grid.corner_zs(), &mut corners);
    prop_assert_eq!(&cells.corners, &corners, "{} corners", name);
    prop_assert_eq!(cells.bounds.len(), grid.len());
    prop_assert_eq!(cells.fines.len(), grid.len());
    for i in 0..grid.nx {
        for j in 0..grid.nz {
            let rect = grid.rect(i, j);
            let (bound, fine) = (cells.bounds[i * grid.nz + j], cells.fines[i * grid.nz + j]);
            let values = cells.corner_values(grid, i, j);
            prop_assert_eq!(values, corner_values(f, rect), "{} cell corners", name);
            let s = rect.x1 - rect.x0;
            let inside = points
                .iter()
                .map(|&(tx, tz)| {
                    (
                        rect.x0 + (tx * s as f64) as i64,
                        rect.z0 + (tz * s as f64) as i64,
                    )
                })
                .chain(rect.corners())
                .chain([(rect.center_x(), rect.center_z())]);
            for (x, z) in inside {
                let v = f.value(x, z);
                prop_assert!(
                    bound.contains(v),
                    "{name}: {v} at ({x}, {z}) out of {bound:?} on {rect:?}"
                );
                let (num, den) = interpolant(rect, values, x, z);
                let scaled = i128::from(v) * den;
                prop_assert!(
                    scaled <= num + i128::from(fine.up) * den
                        && scaled >= num - i128::from(fine.down) * den,
                    "{name}: {v} at ({x}, {z}) with {fine:?} on {rect:?}"
                );
            }
        }
    }
    Ok(())
}

/// A grid of a few cells of a LOD-like size at a place.
fn cell_grid() -> impl Strategy<Value = CellGrid> {
    (
        place(),
        place(),
        prop::sample::select(vec![1i64, 2, 3, 4, 8, 16, 17, 32, 64, 128, 256]),
        1usize..4,
        1usize..4,
        any::<bool>(),
    )
        .prop_map(|(x, z, size, nx, nz, aligned)| {
            let snap = |c: i64| if aligned { c - c.rem_euclid(size) } else { c };
            CellGrid::new(snap(x), snap(z), size, nx, nz)
        })
}

/// Centers: the origin, the edges of `i32`, `2^40`, next to the world limit on both sides.
fn place() -> impl Strategy<Value = i64> {
    let centers = [
        0,
        1 << 31,
        -(1 << 31),
        1 << 40,
        MAX_BLOCK_COORD - (1 << 20),
        -MAX_BLOCK_COORD + (1 << 20),
    ];
    (prop::sample::select(centers.to_vec()), -50_000i64..50_000).prop_map(|(c, d)| c + d)
}

/// A rectangle (often a LOD-like cell aligned to a power of two), points in it, a line and a
/// grid around it.
fn case() -> impl Strategy<Value = Case> {
    let side = prop_oneof![
        prop::sample::select(vec![0i64, 1, 2, 4, 8, 15, 16, 17, 32, 64, 128, 256, 512]),
        0i64..600,
    ];
    (
        place(),
        place(),
        side.clone(),
        side,
        0u32..10,
        prop::collection::vec((0.0f64..=1.0, 0.0f64..=1.0), 6),
        (-3000i64..3000, -3000i64..3000),
        prop::collection::vec(-700i64..700, 1..6),
        prop::collection::vec(-700i64..700, 1..5),
    )
        .prop_map(|(x, z, sx, sz, align, ts, line, gx, gz)| {
            // Snap the corner to a multiple of 2^align (0: anywhere).
            let snap = |c: i64| c & !((1i64 << align) - 1);
            let (x0, z0) = (snap(x), snap(z));
            let rect = Rect::new(x0, z0, x0 + sx, z0 + sz);
            let mut points: Vec<(i64, i64)> = ts
                .iter()
                .map(|&(tx, tz)| (x0 + (tx * sx as f64) as i64, z0 + (tz * sz as f64) as i64))
                .collect();
            points.push((x0 + sx / 2, z0 + sz / 2));
            points.extend(rect.corners());
            Case {
                rect,
                points,
                line,
                xs: gx.iter().map(|&d| x0 + d).collect(),
                zs: gz.iter().map(|&d| z0 + d).collect(),
            }
        })
}

fn fbm(den: u32, octaves: u32) -> Fbm {
    Fbm {
        frequency: Frequency::one_in(den),
        octaves,
        lacunarity: 2,
        gain: 0.5,
    }
}

/// A height-like spline: steep, flat and bent parts, in and past the noise's range.
fn spline() -> Spline {
    Spline::new(&[(-1000, -200), (-100, 0), (0, 300), (400, 350), (1000, 2000)])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(192))]

    #[test]
    fn every_field_keeps_its_contract(seed in any::<u64>(), case in case()) {
        let noise = NoiseField::new(seed, fbm(64, 3), Basis::Gradient, 1000);
        let value = NoiseField::new(seed ^ 1, fbm(32, 2), Basis::Value, 500);
        let lattice = LatticeField::new(4, noise);
        let coarse = LatticeField::new(8, value);
        let splined = Splined::new(&lattice, spline());
        let add = Add::new(&coarse, &splined);
        let min = Min::new(&add, &noise);
        let weight = LatticeField::new(
            6,
            Splined::new(noise, Spline::new(&[(-300, 0), (300, 1 << 16)])),
        );
        let blend = Blend::new(&weight, &min, Const(300), 16);
        let warped = Warped::new(Warp::new(seed, fbm(256, 2), 40), &lattice);
        let plane = Plane { a: 3, b: -2, c: 7, lo: -(1 << 40), hi: 1 << 40 };
        let crossing = Min::new(plane, Plane { a: -1, b: 1, c: 5, lo: -(1 << 40), hi: 1 << 40 });
        check("const", &Const(-42), &case)?;
        check("noise", &noise, &case)?;
        check("value noise", &value, &case)?;
        check("lattice", &lattice, &case)?;
        check("coarse lattice", &coarse, &case)?;
        check("splined", &splined, &case)?;
        check("add", &add, &case)?;
        check("min", &min, &case)?;
        check("weight", &weight, &case)?;
        check("blend", &blend, &case)?;
        check("warped", &warped, &case)?;
        check("planes' min", &crossing, &case)?;
        check("blend of planes", &Blend::new(&weight, plane, Const(0), 16), &case)?;
    }

    #[test]
    fn every_field_keeps_its_contract_over_cells(
        seed in any::<u64>(),
        grid in cell_grid(),
        points in prop::collection::vec((0.0f64..=1.0, 0.0f64..=1.0), 4),
    ) {
        let noise = NoiseField::new(seed, fbm(64, 3), Basis::Gradient, 1000);
        let value = NoiseField::new(seed ^ 1, fbm(32, 2), Basis::Value, 500);
        let lattice = LatticeField::new(4, noise);
        let coarse = LatticeField::new(8, value);
        let fine = LatticeField::new(1, value);
        let splined = Splined::new(&lattice, spline());
        let add = Add::new(&coarse, &splined);
        let min = Min::new(&add, &noise);
        let weight = LatticeField::new(
            6,
            Splined::new(noise, Spline::new(&[(-300, 0), (300, 1 << 16)])),
        );
        let blend = Blend::new(&weight, &min, Const(300), 16);
        let warped = Warped::new(Warp::new(seed, fbm(256, 2), 40), &lattice);
        let plane = Plane { a: 3, b: -2, c: 7, lo: -(1 << 40), hi: 1 << 40 };
        let crossing = Min::new(plane, Plane { a: -1, b: 1, c: 5, lo: -(1 << 40), hi: 1 << 40 });
        check_cells("const", &Const(-42), &grid, &points)?;
        check_cells("noise", &noise, &grid, &points)?;
        check_cells("value noise", &value, &grid, &points)?;
        check_cells("lattice", &lattice, &grid, &points)?;
        check_cells("coarse lattice", &coarse, &grid, &points)?;
        check_cells("fine lattice", &fine, &grid, &points)?;
        check_cells("splined", &splined, &grid, &points)?;
        check_cells("add", &add, &grid, &points)?;
        check_cells("min", &min, &grid, &points)?;
        check_cells("weight", &weight, &grid, &points)?;
        check_cells("blend", &blend, &grid, &points)?;
        check_cells("warped", &warped, &grid, &points)?;
        check_cells("planes' min", &crossing, &grid, &points)?;
        // A leaf's grid version is its per-cell version, cell by cell.
        let cells = noise.cells(&grid);
        for i in 0..grid.nx {
            for j in 0..grid.nz {
                let rect = grid.rect(i, j);
                prop_assert_eq!(cells.bounds[i * grid.nz + j], noise.bound(rect));
                prop_assert_eq!(cells.fines[i * grid.nz + j], noise.fine(rect));
            }
        }
    }

    #[test]
    fn noise_split_bounds_the_full_field(
        seed in any::<u64>(), x in place(), z in place(), cut in 0u64..4096,
        basis in prop::sample::select(vec![Basis::Value, Basis::Gradient]),
    ) {
        let full = NoiseField::new(seed, fbm(1024, 6), basis, 30_000);
        let (coarse, dropped) = full.split_at(cut);
        prop_assert!(full.value(x, z).abs_diff(coarse.value(x, z)) <= dropped);
        prop_assert!(coarse.slope().per_block <= full.slope().per_block);
    }

    #[test]
    fn spline_keeps_its_bounds(
        x in -3000i64..3000, y in -3000i64..3000, lo in -1500i64..1500, len in 0i64..1200,
    ) {
        let s = spline();
        // Slope: |S(u) - S(v)| < m |u - v| + 1.
        let (a, b) = (s.eval(x), s.eval(y));
        let m = u128::from(s.max_slope().raw());
        prop_assert!((u128::from(a.abs_diff(b)) << 16) < m * u128::from(x.abs_diff(y)) + (1 << 16));
        // Range over an interval, against every input in it.
        let input = Span::new(lo, lo + len);
        let r = s.range_over(input);
        let (min, max) = (lo..=lo + len).map(|v| s.eval(v)).fold((i64::MAX, i64::MIN), |(a, b), v| (a.min(v), b.max(v)));
        prop_assert_eq!(r, Span::new(min, max));
        // Slopes over it hold every step inside.
        let (m_lo, m_hi) = s.slopes_over(input);
        for v in lo..lo + len {
            let step = i128::from(s.eval(v + 1) - s.eval(v)) << 16;
            prop_assert!(m_lo - (1 << 16) < step && step < m_hi + (1 << 16), "{v}: {step}");
        }
    }
}

#[test]
fn bounds_stay_local_on_big_cells() {
    // A wavelength of 4096 blocks and cells of 64: the bound is the center value plus and minus
    // the slope over the half diagonal, far inside the range, for the noise, a fine lattice
    // over it (past the 64 lattice points it reads one by one) and a warp of it.
    let noise = NoiseField::new(3, fbm(4096, 3), Basis::Gradient, 1 << 20);
    let lattice = LatticeField::new(2, noise);
    let warped = Warped::new(Warp::new(3, fbm(512, 2), 16), noise);
    for origin in [0, 1 << 40, MAX_BLOCK_COORD - 64] {
        let rect = Rect::cell(origin, -origin, 64);
        for (name, b, f) in [
            ("noise", noise.bound(rect), &noise as &dyn Field),
            ("lattice", lattice.bound(rect), &lattice),
            ("warped", warped.bound(rect), &warped),
        ] {
            let slope = f.slope().over(46);
            assert!(b.width() <= 2 * slope, "{name}: {b:?} over {slope}");
            assert!(b.width() < noise.range().width() / 8, "{name}: {b:?}");
            assert!(b.contains(f.value(rect.center_x(), rect.center_z())));
        }
    }
}

/// A leaf that counts the values it computes, with the slope defaults and their grid form.
#[derive(Debug)]
struct Counted<F> {
    f: F,
    calls: std::cell::Cell<u64>,
}

impl<F: Field> Field for Counted<F> {
    fn value(&self, x: i64, z: i64) -> i64 {
        self.calls.set(self.calls.get() + 1);
        self.f.value(x, z)
    }
    fn grid(&self, xs: &[i64], zs: &[i64], out: &mut [i64]) {
        self.calls.set(self.calls.get() + out.len() as u64);
        self.f.grid(xs, zs, out);
    }
    fn range(&self) -> Span {
        self.f.range()
    }
    fn slope(&self) -> Slope {
        self.f.slope()
    }
    fn cells(&self, grid: &CellGrid) -> Cells {
        slope_cells(self, grid)
    }
}

#[test]
fn node_cells_cost_a_few_grids_per_leaf() {
    // The bench's tree (Blend, Min, Add, Splined, three lattices) over a LOD node of 34 x 34
    // cells: per cell, bound and fine asked every leaf about a hundred values; the node's cells
    // ask each leaf a few grids.
    let terrain = fbm(256, 5);
    let meander = Fbm {
        frequency: Frequency::one_in(512),
        octaves: 3,
        lacunarity: 2,
        gain: 0.5,
    };
    let count = |f| Counted {
        f,
        calls: std::cell::Cell::new(0),
    };
    let (n1, n2, n3) = (
        count(NoiseField::new(7, terrain, Basis::Gradient, 1000)),
        count(NoiseField::new(8, terrain, Basis::Gradient, 300)),
        count(NoiseField::new(9, meander, Basis::Gradient, 40)),
    );
    let lattice = LatticeField::new(5, &n1);
    let weight = LatticeField::new(
        8,
        Splined::new(&n2, Spline::new(&[(-300, 0), (300, 1 << 16)])),
    );
    let relief = Add::new(LatticeField::new(2, &n3), Splined::new(&lattice, spline()));
    let tree = Blend::new(&weight, Min::new(&relief, Const(1500)), Const(64), 16);
    let leaves = || n1.calls.take() + n2.calls.take() + n3.calls.take();
    let mut report = Vec::new();
    for level in [1u32, 4, 6] {
        let s = 1i64 << level;
        let grid = CellGrid::new(32 * s, 64 * s, s, 34, 34);
        let _ = leaves();
        let cells = tree.cells(&grid);
        let node = leaves();
        let mut per_cell = 0;
        for (i, j) in [(0, 0), (17, 9), (33, 33)] {
            let rect = grid.rect(i, j);
            let _ = (tree.bound(rect), tree.fine(rect));
            per_cell += leaves();
        }
        let per_cell_node = per_cell / 3 * 34 * 34;
        assert!(
            node * 5 < per_cell_node,
            "level {level}: {node} against {per_cell_node}"
        );
        report.push(format!(
            "level {level}: {node} per node, {per_cell_node} per cell"
        ));
        assert!(
            node < 12 * 35 * 35,
            "level {level}: {node} leaf values per node"
        );
        assert_eq!(cells.bounds.len(), 34 * 34);
    }
    // WG1_COUNTS=1 shows the counts.
    assert!(std::env::var("WG1_COUNTS").is_err(), "{report:?}");
}

#[test]
fn min_needs_its_crossing_term() {
    // min(x, s - x) on [0, s]: both linear (fine 0), 0 at both corners, s / 2 in the middle.
    let s = 1 << 10;
    for origin in [0, 1 << 31, 1 << 40, MAX_BLOCK_COORD - s] {
        let rising = Plane {
            a: 1,
            b: 0,
            c: -origin,
            lo: -(1 << 62),
            hi: 1 << 62,
        };
        let falling = Plane {
            a: -1,
            b: 0,
            c: origin + s,
            lo: -(1 << 62),
            hi: 1 << 62,
        };
        let rect = Rect::cell(origin, origin, s);
        assert_eq!(
            (rising.fine(rect), falling.fine(rect)),
            (Fine::ZERO, Fine::ZERO)
        );
        let min = Min::new(rising, falling);
        assert_eq!(corner_values(&min, rect), [0; 4]);
        assert_eq!(min.value(origin + s / 2, origin + 7), s / 2);
        // Exactly the crossing term: no less (the maximum of the fines would be 0), no more.
        assert_eq!(
            min.fine(rect),
            Fine {
                up: s as u64 / 2,
                down: 0
            }
        );
    }
}

#[test]
fn blend_term_covers_both_axes() {
    // A weight climbing 0 -> 2^16 over L = 512 blocks along x and along z (a pyramid's flank),
    // a = D, b = 0: inside a cell of s blocks the term is (D / L) * s over both axes, not s / 2.
    let (l, d, s) = (512u64, 1000u64, 32i64);
    let w = Plane {
        a: 128,
        b: 128,
        c: 0,
        lo: 0,
        hi: 1 << 16,
    };
    let blend = Blend::new(w, Const(d as i64), Const(0), 16);
    let rect = Rect::cell(32, 32, s);
    // The weight moves at most its slope over half the diagonal from the corners on average.
    let moved = w.slope().over(rect.half_diagonal());
    let term = (d * moved).div_ceil(1 << 16);
    assert_eq!(blend.fine(rect), Fine::both(term + 1));
    let want = (d * s as u64) as f64 / l as f64;
    assert!(
        term as f64 >= want && (term as f64) < want * 1.05,
        "{term} vs {want}"
    );
}

#[test]
fn blend_term_is_needed_where_the_inputs_cross() {
    // a - b goes from +D to -D across the cell while the weight goes from b to a: the blend is
    // -D/2 at both ends and 0 in the middle, D/2 over the interpolant. The term must cover it.
    let (s, d) = (1i64 << 8, 1i64 << 10);
    let a = Plane {
        a: -d / s,
        b: 0,
        c: d / 2,
        lo: -d,
        hi: d,
    };
    let b = Plane {
        a: d / s,
        b: 0,
        c: -d / 2,
        lo: -d,
        hi: d,
    };
    let w = Plane {
        a: (1 << 16) / s,
        b: 0,
        c: 0,
        lo: 0,
        hi: 1 << 16,
    };
    let blend = Blend::new(w, a, b, 16);
    let rect = Rect::cell(0, 0, s);
    assert_eq!(corner_values(&blend, rect), [-d / 2; 4]);
    let middle = blend.value(s / 2, s / 2);
    assert_eq!(middle, 0);
    assert!(blend.fine(rect).up >= (middle + d / 2) as u64);
}

/// A test field on a `2^shift` lattice: `r` on the lattice points with `i + j` odd, 0 on the
/// others, and an unbounded slope, so a lattice over it sees neighbors `r` apart on both axes.
#[derive(Clone, Copy, Debug)]
struct Checker {
    shift: u32,
    r: i64,
}

impl Field for Checker {
    fn value(&self, x: i64, z: i64) -> i64 {
        if ((x >> self.shift) + (z >> self.shift)) & 1 == 1 {
            self.r
        } else {
            0
        }
    }
    fn range(&self) -> Span {
        Span::new(0, self.r)
    }
    fn slope(&self) -> Slope {
        Slope {
            per_block: Q16::MAX,
            jitter: 0,
        }
    }
}

#[test]
fn lattice_slope_needs_sqrt_2() {
    // Neighbors r apart on both axes: one block along the diagonal from a lattice point climbs
    // 2 r / S (minus the curvature), over sqrt(2) blocks. Without the sqrt(2) the bound would
    // allow only sqrt(2) r / S.
    let (shift, r) = (4, 1 << 20);
    let field = LatticeField::new(shift, Checker { shift, r });
    let (base, step) = (field.value(0, 16), field.value(1, 17));
    let climb = base.abs_diff(step);
    assert!(climb > (r as u64 >> shift) * 3 / 2, "{climb}");
    assert!(slope_holds(climb, field.slope(), 2));
}

/// A test field: a Chebyshev pyramid `h - max(|x - cx|, |z - cz|)` floored at 0, with its exact
/// slope (1 per block along any line) and the default fine.
#[derive(Clone, Copy, Debug)]
struct Pyramid {
    cx: i64,
    cz: i64,
    h: i64,
}

impl Field for Pyramid {
    fn value(&self, x: i64, z: i64) -> i64 {
        (self.h - (x - self.cx).abs().max((z - self.cz).abs())).max(0)
    }
    fn range(&self) -> Span {
        Span::new(0, self.h)
    }
    fn slope(&self) -> Slope {
        Slope {
            per_block: Q16::ONE,
            jitter: 0,
        }
    }
    fn bound(&self, rect: Rect) -> Span {
        // The peak if the rectangle holds it, else the nearest point to it.
        let x = self.cx.clamp(rect.x0, rect.x1);
        let z = self.cz.clamp(rect.z0, rect.z1);
        let far = rect
            .corners()
            .iter()
            .map(|&(x, z)| self.value(x, z))
            .min()
            .unwrap();
        Span::new(far, self.value(x, z))
    }
}

#[test]
fn fine_from_slope_is_tight_on_a_cone() {
    // Corners s/2 from the peak in Chebyshev terms, the peak s/2 over their interpolant: the
    // slope gives s / sqrt(2), the bound s / 2 exactly, and the smaller one is the fine.
    let s = 1 << 10;
    for c in [0, 1 << 40, MAX_BLOCK_COORD - s] {
        let p = Pyramid {
            cx: c + s / 2,
            cz: c + s / 2,
            h: 1 << 20,
        };
        let rect = Rect::cell(c, c, s);
        let fine = p.fine(rect);
        assert_eq!(fine.up, s as u64 / 2);
        assert_eq!(p.value(c + s / 2, c + s / 2) - p.value(c, c), s / 2);
    }
}

#[test]
fn lattice_is_exact_at_any_distance() {
    let child = NoiseField::new(5, fbm(64, 2), Basis::Gradient, 1 << 30);
    for shift in [0, 3, 5, 8, 12] {
        let field = LatticeField::new(shift, child);
        let s = 1i64 << shift;
        for origin in [
            0,
            1 << 31,
            -(1 << 31),
            1 << 40,
            MAX_BLOCK_COORD - 2 * s,
            -MAX_BLOCK_COORD,
        ] {
            // A LOD corner on a lattice point reads the child there.
            let (i, j) = (origin >> shift, (origin >> shift) + 3);
            assert_eq!(
                field.value(i << shift, j << shift),
                child.value(i << shift, j << shift)
            );
            // Every point of a cell: floor of the exact weighted mean.
            let (x0, z0) = (i << shift, j << shift);
            let v = |a: i64, b: i64| i128::from(child.value(x0 + a * s, z0 + b * s));
            let xs: Vec<i64> = (0..=s.min(40)).map(|k| x0 + k * s / s.min(40)).collect();
            let zs: Vec<i64> = (0..=s.min(40)).map(|k| z0 + k * s / s.min(40)).collect();
            let mut grid = vec![0; xs.len() * zs.len()];
            field.grid(&xs, &zs, &mut grid);
            for (a, &x) in xs.iter().enumerate() {
                for (b, &z) in zs.iter().enumerate() {
                    let (fx, fz) = (i128::from(x - x0), i128::from(z - z0));
                    let (sx, sz) = (i128::from(s) - fx, i128::from(s) - fz);
                    let sum = sx * sz * v(0, 0)
                        + fx * sz * v(1, 0)
                        + sx * fz * v(0, 1)
                        + fx * fz * v(1, 1);
                    let got = field.value(x, z);
                    let s2 = i128::from(s) * i128::from(s);
                    assert!(
                        i128::from(got) * s2 <= sum && sum < (i128::from(got) + 1) * s2,
                        "({x}, {z})"
                    );
                    assert_eq!(grid[a * zs.len() + b], got, "grid ({x}, {z})");
                }
            }
            // On a whole lattice cell the field never stands over its interpolant.
            assert_eq!(field.fine(Rect::cell(x0, z0, s)), Fine { up: 0, down: 1 });
        }
    }
}

#[test]
fn lattice_takes_the_wide_path_for_big_values() {
    // Values near 2^40 on a 2^12 lattice: the weighted sums need i128.
    let child = NoiseField::new(9, fbm(4096, 1), Basis::Gradient, 1 << 40);
    let field = LatticeField::new(12, child);
    assert!(!field.narrow);
    let small = LatticeField::new(12, NoiseField::new(9, fbm(4096, 1), Basis::Gradient, 1000));
    assert!(small.narrow);
    for (x, z) in [
        (17, 4000),
        (1 << 40, -(1 << 40) + 77),
        (MAX_BLOCK_COORD - 5, 3),
    ] {
        let (i, j) = (x >> 12, z >> 12);
        let corner = |a: i64, b: i64| i128::from(child.value((i + a) << 12, (j + b) << 12));
        let (fx, fz) = (i128::from(x & 4095), i128::from(z & 4095));
        let sum = (4096 - fx) * (4096 - fz) * corner(0, 0)
            + fx * (4096 - fz) * corner(1, 0)
            + (4096 - fx) * fz * corner(0, 1)
            + fx * fz * corner(1, 1);
        assert_eq!(i128::from(field.value(x, z)), sum >> 24);
    }
}

#[test]
fn spline_is_exact_at_its_knots_and_flat_outside() {
    let s = spline();
    for (x, y) in s.knots() {
        assert_eq!(s.eval(x), y);
    }
    assert_eq!(s.eval(i64::MIN), -200);
    assert_eq!(s.eval(i64::MAX), 2000);
    // Floor between knots: from (-100, 0) to (0, 300), x = -51 gives 147.
    assert_eq!(s.eval(-51), 147);
    // The steepest segment: 300 over 100.
    assert_eq!(s.max_slope(), Q16::from_int(3));
    assert_eq!(s.range(), Span::new(-200, 2000));
    // Only the flat part: slopes 50 / 400 rounded outward.
    assert_eq!(s.slopes_over(Span::new(100, 300)), (8192, 8192));
    assert_eq!(s.slopes_over(Span::new(-5000, -2000)), (0, 0));
    let single = Spline::new(&[(3, 9)]);
    assert_eq!((single.eval(-7), single.max_slope()), (9, Q16::ZERO));
}

#[test]
fn splined_fine_is_tight_where_the_spline_is_linear() {
    // A spline linear over the child's values multiplies the child's fine (here a lattice's 1)
    // instead of the child's whole spread.
    let child = LatticeField::new(5, NoiseField::new(3, fbm(256, 1), Basis::Gradient, 200));
    let doubled = Splined::new(&child, Spline::new(&[(-1000, -2000), (1000, 2000)]));
    let rect = Rect::new(7, 9, 20, 25);
    assert_eq!(child.fine(rect), Fine::both(1));
    assert!(doubled.fine(rect).up <= 3);
}

#[test]
#[should_panic(expected = "leave i64")]
fn add_refuses_ranges_that_overflow() {
    let _ = Add::new(Const(i64::MAX), Const(1));
}

#[test]
#[should_panic(expected = "weight's range")]
fn blend_refuses_a_weight_out_of_scale() {
    let _ = Blend::new(Const(1 << 17), Const(0), Const(1), 16);
}

#[test]
fn spline_takes_knots_at_its_limits() {
    let k = Spline::MAX_KNOT;
    let s = Spline::new(&[(-k, -k), (k, k)]);
    assert_eq!(
        (s.eval(0), s.eval(-k), s.eval(k), s.eval(i64::MIN)),
        (0, -k, k, -k)
    );
    assert_eq!(s.max_slope(), Q16::ONE);
    assert_eq!(s.range_over(Span::new(-k, k)), Span::new(-k, k));
}

#[test]
#[should_panic(expected = "strictly increasing")]
fn spline_refuses_unsorted_knots() {
    let _ = Spline::new(&[(0, 0), (0, 1)]);
}
