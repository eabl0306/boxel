//! The registries against ADR 0039 "Cómo se prueba" 3 to 5 (worldgen plan sec. 10, tests 4 and
//! 5): registering in any order gives the same indices and salts; duplicates and unknown ids
//! fail with their ids in the message; an added entry changes no other entry's salt or seed, and
//! removing it gives the old registry back; no two salts or octave seeds of the test entries
//! collide; the fixed values of the salt and parameter hash. The checks of realm layouts are in
//! `realm/tests.rs`.
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
        // Removing it gives the old registry back, bit for bit.
        let without: Vec<_> = with.into_iter().filter(|(id, _)| *id != extra.0).collect();
        let c = registry(&without);
        prop_assert_eq!(table(&c), table(&a));
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
