//! A world of region generators through a preset (ADR 0039 "Cómo se prueba" 4 and 10, worldgen
//! plan test 13), over the `test:` content of `support/region.rs`: a dimension of a deep realm
//! (a plain generator) under the surface realm (a region generator), built by a factory at every
//! open.
//! - the factory's dimension gives the chunks of the region generator built by hand, and its
//!   record lists the realms, the spec and every line;
//! - against the saved record, a changed entry version, an added realm and a removed entry give
//!   the expected `WorldgenDiff`, and the same registries give an empty one;
//! - a typo in a block name of a realm or a stage fails at world open with the id in the
//!   message, together with the region map's own problems: every freeze check runs;
//! - the realms are the preset's: one dimension opens with the region layout, with a layout of
//!   its own in another preset, and with no layout through a legacy factory, in one app.
#[path = "support/region.rs"]
mod support;

use boxel_voxel::{BlockState, ChunkKey, ChunkPos};
use boxel_worldgen::preset::{
    Build, Built, DimensionDef, GeneratorKind, GeneratorSpec, PresetDimension, WorldPreset,
    Worldgen, WorldgenBuilder,
};
use boxel_worldgen::realm::{RealmLod, RealmSpan, RealmSpec};
use boxel_worldgen::region::{
    Decoration, DimensionGenerator, Entry, FloorFill, RegionGenerator, RegionSettings, RegionSpec,
    RuleEntry, SelectorRule, TerrainKind, TopLayers,
};
use boxel_worldgen::registry::{Checks, GenRegistry, WorldgenDiff};
use boxel_worldgen::{FlatGenerator, WorldGenerator};
use std::sync::Arc;
use support::*;

/// Region cells of the tests: 1024 blocks.
const CELLS: u32 = 10;

/// The game's own registries in this test app: the region map and the kinds.
struct Terrain {
    rules: GenRegistry<SelectorRule>,
    entries: GenRegistry<RuleEntry>,
    kinds: GenRegistry<Arc<dyn TerrainKind<TestContext>>>,
}

/// The contents of a test app.
struct App {
    realms: Vec<(&'static str, RealmSpec)>,
    rules: Vec<(&'static str, SelectorRule)>,
    entries: Vec<(&'static str, RuleEntry)>,
    /// The dimension's realms in the preset `test:default`.
    dimension: Vec<&'static str>,
    surface: TopLayers,
}

/// The realms of the test app: `test:deep` (a plain generator) under `test:surface`.
fn realms() -> Vec<(&'static str, RealmSpec)> {
    vec![
        (
            "test:deep",
            RealmSpec {
                span: RealmSpan::below(-4096),
                lod: RealmLod::Off,
                env: id("test:dark"),
                ..realm()
            },
        ),
        (
            "test:surface",
            RealmSpec {
                span: RealmSpan::above(-4096),
                ..realm()
            },
        ),
    ]
}

fn app() -> App {
    App {
        realms: realms(),
        rules: rules(CELLS),
        entries: vec![(
            "test:mod_entry",
            RuleEntry {
                rule: id("test:land"),
                entry: Entry::new(id("test:basin"), 1),
                version: 1,
            },
        )],
        dimension: vec!["test:deep", "test:surface"],
        surface: TopLayers::new(vec![("test:cutout".into(), 1), ("test:glass".into(), 2)]),
    }
}

/// The deep realm's generator: plain rock, no region generator (so its realm gets a
/// `realm_generator` line).
fn deep() -> FlatGenerator {
    FlatGenerator::new(Vec::new())
}

/// The factory of `test:regions` with the surface stage `surface`: a region generator for each
/// realm with rules, the deep generator for the others, in a dimension generator. Its
/// parameters are the lattice's shift.
fn regions(build: &Build<'_, Terrain>, checks: &mut Checks, surface: &TopLayers) -> Option<Built> {
    let lattice = *build.params::<u32>(checks)?;
    let stack = build.stack?;
    let t = build.registries();
    let mut lines = build.lines();
    let mut realms: Vec<Box<dyn WorldGenerator>> = Vec::new();
    for (realm, _) in stack.iter() {
        if !t.rules.iter().any(|(_, _, r)| r.realm == *realm) {
            realms.push(Box::new(deep()));
            continue;
        }
        let g = RegionGenerator::build(
            RegionSpec {
                seed: build.seed,
                dimension: build.dimension,
                realm,
                realms: build.worldgen.realms(),
                rules: &t.rules,
                entries: &t.entries,
                kinds: &t.kinds,
                blocks: build.blocks,
                decoration: Decoration::new(
                    (id("test:fill"), Arc::new(FloorFill)),
                    (id("test:top"), Arc::new(surface.clone())),
                ),
                settings: RegionSettings {
                    lattice,
                    ..RegionSettings::default()
                },
            },
            |s| TestContext::new(s, BASE),
            checks,
        )?;
        lines.extend(g.digest());
        realms.push(Box::new(g));
    }
    let dimension = DimensionGenerator::new(
        build.dimension.clone(),
        stack.clone(),
        realms,
        &lines.finish(),
    );
    Some(dimension.into())
}

impl App {
    /// The frozen app.
    fn freeze(self) -> Worldgen<Terrain> {
        let surface = self.surface;
        let world = world_of(
            self.realms.clone(),
            self.rules,
            self.entries,
            kind_entries(),
        );
        let terrain = Terrain {
            rules: world.rules,
            entries: world.entries,
            kinds: world.kinds,
        };
        let mut builder = WorldgenBuilder::new();
        for (name, spec) in self.realms {
            builder.realm(id(name), spec);
        }
        builder
            .dimension(id("test:overworld"), DimensionDef { version: 1 })
            .generator(
                id("test:regions"),
                GeneratorKind::new(1, move |build, checks| regions(build, checks, &surface)),
            )
            .preset(
                id("test:default"),
                WorldPreset::one(
                    PresetDimension::new(
                        id("test:overworld"),
                        GeneratorSpec::new(id("test:regions"), 8u32),
                    )
                    .with_realms(self.dimension.into_iter().map(id)),
                ),
            );
        let mut checks = Checks::new();
        let worldgen = builder.freeze(&mut checks, terrain);
        checks.finish().expect("the test app builds");
        worldgen.expect("frozen")
    }
}

fn key(x: i64, y: i64, z: i64) -> ChunkKey {
    ChunkKey::new(ChunkPos::new(x, y, z))
}

/// The diff of opening `live` against the record `saved` made.
fn diff(saved: App, live: App) -> WorldgenDiff {
    let record = saved
        .freeze()
        .open(&id("test:default"), SEED, &registry(), None)
        .expect("opens")
        .record;
    live.freeze()
        .open(&id("test:default"), SEED, &registry(), Some(&record))
        .expect("opens")
        .diff
        .expect("compared")
}

/// `registry id` of each line.
fn lines(lines: &[boxel_worldgen::registry::DigestEntry]) -> Vec<String> {
    lines
        .iter()
        .map(|e| format!("{} {} in {}", e.registry, e.id, e.scope))
        .collect()
}

#[test]
fn a_region_world_opens_through_its_factory() {
    let worldgen = app().freeze();
    let opened = worldgen
        .open(&id("test:default"), SEED, &registry(), None)
        .expect("opens");
    let generator = opened.generator("test:overworld").expect("a dimension");
    // The surface realm's chunks are those of the region generator built by hand from the same
    // registries and seed.
    let mut checks = Checks::new();
    let by_hand = RegionGenerator::build(
        RegionSpec {
            seed: SEED,
            dimension: &id("test:overworld"),
            realm: &id("test:surface"),
            realms: worldgen.realms(),
            rules: &worldgen.registries().rules,
            entries: &worldgen.registries().entries,
            kinds: &worldgen.registries().kinds,
            blocks: &registry(),
            decoration: decoration(),
            settings: RegionSettings::default(),
        },
        |s| TestContext::new(s, BASE),
        &mut checks,
    )
    .expect("builds");
    for k in [key(0, 0, 0), key(-30, 0, 17), key(5, -1, 9), key(5, 3, 9)] {
        assert_eq!(
            generator.generate(k, &registry()),
            by_hand.generate(k, &registry()),
            "{k:?}"
        );
    }
    assert_eq!(
        generator
            .generate(key(0, -200, 0), &registry())
            .as_uniform(),
        Some(BlockState::AIR),
        "the deep realm's plain generator"
    );
    // The record: the realms bottom first with their spans, the spec, and the lines of the
    // dimension, its realms, the region map and the deep realm's generator.
    let record = &opened.record;
    let dim = record.dimension("test:overworld").expect("recorded");
    assert_eq!(
        dim.realms,
        [
            (id("test:deep"), RealmSpan::below(-4096)),
            (id("test:surface"), RealmSpan::above(-4096)),
        ]
    );
    assert_eq!(dim.generator.kind.as_str(), "test:regions");
    let all = lines(dim.digest.entries());
    for line in [
        "dimension test:overworld in dimension test:overworld",
        "generator_kind test:regions in dimension test:overworld",
        "realm test:deep in realm test:deep of dimension test:overworld",
        "realm_generator test:deep in realm test:deep of dimension test:overworld",
        "realm test:surface in realm test:surface of dimension test:overworld",
        "region test:surface in realm test:surface of dimension test:overworld",
        "selector_rule test:land in realm test:surface of dimension test:overworld",
        "selector_entry test:mod_entry in realm test:surface of dimension test:overworld",
        "terrain_kind test:hills in realm test:surface of dimension test:overworld",
    ] {
        assert!(all.contains(&line.to_string()), "{line} not in {all:#?}");
    }
    assert_eq!(
        generator.version(),
        dim.digest.version(),
        "the generator's version is its record's"
    );
    // The same registries again: nothing changed.
    let again = worldgen
        .open(&id("test:default"), SEED, &registry(), Some(record))
        .expect("opens");
    assert!(again.diff.expect("compared").is_empty());
    assert_eq!(again.record, *record);
}

#[test]
fn a_changed_entry_version_is_one_changed_line() {
    let mut bumped = app();
    bumped.rules[1].1.version = 2;
    let diff = diff(app(), bumped);
    assert!(diff.added.is_empty() && diff.removed.is_empty(), "{diff}");
    let changed: Vec<String> = diff
        .changed
        .iter()
        .map(|c| {
            format!(
                "{} {} {} -> {}",
                c.live.registry, c.live.id, c.saved.version, c.live.version
            )
        })
        .collect();
    assert_eq!(changed, ["selector_rule test:land 1 -> 2"]);
}

#[test]
fn an_added_realm_is_added_lines_and_the_span_it_cuts_changed() {
    let mut with_sky = app();
    with_sky.realms[1].1.span = RealmSpan::between(-4096, 12288);
    with_sky.realms.push((
        "test:sky",
        RealmSpec {
            span: RealmSpan::above(12288),
            lod: RealmLod::Off,
            ..realm()
        },
    ));
    with_sky.dimension.push("test:sky");
    let diff = diff(app(), with_sky);
    assert_eq!(
        lines(&diff.added),
        [
            "realm test:sky in realm test:sky of dimension test:overworld",
            "realm_generator test:sky in realm test:sky of dimension test:overworld",
        ]
    );
    assert!(diff.removed.is_empty(), "{diff}");
    let changed: Vec<String> = diff
        .changed
        .iter()
        .map(|c| format!("{} {}", c.live.registry, c.live.id))
        .collect();
    assert_eq!(
        changed,
        ["preset test:default", "realm test:surface"],
        "the preset's realm layout and the span the new realm cuts"
    );
}

#[test]
fn a_removed_entry_is_a_removed_line() {
    let mut without = app();
    without.entries.clear();
    let diff = diff(app(), without);
    assert_eq!(
        lines(&diff.removed),
        ["selector_entry test:mod_entry in realm test:surface of dimension test:overworld"]
    );
    assert!(diff.added.is_empty() && diff.changed.is_empty(), "{diff}");
    let text = diff.to_string();
    assert!(
        text.contains("removed selector_entry test:mod_entry in realm test:surface"),
        "{text}"
    );
}

#[test]
fn a_typo_in_a_block_name_fails_at_world_open_with_every_check() {
    let mut typo = app();
    typo.realms[1].1.floor = "test:sold".into();
    typo.surface = TopLayers::new(vec![("test:glas".into(), 1)]);
    // And a region map problem of its own: a Point rule with two entries.
    typo.rules[0]
        .1
        .entries
        .push(Entry::new(id("test:hills"), 1));
    let error = typo
        .freeze()
        .open(&id("test:default"), SEED, &registry(), None)
        .expect_err("refused");
    let message = error.to_string();
    for needle in [
        "test:surface names the block \"test:sold\", which is not in the block registry",
        "test:top names the block \"test:glas\", which is not in the block registry",
        "selector rule test:basins has 2 entries (test:basin and test:hills); a Point rule \
         needs exactly one",
    ] {
        assert!(message.contains(needle), "{needle:?} not in {message}");
    }
    assert_eq!(error.problems().len(), 3, "each problem once: {message}");
}

#[test]
fn a_factory_must_build_the_realms_its_preset_lists() {
    let mut builder = WorldgenBuilder::<()>::new();
    for (name, spec) in realms() {
        builder.realm(id(name), spec);
    }
    builder
        .dimension(id("test:overworld"), DimensionDef { version: 1 })
        .generator(
            id("test:plain"),
            GeneratorKind::new(1, |_, _| Some(Built::new(Arc::new(deep())))),
        )
        .preset(
            id("test:default"),
            WorldPreset::one(
                PresetDimension::new(
                    id("test:overworld"),
                    GeneratorSpec::new(id("test:plain"), ()),
                )
                .with_realms([id("test:deep"), id("test:surface")]),
            ),
        );
    let mut checks = Checks::new();
    let worldgen = builder.freeze(&mut checks, ()).expect("frozen");
    let message = worldgen
        .open(&id("test:default"), SEED, &registry(), None)
        .expect_err("another layout")
        .to_string();
    assert!(
        message.contains(
            "generator kind test:plain built dimension test:overworld with the realms \
             [boxel:default], not its preset's [test:deep, test:surface]"
        ),
        "{message}"
    );
}

#[test]
fn one_dimension_has_the_realms_of_each_preset() {
    // The test app, with two more presets of the same dimension: one realm of its own over all
    // of y, and a legacy factory whose generator declares its realms.
    let surface = app().surface;
    let world = world_of(realms(), rules(CELLS), Vec::new(), kind_entries());
    let terrain = Terrain {
        rules: world.rules,
        entries: world.entries,
        kinds: world.kinds,
    };
    let mut builder = WorldgenBuilder::new();
    for (name, spec) in realms() {
        builder.realm(id(name), spec);
    }
    let regions_spec = || GeneratorSpec::new(id("test:regions"), 8u32);
    builder
        .realm(id("test:whole"), realm())
        .dimension(id("test:overworld"), DimensionDef { version: 1 })
        .generator(
            id("test:regions"),
            GeneratorKind::new(1, move |build, checks| regions(build, checks, &surface)),
        )
        .generator(
            id("test:legacy"),
            GeneratorKind::legacy(1, |_| Arc::new(deep())),
        )
        .preset(
            id("test:default"),
            WorldPreset::one(
                PresetDimension::new(id("test:overworld"), regions_spec())
                    .with_realms([id("test:surface"), id("test:deep")]),
            ),
        )
        .preset(
            id("test:whole"),
            WorldPreset::one(
                PresetDimension::new(id("test:overworld"), regions_spec())
                    .with_realms([id("test:whole")]),
            ),
        )
        .preset(
            id("test:legacy"),
            WorldPreset::one(PresetDimension::new(
                id("test:overworld"),
                GeneratorSpec::new(id("test:legacy"), ()),
            )),
        );
    let mut checks = Checks::new();
    let worldgen = builder.freeze(&mut checks, terrain).expect("frozen");
    checks.finish().expect("the app builds");
    let realms_of = |preset: &str| -> Vec<String> {
        let opened = worldgen
            .open(&id(preset), SEED, &registry(), None)
            .unwrap_or_else(|e| panic!("{preset}: {e}"));
        assert_eq!(opened.record.names, [id("test:overworld")], "{preset}");
        let realms = opened
            .first()
            .generator
            .realms()
            .iter()
            .map(|(id, _)| id.to_string())
            .collect::<Vec<_>>();
        let recorded = opened.record.dimensions[0]
            .realms
            .iter()
            .map(|(id, _)| id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            realms, recorded,
            "{preset}: the record lists the generator's realms"
        );
        realms
    };
    assert_eq!(realms_of("test:default"), ["test:deep", "test:surface"]);
    assert_eq!(realms_of("test:whole"), ["test:whole"]);
    assert_eq!(realms_of("test:legacy"), ["boxel:default"]);
    // The realm lines of the region layout are its own; the legacy world has none.
    let record = |preset: &str| {
        worldgen
            .open(&id(preset), SEED, &registry(), None)
            .expect("opens")
            .record
    };
    let all = lines(record("test:whole").dimensions[0].digest.entries());
    assert!(
        all.contains(&"realm test:whole in realm test:whole of dimension test:overworld".into()),
        "{all:#?}"
    );
    assert!(
        record("test:legacy").dimensions[0]
            .digest
            .entries()
            .iter()
            .all(|e| e.scope.realm.is_none()),
        "a legacy generator's realms are its own, with no realm lines"
    );
}
