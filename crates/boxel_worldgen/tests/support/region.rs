//! Test content of the region generator (#119), shared by `tests/region.rs` and
//! `benches/region.rs`: a `test:` Context with a base height, two parameters and a warp, test
//! kinds that add relief to that base, the registries of a test world, and the hand-fused
//! single-kind twin the bench compares against.
#![allow(dead_code, reason = "each user takes part of it")]

use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SIZE, ChunkData, ChunkKey,
};
use boxel_worldgen::field::{Add, Field, LatticeField, NoiseField, Rect, Span};
use boxel_worldgen::noise::{Basis, Fbm, Frequency, Warp, mix_seed};
use boxel_worldgen::realm::{FluidRule, RealmForm, RealmLod, RealmSky, RealmSpan, RealmSpec};
use boxel_worldgen::region::{
    ChunkColumns, Context, Decoration, Entry, FillStage, FloorFill, HeightQ, Interval, KindGrid,
    KindLimits, RegionGenerator, RegionSettings, RegionSpec, Rock, RuleEntry, Scope, SelectorRule,
    Stages, SurfaceStage, TerrainKind, TopLayers, block_of, decorate,
};
use boxel_worldgen::registry::{
    Checks, FreezeError, GenEntry, GenId, GenRegistry, GenRegistryBuilder, HashParams, ParamsHasher,
};
use std::sync::Arc;

/// The id `name`.
pub fn id(name: &str) -> GenId {
    GenId::new(name).expect(name)
}

/// The test blocks.
pub fn registry() -> BlockRegistry {
    boxel_voxel::testing::registry()
}

/// The world seed of the tests.
pub const SEED: u64 = 0x5eed_0119;

const SALT_BASE: u64 = 0x7465_7374_0001;
const SALT_WET: u64 = 0x7465_7374_0002;
const SALT_WARP: u64 = 0x7465_7374_0003;

/// `octaves` octaves of gradient noise with a first wavelength of `wavelength` blocks.
pub const fn fbm(wavelength: u32, octaves: u32) -> Fbm {
    Fbm {
        frequency: Frequency::one_in(wavelength),
        octaves,
        lacunarity: 2,
        gain: 0.5,
    }
}

/// The base height of the test Context: gradient noise of `+-amplitude` 1/256 blocks on a
/// 16-block lattice.
pub type Base = LatticeField<NoiseField>;

/// The test Context: a base height every kind adds its relief to, the parameters `test:wet`
/// (gradient noise in `-100..=100` at lattice points) and `test:flat` (always 0), and a large warp
/// of 256 blocks.
pub struct TestContext {
    base: Base,
    wet: NoiseField,
    warp: Warp,
    params: Vec<GenId>,
    amplitude: i64,
}

impl TestContext {
    /// The Context of the realm seed `seed`, with a base of `+-amplitude` 1/256 blocks.
    pub fn new(seed: u64, amplitude: i64) -> Self {
        Self {
            base: LatticeField::new(
                4,
                NoiseField::new(
                    mix_seed(seed, SALT_BASE),
                    fbm(1024, 3),
                    Basis::Gradient,
                    amplitude,
                ),
            ),
            wet: NoiseField::new(mix_seed(seed, SALT_WET), fbm(2048, 2), Basis::Gradient, 100),
            warp: Warp::new(mix_seed(seed, SALT_WARP), fbm(4096, 2), 256),
            params: vec![id("test:wet"), id("test:flat")],
            amplitude,
        }
    }

    /// The base height.
    pub const fn base(&self) -> &Base {
        &self.base
    }
}

impl HashParams for TestContext {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.amplitude).write(&self.params);
    }
}

/// The base height over a grid.
pub struct TestGrid {
    /// Per point, `i * zs.len() + j`.
    pub base: Vec<i64>,
}

impl Context for TestContext {
    type Grid = TestGrid;

    fn version(&self) -> u32 {
        1
    }
    fn params(&self) -> &[GenId] {
        &self.params
    }
    fn param(&self, param: usize, x: i64, z: i64) -> i32 {
        match param {
            0 => self.wet.value(x, z) as i32,
            _ => 0,
        }
    }
    fn warp(&self, x: i64, z: i64) -> (i64, i64) {
        self.warp.offset(x, z)
    }
    fn grid(&self, xs: &[i64], zs: &[i64]) -> TestGrid {
        let mut base = vec![0; xs.len() * zs.len()];
        self.base.grid(xs, zs, &mut base);
        TestGrid { base }
    }
}

/// A test kind: the Context's base plus `offset` and gradient noise of `+-amplitude`, all in
/// 1/256 blocks. Its relief lies in `offset +- amplitude` everywhere, the blend band included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Relief {
    /// The middle of its relief.
    pub offset: i64,
    /// The noise's amplitude.
    pub amplitude: i64,
    /// The noise's first wavelength, in blocks.
    pub wavelength: u32,
    /// The noise's octaves.
    pub octaves: u32,
}

impl Relief {
    /// `offset +- amplitude` blocks of noise of `wavelength` blocks and `octaves` octaves.
    pub const fn blocks(offset: i64, amplitude: i64, wavelength: u32, octaves: u32) -> Self {
        Self {
            offset: offset << 8,
            amplitude: amplitude << 8,
            wavelength,
            octaves,
        }
    }

    /// Its noise under `seed`.
    fn noise(&self, seed: u64) -> NoiseField {
        NoiseField::new(
            seed,
            fbm(self.wavelength, self.octaves),
            Basis::Gradient,
            self.amplitude,
        )
    }

    /// Its relief span.
    fn relief(&self) -> Span {
        Span::new(self.offset - self.amplitude, self.offset + self.amplitude)
    }
}

impl HashParams for Relief {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.offset)
            .write(&self.amplitude)
            .write(&self.wavelength)
            .write(&self.octaves);
    }
}

impl GenEntry for Relief {
    fn version(&self) -> u32 {
        1
    }
}

impl TerrainKind<TestContext> for Relief {
    fn limits(&self, cx: &TestContext) -> KindLimits {
        KindLimits {
            slope: cx.base.slope().sum(self.noise(0).slope()),
            relief: self.relief(),
            band: self.relief(),
        }
    }

    fn range(&self, cx: &TestContext) -> Span {
        let (base, relief) = (cx.base.range(), self.relief());
        Span::new(base.lo + relief.lo, base.hi + relief.hi)
    }

    fn bounds(&self, cx: &TestContext, seed: u64, (x, z): (i64, i64), reach: i64) -> Span {
        let rect = Rect::new(x - reach, z - reach, x + reach, z + reach);
        let b = Add::new(cx.base.clone(), self.noise(seed)).bound(rect);
        Span::new(b.lo + self.offset, b.hi + self.offset)
    }

    fn heights(&self, grid: &KindGrid<'_, TestContext>, _mask: &[bool], out: &mut [HeightQ]) {
        let mut noise = vec![0; grid.len()];
        self.noise(grid.seed).grid(grid.xs, grid.zs, &mut noise);
        for ((o, &b), n) in out.iter_mut().zip(&grid.fields.base).zip(noise) {
            *o = (b + self.offset + n) as HeightQ;
        }
    }
}

/// The test kinds: low plains, high hills and a sunken basin under the sea level (0).
pub fn kinds() -> Vec<(&'static str, Relief)> {
    vec![
        ("test:plains", Relief::blocks(6, 4, 512, 2)),
        ("test:hills", Relief::blocks(200, 60, 256, 2)),
        ("test:basin", Relief::blocks(-30, 10, 512, 2)),
    ]
}

/// The realm of the test world: every `y`, datum 0, a sea of `test:fluid` up to y 0, on
/// `test:solid`.
pub fn realm() -> RealmSpec {
    RealmSpec {
        span: RealmSpan::ALL,
        form: RealmForm::Ground,
        datum: 0,
        fluid: FluidRule::SeaLevel {
            level: 0,
            fluid: "test:fluid".into(),
        },
        floor: "test:solid".into(),
        sky: RealmSky::Open,
        lod: RealmLod::Draw,
        env: id("test:env"),
        version: 1,
    }
}

/// The decoration of the test world: the floor under one `test:cutout` and two `test:glass`.
pub fn decoration() -> Decoration<TestContext> {
    Decoration::new(
        (id("test:fill"), Arc::new(FloorFill)),
        (
            id("test:surface"),
            Arc::new(TopLayers::new(vec![
                ("test:cutout".into(), 1),
                ("test:glass".into(), 2),
            ])),
        ),
    )
}

/// The rules of the test world's realm `test:surface`: basins where `test:wet` is at least 60
/// (a Point rule), else plains or hills by region cells of `2^cells` blocks.
pub fn rules(cells: u32) -> Vec<(&'static str, SelectorRule)> {
    let rule = |priority, condition, scope, entries| SelectorRule {
        realm: id("test:surface"),
        priority,
        condition,
        scope,
        entries,
        version: 1,
    };
    vec![
        (
            "test:basins",
            rule(
                10,
                vec![Interval::new(id("test:wet"), 60, 100)],
                Scope::Point,
                vec![Entry::new(id("test:basin"), 1)],
            ),
        ),
        (
            "test:land",
            rule(
                0,
                Vec::new(),
                Scope::Cell(cells),
                vec![
                    Entry::new(id("test:plains"), 3),
                    Entry::new(id("test:hills"), 2),
                ],
            ),
        ),
    ]
}

/// The frozen registries of a test world.
pub struct TestWorld {
    /// The realms.
    pub realms: GenRegistry<RealmSpec>,
    /// The rules.
    pub rules: GenRegistry<SelectorRule>,
    /// The added entries.
    pub entries: GenRegistry<RuleEntry>,
    /// The kinds.
    pub kinds: GenRegistry<Arc<dyn TerrainKind<TestContext>>>,
}

/// The registries of `realms`, `rules`, `entries` and `kinds`, frozen.
pub fn world_of(
    realms: Vec<(&str, RealmSpec)>,
    rules: Vec<(&str, SelectorRule)>,
    entries: Vec<(&str, RuleEntry)>,
    kinds: Vec<(&str, Arc<dyn TerrainKind<TestContext>>)>,
) -> TestWorld {
    let mut checks = Checks::new();
    let mut r = GenRegistryBuilder::new("realm");
    for (name, spec) in realms {
        r.register(id(name), spec);
    }
    let mut s = GenRegistryBuilder::new("selector_rule");
    for (name, rule) in rules {
        s.register(id(name), rule);
    }
    let mut e = GenRegistryBuilder::new("selector_entry");
    for (name, entry) in entries {
        e.register(id(name), entry);
    }
    let mut k = GenRegistryBuilder::new("terrain_kind");
    for (name, kind) in kinds {
        k.register(id(name), kind);
    }
    let world = TestWorld {
        realms: checks.freeze(r).expect("realms"),
        rules: checks.freeze(s).expect("rules"),
        entries: checks.freeze(e).expect("entries"),
        kinds: checks.freeze(k).expect("kinds"),
    };
    checks.finish().expect("the test app builds");
    world
}

/// The test kinds as registry entries.
pub fn kind_entries() -> Vec<(&'static str, Arc<dyn TerrainKind<TestContext>>)> {
    kinds()
        .into_iter()
        .map(|(name, k)| (name, Arc::new(k) as Arc<dyn TerrainKind<TestContext>>))
        .collect()
}

/// The test world: realm `test:surface`, the test kinds, the rules of [`rules`] with cells of
/// `2^cells` blocks, no added entry.
pub fn world(cells: u32) -> TestWorld {
    world_of(
        vec![("test:surface", realm())],
        rules(cells),
        Vec::new(),
        kind_entries(),
    )
}

/// The base amplitude of the test Context: 12 blocks.
pub const BASE: i64 = 12 << 8;

/// The generator of `world`'s realm `test:surface` in dimension `test:overworld`.
pub fn try_generator(
    world: &TestWorld,
    seed: u64,
    settings: RegionSettings,
) -> Result<RegionGenerator<TestContext>, FreezeError> {
    let mut checks = Checks::new();
    let generator = RegionGenerator::build(
        RegionSpec {
            seed,
            dimension: &id("test:overworld"),
            realm: &id("test:surface"),
            realms: &world.realms,
            rules: &world.rules,
            entries: &world.entries,
            kinds: &world.kinds,
            blocks: &registry(),
            decoration: decoration(),
            settings,
        },
        |realm_seed| TestContext::new(realm_seed, BASE),
        &mut checks,
    );
    checks.finish()?;
    Ok(generator.expect("no problem"))
}

/// [`try_generator`], which must pass its checks.
pub fn generator(
    world: &TestWorld,
    seed: u64,
    settings: RegionSettings,
) -> RegionGenerator<TestContext> {
    try_generator(world, seed, settings).expect("the test world opens")
}

/// The FNV fingerprint of a chunk's states.
pub fn fingerprint(chunk: &ChunkData) -> u64 {
    let mut dense = vec![BlockState::AIR; boxel_voxel::CHUNK_VOLUME];
    chunk.to_dense(&mut dense);
    let bytes: Vec<u8> = dense.iter().flat_map(|s| s.0.to_le_bytes()).collect();
    boxel_worldgen::registry::fnv1a64(&bytes)
}

/// A hand-fused single-kind generator: the region generator of a map with one kind, written
/// without the selector, the memo or the blend. The same Context, kind code, bounds and
/// decoration, so the bench measures what the region machinery adds. It keeps the bounds of the
/// last lattice cell it saw (what a hand-written generator would cache), so it is not `Sync`.
pub struct Twin {
    cx: TestContext,
    kind: Arc<dyn TerrainKind<TestContext>>,
    seed: u64,
    spec: RealmSpec,
    fill: FloorFill,
    surface: TopLayers,
    shift: u32,
    /// The last lattice cell and its bounds.
    last: std::cell::Cell<Option<((i64, i64), (i64, i64))>>,
}

impl Twin {
    /// The twin of a test world whose only kind is `kind` (`name` in the registry), under the
    /// world seed `seed`, on a lattice of `2^shift`.
    pub fn new(name: &str, kind: Relief, seed: u64, shift: u32) -> Self {
        let realm_seed = id("test:surface").seed(seed);
        Self {
            cx: TestContext::new(realm_seed, BASE),
            kind: Arc::new(kind),
            seed: id(name).seed(realm_seed),
            spec: realm(),
            fill: FloorFill,
            surface: TopLayers::new(vec![("test:cutout".into(), 1), ("test:glass".into(), 2)]),
            shift,
            last: std::cell::Cell::new(None),
        }
    }

    /// The `y` of the top solid block of column `(x, z)`.
    pub fn ground(&self, x: i64, z: i64) -> i64 {
        let fields = self.cx.grid(&[x], &[z]);
        let grid = KindGrid {
            cx: &self.cx,
            fields: &fields,
            xs: &[x],
            zs: &[z],
            seed: self.seed,
        };
        let mut h = [0];
        self.kind.heights(&grid, &[true], &mut h);
        block_of(0, h[0])
    }

    /// The chunk at `key`.
    pub fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let origin: BlockPos = key.pos.origin();
        let s = self.shift;
        let cell = (origin.x >> s, origin.z >> s);
        let (lo, hi) = match self.last.get() {
            Some((c, bounds)) if c == cell => bounds,
            _ => {
                let (mut lo, mut hi) = (i64::MAX, i64::MIN);
                for a in 0..4 {
                    for b in 0..4 {
                        let q = ((cell.0 - 1 + a) << s, (cell.1 - 1 + b) << s);
                        let bound = self.kind.bounds(&self.cx, self.seed, q, 2 << s);
                        lo = lo.min(bound.lo);
                        hi = hi.max(bound.hi);
                    }
                }
                self.last.set(Some((cell, (lo, hi))));
                (lo, hi)
            }
        };
        let (ground_lo, ground_hi) = (block_of(0, lo as HeightQ), block_of(0, hi as HeightQ));
        let level = 0;
        let top = if ground_lo < level {
            ground_hi.max(level)
        } else {
            ground_hi
        };
        if origin.y > top {
            return ChunkData::uniform(BlockState::AIR);
        }
        let solid = registry.by_name("test:solid").expect("test:solid");
        if origin.y + i64::from(CHUNK_SIZE) - 1 <= ground_lo - 3 {
            let mut out = Vec::new();
            return match FillStage::<TestContext>::rock(
                &self.fill, &self.cx, origin, solid, registry, &mut out,
            ) {
                Rock::Uniform(state) => ChunkData::uniform(state),
                Rock::Dense => ChunkData::from_dense(&out),
            };
        }
        let xs: [i64; 32] = std::array::from_fn(|i| origin.x + i as i64);
        let zs: [i64; 32] = std::array::from_fn(|i| origin.z + i as i64);
        let fields = self.cx.grid(&xs, &zs);
        let mut h = [0; CHUNK_AREA];
        let grid = KindGrid {
            cx: &self.cx,
            fields: &fields,
            xs: &xs,
            zs: &zs,
            seed: self.seed,
        };
        self.kind.heights(&grid, &[true; CHUNK_AREA], &mut h);
        let ground = h.map(|h| block_of(0, h));
        let fluid_top = ground.map(|g| if g < level { level } else { g });
        let stages = Stages {
            fill: &self.fill as &dyn FillStage<TestContext>,
            surface: &self.surface as &dyn SurfaceStage<TestContext>,
            floor: solid,
            fluid: registry.by_name("test:fluid").expect("test:fluid"),
        };
        let columns = ChunkColumns {
            origin,
            ground: &ground,
            fluid_top: &fluid_top,
            cx: &self.cx,
            fields: &fields,
        };
        decorate(&columns, &stages, registry)
    }
}
