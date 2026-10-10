//! Moving a box through the voxel grid: Minecraft's per-axis clipping, done in the frame of the
//! body's block so every block box is a small exact integer box (ADR 0016).
//!
//! [`move_box`] moves a box by a displacement, one axis at a time (Y first, then the larger of
//! X and Z, X on a tie). On each axis it scans the layers of blocks the leading face reaches,
//! nearest first, over the box's footprint on the other two axes; the first layer with a solid
//! block (or an obstacle box) clips the move, and the box is snapped exactly onto that plane.
//! A block made of boxes (a slab, ADR 0018) blocks at the nearest face of its boxes that overlap
//! the footprint; with such blocks in the table the layer the leading face is in is scanned too.
//! A box may reach into the cell above (a fence collides 1.5 blocks tall, ADR 0026): a move
//! across also looks at the row of blocks under the footprint, for the tall blocks there whose
//! boxes reach it, and a move down scans one layer further, where a tall block's top can be
//! higher than the tops of the layer above it. A move whose blocks (and the row under them) are
//! all in loaded chunks without tall blocks ([`VoxelSource::tall_free`], asked once a move,
//! usually about one chunk) runs the code without those reads: the result is the same, and the
//! cost of tall blocks is paid only near them.
//! The footprint is strict: a block that only touches the box (within `eps`) is not in it, so a
//! box sliding along a floor or a wall never snags on the seams between blocks or chunks. The
//! cost grows linearly with the distance moved; no substeps are needed.
//!
//! Before moving, a box that overlaps solid blocks (a block placed into it, a spawn inside
//! terrain) is pushed out toward the nearest free side within one block. It only stops on
//! block planes (integers): a box inside a slab is pushed onto the next block plane, not onto
//! the slab's top.
//!
//! [`move_box_stepping`] adds Minecraft's step-up for walking bodies: a move clipped across on
//! the ground is tried again lifted by the step height and kept if it goes further.
//!
//! Accepted artifacts, as in Minecraft: a diagonal move can cut an outer corner by up to one
//! step of movement, and the result of a move depends on the axis order (frozen by tests).

use super::shape::LocalBox;
use super::source::{CollisionShape, CollisionTable, Probe, VoxelSource};
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, CHUNK_MASK, CHUNK_SIZE, ChunkPos, Face, WorldPos};

/// The largest displacement [`move_box`] accepts per axis, in blocks; larger components are
/// clamped to it. A guard against runaway values (the scan cost grows with the distance): a body
/// at 100 blocks per second moves 1.6 blocks per 64 Hz step.
pub const MAX_SWEEP_DISTANCE: f64 = 1024.0;

/// The smallest contact tolerance [`move_box`] uses, in blocks (`2^-32`, about `2.3e-10`): a
/// smaller [`SweepConfig::eps`] is raised to it, so a widened thin shape (see [`move_box`])
/// stays many `f64` steps wide anywhere in a block's frame.
const MIN_EPS: f64 = 1.0 / 4_294_967_296.0;

/// The largest contact tolerance [`move_box`] uses, in blocks: a larger [`SweepConfig::eps`] is
/// lowered to it.
const MAX_EPS: f64 = 1.0 / 64.0;

/// Tuning of [`move_box`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SweepConfig {
    /// Contact tolerance, in blocks (default `1e-7`). A box within `eps` of a block touches it
    /// and never counts as overlapping it. Positions are local to a block, so this tolerance
    /// means the same everywhere in the world. Clamped to `2^-32..=1/64`; NaN counts as
    /// `2^-32`.
    pub eps: f64,
    /// The most a box that starts inside full blocks is pushed out per call, in blocks (default
    /// 1). Clamped to `0..=`[`MAX_SWEEP_DISTANCE`]; NaN counts as 0.
    pub max_depenetration: f64,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            eps: 1e-7,
            max_depenetration: 1.0,
        }
    }
}

/// What [`move_box`] did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SweepOut {
    /// Where the box's position ends.
    pub end: WorldPos,
    /// The movement, `end - start` in the frame of the start's block (depenetration included).
    pub moved: DVec3,
    /// Bit `Face::index()` set for each face of the box that a clip stopped: `Face::NegY` means
    /// it landed (on blocks or on an obstacle), `Face::PosX` that it hit something moving +X.
    pub faces: u8,
    /// When it landed on blocks: the block under the center of the box's bottom if that block is
    /// full, else the first full block found under the box (`blocks[1]` of a downward clip).
    pub ground: Option<BlockPos>,
    /// Per axis (0 = X, 1 = Y, 2 = Z), the full block that clipped the move on that axis: on the
    /// blocking layer, the block in line with the center of the box's leading face if it is
    /// full, else the first full block found there. `None` when the axis moved freely or an
    /// obstacle box clipped it.
    pub blocks: [Option<BlockPos>; 3],
    /// The box ends overlapping a fluid block.
    pub in_fluid: bool,
    /// The box started inside full blocks and was pushed out first.
    pub depenetrated: bool,
}

impl SweepOut {
    /// A clip stopped the box's `face`.
    pub fn touched(&self, face: Face) -> bool {
        self.faces & (1 << face.index()) != 0
    }

    /// It landed: a clip stopped it moving down.
    pub fn on_ground(&self) -> bool {
        self.touched(Face::NegY)
    }
}

/// A probe found a block of a chunk that is not loaded: the move did not happen. Physics never
/// guesses unknown terrain; a body keeps its state and retries later (the freeze policy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unloaded {
    /// The first unloaded block probed.
    pub block: BlockPos,
}

/// Moves the box `shape` (relative to the body's position) from `start` by `d`, against the
/// solid blocks of `src` (full cubes and the boxes of shaped blocks) and the `obstacles` (boxes in the frame of `start.block`: the
/// positions of static or moving solid entities).
///
/// Steps: push out of solid blocks if the box starts inside them (toward the nearest free side
/// within `cfg.max_depenetration`, ties in the order +Y, -Y, +X, -X, +Z, -Z; straight up when no
/// side is free), then clip Y, then the larger of X and Z, then the other. A clipped axis ends
/// with the box's face exactly on the blocking plane (`position = plane - face offset`, so a
/// resting box computes the same position every time and never drifts).
///
/// On an axis where `shape` is not larger than `2 * eps`, it is widened to `4 * eps` around its
/// center (a tiny sphere or a point collides like a box that wide instead of slipping between
/// blocks); a shape with a non-finite corner counts as a point at the position. Components of
/// `d` that are not finite count as 0, and each component is clamped to
/// [`MAX_SWEEP_DISTANCE`]. `start` is made canonical first ([`WorldPos::normalize`]).
///
/// At the world limit the end is clamped onto the limit ([`WorldPos::normalize`]); `moved`
/// then reports the movement that happened.
///
/// # Errors
///
/// [`Unloaded`] when any block the move needs is in an unloaded chunk; nothing moves then.
pub fn move_box<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    start: WorldPos,
    shape: LocalBox,
    d: DVec3,
    obstacles: &[LocalBox],
    cfg: &SweepConfig,
) -> Result<SweepOut, Unloaded> {
    move_box_stepping(src, table, start, shape, d, obstacles, cfg, StepUp::NONE)
}

/// Stepping up low ledges while moving, for walking bodies (a character's step-up): see
/// [`move_box_stepping`]. [`StepUp::NONE`] never steps.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StepUp {
    /// The highest ledge the box steps onto, in blocks (Minecraft's player: 0.6, so a slab or a
    /// stair of slabs is climbed and a full block is not). 0, negative or NaN: no step-up.
    /// Clamped to [`MAX_SWEEP_DISTANCE`].
    pub height: f64,
    /// The body ended its previous step on the ground. A step-up also happens when the move
    /// itself lands; a body in the air never steps up.
    pub grounded: bool,
}

impl StepUp {
    /// No step-up: [`move_box_stepping`] is [`move_box`].
    pub const NONE: Self = Self {
        height: 0.0,
        grounded: false,
    };
}

/// [`move_box`] with a step-up: a walking body climbs ledges up to `step.height` high (a slab,
/// a stair of slabs) without jumping. Minecraft's algorithm, per axis like the rest of the
/// sweep.
///
/// When the plain move is clipped on X or Z, the box did not start inside blocks and it is on
/// the ground (`step.grounded`, or the plain move landed), the move across is tried again
/// lifted:
/// - up by `step.height` (clipped by what is above the box), then across;
/// - if something above the whole path (the box stretched by the move across) is lower than
///   that, up only to it, then across. So a body never steps into a ceiling or through a gap it
///   does not fit in.
///
/// The one that goes further across wins (the first on a tie), and only if it goes further
/// across than the plain move. Then the box goes back down by what it rose plus the plain
/// move's own vertical part, onto the ledge or wherever it lands, so it never ends higher than
/// the ledge. The result reports the clips across of the lifted move and the vertical clip of
/// the way down (it lands on the ledge: `on_ground`). Otherwise the plain move stands. A move
/// that is not clipped across costs nothing more.
///
/// A move clipped by a full block that is taller than `step.height` above the box's bottom (a
/// wall, a hill one block high) and that the box already touches skips the lifted moves: no
/// block or box face fits between the box and that block (see
/// [`CollisionTable::plane_gap`]), so every lifted move would stop at the same place across.
/// The result is the plain move, the same that trying them gives, bit for bit.
///
/// # Errors
///
/// [`Unloaded`] when any block the move or the step-up needs is in an unloaded chunk; nothing
/// moves then. A step-up that cannot go further across whatever the unloaded blocks hold is
/// skipped without reading them.
#[expect(
    clippy::too_many_arguments,
    reason = "move_box's arguments and the step-up"
)]
#[inline(always)]
pub fn move_box_stepping<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    start: WorldPos,
    shape: LocalBox,
    d: DVec3,
    obstacles: &[LocalBox],
    cfg: &SweepConfig,
    step: StepUp,
) -> Result<SweepOut, Unloaded> {
    stepping(src, table, start, shape, d, obstacles, cfg, step, true)
}

/// [`move_box_stepping`] that always tries the lifted moves (no early skip): the reference the
/// tests compare the skip with. Not for games.
#[doc(hidden)]
#[expect(
    clippy::too_many_arguments,
    reason = "move_box's arguments and the step-up"
)]
pub fn move_box_stepping_unpruned<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    start: WorldPos,
    shape: LocalBox,
    d: DVec3,
    obstacles: &[LocalBox],
    cfg: &SweepConfig,
    step: StepUp,
) -> Result<SweepOut, Unloaded> {
    stepping(src, table, start, shape, d, obstacles, cfg, step, false)
}

/// [`move_box_stepping`]; `prune` allows skipping a step-up that cannot win.
#[expect(
    clippy::too_many_arguments,
    reason = "move_box_stepping's arguments and a switch"
)]
fn stepping<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    start: WorldPos,
    shape: LocalBox,
    d: DVec3,
    obstacles: &[LocalBox],
    cfg: &SweepConfig,
    step: StepUp,
    prune: bool,
) -> Result<SweepOut, Unloaded> {
    // A hand-built start may be outside the canonical form (a NaN frac, a block past i64 room).
    let start = start.normalize();
    let d = DVec3::new(clean(d.x), clean(d.y), clean(d.z));
    let eps = effective_eps(cfg.eps);
    let shape = usable_shape(shape, eps);
    let max_depenetration = if cfg.max_depenetration >= 0.0 {
        cfg.max_depenetration.min(MAX_SWEEP_DISTANCE)
    } else {
        0.0
    };
    let mv = Move {
        start,
        shape,
        d,
        obstacles,
        eps,
        max_depenetration,
        step,
        prune,
    };
    // Away from tall blocks the move reads as without them (the same result, fewer reads).
    if table.reach() > 0.0 && !region_tall_free(src, table, &mv) {
        stepping_on::<S, true>(src, table, &mv, false)
    } else {
        stepping_on::<S, false>(src, table, &mv, false)
    }
}

/// The arguments of one [`move_box_stepping`], cleaned.
struct Move<'o> {
    /// Canonical.
    start: WorldPos,
    /// [`usable_shape`].
    shape: LocalBox,
    /// [`clean_distance`].
    d: DVec3,
    obstacles: &'o [LocalBox],
    /// [`effective_eps`].
    eps: f64,
    /// Not negative, at most [`MAX_SWEEP_DISTANCE`].
    max_depenetration: f64,
    step: StepUp,
    /// Skipping a step-up that cannot win is allowed.
    prune: bool,
}

/// No block that `mv` reads only for tall boxes can hold one ([`tall_free_blocks`]): read
/// as without tall blocks (a [`Grid`] without `TALL`), the move gives the same result.
///
/// Every box of the move, its lifted moves and the step-up's checks included, stays within the
/// hull of the box at the start, at the end and raised by the step height (also at the end),
/// widened by a few `eps` (a clip snaps a box up to `eps` back). Its scans read the blocks that
/// hull overlaps and one row under it: the row under a footprint across, and the layer a move
/// down reads past its end. A box pushed out of blocks first leaves that region:
/// [`stepping_on`] then starts again with `TALL`.
#[inline]
fn region_tall_free<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    mv: &Move<'_>,
) -> bool {
    // Only a body on the ground can step up, but the region is the same for all walkers.
    let height = if mv.step.height > 0.0 {
        clean(mv.step.height)
    } else {
        0.0
    };
    let margin = 8.0 * mv.eps;
    let pos = mv.start.frac;
    let mut lo = mv.shape.min + pos + mv.d.min(DVec3::ZERO) - margin;
    let mut hi = mv.shape.max + pos + mv.d.max(DVec3::ZERO) + margin;
    hi.y += height;
    lo.y -= 1.0;
    // Usually all of it is in the chunk of the start's block: block `b` of the frame is when
    // `-local <= b < 32 - local` on every axis, with `local` the start block's place in its
    // chunk, which bounds `lo` and `hi` without rounding them.
    let block = mv.start.block;
    let mask = i64::from(CHUNK_MASK);
    let local = DVec3::new(
        (block.x & mask) as f64,
        (block.y & mask) as f64,
        (block.z & mask) as f64,
    );
    if lo.cmpge(-local).all() && hi.cmplt(DVec3::splat(f64::from(CHUNK_SIZE)) - local).all() {
        return src.tall_free(ChunkPos::containing(block), table);
    }
    // Saturating: a region past the world's room spans too many chunks and is not free.
    let corner = |v: DVec3| {
        let o = mv.start.block;
        BlockPos::new(
            o.x.saturating_add(floor_i64(v.x)),
            o.y.saturating_add(floor_i64(v.y)),
            o.z.saturating_add(floor_i64(v.z)),
        )
    };
    tall_free_blocks(src, table, corner(lo), corner(hi))
}

/// [`stepping`] on a [`Grid`] that reads the cells under what it crosses for tall blocks
/// (`TALL`) or not: without `TALL`, a box that starts inside blocks starts again with it when
/// the table has tall blocks (pushed out, it leaves the region [`region_tall_free`] checked),
/// with `inside` set (it is inside blocks with `TALL` too: that reads more blocks, never fewer).
fn stepping_on<S: VoxelSource + ?Sized, const TALL: bool>(
    src: &mut S,
    table: &CollisionTable,
    mv: &Move<'_>,
    inside: bool,
) -> Result<SweepOut, Unloaded> {
    let Move {
        start,
        shape,
        d,
        obstacles,
        eps,
        max_depenetration,
        step,
        prune,
    } = *mv;
    let mut grid = Grid::<S, TALL> {
        src,
        table,
        origin: start.block,
        eps,
    };
    let mut pos = start.frac;
    let depenetrated = inside || grid.any_solid(shape.translate(pos))?;
    if depenetrated {
        if !TALL && table.reach() > 0.0 {
            return stepping_on::<S, true>(grid.src, table, mv, true);
        }
        pos = grid.push_out(pos, shape, max_depenetration)?;
    }
    let mut out = grid.sweep(pos, shape, d, obstacles)?;
    // Only a move clipped across, on the ground, steps up (`>` is false for NaN).
    if step.height > 0.0
        && out.faces & ACROSS != 0
        && !depenetrated
        && (step.grounded || out.faces & LANDED != 0)
        && !(prune && grid.step_up_blocked(pos, shape, d, obstacles, clean(step.height), &out))
        && let Some(lifted) = grid.step_up(pos, shape, d, obstacles, clean(step.height), &out)?
    {
        out = lifted;
    }
    let pos = out.pos;
    let in_fluid = grid.any_fluid(shape.translate(pos))?;
    let end = WorldPos::from_block(start.block, pos);
    // `end` holds `pos` exactly unless it was clamped at the world limit (or a tiny negative
    // frac carried): then the movement is measured to the end itself.
    let moved = if end.block.as_i64vec3() == start.block.as_i64vec3() + pos.floor().as_i64vec3() {
        pos - start.frac
    } else {
        start.delta_to(&end)
    };
    Ok(SweepOut {
        end,
        moved,
        faces: out.faces,
        ground: out.ground,
        blocks: out.blocks,
        in_fluid,
        depenetrated,
    })
}

/// The face bits of the four horizontal faces: a clip across.
const ACROSS: u8 = (1 << Face::PosX.index())
    | (1 << Face::NegX.index())
    | (1 << Face::PosZ.index())
    | (1 << Face::NegZ.index());

/// The face bit of a landing.
const LANDED: u8 = 1 << Face::NegY.index();

/// The face bit of a clip moving up.
const CEILING: u8 = 1 << Face::PosY.index();

/// A displacement as the sweep uses it (see [`clean`]).
pub(crate) fn clean_distance(d: DVec3) -> DVec3 {
    DVec3::new(clean(d.x), clean(d.y), clean(d.z))
}

/// A displacement component as the sweep uses it: not finite counts as 0, and it is clamped to
/// [`MAX_SWEEP_DISTANCE`].
fn clean(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(-MAX_SWEEP_DISTANCE, MAX_SWEEP_DISTANCE)
    } else {
        0.0
    }
}

/// The contact tolerance [`move_box`] uses for a configured `eps`: clamped to
/// `2^-32..=1/64`, NaN as `2^-32`. Contacts between bodies use the same tolerance.
pub(crate) fn effective_eps(eps: f64) -> f64 {
    // `>=` is false for NaN: NaN counts as the minimum.
    if eps >= MIN_EPS {
        eps.min(MAX_EPS)
    } else {
        MIN_EPS
    }
}

/// `shape` as the sweep uses it (see [`move_box`]): a shape with a non-finite corner becomes a
/// point at the origin, and an axis not larger than `2 * eps` is widened to `4 * eps` around
/// its center, so the strict footprint of the box is never empty.
pub(crate) fn usable_shape(shape: LocalBox, eps: f64) -> LocalBox {
    let mut shape = if shape.min.is_finite() && shape.max.is_finite() {
        shape
    } else {
        LocalBox::default()
    };
    for a in 0..3 {
        if shape.max[a] - shape.min[a] <= 2.0 * eps {
            let center = (shape.min[a] + shape.max[a]) * 0.5;
            shape.min[a] = center - 2.0 * eps;
            shape.max[a] = center + 2.0 * eps;
        }
    }
    shape
}

/// Y, then the larger of |d.x| and |d.z| (X on a tie), then the other. Frozen by tests.
pub fn axis_order(d: DVec3) -> [usize; 3] {
    if d.x.abs() >= d.z.abs() {
        [1, 0, 2]
    } else {
        [1, 2, 0]
    }
}

/// The face of a box on `axis` (0 = X, 1 = Y, 2 = Z) facing the positive or negative side.
fn face_of(axis: usize, positive: bool) -> Face {
    match (axis, positive) {
        (0, true) => Face::PosX,
        (0, false) => Face::NegX,
        (1, true) => Face::PosY,
        (1, false) => Face::NegY,
        (_, true) => Face::PosZ,
        (_, false) => Face::NegZ,
    }
}

/// The other two axes, in increasing order.
fn others(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

/// The strict footprint of `[min, max]` on one axis: the first and last index of the blocks the
/// box overlaps by more than `eps` (none when the box is not larger than `2 * eps`, which
/// [`move_box`] prevents by widening such shapes).
#[inline]
fn footprint(min: f64, max: f64, eps: f64) -> (i64, i64) {
    ((min + eps).floor() as i64, (max - eps).ceil() as i64 - 1)
}

/// `x.floor() as i64` (saturating) without calling `floor`, which is a library call on x86-64
/// without SSE4.1: a few nanoseconds each, more than the rest of the check that uses it.
#[inline]
pub(crate) fn floor_i64(x: f64) -> i64 {
    let t = x as i64;
    t.saturating_sub(i64::from((t as f64) > x))
}

/// The most chunks [`tall_free_blocks`] asks about; a larger box (a move of tens of blocks)
/// counts as not free.
const MAX_TALL_FREE_CHUNKS: u64 = 8;

/// Every block of `min..=max` (world blocks, inclusive on every axis) is in a loaded chunk
/// without tall blocks of `table` ([`VoxelSource::tall_free`], asked once per chunk: usually
/// one or two). Reading those blocks only for tall boxes then finds nothing and no unloaded
/// chunk. `false` when the box spans more than [`MAX_TALL_FREE_CHUNKS`] chunks; `true` for an
/// empty box (a `min` past its `max`).
#[inline(never)]
pub(crate) fn tall_free_blocks<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    min: BlockPos,
    max: BlockPos,
) -> bool {
    if min.x > max.x || min.y > max.y || min.z > max.z {
        return true;
    }
    let (lo, hi) = (ChunkPos::containing(min), ChunkPos::containing(max));
    if lo == hi {
        return src.tall_free(lo, table);
    }
    // `hi` is not below `lo` on any axis: the spans fit in a u64.
    let span = |a: i64, b: i64| b.abs_diff(a) + 1;
    let chunks = span(lo.x, hi.x)
        .saturating_mul(span(lo.y, hi.y))
        .saturating_mul(span(lo.z, hi.z));
    if chunks > MAX_TALL_FREE_CHUNKS {
        return false;
    }
    for y in lo.y..=hi.y {
        for x in lo.x..=hi.x {
            for z in lo.z..=hi.z {
                if !src.tall_free(ChunkPos::new(x, y, z), table) {
                    return false;
                }
            }
        }
    }
    true
}

/// How one axis of a move ended.
enum Clip {
    /// Nothing in the way: the whole displacement.
    Free,
    /// Stopped: the new position on that axis, and the block that stopped it (`None` for an
    /// obstacle).
    Plane { pos: f64, block: Option<BlockPos> },
}

/// Where the per-axis clips of one move ([`Grid::sweep`]) left the box, in the grid's frame.
#[derive(Clone, Copy)]
struct Moved {
    pos: DVec3,
    /// As [`SweepOut::faces`].
    faces: u8,
    /// As [`SweepOut::ground`].
    ground: Option<BlockPos>,
    /// As [`SweepOut::blocks`].
    blocks: [Option<BlockPos>; 3],
}

/// The voxel grid seen from the frame of `origin`: block `(x, y, z)` of the frame is world
/// block `origin + (x, y, z)`.
///
/// With `TALL`, a box of a block reaches up to the table's [`CollisionTable::reach`] above its
/// cell, so scans also read the cells under what they cross; without it they read as if the
/// table had no tall block: for a table without them, or a move that no tall block can reach
/// ([`region_tall_free`]). Two copies of the code: without `TALL` it is as lean as before tall
/// blocks existed.
struct Grid<'a, S: ?Sized, const TALL: bool> {
    src: &'a mut S,
    table: &'a CollisionTable,
    origin: BlockPos,
    eps: f64,
}

impl<'a, S: VoxelSource + ?Sized, const TALL: bool> Grid<'a, S, TALL> {
    /// How far above its cell a box of a block the grid reads can reach.
    #[inline]
    fn reach(&self) -> f64 {
        if TALL { self.table.reach() } else { 0.0 }
    }

    /// The world position of frame block `b`.
    #[inline]
    fn world(&self, b: [i64; 3]) -> BlockPos {
        BlockPos::new(
            self.origin.x + b[0],
            self.origin.y + b[1],
            self.origin.z + b[2],
        )
    }

    /// The collision shape of frame block `b`.
    #[inline]
    fn shape(&mut self, b: [i64; 3]) -> Result<CollisionShape<'a>, Unloaded> {
        self.shape_in(b, false)
    }

    /// The collision shape of frame block `b`; for a block in the row under a box's footprint
    /// (`under`), only the boxes of a tall block, the only ones that can reach the box (a full
    /// block there is the floor under it, not in its way).
    #[inline(always)]
    fn shape_in(&mut self, b: [i64; 3], under: bool) -> Result<CollisionShape<'a>, Unloaded> {
        let block = self.world(b);
        match self.src.probe(block) {
            // Without `TALL` nothing is read under a box.
            Probe::Block(state) if TALL && under && !self.table.is_tall(state) => {
                Ok(CollisionShape::Empty)
            }
            Probe::Block(state) => Ok(self.table.shape(state)),
            Probe::Unloaded => Err(Unloaded { block }),
        }
    }

    /// The tall blocks of the row under `bx` (frame layer `low - 1`, with `low` the first layer
    /// of its strict footprint) can reach `bx`: their boxes rise up to the table's reach above
    /// that row, past the bottom of `bx` (by more than `eps`).
    #[inline]
    fn reaches(&self, bx: LocalBox, low: i64) -> bool {
        let reach = self.reach();
        reach > 0.0 && low as f64 + reach > bx.min.y + self.eps
    }

    /// The plane where frame block `b` stops `bx` moving along `axis` (toward `+` if
    /// `positive`), if it does: for a full block its near face, for boxes the nearest near face
    /// of the ones that overlap `bx` strictly on the other two axes. Only planes ahead of the
    /// leading face `lead` count (not behind it by more than `eps`). A block under the frame
    /// layer `below` (the row under the footprint of a move across) only counts through the
    /// boxes of a tall block.
    fn block_plane(
        &mut self,
        b: [i64; 3],
        axis: usize,
        positive: bool,
        bx: LocalBox,
        lead: f64,
        below: i64,
    ) -> Result<Option<f64>, Unloaded> {
        let eps = self.eps;
        let ahead = |p: f64| {
            if positive {
                p >= lead - eps
            } else {
                p <= lead + eps
            }
        };
        Ok(match self.shape_in(b, b[1] < below)? {
            CollisionShape::Empty => None,
            CollisionShape::Full => {
                let p = if positive { b[axis] } else { b[axis] + 1 } as f64;
                ahead(p).then_some(p)
            }
            CollisionShape::Boxes(boxes) => {
                let (b_axis, c_axis) = others(axis);
                let at = DVec3::new(b[0] as f64, b[1] as f64, b[2] as f64);
                let mut best: Option<f64> = None;
                for w in boxes.iter().map(|o| o.translate(at)) {
                    let across = w.min[b_axis] < bx.max[b_axis] - eps
                        && w.max[b_axis] > bx.min[b_axis] + eps
                        && w.min[c_axis] < bx.max[c_axis] - eps
                        && w.max[c_axis] > bx.min[c_axis] + eps;
                    let p = if positive { w.min[axis] } else { w.max[axis] };
                    if across
                        && ahead(p)
                        && best.is_none_or(|q| if positive { p < q } else { p > q })
                    {
                        best = Some(p);
                    }
                }
                best
            }
        })
    }

    /// The strict footprint of `bx` on `axis` (see [`footprint`]).
    #[inline]
    fn span(&self, bx: LocalBox, axis: usize) -> (i64, i64) {
        footprint(bx.min[axis], bx.max[axis], self.eps)
    }

    /// Some solid block (a full one, or one of its boxes) overlaps `bx` by more than `eps` on
    /// every axis (the tall blocks of the row under it included).
    fn any_solid(&mut self, bx: LocalBox) -> Result<bool, Unloaded> {
        let ((x0, x1), (y0, y1), (z0, z1)) = (self.span(bx, 0), self.span(bx, 1), self.span(bx, 2));
        let low = if self.reaches(bx, y0) { y0 - 1 } else { y0 };
        for y in low..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    let hit = match self.shape_in([x, y, z], y < y0)? {
                        CollisionShape::Full => true,
                        CollisionShape::Boxes(boxes) => {
                            let at = DVec3::new(x as f64, y as f64, z as f64);
                            boxes.iter().any(|o| o.translate(at).overlaps(bx, self.eps))
                        }
                        CollisionShape::Empty => false,
                    };
                    if hit {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Some fluid block overlaps `bx` by more than `eps` on every axis.
    fn any_fluid(&mut self, bx: LocalBox) -> Result<bool, Unloaded> {
        let ((x0, x1), (y0, y1), (z0, z1)) = (self.span(bx, 0), self.span(bx, 1), self.span(bx, 2));
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    let block = self.world([x, y, z]);
                    match self.src.probe(block) {
                        Probe::Block(state) if self.table.is_fluid(state) => return Ok(true),
                        Probe::Block(_) => {}
                        Probe::Unloaded => return Err(Unloaded { block }),
                    }
                }
            }
        }
        Ok(false)
    }

    /// The nearest blocking plane of layer `layer` on `axis` within the footprint `b` x `c` (on
    /// the other two axes, in increasing order) and its block ([`Grid::block_plane`], with
    /// `below`), scanning `b` then `c` upward: the first block with the nearest plane. The scan
    /// stops at the first block with the nearest plane any block of the layer can have: a full
    /// block's, except moving down with tall blocks in the table, whose tops reach
    /// [`CollisionTable::reach`] above it (a fence beside a stone block is the ground, not the
    /// stone). Without tall blocks this is the first full block of a layer of full blocks.
    #[expect(clippy::too_many_arguments, reason = "an internal helper of clip_axis")]
    fn first_in_layer(
        &mut self,
        axis: usize,
        positive: bool,
        layer: i64,
        bx: LocalBox,
        lead: f64,
        b: (i64, i64),
        c: (i64, i64),
        below: i64,
    ) -> Result<Option<(f64, [i64; 3])>, Unloaded> {
        let (b_axis, c_axis) = others(axis);
        let nearest = if positive {
            layer as f64
        } else if axis == 1 {
            (layer + 1) as f64 + self.reach()
        } else {
            (layer + 1) as f64
        };
        let mut best: Option<(f64, [i64; 3])> = None;
        for bi in b.0..=b.1 {
            for ci in c.0..=c.1 {
                let mut block = [0; 3];
                block[axis] = layer;
                block[b_axis] = bi;
                block[c_axis] = ci;
                if let Some(p) = self.block_plane(block, axis, positive, bx, lead, below)? {
                    if best.is_none_or(|(q, _)| if positive { p < q } else { p > q }) {
                        best = Some((p, block));
                    }
                    if p == nearest {
                        return Ok(best);
                    }
                }
            }
        }
        Ok(best)
    }

    /// Moves along one axis: the nearest blocking layer or obstacle within `d` clips it. The
    /// nearest obstacle is found first and the layer scan stops at it: blocks behind an
    /// obstacle are never probed (they may be in an unloaded chunk the box never reaches).
    ///
    /// Across (X or Z), the footprint also takes the row under it when the tall blocks there
    /// can reach the box ([`Grid::reaches`]), counting only their boxes. Down, a layer whose
    /// top is found may still be beaten by a tall block of the layer under it, so the scan goes
    /// one layer further while that can be ([`CollisionTable::reach`]).
    fn clip_axis(
        &mut self,
        axis: usize,
        pos: DVec3,
        shape: LocalBox,
        d: f64,
        obstacles: &[LocalBox],
    ) -> Result<Clip, Unloaded> {
        let eps = self.eps;
        let bx = shape.translate(pos);
        let (b_axis, c_axis) = others(axis);
        let mut b = self.span(bx, b_axis);
        let mut c = self.span(bx, c_axis);
        // Across, the strict footprint starts at layer `below` on Y; the tall blocks of the row
        // under it can reach the box.
        let below = if axis == 1 {
            i64::MIN
        } else {
            let y = if b_axis == 1 { &mut b } else { &mut c };
            let low = y.0;
            if self.reaches(bx, low) {
                y.0 = low - 1;
            }
            low
        };
        // An obstacle is in the way if it overlaps the footprint strictly.
        let across = |o: &&LocalBox| {
            o.min[b_axis] < bx.max[b_axis] - eps
                && o.max[b_axis] > bx.min[b_axis] + eps
                && o.min[c_axis] < bx.max[c_axis] - eps
                && o.max[c_axis] > bx.min[c_axis] + eps
        };
        if d > 0.0 {
            let lead = bx.max[axis];
            // The nearest obstacle face ahead (infinity when none).
            let mut plane = obstacles
                .iter()
                .filter(across)
                .map(|o| o.min[axis])
                .filter(|&min| min >= lead - eps)
                .fold(f64::INFINITY, f64::min);
            // Layers whose min plane the leading face has not passed (by more than eps), up to
            // the last one the moved face enters, and only those nearer than the obstacle (a
            // block flush with it would stop the box at the same plane). With boxes, also the
            // layer the leading face is in: a box there can still be ahead of it.
            let first = (lead - eps).ceil() as i64;
            let mut last = (lead + d).ceil() as i64 - 1;
            if plane < f64::INFINITY {
                last = last.min(plane.ceil() as i64 - 1);
            }
            let start = if self.table.has_boxes() {
                first - 1
            } else {
                first
            };
            let mut block = None;
            for layer in start..=last {
                if let Some((p, found)) =
                    self.first_in_layer(axis, true, layer, bx, lead, b, c, below)?
                {
                    // Planes grow with the layer: the first layer with one decides.
                    if p < plane {
                        plane = p;
                        block = Some(found);
                    }
                    break;
                }
            }
            if plane < lead + d {
                let block = match block {
                    Some(found) => {
                        Some(self.contact_block(axis, true, found, bx, plane, b, c, below)?)
                    }
                    None => None,
                };
                return Ok(Clip::Plane {
                    pos: plane - shape.max[axis],
                    block,
                });
            }
        } else {
            let lead = bx.min[axis];
            // The nearest obstacle face below (minus infinity when none).
            let mut plane = obstacles
                .iter()
                .filter(across)
                .map(|o| o.max[axis])
                .filter(|&max| max <= lead + eps)
                .fold(f64::NEG_INFINITY, f64::max);
            // Moving down, a tall block's top is up to `reach` above its layer's top.
            let reach = if axis == 1 { self.reach() } else { 0.0 };
            // Layers whose top plane the leading face has not passed, down to the last one the
            // moved face enters, and only those whose top is not below the obstacle (a block
            // flush with it is the one reported as ground).
            let first = (lead + eps).floor() as i64 - 1;
            let mut last = (lead + d - reach).floor() as i64;
            if plane > f64::NEG_INFINITY {
                last = last.max((plane - reach).ceil() as i64 - 1);
            }
            let start = if self.table.has_boxes() {
                first + 1
            } else {
                first
            };
            let mut best: Option<(f64, [i64; 3])> = None;
            let mut layer = start;
            while layer >= last {
                if let Some((p, found)) =
                    self.first_in_layer(axis, false, layer, bx, lead, b, c, below)?
                    && best.is_none_or(|(q, _)| p > q)
                {
                    best = Some((p, found));
                }
                // Planes fall with the layer: the next layer's tops are at most `reach` above
                // this layer's bottom, so it can only beat a plane below that (once: `reach` is
                // under a block).
                if let Some((q, _)) = best
                    && layer as f64 + reach <= q
                {
                    break;
                }
                layer -= 1;
            }
            let mut block = None;
            // A full block's top is never below the obstacle here (it wins ties).
            if let Some((p, found)) = best
                && p >= plane
            {
                plane = p;
                block = Some(found);
            }
            if plane > lead + d {
                let block = match block {
                    Some(found) => {
                        Some(self.contact_block(axis, false, found, bx, plane, b, c, below)?)
                    }
                    None => None,
                };
                return Ok(Clip::Plane {
                    pos: plane - shape.min[axis],
                    block,
                });
            }
        }
        Ok(Clip::Free)
    }

    /// The block that clipped `axis` at `plane` (found on layer `found[axis]`): the block in
    /// line with the center of the box's leading face (clamped to the footprint `b` x `c` on the
    /// other two axes, in increasing order) if it stops the box at the same plane, else `found`
    /// (the first block of the layer with that plane). For a downward clip this is the
    /// supporting block under the center of the box's bottom. `below` as in
    /// [`Grid::block_plane`].
    #[expect(clippy::too_many_arguments, reason = "an internal helper of clip_axis")]
    fn contact_block(
        &mut self,
        axis: usize,
        positive: bool,
        found: [i64; 3],
        bx: LocalBox,
        plane: f64,
        b: (i64, i64),
        c: (i64, i64),
        below: i64,
    ) -> Result<BlockPos, Unloaded> {
        let center = bx.center();
        let (b_axis, c_axis) = others(axis);
        let lead = if positive { bx.max[axis] } else { bx.min[axis] };
        let mut facing = found;
        facing[b_axis] = (center[b_axis].floor() as i64).clamp(b.0, b.1);
        facing[c_axis] = (center[c_axis].floor() as i64).clamp(c.0, c.1);
        Ok(
            if facing != found
                && self.block_plane(facing, axis, positive, bx, lead, below)? == Some(plane)
            {
                self.world(facing)
            } else {
                self.world(found)
            },
        )
    }

    /// The per-axis clips of one move from `pos` by `d` (Y, then the larger of X and Z), in the
    /// grid's frame: no depenetration, no fluid check.
    fn sweep(
        &mut self,
        pos: DVec3,
        shape: LocalBox,
        d: DVec3,
        obstacles: &[LocalBox],
    ) -> Result<Moved, Unloaded> {
        let mut m = Moved {
            pos,
            faces: 0,
            ground: None,
            blocks: [None; 3],
        };
        for axis in axis_order(d) {
            if d[axis] == 0.0 {
                continue;
            }
            match self.clip_axis(axis, m.pos, shape, d[axis], obstacles)? {
                Clip::Free => m.pos[axis] += d[axis],
                Clip::Plane { pos: p, block } => {
                    m.pos[axis] = p;
                    m.faces |= 1 << face_of(axis, d[axis] > 0.0).index();
                    m.blocks[axis] = block;
                    if axis == 1 && d[axis] < 0.0 {
                        m.ground = block;
                    }
                }
            }
        }
        Ok(m)
    }

    /// The step-up of [`move_box_stepping`] from `start` (the box's position, free of blocks):
    /// the lifted move if it goes further across than `plain` (the plain move), else `None`.
    /// `height` is positive and finite. Out of line: inlined, it slowed every plain sweep.
    #[cold]
    #[inline(never)]
    fn step_up(
        &mut self,
        start: DVec3,
        shape: LocalBox,
        d: DVec3,
        obstacles: &[LocalBox],
        height: f64,
        plain: &Moved,
    ) -> Result<Option<Moved>, Unloaded> {
        // The squared distance across: the same order without a square root.
        let across = |m: &Moved| {
            let x = m.pos.x - start.x;
            let z = m.pos.z - start.z;
            x * x + z * z
        };
        let horizontal = DVec3::new(d.x, 0.0, d.z);
        // Up (clipped by what is above the box), then across: Y goes first.
        let mut best = self.sweep(start, shape, DVec3::new(d.x, height, d.z), obstacles)?;
        // What is above the whole path may be lower than that: up only to it, then across.
        // When nothing above the path clips the rise, this is the move above; when the move
        // above went all the way across, nothing beats it.
        if best.faces & ACROSS != 0 {
            let path = LocalBox::new(
                shape.min.min(shape.min + horizontal),
                shape.max.max(shape.max + horizontal),
            );
            let rise = self.sweep(start, path, DVec3::new(0.0, height, 0.0), obstacles)?;
            if rise.faces & CEILING != 0 {
                let lifted = DVec3::new(start.x, rise.pos.y, start.z);
                let low = self.sweep(lifted, shape, horizontal, obstacles)?;
                if across(&low) > across(&best) {
                    best = low;
                }
            }
        }
        if across(&best) <= across(plain) {
            return Ok(None);
        }
        // Back down by what it rose, plus the plain move's own vertical part.
        let down = d.y - (best.pos.y - start.y);
        let landed = self.sweep(best.pos, shape, DVec3::new(0.0, down, 0.0), obstacles)?;
        Ok(Some(Moved {
            pos: landed.pos,
            faces: (best.faces & ACROSS) | landed.faces,
            ground: landed.ground,
            blocks: [best.blocks[0], landed.blocks[1], best.blocks[2]],
        }))
    }

    /// The step-up from `start` cannot change `plain` (the plain move, clipped across): every
    /// lifted move of [`Grid::step_up`] ends across exactly where `plain` does, so none goes
    /// further. Cheap and exact; `false` when it cannot tell (then the step-up is tried).
    ///
    /// The lifted moves go across at heights from `eps` under `start` (a rise stopped at once by
    /// a plane within `eps` above the box) up to `height` above it. Per axis across, in the
    /// sweep's order, with the box where the plain move had it on the axes before:
    /// - Clipped in `plain` by a full block (not an obstacle) that is taller than `height` above
    ///   the box's bottom and starts below its top, at every one of those heights: the block
    ///   is in the footprint of every lifted move, so they stop at its plane or nearer. The box
    ///   is within less than half of [`CollisionTable::plane_gap`] of that plane (it touches the
    ///   wall: the common case, a walker pushing into a hill) and no obstacle face is in between:
    ///   no block or box face fits there, so they stop exactly at the block's plane.
    /// - Free in `plain`: a box that covers every one of those heights moves freely too (one
    ///   clip, usually without reading a block), so every lifted move is free.
    ///
    /// Each lifted move then ends at the same place across as `plain`, bit for bit, and the
    /// step-up keeps `plain`. A block that needs an unloaded chunk gives `false`. Margins of a
    /// few `eps` keep rounding out of every comparison.
    #[inline(never)]
    fn step_up_blocked(
        &mut self,
        start: DVec3,
        shape: LocalBox,
        d: DVec3,
        obstacles: &[LocalBox],
        height: f64,
        plain: &Moved,
    ) -> bool {
        let eps = self.eps;
        let margin = 4.0 * eps;
        let bottom = start.y + shape.min.y;
        let top = start.y + shape.max.y;
        let [_, first, second] = axis_order(d);
        let mut at = start;
        for axis in [first, second] {
            if d[axis] == 0.0 {
                continue;
            }
            let positive = d[axis] > 0.0;
            if plain.faces & (1 << face_of(axis, positive).index()) != 0 {
                let Some(block) = plain.blocks[axis] else {
                    return false;
                };
                let b = [
                    block.x.wrapping_sub(self.origin.x),
                    block.y.wrapping_sub(self.origin.y),
                    block.z.wrapping_sub(self.origin.z),
                ];
                if !matches!(self.shape(b), Ok(CollisionShape::Full)) {
                    return false;
                }
                let by = b[1] as f64;
                if !(bottom + height + margin < by + 1.0 && top - margin > by) {
                    return false;
                }
                let bx = shape.translate(at);
                let (plane, gap) = if positive {
                    let plane = b[axis] as f64;
                    (plane, plane - (bx.max[axis] - eps))
                } else {
                    let plane = (b[axis] + 1) as f64;
                    (plane, (bx.min[axis] + eps) - plane)
                };
                let touching = gap < 0.5 * self.table.plane_gap();
                if !touching {
                    return false;
                }
                let between = |o: &LocalBox| {
                    if positive {
                        o.min[axis] >= bx.max[axis] - eps && o.min[axis] < plane
                    } else {
                        o.max[axis] <= bx.min[axis] + eps && o.max[axis] > plane
                    }
                };
                if obstacles.iter().any(between) {
                    return false;
                }
            } else {
                let mut tall = shape;
                tall.min.y -= margin;
                tall.max.y += height + margin;
                // A sweep along this axis alone (it clips that axis only).
                let mut along = DVec3::ZERO;
                along[axis] = d[axis];
                if !self
                    .sweep(at, tall, along, obstacles)
                    .is_ok_and(|m| m.faces & ACROSS == 0)
                {
                    return false;
                }
            }
            at[axis] = plain.pos[axis];
        }
        true
    }

    /// Pushes a box that overlaps solid blocks ([`Grid::any_solid`]) out of them: the new
    /// position. See [`move_box`] for the rule.
    fn push_out(&mut self, pos: DVec3, shape: LocalBox, max: f64) -> Result<DVec3, Unloaded> {
        let mut best: Option<(f64, DVec3)> = None;
        for (axis, positive) in [
            (1, true),
            (1, false),
            (0, true),
            (0, false),
            (2, true),
            (2, false),
        ] {
            if let Some((distance, out)) = self.escape(pos, shape, axis, positive, max)?
                && best.is_none_or(|(b, _)| distance < b)
            {
                best = Some((distance, out));
            }
        }
        Ok(match best {
            Some((_, out)) => out,
            None => pos + DVec3::new(0.0, max, 0.0),
        })
    }

    /// The nearest free position moving along one axis direction, at most `max` away: the box
    /// face behind it lands exactly on a block plane. `(distance, position)`.
    fn escape(
        &mut self,
        pos: DVec3,
        shape: LocalBox,
        axis: usize,
        positive: bool,
        max: f64,
    ) -> Result<Option<(f64, DVec3)>, Unloaded> {
        let eps = self.eps;
        let bx = shape.translate(pos);
        if positive {
            // The bottom face moves up onto plane k.
            let mut k = (bx.min[axis] - eps).ceil() as i64;
            loop {
                let distance = k as f64 - bx.min[axis];
                if distance > max + eps {
                    return Ok(None);
                }
                let mut out = pos;
                out[axis] = k as f64 - shape.min[axis];
                if distance > 0.0 && !self.any_solid(shape.translate(out))? {
                    return Ok(Some((distance, out)));
                }
                k += 1;
            }
        } else {
            // The top face moves down onto plane k.
            let mut k = (bx.max[axis] + eps).floor() as i64;
            loop {
                let distance = bx.max[axis] - k as f64;
                if distance > max + eps {
                    return Ok(None);
                }
                let mut out = pos;
                out[axis] = k as f64 - shape.max[axis];
                if distance > 0.0 && !self.any_solid(shape.translate(out))? {
                    return Ok(Some((distance, out)));
                }
                k -= 1;
            }
        }
    }
}
