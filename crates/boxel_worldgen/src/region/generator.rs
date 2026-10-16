//! [`RegionGenerator`]: one realm's terrain from its region map, kinds and decoration.

use super::blend::{self, WINDOW_POINTS, Window};
use super::decorate::{
    CarveReach, ChunkColumns, ColumnsAround, Decoration, FillStage, MAX_APRON, RealmCarver,
    RealmFeature, Stages, SurfaceReach, SurfaceStage,
};
use super::kind::{Context, HEIGHT_ONE, HeightQ, KindGrid, TerrainKind, block_of};
use super::memo::{self, Point, WindowKinds, WindowPoints};
use super::modifier::{ShapeEffect, ShapeModifier};
use super::select::{self, RuleEntry, Selector, SelectorRule, check_rule_entries};
use super::{
    CODE_VERSION, DEFAULT_LATTICE_SHIFT, MAX_LATTICE_SHIFT, MAX_SLOPE, MIN_LATTICE_SHIFT,
    REGION_LINE, check,
};
use crate::field::Rect;
use crate::lod::LodSource;
use crate::query::{ColumnInfo, WorldQuery};
use crate::realm::{FluidRule, RealmSpec, RealmStack, check_floors};
use crate::registry::{
    BuildKey, Checks, DigestBuilder, DigestEntry, DigestScope, GenEntry, GenId, GenRegistry,
    ParamsHasher, Problem, RegistryDigest, hash_params,
};
use crate::{WorldGenerator, block};
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_AREA, CHUNK_SHIFT, CHUNK_SIZE, ChunkData, ChunkKey,
    LocalPos,
};
use std::borrow::Cow;
use std::ops::RangeInclusive;
use std::sync::Arc;

/// Name in panics.
const NAME: &str = "RegionGenerator";

/// The mask of every point of a chunk column (the interior fast path's one `heights` call).
static ALL: [bool; CHUNK_AREA] = [true; CHUNK_AREA];

/// The knobs of a [`RegionGenerator`] that are not registry entries. The lattice goes to the
/// realm's digest line `region`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RegionSettings {
    /// `log2` of the region lattice's spacing, in
    /// `MIN_LATTICE_SHIFT..=MAX_LATTICE_SHIFT`: [`DEFAULT_LATTICE_SHIFT`] (256 blocks), so the
    /// blend band is 768 blocks wide.
    pub lattice: u32,
    /// Whether the generator keeps its lattice points in the per-worker memo. Off, every chunk
    /// computes its 16 lattice points again (for tests and benches: the output is the same).
    pub memo: bool,
}

impl Default for RegionSettings {
    fn default() -> Self {
        Self {
            lattice: DEFAULT_LATTICE_SHIFT,
            memo: true,
        }
    }
}

/// What a [`RegionGenerator`] is built from at world open: the world's seed, where it sits (its
/// dimension and realm), the app's frozen registries, the block registry its names are checked
/// against, its decoration and its settings.
pub struct RegionSpec<'a, Cx: Context> {
    /// The world's seed (the realm's comes from it and the realm's salt).
    pub seed: u64,
    /// The dimension.
    pub dimension: &'a GenId,
    /// The realm, an id of `realms`.
    pub realm: &'a GenId,
    /// The realms.
    pub realms: &'a GenRegistry<RealmSpec>,
    /// The selector rules of every realm: the generator reads those naming its realm.
    pub rules: &'a GenRegistry<SelectorRule>,
    /// The entries mods add to rules.
    pub entries: &'a GenRegistry<RuleEntry>,
    /// The kinds written against `Cx`.
    pub kinds: &'a GenRegistry<Arc<dyn TerrainKind<Cx>>>,
    /// The blocks: every block name the realm and its stages use must be in it (the world's
    /// registry, the one chunks are generated with).
    pub blocks: &'a BlockRegistry,
    /// The decoration: the fill and surface stages, the shape modifiers, the carvers and the
    /// feature stages.
    pub decoration: Decoration<Cx>,
    /// The other knobs.
    pub settings: RegionSettings,
}

/// The vertical bounds of a chunk column of a realm (ADR 0039 sec. 14): exact integers, every one
/// a block `y`. They come from the kinds present in the 16 lattice points around the column (each
/// kind's [`TerrainKind::column_bounds`] over the chunk column, else its points' bounds), the
/// shape modifiers' [`ShapeModifier::column_effect`] there and the feature stages'
/// [`FeatureStage::column_height`](super::FeatureStage::column_height).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalBounds {
    /// A block at or under the lowest ground of the column (the open surface): the realm's
    /// `ground_lo`. Nothing reads it yet; WG4 and the sky light per realm will.
    pub ground_lo: i64,
    /// A block at or over the highest ground.
    pub ground_hi: i64,
    /// The highest block that may not be air: `ground_hi` plus the surface stage's rise or the
    /// feature stages' height over the column, whichever is more, or the fluid. Every chunk over
    /// it is `Uniform` air, after the selection reads.
    pub top: i64,
    /// A chunk whose highest block is at or under this one lies under every column's surface
    /// stage and every carver's surface reach ([`CarveReach::surface`] under `apron_lo`): it
    /// takes the deep path, the fill and the carvers without columns, without the column stage.
    pub deep: i64,
    /// A block at or under the lowest ground of the column and of its carvers' widest apron
    /// around it ([`CarveReach::apron`], read from the next lattice cells' windows where it
    /// reaches them): what `deep` is built from with carvers, and what they get as
    /// [`CarveChunk::apron_lo`](super::CarveChunk::apron_lo). `ground_lo` without carvers or
    /// with an apron of 0.
    pub apron_lo: i64,
}

/// A kind of the realm, ready to run.
pub(super) struct RealmKind<Cx: Context> {
    pub(super) id: GenId,
    pub(super) kind: Arc<dyn TerrainKind<Cx>>,
    /// The realm's seed mixed with the kind's salt.
    pub(super) seed: u64,
}

/// A shape modifier of the realm, ready to run.
pub(super) struct RealmModifier<Cx: Context> {
    pub(super) id: GenId,
    pub(super) modifier: Arc<dyn ShapeModifier<Cx>>,
    /// The realm's seed mixed with the modifier's salt.
    pub(super) seed: u64,
    pub(super) effect: ShapeEffect,
}

/// One realm's terrain (ADR 0039 sec. 4, 5, 9 and 14): the realm's [`Context`], its region map
/// (the selector), its kinds blended at their borders, its fluid rule and its decoration, built
/// at world open from the app's registries ([`RegionGenerator::build`]).
///
/// A chunk, from the cheapest answer to the dearest:
/// - outside the realm's span: `Uniform` air (a [`DimensionGenerator`](super::DimensionGenerator)
///   never asks for one);
/// - the 16 lattice points around the chunk's column (the per-worker memo, else the selector and
///   each point's kind bounds) give the column's [`LocalBounds`]: over their top, `Uniform` air;
///   under their deep bound, the fill alone (the deep path, `Uniform` for a uniform rock);
/// - else the column stage: the Context's fields over the 32 x 32 columns, each present kind's
///   heights (one call when the 16 points hold one kind: the interior fast path, with no
///   multiply), the exact blend, the fluid rule, then [`decorate`](super::decorate) (rock,
///   fluid, surface, carvers, features; `Uniform`, 1-bit or dense).
///
/// Its far LOD is the region pass ([`LodSource`], ADR 0039 sec. 10): the same selection, blend
/// and shape modifiers at the cell corners of each node, a sound top per cell and the cell rule.
///
/// In debug builds every chunk of the column stage, and every point query, checks its column
/// bounds: each column's ground lies between the bounds of its 16 points, nothing but air lies
/// over the column's top, and that top lies in the realm's vertical range (the kinds' ranges).
/// Every chunk of the column stage also checks that every ground its carvers may read around it
/// lies over its chunk column's `apron_lo` (so over `deep` by their surface reach), and that its
/// heights keep the 3:1 cap between axis neighbors (the slope check, ADR 0039 sec. 6). A kind
/// or a mod with a bound, a range or a slope under its heights fails its tests there.
pub struct RegionGenerator<Cx: Context> {
    pub(super) key: BuildKey,
    dimension: GenId,
    /// The realm alone, as [`WorldGenerator::realms`] reports it.
    stack: RealmStack,
    pub(super) cx: Cx,
    pub(super) shift: u32,
    pub(super) memo: bool,
    pub(super) selector: Selector,
    pub(super) kinds: Box<[RealmKind<Cx>]>,
    /// In the order they run.
    pub(super) modifiers: Box<[RealmModifier<Cx>]>,
    fill: Arc<dyn FillStage<Cx>>,
    pub(super) surface: Arc<dyn SurfaceStage<Cx>>,
    pub(super) reach: SurfaceReach,
    /// In the order they run.
    carvers: Box<[RealmCarver<Cx>]>,
    /// The carvers' widest surface reach and apron, `None` without carvers.
    carve: Option<CarveReach>,
    /// In the order they run.
    pub(super) features: Box<[RealmFeature<Cx>]>,
    digest: RegistryDigest,
    vertical: RangeInclusive<i64>,
    /// Whether debug builds hold its chunks to the 3:1 cap ([`RegionGenerator::check_slope`]).
    slope_check: bool,
}

impl<Cx: Context> RegionGenerator<Cx> {
    /// The generator of `spec.realm`, with the Context `context` builds from the realm's seed,
    /// or `None` with every problem recorded in `checks`: the realm, the rules and entries the
    /// realm reaches, their kinds and parameters, the kinds' forms, limits, slopes and border
    /// reliefs, the shape modifiers' slopes on top of them ([`check_modifiers`]), the stages'
    /// ids (each once) and the carvers' aprons ([`MAX_APRON`]), every added entry that names a
    /// missing rule ([`check_rule_entries`]), and every block name the realm (its floor and
    /// fluid) and its stages use ([`Problem::UnknownBlock`](crate::registry::Problem::UnknownBlock),
    /// so a typo fails at world open and never in a worker), and the feature stages' reach (a
    /// height of at least 1, an apron of at most [`MAX_APRON`]). Its digest lines
    /// ([`RegionGenerator::digest`]) are those of the realm: the realm, its `region` settings,
    /// its `context`, its rules and entries, its kinds and its stages (`fill_stage`,
    /// `surface_stage`, one `shape_modifier`, `carver_stage` and `feature_stage` per modifier,
    /// carver and feature stage).
    ///
    /// [`check_modifiers`]: super::check_modifiers
    ///
    /// # Panics
    ///
    /// If `spec.settings.lattice` is outside `MIN_LATTICE_SHIFT..=MAX_LATTICE_SHIFT` (a constant
    /// of the game, not data).
    pub fn build(
        spec: RegionSpec<'_, Cx>,
        context: impl FnOnce(u64) -> Cx,
        checks: &mut Checks,
    ) -> Option<Self> {
        let RegionSpec {
            seed,
            dimension,
            realm,
            realms,
            rules,
            entries,
            kinds,
            blocks,
            decoration,
            settings,
        } = spec;
        let shift = settings.lattice;
        assert!(
            (MIN_LATTICE_SHIFT..=MAX_LATTICE_SHIFT).contains(&shift),
            "{NAME}: a lattice of 2^{shift} blocks is outside 2^{MIN_LATTICE_SHIFT}..=2^\
             {MAX_LATTICE_SHIFT}"
        );
        let before = checks.found();
        let r = checks.resolve(realms, realm, Some(dimension))?;
        let realm_spec = realms[r].clone();
        if !realm_spec.form.is_built() {
            checks.push(Problem::FormNotBuilt {
                dimension: dimension.clone(),
                realm: realm.clone(),
                form: realm_spec.form,
            });
        }
        check_floors(checks, dimension, [(realm, &realm_spec)]);
        check_realm_blocks(checks, blocks, realm, &realm_spec);
        let realm_seed = realm.seed(seed);
        let cx = context(realm_seed);
        let scope = DigestScope::of_realm(dimension, realm);
        let mut digest = DigestBuilder::new();
        digest.add(&scope, realms, r);
        let line = |registry: &'static str, id: &GenId, version: u32, params: u64| DigestEntry {
            scope: scope.clone(),
            registry: Cow::Borrowed(registry),
            id: id.clone(),
            version,
            params,
        };
        digest
            .push(line(REGION_LINE, realm, CODE_VERSION, hash_params(&shift)))
            .push(line("context", realm, cx.version(), hash_params(&cx)));
        let Decoration {
            fill,
            surface,
            mut modifiers,
            mut carvers,
            mut features,
        } = decoration;
        for name in fill.1.block_names() {
            checks.block(blocks, name, &fill.0);
        }
        for name in surface.1.block_names() {
            checks.block(blocks, name, &surface.0);
        }
        digest
            .push(line(
                "fill_stage",
                &fill.0,
                fill.1.version(),
                hash_params(&*fill.1),
            ))
            .push(line(
                "surface_stage",
                &surface.0,
                surface.1.version(),
                hash_params(&*surface.1),
            ));
        sort_stages(checks, "shape_modifier", &mut modifiers);
        let modifiers: Vec<RealmModifier<Cx>> = modifiers
            .into_iter()
            .map(|(id, modifier)| {
                let mut params = ParamsHasher::new();
                params.write(&modifier.order()).write(&*modifier);
                digest.push(line(
                    "shape_modifier",
                    &id,
                    modifier.version(),
                    params.finish(),
                ));
                RealmModifier {
                    seed: id.seed(realm_seed),
                    effect: modifier.effect(),
                    id,
                    modifier,
                }
            })
            .collect();
        let lower = modifiers.iter().map(|m| i64::from(m.effect.lower())).sum();
        let raise = modifiers.iter().map(|m| i64::from(m.effect.raise())).sum();
        sort_stages(checks, "carver_stage", &mut carvers);
        let carvers: Vec<RealmCarver<Cx>> = carvers
            .into_iter()
            .map(|(id, stage)| {
                for name in stage.block_names() {
                    checks.block(blocks, name, &id);
                }
                let reach = stage.reach();
                if reach.apron > MAX_APRON {
                    checks.push(Problem::CarverApron {
                        carver: id.clone(),
                        apron: reach.apron,
                    });
                }
                let mut params = ParamsHasher::new();
                params.write(&stage.order()).write(&*stage);
                digest.push(line("carver_stage", &id, stage.version(), params.finish()));
                RealmCarver {
                    seed: id.seed(realm_seed),
                    id,
                    stage,
                }
            })
            .collect();
        let carve = carvers
            .iter()
            .map(|c| c.stage.reach())
            .reduce(|a, b| CarveReach {
                surface: a.surface.max(b.surface),
                apron: a.apron.max(b.apron),
            });
        sort_stages(checks, "feature_stage", &mut features);
        let features: Vec<RealmFeature<Cx>> = features
            .into_iter()
            .map(|(id, stage)| {
                for name in stage.block_names() {
                    checks.block(blocks, name, &id);
                }
                let reach = stage.reach();
                if reach.height == 0 || reach.apron > MAX_APRON {
                    checks.push(Problem::FeatureReach {
                        feature: id.clone(),
                        height: reach.height,
                        apron: reach.apron,
                    });
                }
                let mut params = ParamsHasher::new();
                params.write(&stage.order()).write(&*stage);
                digest.push(line("feature_stage", &id, stage.version(), params.finish()));
                RealmFeature {
                    seed: id.seed(realm_seed),
                    id,
                    stage,
                }
            })
            .collect();
        let grow = features
            .iter()
            .map(|f| f.stage.reach().height)
            .max()
            .unwrap_or(0);
        // Once per world would do; a problem two realms find is reported once.
        check_rule_entries(checks, rules, entries);
        let resolved = select::resolve(
            checks,
            &mut digest,
            &scope,
            (realm, realm_seed, shift),
            &cx,
            rules,
            entries,
            kinds,
        );
        let realm_kinds: Vec<RealmKind<Cx>> = resolved
            .iter()
            .flat_map(|r| &r.kinds)
            .map(|&k| {
                digest.add(&scope, kinds, k);
                RealmKind {
                    id: kinds.id(k).clone(),
                    kind: Arc::clone(&kinds[k]),
                    seed: kinds.seed(k, realm_seed),
                }
            })
            .collect();
        let limits: Vec<_> = realm_kinds
            .iter()
            .map(|k| (k.id.clone(), k.kind.form(), k.kind.limits(&cx)))
            .collect();
        check::check_kinds(checks, realm, &limits, shift);
        let slopes: Vec<_> = modifiers
            .iter()
            .map(|m| (m.id.clone(), m.modifier.slope(), m.modifier.slope_mode()))
            .collect();
        check::check_modifiers(checks, realm, &limits, &slopes, shift);
        if checks.found() > before {
            return None;
        }
        let selector = resolved.expect("no problem").selector;
        let reach = surface.1.reach();
        let vertical = vertical(
            &realm_spec,
            &cx,
            &realm_kinds,
            (reach, grow),
            (lower, raise),
        );
        Some(Self {
            key: BuildKey::next(),
            dimension: dimension.clone(),
            stack: RealmStack::of_one(realm.clone(), realm_spec),
            cx,
            shift,
            memo: settings.memo,
            selector,
            kinds: realm_kinds.into(),
            modifiers: modifiers.into(),
            fill: fill.1,
            surface: surface.1,
            reach,
            carvers: carvers.into(),
            carve,
            features: features.into(),
            digest: digest.finish(),
            vertical,
            slope_check: true,
        })
    }

    /// The generator without its debug slope check (every chunk of the column stage holds its
    /// heights to 3:1 between axis neighbors, ADR 0039 sec. 6), for a test whose fixture is
    /// steeper than 3:1 on purpose and declares a smaller slope so it opens (a worst case of the
    /// far LOD's divide term). Never for a world: the far LOD's skirts cover 3:1 (ADR 0031).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn without_slope_check(mut self) -> Self {
        self.slope_check = false;
        self
    }

    /// The realm's id.
    pub fn realm(&self) -> &GenId {
        self.stack.id(0)
    }

    /// The realm's spec.
    pub fn spec(&self) -> &RealmSpec {
        self.stack.spec(0)
    }

    /// The dimension's id.
    pub fn dimension(&self) -> &GenId {
        &self.dimension
    }

    /// The realm's Context.
    pub fn context(&self) -> &Cx {
        &self.cx
    }

    /// The digest of the realm's lines (scope: the dimension and the realm).
    pub fn digest(&self) -> &RegistryDigest {
        &self.digest
    }

    /// The version of its output: the low 32 bits of its dimension's sub-digest of the realm's
    /// lines, what [`WorldGenerator::version`] and [`LodSource::version`] both report.
    pub fn version(&self) -> u32 {
        self.digest.dimension(&self.dimension).version()
    }

    /// `log2` of the region lattice's spacing.
    pub const fn lattice(&self) -> u32 {
        self.shift
    }

    /// The ids of the realm's kinds, in the order of their registry (a kind's index).
    pub fn kinds(&self) -> impl ExactSizeIterator<Item = &GenId> {
        self.kinds.iter().map(|k| &k.id)
    }

    /// The kind of lattice point `(i, j)` (block `(i << lattice, j << lattice)`): the selector's
    /// pick, the one place a kind is chosen.
    pub fn lattice_kind(&self, i: i64, j: i64) -> &GenId {
        &self.kinds[self.window_at(&[(i, j)])[0].kind as usize].id
    }

    /// The point `(i, j)` from the selector and its kind's bounds, without the memo.
    fn point(&self, i: i64, j: i64) -> Point {
        let (qx, qz) = (i << self.shift, j << self.shift);
        let kind = self.selector.select(&self.cx, qx, qz);
        let k = &self.kinds[kind as usize];
        let reach = 2i64 << self.shift;
        let b = k.kind.bounds(&self.cx, k.seed, (qx, qz), reach);
        Point {
            kind,
            lo: clamp_height(b.lo),
            hi: clamp_height(b.hi),
        }
    }

    /// The points `at`, through the memo when it is on.
    fn window_at<const N: usize>(&self, at: &[(i64, i64); N]) -> [Point; N] {
        let mut points = [None; N];
        let missing = if self.memo {
            memo::read(self.key, at, &mut points)
        } else {
            (1u32 << N) - 1
        };
        if missing != 0 {
            for (n, (&(i, j), p)) in at.iter().zip(&mut points).enumerate() {
                if missing & (1 << n) != 0 {
                    *p = Some(self.point(i, j));
                }
            }
            if self.memo {
                memo::write(self.key, at, &points, missing);
            }
        }
        points.map(|p| p.expect("every point read or computed"))
    }

    /// The 16 lattice points around lattice cell `(i0, j0)`, point `a * 4 + b` at `(i0 - 1 + a,
    /// j0 - 1 + b)`, through the memo when it is on.
    fn points(&self, cell: (i64, i64)) -> WindowPoints {
        let last = self.memo.then(|| {
            memo::with_last(self.key, |last| {
                last.as_ref().filter(|l| l.cell == cell).map(|l| l.points)
            })
        });
        last.flatten()
            .unwrap_or_else(|| self.window_at(&window_coords(cell.0, cell.1)))
    }

    /// The lattice cell of chunk column `(cx, cz)` (in chunks).
    const fn cell_of(&self, (cx, cz): (i64, i64)) -> (i64, i64) {
        let s = self.shift - CHUNK_SHIFT;
        (cx >> s, cz >> s)
    }

    /// The last window of the memo, if it is in the lattice cell of chunk column `column`: its
    /// points and the column's bounds, which it computes from the cell's kinds when the last one
    /// was another chunk column of the cell (`None` when they need another cell's window: a
    /// carver's apron across the cell's border).
    fn last_window<R>(&self, column: (i64, i64), f: impl FnOnce(&memo::Last) -> R) -> Option<R> {
        if !self.memo {
            return None;
        }
        let cell = self.cell_of(column);
        memo::with_last(self.key, |last| {
            let last = last.as_mut().filter(|l| l.cell == cell)?;
            if last.column != column {
                last.bounds = self.bounds_in_cell(column, &last.kinds)?;
                last.column = column;
            }
            Some(f(last))
        })
    }

    /// The bounds of chunk column `column` (in chunks), through the memo when it is on (the last
    /// window's cell, without copying its points, when it is the column's).
    fn bounds(&self, column: (i64, i64)) -> LocalBounds {
        self.last_window(column, |l| l.bounds)
            .unwrap_or_else(|| self.window(column).1)
    }

    /// The window of chunk column `column` (in chunks): its lattice cell's 16 points
    /// ([`RegionGenerator::points`]) and its bounds, through the memo when it is on.
    fn window(&self, column: (i64, i64)) -> (WindowPoints, LocalBounds) {
        if let Some(window) = self.last_window(column, |l| (l.points, l.bounds)) {
            return window;
        }
        let cell = self.cell_of(column);
        let points = self.window_at(&window_coords(cell.0, cell.1));
        let kinds = WindowKinds::of(&points);
        let bounds = self.bounds_of(column, &kinds);
        if self.memo {
            memo::with_last(self.key, |last| {
                *last = Some(memo::Last {
                    cell,
                    points,
                    kinds,
                    column,
                    bounds,
                });
            });
        }
        (points, bounds)
    }

    /// The span of the heights (in [`HeightQ`], unclamped) of the columns of `area`, inside a
    /// lattice cell whose window holds `kinds`: per kind its [`TerrainKind::column_bounds`] over
    /// `area`, else the bounds of its points, then widened by the shape modifiers' column effects
    /// there.
    fn ground_span(&self, area: Rect, kinds: &WindowKinds) -> (i64, i64) {
        let (mut lo, mut hi) = (i64::MAX, i64::MIN);
        for (kind, point_lo, point_hi) in kinds.iter() {
            let k = &self.kinds[kind as usize];
            let (a, b) = match k.kind.column_bounds(&self.cx, k.seed, area) {
                Some(b) => (clamp_height(b.lo), clamp_height(b.hi)),
                None => (point_lo, point_hi),
            };
            lo = lo.min(a.into());
            hi = hi.max(b.into());
        }
        let (mut lower, mut raise) = (0, 0);
        for m in &self.modifiers {
            let effect = m.modifier.column_effect(&self.cx, m.seed, area);
            if cfg!(debug_assertions) {
                self.check_column_effect(m, area, effect);
            }
            lower += i64::from(effect.lower());
            raise += i64::from(effect.raise());
        }
        (lo - lower, hi + raise)
    }

    /// The debug check of a modifier's column effect: inside its declared effect, which the
    /// realm's vertical range counts on.
    fn check_column_effect(&self, m: &RealmModifier<Cx>, area: Rect, effect: ShapeEffect) {
        assert!(
            effect.within(m.effect),
            "{NAME} span check: realm {} shape modifier {} gives the columns {area:?} the effect \
             {effect:?}, outside its declared effect {:?}",
            self.realm(),
            m.id,
            m.effect,
        );
    }

    /// The bounds of chunk column `column` (in chunks), whose lattice cell's window holds
    /// `kinds`; a carver's apron across the cell's border reads the windows of the cells it
    /// reaches.
    fn bounds_of(&self, column: (i64, i64), kinds: &WindowKinds) -> LocalBounds {
        let area = chunk_rect(column);
        let span = self.ground_span(area, kinds);
        let apron = self.carve.filter(|c| c.apron > 0).map(|c| {
            let a = i64::from(c.apron);
            let wide = Rect::new(area.x0 - a, area.z0 - a, area.x1 + a, area.z1 + a);
            let (s, own) = (self.shift, self.cell_of(column));
            let mut lo = i64::MAX;
            for i in wide.x0 >> s..=wide.x1 >> s {
                for j in wide.z0 >> s..=wide.z1 >> s {
                    let part = Rect::new(
                        (i << s).max(wide.x0),
                        (j << s).max(wide.z0),
                        (((i + 1) << s) - 1).min(wide.x1),
                        (((j + 1) << s) - 1).min(wide.z1),
                    );
                    let span = if (i, j) == own {
                        self.ground_span(part, kinds)
                    } else {
                        self.ground_span(part, &WindowKinds::of(&self.points((i, j))))
                    };
                    lo = lo.min(span.0);
                }
            }
            lo
        });
        self.assemble(span, apron, self.column_height(area))
    }

    /// [`RegionGenerator::bounds_of`] from the kinds of the chunk column's own lattice cell alone,
    /// `None` if a carver's apron reaches another cell (it reads that cell's window, from the
    /// memo).
    fn bounds_in_cell(&self, column: (i64, i64), kinds: &WindowKinds) -> Option<LocalBounds> {
        let area = chunk_rect(column);
        let span = self.ground_span(area, kinds);
        let apron = match self.carve.filter(|c| c.apron > 0) {
            None => None,
            Some(c) => {
                let a = i64::from(c.apron);
                let wide = Rect::new(area.x0 - a, area.z0 - a, area.x1 + a, area.z1 + a);
                let s = self.shift;
                let inside = wide.x0 >> s == wide.x1 >> s && wide.z0 >> s == wide.z1 >> s;
                if !inside {
                    return None;
                }
                Some(self.ground_span(wide, kinds).0)
            }
        };
        Some(self.assemble(span, apron, self.column_height(area)))
    }

    /// The most blocks the feature stages write over the ground at the columns of `area` (a chunk
    /// column): the largest of their [`FeatureStage::column_height`] there, 0 without them.
    ///
    /// [`FeatureStage::column_height`]: super::FeatureStage::column_height
    fn column_height(&self, area: Rect) -> u32 {
        let mut most = 0;
        for f in &self.features {
            let h = f.stage.column_height(&self.cx, f.seed, area);
            if cfg!(debug_assertions) {
                let height = f.stage.reach().height;
                assert!(
                    h <= height,
                    "{NAME} span check: realm {} feature stage {} gives the columns {area:?} a \
                     height of {h}, over its declared height {height}",
                    self.realm(),
                    f.id,
                );
            }
            most = most.max(h);
        }
        most
    }

    /// The bounds of a chunk column whose heights span `(lo, hi)` (in [`HeightQ`], unclamped),
    /// with the lowest height `apron` of the carvers' apron around it, if they have one, and
    /// features up to `grow` blocks over its ground.
    fn assemble(&self, (lo, hi): (i64, i64), apron: Option<i64>, grow: u32) -> LocalBounds {
        let spec = self.spec();
        let block = |h: i64| block_of(spec.datum, clamp_height(h));
        let (ground_lo, ground_hi) = (block(lo), block(hi));
        let over = self.reach.rise.max(grow);
        let mut top = ground_hi.saturating_add(over.into());
        if let FluidRule::SeaLevel { level, .. } = spec.fluid
            && ground_lo < level
        {
            top = top.max(level);
        }
        let apron_lo = apron.map_or(ground_lo, |a| ground_lo.min(block(a)));
        let mut deep = ground_lo.saturating_sub(self.reach.depth.into());
        if let Some(carve) = self.carve {
            deep = deep.min(apron_lo.saturating_sub(carve.surface.into()));
        }
        LocalBounds {
            ground_lo,
            ground_hi,
            top,
            deep,
            apron_lo,
        }
    }

    /// The bounds of chunk column `(cx, cz)` (in chunks), after its selection reads.
    pub fn local_bounds(&self, cx: i64, cz: i64) -> LocalBounds {
        self.bounds((cx, cz))
    }

    /// The heights of the grid `xs` x `zs`, all in lattice cell `(i0, j0)` whose window holds
    /// `points`, into `out` (`i * zs.len() + j`): the kinds' heights blended, then the shape
    /// modifiers. `area` is the chunk column the grid lies in, if one: in debug builds the
    /// modifiers are held to their effect there ([`ShapeModifier::column_effect`]), else to their
    /// declared one.
    fn heights_in(
        &self,
        (xs, zs): (&[i64], &[i64]),
        fields: &Cx::Grid,
        points: &[Point; WINDOW_POINTS],
        area: Option<Rect>,
        out: &mut [HeightQ],
    ) {
        self.blend_in((xs, zs), fields, points, out);
        self.shape((xs, zs), fields, area, out);
    }

    /// The shape modifiers over the heights `out` of the grid `xs` x `zs`, in their order. In debug
    /// builds each one is held to its effect over `area` (the chunk column of the grid), or to its
    /// declared effect.
    fn shape(
        &self,
        (xs, zs): (&[i64], &[i64]),
        fields: &Cx::Grid,
        area: Option<Rect>,
        out: &mut [HeightQ],
    ) {
        for m in &self.modifiers {
            let before = cfg!(debug_assertions).then(|| out.to_vec());
            let grid = KindGrid {
                cx: &self.cx,
                fields,
                xs,
                zs,
                seed: m.seed,
            };
            m.modifier.apply(&grid, out);
            if let Some(before) = before {
                let effect =
                    area.map_or(m.effect, |a| m.modifier.column_effect(&self.cx, m.seed, a));
                self.check_effect(m, effect, (xs, zs), &before, out);
            }
        }
    }

    /// The debug check of a shape modifier's effect: every height it left is one `effect` (its
    /// declared effect, or its effect over the grid's chunk column) allows (the column bounds and
    /// the far LOD count on it).
    pub(super) fn check_effect(
        &self,
        m: &RealmModifier<Cx>,
        effect: ShapeEffect,
        (xs, zs): (&[i64], &[i64]),
        before: &[HeightQ],
        after: &[HeightQ],
    ) {
        let what = if effect == m.effect {
            "declared"
        } else {
            "column"
        };
        for (n, (&b, &a)) in before.iter().zip(after).enumerate() {
            assert!(
                effect.allows(b, a),
                "{NAME} span check: realm {} shape modifier {} turned the height {b} of column \
                 ({}, {}) into {a}, outside its {what} effect {effect:?}",
                self.realm(),
                m.id,
                xs[n / zs.len()],
                zs[n % zs.len()],
            );
        }
    }

    /// The blended heights of the grid `xs` x `zs` (see [`RegionGenerator::heights_in`]).
    fn blend_in(
        &self,
        (xs, zs): (&[i64], &[i64]),
        fields: &Cx::Grid,
        points: &[Point; WINDOW_POINTS],
        out: &mut [HeightQ],
    ) {
        let n = xs.len() * zs.len();
        if n == 0 {
            return;
        }
        let window = Window(points.map(|p| p.kind));
        let grid = |seed| KindGrid {
            cx: &self.cx,
            fields,
            xs,
            zs,
            seed,
        };
        if let Some(k) = window.single() {
            // The interior fast path: the kind's heights are the blend's, bit for bit.
            let k = &self.kinds[k as usize];
            let all = ALL
                .get(..n)
                .map_or_else(|| Cow::Owned(vec![true; n]), Cow::Borrowed);
            k.kind.heights(&grid(k.seed), &all, out);
            return;
        }
        let present = window.present();
        let l = 1i64 << self.shift;
        let m = l - 1;
        let fz: Vec<i64> = zs.iter().map(|&z| z & m).collect();
        // `c_k = (L - fz) a_k(x) + fz b_k(x)`, with `a_k` and `b_k` the counts of the cell's two
        // sides along z weighted along x: per kind and x, `(a_k, b_k)` at `ab[q * nx + i]`.
        let ab: Vec<(i64, i64)> = present
            .iter()
            .flat_map(|&(_, n)| {
                xs.iter().map(move |&x| {
                    let fx = x & m;
                    (n[0] * (l - fx) + n[1] * fx, n[2] * (l - fx) + n[3] * fx)
                })
            })
            .collect();
        let mut mask = vec![false; n];
        let mut height = vec![0; n];
        let mut sums = vec![0i64; n];
        for (&(kind, _), ab) in present.iter().zip(ab.chunks_exact(xs.len())) {
            for (row, &(a, b)) in mask.chunks_exact_mut(zs.len()).zip(ab) {
                for (set, &fz) in row.iter_mut().zip(&fz) {
                    *set = (l - fz) * a + fz * b > 0;
                }
            }
            let k = &self.kinds[kind as usize];
            k.kind.heights(&grid(k.seed), &mask, &mut height);
            let rows = sums
                .chunks_exact_mut(zs.len())
                .zip(height.chunks_exact(zs.len()));
            for ((sum, h), &(a, b)) in rows.zip(ab) {
                for ((sum, &h), &fz) in sum.iter_mut().zip(h).zip(&fz) {
                    // A weight of 0 hides whatever the kind left there.
                    *sum += ((l - fz) * a + fz * b) * i64::from(h);
                }
            }
        }
        for (o, &sum) in out.iter_mut().zip(&sums) {
            *o = blend::blend(sum, self.shift);
        }
    }

    /// The heights of chunk column `(cx, cz)` (in chunks), per column `x << 5 | z`: blended, then
    /// shaped by the modifiers.
    pub fn chunk_heights(&self, cx: i64, cz: i64, out: &mut [HeightQ; CHUNK_AREA]) {
        let (x0, z0) = (cx << CHUNK_SHIFT, cz << CHUNK_SHIFT);
        let (xs, zs) = chunk_axes(x0, z0);
        let fields = self.cx.grid(&xs, &zs);
        let points = self.points(self.cell_of((cx, cz)));
        let area = Some(chunk_rect((cx, cz)));
        self.heights_in((&xs, &zs), &fields, &points, area, out);
    }

    /// The grounds and fluid tops of the columns of the grid `xs` x `zs` (point `i * zs.len() +
    /// j`), bit for bit as their own chunks compute them, however many lattice cells and chunk
    /// columns the grid crosses (each column with its own lattice cell's window): what a stage
    /// reads of the columns around its chunk ([`ChunkColumns::tops`]).
    ///
    /// # Panics
    ///
    /// If `ground` or `fluid_top` is not one entry per column of the grid.
    pub fn column_tops(&self, xs: &[i64], zs: &[i64], ground: &mut [i64], fluid_top: &mut [i64]) {
        let n = xs.len() * zs.len();
        assert!(
            ground.len() == n && fluid_top.len() == n,
            "{NAME}::column_tops: one ground and one fluid top per column"
        );
        let mut h = vec![0; n];
        self.heights_across(xs, zs, &mut h);
        let (level, datum) = (self.fluid_level(), self.spec().datum);
        for ((g, w), &h) in ground.iter_mut().zip(fluid_top.iter_mut()).zip(&h) {
            *g = block_of(datum, h);
            *w = if *g < level { level } else { *g };
        }
    }

    /// The heights of the columns of the grid `xs` x `zs` into `out` (point `i * zs.len() + j`),
    /// bit for bit as their own chunks compute them, each column with its own lattice cell's
    /// window: blended, then shaped by the modifiers.
    fn heights_across(&self, xs: &[i64], zs: &[i64], out: &mut [HeightQ]) {
        let (ci, cj) = (cells_of(xs, self.shift), cells_of(zs, self.shift));
        let (mut sx, mut sz, mut h) = (Vec::new(), Vec::new(), Vec::new());
        for (i, xi) in &ci {
            sx.clear();
            sx.extend(xi.iter().map(|&a| xs[a]));
            for (j, zj) in &cj {
                sz.clear();
                sz.extend(zj.iter().map(|&b| zs[b]));
                let fields = self.cx.grid(&sx, &sz);
                h.clear();
                h.resize(sx.len() * sz.len(), 0);
                self.heights_in((&sx, &sz), &fields, &self.points((*i, *j)), None, &mut h);
                for (row, &a) in h.chunks_exact(sz.len()).zip(xi) {
                    for (&h, &b) in row.iter().zip(zj) {
                        out[a * zs.len() + b] = h;
                    }
                }
            }
        }
    }

    /// The `y` of the realm's fluid level (`FluidRule::top`, with the rule read once): `i64::MIN`
    /// without fluid.
    fn fluid_level(&self) -> i64 {
        match self.spec().fluid {
            FluidRule::SeaLevel { level, .. } => level,
            FluidRule::None => i64::MIN,
        }
    }

    /// The dominant kind of column `(x, z)`: the largest blend weight, ties to the smaller id
    /// (the biome table's input). Selection reads only, no height.
    pub fn dominant_kind(&self, x: i64, z: i64) -> &GenId {
        let points = self.points((x >> self.shift, z >> self.shift));
        let m = (1i64 << self.shift) - 1;
        let window = Window(points.map(|p| p.kind));
        let f = blend::factors(x & m, z & m, self.shift);
        &self.kinds[blend::dominant(&window.present(), f) as usize].id
    }

    /// The column at block `(x, z)`: its height, ground, fluid, dominant kind and its chunk
    /// column's bounds. The same bits as the chunk path.
    pub fn column_info(&self, x: i64, z: i64) -> ColumnInfo {
        let column = (x >> CHUNK_SHIFT, z >> CHUNK_SHIFT);
        let (points, bounds) = self.window(column);
        let fields = self.cx.grid(&[x], &[z]);
        let mut h = [0];
        let area = Some(chunk_rect(column));
        self.heights_in((&[x], &[z]), &fields, &points, area, &mut h);
        let spec = self.spec();
        let ground = block_of(spec.datum, h[0]);
        let m = (1i64 << self.shift) - 1;
        let window = Window(points.map(|p| p.kind));
        let kind = blend::dominant(&window.present(), blend::factors(x & m, z & m, self.shift));
        if cfg!(debug_assertions) {
            // The chunk of a ground outside its bounds may take a fast path, which checks nothing.
            self.check_ground((x, z), ground, &points, &bounds);
            self.check_vertical((x, z), &bounds);
        }
        ColumnInfo {
            realm: self.realm().clone(),
            height: h[0],
            ground,
            fluid_top: spec.fluid.top(ground),
            kind: Some(self.kinds[kind as usize].id.clone()),
            ground_lo: bounds.ground_lo,
            top: bounds.top,
        }
    }

    /// The chunk at `key` through the column stage, without the fast paths over the local top
    /// and under the deep bound: what [`WorldGenerator::generate`] must equal, for tests of a
    /// realm's bounds (a game's or a mod's kinds, modifiers and carvers included).
    pub fn generate_full(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let origin = key.pos.origin();
        if !self.spec().span.contains(origin.y) {
            return ChunkData::uniform(BlockState::AIR);
        }
        let window = self.window((key.pos.x, key.pos.z));
        self.column_stage(origin, &window, &self.stages(registry, false), registry)
    }

    /// The chunk at `key` without the carvers and the feature stages: the terrain the far LOD
    /// draws (the fill, the fluid and the surface stage), which the exported harness compares the
    /// coarse cells with (`check_lod`).
    #[cfg(feature = "test-support")]
    pub fn generate_terrain(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let origin = key.pos.origin();
        if !self.spec().span.contains(origin.y) {
            return ChunkData::uniform(BlockState::AIR);
        }
        let window = self.window((key.pos.x, key.pos.z));
        let stages = Stages {
            carvers: &[],
            features: &[],
            ..self.stages(registry, false)
        };
        if origin.y > window.1.top {
            ChunkData::uniform(BlockState::AIR)
        } else if origin.y + i64::from(CHUNK_SIZE) - 1 <= window.1.deep {
            super::decorate::deep(&self.cx, origin, window.1.apron_lo, &stages, registry)
        } else {
            self.column_stage(origin, &window, &stages, registry)
        }
    }

    /// The stages of a chunk, with the realm's floor and fluid states (the fluid only where the
    /// chunk may hold it: a deep chunk reads it only to carve around it). The feature stages never
    /// run in a deep chunk (nothing they write lies under a ground).
    fn stages(&self, registry: &BlockRegistry, deep: bool) -> Stages<'_, Cx> {
        let spec = self.spec();
        let fluid = match &spec.fluid {
            FluidRule::SeaLevel { fluid, .. } if !deep || !self.carvers.is_empty() => {
                block(registry, fluid, NAME)
            }
            _ => BlockState::AIR,
        };
        Stages {
            fill: &*self.fill,
            surface: &*self.surface,
            carvers: &self.carvers,
            features: &self.features,
            floor: block(registry, &spec.floor, NAME),
            fluid,
        }
    }

    /// The chunk at `origin` from its columns, with `stages`.
    fn column_stage(
        &self,
        origin: BlockPos,
        (points, bounds): &(WindowPoints, LocalBounds),
        stages: &Stages<'_, Cx>,
        registry: &BlockRegistry,
    ) -> ChunkData {
        let (xs, zs) = chunk_axes(origin.x, origin.z);
        let fields = self.cx.grid(&xs, &zs);
        let mut h = [0; CHUNK_AREA];
        let area = Some(chunk_rect((
            origin.x >> CHUNK_SHIFT,
            origin.z >> CHUNK_SHIFT,
        )));
        self.heights_in((&xs, &zs), &fields, points, area, &mut h);
        let spec = self.spec();
        let ground = h.map(|h| block_of(spec.datum, h));
        let level = self.fluid_level();
        let fluid_top = ground.map(|g| if g < level { level } else { g });
        if cfg!(debug_assertions) {
            self.check_grounds(origin, points, bounds, &ground);
            self.check_apron(origin, bounds);
            if self.slope_check {
                self.check_slope(origin, &h);
            }
        }
        let around = Around(self);
        let columns = ChunkColumns::new(origin, &ground, &fluid_top, &self.cx, &fields, &around)
            .with_apron_lo(bounds.apron_lo);
        let chunk = super::decorate::decorate(&columns, stages, registry);
        if cfg!(debug_assertions) {
            self.check_top(origin, bounds, &chunk);
            self.check_vertical((origin.x, origin.z), bounds);
        }
        chunk
    }

    /// The deep path: the fill alone, carved by the carvers without columns (with the chunk
    /// column's `apron_lo`).
    fn deep(&self, origin: BlockPos, apron_lo: i64, registry: &BlockRegistry) -> ChunkData {
        let stages = self.stages(registry, true);
        super::decorate::deep(&self.cx, origin, apron_lo, &stages, registry)
    }

    /// The debug span check of the grounds: each lies between the bounds of its chunk column.
    fn check_grounds(
        &self,
        origin: BlockPos,
        points: &[Point; WINDOW_POINTS],
        bounds: &LocalBounds,
        ground: &[i64; CHUNK_AREA],
    ) {
        for (c, &g) in ground.iter().enumerate() {
            let column = (
                origin.x + (c >> CHUNK_SHIFT) as i64,
                origin.z + (c & (CHUNK_SIZE as usize - 1)) as i64,
            );
            self.check_ground(column, g, points, bounds);
        }
    }

    /// The debug span check of the ground `g` of column `(x, z)`: it lies between the bounds of
    /// its chunk column (its kinds' and modifiers', from the window's points).
    fn check_ground(
        &self,
        (x, z): (i64, i64),
        g: i64,
        points: &[Point; WINDOW_POINTS],
        bounds: &LocalBounds,
    ) {
        assert!(
            (bounds.ground_lo..=bounds.ground_hi).contains(&g),
            "{NAME} span check: realm {} column ({x}, {z}) has its ground at y {g}, outside the \
             bounds {}..={} of its chunk column, from its kinds ({}) and shape modifiers: a \
             kind's bounds or column bounds are under its heights, or a modifier cuts or raises \
             more than its column effect says",
            self.realm(),
            bounds.ground_lo,
            bounds.ground_hi,
            self.window_kinds(points),
        );
    }

    /// The debug span check of the carvers' apron: every ground they may read, the chunk's and
    /// their widest apron's, lies at or over the chunk column's `apron_lo`, the bound they get
    /// ([`CarveChunk::apron_lo`](super::CarveChunk::apron_lo)) and that `deep` is built from (so
    /// a chunk at or under `deep` lies their surface reach under all of them). The grounds' own
    /// check covers the chunk's columns; this one covers the apron, whose bounds come from the
    /// hooks over rects that are no chunk column ([`TerrainKind::column_bounds`],
    /// [`ShapeModifier::column_effect`]).
    fn check_apron(&self, origin: BlockPos, bounds: &LocalBounds) {
        let Some(carve) = self.carve.filter(|c| c.apron > 0) else {
            return;
        };
        let (o, a, size) = (origin, i64::from(carve.apron), i64::from(CHUNK_SIZE));
        // The ring around the chunk column: the -x and +x strips over the whole side, then the
        // -z and +z ones between them.
        let strips = [
            (o.x - a..o.x, o.z - a..o.z + size + a),
            (o.x + size..o.x + size + a, o.z - a..o.z + size + a),
            (o.x..o.x + size, o.z - a..o.z),
            (o.x..o.x + size, o.z + size..o.z + size + a),
        ];
        for (xs, zs) in strips {
            let (xs, zs): (Vec<i64>, Vec<i64>) = (xs.collect(), zs.collect());
            let n = xs.len() * zs.len();
            let (mut ground, mut fluid) = (vec![0; n], vec![0; n]);
            self.column_tops(&xs, &zs, &mut ground, &mut fluid);
            for (k, &g) in ground.iter().enumerate() {
                assert!(
                    bounds.apron_lo <= g,
                    "{NAME} span check: realm {} column ({}, {}), in the apron of {a} blocks its \
                     carvers read around the chunk at ({}, {}, {}), has its ground at y {g}, \
                     under the chunk column's apron_lo {} that its carvers get and its deep bound \
                     {} is built from: a kind's column bounds or a shape modifier's column effect \
                     over the apron around the chunk column are under its heights",
                    self.realm(),
                    xs[k / zs.len()],
                    zs[k % zs.len()],
                    o.x,
                    o.y,
                    o.z,
                    bounds.apron_lo,
                    bounds.deep,
                );
            }
        }
    }

    /// The debug slope check of a chunk column's heights `h` (ADR 0039 sec. 6): every two axis
    /// neighbors, inside the chunk column and across its +x and +z borders (so each border pair
    /// is checked once), differ by at most `MAX_SLOPE * HEIGHT_ONE` (768, 3 blocks), and so their
    /// grounds by at most 3 blocks.
    ///
    /// The rule is exact. The freeze check holds every axis step of the heights to a real bound
    /// `B <= 768` that counts every rounding as declared jitter, but for the blend's own floor,
    /// which adds less than 1 (a capped modifier's cap counts too): two integer heights then
    /// differ by less than `B + 1`, so by at most 768. A ground is `datum + (h >> 8)`, and two
    /// heights at most 768 apart give grounds at most 3 apart. A kind or a shape modifier whose
    /// declared slope is under its heights' fails here, in any chunk where the realm passes 3:1.
    fn check_slope(&self, origin: BlockPos, h: &[HeightQ; CHUNK_AREA]) {
        let size = CHUNK_SIZE as usize;
        let cap = i64::from(MAX_SLOPE) * i64::from(HEIGHT_ONE);
        let (xs, zs) = chunk_axes(origin.x, origin.z);
        let (mut px, mut pz) = (vec![0; size], vec![0; size]);
        self.heights_across(&[origin.x + size as i64], &zs, &mut px);
        self.heights_across(&xs, &[origin.z + size as i64], &mut pz);
        // Column `(x, z)` of the chunk column, `x` or `z` 32 for the +x or +z neighbor.
        let at = |x: usize, z: usize| match (x == size, z == size) {
            (false, false) => h[(x << CHUNK_SHIFT) | z],
            (true, _) => px[z],
            (_, true) => pz[x],
        };
        for x in 0..size {
            for z in 0..size {
                for (nx, nz) in [(x + 1, z), (x, z + 1)] {
                    let (a, b) = (at(x, z), at(nx, nz));
                    assert!(
                        (i64::from(a) - i64::from(b)).abs() <= cap,
                        "{NAME} slope check: realm {} columns ({}, {}) and ({}, {}) have the \
                         heights {a} and {b} (in 1/256 block), more than {MAX_SLOPE} blocks apart: \
                         a kind's or a shape modifier's declared slope (or a capped modifier's \
                         cap) is under its heights' slope",
                        self.realm(),
                        origin.x + x as i64,
                        origin.z + z as i64,
                        origin.x + nx as i64,
                        origin.z + nz as i64,
                    );
                }
            }
        }
    }

    /// The debug span check of the realm's range: the column's top lies in the realm's vertical
    /// range (inside its span), which the world takes for air above without asking.
    fn check_vertical(&self, (x, z): (i64, i64), bounds: &LocalBounds) {
        let span_top = *self.spec().span.chunk_layers().end();
        let layer = (bounds.top >> CHUNK_SHIFT).min(span_top);
        assert!(
            layer <= *self.vertical.end(),
            "{NAME} span check: realm {} column ({x}, {z}) may hold blocks up to y {}, over its \
             vertical range (chunk layers up to {}) that the kinds' ranges give: a kind's range \
             is under its bounds",
            self.realm(),
            bounds.top,
            self.vertical.end(),
        );
    }

    /// The debug span check of the top: nothing but air over the column's top.
    fn check_top(&self, origin: BlockPos, bounds: &LocalBounds, chunk: &ChunkData) {
        let from = (bounds.top.saturating_add(1) - origin.y).clamp(0, i64::from(CHUNK_SIZE));
        if chunk.as_uniform() == Some(BlockState::AIR) {
            return;
        }
        for ly in from as u32..CHUNK_SIZE as u32 {
            for x in 0..CHUNK_SIZE as u32 {
                for z in 0..CHUNK_SIZE as u32 {
                    let state = chunk.get(LocalPos::new(x, ly, z));
                    assert!(
                        state == BlockState::AIR,
                        "{NAME} span check: realm {} has a block at ({}, {}, {}) over the top \
                         {} of its chunk column: a kind's bounds, the surface stage's rise or a \
                         feature stage's column height are under its blocks",
                        self.realm(),
                        origin.x + i64::from(x),
                        origin.y + i64::from(ly),
                        origin.z + i64::from(z),
                        bounds.top,
                    );
                }
            }
        }
    }

    /// The kinds of a window, for messages.
    fn window_kinds(&self, points: &[Point; WINDOW_POINTS]) -> String {
        let mut kinds: Vec<u32> = points.iter().map(|p| p.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        let names: Vec<&str> = kinds
            .iter()
            .map(|&k| self.kinds[k as usize].id.as_str())
            .collect();
        names.join(", ")
    }
}

/// The checks of the block names of realm `realm`'s spec against `blocks`: its floor (air
/// included: the block registry always holds it) and its fluid.
pub fn check_realm_blocks(
    checks: &mut Checks,
    blocks: &BlockRegistry,
    realm: &GenId,
    spec: &RealmSpec,
) {
    checks.block(blocks, &spec.floor, realm);
    if let FluidRule::SeaLevel { fluid, .. } = &spec.fluid {
        checks.block(blocks, fluid, realm);
    }
}

/// Sorts the stages of one phase of a decoration by `(phase, priority, id)`, as a registry orders
/// its entries (the higher priority first), and records in `checks` every id given more than once
/// (`registry` names the kind of stage).
fn sort_stages<T: GenEntry + ?Sized>(
    checks: &mut Checks,
    registry: &'static str,
    stages: &mut [(GenId, Arc<T>)],
) {
    stages.sort_by(|(a, sa), (b, sb)| {
        let key = |s: &Arc<T>| {
            let o = s.order();
            (o.phase, std::cmp::Reverse(o.priority))
        };
        key(sa).cmp(&key(sb)).then_with(|| a.cmp(b))
    });
    let mut ids: Vec<&GenId> = stages.iter().map(|(id, _)| id).collect();
    ids.sort();
    for run in ids.chunk_by(|a, b| a == b).filter(|r| r.len() > 1) {
        checks.push(Problem::Duplicate {
            registry,
            id: run[0].clone(),
            count: run.len(),
        });
    }
}

/// The columns around a chunk, from the generator ([`RegionGenerator::column_tops`]).
struct Around<'g, Cx: Context>(&'g RegionGenerator<Cx>);

impl<Cx: Context> ColumnsAround for Around<'_, Cx> {
    fn tops(&self, xs: &[i64], zs: &[i64], ground: &mut [i64], fluid_top: &mut [i64]) {
        self.0.column_tops(xs, zs, ground, fluid_top);
    }
}

/// The lattice cells of the coordinates `v` on a lattice of `2^shift`, each with the indices of
/// its coordinates, in the order the cells first appear.
fn cells_of(v: &[i64], shift: u32) -> Vec<(i64, Vec<usize>)> {
    let mut cells: Vec<(i64, Vec<usize>)> = Vec::new();
    for (n, &c) in v.iter().enumerate() {
        let cell = c >> shift;
        match cells.iter_mut().find(|(k, _)| *k == cell) {
            Some((_, at)) => at.push(n),
            None => cells.push((cell, vec![n])),
        }
    }
    cells
}

/// The 16 lattice points of the window of lattice cell `(i0, j0)`: point `a * 4 + b` at
/// `(i0 - 1 + a, j0 - 1 + b)`.
fn window_coords(i0: i64, j0: i64) -> [(i64, i64); WINDOW_POINTS] {
    std::array::from_fn(|n| (i0 - 1 + (n / 4) as i64, j0 - 1 + (n % 4) as i64))
}

/// The columns of chunk column `(cx, cz)` (in chunks).
const fn chunk_rect((cx, cz): (i64, i64)) -> Rect {
    let size = CHUNK_SIZE as i64;
    let (x0, z0) = (cx << CHUNK_SHIFT, cz << CHUNK_SHIFT);
    Rect::new(x0, z0, x0 + size - 1, z0 + size - 1)
}

/// `v` clamped into [`HeightQ`].
pub(super) fn clamp_height(v: i64) -> HeightQ {
    v.clamp(i64::from(HeightQ::MIN), i64::from(HeightQ::MAX)) as HeightQ
}

/// The 32 x and z of the chunk column whose lowest corner is `(x0, z0)`.
fn chunk_axes(x0: i64, z0: i64) -> ([i64; CHUNK_SIZE as usize], [i64; CHUNK_SIZE as usize]) {
    (
        std::array::from_fn(|i| x0 + i as i64),
        std::array::from_fn(|i| z0 + i as i64),
    )
}

/// The chunk layers of a realm that may hold something not air: from its span's bottom up to its
/// highest kind height plus the surface rise or the features' height `grow`, or its fluid, inside
/// its span.
fn vertical<Cx: Context>(
    spec: &RealmSpec,
    cx: &Cx,
    kinds: &[RealmKind<Cx>],
    (reach, grow): (SurfaceReach, u32),
    (lower, raise): (i64, i64),
) -> RangeInclusive<i64> {
    let span = spec.span.chunk_layers();
    let (lo, hi) = kinds.iter().fold((i64::MAX, i64::MIN), |(lo, hi), k| {
        let r = k.kind.range(cx);
        (lo.min(r.lo), hi.max(r.hi))
    });
    let (lo, hi) = (lo.saturating_sub(lower), hi.saturating_add(raise));
    let clamp = clamp_height;
    let mut top = block_of(spec.datum, clamp(hi)).saturating_add(reach.rise.max(grow).into());
    if let FluidRule::SeaLevel { level, .. } = spec.fluid
        && block_of(spec.datum, clamp(lo)) < level
    {
        top = top.max(level);
    }
    *span.start()..=(top >> CHUNK_SHIFT).min(*span.end())
}

impl<Cx: Context> WorldGenerator for RegionGenerator<Cx> {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let origin = key.pos.origin();
        if !self.spec().span.contains(origin.y) {
            return ChunkData::uniform(BlockState::AIR);
        }
        let column = (key.pos.x, key.pos.z);
        let fast = |bounds: &LocalBounds| {
            if origin.y > bounds.top {
                Some(ChunkData::uniform(BlockState::AIR))
            } else if origin.y + i64::from(CHUNK_SIZE) - 1 <= bounds.deep {
                Some(self.deep(origin, bounds.apron_lo, registry))
            } else {
                None
            }
        };
        // The chunks of the last window's chunk column read its bounds alone, those of its lattice
        // cell its kinds.
        if self.memo
            && let Some(chunk) =
                fast(&memo::last_bounds(self.key, column).unwrap_or_else(|| self.bounds(column)))
        {
            return chunk;
        }
        let window = self.window(column);
        if let Some(chunk) = fast(&window.1) {
            return chunk;
        }
        self.column_stage(origin, &window, &self.stages(registry, false), registry)
    }

    /// From the realm's span bottom to its highest kind height (plus the surface rise or the
    /// features' height, and the fluid), inside its span.
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        self.vertical.clone()
    }

    /// The region pass of the far LOD (ADR 0039 sec. 10): the realm's own source.
    fn lod(&self) -> Option<&dyn LodSource> {
        Some(self)
    }

    fn name(&self) -> &str {
        self.realm().as_str()
    }

    /// The low 32 bits of the digest of the realm's lines: its dimension's sub-digest when the
    /// realm is the dimension's only one.
    fn version(&self) -> u32 {
        Self::version(self)
    }

    /// The realm alone (inside a dimension, its [`DimensionGenerator`](super::DimensionGenerator)
    /// reports the whole stack).
    fn realms(&self) -> &RealmStack {
        &self.stack
    }

    fn as_query(&self) -> Option<&dyn WorldQuery> {
        Some(self)
    }
}

/// The realm's own view: every `y` is its realm.
impl<Cx: Context> WorldQuery for RegionGenerator<Cx> {
    fn height_at(&self, x: i64, z: i64) -> Option<i64> {
        Some(self.column_info(x, z).ground)
    }

    fn realm_at(&self, _y: i64) -> &GenId {
        self.realm()
    }

    /// The dominant kind's id: a placeholder until the biome table exists.
    fn biome_at(&self, x: i64, _y: i64, z: i64) -> Option<GenId> {
        Some(self.dominant_kind(x, z).clone())
    }

    fn column(&self, x: i64, z: i64) -> Option<ColumnInfo> {
        Some(self.column_info(x, z))
    }

    fn environment_at(&self, _x: i64, _y: i64, _z: i64) -> &GenId {
        &self.spec().env
    }
}
