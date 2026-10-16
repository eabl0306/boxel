//! [`RegionGenerator`]: one realm's terrain from its region map, kinds and decoration.

use super::blend::{self, WINDOW_POINTS, Window};
use super::decorate::{
    ChunkColumns, Decoration, FillStage, Rock, Stages, SurfaceReach, SurfaceStage,
};
use super::kind::{Context, HeightQ, KindGrid, TerrainKind, block_of};
use super::memo::{self, Point, WindowPoints};
use super::select::{self, RuleEntry, Selector, SelectorRule, check_rule_entries};
use super::{
    CODE_VERSION, DEFAULT_LATTICE_SHIFT, MAX_LATTICE_SHIFT, MIN_LATTICE_SHIFT, REGION_LINE, check,
};
use crate::query::{ColumnInfo, WorldQuery};
use crate::realm::{FluidRule, RealmSpec, RealmStack, check_floors};
use crate::registry::{
    BuildKey, Checks, DigestBuilder, DigestEntry, DigestScope, GenId, GenRegistry, Problem,
    RegistryDigest, hash_params,
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
    /// The fill and surface stages.
    pub decoration: Decoration<Cx>,
    /// The other knobs.
    pub settings: RegionSettings,
}

/// The vertical bounds of a chunk column of a realm (ADR 0039 sec. 14), from its 16 lattice
/// points: exact integers, every one a block `y`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalBounds {
    /// A block at or under the lowest ground of the column (the open surface): the realm's
    /// `ground_lo`. Nothing reads it yet; WG4 and the sky light per realm will.
    pub ground_lo: i64,
    /// A block at or over the highest ground.
    pub ground_hi: i64,
    /// The highest block that may not be air (ground, surface rise, fluid): every chunk over it
    /// is `Uniform` air, after the selection reads.
    pub top: i64,
    /// A chunk whose highest block is at or under this one lies under every column's surface
    /// stage: it takes the deep path, the fill alone, without the column stage.
    pub deep: i64,
}

/// A kind of the realm, ready to run.
struct RealmKind<Cx: Context> {
    id: GenId,
    kind: Arc<dyn TerrainKind<Cx>>,
    /// The realm's seed mixed with the kind's salt.
    seed: u64,
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
///   fluid, surface; `Uniform`, 1-bit or dense).
///
/// In debug builds every chunk of the column stage, and every point query, checks its column
/// bounds: each column's ground lies between the bounds of its 16 points, nothing but air lies
/// over the column's top, and that top lies in the realm's vertical range (the kinds' ranges). A
/// kind or a mod with a bound or a range under its heights fails its tests there.
pub struct RegionGenerator<Cx: Context> {
    key: BuildKey,
    dimension: GenId,
    /// The realm alone, as [`WorldGenerator::realms`] reports it.
    stack: RealmStack,
    cx: Cx,
    shift: u32,
    memo: bool,
    selector: Selector,
    kinds: Box<[RealmKind<Cx>]>,
    fill: Arc<dyn FillStage<Cx>>,
    surface: Arc<dyn SurfaceStage<Cx>>,
    reach: SurfaceReach,
    digest: RegistryDigest,
    vertical: RangeInclusive<i64>,
}

impl<Cx: Context> RegionGenerator<Cx> {
    /// The generator of `spec.realm`, with the Context `context` builds from the realm's seed,
    /// or `None` with every problem recorded in `checks`: the realm, the rules and entries the
    /// realm reaches, their kinds and parameters, the kinds' forms, limits, slopes and border
    /// reliefs, every added entry that names a missing rule ([`check_rule_entries`]), and every
    /// block name the realm (its floor and fluid) and its stages use
    /// ([`Problem::UnknownBlock`](crate::registry::Problem::UnknownBlock), so a typo fails at
    /// world open and never in a worker). Its digest lines ([`RegionGenerator::digest`]) are
    /// those of the realm: the realm, its `region` settings, its `context`, its rules and
    /// entries, its kinds and its stages.
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
        let Decoration { fill, surface } = decoration;
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
        if checks.found() > before {
            return None;
        }
        let selector = resolved.expect("no problem").selector;
        let reach = surface.1.reach();
        let vertical = vertical(&realm_spec, &cx, &realm_kinds, reach);
        Some(Self {
            key: BuildKey::next(),
            dimension: dimension.clone(),
            stack: RealmStack::of_one(realm.clone(), realm_spec),
            cx,
            shift,
            memo: settings.memo,
            selector,
            kinds: realm_kinds.into(),
            fill: fill.1,
            surface: surface.1,
            reach,
            digest: digest.finish(),
            vertical,
        })
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
        let clamp = |v: i64| v.clamp(i64::from(HeightQ::MIN), i64::from(HeightQ::MAX)) as i32;
        Point {
            kind,
            lo: clamp(b.lo),
            hi: clamp(b.hi),
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
    /// j0 - 1 + b)`, and their column's bounds, through the memo when it is on.
    fn window(&self, i0: i64, j0: i64) -> (WindowPoints, LocalBounds) {
        let at = std::array::from_fn(|n| (i0 - 1 + (n / 4) as i64, j0 - 1 + (n % 4) as i64));
        if !self.memo {
            let points = self.window_at(&at);
            return (points, self.bounds_of(&points));
        }
        let mut read = [None; WINDOW_POINTS];
        let missing = match memo::read_window(self.key, (i0, j0), &at, &mut read) {
            Ok(window) => return window,
            Err(missing) => missing,
        };
        for (n, (&(i, j), p)) in at.iter().zip(&mut read).enumerate() {
            if missing & (1 << n) != 0 {
                *p = Some(self.point(i, j));
            }
        }
        let points = read.map(|p| p.expect("every point read or computed"));
        let bounds = self.bounds_of(&points);
        memo::write_window(self.key, (i0, j0), &at, (&points, bounds), missing);
        (points, bounds)
    }

    /// The bounds of the column whose window holds `points`.
    fn bounds_of(&self, points: &[Point; WINDOW_POINTS]) -> LocalBounds {
        let lo = points.iter().map(|p| p.lo).min().expect("16 points");
        let hi = points.iter().map(|p| p.hi).max().expect("16 points");
        let spec = self.spec();
        let (ground_lo, ground_hi) = (block_of(spec.datum, lo), block_of(spec.datum, hi));
        let mut top = ground_hi.saturating_add(self.reach.rise.into());
        if let FluidRule::SeaLevel { level, .. } = spec.fluid
            && ground_lo < level
        {
            top = top.max(level);
        }
        LocalBounds {
            ground_lo,
            ground_hi,
            top,
            deep: ground_lo.saturating_sub(self.reach.depth.into()),
        }
    }

    /// The bounds of chunk column `(cx, cz)` (in chunks), after its selection reads.
    pub fn local_bounds(&self, cx: i64, cz: i64) -> LocalBounds {
        let s = self.shift - CHUNK_SHIFT;
        self.window(cx >> s, cz >> s).1
    }

    /// The blended heights of the grid `xs` x `zs`, all in lattice cell `(i0, j0)` whose window
    /// holds `points`, into `out` (`i * zs.len() + j`).
    fn heights_in(
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

    /// The blended heights of chunk column `(cx, cz)` (in chunks), per column `x << 5 | z`.
    pub fn chunk_heights(&self, cx: i64, cz: i64, out: &mut [HeightQ; CHUNK_AREA]) {
        let (x0, z0) = (cx << CHUNK_SHIFT, cz << CHUNK_SHIFT);
        let (xs, zs) = chunk_axes(x0, z0);
        let fields = self.cx.grid(&xs, &zs);
        let (points, _) = self.window(x0 >> self.shift, z0 >> self.shift);
        self.heights_in((&xs, &zs), &fields, &points, out);
    }

    /// The dominant kind of column `(x, z)`: the largest blend weight, ties to the smaller id
    /// (the biome table's input). Selection reads only, no height.
    pub fn dominant_kind(&self, x: i64, z: i64) -> &GenId {
        let (points, _) = self.window(x >> self.shift, z >> self.shift);
        let m = (1i64 << self.shift) - 1;
        let window = Window(points.map(|p| p.kind));
        let f = blend::factors(x & m, z & m, self.shift);
        &self.kinds[blend::dominant(&window.present(), f) as usize].id
    }

    /// The column at block `(x, z)`: its height, ground, fluid, dominant kind and its chunk
    /// column's bounds. The same bits as the chunk path.
    pub fn column_info(&self, x: i64, z: i64) -> ColumnInfo {
        let (points, bounds) = self.window(x >> self.shift, z >> self.shift);
        let fields = self.cx.grid(&[x], &[z]);
        let mut h = [0];
        self.heights_in((&[x], &[z]), &fields, &points, &mut h);
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
    /// realm's bounds (a game's or a mod's kinds included).
    pub fn generate_full(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let origin = key.pos.origin();
        if !self.spec().span.contains(origin.y) {
            return ChunkData::uniform(BlockState::AIR);
        }
        let window = self.window(origin.x >> self.shift, origin.z >> self.shift);
        self.column_stage(origin, &window, registry)
    }

    /// The chunk at `origin` from its columns.
    fn column_stage(
        &self,
        origin: BlockPos,
        (points, bounds): &(WindowPoints, LocalBounds),
        registry: &BlockRegistry,
    ) -> ChunkData {
        let (xs, zs) = chunk_axes(origin.x, origin.z);
        let fields = self.cx.grid(&xs, &zs);
        let mut h = [0; CHUNK_AREA];
        self.heights_in((&xs, &zs), &fields, points, &mut h);
        let spec = self.spec();
        let ground = h.map(|h| block_of(spec.datum, h));
        // `FluidRule::top`, with the rule read once.
        let level = match spec.fluid {
            FluidRule::SeaLevel { level, .. } => level,
            FluidRule::None => i64::MIN,
        };
        let fluid_top = ground.map(|g| if g < level { level } else { g });
        if cfg!(debug_assertions) {
            self.check_grounds(origin, points, bounds, &ground);
        }
        let fluid = match &spec.fluid {
            FluidRule::SeaLevel { fluid, .. } => block(registry, fluid, NAME),
            FluidRule::None => BlockState::AIR,
        };
        let stages = Stages {
            fill: &*self.fill,
            surface: &*self.surface,
            floor: block(registry, &spec.floor, NAME),
            fluid,
        };
        let columns = ChunkColumns {
            origin,
            ground: &ground,
            fluid_top: &fluid_top,
            cx: &self.cx,
            fields: &fields,
        };
        let chunk = super::decorate::decorate(&columns, &stages, registry);
        if cfg!(debug_assertions) {
            self.check_top(origin, bounds, &chunk);
            self.check_vertical((origin.x, origin.z), bounds);
        }
        chunk
    }

    /// The deep path: the fill alone.
    fn deep(&self, origin: BlockPos, registry: &BlockRegistry) -> ChunkData {
        let floor = block(registry, &self.spec().floor, NAME);
        let mut out = Vec::new();
        match self.fill.rock(&self.cx, origin, floor, registry, &mut out) {
            Rock::Uniform(state) => ChunkData::uniform(state),
            Rock::Dense => ChunkData::from_dense(&out),
        }
    }

    /// The debug span check of the grounds: each lies between the bounds of the window's points.
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
    /// the window's points.
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
             bounds {}..={} its kinds give at its 16 lattice points ({}): a kind's bounds are \
             under its heights",
            self.realm(),
            bounds.ground_lo,
            bounds.ground_hi,
            self.window_kinds(points),
        );
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
                         {} of its chunk column: a kind's bounds or the surface stage's rise are \
                         under its blocks",
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

/// The 32 x and z of the chunk column whose lowest corner is `(x0, z0)`.
fn chunk_axes(x0: i64, z0: i64) -> ([i64; CHUNK_SIZE as usize], [i64; CHUNK_SIZE as usize]) {
    (
        std::array::from_fn(|i| x0 + i as i64),
        std::array::from_fn(|i| z0 + i as i64),
    )
}

/// The chunk layers of a realm that may hold something not air: from its span's bottom up to its
/// highest kind height, surface rise or fluid, inside its span.
fn vertical<Cx: Context>(
    spec: &RealmSpec,
    cx: &Cx,
    kinds: &[RealmKind<Cx>],
    reach: SurfaceReach,
) -> RangeInclusive<i64> {
    let span = spec.span.chunk_layers();
    let (lo, hi) = kinds.iter().fold((i64::MAX, i64::MIN), |(lo, hi), k| {
        let r = k.kind.range(cx);
        (lo.min(r.lo), hi.max(r.hi))
    });
    let clamp = |v: i64| v.clamp(i64::from(HeightQ::MIN), i64::from(HeightQ::MAX)) as HeightQ;
    let mut top = block_of(spec.datum, clamp(hi)).saturating_add(reach.rise.into());
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
        let cell = (origin.x >> self.shift, origin.z >> self.shift);
        let fast = |bounds: &LocalBounds| {
            if origin.y > bounds.top {
                Some(ChunkData::uniform(BlockState::AIR))
            } else if origin.y + i64::from(CHUNK_SIZE) - 1 <= bounds.deep {
                Some(self.deep(origin, registry))
            } else {
                None
            }
        };
        // The chunks of the last lattice cell read its bounds alone.
        if self.memo
            && let Some(bounds) = memo::last_bounds(self.key, cell)
            && let Some(chunk) = fast(&bounds)
        {
            return chunk;
        }
        let window = self.window(cell.0, cell.1);
        if let Some(chunk) = fast(&window.1) {
            return chunk;
        }
        self.column_stage(origin, &window, registry)
    }

    /// From the realm's span bottom to its highest kind height (plus the surface rise and the
    /// fluid), inside its span.
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        self.vertical.clone()
    }

    fn name(&self) -> &str {
        self.realm().as_str()
    }

    /// The low 32 bits of the digest of the realm's lines: its dimension's sub-digest when the
    /// realm is the dimension's only one.
    fn version(&self) -> u32 {
        self.digest.dimension(&self.dimension).version()
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
