//! The geometry of the box-made block models (ADR 0018, ADR 0020): slabs, stairs, fences and
//! torches are unions of axis-aligned boxes in sixteenths of a block, the same boxes for
//! drawing ([`BlockModel::boxes`](crate::BlockModel::boxes)), picking and collision. Shapes
//! that turn with a `facing` are written for one facing and turned a quarter at a time around
//! the cell's vertical axis, as Minecraft turns its block models.
use crate::{CollisionBox, Face, SlabHalf};

/// Most distinct [`ModelBox`]es a registry may use, over all its states: the box index of a
/// mesher quad has 10 bits, and the renderer keeps the whole table in its view uniform.
pub const MAX_MODEL_BOXES: usize = 1 << 10;

/// A horizontal direction (Minecraft's `facing` of stairs and wall torches): north is -Z, south
/// +Z, west -X and east +X.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Facing {
    /// -Z.
    #[default]
    North,
    /// +Z.
    South,
    /// -X.
    West,
    /// +X.
    East,
}

impl Facing {
    /// The values of a `facing` property, in Minecraft's order.
    pub const ALL: [Facing; 4] = [Facing::North, Facing::South, Facing::West, Facing::East];

    /// `north`, `south`, `west` or `east`.
    pub const fn name(self) -> &'static str {
        match self {
            Facing::North => "north",
            Facing::South => "south",
            Facing::West => "west",
            Facing::East => "east",
        }
    }

    /// The facing of a property value.
    pub fn from_name(name: &str) -> Option<Facing> {
        Facing::ALL.into_iter().find(|f| f.name() == name)
    }

    /// The block face toward this direction.
    pub const fn face(self) -> Face {
        match self {
            Facing::North => Face::NegZ,
            Facing::South => Face::PosZ,
            Facing::West => Face::NegX,
            Facing::East => Face::PosX,
        }
    }

    /// The facing of a horizontal face (`None` for ±Y).
    pub const fn of_face(face: Face) -> Option<Facing> {
        match face {
            Face::NegZ => Some(Facing::North),
            Face::PosZ => Some(Facing::South),
            Face::NegX => Some(Facing::West),
            Face::PosX => Some(Facing::East),
            Face::PosY | Face::NegY => None,
        }
    }

    /// The opposite direction.
    pub const fn opposite(self) -> Facing {
        match self {
            Facing::North => Facing::South,
            Facing::South => Facing::North,
            Facing::West => Facing::East,
            Facing::East => Facing::West,
        }
    }

    /// A quarter turn clockwise seen from above (north to east).
    pub const fn clockwise(self) -> Facing {
        match self {
            Facing::North => Facing::East,
            Facing::East => Facing::South,
            Facing::South => Facing::West,
            Facing::West => Facing::North,
        }
    }

    /// A quarter turn counterclockwise seen from above (north to west).
    pub const fn counterclockwise(self) -> Facing {
        self.clockwise().opposite()
    }

    /// The horizontal direction a look vector points to most (`x`, `z`; ties go to the
    /// X axis): the facing a player gives the stairs they place.
    pub fn of_look(x: f64, z: f64) -> Facing {
        if x.abs() >= z.abs() {
            if x >= 0.0 { Facing::East } else { Facing::West }
        } else if z >= 0.0 {
            Facing::South
        } else {
            Facing::North
        }
    }

    /// Quarter turns clockwise from east (east 0, south 1, west 2, north 3).
    const fn turns_from_east(self) -> u8 {
        match self {
            Facing::East => 0,
            Facing::South => 1,
            Facing::West => 2,
            Facing::North => 3,
        }
    }
}

/// The `shape` of stairs (Minecraft's names): straight, or a corner with a neighbor stair. Left
/// and right are seen looking along `facing` (toward the high side).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StairShape {
    /// One step across the whole cell.
    #[default]
    Straight,
    /// An inner corner: the step plus a quarter on the left of the low side.
    InnerLeft,
    /// An inner corner: the step plus a quarter on the right of the low side.
    InnerRight,
    /// An outer corner: only the left quarter of the step.
    OuterLeft,
    /// An outer corner: only the right quarter of the step.
    OuterRight,
}

impl StairShape {
    /// The values of a `shape` property, in Minecraft's order.
    pub const ALL: [StairShape; 5] = [
        StairShape::Straight,
        StairShape::InnerLeft,
        StairShape::InnerRight,
        StairShape::OuterLeft,
        StairShape::OuterRight,
    ];

    /// `straight`, `inner_left`, ...
    pub const fn name(self) -> &'static str {
        match self {
            StairShape::Straight => "straight",
            StairShape::InnerLeft => "inner_left",
            StairShape::InnerRight => "inner_right",
            StairShape::OuterLeft => "outer_left",
            StairShape::OuterRight => "outer_right",
        }
    }

    /// The shape of a property value.
    pub fn from_name(name: &str) -> Option<StairShape> {
        StairShape::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// `bottom` or `top`: the value of a `half` (stairs) or `type` (slabs) property.
pub const fn half_name(half: SlabHalf) -> &'static str {
    match half {
        SlabHalf::Bottom => "bottom",
        SlabHalf::Top => "top",
    }
}

/// The half of a property value (`bottom` or `top`).
pub fn half_from_name(name: &str) -> Option<SlabHalf> {
    match name {
        "bottom" => Some(SlabHalf::Bottom),
        "top" => Some(SlabHalf::Top),
        _ => None,
    }
}

/// The connected sides of a fence: one bit per [`Facing`] (`1 << facing as u8`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FenceSides(pub u8);

impl FenceSides {
    /// No side connected (a lone post).
    pub const NONE: FenceSides = FenceSides(0);

    /// True if the side toward `facing` is connected.
    pub const fn has(self, facing: Facing) -> bool {
        self.0 & (1 << facing as u8) != 0
    }

    /// The same sides with `facing` connected or not.
    #[must_use]
    pub const fn with(self, facing: Facing, connected: bool) -> FenceSides {
        let bit = 1 << facing as u8;
        FenceSides(if connected {
            self.0 | bit
        } else {
            self.0 & !bit
        })
    }
}

/// One box of a block model: where it is in the cell, and where its texture comes from. A box
/// face shows the part of the block texture under it as if the texture were laid on the cell's
/// faces (Minecraft's `uvlock`), seen from the box moved by `texture_offset`: a wall torch
/// raised 3 sixteenths shows the same part of the torch texture as a standing torch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelBox {
    /// The box, in sixteenths.
    pub bounds: CollisionBox,
    /// Texture-space offset of the box, in sixteenths (each in `-32..32`).
    pub texture_offset: [i8; 3],
}

impl ModelBox {
    /// A box whose texture is the part of the cell faces under it.
    pub const fn new(min: [u8; 3], max: [u8; 3]) -> Self {
        Self {
            bounds: CollisionBox::new(min, max),
            texture_offset: [0; 3],
        }
    }

    /// Valid bounds ([`CollisionBox::is_valid`]) and offsets in `-32..32`.
    pub const fn is_valid(self) -> bool {
        let mut a = 0;
        while a < 3 {
            if self.texture_offset[a] < -32 || self.texture_offset[a] > 31 {
                return false;
            }
            a += 1;
        }
        self.bounds.is_valid()
    }

    /// The box turned `turns` quarter turns clockwise seen from above, around the cell's
    /// vertical center line (the texture offset turns with it).
    #[must_use]
    pub const fn turned(self, turns: u8) -> ModelBox {
        let mut b = self;
        let mut k = 0;
        while k < turns % 4 {
            // Clockwise from above: east (+X) goes to south (+Z), so (x, z) -> (16 - z, x).
            let (min, max) = (b.bounds.min, b.bounds.max);
            b.bounds =
                CollisionBox::new([16 - max[2], min[1], min[0]], [16 - min[2], max[1], max[0]]);
            let t = b.texture_offset;
            b.texture_offset = [-t[2], t[1], t[0]];
            k += 1;
        }
        b
    }

    /// The box mirrored upside down (`y -> 16 - y`), for top stairs.
    #[must_use]
    pub const fn flipped(self) -> ModelBox {
        let (min, max) = (self.bounds.min, self.bounds.max);
        ModelBox {
            bounds: CollisionBox::new([min[0], 16 - max[1], min[2]], [max[0], 16 - min[1], max[2]]),
            texture_offset: [
                self.texture_offset[0],
                -self.texture_offset[1],
                self.texture_offset[2],
            ],
        }
    }
}

/// The boxes of stairs (Minecraft's `stairs`, `inner_stairs` and `outer_stairs` models): the
/// slab of their `half` and the step on the `facing` side (one or two boxes for the corners).
/// Written for east and bottom; the others turn and flip them.
pub fn stair_boxes(facing: Facing, half: SlabHalf, shape: StairShape) -> Vec<ModelBox> {
    let step_east = ModelBox::new([8, 8, 0], [16, 16, 16]);
    let mut boxes = vec![ModelBox::new([0, 0, 0], [16, 8, 16])];
    match shape {
        StairShape::Straight => boxes.push(step_east),
        // Facing east, the right hand points south (+Z), the left north.
        StairShape::OuterRight => boxes.push(ModelBox::new([8, 8, 8], [16, 16, 16])),
        StairShape::OuterLeft => boxes.push(ModelBox::new([8, 8, 0], [16, 16, 8])),
        StairShape::InnerRight => {
            boxes.extend([step_east, ModelBox::new([0, 8, 8], [8, 16, 16])]);
        }
        StairShape::InnerLeft => {
            boxes.extend([step_east, ModelBox::new([0, 8, 0], [8, 16, 8])]);
        }
    }
    let turns = facing.turns_from_east();
    boxes
        .into_iter()
        .map(|b| {
            let b = b.turned(turns);
            if half == SlabHalf::Top {
                b.flipped()
            } else {
                b
            }
        })
        .collect()
}

/// The render boxes of a fence (Minecraft's `fence_post` and `fence_side`): a 4x16x4 post and,
/// toward each connected side, two 2x3 bars from the post to the cell border.
pub fn fence_boxes(sides: FenceSides) -> Vec<ModelBox> {
    let mut boxes = vec![ModelBox::new([6, 0, 6], [10, 16, 10])];
    for facing in Facing::ALL {
        if sides.has(facing) {
            // Written for north (-Z); turned from there.
            let turns = (facing.turns_from_east() + 1) % 4;
            boxes.extend(
                [
                    ModelBox::new([7, 12, 0], [9, 15, 6]),
                    ModelBox::new([7, 6, 0], [9, 9, 6]),
                ]
                .map(|b| b.turned(turns)),
            );
        }
    }
    boxes
}

/// The picking boxes of a fence (its outline): the post and a 4 wide arm toward each connected
/// side, all a block tall, as Minecraft's fence shape.
pub fn fence_outline(sides: FenceSides) -> Vec<CollisionBox> {
    fence_arms(sides, 16)
}

/// The collision boxes of a fence: [`fence_outline`] 1.5 blocks tall, reaching half a block
/// into the cell above (Minecraft's fence collision, ADR 0026), so a fence cannot be jumped.
pub fn fence_collision(sides: FenceSides) -> Vec<CollisionBox> {
    fence_arms(sides, crate::MAX_COLLISION_HEIGHT)
}

/// The post and the arms of a fence, `top` sixteenths tall.
fn fence_arms(sides: FenceSides, top: u8) -> Vec<CollisionBox> {
    let mut boxes = vec![CollisionBox::new([6, 0, 6], [10, top, 10])];
    for facing in Facing::ALL {
        if sides.has(facing) {
            // Written for north (-Z), turned from there (a quarter turn keeps `y`).
            let turns = (facing.turns_from_east() + 1) % 4;
            let mut arm = ModelBox::new([6, 0, 0], [10, 16, 6]).turned(turns).bounds;
            arm.max[1] = top;
            boxes.push(arm);
        }
    }
    boxes
}

/// The stick of a standing torch (Minecraft's `torch` model): 2x10x2, in the middle of the cell.
pub const TORCH_BOX: ModelBox = ModelBox::new([7, 0, 7], [9, 10, 9]);

/// The picking box of a standing torch.
pub const TORCH_OUTLINE: CollisionBox = CollisionBox::new([6, 0, 6], [10, 10, 10]);

/// The stick of a wall torch facing `facing` (away from the wall it hangs on). Minecraft tilts it
/// 22.5 degrees; here it stands upright against the wall, raised 3 sixteenths, and shows the
/// same part of the texture as a standing torch.
pub fn wall_torch_box(facing: Facing) -> ModelBox {
    // Written for east: the wall is at x = 0.
    ModelBox {
        bounds: CollisionBox::new([0, 3, 7], [2, 13, 9]),
        texture_offset: [7, -3, 0],
    }
    .turned(facing.turns_from_east())
}

/// The picking box of a wall torch facing `facing`.
pub fn wall_torch_outline(facing: Facing) -> CollisionBox {
    ModelBox::new([0, 3, 5], [5, 13, 11])
        .turned(facing.turns_from_east())
        .bounds
}
