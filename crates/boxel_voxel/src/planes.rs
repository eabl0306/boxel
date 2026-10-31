//! Planes inside a box (EW-PLANE, ADR 0046): a single quad of a block model made of boxes, on
//! a mid-plane or a diagonal of a box of its own, such as the two crossed planes of a chain, a
//! plant in a flower pot or a candle's wick, a coral fan's tilted leaves or a fire's sides.
//! Minecraft draws these as elements turned 22.5 or 45 degrees; here the box gives the plane
//! its place and its tilt (any slope a diagonal of whole thirty-seconds can make), so the
//! geometry stays exact and shared in the model box table.
//!
//! A plane takes one entry of the model box table ([`PlaneEntry`]: its box and its texture
//! rectangle); which plane of the box it is ([`PlaneKind`]) and its spin ride in its quad, so
//! the two planes of a cross share one entry.
use crate::registry::{Axis, TextureId};

/// Thirty-seconds of a block: the unit of a plane's box ([`ModelPlane::min`] and `max`), twice
/// as fine as the sixteenths of [`crate::ModelBox`], for Minecraft's planes half a texel off
/// the grid (a candle's wick from 7.5 to 8.5).
pub const PLANE_UNIT: u8 = 32;

/// Which plane of its box a [`ModelPlane`] is. Each has its own axes: `u` runs along the
/// texture's rows and `v` up (along +Z for [`PlaneKind::Mid`] of Y, as on a box's top), and
/// the texture's top left corner is at `u = 0` and the top (`v` = 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlaneKind {
    /// Across the axis, halfway between the box's faces on it (the box may be flat there).
    /// `u` and `v` are those of the box's faces on that axis ([`crate::Face::axes`]: along +Z
    /// and up on X, along +X and up on Z, along +X and +Z on Y).
    Mid(Axis),
    /// Along the axis, through the box's diagonal where its two other coordinates (in x, y, z
    /// order) grow together: Y's runs from the box's low x and z to its high x and z (`u`, `v`
    /// up), as the `PosX` cross quad does in the full cell. On X and Z, `u` runs along the axis
    /// and `v` up the diagonal: a tilted plane.
    Rising(Axis),
    /// Along the axis, through the other diagonal: one of the two other coordinates falls as
    /// the other grows. Y's runs from the box's high x and low z to its low x and high z, as
    /// the `PosZ` cross quad does in the full cell.
    Falling(Axis),
}

impl PlaneKind {
    /// Every kind, in [`PlaneKind::code`] order.
    pub const ALL: [PlaneKind; 9] = [
        PlaneKind::Mid(Axis::X),
        PlaneKind::Rising(Axis::X),
        PlaneKind::Falling(Axis::X),
        PlaneKind::Mid(Axis::Y),
        PlaneKind::Rising(Axis::Y),
        PlaneKind::Falling(Axis::Y),
        PlaneKind::Mid(Axis::Z),
        PlaneKind::Rising(Axis::Z),
        PlaneKind::Falling(Axis::Z),
    ];

    /// The kind's code in a plane quad (`0..9`): three per axis (x, y, z), mid, rising and
    /// falling.
    pub const fn code(self) -> u8 {
        let (axis, variant) = match self {
            PlaneKind::Mid(a) => (a, 0),
            PlaneKind::Rising(a) => (a, 1),
            PlaneKind::Falling(a) => (a, 2),
        };
        axis_index(axis) as u8 * 3 + variant
    }

    /// The kind of a code (`None` past 8).
    pub const fn from_code(code: u8) -> Option<PlaneKind> {
        if code >= 9 {
            return None;
        }
        Some(PlaneKind::ALL[code as usize])
    }

    /// The axis the plane is across ([`PlaneKind::Mid`]) or along (the diagonals).
    pub const fn axis(self) -> Axis {
        match self {
            PlaneKind::Mid(a) | PlaneKind::Rising(a) | PlaneKind::Falling(a) => a,
        }
    }
}

/// `0`, `1` or `2` for x, y or z.
const fn axis_index(axis: Axis) -> usize {
    match axis {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    }
}

/// What a plane keeps in the model box table: its box (thirty-seconds, `0..=32`) and its
/// texture rectangle with its quarter turns. Planes that agree on all of it share an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlaneEntry {
    /// The box's low corner, in thirty-seconds.
    pub min: [u8; 3],
    /// The box's high corner, in thirty-seconds.
    pub max: [u8; 3],
    /// [`ModelPlane::uv`].
    pub uv: [u8; 4],
    /// [`ModelPlane::turn`].
    pub turn: u8,
}

/// One quad of a block model, on a plane of a box of its own ([`PlaneKind`]), drawn from both
/// sides like a cross plant: no face shading, no ambient occlusion, lit by its block's cell
/// ([`crate::BoxModel::planes`]).
///
/// Its texture is a rectangle of the layer ([`ModelPlane::uv`], Minecraft's face `uv`) laid
/// over the plane, so a texture can be cropped, stretched or mirrored onto it, and turned a
/// quarter at a time ([`ModelPlane::turn`], Minecraft's face `rotation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelPlane {
    /// Which plane of the box.
    pub kind: PlaneKind,
    /// The box's low corner, in thirty-seconds ([`PLANE_UNIT`]).
    pub min: [u8; 3],
    /// The box's high corner, in thirty-seconds (each `0..=32` and at least `min`; above it on
    /// both axes the plane spans).
    pub max: [u8; 3],
    /// The texture's rectangle, in texels of a 16 wide texture (`[u0, v0, u1, v1]`, each
    /// `0..=16`): the plane's top left corner shows `(u0, v0)`, its bottom right `(u1, v1)`.
    /// Backward to mirror.
    pub uv: [u8; 4],
    /// Quarter turns of the texture on the plane, clockwise (`0..4`, Minecraft's `rotation` of
    /// a face divided by 90).
    pub turn: u8,
    /// Sixteenths of a turn the quad turns about its center, in its plane, from its `u` toward
    /// its `v` (`0..16`). The box stays where it was: a turned quad may reach past it, but must
    /// stay in the cell. For an item in a frame (45 degrees a step); no block uses it yet.
    pub spin: u8,
    /// The layer it shows; `None`: the block's side (its `PosX` texture, as a cross plant).
    pub texture: Option<TextureId>,
}

/// One corner of a [`ModelPlane`] ([`ModelPlane::corners`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaneCorner {
    /// Where it is in the cell, in blocks (`0..=1` on each axis).
    pub position: [f32; 3],
    /// The texel it shows (`0..=16` each).
    pub texel: [f32; 2],
}

/// `cos(k * 22.5 degrees)` for `k` in `0..4`, as `common.wgsl` spins a plane (`PLANE_COS1` to
/// `PLANE_COS3`).
pub const SPIN_COS: [f32; 4] = [
    1.0,
    0.923_879_5,
    std::f32::consts::FRAC_1_SQRT_2,
    0.382_683_43,
];

/// `(cos, sin)` of `spin` sixteenths of a turn: exact at the quarter turns.
fn spin_cos_sin(spin: u8) -> (f32, f32) {
    let r = usize::from(spin & 3);
    let (c, s) = (SPIN_COS[r], if r == 0 { 0.0 } else { SPIN_COS[4 - r] });
    match (spin >> 2) & 3 {
        0 => (c, s),
        1 => (-s, c),
        2 => (-c, -s),
        _ => (s, -c),
    }
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

impl ModelPlane {
    /// The plane `kind` of the box from `min` to `max` (thirty-seconds), showing the whole
    /// texture of its block's side.
    pub const fn new(kind: PlaneKind, min: [u8; 3], max: [u8; 3]) -> Self {
        Self {
            kind,
            min,
            max,
            uv: [0, 0, 16, 16],
            turn: 0,
            spin: 0,
            texture: None,
        }
    }

    /// The plane `kind` of the box from `min` to `max` in sixteenths, as [`crate::ModelBox`]
    /// takes them.
    pub const fn sixteenths(kind: PlaneKind, min: [u8; 3], max: [u8; 3]) -> Self {
        Self::new(
            kind,
            [min[0] * 2, min[1] * 2, min[2] * 2],
            [max[0] * 2, max[1] * 2, max[2] * 2],
        )
    }

    /// The two diagonal planes along Y of the box from `min` to `max` (thirty-seconds): a cross
    /// plant in that box (the full cell's is the cross of [`crate::BlockModel::Cross`]).
    pub const fn cross(min: [u8; 3], max: [u8; 3]) -> [ModelPlane; 2] {
        [
            Self::new(PlaneKind::Rising(Axis::Y), min, max),
            Self::new(PlaneKind::Falling(Axis::Y), min, max),
        ]
    }

    /// The same plane showing `texture`.
    #[must_use]
    pub const fn with_texture(mut self, texture: TextureId) -> Self {
        self.texture = Some(texture);
        self
    }

    /// The same plane showing the rectangle `uv` of its texture ([`ModelPlane::uv`]).
    #[must_use]
    pub const fn with_uv(mut self, uv: [u8; 4]) -> Self {
        self.uv = uv;
        self
    }

    /// The same plane with its texture turned `turn` quarters ([`ModelPlane::turn`]).
    #[must_use]
    pub const fn with_turn(mut self, turn: u8) -> Self {
        self.turn = turn % 4;
        self
    }

    /// The same plane turned `spin` sixteenths of a turn about its center ([`ModelPlane::spin`]).
    #[must_use]
    pub const fn spun(mut self, spin: u8) -> Self {
        self.spin = spin % 16;
        self
    }

    /// Its entry in the model box table.
    pub const fn entry(self) -> PlaneEntry {
        PlaneEntry {
            min: self.min,
            max: self.max,
            uv: self.uv,
            turn: self.turn,
        }
    }

    /// The plane byte of its quad: the kind's code in the low 4 bits, the spin in the high 4.
    pub const fn quad_byte(self) -> u8 {
        self.kind.code() | self.spin << 4
    }

    /// A valid plane: its box inside the cell, at least as high as low on every axis and
    /// higher on both axes it spans (all three for a diagonal), its texture rectangle inside
    /// the texture, a turn under 4 and a spin under 16.
    pub fn is_valid(&self) -> bool {
        let inside = (0..3).all(|a| self.min[a] <= self.max[a] && self.max[a] <= PLANE_UNIT);
        let spans = (0..3).all(|a| {
            let across = matches!(self.kind, PlaneKind::Mid(axis) if axis_index(axis) == a);
            across || self.min[a] < self.max[a]
        });
        inside && spans && self.uv.iter().all(|&t| t <= 16) && self.turn < 4 && self.spin < 16
    }

    /// The plane's corner, `u` and `v` vectors (blocks), as `common.wgsl` builds them.
    fn frame(&self) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let lo = self.min.map(|c| f32::from(c) / f32::from(PLANE_UNIT));
        let hi = self.max.map(|c| f32::from(c) / f32::from(PLANE_UNIT));
        let d = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        let mid = |a: usize| (lo[a] + hi[a]) * 0.5;
        match self.kind {
            PlaneKind::Mid(Axis::X) => ([mid(0), lo[1], lo[2]], [0.0, 0.0, d[2]], [0.0, d[1], 0.0]),
            PlaneKind::Mid(Axis::Y) => ([lo[0], mid(1), lo[2]], [d[0], 0.0, 0.0], [0.0, 0.0, d[2]]),
            PlaneKind::Mid(Axis::Z) => ([lo[0], lo[1], mid(2)], [d[0], 0.0, 0.0], [0.0, d[1], 0.0]),
            PlaneKind::Rising(Axis::X) => (lo, [d[0], 0.0, 0.0], [0.0, d[1], d[2]]),
            PlaneKind::Falling(Axis::X) => {
                ([lo[0], lo[1], hi[2]], [d[0], 0.0, 0.0], [0.0, d[1], -d[2]])
            }
            PlaneKind::Rising(Axis::Y) => (lo, [d[0], 0.0, d[2]], [0.0, d[1], 0.0]),
            PlaneKind::Falling(Axis::Y) => {
                ([hi[0], lo[1], lo[2]], [-d[0], 0.0, d[2]], [0.0, d[1], 0.0])
            }
            PlaneKind::Rising(Axis::Z) => (lo, [0.0, 0.0, d[2]], [d[0], d[1], 0.0]),
            PlaneKind::Falling(Axis::Z) => {
                ([hi[0], lo[1], lo[2]], [0.0, 0.0, d[2]], [-d[0], d[1], 0.0])
            }
        }
    }

    /// Its 4 corners, corner `k` at `(u, v) = (k & 1, k >> 1)` of its plane, where they are
    /// and what texel each shows: the CPU twin of `common.wgsl`'s plane quads (the GPU may round
    /// a spun corner a little differently).
    pub fn corners(&self) -> [PlaneCorner; 4] {
        let (a, u, v) = self.frame();
        let (c, s) = spin_cos_sin(self.spin);
        let center = add(a, add(scale(u, 0.5), scale(v, 0.5)));
        let n = cross(u, v);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let n = scale(n, 1.0 / len);
        std::array::from_fn(|k| {
            let (cu, cv) = ((k & 1) as f32, (k >> 1) as f32);
            let position = if self.spin == 0 {
                add(add(a, scale(u, cu)), scale(v, cv))
            } else {
                let off = add(scale(u, cu - 0.5), scale(v, cv - 0.5));
                add(center, add(scale(off, c), scale(cross(n, off), s)))
            };
            // Down the texture from the top, but on Mid(Y), whose v runs along +Z as a top's.
            let down = if self.kind == PlaneKind::Mid(Axis::Y) {
                cv
            } else {
                1.0 - cv
            };
            let (mut p, mut q) = (cu, down);
            for _ in 0..self.turn % 4 {
                (p, q) = (q, 1.0 - p);
            }
            let [u0, v0, u1, v1] = self.uv.map(f32::from);
            PlaneCorner {
                position,
                texel: [u0 + (u1 - u0) * p, v0 + (v1 - v0) * q],
            }
        })
    }

    /// The plane turned `turns` quarter turns clockwise seen from above, around the cell's
    /// vertical center line, as [`crate::ModelBox::turned`] turns a box: the same quad, its
    /// texture as before on it.
    #[must_use]
    pub fn turned(self, turns: u8) -> ModelPlane {
        self.map(turns, |[x, y, z]| [1.0 - z, y, x])
    }

    /// The plane turned around X, each quarter taking the top to the north, as
    /// [`crate::ModelBox::turned_x`] turns a box.
    #[must_use]
    pub fn turned_x(self, turns: u8) -> ModelPlane {
        self.map(turns, |[x, y, z]| [x, z, 1.0 - y])
    }

    /// The plane turned around Z, each quarter taking the top to the east, as
    /// [`crate::ModelBox::turned_z`] turns a box.
    #[must_use]
    pub fn turned_z(self, turns: u8) -> ModelPlane {
        self.map(turns, |[x, y, z]| [y, 1.0 - x, z])
    }

    /// The plane `turns` times through the quarter turn `turn` (a point in blocks to the
    /// turned point): the turned box, then the kind, spin, mirror and texture turn whose
    /// corners are the turned corners, showing the same texels.
    fn map(self, turns: u8, turn: impl Fn([f32; 3]) -> [f32; 3]) -> ModelPlane {
        let mut plane = self;
        for _ in 0..turns % 4 {
            plane = plane.turn_once(&turn);
        }
        plane
    }

    fn turn_once(self, turn: &impl Fn([f32; 3]) -> [f32; 3]) -> ModelPlane {
        let unit = f32::from(PLANE_UNIT);
        let ends = [self.min, self.max].map(|p| turn(p.map(|c| f32::from(c) / unit)));
        let mut min = [0; 3];
        let mut max = [0; 3];
        for a in 0..3 {
            // Exact: the turned coordinates are thirty-seconds again.
            let (e0, e1) = ((ends[0][a] * unit).round(), (ends[1][a] * unit).round());
            min[a] = e0.min(e1) as u8;
            max[a] = e0.max(e1) as u8;
        }
        let target = self.corners().map(|c| (turn(c.position), c.texel));
        let [u0, v0, u1, v1] = self.uv;
        let spins = [self.spin, (16 - self.spin) % 16];
        for kind in PlaneKind::ALL {
            for spin in spins {
                for t in 0..4 {
                    for uv in [
                        [u0, v0, u1, v1],
                        [u1, v0, u0, v1],
                        [u0, v1, u1, v0],
                        [u1, v1, u0, v0],
                    ] {
                        let candidate = ModelPlane {
                            kind,
                            min,
                            max,
                            uv,
                            turn: t,
                            spin,
                            texture: self.texture,
                        };
                        if candidate.is_valid() && same_corners(&candidate.corners(), &target) {
                            return candidate;
                        }
                    }
                }
            }
        }
        unreachable!("a turned plane is a plane of its turned box: {self:?}")
    }
}

/// True if `corners` are `target` (positions and texels), in any order.
fn same_corners(corners: &[PlaneCorner; 4], target: &[([f32; 3], [f32; 2]); 4]) -> bool {
    let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
    target.iter().all(|(p, t)| {
        corners.iter().any(|c| {
            (0..3).all(|a| near(c.position[a], p[a])) && (0..2).all(|a| near(c.texel[a], t[a]))
        })
    })
}

/// One entry of the model box table: a box, whose faces `SHAPE_BOX` quads draw (ADR 0020), or
/// a plane's box and texture rectangle (ADR 0046).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TableEntry {
    /// A model box.
    Box(crate::ModelBox),
    /// A plane's entry.
    Plane(PlaneEntry),
}

impl TableEntry {
    /// The box of a box entry.
    pub const fn as_box(self) -> Option<crate::ModelBox> {
        match self {
            TableEntry::Box(b) => Some(b),
            TableEntry::Plane(_) => None,
        }
    }

    /// The entry's box in sixteenths, rounded out for a plane (what holds the quads it draws).
    pub fn bounds(self) -> crate::CollisionBox {
        match self {
            TableEntry::Box(b) => b.bounds,
            TableEntry::Plane(p) => {
                let lo = p.min.map(|c| c / 2);
                let hi = p.max.map(|c| c.div_ceil(2));
                crate::CollisionBox::new(lo, hi)
            }
        }
    }
}
