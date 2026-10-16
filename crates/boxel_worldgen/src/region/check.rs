//! The freeze checks of a realm's kinds (ADR 0039 sec. 6 and 12): their forms, their limits, the
//! slope and border relief of every kind, pair and triple that can meet, and the shape modifiers'
//! slopes on top of them.

use super::MAX_SLOPE;
use super::kind::{HEIGHT_ONE, KindForm, KindLimits};
use crate::field::Slope;
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
    for (set, slope, relief) in failing_sets(&limits_of(kinds), 0, lattice) {
        checks.push(Problem::KindSlope {
            realm: realm.clone(),
            kinds: set.iter().map(|&k| kinds[k].0.clone()).collect(),
            slope,
            relief,
            lattice,
        });
    }
}

/// The checks of the shape modifiers of `realm` (each id with its declared
/// [`ShapeModifier::slope`](super::ShapeModifier::slope)) on top of its kinds (as
/// [`check_kinds`] takes them), in `checks` ([`Problem::ShapeSlope`]): their slopes, summed (each
/// over one block, its jitter included), pass [`slope_passes`] alone, and added to the steepest
/// slope of every kind, pair and triple of kinds that passes [`check_kinds`] (a set is reported
/// only when its smaller sets pass with the modifiers).
///
/// Every modifier may cut every kind, anywhere, and the modifiers stack (a river through a cliff
/// coast): the conditions do not say where they act, so the check sums them all.
pub fn check_modifiers(
    checks: &mut Checks,
    realm: &GenId,
    kinds: &[(GenId, KindForm, KindLimits)],
    modifiers: &[(GenId, Slope)],
    lattice: u32,
) {
    if modifiers.is_empty() {
        return;
    }
    let added = modifiers
        .iter()
        .fold(0u64, |sum, (_, s)| sum.saturating_add(step(*s)));
    let problem = |kinds: Vec<GenId>, slope, relief| Problem::ShapeSlope {
        realm: realm.clone(),
        modifiers: modifiers.iter().map(|(id, _)| id.clone()).collect(),
        kinds,
        slope,
        added,
        relief,
        lattice,
    };
    if !slope_passes(added, 0, lattice) {
        checks.push(problem(Vec::new(), 0, 0));
        return;
    }
    let limits = limits_of(kinds);
    let without = failing_sets(&limits, 0, lattice);
    for (set, slope, relief) in failing_sets(&limits, added, lattice) {
        // A set that fails without the modifiers is the kinds' problem, reported there.
        if without.iter().any(|(s, _, _)| *s == set) {
            continue;
        }
        let ids = set.iter().map(|&k| kinds[k].0.clone()).collect();
        checks.push(problem(ids, slope, relief));
    }
}

/// A slope over one block, its rounding included: `per_block + jitter`, exact in Q16.
fn step(s: Slope) -> u64 {
    s.per_block
        .raw()
        .saturating_add(s.jitter.saturating_mul(1 << 16))
}

/// The limits of `kinds`.
fn limits_of(kinds: &[(GenId, KindForm, KindLimits)]) -> Vec<KindLimits> {
    kinds.iter().map(|(_, _, limits)| *limits).collect()
}

/// Every kind alone, pair and triple of `kinds` that fails [`slope_passes`] with its steepest
/// slope (over one block) plus `added` and its largest border relief, as `(indices, steepest
/// slope, relief)`: a pair only when its two kinds pass alone, a triple only when its three pairs
/// pass.
fn failing_sets(kinds: &[KindLimits], added: u64, lattice: u32) -> Vec<(Vec<usize>, u64, u64)> {
    let slope = |k: usize| step(kinds[k].slope);
    let relief = |a: usize, b: usize| border_relief(&kinds[a], &kinds[b]);
    let passes = |s: u64, d: u64| slope_passes(s.saturating_add(added), d, lattice);
    let alone = |k: usize| passes(slope(k), 0);
    let pair = |a: usize, b: usize| passes(slope(a).max(slope(b)), relief(a, b));
    let n = kinds.len();
    let mut failing = Vec::new();
    for k in 0..n {
        if !alone(k) {
            failing.push((vec![k], slope(k), 0));
        }
    }
    for a in 0..n {
        for b in a + 1..n {
            if alone(a) && alone(b) && !pair(a, b) {
                failing.push((vec![a, b], slope(a).max(slope(b)), relief(a, b)));
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
                if !passes(s, d) {
                    failing.push((vec![a, b, c], s, d));
                }
            }
        }
    }
    failing
}
