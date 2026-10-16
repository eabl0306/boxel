//! Realm spans, exact up to the ends of `i64`, and every layout check failing with its ids in the
//! message (ADR 0039 "Cómo se prueba" 4 and 11), the realms covering every y exactly once (also
//! against a brute force over random layouts). The realm specs, their floor check and the stack
//! that finds the realm of a `y` at and around every plane. Then a whole world open over
//! test registries (a preset, its dimensions, their realms and kinds): every problem of a bad mix
//! reported at once, and the digest of a good one per dimension and realm, with a realm added as
//! one more line. These presets and dimensions are `test:` stand-ins of the checks alone; the
//! real ones are `crate::preset`'s (tested there and in `tests/preset.rs`).
use super::*;
use crate::registry::{
    DigestBuilder, DigestScope, FreezeError, GenEntry, GenRegistry, GenRegistryBuilder,
    RegistryDigest, WorldgenDiff,
};
use proptest::prelude::*;

fn id(name: &str) -> GenId {
    GenId::new(name).expect(name)
}

const fn ground(span: RealmSpan, lod: RealmLod) -> RealmLayout {
    RealmLayout {
        span,
        form: RealmForm::Ground,
        lod,
    }
}

/// The problems of `realms` in `test:dim`, and the message of their error.
fn check(realms: &[(&str, RealmLayout)]) -> (Vec<Problem>, String) {
    let ids: Vec<(GenId, RealmLayout)> = realms.iter().map(|&(name, l)| (id(name), l)).collect();
    let mut checks = Checks::new();
    check_realms(
        &mut checks,
        &id("test:dim"),
        ids.iter().map(|(id, l)| (id, l)),
    );
    match checks.finish() {
        Ok(()) => (Vec::new(), String::new()),
        Err(e) => (e.problems().to_vec(), e.to_string()),
    }
}

/// Asserts that every name is in `message`.
#[track_caller]
fn names_all(message: &str, names: &[&str]) {
    for name in names {
        assert!(message.contains(name), "{name} not in {message:?}");
    }
}

#[test]
fn spans_are_half_open_with_open_ends() {
    let span = RealmSpan::between(-4096, 12288);
    assert!(span.contains(-4096) && span.contains(12287));
    assert!(!span.contains(-4097) && !span.contains(12288));
    assert!(RealmSpan::below(-4096).contains(i64::MIN));
    assert!(!RealmSpan::below(-4096).contains(-4096));
    assert!(RealmSpan::above(12288).contains(i64::MAX));
    assert!(RealmSpan::ALL.contains(i64::MIN) && RealmSpan::ALL.contains(i64::MAX));
    assert!(!RealmSpan::ALL.is_empty());
    assert!(RealmSpan::between(0, 0).is_empty());
    assert!(RealmSpan::between(4096, 0).is_empty());
    // Spans that meet at a plane do not overlap; an empty span overlaps nothing.
    assert!(!RealmSpan::below(-4096).overlaps(RealmSpan::between(-4096, 12288)));
    assert!(!RealmSpan::above(12288).overlaps(RealmSpan::between(-4096, 12288)));
    assert!(RealmSpan::below(0).overlaps(RealmSpan::above(-4096)));
    assert!(RealmSpan::ALL.overlaps(RealmSpan::between(0, 4096)));
    assert!(!RealmSpan::between(0, 0).overlaps(RealmSpan::ALL));
    assert_eq!(
        RealmSpan::between(-4096, 12288).to_string(),
        "[-4096, 12288)"
    );
    assert_eq!(RealmSpan::ALL.to_string(), "(-inf, +inf)");
    assert_eq!(RealmSpan::below(0).planes().collect::<Vec<_>>(), [0]);
}

#[test]
fn a_good_layout_passes() {
    // The game's vertical layout (ADR 0039 sec. 3), with every realm as Ground until the other
    // forms exist, plus the single realm of today.
    let layout = [
        (
            "test:basement",
            ground(RealmSpan::below(-8192), RealmLod::Off),
        ),
        (
            "test:deep",
            ground(RealmSpan::between(-8192, -4096), RealmLod::Off),
        ),
        (
            "test:surface",
            ground(RealmSpan::between(-4096, 12288), RealmLod::Draw),
        ),
        (
            "test:ceiling",
            ground(RealmSpan::above(12288), RealmLod::Off),
        ),
    ];
    assert_eq!(check(&layout).0, []);
    assert_eq!(
        check(&[("test:all", ground(RealmSpan::ALL, RealmLod::Draw))]).0,
        []
    );
    // Eight planes is the limit, not over it.
    let eight: Vec<(String, RealmLayout)> = (0..9)
        .map(|i| {
            let lo = (i > 0).then(|| (i - 1) * REALM_PLANE);
            let hi = (i < 8).then(|| i * REALM_PLANE);
            let lod = if i == 0 {
                RealmLod::Draw
            } else {
                RealmLod::Off
            };
            (format!("test:r{i}"), ground(RealmSpan::new(lo, hi), lod))
        })
        .collect();
    let eight: Vec<(&str, RealmLayout)> = eight.iter().map(|(n, l)| (n.as_str(), *l)).collect();
    assert_eq!(check(&eight).0, []);
}

#[test]
fn an_empty_span_is_refused() {
    let (problems, message) = check(&[
        ("test:surface", ground(RealmSpan::ALL, RealmLod::Draw)),
        (
            "test:flat",
            ground(RealmSpan::between(4096, 4096), RealmLod::Off),
        ),
    ]);
    assert_eq!(
        problems,
        [Problem::EmptySpan {
            dimension: id("test:dim"),
            realm: id("test:flat"),
            span: RealmSpan::between(4096, 4096),
        }]
    );
    assert_eq!(
        message,
        "world generation check failed: realm test:flat of dimension test:dim has an empty span \
         [4096, 4096)"
    );
}

#[test]
fn a_plane_off_the_grid_is_refused() {
    let (problems, message) = check(&[
        ("test:low", ground(RealmSpan::below(-100), RealmLod::Off)),
        (
            "test:surface",
            ground(RealmSpan::between(-100, 12289), RealmLod::Draw),
        ),
        ("test:high", ground(RealmSpan::above(12289), RealmLod::Off)),
    ]);
    let off = |realm: &str, y| Problem::OffPlane {
        dimension: id("test:dim"),
        realm: id(realm),
        y,
    };
    assert_eq!(
        problems,
        [
            off("test:low", -100),
            off("test:surface", -100),
            off("test:surface", 12289),
            off("test:high", 12289),
        ]
    );
    names_all(
        &message,
        &[
            "test:low",
            "test:surface",
            "test:high",
            "test:dim",
            "y -100",
            "4096",
        ],
    );
}

#[test]
fn overlapping_realms_are_refused() {
    let (problems, message) = check(&[
        (
            "test:surface",
            ground(RealmSpan::above(-4096), RealmLod::Draw),
        ),
        ("test:nether", ground(RealmSpan::below(0), RealmLod::Off)),
        (
            "test:mod_realm",
            ground(RealmSpan::between(4096, 8192), RealmLod::Off),
        ),
        ("test:twice", ground(RealmSpan::below(-4096), RealmLod::Off)),
    ]);
    let overlap = |a: &str, b: &str| Problem::Overlap {
        dimension: id("test:dim"),
        a: id(a),
        b: id(b),
    };
    assert_eq!(
        problems,
        [
            overlap("test:surface", "test:nether"),
            overlap("test:surface", "test:mod_realm"),
            overlap("test:nether", "test:twice"),
        ]
    );
    names_all(
        &message,
        &["realms test:surface and test:nether of dimension test:dim overlap"],
    );
    // A realm listed twice overlaps itself.
    let layout = ground(RealmSpan::ALL, RealmLod::Draw);
    let twice = ground(RealmSpan::ALL, RealmLod::Off);
    let (problems, message) = check(&[("test:a", layout), ("test:a", twice)]);
    assert_eq!(problems, [overlap("test:a", "test:a")]);
    names_all(&message, &["dimension test:dim lists realm test:a twice"]);
}

#[test]
fn more_than_eight_planes_are_refused() {
    let realms: Vec<(String, RealmLayout)> = (0..10)
        .map(|i| {
            let lo = (i > 0).then(|| (i - 1) * REALM_PLANE);
            let hi = (i < 9).then(|| i * REALM_PLANE);
            let lod = if i == 0 {
                RealmLod::Draw
            } else {
                RealmLod::Off
            };
            (format!("test:r{i}"), ground(RealmSpan::new(lo, hi), lod))
        })
        .collect();
    let realms: Vec<(&str, RealmLayout)> = realms.iter().map(|(n, l)| (n.as_str(), *l)).collect();
    let (problems, message) = check(&realms);
    assert_eq!(
        problems,
        [Problem::TooManyPlanes {
            dimension: id("test:dim"),
            planes: (0..9).map(|i| i * REALM_PLANE).collect(),
        }]
    );
    names_all(
        &message,
        &["dimension test:dim has 9 realm planes", "at most 8"],
    );
}

#[test]
fn a_dimension_needs_exactly_one_drawn_realm() {
    let (problems, message) = check(&[
        ("test:low", ground(RealmSpan::below(0), RealmLod::Off)),
        ("test:high", ground(RealmSpan::above(0), RealmLod::Off)),
    ]);
    assert_eq!(
        problems,
        [Problem::DrawRealms {
            dimension: id("test:dim"),
            draw: Vec::new(),
        }]
    );
    names_all(
        &message,
        &["dimension test:dim has no realm the far LOD draws"],
    );

    let (problems, message) = check(&[
        ("test:low", ground(RealmSpan::below(0), RealmLod::Draw)),
        ("test:high", ground(RealmSpan::above(0), RealmLod::Draw)),
    ]);
    assert_eq!(
        problems,
        [Problem::DrawRealms {
            dimension: id("test:dim"),
            draw: vec![id("test:low"), id("test:high")],
        }]
    );
    names_all(
        &message,
        &["dimension test:dim has 2 realms the far LOD draws (test:low, test:high)"],
    );

    // No realm at all: nothing covers any y, and nothing is drawn.
    let (problems, _) = check(&[]);
    assert_eq!(
        problems,
        [
            Problem::Gap {
                dimension: id("test:dim"),
                span: RealmSpan::ALL,
                below: None,
                above: None,
            },
            Problem::DrawRealms {
                dimension: id("test:dim"),
                draw: Vec::new(),
            },
        ]
    );
}

#[test]
fn a_y_outside_every_realm_is_refused() {
    let gap = |span, below: Option<&str>, above: Option<&str>| Problem::Gap {
        dimension: id("test:dim"),
        span,
        below: below.map(id),
        above: above.map(id),
    };
    // A stretch between two realms, in any listing order.
    let low = ("test:low", ground(RealmSpan::below(0), RealmLod::Off));
    let high = ("test:high", ground(RealmSpan::above(4096), RealmLod::Draw));
    for realms in [[low, high], [high, low]] {
        let (problems, message) = check(&realms);
        assert_eq!(
            problems,
            [gap(
                RealmSpan::between(0, 4096),
                Some("test:low"),
                Some("test:high")
            )]
        );
        assert_eq!(
            message,
            "world generation check failed: dimension test:dim has no realm for y in [0, 4096), \
             between realms test:low and test:high"
        );
    }
    // Open ends left uncovered, under and over a bounded realm.
    let (problems, message) = check(&[(
        "test:mid",
        ground(RealmSpan::between(0, 4096), RealmLod::Draw),
    )]);
    assert_eq!(
        problems,
        [
            gap(RealmSpan::below(0), None, Some("test:mid")),
            gap(RealmSpan::above(4096), Some("test:mid"), None),
        ]
    );
    names_all(
        &message,
        &[
            "no realm for y in (-inf, 0), under realm test:mid",
            "no realm for y in [4096, +inf), over realm test:mid",
        ],
    );
    // Overlaps do not hide a gap further up, and an empty span covers nothing.
    let (problems, _) = check(&[
        ("test:a", ground(RealmSpan::below(8192), RealmLod::Draw)),
        ("test:b", ground(RealmSpan::below(4096), RealmLod::Off)),
        (
            "test:flat",
            ground(RealmSpan::between(12288, 12288), RealmLod::Off),
        ),
        ("test:c", ground(RealmSpan::above(16384), RealmLod::Off)),
    ]);
    assert!(
        problems.contains(&gap(
            RealmSpan::between(8192, 16384),
            Some("test:a"),
            Some("test:c")
        )),
        "{problems:?}"
    );
    assert_eq!(
        problems
            .iter()
            .filter(|p| matches!(p, Problem::Gap { .. }))
            .count(),
        1
    );
}

#[test]
fn a_plane_outside_the_world_is_refused() {
    let far = MAX_BLOCK_COORD + REALM_PLANE;
    let (problems, message) = check(&[
        ("test:low", ground(RealmSpan::below(-far), RealmLod::Off)),
        (
            "test:surface",
            ground(RealmSpan::between(-far, far), RealmLod::Draw),
        ),
        ("test:high", ground(RealmSpan::above(far), RealmLod::Off)),
    ]);
    let outside = |realm: &str, y| Problem::PlaneOutsideWorld {
        dimension: id("test:dim"),
        realm: id(realm),
        y,
    };
    assert_eq!(
        problems,
        [
            outside("test:low", -far),
            outside("test:surface", -far),
            outside("test:surface", far),
            outside("test:high", far),
        ]
    );
    names_all(
        &message,
        &[&format!(
            "realm test:low of dimension test:dim has a plane at y {}, outside the world",
            -far
        )],
    );
    // The world limit itself is a plane like any other; open ends never count.
    let (problems, _) = check(&[
        (
            "test:low",
            ground(RealmSpan::below(-MAX_BLOCK_COORD), RealmLod::Off),
        ),
        (
            "test:surface",
            ground(
                RealmSpan::between(-MAX_BLOCK_COORD, MAX_BLOCK_COORD),
                RealmLod::Draw,
            ),
        ),
        (
            "test:high",
            ground(RealmSpan::above(MAX_BLOCK_COORD), RealmLod::Off),
        ),
    ]);
    assert_eq!(problems, []);
}

#[test]
fn a_form_not_built_yet_is_refused() {
    assert_eq!(RealmForm::BUILT, [RealmForm::Ground]);
    for form in [RealmForm::Cavern, RealmForm::Ceiling, RealmForm::Void] {
        let layout = RealmLayout {
            span: RealmSpan::below(-4096),
            form,
            lod: RealmLod::Off,
        };
        let (problems, message) = check(&[
            ("test:nether", layout),
            (
                "test:surface",
                ground(RealmSpan::above(-4096), RealmLod::Draw),
            ),
        ]);
        assert_eq!(
            problems,
            [Problem::FormNotBuilt {
                dimension: id("test:dim"),
                realm: id("test:nether"),
                form,
            }]
        );
        names_all(
            &message,
            &[&format!(
                "realm test:nether of dimension test:dim has the form {form}, which is not built \
                 yet"
            )],
        );
    }
}

#[test]
fn every_layout_problem_is_reported_at_once() {
    let cavern = RealmLayout {
        span: RealmSpan::below(10),
        form: RealmForm::Cavern,
        lod: RealmLod::Draw,
    };
    let (problems, message) = check(&[
        ("test:nether", cavern),
        ("test:surface", ground(RealmSpan::above(0), RealmLod::Draw)),
        (
            "test:flat",
            ground(RealmSpan::between(8192, 0), RealmLod::Off),
        ),
    ]);
    assert_eq!(problems.len(), 5, "{message}");
    assert!(
        message.starts_with("world generation checks failed with 5 problems:"),
        "{message}"
    );
    names_all(
        &message,
        &[
            "test:nether",
            "test:surface",
            "test:flat",
            "y 10",
            "Cavern",
            "overlap",
            "empty span",
        ],
    );
}

#[test]
fn layout_hashes_follow_every_field() {
    let base = ground(RealmSpan::between(-4096, 12288), RealmLod::Draw);
    let others = [
        ground(RealmSpan::between(-8192, 12288), RealmLod::Draw),
        ground(RealmSpan::above(-4096), RealmLod::Draw),
        ground(RealmSpan::between(-4096, 12288), RealmLod::Off),
        RealmLayout {
            form: RealmForm::Cavern,
            ..base
        },
    ];
    for other in others {
        assert_ne!(
            crate::registry::hash_params(&other),
            crate::registry::hash_params(&base)
        );
    }
}

#[test]
fn spans_are_exact_at_the_ends_of_i64() {
    // No `y` is under `i64::MIN`: a span that ends there is empty and overlaps nothing.
    assert!(RealmSpan::below(i64::MIN).is_empty());
    assert!(!RealmSpan::below(i64::MIN).overlaps(RealmSpan::ALL));
    assert!(!RealmSpan::below(i64::MIN).contains(i64::MIN));
    // A span from `i64::MIN` holds every `y` an open bottom does, and the top is the other way.
    assert!(RealmSpan::above(i64::MIN).contains(i64::MIN));
    assert!(RealmSpan::above(i64::MIN).overlaps(RealmSpan::below(i64::MIN + 1)));
    assert!(!RealmSpan::above(i64::MAX).is_empty());
    assert!(!RealmSpan::between(i64::MIN, i64::MAX).contains(i64::MAX));
    // So a layout over the whole of `i64` has no gap under `i64::MIN`, only planes outside the
    // world, and an empty span at the bottom is refused as empty.
    let outside = |realm: &str, y| Problem::PlaneOutsideWorld {
        dimension: id("test:dim"),
        realm: id(realm),
        y,
    };
    let (problems, message) = check(&[
        (
            "test:all",
            ground(RealmSpan::above(i64::MIN), RealmLod::Draw),
        ),
        (
            "test:none",
            ground(RealmSpan::below(i64::MIN), RealmLod::Off),
        ),
    ]);
    assert_eq!(
        problems,
        [
            outside("test:all", i64::MIN),
            Problem::EmptySpan {
                dimension: id("test:dim"),
                realm: id("test:none"),
                span: RealmSpan::below(i64::MIN),
            },
            outside("test:none", i64::MIN),
        ],
        "{message}"
    );
    // `i64::MAX` is a `y` like any other: a span that stops under it leaves it uncovered.
    let (problems, _) = check(&[(
        "test:all",
        ground(RealmSpan::below(i64::MAX), RealmLod::Draw),
    )]);
    assert!(problems.contains(&Problem::Gap {
        dimension: id("test:dim"),
        span: RealmSpan::above(i64::MAX),
        below: Some(id("test:all")),
        above: None,
    }));
}

/// A plane for [`layouts_match_a_brute_force`]: open, on the grid near 0, at the world limit or
/// at the ends of `i64`.
fn any_plane() -> impl Strategy<Value = Option<i64>> {
    prop_oneof![
        Just(None),
        (-3i64..=3).prop_map(|k| Some(k * REALM_PLANE)),
        prop::sample::select(vec![
            i64::MIN,
            i64::MIN + 1,
            -MAX_BLOCK_COORD,
            MAX_BLOCK_COORD,
            i64::MAX - 1,
            i64::MAX,
        ])
        .prop_map(Some),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// Empty spans, overlaps and gaps of up to 6 realms in any order (nested, touching, inverted,
    /// at the ends of `i64`), against `contains` at every plane and the `y` under it, which is
    /// enough: what covers a `y` changes only at planes.
    #[test]
    fn layouts_match_a_brute_force(
        spans in prop::collection::vec((any_plane(), any_plane()), 0..6),
    ) {
        let span = |i: usize| RealmSpan::new(spans[i].0, spans[i].1);
        let names: Vec<GenId> = (0..spans.len()).map(|i| id(&format!("test:r{i}"))).collect();
        let layouts: Vec<RealmLayout> = (0..spans.len())
            .map(|i| ground(span(i), if i == 0 { RealmLod::Draw } else { RealmLod::Off }))
            .collect();
        let mut ys = BTreeSet::from([i64::MIN, i64::MAX]);
        for p in spans.iter().flat_map(|&(lo, hi)| lo.into_iter().chain(hi)) {
            ys.extend([p, p.saturating_sub(1)]);
        }
        let mut checks = Checks::new();
        check_realms(&mut checks, &id("test:dim"), names.iter().zip(&layouts));
        let problems = checks.problems();

        let empty: Vec<&GenId> = (0..spans.len())
            .filter(|&i| !ys.iter().any(|&y| span(i).contains(y)))
            .map(|i| &names[i])
            .collect();
        let found: Vec<&GenId> = problems
            .iter()
            .filter_map(|p| match p {
                Problem::EmptySpan { realm, .. } => Some(realm),
                _ => None,
            })
            .collect();
        prop_assert_eq!(found, empty, "empty spans");

        let mut overlaps = Vec::new();
        for i in 0..spans.len() {
            for j in i + 1..spans.len() {
                if ys.iter().any(|&y| span(i).contains(y) && span(j).contains(y)) {
                    overlaps.push((&names[i], &names[j]));
                }
            }
        }
        let found: Vec<(&GenId, &GenId)> = problems
            .iter()
            .filter_map(|p| match p {
                Problem::Overlap { a, b, .. } => Some((a, b)),
                _ => None,
            })
            .collect();
        prop_assert_eq!(found, overlaps, "overlaps");

        // Every uncovered `y` is in exactly one gap, every gap holds some `y` and none that a
        // realm covers, and the gaps go bottom first.
        let gaps: Vec<RealmSpan> = problems
            .iter()
            .filter_map(|p| match p {
                Problem::Gap { span, .. } => Some(*span),
                _ => None,
            })
            .collect();
        for &y in &ys {
            let covered = (0..spans.len()).any(|i| span(i).contains(y));
            let holding = gaps.iter().filter(|g| g.contains(y)).count();
            prop_assert_eq!(holding, usize::from(!covered), "y {}", y);
        }
        for gap in &gaps {
            prop_assert!(ys.iter().any(|&y| gap.contains(y)), "gap {} holds no y", gap);
        }
        prop_assert!(gaps.is_sorted_by_key(|g| g.lo.map_or(i128::MIN, i128::from)));
    }
}

// ---- Realm specs and stacks -------------------------------------------------------------------

fn spec(span: RealmSpan, form: RealmForm, lod: RealmLod, floor: &str) -> RealmSpec {
    RealmSpec {
        span,
        form,
        datum: 63,
        fluid: FluidRule::SeaLevel {
            level: 63,
            fluid: "test:fluid".into(),
        },
        floor: floor.into(),
        sky: RealmSky::Open,
        lod,
        env: id("test:env"),
        version: 1,
    }
}

#[test]
fn a_stack_finds_the_realm_of_every_y() {
    let realms = vec![
        (
            id("test:sky"),
            spec(
                RealmSpan::above(12288),
                RealmForm::Ground,
                RealmLod::Off,
                "test:solid",
            ),
        ),
        (
            id("test:surface"),
            spec(
                RealmSpan::between(-4096, 12288),
                RealmForm::Ground,
                RealmLod::Draw,
                "test:solid",
            ),
        ),
        (
            id("test:deep"),
            spec(
                RealmSpan::below(-4096),
                RealmForm::Ground,
                RealmLod::Off,
                "test:solid",
            ),
        ),
    ];
    let mut checks = Checks::new();
    let stack = RealmStack::build(&mut checks, &id("test:dim"), realms).expect("a good stack");
    assert!(checks.is_empty());
    // Bottom first, whatever order they came in.
    let ids: Vec<&str> = stack.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["test:deep", "test:surface", "test:sky"]);
    assert_eq!(stack.planes(), [-4096, 12288]);
    assert_eq!((stack.len(), stack.drawn()), (3, 1));
    for (y, realm) in [
        (i64::MIN, 0),
        (-4097, 0),
        (-4096, 1),
        (-4095, 1),
        (0, 1),
        (12287, 1),
        (12288, 2),
        (12289, 2),
        (i64::MAX, 2),
    ] {
        assert_eq!(stack.realm_of(y), realm, "y {y}");
        assert!(stack.spec(realm).span.contains(y), "y {y}");
    }
    // A chunk is in one realm: its lowest and highest voxels agree, at every plane.
    for plane in [-4096i64, 12288] {
        for layer in (plane >> 5) - 2..(plane >> 5) + 2 {
            let (lo, hi) = (layer << 5, (layer << 5) + 31);
            assert_eq!(stack.realm_of(lo), stack.realm_of(hi), "layer {layer}");
        }
    }
    assert_eq!(stack.spec(1).span.chunk_layers(), -128..=383);
    assert_eq!(stack.spec(0).span.chunk_layers(), i64::MIN..=-129);
    assert!(RealmSpan::between(0, 0).chunk_layers().is_empty());

    // A bad stack is refused with its problems, also when they were already reported (two
    // realms open above, one gap under them, no drawn realm).
    let mut checks = Checks::new();
    let bad = vec![
        (
            id("test:only"),
            spec(
                RealmSpan::above(0),
                RealmForm::Ground,
                RealmLod::Off,
                "test:solid",
            ),
        ),
        (
            id("test:open"),
            spec(
                RealmSpan::above(4096),
                RealmForm::Ground,
                RealmLod::Off,
                "test:solid",
            ),
        ),
    ];
    assert!(RealmStack::build(&mut checks, &id("test:dim"), bad.clone()).is_none());
    assert!(RealmStack::build(&mut checks, &id("test:dim"), bad).is_none());
    let message = checks.finish().unwrap_err().to_string();
    assert_eq!(
        message.matches("no realm the far LOD draws").count(),
        1,
        "{message}"
    );
}

#[test]
fn a_floor_must_fit_its_form() {
    let realms = [
        (
            id("test:ground"),
            spec(RealmSpan::ALL, RealmForm::Ground, RealmLod::Draw, AIR_NAME),
        ),
        (
            id("test:roof"),
            spec(
                RealmSpan::ALL,
                RealmForm::Ceiling,
                RealmLod::Off,
                "test:solid",
            ),
        ),
        (
            id("test:void"),
            spec(RealmSpan::ALL, RealmForm::Void, RealmLod::Off, AIR_NAME),
        ),
        (
            id("test:cave"),
            spec(
                RealmSpan::ALL,
                RealmForm::Cavern,
                RealmLod::Off,
                "test:solid",
            ),
        ),
    ];
    let mut checks = Checks::new();
    check_floors(
        &mut checks,
        &id("test:dim"),
        realms.iter().map(|(id, s)| (id, s)),
    );
    let message = checks.finish().unwrap_err().to_string();
    names_all(
        &message,
        &[
            "realm test:ground of dimension test:dim has the form Ground and the floor boxel:air; \
             its form needs a floor that is not air",
            "realm test:roof of dimension test:dim has the form Ceiling and the floor test:solid; \
             its form needs air",
        ],
    );
    assert!(
        !message.contains("test:void") && !message.contains("test:cave"),
        "{message}"
    );
}

#[test]
fn the_fluid_rule_wets_columns_under_its_level() {
    let sea = FluidRule::SeaLevel {
        level: 63,
        fluid: "test:fluid".into(),
    };
    assert_eq!(sea.top(62), Some(63));
    assert_eq!(sea.top(63), None);
    assert_eq!(sea.top(i64::MIN), Some(63));
    assert_eq!(FluidRule::None.top(0), None);
}

#[test]
fn realm_spec_hashes_follow_every_field() {
    let base = spec(
        RealmSpan::ALL,
        RealmForm::Ground,
        RealmLod::Draw,
        "test:solid",
    );
    let others = [
        RealmSpec {
            datum: 0,
            ..base.clone()
        },
        RealmSpec {
            fluid: FluidRule::None,
            ..base.clone()
        },
        RealmSpec {
            fluid: FluidRule::SeaLevel {
                level: 62,
                fluid: "test:fluid".into(),
            },
            ..base.clone()
        },
        RealmSpec {
            floor: "test:glass".into(),
            ..base.clone()
        },
        RealmSpec {
            sky: RealmSky::Closed,
            ..base.clone()
        },
        RealmSpec {
            sky: RealmSky::Under,
            ..base.clone()
        },
        RealmSpec {
            env: id("test:dark"),
            ..base.clone()
        },
        RealmSpec {
            span: RealmSpan::above(0),
            ..base.clone()
        },
    ];
    let hash = crate::registry::hash_params(&base);
    for other in &others {
        assert_ne!(crate::registry::hash_params(other), hash, "{other:?}");
    }
    // The version is the entry's, not a parameter.
    let newer = RealmSpec {
        version: 2,
        ..base.clone()
    };
    assert_eq!(crate::registry::hash_params(&newer), hash);
    assert_eq!(newer.version(), 2);
    assert_eq!(base.layout(), ground(RealmSpan::ALL, RealmLod::Draw));
}

#[test]
fn the_default_stack_is_one_open_drawn_realm() {
    let stack = RealmStack::single();
    assert_eq!(stack.len(), 1);
    assert_eq!(stack.id(0).as_str(), "boxel:default");
    let spec = stack.spec(0);
    assert_eq!(
        (spec.span, spec.form, spec.sky, spec.lod),
        (
            RealmSpan::ALL,
            RealmForm::Ground,
            RealmSky::Open,
            RealmLod::Draw
        )
    );
    assert_eq!(stack.realm_of(i64::MIN), 0);
    assert_eq!(stack.realm_of(i64::MAX), 0);
}

// ---- A world open over test registries ------------------------------------------------------

/// A test preset: the dimensions it maps.
struct Preset(Vec<GenId>);
/// A test dimension: its realms, bottom first.
struct Dimension(Vec<GenId>);
/// A test realm: its layout and the kinds its selector can pick.
struct Realm(RealmLayout, Vec<GenId>);
/// A test terrain kind with one parameter.
struct Kind(u32);

impl HashParams for Preset {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0);
    }
}
impl HashParams for Dimension {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0);
    }
}
impl HashParams for Realm {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0).write(&self.1);
    }
}
impl HashParams for Kind {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0);
    }
}
impl GenEntry for Preset {
    fn version(&self) -> u32 {
        1
    }
}
impl GenEntry for Dimension {
    fn version(&self) -> u32 {
        1
    }
}
impl GenEntry for Realm {
    fn version(&self) -> u32 {
        1
    }
}
impl GenEntry for Kind {
    fn version(&self) -> u32 {
        3
    }
}

/// The test app's registries, frozen.
struct Registries {
    presets: GenRegistry<Preset>,
    dimensions: GenRegistry<Dimension>,
    realms: GenRegistry<Realm>,
    kinds: GenRegistry<Kind>,
}

impl Registries {
    /// The resolution of `preset` at world open: every id it reaches, checked, and the digest.
    fn open(&self, preset: &GenId) -> Result<RegistryDigest, FreezeError> {
        let mut checks = Checks::new();
        let mut digest = DigestBuilder::new();
        if let Some(p) = checks.resolve(&self.presets, preset, None) {
            digest.add(&DigestScope::PRESET, &self.presets, p);
            for dim_id in &self.presets[p].0 {
                let Some(d) = checks.resolve(&self.dimensions, dim_id, Some(preset)) else {
                    continue;
                };
                digest.add(&DigestScope::of_dimension(dim_id), &self.dimensions, d);
                let mut layouts = Vec::new();
                for realm_id in &self.dimensions[d].0 {
                    let Some(r) = checks.resolve(&self.realms, realm_id, Some(dim_id)) else {
                        continue;
                    };
                    let scope = DigestScope::of_realm(dim_id, realm_id);
                    digest.add(&scope, &self.realms, r);
                    let realm = &self.realms[r];
                    layouts.push((realm_id, &realm.0));
                    for kind_id in &realm.1 {
                        if let Some(k) = checks.resolve(&self.kinds, kind_id, Some(realm_id)) {
                            digest.add(&scope, &self.kinds, k);
                        }
                    }
                }
                check_realms(&mut checks, dim_id, layouts);
            }
        }
        checks.finish().map(|()| digest.finish())
    }
}

/// The registries plugins would build: a good preset `test:default` (one dimension, a surface
/// over a deep realm, kinds shared between them) and a bad one, `test:broken`.
fn registries(extra_realm: bool) -> Registries {
    let mut checks = Checks::new();
    let mut presets = GenRegistryBuilder::new("test_preset");
    presets
        .register(id("test:default"), Preset(vec![id("test:overworld")]))
        .register(
            id("test:broken"),
            Preset(vec![
                id("test:overworld"),
                id("test:broken"),
                id("test:missing"),
            ]),
        );
    let mut dimensions = GenRegistryBuilder::new("test_dimension");
    let mut overworld = vec![id("test:deep"), id("test:surface")];
    if extra_realm {
        overworld.insert(0, id("test:abyss"));
    }
    dimensions
        .register(id("test:overworld"), Dimension(overworld))
        .register(
            id("test:broken"),
            Dimension(vec![
                id("test:surface"),
                id("test:sky"),
                id("test:gone"),
                id("test:cave"),
            ]),
        );
    let mut realms = GenRegistryBuilder::new("test_realm");
    let deep_lo = if extra_realm { Some(-8192) } else { None };
    realms
        .register(
            id("test:abyss"),
            Realm(
                ground(RealmSpan::below(-8192), RealmLod::Off),
                vec![id("test:plains")],
            ),
        )
        .register(
            id("test:deep"),
            Realm(
                ground(RealmSpan::new(deep_lo, Some(-4096)), RealmLod::Off),
                vec![id("test:plains")],
            ),
        )
        .register(
            id("test:surface"),
            Realm(
                ground(RealmSpan::above(-4096), RealmLod::Draw),
                vec![id("test:plains"), id("test:hills")],
            ),
        )
        .register(
            id("test:sky"),
            Realm(
                ground(RealmSpan::above(12000), RealmLod::Draw),
                vec![id("test:clouds")],
            ),
        )
        .register(
            id("test:cave"),
            Realm(
                RealmLayout {
                    span: RealmSpan::below(0),
                    form: RealmForm::Cavern,
                    lod: RealmLod::Off,
                },
                Vec::new(),
            ),
        );
    let mut kinds = GenRegistryBuilder::new("test_kind");
    kinds
        .register(id("test:plains"), Kind(1))
        .register(id("test:hills"), Kind(2));
    let registries = Registries {
        presets: checks.freeze(presets).expect("presets"),
        dimensions: checks.freeze(dimensions).expect("dimensions"),
        realms: checks.freeze(realms).expect("realms"),
        kinds: checks.freeze(kinds).expect("kinds"),
    };
    checks.finish().expect("the app builds");
    registries
}

#[test]
fn a_bad_mix_fails_at_world_open_with_every_id() {
    let error = registries(false)
        .open(&id("test:broken"))
        .expect_err("broken");
    let message = error.to_string();
    for needle in [
        "test:broken names test_dimension test:missing, which is not registered",
        "test:broken names test_realm test:gone, which is not registered",
        "test:sky names test_kind test:clouds, which is not registered",
        "test:sky of dimension test:broken has a plane at y 12000",
        "realms test:surface and test:sky of dimension test:broken overlap",
        "realms test:surface and test:cave of dimension test:broken overlap",
        "dimension test:broken has 2 realms the far LOD draws (test:surface, test:sky)",
        "realm test:cave of dimension test:broken has the form Cavern, which is not built yet",
    ] {
        assert!(message.contains(needle), "{needle:?} not in {message}");
    }
    assert_eq!(error.problems().len(), 8, "{message}");
    let unknown = registries(false)
        .open(&id("test:nothing"))
        .expect_err("unknown preset");
    assert_eq!(
        unknown.to_string(),
        "world generation check failed: test_preset test:nothing is not registered"
    );
}

#[test]
fn a_good_mix_opens_with_its_digest() {
    let digest = registries(false).open(&id("test:default")).expect("opens");
    let lines: Vec<String> = digest
        .entries()
        .iter()
        .map(|e| format!("{} {} {}", e.scope, e.registry, e.id))
        .collect();
    assert_eq!(
        lines,
        [
            "the preset test_preset test:default",
            "dimension test:overworld test_dimension test:overworld",
            "realm test:deep of dimension test:overworld test_kind test:plains",
            "realm test:deep of dimension test:overworld test_realm test:deep",
            "realm test:surface of dimension test:overworld test_kind test:hills",
            "realm test:surface of dimension test:overworld test_kind test:plains",
            "realm test:surface of dimension test:overworld test_realm test:surface",
        ]
    );
    assert!(
        digest
            .entries()
            .iter()
            .filter(|e| e.registry == "test_kind")
            .all(|e| e.version == 3)
    );
    // The same registries give the same digest on every open.
    assert_eq!(
        registries(false).open(&id("test:default")).expect("opens"),
        digest
    );

    // A mod's realm cuts the deep realm's open span at a free plane: one realm added (with its
    // kind), one realm changed (its span) and one dimension changed (its list).
    let with_abyss = registries(true).open(&id("test:default")).expect("opens");
    let diff = WorldgenDiff::between(&digest, &with_abyss);
    let added: Vec<(&str, &str)> = diff
        .added
        .iter()
        .map(|e| (&*e.registry, e.id.as_str()))
        .collect();
    assert_eq!(
        added,
        [("test_kind", "test:plains"), ("test_realm", "test:abyss")]
    );
    assert!(diff.removed.is_empty());
    let changed: Vec<&str> = diff.changed.iter().map(|c| c.live.id.as_str()).collect();
    assert_eq!(changed, ["test:overworld", "test:deep"]);
    assert_ne!(
        with_abyss.dimension(&id("test:overworld")).version(),
        digest.dimension(&id("test:overworld")).version()
    );
}
