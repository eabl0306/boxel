//! Data-driven block registry. M0: one state per block (no properties yet); `BlockState(u16)` is
//! the dense runtime id (`0` = air), assigned in registration order. Frozen before the world
//! loads and shared as `Arc<BlockRegistry>`.
use crate::{Face, LightValue, OPAQUE_OPACITY};

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
    /// the neighbor faces it touches ([`StateFlags::OCCLUDES`]); other models hide nothing.
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
}

impl Default for BlockModel {
    fn default() -> Self {
        Self::CUBE
    }
}

impl BlockModel {
    /// The unturned full cube.
    pub const CUBE: BlockModel = BlockModel::Cube { axis: Axis::Y };

    /// True for the full cube (any axis).
    pub const fn is_cube(self) -> bool {
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
    /// slabs, a smaller box around the stem for crosses.
    pub fn outline(self) -> BlockCollision {
        match self {
            BlockModel::Cube { .. } => BlockCollision::Full,
            BlockModel::Slab(half) => BlockCollision::Boxes(vec![CollisionBox::slab(half)]),
            BlockModel::Cross => {
                BlockCollision::Boxes(vec![CollisionBox::new([2, 0, 2], [14, 13, 14])])
            }
        }
    }

    /// The usual collision of the model: full cubes and slabs collide with their shape, crosses
    /// do not collide.
    pub fn default_collision(self) -> BlockCollision {
        match self {
            BlockModel::Cross => BlockCollision::None,
            other => other.outline(),
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
        /// Liquid.
        const FLUID = 1 << 2;
        /// Emits light (some `emission` channel above 0).
        const EMISSIVE = 1 << 3;
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

/// The built-in lamps, in state order (states 12 to 15 of [`BlockRegistry::default_blocks`]).
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

/// Emission (red, green, blue) of the built-in lava placeholder, `boxel:lava` (state 16).
pub const LAVA_EMISSION: [u8; 3] = [15, 7, 2];

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

/// Block definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockDef {
    /// Namespaced unique name (`"boxel:stone"`).
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
    /// More than 4096 textures or 65535 states.
    #[error("registry full: {0}")]
    Full(&'static str),
    /// Texture id not registered.
    #[error("unknown texture id {0}")]
    UnknownTexture(u16),
    /// Emission channel or light opacity above 15, or an `Opaque` cube whose light opacity is
    /// not 15.
    #[error("invalid light values of {0}")]
    InvalidLight(String),
    /// A collision box outside the cell or empty (`min < max <= 16` on every axis).
    #[error("invalid collision box of {0}")]
    InvalidCollision(String),
}

/// Builder; `boxel:air` is pre-registered as `BlockState(0)`.
#[derive(Debug, Default)]
pub struct BlockRegistryBuilder {
    textures: Vec<TextureSpec>,
    blocks: Vec<BlockDef>,
}

impl BlockRegistryBuilder {
    /// New builder with `boxel:air` (state 0, shape `None`, texture 0 unused).
    pub fn new() -> Self {
        Self {
            textures: Vec::new(),
            blocks: vec![BlockDef {
                name: "boxel:air".to_string(),
                shape: RenderShape::None,
                model: BlockModel::CUBE,
                collision: BlockCollision::None,
                emission: [0; 3],
                light_opacity: 0,
                textures: FaceTextures::all(TextureId(0)),
            }],
        }
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
        let log_side = tex("log_side", [102, 81, 51], 14, P::Planks);
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
            block(
                "log",
                opaque,
                true,
                FaceTextures {
                    top: log_top,
                    bottom: log_top,
                    side: log_side,
                },
            ),
            block("leaves", RenderShape::Cutout, true, all(leaves)),
            block("water", RenderShape::Translucent, false, all(water)),
            block("planks", opaque, true, all(planks)),
        ] {
            b.add_block(def).expect("built-in block");
        }
        // The first emitters (ADR 0015): full-cube colored lamps and a lava placeholder (an
        // opaque, solid, glowing cube until fluids exist). Torches wait for non-cube shapes.
        let emitters = LAMPS
            .iter()
            .zip(lamp_textures)
            .map(|(lamp, texture)| (lamp.short_name(), lamp.emission, texture))
            .chain([("lava", LAVA_EMISSION, lava)]);
        for (name, emission, texture) in emitters {
            let def = BlockDef {
                emission,
                ..block(name, opaque, true, all(texture))
            };
            b.add_block(def).expect("built-in emitter");
        }
        // Non-cube blocks and the turned logs (ADR 0018). Glass is a clear cube: light and sky
        // pass (opacity 0). Slabs let light through at the cost of leaves (a slab roof is not
        // dark; opacity 1 keeps the light flood on its fast path). Plants stop nothing.
        let log_textures = FaceTextures {
            top: log_top,
            bottom: log_top,
            side: log_side,
        };
        let model = |name: &str, shape, model: BlockModel, opacity, textures| BlockDef {
            name: format!("boxel:{name}"),
            shape,
            model,
            collision: model.default_collision(),
            emission: [0; 3],
            light_opacity: opacity,
            textures,
        };
        let cube_on = |axis| BlockModel::Cube { axis };
        for def in [
            model(
                "glass",
                RenderShape::Translucent,
                BlockModel::CUBE,
                0,
                all(glass),
            ),
            model(
                "stone_slab",
                opaque,
                BlockModel::Slab(SlabHalf::Bottom),
                1,
                all(stone),
            ),
            model(
                "stone_slab_top",
                opaque,
                BlockModel::Slab(SlabHalf::Top),
                1,
                all(stone),
            ),
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
            model(
                "log_x",
                opaque,
                cube_on(Axis::X),
                OPAQUE_OPACITY,
                log_textures,
            ),
            model(
                "log_z",
                opaque,
                cube_on(Axis::Z),
                OPAQUE_OPACITY,
                log_textures,
            ),
        ] {
            b.add_block(def).expect("built-in block");
        }
        b
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

    /// Registers a block; returns its state.
    pub fn add_block(&mut self, def: BlockDef) -> Result<BlockState, RegistryError> {
        validate_name(&def.name)?;
        if self.blocks.iter().any(|b| b.name == def.name) {
            return Err(RegistryError::Duplicate(def.name));
        }
        if self.blocks.len() >= u16::MAX as usize {
            return Err(RegistryError::Full("block states"));
        }
        let channels_ok = def.emission.iter().all(|&c| c <= crate::MAX_LIGHT);
        let opaque_cube = def.shape == RenderShape::Opaque && def.model.is_cube();
        let opacity_ok = def.light_opacity <= OPAQUE_OPACITY
            && (!opaque_cube || def.light_opacity == OPAQUE_OPACITY);
        if !channels_ok || !opacity_ok {
            return Err(RegistryError::InvalidLight(def.name));
        }
        if let BlockCollision::Boxes(boxes) = &def.collision
            && !boxes.iter().all(|b| b.is_valid())
        {
            return Err(RegistryError::InvalidCollision(def.name));
        }
        if def.shape != RenderShape::None {
            for t in [def.textures.top, def.textures.bottom, def.textures.side] {
                if t.0 as usize >= self.textures.len() {
                    return Err(RegistryError::UnknownTexture(t.0));
                }
            }
        }
        self.blocks.push(def);
        Ok(BlockState((self.blocks.len() - 1) as u16))
    }

    /// Freezes the registry (builds the `SoA` lookup tables).
    pub fn build(self) -> BlockRegistry {
        let mut by_name =
            std::collections::HashMap::with_hasher(foldhash::fast::FixedState::default());
        for (i, b) in self.blocks.iter().enumerate() {
            by_name.insert(b.name.clone(), BlockState(i as u16));
        }
        BlockRegistry {
            names: self.blocks.iter().map(|b| b.name.clone()).collect(),
            shapes: self.blocks.iter().map(|b| b.shape).collect(),
            models: self.blocks.iter().map(|b| b.model).collect(),
            collisions: self.blocks.iter().map(|b| b.collision.clone()).collect(),
            flags: self
                .blocks
                .iter()
                .map(|b| {
                    let mut f = StateFlags::empty();
                    let solid = !b.collision.is_none();
                    f.set(StateFlags::SOLID, solid);
                    f.set(
                        StateFlags::OCCLUDES,
                        b.shape == RenderShape::Opaque && b.model.is_cube(),
                    );
                    f.set(
                        StateFlags::FLUID,
                        b.shape == RenderShape::Translucent && !solid,
                    );
                    f.set(StateFlags::EMISSIVE, b.emission != [0; 3]);
                    f
                })
                .collect(),
            emission: self
                .blocks
                .iter()
                .map(|b| LightValue::from_rgb(b.emission))
                .collect(),
            opacity: self.blocks.iter().map(|b| b.light_opacity).collect(),
            faces: self.blocks.iter().map(|b| b.textures).collect(),
            textures: self.textures,
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
    names: Vec<String>,
    shapes: Vec<RenderShape>,
    models: Vec<BlockModel>,
    collisions: Vec<BlockCollision>,
    flags: Vec<StateFlags>,
    emission: Vec<LightValue>,
    opacity: Vec<u8>,
    faces: Vec<FaceTextures>,
    textures: Vec<TextureSpec>,
    by_name: std::collections::HashMap<String, BlockState, foldhash::fast::FixedState>,
}

impl BlockRegistry {
    /// Built-in content ([`BlockRegistryBuilder::with_default_blocks`], built), in this exact
    /// order (state ids are frozen for tests and worldgen): 0 `boxel:air`, 1 `boxel:stone`,
    /// 2 `boxel:dirt`, 3 `boxel:grass`, 4 `boxel:sand`, 5 `boxel:gravel`, 6 `boxel:snow`,
    /// 7 `boxel:bedrock`, 8 `boxel:log`, 9 `boxel:leaves` (Cutout), 10 `boxel:water`
    /// (Translucent), 11 `boxel:planks`, then the emitters (opaque, solid, light opacity 15;
    /// emission red, green, blue): 12 `boxel:lamp_orange` (14, 11, 6), 13 `boxel:lamp_red`
    /// (14, 2, 2), 14 `boxel:lamp_blue` (3, 6, 14), 15 `boxel:lamp_white` (15, 15, 15) and
    /// 16 `boxel:lava` (15, 7, 2), a placeholder until fluids exist ([`LAMPS`],
    /// [`LAVA_EMISSION`]); then the blocks of ADR 0018: 17 `boxel:glass` (Translucent cube,
    /// solid, light opacity 0), 18 `boxel:stone_slab` and 19 `boxel:stone_slab_top` (Opaque
    /// slabs, light opacity 1), 20 `boxel:short_grass` and 21 `boxel:poppy` (Cutout crosses, no
    /// collision, light opacity 0), 22 `boxel:log_x` and 23 `boxel:log_z` (the log turned to
    /// the X and Z axes).
    pub fn default_blocks() -> BlockRegistry {
        BlockRegistryBuilder::with_default_blocks().build()
    }
    /// Number of states.
    pub fn len(&self) -> usize {
        self.names.len()
    }
    /// True if only air.
    pub fn is_empty(&self) -> bool {
        self.names.len() <= 1
    }
    /// State by name.
    pub fn by_name(&self, name: &str) -> Option<BlockState> {
        self.by_name.get(name).copied()
    }
    /// Name of a state (`"boxel:unknown"` if out of range).
    pub fn name(&self, s: BlockState) -> &str {
        self.names
            .get(s.0 as usize)
            .map_or("boxel:unknown", String::as_str)
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
    /// Mesher voxel value of a state: `0` for `RenderShape::None`, else `s.0` (M0 identity map).
    pub fn material(&self, s: BlockState) -> u16 {
        if self.shape(s) == RenderShape::None {
            0
        } else {
            s.0
        }
    }
}
