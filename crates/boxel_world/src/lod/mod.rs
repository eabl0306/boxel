//! Far level of detail, the world side (ADR 0031): terrain far beyond the full-detail view
//! distance, drawn as coarse nodes, like the Distant Horizons mod. Optional: without
//! [`LodWorldPlugin`] none of this exists (no resource, no system, no job).
//!
//! A level-`k` node is a 32^3 grid of cells of `2^k` blocks (`k = 1..=K`), keyed by a
//! [`ChunkKey`] with `lod = k`. Which nodes are wanted is an octree refined by distance from the
//! eye, the same ring radius `R` (in node edges) at every level, out to the horizon
//! ([`Thresholds`]). The generator samples every level directly
//! ([`WorldGenerator::lod`](boxel_worldgen::WorldGenerator::lod)): one job builds one column of
//! one level and meshes its non-uniform nodes with the ordinary mesher. The renderer gets
//! [`LodOp`]s through [`LodOps`]: meshes, removals, and the octants each node hides because
//! finer nodes or full-detail chunks draw them ([`coverage`]).
//!
//! Per frame (in `Update`, after the world's systems, chained [`LodSystems`]):
//! 1. `Coverage`: every message of the frame, read in full whatever the budget: the world's mesh
//!    and data events and the main renderer's resyncs and refusals feed the full-detail coverage
//!    ([`LodCoverage`]); the renderer's own resyncs ([`LodResync`]) and refusals
//!    ([`LodFeedback`]) make nodes unavailable. The masks follow in the same frame, so a chunk and
//!    the octant that hid it swap with neither a hole nor a doubled frame.
//! 2. `Plan`: the eye ([`LodFocus`], or the first [`ChunkViewer`] without one) and the
//!    thresholds; the wanted tree made again from the coarsest level whose eye moved an eighth of
//!    its hysteresis band (8 blocks for level 2, 16 for level 3, ...), or all of it after a
//!    threshold changed; stale job priorities refreshed a batch at a time.
//! 3. `Dispatch`: finished jobs collected and new ones started by priority, within
//!    [`LodSettings::frame_budget`] and the in-flight caps (fewer while the world is busy, so
//!    near chunks come first).
//! 4. `Collect`: the frame's ops into [`LodOps`] (sends capped per frame), then [`LodStats`].
//!
//! Between frames (ADR 0023) an `Idle` step goes on collecting and starting jobs; it never
//! writes ops (finished meshes wait for `Collect`).
use crate::interest::Viewer;
use crate::jobs::{JobExecutor, TaskPoolExecutor};
use crate::streaming::ViewerChunks;
use crate::{
    ChunkDataEvent, ChunkMap, ChunkMeshEvent, ChunkMeshResync, ChunkViewer, WorldContent,
    WorldSettings, WorldStats, WorldSystems,
};
use boxel_app::{App, Idle, IdleStep, IdleWanted, IdleWork, Plugin, Update};
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec3};
use boxel_mesh::ChunkMesh;
use boxel_mesh::lod::LodMaterials;
use boxel_transform::Transform;
use boxel_voxel::{ChunkKey, ChunkPos, DimensionId};
use boxel_worldgen::lod::LodRequest;
use foldhash::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub mod coverage;
mod jobs;
mod plan;
#[cfg(test)]
mod tests;
mod tree;

pub use coverage::{
    ChunkCoverage, LodCoverage, Octants, POLL_PER_FRAME, chunk_of_octant, octant_of_chunk,
};
pub use plan::{KeepExtent, MAX_LEVELS, Thresholds, node_edge, world_limit};

use jobs::{JobInput, LodJobs, LodMessage};
use plan::{ColumnKey, Eye, Plan};
use tree::{Column, Tree};

/// How far the eye moves at least before a level of the wanted tree is planned again, in blocks.
pub const REPLAN_DISTANCE: f64 = 8.0;
/// How far the eye moves, or how much it turns, before queued job priorities are refreshed.
const STALE_DISTANCE: f64 = 32.0;
const STALE_TURN_DEGREES: f64 = 15.0;
/// Queued columns whose priority is refreshed per frame.
const REFRESH_PER_FRAME: usize = 256;
/// Results and dispatches between two looks at the clock.
const CLOCK_EVERY: usize = 16;

/// The far LOD's knobs (ADR 0031). Read every frame; an invalid change is logged once and the
/// last valid settings stay (invalid settings from the start mean no LOD).
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct LodSettings {
    /// False keeps the plugin but draws nothing and starts no job (every node is dropped; turning
    /// it on again fills from scratch).
    pub enabled: bool,
    /// `R`: the radius of every ring, in node edges of its level (8..=48). A cell at the inner
    /// edge of a ring spans about `1 / (16 R)` radians: 2 pixels at 1080p for 24.
    pub ring_radius: u32,
    /// `K`: the coarsest level (1..=[`MAX_LEVELS`]). The horizon is `32 R 2^K` blocks.
    pub levels: u8,
    /// Where the terrain fog starts, as a fraction of the horizon (0..=1; 1: a hard edge). Read
    /// by the renderer.
    pub fade_start: f32,
    /// Quads the renderer's LOD arena holds (1 Mi..=32 Mi, clamped to the device). Read by the
    /// renderer.
    pub arena_quads: u32,
    /// Jobs in flight at most while the world is busy (loading or meshing): near chunks wait
    /// behind at most this many LOD jobs (1..=8).
    pub in_flight_busy: usize,
    /// Jobs in flight at most once the world is settled (1..=8).
    pub in_flight_settled: usize,
    /// Node meshes sent to the renderer per frame at most (at least 1).
    pub publish_per_frame: usize,
    /// Bytes of quads sent to the renderer per frame at most (a node larger than this still
    /// goes, alone).
    pub upload_bytes_per_frame: usize,
    /// Main-thread time per frame for collecting finished jobs and starting new ones, and between
    /// frames per idle step. Reading the renderer's messages is not counted.
    pub frame_budget: Duration,
    /// How long an edited chunk waits before its LOD nodes are rebuilt (milestone 2).
    pub edit_debounce: Duration,
    /// Up to which level a generator without a LOD source is drawn from its real chunks
    /// (0..=1; milestone 2).
    pub fallback_max_level: u8,
    /// Tints each level with its own color. Read by the renderer.
    pub debug_tint: bool,
}

impl Default for LodSettings {
    /// The `far` preset of the examples: `R = 24`, `K = 4`, a horizon of 12,288 blocks.
    fn default() -> Self {
        Self {
            enabled: true,
            ring_radius: 24,
            levels: 4,
            fade_start: 0.5,
            arena_quads: 12 << 20,
            in_flight_busy: 2,
            in_flight_settled: 5,
            publish_per_frame: 64,
            upload_bytes_per_frame: 2 << 20,
            frame_budget: Duration::from_micros(300),
            edit_debounce: Duration::from_millis(500),
            fallback_max_level: 1,
            debug_tint: false,
        }
    }
}

/// An invalid [`LodSettings`] field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LodSettingsError(pub String);

impl std::fmt::Display for LodSettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LodSettingsError {}

impl LodSettings {
    /// Checks every field's range (field docs).
    pub fn validate(&self) -> Result<(), LodSettingsError> {
        let err = |m: String| Err(LodSettingsError(m));
        if !(8..=48).contains(&self.ring_radius) {
            return err(format!("ring_radius {} is not in 8..=48", self.ring_radius));
        }
        if !(1..=MAX_LEVELS).contains(&self.levels) {
            return err(format!("levels {} is not in 1..={MAX_LEVELS}", self.levels));
        }
        if !(0.0..=1.0).contains(&self.fade_start) {
            return err(format!("fade_start {} is not in 0..=1", self.fade_start));
        }
        if !((1 << 20)..=(32 << 20)).contains(&self.arena_quads) {
            return err(format!(
                "arena_quads {} is not in 1 Mi..=32 Mi",
                self.arena_quads
            ));
        }
        for (name, n) in [
            ("in_flight_busy", self.in_flight_busy),
            ("in_flight_settled", self.in_flight_settled),
        ] {
            if !(1..=8).contains(&n) {
                return err(format!("{name} {n} is not in 1..=8"));
            }
        }
        if self.publish_per_frame == 0 || self.upload_bytes_per_frame == 0 {
            return err("publish_per_frame and upload_bytes_per_frame must be above 0".into());
        }
        if self.frame_budget.is_zero() {
            return err("frame_budget must be above 0".into());
        }
        if self.fallback_max_level > 1 {
            return err(format!(
                "fallback_max_level {} is not 0 or 1",
                self.fallback_max_level
            ));
        }
        Ok(())
    }

    /// The thresholds around a keep region of `keep` ([`Thresholds`]).
    pub fn thresholds(&self, keep: KeepExtent) -> Thresholds {
        Thresholds::new(self.ring_radius, self.levels, keep)
    }

    /// The horizon around a keep region of `keep`, in blocks: `32 R 2^K` unless the keep region
    /// reaches it.
    pub fn horizon(&self, keep: KeepExtent) -> f64 {
        self.thresholds(keep).horizon()
    }

    /// The preset's horizon `32 R 2^K`, in blocks.
    pub fn preset_horizon(&self) -> f64 {
        f64::from(self.ring_radius) * node_edge(self.levels.clamp(1, MAX_LEVELS)) as f64
    }
}

/// Adds the far LOD's world side (module docs): [`LodSettings`] (from `settings`),
/// [`LodStats`], [`LodOps`], [`LodFeedback`], [`LodCoverage`], the [`LodResync`] message, the
/// chained [`LodSystems`] in `Update` after the world's and a step in `Idle`. Needs
/// [`WorldPlugin`](crate::WorldPlugin) first. The renderer's `LodPlugin` adds this one; a
/// headless app (tools, tests) adds it alone.
pub struct LodWorldPlugin {
    /// The initial settings.
    pub settings: LodSettings,
    /// Where the column jobs run (tests use `jobs::InlineExecutor`). The world's executor is
    /// private to its jobs, so the LOD keeps its own; the default uses the same pool.
    pub executor: Arc<dyn JobExecutor>,
}

impl Default for LodWorldPlugin {
    fn default() -> Self {
        Self {
            settings: LodSettings::default(),
            executor: Arc::new(TaskPoolExecutor),
        }
    }
}

/// The LOD's system sets, chained in `Update` after [`WorldSystems::Visibility`].
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LodSystems {
    /// Player edits (milestone 2).
    Edits,
    /// Resyncs, the renderer's feedback and the full-detail coverage.
    Coverage,
    /// The eye, the thresholds and the wanted tree.
    Plan,
    /// Collect finished jobs and start new ones.
    Dispatch,
    /// The frame's ops and the stats.
    Collect,
}

impl Plugin for LodWorldPlugin {
    fn build(&self, app: &mut App) {
        assert!(
            app.world().contains_resource::<WorldContent>(),
            "LodWorldPlugin needs WorldPlugin first"
        );
        app.insert_resource(self.settings.clone())
            .insert_resource(LodWorld::new(Arc::clone(&self.executor)))
            .init_resource::<LodStats>()
            .init_resource::<LodOps>()
            .init_resource::<LodFeedback>()
            .init_resource::<LodCoverage>()
            .init_resource::<ViewerEye>()
            .add_message::<LodResync>()
            .configure_sets(
                Update,
                (
                    LodSystems::Edits,
                    LodSystems::Coverage,
                    LodSystems::Plan,
                    LodSystems::Dispatch,
                    LodSystems::Collect,
                )
                    .chain()
                    .after(WorldSystems::Visibility),
            )
            .add_systems(
                Update,
                (
                    track_viewer_eye
                        .ambiguous_with_all()
                        .before(LodSystems::Plan),
                    lod_coverage.in_set(LodSystems::Coverage),
                    lod_plan.in_set(LodSystems::Plan),
                    lod_dispatch.in_set(LodSystems::Dispatch),
                    lod_collect.in_set(LodSystems::Collect),
                ),
            )
            // Its order against the world's idle step does not matter: they share no state but
            // the step's answer, which keeps the most urgent.
            .add_systems(Idle, lod_idle.ambiguous_with_all());
    }
}

/// Where the LOD looks from: the active camera's render origin and the eye's offset from it,
/// its forward direction and its field of view. Written by the renderer's focus system before
/// [`LodSystems`]; without it the eye is the first [`ChunkViewer`] and every
/// direction counts as in view.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct LodFocus {
    /// Render origin (blocks).
    pub origin: I64Vec3,
    /// The eye, relative to `origin`.
    pub eye_rel: Vec3,
    /// Unit forward direction.
    pub forward: Vec3,
    /// Vertical field of view, in radians.
    pub fov_y: f32,
    /// Width over height.
    pub aspect: f32,
}

/// Why the LOD draws nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LodOff {
    /// No valid [`LodSettings`] yet.
    InvalidSettings,
    /// [`LodSettings::enabled`] is false.
    Disabled,
    /// The keep region of the viewers reaches the preset's horizon ([`Thresholds::guard`]).
    Guard,
    /// The generator has no LOD source (the fallback from real chunks is milestone 2).
    NoSource,
    /// No eye: no [`LodFocus`] and no viewer.
    NoEye,
}

/// Counts of one level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LevelStats {
    /// Wanted columns.
    pub columns: usize,
    /// Nodes with a mesh (resident on the CPU, sent or not).
    pub nodes: usize,
    /// Their quads.
    pub quads: u64,
    /// Nodes the renderer has with at least one octant shown.
    pub drawn: usize,
    /// Nodes built with nothing to draw (uniform layers have no node at all).
    pub blank: usize,
    /// Nodes waiting for a job.
    pub pending: usize,
    /// Nodes lost (a failed job, or a face bucket past the renderer's limit).
    pub failed: usize,
    /// Of `nodes`, those in the layer that holds the generator's floor
    /// ([`vertical_chunks`](boxel_worldgen::WorldGenerator::vertical_chunks) start): faces
    /// under the world, expected 0.
    pub floor_nodes: usize,
    /// Their quads.
    pub floor_quads: u64,
}

/// The LOD's counters, refreshed every frame in `Collect` (the per-level counts are kept as nodes
/// change, never by a walk).
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct LodStats {
    /// Why it draws nothing (`None`: it works).
    pub off: Option<LodOff>,
    /// The horizon of the last plan, in blocks.
    pub horizon: f64,
    /// Per level (`levels[k - 1]`).
    pub levels: Vec<LevelStats>,
    /// Entries in the job queue (a few may be stale).
    pub queued_jobs: usize,
    /// Jobs running.
    pub in_flight: usize,
    /// Jobs finished with a result, since start.
    pub jobs_done: u64,
    /// Jobs that failed (a panic), since start.
    pub jobs_failed: usize,
    /// Median and 99th percentile job time (10 us resolution).
    pub job_p50: Duration,
    /// See `job_p50`.
    pub job_p99: Duration,
    /// Share of the wanted columns built at least once, at most 0.99 until every wanted node is
    /// built and sent: 1 then.
    pub fill: f32,
    /// From the first plan to the first full fill.
    pub filled_after: Option<Duration>,
    /// Meshes waiting to be sent (the per-frame cap).
    pub sends_waiting: usize,
    /// Meshes the renderer refused (arena full), since start.
    pub refused: usize,
    /// Full-detail chunks the main renderer draws, as the LOD sees them.
    pub published_chunks: usize,
    /// Full-detail chunks the LOD does not draw over (published, empty or air).
    pub covered_chunks: usize,
    /// Loaded chunks watched for an empty mesh.
    pub watched_chunks: usize,
}

impl LodStats {
    /// Nodes with a mesh over every level.
    pub fn nodes(&self) -> usize {
        self.levels.iter().map(|l| l.nodes).sum()
    }

    /// Their quads.
    pub fn quads(&self) -> u64 {
        self.levels.iter().map(|l| l.quads).sum()
    }

    /// Nodes drawn over every level.
    pub fn drawn(&self) -> usize {
        self.levels.iter().map(|l| l.drawn).sum()
    }

    /// Wanted columns over every level.
    pub fn columns(&self) -> usize {
        self.levels.iter().map(|l| l.columns).sum()
    }
}

/// One change for the renderer.
#[derive(Clone, Debug)]
pub enum LodOp {
    /// A new or replaced node mesh (never empty) with its hidden octants.
    Upsert {
        /// The node (`lod` = its level).
        key: ChunkKey,
        /// Its quads, positions in cells of the node.
        mesh: Arc<ChunkMesh>,
        /// Hidden octants, bit `x | y << 1 | z << 2` (a quad's octant: `boxel_mesh::lod`).
        mask: u8,
    },
    /// A node's hidden octants changed. It may follow the node's `Upsert` in the same frame: a
    /// renderer that refused that `Upsert` ignores it (the refusal comes back in
    /// [`LodFeedback::refused`], and no other op names the node until it is sent again).
    Mask {
        /// The node.
        key: ChunkKey,
        /// Hidden octants.
        mask: u8,
    },
    /// A node is gone.
    Remove {
        /// The node.
        key: ChunkKey,
    },
}

/// The frame's ops (the outbox), in order: the renderer takes them all in its extract step and
/// applies them before drawing. A node is only sent while its parent chain is on the renderer
/// (or holds nothing), and its descendants leave when it stops being there: a finer node never
/// shows inside an octant its parent draws. Ops left at the next `Update` (no renderer) are
/// dropped. A renderer drops them too while it has no device; its next device starts with a
/// resync, and a renderer that drops its nodes writes [`LodResync`].
#[derive(Resource, Debug, Default)]
pub struct LodOps {
    /// The ops, in order.
    pub ops: Vec<LodOp>,
}

impl LodOps {
    /// Takes every op.
    pub fn take(&mut self) -> Vec<LodOp> {
        std::mem::take(&mut self.ops)
    }
}

/// What the renderer reports back, read by the LOD at the start of its next `Update`.
#[derive(Resource, Debug, Default)]
pub struct LodFeedback {
    /// LOD meshes the renderer refused (its LOD arena is full): unavailable from the next frame,
    /// sent again once `arena_free_quads` has room for them, at most once a second.
    pub refused: Vec<ChunkKey>,
    /// Free quads in the LOD arena after the frame's uploads (`None`: no report yet).
    pub arena_free_quads: Option<u64>,
    /// Full-detail chunks the main renderer draws nothing for after refusing their upload
    /// (`true`: no older mesh of the chunk is resident; a refused chunk that keeps its older mesh
    /// is still drawn and never named) or shows as the world sent them again (`false`),
    /// forwarded from its own refusals: the coverage counts such a chunk as not drawn until it
    /// is sent again. Read before this frame's `ChunkMeshResync`, which clears them all.
    pub chunks: Vec<(ChunkPos, bool)>,
}

/// Written by a renderer that dropped its LOD nodes (a resync, which every new device starts
/// with, or a new LOD arena): every node becomes unavailable and is sent again, the coarsest level
/// first.
#[derive(Message, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LodResync;

/// The eye of a plan, where it looks, and the thresholds.
#[derive(Clone, Copy, Debug)]
struct Planned {
    eye: Eye,
    th: Thresholds,
}

/// Where the eye looks, for the priority classes.
#[derive(Clone, Copy, Debug)]
struct View {
    /// Unit forward (`None`: every direction is in view).
    forward: Option<DVec3>,
    /// Half the view cone's angle.
    half_angle: f64,
}

/// The LOD's state (a resource only `LodWorldPlugin` adds).
#[derive(Resource)]
pub(crate) struct LodWorld {
    tree: Tree,
    jobs: LodJobs,
    /// Columns of the jobs in flight.
    jobs_of: HashMap<u64, ColumnKey>,
    /// The last valid settings, and the last error logged.
    settings: Option<LodSettings>,
    error: Option<LodSettingsError>,
    planned: Option<Planned>,
    /// The keep extent the last thresholds followed (held while published chunks lie past a
    /// smaller one: [`plan::reaches_published`]).
    keep: Option<KeepExtent>,
    /// The wanted tree the nodes follow.
    plan: Plan,
    /// The eye each level was last planned from (`level_eyes[k]`).
    level_eyes: [Option<Eye>; MAX_LEVELS as usize + 1],
    view: View,
    /// Where priorities were last refreshed from.
    refreshed: Option<(Eye, Option<DVec3>)>,
    /// Queued columns whose priority is refreshed next.
    refresh: Vec<ColumnKey>,
    /// The content the tree was built from: generator and registry (pointers), materials.
    content: Option<(usize, usize, Arc<LodMaterials>)>,
    /// The generator's highest chunk layer ([`plan::plan`]'s ceiling), `None` without one.
    ceiling: Option<i64>,
    off: Option<LodOff>,
    /// Logged once each.
    warned: Vec<LodOff>,
    outbox: Vec<LodOp>,
    arena_free: Option<u64>,
    started: Option<Instant>,
    filled_after: Option<Duration>,
}

impl LodWorld {
    fn new(executor: Arc<dyn JobExecutor>) -> Self {
        Self {
            tree: Tree::new(DimensionId(0), 1, None),
            jobs: LodJobs::new(executor),
            jobs_of: HashMap::default(),
            settings: None,
            error: None,
            planned: None,
            keep: None,
            plan: Plan::default(),
            level_eyes: [None; MAX_LEVELS as usize + 1],
            view: View {
                forward: None,
                half_angle: std::f64::consts::PI,
            },
            refreshed: None,
            refresh: Vec::new(),
            content: None,
            ceiling: None,
            off: Some(LodOff::InvalidSettings),
            warned: Vec::new(),
            outbox: Vec::new(),
            arena_free: None,
            started: None,
            filled_after: None,
        }
    }
}

impl LodWorld {
    /// Checks the settings when they changed (or never were): a valid change replaces the
    /// settings, an invalid one is logged once and the last valid ones stay.
    fn check_settings(&mut self, settings: &LodSettings, changed: bool) {
        if !changed && (self.settings.is_some() || self.error.is_some()) {
            return;
        }
        match settings.validate() {
            Ok(()) => {
                self.settings = Some(settings.clone());
                self.error = None;
            }
            Err(e) => {
                if self.error.as_ref() != Some(&e) {
                    let kept = if self.settings.is_some() {
                        "keeping the last valid settings"
                    } else {
                        "no LOD"
                    };
                    tracing::warn!("LOD settings: {e}; {kept}");
                    self.error = Some(e);
                }
            }
        }
    }

    /// Turns the LOD off for `why`: every node goes (their removals leave in `Collect`), every
    /// job is cancelled. Logged once per reason, but for `Disabled`.
    fn turn_off(&mut self, why: LodOff, generator: &str) {
        if self.off == Some(why) {
            return;
        }
        if why != LodOff::Disabled && !self.warned.contains(&why) {
            self.warned.push(why);
            match why {
                LodOff::Guard => tracing::warn!(
                    "LOD off: the viewers' keep region reaches the horizon (a huge vertical \
                     radius without vertical bounds, or an eye far above a fixed-height world)"
                ),
                LodOff::NoSource => tracing::warn!(
                    generator,
                    "LOD off: the generator has no LOD source (WorldGenerator::lod)"
                ),
                LodOff::InvalidSettings | LodOff::NoEye | LodOff::Disabled => {}
            }
        }
        self.outbox.extend(self.tree.clear());
        self.jobs.queue.clear();
        self.off = Some(why);
        self.planned = None;
        self.plan = Plan::default();
        self.level_eyes = [None; MAX_LEVELS as usize + 1];
        self.refreshed = None;
        self.refresh.clear();
        self.started = None;
        self.filled_after = None;
    }

    /// Starts over when the content (generator, registry), the dimension or the levels changed.
    fn check_content(&mut self, content: &WorldContent, dim: DimensionId, levels: u8) {
        let generator = Arc::as_ptr(&content.generator).cast::<()>() as usize;
        let registry = Arc::as_ptr(&content.registry) as usize;
        let same = self
            .content
            .as_ref()
            .is_some_and(|(g, r, _)| (*g, *r) == (generator, registry));
        if !same {
            let materials = Arc::new(LodMaterials::from_registry(&content.registry));
            self.content = Some((generator, registry, materials));
        }
        if !same || self.tree.dim != dim || self.tree.levels != levels {
            self.outbox.extend(self.tree.clear());
            self.jobs.queue.clear();
            let layers = content.generator.vertical_chunks();
            let (floor, ceiling) = (*layers.start(), *layers.end());
            self.tree = Tree::new(dim, levels, (floor != i64::MIN).then_some(floor));
            self.ceiling = (ceiling != i64::MAX).then_some(ceiling);
            self.planned = None;
            self.plan = Plan::default();
            self.level_eyes = [None; MAX_LEVELS as usize + 1];
            self.refreshed = None;
            self.refresh.clear();
            self.started = None;
            self.filled_after = None;
        }
    }

    /// The `Plan` step (module docs) for an eye at `eye` looking along `view`.
    fn plan(
        &mut self,
        eye: Eye,
        view: View,
        viewers: &[(u64, Viewer)],
        world: &WorldSettings,
        cov: &ChunkCoverage,
        generator: &str,
    ) {
        let Some(settings) = &self.settings else {
            return;
        };
        let keep = KeepExtent::of_viewers(
            eye.chunk(),
            viewers.iter().map(|(_, v)| *v),
            world.unload_hysteresis,
            world.vertical_bounds.as_ref(),
        );
        // A smaller keep region leaves published chunks behind until the world unloads them:
        // `T_1` keeps its reach until it covers them again (else a level-2 node would hide the
        // octant of a touched child nobody builds, and the sky would show through the ground).
        let keep = match self.keep {
            Some(held)
                if settings.thresholds(keep).level(1) < settings.thresholds(held).level(1)
                    && !plan::reaches_published(
                        &eye,
                        &settings.thresholds(keep),
                        self.ceiling,
                        cov,
                    ) =>
            {
                held
            }
            _ => keep,
        };
        self.keep = Some(keep);
        let th = settings.thresholds(keep);
        if th.guard() || (self.off == Some(LodOff::Guard) && th.guard_holds()) {
            self.turn_off(LodOff::Guard, generator);
            return;
        }
        self.off = None;
        self.view = view;
        // The coarsest level due: all of them after a threshold changed, else the coarsest whose
        // eye moved past its cadence (finer levels follow it).
        let fresh = self.planned.is_none_or(|p| p.th != th);
        let redo = (1..=th.levels())
            .rev()
            .find(|&k| {
                fresh
                    || self.level_eyes[usize::from(k)]
                        .is_none_or(|e| e.distance_to(&eye) >= plan::cadence(k))
            })
            .unwrap_or(0);
        if redo > 0 {
            let redo = if fresh { th.levels() } else { redo };
            let mut old = std::mem::take(&mut self.plan);
            let new = plan::plan(&eye, &th, &mut old, redo, self.ceiling);
            self.tree.apply_plan(&old, &new, redo, cov);
            self.plan = new;
            for e in &mut self.level_eyes[1..=usize::from(redo)] {
                *e = Some(eye);
            }
            self.planned = Some(Planned { eye, th });
            self.started.get_or_insert_with(Instant::now);
            self.queue_work(cov);
        }
        // Stale priorities: a batch per frame after the eye moved or turned enough.
        let turned = |a: Option<DVec3>, b: Option<DVec3>| match (a, b) {
            (Some(a), Some(b)) => a.dot(b).clamp(-1.0, 1.0).acos().to_degrees(),
            (None, None) => 0.0,
            _ => 180.0,
        };
        let stale = self.refreshed.is_none_or(|(e, f)| {
            e.distance_to(&eye) >= STALE_DISTANCE || turned(f, view.forward) >= STALE_TURN_DEGREES
        });
        if stale {
            self.refreshed = Some((eye, view.forward));
            self.refresh = self
                .tree
                .columns
                .iter()
                .filter(|(_, c)| c.ticket.is_some())
                .map(|(&k, _)| k)
                .collect();
        }
        for _ in 0..REFRESH_PER_FRAME {
            let Some(c) = self.refresh.pop() else {
                break;
            };
            self.queue(c, true, cov);
        }
    }

    /// Queues every column whose work may have changed.
    fn queue_work(&mut self, cov: &ChunkCoverage) {
        for c in self.tree.take_work() {
            self.queue(c, false, cov);
        }
    }

    /// Queues again, with a fresh class, the queued columns whose coverage changed.
    fn requeue_covered(&mut self, cov: &ChunkCoverage) {
        for c in self.tree.take_reprioritize() {
            if self
                .tree
                .columns
                .get(&c)
                .is_some_and(|col| col.ticket.is_some())
            {
                self.queue(c, true, cov);
            }
        }
    }

    /// Queues column `c` if it needs a job (`again`: with a fresh priority even if queued).
    fn queue(&mut self, c: ColumnKey, again: bool, cov: &ChunkCoverage) {
        let Some(col) = self.tree.columns.get(&c) else {
            return;
        };
        if col.ticket.is_some() && !again {
            return;
        }
        let Some(work) = col.work() else {
            return;
        };
        let (class, size) = self.priority(c, col, &work, cov);
        let ticket = self.jobs.push(c, class, size);
        if let Some(col) = self.tree.columns.get_mut(&c) {
            col.ticket = Some(ticket);
        }
    }

    /// The priority of column `c`'s next job, by class and then by the screen size of its
    /// nearest node, `e_k / max(d, e_k / 4)`. Classes:
    /// - 0, coverage-critical and fill-first: the top level (the horizon appears first, and
    ///   after a jump), and nodes that close a hole in full detail ([`Tree::fills_hole`]: a
    ///   lod-1 node with published chunks next to uncovered ones, whose parent hides that
    ///   octant until it is sent, and its ancestors, as the chain rule needs);
    /// - 1 in view, 2 out of it;
    /// - 3, last: lod-1 nodes under full detail that close no hole (placeholders for when
    ///   chunks unload or stream in).
    ///
    /// [`Tree::fills_hole`]: tree::Tree::fills_hole
    fn priority(
        &self,
        c: ColumnKey,
        col: &Column,
        work: &tree::Work,
        cov: &ChunkCoverage,
    ) -> (u8, f32) {
        let Some(eye) = self.planned.map(|p| p.eye) else {
            return (2, 0.0);
        };
        let e = node_edge(c.level);
        let y = col.nearest(work, eye.block[1].div_euclid(e));
        let key = c.node(self.tree.dim, y);
        let d = eye.distance(key);
        let ef = e as f64;
        let size = (ef / d.max(ef / 4.0)) as f32;
        let dim = self.tree.dim;
        let class = if c.level >= self.tree.levels
            || work
                .layers()
                .any(|y| self.tree.fills_hole(c.node(dim, y), cov))
        {
            0
        } else if c.level == 1
            && work
                .layers()
                .any(|y| cov.get(c.node(dim, y).pos).published != 0)
        {
            3
        } else if self.in_view(&eye, key) {
            1
        } else {
            2
        };
        (class, size)
    }

    /// True if node `key`'s bounding sphere meets the view cone.
    fn in_view(&self, eye: &Eye, key: ChunkKey) -> bool {
        let Some(forward) = self.view.forward else {
            return true;
        };
        let v = eye.center_offset(key);
        let dist = v.length();
        let radius = node_edge(key.lod) as f64 * (3.0f64.sqrt() / 2.0);
        if dist <= radius {
            return true;
        }
        let angle = (v.dot(forward) / dist).clamp(-1.0, 1.0).acos();
        angle - (radius / dist).asin() < self.view.half_angle
    }

    /// The in-flight cap.
    fn cap(&self, settled: bool) -> usize {
        self.settings.as_ref().map_or(0, |s| {
            if settled {
                s.in_flight_settled
            } else {
                s.in_flight_busy
            }
        })
    }

    /// Collects finished jobs and starts queued ones while under `cap`, until `until` (checked
    /// every [`CLOCK_EVERY`] results and dispatches), again while results keep arriving.
    fn run_jobs(
        &mut self,
        until: Instant,
        cap: usize,
        content: &WorldContent,
        cov: &ChunkCoverage,
    ) {
        loop {
            let mut got = 0;
            while let Some(report) = self.jobs.try_recv() {
                self.apply(report, cov, content.generator.name());
                got += 1;
                if got % CLOCK_EVERY == 0 && Instant::now() >= until {
                    return;
                }
            }
            self.queue_work(cov);
            let mut spawned = 0;
            while self.off.is_none() && self.jobs.in_flight < cap && self.dispatch_one(content) {
                spawned += 1;
                if spawned % CLOCK_EVERY == 0 && Instant::now() >= until {
                    return;
                }
            }
            if got == 0 || Instant::now() >= until {
                return;
            }
        }
    }

    /// Applies one job's report.
    fn apply(&mut self, report: jobs::LodReport, cov: &ChunkCoverage, generator: &str) {
        let id = report.message.id();
        let Some(c) = self.jobs_of.remove(&id) else {
            return;
        };
        match report.message {
            LodMessage::Built { column, nodes, .. } => {
                let lost = self.tree.apply_built(c, id, column, nodes, cov);
                if lost > 0 {
                    tracing::warn!(
                        generator,
                        level = c.level,
                        x = c.x,
                        z = c.z,
                        "a LOD node has a face with more quads than the renderer takes: it is \
                         lost, its parent draws there"
                    );
                }
            }
            LodMessage::NoSource { .. } | LodMessage::Cancelled { .. } => {
                self.tree.job_ended(c, id);
            }
            LodMessage::Failed { panic, .. } => {
                tracing::warn!(
                    generator,
                    level = c.level,
                    x = c.x,
                    z = c.z,
                    "a LOD column job failed: {panic}"
                );
                self.tree.job_failed(c, id);
            }
        }
    }

    /// Starts the best queued job (stale entries are skipped). False if none is left.
    fn dispatch_one(&mut self, content: &WorldContent) -> bool {
        let Some((_, _, materials)) = &self.content else {
            return false;
        };
        while let Some(entry) = self.jobs.queue.pop() {
            let c = entry.column;
            let Some(col) = self.tree.columns.get_mut(&c) else {
                continue;
            };
            if col.ticket != Some(entry.ticket) {
                continue;
            }
            col.ticket = None;
            let Some(work) = col.work() else {
                continue;
            };
            let id = self.jobs.next_id();
            let cancel = Arc::new(AtomicBool::new(false));
            self.tree.start_job(c, id, Arc::clone(&cancel));
            self.jobs_of.insert(id, c);
            self.jobs.spawn(JobInput {
                id,
                generator: Arc::clone(&content.generator),
                registry: Arc::clone(&content.registry),
                materials: Arc::clone(materials),
                request: LodRequest {
                    dim: self.tree.dim,
                    level: c.level,
                    column: [c.x, c.z],
                    layers: work.layers.0..=work.layers.1,
                },
                only: work.only,
                cancel,
            });
            return true;
        }
        false
    }

    /// What an idle step would find under `cap`.
    fn idle_pending(&self, cap: usize) -> IdleWork {
        if self.jobs.has_results()
            || (self.off.is_none() && self.jobs.in_flight < cap && !self.jobs.queue.is_empty())
        {
            IdleWork::Ready
        } else if self.jobs.in_flight > 0 {
            IdleWork::Waiting
        } else {
            IdleWork::Done
        }
    }

    /// The frame's ops (module docs).
    fn collect(&mut self, cov: &ChunkCoverage, now: Instant) -> Vec<LodOp> {
        let mut ops = std::mem::take(&mut self.outbox);
        if let Some(s) = &self.settings {
            let (nodes, bytes) = (s.publish_per_frame, s.upload_bytes_per_frame);
            ops.extend(self.tree.collect(nodes, bytes, self.arena_free, now, cov));
        }
        ops
    }

    /// True if the LOD is on and has nothing left to do: no job queued or running, no mesh
    /// waiting to be sent. Every wanted node is then built and sent (or lost).
    fn filled(&self) -> bool {
        self.off.is_none()
            && self.planned.is_some()
            && self.jobs.in_flight == 0
            && self.jobs.queue.is_empty()
            && self.tree.sends_waiting() == 0
            && !self.tree.has_refused()
    }

    /// This frame's stats.
    fn stats(&mut self, now: Instant, coverage: &LodCoverage) -> LodStats {
        let filled = self.filled();
        if filled
            && self.filled_after.is_none()
            && let Some(started) = self.started
        {
            self.filled_after = Some(now.saturating_duration_since(started));
        }
        let (wanted, known) = self.tree.columns_known();
        let fill = if filled {
            1.0
        } else if wanted == 0 {
            0.0
        } else {
            (known as f32 / wanted as f32).min(0.99)
        };
        LodStats {
            off: self.off,
            horizon: self.planned.map_or(0.0, |p| p.th.horizon()),
            levels: self.tree.counts().to_vec(),
            queued_jobs: self.jobs.queue.len(),
            in_flight: self.jobs.in_flight,
            jobs_done: self.jobs.times.count(),
            jobs_failed: self.jobs.failed,
            job_p50: self.jobs.times.quantile(0.5),
            job_p99: self.jobs.times.quantile(0.99),
            fill,
            filled_after: self.filled_after,
            sends_waiting: self.tree.sends_waiting(),
            refused: self.tree.refusals,
            published_chunks: coverage.coverage().published_chunks(),
            covered_chunks: coverage.coverage().covered_chunks(),
            watched_chunks: coverage.watched(),
        }
    }

    /// True if jobs run or wait.
    fn has_job_work(&self) -> bool {
        self.jobs.in_flight > 0 || !self.jobs.queue.is_empty()
    }
}

/// The eye and where it looks: [`LodFocus`], else the first viewer (by entity) looking
/// everywhere.
fn eye_and_view(focus: Option<&LodFocus>, viewer: Option<DVec3>) -> Option<(Eye, View)> {
    if let Some(f) = focus {
        let forward = f.forward.as_dvec3().normalize_or_zero();
        let tan = (f64::from(f.fov_y) / 2.0).tan();
        let aspect = f64::from(f.aspect);
        // The cone through the view's corners.
        let half_angle = (tan * (1.0 + aspect * aspect).sqrt()).atan();
        return Some((
            Eye::from_origin(f.origin, f.eye_rel),
            View {
                forward: (forward != DVec3::ZERO).then_some(forward),
                half_angle,
            },
        ));
    }
    viewer.map(|p| {
        (
            Eye::from_world(p),
            View {
                forward: None,
                half_angle: std::f64::consts::PI,
            },
        )
    })
}

/// `LodSystems::Coverage`: drops ops no renderer took, then reads every message of the frame in
/// full (never under the budget, module docs): the main renderer's refusals, the world's mesh
/// events, the main renderer's resync, the world's data events, then polls the watch list, and
/// applies the LOD renderer's resyncs and refusals. The coverage changes reach the masks, and
/// the queued columns whose class changed are queued again.
#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy-style system: one parameter per resource it reads"
)]
fn lod_coverage(
    mut lod: ResMut<LodWorld>,
    mut ops: ResMut<LodOps>,
    mut feedback: ResMut<LodFeedback>,
    mut coverage: ResMut<LodCoverage>,
    map: Res<ChunkMap>,
    mut resyncs: MessageReader<LodResync>,
    mut meshes: MessageReader<ChunkMeshEvent>,
    mut chunk_resyncs: MessageReader<ChunkMeshResync>,
    mut data: MessageReader<ChunkDataEvent>,
) {
    if !ops.ops.is_empty() {
        ops.ops.clear();
    }
    let lod = &mut *lod;
    let coverage = &mut *coverage;
    let now = Instant::now();
    let dim = lod.tree.dim;
    let ours = |key: ChunkKey| key.lod == 0 && key.dim == dim;
    // The main renderer's refusals first: a resync of this frame clears them.
    if !feedback.chunks.is_empty() {
        for (pos, refused) in std::mem::take(&mut feedback.chunks) {
            coverage.refused(pos, refused);
        }
    }
    for event in meshes.read() {
        match event {
            ChunkMeshEvent::Upsert { key, .. } if ours(*key) => coverage.upserted(key.pos),
            ChunkMeshEvent::Remove { key, .. } if ours(*key) => coverage.removed(key.pos, &map),
            _ => {}
        }
    }
    if chunk_resyncs.read().count() > 0 {
        coverage.renderer_resynced(&map);
    }
    let tree = &lod.tree;
    for event in data.read() {
        if ours(event.key()) {
            let loaded = matches!(event, ChunkDataEvent::Loaded { .. });
            coverage.data_changed(event.pos(), loaded, &map, |p| tree.lod1_octants(p));
        }
    }
    for (p, octants) in lod.tree.take_lod1_quads() {
        coverage.lod1_quads(p, octants, &map);
    }
    let tree = &lod.tree;
    coverage.poll(&map, POLL_PER_FRAME, |p| tree.lod1_octants(p));
    let changed = coverage.take_changed();
    let cov = coverage.coverage();
    // The LOD renderer's refusals, then its resync (which supersedes them).
    if !feedback.refused.is_empty() {
        lod.tree
            .refuse(&std::mem::take(&mut feedback.refused), now, cov);
    }
    if let Some(free) = feedback.arena_free_quads.take() {
        lod.arena_free = Some(free);
    }
    if resyncs.read().count() > 0 {
        lod.tree.resync(cov);
        lod.outbox.clear();
        lod.arena_free = None;
    }
    for p in changed {
        lod.tree.chunks_changed(p, cov);
    }
    lod.requeue_covered(cov);
    coverage.end_frame();
}

/// The first viewer's position (by entity), the eye when there is no [`LodFocus`].
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
struct ViewerEye(Option<DVec3>);

/// Reads the first viewer's `Transform` (only written when it changes). Its order against the
/// systems that move viewers does not matter (one frame of lag at most), as for the world's own
/// viewer tracking.
fn track_viewer_eye(
    viewers: Query<(Entity, &Transform), With<ChunkViewer>>,
    mut eye: ResMut<ViewerEye>,
) {
    let first = viewers
        .iter()
        .min_by_key(|(e, _)| e.to_bits())
        .map(|(_, t)| t.translation);
    if eye.0 != first {
        eye.0 = first;
    }
}

/// `LodSystems::Plan` (module docs).
#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy-style system: one parameter per resource it reads"
)]
fn lod_plan(
    mut lod: ResMut<LodWorld>,
    settings: Res<LodSettings>,
    world: Res<WorldSettings>,
    content: Res<WorldContent>,
    viewers: Res<ViewerChunks>,
    focus: Option<Res<LodFocus>>,
    viewer_eye: Res<ViewerEye>,
    coverage: Res<LodCoverage>,
) {
    let lod = &mut *lod;
    let name = content.generator.name();
    lod.check_settings(&settings, settings.is_changed());
    let Some(valid) = lod.settings.clone() else {
        lod.turn_off(LodOff::InvalidSettings, name);
        return;
    };
    lod.check_content(&content, world.dimension, valid.levels);
    if !valid.enabled {
        lod.turn_off(LodOff::Disabled, name);
        return;
    }
    if content.generator.lod().is_none() {
        lod.turn_off(LodOff::NoSource, name);
        return;
    }
    let Some((eye, view)) = eye_and_view(focus.as_deref(), viewer_eye.0) else {
        lod.turn_off(LodOff::NoEye, name);
        return;
    };
    // A new tree (first plan, content changed, turned on again) reads the coverage as it is.
    lod.tree.seed(coverage.coverage());
    lod.plan(eye, view, &viewers.0, &world, coverage.coverage(), name);
}

/// True once the world has chunks and nothing left to load or mesh (before its first stats, in
/// the first frame, it has none: busy).
fn world_settled(stats: &WorldStats) -> bool {
    stats.chunks > 0 && stats.is_settled()
}

/// `LodSystems::Dispatch`: finished jobs and new ones within the frame budget.
fn lod_dispatch(
    mut lod: ResMut<LodWorld>,
    stats: Res<WorldStats>,
    content: Res<WorldContent>,
    coverage: Res<LodCoverage>,
) {
    if !lod.has_job_work() && !lod.jobs.has_results() {
        return;
    }
    let budget = lod
        .settings
        .as_ref()
        .map_or(Duration::ZERO, |s| s.frame_budget);
    let cap = lod.cap(world_settled(&stats));
    lod.run_jobs(Instant::now() + budget, cap, &content, coverage.coverage());
}

/// `LodSystems::Collect`: the frame's ops, the stats, and an ask for idle steps while jobs are
/// left.
fn lod_collect(
    mut lod: ResMut<LodWorld>,
    mut ops: ResMut<LodOps>,
    coverage: Res<LodCoverage>,
    mut stats: ResMut<LodStats>,
    world: Res<WorldSettings>,
    wanted: Option<ResMut<IdleWanted>>,
) {
    let now = Instant::now();
    let out = lod.collect(coverage.coverage(), now);
    if !out.is_empty() {
        ops.ops.extend(out);
    }
    let fresh = lod.stats(now, &coverage);
    stats.set_if_neq(fresh);
    if let Some(mut wanted) = wanted {
        wanted.want(world.idle_work && lod.has_job_work());
    }
}

/// `Idle` (between frames, ADR 0023): collects and starts jobs until the step ends. Never writes
/// ops.
fn lod_idle(
    mut step: ResMut<IdleStep>,
    mut lod: ResMut<LodWorld>,
    stats: Res<WorldStats>,
    content: Res<WorldContent>,
    world: Res<WorldSettings>,
    coverage: Res<LodCoverage>,
) {
    if !world.idle_work {
        return;
    }
    let cap = lod.cap(world_settled(&stats));
    let pending = lod.idle_pending(cap);
    if pending != IdleWork::Ready || Instant::now() >= step.until {
        step.want(pending);
        return;
    }
    lod.run_jobs(step.until, cap, &content, coverage.coverage());
    step.want(lod.idle_pending(cap));
}
