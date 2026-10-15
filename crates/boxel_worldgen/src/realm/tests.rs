//! Realm spans, and every layout check failing with its ids in the message (ADR 0039 "Cómo se
//! prueba" 4 and 11).
use super::*;

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

    let (problems, _) = check(&[]);
    assert_eq!(
        problems,
        [Problem::DrawRealms {
            dimension: id("test:dim"),
            draw: Vec::new(),
        }]
    );
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
