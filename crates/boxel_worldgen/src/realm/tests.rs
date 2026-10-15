//! Realm spans, and every layout check failing with its ids in the message (ADR 0039 "Cómo se
//! prueba" 4 and 11). Then a whole world open over test registries (a preset, its dimensions,
//! their realms and kinds): every problem of a bad mix reported at once, and the digest of a
//! good one per dimension and realm, with a realm added as one more line. The real presets and
//! dimension tables come with WG-SAVE; these are `test:` stand-ins.
use super::*;
use crate::registry::{
    DigestBuilder, DigestScope, FreezeError, GenEntry, GenRegistry, GenRegistryBuilder,
    RegistryDigest, WorldgenDiff,
};

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
        let world = id("test:world");
        if let Some(p) = checks.resolve(&self.presets, preset, &world) {
            digest.add(&DigestScope::PRESET, &self.presets, p);
            for dim_id in &self.presets[p].0 {
                let Some(d) = checks.resolve(&self.dimensions, dim_id, preset) else {
                    continue;
                };
                digest.add(&DigestScope::of_dimension(dim_id), &self.dimensions, d);
                let mut layouts = Vec::new();
                for realm_id in &self.dimensions[d].0 {
                    let Some(r) = checks.resolve(&self.realms, realm_id, dim_id) else {
                        continue;
                    };
                    let scope = DigestScope::of_realm(dim_id, realm_id);
                    digest.add(&scope, &self.realms, r);
                    let realm = &self.realms[r];
                    layouts.push((realm_id, &realm.0));
                    for kind_id in &realm.1 {
                        if let Some(k) = checks.resolve(&self.kinds, kind_id, realm_id) {
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
        "world generation check failed: test:world names test_preset test:nothing, which is not \
         registered"
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
