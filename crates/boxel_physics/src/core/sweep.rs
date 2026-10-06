//! Moving a box through the voxel grid: Minecraft's per-axis clipping, done in the frame of the
//! body's block so every block box is a small exact integer box (ADR 0016).
//!
//! [`move_box`] moves a box by a displacement, one axis at a time (Y first, then the larger of
//! X and Z, X on a tie). On each axis it scans the layers of blocks the leading face reaches,
//! nearest first, over the box's footprint on the other two axes; the first layer with a full
//! block (or an obstacle box) clips the move, and the box is snapped exactly onto that plane.
//! The footprint is strict: a block that only touches the box (within `eps`) is not in it, so a
//! box sliding along a floor or a wall never snags on the seams between blocks or chunks. The
//! cost grows linearly with the distance moved; no substeps are needed.
//!
//! Before moving, a box that overlaps full blocks (a block placed into it, a spawn inside
//! terrain) is pushed out toward the nearest free side within one block.
//!
//! Accepted artifacts, as in Minecraft: a diagonal move can cut an outer corner by up to one
//! step of movement, and the result of a move depends on the axis order (frozen by tests).

use super::shape::LocalBox;
use super::source::{CollisionTable, Probe, VoxelSource};
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, Face, WorldPos};

/// The largest displacement [`move_box`] accepts per axis, in blocks; larger components are
/// clamped to it. A guard against runaway values (the scan cost grows with the distance): a body
/// at 100 blocks per second moves 1.6 blocks per 64 Hz step.
pub const MAX_SWEEP_DISTANCE: f64 = 1024.0;

/// Tuning of [`move_box`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SweepConfig {
    /// Contact tolerance, in blocks (default `1e-7`). A box within `eps` of a block touches it
    /// and never counts as overlapping it. Positions are local to a block, so this tolerance
    /// means the same everywhere in the world.
    pub eps: f64,
    /// The most a box that starts inside full blocks is pushed out per call, in blocks (default
    /// 1).
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
    /// full, else the first full block found under the box.
    pub ground: Option<BlockPos>,
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
/// full blocks of `src` and the `obstacles` (boxes in the frame of `start.block`: the
/// positions of static or moving solid entities).
///
/// Steps: push out of full blocks if the box starts inside them (toward the nearest free side
/// within `cfg.max_depenetration`, ties in the order +Y, -Y, +X, -X, +Z, -Z; straight up when no
/// side is free), then clip Y, then the larger of X and Z, then the other. A clipped axis ends
/// with the box's face exactly on the blocking plane (`position = plane - face offset`, so a
/// resting box computes the same position every time and never drifts).
///
/// `shape` must be larger than `2 * cfg.eps` on every axis. Components of `d` that are not
/// finite count as 0, and each component is clamped to [`MAX_SWEEP_DISTANCE`].
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
    debug_assert!(
        (shape.max - shape.min)
            .cmpgt(DVec3::splat(2.0 * cfg.eps))
            .all(),
        "move_box: shape {shape:?} is not larger than 2 * eps"
    );
    let clean = |v: f64| {
        if v.is_finite() {
            v.clamp(-MAX_SWEEP_DISTANCE, MAX_SWEEP_DISTANCE)
        } else {
            0.0
        }
    };
    let d = DVec3::new(clean(d.x), clean(d.y), clean(d.z));
    let mut grid = Grid {
        src,
        table,
        origin: start.block,
        eps: cfg.eps,
    };
    let mut pos = start.frac;
    let depenetrated = match grid.depenetrate(pos, shape, cfg.max_depenetration)? {
        Some(out) => {
            pos = out;
            true
        }
        None => false,
    };
    let mut faces = 0u8;
    let mut ground = None;
    for axis in axis_order(d) {
        if d[axis] == 0.0 {
            continue;
        }
        let clip = grid.clip_axis(axis, pos, shape, d[axis], obstacles)?;
        match clip {
            Clip::Free => pos[axis] += d[axis],
            Clip::Plane { pos: p, ground: g } => {
                pos[axis] = p;
                faces |= 1 << face_of(axis, d[axis] > 0.0).index();
                if axis == 1 && d[axis] < 0.0 {
                    ground = g;
                }
            }
        }
    }
    let in_fluid = grid.any_fluid(shape.translate(pos))?;
    Ok(SweepOut {
        end: WorldPos::from_block(start.block, pos),
        moved: pos - start.frac,
        faces,
        ground,
        in_fluid,
        depenetrated,
    })
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

/// First block index of the strict footprint of `[min, max]` on one axis: blocks the box
/// overlaps by more than `eps`.
#[inline]
fn first_block(min: f64, eps: f64) -> i64 {
    (min + eps).floor() as i64
}

/// Last block index of the strict footprint (see [`first_block`]); smaller than the first when
/// the box is thinner than `2 * eps`.
#[inline]
fn last_block(max: f64, eps: f64) -> i64 {
    (max - eps).ceil() as i64 - 1
}

/// How one axis of a move ended.
enum Clip {
    /// Nothing in the way: the whole displacement.
    Free,
    /// Stopped: the new position on that axis, and the supporting block for a downward clip on
    /// blocks.
    Plane { pos: f64, ground: Option<BlockPos> },
}

/// The voxel grid seen from the frame of `origin`: block `(x, y, z)` of the frame is world
/// block `origin + (x, y, z)`.
struct Grid<'a, S: ?Sized> {
    src: &'a mut S,
    table: &'a CollisionTable,
    origin: BlockPos,
    eps: f64,
}

impl<S: VoxelSource + ?Sized> Grid<'_, S> {
    /// The world position of frame block `b`.
    #[inline]
    fn world(&self, b: [i64; 3]) -> BlockPos {
        BlockPos::new(
            self.origin.x + b[0],
            self.origin.y + b[1],
            self.origin.z + b[2],
        )
    }

    /// Frame block `b` is full.
    #[inline]
    fn full(&mut self, b: [i64; 3]) -> Result<bool, Unloaded> {
        let block = self.world(b);
        match self.src.probe(block) {
            Probe::Block(state) => Ok(self.table.is_full(state)),
            Probe::Unloaded => Err(Unloaded { block }),
        }
    }

    /// Some full block overlaps `bx` by more than `eps` on every axis.
    fn any_full(&mut self, bx: LocalBox) -> Result<bool, Unloaded> {
        let eps = self.eps;
        for y in first_block(bx.min.y, eps)..=last_block(bx.max.y, eps) {
            for x in first_block(bx.min.x, eps)..=last_block(bx.max.x, eps) {
                for z in first_block(bx.min.z, eps)..=last_block(bx.max.z, eps) {
                    if self.full([x, y, z])? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Some fluid block overlaps `bx` by more than `eps` on every axis.
    fn any_fluid(&mut self, bx: LocalBox) -> Result<bool, Unloaded> {
        let eps = self.eps;
        for y in first_block(bx.min.y, eps)..=last_block(bx.max.y, eps) {
            for x in first_block(bx.min.x, eps)..=last_block(bx.max.x, eps) {
                for z in first_block(bx.min.z, eps)..=last_block(bx.max.z, eps) {
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

    /// The first full block of layer `layer` on `axis` within the footprint `b` x `c` (on the
    /// other two axes, in increasing order), scanning `b` then `c` upward.
    fn first_full_in_layer(
        &mut self,
        axis: usize,
        layer: i64,
        (b_axis, c_axis): (usize, usize),
        b: (i64, i64),
        c: (i64, i64),
    ) -> Result<Option<[i64; 3]>, Unloaded> {
        for bi in b.0..=b.1 {
            for ci in c.0..=c.1 {
                let mut block = [0; 3];
                block[axis] = layer;
                block[b_axis] = bi;
                block[c_axis] = ci;
                if self.full(block)? {
                    return Ok(Some(block));
                }
            }
        }
        Ok(None)
    }

    /// Moves along one axis: the nearest blocking layer or obstacle within `d` clips it.
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
        let b = (
            first_block(bx.min[b_axis], eps),
            last_block(bx.max[b_axis], eps),
        );
        let c = (
            first_block(bx.min[c_axis], eps),
            last_block(bx.max[c_axis], eps),
        );
        // An obstacle is in the way if it overlaps the footprint strictly.
        let across = |o: &LocalBox| {
            o.min[b_axis] < bx.max[b_axis] - eps
                && o.max[b_axis] > bx.min[b_axis] + eps
                && o.min[c_axis] < bx.max[c_axis] - eps
                && o.max[c_axis] > bx.min[c_axis] + eps
        };
        if d > 0.0 {
            let lead = bx.max[axis];
            // Layers whose min plane the leading face has not passed (by more than eps), up to
            // the last one the moved face enters.
            let first = (lead - eps).ceil() as i64;
            let last = (lead + d).ceil() as i64 - 1;
            let mut plane = f64::INFINITY;
            for layer in first..=last {
                if self
                    .first_full_in_layer(axis, layer, (b_axis, c_axis), b, c)?
                    .is_some()
                {
                    plane = layer as f64;
                    break;
                }
            }
            for o in obstacles.iter().filter(|o| across(o)) {
                if o.min[axis] >= lead - eps && o.min[axis] < plane {
                    plane = o.min[axis];
                }
            }
            if plane < lead + d {
                return Ok(Clip::Plane {
                    pos: plane - shape.max[axis],
                    ground: None,
                });
            }
        } else {
            let lead = bx.min[axis];
            // Layers whose top plane the leading face has not passed, down to the last one the
            // moved face enters.
            let first = (lead + eps).floor() as i64 - 1;
            let last = (lead + d).floor() as i64;
            let mut plane = f64::NEG_INFINITY;
            let mut block = None;
            let mut layer = first;
            while layer >= last {
                if let Some(found) =
                    self.first_full_in_layer(axis, layer, (b_axis, c_axis), b, c)?
                {
                    plane = (layer + 1) as f64;
                    block = Some(found);
                    break;
                }
                layer -= 1;
            }
            for o in obstacles.iter().filter(|o| across(o)) {
                if o.max[axis] <= lead + eps && o.max[axis] > plane {
                    plane = o.max[axis];
                    block = None;
                }
            }
            if plane > lead + d {
                let ground = match block {
                    Some(found) if axis == 1 => Some(self.ground(found, bx, b, c)?),
                    _ => None,
                };
                return Ok(Clip::Plane {
                    pos: plane - shape.min[axis],
                    ground,
                });
            }
        }
        Ok(Clip::Free)
    }

    /// The supporting block of a downward clip on layer `found[1]`: the block under the center
    /// of the box's bottom if it is full and in the footprint, else `found` (the first full
    /// block of the layer).
    fn ground(
        &mut self,
        found: [i64; 3],
        bx: LocalBox,
        x: (i64, i64),
        z: (i64, i64),
    ) -> Result<BlockPos, Unloaded> {
        let center = bx.center();
        let under = [
            (center.x.floor() as i64).clamp(x.0, x.1),
            found[1],
            (center.z.floor() as i64).clamp(z.0, z.1),
        ];
        Ok(if under != found && self.full(under)? {
            self.world(under)
        } else {
            self.world(found)
        })
    }

    /// Pushes a box that overlaps full blocks out of them: the new position, or `None` when the
    /// box is free. See [`move_box`] for the rule.
    fn depenetrate(
        &mut self,
        pos: DVec3,
        shape: LocalBox,
        max: f64,
    ) -> Result<Option<DVec3>, Unloaded> {
        if !self.any_full(shape.translate(pos))? {
            return Ok(None);
        }
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
        Ok(Some(match best {
            Some((_, out)) => out,
            None => pos + DVec3::new(0.0, max, 0.0),
        }))
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
                if distance > 0.0 && !self.any_full(shape.translate(out))? {
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
                if distance > 0.0 && !self.any_full(shape.translate(out))? {
                    return Ok(Some((distance, out)));
                }
                k -= 1;
            }
        }
    }
}
