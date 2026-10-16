//! The region map's selector (ADR 0039 sec. 4): the rules that give every lattice point a kind
//! ([`SelectorRule`]), the entries mods add to them ([`RuleEntry`]) and the runtime selector
//! built from them at world open.

use super::kind::{Context, TerrainKind};
use crate::noise::{Cellular, SiteId, mix_seed, splitmix64};
use crate::registry::{
    Checks, DigestBuilder, DigestScope, GenEntry, GenId, GenIx, GenRegistry, HashParams, Order,
    ParamsHasher, Problem,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The largest weight of a selector entry.
pub const MAX_WEIGHT: u8 = 64;

/// The default size of a region cell: `2^12` blocks (regions of 2 to 6 km).
pub const DEFAULT_CELL_SHIFT: u32 = 12;

/// Salt of the region cells' sites (mixed with the realm's seed): every `Cell` rule of a realm
/// with one cell size reads the same cells.
const SALT_CELLS: u64 = 0x5245_4749_4f4e_0001;

/// The `SplitMix64` increment: a rendezvous score's samples are a `SplitMix64` stream.
const GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;

/// One entry of a rule: a kind and its weight, `1..=64`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    /// The kind's id.
    pub kind: GenId,
    /// Its weight: it wins about `weight / W` of the cells, `W` the sum of the rule's weights.
    pub weight: u8,
}

impl Entry {
    /// `kind` with weight `weight`.
    pub fn new(kind: GenId, weight: u8) -> Self {
        Self { kind, weight }
    }
}

impl HashParams for Entry {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.kind).write(&self.weight);
    }
}

/// An integer interval of a Context parameter, `lo..=hi`, read at lattice points.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Interval {
    /// The parameter's id ([`Context::params`]).
    pub param: GenId,
    /// The lowest value inside.
    pub lo: i32,
    /// The highest value inside.
    pub hi: i32,
}

impl Interval {
    /// `param` in `lo..=hi`.
    pub fn new(param: GenId, lo: i32, hi: i32) -> Self {
        Self { param, lo, hi }
    }
}

impl HashParams for Interval {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.param).write(&self.lo).write(&self.hi);
    }
}

/// Where a rule picks its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The kind of the point's region cell: a jittered cellular field of cells of `2^shift`
    /// blocks ([`DEFAULT_CELL_SHIFT`] by default), warped by the Context's large warp
    /// ([`Context::warp`]). Every point of a cell gets the same kind, by rendezvous among the
    /// rule's entries, so regions are contiguous.
    Cell(u32),
    /// The kind of the rule's one entry, at every lattice point where the condition holds (a
    /// contour of a field, such as a mountain belt). Exactly one entry: with several it would
    /// pick per lattice point and make confetti.
    Point,
}

impl Scope {
    /// Region cells of the default size.
    pub const CELL: Self = Self::Cell(DEFAULT_CELL_SHIFT);
}

impl HashParams for Scope {
    fn hash_params(&self, h: &mut ParamsHasher) {
        match self {
            Self::Cell(shift) => h.write(&0u8).write(shift),
            Self::Point => h.write(&1u8),
        };
    }
}

/// A rule of a realm's region map (ADR 0039 sec. 4), registered under its id
/// (`game:land_cells`). At each lattice point the first rule of the realm whose condition holds
/// decides, in the order of its registry: the higher priority first, then by id. Every map ends
/// with a fallback rule, of the lowest priority, with no condition and a [`Scope::Cell`] scope,
/// so every point gets a kind.
///
/// A mod adds an entry to an existing rule with a [`RuleEntry`] (a registered value cannot be
/// appended to), and a rule to a realm by naming the realm.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SelectorRule {
    /// The realm whose map it belongs to.
    pub realm: GenId,
    /// Its priority: higher decides first.
    pub priority: i32,
    /// Every interval must hold (none: always).
    pub condition: Vec<Interval>,
    /// Where it picks.
    pub scope: Scope,
    /// Its own entries; the [`RuleEntry`]s naming it add more.
    pub entries: Vec<Entry>,
    /// The version of the entry.
    pub version: u32,
}

impl HashParams for SelectorRule {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.realm)
            .write(&self.condition)
            .write(&self.scope)
            .write(&self.entries);
    }
}

/// Ordered by priority (the registry hashes the order with the parameters).
impl GenEntry for SelectorRule {
    fn version(&self) -> u32 {
        self.version
    }
    fn order(&self) -> Order {
        Order::new(0, self.priority)
    }
}

/// An entry added to an existing [`SelectorRule`] (by a mod), registered under its own id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RuleEntry {
    /// The rule it joins.
    pub rule: GenId,
    /// The kind and its weight.
    pub entry: Entry,
    /// The version of the entry.
    pub version: u32,
}

impl HashParams for RuleEntry {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.rule).write(&self.entry);
    }
}

impl GenEntry for RuleEntry {
    fn version(&self) -> u32 {
        self.version
    }
}

/// A rule ready to run: its condition on parameter indices and its scope.
#[derive(Clone, Debug)]
pub(crate) struct Rule {
    /// `(parameter index, lo, hi)`.
    pub(crate) condition: Box<[(usize, i32, i32)]>,
    pub(crate) pick: Pick,
}

/// How a rule picks.
#[derive(Clone, Debug)]
pub(crate) enum Pick {
    /// The one kind.
    Point(u32),
    /// Rendezvous among `entries` (sorted by kind id) over the region cells of `cells`.
    Cell {
        cells: Cellular,
        entries: Box<[Candidate]>,
    },
}

/// An entry ready to run.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Candidate {
    /// `mix_seed(rule seed, kind salt)`.
    pub(crate) seed: u64,
    pub(crate) weight: u32,
    /// The kind's index among the realm's kinds.
    pub(crate) kind: u32,
}

/// The runtime selector of a realm: its rules in decision order.
#[derive(Clone, Debug)]
pub(crate) struct Selector {
    pub(crate) rules: Box<[Rule]>,
}

impl Candidate {
    /// The entry's rendezvous score in `cell`: the largest of its `weight` samples. Adding or
    /// removing another entry never changes it.
    #[inline]
    pub(crate) fn score(&self, cell: SiteId) -> u64 {
        let base = splitmix64(splitmix64(self.seed ^ cell.i as u64) ^ cell.j as u64);
        (0..u64::from(self.weight))
            .map(|t| splitmix64(base.wrapping_add(GAMMA.wrapping_mul(t))))
            .max()
            .expect("a weight of at least 1")
    }
}

/// The region cells of `shift` of a realm whose seed is `realm_seed`, with sites jittered over
/// the whole cell.
pub(crate) fn region_cells(realm_seed: u64, shift: u32) -> Cellular {
    Cellular::new(mix_seed(realm_seed, SALT_CELLS), shift, 0)
}

impl Selector {
    /// The kind of lattice point `(qx, qz)` (in blocks): the pick of the first rule whose
    /// condition holds there. `params` caches the parameters read so far.
    pub(crate) fn select<Cx: Context>(&self, cx: &Cx, qx: i64, qz: i64) -> u32 {
        let mut params: Vec<Option<i32>> = Vec::new();
        let mut warped: Option<(i64, i64)> = None;
        for rule in &self.rules {
            let holds = rule.condition.iter().all(|&(param, lo, hi)| {
                if params.len() <= param {
                    params.resize(param + 1, None);
                }
                let v = *params[param].get_or_insert_with(|| cx.param(param, qx, qz));
                (lo..=hi).contains(&v)
            });
            if !holds {
                continue;
            }
            return match &rule.pick {
                Pick::Point(kind) => *kind,
                Pick::Cell { cells, entries } => {
                    let (wx, wz) = *warped.get_or_insert_with(|| {
                        let (dx, dz) = cx.warp(qx, qz);
                        (qx.saturating_add(dx), qz.saturating_add(dz))
                    });
                    let cell = cells.nearest(wx, wz).f1.id;
                    rendezvous(entries, cell)
                }
            };
        }
        unreachable!("the freeze check requires a fallback rule")
    }
}

/// The kind of the entry with the highest score in `cell`, ties to the first (the smaller id).
pub(crate) fn rendezvous(entries: &[Candidate], cell: SiteId) -> u32 {
    let mut best: Option<(u64, u32)> = None;
    for e in entries {
        let score = e.score(cell);
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, e.kind));
        }
    }
    best.expect("the freeze check requires an entry").1
}

/// The registry name a condition's parameters are looked up in, for [`Problem::Unknown`]: a
/// realm's [`Context::params`].
pub const PARAM_REGISTRY: &str = "context_param";

/// What resolving a realm's rules gives: the runtime selector and the kinds its entries reach, in
/// their registry's order (a kind's index among them is what the selector returns).
pub(crate) struct Resolved<Cx: Context> {
    pub(crate) selector: Selector,
    pub(crate) kinds: Vec<GenIx<Arc<dyn TerrainKind<Cx>>>>,
}

/// The rules of `realm` resolved at world open: every rule of `rules` that names the realm, in
/// decision order, with its own entries and the [`RuleEntry`]s that name it. Their lines go to
/// `digest` under `scope`; their problems to `checks` (unknown kinds and parameters, weights,
/// duplicates, entry counts, cell sizes, the fallback rule). `None` if there was one.
pub(crate) fn resolve<Cx: Context>(
    checks: &mut Checks,
    digest: &mut DigestBuilder,
    scope: &DigestScope,
    (realm, realm_seed, lattice): (&GenId, u64, u32),
    cx: &Cx,
    rules: &GenRegistry<SelectorRule>,
    entries: &GenRegistry<RuleEntry>,
    kinds: &GenRegistry<Arc<dyn TerrainKind<Cx>>>,
) -> Option<Resolved<Cx>> {
    let before = checks.found();
    let mut added: BTreeMap<&GenId, Vec<GenIx<RuleEntry>>> = BTreeMap::new();
    for (ix, _, e) in entries.iter() {
        added.entry(&e.rule).or_default().push(ix);
    }
    // Per rule: its condition on parameter indices, and its entries as (kind, weight).
    let mut resolved: Vec<(
        &GenId,
        &SelectorRule,
        Vec<(usize, i32, i32)>,
        Vec<(GenIx<_>, u32)>,
    )> = Vec::new();
    for (ix, id, rule) in rules.iter().filter(|(_, _, r)| r.realm == *realm) {
        digest.add(scope, rules, ix);
        let mut condition = Vec::with_capacity(rule.condition.len());
        for interval in &rule.condition {
            match cx.params().iter().position(|p| *p == interval.param) {
                Some(param) => condition.push((param, interval.lo, interval.hi)),
                None => checks.push(Problem::Unknown {
                    registry: PARAM_REGISTRY,
                    id: interval.param.clone(),
                    by: Some(id.clone()),
                }),
            }
        }
        // Its own entries, then the added ones, each with the id that names its kind.
        let mut listed: Vec<(&Entry, &GenId)> = rule.entries.iter().map(|e| (e, id)).collect();
        for &e in added.get(id).map_or(&[][..], Vec::as_slice) {
            digest.add(scope, entries, e);
            listed.push((&entries[e].entry, entries.id(e)));
        }
        let mut picks = Vec::with_capacity(listed.len());
        for &(entry, by) in &listed {
            if !(1..=MAX_WEIGHT).contains(&entry.weight) {
                checks.push(Problem::EntryWeight {
                    rule: id.clone(),
                    kind: entry.kind.clone(),
                    weight: entry.weight,
                });
            }
            if let Some(k) = checks.resolve(kinds, &entry.kind, Some(by)) {
                picks.push((k, u32::from(entry.weight)));
            }
        }
        let mut seen: Vec<&GenId> = listed.iter().map(|(e, _)| &e.kind).collect();
        seen.sort();
        for twice in seen.chunk_by(|a, b| a == b).filter(|run| run.len() > 1) {
            checks.push(Problem::DuplicateEntry {
                rule: id.clone(),
                kind: twice[0].clone(),
            });
        }
        let point = rule.scope == Scope::Point;
        if listed.len() != 1 && (point || listed.is_empty()) {
            checks.push(Problem::RuleEntries {
                rule: id.clone(),
                point,
                kinds: listed.iter().map(|(e, _)| e.kind.clone()).collect(),
            });
        }
        if let Scope::Cell(shift) = rule.scope
            && !(lattice..=Cellular::MAX_SHIFT).contains(&shift)
        {
            checks.push(Problem::CellSize {
                rule: id.clone(),
                shift,
                lattice,
                max: Cellular::MAX_SHIFT,
            });
        }
        resolved.push((id, rule, condition, picks));
    }
    let last = resolved.last();
    if !last.is_some_and(|(_, r, ..)| r.condition.is_empty() && r.scope != Scope::Point) {
        checks.push(Problem::NoFallback {
            realm: realm.clone(),
            last: last.map(|(id, ..)| (*id).clone()),
        });
    }
    if checks.found() > before {
        return None;
    }
    let mut reached: Vec<GenIx<_>> = resolved
        .iter()
        .flat_map(|(.., picks)| picks.iter().map(|&(k, _)| k))
        .collect();
    reached.sort();
    reached.dedup();
    let local = |k: GenIx<_>| reached.binary_search(&k).expect("reached") as u32;
    let rules = resolved
        .into_iter()
        .map(|(id, rule, condition, picks)| {
            let pick = match rule.scope {
                Scope::Point => Pick::Point(local(picks[0].0)),
                Scope::Cell(shift) => {
                    let seed = id.seed(realm_seed);
                    let mut candidates: Vec<Candidate> = picks
                        .iter()
                        .map(|&(k, weight)| Candidate {
                            seed: mix_seed(seed, kinds.salt(k)),
                            weight,
                            kind: local(k),
                        })
                        .collect();
                    // Ties go to the smaller kind id: the kinds' order.
                    candidates.sort_by_key(|c| c.kind);
                    Pick::Cell {
                        cells: region_cells(realm_seed, shift),
                        entries: candidates.into(),
                    }
                }
            };
            Rule {
                condition: condition.into(),
                pick,
            }
        })
        .collect();
    Some(Resolved {
        selector: Selector { rules },
        kinds: reached,
    })
}

/// The check that every [`RuleEntry`] names a registered rule ([`Problem::Unknown`] with the
/// entry as the one that names it): an entry for a missing rule would never be read.
/// [`RegionGenerator::build`](super::RegionGenerator::build) runs it (each realm's build finds the
/// same problems, and [`Checks`] reports each once).
pub fn check_rule_entries(
    checks: &mut Checks,
    rules: &GenRegistry<SelectorRule>,
    entries: &GenRegistry<RuleEntry>,
) {
    for (_, id, e) in entries.iter() {
        checks.resolve(rules, &e.rule, Some(id));
    }
}
