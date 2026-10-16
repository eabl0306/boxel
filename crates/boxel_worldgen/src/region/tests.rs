//! The blend's exact integers (ADR 0039 "Cómo se prueba" 1 and 2): the weights sum to exactly
//! `9 L^2` at every column of every lattice, the blend with any number of kinds at extreme heights
//! never overflows `i64` and equals an `i128` reference, the dominant kind is the largest weight,
//! and the slope check is exact at its edge. The generator itself is tested through the public
//! API in `tests/region.rs`.
use super::blend::{self, BOX, Window};
use super::*;
use proptest::prelude::*;

/// A window of up to `kinds` kinds from the bits of `seed`.
fn window(seed: u64, kinds: u32) -> Window {
    Window(std::array::from_fn(|n| {
        (crate::noise::splitmix64(seed ^ n as u64) % u64::from(kinds)) as u32
    }))
}

proptest! {
    #[test]
    fn weights_sum_to_nine_lattice_squares(
        seed in any::<u64>(),
        kinds in 1u32..=16,
        shift in MIN_LATTICE_SHIFT..=MAX_LATTICE_SHIFT,
        fx in any::<u32>(),
        fz in any::<u32>(),
    ) {
        let w = window(seed, kinds);
        let m = (1i64 << shift) - 1;
        let f = blend::factors(i64::from(fx) & m, i64::from(fz) & m, shift);
        prop_assert_eq!(f.iter().sum::<i64>(), 1 << (2 * shift));
        let present = w.present();
        let total: i64 = present.iter().map(|&(_, n)| blend::weight(n, f)).sum();
        prop_assert_eq!(total, blend::total(shift));
        prop_assert_eq!(blend::total(shift), BOX << (2 * shift));
        // Each corner's box holds 9 points.
        for c in 0..4 {
            prop_assert_eq!(present.iter().map(|&(_, n)| n[c]).sum::<i64>(), BOX);
        }
        // At a cell corner the weights are the corner's counts.
        let at_q00 = blend::factors(0, 0, shift);
        for &(k, n) in &present {
            prop_assert_eq!(blend::weight(n, at_q00), n[0] << (2 * shift));
            prop_assert_eq!(w.counts(k), n);
        }
    }

    #[test]
    fn the_blend_never_overflows_and_matches_i128(
        seed in any::<u64>(),
        kinds in 5u32..=16,
        shift in MIN_LATTICE_SHIFT..=MAX_LATTICE_SHIFT,
        fx in any::<u32>(),
        fz in any::<u32>(),
        extreme in any::<u16>(),
    ) {
        let w = window(seed, kinds);
        let m = (1i64 << shift) - 1;
        let f = blend::factors(i64::from(fx) & m, i64::from(fz) & m, shift);
        // Heights at the ends of `HeightQ` for some kinds, hashed for the others.
        let height = |k: u32| match (extreme >> (k % 16)) & 3 {
            0 => HeightQ::MIN,
            1 => HeightQ::MAX,
            _ => crate::noise::splitmix64(seed.wrapping_add(u64::from(k))) as HeightQ,
        };
        let present = w.present();
        let sum: i64 = present.iter().map(|&(k, n)| blend::weight(n, f) * i64::from(height(k))).sum();
        let wide: i128 = present.iter().map(|&(k, n)| i128::from(blend::weight(n, f)) * i128::from(height(k))).sum();
        prop_assert_eq!(i128::from(sum), wide);
        let h = blend::blend(sum, shift);
        prop_assert_eq!(i128::from(h), wide.div_euclid(i128::from(blend::total(shift))));
        // A convex combination: between the lowest and the highest height present.
        let (lo, hi) = present.iter().fold((HeightQ::MAX, HeightQ::MIN), |(lo, hi), &(k, n)| {
            if blend::weight(n, f) > 0 { (lo.min(height(k)), hi.max(height(k))) } else { (lo, hi) }
        });
        prop_assert!(lo <= h && h <= hi);
    }

    #[test]
    fn the_dominant_kind_has_the_largest_weight(seed in any::<u64>(), kinds in 2u32..=6, fx in 0i64..256, fz in 0i64..256) {
        let w = window(seed, kinds);
        let f = blend::factors(fx, fz, 8);
        let present = w.present();
        let d = blend::dominant(&present, f);
        let best = present.iter().map(|&(_, n)| blend::weight(n, f)).max().unwrap();
        let first = present.iter().find(|&&(_, n)| blend::weight(n, f) == best).unwrap().0;
        prop_assert_eq!(d, first);
    }
}

#[test]
fn a_single_kind_window_is_the_interior() {
    let w = Window([3; 16]);
    assert_eq!(w.single(), Some(3));
    assert_eq!(w.present(), vec![(3, [9, 9, 9, 9])]);
    let mut mixed = w;
    mixed.0[15] = 1;
    assert_eq!(mixed.single(), None);
    // Point (3, 3) lies only in the box of q11.
    assert_eq!(mixed.counts(1), [0, 0, 0, 1]);
}

#[test]
fn the_slope_check_is_exact_at_its_edge() {
    let one = 256 * 65536;
    // No relief: the slope alone, up to 3 included.
    assert!(slope_passes(3 * one, 0, 8));
    assert!(!slope_passes(3 * one + 1, 0, 8));
    // No slope: a relief up to 3 * 768 / sqrt(2) = 1629.1 blocks at the default lattice.
    assert!(slope_passes(0, 1629 * 256, 8));
    assert!(!slope_passes(0, 1630 * 256, 8));
    // A coarser lattice has a wider band: twice the relief.
    assert!(slope_passes(0, 3258 * 256, 9));
    // 2.5 + 271 / 543.06 = 2.9990 passes, 272 blocks (3.00087) does not.
    assert!(slope_passes(one * 5 / 2, 271 * 256, 8));
    assert!(!slope_passes(one * 5 / 2, 272 * 256, 8));
}
