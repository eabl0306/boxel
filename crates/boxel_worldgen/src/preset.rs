//! Presets, dimensions and generator kinds (ADR 0039 sec. 1 and 17): what a world is made with,
//! the factories that build its generators every time it opens, the record a world saves of them
//! ([`WorldgenRecord`]) and what changed since ([`WorldgenDiff`]).
//!
//! - A [`WorldPreset`] (`game:default`, `game:large_biomes`) is chosen when a world is made and
//!   saved by id. It maps each of its dimensions to a [`GeneratorSpec`]: the id of a
//!   [`GeneratorKind`] and its typed parameters.
//! - A [`DimensionDef`] lists the realms of a dimension, bottom or top first (none: its
//!   generator declares its own, as every generator from before realms).
//! - A [`GeneratorKind`] is a factory, not an instance: it builds a dimension's generator from
//!   the world's seed, the spec's parameters and the app's frozen registries ([`Build`]), every
//!   time a world opens, so a world reopens from its metadata and never shares a generator (or
//!   its per-worker memos) with the world before. [`GeneratorKind::legacy`] wraps a generator
//!   made from the seed alone, the way worlds were made before presets: a world saved without a
//!   record reopens through it unchanged.
//! - [`WorldgenBuilder`] gathers them while the app is built (the game's plugins and mods), and
//!   freezes them before any world opens (ADR 0034 sec. 1) into a [`Worldgen`], next to the
//!   game's own frozen registries (`R`: its selector rules, terrain kinds...).
//!
//! [`Worldgen::open`] resolves a preset at world open: every id it reaches, every freeze check
//! (the realm layouts and floors, every block name a realm uses, and whatever each factory
//! checks: a region generator runs the checks of its rules and kinds), then each factory. A bad
//! mix fails there, with every problem and its ids in one [`FreezeError`], never in a worker in
//! the middle of a game. It gives the generators, the record a new world saves under the meta
//! key `boxel:worldgen`, and, for a world that saved one, the diff against it. The policy for a
//! diff is the game's (the engine only reports it).

use crate::WorldGenerator;
use crate::realm::{RealmSpec, RealmStack};
use crate::region::{DimensionGenerator, check_realm_blocks};
use crate::registry::{
    Checks, DigestBuilder, DigestEntry, DigestScope, FreezeError, GenEntry, GenId, GenIx,
    GenRegistry, GenRegistryBuilder, HashParams, ParamsHasher, Problem, RegistryDigest,
    WorldgenDiff, hash_params,
};
use boxel_voxel::{BlockRegistry, DimensionId};
use std::any::Any;
use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

mod record;

pub use record::{DimensionRecord, MAX_DIMENSIONS, SpecLine, WorldgenRecord};

/// The name of the presets' registry, in digest lines and messages.
pub const PRESET_REGISTRY: &str = "preset";
/// The name of the dimensions' registry.
pub const DIMENSION_REGISTRY: &str = "dimension";
/// The name of the realms' registry.
pub const REALM_REGISTRY: &str = "realm";
/// The name of the generator kinds' registry: a dimension's spec line is listed under it.
pub const GENERATOR_REGISTRY: &str = "generator_kind";

/// The typed parameters of a [`GeneratorSpec`]: what its factory reads ([`Build::params`]),
/// hashed into the dimension's spec line, so a changed parameter shows in the diff and in the
/// dimension's version. Any plain type with a fixed [`HashParams`] encoding is one (`()` for
/// none).
pub trait GeneratorParams: HashParams + fmt::Debug + Send + Sync + 'static {
    /// The parameters as `Any`, for [`GeneratorSpec::params`].
    fn as_any(&self) -> &dyn Any;
}

impl<T: HashParams + fmt::Debug + Send + Sync + 'static> GeneratorParams for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The generator of a dimension in a preset: a [`GeneratorKind`]'s id and the parameters its
/// factory reads.
#[derive(Clone, Debug)]
pub struct GeneratorSpec {
    /// The generator kind.
    pub kind: GenId,
    /// Its parameters.
    pub params: Arc<dyn GeneratorParams>,
}

impl GeneratorSpec {
    /// Kind `kind` with `params`.
    pub fn new(kind: GenId, params: impl GeneratorParams) -> Self {
        Self {
            kind,
            params: Arc::new(params),
        }
    }

    /// The parameters as a `P`, `None` if they are of another type.
    pub fn params<P: 'static>(&self) -> Option<&P> {
        GeneratorParams::as_any(&*self.params).downcast_ref()
    }

    /// The hash of the parameters ([`HashParams`]).
    pub fn params_hash(&self) -> u64 {
        hash_params(&*self.params)
    }
}

/// A preset (ADR 0039 sec. 1), registered under its id: the dimensions of a world made with it,
/// each with the spec of its generator.
///
/// A preset holds nothing else that changes the output: whatever changes chunks lives on its
/// dimensions' entries and specs, whose lines are in each dimension's digest, so
/// [`RegistryDigest::dimension`] (which leaves the preset's own line out) stays a sound
/// generator version.
#[derive(Clone, Debug)]
pub struct WorldPreset {
    /// Its dimensions, each once, with its generator's spec. The first is where a world made
    /// with it starts: number 0 (`DimensionId(0)`) in a new world.
    pub dimensions: Vec<(GenId, GeneratorSpec)>,
    /// The version of its entry.
    pub version: u32,
}

impl WorldPreset {
    /// A preset of one dimension, `dimension`, generated by `spec`, version 1.
    pub fn one(dimension: GenId, spec: GeneratorSpec) -> Self {
        Self {
            dimensions: vec![(dimension, spec)],
            version: 1,
        }
    }
}

/// Its dimensions and their generator kinds: the specs' parameters are in each dimension's
/// spec line.
impl HashParams for WorldPreset {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.dimensions.len());
        for (dimension, spec) in &self.dimensions {
            h.write(dimension).write(&spec.kind);
        }
    }
}

impl GenEntry for WorldPreset {
    fn version(&self) -> u32 {
        self.version
    }
}

/// A dimension (ADR 0039 sec. 1), registered under its id (`game:overworld`).
///
/// The ADR's `DimensionDef { id, factory, realms }`: the id is its registry key, and the
/// factory is the generator kind its preset's spec names, so one dimension can be generated by
/// different kinds in different presets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct DimensionDef {
    /// Its realms, ids in the realms' registry, in any order (they sort by `y`). Checked at world
    /// open (layout, floors, block names) and handed to the factory as a [`RealmStack`]. Empty
    /// for a generator that declares its realms itself (one from before realms: the default
    /// [`RealmStack::single`]).
    pub realms: Vec<GenId>,
    /// The version of its entry.
    pub version: u32,
}

impl HashParams for DimensionDef {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.realms);
    }
}

impl GenEntry for DimensionDef {
    fn version(&self) -> u32 {
        self.version
    }
}

/// The factory of a [`GeneratorKind`].
type Factory<R> = dyn Fn(&Build<'_, R>, &mut Checks) -> Option<Built> + Send + Sync;

/// A kind of generator (ADR 0039 sec. 1), registered under its id: the factory that builds a
/// dimension's generator at world open from the world's seed, its spec's parameters and the
/// app's frozen registries ([`Build`]). A new generator every open: per-worker memos keyed by
/// the build counter never mix two worlds.
///
/// The factory records what is wrong in the [`Checks`] it gets and returns `None` (a region
/// generator's checks: [`RegionGenerator::build`](crate::region::RegionGenerator::build)), so a
/// bad mix fails at world open with its ids. Its version is the version of its code: bump it
/// when the same parameters give other chunks. It has no parameters of its own (the specs
/// carry them).
pub struct GeneratorKind<R = ()> {
    factory: Box<Factory<R>>,
    version: u32,
}

impl<R> GeneratorKind<R> {
    /// The kind of factory `factory`, version `version`.
    pub fn new(
        version: u32,
        factory: impl Fn(&Build<'_, R>, &mut Checks) -> Option<Built> + Send + Sync + 'static,
    ) -> Self {
        Self {
            factory: Box::new(factory),
            version,
        }
    }

    /// A legacy factory (ADR 0039 sec. 17): `make` builds the generator from the world's seed
    /// alone, the way worlds were made before presets, so a world saved without a record reopens
    /// through it with the same generator and the same chunks. It reads no parameters and
    /// checks nothing; its digest lines are its dimension's own (the dimension and the spec).
    pub fn legacy(
        version: u32,
        make: impl Fn(u64) -> Arc<dyn WorldGenerator> + Send + Sync + 'static,
    ) -> Self {
        Self::new(version, move |build, _| Some(Built::new(make(build.seed))))
    }

    /// Builds the generator of `build` (the factory).
    pub fn build(&self, build: &Build<'_, R>, checks: &mut Checks) -> Option<Built> {
        (self.factory)(build, checks)
    }
}

impl<R> fmt::Debug for GeneratorKind<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratorKind")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// No parameters: a kind's are its specs'.
impl<R> HashParams for GeneratorKind<R> {
    fn hash_params(&self, _: &mut ParamsHasher) {}
}

impl<R> GenEntry for GeneratorKind<R> {
    fn version(&self) -> u32 {
        self.version
    }
}

/// What a [`GeneratorKind`]'s factory builds a dimension's generator from, at world open.
pub struct Build<'a, R = ()> {
    /// The world's seed.
    pub seed: u64,
    /// The dimension.
    pub dimension: &'a GenId,
    /// Its definition.
    pub def: &'a DimensionDef,
    /// Its realms, built and checked from [`DimensionDef::realms`]; `None` when the definition
    /// lists none (the generator declares its own).
    pub stack: Option<&'a RealmStack>,
    /// The spec the preset gives the dimension: the kind's id and its parameters.
    pub spec: &'a GeneratorSpec,
    /// The app's frozen registries (with the game's own, [`Worldgen::registries`]).
    pub worldgen: &'a Worldgen<R>,
    /// The blocks the world is generated with: every block name the generator uses must be in
    /// it ([`Build::check_blocks`]).
    pub blocks: &'a BlockRegistry,
    /// The dimension's own lines ([`Build::lines`]).
    lines: &'a [DigestEntry],
}

impl<'a, R> Build<'a, R> {
    /// The spec's parameters as a `P`, or `None` with a [`Problem::Params`] recorded when they
    /// are of another type (a preset that gives the kind someone else's parameters).
    pub fn params<P: 'static>(&self, checks: &mut Checks) -> Option<&'a P> {
        let params = self.spec.params::<P>();
        if params.is_none() {
            checks.push(Problem::Params {
                dimension: self.dimension.clone(),
                kind: self.spec.kind.clone(),
                expected: std::any::type_name::<P>(),
            });
        }
        params
    }

    /// The game's frozen registries.
    pub fn registries(&self) -> &'a R {
        &self.worldgen.registries
    }

    /// The dimension's own digest lines: its definition, its spec (the kind's id and version,
    /// the parameters' hash) and its realms' entries. A factory that builds a
    /// [`DimensionGenerator`] extends them with its realm generators' lines and hands them to
    /// it, so the generator's version follows them; the world's record lists them whatever the
    /// factory does.
    pub fn lines(&self) -> DigestBuilder {
        let mut lines = DigestBuilder::new();
        for line in self.lines {
            lines.push(line.clone());
        }
        lines
    }

    /// Whether `blocks` holds every name of `names`, with a [`Problem::UnknownBlock`] recorded
    /// for each it lacks, named by the spec's kind: a factory checks the block names its
    /// generator looks up, so a typo fails at world open.
    pub fn check_blocks<'n>(
        &self,
        checks: &mut Checks,
        names: impl IntoIterator<Item = &'n str>,
    ) -> bool {
        let mut all = true;
        for name in names {
            all &= checks.block(self.blocks, name, &self.spec.kind);
        }
        all
    }
}

/// What a factory built: the dimension's generator and the digest lines it brings (a region
/// dimension's realm lines; none for a legacy generator).
pub struct Built {
    /// The generator.
    pub generator: Arc<dyn WorldGenerator>,
    /// Its lines; only those of its dimension are kept.
    pub digest: RegistryDigest,
}

impl Built {
    /// `generator`, with no line of its own.
    pub fn new(generator: Arc<dyn WorldGenerator>) -> Self {
        Self {
            generator,
            digest: RegistryDigest::from_entries(Vec::new()),
        }
    }

    /// `generator` with the lines of `digest`.
    pub fn with_digest(generator: Arc<dyn WorldGenerator>, digest: RegistryDigest) -> Self {
        Self { generator, digest }
    }
}

/// A dimension generator with its digest ([`DimensionGenerator::digest`]).
impl From<DimensionGenerator> for Built {
    fn from(dimension: DimensionGenerator) -> Self {
        let digest = dimension.digest().clone();
        Self::with_digest(Arc::new(dimension), digest)
    }
}

/// The registries of presets, dimensions, realms and generator kinds while the app is built:
/// plugins and mods register in any order, then [`WorldgenBuilder::freeze`] freezes them all.
pub struct WorldgenBuilder<R = ()> {
    presets: GenRegistryBuilder<WorldPreset>,
    dimensions: GenRegistryBuilder<DimensionDef>,
    realms: GenRegistryBuilder<RealmSpec>,
    generators: GenRegistryBuilder<GeneratorKind<R>>,
}

impl<R> Default for WorldgenBuilder<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> WorldgenBuilder<R> {
    /// Nothing registered yet.
    pub const fn new() -> Self {
        Self {
            presets: GenRegistryBuilder::new(PRESET_REGISTRY),
            dimensions: GenRegistryBuilder::new(DIMENSION_REGISTRY),
            realms: GenRegistryBuilder::new(REALM_REGISTRY),
            generators: GenRegistryBuilder::new(GENERATOR_REGISTRY),
        }
    }

    /// Adds preset `id`.
    pub fn preset(&mut self, id: GenId, preset: WorldPreset) -> &mut Self {
        self.presets.register(id, preset);
        self
    }

    /// Adds dimension `id`.
    pub fn dimension(&mut self, id: GenId, def: DimensionDef) -> &mut Self {
        self.dimensions.register(id, def);
        self
    }

    /// Adds realm `id`.
    pub fn realm(&mut self, id: GenId, spec: RealmSpec) -> &mut Self {
        self.realms.register(id, spec);
        self
    }

    /// Adds generator kind `id`.
    pub fn generator(&mut self, id: GenId, kind: GeneratorKind<R>) -> &mut Self {
        self.generators.register(id, kind);
        self
    }

    /// Freezes every registry next to the game's own frozen `registries`, or `None` with the
    /// problems recorded in `checks` (duplicates, a registry name frozen twice in `checks`). The
    /// game freezes its own registries in the same `checks` first, so one error reports them all.
    pub fn freeze(self, checks: &mut Checks, registries: R) -> Option<Worldgen<R>> {
        let Self {
            presets,
            dimensions,
            realms,
            generators,
        } = self;
        let presets = checks.freeze(presets);
        let dimensions = checks.freeze(dimensions);
        let realms = checks.freeze(realms);
        let generators = checks.freeze(generators);
        Some(Worldgen {
            presets: presets?,
            dimensions: dimensions?,
            realms: realms?,
            generators: generators?,
            registries,
        })
    }
}

/// The app's frozen presets, dimensions, realms and generator kinds, with the game's own frozen
/// registries `R` (ADR 0039 sec. 11): built once with the app ([`WorldgenBuilder::freeze`]),
/// never changed while it runs, and read by [`Worldgen::open`] at every world open.
pub struct Worldgen<R = ()> {
    presets: GenRegistry<WorldPreset>,
    dimensions: GenRegistry<DimensionDef>,
    realms: GenRegistry<RealmSpec>,
    generators: GenRegistry<GeneratorKind<R>>,
    registries: R,
}

impl<R> fmt::Debug for Worldgen<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn ids<T>(registry: &GenRegistry<T>) -> Vec<&str> {
            registry.iter().map(|(_, id, _)| id.as_str()).collect()
        }
        f.debug_struct("Worldgen")
            .field("presets", &ids(&self.presets))
            .field("dimensions", &ids(&self.dimensions))
            .field("realms", &ids(&self.realms))
            .field("generators", &ids(&self.generators))
            .finish_non_exhaustive()
    }
}

/// A dimension a world open built, before it is numbered ([`Worldgen::open`]).
struct Made {
    id: GenId,
    built: Built,
    /// Its own lines ([`Build::lines`]).
    lines: Vec<DigestEntry>,
    spec: SpecLine,
}

/// A dimension a world open built ([`Worldgen::open`]).
pub struct OpenedDimension {
    /// The dimension's id.
    pub id: GenId,
    /// Its number in chunk keys and save keys: its place in the world's name table.
    pub number: DimensionId,
    /// Its generator, new for this open.
    pub generator: Arc<dyn WorldGenerator>,
}

impl fmt::Debug for OpenedDimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenedDimension")
            .field("id", &self.id)
            .field("number", &self.number)
            .field("generator", &self.generator.name())
            .finish()
    }
}

/// What a world open gives ([`Worldgen::open`]): the generator of each dimension of the preset,
/// the record of how the world is generated now, and what changed since the record the world
/// saved.
#[derive(Debug)]
pub struct OpenedWorld {
    /// The preset's dimensions, in its order: the first is where a world made with it starts.
    pub dimensions: Vec<OpenedDimension>,
    /// The live record: what a new world saves under `boxel:worldgen`.
    pub record: WorldgenRecord,
    /// The saved record against the live one (`WorldgenDiff::between`), `None` for a world with
    /// no saved record (a new or a legacy one). The engine only reports it; the policy is the
    /// game's.
    pub diff: Option<WorldgenDiff>,
}

impl OpenedWorld {
    /// The generator of `dimension`, `None` if the preset has no such dimension.
    pub fn generator(&self, dimension: &str) -> Option<&Arc<dyn WorldGenerator>> {
        self.dimensions
            .iter()
            .find(|d| d.id.as_str() == dimension)
            .map(|d| &d.generator)
    }

    /// The preset's first dimension: where a world made with it starts.
    pub fn first(&self) -> &OpenedDimension {
        &self.dimensions[0]
    }
}

impl<R> Worldgen<R> {
    /// The presets.
    pub const fn presets(&self) -> &GenRegistry<WorldPreset> {
        &self.presets
    }

    /// The dimensions.
    pub const fn dimensions(&self) -> &GenRegistry<DimensionDef> {
        &self.dimensions
    }

    /// The realms.
    pub const fn realms(&self) -> &GenRegistry<RealmSpec> {
        &self.realms
    }

    /// The generator kinds.
    pub const fn generators(&self) -> &GenRegistry<GeneratorKind<R>> {
        &self.generators
    }

    /// The game's own frozen registries.
    pub const fn registries(&self) -> &R {
        &self.registries
    }

    /// Opens a world of `preset` with `seed` and the world's blocks `blocks` (ADR 0039 sec. 12
    /// and 17): resolves the preset, its dimensions, their specs' generator kinds and their
    /// realms; checks each dimension's realms (layout and floors, [`RealmStack::build`]) and
    /// every block name they use; then builds each dimension's generator with its kind's factory,
    /// which runs its own checks. Every problem is reported at once, in one [`FreezeError`] whose
    /// message names the ids, and no generator is returned then.
    ///
    /// `saved` is the record the world saved, if any: the dimensions keep their saved numbers
    /// (the name table only grows: a dimension new to the preset takes the next free number), and
    /// [`OpenedWorld::diff`] compares it with the live record. A world without a record (a new one,
    /// or a legacy one made before records) passes `None`; the store writes the live record only
    /// into a new world, so a legacy one stays legacy.
    pub fn open(
        &self,
        preset: &GenId,
        seed: u64,
        blocks: &BlockRegistry,
        saved: Option<&WorldgenRecord>,
    ) -> Result<OpenedWorld, FreezeError> {
        let mut checks = Checks::new();
        let Some(p) = checks.resolve(&self.presets, preset, None) else {
            return Err(FreezeError::new(checks.problems().to_vec()));
        };
        let preset_line = DigestEntry::of(&DigestScope::PRESET, &self.presets, p);
        let dims = &self.presets[p].dimensions;
        if dims.is_empty() {
            checks.push(Problem::Other {
                ids: vec![preset.clone()],
                message: format!("preset {preset} has no dimension"),
            });
        }
        let mut made = Vec::new();
        for (n, (dimension, spec)) in dims.iter().enumerate() {
            if dims[..n].iter().any(|(d, _)| d == dimension) {
                checks.push(Problem::Other {
                    ids: vec![preset.clone(), dimension.clone()],
                    message: format!("preset {preset} lists dimension {dimension} twice"),
                });
                continue;
            }
            let d = checks.resolve(&self.dimensions, dimension, Some(preset));
            let k = checks.resolve(&self.generators, &spec.kind, Some(preset));
            let (Some(d), Some(k)) = (d, k) else {
                continue;
            };
            made.extend(self.dimension(&mut checks, (dimension, d), (spec, k), seed, blocks));
        }
        let (names, numbers) = number(&mut checks, &made, saved);
        checks.finish()?;
        Ok(opened(preset_line, made, names, numbers, saved))
    }

    /// The generator of `dimension` (`d` in the dimensions' registry) by `spec` (its kind `k`),
    /// with its own lines, or `None` with the problems in `checks`.
    fn dimension(
        &self,
        checks: &mut Checks,
        (dimension, d): (&GenId, GenIx<DimensionDef>),
        (spec, k): (&GeneratorSpec, GenIx<GeneratorKind<R>>),
        seed: u64,
        blocks: &BlockRegistry,
    ) -> Option<Made> {
        let before = checks.found();
        let def = &self.dimensions[d];
        let scope = DigestScope::of_dimension(dimension);
        let spec_line = SpecLine {
            kind: spec.kind.clone(),
            version: self.generators.version(k),
            params: spec.params_hash(),
        };
        let mut lines = vec![
            DigestEntry::of(&scope, &self.dimensions, d),
            DigestEntry {
                scope: scope.clone(),
                registry: Cow::Borrowed(self.generators.name()),
                id: spec_line.kind.clone(),
                version: spec_line.version,
                params: spec_line.params,
            },
        ];
        let mut realms = Vec::with_capacity(def.realms.len());
        for realm in &def.realms {
            if let Some(r) = checks.resolve(&self.realms, realm, Some(dimension)) {
                check_realm_blocks(checks, blocks, realm, &self.realms[r]);
                lines.push(DigestEntry::of(
                    &DigestScope::of_realm(dimension, realm),
                    &self.realms,
                    r,
                ));
                realms.push((realm.clone(), self.realms[r].clone()));
            }
        }
        let stack = if def.realms.is_empty() {
            None
        } else if realms.len() < def.realms.len() {
            return None;
        } else {
            // A layout that does not build leaves the factory nothing to build on; a bad block
            // name does not stop it, so its own checks are reported with it.
            Some(RealmStack::build(checks, dimension, realms)?)
        };
        let build = Build {
            seed,
            dimension,
            def,
            stack: stack.as_ref(),
            spec,
            worldgen: self,
            blocks,
            lines: &lines,
        };
        let built = self.generators[k].build(&build, checks);
        let built = match built {
            Some(built) if checks.found() == before => built,
            Some(_) => return None,
            None => {
                if checks.found() == before {
                    checks.push(Problem::Other {
                        ids: vec![spec.kind.clone(), dimension.clone()],
                        message: format!(
                            "generator kind {} built nothing for dimension {dimension} and \
                             reported no problem",
                            spec.kind
                        ),
                    });
                }
                return None;
            }
        };
        if let Some(stack) = &stack {
            let ids = |stack: &RealmStack| -> Vec<String> {
                stack.iter().map(|(id, _)| id.to_string()).collect()
            };
            let (want, got) = (ids(stack), ids(built.generator.realms()));
            if want != got {
                checks.push(Problem::Other {
                    ids: vec![spec.kind.clone(), dimension.clone()],
                    message: format!(
                        "generator kind {} built dimension {dimension} with the realms [{}], not \
                         its definition's [{}]",
                        spec.kind,
                        got.join(", "),
                        want.join(", ")
                    ),
                });
                return None;
            }
        }
        Some(Made {
            id: dimension.clone(),
            built,
            lines,
            spec: spec_line,
        })
    }
}

/// The name table of the world of the dimensions `made` and their numbers in it: `saved`'s
/// table (none: an empty one), where a dimension it lacks takes the next number. A dimension
/// past the last number ([`MAX_DIMENSIONS`]) is a problem in `checks`, not a panic: a saved table
/// comes from the world's file.
fn number(
    checks: &mut Checks,
    made: &[Made],
    saved: Option<&WorldgenRecord>,
) -> (Vec<GenId>, Vec<DimensionId>) {
    let mut names: Vec<GenId> = saved.map_or_else(Vec::new, |s| s.names.clone());
    let mut numbers = Vec::with_capacity(made.len());
    for Made { id, .. } in made {
        let n = names.iter().position(|n| n == id).unwrap_or_else(|| {
            names.push(id.clone());
            names.len() - 1
        });
        match u16::try_from(n) {
            Ok(n) => numbers.push(DimensionId(n)),
            Err(_) => checks.push(Problem::Other {
                ids: vec![id.clone()],
                message: format!(
                    "dimension {id} gets no number: it is name {n} of the world's dimension \
                     table, which has {MAX_DIMENSIONS} numbers"
                ),
            }),
        }
    }
    (names, numbers)
}

/// The world of the dimensions `made` with the preset's line `preset`, numbered `numbers` in the
/// name table `names` ([`number`]), with its record and the diff against `saved`.
fn opened(
    preset: DigestEntry,
    made: Vec<Made>,
    names: Vec<GenId>,
    numbers: Vec<DimensionId>,
    saved: Option<&WorldgenRecord>,
) -> OpenedWorld {
    let mut dimensions = Vec::with_capacity(made.len());
    let mut records = Vec::with_capacity(made.len());
    for (
        Made {
            id,
            built,
            lines,
            spec,
        },
        number,
    ) in made.into_iter().zip(numbers)
    {
        let mut digest = DigestBuilder::new();
        for line in lines {
            digest.push(line);
        }
        digest.extend(&built.digest.dimension(&id));
        records.push(DimensionRecord {
            id: id.clone(),
            realms: built
                .generator
                .realms()
                .iter()
                .map(|(id, spec)| (id.clone(), spec.span))
                .collect(),
            generator: spec,
            digest: digest.finish(),
        });
        dimensions.push(OpenedDimension {
            id,
            number,
            generator: built.generator,
        });
    }
    let record = WorldgenRecord {
        preset,
        names,
        dimensions: records,
    };
    let diff = saved.map(|saved| saved.diff(&record));
    OpenedWorld {
        dimensions,
        record,
        diff,
    }
}

#[cfg(test)]
mod tests;
