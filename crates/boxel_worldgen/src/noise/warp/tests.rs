//! The warp: offsets inside the amplitude, the grid bit-identical to the point form anywhere,
//! and its stated bounds along random lines (diagonals included), checked exactly.
use super::*;
use crate::noise::Frequency;
use boxel_voxel::MAX_BLOCK_COORD;
use proptest::prelude::*;

fn meander() -> Fbm {
    Fbm {
        frequency: Frequency::one_in(512),
        octaves: 3,
        lacunarity: 2,
        gain: 0.5,
    }
}

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

/// `|a - b| <= slope` for points `d` apart, exactly.
fn within(a: i64, b: i64, slope: Slope, d: (i64, i64)) -> bool {
    let d2 = (d.0 as i128 * d.0 as i128 + d.1 as i128 * d.1 as i128) as u128;
    a.abs_diff(b) <= slope.over_sq(d2)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn offsets_stay_in_range_and_grid_equals_point(
        seed in any::<u64>(), x0 in place(), z0 in place(), amplitude in 0i64..5000,
        step in 1i64..64,
    ) {
        let w = Warp::new(seed, meander(), amplitude);
        let xs: Vec<i64> = (0..8).map(|i| x0 + i * step).collect();
        let zs: Vec<i64> = (0..5).map(|i| z0 - i * step).collect();
        let mut grid = vec![(0, 0); 8 * 5];
        w.offset_grid(&xs, &zs, &mut grid);
        for (i, &x) in xs.iter().enumerate() {
            for (j, &z) in zs.iter().enumerate() {
                let o = w.offset(x, z);
                prop_assert_eq!(grid[i * 5 + j], o);
                prop_assert!(o.0.abs() <= amplitude && o.1.abs() <= amplitude);
            }
        }
    }

    #[test]
    fn offsets_keep_their_slope_along_any_line(
        seed in any::<u64>(), x in place(), z in place(), amplitude in 0i64..5000,
        d in (-4000i64..4000, -4000i64..4000),
    ) {
        let w = Warp::new(seed, meander(), amplitude);
        let (a, b) = (w.offset(x, z), w.offset(x + d.0, z + d.1));
        let c = w.component_slope();
        prop_assert!(within(a.0, b.0, c, d) && within(a.1, b.1, c, d), "{a:?} {b:?}");
        // As a vector: |o(p) - o(q)|^2 <= bound^2.
        let v = w.displacement_slope();
        let d2 = (d.0 * d.0 + d.1 * d.1) as u128;
        let bound = u128::from(v.over_sq(d2));
        let moved = (a.0 - b.0).pow(2) as u128 + (a.1 - b.1).pow(2) as u128;
        prop_assert!(moved <= bound * bound, "{a:?} {b:?} over {d:?}");
    }
}

#[test]
fn warp_seeds_are_mixed_and_distinct() {
    let w = Warp::new(7, meander(), 48);
    let [sx, sz] = w.seeds();
    assert_ne!(sx, sz);
    assert_eq!(sx, mix_seed(7, SALT_X));
    // Not the world seed plus a small number: seeds 7 and 8 share no octave seed.
    let other = Warp::new(8, meander(), 48).seeds();
    for k in 0..64u64 {
        for j in 0..64u64 {
            for (a, b) in [(sx, other[0]), (sx, other[1]), (sz, other[0]), (sx, sz)] {
                assert_ne!(a.wrapping_add(k), b.wrapping_add(j));
            }
        }
    }
    // Zero amplitude: no offset anywhere.
    let still = Warp::new(7, meander(), 0);
    assert_eq!(still.offset(123, -456), (0, 0));
    assert_eq!(still.component_slope().per_block, Q16::ZERO);
}

#[test]
fn fixed_point_warp_bounds_its_map() {
    // A meander warp of 16 blocks kept in 1/16 block: amplitude 256 sixteenths.
    let w = Warp::new(3, meander(), 16 * 16);
    let l = w.displacement_slope();
    // The map p -> 16 p + o(p) stretches by at most 16 + L and shrinks by at most 16 - L.
    assert!(l.per_block < Q16::from_int(16), "{l:?}");
    for i in 0..2000i64 {
        let (x, z) = (i * 37 - 30_000, i * -11 + 9000);
        let (a, b) = (w.offset(x, z), w.offset(x + 1, z + 1));
        let moved = ((16 + b.0 - a.0).pow(2) + (16 + b.1 - a.1).pow(2)) as u128;
        // |step| = sqrt(2) blocks: (16 sqrt(2) +- bound) squared.
        let bound = u128::from(l.over_sq(2));
        assert!(moved <= (23 + bound).pow(2), "{a:?} {b:?}");
    }
}

#[test]
#[should_panic(expected = "amplitude")]
fn warp_refuses_a_huge_amplitude() {
    let _ = Warp::new(0, meander(), (1 << 40) + 1);
}
