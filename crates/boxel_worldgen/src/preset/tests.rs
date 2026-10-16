//! Presets, dimensions and generator kinds over `test:` entries (ADR 0039 "Cómo se prueba" 4 and
//! 10): a world open resolves every id and fails with all of them at once; a legacy factory gives
//! the generator it wraps; the record holds the preset, the name table, the realms, the spec and
//! the digest; dimensions keep their saved numbers; a changed spec, kind version or dimension is a
//! changed line of the diff, and nothing changed is an empty diff. The region generators' side
//! (realms added, entries removed, block names of a realm) is in `tests/preset.rs`.
use super::*;
use crate::FlatGenerator;
use crate::realm::RealmSpan;
use boxel_voxel::{ChunkKey, ChunkPos};

fn id(name: &str) -> GenId {
    GenId::new(name).expect(name)
}

fn blocks() -> BlockRegistry {
    boxel_voxel::testing::registry()
}

/// The layers of a test flat world, as a spec's parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Layers(Vec<(String, u32)>);

impl HashParams for Layers {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0.len());
        for (name, n) in &self.0 {
            h.write(name).write(n);
        }
    }
}

fn layers(n: u32) -> Layers {
    Layers(vec![("test:solid".into(), n), ("test:glass".into(), 1)])
}

/// A flat generator from its spec's [`Layers`], with their block names checked.
fn flat_kind(version: u32) -> GeneratorKind {
    GeneratorKind::new(version, |build, checks| {
        let layers = build.params::<Layers>(checks)?;
        if !build.check_blocks(checks, layers.0.iter().map(|(name, _)| name.as_str())) {
            return None;
        }
        Some(Built::new(Arc::new(FlatGenerator::new(layers.0.clone()))))
    })
}

/// The test app: presets `test:flat` (dimension `test:overworld`, a flat generator) and
/// `test:legacy` (the same dimension through a legacy factory), `test:two` (both dimensions),
/// and broken ones.
fn worldgen(flat_version: u32, solid: u32) -> Worldgen {
    let mut builder = WorldgenBuilder::new();
    let flat = || GeneratorSpec::new(id("test:flat"), layers(solid));
    builder
        .preset(
            id("test:flat"),
            WorldPreset::one(id("test:overworld"), flat()),
        )
        .preset(
            id("test:legacy"),
            WorldPreset::one(
                id("test:overworld"),
                GeneratorSpec::new(id("test:legacy"), ()),
            ),
        )
        .preset(
            id("test:two"),
            WorldPreset {
                dimensions: vec![
                    (id("test:overworld"), flat()),
                    (id("test:nether"), GeneratorSpec::new(id("test:legacy"), ())),
                ],
                version: 1,
            },
        )
        .preset(
            id("test:broken"),
            WorldPreset {
                dimensions: vec![
                    (id("test:overworld"), flat()),
                    (id("test:overworld"), flat()),
                    (id("test:moon"), flat()),
                    (
                        id("test:nether"),
                        GeneratorSpec::new(id("test:missing"), ()),
                    ),
                ],
                version: 1,
            },
        )
        .preset(
            id("test:typo"),
            WorldPreset::one(
                id("test:overworld"),
                GeneratorSpec::new(id("test:flat"), Layers(vec![("test:sold".into(), 3)])),
            ),
        )
        .preset(
            id("test:wrong_params"),
            WorldPreset::one(
                id("test:overworld"),
                GeneratorSpec::new(id("test:flat"), 7u32),
            ),
        )
        .dimension(id("test:overworld"), DimensionDef::default())
        .dimension(id("test:nether"), DimensionDef::default())
        .generator(id("test:flat"), flat_kind(flat_version))
        .generator(
            id("test:legacy"),
            GeneratorKind::legacy(1, |seed| {
                Arc::new(FlatGenerator::new(vec![(
                    "test:solid".into(),
                    1 + (seed % 7) as u32,
                )]))
            }),
        );
    let mut checks = Checks::new();
    let worldgen = builder.freeze(&mut checks, ());
    checks.finish().expect("the test app builds");
    worldgen.expect("frozen")
}

fn key(y: i64) -> ChunkKey {
    ChunkKey::new(ChunkPos::new(0, y, 0))
}

#[test]
fn a_legacy_factory_gives_the_generator_it_wraps() {
    let w = worldgen(1, 3);
    let opened = w
        .open(&id("test:legacy"), 12, &blocks(), None)
        .expect("opens");
    let generator = opened.generator("test:overworld").expect("its dimension");
    let direct = FlatGenerator::new(vec![("test:solid".into(), 1 + 12 % 7)]);
    assert_eq!(generator.name(), direct.name());
    assert_eq!(generator.vertical_chunks(), direct.vertical_chunks());
    for y in -1..2 {
        assert_eq!(
            generator.generate(key(y), &blocks()),
            direct.generate(key(y), &blocks())
        );
    }
    assert!(opened.diff.is_none(), "nothing saved to compare");
    // The record: the preset's line, one dimension numbered 0, the generator's own realm (the
    // default one), its spec and its lines.
    let record = &opened.record;
    assert_eq!(record.preset.id.as_str(), "test:legacy");
    assert_eq!(record.preset.registry, PRESET_REGISTRY);
    assert_eq!(record.names, [id("test:overworld")]);
    assert_eq!(record.number("test:overworld"), Some(DimensionId(0)));
    assert_eq!(opened.first().number, DimensionId(0));
    let dim = record.dimension("test:overworld").expect("recorded");
    assert_eq!(dim.realms, [(id("boxel:default"), RealmSpan::ALL)]);
    assert_eq!(
        dim.generator,
        SpecLine {
            kind: id("test:legacy"),
            version: 1,
            params: hash_params(&()),
        }
    );
    let lines: Vec<String> = dim
        .digest
        .entries()
        .iter()
        .map(|e| format!("{} {} {}", e.scope, e.registry, e.id))
        .collect();
    assert_eq!(
        lines,
        [
            "dimension test:overworld dimension test:overworld",
            "dimension test:overworld generator_kind test:legacy",
        ]
    );
}

#[test]
fn reopening_with_the_same_registries_changes_nothing() {
    let w = worldgen(1, 3);
    let first = w.open(&id("test:two"), 5, &blocks(), None).expect("opens");
    assert_eq!(
        first.record.names,
        [id("test:overworld"), id("test:nether")]
    );
    let again = w
        .open(&id("test:two"), 5, &blocks(), Some(&first.record))
        .expect("opens");
    assert_eq!(again.record, first.record);
    let diff = again.diff.expect("compared");
    assert!(diff.is_empty(), "{diff}");
    // Another seed is the same record: the seed is the world's, not a parameter.
    let other = w
        .open(&id("test:two"), 6, &blocks(), Some(&first.record))
        .expect("opens");
    assert!(other.diff.expect("compared").is_empty());
}

#[test]
fn a_changed_spec_or_kind_version_is_a_changed_line() {
    let saved = worldgen(1, 3)
        .open(&id("test:flat"), 1, &blocks(), None)
        .expect("opens")
        .record;
    // Other parameters: the spec's line changes (the preset's own line does not: it holds no
    // parameter that changes chunks).
    let thicker = worldgen(1, 4)
        .open(&id("test:flat"), 1, &blocks(), Some(&saved))
        .expect("opens");
    let diff = thicker.diff.expect("compared");
    let changed: Vec<(&str, &str)> = diff
        .changed
        .iter()
        .map(|c| (&*c.live.registry, c.live.id.as_str()))
        .collect();
    assert_eq!(changed, [("generator_kind", "test:flat")]);
    assert!(diff.added.is_empty() && diff.removed.is_empty());
    assert_eq!(thicker.record.preset, saved.preset);
    assert_ne!(
        thicker.record.dimensions[0].generator,
        saved.dimensions[0].generator
    );
    // A new version of the kind's code.
    let bumped = worldgen(2, 3)
        .open(&id("test:flat"), 1, &blocks(), Some(&saved))
        .expect("opens");
    let diff = bumped.diff.expect("compared");
    assert_eq!(diff.changed.len(), 1, "{diff}");
    assert_eq!(
        (diff.changed[0].saved.version, diff.changed[0].live.version),
        (1, 2)
    );
    assert!(
        diff.to_string().contains("generator_kind test:flat"),
        "{diff}"
    );
}

#[test]
fn another_preset_is_a_changed_or_moved_line() {
    let w = worldgen(1, 3);
    let saved = w
        .open(&id("test:flat"), 1, &blocks(), None)
        .expect("opens")
        .record;
    let legacy = w
        .open(&id("test:legacy"), 1, &blocks(), Some(&saved))
        .expect("opens");
    let diff = legacy.diff.expect("compared");
    let ids = |lines: &[DigestEntry]| -> Vec<String> {
        lines
            .iter()
            .map(|e| format!("{} {}", e.registry, e.id))
            .collect()
    };
    // The preset's line sorts first (its scope is the preset's).
    assert_eq!(
        ids(&diff.added),
        ["preset test:legacy", "generator_kind test:legacy"]
    );
    assert_eq!(
        ids(&diff.removed),
        ["preset test:flat", "generator_kind test:flat"]
    );
    assert!(diff.changed.is_empty());
}

#[test]
fn dimensions_keep_their_saved_numbers() {
    let w = worldgen(1, 3);
    // A world saved with only the nether (by a preset of old), in a table that only grows.
    let mut saved = w
        .open(&id("test:two"), 1, &blocks(), None)
        .expect("opens")
        .record;
    saved.names = vec![id("test:gone"), id("test:nether")];
    let opened = w
        .open(&id("test:two"), 1, &blocks(), Some(&saved))
        .expect("opens");
    let numbers: Vec<(&str, u16)> = opened
        .dimensions
        .iter()
        .map(|d| (d.id.as_str(), d.number.0))
        .collect();
    assert_eq!(numbers, [("test:overworld", 2), ("test:nether", 1)]);
    assert_eq!(
        opened.record.names,
        [id("test:gone"), id("test:nether"), id("test:overworld")]
    );
    assert_eq!(opened.record.number("test:gone"), Some(DimensionId(0)));
    assert_eq!(opened.record.number("test:moon"), None);
}

#[test]
fn a_full_dimension_table_is_an_error_not_a_panic() {
    let w = worldgen(1, 3);
    let mut saved = w
        .open(&id("test:flat"), 1, &blocks(), None)
        .expect("opens")
        .record;
    // Every number taken by other names: the overworld has none left.
    saved.names = (0..MAX_DIMENSIONS)
        .map(|n| id(&format!("test:d{n}")))
        .collect();
    let error = w
        .open(&id("test:flat"), 1, &blocks(), Some(&saved))
        .expect_err("no number left");
    assert_eq!(
        error.to_string(),
        "world generation check failed: dimension test:overworld gets no number: it is name \
         65536 of the world's dimension table, which has 65536 numbers"
    );
    // A table made by hand may be longer than any saved one: a name past the numbers is the same
    // problem.
    let mut long = saved.clone();
    long.names.push(id("test:overworld"));
    assert!(w.open(&id("test:flat"), 1, &blocks(), Some(&long)).is_err());
    // The last number is still a number.
    saved.names.pop();
    let opened = w
        .open(&id("test:flat"), 1, &blocks(), Some(&saved))
        .expect("the last number");
    assert_eq!(opened.first().number, DimensionId(u16::MAX));
}

#[test]
fn a_bad_preset_fails_at_world_open_with_every_id() {
    let w = worldgen(1, 3);
    let error = w
        .open(&id("test:broken"), 1, &blocks(), None)
        .expect_err("broken");
    let message = error.to_string();
    for needle in [
        "preset test:broken lists dimension test:overworld twice",
        "test:broken names dimension test:moon, which is not registered",
        "test:broken names generator_kind test:missing, which is not registered",
    ] {
        assert!(message.contains(needle), "{needle:?} not in {message}");
    }
    assert_eq!(error.problems().len(), 3, "{message}");
    // The root has no referrer.
    let unknown = w
        .open(&id("test:nothing"), 1, &blocks(), None)
        .expect_err("unknown");
    assert_eq!(
        unknown.to_string(),
        "world generation check failed: preset test:nothing is not registered"
    );
}

#[test]
fn a_typo_in_a_block_name_fails_at_world_open() {
    let error = worldgen(1, 3)
        .open(&id("test:typo"), 1, &blocks(), None)
        .expect_err("a typo");
    assert_eq!(
        error.problems(),
        [Problem::UnknownBlock {
            by: id("test:flat"),
            name: "test:sold".into(),
        }]
    );
    assert_eq!(
        error.to_string(),
        "world generation check failed: test:flat names the block \"test:sold\", which is not \
         in the block registry"
    );
}

#[test]
fn parameters_of_another_type_are_refused() {
    let error = worldgen(1, 3)
        .open(&id("test:wrong_params"), 1, &blocks(), None)
        .expect_err("wrong parameters");
    let message = error.to_string();
    assert!(
        message.contains(
            "the generator spec of dimension test:overworld gives generator kind test:flat \
             parameters that are not its"
        ) && message.contains("Layers"),
        "{message}"
    );
}

#[test]
fn a_factory_that_builds_nothing_must_say_why() {
    let mut builder = WorldgenBuilder::new();
    builder
        .preset(
            id("test:silent"),
            WorldPreset::one(
                id("test:overworld"),
                GeneratorSpec::new(id("test:none"), ()),
            ),
        )
        .dimension(id("test:overworld"), DimensionDef::default())
        .generator(id("test:none"), GeneratorKind::new(1, |_, _| None));
    let mut checks = Checks::new();
    let w = builder.freeze(&mut checks, ()).expect("frozen");
    let message = w
        .open(&id("test:silent"), 1, &blocks(), None)
        .expect_err("nothing built")
        .to_string();
    assert!(
        message.contains(
            "generator kind test:none built nothing for dimension test:overworld and reported \
             no problem"
        ),
        "{message}"
    );
}

#[test]
fn duplicates_are_refused_when_the_app_is_built() {
    let mut builder: WorldgenBuilder = WorldgenBuilder::new();
    builder
        .dimension(id("test:overworld"), DimensionDef::default())
        .dimension(id("test:overworld"), DimensionDef::default())
        .generator(
            id("test:legacy"),
            GeneratorKind::legacy(1, |_| unreachable!()),
        )
        .generator(
            id("test:legacy"),
            GeneratorKind::legacy(2, |_| unreachable!()),
        );
    let mut checks = Checks::new();
    assert!(builder.freeze(&mut checks, ()).is_none());
    let message = checks.finish().expect_err("duplicates").to_string();
    assert!(
        message.contains("dimension test:overworld is registered 2 times")
            && message.contains("generator_kind test:legacy is registered 2 times"),
        "{message}"
    );
}
