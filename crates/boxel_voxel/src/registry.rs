//! Data-driven block registry. A block has state properties (ADR 0019, [`Property`]); every
//! combination of their values is one `BlockState(u16)`, the dense runtime id (`0` = air),
//! assigned in registration order and contiguous per block. Every per-state datum (model,
//! collision, textures, light) lives in flat arrays indexed by the state id. Frozen before the
//! world loads and shared as `Arc<BlockRegistry>`.
use crate::shapes::{self, Facing, FenceSides, MAX_MODEL_BOXES, ModelBox, StairShape};
use crate::state::{Property, StateName, StateParseError, StateValues, split_state};
use crate::{Face, LightValue, OPAQUE_OPACITY};

/// The state property of a fluid's level (Minecraft's `level`, `0..=15`): 0 is a source, 1 to 7
/// flowing (lower as it grows), 8 to 15 falling (ADR 0021).
pub const FLUID_LEVEL: &str = "level";

/// The state property of a block that holds a water source (Minecraft's `waterlogged`, `false`
/// or `true`; ADR 0025): the states with `true` have [`StateFlags::WATERLOGGED`].
pub const WATERLOGGED: &str = "waterlogged";

/// Most states a registry holds: `2^15`. The mesher's material id is the state id, and its top
/// bit (`boxel_mesh::FLUID_SURFACE_BIT`) marks fluid surface cells, so every state id must stay
/// below it.
pub const MAX_STATES: usize = 1 << 15;

/// Dense runtime block state id. `BlockState::AIR == BlockState(0)`. Never persisted raw
/// (persistence will store names through an id map, M1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct BlockState(pub u16);

impl BlockState {
    /// Air.
    pub const AIR: BlockState = BlockState(0);
    /// True for air.
    pub const fn is_air(self) -> bool {
        self.0 == 0
    }
}

/// How a block is drawn: its render layer (the geometry is its [`BlockModel`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RenderShape {
    /// No geometry (air, invisible blocks).
    #[default]
    None,
    /// Opaque, no alpha: drawn in the opaque face buckets. A full cube (`BlockModel::Cube`) hides
    /// the neighbor faces it touches ([`StateFlags::OCCLUDES`]); other models (lava, a
    /// `BlockModel::Fluid`, too) hide nothing.
    Opaque,
    /// Alpha-tested (leaves, plants): the cutout bucket, drawn after the opaque one with discard.
    /// Does not hide neighbors; faces between two identical cutout cubes are culled.
    Cutout,
    /// Blended (water, glass): the translucent bucket, drawn last, back to front per chunk, with
    /// depth test and no depth write. Does not hide neighbors; faces between two identical
    /// translucent cubes are culled.
    Translucent,
}

/// A world axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Axis {
    /// X.
    X,
    /// Y (up).
    #[default]
    Y,
    /// Z.
    Z,
}

impl Axis {
    /// The axes in order.
    pub const ALL: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

    /// The axis a face is across (`PosX` and `NegX` -> `X`).
    pub const fn of_face(face: Face) -> Axis {
        match face {
            Face::PosX | Face::NegX => Axis::X,
            Face::PosY | Face::NegY => Axis::Y,
            Face::PosZ | Face::NegZ => Axis::Z,
        }
    }

    /// `x`, `y` or `z`: the value of an `axis` state property.
    pub const fn name(self) -> &'static str {
        match self {
            Axis::X => "x",
            Axis::Y => "y",
            Axis::Z => "z",
        }
    }

    /// The axis named `x`, `y` or `z`.
    pub fn from_name(name: &str) -> Option<Axis> {
        Axis::ALL.into_iter().find(|a| a.name() == name)
    }
}

/// Which half of its cell a slab fills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SlabHalf {
    /// The lower half (`y` in `0..0.5`).
    #[default]
    Bottom,
    /// The upper half (`y` in `0.5..1`).
    Top,
}

/// The geometry of a block (ADR 0018). Cubes go through the greedy mesher; the other models are
/// meshed one block at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockModel {
    /// The full cube. `axis` orients it (logs): [`FaceTextures::top`] and
    /// [`FaceTextures::bottom`] go on the faces across `axis` (+ and -), the side texture on the
    /// other four, turned so that its vertical runs along `axis`.
    Cube {
        /// Orientation; `Axis::Y` is the unturned cube.
        axis: Axis,
    },
    /// Half a cube (slabs): the bottom or top half of the cell. Its face against the cell
    /// border (bottom of a bottom slab) is full; its sides are half faces.
    Slab(SlabHalf),
    /// Two diagonal quads through the cell (plants), with the side texture, drawn from both
    /// sides. Never hides or is hidden by anything.
    Cross,
    /// A fluid (water, lava): a cube for meshing and culling, but where the block above is
    /// neither the same fluid nor an opaque block (the surface) its top sits 2/16 lower, as in
    /// Minecraft. Lets everything through ([`BlockModel::default_collision`] is `None`). Hides
    /// no neighbor face, even when drawn `Opaque` (lava).
    Fluid,
    /// Stairs (ADR 0020): the slab of `half` and a step on the `facing` side, shaped by `shape`
    /// ([`crate::shapes::stair_boxes`]). Collide and are picked by their boxes.
    Stairs {
        /// Toward the high side.
        facing: Facing,
        /// Which half the slab part fills.
        half: SlabHalf,
        /// Straight or a corner.
        shape: StairShape,
    },
    /// A fence (ADR 0020): a post and bars toward the connected `sides`
    /// ([`crate::shapes::fence_boxes`]).
    Fence {
        /// Connected sides.
        sides: FenceSides,
    },
    /// A standing torch: a 2x10x2 stick ([`crate::shapes::TORCH_BOX`]). No collision.
    Torch,
    /// A torch on a wall, pointing toward `facing` (away from the wall). No collision.
    WallTorch {
        /// Away from the wall.
        facing: Facing,
    },
}

impl Default for BlockModel {
    fn default() -> Self {
        Self::CUBE
    }
}

impl BlockModel {
    /// The unturned full cube.
    pub const CUBE: BlockModel = BlockModel::Cube { axis: Axis::Y };

    /// True for the full cube (any axis) and for fluids, which mesh and cull as cubes.
    pub const fn is_cube(self) -> bool {
        matches!(self, BlockModel::Cube { .. } | BlockModel::Fluid)
    }

    /// True for the full cube (any axis) but not a fluid: what an `Opaque` block must be to
    /// hide its neighbors' faces (an opaque fluid, lava, hides nothing: its top sits lower).
    pub const fn is_solid_cube(self) -> bool {
        matches!(self, BlockModel::Cube { .. })
    }

    /// The face of the unturned cube whose texture `face` shows: for a cube turned to X or Z,
    /// the faces across the axis show the top and bottom textures and the others the side one.
    pub const fn texture_face(self, face: Face) -> Face {
        match (self, face) {
            (BlockModel::Cube { axis: Axis::X }, Face::PosX)
            | (BlockModel::Cube { axis: Axis::Z }, Face::PosZ) => Face::PosY,
            (BlockModel::Cube { axis: Axis::X }, Face::NegX)
            | (BlockModel::Cube { axis: Axis::Z }, Face::NegZ) => Face::NegY,
            (BlockModel::Cube { axis: Axis::X }, _) | (BlockModel::Cube { axis: Axis::Z }, _) => {
                Face::PosX
            }
            _ => face,
        }
    }

    /// True if the texture of `face` is turned a quarter (transposed: the texture's vertical
    /// runs along the face's u axis, [`Face::axes`]), as on the side faces of a log lying along
    /// X or Z, so the grain follows the axis.
    pub const fn face_turned(self, face: Face) -> bool {
        match self {
            BlockModel::Cube { axis: Axis::X } => !matches!(face, Face::PosX | Face::NegX),
            BlockModel::Cube { axis: Axis::Z } => matches!(face, Face::PosX | Face::NegX),
            _ => false,
        }
    }

    /// The box the block is picked by (its outline): the full cell for cubes, the half cell for
    /// slabs, a smaller box around the stem for crosses, the boxes of stairs, the post and arms
    /// of a fence, a box around a torch's stick.
    pub fn outline(self) -> BlockCollision {
        match self {
            BlockModel::Cube { .. } | BlockModel::Fluid => BlockCollision::Full,
            BlockModel::Slab(half) => BlockCollision::Boxes(vec![CollisionBox::slab(half)]),
            BlockModel::Cross => {
                BlockCollision::Boxes(vec![CollisionBox::new([2, 0, 2], [14, 13, 14])])
            }
            BlockModel::Stairs { .. } => {
                BlockCollision::Boxes(self.boxes().iter().map(|b| b.bounds).collect())
            }
            BlockModel::Fence { sides } => BlockCollision::Boxes(shapes::fence_collision(sides)),
            BlockModel::Torch => BlockCollision::Boxes(vec![shapes::TORCH_OUTLINE]),
            BlockModel::WallTorch { facing } => {
                BlockCollision::Boxes(vec![shapes::wall_torch_outline(facing)])
            }
        }
    }

    /// The usual collision of the model: full cubes, slabs, stairs and fences collide with their
    /// shape (a fence a block tall, ADR 0020), crosses, fluids and torches do not collide.
    pub fn default_collision(self) -> BlockCollision {
        match self {
            BlockModel::Cross
            | BlockModel::Fluid
            | BlockModel::Torch
            | BlockModel::WallTorch { .. } => BlockCollision::None,
            other => other.outline(),
        }
    }

    /// The boxes the model is drawn with, for the models made of boxes (slabs, stairs, fences
    /// and torches; ADR 0020). Empty for cubes, crosses and fluids, which the mesher draws its
    /// own way.
    pub fn boxes(self) -> Vec<ModelBox> {
        match self {
            BlockModel::Cube { .. } | BlockModel::Fluid | BlockModel::Cross => Vec::new(),
            BlockModel::Slab(half) => vec![ModelBox {
                bounds: CollisionBox::slab(half),
                texture_offset: [0; 3],
            }],
            BlockModel::Stairs {
                facing,
                half,
                shape,
            } => shapes::stair_boxes(facing, half, shape),
            BlockModel::Fence { sides } => shapes::fence_boxes(sides),
            BlockModel::Torch => vec![shapes::TORCH_BOX],
            BlockModel::WallTorch { facing } => vec![shapes::wall_torch_box(facing)],
        }
    }
}

/// An axis-aligned box inside a block cell, in sixteenths of a block (`0..=16`, `min < max` on
/// every axis), so that block shapes are exact in binary floating point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CollisionBox {
    /// Min corner (sixteenths).
    pub min: [u8; 3],
    /// Max corner (sixteenths).
    pub max: [u8; 3],
}

impl CollisionBox {
    /// The whole cell.
    pub const FULL: CollisionBox = CollisionBox::new([0; 3], [16; 3]);

    /// A box from its corners in sixteenths.
    pub const fn new(min: [u8; 3], max: [u8; 3]) -> Self {
        Self { min, max }
    }

    /// The box of a slab.
    pub const fn slab(half: SlabHalf) -> Self {
        match half {
            SlabHalf::Bottom => CollisionBox::new([0, 0, 0], [16, 8, 16]),
            SlabHalf::Top => CollisionBox::new([0, 8, 0], [16, 16, 16]),
        }
    }

    /// `min < max <= 16` on every axis.
    pub const fn is_valid(self) -> bool {
        let mut a = 0;
        while a < 3 {
            if self.min[a] >= self.max[a] || self.max[a] > 16 {
                return false;
            }
            a += 1;
        }
        true
    }
}

/// How a block collides (ADR 0016): what the physics `CollisionTable` is built from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlockCollision {
    /// No collision (air, water, plants).
    #[default]
    None,
    /// The whole cell.
    Full,
    /// A union of boxes (a slab is one box). An empty list is the same as `None`.
    Boxes(Vec<CollisionBox>),
}

impl BlockCollision {
    /// True for `None` (or an empty box list).
    pub fn is_none(&self) -> bool {
        match self {
            BlockCollision::None => true,
            BlockCollision::Full => false,
            BlockCollision::Boxes(boxes) => boxes.is_empty(),
        }
    }
}

bitflags::bitflags! {
    /// Per-state flags (SoA LUT).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct StateFlags: u16 {
        /// Collides (its `BlockDef::collision` is not `None`).
        const SOLID = 1;
        /// Hides the neighbor faces it touches: shape `Opaque` with a full cube model.
        const OCCLUDES = 1 << 1;
        /// Liquid: a `BlockModel::Fluid` or a `Translucent` block without collision.
        const FLUID = 1 << 2;
        /// Emits light (some `emission` channel above 0).
        const EMISSIVE = 1 << 3;
        /// Holds a water source besides itself: its [`WATERLOGGED`] property is `true` (ADR
        /// 0025).
        const WATERLOGGED = 1 << 4;
        /// Could hold a water source: it has the [`WATERLOGGED`] property and is not a full
        /// cube (ADR 0025).
        const WATERLOGGABLE = 1 << 5;
    }
}

/// Procedural texture pattern (M0 has no texture assets; the renderer synthesizes 16x16 layers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexturePattern {
    /// Base color with per-texel noise of amplitude `noise`.
    Noise,
    /// Noise plus a top band of `color` `rows` texels tall (grass side).
    TopBand {
        /// Band color.
        color: [u8; 3],
        /// Band height in texels (of 16).
        rows: u8,
    },
    /// Noise plus random speckles of `color`.
    Speckle {
        /// Speckle color.
        color: [u8; 3],
        /// Speckles per 256 texels.
        density: u8,
    },
    /// Leaves: noise with ~25% transparent texels (alpha 0), for cutout tests.
    Leaves,
    /// Horizontal planks.
    Planks,
    /// Bark: dark vertical cracks, broken here and there (log sides; a turned log runs them
    /// along its axis).
    Bark,
    /// Glass: a clear inside (alpha 0) with a light frame and a few streaks (translucent).
    Glass,
    /// Noise with a constant alpha (water; translucent).
    Liquid {
        /// Alpha of every texel.
        alpha: u8,
    },
    /// Grass blades on a clear background, for cross plants (binary alpha, like `Leaves`).
    Plant,
    /// A flower on a clear background: a stem of the base color and a head of `color` (binary
    /// alpha).
    Flower {
        /// Petal color.
        color: [u8; 3],
    },
    /// A torch on a clear background, laid out like Minecraft's: a stick of the base color in
    /// columns 7 and 8 from row 8 down, under a flame in rows 6 and 7 (binary alpha).
    Torch,
    /// Cobblestone: rounded stones of the base color, each a little lighter or darker, between
    /// dark joints.
    Cobble,
}

/// One built-in lamp of [`BlockRegistry::default_blocks`]: an opaque, solid cube that emits
/// colored light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinLamp {
    /// Block (and texture) name, `"boxel:lamp_orange"`.
    pub name: &'static str,
    /// Emission: red, green, blue, each `0..=15`.
    pub emission: [u8; 3],
    /// Texture base color and speckle color (sRGB).
    pub colors: [[u8; 3]; 2],
}

impl BuiltinLamp {
    /// The name without the `boxel:` prefix.
    pub fn short_name(&self) -> &'static str {
        self.name.strip_prefix("boxel:").unwrap_or(self.name)
    }
}

/// The built-in lamps, in state order (states 29 to 32 of [`BlockRegistry::default_blocks`]).
pub const LAMPS: [BuiltinLamp; 4] = [
    BuiltinLamp {
        name: "boxel:lamp_orange",
        emission: [14, 11, 6],
        colors: [[235, 150, 60], [255, 225, 160]],
    },
    BuiltinLamp {
        name: "boxel:lamp_red",
        emission: [14, 2, 2],
        colors: [[200, 40, 35], [255, 150, 130]],
    },
    BuiltinLamp {
        name: "boxel:lamp_blue",
        emission: [3, 6, 14],
        colors: [[50, 90, 220], [160, 205, 255]],
    },
    BuiltinLamp {
        name: "boxel:lamp_white",
        emission: [15, 15, 15],
        colors: [[230, 230, 220], [255, 255, 255]],
    },
];

/// Emission (red, green, blue) of `boxel:lava`, every level (red at Minecraft's 15).
pub const LAVA_EMISSION: [u8; 3] = [15, 7, 2];

/// Emission (red, green, blue) of `boxel:torch` and `boxel:wall_torch`: level 14 like
/// Minecraft's torch, a warm white.
pub const TORCH_EMISSION: [u8; 3] = [14, 13, 10];

/// One texture layer description.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextureSpec {
    /// Unique name (`"boxel:grass_top"`).
    pub name: String,
    /// Base sRGB color.
    pub base_color: [u8; 3],
    /// Noise amplitude (0..=64 reasonable).
    pub noise: u8,
    /// Pattern.
    pub pattern: TexturePattern,
}

/// Texture layer index in the block texture array (`0..4096`, fits `PackedQuad`'s 12 bits).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureId(pub u16);

/// Texture per face group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceTextures {
    /// +Y.
    pub top: TextureId,
    /// -Y.
    pub bottom: TextureId,
    /// ±X, ±Z.
    pub side: TextureId,
}

impl FaceTextures {
    /// Same texture on every face.
    pub const fn all(t: TextureId) -> Self {
        Self {
            top: t,
            bottom: t,
            side: t,
        }
    }
    /// Texture for `face`.
    pub const fn get(self, face: Face) -> TextureId {
        match face {
            Face::PosY => self.top,
            Face::NegY => self.bottom,
            _ => self.side,
        }
    }
}

/// Block definition: the data of one state. A block with state properties
/// ([`BlockRegistryBuilder::add_block_with_states`]) starts every state from one `BlockDef` and
/// a function adjusts it per state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockDef {
    /// Namespaced unique name (`"boxel:stone"`), the same for every state of the block.
    pub name: String,
    /// Render layer.
    pub shape: RenderShape,
    /// Geometry (ignored for `RenderShape::None`).
    pub model: BlockModel,
    /// How it collides (physics, ADR 0016). Usually [`BlockModel::default_collision`].
    pub collision: BlockCollision,
    /// Block light emission `[red, green, blue]`, each `0..=15` (white light has equal
    /// channels; `[0, 0, 0]` emits nothing). Emitters keep their emission even when opaque.
    pub emission: [u8; 3],
    /// Light opacity `0..=15`: 15 stops light (required for an `Opaque` cube), 0 lets it
    /// through (air, glass), 1..=14 lets it through at a cost of `max(1, opacity)` levels per
    /// voxel (leaves, water and slabs are 1). Any value above 0 also stops direct sky.
    pub light_opacity: u8,
    /// Textures (ignored for `RenderShape::None`).
    pub textures: FaceTextures,
}

/// Registry errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// Name already registered.
    #[error("duplicate name {0}")]
    Duplicate(String),
    /// Name must be `namespace:path` with `[a-z0-9_]` parts.
    #[error("invalid name {0}")]
    InvalidName(String),
    /// More than 4096 textures, [`MAX_STATES`] states or [`MAX_MODEL_BOXES`] distinct model
    /// boxes.
    #[error("registry full: {0}")]
    Full(&'static str),
    /// A property of the block with an invalid name or value, no values, a default that is not
    /// one of them, or a name used twice.
    #[error("invalid state property of {0}")]
    InvalidProperty(String),
    /// Texture id not registered.
    #[error("unknown texture id {0}")]
    UnknownTexture(u16),
    /// Emission channel or light opacity above 15, or an `Opaque` cube whose light opacity is
    /// not 15.
    #[error("invalid light values of {0}")]
    InvalidLight(String),
    /// A collision or model box outside the cell or empty (`min < max <= 16` on every axis), or
    /// a model box texture offset outside `-32..32`.
    #[error("invalid collision box of {0}")]
    InvalidCollision(String),
}

/// Index of a block (not of a state) in its registry, in registration order (`0` = air).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct BlockId(pub u16);

/// One block of the registry: its name, its properties and where its states are.
#[derive(Clone, Debug)]
struct BlockInfo {
    name: String,
    /// The first state of the block; its states are `first..first + count`.
    first: u16,
    count: u16,
    /// The default state (every property at its default value).
    default: u16,
    /// Sorted by name.
    properties: Vec<Property>,
    /// Id step of each property: the last one varies fastest (stride 1).
    strides: Vec<u16>,
}

impl BlockInfo {
    fn values(&self, state: BlockState) -> StateValues<'_> {
        StateValues {
            properties: &self.properties,
            strides: &self.strides,
            offset: state.0 - self.first,
        }
    }
}

/// Builder; `boxel:air` is pre-registered as `BlockState(0)`.
#[derive(Debug, Default)]
pub struct BlockRegistryBuilder {
    textures: Vec<TextureSpec>,
    /// One per state, in state order.
    states: Vec<BlockDef>,
    blocks: Vec<BlockInfo>,
    /// The distinct [`ModelBox`]es of the states so far (at most [`MAX_MODEL_BOXES`]).
    model_boxes: std::collections::HashSet<ModelBox>,
}

impl BlockRegistryBuilder {
    /// New builder with `boxel:air` (state 0, shape `None`, texture 0 unused).
    pub fn new() -> Self {
        let mut b = Self::default();
        b.add_block(BlockDef {
            name: "boxel:air".to_string(),
            shape: RenderShape::None,
            model: BlockModel::CUBE,
            collision: BlockCollision::None,
            emission: [0; 3],
            light_opacity: 0,
            textures: FaceTextures::all(TextureId(0)),
        })
        .expect("air");
        b
    }

    /// A builder with the built-in M0 content of [`BlockRegistry::default_blocks`], to add more
    /// blocks after it (their states follow the built-in ones).
    pub fn with_default_blocks() -> Self {
        use TexturePattern as P;
        let mut b = BlockRegistryBuilder::new();
        let mut tex = |name: &str, rgb: [u8; 3], noise: u8, pattern: TexturePattern| {
            b.add_texture(TextureSpec {
                name: format!("boxel:{name}"),
                base_color: rgb,
                noise,
                pattern,
            })
            .expect("built-in texture")
        };
        let stone = tex("stone", [125, 125, 125], 18, P::Noise);
        let dirt = tex("dirt", [134, 96, 67], 16, P::Noise);
        let grass_top = tex("grass_top", [95, 159, 53], 18, P::Noise);
        let grass_side = tex(
            "grass_side",
            [134, 96, 67],
            16,
            P::TopBand {
                color: [95, 159, 53],
                rows: 4,
            },
        );
        let sand = tex("sand", [219, 207, 163], 10, P::Noise);
        let gravel = tex(
            "gravel",
            [136, 126, 126],
            14,
            P::Speckle {
                color: [90, 85, 85],
                density: 40,
            },
        );
        let snow = tex("snow", [240, 245, 250], 6, P::Noise);
        let bedrock = tex(
            "bedrock",
            [70, 70, 70],
            24,
            P::Speckle {
                color: [30, 30, 30],
                density: 60,
            },
        );
        let log_side = tex("log_side", [102, 81, 51], 14, P::Bark);
        let log_top = tex("log_top", [156, 127, 78], 10, P::Noise);
        let leaves = tex("leaves", [60, 130, 40], 20, P::Leaves);
        let water = tex("water", [40, 90, 200], 6, P::Liquid { alpha: 160 });
        let planks = tex("planks", [162, 130, 78], 10, P::Planks);
        // Glowing cubes: a base color with bright speckles.
        let glow = |spark: [u8; 3]| P::Speckle {
            color: spark,
            density: 60,
        };
        let lamp_textures = LAMPS.map(|lamp| {
            let [base, spark] = lamp.colors;
            tex(lamp.short_name(), base, 10, glow(spark))
        });
        let lava = tex("lava", [207, 78, 16], 22, glow([255, 190, 50]));
        // Non-cube content (ADR 0018), after the lamps so the earlier texture ids stay.
        let glass = tex("glass", [214, 236, 240], 4, P::Glass);
        let short_grass = tex("short_grass", [124, 178, 66], 18, P::Plant);
        let poppy = tex(
            "poppy",
            [58, 120, 38],
            10,
            P::Flower {
                color: [205, 30, 30],
            },
        );
        // Light opacity: 15 for opaque cubes, 1 for leaves and water (light passes, losing one
        // level per voxel like in air, but direct sky stops at them).
        let block =
            |name: &str, shape: RenderShape, solid: bool, textures: FaceTextures| BlockDef {
                name: format!("boxel:{name}"),
                shape,
                model: BlockModel::CUBE,
                collision: if solid {
                    BlockCollision::Full
                } else {
                    BlockCollision::None
                },
                emission: [0; 3],
                light_opacity: if shape == RenderShape::Opaque {
                    OPAQUE_OPACITY
                } else {
                    1
                },
                textures,
            };
        let all = FaceTextures::all;
        let opaque = RenderShape::Opaque;
        for def in [
            block("stone", opaque, true, all(stone)),
            block("dirt", opaque, true, all(dirt)),
            block(
                "grass",
                opaque,
                true,
                FaceTextures {
                    top: grass_top,
                    bottom: dirt,
                    side: grass_side,
                },
            ),
            block("sand", opaque, true, all(sand)),
            block("gravel", opaque, true, all(gravel)),
            block("snow", opaque, true, all(snow)),
            block("bedrock", opaque, true, all(bedrock)),
        ] {
            b.add_block(def).expect("built-in block");
        }
        // The log lies along its `axis` (ADR 0018, 0019): the end texture on the faces across it.
        let log_textures = FaceTextures {
            top: log_top,
            bottom: log_top,
            side: log_side,
        };
        b.add_block_with_states(
            block("log", opaque, true, log_textures),
            vec![Property::new("axis", ["x", "y", "z"]).with_default("y")],
            |state, def| {
                let axis = state.get("axis").and_then(Axis::from_name);
                def.model = BlockModel::Cube {
                    axis: axis.unwrap_or_default(),
                };
            },
        )
        .expect("built-in log");
        b.add_block(block("leaves", RenderShape::Cutout, true, all(leaves)))
            .expect("built-in leaves");
        // Water has Minecraft's `level` (ADR 0021): 0 is a source, 1 to 7 flowing water (lower
        // as the level grows) and 8 to 15 falling water. Every level draws, collides and lets
        // light through the same way: only the fluid ticks and the mesher's corner heights read
        // it.
        b.add_block_with_states(
            BlockDef {
                model: BlockModel::Fluid,
                ..block("water", RenderShape::Translucent, false, all(water))
            },
            vec![Property::int(FLUID_LEVEL, 0..=15)],
            |_, _| {},
        )
        .expect("built-in water");
        b.add_block(block("planks", opaque, true, all(planks)))
            .expect("built-in planks");
        // The first emitters (ADR 0015): full-cube colored lamps. Torches wait for non-cube
        // shapes.
        let emitters = LAMPS
            .iter()
            .zip(lamp_textures)
            .map(|(lamp, texture)| (lamp.short_name(), lamp.emission, texture));
        for (name, emission, texture) in emitters {
            let def = BlockDef {
                emission,
                ..block(name, opaque, true, all(texture))
            };
            b.add_block(def).expect("built-in emitter");
        }
        // Lava flows like water with its own rules (ADR 0025) and the same `level` property. It
        // glows, is drawn opaque (Minecraft's solid layer) but hides no neighbor face, lets
        // everything through and lets light through like water (opacity 1, the sky stops).
        b.add_block_with_states(
            BlockDef {
                model: BlockModel::Fluid,
                emission: LAVA_EMISSION,
                light_opacity: 1,
                ..block("lava", opaque, false, all(lava))
            },
            vec![Property::int(FLUID_LEVEL, 0..=15)],
            |_, _| {},
        )
        .expect("built-in lava");
        // Non-cube blocks (ADR 0018). Glass is a clear cube: light and sky pass (opacity 0).
        // Slabs let light through at the cost of leaves (a slab roof is not dark; opacity 1 keeps
        // the light flood on its fast path); a double slab is a plain stone cube. Plants stop
        // nothing.
        let model = |name: &str, shape, model: BlockModel, opacity, textures| BlockDef {
            name: format!("boxel:{name}"),
            shape,
            model,
            collision: model.default_collision(),
            emission: [0; 3],
            light_opacity: opacity,
            textures,
        };
        b.add_block(model(
            "glass",
            RenderShape::Translucent,
            BlockModel::CUBE,
            0,
            all(glass),
        ))
        .expect("built-in glass");
        // A slab holds water (`waterlogged`, ADR 0025) unless it is double.
        b.add_block_with_states(
            model(
                "stone_slab",
                opaque,
                BlockModel::Slab(SlabHalf::Bottom),
                1,
                all(stone),
            ),
            vec![
                Property::new("type", ["bottom", "top", "double"]),
                Property::boolean(WATERLOGGED),
            ],
            |state, def| match state.get("type") {
                Some("top") => {
                    def.model = BlockModel::Slab(SlabHalf::Top);
                    def.collision = def.model.default_collision();
                }
                Some("double") => {
                    def.model = BlockModel::CUBE;
                    def.collision = BlockCollision::Full;
                    def.light_opacity = OPAQUE_OPACITY;
                }
                _ => {}
            },
        )
        .expect("built-in slab");
        for def in [
            model(
                "short_grass",
                RenderShape::Cutout,
                BlockModel::Cross,
                0,
                all(short_grass),
            ),
            model(
                "poppy",
                RenderShape::Cutout,
                BlockModel::Cross,
                0,
                all(poppy),
            ),
        ] {
            b.add_block(def).expect("built-in block");
        }
        b.add_default_shapes(stone, planks);
        b.add_fluid_products();
        b
    }

    /// The blocks lava makes when it meets water (ADR 0025), after the torches: cobblestone and
    /// obsidian, opaque solid cubes with their textures (the last layers).
    fn add_fluid_products(&mut self) {
        let mut cube = |name: &str, base_color, noise, pattern| {
            let texture = self
                .add_texture(TextureSpec {
                    name: format!("boxel:{name}"),
                    base_color,
                    noise,
                    pattern,
                })
                .expect("built-in texture");
            self.add_block(BlockDef {
                name: format!("boxel:{name}"),
                shape: RenderShape::Opaque,
                model: BlockModel::CUBE,
                collision: BlockCollision::Full,
                emission: [0; 3],
                light_opacity: OPAQUE_OPACITY,
                textures: FaceTextures::all(texture),
            })
            .expect("built-in block");
        };
        cube("cobblestone", [122, 122, 122], 14, TexturePattern::Cobble);
        cube(
            "obsidian",
            [20, 16, 31],
            6,
            TexturePattern::Speckle {
                color: [58, 38, 92],
                density: 40,
            },
        );
    }

    /// The blocks of ADR 0020, after the plants: stone and oak stairs, the oak fence, the torch
    /// and the wall torch (with its texture, the last layer).
    fn add_default_shapes(&mut self, stone: TextureId, planks: TextureId) {
        let torch = self
            .add_texture(TextureSpec {
                name: "boxel:torch".to_string(),
                base_color: [110, 82, 46],
                noise: 8,
                pattern: TexturePattern::Torch,
            })
            .expect("built-in texture");
        let facing = || Property::new("facing", Facing::ALL.map(Facing::name));
        let facing_of = |state: &StateValues<'_>| {
            state
                .get("facing")
                .and_then(Facing::from_name)
                .unwrap_or_default()
        };
        // Stairs let light through like slabs (opacity 1). `waterlogged` is Minecraft's: the
        // stairs hold a water source (ADR 0025).
        for (name, texture) in [("stone_stairs", stone), ("oak_stairs", planks)] {
            let def = BlockDef {
                name: format!("boxel:{name}"),
                shape: RenderShape::Opaque,
                model: BlockModel::Stairs {
                    facing: Facing::North,
                    half: SlabHalf::Bottom,
                    shape: StairShape::Straight,
                },
                collision: BlockCollision::None,
                emission: [0; 3],
                light_opacity: 1,
                textures: FaceTextures::all(texture),
            };
            let properties = vec![
                facing(),
                Property::new("half", ["bottom", "top"]),
                Property::new("shape", StairShape::ALL.map(StairShape::name)),
                Property::boolean(WATERLOGGED),
            ];
            self.add_block_with_states(def, properties, |state, def| {
                def.model = BlockModel::Stairs {
                    facing: facing_of(state),
                    half: state
                        .get("half")
                        .and_then(shapes::half_from_name)
                        .unwrap_or_default(),
                    shape: state
                        .get("shape")
                        .and_then(StairShape::from_name)
                        .unwrap_or_default(),
                };
                def.collision = def.model.default_collision();
            })
            .expect("built-in stairs");
        }
        // The fence stops no light (but direct sky when it holds water); its sides follow its
        // neighbors (`boxel_voxel::rules`).
        let fence = BlockDef {
            name: "boxel:oak_fence".to_string(),
            shape: RenderShape::Opaque,
            model: BlockModel::Fence {
                sides: FenceSides::NONE,
            },
            collision: BlockCollision::None,
            emission: [0; 3],
            light_opacity: 0,
            textures: FaceTextures::all(planks),
        };
        let mut properties = Facing::ALL.map(|f| Property::boolean(f.name())).to_vec();
        properties.push(Property::boolean(WATERLOGGED));
        self.add_block_with_states(fence, properties, |state, def| {
            let mut sides = FenceSides::NONE;
            for f in Facing::ALL {
                sides = sides.with(f, state.get(f.name()) == Some("true"));
            }
            def.model = BlockModel::Fence { sides };
            def.collision = def.model.default_collision();
            if state.get(WATERLOGGED) == Some("true") {
                def.light_opacity = def.light_opacity.max(1);
            }
        })
        .expect("built-in fence");
        // Torches: cutout sticks that glow ([`TORCH_EMISSION`]), no collision, no opacity.
        let torch_def = |name: &str, model: BlockModel| BlockDef {
            name: format!("boxel:{name}"),
            shape: RenderShape::Cutout,
            model,
            collision: BlockCollision::None,
            emission: TORCH_EMISSION,
            light_opacity: 0,
            textures: FaceTextures::all(torch),
        };
        self.add_block(torch_def("torch", BlockModel::Torch))
            .expect("built-in torch");
        self.add_block_with_states(
            torch_def(
                "wall_torch",
                BlockModel::WallTorch {
                    facing: Facing::North,
                },
            ),
            vec![facing()],
            |state, def| {
                def.model = BlockModel::WallTorch {
                    facing: facing_of(state),
                };
            },
        )
        .expect("built-in wall torch");
    }

    /// Registers a texture layer.
    pub fn add_texture(&mut self, spec: TextureSpec) -> Result<TextureId, RegistryError> {
        validate_name(&spec.name)?;
        if self.textures.iter().any(|t| t.name == spec.name) {
            return Err(RegistryError::Duplicate(spec.name));
        }
        if self.textures.len() >= 4096 {
            return Err(RegistryError::Full("textures"));
        }
        self.textures.push(spec);
        Ok(TextureId((self.textures.len() - 1) as u16))
    }

    /// Registers a block without state properties (one state); returns its state.
    pub fn add_block(&mut self, def: BlockDef) -> Result<BlockState, RegistryError> {
        self.add_block_with_states(def, Vec::new(), |_, _| {})
    }

    /// Registers a block with state `properties` (ADR 0019); returns its default state. Each
    /// state starts from a copy of `def` that `state` adjusts for its property values (a model,
    /// a collision, a light opacity...): the per-state data of all its states comes from one
    /// function, however many there are. The properties are sorted by name; the states take
    /// consecutive ids, the last property varying fastest. Every state is checked as
    /// [`BlockRegistryBuilder::add_block`] checks its one state, and keeps the name of `def`.
    pub fn add_block_with_states(
        &mut self,
        def: BlockDef,
        mut properties: Vec<Property>,
        mut state: impl FnMut(&StateValues<'_>, &mut BlockDef),
    ) -> Result<BlockState, RegistryError> {
        validate_name(&def.name)?;
        if self.blocks.iter().any(|b| b.name == def.name) {
            return Err(RegistryError::Duplicate(def.name));
        }
        properties.sort_by(|a, b| a.name().cmp(b.name()));
        let repeated = properties
            .windows(2)
            .any(|pair| pair[0].name() == pair[1].name());
        if repeated || !properties.iter().all(Property::is_valid) {
            return Err(RegistryError::InvalidProperty(def.name));
        }
        // Strides from the last property (1) back; the product is the state count.
        let mut strides = vec![0u16; properties.len()];
        let mut count = 1usize;
        for (stride, property) in strides.iter_mut().zip(&properties).rev() {
            *stride = u16::try_from(count).map_err(|_| RegistryError::Full("block states"))?;
            count = count.saturating_mul(property.values().len());
        }
        let first = self.states.len();
        if first + count > MAX_STATES {
            return Err(RegistryError::Full("block states"));
        }
        let (first, count) = (first as u16, count as u16);
        let default = first
            + properties
                .iter()
                .zip(&strides)
                .map(|(p, &stride)| p.default_index() as u16 * stride)
                .sum::<u16>();
        let mut states = Vec::with_capacity(usize::from(count));
        for offset in 0..count {
            let values = StateValues {
                properties: &properties,
                strides: &strides,
                offset,
            };
            let mut state_def = def.clone();
            state(&values, &mut state_def);
            state_def.name.clone_from(&def.name);
            self.check_state(&state_def)?;
            states.push(state_def);
        }
        let mut new_boxes = std::collections::HashSet::new();
        for def in states.iter().filter(|d| d.shape != RenderShape::None) {
            for b in def.model.boxes() {
                if !b.is_valid() {
                    return Err(RegistryError::InvalidCollision(def.name.clone()));
                }
                if !self.model_boxes.contains(&b) {
                    new_boxes.insert(b);
                }
            }
        }
        if self.model_boxes.len() + new_boxes.len() > MAX_MODEL_BOXES {
            return Err(RegistryError::Full("model boxes"));
        }
        self.model_boxes.extend(new_boxes);
        self.states.extend(states);
        self.blocks.push(BlockInfo {
            name: def.name,
            first,
            count,
            default,
            properties,
            strides,
        });
        Ok(BlockState(default))
    }

    /// The light, collision and texture checks of one state.
    fn check_state(&self, def: &BlockDef) -> Result<(), RegistryError> {
        let channels_ok = def.emission.iter().all(|&c| c <= crate::MAX_LIGHT);
        let opaque_cube = def.shape == RenderShape::Opaque && def.model.is_solid_cube();
        let opacity_ok = def.light_opacity <= OPAQUE_OPACITY
            && (!opaque_cube || def.light_opacity == OPAQUE_OPACITY);
        if !channels_ok || !opacity_ok {
            return Err(RegistryError::InvalidLight(def.name.clone()));
        }
        if let BlockCollision::Boxes(boxes) = &def.collision
            && !boxes.iter().all(|b| b.is_valid())
        {
            return Err(RegistryError::InvalidCollision(def.name.clone()));
        }
        if def.shape != RenderShape::None {
            for t in [def.textures.top, def.textures.bottom, def.textures.side] {
                if t.0 as usize >= self.textures.len() {
                    return Err(RegistryError::UnknownTexture(t.0));
                }
            }
        }
        Ok(())
    }

    /// Freezes the registry (builds the `SoA` lookup tables).
    pub fn build(self) -> BlockRegistry {
        let mut by_name =
            std::collections::HashMap::with_hasher(foldhash::fast::FixedState::default());
        let mut state_block = Vec::with_capacity(self.states.len());
        for (i, b) in self.blocks.iter().enumerate() {
            by_name.insert(b.name.clone(), BlockId(i as u16));
            state_block.extend(std::iter::repeat_n(i as u16, usize::from(b.count)));
        }
        let states = &self.states;
        let mut flags: Vec<StateFlags> = states
            .iter()
            .map(|b| {
                let mut f = StateFlags::empty();
                let solid = !b.collision.is_none();
                f.set(StateFlags::SOLID, solid);
                f.set(
                    StateFlags::OCCLUDES,
                    b.shape == RenderShape::Opaque && b.model.is_solid_cube(),
                );
                let fluid = b.shape == RenderShape::Translucent || b.model == BlockModel::Fluid;
                f.set(StateFlags::FLUID, fluid && !solid);
                f.set(StateFlags::EMISSIVE, b.emission != [0; 3]);
                f
            })
            .collect();
        for block in &self.blocks {
            if !block.properties.iter().any(|p| p.name() == WATERLOGGED) {
                continue;
            }
            // A full cube holds no water (a double slab), whatever its property says.
            for state in block.first..block.first + block.count {
                if states[usize::from(state)].model.is_solid_cube() {
                    continue;
                }
                let f = &mut flags[usize::from(state)];
                f.insert(StateFlags::WATERLOGGABLE);
                if block.values(BlockState(state)).get(WATERLOGGED) == Some("true") {
                    f.insert(StateFlags::WATERLOGGED);
                }
            }
        }
        BlockRegistry {
            state_block,
            shapes: states.iter().map(|b| b.shape).collect(),
            models: states.iter().map(|b| b.model).collect(),
            collisions: states.iter().map(|b| b.collision.clone()).collect(),
            flags,
            emission: states
                .iter()
                .map(|b| LightValue::from_rgb(b.emission))
                .collect(),
            opacity: states.iter().map(|b| b.light_opacity).collect(),
            faces: states.iter().map(|b| b.textures).collect(),
            textures: self.textures,
            blocks: self.blocks,
            by_name,
        }
    }
}

/// `namespace:path`, both non-empty, `[a-z0-9_]` (path may contain `/`).
fn validate_name(name: &str) -> Result<(), RegistryError> {
    let ok = name.split_once(':').is_some_and(|(ns, path)| {
        !ns.is_empty()
            && !path.is_empty()
            && ns
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            && path
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '/')
    });
    if ok {
        Ok(())
    } else {
        Err(RegistryError::InvalidName(name.to_string()))
    }
}

/// Frozen registry with `SoA` lookup tables indexed by `BlockState.0`.
#[derive(Debug, Clone)]
pub struct BlockRegistry {
    /// The block (index into `blocks`) of each state.
    state_block: Vec<u16>,
    shapes: Vec<RenderShape>,
    models: Vec<BlockModel>,
    collisions: Vec<BlockCollision>,
    flags: Vec<StateFlags>,
    emission: Vec<LightValue>,
    opacity: Vec<u8>,
    faces: Vec<FaceTextures>,
    textures: Vec<TextureSpec>,
    blocks: Vec<BlockInfo>,
    by_name: std::collections::HashMap<String, BlockId, foldhash::fast::FixedState>,
}

impl BlockRegistry {
    /// Built-in content ([`BlockRegistryBuilder::with_default_blocks`], built), in this exact
    /// order (state ids are frozen for tests and worldgen): 0 `boxel:air`, 1 `boxel:stone`,
    /// 2 `boxel:dirt`, 3 `boxel:grass`, 4 `boxel:sand`, 5 `boxel:gravel`, 6 `boxel:snow`,
    /// 7 `boxel:bedrock`, 8 to 10 `boxel:log[axis=x|y|z]` (default 9, `axis=y`),
    /// 11 `boxel:leaves` (Cutout), 12 to 27 `boxel:water[level=0..15]` (Translucent fluid,
    /// default 12, the source; ADR 0021), 28 `boxel:planks`, then the lamps (opaque, solid,
    /// light opacity 15; emission red, green, blue): 29 `boxel:lamp_orange` (14, 11, 6), 30
    /// `boxel:lamp_red` (14, 2, 2), 31 `boxel:lamp_blue` (3, 6, 14), 32 `boxel:lamp_white`
    /// (15, 15, 15) ([`LAMPS`]); 33 to 48 `boxel:lava[level=0..15]` (an Opaque fluid with no
    /// collision, light opacity 1, emission (15, 7, 2), [`LAVA_EMISSION`]; default 33, the
    /// source; ADR 0025); then the blocks of ADR 0018: 49 `boxel:glass` (Translucent cube,
    /// solid, light opacity 0), 50 to 55 `boxel:stone_slab[type=bottom|top|double,
    /// waterlogged=false|true]` (default 50; the halves are Opaque slabs with light opacity 1,
    /// `double` an Opaque stone cube), 56 `boxel:short_grass` and 57 `boxel:poppy` (Cutout
    /// crosses, no collision, light opacity 0); then the blocks of ADR 0020: 58 to 137
    /// `boxel:stone_stairs` and 138 to 217 `boxel:oak_stairs` (`facing`, `half`, `shape`,
    /// `waterlogged`; default the first, all values first; Opaque, light opacity 1), 218 to 249
    /// `boxel:oak_fence` (`east`, `north`, `south`, `waterlogged`, `west`; default 218,
    /// unconnected and dry; Opaque, light opacity 0, 1 with water), 250 `boxel:torch` and 251 to
    /// 254 `boxel:wall_torch[facing=north|south|west|east]` (Cutout, no collision, light opacity
    /// 0, emission [`TORCH_EMISSION`]); then what lava makes with water (ADR 0025): 255
    /// `boxel:cobblestone` and 256 `boxel:obsidian` (Opaque solid cubes). The states whose
    /// `waterlogged` is `true` hold a water source ([`StateFlags::WATERLOGGED`]).
    pub fn default_blocks() -> BlockRegistry {
        BlockRegistryBuilder::with_default_blocks().build()
    }
    /// Number of states.
    pub fn len(&self) -> usize {
        self.state_block.len()
    }
    /// True if only air.
    pub fn is_empty(&self) -> bool {
        self.state_block.len() <= 1
    }
    /// The default state of the block named `name` (`boxel:log` -> `boxel:log[axis=y]`). A
    /// state with properties is [`BlockRegistry::parse_state`].
    pub fn by_name(&self, name: &str) -> Option<BlockState> {
        self.block_by_name(name).map(|b| self.default_state(b))
    }
    /// The block of the name.
    pub fn block_by_name(&self, name: &str) -> Option<BlockId> {
        self.by_name.get(name).copied()
    }
    /// Block name of a state (`"boxel:unknown"` if out of range); the same for every state of a
    /// block. [`BlockRegistry::state_name`] adds the property values.
    pub fn name(&self, s: BlockState) -> &str {
        self.info(s).map_or("boxel:unknown", |b| b.name.as_str())
    }
    /// Number of blocks (air included).
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }
    /// Every block, in registration order.
    pub fn blocks(&self) -> impl ExactSizeIterator<Item = BlockId> + use<> {
        (0..self.blocks.len() as u16).map(BlockId)
    }
    /// The block of a state (`None` if out of range).
    pub fn block(&self, s: BlockState) -> Option<BlockId> {
        self.state_block.get(usize::from(s.0)).copied().map(BlockId)
    }
    fn info(&self, s: BlockState) -> Option<&BlockInfo> {
        let b = *self.state_block.get(usize::from(s.0))?;
        self.blocks.get(usize::from(b))
    }
    /// Name of a block (`"boxel:unknown"` if out of range).
    pub fn block_name(&self, b: BlockId) -> &str {
        self.blocks
            .get(usize::from(b.0))
            .map_or("boxel:unknown", |b| b.name.as_str())
    }
    /// The default state of a block (air if out of range).
    pub fn default_state(&self, b: BlockId) -> BlockState {
        self.blocks
            .get(usize::from(b.0))
            .map_or(BlockState::AIR, |b| BlockState(b.default))
    }
    /// The states of a block, in id order (none if out of range).
    pub fn states(&self, b: BlockId) -> impl ExactSizeIterator<Item = BlockState> + use<> {
        let range = self
            .blocks
            .get(usize::from(b.0))
            .map_or(0..0, |b| b.first..b.first + b.count);
        range.map(BlockState)
    }
    /// The state properties of a block, sorted by name (none if out of range).
    pub fn properties(&self, b: BlockId) -> &[Property] {
        self.blocks
            .get(usize::from(b.0))
            .map_or(&[], |b| b.properties.as_slice())
    }
    /// The property values of a state (none if out of range).
    pub fn values(&self, s: BlockState) -> StateValues<'_> {
        self.info(s).map_or(StateValues::EMPTY, |b| b.values(s))
    }
    /// The value of property `property` of `s` (`boxel:log[axis=x]`, `axis` -> `x`); `None` if
    /// the block has no such property.
    pub fn property(&self, s: BlockState, property: &str) -> Option<&str> {
        self.values(s).get(property)
    }
    /// `s` with `property` set to `value` (the other properties kept); `None` if the block has no
    /// such property or value.
    pub fn with(&self, s: BlockState, property: &str, value: &str) -> Option<BlockState> {
        let b = self.info(s)?;
        let i = b.properties.iter().position(|p| p.name() == property)?;
        let new = b.properties[i].index_of(value)? as u16;
        let old = b.values(s).index(property)? as u16;
        Some(BlockState(s.0 - old * b.strides[i] + new * b.strides[i]))
    }
    /// `s` written as `name[property=value,...]` (`boxel:stone_slab[type=top]`; just the name for
    /// a block without properties): its `Display` is the inverse of
    /// [`BlockRegistry::parse_state`].
    pub fn state_name(&self, s: BlockState) -> StateName<'_> {
        StateName {
            name: self.name(s),
            values: self.values(s),
        }
    }
    /// The state written as `name` (the default state) or `name[property=value,...]`
    /// (`boxel:stone_slab[type=top]`): properties left out take their default value; an unknown
    /// block, property or value is an error.
    pub fn parse_state(&self, text: &str) -> Result<BlockState, StateParseError> {
        let (name, pairs) = split_state(text)?;
        let block = self
            .block_by_name(name)
            .ok_or_else(|| StateParseError::UnknownBlock(name.to_string()))?;
        let mut state = self.default_state(block);
        let mut seen: Vec<&str> = Vec::with_capacity(pairs.len());
        for (property, value) in pairs {
            if seen.contains(&property) {
                return Err(StateParseError::Repeated {
                    block: name.to_string(),
                    property: property.to_string(),
                });
            }
            seen.push(property);
            let Some(known) = self.properties(block).iter().find(|p| p.name() == property) else {
                return Err(StateParseError::UnknownProperty {
                    block: name.to_string(),
                    property: property.to_string(),
                });
            };
            state = self.with(state, known.name(), value).ok_or_else(|| {
                StateParseError::UnknownValue {
                    block: name.to_string(),
                    property: property.to_string(),
                    value: value.to_string(),
                }
            })?;
        }
        Ok(state)
    }
    /// Shape (out of range -> `None`).
    pub fn shape(&self, s: BlockState) -> RenderShape {
        self.shapes.get(s.0 as usize).copied().unwrap_or_default()
    }
    /// Geometry (out of range -> the cube).
    pub fn model(&self, s: BlockState) -> BlockModel {
        self.models.get(s.0 as usize).copied().unwrap_or_default()
    }
    /// How `s` collides (out of range -> `None`).
    pub fn collision(&self, s: BlockState) -> &BlockCollision {
        self.collisions
            .get(s.0 as usize)
            .unwrap_or(&BlockCollision::None)
    }
    /// True if `s` hides the neighbor faces it touches (an `Opaque` full cube,
    /// [`StateFlags::OCCLUDES`]).
    pub fn occludes(&self, s: BlockState) -> bool {
        self.flags(s).contains(StateFlags::OCCLUDES)
    }
    /// True if `s` holds a water source besides itself ([`StateFlags::WATERLOGGED`], ADR 0025).
    pub fn is_waterlogged(&self, s: BlockState) -> bool {
        self.flags(s).contains(StateFlags::WATERLOGGED)
    }
    /// True if `s` could hold water: its block has the [`WATERLOGGED`] property and it is not a
    /// full cube (a double slab holds none, as in Minecraft).
    pub fn can_be_waterlogged(&self, s: BlockState) -> bool {
        self.flags(s).contains(StateFlags::WATERLOGGABLE)
    }
    /// `s` with its [`WATERLOGGED`] property set to `water` (`None` without the property).
    pub fn with_water(&self, s: BlockState, water: bool) -> Option<BlockState> {
        self.with(s, WATERLOGGED, if water { "true" } else { "false" })
    }
    /// Flags.
    pub fn flags(&self, s: BlockState) -> StateFlags {
        self.flags.get(s.0 as usize).copied().unwrap_or_default()
    }
    /// Light emission (sky 0; out of range -> none).
    pub fn emission(&self, s: BlockState) -> LightValue {
        self.emission
            .get(s.0 as usize)
            .copied()
            .unwrap_or(LightValue::DARK)
    }
    /// Light opacity `0..=15` (out of range -> 0).
    pub fn light_opacity(&self, s: BlockState) -> u8 {
        self.opacity.get(s.0 as usize).copied().unwrap_or(0)
    }
    /// Texture layer of `face` of `s` (for a turned cube, the end texture is on the faces
    /// across its axis).
    pub fn face_texture(&self, s: BlockState, face: Face) -> TextureId {
        self.faces
            .get(s.0 as usize)
            .map_or(TextureId(0), |f| f.get(self.model(s).texture_face(face)))
    }
    /// Texture layers in id order (the renderer builds its texture array from this).
    pub fn textures(&self) -> &[TextureSpec] {
        &self.textures
    }
    /// Mesher voxel value of a state: `0` for `RenderShape::None`, else `s.0` (the identity map;
    /// below [`MAX_STATES`], so the mesher's top bit stays free).
    pub fn material(&self, s: BlockState) -> u16 {
        if self.shape(s) == RenderShape::None {
            0
        } else {
            s.0
        }
    }
}
