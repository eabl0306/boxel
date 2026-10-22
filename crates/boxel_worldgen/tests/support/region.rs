//! Test content of the region generator (#119, #133, #134, #135), shared by `tests/region.rs`,
//! `tests/region_lod.rs`, `tests/decoration.rs`, `tests/features.rs` and the benches: a `test:`
//! Context with a base height, two parameters and a warp, test kinds that add relief to that base
//! (also with bounds per chunk column), valleys carved by a shape modifier, a carver of holes
//! under a roof and one with entrances in some regions only, trees whose canopies cross chunk
//! borders and plants (feature stages), the registries of a test world, and the hand-fused
//! single-kind twin the bench compares against.
#![allow(dead_code, reason = "each user takes part of it")]

use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SIZE, ChunkData, ChunkKey, Occupancy,
};
use boxel_worldgen::field::{Add, Field, LatticeField, NoiseField, Q16, Rect, Slope, Span};
use boxel_worldgen::noise::{Basis, Fbm, Frequency, Warp, mix_seed, splitmix64};
use boxel_worldgen::realm::{FluidRule, RealmForm, RealmLod, RealmSky, RealmSpan, RealmSpec};
use boxel_worldgen::region::{CarveChunk, CarveMask, CarveReach, CarverStage};
use boxel_worldgen::region::{CarveCorner, ShapeEffect, ShapeModifier, divide_tops};
use boxel_worldgen::region::{
    ChunkColumns, ColumnsAround, Context, Decoration, Entry, FillStage, FloorFill, HeightQ,
    Interval, KindGrid, KindLimits, RegionGenerator, RegionSettings, RegionSpec, Rock, RuleEntry,
    Scope, SelectorRule, Stages, SurfaceStage, TerrainKind, TopLayers, block_of, decorate,
};
use boxel_worldgen::region::{FeatureReach, FeatureStage, LodColumns};
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
    pub fn noise(&self, seed: u64) -> NoiseField {
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

/// The generator of `world`'s realm `test:surface` with `decoration`, which must pass its checks.
pub fn generator_with(
    world: &TestWorld,
    seed: u64,
    settings: RegionSettings,
    decoration: Decoration<TestContext>,
) -> RegionGenerator<TestContext> {
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
            decoration,
            settings,
        },
        |realm_seed| TestContext::new(realm_seed, BASE),
        &mut checks,
    );
    checks.finish().expect("the test world opens");
    generator.expect("no problem")
}

/// A test kind whose far LOD drops the octaves of its noise shorter than `8 s` from level
/// `from` on (`s` the cell size), with the margin of what is kept and the bound of what is
/// dropped as left out: a `lod_heights` of its own, as a game writes one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CoarseRelief {
    /// The kind.
    pub relief: Relief,
    /// The first level that drops octaves.
    pub from: u8,
}

impl HashParams for CoarseRelief {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.relief).write(&self.from);
    }
}

impl GenEntry for CoarseRelief {
    fn version(&self) -> u32 {
        1
    }
}

impl CoarseRelief {
    /// The noise of `level` under `seed` and how far it is from the whole one.
    fn lod_noise(&self, seed: u64, level: u8) -> (NoiseField, u64) {
        let noise = self.relief.noise(seed);
        if level < self.from {
            return (noise, 0);
        }
        noise.split_at(8 << level)
    }
}

impl TerrainKind<TestContext> for CoarseRelief {
    fn limits(&self, cx: &TestContext) -> KindLimits {
        self.relief.limits(cx)
    }
    fn range(&self, cx: &TestContext) -> Span {
        self.relief.range(cx)
    }
    fn bounds(&self, cx: &TestContext, seed: u64, q: (i64, i64), reach: i64) -> Span {
        self.relief.bounds(cx, seed, q, reach)
    }
    fn heights(&self, grid: &KindGrid<'_, TestContext>, mask: &[bool], out: &mut [HeightQ]) {
        self.relief.heights(grid, mask, out);
    }
    fn lod_heights(
        &self,
        grid: &KindGrid<'_, TestContext>,
        level: u8,
        _mask: &[bool],
        out: &mut [HeightQ],
        margin: &mut [HeightQ],
    ) {
        let (noise, dropped) = self.lod_noise(grid.seed, level);
        let mut n = vec![0; grid.len()];
        noise.grid(grid.xs, grid.zs, &mut n);
        for ((o, &b), n) in out.iter_mut().zip(&grid.fields.base).zip(n) {
            *o = (b + self.relief.offset + n) as HeightQ;
        }
        // The kept field's slope over half a cell's diagonal, and what the dropped octaves add.
        let half = Rect::cell(0, 0, 1 << level).half_diagonal();
        let slope = grid.cx.base().slope().sum(noise.slope());
        margin.fill((slope.over(half) + dropped) as HeightQ);
    }
    fn lod_left_out(&self, cx: &TestContext, level: u8) -> HeightQ {
        let _ = cx;
        self.lod_noise(0, level).1 as HeightQ
    }
}

/// A pair of straight valleys along x every `period` blocks of z, at `z = 0` and `z = gap`
/// (modulo the period), carved `min(H, floor + (H - floor) min(n, width) / width)` with `n` the
/// distance to the nearer one: two valleys with a divide between them. Its far LOD form uses the
/// divide term (`divide: true`) or not, to show the harness what goes wrong without it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Valleys {
    /// The pattern's period along z, in blocks.
    pub period: i64,
    /// The second valley's offset from the first.
    pub gap: i64,
    /// The half-width of a valley, in blocks.
    pub width: i64,
    /// The valleys' floor, in 1/256 blocks.
    pub floor: HeightQ,
    /// The deepest cut it declares, in 1/256 blocks.
    pub depth: u32,
    /// Whether its coarse form takes the divide term.
    pub divide: bool,
}

impl Valleys {
    /// The nearer valley of column `z` (its id) and the distance to it.
    pub fn nearest(&self, z: i64) -> (u64, i64) {
        let (k, r) = (z.div_euclid(self.period), z.rem_euclid(self.period));
        // The valleys around `r`: the gap's, this period's start and the next's.
        let candidates = [
            (2 * k, r),
            (2 * k + 1, (r - self.gap).abs()),
            (2 * k + 2, self.period - r),
        ];
        let (id, d) = candidates
            .into_iter()
            .min_by_key(|&(id, d)| (d, id))
            .expect("three");
        (id as u64, d)
    }

    /// The distance from the columns `z0..=z1` to the nearest valley.
    pub fn distance(&self, z0: i64, z1: i64) -> i64 {
        let first = z0.div_euclid(self.period) - 1;
        let last = z1.div_euclid(self.period) + 1;
        (first..=last)
            .flat_map(|k| [k * self.period, k * self.period + self.gap])
            .map(|v| {
                if v < z0 {
                    z0 - v
                } else if v > z1 {
                    v - z1
                } else {
                    0
                }
            })
            .min()
            .expect("valleys")
    }

    /// The carve of a height `h` at distance `n`.
    pub fn carve(&self, h: i64, n: i64) -> i64 {
        let floor = i64::from(self.floor);
        let g = n.clamp(0, self.width);
        h.min(floor + ((h - floor) * g).div_euclid(self.width))
    }
}

impl HashParams for Valleys {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.period)
            .write(&self.gap)
            .write(&self.width)
            .write(&self.floor)
            .write(&self.depth)
            .write(&self.divide);
    }
}

impl GenEntry for Valleys {
    fn version(&self) -> u32 {
        1
    }
}

impl<Cx: Context> ShapeModifier<Cx> for Valleys {
    fn effect(&self) -> ShapeEffect {
        ShapeEffect::LowerOnly(self.depth)
    }
    /// The walls: at most the deepest cut over the width, per block of distance (the distance
    /// is `z`'s, 1 per block), rounded down once.
    fn slope(&self) -> Slope {
        Slope {
            per_block: Q16::ratio(u128::from(self.depth), self.width as u128),
            jitter: 1,
        }
    }
    /// No cut where every column of `area` is `width` or more from both valleys.
    fn column_effect(&self, _: &Cx, _: u64, area: Rect) -> ShapeEffect {
        if self.distance(area.z0, area.z1) >= self.width {
            ShapeEffect::LowerOnly(0)
        } else {
            ShapeEffect::LowerOnly(self.depth)
        }
    }
    fn apply(&self, grid: &KindGrid<'_, Cx>, heights: &mut [HeightQ]) {
        for (row, _) in heights.chunks_exact_mut(grid.zs.len()).zip(grid.xs) {
            for (h, &z) in row.iter_mut().zip(grid.zs) {
                *h = self.carve(i64::from(*h), self.nearest(z).1) as HeightQ;
            }
        }
    }
    fn lod(&self, grid: &KindGrid<'_, Cx>, _level: u8, corners: &mut [HeightQ], tops: &mut [i64]) {
        self.apply(grid, corners);
        let at: Vec<CarveCorner> = grid
            .xs
            .iter()
            .flat_map(|_| grid.zs.iter())
            .map(|&z| {
                let (segment, n) = self.nearest(z);
                CarveCorner {
                    segment: if self.divide { segment } else { 0 },
                    n,
                }
            })
            .collect();
        divide_tops(
            (grid.xs.len() - 1, grid.zs.len() - 1),
            &at,
            0,
            tops,
            |_, _, top, n| self.carve(top, n),
        );
    }
}

/// A test kind with its bounds per chunk column ([`TerrainKind::column_bounds`]): the [`Relief`]
/// with the bound of its fields over the chunk column, much tighter than over the two lattice
/// cells around a lattice point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hooked(pub Relief);

impl HashParams for Hooked {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.0);
    }
}

impl GenEntry for Hooked {
    fn version(&self) -> u32 {
        1
    }
}

impl TerrainKind<TestContext> for Hooked {
    fn limits(&self, cx: &TestContext) -> KindLimits {
        self.0.limits(cx)
    }
    fn range(&self, cx: &TestContext) -> Span {
        self.0.range(cx)
    }
    fn bounds(&self, cx: &TestContext, seed: u64, q: (i64, i64), reach: i64) -> Span {
        self.0.bounds(cx, seed, q, reach)
    }
    fn column_bounds(&self, cx: &TestContext, seed: u64, area: Rect) -> Option<Span> {
        let b = Add::new(cx.base.clone(), self.0.noise(seed)).bound(area);
        Some(Span::new(b.lo + self.0.offset, b.hi + self.0.offset))
    }
    fn heights(&self, grid: &KindGrid<'_, TestContext>, mask: &[bool], out: &mut [HeightQ]) {
        self.0.heights(grid, mask, out);
    }
}

/// The test kinds with their bounds per chunk column, as registry entries.
pub fn hooked_entries() -> Vec<(&'static str, Arc<dyn TerrainKind<TestContext>>)> {
    kinds()
        .into_iter()
        .map(|(name, k)| {
            (
                name,
                Arc::new(Hooked(k)) as Arc<dyn TerrainKind<TestContext>>,
            )
        })
        .collect()
}

/// A test carver: holes of 4 x 4 x 4 blocks, one cube in `one_in` by a hash of the cube, under a
/// roof: nothing is carved within `roof` blocks under the lowest ground of the column and of its
/// 4 neighbors `reads` blocks away. So its rules read the surface `roof` blocks deep over an
/// apron of `reads`; it declares `reach`, right or wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Holes {
    /// One cube in this many is a hole.
    pub one_in: u64,
    /// The roof, in blocks.
    pub roof: u32,
    /// How far it reads the neighbors' grounds.
    pub reads: u32,
    /// What it declares.
    pub reach: CarveReach,
}

impl Holes {
    /// Holes in one cube of `one_in` under a roof of `roof` over an apron of `reads`, declared
    /// right.
    pub const fn new(one_in: u64, roof: u32, reads: u32) -> Self {
        Self {
            one_in,
            roof,
            reads,
            reach: CarveReach {
                surface: roof,
                apron: reads,
            },
        }
    }

    /// Whether the cube `(i, j, k)` (blocks `4 i..4 i + 4`...) is a hole under `seed`.
    pub fn hole(&self, seed: u64, (i, j, k): (i64, i64, i64)) -> bool {
        let h = splitmix64(
            seed ^ splitmix64(i as u64)
                ^ splitmix64(j as u64).rotate_left(21)
                ^ splitmix64(k as u64).rotate_left(42),
        );
        h.is_multiple_of(self.one_in)
    }

    /// Per column of the chunk of `columns`, the lowest ground its rule reads: its chunk's own
    /// columns, and the ring of its apron through [`ChunkColumns::tops`], 4 strips (as a cave
    /// stage asks for its water shell).
    fn lowest<Cx: Context>(&self, columns: &ChunkColumns<'_, Cx>) -> Vec<i64> {
        let (o, r) = (columns.origin, i64::from(self.reads));
        let side = 32 + 2 * r;
        let mut ground = vec![0; (side * side) as usize];
        for (c, &g) in columns.ground.iter().enumerate() {
            let (x, z) = (r + (c >> 5) as i64, r + (c & 31) as i64);
            ground[(x * side + z) as usize] = g;
        }
        if r > 0 {
            // The -x and +x strips over the whole side, then the -z and +z ones between them.
            let strips = [
                (0, r, 0, side),
                (32 + r, side, 0, side),
                (r, 32 + r, 0, r),
                (r, 32 + r, 32 + r, side),
            ];
            for (x0, x1, z0, z1) in strips {
                let xs: Vec<i64> = (x0..x1).map(|x| o.x - r + x).collect();
                let zs: Vec<i64> = (z0..z1).map(|z| o.z - r + z).collect();
                let n = xs.len() * zs.len();
                let (mut g, mut f) = (vec![0; n], vec![0; n]);
                columns.tops(&xs, &zs, &mut g, &mut f);
                for (i, x) in (x0..x1).enumerate() {
                    for (j, z) in (z0..z1).enumerate() {
                        ground[(x * side + z) as usize] = g[i * zs.len() + j];
                    }
                }
            }
        }
        let at = |x: i64, z: i64| ground[(x * side + z) as usize];
        (0..CHUNK_AREA as i64)
            .map(|c| {
                let (x, z) = (r + (c >> 5), r + (c & 31));
                at(x, z)
                    .min(at(x - r, z))
                    .min(at(x + r, z))
                    .min(at(x, z - r))
                    .min(at(x, z + r))
            })
            .collect()
    }
}

impl HashParams for Holes {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.one_in)
            .write(&self.roof)
            .write(&self.reads)
            .write(&self.reach.surface)
            .write(&self.reach.apron);
    }
}

impl GenEntry for Holes {
    fn version(&self) -> u32 {
        1
    }
}

impl<Cx: Context> CarverStage<Cx> for Holes {
    fn reach(&self) -> CarveReach {
        self.reach
    }
    fn carve(&self, chunk: &CarveChunk<'_, Cx>) -> CarveMask {
        let o = chunk.origin;
        let lowest = chunk.columns.map(|c| self.lowest(c));
        let roof = i64::from(self.roof);
        let mut keep: Box<Occupancy> = Box::new([u32::MAX; CHUNK_AREA]);
        for n in 0..512 {
            let (ci, cj, ck) = (n >> 6, (n >> 3) & 7, n & 7);
            if !self.hole(
                chunk.seed,
                ((o.x >> 2) + ci, (o.y >> 2) + cj, (o.z >> 2) + ck),
            ) {
                continue;
            }
            for ly in 4 * cj..4 * cj + 4 {
                for lx in 4 * ci..4 * ci + 4 {
                    let word = &mut keep[(ly * 32 + lx) as usize];
                    for lz in 4 * ck..4 * ck + 4 {
                        let under_roof = lowest
                            .as_ref()
                            .is_none_or(|low| o.y + ly <= low[(lx * 32 + lz) as usize] - roof);
                        if under_roof {
                            *word &= !(1 << lz);
                        }
                    }
                }
            }
        }
        CarveMask::of(keep)
    }
    fn block_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

/// How [`Entrances`] gives its reach per chunk column ([`CarverStage::column_reach`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Declares {
    /// Its declared surface reach everywhere (the default).
    Global,
    /// The shafts' depth where the columns it reads touch a region with shafts, the holes' roof
    /// elsewhere: right.
    Regional,
    /// The holes' roof everywhere: under its shafts.
    Roof,
    /// One block over its declared surface reach everywhere.
    Over,
}

/// A test carver with entrances in some regions only: the holes of [`Holes`] everywhere, and in
/// one region of `2^shift` x `2^shift` blocks in `one_in` (by a hash of the region) a shaft of
/// 2 x 2 columns every 8 blocks along x and z, open from each column's ground down to `depth`
/// blocks under it. So its rules read the surface `depth` blocks deep where the columns it reads
/// touch such a region and the holes' roof elsewhere. It declares a surface reach of the deeper
/// of the two over the holes' apron, and per chunk column what `declares` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Entrances {
    /// The holes.
    pub holes: Holes,
    /// `log2` of a region's side, in blocks.
    pub shift: u32,
    /// One region in this many holds shafts.
    pub one_in: u64,
    /// How deep a shaft opens under its column's ground, in blocks.
    pub depth: u32,
    /// Its reach per chunk column.
    pub declares: Declares,
}

impl Entrances {
    /// The holes `holes` and shafts `depth` blocks deep in one region of 256 x 256 blocks in 3,
    /// with a reach per chunk column as `declares` says.
    pub const fn new(holes: Holes, depth: u32, declares: Declares) -> Self {
        Self {
            holes,
            shift: 8,
            one_in: 3,
            depth,
            declares,
        }
    }

    /// Whether region `(i, j)` (blocks `i << shift..(i + 1) << shift` along x) holds shafts under
    /// `seed`.
    pub fn region(&self, seed: u64, (i, j): (i64, i64)) -> bool {
        let h =
            splitmix64(seed ^ 0x5aff ^ splitmix64(i as u64) ^ splitmix64(j as u64).rotate_left(32));
        h.is_multiple_of(self.one_in)
    }

    /// Whether a column of `area` lies in a region with shafts under `seed`.
    pub fn touches(&self, seed: u64, area: Rect) -> bool {
        let s = self.shift;
        (area.x0 >> s..=area.x1 >> s)
            .any(|i| (area.z0 >> s..=area.z1 >> s).any(|j| self.region(seed, (i, j))))
    }

    /// Whether column `(x, z)` is a shaft's under `seed`.
    pub fn shaft(&self, seed: u64, x: i64, z: i64) -> bool {
        x & 7 < 2 && z & 7 < 2 && self.region(seed, (x >> self.shift, z >> self.shift))
    }
}

impl HashParams for Entrances {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.holes)
            .write(&self.shift)
            .write(&self.one_in)
            .write(&self.depth)
            .write(&(self.declares as u8));
    }
}

impl GenEntry for Entrances {
    fn version(&self) -> u32 {
        1
    }
}

impl<Cx: Context> CarverStage<Cx> for Entrances {
    fn reach(&self) -> CarveReach {
        CarveReach {
            surface: self.depth.max(self.holes.roof),
            apron: self.holes.reads,
        }
    }
    fn column_reach(&self, _: &Cx, seed: u64, area: Rect) -> u32 {
        let surface = self.depth.max(self.holes.roof);
        match self.declares {
            Declares::Global => surface,
            Declares::Regional if self.touches(seed, area) => surface,
            Declares::Regional | Declares::Roof => self.holes.roof,
            Declares::Over => surface + 1,
        }
    }
    fn carve(&self, chunk: &CarveChunk<'_, Cx>) -> CarveMask {
        let holes = self.holes.carve(chunk);
        let Some(c) = chunk.columns else {
            return holes;
        };
        let (o, depth) = (c.origin, i64::from(self.depth));
        let mut keep: Box<Occupancy> = Box::new([u32::MAX; CHUNK_AREA]);
        for (n, &g) in c.ground.iter().enumerate() {
            let (lx, lz) = (n >> 5, n & 31);
            if !self.shaft(chunk.seed, o.x + lx as i64, o.z + lz as i64) {
                continue;
            }
            // From the ground down to `depth` under it, inside the chunk.
            for ly in (g - depth + 1 - o.y).max(0)..=(g - o.y).min(31) {
                keep[ly as usize * 32 + lx] &= !(1 << lz);
            }
        }
        holes.and(CarveMask::of(keep))
    }
    fn block_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

/// The chunks of 32 x 32 chunk columns from `(cx, cz)` that `g` builds through the column stage:
/// those over the deep bound and at or under the local top of their chunk column.
pub fn column_stage_chunks(g: &RegionGenerator<TestContext>, (cx, cz): (i64, i64)) -> i64 {
    let mut chunks = 0;
    for x in cx..cx + 32 {
        for z in cz..cz + 32 {
            let b = g.local_bounds(x, z);
            chunks += ((b.top >> 5) - ((b.deep - 31) >> 5)).max(0);
        }
    }
    chunks
}

/// A test feature stage: trees with a trunk of `test:lamp` and a canopy of `test:cutout`, at most
/// one per cell of [`Groves::CELL`] columns (one cell in `one_in`, by a hash of the cell), its
/// trunk anywhere in the cell, so canopies cross cell and chunk borders: a chunk reads the grounds
/// of the trunks around it through its apron. A tree grows on a dry column whose ground is at or
/// over `min_ground`; its trunk is 4 to 6 blocks, its canopy a ring of radius 2 around its top two
/// blocks and a cross over it. Each block goes only over air and inside its own column's reach
/// (over the column's ground, at most [`Groves::HEIGHT`] over it), so a canopy over a slope is
/// cut, the same from every chunk.
///
/// Its far LOD gives a run of `run` blocks of `test:cutout` (0: none) on every dry cell column
/// whose ground is at or over `min_ground`, or on every cell column with `wet_runs` (wrong on a
/// shallow coast: the run is buried under the land between the corners).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Groves {
    /// One cell in this many holds a tree.
    pub one_in: u64,
    /// The lowest ground a tree grows on.
    pub min_ground: i64,
    /// The far LOD run's blocks, 0 for none.
    pub run: u32,
    /// Whether the far LOD gives the run on wet cells too.
    pub wet_runs: bool,
}

impl Groves {
    /// Columns per side of a tree cell, aligned with the chunks.
    pub const CELL: i64 = 16;
    /// The canopy's reach around the trunk.
    pub const RADIUS: i64 = 2;
    /// The reach over a column's ground: a tree is at most 7 blocks over its trunk's ground, and
    /// 5 more let a canopy hang over a lower neighbor.
    pub const HEIGHT: u32 = 12;

    /// Trees in one cell of `one_in` on dry ground at or over y 0, with a far LOD run of `run`
    /// on the dry cells.
    pub const fn new(one_in: u64, run: u32) -> Self {
        Self {
            one_in,
            min_ground: 0,
            run,
            wet_runs: false,
        }
    }

    /// The tree of cell `(i, j)` under `seed`, if it has one: its trunk column and its trunk's
    /// blocks. Its ground is the trunk column's.
    pub fn tree(&self, seed: u64, (i, j): (i64, i64)) -> Option<(i64, i64, i64)> {
        let h = splitmix64(seed ^ splitmix64(i as u64) ^ splitmix64(j as u64).rotate_left(32));
        if !h.is_multiple_of(self.one_in) {
            return None;
        }
        let x = i * Self::CELL + ((h >> 8) % Self::CELL as u64) as i64;
        let z = j * Self::CELL + ((h >> 16) % Self::CELL as u64) as i64;
        Some((x, z, 4 + ((h >> 24) % 3) as i64))
    }

    /// Calls `put(x, y, z, log)` for every block of the tree with its trunk at `(x, z)` on the
    /// ground `g`, `trunk` blocks tall (each block once; `log` for the trunk).
    pub fn blocks(x: i64, z: i64, g: i64, trunk: i64, mut put: impl FnMut(i64, i64, i64, bool)) {
        let top = g + trunk;
        for y in g + 1..=top {
            put(x, y, z, true);
        }
        for y in top - 1..=top + 1 {
            let r = if y <= top { Self::RADIUS } else { 1 };
            for dx in -r..=r {
                for dz in -r..=r {
                    if (dx.abs() == r && dz.abs() == r) || (dx == 0 && dz == 0 && y <= top) {
                        continue;
                    }
                    put(x + dx, y, z + dz, false);
                }
            }
        }
    }

    /// The cells whose trees may reach the columns of `area`.
    fn cells(area: Rect) -> impl Iterator<Item = (i64, i64)> {
        let (r, c) = (Self::RADIUS, Self::CELL);
        let is = (area.x0 - r).div_euclid(c)..=(area.x1 + r).div_euclid(c);
        let js = (area.z0 - r).div_euclid(c)..=(area.z1 + r).div_euclid(c);
        is.flat_map(move |i| js.clone().map(move |j| (i, j)))
    }

    /// Whether the tree with its trunk at `(x, z)` may put a block over a column of `area`.
    fn reaches(area: Rect, x: i64, z: i64) -> bool {
        let r = Self::RADIUS;
        x + r >= area.x0 && x - r <= area.x1 && z + r >= area.z0 && z - r <= area.z1
    }
}

impl HashParams for Groves {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.one_in)
            .write(&self.min_ground)
            .write(&self.run)
            .write(&self.wet_runs);
    }
}

impl GenEntry for Groves {
    fn version(&self) -> u32 {
        1
    }
}

impl<Cx: Context> FeatureStage<Cx> for Groves {
    fn reach(&self) -> FeatureReach {
        FeatureReach {
            height: Self::HEIGHT,
            apron: Self::RADIUS as u32,
        }
    }
    /// The height where a tree's cell hashes to one whose canopy reaches `area` (whatever its
    /// ground), else 0.
    fn column_height(&self, _cx: &Cx, seed: u64, area: Rect) -> u32 {
        let grows = Self::cells(area).any(|cell| {
            self.tree(seed, cell)
                .is_some_and(|(x, z, _)| Self::reaches(area, x, z))
        });
        if grows { Self::HEIGHT } else { 0 }
    }
    fn apply(
        &self,
        columns: &ChunkColumns<'_, Cx>,
        seed: u64,
        registry: &BlockRegistry,
        out: &mut [BlockState],
    ) {
        let log = registry.by_name("test:lamp").expect("test:lamp");
        let leaves = registry.by_name("test:cutout").expect("test:cutout");
        let o = columns.origin;
        let size = i64::from(CHUNK_SIZE);
        let chunk = Rect::new(o.x, o.z, o.x + size - 1, o.z + size - 1);
        let height = i64::from(Self::HEIGHT);
        for cell in Self::cells(chunk) {
            let Some((x, z, trunk)) = self.tree(seed, cell) else {
                continue;
            };
            if !Self::reaches(chunk, x, z) {
                continue;
            }
            // The trunk's ground: the chunk's own column, or one of its apron.
            let (lx, lz) = (x - o.x, z - o.z);
            let (g, fluid) = if (0..size).contains(&lx) && (0..size).contains(&lz) {
                let c = (lx * size + lz) as usize;
                (columns.ground[c], columns.fluid_top[c])
            } else {
                let (mut g, mut f) = ([0], [0]);
                columns.tops(&[x], &[z], &mut g, &mut f);
                (g[0], f[0])
            };
            if g < self.min_ground || fluid > g || g + trunk + 1 < o.y || g + 1 > o.y + size - 1 {
                continue;
            }
            Self::blocks(x, z, g, trunk, |bx, by, bz, is_log| {
                let (lx, ly, lz) = (bx - o.x, by - o.y, bz - o.z);
                if ![lx, ly, lz].iter().all(|v| (0..size).contains(v)) {
                    return;
                }
                let c = (lx * size + lz) as usize;
                let ground = columns.ground[c];
                let i = ((ly as usize) << 10) | c;
                if by > ground && by <= ground + height && out[i] == BlockState::AIR {
                    out[i] = if is_log { log } else { leaves };
                }
            });
        }
    }
    fn block_names(&self) -> Vec<&str> {
        vec!["test:lamp", "test:cutout"]
    }
    fn lod_runs(
        &self,
        columns: &LodColumns<'_, Cx>,
        _seed: u64,
        registry: &BlockRegistry,
        out: &mut [(BlockState, u32)],
    ) {
        if self.run == 0 {
            return;
        }
        let leaves = registry.by_name("test:cutout").expect("test:cutout");
        for ((o, &g), &f) in out.iter_mut().zip(columns.ground).zip(columns.fluid_top) {
            if self.wet_runs || (f == g && g >= self.min_ground) {
                *o = (leaves, self.run);
            }
        }
    }
}

/// A test feature stage: one `test:plant` over the ground of one dry column in `one_in` (by a
/// hash of the column), on ground at or over y 0, where it finds air.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tufts {
    /// One column in this many has a plant.
    pub one_in: u64,
}

impl HashParams for Tufts {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.one_in);
    }
}

impl GenEntry for Tufts {
    fn version(&self) -> u32 {
        1
    }
}

impl<Cx: Context> FeatureStage<Cx> for Tufts {
    fn reach(&self) -> FeatureReach {
        FeatureReach {
            height: 1,
            apron: 0,
        }
    }
    fn apply(
        &self,
        columns: &ChunkColumns<'_, Cx>,
        seed: u64,
        registry: &BlockRegistry,
        out: &mut [BlockState],
    ) {
        let plant = registry.by_name("test:plant").expect("test:plant");
        let o = columns.origin;
        for (c, (&g, &f)) in columns.ground.iter().zip(columns.fluid_top).enumerate() {
            let ly = g + 1 - o.y;
            let (x, z) = (o.x + (c >> 5) as i64, o.z + (c & 31) as i64);
            let h = splitmix64(seed ^ splitmix64(x as u64) ^ splitmix64(z as u64).rotate_left(29));
            if f > g || g < 0 || !(0..32).contains(&ly) || !h.is_multiple_of(self.one_in) {
                continue;
            }
            let i = ((ly as usize) << 10) | c;
            if out[i] == BlockState::AIR {
                out[i] = plant;
            }
        }
    }
    fn block_names(&self) -> Vec<&str> {
        vec!["test:plant"]
    }
}

/// The FNV fingerprint of a chunk's states.
pub fn fingerprint(chunk: &ChunkData) -> u64 {
    let mut dense = vec![BlockState::AIR; boxel_voxel::CHUNK_VOLUME];
    chunk.to_dense(&mut dense);
    let bytes: Vec<u8> = dense.iter().flat_map(|s| s.0.to_le_bytes()).collect();
    boxel_worldgen::registry::fnv1a64(&bytes)
}

/// The columns around a chunk of a stage that reads none (the twin's surface).
pub struct NoApron;

impl ColumnsAround for NoApron {
    fn tops(&self, _: &[i64], _: &[i64], _: &mut [i64], _: &mut [i64]) {
        unreachable!("the twin's stages read no column outside their chunk")
    }
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
            carvers: &[],
            features: &[],
            floor: solid,
            fluid: registry.by_name("test:fluid").expect("test:fluid"),
        };
        let columns = ChunkColumns::new(origin, &ground, &fluid_top, &self.cx, &fields, &NoApron);
        decorate(&columns, &stages, registry)
    }
}

/// The distinct kinds of the lattice points a level-`level` node column at `column` reads: the
/// windows of the lattice cells its padded corners fall in.
pub fn node_kinds(g: &RegionGenerator<TestContext>, level: u8, column: [i64; 2]) -> usize {
    let ls = g.lattice();
    let (s, e) = (1i64 << level, 32i64 << level);
    let (x, z) = (column[0] * e - s, column[1] * e - s);
    let (i0, i1) = ((x >> ls) - 1, ((x + 34 * s) >> ls) + 2);
    let (j0, j1) = ((z >> ls) - 1, ((z + 34 * s) >> ls) + 2);
    let mut kinds = Vec::new();
    for i in i0..=i1 {
        for j in j0..=j1 {
            kinds.push(g.lattice_kind(i, j).clone());
        }
    }
    kinds.sort();
    kinds.dedup();
    kinds.len()
}

/// Node columns of `level` with exactly `kinds` kinds, searched from `from` along a diagonal walk.
pub fn nodes_with(
    g: &RegionGenerator<TestContext>,
    level: u8,
    kinds: usize,
    from: [i64; 2],
    count: usize,
) -> Vec<[i64; 2]> {
    let e = 32i64 << level;
    let step = (1024 / e).max(1);
    let mut found = Vec::new();
    for n in 0..1 << 16 {
        let c = [from[0] + n * step, from[1] + (n * 7 % 13) * step];
        if node_kinds(g, level, c) == kinds {
            found.push(c);
            if found.len() == count {
                return found;
            }
        }
    }
    panic!("no {count} node columns of level {level} with {kinds} kinds from {from:?}");
}
