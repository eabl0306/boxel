//! The freeze checks of a realm's kinds (ADR 0039 sec. 6 and 12): their forms, their limits, and
//! the slope and border relief of every kind, pair and triple that can meet.

use super::MAX_SLOPE;
use super::kind::{HEIGHT_ONE, KindForm, KindLimits};
use crate::registry::{Checks, GenId, Problem};

/// Whether a set of kinds whose steepest slope is `slope` (raw Q16, in 1/256 block per block) and
/// whose largest border relief is `relief` (in 1/256 block) keeps the blend under the 3:1 cap on
/// a lattice of `2^lattice` blocks.
///
/// Along any line, `grad H = sum_k w_k grad H_k + sum_k (H_k - m) grad w_k`: the first term is at
/// most the steepest slope, and a step of the 3 x 3 box moves the counts of all kinds together by
/// at most 6 of 9, so the second is at most `relief * sqrt(2) / (3 L)` (`relief / 543` at the
/// default lattice). The check is `slope + sqrt(2) relief / (3 L) <= 3`, exact: with `R = 3 -
/// slope` (in Q16 of 1/256 block), `2 relief^2 2^32 <= 9 L^2 R^2`. A left side past `u128` (a
/// relief over `2^47`, far beyond any cap) fails.
pub fn slope_passes(slope: u64, relief: u64, lattice: u32) -> bool {
    let cap = (u64::from(MAX_SLOPE) * u64::from(HEIGHT_ONE.unsigned_abs())) << 16;
    let Some(room) = cap.checked_sub(slope) else {
        return false;
    };
    let (room, relief) = (u128::from(room), u128::from(relief));
    let Some(lhs) = relief
        .checked_mul(relief)
        .and_then(|r| r.checked_mul(1 << 33))
    else {
        return false;
    };
    // `9 L^2 R^2 < 2^4 2^(2 lattice) 2^52`: inside `u128` up to a lattice of 2^36 (a generator's
    // is at most 2^12).
    let l = 1u128 << lattice;
    lhs <= 9 * l * l * room * room
}

/// The largest difference between two kinds inside a blend band: `max(hi_a - lo_b, hi_b -
/// lo_a)` of their band reliefs (never negative: the two sum to both widths), exact for any
/// bands (at most `2^64 - 1`).
pub fn border_relief(a: &KindLimits, b: &KindLimits) -> u64 {
    let (a, b) = (a.band, b.band);
    let d = (i128::from(a.hi) - i128::from(b.lo)).max(i128::from(b.hi) - i128::from(a.lo));
    u64::try_from(d).unwrap_or(u64::MAX)
}

/// The checks of the kinds of `realm` (each id with its form and limits), in `checks`:
/// - every form is built ([`Problem::KindFormNotBuilt`]);
/// - every band relief lies inside its relief ([`Problem::KindLimits`]);
/// - every kind alone, every pair and every triple passes [`slope_passes`] with its steepest slope
///   and largest border relief ([`Problem::KindSlope`]; a pair is reported only when its two kinds
///   pass alone, a triple only when its three pairs pass). A kind's slope there is its slope over
///   a step of one block, `per_block + jitter`: its rounding counts.
///
/// Any two kinds of a realm may meet in a window: the rules' conditions cannot tell where they
/// will hold, so the check takes every pair and triple of the realm. Triples are enough for any
/// set: a set passes if the triple of its steepest kind and its farthest pair does. A kind alone
/// is the interior of its regions, where its own slope is the terrain's.
pub fn check_kinds(
    checks: &mut Checks,
    realm: &GenId,
    kinds: &[(GenId, KindForm, KindLimits)],
    lattice: u32,
) {
    for (id, form, limits) in kinds {
        if !form.is_built() {
            checks.push(Problem::KindFormNotBuilt {
                kind: id.clone(),
                form: *form,
            });
        }
        let (band, relief) = (limits.band, limits.relief);
        if band.lo < relief.lo || band.hi > relief.hi {
            checks.push(Problem::KindLimits { kind: id.clone() });
        }
    }
    // A kind's slope over one block, its rounding included: `per_block + jitter`, exact in Q16.
    let slope = |k: usize| {
        let s = kinds[k].2.slope;
        s.per_block
            .raw()
            .saturating_add(s.jitter.saturating_mul(1 << 16))
    };
    let relief = |a: usize, b: usize| border_relief(&kinds[a].2, &kinds[b].2);
    let pair = |a: usize, b: usize| slope_passes(slope(a).max(slope(b)), relief(a, b), lattice);
    let fail = |checks: &mut Checks, set: &[usize], slope: u64, relief: u64| {
        checks.push(Problem::KindSlope {
            realm: realm.clone(),
            kinds: set.iter().map(|&k| kinds[k].0.clone()).collect(),
            slope,
            relief,
            lattice,
        });
    };
    let n = kinds.len();
    let alone = |k: usize| slope_passes(slope(k), 0, lattice);
    for k in 0..n {
        if !alone(k) {
            fail(checks, &[k], slope(k), 0);
        }
    }
    for a in 0..n {
        for b in a + 1..n {
            if alone(a) && alone(b) && !pair(a, b) {
                fail(checks, &[a, b], slope(a).max(slope(b)), relief(a, b));
            }
        }
    }
    for a in 0..n {
        for b in a + 1..n {
            for c in b + 1..n {
                if !(pair(a, b) && pair(a, c) && pair(b, c)) {
                    continue;
                }
                let s = slope(a).max(slope(b)).max(slope(c));
                let d = relief(a, b).max(relief(a, c)).max(relief(b, c));
                if !slope_passes(s, d, lattice) {
                    fail(checks, &[a, b, c], s, d);
                }
            }
        }
    }
}
