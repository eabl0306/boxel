//! Minecraft's sneak edge guard (ADR 0026): a sneaking character on the ground never walks off a
//! ledge deeper than it would step down.
//!
//! Minecraft (`Player::maybeBackOffFromEdge`) shortens the move on X, then on Z, then both,
//! by 0.05 blocks at a time while the player's box, moved there and lowered by its step
//! height, would touch nothing: so it stops where it would start to fall. Here the same test is
//! exact: on each axis the move is cut where the box still keeps [`EDGE_MARGIN`] of the last
//! thing under it (a block, a box of a block or a static or kinematic body), and the result
//! does not depend on the step rate.

use super::shape::LocalBox;
use super::source::{CollisionShape, CollisionTable, Probe, VoxelSource};
use super::sweep::{SweepConfig, Unloaded, clean_distance, effective_eps, usable_shape};
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, WorldPos};

/// How much of the support a guarded move keeps under the box, in blocks (`2^-10`): it stops
/// this far inside the edge, well over the contact tolerance, so the next step still stands on
/// it (and the sweep's footprint still finds it).
pub const EDGE_MARGIN: f64 = 1.0 / 1024.0;

/// Minecraft's sneak edge guard: the move `d` of the box `shape` (relative to the body's
/// position `start`) with its horizontal part cut so that the box keeps standing on something.
///
/// Support is what the box would overlap (by more than the contact tolerance) if it were lowered
/// by `drop`: blocks, boxes of shaped blocks (the tall boxes of the row under it included, ADR
/// 0026) and the `obstacles` (boxes in the frame of `start.block`, the static and kinematic
/// solid bodies). So a ledge up to `drop` deep (a slab with Minecraft's step height of 0.6) can
/// be walked down, a deeper one cannot.
///
/// - Nothing changes when the box has no support where it is (it stands on a dynamic body, or
///   it is in the air), when `drop` is not more than twice the contact tolerance (or NaN), or
///   when the move keeps support.
/// - Otherwise X and then Z, each alone: a move that would leave every support is cut to the
///   farthest point, not past `d` and not backward, where the box keeps [`EDGE_MARGIN`] of one
///   of them (or to 0 when none is ahead).
/// - Then both together: if the box would have no support there (a corner between two
///   supports), neither axis moves, as Minecraft's shortening does at walking speeds.
///
/// `d.y` is returned as it is. Components of `d` that are not finite count as 0 and each one is
/// clamped like a sweep.
///
/// # Errors
///
/// [`Unloaded`] when a block it needs is in an unloaded chunk.
#[expect(
    clippy::too_many_arguments,
    reason = "move_box's arguments and the drop"
)]
pub fn guard_edge<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    start: WorldPos,
    shape: LocalBox,
    d: DVec3,
    obstacles: &[LocalBox],
    cfg: &SweepConfig,
    drop: f64,
) -> Result<DVec3, Unloaded> {
    let d = clean_distance(d);
    let eps = effective_eps(cfg.eps);
    // `>` is false for NaN: no guard.
    let deep = drop > 2.0 * eps;
    if !deep || (d.x == 0.0 && d.z == 0.0) {
        return Ok(d);
    }
    let drop = drop.min(super::sweep::MAX_SWEEP_DISTANCE);
    let start = start.normalize();
    let shape = usable_shape(shape, eps);
    let lowered = shape.translate(start.frac - DVec3::new(0.0, drop, 0.0));
    let supports = gather(src, table, start.block, lowered, d, obstacles, eps)?;
    let fits = |s: &Rect, x: f64, z: f64| {
        lowered.min.x + x < s.max_x - eps
            && lowered.max.x + x > s.min_x + eps
            && lowered.min.z + z < s.max_z - eps
            && lowered.max.z + z > s.min_z + eps
    };
    let supported = |x: f64, z: f64| supports.iter().any(|s| fits(s, x, z));
    if !supported(0.0, 0.0) {
        return Ok(d);
    }
    let mut x = d.x;
    if x != 0.0 && !supported(x, 0.0) {
        x = cut(
            &supports,
            x,
            |s, t| fits(s, t, 0.0),
            |s| (s.min_x - lowered.max.x, s.max_x - lowered.min.x),
        );
    }
    let mut z = d.z;
    if z != 0.0 && !supported(0.0, z) {
        z = cut(
            &supports,
            z,
            |s, t| fits(s, 0.0, t),
            |s| (s.min_z - lowered.max.z, s.max_z - lowered.min.z),
        );
    }
    if x != 0.0 && z != 0.0 && !supported(x, z) {
        x = 0.0;
        z = 0.0;
    }
    Ok(DVec3::new(x, d.y, z))
}

/// A support under the box, on X and Z (in the frame of the body's block).
#[derive(Clone, Copy, Debug)]
struct Rect {
    min_x: f64,
    max_x: f64,
    min_z: f64,
    max_z: f64,
}

/// The farthest move toward `d` (not past it, not backward) on one axis that keeps
/// [`EDGE_MARGIN`] of a support: for each support, the move whose trailing side stops that far
/// inside its far edge (`span(s)` gives the moves at which the box starts and stops touching
/// it), kept if the box still overlaps it there (`fits`). 0 when none does.
fn cut(
    supports: &[Rect],
    d: f64,
    fits: impl Fn(&Rect, f64) -> bool,
    span: impl Fn(&Rect) -> (f64, f64),
) -> f64 {
    let mut best = 0.0;
    for s in supports {
        let (enter, leave) = span(s);
        let t = if d > 0.0 {
            d.min(leave - EDGE_MARGIN)
        } else {
            d.max(enter + EDGE_MARGIN)
        };
        let forward = if d > 0.0 { t > best } else { t < best };
        if forward && fits(s, t) {
            best = t;
        }
    }
    best
}

/// The supports of `lowered` moving by `d` across: every box (of a block, or an obstacle)
/// overlapping its height by more than `eps`, in the region it sweeps across.
fn gather<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    origin: BlockPos,
    lowered: LocalBox,
    d: DVec3,
    obstacles: &[LocalBox],
    eps: f64,
) -> Result<Vec<Rect>, Unloaded> {
    let region = LocalBox::new(
        lowered.min + DVec3::new(d.x.min(0.0), 0.0, d.z.min(0.0)),
        lowered.max + DVec3::new(d.x.max(0.0), 0.0, d.z.max(0.0)),
    );
    let mut out = Vec::new();
    let mut push = |w: LocalBox| {
        if w.min.y < lowered.max.y - eps && w.max.y > lowered.min.y + eps {
            out.push(Rect {
                min_x: w.min.x,
                max_x: w.max.x,
                min_z: w.min.z,
                max_z: w.max.z,
            });
        }
    };
    for &o in obstacles {
        push(o);
    }
    let span = |lo: f64, hi: f64| ((lo + eps).floor() as i64, (hi - eps).ceil() as i64 - 1);
    let (x0, x1) = span(region.min.x, region.max.x);
    let (y0, y1) = span(region.min.y, region.max.y);
    let (z0, z1) = span(region.min.z, region.max.z);
    // The row under the region holds tall blocks whose boxes may reach into it.
    let reach = table.reach();
    let low = if reach > 0.0 && y0 as f64 + reach > region.min.y + eps {
        y0 - 1
    } else {
        y0
    };
    for y in low..=y1 {
        for x in x0..=x1 {
            for z in z0..=z1 {
                let block = BlockPos::new(origin.x + x, origin.y + y, origin.z + z);
                let state = match src.probe(block) {
                    Probe::Block(state) => state,
                    Probe::Unloaded => return Err(Unloaded { block }),
                };
                if y < y0 && !table.is_tall(state) {
                    continue;
                }
                let at = DVec3::new(x as f64, y as f64, z as f64);
                match table.shape(state) {
                    CollisionShape::Full => push(LocalBox::new(at, at + DVec3::ONE)),
                    CollisionShape::Boxes(boxes) => {
                        for b in boxes {
                            push(b.translate(at));
                        }
                    }
                    CollisionShape::Empty => {}
                }
            }
        }
    }
    Ok(out)
}
