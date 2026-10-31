//! Block surfaces (ADR 0047): how a block acts on the bodies on it and in it. Slippery ground,
//! ground that slows walkers or weakens jumps, cells that hold a body back, cells a character
//! climbs, sides a falling body slides down and blocks that only hold bodies from above. The
//! game registers a [`Surface`] per block ([`crate::BlockSurfaces`]); the [`CollisionTable`]
//! keeps them per state.
//!
//! Units are the engine's (blocks, seconds, rates per second), never per tick. ADR 0047 shows
//! how Minecraft's per-tick factors become these numbers.
//!
//! When each effect acts, in a body's step (the ECS layer, `PhysicsSystems::Step`):
//! - On the ground: the surface of the block it stood on at the end of the previous step
//!   (`Contacts::ground_block`) adds [`Surface::friction`] and [`Surface::drag`] to its ground
//!   friction, and a character jumping off it jumps [`Surface::jump`] times as fast.
//! - Inside: the cells its box overlapped at the end of the previous step ([`sense_inside`]) add
//!   their [`Surface::inside`] drag, a character whose feet were in a [`Surface::climb`] cell
//!   climbs, and a body falling against a [`Surface::slide`] side slides down.
//! - The sweep: a block with a [`Surface::hold`] collides only for bodies above it ([`Tread`]).
//! - A landing on a block with [`Surface::bounce`] bounces with that restitution.
//! - Pushed (ADR 0057): each cell with a [`Surface::push`] the box overlapped at the end of the
//!   previous step moves its vertical speed toward the push's limit (a bubble column).

use super::shape::LocalBox;
use super::source::{CollisionShape, CollisionTable, Probe, VoxelSource};
use super::sweep::footprint;
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, WorldPos};

/// How a block acts on bodies (ADR 0047). [`Surface::PLAIN`] (the default) does nothing: a block
/// without a registered surface is plain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Surface {
    /// Added to the ground friction of a body standing on the block, per second (negative:
    /// slippery). The sum is at least 0. A character controller makes up for it: it accelerates
    /// slower on a slippery block but reaches the same top speed (`game:ice` is about -9.81).
    pub friction: f64,
    /// Horizontal drag added while a body stands on the block, per second. A character
    /// controller does not make up for it, so walkers are slower (`game:soul_sand` is about
    /// 8.66: 58% of the walking speed).
    pub drag: f64,
    /// A character jumping off the block jumps this many times as fast (1: plain).
    pub jump: f64,
    /// The restitution of a body landing on the block (`None`: the body's own). A sneaking
    /// character never bounces.
    pub bounce: Option<f64>,
    /// Drag inside the block's cell, while the body's box overlaps it.
    pub inside: Option<Drag>,
    /// A character whose feet are in the cell climbs.
    pub climb: Option<Climb>,
    /// A body falling against a side of the block's collision, below its top, slides down at
    /// this speed, blocks per second.
    pub slide: Option<f64>,
    /// The block's collision holds only some bodies (`None`: every body, as registered).
    pub hold: Option<Hold>,
    /// A vertical push on bodies whose box overlaps the cell (ADR 0057: a bubble column).
    pub push: Option<Push>,
}

impl Surface {
    /// A plain block: no effect.
    pub const PLAIN: Self = Self {
        friction: 0.0,
        drag: 0.0,
        jump: 1.0,
        bounce: None,
        inside: None,
        climb: None,
        slide: None,
        hold: None,
        push: None,
    };

    /// The same surface with every value finite and in range: NaN and infinities count as the
    /// plain value, drags and speeds are at least 0, a restitution is in `0..=1`.
    pub fn sanitized(self) -> Self {
        let or = |v: f64, plain: f64| if v.is_finite() { v } else { plain };
        let rate = |v: f64| or(v, 0.0).max(0.0);
        Self {
            friction: or(self.friction, 0.0),
            drag: rate(self.drag),
            jump: rate(or(self.jump, 1.0)),
            bounce: self.bounce.map(|b| rate(b).min(1.0)),
            inside: self.inside.map(|d| Drag {
                across: rate(d.across),
                vertical: rate(d.vertical),
            }),
            climb: self.climb.map(|c| Climb {
                up: rate(c.up),
                // Infinity is a valid "no limit" here.
                max: if c.max.is_nan() { 0.0 } else { c.max.max(0.0) },
                ..c
            }),
            slide: self.slide.map(rate),
            hold: self.hold,
            push: self.push.map(|p| Push {
                inside: p.inside.sanitized(),
                top: p.top.sanitized(),
            }),
        }
    }

    /// It acts on bodies standing on the block (friction, drag, jump or bounce).
    pub fn acts_on_ground(&self) -> bool {
        self.friction != 0.0 || self.drag != 0.0 || self.jump != 1.0 || self.bounce.is_some()
    }

    /// It acts on bodies in the block's cell (inside drag, climbing, sliding or a push).
    pub fn acts_inside(&self) -> bool {
        self.inside.is_some() || self.climb.is_some() || self.slide.is_some() || self.push.is_some()
    }
}

impl Default for Surface {
    fn default() -> Self {
        Self::PLAIN
    }
}

/// Drag inside a block's cell, per second, added to a body's own while its box overlaps the
/// cell. Not made up for by a character controller: the steady speed across is
/// `speed * rate / (rate + across)` (with `rate` the body's ground friction or air drag) and the
/// fall speed `gravity / (drag_y + vertical)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    /// Across (X and Z).
    pub across: f64,
    /// Up and down.
    pub vertical: f64,
}

/// A character whose feet are in the cell climbs (a `game:ladder`): its speed across and its
/// fall speed are held to `max`, and it rises at `up` while it pushes against something across
/// (it clipped across in the previous step) or holds `MoveIntent::up`. Flying characters do not
/// climb.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Climb {
    /// Climbing speed, blocks per second (Minecraft's ladder: 2.352).
    pub up: f64,
    /// The most speed across and down while climbing, blocks per second (Minecraft's: 3).
    /// `f64::INFINITY`: no limit.
    pub max: f64,
    /// A sneaking character holds still instead of sliding down (`game:ladder`, not
    /// `game:scaffolding`).
    pub sneak_holds: bool,
    /// Only bodies with one of these [`crate::Treads`] bits climb (0: every character).
    pub treads: u8,
}

impl Default for Climb {
    /// Minecraft's ladder: up at 2.352 blocks/s (0.2 per tick less a tick of gravity and
    /// drag), at most 3 blocks/s across and down (0.15 per tick), sneaking holds.
    fn default() -> Self {
        Self {
            up: 2.352,
            max: 3.0,
            sneak_holds: true,
            treads: 0,
        }
    }
}

/// A vertical push on bodies whose box overlaps a block's cell (ADR 0057: Minecraft's bubble
/// column): each step, every such cell moves a body's vertical speed by its [`Lift`]. A cell
/// whose cell above has no collision, holds no fluid and does not push (the column's top, under
/// air) pushes with `top`, the others with `inside` (Minecraft's `onAboveBubbleColumn` and
/// `onInsideBubbleColumn`; there the cell above a column cell holds water). A flying character
/// is not pushed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Push {
    /// The push of a cell with something above it (a fluid, a block).
    pub inside: Lift,
    /// The push of a cell under an empty cell.
    pub top: Lift,
}

/// One cell's push on a body's vertical speed `v` in a step of `dt` seconds:
/// `min(limit, v + accel * dt)` for a push up (`accel` at least 0), `max(limit, v + accel * dt)`
/// for a push down, as Minecraft's `min(0.7, v + 0.06)` per tick: a body faster than the limit
/// is brought back to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lift {
    /// Vertical acceleration, blocks per second squared (negative: down).
    pub accel: f64,
    /// The vertical speed the push stops at, blocks per second (signed: up or down).
    pub limit: f64,
}

impl Lift {
    /// No push.
    pub const NONE: Self = Self {
        accel: 0.0,
        limit: 0.0,
    };

    /// The same lift with NaN and infinities as 0.
    pub fn sanitized(self) -> Self {
        let or0 = |v: f64| if v.is_finite() { v } else { 0.0 };
        Self {
            accel: or0(self.accel),
            limit: or0(self.limit),
        }
    }

    /// The vertical speed `v` after this lift acts for `dt` seconds.
    #[inline]
    pub fn apply(self, v: f64, dt: f64) -> f64 {
        if self.accel == 0.0 {
            return v;
        }
        let pushed = v + self.accel * dt;
        if self.accel > 0.0 {
            pushed.min(self.limit)
        } else {
            pushed.max(self.limit)
        }
    }
}

/// The most different lifts [`Inside::pushes`] keeps for one body (a body in two kinds of
/// column, or at a column's top, uses two); more are left out.
pub const MAX_PUSHES: usize = 4;

/// Which bodies a block's collision holds: only bodies above the block's collision at the start
/// of a move (their bottom at most [`HOLD_TOLERANCE`] under its top), that do not descend
/// (sneak), and, when `treads` is not 0, that have one of these [`crate::Treads`] bits. Others
/// go through it (`game:scaffolding` from the side, `game:powder_snow` without boots). Rays and
/// queries see the block's collision as registered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hold {
    /// The [`crate::Treads`] bits a body needs (0: none).
    pub treads: u8,
}

/// How far a body's bottom may be under the top of a [`Hold`] block's collision and still be
/// above it, blocks (Minecraft's `isAbove`, 1e-5).
pub const HOLD_TOLERANCE: f64 = 1e-5;

/// A body falling faster than this (blocks per second) against a [`Surface::slide`] side slides
/// (Minecraft: 0.08 per tick).
pub const SLIDE_FROM: f64 = 1.6;

/// A body that starts sliding faster than this (blocks per second) has its speed across cut to
/// [`SLIDE_ACROSS`] divided by its fall speed (Minecraft: 0.13 per tick).
pub const SLIDE_CUT_FROM: f64 = 2.6;

/// See [`SLIDE_CUT_FROM`], blocks per second (Minecraft: 0.05 per tick).
pub const SLIDE_ACROSS: f64 = 1.0;

/// The body a sweep moves, for the blocks with a [`Hold`]: whether it descends (a sneaking
/// character) and its [`crate::Treads`]. The default (not descending, no treads) is a body
/// without a controller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tread {
    /// It descends (sneaks): no [`Hold`] block holds it.
    pub descending: bool,
    /// Its [`crate::Treads`] bits.
    pub treads: u8,
}

impl Tread {
    /// Whether the collision `shape` of a [`Hold`] block whose cell starts at height `cell` (in
    /// the frame of the move) holds a body whose bottom was at `bottom` when the move started.
    #[inline]
    pub(crate) fn held(
        self,
        hold: Hold,
        shape: CollisionShape<'_>,
        cell: f64,
        bottom: f64,
    ) -> bool {
        if self.descending || (hold.treads != 0 && self.treads & hold.treads == 0) {
            return false;
        }
        let top = match shape {
            CollisionShape::Empty => return false,
            CollisionShape::Full => 1.0,
            CollisionShape::Boxes(boxes) => boxes.iter().fold(0.0f64, |t, b| t.max(b.max.y)),
        };
        bottom > cell + top - HOLD_TOLERANCE
    }
}

/// What the cells around a body do to it in its next step ([`sense_inside`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inside {
    /// The climb of the cell its feet are in.
    pub climb: Option<Climb>,
    /// The strongest drag of the cells its box overlaps (by `across`, then `vertical`).
    pub drag: Option<Drag>,
    /// The fastest slide of a side it is against (off the ground, below the block's top).
    pub slide: Option<f64>,
    /// The lifts of the push cells its box overlaps, each with how many cells push with it, in
    /// the order first met (cells by y, then x, then z, from the bottom): applied in that order,
    /// once per cell.
    pub pushes: [(Lift, u16); MAX_PUSHES],
}

impl Inside {
    /// True if a cell pushes the body.
    pub fn pushed(&self) -> bool {
        self.pushes[0].1 != 0
    }

    /// True if a cell pushes the body down.
    pub fn pulled(&self) -> bool {
        self.pushes
            .iter()
            .any(|&(lift, n)| n != 0 && lift.accel < 0.0)
    }

    /// The vertical speed `v` after every push cell acts for `dt` seconds.
    pub fn push(&self, mut v: f64, dt: f64) -> f64 {
        for &(lift, n) in &self.pushes {
            for _ in 0..n {
                v = lift.apply(v, dt);
            }
        }
        v
    }

    /// Counts one more cell pushing with `lift`.
    fn add_push(&mut self, lift: Lift) {
        for entry in &mut self.pushes {
            if entry.1 == 0 {
                *entry = (lift, 1);
                return;
            }
            if entry.0 == lift {
                entry.1 = entry.1.saturating_add(1);
                return;
            }
        }
    }
}

impl Default for Inside {
    fn default() -> Self {
        Self {
            climb: None,
            drag: None,
            slide: None,
            pushes: [(Lift::NONE, 0); MAX_PUSHES],
        }
    }
}

/// The surfaces in the cells the box `shape` at `pos` overlaps by more than `eps` (its strict
/// footprint, as the sweep counts cells): the strongest [`Surface::inside`] drag, the
/// [`Surface::climb`] of the cell holding `pos` (the feet of a standing collider) when `climber`
/// gives the body's [`crate::Treads`] (`None`: it does not climb), and the fastest
/// [`Surface::slide`] whose block's collision the box is beside (off the ground, its bottom
/// under the collision's top, and not overlapping it on X or on Z), and the [`Surface::push`]
/// lift of every cell it overlaps (the `top` one under a cell without collision, fluid or push,
/// which is looked at only for those cells; a cell above not loaded counts as full). Cells not loaded
/// count as plain. Exact far from the origin: every cell is in the frame of `pos.block`.
pub fn sense_inside<S: VoxelSource + ?Sized>(
    src: &mut S,
    table: &CollisionTable,
    pos: WorldPos,
    shape: LocalBox,
    eps: f64,
    on_ground: bool,
    climber: Option<u8>,
) -> Inside {
    let mut out = Inside::default();
    if !table.has_inside_surfaces() {
        return out;
    }
    let pos = pos.normalize();
    let bx = shape.translate(pos.frac);
    let (x0, x1) = footprint(bx.min.x, bx.max.x, eps);
    let (y0, y1) = footprint(bx.min.y, bx.max.y, eps);
    let (z0, z1) = footprint(bx.min.z, bx.max.z, eps);
    let o = pos.block;
    let state_at = |src: &mut S, x: i64, y: i64, z: i64| match src.probe(BlockPos::new(
        o.x + x,
        o.y + y,
        o.z + z,
    )) {
        Probe::Block(state) => Some(state),
        Probe::Unloaded => None,
    };
    if let Some(treads) = climber
        && let Some(state) = state_at(src, 0, 0, 0)
        && let Some(climb) = table.surface(state).climb
        && (climb.treads == 0 || climb.treads & treads != 0)
    {
        out.climb = Some(climb);
    }
    for y in y0..=y1 {
        for x in x0..=x1 {
            for z in z0..=z1 {
                let Some(state) = state_at(src, x, y, z) else {
                    continue;
                };
                if !table.acts_inside(state) {
                    continue;
                }
                let surface = table.surface(state);
                if let Some(drag) = surface.inside
                    && out
                        .drag
                        .is_none_or(|d| (drag.across, drag.vertical) > (d.across, d.vertical))
                {
                    out.drag = Some(drag);
                }
                if let Some(speed) = surface.slide
                    && !on_ground
                    && out.slide.is_none_or(|s| speed > s)
                    && beside(table.shape(state), [x, y, z], bx, eps)
                {
                    out.slide = Some(speed);
                }
                if let Some(push) = surface.push {
                    let open = state_at(src, x, y + 1, z).is_some_and(|above| {
                        matches!(table.shape(above), CollisionShape::Empty)
                            && !table.is_wet(above)
                            && table.surface(above).push.is_none()
                    });
                    out.add_push(if open { push.top } else { push.inside });
                }
            }
        }
    }
    out
}

/// The box `bx` is beside the collision `shape` of the cell at `cell` (in the same frame): its
/// bottom is under the shape's top by more than `eps`, and it does not overlap the shape's bounds
/// on X or on Z by more than `eps` (Minecraft's honey block: the body is against a side, not on
/// top).
fn beside(shape: CollisionShape<'_>, cell: [i64; 3], bx: LocalBox, eps: f64) -> bool {
    let bounds = match shape {
        CollisionShape::Empty => return false,
        CollisionShape::Full => LocalBox::new(DVec3::ZERO, DVec3::ONE),
        CollisionShape::Boxes(boxes) => boxes
            .iter()
            .copied()
            .reduce(|a, b| LocalBox::new(a.min.min(b.min), a.max.max(b.max)))
            .unwrap_or_default(),
    };
    let w = bounds.translate(DVec3::new(cell[0] as f64, cell[1] as f64, cell[2] as f64));
    let apart = |a: usize| !(w.min[a] < bx.max[a] - eps && w.max[a] > bx.min[a] + eps);
    bx.min.y < w.max.y - eps && (apart(0) || apart(2))
}
