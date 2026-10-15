//! The registries against ADR 0039 "Cómo se prueba" 3 to 5 (worldgen plan sec. 10, tests 4 and
//! 5): registering in any order gives the same indices, salts and digest; duplicates and unknown
//! ids fail with their ids in the message; an added entry changes no other entry's salt, seed or
//! line, and removing it gives the old digest back; the diff of two digests; no two salts or
//! octave seeds of the test entries collide; the fixed values of the salt and parameter hash. The
//! checks of realm layouts and a whole world open are in `realm/tests.rs`.
use super::*;
use crate::noise::{mix_seed, splitmix64};
use boxel_voxel::RegistryError;
use proptest::prelude::*;
use std::collections::HashSet;

fn id(name: &str) -> GenId {
    GenId::new(name).expect(name)
}

/// A test entry: a version, a weight (its one parameter) and an order.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Entry {
    version: u32,
    weight: u32,
    order: Order,
}

impl Entry {
    const fn new(weight: u32) -> Self {
        Self {
            version: 1,
            weight,
            order: Order::NONE,
        }
    }

    const fn ordered(weight: u32, phase: u16, priority: i32) -> Self {
        Self {
            order: Order::new(phase, priority),
            ..Self::new(weight)
        }
    }
}

impl HashParams for Entry {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.weight);
    }
}

impl GenEntry for Entry {
    fn version(&self) -> u32 {
        self.version
    }
    fn order(&self) -> Order {
        self.order
    }
}

/// The name of test entry `n`: half of them with a path, so ids of both shapes mix.
fn name(n: u32) -> String {
    if n.is_multiple_of(2) {
        format!("test:e{n}")
    } else {
        format!("test:e/{n}")
    }
}

/// Test entry `n` with weight `n % 64`.
fn numbered(n: u32) -> (GenId, Entry) {
    (id(&name(n)), Entry::new(n % 64))
}

/// The frozen registry of `entries`, registered in their order.
fn registry(entries: &[(GenId, Entry)]) -> GenRegistry<Entry> {
    let mut builder = GenRegistryBuilder::new("test_entry");
    for (id, entry) in entries {
        builder.register(id.clone(), *entry);
    }
    builder.freeze().expect("no duplicates")
}

/// Every entry of `r` in index order: index, id, salt, version and parameter hash.
fn table(r: &GenRegistry<Entry>) -> Vec<(usize, GenId, u64, u32, u64)> {
    r.indices()
        .map(|ix| {
            (
                ix.index(),
                r.id(ix).clone(),
                r.salt(ix),
                r.version(ix),
                r.params_hash(ix),
            )
        })
        .collect()
}

/// The scope every entry of [`digest`] sits in.
fn realm_scope() -> DigestScope {
    DigestScope::of_realm(&id("test:dimension"), &id("test:realm"))
}

/// The digest of every entry of `r`, all in [`realm_scope`].
fn digest(r: &GenRegistry<Entry>) -> RegistryDigest {
    let scope = realm_scope();
    let mut digest = DigestBuilder::new();
    for ix in r.indices() {
        digest.add(&scope, r, ix);
    }
    digest.finish()
}

/// A digest line.
fn line(
    scope: &DigestScope,
    registry: &'static str,
    name: &str,
    version: u32,
    params: u64,
) -> DigestEntry {
    DigestEntry {
        scope: scope.clone(),
        registry: registry.into(),
        id: id(name),
        version,
        params,
    }
}

/// Up to 40 test entries with random weights, versions and orders, plus a shuffled copy.
fn entries_and_shuffled() -> impl Strategy<Value = (Vec<(GenId, Entry)>, Vec<(GenId, Entry)>)> {
    let entry =
        (0u32..64, 0u32..3, 0u16..3, -2i32..3).prop_map(|(weight, version, phase, priority)| {
            Entry {
                version,
                ..Entry::ordered(weight, phase, priority)
            }
        });
    prop::collection::btree_map(0u32..1000, entry, 1..40)
        .prop_map(|m| {
            m.into_iter()
                .map(|(n, e)| (id(&name(n)), e))
                .collect::<Vec<_>>()
        })
        .prop_flat_map(|entries| (Just(entries.clone()), Just(entries).prop_shuffle()))
}

#[test]
fn ids_take_the_block_name_rule() {
    for name in ["test:a", "test_2:dirt", "test:land/cells_3"] {
        assert_eq!(id(name).as_str(), name);
        assert_eq!(id(name).to_string(), name);
    }
    for name in ["", "test", ":a", "test:", "Test:a", "test:a-b", "test:a:b"] {
        assert_eq!(
            GenId::new(name),
            Err(RegistryError::InvalidName(name.to_string())),
            "{name}"
        );
    }
    // Byte order: '/' < '0' < '_' < 'a'.
    let mut ids = ["test:b", "test:a_b", "test:a/b", "test:a", "test:a0"].map(id);
    ids.sort();
    assert_eq!(
        ids.each_ref().map(GenId::as_str),
        ["test:a", "test:a/b", "test:a0", "test:a_b", "test:b"]
    );
}

#[test]
fn salts_and_parameter_hashes_have_fixed_values() {
    // FNV-1a 64 reference values.
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    // The salt is the name's alone, the same on every run and platform (an independent
    // implementation computed the literals).
    let a = id("test:a");
    assert_eq!(a.salt(), splitmix64(fnv1a64(b"test:a")));
    assert_eq!(a.salt(), 0x433c_6992_2ec0_bb43);
    assert_eq!(a.seed(7), mix_seed(7, a.salt()));
    // The parameter encoding: `7u32 | len 2 as u64 | "ab" | 1 | -1i64 | 1 | 0.5f64 bits`.
    assert_eq!(
        hash_params(&(7u32, "ab", Some(-1i64), (true, 0.5f64))),
        0x4574_d84e_6487_3ba7
    );
    // Lengths keep concatenations apart; widths are fixed; the containers agree.
    assert_ne!(hash_params(&("ab", "c")), hash_params(&("a", "bc")));
    assert_ne!(hash_params(&[1u8, 2]), hash_params(&[1u16, 2]));
    assert_ne!(hash_params(&None::<u8>), hash_params(&Some(0u8)));
    assert_eq!(hash_params(&3usize), hash_params(&3u64));
    assert_eq!(hash_params(&vec![1u32, 2]), hash_params(&[1u32, 2]));
    assert_eq!(hash_params(&[1u32, 2][..]), hash_params(&[1u32, 2]));
    assert_eq!(hash_params(&id("test:a")), hash_params("test:a"));
    assert_eq!(hash_params(&String::from("x")), hash_params("x"));
    assert_ne!(hash_params(&0.0f64), hash_params(&-0.0f64));
}

#[test]
fn freezing_sorts_by_id_with_dense_indices() {
    let r = registry(&[
        (id("test:c"), Entry::new(3)),
        (id("test:a"), Entry::new(1)),
        (id("test:b"), Entry::new(2)),
    ]);
    assert_eq!(r.name(), "test_entry");
    assert_eq!(r.len(), 3);
    let ids: Vec<&str> = r.iter().map(|(_, id, _)| id.as_str()).collect();
    assert_eq!(ids, ["test:a", "test:b", "test:c"]);
    for (i, (ix, id, entry)) in r.iter().enumerate() {
        assert_eq!(ix.index(), i);
        assert_eq!(r.find(id.as_str()), Some(ix));
        assert_eq!(r[ix], *entry);
        assert_eq!(r.salt(ix), id.salt());
        assert_eq!(r.seed(ix, 42), id.seed(42));
        assert_eq!(r.version(ix), 1);
    }
    assert_eq!(r.find("test:d"), None);
    assert_eq!(r.find("test:"), None);
    let params: HashSet<u64> = r.indices().map(|ix| r.params_hash(ix)).collect();
    assert_eq!(params.len(), 3, "other weights, other parameter hashes");

    let empty = GenRegistryBuilder::<Entry>::new("test_entry")
        .freeze()
        .expect("empty");
    assert!(empty.is_empty());
    assert_eq!(empty.find("test:a"), None);
}

#[test]
fn ordered_entries_sort_by_phase_then_priority_then_id() {
    let r = registry(&[
        (id("test:z"), Entry::ordered(0, 0, 0)),
        (id("test:fallback"), Entry::ordered(0, 0, i32::MIN)),
        (id("test:late"), Entry::ordered(0, 2, -3)),
        (id("test:high"), Entry::ordered(0, 0, 10)),
        (id("test:b"), Entry::ordered(0, 1, 5)),
        (id("test:a"), Entry::ordered(0, 1, 5)),
    ]);
    let ids: Vec<&str> = r.iter().map(|(_, id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "test:high",
            "test:z",
            "test:fallback",
            "test:a",
            "test:b",
            "test:late"
        ],
        "phase ascending, priority descending (the fallback last), then id"
    );
    for (ix, id, _) in r.iter() {
        assert_eq!(r.find(id.as_str()), Some(ix), "found by id in any order");
    }
    // The order is part of the parameter hash: a new priority is a changed digest line.
    let low = registry(&[(id("test:a"), Entry::ordered(7, 0, 1))]);
    let high = registry(&[(id("test:a"), Entry::ordered(7, 0, 2))]);
    let ix = GenIx::new(0);
    assert_ne!(low.params_hash(ix), high.params_hash(ix));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn registering_in_any_order_gives_the_same_registry(
        (entries, shuffled) in entries_and_shuffled(),
    ) {
        let (a, b) = (registry(&entries), registry(&shuffled));
        prop_assert_eq!(table(&a), table(&b));
        let (da, db) = (digest(&a), digest(&b));
        prop_assert_eq!(da.hash(), db.hash());
        prop_assert_eq!(da, db);
    }

    #[test]
    fn adding_an_entry_moves_no_other(
        base in prop::collection::btree_set(0u32..400, 0..30),
        extra in 400u32..800,
        weight in 0u32..64,
        world in any::<u64>(),
    ) {
        let entries: Vec<_> = base.iter().map(|&n| numbered(n)).collect();
        let extra = (id(&name(extra)), Entry::new(weight));
        let mut with = entries.clone();
        with.insert(entries.len() / 2, extra.clone());
        let (a, b) = (registry(&entries), registry(&with));
        prop_assert_eq!(b.len(), a.len() + 1);
        for (ix, id, entry) in a.iter() {
            let jx = b.find(id.as_str()).expect("still there");
            // Only the index moves, by one if the new id sorts before.
            prop_assert_eq!(jx.index(), ix.index() + usize::from(extra.0 < *id));
            prop_assert_eq!(b[jx], *entry);
            prop_assert_eq!(b.salt(jx), a.salt(ix));
            prop_assert_eq!(b.seed(jx, world), a.seed(ix, world));
            prop_assert_eq!(b.version(jx), a.version(ix));
            prop_assert_eq!(b.params_hash(jx), a.params_hash(ix));
        }
        // The diff shows exactly the new line, and the other way exactly its removal.
        let (da, db) = (digest(&a), digest(&b));
        let new_ix = b.find(extra.0.as_str()).expect("added");
        let new_line = DigestEntry::of(&realm_scope(), &b, new_ix);
        let added = WorldgenDiff::between(&da, &db);
        prop_assert_eq!(&added.added, &vec![new_line.clone()]);
        prop_assert!(added.removed.is_empty() && added.changed.is_empty());
        let removed = WorldgenDiff::between(&db, &da);
        prop_assert_eq!(&removed.removed, &vec![new_line]);
        prop_assert!(removed.added.is_empty() && removed.changed.is_empty());
        // Removing it gives the old registry and digest back, bit for bit.
        let without: Vec<_> = with.into_iter().filter(|(id, _)| *id != extra.0).collect();
        let c = registry(&without);
        prop_assert_eq!(table(&c), table(&a));
        prop_assert_eq!(digest(&c), da);
    }
}

#[test]
fn duplicates_are_refused_with_their_ids() {
    let mut builder = GenRegistryBuilder::new("test_entry");
    builder
        .register(id("test:a"), Entry::new(1))
        .register(id("test:c"), Entry::new(2))
        .register(id("test:b"), Entry::new(3))
        // Another order does not make it another entry.
        .register(id("test:a"), Entry::ordered(4, 2, 9))
        .register(id("test:c"), Entry::new(2))
        .register(id("test:c"), Entry::new(5));
    assert_eq!(builder.len(), 6);
    let error = builder.freeze().expect_err("duplicates");
    assert_eq!(
        error.problems(),
        [
            Problem::Duplicate {
                registry: "test_entry",
                id: id("test:a"),
                count: 2
            },
            Problem::Duplicate {
                registry: "test_entry",
                id: id("test:c"),
                count: 3
            },
        ]
    );
    assert_eq!(
        error.to_string(),
        "world generation checks failed with 2 problems:\n  test_entry test:a is registered 2 \
         times\n  test_entry test:c is registered 3 times"
    );
    assert_eq!(error.problems()[1].ids(), [&id("test:c")]);
}

#[test]
fn checks_report_every_problem_of_every_registry_at_once() {
    let mut checks = Checks::new();
    let mut kinds = GenRegistryBuilder::new("test_kind");
    kinds
        .register(id("test:a"), Entry::new(1))
        .register(id("test:a"), Entry::new(1));
    let mut rules = GenRegistryBuilder::new("test_rule");
    rules
        .register(id("test:a"), Entry::new(1))
        .register(id("test:b"), Entry::new(1));
    let mut stages = GenRegistryBuilder::new("test_stage");
    stages
        .register(id("test:x"), Entry::new(1))
        .register(id("test:x"), Entry::new(2));
    assert!(checks.freeze(kinds).is_none());
    let rules = checks
        .freeze(rules)
        .expect("no duplicate: the same id in another registry");
    assert!(checks.freeze(stages).is_none());
    assert_eq!(checks.problems().len(), 2);
    assert!(!checks.is_empty());
    let message = checks
        .finish()
        .expect_err("two registries failed")
        .to_string();
    assert!(
        message.contains("test_kind test:a is registered 2 times"),
        "{message}"
    );
    assert!(
        message.contains("test_stage test:x is registered 2 times"),
        "{message}"
    );
    assert_eq!(rules.len(), 2);
    assert_eq!(Checks::new().finish(), Ok(()));
}

#[test]
fn unknown_ids_name_who_asked_for_them() {
    let r = registry(&[(id("test:a"), Entry::new(1))]);
    let mut checks = Checks::new();
    let preset = id("test:preset");
    assert_eq!(checks.resolve(&r, &id("test:a"), &preset), r.find("test:a"));
    assert!(checks.is_empty());
    assert_eq!(checks.resolve(&r, &id("test:missing"), &preset), None);
    let error = checks.finish().expect_err("unknown id");
    assert_eq!(
        error.problems(),
        [Problem::Unknown {
            registry: "test_entry",
            id: id("test:missing"),
            by: preset,
        }]
    );
    assert_eq!(
        error.to_string(),
        "world generation check failed: test:preset names test_entry test:missing, which is not \
         registered"
    );
}

#[test]
fn other_problems_carry_their_ids_and_message() {
    let problem = Problem::Other {
        ids: vec![id("test:steep"), id("test:flat")],
        message: "kinds test:steep and test:flat meet too steeply".to_string(),
    };
    assert_eq!(problem.ids(), [&id("test:steep"), &id("test:flat")]);
    assert_eq!(
        problem.to_string(),
        "kinds test:steep and test:flat meet too steeply"
    );
}

#[test]
fn no_two_salts_or_octave_seeds_of_the_test_entries_collide() {
    // Octave k of an fbm takes the seed `seed + k`: no entry's octaves may reach another's,
    // under any of these world seeds (a salt added to the seed would put entries a few salts
    // apart on each other's octaves).
    let r = registry(&(0..512).map(numbered).collect::<Vec<_>>());
    let salts: HashSet<u64> = r.indices().map(|ix| r.salt(ix)).collect();
    assert_eq!(salts.len(), r.len());
    let mut seen = HashSet::new();
    for world in [0, 1, 2, 3, 0x5eed, u64::MAX - 1, u64::MAX] {
        for ix in r.indices() {
            let seed = r.seed(ix, world);
            for k in 0..64u64 {
                assert!(
                    seen.insert(seed.wrapping_add(k)),
                    "{} octave {k} of world {world} collides",
                    r.id(ix)
                );
            }
        }
    }
}

#[test]
fn a_digest_lists_each_line_once_in_order() {
    let r = registry(&[(id("test:b"), Entry::new(2)), (id("test:a"), Entry::new(1))]);
    let (a, b) = (r.find("test:a").expect("a"), r.find("test:b").expect("b"));
    let dim = id("test:dimension");
    let (r1, r2) = (
        DigestScope::of_realm(&dim, &id("test:r1")),
        DigestScope::of_realm(&dim, &id("test:r2")),
    );
    let mut forward = DigestBuilder::new();
    forward
        .add(&r2, &r, a)
        .add(&r1, &r, b)
        .add(&r1, &r, a)
        .add(&r1, &r, a);
    let mut backward = DigestBuilder::new();
    backward.add(&r1, &r, a).add(&r1, &r, b).add(&r2, &r, a);
    let (forward, backward) = (forward.finish(), backward.finish());
    assert_eq!(forward, backward);
    let lines: Vec<(&str, &str)> = forward
        .entries()
        .iter()
        .map(|e| {
            (
                e.scope.realm.as_ref().expect("realm").as_str(),
                e.id.as_str(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            ("test:r1", "test:a"),
            ("test:r1", "test:b"),
            ("test:r2", "test:a")
        ]
    );
    assert_eq!(forward.version(), forward.hash() as u32);
    assert_eq!(
        forward.entries()[0].to_string(),
        format!(
            "test_entry test:a in realm test:r1 of dimension test:dimension (version 1, params \
             {:#018x})",
            r.params_hash(a)
        )
    );

    // Any field of any line changes the hash.
    let base = forward.entries().to_vec();
    let edits: [fn(&mut DigestEntry); 6] = [
        |e| e.scope = DigestScope::PRESET,
        |e| e.scope.realm = Some(GenId::new("test:r3").expect("id")),
        |e| e.registry = "test_other".into(),
        |e| e.id = GenId::new("test:c").expect("id"),
        |e| e.version += 1,
        |e| e.params ^= 1,
    ];
    for edit in edits {
        let mut lines = base.clone();
        edit(&mut lines[1]);
        assert_ne!(RegistryDigest::from_entries(lines).hash(), forward.hash());
    }
}

#[test]
fn a_dimension_has_its_own_digest() {
    let r = registry(&[(id("test:a"), Entry::new(1)), (id("test:b"), Entry::new(2))]);
    let (a, b) = (GenIx::new(0), GenIx::new(1));
    let (d1, d2) = (id("test:d1"), id("test:d2"));
    let mut whole = DigestBuilder::new();
    whole
        .add(&DigestScope::PRESET, &r, a)
        .add(&DigestScope::of_dimension(&d1), &r, a)
        .add(&DigestScope::of_realm(&d1, &id("test:r")), &r, b)
        .add(&DigestScope::of_dimension(&d2), &r, b);
    let whole = whole.finish();
    let mut only_d1 = DigestBuilder::new();
    only_d1.add(&DigestScope::of_dimension(&d1), &r, a).add(
        &DigestScope::of_realm(&d1, &id("test:r")),
        &r,
        b,
    );
    assert_eq!(whole.dimension(&d1), only_d1.finish());
    assert_eq!(whole.dimension(&d2).entries().len(), 1);
    assert_ne!(whole.dimension(&d1).hash(), whole.hash());
    assert!(whole.dimension(&id("test:d3")).entries().is_empty());
}

#[test]
fn the_diff_lists_added_removed_and_changed_lines() {
    let dim = id("test:overworld");
    let preset = DigestScope::PRESET;
    let world = DigestScope::of_dimension(&dim);
    let surface = DigestScope::of_realm(&dim, &id("test:surface"));
    let deep = DigestScope::of_realm(&dim, &id("test:deep"));
    let saved = RegistryDigest::from_entries(vec![
        line(&preset, "test_preset", "test:default", 1, 10),
        line(&world, "test_generator", "test:regions", 1, 20),
        line(&surface, "test_kind", "test:plains", 1, 30),
        line(&surface, "test_kind", "test:hills", 1, 31),
        line(&surface, "test_kind", "test:dunes", 2, 32),
        line(&surface, "test_kind", "test:mod_kind", 1, 33),
        line(&surface, "test_rule", "test:plains", 1, 34),
    ]);
    let live = RegistryDigest::from_entries(vec![
        // A realm added: its lines are new, even for a kind the surface had already.
        line(&deep, "test_kind", "test:plains", 1, 30),
        line(&surface, "test_rule", "test:plains", 1, 34),
        line(&surface, "test_kind", "test:dunes", 2, 99),
        line(&surface, "test_kind", "test:hills", 2, 31),
        line(&surface, "test_kind", "test:plains", 1, 30),
        line(&world, "test_generator", "test:regions", 1, 20),
        line(&preset, "test_preset", "test:default", 1, 10),
    ]);
    let diff = WorldgenDiff::between(&saved, &live);
    assert_eq!(diff.added, [line(&deep, "test_kind", "test:plains", 1, 30)]);
    assert_eq!(
        diff.removed,
        [line(&surface, "test_kind", "test:mod_kind", 1, 33)]
    );
    assert_eq!(
        diff.changed,
        [
            Changed {
                saved: line(&surface, "test_kind", "test:dunes", 2, 32),
                live: line(&surface, "test_kind", "test:dunes", 2, 99),
            },
            Changed {
                saved: line(&surface, "test_kind", "test:hills", 1, 31),
                live: line(&surface, "test_kind", "test:hills", 2, 31),
            },
        ]
    );
    let message = diff.to_string();
    for needle in [
        "1 added, 1 removed, 2 changed",
        "added test_kind test:plains in realm test:deep of dimension test:overworld",
        "removed test_kind test:mod_kind in realm test:surface",
        "changed test_kind test:hills in realm test:surface of dimension test:overworld: version 1 \
         -> 2",
        "changed test_kind test:dunes",
    ] {
        assert!(message.contains(needle), "{needle:?} not in {message}");
    }
    assert!(!diff.is_empty());

    let same = WorldgenDiff::between(&saved, &saved);
    assert!(same.is_empty());
    assert_eq!(
        same.to_string(),
        "world generation unchanged since the world was saved"
    );
    let empty = RegistryDigest::from_entries(Vec::new());
    assert_eq!(WorldgenDiff::between(&empty, &live).added, live.entries());
    assert_eq!(WorldgenDiff::between(&live, &empty).removed, live.entries());
}

#[test]
fn a_damaged_list_keeps_one_line_per_key() {
    let scope = realm_scope();
    let digest = RegistryDigest::from_entries(vec![
        line(&scope, "test_kind", "test:a", 2, 5),
        line(&scope, "test_kind", "test:a", 1, 9),
        line(&scope, "test_kind", "test:a", 1, 9),
    ]);
    assert_eq!(
        digest.entries(),
        [line(&scope, "test_kind", "test:a", 1, 9)]
    );
}
